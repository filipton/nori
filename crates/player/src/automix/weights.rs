//! Beat This!'s weights, made on the device from the authors' checkpoint. The app carries the network without
//! weights ([`GRAPH`], from tools/beat-this/export.py); each initializer is external data in one weights file with a
//! recipe (`nori.op`) from the checkpoint's state_dict: `copy`, `transpose` (a Linear folded into MatMul), `conv_bn`
//! (a Conv2d with its BatchNorm folded in) or `bn_shift` (the bias that fold leaves). [`convert`] writes the file
//! once, one IEEE float32 operation per step so every platform makes the pinned bytes; [`assemble`] puts it back
//! into the graph for tract.

use std::collections::HashMap;

use prost::Message;
use tract_onnx::pb::{tensor_proto, ModelProto, TensorProto};
use tract_onnx::prelude::f16;

use super::checkpoint::{self, Tensor};

/// Beat This! small0 as ONNX, its weights left out.
pub static GRAPH: &[u8] = include_bytes!("../../models/beat-this-small0.graph.onnx");

const FLOAT: i32 = tensor_proto::DataType::Float as i32;
const FLOAT16: i32 = tensor_proto::DataType::Float16 as i32;
const EXTERNAL: i32 = tensor_proto::DataLocation::External as i32;

/// Where an initializer's bytes are in the weights file.
fn place(t: &TensorProto) -> Result<(usize, usize), String> {
    let get = |k: &str| t.external_data.iter().find(|e| e.key == k).map(|e| e.value.as_str());
    let num = |k: &str| get(k).and_then(|v| v.parse::<usize>().ok()).ok_or_else(|| format!("{}: no {k}", t.name));
    Ok((num("offset")?, num("length")?))
}

/// The graph's initializers that come from the weights file, in the order they lie in it.
fn external(model: &ModelProto) -> Result<Vec<&TensorProto>, String> {
    let graph = model.graph.as_ref().ok_or("no graph")?;
    Ok(graph.initializer.iter().filter(|t| t.data_location == Some(EXTERNAL)).collect())
}

fn graph() -> Result<ModelProto, String> {
    ModelProto::decode(GRAPH).map_err(|e| format!("the graph: {e}"))
}

/// The weights file the graph expects, made from the checkpoint's bytes (`small0.ckpt`, checked by the caller).
pub fn convert(ckpt: &[u8]) -> Result<Vec<u8>, String> {
    let sd = checkpoint::state_dict(ckpt)?;
    let model = graph()?;
    let mut out = Vec::new();
    for t in external(&model)? {
        let get = |k: &str| t.external_data.iter().find(|e| e.key == k).map(|e| e.value.as_str());
        let tensor = |k: &str| sd.get(k).ok_or_else(|| format!("the checkpoint has no {k}"));
        let eps = || get("nori.eps").and_then(|e| e.parse::<f64>().ok()).map(|e| e as f32).ok_or_else(|| format!("{}: no epsilon", t.name));
        let from = get("nori.from").ok_or_else(|| format!("{}: no source", t.name))?;
        let dims: Vec<usize> = t.dims.iter().map(|d| usize::try_from(*d).map_err(|_| format!("{}: a negative size", t.name))).collect::<Result<_, _>>()?;
        let values = match get("nori.op") {
            Some("copy") => tensor(from)?.clone(),
            Some("transpose") => transpose(tensor(from)?)?,
            Some("conv_bn") => {
                let (w, s) = (tensor(from)?, bn_scale(&sd, get("nori.bn").unwrap_or(""), eps()?)?);
                let per = w.numel() / s.len().max(1);
                if w.shape.first() != Some(&s.len()) {
                    return Err(format!("{from} and its BatchNorm disagree"));
                }
                Tensor { shape: w.shape.clone(), data: w.data.iter().enumerate().map(|(i, v)| v * s[i / per]).collect() }
            }
            Some("bn_shift") => {
                let s = bn_scale(&sd, from, eps()?)?;
                let (beta, mean) = (tensor(&format!("{from}.bias"))?, tensor(&format!("{from}.running_mean"))?);
                if beta.numel() != s.len() || mean.numel() != s.len() {
                    return Err(format!("{from}: its parts disagree"));
                }
                Tensor { shape: vec![s.len()], data: beta.data.iter().zip(&mean.data).zip(&s).map(|((b, m), s)| b - m * s).collect() }
            }
            op => return Err(format!("{}: no recipe {op:?}", t.name)),
        };
        if values.shape != dims {
            return Err(format!("{}: {from} is {:?}, the graph wants {dims:?}", t.name, values.shape));
        }
        let (offset, length) = place(t)?;
        if offset != out.len() {
            return Err(format!("{}: not where the last one ended", t.name));
        }
        match t.data_type {
            FLOAT => out.extend(values.data.iter().flat_map(|v| v.to_le_bytes())),
            FLOAT16 => out.extend(values.data.iter().flat_map(|v| f16::from_f32(*v).to_bits().to_le_bytes())),
            _ => return Err(format!("{}: neither fp32 nor fp16", t.name)),
        }
        if out.len() != offset + length {
            return Err(format!("{}: {} bytes, the graph says {length}", t.name, out.len() - offset));
        }
    }
    Ok(out)
}

/// A 2-D tensor turned.
fn transpose(t: &Tensor) -> Result<Tensor, String> {
    let [r, c] = t.shape[..] else { return Err("a transpose of something not 2-D".into()) };
    let data = (0..c).flat_map(|j| (0..r).map(move |i| (i, j))).map(|(i, j)| t.data[i * c + j]).collect();
    Ok(Tensor { shape: vec![c, r], data })
}

/// gamma / sqrt(var + eps) per channel of the BatchNorm `bn`, one float32 rounding per step.
fn bn_scale(sd: &HashMap<String, Tensor>, bn: &str, eps: f32) -> Result<Vec<f32>, String> {
    let get = |k: &str| sd.get(&format!("{bn}.{k}")).ok_or_else(|| format!("the checkpoint has no {bn}.{k}"));
    let (gamma, var) = (get("weight")?, get("running_var")?);
    if gamma.numel() != var.numel() {
        return Err(format!("{bn}: its parts disagree"));
    }
    Ok(gamma.data.iter().zip(&var.data).map(|(g, v)| g / (v + eps).sqrt()).collect())
}

/// The graph with the weights file's bytes in it, for tract.
pub fn assemble(weights: &[u8]) -> Result<ModelProto, String> {
    let mut model = graph()?;
    let graph = model.graph.as_mut().ok_or("no graph")?;
    let mut end = 0;
    for t in graph.initializer.iter_mut().filter(|t| t.data_location == Some(EXTERNAL)) {
        let (offset, length) = place(t)?;
        t.raw_data = weights.get(offset..offset + length).ok_or_else(|| format!("{}: past the end of the weights", t.name))?.to_vec();
        t.external_data.clear();
        t.data_location = None;
        end = end.max(offset + length);
    }
    if end != weights.len() {
        return Err(format!("{} bytes of weights, the graph reads {end}", weights.len()));
    }
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automix::neural::{BeatThis, CHUNK, MELS};

    /// Every initializer comes from the weights file; the constants left are tiny.
    #[test]
    fn graph_carries_no_weights() {
        let model = graph().unwrap();
        let g = model.graph.as_ref().unwrap();
        assert!(g.initializer.iter().all(|t| t.data_location == Some(EXTERNAL) && t.raw_data.is_empty()));
        let ext = external(&model).unwrap();
        let total: usize = ext.iter().map(|t| place(t).unwrap().1).sum();
        assert_eq!((ext.len(), total), (138, 4_229_216));
        let biggest = g.node.iter().flat_map(|n| &n.attribute).filter_map(|a| a.t.as_ref()).map(|t| t.raw_data.len()).max().unwrap_or(0);
        assert!(biggest < 64, "a constant of {biggest} bytes");
        assert!(GRAPH.len() < 256 << 10, "{} bytes", GRAPH.len());
        assert!(assemble(&[0u8; 10]).is_err(), "a short file is refused");
    }

    /// The authors' checkpoint converts to the pinned bytes and loads:
    /// `NORI_BEAT_THIS_CKPT=small0.ckpt cargo test --release -p nori-player --features neural-beats official_weights -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the authors' checkpoint in NORI_BEAT_THIS_CKPT"]
    fn official_weights_convert_to_pin() {
        let Ok(ckpt) = std::env::var("NORI_BEAT_THIS_CKPT") else {
            eprintln!("no checkpoint in NORI_BEAT_THIS_CKPT: skipped");
            return;
        };
        let ckpt = std::fs::read(ckpt).unwrap();
        let rss = || {
            let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            s.lines().find_map(|l| l.strip_prefix("VmHWM:").map(|v| v.trim().to_string())).unwrap_or_default()
        };
        let before = rss();
        let t0 = std::time::Instant::now();
        let weights = convert(&ckpt).unwrap();
        let converted = t0.elapsed();
        let pin = sha(&weights);
        println!("converted in {:.0} ms, {} bytes, SHA-256 {pin}; peak RSS {before} before, {} after", converted.as_secs_f64() * 1e3, weights.len(), rss());
        assert_eq!(pin, "e9349da04b9da4ad41c5e416c71a9471af3a416249e7addef0101b3d569df5a7", "export.py's numpy makes the same bytes");
        let t1 = std::time::Instant::now();
        let ours = BeatThis::from_weights(&weights).unwrap();
        println!("assembled and loaded in {:.0} ms; peak RSS {}", t1.elapsed().as_secs_f64() * 1e3, rss());

        assert!(ours.track(&[[0.0; MELS]; CHUNK], true, true).is_ok());
    }

    fn sha(b: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(b).iter().map(|b| format!("{b:02x}")).collect()
    }
}
