//! The screen's state and what input does to it. Nothing here waits or talks to the network: input
//! and answers come in as [`Msg`]s, and what has to happen outside - playing, reading, changing a
//! setting - goes out as [`Cmd`]s the runner carries out. So the whole of it runs in a test with no
//! server, no sound card and no terminal.

use std::time::{Duration, Instant};

use nori_core::client::Starrable;
use nori_core::playlist::PlaylistView;
use nori_core::search::SearchView;
use nori_core::settings::{EqLevel, SavedServer, SoundBand, StoredPrefs, TapAction};
use nori_core::settings_store::SoundTool;
use nori_core::{Album, AlbumDetail, Artist, ArtistDetail, OriginKind, PageOrigin, Playlist, PlaylistDetail, Song};
use nori_engine::{Event, State};
use nori_core::rules::equalizer_tuning;
use nori_look::cover::CoverColours;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::art::Theme;
use crate::backend::{Data, Downloads, Fetch, Msg, Req, ALBUM_PAGE, HOME_ROWS};
use crate::keys::{action, Action, Scope};
use crate::lyrics::SongLyrics;
use crate::settings_view::SettingsView;

/// The screens, in the order of the tabs and the number keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Library,
    Search,
    Queue,
    Playing,
    Lyrics,
    Downloads,
    Equalizer,
    Settings,
    /// The login form: not a tab, shown until a server is set up and whenever one is added.
    Login,
}

pub const SCREENS: [(Screen, &str); 9] = [
    (Screen::Home, "Home"),
    (Screen::Library, "Library"),
    (Screen::Search, "Search"),
    (Screen::Queue, "Queue"),
    (Screen::Playing, "Playing"),
    (Screen::Lyrics, "Lyrics"),
    (Screen::Downloads, "Downloads"),
    (Screen::Equalizer, "Equalizer"),
    (Screen::Settings, "Settings"),
];

pub const LIB_TABS: [&str; 4] = ["Albums", "Artists", "Playlists", "Songs"];

/// What the runner carries out.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Load(Req),
    /// `from`: the page the songs are that page's own list from (nori-queue `playlist_set`'s origin).
    Play { songs: Vec<Song>, start: usize, shuffle: bool, from: Option<PageOrigin> },
    PlayFetch(Fetch, bool),
    Enqueue(Vec<Song>, bool),
    EnqueueFetch(Fetch, bool),
    Toggle,
    Next,
    Previous,
    Seek(i64),
    Volume(f32),
    Jump(usize),
    Remove(usize),
    /// Undo: the song taken out last put back where it was (nori-queue `playlist_restore`).
    Restore(String),
    Move(usize, usize),
    Shuffle(bool),
    Repeat(u8),
    Download(Vec<Song>),
    DownloadFetch(Fetch),
    DownloadRemove(String),
    Star(Starrable, String, bool),
    Setting(String, String),
    Level(EqLevel, f32),
    Band(u32, SoundBand),
    /// One graphic equalizer slider, dB.
    Graphic(u32, f32),
    Sound(SoundToolCmd),
    Action(String),
    Mouse(bool),
    Images(bool),
    /// The output device for the next start, by name; empty for the system's own.
    Device(String),
    Tuning(bool),
    SearchTyped(String),
    SearchServer(String),
    Lyrics(String),
    /// A cover by its id (`cover_art`); `colours` works out the page's colours from it too.
    Cover { art: String, colours: bool },
    Login(SavedServer),
    SwitchServer(String),
    Quit,
}

impl Cmd {
    /// In a few words, for the debug log (no password).
    pub fn brief(&self) -> String {
        match self {
            Cmd::Load(r) => format!("load {r:?}"),
            Cmd::Play { songs, start, shuffle, .. } => format!("play {} songs from {start} shuffle={shuffle}", songs.len()),
            Cmd::Enqueue(songs, next) => format!("enqueue {} songs next={next}", songs.len()),
            Cmd::Download(songs) => format!("download {} songs", songs.len()),
            Cmd::Login(p) => format!("log in to {}", p.url),
            Cmd::Cover { art, colours } => format!("cover {art} colours={colours}"),
            Cmd::Lyrics(id) => format!("lyrics {id}"),
            Cmd::Toggle => "toggle".into(),
            Cmd::Next => "next".into(),
            Cmd::Previous => "previous".into(),
            Cmd::Seek(ms) => format!("seek {ms}"),
            Cmd::Volume(v) => format!("volume {v}"),
            Cmd::Jump(i) => format!("jump {i}"),
            Cmd::Shuffle(on) => format!("shuffle {on}"),
            Cmd::Repeat(m) => format!("repeat {m}"),
            Cmd::Setting(k, v) => format!("setting {k}={v}"),
            Cmd::Action(a) => format!("action {a}"),
            Cmd::Tuning(on) => format!("tuning {on}"),
            Cmd::Mouse(on) => format!("mouse {on}"),
            Cmd::Images(on) => format!("images {on}"),
            Cmd::Device(d) => format!("device {d}"),
            Cmd::Quit => "quit".into(),
            _ => "an edit".into(),
        }
    }
}

/// The equalizer's tools, as a command (SoundTool is not comparable).
#[derive(Debug, Clone, PartialEq)]
pub enum SoundToolCmd {
    Preset(usize),
    AutoPreamp(bool),
    AddBand,
    RemoveBand(u32),
    ResetBands,
}

impl SoundToolCmd {
    pub fn tool(&self) -> Option<SoundTool> {
        Some(match self {
            SoundToolCmd::Preset(i) => SoundTool::Preset { preset: nori_core::dsp::eq_presets().get(*i)?.clone() },
            SoundToolCmd::AutoPreamp(a) => SoundTool::AutoPreamp { automatic: *a },
            SoundToolCmd::AddBand => SoundTool::AddBand,
            SoundToolCmd::RemoveBand(i) => SoundTool::RemoveBand { index: *i },
            SoundToolCmd::ResetBands => SoundTool::ResetBands,
        })
    }
}

/// A list's selection and scroll.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Sel {
    pub at: usize,
    pub top: usize,
}

impl Sel {
    pub fn by(&mut self, d: isize, len: usize) {
        if len == 0 {
            self.at = 0;
            return;
        }
        self.at = (self.at as isize + d).clamp(0, len as isize - 1) as usize;
    }

    pub fn to(&mut self, i: usize, len: usize) {
        self.at = i.min(len.saturating_sub(1));
    }

    /// Scrolls so the selection shows in `height` rows.
    pub fn fit(&mut self, height: usize, len: usize) {
        if height == 0 {
            return;
        }
        self.at = self.at.min(len.saturating_sub(1));
        if self.at < self.top {
            self.top = self.at;
        } else if self.at >= self.top + height {
            self.top = self.at + 1 - height;
        }
        self.top = self.top.min(len.saturating_sub(height.min(len)));
    }
}

/// Something read from the server, on its way, or failed.
#[derive(Debug, Default)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(t) => Some(t),
            _ => None,
        }
    }
}

/// A page opened from a list: an album, an artist, a playlist.
pub enum Page {
    Album { id: String, detail: Load<Box<AlbumDetail>>, sel: Sel },
    Artist { id: String, detail: Load<Box<ArtistDetail>>, sel: Sel },
    Playlist { id: String, detail: Load<Box<PlaylistDetail>>, sel: Sel },
}

impl Page {
    pub fn req(&self) -> Req {
        match self {
            Page::Album { id, .. } => Req::Album(id.clone()),
            Page::Artist { id, .. } => Req::Artist(id.clone()),
            Page::Playlist { id, .. } => Req::Playlist(id.clone()),
        }
    }

    fn sel(&mut self) -> &mut Sel {
        match self {
            Page::Album { sel, .. } | Page::Artist { sel, .. } | Page::Playlist { sel, .. } => sel,
        }
    }

    fn len(&self) -> usize {
        match self {
            Page::Album { detail, .. } => detail.ready().map_or(0, |d| d.songs.len()),
            Page::Artist { detail, .. } => detail.ready().map_or(0, |d| d.albums.len()),
            Page::Playlist { detail, .. } => detail.ready().map_or(0, |d| d.songs.len()),
        }
    }

    /// The page's songs, when it lists songs.
    pub fn songs(&self) -> Option<&[Song]> {
        match self {
            Page::Album { detail, .. } => detail.ready().map(|d| d.songs.as_slice()),
            Page::Playlist { detail, .. } => detail.ready().map(|d| d.songs.as_slice()),
            Page::Artist { .. } => None,
        }
    }
}

/// What a list row is, for the keys that act on "the thing selected".
#[derive(Debug, Clone)]
pub enum Item {
    Song(Vec<Song>, usize),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
}

/// The home page: its shelves, each filled as it comes.
#[derive(Default)]
pub struct Home {
    pub rows: Vec<Option<(&'static str, Vec<Album>)>>,
    pub error: Option<String>,
    pub sel: Sel,
    pub asked: bool,
}

/// A row of the home page, flattened: a shelf's title or one of its albums.
pub enum HomeRow<'a> {
    Title(&'a str),
    Album(&'a Album),
}

impl Home {
    pub fn flat(&self) -> Vec<HomeRow<'_>> {
        let mut out = Vec::new();
        for (title, albums) in self.rows.iter().flatten() {
            if albums.is_empty() {
                continue;
            }
            out.push(HomeRow::Title(title));
            out.extend(albums.iter().map(HomeRow::Album));
        }
        out
    }
}

#[derive(Default)]
pub struct Library {
    pub tab: usize,
    pub albums: Load<Vec<Album>>,
    pub albums_more: bool,
    pub artists: Load<Vec<Artist>>,
    pub playlists: Load<Vec<Playlist>>,
    pub songs: Load<Vec<Song>>,
    pub songs_more: bool,
    pub sels: [Sel; 4],
}

impl Library {
    pub fn len(&self, tab: usize) -> usize {
        match tab {
            0 => self.albums.ready().map_or(0, Vec::len),
            1 => self.artists.ready().map_or(0, Vec::len),
            2 => self.playlists.ready().map_or(0, Vec::len),
            _ => self.songs.ready().map_or(0, Vec::len),
        }
    }
}

#[derive(Default)]
pub struct Search {
    pub text: String,
    pub editing: bool,
    pub view: Option<SearchView>,
    /// 0 artists, 1 albums, 2 songs.
    pub pane: usize,
    pub sels: [Sel; 3],
    /// When the server is asked, once typing has paused.
    pub ask_at: Option<Instant>,
}

impl Search {
    pub fn len(&self, pane: usize) -> usize {
        let Some(r) = self.view.as_ref().and_then(|v| v.shown.as_ref()) else { return 0 };
        match pane {
            0 => r.artists.len(),
            1 => r.albums.len(),
            _ => r.songs.len(),
        }
    }
}

/// The login form.
#[derive(Default)]
pub struct Login {
    /// Name, address, user, password.
    pub fields: [String; 4],
    pub focus: usize,
    pub error: Option<String>,
    pub busy: bool,
    /// The saved servers, to pick one instead.
    pub sel: Sel,
    pub on_list: bool,
}

pub const LOGIN_FIELDS: [&str; 4] = ["Name (optional)", "Server address", "User", "Password"];

/// The engine's state as the screen needs it, copied without allocating.
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub state: State,
    pub position_ms: i64,
    pub at: Instant,
    pub speed: f32,
    pub mixing: bool,
    pub buffering: bool,
}

impl Default for Now {
    fn default() -> Self {
        Now { state: State::Idle, position_ms: 0, at: Instant::now(), speed: 1.0, mixing: false, buffering: false }
    }
}

impl Now {
    pub fn position(&self, now: Instant) -> i64 {
        match self.state {
            State::Playing if !self.buffering => self.position_ms + (now.saturating_duration_since(self.at).as_secs_f64() * 1000.0 * self.speed as f64) as i64,
            _ => self.position_ms,
        }
    }
}

/// On top of the screen.
pub enum Overlay {
    Help { scroll: usize },
    /// A list of choices; `name` is the setting it sets (or a tool).
    Picker { title: String, options: Vec<(String, String)>, sel: Sel, name: String },
    /// Text typed in for a setting.
    Input { title: String, text: String, secret: bool, name: String },
}

/// Where a click lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    LibTab(usize),
    /// A row of a list, by its index in that list.
    Row(ListRef, usize),
    /// A list's area, for the wheel.
    List(ListRef),
    Seek,
    Button(Button),
    SearchField,
    LoginField(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListRef {
    Home,
    Library,
    Page,
    Search(usize),
    Queue,
    Lyrics,
    Downloads,
    Groups,
    Rows,
    Eq,
    Profiles,
    Picker,
    Help,
    UpNext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Previous,
    Toggle,
    Next,
    Shuffle,
    Repeat,
    VolumeDown,
    VolumeUp,
    PlayAll,
    ShuffleAll,
    Back,
    Help,
    Connect,
}

pub struct App {
    pub screen: Screen,
    /// Pages opened, each over the screen it was opened from.
    pub pages: Vec<(Screen, Page)>,
    pub home: Home,
    pub library: Library,
    pub search: Search,
    pub queue: Option<PlaylistView>,
    pub queue_sel: Sel,
    /// The id of the song last taken out of the queue here, for `u` to put back.
    pub taken: Option<String>,
    pub up_next_sel: Sel,
    pub downloads: Load<Box<Downloads>>,
    pub downloads_sel: Sel,
    pub downloads_at: Option<Instant>,
    pub settings: SettingsView,
    pub eq_sel: Sel,
    pub login: Login,
    pub lyrics: Option<SongLyrics>,
    pub lyrics_for: Option<String>,
    pub lyrics_sel: Option<usize>,
    pub lyrics_wake: Option<Instant>,
    pub overlay: Option<Overlay>,
    pub now: Now,
    /// The song heard, as the engine says (through a mix, the one the ear is on).
    pub song: Option<Song>,
    pub transition: Option<nori_core::automix::planner::TransitionNote>,
    /// The mix the ear is in, once it is on the incoming song.
    pub mixed_in: Option<nori_core::automix::planner::TransitionNote>,
    /// The heard song's cover, by id.
    pub cover_art: Option<String>,
    pub colours: Option<Box<CoverColours>>,
    pub theme: Theme,
    pub prefs: StoredPrefs,
    pub volume: f32,
    pub mouse: bool,
    pub images: bool,
    /// The image protocol in use, in words, for the settings page.
    pub protocol: &'static str,
    pub server: String,
    pub offline: bool,
    pub unreachable: Option<String>,
    pub note: Option<(String, bool, Instant)>,
    pub hits: Vec<(Rect, Hit)>,
    pub cmds: Vec<Cmd>,
    pub dirty: bool,
    pub quit: bool,
    /// The engine was asked for the equalizer's shallow buffer ([`App::sound_edited`]): asked back when
    /// the equalizer screen closes or the equalizer is switched off.
    pub tuning: bool,
    /// The sound was changed on the equalizer screen since it was opened.
    pub touched: bool,
    /// A drag on the seek bar: where it is, as a share of the song.
    pub scrub: Option<f32>,
    pub seek_rect: Rect,
    /// The last left click, for a second click on the same row.
    last_click: Option<(Hit, Instant)>,
}

impl App {
    pub fn new(prefs: StoredPrefs) -> App {
        App {
            screen: Screen::Home,
            pages: Vec::new(),
            home: Home::default(),
            library: Library::default(),
            search: Search::default(),
            queue: None,
            queue_sel: Sel::default(),
            taken: None,
            up_next_sel: Sel::default(),
            downloads: Load::Idle,
            downloads_sel: Sel::default(),
            downloads_at: None,
            settings: SettingsView::default(),
            eq_sel: Sel::default(),
            login: Login::default(),
            lyrics: None,
            lyrics_for: None,
            lyrics_sel: None,
            lyrics_wake: None,
            overlay: None,
            now: Now::default(),
            song: None,
            transition: None,
            mixed_in: None,
            cover_art: None,
            colours: None,
            theme: Theme::plain(prefs.accent),
            prefs,
            volume: 1.0,
            mouse: true,
            images: true,
            protocol: "none",
            server: String::new(),
            offline: false,
            unreachable: None,
            note: None,
            hits: Vec::new(),
            cmds: Vec::new(),
            dirty: true,
            quit: false,
            tuning: false,
            touched: false,
            scrub: None,
            seek_rect: Rect::default(),
            last_click: None,
        }
    }

    pub fn say(&mut self, text: impl Into<String>, error: bool) {
        self.note = Some((text.into(), error, Instant::now()));
        self.dirty = true;
    }

    /// The settings as the core keeps them now, and whatever follows from them on screen.
    pub fn prefs_changed(&mut self, prefs: StoredPrefs) {
        self.prefs = prefs;
        // The equalizer switched off gives the shallow buffer back.
        self.tune();
        self.retheme();
        self.settings.invalidate();
        self.dirty = true;
    }

    fn retheme(&mut self) {
        self.theme = match &self.colours {
            Some(c) if self.prefs.cover_colors && self.images => Theme::from_cover(c),
            _ => Theme::plain(self.prefs.accent),
        };
    }

    // ---- going places ----

    pub fn go(&mut self, screen: Screen) {
        if screen != Screen::Equalizer {
            self.touched = false;
        }
        self.screen = screen;
        self.tune();
        self.dirty = true;
        match screen {
            Screen::Home if !self.home.asked => {
                self.home.asked = true;
                self.home.rows = (0..HOME_ROWS.len()).map(|_| None).collect();
                self.cmds.push(Cmd::Load(Req::Home));
            }
            Screen::Library => self.library_tab(self.library.tab),
            Screen::Downloads => self.cmds.push(Cmd::Load(Req::Downloads)),
            Screen::Settings => {
                if !self.settings.facts_asked {
                    self.settings.facts_asked = true;
                    self.cmds.push(Cmd::Load(Req::Facts));
                }
            }
            // The queue opens on the song playing.
            Screen::Queue => {
                let current = self.queue.as_ref().map_or(-1, |q| q.index);
                if let Some(row) = self.queue_order().iter().position(|&i| i as i32 == current) {
                    self.queue_sel.at = row;
                }
            }
            Screen::Lyrics => self.want_lyrics(),
            Screen::Search if self.search.view.is_none() => self.search.editing = true,
            _ => {}
        }
    }

    fn library_tab(&mut self, tab: usize) {
        self.library.tab = tab;
        self.dirty = true;
        let l = &mut self.library;
        let asked = match tab {
            0 => matches!(l.albums, Load::Idle).then(|| {
                l.albums = Load::Loading;
                Req::Albums { offset: 0 }
            }),
            1 => matches!(l.artists, Load::Idle).then(|| {
                l.artists = Load::Loading;
                Req::Artists
            }),
            2 => matches!(l.playlists, Load::Idle).then(|| {
                l.playlists = Load::Loading;
                Req::Playlists
            }),
            _ => matches!(l.songs, Load::Idle).then(|| {
                l.songs = Load::Loading;
                Req::Songs { offset: 0 }
            }),
        };
        if let Some(r) = asked {
            self.cmds.push(Cmd::Load(r));
        }
    }

    fn open_page(&mut self, page: Page) {
        self.cmds.push(Cmd::Load(page.req()));
        self.pages.push((self.screen, page));
        self.dirty = true;
    }

    pub fn open_album(&mut self, id: String) {
        self.open_page(Page::Album { id, detail: Load::Loading, sel: Sel::default() });
    }

    fn open_artist(&mut self, id: String) {
        self.open_page(Page::Artist { id, detail: Load::Loading, sel: Sel::default() });
    }

    pub(crate) fn open_playlist(&mut self, id: String) {
        self.open_page(Page::Playlist { id, detail: Load::Loading, sel: Sel::default() });
    }

    /// Pages open over the screens that list things.
    pub fn page_shown(&self) -> bool {
        self.page().is_some()
    }

    /// The page open over this screen, if one is.
    pub fn page(&self) -> Option<&Page> {
        let screen = self.screen;
        self.pages.iter().rev().find(|(s, _)| *s == screen).map(|(_, p)| p)
    }

    fn page_mut(&mut self) -> Option<&mut Page> {
        let screen = self.screen;
        self.pages.iter_mut().rev().find(|(s, _)| *s == screen).map(|(_, p)| p)
    }

    fn pop_page(&mut self) -> bool {
        let screen = self.screen;
        match self.pages.iter().rposition(|(s, _)| *s == screen) {
            Some(i) => {
                self.pages.remove(i);
                true
            }
            None => false,
        }
    }

    fn clear_pages(&mut self) {
        let screen = self.screen;
        self.pages.retain(|(s, _)| *s != screen);
    }

    fn want_lyrics(&mut self) {
        let Some(id) = self.song.as_ref().map(|s| s.id.clone()) else { return };
        if self.lyrics_for.as_deref() != Some(&id) {
            self.lyrics_for = Some(id.clone());
            self.lyrics = None;
            self.lyrics_sel = None;
            self.cmds.push(Cmd::Lyrics(id));
        }
        self.lyrics_wake = Some(Instant::now());
    }

    // ---- what comes in ----

    pub fn handle(&mut self, msg: Msg) {
        // A message that changes nothing on screen says so by clearing `dirty`; one taken before it in
        // the same batch (an engine event, then a key that does nothing) is still drawn.
        let was = std::mem::replace(&mut self.dirty, true);
        self.take(msg);
        self.dirty |= was;
    }

    fn take(&mut self, msg: Msg) {
        match msg {
            Msg::Key(k) => self.key(k),
            Msg::Mouse(m) => self.mouse_event(m),
            Msg::Paste(text) => self.paste(&text),
            Msg::Resize => {}
            // Focus lost changes nothing on screen; focus back is drawn (the runner decides how fully).
            Msg::Focus(on) => self.dirty = on,
            Msg::Engine(e) => self.engine(e),
            Msg::Data(req, r) => self.data(req, r),
            Msg::Cover { art, colours, .. } => {
                if let Some(c) = colours {
                    if self.cover_art.as_deref() == Some(&art) {
                        self.colours = Some(c);
                        self.retheme();
                    }
                }
            }
            Msg::Lyrics { song, pick } => {
                if self.lyrics_for.as_deref() == Some(&song) {
                    if self.lyrics.as_ref().is_none_or(|l| l.replaced_by(&pick)) {
                        let pos = self.now.position(Instant::now());
                        self.lyrics = Some(SongLyrics::new(pick, pos));
                        self.lyrics_wake = Some(Instant::now());
                    }
                }
            }
            Msg::Search(v) => {
                if v.query == self.search.text.trim() {
                    self.search.view = Some(v);
                }
            }
            Msg::Note { text, error } => self.say(text, error),
            Msg::LoggedIn(r) => {
                self.login.busy = false;
                if let Err(e) = r {
                    self.login.error = Some(e);
                }
            }
            Msg::Reachable(r) => self.unreachable = r.err(),
        }
    }

    fn engine(&mut self, e: Event) {
        match e {
            Event::State(s) => {
                // The clock stops or starts where it is now: the engine's status, read by the runner,
                // may still be the one from before this event.
                let t = Instant::now();
                self.now.position_ms = self.now.position(t);
                self.now.at = t;
                self.now.state = s;
                if s == State::Playing {
                    self.lyrics_wake = Some(Instant::now());
                }
            }
            Event::Song { .. } | Event::Looped { .. } => {}
            Event::Error { id, message } => {
                let what = nori_core::queue::queue_song(id).map_or_else(|| "The output".to_string(), |s| format!("“{}”", s.title));
                self.say(format!("{what} would not play: {message}"), true);
            }
            Event::Buffering(on) => self.now.buffering = on,
            Event::Stopped => self.say("Playback stopped: too many songs in a row would not play", true),
            Event::Output { name } => self.say(format!("Playing on {name}"), false),
            Event::Title(t) => self.say(format!("On air: {t}"), false),
            Event::Bridge => self.say("The network is gone", true),
            Event::Mixing(on) => self.now.mixing = on,
            // A desktop keeps no wake lock: the system does not sleep under playing music.
            Event::Position { .. } | Event::Placed { .. } | Event::Awake(_) => {}
        }
    }

    /// The song heard changed (the runner read it from the engine): the cover, the lyrics and the plan
    /// follow it.
    pub fn heard(&mut self, song: Option<Song>) {
        let changed = self.song.as_ref().map(|s| &s.id) != song.as_ref().map(|s| &s.id);
        self.song = song;
        if !changed {
            return;
        }
        self.transition = None;
        let art = self.song.as_ref().and_then(|s| s.cover_art.clone());
        if art != self.cover_art {
            self.colours = None;
            self.retheme();
        }
        self.cover_art = art.clone();
        if let Some(art) = art {
            self.cmds.push(Cmd::Cover { art, colours: true });
        }
        if self.screen == Screen::Lyrics {
            self.want_lyrics();
        } else {
            self.lyrics = None;
            self.lyrics_for = None;
        }
    }

    fn data(&mut self, req: Req, r: Result<Data, String>) {
        match (req, r) {
            (Req::Home, Ok(Data::HomeRow(i, title, albums))) => {
                if let Some(slot) = self.home.rows.get_mut(i) {
                    *slot = Some((title, albums));
                }
                self.home.error = None;
            }
            (Req::Home, Err(e)) => self.home.error = Some(e),
            (Req::Albums { offset }, Ok(Data::Albums(v))) => {
                self.library.albums_more = v.len() as u32 == ALBUM_PAGE;
                match &mut self.library.albums {
                    Load::Ready(have) if offset > 0 => {
                        have.truncate(offset as usize);
                        have.extend(v);
                    }
                    slot => *slot = Load::Ready(v),
                }
            }
            (Req::Albums { .. }, Err(e)) => self.library.albums = Load::Failed(e),
            (Req::Artists, Ok(Data::Artists(v))) => self.library.artists = Load::Ready(v),
            (Req::Artists, Err(e)) => self.library.artists = Load::Failed(e),
            (Req::Playlists, Ok(Data::Playlists(v))) => self.library.playlists = Load::Ready(v),
            (Req::Playlists, Err(e)) => self.library.playlists = Load::Failed(e),
            (Req::Songs { offset }, Ok(Data::Songs(v, exhausted))) => {
                self.library.songs_more = !exhausted;
                match &mut self.library.songs {
                    Load::Ready(have) if offset > 0 => {
                        have.truncate(offset as usize);
                        have.extend(v);
                    }
                    slot => *slot = Load::Ready(v),
                }
            }
            (Req::Songs { .. }, Err(e)) => self.library.songs = Load::Failed(e),
            (Req::Downloads, Ok(Data::Downloads(d))) => {
                // While something downloads, the page looks again every second (only while it is shown).
                let running = !d.active.is_empty() || !d.queued.is_empty();
                self.downloads_at = running.then(|| Instant::now() + Duration::from_secs(1));
                self.downloads = Load::Ready(d);
            }
            (Req::Downloads, Err(e)) => self.downloads = Load::Failed(e),
            (Req::Facts, Ok(Data::Facts(f))) => self.settings.set_facts(*f),
            (Req::Facts, Err(_)) => {}
            (req, r) => {
                for (_, page) in self.pages.iter_mut().rev() {
                    if page.req() != req {
                        continue;
                    }
                    match (page, r) {
                        (Page::Album { detail, .. }, Ok(Data::Album(d))) => {
                            if let Some(art) = d.album.cover_art.clone() {
                                self.cmds.push(Cmd::Cover { art, colours: false });
                            }
                            *detail = Load::Ready(d);
                        }
                        (Page::Artist { detail, .. }, Ok(Data::Artist(d))) => *detail = Load::Ready(d),
                        (Page::Playlist { detail, .. }, Ok(Data::Playlist(d))) => *detail = Load::Ready(d),
                        (Page::Album { detail, .. }, Err(e)) => *detail = Load::Failed(e),
                        (Page::Artist { detail, .. }, Err(e)) => *detail = Load::Failed(e),
                        (Page::Playlist { detail, .. }, Err(e)) => *detail = Load::Failed(e),
                        _ => {}
                    }
                    break;
                }
            }
        }
    }

    // ---- time ----

    /// When the screen next has to look again by itself, if ever: the clock's next second while music
    /// plays, the lyrics' next change, a paused search, a note to clear, downloads running. None while
    /// nothing moves: the program then sleeps until a key, a click or the engine wakes it.
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let mut at: Option<Instant> = None;
        let mut sooner = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        if self.now.state == State::Playing && !self.now.buffering {
            // The next whole second of the song, when the times on screen change.
            let pos = self.now.position(now).max(0);
            let left = 1000 - pos % 1000;
            sooner(now + Duration::from_millis((left as f32 / self.now.speed.max(0.1)) as u64 + 5));
            if self.screen == Screen::Lyrics {
                if let Some(t) = self.lyrics_wake {
                    sooner(t);
                }
            }
        }
        if let Some(t) = self.search.ask_at {
            sooner(t);
        }
        if let Some((_, _, since)) = &self.note {
            sooner(*since + NOTE_FOR);
        }
        if let Some(t) = self.downloads_at {
            sooner(t);
        }
        at
    }

    /// Time passed: whatever was due is done.
    pub fn tick(&mut self, now: Instant) {
        self.dirty = true;
        if self.search.ask_at.is_some_and(|t| t <= now) {
            self.search.ask_at = None;
            self.cmds.push(Cmd::SearchServer(self.search.text.trim().to_string()));
        }
        if self.note.as_ref().is_some_and(|(_, _, since)| *since + NOTE_FOR <= now) {
            self.note = None;
        }
        if self.downloads_at.is_some_and(|t| t <= now) {
            self.downloads_at = None;
            if self.screen == Screen::Downloads {
                self.cmds.push(Cmd::Load(Req::Downloads));
            }
        }
    }

    // ---- keys ----

    fn scopes(&self) -> &'static [Scope] {
        match self.screen {
            Screen::Queue if !self.page_shown() => &[Scope::Queue, Scope::List, Scope::Global],
            Screen::Downloads => &[Scope::Queue, Scope::List, Scope::Global],
            Screen::Lyrics => &[Scope::Lyrics, Scope::List, Scope::Global],
            Screen::Equalizer => &[Scope::Values, Scope::Queue, Scope::List, Scope::Global],
            Screen::Settings if self.settings.pane == 1 && self.settings.adjustable() => &[Scope::Values, Scope::List, Scope::Global],
            _ => &[Scope::List, Scope::Global],
        }
    }

    pub fn key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            self.dirty = false;
            return;
        }
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            self.do_action(Action::Quit);
            return;
        }
        if self.overlay.is_some() {
            self.overlay_key(k);
            return;
        }
        if self.screen == Screen::Login {
            self.login_key(k);
            return;
        }
        if self.screen == Screen::Search && self.search.editing {
            self.search_key(k);
            return;
        }
        if let Some(a) = action(&k, self.scopes()) {
            self.do_action(a);
        } else {
            self.dirty = false;
        }
    }

    fn paste(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if let Some(Overlay::Input { text: t, .. }) = &mut self.overlay {
            t.push_str(&text);
        } else if self.screen == Screen::Login {
            self.login.fields[self.login.focus].push_str(&text);
        } else if self.screen == Screen::Search {
            self.search.editing = true;
            self.search.text.push_str(&text);
            self.typed();
        }
    }

    fn typed(&mut self) {
        self.cmds.push(Cmd::SearchTyped(self.search.text.clone()));
        let delay = self.prefs.live_search_delay_ms.clamp(100, 2000) as u64;
        self.search.ask_at = (!self.search.text.trim().is_empty()).then(|| Instant::now() + Duration::from_millis(delay));
        self.search.sels = Default::default();
    }

    fn search_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => self.search.editing = false,
            KeyCode::Enter | KeyCode::Down | KeyCode::Tab => {
                self.search.editing = false;
                // The query acted on is remembered, and the server asked now rather than after the pause.
                if k.code == KeyCode::Enter && !self.search.text.trim().is_empty() {
                    self.search.ask_at = None;
                    self.cmds.push(Cmd::SearchServer(self.search.text.trim().to_string()));
                }
                // The first pane with something in it.
                if let Some(p) = (0..3).rev().find(|p| self.search.len(*p) > 0) {
                    self.search.pane = if self.search.len(2) > 0 { 2 } else { p };
                }
            }
            KeyCode::Backspace => {
                self.search.text.pop();
                self.typed();
            }
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.text.clear();
                self.typed();
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.text.push(c);
                self.typed();
            }
            _ => self.dirty = false,
        }
    }

    fn login_key(&mut self, k: KeyEvent) {
        let l = &mut self.login;
        let profiles = self.prefs.servers.len();
        if l.on_list {
            match k.code {
                KeyCode::Up | KeyCode::Char('k') => l.sel.by(-1, profiles),
                KeyCode::Down | KeyCode::Char('j') => l.sel.by(1, profiles),
                KeyCode::Tab | KeyCode::Esc => l.on_list = false,
                KeyCode::Enter => {
                    if let Some(s) = self.prefs.servers.get(l.sel.at) {
                        self.cmds.push(Cmd::SwitchServer(s.id.clone()));
                    }
                }
                KeyCode::Char('q') => self.quit = true,
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Tab | KeyCode::Down => {
                if l.focus == 3 && profiles > 0 && k.code == KeyCode::Tab {
                    l.on_list = true;
                } else {
                    l.focus = (l.focus + 1) % 4;
                }
            }
            KeyCode::BackTab | KeyCode::Up => l.focus = (l.focus + 3) % 4,
            KeyCode::Enter if l.focus < 3 => l.focus += 1,
            KeyCode::Enter => self.connect(),
            KeyCode::Esc => {
                if !self.prefs.servers.is_empty() {
                    self.screen = Screen::Settings;
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Backspace => {
                l.fields[l.focus].pop();
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => l.fields[l.focus].push(c),
            _ => {}
        }
    }

    fn connect(&mut self) {
        let l = &mut self.login;
        let [name, url, user, password] = l.fields.clone();
        let url = url.trim().trim_end_matches('/').to_string();
        if url.is_empty() || user.trim().is_empty() {
            l.error = Some("An address and a user are needed".into());
            return;
        }
        let url = if url.contains("://") { url } else { format!("https://{url}") };
        l.error = None;
        l.busy = true;
        let draft = SavedServer { id: nori_core::settings::new_server_id(), name: name.trim().to_string(), url, user: user.trim().to_string(), password, ..Default::default() };
        self.cmds.push(Cmd::Login(draft));
    }

    fn overlay_key(&mut self, k: KeyEvent) {
        let Some(o) = &mut self.overlay else { return };
        match o {
            Overlay::Help { scroll } => match k.code {
                KeyCode::Down | KeyCode::Char('j') => *scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => *scroll += 10,
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                _ => self.overlay = None,
            },
            Overlay::Picker { options, sel, name, .. } => match k.code {
                KeyCode::Down | KeyCode::Char('j') => sel.by(1, options.len()),
                KeyCode::Up | KeyCode::Char('k') => sel.by(-1, options.len()),
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char(' ') => {
                    if let Some((_, value)) = options.get(sel.at) {
                        let (name, value) = (name.clone(), value.clone());
                        self.overlay = None;
                        self.pick(&name, &value);
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => self.overlay = None,
                _ => {}
            },
            Overlay::Input { text, name, .. } => match k.code {
                KeyCode::Enter => {
                    let (name, value) = (name.clone(), text.clone());
                    self.overlay = None;
                    self.cmds.push(Cmd::Setting(name, value));
                }
                KeyCode::Esc => self.overlay = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
                _ => {}
            },
        }
    }

    /// A choice made in a picker.
    fn pick(&mut self, name: &str, value: &str) {
        match name {
            "!preset" => {
                if let Ok(i) = value.parse() {
                    self.cmds.push(Cmd::Sound(SoundToolCmd::Preset(i)));
                }
            }
            "!device" => self.cmds.push(Cmd::Device(value.to_string())),
            _ => self.cmds.push(Cmd::Setting(name.to_string(), value.to_string())),
        }
    }

    pub fn do_action(&mut self, a: Action) {
        let now = Instant::now();
        match a {
            Action::Quit => {
                self.quit = true;
                self.cmds.push(Cmd::Quit);
            }
            Action::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            Action::TogglePlay => self.cmds.push(Cmd::Toggle),
            Action::Next => self.cmds.push(Cmd::Next),
            Action::Previous => self.cmds.push(Cmd::Previous),
            Action::SeekBack => self.seek_by(-5_000, now),
            Action::SeekForward => self.seek_by(5_000, now),
            Action::SeekBackLong => self.seek_by(-30_000, now),
            Action::SeekForwardLong => self.seek_by(30_000, now),
            Action::VolumeUp => self.set_volume(self.volume + 0.05),
            Action::VolumeDown => self.set_volume(self.volume - 0.05),
            Action::Search => {
                self.go(Screen::Search);
                self.search.editing = true;
            }
            Action::Mouse => {
                self.mouse = !self.mouse;
                self.cmds.push(Cmd::Mouse(self.mouse));
                self.say(if self.mouse { "Mouse on" } else { "Mouse off: the terminal selects text" }, false);
            }
            Action::Images => {
                self.images = !self.images;
                self.retheme();
                self.cmds.push(Cmd::Images(self.images));
            }
            Action::Shuffle => {
                let on = !self.queue_shuffled();
                self.cmds.push(Cmd::Shuffle(on));
                self.say(if on { "Shuffle on" } else { "Shuffle off" }, false);
            }
            Action::Repeat => {
                use nori_player_repeat::*;
                let next = match self.repeat() {
                    OFF => ALL,
                    ALL => ONE,
                    _ => OFF,
                };
                self.cmds.push(Cmd::Repeat(next));
                self.say(["Repeat off", "Repeat one", "Repeat all"][next as usize], false);
            }
            Action::Screen(n) => {
                if let Some((s, _)) = SCREENS.get(n as usize) {
                    if self.screen == *s {
                        self.clear_pages();
                    }
                    self.go(*s);
                }
            }
            Action::NextScreen | Action::PreviousScreen => {
                let at = SCREENS.iter().position(|(s, _)| *s == self.screen).unwrap_or(0);
                let n = SCREENS.len();
                let to = if a == Action::NextScreen { (at + 1) % n } else { (at + n - 1) % n };
                self.go(SCREENS[to].0);
            }
            Action::Back => self.back(),
            Action::Refresh => self.refresh(),
            Action::NextPane | Action::PreviousPane => self.pane(a == Action::NextPane),
            Action::TabLeft | Action::TabRight if self.screen == Screen::Library && !self.page_shown() => {
                let t = if a == Action::TabRight { (self.library.tab + 1) % 4 } else { (self.library.tab + 3) % 4 };
                self.library_tab(t);
            }
            Action::Sooner | Action::Later | Action::Unnudge => {
                if let Some(l) = &self.lyrics {
                    let dir = match a {
                        Action::Sooner => 1,
                        Action::Later => -1,
                        _ => 0,
                    };
                    let ms = l.clock.nudge(dir);
                    self.lyrics_wake = Some(now);
                    self.say(if ms == 0 { "Lyrics on their own timing".to_string() } else { format!("Lyrics {}", crate::text::nudge(ms)) }, false);
                }
            }
            Action::Decrease | Action::Increase => {
                let up = a == Action::Increase;
                match self.screen {
                    Screen::Equalizer => self.eq_step(up),
                    Screen::Settings => {
                        let cmds = self.settings.step(&self.prefs, up);
                        self.cmds.extend(cmds);
                    }
                    _ => {}
                }
            }
            Action::Remove if self.screen == Screen::Equalizer => {
                if let Some(crate::settings_view::EqRow::Band(i)) = crate::settings_view::eq_rows(&self.prefs).get(self.eq_sel.at) {
                    self.cmds.push(Cmd::Sound(SoundToolCmd::RemoveBand(*i as u32)));
                }
            }
            Action::Remove if self.screen == Screen::Downloads => {
                if let Some(Item::Song(songs, i)) = self.selected() {
                    self.cmds.push(Cmd::DownloadRemove(songs[i].id.clone()));
                }
            }
            Action::Remove => {
                if let Some(i) = self.queue_selected_index() {
                    self.taken = self.queue.as_ref().and_then(|q| q.songs.get(i)).map(|s| s.id.clone());
                    self.cmds.push(Cmd::Remove(i));
                }
            }
            Action::Undo if self.screen == Screen::Queue => match self.taken.take() {
                Some(id) => self.cmds.push(Cmd::Restore(id)),
                None => self.say("Nothing to put back", false),
            },
            Action::Undo => self.dirty = false,
            Action::MoveUp | Action::MoveDown if matches!(self.screen, Screen::Downloads | Screen::Equalizer) => self.dirty = false,
            Action::MoveUp | Action::MoveDown => self.queue_move(a == Action::MoveDown),
            _ => self.list_action(a),
        }
    }

    fn seek_by(&mut self, delta: i64, now: Instant) {
        let Some(song) = &self.song else { return };
        let len = song.duration as i64 * 1000;
        let to = (self.now.position(now) + delta).clamp(0, (len - 1000).max(0));
        self.now.position_ms = to;
        self.now.at = now;
        self.cmds.push(Cmd::Seek(to));
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = (v * 20.0).round().clamp(0.0, 20.0) / 20.0;
        self.cmds.push(Cmd::Volume(self.volume));
    }

    pub fn queue_shuffled(&self) -> bool {
        self.queue.as_ref().is_some_and(|q| q.shuffle)
    }

    pub fn repeat(&self) -> u8 {
        self.queue.as_ref().map_or(0, |q| q.repeat)
    }

    fn back(&mut self) {
        if self.pop_page() {
            return;
        }
        match self.screen {
            Screen::Settings if self.settings.pane == 1 => self.settings.pane = 0,
            Screen::Search => self.search.editing = true,
            _ => self.dirty = false,
        }
    }

    fn refresh(&mut self) {
        if let Some(p) = self.page() {
            self.cmds.push(Cmd::Load(p.req()));
            return;
        }
        match self.screen {
            Screen::Home => {
                self.home.asked = false;
                self.go(Screen::Home);
            }
            Screen::Library => {
                let l = &mut self.library;
                match l.tab {
                    0 => l.albums = Load::Idle,
                    1 => l.artists = Load::Idle,
                    2 => l.playlists = Load::Idle,
                    _ => l.songs = Load::Idle,
                }
                self.library_tab(self.library.tab);
            }
            Screen::Downloads => self.cmds.push(Cmd::Load(Req::Downloads)),
            Screen::Settings => self.cmds.push(Cmd::Load(Req::Facts)),
            Screen::Lyrics => {
                self.lyrics_for = None;
                self.want_lyrics();
            }
            _ => {}
        }
    }

    fn pane(&mut self, forward: bool) {
        match self.screen {
            Screen::Search if !self.page_shown() => {
                self.search.pane = if forward { (self.search.pane + 1) % 3 } else { (self.search.pane + 2) % 3 };
            }
            Screen::Settings => self.settings.pane = 1 - self.settings.pane,
            Screen::Library if !self.page_shown() => {
                let t = if forward { (self.library.tab + 1) % 4 } else { (self.library.tab + 3) % 4 };
                self.library_tab(t);
            }
            Screen::Home if !self.page_shown() => {
                // To the next shelf's first album.
                let flat = self.home.flat();
                let at = self.home.sel.at;
                let titles: Vec<usize> = flat.iter().enumerate().filter(|(_, r)| matches!(r, HomeRow::Title(_))).map(|(i, _)| i + 1).collect();
                let next = if forward { titles.iter().find(|&&t| t > at).or(titles.first()) } else { titles.iter().rev().find(|&&t| t < at).or(titles.last()) };
                if let Some(&t) = next {
                    self.home.sel.at = t;
                }
            }
            _ => self.dirty = false,
        }
    }

    // ---- lists ----

    /// The list the keys move in now: its selection and its length.
    fn list(&mut self) -> Option<(&mut Sel, usize)> {
        if self.page_shown() {
            let p = self.page_mut()?;
            let len = p.len();
            return Some((p.sel(), len));
        }
        Some(match self.screen {
            Screen::Home => {
                let len = self.home.flat().len();
                (&mut self.home.sel, len)
            }
            Screen::Library => {
                let t = self.library.tab;
                let len = self.library.len(t);
                (&mut self.library.sels[t], len)
            }
            Screen::Search => {
                let p = self.search.pane;
                let len = self.search.len(p);
                (&mut self.search.sels[p], len)
            }
            Screen::Queue => {
                let len = self.queue.as_ref().map_or(0, |q| q.len as usize);
                (&mut self.queue_sel, len)
            }
            Screen::Playing => {
                let len = self.up_next().len();
                (&mut self.up_next_sel, len)
            }
            Screen::Downloads => {
                let len = self.download_rows().len();
                (&mut self.downloads_sel, len)
            }
            Screen::Equalizer => (&mut self.eq_sel, crate::settings_view::eq_rows(&self.prefs).len()),
            Screen::Settings => return Some(self.settings.list()),
            Screen::Lyrics => return None,
            Screen::Login => return None,
        })
    }

    fn list_action(&mut self, a: Action) {
        if self.screen == Screen::Lyrics {
            return self.lyrics_action(a);
        }
        let Some((sel, len)) = self.list() else { return };
        let page = 10;
        match a {
            Action::Up => sel.by(-1, len),
            Action::Down => sel.by(1, len),
            Action::Top => sel.to(0, len),
            Action::Bottom => sel.to(len.saturating_sub(1), len),
            Action::PageUp => sel.by(-page, len),
            Action::PageDown => sel.by(page, len),
            _ => return self.act_on_selected(a),
        }
        // A settings page never rests on a section's title.
        if self.screen == Screen::Settings && self.settings.pane == 1 {
            self.settings.skip_titles(!matches!(a, Action::Up | Action::PageUp | Action::Top));
        }
        // The home page never rests on a shelf's title.
        if self.screen == Screen::Home && !self.page_shown() {
            let flat_len = self.home.flat().len();
            let down = !matches!(a, Action::Up | Action::PageUp | Action::Bottom);
            let at = self.home.sel.at;
            let is_title = |i: usize| matches!(self.home.flat().get(i), Some(HomeRow::Title(_)));
            if is_title(at) {
                let next = if down || at == 0 { at + 1 } else { at - 1 };
                self.home.sel.to(next, flat_len);
            }
        }
        self.more();
    }

    /// Near the end of a list read in pages: the next page.
    fn more(&mut self) {
        if self.screen != Screen::Library || self.page_shown() {
            return;
        }
        let t = self.library.tab;
        let len = self.library.len(t);
        let near = self.library.sels[t].at + 50 >= len;
        if t == 0 && near && self.library.albums_more {
            self.library.albums_more = false;
            self.cmds.push(Cmd::Load(Req::Albums { offset: len as u32 }));
        }
        if t == 3 && near && self.library.songs_more {
            self.library.songs_more = false;
            self.cmds.push(Cmd::Load(Req::Songs { offset: len as u32 }));
        }
    }

    /// What is selected on the screen now.
    pub fn selected(&self) -> Option<Item> {
        if self.page_shown() {
            return match self.page()? {
                Page::Album { detail, sel, .. } => detail.ready().map(|d| Item::Song(d.songs.clone(), sel.at)),
                Page::Playlist { detail, sel, .. } => detail.ready().map(|d| Item::Song(d.songs.clone(), sel.at)),
                Page::Artist { detail, sel, .. } => detail.ready().and_then(|d| d.albums.get(sel.at).cloned()).map(Item::Album),
            };
        }
        match self.screen {
            Screen::Home => match self.home.flat().get(self.home.sel.at) {
                Some(HomeRow::Album(a)) => Some(Item::Album((*a).clone())),
                _ => None,
            },
            Screen::Library => {
                let l = &self.library;
                let at = l.sels[l.tab].at;
                match l.tab {
                    0 => l.albums.ready()?.get(at).cloned().map(Item::Album),
                    1 => l.artists.ready()?.get(at).cloned().map(Item::Artist),
                    2 => l.playlists.ready()?.get(at).cloned().map(Item::Playlist),
                    _ => l.songs.ready().filter(|s| at < s.len()).map(|s| Item::Song(s.clone(), at)),
                }
            }
            Screen::Search => {
                let r = self.search.view.as_ref()?.shown.as_ref()?;
                let at = self.search.sels[self.search.pane].at;
                match self.search.pane {
                    0 => r.artists.get(at).cloned().map(Item::Artist),
                    1 => r.albums.get(at).cloned().map(Item::Album),
                    // A search's songs are one song each: the list is not an album, and may hold a provider's.
                    _ => r.songs.get(at).cloned().map(|s| Item::Song(vec![s], 0)),
                }
            }
            Screen::Downloads => {
                let rows = self.download_rows();
                let (songs, i) = rows.get(self.downloads_sel.at).and_then(|r| r.1)?;
                Some(Item::Song(songs.to_vec(), i))
            }
            _ => None,
        }
    }

    fn act_on_selected(&mut self, a: Action) {
        if self.screen == Screen::Queue && a == Action::Open {
            if let Some(i) = self.queue_selected_index() {
                self.cmds.push(Cmd::Jump(i));
            }
            return;
        }
        if self.screen == Screen::Playing && a == Action::Open && !self.page_shown() {
            if let Some(&i) = self.up_next().get(self.up_next_sel.at) {
                self.cmds.push(Cmd::Jump(i));
            }
            return;
        }
        if self.screen == Screen::Settings {
            if a == Action::Open {
                self.settings_open();
            }
            return;
        }
        if self.screen == Screen::Equalizer {
            if a == Action::Open {
                self.eq_open();
            }
            return;
        }
        // Playing a whole page works with nothing selected.
        if matches!(a, Action::PlayAll | Action::ShuffleAll) {
            return self.play_all(a == Action::ShuffleAll);
        }
        let Some(item) = self.selected() else { return };
        match (a, item) {
            (Action::Open, Item::Song(songs, i)) => self.tap(songs, i),
            (Action::Open, Item::Album(al)) => self.open_album(al.id),
            (Action::Open, Item::Artist(ar)) => self.open_artist(ar.id),
            (Action::Open, Item::Playlist(p)) => self.open_playlist(p.id),
            (Action::Enqueue | Action::PlayNext, item) => {
                let next = a == Action::PlayNext;
                match item {
                    Item::Song(songs, i) => self.cmds.push(Cmd::Enqueue(vec![songs[i].clone()], next)),
                    Item::Album(al) => self.cmds.push(Cmd::EnqueueFetch(Fetch::Album(al.id), next)),
                    Item::Artist(ar) => self.cmds.push(Cmd::EnqueueFetch(Fetch::Artist(ar.id), next)),
                    Item::Playlist(p) => self.cmds.push(Cmd::EnqueueFetch(Fetch::Playlist(p.id), next)),
                }
            }
            (Action::Download, item) => match item {
                Item::Song(songs, i) => self.cmds.push(Cmd::Download(vec![songs[i].clone()])),
                Item::Album(al) => self.cmds.push(Cmd::DownloadFetch(Fetch::Album(al.id))),
                Item::Artist(ar) => self.cmds.push(Cmd::DownloadFetch(Fetch::Artist(ar.id))),
                Item::Playlist(p) => self.cmds.push(Cmd::DownloadFetch(Fetch::Playlist(p.id))),
            },
            (Action::Remove, Item::Song(songs, i)) if self.screen == Screen::Downloads => self.cmds.push(Cmd::DownloadRemove(songs[i].id.clone())),
            (Action::Star, item) => {
                let (kind, id, on) = match item {
                    Item::Song(songs, i) => (Starrable::Song, songs[i].id.clone(), !songs[i].starred),
                    Item::Album(al) => (Starrable::Album, al.id, !al.starred),
                    Item::Artist(ar) => (Starrable::Artist, ar.id, !ar.starred),
                    Item::Playlist(_) => return,
                };
                self.cmds.push(Cmd::Star(kind, id, on));
            }
            _ => self.dirty = false,
        }
    }

    /// A song picked from a list, as the "Choosing a song" setting says.
    fn tap(&mut self, songs: Vec<Song>, i: usize) {
        match self.prefs.tap_action {
            // One song on its own is no page's queue.
            TapAction::PlayOne => self.cmds.push(Cmd::Play { songs: vec![songs[i].clone()], start: 0, shuffle: false, from: None }),
            TapAction::Queue => self.cmds.push(Cmd::Enqueue(vec![songs[i].clone()], false)),
            TapAction::PlayNext => self.cmds.push(Cmd::Enqueue(vec![songs[i].clone()], true)),
            TapAction::PlayList => {
                let from = self.origin_here();
                self.cmds.push(Cmd::Play { songs, start: i, shuffle: false, from })
            }
        }
    }

    /// The page whose own list of songs is the one picked from here, for the queue it starts: the page
    /// open, or the screen's song list. None where the list is not one place's.
    fn origin_here(&self) -> Option<PageOrigin> {
        if let Some(page) = self.page() {
            return Some(match page {
                Page::Album { id, .. } => PageOrigin::new(OriginKind::Album, id.as_str()),
                Page::Artist { id, .. } => PageOrigin::new(OriginKind::Artist, id.as_str()),
                Page::Playlist { id, .. } => PageOrigin::new(OriginKind::Playlist, id.as_str()),
            });
        }
        match self.screen {
            Screen::Library => Some(PageOrigin::new(OriginKind::Songs, "")),
            Screen::Search => Some(PageOrigin::new(OriginKind::Search, self.search.text.as_str())),
            Screen::Downloads => Some(PageOrigin::new(OriginKind::Downloads, "")),
            _ => None,
        }
    }

    fn play_all(&mut self, shuffle: bool) {
        if self.page_shown() {
            match self.page() {
                Some(Page::Artist { id, .. }) => self.cmds.push(Cmd::PlayFetch(Fetch::Artist(id.clone()), shuffle)),
                Some(p) => {
                    if let Some(songs) = p.songs() {
                        let from = self.origin_here();
                        self.cmds.push(Cmd::Play { songs: songs.to_vec(), start: 0, shuffle, from });
                    }
                }
                None => {}
            }
            return;
        }
        match self.selected() {
            Some(Item::Album(a)) => self.cmds.push(Cmd::PlayFetch(Fetch::Album(a.id), shuffle)),
            Some(Item::Artist(a)) => self.cmds.push(Cmd::PlayFetch(Fetch::Artist(a.id), shuffle)),
            Some(Item::Playlist(p)) => self.cmds.push(Cmd::PlayFetch(Fetch::Playlist(p.id), shuffle)),
            Some(Item::Song(songs, _)) => {
                let from = self.origin_here();
                self.cmds.push(Cmd::Play { songs, start: 0, shuffle, from })
            }
            None => {}
        }
    }

    // ---- the queue ----

    /// The queue's rows in the order they play: list indexes.
    pub fn queue_order(&self) -> Vec<usize> {
        self.queue.as_ref().map_or_else(Vec::new, |q| q.order.iter().map(|&i| i as usize).collect())
    }

    fn queue_selected_index(&self) -> Option<usize> {
        if self.screen != Screen::Queue {
            return None;
        }
        self.queue_order().get(self.queue_sel.at).copied()
    }

    fn queue_move(&mut self, down: bool) {
        if self.queue_shuffled() {
            self.say("Turn shuffle off to move songs", false);
            return;
        }
        let Some(from) = self.queue_selected_index() else { return };
        let len = self.queue.as_ref().map_or(0, |q| q.len as usize);
        let to = if down { from + 1 } else { from.wrapping_sub(1) };
        if to >= len {
            return;
        }
        self.cmds.push(Cmd::Move(from, to));
        self.queue_sel.at = if down { self.queue_sel.at + 1 } else { self.queue_sel.at - 1 };
    }

    /// The songs after the one playing, in play order: list indexes.
    pub fn up_next(&self) -> Vec<usize> {
        let order = self.queue_order();
        let current = self.queue.as_ref().map_or(-1, |q| q.index);
        match order.iter().position(|&i| i as i32 == current) {
            Some(p) => order[p + 1..].to_vec(),
            None => order,
        }
    }

    // ---- downloads ----

    /// The downloads page's rows: a heading, or a song with the list it is in.
    pub fn download_rows(&self) -> Vec<DownloadRow<'_>> {
        let Some(d) = self.downloads.ready() else { return Vec::new() };
        let mut out = Vec::new();
        for (title, list) in [("Downloading", &d.active), ("Waiting", &d.queued), ("Failed", &d.failed), ("On this computer", &d.stored)] {
            if list.is_empty() {
                continue;
            }
            out.push((format!("{title} ({})", list.len()), None));
            for i in 0..list.len() {
                out.push((String::new(), Some((list.as_slice(), i))));
            }
        }
        out
    }

    // ---- lyrics ----

    fn lyrics_action(&mut self, a: Action) {
        let Some(l) = &self.lyrics else { return };
        let len = l.pick.lyrics.lines.len();
        // Past the last line once it is over: the last line is where a selection starts.
        let active = (l.clock.shown().active.max(0) as usize).min(len.saturating_sub(1));
        let at = self.lyrics_sel.unwrap_or(active);
        match a {
            Action::Up => self.lyrics_sel = Some(at.saturating_sub(1)),
            Action::Down => self.lyrics_sel = Some((at + 1).min(len.saturating_sub(1))),
            Action::Top => self.lyrics_sel = Some(0),
            Action::Bottom => self.lyrics_sel = Some(len.saturating_sub(1)),
            Action::Open => {
                if l.pick.lyrics.synced {
                    let to = l.clock.tap(at);
                    self.lyrics_sel = None;
                    self.now.position_ms = to;
                    self.now.at = Instant::now();
                    self.cmds.push(Cmd::Seek(to));
                    self.lyrics_wake = Some(Instant::now());
                }
            }
            Action::Back => self.lyrics_sel = None,
            _ => {}
        }
    }

    // ---- the settings and the equalizer ----

    fn settings_open(&mut self) {
        let prefs = self.prefs.clone();
        match self.settings.open(&prefs, self.mouse, self.images) {
            Some(crate::settings_view::Opened::Cmds(c)) => self.cmds.extend(c),
            Some(crate::settings_view::Opened::Overlay(o)) => self.overlay = Some(o),
            Some(crate::settings_view::Opened::Screen(s)) => self.go(s),
            Some(crate::settings_view::Opened::Own(key)) => self.own_toggle(key),
            Some(crate::settings_view::Opened::Login) => {
                self.login = Login::default();
                self.screen = Screen::Login;
            }
            None => self.dirty = false,
        }
    }

    fn own_toggle(&mut self, key: &str) {
        match key {
            "mouse" => self.do_action(Action::Mouse),
            "images" => self.do_action(Action::Images),
            _ => {}
        }
        self.settings.invalidate();
    }

    /// A change of the sound was kept (a band, a level, a preset). On the equalizer screen with the
    /// equalizer on, the first one asks the engine for its shallow buffer (true), so the ones after it
    /// are heard at once and without a dip. Opening the screen alone asks nothing: the output stays as it
    /// was until something is really changed. When is the core's (`rules::equalizer_tuning`).
    pub fn sound_edited(&mut self) -> bool {
        self.touched |= equalizer_tuning(self.screen == Screen::Equalizer, true, self.prefs.eq_enabled);
        self.tune()
    }

    /// Asks the engine for the shallow buffer, or gives it back, when what the core wants changed: the
    /// equalizer screen left, or the equalizer switched off. True when it was asked for now.
    fn tune(&mut self) -> bool {
        let want = equalizer_tuning(self.screen == Screen::Equalizer, self.touched, self.prefs.eq_enabled);
        if want == self.tuning {
            return false;
        }
        self.tuning = want;
        if !want {
            self.cmds.push(Cmd::Tuning(false));
        }
        want
    }

    fn eq_step(&mut self, up: bool) {
        let rows = crate::settings_view::eq_rows(&self.prefs);
        if let Some(row) = rows.get(self.eq_sel.at) {
            if let Some(c) = row.step(&self.prefs, up) {
                self.cmds.push(c);
            }
        }
    }

    fn eq_open(&mut self) {
        let rows = crate::settings_view::eq_rows(&self.prefs);
        match rows.get(self.eq_sel.at) {
            Some(crate::settings_view::EqRow::Presets) => {
                let options = nori_core::dsp::eq_presets().iter().enumerate().map(|(i, p)| (crate::text::preset(p.kind).to_string(), i.to_string())).collect();
                self.overlay = Some(Overlay::Picker { title: "Presets".into(), options, sel: Sel::default(), name: "!preset".into() });
            }
            Some(row) => {
                if let Some(c) = row.open(&self.prefs) {
                    self.cmds.push(c);
                }
            }
            None => {}
        }
    }

    // ---- the mouse ----

    fn hit(&self, col: u16, row: u16) -> Option<Hit> {
        // The last drawn on top wins: overlays are drawn last.
        self.hits.iter().rev().find(|(r, _)| r.contains(Position { x: col, y: row })).map(|(_, h)| *h)
    }

    fn mouse_event(&mut self, m: MouseEvent) {
        if !self.mouse {
            self.dirty = false;
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(h) = self.hit(m.column, m.row) else {
                    self.dirty = false;
                    return;
                };
                let again = self.last_click.is_some_and(|(last, t)| last == h && t.elapsed() < Duration::from_millis(500));
                self.last_click = Some((h, Instant::now()));
                self.click(h, m.column, again);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.scrub.is_some() || self.hit(m.column, m.row) == Some(Hit::Seek) {
                    self.scrub = Some(self.share(m.column));
                } else {
                    self.dirty = false;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(share) = self.scrub.take() {
                    self.seek_share(share);
                } else {
                    self.dirty = false;
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d = if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                match self.hit(m.column, m.row) {
                    Some(Hit::Row(l, _) | Hit::List(l)) => self.scroll(l, d),
                    _ => self.dirty = false,
                }
            }
            _ => self.dirty = false,
        }
    }

    fn share(&self, col: u16) -> f32 {
        let r = self.seek_rect;
        if r.width == 0 {
            return 0.0;
        }
        ((col.saturating_sub(r.x)) as f32 / r.width.max(1) as f32).clamp(0.0, 1.0)
    }

    fn seek_share(&mut self, share: f32) {
        let Some(song) = &self.song else { return };
        let to = (song.duration as f32 * 1000.0 * share) as i64;
        self.now.position_ms = to;
        self.now.at = Instant::now();
        self.cmds.push(Cmd::Seek(to));
    }

    fn scroll(&mut self, l: ListRef, d: isize) {
        match l {
            ListRef::Help => {
                if let Some(Overlay::Help { scroll }) = &mut self.overlay {
                    *scroll = (*scroll as isize + d).max(0) as usize;
                }
            }
            ListRef::Lyrics => {
                let len = self.lyrics.as_ref().map_or(0, |l| l.pick.lyrics.lines.len());
                let at = self.lyrics_sel.unwrap_or_else(|| self.lyrics.as_ref().map_or(0, |l| l.clock.shown().active.max(0) as usize));
                self.lyrics_sel = Some((at as isize + d).clamp(0, len.saturating_sub(1) as isize) as usize);
            }
            ListRef::Picker => {
                if let Some(Overlay::Picker { sel, options, .. }) = &mut self.overlay {
                    sel.by(d, options.len());
                }
            }
            _ => {
                self.focus_list(l);
                if let Some((sel, len)) = self.list() {
                    sel.by(d, len);
                }
                self.more();
            }
        }
    }

    /// Makes `l` the list the keys move in.
    fn focus_list(&mut self, l: ListRef) {
        match l {
            ListRef::Search(p) => self.search.pane = p,
            ListRef::Groups => self.settings.pane = 0,
            ListRef::Rows => self.settings.pane = 1,
            _ => {}
        }
    }

    fn click(&mut self, h: Hit, col: u16, again: bool) {
        match h {
            Hit::Tab(i) => {
                if let Some((s, _)) = SCREENS.get(i) {
                    if self.screen == *s {
                        self.clear_pages();
                    }
                    self.go(*s);
                }
            }
            Hit::LibTab(i) => self.library_tab(i),
            Hit::Seek => {
                self.scrub = Some(self.share(col));
            }
            Hit::SearchField => {
                self.search.editing = true;
            }
            Hit::LoginField(i) => {
                self.login.on_list = false;
                self.login.focus = i;
            }
            Hit::Button(b) => self.button(b),
            Hit::List(_) => self.dirty = false,
            Hit::Row(l, i) => self.click_row(l, i, again),
        }
    }

    fn button(&mut self, b: Button) {
        match b {
            Button::Previous => self.do_action(Action::Previous),
            Button::Toggle => self.do_action(Action::TogglePlay),
            Button::Next => self.do_action(Action::Next),
            Button::Shuffle => self.do_action(Action::Shuffle),
            Button::Repeat => self.do_action(Action::Repeat),
            Button::VolumeDown => self.do_action(Action::VolumeDown),
            Button::VolumeUp => self.do_action(Action::VolumeUp),
            Button::PlayAll => self.play_all(false),
            Button::ShuffleAll => self.play_all(true),
            Button::Back => self.back(),
            Button::Help => self.do_action(Action::Help),
            Button::Connect => self.connect(),
        }
    }

    fn click_row(&mut self, l: ListRef, i: usize, again: bool) {
        match l {
            ListRef::Picker => {
                if let Some(Overlay::Picker { sel, options, .. }) = &mut self.overlay {
                    sel.to(i, options.len());
                }
                self.overlay_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                return;
            }
            ListRef::Lyrics => {
                self.lyrics_sel = Some(i);
                if again {
                    self.lyrics_action(Action::Open);
                }
                return;
            }
            ListRef::Profiles => {
                self.login.on_list = true;
                self.login.sel.at = i;
                if again {
                    self.login_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
                return;
            }
            ListRef::Help => return,
            _ => {}
        }
        self.focus_list(l);
        let settings_row = l == ListRef::Rows;
        if let Some((sel, len)) = self.list() {
            sel.to(i, len);
        }
        // A click on a toggle switches it at once; anything else opens on a second click.
        if again || (settings_row && self.settings.clicks_open()) || l == ListRef::Groups {
            self.act_on_selected(Action::Open);
            if l == ListRef::Groups {
                self.settings.pane = 0;
            }
        }
    }
}

/// A row of the downloads page: a heading, or a song with the list it is in and its place there.
pub type DownloadRow<'a> = (String, Option<(&'a [Song], usize)>);

/// How long a note stays in the status bar.
pub const NOTE_FOR: Duration = Duration::from_secs(4);

/// The queue's repeat modes (nori_player::playlist's numbering).
mod nori_player_repeat {
    pub const OFF: u8 = 0;
    pub const ONE: u8 = 1;
    pub const ALL: u8 = 2;
}
