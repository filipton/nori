//! Signals, measurements and set-ups the pipeline tests share.

use std::f64::consts::TAU;
use std::sync::{Arc, Mutex};

use nori_player::automix::mixer::Mixer;
use nori_player::automix::plan;
use nori_player::automix::synth::Rng;
use nori_player::sim::{prefs_off, Audio, Track};
use nori_player::transitions::TransitionPrefs;
use nori_player::types::{AutoMixSettings, TransitionPlan};

pub const RATE: u32 = 44_100;

pub fn testdata(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Seconds to frames at [`RATE`].
pub fn frames(secs: f64) -> usize {
    (secs * RATE as f64).round() as usize
}

/// A stereo sine, the right channel a quarter turn behind the left so the two sides differ.
pub fn sine(hz: f64, amp: f64, secs: f64) -> Vec<i16> {
    (0..frames(secs))
        .flat_map(|i| {
            let t = i as f64 / RATE as f64;
            [(amp * (TAU * hz * t).sin() * 32767.0).round() as i16, (amp * (TAU * hz * t - TAU / 4.0).sin() * 32767.0).round() as i16]
        })
        .collect()
}

/// A stereo sine with both sides the same: a centred voice.
pub fn centred(hz: f64, amp: f64, secs: f64) -> Vec<i16> {
    (0..frames(secs)).flat_map(|i| [((amp * (TAU * hz * i as f64 / RATE as f64).sin()) * 32767.0).round() as i16; 2]).collect()
}

/// Stereo white noise at `amp` of full scale (uniform), the same every time for the same seed.
pub fn noise(amp: f64, secs: f64, seed: u64) -> Vec<i16> {
    let mut r = Rng(seed);
    (0..frames(secs) * 2).map(|_| (r.next() * amp * 32767.0).round() as i16).collect()
}

/// A sine by rotation (`sin` per sample is slow in debug builds).
struct Osc {
    c: f64,
    s: f64,
    dc: f64,
    ds: f64,
}

impl Osc {
    fn new(hz: f64) -> Osc {
        let w = TAU * hz / RATE as f64;
        Osc { c: 1.0, s: 0.0, dc: w.cos(), ds: w.sin() }
    }

    fn next(&mut self) -> f64 {
        let s = self.s;
        (self.c, self.s) = (self.c * self.dc - self.s * self.ds, self.s * self.dc + self.c * self.ds);
        s
    }
}

/// `make()`'s signal, generated once per `key` for the whole test binary (a process-wide cache: tests
/// run on separate threads with nothing else shared).
pub fn cached(key: (u8, u64, u64, u64), make: impl FnOnce() -> Vec<i16>) -> Vec<i16> {
    type Made = Vec<((u8, u64, u64, u64), Arc<Vec<i16>>)>;
    static MADE: Mutex<Made> = Mutex::new(Vec::new());
    let made = MADE.lock().unwrap().iter().find(|(k, _)| *k == key).map(|(_, m)| m.clone());
    let m = made.unwrap_or_else(|| {
        let m = Arc::new(make());
        MADE.lock().unwrap().push((key, m.clone()));
        m
    });
    m.to_vec()
}

/// Music-like stereo: partials, a slow swell and a little noise, different per seed.
pub fn music(secs: f64, seed: u64) -> Vec<i16> {
    cached((0, secs.to_bits(), seed, 0), || make_music(secs, seed))
}

fn make_music(secs: f64, seed: u64) -> Vec<i16> {
    let mut r = Rng(seed);
    let detune = 1.0 + (seed % 7) as f64 * 0.013;
    let mut o = [110.0, 330.0, 1250.0, 523.0, 0.25].map(|hz| Osc::new(hz * detune));
    let mut out = Vec::with_capacity(frames(secs) * 2);
    for _ in 0..frames(secs) {
        let [a, b, c, d, e] = [0, 1, 2, 3, 4].map(|k| o[k].next());
        let swell = 0.6 + 0.4 * e;
        let base = 0.25 * a + 0.12 * b + 0.06 * c;
        let l = swell * base + 0.02 * r.next();
        let rr = swell * (base * 0.8 + 0.1 * d) + 0.02 * r.next();
        out.push((l * 32767.0).round() as i16);
        out.push((rr * 32767.0).round() as i16);
    }
    out
}

pub fn track(id: &str, samples: &[i16]) -> Track {
    Track::new(id, Audio::pcm(RATE, 2, samples))
}

/// A plain crossfade of `secs`, AutoMix off.
pub fn crossfade(secs: i32) -> TransitionPrefs {
    TransitionPrefs { crossfade_s: secs, keep_albums: true, ..prefs_off() }
}

pub fn left(s: &[i16]) -> Vec<f64> {
    s.iter().step_by(2).map(|&v| v as f64 / 32768.0).collect()
}

pub fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

pub fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

/// The largest step from one sample to the next.
pub fn max_step(x: &[f64]) -> f64 {
    x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max)
}

/// The level of `hz` in `x`, as the amplitude of a sine (Goertzel over the whole slice, Hann-windowed).
pub fn level_at(x: &[f64], hz: f64, rate: f64) -> f64 {
    let n = x.len();
    let w = TAU * hz / rate;
    let (mut s1, mut s2) = (0.0, 0.0);
    let mut win_sum = 0.0;
    for (i, v) in x.iter().enumerate() {
        let h = 0.5 - 0.5 * (TAU * i as f64 / n as f64).cos();
        win_sum += h;
        let s = v * h + 2.0 * w.cos() * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    let power = s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2;
    2.0 * power.sqrt() / win_sum
}

/// Zero crossings per second, upwards: a pure tone's frequency.
pub fn pitch_hz(x: &[f64], rate: f64) -> f64 {
    let ups: Vec<usize> = x.windows(2).enumerate().filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0).map(|(i, _)| i).collect();
    if ups.len() < 2 {
        return 0.0;
    }
    (ups.len() - 1) as f64 * rate / (ups[ups.len() - 1] - ups[0]) as f64
}

/// FNV-1a over the samples: a fingerprint of exactly what was heard.
pub fn fingerprint(s: &[i16]) -> u64 {
    s.iter().flat_map(|v| v.to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// What the planner makes of two songs nobody has measured, under a crossfade of `secs`.
pub fn blind_plan(out_ms: i64, in_ms: i64, secs: f32) -> TransitionPlan {
    let s = AutoMixSettings { max_transition_s: secs, beat_match: false, bass_swap: false, filter_effects: false, echo_out: false, ..Default::default() };
    plan::plan(None, None, out_ms, in_ms, &s)
}

/// `out` and `inc` mixed as the plan says, by the mixer alone: what the engine must have produced.
pub fn reference_mix(out: &[i16], inc: &[i16], p: &TransitionPlan) -> Vec<i16> {
    let mut m = Mixer::new(RATE, 2);
    m.configure(p);
    let mut dst = out[..out.len().min(inc.len())].to_vec();
    m.process(&mut dst, inc);
    dst
}
