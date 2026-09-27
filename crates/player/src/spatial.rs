//! The virtualizer: a headphone effect that moves the music out of the middle of the head, kept
//! modest. Three cheap parts, all scaled by one strength:
//!
//! - **Width.** Mid/side: the side signal above about 400 Hz is lifted (up to +3.5 dB), so what is
//!   already stereo spreads a little further; the bass stays as it was, where widening only sounds
//!   phasey.
//! - **Head.** Each ear also hears the other channel the way a loudspeaker in front would reach it:
//!   0.28 ms later (the time sound takes round a head), low-passed at 1.4 kHz (the head's shadow) and
//!   up to 12 dB down.
//! - **Room.** One early reflection per ear of the opposite channel's highs (above the shadow's corner:
//!   an echo this early in the bass is a comb, not a room), at 9.3 and 11.7 ms (different, so the two
//!   ears do not hear the same echo), up to 18 dB down.
//!
//! The head and the room are fed the *difference* between the channels, not the other channel itself:
//! each ear gets the other side's delayed signal and loses its own by the same path. For anything in
//! the middle the two cancel, so a centred voice passes sample for sample as it came (a plain delayed
//! crossfeed puts a comb into it, 2 to 3 dB deep around 1 kHz), and only what is off centre is moved.
//!
//! Per frame: a few multiply-adds and a ring buffer of 12 ms allocated when it is made. Stereo only.

/// How long the ring is: longer than the latest tap.
const RING_MS: f64 = 12.5;
const ITD_MS: f64 = 0.28;
const SHADOW_HZ: f64 = 1400.0;
const SIDE_SPLIT_HZ: f64 = 400.0;
const REFLECT_MS: [f64; 2] = [9.3, 11.7];
/// At full strength.
const SIDE_LIFT: f64 = 0.5;
const CROSS: f64 = 0.25;
const REFLECT: f64 = 0.12;

#[derive(Clone, Debug)]
pub struct Virtualizer {
    strength: f64,
    /// One-pole low-pass coefficients: `y += a (x - y)`.
    shadow_a: f64,
    side_a: f64,
    side_lift: f64,
    cross: f64,
    reflect: f64,
    itd: usize,
    taps: [usize; 2],
    /// Per frame: the difference between the channels (left minus right), low-passed and what is left
    /// above it.
    ring: Vec<[f64; 2]>,
    pos: usize,
    lp: f64,
    side_lp: f64,
}

fn one_pole(rate: f64, hz: f64) -> f64 {
    1.0 - (-std::f64::consts::TAU * hz / rate).exp()
}

impl Virtualizer {
    /// `strength` 0 to 1; 0 (or anything that is not a number) is none.
    pub fn new(rate: f64, strength: f64) -> Self {
        let frames = |ms: f64| ((ms / 1000.0 * rate).round() as usize).max(1);
        let len = frames(RING_MS) + 1;
        let mut v = Virtualizer {
            strength: 0.0,
            shadow_a: one_pole(rate, SHADOW_HZ),
            side_a: one_pole(rate, SIDE_SPLIT_HZ),
            side_lift: 0.0,
            cross: 0.0,
            reflect: 0.0,
            itd: frames(ITD_MS).min(len - 1),
            taps: [frames(REFLECT_MS[0]).min(len - 1), frames(REFLECT_MS[1]).min(len - 1)],
            ring: vec![[0.0; 2]; len],
            pos: 0,
            lp: 0.0,
            side_lp: 0.0,
        };
        v.tune(strength);
        v
    }

    /// A new strength, the ring and the filters' memories kept.
    pub fn tune(&mut self, strength: f64) {
        let s = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { 0.0 };
        self.strength = s;
        self.side_lift = SIDE_LIFT * s;
        self.cross = CROSS * s;
        self.reflect = REFLECT * s;
    }

    pub fn strength(&self) -> f64 {
        self.strength
    }

    #[inline]
    fn back(&self, frames: usize) -> [f64; 2] {
        let len = self.ring.len();
        self.ring[(self.pos + len - frames) % len]
    }

    #[inline]
    pub fn frame(&mut self, l: f64, r: f64) -> (f64, f64) {
        // Width: the side's highs lifted.
        let (m, s) = ((l + r) * 0.5, (l - r) * 0.5);
        self.side_lp += self.side_a * (s - self.side_lp);
        let s = s + self.side_lift * (s - self.side_lp);
        let (l, r) = (m + s, m - s);
        // The difference: what the far ear should get of the right, minus what it loses of its own.
        let d = l - r;
        self.lp += self.shadow_a * (d - self.lp);
        self.ring[self.pos] = [self.lp, d - self.lp];
        let head = self.back(self.itd)[0];
        let (e0, e1) = (self.back(self.taps[0])[1], self.back(self.taps[1])[1]);
        self.pos = if self.pos + 1 == self.ring.len() { 0 } else { self.pos + 1 };
        (l - self.cross * head - self.reflect * e0, r + self.cross * head + self.reflect * e1)
    }

    pub fn reset(&mut self) {
        self.ring.iter_mut().for_each(|f| *f = [0.0; 2]);
        (self.pos, self.lp, self.side_lp) = (0, 0.0, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    /// Runs a stereo tone (`gl`, `gr` its level in each channel) and returns each ear's level in dB
    /// against a full tone, over the second half.
    fn run(v: &mut Virtualizer, freq: f64, gl: f64, gr: f64) -> (f64, f64) {
        v.reset();
        let n = 24_000;
        let (mut el, mut er, mut ex) = (0.0, 0.0, 0.0);
        for i in 0..n {
            let x = 0.25 * (std::f64::consts::TAU * freq * i as f64 / RATE).sin();
            let (l, r) = v.frame(gl * x, gr * x);
            if i >= n / 2 {
                (el, er, ex) = (el + l * l, er + r * r, ex + x * x);
            }
        }
        (10.0 * (el / ex).log10(), 10.0 * (er / ex).log10())
    }

    #[test]
    fn a_centred_voice_passes_untouched() {
        let mut v = Virtualizer::new(RATE, 1.0);
        for i in 0..20_000 {
            let x = 0.3 * (i as f64 * 0.0371).sin() + 0.2 * (i as f64 * 0.51).sin();
            assert_eq!(v.frame(x, x), (x, x));
        }
    }

    #[test]
    fn one_side_reaches_the_other_ear_later_softer_and_duller() {
        let mut v = Virtualizer::new(RATE, 1.0);
        v.side_lift = 0.0; // the head alone
        let (l, r) = run(&mut v, 300.0, 1.0, 0.0);
        assert!(r < l - 6.0 && r > l - 20.0, "300 Hz: the far ear {r} dB, the near {l}");
        assert!(l > -3.0, "the near ear keeps most of it: {l}");
        let (l_hi, r_hi) = run(&mut v, 6000.0, 1.0, 0.0);
        assert!(r_hi - l_hi < r - l - 3.0, "the head shadows the highs: {} at 6 kHz against {} at 300 Hz", r_hi - l_hi, r - l);
        // An impulse on the left reaches the right ear first after the head's delay.
        v.reset();
        let first = (0..600).position(|i| v.frame(if i == 0 { 1.0 } else { 0.0 }, 0.0).1.abs() > 1e-12);
        assert_eq!(first, Some(13), "0.28 ms at 48 kHz");
    }

    #[test]
    fn stereo_gets_wider() {
        let mut off = Virtualizer::new(RATE, 0.0);
        
        // Opposite phase: all side. At 3 kHz the side is lifted; at 100 Hz hardly.
        let (l0, _) = run(&mut off, 3000.0, 1.0, -1.0);
        assert!(l0.abs() < 1e-9, "strength 0 changes nothing: {l0}");
        let mut v = Virtualizer::new(RATE, 1.0);
        v.cross = 0.0;
        v.reflect = 0.0;
        let (hi, _) = run(&mut v, 3000.0, 1.0, -1.0);
        let (lo, _) = run(&mut v, 60.0, 1.0, -1.0);
        assert!(hi > 2.5 && lo < 0.6, "side lifted {hi} dB at 3 kHz, {lo} dB at 60 Hz");
    }

    #[test]
    fn nothing_that_is_not_a_number_gets_in() {
        let mut v = Virtualizer::new(RATE, f64::NAN);
        assert_eq!(v.strength(), 0.0);
        v.tune(5.0);
        assert_eq!(v.strength(), 1.0);
        let (l, r) = v.frame(0.5, -0.5);
        assert!(l.is_finite() && r.is_finite());
    }
}
