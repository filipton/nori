//! An output that writes a WAV file: a device clocked `pace` times faster than real time, so the engine
//! behaves as on a sound card and a queue renders in seconds. Writes what a device would have played
//! (underruns as silence) from the first music to the end, as 16-bit or float samples.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::output::{AudioOutput, Feed, OutputFormat};

/// Frames per pull (a sound card's period).
const PERIOD: usize = 1024;

struct Shared {
    playing: AtomicBool,
    closed: AtomicBool,
    frames: AtomicU64,
}

pub struct WavOutput {
    path: PathBuf,
    pace: f64,
    float: bool,
    format: Option<OutputFormat>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl WavOutput {
    /// Writes to `path`, playing `pace` times faster than real time (1 for real time).
    pub fn new(path: impl Into<PathBuf>, pace: f64) -> WavOutput {
        let shared = Arc::new(Shared { playing: AtomicBool::new(false), closed: AtomicBool::new(false), frames: AtomicU64::new(0) });
        WavOutput { path: path.into(), pace: pace.max(0.01), float: false, format: None, shared, thread: None }
    }

    /// Writes float samples, as a float device would take.
    pub fn in_float(mut self) -> WavOutput {
        self.float = true;
        self
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }
}

fn header(f: OutputFormat, frames: u64, float: bool) -> Vec<u8> {
    let width = if float { 4 } else { 2 };
    let data = frames * f.channels as u64 * width;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    // 1 is integer PCM, 3 IEEE float.
    h.extend_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
    h.extend_from_slice(&(f.channels as u16).to_le_bytes());
    h.extend_from_slice(&f.rate.to_le_bytes());
    h.extend_from_slice(&(f.rate * f.channels as u32 * width as u32).to_le_bytes());
    h.extend_from_slice(&(f.channels as u16 * width as u16).to_le_bytes());
    h.extend_from_slice(&(width as u16 * 8).to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(data as u32).to_le_bytes());
    h
}

fn write_file(path: PathBuf, pace: f64, float: bool, mut feed: Feed, shared: Arc<Shared>) -> Result<(), String> {
    let f = feed.format();
    let mut w = BufWriter::new(File::create(&path).map_err(|e| e.to_string())?);
    w.write_all(&header(f, 0, float)).map_err(|e| e.to_string())?;
    let mut block = vec![0i16; PERIOD * f.channels];
    let mut floats = vec![0f32; PERIOD * f.channels];
    let mut bytes = Vec::with_capacity(block.len() * 4);
    let period = Duration::from_secs_f64(PERIOD as f64 / f.rate as f64 / pace);
    let mut due = Instant::now();
    let mut started = false;
    let mut frames = 0u64;
    while !shared.closed.load(Ordering::Acquire) {
        if !shared.playing.load(Ordering::Acquire) {
            std::thread::park();
            due = Instant::now();
            continue;
        }
        let now = Instant::now();
        if now < due {
            std::thread::sleep(due - now);
        }
        due += period;
        // Skip silence before the first music.
        if !started && feed.available() < PERIOD {
            continue;
        }
        started = true;
        let got = if float { feed.pull(&mut floats) } else { feed.pull_i16(&mut block) };
        let n = if feed.finished() && got < PERIOD { got } else { PERIOD };
        bytes.clear();
        if float {
            bytes.extend(floats[..n * f.channels].iter().flat_map(|v| v.to_le_bytes()));
        } else {
            bytes.extend(block[..n * f.channels].iter().flat_map(|v| v.to_le_bytes()));
        }
        w.write_all(&bytes).map_err(|e| e.to_string())?;
        frames += n as u64;
        shared.frames.store(frames, Ordering::Relaxed);
        if feed.finished() {
            shared.playing.store(false, Ordering::Release);
        }
    }
    w.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    w.write_all(&header(f, frames, float)).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

impl AudioOutput for WavOutput {
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        self.format = Some(want);
        Ok(want)
    }

    fn start(&mut self, feed: Feed) -> Result<(), String> {
        let (path, pace, float, shared) = (self.path.clone(), self.pace, self.float, self.shared.clone());
        let t = std::thread::Builder::new().name("nori-wav".into()).spawn(move || write_file(path, pace, float, feed, shared)).map_err(|e| e.to_string())?;
        self.thread = Some(t);
        Ok(())
    }

    fn pause(&mut self) {
        self.shared.playing.store(false, Ordering::Release);
    }

    fn resume(&mut self) {
        self.shared.playing.store(true, Ordering::Release);
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }

    fn latency_us(&self) -> u64 {
        0
    }

    fn takes_float(&mut self) -> bool {
        self.float
    }

    fn close(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            if let Ok(Err(e)) = t.join() {
                eprintln!("nori: the WAV file could not be written: {e}");
            }
        }
    }
}
