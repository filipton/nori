//! The window's state and what its buttons do. Lives on Slint's event loop thread: the window's callbacks,
//! the engine's events and the workers' answers (`Msg`, through `session::Tx`) all arrive here, one at a
//! time. The only timer is the seek bar's, and it runs only while music plays.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nori_core::playlist::PlaylistView;
use nori_core::search::SearchView;
use nori_core::settings::SavedServer;
use nori_core::settings_store;
use nori_core::Song;
use nori_covers::loader::Ticket;
use nori_covers::memory::Image as Picture;
use nori_engine::{Event, State};
use nori_http::Http;
use nori_look::cover::CoverColours;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel};

use crate::session::{self, Data, Fetch, Msg, Req, Session};
use crate::words;
use crate::{AppWindow, Card, LyricPiece, PlayerBar, Shelf, SidebarWindow, SongRow};

/// Pixels a side: a card's cover, and the large one (now playing, a page's), whose colours are worked out too.
const SMALL_PX: u32 = 256;
const LARGE_PX: u32 = 800;
/// An artist's picture across the page's whole width.
const HERO_PX: u32 = 1600;
/// Decoded covers kept: the cards on a few screens, and the few large ones. A cover drawn in the last
/// `IN_USE` is kept beyond these, so a screen with more than fits never drops what it shows.
const SMALL_KEPT: usize = 240;
const LARGE_KEPT: usize = 12;
const IN_USE: Duration = Duration::from_secs(3);
/// Covers asked for and not come yet; the oldest past this are let go (the pictures scrolled away).
const PENDING_KEPT: usize = 1000;
/// A cover on its way that no picture has looked for in this long is off the screen, and let go.
const GONE: Duration = Duration::from_millis(1500);

// The views, as app.slint numbers them.
const HOME: i32 = 0;
const SEARCH: i32 = 1;
const ALBUMS: i32 = 2;
const ARTISTS: i32 = 3;
const PLAYLISTS: i32 = 4;
const SONGS: i32 = 5;
const PAGE: i32 = 6;
const LOGIN: i32 = 7;
const SETTINGS: i32 = 8;
const EQUALIZER: i32 = 9;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static ART: RefCell<Art> = RefCell::new(Art::default());
}

/// Runs `f` on the window's state, unless it is already in use (a callback fired while it was being changed).
fn with(f: impl FnOnce(&mut App)) {
    APP.with(|a| match a.try_borrow_mut() {
        Ok(mut a) => {
            if let Some(app) = a.as_mut() {
                f(app);
            }
        }
        Err(_) => eprintln!("nori: a callback came while the window's state was busy; dropped"),
    });
}

/// A message from another thread, on the window's.
pub fn take(m: Msg) {
    with(|app| app.take(m));
}

/// The library's lists as the server gave them, before the find field narrows them.
#[derive(Default)]
struct Lists {
    albums: Vec<Card>,
    artists: Vec<Card>,
    playlists: Vec<Card>,
}

/// The decoded covers, looked up by the pictures as they draw (`art` in app.slint). A picture not here yet
/// is asked for once; its arrival bumps `covers-rev`, and every picture looks again. The least lately drawn
/// go first when there are too many.
#[derive(Default)]
struct Art {
    images: HashMap<String, (Image, Instant)>,
    /// The page colours of the large covers, by cover id.
    colours: HashMap<String, Rc<CoverColours>>,
    asked: HashSet<String>,
    wanted: Vec<(String, i32)>,
    /// When a picture last looked for a cover still on its way: one not looked for lately has left the
    /// screen, and its request makes way for the ones on it.
    missing: HashMap<String, Instant>,
}

/// A cover's name among the pictures: its size's letter (s a card's, l large, x an artist's hero) and its id.
fn key(id: &str, size: i32) -> String {
    format!("{}:{id}", ['s', 'l', 'x'][size.clamp(0, 2) as usize])
}

/// The `art` callback: the cover `id` at size 0 (a card's), 1 (large) or 2 (a page's whole width), or
/// nothing yet.
fn art(id: SharedString, size: i32) -> Image {
    if id.is_empty() {
        return Image::default();
    }
    let size = size.clamp(0, 2);
    let k = key(&id, size);
    ART.with(|a| {
        let mut a = a.borrow_mut();
        if let Some((i, drawn)) = a.images.get_mut(&k) {
            *drawn = Instant::now();
            return i.clone();
        }
        a.missing.insert(k.clone(), Instant::now());
        if a.asked.insert(k) {
            if a.wanted.is_empty() {
                // Asked for after this frame's drawing, not from inside it.
                Timer::single_shot(Duration::ZERO, || with(App::ask_covers));
            }
            a.wanted.push((id.to_string(), size));
        }
        Image::default()
    })
}

fn picture(p: &Picture) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&p.pixels, p.width, p.height))
}

/// The cover's blurred wash (ARGB, square) as a picture.
fn wash(c: &CoverColours) -> Image {
    let Some(w) = &c.wash else { return Image::default() };
    let side = (w.len() as f64).sqrt() as u32;
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(side, side);
    for (px, argb) in buf.make_mut_slice().iter_mut().zip(w) {
        let [a, r, g, b] = argb.to_be_bytes();
        *px = Rgba8Pixel { r, g, b, a };
    }
    Image::from_rgba8(buf)
}

fn colour(argb: u32) -> Color {
    Color::from_argb_encoded(argb)
}

/// What the events said before the engine's status caught up (it is written at the end of the same wake).
#[derive(Default)]
struct Said {
    state: Option<State>,
    song: Option<String>,
    tries: u32,
}

pub struct App {
    ui: slint::Weak<AppWindow>,
    data: PathBuf,
    http: Arc<Http>,
    session: Option<Session>,
    /// The song heard, by id, and its record.
    heard: Option<String>,
    song: Option<Song>,
    said: Said,
    queue: Option<PlaylistView>,
    /// The songs behind the lists a click plays from: the page's, the songs list's, the search's.
    page_songs: Vec<Song>,
    page_fetch: Option<Fetch>,
    songs: Vec<Song>,
    search_songs: Vec<Song>,
    /// The read the page shown waits for.
    want: Option<Req>,
    shelves: Rc<VecModel<Shelf>>,
    /// The player as a window of its own over the system's glass (macOS); None where the page draws it.
    player: Option<PlayerBar>,
    /// The sidebar as the system's glass over the page (macOS); None where the page draws it.
    sidebar: Option<SidebarWindow>,
    /// The cover whose wash is the window's backdrop.
    backdrop: Option<String>,
    tickets: VecDeque<(String, Ticket)>,
    tick: Timer,
    tick_ms: u64,
    /// The library's lists as the server gave them, and the text they are narrowed by.
    lists: Lists,
    find: String,
    /// What the settings pages show besides the settings, as last worked out.
    facts: crate::settings::Facts,
    /// The engine has its shallow buffer for the equalizer.
    tuning: bool,
    again: Timer,
    /// The lyrics of the song heard, and their clock; when the next line is due.
    lyrics: Option<crate::lyrics::SongLyrics>,
    lyrics_timer: Timer,
    /// The lit line's pieces in the panel and in Now Playing, changed in place frame to frame.
    pieces: [Rc<VecModel<LyricPiece>>; 2],
    /// The queue's rows, changed in place so the rows that stay stay put; the rows going out go first, and
    /// the list settles to `queue_next` once they have.
    queue_rows: Rc<VecModel<SongRow>>,
    queue_next: Option<Vec<SongRow>>,
    queue_timer: Timer,
    note: Timer,
    search: Timer,
}

/// The player window, on macOS: its buttons do what the main window's do, and it is put over the page once
/// both windows exist.
fn player(ui: &AppWindow) -> Option<PlayerBar> {
    let bar = PlayerBar::new().map_err(|e| eprintln!("nori: no player window: {e}")).ok()?;
    ui.set_native_player(true);
    bar.set_font(ui.get_font());
    bar.on_art(|id, size, _rev| art(id, size));
    let main = ui.as_weak();
    let call = move |f: fn(&AppWindow)| {
        let main = main.clone();
        move || {
            if let Some(m) = main.upgrade() {
                f(&m);
            }
        }
    };
    bar.on_toggle(call(|m| m.invoke_toggle()));
    bar.on_next(call(|m| m.invoke_next()));
    bar.on_previous(call(|m| m.invoke_previous()));
    bar.on_toggle_shuffle(call(|m| m.invoke_toggle_shuffle()));
    bar.on_cycle_repeat(call(|m| m.invoke_cycle_repeat()));
    bar.on_open_full(call(|m| {
        if m.get_has_song() {
            m.set_full_player(true);
            m.invoke_player_changed();
        }
    }));
    let main = ui.as_weak();
    bar.on_seek(move |v| {
        if let Some(m) = main.upgrade() {
            m.invoke_seek(v);
        }
    });
    let main = ui.as_weak();
    bar.on_set_volume(move |v| {
        if let Some(m) = main.upgrade() {
            m.invoke_set_volume(v);
        }
    });
    let main = ui.as_weak();
    bar.on_set_inspector(move |i| {
        if let Some(m) = main.upgrade() {
            m.set_inspector(i);
            m.invoke_player_changed();
        }
    });
    Some(bar)
}

/// The sidebar window, on macOS: its rows do what the main window's do; its view is moved into the system's
/// glass over the page once both windows exist.
fn sidebar(ui: &AppWindow) -> Option<SidebarWindow> {
    let side = SidebarWindow::new().map_err(|e| eprintln!("nori: no glass sidebar: {e}")).ok()?;
    side.set_font(ui.get_font());
    side.set_inset_top(ui.get_inset_top());
    side.on_art(|id, size, _rev| art(id, size));
    let main = ui.as_weak();
    side.on_go(move |v| {
        if let Some(m) = main.upgrade() {
            m.invoke_go(v);
        }
    });
    let main = ui.as_weak();
    side.on_open_playlist(move |id| {
        if let Some(m) = main.upgrade() {
            m.invoke_open_playlist(id);
        }
    });
    let main = ui.as_weak();
    side.on_open_accounts(move || {
        if let Some(m) = main.upgrade() {
            m.invoke_open_accounts();
        }
    });
    let main = ui.as_weak();
    side.on_drag_window(move || {
        if let Some(m) = main.upgrade() {
            m.invoke_drag_window();
        }
    });
    let main = ui.as_weak();
    side.on_zoom_window(move || {
        if let Some(m) = main.upgrade() {
            m.invoke_zoom_window();
        }
    });
    ui.set_native_sidebar(true);
    Some(side)
}

pub fn start(ui: &AppWindow, data: PathBuf) {
    let http = Http::new();
    let shelves = Rc::new(VecModel::from(
        session::HOME_ROWS.iter().map(|(t, _)| Shelf { title: (*t).into(), cards: ModelRc::default() }).collect::<Vec<_>>(),
    ));
    ui.set_shelves(ModelRc::from(shelves.clone()));
    ui.set_greeting("Home".into());
    ui.on_art(|id, size, _rev| art(id, size));
    wire(ui);
    let app = App {
        ui: ui.as_weak(),
        data,
        http,
        session: None,
        heard: None,
        song: None,
        said: Said::default(),
        queue: None,
        page_songs: Vec::new(),
        page_fetch: None,
        songs: Vec::new(),
        search_songs: Vec::new(),
        want: None,
        shelves,
        backdrop: None,
        sidebar: sidebar(ui),
        player: player(ui),
        tickets: VecDeque::new(),
        tick: Timer::default(),
        tick_ms: 0,
        facts: crate::settings::Facts::default(),
        lists: Lists::default(),
        find: String::new(),
        tuning: false,
        again: Timer::default(),
        lyrics: None,
        lyrics_timer: Timer::default(),
        pieces: [Rc::new(VecModel::default()), Rc::new(VecModel::default())],
        queue_rows: Rc::new(VecModel::default()),
        queue_next: None,
        queue_timer: Timer::default(),
        note: Timer::default(),
        search: Timer::default(),
    };
    ui.set_queue(ModelRc::from(app.queue_rows.clone()));
    ui.set_lyric_pieces_side(ModelRc::from(app.pieces[0].clone()));
    ui.set_lyric_pieces_full(ModelRc::from(app.pieces[1].clone()));
    APP.with(|a| *a.borrow_mut() = Some(app));
    // The compositor draws the page with the sidebar and the player on glass over it.
    with(|a| crate::compositor::roles(ui.window(), a.sidebar.as_ref().map(|s| s.window()), a.player.as_ref().map(|p| p.window())));
    menu_actions(ui);
    // Now Playing's lyrics: the line sung sharp, the rest blurred by the compositor.
    let weak = ui.as_weak();
    crate::compositor::set_focus_source(move || {
        let ui = weak.upgrade()?;
        if !ui.get_full_player() || ui.get_full_panel() != 2 || ui.get_lyrics_lines().row_count() == 0 || !ui.get_lyrics_synced() {
            return None;
        }
        let size = ui.window().size().to_logical(ui.window().scale_factor());
        let x = size.width / 2.0;
        // Clear of the volume's pill above and the lyrics and queue pill below.
        Some(crate::compositor::Focus { region: [x, 48.0, size.width - x - 100.0, size.height - 48.0 - 56.0], band_top: size.height * 0.36 - 8.0, band_h: ui.get_full_lyric_h() + 16.0 })
    });
    with(|a| a.settings_shown());
    let prefs = settings_store::settings_current().unwrap_or_default();
    match prefs.servers.iter().find(|s| s.id == prefs.active_server_id).cloned() {
        Some(p) => with(|app| app.open(p)),
        None => ui.set_view(LOGIN),
    }
}

/// What the menu bar's items do: the same as the window's own buttons and keys.
fn menu_actions(ui: &AppWindow) {
    let on = |id: &str, f: fn(&AppWindow)| {
        let main = ui.as_weak();
        crate::menu::action(id, move || {
            if let Some(m) = main.upgrade() {
                f(&m);
            }
        });
    };
    on("toggle", |m| m.invoke_toggle());
    on("next", |m| m.invoke_next());
    on("previous", |m| m.invoke_previous());
    on("shuffle", |m| m.invoke_toggle_shuffle());
    on("repeat", |m| m.invoke_cycle_repeat());
    on("home", |m| m.invoke_go(HOME));
    on("albums", |m| m.invoke_go(ALBUMS));
    on("artists", |m| m.invoke_go(ARTISTS));
    on("songs", |m| m.invoke_go(SONGS));
    on("search", |m| m.invoke_go(SEARCH));
    on("settings", |m| m.invoke_go(SETTINGS));
    on("queue", |m| m.set_inspector(if m.get_inspector() == 1 { 0 } else { 1 }));
    on("lyrics", |m| m.set_inspector(if m.get_inspector() == 2 { 0 } else { 2 }));
    on("full", |m| {
        if m.get_has_song() {
            m.set_full_player(!m.get_full_player());
        }
    });
}

/// The window closed: the queue kept, the engine stopped.
pub fn stop() {
    with(|app| {
        if let Some(s) = app.session.take() {
            session::own::keep(session::own::VOLUME, s.volume.get().to_string());
            s.close();
        }
    });
}

/// Every button of the window, to the state.
fn wire(ui: &AppWindow) {
    ui.on_go(|v| with(|a| a.go(v)));
    ui.on_open_album(|id| with(|a| a.open_page(Req::Album(id.into()))));
    ui.on_open_artist(|id| with(|a| a.open_page(Req::Artist(id.into()))));
    ui.on_open_playlist(|id| with(|a| a.open_page(Req::Playlist(id.into()))));
    ui.on_play_album(|id| with(|a| a.play_fetch(Fetch::Album(id.into()))));
    ui.on_play_playlist(|id| with(|a| a.play_fetch(Fetch::Playlist(id.into()))));
    ui.on_song(|list, i, how| with(|a| a.song(list, i as usize, how)));
    ui.on_play_page(|shuffle| with(|a| a.play_page(shuffle)));
    ui.on_page_later(|| {
        with(|a| {
            let songs = a.page_songs.clone();
            if !songs.is_empty() {
                // The page's songs whole: an album so added plays as an album.
                let from = a.page_fetch.as_ref().map(Fetch::origin);
                a.on_session(|s| s.enqueue(songs, false, from));
            }
        })
    });
    ui.on_more(|| with(|a| a.more_songs()));
    ui.on_search_edited(|t| with(|a| a.search_edited(&t)));
    ui.on_toggle(|| with(App::toggle));
    ui.on_next(|| with(|a| a.on_session(|s| s.next())));
    ui.on_previous(|| {
        with(|a| {
            a.on_session(|s| {
                s.engine.previous();
            })
        })
    });
    ui.on_seek(|f| {
        with(|a| a.seek(f));
        lyrics_after_seek();
    });
    ui.on_set_volume(|v| {
        with(|a| {
            a.on_session(|s| s.set_volume(v));
            a.ui().set_volume(v);
            a.mirror();
        })
    });
    ui.on_toggle_shuffle(|| {
        with(|a| {
            let on = !a.queue.as_ref().is_some_and(|q| q.shuffle);
            a.on_session(|s| s.shuffle(on));
            a.say(if on { "Shuffle on" } else { "Shuffle off" }, false);
            a.follow();
        })
    });
    ui.on_cycle_repeat(|| {
        with(|a| {
            // Off, all, one, as the other clients go round.
            let next = match a.queue.as_ref().map_or(0, |q| q.repeat) {
                0 => 2,
                2 => 1,
                _ => 0,
            };
            a.on_session(|s| s.repeat(next));
            a.say(["Repeat off", "Repeat one", "Repeat all"][next as usize], false);
            a.follow();
        })
    });
    ui.on_lyric_tapped(|line| with(|a| a.lyric_tapped(line)));
    ui.on_jump(|i| {
        with(|a| {
            a.on_session(|s| {
                s.engine.play_at(i.max(0) as usize, 0);
            })
        })
    });
    ui.on_seek_by(|ms| {
        with(|a| {
            a.on_session(|s| {
                let at = (s.engine.status().position_now() + ms as i64).max(0);
                s.engine.seek(at);
            })
        });
        lyrics_after_seek();
    });
    ui.on_drag_window(crate::compositor::drag_window);
    ui.on_zoom_window(crate::compositor::zoom_window);
    ui.on_page_moved(|| with(|a| a.place_player()));
    ui.on_player_changed(|| {
        with(|a| {
            a.mirror();
            a.place_player();
        })
    });
    ui.on_login(|| with(App::login));
    ui.on_cancel_login(|| with(|a| a.go(HOME)));
    ui.on_find_edited(|t| {
        with(|a| {
            a.find = t.to_string();
            a.narrowed();
        })
    });
    ui.on_choose_artist(|id| with(|a| a.choose_artist(id.to_string())));
    ui.on_open_accounts(|| {
        with(|a| {
            a.ui().set_settings_tab(6);
            a.go(SETTINGS);
        })
    });
    ui.on_clear_queue(|| {
        with(|a| {
            a.on_session(|s| s.clear_upcoming());
            a.follow();
        })
    });
    ui.on_settings_tab_chosen(|t| {
        with(|a| {
            a.ui().set_settings_tab(t);
            a.settings_shown();
        })
    });
    ui.on_setting_toggled(|name, on| with(|a| a.setting(&name, if on { "true" } else { "false" })));
    ui.on_setting_chosen(|name, i| {
        with(|a| {
            let Some(v) = crate::settings::option_value(&name, i.max(0) as usize, &a.facts) else { return };
            if name == "!device" {
                // Opened at the next start, as the output is opened with the engine.
                session::own::keep(session::own::DEVICE, v.clone());
                a.facts.device = v;
                a.say("The new output is used from the next start", false);
                a.settings_shown();
            } else {
                a.setting(&name, &v);
            }
        })
    });
    ui.on_setting_action(|name| with(|a| a.setting_action(&name)));
    ui.on_setting_slid(|name, v, last| with(|a| a.slid(&name, v, last)));
    ui.on_setting_typed(|name, text| with(|a| a.setting(&name, &text)));
    ui.on_source_moved(|id, up| with(|a| a.source_moved(&id, up)));
    ui.on_accent_chosen(|i| {
        with(|a| {
            if let Some(v) = crate::settings::option_value("accent", i.max(0) as usize, &a.facts) {
                a.setting("accent", &v);
            }
        })
    });
    ui.on_eq_set(|name, value| {
        with(|a| {
            a.setting(&name, &value);
            a.tune();
        })
    });
    ui.on_eq_gain(|i, v, last| with(|a| a.eq_gain(i.max(0) as usize, v, last)));
    ui.on_eq_tool(|name, i| with(|a| a.eq_tool(&name, i.max(0) as usize)));
    ui.on_eq_sized(|w, h| {
        with(|a| {
            let ui = a.ui();
            ui.set_eq_curve_w(w);
            ui.set_eq_curve_h(h);
            if let Some(p) = settings_store::settings_current() {
                crate::eq::curve_only(&ui, &p);
            }
        })
    });
}

/// A seek lands a moment later: the lyrics' clock is asked again once the engine is there.
fn lyrics_after_seek() {
    Timer::single_shot(Duration::from_millis(120), || with(|a| a.lyrics_step(true)));
}

impl App {
    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("the window is open while its state is")
    }

    fn on_session(&self, f: impl FnOnce(&Session)) {
        if let Some(s) = &self.session {
            f(s);
        }
    }

    fn say(&self, text: &str, error: bool) {
        let ui = self.ui();
        ui.set_note(text.into());
        ui.set_note_error(error);
        let weak = self.ui.clone();
        self.note.start(TimerMode::SingleShot, Duration::from_millis(if error { 5000 } else { 2500 }), move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_note("".into());
            }
        });
    }

    // ---- the server ----

    fn open(&mut self, profile: SavedServer) {
        if let Some(s) = self.session.take() {
            s.close();
        }
        let ui = self.ui();
        ui.set_server(nori_core::settings::label(&profile.name, &profile.url).into());
        ui.set_account(profile.user.as_str().into());
        ui.set_account_initial(profile.user.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into());
        match Session::open(&self.data, self.http.clone(), profile) {
            Ok(s) => {
                s.check();
                ui.set_volume(s.volume.get());
                self.session = Some(s);
                self.heard = None;
                self.song = None;
                self.queue = None;
                self.said = Said::default();
                for i in 0..self.shelves.row_count() {
                    if let Some(mut shelf) = self.shelves.row_data(i) {
                        shelf.cards = ModelRc::default();
                        self.shelves.set_row_data(i, shelf);
                    }
                }
                self.follow();
                self.go(HOME);
                // The sidebar lists the playlists, whatever page is open.
                self.on_session(|s| s.load(Req::Playlists));
            }
            Err(e) => {
                ui.set_login_error(e.into());
                ui.set_view(LOGIN);
            }
        }
    }

    fn login(&mut self) {
        let ui = self.ui();
        let url = ui.get_login_url().trim().trim_end_matches('/').to_string();
        let user = ui.get_login_user().trim().to_string();
        if url.len() < 8 || user.is_empty() {
            ui.set_login_error("The server's address and a user name, please.".into());
            return;
        }
        let draft = SavedServer { id: nori_core::settings::new_server_id(), url, user, password: ui.get_login_password().to_string(), ..Default::default() };
        ui.set_login_busy(true);
        ui.set_login_error("".into());
        let (data, http) = (self.data.clone(), self.http.clone());
        std::thread::spawn(move || session::Tx.send(Msg::LoggedIn(session::check_login(&data, http, draft))));
    }

    // ---- pages ----

    fn go(&mut self, view: i32) {
        // A new page, listed whole.
        if !self.find.is_empty() {
            self.find.clear();
            self.ui().set_find_text("".into());
            self.narrowed();
        }
        if self.tuning && view != EQUALIZER {
            self.tuning = false;
            self.on_session(|s| s.tuning(false));
        }
        let ui = self.ui();
        ui.set_failed("".into());
        ui.set_loading(false);
        if view == LOGIN {
            ui.set_can_cancel_login(self.session.is_some());
            ui.set_login_error("".into());
            ui.set_view(LOGIN);
            return;
        }
        ui.set_view(view);
        let req = match view {
            HOME => Some(Req::Home),
            ALBUMS => Some(Req::Albums),
            ARTISTS => Some(Req::Artists),
            PLAYLISTS => Some(Req::Playlists),
            SONGS => {
                self.songs.clear();
                Some(Req::Songs { offset: 0 })
            }
            SETTINGS => {
                self.settings_shown();
                self.on_session(|s| s.facts());
                None
            }
            EQUALIZER => {
                self.settings_shown();
                None
            }
            _ => None,
        };
        if let Some(r) = req {
            self.load(r);
        }
    }

    fn load(&mut self, req: Req) {
        let Some(s) = &self.session else { return };
        self.ui().set_loading(true);
        self.want = Some(req.clone());
        s.load(req);
    }

    fn open_page(&mut self, req: Req) {
        if !self.find.is_empty() {
            self.find.clear();
            self.ui().set_find_text("".into());
            self.narrowed();
        }
        let ui = self.ui();
        self.page_songs.clear();
        self.page_fetch = Some(match &req {
            Req::Album(id) => Fetch::Album(id.clone()),
            Req::Artist(id) => Fetch::Artist(id.clone()),
            Req::Playlist(id) => Fetch::Playlist(id.clone()),
            _ => return,
        });
        ui.set_page_id(match &req {
            Req::Album(id) | Req::Artist(id) | Req::Playlist(id) => id.as_str().into(),
            _ => "".into(),
        });
        ui.set_page_kind(match &req {
            Req::Album(_) => 0,
            Req::Artist(_) => 1,
            _ => 2,
        });
        ui.set_page_title("".into());
        ui.set_page_sub("".into());
        ui.set_page_caption("".into());
        ui.set_page_art("".into());
        ui.set_page_songs(ModelRc::default());
        ui.set_page_albums(ModelRc::default());
        self.page_colours(None);
        ui.set_view(PAGE);
        ui.set_failed("".into());
        self.mirror();
        self.load(req);
    }

    /// The page's colours from its cover, or the plain page's.
    fn page_colours(&self, c: Option<&CoverColours>) {
        let ui = self.ui();
        match c {
            Some(c) => {
                ui.set_page_bg(colour(c.background));
                ui.set_page_accent(colour(c.accent));
                ui.set_page_wash(wash(c));
            }
            None => {
                ui.set_page_bg(Color::from_rgb_u8(0x1c, 0x1c, 0x1e));
                ui.set_page_accent(Color::from_rgb_u8(0xfa, 0x2d, 0x48));
                ui.set_page_wash(Image::default());
            }
        }
    }

    /// The window's backdrop from the song's cover: its wash put in the layer not shown, and the two
    /// cross-faded (app.slint animates it). The same cover again changes nothing.
    fn now_colours(&mut self, art: &str, c: Option<&CoverColours>) {
        if self.backdrop.as_deref() == Some(art) && c.is_some() {
            return;
        }
        let ui = self.ui();
        let Some(c) = c else {
            self.backdrop = None;
            return;
        };
        self.backdrop = Some(art.to_string());
        let on_b = !ui.get_wash_on_b();
        if on_b {
            ui.set_wash_b(wash(c));
        } else {
            ui.set_wash_a(wash(c));
        }
        ui.set_wash_on_b(on_b);
        ui.set_wash_tint(colour(c.background));
    }

    fn data(&mut self, req: Req, r: Result<Data, String>) {
        let ui = self.ui();
        let shown = self.want.as_ref() == Some(&req);
        if shown {
            ui.set_loading(false);
        }
        let d = match r {
            Ok(d) => d,
            Err(e) => {
                if shown {
                    ui.set_failed(e.into());
                }
                return;
            }
        };
        match d {
            Data::HomeRow(i, albums) => {
                if let Some(mut shelf) = self.shelves.row_data(i) {
                    shelf.cards = cards(albums.iter().map(album_card));
                    self.shelves.set_row_data(i, shelf);
                }
            }
            Data::Albums(v) => {
                self.lists.albums = v.iter().map(album_card).collect();
                self.narrowed();
            }
            Data::Artists(v) => {
                self.lists.artists = v.iter().map(artist_card).collect();
                self.narrowed();
                // The Artists page opens on its first artist, as Music's does.
                if ui.get_artist_chosen().is_empty() {
                    if let Some(first) = v.first() {
                        self.choose_artist(first.id.clone());
                    }
                }
            }
            Data::Playlists(v) => {
                self.lists.playlists = v
                    .iter()
                    .map(|p| Card {
                        id: p.id.as_str().into(),
                        title: p.name.as_str().into(),
                        sub: words::songs(p.song_count as usize).into(),
                        art: p.cover_art.clone().unwrap_or_default().into(),
                    })
                    .collect();
                ui.set_side_playlists(cards(self.lists.playlists.iter().cloned()));
                self.narrowed();
            }
            Data::Songs(v, exhausted) => {
                self.songs.extend(v);
                ui.set_more_songs(!exhausted);
                ui.set_songs(self.rows(&self.songs));
            }
            // A page answered: only if it is still the one open.
            Data::Album(d) if shown => {
                ui.set_page_title(d.album.name.as_str().into());
                ui.set_page_sub(d.album.artist.as_str().into());
                let mut caption = Vec::new();
                if d.album.year > 0 {
                    caption.push(d.album.year.to_string());
                }
                caption.push(words::songs_caption(d.songs.len(), d.seconds));
                if let Some(q) = d.songs.first().and_then(words::quality) {
                    caption.push(q);
                }
                ui.set_page_caption(caption.join(" · ").into());
                self.set_page_art(d.album.cover_art.clone());
                self.page_songs = d.songs;
                ui.set_page_songs(self.rows(&self.page_songs));
            }
            Data::Artist(d) if shown => {
                ui.set_page_title(d.artist.name.as_str().into());
                ui.set_page_sub("".into());
                ui.set_page_caption(words::albums(d.albums.len() as u32).into());
                self.set_page_art(d.artist.cover_art.clone().or_else(|| d.albums.first().and_then(|a| a.cover_art.clone())));
                ui.set_page_albums(cards(d.albums.iter().map(|a| Card { sub: if a.year > 0 { a.year.to_string().into() } else { "".into() }, ..album_card(a) })));
            }
            Data::Playlist(d) if shown => {
                ui.set_page_title(d.playlist.name.as_str().into());
                ui.set_page_sub(d.playlist.owner.clone().unwrap_or_default().into());
                ui.set_page_caption(words::songs_caption(d.songs.len(), d.seconds).into());
                self.set_page_art(d.playlist.cover_art.clone());
                self.page_songs = d.songs;
                ui.set_page_songs(self.rows(&self.page_songs));
            }
            Data::Album(_) | Data::Artist(_) | Data::Playlist(_) => {}
        }
    }

    /// The page's picture. Only an artist's page wears its colours (as iOS 27's do); an album or a playlist
    /// stays on the plain ground, as the Mac's Music keeps them.
    fn set_page_art(&self, art: Option<String>) {
        let art = art.unwrap_or_default();
        let ui = self.ui();
        ui.set_page_art(art.as_str().into());
        let c = ART.with(|a| a.borrow().colours.get(&art).cloned()).filter(|_| ui.get_page_kind() == 1);
        self.page_colours(c.as_deref());
    }

    /// Songs as rows, each keeping its place in `songs` (what a click plays), narrowed by the find field.
    fn rows(&self, songs: &[Song]) -> ModelRc<SongRow> {
        let heard = self.heard.as_deref();
        let find = self.find.to_lowercase();
        let hit = |s: &Song| find.is_empty() || [&s.title, &s.artist, &s.album].iter().any(|t| t.to_lowercase().contains(&find));
        ModelRc::new(VecModel::from(songs.iter().enumerate().filter(|(_, s)| hit(s)).map(|(i, s)| row(s, i, heard == Some(s.id.as_str()))).collect::<Vec<_>>()))
    }

    /// The lists the page shows, narrowed by the find field.
    fn narrowed(&self) {
        let ui = self.ui();
        let find = self.find.to_lowercase();
        let pick = |v: &[Card]| cards(v.iter().filter(|c| find.is_empty() || c.title.to_lowercase().contains(&find) || c.sub.to_lowercase().contains(&find)).cloned());
        ui.set_albums(pick(&self.lists.albums));
        ui.set_artists(pick(&self.lists.artists));
        ui.set_playlists(pick(&self.lists.playlists));
        ui.set_songs(self.rows(&self.songs));
        ui.set_page_songs(self.rows(&self.page_songs));
    }

    /// An artist chosen on the Artists page: shown beside the list, without leaving it.
    fn choose_artist(&mut self, id: String) {
        let ui = self.ui();
        ui.set_artist_chosen(id.as_str().into());
        ui.set_page_kind(1);
        ui.set_page_id(id.as_str().into());
        ui.set_page_title("".into());
        ui.set_page_caption("".into());
        ui.set_page_albums(ModelRc::default());
        self.page_songs.clear();
        self.page_fetch = Some(Fetch::Artist(id.clone()));
        self.load(Req::Artist(id));
        // The list stays where it is while the artist loads.
        ui.set_loading(false);
    }

    /// The lists' playing marks moved with the song heard.
    fn mark_playing(&self) {
        let ui = self.ui();
        ui.set_page_songs(self.rows(&self.page_songs));
        ui.set_search_songs(self.rows(&self.search_songs));
        if !self.songs.is_empty() {
            ui.set_songs(self.rows(&self.songs));
        }
    }

    fn more_songs(&mut self) {
        let offset = self.songs.len() as u32;
        self.load(Req::Songs { offset });
    }

    // ---- search ----

    fn search_edited(&mut self, text: &str) {
        let Some(s) = &self.session else { return };
        let view = s.search_typed(text);
        let query = view.query.clone();
        self.show_search(view);
        let delay = settings_store::with_prefs(|p| p.live_search_delay_ms).unwrap_or(400).max(100) as u64;
        self.search.start(TimerMode::SingleShot, Duration::from_millis(delay), move || {
            with(|a| a.on_session(|s| s.search_server(query.clone())));
        });
    }

    fn show_search(&mut self, v: SearchView) {
        let ui = self.ui();
        let found = v.shown.unwrap_or_default();
        ui.set_search_artists(cards(found.artists.iter().map(artist_card)));
        ui.set_search_albums(cards(found.albums.iter().map(album_card)));
        self.search_songs = found.songs;
        ui.set_search_songs(self.rows(&self.search_songs));
        let note = if v.searching {
            "Searching the server…".to_string()
        } else if let Some(e) = v.error {
            format!("The server could not be searched ({}); these are from the offline index", e.reason.unwrap_or_default())
        } else if v.nothing_found {
            "Nothing found".to_string()
        } else {
            String::new()
        };
        ui.set_search_note(note.into());
    }

    // ---- playing ----

    fn play_fetch(&self, what: Fetch) {
        self.on_session(|s| s.play_later(what, false));
    }

    fn song(&self, list: i32, i: usize, how: i32) {
        let (songs, origin) = match list {
            0 => (&self.page_songs, self.page_fetch.as_ref().map(Fetch::origin)),
            1 => (&self.songs, None),
            _ => (&self.search_songs, None),
        };
        let Some(one) = songs.get(i).cloned() else { return };
        self.on_session(|s| match how {
            0 => s.play(songs.clone(), i, false, origin),
            _ => s.enqueue(vec![one], how == 1, None),
        });
    }

    fn play_page(&self, shuffle: bool) {
        let Some(fetch) = self.page_fetch.clone() else { return };
        if self.page_songs.is_empty() {
            self.on_session(|s| s.play_later(fetch, shuffle));
        } else {
            let origin = fetch.origin();
            self.on_session(|s| s.play(self.page_songs.clone(), 0, shuffle, Some(origin)));
        }
    }

    fn toggle(&mut self) {
        let Some(s) = &self.session else { return };
        let st = s.engine.status();
        // Nothing loaded: the queue kept from last time starts where it was.
        if st.state == State::Idle {
            if let Some(q) = self.queue.as_ref().filter(|q| q.len > 0) {
                s.engine.play_at(q.index.max(0) as usize, st.position_now());
                return;
            }
        }
        s.engine.toggle();
    }

    fn seek(&self, fraction: f32) {
        let Some(song) = &self.song else { return };
        let ms = (fraction as f64 * song.duration as f64 * 1000.0) as i64;
        self.ui().set_position_ms(ms as i32);
        self.on_session(|s| s.engine.seek(ms));
    }

    // ---- what arrives ----

    fn take(&mut self, m: Msg) {
        match m {
            Msg::Engine(e) => {
                match &e {
                    Event::State(st) => self.said.state = Some(*st),
                    Event::Song { id, .. } | Event::Looped { id, .. } => self.said.song = Some(id.clone()),
                    Event::Buffering(b) => self.ui().set_buffering(*b),
                    Event::Error { message, .. } => self.say(&format!("Could not play: {message}"), true),
                    _ => {}
                }
                if let Some(s) = &self.session {
                    s.desktop_changed();
                    s.followed(&e);
                }
                self.follow();
            }
            Msg::Data(req, r) => {
                self.data(req, r);
                // The sidebar lists the playlists and marks the page open.
                self.mirror();
            }
            Msg::Cover { key, image, colours } => self.cover(key, &image, colours),
            Msg::Search(v) => {
                if self.ui().get_view() == SEARCH {
                    self.show_search(v);
                }
            }
            Msg::Lyrics { song, pick } => {
                if self.heard.as_deref() == Some(song.as_str()) && self.lyrics.as_ref().is_none_or(|l| l.replaced_by(&pick)) {
                    let at = self.session.as_ref().map_or(0, |s| s.engine.status().position_now());
                    let l = crate::lyrics::SongLyrics::new(pick, at);
                    let ui = self.ui();
                    ui.set_lyrics_lines(l.lines());
                    ui.set_lyrics_synced(l.synced());
                    ui.set_lyrics_note(if l.is_empty() { "No lyrics for this song".into() } else { l.credit().into() });
                    self.lyrics = Some(l);
                    self.lyrics_step(true);
                }
            }
            Msg::Facts(f) => {
                self.facts = *f;
                self.settings_shown();
            }
            Msg::Note { text, error } => self.say(&text, error),
            Msg::Reachable(Err(e)) => self.say(&e, true),
            Msg::Reachable(Ok(())) => {}
            Msg::LoggedIn(r) => {
                let ui = self.ui();
                ui.set_login_busy(false);
                match r {
                    Ok(p) => {
                        let mut prefs = settings_store::settings_current().unwrap_or_default();
                        prefs.servers.retain(|s| !(s.url == p.url && s.user == p.user));
                        prefs.servers.push(p.clone());
                        prefs.active_server_id = p.id.clone();
                        settings_store::settings_put(prefs);
                        ui.set_login_password("".into());
                        self.open(p);
                    }
                    Err(e) => ui.set_login_error(e.into()),
                }
            }
        }
    }

    /// Asks for the covers the pictures found missing.
    fn ask_covers(&mut self) {
        let wanted = ART.with(|a| std::mem::take(&mut a.borrow_mut().wanted));
        let Some(s) = &self.session else {
            // Nothing to ask with: asked again once there is.
            ART.with(|a| a.borrow_mut().asked.clear());
            return;
        };
        // The requests for pictures gone from the screen are dropped, so the loader gets to those on it.
        let now = Instant::now();
        ART.with(|a| {
            let mut a = a.borrow_mut();
            let a = &mut *a;
            self.tickets.retain(|(k, _)| {
                let shown = a.missing.get(k).is_some_and(|t| now.duration_since(*t) < GONE);
                if !shown {
                    a.asked.remove(k);
                    a.missing.remove(k);
                }
                shown
            });
        });
        // The loader serves the newest first: a frame's pictures are asked for last to first, so the top
        // left comes first.
        for (id, size) in wanted.into_iter().rev() {
            let k = key(&id, size);
            // The large ones are the pages' own pictures: their colours are worked out with them.
            let t = s.cover(&id, k.clone(), [SMALL_PX, LARGE_PX, HERO_PX][size as usize], size > 0);
            self.tickets.push_back((k, t));
        }
        // A ticket dropped cancels its cover: one that never came may be asked for again.
        while self.tickets.len() > PENDING_KEPT {
            let (k, _) = self.tickets.pop_front().expect("longer than the limit");
            ART.with(|a| {
                let mut a = a.borrow_mut();
                a.asked.remove(&k);
                a.missing.remove(&k);
            });
        }
    }

    fn cover(&mut self, key: String, image: &Picture, colours: Option<Box<CoverColours>>) {
        let id = key[2..].to_string();
        let large = !key.starts_with('s');
        self.tickets.retain(|(k, _)| *k != key);
        ART.with(|a| {
            let mut a = a.borrow_mut();
            let now = Instant::now();
            a.missing.remove(&key);
            a.images.insert(key.clone(), (picture(image), now));
            if let Some(c) = colours {
                a.colours.insert(id.clone(), Rc::from(c));
            }
            let limit = if large { LARGE_KEPT } else { SMALL_KEPT };
            let class = |k: &str| k.starts_with('s') != large;
            let mut count = a.images.keys().filter(|k| class(k)).count();
            while count > limit {
                let oldest = a
                    .images
                    .iter()
                    .filter(|(k, (_, drawn))| class(k) && now.duration_since(*drawn) > IN_USE)
                    .min_by_key(|(_, (_, drawn))| *drawn)
                    .map(|(k, _)| k.clone());
                let Some(old) = oldest else { break };
                a.images.remove(&old);
                a.asked.remove(&old);
                if large {
                    a.colours.remove(&old[2..]);
                }
                count -= 1;
            }
        });
        let ui = self.ui();
        ui.set_covers_rev(ui.get_covers_rev().wrapping_add(1));
        if let Some(p) = &self.player {
            p.set_covers_rev(ui.get_covers_rev());
        }
        if let Some(sd) = &self.sidebar {
            sd.set_covers_rev(ui.get_covers_rev());
        }
        if large {
            let c = ART.with(|a| a.borrow().colours.get(&id).cloned());
            if ui.get_now_art() == id.as_str() {
                self.now_colours(&id, c.as_deref());
            }
            if ui.get_view() == PAGE && ui.get_page_kind() == 1 && ui.get_page_art() == id.as_str() {
                self.page_colours(c.as_deref());
            }
        }
    }

    /// What the player window shows, copied from the main window, where it is kept.
    fn mirror(&self) {
        let ui = self.ui();
        if let Some(p) = &self.player {
        p.set_has_song(ui.get_has_song());
        p.set_now_title(ui.get_now_title());
        p.set_now_artist(ui.get_now_artist());
        p.set_now_album(ui.get_now_album());
        p.set_now_art(ui.get_now_art());
        p.set_playing(ui.get_playing());
        p.set_position_ms(ui.get_position_ms());
        p.set_duration_ms(ui.get_duration_ms());
        p.set_shuffle(ui.get_shuffle());
        p.set_repeat(ui.get_repeat());
        p.set_volume(ui.get_volume());
        p.set_inspector(ui.get_inspector());
        p.set_covers_rev(ui.get_covers_rev());
        }
        if let Some(sd) = &self.sidebar {
            sd.set_view(ui.get_view());
            sd.set_page_kind(ui.get_page_kind());
            sd.set_page_id(ui.get_page_id());
            sd.set_playlists(ui.get_playlists());
            sd.set_side_playlists(ui.get_side_playlists());
            sd.set_server(ui.get_server());
            sd.set_account(ui.get_account());
            sd.set_account_initial(ui.get_account_initial());
            sd.set_covers_rev(ui.get_covers_rev());
            sd.set_inset_top(ui.get_inset_top());
        }
    }

    /// The player window over the page's bottom; out of sight over Now Playing and the sign-in page.
    fn place_player(&self) {
        let ui = self.ui();
        let shown = !ui.get_full_player() && ui.get_view() != LOGIN;
        crate::compositor::set_right(if ui.get_inspector() != 0 { 280.0 } else { 0.0 });
        crate::compositor::show_glass(shown && self.sidebar.is_some(), shown && self.player.is_some());
    }

    /// The lyrics' clock asked where the music is: the line lit, how far its words are sung, and when to
    /// look again.
    fn lyrics_step(&mut self, force: bool) {
        let Some(l) = &self.lyrics else { return };
        let Some(s) = &self.session else { return };
        let (at, playing) = s.engine.status_with(|st| (st.position_now(), st.state == State::Playing));
        let now = l.advance(at, force);
        let ui = self.ui();
        ui.set_lyrics_active(now.active);
        ui.set_lyric_sweeping(now.sweeping);
        // The lit line in pieces, each word or syllable rising as it is sung (sung.rs), where it is shown.
        let full = ui.get_full_player() && ui.get_full_panel() == 2;
        let side = ui.get_inspector() == 2;
        let width = ui.window().size().to_logical(ui.window().scale_factor()).width;
        let pieces = |view, on: bool, size, w, lit, dim| if on && now.sweeping { l.pieces(view, &now, size, w, lit, dim) } else { Vec::new() };
        renew(&self.pieces[0], pieces(0, side, 22.0, 280.0 - 44.0, 0.92, 0.26));
        renew(&self.pieces[1], pieces(1, full, 44.0, width / 2.0 - 140.0, 1.0, 0.36));
        ui.set_lyric_sung(now.sung.into());
        ui.set_lyric_now(now.now.into());
        ui.set_lyric_mix(now.mix);
        ui.set_lyric_rest(now.rest.into());
        match now.wait.filter(|_| playing) {
            Some(ms) => self.lyrics_timer.start(TimerMode::SingleShot, Duration::from_millis(ms), || with(|a| a.lyrics_step(false))),
            None => self.lyrics_timer.stop(),
        }
    }

    /// A lyrics line clicked: the song goes to where it is sung.
    fn lyric_tapped(&mut self, line: i32) {
        let (Some(l), Ok(line)) = (&self.lyrics, usize::try_from(line)) else { return };
        let ms = l.tap(line);
        self.on_session(|s| s.engine.seek(ms));
        self.lyrics_step(true);
        lyrics_after_seek();
    }

    /// The queue's rows become `rows`: the ones leaving fold away first, then the list settles, the new
    /// ones opening in their places.
    fn queue_shown(&mut self, rows: Vec<SongRow>) {
        if let Some(next) = self.queue_next.take() {
            settle(&self.queue_rows, next);
        }
        self.queue_timer.stop();
        let m = &self.queue_rows;
        let keep: HashSet<i32> = rows.iter().map(|r| r.index).collect();
        let gone: Vec<usize> = (0..m.row_count()).filter(|&i| m.row_data(i).is_some_and(|r| !keep.contains(&r.index))).collect();
        if gone.is_empty() || gone.len() == m.row_count() {
            settle(m, rows);
            return;
        }
        for i in gone {
            if let Some(mut r) = m.row_data(i) {
                r.leaving = true;
                m.set_row_data(i, r);
            }
        }
        self.queue_next = Some(rows);
        self.queue_timer.start(TimerMode::SingleShot, Duration::from_millis(QUEUE_FOLD_MS), || {
            with(|a| {
                if let Some(next) = a.queue_next.take() {
                    settle(&a.queue_rows, next);
                }
            })
        });
    }

    /// The settings page drawn again from the settings as they are now.
    fn settings_shown(&self) {
        let ui = self.ui();
        let prefs = settings_store::settings_current().unwrap_or_default();
        ui.set_autoplay(prefs.auto_fill);
        ui.set_automix(prefs.auto_mix);
        ui.global::<crate::Theme>().set_accent(slint::Color::from_argb_encoded(crate::settings::accent_shown(prefs.accent as u32)));
        let view = ui.get_view();
        if view == SETTINGS {
            ui.set_settings(crate::settings::rows(&prefs, &self.facts, ui.get_settings_tab()));
        }
        if view == EQUALIZER {
            crate::eq::fill(&ui, &prefs);
        }
    }

    fn setting(&mut self, name: &str, value: &str) {
        let Some(s) = &self.session else { return };
        if s.setting(name, value).is_none() {
            self.say(&format!("{name}: not a setting"), true);
        }
        self.settings_shown();
    }

    /// An equalizer edit made: while the page is open, the engine answers at once (its shallow buffer).
    fn tune(&mut self) {
        if !self.tuning && self.ui().get_view() == EQUALIZER {
            self.tuning = true;
            self.on_session(|s| s.tuning(true));
        }
    }

    /// A slider moved: a level edited in place; the page drawn again once it is let go.
    fn slid(&mut self, name: &str, v: f32, last: bool) {
        let Some(level) = crate::settings::level_of(name) else { return };
        if let Some((effect, _)) = settings_store::edit_level(level, v) {
            self.on_session(|s| s.applied(effect));
            self.tune();
        }
        if last {
            self.settings_shown();
        }
    }

    /// A band of the equalizer moved: its curve follows at once, the page once it is let go.
    fn eq_gain(&mut self, i: usize, v: f32, last: bool) {
        let Some(p) = settings_store::settings_current() else { return };
        let effect = if p.eq_mode == nori_core::settings::EqMode::Graphic {
            settings_store::edit_graphic(i as u32, v).map(|e| e.0)
        } else {
            p.eq_bands.get(i).and_then(|b| settings_store::edit_band(i as u32, nori_core::settings::SoundBand { gain_db: v, ..*b }).map(|e| e.0))
        };
        if let Some(effect) = effect {
            self.on_session(|s| s.applied(effect));
            self.tune();
        }
        if last {
            self.settings_shown();
        } else if let Some(p) = settings_store::settings_current() {
            crate::eq::curve_only(&self.ui(), &p);
        }
    }

    fn eq_tool(&mut self, name: &str, i: usize) {
        use nori_core::settings_store::SoundTool;
        let tool = match name {
            "preset" => nori_core::dsp::eq_presets().get(i).cloned().map(|preset| SoundTool::Preset { preset }),
            "auto-preamp" => Some(SoundTool::AutoPreamp { automatic: true }),
            "manual-preamp" => Some(SoundTool::AutoPreamp { automatic: false }),
            "add-band" => Some(SoundTool::AddBand),
            "remove-band" => Some(SoundTool::RemoveBand { index: i as u32 }),
            "reset" => Some(SoundTool::ResetBands),
            _ => None,
        };
        let Some(tool) = tool else { return };
        match settings_store::settings_sound_tool(tool) {
            Ok(Some(change)) => {
                self.on_session(|s| s.applied(change.effect));
                self.tune();
            }
            Ok(None) => {}
            Err(e) => self.say(&format!("{e:?}"), true),
        }
        self.settings_shown();
    }

    /// A lyrics source moved a place up or down its list.
    fn source_moved(&mut self, id: &str, up: bool) {
        let Some(p) = settings_store::settings_current() else { return };
        let s = nori_core::settings_model::state(&p, nori_core::settings_model::Output::default());
        let Some(at) = s.lyrics_sources.iter().position(|x| x.id == id) else { return };
        let to = if up { at.saturating_sub(1) } else { (at + 1).min(s.lyrics_sources.len() - 1) };
        if to != at {
            self.setting("lyricsPlace", &format!("{id}:{to}"));
        }
    }

    fn setting_action(&mut self, name: &str) {
        if name == "equalizer" {
            self.go(EQUALIZER);
        } else if name == "add-server" {
            self.go(LOGIN);
        } else if let Some(id) = name.strip_prefix("server:") {
            let mut prefs = settings_store::settings_current().unwrap_or_default();
            let Some(p) = prefs.servers.iter().find(|s| s.id == id).cloned() else { return };
            prefs.active_server_id = id.to_string();
            settings_store::settings_put(prefs);
            self.open(p);
        } else {
            self.on_session(|s| s.action(name));
            // What was cleared or measured shows as it is now.
            self.on_session(|s| s.facts());
        }
    }

    /// What the window shows of the engine and the queue.
    fn follow(&mut self) {
        let Some(s) = &self.session else { return };
        let st = s.engine.status();
        let said = &self.said;
        let song_said = said.song.as_deref().filter(|x| st.id.as_deref() != Some(*x));
        let agrees = said.state.is_none_or(|x| x == st.state) && song_said.is_none();
        let (state, id) = if agrees { (st.state, st.id.clone()) } else { (said.state.unwrap_or(st.state), song_said.map(String::from).or(st.id.clone())) };
        if agrees {
            self.said = Said::default();
        } else if self.said.tries < 50 {
            // The status is behind the events: looked at again in a moment.
            self.said.tries += 1;
            self.again.start(TimerMode::SingleShot, Duration::from_millis(20), || with(App::follow));
        } else {
            self.said = Said::default();
        }
        let ui = self.ui();
        let playing = state == State::Playing;
        ui.set_playing(playing);
        if agrees {
            ui.set_position_ms(st.position_now() as i32);
        }
        if id != self.heard {
            self.heard = id.clone();
            self.song = id.and_then(nori_core::queue::queue_song);
            if !agrees {
                ui.set_position_ms(0);
            }
            let song = self.song.clone().unwrap_or_default();
            ui.set_has_song(self.song.is_some());
            ui.set_now_title(song.title.as_str().into());
            ui.set_now_artist(song.artist.as_str().into());
            ui.set_now_album(song.album.as_str().into());
            ui.set_duration_ms((song.duration as i64 * 1000) as i32);
            let art = song.cover_art.clone().unwrap_or_default();
            ui.set_now_art(art.as_str().into());
            let c = ART.with(|a| a.borrow().colours.get(&art).cloned());
            self.now_colours(&art, c.as_deref());
            // The large cover (and with it the backdrop's colours) is asked for even with the panel shut.
            if c.is_none() {
                let _ = self::art(art.as_str().into(), 1);
            }
            self.mark_playing();
            self.lyrics = None;
            let ui = self.ui();
            ui.set_lyrics_lines(ModelRc::default());
            ui.set_lyrics_active(-1);
            ui.set_lyrics_note(if self.song.is_some() { "Looking for lyrics…".into() } else { "".into() });
            if let (Some(s), Some(id)) = (&self.session, &self.heard) {
                s.lyrics(id.clone());
            }
        }
        // The queue, copied again only when it changed.
        let (rev, repeat, index) = nori_core::playlist::with(|p| (p.rev(), p.repeat(), p.current().map_or(-1, |c| c as i32)));
        if self.queue.as_ref().is_none_or(|q| q.rev != rev || q.repeat != repeat || q.index != index) {
            let held = self.queue.as_ref().map_or(u64::MAX, |q| q.list_rev);
            let mut v = nori_core::playlist::playlist_view(held);
            if v.songs.is_empty() && v.len > 0 {
                if let Some(q) = &self.queue {
                    v.songs = q.songs.clone();
                }
            }
            ui.set_shuffle(v.shuffle);
            ui.set_repeat(v.repeat as i32);
            self.queue_shown(queue_rows(&v));
            ui.set_queue_from(queue_from(&v).into());
            self.queue = Some(v);
        }
        // The seek bar's clock: running only while music plays, stepping as often as the widest bar moves a
        // pixel (a long song seldom, a short one often), so the bar glides without redrawing every frame.
        let step = pixel_ms(ui.get_duration_ms() as i64);
        if playing && (!self.tick.running() || self.tick_ms != step) {
            self.tick_ms = step;
            let weak = self.ui.clone();
            self.tick.start(TimerMode::Repeated, Duration::from_millis(step), move || {
                let Some(ui) = weak.upgrade() else { return };
                with(|a| {
                    a.on_session(|s| ui.set_position_ms(s.engine.status().position_now() as i32));
                    if let Some(p) = &a.player {
                        p.set_position_ms(ui.get_position_ms());
                    }
                });
            });
        } else if !playing {
            self.tick.stop();
        }
        self.lyrics_step(true);
        self.mirror();
    }
}

/// How often the seek bar steps: once a pixel of the widest bar (Now Playing's, about 380 points on a
/// 2x screen), at most once a frame and at least every quarter second.
fn pixel_ms(duration_ms: i64) -> u64 {
    (duration_ms.max(1) as u64 / 760).clamp(16, 250)
}

fn cards(it: impl Iterator<Item = Card>) -> ModelRc<Card> {
    ModelRc::new(VecModel::from(it.collect::<Vec<_>>()))
}

fn album_card(a: &nori_core::Album) -> Card {
    Card { id: a.id.as_str().into(), title: a.name.as_str().into(), sub: a.artist.as_str().into(), art: a.cover_art.clone().unwrap_or_default().into() }
}

fn artist_card(a: &nori_core::Artist) -> Card {
    Card { id: a.id.as_str().into(), title: a.name.as_str().into(), sub: words::albums(a.album_count).into(), art: a.cover_art.clone().unwrap_or_default().into() }
}

fn row(s: &Song, index: usize, playing: bool) -> SongRow {
    SongRow {
        title: s.title.as_str().into(),
        artist: s.artist.as_str().into(),
        album: s.album.as_str().into(),
        time: words::duration(s.duration as i64).into(),
        art: s.cover_art.clone().unwrap_or_default().into(),
        index: index as i32,
        playing,
        leaving: false,
        fresh: false,
    }
}

/// The queue in the order it plays, from the song playing on; each row jumps to its list index.
/// What plays after the song playing, in the order it plays; each row jumps to its list index.
fn queue_rows(v: &PlaylistView) -> Vec<SongRow> {
    let from = v.order.iter().position(|&i| i as i32 == v.index).map_or(0, |p| p + 1);
    v.order[from.min(v.order.len())..].iter().filter_map(|&i| v.songs.get(i as usize).map(|s| row(s, i as usize, false))).collect()
}

/// `m` made `rows`, row by row where it has as many (the pieces of a line move frame to frame; their texts
/// stay).
fn renew<T: Clone + PartialEq + 'static>(m: &VecModel<T>, rows: Vec<T>) {
    if m.row_count() != rows.len() {
        m.set_vec(rows);
        return;
    }
    for (i, r) in rows.into_iter().enumerate() {
        if m.row_data(i).as_ref() != Some(&r) {
            m.set_row_data(i, r);
        }
    }
}

/// How long a row leaving the queue takes to fold away (app.slint's QueueView).
const QUEUE_FOLD_MS: u64 = 300;

/// The queue's rows made `rows` in place: those not in it taken out, the new ones put in where they go
/// (marked fresh, so they open), the rest kept as they are. Rows that changed order are drawn again.
fn settle(m: &VecModel<SongRow>, rows: Vec<SongRow>) {
    let keep: HashSet<i32> = rows.iter().map(|r| r.index).collect();
    for i in (0..m.row_count()).rev() {
        if m.row_data(i).is_some_and(|r| !keep.contains(&r.index)) {
            m.remove(i);
        }
    }
    let had: HashSet<i32> = m.iter().map(|r| r.index).collect();
    if had.is_empty() {
        m.set_vec(rows);
        return;
    }
    let order: Vec<i32> = rows.iter().map(|r| r.index).filter(|i| had.contains(i)).collect();
    if order != m.iter().map(|r| r.index).collect::<Vec<_>>() {
        m.set_vec(rows);
        return;
    }
    let opening = !had.is_empty();
    for (j, mut r) in rows.into_iter().enumerate() {
        match m.row_data(j) {
            Some(old) if old.index == r.index => {
                r.fresh = old.fresh;
                if old != r {
                    m.set_row_data(j, r);
                }
            }
            _ => {
                r.fresh = opening;
                m.insert(j, r);
            }
        }
    }
}

/// Where the songs coming up are from, when they are all of one album.
fn queue_from(v: &PlaylistView) -> String {
    let from = v.order.iter().position(|&i| i as i32 == v.index).map_or(0, |p| p + 1);
    let mut albums = v.order[from.min(v.order.len())..].iter().filter_map(|&i| v.songs.get(i as usize)).map(|s| s.album.as_str());
    match albums.next() {
        Some(first) if !first.is_empty() && albums.all(|a| a == first) => first.to_string(),
        _ => String::new(),
    }
}
