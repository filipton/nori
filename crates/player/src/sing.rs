//! Sing: a song's vocals turned down to a level the listener picks. Open-Unmix's vocals model ([`model`], behind
//! `neural-beats`) makes each song's [`VocalMask`] from its samples as the player decodes them ([`Feed`], [`MaskMaker`]):
//! the vocal share of each time-frequency cell. In the chain the [`Masker`] plays `mix * (1 - (1 - level) * mask)`
//! through a short-time Fourier transform; the sink holds back input whose rows have not come while the track holds
//! enough ahead of the ear (`crate::sink`). A song or a stretch without a mask plays unchanged.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::dither::Dither;
use crate::pcm::Encoding;

mod feed;
mod maker;
#[cfg(feature = "neural-beats")]
pub mod model;

pub use feed::{Feed, Feeding};
pub use maker::{MaskMaker, Separator, IN_BINS};

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
/// band, both channels together. Whole when read back, or growing while its maker's thread puts rows and the player
/// reads them: a row is read only once its bit in `done` says it is in.
pub struct VocalMask {
    pub fps: f32,
    bands: usize,
    cells: Box<[AtomicU8]>,
    /// A bit per frame: its row is in.
    done: Box<[AtomicU64]>,
    rows: AtomicUsize,
    /// The song's frames once its end was read; the room until then.
    frames: AtomicUsize,
    ended: AtomicBool,
    /// Its making was given up: nothing waits for it.
    given_up: AtomicBool,
    /// The player waits for rows not in yet.
    awaited: AtomicBool,
    /// Frames fed for it and not yet made into rows.
    coming: AtomicU64,
}

impl VocalMask {
    /// A whole mask, from rows of [`bands`] bytes each.
    pub fn new(fps: f32, data: Vec<u8>) -> VocalMask {
        let m = VocalMask::growing(fps, data.len() / bands());
        for (k, row) in data.chunks_exact(m.bands).enumerate() {
            m.put(k, row);
        }
        m.end(m.frames());
        m
    }

    /// An empty mask with room for `frames` rows.
    pub fn growing(fps: f32, frames: usize) -> VocalMask {
        let bands = bands();
        VocalMask {
            fps,
            bands,
            cells: (0..frames * bands).map(|_| AtomicU8::new(0)).collect(),
            done: (0..frames.div_ceil(64)).map(|_| AtomicU64::new(0)).collect(),
            rows: AtomicUsize::new(0),
            frames: AtomicUsize::new(frames),
            ended: AtomicBool::new(false),
            given_up: AtomicBool::new(false),
            coming: AtomicU64::new(0),
            awaited: AtomicBool::new(false),
        }
    }

    /// The song's frames once its end is known, else the room.
    pub fn frames(&self) -> usize {
        self.frames.load(Ordering::Acquire)
    }

    /// Whether row `k` is in.
    pub fn has(&self, k: usize) -> bool {
        k < self.frames() && self.done[k / 64].load(Ordering::Acquire) & (1 << (k % 64)) != 0
    }

    /// Row `k`'s share in `band`, once [`VocalMask::has`] it.
    pub fn cell(&self, k: usize, band: usize) -> u8 {
        self.cells[k * self.bands + band].load(Ordering::Relaxed)
    }

    fn row_is_zero(&self, k: usize) -> bool {
        self.cells[k * self.bands..(k + 1) * self.bands].iter().all(|c| c.load(Ordering::Relaxed) == 0)
    }

    /// Puts row `k`; one past the room, or already in, is left as it is.
    pub fn put(&self, k: usize, row: &[u8]) {
        if k >= self.frames() || self.has(k) {
            return;
        }
        for (c, v) in self.cells[k * self.bands..(k + 1) * self.bands].iter().zip(row) {
            c.store(*v, Ordering::Relaxed);
        }
        self.done[k / 64].fetch_or(1 << (k % 64), Ordering::Release);
        self.rows.fetch_add(1, Ordering::AcqRel);
    }

    /// The song has `frames` frames.
    pub fn end(&self, frames: usize) {
        self.frames.fetch_min(frames, Ordering::AcqRel);
        self.ended.store(true, Ordering::Release);
    }

    fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Every row of the song is in.
    pub fn whole(&self) -> bool {
        self.ended() && self.first_missing(0) == self.frames()
    }

    /// Some row is in.
    pub fn begun(&self) -> bool {
        self.rows.load(Ordering::Acquire) > 0
    }

    /// Nothing waits for it any more.
    pub fn give_up(&self) {
        self.given_up.store(true, Ordering::Release);
    }

    pub fn given_up(&self) -> bool {
        self.given_up.load(Ordering::Acquire)
    }

    /// `n` more frames were fed for it.
    fn fed(&self, n: u64) {
        self.coming.fetch_add(n, Ordering::AcqRel);
    }

    /// `n` frames fed were made into what rows they can make.
    pub fn made(&self, n: u64) {
        self.coming.fetch_sub(n, Ordering::AcqRel);
    }

    /// The player waits for rows not in yet.
    pub fn await_rows(&self) {
        self.awaited.store(true, Ordering::Release);
    }

    /// Whether the player waited for rows since last asked.
    pub fn awaited(&self) -> bool {
        self.awaited.swap(false, Ordering::AcqRel)
    }

    /// Rows are being made of frames already fed.
    pub fn rows_coming(&self) -> bool {
        self.coming.load(Ordering::Acquire) > 0
    }

    /// The first row from `k` on that is not in (the song's frames when all are).
    fn first_missing(&self, k: usize) -> usize {
        let frames = self.frames();
        let mut k = k.min(frames);
        while k < frames {
            let missing = !self.done[k / 64].load(Ordering::Acquire) >> (k % 64);
            if missing != 0 {
                return (k + missing.trailing_zeros() as usize).min(frames);
            }
            k = (k / 64 + 1) * 64;
        }
        frames
    }

    /// The bytes it is kept in.
    pub fn to_bytes(&self) -> Vec<u8> {
        let frames = self.frames();
        let mut b = Vec::with_capacity(HEADER + frames * self.bands);
        b.extend_from_slice(MAGIC);
        b.push(VERSION);
        b.extend_from_slice(&(self.bands as u16).to_le_bytes());
        b.extend_from_slice(&self.fps.to_le_bytes());
        b.extend_from_slice(&(frames as u32).to_le_bytes());
        b.extend(self.cells[..frames * self.bands].iter().map(|c| c.load(Ordering::Relaxed)));
        b
    }

    /// A whole mask from its bytes.
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
        Ok(VocalMask::new(fps, data.to_vec()))
    }
}

/// Alike when both are whole or both not, with the same rows.
impl PartialEq for VocalMask {
    fn eq(&self, o: &VocalMask) -> bool {
        self.whole() == o.whole() && self.to_bytes() == o.to_bytes()
    }
}

impl std::fmt::Debug for VocalMask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whole = if self.whole() { ", whole" } else { "" };
        write!(f, "VocalMask {{ fps: {}, {} of {} rows{whole} }}", self.fps, self.rows.load(Ordering::Relaxed), self.frames())
    }
}

/// A song's mask where the song is on the timeline (µs, as the chain's input is stamped).
#[derive(Debug, Clone)]
pub struct Placed {
    pub at: Range<i64>,
    pub mask: Arc<VocalMask>,
}

impl Placed {
    /// How far on from timeline position `pts` the masker has the rows it reads: each moment's frame and the one
    /// after it. `i64::MAX` when none is missing, or none is awaited.
    pub fn ready_until(&self, pts: i64) -> i64 {
        let m = &self.mask;
        if m.given_up() {
            return i64::MAX;
        }
        let k = ((pts - self.at.start).max(0) as f64 / 1e6 * m.fps as f64) as usize + 1;
        let missing = m.first_missing(k);
        if missing >= m.frames() && m.ended() {
            return i64::MAX;
        }
        self.at.start + (missing.saturating_sub(1) as f64 * 1e6 / m.fps as f64) as i64
    }
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
    /// Per hop of `acc`, whether a masked frame or scaled input reached it (else its output is the input, bit for
    /// bit).
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

/// `clone_from` keeps every buffer's memory.
impl Clone for Masker {
    fn clone(&self) -> Self {
        let mut m = self.shallow();
        m.clone_from(self);
        m
    }

    fn clone_from(&mut self, o: &Self) {
        self.store_from(o);
        // Scratch: only its size matters.
        let (n, scratch) = (self.n, self.scratch_len());
        self.buf.resize(n, Complex32::default());
        self.scratch.resize(scratch, Complex32::default());
        self.gains.resize(n / 2 + 1, 1.0);
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

    fn scratch_len(&self) -> usize {
        self.fft.get_inplace_scratch_len().max(self.ifft.get_inplace_scratch_len())
    }

    /// A copy of the state without the scratch it runs in, for keeping: [`Clone::clone_from`] runs it again.
    pub fn stored(&self) -> Masker {
        let mut m = self.shallow();
        m.store_from(self);
        m
    }

    /// [`Masker::stored`] into this one's memory.
    pub fn store_from(&mut self, o: &Self) {
        let (mut input, mut acc, mut touched) = (std::mem::take(&mut self.input), std::mem::take(&mut self.acc), std::mem::take(&mut self.touched));
        let (buf, scratch, gains) = (std::mem::take(&mut self.buf), std::mem::take(&mut self.scratch), std::mem::take(&mut self.gains));
        input.clone_from(&o.input);
        acc.clone_from(&o.acc);
        touched.clone_from(&o.touched);
        *self = Masker { input, acc, touched, buf, scratch, gains, ..o.shallow() };
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

    /// Masks `input` (interleaved, its first frame at timeline position `pts_us`, `pace` song frames per frame,
    /// scaled by `gain` as it is read) with the songs' masks in `masks`, appending what is ready to `out`.
    pub fn process(&mut self, input: &[u8], pts_us: i64, pace: f64, gain: f32, masks: &[Placed], out: &mut Vec<u8>) {
        self.pts = pts_us as f64;
        self.pace = pace;
        let fb = self.encoding.width() * self.channels;
        let mut frames = &input[..input.len() / fb * fb];
        while !frames.is_empty() {
            // Up to the hop's end at once; `advance` counts the last frame of it.
            let take = (self.hop - self.filled).min(frames.len() / fb);
            let (now, rest) = frames.split_at(take * fb);
            self.read(now, gain);
            // Scaled input is off the 16-bit grid: out through the dither.
            *self.touched.last_mut().expect("four hops") |= gain != 1.0;
            self.held += take;
            self.filled += take - 1;
            self.pts += (take - 1) as f64 * self.pace * 1e6 / self.rate as f64;
            self.advance(masks, out);
            frames = rest;
        }
    }

    /// Interleaved frames into `input` at `gain`, from the `filled`th of the hop on.
    fn read(&mut self, frames: &[u8], gain: f32) {
        let (width, from) = (self.encoding.width(), self.n - self.hop + self.filled);
        for (i, frame) in frames.chunks_exact(width * self.channels).enumerate() {
            for (c, b) in frame.chunks_exact(width).enumerate() {
                self.input[c * self.n + from + i] = match self.encoding {
                    Encoding::Pcm16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                    Encoding::Float => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                } * gain;
            }
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
        if masked {
            self.mask_frame();
            self.touched.fill(true);
        } else {
            for (acc, x) in self.acc.chunks_exact_mut(n).zip(self.input.chunks_exact(n)) {
                // w * w sums to 2 over four overlapping frames.
                for ((a, v), w) in acc.iter_mut().zip(x).zip(self.window.iter()) {
                    *a += v * w * w * 0.5;
                }
            }
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
                    // Untouched input is on the 16-bit grid, where the dither changes nothing.
                    Encoding::Pcm16 if !touched => out[o..o + 2].copy_from_slice(&((v * 32768.0) as i16).to_le_bytes()),
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

    /// The frame in `input` through the gains, overlap-added into `acc`. Channels go two to a transform, one as
    /// the real part and one as the imaginary: the gains are real and even, so they come back apart.
    fn mask_frame(&mut self) {
        let n = self.n;
        let scale = 0.5 / n as f32;
        let (window, buf) = (&self.window[..], &mut self.buf[..]);
        for (x, acc) in self.input.chunks(2 * n).zip(self.acc.chunks_mut(2 * n)) {
            let (l, r) = x.split_at(n);
            if r.is_empty() {
                for ((b, u), w) in buf.iter_mut().zip(l).zip(window) {
                    *b = Complex32::new(u * w, 0.0);
                }
            } else {
                for (((b, u), v), w) in buf.iter_mut().zip(l).zip(r).zip(window) {
                    *b = Complex32::new(u * w, v * w);
                }
            }
            self.fft.process_with_scratch(buf, &mut self.scratch);
            for (k, g) in self.gains.iter().enumerate() {
                buf[k] *= *g;
                if k > 0 && k < n / 2 {
                    buf[n - k] *= *g;
                }
            }
            self.ifft.process_with_scratch(buf, &mut self.scratch);
            let (al, ar) = acc.split_at_mut(n);
            for ((a, b), w) in al.iter_mut().zip(buf.iter()).zip(window) {
                *a += b.re * w * scale;
            }
            for ((a, b), w) in ar.iter_mut().zip(buf.iter()).zip(window) {
                *a += b.im * w * scale;
            }
        }
    }

    /// Fills the bins' gains for a frame centred at timeline position `pts`; false when they are all 1 (no mask,
    /// or its row not in yet).
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
        // Past a whole song's last frame, its last row.
        let k = if m.ended() { (f as usize).min(m.frames() - 1) } else { f as usize };
        if !m.has(k) {
            return false;
        }
        let (b, t) = if m.has(k + 1) { (k + 1, (f - k as f64).min(1.0) as f32) } else { (k, 0.0) };
        if m.row_is_zero(k) && m.row_is_zero(b) {
            return false;
        }
        let cut = (1.0 - self.level) / 255.0;
        for (g, band) in self.gains.iter_mut().zip(self.band.iter()) {
            let (x, y) = (m.cell(k, *band as usize) as f32, m.cell(b, *band as usize) as f32);
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
            m.process(&floats(chunk), (k as f64 * 1000.0 * 1e6 / RATE as f64) as i64, 1.0, 1.0, masks, &mut out);
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
        m.process(&pcm, 0, 1.0, 1.0, &[], &mut out);
        m.end(&[], &mut out);
        assert_eq!(out, pcm);
    }

    /// 16-bit input around a masked second comes out bit for bit, before it and after it.
    #[test]
    fn unmasked_16_bit_around_a_masked_stretch_is_exact() {
        let frames = 3 * RATE as usize;
        let pcm: Vec<u8> = (0..2 * frames).flat_map(|i| ((((i * 7919) % 20_000) as i32 - 10_000) as i16).to_le_bytes()).collect();
        let masks = vec![Placed { at: 1_000_000..2_000_000, mask: flat(255, 2.0) }];
        let mut m = Masker::new(RATE, 2, Encoding::Pcm16, 0.3);
        let mut out = Vec::new();
        for (k, chunk) in pcm.chunks(4 * 1000).enumerate() {
            m.process(chunk, (k as f64 * 1000.0 * 1e6 / RATE as f64) as i64, 1.0, 1.0, &masks, &mut out);
        }
        m.end(&masks, &mut out);
        assert_eq!(out.len(), pcm.len());
        let (before, after) = (4 * (RATE as usize * 9 / 10), 4 * (RATE as usize * 21 / 10));
        assert_ne!(out[before..after], pcm[before..after], "masked in between");
        assert_eq!(out[..before], pcm[..before]);
        assert_eq!(out[after..], pcm[after..]);
    }

    /// A stored copy, made live again (into a masker or as a new one), goes on as the original does.
    #[test]
    fn stored_copy_goes_on_alike() {
        let x = floats(&(0..2 * 20_000).map(|i| (i as f32 * 0.013).sin() * 0.5).collect::<Vec<_>>());
        let masks = whole(flat(200, 2.0));
        let mut m = Masker::new(RATE, 2, Encoding::Pcm16, 0.3);
        let (head, tail) = x.split_at(8 * 7000);
        m.process(head, 0, 1.0, 1.0, &masks, &mut Vec::new());
        let kept = m.stored();
        let mut into = Masker::new(RATE, 2, Encoding::Pcm16, 1.0);
        into.clone_from(&kept);
        let at = (7000.0 * 1e6 / RATE as f64) as i64;
        let mut outs = Vec::new();
        for mut live in [m, kept.clone(), into] {
            let mut out = Vec::new();
            live.process(tail, at, 1.0, 1.0, &masks, &mut out);
            live.end(&masks, &mut out);
            outs.push(out);
        }
        assert_eq!(outs[1], outs[0]);
        assert_eq!(outs[2], outs[0]);
    }

    /// Mono and an odd third channel go through the transform as stereo does: rebuilt at a level just under 1,
    /// silenced at 0.
    #[test]
    fn odd_channel_counts_are_masked() {
        for channels in [1, 3] {
            let x: Vec<f32> = (0..channels * 30_000).map(|i| ((i / channels) as f32 * 0.013 * (1 + i % channels) as f32).sin() * 0.5).collect();
            let masked = |level: f32| {
                let mut m = Masker::new(RATE, channels, Encoding::Float, level);
                let mut out = Vec::new();
                for (k, chunk) in x.chunks(channels * 1000).enumerate() {
                    m.process(&floats(chunk), (k as f64 * 1000.0 * 1e6 / RATE as f64) as i64, 1.0, 1.0, &whole(flat(255, 2.0)), &mut out);
                }
                m.end(&whole(flat(255, 2.0)), &mut out);
                samples(&out)
            };
            let y = masked(0.999_999);
            assert_eq!(y.len(), x.len());
            let worst = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-4, "{channels} channels rebuilt: {worst}");
            for c in 0..channels {
                let quiet = rms(masked(0.0).iter().skip(channels * 5000 + c).step_by(channels).copied());
                assert!(quiet < 1e-3, "{channels} channels, channel {c} silenced: {quiet}");
            }
        }
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
