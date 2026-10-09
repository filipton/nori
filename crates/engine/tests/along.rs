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
use nori_engine::{AudioOutput, Body, ByteSource, Config, Engine, Event, Feed, Lead, Library, Located, OpenError, OutputFormat, Settings, SharedQueue, Source};
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
        self.0.lock().playing = false;
    }
    fn resume(&mut self) {
        self.0.lock().playing = true;
    }
    /// The block pulled plays from the pull on: what is left of it.
    fn latency_us(&self) -> u64 {
        let p = self.0.lock();
        let Some(&(at, _)) = p.pulls.last() else { return 0 };
        let block = BLOCK as i64 * 1_000_000_000 / RATE as i64;
        ((at + block - self.1.now_ns()).clamp(0, block) / 1000) as u64
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
    net: Arc<Net>,
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
        let engine = Engine::start_on(library, Planned(app, plan), queue, Box::new(card.clone()), None, config, clock.clone(), move |e| {
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
        Rig { engine, time: Stepper::new(clock, card.0.clone()), card, said, events, net }
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
        Jam { host, host_skew_us: 4_000_000, mix, speed, guests, told: 0, exchanges: 0 }
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
            for k in self.told..said {
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
                    let mix = self.mix.clone().map(|p| ("a".to_string(), p));
                    g.rig.engine.follow(Some(Lead { index: w.index, ms: w.ms, ago_us: here_us - (w.at_us - off), rate: w.rate, playing: w.playing, speed: w.speed, pitch: 1.0, mix }));
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
        // Through the mix and, in b, the tempo's ramp back.
        jam.run(4_000);
        let into_b = |j: &Jam| j.guests.iter().all(|g| g.rig.engine.status().index == Some(1)) && j.host.engine.status().index == Some(1);
        assert!(into_b(&jam), "{what}: all in b");
        let starts = |j: &Jam| j.guests.iter().map(|g| g.rig.events.lock().iter().filter(|(_, e)| matches!(e, Event::Song { index: 1, .. })).count()).collect::<Vec<_>>();
        assert_eq!(starts(&jam), [1, 1], "{what}: each guest went into b once, by its own mix");
        in_step(&mut jam, 3_000, 6_000, 3.0, 5.0);
    }
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

#[test]
fn a_guests_own_controls_do_not_move_it() {
    let a = song(60.0, 1, None);
    let mut jam = Jam::new(&[("a", &a)], None, Settings::default(), &two_ways()[..1]);
    jam.host.engine.play_at(0, 0);
    jam.run(1_000);
    jam.join(0);
    jam.run(2_000);
    let g = &jam.guests[0].rig.engine;
    g.pause();
    g.seek(40_000);
    g.next();
    for _ in 0..40 {
        jam.run(50);
        assert_eq!(jam.guests[0].rig.engine.status().state, nori_engine::State::Playing, "it plays on");
    }
    in_step(&mut jam, 0, 4_000, 2.5, 5.0);
}
