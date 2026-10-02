//! Fetches the Beat This! checkpoint through the platform transport, verifies it and converts it to the
//! weights file (`nori_player::automix::weights`). Runs on the calling thread (nori-engine's measuring
//! thread) when a song is about to be read; a skipped or failed download is retried at the next song.

use std::path::{Path, PathBuf};

use nori_automix::beat_model::{BeatFailure, State, BYTES, CHECKPOINT_BYTES, CHECKPOINT_SHA256, CHECKPOINT_URL, SHA256};
use sha2::{Digest, Sha256};

/// Timeout for the checkpoint download.
const TIMEOUT_MS: u32 = 120_000;

/// The weights file, downloading and converting it first through `client` if needed. None when disabled or
/// unavailable now (metered network not allowed, failure).
pub fn ensure(client: &crate::client::Client) -> Option<PathBuf> {
    let settings = &client.session().settings;
    let model = &settings.model;
    let wanted = || settings.prefs(|p| p.auto_mix && p.auto_mix_better_beats);
    if !wanted() {
        return None;
    }
    if let Some(f) = model.ready() {
        return Some(f);
    }
    let file = model.file()?;
    let mobile = settings.prefs(|p| p.auto_mix_beats_mobile_data);
    if nori_net::stream::metered() && !mobile {
        model.set_state(State::WaitingForWifi);
        return None;
    }
    if !model.begin_download() {
        return None;
    }
    let t0 = std::time::Instant::now();
    let got = nori_net::transport::block_on(nori_net::transport::get(&*client.transport, CHECKPOINT_URL.to_string(), TIMEOUT_MS))
        .map_err(|e| (BeatFailure::Network, e.to_string()))
        .and_then(|ckpt| {
            let fetched = t0.elapsed();
            let weights = make(&ckpt)?;
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
            crate::alog::info(&format!("beat model: checkpoint fetched in {} ms, weights made and kept in {} ms", fetched.as_millis(), all.as_millis()));
            model.set_state(State::Ready);
            Some(file)
        }
        Err((why, detail)) => {
            crate::alog::info(&format!("beat model not made: {detail}"));
            model.set_state(State::Failed(why));
            None
        }
    }
}

/// Converts a checkpoint to weights, verifying both against their pins.
pub fn make(ckpt: &[u8]) -> Result<Vec<u8>, (BeatFailure, String)> {
    if !pinned(ckpt, CHECKPOINT_BYTES, CHECKPOINT_SHA256) {
        return Err((BeatFailure::WrongFile, format!("{} bytes, not the pinned checkpoint", ckpt.len())));
    }
    let weights = nori_player::automix::weights::convert(ckpt).map_err(|e| (BeatFailure::WrongFile, format!("converting the checkpoint: {e}")))?;
    if !pinned(&weights, BYTES, SHA256) {
        return Err((BeatFailure::WrongFile, format!("the checkpoint made {} bytes, not the pinned weights", weights.len())));
    }
    Ok(weights)
}

/// Replaces the models directory's contents with `weights`, written then renamed into place.
fn store(file: &Path, weights: &[u8]) -> std::io::Result<()> {
    let dir = file.parent().expect("a file in the models directory");
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let part = file.with_extension("part");
    std::fs::write(&part, weights)?;
    std::fs::rename(&part, file)
}

/// Reads the weights file, verifying it against the pin.
pub fn read(file: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    if !pinned(&bytes, BYTES, SHA256) {
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

    #[test]
    fn make_rejects_unpinned_checkpoint() {
        assert_eq!(make(b"<html>not found</html>").unwrap_err().0, BeatFailure::WrongFile);
        assert_eq!(make(&vec![0u8; CHECKPOINT_BYTES as usize]).unwrap_err().0, BeatFailure::WrongFile);
    }
}
