//! A sound card on the virtual clock that records everything it plays (as float) and every format it
//! was opened in.

use std::sync::Arc;

use nori_engine::{AudioOutput, Feed, OutputFormat};
use parking_lot::Mutex;

/// The card's puller, driven by the clock.
#[derive(Default)]
pub struct Pull {
    feed: Option<Feed>,
    playing: bool,
    due_ns: i64,
    heard: Arc<Mutex<Vec<f32>>>,
    block: Vec<f32>,
    /// Pulls that found less than a block while playing: underruns.
    pub dry: u64,
    /// Takes no music until opened again, as a dead device.
    dead: bool,
    /// Reported latency, µs; a test changes it to move the engine's clock reading back a little.
    pub latency_us: u64,
}

impl Pull {
    /// Stops taking music until the engine opens the device again.
    pub fn pause_pulling(&mut self) {
        self.dead = true;
    }
}

/// Frames per pull.
const BLOCK: usize = 512;

impl super::Device for Pull {
    fn due_ns(&self) -> i64 {
        self.due_ns
    }

    fn tick(&mut self, now_ns: i64) -> bool {
        let rate = self.feed.as_ref().map_or(44_100, |f| f.format().rate);
        self.due_ns = now_ns + (BLOCK as i64 * 1_000_000_000) / rate as i64;
        let Some(feed) = self.feed.as_mut() else { return false };
        if self.dead {
            return false;
        }
        if !self.playing || (feed.available() < BLOCK && !feed.ending()) {
            if self.playing && !feed.ending() && !self.heard.lock().is_empty() {
                self.dry += 1;
            }
            return false;
        }
        let ch = feed.format().channels;
        self.block.resize(BLOCK * ch, 0.0);
        let waits = feed.engine_waits();
        let got = feed.pull(&mut self.block);
        self.heard.lock().extend_from_slice(&self.block[..got * ch]);
        waits && !feed.engine_waits()
    }
}

/// What the card heard since last opened, and every format it was opened in.
#[derive(Clone)]
pub struct Card {
    pub heard: Arc<Mutex<Vec<f32>>>,
    pub opened: Arc<Mutex<Vec<OutputFormat>>>,
    pub pull: Arc<Mutex<Pull>>,
}

impl Card {
    pub fn new() -> Card {
        let heard: Arc<Mutex<Vec<f32>>> = Arc::default();
        Card { heard: heard.clone(), opened: Arc::default(), pull: Arc::new(Mutex::new(Pull { heard, ..Pull::default() })) }
    }

    pub fn format(&self) -> Option<OutputFormat> {
        self.opened.lock().last().copied()
    }

    /// Seconds heard since last opened.
    pub fn secs(&self) -> f64 {
        self.format().map_or(0.0, |f| self.heard.lock().len() as f64 / f.channels as f64 / f.rate as f64)
    }

    /// The frequency of the last second heard, from zero crossings of the left channel.
    pub fn last_second_hz(&self) -> f64 {
        let f = self.format().expect("opened");
        let heard = self.heard.lock();
        let left: Vec<f32> = heard.chunks_exact(f.channels).map(|c| c[0]).collect();
        assert!(left.len() >= f.rate as usize, "a second heard: {} frames", left.len());
        let w = &left[left.len() - f.rate as usize..];
        w.windows(2).filter(|p| (p[0] < 0.0) != (p[1] < 0.0)).count() as f64 / 2.0
    }
}

impl AudioOutput for Card {
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        self.opened.lock().push(want);
        self.heard.lock().clear();
        Ok(want)
    }

    fn start(&mut self, feed: Feed) -> Result<(), String> {
        let mut p = self.pull.lock();
        p.feed = Some(feed);
        p.dead = false;
        Ok(())
    }

    fn pause(&mut self) {
        self.pull.lock().playing = false;
    }

    fn resume(&mut self) {
        self.pull.lock().playing = true;
    }

    fn latency_us(&self) -> u64 {
        self.pull.lock().latency_us
    }

    fn takes_float(&mut self) -> bool {
        true
    }

    fn close(&mut self) {
        self.pull.lock().feed = None;
    }
}
