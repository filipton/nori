//! The session behind the screen (nori-host's), its reports worded as [`Msg`]s, and the screens' reads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use nori_core::browse::AlbumSort;
use nori_core::cache_policy::{Page, Read};
use nori_core::race::LyricsPick;
use nori_core::search::SearchView;
use nori_core::settings::SavedServer;
use nori_core::transport::NetError;
use nori_core::{AlbumDetail, ArtistDetail, PlaylistDetail, Song};
use nori_covers::memory::Image;
pub use nori_host::session::{Audio, Chore};
use nori_host::session::{read_pages, Note, Said};
pub use nori_host::{db_path, Controls, Fetch};
use nori_http::Http;
use nori_look::cover::CoverColours;
use nori_engine::AudioOutput;
use nori_output_cpal::CpalOutput;
use ratatui::crossterm::event::{KeyEvent, MouseEvent};

use crate::settings_view::{Facts, Storage};
use crate::text::{count, net_error};

pub use nori_core::transport::block_on;

/// Everything that wakes the event loop.
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    /// Terminal (or tmux pane) focus gained or lost.
    Focus(bool),
    Engine(nori_engine::Event),
    Data(Req, Result<Data, String>),
    /// A decoded cover by key, with derived colours when asked for.
    Cover { art: String, image: Arc<Image>, colours: Option<Box<CoverColours>> },
    /// One of possibly several lyrics answers for `song`, each better than the last.
    Lyrics { song: String, pick: LyricsPick },
    Search(SearchView),
    /// A status bar message.
    Note { text: String, error: bool },
    /// The login form's result: the profile to keep, or the error.
    LoggedIn(Result<SavedServer, String>),
    /// Result of the reachability check made when a session opens.
    Reachable(Result<(), String>),
    /// Another device set the volume (0 to 1).
    Volume(f32),
    /// The core's star marks moved (a heart pressed here or on another device).
    Starred(nori_core::stars::StarMarks),
    /// The other devices, the one playing or the jam changed: read them again.
    Remote,
    /// Whether the jam asked for opened, or why not.
    Jam(Result<(), String>),
    /// Someone's jam joined (what the guest profile signs in with), or why not.
    Joined(Result<nori_core::remote::JamPass, String>),
    /// This guest left its jam.
    Left,
    /// A message from the session with this id; dropped once another session is open.
    From(u64, Box<Msg>),
}

impl Msg {
    /// A short description for the debug log, without lyrics, pixels or passwords.
    pub fn brief(&self) -> String {
        match self {
            Msg::Key(k) => format!("key {:?} {:?} {:?}", k.code, k.modifiers, k.kind),
            Msg::Mouse(m) => format!("mouse {:?} at {},{}", m.kind, m.column, m.row),
            Msg::Paste(p) => format!("paste of {} bytes", p.len()),
            Msg::Resize => "resize".into(),
            Msg::Focus(on) => format!("focus {}", if *on { "gained" } else { "lost" }),
            Msg::Engine(e) => format!("engine {e:?}"),
            Msg::Data(req, r) => format!("data {req:?} {}", if r.is_ok() { "ok" } else { "failed" }),
            Msg::Cover { art, image, colours } => format!("cover {art} {}x{}{}", image.width, image.height, if colours.is_some() { " with colours" } else { "" }),
            Msg::Lyrics { song, .. } => format!("lyrics for {song}"),
            Msg::Search(v) => format!("search results for {:?}", v.query),
            Msg::Note { text, error } => format!("note {text:?} error={error}"),
            Msg::LoggedIn(r) => format!("logged in: {}", r.is_ok()),
            Msg::Reachable(r) => format!("reachable: {}", r.is_ok()),
            Msg::Volume(v) => format!("volume {v}"),
            Msg::Starred(_) => "star marks".into(),
            Msg::Remote => "remote".into(),
            Msg::Jam(r) => format!("jam opened: {}", r.is_ok()),
            Msg::Joined(r) => format!("jam joined: {}", r.is_ok()),
            Msg::Left => "jam left".into(),
            Msg::From(id, m) => format!("session {id}: {}", m.brief()),
        }
    }
}

/// A screen's data request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Req {
    Home,
    Albums { offset: u32 },
    Artists,
    Playlists,
    Songs { offset: u32 },
    Album(String),
    Artist(String),
    Playlist(String),
    Downloads,
    Facts,
}

pub enum Data {
    /// Home shelf: index into `HOME_ROWS`, title, albums.
    HomeRow(usize, &'static str, Vec<nori_core::Album>),
    Albums(Vec<nori_core::Album>),
    Artists(Vec<nori_core::Artist>),
    Playlists(Vec<nori_core::Playlist>),
    Songs(Vec<Song>, bool),
    Album(Box<AlbumDetail>),
    Artist(Box<ArtistDetail>),
    Playlist(Box<PlaylistDetail>),
    Downloads(Box<Downloads>),
    Facts(Box<Facts>),
}

/// The downloads screen's sections.
#[derive(Default)]
pub struct Downloads {
    pub active: Vec<Song>,
    pub queued: Vec<Song>,
    pub failed: Vec<Song>,
    pub stored: Vec<Song>,
}

/// Home shelves: title and Subsonic album list type.
pub const HOME_ROWS: [(&str, AlbumSort); 5] = [
    ("Recently added", AlbumSort::Newest),
    ("Recently played", AlbumSort::Recent),
    ("Most played", AlbumSort::Frequent),
    ("Favorites", AlbumSort::Starred),
    ("Something random", AlbumSort::Random),
];

/// Albums requested per page.
pub const ALBUM_PAGE: u32 = 500;

/// Key prefix of album card covers in [`crate::art::Art`]: `thumb:<cover id>`.
pub const THUMB: &str = "thumb:";

/// Logs `draft` in (blocking). The core tries the second address and falls back to legacy auth, which
/// is then kept on the profile.
pub fn check_login(http: Arc<Http>, draft: SavedServer) -> Result<SavedServer, String> {
    let legacy = block_on(nori_core::client::login_check(http, nori_host::config(&draft), draft.alt_url.clone())).map_err(|e| match net_error(&e) {
        e if e.is_empty() => "the server did not answer".to_string(),
        e => e,
    })?;
    Ok(SavedServer { legacy_auth: legacy || draft.legacy_auth, ..draft })
}

/// The app's queue session over its settings: the client's object graph root, which every screen reads.
/// Global: the root of the terminal client's own object graph (as Kotlin's `Nori`), made once.
pub fn app() -> &'static Arc<nori_core::queue::Session> {
    static APP: std::sync::OnceLock<Arc<nori_core::queue::Session>> = std::sync::OnceLock::new();
    APP.get_or_init(|| Arc::new(nori_core::queue::Session::new(nori_core::settings_store::Settings::new())))
}

/// The terminal client's own settings, stored in the core's `app_kv`.
pub mod own {
    pub const MOUSE: &str = "tui.mouse";
    pub const IMAGES: &str = "tui.images";
    pub const CARD_COVERS: &str = "tui.cardCovers";
    pub const VOLUME: &str = "tui.volume";
    /// Output device name opened at start; empty for the system default.
    pub const DEVICE: &str = "tui.device";

    pub fn text(key: &str) -> Option<String> {
        crate::backend::app().settings.app_value(key).filter(|v| !v.is_empty())
    }

    pub fn flag(key: &str, default: bool) -> bool {
        crate::backend::app().settings.app_value(key).map_or(default, |v| v == "true")
    }

    pub fn number(key: &str, default: f32) -> f32 {
        crate::backend::app().settings.app_value(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn keep(key: &'static str, value: String) {
        crate::backend::app().settings.keep_app_value(key, value);
    }
}

pub struct Open<'a> {
    pub data: &'a Path,
    pub http: Arc<Http>,
    pub profile: SavedServer,
    pub device: Option<String>,
    pub images: bool,
    pub offline: bool,
    /// The process's media controls, driven by this session while it is open.
    pub mpris: Option<Arc<nori_mpris::Mpris>>,
    pub tx: Sender<Msg>,
}

/// The cpal device at `volume` (0 to 1).
fn sound(device: Option<&str>, volume: f32) -> (Box<dyn AudioOutput>, Arc<nori_host::Level>) {
    let card = match device {
        Some(name) => CpalOutput::with_device(name),
        None => CpalOutput::new(),
    };
    let level = card.volume();
    (Box::new(card), nori_host::Level::new(volume, Some(Box::new(move |v| level.set(v)))))
}

/// One open server profile; its messages come tagged with its id.
pub struct Session {
    pub id: u64,
    host: nori_host::session::Session,
    tx: Sender<Msg>,
}

impl std::ops::Deref for Session {
    type Target = nori_host::session::Session;

    fn deref(&self) -> &Self::Target {
        &self.host
    }
}

impl Session {
    pub fn open(o: Open) -> Result<Session, String> {
        static IDS: AtomicU64 = AtomicU64::new(0);
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        let tx = o.tx.clone();
        let out = Arc::new(move |s: Said| {
            let _ = tx.send(Msg::From(id, Box::new(worded(s))));
        });
        let v = own::number(own::VOLUME, 1.0);
        let (output, level) = sound(o.device.or_else(|| own::text(own::DEVICE)).as_deref(), v);
        let host = nori_host::session::Session::open(nori_host::session::Open {
            queue: app().clone(),
            data: o.data,
            http: o.http,
            profile: o.profile,
            output,
            volume: level,
            memory_mb: 256,
            covers: o.images,
            offline: o.offline,
            mpris: o.mpris,
            device: nori_core::remote::RemoteMe { name: nori_host::device_name(), kind: nori_core::remote::wire::DeviceKind::Terminal },
            discovery: None,
            out,
        })?;
        Ok(Session { id, host, tx: o.tx })
    }

    /// Loads a screen's data in the background: the stored copy first, then the server's if different.
    pub fn load(&self, req: Req) {
        let (client, core, store, id, tx) = (self.client.clone(), self.core.clone(), self.store.clone(), self.id, self.tx.clone());
        let (offline, db) = (self.offline, self.db.clone());
        let covers_bytes = self.covers.as_ref().and_then(|l| l.disk().map(|d| d.bytes())).unwrap_or(0);
        nori_host::spawn("nori-read", move || {
            let send = |r: Result<Data, String>| {
                let _ = tx.send(Msg::From(id, Box::new(Msg::Data(req.clone(), r))));
            };
            let pages = |read: Read, make: &dyn Fn(Page) -> Option<Data>| {
                let mut any = false;
                let r = read_pages(&client, read, |p| {
                    if let Some(d) = make(p) {
                        any = true;
                        send(Ok(d));
                    }
                });
                match r {
                    Ok(()) if !any => send(Err("The server sent nothing for this page".into())),
                    Ok(()) => {}
                    Err(e) => send(Err(unreachable(&e))),
                }
            };
            match &req {
                Req::Home => {
                    // The favourites are the account's: a jam guest's Home is the host's shelves.
                    let account = core.rules().account;
                    for (i, (title, kind)) in HOME_ROWS.iter().enumerate() {
                        if *kind == AlbumSort::Starred && !account {
                            send(Ok(Data::HomeRow(i, title, Vec::new())));
                            continue;
                        }
                        let read = if *kind == AlbumSort::Starred { Read::FavouriteAlbums { size: 40 } } else { Read::AlbumList { kind: *kind, size: 40, offset: 0, genre: None } };
                        if let Err(e) = read_pages(&client, read, |p| {
                            if let Page::Albums { v } = p {
                                send(Ok(Data::HomeRow(i, title, v)));
                            }
                        }) {
                            return send(Err(unreachable(&e)));
                        }
                    }
                }
                Req::Albums { offset } => {
                    let read = Read::AlbumList { kind: AlbumSort::ByName, size: ALBUM_PAGE as i32, offset: *offset as i32, genre: None };
                    pages(read, &|p| if let Page::Albums { v } = p { Some(Data::Albums(v)) } else { None });
                }
                Req::Artists => pages(Read::ArtistIndex, &|p| if let Page::Artists { v } = p { Some(Data::Artists(v)) } else { None }),
                Req::Playlists => pages(Read::PlaylistList, &|p| if let Page::Playlists { v } = p { Some(Data::Playlists(v)) } else { None }),
                Req::Album(id) => pages(Read::AlbumById { id: id.clone() }, &|p| if let Page::AlbumPage { v } = p { Some(Data::Album(Box::new(v))) } else { None }),
                Req::Artist(id) => pages(Read::ArtistById { id: id.clone() }, &|p| if let Page::ArtistPage { v } = p { Some(Data::Artist(Box::new(v))) } else { None }),
                Req::Playlist(id) => pages(Read::PlaylistById { id: id.clone() }, &|p| if let Page::PlaylistPage { v } = p { Some(Data::Playlist(Box::new(v))) } else { None }),
                Req::Songs { offset } => send(block_on(client.songs_listed("title".into(), false, 0, 0, *offset)).map(|p| Data::Songs(p.songs, p.exhausted)).map_err(|e| unreachable(&e))),
                Req::Downloads => {
                    let stored = core.downloads(true).unwrap_or_default();
                    send(core.download_sections().map(|s| Data::Downloads(Box::new(Downloads { active: s.active, queued: s.queued, failed: s.failed, stored }))).map_err(|e| e.to_string()));
                }
                Req::Facts => send(Ok(Data::Facts(Box::new(facts(&core, &client, &store, &db, covers_bytes, offline))))),
            }
        });
    }

    /// Requests cover `art` at `size` px, deriving colours on the loader thread when `colours`.
    pub fn cover(&self, art: String, size: u32, colours: bool) -> Option<nori_covers::loader::Ticket> {
        self.cover_as(art.clone(), art, size, colours)
    }

    /// Requests an album card cover, delivered under the key `thumb:<art>`.
    pub fn thumb(&self, art: String, size: u32) -> Option<nori_covers::loader::Ticket> {
        self.cover_as(art.clone(), format!("{THUMB}{art}"), size, false)
    }

    fn cover_as(&self, id: String, art: String, size: u32, colours: bool) -> Option<nori_covers::loader::Ticket> {
        let (tx, me) = (self.tx.clone(), self.id);
        self.host.cover(id, size, colours, move |image, colours| {
            let _ = tx.send(Msg::From(me, Box::new(Msg::Cover { art, image, colours })));
        })
    }

    pub fn search_server(&self, query: String) {
        self.host.search_server(query, net_error);
    }

    /// Starts hosting a jam on the server's relay; [`Msg::Jam`] says whether it opened.
    pub fn jam_open(&self) {
        let Some(r) = self.remote() else { return };
        let (tx, me) = (self.tx.clone(), self.id);
        nori_host::spawn("nori-jam", move || {
            let opened = block_on(r.jam_open()).map(drop).map_err(|e| net_error(&e));
            let _ = tx.send(Msg::From(me, Box::new(Msg::Jam(opened))));
        });
    }

    /// Leaves the jam this guest is in; [`Msg::Left`] once the relay was told (or could not be).
    pub fn jam_leave(&self) {
        let (remote, tx) = (self.remote(), self.tx.clone());
        nori_host::spawn("nori-jam-leave", move || {
            if let Some(r) = remote {
                let _ = block_on(r.jam_leave());
            }
            let _ = tx.send(Msg::Left);
        });
    }
}

/// A session's report as the event loop takes it.
fn worded(s: Said) -> Msg {
    let note = |text: String, error: bool| Msg::Note { text, error };
    match s {
        Said::Remote => Msg::Remote,
        Said::Starred(marks) => Msg::Starred(marks),
        Said::Volume(v) => Msg::Volume(v),
        Said::Engine(e) => Msg::Engine(e),
        Said::Lyrics { song, pick } => Msg::Lyrics { song, pick },
        Said::Search(v) => Msg::Search(v),
        Said::Reachable(r) => Msg::Reachable(r.map_err(|e| unreachable(&e))),
        Said::Note(n) => match n {
            Note::Queued { next, songs } => note(format!("{}: {}", if next { "Playing next" } else { "Added to the queue" }, count(songs as u64, "song", "songs")), false),
            Note::NothingToPlay => note("Nothing to play".into(), false),
            Note::SongsFailed(e) => note(format!("Could not load the songs: {}", net_error(&e)), true),
            Note::NothingToPutBack => note("Nothing to put back".into(), false),
            Note::Downloading(n) => note(format!("Downloading {n} songs"), false),
            Note::DownloadFailed(e) => note(format!("Could not download: {e}"), true),
            Note::Starred(on) => note((if on { "Added to favorites" } else { "Removed from favorites" }).into(), false),
            Note::StarFailed(e) => note(format!("Could not change the favorite: {}", net_error(&e)), true),
            Note::Indexing => note("Filling the offline index…".into(), false),
            Note::Indexed(t) => note(format!("Offline index: {} songs, {} albums, {} artists", t.songs, t.albums, t.artists), false),
            Note::IndexStopped(e) => note(format!("The offline index stopped: {}", net_error(&e)), true),
            Note::Forgot(n) => note(format!("Forgot {n} measured songs"), false),
            Note::Done(chore) => note(
                match chore {
                    Chore::ClearStream => "Cleared the streamed music",
                    Chore::ClearLyrics => "Cleared the lyrics found online",
                    _ => "Cleared the covers",
                }
                .into(),
                false,
            ),
        },
    }
}

/// An unreachable server in words.
fn unreachable(e: &NetError) -> String {
    match net_error(e) {
        e if e.is_empty() => "The server did not answer".into(),
        // Already a full sentence.
        e if e.ends_with(['.', '?']) => e,
        e => format!("The server did not answer: {e}"),
    }
}

/// Library, storage and device facts for the settings page.
fn facts(core: &nori_core::Core, client: &nori_core::client::Client, store: &nori_engine::Store, db: &PathBuf, cover_bytes: u64, offline: bool) -> Facts {
    let index = core.index_size().unwrap_or_default();
    let downloads = core.downloads(true).unwrap_or_default();
    let folders = match (!offline).then(|| block_on(client.read_now(Read::MusicFolders))) {
        Some(Ok(Page::Folders { v })) => v,
        _ => Vec::new(),
    };
    Facts {
        analysed: core.analysis_count().unwrap_or(0),
        indexed: (index.songs, index.albums, index.artists),
        storage: Storage {
            stream: store.cache_bytes() as i64,
            covers: cover_bytes as i64,
            lyrics: core.lyrics_cache_bytes(),
            downloads: downloads.iter().map(|s| s.size as i64).sum(),
            download_songs: downloads.len() as u32,
            database: std::fs::metadata(db).map(|m| m.len() as i64).unwrap_or(0),
        },
        folders,
        devices: CpalOutput::devices(),
    }
}
