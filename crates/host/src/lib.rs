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
use nori_core::remote::JamPass;
use nori_core::settings::SavedServer;
use nori_core::settings_store::Settings;
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

/// The profile open before a jam was joined, opened again on leaving it (an app value).
const BEFORE_JAM: &str = "host.beforeJam";

/// Why a jam was not joined.
#[derive(Debug)]
pub enum JoinError {
    /// The invite is to the jam this device hosts.
    Own,
    Failed(NetError),
}

/// Joins the jam `link` invites to (blocking), named as the active profile's user, else `device`; never
/// the one `remote` (this device's) hosts.
pub fn jam_join(transport: Arc<dyn nori_core::transport::Transport>, settings: &Settings, link: String, device: &str, remote: Option<&nori_core::remote::Remote>) -> Result<JamPass, JoinError> {
    if remote.is_some_and(|r| r.hosts_invite(link.clone())) {
        return Err(JoinError::Own);
    }
    let user = settings.prefs(|p| p.servers.iter().find(|s| s.id == p.active_server_id).map(|s| s.user.clone()).filter(|u| !u.is_empty()));
    block_on(nori_core::remote::jam_join(transport, link, user.unwrap_or_else(|| device.into()))).map_err(JoinError::Failed)
}

/// The guest profile `pass` signs in with, called `name`, made the active one in place of any guest
/// profile before it. The account's profile active until now is opened again by [`jam_left`].
pub fn jam_joined(settings: &Settings, pass: JamPass, name: &str) -> SavedServer {
    let mut prefs = settings.current().unwrap_or_default();
    let active = prefs.servers.iter().find(|s| s.id == prefs.active_server_id);
    if active.is_some_and(|s| !nori_remote::is_guest_key(&s.api_key)) {
        settings.keep_app_value(BEFORE_JAM, prefs.active_server_id.clone());
    }
    let guest = SavedServer { id: nori_core::settings::new_server_id(), name: name.into(), url: pass.url, api_key: pass.api_key, ..Default::default() };
    prefs.servers.retain(|s| !nori_remote::is_guest_key(&s.api_key));
    prefs.servers.push(guest.clone());
    prefs.active_server_id = guest.id.clone();
    settings.put(prefs);
    guest
}

/// Drops the jam's guest profile; the profile to open now: the one open before the jam, else the first
/// saved, else none (the login).
pub fn jam_left(settings: &Settings) -> Option<SavedServer> {
    let mut prefs = settings.current().unwrap_or_default();
    prefs.servers.retain(|s| !nori_remote::is_guest_key(&s.api_key));
    let before = settings.app_value(BEFORE_JAM).and_then(|id| prefs.servers.iter().find(|s| s.id == id));
    let back = before.or(prefs.servers.first()).cloned();
    prefs.active_server_id = back.as_ref().map(|s| s.id.clone()).unwrap_or_default();
    settings.put(prefs);
    back
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
    /// Sets the sound card's level; None where this client cannot set the volume, which is then followed
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A guest profile takes the active one's place while in a jam; leaving drops it and opens the
    /// account's again, also after a second jam joined from the first.
    #[test]
    fn a_jam_guest_profile_comes_and_goes() {
        let dir = nori_testdir::TempDir::new("host-jam");
        let settings = Settings::new();
        let mut prefs = settings.open(&db_path(dir.path())).unwrap();
        let own = |id: &str| SavedServer { id: id.into(), url: format!("http://{id}"), user: "ann".into(), password: "pw".into(), ..Default::default() };
        prefs.servers = vec![own("home"), own("work")];
        prefs.active_server_id = "work".into();
        settings.put(prefs);
        let pass = |n: u8| JamPass { url: "http://friend".into(), api_key: nori_remote::guest_key(&format!("key{n}")) };

        let first = jam_joined(&settings, pass(1), "Jam");
        nori_core::background::flush();
        let second = jam_joined(&settings, pass(2), "Jam");
        nori_core::background::flush();
        let p = settings.current().unwrap();
        let ids: Vec<&str> = p.servers.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["home", "work", second.id.as_str()], "one guest profile, the latest");
        assert_ne!(first.id, second.id);
        assert_eq!((p.active_server_id.as_str(), second.name.as_str()), (second.id.as_str(), "Jam"));

        assert_eq!(jam_left(&settings).map(|s| s.id), Some("work".into()), "the profile open before the jams");
        let p = settings.current().unwrap();
        assert_eq!((p.servers.len(), p.active_server_id.as_str()), (2, "work"));
    }
}
