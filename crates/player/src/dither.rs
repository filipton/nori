//! TPDF dither (±1 LSB) for rounding changed samples back to 16 bits, so the rounding error is
//! signal-independent hiss instead of harmonic grit. Samples already on the 16-bit grid are passed
//! through untouched, so a flat chain stays bit-exact.
//!
//! One xorshift32 generator per channel, seeded apart so the channels' noise is uncorrelated.

use std::hint::select_unpredictable;

/// Channels with their own generator; more share them (the chain takes eight at most).
const CHANNELS: usize = 8;

/// Per-stream dither state.
#[derive(Clone, Copy, Debug)]
pub struct Dither {
    rng: [u32; CHANNELS],
    /// The noise channel 0 took this frame, reused by [`Dither::to_i16_linked`].
    last: f64,
}

impl Default for Dither {
    fn default() -> Self {
        Dither::new()
    }
}

impl Dither {
    pub fn new() -> Dither {
        let mut rng = [0u32; CHANNELS];
        for (c, r) in rng.iter_mut().enumerate() {
            *r = 0x9E37_79B9u32.wrapping_mul(c as u32 + 1) ^ 0x2545_F491;
        }
        Dither { rng, last: 0.0 }
    }

    /// Back to the seeds, so the same input gives the same output.
    pub fn reset(&mut self) {
        *self = Dither::new();
    }

    /// Triangular noise in (-1, 1) LSB, mean zero.
    #[cfg(test)]
    fn tpdf(&mut self, ch: usize) -> f64 {
        let r = &mut self.rng[ch % CHANNELS];
        *r = xorshift(*r);
        noise(*r)
    }

    /// `y` (full scale 1.0) as a dithered 16-bit sample.
    #[inline(always)]
    pub fn to_i16(&mut self, ch: usize, y: f64) -> i16 {
        self.quantize(ch, y, false)
    }

    /// [`Dither::to_i16`] where every channel of the frame is the same signal (mono): all channels
    /// reuse channel 0's noise so they stay identical.
    #[inline(always)]
    pub fn to_i16_linked(&mut self, ch: usize, y: f64) -> i16 {
        self.quantize(ch, y, true)
    }

    /// Branch-free (whether a sample is on the grid is as random as the music): the generator moves
    /// on, and `last` changes, only off the grid.
    #[inline(always)]
    fn quantize(&mut self, ch: usize, y: f64, linked: bool) -> i16 {
        let want = y * 32768.0;
        let off_grid = want.round() != want;
        if !(linked && ch > 0) {
            let r = &mut self.rng[ch % CHANNELS];
            let x = xorshift(*r);
            *r = select_unpredictable(off_grid, x, *r);
            self.last = select_unpredictable(off_grid, noise(x), self.last);
        }
        (want + select_unpredictable(off_grid, self.last, 0.0)).round().clamp(-32768.0, 32767.0) as i16
    }
}

#[inline(always)]
fn xorshift(mut x: u32) -> u32 {
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x
}

/// Triangular noise in (-1, 1) LSB from a generator state: the sum of its two halves.
#[inline(always)]
fn noise(x: u32) -> f64 {
    ((x & 0xFFFF) + (x >> 16) + 1) as f64 * (1.0 / 65536.0) - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_triangular_and_centred() {
        let mut d = Dither::new();
        let n = 1_000_000;
        let (mut sum, mut sq, mut lo, mut hi) = (0.0, 0.0, 0.0f64, 0.0f64);
        let mut hist = [0usize; 4];
        for _ in 0..n {
            let v = d.tpdf(0);
            sum += v;
            sq += v * v;
            lo = lo.min(v);
            hi = hi.max(v);
            hist[((v + 1.0) * 2.0) as usize] += 1;
        }
        assert!(lo > -1.0 && hi < 1.0, "{lo} {hi}");
        assert!((sum / n as f64).abs() < 2e-3, "mean {}", sum / n as f64);
        // A triangle over (-1, 1): variance 1/6, each outer quarter holds 1/8.
        assert!((sq / n as f64 - 1.0 / 6.0).abs() < 2e-3, "variance {}", sq / n as f64);
        let q = |k: usize| hist[k] as f64 / n as f64;
        assert!((q(0) - 0.125).abs() < 5e-3 && (q(3) - 0.125).abs() < 5e-3 && (q(1) - 0.375).abs() < 5e-3, "{hist:?}");
    }

    #[test]
    fn channels_are_uncorrelated() {
        let mut d = Dither::new();
        let n = 200_000;
        let mut c = 0.0;
        for _ in 0..n {
            c += d.tpdf(0) * d.tpdf(1);
        }
        assert!((c / n as f64 / (1.0 / 6.0)).abs() < 0.01, "correlation {}", c / n as f64 * 6.0);
    }

    #[test]
    fn grid_samples_pass_and_others_stay_within_a_step() {
        let mut d = Dither::new();
        for v in [-32768i32, -1, 0, 1, 1000, 32767] {
            for _ in 0..1000 {
                assert_eq!(d.to_i16(0, v as f64 / 32768.0) as i32, v);
                let q = d.to_i16(0, (v as f64 + 0.3) / 32768.0) as i32;
                assert!((q - v).abs() <= 1 || (v == 32767 && q == v), "{v}.3 -> {q}");
            }
        }
    }

    #[test]
    fn linked_channels_match() {
        let mut d = Dither::new();
        for i in 0..1000 {
            let y = (i as f64 * 0.37).sin() * 0.01;
            assert_eq!(d.to_i16_linked(0, y), d.to_i16_linked(1, y));
        }
    }
}
