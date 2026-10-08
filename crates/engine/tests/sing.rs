//! Sing over a core: a song's vocal mask is made from its samples as the engine reads them, so its vocals go down
//! about a second in, while it is still streaming; after a seek, from there; and a song masked whole is not made
//! again.
use crate::common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_core::Song;
use nori_engine::core::{settings, Analyses, CoreApp, CoreLibrary, CoreQueue};
use nori_engine::sing::Vocals;
use nori_engine::{Body, ByteSource, Config, Engine, Store};
use nori_player::sing::{Separator, VocalMask, MODEL_BINS};

const PEAK: f64 = 0.3;

#[cfg(feature = "neural-beats")]
struct Net(Arc<Vec<u8>>);

#[cfg(feature = "neural-beats")]
impl ByteSource for Net {
    fn open(&self, _url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let mut c = std::io::Cursor::new(self.0.as_ref().clone());
        c.set_position(from);
        Ok(Body { start: from, len: Some(self.0.len() as u64), reader: Box::new(c) })
    }
}

/// A server sending a song at twice its bitrate on the test's clock: it is still arriving while it plays.
struct Slow {
    song: Arc<Vec<u8>>,
    clock: Virtual,
}

/// The bytes from `at` on, the nth sent not before n / `rate` seconds of the clock after `t0`.
struct Paced {
    song: Arc<Vec<u8>>,
    at: usize,
    clock: Virtual,
    rate: usize,
    from: usize,
    t0: i64,
}

impl std::io::Read for Paced {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.song.len() - self.at).min(16 << 10);
        self.clock.wait_until(self.t0 + ((self.at + n - self.from) as u128 * 1_000_000_000 / self.rate as u128) as i64);
        buf[..n].copy_from_slice(&self.song[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl ByteSource for Slow {
    fn open(&self, _url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let reader = Paced { song: self.song.clone(), at: from as usize, clock: self.clock.clone(), rate: 2 * 44_100 * 4, from: from as usize, t0: self.clock.now_ns() };
        Ok(Body { start: from, len: Some(self.song.len() as u64), reader: Box::new(reader) })
    }
}

/// The left channel's RMS over `from..to` seconds of what the card heard.
fn level(card: &Card, from: f64, to: f64) -> f64 {
    let heard = card.heard.lock();
    let left: Vec<f64> = heard.as_chunks::<2>().0.iter().map(|c| c[0] as f64).collect();
    let w = &left[(from * 44_100.0) as usize..((to * 44_100.0) as usize).min(left.len())];
    (w.iter().map(|v| v * v).sum::<f64>() / w.len() as f64).sqrt()
}

/// A vocals model that calls everything vocals, counting the frames it reads.
#[derive(Default)]
struct AllVocals(Arc<AtomicUsize>);

impl Separator for AllVocals {
    fn separate(&self, _mags: Vec<f32>, frames: usize, row: &mut dyn FnMut(usize, &[f32])) -> Result<(), String> {
        self.0.fetch_add(frames, Ordering::Relaxed);
        let ones = vec![1.0; 2 * MODEL_BINS];
        (0..frames).for_each(|k| row(k, &ones));
        Ok(())
    }
}

impl Vocals for AllVocals {
    fn ready(&self, _: &nori_core::client::Client) -> bool {
        true
    }

    fn fetch(&self, _: &nori_core::client::Client) -> bool {
        true
    }

    fn load(&self, _: &nori_core::client::Client) -> Option<Box<dyn Separator>> {
        Some(Box::new(AllVocals(self.0.clone())))
    }
}

/// An engine streaming one tone of `secs` with Sing on (vocals at 0), its masks made by [`AllVocals`]; the card,
/// the clock, the frames the model read, the analyses and the test's directory.
fn singing(name: &str, secs: f64) -> (Engine, Card, Stepper<Pull>, Arc<AtomicUsize>, Arc<Analyses>, nori_testdir::TempDir) {
    let dir = nori_testdir::TempDir::new(name);
    let (core, client) = common::own_core(&dir, |p| (p.sing, p.sing_vocal_level) = (true, 0.0));
    let prefs = core.session.settings.current().unwrap();
    let song = common::wav(44_100, &common::sine(44_100, 440.0, secs, PEAK * 32767.0));
    core.session.register(vec![Song { id: "s".into(), title: "s".into(), duration: secs as u32, suffix: "wav".into(), ..Default::default() }]);
    core.session.set(vec!["s".into()], Some(0), false, None);
    let analyses = Analyses::of(client.clone());
    let read = Arc::new(AtomicUsize::new(0));
    analyses.use_vocals(Arc::new(AllVocals(read.clone())));
    let store = Store::open(dir.join("music"), 64 << 20).unwrap();
    let clock = Virtual::default();
    let library = CoreLibrary { client, bytes: Arc::new(Slow { song: Arc::new(song), clock: clock.clone() }), store: Some(store), analyses: analyses.clone() };
    let app = CoreApp::new(core.session.clone()).singing(analyses.clone());
    let card = Card::new();
    let time: Stepper<Pull> = Stepper::new(clock.clone(), card.pull.clone());
    let engine = Engine::start_on(library, app, CoreQueue(core.session.clone()), Box::new(card.clone()), None, Config { memory_mb: 64, settings: settings(&prefs, 0.0), ..Config::default() }, clock, |_| {});
    engine.queue_changed();
    (engine, card, time, read, analyses, dir)
}

#[test]
fn streamed_song_sung_a_second_in() {
    let (engine, card, time, _, _, _dir) = singing("sing-start", 12.0);
    // The song comes at twice its bitrate: it is still arriving for its first six seconds.
    engine.play_at(0, 0);
    time.run(Duration::from_secs(11));
    engine.stop();
    let full = PEAK / 2f64.sqrt();
    let after = level(&card, 1.1, 10.5);
    assert!(after < full * 0.02, "its vocals down from a second in: {:.1} dB", 20.0 * (after / full).log10());
}

#[test]
fn seek_sung_a_second_after() {
    let (engine, card, time, _, analyses, _dir) = singing("sing-seek", 60.0);
    engine.play_at(0, 0);
    time.run(Duration::from_secs(3));
    let at = card.heard.lock().len() as f64 / 2.0 / 44_100.0;
    engine.seek(40_000);
    time.run(Duration::from_secs(6));
    engine.stop();
    let heard = card.heard.lock().len() as f64 / 2.0 / 44_100.0;
    assert!(heard > at + 2.5, "it played on after the seek");
    let full = PEAK / 2f64.sqrt();
    let after = level(&card, at + 1.1, heard);
    assert!(after < full * 0.02, "its vocals down a second after the seek: {:.1} dB", 20.0 * (after / full).log10());
    // From the seek point's history on, not the music between.
    let mask = analyses.vocal_mask("s").unwrap();
    let frame = |s: f64| (s * mask.fps as f64) as usize;
    assert!(mask.has(frame(38.7)) && mask.has(frame(40.0)), "rows from the history read before the seek point");
    assert!(!mask.has(frame(25.0)), "none of the music between");
}

/// Played to its end, a song's mask is kept whole, and playing it again makes none.
#[test]
fn whole_mask_kept_and_not_made_again() {
    let (engine, card, time, read, _, dir) = singing("sing-again", 8.0);
    engine.play_at(0, 0);
    time.run(Duration::from_secs(10));
    let kept = std::fs::read_dir(dir.join("sing").join("masks")).unwrap().next().unwrap().unwrap().path();
    let mask = VocalMask::from_bytes(&std::fs::read(kept).unwrap()).unwrap();
    assert!(mask.whole() && mask.frames() as f64 > 7.9 * mask.fps as f64);
    let made = read.load(Ordering::Relaxed);
    engine.play_at(0, 0);
    time.run(Duration::from_secs(5));
    engine.stop();
    assert_eq!(read.load(Ordering::Relaxed), made, "the model read nothing the second time");
    let full = PEAK / 2f64.sqrt();
    let again = level(&card, 10.2, 14.5);
    assert!(again < full * 0.02, "sung again from the kept mask: {:.1} dB", 20.0 * (again / full).log10());
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
impl nori_engine::core::Shelf for File {
    fn whole(&self, _id: &str) -> Option<nori_engine::core::Whole> {
        Some(nori_engine::core::Whole { files: vec![self.0.clone()], hint: Some("wav".into()) })
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
    let bin = |hz: f64| (hz * nori_player::sing::MODEL_FFT as f64 / nori_player::sing::MODEL_RATE) as usize;
    edges.partition_point(|e| *e <= bin(300.0))..edges.partition_point(|e| *e <= bin(3400.0))
}

/// How far the voice's range of `heard` is below `original`'s (dB) over the `n` seconds from `from` on that `mask`
/// calls most vocal, each second's drop.
#[cfg(feature = "neural-beats")]
fn voice_down(original: &[f32], heard: &[f32], mask: &VocalMask, from: usize, n: usize) -> Vec<f64> {
    let (before, after) = (voice_energy(original), voice_energy(heard));
    let voice = voice_bands();
    let share = |s: usize| {
        let rows = (s as f32 * mask.fps) as usize..((s + 1) as f32 * mask.fps) as usize;
        rows.filter(|k| mask.has(*k)).flat_map(|k| voice.clone().map(move |b| mask.cell(k, b) as f64)).sum::<f64>()
    };
    let mut secs: Vec<usize> = (from..after.len().min(before.len()) - 1).collect();
    secs.sort_by(|a, b| share(*b).total_cmp(&share(*a)));
    secs[..n].iter().map(|s| 10.0 * (after[*s] / before[*s]).log10()).collect()
}

/// Sing on with no model: the user downloads it while a song plays; once it is in, the song playing is read again
/// for its mask and its voice goes down by a lot. Needs the authors' checkpoint and a song with singing, a 16-bit
/// stereo 44.1 kHz WAV: `NORI_UMX_CKPT=vocals-b62c91ce.pth NORI_SING_SONG=song.wav cargo test --release -p nori-engine
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
    let measurer = nori_engine::core::Measurer::on_shelf(analyses.clone(), Box::new(File(song.into())), None);
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
    time.run(Duration::from_secs(2));
    assert_eq!(analyses.sing_now("s"), SingNow::Singing, "the song playing was read again for its mask once the model was in");
    time.run(Duration::from_secs(secs as u64 - 8));
    engine.stop();

    let heard = pcm_of(&card);
    let first: Vec<f64> = voice_energy(&original).iter().zip(voice_energy(&heard)).skip(1).take(3).map(|(b, a)| 10.0 * (a / b).log10()).collect();
    assert!(first.iter().all(|d| d.abs() < 0.5), "before the model, as recorded: {first:?}");
    let down = voice_down(&original, &heard, &analyses.vocal_mask("s").unwrap(), 8, 10);
    let mean = down.iter().sum::<f64>() / down.len() as f64;
    eprintln!("the voice's range down by {mean:.1} dB over the song's ten most vocal seconds: {down:.1?}");
    assert!(mean < -4.0, "the singing turned down: {mean:.1} dB");
}

/// What the card heard, as floats.
#[cfg(feature = "neural-beats")]
fn pcm_of(card: &Card) -> Vec<f32> {
    card.heard.lock().clone()
}

/// Open-Unmix from the authors' checkpoint, converted once.
#[cfg(feature = "neural-beats")]
struct Ckpt(Vec<u8>);

#[cfg(feature = "neural-beats")]
impl Vocals for Ckpt {
    fn ready(&self, _: &nori_core::client::Client) -> bool {
        true
    }

    fn fetch(&self, _: &nori_core::client::Client) -> bool {
        true
    }

    fn load(&self, _: &nori_core::client::Client) -> Option<Box<dyn Separator>> {
        Some(Box::new(nori_player::sing::model::Unmix::from_weights(&self.0).ok()?))
    }
}

/// A song with singing, still streaming, sung from about a second in: its voice down by a lot from there, the
/// model's CPU said. Needs the checkpoint and song as above: `NORI_UMX_CKPT=vocals-b62c91ce.pth NORI_SING_SONG=song.wav
/// cargo test --release -p nori-engine --features neural-beats,testing --test engine streamed_song_sung_by_the_model
/// -- --nocapture`.
#[cfg(feature = "neural-beats")]
#[test]
fn streamed_song_sung_by_the_model() {
    let (Ok(ckpt), Ok(song)) = (std::env::var("NORI_UMX_CKPT"), std::env::var("NORI_SING_SONG")) else {
        eprintln!("no checkpoint in NORI_UMX_CKPT or song in NORI_SING_SONG: skipped");
        return;
    };
    let weights = nori_player::automix::weights::convert(nori_player::sing::model::GRAPH, &std::fs::read(ckpt).unwrap()).unwrap();
    let dir = nori_testdir::TempDir::new("sing-model");
    let (core, client) = common::own_core(&dir, |p| (p.sing, p.sing_vocal_level) = (true, 0.0));
    let prefs = core.session.settings.current().unwrap();
    let wav = std::fs::read(&song).unwrap();
    let original = pcm(&wav);
    let secs = original.len() as f64 / 2.0 / 44_100.0;
    core.session.register(vec![Song { id: "s".into(), title: "s".into(), duration: secs as u32, suffix: "wav".into(), ..Default::default() }]);
    core.session.set(vec!["s".into()], Some(0), false, None);
    let analyses = Analyses::of(client.clone());
    analyses.use_vocals(Arc::new(Ckpt(weights)));
    let store = Store::open(dir.join("music"), 64 << 20).unwrap();
    let clock = Virtual::default();
    let library = CoreLibrary { client, bytes: Arc::new(Slow { song: Arc::new(wav), clock: clock.clone() }), store: Some(store), analyses: analyses.clone() };
    let card = Card::new();
    let time: Stepper<Pull> = Stepper::new(clock.clone(), card.pull.clone());
    let engine = Engine::start_on(library, CoreApp::new(core.session.clone()).singing(analyses.clone()), CoreQueue(core.session.clone()), Box::new(card.clone()), None, Config { memory_mb: 128, settings: settings(&prefs, 0.0), ..Config::default() }, clock, |_| {});
    engine.queue_changed();
    let t0 = std::time::Instant::now();
    engine.play_at(0, 0);
    time.run(Duration::from_secs(secs as u64 - 1));
    engine.stop();
    eprintln!("{secs:.0} s played in {:.1} s", t0.elapsed().as_secs_f64());

    let heard = pcm_of(&card);
    let mask = analyses.vocal_mask("s").unwrap();
    for n in [5, 10] {
        let down = voice_down(&original, &heard, &mask, 2, n);
        let mean = down.iter().sum::<f64>() / n as f64;
        eprintln!("the voice's range down by {mean:.1} dB over the {n} most vocal seconds from 2 s: {down:.1?}");
        assert!(mean < -4.0, "the singing turned down: {mean:.1} dB");
    }
}
