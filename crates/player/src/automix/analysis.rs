//! Streaming front end of the track analysis. Mono samples go in at any rate, in any buffer size (stereo ones are
//! measured on their downmix, and their side only tells the vocal curve where the middle is); what comes out is a
//! handful of per-frame feature curves (onset strength, low-band onset, power, chroma) and 100 ms loudness blocks.
//! All the expensive work (two FFTs, K-weighting) happens here, once per sample, so the same code serves a whole
//! decoded file (`analyse`) and the songs decoded ahead or as they come (nori-engine's measurer). `finish` then runs the
//! cheap whole-track steps: tempo, beats, downbeats, phrases, key.
//!
//! **Rate.** The input is decimated by an integer factor to about 22 kHz (44.1 -> 22.05, 48 -> 24, 96 -> 24 kHz)
//! with a boxcar pre-filter, and the FFT sizes and hop are then picked in seconds, not samples. Integer decimation
//! costs one add per input sample, needs no resampler state, and keeps every frame the same length in time within
//! a few percent; the leftover aliasing lands above 5 kHz, where the onset detector only needs "something changed"
//! and chroma does not look at all. All timing downstream goes through `fps`, so the exact rate never matters.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use super::loudness::Meter;
use super::vocal;

pub const TARGET_RATE: f64 = 22050.0;
/// Hop between onset frames, seconds (256 samples at 22.05 kHz, 86 frames/s).
/// What an analysis of a song of unknown length is sized for.
const UNKNOWN_LENGTH_MS: u64 = 8 * 60_000;
const HOP_S: f64 = 256.0 / 22050.0;
/// Log-spaced bands for the spectral flux, 30 Hz up to 11 kHz (or Nyquist).
const BANDS: usize = 40;
const FLUX_LO_HZ: f64 = 30.0;
const FLUX_HI_HZ: f64 = 11000.0;
/// The "kick" band for downbeat voting and bass energy.
pub const LOW_HZ: f64 = 150.0;
/// The "voice" band for vocal-activity estimation: most speech and sung energy sits here.
pub const VOCAL_LO_HZ: f64 = 300.0;
pub const VOCAL_HI_HZ: f64 = 3400.0;
/// Log compression: linear below -60 dB, logarithmic above, so dither does not become onsets.
const GAMMA: f32 = 1000.0;
/// One chroma frame per this many onset frames (about 93 ms), from a 4x longer FFT.
pub const CHROMA_EVERY: usize = 8;
const CHROMA_LO_HZ: f64 = 100.0;
const CHROMA_HI_HZ: f64 = 2500.0;
/// Resolution of the whole-song pitch profile the key is read from: 10 cents.
pub const PITCH_SLOTS: usize = 120;
/// Peaks below this are too coarsely resolved by the long FFT (5.4 Hz bins) to say where A is.
const TUNING_LO_HZ: f64 = 250.0;
/// Where an onset frame sits in time relative to the end of its window, in hops. Measured on click tracks
/// (`beat_times_line_up_with_the_clicks`): log-compressed flux peaks as soon as a click enters the window.
const FRAME_LAG_HOPS: f64 = 1.28;

/// The per-frame curves `finish` works from.
pub struct Features {
    /// Frames per second of the onset curves.
    pub fps: f64,
    /// Time of frame 0, seconds; frame `k` is at `t0 + k / fps`.
    pub t0: f64,
    /// Spectral flux, log-compressed, summed over bands.
    pub onset: Vec<f32>,
    /// The same below `LOW_HZ`.
    pub low_onset: Vec<f32>,
    /// Mean square of the frame, and of its part below `LOW_HZ`.
    pub power: Vec<f32>,
    pub low_power: Vec<f32>,
    /// Share of the frame's power in the voice band (`VOCAL_LO_HZ..VOCAL_HI_HZ`), 0..1.
    pub vocal: Vec<f32>,
    /// Spectral centroid of the frame in Hz: the track's brightness over time.
    pub centroid: Vec<f32>,
    /// Pitch-class magnitudes, one frame per `CHROMA_EVERY` onset frames; chroma frame `j` is at `chroma_t0 + j * chroma_step`.
    pub chroma: Vec<[f32; 12]>,
    pub chroma_t0: f64,
    pub chroma_step: f64,
    /// The whole song's pitch-class magnitudes in 10-cent slots (slot `s` is `s / 10` semitones above C at
    /// A = 440 Hz), summed over every chroma frame: what the key is read from once the tuning is known.
    pub pitch: Vec<f64>,
    /// Where the spectral peaks sit between semitones: magnitude-weighted sums of cos and sin of 2π times
    /// the peak's distance from the nearest equal-tempered pitch. Their angle is the tuning.
    pub tuning_cs: (f64, f64),
    /// 100 ms mean squares at the native rate, K-weighted and plain.
    pub blocks_k: Vec<f32>,
    pub blocks_raw: Vec<f32>,
    pub duration_s: f64,
    /// Pitched movement in the voice band, one value per `vocal::CURVE_EVERY` onset frames: what the vocal
    /// activity curve is made of (`voice_curve`).
    pub voice: Vec<f32>,
}

impl Features {
    /// The song's vocal activity curve, as it is stored beside its analysis.
    pub fn voice_curve(&self) -> vocal::VocalCurve {
        vocal::VocalCurve::from_raw(&self.voice, self.fps, self.t0)
    }
}

pub struct Analyzer {
    rate: f64,
    dec: usize,
    sr: f64,
    acc: f32,
    acc_n: usize,
    /// The last `cn` decimated samples; `cn` is a power of two.
    ring: Vec<f32>,
    /// A stereo source's side, (L - R) / 2, decimated alike: the last `n` samples, 0 for a mono source.
    side_ring: Vec<f32>,
    side_acc: f32,
    /// The frames computed while `written` is at most this have a side in their window, and take the
    /// stereo path (`frame`).
    side_until: usize,
    /// The side's spectrum, `n / 2 + 1` bins.
    side_buf: Vec<Complex32>,
    written: usize,
    hop: usize,
    since_hop: usize,
    hops: usize,
    n: usize,
    cn: usize,
    fft: Arc<dyn Fft<f32>>,
    cfft: Arc<dyn Fft<f32>>,
    win: Vec<f32>,
    cwin: Vec<f32>,
    /// Normalisers turning |X|² into the mean square of the windowed frame, and |X| into a sine amplitude.
    pow_norm: f32,
    amp_norm: f32,
    camp_norm: f32,
    buf: Vec<Complex32>,
    cbuf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Bin range per flux band; bins are ascending so each band is contiguous.
    bands: Vec<(usize, usize)>,
    low_bins: usize,
    prev: [f32; BANDS],
    prev_low: [f32; 8],
    level: [f32; BANDS],
    chroma_map: Vec<(u32, u8, f32)>,
    /// Chroma bin -> 10-cent pitch-class slot.
    pitch_map: Vec<(u32, u8)>,
    /// Long-FFT bins searched for tuning peaks, and a scratch row of their magnitudes.
    tune_bins: (usize, usize),
    mags: Vec<f32>,
    meter: Meter,
    samples: u64,
    voice: vocal::Tracker,
    f: Features,
}

fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect()
}

impl Analyzer {
    /// `expected_ms` (0 if unknown) only sizes the buffers up front.
    pub fn new(rate: u32, expected_ms: u64) -> Self {
        let rate = rate.max(1000) as f64;
        let dec = ((rate / TARGET_RATE).round() as usize).max(1);
        let sr = rate / dec as f64;
        let n = ((0.04 * sr) as usize).next_power_of_two().max(256);
        let cn = n * 4;
        let hop = ((HOP_S * sr).round() as usize).max(1);
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let cfft = planner.plan_fft_forward(cn);
        let scratch_len = fft.get_inplace_scratch_len().max(cfft.get_inplace_scratch_len());
        let (win, cwin) = (hann(n), hann(cn));
        let wsum: f32 = win.iter().sum();
        let w2: f32 = win.iter().map(|w| w * w).sum();
        let cwsum: f32 = cwin.iter().sum();

        let bin_hz = sr / n as f64;
        let hi = FLUX_HI_HZ.min(sr / 2.0 * 0.95);
        let mut bands = vec![(0usize, 0usize); BANDS];
        let span = (hi / FLUX_LO_HZ).ln();
        for i in 1..n / 2 {
            let f = i as f64 * bin_hz;
            if f < FLUX_LO_HZ || f > hi {
                continue;
            }
            let b = (((f / FLUX_LO_HZ).ln() / span * BANDS as f64) as usize).min(BANDS - 1);
            if bands[b].1 == 0 {
                bands[b].0 = i;
            }
            bands[b].1 = i + 1;
        }
        let low_bins = ((LOW_HZ / bin_hz) as usize + 1).min(8);

        let cbin_hz = sr / cn as f64;
        let mut chroma_map = Vec::new();
        let mut pitch_map = Vec::new();
        for i in 1..cn / 2 {
            let f = i as f64 * cbin_hz;
            if !(CHROMA_LO_HZ..=CHROMA_HI_HZ).contains(&f) {
                continue;
            }
            let p = 69.0 + 12.0 * (f / 440.0).log2();
            pitch_map.push((i as u32, ((p * 10.0).round() as i64).rem_euclid(PITCH_SLOTS as i64) as u8));
            let d = (p - p.round()).abs();
            let w = (1.0 - 2.0 * d).max(0.0);
            if w > 0.0 {
                chroma_map.push((i as u32, (p.round() as i64).rem_euclid(12) as u8, w as f32));
            }
        }

        // Sized for the whole song, with room for a length that was a little off, so that nothing grows
        // (and reallocates) while it plays. Unknown, it is sized for a long song.
        let expected_ms = if expected_ms == 0 { UNKNOWN_LENGTH_MS } else { expected_ms + expected_ms / 20 + 10_000 };
        let tune_bins = (((TUNING_LO_HZ / cbin_hz).ceil() as usize).max(2), ((CHROMA_HI_HZ / cbin_hz) as usize).min(cn / 2 - 2));
        let frames = (expected_ms as f64 / 1000.0 / HOP_S) as usize + 16;
        let blocks = (expected_ms / 100) as usize + 4;
        Analyzer {
            rate,
            dec,
            sr,
            acc: 0.0,
            acc_n: 0,
            ring: vec![0.0; cn],
            side_ring: vec![0.0; n],
            side_acc: 0.0,
            side_until: 0,
            side_buf: vec![Complex32::default(); n / 2 + 1],
            written: 0,
            hop,
            since_hop: 0,
            hops: 0,
            n,
            cn,
            fft,
            cfft,
            win,
            cwin,
            pow_norm: 2.0 / (n as f32 * w2),
            amp_norm: 2.0 / wsum,
            camp_norm: 2.0 / cwsum,
            buf: vec![Complex32::default(); n],
            cbuf: vec![Complex32::default(); cn],
            scratch: vec![Complex32::default(); scratch_len],
            bands,
            low_bins,
            prev: [0.0; BANDS],
            prev_low: [0.0; 8],
            level: [0.0; BANDS],
            chroma_map,
            pitch_map,
            tune_bins,
            mags: vec![0.0; cn / 2],
            meter: Meter::new(rate, blocks),
            samples: 0,
            voice: vocal::Tracker::new(n, hop, sr, 2.0 / wsum, frames),
            f: Features {
                fps: sr / hop as f64,
                // Frame k is computed once (k + 1) hops have arrived.
                t0: hop as f64 * (1.0 - FRAME_LAG_HOPS) / sr,
                onset: Vec::with_capacity(frames),
                low_onset: Vec::with_capacity(frames),
                power: Vec::with_capacity(frames),
                low_power: Vec::with_capacity(frames),
                vocal: Vec::with_capacity(frames),
                centroid: Vec::with_capacity(frames),
                chroma: Vec::with_capacity(frames / CHROMA_EVERY + 2),
                chroma_t0: 0.0,
                chroma_step: (hop * CHROMA_EVERY) as f64 / sr,
                pitch: vec![0.0; PITCH_SLOTS],
                tuning_cs: (0.0, 0.0),
                blocks_k: Vec::new(),
                blocks_raw: Vec::new(),
                duration_s: 0.0,
                voice: Vec::new(),
            },
        }
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Mono samples in [-1, 1].
    pub fn feed(&mut self, x: &[f32]) {
        self.meter.feed(x);
        for v in x {
            let v = if v.is_finite() { *v } else { 0.0 };
            self.acc += v;
            self.acc_n += 1;
            if self.acc_n == self.dec {
                let s = self.acc / self.dec as f32;
                (self.acc, self.acc_n) = (0.0, 0);
                self.push(s, 0.0);
            }
        }
        self.samples += x.len() as u64;
    }

    /// A stereo source's mid, (L + R) / 2, which everything is measured on, and its side, (L - R) / 2, with
    /// which the vocal curve measures only the middle of the stereo image (vocal.rs `frame_stereo`).
    fn feed_stereo(&mut self, mid: &[f32], side: &[f32]) {
        self.meter.feed(mid);
        for (v, s) in mid.iter().zip(side) {
            let v = if v.is_finite() { *v } else { 0.0 };
            let s = if s.is_finite() { *s } else { 0.0 };
            self.acc += v;
            self.side_acc += s;
            self.acc_n += 1;
            if self.acc_n == self.dec {
                let (v, s) = (self.acc / self.dec as f32, self.side_acc / self.dec as f32);
                (self.acc, self.side_acc, self.acc_n) = (0.0, 0.0, 0);
                self.push(v, s);
            }
        }
        self.samples += mid.len() as u64;
    }

    /// Interleaved frames of `channels` samples, averaged to mono. `load` converts one sample to [-1, 1]. Two
    /// channels also give the vocal curve their side (`feed_stereo`); a stereo source whose channels are the
    /// same is measured exactly as the mono one.
    pub fn feed_interleaved<T: Copy>(&mut self, x: &[T], channels: usize, load: impl Fn(T) -> f32) {
        let channels = channels.max(1);
        let scale = 1.0 / channels as f32;
        let mut mono = [0f32; 256];
        if channels == 2 {
            let mut side = [0f32; 256];
            for chunk in x.chunks(512) {
                let frames = chunk.len() / 2;
                for (i, p) in chunk.as_chunks::<2>().0.iter().enumerate() {
                    let (l, r) = (load(p[0]), load(p[1]));
                    // As the sum below makes it, to the bit.
                    mono[i] = (l + r) * scale;
                    side[i] = (l - r) * scale;
                }
                self.feed_stereo(&mono[..frames], &side[..frames]);
            }
            return;
        }
        for chunk in x.chunks(256 * channels) {
            let frames = chunk.len() / channels;
            for (i, frame) in chunk.chunks_exact(channels).enumerate() {
                mono[i] = frame.iter().map(|s| load(*s)).sum::<f32>() * scale;
            }
            self.feed(&mono[..frames]);
        }
    }

    #[inline]
    fn push(&mut self, s: f32, side: f32) {
        let mask = self.cn - 1;
        self.side_ring[self.written & (self.n - 1)] = side;
        if side != 0.0 {
            self.side_until = self.written + self.n;
        }
        self.ring[self.written & mask] = s;
        self.written += 1;
        self.since_hop += 1;
        if self.since_hop == self.hop {
            self.since_hop = 0;
            self.frame();
        }
    }

    /// Copies the last `len` samples, windowed, into `out`.
    fn window_into(ring: &[f32], written: usize, win: &[f32], out: &mut [Complex32]) {
        let (cn, len) = (ring.len(), win.len());
        let start = written.wrapping_sub(len);
        for (i, (o, w)) in out.iter_mut().zip(win).enumerate() {
            *o = Complex32::new(ring[start.wrapping_add(i) & (cn - 1)] * w, 0.0);
        }
    }

    /// The mid's last `n` samples windowed into the real part of `buf`, the side's into the imaginary part: one
    /// FFT then gives both spectra (`frame`).
    fn window_pair_into(ring: &[f32], side: &[f32], written: usize, win: &[f32], out: &mut [Complex32]) {
        let (cn, n) = (ring.len(), side.len());
        let start = written.wrapping_sub(n);
        for (i, (o, w)) in out.iter_mut().zip(win).enumerate() {
            let at = start.wrapping_add(i);
            *o = Complex32::new(ring[at & (cn - 1)] * w, side[at & (n - 1)] * w);
        }
    }

    fn frame(&mut self) {
        if self.written <= self.side_until {
            // Stereo: the mid and the side through one FFT as the real and imaginary parts of one signal, and
            // parted by the symmetry of a real signal's spectrum: X_mid[k] = (Z[k] + Z*[n-k]) / 2, X_side[k] =
            // (Z[k] - Z*[n-k]) / 2i. Only bins 0..=n/2 of `buf` are read, and bin k's partner n-k is above
            // them, so the mid is written over them in place.
            let n = self.n;
            Self::window_pair_into(&self.ring, &self.side_ring, self.written, &self.win, &mut self.buf);
            self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            for k in 0..=n / 2 {
                let (a, b) = (self.buf[k], self.buf[(n - k) & (n - 1)].conj());
                self.buf[k] = (a + b) * 0.5;
                self.side_buf[k] = (a - b) * Complex32::new(0.0, -0.5);
            }
            self.voice.frame_stereo(&self.buf, &self.side_buf);
        } else {
            Self::window_into(&self.ring, self.written, &self.win, &mut self.buf);
            self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            self.voice.frame(&self.buf);
        }

        let mut total = 0f32;
        let mut low = 0f32;
        let mut vocal = 0f32;
        let mut fsum = 0f64;
        let mut flux_low = 0f32;
        let bin_hz = self.sr / self.n as f64;
        for (i, c) in self.buf[1..self.n / 2].iter().enumerate() {
            let p = c.norm_sqr();
            total += p;
            let bin = i + 1;
            let f = bin as f64 * bin_hz;
            fsum += f * p as f64;
            if f >= VOCAL_LO_HZ && f <= VOCAL_HI_HZ {
                vocal += p;
            }
            if bin < self.low_bins {
                low += p;
                let l = (1.0 + GAMMA * p.sqrt() * self.amp_norm).ln();
                flux_low += (l - self.prev_low[bin]).max(0.0);
                self.prev_low[bin] = l;
            }
        }
        let mut flux = 0f32;
        for (b, &(lo, hi)) in self.bands.iter().enumerate() {
            if hi <= lo {
                continue;
            }
            let p: f32 = self.buf[lo..hi].iter().map(|c| c.norm_sqr()).sum();
            let l = (1.0 + GAMMA * (p / (hi - lo) as f32).sqrt() * self.amp_norm).ln();
            flux += (l - self.prev[b]).max(0.0);
            self.level[b] = l;
        }
        self.prev = self.level;
        self.f.onset.push(flux);
        self.f.low_onset.push(flux_low);
        self.f.power.push(total * self.pow_norm);
        self.f.low_power.push(low * self.pow_norm);
        self.f.vocal.push(vocal / (total + 1e-12));
        self.f.centroid.push((fsum / (total as f64 + 1e-9)) as f32);

        if self.hops % CHROMA_EVERY == 0 {
            if self.hops == 0 {
                self.f.chroma_t0 = (self.written as f64 - self.cn as f64 / 2.0) / self.sr;
            }
            Self::window_into(&self.ring, self.written, &self.cwin, &mut self.cbuf);
            self.cfft.process_with_scratch(&mut self.cbuf, &mut self.scratch);
            let (lo, hi) = (self.pitch_map.first().map_or(0, |m| m.0 as usize), self.pitch_map.last().map_or(0, |m| m.0 as usize));
            for i in lo.min(self.tune_bins.0 - 1)..=hi.max(self.tune_bins.1 + 1).min(self.mags.len() - 1) {
                self.mags[i] = self.cbuf[i].norm() * self.camp_norm;
            }
            let mut c = [0f32; 12];
            for &(bin, pc, w) in &self.chroma_map {
                c[pc as usize] += w * self.mags[bin as usize];
            }
            self.f.chroma.push(c);
            for &(bin, slot) in &self.pitch_map {
                self.f.pitch[slot as usize] += self.mags[bin as usize] as f64;
            }
            self.tuning_peaks();
        }
        self.hops += 1;
    }

    /// Adds this chroma frame's spectral peaks to the tuning estimate: each clear peak, its frequency refined
    /// by a parabola through the log magnitudes, votes for how far above or below the equal-tempered grid it
    /// sits, weighted by its magnitude.
    fn tuning_peaks(&mut self) {
        let (lo, hi) = self.tune_bins;
        let m = &self.mags;
        let floor = 0.05 * m[lo..=hi].iter().fold(0f32, |a, v| a.max(*v));
        if floor <= 1e-7 {
            return;
        }
        let cbin_hz = self.sr / self.cn as f64;
        let (mut cs, mut sn) = (0.0, 0.0);
        for i in lo..=hi {
            if m[i] > floor && m[i] > m[i - 1] && m[i] >= m[i + 1] {
                let (a, b, c) = ((m[i - 1] as f64).max(1e-12).ln(), (m[i] as f64).ln(), (m[i + 1] as f64).max(1e-12).ln());
                let d = a - 2.0 * b + c;
                let delta = if d.abs() < 1e-12 { 0.0 } else { (0.5 * (a - c) / d).clamp(-0.5, 0.5) };
                let semis = 12.0 * ((i as f64 + delta) * cbin_hz / 440.0).log2();
                let frac = semis - semis.round();
                let w = m[i] as f64;
                cs += w * (2.0 * std::f64::consts::PI * frac).cos();
                sn += w * (2.0 * std::f64::consts::PI * frac).sin();
            }
        }
        self.f.tuning_cs.0 += cs;
        self.f.tuning_cs.1 += sn;
    }

    /// Everything measured so far. The analyser is left empty and can be fed again from the start.
    pub fn take_features(&mut self) -> Features {
        self.meter.finish();
        let (fps, t0, chroma_step) = (self.f.fps, self.f.t0, self.f.chroma_step);
        let mut f = std::mem::replace(
            &mut self.f,
            Features {
                fps,
                t0,
                onset: Vec::new(),
                low_onset: Vec::new(),
                power: Vec::new(),
                low_power: Vec::new(),
                vocal: Vec::new(),
                centroid: Vec::new(),
                chroma: Vec::new(),
                chroma_t0: 0.0,
                chroma_step,
                pitch: vec![0.0; PITCH_SLOTS],
                tuning_cs: (0.0, 0.0),
                blocks_k: Vec::new(),
                blocks_raw: Vec::new(),
                duration_s: 0.0,
                voice: Vec::new(),
            },
        );
        f.voice = self.voice.take();
        f.blocks_k = std::mem::take(&mut self.meter.blocks_k);
        f.blocks_raw = std::mem::take(&mut self.meter.blocks_raw);
        f.duration_s = self.samples as f64 / self.rate;
        self.reset();
        f
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.side_ring.fill(0.0);
        (self.side_acc, self.side_until) = (0.0, 0);
        (self.written, self.since_hop, self.hops, self.samples, self.acc, self.acc_n) = (0, 0, 0, 0, 0.0, 0);
        (self.prev, self.prev_low) = ([0.0; BANDS], [0.0; 8]);
        self.meter.reset();
        self.voice.reset();
        for v in [&mut self.f.onset, &mut self.f.low_onset, &mut self.f.power, &mut self.f.low_power, &mut self.f.vocal, &mut self.f.centroid] {
            v.clear();
        }
        self.f.chroma.clear();
        self.f.pitch.iter_mut().for_each(|v| *v = 0.0);
        self.f.tuning_cs = (0.0, 0.0);
    }
}
