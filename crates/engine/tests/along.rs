//! Another device following this one's place: what the engine says ([`Event::Placed`], a seek's
//! [`Event::Position`], the pace) must let a follower running the place on find what the card plays,
//! through another speed, a mix's tempo and skipped silence. The place heard is read off the card: each
//! song's right channel is a sawtooth that says where in the song it is.

use crate::common;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use common::{Stepper, Virtual};
use nori_engine::{AudioOutput, Config, Engine, Event, Feed, Library, Located, OutputFormat, Settings, SharedQueue, Source};
use nori_player::automix::analysis::Analyzer;
use nori_player::automix::plan;
use nori_player::automix::synth::Rng;
use nori_player::engine::{Host, Plan};
use nori_player::pipeline::App;
use nori_player::playlist::Playlist;
use nori_player::sim;
use nori_player::transitions::WindowSong;
use nori_player::types::AutoMixSettings;
use parking_lot::Mutex;

const RATE: u32 = 44_100;
/// The sawtooth's period, ms, and its reach either side of zero.
const SAW_MS: i64 = 2_000;
const SAW: f64 = 20_000.0;
/// Frames the card pulls at a time; it plays each block from the pull on.
const BLOCK: usize = 128;

/// A song `secs` long: noise and tones on the left, the sawtooth on the right, both silent within `quiet` (ms).
fn song(secs: f64, seed: u64, quiet: Option<(i64, i64)>) -> Vec<i16> {
    let mut r = Rng(seed);
    let n = (secs * RATE as f64) as usize;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / RATE as f64;
        let ms = i as i64 * 1000 / RATE as i64;
        if quiet.is_some_and(|(a, b)| (a..b).contains(&ms)) {
            out.extend([0, 0]);
            continue;
        }
        let tone = (std::f64::consts::TAU * 220.0 * (1.0 + seed as f64 * 0.01) * t).sin() * 0.2 + (std::f64::consts::TAU * 1250.0 * t).sin() * 0.1;
        let phase = (i as f64 * 1000.0 / RATE as f64 % SAW_MS as f64) / SAW_MS as f64;
        out.push(((tone + 0.1 * r.next()) * 32767.0) as i16);
        out.push((-SAW + 2.0 * SAW * phase) as i16);
    }
    out
}

/// Songs as WAV files in a temporary directory.
struct Songs(Vec<(String, PathBuf, i64)>, #[allow(dead_code)] Arc<nori_testdir::TempDir>);

impl Songs {
    fn new(songs: &[(&str, &[i16])]) -> Songs {
        let dir = Arc::new(nori_testdir::TempDir::new("along"));
        let files = songs
            .iter()
            .map(|(id, s)| {
                let path = dir.join(format!("{id}.wav"));
                std::fs::write(&path, common::wav(RATE, s)).unwrap();
                (id.to_string(), path, (s.len() / 2) as i64 * 1000 / RATE as i64)
            })
            .collect();
        Songs(files, dir)
    }
}

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let (_, path, ms) = self.0.iter().find(|s| s.0 == id).cloned().ok_or("no such song")?;
        Ok(Located { source: Source::File(vec![path]), hint: Some("wav".into()), duration_ms: Some(ms), estimated: false })
    }

    fn about(&self, id: &str) -> WindowSong {
        let ms = self.0.iter().find(|s| s.0 == id).map_or(0, |s| s.2);
        WindowSong { id: id.into(), title: id.into(), duration_ms: ms, ..Default::default() }
    }
}

/// A beat-matched mix out of `a` at `at_ms` into `b`, its incoming side `tempo` fast for `secs` and ramped
/// back over `ramp_s`.
fn mix(at_ms: i64, secs: f64, tempo: f32, ramp_s: f64) -> Plan {
    let s = AutoMixSettings { max_transition_s: secs as f32, ..Default::default() };
    let t = plan::plan(None, None, 60_000, 60_000, &s);
    Plan {
        incoming_id: "b".into(),
        out_start_us: at_ms * 1000,
        duration_us: (secs * 1e6) as i64,
        in_skip_us: 0,
        mixer: t,
        tempo_ratio: tempo,
        // Varispeed: a stretch keeping the pitch smears the sawtooth.
        keep_pitch: false,
        ramp_us: (ramp_s * 1e6) as i64,
        out_loop_us: 0,
    }
}

/// The simulated app, planning a fixed mix out of `a` (or none).
struct Planned(sim::App, Option<Plan>);

impl Host for Planned {
    fn plan_for(&mut self, id: &str) -> Option<Plan> {
        self.1.clone().filter(|_| id == "a")
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
        false
    }
    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool) {
        self.0.window(window, shuffling)
    }
    fn transitions_off(&mut self, off: bool) {
        self.0.transitions_off(off)
    }
    fn gain(&mut self, list: &Playlist, index: usize) -> f32 {
        self.0.gain(list, index)
    }
}

/// The card's puller: what it played, and when each block began.
#[derive(Default)]
struct Pull {
    feed: Option<Feed>,
    playing: bool,
    due_ns: i64,
    heard: Vec<f32>,
    /// (ns, the first frame of `heard` it pulled).
    pulls: Vec<(i64, usize)>,
    block: Vec<f32>,
}

impl common::Device for Pull {
    fn due_ns(&self) -> i64 {
        self.due_ns
    }

    fn tick(&mut self, now_ns: i64) -> bool {
        self.due_ns = now_ns + BLOCK as i64 * 1_000_000_000 / RATE as i64;
        let Some(feed) = self.feed.as_mut() else { return false };
        if !self.playing || (feed.available() < BLOCK && !feed.ending()) {
            return false;
        }
        let ch = feed.format().channels;
        self.block.resize(BLOCK * ch, 0.0);
        let waits = feed.engine_waits();
        let got = feed.pull(&mut self.block);
        self.pulls.push((now_ns, self.heard.len() / ch));
        self.heard.extend_from_slice(&self.block[..got * ch]);
        waits && !feed.engine_waits()
    }
}

#[derive(Clone, Default)]
struct Card(Arc<Mutex<Pull>>);

impl AudioOutput for Card {
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        assert_eq!(want.rate, RATE);
        Ok(want)
    }
    fn start(&mut self, feed: Feed) -> Result<(), String> {
        self.0.lock().feed = Some(feed);
        Ok(())
    }
    fn pause(&mut self) {
        self.0.lock().playing = false;
    }
    fn resume(&mut self) {
        self.0.lock().playing = true;
    }
    /// The block pulled plays from the pull on.
    fn latency_us(&self) -> u64 {
        BLOCK as u64 * 1_000_000 / RATE as u64
    }
    fn takes_float(&mut self) -> bool {
        true
    }
    fn close(&mut self) {
        self.0.lock().feed = None;
    }
}

impl Card {
    /// Where in its song the card was at `ns`, read off the sawtooth near `near_ms` (a line through the
    /// 20 ms before, as a stretch moves what is heard a little either way); None while it played nothing,
    /// silence, or the sawtooth's turn.
    fn place_at(&self, ns: i64, near_ms: f64) -> Option<f64> {
        const AROUND: usize = 441;
        let p = self.0.lock();
        let at = p.pulls.partition_point(|&(t, _)| t <= ns).checked_sub(1)?;
        let (t, first) = p.pulls[at];
        let frame = first + ((ns - t) as f64 * RATE as f64 / 1e9) as usize;
        let phases: Vec<(f64, f64)> = (frame.checked_sub(2 * AROUND)?..frame)
            .map(|f| p.heard.get(f * 2 + 1).map(|v| ((f as f64 - frame as f64), (*v as f64 * 32768.0 + SAW) / (2.0 * SAW) * SAW_MS as f64)))
            .collect::<Option<_>>()?;
        let (lo, hi) = phases.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (_, v)| (lo.min(*v), hi.max(*v)));
        // Silence, or the sawtooth turning within the window.
        if hi - lo < 1.0 || hi - lo > 200.0 {
            return None;
        }
        let n = phases.len() as f64;
        let (mx, my) = (phases.iter().map(|p| p.0).sum::<f64>() / n, phases.iter().map(|p| p.1).sum::<f64>() / n);
        let slope = phases.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / phases.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
        let phase = my - slope * mx;
        let turns = ((near_ms - phase) / SAW_MS as f64).round();
        Some(phase + turns * SAW_MS as f64)
    }
}

/// A place said: (ns, song, ms, pace).
type Said = (i64, usize, i64, f64);

/// One engine on its own virtual clock, over a card, and what a follower of it knows: the place at each
/// event that says it, with the pace then.
struct Rig {
    engine: Arc<Engine>,
    time: Stepper<Pull>,
    card: Card,
    said: Arc<Mutex<Vec<Said>>>,
    events: Arc<Mutex<Vec<(i64, Event)>>>,
}

impl Rig {
    fn new(songs: &[(&str, &[i16])], plan: Option<Plan>, settings: Settings) -> Rig {
        let queue = SharedQueue::default();
        queue.0.lock().set(songs.iter().map(|s| s.0.to_string()).collect(), Some(0), false, 0);
        let card = Card::default();
        let clock = Virtual::default();
        let mut app = sim::App::new();
        app.prefs = sim::prefs_off();
        let slot: Arc<OnceLock<Arc<Engine>>> = Arc::default();
        let said: Arc<Mutex<Vec<Said>>> = Arc::default();
        let events: Arc<Mutex<Vec<(i64, Event)>>> = Arc::default();
        let (e_slot, e_said, e_events, e_clock) = (slot.clone(), said.clone(), events.clone(), clock.clone());
        let config = Config { settings, ..Config::default() };
        let engine = Engine::start_on(Songs::new(songs), Planned(app, plan), queue, Box::new(card.clone()), None, config, clock.clone(), move |e| {
            let ns = e_clock.now_ns();
            // As a host tells its remote: the status on the events that move the place.
            if matches!(e, Event::Placed { .. } | Event::Position { .. } | Event::Song { .. } | Event::State(_)) {
                if let Some(s) = e_slot.get().map(|en| en.status()) {
                    if let Some(i) = s.index {
                        e_said.lock().push((ns, i, s.position_ms, if s.state == nori_engine::State::Playing { s.pace as f64 } else { 0.0 }));
                    }
                }
            }
            e_events.lock().push((ns, e));
        });
        let engine = Arc::new(engine);
        let _ = slot.set(engine.clone());
        engine.queue_changed();
        Rig { engine, time: Stepper::new(clock, card.0.clone()), card, said, events }
    }

    fn now_ns(&self) -> i64 {
        self.time.clock.now_ns()
    }

    fn run(&self, ms: u64) {
        self.time.run(Duration::from_millis(ms));
    }

    /// Where a follower puts the place at `ns`: the last said run on at its pace.
    fn followed(&self, ns: i64) -> Option<(usize, f64)> {
        let said = self.said.lock();
        let &(t, i, ms, pace) = said.iter().rev().find(|s| s.0 <= ns)?;
        Some((i, ms as f64 + (ns - t) as f64 / 1e6 * pace))
    }

    fn placed(&self) -> usize {
        self.events.lock().iter().filter(|(_, e)| matches!(e, Event::Placed { .. })).count()
    }

    /// Runs `ms`, comparing every 50 ms what the follower shows with what the card plays: the gaps, ms.
    fn follow_for(&self, ms: u64) -> Gaps {
        let mut gaps = Gaps::default();
        for _ in 0..ms / 50 {
            self.run(50);
            let ns = self.now_ns();
            let Some((_, f)) = self.followed(ns) else { continue };
            if let Some(heard) = self.card.place_at(ns, f) {
                gaps.0.push(heard - f);
            }
        }
        gaps
    }
}

/// Gaps between two places, ms.
#[derive(Debug, Default)]
struct Gaps(Vec<f64>);

impl Gaps {
    fn worst(&self) -> f64 {
        self.0.iter().fold(0.0, |w, g| w.max(g.abs()))
    }

    fn mean(&self) -> f64 {
        self.0.iter().map(|g| g.abs()).sum::<f64>() / self.0.len().max(1) as f64
    }

    /// At least `n` readings, `mean_ms` apart on average and `worst_ms` at most.
    fn within(&self, n: usize, mean_ms: f64, worst_ms: f64) -> bool {
        self.0.len() >= n && self.mean() <= mean_ms && self.worst() <= worst_ms
    }
}

impl std::fmt::Display for Gaps {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} readings, {:.1} ms apart on average, {:.1} ms at most", self.0.len(), self.mean(), self.worst())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

#[test]
fn a_follower_keeps_up_with_another_speed() {
    let a = song(40.0, 1, None);
    let rig = Rig::new(&[("a", &a)], None, Settings { speed: 1.25, ..Settings::default() });
    rig.engine.play_at(0, 0);
    rig.run(2_000);
    let gaps = rig.follow_for(20_000);
    // A stretch moves what is heard by up to a pitch period either way.
    assert!(gaps.within(300, 4.0, 15.0), "{gaps}");
}

#[test]
fn a_follower_keeps_up_through_a_mix_tempo() {
    let (a, b) = (song(30.0, 1, None), song(40.0, 2, None));
    // Into b 4 % fast for 3 s, back over 4 s.
    let rig = Rig::new(&[("a", &a), ("b", &b)], Some(mix(20_000, 3.0, 1.04, 4.0)), Settings::default());
    rig.engine.play_at(0, 18_000);
    let into_b = || rig.followed(rig.now_ns()).is_some_and(|(i, ms)| i == 1 && ms > 3_200.0);
    for _ in 0..200 {
        if into_b() {
            break;
        }
        rig.run(50);
    }
    assert!(into_b(), "b heard after the mix");
    let gaps = rig.follow_for(8_000);
    assert!(gaps.within(100, 3.0, 6.0), "through the tempo's ramp: {gaps}");
}

#[test]
fn a_skipped_silence_is_said_once() {
    let a = song(40.0, 1, Some((10_000, 13_000)));
    let rig = Rig::new(&[("a", &a)], None, Settings { skip_silence: true, ..Settings::default() });
    rig.engine.play_at(0, 5_000);
    rig.run(3_000);
    let before = rig.placed();
    let gaps = rig.follow_for(4_000);
    let past = rig.followed(rig.now_ns()).unwrap().1;
    assert!(past > 13_000.0, "the silence went by: at {past} ms");
    assert_eq!(rig.placed() - before, 1, "one word for the jump: {:?}", rig.events.lock().iter().filter(|e| matches!(e.1, Event::Placed { .. })).collect::<Vec<_>>());
    assert!(gaps.within(40, 3.0, 6.0), "{gaps}");
}
