//! A beat-matched AutoMix plays the incoming song 4 % fast and ramps it back after. Whatever happens
//! meanwhile (seek, skip, pause), the song must end up at its own pitch, measured from a tone. The
//! songs are 44.1 and 48 kHz, so the incoming one is resampled.

use crate::common;

use std::path::PathBuf;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_engine::{Config, Engine, Library, Located, SharedQueue, Source};
use nori_player::automix::analysis::Analyzer;
use nori_player::automix::plan;
use nori_player::engine::{Host, Plan};
use nori_player::pipeline::App;
use nori_player::sim;
use nori_player::transitions::WindowSong;
use nori_player::types::AutoMixSettings;

const HZ: f64 = 1000.0;

/// `secs` of a sine at [`HZ`] as a WAV file at `rate`.
fn wav(rate: u32, secs: f64) -> Vec<u8> {
    common::wav(rate, &common::sine(rate, HZ, secs, 8000.0))
}

/// `a` at 44.1 kHz (it opens the output), `b` and `c` at 48 kHz, as files in a temporary directory.
struct Songs(Vec<(String, PathBuf, i64)>, #[allow(dead_code)] nori_testdir::TempDir);

impl Songs {
    fn new() -> Songs {
        let dir = nori_testdir::TempDir::new("tempo");
        let songs = [("a", 44_100, 12.0), ("b", 48_000, 30.0), ("c", 48_000, 30.0)];
        Songs(
            songs
                .iter()
                .map(|&(id, rate, secs)| {
                    let path = dir.join(format!("{id}.wav"));
                    std::fs::write(&path, wav(rate, secs)).unwrap();
                    (id.to_string(), path, (secs * 1000.0) as i64)
                })
                .collect(),
            dir,
        )
    }
}

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let (_, path, ms) = self.0.iter().find(|s| s.0 == id).cloned().ok_or("no such song")?;
        Ok(Located { source: Source::File(path), hint: Some("wav".into()), duration_ms: Some(ms), estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        let ms = self.0.iter().find(|s| s.0 == id).map_or(0, |s| s.2);
        WindowSong { id: id.into(), title: id.into(), duration_ms: ms, ..Default::default() }
    }
}

/// A 3 s mix out of `a` at 8 s into `b` played 4 % fast (pitch not kept), ramped back over a second.
fn beat_matched() -> Plan {
    let s = AutoMixSettings { max_transition_s: 3.0, ..Default::default() };
    let t = plan::plan(None, None, 12_000, 30_000, &s);
    Plan {
        incoming_id: "b".into(),
        out_start_us: 8_000_000,
        duration_us: 3_000_000,
        in_skip_us: 0,
        mixer: t.clone(),
        tempo_ratio: 1.04,
        keep_pitch: false,
        ramp_us: 1_000_000,
        out_loop_us: 0,
    }
}

/// The simulated app, planning [`beat_matched`] out of `a`.
struct Planned(sim::App);

impl Host for Planned {
    fn plan_for(&mut self, id: &str) -> Option<Plan> {
        (id == "a").then(beat_matched)
    }
    fn wants_analysis(&mut self, id: &str) -> Option<u64> {
        self.0.wants_analysis(id)
    }
    fn analysed(&mut self, id: &str, a: Analyzer, channels: usize, frames: u64, rate: u32) {
        self.0.analysed(id, a, channels, frames, rate)
    }
    fn log(&mut self, message: &str) {
        self.0.log(message)
    }
    fn now_ms(&self) -> i64 {
        self.0.now_ms()
    }
}

impl App for Planned {
    fn clock(&mut self, now_ms: i64) {
        self.0.clock(now_ms)
    }
    fn auto_mix(&self) -> bool {
        true
    }
    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool) {
        self.0.window(window, shuffling)
    }
    fn transitions_off(&mut self, off: bool) {
        self.0.transitions_off(off)
    }
    fn gain(&mut self, index: usize, id: &str) -> f32 {
        self.0.gain(index, id)
    }
}

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    card: Card,
}

impl Rig {
    /// Plays `a`, `b`, `c` until the mix out of `a` is heard.
    fn in_the_mix() -> Rig {
        let queue = SharedQueue::default();
        queue.0.lock().set(vec!["a".into(), "b".into(), "c".into()], Some(0), false, 0);
        let card = Card::new();
        let clock = Virtual::default();
        let mut app = sim::App::new();
        app.prefs = sim::prefs_off();
        let engine = Engine::start_on(Songs::new(), Planned(app), queue, Box::new(card.clone()), None, Config::default(), clock.clone(), |_| {});
        engine.queue_changed();
        engine.play_at(0, 0);
        let rig = Rig { engine, time: Stepper::new(clock, card.pull.clone()), card };
        assert!(rig.until(30, |r| r.engine.status().mixing), "the mix is heard");
        rig
    }

    fn until(&self, secs: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        self.time.until(Duration::from_secs(secs), || done(self))
    }

    fn run(&self, ms: u64) {
        self.time.run(Duration::from_millis(ms));
    }

    fn assert_own_pitch(&self, what: &str) {
        let hz = self.card.last_second_hz();
        assert!((hz - HZ).abs() < 3.0, "{what}: the song plays at {hz} Hz, not {HZ} (x{:.4})", hz / HZ);
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

#[test]
fn mix_plays_incoming_at_mix_tempo() {
    let rig = Rig::in_the_mix();
    rig.run(1_800);
    assert!(rig.engine.status().mixing);
    let hz = rig.card.last_second_hz();
    assert!(hz > HZ * 1.01, "the tempo is matched: {hz} Hz");
}

#[test]
fn tempo_restored() {
    let rig = Rig::in_the_mix();
    rig.run(8_000);
    assert!(!rig.engine.status().mixing);
    rig.assert_own_pitch("after the mix and the ramp");

    // Seek in mix restores tempo.
    let rig = Rig::in_the_mix();
    rig.run(1_000);
    rig.engine.seek(25_000);
    rig.run(3_000);
    rig.assert_own_pitch("after a seek in the mix");

    // Seek in tempo ramp restores tempo.
    let rig = Rig::in_the_mix();
    assert!(rig.until(30, |r| !r.engine.status().mixing), "the mix ends");
    // Inside the second-long ramp.
    rig.run(300);
    rig.engine.seek(20_000);
    rig.run(3_000);
    rig.assert_own_pitch("after a seek in the ramp");
    rig.engine.seek(27_000);
    rig.run(2_000);
    rig.assert_own_pitch("after a second seek, near the end");

    // Skip in mix restores tempo.
    let rig = Rig::in_the_mix();
    rig.run(1_000);
    rig.engine.next();
    rig.run(3_000);
    rig.assert_own_pitch("the song skipped to");
    rig.engine.previous();
    rig.run(3_000);
    rig.assert_own_pitch("the song skipped back to");

    // Pause in mix keeps tempo.
    let rig = Rig::in_the_mix();
    rig.run(1_000);
    rig.engine.pause();
    rig.run(3_000);
    rig.engine.play();
    rig.run(8_000);
    rig.assert_own_pitch("after a pause in the mix");
}

