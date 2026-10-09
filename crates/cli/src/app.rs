//! UI state and input handling, free of I/O: [`Msg`]s in, [`Cmd`]s out for the runner, so it runs in
//! tests without a server, sound card or terminal. The window has a sidebar, a page, a right panel and
//! a player bar; [`Focus`] says which of the first three takes the keys.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use nori_core::browse::{LibrarySection, ProfileRules};
use nori_core::client::Starrable;
use nori_core::playlist::PlaylistView;
use nori_core::remote::wire::Pending;
use nori_core::remote::{Controls, JamControls, JamView, Listening, Reach, RemoteDevice};
use nori_core::search::SearchView;
use nori_core::settings::{EqLevel, SavedServer, SoundBand, StoredPrefs, TapAction};
use nori_core::settings_store::SoundTool;
use nori_core::stars::StarMarks;
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
use crate::settings_view::{Chore, Opened, SettingsView, Switch};

/// The page's root view, under any opened album, artist or playlist pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Home,
    Search,
    Albums,
    Artists,
    Songs,
    Downloads,
    Equalizer,
    Settings,
    /// Full-window login form.
    Login,
}

/// Number key targets, in order.
pub const GO: [View; 7] = [View::Home, View::Albums, View::Artists, View::Songs, View::Downloads, View::Equalizer, View::Settings];

/// The part of the window that has the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    Side,
    #[default]
    Main,
    Panel,
}

/// Right panel content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Playing,
    Queue,
    Lyrics,
    /// Where the music plays: this computer or another device (remote control), and the jam.
    Devices,
}

pub const PANELS: [(Panel, &str); 4] = [(Panel::Playing, "Playing"), (Panel::Queue, "Queue"), (Panel::Lyrics, "Lyrics"), (Panel::Devices, "Devices")];

/// A sidebar entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Nav {
    Search,
    #[default]
    Home,
    Albums,
    Artists,
    Songs,
    Downloads,
    /// Index into the library's playlists.
    Playlist(usize),
    Equalizer,
    Settings,
}

impl Nav {
    pub fn icon(self) -> &'static str {
        match self {
            Nav::Search => "⌕",
            Nav::Home => "⌂",
            Nav::Albums => "◫",
            Nav::Artists => "◉",
            Nav::Songs => "♪",
            Nav::Downloads => "↓",
            Nav::Playlist(_) => "≡",
            Nav::Equalizer => "≋",
            Nav::Settings => "✱",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Nav::Search => "Search",
            Nav::Home => "Home",
            Nav::Albums => "Albums",
            Nav::Artists => "Artists",
            Nav::Songs => "Songs",
            Nav::Downloads => "Downloads",
            Nav::Playlist(_) => "",
            Nav::Equalizer => "Equalizer",
            Nav::Settings => "Settings",
        }
    }
}

/// Sidebar entries above and below the playlists.
pub const NAV_TOP: [Nav; 2] = [Nav::Search, Nav::Home];
pub const NAV_LIBRARY: [Nav; 4] = [Nav::Albums, Nav::Artists, Nav::Songs, Nav::Downloads];
pub const NAV_BOTTOM: [Nav; 2] = [Nav::Equalizer, Nav::Settings];

/// Work for the runner.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Load(Req),
    /// `from`: the page the songs are the list of (`playlist_set`'s origin).
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
    /// Undo the last removal (`playlist_restore`).
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
    /// A graphic equalizer band's gain in dB.
    Graphic(u32, f32),
    Sound(SoundToolCmd),
    Action(Chore),
    /// The answer to the last AutoEQ note (`Session::curve_answer`).
    Curve,
    Mouse(bool),
    Images(bool),
    /// Output device for the next start; empty for the system default.
    Device(String),
    Tuning(bool),
    SearchTyped(String),
    SearchServer(String),
    Lyrics(String),
    /// A large cover by id; `colours` also derives the theme from it.
    Cover { art: String, colours: bool },
    /// An album card cover by id.
    Thumb(String),
    CardCovers(bool),
    Login(SavedServer),
    SwitchServer(String),
    /// Moves the music to this device, or here (None).
    Pick(Option<String>),
    JamStart,
    JamEnd,
    /// Accepts (true) or refuses a jam request.
    JamDecide(u64, bool),
    /// Joins the jam this invite link is to.
    JoinJam(String),
    LeaveJam,
    /// A jam guest listens along (plays the host's music here, in step), or only watches.
    Listen(bool),
    Quit,
}

impl Cmd {
    /// A short description for the debug log, without passwords.
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
            Cmd::Action(a) => format!("action {a:?}"),
            Cmd::Curve => "curve".into(),
            Cmd::Tuning(on) => format!("tuning {on}"),
            Cmd::Mouse(on) => format!("mouse {on}"),
            Cmd::Images(on) => format!("images {on}"),
            Cmd::Device(d) => format!("device {d}"),
            Cmd::Pick(d) => format!("play on {d:?}"),
            Cmd::JamStart => "start a jam".into(),
            Cmd::JamEnd => "end the jam".into(),
            Cmd::JamDecide(r, yes) => format!("jam request {r} accepted={yes}"),
            Cmd::JoinJam(_) => "join a jam".into(),
            Cmd::LeaveJam => "leave the jam".into(),
            Cmd::Listen(on) => format!("listen along {on}"),
            Cmd::Quit => "quit".into(),
            _ => "an edit".into(),
        }
    }
}

/// `SoundTool` as a comparable command.
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

/// Loading state of fetched data.
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

/// A page opened over the view.
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

    /// The page's songs, for album and playlist pages.
    pub fn songs(&self) -> Option<&[Song]> {
        match self {
            Page::Album { detail, .. } => detail.ready().map(|d| d.songs.as_slice()),
            Page::Playlist { detail, .. } => detail.ready().map(|d| d.songs.as_slice()),
            Page::Artist { .. } => None,
        }
    }
}

/// The selected thing that item actions (open, enqueue, star...) apply to.
#[derive(Debug, Clone)]
pub enum Item {
    Song(Vec<Song>, usize),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
}

/// The home page: album shelves, each scrolled independently.
#[derive(Default)]
pub struct Home {
    /// Indexed like `HOME_ROWS`; None until loaded.
    pub rows: Vec<Option<(&'static str, Vec<Album>)>>,
    pub error: Option<String>,
    pub asked: bool,
    /// Selected shelf, indexed like `HOME_ROWS`.
    pub shelf: usize,
    /// Per shelf: the selected album and the first visible one.
    pub pos: Vec<usize>,
    pub left: Vec<usize>,
    /// First visible shelf.
    pub top: usize,
}

impl Home {
    /// Non-empty shelves: index, title, albums.
    pub fn shelves(&self) -> Vec<(usize, &'static str, &[Album])> {
        self.rows.iter().enumerate().filter_map(|(i, r)| r.as_ref().filter(|(_, v)| !v.is_empty()).map(|(t, v)| (i, *t, v.as_slice()))).collect()
    }

    /// The selected album.
    pub fn album(&self) -> Option<&Album> {
        let (_, v) = self.rows.get(self.shelf)?.as_ref()?;
        v.get(*self.pos.get(self.shelf)?)
    }

    /// Moves the selection to the nearest non-empty shelf in direction `down`.
    fn settle(&mut self, down: bool) {
        let full: Vec<usize> = self.shelves().iter().map(|s| s.0).collect();
        if full.is_empty() || full.contains(&self.shelf) {
            return;
        }
        self.shelf = if down { full.iter().find(|&&s| s > self.shelf).or(full.last()) } else { full.iter().rev().find(|&&s| s < self.shelf).or(full.first()) }.copied().unwrap_or(0);
    }

    fn step_shelf(&mut self, d: isize) {
        let full: Vec<usize> = self.shelves().iter().map(|s| s.0).collect();
        let Some(at) = full.iter().position(|&s| s == self.shelf) else { return self.settle(d > 0) };
        self.shelf = full[(at as isize + d).clamp(0, full.len() as isize - 1) as usize];
    }

    fn step_along(&mut self, d: isize) {
        let len = self.rows.get(self.shelf).and_then(|r| r.as_ref()).map_or(0, |r| r.1.len());
        if let Some(p) = self.pos.get_mut(self.shelf) {
            *p = (*p as isize + d).clamp(0, len.saturating_sub(1) as isize) as usize;
        }
    }
}

#[derive(Default)]
pub struct Library {
    pub albums: Load<Vec<Album>>,
    pub albums_more: bool,
    pub albums_sel: Sel,
    pub artists: Load<Vec<Artist>>,
    pub artists_sel: Sel,
    pub playlists: Load<Vec<Playlist>>,
    pub songs: Load<Vec<Song>>,
    pub songs_more: bool,
    pub songs_sel: Sel,
}

#[derive(Default)]
pub struct Search {
    pub text: String,
    pub editing: bool,
    pub view: Option<SearchView>,
    /// Selection over `rows()`, titles included.
    pub sel: Sel,
    /// When to query the server, once typing pauses.
    pub ask_at: Option<Instant>,
}

/// A search result row: a section title (with its count) or an item.
pub enum SearchRow<'a> {
    Title(&'static str, usize),
    Song(&'a Song),
    Album(&'a Album),
    Artist(&'a Artist),
}

impl Search {
    /// The results as one list: songs, albums, artists, each under a title.
    pub fn rows(&self) -> Vec<SearchRow<'_>> {
        let mut out = Vec::new();
        let Some(r) = self.view.as_ref().and_then(|v| v.shown.as_ref()) else { return out };
        if !r.songs.is_empty() {
            out.push(SearchRow::Title("Songs", r.songs.len()));
            out.extend(r.songs.iter().map(SearchRow::Song));
        }
        if !r.albums.is_empty() {
            out.push(SearchRow::Title("Albums", r.albums.len()));
            out.extend(r.albums.iter().map(SearchRow::Album));
        }
        if !r.artists.is_empty() {
            out.push(SearchRow::Title("Artists", r.artists.len()));
            out.extend(r.artists.iter().map(SearchRow::Artist));
        }
        out
    }

    /// Moves the selection off a title in direction `down`.
    fn settle(&mut self, down: bool) {
        let rows = self.rows();
        let title = |i: usize| matches!(rows.get(i), Some(SearchRow::Title(..)));
        let mut at = self.sel.at;
        if title(at) {
            at = if (down || at == 0) && at + 1 < rows.len() { at + 1 } else { at.saturating_sub(1) };
        }
        self.sel.at = at;
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
    /// Selection in the saved servers list.
    pub sel: Sel,
    /// Whether the saved servers list has the keys.
    pub on_list: bool,
}

pub const LOGIN_FIELDS: [&str; 4] = ["Name (optional)", "Server address", "User", "Password"];

/// Playback state for drawing; position extrapolated from `at`.
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

/// Remote control and jams as the runner last read them from the session; empty while both are off.
#[derive(Default)]
pub struct Devices {
    /// Remote control or jams are switched on.
    pub on: bool,
    /// A jam can be started: jams are on and the server has not said it cannot relay.
    pub jams: bool,
    /// Jams are on but the server cannot relay them.
    pub jams_unsupported: bool,
    /// The account's other devices.
    pub list: Vec<RemoteDevice>,
    pub sel: Sel,
    /// The device playing while it is another one: its id and name.
    pub active: Option<(String, String)>,
    /// Hearts as the device playing shows them, by song id, while it is another one.
    pub hearts: HashMap<String, bool>,
    /// The jam this computer hosts, or is a guest in.
    pub jam: Option<JamView>,
    /// Who asked for each song that came in through the jam, by song id.
    pub added: HashMap<String, String>,
    /// A jam guest's controls by its role: what its keys reach, and what the player says.
    pub controls: Option<JamControls>,
}

/// A row of the devices panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRow {
    Here,
    /// Index into [`Devices::list`]: its name, then what it plays.
    Device(usize),
    Playing(usize),
    /// Starts a jam, or shows the one hosted.
    Jam,
    /// Joins someone's jam.
    Join,
}

impl Devices {
    pub fn rows(&self) -> Vec<DeviceRow> {
        let mut rows = vec![DeviceRow::Here];
        rows.extend((0..self.list.len()).flat_map(|i| [DeviceRow::Device(i), DeviceRow::Playing(i)]));
        if self.jams || self.jam.is_some() {
            rows.push(DeviceRow::Jam);
        }
        rows.push(DeviceRow::Join);
        rows
    }

    /// Moves the selection off a line of what a device plays, in direction `down`.
    fn settle(&mut self, down: bool) {
        let rows = self.rows();
        if let Some(DeviceRow::Playing(i)) = rows.get(self.sel.at) {
            let past = self.sel.at + 1;
            self.sel.at = if down && past < rows.len() { past } else { rows.iter().position(|r| *r == DeviceRow::Device(*i)).unwrap_or(0) };
        }
    }

    /// The jam's guests, the host left out.
    pub fn listeners(&self) -> Vec<&nori_core::remote::wire::JamMember> {
        self.jam.as_ref().map_or_else(Vec::new, |j| j.listeners().collect())
    }

    /// The jam's host's name.
    pub fn host(&self) -> &str {
        self.jam.as_ref().map_or("", JamView::host)
    }

    /// The jam as the player bar and the queue say it: "Jam · 2 listening", a guest's with its host.
    pub fn jam_strip(&self) -> Option<String> {
        let j = self.jam.as_ref()?;
        let n = self.listeners().len();
        if self.controls.is_some_and(|c| c.paused_here) {
            return Some(crate::text::JAM_PAUSED_HERE.to_string());
        }
        Some(if j.hosting { crate::text::jam_strip(n) } else { crate::text::jam_guest_strip(self.host(), n) })
    }

    /// The requests the queue lists ([`JamView::asks`]).
    pub fn asks(&self) -> Vec<&Pending> {
        self.jam.as_ref().map_or_else(Vec::new, |j| j.asks().collect())
    }

    /// The songs this guest asked for that wait for the host, by id.
    pub fn asked(&self) -> HashSet<&str> {
        let guest = self.jam.as_ref().is_some_and(|j| !j.hosting);
        self.asks().into_iter().filter(|_| guest).map(|p| p.song.id.as_str()).collect()
    }

    /// How this guest listens along; Watching while hosting or in no jam.
    pub fn listening(&self) -> Listening {
        self.jam.as_ref().filter(|j| !j.hosting).map_or(Listening::Watching, |j| j.listening)
    }
}

/// A row of the queue panel: the hosted jam's header and requests, then the songs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueRow {
    /// How many listen; opens the invite, as the line of their names under it does.
    Jam,
    Listeners,
    /// A guest's switch for listening along.
    Listen,
    /// A line of why a guest who asked to listen along does not.
    AlongNote(usize),
    /// A guest's heading over its requests.
    Asking,
    /// A request, by index into [`Devices::asks`].
    Ask(usize),
    /// A line of the note under a provider's song asked for: request, line.
    Downloads(usize, usize),
    End,
    /// A song by list index.
    Song(usize),
}

impl QueueRow {
    /// The request a row is about.
    pub fn ask(self) -> Option<usize> {
        match self {
            QueueRow::Ask(k) | QueueRow::Downloads(k, _) => Some(k),
            _ => None,
        }
    }
}

/// A popup over the screen.
pub enum Overlay {
    Help { scroll: usize },
    /// A choice list; `target` receives the value picked.
    Picker { title: String, options: Vec<(String, String)>, sel: Sel, target: Target },
    /// A text field for setting `name`.
    Input { title: String, text: String, secret: bool, name: String },
    /// The jam's invite: its link and QR code.
    Invite { link: String },
    /// An invite link pasted to join someone's jam, and why joining failed.
    Join { text: String, error: Option<String>, busy: bool },
}

/// What a picker sets.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Setting(String),
    Device,
    Preset,
}

impl Target {
    /// The command for picking `value`; None for a non-numeric preset.
    pub fn cmd(self, value: String) -> Option<Cmd> {
        match self {
            Target::Setting(name) => Some(Cmd::Setting(name, value)),
            Target::Device => Some(Cmd::Device(value)),
            Target::Preset => value.parse().ok().map(|i| Cmd::Sound(SoundToolCmd::Preset(i))),
        }
    }
}

/// A clickable area's meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Index into [`App::nav`].
    Nav(usize),
    /// A list row or card by index.
    Row(ListRef, usize),
    /// A list's whole area, for the wheel.
    List(ListRef),
    Seek,
    Volume,
    Button(Button),
    SearchField,
    LoginField(usize),
    /// Index into `settings_view::GROUPS`.
    Group(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListRef {
    Side,
    /// Home shelf, indexed like `HOME_ROWS`.
    Shelf(usize),
    Albums,
    Artists,
    Songs,
    Search,
    Page,
    Downloads,
    Settings,
    Eq,
    Queue,
    UpNext,
    Lyrics,
    Devices,
    Profiles,
    Picker,
    Help,
}

impl ListRef {
    /// The window part the list is in.
    fn focus(self) -> Focus {
        match self {
            ListRef::Side => Focus::Side,
            ListRef::Queue | ListRef::UpNext | ListRef::Lyrics | ListRef::Devices => Focus::Panel,
            _ => Focus::Main,
        }
    }

    /// Whether a single click opens (cards).
    fn opens_on_click(self) -> bool {
        matches!(self, ListRef::Shelf(_) | ListRef::Albums | ListRef::Artists)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Previous,
    Toggle,
    Next,
    Shuffle,
    Repeat,
    Panel(Panel),
    Full,
    PlayAll,
    ShuffleAll,
    /// Star the page's album or artist.
    Star,
    /// Download the page's album, artist or playlist.
    Download,
    /// Star the playing song.
    StarSong,
    Back,
    Help,
    Connect,
}

/// Layout facts from the last frame: sidebar and panel shown, grid columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    pub side: bool,
    pub panel: bool,
    pub cols: usize,
    /// The right panel's width inside its margins.
    pub panel_w: u16,
}

impl Default for Shown {
    fn default() -> Self {
        Shown { side: true, panel: true, cols: 4, panel_w: 37 }
    }
}

/// Columns before the note under a provider's song asked for, in the queue panel.
pub const QUEUE_NOTE_INDENT: usize = 4;

/// A mouse drag in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    /// Seek bar position as a fraction of the song.
    Seek(f32),
    Volume,
    /// Index into `eq_rows`.
    Band(usize),
}

#[derive(Default)]
pub struct App {
    pub view: View,
    /// Pages opened over the view, topmost last.
    pub pages: Vec<Page>,
    pub focus: Focus,
    /// The open sidebar entry.
    pub root: Nav,
    pub side: Sel,
    pub home: Home,
    pub library: Library,
    pub search: Search,
    /// None when the panel is hidden.
    pub panel: Option<Panel>,
    /// Full-window player.
    pub full: bool,
    pub shown: Shown,
    pub queue: Option<PlaylistView>,
    pub queue_sel: Sel,
    /// Id of the song last removed from the queue, for undo.
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
    /// The audible song (during a mix, the louder one).
    pub song: Option<Song>,
    /// This session's star marks, drawn over the records' flags.
    pub marks: StarMarks,
    pub devices: Devices,
    /// What the open profile offers (a jam guest's: the host's library, its picks asked of the host).
    pub rules: ProfileRules,
    /// The planned transition out of `song`.
    pub transition: Option<nori_core::automix::planner::TransitionNote>,
    /// The transition that brought `song` in, while still mixing.
    pub mixed_in: Option<nori_core::automix::planner::TransitionNote>,
    /// Cover id of `song`.
    pub cover_art: Option<String>,
    pub colours: Option<Box<CoverColours>>,
    /// Theme of the panel and full player (from the cover); the rest uses the terminal's colours.
    pub theme: Theme,
    pub prefs: StoredPrefs,
    pub volume: f32,
    pub mouse: bool,
    pub images: bool,
    /// Covers on album cards (when covers are on).
    pub card_covers: bool,
    /// Card covers already requested.
    pub thumbs_asked: std::collections::HashSet<String>,
    /// Image protocol name, for the settings page.
    pub protocol: &'static str,
    pub server: String,
    pub offline: bool,
    pub unreachable: Option<String>,
    pub note: Option<(String, bool, Instant)>,
    pub hits: Vec<(Rect, Hit)>,
    pub cmds: Vec<Cmd>,
    pub dirty: bool,
    pub quit: bool,
    /// Whether the engine holds the equalizer's low-latency buffer ([`App::sound_edited`]); released
    /// when the equalizer closes or is switched off.
    pub tuning: bool,
    /// The sound was changed on the equalizer since it was opened.
    pub touched: bool,
    pub drag: Option<Drag>,
    /// A pending seek target and when it was asked: the engine's status lags it for a few frames.
    pub seek_hold: Option<(i64, Instant)>,
    /// Equalizer fader track top row and height, for mouse hits.
    pub eq_track: (u16, u16),
    pub seek_rect: Rect,
    pub volume_rect: Rect,
    /// The last left click, to detect a second click on the same target.
    last_click: Option<(Hit, Instant)>,
}

impl App {
    pub fn new(prefs: StoredPrefs) -> App {
        App {
            side: Sel { at: 1, top: 0 },
            panel: Some(Panel::Playing),
            theme: Theme::plain(prefs.accent),
            prefs,
            volume: 1.0,
            mouse: true,
            images: true,
            card_covers: true,
            protocol: "none",
            dirty: true,
            ..App::default()
        }
    }

    pub fn say(&mut self, text: impl Into<String>, error: bool) {
        self.note = Some((text.into(), error, Instant::now()));
        self.dirty = true;
    }

    /// Takes the stored settings and updates what depends on them.
    pub fn prefs_changed(&mut self, prefs: StoredPrefs) {
        self.prefs = prefs;
        // Releases the low-latency buffer if the equalizer was switched off.
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

    /// Whether lyrics are visible (panel or full player).
    pub fn lyrics_shown(&self) -> bool {
        self.full || self.panel == Some(Panel::Lyrics)
    }

    /// Whether the transition note is visible (playing panel or full player).
    pub fn transition_shown(&self) -> bool {
        self.full || self.panel == Some(Panel::Playing)
    }

    // ---- going places ----

    /// The sidebar entries.
    pub fn nav(&self) -> Vec<Nav> {
        let playlists = self.library.playlists.ready().map_or(0, Vec::len);
        let mut v = Vec::with_capacity(8 + playlists);
        v.extend(NAV_TOP);
        v.extend(self.nav_library());
        v.extend((0..playlists).map(Nav::Playlist));
        v.extend(NAV_BOTTOM);
        v
    }

    /// The library's entries the profile has.
    pub fn nav_library(&self) -> Vec<Nav> {
        let has = |n: Nav| {
            let section = match n {
                Nav::Albums => LibrarySection::Albums,
                Nav::Artists => LibrarySection::Artists,
                Nav::Songs => LibrarySection::Songs,
                _ => LibrarySection::Downloads,
            };
            self.rules.sections.contains(&section)
        };
        NAV_LIBRARY.into_iter().filter(|n| has(*n)).collect()
    }


    /// A jam guest's: its picks are asked of the host, and the player shows the jam, which it controls by
    /// its role ([`App::offers`]).
    pub fn guest(&self) -> bool {
        self.rules.asks
    }

    /// Whether the player offers `control`: a jam guest's by its role (the core's jam controls), this
    /// computer's own player all of them.
    pub fn offers(&self, control: impl Fn(&Controls) -> Reach) -> bool {
        !self.guest() || self.devices.controls.is_some_and(|c| control(&c.controls) != Reach::Nowhere)
    }

    /// Opens a root view, closing any opened pages, and requests its data if needed.
    pub fn go(&mut self, view: View) {
        if view != View::Equalizer {
            self.touched = false;
        }
        self.view = view;
        self.pages.clear();
        self.full = false;
        self.tune();
        self.dirty = true;
        self.root = nav_of(view);
        if let Some(i) = self.nav().iter().position(|n| *n == self.root) {
            self.side.at = i;
        }
        self.want_playlists();
        let l = &mut self.library;
        match view {
            View::Home if !self.home.asked => {
                self.home.asked = true;
                self.home.rows = (0..HOME_ROWS.len()).map(|_| None).collect();
                self.home.pos = vec![0; HOME_ROWS.len()];
                self.home.left = vec![0; HOME_ROWS.len()];
                self.cmds.push(Cmd::Load(Req::Home));
            }
            View::Albums if matches!(l.albums, Load::Idle) => {
                l.albums = Load::Loading;
                self.cmds.push(Cmd::Load(Req::Albums { offset: 0 }));
            }
            View::Artists if matches!(l.artists, Load::Idle) => {
                l.artists = Load::Loading;
                self.cmds.push(Cmd::Load(Req::Artists));
            }
            View::Songs if matches!(l.songs, Load::Idle) => {
                l.songs = Load::Loading;
                self.cmds.push(Cmd::Load(Req::Songs { offset: 0 }));
            }
            View::Downloads => self.cmds.push(Cmd::Load(Req::Downloads)),
            View::Settings if !self.settings.facts_asked => {
                self.settings.facts_asked = true;
                self.cmds.push(Cmd::Load(Req::Facts));
            }
            View::Search if self.search.view.is_none() => self.search.editing = true,
            _ => {}
        }
    }

    /// Requests the sidebar's playlists once, where the profile has them.
    fn want_playlists(&mut self) {
        if matches!(self.library.playlists, Load::Idle) && self.view != View::Login && self.rules.sections.contains(&LibrarySection::Playlists) {
            self.library.playlists = Load::Loading;
            self.cmds.push(Cmd::Load(Req::Playlists));
        }
    }

    /// Opens a sidebar entry and focuses the page.
    fn open_nav(&mut self, n: Nav) {
        self.focus = Focus::Main;
        match n {
            Nav::Search => {
                self.go(View::Search);
                self.search.editing = true;
            }
            Nav::Home => self.go(View::Home),
            Nav::Albums => self.go(View::Albums),
            Nav::Artists => self.go(View::Artists),
            Nav::Songs => self.go(View::Songs),
            Nav::Downloads => self.go(View::Downloads),
            Nav::Equalizer => self.go(View::Equalizer),
            Nav::Settings => self.go(View::Settings),
            Nav::Playlist(i) => {
                let Some(id) = self.library.playlists.ready().and_then(|v| v.get(i)).map(|p| p.id.clone()) else { return };
                self.pages.clear();
                self.full = false;
                self.open_playlist(id);
                self.root = n;
            }
        }
        if let Some(i) = self.nav().iter().position(|x| *x == n) {
            self.side.at = i;
        }
    }

    fn open_page(&mut self, page: Page) {
        self.cmds.push(Cmd::Load(page.req()));
        self.pages.push(page);
        self.focus = Focus::Main;
        self.full = false;
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

    /// The topmost opened page.
    pub fn page(&self) -> Option<&Page> {
        self.pages.last()
    }

    fn page_mut(&mut self) -> Option<&mut Page> {
        self.pages.last_mut()
    }

    /// Shows panel `p`, or hides it if already shown; the queue and lyrics take focus.
    pub fn set_panel(&mut self, p: Panel) {
        let there = self.panel == Some(p) && (self.shown.panel || self.focus == Focus::Panel);
        self.full = false;
        if there {
            if self.shown.panel {
                self.panel = None;
            }
            if self.focus == Focus::Panel {
                self.focus = Focus::Main;
            }
            return;
        }
        if p == Panel::Devices && !self.devices.on {
            return self.say(crate::text::REMOTE_OFF, false);
        }
        self.panel = Some(p);
        match p {
            // Select the playing song.
            Panel::Queue => {
                let current = self.queue.as_ref().map_or(-1, |q| q.index);
                if let Some(row) = self.queue_rows().iter().position(|r| *r == QueueRow::Song(current as usize)) {
                    self.queue_sel.at = row;
                }
                self.focus = Focus::Panel;
            }
            // Select the device playing.
            Panel::Devices => {
                let d = &mut self.devices;
                let active = d.active.as_ref().and_then(|(id, _)| d.list.iter().position(|x| &x.id == id));
                d.sel.at = active.map_or(0, |i| d.rows().iter().position(|r| *r == DeviceRow::Device(i)).unwrap_or(0));
                self.focus = Focus::Panel;
            }
            Panel::Lyrics => {
                self.want_lyrics();
                self.focus = Focus::Panel;
            }
            Panel::Playing if !self.shown.panel => self.focus = Focus::Panel,
            Panel::Playing => {}
        }
    }

    fn toggle_full(&mut self) {
        self.full = !self.full;
        if self.full {
            self.want_lyrics();
        }
    }

    /// Cycles focus between sidebar, page and panel.
    fn cycle(&mut self, forward: bool) {
        let mut parts = vec![Focus::Side, Focus::Main];
        if self.panel.is_some() {
            parts.push(Focus::Panel);
        }
        let at = parts.iter().position(|f| *f == self.focus).unwrap_or(1);
        let n = parts.len();
        self.focus = parts[if forward { (at + 1) % n } else { (at + n - 1) % n }];
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
        // Handlers clear `dirty` when nothing changed; an earlier message in the batch stays dirty.
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
            // Regaining focus redraws; the runner decides whether fully.
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
                if self.lyrics_for.as_deref() == Some(&song) && self.lyrics.as_ref().is_none_or(|l| l.replaced_by(&pick)) {
                    let pos = self.now.position(Instant::now());
                    self.lyrics = Some(SongLyrics::new(pick, pos));
                    self.lyrics_wake = Some(Instant::now());
                }
            }
            Msg::Search(v) => {
                if v.query == self.search.text.trim() {
                    self.search.view = Some(v);
                    self.search.settle(true);
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
            // Another device playing shows its own volume.
            Msg::Volume(v) if self.devices.active.is_none() => self.volume = v,
            Msg::Volume(_) => self.dirty = false,
            Msg::Starred(marks) => self.marks = marks,
            // The runner read the devices into `devices`.
            Msg::Remote => {}
            Msg::Jam(Ok(())) => {
                self.panel = Some(Panel::Queue);
                self.invite();
            }
            Msg::Jam(Err(e)) => self.say(format!("{} ({e})", crate::text::JAM_FAILED), true),
            Msg::Joined(Err(e)) => match &mut self.overlay {
                Some(Overlay::Join { error, busy, .. }) => {
                    *error = Some(e.clone());
                    *busy = false;
                }
                _ => self.say(e.clone(), true),
            },
            // The runner opens these, and the profiles a jam is joined and left with.
            Msg::From(..) | Msg::Joined(Ok(_)) | Msg::Left(_) => {}
        }
    }

    fn engine(&mut self, e: Event) {
        match e {
            Event::State(s) => {
                // Restart the clock from now: the engine's status may still predate this event.
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
                let what = crate::backend::app().song(&id).map_or_else(|| "The output".to_string(), |s| format!("“{}”", s.title));
                self.say(format!("{what} would not play: {message}"), true);
            }
            Event::Buffering(on) => self.now.buffering = on,
            Event::Stopped { .. } => self.say("Playback stopped: too many songs in a row would not play", true),
            Event::Output { name } => self.say(format!("Playing on {name}"), false),
            Event::Title(t) => self.say(format!("On air: {t}"), false),
            Event::Bridge { .. } => self.say("The network is gone", true),
            Event::Mixing(on) => self.now.mixing = on,
            // How a jam's host is followed is the core's jam controls' to show.
            Event::Position { .. } | Event::Placed { .. } | Event::Awake(_) | Event::Following(_) => {}
        }
    }

    /// The audible song, as read from the engine; on a change, cover and lyrics follow.
    pub fn heard(&mut self, song: Option<Song>) {
        let changed = self.song.as_ref().map(|s| &s.id) != song.as_ref().map(|s| &s.id);
        self.song = song;
        if !changed {
            return;
        }
        self.transition = None;
        self.seek_hold = None;
        let art = self.song.as_ref().and_then(|s| s.cover_art.clone());
        if art != self.cover_art {
            self.colours = None;
            self.retheme();
        }
        self.cover_art = art.clone();
        if let Some(art) = art {
            self.cmds.push(Cmd::Cover { art, colours: true });
        }
        if self.lyrics_shown() {
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
                self.home.settle(true);
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
            (Req::Playlists, Ok(Data::Playlists(v))) => {
                // Keep the same sidebar entry selected as the playlist count changes.
                let was = self.nav().get(self.side.at).copied();
                self.library.playlists = Load::Ready(v);
                if let Some(i) = was.and_then(|n| self.nav().iter().position(|x| *x == n)) {
                    self.side.at = i;
                }
            }
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
                // Refresh every second while downloads run (only while shown).
                let running = !d.active.is_empty() || !d.queued.is_empty();
                self.downloads_at = running.then(|| Instant::now() + Duration::from_secs(1));
                self.downloads = Load::Ready(d);
            }
            (Req::Downloads, Err(e)) => self.downloads = Load::Failed(e),
            (Req::Facts, Ok(Data::Facts(f))) => self.settings.set_facts(*f),
            (Req::Facts, Err(_)) => {}
            (req, r) => {
                for page in self.pages.iter_mut().rev() {
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

    /// When the loop must next wake on its own: the next second of playback, a lyrics change, a pending
    /// search, a note expiring, a downloads refresh. None when idle.
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let mut at: Option<Instant> = None;
        let mut sooner = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        if self.now.state == State::Playing && !self.now.buffering {
            let pos = self.now.position(now).max(0);
            let left = 1000 - pos % 1000;
            sooner(now + Duration::from_millis((left as f32 / self.now.speed.max(0.1)) as u64 + 5));
            if self.lyrics_shown() {
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

    /// Runs whatever is due at `now`.
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
            if self.view == View::Downloads {
                self.cmds.push(Cmd::Load(Req::Downloads));
            }
        }
    }

    // ---- keys ----

    /// Whether lyrics take the list keys.
    fn lyrics_focused(&self) -> bool {
        self.full || (self.focus == Focus::Panel && self.panel == Some(Panel::Lyrics))
    }

    /// Whether the focused page is a card grid.
    fn grid_focused(&self) -> bool {
        self.focus == Focus::Main
            && match self.page() {
                Some(p) => matches!(p, Page::Artist { .. }),
                None => matches!(self.view, View::Home | View::Albums),
            }
    }

    fn scopes(&self) -> &'static [Scope] {
        if self.lyrics_focused() {
            return &[Scope::Lyrics, Scope::List, Scope::Global];
        }
        match self.focus {
            Focus::Side => &[Scope::List, Scope::Global],
            Focus::Panel if self.panel == Some(Panel::Queue) => &[Scope::Edit, Scope::List, Scope::Global],
            Focus::Panel => &[Scope::List, Scope::Global],
            Focus::Main if self.grid_focused() => &[Scope::Grid, Scope::List, Scope::Global],
            Focus::Main if self.page().is_some() => &[Scope::List, Scope::Global],
            Focus::Main => match self.view {
                View::Downloads => &[Scope::Edit, Scope::List, Scope::Global],
                View::Equalizer => &[Scope::Eq, Scope::List, Scope::Global],
                View::Settings => &[Scope::Values, Scope::List, Scope::Global],
                _ => &[Scope::List, Scope::Global],
            },
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
        if self.view == View::Login {
            self.login_key(k);
            return;
        }
        if self.view == View::Search && self.search.editing && !self.full {
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
        if let Some(Overlay::Input { text: t, .. } | Overlay::Join { text: t, .. }) = &mut self.overlay {
            t.push_str(&text);
        } else if self.view == View::Login {
            self.login.fields[self.login.focus].push_str(&text);
        } else if nori_core::remote::is_invite(&text) {
            // An invite pasted anywhere offers to join its jam.
            self.overlay = Some(Overlay::Join { text, error: None, busy: false });
        } else {
            if self.view != View::Search {
                self.go(View::Search);
            }
            self.focus = Focus::Main;
            self.search.editing = true;
            self.search.text.push_str(&text);
            self.typed();
        }
    }

    fn typed(&mut self) {
        self.cmds.push(Cmd::SearchTyped(self.search.text.clone()));
        let delay = self.prefs.live_search_delay_ms.clamp(100, 2000) as u64;
        self.search.ask_at = (!self.search.text.trim().is_empty()).then(|| Instant::now() + Duration::from_millis(delay));
        self.search.sel = Sel::default();
        self.search.settle(true);
    }

    fn search_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => self.search.editing = false,
            KeyCode::Enter | KeyCode::Down | KeyCode::Tab => {
                self.search.editing = false;
                self.focus = Focus::Main;
                // Enter queries the server now.
                if k.code == KeyCode::Enter && !self.search.text.trim().is_empty() {
                    self.search.ask_at = None;
                    self.cmds.push(Cmd::SearchServer(self.search.text.trim().to_string()));
                }
                self.search.sel = Sel::default();
                self.search.settle(true);
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
                    self.view = View::Settings;
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
            Overlay::Picker { options, sel, target, .. } => match k.code {
                KeyCode::Down | KeyCode::Char('j') => sel.by(1, options.len()),
                KeyCode::Up | KeyCode::Char('k') => sel.by(-1, options.len()),
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char(' ') => {
                    if let Some((_, value)) = options.get(sel.at) {
                        let (target, value) = (target.clone(), value.clone());
                        self.overlay = None;
                        self.cmds.extend(target.cmd(value));
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => self.overlay = None,
                _ => {}
            },
            Overlay::Invite { .. } => self.overlay = None,
            Overlay::Join { busy: true, .. } if k.code != KeyCode::Esc => self.dirty = false,
            Overlay::Join { text, error, busy } => match k.code {
                KeyCode::Enter if nori_core::remote::is_invite(text) => {
                    *busy = true;
                    *error = None;
                    let link = text.trim().to_string();
                    self.cmds.push(Cmd::JoinJam(link));
                }
                KeyCode::Enter => *error = Some(crate::text::NOT_AN_INVITE.into()),
                KeyCode::Esc => self.overlay = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
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

    pub fn do_action(&mut self, a: Action) {
        let now = Instant::now();
        match a {
            Action::Quit => {
                self.quit = true;
                self.cmds.push(Cmd::Quit);
            }
            Action::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            Action::Join => self.join(),
            // A jam guest's keys reach what its role does (Spotify's Jam): an admin's the host's playback, a
            // guest's play and pause its own listening; shuffle, repeat and removals are the host's.
            Action::TogglePlay if !self.offers(|c| c.play_pause) => self.dirty = false,
            Action::Next | Action::Previous if !self.offers(|c| c.skip) => self.dirty = false,
            Action::SeekBack | Action::SeekForward | Action::SeekBackLong | Action::SeekForwardLong if !self.offers(|c| c.seek) => self.dirty = false,
            Action::Shuffle | Action::Repeat if self.guest() => self.dirty = false,
            Action::MoveUp | Action::MoveDown if self.queue_in_focus() && !self.offers(|c| c.reorder) => self.dirty = false,
            Action::Remove | Action::Undo if self.guest() && self.queue_in_focus() => self.dirty = false,
            Action::TogglePlay => self.cmds.push(Cmd::Toggle),
            Action::Next => self.cmds.push(Cmd::Next),
            Action::Previous => self.cmds.push(Cmd::Previous),
            Action::SeekBack => self.seek_by(-5_000, now),
            Action::SeekForward => self.seek_by(5_000, now),
            Action::SeekBackLong => self.seek_by(-30_000, now),
            Action::SeekForwardLong => self.seek_by(30_000, now),
            Action::VolumeUp => self.set_volume(self.volume + 0.05),
            Action::VolumeDown => self.set_volume(self.volume - 0.05),
            Action::Search => self.open_nav(Nav::Search),
            Action::Curve => self.cmds.push(Cmd::Curve),
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
                let (next, said) = match self.repeat() {
                    REPEAT_OFF => (REPEAT_ALL, "Repeat all"),
                    REPEAT_ALL => (REPEAT_ONE, "Repeat one"),
                    _ => (REPEAT_OFF, "Repeat off"),
                };
                self.cmds.push(Cmd::Repeat(next));
                self.say(said, false);
            }
            Action::Go(n) => {
                if let Some(v) = GO.get(n as usize).filter(|v| self.nav().contains(&nav_of(**v))) {
                    self.go(*v);
                    self.focus = Focus::Main;
                }
            }
            Action::Panel(p) => self.set_panel(p),
            Action::Full => self.toggle_full(),
            Action::NextPane | Action::PreviousPane => self.cycle(a == Action::NextPane),
            Action::Back => self.back(),
            Action::Refresh => self.refresh(),
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
            Action::GroupBack | Action::GroupOn if self.view == View::Settings => {
                let g = self.settings.group_at() as isize + if a == Action::GroupOn { 1 } else { -1 };
                let last = crate::settings_view::GROUPS.len() as isize - 1;
                self.settings.jump(g.clamp(0, last) as usize);
            }
            Action::GroupBack | Action::GroupOn => self.dirty = false,
            Action::Decrease | Action::Increase => {
                let up = a == Action::Increase;
                match self.view {
                    View::Equalizer => self.eq_step(up),
                    View::Settings => {
                        let cmds = self.settings.step(up);
                        self.cmds.extend(cmds);
                    }
                    _ => {}
                }
            }
            Action::Remove if self.focus == Focus::Main && self.view == View::Equalizer => {
                if let Some(crate::settings_view::EqRow::Band(i)) = crate::settings_view::eq_rows(&self.prefs).get(self.eq_sel.at) {
                    self.cmds.push(Cmd::Sound(SoundToolCmd::RemoveBand(*i as u32)));
                }
            }
            Action::Remove if self.focus == Focus::Main && self.view == View::Downloads => {
                if let Some(Item::Song(songs, i)) = self.selected() {
                    self.cmds.push(Cmd::DownloadRemove(songs[i].id.clone()));
                }
            }
            Action::Remove if self.queue_in_focus() => match self.queue_rows().get(self.queue_sel.at).copied() {
                Some(QueueRow::Song(i)) => {
                    // Another device's queue has its own undo.
                    self.taken = self.queue.as_ref().filter(|_| self.devices.active.is_none()).and_then(|q| q.songs.get(i)).map(|s| s.id.clone());
                    self.cmds.push(Cmd::Remove(i));
                }
                Some(row) => match row.ask().and_then(|k| self.ask_request(k)) {
                    Some(request) => self.cmds.push(Cmd::JamDecide(request, false)),
                    None => self.dirty = false,
                },
                None => self.dirty = false,
            },
            Action::Remove => self.dirty = false,
            Action::Jam => self.jam_with_selected(),
            Action::Undo if self.queue_in_focus() => match self.taken.take() {
                Some(id) => self.cmds.push(Cmd::Restore(id)),
                None => self.say("Nothing to put back", false),
            },
            Action::Undo => self.dirty = false,
            Action::MoveUp | Action::MoveDown if !self.queue_in_focus() => self.dirty = false,
            Action::MoveUp | Action::MoveDown => self.queue_move(a == Action::MoveDown),
            _ => self.list_action(a),
        }
    }

    fn seek_by(&mut self, delta: i64, now: Instant) {
        let Some(song) = &self.song else { return };
        let len = song.duration as i64 * 1000;
        let to = (self.now.position(now) + delta).clamp(0, (len - 1000).max(0));
        self.sought(to);
    }

    /// Seeks to `to`, shown at once and held until the engine's status catches up.
    fn sought(&mut self, to: i64) {
        let now = Instant::now();
        self.now.position_ms = to;
        self.now.at = now;
        self.seek_hold = Some((to, now));
        self.cmds.push(Cmd::Seek(to));
    }

    /// Takes the engine's status. After a seek the status lags: the target is kept until the status is
    /// near it, or `SEEK_HOLD` passes (the seek landed elsewhere).
    pub fn follow_now(&mut self, n: Now) {
        if let Some((to, at)) = self.seek_hold {
            let t = Instant::now();
            let there = (n.position(t) - to).abs() < 1_500;
            if !there && at.elapsed() < SEEK_HOLD {
                let held = self.now.position(t).max(to);
                self.now = Now { position_ms: held, at: t, ..n };
                return;
            }
            self.seek_hold = None;
        }
        self.now = n;
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

    /// Back: out of the full player, out of a page, then from the page to the sidebar.
    fn back(&mut self) {
        if self.full {
            self.full = false;
            return;
        }
        match self.focus {
            Focus::Panel if self.lyrics_sel.is_some() && self.panel == Some(Panel::Lyrics) => self.lyrics_sel = None,
            Focus::Panel => self.focus = Focus::Main,
            Focus::Main => {
                if self.pages.pop().is_none() {
                    self.focus = Focus::Side;
                }
            }
            Focus::Side => self.dirty = false,
        }
    }

    fn refresh(&mut self) {
        if self.focus == Focus::Side {
            self.library.playlists = Load::Idle;
            return self.want_playlists();
        }
        if self.lyrics_focused() {
            self.lyrics_for = None;
            return self.want_lyrics();
        }
        if let Some(p) = self.page() {
            self.cmds.push(Cmd::Load(p.req()));
            return;
        }
        let l = &mut self.library;
        match self.view {
            View::Home => {
                self.home.asked = false;
                self.library.playlists = Load::Idle;
            }
            View::Albums => l.albums = Load::Idle,
            View::Artists => l.artists = Load::Idle,
            View::Songs => l.songs = Load::Idle,
            View::Settings => self.settings.facts_asked = false,
            _ => {}
        }
        self.go(self.view);
    }

    // ---- lists ----

    /// The focused list's selection and length; None for home shelves and lyrics.
    fn list(&mut self) -> Option<(&mut Sel, usize)> {
        match self.focus {
            Focus::Side => {
                let len = self.nav().len();
                return Some((&mut self.side, len));
            }
            Focus::Panel => {
                return match self.panel? {
                    Panel::Queue => {
                        let len = self.queue_rows().len();
                        Some((&mut self.queue_sel, len))
                    }
                    Panel::Devices => {
                        let len = self.devices.rows().len();
                        Some((&mut self.devices.sel, len))
                    }
                    Panel::Playing => {
                        let len = self.up_next().len();
                        Some((&mut self.up_next_sel, len))
                    }
                    Panel::Lyrics => None,
                };
            }
            Focus::Main => {}
        }
        if self.page().is_some() {
            let p = self.page_mut()?;
            let len = p.len();
            return Some((p.sel(), len));
        }
        let l = &self.library;
        let lens = (l.albums.ready().map_or(0, Vec::len), l.artists.ready().map_or(0, Vec::len), l.songs.ready().map_or(0, Vec::len));
        Some(match self.view {
            View::Albums => (&mut self.library.albums_sel, lens.0),
            View::Artists => (&mut self.library.artists_sel, lens.1),
            View::Songs => (&mut self.library.songs_sel, lens.2),
            View::Search => {
                let len = self.search.rows().len();
                (&mut self.search.sel, len)
            }
            View::Downloads => {
                let len = self.download_rows().len();
                (&mut self.downloads_sel, len)
            }
            View::Equalizer => (&mut self.eq_sel, crate::settings_view::eq_rows(&self.prefs).len()),
            View::Settings => {
                self.settings.pages(&self.prefs);
                return Some(self.settings.list());
            }
            View::Home | View::Login => return None,
        })
    }

    fn list_action(&mut self, a: Action) {
        if self.lyrics_focused() {
            return self.lyrics_action(a);
        }
        if self.focus == Focus::Main && self.page().is_none() && self.view == View::Home {
            return self.home_action(a);
        }
        let grid = self.grid_focused();
        let cols = if grid { self.shown.cols.max(1) as isize } else { 1 };
        let Some((sel, len)) = self.list() else { return };
        let page = 10;
        match a {
            Action::Up => sel.by(-cols, len),
            Action::Down => sel.by(cols, len),
            Action::Left => sel.by(-1, len),
            Action::Right => sel.by(1, len),
            Action::Top => sel.to(0, len),
            Action::Bottom => sel.to(len.saturating_sub(1), len),
            Action::PageUp => sel.by(-page * cols, len),
            Action::PageDown => sel.by(page * cols, len),
            _ => return self.act_on_selected(a),
        }
        let down = !matches!(a, Action::Up | Action::PageUp | Action::Top | Action::Left);
        if self.focus == Focus::Panel && self.panel == Some(Panel::Devices) {
            self.devices.settle(down);
        }
        if self.focus == Focus::Main && self.page().is_none() {
            match self.view {
                View::Settings => self.settings.skip_titles(down),
                View::Search => self.search.settle(down),
                _ => {}
            }
        }
        self.more();
    }

    fn home_action(&mut self, a: Action) {
        let h = &mut self.home;
        match a {
            Action::Up => h.step_shelf(-1),
            Action::Down => h.step_shelf(1),
            Action::PageUp | Action::Top => h.step_shelf(-99),
            Action::PageDown | Action::Bottom => h.step_shelf(99),
            Action::Left => h.step_along(-1),
            Action::Right => h.step_along(1),
            _ => self.act_on_selected(a),
        }
    }

    /// Requests the next page when the selection nears the end of a paged list.
    fn more(&mut self) {
        if self.focus != Focus::Main || self.page().is_some() {
            return;
        }
        let l = &mut self.library;
        match self.view {
            View::Albums => {
                let len = l.albums.ready().map_or(0, Vec::len);
                if l.albums_sel.at + 50 >= len && l.albums_more {
                    l.albums_more = false;
                    self.cmds.push(Cmd::Load(Req::Albums { offset: len as u32 }));
                }
            }
            View::Songs => {
                let len = l.songs.ready().map_or(0, Vec::len);
                if l.songs_sel.at + 50 >= len && l.songs_more {
                    l.songs_more = false;
                    self.cmds.push(Cmd::Load(Req::Songs { offset: len as u32 }));
                }
            }
            _ => {}
        }
    }

    /// The selected item in the focused part.
    pub fn selected(&self) -> Option<Item> {
        match self.focus {
            Focus::Side => {
                return match self.nav().get(self.side.at)? {
                    Nav::Playlist(i) => self.library.playlists.ready()?.get(*i).cloned().map(Item::Playlist),
                    _ => None,
                };
            }
            Focus::Panel => {
                let q = self.queue.as_ref()?;
                let i = match self.panel? {
                    Panel::Queue => self.queue_selected_index()?,
                    Panel::Playing => *self.up_next().get(self.up_next_sel.at)?,
                    Panel::Lyrics | Panel::Devices => return None,
                };
                return q.songs.get(i).cloned().map(|s| Item::Song(vec![s], 0));
            }
            Focus::Main => {}
        }
        if let Some(p) = self.page() {
            return match p {
                Page::Album { sel, .. } | Page::Playlist { sel, .. } => p.songs().filter(|s| sel.at < s.len()).map(|s| Item::Song(s.to_vec(), sel.at)),
                Page::Artist { detail, sel, .. } => detail.ready().and_then(|d| d.albums.get(sel.at).cloned()).map(Item::Album),
            };
        }
        let l = &self.library;
        match self.view {
            View::Home => self.home.album().cloned().map(Item::Album),
            View::Albums => l.albums.ready()?.get(l.albums_sel.at).cloned().map(Item::Album),
            View::Artists => l.artists.ready()?.get(l.artists_sel.at).cloned().map(Item::Artist),
            View::Songs => l.songs.ready().filter(|s| l.songs_sel.at < s.len()).map(|s| Item::Song(s.clone(), l.songs_sel.at)),
            View::Search => match self.search.rows().get(self.search.sel.at)? {
                // Search songs play alone: the list is no page's queue and may hold provider songs.
                SearchRow::Song(s) => Some(Item::Song(vec![(*s).clone()], 0)),
                SearchRow::Album(a) => Some(Item::Album((*a).clone())),
                SearchRow::Artist(a) => Some(Item::Artist((*a).clone())),
                SearchRow::Title(..) => None,
            },
            View::Downloads => {
                let rows = self.download_rows();
                let (songs, i) = rows.get(self.downloads_sel.at).and_then(|r| r.1)?;
                Some(Item::Song(songs.to_vec(), i))
            }
            _ => None,
        }
    }

    fn act_on_selected(&mut self, a: Action) {
        match self.focus {
            Focus::Side if a == Action::Open => {
                if let Some(n) = self.nav().get(self.side.at).copied() {
                    self.open_nav(n);
                }
                return;
            }
            Focus::Panel if a == Action::Open => {
                let i = match self.panel {
                    Some(Panel::Queue) => match self.queue_rows().get(self.queue_sel.at).copied() {
                        Some(QueueRow::Song(_)) if self.guest() => None,
                        Some(QueueRow::Song(i)) => Some(i),
                        Some(QueueRow::Jam | QueueRow::Listeners) => return self.invite(),
                        Some(QueueRow::Listen | QueueRow::AlongNote(_)) => return self.cmds.push(Cmd::Listen(self.devices.listening() == Listening::Watching)),
                        Some(QueueRow::End) if self.guest() => return self.cmds.push(Cmd::LeaveJam),
                        Some(QueueRow::End) => return self.cmds.push(Cmd::JamEnd),
                        Some(row) => {
                            if let Some(request) = row.ask().and_then(|k| self.ask_request(k)) {
                                self.cmds.push(Cmd::JamDecide(request, true));
                            }
                            return;
                        }
                        None => None,
                    },
                    Some(Panel::Playing) => self.up_next().get(self.up_next_sel.at).copied(),
                    Some(Panel::Devices) => return self.device_open(),
                    _ => None,
                };
                if let Some(i) = i {
                    self.cmds.push(Cmd::Jump(i));
                }
                return;
            }
            Focus::Main if self.page().is_none() && self.view == View::Settings => {
                if a == Action::Open {
                    self.settings_open();
                }
                return;
            }
            Focus::Main if self.page().is_none() && self.view == View::Equalizer => {
                if a == Action::Open {
                    self.eq_open();
                }
                return;
            }
            _ => {}
        }
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
            // Hearts and downloads are the account's.
            (Action::Download | Action::Star, _) if !self.rules.account => self.dirty = false,
            (Action::Download, item) => match item {
                Item::Song(songs, i) => self.cmds.push(Cmd::Download(vec![songs[i].clone()])),
                Item::Album(al) => self.cmds.push(Cmd::DownloadFetch(Fetch::Album(al.id))),
                Item::Artist(ar) => self.cmds.push(Cmd::DownloadFetch(Fetch::Artist(ar.id))),
                Item::Playlist(p) => self.cmds.push(Cmd::DownloadFetch(Fetch::Playlist(p.id))),
            },
            (Action::Star, item) => {
                let (kind, id, listed) = match item {
                    Item::Song(songs, i) => (Starrable::Song, songs[i].id.clone(), songs[i].starred),
                    Item::Album(al) => (Starrable::Album, al.id, al.starred),
                    Item::Artist(ar) => (Starrable::Artist, ar.id, ar.starred),
                    Item::Playlist(_) => return,
                };
                self.star(kind, id, listed);
            }
            _ => self.dirty = false,
        }
    }

    /// A song picked from a list, per the `tap_action` setting (not offered here; default PlayList).
    fn tap(&mut self, songs: Vec<Song>, i: usize) {
        match self.prefs.tap_action {
            TapAction::PlayOne => self.cmds.push(Cmd::Play { songs: vec![songs[i].clone()], start: 0, shuffle: false, from: None }),
            TapAction::Queue => self.cmds.push(Cmd::Enqueue(vec![songs[i].clone()], false)),
            TapAction::PlayNext => self.cmds.push(Cmd::Enqueue(vec![songs[i].clone()], true)),
            TapAction::PlayList => {
                let from = self.origin_here();
                self.cmds.push(Cmd::Play { songs, start: i, shuffle: false, from })
            }
        }
    }

    /// The page the focused song list belongs to, as a queue origin.
    fn origin_here(&self) -> Option<PageOrigin> {
        if self.focus != Focus::Main {
            return None;
        }
        if let Some(page) = self.page() {
            return Some(match page {
                Page::Album { id, .. } => PageOrigin::new(OriginKind::Album, id.as_str()),
                Page::Artist { id, .. } => PageOrigin::new(OriginKind::Artist, id.as_str()),
                Page::Playlist { id, .. } => PageOrigin::new(OriginKind::Playlist, id.as_str()),
            });
        }
        match self.view {
            View::Songs => Some(PageOrigin::new(OriginKind::Songs, "")),
            View::Search => Some(PageOrigin::new(OriginKind::Search, self.search.text.as_str())),
            View::Downloads => Some(PageOrigin::new(OriginKind::Downloads, "")),
            _ => None,
        }
    }

    fn play_all(&mut self, shuffle: bool) {
        if self.focus == Focus::Main && self.page().is_some() {
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

    /// Flips an item's heart (`listed`: its record's flag), marked at once; the core's marks follow
    /// ([`Msg::Starred`]).
    fn star(&mut self, kind: Starrable, id: String, listed: bool) {
        let on = !self.starred(kind, &id, listed);
        match self.devices.hearts.get_mut(&id).filter(|_| kind == Starrable::Song) {
            Some(heart) => *heart = on,
            None => {
                self.marks.mark(kind, id.clone(), on);
            }
        }
        self.cmds.push(Cmd::Star(kind, id, on));
    }

    /// Whether an item shows starred (`listed`: its record's flag): a song in the queue of another
    /// device playing as that device shows it, else as this session marked it.
    pub fn starred(&self, kind: Starrable, id: &str, listed: bool) -> bool {
        match self.devices.hearts.get(id).filter(|_| kind == Starrable::Song) {
            Some(on) => *on,
            None => self.marks.starred(kind, id, listed),
        }
    }

    /// Flips the star on the page's album or artist.
    fn star_page(&mut self) {
        let (kind, id, listed) = match self.page() {
            Some(Page::Album { detail: Load::Ready(d), .. }) => (Starrable::Album, d.album.id.clone(), d.album.starred),
            Some(Page::Artist { detail: Load::Ready(d), .. }) => (Starrable::Artist, d.artist.id.clone(), d.artist.starred),
            _ => return,
        };
        self.star(kind, id, listed);
    }

    // ---- the queue ----

    /// Queue list indexes in play order.
    pub fn queue_order(&self) -> Vec<usize> {
        self.queue.as_ref().map_or_else(Vec::new, |q| q.order.iter().map(|&i| i as usize).collect())
    }

    fn queue_in_focus(&self) -> bool {
        self.focus == Focus::Panel && self.panel == Some(Panel::Queue) && !self.full
    }

    fn queue_selected_index(&self) -> Option<usize> {
        if !self.queue_in_focus() {
            return None;
        }
        match self.queue_rows().get(self.queue_sel.at)? {
            QueueRow::Song(i) => Some(*i),
            _ => None,
        }
    }

    /// The queue panel's rows: the hosted jam's header, requests and end, then the songs in play order.
    pub fn queue_rows(&self) -> Vec<QueueRow> {
        let mut rows = Vec::new();
        if let Some(j) = &self.devices.jam {
            rows.push(QueueRow::Jam);
            if !self.devices.listeners().is_empty() {
                rows.push(QueueRow::Listeners);
            }
            let asks = self.devices.asks();
            if !j.hosting {
                rows.push(QueueRow::Listen);
                rows.extend((0..self.along_note().len()).map(QueueRow::AlongNote));
                if !asks.is_empty() {
                    rows.push(QueueRow::Asking);
                }
            }
            for (k, p) in asks.iter().enumerate() {
                rows.push(QueueRow::Ask(k));
                // The host decides, and its server downloads the song.
                if p.provider && j.hosting {
                    rows.extend((0..self.downloads_note().len()).map(|n| QueueRow::Downloads(k, n)));
                }
            }
            rows.push(QueueRow::End);
        }
        rows.extend(self.queue_order().into_iter().map(QueueRow::Song));
        rows
    }

    /// The note under a provider's song asked for, in the lines the queue panel has room for.
    pub fn downloads_note(&self) -> Vec<String> {
        crate::ui::wrap(crate::text::JAM_DOWNLOADS, (self.shown.panel_w as usize).saturating_sub(QUEUE_NOTE_INDENT + 1).max(12))
    }

    /// Why this guest, having asked to listen along, does not, in the lines the queue panel has room for.
    pub fn along_note(&self) -> Vec<String> {
        let note = crate::text::jam_along(self.devices.listening());
        if note.is_empty() {
            return Vec::new();
        }
        crate::ui::wrap(note, (self.shown.panel_w as usize).saturating_sub(QUEUE_NOTE_INDENT + 1).max(12))
    }

    /// The request id of the jam's request `k` ([`Devices::asks`]) while hosting: only the host decides.
    fn ask_request(&self, k: usize) -> Option<u64> {
        self.devices.jam.as_ref().filter(|j| j.hosting)?;
        self.devices.asks().get(k).map(|p| p.request)
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

    /// List indexes after the current song, in play order.
    pub fn up_next(&self) -> Vec<usize> {
        let order = self.queue_order();
        let current = self.queue.as_ref().map_or(-1, |q| q.index);
        match order.iter().position(|&i| i as i32 == current) {
            Some(p) => order[p + 1..].to_vec(),
            None => order,
        }
    }

    // ---- other devices and the jam ----

    /// Enter on a devices panel row: the music moves there, or the jam starts or shows its invite.
    fn device_open(&mut self) {
        let d = &self.devices;
        match d.rows().get(d.sel.at).copied() {
            Some(DeviceRow::Here) => self.cmds.push(Cmd::Pick(None)),
            Some(DeviceRow::Device(i) | DeviceRow::Playing(i)) => {
                if let Some(id) = d.list.get(i).map(|x| x.id.clone()) {
                    self.cmds.push(Cmd::Pick(Some(id)));
                }
            }
            Some(DeviceRow::Jam) if d.jam.is_some() => self.invite(),
            Some(DeviceRow::Jam) => self.jam_start(),
            Some(DeviceRow::Join) => self.join(),
            None => self.dirty = false,
        }
    }

    /// Shows the hosted jam's invite.
    fn invite(&mut self) {
        if let Some(link) = self.devices.jam.as_ref().and_then(|j| j.link.clone()) {
            self.overlay = Some(Overlay::Invite { link });
        }
    }

    /// Asks for an invite link, to join someone's jam.
    fn join(&mut self) {
        self.overlay = Some(Overlay::Join { text: String::new(), error: None, busy: false });
    }

    /// Starts a jam; the one hosted shows its invite instead, and where jams cannot start, why. A guest
    /// starts none.
    fn jam_start(&mut self) {
        if self.guest() {
            self.dirty = false;
        } else if self.devices.jam.is_some() {
            self.invite();
        } else if self.devices.jams {
            self.cmds.push(Cmd::JamStart);
        } else {
            self.say(if self.devices.jams_unsupported { crate::text::JAM_UNSUPPORTED } else { crate::text::JAMS_OFF }, false);
        }
    }

    /// Starts a jam around the selected song, album, artist or playlist, which plays (a jam alone where
    /// nothing on the page is selected).
    fn jam_with_selected(&mut self) {
        let item = if self.focus == Focus::Main { self.selected() } else { None };
        let play = match item {
            Some(Item::Song(songs, i)) => Some(Cmd::Play { songs, start: i, shuffle: false, from: self.origin_here() }),
            Some(Item::Album(a)) => Some(Cmd::PlayFetch(Fetch::Album(a.id), false)),
            Some(Item::Artist(a)) => Some(Cmd::PlayFetch(Fetch::Artist(a.id), false)),
            Some(Item::Playlist(p)) => Some(Cmd::PlayFetch(Fetch::Playlist(p.id), false)),
            None => None,
        };
        if self.devices.jam.is_none() && self.devices.jams {
            self.cmds.extend(play);
        }
        self.jam_start();
    }

    // ---- downloads ----

    /// Downloads page rows.
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
        // `active` is past the end after the last line.
        let active = (l.clock.shown().active.max(0) as usize).min(len.saturating_sub(1));
        let at = self.lyrics_sel.unwrap_or(active);
        match a {
            Action::Up => self.lyrics_sel = Some(at.saturating_sub(1)),
            Action::Down => self.lyrics_sel = Some((at + 1).min(len.saturating_sub(1))),
            Action::Top => self.lyrics_sel = Some(0),
            Action::Bottom => self.lyrics_sel = Some(len.saturating_sub(1)),
            Action::Open if l.pick.lyrics.synced => {
                let to = l.clock.tap(at);
                self.lyrics_sel = None;
                self.sought(to);
                self.lyrics_wake = Some(Instant::now());
            }
            _ => {}
        }
    }

    // ---- the settings and the equalizer ----

    fn settings_open(&mut self) {
        match self.settings.open() {
            Some(Opened::Cmds(c)) => self.cmds.extend(c),
            Some(Opened::Overlay(o)) => self.overlay = Some(o),
            Some(Opened::View(v)) => self.go(v),
            Some(Opened::Own(switch)) => self.own_toggle(switch),
            Some(Opened::Login) => {
                self.login = Login::default();
                self.view = View::Login;
            }
            None => self.dirty = false,
        }
    }

    fn own_toggle(&mut self, switch: Switch) {
        match switch {
            Switch::Mouse => self.do_action(Action::Mouse),
            Switch::Images => self.do_action(Action::Images),
            Switch::CardCovers => {
                self.card_covers = !self.card_covers;
                self.cmds.push(Cmd::CardCovers(self.card_covers));
            }
            Switch::Setting(_) => {}
        }
        self.settings.invalidate();
    }

    /// A sound change was stored. Returns true when the engine should now take the low-latency buffer
    /// (first real edit on an enabled equalizer; `rules::equalizer_tuning`), so later edits are heard
    /// at once. Merely opening the equalizer does not.
    pub fn sound_edited(&mut self) -> bool {
        self.touched |= equalizer_tuning(self.view == View::Equalizer, true, self.prefs.eq_enabled);
        self.tune()
    }

    /// Updates `tuning` from `rules::equalizer_tuning`; pushes the release command, returns true on take.
    fn tune(&mut self) -> bool {
        let want = equalizer_tuning(self.view == View::Equalizer, self.touched, self.prefs.eq_enabled);
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
                self.overlay = Some(Overlay::Picker { title: "Presets".into(), options, sel: Sel::default(), target: Target::Preset });
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
        // Last drawn wins: overlays are drawn last.
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
                self.click(h, m.column, m.row, again);
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::Seek(_)) => self.drag = Some(Drag::Seek(share(self.seek_rect, m.column))),
                Some(Drag::Volume) => self.set_volume(share(self.volume_rect, m.column)),
                Some(Drag::Band(i)) => self.band_to(i, m.row),
                None => self.dirty = false,
            },
            MouseEventKind::Up(MouseButton::Left) => match self.drag.take() {
                Some(Drag::Seek(s)) => self.seek_share(s),
                Some(Drag::Volume | Drag::Band(_)) => {}
                None => self.dirty = false,
            },
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d = if m.kind == MouseEventKind::ScrollDown { 1 } else { -1 };
                match self.hit(m.column, m.row) {
                    // The wheel changes an equalizer control, up for more.
                    Some(Hit::Row(ListRef::Eq, i)) => {
                        self.focus = Focus::Main;
                        self.eq_sel.at = i;
                        self.eq_step(d < 0);
                    }
                    Some(Hit::Row(l, _) | Hit::List(l)) => self.scroll(l, d),
                    Some(Hit::Nav(_)) => self.scroll(ListRef::Side, d),
                    Some(Hit::Volume) => self.set_volume(self.volume - d as f32 * 0.05),
                    _ => self.dirty = false,
                }
            }
            _ => self.dirty = false,
        }
    }

    fn seek_share(&mut self, share: f32) {
        let Some(song) = &self.song else { return };
        let to = (song.duration as f32 * 1000.0 * share) as i64;
        self.sought(to);
    }

    fn scroll(&mut self, l: ListRef, d: isize) {
        match l {
            ListRef::Help => {
                if let Some(Overlay::Help { scroll }) = &mut self.overlay {
                    *scroll = (*scroll as isize + d * 3).max(0) as usize;
                }
            }
            ListRef::Lyrics => {
                let len = self.lyrics.as_ref().map_or(0, |l| l.pick.lyrics.lines.len());
                let at = self.lyrics_sel.unwrap_or_else(|| self.lyrics.as_ref().map_or(0, |l| l.clock.shown().active.max(0) as usize));
                self.lyrics_sel = Some((at as isize + d * 3).clamp(0, len.saturating_sub(1) as isize) as usize);
            }
            ListRef::Picker => {
                if let Some(Overlay::Picker { sel, options, .. }) = &mut self.overlay {
                    sel.by(d * 3, options.len());
                }
            }
            ListRef::Profiles => {}
            ListRef::Shelf(s) => {
                self.focus = Focus::Main;
                self.home.shelf = s;
                self.home.step_along(d);
            }
            _ => {
                self.focus = l.focus();
                let grid = self.grid_focused();
                let step = if grid { self.shown.cols.max(1) as isize } else { 3 };
                if let Some((sel, len)) = self.list() {
                    sel.by(d * step, len);
                }
                if self.view == View::Settings && l == ListRef::Settings {
                    self.settings.skip_titles(d > 0);
                }
                if self.view == View::Search && l == ListRef::Search {
                    self.search.settle(d > 0);
                }
                self.more();
            }
        }
    }

    fn click(&mut self, h: Hit, col: u16, row: u16, again: bool) {
        match h {
            Hit::Nav(i) => {
                if let Some(n) = self.nav().get(i).copied() {
                    self.open_nav(n);
                }
            }
            Hit::Seek => self.drag = Some(Drag::Seek(share(self.seek_rect, col))),
            Hit::Volume => {
                self.drag = Some(Drag::Volume);
                self.set_volume(share(self.volume_rect, col));
            }
            Hit::SearchField => {
                if self.view != View::Search {
                    self.go(View::Search);
                }
                self.focus = Focus::Main;
                self.search.editing = true;
            }
            Hit::LoginField(i) => {
                self.login.on_list = false;
                self.login.focus = i;
            }
            Hit::Group(g) => {
                self.focus = Focus::Main;
                self.settings.jump(g);
            }
            Hit::Button(b) => self.button(b),
            Hit::List(l) => {
                if !matches!(l, ListRef::Help | ListRef::Picker | ListRef::Profiles) && !self.full {
                    self.focus = l.focus();
                } else {
                    self.dirty = false;
                }
            }
            Hit::Row(ListRef::Eq, i) => {
                self.focus = Focus::Main;
                self.eq_sel.at = i;
                let rows = crate::settings_view::eq_rows(&self.prefs);
                match rows.get(i) {
                    Some(r) if r.band(&self.prefs).is_some() => {
                        self.drag = Some(Drag::Band(i));
                        self.band_to(i, row);
                    }
                    Some(r) if r.clicks() || again => self.eq_open(),
                    _ => {}
                }
            }
            Hit::Row(l, i) => self.click_row(l, i, again),
        }
    }

    /// Sets fader `i` from pointer row `y` on its track.
    fn band_to(&mut self, i: usize, y: u16) {
        let (top, h) = self.eq_track;
        if h < 2 {
            return;
        }
        let r = nori_core::settings::EQ_RANGES.gain;
        let share = (y.saturating_sub(top).min(h - 1)) as f32 / (h - 1) as f32;
        let db = r.max - share * (r.max - r.min);
        let rows = crate::settings_view::eq_rows(&self.prefs);
        if let Some(c) = rows.get(i).and_then(|row| row.set_gain(&self.prefs, db)) {
            self.cmds.push(c);
        }
    }

    fn button(&mut self, b: Button) {
        match b {
            Button::Previous => self.do_action(Action::Previous),
            Button::Toggle => self.do_action(Action::TogglePlay),
            Button::Next => self.do_action(Action::Next),
            Button::Shuffle => self.do_action(Action::Shuffle),
            Button::Repeat => self.do_action(Action::Repeat),
            Button::Panel(p) => self.set_panel(p),
            Button::Full => self.toggle_full(),
            Button::PlayAll => self.play_all(false),
            Button::ShuffleAll => self.play_all(true),
            Button::Star => self.star_page(),
            Button::Download => {
                let what = match self.page() {
                    Some(Page::Album { id, .. }) => Fetch::Album(id.clone()),
                    Some(Page::Artist { id, .. }) => Fetch::Artist(id.clone()),
                    Some(Page::Playlist { id, .. }) => Fetch::Playlist(id.clone()),
                    None => return,
                };
                self.cmds.push(Cmd::DownloadFetch(what));
                self.say("Downloading…", false);
            }
            Button::StarSong => {
                if let Some(s) = self.song.as_ref().filter(|_| self.rules.account) {
                    let (id, listed) = (s.id.clone(), s.starred);
                    self.star(Starrable::Song, id, listed);
                }
            }
            Button::Back => {
                self.focus = Focus::Main;
                self.back();
            }
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
            ListRef::Shelf(s) => {
                self.focus = Focus::Main;
                self.home.shelf = s;
                if let Some(p) = self.home.pos.get_mut(s) {
                    *p = i;
                }
                return self.act_on_selected(Action::Open);
            }
            _ => {}
        }
        self.focus = l.focus();
        if let Some((sel, len)) = self.list() {
            sel.to(i, len);
        }
        // Cards and switches act on one click; songs on a second.
        let artist_cards = l == ListRef::Page && matches!(self.page(), Some(Page::Artist { .. }));
        let settings_now = l == ListRef::Settings && self.settings.clicks_open();
        if again || l.opens_on_click() || artist_cards || settings_now {
            self.act_on_selected(Action::Open);
        }
    }
}

/// The sidebar entry of a root view.
fn nav_of(view: View) -> Nav {
    match view {
        View::Home | View::Login => Nav::Home,
        View::Search => Nav::Search,
        View::Albums => Nav::Albums,
        View::Artists => Nav::Artists,
        View::Songs => Nav::Songs,
        View::Downloads => Nav::Downloads,
        View::Equalizer => Nav::Equalizer,
        View::Settings => Nav::Settings,
    }
}

/// `col`'s position across `r`, 0 to 1.
fn share(r: Rect, col: u16) -> f32 {
    if r.width == 0 {
        return 0.0;
    }
    (col.saturating_sub(r.x) as f32 / r.width.saturating_sub(1).max(1) as f32).clamp(0.0, 1.0)
}

/// A downloads page row: a heading, or a song as (its list, index).
pub type DownloadRow<'a> = (String, Option<(&'a [Song], usize)>);

/// How long a status note shows.
pub const NOTE_FOR: Duration = Duration::from_secs(4);

/// Longest a seek target overrides a lagging engine status.
const SEEK_HOLD: Duration = Duration::from_secs(3);

/// Repeat modes, numbered as `nori_player::playlist`.
pub const REPEAT_OFF: u8 = 0;
pub const REPEAT_ONE: u8 = 1;
pub const REPEAT_ALL: u8 = 2;
