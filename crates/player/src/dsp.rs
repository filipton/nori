//! The sample-domain chain (pre-amp, parametric or graphic equalizer, bass boost, compressor, mono, virtualizer,
//! crossfeed, balance, volume boost, limiter), run on every buffer by the player. It runs on the playback thread every
//! few milliseconds and must not allocate, copy or serialise anything.
//!
//! Chain order, and why: pre-amp, the equalizer and the bass boost come first because everything after them is a mix
//! or a level decision that wants to see the tone the user actually chose. The compressor follows, so it evens out
//! the music as it will sound, boosted bass included. Mono collapses the stage before the virtualizer and the
//! crossfeed, so they model one loudspeaker pair rather than two already-mixed ears. Balance sits after the
//! crossfeed, otherwise the crossfeed would leak the louder side back into the quieter ear and undo half of it. The
//! volume boost is the last gain, and the limiter is last of all, so it sees every boost (pre-amp, EQ, bass boost,
//! the compressor's make-up, the volume boost, ReplayGain that the player applied upstream, the +3 dB that centred
//! material gains from the mono sum) and is the only stage that can decide what leaves the chain.


use crate::compressor::{Compressor, CompressorSettings};
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
/// Shelves whose `q` is the RBJ slope S (1 is the steepest slope that does not ripple) instead of a Q.
pub const LOW_SHELF_SLOPE: i32 = 8;
pub const HIGH_SHELF_SLOPE: i32 = 9;

pub const CH_BOTH: i32 = 0;
pub const CH_LEFT: i32 = 1;
pub const CH_RIGHT: i32 = 2;

const MAX_CHANNELS: usize = 8;
/// A balance of ±1 mutes one side; in between the quiet side is trimmed linearly in dB, which is what a slider feels like.
const BALANCE_RANGE_DB: f64 = 24.0;
/// Sum and then -3.01 dB: uncorrelated material keeps its level. Centred material gains 3 dB, which the limiter catches.
const MONO_SUM: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// The bass boost: a low shelf with its half-gain point here, as gentle a slope as still sounds like bass
/// rather than a tilt, so it lifts kick and bass lines and leaves voices alone.
const BASS_HZ: f64 = 100.0;
const BASS_SLOPE: f64 = 0.8;
/// How far the bass boost and the volume boost go, dB.
pub const BASS_BOOST_MAX_DB: f64 = 12.0;
pub const VOLUME_BOOST_MAX_DB: f64 = 12.0;
/// Width of the limiter's soft knee, centred on the threshold. Below `threshold - KNEE_DB / 2` the limiter is bit-exact.
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

/// Anything but NaN and the infinities, which would otherwise poison the filter state for good.
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
    /// Bit per channel this filter runs on, so per-side bands cost a test rather than a second filter list.
    chans: u8,
}

impl Biquad {
    /// RBJ cookbook filters.
    fn new(rate: f64, band: &Band) -> Self {
        let a = 10f64.powf(band.gain_db / 40.0);
        let w = 2.0 * std::f64::consts::PI * band.freq / rate;
        let (sin, cos) = (w.sin(), w.cos());
        let q = band.q.clamp(0.05, 40.0);
        let alpha = sin / (2.0 * q);
        // The shelf slope form: S = 1 is as steep as a shelf gets without a peak at the corner.
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
            // Constant 0 dB peak gain, so Q only sets the width and never the level.
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

/// The normalised coefficients `[b0, b1, b2, a1, a2]` the chain runs `band` with at `rate`, for code that
/// designs filters (`eqfit`) and has to see exactly the response the chain will play.
pub fn band_coefficients(rate: f64, band: &Band) -> [f64; 5] {
    let b = Biquad::new(rate, band);
    [b.b0, b.b1, b.b2, b.a1, b.a2]
}

/// True for the kinds whose `gain_db` means something; the others are shapes and stay in the chain at any gain.
pub fn uses_gain(kind: i32) -> bool {
    matches!(kind, PEAKING | LOW_SHELF | HIGH_SHELF | LOW_SHELF_SLOPE | HIGH_SHELF_SLOPE)
}

/// Headphone crossfeed after Boris Mikhaylov's bs2b: each ear also gets the other channel, low-passed and
/// attenuated, the way a loudspeaker would reach it. Stereo only.
#[derive(Clone, Copy, Default)]
struct Crossfeed {
    a0_lo: f64,
    b1_lo: f64,
    a0_hi: f64,
    a1_hi: f64,
    b1_hi: f64,
    gain: f64,
    lo: [f64; 2],
    hi: [f64; 2],
    last: [f64; 2],
}

impl Crossfeed {
    fn new(rate: f64, level_db: f64, cut_hz: f64) -> Self {
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
            ..Default::default()
        }
    }

    /// The same filters, whatever their memories hold.
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

/// Look-ahead peak limiter. The gain follower reads the frame going in while the output reads the delay line, so the
/// gain has the whole look-ahead window to arrive before the peak does. Below the knee the gain is *exactly* 1.0 and
/// the samples come back out of the delay line untouched, which is what makes turning the limiter on free: a boost
/// that never reaches the threshold costs a few ms of delay and nothing else.
#[derive(Clone)]
struct Limiter {
    /// `frames * channels`, a ring; allocated here, never in the per-buffer path.
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
    /// Peak hold, so the envelope cannot start releasing while the peak it describes is still inside the delay line.
    env: f64,
    hold: usize,
    gain: f64,
    /// Smallest gain in the buffer just processed; the meter the UI polls.
    meter: f64,
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

    /// Coefficients only, so moving a slider re-tunes the curve without dropping the delay line or the current gain.
    fn tune(&mut self, rate: f64, thresh_db: f64, release_ms: f64) {
        let thresh_db = finite(thresh_db, -1.0).clamp(-40.0, 0.0);
        let per_sample = (-1.0 / (finite(release_ms, 100.0).clamp(5.0, 2000.0) / 1000.0 * rate)).exp();
        self.thresh_db = thresh_db;
        self.thresh_lin = 10f64.powf(thresh_db / 20.0);
        self.knee_start = 10f64.powf((thresh_db - KNEE_DB / 2.0) / 20.0);
        self.knee_end = 10f64.powf((thresh_db + KNEE_DB / 2.0) / 20.0);
        // Converge to within 0.1 % of the target inside the look-ahead window, so peaks arrive at the right gain.
        self.attack = 1.0 - 0.001f64.powf(1.0 / self.frames as f64);
        self.release = 1.0 - per_sample;
        self.decay = per_sample;
    }

    /// The static curve: unity below the knee, a quadratic through it, a hard ceiling above it.
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

    #[inline]
    fn frame(&mut self, x: &mut [f64]) {
        let mut peak = 0.0f64;
        for v in x.iter() {
            peak = peak.max(v.abs());
        }
        if peak >= self.env {
            (self.env, self.hold) = (peak, self.frames);
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.env *= self.decay;
        }
        let want = self.curve(self.env);
        self.gain += (want - self.gain) * if want < self.gain { self.attack } else { self.release };
        if self.gain > 1.0 - 1e-7 {
            self.gain = 1.0; // snap, so the chain goes back to bit-exact once it has released
        }
        let slot = self.pos * self.channels;
        // The follower only gets within 0.1 % of its target inside the look-ahead, and on a peak far over
        // the threshold that last sliver of gain is enough to cross the ceiling (by up to 0.13 dB, past full
        // scale at a 0 dB threshold). The frame leaving now is known, so its gain is also held to the curve
        // for its own peak: the ceiling is a ceiling. Below the knee this never engages.
        let leaving = self.delay[slot..slot + self.channels].iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let gain = if leaving * self.gain > self.knee_start { self.gain.min(self.curve(leaving)) } else { self.gain };
        for (c, v) in x.iter_mut().enumerate() {
            let out = self.delay[slot + c];
            self.delay[slot + c] = *v;
            *v = out * gain;
        }
        self.pos = if self.pos + 1 == self.frames { 0 } else { self.pos + 1 };
        self.meter = self.meter.min(self.gain);
    }

    fn reset(&mut self) {
        self.delay.fill(0.0);
        (self.pos, self.env, self.hold, self.gain, self.meter) = (0, 0.0, 0, 1.0, 1.0);
    }
}

/// How long the output takes to fade from the chain as it was to the chain as it is, after a change.
const CHANGE_FADE_MS: f64 = 10.0;

/// What the chain does to a frame, with its own filter memories. There are two of these for a moment
/// after the settings change while music plays; see [`Equalizer`].
#[derive(Clone)]
struct Stages {
    channels: usize,
    /// Only the bands that do something, so a flat band costs nothing.
    filters: Vec<Biquad>,
    /// Transposed direct form II state, per filter per channel.
    state: Vec<[[f64; 2]; MAX_CHANNELS]>,
    preamp: f64,
    /// The bass boost's shelf and its memories, per channel.
    bass: Option<(Biquad, [[f64; 2]; MAX_CHANNELS])>,
    compressor: Option<Compressor>,
    virtualizer: Option<Virtualizer>,
    crossfeed: Option<Crossfeed>,
    mono: bool,
    balance: (f64, f64),
    /// The volume boost, linear; 1 is none.
    boost: f64,
    limiter: Option<Limiter>,
}

impl Stages {
    fn is_identity(&self) -> bool {
        self.filters.is_empty()
            && self.bass.is_none()
            && self.compressor.is_none()
            && self.virtualizer.is_none()
            && self.boost == 1.0
            && self.crossfeed.is_none()
            && self.limiter.is_none()
            && !self.mono
            && self.balance == (1.0, 1.0)
            && (self.preamp - 1.0).abs() < 1e-6
    }

    /// Whether the two would sound the same. A limiter retuned is not a new sound: its gain glides to
    /// the new curve by itself.
    fn sounds_like(&self, o: &Stages) -> bool {
        self.filters == o.filters
            && self.preamp == o.preamp
            && self.bass.map(|b| b.0) == o.bass.map(|b| b.0)
            && self.compressor.as_ref().map(Compressor::settings) == o.compressor.as_ref().map(Compressor::settings)
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
            if f.chans & bit == 0 {
                continue;
            }
            let s = &mut st[ch];
            let y = f.b0 * x + s[0];
            s[0] = f.b1 * x - f.a1 * y + s[1];
            s[1] = f.b2 * x - f.a2 * y;
            x = y;
        }
        if let Some((f, st)) = self.bass.as_mut() {
            let s = &mut st[ch];
            let y = f.b0 * x + s[0];
            s[0] = f.b1 * x - f.a1 * y + s[1];
            s[1] = f.b2 * x - f.a2 * y;
            x = y;
        }
        x
    }

    /// Everything after the equalizer, on one frame: compressor, mono, virtualizer, crossfeed, balance,
    /// volume boost, limiter.
    #[inline]
    fn output_stage(&mut self, f: &mut [f64]) {
        if let Some(c) = self.compressor.as_mut() {
            c.frame(f);
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

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = [[0.0; 2]; MAX_CHANNELS]);
        if let Some((_, st)) = self.bass.as_mut() {
            *st = [[0.0; 2]; MAX_CHANNELS];
        }
        if let Some(c) = self.compressor.as_mut() {
            c.reset();
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

/// The whole sample-domain chain: pre-amp, parametric or graphic equalizer, bass boost, compressor, mono,
/// virtualizer, crossfeed, balance, volume boost, limiter.
///
/// A change to the settings while music plays does not switch from one sample to the next: that was a
/// click every time, whether a band, mono, balance or crossfeed moved, and the limiter - whose look-ahead
/// is a delay - cut five milliseconds out of the song when it went off and put five of silence in when
/// it came on. The chain as it was keeps running beside the new one and the output fades over to it in
/// [`CHANGE_FADE_MS`]; a new limiter fills its look-ahead first. Nothing fades before the first sample
/// or after a reset, where there is nothing to be heard switching.
pub struct Equalizer {
    rate: f64,
    channels: usize,
    now: Stages,
    /// The chain before the last change, running beside `now` while the output fades from it.
    was: Stages,
    /// Frames into that fade, negative while a new limiter's look-ahead fills; `None` when not fading.
    fade: Option<i64>,
    fade_len: i64,
    /// Samples have gone through since the last reset, so a change from here on would be heard.
    live: bool,
    /// What 16-bit samples the chain changed are rounded back through ([`Equalizer::process_i16`]).
    dither: Dither,
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
            compressor: None,
            virtualizer: None,
            crossfeed: None,
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
        }
    }

    /// Applies a change to the chain; heard, it fades in (see [`Equalizer`]). A change during a fade
    /// carries on fading from the same old chain.
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
        // Behind a limiter the change reaches the output only once its look-ahead has passed (a new
        // limiter's is empty until then): the fade waits for it, or it would still be a switch.
        if delay as usize != lookahead.unwrap_or(0) && delay > 0 || self.fade.is_none() && !self.now.sounds_like(&self.was) {
            self.fade = Some(-delay);
        }
    }

    /// `crossfeed_db` 0 turns crossfeed off; typical values are 3 to 6.
    pub fn configure(&mut self, bands: &[Band], preamp_db: f64, crossfeed_db: f64) {
        self.change(|s, rate| {
            s.filters.clear();
            for b in bands {
                // A side band needs a side: on a mono stream there is nothing to route, so only `CH_BOTH` survives.
                let routable = b.channel == CH_BOTH || (s.channels >= 2 && (b.channel == CH_LEFT || b.channel == CH_RIGHT));
                let shaped = !uses_gain(b.kind) || b.gain_db.abs() >= 0.05;
                if routable && shaped && b.freq > 0.0 && b.freq < rate / 2.0 && matches!(b.kind, PEAKING..=HIGH_SHELF_SLOPE) {
                    s.filters.push(Biquad::new(rate, &Band { gain_db: b.gain_db.clamp(-24.0, 24.0), ..*b }));
                }
            }
            Self::rest(s, rate, preamp_db, crossfeed_db);
        });
    }

    /// The graphic equalizer in place of the parametric one: `sliders` are the response in dB at the
    /// centres of one of `graphic::LAYOUTS`, and the filters that draw it are designed here, for this
    /// stream's rate (`graphic::design`), once per change.
    pub fn configure_graphic(&mut self, sliders: &[f64], preamp_db: f64, crossfeed_db: f64) {
        self.change(|s, rate| {
            s.filters.clear();
            // Filter gains past a band's ±24 dB are how the design makes neighbours differ; they are its own
            // and held to its own limit.
            s.filters.extend(crate::graphic::design(rate, sliders).iter().map(|b| Biquad::new(rate, b)));
            Self::rest(s, rate, preamp_db, crossfeed_db);
        });
    }

    fn rest(s: &mut Stages, rate: f64, preamp_db: f64, crossfeed_db: f64) {
        s.state.resize(s.filters.len(), [[0.0; 2]; MAX_CHANNELS]);
        s.preamp = 10f64.powf(finite(preamp_db, 0.0).clamp(-30.0, 12.0) / 20.0);
        s.crossfeed = (crossfeed_db > 0.0 && s.channels == 2).then(|| Crossfeed::new(rate, crossfeed_db.clamp(1.0, 15.0), 700.0));
    }

    /// The effects: bass boost, compressor, virtualizer (stereo only) and volume boost, each off at 0 or
    /// `None`. A compressor or virtualizer already running is retuned and keeps its state. The volume
    /// boost and the other boosts want the limiter on behind them (`Effects::guard`); that is the
    /// caller's, through [`Equalizer::configure_output`].
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

    /// The output stage. `balance` is -1 (left) to 1 (right); mono and balance are stereo ideas and are ignored
    /// otherwise. `lookahead_ms` at or below 0 turns the limiter off, which is also the only way to get its delay back.
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

    /// True when the chain would not change a single sample.
    pub fn is_identity(&self) -> bool {
        self.fade.is_none() && self.now.is_identity()
    }

    /// How many frames the chain holds back: the limiter's look-ahead, 0 without one. At the end of a
    /// stream that many frames of silence pushed through bring the last of the music out.
    pub fn delay_frames(&self) -> usize {
        self.now.limiter.as_ref().map_or(0, |l| l.frames)
    }

    /// Peak gain reduction in the buffer just processed, for the UI meter. Zero when the limiter is off or idle.
    pub fn gain_reduction_db(&self) -> f32 {
        self.now.limiter.as_ref().map_or(0.0, |l| (-20.0 * l.meter.log10()) as f32)
    }

    /// The compressor's largest gain reduction in the buffer just processed, dB; 0 when it is off.
    pub fn compression_db(&self) -> f32 {
        self.now.compressor.as_ref().map_or(0.0, |c| c.meter_db as f32)
    }

    /// One generic loop; `load` and `store` (told the channel) are the only things that differ between
    /// sample formats.
    #[inline]
    fn run<T: Copy>(&mut self, input: &[T], output: &mut [T], load: impl Fn(T) -> f64, mut store: impl FnMut(f64, usize) -> T) {
        let len = input.len().min(output.len());
        let (input, output) = (&input[..len], &mut output[..len]);
        self.live |= len > 0;
        if self.is_identity() {
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
        let mut frame = [0f64; MAX_CHANNELS];
        let mut old = [0f64; MAX_CHANNELS];
        for (x, y) in input.chunks_exact(n).zip(output.chunks_exact_mut(n)) {
            for (c, v) in x.iter().enumerate() {
                frame[c] = load(*v);
            }
            match self.fade {
                None => self.now.frame(&mut frame[..n]),
                Some(at) => {
                    old[..n].copy_from_slice(&frame[..n]);
                    self.was.frame(&mut old[..n]);
                    self.now.frame(&mut frame[..n]);
                    // Raised cosine: no corner at either end of the fade.
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
        // media3 hands over whole frames; a ragged tail would still have to come out somewhere.
        let tail = len - len % n;
        output[tail..].copy_from_slice(&input[tail..]);
    }

    /// 16-bit samples are brought to the same scale as float ones, where 1.0 is full scale, before anything touches
    /// them. The filters would not notice either way - they are linear - but the limiter compares against a ceiling
    /// in full-scale units, and fed raw integers it took every sample for thirty thousand times too loud and turned
    /// the music down by some 91 dB: silence, on the default 16-bit path, whenever the limiter was on. Dividing and
    /// multiplying by a power of two is exact in f64, so a chain that changes nothing still changes nothing.
    ///
    /// What the chain changed goes back to 16 bits through TPDF dither ([`crate::dither`]), not plain
    /// rounding: the error is noise 96 dB down rather than distortion that follows the music. A chain that
    /// changes nothing hands its input back as it was, never dithered.
    pub fn process_i16(&mut self, input: &[i16], output: &mut [i16]) {
        let mut d = self.dither;
        // Mono's two channels are one sound: they take one noise, and stay one.
        if self.now.mono {
            self.run(input, output, |x| x as f64 / I16_SCALE, |y, c| d.to_i16_linked(c, y));
        } else {
            self.run(input, output, |x| x as f64 / I16_SCALE, |y, c| d.to_i16(c, y));
        }
        self.dither = d;
    }

    /// Float samples in and out; the chain runs in f64 between, and a float keeps its 24 bits of mantissa,
    /// so a 24-bit song comes through to 24-bit accuracy.
    pub fn process_f32(&mut self, input: &[f32], output: &mut [f32]) {
        self.run(input, output, |x| x as f64, |y, _| y as f32);
    }

    /// A new stream (a seek, a flush): the memories go, and so does any fade, since nothing is playing
    /// through the change.
    pub fn reset(&mut self) {
        self.now.reset();
        self.fade = None;
        self.live = false;
        self.dither.reset();
    }
}

/// The effects besides the equalizer, each off at 0 or `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Effects {
    /// The low shelf's gain, 0 to [`BASS_BOOST_MAX_DB`].
    pub bass_boost_db: f64,
    pub compressor: Option<CompressorSettings>,
    /// Strength, 0 to 1.
    pub virtualizer: f64,
    /// 0 to [`VOLUME_BOOST_MAX_DB`].
    pub boost_db: f64,
}

impl Effects {
    /// Whether any of them touches the samples.
    pub fn on(&self) -> bool {
        self.bass_boost_db > 0.0 || self.compressor.is_some() || self.virtualizer > 0.0 || self.boost_db > 0.0
    }

    /// Whether they add level the music did not have: the limiter then runs behind them, whether or not
    /// it was asked for, so a boost can never clip.
    pub fn guard(&self) -> bool {
        self.bass_boost_db > 0.0 || self.boost_db > 0.0 || self.compressor.is_some_and(|c| c.makeup_db > 0.0)
    }
}

/// Full scale for 16-bit samples: `i16::MIN` maps to exactly -1.0.
const I16_SCALE: f64 = 32768.0;

fn band(kind: EqKind, freq: f32, gain_db: f32, q: f32) -> EqBand {
    EqBand { kind, freq, gain_db, q }
}

/// The automatic pre-amp: as much cut as the largest boost, so a curve never lifts a full-scale
/// song into clipping. Cuts need no room.
pub fn auto_preamp_db(bands: impl IntoIterator<Item = (i32, f32)>) -> f32 {
    -bands.into_iter().filter(|&(k, _)| uses_gain(k)).map(|(_, g)| g).fold(0f32, f32::max)
}

/// The built-in curves, as data, so the UI (and the settings store) never holds a frequency of its own.
/// Every preset that boosts carries a pre-amp that pays the boost back, so a preset cannot clip on its own.
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
        // The equal-loudness smile: what quiet listening takes away at both ends.
        preset(
            PresetKind::Loudness,
            -7.0,
            vec![band(EqKind::LowShelfSlope, 80.0, 7.0, 0.8), band(EqKind::Peaking, 1000.0, -2.0, 1.0), band(EqKind::HighShelfSlope, 10000.0, 5.0, 0.8)],
        ),
        // Phone and laptop drivers: throw away what they can only rattle on, then put the body back an octave up.
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
    fn the_automatic_preamp_makes_room_for_the_largest_boost_only() {
        assert_eq!(auto_preamp_db([(PEAKING, 4.5), (LOW_SHELF, 6.0), (HIGH_PASS, 12.0)]), -6.0, "a pass filter's gain is not a boost");
        assert_eq!(auto_preamp_db([(PEAKING, -3.0)]), 0.0, "cuts need no room");
        assert_eq!(auto_preamp_db([]), 0.0);
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn tone(freq: f64) -> Vec<f32> {
        (0..48000).map(|i| (0.25 * (2.0 * std::f64::consts::PI * freq * i as f64 / 48000.0).sin()) as f32).collect()
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

    /// Runs a stereo tone and returns the per-channel gain in dB.
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
    fn flat_is_identity_and_a_band_moves_only_its_neighbourhood() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[], 0.0, 0.0);
        assert!(eq.is_identity());
        let x = tone(1000.0);
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        assert_eq!(x, y);

        eq.configure(&[b(PEAKING, 1000.0, -12.0, 1.41)], 0.0, 0.0);
        assert!((gain_at(&mut eq, 1000.0) + 12.0).abs() < 0.5);
        assert!(gain_at(&mut eq, 8000.0).abs() < 0.5);
    }

    #[test]
    fn shelves_and_preamp() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[b(LOW_SHELF, 200.0, 6.0, 0.71)], -3.0, 0.0);
        assert!((gain_at(&mut eq, 40.0) - 3.0).abs() < 0.5, "low shelf + preamp at 40 Hz");
        assert!((gain_at(&mut eq, 5000.0) + 3.0).abs() < 0.5, "only the preamp at 5 kHz");
        eq.configure(&[b(HIGH_SHELF, 4000.0, -6.0, 0.71)], 0.0, 0.0);
        assert!((gain_at(&mut eq, 16000.0) + 6.0).abs() < 0.6);
        assert!(gain_at(&mut eq, 200.0).abs() < 0.5);
    }

    #[test]
    fn slope_shelves_reach_their_gain_and_stay_out_of_the_other_end() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[b(LOW_SHELF_SLOPE, 250.0, 8.0, 1.0)], 0.0, 0.0);
        assert!((gain_at(&mut eq, 40.0) - 8.0).abs() < 0.6, "low slope shelf at 40 Hz: {}", gain_at(&mut eq, 40.0));
        assert!((gain_at(&mut eq, 250.0) - 4.0).abs() < 0.6, "half the gain at the corner");
        assert!(gain_at(&mut eq, 8000.0).abs() < 0.3);

        eq.configure(&[b(HIGH_SHELF_SLOPE, 3000.0, -8.0, 1.0)], 0.0, 0.0);
        assert!((gain_at(&mut eq, 16000.0) + 8.0).abs() < 0.7);
        assert!(gain_at(&mut eq, 100.0).abs() < 0.3);
    }

    #[test]
    fn pass_filters_cut_the_far_side_and_leave_the_pass_band() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[b(LOW_PASS, 1000.0, 0.0, 0.707)], 0.0, 0.0);
        assert!(gain_at(&mut eq, 100.0).abs() < 0.2, "low pass pass-band");
        assert!((gain_at(&mut eq, 1000.0) + 3.0).abs() < 0.6, "-3 dB at the corner");
        assert!(gain_at(&mut eq, 8000.0) < -15.0, "two poles, three octaves up");

        eq.configure(&[b(HIGH_PASS, 1000.0, 0.0, 0.707)], 0.0, 0.0);
        assert!(gain_at(&mut eq, 10000.0).abs() < 0.2);
        assert!((gain_at(&mut eq, 1000.0) + 3.0).abs() < 0.6);
        assert!(gain_at(&mut eq, 125.0) < -15.0);
    }

    #[test]
    fn band_pass_peaks_at_unity_and_notch_digs_a_hole() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[b(BAND_PASS, 1000.0, 0.0, 2.0)], 0.0, 0.0);
        assert!(gain_at(&mut eq, 1000.0).abs() < 0.2, "constant 0 dB peak gain");
        assert!(gain_at(&mut eq, 100.0) < -12.0 && gain_at(&mut eq, 10000.0) < -12.0);

        eq.configure(&[b(NOTCH, 1000.0, 0.0, 8.0)], 0.0, 0.0);
        assert!(gain_at(&mut eq, 1000.0) < -20.0, "notch at the centre");
        assert!(gain_at(&mut eq, 250.0).abs() < 0.4 && gain_at(&mut eq, 4000.0).abs() < 0.4);
    }

    #[test]
    fn all_pass_keeps_the_level_and_moves_the_phase() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[b(ALL_PASS, 1000.0, 0.0, 0.707)], 0.0, 0.0);
        for f in [100.0, 1000.0, 5000.0, 15000.0] {
            assert!(gain_at(&mut eq, f).abs() < 0.2, "all pass is flat at {f} Hz");
        }
        let x = tone(1000.0);
        let mut y = vec![0f32; x.len()];
        eq.reset();
        eq.process_f32(&x, &mut y);
        assert!(!eq.is_identity() && x[24000..] != y[24000..], "the phase moved even though the level did not");
    }

    #[test]
    fn bad_bands_are_ignored_rather_than_played() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(
            &[
                b(PEAKING, f64::NAN, 6.0, 1.0),
                b(PEAKING, 30000.0, 6.0, 1.0),
                b(PEAKING, -100.0, 6.0, 1.0),
                b(PEAKING, 1000.0, f64::NAN, 1.0),
                b(99, 1000.0, 6.0, 1.0),
            ],
            f64::NAN,
            0.0,
        );
        assert!(eq.is_identity(), "nothing survived, so the processor can be skipped");
    }

    #[test]
    fn a_band_can_be_routed_to_one_side_only() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[Band { channel: CH_LEFT, ..b(PEAKING, 1000.0, 12.0, 1.0) }], 0.0, 0.0);
        let (l, r) = stereo_gain_at(&mut eq, 1000.0);
        assert!((l - 12.0).abs() < 0.5 && r.abs() < 0.2, "left only: {l} / {r}");

        eq.configure(&[Band { channel: CH_RIGHT, ..b(PEAKING, 1000.0, -12.0, 1.0) }], 0.0, 0.0);
        let (l, r) = stereo_gain_at(&mut eq, 1000.0);
        assert!(l.abs() < 0.2 && (r + 12.0).abs() < 0.5, "right only: {l} / {r}");

        // A `both` band must leave centre content centred, sample for sample.
        eq.configure(&[b(HIGH_SHELF, 4000.0, 8.0, 0.71)], 0.0, 0.0);
        let m = tone(4000.0);
        let x: Vec<f32> = m.iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.reset();
        eq.process_f32(&x, &mut y);
        assert!(y.chunks_exact(2).all(|f| f[0] == f[1]), "both-channel band kept the centre centred");

        // A mono stream has no sides, so a side band is dropped instead of half-applied.
        let mut mono = Equalizer::new(48000, 1);
        mono.configure(&[Band { channel: CH_RIGHT, ..b(PEAKING, 1000.0, 12.0, 1.0) }], 0.0, 0.0);
        assert!(mono.is_identity());
    }

    #[test]
    fn balance_trims_one_side_and_mono_keeps_the_level() {
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

        // Two uncorrelated tones: the mono sum must keep the level, not lose 3 dB.
        eq.configure_output(0.0, true, 0.0, 100.0, 0.0);
        let (a, c) = (tone(440.0), tone(3700.0));
        let x: Vec<f32> = a.iter().zip(&c).flat_map(|(l, r)| [*l, *r]).collect();
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        let db = 20.0 * (rms(&y[19200..]) / rms(&x[19200..])).log10();
        assert!(db.abs() < 0.3, "mono sum moved the level by {db} dB");
        // Mono came on mid-stream, so it faded in over the first 10 ms.
        assert!(y[960..].chunks_exact(2).all(|f| f[0] == f[1]), "both channels carry the same mono signal");
    }

    #[test]
    fn crossfeed_leaks_bass_to_the_other_ear_and_keeps_mono_level() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 4.5);
        let left_only: Vec<f32> = tone(150.0).iter().flat_map(|s| [*s, 0.0]).collect();
        let mut y = vec![0f32; left_only.len()];
        eq.process_f32(&left_only, &mut y);
        let l: Vec<f32> = y.iter().step_by(2).copied().collect();
        let r: Vec<f32> = y.iter().skip(1).step_by(2).copied().collect();
        let leak = 20.0 * (rms(&r[9600..]) / rms(&l[9600..])).log10();
        assert!(leak < -2.0 && leak > -12.0, "right ear is {leak} dB below left");

        let mono: Vec<f32> = tone(150.0).iter().flat_map(|s| [*s, *s]).collect();
        eq.reset();
        eq.process_f32(&mono, &mut y);
        let db = 20.0 * (rms(&y[19200..]) / rms(&mono[19200..])).log10();
        assert!(db.abs() < 1.0, "mono level moved by {db} dB");
    }

    #[test]
    fn the_limiter_is_bit_exact_below_the_threshold() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(0.0, false, -6.0, 120.0, 5.0);
        assert!(!eq.is_identity(), "the look-ahead delay alone means the processor must run");

        let x = tone_at(1000.0, 0.25); // -12 dBFS, well under the knee
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        let d = 240; // 5 ms at 48 kHz
        assert_eq!(&y[d..], &x[..x.len() - d], "below the knee the samples come back untouched");
        assert_eq!(eq.gain_reduction_db(), 0.0);
    }

    #[test]
    fn silence_pushed_through_brings_out_what_the_limiter_held_back() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(0.0, false, -6.0, 120.0, 5.0);
        let x = tone_at(1000.0, 0.25);
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        let d = eq.delay_frames();
        assert_eq!(d, 240, "5 ms at 48 kHz");
        // The end of the stream: the last 5 ms are still inside, and silence brings them out whole.
        let mut tail = vec![0f32; d];
        eq.process_f32(&vec![0f32; d], &mut tail);
        assert_eq!(&tail[..], &x[x.len() - d..]);
        eq.configure_output(0.0, false, 0.0, 120.0, 0.0);
        assert_eq!(eq.delay_frames(), 0, "no limiter, nothing held");
    }

    #[test]
    fn retuning_the_limiter_does_not_break_the_stream() {
        let mut eq = Equalizer::new(48000, 1);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(0.0, false, -6.0, 120.0, 5.0);
        let x = tone_at(1000.0, 0.25);
        let (head, tail) = x.split_at(24000);
        let (mut a, mut b) = (vec![0f32; head.len()], vec![0f32; tail.len()]);
        eq.process_f32(head, &mut a);
        eq.configure_output(0.0, false, -3.0, 300.0, 5.0); // a slider moved mid-track
        eq.process_f32(tail, &mut b);
        let joined: Vec<f32> = a.into_iter().chain(b).collect();
        let d = 240;
        assert_eq!(&joined[d..], &x[..x.len() - d], "the delay line survived the new settings");
    }

    #[test]
    fn the_limiter_holds_the_ceiling_and_reports_the_reduction() {
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

        // A quiet buffer after the release has run its course reads back as no reduction at all.
        eq.reset();
        let quiet = tone_at(220.0, 0.1);
        let x: Vec<f32> = quiet.iter().flat_map(|s| [*s, *s]).collect();
        eq.process_f32(&x, &mut y);
        assert_eq!(eq.gain_reduction_db(), 0.0);
    }

    /// The path the phone uses by default. Every other limiter test feeds floats, which is how 16-bit audio being
    /// turned down by 91 dB - silence - went unnoticed: the float path was always scaled right.
    #[test]
    fn the_limiter_on_16_bit_audio_passes_normal_music_and_only_catches_peaks() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(0.0, false, -1.0, 120.0, 5.0);
        let d = 240 * 2; // 5 ms at 48 kHz, two channels

        // -12 dBFS, far below the ceiling: must come out unchanged, only delayed.
        let quiet: Vec<i16> = tone_at(1000.0, 0.25).iter().flat_map(|s| { let v = (*s * 32767.0) as i16; [v, v] }).collect();
        let mut y = vec![0i16; quiet.len()];
        eq.process_i16(&quiet, &mut y);
        assert_eq!(&y[d..], &quiet[..quiet.len() - d], "16-bit audio below the ceiling must pass through untouched");
        assert_eq!(eq.gain_reduction_db(), 0.0, "and the meter must not claim a reduction");

        // Full scale into a -1 dB ceiling: about 1 dB of reduction, not 91.
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

    /// A tone taken down to -100 dBFS by the chain (a third of a 16-bit step) comes out of the 16-bit path as a
    /// tone at its level in a flat hiss: dithered, nothing of it is lost and no harmonic of the steps appears.
    /// Rounded plainly, the same chain gave silence or a square wave's harmonics.
    #[test]
    fn a_tone_under_one_step_survives_the_16_bit_chain_as_a_tone_in_noise() {
        const N: usize = 1 << 16;
        const K: usize = 1000; // 732 Hz at 48 kHz, whole cycles in the window
        // -46 dBFS in, and the chain takes 54 dB off: the preamp's 30 and a wide 24 dB cut at the tone.
        let x: Vec<i16> = (0..N).map(|i| ((std::f64::consts::TAU * K as f64 * i as f64 / N as f64).sin() * 10f64.powf(-46.0 / 20.0) * 32768.0).round() as i16).collect();
        let chain = || {
            let mut eq = Equalizer::new(48000, 1);
            eq.configure(&[b(PEAKING, 48000.0 * K as f64 / N as f64, -24.0, 0.3)], -30.0, 0.0);
            eq
        };
        // What the chain makes of it with nothing rounded, and through the 16-bit path, dithered and not.
        let xf: Vec<f32> = x.iter().map(|v| *v as f32 / 32768.0).collect();
        let mut want = vec![0f32; N];
        chain().process_f32(&xf, &mut want);
        let mut dithered = vec![0i16; N];
        chain().process_i16(&x, &mut dithered);
        let mut rounded = vec![0i16; N];
        chain().run(&x, &mut rounded, |v| v as f64 / I16_SCALE, |y, _| (y * I16_SCALE).round() as i16);
        // The last three quarters, past the filter's start.
        let tail = |s: &[f64]| s[N / 4..].to_vec();
        let want = tail(&want.iter().map(|v| *v as f64).collect::<Vec<_>>());
        let level = 10.0 * bin_power(&want, 3 * K / 4).log10();
        // A sine of peak A has A²/16 in its bin under a Hann window: -100 dBFS is -112 dB there.
        assert!((level + 112.0).abs() < 1.5, "the tone the chain makes is -100 dBFS peak: {level:.1} dB in its bin");
        let got = |y: &[i16]| tail(&y.iter().map(|v| *v as f64 / 32768.0).collect::<Vec<_>>());
        let (d, r) = (got(&dithered), got(&rounded));
        let db = |x: &[f64], k: usize| 10.0 * bin_power(x, k).log10();
        // The noise floor: the median bin between the harmonics.
        let mut floor: Vec<f64> = (3 * K / 4 + 40..4 * 3 * K / 4).step_by(7).map(|k| db(&d, k)).collect();
        floor.sort_by(f64::total_cmp);
        let floor = floor[floor.len() / 2];
        let tone = db(&d, 3 * K / 4);
        eprintln!("dithered: tone {tone:.1} dB (wanted {level:.1}), floor {floor:.1} dB/bin, 3rd {:.1}, 5th {:.1}", db(&d, 9 * K / 4), db(&d, 15 * K / 4));
        eprintln!("rounded:  tone {:.1} dB, 3rd {:.1}, 5th {:.1}", db(&r, 3 * K / 4), db(&r, 9 * K / 4), db(&r, 15 * K / 4));
        assert!((tone - level).abs() < 1.5, "dithered, the tone keeps its level: {tone:.1} against {level:.1}");
        assert!(tone > floor + 25.0, "and stands clear of the noise: {tone:.1} over {floor:.1}");
        for h in [3, 5, 7] {
            assert!(db(&d, h * 3 * K / 4) < floor + 10.0, "no harmonic {h} of the steps: {:.1} over a floor of {floor:.1}", db(&d, h * 3 * K / 4));
        }
        // Rounded plainly, the tone is gone (a third of a step rounds to nothing): what dither is for.
        assert!(r.iter().all(|v| *v == 0.0) || db(&r, 3 * K / 4) < level - 6.0, "plain rounding lost it");
        // At -90 dBFS (a step's worth) plain rounding makes a stepped wave, its odd harmonics well out of the noise.
        let loud: Vec<i16> = x.iter().map(|v| v.saturating_mul(3)).collect();
        let mut stepped = vec![0i16; N];
        chain().run(&loud, &mut stepped, |v| v as f64 / I16_SCALE, |y, _| (y * I16_SCALE).round() as i16);
        let mut clean = vec![0i16; N];
        chain().process_i16(&loud, &mut clean);
        let (s, c) = (got(&stepped), got(&clean));
        let worst = |x: &[f64]| [3, 5, 7].iter().map(|h| db(x, h * 3 * K / 4)).fold(f64::MIN, f64::max);
        eprintln!("-90 dBFS: rounded, the loudest odd harmonic {:.1} dB; dithered {:.1} dB", worst(&s), worst(&c));
        assert!(worst(&s) > floor + 20.0, "rounded: a harmonic of the steps");
        assert!(worst(&c) < floor + 10.0, "dithered: none");
    }

    /// What the chain costs per second of stereo music, 16-bit dithered against float, with the equalizer, a
    /// compressor and the limiter on. `cargo test --release -p nori-player --lib chain_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn chain_cost() {
        let secs = 60usize;
        let x16: Vec<i16> = (0..48000 * secs * 2).map(|i| ((i as f64 * 0.0123).sin() * 12000.0 + (i as f64 * 0.77).sin() * 3000.0) as i16).collect();
        let xf: Vec<f32> = x16.iter().map(|v| *v as f32 / 32768.0).collect();
        let make = || {
            let mut eq = Equalizer::new(48000, 2);
            eq.configure_graphic(&[3.0, 5.0, 2.0, 0.0, -2.0, -4.0, 0.0, 2.0, 4.0, 6.0], -6.0, 0.0);
            eq.configure_effects(&Effects { compressor: Some(crate::compressor::CompressorPreset::Strong.settings()), ..Default::default() });
            eq.configure_output(0.0, false, -1.0, 120.0, 5.0);
            eq
        };
        let chunk = 4096;
        let t = std::time::Instant::now();
        let mut eq = make();
        let (mut y, mut b) = (vec![0i16; chunk], vec![0u8; chunk * 2]);
        for c in x16.chunks_exact(chunk) {
            eq.process_i16(c, &mut y);
            for (d, v) in b.chunks_exact_mut(2).zip(&y) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
        let i16_ms = t.elapsed().as_secs_f64() * 1000.0 / secs as f64;
        let t = std::time::Instant::now();
        let mut eq = make();
        for c in x16.chunks_exact(chunk) {
            eq.run(c, &mut y, |x| x as f64 / I16_SCALE, |v, _| (v * I16_SCALE).round().clamp(-32768.0, 32767.0) as i16);
            for (d, v) in b.chunks_exact_mut(2).zip(&y) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
        let rounded_ms = t.elapsed().as_secs_f64() * 1000.0 / secs as f64;
        let t = std::time::Instant::now();
        let mut eq = make();
        let (mut y, mut b) = (vec![0f32; chunk], vec![0u8; chunk * 4]);
        for c in xf.chunks_exact(chunk) {
            eq.process_f32(c, &mut y);
            for (d, v) in b.chunks_exact_mut(4).zip(&y) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
        let f_ms = t.elapsed().as_secs_f64() * 1000.0 / secs as f64;
        eprintln!("per second of 48 kHz stereo: 16-bit rounded {rounded_ms:.3} ms, dithered {i16_ms:.3} ms, float {f_ms:.3} ms");
        std::hint::black_box(b);
    }

    /// A flat chain hands 16-bit samples back as they came, never dithered.
    #[test]
    fn a_flat_16_bit_chain_is_bit_exact() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 0.0);
        let x: Vec<i16> = (0..9600).map(|i| ((i * 7919) % 65536) as i32 as i16).collect();
        let mut y = vec![0i16; x.len()];
        eq.process_i16(&x, &mut y);
        assert_eq!(x, y);
    }

    #[test]
    fn limiter_input_is_clamped_rather_than_trusted() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure(&[], 0.0, 0.0);
        eq.configure_output(f64::NAN, false, f64::NAN, -5.0, f64::INFINITY);
        let x: Vec<f32> = tone(1000.0).iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        assert!(y.iter().all(|v| v.is_finite()), "bad settings must not poison the output");
    }

    #[test]
    fn presets_are_sane_and_flat_really_is_flat() {
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
            // With its own pre-amp a preset must stay near unity everywhere, so picking one cannot clip on its own.
            for f in [30.0, 60.0, 100.0, 220.0, 440.0, 1000.0, 2500.0, 4000.0, 8000.0, 12000.0] {
                let g = gain_at(&mut eq, f);
                assert!(g.is_finite() && g < 3.5, "{:?} is {g} dB at {f} Hz", p.kind);
            }
        }
    }

    /// The graphic equalizer as the chain plays it, measured with tones: the level at each band's centre is
    /// the slider's.
    #[test]
    fn the_graphic_equalizer_plays_what_the_sliders_say() {
        let sliders = [6.0, 6.0, 3.0, 0.0, -4.0, -4.0, 0.0, 3.0, 6.0, 9.0];
        let mut eq = Equalizer::new(48000, 1);
        eq.configure_graphic(&sliders, 0.0, 0.0);
        for (f, want) in crate::graphic::centres(10).into_iter().zip(sliders) {
            if f < 60.0 {
                continue; // a second of tone is too short to measure 31 Hz to a tenth of a dB
            }
            let got = gain_at(&mut eq, f);
            assert!((got - want).abs() < 0.35, "{f} Hz: {got} dB, the slider says {want}");
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
    fn the_bass_boost_lifts_the_bass_and_leaves_the_voice() {
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
    fn the_volume_boost_is_louder_and_the_limiter_holds_it() {
        let fx = Effects { boost_db: 6.0, ..Effects::default() };
        assert!(fx.guard());
        let mut eq = Equalizer::new(48000, 2);
        eq.configure_effects(&fx);
        let (l, r) = stereo_gain_at(&mut eq, 1000.0);
        assert!((l - 6.0).abs() < 0.01 && (r - 6.0).abs() < 0.01, "{l} / {r}");
        // Full-scale music boosted 6 dB with the limiter behind it: nothing over the ceiling.
        eq.configure_output(0.0, false, -1.0, 120.0, 5.0);
        let x: Vec<f32> = tone_at(220.0, 0.9).iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.reset();
        eq.process_f32(&x, &mut y);
        assert!(peak(&y) <= 10f64.powf(-1.0 / 20.0) * 1.001, "peak {}", peak(&y));
        assert!(eq.gain_reduction_db() > 4.0);
    }

    #[test]
    fn the_compressor_in_the_chain_evens_out_and_meters() {
        let c = crate::compressor::CompressorPreset::Balanced.settings();
        let mut eq = Equalizer::new(48000, 2);
        eq.configure_effects(&Effects { compressor: Some(crate::compressor::CompressorSettings { makeup_db: 0.0, ..c }), ..Effects::default() });
        let quiet = stereo_gain_at(&mut eq, 440.0).0; // the tone is -12 dBFS: 8 dB over, in the knee's reach
        assert!(eq.compression_db() > 2.0, "it works: {}", eq.compression_db());
        assert!(quiet < -2.0, "{quiet}");
        // Retuned while playing: the change fades rather than clicks, and the meter follows.
        eq.configure_effects(&Effects { compressor: Some(crate::compressor::CompressorSettings { threshold_db: -6.0, ..c }), ..Effects::default() });
        assert!(!eq.is_identity());
        eq.configure_effects(&Effects::default());
        let (x, mut y) = (vec![0f32; 960], vec![0f32; 960]);
        eq.process_f32(&x, &mut y);
        assert!(eq.is_identity() && eq.compression_db() == 0.0, "off again is gone");
    }

    #[test]
    fn the_virtualizer_leaves_centred_sound_alone() {
        let mut eq = Equalizer::new(48000, 2);
        eq.configure_effects(&Effects { virtualizer: 1.0, ..Effects::default() });
        assert!(!eq.is_identity());
        let m = tone(1000.0);
        let x: Vec<f32> = m.iter().flat_map(|s| [*s, *s]).collect();
        let mut y = vec![0f32; x.len()];
        eq.process_f32(&x, &mut y);
        assert_eq!(x, y, "a centred tone passes as it came");
    }

    /// The `kind` field crossing the JNI boundary is an `EqKind` ordinal; the two lists must not drift apart.
    #[test]
    fn eq_kind_ordinals_match_the_dsp_codes() {
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
