//! Open-Unmix UMX-HQ's vocals model (MIT; tools/umx/export.py) through tract, as Sing's [`Separator`]. One plan
//! takes any number of frames: the graph's frame axis stays a symbol, and a run costs in proportion to its frames.

use std::sync::Arc;

use tract_onnx::prelude::*;

use super::{Separator, IN_BINS, MODEL_BINS};

/// The vocals model as ONNX, its weights left out (`crate::automix::weights`).
pub static GRAPH: &[u8] = include_bytes!("../../models/umx-hq-vocals.graph.onnx");

/// The model, loaded and optimised for runs of any length.
pub struct Unmix {
    model: Arc<TypedRunnableModel>,
}

impl Unmix {
    /// The app's graph filled with the weights file (`weights::convert`).
    pub fn from_weights(weights: &[u8]) -> TractResult<Self> {
        let proto = crate::automix::weights::assemble(GRAPH, weights).map_err(TractError::msg)?;
        let model = tract_onnx::onnx().model_for_proto_model(&proto)?;
        let frames = model.sym("frames");
        let model = model.with_input_fact(0, f32::fact(&[frames.to_dim(), 2.to_dim(), IN_BINS.to_dim()]).into())?.into_optimized()?.into_runnable()?;
        Ok(Unmix { model })
    }

    /// The model's mask, [frame][channel][bin], for `frames` frames of magnitudes laid out [frame][channel][bin].
    fn run(&self, mags: Vec<f32>, frames: usize) -> TractResult<Tensor> {
        let input: Tensor = tract_ndarray::Array3::from_shape_vec((frames, 2, IN_BINS), mags)?.into();
        Ok(self.model.run(tvec!(input.into()))?.remove(0).into_tensor())
    }
}

impl Separator for Unmix {
    fn separate(&self, mags: Vec<f32>, frames: usize, row: &mut dyn FnMut(usize, &[f32])) -> Result<(), String> {
        let mask = self.run(mags, frames).map_err(|e| e.to_string())?;
        let mask = mask.to_plain_array_view::<f32>().map_err(|e| e.to_string())?;
        let mask = mask.as_slice().ok_or("the mask is not contiguous")?;
        for (k, shares) in mask.as_chunks::<{ 2 * MODEL_BINS }>().0.iter().enumerate() {
            row(k, shares);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sing::{MaskMaker, VocalMask};

    /// The authors' checkpoint converts to the pinned weights, and the model answers a fixed input as tools/umx/export.py
    /// printed it (onnxruntime on the same fp16 graph), the same in a run of its own length as in a longer one:
    /// `NORI_UMX_CKPT=vocals-b62c91ce.pth cargo test --release -p nori-player --features neural-beats umx -- --ignored --nocapture`
    /// With `NORI_UMX_SONG` (raw stereo f32 at 44.1 kHz) it also times a whole song made a second at a time.
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
        let t1 = std::time::Instant::now();
        let unmix = Unmix::from_weights(&weights).unwrap();
        println!("planned in {:.0} ms", t1.elapsed().as_secs_f64() * 1e3);
        // export.py's test input: 64 frames, then silence to 640.
        let mags = |frames: usize| {
            let mut m = vec![0f32; frames * 2 * IN_BINS];
            for t in 0..64 {
                for c in 0..2 {
                    for k in 0..IN_BINS {
                        m[(t * 2 + c) * IN_BINS + k] = 0.5 + 0.4 * (0.11 * t as f64 + 0.037 * k as f64 + 1.3 * c as f64).sin() as f32;
                    }
                }
            }
            m
        };
        let out = unmix.run(mags(640), 640).unwrap();
        let out = out.to_plain_array_view::<f32>().unwrap();
        for ((t, c, k), want) in [((5, 1, 100), 0.118_541_93), ((33, 1, 1000), 0.074_441_43), ((48, 0, 1486), 0.132_628_62), ((63, 1, 2048), 0.014_129_64)] {
            assert!((out[[t, c, k]] - want).abs() < 1e-4, "mask at {t},{c},{k}: {}, onnxruntime {want}", out[[t, c, k]]);
        }
        let short = unmix.run(mags(64), 64).unwrap();
        assert_eq!(short.shape(), &[64, 2, MODEL_BINS], "a run of 64 frames answers for 64");
        if let Ok(song) = std::env::var("NORI_UMX_SONG") {
            let x: Vec<f32> = std::fs::read(song).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            let t1 = std::time::Instant::now();
            let mask = VocalMask::growing(MaskMaker::fps(44_100), x.len() / 2 / 1024 + 2);
            let mut maker = MaskMaker::new(44_100, 0);
            for second in x.chunks(88_200) {
                maker.feed(second);
                maker.answer(&unmix, &mask).unwrap();
            }
            mask.end(maker.end());
            maker.answer(&unmix, &mask).unwrap();
            assert!(mask.whole());
            println!("{:.0} s of music masked a second at a time in {:.2} s, {} frames, {} bytes stored", x.len() as f64 / 88_200.0, t1.elapsed().as_secs_f64(), mask.frames(), mask.to_bytes().len());
        }
    }
}
