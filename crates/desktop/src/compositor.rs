//! A custom Slint platform on winit + wgpu. Each Slint window (page, sidebar, player) is a layer Skia
//! renders offscreen; glass.wgsl composites them, drawing the sidebar and player as blurred glass over
//! the page. Pointer events go to the layer under the pointer (or the one pressed in), keys to the page.
//! Frames are drawn only when a layer is dirty or animating.

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

use crate::menu::MenuBar;

/// Layer geometry in logical pixels; must match app.slint.
pub const SIDEBAR_W: f32 = 216.0;
const PLAYER_H: f32 = 54.0;
const PLAYER_MAX_W: f32 = 720.0;
const PLAYER_BOTTOM: f32 = 12.0;

/// Format of every layer and the surface; Skia renders it on Metal and Vulkan.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

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

/// A Slint window rendered into its own texture.
struct Layer {
    window: slint::Window,
    renderer: SkiaWGPU30Renderer,
    size: Cell<PhysicalSize>,
    dirty: Cell<bool>,
    texture: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
    /// Weak: `Shared` owns the layers.
    shared: Weak<Shared>,
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
        if let Some(w) = self.shared.upgrade().and_then(|s| s.window.borrow().clone()) {
            w.request_redraw();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Page,
    Sidebar,
    Player,
}

/// Layers in creation order, each with its role once assigned.
type Layers = RefCell<Vec<(Rc<Layer>, Cell<Option<Role>>)>>;

/// Halvings in the page's blur pyramid: its smallest level is 1/64 of the page.
const PYRAMID_LEVELS: u32 = 6;

/// Format of the blur pyramid: half floats, so a long smooth blur does not band.
const PYRAMID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The page blurred at every halving: the texture, a view of each mip to draw into, and one of them all
/// to read.
struct Pyramid {
    texture: wgpu::Texture,
    mips: Vec<wgpu::TextureView>,
    all: wgpu::TextureView,
}

/// State shared by the platform, the event loop and [`Compositor`] handles (main thread only).
struct Shared {
    gpu: Rc<Gpu>,
    layers: Layers,
    window: RefCell<Option<Arc<WinitWindow>>>,
    size: Cell<PhysicalSize>,
    proxy: EventLoopProxy<Wake>,
    sidebar_shown: Cell<bool>,
    player_shown: Cell<bool>,
    /// The window is not on screen (minimised, covered, locked): nothing is drawn until it shows again.
    occluded: Cell<bool>,
    /// Width of the right panel (0 when closed); the player centres in the remaining page.
    right: Cell<f32>,
    /// Queried each frame for the lyrics focus blur.
    focus: RefCell<Option<FocusSource>>,
    menu: MenuBar,
}

type FocusSource = Box<dyn Fn() -> Option<Focus>>;

/// A page region drawn blurred except for a sharp horizontal band (logical pixels).
#[derive(Clone, Copy)]
pub struct Focus {
    pub region: [f32; 4],
    pub band_top: f32,
    pub band_h: f32,
    /// A line and the gap after it: each step further from the band blurs a point more.
    pub pitch: f32,
}

impl Shared {
    fn shown(&self, role: Role) -> bool {
        match role {
            Role::Page => true,
            Role::Sidebar => self.sidebar_shown.get(),
            Role::Player => self.player_shown.get(),
        }
    }
}

/// The app's handle to the compositor, alongside the platform Slint owns.
#[derive(Clone)]
pub struct Compositor(Rc<Shared>);

struct Platform {
    shared: Rc<Shared>,
    event_loop: RefCell<Option<EventLoop<Wake>>>,
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
            shared: Rc::downgrade(&self.shared),
        });
        self.shared.layers.borrow_mut().push((layer.clone(), Cell::new(None)));
        Ok(layer)
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let event_loop = self.event_loop.borrow_mut().take().ok_or_else(|| PlatformError::from("the event loop ran already"))?;
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

/// Installs the platform. Must run before any Slint window is created.
pub fn install() -> Result<Compositor, String> {
    let event_loop = EventLoop::<Wake>::with_user_event().build().map_err(|e| format!("the event loop: {e}"))?;
    let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    // Every draw uses a fixed vertex range; indirect-command pipelines are unused.
    instance_descriptor.flags.remove(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL);
    let instance = wgpu::Instance::new(instance_descriptor);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::LowPower, ..Default::default() }))
        .map_err(|e| format!("no GPU: {e}"))?;
    let descriptor = wgpu::DeviceDescriptor { memory_hints: wgpu::MemoryHints::MemoryUsage, ..Default::default() };
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor)).map_err(|e| format!("the GPU: {e}"))?;
    let shared = Rc::new(Shared {
        gpu: Rc::new(Gpu { instance, adapter, device, queue }),
        layers: RefCell::new(Vec::new()),
        window: RefCell::new(None),
        size: Cell::new(PhysicalSize::new(1280, 820)),
        proxy: event_loop.create_proxy(),
        sidebar_shown: Cell::new(false),
        player_shown: Cell::new(false),
        occluded: Cell::new(false),
        right: Cell::new(0.0),
        focus: RefCell::new(None),
        menu: MenuBar::default(),
    });
    slint::platform::set_platform(Box::new(Platform { shared: shared.clone(), event_loop: RefCell::new(Some(event_loop)) })).map_err(|e| format!("the platform: {e}"))?;
    Ok(Compositor(shared))
}

impl Compositor {
    /// Vulkan's borrowed Skia images cannot own imported wgpu texture lifetimes.
    #[cfg(target_os = "linux")]
    pub fn picture(&self, pixels: &[u8], width: u32, height: u32) -> slint::Image {
        slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(pixels, width, height))
    }

    #[cfg(target_os = "linux")]
    pub fn mosaic(&self, images: &[slint::Image]) -> Option<slint::Image> {
        crate::app::mosaic(images)
    }

    /// Uploads decoded pixels once; Slint keeps the texture instead of CPU raster copies.
    #[cfg(not(target_os = "linux"))]
    pub fn picture(&self, pixels: &[u8], width: u32, height: u32) -> slint::Image {
        let texture = self.image_texture(width, height);
        self.0.gpu.queue.write_texture(
            texture.as_image_copy(), pixels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            texture.size(),
        );
        slint::Image::try_from(texture).expect("RGBA texture with render and sampling usages")
    }

    #[cfg(not(target_os = "linux"))]
    fn image_texture(&self, width: u32, height: u32) -> wgpu::Texture {
        self.0.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cover"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Card covers are decoded to equal squares; copy their pixels into one seamless image.
    #[cfg(not(target_os = "linux"))]
    pub fn mosaic(&self, images: &[slint::Image]) -> Option<slint::Image> {
        if images.len() != 4 { return None; }
        let textures: Vec<_> = images.iter().map(slint::Image::to_wgpu_30_texture).collect::<Option<_>>()?;
        let size = textures[0].size();
        if size.width != size.height || textures.iter().any(|t| t.size() != size) { return None; }
        let target = self.image_texture(size.width * 2, size.height * 2);
        let mut encoder = self.0.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pick covers") });
        for (i, texture) in textures.iter().enumerate() {
            let mut destination = target.as_image_copy();
            destination.origin = wgpu::Origin3d { x: (i as u32 % 2) * size.width, y: (i as u32 / 2) * size.height, z: 0 };
            encoder.copy_texture_to_texture(texture.as_image_copy(), destination, size);
        }
        self.0.gpu.queue.submit([encoder.finish()]);
        Some(slint::Image::try_from(target).expect("RGBA texture with render and sampling usages"))
    }

    /// Assigns each Slint window its layer role.
    pub fn roles(&self, page: &slint::Window, sidebar: Option<&slint::Window>, player: Option<&slint::Window>) {
        let s = &self.0;
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
        relayout(s);
    }

    pub fn set_focus_source(&self, f: impl Fn() -> Option<Focus> + 'static) {
        *self.0.focus.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_right_panel(&self, w: f32) {
        if self.0.right.get() != w {
            self.0.right.set(w);
            relayout(&self.0);
        }
    }

    /// Shows or hides the sidebar and player layers over the page.
    pub fn show_glass(&self, sidebar: bool, player: bool) {
        let s = &self.0;
        let has = |r| s.layers.borrow().iter().any(|(_, x)| x.get() == Some(r));
        let (sidebar, player) = (sidebar && has(Role::Sidebar), player && has(Role::Player));
        if s.sidebar_shown.get() != sidebar || s.player_shown.get() != player {
            s.sidebar_shown.set(sidebar);
            s.player_shown.set(player);
            redraw(s);
        }
    }

    /// Starts a window drag, as from a titlebar.
    pub fn drag_window(&self) {
        if let Some(w) = self.0.window.borrow().as_ref() {
            let _ = w.drag_window();
        }
    }

    /// Toggles maximized, as a titlebar double click does.
    pub fn zoom_window(&self) {
        if let Some(w) = self.0.window.borrow().as_ref() {
            w.set_maximized(!w.is_maximized());
        }
    }

    pub fn menu(&self) -> &MenuBar {
        &self.0.menu
    }
}

fn redraw(s: &Shared) {
    if s.occluded.get() {
        return;
    }
    if let Some(w) = s.window.borrow().as_ref() {
        w.request_redraw();
    }
}

/// A layer's rect in the window, logical pixels.
fn place(s: &Shared, role: Role, window: LogicalSize) -> (LogicalPosition, LogicalSize) {
    match role {
        Role::Page => (LogicalPosition::new(0.0, 0.0), window),
        Role::Sidebar => (LogicalPosition::new(0.0, 0.0), LogicalSize::new(SIDEBAR_W, window.height)),
        Role::Player => {
            let page = (window.width - SIDEBAR_W - s.right.get()).max(0.0);
            let w = (page - 32.0).clamp(200.0, PLAYER_MAX_W);
            (LogicalPosition::new(SIDEBAR_W + (page - w) / 2.0, window.height - PLAYER_H - PLAYER_BOTTOM), LogicalSize::new(w, PLAYER_H))
        }
    }
}

/// Resizes every layer to the current window size.
fn relayout(s: &Shared) {
    layout_layers(s);
    redraw(s);
}

fn layout_layers(s: &Shared) {
    let Some(win) = s.window.borrow().clone() else { return };
    let scale = win.scale_factor() as f32;
    let phys = s.size.get();
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
}

struct Runner {
    shared: Rc<Shared>,
    draw: Option<Draw>,
    pointer: LogicalPosition,
    /// Layer under the pointer, and the layer a button was pressed in (captures until release).
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
        let p = self.shared.size.get();
        Some(LogicalSize::new(p.width as f32 / scale, p.height as f32 / scale))
    }

    fn under(&self, at: LogicalPosition) -> Role {
        let Some(win) = self.logical_window() else { return Role::Page };
        for role in [Role::Player, Role::Sidebar] {
            if !self.shared.shown(role) || self.layer(role).is_none() {
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
        self.shared.layers.borrow().iter().any(|(l, r)| r.get().is_some_and(|r| self.shared.shown(r)) && (l.window.has_active_animations() || l.dirty.get()))
    }
}

fn wheel_event(position: LogicalPosition, dx: f32, dy: f32, phase: winit::event::TouchPhase) -> WindowEvent {
    use i_slint_core::input::{BackendMouseEvent, TouchPhase};
    let phase = match phase {
        winit::event::TouchPhase::Started => TouchPhase::Started,
        winit::event::TouchPhase::Moved => TouchPhase::Moved,
        winit::event::TouchPhase::Ended => TouchPhase::Ended,
        winit::event::TouchPhase::Cancelled => TouchPhase::Cancelled,
    };
    WindowEvent::internal(BackendMouseEvent::Wheel {
        position: i_slint_core::lengths::logical_point_from_api(position), delta_x: dx, delta_y: dy, phase,
    })
}

/// Maps a winit key to Slint's key text. On macOS, Command maps to Control and Control to Meta, as in
/// Slint's own macOS backend.
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
        let attrs = WindowAttributes::default().with_title("nori").with_inner_size(winit::dpi::LogicalSize::new(1280.0, 820.0)).with_min_inner_size(winit::dpi::LogicalSize::new(860.0, 560.0));
        #[cfg(target_os = "macos")]
        let attrs = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true)
        };
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
        #[cfg(target_os = "macos")]
        unified_toolbar(&window);
        let size = window.inner_size();
        self.shared.size.set(PhysicalSize::new(size.width, size.height));
        *self.shared.window.borrow_mut() = Some(window);
        relayout(&self.shared);
        self.shared.menu.install();
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
            E::Occluded(o) => {
                self.shared.occluded.set(o);
                redraw(&self.shared);
            }
            E::Resized(size) => {
                self.shared.size.set(PhysicalSize::new(size.width, size.height));
                redraw(&self.shared);
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
            E::MouseWheel { delta, phase, .. } => {
                let scale = self.shared.window.borrow().as_ref().map_or(1.0, |w| w.scale_factor()) as f32;
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 60.0, y * 60.0),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32 / scale, p.y as f32 / scale),
                };
                self.pointer(|position| wheel_event(position, dx, dy, phase));
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
                if self.shared.occluded.get() {
                    return;
                }
                let size = self.shared.size.get();
                if let Some(d) = &mut self.draw {
                    if d.config.width != size.width.max(1) || d.config.height != size.height.max(1) {
                        d.resize(size.width, size.height);
                        layout_layers(&self.shared);
                    }
                }
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
        self.shared.menu.poll();
        if self.animating() {
            redraw(&self.shared);
        }
        el.set_control_flow(match slint::platform::duration_until_next_timer_update() {
            Some(d) => ControlFlow::WaitUntil(Instant::now() + d.min(Duration::from_secs(60))),
            None => ControlFlow::Wait,
        });
    }
}

struct Draw {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    copy: wgpu::RenderPipeline,
    over: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    glass: wgpu::RenderPipeline,
    focus: wgpu::RenderPipeline,
    bind: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    groups: RefCell<Vec<Group>>,
    next_group: Cell<usize>,
    pyramid: Option<Pyramid>,
    /// The pyramid holds the page as last rendered.
    page_blurred: bool,
    /// Surface must be reconfigured before the next frame.
    stale: bool,
}

struct Group {
    buffer: wgpu::Buffer,
    a: wgpu::TextureView,
    b: wgpu::TextureView,
    binding: wgpu::BindGroup,
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
            ],
        });
        let layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("glass"), bind_group_layouts: &[Some(&bind)], immediate_size: 0 });
        let premultiplied = wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
        };
        let pipeline = |entry: &str, blend: Option<wgpu::BlendState>, format: wgpu::TextureFormat| {
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
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
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
            copy: pipeline("fs_copy", None, FORMAT),
            over: pipeline("fs_copy", Some(premultiplied), FORMAT),
            down: pipeline("fs_down", None, PYRAMID_FORMAT),
            glass: pipeline("fs_glass", Some(premultiplied), FORMAT),
            focus: pipeline("fs_focus", None, FORMAT),
            bind,
            sampler,
            groups: RefCell::new(Vec::new()),
            next_group: Cell::new(0),
            pyramid: None,
            page_blurred: false,
            stale: false,
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.pyramid = None;
        self.groups.borrow_mut().clear();
        self.stale = true;
    }

    /// Renders dirty layers, then composites page, glass and the sidebar/player on top.
    fn frame(&mut self, s: &Shared, win: LogicalSize) {
        self.next_group.set(0);
        let gpu = s.gpu.clone();
        let d = &gpu.device;
        let scale = s.window.borrow().as_ref().map_or(1.0, |w| w.scale_factor() as f32);
        if std::mem::take(&mut self.stale) {
            self.surface.configure(d, &self.config);
        }
        // Acquire before Skia submits this frame so the fence wait only covers earlier work.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            // Also when the window was hidden before it was ever shown, so winit sends no Occluded.
            wgpu::CurrentSurfaceTexture::Occluded => {
                s.occluded.set(true);
                return;
            }
            _ => {
                self.surface.configure(d, &self.config);
                return;
            }
        };
        for (layer, role) in s.layers.borrow().iter() {
            let Some(role) = role.get().filter(|r| s.shown(*r)) else { continue };
            let size = layer.size.get();
            let fresh = layer.texture.borrow().as_ref().is_none_or(|(t, _)| t.width() != size.width || t.height() != size.height);
            if fresh {
                let t = texture(d, size.width, size.height, "layer");
                let v = t.create_view(&Default::default());
                *layer.texture.borrow_mut() = Some((t, v));
            }
            if fresh || layer.dirty.get() {
                layer.dirty.set(false);
                if role == Role::Page {
                    self.page_blurred = false;
                }
                if let Some((t, _)) = layer.texture.borrow().as_ref() {
                    prepare_layer(&gpu, t, fresh);
                    if let Err(e) = layer.renderer.render_to_texture(t) {
                        eprintln!("nori: a layer was not drawn: {e}");
                    }
                }
            }
        }
        let target = frame.texture.create_view(&Default::default());
        let layers = s.layers.borrow();
        let find = |r: Role| layers.iter().find(|(_, x)| x.get() == Some(r)).and_then(|(l, _)| l.texture.borrow().as_ref().map(|(_, v)| v.clone()));
        let Some(page) = find(Role::Page) else { return };
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        let mut enc = d.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let focus = s.focus.borrow().as_ref().and_then(|f| f());
        let glass_needed = s.sidebar_shown.get() || s.player_shown.get() || focus.is_some();
        if glass_needed {
            self.blur_page(d, &gpu.queue, &mut enc, &page);
        }
        let pyramid = self.pyramid.as_ref().map(|p| p.all.clone());
        // Levels as the window's pixels count them: a level's blur is in pixels, so a sharper screen needs one more.
        let dense = scale.max(1.0).log2();
        let full = uniforms([0.0, 0.0, w, h], [w, h, w, h], [0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4]);
        let g = self.group(d, &gpu.queue, &full, &page, pyramid.as_ref().unwrap_or(&page));
        pass(&mut enc, &target, &self.copy, &g, true);
        if let (Some(f), Some(py)) = (focus, &pyramid) {
            let r = [f.region[0] * scale, f.region[1] * scale, f.region[2] * scale, f.region[3] * scale];
            let u = uniforms(r, [w, h, w, h], [0.0; 4], [0.0; 4], [f.band_top * scale, f.band_h * scale, f.pitch * scale, scale], [0.0; 4]);
            let g = self.group(d, &gpu.queue, &u, &page, py);
            pass(&mut enc, &target, &self.focus, &g, false);
        }
        let mut glass = |role: Role, rect: [f32; 4], look: Glass| {
            let (Some(view), Some(py)) = (find(role), &pyramid) else { return };
            let u = uniforms(rect, [w, h, w, h], look.shape, look.face, look.light, look.gather);
            let g = self.group(d, &gpu.queue, &u, &page, py);
            pass(&mut enc, &target, &self.glass, &g, false);
            let (o, sz) = place(s, role, win);
            let r = [o.x * scale, o.y * scale, sz.width * scale, sz.height * scale];
            let u = uniforms(r, [w, h, w, h], [0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4]);
            let g = self.group(d, &gpu.queue, &u, &view, &view);
            pass(&mut enc, &target, &self.over, &g, false);
        };
        if s.sidebar_shown.get() {
            // Extended far past the window so only the right edge acts as a rim.
            let r = [-8000.0 * scale, -8000.0 * scale, (SIDEBAR_W + 8000.0) * scale, h + 16000.0 * scale];
            glass(Role::Sidebar, r, Glass {
                shape: [0.0, 28.0 * scale, 8.0 * scale, 0.04],
                face: [0.06, 0.22, 1.2, 4.5 + dense],
                light: [0.05, 0.34, 45.0 * scale, 6.0 + dense],
                gather: [180.0 * scale, 1.0, 0.0, 0.0],
            });
        }
        if s.player_shown.get() {
            let (o, sz) = place(s, Role::Player, win);
            let r = [o.x * scale, o.y * scale, sz.width * scale, sz.height * scale];
            // The page shows through, blurred and vivid, dimmed only where too bright for the white controls.
            glass(Role::Player, r, Glass {
                shape: [PLAYER_H * 0.5 * scale, 14.0 * scale, 6.0 * scale, 0.05],
                face: [0.05, 0.24, 1.5, 3.0 + dense],
                light: [0.14, 0.0, 0.0, 0.0],
                gather: [0.0; 4],
            });
        }
        drop(layers);
        gpu.queue.submit([enc.finish()]);
        gpu.queue.present(frame);
        self.groups.borrow_mut().truncate(self.next_group.get());
    }

    /// Draws the page's blur pyramid, each level a 13-tap downsample of the one above it, unless it
    /// already holds the page as last rendered.
    fn blur_page(&mut self, d: &wgpu::Device, q: &wgpu::Queue, enc: &mut wgpu::CommandEncoder, page: &wgpu::TextureView) {
        let (w, h) = ((self.config.width / 2).max(1), (self.config.height / 2).max(1));
        let sized = self.pyramid.as_ref().is_some_and(|p| p.texture.width() == w && p.texture.height() == h);
        if sized && self.page_blurred {
            return;
        }
        if !sized {
            self.pyramid = Some(pyramid(d, w, h));
        }
        self.page_blurred = true;
        let Some(p) = &self.pyramid else { return };
        for (k, target) in p.mips.iter().enumerate() {
            let from = if k == 0 { page } else { &p.mips[k - 1] };
            let (tw, th) = ((w >> k).max(1) as f32, (h >> k).max(1) as f32);
            let u = uniforms([0.0, 0.0, tw, th], [tw, th, tw, th], [0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4]);
            let g = self.group(d, q, &u, from, from);
            pass(enc, target, &self.down, &g, false);
        }
    }

    fn group(&self, d: &wgpu::Device, q: &wgpu::Queue, u: &[u8], a: &wgpu::TextureView, b: &wgpu::TextureView) -> wgpu::BindGroup {
        let slot = self.next_group.get();
        self.next_group.set(slot + 1);
        let mut groups = self.groups.borrow_mut();
        if let Some(group) = groups.get(slot) {
            if &group.a == a && &group.b == b {
                q.write_buffer(&group.buffer, 0, u);
                return group.binding.clone();
            }
        }
        let buf = groups.get(slot).map(|group| group.buffer.clone()).unwrap_or_else(|| d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass"), size: u.len() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        }));
        q.write_buffer(&buf, 0, u);
        let group = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass"),
            layout: &self.bind,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(a) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(b) },
            ],
        });
        let entry = Group { buffer: buf, a: a.clone(), b: b.clone(), binding: group.clone() };
        if slot == groups.len() {
            groups.push(entry);
        } else {
            groups[slot] = entry;
        }
        group
    }
}

/// Skia writes outside wgpu's tracker; initialize first and restore its expected attachment state.
fn prepare_layer(gpu: &Gpu, texture: &wgpu::Texture, fresh: bool) {
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("Slint layer attachment") });
    if fresh {
        let view = texture.create_view(&Default::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Slint layer init"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view, depth_slice: None, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
    } else {
        encoder.transition_resources(std::iter::empty(), [wgpu::TextureTransition { texture, selector: None, state: wgpu::TextureUses::COLOR_TARGET }].into_iter());
    }
    gpu.queue.submit([encoder.finish()]);
}

/// A pyramid `w` x `h` at its largest, halving [`PYRAMID_LEVELS`] times or until a side is one pixel.
fn pyramid(d: &wgpu::Device, w: u32, h: u32) -> Pyramid {
    let levels = PYRAMID_LEVELS.min(32 - w.max(h).leading_zeros());
    let texture = d.create_texture(&wgpu::TextureDescriptor {
        label: Some("pyramid"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PYRAMID_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let mips = (0..levels).map(|k| texture.create_view(&wgpu::TextureViewDescriptor { base_mip_level: k, mip_level_count: Some(1), ..Default::default() })).collect();
    let all = texture.create_view(&Default::default());
    Pyramid { texture, mips, all }
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

/// How a pane of glass looks: glass.wgsl's `shape`, `face`, `light` and `gather`.
struct Glass {
    shape: [f32; 4],
    face: [f32; 4],
    light: [f32; 4],
    gather: [f32; 4],
}

/// Uniforms as glass.wgsl lays them out (six vec4s).
fn uniforms(rect: [f32; 4], view: [f32; 4], shape: [f32; 4], face: [f32; 4], light: [f32; 4], gather: [f32; 4]) -> [u8; 96] {
    let mut bytes = [0; 96];
    for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip([rect, view, shape, face, light, gather].iter().flatten()) {
        out.copy_from_slice(&value.to_ne_bytes());
    }
    bytes
}

/// Adds an empty unified toolbar so the traffic lights sit lower, level with the page's toolbar.
#[cfg(target_os = "macos")]
fn unified_toolbar(window: &WinitWindow) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSToolbar, NSView, NSWindowToolbarStyle};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    // SAFETY: winit's AppKit handle is the content view, alive as long as the window.
    let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
    let Some(ns) = view.window() else { return };
    let toolbar = NSToolbar::new(mtm);
    ns.setToolbar(Some(&toolbar));
    ns.setToolbarStyle(NSWindowToolbarStyle::Unified);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_scroll_moves_over_time() {
        slint::slint! {
            export component ScrollWindow inherits Window {
                width: 100px; height: 100px;
                out property <length> scroll-y: flick.content-y;
                flick := Flickable { content-height: 1000px; Rectangle { height: 1000px; } }
            }
        }
        i_slint_backend_testing::init_integration_test_with_mock_time();
        use slint::ComponentHandle;
        let ui = ScrollWindow::new().unwrap();
        ui.show().unwrap();
        let event = wheel_event(LogicalPosition::new(50.0, 50.0), 0.0, -60.0, winit::event::TouchPhase::Moved);
        ui.window().dispatch_event(event);
        assert!(ui.get_scroll_y() > -60.0, "a wheel notch starts an animation instead of jumping");
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(60));
        slint::platform::update_timers_and_animations();
        assert!(ui.get_scroll_y() < 0.0 && ui.get_scroll_y() > -60.0);
        ui.window().dispatch_event(wheel_event(LogicalPosition::new(50.0, 50.0), 0.0, -60.0, winit::event::TouchPhase::Moved));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(1));
        slint::platform::update_timers_and_animations();
        assert!((ui.get_scroll_y() + 120.0).abs() < 0.1, "successive notches keep their full distance");
    }

    #[test]
    #[ignore = "requires a GPU; run with --ignored"]
    fn layer_pixels_survive_first_sampling_and_redraw() {
        slint::slint! {
            export component TestWindow inherits Window {
                in-out property <color> colour;
                background: colour;
            }
        }
        struct TestPlatform(Rc<Layer>);
        impl slint::platform::Platform for TestPlatform {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
                Ok(self.0.clone())
            }
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let gpu = Gpu { instance, adapter, device, queue };
        let renderer = SkiaWGPU30Renderer::new(gpu.instance.clone(), gpu.adapter.clone(), gpu.device.clone(), gpu.queue.clone()).unwrap();
        let layer = Rc::new_cyclic(|me: &Weak<Layer>| Layer {
            window: slint::Window::new(me.clone() as Weak<dyn WindowAdapter>), renderer,
            size: Cell::new(PhysicalSize::new(64, 64)), dirty: Cell::new(true),
            texture: RefCell::new(None), shared: Weak::new(),
        });
        slint::platform::set_platform(Box::new(TestPlatform(layer.clone()))).unwrap();
        use slint::ComponentHandle;
        let ui = TestWindow::new().unwrap();
        ui.show().unwrap();
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test layer"), size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
        });
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer pixels"), size: 64 * 256, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
        });
        for (fresh, colour, expected) in [
            (true, slint::Color::from_rgb_u8(18, 52, 86), [86, 52, 18, 255]),
            (false, slint::Color::from_rgb_u8(171, 205, 239), [239, 205, 171, 255]),
        ] {
            ui.set_colour(colour);
            prepare_layer(&gpu, &target, fresh);
            layer.renderer.render_to_texture(&target).unwrap();
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(target.as_image_copy(), wgpu::TexelCopyBufferInfo {
                buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(64) },
            }, target.size());
            gpu.queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
            gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            rx.recv().unwrap().unwrap();
            {
                let pixels = buffer.slice(..).get_mapped_range().unwrap();
                for pixel in pixels.as_chunks::<4>().0 { assert_eq!(*pixel, expected, "layer pixels must survive wgpu's first read and subsequent redraws"); }
            }
            buffer.unmap();
        }
    }
}
