//! Open-Unmix UMX-HQ's vocals model (MIT; tools/umx/export.py) through tract: a song's [`VocalMask`] made from its
//! samples as they are decoded. The magnitudes are Open-Unmix's (4096-point Hann frames every 1024 samples at
//! 44.1 kHz; at other rates both scale, so the bins stay 10.77 Hz apart); the model reads them in windows of
//! [`WINDOW`] frames with [`BORDER`] frames of context either side, so it holds tens of megabytes whatever the song's
//! length.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use tract_onnx::prelude::*;

use super::{band_edges, VocalMask, MODEL_FFT, MODEL_HOP, MODEL_RATE};

/// The vocals model as ONNX, its weights left out (`crate::automix::weights`).
pub static GRAPH: &[u8] = include_bytes!("../../models/umx-hq-vocals.graph.onnx");

/// Bins the model reads (to 16 kHz) and answers for.
const IN_BINS: usize = 1487;
const OUT_BINS: usize = super::MODEL_BINS;
/// Frames answered for per run (12 s), and the context read either side of them.
pub const WINDOW: usize = 512;
pub const BORDER: usize = 64;
const CHUNK: usize = WINDOW + 2 * BORDER;

/// The model, loaded and optimised for chunks of [`CHUNK`] frames.
pub struct Unmix {
    model: Arc<TypedRunnableModel>,
}

impl Unmix {
    /// The app's graph filled with the weights file (`weights::convert`).
    pub fn from_weights(weights: &[u8]) -> TractResult<Self> {
        let proto = crate::automix::weights::assemble(GRAPH, weights).map_err(TractError::msg)?;
        let model = tract_onnx::onnx()
            .model_for_proto_model(&proto)?
            .with_input_fact(0, f32::fact([CHUNK, 2, IN_BINS]).into())?
            .into_optimized()?
            .into_runnable()?;
        Ok(Unmix { model })
    }

    /// The model's mask, [frame][channel][bin], for [`CHUNK`] frames of magnitudes laid out [frame][channel][bin].
    fn run(&self, mags: Vec<f32>) -> TractResult<Tensor> {
        let input: Tensor = tract_ndarray::Array3::from_shape_vec((CHUNK, 2, IN_BINS), mags)?.into();
        Ok(self.model.run(tvec!(input.into()))?.remove(0).into_tensor())
    }
}

/// A song's mask, made as its samples come: the magnitude frames are kept until a window and its context are in.
pub struct MaskMaker<'a> {
    unmix: &'a Unmix,
    n: usize,
    hop: usize,
    /// Magnitudes are scaled to what a 4096-point frame would give.
    scale: f32,
    fps: f32,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// Stereo samples (interleaved) from the next frame's start.
    samples: Vec<f32>,
    /// Magnitudes, [frame][channel][bin], from frame `kept_from`.
    mags: Vec<f32>,
    kept_from: usize,
    frames: usize,
    /// Mask frames answered for so far, a row of bands each.
    out: Vec<u8>,
    answered: usize,
    edges: Vec<usize>,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
}

impl<'a> MaskMaker<'a> {
    pub fn new(unmix: &'a Unmix, rate: u32) -> MaskMaker<'a> {
        let n = (MODEL_FFT as f64 * rate as f64 / MODEL_RATE).round() as usize;
        let hop = (MODEL_HOP as f64 * rate as f64 / MODEL_RATE).round() as usize;
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
        // Periodic Hann, as torch.hann_window.
        let window = (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect();
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        MaskMaker {
            unmix,
            n,
            hop,
            scale: MODEL_FFT as f32 / n as f32,
            fps: (rate as f64 / hop as f64) as f32,
            fft,
            window,
            // Centred frames: the first is centred on the first sample, half a window of silence before it.
            samples: vec![0.0; n / 2 * 2],
            mags: Vec::new(),
            kept_from: 0,
            frames: 0,
            out: Vec::new(),
            answered: 0,
            edges: band_edges(),
            buf: vec![Complex32::default(); n],
            scratch,
        }
    }

    /// Interleaved samples of `channels` channels (mono is both sides; past two, the first two).
    pub fn feed(&mut self, x: &[f32], channels: usize) -> TractResult<()> {
        let ch = channels.max(1);
        for f in x.chunks_exact(ch) {
            self.samples.extend_from_slice(&[f[0], f[1.min(ch - 1)]]);
        }
        self.frames_ready()
    }

    /// Every whole window in `samples`, a hop apart.
    fn frames_ready(&mut self) -> TractResult<()> {
        let mut at = 0;
        while self.samples.len() - at >= 2 * self.n {
            self.frame(at)?;
            at += 2 * self.hop;
        }
        self.samples.drain(..at);
        Ok(())
    }

    /// The magnitudes of the window from interleaved sample `at`; none above the song's Nyquist frequency.
    fn frame(&mut self, at: usize) -> TractResult<()> {
        let n = self.n;
        let bins = IN_BINS.min(n / 2 + 1);
        for c in 0..2 {
            for (i, (b, w)) in self.buf.iter_mut().zip(&self.window).enumerate() {
                *b = Complex32::new(self.samples[at + 2 * i + c] * w, 0.0);
            }
            self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            self.mags.extend(self.buf[..bins].iter().map(|b| b.norm() * self.scale));
            self.mags.resize(self.mags.len() + IN_BINS - bins, 0.0);
        }
        self.frames += 1;
        if self.frames >= self.answered + WINDOW + BORDER {
            self.answer()?;
        }
        Ok(())
    }

    /// Runs the model over the next window and its context (silence where there are no frames) and keeps its mask.
    fn answer(&mut self) -> TractResult<()> {
        let per = 2 * IN_BINS;
        let start = self.answered as isize - BORDER as isize;
        let mut input = vec![0f32; CHUNK * per];
        for (k, row) in input.chunks_exact_mut(per).enumerate() {
            let t = start + k as isize;
            if t >= self.kept_from as isize && (t as usize) < self.frames {
                let at = (t as usize - self.kept_from) * per;
                row.copy_from_slice(&self.mags[at..at + per]);
            }
        }
        let mask = self.unmix.run(input)?;
        let mask = mask.to_plain_array_view::<f32>()?;
        let mask = mask.as_slice().ok_or_else(|| TractError::msg("the mask is not contiguous"))?;
        let take = WINDOW.min(self.frames - self.answered);
        for k in BORDER..BORDER + take {
            let frame = &mask[k * 2 * OUT_BINS..(k + 1) * 2 * OUT_BINS];
            for w in self.edges.windows(2) {
                let sum: f32 = (w[0]..w[1]).map(|b| frame[b] + frame[OUT_BINS + b]).sum();
                let share = (sum / (2 * (w[1] - w[0])) as f32).clamp(0.0, 1.0);
                self.out.push((share * 255.0).round() as u8);
            }
        }
        self.answered += take;
        // Keep only what the next window reads as context.
        let keep_from = self.answered.saturating_sub(BORDER);
        if keep_from > self.kept_from {
            self.mags.drain(..(keep_from - self.kept_from) * per);
            self.kept_from = keep_from;
        }
        Ok(())
    }

    /// The mask of everything fed: the last frames (centred to the end, silence after it) answered for.
    pub fn finish(mut self) -> TractResult<VocalMask> {
        self.samples.resize(self.samples.len() + self.n / 2 * 2, 0.0);
        self.frames_ready()?;
        while self.answered < self.frames {
            self.answer()?;
        }
        Ok(VocalMask::new(self.fps, self.out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The authors' checkpoint converts to the pinned weights, and the model answers a fixed input as tools/umx/export.py
    /// printed it (onnxruntime on the same fp16 graph):
    /// `NORI_UMX_CKPT=vocals-b62c91ce.pth cargo test --release -p nori-player --features neural-beats umx -- --ignored --nocapture`
    /// With `NORI_UMX_SONG` (raw stereo f32 at 44.1 kHz) it also times a whole song.
    #[test]
    #[ignore = "needs the authors' checkpoint in NORI_UMX_CKPT"]
    fn umx_matches_reference() {
        let Ok(ckpt) = std::env::var("NORI_UMX_CKPT") else { return };
        let t0 = std::time::Instant::now();
        let weights = crate::automix::weights::convert(GRAPH, &std::fs::read(ckpt).unwrap()).unwrap();
        use sha2::Digest;
        let pin: String = sha2::Sha256::digest(&weights).iter().map(|b| format!("{b:02x}")).collect();
        println!("converted in {:.0} ms, {} bytes, SHA-256 {pin}", t0.elapsed().as_secs_f64() * 1e3, weights.len());
        assert_eq!(pin, "49deff4c4c0b7f03068ab46d24f2e6c37f3f4c837109116e96c8fc64a8d7d1ad", "export.py's numpy makes the same bytes");
        let unmix = Unmix::from_weights(&weights).unwrap();
        // export.py's test input: 64 frames, then silence to a chunk.
        let mut mags = vec![0f32; CHUNK * 2 * IN_BINS];
        for t in 0..64 {
            for c in 0..2 {
                for k in 0..IN_BINS {
                    mags[(t * 2 + c) * IN_BINS + k] = 0.5 + 0.4 * (0.11 * t as f64 + 0.037 * k as f64 + 1.3 * c as f64).sin() as f32;
                }
            }
        }
        let out = unmix.run(mags).unwrap();
        let out = out.to_plain_array_view::<f32>().unwrap();
        let at = |t: usize, c: usize, k: usize| out[[t, c, k]];
        for ((t, c, k), want) in [((5, 1, 100), 0.118_541_93), ((33, 1, 1000), 0.074_441_43), ((48, 0, 1486), 0.132_628_62), ((63, 1, 2048), 0.014_129_64)] {
            assert!((at(t, c, k) - want).abs() < 1e-4, "mask at {t},{c},{k}: {}, onnxruntime {want}", at(t, c, k));
        }
        if let Ok(song) = std::env::var("NORI_UMX_SONG") {
            let x: Vec<f32> = std::fs::read(song).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            let t1 = std::time::Instant::now();
            let mut maker = MaskMaker::new(&unmix, 44_100);
            maker.feed(&x, 2).unwrap();
            let mask = maker.finish().unwrap();
            println!("{:.0} s of music masked in {:.2} s, {} frames, {} bytes stored", x.len() as f64 / 88_200.0, t1.elapsed().as_secs_f64(), mask.frames(), mask.to_bytes().len());
        }
    }
}
