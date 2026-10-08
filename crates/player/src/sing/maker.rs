//! A song's vocal mask made as its samples come ([`MaskMaker`]): Open-Unmix's magnitudes (4096-point Hann frames
//! every 1024 samples at 44.1 kHz; at other rates both scale, so the bins stay 10.77 Hz apart), read by the model
//! ([`Separator`]) in runs of the frames that came since the last, each with up to [`HISTORY`] frames before them
//! for context and none after.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use super::{band_edges, VocalMask, MODEL_BINS, MODEL_FFT, MODEL_HOP, MODEL_RATE};

/// Bins the model reads (to 16 kHz).
pub const IN_BINS: usize = 1487;
/// Frames of context a run reads before its new frames.
pub const HISTORY: usize = 64;
/// Most new frames a run answers for (13 s), so a run holds tens of megabytes at most.
const MOST: usize = 576;

/// What tells the vocals from the rest.
pub trait Separator {
    /// For `frames` frames of magnitudes laid out [frame][channel][bin] ([`IN_BINS`] bins), each frame's vocal share
    /// per channel and model bin, handed to `row` as [channel][bin] ([`MODEL_BINS`] bins).
    fn separate(&self, mags: Vec<f32>, frames: usize, row: &mut dyn FnMut(usize, &[f32])) -> Result<(), String>;
}

/// A song's mask from its samples as they come, from any song frame on. Frame `k` is centred on song sample
/// `k * hop`; samples before the first fed are taken as silence.
pub struct MaskMaker {
    n: usize,
    hop: usize,
    /// Magnitudes are scaled to what a 4096-point frame would give.
    scale: f32,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// Stereo samples (interleaved) from the start of frame `next`'s window.
    samples: Vec<f32>,
    /// The frame transformed next.
    next: usize,
    /// Magnitudes, [frame][channel][bin], of frames `kept..next`.
    mags: Vec<f32>,
    kept: usize,
    /// Frames before this one have been answered for.
    answered: usize,
    edges: Vec<usize>,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
}

impl MaskMaker {
    /// Mask frames a second at `rate`.
    pub fn fps(rate: u32) -> f32 {
        (rate as f64 / hop(rate) as f64) as f32
    }

    /// For samples at `rate` from song frame `from` on.
    pub fn new(rate: u32, from: u64) -> MaskMaker {
        let n = (MODEL_FFT as f64 * rate as f64 / MODEL_RATE).round() as usize;
        let hop = hop(rate);
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
        // Periodic Hann, as torch.hann_window.
        let window = (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect();
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        let first = from.div_ceil(hop as u64) as usize;
        // Silence from the first window's start to the first sample.
        let silent = (from as i64 - (first * hop) as i64 + (n / 2) as i64).max(0) as usize;
        MaskMaker {
            n,
            hop,
            scale: MODEL_FFT as f32 / n as f32,
            fft,
            window,
            samples: vec![0.0; 2 * silent],
            next: first,
            mags: Vec::new(),
            kept: first,
            answered: first,
            edges: band_edges(),
            buf: vec![Complex32::default(); n],
            scratch,
        }
    }

    /// Stereo samples (interleaved), in order.
    pub fn feed(&mut self, x: &[f32]) {
        self.samples.extend_from_slice(x);
        self.transform();
    }

    /// The song ends after what was fed: its last frames are transformed with silence after it. Returns the song's
    /// frames.
    pub fn end(&mut self) -> usize {
        self.samples.resize(self.samples.len() + self.n / 2 * 2, 0.0);
        self.transform();
        self.next
    }

    /// Frames transformed and not answered for yet.
    pub fn waiting(&self) -> usize {
        self.next - self.answered
    }

    /// Enough frames wait for a run to read more new frames than history.
    pub fn due(&self) -> bool {
        self.waiting() >= HISTORY
    }

    /// Every whole window in `samples`, a hop apart.
    fn transform(&mut self) {
        let mut at = 0;
        while self.samples.len() - at >= 2 * self.n {
            self.magnitudes(at);
            at += 2 * self.hop;
        }
        self.samples.drain(..at);
    }

    /// The magnitudes of the window from interleaved sample `at`; none above the song's Nyquist frequency.
    fn magnitudes(&mut self, at: usize) {
        let bins = IN_BINS.min(self.n / 2 + 1);
        for c in 0..2 {
            for (i, (b, w)) in self.buf.iter_mut().zip(&self.window).enumerate() {
                *b = Complex32::new(self.samples[at + 2 * i + c] * w, 0.0);
            }
            self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            self.mags.extend(self.buf[..bins].iter().map(|b| b.norm() * self.scale));
            self.mags.resize(self.mags.len() + IN_BINS - bins, 0.0);
        }
        self.next += 1;
    }

    /// Runs the model over the frames waiting (up to [`MOST`]) after their history and puts the rows `mask` lacks;
    /// none if it has them all.
    pub fn answer(&mut self, model: &dyn Separator, mask: &VocalMask) -> Result<(), String> {
        let new = self.waiting().min(MOST);
        if new == 0 {
            return Ok(());
        }
        let per = 2 * IN_BINS;
        let from = self.answered - (self.answered - self.kept).min(HISTORY);
        if (self.answered..self.answered + new).any(|k| !mask.has(k)) {
            let frames = self.answered + new - from;
            let mags = self.mags[(from - self.kept) * per..(from - self.kept + frames) * per].to_vec();
            let (answered, edges) = (self.answered, &self.edges);
            let mut row = vec![0u8; edges.len() - 1];
            model.separate(mags, frames, &mut |k, shares| {
                let frame = from + k;
                if frame < answered || mask.has(frame) {
                    return;
                }
                for (r, w) in row.iter_mut().zip(edges.windows(2)) {
                    let sum: f32 = (w[0]..w[1]).map(|b| shares[b] + shares[MODEL_BINS + b]).sum();
                    *r = ((sum / (2 * (w[1] - w[0])) as f32).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                mask.put(frame, &row);
            })?;
        }
        self.answered += new;
        // Keep only what the next run reads as history.
        let keep_from = self.answered.saturating_sub(HISTORY).max(self.kept);
        self.mags.drain(..(keep_from - self.kept) * per);
        self.kept = keep_from;
        Ok(())
    }
}

/// Samples between frames at `rate`.
fn hop(rate: u32) -> usize {
    (MODEL_HOP as f64 * rate as f64 / MODEL_RATE).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bin all vocals; counts its runs and the frames it read.
    #[derive(Default)]
    struct AllVocals(std::cell::Cell<(usize, usize)>);

    impl Separator for AllVocals {
        fn separate(&self, mags: Vec<f32>, frames: usize, row: &mut dyn FnMut(usize, &[f32])) -> Result<(), String> {
            assert_eq!(mags.len(), frames * 2 * IN_BINS);
            let (runs, read) = self.0.get();
            self.0.set((runs + 1, read + frames));
            let ones = vec![1.0; 2 * MODEL_BINS];
            (0..frames).for_each(|k| row(k, &ones));
            Ok(())
        }
    }

    const RATE: u32 = 44_100;

    /// Fed a second at a time from a seek point, each run answers for the frames that came, after their history;
    /// the song's end gives its last rows and its length.
    #[test]
    fn rows_come_as_samples_do() {
        let model = AllVocals::default();
        let from = 10 * RATE as u64;
        let mask = VocalMask::growing(MaskMaker::fps(RATE), 30 * 44);
        let mut m = MaskMaker::new(RATE, from);
        let first = (from as usize).div_ceil(MODEL_HOP);
        let second = vec![0.1f32; 2 * RATE as usize];
        m.feed(&second);
        m.answer(&model, &mask).unwrap();
        // A frame's window reaches half a window past its centre.
        let rows = (from as usize + RATE as usize - MODEL_FFT / 2) / MODEL_HOP + 1 - first;
        assert!(!mask.has(first - 1) && mask.has(first) && mask.has(first + rows - 1) && !mask.has(first + rows));
        m.feed(&second);
        m.answer(&model, &mask).unwrap();
        assert_eq!(model.0.get(), (2, rows + (rows + 43)), "the second run reads the first's frames before its own");
        let frames = m.end();
        m.answer(&model, &mask).unwrap();
        assert_eq!(frames, (from as usize + 2 * RATE as usize) / MODEL_HOP + 1);
        assert!((first..frames).all(|k| mask.has(k)));
        // Fed again over rows it has, it reads nothing.
        let mut again = MaskMaker::new(RATE, from);
        again.feed(&second);
        again.answer(&model, &mask).unwrap();
        assert_eq!(model.0.get().0, 3);
    }
}
