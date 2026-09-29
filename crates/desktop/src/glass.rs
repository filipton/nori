//! Platform-dependent window properties: the system font and the macOS traffic-light inset.

use crate::AppWindow;

pub fn dress(ui: &AppWindow) {
    ui.set_vibrancy(false);
    // Room for the traffic lights at the sidebar's top.
    ui.set_inset_top(if cfg!(target_os = "macos") { 44.0 } else { 16.0 });
    // SF Pro on macOS; the bundled Inter elsewhere.
    if cfg!(target_os = "macos") {
        ui.set_font("System Font".into());
    }
}
