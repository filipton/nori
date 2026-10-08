//! Fetches the Beat This! checkpoint through the platform transport, verifies it and converts it to the weights file (`nori_player::automix::weights`). Runs on the calling thread (nori-engine's measuring thread)
//! when a song is about to be read; a skipped or failed download is retried at the next song.

use std::path::{Path, PathBuf};

use nori_automix::beat_model::{BeatFailure, Model, State};
use sha2::{Digest, Sha256};

/// The checkpoint comes a piece at a time, so its download shows progress and a stalled piece fails alone.
const PIECE: u64 = 8 << 20;
/// Timeout for one piece.
const PIECE_TIMEOUT_MS: u32 = 90_000;

/// Beat This!'s weights file, downloading and converting it first through `client` if needed. None when disabled or
/// unavailable now (metered network not allowed, failure).
pub fn ensure(client: &crate::client::Client) -> Option<PathBuf> {
    let settings = &client.session().settings;
    let model = &settings.model;
    let wanted = || settings.prefs(|p| p.auto_mix && p.auto_mix_better_beats);
    let mobile = settings.prefs(|p| p.auto_mix_beats_mobile_data);
    if !wanted() {
        return None;
    }
    if let Some(f) = model.ready() {
        return Some(f);
    }
    let file = model.file()?;
    if !model.may_fetch(client.metered(), mobile) {
        return None;
    }
    if !model.begin_download() {
        return None;
    }
    let spec = model.model;
    let t0 = std::time::Instant::now();
    let got = fetch(&*client.transport, spec.checkpoint_url, spec.checkpoint_bytes, wanted)
        .map_err(|e| (BeatFailure::Network, e))
        .and_then(|ckpt| {
            let fetched = t0.elapsed();
            let weights = make(spec, &ckpt)?;
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

/// The `total` bytes at `url`, fetched a [`PIECE`] at a time (HTTP ranges) while `wanted`; a server that ignores ranges
/// sends the whole file at once.
fn fetch(transport: &dyn nori_net::transport::Transport, url: &str, total: u64, wanted: impl Fn() -> bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(total as usize);
    while (out.len() as u64) < total {
        if !wanted() {
            return Err("no longer wanted".into());
        }
        let from = out.len() as u64;
        let to = (from + PIECE).min(total) - 1;
        let ask = nori_net::transport::Exchange { url: url.to_string(), headers: [("Range".to_string(), format!("bytes={from}-{to}"))].into(), json: None, timeout_ms: PIECE_TIMEOUT_MS };
        let r = nori_net::transport::block_on(transport.send(ask)).map_err(|e| e.to_string())?;
        match r.status {
            206 if !r.body.is_empty() => out.extend_from_slice(&r.body),
            200 if from == 0 => out = r.body,
            status => return Err(format!("HTTP {status} for bytes {from}-{to}")),
        }
    }
    Ok(out)
}

/// Converts a checkpoint to the weights the graph reads, verifying both against `model`'s pins.
pub fn make(model: &Model, ckpt: &[u8]) -> Result<Vec<u8>, (BeatFailure, String)> {
    if !pinned(ckpt, model.checkpoint_bytes, model.checkpoint_sha256) {
        return Err((BeatFailure::WrongFile, format!("{} bytes, not the pinned checkpoint", ckpt.len())));
    }
    let weights = nori_player::automix::weights::convert(ckpt).map_err(|e| (BeatFailure::WrongFile, format!("converting the checkpoint: {e}")))?;
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
    use nori_automix::beat_model::BEAT_THIS;

    #[test]
    fn make_rejects_unpinned_checkpoint() {
        assert_eq!(make(&BEAT_THIS, b"<html>not found</html>").unwrap_err().0, BeatFailure::WrongFile);
        assert_eq!(make(&BEAT_THIS, &vec![0u8; BEAT_THIS.checkpoint_bytes as usize]).unwrap_err().0, BeatFailure::WrongFile);
    }

    #[test]
    fn fetch_comes_in_pieces() {
        let fake = crate::client::tests::Fake::default();
        let total = PIECE * 2 + 5;
        for n in [PIECE, PIECE, 5] {
            fake.answers.lock().push_back(Ok((206, vec![7; n as usize])));
        }
        let got = fetch(&fake, "u", total, || true).unwrap();
        assert_eq!(got.len() as u64, total);
        let ranges: Vec<String> = fake.sent.lock().iter().map(|e| e.headers["Range"].clone()).collect();
        assert_eq!(ranges, [format!("bytes=0-{}", PIECE - 1), format!("bytes={}-{}", PIECE, 2 * PIECE - 1), format!("bytes={}-{}", 2 * PIECE, total - 1)]);

        // A server without ranges: the whole file at once.
        fake.answers.lock().push_back(Ok((200, vec![1; total as usize])));
        assert_eq!(fetch(&fake, "u", total, || true).unwrap().len() as u64, total);
        // An error, or an empty piece, ends it; so does the switch going off.
        fake.answers.lock().push_back(Ok((206, Vec::new())));
        assert!(fetch(&fake, "u", total, || true).is_err());
        let asked = fake.sent.lock().len();
        assert!(fetch(&fake, "u", total, || false).is_err());
        assert_eq!(fake.sent.lock().len(), asked, "nothing fetched once unwanted");
    }
}
