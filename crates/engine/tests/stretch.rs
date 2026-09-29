//! The place said for a song brought in at another tempo: a beat-matched AutoMix plays the incoming song
//! faster (here 7.1 %, 84 to 90 BPM) through the mix and eases it back to its own tempo after, while the
//! output runs at one times. The place the engine says is the song's own time - where in the song's file
//! the music heard is - at every moment through the mix, the ease back and after, and never jumps back.
//! The song is 48 kHz into an output opened at 44.1 kHz, as on the phone the report came from.
//!
//! The truth is read off the sound itself: the incoming song's left channel is a steady tone and its
//! right a tone that rises with the place in the song, so the ratio of the two pitches heard says the
//! place, whatever the tempo, the pitch kept or not, and the output's rate.

use crate::common;

use std::path::PathBuf;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use std::sync::Arc;

use nori_engine::{Config, Engine, Event, Library, Located, Settings, SharedQueue, Source};
use parking_lot::Mutex;
use nori_player::automix::analysis::Analyzer;
use nori_player::automix::plan;
use nori_player::engine::{Host, Plan};
use nori_player::pipeline::App;
use nori_player::sim;
use nori_player::transitions::WindowSong;
use nori_player::types::AutoMixSettings;

/// The steady tone, Hz.
const STEADY: f64 = 1000.0;
/// The rising tone: `BASE + SLOPE * s` Hz at `s` seconds into the song.
const BASE: f64 = 1500.0;
const SLOPE: f64 = 60.0;

const A_SECS: f64 = 50.0;
const B_SECS: f64 = 80.0;
/// Where the mix begins in `a`, how long it is, where it enters `b`, and the tempo ease after it.
const MIX_AT_MS: i64 = 20_000;
const MIX_MS: i64 = 22_012;
const SKIP_MS: i64 = 4_000;
const RAMP_MS: i64 = 5_000;
const TEMPO: f32 = 1.071;

/// `secs` of silence at `rate`: the outgoing song, so only the incoming one is measured.
fn silence(rate: u32, secs: f64) -> Vec<u8> {
    common::wav(rate, &vec![0; (rate as f64 * secs) as usize * 2])
}

/// `secs` of two tones at `rate`: a steady one left, a rising one right.
fn marked(rate: u32, secs: f64) -> Vec<u8> {
    let frames = (rate as f64 * secs) as usize;
    let samples: Vec<i16> = (0..frames)
        .flat_map(|k| {
            let t = k as f64 / rate as f64;
            let l = (t * STEADY * std::f64::consts::TAU).sin();
            // The phase of a tone whose pitch is BASE + SLOPE * t.
            let r = ((BASE * t + SLOPE * t * t / 2.0) * std::f64::consts::TAU).sin();
            [(l * 8000.0).round() as i16, (r * 8000.0).round() as i16]
        })
        .collect();
    common::wav(rate, &samples)
}

/// The songs as files: `a`, silent, at 44.1 kHz (it opens the output), `b`, marked, at 48 kHz.
struct Songs(Vec<(String, PathBuf, i64)>, #[allow(dead_code)] nori_testdir::TempDir);

impl Songs {
    fn new() -> Songs {
        let dir = nori_testdir::TempDir::new("stretch");
        let songs = [("a", silence(44_100, A_SECS), A_SECS), ("b", marked(48_000, B_SECS), B_SECS)];
        Songs(
            songs
                .into_iter()
                .map(|(id, bytes, secs)| {
                    let path = dir.join(format!("{id}.wav"));
                    std::fs::write(&path, bytes).unwrap();
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

/// The report's mix: beat-matched out of `a` into `b`, `b` played 7.1 % fast with its pitch kept, and
/// eased back over five seconds after.
fn beat_matched() -> Plan {
    let s = AutoMixSettings { max_transition_s: 23.0, ..Default::default() };
    let t = plan::plan(None, None, (A_SECS * 1000.0) as i64, (B_SECS * 1000.0) as i64, &s);
    Plan {
        incoming_id: "b".into(),
        out_start_us: MIX_AT_MS * 1000,
        duration_us: MIX_MS * 1000,
        in_skip_us: SKIP_MS * 1000,
        mixer: t.clone(),
        tempo_ratio: TEMPO,
        keep_pitch: true,
        ramp_us: RAMP_MS * 1000,
        out_loop_us: 0,
    }
}

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
    events: Arc<Mutex<Vec<Event>>>,
    time: Stepper<Pull>,
    card: Card,
}

impl Rig {
    /// Plays `a` then `b` from `from_ms` into `a`, at `speed`.
    fn start(from_ms: i64, speed: f32) -> Rig {
        let queue = SharedQueue::default();
        queue.0.lock().set(vec!["a".into(), "b".into()], Some(0), false, 0);
        let card = Card::new();
        let clock = Virtual::default();
        let mut app = sim::App::new();
        app.prefs = sim::prefs_off();
        // The pitch goes with the speed: played faster as a record is, every period of the tones stays
        // whole, where a speed stage keeping the pitch splices periods out and the tones heard cannot be
        // measured to a few milliseconds. What is counted as played is the same either way.
        let settings = Settings { speed, pitch: speed, ..Settings::default() };
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let told = events.clone();
        let engine = Engine::start_on(Songs::new(), Planned(app), queue, Box::new(card.clone()), None, Config { settings, ..Config::default() }, clock.clone(), move |e| told.lock().push(e));
        engine.queue_changed();
        engine.play_at(0, from_ms);
        Rig { engine, events, time: Stepper::new(clock, card.pull.clone()), card }
    }

    fn run(&self, ms: u64) {
        self.time.run(Duration::from_millis(ms));
    }

    /// The engine's own place now, read from its output afresh: the song heard, the place in it, whether
    /// a mix is heard, and the pace the place moves at.
    fn said(&self) -> (Option<usize>, i64, bool, f32) {
        self.engine.look();
        self.time.clock.settle();
        let s = self.engine.status();
        (s.index, s.position_ms, s.mixing, s.pace)
    }

    /// Where in `b` the music heard now is, ms, from the two tones; `None` while `b` is not heard, and
    /// where a window heard does not say it plainly (two sounds of `b` crossfading into each other, as
    /// the stretch hands over to the song as it is).
    fn truth(&self) -> Option<f64> {
        let f = self.card.format()?;
        let heard = self.card.heard.lock();
        let ch = f.channels;
        const W: usize = 2048;
        let frames = heard.len() / ch;
        if frames < 2 * W {
            return None;
        }
        // The place at the middle of a window of W frames ending `back` windows before the last.
        let at = |back: usize| -> Option<f64> {
            let end = frames - back * W;
            let w = &heard[(end - W) * ch..end * ch];
            let hz = |c: usize| -> Option<f64> {
                let x: Vec<f64> = w.chunks_exact(ch).map(|s| s[c] as f64).collect();
                let peak = x.iter().fold(0.0f64, |m, v| m.max(v.abs()));
                if peak < 1e-4 {
                    return None;
                }
                let ups: Vec<f64> = x.windows(2).enumerate().filter(|(_, p)| p[0] < 0.0 && p[1] >= 0.0).map(|(i, p)| i as f64 + p[0] / (p[0] - p[1])).collect();
                if ups.len() < 9 {
                    return None;
                }
                // The middle period of eight in a row, averaged: a speed stage splices the song where it
                // takes a period out or puts one in, and the one period across the splice is off.
                let mut runs: Vec<f64> = ups.windows(9).map(|w| (w[8] - w[0]) / 8.0).collect();
                runs.sort_by(f64::total_cmp);
                Some(1.0 / runs[runs.len() / 2])
            };
            let ratio = hz(1)? / hz(0)?;
            Some((ratio * STEADY - BASE) / SLOPE * 1000.0)
        };
        let (before, last) = (at(1)?, at(0)?);
        // A window apart in the output's time: the song moves on by that times its pace, which is between
        // one and the tempo times the speed. Anything else is a window that did not measure cleanly.
        let apart = W as f64 / f.rate as f64 * 1000.0;
        let moved = last - before;
        if !(moved > apart * 0.8 && moved < apart * 1.6) {
            return None;
        }
        // The end of the last window is heard now: half a window on from its middle, at the pace measured.
        Some(last + moved / 2.0)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

/// Plays through the mix and on for a while after, reading the engine's place every 100 ms of the
/// card's time, and checks it against the place heard: within 50 ms of it at every reading, never back
/// by more than a hair, and moving at the pace of the song heard. `seek_to`: a seek, a second in, to
/// that place in `a` (into the mix: the hold then begins late, as the report's did).
fn walk(from_ms: i64, speed: f32, seek_to: Option<i64>) {
    let rig = Rig::start(from_ms, speed);
    if let Some(ms) = seek_to {
        rig.run(1_000);
        rig.engine.go_to(0, ms);
    }
    let mut last: Option<i64> = None;
    let mut worst = 0.0f64;
    let mut measured = 0;
    let (mut paced, mut tempo_paced) = (0, 0);
    let mut log = Vec::new();
    let mut fails: Vec<String> = Vec::new();
    for _ in 0..(40_000 / 100) {
        rig.run(100);
        let (index, ms, mixing, pace) = rig.said();
        let truth = rig.truth();
        log.push(format!("{:.2} s: song {index:?} at {ms} ms, pace {pace:.3}{}, heard at {}", rig.card.secs(), if mixing { ", mixing" } else { "" }, truth.map_or("-".into(), |t| format!("{t:.0} ms"))));
        if index != Some(1) {
            continue;
        }
        if let Some(prev) = last {
            if ms < prev - 20 {
                fails.push(format!("the place went back from {prev} to {ms} ms"));
            }
        }
        last = Some(ms);
        if let Some(t) = truth {
            measured += 1;
            let off = ms as f64 - t;
            worst = worst.max(off.abs());
            if off.abs() > 50.0 {
                fails.push(format!("{ms} ms said, {t:.0} ms heard"));
            }
        }
        // The pace a screen runs the place on at between readings: the tempo times the speed while the
        // song is heard stretched, the speed once it is its own again.
        let tempo = pace as f64 / speed as f64;
        if (tempo - TEMPO as f64).abs() < 0.005 {
            tempo_paced += 1;
        }
        if (tempo - 1.0).abs() < 0.005 {
            paced += 1;
        }
    }
    // Made ahead as the output needs it through the stretch and the conversion: never short.
    let (underruns, dry) = (rig.engine.status().underruns, rig.card.pull.lock().dry);
    assert!(underruns == 0 && dry == 0, "the output ran short: {underruns} underruns, {dry} pulls found the ring short");
    let log = log.join("\n");
    assert!(measured > 100, "b was heard and measured: {measured} times\n{log}");
    assert!(fails.is_empty(), "worst {worst:.0} ms off, {} wrong: {}\n{log}", fails.len(), fails.join("; "));
    assert!(tempo_paced >= 30 && paced >= 30, "the pace said follows the song's: {tempo_paced} readings at the mix's tempo, {paced} at its own\n{log}");
    // A client that runs the place on at the speed alone is told it again once the song is its own.
    let events = rig.events.lock();
    assert!(events.iter().any(|e| matches!(e, Event::Placed { index: 1, .. })), "the place said again after the stretch: {events:?}");
}

#[test]
fn place_through_stretched_mix() {
    walk(MIX_AT_MS - 5_000, 1.0, None);
}

#[test]
fn place_after_seek_into_stretched_mix() {
    // The report's: a seek 9.3 s into a 22 s mix, the hold begun late with no sound left in the output.
    walk(MIX_AT_MS - 5_000, 1.0, Some(MIX_AT_MS + 9_345));
}

#[test]
fn place_through_stretched_mix_at_speed() {
    walk(MIX_AT_MS - 5_000, 1.25, None);
}

/// The lyrics page through the mix, read as a phone's screen reads it: every display frame, the engine's
/// last reading run on at its pace (`nori_player::heard::screen_place`), a fresh one asked for once that is
/// a second old, and the bar's place (`Playhead`) handed to the lyrics' clock. Two seconds into the mix,
/// while the ending is still the louder, the output's clock is read 60 ms back for a moment, as a phone's
/// is now and then when its output corrects itself. That was the report's 22 s mix: the page went over to
/// the incoming song at the wobble, long before it was the louder, its place stood still where the ear was
/// to land in it, and the screen's place ran on from there and was pulled back to it every few seconds -
/// the first line filling in word by word, going back and filling again, until the song got there.
#[test]
fn lagging_clock_does_not_step_back() {
    use nori_look::lyrics::{Line, LyricClock, LyricTiming, Word};
    use nori_player::heard::{screen_place, HeardTracker, Playhead, Seen};

    let rig = Rig::start(MIX_AT_MS - 5_000, 1.0);
    let now = || rig.time.clock.now_ns() / 1_000_000;
    // The ear takes `b` over near 10.7 s into it; its first line comes a little over a second later, four
    // words over two seconds, and the next four seconds after that.
    let line = |start_ms: i64| Line {
        start_ms,
        end_ms: start_ms + 2_000,
        len: 19,
        words: (0..4u32).map(|k| Word { start_ms: start_ms + k as i64 * 500, end_ms: start_ms + (k as i64 + 1) * 500, start: k * 5, end: k * 5 + 4 }).collect(),
        ..Line::default()
    };
    let lyrics = LyricClock::new(LyricTiming::new(true, true, [line(12_000), line(16_000)]), 0);
    let (tracker, mut head) = (HeardTracker::new(), Playhead::new());
    // The engine's last reading: when it was taken on the test's clock, the song, the place and its pace.
    let mut last_at = None;
    let (mut read_at, mut index, mut reading, mut pace) = (0i64, None, 0i64, 1.0f32);
    let mut placed: Option<i64> = None;
    let mut shown: Option<(i32, f32)> = None;
    let (mut fills, mut fails, mut log) = (0, Vec::new(), Vec::new());
    let mix_at = now() + 5_000;
    while now() < mix_at + 25_000 {
        // Before the takeover, and again after it with the mix still heard.
        let wobble = [2_000, 9_000].iter().any(|&at| (mix_at + at..mix_at + at + 200).contains(&now()));
        rig.card.pull.lock().latency_us = if wobble { 60_000 } else { 0 };
        rig.run(16);
        let t = now();
        if t - read_at >= 1_000 || wobble {
            rig.engine.look();
            rig.time.clock.settle();
        }
        let s = rig.engine.status();
        if Some(s.at) != last_at {
            last_at = Some(s.at);
            (read_at, index, reading, pace) = (t, s.index, s.position_ms, s.pace);
            if index == Some(1) {
                if let Some(truth) = rig.truth() {
                    if (reading as f64 - truth).abs() > 100.0 {
                        fails.push(format!("{t} ms: {reading} ms said in b, {truth:.0} ms heard"));
                    }
                }
            }
        }
        let (place, _) = screen_place(reading, t - read_at, pace, true);
        let place = head.show_for(&tracker, Seen { index: None, ms: place, changed: false }, index, t, index, place, true);
        if index != Some(1) {
            continue;
        }
        if let Some(before) = placed.filter(|&before| place < before) {
            fails.push(format!("{t} ms: the place in b went back from {before} to {place} ms"));
        }
        placed = Some(place);
        let f = lyrics.advance(place, true, true, false).frame;
        log.push(format!("{t} ms: b at {place} ms (said {reading}), line {} sung {:.2}", f.active, f.sung));
        if let Some((line, sung)) = shown {
            if f.active < line || (f.active == line && f.sung < sung - 1e-3) {
                fails.push(format!("{t} ms: the lyrics went back from line {line} sung {sung:.2} to line {} sung {:.2}", f.active, f.sung));
            }
        }
        if f.active == 0 && f.sung > 0.0 && shown.is_none_or(|(line, sung)| line != 0 || sung == 0.0) {
            fills += 1;
        }
        shown = Some((f.active, f.sung));
    }
    let log = log.join("\n");
    assert!(shown.is_some_and(|(line, _)| line >= 1), "b's lyrics were followed to their second line\n{log}");
    assert!(fails.is_empty(), "{}\n{log}", fails.join("\n"));
    assert_eq!(fills, 1, "the first line filled in once\n{log}");
}
