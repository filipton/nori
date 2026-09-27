//! The window itself: drawn by Skia on the GPU (Metal on macOS, through wgpu; Vulkan or OpenGL on Linux),
//! and on macOS made the way Apple's own apps are - no title bar, the content under the traffic lights, and
//! the system's blur of whatever is behind the window (NSVisualEffectView) showing through where the
//! interface is clear.

use slint::winit_030::winit::window::WindowAttributes;
use slint::ComponentHandle;

use crate::{AppWindow, PlayerBar};

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


/// The player's size and place: centred over the page (right of the sidebar), a little above the
/// window's bottom, as wide as the page allows up to 820 points.
const SIDEBAR: f64 = 240.0;
const PLAYER_H: f64 = 60.0;
const PLAYER_MAX_W: f64 = 820.0;
const PLAYER_BOTTOM: f64 = 12.0;

/// The player window put over the main window's page, as a child window (it moves with the main window
/// and stays over it), with Liquid Glass behind its controls. Tried on the loop's first turns until both
/// windows exist.
#[cfg(target_os = "macos")]
pub fn attach_player(main: slint::Weak<AppWindow>, bar: slint::Weak<PlayerBar>) {
    use slint::winit_030::WinitWindowAccessor;
    let (Some(m), Some(b)) = (main.upgrade(), bar.upgrade()) else { return };
    let ready = m.window().has_winit_window() && b.window().has_winit_window();
    if !ready {
        slint::Timer::single_shot(std::time::Duration::from_millis(16), move || attach_player(main, bar));
        return;
    }
    let done = (|| -> Result<(), String> {
        let (mw, bw) = (ns_window(&m)?, ns_window(&b)?);
        dress_player(&bw)?;
        // SAFETY: both windows are live AppKit windows, used on the main thread.
        unsafe { mw.addChildWindow_ordered(&bw, objc2_app_kit::NSWindowOrderingMode::Above) };
        Ok(())
    })();
    if let Err(e) = done {
        eprintln!("nori: the player has no glass window: {e}");
    }
    place_player(&m, &b, true);
}

#[cfg(target_os = "macos")]
fn ns_window(w: &impl slint::ComponentHandle) -> Result<objc2::rc::Retained<objc2_app_kit::NSWindow>, String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;
    w.window()
        .with_winit_window(|w| {
            let handle = w.window_handle().map_err(|e| e.to_string())?;
            let RawWindowHandle::AppKit(h) = handle.as_raw() else { return Err("not an AppKit window".to_string()) };
            // SAFETY: winit hands out the window's live content view; this runs on the main thread.
            let view: &objc2_app_kit::NSView = unsafe { h.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
            view.window().ok_or_else(|| "the view has no window".to_string())
        })
        .ok_or_else(|| "no window yet".to_string())?
}

/// The player window clear, without a shadow of its own, and the system's glass behind its content view.
#[cfg(target_os = "macos")]
fn dress_player(bw: &objc2_app_kit::NSWindow) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAutoresizingMaskOptions, NSColor, NSGlassEffectView, NSWindowOrderingMode};
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    bw.setOpaque(false);
    bw.setBackgroundColor(Some(&NSColor::clearColor()));
    bw.setHasShadow(false);
    let view = bw.contentView().ok_or("no content view")?;
    let frame_view = unsafe { view.superview() }.ok_or("the content view has no superview")?;
    let glass = NSGlassEffectView::initWithFrame(mtm.alloc(), view.frame());
    glass.setCornerRadius(PLAYER_H / 2.0);
    glass.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    frame_view.addSubview_positioned_relativeTo(&glass, NSWindowOrderingMode::Below, Some(&view));
    Ok(())
}

/// Moves the player window over the page's bottom, or out of sight (Now Playing, the sign-in page).
#[cfg(target_os = "macos")]
pub fn place_player(main: &AppWindow, bar: &PlayerBar, shown: bool) {
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    let (Ok(mw), Ok(bw)) = (ns_window(main), ns_window(bar)) else { return };
    let f = mw.frame();
    let page = f.size.width - SIDEBAR;
    let w = (page - 32.0).clamp(200.0, PLAYER_MAX_W);
    let rect = NSRect::new(NSPoint::new(f.origin.x + SIDEBAR + (page - w) / 2.0, f.origin.y + PLAYER_BOTTOM), NSSize::new(w, PLAYER_H));
    bw.setFrame_display(rect, true);
    bw.setAlphaValue(if shown { 1.0 } else { 0.0 });
    bw.setIgnoresMouseEvents(!shown);
}

#[cfg(not(target_os = "macos"))]
pub fn attach_player(_: slint::Weak<AppWindow>, _: slint::Weak<PlayerBar>) {}

#[cfg(not(target_os = "macos"))]
pub fn place_player(_: &AppWindow, _: &PlayerBar, _: bool) {}
