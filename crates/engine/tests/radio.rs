//! Internet radio end to end (MP3, AAC, HE-AAC, Vorbis, Opus; various rates and channels; ICY titles):
//! the card's output must keep the right pitch and speed and survive mid-stream format changes. Tones
//! are measured by zero crossings; a real HE-AAC capture (`testdata`) is compared with ffmpeg's decode.
//! Tests needing ffmpeg pass trivially without it.

use crate::common;

use std::io::Read;
use std::process::Command;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_engine::{Body, ByteSource, Config, Engine, Library, Located, OutputFormat, SharedQueue, Source};
use nori_player::decode::{lend_platform_aac, Fault, PlatformDecoder};
use nori_player::sim;
use nori_player::transitions::WindowSong;

use common::ffmpeg;

/// Whether ffmpeg has `encoder` (Homebrew's lacks libvorbis; its own Vorbis encoder makes no mono).
fn encodes(encoder: &str) -> bool {
    Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().any(|w| w == encoder))
}

/// A temporary directory.
fn dir() -> nori_testdir::TempDir {
    nori_testdir::TempDir::new("radio")
}

/// A tone encoded by ffmpeg as a station sends it (no Xing/LAME header).
fn tone(hz: u32, secs: f64, rate: u32, channels: u32, codec: &[&str], format: &str) -> Vec<u8> {
    let d = dir();
    let out = d.join(format!("tone.{format}"));
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={hz}:sample_rate={rate}:duration={secs}")])
        .args(["-ac", &channels.to_string()])
        .args(codec)
        .args(["-f", format])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg made a {format} tone");
    std::fs::read(&out).unwrap()
}

fn mp3(hz: u32, secs: f64, rate: u32, channels: u32) -> Vec<u8> {
    tone(hz, secs, rate, channels, &["-c:a", "libmp3lame", "-b:a", "96k", "-write_xing", "0"], "mp3")
}

/// [`mp3`] without an ID3 tag, as between two songs on a station.
fn mp3_bare(hz: u32, secs: f64, rate: u32, channels: u32) -> Vec<u8> {
    tone(hz, secs, rate, channels, &["-c:a", "libmp3lame", "-b:a", "96k", "-write_xing", "0", "-id3v2_version", "0"], "mp3")
}

/// `n` bytes of noise strewn with MPEG sync words (false frame headers), as between songs on a station.
fn junk(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.max(1);
    let mut v: Vec<u8> = (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    for (i, at) in (0..n.saturating_sub(4)).step_by(97).enumerate() {
        v[at] = 0xff;
        v[at + 1] = [0xfb, 0xf3, 0xe3, 0xfa, 0xf2][i % 5];
    }
    v
}

// ---- the station ----

/// Music bytes between ICY blocks.
const EVERY: usize = 16_000;

/// A station sending `bytes` once, with an ICY block every [`EVERY`] bytes.
struct Station(Arc<Vec<u8>>);

struct Live {
    bytes: Arc<Vec<u8>>,
    at: usize,
    since: usize,
    meta: Vec<u8>,
}

impl Read for Live {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.meta.is_empty() {
            let n = buf.len().min(self.meta.len());
            buf[..n].copy_from_slice(&self.meta[..n]);
            self.meta.drain(..n);
            return Ok(n);
        }
        if self.since == EVERY {
            self.since = 0;
            let text = b"StreamTitle='Artist - Song';";
            let blocks = text.len().div_ceil(16);
            self.meta = vec![blocks as u8];
            self.meta.extend_from_slice(text);
            self.meta.resize(1 + blocks * 16, 0);
            return self.read(buf);
        }
        let n = buf.len().min(EVERY - self.since).min(4096).min(self.bytes.len() - self.at);
        buf[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        self.since += n;
        Ok(n)
    }
}

impl ByteSource for Station {
    fn open(&self, _: &str, _: u64) -> Result<Body, nori_engine::OpenError> {
        Err("a live stream is opened live".into())
    }

    fn open_live(&self, _: &str) -> Result<(Body, Option<usize>), String> {
        Ok((Body { start: 0, len: None, reader: Box::new(Live { bytes: self.0.clone(), at: 0, since: 0, meta: Vec::new() }) }, Some(EVERY)))
    }
}

struct Radio(Vec<(String, Arc<Vec<u8>>)>);

impl Library for Radio {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let bytes = self.0.iter().find(|(i, _)| i == id).map(|(_, b)| b.clone()).ok_or("no such station")?;
        // No hint for a station, as on Android.
        Ok(Located { source: Source::Live { url: id.into(), bytes: Arc::new(Station(bytes)) }, hint: None, duration_ms: None, estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        WindowSong { id: id.into(), title: id.into(), radio: true, ..Default::default() }
    }
}

fn app() -> sim::App {
    let mut a = sim::App::new();
    a.prefs = sim::prefs_off();
    a
}

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    card: Card,
}

impl Rig {
    /// Queues the stations and plays the first.
    fn new(stations: Vec<(&str, Vec<u8>)>) -> Rig {
        let queue = SharedQueue::default();
        queue.0.lock().set(stations.iter().map(|s| s.0.to_string()).collect(), Some(0), false, 0);
        let card = Card::new();
        let clock = Virtual::default();
        let radio = Radio(stations.into_iter().map(|(id, b)| (id.to_string(), Arc::new(b))).collect());
        let engine = Engine::start_on(radio, app(), queue, Box::new(card.clone()), None, Config::default(), clock.clone(), |_| {});
        engine.queue_changed();
        engine.play_at(0, 0);
        Rig { engine, time: Stepper::new(clock, card.pull.clone()), card }
    }

    /// Plays until `secs` reached the card since it opened; returns its format and what it heard.
    fn hear(&self, secs: f64) -> (OutputFormat, Vec<f32>) {
        let played = self.time.until(Duration::from_secs(600), || self.card.secs() >= secs);
        assert!(played, "{secs} s reached the card: {} s did", self.card.secs());
        (self.card.format().expect("opened"), self.card.heard.lock().clone())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

/// Plays a station's `bytes` until `secs` reached the card.
fn play(bytes: Vec<u8>, secs: f64) -> (OutputFormat, Vec<f32>) {
    Rig::new(vec![("radio:1", bytes)]).hear(secs)
}

// ---- measuring ----

fn mono(x: &[f32], channels: usize) -> Vec<f32> {
    x.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect()
}

/// A tone's frequency from zero crossings.
fn hz(x: &[f32], rate: u32) -> f64 {
    let crossings = x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
    crossings as f64 / 2.0 / (x.len() as f64 / rate as f64)
}

/// The pitch of each whole second heard (mono) from `from_s`.
fn pitches(heard: &[f32], f: OutputFormat, from_s: usize) -> Vec<f64> {
    let m = mono(heard, f.channels);
    let r = f.rate as usize;
    (from_s..m.len() / r).map(|s| hz(&m[s * r..(s + 1) * r], f.rate)).collect()
}

fn assert_pitch(what: &str, heard: &[f32], f: OutputFormat, want: f64) {
    let p = pitches(heard, f, 1);
    assert!(!p.is_empty(), "{what}: nothing heard past its first second");
    for (s, hz) in p.iter().enumerate() {
        assert!((hz - want).abs() < want * 0.005, "{what}: {hz:.1} Hz in second {}, not {want} (heard {:?})", s + 1, p);
    }
}

/// ffmpeg's decode of `bytes`, interleaved float.
fn reference(bytes: &[u8], rate: u32, channels: usize) -> Vec<f32> {
    let d = dir();
    let input = d.join("in.bin");
    std::fs::write(&input, bytes).unwrap();
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&input)
        .args(["-f", "f32le", "-ar", &rate.to_string(), "-ac", &channels.to_string(), "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "ffmpeg decodes it");
    out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// Best normalised correlation of a third of a second within `max_lag` frames: 1 for the same waveform,
/// far lower at another pitch or speed.
fn likeness(ours: &[f32], theirs: &[f32], max_lag: usize) -> f64 {
    let len = 8_192;
    let from = max_lag + 8_192;
    assert!(ours.len() > from + len && theirs.len() > from + len + max_lag, "{} and {} frames", ours.len(), theirs.len());
    let a = &ours[from..from + len];
    let ea: f64 = a.iter().map(|v| (*v as f64).powi(2)).sum();
    let mut best: f64 = 0.0;
    for lag in -(max_lag as i64)..=max_lag as i64 {
        let b = &theirs[(from as i64 + lag) as usize..(from as i64 + lag) as usize + len];
        let eb: f64 = b.iter().map(|v| (*v as f64).powi(2)).sum();
        let dot: f64 = a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum();
        best = best.max(dot / (ea * eb).sqrt().max(1e-12));
    }
    best
}

// ---- the tests ----

#[test]
fn station_pitch_at_any_format() {
    if !ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let aac = ["-c:a", "aac", "-b:a", "64k"];
    let vorbis = ["-c:a", "libvorbis", "-q:a", "3"];
    let opus = ["-c:a", "libopus", "-b:a", "48k"];
    let mut stations: Vec<(&str, Vec<u8>, u32, usize)> = vec![
        ("MP3 44.1 kHz stereo", mp3(1000, 4.0, 44_100, 2), 44_100, 2),
        ("MP3 48 kHz stereo", mp3(1000, 4.0, 48_000, 2), 48_000, 2),
        ("MP3 32 kHz stereo", mp3(1000, 4.0, 32_000, 2), 32_000, 2),
        ("MP3 22.05 kHz mono", mp3(1000, 4.0, 22_050, 1), 22_050, 1),
        ("MP3 24 kHz mono", mp3(1000, 4.0, 24_000, 1), 24_000, 1),
        ("AAC 48 kHz stereo", tone(1000, 4.0, 48_000, 2, &aac, "adts"), 48_000, 2),
        ("AAC 22.05 kHz mono", tone(1000, 4.0, 22_050, 1, &aac, "adts"), 22_050, 1),
    ];
    if encodes("libvorbis") {
        stations.push(("Vorbis 22.05 kHz mono", tone(1000, 4.0, 22_050, 1, &vorbis, "ogg"), 22_050, 1));
    } else {
        eprintln!("no libvorbis: the Vorbis station skipped");
    }
    if encodes("libopus") {
        stations.push(("Opus mono", tone(1000, 4.0, 48_000, 1, &opus, "ogg"), 48_000, 1));
    } else {
        eprintln!("no libopus: the Opus station skipped");
    }
    for (what, bytes, rate, channels) in stations {
        let (f, heard) = play(bytes, 3.0);
        assert_eq!((f.rate, f.channels), (rate, channels), "{what}: the card is opened at the station's own format");
        assert_pitch(what, &heard, f, 1000.0);
    }
}

#[test]
fn station_joined_mid_frame() {
    if !ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    // A station starts mid-frame.
    let bytes = mp3(1000, 4.0, 48_000, 2);
    let (f, heard) = play(bytes[1001..].to_vec(), 3.0);
    assert_eq!(f.rate, 48_000);
    assert_pitch("joined mid-frame", &heard, f, 1000.0);
}

#[test]
fn chained_ogg_station_plays_on() {
    if !ffmpeg() || !encodes("libvorbis") {
        eprintln!("no ffmpeg with libvorbis: skipped");
        return;
    }
    // A chained Ogg station: each song a new logical stream, possibly another format.
    let vorbis = ["-c:a", "libvorbis", "-q:a", "3"];
    let mut bytes = tone(1000, 3.0, 44_100, 2, &vorbis, "ogg");
    bytes.extend_from_slice(&tone(1000, 4.0, 22_050, 1, &vorbis, "ogg"));
    let (f, heard) = play(bytes, 6.0);
    assert_eq!((f.rate, f.channels), (44_100, 2));
    let p = pitches(&heard, f, 0);
    assert!(p.len() >= 6, "it plays on into the next song: {p:?}");
    for (s, hz) in p.iter().enumerate().filter(|(s, _)| *s != 0 && *s != 3) {
        assert!((hz - 1000.0).abs() < 5.0, "{hz:.1} Hz in second {s}: {p:?}");
    }
}

#[test]
fn next_station_at_other_rate() {
    if !ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let rig = Rig::new(vec![("radio:1", mp3(1000, 6.0, 44_100, 2)), ("radio:2", mp3(1000, 6.0, 48_000, 1))]);
    let (f, heard) = rig.hear(2.0);
    assert_pitch("the first station", &heard, f, 1000.0);
    rig.engine.play_at(1, 0);
    rig.card.heard.lock().clear();
    let (f, heard) = rig.hear(3.0);
    assert_pitch("the second station", &heard, f, 1000.0);
}

/// The HE-AAC capture's access units and ffmpeg's full decode (44.1 kHz, 2048 frames each).
static REFERENCE: OnceLock<(Vec<Vec<u8>>, Vec<f32>)> = OnceLock::new();
/// A unit handed to [`Reference`] out of order.
static STRAY_UNIT: AtomicBool = AtomicBool::new(false);

/// Stands in for a platform HE-AAC decoder (MediaCodec): checks each unit is the capture's next and
/// returns ffmpeg's decode of it.
struct Reference {
    next: Option<usize>,
}

impl PlatformDecoder for Reference {
    fn decode(&mut self, unit: &[u8], out: &mut Vec<f32>) -> Result<(usize, u32), Fault> {
        let (units, pcm) = REFERENCE.get().expect("lent only once made");
        // The first unit may be any; a reconnect starts over.
        let at = |i: usize| units.get(i).is_some_and(|u| u == unit);
        let Some(i) = self.next.filter(|&i| at(i)).or_else(|| (0..units.len()).find(|&i| at(i))) else {
            STRAY_UNIT.store(true, Ordering::Relaxed);
            return Err(Fault::BadPacket);
        };
        self.next = Some(i + 1);
        out.extend_from_slice(&pcm[(i * 4096).min(pcm.len())..((i + 1) * 4096).min(pcm.len())]);
        Ok((2, 44_100))
    }

    fn reset(&mut self) {
        self.next = None;
    }
}

/// The ADTS frame payloads of `bytes`.
fn adts_units(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut units = Vec::new();
    let mut at = 0;
    while at + 7 <= bytes.len() && bytes[at] == 0xff && bytes[at + 1] & 0xf6 == 0xf0 {
        let len = ((bytes[at + 3] as usize & 3) << 11) | (bytes[at + 4] as usize) << 3 | (bytes[at + 5] as usize) >> 5;
        let header = if bytes[at + 1] & 1 == 0 { 9 } else { 7 };
        if at + len > bytes.len() {
            break;
        }
        units.push(bytes[at + header..at + len].to_vec());
        at += len;
    }
    units
}

/// The share of energy above `hz` in `x` (mono), by DFT over a few windows.
fn energy_above(x: &[f32], rate: u32, hz: f64) -> f64 {
    let n = 4096;
    let (mut high, mut all) = (0.0, 0.0);
    for w in x.chunks_exact(n).step_by(8).take(6) {
        let hann: Vec<f64> = w.iter().enumerate().map(|(i, v)| *v as f64 * (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos())).collect();
        for k in 1..n / 2 {
            let (step_re, step_im) = ((2.0 * PI * k as f64 / n as f64).cos(), -(2.0 * PI * k as f64 / n as f64).sin());
            let (mut re, mut im, mut c, mut s) = (0.0, 0.0, 1.0f64, 0.0f64);
            for v in &hann {
                re += v * c;
                im += v * s;
                (c, s) = (c * step_re - s * step_im, c * step_im + s * step_re);
            }
            let e = re * re + im * im;
            all += e;
            if k as f64 * rate as f64 / n as f64 > hz {
                high += e;
            }
        }
    }
    high / all.max(1e-30)
}

#[test]
fn he_aac_station_decoding() {
    // SomaFM at 32 kbps: ADTS says AAC-LC 22.05 kHz with implicit SBR. Without a platform decoder the
    // core plays at 22.05 kHz: right pitch, nothing above 11 kHz.
    let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/he-aac-32k.aac")).unwrap();
    let (f, heard) = play(bytes.clone(), 4.0);
    assert_eq!((f.rate, f.channels), (22_050, 2), "opened at the core's rate, which is what comes out");
    if !ffmpeg() {
        eprintln!("no ffmpeg: the pitch is not compared, the platform's decoder not stood in for");
        return;
    }
    // Downsampled, ffmpeg's SBR decode matches.
    let theirs = mono(&reference(&bytes, f.rate, f.channels), f.channels);
    let c = likeness(&mono(&heard, f.channels), &theirs, 1024);
    assert!(c > 0.95, "the same music at the same pitch: likeness {c:.3}");

    // With a platform decoder, every unit goes to it in order and the card opens at 44.1 kHz with the
    // SBR band present.
    let whole = reference(&bytes, 44_100, 2);
    REFERENCE.set((adts_units(&bytes), whole.clone())).unwrap();
    lend_platform_aac(|setup| {
        // Lent for this capture only.
        ((setup.rate, setup.channels) == (22_050, 2)).then(|| Box::new(Reference { next: None }) as Box<dyn PlatformDecoder>)
    });
    let (f, heard) = play(bytes.clone(), 4.0);
    assert!(!STRAY_UNIT.load(Ordering::Relaxed), "every unit handed over was the stream's next");
    assert_eq!((f.rate, f.channels), (44_100, 2), "opened at the rate the platform's decoder plays at");
    let ours = mono(&heard, 2);
    let c = likeness(&ours, &mono(&whole, 2), 4096);
    assert!(c > 0.99, "ffmpeg's decode, as it came out: likeness {c:.3}");
    let core = energy_above(&theirs, 22_050, 10_000.0);
    let high = energy_above(&ours, 44_100, 11_025.0);
    eprintln!("energy above 11.025 kHz: {:.2e} of it (the core's above 10 kHz: {core:.2e})", high);
    assert!(high > 1e-3, "the band above the core's is heard: {high:.2e} of the energy");
}

/// Runs `test` on a thread and fails if it takes more than `secs` of real time.
fn within(secs: u64, test: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        test();
        let _ = tx.send(());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(()) => t.join().unwrap(),

        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => std::panic::resume_unwind(t.join().unwrap_err()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("hung: not done in {secs} s"),
    }
}

/// The longest stretch below -40 dBFS in `heard` (mono), s, in 10 ms windows.
fn longest_silence(heard: &[f32], f: OutputFormat) -> f64 {
    let m = mono(heard, f.channels);
    let w = f.rate as usize / 100;
    let (mut run, mut longest) = (0usize, 0usize);
    for c in m.chunks_exact(w) {
        let rms = (c.iter().map(|v| v * v).sum::<f32>() / w as f32).sqrt();
        run = if rms < 0.01 { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest as f64 / 100.0
}

#[test]
fn mp3_station_format_change() {
    if !ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    // Three songs of different formats, each its own tone, joined mid-frame with noise between. The card
    // keeps the first format; the others are converted at their own pitch.
    within(120, || {
        let mut bytes = mp3_bare(1000, 3.0, 44_100, 2)[1001..].to_vec();
        bytes.extend_from_slice(&junk(3000, 7));
        bytes.extend_from_slice(&mp3_bare(1500, 3.0, 48_000, 1));
        bytes.extend_from_slice(&junk(5000, 11));
        bytes.extend_from_slice(&mp3_bare(700, 3.0, 22_050, 2));
        let (f, heard) = play(bytes, 8.5);
        assert_eq!((f.rate, f.channels), (44_100, 2));
        let p = pitches(&heard, f, 0);
        assert!(p.len() >= 8, "it plays on through both changes: {p:?}");
        // Seconds 2 and 5 hold a change; every other second is its song's tone.
        for (s, want) in [(1, 1000.0), (3, 1500.0), (4, 1500.0), (6, 700.0), (7, 700.0)] {
            assert!((p[s] - want).abs() < want * 0.005, "{:.1} Hz in second {s}, not {want}: {p:?}", p[s]);
        }
        // No gap beyond the encoder edges (~50 ms each side).
        let gap = longest_silence(&heard[f.channels * f.rate as usize / 2..], f);
        assert!(gap <= 0.15, "a {gap} s gap");
    });
}

#[test]
fn noisy_mp3_station_never_hangs() {
    if !ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    // Long noise, a cut frame, a second song at another rate, and a mid-frame end: plays what music
    // there is within the real time allowed.
    within(120, || {
        let first = mp3_bare(1000, 2.0, 44_100, 2);
        let second = mp3_bare(1500, 2.0, 32_000, 1);
        let mut bytes = first[1001..].to_vec();
        bytes.extend_from_slice(&junk(200_000, 3));
        bytes.extend_from_slice(&second[..300]);
        bytes.extend_from_slice(&second[..second.len() - 150]);
        let rig = Rig::new(vec![("radio:1", bytes)]);
        let (f, heard) = rig.hear(3.5);
        assert_eq!((f.rate, f.channels), (44_100, 2));
        let m = mono(&heard, f.channels);
        let r = f.rate as usize;
        let first_hz = hz(&m[r / 2..3 * r / 2], f.rate);
        // After the noise: the second song's tone.
        let second_hz = hz(&m[5 * r / 2..7 * r / 2], f.rate);
        assert!((first_hz - 1000.0).abs() < 5.0 && (second_hz - 1500.0).abs() < 7.5, "both songs at their own pitch: {first_hz} then {second_hz}");
    });
}
