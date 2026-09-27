//! The menu bar, macOS's own (through muda): the app's menu, Controls, View and Window, with the player's
//! shortcuts. No Edit menu: ⌘C and ⌘V reach the text fields as keys, which Slint turns into copy and paste.
//! Each item runs an action the window registered by name ([`action`]).

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static ACTIONS: RefCell<HashMap<String, Box<dyn Fn()>>> = RefCell::new(HashMap::new());
    #[cfg(target_os = "macos")]
    static MENU: RefCell<Option<muda::Menu>> = const { RefCell::new(None) };
}

/// What the item named `id` does.
pub fn action(id: &str, f: impl Fn() + 'static) {
    ACTIONS.with(|a| a.borrow_mut().insert(id.to_string(), Box::new(f)));
}

/// The menu bar, once the app runs.
#[cfg(target_os = "macos")]
pub fn install() {
    use muda::{accelerator::Accelerator, AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
    if MENU.with(|m| m.borrow().is_some()) {
        return;
    }
    let item = |id: &str, title: &str, keys: Option<&str>| MenuItem::with_id(id, title, true, keys.and_then(|k| k.parse::<Accelerator>().ok()));
    let app = Submenu::with_items(
        "nori",
        true,
        &[
            &PredefinedMenuItem::about(Some("About nori"), Some(AboutMetadata { name: Some("nori".into()), version: Some(env!("CARGO_PKG_VERSION").into()), ..Default::default() })),
            &PredefinedMenuItem::separator(),
            &item("settings", "Settings…", Some("CmdOrCtrl+Comma")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ],
    );
    let controls = Submenu::with_items(
        "Controls",
        true,
        &[
            &item("toggle", "Play/Pause", None),
            &item("next", "Next", Some("CmdOrCtrl+Right")),
            &item("previous", "Previous", Some("CmdOrCtrl+Left")),
            &PredefinedMenuItem::separator(),
            &item("shuffle", "Shuffle", None),
            &item("repeat", "Repeat", None),
        ],
    );
    let view = Submenu::with_items(
        "View",
        true,
        &[
            &item("home", "Home", Some("CmdOrCtrl+1")),
            &item("albums", "Albums", Some("CmdOrCtrl+2")),
            &item("artists", "Artists", Some("CmdOrCtrl+3")),
            &item("songs", "Songs", Some("CmdOrCtrl+4")),
            &item("search", "Search", Some("CmdOrCtrl+F")),
            &PredefinedMenuItem::separator(),
            &item("queue", "Playing Next", Some("CmdOrCtrl+U")),
            &item("lyrics", "Lyrics", Some("CmdOrCtrl+L")),
            &item("full", "Now Playing", Some("CmdOrCtrl+Shift+F")),
        ],
    );
    let window = Submenu::with_items("Window", true, &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::maximize(None), &PredefinedMenuItem::close_window(None)]);
    let (Ok(app), Ok(controls), Ok(view), Ok(window)) = (app, controls, view, window) else { return };
    let menu = Menu::new();
    if menu.append_items(&[&app, &controls, &view, &window]).is_err() {
        return;
    }
    menu.init_for_nsapp();
    if let Some(w) = window.items().first() {
        let _ = w;
    }
    window.set_as_windows_menu_for_nsapp();
    MENU.with(|m| *m.borrow_mut() = Some(menu));
}

#[cfg(not(target_os = "macos"))]
pub fn install() {}

/// The items chosen since the last look, run.
pub fn poll() {
    #[cfg(target_os = "macos")]
    while let Ok(e) = muda::MenuEvent::receiver().try_recv() {
        ACTIONS.with(|a| {
            if let Some(f) = a.borrow().get(e.id().as_ref()) {
                f();
            }
        });
    }
}
