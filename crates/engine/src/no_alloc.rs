//! Steady playback must not allocate, per buffer on the engine's thread or ever on the device's. This
//! test binary counts allocations per thread; each path must make none after warm-up.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use nori_player::dsp::{Band, PEAKING};
use nori_player::engine::Downstream;
use nori_player::pcm::{Encoding, Format};
use nori_player::pipeline::{ChainSettings, Sink, Sound};

use crate::output::{AudioOutput, Feed, OutputFormat, RingTrack};

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations(f: impl FnOnce()) -> u64 {
    let before = ALLOCS.with(Cell::get);
    f();
    ALLOCS.with(Cell::get) - before
}

/// A device at `rate` whose feed the test pulls.
struct Hand(Arc<parking_lot::Mutex<Option<Feed>>>, u32);

impl AudioOutput for Hand {
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        Ok(OutputFormat { rate: self.1, channels: want.channels, bits: 0 })
    }
    fn start(&mut self, feed: Feed) -> Result<(), String> {
        *self.0.lock() = Some(feed);
        Ok(())
    }
    fn pause(&mut self) {}
    fn resume(&mut self) {}
    fn latency_us(&self) -> u64 {
        20_000
    }
    fn close(&mut self) {}
}

fn tone(frames: usize, enc: Encoding) -> Vec<u8> {
    (0..frames)
        .flat_map(|i| {
            let v = (i as f64 / 44_100.0 * 440.0 * std::f64::consts::TAU).sin() * 9000.0 / 32768.0;
            let one = match enc {
                Encoding::Pcm16 => ((v * 32768.0) as i16).to_le_bytes().to_vec(),
                Encoding::Float => (v as f32).to_le_bytes().to_vec(),
            };
            [one.clone(), one].concat()
        })
        .collect()
}

/// Allocations while buffers go through the sink and ring and out of the device, after warm-up.
fn steady(device_rate: u32, sound: Sound, speed: f32, skip_silence: bool) -> u64 {
    steady_in(Encoding::Pcm16, device_rate, sound, speed, skip_silence)
}

fn steady_in(encoding: Encoding, device_rate: u32, sound: Sound, speed: f32, skip_silence: bool) -> u64 {
    let fmt = Format { rate: 44_100, channels: 2, encoding };
    let feed = Arc::new(parking_lot::Mutex::new(None));
    let settings = ChainSettings { sound, speed, skip_silence, ..ChainSettings::default() };
    let mut sink = Sink::new(nori_player::burst::BUFFER_US, settings, RingTrack::new(Box::new(Hand(feed.clone(), device_rate))));
    sink.configure(&1, Some(fmt));
    sink.play();
    let data = tone(1152, encoding);
    let mut out = vec![0f32; 2048 * 2];
    let mut feed = feed.lock().take().expect("the device was started");
    let mut pts = 0i64;
    let mut turn = |sink: &mut Sink<RingTrack>, feed: &mut Feed| {
        sink.handle_buffer(&data, 0, pts);
        pts += fmt.us(data.len());
        feed.pull(&mut out);
        sink.position_us(false);
    };
    for _ in 0..400 {
        turn(&mut sink, &mut feed);
    }
    allocations(|| {
        for _ in 0..400 {
            turn(&mut sink, &mut feed);
        }
    })
}

#[test]
fn pcm16_buffer_path_allocates_nothing() {
    assert_eq!(steady(44_100, Sound::default(), 1.0, false), 0, "straight through");
    let eq = Sound { bands: vec![Band { kind: PEAKING, freq: 1000.0, gain_db: 4.0, q: 1.0, channel: 0 }], limiter: true, ..Sound::default() };
    assert_eq!(steady(44_100, eq, 1.25, true), 0, "equalizer, limiter, silence skipping and speed");
    assert_eq!(steady(48_000, Sound::default(), 1.0, false), 0, "resampled for a device at another rate");
}

#[test]
fn float_buffer_path_allocates_nothing() {
    assert_eq!(steady_in(Encoding::Float, 44_100, Sound::default(), 1.0, false), 0, "straight through");
    let eq = Sound { bands: vec![Band { kind: PEAKING, freq: 1000.0, gain_db: 4.0, q: 1.0, channel: 0 }], limiter: true, ..Sound::default() };
    assert_eq!(steady_in(Encoding::Float, 44_100, eq, 1.25, true), 0, "equalizer, limiter and speed in float");
    assert_eq!(steady_in(Encoding::Float, 48_000, Sound::default(), 1.0, false), 0, "resampled from float");
}

/// Allocations per buffer read from `file` after the first 16, in `encoding`.
fn per_read(file: Vec<u8>, hint: &str, encoding: Encoding) -> f64 {
    use nori_player::pipeline::Reading;
    let mut d = crate::demux::Demuxed::open(Box::new(std::io::Cursor::new(file)), Some(hint), 0, None, encoding).expect("opens");
    for _ in 0..16 {
        assert!(d.fill(), "longer than the warm-up");
    }
    let mut reads = 0;
    let made = allocations(|| {
        while d.fill() {
            reads += 1;
        }
    });
    made as f64 / reads as f64
}

/// A stereo 16-bit WAV of `frames` frames of a tone.
fn wav(frames: usize) -> Vec<u8> {
    let data = tone(frames, Encoding::Pcm16);
    let mut w = b"RIFF".to_vec();
    w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    for v in [16u32, 1 | 2 << 16, 44_100, 44_100 * 4, 4 | 16 << 16] {
        w.extend_from_slice(&v.to_le_bytes());
    }
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    w
}

/// Reading a song allocates what symphonia's reader does, one packet each, and nothing of its own: its
/// `FormatReader::next_packet` hands out an owned packet and takes no buffer to read into.
#[test]
fn reading_allocates_only_the_readers_packets() {
    let mp3 = include_bytes!("../../player/testdata/tone440.mp3").to_vec();
    for (file, hint, encoding) in [(wav(441_000), "wav", Encoding::Pcm16), (wav(441_000), "wav", Encoding::Float), (mp3.clone(), "mp3", Encoding::Pcm16), (mp3, "mp3", Encoding::Float)] {
        assert_eq!(per_read(file, hint, encoding), 1.0, "{hint} into {encoding:?}");
    }
}
