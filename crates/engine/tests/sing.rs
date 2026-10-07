//! Sing over a core: a vocal mask that comes while its song plays turns the vocals down from there, with
//! the measurer driven apart from the engine, as on Android.
use crate::common;

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_core::Song;
use nori_engine::core::{settings, Analyses, CoreApp, CoreLibrary, CoreQueue, Measurer, Shelf, Whole};
use nori_engine::{Body, ByteSource, Config, Engine, Store};
use nori_player::sing::{bands, VocalMask, MODEL_HOP, MODEL_RATE};

const SECS: f64 = 12.0;
const PEAK: f64 = 0.3;

struct Net(Arc<Vec<u8>>);

impl ByteSource for Net {
    fn open(&self, _url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let mut c = Cursor::new(self.0.as_ref().clone());
        c.set_position(from);
        Ok(Body { start: from, len: Some(self.0.len() as u64), reader: Box::new(c) })
    }
}

/// Nothing whole: the masks come from the disk.
struct Nowhere;

impl Shelf for Nowhere {
    fn whole(&self, _id: &str) -> Option<Whole> {
        None
    }
}

/// The left channel's RMS over `from..to` seconds of what the card heard.
fn level(card: &Card, from: f64, to: f64) -> f64 {
    let heard = card.heard.lock();
    let left: Vec<f64> = heard.as_chunks::<2>().0.iter().map(|c| c[0] as f64).collect();
    let w = &left[(from * 44_100.0) as usize..((to * 44_100.0) as usize).min(left.len())];
    (w.iter().map(|v| v * v).sum::<f64>() / w.len() as f64).sqrt()
}

#[test]
fn mask_made_mid_song_turns_its_vocals_down() {
    let dir = nori_testdir::TempDir::new("sing-mid-song");
    let (core, client) = common::own_core(&dir, |p| (p.sing, p.sing_vocal_level) = (true, 0.0));
    let prefs = core.session.settings.current().unwrap();
    let song = common::wav(44_100, &common::sine(44_100, 440.0, SECS, PEAK * 32767.0));
    core.session.register(vec![Song { id: "s".into(), title: "s".into(), duration: SECS as u32, suffix: "wav".into(), ..Default::default() }]);
    core.session.set(vec!["s".into()], Some(0), false, None);
    let analyses = Analyses::of(client.clone());
    let measurer = Measurer::on_shelf(analyses.clone(), Box::new(Nowhere), None);
    let store = Store::open(dir.join("music"), 64 << 20).unwrap();
    let library = CoreLibrary { client, bytes: Arc::new(Net(Arc::new(song))), store: Some(store), analyses: analyses.clone() };
    let app = CoreApp::new(core.session.clone()).singing(analyses);
    let card = Card::new();
    let clock = Virtual::default();
    let time: Stepper<Pull> = Stepper::new(clock.clone(), card.pull.clone());
    let engine = Engine::start_on(library, app, CoreQueue(core.session.clone()), Box::new(card.clone()), None, Config { memory_mb: 64, settings: settings(&prefs, 0.0), ..Config::default() }, clock, |_| {});
    engine.queue_changed();
    engine.play_at(0, 0);
    time.run(Duration::from_secs(3));

    // The song's mask (all vocals) is made, the platform's measurer finds it and says so (Kotlin's replan).
    let fps = (MODEL_RATE / MODEL_HOP as f64) as f32;
    let mask = VocalMask::new(fps, vec![255; (SECS * fps as f64) as usize * bands()]);
    let name: String = "s".bytes().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(dir.join("sing").join("masks")).unwrap();
    std::fs::write(dir.join("sing").join("masks").join(name + ".mask"), mask.to_bytes()).unwrap();
    measurer.ask(core.session.measure());
    measurer.wait();
    engine.replan();
    time.run(Duration::from_secs(6));
    engine.stop();

    let full = PEAK / 2f64.sqrt();
    assert!((level(&card, 0.5, 2.5) / full - 1.0).abs() < 0.05, "no mask yet: as recorded");
    let after = level(&card, 5.0, 8.5);
    assert!(after < full * 0.1, "its vocals down once the mask came: {:.1} dB", 20.0 * (after / full).log10());
}

/// Serves the Open-Unmix checkpoint by ranges once let through; counts the pieces asked for.
#[cfg(feature = "neural-beats")]
struct Authors {
    ckpt: Vec<u8>,
    gate: common::Gate,
    asked: common::Signal,
}

#[cfg(feature = "neural-beats")]
#[async_trait::async_trait]
impl nori_core::transport::Transport for Authors {
    async fn get(&self, _url: String, _timeout_ms: u32) -> Result<nori_core::transport::TransportResponse, nori_core::transport::TransportError> {
        Ok(nori_core::transport::TransportResponse { status: 404, body: Vec::new() })
    }

    async fn send(&self, r: nori_core::transport::Exchange) -> Result<nori_core::transport::TransportResponse, nori_core::transport::TransportError> {
        self.asked.bump();
        self.gate.wait();
        let range = r.headers.get("Range").and_then(|v| v.strip_prefix("bytes=")).and_then(|v| v.split_once('-'));
        let Some((from, to)) = range.and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))) else {
            return Ok(nori_core::transport::TransportResponse { status: 400, body: Vec::new() });
        };
        Ok(nori_core::transport::TransportResponse { status: 206, body: self.ckpt[from..=to.min(self.ckpt.len() - 1)].to_vec() })
    }

    fn address_changed(&self) {}

    fn network(&self) -> nori_core::transport::Network {
        nori_core::transport::Network::Unmetered
    }
}

/// The song's WAV file, whole on disk.
#[cfg(feature = "neural-beats")]
struct File(std::path::PathBuf);

#[cfg(feature = "neural-beats")]
impl Shelf for File {
    fn whole(&self, _id: &str) -> Option<Whole> {
        Some(Whole { files: vec![self.0.clone()], hint: Some("wav".into()) })
    }
}

/// A 16-bit stereo WAV's samples, as floats.
#[cfg(feature = "neural-beats")]
fn pcm(wav: &[u8]) -> Vec<f32> {
    let at = wav.windows(4).position(|w| w == b"data").expect("a data chunk") + 8;
    wav[at..].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / 32768.0).collect()
}

/// The left channel's energy per second in the voice's range (a 300 Hz high-pass, then a 3.4 kHz low-pass).
#[cfg(feature = "neural-beats")]
fn voice_energy(stereo: &[f32]) -> Vec<f64> {
    let (rate, mut hp, mut lp, mut x1) = (44_100.0f64, 0.0f64, 0.0f64, 0.0f64);
    let a_hp = 1.0 / (1.0 + std::f64::consts::TAU * 300.0 / rate);
    let a_lp = std::f64::consts::TAU * 3400.0 / rate / (1.0 + std::f64::consts::TAU * 3400.0 / rate);
    let band: Vec<f64> = stereo
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            let x = c[0] as f64;
            hp = a_hp * (hp + x - x1);
            x1 = x;
            lp += a_lp * (hp - lp);
            lp
        })
        .collect();
    band.chunks(44_100).map(|s| s.iter().map(|v| v * v).sum::<f64>()).collect()
}

/// The mask's bands in the voice's range, 300 Hz to 3.4 kHz (`nori_player::sing`'s bands: one 10.77 Hz bin
/// each at the bottom, then 3 % of their frequency wide).
#[cfg(feature = "neural-beats")]
fn voice_bands() -> std::ops::Range<usize> {
    let mut edges = vec![0];
    while *edges.last().unwrap() < nori_player::sing::MODEL_BINS {
        let at = *edges.last().unwrap();
        edges.push((at + (at * 3 / 100).max(1)).min(nori_player::sing::MODEL_BINS));
    }
    let bin = |hz: f64| (hz * nori_player::sing::MODEL_FFT as f64 / MODEL_RATE) as usize;
    edges.partition_point(|e| *e <= bin(300.0))..edges.partition_point(|e| *e <= bin(3400.0))
}

/// Sing on with no model: the user downloads it while a song plays and the measurer, which found the
/// download taken, gives up on it; once the model is in, the song playing gets its mask and its voice goes
/// down by a lot. Needs the authors' checkpoint and a song with singing, a 16-bit stereo 44.1 kHz WAV:
/// `NORI_UMX_CKPT=vocals-b62c91ce.pth NORI_SING_SONG=song.wav cargo test --release -p nori-engine
/// --features neural-beats,testing --test engine model_downloaded_mid_song`.
#[cfg(feature = "neural-beats")]
#[test]
fn model_downloaded_mid_song_masks_the_song_playing() {
    let (Ok(ckpt), Ok(song)) = (std::env::var("NORI_UMX_CKPT"), std::env::var("NORI_SING_SONG")) else {
        eprintln!("no checkpoint in NORI_UMX_CKPT or song in NORI_SING_SONG: skipped");
        return;
    };
    let dir = nori_testdir::TempDir::new("sing-download");
    let (core, _) = common::own_core(&dir, |p| (p.sing, p.sing_vocal_level) = (true, 0.0));
    let authors = Arc::new(Authors { ckpt: std::fs::read(ckpt).unwrap(), gate: common::Gate::default(), asked: common::Signal::default() });
    let client = nori_core::client::Client::new(core.clone(), authors.clone(), Default::default());
    client.set_profile(nori_core::client::NetProfile { url: "http://music.test".into(), ..Default::default() });
    let prefs = core.session.settings.current().unwrap();
    let wav = std::fs::read(&song).unwrap();
    let original = pcm(&wav);
    let secs = original.len() as f64 / 2.0 / 44_100.0;
    core.session.register(vec![Song { id: "s".into(), title: "s".into(), duration: secs as u32, suffix: "wav".into(), ..Default::default() }]);
    core.session.set(vec!["s".into()], Some(0), false, None);
    let analyses = Analyses::of(client.clone());
    let measurer = Measurer::on_shelf(analyses.clone(), Box::new(File(song.into())), None);
    let store = Store::open(dir.join("music"), 64 << 20).unwrap();
    let library = CoreLibrary { client, bytes: Arc::new(Net(Arc::new(wav))), store: Some(store), analyses: analyses.clone() };
    let card = Card::new();
    let clock = Virtual::default();
    let time: Stepper<Pull> = Stepper::new(clock.clone(), card.pull.clone());
    let engine = Engine::start_on(library, CoreApp::new(core.session.clone()).singing(analyses.clone()), CoreQueue(core.session.clone()), Box::new(card.clone()), None, Config { memory_mb: 128, settings: settings(&prefs, 0.0), ..Config::default() }, clock, |_| {});
    engine.queue_changed();
    engine.play_at(0, 0);
    time.run(Duration::from_secs(5));

    use nori_core::settings_model::SingNow;
    let download = analyses.sing_download_now().unwrap();
    authors.asked.reach(1);
    assert_eq!(analyses.sing_now("s"), SingNow::Downloading);
    measurer.ask(core.session.measure());
    measurer.wait();
    authors.gate.open();
    download.join().unwrap();
    measurer.wait();
    assert_eq!(analyses.sing_now("s"), SingNow::Singing, "the measurer looked again once the model was in");
    engine.replan();
    time.run(Duration::from_secs(secs as u64 - 6));
    engine.stop();

    let heard = card.heard.lock().clone();
    let (before, after) = (voice_energy(&original), voice_energy(&heard));
    let first: Vec<f64> = (1..4).map(|s| 10.0 * (after[s] / before[s]).log10()).collect();
    assert!(first.iter().all(|d| d.abs() < 0.5), "before the mask, as recorded: {first:?}");
    // The seconds the mask calls most vocal, from a second after it came.
    let kept = std::fs::read_dir(dir.join("sing").join("masks")).unwrap().next().unwrap().unwrap().path();
    let mask = VocalMask::from_bytes(&std::fs::read(kept).unwrap()).unwrap();
    let voice = voice_bands();
    let share = |s: usize| {
        let rows = (s as f32 * mask.fps) as usize..((s + 1) as f32 * mask.fps) as usize;
        rows.flat_map(|k| mask.row(k)[voice.clone()].iter().map(|v| *v as f64)).sum::<f64>()
    };
    let mut secs: Vec<usize> = (6..after.len().min(before.len()) - 1).collect();
    secs.sort_by(|a, b| share(*b).total_cmp(&share(*a)));
    let down: Vec<f64> = secs[..10].iter().map(|s| 10.0 * (after[*s] / before[*s]).log10()).collect();
    let mean = down.iter().sum::<f64>() / down.len() as f64;
    eprintln!("the voice's range down by {mean:.1} dB over the song's ten most vocal seconds: {down:.1?}");
    assert!(mean < -4.0, "the singing turned down: {mean:.1} dB");
}
