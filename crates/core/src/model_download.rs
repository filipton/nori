//! Fetches a model's checkpoint (Beat This!, Open-Unmix) through the platform transport, verifies it and converts it
//! to the weights file (`nori_player::automix::weights`). Runs on the calling thread (nori-engine's measuring thread)
//! when a song is about to be read; a skipped or failed download is retried at the next song.

use std::path::{Path, PathBuf};

use nori_automix::beat_model::{BeatFailure, Model, ModelFile, State};
use sha2::{Digest, Sha256};

/// Timeout for the checkpoint download.
const TIMEOUT_MS: u32 = 120_000;

/// Beat This!'s weights file, downloading and converting it first through `client` if needed. None when disabled or
/// unavailable now (metered network not allowed, failure).
pub fn ensure(client: &crate::client::Client) -> Option<PathBuf> {
    let settings = &client.session().settings;
    let wanted = || settings.prefs(|p| p.auto_mix && p.auto_mix_better_beats);
    let mobile = settings.prefs(|p| p.auto_mix_beats_mobile_data);
    ensure_model(client, &settings.model, nori_player::automix::weights::GRAPH, wanted, mobile)
}

/// Sing's vocals model's weights file, as [`ensure`]; over Wi-Fi only.
pub fn ensure_sing(client: &crate::client::Client) -> Option<PathBuf> {
    let settings = &client.session().settings;
    ensure_model(client, &settings.sing_model, nori_player::sing::model::GRAPH, || settings.prefs(|p| p.sing), false)
}

/// `model`'s weights file for `graph`, made first while `wanted`; over mobile data only if `mobile`.
fn ensure_model(client: &crate::client::Client, model: &ModelFile, graph: &[u8], wanted: impl Fn() -> bool, mobile: bool) -> Option<PathBuf> {
    if !wanted() {
        return None;
    }
    if let Some(f) = model.ready() {
        return Some(f);
    }
    let file = model.file()?;
    if client.metered() && !mobile {
        model.set_state(State::WaitingForWifi);
        return None;
    }
    if !model.begin_download() {
        return None;
    }
    let spec = model.model;
    let t0 = std::time::Instant::now();
    let got = nori_net::transport::block_on(nori_net::transport::get(&*client.transport, spec.checkpoint_url.to_string(), TIMEOUT_MS))
        .map_err(|e| (BeatFailure::Network, e.to_string()))
        .and_then(|ckpt| {
            let fetched = t0.elapsed();
            let weights = make(spec, graph, &ckpt)?;
            drop(ckpt);
            store(&file, &weights).map_err(|e| (BeatFailure::Storage, e.to_string()))?;
            Ok((fetched, t0.elapsed()))
        });
    match got {
        // Disabled meanwhile: discard.
        Ok(_) if !wanted() => {
            let _ = std::fs::remove_file(&file);
            model.set_state(State::Absent);
            None
        }
        Ok((fetched, all)) => {
            crate::alog::info(&format!("{}: checkpoint fetched in {} ms, weights made and kept in {} ms", spec.file_name, fetched.as_millis(), all.as_millis()));
            model.set_state(State::Ready);
            Some(file)
        }
        Err((why, detail)) => {
            crate::alog::info(&format!("{} not made: {detail}", spec.file_name));
            model.set_state(State::Failed(why));
            None
        }
    }
}

/// Converts a checkpoint to the weights `graph` reads, verifying both against `model`'s pins.
pub fn make(model: &Model, graph: &[u8], ckpt: &[u8]) -> Result<Vec<u8>, (BeatFailure, String)> {
    if !pinned(ckpt, model.checkpoint_bytes, model.checkpoint_sha256) {
        return Err((BeatFailure::WrongFile, format!("{} bytes, not the pinned checkpoint", ckpt.len())));
    }
    let weights = nori_player::automix::weights::convert(graph, ckpt).map_err(|e| (BeatFailure::WrongFile, format!("converting the checkpoint: {e}")))?;
    if !pinned(&weights, model.bytes, model.sha256) {
        return Err((BeatFailure::WrongFile, format!("the checkpoint made {} bytes, not the pinned weights", weights.len())));
    }
    Ok(weights)
}

/// Replaces the model directory's contents with `weights`, written then renamed into place.
fn store(file: &Path, weights: &[u8]) -> std::io::Result<()> {
    let dir = file.parent().expect("a file in the model's directory");
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let part = file.with_extension("part");
    std::fs::write(&part, weights)?;
    std::fs::rename(&part, file)
}

/// Reads `model`'s weights file, verifying it against the pin.
pub fn read(model: &Model, file: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    if !pinned(&bytes, model.bytes, model.sha256) {
        return Err(format!("{} is not the pinned weights", file.display()));
    }
    Ok(bytes)
}

/// Whether `bytes` has length `len` and SHA-256 `sha` (lowercase hex).
fn pinned(bytes: &[u8], len: u64, sha: &str) -> bool {
    bytes.len() as u64 == len && Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>() == sha
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_automix::beat_model::{BEAT_THIS, UMX};

    #[test]
    fn make_rejects_unpinned_checkpoint() {
        for (model, graph) in [(&BEAT_THIS, nori_player::automix::weights::GRAPH), (&UMX, nori_player::sing::model::GRAPH)] {
            assert_eq!(make(model, graph, b"<html>not found</html>").unwrap_err().0, BeatFailure::WrongFile);
            assert_eq!(make(model, graph, &vec![0u8; model.checkpoint_bytes as usize]).unwrap_err().0, BeatFailure::WrongFile);
        }
    }
}
