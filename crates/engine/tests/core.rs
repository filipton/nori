//! The engine's pieces over the core: downloads run from the core's queue into the store, a downloaded
//! or cached song is found on the disk before the network is asked, and with AutoMix on the songs
//! coming up that are on the disk are measured ahead. The core keeps one active database and one queue
//! per process, so it is all one test.
#![cfg(feature = "core")]

use std::io::Cursor;
use std::sync::Arc;

use nori_engine::core::{CoreLibrary, CoreOrder, Downloader, Measurer, Shelf, Whole};
use nori_engine::{Body, ByteSource, Library, Source, Store};
use nori_core::client::{Client, NetProfile};
use nori_core::transport::{Exchange, Transport, TransportError, TransportResponse};
use nori_core::{Core, ServerConfig, Song};
use parking_lot::Mutex;

/// No API calls are made here; resolving a song's address needs none.
struct NoApi;

#[async_trait::async_trait]
impl Transport for NoApi {
    async fn get(&self, _url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 500, body: Vec::new() })
    }

    async fn send(&self, _request: Exchange) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 500, body: Vec::new() })
    }

    fn address_changed(&self) {}
}

/// Audio: each song's bytes made up from its id, every request counted, and a connection that breaks
/// half way through the first time it is asked for a song.
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

/// A client's disk: which songs are whole there and in which files, and every song it was asked about.
#[derive(Default)]
struct Disk {
    whole: Mutex<std::collections::HashMap<String, Vec<std::path::PathBuf>>>,
    asked: Mutex<Vec<String>>,
}

/// The measurer's view of it.
struct OnDisk(Arc<Disk>);

impl Shelf for OnDisk {
    fn whole(&self, id: &str) -> Option<Whole> {
        self.0.asked.lock().push(id.to_string());
        let files = self.0.whole.lock().get(id).cloned()?;
        Some(Whole { files, hint: Some("wav".into()) })
    }
}

/// Forty seconds of a steady beat at 120 bpm as a 16-bit stereo WAV file.
fn beat_wav() -> Vec<u8> {
    let rate = 44_100u32;
    let frames = rate as usize * 40;
    let mut samples = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let in_beat = i % (rate as usize / 2);
        let click = if in_beat < 2000 { (1.0 - in_beat as f64 / 2000.0) * 0.8 } else { 0.0 };
        let tone = (i as f64 * 220.0 * std::f64::consts::TAU / rate as f64).sin() * 0.1;
        let v = (((click * ((i * 7919) % 97) as f64 / 97.0) + tone) * 32767.0) as i16;
        samples.extend([v, v]);
    }
    let data = samples.len() as u32 * 2;
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.extend(samples.iter().flat_map(|v| v.to_le_bytes()));
    w
}

#[test]
fn downloads_disk_and_measuring_over_core() {
    let dir = nori_testdir::TempDir::new("core");
    let core = Core::new(dir.join("nori.db").to_string_lossy().into_owned(), "test".into()).unwrap();
    let config = ServerConfig { url: "http://music.test".into(), user: "u".into(), password: "p".into(), api_key: None, legacy_auth: false };
    core.configure(config).unwrap();
    let client = Client::new(core.clone(), Arc::new(NoApi));
    client.set_profile(NetProfile { url: "http://music.test".into(), ..Default::default() });
    let store = Store::open(dir.join("music"), 64 << 20, Box::new(CoreOrder)).unwrap();
    let audio = Arc::new(Audio::default());

    let song = Song { id: "dl-1".into(), title: "One".into(), duration: 3, suffix: "mp3".into(), ..Default::default() };
    core.download_queue(vec![song.clone()]).unwrap();
    let d = Downloader::new(core.clone(), client.clone(), audio.clone(), store.clone());
    d.start(2);
    d.wait();

    let path = store.downloaded("dl-1").expect("downloaded");
    let url = client.resolve("dl-1".into(), true, false).url;
    assert_eq!(std::fs::read(&path).unwrap(), bytes_of(&url), "the whole song, byte for byte");
    let asked: Vec<u64> = audio.requests.lock().iter().map(|r| r.1).collect();
    assert_eq!(asked, [0, LEN as u64 / 2], "taken up where the connection broke");
    assert_eq!(nori_core::transfers::held("dl-1"), nori_core::transfers::HeldState::Done, "the core has it as finished");
    assert_eq!(nori_core::transfers::download_phase("dl-1".into()), Some(nori_core::DownloadPhase::Done));

    nori_core::queue::queue_register(vec![song]);
    let mut library = CoreLibrary { client: client.clone(), bytes: audio.clone(), metered: false, store: Some(store.clone()) };
    match library.locate("dl-1").unwrap().source {
        Source::File(p) => assert_eq!(p, path, "the download, not the network"),
        _ => panic!("a downloaded song is read from the disk"),
    }
    match library.locate("other").unwrap().source {
        Source::Cached { key, .. } => assert_eq!(key, "other:0", "streamed, and kept in the cache"),
        _ => panic!("a song that is not downloaded streams"),
    }
    metered_and_ahead(&client, &store, &dir);
    downloads_read_back(&core, &store);

    // AutoMix on: the songs coming up are measured, those on the disk only.
    let mut prefs = nori_core::settings_store::settings_open(dir.join("app.db").to_string_lossy().into_owned()).unwrap();
    prefs.auto_mix = true;
    nori_core::settings_store::settings_put(prefs);
    let on_disk = Song { id: "m-1".into(), title: "Beat".into(), duration: 40, suffix: "wav".into(), ..Default::default() };
    let elsewhere = Song { id: "m-2".into(), title: "Not here".into(), duration: 40, suffix: "wav".into(), ..Default::default() };
    core.download_queue(vec![on_disk.clone()]).unwrap();
    core.download_settle(vec!["m-1".into()], vec![true]).unwrap();
    std::fs::write(store.download_path("m-1"), beat_wav()).unwrap();
    nori_core::queue::queue_register(vec![on_disk, elsewhere]);
    nori_core::playlist::playlist_set(vec!["m-1".into(), "m-2".into(), "ext-3".into()], Some(0), false, None);
    let ahead = nori_core::rules::queue_measure();
    assert!(!ahead.contains(&"ext-3".to_string()), "a provider's song is never measured: {ahead:?}");
    let measurer = Measurer::new(core.clone(), client.clone(), store.clone());
    measurer.update(ahead, std::thread::current());
    let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while core.analysis_get("m-1".into()).unwrap().is_none() {
        assert!(std::time::Instant::now() < until, "m-1 was measured");
        std::thread::park_timeout(std::time::Duration::from_millis(200));
    }
    let a = core.analysis_get("m-1".into()).unwrap().unwrap();
    assert!((a.bpm - 120.0).abs() < 2.0 || (a.bpm - 60.0).abs() < 1.0 || (a.bpm - 240.0).abs() < 4.0, "the beat heard: {}", a.bpm);
    assert!(core.analysis_get("m-2".into()).unwrap().is_none(), "not on the disk: left for later");
    assert!(audio.requests.lock().iter().all(|(u, _)| !u.contains("m-2")), "and never fetched for it");
    // The player fetches it (the next song, or one a queue edit put next): measured as it becomes whole.
    let key = client.resolve("m-2".into(), false, nori_core::stream::metered()).key;
    let beat = beat_wav();
    let mut w = store.writer(&key).unwrap();
    assert!(w.write(0, &beat));
    assert!(w.finish(beat.len() as u64));
    let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while core.analysis_get("m-2".into()).unwrap().is_none() {
        assert!(std::time::Instant::now() < until, "m-2 was measured once it was whole in the cache");
        std::thread::park_timeout(std::time::Duration::from_millis(200));
    }
    drop(measurer);

    // From a client's own disk (Android's media3 cache, a song in pieces): each song decoded once, only
    // once it is whole, and one that cannot be measured is not tried again every time it is looked at.
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
    nori_core::queue::queue_register(songs);
    nori_core::playlist::playlist_set(vec!["m-3".into(), "m-4".into(), "m-5".into()], Some(0), false, None);
    let told = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let heard = told.clone();
    let active = core.clone();
    let measurer = Measurer::on_shelf(move || Some(active.clone()), Box::new(OnDisk(disk.clone())), Some(Box::new(move || {
        heard.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    })));
    let settle = |m: &Measurer| {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while m.busy() {
            assert!(std::time::Instant::now() < until, "the measuring thread ends");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    measurer.ask(nori_core::rules::queue_measure());
    settle(&measurer);
    assert!(core.analysis_get("m-3".into()).unwrap().is_some(), "a song whole in two pieces is measured");
    assert!(core.analysis_get("m-4".into()).unwrap().is_none(), "one still coming is not");
    assert_eq!(measurer.decoded(), 2, "m-3, and m-5 which could not be");
    let looked = disk.asked.lock().len();
    // Every loading burst, precache and queue event asks again: with nothing changed, nothing runs.
    for _ in 0..50 {
        measurer.ask(nori_core::rules::queue_measure());
        assert!(!measurer.busy(), "no thread for the same songs");
    }
    assert_eq!(disk.asked.lock().len(), looked, "and nothing is looked at");
    // Another song arrives on the disk: a look, in which the song that failed is not decoded again.
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

    // "Better beat detection" on: a build without the model's runtime, or one whose model cannot be fetched
    // (this transport answers nothing), decodes nothing more for it, and says why on the settings page.
    let mut prefs = nori_core::settings_store::settings_current().unwrap();
    (prefs.auto_mix_better_beats, prefs.auto_mix_beats_mobile_data) = (true, true);
    nori_core::settings_store::settings_put(prefs);
    measurer.ask(nori_core::rules::queue_measure());
    settle(&measurer);
    assert_eq!(measurer.decoded(), 3, "no song decoded again for a model that is not there");
    if nori_core::automix::beats::AVAILABLE {
        assert!(matches!(nori_core::automix::beat_model::state(), nori_core::automix::beat_model::State::Failed(_)));
    }
    #[cfg(feature = "neural-beats")]
    listens_with_a_real_model(&core, &dir, &measurer, &settle);
}

/// A download that was not measured as it came (an MP4, one taken up half way, an analysis of an older version)
/// is read back from the disk once saved, whatever AutoMix says, one song at a time, and nothing is left running
/// after; the settings' "Analyse downloaded songs" does the same for the downloads already there.
fn downloads_read_back(core: &Arc<Core>, store: &Arc<Store>) {
    use nori_core::transfers::{download_phase, followed, work_done, Work, COMPLETED};
    assert!(!nori_core::settings_store::with_prefs(|p| p.auto_mix).unwrap_or(false), "AutoMix is off");
    let songs: Vec<Song> = ["rb-1", "rb-2"].iter().map(|id| Song { id: id.to_string(), title: id.to_string(), duration: 40, suffix: "wav".into(), ..Default::default() }).collect();
    core.download_queue(songs).unwrap();
    for id in ["rb-1", "rb-2"] {
        std::fs::write(store.download_path(id), beat_wav()).unwrap();
    }
    core.download_settle(vec!["rb-1".into(), "rb-2".into()], vec![true, true]).unwrap();
    for id in ["rb-1", "rb-2"] {
        followed(id, COMPLETED, 0);
        work_done(id, Work::Lyrics);
    }
    nori_engine::processing::saved(vec!["rb-1".into(), "rb-2".into()]);
    nori_engine::processing::wait();
    for id in ["rb-1", "rb-2"] {
        let a = core.analysis_get(id.into()).unwrap().expect("analysed from the disk");
        assert!((a.bpm - 120.0).abs() < 2.0 || (a.bpm - 60.0).abs() < 1.0 || (a.bpm - 240.0).abs() < 4.0, "the beat heard: {}", a.bpm);
        assert_eq!(download_phase(id.into()), Some(nori_core::DownloadPhase::Done), "done with it");
    }
    // Analysed already: nothing to read back.
    assert!(!core.download_unanalysed(false).unwrap().iter().any(|id| id.starts_with("rb-")));
    // Its analysis gone (an older version): "Analyse downloaded songs" reads it back again.
    core.analysis_clear().unwrap();
    let again: Vec<String> = core.download_unanalysed(false).unwrap().into_iter().filter(|id| id.starts_with("rb-")).collect();
    assert_eq!(again.len(), 2);
    assert_eq!(nori_engine::processing::analyse(again), 2);
    nori_engine::processing::wait();
    assert!(core.analysis_get("rb-1".into()).unwrap().is_some() && core.analysis_get("rb-2".into()).unwrap().is_some());
    assert_eq!((download_phase("rb-1".into()), download_phase("rb-2".into())), (Some(nori_core::DownloadPhase::Done), Some(nori_core::DownloadPhase::Done)));
    assert!(nori_core::transfers::processing(0).is_none_or(|p| p.analysing == 0 && p.beats == 0), "nothing left waiting");
}

/// Whole songs, every request counted: the precacher's network.
#[derive(Default)]
struct Plain(Mutex<Vec<String>>);

impl ByteSource for Plain {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        self.0.lock().push(url.to_string());
        let mut c = Cursor::new(bytes_of(url));
        c.set_position(from);
        Ok(Body { start: from, len: Some(LEN as u64), reader: Box::new(c) })
    }
}

/// The network turning metered in the middle of a queue: the song playing and the one already on its way
/// keep their addresses, the next song fetched streams at the metered quality. And the songs after the
/// next are fetched whole ahead of their turn, as many as the settings give the network, none on a metered
/// one by default.
fn metered_and_ahead(client: &Arc<Client>, store: &Arc<Store>, dir: &std::path::Path) {
    use nori_player::pipeline::Songs;
    let songs: Vec<Song> = (1..=5).map(|i| Song { id: format!("p-{i}"), title: format!("P{i}"), duration: 3, suffix: "mp3".into(), ..Default::default() }).collect();
    nori_core::queue::queue_register(songs);
    let ids: Vec<String> = (1..=5).map(|i| format!("p-{i}")).collect();
    nori_core::playlist::playlist_set(ids, Some(0), false, None);
    let _ = nori_core::settings_store::settings_open(dir.join("app.db").to_string_lossy().into_owned()).unwrap();

    let net = Arc::new(Plain::default());
    let library = CoreLibrary { client: client.clone(), bytes: net.clone(), metered: false, store: Some(store.clone()) };
    let load: [i64; 5] = nori_core::rules::load_control(256).try_into().unwrap();
    let mut sources = nori_engine::Sources::new(library, load, Default::default(), std::thread::current());
    let q = nori_engine::core::network_metered(client, false);
    assert_eq!((q.bit_rate, q.format.as_str()), (0, ""), "the original file on Wi-Fi");
    let _playing = sources.open("p-1", 0).unwrap();
    sources.upcoming("p-2");
    let settle = || {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while store.fetching_ahead() {
            assert!(std::time::Instant::now() < until, "the fetching ahead ends");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    settle();
    assert!(store.peek("p-3:0").is_some(), "the song after the next, fetched whole ahead");
    assert!(store.peek("p-4:0").is_none(), "two ahead on Wi-Fi by default: the next (the engine's own) and this one");
    assert_eq!(net.0.lock().iter().filter(|u| u.ends_with("&id=p-3")).count(), 1, "in one request");

    // Mobile data plays the original file too out of the box; a lower quality there shows the switch.
    nori_core::settings_store::edit_by_name("mobile", "192:opus");
    let q = nori_engine::core::network_metered(client, true);
    assert_eq!((q.bit_rate, q.format.as_str()), (192, "opus"), "the settings' quality for mobile data");
    let asked = net.0.lock().len();
    let _seek = sources.open("p-1", 1_000).unwrap();
    let _next = sources.open("p-2", 0).unwrap();
    assert!(net.0.lock()[asked..].iter().all(|u| !u.contains("format=opus")), "the song playing and the one on its way keep theirs: {:?}", &net.0.lock()[asked..]);
    nori_core::playlist::playlist_moved_to(1);
    sources.upcoming("p-3");
    settle();
    let _after = sources.open("p-4", 0).unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !net.0.lock().iter().any(|u| u.contains("&id=p-4")) {
        assert!(std::time::Instant::now() < until, "p-4 is asked for");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let opened: Vec<String> = net.0.lock()[asked..].to_vec();
    assert!(opened.iter().any(|u| u.ends_with("&id=p-4&maxBitRate=192&format=opus&estimateContentLength=true")), "the next song fetched streams at the metered quality: {opened:?}");
    assert!(store.peek("p-5:0").is_none() && store.peek("p-5:192opus").is_none(), "one ahead on mobile data by default: the engine's own next song, none more");
    nori_engine::core::network_metered(client, false);
}

/// The authors' checkpoint, served where they publish it; anything else is not found.
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
}

/// The measurer with the real model, made as every client makes it: the checkpoint fetched from the authors'
/// address, checked, converted and kept where the app keeps it; then the songs measured before it are decoded once
/// more, and each end is read and marked. Needs the checkpoint (no network here):
/// `NORI_BEAT_THIS_CKPT=<small0.ckpt> cargo test --release -p nori-engine --features neural-beats --test core`;
/// without it there is nothing to run.
#[cfg(feature = "neural-beats")]
fn listens_with_a_real_model(core: &Arc<Core>, dir: &std::path::Path, measurer: &Arc<Measurer>, settle: &dyn Fn(&Measurer)) {
    use nori_core::automix::beat_model::{self, State};
    use nori_core::automix::beats::GRID_CHECKED;
    let Some(ckpt) = std::env::var("NORI_BEAT_THIS_CKPT").ok().filter(|p| std::path::Path::new(p).is_file()) else {
        eprintln!("no checkpoint in NORI_BEAT_THIS_CKPT: skipped");
        return;
    };
    let authors = Arc::new(Authors(std::fs::read(ckpt).unwrap(), Mutex::new(Vec::new())));
    let _client = Client::new(core.clone(), authors.clone());
    measurer.ask(Vec::new());
    measurer.ask(nori_core::rules::queue_measure());
    settle(measurer.as_ref());
    assert_eq!(*authors.1.lock(), [beat_model::CHECKPOINT_URL], "fetched once, from the authors");
    assert_eq!(beat_model::state(), State::Ready);
    let kept = dir.join("models").join(beat_model::FILE_NAME);
    assert_eq!(beat_model::ready(), Some(kept.clone()));
    assert_eq!(std::fs::read_dir(dir.join("models")).unwrap().count(), 1, "the weights file only: no checkpoint left");
    assert!(nori_core::beat_download::read(&kept).is_ok());
    for id in ["m-3", "m-4"] {
        let a = core.analysis_get(id.into()).unwrap().unwrap();
        assert!(a.intro_grid_source >= GRID_CHECKED && a.outro_grid_source >= GRID_CHECKED, "{id}: both ends read");
    }
    assert!(core.analysis_neural_missing(vec!["m-3".into(), "m-4".into()]).unwrap().is_empty());

    // A file changed on the disk is refused, not misread.
    let mut bytes = std::fs::read(&kept).unwrap();
    bytes[1000] ^= 1;
    std::fs::write(&kept, &bytes).unwrap();
    assert!(nori_core::beat_download::read(&kept).is_err());
}
