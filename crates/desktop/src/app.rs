//! The window's state and what its buttons do. Lives on Slint's event loop thread: the window's callbacks,
//! the engine's events and the workers' answers (`Msg`, through `session::Tx`) all arrive here, one at a
//! time. The only timer is the seek bar's, and it runs only while music plays.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

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
use crate::{AppWindow, Card, PlayerBar, Shelf, SongRow};

/// Pixels a side: a card's cover, and the large one (now playing, a page's), whose colours are worked out too.
const SMALL_PX: u32 = 256;
const LARGE_PX: u32 = 800;
/// An artist's picture across the page's whole width.
const HERO_PX: u32 = 1600;
/// Decoded covers kept: the cards on a few screens, and the few large ones.
const SMALL_KEPT: usize = 240;
const LARGE_KEPT: usize = 12;

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

/// The decoded covers, looked up by the pictures as they draw (`art` in app.slint). A picture not here yet
/// is asked for once; its arrival bumps `covers-rev`, and every picture looks again.
#[derive(Default)]
struct Art {
    images: HashMap<String, Image>,
    small: VecDeque<String>,
    large: VecDeque<String>,
    /// The page colours of the large covers, by cover id.
    colours: HashMap<String, Rc<CoverColours>>,
    asked: HashSet<String>,
    wanted: Vec<(String, i32)>,
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
        if let Some(i) = a.images.get(&k) {
            return i.clone();
        }
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
    /// The cover whose wash is the window's backdrop.
    backdrop: Option<String>,
    tickets: VecDeque<(String, Ticket)>,
    tick: Timer,
    tick_ms: u64,
    again: Timer,
    /// The lyrics of the song heard, and their clock; when the next line is due.
    lyrics: Option<crate::lyrics::SongLyrics>,
    lyrics_due: Option<std::time::Instant>,
    note: Timer,
    search: Timer,
}

/// The player window, on macOS: its buttons do what the main window's do, and it is put over the page once
/// both windows exist.
fn player(ui: &AppWindow) -> Option<PlayerBar> {
    if !cfg!(target_os = "macos") {
        return None;
    }
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
    if let Err(e) = bar.show() {
        eprintln!("nori: no player window: {e}");
        ui.set_native_player(false);
        return None;
    }
    crate::glass::attach_player(ui.as_weak(), bar.as_weak());
    Some(bar)
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
        player: player(ui),
        tickets: VecDeque::new(),
        tick: Timer::default(),
        tick_ms: 0,
        again: Timer::default(),
        lyrics: None,
        lyrics_due: None,
        note: Timer::default(),
        search: Timer::default(),
    };
    APP.with(|a| *a.borrow_mut() = Some(app));
    let prefs = settings_store::settings_current().unwrap_or_default();
    match prefs.servers.iter().find(|s| s.id == prefs.active_server_id).cloned() {
        Some(p) => with(|app| app.open(p)),
        None => ui.set_view(LOGIN),
    }
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
    let weak = ui.as_weak();
    ui.on_drag_window(move || crate::glass::drag(&weak));
    let weak = ui.as_weak();
    ui.on_zoom_window(move || crate::glass::zoom(&weak));
    ui.on_page_moved(|| with(|a| a.place_player()));
    ui.on_player_changed(|| {
        with(|a| {
            a.mirror();
            a.place_player();
        })
    });
    ui.on_login(|| with(App::login));
    ui.on_cancel_login(|| with(|a| a.go(HOME)));
    ui.on_setting_toggled(|name, on| with(|a| a.setting(&name, if on { "true" } else { "false" })));
    ui.on_setting_chosen(|name, i| {
        with(|a| {
            if let Some(v) = crate::settings::option_value(&name, i.max(0) as usize) {
                a.setting(&name, &v);
            }
        })
    });
    ui.on_setting_action(|name| {
        with(|a| {
            if name == "servers" {
                a.go(LOGIN);
            } else {
                a.on_session(|s| s.action(&name));
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
            Data::Albums(v) => ui.set_albums(cards(v.iter().map(album_card))),
            Data::Artists(v) => ui.set_artists(cards(v.iter().map(artist_card))),
            Data::Playlists(v) => ui.set_playlists(cards(v.iter().map(|p| Card {
                id: p.id.as_str().into(),
                title: p.name.as_str().into(),
                sub: words::songs(p.song_count as usize).into(),
                art: p.cover_art.clone().unwrap_or_default().into(),
            }))),
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

    fn rows(&self, songs: &[Song]) -> ModelRc<SongRow> {
        let heard = self.heard.as_deref();
        ModelRc::new(VecModel::from(songs.iter().enumerate().map(|(i, s)| row(s, i, heard == Some(s.id.as_str()))).collect::<Vec<_>>()))
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
            _ => s.enqueue(vec![one], how == 1),
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
            Msg::Data(req, r) => self.data(req, r),
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
        for (id, size) in wanted {
            let k = key(&id, size);
            // The large ones are the pages' own pictures: their colours are worked out with them.
            let t = s.cover(&id, k.clone(), [SMALL_PX, LARGE_PX, HERO_PX][size as usize], size > 0);
            self.tickets.push_back((k, t));
        }
        // A ticket dropped cancels its cover: one that never came may be asked for again.
        while self.tickets.len() > SMALL_KEPT + LARGE_KEPT {
            let (k, _) = self.tickets.pop_front().expect("longer than the limit");
            ART.with(|a| {
                let mut a = a.borrow_mut();
                if !a.images.contains_key(&k) {
                    a.asked.remove(&k);
                }
            });
        }
    }

    fn cover(&mut self, key: String, image: &Picture, colours: Option<Box<CoverColours>>) {
        let id = key[2..].to_string();
        let large = !key.starts_with('s');
        ART.with(|a| {
            let mut a = a.borrow_mut();
            let kept = if large { &mut a.large } else { &mut a.small };
            kept.push_back(key.clone());
            let limit = if large { LARGE_KEPT } else { SMALL_KEPT };
            let old = if kept.len() > limit { kept.pop_front() } else { None };
            if let Some(old) = old {
                a.images.remove(&old);
                a.asked.remove(&old);
                if large {
                    a.colours.remove(&old[2..]);
                }
            }
            a.images.insert(key, picture(image));
            if let Some(c) = colours {
                a.colours.insert(id.clone(), Rc::from(c));
            }
        });
        let ui = self.ui();
        ui.set_covers_rev(ui.get_covers_rev().wrapping_add(1));
        if let Some(p) = &self.player {
            p.set_covers_rev(ui.get_covers_rev());
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
        let Some(p) = &self.player else { return };
        let ui = self.ui();
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

    /// The player window over the page's bottom; out of sight over Now Playing and the sign-in page.
    fn place_player(&self) {
        let Some(p) = &self.player else { return };
        let ui = self.ui();
        crate::glass::place_player(&ui, p, !ui.get_full_player() && ui.get_view() != LOGIN);
    }

    /// The lyrics' clock asked where the music is: the line lit, and when to look again.
    fn lyrics_step(&mut self, force: bool) {
        let Some(l) = &self.lyrics else { return };
        let Some(s) = &self.session else { return };
        let (at, playing) = s.engine.status_with(|st| (st.position_now(), st.state == State::Playing));
        let (active, wait) = l.advance(at, force);
        self.ui().set_lyrics_active(active);
        self.lyrics_due = wait.filter(|_| playing).map(|ms| std::time::Instant::now() + Duration::from_millis(ms));
    }

    /// On the seek bar's clock: the next line, when it is due.
    fn lyrics_due(&mut self) {
        if self.lyrics_due.is_some_and(|t| t <= std::time::Instant::now()) {
            self.lyrics_step(false);
        }
    }

    /// The settings page drawn again from the settings as they are now.
    fn settings_shown(&self) {
        let ui = self.ui();
        let prefs = settings_store::settings_current().unwrap_or_default();
        ui.set_settings(crate::settings::rows(&prefs, &ui.get_server()));
    }

    fn setting(&mut self, name: &str, value: &str) {
        let Some(s) = &self.session else { return };
        if s.setting(name, value).is_none() {
            self.say(&format!("{name}: not a setting"), true);
        }
        self.settings_shown();
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
            ui.set_queue(queue_rows(&v));
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
                    a.lyrics_due();
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
    }
}

/// The queue in the order it plays, from the song playing on; each row jumps to its list index.
fn queue_rows(v: &PlaylistView) -> ModelRc<SongRow> {
    let from = v.order.iter().position(|&i| i as i32 == v.index).unwrap_or(0);
    let rows: Vec<SongRow> = v.order[from..].iter().filter_map(|&i| v.songs.get(i as usize).map(|s| row(s, i as usize, i as i32 == v.index))).collect();
    ModelRc::new(VecModel::from(rows))
}
