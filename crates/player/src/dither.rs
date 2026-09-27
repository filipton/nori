//! Back to 16 bits, dithered: wherever the 16-bit chain has changed a sample (the equalizer and the
//! effects, `dsp::Equalizer::process_i16`; ReplayGain, `pcm::scale_dithered`) the result is rounded to
//! the 16-bit grid with triangular (TPDF) noise of ±1 LSB added first. Plain rounding makes the error a
//! function of the signal: a quiet passage, a fade's tail, a reverb dying away come out as a stepped
//! waveform whose harmonics are heard as grit. With TPDF dither the error is independent of the signal,
//! its mean and its power both, so what is left is a steady hiss 96 dB down and every detail under one
//! step is still there, inside it. Samples nothing changed are never dithered: a flat chain stays
//! bit-exact, and so does any sample the chain hands back on the 16-bit grid (what a limiter below its
//! threshold only delays, the louder side under balance, digital silence), which has no rounding error to
//! hide. Music through an equalizer or a gain lands between the steps all but always.
//!
//! One cheap generator per channel (xorshift32, one step a sample, its two halves summed for the
//! triangle), each seeded apart so the channels' noise is uncorrelated and a mono image does not collapse
//! it to the centre. Nothing is allocated; the state is a few words.
//!
//! [`NOISE_SHAPING`] would add first-order error feedback (the rounding error of each sample taken off the
//! next), which moves the noise up the spectrum, 3 dB more of it in all. Measured (the test below, at
//! 44.1 kHz): 6.7 dB less noise at 1-5 kHz, where the ear is most sensitive, and 5.2 dB more in the top
//! octave. That takes the hiss from about -96 dBFS to about -101 dBFS in the band that matters: below the
//! noise of the phone's own speaker amplifier, of a Bluetooth codec and of any room, so not heard on the
//! outputs the 16-bit path serves; and more noise near 20 kHz is what a lossy Bluetooth codec spends bits
//! on. Whoever wants the lower floor has it from the float path (high quality output). It stays off.

/// First-order noise shaping on top of the dither. See the module's words: measured, and not worth it.
pub const NOISE_SHAPING: bool = false;

/// Channels a dither keeps apart; beyond this they share generators (the chain takes eight at most).
const CHANNELS: usize = 8;

/// The dither's state for one stream: a generator and (for noise shaping) the last error, per channel.
#[derive(Clone, Copy, Debug)]
pub struct Dither {
    rng: [u32; CHANNELS],
    err: [f64; CHANNELS],
    /// The noise the frame's first channel took, for the others of a frame that is one sound ([`Dither::to_i16_linked`]).
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
            // Any non-zero seeds do; these are far apart in the generator's sequence.
            *r = 0x9E37_79B9u32.wrapping_mul(c as u32 + 1) ^ 0x2545_F491;
        }
        Dither { rng, err: [0.0; CHANNELS], last: 0.0 }
    }

    /// Back to the seeds: the same input dithered again gives the same output (a seek, a stream made again).
    pub fn reset(&mut self) {
        *self = Dither::new();
    }

    /// Triangular noise in (-1, 1), in steps of the output grid, mean zero.
    #[inline(always)]
    pub fn tpdf(&mut self, ch: usize) -> f64 {
        let r = &mut self.rng[ch % CHANNELS];
        let mut x = *r;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *r = x;
        ((x & 0xFFFF) + (x >> 16) + 1) as f64 * (1.0 / 65536.0) - 1.0
    }

    /// `y` (full scale 1.0) as a 16-bit sample, dithered.
    #[inline(always)]
    pub fn to_i16(&mut self, ch: usize, y: f64) -> i16 {
        self.quantize(ch, y, NOISE_SHAPING, false)
    }

    /// [`Dither::to_i16`] for a frame whose channels are one sound (the chain's mono): every channel takes
    /// the noise the first one took, so what was made the same comes out the same, hiss and all.
    #[inline(always)]
    pub fn to_i16_linked(&mut self, ch: usize, y: f64) -> i16 {
        self.quantize(ch, y, NOISE_SHAPING, true)
    }

    #[inline(always)]
    fn quantize(&mut self, ch: usize, y: f64, shaped: bool, linked: bool) -> i16 {
        let mut want = y * 32768.0;
        let on_grid = want.round();
        if on_grid == want {
            // Nothing to round: kept exactly, as it came.
            return on_grid.clamp(-32768.0, 32767.0) as i16;
        }
        if shaped {
            want -= self.err[ch % CHANNELS];
        }
        let noise = if linked && ch > 0 {
            self.last
        } else {
            self.last = self.tpdf(ch);
            self.last
        };
        let q = (want + noise).round().clamp(-32768.0, 32767.0);
        if shaped {
            // Clipped samples feed back nothing, or the loop would chase a level it cannot reach.
            self.err[ch % CHANNELS] = if q.abs() < 32767.0 { q - want } else { 0.0 };
        }
        q as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_noise_is_triangular_within_one_step_and_centred() {
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
        // A triangle over (-1, 1) has variance 1/6; its outer quarters hold an eighth each.
        assert!((sq / n as f64 - 1.0 / 6.0).abs() < 2e-3, "variance {}", sq / n as f64);
        let q = |k: usize| hist[k] as f64 / n as f64;
        assert!((q(0) - 0.125).abs() < 5e-3 && (q(3) - 0.125).abs() < 5e-3 && (q(1) - 0.375).abs() < 5e-3, "{hist:?}");
    }

    #[test]
    fn the_channels_noise_is_uncorrelated() {
        let mut d = Dither::new();
        let n = 200_000;
        let mut c = 0.0;
        for _ in 0..n {
            c += d.tpdf(0) * d.tpdf(1);
        }
        assert!((c / n as f64 / (1.0 / 6.0)).abs() < 0.01, "correlation {}", c / n as f64 * 6.0);
    }

    /// What noise shaping would buy: the error's power in the band the ear hears best (1-5 kHz) and in the
    /// top octave, plain TPDF against TPDF with first-order error feedback, on a quiet 44.1 kHz signal.
    #[test]
    fn what_noise_shaping_would_buy() {
        let n = 1 << 15;
        let x: Vec<f64> = (0..n).map(|i| (i as f64 * 0.0371).sin() * 0.001 + (i as f64 * 0.23).sin() * 0.0003).collect();
        let band = |shaped: bool, lo: f64, hi: f64| {
            let mut d = Dither::new();
            let e: Vec<f64> = x.iter().map(|v| d.quantize(0, *v, shaped, false) as f64 - v * 32768.0).collect();
            // The error's power between `lo` and `hi` Hz (a plain DFT over a Hann window, every 8th bin).
            let (k0, k1) = ((lo / 44_100.0 * n as f64) as usize, (hi / 44_100.0 * n as f64) as usize);
            let mut p = 0.0;
            for k in (k0..k1).step_by(8) {
                let (mut re, mut im) = (0.0, 0.0);
                for (i, v) in e.iter().enumerate() {
                    let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
                    let ph = std::f64::consts::TAU * k as f64 * i as f64 / n as f64;
                    re += v * w * ph.cos();
                    im += v * w * ph.sin();
                }
                p += re * re + im * im;
            }
            10.0 * p.log10()
        };
        let (mid, mid_shaped) = (band(false, 1_000.0, 5_000.0), band(true, 1_000.0, 5_000.0));
        let (top, top_shaped) = (band(false, 11_025.0, 22_050.0), band(true, 11_025.0, 22_050.0));
        eprintln!("1-5 kHz: shaping {:+.1} dB; 11-22 kHz: {:+.1} dB", mid_shaped - mid, top_shaped - top);
        assert!(mid_shaped < mid - 1.0 && top_shaped > top + 3.0, "it moves the noise up: {:.1} {:.1}", mid_shaped - mid, top_shaped - top);
    }

    #[test]
    fn samples_on_the_grid_are_kept_and_the_rest_stay_within_a_step() {
        let mut d = Dither::new();
        for v in [-32768i32, -1, 0, 1, 1000, 32767] {
            for _ in 0..1000 {
                assert_eq!(d.to_i16(0, v as f64 / 32768.0) as i32, v);
                let q = d.to_i16(0, (v as f64 + 0.3) / 32768.0) as i32;
                assert!((q - v).abs() <= 1 || (v == 32767 && q == v), "{v}.3 -> {q}");
            }
        }
    }
}
