//! Sample-rate (and mono/stereo) conversion wherever two formats disagree: the incoming side of a transition,
//! a device that will not open at a song's rate, a station that changes rate mid-stream.
//!
//! A polyphase Kaiser-windowed sinc: flat to 0.45 of the lower rate, at least 100 dB down past 0.5 of it.
//! Ratios with at most [`EXACT_PHASES`] steps get a row per output position; others interpolate between
//! [`PHASES`] rows. Output positions are counted in exact fractions (no drift). The filter is zero-phase and
//! holds back half its taps of input (under 2 ms). At the same rate only the channels are converted.

use std::sync::{Arc, Mutex};

use super::{PCM_16, PCM_FLOAT};
use crate::dither::Dither;

/// Taps at the lower of the two rates (a 110 dB Kaiser design over the 0.05 transition band needs 142).
pub const TAPS: usize = 144;
/// Phases of an interpolated table, for a ratio with more steps than [`EXACT_PHASES`].
pub const PHASES: usize = 256;
/// The most steps a ratio may have for a row of its own per step.
pub const EXACT_PHASES: usize = 512;
/// The passband's edge and the stopband's, as fractions of the lower rate.
pub const PASS: f64 = 0.45;
pub const STOP: f64 = 0.5;
/// The stopband's designed depth, dB.
const ATTENUATION_DB: f64 = 110.0;

/// One pair of rates' coefficients: `phases + 1` rows of `taps` (the extra row lets interpolation not wrap).
pub struct Table {
    taps: usize,
    phases: usize,
    /// Rows of `taps`, row `p` for an output falling `p / phases` of the way from one input sample to the next.
    coef: Vec<f32>,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// The zeroth-order modified Bessel function of the first kind, for the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, q) = (1.0, 1.0, x * x / 4.0);
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

impl Table {
    fn design(in_rate: u32, out_rate: u32) -> Table {
        let g = gcd(in_rate as u64, out_rate as u64);
        let steps = (out_rate as u64 / g) as usize;
        let phases = if steps <= EXACT_PHASES { steps } else { PHASES };
        // Cut-off mid-transition band, normalised to the input rate.
        let r = (out_rate as f64 / in_rate as f64).min(1.0);
        let fc = (PASS + STOP) / 2.0 * r;
        let taps = ((TAPS as f64 / r).ceil() as usize).div_ceil(4) * 4;
        let beta = 0.1102 * (ATTENUATION_DB - 8.7);
        let half = taps as f64 / 2.0;
        let i0_beta = bessel_i0(beta);
        let mut coef = vec![0f32; (phases + 1) * taps];
        for p in 0..=phases {
            let frac = p as f64 / phases as f64;
            let row = &mut coef[p * taps..(p + 1) * taps];
            let mut h = vec![0f64; taps];
            for (k, v) in h.iter_mut().enumerate() {
                // Tap k reads input sample (i - taps/2 + 1 + k) for an output at i + frac.
                let t = k as f64 - (half - 1.0) - frac;
                let x = std::f64::consts::PI * 2.0 * fc * t;
                let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
                let w = (t / half).clamp(-1.0, 1.0);
                *v = 2.0 * fc * sinc * bessel_i0(beta * (1.0 - w * w).sqrt()) / i0_beta;
            }
            // Each row passes DC exactly.
            let sum: f64 = h.iter().sum();
            for (d, v) in row.iter_mut().zip(&h) {
                *d = (v / sum) as f32;
            }
        }
        Table { taps, phases, coef }
    }

    fn row(&self, p: usize) -> &[f32] {
        &self.coef[p * self.taps..(p + 1) * self.taps]
    }
}

/// The table for a pair of rates, shared by every converter between them; the last six pairs are kept.
pub fn table(in_rate: u32, out_rate: u32) -> Arc<Table> {
    // A cache of immutable designs, global because converters are made all over the player.
    static TABLES: Mutex<Vec<((u32, u32), Arc<Table>)>> = Mutex::new(Vec::new());
    let mut tables = TABLES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = tables.iter().find(|t| t.0 == (in_rate, out_rate)) {
        return t.1.clone();
    }
    let t = Arc::new(Table::design(in_rate, out_rate));
    if tables.len() >= 6 {
        tables.remove(0);
    }
    tables.push(((in_rate, out_rate), t.clone()));
    t
}

pub struct Resampler {
    in_ch: usize,
    out_ch: usize,
    /// Channels the filter runs on: the fewer of the two.
    ch: usize,
    /// None at the same rate: only the channels change.
    table: Option<Arc<Table>>,
    /// The ratio in lowest terms: `num` input samples per `den` output samples.
    num: u64,
    den: u64,
    /// The next output's place: input sample `at` (counted from the stream's start) and `frac / den` on.
    at: i64,
    frac: u64,
    /// Input held, `ch` channels interleaved; the first frame is input sample `base` (negative: leading silence).
    hist: Vec<f32>,
    base: i64,
    dither: Dither,
}

impl Resampler {
    pub fn new(in_rate: i32, in_ch: i32, out_rate: i32, out_ch: i32) -> Option<Resampler> {
        if in_rate <= 0 || out_rate <= 0 || in_rate > 384_000 || out_rate > 384_000 {
            return None;
        }
        if !matches!(in_ch, 1 | 2) || !matches!(out_ch, 1 | 2) {
            return None;
        }
        let g = gcd(in_rate as u64, out_rate as u64);
        let table = (in_rate != out_rate).then(|| table(in_rate as u32, out_rate as u32));
        let ch = in_ch.min(out_ch) as usize;
        let lead = table.as_ref().map_or(0, |t| t.taps / 2 - 1);
        Some(Resampler {
            in_ch: in_ch as usize,
            out_ch: out_ch as usize,
            ch,
            table,
            num: in_rate as u64 / g,
            den: out_rate as u64 / g,
            at: 0,
            frac: 0,
            hist: vec![0.0; lead * ch],
            base: -(lead as i64),
            dither: Dither::new(),
        })
    }

    /// Input frames the converter holds back before its output catches up with them.
    pub fn latency_frames(&self) -> usize {
        self.table.as_ref().map_or(0, |t| t.taps / 2)
    }

    /// Converts all of `input` (interleaved, `in_enc`) into the start of `output`. Returns (consumed, produced)
    /// bytes; None when the output is too small (nothing taken) or an encoding is not PCM.
    pub fn process(&mut self, input: &[u8], in_enc: i32, output: &mut [u8], out_enc: i32) -> Option<(usize, usize)> {
        let wi = match in_enc {
            PCM_16 => 2,
            PCM_FLOAT => 4,
            _ => return None,
        };
        let wo = match out_enc {
            PCM_16 => 2,
            PCM_FLOAT => 4,
            _ => return None,
        };
        let n = input.len() / (wi * self.in_ch);
        if n == 0 {
            return Some((0, 0));
        }
        let cap = output.len() / (wo * self.out_ch);
        let made = self.can_make(self.base + (self.hist.len() / self.ch) as i64 + n as i64);
        if made > cap {
            return None;
        }
        let samples = n * self.in_ch;
        if wi == 4 {
            keep(&mut self.hist, &input.as_chunks::<4>().0[..samples], self.in_ch, self.ch, f32::from_le_bytes);
        } else {
            keep(&mut self.hist, &input.as_chunks::<2>().0[..samples], self.in_ch, self.ch, |b| i16::from_le_bytes(b) as f32 / 32768.0);
        }
        let mut out = [0f32; 2];
        for k in 0..made {
            self.next(&mut out);
            for c in 0..self.out_ch {
                let v = out[c.min(self.ch - 1)];
                let at = (k * self.out_ch + c) * wo;
                if wo == 4 {
                    output[at..at + 4].copy_from_slice(&v.to_le_bytes());
                } else {
                    // Mono spread to stereo keeps one dither noise for both sides.
                    let v = v.clamp(-1.0, 1.0) as f64;
                    let q = if self.ch < self.out_ch { self.dither.to_i16_linked(c, v) } else { self.dither.to_i16(c, v) };
                    output[at..at + 2].copy_from_slice(&q.to_le_bytes());
                }
            }
        }
        // Drop what no output's taps reach again.
        let keep_from = match &self.table {
            Some(t) => self.at - (t.taps / 2 - 1) as i64,
            None => self.at,
        };
        let drop = ((keep_from - self.base).max(0) as usize).min(self.hist.len() / self.ch);
        if drop > 0 {
            self.hist.drain(..drop * self.ch);
            self.base += drop as i64;
        }
        Some((n * wi * self.in_ch, made * wo * self.out_ch))
    }

    /// How many outputs the input up to (not including) sample `end` makes from where the converter is.
    fn can_make(&self, end: i64) -> usize {
        let ahead = self.table.as_ref().map_or(0, |t| t.taps / 2) as i64;
        let avail = end - 1 - ahead - self.at;
        if avail < 0 {
            return 0;
        }
        // Outputs k = 0.. while at + (frac + k·num) / den <= at + avail.
        let limit = (avail as u128 + 1) * self.den as u128;
        let from = self.frac as u128;
        if limit <= from {
            return 0;
        }
        (limit - from).div_ceil(self.num as u128) as usize
    }

    /// The output at the current place, in the filter's channels; the place moves on one output.
    #[inline]
    fn next(&mut self, out: &mut [f32; 2]) {
        let ch = self.ch;
        match &self.table {
            None => {
                let i = (self.at - self.base) as usize * ch;
                out[..ch].copy_from_slice(&self.hist[i..i + ch]);
            }
            Some(t) => {
                let start = (self.at - (t.taps / 2 - 1) as i64 - self.base) as usize;
                let x = &self.hist[start * ch..(start + t.taps) * ch];
                if t.phases as u64 == self.den {
                    // A row for this very place.
                    dot(t.row(self.frac as usize), x, ch, out);
                } else {
                    let place = self.frac as f64 * t.phases as f64 / self.den as f64;
                    let p = place as usize;
                    let w = (place - p as f64) as f32;
                    let (mut a, mut b) = ([0f32; 2], [0f32; 2]);
                    dot(t.row(p), x, ch, &mut a);
                    dot(t.row(p + 1), x, ch, &mut b);
                    for c in 0..ch {
                        out[c] = a[c] + w * (b[c] - a[c]);
                    }
                }
            }
        }
        self.frac += self.num;
        self.at += (self.frac / self.den) as i64;
        self.frac %= self.den;
    }
}

/// Appends `input` (`in_ch` channels, each sample's float by `load`) to `hist` as `ch` channels: a stereo
/// pair averaged for mono, else the first `ch` of each frame.
fn keep<const W: usize>(hist: &mut Vec<f32>, input: &[[u8; W]], in_ch: usize, ch: usize, load: impl Fn([u8; W]) -> f32) {
    if in_ch == 2 && ch == 1 {
        hist.extend(input.as_chunks::<2>().0.iter().map(|f| (load(f[0]) + load(f[1])) * 0.5));
    } else if in_ch == ch {
        hist.extend(input.iter().map(|&b| load(b)));
    } else {
        hist.extend(input.chunks_exact(in_ch).flat_map(|f| f[..ch].iter().map(|&b| load(b))));
    }
}

/// One row of coefficients over `ch`-channel interleaved samples, four taps at a time.
#[inline]
fn dot(h: &[f32], x: &[f32], ch: usize, out: &mut [f32; 2]) {
    if ch == 2 {
        let (mut l, mut r) = ([0f32; 4], [0f32; 4]);
        for (hc, xc) in h.as_chunks::<4>().0.iter().zip(x.as_chunks::<8>().0) {
            for j in 0..4 {
                l[j] += hc[j] * xc[2 * j];
                r[j] += hc[j] * xc[2 * j + 1];
            }
        }
        out[0] = (l[0] + l[1]) + (l[2] + l[3]);
        out[1] = (r[0] + r[1]) + (r[2] + r[3]);
    } else {
        let mut m = [0f32; 4];
        for (hc, xc) in h.as_chunks::<4>().0.iter().zip(x.as_chunks::<4>().0) {
            for j in 0..4 {
                m[j] += hc[j] * xc[j];
            }
        }
        out[0] = (m[0] + m[1]) + (m[2] + m[3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, frames: usize) -> Vec<i16> {
        (0..frames).map(|i| ((i as f64 * hz * std::f64::consts::TAU / rate as f64).sin() * 20000.0) as i16).collect()
    }

    fn bytes_of(s: &[i16]) -> Vec<u8> {
        s.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn shorts_of(b: &[u8]) -> Vec<i16> {
        b.as_chunks::<2>().0.iter().map(|&c| i16::from_le_bytes(c)).collect()
    }

    fn floats(b: &[u8]) -> Vec<f64> {
        b.as_chunks::<4>().0.iter().map(|&c| f32::from_le_bytes(c) as f64).collect()
    }

    /// `x` (mono float) through a converter from `a` to `b` Hz, in float, in buffers of `chunk` frames.
    fn convert(x: &[f64], a: u32, b: u32, chunk: usize) -> Vec<f64> {
        let mut r = Resampler::new(a as i32, 1, b as i32, 1).unwrap();
        let mut out = Vec::new();
        let mut buf = vec![0u8; (chunk * b as usize / a as usize + 8) * 4];
        for c in x.chunks(chunk) {
            let bytes: Vec<u8> = c.iter().flat_map(|v| (*v as f32).to_le_bytes()).collect();
            let (_, made) = r.process(&bytes, PCM_FLOAT, &mut buf, PCM_FLOAT).unwrap();
            out.extend(floats(&buf[..made]));
        }
        out
    }

    /// Amplitude of `x` at `hz` (and the residue once that sine is fitted out, as a fraction of it), over the
    /// middle of it, where the filter has settled.
    fn fit(x: &[f64], rate: u32, hz: f64) -> (f64, f64) {
        let x = &x[x.len() / 4..x.len() * 3 / 4];
        let w = std::f64::consts::TAU * hz / rate as f64;
        // Least squares on sin and cos over the window (at an offset, so the start of `x` is t = 0).
        let (mut ss, mut sc, mut cc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let (s, c) = ((w * i as f64).sin(), (w * i as f64).cos());
            ss += s * s;
            sc += s * c;
            cc += c * c;
            xs += v * s;
            xc += v * c;
        }
        let det = ss * cc - sc * sc;
        let (a, b) = ((xs * cc - xc * sc) / det, (xc * ss - xs * sc) / det);
        let amp = (a * a + b * b).sqrt();
        let resid = x.iter().enumerate().map(|(i, v)| (v - a * (w * i as f64).sin() - b * (w * i as f64).cos()).powi(2)).sum::<f64>() / x.len() as f64;
        (amp, resid.sqrt() / (amp / 2f64.sqrt()).max(1e-30))
    }

    fn tone(rate: u32, hz: f64, secs: f64, amp: f64) -> Vec<f64> {
        (0..(rate as f64 * secs) as usize).map(|i| amp * (std::f64::consts::TAU * hz * i as f64 / rate as f64).sin()).collect()
    }

    fn db(x: f64) -> f64 {
        20.0 * x.log10()
    }

    #[test]
    fn same_rate_is_a_passthrough() {
        let mut r = Resampler::new(44100, 2, 44100, 2).unwrap();
        let l = sine(44100, 440.0, 1000);
        let stereo: Vec<i16> = l.iter().flat_map(|v| [*v, (*v / 2)]).collect();
        let input = bytes_of(&stereo);
        let mut out = vec![0u8; 20000];
        let (used, made) = r.process(&input, PCM_16, &mut out, PCM_16).unwrap();
        assert_eq!(used, input.len());
        assert_eq!(shorts_of(&out[..made]), stereo, "sample for sample, nothing held back");
    }

    #[test]
    fn rate_change_keeps_tone() {
        let mut r = Resampler::new(44100, 1, 48000, 1).unwrap();
        let input = bytes_of(&sine(44100, 1000.0, 4410));
        let mut out = vec![0u8; 20000];
        let (used, made) = r.process(&input, PCM_16, &mut out, PCM_16).unwrap();
        assert_eq!(used, input.len());
        // Everything but the filter's look-ahead.
        let frames = made / 2;
        let held = r.latency_frames() * 48000 / 44100;
        assert!((frames as i64 - (4800 - held) as i64).abs() <= 2, "{frames}");
        let back: Vec<f64> = shorts_of(&out[..made]).iter().map(|v| *v as f64).collect();
        let (amp, resid) = fit(&back, 48000, 1000.0);
        assert!((amp - 20000.0).abs() < 20.0 && resid < 1e-3, "{amp} {resid}");
    }

    #[test]
    fn split_buffers_match_one_whole_one() {
        let x: Vec<f64> = tone(48000, 500.0, 0.5, 0.5).iter().zip(tone(48000, 7777.0, 0.5, 0.3)).map(|(a, b)| a + b).collect();
        let whole = convert(&x, 48000, 44100, x.len());
        for chunk in [1, 7, 100, 1024] {
            let split = convert(&x, 48000, 44100, chunk);
            assert_eq!(split.len(), whole.len(), "{chunk}");
            assert!(split.iter().zip(&whole).all(|(a, b)| a == b), "in buffers of {chunk} frames: the same samples");
        }
    }

    #[test]
    fn mono_to_stereo_and_back() {
        let mut r = Resampler::new(44100, 1, 44100, 2).unwrap();
        let input = bytes_of(&sine(44100, 440.0, 500));
        let mut out = vec![0u8; 20000];
        let (_, made) = r.process(&input, PCM_16, &mut out, PCM_16).unwrap();
        assert_eq!(made, 500 * 2 * 2);
        let back = shorts_of(&out[..made]);
        assert!(back.as_chunks::<2>().0.iter().all(|c| c[0] == c[1]));
        let mut r = Resampler::new(44100, 2, 44100, 1).unwrap();
        let mut mono = vec![0u8; 20000];
        let (_, made) = r.process(&out[..made], PCM_16, &mut mono, PCM_16).unwrap();
        assert_eq!(made, 500 * 2);
        assert_eq!(shorts_of(&mono[..made]), shorts_of(&input));
        // And across rates: stereo 48 kHz to mono 44.1 kHz is the two sides' average, converted.
        let mut r = Resampler::new(48000, 2, 44100, 1).unwrap();
        let x = sine(48000, 1000.0, 9600);
        let st: Vec<i16> = x.iter().flat_map(|v| [*v, *v / 3]).collect();
        let mut out = vec![0u8; 40000];
        let (_, made) = r.process(&bytes_of(&st), PCM_16, &mut out, PCM_16).unwrap();
        let (amp, _) = fit(&shorts_of(&out[..made]).iter().map(|v| *v as f64).collect::<Vec<_>>(), 44100, 1000.0);
        assert!((amp - 20000.0 * 2.0 / 3.0).abs() < 20.0, "{amp}");
    }

    #[test]
    fn passband_is_flat() {
        for (a, b) in [(44100u32, 48000u32), (48000, 44100), (96000, 44100), (44100, 96000), (192000, 48000), (44100, 47999)] {
            let low = a.min(b) as f64;
            let mut worst = 0.0f64;
            for k in 1..=18 {
                let hz = low * PASS * k as f64 / 18.0;
                let y = convert(&tone(a, hz, 0.25, 0.5), a, b, 4096);
                let (amp, resid) = fit(&y, b, hz);
                worst = worst.max(db(amp / 0.5).abs());
                assert!(resid < 1e-4, "{a} -> {b}, {hz:.0} Hz: {resid:e} of the tone is something else");
            }
            eprintln!("{a} -> {b}: passband within {worst:.4} dB");
            assert!(worst < 0.1, "{a} -> {b}: {worst:.3} dB");
        }
    }

    /// Past 0.5 of the lower rate: no aliasing on a downsample, no images on an upsample.
    #[test]
    fn stopband_is_100_db_down() {
        for (a, b) in [(48000u32, 44100u32), (96000, 44100), (96000, 48000), (192000, 44100), (48000, 44099)] {
            let mut worst = f64::MIN;
            let top = a as f64 / 2.0;
            for k in 0..24 {
                let hz = b.min(a) as f64 * STOP + (top - b.min(a) as f64 * STOP) * (k as f64 + 0.5) / 24.0;
                let y = convert(&tone(a, hz, 0.25, 0.5), a, b, 4096);
                let mid = &y[y.len() / 4..y.len() * 3 / 4];
                let rms = (mid.iter().map(|v| v * v).sum::<f64>() / mid.len() as f64).sqrt();
                worst = worst.max(db(rms * 2f64.sqrt() / 0.5));
            }
            eprintln!("{a} -> {b}: the stopband at {worst:.1} dB");
            assert!(worst < -100.0, "{a} -> {b}: {worst:.1} dB gets through");
        }
        for b in [48000u32, 96000, 47999] {
            let hz = 44100.0 * 0.44;
            let y = convert(&tone(44100, hz, 0.25, 0.5), 44100, b, 4096);
            let (amp, resid) = fit(&y, b, hz);
            eprintln!("44100 -> {b}: all but the tone {:.1} dB", db(resid * amp / 0.5));
            assert!(db(resid * amp / 0.5) < -100.0, "44100 -> {b}: an image at {:.1} dB", db(resid * amp / 0.5));
        }
    }

    #[test]
    fn sweep_matches_to_90_db() {
        for (a, b) in [(48000u32, 44100u32), (44100, 48000), (88200, 44100)] {
            let low = a.min(b) as f64;
            let (f0, f1, secs) = (20.0, low * 0.42, 2.0);
            let k = (f1 / f0).ln() / secs;
            let at = |t: f64| 0.5 * (std::f64::consts::TAU * f0 * ((k * t).exp() - 1.0) / k).sin();
            let x: Vec<f64> = (0..(a as f64 * secs) as usize).map(|i| at(i as f64 / a as f64)).collect();
            let y = convert(&x, a, b, 1000);
            let want: Vec<f64> = (0..y.len()).map(|i| at(i as f64 / b as f64)).collect();
            // Past the onset, which is a step.
            let from = b as usize / 10;
            let err = (y[from..].iter().zip(&want[from..]).map(|(p, q)| (p - q).powi(2)).sum::<f64>() / (y.len() - from) as f64).sqrt();
            eprintln!("{a} -> {b}: a sweep to {f1:.0} Hz off by {:.1} dB", db(err / 0.5 * 2f64.sqrt()));
            assert!(db(err / 0.5 * 2f64.sqrt()) < -90.0, "{a} -> {b}: {:.1} dB", db(err / 0.5 * 2f64.sqrt()));
        }
    }

    #[test]
    fn pcm16_output_is_dithered() {
        let x = tone(48000, 1000.0, 0.2, 0.5);
        let bytes: Vec<u8> = x.iter().flat_map(|v| (*v as f32).to_le_bytes()).collect();
        let mut r = Resampler::new(48000, 1, 44100, 1).unwrap();
        let mut out = vec![0u8; 40000];
        let (_, made) = r.process(&bytes, PCM_FLOAT, &mut out, PCM_16).unwrap();
        let y: Vec<f64> = shorts_of(&out[..made]).iter().map(|v| *v as f64 / 32768.0).collect();
        let (amp, resid) = fit(&y, 44100, 1000.0);
        // TPDF plus rounding: a quarter of a step squared of noise, against the tone.
        let floor = (0.25f64).sqrt() / 32768.0;
        assert!((amp - 0.5).abs() < 1e-4 && (resid * amp / 2f64.sqrt() / floor - 1.0).abs() < 0.2, "{amp} {resid}");
    }

    /// `cargo test --release -p nori-player --lib resample_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn resample_cost() {
        for (a, b) in [(44100u32, 48000u32), (48000, 44100), (96000, 48000), (192000, 44100), (44100, 47999)] {
            let secs = 20usize;
            let x: Vec<u8> = (0..a as usize * secs * 2).flat_map(|i| (((i as f64 * 0.001).sin() * 10000.0) as i16).to_le_bytes()).collect();
            let mut r = Resampler::new(a as i32, 2, b as i32, 2).unwrap();
            let mut out = vec![0u8; 1 << 16];
            let t = std::time::Instant::now();
            for c in x.chunks(16384) {
                r.process(c, PCM_16, &mut out, PCM_16).unwrap();
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 / secs as f64;
            eprintln!("{a} -> {b}: {ms:.2} ms per second of stereo ({} taps)", table(a, b).taps);
            std::hint::black_box(&out);
        }
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(Resampler::new(0, 2, 44100, 2).is_none());
        assert!(Resampler::new(44100, 6, 44100, 2).is_none());
        assert!(Resampler::new(44100, 2, 44100, 6).is_none());
        let mut r = Resampler::new(44100, 2, 48000, 2).unwrap();
        assert!(r.process(&[0u8; 100], 7, &mut [0u8; 10000], PCM_16).is_none());
        assert!(r.process(&[0u8; 4000], PCM_16, &mut [0u8; 4], PCM_16).is_none());
    }
}
