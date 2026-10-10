//! nori's Slint desktop client over the core and nori-engine. Shares its data directory with the
//! terminal client; this crate only draws and forwards input.

mod app;
mod compositor;
#[cfg(all(feature = "test-control", unix))]
mod control;
mod eq;
mod jam;
mod lyrics;
mod menu;
mod session;
mod settings;
mod sung;
mod words;

use std::path::PathBuf;

use nori_core::settings::SavedServer;

slint::include_modules!();

/// $XDG_DATA_HOME/nori, else ~/.local/share/nori (same as the terminal client).
fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("nori");
    }
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h).join(".local/share/nori"),
        None => std::env::temp_dir().join("nori"),
    }
}

fn main() -> Result<(), String> {
    // Return large freed cover/analysis buffers to Linux.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 256 * 1024);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 128 * 1024);
    }
    let usage = || {
        let control = if cfg!(all(feature = "test-control", unix)) { " [--control-socket PATH]" } else { "" };
        format!("usage: nori-desktop [--data DIR] [--url URL --user USER --password PASSWORD]{control}")
    };
    let mut data = data_dir();
    #[cfg(all(feature = "test-control", unix))]
    let mut control_socket = None;
    let (mut url, mut user, mut password) = (None, None, String::new());
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut v = || args.next().ok_or_else(usage);
        match a.as_str() {
            #[cfg(all(feature = "test-control", unix))]
            "--control-socket" => control_socket = Some(PathBuf::from(v()?)),
            "--data" => data = v()?.into(),
            "--url" => url = Some(v()?),
            "--user" => user = Some(v()?),
            "--password" => password = v()?,
            _ => return Err(usage()),
        }
    }
    std::fs::create_dir_all(&data).map_err(|e| format!("{}: {e}", data.display()))?;
    let db = nori_host::db_path(&data);
    let mut prefs = crate::session::app().settings.open(&db).map_err(|e| format!("the settings: {e}"))?;
    // A server given on the command line is added if new, and made active.
    if let (Some(url), Some(user)) = (url, user) {
        let found = prefs.servers.iter().find(|s| s.url == url && s.user == user).map(|s| s.id.clone());
        let id = found.unwrap_or_else(|| {
            let s = SavedServer { id: nori_core::settings::new_server_id(), url: url.clone(), user: user.clone(), password: password.clone(), ..Default::default() };
            prefs.servers.push(s.clone());
            s.id
        });
        prefs.servers.iter_mut().filter(|s| s.id == id && !password.is_empty()).for_each(|s| s.password = password.clone());
        prefs.active_server_id = id;
        crate::session::app().settings.put(prefs);
    }
    let compositor = compositor::install()?;
    let ui = AppWindow::new().map_err(|e| e.to_string())?;
    // Room for the traffic lights; SF Pro on macOS (the bundled Inter elsewhere).
    if cfg!(target_os = "macos") {
        ui.set_inset_top(44.0);
        ui.set_font("System Font".into());
    }
    let app = app::start(&ui, data, compositor);
    #[cfg(all(feature = "test-control", unix))]
    let _control = control_socket.map(|path| control::start(path, ui.as_weak())).transpose()?;
    let r = ui.run().map_err(|e| e.to_string());
    app::stop(&app);
    nori_core::background::flush();
    r
}
