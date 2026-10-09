//! Window state and UI callbacks, on Slint's event loop thread. Worker results arrive as [`Msg`]s through
//! the inbox ([`session::Tx`]). The only repeating timer is the seek bar's, and only while playing.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use nori_core::playlist::PlaylistView;
use nori_core::remote::wire::DeviceState;
use nori_core::search::SearchView;
use nori_core::settings::SavedServer;
use nori_core::Song;
use nori_covers::loader::Ticket;
use nori_covers::memory::Image as Picture;
use nori_engine::{Event, State};
use nori_host::remote::Elsewhere;
use nori_http::Http;
use nori_look::cover::CoverColours;
use skia_safe::Typeface;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel};

use crate::compositor::{Compositor, Focus};
use crate::session::{self, CoverKey, CoverSize, Data, Fetch, Msg, Req, Session, Tx};
use crate::settings::{Act, Target};
use crate::words;
use crate::{AppWindow, ArtistLink, Card, Credits, LyricPiece, Pick, PlayerBar, Shelf, SidebarWindow, SongGo, SongRow};

/// Cover fetch sizes, px square.
const SMALL_PX: u32 = 256;
const LARGE_PX: u32 = 800;
const HERO_PX: u32 = 1600;
/// Decoded covers kept per class. Covers drawn within `IN_USE` are never evicted.
const SMALL_KEPT: usize = 240;
const LARGE_KEPT: usize = 12;
const IN_USE: Duration = Duration::from_secs(3);
/// Pending cover requests kept; the oldest beyond this are cancelled.
const PENDING_KEPT: usize = 1000;
/// A pending cover not looked up for this long is off screen, and its request is cancelled.
const GONE: Duration = Duration::from_millis(1500);
/// How long a row leaving the queue takes to fold away (app.slint's QueueView).
const QUEUE_FOLD_MS: u64 = 300;
/// How long the undo of a song taken out of the queue is offered.
const UNDO_MS: u64 = 5_000;

// View indices, as app.slint numbers them.
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

/// Weak handle to the app for callbacks and timers.
#[derive(Clone)]
struct AppHandle(Weak<RefCell<App>>);

impl AppHandle {
    /// Runs `f` on the app; dropped if the app is gone or already borrowed (a re-entrant callback).
    fn with(&self, f: impl FnOnce(&mut App)) {
        let Some(app) = self.0.upgrade() else { return };
        match app.try_borrow_mut() {
            Ok(mut a) => f(&mut a),
            Err(_) => eprintln!("nori: a callback came while the window's state was busy; dropped"),
        };
    }
}

/// Wires a Slint callback to an [`App`] method: `on!(ui.on_go, h, |a, v| a.go(v))`.
macro_rules! on {
    ($ui:ident.$setter:ident, $h:expr, |$a:ident $(, $arg:ident)*| $body:expr) => {{
        let h = $h.clone();
        $ui.$setter(move |$($arg),*| h.with(|$a| $body));
    }};
}

/// Library lists as loaded, before the find filter.
#[derive(Default)]
struct Lists {
    albums: Vec<Card>,
    artists: Vec<Card>,
    playlists: Vec<Card>,
}

/// Decoded covers, looked up by the `art` callback while drawing. A missing cover is requested once;
/// its arrival bumps `covers-rev` so pictures look again. Least recently drawn are evicted first.
#[derive(Default)]
struct Art {
    images: HashMap<CoverKey, (Image, Instant)>,
    /// Page colours of large covers, by cover id.
    colours: HashMap<String, Rc<CoverColours>>,
    asked: HashSet<CoverKey>,
    wanted: Vec<CoverKey>,
    /// When a pending cover was last looked up; stale ones are off screen.
    missing: HashMap<CoverKey, Instant>,
}

/// app.slint's size code: 0 card, 1 large, 2 hero.
fn cover_size(size: i32) -> CoverSize {
    match size.clamp(0, 2) {
        0 => CoverSize::Card,
        1 => CoverSize::Large,
        _ => CoverSize::Hero,
    }
}

fn cover_px(size: CoverSize) -> u32 {
    match size {
        CoverSize::Card => SMALL_PX,
        CoverSize::Large => LARGE_PX,
        CoverSize::Hero => HERO_PX,
    }
}

/// The `art` callback: the cover if decoded, else an empty image and a queued request.
fn cover_image(art: &RefCell<Art>, app: &AppHandle, id: SharedString, size: i32) -> Image {
    if id.is_empty() {
        return Image::default();
    }
    let k = CoverKey { id: id.to_string(), size: cover_size(size) };
    let mut a = art.borrow_mut();
    if let Some((i, drawn)) = a.images.get_mut(&k) {
        *drawn = Instant::now();
        return i.clone();
    }
    a.missing.insert(k.clone(), Instant::now());
    if a.asked.insert(k.clone()) {
        if a.wanted.is_empty() {
            // Requested after this frame is drawn, not from inside it.
            let app = app.clone();
            Timer::single_shot(Duration::ZERO, move || app.with(App::ask_covers));
        }
        a.wanted.push(k);
    }
    Image::default()
}

fn picture(p: &Picture) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&p.pixels, p.width, p.height))
}

/// The cover's blurred wash (square ARGB) as an image.
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

pub struct App {
    me: AppHandle,
    ui: slint::Weak<AppWindow>,
    compositor: Compositor,
    tx: Tx,
    inbox: mpsc::Receiver<Msg>,
    /// Media controls for the process; each session drives them while open.
    mpris: Option<Arc<nori_mpris::Mpris>>,
    data: PathBuf,
    http: Arc<Http>,
    session: Option<Session>,
    art: Rc<RefCell<Art>>,
    lyric_face: Option<Typeface>,
    /// Id and record of the song being heard.
    heard: Option<String>,
    song: Option<Song>,
    queue: Option<PlaylistView>,
    /// Songs behind the clickable lists: the page's, the Songs view's, the search's.
    page_songs: Vec<Song>,
    page_fetch: Option<Fetch>,
    songs: Vec<Song>,
    search_songs: Vec<Song>,
    /// The read the shown page waits for.
    want: Option<Req>,
    shelves: Rc<VecModel<Shelf>>,
    /// Separate player and sidebar windows drawn as glass (macOS); None where the page draws them.
    player: Option<PlayerBar>,
    sidebar: Option<SidebarWindow>,
    /// Cover id whose wash is the window backdrop.
    backdrop: Option<String>,
    tickets: VecDeque<(CoverKey, Ticket)>,
    tick: Timer,
    tick_ms: u64,
    lists: Lists,
    find: String,
    facts: crate::settings::Facts,
    /// The engine is in its shallow equalizer-tuning buffer.
    tuning: bool,
    lyrics: Option<crate::lyrics::SongLyrics>,
    lyrics_timer: Timer,
    /// Where a seek sent the music, until the engine says it landed: its status still holds the old place.
    seeking: Option<i64>,
    /// Active lyric line pieces in the side panel and Now Playing, updated in place each frame.
    pieces: [Rc<VecModel<LyricPiece>>; 2],
    /// Queue rows, edited in place; leaving rows fold first, then the list settles to `queue_next`.
    queue_rows: Rc<VecModel<SongRow>>,
    queue_next: Option<Vec<SongRow>>,
    queue_timer: Timer,
    /// The song last taken out of the queue, while its undo is offered.
    removed: Option<String>,
    undo_timer: Timer,
    note: Timer,
    search: Timer,
    /// The hosted jam's invite link its QR code was drawn for, and whether a jam is hosted.
    jam_link: String,
    jam_hosting: bool,
    /// The device playing while it is another one: the player shows and controls it.
    elsewhere: Option<Elsewhere>,
    /// A jam guest's: the host's playback as last published, and when it was right.
    jam_now: Option<(DeviceState, Instant)>,
    /// A jam guest's controls by its role: what the player offers, and whether it paused here.
    jam_controls: Option<nori_core::remote::JamControls>,
    /// The songs this guest asked for that wait for the host.
    asked: HashSet<String>,
}

/// Wraps `f` to run on the main window while it is open.
fn to_main(ui: &AppWindow, f: impl Fn(&AppWindow) + 'static) -> impl Fn() + 'static {
    let main = ui.as_weak();
    move || {
        if let Some(m) = main.upgrade() {
            f(&m)
        }
    }
}

fn to_main_with<A>(ui: &AppWindow, f: impl Fn(&AppWindow, A) + 'static) -> impl Fn(A) + 'static {
    let main = ui.as_weak();
    move |v| {
        if let Some(m) = main.upgrade() {
            f(&m, v)
        }
    }
}

/// The macOS player window; its buttons forward to the main window.
fn player(ui: &AppWindow, art: impl Fn(SharedString, i32) -> Image + 'static) -> Option<PlayerBar> {
    let bar = PlayerBar::new().map_err(|e| eprintln!("nori: no player window: {e}")).ok()?;
    ui.set_native_player(true);
    bar.set_font(ui.get_font());
    bar.on_art(move |id, size, _rev| art(id, size));
    bar.on_toggle(to_main(ui, |m| m.invoke_toggle()));
    bar.on_star(to_main(ui, |m| m.invoke_star()));
    bar.on_next(to_main(ui, |m| m.invoke_next()));
    bar.on_previous(to_main(ui, |m| m.invoke_previous()));
    bar.on_toggle_shuffle(to_main(ui, |m| m.invoke_toggle_shuffle()));
    bar.on_cycle_repeat(to_main(ui, |m| m.invoke_cycle_repeat()));
    bar.on_open_full(to_main(ui, |m| {
        if m.get_has_song() {
            m.set_full_player(true);
            m.invoke_player_changed();
        }
    }));
    bar.on_seek(to_main_with(ui, |m, v| m.invoke_seek(v)));
    bar.on_set_volume(to_main_with(ui, |m, v| m.invoke_set_volume(v)));
    let go = bar.global::<crate::Go>();
    go.on_artist(to_main_with(ui, |m, id| m.global::<crate::Go>().invoke_artist(id)));
    go.on_album(to_main_with(ui, |m, id| m.global::<crate::Go>().invoke_album(id)));
    bar.on_set_inspector(to_main_with(ui, |m, i| {
        m.set_inspector(i);
        m.invoke_player_changed();
    }));
    Some(bar)
}

/// The macOS glass sidebar window; its rows forward to the main window.
fn sidebar(ui: &AppWindow, art: impl Fn(SharedString, i32) -> Image + 'static) -> Option<SidebarWindow> {
    let side = SidebarWindow::new().map_err(|e| eprintln!("nori: no glass sidebar: {e}")).ok()?;
    side.set_font(ui.get_font());
    side.set_inset_top(ui.get_inset_top());
    side.on_art(move |id, size, _rev| art(id, size));
    side.on_go(to_main_with(ui, |m, v| m.invoke_go(v)));
    side.on_open_playlist(to_main_with(ui, |m, id| m.invoke_open_playlist(id)));
    side.on_open_accounts(to_main(ui, |m| m.invoke_open_accounts()));
    side.on_drag_window(to_main(ui, |m| m.invoke_drag_window()));
    side.on_zoom_window(to_main(ui, |m| m.invoke_zoom_window()));
    ui.set_native_sidebar(true);
    Some(side)
}

/// Builds the app state and wires the window. Keep the returned app alive while the window runs.
pub fn start(ui: &AppWindow, data: PathBuf, compositor: Compositor) -> Rc<RefCell<App>> {
    let shelves = Rc::new(VecModel::from(
        session::HOME_ROWS.iter().map(|(t, _)| Shelf { title: (*t).into(), cards: ModelRc::default(), loaded: false }).collect::<Vec<_>>(),
    ));
    ui.set_shelves(ModelRc::from(shelves.clone()));
    ui.set_greeting("Home".into());
    let (tx, inbox) = Tx::new(ui.as_weak());
    let art = Rc::new(RefCell::new(Art::default()));
    let app = Rc::new_cyclic(|me: &Weak<RefCell<App>>| {
        let me = AppHandle(me.clone());
        let art_cb = |art: &Rc<RefCell<Art>>| {
            let (art, me) = (art.clone(), me.clone());
            move |id, size| cover_image(&art, &me, id, size)
        };
        let main_art = art_cb(&art);
        ui.on_art(move |id, size, _rev| main_art(id, size));
        RefCell::new(App {
            ui: ui.as_weak(),
            sidebar: sidebar(ui, art_cb(&art)),
            player: player(ui, art_cb(&art)),
            me,
            compositor,
            tx,
            inbox,
            mpris: nori_mpris::Mpris::for_app(&format!("nori.desktop{}", std::process::id())).ok().map(Arc::new),
            data,
            http: Http::new(),
            session: None,
            art,
            lyric_face: crate::sung::bold_face(),
            heard: None,
            song: None,
            queue: None,
            page_songs: Vec::new(),
            page_fetch: None,
            songs: Vec::new(),
            search_songs: Vec::new(),
            want: None,
            shelves,
            backdrop: None,
            tickets: VecDeque::new(),
            tick: Timer::default(),
            tick_ms: 0,
            facts: crate::settings::Facts::default(),
            lists: Lists::default(),
            find: String::new(),
            tuning: false,
            lyrics: None,
            lyrics_timer: Timer::default(),
            seeking: None,
            pieces: [Rc::new(VecModel::default()), Rc::new(VecModel::default())],
            queue_rows: Rc::new(VecModel::default()),
            queue_next: None,
            queue_timer: Timer::default(),
            removed: None,
            undo_timer: Timer::default(),
            note: Timer::default(),
            search: Timer::default(),
            jam_link: String::new(),
            jam_hosting: false,
            elsewhere: None,
            jam_now: None,
            jam_controls: None,
            asked: HashSet::new(),
        })
    });
    let h = app.borrow().me.clone();
    wire(ui, &h);
    h.with(|a| {
        ui.set_queue(ModelRc::from(a.queue_rows.clone()));
        ui.set_lyric_pieces_side(ModelRc::from(a.pieces[0].clone()));
        ui.set_lyric_pieces_full(ModelRc::from(a.pieces[1].clone()));
        a.compositor.roles(ui.window(), a.sidebar.as_ref().map(|s| s.window()), a.player.as_ref().map(|p| p.window()));
        menu_actions(ui, &a.compositor);
        // Now Playing's lyrics: the active line sharp, the rest blurred by the compositor.
        let weak = ui.as_weak();
        a.compositor.set_focus_source(move || {
            let ui = weak.upgrade()?;
            // Only while a line is lit: none before the first, and none once the last is over.
            let lit = usize::try_from(ui.get_lyrics_active()).is_ok_and(|i| i < ui.get_lyrics_lines().row_count());
            if !ui.get_full_player() || ui.get_full_panel() != 2 || !lit || !ui.get_lyrics_synced() || ui.get_full_lyrics_browsed() {
                return None;
            }
            let size = ui.window().size().to_logical(ui.window().scale_factor());
            let x = size.width / 2.0;
            // Clear of the volume pill above and the lyrics/queue pill below.
            // The band where the line sung is now, moving with it as the lyrics scroll; a step a line of 44 px
            // text and its 36 px gap.
            Some(Focus { region: [x, 48.0, size.width - x - 100.0, size.height - 48.0 - 56.0], band_top: ui.get_full_lyric_top() - 8.0, band_h: ui.get_full_lyric_h() + 16.0, pitch: 90.0 })
        });
        a.settings_shown();
        let prefs = crate::session::app().settings.current().unwrap_or_default();
        match prefs.servers.iter().find(|s| s.id == prefs.active_server_id).cloned() {
            Some(p) => a.open(p),
            None => ui.set_view(LOGIN),
        }
    });
    app
}

/// Menu bar items do what the window's buttons and keys do.
fn menu_actions(ui: &AppWindow, compositor: &Compositor) {
    let on = |id: &str, f: fn(&AppWindow)| compositor.menu().action(id, to_main(ui, f));
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
    on("join-jam", |m| m.set_join_open(true));
    on("queue", |m| m.set_inspector(if m.get_inspector() == 1 { 0 } else { 1 }));
    on("lyrics", |m| m.set_inspector(if m.get_inspector() == 2 { 0 } else { 2 }));
    on("full", |m| {
        if m.get_has_song() {
            m.set_full_player(!m.get_full_player());
        }
    });
}

/// The window closed: saves the volume and queue, stops the engine.
pub fn stop(app: &RefCell<App>) {
    if let Some(s) = app.borrow_mut().session.take() {
        session::own::keep(session::own::VOLUME, s.volume().to_string());
        s.close();
    }
}

fn wire(ui: &AppWindow, h: &AppHandle) {
    on!(ui.on_messages_arrived, h, |a| a.drain_inbox());
    on!(ui.on_go, h, |a, v| a.go(v));
    on!(ui.on_open_album, h, |a, id| a.open_page(Req::Album(id.into())));
    on!(ui.on_open_artist, h, |a, id| a.open_page(Req::Artist(id.into())));
    on!(ui.on_open_playlist, h, |a, id| a.open_page(Req::Playlist(id.into())));
    on!(ui.on_open_mix, h, |a, id| a.open_page(Req::Mix(id.into())));
    on!(ui.on_play_album, h, |a, id| a.play_fetch(Fetch::Album(id.into())));
    on!(ui.on_play_playlist, h, |a, id| a.play_fetch(Fetch::Playlist(id.into())));
    on!(ui.on_song, h, |a, list, i, how| a.song(list, i as usize, how));
    on!(ui.on_play_page, h, |a, shuffle| a.play_page(shuffle));
    on!(ui.on_page_later, h, |a| a.enqueue_page());
    on!(ui.on_more, h, |a| a.more_songs());
    on!(ui.on_search_edited, h, |a, t| a.search_edited(&t));
    on!(ui.on_toggle, h, |a| a.on_session(|s| s.toggle()));
    on!(ui.on_star, h, |a| {
        if let Some(song) = a.song.clone() {
            a.star(&song);
        }
    });
    on!(ui.on_next, h, |a| a.on_session(|s| s.next()));
    on!(ui.on_previous, h, |a| a.on_session(|s| s.previous()));
    on!(ui.on_seek, h, |a, f| a.seek(f));
    on!(ui.on_set_volume, h, |a, v| {
        a.on_session(|s| s.set_volume(v));
        a.ui().set_volume(v);
        a.mirror();
    });
    on!(ui.on_toggle_shuffle, h, |a| {
        let on = !a.ui().get_shuffle();
        a.on_session(|s| s.shuffle(on));
        a.say(if on { "Shuffle on" } else { "Shuffle off" }, false);
        a.follow();
    });
    on!(ui.on_cycle_repeat, h, |a| {
        // Off, all, one.
        let next = match a.ui().get_repeat() {
            0 => 2,
            2 => 1,
            _ => 0,
        };
        a.on_session(|s| s.repeat(next));
        a.say(["Repeat off", "Repeat one", "Repeat all"][next as usize], false);
        a.follow();
    });
    on!(ui.on_lyric_tapped, h, |a, line| a.lyric_tapped(line));
    on!(ui.on_jump, h, |a, i| a.on_session(|s| s.jump(i.max(0) as usize)));
    on!(ui.on_seek_by, h, |a, ms| {
        if a.session.is_some() {
            a.seek_to((a.position_now() + ms as i64).max(0));
        }
    });
    on!(ui.on_drag_window, h, |a| a.compositor.drag_window());
    on!(ui.on_zoom_window, h, |a| a.compositor.zoom_window());
    on!(ui.on_page_moved, h, |a| a.place_player());
    on!(ui.on_player_changed, h, |a| {
        a.lyrics_step(true);
        a.mirror();
        a.place_player();
        a.devices_watched();
    });
    on!(ui.on_pick_device, h, |a, id| a.pick_device(id.to_string()));
    let go = ui.global::<crate::Go>();
    on!(go.on_artist, h, |a, id| a.go_to(Req::Artist(id.into())));
    on!(go.on_album, h, |a, id| a.go_to(Req::Album(id.into())));
    on!(ui.on_jam_play, h, |a, kind, id| {
        let what = if kind == 0 { Fetch::Album(id.into()) } else { Fetch::Playlist(id.into()) };
        a.on_session(|s| {
            s.play_later(what, false);
            s.jam_open();
        });
    });
    use nori_core::remote::wire::Op;
    let jam = ui.global::<crate::Jam>();
    on!(jam.on_start, h, |a| a.on_session(|s| s.jam_open()));
    on!(jam.on_end, h, |a| {
        if let Some(r) = a.session.as_ref().and_then(|s| s.remote()) {
            r.jam_close();
        }
    });
    on!(jam.on_copy_link, h, |a| a.jam_copy_link());
    on!(jam.on_leave, h, |a| a.leave_jam());
    on!(ui.on_join_jam, h, |a, link| a.join_jam(link.trim().to_string()));
    on!(jam.on_listen, h, |a, on| {
        if let Some(r) = a.session.as_ref().and_then(|s| s.remote()) {
            r.listen(on);
        }
    });
    on!(jam.on_set_along, h, |a, on| {
        if let Some(r) = a.session.as_ref().and_then(|s| s.remote()) {
            r.jam_along(on);
        }
    });
    on!(jam.on_decide, h, |a, request, accept| {
        if let Ok(request) = request.parse() {
            a.jam_act(Op::Decide { request, accept });
        }
    });
    on!(jam.on_promote, h, |a, member, admin| a.jam_act(Op::Promote { member: member.into(), admin }));
    on!(jam.on_send_out, h, |a, member| a.jam_act(Op::Kick { member: member.into() }));
    on!(ui.on_login, h, |a| a.login());
    on!(ui.on_cancel_login, h, |a| a.go(HOME));
    on!(ui.on_find_edited, h, |a, t| {
        a.find = t.to_string();
        a.narrowed();
    });
    on!(ui.on_choose_artist, h, |a, id| a.choose_artist(id.to_string()));
    on!(ui.on_open_accounts, h, |a| {
        a.ui().set_settings_tab(6);
        a.go(SETTINGS);
    });
    on!(ui.on_clear_queue, h, |a| {
        a.on_session(|s| s.clear_upcoming());
        a.follow();
    });
    on!(ui.on_remove_queued, h, |a, i| a.remove_queued(i.max(0) as usize));
    on!(ui.on_move_queued, h, |a, from, to| {
        a.on_session(|s| s.move_song(from.max(0) as usize, to.max(0) as usize));
        a.follow();
    });
    on!(ui.on_undo_removed, h, |a| {
        if let Some(id) = a.removed.take() {
            a.on_session(|s| s.put_back(&id));
        }
        a.undo_timer.stop();
        a.ui().set_queue_undo("".into());
        a.follow();
    });
    on!(ui.on_settings_tab_chosen, h, |a, t| {
        a.ui().set_settings_tab(t);
        a.settings_shown();
    });
    on!(ui.on_setting_toggled, h, |a, name, on| a.setting(&name, if on { "true" } else { "false" }));
    on!(ui.on_setting_chosen, h, |a, name, i| a.setting_chosen(&name, i.max(0) as usize));
    on!(ui.on_setting_action, h, |a, name| a.setting_action(&name));
    on!(ui.on_setting_slid, h, |a, name, v, last| a.slid(&name, v, last));
    on!(ui.on_setting_typed, h, |a, name, text| a.setting(&name, &text));
    on!(ui.on_source_moved, h, |a, id, up| a.source_moved(&id, up));
    on!(ui.on_accent_chosen, h, |a, i| {
        if let Some(v) = crate::settings::option_value(Target::Setting("accent"), i.max(0) as usize, &a.facts) {
            a.setting("accent", &v);
        }
    });
    on!(ui.on_eq_set, h, |a, name, value| {
        a.setting(&name, &value);
        a.tune();
    });
    on!(ui.on_eq_gain, h, |a, i, v, last| a.eq_gain(i.max(0) as usize, v, last));
    on!(ui.on_eq_tool, h, |a, name, i| a.eq_tool(&name, i.max(0) as usize));
    on!(ui.on_eq_sized, h, |a, w, hh| {
        let ui = a.ui();
        ui.set_eq_curve_w(w);
        ui.set_eq_curve_h(hh);
        if let Some(p) = crate::session::app().settings.current() {
            crate::eq::curve_only(&ui, &p);
        }
    });
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

    fn drain_inbox(&mut self) {
        while let Ok(m) = self.inbox.try_recv() {
            match m {
                Msg::From(id, m) if self.session.as_ref().is_some_and(|s| s.id == id) => self.take(*m),
                // Left in flight by an earlier session.
                Msg::From(..) => {}
                m => self.take(m),
            }
        }
    }

    /// Adds the whole page to the queue.
    fn enqueue_page(&self) {
        if !self.page_songs.is_empty() {
            self.on_session(|s| s.enqueue(self.page_songs.clone(), false));
        }
    }

    fn setting_chosen(&mut self, name: &str, i: usize) {
        let target = Target::of(name);
        let Some(v) = crate::settings::option_value(target, i, &self.facts) else { return };
        match target {
            Target::Device => {
                // The output opens with the engine, so a new device applies from the next start.
                session::own::keep(session::own::DEVICE, v.clone());
                self.facts.device = v;
                self.say("The new output is used from the next start", false);
                self.settings_shown();
            }
            Target::Setting(name) => self.setting(name, &v),
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

    fn open(&mut self, profile: SavedServer) {
        if let Some(s) = self.session.take() {
            s.close();
        }
        let ui = self.ui();
        ui.set_server(nori_core::settings::label(&profile.name, &profile.url).into());
        ui.set_account(profile.user.as_str().into());
        ui.set_account_initial(profile.user.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into());
        match Session::open(&self.data, self.http.clone(), profile, self.tx.clone(), self.mpris.clone()) {
            Ok(s) => {
                s.check();
                ui.set_volume(s.volume());
                self.session = Some(s);
                self.heard = None;
                self.song = None;
                self.queue = None;
                for i in 0..self.shelves.row_count() {
                    if let Some(mut shelf) = self.shelves.row_data(i) {
                        shelf.cards = ModelRc::default();
                        shelf.loaded = false;
                        self.shelves.set_row_data(i, shelf);
                    }
                }
                ui.set_picks(ModelRc::default());
                ui.set_picks_loaded(false);
                self.jam_now = None;
                self.follow();
                self.jam_shown();
                self.go(HOME);
                if self.account() {
                    // The sidebar lists playlists on every page.
                    self.on_session(|s| s.load(Req::Playlists));
                } else {
                    // A jam guest's: the host's library, and the jam in the queue panel.
                    ui.set_side_playlists(ModelRc::default());
                    ui.set_inspector(1);
                }
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
        let (http, tx) = (self.http.clone(), self.tx.clone());
        std::thread::spawn(move || tx.send(Msg::LoggedIn(session::check_login(http, draft))));
    }

    fn go(&mut self, view: i32) {
        // A new page starts unfiltered.
        if !self.find.is_empty() {
            self.find.clear();
            self.ui().set_find_text("".into());
            self.narrowed();
        }
        if self.tuning && view != EQUALIZER {
            self.tuning = false;
            self.on_session(|s| s.engine.set_shallow(false));
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
            Req::Mix(id) => Fetch::Mix(id.clone()),
            _ => return,
        });
        ui.set_page_id(match &req {
            Req::Album(id) | Req::Artist(id) | Req::Playlist(id) | Req::Mix(id) => id.as_str().into(),
            _ => "".into(),
        });
        ui.set_page_kind(match &req {
            Req::Album(_) => 0,
            Req::Artist(_) => 1,
            _ => 2,
        });
        ui.set_page_mix(matches!(req, Req::Mix(_)));
        ui.set_page_title("".into());
        ui.set_page_sub("".into());
        ui.set_page_sub_artist("".into());
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

    /// Opens a page from a name that links to it, leaving Now Playing for it.
    fn go_to(&mut self, req: Req) {
        self.ui().set_full_player(false);
        self.open_page(req);
    }

    /// Page colours from its cover, or the defaults.
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

    /// Cross-fades the window backdrop to this cover's wash (into the hidden layer; app.slint animates).
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
        // Now Playing draws in white whatever the sleeve: a light page is taken dark.
        let c = &nori_look::cover::under_white(c);
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
            Data::Picks(tiles) => {
                let picks: Vec<Pick> = tiles
                    .into_iter()
                    .map(|t| {
                        let colours = nori_core::mixes::board::mix_tile_colours(t.id.clone());
                        Pick {
                            title: words::mix_name(t.name).into(),
                            sub: words::mix_caption(t.favourites).into(),
                            covers: ModelRc::new(VecModel::from(t.covers.into_iter().map(SharedString::from).collect::<Vec<_>>())),
                            tint: colour(colours[0]),
                            deep: colour(colours[1]),
                            id: t.id.into(),
                        }
                    })
                    .collect();
                ui.set_picks(ModelRc::new(VecModel::from(picks)));
                ui.set_picks_loaded(true);
            }
            Data::HomeRow(i, albums) => {
                if let Some(mut shelf) = self.shelves.row_data(i) {
                    shelf.cards = cards(albums.iter().map(album_card));
                    shelf.loaded = true;
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
                // The Artists view opens on its first artist.
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
                        sub_artist: SharedString::default(),
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
            // Page details apply only if that page is still shown.
            Data::Album(d) if shown => {
                ui.set_page_title(d.album.name.as_str().into());
                ui.set_page_sub(d.album.artist.as_str().into());
                ui.set_page_sub_artist(d.album.artist_id.clone().unwrap_or_default().into());
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
                ui.set_page_albums(cards(d.albums.iter().map(|a| Card { sub: if a.year > 0 { a.year.to_string().into() } else { "".into() }, sub_artist: SharedString::default(), ..album_card(a) })));
            }
            Data::Playlist(d) if shown => {
                ui.set_page_title(d.playlist.name.as_str().into());
                ui.set_page_sub(d.playlist.owner.clone().unwrap_or_default().into());
                ui.set_page_caption(words::songs_caption(d.songs.len(), d.seconds).into());
                self.set_page_art(d.playlist.cover_art.clone());
                self.page_songs = d.songs;
                ui.set_page_songs(self.rows(&self.page_songs));
            }
            Data::Mix(m) if shown => {
                ui.set_page_title(words::mix_name(m.name).into());
                ui.set_page_sub(words::mix_caption(m.favourites).into());
                ui.set_page_caption(words::songs_caption(m.songs.len(), m.seconds).into());
                self.set_page_art(m.covers.first().cloned());
                self.page_songs = m.songs;
                ui.set_page_songs(self.rows(&self.page_songs));
            }
            Data::Album(_) | Data::Artist(_) | Data::Playlist(_) | Data::Mix(_) => {}
        }
    }

    /// Sets the page picture. Only artist pages take cover colours; albums and playlists stay plain.
    fn set_page_art(&self, art: Option<String>) {
        let art = art.unwrap_or_default();
        let ui = self.ui();
        ui.set_page_art(art.as_str().into());
        let c = self.art.borrow().colours.get(&art).cloned().filter(|_| ui.get_page_kind() == 1);
        self.page_colours(c.as_deref());
    }

    /// Songs as rows filtered by the find text; each row keeps its index in `songs`.
    fn rows(&self, songs: &[Song]) -> ModelRc<SongRow> {
        let heard = self.heard.as_deref();
        let find = self.find.to_lowercase();
        let hit = |s: &Song| find.is_empty() || [&s.title, &s.artist, &s.album].iter().any(|t| t.to_lowercase().contains(&find));
        let row = |(i, s): (usize, &Song)| SongRow { by: if self.asked.contains(&s.id) { words::ASKED.into() } else { SharedString::default() }, ..row(s, i, heard == Some(s.id.as_str()), self.heart(s)) };
        ModelRc::new(VecModel::from(songs.iter().enumerate().filter(|(_, s)| hit(s)).map(row).collect::<Vec<_>>()))
    }

    /// Re-applies the find filter to every list.
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

    /// Shows an artist beside the Artists list.
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
        // Keep the list visible while the artist loads.
        ui.set_loading(false);
    }

    /// Refreshes the now-playing marks in the lists.
    fn mark_playing(&self) {
        let ui = self.ui();
        ui.set_page_songs(self.rows(&self.page_songs));
        ui.set_search_songs(self.rows(&self.search_songs));
        if !self.songs.is_empty() {
            ui.set_songs(self.rows(&self.songs));
        }
    }

    fn more_songs(&mut self) {
        let req = Req::Songs { offset: self.songs.len() as u32 };
        // Scrolling asks again as the list nears its end: the page already on its way is asked for once.
        if self.ui().get_loading() && self.want.as_ref() == Some(&req) {
            return;
        }
        self.load(req);
    }

    fn search_edited(&mut self, text: &str) {
        let Some(s) = &self.session else { return };
        let view = s.search_typed(text);
        let query = view.query.clone();
        self.show_search(view);
        let delay = crate::session::app().settings.prefs(|p| p.live_search_delay_ms).max(100) as u64;
        let me = self.me.clone();
        self.search.start(TimerMode::SingleShot, Duration::from_millis(delay), move || {
            me.with(|a| a.on_session(|s| s.search_server(query.clone())));
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
        if how == 4 {
            return self.star(&one);
        }
        // A guest asks for the song (the session does), and starts no jam of its own.
        if how == 3 && self.guest() {
            return;
        }
        self.on_session(|s| match how {
            0 => s.play(songs.clone(), i, false, origin),
            3 => {
                s.play(songs.clone(), i, false, origin);
                s.jam_open();
            }
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

    fn seek(&mut self, fraction: f32) {
        let Some(song) = &self.song else { return };
        let ms = (fraction as f64 * song.duration as f64 * 1000.0) as i64;
        self.ui().set_position_ms(ms as i32);
        self.seek_to(ms);
    }

    /// Seeks to `ms`; the lyrics show that place at once, not the old one until the seek lands (another
    /// device's place shows the seek at once).
    fn seek_to(&mut self, ms: i64) {
        if self.elsewhere.is_none() {
            self.seeking = Some(ms);
        }
        self.on_session(|s| s.seek(ms));
        self.lyrics_step(true);
    }

    /// Whether `s` shows starred: as the device playing shows it, while that is another one.
    fn starred(&self, s: &Song) -> bool {
        self.session.as_ref().is_some_and(|x| x.starred(s, self.elsewhere.as_ref()))
    }

    /// Flips `s`'s heart, once: through the device playing while it has the song, else here.
    fn star(&self, s: &Song) {
        let on = !self.starred(s);
        self.on_session(|x| x.star_song(s.id.clone(), on, self.elsewhere.as_ref()));
    }

    /// Where the song playing is now, here or on the device playing.
    fn position_now(&self) -> i64 {
        match (&self.elsewhere, &self.jam_now, &self.session) {
            (Some(e), _, _) => e.position_ms(),
            (None, Some((st, at)), _) => nori_core::remote::position_now(st, at.elapsed().as_millis() as i64),
            (None, None, Some(s)) => s.engine.status_with(|st| st.position_now()),
            (None, None, None) => 0,
        }
    }

    /// The open profile is a jam guest's.
    fn guest(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.guest)
    }

    /// Whether the player offers `control`: a jam guest's by its role (the core's jam controls), this
    /// computer's own player all of them.
    fn offers(&self, control: fn(&nori_core::remote::Controls) -> nori_core::remote::Reach) -> bool {
        !self.guest() || self.jam_controls.is_some_and(|c| control(&c.controls) != nori_core::remote::Reach::Nowhere)
    }

    /// Whether the open profile has the account's things (hearts, playlists, settings): a jam guest's has
    /// not (the core's `ProfileRules`).
    fn account(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.rules.account)
    }

    /// `s`'s heart as the lists show it; a guest has none.
    fn heart(&self, s: &Song) -> Option<bool> {
        self.account().then(|| self.starred(s))
    }

    /// Joins the jam `link` invites to, as a guest profile of its own.
    fn join_jam(&mut self, link: String) {
        let ui = self.ui();
        if !nori_core::remote::is_invite(&link) {
            return ui.set_join_error(words::NOT_AN_INVITE.into());
        }
        ui.set_join_busy(true);
        ui.set_join_error("".into());
        let (http, tx, remote) = (self.http.clone(), self.tx.clone(), self.session.as_ref().and_then(|s| s.remote()));
        nori_host::spawn("nori-jam-join", move || {
            let joined = nori_host::jam_join(http, &session::app().settings, link, &nori_host::device_name(), remote.as_deref());
            tx.send(Msg::Joined(joined.map_err(|e| words::jam_join_failed(&e))))
        });
    }

    /// The guest profile joined with: opened in place of the user's own, which is opened again on leaving.
    fn joined(&mut self, pass: nori_core::remote::JamPass) {
        let ui = self.ui();
        ui.set_join_busy(false);
        ui.set_join_open(false);
        ui.set_join_link("".into());
        let guest = nori_host::jam_joined(&session::app().settings, pass, words::JAM_GUEST);
        self.open(guest);
    }

    /// Leaves the jam this guest is in, at once; the relay is told on the way.
    fn leave_jam(&mut self) {
        if let Some(r) = self.session.as_ref().and_then(|s| s.remote()) {
            r.jam_leave();
        }
        self.left(words::JAM_LEFT);
    }

    /// The guest profile is dropped, and the user's own opened again; `said` says why.
    fn left(&mut self, said: &str) {
        let back = nori_host::jam_left(&session::app().settings);
        self.jam_now = None;
        self.asked.clear();
        match back {
            Some(p) => self.open(p),
            None => {
                if let Some(s) = self.session.take() {
                    s.close();
                }
                self.go(LOGIN);
            }
        }
        self.say(said, false);
    }

    fn take(&mut self, m: Msg) {
        match m {
            Msg::Engine(e) => {
                match &e {
                    // A seek landed: the lyrics follow from the new place.
                    Event::Position { .. } => {
                        self.seeking = None;
                        self.lyrics_step(true);
                    }
                    // A seek still under way is overtaken by another song.
                    Event::Song { .. } => self.seeking = None,
                    Event::Buffering(b) if self.elsewhere.is_none() => self.ui().set_buffering(*b),
                    Event::Error { message, .. } => self.say(&format!("Could not play: {message}"), true),
                    _ => {}
                }
                if let Some(s) = &self.session {
                    s.mpris_changed();
                    s.followed(&e);
                }
                self.follow();
            }
            Msg::Data(req, r) => {
                self.data(req, r);
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
                    let l = crate::lyrics::SongLyrics::new(pick, self.position_now(), self.lyric_face.clone());
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
            Msg::Reachable(Ok(())) | Msg::From(..) => {}
            Msg::Remote => {
                self.elsewhere_read();
                self.devices_shown();
                self.jam_shown();
                // Another device or the jam may have changed the queue (a song accepted, added from afar).
                self.follow();
            }
            Msg::Jam(Ok(())) => {
                // The jam shows in the queue panel.
                self.ui().set_inspector(1);
                self.jam_shown();
            }
            Msg::Jam(Err(e)) => self.say(&format!("{} ({e})", words::JAM_FAILED), true),
            Msg::Joined(Ok(pass)) => self.joined(pass),
            Msg::Joined(Err(e)) => {
                let ui = self.ui();
                ui.set_join_busy(false);
                ui.set_join_error(e.into());
            }
            Msg::JamEnded(host) => self.left(&words::jam_ended(host.as_deref())),
            Msg::Starred => {
                self.on_session(|s| s.mpris_changed());
                self.follow();
                self.mark_playing();
            }
            Msg::Volume(v) if self.elsewhere.is_none() => self.ui().set_volume(v),
            Msg::Volume(_) => {}
            Msg::LoggedIn(r) => {
                let ui = self.ui();
                ui.set_login_busy(false);
                match r {
                    Ok(p) => {
                        let mut prefs = crate::session::app().settings.current().unwrap_or_default();
                        prefs.servers.retain(|s| !(s.url == p.url && s.user == p.user));
                        prefs.servers.push(p.clone());
                        prefs.active_server_id = p.id.clone();
                        crate::session::app().settings.put(prefs);
                        ui.set_login_password("".into());
                        self.open(p);
                    }
                    Err(e) => ui.set_login_error(e.into()),
                }
            }
        }
    }

    /// Requests the covers found missing while drawing.
    fn ask_covers(&mut self) {
        let mut art = self.art.borrow_mut();
        let a = &mut *art;
        let wanted = std::mem::take(&mut a.wanted);
        let Some(s) = &self.session else {
            // No session yet: forget them so they are requested again later.
            a.asked.clear();
            return;
        };
        // Cancel requests for covers no longer on screen.
        let now = Instant::now();
        self.tickets.retain(|(k, _)| {
            let shown = a.missing.get(k).is_some_and(|t| now.duration_since(*t) < GONE);
            if !shown {
                a.asked.remove(k);
                a.missing.remove(k);
            }
            shown
        });
        // The loader serves newest first; reversed so the top left of a frame comes first.
        for k in wanted.into_iter().rev() {
            let Some(t) = s.cover(k.clone(), cover_px(k.size)) else { continue };
            self.tickets.push_back((k, t));
        }
        // Dropping a ticket cancels it; the cover may be requested again.
        while self.tickets.len() > PENDING_KEPT {
            let (k, _) = self.tickets.pop_front().expect("longer than the limit");
            a.asked.remove(&k);
            a.missing.remove(&k);
        }
    }

    fn cover(&mut self, key: CoverKey, image: &Picture, colours: Option<Box<CoverColours>>) {
        let id = key.id.clone();
        let large = key.size != CoverSize::Card;
        self.tickets.retain(|(k, _)| *k != key);
        {
            let mut a = self.art.borrow_mut();
            let now = Instant::now();
            a.missing.remove(&key);
            a.images.insert(key.clone(), (picture(image), now));
            if let Some(c) = colours {
                a.colours.insert(id.clone(), Rc::from(c));
            }
            let limit = if large { LARGE_KEPT } else { SMALL_KEPT };
            let class = |k: &CoverKey| (k.size != CoverSize::Card) == large;
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
                    a.colours.remove(&old.id);
                }
                count -= 1;
            }
        }
        let ui = self.ui();
        ui.set_covers_rev(ui.get_covers_rev().wrapping_add(1));
        if let Some(p) = &self.player {
            p.set_covers_rev(ui.get_covers_rev());
        }
        if let Some(sd) = &self.sidebar {
            sd.set_covers_rev(ui.get_covers_rev());
        }
        if large {
            let c = self.art.borrow().colours.get(&id).cloned();
            if ui.get_now_art() == id.as_str() {
                self.now_colours(&id, c.as_deref());
            }
            if ui.get_view() == PAGE && ui.get_page_kind() == 1 && ui.get_page_art() == id.as_str() {
                self.page_colours(c.as_deref());
            }
        }
    }

    /// Copies the main window's state to the player and sidebar windows.
    fn mirror(&self) {
        let ui = self.ui();
        if let Some(p) = &self.player {
            p.set_has_song(ui.get_has_song());
            p.set_now_title(ui.get_now_title());
            p.set_now_artist(ui.get_now_artist());
            p.set_now_album(ui.get_now_album());
            p.set_now_artist_id(ui.get_now_artist_id());
            p.set_now_album_id(ui.get_now_album_id());
            p.set_now_credits(ui.get_now_credits());
            p.set_now_starred(ui.get_now_starred());
            p.set_now_art(ui.get_now_art());
            p.set_playing(ui.get_playing());
            p.set_position_ms(ui.get_position_ms());
            p.set_duration_ms(ui.get_duration_ms());
            p.set_shuffle(ui.get_shuffle());
            p.set_repeat(ui.get_repeat());
            p.set_volume(ui.get_volume());
            p.set_inspector(ui.get_inspector());
            p.set_covers_rev(ui.get_covers_rev());
            p.set_devices_on(ui.get_devices_on());
            p.set_jam(ui.global::<crate::Jam>().get_strip());
            p.set_guest(ui.global::<crate::Jam>().get_guest());
            p.set_can_play(ui.global::<crate::Jam>().get_can_play());
            p.set_can_skip(ui.global::<crate::Jam>().get_can_skip());
            p.set_can_seek(ui.global::<crate::Jam>().get_can_seek());
            p.set_playing_on(ui.get_playing_on());
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
            sd.set_guest(ui.global::<crate::Jam>().get_guest());
        }
    }

    /// Positions the glass layers; hidden over Now Playing and the login page.
    fn place_player(&self) {
        let ui = self.ui();
        let shown = !ui.get_full_player() && ui.get_view() != LOGIN;
        self.compositor.set_right_panel(if ui.get_inspector() != 0 { 280.0 } else { 0.0 });
        self.compositor.show_glass(shown && self.sidebar.is_some(), shown && self.player.is_some());
    }

    /// Updates the lyrics view from the playback position and schedules the next update.
    fn lyrics_step(&mut self, force: bool) {
        let ui = self.ui();
        let at = self.seeking.unwrap_or_else(|| self.position_now());
        let playing = ui.get_playing();
        let Some(l) = &mut self.lyrics else { return };
        let full = ui.get_full_player() && ui.get_full_panel() == 2;
        let side = ui.get_inspector() == 2;
        let now = l.advance(at, full || side, force);
        ui.set_lyrics_active(now.active);
        ui.set_lyric_sweeping(now.sweeping);
        let width = ui.window().size().to_logical(ui.window().scale_factor()).width;
        let mut pieces = |view, on: bool, size, w, lit, dim| if on && now.sweeping { l.pieces(view, &now, size, w, lit, dim) } else { Vec::new() };
        renew(&self.pieces[0], pieces(0, side, 22.0, 280.0 - 44.0, 0.92, 0.26));
        renew(&self.pieces[1], pieces(1, full, 44.0, width / 2.0 - 140.0, 1.0, 0.36));
        ui.set_lyric_sung(now.sung.into());
        ui.set_lyric_now(now.now.into());
        ui.set_lyric_mix(now.mix);
        ui.set_lyric_rest(now.rest.into());
        match now.wait.filter(|_| playing) {
            Some(ms) => {
                let me = self.me.clone();
                self.lyrics_timer.start(TimerMode::SingleShot, Duration::from_millis(ms), move || me.with(|a| a.lyrics_step(false)));
            }
            None => self.lyrics_timer.stop(),
        }
    }

    fn lyric_tapped(&mut self, line: i32) {
        let (Some(l), Ok(line)) = (&self.lyrics, usize::try_from(line)) else { return };
        let ms = l.tap(line);
        self.seek_to(ms);
    }

    /// Shows `rows` in the queue: leaving rows fold away first, then the list settles.
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
        let me = self.me.clone();
        self.queue_timer.start(TimerMode::SingleShot, Duration::from_millis(QUEUE_FOLD_MS), move || {
            me.with(|a| {
                if let Some(next) = a.queue_next.take() {
                    settle(&a.queue_rows, next);
                }
            })
        });
    }

    /// Takes the song at list index `index` out of the queue shown (here, or on the device playing), and
    /// offers its undo for a few seconds.
    fn remove_queued(&mut self, index: usize) {
        let song = match &self.elsewhere {
            Some(e) => e.mirror.rows.iter().find(|r| r.index as usize == index).map(|r| r.song.clone()),
            None => self.queue.as_ref().and_then(|q| q.songs.get(index).cloned()),
        };
        let Some(song) = song else { return };
        self.on_session(|s| s.remove(index));
        self.ui().set_queue_undo(words::removed(&song.title).into());
        self.removed = Some(song.id);
        let weak = self.me.clone();
        self.undo_timer.start(TimerMode::SingleShot, Duration::from_millis(UNDO_MS), move || {
            weak.with(|a| {
                a.removed = None;
                a.ui().set_queue_undo("".into());
            })
        });
        self.follow();
    }

    /// Follows the other devices while their panel is open (remote control), and lists them.
    fn devices_watched(&self) {
        let ui = self.ui();
        let open = ui.get_inspector() == 3;
        // A jam guest follows its jam all along.
        if let Some(r) = self.session.as_ref().and_then(|s| s.remote()).filter(|_| !self.guest()) {
            r.watch(open);
        }
        if open {
            self.devices_shown();
        }
    }

    /// The devices panel's rows: this computer, then the remote's devices, the one playing ticked.
    fn devices_shown(&self) {
        let ui = self.ui();
        let remote = self.session.as_ref().and_then(|s| s.remote());
        // A jam guest's music is the jam's; it moves to no other device.
        ui.set_devices_on(remote.is_some() && !self.guest());
        let Some(r) = remote.filter(|_| ui.get_inspector() == 3) else { return };
        let active = self.elsewhere.as_ref().map(|e| e.mirror.id.as_str());
        let here = std::iter::once(words::device_row(None, active.is_none()));
        let mut devices = r.devices();
        let names = nori_core::remote::device_names(devices.clone(), r.me(), words::kind_words());
        devices.iter_mut().zip(names).for_each(|(d, name)| d.name = name);
        let rows: Vec<crate::DeviceRow> = here.chain(devices.iter().map(|d| words::device_row(Some(d), active == Some(d.id.as_str())))).collect();
        ui.set_devices(ModelRc::new(VecModel::from(rows)));
    }

    /// Moves the music to device `id`, or here ("").
    fn pick_device(&self, id: String) {
        if let Some(r) = self.session.as_ref().and_then(|s| s.remote()) {
            r.pick((!id.is_empty()).then_some(id));
        }
    }

    /// Reads the device playing while it is another one. Coming or going, the player starts over from
    /// what it then shows.
    fn elsewhere_read(&mut self) {
        let Some(s) = &self.session else { return };
        let e = s.elsewhere();
        let ui = self.ui();
        ui.set_playing_on(e.as_ref().map_or_else(String::new, |e| words::playing_on(&e.mirror.name)).into());
        let moved = e.is_some() != self.elsewhere.is_some();
        self.elsewhere = e;
        if moved {
            self.queue = None;
            self.seeking = None;
            ui.set_buffering(false);
            let id = match &self.elsewhere {
                Some(e) => e.song().map(|s| s.id.clone()),
                None => {
                    ui.set_volume(s.volume());
                    s.engine.status_with(|st| st.id.clone())
                }
            };
            self.song_shown(id);
        }
        if moved || self.elsewhere.is_some() {
            self.on_session(|s| s.mpris_changed());
        }
    }

    /// Redraws the settings or equalizer page from the current settings.
    fn settings_shown(&self) {
        let ui = self.ui();
        let prefs = crate::session::app().settings.current().unwrap_or_default();
        self.devices_shown();
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
        self.jam_shown();
    }

    /// The `Jam` global from the jam this computer hosts; nothing is asked of a remote while jams are off.
    fn jam_shown(&mut self) {
        let ui = self.ui();
        let g = ui.global::<crate::Jam>();
        let guest = self.guest();
        let jams = guest || crate::session::app().settings.prefs(|p| p.jam);
        let remote = self.session.as_ref().and_then(|s| s.remote()).filter(|_| jams);
        let unsupported = remote.as_ref().is_some_and(|r| r.relay() == nori_core::remote::RelaySupport::Unsupported);
        let view = remote.as_ref().and_then(|r| r.jam_view()).filter(|v| v.hosting || guest);
        g.set_on(remote.is_some() && !unsupported && !guest);
        g.set_guest(guest);
        g.set_note(if unsupported && !guest { words::JAM_UNSUPPORTED.into() } else { "".into() });
        g.set_hosting(view.as_ref().is_some_and(|v| v.hosting));
        g.set_along(view.as_ref().is_some_and(|v| v.along));
        let listening = view.as_ref().filter(|_| guest).map_or(nori_core::remote::Listening::Watching, |v| v.listening);
        g.set_here(listening != nori_core::remote::Listening::Watching);
        g.set_along_note(words::jam_along(listening).into());
        // A guest follows the host's playback, its place run on from when the host heard it.
        let heard = remote.as_ref().and_then(|r| r.jam_playing()).map(|m| Instant::now() - Duration::from_millis(m.heard_ago_ms().max(0) as u64));
        self.jam_controls = remote.as_ref().filter(|_| guest).and_then(|r| r.jam_controls());
        g.set_can_play(self.offers(|c| c.play_pause));
        g.set_can_skip(self.offers(|c| c.skip));
        g.set_can_seek(self.offers(|c| c.seek));
        self.jam_now = view.as_ref().filter(|_| guest).and_then(|v| v.queue.clone().map(|q| (q, heard.unwrap_or_else(|| Instant::now() - Duration::from_millis(v.age_ms.max(0) as u64)))));
        let link = view.as_ref().and_then(|v| v.link.clone()).unwrap_or_default();
        if link != self.jam_link {
            g.set_qr(if link.is_empty() { Image::default() } else { crate::jam::qr(&link) });
            g.set_link(link.as_str().into());
            let home = nori_core::remote::parse_invite(&link).map(|(server, _)| server).filter(|s| nori_core::remote::is_home_only(s));
            g.set_reach(home.map_or_else(String::new, |s| words::jam_home_only(&s)).into());
            self.jam_link = link;
        }
        let shown = view.as_ref().map(crate::jam::shown);
        g.set_host(shown.as_ref().map_or_else(String::new, |s| s.host.clone()).into());
        let asked = shown.as_ref().map(|s| s.asked.clone()).unwrap_or_default();
        if asked != self.asked {
            self.asked = asked;
            self.mark_playing();
        }
        let strip = shown.as_ref().map_or_else(String::new, |s| s.strip.clone());
        // Paused here while the jam plays on: play joins it again.
        let strip = if self.jam_controls.is_some_and(|c| c.paused_here) { words::JAM_PAUSED_HERE.to_string() } else { strip };
        g.set_strip(strip.into());
        g.set_listening(shown.as_ref().map_or_else(String::new, |s| s.listening.clone()).into());
        let (people, asks) = shown.map_or_else(Default::default, |s| (s.people, s.asks));
        g.set_people(ModelRc::new(VecModel::from(people)));
        g.set_asks(ModelRc::new(VecModel::from(asks)));
        let hosting = view.as_ref().is_some_and(|v| v.hosting);
        if self.jam_hosting != hosting {
            // The queue's "added by" chips come and go with the jam.
            self.jam_hosting = hosting;
            self.queue = None;
            self.follow();
        }
        self.mirror();
    }

    /// A jam op of the host's own: accepting or refusing a request, a role, sending someone out.
    fn jam_act(&self, op: nori_core::remote::wire::Op) {
        if let Some(r) = self.session.as_ref().and_then(|s| s.remote()) {
            r.jam_act(op);
        }
    }

    fn jam_copy_link(&self) {
        let copied = arboard::Clipboard::new().and_then(|mut c| c.set_text(self.jam_link.clone()));
        match copied {
            Ok(()) => self.say(words::LINK_COPIED, false),
            Err(e) => self.say(&e.to_string(), true),
        }
    }

    /// Enters the engine's shallow buffer while the equalizer page is open.
    fn tune(&mut self) {
        if !self.tuning && self.ui().get_view() == EQUALIZER {
            self.tuning = true;
            self.on_session(|s| s.engine.set_shallow(true));
        }
    }

    /// Edits a level in place; redraws the page when the slider is released.
    fn slid(&mut self, name: &str, v: f32, last: bool) {
        let Some(level) = crate::settings::level_of(name) else { return };
        if let Some((effect, _)) = crate::session::app().settings.edit_level(level, v) {
            self.on_session(|s| s.applied(effect));
            self.tune();
        }
        if last {
            self.settings_shown();
        }
    }

    /// Edits an EQ band; the curve updates at once, the page on release.
    fn eq_gain(&mut self, i: usize, v: f32, last: bool) {
        let Some(p) = crate::session::app().settings.current() else { return };
        let effect = if p.eq_mode == nori_core::settings::EqMode::Graphic {
            crate::session::app().settings.edit_graphic(i as u32, v).map(|e| e.0)
        } else {
            p.eq_bands.get(i).and_then(|b| crate::session::app().settings.edit_band(i as u32, nori_core::settings::SoundBand { gain_db: v, ..*b }).map(|e| e.0))
        };
        if let Some(effect) = effect {
            self.on_session(|s| s.applied(effect));
            self.tune();
        }
        if last {
            self.settings_shown();
        } else if let Some(p) = crate::session::app().settings.current() {
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
        match crate::session::app().settings.sound_tool(tool) {
            Ok(Some(change)) => {
                self.on_session(|s| s.applied(change.effect));
                self.tune();
            }
            Ok(None) => {}
            Err(e) => self.say(&format!("{e:?}"), true),
        }
        self.settings_shown();
    }

    fn source_moved(&mut self, id: &str, up: bool) {
        let Some(p) = crate::session::app().settings.current() else { return };
        let s = nori_core::settings_model::state(&p, nori_core::settings_model::Output::default(), &crate::session::app().settings.model);
        let Some(at) = s.lyrics_sources.iter().position(|x| x.id == id) else { return };
        let to = if up { at.saturating_sub(1) } else { (at + 1).min(s.lyrics_sources.len() - 1) };
        if to != at {
            self.setting("lyricsPlace", &format!("{id}:{to}"));
        }
    }

    fn setting_action(&mut self, name: &str) {
        match Act::of(name) {
            Some(Act::Equalizer) => self.go(EQUALIZER),
            Some(Act::AddServer) => self.go(LOGIN),
            Some(Act::Server(id)) => {
                let mut prefs = crate::session::app().settings.current().unwrap_or_default();
                let Some(p) = prefs.servers.iter().find(|s| s.id == id).cloned() else { return };
                prefs.active_server_id = id;
                crate::session::app().settings.put(prefs);
                self.open(p);
            }
            Some(Act::Chore(c)) => {
                self.on_session(|s| s.action(c));
                self.on_session(|s| s.facts());
            }
            None => {}
        }
    }

    /// Shows song `id` as the one playing, and asks for its lyrics.
    fn song_shown(&mut self, id: Option<String>) {
        let ui = self.ui();
        self.heard = id.clone();
        self.song = match (&self.elsewhere, &self.jam_now) {
            (Some(e), _) => e.song().cloned(),
            (None, Some((st, _))) => crate::jam::playing(st).map(|e| e.song()),
            (None, None) => id.and_then(|id| crate::session::app().song(&id)),
        };
        let song = self.song.clone().unwrap_or_default();
        ui.set_has_song(self.song.is_some());
        ui.set_now_title(song.title.as_str().into());
        ui.set_now_artist(song.artist.as_str().into());
        ui.set_now_album(song.album.as_str().into());
        ui.set_now_artist_id(song.artist_id.clone().unwrap_or_default().into());
        ui.set_now_album_id(song.album_id.clone().unwrap_or_default().into());
        ui.set_now_credits(credits(&song));
        ui.set_duration_ms((song.duration as i64 * 1000) as i32);
        let art = song.cover_art.clone().unwrap_or_default();
        ui.set_now_art(art.as_str().into());
        let c = self.art.borrow().colours.get(&art).cloned();
        self.now_colours(&art, c.as_deref());
        // Request the large cover for the backdrop colours even when no view shows it.
        if c.is_none() {
            let _ = cover_image(&self.art, &self.me, art.as_str().into(), 1);
        }
        self.mark_playing();
        self.lyrics = None;
        ui.set_lyrics_lines(ModelRc::default());
        ui.set_lyrics_active(-1);
        ui.set_lyrics_note(if self.song.is_some() { "Looking for lyrics…".into() } else { "".into() });
        if let (Some(s), Some(id)) = (&self.session, &self.heard) {
            s.lyrics(id.clone());
        }
    }

    /// Syncs the window with the device playing: this computer's engine status and queue, or the device's
    /// it shows instead.
    fn follow(&mut self) {
        let Some(s) = &self.session else { return };
        let ui = self.ui();
        let guest = s.guest;
        let (id, playing) = match (&self.elsewhere, &self.jam_now) {
            (Some(e), _) => (e.song().map(|s| s.id.clone()), e.mirror.playing),
            // A guest's play button says what its controls say: paused here while the jam plays on.
            (None, Some((st, _))) => (crate::jam::playing(st).map(|e| e.id.clone()), self.jam_controls.map_or(st.playing, |c| c.playing)),
            (None, None) if guest => (None, false),
            (None, None) => s.engine.status_with(|st| (st.id.clone(), st.state == State::Playing)),
        };
        ui.set_playing(playing);
        ui.set_position_ms(self.position_now() as i32);
        if id != self.heard {
            self.song_shown(id);
        }
        ui.set_now_starred(self.song.as_ref().is_some_and(|s| self.starred(s)));
        if let Some(e) = &self.elsewhere {
            let m = &e.mirror;
            ui.set_shuffle(m.shuffle);
            ui.set_repeat(m.repeat as i32);
            ui.set_buffering(m.buffering && m.playing);
            if let Some(v) = e.volume() {
                ui.set_volume(v);
            }
            let rows = e.upcoming().iter().map(|r| row(&r.song, r.index as usize, false, self.heart(&r.song))).collect();
            ui.set_queue_from(queue_from(e.upcoming().iter().map(|r| &r.song)).into());
            self.queue_shown(rows);
        }
        if let Some((st, _)) = &self.jam_now {
            let next: Vec<(u32, Song, Option<String>)> = crate::jam::upcoming(st).map(|e| (e.index, e.song(), e.by.clone())).collect();
            let rows = next.iter().map(|(i, s, by)| SongRow { by: by.clone().unwrap_or_default().into(), ..row(s, *i as usize, false, None) }).collect();
            ui.set_queue_from(queue_from(next.iter().map(|(_, s, _)| s)).into());
            self.queue_shown(rows);
        }
        // Copy the queue only when it changed.
        let (rev, repeat, index) = crate::session::app().playlist(|p| (p.rev(), p.repeat(), p.current().map_or(-1, |c| c as i32)));
        if self.elsewhere.is_none() && !guest && self.queue.as_ref().is_none_or(|q| q.rev != rev || q.repeat != repeat || q.index != index) {
            let held = self.queue.as_ref().map_or(u64::MAX, |q| q.list_rev);
            let mut v = crate::session::app().view(held);
            if v.songs.is_empty() && v.len > 0 {
                if let Some(q) = &self.queue {
                    v.songs = q.songs.clone();
                }
            }
            ui.set_shuffle(v.shuffle);
            ui.set_repeat(v.repeat as i32);
            let jam = self.session.as_ref().and_then(|s| s.remote()).filter(|_| self.jam_hosting);
            let added = jam.map(|r| r.jam_added()).unwrap_or_default();
            let rows = queue_rows(&v, |id| added.get(id).cloned(), |s| self.heart(s));
            self.queue_shown(rows);
            ui.set_queue_from(queue_from(upcoming(&v).map(|(_, s)| s)).into());
            self.queue = Some(v);
        }
        // Seek bar timer: only while playing, one step per pixel of the widest bar.
        let step = pixel_ms(ui.get_duration_ms() as i64);
        if playing && (!self.tick.running() || self.tick_ms != step) {
            self.tick_ms = step;
            let me = self.me.clone();
            self.tick.start(TimerMode::Repeated, Duration::from_millis(step), move || {
                me.with(|a| {
                    let ui = a.ui();
                    ui.set_position_ms(a.position_now() as i32);
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

/// Seek bar step: one pixel of the widest bar (~380 pt at 2x), clamped to 16..250 ms.
fn pixel_ms(duration_ms: i64) -> u64 {
    (duration_ms.max(1) as u64 / 760).clamp(16, 250)
}

fn cards(it: impl Iterator<Item = Card>) -> ModelRc<Card> {
    ModelRc::new(VecModel::from(it.collect::<Vec<_>>()))
}

fn album_card(a: &nori_core::Album) -> Card {
    let sub_artist = a.artist_id.clone().unwrap_or_default().into();
    Card { id: a.id.as_str().into(), title: a.name.as_str().into(), sub: a.artist.as_str().into(), art: a.cover_art.clone().unwrap_or_default().into(), sub_artist }
}

fn artist_card(a: &nori_core::Artist) -> Card {
    Card { id: a.id.as_str().into(), title: a.name.as_str().into(), sub: words::albums(a.album_count).into(), art: a.cover_art.clone().unwrap_or_default().into(), sub_artist: SharedString::default() }
}

/// `starred`: its heart as shown; None where there is no heart (a jam guest's lists).
fn row(s: &Song, index: usize, playing: bool, starred: Option<bool>) -> SongRow {
    SongRow {
        title: s.title.as_str().into(),
        artist: s.artist.as_str().into(),
        credits: credits(s),
        menu: song_menu(s, starred),
        album: s.album.as_str().into(),
        artist_id: s.artist_id.clone().unwrap_or_default().into(),
        album_id: s.album_id.clone().unwrap_or_default().into(),
        time: words::duration(s.duration as i64).into(),
        art: s.cover_art.clone().unwrap_or_default().into(),
        index: index as i32,
        playing,
        leaving: false,
        fresh: false,
        by: SharedString::default(),
    }
}

/// How `s`'s artist line links its artists (menus.rs's `artist_line`).
fn credits(s: &Song) -> Credits {
    use nori_core::menus::{artist_line, ArtistLine};
    let link = |text: String, id: Option<String>| ArtistLink { text: text.into(), id: id.unwrap_or_default().into() };
    let model = |v: Vec<ArtistLink>| ModelRc::new(VecModel::from(v));
    match artist_line(s) {
        ArtistLine::One => Credits::default(),
        ArtistLine::Split(pieces) => Credits { pieces: model(pieces.into_iter().map(|p| link(p.text, p.id)).collect()), choices: ModelRc::default() },
        ArtistLine::Several(artists) => Credits { pieces: ModelRc::default(), choices: model(artists.into_iter().map(|a| link(a.name, Some(a.id))).collect()) },
    }
}

/// The heart, album and artists of `s`'s menu (menus.rs's `song_menu`), `starred` as its heart shows
/// (None: no heart).
fn song_menu(s: &Song, starred: Option<bool>) -> ModelRc<SongGo> {
    use nori_core::menus::{song_menu, SongAction, SongDownload};
    let lines: Vec<SongGo> = song_menu(s.clone(), starred.unwrap_or_default(), SongDownload::None, false, None)
        .into_iter()
        .filter(|item| starred.is_some() || !matches!(item.action, SongAction::Favourite { .. }))
        .filter_map(|item| {
            let title = words::song_action(&item.action)?.into();
            let (kind, id) = match item.action {
                SongAction::Favourite { .. } => (0, String::new()),
                SongAction::GoToAlbum { id } => (1, id),
                SongAction::GoToArtist { id, .. } => (2, id),
                _ => return None,
            };
            Some(SongGo { title, kind, id: id.into() })
        })
        .collect();
    ModelRc::new(VecModel::from(lines))
}

/// Upcoming songs in play order; each row carries its list index, and who asked for it in the jam (`by`).
fn queue_rows(v: &PlaylistView, by: impl Fn(&str) -> Option<String>, starred: impl Fn(&Song) -> Option<bool>) -> Vec<SongRow> {
    upcoming(v).map(|(i, s)| SongRow { by: by(&s.id).unwrap_or_default().into(), ..row(s, i as usize, false, starred(s)) }).collect()
}

/// The songs after the current one in play order, with their list indexes.
fn upcoming(v: &PlaylistView) -> impl Iterator<Item = (u32, &Song)> {
    let from = v.order.iter().position(|&i| i as i32 == v.index).map_or(0, |p| p + 1);
    v.order[from.min(v.order.len())..].iter().filter_map(|&i| v.songs.get(i as usize).map(|s| (i, s)))
}

/// Sets `m` to `rows`, updating in place when the length matches.
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

/// Updates `m` to `rows` in place: removes rows not in `rows`, inserts new ones marked fresh (they
/// animate open), keeps the rest. A reorder replaces the whole list.
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
    for (j, mut r) in rows.into_iter().enumerate() {
        match m.row_data(j) {
            Some(old) if old.index == r.index => {
                r.fresh = old.fresh;
                if old != r {
                    m.set_row_data(j, r);
                }
            }
            _ => {
                r.fresh = true;
                m.insert(j, r);
            }
        }
    }
}

/// The album of all `upcoming` songs, if they share one.
fn queue_from<'a>(upcoming: impl Iterator<Item = &'a Song>) -> String {
    let mut albums = upcoming.map(|s| s.album.as_str());
    match albums.next() {
        Some(first) if !first.is_empty() && albums.all(|a| a == first) => first.to_string(),
        _ => String::new(),
    }
}
