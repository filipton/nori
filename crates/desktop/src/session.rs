//! The open server profile (nori-host's session), its reports worded as [`Msg`]s for the UI thread, and
//! the pages' reads.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use nori_core::browse::AlbumSort;
use nori_core::cache_policy::{Page, Read};
use nori_core::race::LyricsPick;
use nori_core::search::SearchView;
use nori_core::settings::SavedServer;
use nori_core::mixes::board::{MixLookup, MixSheet, MixTile};
use nori_core::{AlbumDetail, ArtistDetail, PlaylistDetail, Song};
use nori_covers::loader::Ticket;
use nori_covers::memory::Image;
use nori_engine::Event;
use nori_host::config;
use nori_host::session::{read_pages, Chore, Note, Said};
pub use nori_host::Fetch;
use nori_http::Http;
use nori_look::cover::CoverColours;
use nori_engine::core::OutputVolume;
use nori_engine::AudioOutput;
use nori_output_cpal::{CpalOutput, Volume};

use crate::words::{net_error, songs};
use crate::AppWindow;

pub use nori_core::transport::block_on;

/// Cover draw size. Large and hero covers also get their page colours derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoverSize {
    Card,
    Large,
    Hero,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoverKey {
    pub id: String,
    pub size: CoverSize,
}

/// Messages from other threads to the UI thread.
pub enum Msg {
    Engine(Event),
    Data(Req, Result<Data, String>),
    /// A decoded cover, with page colours for large sizes.
    Cover { key: CoverKey, image: Arc<Image>, colours: Option<Box<CoverColours>> },
    Search(SearchView),
    Lyrics { song: String, pick: LyricsPick },
    Facts(Box<crate::settings::Facts>),
    Note { text: String, error: bool },
    LoggedIn(Result<SavedServer, String>),
    Reachable(Result<(), String>),
    /// The other devices changed (remote control).
    Remote,
    /// A message from the session with this id; dropped once another session is open.
    From(u64, Box<Msg>),
}

/// Sends [`Msg`]s to the UI thread: queued in the app's inbox, then the window is woken to drain it.
#[derive(Clone)]
pub struct Tx {
    inbox: mpsc::Sender<Msg>,
    ui: slint::Weak<AppWindow>,
}

impl Tx {
    pub fn new(ui: slint::Weak<AppWindow>) -> (Tx, mpsc::Receiver<Msg>) {
        let (inbox, rx) = mpsc::channel();
        (Tx { inbox, ui }, rx)
    }

    pub fn send(&self, m: Msg) {
        if self.inbox.send(m).is_ok() {
            let _ = self.ui.upgrade_in_event_loop(|ui| ui.invoke_messages_arrived());
        }
    }
}

/// A page read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Req {
    Home,
    Albums,
    Artists,
    Playlists,
    Songs { offset: u32 },
    Album(String),
    Artist(String),
    Playlist(String),
    Mix(String),
}

pub enum Data {
    /// Home's "Top Picks": the favourites tile, then the mixes.
    Picks(Vec<MixTile>),
    HomeRow(usize, Vec<nori_core::Album>),
    Albums(Vec<nori_core::Album>),
    Artists(Vec<nori_core::Artist>),
    Playlists(Vec<nori_core::Playlist>),
    Songs(Vec<Song>, bool),
    Album(Box<AlbumDetail>),
    Artist(Box<ArtistDetail>),
    Playlist(Box<PlaylistDetail>),
    Mix(Box<MixSheet>),
}

/// Home shelves: title and album list kind.
pub const HOME_ROWS: [(&str, AlbumSort); 5] = [
    ("Recently played", AlbumSort::Recent),
    ("Recently added", AlbumSort::Newest),
    ("Most played", AlbumSort::Frequent),
    ("Favorites", AlbumSort::Starred),
    ("Something random", AlbumSort::Random),
];

const ALBUM_PAGE: i32 = 500;

/// The app's queue session over its settings: the client's object graph root, which every view reads.
/// Global: the root of the desktop client's own object graph (as Kotlin's `Nori`), made once.
pub fn app() -> &'static Arc<nori_core::queue::Session> {
    static APP: std::sync::OnceLock<Arc<nori_core::queue::Session>> = std::sync::OnceLock::new();
    APP.get_or_init(|| Arc::new(nori_core::queue::Session::new(nori_core::settings_store::Settings::new())))
}

/// Desktop-only settings, stored as app values in the database.
pub mod own {
    pub const VOLUME: &str = "desktop.volume";
    /// Output device name; empty for the system default.
    pub const DEVICE: &str = "desktop.device";

    pub fn text(key: &str) -> Option<String> {
        crate::session::app().settings.app_value(key).filter(|v| !v.is_empty())
    }

    pub fn number(key: &str, default: f32) -> f32 {
        crate::session::app().settings.app_value(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn keep(key: &'static str, value: String) {
        crate::session::app().settings.keep_app_value(key, value);
    }
}

/// Logs `draft` in against its server (blocking).
pub fn check_login(http: Arc<Http>, draft: SavedServer) -> Result<SavedServer, String> {
    let legacy = block_on(nori_core::client::login_check(http, config(&draft), draft.alt_url.clone())).map_err(|e| net_error(&e))?;
    Ok(SavedServer { legacy_auth: legacy || draft.legacy_auth, ..draft })
}

/// The cpal device at `volume` (0 to 1), and the loudness compensation that volume is.
fn sound(device: Option<&str>, volume: f32) -> (Box<dyn AudioOutput>, Volume, Arc<OutputVolume>) {
    let card = match device {
        Some(name) => CpalOutput::with_device(name),
        None => CpalOutput::new(),
    };
    let level = card.volume();
    level.set(volume);
    let loudness = Arc::new(OutputVolume::default());
    loudness.set(nori_host::volume_db(volume));
    (Box::new(card), level, loudness)
}

/// One open server profile; its messages come tagged with its id.
pub struct Session {
    pub id: u64,
    host: nori_host::session::Session,
    tx: Tx,
    level: Volume,
    loudness: Arc<OutputVolume>,
}

impl std::ops::Deref for Session {
    type Target = nori_host::session::Session;

    fn deref(&self) -> &Self::Target {
        &self.host
    }
}

impl Session {
    /// Opens the profile without touching the network, so an unreachable server still opens.
    /// `mpris`: the process's media controls, driven by this session while it is open.
    pub fn open(data: &Path, http: Arc<Http>, profile: SavedServer, tx: Tx, mpris: Option<Arc<nori_mpris::Mpris>>) -> Result<Session, String> {
        static IDS: AtomicU64 = AtomicU64::new(0);
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        let to = tx.clone();
        let out = Arc::new(move |s: Said| to.send(Msg::From(id, Box::new(worded(s)))));
        let v = own::number(own::VOLUME, 1.0);
        let (output, level, loudness) = sound(own::text(own::DEVICE).as_deref(), v);
        let o = nori_host::session::Open {
            queue: app().clone(),
            data,
            http,
            profile,
            output,
            volume: loudness.clone(),
            memory_mb: 256,
            covers: true,
            offline: false,
            mpris,
            device: nori_core::remote::RemoteMe { name: nori_host::device_name(), kind: nori_core::remote::wire::DeviceKind::Desktop },
            out,
        };
        Ok(Session { id, host: nori_host::session::Session::open(o)?, tx, level, loudness })
    }

    /// Listener volume, 0 to 1.
    pub fn volume(&self) -> f32 {
        self.level.get()
    }

    /// Sets the device volume and, when that changes loudness compensation, the chain.
    pub fn set_volume(&self, v: f32) {
        self.level.set(v);
        if self.loudness.set(nori_host::volume_db(v)) {
            self.host.volume_changed();
        }
    }

    fn sender(&self) -> impl Fn(Msg) + Send + 'static {
        let (tx, id) = (self.tx.clone(), self.id);
        move |m| tx.send(Msg::From(id, Box::new(m)))
    }

    /// Reads a page: the cached answer first, then the server's if it differs.
    pub fn load(&self, req: Req) {
        let (client, core, send) = (self.client.clone(), self.core.clone(), self.sender());
        nori_host::spawn("nori-read", move || {
            let send = |r: Result<Data, String>| send(Msg::Data(req.clone(), r));
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
                    Err(e) => send(Err(net_error(&e))),
                }
            };
            match &req {
                Req::Home => {
                    // The mixes are drawn here from the index and the history; the favourites follow the
                    // starred songs, stored at once and the server's after.
                    let taste = app().settings.prefs(|p| p.taste_model);
                    let stored = client.mix_favourites_stored().ok();
                    if taste {
                        block_on(client.mix_warm_all());
                    }
                    send(Ok(Data::Picks(core.mix_cards(taste))));
                    if let Some(s) = stored.filter(|s| !s.fresh) {
                        if block_on(client.mix_favourites_refresh(s.digest)).is_ok_and(|changed| changed) {
                            send(Ok(Data::Picks(core.mix_cards(taste))));
                        }
                    }
                    for (i, (_, kind)) in HOME_ROWS.iter().enumerate() {
                        let read = if *kind == AlbumSort::Starred { Read::FavouriteAlbums { size: 40 } } else { Read::AlbumList { kind: *kind, size: 40, offset: 0, genre: None } };
                        let mut got = false;
                        if let Err(e) = read_pages(&client, read, |p| {
                            if let Page::Albums { v } = p {
                                got = true;
                                send(Ok(Data::HomeRow(i, v)));
                            }
                        }) {
                            return send(Err(net_error(&e)));
                        }
                        // A row with nothing in it is read too: its shelf goes, rather than waiting on.
                        if !got {
                            send(Ok(Data::HomeRow(i, Vec::new())));
                        }
                    }
                }
                Req::Albums => {
                    let read = Read::AlbumList { kind: AlbumSort::ByName, size: ALBUM_PAGE, offset: 0, genre: None };
                    pages(read, &|p| if let Page::Albums { v } = p { Some(Data::Albums(v)) } else { None });
                }
                Req::Artists => pages(Read::ArtistIndex, &|p| if let Page::Artists { v } = p { Some(Data::Artists(v)) } else { None }),
                Req::Playlists => pages(Read::PlaylistList, &|p| if let Page::Playlists { v } = p { Some(Data::Playlists(v)) } else { None }),
                Req::Album(id) => pages(Read::AlbumById { id: id.clone() }, &|p| if let Page::AlbumPage { v } = p { Some(Data::Album(Box::new(v))) } else { None }),
                Req::Artist(id) => pages(Read::ArtistById { id: id.clone() }, &|p| if let Page::ArtistPage { v } = p { Some(Data::Artist(Box::new(v))) } else { None }),
                Req::Playlist(id) => pages(Read::PlaylistById { id: id.clone() }, &|p| if let Page::PlaylistPage { v } = p { Some(Data::Playlist(Box::new(v))) } else { None }),
                Req::Mix(id) => match block_on(client.mix_songs(id.clone())) {
                    Ok(_) => match core.mix_page(id.clone()) {
                        MixLookup::Ready { sheet } => send(Ok(Data::Mix(Box::new(sheet)))),
                        MixLookup::NotDrawn | MixLookup::Unknown => send(Err("This mix has nothing in it yet".into())),
                    },
                    Err(e) => send(Err(net_error(&e))),
                },
                Req::Songs { offset } => send(core.songs_page("title".into(), false, 0, 0, *offset).map(|p| Data::Songs(p.songs, p.exhausted)).map_err(|e| e.to_string())),
            }
        });
    }

    /// Requests a cover at `px` square; large ones also get page colours, derived on the loader thread.
    pub fn cover(&self, key: CoverKey, px: u32) -> Option<Ticket> {
        let (send, colours) = (self.sender(), key.size != CoverSize::Card);
        self.host.cover(key.id.clone(), px, colours, move |image, colours| send(Msg::Cover { key, image, colours }))
    }

    /// Remote control, while it is on: the other devices and moving the music between them.
    pub fn remote(&self) -> Option<Arc<nori_core::remote::Remote>> {
        self.host.remote()
    }

    /// Searches the server (after a typing pause).
    pub fn search_server(&self, query: String) {
        self.host.search_server(query, net_error);
    }

    /// Gathers [`Facts`](crate::settings::Facts) on a worker (asks the server for music folders).
    pub fn facts(&self) {
        let (core, client, store, db, send) = (self.core.clone(), self.client.clone(), self.store.clone(), self.db.clone(), self.sender());
        let cover_bytes = self.covers.as_ref().and_then(|l| l.disk()).map_or(0, |d| d.bytes());
        nori_host::spawn("nori-facts", move || {
            let index = core.index_size().unwrap_or_default();
            let downloads = core.downloads(true).unwrap_or_default();
            let folders = match block_on(client.read_now(Read::MusicFolders)) {
                Ok(Page::Folders { v }) => v.into_iter().map(|f| (f.name, f.id)).collect(),
                _ => Vec::new(),
            };
            send(Msg::Facts(Box::new(crate::settings::Facts {
                analysed: core.analysis_count().unwrap_or(0),
                indexed: (index.songs, index.albums, index.artists),
                stream_bytes: store.cache_bytes(),
                cover_bytes,
                lyrics_bytes: core.lyrics_cache_bytes().max(0) as u64,
                download_bytes: downloads.iter().map(|s| s.size).sum(),
                download_songs: downloads.len() as u32,
                database_bytes: std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0),
                folders,
                devices: CpalOutput::devices(),
                device: own::text(own::DEVICE).unwrap_or_default(),
                syncing: false,
            })));
        });
    }
}

/// A session's report as the UI thread takes it.
fn worded(s: Said) -> Msg {
    let note = |text: String, error: bool| Msg::Note { text, error };
    match s {
        Said::Engine(e) => Msg::Engine(e),
        Said::Lyrics { song, pick } => Msg::Lyrics { song, pick },
        Said::Search(v) => Msg::Search(v),
        Said::Reachable(r) => Msg::Reachable(r.map_err(|e| net_error(&e))),
        Said::Remote => Msg::Remote,
        Said::Note(n) => match n {
            Note::Queued { next, songs: n } => note(format!("{}: {}", if next { "Playing next" } else { "Added to the queue" }, songs(n)), false),
            Note::NothingToPlay => note("Nothing to play".into(), false),
            Note::SongsFailed(e) => note(format!("Could not load the songs: {}", net_error(&e)), true),
            Note::NothingToPutBack => note("Nothing to put back".into(), false),
            Note::Downloading(n) => note(format!("Downloading {n} songs"), false),
            Note::DownloadFailed(e) => note(format!("Could not download the library: {e}"), true),
            Note::Starred(on) => note((if on { "Added to favorites" } else { "Removed from favorites" }).into(), false),
            Note::StarFailed(e) => note(format!("Could not change the favorite: {}", net_error(&e)), true),
            Note::Indexing => note("Filling the offline index…".into(), false),
            Note::Indexed(t) => note(format!("Offline index: {} songs", t.songs), false),
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
