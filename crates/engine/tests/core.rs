//! The engine's core pieces: downloads into the store, songs found on disk before the network, and
//! measuring ahead, over a core of the test's own.
#![cfg(feature = "core")]

mod common;

use std::io::Cursor;
use std::sync::Arc;

use nori_engine::core::{Analyses, CoreLibrary, Downloader, Measurer, Shelf, Whole};
use nori_engine::{Body, ByteSource, Library, Source, Store};
use nori_core::client::{Client, NetProfile};
#[cfg(feature = "neural-beats")]
use nori_core::transport::{Exchange, Transport, TransportError, TransportResponse};
use nori_core::{Core, ServerConfig, Song};
use parking_lot::Mutex;

use common::NoApi;

/// Songs made up from their ids, requests counted; each song's first connection breaks half way.
#[derive(Default)]
struct Audio {
    requests: Mutex<Vec<(String, u64)>>,
}

const LEN: usize = 300_000;

fn bytes_of(url: &str) -> Vec<u8> {
    let seed = url.bytes().fold(7u8, |a, b| a.wrapping_mul(31).wrapping_add(b));
    (0..LEN).map(|i| (i as u8).wrapping_add(seed)).collect()
}

/// Reads up to `stop`, then fails.
struct Breaks(Cursor<Vec<u8>>, u64);

impl std::io::Read for Breaks {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.1.saturating_sub(self.0.position()) as usize;
        if left == 0 {
            return Err(std::io::Error::other("reset"));
        }
        let n = buf.len().min(left);
        self.0.read(&mut buf[..n])
    }
}

impl ByteSource for Audio {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let first = !self.requests.lock().iter().any(|(u, _)| u == url);
        self.requests.lock().push((url.to_string(), from));
        let mut c = Cursor::new(bytes_of(url));
        c.set_position(from);
        let reader: Box<dyn std::io::Read + Send> = if first { Box::new(Breaks(c, LEN as u64 / 2)) } else { Box::new(c) };
        Ok(Body { start: from, len: Some(LEN as u64), reader })
    }
}

/// A client's disk: which songs are whole and in which files, and what it was asked about.
#[derive(Default)]
struct Disk {
    whole: Mutex<std::collections::HashMap<String, Vec<std::path::PathBuf>>>,
    asked: Mutex<Vec<String>>,
}

struct OnDisk(Arc<Disk>);

impl Shelf for OnDisk {
    fn whole(&self, id: &str) -> Option<Whole> {
        self.0.asked.lock().push(id.to_string());
        let files = self.0.whole.lock().get(id).cloned()?;
        Some(Whole { files, hint: Some("wav".into()) })
    }
}

fn beat_wav() -> Vec<u8> {
    common::wav(44_100, &common::beat(220.0))
}

#[test]
fn downloads_and_measuring_over_core() {
    let dir = nori_testdir::TempDir::new("core");
    let settings = Arc::new(nori_core::settings_store::Settings::default());
    settings.open(&dir.join("app.db").to_string_lossy()).unwrap();
    let session = Arc::new(nori_core::queue::Session::new(settings.clone()));
    let core = Core::new(dir.join("nori.db").to_string_lossy().into_owned(), "test".into(), session).unwrap();
    let config = ServerConfig { url: "http://music.test".into(), user: "u".into(), password: "p".into(), api_key: None, legacy_auth: false };
    core.configure(config).unwrap();
    let net = Arc::new(NoApi::default());
    let client = Client::new(core.clone(), net.clone(), Default::default());
    client.set_profile(NetProfile { url: "http://music.test".into(), ..Default::default() });
    let store = Store::open(dir.join("music"), 64 << 20).unwrap();
    let audio = Arc::new(Audio::default());
    // The client the analyses work through; the real model's test swaps it for one that serves it.
    let current = Arc::new(Mutex::new(client.clone()));
    let through = current.clone();
    let analyses = Analyses::new(move || Some(through.lock().clone()));

    let song = Song { id: "dl-1".into(), title: "One".into(), duration: 3, suffix: "mp3".into(), ..Default::default() };
    core.download_queue(vec![song.clone()]).unwrap();
    let d = Downloader::new(client.clone(), audio.clone(), store.clone(), analyses.clone());
    d.start(2);
    d.wait();

    let path = store.downloaded("dl-1").expect("downloaded");
    let url = client.resolve("dl-1".into(), true, false).url;
    assert_eq!(std::fs::read(&path).unwrap(), bytes_of(&url), "the whole song, byte for byte");
    let asked: Vec<u64> = audio.requests.lock().iter().map(|r| r.1).collect();
    assert_eq!(asked, [0, LEN as u64 / 2], "taken up where the connection broke");
    assert_eq!(core.transfers().held().state("dl-1"), nori_core::transfers::HeldState::Done, "the core has it as finished");
    assert_eq!(core.transfers().with(|t| t.phase("dl-1")), Some(nori_core::DownloadPhase::Done));

    core.session.register(vec![song]);
    let mut library = CoreLibrary { client: client.clone(), bytes: audio.clone(), store: Some(store.clone()), analyses: analyses.clone() };
    match library.locate("dl-1").unwrap().source {
        Source::File(p) => assert_eq!(p, [path], "the download, not the network"),
        _ => panic!("a downloaded song is read from the disk"),
    }
    match library.locate("other").unwrap().source {
        Source::Cached { key, .. } => assert_eq!(key, "other:0", "streamed, and kept in the cache"),
        _ => panic!("a song that is not downloaded streams"),
    }
    metered_and_ahead(&client, &net, &store, &analyses);
    downloads_read_back(&core, &store, &analyses);

    // AutoMix on: upcoming songs on disk are measured.
    let mut prefs = settings.current().unwrap();
    prefs.auto_mix = true;
    settings.put(prefs);
    let on_disk = Song { id: "m-1".into(), title: "Beat".into(), duration: 40, suffix: "wav".into(), ..Default::default() };
    let elsewhere = Song { id: "m-2".into(), title: "Not here".into(), duration: 40, suffix: "wav".into(), ..Default::default() };
    core.download_queue(vec![on_disk.clone()]).unwrap();
    core.download_settle(vec!["m-1".into()], vec![true]).unwrap();
    std::fs::write(store.download_path("m-1"), beat_wav()).unwrap();
    core.session.register(vec![on_disk, elsewhere]);
    core.session.set(vec!["m-1".into(), "m-2".into(), "ext-3".into()], Some(0), false, None);
    let ahead = core.session.measure();
    assert!(!ahead.contains(&"ext-3".to_string()), "a provider's song is never measured: {ahead:?}");
    let measurer = Measurer::new(analyses.clone(), store.clone());
    measurer.update(ahead, std::thread::current());
    measurer.wait();
    let a = core.analysis_get("m-1".into()).unwrap().expect("m-1 was measured");
    assert!((a.bpm - 120.0).abs() < 2.0 || (a.bpm - 60.0).abs() < 1.0 || (a.bpm - 240.0).abs() < 4.0, "the beat heard: {}", a.bpm);
    assert!(core.analysis_get("m-2".into()).unwrap().is_none(), "not on the disk: left for later");
    assert!(audio.requests.lock().iter().all(|(u, _)| !u.contains("m-2")), "and never fetched for it");
    // Fetched by the player: measured once whole.
    let key = client.resolve("m-2".into(), false, client.metered()).key;
    let beat = beat_wav();
    let mut w = store.writer(&key).unwrap();
    assert!(w.write(0, &beat));
    assert!(w.finish(beat.len() as u64));
    measurer.wait();
    assert!(core.analysis_get("m-2".into()).unwrap().is_some(), "m-2 was measured once it was whole in the cache");
    drop(measurer);

    // From a client's disk in pieces: decoded once when whole; a failure is not retried per look.
    let beat = beat_wav();
    let pieces = dir.join("pieces");
    std::fs::create_dir_all(&pieces).unwrap();
    let put = |name: &str, bytes: &[u8]| {
        let p = pieces.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let whole_3 = vec![put("m-3.0", &beat[..1_000_000]), put("m-3.1", &beat[1_000_000..])];
    let broken = vec![put("m-5.0", &[0u8; 100_000])];
    let disk = Arc::new(Disk::default());
    disk.whole.lock().insert("m-3".into(), whole_3);
    disk.whole.lock().insert("m-5".into(), broken);
    let songs: Vec<Song> = ["m-3", "m-4", "m-5"].iter().map(|id| Song { id: id.to_string(), title: id.to_string(), duration: 40, suffix: "wav".into(), ..Default::default() }).collect();
    core.session.register(songs);
    core.session.set(vec!["m-3".into(), "m-4".into(), "m-5".into()], Some(0), false, None);
    let told = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let heard = told.clone();
    let measurer = Measurer::on_shelf(analyses.clone(), Box::new(OnDisk(disk.clone())), Some(Box::new(move || {
        heard.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    })));
    let settle = |m: &Measurer| m.wait();
    measurer.ask(core.session.measure());
    settle(&measurer);
    assert!(core.analysis_get("m-3".into()).unwrap().is_some(), "a song whole in two pieces is measured");
    assert!(core.analysis_get("m-4".into()).unwrap().is_none(), "one still coming is not");
    assert_eq!(measurer.decoded(), 2, "m-3, and m-5 which could not be");
    let looked = disk.asked.lock().len();
    // Repeated asks with nothing changed run nothing.
    for _ in 0..50 {
        measurer.ask(core.session.measure());
        assert!(!measurer.busy(), "no thread for the same songs");
    }
    assert_eq!(disk.asked.lock().len(), looked, "and nothing is looked at");
    // An arrival triggers a look; the failed song is not decoded again.
    measurer.arrived();
    settle(&measurer);
    assert_eq!(measurer.decoded(), 2, "m-5 is not tried again with the same bytes, m-4 is not whole");
    assert!(!disk.asked.lock()[looked..].contains(&"m-3".to_string()), "a measured song is not even looked for");
    disk.whole.lock().insert("m-4".into(), vec![put("m-4.0", &beat)]);
    measurer.arrived();
    settle(&measurer);
    assert!(core.analysis_get("m-4".into()).unwrap().is_some(), "measured as soon as it is whole");
    assert_eq!(measurer.decoded(), 3, "once");
    assert_eq!(told.load(std::sync::atomic::Ordering::Relaxed), 2, "told of each song stored, to plan again");
    measurer.ask(Vec::new());
    assert!(!measurer.busy(), "AutoMix off: nothing to measure");

    // "Better beat detection" without a model decodes nothing more and says why.
    let mut prefs = settings.current().unwrap();
    (prefs.auto_mix_better_beats, prefs.auto_mix_beats_mobile_data) = (true, true);
    settings.put(prefs);
    measurer.ask(core.session.measure());
    settle(&measurer);
    assert_eq!(measurer.decoded(), 3, "no song decoded again for a model that is not there");
    if nori_core::automix::beats::AVAILABLE {
        assert!(matches!(settings.model.state(), nori_core::automix::beat_model::State::Failed(_)));
    }
    #[cfg(feature = "neural-beats")]
    listens_with_a_real_model(&core, &current, &dir, &measurer, &settle);
    downloads_take_up_rightly(&core, &client, &store, &analyses);
}

/// What a [`Flaky`] request gets: the song breaking at a byte, or a refusal.
#[derive(Clone, Copy)]
enum Answer {
    BreaksAt(u64),
    Refused,
}

/// Songs made up from their URLs, each request answered as planned (the whole song once the plan is
/// through); while shut, requests wait at the gate.
#[derive(Default)]
struct Flaky {
    requests: Mutex<Vec<(String, u64)>>,
    plan: Mutex<std::collections::VecDeque<Answer>>,
    shut: Mutex<bool>,
    opened: parking_lot::Condvar,
    waiting: std::sync::atomic::AtomicU32,
}

impl ByteSource for Flaky {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let mut shut = self.shut.lock();
        self.waiting.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.opened.notify_all();
        while *shut {
            self.opened.wait(&mut shut);
        }
        drop(shut);
        self.requests.lock().push((url.to_string(), from));
        let mut c = Cursor::new(bytes_of(url));
        c.set_position(from);
        match self.plan.lock().pop_front() {
            Some(Answer::Refused) => Err("the network is gone".into()),
            Some(Answer::BreaksAt(at)) => Ok(Body { start: from, len: Some(LEN as u64), reader: Box::new(Breaks(c, at)) }),
            None => Ok(Body { start: from, len: Some(LEN as u64), reader: Box::new(c) }),
        }
    }
}

impl Flaky {
    fn gate(&self, shut: bool) {
        *self.shut.lock() = shut;
        self.opened.notify_all();
    }

    fn answer(&self, plan: &[Answer]) {
        *self.plan.lock() = plan.iter().copied().collect();
        self.requests.lock().clear();
    }
}

fn download_quality(core: &Core, bit_rate: i32, format: &str) {
    let settings = &core.session.settings;
    let mut prefs = settings.current().unwrap();
    prefs.download = nori_core::settings::SavedQuality { bit_rate, format: format.into() };
    settings.put(prefs);
}

/// A download taken up keeps to its quality, a connection that keeps bringing bytes is taken up however
/// often it breaks, and a song being fetched stays "downloading" when the queue is started again.
fn downloads_take_up_rightly(core: &Arc<Core>, client: &Arc<Client>, store: &Arc<Store>, analyses: &Arc<Analyses>) {
    use nori_core::DownloadPhase;
    let download_phase = |id: String| core.transfers().with(|t| t.phase(&id));
    let net = Arc::new(Flaky::default());
    let d = Downloader::new(client.clone(), net.clone(), store.clone(), analyses.clone());
    let queue = |id: &str| core.download_queue(vec![Song { id: id.into(), title: id.into(), duration: 3, suffix: "mp3".into(), ..Default::default() }]).unwrap();
    let url = |id: &str| client.resolve(id.into(), true, false).url;

    // Half of it at 320 kbps MP3, then the network gone.
    download_quality(core, 320, "mp3");
    queue("q-1");
    net.answer(&[Answer::BreaksAt(LEN as u64 / 2), Answer::Refused, Answer::Refused, Answer::Refused, Answer::Refused]);
    d.start(1);
    d.wait();
    assert_eq!(download_phase("q-1".into()), Some(DownloadPhase::Failed), "{:?}", net.requests.lock());
    // Taken up at 128 kbps Opus: from its start, not after the MP3's half.
    download_quality(core, 128, "opus");
    let opus = url("q-1");
    net.answer(&[]);
    d.start(1);
    d.wait();
    assert_eq!(*net.requests.lock(), [(opus.clone(), 0)], "another quality starts over");
    assert!(std::fs::read(store.downloaded("q-1").expect("downloaded")).unwrap() == bytes_of(&opus), "one encoding, whole");

    // Breaking every fifth of the song, and brought whole.
    queue("r-1");
    let fifth = LEN as u64 / 5;
    net.answer(&[Answer::BreaksAt(fifth), Answer::BreaksAt(2 * fifth), Answer::BreaksAt(3 * fifth), Answer::BreaksAt(4 * fifth)]);
    d.start(1);
    d.wait();
    let asked: Vec<u64> = net.requests.lock().iter().map(|r| r.1).collect();
    assert_eq!(asked, [0, fifth, 2 * fifth, 3 * fifth, 4 * fifth], "taken up where each break left it");
    assert!(std::fs::read(store.downloaded("r-1").expect("downloaded")).unwrap() == bytes_of(&url("r-1")));
    // Started again while fetching.
    queue("b-1");
    net.answer(&[]);
    net.gate(true);
    let waiting = net.waiting.load(std::sync::atomic::Ordering::Acquire);
    d.start(1);
    let mut shut = net.shut.lock();
    while net.waiting.load(std::sync::atomic::Ordering::Acquire) == waiting {
        net.opened.wait(&mut shut);
    }
    drop(shut);
    d.start(1);
    assert_eq!(download_phase("b-1".into()), Some(DownloadPhase::Downloading), "not queued again");
    net.gate(false);
    d.wait();
    assert_eq!(download_phase("b-1".into()), Some(DownloadPhase::Done));

    // Three connections in a row that bring nothing fail it.
    queue("n-1");
    net.answer(&[Answer::BreaksAt(0), Answer::BreaksAt(0), Answer::BreaksAt(0)]);
    d.start(1);
    d.wait();
    assert_eq!(download_phase("n-1".into()), Some(DownloadPhase::Failed));
    assert_eq!(net.requests.lock().len(), 3);
}

/// Unmeasured downloads are read back from disk once saved, one at a time; "Analyse downloaded songs"
/// does the same for existing ones.
fn downloads_read_back(core: &Arc<Core>, store: &Arc<Store>, analyses: &Arc<Analyses>) {
    use nori_core::transfers::{Work, COMPLETED};
    let download_phase = |id: String| core.transfers().with(|t| t.phase(&id));
    assert!(!core.session.settings.prefs(|p| p.auto_mix), "AutoMix is off");
    let songs: Vec<Song> = ["rb-1", "rb-2"].iter().map(|id| Song { id: id.to_string(), title: id.to_string(), duration: 40, suffix: "wav".into(), ..Default::default() }).collect();
    core.download_queue(songs).unwrap();
    for id in ["rb-1", "rb-2"] {
        std::fs::write(store.download_path(id), beat_wav()).unwrap();
    }
    core.download_settle(vec!["rb-1".into(), "rb-2".into()], vec![true, true]).unwrap();
    for id in ["rb-1", "rb-2"] {
        core.transfers().with(|t| {
            t.followed(id, COMPLETED, 0);
            t.work_done(id, Work::Lyrics);
        });
    }
    analyses.saved(vec!["rb-1".into(), "rb-2".into()]);
    analyses.wait();
    for id in ["rb-1", "rb-2"] {
        let a = core.analysis_get(id.into()).unwrap().expect("analysed from the disk");
        assert!((a.bpm - 120.0).abs() < 2.0 || (a.bpm - 60.0).abs() < 1.0 || (a.bpm - 240.0).abs() < 4.0, "the beat heard: {}", a.bpm);
        assert_eq!(download_phase(id.into()), Some(nori_core::DownloadPhase::Done), "done with it");
    }
    // Already analysed: nothing to read back.
    assert!(!core.download_unanalysed(false).unwrap().iter().any(|id| id.starts_with("rb-")));
    // Analysis gone: read back again.
    core.measure_again().unwrap();
    let again: Vec<String> = core.download_unanalysed(false).unwrap().into_iter().filter(|id| id.starts_with("rb-")).collect();
    assert_eq!(again.len(), 2);
    assert_eq!(analyses.analyse(again), 2);
    analyses.wait();
    assert!(core.analysis_get("rb-1".into()).unwrap().is_some() && core.analysis_get("rb-2".into()).unwrap().is_some());
    assert_eq!((download_phase("rb-1".into()), download_phase("rb-2".into())), (Some(nori_core::DownloadPhase::Done), Some(nori_core::DownloadPhase::Done)));
    assert!(core.transfers().with(|t| t.processing_at(0)).is_none_or(|p| p.analysing == 0 && p.beats == 0), "nothing left waiting");
}

/// Whole songs, requests counted and signalled.
#[derive(Default)]
struct Plain(Mutex<Vec<String>>, parking_lot::Condvar);

impl ByteSource for Plain {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        self.0.lock().push(url.to_string());
        self.1.notify_all();
        let mut c = Cursor::new(bytes_of(url));
        c.set_position(from);
        Ok(Body { start: from, len: Some(LEN as u64), reader: Box::new(c) })
    }
}

/// Turning metered mid-queue: songs already fetched keep their address, the next streams at the metered
/// quality; fetching ahead follows the network's setting (none on metered by default).
fn metered_and_ahead(client: &Arc<Client>, network: &NoApi, store: &Arc<Store>, analyses: &Arc<Analyses>) {
    use nori_player::pipeline::Songs;
    let songs: Vec<Song> = (1..=5).map(|i| Song { id: format!("p-{i}"), title: format!("P{i}"), duration: 3, suffix: "mp3".into(), ..Default::default() }).collect();
    client.session().register(songs);
    let ids: Vec<String> = (1..=5).map(|i| format!("p-{i}")).collect();
    client.session().set(ids, Some(0), false, None);

    let net = Arc::new(Plain::default());
    let library = CoreLibrary { client: client.clone(), bytes: net.clone(), store: Some(store.clone()), analyses: analyses.clone() };
    let load = nori_player::transport::load_control(256);
    let mut sources = nori_engine::Sources::new(library, load, None, Default::default(), std::thread::current(), Default::default());
    let q = client.streaming_quality(client.metered());
    assert_eq!((q.bit_rate, q.format.as_str()), (0, ""), "the original file on Wi-Fi");
    let _playing = sources.open("p-1", 0).unwrap();
    sources.upcoming("p-2");
    store.wait_ahead();
    assert!(store.peek("p-3:0").is_some(), "the song after the next, fetched whole ahead");
    assert!(store.peek("p-4:0").is_none(), "two ahead on Wi-Fi by default: the next (the engine's own) and this one");
    assert_eq!(net.0.lock().iter().filter(|u| u.ends_with("&id=p-3")).count(), 1, "in one request");

    // A lower metered quality makes the switch visible.
    client.session().settings.edit_by_name("mobile", "192:opus");
    network.metered.store(true, std::sync::atomic::Ordering::Relaxed);
    let q = client.streaming_quality(client.metered());
    assert_eq!((q.bit_rate, q.format.as_str()), (192, "opus"), "the settings' quality for mobile data");
    let asked = net.0.lock().len();
    let _seek = sources.open("p-1", 1_000).unwrap();
    let _next = sources.open("p-2", 0).unwrap();
    assert!(net.0.lock()[asked..].iter().all(|u| !u.contains("format=opus")), "the song playing and the one on its way keep theirs: {:?}", &net.0.lock()[asked..]);
    client.session().moved_to(1);
    sources.upcoming("p-3");
    store.wait_ahead();
    let _after = sources.open("p-4", 0).unwrap();
    let mut asked_for = net.0.lock();
    while !asked_for.iter().any(|u| u.contains("&id=p-4")) {
        net.1.wait(&mut asked_for);
    }
    drop(asked_for);
    let opened: Vec<String> = net.0.lock()[asked..].to_vec();
    assert!(opened.iter().any(|u| u.ends_with("&id=p-4&maxBitRate=192&format=opus")), "the next song fetched streams at the metered quality: {opened:?}");
    assert!(store.peek("p-5:0").is_none() && store.peek("p-5:192opus").is_none(), "one ahead on mobile data by default: the engine's own next song, none more");
    network.metered.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Serves the Beat This! checkpoint at its URL; anything else is 404.
#[cfg(feature = "neural-beats")]
struct Authors(Vec<u8>, Mutex<Vec<String>>);

#[cfg(feature = "neural-beats")]
#[async_trait::async_trait]
impl Transport for Authors {
    async fn get(&self, url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        let found = url == nori_core::automix::beat_model::CHECKPOINT_URL;
        self.1.lock().push(url);
        Ok(if found { TransportResponse { status: 200, body: self.0.clone() } } else { TransportResponse { status: 404, body: b"<html>not found</html>".to_vec() } })
    }

    async fn send(&self, _request: Exchange) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 500, body: Vec::new() })
    }

    fn address_changed(&self) {}

    fn network(&self) -> nori_core::transport::Network {
        nori_core::transport::Network::Unmetered
    }
}

/// The measurer with the real model: fetched, checked, converted and stored, then earlier songs decoded
/// again for their ends. Needs the checkpoint:
/// `NORI_BEAT_THIS_CKPT=<small0.ckpt> cargo test --release -p nori-engine --features neural-beats --test core`.
#[cfg(feature = "neural-beats")]
fn listens_with_a_real_model(core: &Arc<Core>, current: &Mutex<Arc<Client>>, dir: &std::path::Path, measurer: &Arc<Measurer>, settle: &dyn Fn(&Measurer)) {
    use nori_core::automix::beat_model::{self, State};
    use nori_core::automix::beats::GRID_CHECKED;
    let Some(ckpt) = std::env::var("NORI_BEAT_THIS_CKPT").ok().filter(|p| std::path::Path::new(p).is_file()) else {
        eprintln!("no checkpoint in NORI_BEAT_THIS_CKPT: skipped");
        return;
    };
    let authors = Arc::new(Authors(std::fs::read(ckpt).unwrap(), Mutex::new(Vec::new())));
    *current.lock() = Client::new(core.clone(), authors.clone(), Default::default());
    measurer.ask(Vec::new());
    measurer.ask(core.session.measure());
    settle(measurer.as_ref());
    assert_eq!(*authors.1.lock(), [beat_model::CHECKPOINT_URL], "fetched once, from the authors");
    let model = &core.session.settings.model;
    assert_eq!(model.state(), State::Ready);
    let kept = dir.join("models").join(beat_model::FILE_NAME);
    assert_eq!(model.ready(), Some(kept.clone()));
    assert_eq!(std::fs::read_dir(dir.join("models")).unwrap().count(), 1, "the weights file only: no checkpoint left");
    assert!(nori_core::model_download::read(&nori_core::beat_model::BEAT_THIS, &kept).is_ok());
    for id in ["m-3", "m-4"] {
        let a = core.analysis_get(id.into()).unwrap().unwrap();
        assert!(a.intro_grid_source >= GRID_CHECKED && a.outro_grid_source >= GRID_CHECKED, "{id}: both ends read");
    }
    assert!(core.analysis_neural_missing(vec!["m-3".into(), "m-4".into()]).unwrap().is_empty());

    // A changed file is refused.
    let mut bytes = std::fs::read(&kept).unwrap();
    bytes[1000] ^= 1;
    std::fs::write(&kept, &bytes).unwrap();
    assert!(nori_core::model_download::read(&nori_core::beat_model::BEAT_THIS, &kept).is_err());
}

/// A jam guest's queue: the host's songs, set anew only when its song and the next are not in a row.
#[test]
fn a_guest_queues_the_host_songs_once() {
    let dir = nori_testdir::TempDir::new("guest");
    let (core, _client) = common::own_core(&dir, |p| p.auto_fill = true);
    let song = |id: &str| Song { id: id.into(), title: id.into(), ..Default::default() };
    let lead = |ids: &[&str], index: usize| nori_core::remote::Lead {
        songs: ids.iter().map(|id| song(id)).collect(),
        index,
        ms: 0.0,
        at_us: 0,
        there_us: 0,
        rate: 1.0,
        playing: true,
        speed: 1.0,
        pitch: 1.0,
        mix: None,
    };
    let s = &core.session;
    assert_eq!(nori_engine::core::queued_for(s, &lead(&["a", "b", "c"], 0)), (0, true));
    // The host moved on and its window with it: the queue here already has b then c.
    assert_eq!(nori_engine::core::queued_for(s, &lead(&["a", "b", "c", "d"], 1)), (1, false));
    // A song put next there: queued anew.
    assert_eq!(nori_engine::core::queued_for(s, &lead(&["b", "x", "c"], 0)), (0, true));
    assert_eq!(s.playlist(|p| p.ids().to_vec()), ["b", "x", "c"]);
    // Near its end, autofill on here: the host's queue is the host's to fill.
    s.moved_to(1);
    assert!(!s.song_arrived().fill, "nothing fetched for it here");
}
