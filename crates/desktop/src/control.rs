//! Opt-in developer control socket; actions enter the same callbacks as UI gestures.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use slint::ComponentHandle;

use crate::AppWindow;

#[derive(Deserialize)]
#[serde(tag = "command", content = "arg", rename_all = "snake_case")]
enum Command {
    State,
    Open(View),
    Do(Action),
    Panel(Panel),
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum View { Home, Search, Albums, Artists, Playlists, Songs, Settings, Equalizer, Fullscreen }

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action { Play, Pause, Toggle, Next, Previous, CloseFullscreen }

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Panel { None, Queue, Lyrics, Devices }

#[derive(Serialize)]
struct State {
    view: i32,
    fullscreen: bool,
    panel: i32,
    playing: bool,
    buffering: bool,
    animating: bool,
    position_ms: i32,
    duration_ms: i32,
    title: String,
    artist: String,
    note: String,
    note_error: bool,
}

fn dispatch(ui: &AppWindow, command: Command) -> State {
    match command {
        Command::State => {}
        Command::Open(View::Fullscreen) => {
            if ui.get_has_song() {
                ui.set_full_player(true);
                ui.invoke_player_changed();
            }
        }
        Command::Open(view) => {
            ui.set_full_player(false);
            ui.invoke_go(match view {
                View::Home => 0, View::Search => 1, View::Albums => 2, View::Artists => 3,
                View::Playlists => 4, View::Songs => 5, View::Settings => 8, View::Equalizer => 9,
                View::Fullscreen => unreachable!(),
            });
        }
        Command::Panel(panel) => {
            ui.set_inspector(match panel { Panel::None => 0, Panel::Queue => 1, Panel::Lyrics => 2, Panel::Devices => 3 });
            ui.invoke_player_changed();
        }
        Command::Do(action) => match action {
            Action::Play if !ui.get_playing() => ui.invoke_toggle(),
            Action::Pause if ui.get_playing() => ui.invoke_toggle(),
            Action::Play | Action::Pause => {}
            Action::Toggle => ui.invoke_toggle(),
            Action::Next => ui.invoke_next(),
            Action::Previous => ui.invoke_previous(),
            Action::CloseFullscreen => {
                ui.set_full_player(false);
                ui.invoke_player_changed();
            }
        },
    }
    State {
        view: ui.get_view(), fullscreen: ui.get_full_player(), panel: ui.get_inspector(),
        playing: ui.get_playing(), buffering: ui.get_buffering(), animating: ui.window().has_active_animations(), position_ms: ui.get_position_ms(),
        duration_ms: ui.get_duration_ms(), title: ui.get_now_title().to_string(),
        artist: ui.get_now_artist().to_string(), note: ui.get_note().to_string(), note_error: ui.get_note_error(),
    }
}

pub struct Control {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub fn start(path: PathBuf, ui: slint::Weak<AppWindow>) -> Result<Control, String> {
    let listener = UnixListener::bind(&path).map_err(|e| format!("control socket: {e}"))?;
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("control socket permissions: {e}"));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stopped.load(Ordering::Acquire) { break; }
            let Ok(mut stream) = stream else { break };
            let timeout = Some(Duration::from_secs(2));
            let _ = stream.set_read_timeout(timeout);
            let _ = stream.set_write_timeout(timeout);
            let reply = request(&mut stream, &ui).unwrap_or_else(|e| serde_json::json!({"error": e}));
            let _ = writeln!(stream, "{reply}");
        }
    });
    Ok(Control { path, stop, thread: Some(thread) })
}

fn request(stream: &mut UnixStream, ui: &slint::Weak<AppWindow>) -> Result<serde_json::Value, String> {
    let mut line = String::new();
    BufReader::new(stream.take(4097)).read_line(&mut line).map_err(|e| e.to_string())?;
    if line.len() > 4096 { return Err("command is too long".into()); }
    let command = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::sync_channel(1);
    ui.upgrade_in_event_loop(move |ui| { let _ = tx.send(dispatch(&ui, command)); }).map_err(|e| e.to_string())?;
    let state = rx.recv_timeout(Duration::from_secs(2)).map_err(|e| e.to_string())?;
    serde_json::to_value(state).map_err(|e| e.to_string())
}

impl Drop for Control {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn actions_use_ui_callbacks_and_report_state() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let weak = ui.as_weak();
        ui.on_go(move |view| weak.unwrap().set_view(view));
        let weak = ui.as_weak();
        ui.on_toggle(move || { let ui = weak.unwrap(); ui.set_playing(!ui.get_playing()); });
        let skips = Rc::new(Cell::new(0));
        let next = skips.clone();
        ui.on_next(move || next.set(next.get() + 1));
        let previous = skips.clone();
        ui.on_previous(move || previous.set(previous.get() - 1));
        ui.set_has_song(true);
        ui.set_now_title("Silence".into());
        assert_eq!(dispatch(&ui, Command::Open(View::Albums)).view, 2);
        assert!(dispatch(&ui, Command::Open(View::Fullscreen)).fullscreen);
        assert_eq!(dispatch(&ui, Command::Panel(Panel::Queue)).panel, 1);
        assert!(dispatch(&ui, Command::Do(Action::Play)).playing);
        assert!(dispatch(&ui, Command::Do(Action::Play)).playing);
        dispatch(&ui, Command::Do(Action::Next));
        assert_eq!(skips.get(), 1);
        dispatch(&ui, Command::Do(Action::Previous));
        assert_eq!(skips.get(), 0);
        assert!(!dispatch(&ui, Command::Do(Action::Pause)).playing);
        assert!(!dispatch(&ui, Command::Do(Action::CloseFullscreen)).fullscreen);
        assert_eq!(dispatch(&ui, Command::State).title, "Silence");
    }
}
