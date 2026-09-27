//! The window itself: drawn by Skia on the GPU (Metal on macOS, through wgpu; Vulkan or OpenGL on Linux),
//! and on macOS made the way Apple's own apps are - no title bar, the content under the traffic lights, and
//! the system's blur of whatever is behind the window (NSVisualEffectView) showing through where the
//! interface is clear.

use slint::winit_030::winit::window::WindowAttributes;
use slint::ComponentHandle;

use crate::AppWindow;

/// Picks the backend before any window exists: winit, Skia, and Metal on macOS.
pub fn backend() -> Result<(), String> {
    let pick = || {
        slint::BackendSelector::new().backend_name("winit".into()).renderer_name("skia".into()).with_winit_window_attributes_hook(attributes)
    };
    #[cfg(target_os = "macos")]
    let chosen = pick().require_metal().select().or_else(|e| {
        eprintln!("nori: Metal was refused ({e}); letting Skia choose");
        pick().select()
    });
    #[cfg(not(target_os = "macos"))]
    let chosen = pick().select();
    chosen.map_err(|e| format!("the window's renderer: {e}"))
}

fn attributes(a: WindowAttributes) -> WindowAttributes {
    #[cfg(target_os = "macos")]
    {
        use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;
        a.with_transparent(true).with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true)
    }
    #[cfg(not(target_os = "macos"))]
    a
}

/// What the interface needs to know of the window: whether the system blurs behind it, and how far down
/// the traffic lights reach.
pub fn dress(ui: &AppWindow) {
    ui.set_vibrancy(cfg!(target_os = "macos"));
    // Room for the traffic lights at the sidebar's top.
    ui.set_inset_top(if cfg!(target_os = "macos") { 52.0 } else { 24.0 });
    // SF Pro, as CoreText names the system's font; Inter (bundled) elsewhere.
    if cfg!(target_os = "macos") {
        ui.set_font("System Font".into());
    }
}

/// Puts the system's blur behind the window, once winit has made it: the window exists only once the event
/// loop runs, so this tries on the loop's first turns until it does.
pub fn blur(ui: &AppWindow) {
    #[cfg(target_os = "macos")]
    try_blur(ui.as_weak());
    #[cfg(not(target_os = "macos"))]
    let _ = ui;
}

#[cfg(target_os = "macos")]
fn try_blur(weak: slint::Weak<AppWindow>) {
    use slint::winit_030::WinitWindowAccessor;
    let Some(ui) = weak.upgrade() else { return };
    match ui.window().with_winit_window(behind) {
        Some(Err(e)) => eprintln!("nori: no blur behind the window: {e}"),
        Some(Ok(())) => {}
        None => slint::Timer::single_shot(std::time::Duration::from_millis(16), move || try_blur(weak)),
    }
}

/// The blur view goes below the content view, in the window's frame view: the content view's own layer is
/// the one Skia draws into (through a transparent Metal layer), and a subview of it would cover the drawing.
#[cfg(target_os = "macos")]
fn behind(w: &slint::winit_030::winit::window::Window) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = w.window_handle().map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return Err("not an AppKit window".into()) };
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    // SAFETY: winit hands out the window's live content view, and this runs on the main thread, where
    // AppKit's views are used.
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    let frame_view = unsafe { view.superview() }.ok_or("the content view has no superview")?;
    let blur = NSVisualEffectView::initWithFrame(mtm.alloc(), view.frame());
    blur.setMaterial(NSVisualEffectMaterial::Sidebar);
    blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    blur.setState(NSVisualEffectState::Active);
    blur.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    frame_view.addSubview_positioned_relativeTo(&blur, NSWindowOrderingMode::Below, Some(view));
    Ok(())
}

/// The window follows the pointer from here, as a titlebar does (the content runs up under it).
pub fn drag(ui: &slint::Weak<AppWindow>) {
    use slint::winit_030::WinitWindowAccessor;
    if let Some(ui) = ui.upgrade() {
        ui.window().with_winit_window(|w| {
            let _ = w.drag_window();
        });
    }
}

/// A double click on the titlebar: the window zooms, or comes back.
pub fn zoom(ui: &slint::Weak<AppWindow>) {
    if let Some(ui) = ui.upgrade() {
        let w = ui.window();
        w.set_maximized(!w.is_maximized());
    }
}
