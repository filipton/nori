//! What the terminal, desktop and iOS clients share around an open session: the queue saved as it
//! changes, a collection's songs, the offline index, cover colours and the output volume in dB. Media
//! controls (`desktop` feature) sit over the engine. Each client words what happens itself.

pub mod remote;
pub mod session;

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nori_core::cache_policy::{Page, Read};
use nori_core::client::{Client, NetProfile};
use nori_core::settings::SavedServer;
use nori_core::transport::{block_on, NetError};
use nori_core::{Core, IngestStats, OriginKind, PageOrigin, ServerConfig, Song};
use nori_covers::memory::Image;
#[cfg(feature = "desktop")]
use nori_engine::State;
use nori_engine::core::OutputVolume;
use nori_engine::{Engine, Status};
use nori_look::cover::CoverColours;

/// The database shared by all profiles and the settings.
pub fn db_path(data: &Path) -> String {
    data.join(nori_core::db::DB_FILE).to_string_lossy().into_owned()
}

pub fn config(p: &SavedServer) -> ServerConfig {
    ServerConfig { url: p.url.clone(), user: p.user.clone(), password: p.password.clone(), api_key: (!p.api_key.is_empty()).then(|| p.api_key.clone()), legacy_auth: p.legacy_auth }
}

pub fn net(p: &SavedServer) -> NetProfile {
    NetProfile { url: p.url.clone(), alt_url: p.alt_url.clone(), music_folder_id: p.music_folder_id.clone(), alt_max_bit_rate: p.alt_max_bit_rate.max(0) as u32 }
}

/// This computer's name, as the account's other devices list it (remote control).
pub fn device_name() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is writable for its whole length; gethostname writes a NUL-terminated name into it.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let name = if ok { String::from_utf8_lossy(buf.split(|b| *b == 0).next().unwrap_or_default()).into_owned() } else { String::new() };
    let name = name.trim_end_matches(".local").to_string();
    if name.is_empty() { "nori".into() } else { name }
}

/// Runs `f` on a named thread.
pub fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(f);
}

/// Volume (0 to 1) in dB, floored at -96.
pub fn volume_db(v: f32) -> f64 {
    if v > 0.0 { 20.0 * (v as f64).log10() } else { -96.0 }
}

/// The client's volume, 0 to 1, and the loudness compensation it is. The account's other devices read
/// and set it too (remote control).
pub struct Level {
    value: AtomicU32,
    /// Sets the sound card's level; None where the system keeps the volume (iOS), which is then followed
    /// but not offered to other devices.
    card: Option<Box<dyn Fn(f32) + Send + Sync>>,
    /// The volume in dB, for loudness compensation.
    pub loudness: Arc<OutputVolume>,
}

impl Level {
    pub fn new(v: f32, card: Option<Box<dyn Fn(f32) + Send + Sync>>) -> Arc<Level> {
        let level = Level { value: AtomicU32::new(0), card, loudness: Arc::default() };
        level.set(v);
        Arc::new(level)
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.value.load(Ordering::Relaxed))
    }

    /// Sets the volume; true when that moved loudness compensation audibly.
    pub fn set(&self, v: f32) -> bool {
        let v = v.clamp(0.0, 1.0);
        self.value.store(v.to_bits(), Ordering::Relaxed);
        if let Some(card) = &self.card {
            card(v);
        }
        self.loudness.set(volume_db(v))
    }

    /// As another device shows and sets it, 0 to 100; None when this client cannot set it.
    pub fn percent(&self) -> Option<u8> {
        self.card.as_ref().map(|_| (self.get() * 100.0).round() as u8)
    }
}

/// Page colours from a cover (dark theme), as Android's `CoverLoader.colours`: RGBA converted to ARGB.
pub fn derive(image: &Image) -> CoverColours {
    let px: Vec<u32> = image.pixels.as_chunks::<4>().0.iter().map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]])).collect();
    nori_look::cover::derive(&px, image.width as usize, image.height as usize, true, false)
}

/// A collection whose songs are fetched on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    Album(String),
    Playlist(String),
    Artist(String),
    /// One of the core's mixes (or the favourites), as its page lists it.
    Mix(String),
}

impl Fetch {
    /// The page these songs are the list of.
    pub fn origin(&self) -> PageOrigin {
        match self {
            Fetch::Album(id) => PageOrigin::new(OriginKind::Album, id.as_str()),
            Fetch::Playlist(id) => PageOrigin::new(OriginKind::Playlist, id.as_str()),
            Fetch::Artist(id) => PageOrigin::new(OriginKind::Artist, id.as_str()),
            Fetch::Mix(id) => PageOrigin::new(OriginKind::Mix, id.as_str()),
        }
    }

    /// The collection's songs, from the server.
    pub fn songs(self, client: &Client) -> Result<Vec<Song>, NetError> {
        let now = |r: Read| block_on(client.read_now(r));
        Ok(match self {
            Fetch::Album(id) => match now(Read::AlbumSongs { id })? {
                Page::Songs { v } => v,
                Page::AlbumPage { v } => v.songs,
                _ => Vec::new(),
            },
            Fetch::Playlist(id) => match now(Read::PlaylistSongs { id })? {
                Page::Songs { v } => v,
                Page::PlaylistPage { v } => v.songs,
                _ => Vec::new(),
            },
            Fetch::Artist(id) => match now(Read::ArtistById { id })? {
                Page::ArtistPage { v } => block_on(client.artist_songs(v.albums)),
                _ => Vec::new(),
            },
            Fetch::Mix(id) => block_on(client.mix_songs(id))?,
        })
    }
}

/// Fills the offline index from the server, page by page; the totals indexed.
pub fn sync(client: &Client) -> Result<IngestStats, NetError> {
    block_on(client.sync_library())
}

/// Saves the queue and playback position.
pub fn save(core: &Core, engine: &Engine) {
    let _ = core.playlist_save(engine.status_with(|s| s.position_now()).max(0) as u64);
}

/// Debounced queue saves (`rules::queue_keep`) on a thread that sleeps until a save is due.
pub struct Keeper {
    /// Next save time, and whether the session closed.
    due: parking_lot::Mutex<(Option<Instant>, bool)>,
    wake: parking_lot::Condvar,
}

impl Keeper {
    pub fn start(core: Arc<Core>, engine: Arc<Engine>) -> Arc<Keeper> {
        let k = Arc::new(Keeper { due: parking_lot::Mutex::new((None, false)), wake: parking_lot::Condvar::new() });
        let me = k.clone();
        spawn("nori-keep", move || {
            let mut due = me.due.lock();
            while !due.1 {
                match due.0 {
                    None => me.wake.wait(&mut due),
                    Some(at) if Instant::now() < at => {
                        me.wake.wait_until(&mut due, at);
                    }
                    Some(_) => {
                        due.0 = None;
                        parking_lot::MutexGuard::unlocked(&mut due, || save(&core, &engine));
                    }
                }
            }
        });
        k
    }

    /// Schedules a save `ms` from now, replacing any pending one.
    pub fn later(&self, ms: i64) {
        let mut due = self.due.lock();
        due.0 = Some(Instant::now() + Duration::from_millis(ms.max(0) as u64));
        self.wake.notify_one();
    }

    pub fn stop(&self) {
        self.due.lock().1 = true;
        self.wake.notify_one();
    }
}

/// The playing song, from the engine's status.
pub type SongOf = Box<dyn Fn(&Status) -> Option<Song> + Send + Sync>;

/// Media controls over the engine.
pub struct Controls {
    pub engine: Arc<Engine>,
    pub song: SongOf,
}

impl Controls {
    /// Controls whose song is `queue`'s.
    pub fn over_queue(engine: Arc<Engine>, queue: Arc<nori_core::queue::Session>) -> Controls {
        Controls { engine, song: Box::new(move |s| s.id.as_deref().and_then(|id| queue.song(id))) }
    }
}

#[cfg(feature = "desktop")]
impl nori_mpris::Controls for Controls {
    fn play(&self) {
        self.engine.play();
    }
    fn pause(&self) {
        self.engine.pause();
    }
    fn toggle(&self) {
        self.engine.toggle();
    }
    fn next(&self) {
        self.engine.next();
    }
    fn previous(&self) {
        self.engine.previous();
    }
    fn seek(&self, ms: i64) {
        self.engine.seek(ms);
    }
    fn now(&self) -> nori_mpris::Now {
        let s = self.engine.status();
        let song = (self.song)(&s).unwrap_or_default();
        nori_mpris::Now {
            playing: s.state == State::Playing,
            loaded: s.state == State::Paused,
            index: s.index,
            title: song.title,
            artist: song.artist,
            album: song.album,
            length_ms: song.duration as i64 * 1000,
            position_ms: s.position_now(),
            starred: song.starred,
            art: None,
        }
    }
}
