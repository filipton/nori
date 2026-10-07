//! The sample-domain chain, run on the playback thread for every buffer without allocating.
//!
//! Order: pre-amp, equalizer, bass boost, loudness (tone first, so later level decisions see it);
//! expander, compressor; mono (before virtualizer and crossfeed, which model one speaker pair);
//! virtualizer, crossfeed; balance (after crossfeed, which would otherwise leak the louder side back);
//! volume boost; limiter last, so it sees every boost.

use crate::compressor::{Compressor, CompressorSettings, Expander, ExpanderSettings};
use crate::dither::Dither;
use crate::spatial::Virtualizer;
use crate::types::{EqBand, EqKind, NamedPreset, PresetKind};

pub const PEAKING: i32 = 0;
pub const LOW_SHELF: i32 = 1;
pub const HIGH_SHELF: i32 = 2;
pub const LOW_PASS: i32 = 3;
pub const HIGH_PASS: i32 = 4;
pub const BAND_PASS: i32 = 5;
pub const NOTCH: i32 = 6;
pub const ALL_PASS: i32 = 7;
/// Shelves whose `q` is the RBJ slope S (1: steepest without ripple).
pub const LOW_SHELF_SLOPE: i32 = 8;
pub const HIGH_SHELF_SLOPE: i32 = 9;

pub const CH_BOTH: i32 = 0;
pub const CH_LEFT: i32 = 1;
pub const CH_RIGHT: i32 = 2;

const MAX_CHANNELS: usize = 8;
/// Most stereo filters run in one pass over a block ([`Biquad::run2_chain`]).
const CHAIN: usize = 4;
/// Attenuation of the quiet side at balance ±1 (then muted); linear in dB in between.
const BALANCE_RANGE_DB: f64 = 24.0;
/// Mono sum gain (-3 dB): uncorrelated material keeps its level; centred gains 3 dB for the limiter.
const MONO_SUM: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// Bass boost low shelf: half-gain point and gentle slope (lifts bass, leaves voices).
const BASS_HZ: f64 = 100.0;
const BASS_SLOPE: f64 = 0.8;
pub const BASS_BOOST_MAX_DB: f64 = 12.0;
pub const VOLUME_BOOST_MAX_DB: f64 = 12.0;
/// Limiter soft knee width, centred on the threshold; below it the limiter is bit-exact.
const KNEE_DB: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub kind: i32,
    pub freq: f64,
    pub gain_db: f64,
    /// Q, or the slope S for the `*_SHELF_SLOPE` kinds.
    pub q: f64,
    /// `CH_BOTH`, `CH_LEFT` or `CH_RIGHT`.
    pub channel: i32,
}

/// `v` unless NaN or infinite (which would poison filter state).
#[inline]
fn finite(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    /// Channel bitmask this filter applies to.
    chans: u8,
}

impl Biquad {
    /// One transposed direct form II step with state `s`.
    #[inline(always)]
    fn tick(&self, s: &mut [f64; 2], x: f64) -> f64 {
        let y = self.b0 * x + s[0];
        s[0] = self.b1 * x - self.a1 * y + s[1];
        s[1] = self.b2 * x - self.a2 * y;
        y
    }

    /// [`Biquad::tick`] over a channel's samples in place, the state kept in registers.
    #[inline]
    fn run(&self, s: &mut [f64; 2], x: &mut [f64]) {
        let (b0, b1, b2, a1, a2) = (self.b0, self.b1, self.b2, self.a1, self.a2);
        let [mut s0, mut s1] = *s;
        for v in x.iter_mut() {
            let i = *v;
            let y = b0 * i + s0;
            s0 = b1 * i - a1 * y + s1;
            s1 = b2 * i - a2 * y;
            *v = y;
        }
        *s = [s0, s1];
    }

    /// [`Biquad::run`] over two channels at once: two chains that do not wait on each other.
    #[inline]
    fn run2(&self, s: &mut [[f64; 2]], left: &mut [f64], right: &mut [f64]) {
        let (b0, b1, b2, a1, a2) = (self.b0, self.b1, self.b2, self.a1, self.a2);
        let ([mut l0, mut l1], [mut r0, mut r1]) = (s[0], s[1]);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (i, j) = (*l, *r);
            let y = b0 * i + l0;
            let z = b0 * j + r0;
            l0 = b1 * i - a1 * y + l1;
            r0 = b1 * j - a1 * z + r1;
            l1 = b2 * i - a2 * y;
            r1 = b2 * j - a2 * z;
            *l = y;
            *r = z;
        }
        (s[0], s[1]) = ([l0, l1], [r0, r1]);
    }

    /// The first `N` filters of `fs` in series over both channels in one pass: each sample's arithmetic
    /// is [`Biquad::run2`]'s, filter after filter, but the filters' chains overlap.
    #[cfg(not(target_arch = "aarch64"))]
    #[inline(always)]
    fn run2_chain<const N: usize>(fs: &[Biquad], st: &mut [[[f64; 2]; MAX_CHANNELS]], left: &mut [f64], right: &mut [f64]) {
        let fs: [Biquad; N] = fs[..N].try_into().expect("N filters");
        let mut s: [[[f64; 2]; 2]; N] = std::array::from_fn(|k| [st[k][0], st[k][1]]);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut i, mut j) = (*l, *r);
            for (f, s) in fs.iter().zip(s.iter_mut()) {
                let y = f.b0 * i + s[0][0];
                let z = f.b0 * j + s[1][0];
                s[0][0] = f.b1 * i - f.a1 * y + s[0][1];
                s[1][0] = f.b1 * j - f.a1 * z + s[1][1];
                s[0][1] = f.b2 * i - f.a2 * y;
                s[1][1] = f.b2 * j - f.a2 * z;
                (i, j) = (y, z);
            }
            (*l, *r) = (i, j);
        }
        for (s, st) in s.iter().zip(st.iter_mut()) {
            (st[0], st[1]) = (s[0], s[1]);
        }
    }

    /// [`Biquad::run2_chain`] with left and right as the two lanes of one register. Separate multiplies
    /// and adds (never fused), so the result is the scalar code's to the bit.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn run2_chain<const N: usize>(fs: &[Biquad], st: &mut [[[f64; 2]; MAX_CHANNELS]], left: &mut [f64], right: &mut [f64]) {
        use std::arch::aarch64::*;
        // SAFETY: NEON is part of every aarch64 target; the intrinsics only do lane arithmetic.
        unsafe {
            let pair = |l: f64, r: f64| vcombine_f64(vdup_n_f64(l), vdup_n_f64(r));
            let c: [[float64x2_t; 5]; N] = std::array::from_fn(|k| [fs[k].b0, fs[k].b1, fs[k].b2, fs[k].a1, fs[k].a2].map(|v| vdupq_n_f64(v)));
            let mut s0: [float64x2_t; N] = std::array::from_fn(|k| pair(st[k][0][0], st[k][1][0]));
            let mut s1: [float64x2_t; N] = std::array::from_fn(|k| pair(st[k][0][1], st[k][1][1]));
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let mut x = pair(*l, *r);
                for k in 0..N {
                    let [b0, b1, b2, a1, a2] = c[k];
                    let y = vaddq_f64(vmulq_f64(b0, x), s0[k]);
                    s0[k] = vaddq_f64(vsubq_f64(vmulq_f64(b1, x), vmulq_f64(a1, y)), s1[k]);
                    s1[k] = vsubq_f64(vmulq_f64(b2, x), vmulq_f64(a2, y));
                    x = y;
                }
                (*l, *r) = (vgetq_lane_f64::<0>(x), vgetq_lane_f64::<1>(x));
            }
            for k in 0..N {
                st[k][0] = [vgetq_lane_f64::<0>(s0[k]), vgetq_lane_f64::<0>(s1[k])];
                st[k][1] = [vgetq_lane_f64::<1>(s0[k]), vgetq_lane_f64::<1>(s1[k])];
            }
        }
    }

    /// RBJ cookbook filters.
    fn new(rate: f64, band: &Band) -> Self {
        let a = 10f64.powf(band.gain_db / 40.0);
        let w = 2.0 * std::f64::consts::PI * band.freq / rate;
        let (sin, cos) = (w.sin(), w.cos());
        let q = band.q.clamp(0.05, 40.0);
        let alpha = sin / (2.0 * q);
        let slope_alpha = || {
            let s = band.q.clamp(0.05, 1.0);
            sin / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt()
        };
        let low_shelf = |k: f64| {
            (
                a * ((a + 1.0) - (a - 1.0) * cos + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - k),
                (a + 1.0) + (a - 1.0) * cos + k,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - k,
            )
        };
        let high_shelf = |k: f64| {
            (
                a * ((a + 1.0) + (a - 1.0) * cos + k),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - k),
                (a + 1.0) - (a - 1.0) * cos + k,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - k,
            )
        };
        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            LOW_SHELF => low_shelf(2.0 * a.sqrt() * alpha),
            HIGH_SHELF => high_shelf(2.0 * a.sqrt() * alpha),
            LOW_SHELF_SLOPE => low_shelf(2.0 * a.sqrt() * slope_alpha()),
            HIGH_SHELF_SLOPE => high_shelf(2.0 * a.sqrt() * slope_alpha()),
            LOW_PASS => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            HIGH_PASS => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            // Constant 0 dB peak gain.
            BAND_PASS => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            NOTCH => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            ALL_PASS => (1.0 - alpha, -2.0 * cos, 1.0 + alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            _ => (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a),
        };
        let chans = match band.channel {
            CH_LEFT => 0b0000_0001,
            CH_RIGHT => 0b0000_0010,
            _ => u8::MAX,
        };
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, chans }
    }
}

/// The normalised `[b0, b1, b2, a1, a2]` the chain uses for `band`, for filter design code.
pub fn band_coefficients(rate: f64, band: &Band) -> [f64; 5] {
    let b = Biquad::new(rate, band);
    [b.b0, b.b1, b.b2, b.a1, b.a2]
}

/// The dB gain of `band`'s biquad at `freq` and `rate`.
pub fn band_db(rate: f64, band: &Band, freq: f64) -> f64 {
    let c = band_coefficients(rate, band);
    let w = std::f64::consts::TAU * freq / rate;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let (nr, ni) = (c[0] + c[1] * c1 + c[2] * c2, -(c[1] * s1 + c[2] * s2));
    let (dr, di) = (1.0 + c[3] * c1 + c[4] * c2, -(c[3] * s1 + c[4] * s2));
    10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).max(1e-30).log10()
}

/// Whether `gain_db` matters for this kind.
pub fn uses_gain(kind: i32) -> bool {
    matches!(kind, PEAKING | LOW_SHELF | HIGH_SHELF | LOW_SHELF_SLOPE | HIGH_SHELF_SLOPE)
}

/// bs2b's cutoff and level limits.
pub const CROSSFEED_CUT_HZ: (f64, f64) = (300.0, 2000.0);
pub const CROSSFEED_DB: (f64, f64) = (1.0, 15.0);
/// bs2b's default cutoff.
pub const CROSSFEED_DEFAULT_HZ: f64 = 700.0;

/// bs2b's standard settings (`BS2B_DEFAULT_CLEVEL`, `BS2B_CMOY_CLEVEL`, `BS2B_JMEIER_CLEVEL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrossfeedPreset {
    /// 700 Hz, 4.5 dB.
    Default,
    /// 700 Hz, 6 dB.
    ChuMoy,
    /// 650 Hz, 9.5 dB.
    JanMeier,
}

impl CrossfeedPreset {
    pub const ALL: [CrossfeedPreset; 3] = [CrossfeedPreset::Default, CrossfeedPreset::ChuMoy, CrossfeedPreset::JanMeier];

    /// (cutoff Hz, level dB).
    pub fn settings(self) -> (f64, f64) {
        match self {
            CrossfeedPreset::Default => (700.0, 4.5),
            CrossfeedPreset::ChuMoy => (700.0, 6.0),
            CrossfeedPreset::JanMeier => (650.0, 9.5),
        }
    }

    /// The preset matching these settings (within 1 Hz and 0.05 dB).
    pub fn of(cut_hz: f64, level_db: f64) -> Option<CrossfeedPreset> {
        CrossfeedPreset::ALL.into_iter().find(|p| {
            let (c, l) = p.settings();
            (c - cut_hz).abs() < 1.0 && (l - level_db).abs() < 0.05
        })
    }
}

/// bs2b headphone crossfeed (Boris Mikhaylov): each ear gets the other channel low-passed at `cut_hz`,
/// `level_db` down in the bass, with a compensating high shelf on the direct path. Stereo only.
#[derive(Clone, Copy, Default)]
struct Crossfeed {
    a0_lo: f64,
    b1_lo: f64,
    a0_hi: f64,
    a1_hi: f64,
    b1_hi: f64,
    gain: f64,
    /// Kept so a new cutoff can reuse the level.
    level_db: f64,
    lo: [f64; 2],
    hi: [f64; 2],
    last: [f64; 2],
}

impl Crossfeed {
    fn new(rate: f64, level_db: f64, cut_hz: f64) -> Self {
        let level_db = finite(level_db, 4.5).clamp(CROSSFEED_DB.0, CROSSFEED_DB.1);
        let cut_hz = finite(cut_hz, CROSSFEED_DEFAULT_HZ).clamp(CROSSFEED_CUT_HZ.0, CROSSFEED_CUT_HZ.1).min(rate * 0.45);
        let gb_lo = level_db * -5.0 / 6.0 - 3.0;
        let gb_hi = level_db / 6.0 - 3.0;
        let g_lo = 10f64.powf(gb_lo / 20.0);
        let g_hi = 1.0 - 10f64.powf(gb_hi / 20.0);
        let cut_hi = cut_hz * 2f64.powf((gb_lo - 20.0 * g_hi.log10()) / 12.0);
        let x_lo = (-2.0 * std::f64::consts::PI * cut_hz / rate).exp();
        let x_hi = (-2.0 * std::f64::consts::PI * cut_hi / rate).exp();
        Crossfeed {
            a0_lo: g_lo * (1.0 - x_lo),
            b1_lo: x_lo,
            a0_hi: 1.0 - g_hi * (1.0 - x_hi),
            a1_hi: -x_hi,
            b1_hi: x_hi,
            gain: 1.0 / (1.0 - g_hi + g_lo),
            level_db,
            ..Default::default()
        }
    }

    /// Same coefficients (state ignored).
    fn same(&self, o: &Crossfeed) -> bool {
        (self.a0_lo, self.b1_lo, self.a0_hi, self.a1_hi, self.b1_hi, self.gain) == (o.a0_lo, o.b1_lo, o.a0_hi, o.a1_hi, o.b1_hi, o.gain)
    }

    #[inline]
    fn frame(&mut self, l: f64, r: f64) -> (f64, f64) {
        let x = [l, r];
        for c in 0..2 {
            self.lo[c] = self.a0_lo * x[c] + self.b1_lo * self.lo[c];
            self.hi[c] = self.a0_hi * x[c] + self.a1_hi * self.last[c] + self.b1_hi * self.hi[c];
            self.last[c] = x[c];
        }
        ((self.hi[0] + self.lo[1]) * self.gain, (self.hi[1] + self.lo[0]) * self.gain)
    }
}

/// Look-ahead peak limiter: the gain follows the incoming frame while output comes from the delay line.
/// Below the knee the gain is exactly 1, so samples pass bit-exact (only delayed).
struct Limiter {
    /// Ring of `frames * channels`.
    delay: Vec<f64>,
    pos: usize,
    frames: usize,
    channels: usize,
    thresh_lin: f64,
    thresh_db: f64,
    knee_start: f64,
    knee_end: f64,
    attack: f64,
    release: f64,
    decay: f64,
    /// Peak hold for the look-ahead length, so release waits until the peak has left the delay line.
    env: f64,
    hold: usize,
    gain: f64,
    /// Smallest gain in the last buffer (UI meter).
    meter: f64,
}

/// `clone_from` keeps the delay line's memory (the sink copies the chain's state without allocating).
impl Clone for Limiter {
    fn clone(&self) -> Self {
        Limiter { delay: self.delay.clone(), ..*self }
    }

    fn clone_from(&mut self, o: &Self) {
        let mut delay = std::mem::take(&mut self.delay);
        delay.clone_from(&o.delay);
        *self = Limiter { delay, ..*o };
    }
}

impl Limiter {
    fn frames_for(rate: f64, lookahead_ms: f64) -> usize {
        ((finite(lookahead_ms, 5.0).clamp(0.5, 20.0) / 1000.0 * rate) as usize).max(1)
    }

    fn new(rate: f64, channels: usize, frames: usize) -> Self {
        let mut l = Limiter {
            delay: vec![0.0; frames * channels],
            pos: 0,
            frames,
            channels,
            thresh_lin: 1.0,
            thresh_db: 0.0,
            knee_start: 1.0,
            knee_end: 1.0,
            attack: 1.0,
            release: 1.0,
            decay: 1.0,
            env: 0.0,
            hold: 0,
            gain: 1.0,
            meter: 1.0,
        };
        l.tune(rate, 0.0, 100.0);
        l
    }

    /// Retunes coefficients, keeping the delay line and gain.
    fn tune(&mut self, rate: f64, thresh_db: f64, release_ms: f64) {
        let thresh_db = finite(thresh_db, -1.0).clamp(-40.0, 0.0);
        let per_sample = (-1.0 / (finite(release_ms, 100.0).clamp(5.0, 2000.0) / 1000.0 * rate)).exp();
        self.thresh_db = thresh_db;
        self.thresh_lin = 10f64.powf(thresh_db / 20.0);
        self.knee_start = 10f64.powf((thresh_db - KNEE_DB / 2.0) / 20.0);
        self.knee_end = 10f64.powf((thresh_db + KNEE_DB / 2.0) / 20.0);
        // Within 0.1 % of the target over the look-ahead.
        self.attack = 1.0 - 0.001f64.powf(1.0 / self.frames as f64);
        self.release = 1.0 - per_sample;
        self.decay = per_sample;
    }

    /// Static curve: 1 below the knee, quadratic through it, hard ceiling above.
    #[inline]
    fn curve(&self, env: f64) -> f64 {
        if env <= self.knee_start {
            1.0
        } else if env >= self.knee_end {
            self.thresh_lin / env // = 10^(-(env_db - thresh_db)/20), without the logarithms
        } else {
            let over = 20.0 * env.log10() - self.thresh_db + KNEE_DB / 2.0;
            10f64.powf(-(over * over / (2.0 * KNEE_DB)) / 20.0)
        }
    }

    /// The gain follower after a frame peaking at `peak`; the state is passed in so a block keeps it in
    /// registers.
    #[inline(always)]
    fn follow(&self, peak: f64, env: &mut f64, hold: &mut usize, gain: &mut f64) {
        if peak >= *env {
            (*env, *hold) = (peak, self.frames);
        } else if *hold > 0 {
            *hold -= 1;
        } else {
            *env *= self.decay;
        }
        let want = self.curve(*env);
        // At rest (the usual case) the gain stays 1 without its update.
        if want != 1.0 || *gain != 1.0 {
            *gain += (want - *gain) * if want < *gain { self.attack } else { self.release };
            if *gain > 1.0 - 1e-7 {
                *gain = 1.0; // snap back to bit-exact
            }
        }
    }

    /// The gain for the frame leaving the delay line, peaking at `leaving`. The follower lands within
    /// 0.1 %, which can still overshoot a large peak (up to 0.13 dB): the leaving frame's gain is also
    /// held to the curve for its own peak.
    #[inline(always)]
    fn leaving_gain(&self, leaving: f64, gain: f64) -> f64 {
        if leaving * gain > self.knee_start {
            gain.min(self.curve(leaving))
        } else {
            gain
        }
    }

    #[inline]
    fn frame(&mut self, x: &mut [f64]) {
        let peak = x.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let (mut env, mut hold, mut gain) = (self.env, self.hold, self.gain);
        self.follow(peak, &mut env, &mut hold, &mut gain);
        (self.env, self.hold, self.gain) = (env, hold, gain);
        let slot = self.pos * self.channels;
        let leaving = self.delay[slot..slot + self.channels].iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let gain = self.leaving_gain(leaving, self.gain);
        for (c, v) in x.iter_mut().enumerate() {
            let out = self.delay[slot + c];
            self.delay[slot + c] = *v;
            *v = out * gain;
        }
        self.pos = if self.pos + 1 == self.frames { 0 } else { self.pos + 1 };
        self.meter = self.meter.min(self.gain);
    }

    /// [`Limiter::frame`] over a stereo block, the state in locals.
    fn block2(&mut self, left: &mut [f64], right: &mut [f64]) {
        let mut delay = std::mem::take(&mut self.delay);
        let (mut pos, mut env, mut hold, mut gain, mut meter) = (self.pos, self.env, self.hold, self.gain, self.meter);
        let slots = delay.as_chunks_mut::<2>().0;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.follow(l.abs().max(r.abs()), &mut env, &mut hold, &mut gain);
            let slot = &mut slots[pos];
            let g = self.leaving_gain(slot[0].abs().max(slot[1].abs()), gain);
            let out = std::mem::replace(slot, [*l, *r]);
            (*l, *r) = (out[0] * g, out[1] * g);
            pos = if pos + 1 == self.frames { 0 } else { pos + 1 };
            meter = meter.min(gain);
        }
        (self.pos, self.env, self.hold, self.gain, self.meter) = (pos, env, hold, gain, meter);
        self.delay = delay;
    }

    fn reset(&mut self) {
        self.delay.fill(0.0);
        (self.pos, self.env, self.hold, self.gain, self.meter) = (0, 0.0, 0, 1.0, 1.0);
    }
}

/// Loudness compensation (`contour::design`): up to two shelves and their pre-gain.
#[derive(Clone, Copy)]
struct Loud {
    filters: [Biquad; 2],
    count: usize,
    pre: f64,
    state: [[[f64; 2]; MAX_CHANNELS]; 2],
}

impl Loud {
    fn same(&self, o: &Loud) -> bool {
        self.filters[..self.count] == o.filters[..o.count] && self.pre == o.pre
    }
}

/// Crossfade length from the old chain to the new after a settings change.
const CHANGE_FADE_MS: f64 = 10.0;

/// One configuration of the chain with its state; two run side by side during a change fade.
struct Stages {
    channels: usize,
    /// Only bands that change something.
    filters: Vec<Biquad>,
    /// Per filter, per channel.
    state: Vec<[[f64; 2]; MAX_CHANNELS]>,
    preamp: f64,
    /// Bass boost shelf and per-channel state.
    bass: Option<(Biquad, [[f64; 2]; MAX_CHANNELS])>,
    loud: Option<Loud>,
    expander: Option<Expander>,
    compressor: Option<Compressor>,
    virtualizer: Option<Virtualizer>,
    crossfeed: Option<Crossfeed>,
    /// Crossfeed cutoff, kept for when it is (re)built.
    crossfeed_hz: f64,
    mono: bool,
    balance: (f64, f64),
    /// Volume boost, linear.
    boost: f64,
    limiter: Option<Limiter>,
}

/// `clone_from` keeps the memory of every buffer.
impl Clone for Stages {
    fn clone(&self) -> Self {
        Stages { filters: self.filters.clone(), state: self.state.clone(), virtualizer: self.virtualizer.clone(), limiter: self.limiter.clone(), ..*self }
    }

    fn clone_from(&mut self, o: &Self) {
        let (mut filters, mut state, mut virtualizer, mut limiter) = (std::mem::take(&mut self.filters), std::mem::take(&mut self.state), self.virtualizer.take(), self.limiter.take());
        filters.clone_from(&o.filters);
        state.clone_from(&o.state);
        virtualizer.clone_from(&o.virtualizer);
        limiter.clone_from(&o.limiter);
        *self = Stages { filters, state, virtualizer, limiter, ..*o };
    }
}

impl Stages {
    fn is_identity(&self) -> bool {
        self.filters.is_empty()
            && self.bass.is_none()
            && self.loud.is_none()
            && self.expander.is_none()
            && self.compressor.is_none()
            && self.virtualizer.is_none()
            && self.boost == 1.0
            && self.crossfeed.is_none()
            && self.limiter.is_none()
            && !self.mono
            && self.balance == (1.0, 1.0)
            && (self.preamp - 1.0).abs() < 1e-6
    }

    /// Whether the two sound the same (a retuned limiter glides by itself, so it does not count).
    fn sounds_like(&self, o: &Stages) -> bool {
        self.filters == o.filters
            && self.preamp == o.preamp
            && self.bass.map(|b| b.0) == o.bass.map(|b| b.0)
            && match (&self.loud, &o.loud) {
                (Some(a), Some(b)) => a.same(b),
                (a, b) => a.is_none() && b.is_none(),
            }
            && self.compressor.as_ref().map(Compressor::settings) == o.compressor.as_ref().map(Compressor::settings)
            && self.expander.as_ref().map(Expander::settings) == o.expander.as_ref().map(Expander::settings)
            && self.virtualizer.as_ref().map(Virtualizer::strength) == o.virtualizer.as_ref().map(Virtualizer::strength)
            && self.boost == o.boost
            && self.mono == o.mono
            && self.balance == o.balance
            && match (&self.crossfeed, &o.crossfeed) {
                (Some(a), Some(b)) => a.same(b),
                (a, b) => a.is_none() && b.is_none(),
            }
            && self.limiter.as_ref().map(|l| l.frames) == o.limiter.as_ref().map(|l| l.frames)
    }

    #[inline]
    fn sample(&mut self, ch: usize, x: f64) -> f64 {
        let mut x = x * self.preamp;
        let bit = 1u8 << ch;
        for (f, st) in self.filters.iter().zip(self.state.iter_mut()) {
            if f.chans & bit != 0 {
                x = f.tick(&mut st[ch], x);
            }
        }
        if let Some((f, st)) = self.bass.as_mut() {
            x = f.tick(&mut st[ch], x);
        }
        if let Some(l) = self.loud.as_mut() {
            x *= l.pre;
            for (f, st) in l.filters[..l.count].iter().zip(l.state.iter_mut()) {
                x = f.tick(&mut st[ch], x);
            }
        }
        x
    }

    /// Everything after the filters, on one frame.
    #[inline]
    fn output_stage(&mut self, f: &mut [f64]) {
        // Expander before the compressor, so the compressor does not lift what the expander lowered.
        if let Some(e) = self.expander.as_mut() {
            e.frame(f);
        }
        if let Some(c) = self.compressor.as_mut() {
            c.block(f, 1);
        }
        if self.channels == 2 {
            if self.mono {
                let m = (f[0] + f[1]) * MONO_SUM;
                (f[0], f[1]) = (m, m);
            }
            if let Some(v) = self.virtualizer.as_mut() {
                (f[0], f[1]) = v.frame(f[0], f[1]);
            }
            if let Some(cf) = self.crossfeed.as_mut() {
                (f[0], f[1]) = cf.frame(f[0], f[1]);
            }
            f[0] *= self.balance.0;
            f[1] *= self.balance.1;
        }
        if self.boost != 1.0 {
            f.iter_mut().for_each(|v| *v *= self.boost);
        }
        if let Some(l) = self.limiter.as_mut() {
            l.frame(f);
        }
    }

    #[inline]
    fn frame(&mut self, f: &mut [f64]) {
        for (c, v) in f.iter_mut().enumerate() {
            *v = self.sample(c, *v);
        }
        self.output_stage(f);
    }

    /// [`Stages::frame`] over `frames` frames held channel after channel: each filter runs over a
    /// channel at a time, the same arithmetic in the same order per sample.
    fn block(&mut self, planar: &mut [f64], frames: usize) {
        if self.channels == 2 {
            self.block2(planar, frames);
        } else {
            self.block_each(planar, frames);
        }
        self.block_output(planar, frames);
    }

    /// Stereo: filters on both sides run both at once.
    fn block2(&mut self, planar: &mut [f64], frames: usize) {
        let (left, right) = planar.split_at_mut(frames);
        if self.preamp != 1.0 {
            left.iter_mut().chain(right.iter_mut()).for_each(|v| *v *= self.preamp);
        }
        let mut k = 0;
        while k < self.filters.len() {
            // Consecutive filters on both sides run together, up to CHAIN at once.
            let (fs, st) = (&self.filters[k..], &mut self.state[k..]);
            let both = fs.iter().take(CHAIN).take_while(|f| f.chans & 3 == 3).count();
            match (both, fs[0].chans & 3) {
                (2, _) => Biquad::run2_chain::<2>(fs, st, left, right),
                (3, _) => Biquad::run2_chain::<3>(fs, st, left, right),
                (CHAIN, _) => Biquad::run2_chain::<CHAIN>(fs, st, left, right),
                (_, 3) => fs[0].run2(&mut st[0][..2], left, right),
                (_, 1) => fs[0].run(&mut st[0][0], left),
                (_, 2) => fs[0].run(&mut st[0][1], right),
                _ => {}
            }
            k += both.max(1);
        }
        if let Some((f, st)) = self.bass.as_mut() {
            f.run2(&mut st[..2], left, right);
        }
        if let Some(l) = self.loud.as_mut() {
            left.iter_mut().chain(right.iter_mut()).for_each(|v| *v *= l.pre);
            for (f, st) in l.filters[..l.count].iter().zip(l.state.iter_mut()) {
                f.run2(&mut st[..2], left, right);
            }
        }
    }

    fn block_each(&mut self, planar: &mut [f64], frames: usize) {
        for c in 0..self.channels {
            let x = &mut planar[c * frames..(c + 1) * frames];
            if self.preamp != 1.0 {
                x.iter_mut().for_each(|v| *v *= self.preamp);
            }
            let bit = 1u8 << c;
            for (f, st) in self.filters.iter().zip(self.state.iter_mut()) {
                if f.chans & bit != 0 {
                    f.run(&mut st[c], x);
                }
            }
            if let Some((f, st)) = self.bass.as_mut() {
                f.run(&mut st[c], x);
            }
            if let Some(l) = self.loud.as_mut() {
                x.iter_mut().for_each(|v| *v *= l.pre);
                for (f, st) in l.filters[..l.count].iter().zip(l.state.iter_mut()) {
                    f.run(&mut st[c], x);
                }
            }
        }
    }

    /// The output stage over the block when it does anything: frame by frame, or stage by stage in stereo.
    fn block_output(&mut self, planar: &mut [f64], frames: usize) {
        let stereo = self.channels == 2 && (self.mono || self.virtualizer.is_some() || self.crossfeed.is_some() || self.balance != (1.0, 1.0));
        if !(stereo || self.expander.is_some() || self.compressor.is_some() || self.boost != 1.0 || self.limiter.is_some()) {
            return;
        }
        if self.channels == 2 {
            self.block_output2(planar, frames);
            return;
        }
        let mut frame = [0f64; MAX_CHANNELS];
        let n = self.channels;
        for k in 0..frames {
            for (c, v) in frame[..n].iter_mut().enumerate() {
                *v = planar[c * frames + k];
            }
            self.output_stage(&mut frame[..n]);
            for (c, v) in frame[..n].iter().enumerate() {
                planar[c * frames + k] = *v;
            }
        }
    }

    /// [`Stages::output_stage`] in stereo, a stage at a time over the block: each stage keeps only its
    /// own state, so every frame gets the same arithmetic in the same order, except the compressor's
    /// interpolated gain.
    fn block_output2(&mut self, planar: &mut [f64], frames: usize) {
        if let Some(e) = self.expander.as_mut() {
            let (left, right) = planar.split_at_mut(frames);
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let mut f = [*l, *r];
                e.frame(&mut f);
                (*l, *r) = (f[0], f[1]);
            }
        }
        if let Some(c) = self.compressor.as_mut() {
            c.block(planar, frames);
        }
        let (left, right) = planar.split_at_mut(frames);
        if self.mono {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let m = (*l + *r) * MONO_SUM;
                (*l, *r) = (m, m);
            }
        }
        if let Some(v) = self.virtualizer.as_mut() {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                (*l, *r) = v.frame(*l, *r);
            }
        }
        if let Some(cf) = self.crossfeed.as_mut() {
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                (*l, *r) = cf.frame(*l, *r);
            }
        }
        if self.balance != (1.0, 1.0) {
            left.iter_mut().for_each(|v| *v *= self.balance.0);
            right.iter_mut().for_each(|v| *v *= self.balance.1);
        }
        if self.boost != 1.0 {
            left.iter_mut().chain(right.iter_mut()).for_each(|v| *v *= self.boost);
        }
        if let Some(l) = self.limiter.as_mut() {
            l.block2(left, right);
        }
    }

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = [[0.0; 2]; MAX_CHANNELS]);
        if let Some((_, st)) = self.bass.as_mut() {
            *st = [[0.0; 2]; MAX_CHANNELS];
        }
        if let Some(l) = self.loud.as_mut() {
            l.state = [[[0.0; 2]; MAX_CHANNELS]; 2];
        }
        if let Some(c) = self.compressor.as_mut() {
            c.reset();
        }
        if let Some(e) = self.expander.as_mut() {
            e.reset();
        }
        if let Some(v) = self.virtualizer.as_mut() {
            v.reset();
        }
        if let Some(c) = self.crossfeed.as_mut() {
            (c.lo, c.hi, c.last) = ([0.0; 2], [0.0; 2], [0.0; 2]);
        }
        if let Some(l) = self.limiter.as_mut() {
            l.reset();
        }
    }
}

/// The whole sample-domain chain. A settings change while playing crossfades from the old chain (kept
/// running) to the new over [`CHANGE_FADE_MS`] instead of clicking; a new limiter first fills its
/// look-ahead. No fade before the first sample or after a reset.
pub struct Equalizer {
    rate: f64,
    channels: usize,
    now: Stages,
    /// The chain before the last change, run during the fade.
    was: Stages,
    /// Frames into the fade (negative while a new limiter fills); `None` when not fading.
    fade: Option<i64>,
    fade_len: i64,
    /// Samples have passed since the last reset, so changes must fade.
    live: bool,
    /// For 16-bit output ([`Equalizer::process_i16`]).
    dither: Dither,
    /// The samples being run, channel after channel (scratch).
    planar: Vec<f64>,
}

/// `clone_from` keeps the memory of both chains.
impl Clone for Equalizer {
    fn clone(&self) -> Self {
        Equalizer { now: self.now.clone(), was: self.was.clone(), planar: Vec::new(), ..*self }
    }

    fn clone_from(&mut self, o: &Self) {
        self.now.clone_from(&o.now);
        self.was.clone_from(&o.was);
        let Equalizer { rate, channels, now: _, was: _, fade, fade_len, live, dither, planar: _ } = *o;
        (self.rate, self.channels, self.fade, self.fade_len, self.live, self.dither) = (rate, channels, fade, fade_len, live, dither);
    }
}

/// Left and right gain for a balance in -1 (hard left) to 1 (hard right).
fn balance_gains(balance: f64) -> (f64, f64) {
    let b = finite(balance, 0.0).clamp(-1.0, 1.0);
    let att = if b.abs() >= 1.0 { 0.0 } else { 10f64.powf(-b.abs() * BALANCE_RANGE_DB / 20.0) };
    if b >= 0.0 {
        (att, 1.0)
    } else {
        (1.0, att)
    }
}

impl Equalizer {
    pub fn new(rate: u32, channels: usize) -> Self {
        let channels = channels.clamp(1, MAX_CHANNELS);
        let stages = Stages {
            channels,
            filters: Vec::new(),
            state: Vec::new(),
            preamp: 1.0,
            bass: None,
            loud: None,
            expander: None,
            compressor: None,
            virtualizer: None,
            crossfeed: None,
            crossfeed_hz: CROSSFEED_DEFAULT_HZ,
            mono: false,
            balance: (1.0, 1.0),
            boost: 1.0,
            limiter: None,
        };
        Equalizer {
            rate: rate as f64,
            channels,
            now: stages.clone(),
            was: stages,
            fade: None,
            fade_len: ((rate as f64 * CHANGE_FADE_MS / 1000.0).round() as i64).max(1),
            live: false,
            dither: Dither::new(),
            planar: Vec::new(),
        }
    }

    /// Applies a change, fading if live; a change mid-fade keeps fading from the same old chain.
    fn change(&mut self, apply: impl FnOnce(&mut Stages, f64)) {
        if !self.live {
            apply(&mut self.now, self.rate);
            return;
        }
        if self.fade.is_none() {
            self.was = self.now.clone();
        }
        let lookahead = self.now.limiter.as_ref().map(|l| l.frames);
        apply(&mut self.now, self.rate);
        let delay = self.now.limiter.as_ref().map_or(0, |l| l.frames as i64);
        // A changed limiter delays the change by its look-ahead: the fade waits for it.
        if delay as usize != lookahead.unwrap_or(0) && delay > 0 || self.fade.is_none() && !self.now.sounds_like(&self.was) {
            self.fade = Some(-delay);
        }
    }

    /// Samples have passed through the chain this replaces: changes fade from the first.
    pub fn continuing(&mut self) {
        self.live = true;
    }

    /// `crossfeed_db` 0 turns crossfeed off; typical values are 3 to 6.
    pub fn configure(&mut self, bands: &[Band], preamp_db: f64, crossfeed_db: f64) {
        self.change(|s, rate| {
            s.filters.clear();
            for b in bands {
                // Side bands need two channels.
                let routable = b.channel == CH_BOTH || (s.channels >= 2 && (b.channel == CH_LEFT || b.channel == CH_RIGHT));
                let shaped = !uses_gain(b.kind) || b.gain_db.abs() >= 0.05;
                if routable && shaped && b.freq > 0.0 && b.freq < rate / 2.0 && matches!(b.kind, PEAKING..=HIGH_SHELF_SLOPE) {
                    s.filters.push(Biquad::new(rate, &Band { gain_db: b.gain_db.clamp(-24.0, 24.0), ..*b }));
                }
            }
            Self::rest(s, rate, preamp_db, crossfeed_db);
        });
    }

    /// The graphic equalizer instead of the parametric one (`sliders` per `graphic::LAYOUTS`).
    pub fn configure_graphic(&mut self, sliders: &[f64], preamp_db: f64, crossfeed_db: f64) {
        self.change(|s, rate| {
            s.filters.clear();
            // Not clamped to ±24 dB: the design needs larger opposing gains and bounds them itself.
            s.filters.extend(crate::graphic::design(rate, sliders).iter().map(|b| Biquad::new(rate, b)));
            Self::rest(s, rate, preamp_db, crossfeed_db);
        });
    }

    fn rest(s: &mut Stages, rate: f64, preamp_db: f64, crossfeed_db: f64) {
        s.state.resize(s.filters.len(), [[0.0; 2]; MAX_CHANNELS]);
        s.preamp = 10f64.powf(finite(preamp_db, 0.0).clamp(-30.0, 12.0) / 20.0);
        s.crossfeed = (crossfeed_db > 0.0 && s.channels == 2).then(|| Crossfeed::new(rate, crossfeed_db, s.crossfeed_hz));
    }

    /// Sets the crossfeed cutoff (clamped to [`CROSSFEED_CUT_HZ`]); the level comes from `configure`.
    pub fn set_crossfeed_cut(&mut self, cut_hz: f64) {
        let cut_hz = finite(cut_hz, CROSSFEED_DEFAULT_HZ).clamp(CROSSFEED_CUT_HZ.0, CROSSFEED_CUT_HZ.1);
        if cut_hz == self.now.crossfeed_hz {
            return;
        }
        self.change(|s, rate| {
            s.crossfeed_hz = cut_hz;
            if let Some(level) = s.crossfeed.as_ref().map(|c| c.level_db) {
                s.crossfeed = Some(Crossfeed::new(rate, level, cut_hz));
            }
        });
    }

    /// Configures the effects; running ones are retuned keeping state. Boosts need the limiter
    /// ([`Effects::guard`]), which the caller enables via [`Equalizer::configure_output`].
    pub fn configure_effects(&mut self, e: &Effects) {
        self.change(|s, rate| {
            let bass = finite(e.bass_boost_db, 0.0).clamp(0.0, BASS_BOOST_MAX_DB);
            let shelf = (bass >= 0.05).then(|| Biquad::new(rate, &Band { kind: LOW_SHELF_SLOPE, freq: BASS_HZ, gain_db: bass, q: BASS_SLOPE, channel: CH_BOTH }));
            s.bass = match (shelf, s.bass) {
                (Some(f), Some((old, st))) if old == f => Some((f, st)),
                (Some(f), Some((_, st))) => Some((f, st)),
                (Some(f), None) => Some((f, [[0.0; 2]; MAX_CHANNELS])),
                (None, _) => None,
            };
            s.compressor = match (e.compressor, s.compressor.take()) {
                (Some(c), Some(mut old)) => {
                    old.tune(rate, c);
                    Some(old)
                }
                (Some(c), None) => Some(Compressor::new(rate, c)),
                (None, _) => None,
            };
            let loud = e.loudness.map(|l| crate::contour::design(l.reference_phon, l.volume_db)).and_then(|sh| {
                let bands: Vec<Band> = sh.low.into_iter().chain(sh.high).collect();
                (!bands.is_empty()).then(|| {
                    let mut filters = [Biquad::default(); 2];
                    for (f, b) in filters.iter_mut().zip(&bands) {
                        *f = Biquad::new(rate, b);
                    }
                    Loud { filters, count: bands.len(), pre: 10f64.powf(sh.pre_db / 20.0), state: [[[0.0; 2]; MAX_CHANNELS]; 2] }
                })
            });
            s.loud = match (loud, s.loud.take()) {
                (Some(mut new), Some(old)) if new.count == old.count => {
                    new.state = old.state;
                    Some(new)
                }
                (new, _) => new,
            };
            s.expander = match (e.expander, s.expander.take()) {
                (Some(x), Some(mut old)) => {
                    old.tune(rate, x);
                    Some(old)
                }
                (Some(x), None) => Some(Expander::new(rate, x)),
                (None, _) => None,
            };
            let width = finite(e.virtualizer, 0.0).clamp(0.0, 1.0);
            s.virtualizer = match (width > 0.0 && s.channels == 2, s.virtualizer.take()) {
                (true, Some(mut v)) => {
                    v.tune(width);
                    Some(v)
                }
                (true, None) => Some(Virtualizer::new(rate, width)),
                (false, _) => None,
            };
            s.boost = 10f64.powf(finite(e.boost_db, 0.0).clamp(0.0, VOLUME_BOOST_MAX_DB) / 20.0);
        });
    }

    /// The output stage. `balance` -1 (left) to 1 (right); mono and balance need stereo. `lookahead_ms <= 0`
    /// turns the limiter off.
    pub fn configure_output(&mut self, balance: f64, mono: bool, threshold_db: f64, release_ms: f64, lookahead_ms: f64) {
        self.change(|s, rate| {
            s.mono = mono && s.channels == 2;
            s.balance = if s.channels == 2 { balance_gains(balance) } else { (1.0, 1.0) };
            if lookahead_ms <= 0.0 {
                s.limiter = None;
                return;
            }
            let frames = Limiter::frames_for(rate, lookahead_ms);
            let mut l = s.limiter.take().filter(|l| l.frames == frames).unwrap_or_else(|| Limiter::new(rate, s.channels, frames));
            l.tune(rate, threshold_db, release_ms);
            s.limiter = Some(l);
        });
    }

    /// The chain would not change any sample.
    pub fn is_identity(&self) -> bool {
        self.fade.is_none() && self.now.is_identity()
    }

    /// Frames held back by the limiter's look-ahead (push this much silence at end of stream).
    pub fn delay_frames(&self) -> usize {
        self.now.limiter.as_ref().map_or(0, |l| l.frames)
    }

    /// Limiter's peak gain reduction in the last buffer, dB.
    pub fn gain_reduction_db(&self) -> f32 {
        self.now.limiter.as_ref().map_or(0.0, |l| (-20.0 * l.meter.log10()) as f32)
    }

    /// Compressor's largest gain reduction in the last buffer, dB.
    pub fn compression_db(&self) -> f32 {
        self.now.compressor.as_ref().map_or(0.0, |c| c.meter_db as f32)
    }

    /// The processing loop, generic over sample format via `load` and `store(value, channel)`; `gain`
    /// scales the input first.
    #[inline]
    fn run<T: Copy>(&mut self, input: &[T], output: &mut [T], load: impl Fn(T) -> f64, mut store: impl FnMut(f64, usize) -> T, gain: f64) {
        let len = input.len().min(output.len());
        let (input, output) = (&input[..len], &mut output[..len]);
        self.live |= len > 0;
        if gain == 1.0 && self.is_identity() {
            output.copy_from_slice(input);
            return;
        }
        let n = self.channels;
        if let Some(l) = self.now.limiter.as_mut() {
            l.meter = 1.0;
        }
        if let Some(c) = self.now.compressor.as_mut() {
            c.meter_db = 0.0;
        }
        if self.fade.is_none() {
            let frames = len / n;
            let mut planar = std::mem::take(&mut self.planar);
            // Every sample is written over: only a longer buffer grows it.
            if planar.len() < frames * n {
                planar.resize(frames * n, 0.0);
            }
            // Stereo, with each side's channel known in the loop (the dither keeps both in registers).
            if n == 2 {
                let (left, right) = planar[..frames * 2].split_at_mut(frames);
                for ((l, r), &[x, y]) in left.iter_mut().zip(right.iter_mut()).zip(input.as_chunks::<2>().0) {
                    (*l, *r) = (load(x) * gain, load(y) * gain);
                }
            } else if frames > 0 {
                for (c, lane) in planar[..frames * n].chunks_exact_mut(frames).enumerate() {
                    lane.iter_mut().zip(input[c..].iter().step_by(n)).for_each(|(p, &v)| *p = load(v) * gain);
                }
            }
            self.now.block(&mut planar[..frames * n], frames);
            if n == 2 {
                let (left, right) = planar[..frames * 2].split_at(frames);
                for ((y, &l), &r) in output.as_chunks_mut::<2>().0.iter_mut().zip(left).zip(right) {
                    *y = [store(l, 0), store(r, 1)];
                }
            } else {
                for (k, y) in output.chunks_exact_mut(n).enumerate() {
                    for (c, v) in y.iter_mut().enumerate() {
                        *v = store(planar[c * frames + k], c);
                    }
                }
            }
            self.planar = planar;
            let tail = len - len % n;
            output[tail..].copy_from_slice(&input[tail..]);
            return;
        }
        let mut frame = [0f64; MAX_CHANNELS];
        let mut old = [0f64; MAX_CHANNELS];
        for (x, y) in input.chunks_exact(n).zip(output.chunks_exact_mut(n)) {
            for (c, v) in x.iter().enumerate() {
                frame[c] = load(*v) * gain;
            }
            match self.fade {
                None => self.now.frame(&mut frame[..n]),
                Some(at) => {
                    old[..n].copy_from_slice(&frame[..n]);
                    self.was.frame(&mut old[..n]);
                    self.now.frame(&mut frame[..n]);
                    // Raised cosine.
                    let g = if at < 0 { 0.0 } else { 0.5 - 0.5 * (std::f64::consts::PI * (at + 1) as f64 / self.fade_len as f64).cos() };
                    for (v, o) in frame[..n].iter_mut().zip(&old[..n]) {
                        *v = o + g * (*v - o);
                    }
                    self.fade = (at + 1 < self.fade_len).then_some(at + 1);
                }
            }
            for (c, v) in y.iter_mut().enumerate() {
                *v = store(frame[c], c);
            }
        }
        // A partial trailing frame passes through.
        let tail = len - len % n;
        output[tail..].copy_from_slice(&input[tail..]);
    }

    /// 16-bit processing, scaled to full scale 1.0 (exact, and the limiter's ceiling needs it) and dithered
    /// back ([`crate::dither`]); unchanged samples come back as they were.
    pub fn process_i16(&mut self, input: &[i16], output: &mut [i16]) {
        let mut d = self.dither;
        // Mono: both channels share one noise and stay identical.
        if self.now.mono {
            self.run(input, output, |x| x as f64 / I16_SCALE, |y, c| d.to_i16_linked(c, y), 1.0);
        } else {
            self.run(input, output, |x| x as f64 / I16_SCALE, |y, c| d.to_i16(c, y), 1.0);
        }
        self.dither = d;
    }

    /// Float processing (f64 inside, so 24-bit sources keep full precision).
    pub fn process_f32(&mut self, input: &[f32], output: &mut [f32]) {
        self.run(input, output, |x| x as f64, |y, _| y as f32, 1.0);
    }

    /// [`Equalizer::process_i16`] or, with `float`, [`Equalizer::process_f32`] over little-endian samples
    /// as the player carries them, scaled by `gain` (a song's ReplayGain) first.
    pub fn process_bytes(&mut self, input: &[u8], output: &mut [u8], float: bool, gain: f32) {
        let gain = gain as f64;
        if float {
            self.run(input.as_chunks::<4>().0, output.as_chunks_mut::<4>().0, |x| f32::from_le_bytes(x) as f64, |y, _| (y as f32).to_le_bytes(), gain);
            return;
        }
        let mut d = self.dither;
        let (input, output) = (input.as_chunks::<2>().0, output.as_chunks_mut::<2>().0);
        let load = |x: [u8; 2]| i16::from_le_bytes(x) as f64 / I16_SCALE;
        if self.now.mono {
            self.run(input, output, load, |y, c| d.to_i16_linked(c, y).to_le_bytes(), gain);
        } else {
            self.run(input, output, load, |y, c| d.to_i16(c, y).to_le_bytes(), gain);
        }
        self.dither = d;
    }

    /// New stream (seek, flush): clears state and any fade.
    pub fn reset(&mut self) {
        self.now.reset();
        self.fade = None;
        self.live = false;
        self.dither.reset();
    }
}

/// Effects besides the equalizer, each off at 0 or `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Effects {
    /// 0 to [`BASS_BOOST_MAX_DB`].
    pub bass_boost_db: f64,
    pub compressor: Option<CompressorSettings>,
    pub expander: Option<ExpanderSettings>,
    /// Volume-dependent loudness compensation (`contour`).
    pub loudness: Option<crate::contour::Loudness>,
    /// Strength, 0 to 1.
    pub virtualizer: f64,
    /// 0 to [`VOLUME_BOOST_MAX_DB`].
    pub boost_db: f64,
}

impl Effects {
    /// Whether any of them touches the samples.
    pub fn on(&self) -> bool {
        self.bass_boost_db > 0.0
            || self.compressor.is_some()
            || self.expander.is_some()
            || self.loudness.is_some()
            || self.virtualizer > 0.0
            || self.boost_db > 0.0
    }

    /// Whether they add level, requiring the limiter.
    pub fn guard(&self) -> bool {
        self.bass_boost_db > 0.0 || self.boost_db > 0.0 || self.compressor.is_some_and(|c| c.makeup_db > 0.0)
    }
}

/// Full scale for 16-bit samples: `i16::MIN` maps to exactly -1.0.
const I16_SCALE: f64 = 32768.0;

fn band(kind: EqKind, freq: f32, gain_db: f32, q: f32) -> EqBand {
    EqBand { kind, freq, gain_db, q }
}

/// Automatic pre-amp: cut by the largest boost.
pub fn auto_preamp_db(bands: impl IntoIterator<Item = (i32, f32)>) -> f32 {
    -bands.into_iter().filter(|&(k, _)| uses_gain(k)).map(|(_, g)| g).fold(0f32, f32::max)
}

/// The built-in curves; each boosting preset carries a pre-amp that cancels its boost.
pub fn eq_presets() -> Vec<NamedPreset> {
    let preset = |kind: PresetKind, preamp_db: f32, bands: Vec<EqBand>| NamedPreset { kind, preamp_db, bands };
    vec![
        preset(PresetKind::Flat, 0.0, vec![]),
        preset(PresetKind::BassBoost, -6.0, vec![band(EqKind::LowShelf, 100.0, 6.0, 0.7), band(EqKind::Peaking, 60.0, 3.0, 1.0)]),
        preset(PresetKind::BassCut, 0.0, vec![band(EqKind::LowShelf, 110.0, -6.0, 0.7)]),
        preset(PresetKind::TrebleBoost, -5.0, vec![band(EqKind::HighShelf, 6000.0, 5.0, 0.7)]),
        preset(PresetKind::TrebleCut, 0.0, vec![band(EqKind::HighShelf, 6000.0, -5.0, 0.7)]),
        preset(
            PresetKind::VocalBoost,
            -4.0,
            vec![band(EqKind::Peaking, 300.0, -2.0, 1.0), band(EqKind::Peaking, 2500.0, 4.0, 1.2), band(EqKind::Peaking, 5000.0, 2.0, 1.5)],
        ),
        preset(
            PresetKind::Loudness,
            -7.0,
            vec![band(EqKind::LowShelfSlope, 80.0, 7.0, 0.8), band(EqKind::Peaking, 1000.0, -2.0, 1.0), band(EqKind::HighShelfSlope, 10000.0, 5.0, 0.8)],
        ),
        // Cut what small drivers only rattle on, restore body an octave up.
        preset(
            PresetKind::SmallSpeakers,
            -4.0,
            vec![band(EqKind::HighPass, 90.0, 0.0, 0.71), band(EqKind::Peaking, 220.0, 4.0, 1.0), band(EqKind::Peaking, 3000.0, 2.0, 1.2)],
        ),
    ]
}

impl From<&EqBand> for Band {
    fn from(b: &EqBand) -> Self {
        Band { kind: b.kind as i32, freq: b.freq as f64, gain_db: b.gain_db as f64, q: b.q as f64, channel: CH_BOTH }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_preamp_offsets_largest_boost() {
        assert_eq!(auto_preamp_db([(PEAKING, 4.5), (LOW_SHELF, 6.0), (HIGH_PASS, 12.0)]), -6.0, "a pass filter's gain is not a boost");
        assert_eq!(auto_preamp_db([(PEAKING, -3.0)]), 0.0, "cuts need no room");
        assert_eq!(auto_preamp_db([]), 0.0);
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn tone(freq: f64) -> Vec<f32> {
        tone_at(freq, 0.25)
    }

    fn tone_at(freq: f64, amplitude: f64) -> Vec<f32> {
        (0..48000).map(|i| (amplitude * (2.0 * std::f64::consts::PI * freq * i as f64 / 48000.0).sin()) as f32).collect()
    }

    fn b(kind: i32, freq: f64, gain_db: f64, q: f64) -> Band {
        Band { kind, freq, gain_db, q, channel: CH_BOTH }
    }

    fn gain_at(eq: &mut Equalizer, freq: f64) -> f64 {
        let x = tone(freq);
        let mut y = vec![0f32; x.len()];
        eq.reset();
        eq.process_f32(&x, &mut y);
        20.0 * (rms(&y[9600..]) / rms(&x[9600..])).log10()
    }

    /// Per-channel gain in dB for a centred stereo tone.
    fn stereo_gain_at(eq: &mut Equalizer, freq: f64) -> (f64, f64) {
        let m = tone(freq);
        let x: Vec<f32> = m.iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.reset();
        eq.process_f32(&x, &mut y);
        let l: Vec<f32> = y.iter().step_by(2).skip(4800).copied().collect();
        let r: Vec<f32> = y.iter().skip(1).step_by(2).skip(4800).copied().collect();
        let ref_rms = rms(&m[9600..]);
        (20.0 * (rms(&l) / ref_rms).log10(), 20.0 * (rms(&r) / ref_rms).log10())
    }

    fn peak(x: &[f32]) -> f64 {
        x.iter().fold(0f64, |m, v| m.max(v.abs() as f64))
    }

    #[test]
    fn flat_is_identity() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[], 0.0, 0.0);
        assert!(eq.is_identity());
        let x = tone(1000.0);
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        assert_eq!(x, y);
    }

    /// Each band kind's response: (bands, pre-amp, [(Hz, lowest dB, highest dB)]).
    #[test]
    fn band_responses() {
        let near = |db: f64, tol: f64| (db - tol, db + tol);
        let below = |db: f64| (-200.0, db);
        type Want = (f64, (f64, f64));
        let cases: &[(&str, Band, f64, &[Want])] = &[
            ("peaking", b(PEAKING, 1000.0, -12.0, 1.41), 0.0, &[(1000.0, near(-12.0, 0.5)), (8000.0, near(0.0, 0.5))]),
            ("low shelf and pre-amp", b(LOW_SHELF, 200.0, 6.0, 0.71), -3.0, &[(40.0, near(3.0, 0.5)), (5000.0, near(-3.0, 0.5))]),
            ("high shelf", b(HIGH_SHELF, 4000.0, -6.0, 0.71), 0.0, &[(16000.0, near(-6.0, 0.6)), (200.0, near(0.0, 0.5))]),
            ("low slope shelf", b(LOW_SHELF_SLOPE, 250.0, 8.0, 1.0), 0.0, &[(40.0, near(8.0, 0.6)), (250.0, near(4.0, 0.6)), (8000.0, near(0.0, 0.3))]),
            ("high slope shelf", b(HIGH_SHELF_SLOPE, 3000.0, -8.0, 1.0), 0.0, &[(16000.0, near(-8.0, 0.7)), (100.0, near(0.0, 0.3))]),
            ("low pass", b(LOW_PASS, 1000.0, 0.0, 0.707), 0.0, &[(100.0, near(0.0, 0.2)), (1000.0, near(-3.0, 0.6)), (8000.0, below(-15.0))]),
            ("high pass", b(HIGH_PASS, 1000.0, 0.0, 0.707), 0.0, &[(10000.0, near(0.0, 0.2)), (1000.0, near(-3.0, 0.6)), (125.0, below(-15.0))]),
            ("band pass", b(BAND_PASS, 1000.0, 0.0, 2.0), 0.0, &[(1000.0, near(0.0, 0.2)), (100.0, below(-12.0)), (10000.0, below(-12.0))]),
            ("notch", b(NOTCH, 1000.0, 0.0, 8.0), 0.0, &[(1000.0, below(-20.0)), (250.0, near(0.0, 0.4)), (4000.0, near(0.0, 0.4))]),
            ("all pass", b(ALL_PASS, 1000.0, 0.0, 0.707), 0.0, &[(100.0, near(0.0, 0.2)), (1000.0, near(0.0, 0.2)), (5000.0, near(0.0, 0.2)), (15000.0, near(0.0, 0.2))]),
        ];
        for (what, band, preamp, want) in cases {
            let mut eq = Equalizer::new(48000, 1);
            eq.configure(&[*band], *preamp, 0.0);
            for &(f, (lo, hi)) in *want {
                let g = gain_at(&mut eq, f);
                assert!(g >= lo && g <= hi, "{what}: {g:.2} dB at {f} Hz, wanted {lo}..{hi}");
            }
        }
        // The all pass moves the phase even though the level stays.
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[b(ALL_PASS, 1000.0, 0.0, 0.707)], 0.0, 0.0);
        let x = tone(1000.0);
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        assert!(!eq.is_identity() && x[24000..] != y[24000..]);
    }

    #[test]
    fn bad_settings_are_harmless() {
        let mut eq = Equalizer::new(48000, 2);
        let bad = [b(PEAKING, f64::NAN, 6.0, 1.0), b(PEAKING, 30000.0, 6.0, 1.0), b(PEAKING, -100.0, 6.0, 1.0), b(PEAKING, 1000.0, f64::NAN, 1.0), b(99, 1000.0, 6.0, 1.0)];
        eq.configure(&bad, f64::NAN, 0.0);
        assert!(eq.is_identity(), "no band survived, so the processor can be skipped");
        eq.configure_output(f64::NAN, false, f64::NAN, -5.0, f64::INFINITY);
        let x: Vec<f32> = tone(1000.0).iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        assert!(y.iter().all(|v| v.is_finite()), "bad limiter settings must not poison the output");
    }

    #[test]
    fn side_routed_bands() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[Band { channel: CH_LEFT, ..b(PEAKING, 1000.0, 12.0, 1.0) }], 0.0, 0.0);
        let (l, r) = stereo_gain_at(&mut eq, 1000.0);
        assert!((l - 12.0).abs() < 0.5 && r.abs() < 0.2, "left only: {l} / {r}");

        eq.configure(&[Band { channel: CH_RIGHT, ..b(PEAKING, 1000.0, -12.0, 1.0) }], 0.0, 0.0);
        let (l, r) = stereo_gain_at(&mut eq, 1000.0);
        assert!(l.abs() < 0.2 && (r + 12.0).abs() < 0.5, "right only: {l} / {r}");

        eq.configure(&[b(HIGH_SHELF, 4000.0, 8.0, 0.71)], 0.0, 0.0);
        let m = tone(4000.0);
        let x: Vec<f32> = m.iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.reset();
        eq.process_f32(&x, &mut y);
        assert!(y.as_chunks::<2>().0.iter().all(|f| f[0] == f[1]), "both-channel band kept the centre centred");

        let mut mono = Equalizer::new(48000, 1);
        mono.configure(&[Band { channel: CH_RIGHT, ..b(PEAKING, 1000.0, 12.0, 1.0) }], 0.0, 0.0);
        assert!(mono.is_identity());
    }

    /// The block path is the frame-by-frame chain, sample for sample: bands on both sides and on one
    /// (in runs of every length), and every output stage acting but the compressor, whose gain a block
    /// interpolates (`compressor::tests::interpolated_gain_follows_exact`).
    #[test]
    fn stereo_block_is_frame_by_frame() {
        let side = |channel, band: Band| Band { channel, ..band };
        let bands = [
            b(PEAKING, 60.0, 5.0, 1.0),
            b(LOW_SHELF, 120.0, -3.0, 0.7),
            side(CH_LEFT, b(PEAKING, 300.0, 4.0, 2.0)),
            b(PEAKING, 700.0, -4.0, 1.4),
            b(PEAKING, 1500.0, 6.0, 1.0),
            b(NOTCH, 2500.0, 0.0, 4.0),
            b(PEAKING, 4000.0, 3.0, 1.4),
            b(HIGH_SHELF, 8000.0, 4.0, 0.7),
            side(CH_RIGHT, b(PEAKING, 10000.0, -6.0, 1.0)),
            b(PEAKING, 12000.0, 2.0, 1.0),
            b(PEAKING, 14000.0, -2.0, 1.0),
        ];
        let expander = crate::compressor::ExpanderSettings { threshold_db: -30.0, ratio: 2.0, attack_ms: 2.0, release_ms: 50.0 };
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&bands, -2.0, 0.0);
        eq.configure_effects(&Effects { bass_boost_db: 4.0, expander: Some(expander), virtualizer: 0.5, boost_db: 3.0, ..Effects::default() });
        eq.configure_output(0.3, false, -3.0, 80.0, 2.0);
        // Loud and quiet stretches, so the expander and the limiter both move.
        let x: Vec<f64> = (0..48000).flat_map(|i| {
            let level = if (i / 6000) % 2 == 0 { 0.9 } else { 0.01 };
            let t = i as f64 / 48000.0;
            [level * (440.0 * std::f64::consts::TAU * t).sin(), level * (0.7 * (3000.0 * std::f64::consts::TAU * t).sin() + 0.3 * (90.0 * std::f64::consts::TAU * t).sin())]
        }).collect();
        let mut framed = eq.now.clone();
        let want: Vec<f64> = x.as_chunks::<2>().0.iter().flat_map(|&(mut f)| {
            framed.frame(&mut f);
            f
        }).collect();
        let mut blocked = eq.now.clone();
        let mut got = Vec::new();
        for chunk in x.chunks(2 * 1000) {
            let frames = chunk.len() / 2;
            let mut planar: Vec<f64> = chunk.iter().step_by(2).chain(chunk.iter().skip(1).step_by(2)).copied().collect();
            blocked.block(&mut planar, frames);
            got.extend((0..frames).flat_map(|k| [planar[k], planar[frames + k]]));
        }
        assert!(blocked.limiter.as_ref().unwrap().meter < 1.0 && blocked.expander.as_ref().unwrap().meter_db > 0.0, "the dynamics acted");
        assert!(got == want, "first difference at {:?}", got.iter().zip(&want).position(|(a, b)| a != b));
    }

    #[test]
    fn balance_and_mono_level() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(0.5, false, 0.0, 100.0, 0.0);
        let (l, r) = stereo_gain_at(&mut eq, 1000.0);
        assert!((l + 12.0).abs() < 0.01 && r.abs() < 0.01, "half right is -12 dB on the left: {l} / {r}");

        eq.configure_output(-1.0, false, 0.0, 100.0, 0.0);
        let (l, r) = stereo_gain_at(&mut eq, 1000.0);
        assert!(l.abs() < 0.01 && r < -100.0, "hard left mutes the right: {l} / {r}");

        eq.configure_output(0.0, false, 0.0, 100.0, 0.0);
        assert!(!eq.is_identity(), "the change fades in first");
        let (x, mut y) = (vec![0f32; 960], vec![0f32; 960]);
        eq.process_f32(&x, &mut y);
        assert!(eq.is_identity(), "and then centred balance costs nothing");

        // Uncorrelated channels keep their level through the mono sum.
        eq.configure_output(0.0, true, 0.0, 100.0, 0.0);
        let (a, c) = (tone(440.0), tone(3700.0));
        let x: Vec<f32> = a.iter().zip(&c).flat_map(|(l, r)| [*l, *r]).collect();
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        let db = 20.0 * (rms(&y[19200..]) / rms(&x[19200..])).log10();
        assert!(db.abs() < 0.3, "mono sum moved the level by {db} dB");
        // After the 10 ms fade-in.
        assert!(y[960..].as_chunks::<2>().0.iter().all(|f| f[0] == f[1]), "both channels carry the same mono signal");
    }

    /// Right relative to left, dB, for a left-only tone.
    fn leak_at(eq: &mut Equalizer, freq: f64) -> f64 {
        let left_only: Vec<f32> = tone(freq).iter().flat_map(|s| [*s, 0.0]).collect();
        let mut y = vec![0f32; left_only.len()];
        eq.reset();
        eq.process_f32(&left_only, &mut y);
        let l: Vec<f32> = y.iter().step_by(2).skip(9600).copied().collect();
        let r: Vec<f32> = y.iter().skip(1).step_by(2).skip(9600).copied().collect();
        20.0 * (rms(&r) / rms(&l)).log10()
    }

    #[test]
    fn crossfeed_presets_behave_like_bs2b() {
        for p in CrossfeedPreset::ALL {
            let (cut, level) = p.settings();
            assert_eq!(CrossfeedPreset::of(cut, level), Some(p));
            // Deep bass leaks exactly `level` down; centred sound loses under 2 dB anywhere.
            let mut eq = Equalizer::new(48000, 2);
            eq.set_crossfeed_cut(cut);
            eq.configure(&[], 0.0, level);
            let leak = leak_at(&mut eq, 40.0);
            assert!((leak + level).abs() < 0.3, "{p:?}: {leak} dB at 40 Hz, the preset says -{level}");
            for f in [50.0, 300.0, 700.0, 2000.0, 8000.0] {
                let (l, r) = stereo_gain_at(&mut eq, f);
                assert!(l <= 0.05 && l > -2.0 && (l - r).abs() < 1e-9, "{p:?}: a centred tone at {f} Hz moved {l} dB");
            }
        }
        assert_eq!(CrossfeedPreset::of(700.0, 5.0), None, "custom");
    }

    #[test]
    fn crossfeed_cutoff() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 6.0);
        let low = leak_at(&mut eq, 2500.0);
        eq.set_crossfeed_cut(1500.0);
        let high = leak_at(&mut eq, 2500.0);
        assert!(high > low + 3.0, "a higher cutoff lets more of 2.5 kHz across: {low} dB at 700 Hz, {high} dB at 1500 Hz");
        // A cutoff set before crossfeed is on is kept.
        let mut later = Equalizer::new(48000, 2);
        later.set_crossfeed_cut(1500.0);
        assert!(later.is_identity(), "a cutoff alone is no crossfeed");
        later.configure(&[], 0.0, 6.0);
        assert!((leak_at(&mut later, 2500.0) - high).abs() < 1e-6);
        later.set_crossfeed_cut(f64::NAN);
        let d = leak_at(&mut later, 2500.0);
        assert!((d - low).abs() < 1e-6, "NaN is the default cutoff: {d} / {low}");
        later.set_crossfeed_cut(1e9);
        assert!(leak_at(&mut later, 2500.0).is_finite());
    }

    /// Below its knee the limiter only delays, by its look-ahead: kept across a retune, given back by
    /// silence, gone with it.
    #[test]
    fn limiter_only_delays_below_knee() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(0.0, false, -6.0, 120.0, 5.0);
        assert!(!eq.is_identity(), "the look-ahead delay alone means the processor must run");
        // -12 dBFS, well under the knee.
        let x = tone_at(1000.0, 0.25);
        let (head, tail) = x.split_at(24000);
        let (mut a, mut b) = (vec![0f32; head.len()], vec![0f32; tail.len()]);
        eq.process_f32(head, &mut a);
        // A slider moved mid-track.
        eq.configure_output(0.0, false, -3.0, 300.0, 5.0);
        eq.process_f32(tail, &mut b);
        let d = eq.delay_frames();
        assert_eq!(d, 240, "5 ms at 48 kHz");
        let mut rest = vec![0f32; d];
        eq.process_f32(&vec![0f32; d], &mut rest);
        let joined: Vec<f32> = a.into_iter().chain(b).chain(rest).collect();
        assert_eq!(&joined[d..], &x[..], "untouched, the delay line kept through the new settings");
        assert_eq!(eq.gain_reduction_db(), 0.0);
        eq.configure_output(0.0, false, 0.0, 120.0, 0.0);
        assert_eq!(eq.delay_frames(), 0, "no limiter, nothing held");
    }

    #[test]
    fn limiter_ceiling_and_meter() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(0.0, false, -6.0, 80.0, 5.0);
        let m = tone_at(220.0, 1.0); // 0 dBFS into a -6 dBFS ceiling: 6 dB of reduction
        let x: Vec<f32> = m.iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);

        let ceiling = 10f64.powf(-6.0 / 20.0);
        assert!(peak(&y[9600..]) <= ceiling * 1.04, "peak {} over the {ceiling} ceiling", peak(&y[9600..]));
        assert!(peak(&y[9600..]) > ceiling * 0.9, "and it is not simply squashed flat");
        let gr = eq.gain_reduction_db() as f64;
        assert!((gr - 6.0).abs() < 0.5, "meter says {gr} dB, expected about 6");

        eq.reset();
        let quiet = tone_at(220.0, 0.1);
        let x: Vec<f32> = quiet.iter().flat_map(|s| [*s, *s]).collect();
        eq.process_f32(&x, &mut y);
        assert_eq!(eq.gain_reduction_db(), 0.0);
    }

    /// Regression: unscaled 16-bit input made the limiter cut 91 dB.
    #[test]
    fn limiter_on_16_bit() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(0.0, false, -1.0, 120.0, 5.0);
        let d = 240 * 2; // 5 ms at 48 kHz, two channels

        let quiet: Vec<i16> = tone_at(1000.0, 0.25).iter().flat_map(|s| { let v = (*s * 32767.0) as i16; [v, v] }).collect();
        let mut y = vec![0i16; quiet.len()];
        eq.process_i16(&quiet, &mut y);
        assert_eq!(&y[d..], &quiet[..quiet.len() - d], "16-bit audio below the ceiling must pass through untouched");
        assert_eq!(eq.gain_reduction_db(), 0.0, "and the meter must not claim a reduction");

        eq.reset();
        let loud: Vec<i16> = tone_at(220.0, 1.0).iter().flat_map(|s| { let v = (*s * 32767.0) as i16; [v, v] }).collect();
        let mut y = vec![0i16; loud.len()];
        eq.process_i16(&loud, &mut y);
        let gr = eq.gain_reduction_db() as f64;
        assert!(gr > 0.3 && gr < 2.0, "meter says {gr} dB on a full-scale tone into a -1 dB ceiling");
        let peak = y[9600..].iter().map(|v| (*v as f64).abs()).fold(0.0, f64::max) / 32768.0;
        assert!(peak > 0.8 && peak <= 10f64.powf(-1.0 / 20.0) * 1.04, "16-bit peak {peak}, should sit just under the ceiling");
    }

    /// Power of `x` in DFT bin `k` (Hann window), relative to full scale.
    fn bin_power(x: &[f64], k: usize) -> f64 {
        let n = x.len() as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n).cos();
            let ph = std::f64::consts::TAU * k as f64 * i as f64 / n;
            re += v * w * ph.cos();
            im += v * w * ph.sin();
        }
        (re * re + im * im) / (n * n)
    }

    /// A -100 dBFS tone (a third of a 16-bit step) survives dithering at its level with no step harmonics;
    /// plain rounding loses it or adds harmonics.
    #[test]
    fn sub_lsb_tone_survives_dither() {
        const N: usize = 1 << 16;
        const K: usize = 1000; // 732 Hz at 48 kHz, whole cycles in the window
        // -46 dBFS in, 54 dB off (30 pre-amp, 24 cut).
        let x: Vec<i16> = (0..N).map(|i| ((std::f64::consts::TAU * K as f64 * i as f64 / N as f64).sin() * 10f64.powf(-46.0 / 20.0) * 32768.0).round() as i16).collect();
        let chain = || {
            let mut eq = Equalizer::new(48000, 1);
            eq.configure(&[b(PEAKING, 48000.0 * K as f64 / N as f64, -24.0, 0.3)], -30.0, 0.0);
            eq
        };
        let xf: Vec<f32> = x.iter().map(|v| *v as f32 / 32768.0).collect();
        let mut want = vec![0f32; N];
        chain().process_f32(&xf, &mut want);
        let mut dithered = vec![0i16; N];
        chain().process_i16(&x, &mut dithered);
        let mut rounded = vec![0i16; N];
        chain().run(&x, &mut rounded, |v| v as f64 / I16_SCALE, |y, _| (y * I16_SCALE).round() as i16, 1.0);
        // Skip the filter's settling.
        let tail = |s: &[f64]| s[N / 4..].to_vec();
        let want = tail(&want.iter().map(|v| *v as f64).collect::<Vec<_>>());
        let level = 10.0 * bin_power(&want, 3 * K / 4).log10();
        // Peak A has A²/16 in its Hann-windowed bin: -100 dBFS is -112 dB.
        assert!((level + 112.0).abs() < 1.5, "the tone the chain makes is -100 dBFS peak: {level:.1} dB in its bin");
        let got = |y: &[i16]| tail(&y.iter().map(|v| *v as f64 / 32768.0).collect::<Vec<_>>());
        let (d, r) = (got(&dithered), got(&rounded));
        let db = |x: &[f64], k: usize| 10.0 * bin_power(x, k).log10();
        // Noise floor: median bin between harmonics.
        let mut floor: Vec<f64> = (3 * K / 4 + 40..4 * 3 * K / 4).step_by(7).map(|k| db(&d, k)).collect();
        floor.sort_by(f64::total_cmp);
        let floor = floor[floor.len() / 2];
        let tone = db(&d, 3 * K / 4);
        assert!((tone - level).abs() < 1.5, "dithered, the tone keeps its level: {tone:.1} against {level:.1}");
        assert!(tone > floor + 25.0, "and stands clear of the noise: {tone:.1} over {floor:.1}");
        for h in [3, 5, 7] {
            assert!(db(&d, h * 3 * K / 4) < floor + 10.0, "no harmonic {h} of the steps: {:.1} over a floor of {floor:.1}", db(&d, h * 3 * K / 4));
        }
        assert!(r.iter().all(|v| *v == 0.0) || db(&r, 3 * K / 4) < level - 6.0, "plain rounding lost it");
        // At -90 dBFS rounding makes step harmonics; dither does not.
        let loud: Vec<i16> = x.iter().map(|v| v.saturating_mul(3)).collect();
        let mut stepped = vec![0i16; N];
        chain().run(&loud, &mut stepped, |v| v as f64 / I16_SCALE, |y, _| (y * I16_SCALE).round() as i16, 1.0);
        let mut clean = vec![0i16; N];
        chain().process_i16(&loud, &mut clean);
        let (s, c) = (got(&stepped), got(&clean));
        let worst = |x: &[f64]| [3, 5, 7].iter().map(|h| db(x, h * 3 * K / 4)).fold(f64::MIN, f64::max);
        assert!(worst(&s) > floor + 20.0, "rounded: a harmonic of the steps");
        assert!(worst(&c) < floor + 10.0, "dithered: none");
    }

    #[test]
    fn flat_16_bit_chain_is_bit_exact() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 0.0);
        let x: Vec<i16> = (0..9600).map(|i| ((i * 7919) % 65536) as i16).collect();
        let mut y = vec![0i16; x.len()];
        eq.process_i16(&x, &mut y);
        assert_eq!(x, y);
    }

    #[test]
    fn presets_are_sane() {
        let presets = eq_presets();
        assert!(presets.iter().any(|p| p.kind == PresetKind::Flat && p.bands.is_empty()));
        let kinds: std::collections::HashSet<PresetKind> = presets.iter().map(|p| p.kind).collect();
        assert_eq!(kinds.len(), presets.len(), "each kind once");

        for p in &presets {
            assert!((-12.0..=0.0).contains(&p.preamp_db), "{:?} pre-amp {}", p.kind, p.preamp_db);
            let boost = p.bands.iter().fold(0f32, |m, b| m.max(b.gain_db));
            assert!(p.preamp_db <= -boost, "{:?} boosts {boost} dB but only pays back {}", p.kind, p.preamp_db);
            let mut eq = Equalizer::new(48000, 2);
            let bands: Vec<Band> = p.bands.iter().map(Band::from).collect();
            for band in &bands {
                assert!((20.0..=20000.0).contains(&band.freq), "{:?} band at {} Hz", p.kind, band.freq);
                assert!(band.q > 0.0 && band.q <= 10.0, "{:?} band Q {}", p.kind, band.q);
                assert!(band.gain_db.abs() <= 12.0, "{:?} band gain {}", p.kind, band.gain_db);
            }
            eq.configure(&bands, p.preamp_db as f64, 0.0);
            assert_eq!(eq.is_identity(), p.bands.is_empty() && p.preamp_db == 0.0, "{:?}", p.kind);
            for f in [30.0, 60.0, 100.0, 220.0, 440.0, 1000.0, 2500.0, 4000.0, 8000.0, 12000.0] {
                let g = gain_at(&mut eq, f);
                assert!(g.is_finite() && g < 3.5, "{:?} is {g} dB at {f} Hz", p.kind);
            }
        }
    }

    #[test]
    fn graphic_equalizer_hits_sliders() {
        let sliders = [6.0, 6.0, 3.0, 0.0, -4.0, -4.0, 0.0, 3.0, 6.0, 9.0];
        let mut eq = Equalizer::new(48000, 1);
        eq.configure_graphic(&sliders, 0.0, 0.0);
        for (f, want) in crate::graphic::centres(10).into_iter().zip(sliders) {
            if f < 60.0 {
                continue; // 1 s is too short to measure 31 Hz precisely
            }
            let got = gain_at(&mut eq, f);
            assert!((got - want).abs() < 0.35, "{f} Hz: {got} dB, the slider says {want}");
        }
        let five = [4.0, -3.0, 2.0, 5.0, -2.0];
        eq.configure_graphic(&five, 0.0, 0.0);
        for (f, want) in crate::graphic::centres(5).into_iter().zip(five) {
            let got = gain_at(&mut eq, f);
            assert!((got - want).abs() < 0.35, "five bands, {f} Hz: {got} dB, the slider says {want}");
        }
        eq.configure_graphic(&[0.0; 10], 0.0, 0.0);
        let (x, mut y) = (vec![0f32; 960], vec![0f32; 960]);
        eq.process_f32(&x, &mut y);
        assert!(eq.is_identity(), "flat sliders cost nothing");
        eq.configure_graphic(&[4.0; 7], 0.0, 0.0);
        eq.process_f32(&x, &mut y);
        assert!(eq.is_identity(), "a slider count that is no layout plays nothing");
    }

    #[test]
    fn effects_off_cost_nothing() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_effects(&Effects::default());
        assert!(eq.is_identity());
        assert!(!Effects::default().on() && !Effects::default().guard());
        eq.configure_effects(&Effects { virtualizer: 0.0, bass_boost_db: 0.01, ..Effects::default() });
        assert!(eq.is_identity(), "a bass boost too small to hear is none");
        // A mono stream has no width to work on.
        let mut mono = Equalizer::new(48000, 1);
        mono.configure_effects(&Effects { virtualizer: 1.0, ..Effects::default() });
        assert!(mono.is_identity());
    }

    #[test]
    fn bass_boost() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure_effects(&Effects { bass_boost_db: 9.0, ..Effects::default() });
        let low = gain_at(&mut eq, 40.0);
        assert!((low - 9.0).abs() < 1.0, "40 Hz: {low}");
        assert!((gain_at(&mut eq, 100.0) - 4.5).abs() < 1.0, "half at the corner");
        assert!(gain_at(&mut eq, 1000.0).abs() < 0.3 && gain_at(&mut eq, 5000.0).abs() < 0.1);
        eq.configure_effects(&Effects { bass_boost_db: 99.0, ..Effects::default() });
        assert!(gain_at(&mut eq, 40.0) < BASS_BOOST_MAX_DB + 0.5, "held to its range");
    }

    #[test]
    fn volume_boost_with_limiter() {
        let fx = Effects { boost_db: 6.0, ..Effects::default() };
        assert!(fx.guard());
        let mut eq = Equalizer::new(48000, 2);
        eq.configure_effects(&fx);
        let (l, r) = stereo_gain_at(&mut eq, 1000.0);
        assert!((l - 6.0).abs() < 0.01 && (r - 6.0).abs() < 0.01, "{l} / {r}");
        eq.configure_output(0.0, false, -1.0, 120.0, 5.0);
        let x: Vec<f32> = tone_at(220.0, 0.9).iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.reset();
        eq.process_f32(&x, &mut y);
        assert!(peak(&y) <= 10f64.powf(-1.0 / 20.0) * 1.001, "peak {}", peak(&y));
        assert!(eq.gain_reduction_db() > 4.0);
    }

    #[test]
    fn compressor_in_chain() {
        let c = crate::compressor::CompressorPreset::Balanced.settings();
        let mut eq = Equalizer::new(48000, 2);
        eq.configure_effects(&Effects { compressor: Some(crate::compressor::CompressorSettings { makeup_db: 0.0, ..c }), ..Effects::default() });
        let quiet = stereo_gain_at(&mut eq, 440.0).0; // the tone is -12 dBFS: 8 dB over, in the knee's reach
        assert!(eq.compression_db() > 2.0, "it works: {}", eq.compression_db());
        assert!(quiet < -2.0, "{quiet}");
        eq.configure_effects(&Effects { compressor: Some(crate::compressor::CompressorSettings { threshold_db: -6.0, ..c }), ..Effects::default() });
        assert!(!eq.is_identity());
        eq.configure_effects(&Effects::default());
        let (x, mut y) = (vec![0f32; 960], vec![0f32; 960]);
        eq.process_f32(&x, &mut y);
        assert!(eq.is_identity() && eq.compression_db() == 0.0, "off again is gone");
    }

    #[test]
    fn expander_in_chain() {
        let x = crate::compressor::ExpanderSettings { threshold_db: -40.0, ratio: 4.0, attack_ms: 2.0, release_ms: 50.0 };
        let mut eq = Equalizer::new(48000, 2);
        eq.configure_effects(&Effects { expander: Some(x), ..Effects::default() });
        assert!(!eq.is_identity() && Effects { expander: Some(x), ..Effects::default() }.on());
        assert!(!Effects { expander: Some(x), ..Effects::default() }.guard(), "it never adds level");
        let m = tone(440.0);
        let loud: Vec<f32> = m.iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; loud.len()];
        eq.process_f32(&loud, &mut y);
        assert_eq!(loud[9600..], y[9600..], "above the threshold, bit for bit");
        let hum: Vec<f32> = tone_at(60.0, 0.001).iter().flat_map(|s| [*s, *s]).collect();
        eq.reset();
        eq.process_f32(&hum, &mut y);
        let db = 20.0 * (rms(&y[48000..]) / rms(&hum[48000..])).log10();
        assert!((db + 60.0).abs() < 3.0, "20 dB under at 4:1 is 80 under: {db} dB");
        eq.configure_effects(&Effects::default());
        let (z, mut w) = (vec![0f32; 960], vec![0f32; 960]);
        eq.process_f32(&z, &mut w);
        assert!(eq.is_identity(), "off again is gone");
    }

    #[test]
    fn loudness_follows_volume() {
        use crate::contour::Loudness;
        let mut eq = Equalizer::new(48000, 1);
        let at = |v: f64| Effects { loudness: Some(Loudness { reference_phon: 80.0, volume_db: v }), ..Effects::default() };
        eq.configure_effects(&at(0.0));
        assert!(eq.is_identity(), "all the way up: nothing to make up, nothing in the samples' path");
        assert!(at(0.0).on() && !at(-30.0).guard(), "on, and its own pre-gain keeps it from clipping");
        eq.configure_effects(&at(-30.0));
        let tilt = |eq: &mut Equalizer| gain_at(eq, 60.0) - gain_at(eq, 1000.0);
        let quiet = tilt(&mut eq);
        let want = crate::contour::compensation_db(50.0, 80.0, 60.0);
        assert!((quiet - want).abs() < 1.5, "60 Hz against 1 kHz: {quiet} dB, the contours say {want}");
        assert!(gain_at(&mut eq, 60.0) <= 0.05, "and nothing goes over full scale");
        eq.configure_effects(&at(-10.0));
        let louder = tilt(&mut eq);
        assert!(louder > 0.5 && louder < quiet, "turned up, less of it: {louder} dB");
        eq.configure_effects(&Effects::default());
        let (z, mut w) = (vec![0f32; 960], vec![0f32; 960]);
        eq.process_f32(&z, &mut w);
        assert!(eq.is_identity(), "off again is gone");
    }

    #[test]
    fn virtualizer_passes_centred_sound() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure_effects(&Effects { virtualizer: 1.0, ..Effects::default() });
        assert!(!eq.is_identity());
        let m = tone(1000.0);
        let x: Vec<f32> = m.iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        assert_eq!(x, y, "a centred tone passes as it came");
    }

    /// `EqKind` ordinals cross JNI as dsp codes.
    #[test]
    fn eq_kind_ordinals_match_codes() {
        for (kind, code) in [
            (EqKind::Peaking, PEAKING),
            (EqKind::LowShelf, LOW_SHELF),
            (EqKind::HighShelf, HIGH_SHELF),
            (EqKind::LowPass, LOW_PASS),
            (EqKind::HighPass, HIGH_PASS),
            (EqKind::BandPass, BAND_PASS),
            (EqKind::Notch, NOTCH),
            (EqKind::AllPass, ALL_PASS),
            (EqKind::LowShelfSlope, LOW_SHELF_SLOPE),
            (EqKind::HighShelfSlope, HIGH_SHELF_SLOPE),
        ] {
            assert_eq!(kind as i32, code, "{kind:?}");
        }
    }
}
