//! Another device following this one's place. First what the engine says ([`Event::Placed`], a seek's
//! [`Event::Position`], the pace) must let a follower running the place on find what the card plays,
//! through another speed, a mix's tempo and skipped silence. Then jam guests listening along
//! ([`Engine::follow`]): each guest's card must play what the host's plays at the same moment, through
//! skewed clocks, late words, another speed, skipped silence, transitions, seeks, pauses and stalls.
//! The place heard is read off the card: each song's right channel is a sawtooth that says where in
//! the song it is.

use crate::common;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use common::{Stepper, Virtual};
use nori_engine::{AudioOutput, Body, ByteSource, Config, Engine, Event, Feed, Followed, Lead, Library, Located, OpenError, OutputFormat, Settings, SharedQueue, Source};
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

/// Songs as WAV files in a temporary directory, fetched as from a server that holds back its answers
/// until [`Net::stalled_until`] on the rig's clock.
struct Songs(Vec<(String, PathBuf, i64)>, Arc<Net>);

struct Net {
    clock: Virtual,
    stalled_until: AtomicI64,
    #[allow(dead_code)]
    dir: nori_testdir::TempDir,
}

impl ByteSource for Net {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        let until = self.stalled_until.load(Ordering::Relaxed);
        if self.clock.now_ns() < until {
            self.clock.wait_until(until);
        }
        let file = std::fs::read(url).map_err(|e| OpenError::from(e.to_string()))?;
        let len = file.len() as u64;
        let mut c = std::io::Cursor::new(file);
        c.set_position(from);
        Ok(Body { start: from, len: Some(len), reader: Box::new(c) })
    }
}

impl Songs {
    fn new(songs: &[(&str, &[i16])], clock: Virtual) -> Songs {
        let dir = nori_testdir::TempDir::new("along");
        let files = songs
            .iter()
            .map(|(id, s)| {
                let path = dir.join(format!("{id}.wav"));
                std::fs::write(&path, common::wav(RATE, s)).unwrap();
                (id.to_string(), path, (s.len() / 2) as i64 * 1000 / RATE as i64)
            })
            .collect();
        Songs(files, Arc::new(Net { clock, stalled_until: AtomicI64::new(0), dir }))
    }
}

impl Library for Songs {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        let (_, path, ms) = self.0.iter().find(|s| s.0 == id).cloned().ok_or("no such song")?;
        let bytes: Arc<dyn ByteSource> = self.1.clone();
        Ok(Located { source: Source::Url { url: path.to_string_lossy().into_owned(), bytes }, hint: Some("wav".into()), duration_ms: Some(ms), estimated: false })
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

/// The simulated app, planning a fixed mix out of `a` (or none), keeping the engine's log lines.
struct Planned(sim::App, Option<Plan>, Arc<Mutex<Vec<String>>>);

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
        self.2.lock().push(message.to_string());
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
    /// (ns heard from, the first frame of `heard` it pulled).
    pulls: Vec<(i64, usize)>,
    block: Vec<f32>,
    /// The output's own delay: a block is heard this long after it is pulled, ns.
    delay_ns: i64,
    /// For this long after each resume the card says it holds only what it pulled, not its delay (an
    /// Android track before its first timestamp), ns.
    unsure_ns: i64,
    resumed_ns: i64,
    resumes: usize,
    /// When each block was pulled, ns.
    pulled_at: Option<i64>,
    /// How much faster than the clock the card plays (its own crystal), parts per million.
    ppm: f64,
    /// Every this many seconds the card misreads its delay by `spike_ns` for a second (an emulator's
    /// AudioTrack latency estimate jumping); 0: never.
    spike_every_s: i64,
    spike_ns: i64,
    /// How much the card holds ahead of what it plays, ns ([`Pull::fill`]); 0: it takes each block as
    /// it plays it.
    holds_ns: i64,
    /// When what it holds is all played, ns.
    queued_until_ns: i64,
    paused_ns: i64,
}

impl Pull {
    /// How long the card takes to play a block, ns.
    fn block_ns(&self) -> i64 {
        (BLOCK as f64 * 1e9 / RATE as f64 / (1.0 + self.ppm / 1e6)) as i64
    }
}

impl common::Device for Pull {
    fn due_ns(&self) -> i64 {
        self.due_ns
    }

    fn tick(&mut self, now_ns: i64) -> bool {
        self.due_ns = now_ns + self.block_ns();
        if self.holds_ns > 0 {
            return self.fill(now_ns);
        }
        let Some(feed) = self.feed.as_mut() else { return false };
        if !self.playing || (feed.available() < BLOCK && !feed.ending()) {
            return false;
        }
        let ch = feed.format().channels;
        self.block.resize(BLOCK * ch, 0.0);
        let waits = feed.engine_waits();
        let got = feed.pull(&mut self.block);
        self.pulled_at = Some(now_ns);
        self.pulls.push((now_ns + self.delay_ns, self.heard.len() / ch));
        self.heard.extend_from_slice(&self.block[..got * ch]);
        waits && !feed.engine_waits()
    }
}

impl Pull {
    /// A card that holds `holds_ns` (a phone's deep track): pulls until it holds that much, plays it on in
    /// order, and drops what it holds unplayed at a flush, giving it back.
    fn fill(&mut self, now_ns: i64) -> bool {
        let block_ns = self.block_ns();
        let Some(feed) = self.feed.as_mut() else { return false };
        if !self.playing {
            return false;
        }
        let ch = feed.format().channels;
        self.block.resize(BLOCK * ch, 0.0);
        let waits = feed.engine_waits();
        while self.queued_until_ns.max(now_ns) - now_ns < self.holds_ns && (feed.available() >= BLOCK || feed.ending() && feed.available() > 0) {
            let got = feed.pull(&mut self.block);
            if feed.flushed() {
                let kept = self.pulls.partition_point(|&(t, _)| t <= now_ns + self.delay_ns);
                let frame = self.pulls.get(kept).map_or(self.heard.len() / ch, |p| p.1);
                let dropped = self.heard.len() / ch - frame;
                self.pulls.truncate(kept);
                self.heard.truncate(frame * ch);
                feed.rewind((dropped + got) as u64);
                self.queued_until_ns = now_ns;
                continue;
            }
            let from = self.queued_until_ns.max(now_ns);
            self.pulls.push((from + self.delay_ns, self.heard.len() / ch));
            self.heard.extend_from_slice(&self.block[..got * ch]);
            self.queued_until_ns = from + block_ns * got as i64 / BLOCK as i64;
            self.pulled_at = Some(now_ns);
        }
        waits && !feed.engine_waits()
    }
}

/// The card, and the clock it plays on.
#[derive(Clone)]
struct Card(Arc<Mutex<Pull>>, Virtual);

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
        let mut p = self.0.lock();
        p.playing = false;
        p.paused_ns = self.1.now_ns();
    }
    fn resume(&mut self) {
        let mut p = self.0.lock();
        let now = self.1.now_ns();
        // What a deep card holds plays on from where it paused.
        if p.holds_ns > 0 && p.queued_until_ns > p.paused_ns {
            let (paused, by) = (p.paused_ns + p.delay_ns, now - p.paused_ns);
            p.pulls.iter_mut().filter(|(t, _)| *t > paused).for_each(|(t, _)| *t += by);
            p.queued_until_ns += by;
        }
        p.playing = true;
        p.resumed_ns = now;
        p.resumes += 1;
    }
    /// The block pulled plays from the pull on, after the output's delay: what is left of it.
    fn latency_us(&self) -> u64 {
        let p = self.0.lock();
        let Some(at) = p.pulled_at else { return 0 };
        let now = self.1.now_ns();
        let delay = if now < p.resumed_ns + p.unsure_ns { 0 } else { p.delay_ns };
        let spike = if p.spike_every_s > 0 && now / 1_000_000_000 % p.spike_every_s == 0 { p.spike_ns } else { 0 };
        if p.holds_ns > 0 {
            // As a phone's track says it: the frames it holds, at their nominal rate.
            let until = if p.playing { p.queued_until_ns } else { p.queued_until_ns + now - p.paused_ns };
            let held = ((until - now).max(0) as f64 * (1.0 + p.ppm / 1e6)) as i64;
            return (held + delay + spike) as u64 / 1000;
        }
        let block = p.block_ns();
        ((at + block + delay - now).clamp(0, block + delay) / 1000) as u64 + (spike / 1000) as u64
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
        let frame = first + ((ns - t) as f64 * RATE as f64 * (1.0 + p.ppm / 1e6) / 1e9) as usize;
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

impl Card {
    /// The `n` frames of the left channel the card played up to `ns`; None before it played that much.
    fn left_before(&self, ns: i64, n: usize) -> Option<Vec<f32>> {
        let p = self.0.lock();
        let at = p.pulls.partition_point(|&(t, _)| t <= ns).checked_sub(1)?;
        let (t, first) = p.pulls[at];
        let end = first + ((ns - t) as f64 * RATE as f64 * (1.0 + p.ppm / 1e6) / 1e9) as usize;
        (end.checked_sub(n)?..end).map(|f| p.heard.get(f * 2).copied()).collect()
    }
}

/// How far the guest's card played behind the host's 20 ms before `ns` (ahead when below zero), ms, read
/// off what both play whatever it is (through a mix too): the lag of the guest's 30 ms that matches the
/// host's best, within 20 ms either way. None while either is silent or they do not match.
fn behind_ms(host: &Card, guest: &Card, ns: i64) -> Option<f64> {
    const WINDOW: usize = 1_323;
    const REACH: i64 = 882;
    let ns = ns - REACH * 1_000_000_000 / RATE as i64;
    let h = host.left_before(ns, WINDOW)?;
    let power = |v: &[f32]| v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
    let hp = power(&h);
    if hp < 1e-3 {
        return None;
    }
    let all = guest.left_before(ns + REACH * 1_000_000_000 / RATE as i64, WINDOW + 2 * REACH as usize)?;
    let mut best = (f64::MIN, 0);
    for lag in -REACH..=REACH {
        let g = &all[(REACH + lag) as usize..][..WINDOW];
        let c = h.iter().zip(g).map(|(a, b)| *a as f64 * *b as f64).sum::<f64>() / (hp * power(g)).sqrt().max(1e-9);
        if c > best.0 {
            best = (c, lag);
        }
    }
    (best.0 > 0.9).then(|| best.1 as f64 * 1000.0 / RATE as f64)
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
    net: Arc<Net>,
    logs: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn new(songs: &[(&str, &[i16])], plan: Option<Plan>, settings: Settings) -> Rig {
        let queue = SharedQueue::default();
        queue.0.lock().set(songs.iter().map(|s| s.0.to_string()).collect(), Some(0), false, 0);
        let clock = Virtual::default();
        let card = Card(Arc::default(), clock.clone());
        let mut app = sim::App::new();
        app.prefs = sim::prefs_off();
        let slot: Arc<OnceLock<Arc<Engine>>> = Arc::default();
        let said: Arc<Mutex<Vec<Said>>> = Arc::default();
        let events: Arc<Mutex<Vec<(i64, Event)>>> = Arc::default();
        let (e_slot, e_said, e_events, e_clock) = (slot.clone(), said.clone(), events.clone(), clock.clone());
        let config = Config { settings, ..Config::default() };
        let library = Songs::new(songs, clock.clone());
        let net = library.1.clone();
        let logs: Arc<Mutex<Vec<String>>> = Arc::default();
        let engine = Engine::start_on(library, Planned(app, plan, logs.clone()), queue, Box::new(card.clone()), None, config, clock.clone(), move |e| {
            let ns = e_clock.now_ns();
            // As a host tells its remote: the status on the events that move the place.
            if matches!(e, Event::Placed { .. } | Event::Position { .. } | Event::Song { .. } | Event::State(_)) {
                if let Some(s) = e_slot.get().map(|en| en.status()) {
                    if let Some(i) = s.index {
                        let mut said = e_said.lock();
                        let pace = if s.state == nori_engine::State::Playing { s.pace as f64 } else { 0.0 };
                        // The status's place is as of its last reading: run on from what was said, as
                        // `Status::position_now` runs it on from its reading's time.
                        let ms = match (&e, said.last()) {
                            (Event::State(_), Some(&(t, at, ms, p))) if at == i && p > 0.0 => ms + ((ns - t) as f64 / 1e6 * p) as i64,
                            _ => s.position_ms,
                        };
                        said.push((ns, i, ms, pace));
                    }
                }
            }
            e_events.lock().push((ns, e));
        });
        let engine = Arc::new(engine);
        let _ = slot.set(engine.clone());
        engine.queue_changed();
        Rig { engine, time: Stepper::new(clock, card.0.clone()), card, said, events, net, logs }
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

/// A host's word as its guests get it: its place at `at_us` on its own clock.
#[derive(Debug, Clone)]
struct Word {
    at_us: i64,
    index: usize,
    ms: f64,
    rate: f64,
    playing: bool,
    speed: f32,
}

/// A guest: its engine, how its clock reads against the virtual one, and the way words take to it.
struct Guest {
    rig: Rig,
    /// Its clock reads this much more than the virtual one; the host's reads `Jam::host_skew_us` more.
    skew_us: i64,
    /// The least a message takes either way, and how late the host's words reach it, ms.
    floor_ms: i64,
    word_ms: i64,
    sync: nori_remote::clock::ClockSync,
    /// Words on their way: (when they arrive, ns).
    inbox: VecDeque<(i64, Word)>,
    listening: bool,
}

/// A host and its guests, every engine on its own clock moved together.
struct Jam {
    host: Rig,
    host_skew_us: i64,
    /// The mix out of the song playing, as the host planned it.
    mix: Option<Plan>,
    speed: f32,
    guests: Vec<Guest>,
    /// The host's words said so far.
    told: usize,
    exchanges: u64,
    /// The host's words carry its plan from then on (ns): before, it had not reached the guests.
    plan_from_ns: i64,
}

/// What a test sets for a guest.
struct Way {
    skew_us: i64,
    floor_ms: i64,
    word_ms: i64,
}

impl Jam {
    fn new(songs: &[(&str, &[i16])], mix: Option<Plan>, settings: Settings, ways: &[Way]) -> Jam {
        let speed = settings.speed;
        let host = Rig::new(songs, mix.clone(), settings);
        let guests = ways
            .iter()
            .map(|w| Guest { rig: Rig::new(songs, None, Settings::default()), skew_us: w.skew_us, floor_ms: w.floor_ms, word_ms: w.word_ms, sync: Default::default(), inbox: VecDeque::new(), listening: false })
            .collect();
        Jam { host, host_skew_us: 4_000_000, mix, speed, guests, told: 0, exchanges: 0, plan_from_ns: 0 }
    }

    fn now_ns(&self) -> i64 {
        self.host.now_ns()
    }

    /// Guest `g` learns the host's clock and starts listening along.
    fn join(&mut self, g: usize) {
        for _ in 0..nori_remote::clock::BURST {
            self.exchange(g);
        }
        self.guests[g].listening = true;
        // It reads the host's last word as it joins.
        if let Some(w) = self.word(self.told.checked_sub(1)) {
            let ns = self.now_ns();
            self.guests[g].inbox.push_back((ns, w));
        }
    }

    /// A time exchange of guest `g` with the host: either way at least its floor, the answer mostly far
    /// later (it waits for a poll), a few ms of jitter.
    fn exchange(&mut self, g: usize) {
        let now = self.now_ns() / 1000;
        self.exchanges += 1;
        let k = self.exchanges;
        let jitter = |salt: u64| ((k * 7919 + salt) % 13) as i64 * 250;
        let (hs, gu) = (self.host_skew_us, &mut self.guests[g]);
        let t1 = now + gu.skew_us;
        let there = gu.floor_ms * 1000 + jitter(1);
        let back = gu.floor_ms * 1000 + jitter(2) + if k % 4 == 1 { 0 } else { 100_000 + (k * 6151 % 200) as i64 * 1000 };
        let t2 = now + there + hs;
        let t3 = t2 + 300;
        let t4 = now + there + 300 + back + gu.skew_us;
        gu.sync.add(nori_remote::clock::Exchange { t1, t2, t3, t4 });
    }

    /// The host's `k`th word.
    fn word(&self, k: Option<usize>) -> Option<Word> {
        let said = self.host.said.lock();
        let &(ns, index, ms, pace) = said.get(k?)?;
        Some(Word { at_us: ns / 1000 + self.host_skew_us, index, ms: ms as f64, rate: if pace > 0.0 { pace } else { 1.0 }, playing: pace > 0.0, speed: self.speed })
    }

    /// Runs `ms` in steps of 5, passing the host's words to its guests the time their way takes.
    fn run(&mut self, ms: u64) {
        for _ in 0..ms / 5 {
            self.host.run(5);
            for g in &self.guests {
                g.rig.run(5);
            }
            let now = self.now_ns();
            let said = self.host.said.lock().len();
            // The plan reaching the guests is a word of its own, with the host's place as last said.
            let planned = (self.plan_from_ns > 0 && self.plan_from_ns <= now && self.plan_from_ns > now - 5_000_000).then(|| said.checked_sub(1)).flatten();
            for k in (self.told..said).chain(planned) {
                let w = self.word(Some(k)).expect("said");
                for g in self.guests.iter_mut().filter(|g| g.listening) {
                    g.inbox.push_back((now + g.word_ms * 1_000_000, w.clone()));
                }
            }
            self.told = said;
            if now / 1_000_000 % 15_000 < 5 {
                for g in 0..self.guests.len() {
                    self.exchange(g);
                }
            }
            for g in &mut self.guests {
                while g.inbox.front().is_some_and(|(at, _)| *at <= now) {
                    let (_, w) = g.inbox.pop_front().expect("checked");
                    let here_us = now / 1000 + g.skew_us;
                    // The host's clock minus this one's.
                    let off = g.sync.offset_at(here_us).expect("the clock learned");
                    let mix = self.mix.clone().filter(|_| now >= self.plan_from_ns + g.word_ms * 1_000_000).map(|p| ("a".to_string(), p));
                    g.rig.engine.follow(Some(Lead { index: w.index, ms: w.ms, ago_us: here_us - (w.at_us - off), there_us: w.at_us, rate: w.rate, playing: w.playing, speed: w.speed, pitch: 1.0, mix }));
                }
            }
        }
    }

    /// Runs `ms`, reading every 50 ms what guest `g`'s card plays against the host's card: the gaps.
    fn gaps(&mut self, g: usize, ms: u64) -> Gaps {
        let mut gaps = Gaps::default();
        for _ in 0..ms / 50 {
            self.run(50);
            let ns = self.now_ns();
            let (h, s) = (self.host.engine.status(), self.guests[g].rig.engine.status());
            if h.mixing || s.mixing || h.index != s.index {
                continue;
            }
            let Some((_, near)) = self.host.followed(ns) else { continue };
            if let (Some(a), Some(b)) = (self.host.card.place_at(ns, near), self.guests[g].rig.card.place_at(ns, near)) {
                gaps.0.push(b - a);
            }
        }
        gaps
    }
}

/// Two guests: clocks seconds off the host's either way, 20 and 60 ms away, words reaching them 80 and
/// 250 ms late.
fn two_ways() -> [Way; 2] {
    [Way { skew_us: -7_000_000, floor_ms: 20, word_ms: 80 }, Way { skew_us: 2_500_000, floor_ms: 60, word_ms: 250 }]
}

/// The gaps each guest's card plays at, after `settle_ms` and over `ms`, are within `mean_ms` on average and
/// `worst_ms` at most.
fn in_step(jam: &mut Jam, settle_ms: u64, ms: u64, mean_ms: f64, worst_ms: f64) {
    jam.run(settle_ms);
    for g in 0..jam.guests.len() {
        let gaps = jam.gaps(g, ms);
        eprintln!("guest {g}: {gaps}");
        assert!(gaps.within((ms / 250) as usize, mean_ms, worst_ms), "guest {g}: {gaps}");
    }
}

#[test]
fn guests_play_in_step_through_skewed_clocks() {
    let a = song(60.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways());
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    jam.join(1);
    in_step(&mut jam, 2_000, 20_000, 2.5, 5.0);
}

#[test]
fn guests_play_at_the_host_speed() {
    let a = song(60.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings { speed: 1.25, ..Settings::default() }, &two_ways());
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    jam.join(1);
    // Each card's stretch moves what it plays by up to a pitch period either way.
    in_step(&mut jam, 2_000, 20_000, 3.5, 15.0);
}

#[test]
fn guests_follow_the_host_changing_speed() {
    let a = song(60.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways());
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    jam.join(1);
    in_step(&mut jam, 2_000, 4_000, 2.5, 5.0);
    jam.speed = 1.25;
    jam.host.engine.set_settings(Settings { speed: 1.25, ..Settings::default() });
    in_step(&mut jam, 5_000, 10_000, 3.5, 15.0);
}

#[test]
fn guests_skip_the_silence_the_host_skips() {
    let a = song(60.0, 1, Some((12_000, 15_000)));
    let mut jam = Jam::new(&[("a", &a)], None, Settings { skip_silence: true, ..Settings::default() }, &two_ways());
    jam.host.engine.play_at(0, 5_000);
    jam.run(1_000);
    jam.join(0);
    jam.join(1);
    in_step(&mut jam, 2_000, 4_000, 2.5, 5.0);
    // Past the silence (shortened to 0.6 s there).
    in_step(&mut jam, 2_000, 8_000, 2.5, 5.0);
}

#[test]
fn guests_play_the_host_mix() {
    let (a, b) = (song(30.0, 1, None), song(40.0, 2, None));
    for (what, plan) in [("crossfade", mix(20_000, 4.0, 1.0, 0.0)), ("beat-matched", mix(20_000, 3.0, 1.04, 4.0))] {
        let mut jam = Jam::new(&[("a", &a), ("b", &b)], Some(plan), Settings::default(), &two_ways());
        jam.host.engine.play_at(0, 12_000);
        jam.run(1_000);
        jam.join(0);
        jam.join(1);
        in_step(&mut jam, 2_000, 4_000, 2.5, 5.0);
        // Through the mix each guest's card plays what the host's does, read off the music itself.
        let mut through = [Vec::new(), Vec::new()];
        for _ in 0..50 {
            jam.run(100);
            let ns = jam.now_ns();
            if jam.host.engine.status().mixing {
                for (g, d) in through.iter_mut().enumerate() {
                    d.extend(behind_ms(&jam.host.card, &jam.guests[g].rig.card, ns));
                }
            }
        }
        for (g, d) in through.iter().enumerate() {
            let worst = d.iter().fold(0.0f64, |w, d| w.max(d.abs()));
            eprintln!("{what}: guest {g} through the mix: {} readings, {worst:.2} ms off at most", d.len());
            assert!(d.len() >= 8 && worst <= 3.0, "{what}: guest {g} through the mix: {d:?}");
        }
        let into_b = |j: &Jam| j.guests.iter().all(|g| g.rig.engine.status().index == Some(1)) && j.host.engine.status().index == Some(1);
        assert!(into_b(&jam), "{what}: all in b");
        let starts = |j: &Jam| j.guests.iter().map(|g| g.rig.events.lock().iter().filter(|(_, e)| matches!(e, Event::Song { index: 1, .. })).count()).collect::<Vec<_>>();
        assert_eq!(starts(&jam), [1, 1], "{what}: each guest went into b once, by its own mix");
        in_step(&mut jam, 3_000, 6_000, 3.0, 5.0);
    }
}

/// The host's plan reaching a guest only once its music passed the plan's start (a slow relay): the guest
/// does not mix from somewhere in it, plays on, and changes song cleanly as soon as the host's word says
/// it did; then plays in step.
#[test]
fn a_late_plan_is_not_mixed_from_its_middle() {
    let (a, b) = (song(30.0, 1, None), song(40.0, 2, None));
    let mut jam = Jam::new(&[("a", &a), ("b", &b)], Some(mix(20_000, 4.0, 1.0, 0.0)), Settings::default(), &two_ways()[..1]);
    jam.host.engine.play_at(0, 14_000);
    jam.run(1_000);
    // The plan reaches the guest as the host is 300 ms into its mix.
    jam.plan_from_ns = jam.now_ns() + 5_300_000_000;
    jam.join(0);
    in_step(&mut jam, 2_000, 2_000, 2.5, 5.0);
    let (mut host_in_b, mut guest_in_b, mut mixed) = (None, None, false);
    for _ in 0..200 {
        jam.run(50);
        let ns = jam.now_ns();
        let g = jam.guests[0].rig.engine.status();
        mixed |= g.mixing;
        host_in_b = host_in_b.or((jam.host.engine.status().index == Some(1)).then_some(ns));
        guest_in_b = guest_in_b.or((g.index == Some(1)).then_some(ns));
    }
    assert!(!mixed, "not mixed from its middle");
    let (h, g) = (host_in_b.expect("the host in b"), guest_in_b.expect("the guest in b"));
    assert!((0..1_000_000_000).contains(&(g - h)), "into b {} ms after the host", (g - h) / 1_000_000);
    in_step(&mut jam, 1_000, 4_000, 2.5, 5.0);
}

#[test]
fn guests_follow_seeks_pauses_and_skips() {
    let (a, b) = (song(60.0, 1, None), song(60.0, 2, None));
    let mut jam = Jam::new(&[("a", &a), ("b", &b)], None, Settings::default(), &two_ways());
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    jam.join(1);
    in_step(&mut jam, 2_000, 3_000, 2.5, 5.0);
    jam.host.engine.seek(30_000);
    in_step(&mut jam, 1_500, 3_000, 2.5, 5.0);
    jam.host.engine.pause();
    jam.run(2_000);
    for g in &jam.guests {
        assert_eq!(g.rig.engine.status().state, nori_engine::State::Paused, "the guests pause with the host");
    }
    jam.host.engine.play();
    in_step(&mut jam, 1_500, 3_000, 2.5, 5.0);
    jam.host.engine.next();
    in_step(&mut jam, 1_500, 3_000, 2.5, 5.0);
    assert!(jam.guests.iter().all(|g| g.rig.engine.status().index == Some(1)), "on b");
}

#[test]
fn a_guest_joins_mid_mix() {
    let (a, b) = (song(30.0, 1, None), song(40.0, 2, None));
    let mut jam = Jam::new(&[("a", &a), ("b", &b)], Some(mix(20_000, 4.0, 1.0, 0.0)), Settings::default(), &two_ways()[..1]);
    jam.host.engine.play_at(0, 18_000);
    jam.run(3_000);
    assert!(jam.host.engine.status().mixing, "the host mixes");
    jam.join(0);
    in_step(&mut jam, 8_000, 8_000, 2.5, 5.0);
}

#[test]
fn a_stalled_guest_catches_up() {
    let a = song(60.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways()[..1]);
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    // Its song's bytes come 3 s late.
    let g = &jam.guests[0].rig;
    g.net.stalled_until.store(g.now_ns() + 3_000_000_000, Ordering::Relaxed);
    jam.join(0);
    jam.run(2_000);
    assert!(jam.guests[0].rig.card.0.lock().heard.is_empty(), "nothing heard while stalled");
    in_step(&mut jam, 2_000, 8_000, 2.5, 5.0);
}

/// How guest `g` said it follows the host, last.
fn followed(jam: &Jam, g: usize) -> Option<Option<Followed>> {
    jam.guests[g].rig.events.lock().iter().rev().find_map(|(_, e)| match e {
        Event::Following(f) => Some(*f),
        _ => None,
    })
}

/// A plain guest's own controls (Spotify's Jam): its skips and seeks do nothing; its pause holds its own
/// listening, silent while the host plays on; play joins the host again where it is then, not where it
/// paused.
#[test]
fn a_guest_paused_here_joins_again_where_the_host_is() {
    let a = song(120.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways()[..1]);
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    in_step(&mut jam, 2_000, 2_000, 2.5, 5.0);
    let g = jam.guests[0].rig.engine.clone();
    g.seek(40_000);
    g.next();
    in_step(&mut jam, 500, 2_000, 2.5, 5.0);

    g.pause();
    jam.run(1_000);
    assert_eq!(g.status().state, nori_engine::State::Paused, "paused here");
    assert_eq!(followed(&jam, 0), Some(Some(Followed { playing: true, held: true })));
    let heard = jam.guests[0].rig.card.0.lock().heard.len();
    jam.run(10_000);
    assert_eq!(jam.guests[0].rig.card.0.lock().heard.len(), heard, "silent here");
    assert_eq!(jam.host.engine.status().state, nori_engine::State::Playing, "the jam plays on");

    g.play();
    jam.run(1_000);
    assert_eq!(followed(&jam, 0), Some(Some(Followed { playing: true, held: false })));
    in_step(&mut jam, 1_000, 4_000, 2.5, 5.0);
}

/// An admin's pause is the host's (its controls reach the jam): every listener pauses with it, the admin
/// too, none of them held; its play starts them all again, in step.
#[test]
fn an_admins_pause_pauses_every_guest() {
    let a = song(120.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways());
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    jam.join(1);
    in_step(&mut jam, 2_000, 2_000, 2.5, 5.0);
    // Guest 0 is the admin: its pause takes its way to the host.
    jam.run(jam.guests[0].word_ms as u64);
    jam.host.engine.pause();
    jam.run(2_000);
    for g in 0..2 {
        assert_eq!(jam.guests[g].rig.engine.status().state, nori_engine::State::Paused, "guest {g} paused");
        assert_eq!(followed(&jam, g), Some(Some(Followed { playing: false, held: false })), "guest {g}: the jam paused, not it");
    }
    jam.run(jam.guests[0].word_ms as u64);
    jam.host.engine.play();
    in_step(&mut jam, 2_000, 4_000, 2.5, 5.0);
}

/// The app in the background: the platform lets the guest's output go (the engine's release), which
/// holds its listening; play opens it again in step. And a host paused past the idle release: the
/// guest's output goes too, and comes back in step when the host plays again.
#[test]
fn a_guests_output_let_go_comes_back_in_step() {
    let a = song(500.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways()[..1]);
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    in_step(&mut jam, 2_000, 2_000, 2.5, 5.0);
    let g = jam.guests[0].rig.engine.clone();
    g.release_now();
    jam.run(2_000);
    assert_eq!(g.status().releases, 1, "let go");
    assert_eq!(followed(&jam, 0), Some(Some(Followed { playing: true, held: true })));
    g.play();
    in_step(&mut jam, 2_000, 4_000, 2.5, 5.0);

    jam.host.engine.pause();
    jam.run(6 * 60_000);
    assert_eq!(g.status().releases, 2, "let go after the idle time");
    jam.host.engine.play();
    // The host's own word on its place, its card opened again, is up to a block of the card's off what
    // the card plays (it starts at its next pull), and the guest plays where the word says.
    in_step(&mut jam, 2_000, 4_000, 3.5, 5.0);
}

/// Leaving the jam (`nori_engine::core::follow` given no lead): silent at once, then the guest's own
/// player again.
#[test]
fn a_guest_that_stops_following_plays_on_its_own_again() {
    let a = song(60.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways()[..1]);
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    jam.run(3_000);
    jam.guests[0].listening = false;
    jam.guests[0].inbox.clear();
    let g = jam.guests[0].rig.engine.clone();
    g.follow(None);
    g.pause();
    jam.run(1_000);
    assert_eq!(g.status().state, nori_engine::State::Paused);
    let heard = jam.guests[0].rig.card.0.lock().heard.len();
    jam.run(1_000);
    assert_eq!(jam.guests[0].rig.card.0.lock().heard.len(), heard, "silent");

    g.play_at(0, 40_000);
    jam.run(2_000);
    assert_eq!(g.status().state, nori_engine::State::Playing);
    let place = jam.guests[0].rig.card.place_at(jam.now_ns(), 41_800.0).expect("music heard");
    assert!((41_000.0..=42_000.0).contains(&place), "plays its own pick, at {place:.0} ms");
}

/// An output that says it holds less than it does at first (an Android track before its first timestamp):
/// what it says steps by its 80 ms delay a while after it starts. The guest waits for it to settle, then
/// slips its way into step instead of starting again.
#[test]
fn a_guest_whose_output_settles_late_slips_rather_than_restarts() {
    let a = song(60.0, 1, None);
    for unsure_ms in [300, 3_000] {
        let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways()[..1]);
        {
            let mut card = jam.guests[0].rig.card.0.lock();
            card.delay_ns = 80_000_000;
            card.unsure_ns = unsure_ms * 1_000_000;
        }
        jam.host.engine.play_at(0, 0);
        jam.run(1_000);
        jam.join(0);
        in_step(&mut jam, 25_000, 6_000, 2.5, 5.0);
        assert_eq!(jam.guests[0].rig.card.0.lock().resumes, 1, "unsure {unsure_ms} ms: started once");
    }
}

/// Guests whose cards play a little fast or slow against their clocks (each its own crystal), for five
/// minutes of steady playback: each starts once, never runs dry, never writes again what its output
/// holds (each time it would, a phone's deep track drops what it holds), and stays within a few ms of the
/// host. One holds ten seconds, as a phone's track does with the screen off, and misreads its delay by
/// 150 ms now and then (an emulator's latency estimate jumping).
#[test]
fn guests_with_drifting_cards_stay_in_step_for_minutes() {
    let a = song(360.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways());
    {
        let mut card = jam.guests[0].rig.card.0.lock();
        (card.ppm, card.holds_ns, card.spike_every_s, card.spike_ns) = (300.0, 10_000_000_000, 23, 150_000_000);
    }
    jam.guests[1].rig.card.0.lock().ppm = -150.0;
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    jam.join(1);
    // Until a correction made after the drift is learned (eight seconds) is heard through the deep card.
    jam.run(30_000);
    // Five minutes, each guest read every other five seconds.
    let mut gaps = [Gaps::default(), Gaps::default()];
    for _ in 0..30 {
        for (g, gaps) in gaps.iter_mut().enumerate() {
            gaps.0.extend(jam.gaps(g, 5_000).0);
        }
    }
    for (g, gaps) in gaps.iter().enumerate() {
        let rig = &jam.guests[g].rig;
        let logs = rig.logs.lock();
        let starts = logs.iter().filter(|l| l.starts_with("following: Start")).count();
        let slips = logs.iter().filter(|l| l.starts_with("following: Slip")).count();
        // Once, at most, as the start settles.
        let rewritten = logs.iter().filter(|l| l.starts_with("the track changes")).count();
        let underruns = rig.engine.status().underruns;
        eprintln!("guest {g}: {gaps}; {starts} starts, {slips} slips, {rewritten} written again, {underruns} underruns");
        assert_eq!((starts, underruns), (1, 0), "guest {g}: started once, never ran dry");
        assert!(rewritten <= 1, "guest {g}: written again {rewritten} times");
        assert!(gaps.within(2_500, 1.5, 3.0), "guest {g}: {gaps}");
    }
}

/// A host's place run on past the end of its song (its word on the next one late): the guest plays on
/// into the next song as the host does, and stays there.
#[test]
fn a_guest_follows_its_host_past_the_end_of_a_song() {
    let (a, b) = (song(10.0, 1, None), song(30.0, 2, None));
    let guest = Rig::new(&[("a", &a), ("b", &b)], None, Settings::default());
    guest.engine.follow(Some(Lead { index: 0, ms: 6_000.0, ago_us: 0, there_us: 0, rate: 1.0, playing: true, speed: 1.0, pitch: 1.0, mix: None }));
    guest.run(10_000);
    let starts = || guest.logs.lock().iter().filter(|l| l.starts_with("following: Start")).count();
    let before = starts();
    for _ in 0..40 {
        guest.run(100);
        assert_eq!(guest.engine.status().index, Some(1), "on b, past a's end");
    }
    assert_eq!(starts(), before, "not started again: {:?}", guest.logs.lock().iter().filter(|l| l.starts_with("following")).collect::<Vec<_>>());
}

/// The same past the end of the host's last song: the guest's queue ends with the host's, and it waits
/// for the host's word rather than starting the end again and again.
#[test]
fn a_guest_ends_with_its_hosts_queue() {
    let (a, b) = (song(10.0, 1, None), song(10.0, 2, None));
    let guest = Rig::new(&[("a", &a), ("b", &b)], None, Settings::default());
    guest.engine.follow(Some(Lead { index: 1, ms: 6_000.0, ago_us: 0, there_us: 0, rate: 1.0, playing: true, speed: 1.0, pitch: 1.0, mix: None }));
    guest.run(20_000);
    let starts = guest.logs.lock().iter().filter(|l| l.starts_with("following: Start")).count();
    assert_eq!(starts, 1, "started once: {:?}", guest.logs.lock().iter().filter(|l| l.starts_with("following")).collect::<Vec<_>>());
    assert_ne!(guest.engine.status().state, nori_engine::State::Playing, "the music is over");
}

#[test]
fn a_pause_cancels_a_guest_starting_a_mix() {
    let (a, b) = (song(30.0, 1, None), song(40.0, 2, None));
    for (local, hold_ms) in [(false, 0), (true, 0), (false, 10_000), (true, 10_000)] {
        let mut jam = Jam::new(&[("a", &a), ("b", &b)], Some(mix(20_000, 4.0, 1.0, 0.0)), Settings::default(), &two_ways()[..1]);
        jam.guests[0].rig.card.0.lock().holds_ns = hold_ms * 1_000_000;
        jam.host.engine.play_at(0, 18_000);
        jam.run(2_500);
        assert!(jam.host.engine.status().mixing);
        jam.join(0);
        jam.run(100);
        if local { jam.guests[0].rig.engine.pause(); } else { jam.host.engine.pause(); }
        jam.run(1_000);
        assert!(jam.guests[0].rig.card.0.lock().heard.is_empty(), "local={local}, hold={hold_ms}: a cancelled start must remain silent: {:?}", jam.guests[0].rig.logs.lock());
        if local { jam.guests[0].rig.engine.play(); } else { jam.host.engine.play(); }
        jam.run(800);
        let samples = jam.guests[0].rig.card.left_before(jam.now_ns(), 441).unwrap_or_else(|| panic!("local={local}, hold={hold_ms}: the resumed guest plays: {:?}", jam.guests[0].rig.logs.lock()));
        assert!(samples.iter().map(|s| s * s).sum::<f32>() > 0.01, "local={local}, hold={hold_ms}: resuming must be audible");
    }
}
