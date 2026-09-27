//! What the interface needs to know of the window it is drawn in (compositor.rs draws it): the system's
//! font, and how far down the traffic lights reach on macOS.

use crate::AppWindow;

pub fn dress(ui: &AppWindow) {
    ui.set_vibrancy(false);
    // Room for the traffic lights at the sidebar's top.
    ui.set_inset_top(if cfg!(target_os = "macos") { 44.0 } else { 16.0 });
    // SF Pro, as CoreText names the system's font; Inter (bundled) elsewhere.
    if cfg!(target_os = "macos") {
        ui.set_font("System Font".into());
    }
}
