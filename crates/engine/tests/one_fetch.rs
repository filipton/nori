//! Each song crosses the network once with AutoMix measuring ahead: songs fetched ahead
//! ([`nori_engine::ahead`]) are measured as they arrive, and a song the player takes mid-fetch resumes
//! where the fetch got to. The core is per process, so this is its own binary.
#![cfg(feature = "core")]

mod common;

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_core::client::{Client, NetProfile};
use nori_core::{Core, ServerConfig, Song};
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue, Measurer};
use nori_engine::{Body, ByteSource, Config, Engine, Store};
use parking_lot::Mutex;

use common::NoApi;

fn beat_wav(seed: u32) -> Vec<u8> {
    common::wav(44_100, &common::beat(220.0 + seed as f64))
}

/// Serves songs by the id in the URL and counts bytes sent. The `slow` song comes 256 KB per read and
/// its first body stops at [`HELD_AT`] until the player takes it over from the fetching ahead.
#[derive(Default)]
struct Net {
    files: HashMap<String, Arc<Vec<u8>>>,
    sent: Arc<Mutex<HashMap<String, u64>>>,
    requests: Mutex<Vec<(String, u64)>>,
    slow: Mutex<Option<String>>,
    store: Option<Arc<Store>>,
}

/// Where a slow song's first body waits for the player.
const HELD_AT: u64 = 512 << 10;

fn id_of(url: &str) -> String {
    url.split("&id=").nth(1).unwrap_or("").split('&').next().unwrap_or("").to_string()
}

struct Counted {
    id: String,
    inner: Cursor<Arc<Vec<u8>>>,
    sent: Arc<Mutex<HashMap<String, u64>>>,
    slow: bool,
    /// Stops at [`HELD_AT`] until this store's player takes the song over.
    held: Option<Arc<Store>>,
}

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(store) = self.held.as_ref().filter(|_| self.inner.position() >= HELD_AT) {
            let until = Instant::now() + Duration::from_secs(60);
            while !store.taken_over(&format!("{}:0", self.id)) {
                assert!(Instant::now() < until, "the player never took {} over from the fetching ahead", self.id);
                std::thread::park_timeout(Duration::from_millis(2));
            }
            self.held = None;
        }
        let want = if self.slow { buf.len().min(256 << 10) } else { buf.len() };
        let data = self.inner.get_ref().clone();
        let at = self.inner.position() as usize;
        let n = want.min(data.len() - at);
        buf[..n].copy_from_slice(&data[at..at + n]);
        self.inner.set_position((at + n) as u64);
        *self.sent.lock().entry(self.id.clone()).or_default() += n as u64;
        Ok(n)
    }
}

impl ByteSource for Net {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let id = id_of(url);
        let file = self.files.get(&id).cloned().ok_or("no such song")?;
        self.requests.lock().push((id.clone(), from));
        let len = file.len() as u64;
        let mut inner = Cursor::new(file);
        inner.set_position(from);
        let slow = self.slow.lock().as_deref() == Some(id.as_str());
        let held = self.store.clone().filter(|_| slow && from == 0);
        Ok(Body { start: from, len: Some(len), reader: Box::new(Counted { id, inner, sent: self.sent.clone(), slow, held }) })
    }
}

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    core: Arc<Core>,
    store: Arc<Store>,
    net: Arc<Net>,
    measurer: Arc<Measurer>,
    ids: Vec<String>,
    _dir: nori_testdir::TempDir,
}

impl Rig {
    fn new(name: &str, ids: &[&str]) -> Rig {
        let dir = nori_testdir::TempDir::new(name);
        let core = Core::new(dir.join("nori.db").to_string_lossy().into_owned(), "test".into()).unwrap();
        core.configure(ServerConfig { url: "http://music.test".into(), user: "u".into(), password: "p".into(), api_key: None, legacy_auth: false }).unwrap();
        let client = Client::new(core.clone(), Arc::new(NoApi));
        client.set_profile(NetProfile { url: "http://music.test".into(), ..Default::default() });
        let mut prefs = nori_core::settings_store::settings_open(dir.join("app.db").to_string_lossy().into_owned()).unwrap();
        prefs.auto_mix = true;
        prefs.precache_wifi = 2;
        nori_core::settings_store::settings_put(prefs.clone());
        let store = Store::open(dir.join("music"), 512 << 20, Box::new(CoreOrder)).unwrap();
        let mut net = Net { store: Some(store.clone()), ..Net::default() };
        let songs: Vec<Song> = ids.iter().map(|id| Song { id: id.to_string(), title: id.to_string(), duration: 40, suffix: "wav".into(), ..Default::default() }).collect();
        for (k, id) in ids.iter().enumerate() {
            net.files.insert(id.to_string(), Arc::new(beat_wav(k as u32 * 17)));
        }
        let net = Arc::new(net);
        core.session.register(songs);
        core.session.set(ids.iter().map(|s| s.to_string()).collect(), Some(0), false, None);
        let measurer = Measurer::new(core.clone(), client.clone(), store.clone());
        let library = CoreLibrary { client: client.clone(), bytes: net.clone(), metered: false, store: Some(store.clone()) };
        let app = CoreApp::new(core.session.clone()).measuring(measurer.clone());
        let card = Card::new();
        let clock = Virtual::default();
        let engine = Engine::start_on(library, app, CoreQueue(core.session.clone()), Box::new(card.clone()), None, Config { memory_mb: 256, settings: settings(&prefs, 0.0), ..Config::default() }, clock.clone(), |_| {});
        engine.queue_changed();
        Rig { engine, time: Stepper::new(clock, card.pull.clone()), core, store, net, measurer, ids: ids.iter().map(|s| s.to_string()).collect(), _dir: dir }
    }

    fn until(&self, secs: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        self.time.until(Duration::from_secs(secs), || done(self))
    }

    /// [`Rig::until`] with background fetching and measuring finished before each step: on a device
    /// they finish long before the next song, but the virtual clock would overtake them.
    fn until_settled(&self, secs: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        self.time.until(Duration::from_secs(secs), || {
            self.settle();
            done(self)
        })
    }

    /// Blocks until background fetching and measuring are done.
    fn settle(&self) {
        let until = Instant::now() + Duration::from_secs(120);
        while self.store.fetching_ahead() || nori_engine::core::measuring_as_they_come() || self.measurer.busy() {
            assert!(Instant::now() < until, "the fetching and measuring end");
            std::thread::park_timeout(Duration::from_millis(20));
        }
    }

    fn sent(&self, id: &str) -> u64 {
        self.net.sent.lock().get(id).copied().unwrap_or(0)
    }

    fn len(&self, id: &str) -> u64 {
        self.net.files[id].len() as u64
    }

    fn measured(&self, id: &str) -> bool {
        self.core.analysis_get(id.into()).unwrap().is_some()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

#[test]
fn one_fetch_per_song() {
    every_song_crosses_the_network_once_and_the_songs_fetched_ahead_are_measured_as_they_come();
    a_song_skipped_to_while_it_is_fetched_ahead_goes_on_from_where_the_fetch_got_to();
}

fn every_song_crosses_the_network_once_and_the_songs_fetched_ahead_are_measured_as_they_come() {
    let rig = Rig::new("one-fetch", &["s1", "s2", "s3", "s4", "s5"]);
    rig.engine.play_at(0, 0);
    // Into the third song: each start fetches the next (loader) and the one after (fetching ahead).
    for song in 1..=2 {
        assert!(rig.until_settled(120, |r| r.engine.status().index == Some(song)), "song {} is heard: {:?}", song + 1, rig.engine.status());
    }
    let then = rig.time.clock.now_ns() + 15_000_000_000;
    assert!(rig.until_settled(20, |r| r.time.clock.now_ns() >= then));
    rig.settle();
    // Two ahead on Wi-Fi: the third song's start fetched the fifth.
    for id in &rig.ids {
        assert_eq!(rig.sent(id), rig.len(id), "{id} crossed the network once: {:?}", rig.net.requests.lock());
        assert!(rig.measured(id), "{id} is measured before its turn");
    }
    // s3-s5 were measured as they arrived; only s1 and s2 may have been read back from disk.
    assert!(nori_engine::core::measured_as_they_came() >= 3, "{} measured as they came: {:?}", nori_engine::core::measured_as_they_came(), rig.net.requests.lock());
    assert!(rig.measurer.decoded() <= 2, "no song fetched ahead was decoded again from the disk: {}", rig.measurer.decoded());
    let asked: Vec<String> = rig.net.requests.lock().iter().map(|(id, _)| id.clone()).collect();
    for id in ["s3", "s4", "s5"] {
        assert_eq!(asked.iter().filter(|a| *a == id).count(), 1, "{id} in one request, one burst: {asked:?}");
    }
}

fn a_song_skipped_to_while_it_is_fetched_ahead_goes_on_from_where_the_fetch_got_to() {
    let rig = Rig::new("one-fetch-skip", &["k1", "k2", "k3", "k4"]);
    // k3 is fetched ahead and held part way; the listener skips to it.
    *rig.net.slow.lock() = Some("k3".into());
    rig.engine.play_at(0, 0);
    assert!(rig.until(30, |r| r.engine.status().index == Some(0)), "{:?}", rig.engine.status());
    let until = Instant::now() + Duration::from_secs(60);
    while rig.sent("k3") < HELD_AT {
        assert!(Instant::now() < until, "k3 is being fetched ahead");
        std::thread::park_timeout(Duration::from_millis(5));
    }
    rig.engine.play_at(2, 0);
    assert!(rig.until(120, |r| r.engine.status().index == Some(2) && r.engine.status().position_ms > 5_000), "k3 plays: {:?}", rig.engine.status());
    rig.settle();
    // The rest comes at once, in the same burst.
    let until = Instant::now() + Duration::from_secs(60);
    while rig.store.cached("k3:0").is_none() {
        assert!(Instant::now() < until, "the rest of k3 comes: {:?}", rig.net.requests.lock());
        std::thread::park_timeout(Duration::from_millis(20));
    }
    assert_eq!(rig.sent("k3"), rig.len("k3"), "k3 crossed the network once: {:?}", rig.net.requests.lock());
    let k3: Vec<u64> = rig.net.requests.lock().iter().filter(|(id, _)| id == "k3").map(|(_, from)| *from).collect();
    assert!(k3.len() == 2 && k3[0] == 0 && k3[1] > 0, "the player asked only for the rest: {k3:?}");
    assert!(rig.store.cached("k3:0").is_some(), "and the whole of it is kept");
}
