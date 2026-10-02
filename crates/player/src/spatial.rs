//! Stereo headphone virtualizer, scaled by one strength:
//! - width: side signal above ~400 Hz lifted up to +3.5 dB;
//! - head: crossfeed delayed 0.28 ms, low-passed at 1.4 kHz, up to -12 dB;
//! - room: one early reflection per ear of the opposite highs (9.3 / 11.7 ms), up to -18 dB.
//!
//! Head and room are fed the L-R difference, so centred content cancels and passes bit-exact
//! (plain crossfeed would comb it).

/// Ring length; longer than the latest tap.
const RING_MS: f64 = 12.5;
const ITD_MS: f64 = 0.28;
const SHADOW_HZ: f64 = 1400.0;
const SIDE_SPLIT_HZ: f64 = 400.0;
const REFLECT_MS: [f64; 2] = [9.3, 11.7];
/// At full strength.
const SIDE_LIFT: f64 = 0.5;
const CROSS: f64 = 0.25;
const REFLECT: f64 = 0.12;

#[derive(Debug)]
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
    /// Per frame: L-R low-passed, and the rest above it.
    ring: Vec<[f64; 2]>,
    pos: usize,
    lp: f64,
    side_lp: f64,
}

/// `clone_from` keeps the ring's memory (the sink copies the chain's state without allocating).
impl Clone for Virtualizer {
    fn clone(&self) -> Self {
        Virtualizer { ring: self.ring.clone(), ..*self }
    }

    fn clone_from(&mut self, o: &Self) {
        let mut ring = std::mem::take(&mut self.ring);
        ring.clone_from(&o.ring);
        *self = Virtualizer { ring, ..*o };
    }
}

fn one_pole(rate: f64, hz: f64) -> f64 {
    1.0 - (-std::f64::consts::TAU * hz / rate).exp()
}

impl Virtualizer {
    /// `strength` 0 to 1; NaN is 0.
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

    /// Sets the strength, keeping the filter state.
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
        // Width: lift the side's highs.
        let (m, s) = ((l + r) * 0.5, (l - r) * 0.5);
        self.side_lp += self.side_a * (s - self.side_lp);
        let s = s + self.side_lift * (s - self.side_lp);
        let (l, r) = (m + s, m - s);
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

    /// Each ear's level in dB relative to the input tone (scaled `gl`, `gr` per channel), second half.
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
    fn centred_and_clamped() {
        let mut v = Virtualizer::new(RATE, 1.0);
        for i in 0..20_000 {
            let x = 0.3 * (i as f64 * 0.0371).sin() + 0.2 * (i as f64 * 0.51).sin();
            assert_eq!(v.frame(x, x), (x, x));
        }

        // Strength is clamped.

        let mut v = Virtualizer::new(RATE, f64::NAN);
        assert_eq!(v.strength(), 0.0);
        v.tune(5.0);
        assert_eq!(v.strength(), 1.0);
        let (l, r) = v.frame(0.5, -0.5);
        assert!(l.is_finite() && r.is_finite());
    }

    #[test]
    fn crossfeed_is_delayed_quieter_and_low_passed() {
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
        // Opposite phase is all side.
        let (l0, _) = run(&mut off, 3000.0, 1.0, -1.0);
        assert!(l0.abs() < 1e-9, "strength 0 changes nothing: {l0}");
        let mut v = Virtualizer::new(RATE, 1.0);
        v.cross = 0.0;
        v.reflect = 0.0;
        let (hi, _) = run(&mut v, 3000.0, 1.0, -1.0);
        let (lo, _) = run(&mut v, 60.0, 1.0, -1.0);
        assert!(hi > 2.5 && lo < 0.6, "side lifted {hi} dB at 3 kHz, {lo} dB at 60 Hz");
    }

}
