//! The window, drawn by us: a Slint platform of our own that renders each part of the interface into a texture
//! of its own (the page, the sidebar's rows, the player's controls, each a Slint window with Skia drawing it
//! offscreen on our GPU device) and puts them together through our shaders (glass.wgsl). Where the sidebar
//! and the player are, the page is seen through glass: blurred, tinted, bent at the rim so the rim carries
//! the colours beside it. The sharp rows and controls go on top. Metal on macOS, Vulkan (or GL) elsewhere.
//!
//! The event loop is winit's; the pointer goes to the layer under it (or the one it was pressed in), the keys
//! to the page. Nothing is drawn unless a layer asked to be (or an animation runs).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::platform::skia_renderer::SkiaWGPU30Renderer;
use slint::platform::{Key, PointerEventButton, WindowAdapter, WindowEvent};
use slint::wgpu_30::wgpu;
use slint::{LogicalPosition, LogicalSize, PhysicalSize, PlatformError, SharedString};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WKey, NamedKey};
use winit::window::{Window as WinitWindow, WindowAttributes, WindowId};

/// The sidebar's width, and the player's size and place (logical pixels), as app.slint lays the page out.
pub const SIDEBAR_W: f32 = 240.0;
const PLAYER_H: f32 = 54.0;
const PLAYER_MAX_W: f32 = 720.0;
const PLAYER_BOTTOM: f32 = 12.0;

/// The one format every layer and the window are drawn in: Skia renders into it on Metal and Vulkan alike.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// What wakes the event loop from another thread.
enum Wake {
    Call(Box<dyn FnOnce() + Send>),
    Quit,
}

struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

/// One part of the interface: a Slint window drawn into a texture of its own.
struct Layer {
    window: slint::Window,
    renderer: SkiaWGPU30Renderer,
    size: Cell<PhysicalSize>,
    dirty: Cell<bool>,
    texture: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
}

impl WindowAdapter for Layer {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn size(&self) -> PhysicalSize {
        self.size.get()
    }

    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }

    fn request_redraw(&self) {
        self.dirty.set(true);
        SHARED.with(|s| {
            if let Some(w) = s.borrow().as_ref().and_then(|s| s.window.borrow().clone()) {
                w.request_redraw();
            }
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Page,
    Sidebar,
    Player,
}

/// The layers, in the order Slint made their windows, each with the part it plays once known.
type Layers = RefCell<Vec<(Rc<Layer>, Cell<Option<Role>>)>>;

/// One blur pass: from, into (which of the two), the step's direction, and the source's size.
type BlurPass<'a> = (&'a wgpu::TextureView, usize, [f32; 2], (f32, f32));

/// Everything the platform and the event loop share, on the main thread.
struct Shared {
    gpu: Rc<Gpu>,
    layers: Layers,
    window: RefCell<Option<Arc<WinitWindow>>>,
    proxy: EventLoopProxy<Wake>,
    /// Whether the sidebar and the player are shown over the page (not over Now Playing, nor the sign-in page).
    sidebar_shown: Cell<bool>,
    player_shown: Cell<bool>,
}

thread_local! {
    static SHARED: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
    static EVENT_LOOP: RefCell<Option<EventLoop<Wake>>> = const { RefCell::new(None) };
}

fn shared() -> Option<Rc<Shared>> {
    SHARED.with(|s| s.borrow().clone())
}

struct Platform {
    shared: Rc<Shared>,
}

impl slint::platform::Platform for Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let g = &self.shared.gpu;
        let renderer = SkiaWGPU30Renderer::new(g.instance.clone(), g.adapter.clone(), g.device.clone(), g.queue.clone())?;
        let layer = Rc::new_cyclic(|me: &Weak<Layer>| Layer {
            window: slint::Window::new(me.clone() as Weak<dyn WindowAdapter>),
            renderer,
            size: Cell::new(PhysicalSize::new(1, 1)),
            dirty: Cell::new(true),
            texture: RefCell::new(None),
        });
        self.shared.layers.borrow_mut().push((layer.clone(), Cell::new(None)));
        Ok(layer)
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let event_loop = EVENT_LOOP.with(|e| e.borrow_mut().take()).ok_or_else(|| PlatformError::from("the event loop ran already"))?;
        let mut runner = Runner { shared: self.shared.clone(), draw: None, pointer: LogicalPosition::new(0.0, 0.0), over: None, held: None, modifiers: Default::default() };
        event_loop.run_app(&mut runner).map_err(|e| PlatformError::from(e.to_string()))
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn slint::platform::EventLoopProxy>> {
        Some(Box::new(Proxy(self.shared.proxy.clone())))
    }

    fn set_clipboard_text(&self, text: &str, clipboard: slint::platform::Clipboard) {
        if clipboard == slint::platform::Clipboard::DefaultClipboard {
            if let Ok(mut c) = arboard::Clipboard::new() {
                let _ = c.set_text(text.to_string());
            }
        }
    }

    fn clipboard_text(&self, clipboard: slint::platform::Clipboard) -> Option<String> {
        if clipboard != slint::platform::Clipboard::DefaultClipboard {
            return None;
        }
        arboard::Clipboard::new().ok()?.get_text().ok()
    }
}

struct Proxy(EventLoopProxy<Wake>);

impl slint::platform::EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.0.send_event(Wake::Quit).map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }

    fn invoke_from_event_loop(&self, event: Box<dyn FnOnce() + Send>) -> Result<(), slint::EventLoopError> {
        self.0.send_event(Wake::Call(event)).map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }
}

/// Makes this the platform Slint draws with: before any window is created.
pub fn install() -> Result<(), String> {
    let event_loop = EventLoop::<Wake>::with_user_event().build().map_err(|e| format!("the event loop: {e}"))?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::LowPower, ..Default::default() }))
        .map_err(|e| format!("no GPU: {e}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).map_err(|e| format!("the GPU: {e}"))?;
    let shared = Rc::new(Shared {
        gpu: Rc::new(Gpu { instance, adapter, device, queue }),
        layers: RefCell::new(Vec::new()),
        window: RefCell::new(None),
        proxy: event_loop.create_proxy(),
        sidebar_shown: Cell::new(false),
        player_shown: Cell::new(false),
    });
    SHARED.with(|s| *s.borrow_mut() = Some(shared.clone()));
    EVENT_LOOP.with(|e| *e.borrow_mut() = Some(event_loop));
    slint::platform::set_platform(Box::new(Platform { shared })).map_err(|e| format!("the platform: {e}"))
}

/// Which Slint window is which part: the page (the main window), and the sidebar's and the player's.
pub fn roles(page: &slint::Window, sidebar: Option<&slint::Window>, player: Option<&slint::Window>) {
    let Some(s) = shared() else { return };
    for (layer, role) in s.layers.borrow().iter() {
        let w = &layer.window as *const slint::Window;
        if std::ptr::eq(w, page) {
            role.set(Some(Role::Page));
        } else if sidebar.is_some_and(|x| std::ptr::eq(w, x)) {
            role.set(Some(Role::Sidebar));
        } else if player.is_some_and(|x| std::ptr::eq(w, x)) {
            role.set(Some(Role::Player));
        }
    }
    s.sidebar_shown.set(sidebar.is_some());
    s.player_shown.set(player.is_some());
    relayout(&s);
}

/// Whether the sidebar and the player show over the page.
pub fn show_glass(sidebar: bool, player: bool) {
    let Some(s) = shared() else { return };
    let has = |r| s.layers.borrow().iter().any(|(_, x)| x.get() == Some(r));
    let (sidebar, player) = (sidebar && has(Role::Sidebar), player && has(Role::Player));
    if s.sidebar_shown.get() != sidebar || s.player_shown.get() != player {
        s.sidebar_shown.set(sidebar);
        s.player_shown.set(player);
        redraw(&s);
    }
}

/// The window follows the pointer, as a titlebar does (the content runs up under it).
pub fn drag_window() {
    if let Some(w) = shared().and_then(|s| s.window.borrow().clone()) {
        let _ = w.drag_window();
    }
}

/// A double click on the titlebar: the window zooms, or comes back.
pub fn zoom_window() {
    if let Some(w) = shared().and_then(|s| s.window.borrow().clone()) {
        w.set_maximized(!w.is_maximized());
    }
}

fn redraw(s: &Shared) {
    if let Some(w) = s.window.borrow().as_ref() {
        w.request_redraw();
    }
}

/// A layer's place in the window, logical pixels.
fn place(s: &Shared, role: Role, window: LogicalSize) -> (LogicalPosition, LogicalSize) {
    match role {
        Role::Page => (LogicalPosition::new(0.0, 0.0), window),
        Role::Sidebar => (LogicalPosition::new(0.0, 0.0), LogicalSize::new(SIDEBAR_W, window.height)),
        Role::Player => {
            let page = (window.width - SIDEBAR_W).max(0.0);
            let w = (page - 32.0).clamp(200.0, PLAYER_MAX_W);
            let _ = s;
            (LogicalPosition::new(SIDEBAR_W + (page - w) / 2.0, window.height - PLAYER_H - PLAYER_BOTTOM), LogicalSize::new(w, PLAYER_H))
        }
    }
}

/// Every layer sized for the window as it is now.
fn relayout(s: &Shared) {
    let Some(win) = s.window.borrow().clone() else { return };
    let scale = win.scale_factor() as f32;
    let phys = win.inner_size();
    let logical = LogicalSize::new(phys.width as f32 / scale, phys.height as f32 / scale);
    for (layer, role) in s.layers.borrow().iter() {
        let Some(role) = role.get() else { continue };
        let (_, size) = place(s, role, logical);
        let px = PhysicalSize::new((size.width * scale).round().max(1.0) as u32, (size.height * scale).round().max(1.0) as u32);
        if layer.window.scale_factor() != scale {
            layer.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        }
        if layer.size.get() != px {
            layer.size.set(px);
            layer.window.dispatch_event(WindowEvent::Resized { size });
            layer.dirty.set(true);
        }
    }
    redraw(s);
}

struct Runner {
    shared: Rc<Shared>,
    draw: Option<Draw>,
    pointer: LogicalPosition,
    /// The layer the pointer is over, and the one a button was pressed in (it keeps the pointer until the
    /// button is let go).
    over: Option<Role>,
    held: Option<Role>,
    modifiers: winit::keyboard::ModifiersState,
}

impl Runner {
    fn layer(&self, role: Role) -> Option<Rc<Layer>> {
        self.shared.layers.borrow().iter().find(|(_, r)| r.get() == Some(role)).map(|(l, _)| l.clone())
    }

    fn logical_window(&self) -> Option<LogicalSize> {
        let w = self.shared.window.borrow().clone()?;
        let scale = w.scale_factor() as f32;
        let p = w.inner_size();
        Some(LogicalSize::new(p.width as f32 / scale, p.height as f32 / scale))
    }

    /// The layer under `at`: the player, the sidebar or the page.
    fn under(&self, at: LogicalPosition) -> Role {
        let Some(win) = self.logical_window() else { return Role::Page };
        for role in [Role::Player, Role::Sidebar] {
            let shown = match role {
                Role::Player => self.shared.player_shown.get(),
                _ => self.shared.sidebar_shown.get(),
            };
            if !shown || self.layer(role).is_none() {
                continue;
            }
            let (o, sz) = place(&self.shared, role, win);
            if at.x >= o.x && at.y >= o.y && at.x < o.x + sz.width && at.y < o.y + sz.height {
                return role;
            }
        }
        Role::Page
    }

    fn local(&self, role: Role, at: LogicalPosition) -> LogicalPosition {
        let Some(win) = self.logical_window() else { return at };
        let (o, _) = place(&self.shared, role, win);
        LogicalPosition::new(at.x - o.x, at.y - o.y)
    }

    fn pointer(&mut self, event: impl Fn(LogicalPosition) -> WindowEvent) {
        let to = self.held.unwrap_or_else(|| self.under(self.pointer));
        if self.over != Some(to) {
            if let Some(old) = self.over.and_then(|r| self.layer(r)) {
                old.window.dispatch_event(WindowEvent::PointerExited);
            }
            self.over = Some(to);
        }
        if let Some(l) = self.layer(to) {
            l.window.dispatch_event(event(self.local(to, self.pointer)));
        }
    }

    fn key(&self, event: &winit::event::KeyEvent) {
        let Some(page) = self.layer(Role::Page) else { return };
        let Some(text) = key_text(event) else { return };
        let e = match (event.state, event.repeat) {
            (ElementState::Pressed, false) => WindowEvent::KeyPressed { text },
            (ElementState::Pressed, true) => WindowEvent::KeyPressRepeated { text },
            (ElementState::Released, _) => WindowEvent::KeyReleased { text },
        };
        page.window.dispatch_event(e);
    }

    fn animating(&self) -> bool {
        self.shared.layers.borrow().iter().any(|(l, r)| r.get().is_some() && (l.window.has_active_animations() || l.dirty.get()))
    }
}

/// What Slint calls a key, from what winit says was pressed. On macOS, Command is Slint's Control (its
/// shortcuts are written with Control), and Control its Meta, as Slint's own macOS backend has them.
fn key_text(event: &winit::event::KeyEvent) -> Option<SharedString> {
    let mac = cfg!(target_os = "macos");
    let named = |k: Key| Some(SharedString::from(k));
    match &event.logical_key {
        WKey::Named(n) => match n {
            NamedKey::Enter => named(Key::Return),
            NamedKey::Tab => named(Key::Tab),
            NamedKey::Backspace => named(Key::Backspace),
            NamedKey::Delete => named(Key::Delete),
            NamedKey::Escape => named(Key::Escape),
            NamedKey::ArrowLeft => named(Key::LeftArrow),
            NamedKey::ArrowRight => named(Key::RightArrow),
            NamedKey::ArrowUp => named(Key::UpArrow),
            NamedKey::ArrowDown => named(Key::DownArrow),
            NamedKey::Home => named(Key::Home),
            NamedKey::End => named(Key::End),
            NamedKey::PageUp => named(Key::PageUp),
            NamedKey::PageDown => named(Key::PageDown),
            NamedKey::Space => Some(" ".into()),
            NamedKey::Shift => named(Key::Shift),
            NamedKey::Alt => named(Key::Alt),
            NamedKey::Control => named(if mac { Key::Meta } else { Key::Control }),
            NamedKey::Super | NamedKey::Meta => named(if mac { Key::Control } else { Key::Meta }),
            _ => None,
        },
        WKey::Character(c) => Some(event.text.as_ref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control)).map_or_else(|| c.as_str().into(), |t| t.as_str().into())),
        _ => None,
    }
}

impl ApplicationHandler<Wake> for Runner {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.shared.window.borrow().is_some() {
            return;
        }
        let mut attrs = WindowAttributes::default().with_title("nori").with_inner_size(winit::dpi::LogicalSize::new(1280.0, 820.0)).with_min_inner_size(winit::dpi::LogicalSize::new(860.0, 560.0));
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true);
        }
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("nori: no window: {e}");
                el.exit();
                return;
            }
        };
        match Draw::new(&self.shared.gpu, window.clone()) {
            Ok(d) => self.draw = Some(d),
            Err(e) => {
                eprintln!("nori: the window cannot be drawn: {e}");
                el.exit();
                return;
            }
        }
        *self.shared.window.borrow_mut() = Some(window);
        relayout(&self.shared);
        crate::menu::install();
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: Wake) {
        match event {
            Wake::Call(f) => f(),
            Wake::Quit => el.exit(),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: winit::event::WindowEvent) {
        use winit::event::WindowEvent as E;
        match event {
            E::CloseRequested => el.exit(),
            E::Resized(size) => {
                if let Some(d) = &mut self.draw {
                    d.resize(size.width, size.height);
                }
                relayout(&self.shared);
            }
            E::ScaleFactorChanged { .. } => relayout(&self.shared),
            E::Focused(on) => {
                if let Some(l) = self.layer(Role::Page) {
                    l.window.dispatch_event(WindowEvent::WindowActiveChanged(on));
                }
            }
            E::CursorMoved { position, .. } => {
                let scale = self.shared.window.borrow().as_ref().map_or(1.0, |w| w.scale_factor());
                let p = position.to_logical::<f32>(scale);
                self.pointer = LogicalPosition::new(p.x, p.y);
                self.pointer(|position| WindowEvent::PointerMoved { position });
            }
            E::CursorLeft { .. } => {
                if let Some(old) = self.over.take().and_then(|r| self.layer(r)) {
                    old.window.dispatch_event(WindowEvent::PointerExited);
                }
            }
            E::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => PointerEventButton::Left,
                    MouseButton::Right => PointerEventButton::Right,
                    MouseButton::Middle => PointerEventButton::Middle,
                    _ => PointerEventButton::Other,
                };
                match state {
                    ElementState::Pressed => {
                        self.held = Some(self.under(self.pointer));
                        self.pointer(|position| WindowEvent::PointerPressed { position, button });
                    }
                    ElementState::Released => {
                        self.pointer(|position| WindowEvent::PointerReleased { position, button });
                        self.held = None;
                    }
                }
            }
            E::MouseWheel { delta, .. } => {
                let scale = self.shared.window.borrow().as_ref().map_or(1.0, |w| w.scale_factor()) as f32;
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 60.0, y * 60.0),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32 / scale, p.y as f32 / scale),
                };
                self.pointer(|position| WindowEvent::PointerScrolled { position, delta_x: dx, delta_y: dy });
            }
            E::ModifiersChanged(m) => self.modifiers = m.state(),
            E::KeyboardInput { event, .. } => self.key(&event),
            E::Ime(winit::event::Ime::Commit(text)) => {
                if let Some(page) = self.layer(Role::Page) {
                    page.window.dispatch_event(WindowEvent::KeyPressed { text: text.as_str().into() });
                    page.window.dispatch_event(WindowEvent::KeyReleased { text: text.as_str().into() });
                }
            }
            E::RedrawRequested => {
                slint::platform::update_timers_and_animations();
                let win = self.logical_window();
                if let (Some(d), Some(win)) = (&mut self.draw, win) {
                    d.frame(&self.shared, win);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        slint::platform::update_timers_and_animations();
        crate::menu::poll();
        if self.animating() {
            redraw(&self.shared);
        }
        el.set_control_flow(match slint::platform::duration_until_next_timer_update() {
            Some(d) => ControlFlow::WaitUntil(Instant::now() + d.min(Duration::from_secs(60))),
            None => ControlFlow::Wait,
        });
    }
}

/// The GPU side of the window: the surface, the pipelines, and the textures the glass looks through.
struct Draw {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    copy: wgpu::RenderPipeline,
    over: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    glass: wgpu::RenderPipeline,
    bind: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The page a quarter size, blurred: two textures the blur passes go back and forth between.
    blurred: Option<[(wgpu::Texture, wgpu::TextureView); 2]>,
    /// The page a sixteenth size, blurred much further: the soft light the glass takes from beside it.
    glow: Option<[(wgpu::Texture, wgpu::TextureView); 2]>,
    /// The window changed size: the surface is set up again before the next frame.
    stale: bool,
}

impl Draw {
    fn new(gpu: &Gpu, window: Arc<WinitWindow>) -> Result<Draw, String> {
        let d = &gpu.device;
        let size = window.inner_size();
        let surface = gpu.instance.create_surface(window).map_err(|e| e.to_string())?;
        let mut config = surface.get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1)).ok_or("the surface has no configuration")?;
        config.format = FORMAT;
        config.alpha_mode = wgpu::CompositeAlphaMode::Auto;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(d, &config);
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("glass"), source: wgpu::ShaderSource::Wgsl(include_str!("glass.wgsl").into()) });
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let bind = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex(1),
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                tex(3),
                tex(4),
            ],
        });
        let layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("glass"), bind_group_layouts: &[Some(&bind)], immediate_size: 0 });
        let premultiplied = wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
        };
        let pipeline = |entry: &str, blend: Option<wgpu::BlendState>| {
            d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: FORMAT, blend, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glass"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Draw {
            surface,
            config,
            copy: pipeline("fs_copy", None),
            over: pipeline("fs_copy", Some(premultiplied)),
            blur: pipeline("fs_blur", None),
            glass: pipeline("fs_glass", Some(premultiplied)),
            bind,
            sampler,
            blurred: None,
            glow: None,
            stale: false,
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.blurred = None;
        self.glow = None;
        self.stale = true;
    }

    /// Draws the layers that asked to be, then the window: the page, the glass where the sidebar and the
    /// player are, and them on top.
    fn frame(&mut self, s: &Shared, win: LogicalSize) {
        let gpu = s.gpu.clone();
        let d = &gpu.device;
        let scale = s.window.borrow().as_ref().map_or(1.0, |w| w.scale_factor() as f32);
        // Each layer drawn into its texture, only when it changed.
        for (layer, role) in s.layers.borrow().iter() {
            if role.get().is_none() {
                continue;
            }
            let size = layer.size.get();
            let fresh = layer.texture.borrow().as_ref().is_none_or(|(t, _)| t.width() != size.width || t.height() != size.height);
            if fresh {
                let t = texture(d, size.width, size.height, "layer");
                let v = t.create_view(&Default::default());
                *layer.texture.borrow_mut() = Some((t, v));
            }
            if fresh || layer.dirty.get() {
                // A new texture is drawn again on the next frame too: the first drawing into it can come out
                // empty (the layer's first frame at a new size), and nothing else would ask for another.
                layer.dirty.set(fresh);
                if let Some((t, _)) = layer.texture.borrow().as_ref() {
                    if let Err(e) = layer.renderer.render_to_texture(t) {
                        eprintln!("nori: a layer was not drawn: {e}");
                    }
                }
            }
        }
        if std::mem::take(&mut self.stale) {
            self.surface.configure(d, &self.config);
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.surface.configure(d, &self.config);
                return;
            }
        };
        let target = frame.texture.create_view(&Default::default());
        let layers = s.layers.borrow();
        let find = |r: Role| layers.iter().find(|(_, x)| x.get() == Some(r)).and_then(|(l, _)| l.texture.borrow().as_ref().map(|(_, v)| v.clone()));
        let Some(page) = find(Role::Page) else { return };
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        // The page, a quarter size and blurred, for the glass to look through.
        let (bw, bh) = ((self.config.width / 4).max(1), (self.config.height / 4).max(1));
        if self.blurred.as_ref().is_none_or(|b| b[0].0.width() != bw || b[0].0.height() != bh) {
            let mk = || {
                let t = texture(d, bw, bh, "blur");
                let v = t.create_view(&Default::default());
                (t, v)
            };
            self.blurred = Some([mk(), mk()]);
        }
        let (gw, gh) = ((self.config.width / 16).max(1), (self.config.height / 16).max(1));
        if self.glow.as_ref().is_none_or(|b| b[0].0.width() != gw || b[0].0.height() != gh) {
            let mk = || {
                let t = texture(d, gw, gh, "glow");
                let v = t.create_view(&Default::default());
                (t, v)
            };
            self.glow = Some([mk(), mk()]);
        }
        let mut enc = d.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let glass_needed = s.sidebar_shown.get() || s.player_shown.get();
        if glass_needed {
            let b = self.blurred.as_ref().expect("made above");
            let (bwf, bhf) = (bw as f32, bh as f32);
            let passes: [BlurPass; 4] =
                [(&page, 0, [1.5, 0.0], (w, h)), (&b[0].1, 1, [0.0, 1.5], (bwf, bhf)), (&b[1].1, 0, [2.5, 0.0], (bwf, bhf)), (&b[0].1, 1, [0.0, 2.5], (bwf, bhf))];
            for (src, dst, dir, src_size) in passes {
                let u = uniforms([0.0, 0.0, bwf, bhf], [bwf, bhf, src_size.0, src_size.1], [0.0; 4], [0.0; 4], [0.0, 0.0, dir[0], dir[1]]);
                let group = self.group(d, &gpu.queue, &u, src, src, src);
                pass(&mut enc, &b[dst].1, &self.blur, &group, false);
            }
            // And from it, the glow: a sixteenth of the page, blurred until no edge is left in it.
            let gl = self.glow.as_ref().expect("made above");
            let (gwf, ghf) = (gw as f32, gh as f32);
            let passes: [BlurPass; 4] =
                [(&b[1].1, 0, [2.0, 0.0], (bwf, bhf)), (&gl[0].1, 1, [0.0, 2.0], (gwf, ghf)), (&gl[1].1, 0, [3.0, 0.0], (gwf, ghf)), (&gl[0].1, 1, [0.0, 3.0], (gwf, ghf))];
            for (src, dst, dir, src_size) in passes {
                let u = uniforms([0.0, 0.0, gwf, ghf], [gwf, ghf, src_size.0, src_size.1], [0.0; 4], [0.0; 4], [0.0, 0.0, dir[0], dir[1]]);
                let group = self.group(d, &gpu.queue, &u, src, src, src);
                pass(&mut enc, &gl[dst].1, &self.blur, &group, false);
            }
        }
        let blurred = self.blurred.as_ref().map(|b| b[1].1.clone()).unwrap_or_else(|| page.clone());
        let glow = self.glow.as_ref().map(|b| b[1].1.clone()).unwrap_or_else(|| page.clone());
        // The window: the page, then glass and what sits on it.
        let full = uniforms([0.0, 0.0, w, h], [w, h, w, h], [0.0; 4], [0.0; 4], [0.0; 4]);
        let g = self.group(d, &gpu.queue, &full, &page, &page, &page);
        pass(&mut enc, &target, &self.copy, &g, true);
        let mut glass = |role: Role, shape: [f32; 4], tint: [f32; 4], light: [f32; 4], gather: [f32; 4], rect: [f32; 4]| {
            let Some(view) = find(role) else { return };
            let u = uniforms_gather(rect, [w, h, w, h], shape, tint, light, gather);
            let g = self.group(d, &gpu.queue, &u, &page, &blurred, &glow);
            pass(&mut enc, &target, &self.glass, &g, false);
            let (o, sz) = place(s, role, win);
            let r = [o.x * scale, o.y * scale, sz.width * scale, sz.height * scale];
            let u = uniforms(r, [w, h, w, h], [0.0; 4], [0.0; 4], [0.0; 4]);
            let g = self.group(d, &gpu.queue, &u, &view, &view, &view);
            pass(&mut enc, &target, &self.over, &g, false);
        };
        if s.sidebar_shown.get() {
            // The pane runs past the window's other edges, so only its right edge (by the page) is a rim.
            // Far past them: the right edge is the nearest everywhere in the pane, so all of it looks to the page.
            let r = [-8000.0 * scale, -8000.0 * scale, (SIDEBAR_W + 8000.0) * scale, h + 16000.0 * scale];
            // Light from as far as 180 points beside it, faint, and gone within a few dozen points of the edge.
            glass(Role::Sidebar, [0.0, 28.0 * scale, 22.0 * scale, 0.12], [0.1, 0.095, 0.09, 0.18], [0.22, 0.04, 0.34, 45.0 * scale], [180.0 * scale, 1.0, 0.0, 0.0], r);
        }
        if s.player_shown.get() {
            let (o, sz) = place(s, Role::Player, win);
            let r = [o.x * scale, o.y * scale, sz.width * scale, sz.height * scale];
            // Nearly invisible, as Music's is: the page blurred behind a neutral grey veil, no colour of its own
            // and none washed in from beside it, a gentle bend and a faint rim.
            glass(Role::Player, [PLAYER_H * 0.5 * scale, 14.0 * scale, 6.0 * scale, 0.05], [0.14, 0.14, 0.145, 0.38], [0.2, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 0.0], r);
        }
        drop(layers);
        gpu.queue.submit([enc.finish()]);
        gpu.queue.present(frame);
    }

    fn group(&self, d: &wgpu::Device, q: &wgpu::Queue, u: &[u8], a: &wgpu::TextureView, b: &wgpu::TextureView, c: &wgpu::TextureView) -> wgpu::BindGroup {
        let buf = d.create_buffer(&wgpu::BufferDescriptor { label: Some("glass"), size: u.len() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        q.write_buffer(&buf, 0, u);
        d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass"),
            layout: &self.bind,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(a) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(b) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(c) },
            ],
        })
    }
}

fn texture(d: &wgpu::Device, w: u32, h: u32, label: &str) -> wgpu::Texture {
    d.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn pass(enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, pipeline: &wgpu::RenderPipeline, group: &wgpu::BindGroup, clear: bool) {
    let mut p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: if clear { wgpu::LoadOp::Clear(wgpu::Color::BLACK) } else { wgpu::LoadOp::Load }, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    p.set_pipeline(pipeline);
    p.set_bind_group(0, group, &[]);
    p.draw(0..4, 0..1);
}

/// The shaders' uniforms, as glass.wgsl lays them out (six vec4s); `gather` only for the glass.
fn uniforms(rect: [f32; 4], view: [f32; 4], shape: [f32; 4], tint: [f32; 4], light: [f32; 4]) -> Vec<u8> {
    uniforms_gather(rect, view, shape, tint, light, [0.0; 4])
}

fn uniforms_gather(rect: [f32; 4], view: [f32; 4], shape: [f32; 4], tint: [f32; 4], light: [f32; 4], gather: [f32; 4]) -> Vec<u8> {
    [rect, view, shape, tint, light, gather].iter().flatten().flat_map(|f| f.to_ne_bytes()).collect()
}
