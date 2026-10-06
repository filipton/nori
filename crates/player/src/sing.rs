//! Sing: a song's vocals turned down to a level the listener picks. Open-Unmix's vocals model ([`model`], behind
//! `neural-beats`) makes each song's [`VocalMask`] ahead of playback, the vocal share of each time-frequency cell; in the
//! chain the [`Masker`] plays `mix * (1 - (1 - level) * mask)` through a short-time Fourier transform. A song without a
//! mask plays unchanged.

use std::ops::Range;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::dither::Dither;
use crate::pcm::Encoding;

#[cfg(feature = "neural-beats")]
pub mod model;

/// The model's spectrum: 4096 points at 44.1 kHz, so 2049 bins 10.77 Hz apart, a frame every 1024 samples.
pub const MODEL_RATE: f64 = 44_100.0;
pub const MODEL_FFT: usize = 4096;
pub const MODEL_HOP: usize = 1024;
pub const MODEL_BINS: usize = MODEL_FFT / 2 + 1;

/// The mask's bands, in model bins: one bin each at the bottom, then 3 % of their frequency wide (192 bands).
fn band_edges() -> Vec<usize> {
    let mut edges = vec![0];
    let mut at = 0;
    while at < MODEL_BINS {
        at = (at + (at * 3 / 100).max(1)).min(MODEL_BINS);
        edges.push(at);
    }
    edges
}

/// Bands a mask frame holds.
pub fn bands() -> usize {
    band_edges().len() - 1
}

/// The band holding frequency `hz`.
fn band_of(edges: &[usize], hz: f64) -> usize {
    let bin = (hz * MODEL_FFT as f64 / MODEL_RATE).round().max(0.0) as usize;
    edges.partition_point(|e| *e <= bin).saturating_sub(1).min(edges.len() - 2)
}

/// File format: magic, version, then bands (u16), frames per second (f32), frames (u32), and a byte per band and
/// frame (0 no vocals, 255 all vocals). A new band layout or meaning gets a new version.
const MAGIC: &[u8; 4] = b"NVMK";
pub const VERSION: u8 = 1;
const HEADER: usize = 4 + 1 + 2 + 4 + 4;

/// Why a stored mask was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskError {
    NotAMask,
    /// Made by another version; made again.
    OtherVersion,
    Truncated,
}

/// A song's vocal mask: per frame (`fps` a second, frame `k` centred on second `k / fps`), the vocal share of each
/// band, both channels together.
#[derive(Debug, Clone, PartialEq)]
pub struct VocalMask {
    pub fps: f32,
    bands: usize,
    data: Vec<u8>,
}

impl VocalMask {
    /// From rows of [`bands`] bytes each.
    pub fn new(fps: f32, data: Vec<u8>) -> VocalMask {
        VocalMask { fps, bands: bands(), data }
    }

    pub fn frames(&self) -> usize {
        self.data.len() / self.bands.max(1)
    }

    pub fn row(&self, frame: usize) -> &[u8] {
        &self.data[frame * self.bands..(frame + 1) * self.bands]
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER + self.data.len());
        b.extend_from_slice(MAGIC);
        b.push(VERSION);
        b.extend_from_slice(&(self.bands as u16).to_le_bytes());
        b.extend_from_slice(&self.fps.to_le_bytes());
        b.extend_from_slice(&(self.frames() as u32).to_le_bytes());
        b.extend_from_slice(&self.data);
        b
    }

    pub fn from_bytes(b: &[u8]) -> Result<VocalMask, MaskError> {
        if b.len() < HEADER || &b[..4] != MAGIC {
            return Err(MaskError::NotAMask);
        }
        let bands = u16::from_le_bytes([b[5], b[6]]) as usize;
        if b[4] != VERSION || bands != self::bands() {
            return Err(MaskError::OtherVersion);
        }
        let fps = f32::from_le_bytes([b[7], b[8], b[9], b[10]]);
        let frames = u32::from_le_bytes([b[11], b[12], b[13], b[14]]) as usize;
        let data = &b[HEADER..];
        if data.len() != frames * bands || fps.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            return Err(MaskError::Truncated);
        }
        Ok(VocalMask { fps, bands, data: data.to_vec() })
    }
}

/// A song's mask where the song is on the timeline (µs, as the chain's input is stamped).
#[derive(Debug, Clone)]
pub struct Placed {
    pub at: Range<i64>,
    pub mask: Arc<VocalMask>,
}

/// The chain's vocal masker: a short-time Fourier transform (sqrt-Hann windows, 75 % overlap) whose bins are scaled by
/// `1 - (1 - level) * mask` of the song and moment the frame is stamped with. It looks ahead a window less a hop: the
/// first output waits for that much input, the rest comes at [`Masker::end`], so output frame `i` is input frame `i`.
/// A stretch without vocals (or at full level) is passed through untouched. Allocation-free once made.
pub struct Masker {
    n: usize,
    hop: usize,
    channels: usize,
    rate: u32,
    encoding: Encoding,
    level: f32,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    window: Arc<[f32]>,
    /// Mask band per bin up to Nyquist.
    band: Arc<[u16]>,
    /// The last `n` input frames per channel (`c * n + i`), the newest `filled` of them after the last transform.
    input: Vec<f32>,
    filled: usize,
    /// Overlap-added output per channel, aligned with `input`.
    acc: Vec<f32>,
    /// Per hop of `acc`, whether a masked frame reached it (else its output is the input, bit for bit).
    touched: Vec<bool>,
    /// Leading output frames still to drop (before the first input frame).
    skip: usize,
    /// Input frames not yet output.
    held: usize,
    /// Timeline position of the next input frame, µs, and song frames per frame.
    pts: f64,
    pace: f64,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    gains: Vec<f32>,
    dither: Dither,
}

impl Clone for Masker {
    fn clone(&self) -> Self {
        let mut m = Masker { input: Vec::new(), acc: Vec::new(), touched: Vec::new(), buf: Vec::new(), scratch: Vec::new(), gains: Vec::new(), ..self.shallow() };
        m.clone_from(self);
        m
    }

    fn clone_from(&mut self, o: &Self) {
        let (mut input, mut acc, mut touched) = (std::mem::take(&mut self.input), std::mem::take(&mut self.acc), std::mem::take(&mut self.touched));
        let (mut buf, mut scratch, mut gains) = (std::mem::take(&mut self.buf), std::mem::take(&mut self.scratch), std::mem::take(&mut self.gains));
        input.clone_from(&o.input);
        acc.clone_from(&o.acc);
        touched.clone_from(&o.touched);
        // Scratch: only its size matters.
        buf.resize(o.buf.len(), Complex32::default());
        scratch.resize(o.scratch.len(), Complex32::default());
        gains.resize(o.gains.len(), 1.0);
        *self = Masker { input, acc, touched, buf, scratch, gains, ..o.shallow() };
    }
}

impl Masker {
    /// A masker for `channels` channels of `encoding` at `rate`, vocals at `level` (0 to 1).
    pub fn new(rate: u32, channels: usize, encoding: Encoding, level: f32) -> Masker {
        // About 46 ms at any rate; a power of two for the transform.
        let n = if rate <= 50_000 { 2048 } else if rate <= 100_000 { 4096 } else { 8192 };
        let hop = n / 4;
        let mut planner = FftPlanner::<f32>::new();
        let (fft, ifft) = (planner.plan_fft_forward(n), planner.plan_fft_inverse(n));
        let window: Arc<[f32]> = (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()).sqrt() as f32).collect();
        let edges = band_edges();
        let band: Arc<[u16]> = (0..=n / 2).map(|k| band_of(&edges, k as f64 * rate as f64 / n as f64) as u16).collect();
        let channels = channels.max(1);
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len().max(ifft.get_inplace_scratch_len())];
        Masker {
            n,
            hop,
            channels,
            rate,
            encoding,
            level: level.clamp(0.0, 1.0),
            fft,
            ifft,
            window,
            band,
            input: vec![0.0; n * channels],
            filled: 0,
            acc: vec![0.0; n * channels],
            touched: vec![false; 4],
            skip: n - hop,
            held: 0,
            pts: 0.0,
            pace: 1.0,
            buf: vec![Complex32::default(); n],
            scratch,
            gains: vec![1.0; n / 2 + 1],
            dither: Dither::new(),
        }
    }

    /// Everything but the buffers.
    fn shallow(&self) -> Masker {
        Masker {
            fft: self.fft.clone(),
            ifft: self.ifft.clone(),
            window: self.window.clone(),
            band: self.band.clone(),
            input: Vec::new(),
            acc: Vec::new(),
            touched: Vec::new(),
            buf: Vec::new(),
            scratch: Vec::new(),
            gains: Vec::new(),
            ..*self
        }
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn set_level(&mut self, level: f32) {
        self.level = level.clamp(0.0, 1.0);
    }

    /// Input frames held back before their output (the look-ahead).
    pub fn delay_frames(&self) -> usize {
        self.n - self.hop
    }

    /// Forgets the input, as after a seek.
    pub fn reset(&mut self) {
        self.input.fill(0.0);
        self.acc.fill(0.0);
        self.touched.fill(false);
        self.filled = 0;
        self.skip = self.n - self.hop;
        self.held = 0;
        self.dither.reset();
    }

    /// Masks `input` (interleaved, its first frame at timeline position `pts_us`, `pace` song frames per frame) with
    /// the songs' masks in `masks`, appending what is ready to `out`.
    pub fn process(&mut self, input: &[u8], pts_us: i64, pace: f64, masks: &[Placed], out: &mut Vec<u8>) {
        self.pts = pts_us as f64;
        self.pace = pace;
        let width = self.encoding.width();
        for frame in input.chunks_exact(width * self.channels) {
            for c in 0..self.channels {
                let b = &frame[c * width..(c + 1) * width];
                let v = match self.encoding {
                    Encoding::Pcm16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                    Encoding::Float => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                };
                self.input[c * self.n + self.n - self.hop + self.filled] = v;
            }
            self.held += 1;
            self.advance(masks, out);
        }
    }

    /// The input still held, out through the window's end (silence after it), into `out`.
    pub fn end(&mut self, masks: &[Placed], out: &mut Vec<u8>) {
        while self.held > 0 {
            for c in 0..self.channels {
                self.input[c * self.n + self.n - self.hop + self.filled] = 0.0;
            }
            self.advance(masks, out);
        }
        self.reset();
    }

    /// One more input frame is in; at a hop's end, the frame is transformed and a hop output (no more than was put in).
    fn advance(&mut self, masks: &[Placed], out: &mut Vec<u8>) {
        self.filled += 1;
        self.pts += self.pace * 1e6 / self.rate as f64;
        if self.filled < self.hop {
            return;
        }
        self.filled = 0;
        // The frame's centre, half a window before the next input frame.
        let centre = self.pts - (self.n / 2) as f64 * self.pace * 1e6 / self.rate as f64;
        let masked = self.gains_at(centre.round() as i64, masks);
        let (n, hop) = (self.n, self.hop);
        for c in 0..self.channels {
            let x = &self.input[c * n..(c + 1) * n];
            let acc = &mut self.acc[c * n..(c + 1) * n];
            if !masked {
                // w * w sums to 2 over four overlapping frames.
                for ((a, v), w) in acc.iter_mut().zip(x).zip(self.window.iter()) {
                    *a += v * w * w * 0.5;
                }
                continue;
            }
            for ((b, v), w) in self.buf.iter_mut().zip(x).zip(self.window.iter()) {
                *b = Complex32::new(v * w, 0.0);
            }
            self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            for (k, g) in self.gains.iter().enumerate() {
                self.buf[k] *= *g;
                if k > 0 && k < n / 2 {
                    self.buf[n - k] *= *g;
                }
            }
            self.ifft.process_with_scratch(&mut self.buf, &mut self.scratch);
            let scale = 0.5 / n as f32;
            for ((a, b), w) in acc.iter_mut().zip(&self.buf).zip(self.window.iter()) {
                *a += b.re * w * scale;
            }
        }
        if masked {
            self.touched.fill(true);
        }
        // The oldest hop is whole: out, unless it is before the first input frame.
        let drop = self.skip.min(hop);
        self.skip -= drop;
        let emit = (hop - drop).min(self.held);
        let touched = self.touched[0];
        let width = self.encoding.width();
        let start = out.len();
        out.resize(start + emit * self.channels * width, 0);
        for i in 0..emit {
            for c in 0..self.channels {
                let at = c * n + drop + i;
                let v = if touched { self.acc[at] } else { self.input[at] };
                let o = start + (i * self.channels + c) * width;
                match self.encoding {
                    Encoding::Pcm16 => out[o..o + 2].copy_from_slice(&self.dither.to_i16(c, v as f64).to_le_bytes()),
                    Encoding::Float => out[o..o + 4].copy_from_slice(&v.to_le_bytes()),
                }
            }
        }
        self.held -= emit;
        for c in 0..self.channels {
            self.input[c * n..(c + 1) * n].copy_within(hop.., 0);
            let acc = &mut self.acc[c * n..(c + 1) * n];
            acc.copy_within(hop.., 0);
            acc[n - hop..].fill(0.0);
        }
        self.touched.copy_within(1.., 0);
        *self.touched.last_mut().expect("four hops") = false;
    }

    /// Fills the bins' gains for a frame centred at timeline position `pts`; false when they are all 1.
    fn gains_at(&mut self, pts: i64, masks: &[Placed]) -> bool {
        if self.level >= 1.0 {
            return false;
        }
        let Some(p) = masks.iter().find(|p| p.at.contains(&pts)) else { return false };
        let m = &p.mask;
        let f = (pts - p.at.start) as f64 / 1e6 * m.fps as f64;
        if f.is_nan() || f < 0.0 || m.frames() == 0 {
            return false;
        }
        let k = (f as usize).min(m.frames() - 1);
        let (a, b, t) = (m.row(k), m.row((k + 1).min(m.frames() - 1)), (f - k as f64).min(1.0) as f32);
        if a.iter().chain(b).all(|v| *v == 0) {
            return false;
        }
        let cut = (1.0 - self.level) / 255.0;
        for (g, band) in self.gains.iter_mut().zip(self.band.iter()) {
            let (x, y) = (a[*band as usize] as f32, b[*band as usize] as f32);
            *g = 1.0 - cut * (x + (y - x) * t);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn floats(x: &[f32]) -> Vec<u8> {
        x.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn samples(b: &[u8]) -> Vec<f32> {
        b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
    }

    /// A whole song's mask of `value` everywhere.
    fn flat(value: u8, seconds: f64) -> Arc<VocalMask> {
        let fps = (MODEL_RATE / MODEL_HOP as f64) as f32;
        Arc::new(VocalMask::new(fps, vec![value; (seconds * fps as f64) as usize * bands()]))
    }

    /// Runs interleaved stereo `x` through a masker in buffers of 1000 frames; returns its whole output.
    fn run(m: &mut Masker, x: &[f32], masks: &[Placed]) -> Vec<f32> {
        let mut out = Vec::new();
        for (k, chunk) in x.chunks(2000).enumerate() {
            m.process(&floats(chunk), (k as f64 * 1000.0 * 1e6 / RATE as f64) as i64, 1.0, masks, &mut out);
        }
        m.end(masks, &mut out);
        samples(&out)
    }

    fn whole(mask: Arc<VocalMask>) -> Vec<Placed> {
        vec![Placed { at: 0..i64::MAX, mask }]
    }

    fn rms(x: impl Iterator<Item = f32>) -> f32 {
        let (s, n) = x.fold((0.0f64, 0usize), |(s, n), v| (s + (v as f64).powi(2), n + 1));
        (s / n.max(1) as f64).sqrt() as f32
    }

    #[test]
    fn mask_round_trip_and_version() {
        let m = VocalMask::new(43.07, (0..bands() * 3).map(|i| i as u8).collect());
        assert_eq!(bands(), 192);
        let b = m.to_bytes();
        assert_eq!(VocalMask::from_bytes(&b), Ok(m));
        let mut old = b.clone();
        old[4] = VERSION + 1;
        assert_eq!(VocalMask::from_bytes(&old), Err(MaskError::OtherVersion));
        assert_eq!(VocalMask::from_bytes(&b[..b.len() - 1]), Err(MaskError::Truncated));
        assert_eq!(VocalMask::from_bytes(b"RIFF...."), Err(MaskError::NotAMask));
    }

    /// Every input frame comes out once, in place, whatever the mask: a full vocal share at full level and no mask
    /// at all are both the input; the transform's own path (a mask, level just under 1) is within rounding of it.
    #[test]
    fn all_ones_mask_at_full_level_leaves_samples() {
        let x: Vec<f32> = (0..2 * 30_000).map(|i| (i as f32 * 0.013).sin() * 0.5 + (i as f32 * 0.0021).cos() * 0.3).collect();
        for (masks, level, exact) in [(whole(flat(255, 2.0)), 1.0, true), (Vec::new(), 0.0, true), (whole(flat(255, 2.0)), 0.999_999, false)] {
            let mut m = Masker::new(RATE, 2, Encoding::Float, level);
            let y = run(&mut m, &x, &masks);
            assert_eq!(y.len(), x.len(), "as many frames out as in");
            if exact {
                assert_eq!(y, x);
            } else {
                let worst = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(worst < 1e-4, "the transform reconstructs the input: {worst}");
            }
        }
        // 16-bit stays bit-exact where nothing is masked.
        let pcm: Vec<u8> = (0..2 * 5000).flat_map(|i| (((i * 37) % 2000) as i16 - 1000).to_le_bytes()).collect();
        let mut m = Masker::new(RATE, 2, Encoding::Pcm16, 0.0);
        let mut out = Vec::new();
        m.process(&pcm, 0, 1.0, &[], &mut out);
        m.end(&[], &mut out);
        assert_eq!(out, pcm);
    }

    /// A centred voice with a mask on its band is turned down; a hard-panned tone outside it stays.
    #[test]
    fn centred_voice_goes_panned_tone_stays() {
        let voice_hz = 440.0;
        let tone_hz = 3000.0;
        let n = RATE as usize * 2;
        let x: Vec<f32> = (0..n)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                let voice = 0.3 * (std::f32::consts::TAU * voice_hz * t).sin();
                let tone = 0.3 * (std::f32::consts::TAU * tone_hz * t).sin();
                [voice + tone, voice]
            })
            .collect();
        // Vocals where the voice is, nothing elsewhere.
        let edges = band_edges();
        let fps = (MODEL_RATE / MODEL_HOP as f64) as f32;
        let row: Vec<u8> = (0..bands()).map(|b| if (b as i64 - band_of(&edges, voice_hz as f64) as i64).abs() <= 12 { 255 } else { 0 }).collect();
        let mask = Arc::new(VocalMask::new(fps, row.repeat(3 * fps as usize)));
        let mut m = Masker::new(RATE, 2, Encoding::Float, 0.0);
        let y = run(&mut m, &x, &whole(mask));
        let (left, right): (Vec<f32>, Vec<f32>) = y[RATE as usize / 2 * 2..].chunks(2).map(|f| (f[0], f[1])).unzip();
        // The right channel held only the voice; the left keeps the tone at its level.
        assert!(rms(right.iter().copied()) < 0.3 / 2f32.sqrt() * 0.1, "the voice is down by more than 20 dB: {}", rms(right.iter().copied()));
        let tone_rms = 0.3 / 2f32.sqrt();
        assert!((rms(left.iter().copied()) / tone_rms - 1.0).abs() < 0.05, "the tone stays: {}", rms(left.iter().copied()));
    }

    /// The level is heard in proportion, and only from the song's own place on the timeline.
    #[test]
    fn level_and_placement() {
        let x: Vec<f32> = (0..2 * RATE as usize).flat_map(|i| [(i as f32 * 0.05).sin() * 0.5; 2]).collect();
        let at = |level: f32, masks: &[Placed]| {
            let mut m = Masker::new(RATE, 2, Encoding::Float, level);
            rms(run(&mut m, &x, masks)[RATE as usize..].iter().copied())
        };
        let full = at(1.0, &whole(flat(255, 3.0)));
        assert!((at(0.5, &whole(flat(255, 3.0))) / full - 0.5).abs() < 0.02);
        assert!(at(0.0, &whole(flat(255, 3.0))) < full * 0.01);
        // Another song's place: untouched.
        let elsewhere = vec![Placed { at: 10_000_000..20_000_000, mask: flat(255, 3.0) }];
        assert_eq!(at(0.0, &elsewhere), full);
    }
}
