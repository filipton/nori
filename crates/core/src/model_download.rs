//! Fetches a model's checkpoint (Beat This!, Open-Unmix) through the platform transport, verifies it and converts it
//! to the weights file (`nori_player::automix::weights`). Runs on the calling thread (nori-engine's measuring thread)
//! when a song is about to be read; a skipped or failed download is retried at the next song.

use std::path::{Path, PathBuf};

use nori_automix::beat_model::{BeatFailure, Model, ModelFile, State};
use sha2::{Digest, Sha256};

/// The checkpoint comes a piece at a time, so its download shows progress and a stalled piece fails alone.
const PIECE: u64 = 8 << 20;
/// Timeout for one piece.
const PIECE_TIMEOUT_MS: u32 = 90_000;

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

/// `model`'s weights file for `graph`, made first while `wanted`; over mobile data only if `mobile` or the user
/// asked for it now (`ModelFile::download_now`).
fn ensure_model(client: &crate::client::Client, model: &ModelFile, graph: &[u8], wanted: impl Fn() -> bool, mobile: bool) -> Option<PathBuf> {
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
    let got = fetch(&*client.transport, spec.checkpoint_url, spec.checkpoint_bytes, &wanted, |n| model.came(n))
        .map_err(|e| (BeatFailure::Network, e))
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

/// The `total` bytes at `url`, fetched a [`PIECE`] at a time (HTTP ranges) while `wanted`, `came` hearing how many have
/// come after each; a server that ignores ranges sends the whole file at once.
fn fetch(transport: &dyn nori_net::transport::Transport, url: &str, total: u64, wanted: impl Fn() -> bool, came: impl Fn(u64)) -> Result<Vec<u8>, String> {
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
        came(out.len() as u64);
    }
    Ok(out)
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

    #[test]
    fn sing_downloads_over_mobile_data_once_asked() {
        let dir = nori_testdir::TempDir::new("sing-mobile");
        let db = dir.path().join("nori.db").to_string_lossy().into_owned();
        let session: std::sync::Arc<nori_queue::Session> = Default::default();
        let core = crate::Core::new(db.clone(), "t".into(), session.clone()).unwrap();
        let settings = &session.settings;
        let prefs = settings.open(&db).unwrap();
        settings.put(crate::settings::StoredPrefs { sing: true, ..prefs });
        let fake = std::sync::Arc::new(crate::client::tests::Fake::default());
        *fake.metered.lock() = true;
        let client = crate::client::Client::new(core, fake.clone(), Default::default());

        assert_eq!(ensure_sing(&client), None);
        assert_eq!(settings.sing_model.state(), State::WaitingForWifi);
        assert!(fake.asked().is_empty(), "nothing fetched over mobile data");

        settings.sing_model.download_now();
        assert_eq!(ensure_sing(&client), None, "the fake transport fails");
        assert_eq!(fake.asked(), [UMX.checkpoint_url], "asked: fetched over mobile data");
        assert_eq!(fake.sent.lock()[0].headers["Range"], format!("bytes=0-{}", PIECE - 1), "the first piece");
        assert_eq!(settings.sing_model.state(), State::Failed(BeatFailure::Network));

        assert_eq!(ensure_sing(&client), None);
        assert_eq!((settings.sing_model.state(), fake.asked().len()), (State::Failed(BeatFailure::Network), 1), "the next one waits for Wi-Fi again, saying it failed");
    }

    #[test]
    fn fetch_comes_in_pieces() {
        let fake = crate::client::tests::Fake::default();
        let total = PIECE * 2 + 5;
        for n in [PIECE, PIECE, 5] {
            fake.answers.lock().push_back(Ok((206, vec![7; n as usize])));
        }
        let seen = std::sync::Mutex::new(Vec::new());
        let got = fetch(&fake, "u", total, || true, |n| seen.lock().unwrap().push(n)).unwrap();
        assert_eq!(got.len() as u64, total);
        assert_eq!(*seen.lock().unwrap(), [PIECE, 2 * PIECE, total]);
        let ranges: Vec<String> = fake.sent.lock().iter().map(|e| e.headers["Range"].clone()).collect();
        assert_eq!(ranges, [format!("bytes=0-{}", PIECE - 1), format!("bytes={}-{}", PIECE, 2 * PIECE - 1), format!("bytes={}-{}", 2 * PIECE, total - 1)]);

        // A server without ranges: the whole file at once.
        fake.answers.lock().push_back(Ok((200, vec![1; total as usize])));
        assert_eq!(fetch(&fake, "u", total, || true, |_| {}).unwrap().len() as u64, total);
        // An error, or an empty piece, ends it; so does the switch going off.
        fake.answers.lock().push_back(Ok((206, Vec::new())));
        assert!(fetch(&fake, "u", total, || true, |_| {}).is_err());
        let asked = fake.sent.lock().len();
        assert!(fetch(&fake, "u", total, || false, |_| {}).is_err());
        assert_eq!(fake.sent.lock().len(), asked, "nothing fetched once unwanted");
    }
}
