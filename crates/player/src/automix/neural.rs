//! Beat This! (Foscarin, Schlüter and Widmer, ISMIR 2024; MIT) through tract, behind `neural-beats`: the model's
//! log-mel input as trained (torchaudio settings), one pass over a 30 s window padded by its border, the paper's
//! minimal peak picking, and a grid fitted to the result.
//!
//! The app's graph expresses attention as ONNX `Attention` nodes (tools/beat-this/export.py), which tract runs as
//! flash attention: about 100 MB held instead of 700 MB for MatMul/Softmax/MatMul. It runs on the calling thread
//! (`TRACT_FLASH_SDPA_ST`). Costs: docs/research/analysis.md, tests/neural_memory.rs.

use std::path::Path;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use tract_onnx::prelude::*;

use super::tempo;

pub const MELS: usize = 128;
pub const FPS: f64 = 50.0;
/// Frames read at once: 30 s, the model's training length.
pub const WINDOW: usize = 1500;
/// Frames at each edge of a chunk the model was not trained to answer for.
pub const BORDER: usize = 6;
/// Frames per model run: a window plus a border of silence either side.
pub const CHUNK: usize = WINDOW + 2 * BORDER;
const MODEL_RATE: f64 = 22050.0;
const F_MIN: f64 = 30.0;
const F_MAX: f64 = 11000.0;

fn hz_to_mel(f: f64) -> f64 {
    // Slaney mel.
    let f_sp = 200.0 / 3.0;
    if f >= 1000.0 {
        1000.0 / f_sp + (f / 1000.0).ln() / (6.4f64.ln() / 27.0)
    } else {
        f / f_sp
    }
}

fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_mel = 1000.0 / f_sp;
    if m >= min_log_mel {
        1000.0 * ((6.4f64.ln() / 27.0) * (m - min_log_mel)).exp()
    } else {
        f_sp * m
    }
}

/// torchaudio's `MelSpectrogram` with Beat This!'s settings; FFT length and hop scale with the rate (near 22 kHz)
/// so frames stay 46 ms long, 50 a second.
pub struct LogMel {
    n_fft: usize,
    hop: usize,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// Per mel band: first FFT bin and the triangle's weights from there.
    filters: Vec<(usize, Vec<f32>)>,
    norm: f32,
}

impl LogMel {
    pub fn new(rate: f64) -> Self {
        let n_fft = (1024.0 * rate / MODEL_RATE).round() as usize;
        let hop = (rate / FPS).round() as usize;
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n_fft);
        // Periodic Hann, as torch.hann_window.
        let window = (0..n_fft).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n_fft as f64).cos()) as f32).collect();
        let bins = n_fft / 2 + 1;
        let bin_hz = |i: usize| i as f64 * (rate / 2.0) / (bins - 1) as f64;
        let (m0, m1) = (hz_to_mel(F_MIN), hz_to_mel(F_MAX));
        let pts: Vec<f64> = (0..MELS + 2).map(|i| mel_to_hz(m0 + (m1 - m0) * i as f64 / (MELS + 1) as f64)).collect();
        let filters = (0..MELS)
            .map(|m| {
                let (lo, mid, hi) = (pts[m], pts[m + 1], pts[m + 2]);
                let weights: Vec<(usize, f32)> = (0..bins)
                    .map(|i| {
                        let f = bin_hz(i);
                        (i, ((f - lo) / (mid - lo)).min((hi - f) / (hi - mid)).max(0.0) as f32)
                    })
                    .filter(|(_, w)| *w > 0.0)
                    .collect();
                let first = weights.first().map_or(0, |w| w.0);
                (first, weights.iter().map(|w| w.1).collect())
            })
            .collect();
        LogMel { n_fft, hop, fft, window, filters, norm: 1.0 / (n_fft as f32).sqrt() }
    }

    /// Frames of mono `x`, centred as torch.stft(center=True): frame `k` at sample `k * hop`, ends mirrored.
    pub fn frames(&self, x: &[f32]) -> Vec<[f32; MELS]> {
        let pad = self.n_fft / 2;
        if x.len() <= pad + 1 {
            return Vec::new();
        }
        let at = |i: isize| -> f32 {
            let n = x.len() as isize;
            let j = if i < 0 { -i } else if i >= n { 2 * (n - 1) - i } else { i };
            x[j.clamp(0, n - 1) as usize]
        };
        let count = x.len() / self.hop + 1;
        let mut buf = vec![Complex32::default(); self.n_fft];
        let mut scratch = vec![Complex32::default(); self.fft.get_inplace_scratch_len()];
        let mut mag = vec![0f32; self.n_fft / 2 + 1];
        let mut out = Vec::with_capacity(count);
        for k in 0..count {
            let start = (k * self.hop) as isize - pad as isize;
            for (i, (b, w)) in buf.iter_mut().zip(&self.window).enumerate() {
                *b = Complex32::new(at(start + i as isize) * w, 0.0);
            }
            self.fft.process_with_scratch(&mut buf, &mut scratch);
            for (m, b) in mag.iter_mut().zip(&buf) {
                *m = b.norm() * self.norm;
            }
            let mut row = [0f32; MELS];
            for (r, (first, w)) in row.iter_mut().zip(&self.filters) {
                let e: f32 = w.iter().zip(&mag[*first..]).map(|(a, b)| a * b).sum();
                *r = (1.0 + 1000.0 * e).ln();
            }
            out.push(row);
        }
        out
    }
}

/// Mono samples decimated by averaging to about 22 kHz; returns the samples and their rate.
pub fn decimate(x: &[f32], rate: u32) -> (Vec<f32>, f64) {
    let k = ((rate as f64 / MODEL_RATE).round() as usize).max(1);
    (x.chunks_exact(k).map(|c| c.iter().sum::<f32>() / k as f32).collect(), rate as f64 / k as f64)
}

/// The model, loaded and optimised for chunks of `chunk` frames.
pub struct BeatThis {
    model: Arc<TypedRunnableModel>,
    chunk: usize,
}

/// Chunk starts over `n` frames, as beat_this's `split_piece`: overlapping by twice the border, from a border
/// before the stretch to a border after it.
fn chunk_starts(n: usize, chunk: usize) -> Vec<isize> {
    let (n, c, b) = (n as isize, chunk as isize, BORDER as isize);
    let mut starts: Vec<isize> = (-b..(n - b).max(-b + 1)).step_by((c - 2 * b) as usize).collect();
    if n > c - 2 * b {
        *starts.last_mut().expect("at least one") = n - (c - b);
    }
    starts
}

/// Beats and downbeats of one window, seconds from the window's first frame.
#[derive(Debug, Default, Clone)]
pub struct Tracked {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
}

/// Frames with a positive logit that are the maximum of the 7 around them; adjacent ties merge at their mean.
fn peaks(logits: &[f32]) -> Vec<f64> {
    let n = logits.len();
    let mut frames: Vec<usize> = (0..n)
        .filter(|&i| {
            let (a, b) = (i.saturating_sub(3), (i + 4).min(n));
            logits[i] > 0.0 && logits[a..b].iter().all(|v| *v <= logits[i])
        })
        .collect();
    let mut out = Vec::new();
    while !frames.is_empty() {
        let mut run = 1;
        while run < frames.len() && frames[run] - frames[run - 1] <= 1 {
            run += 1;
        }
        out.push(frames[..run].iter().sum::<usize>() as f64 / run as f64 / FPS);
        frames.drain(..run);
    }
    out
}

impl BeatThis {
    pub fn load(path: &Path) -> TractResult<Self> {
        Self::load_chunked(path, CHUNK)
    }

    /// With chunks of `chunk` frames (more than twice the border), for evaluation.
    pub fn load_chunked(path: &Path, chunk: usize) -> TractResult<Self> {
        Self::prepare(tract_onnx::onnx().model_for_path(path)?, chunk)
    }

    /// From an ONNX file with its weights.
    pub fn from_bytes(bytes: &[u8]) -> TractResult<Self> {
        Self::prepare(tract_onnx::onnx().model_for_read(&mut std::io::Cursor::new(bytes))?, CHUNK)
    }

    /// The app's graph filled with the weights file (`weights::convert`).
    pub fn from_weights(weights: &[u8]) -> TractResult<Self> {
        let proto = super::weights::assemble(weights).map_err(|e| tract_onnx::prelude::TractError::msg(e))?;
        Self::prepare(tract_onnx::onnx().model_for_proto_model(&proto)?, CHUNK)
    }

    fn prepare(model: InferenceModel, chunk: usize) -> TractResult<Self> {
        // Flash attention on this thread, not rayon's pool, so the caller's low priority holds.
        let _ = tract_onnx::prelude::tract_data::knobs::set_str("TRACT_FLASH_SDPA_ST", "true");
        let model = model
            .with_input_fact(0, f32::fact([1, chunk, MELS]).into())?
            .into_optimized()?
            .into_runnable()?;
        Ok(BeatThis { model, chunk })
    }

    /// Beat and downbeat logits per frame of `mel`, from the first chunk answering for each; outside is silence.
    pub fn logits(&self, mel: &[[f32; MELS]]) -> TractResult<(Vec<f32>, Vec<f32>)> {
        let (n, c) = (mel.len(), self.chunk);
        let mut beat = vec![f32::NAN; n];
        let mut down = vec![f32::NAN; n];
        for s in chunk_starts(n, c) {
            let mut input = vec![0f32; c * MELS];
            for (k, row) in input.chunks_exact_mut(MELS).enumerate() {
                let t = s + k as isize;
                if t >= 0 && (t as usize) < n {
                    row.copy_from_slice(&mel[t as usize]);
                }
            }
            let input: Tensor = tract_ndarray::Array3::from_shape_vec((1, c, MELS), input)?.into();
            let out = self.model.run(tvec!(input.into()))?;
            let (b, d) = (out[0].to_plain_array_view::<f32>()?, out[1].to_plain_array_view::<f32>()?);
            for (k, (bv, dv)) in b.iter().zip(d.iter()).enumerate().take(c - BORDER).skip(BORDER) {
                let t = s + k as isize;
                if t >= 0 && (t as usize) < n && beat[t as usize].is_nan() {
                    beat[t as usize] = *bv;
                    down[t as usize] = *dv;
                }
            }
        }
        for v in beat.iter_mut().chain(down.iter_mut()) {
            if v.is_nan() {
                *v = -1000.0;
            }
        }
        Ok((beat, down))
    }

    /// Beats and downbeats of one window; border frames are dropped unless at the song's edge. Downbeats snap to
    /// the nearest beat.
    pub fn track(&self, mel: &[[f32; MELS]], song_start: bool, song_end: bool) -> TractResult<Tracked> {
        let (mut beat, mut down) = self.logits(mel)?;
        let n = beat.len();
        for i in 0..n {
            if (!song_start && i < BORDER) || (!song_end && i + BORDER >= n) {
                beat[i] = -1000.0;
                down[i] = -1000.0;
            }
        }
        let beats = peaks(&beat);
        let mut downbeats: Vec<f64> = peaks(&down)
            .iter()
            .filter_map(|d| beats.iter().copied().min_by(|a, b| (a - d).abs().total_cmp(&(b - d).abs())))
            .collect();
        downbeats.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        Ok(Tracked { beats, downbeats })
    }

    /// Beats and downbeats of one end of a song (`x` mono at `rate`), seconds from its first sample: the first
    /// `WINDOW` frames of an intro or the last of an outro; the cut edge counts as the song's edge.
    pub fn track_window(&self, x: &[f32], rate: u32, intro: bool) -> TractResult<Tracked> {
        let (y, sr) = decimate(x, rate);
        let mel = LogMel::new(sr).frames(&y);
        let n = mel.len();
        let from = if intro { 0 } else { n.saturating_sub(WINDOW) };
        let whole = n <= WINDOW;
        let mut t = self.track(&mel[from..(from + WINDOW).min(n)], intro || whole, !intro || whole)?;
        let shift = from as f64 / FPS;
        t.beats.iter_mut().chain(t.downbeats.iter_mut()).for_each(|v| *v += shift);
        Ok(t)
    }
}

/// A constant grid through tracked beats, as AutoMix stores one.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeuralGrid {
    pub bpm: f64,
    pub offset_s: f64,
    pub stability: f32,
    pub confidence: f32,
    pub downbeat_phase: i32,
    pub beats_per_bar: i32,
    /// A second bar start the downbeats point at almost as often (the model marks every other beat where only
    /// drums play).
    pub other_phase: Option<i32>,
    /// A beat mid-window, seconds, where two grids are compared.
    pub anchor_s: f64,
}

/// The grid through beats and downbeats: metre from the usual downbeat gap (3 or 4), phase by vote, `other_phase`
/// when the runner-up has over half the votes. `None` for fewer than 8 beats.
pub fn grid(tracked: &Tracked) -> Option<NeuralGrid> {
    let beats = &tracked.beats;
    if beats.len() < 8 {
        return None;
    }
    let mut ibi: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    ibi.sort_unstable_by(|a, b| a.total_cmp(b));
    let median = ibi[ibi.len() / 2];
    let (a, period, rms) = tempo::fit_grid(beats, median);
    if !(period > 0.0) {
        return None;
    }
    let offset = a.rem_euclid(period);
    let index = |t: f64| ((t - offset) / period).round() as i64;
    let mut gaps: Vec<i64> = tracked.downbeats.windows(2).map(|w| index(w[1]) - index(w[0])).filter(|g| *g > 0).collect();
    gaps.sort_unstable();
    let meter = gaps.get(gaps.len() / 2).copied().filter(|g| *g == 3).unwrap_or(4);
    let mut votes = [0usize; 4];
    for d in &tracked.downbeats {
        votes[index(*d).rem_euclid(meter) as usize] += 1;
    }
    // Most votes first; a tie goes to the earlier phase.
    let mut order: Vec<usize> = (0..meter as usize).collect();
    order.sort_by_key(|p| std::cmp::Reverse(votes[*p]));
    let phase = order[0];
    let other = order.get(1).copied().filter(|p| votes[*p] > 0 && 2 * votes[*p] > votes[phase]);
    // Share of beat gaps within 15 % of the median.
    let regular = ibi.iter().filter(|g| (*g / median - 1.0).abs() < 0.15).count() as f64 / ibi.len() as f64;
    Some(NeuralGrid {
        bpm: 60.0 / period,
        offset_s: offset,
        stability: tempo::stability(beats, period, rms),
        confidence: ((regular - 0.5) / 0.4).clamp(0.0, 1.0) as f32,
        downbeat_phase: phase as i32,
        beats_per_bar: meter as i32,
        other_phase: other.map(|p| p as i32),
        anchor_s: beats[beats.len() / 2],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 kHz tone lights the mel band around 1 kHz and no band far from it; silence is log(1) = 0.
    #[test]
    fn log_mel_puts_tone_in_its_band() {
        let rate = 22050.0;
        let tone: Vec<f32> = (0..22050).map(|i| (0.5 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate).sin()) as f32).collect();
        let mel = LogMel::new(rate);
        let frames = mel.frames(&tone);
        assert_eq!(frames.len(), 22050 / 441 + 1);
        let mid = frames[frames.len() / 2];
        let top = (0..MELS).max_by(|a, b| mid[*a].total_cmp(&mid[*b])).unwrap();
        let centre = |m: usize| {
            let (m0, m1) = (hz_to_mel(F_MIN), hz_to_mel(F_MAX));
            mel_to_hz(m0 + (m1 - m0) * (m + 1) as f64 / (MELS + 1) as f64)
        };
        assert!((centre(top) / 1000.0 - 1.0).abs() < 0.05, "loudest band centred at {} Hz", centre(top));
        assert!(mid[10] < 0.1 * mid[top], "a band far below stays quiet");
        assert!(mel.frames(&vec![0.0; 22050]).iter().all(|f| f.iter().all(|v| *v == 0.0)));
        assert_eq!(LogMel::new(24000.0).frames(&vec![0.0; 24000]).len(), 51);
    }

    /// Cost of keeping a song's ends and the front end over both windows.
    /// `cargo test --release -p nori-player --features neural-beats neural_front_end_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn neural_front_end_cost() {
        use super::super::beats::Ends;
        let x: Vec<f32> = (0..44_100 * 240 * 2).map(|i| ((i as f32) * 0.001).sin() * 0.3).collect();
        let t0 = std::time::Instant::now();
        let mut ends = Ends::new(44_100);
        for piece in x.chunks(4096) {
            ends.feed(piece, 2);
        }
        let kept = t0.elapsed();
        let t1 = std::time::Instant::now();
        let mel = LogMel::new(ends.rate() as f64);
        let head = mel.frames(&ends.head()[..30 * ends.rate() as usize]).len();
        let (tail, _) = ends.tail();
        let tail = mel.frames(&tail[tail.len() - 30 * 22_050..]).len();
        println!("ends kept in {:.1} ms, front end over both windows ({} frames) in {:.1} ms", kept.as_secs_f64() * 1e3, head + tail, t1.elapsed().as_secs_f64() * 1e3);
    }

    /// Time and peak RSS of loading the model and running a song's two windows.
    /// `NORI_BEAT_THIS=<model.onnx> NORI_SONG=<x.s16> cargo test --release -p nori-player --features neural-beats
    /// neural_song_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn neural_song_cost() {
        let (Ok(model), Ok(song)) = (std::env::var("NORI_BEAT_THIS"), std::env::var("NORI_SONG")) else { return };
        let rate: u32 = std::env::var("NORI_SONG_RATE").ok().and_then(|r| r.parse().ok()).unwrap_or(44_100);
        let peak = || {
            let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            s.lines().find_map(|l| l.strip_prefix("VmHWM:").map(|v| v.trim().to_string())).unwrap_or_default()
        };
        let bytes = std::fs::read(song).unwrap();
        let x: Vec<f32> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        let before = peak();
        let t0 = std::time::Instant::now();
        let m = BeatThis::from_bytes(&std::fs::read(model).unwrap()).unwrap();
        let loaded = t0.elapsed().as_secs_f64();
        let n = (30 * rate as usize).min(x.len());
        let mut windows = Vec::new();
        for (piece, intro) in [(&x[..n], true), (&x[x.len() - n..], false)] {
            let t = std::time::Instant::now();
            let got = m.track_window(piece, rate, intro).unwrap();
            windows.push(t.elapsed().as_secs_f64());
            assert!(!got.beats.is_empty());
        }
        println!(
            "loaded in {loaded:.2} s; windows {:.2} s and {:.2} s; the song {:.2} s; peak RSS {before} before the model, {} after",
            windows[0],
            windows[1],
            loaded + windows[0] + windows[1],
            peak()
        );
    }

    /// Dumps the log-mel of a raw mono 16-bit 22.05 kHz file, to compare with torchaudio:
    /// `NORI_MEL_IN=x.s16 NORI_MEL_OUT=x.mel cargo test --features neural-beats mel_dump -- --ignored`
    #[test]
    #[ignore]
    fn mel_dump() {
        let (Ok(input), Ok(output)) = (std::env::var("NORI_MEL_IN"), std::env::var("NORI_MEL_OUT")) else { return };
        let bytes = std::fs::read(input).unwrap();
        let x: Vec<f32> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        let frames = LogMel::new(MODEL_RATE).frames(&x);
        std::fs::write(output, frames.iter().flatten().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    }

    /// As beat_this's split_piece; every frame of any stretch is answered for.
    #[test]
    fn chunks_cover_window() {
        assert_eq!(chunk_starts(WINDOW, CHUNK), vec![-6]);
        assert_eq!(chunk_starts(1500, 512), vec![-6, 494, 994]);
        assert_eq!(chunk_starts(1500, 1500), vec![-6, 6]);
        for c in [512, CHUNK] {
            for n in [1, 40, 499, 500, 501, 1234, 1500] {
                let starts = chunk_starts(n, c);
                for t in 0..n as isize {
                    assert!(starts.iter().any(|s| t >= s + BORDER as isize && t < s + (c - BORDER) as isize), "frame {t} of {n}");
                }
            }
        }
    }

    #[test]
    fn peaks_are_positive_local_maxima() {
        let mut l = vec![-5.0f32; 200];
        for (i, v) in [(20, 3.0), (21, 3.0), (60, 1.0), (62, 2.0), (100, -0.5)] {
            l[i] = v;
        }
        let p = peaks(&l);
        assert_eq!(p, vec![20.5 / FPS, 62.0 / FPS]);
    }

    #[test]
    fn grid_from_beats_and_downbeats() {
        let beats: Vec<f64> = (0..40).map(|i| 0.3 + i as f64 * 0.5).collect();
        let t = Tracked { downbeats: beats.iter().skip(2).step_by(3).copied().collect(), beats };
        let g = grid(&t).unwrap();
        assert!((g.bpm - 120.0).abs() < 1e-6 && (g.offset_s - 0.3).abs() < 1e-6);
        assert_eq!((g.beats_per_bar, g.downbeat_phase, g.other_phase), (3, 2, None));
        assert!(g.stability > 0.9 && g.confidence > 0.9);
    }

    #[test]
    fn every_other_beat_marked_gives_two_candidate_bars() {
        let beats: Vec<f64> = (0..40).map(|i| 0.3 + i as f64 * 0.5).collect();
        let t = Tracked { downbeats: beats.iter().skip(1).step_by(2).copied().collect(), beats };
        let g = grid(&t).unwrap();
        assert_eq!(g.beats_per_bar, 4);
        let mut both = [g.downbeat_phase, g.other_phase.unwrap()];
        both.sort();
        assert_eq!(both, [1, 3]);
    }
}
