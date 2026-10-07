//! Sing's vocal masks for the engine: made by the measurer for the current and next song while Sing is on
//! (`crate::core::Measurer`), kept on disk in the vocals model's directory up to [`DISK_BYTES`] (the least recently
//! used go first), and the near songs' held in memory for the player (`CoreApp::vocal_mask`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use nori_player::sing::VocalMask;
use parking_lot::Mutex;

/// Masks kept on disk at most (about 2 MB a song).
pub const DISK_BYTES: u64 = 64 << 20;
/// Songs whose masks Sing makes: the current and the next.
pub const AHEAD: usize = 2;

/// The near songs' masks in memory.
#[derive(Default)]
pub struct VocalMasks {
    held: Mutex<Vec<(String, Arc<VocalMask>)>>,
    /// Masks put so far, so each engine reading them sees new ones.
    made: AtomicU64,
}

impl VocalMasks {
    pub fn get(&self, id: &str) -> Option<Arc<VocalMask>> {
        self.held.lock().iter().find(|(i, _)| i == id).map(|(_, m)| m.clone())
    }

    pub fn put(&self, id: &str, mask: Arc<VocalMask>) {
        let mut held = self.held.lock();
        held.retain(|(i, _)| i != id);
        held.push((id.to_string(), mask));
        self.made.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn made(&self) -> u64 {
        self.made.load(Ordering::Acquire)
    }

    /// Lets go of every mask but those of `ids`.
    pub fn keep_only(&self, ids: &[String]) {
        self.held.lock().retain(|(i, _)| ids.contains(i));
    }
}

/// Where `id`'s mask is kept under the model's directory `dir` (the id hex-encoded, so any id is a file name).
fn path(dir: &Path, id: &str) -> PathBuf {
    let name: String = id.bytes().map(|b| format!("{b:02x}")).collect();
    dir.join("masks").join(name + ".mask")
}

/// `id`'s mask from disk, marked as just used; a mask of another version is deleted.
pub fn load(dir: &Path, id: &str) -> Option<VocalMask> {
    let p = path(dir, id);
    let bytes = std::fs::read(&p).ok()?;
    match VocalMask::from_bytes(&bytes) {
        Ok(m) => {
            let _ = std::fs::File::options().write(true).open(&p).and_then(|f| f.set_modified(std::time::SystemTime::now()));
            Some(m)
        }
        Err(_) => {
            let _ = std::fs::remove_file(&p);
            None
        }
    }
}

/// Keeps `id`'s mask on disk, then drops the least recently used masks past [`DISK_BYTES`].
pub fn store(dir: &Path, id: &str, mask: &VocalMask) -> std::io::Result<()> {
    let p = path(dir, id);
    std::fs::create_dir_all(p.parent().expect("in the masks directory"))?;
    let part = p.with_extension("part");
    std::fs::write(&part, mask.to_bytes())?;
    std::fs::rename(&part, &p)?;
    prune(&dir.join("masks"), DISK_BYTES);
    Ok(())
}

/// Deletes the least recently used files of `dir` until the rest fit in `cap` bytes.
fn prune(dir: &Path, cap: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    for (_, len, p) in files {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

/// The vocals model for a measuring thread's life: loaded on first need, tried once per thread and again
/// on news of it (`ModelFile::news`: the user asked for it, or it was made).
pub(crate) struct Unmixer {
    /// The model's news when it was last tried.
    tried: Option<u64>,
    loaded: Option<Model>,
}

impl Unmixer {
    pub(crate) fn new() -> Unmixer {
        Unmixer { tried: None, loaded: None }
    }

    /// The model, loading it (and downloading it first) on the first call.
    pub(crate) fn ready(&mut self, client: &nori_core::client::Client) -> Option<&Model> {
        let news = client.session().settings.sing_model.news();
        if self.loaded.is_none() && self.tried != Some(news) {
            self.tried = Some(news);
            self.loaded = Model::load(client);
        }
        self.loaded.as_ref()
    }
}

impl Drop for Unmixer {
    /// Frees the model and returns its pages.
    fn drop(&mut self) {
        if self.loaded.take().is_some() {
            crate::arriving::give_memory_back();
        }
    }
}

/// Open-Unmix through tract (`neural-beats`).
#[cfg(feature = "neural-beats")]
pub(crate) struct Model(nori_player::sing::model::Unmix);

#[cfg(feature = "neural-beats")]
impl Model {
    fn load(client: &nori_core::client::Client) -> Option<Model> {
        let file = nori_core::model_download::ensure_sing(client)?;
        let t0 = std::time::Instant::now();
        let loaded = nori_core::model_download::read(&nori_core::beat_model::UMX, &file).and_then(|b| nori_player::sing::model::Unmix::from_weights(&b).map_err(|e| e.to_string()));
        match loaded {
            Ok(m) => {
                nori_core::alog::info(&format!("vocals model loaded in {} ms", t0.elapsed().as_millis()));
                Some(Model(m))
            }
            Err(e) => {
                nori_core::alog::info(&format!("loading the vocals model: {e}"));
                None
            }
        }
    }

    /// A mask made from a song's samples as they are decoded.
    pub(crate) fn maker(&self) -> Making<'_> {
        Making { model: &self.0, maker: None, failed: None }
    }
}

/// A mask in the making; the first samples say the rate.
#[cfg(feature = "neural-beats")]
pub(crate) struct Making<'a> {
    model: &'a nori_player::sing::model::Unmix,
    maker: Option<nori_player::sing::model::MaskMaker<'a>>,
    failed: Option<String>,
}

#[cfg(feature = "neural-beats")]
impl Making<'_> {
    pub(crate) fn feed(&mut self, rate: u32, channels: usize, x: &[f32]) {
        if self.failed.is_some() {
            return;
        }
        let model = self.model;
        if let Err(e) = self.maker.get_or_insert_with(|| nori_player::sing::model::MaskMaker::new(model, rate)).feed(x, channels) {
            self.failed = Some(e.to_string());
        }
    }

    /// The mask, or why there is none.
    pub(crate) fn finish(self) -> Result<VocalMask, String> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        self.maker.ok_or("nothing decoded")?.finish().map_err(|e| e.to_string())
    }
}

/// Without `neural-beats`: never loaded.
#[cfg(not(feature = "neural-beats"))]
pub(crate) struct Model;

#[cfg(not(feature = "neural-beats"))]
impl Model {
    fn load(_: &nori_core::client::Client) -> Option<Model> {
        None
    }

    pub(crate) fn maker(&self) -> Making {
        Making
    }
}

#[cfg(not(feature = "neural-beats"))]
pub(crate) struct Making;

#[cfg(not(feature = "neural-beats"))]
impl Making {
    pub(crate) fn feed(&mut self, _: u32, _: usize, _: &[f32]) {}

    pub(crate) fn finish(self) -> Result<VocalMask, String> {
        Err("no vocals model in this build".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(frames: usize) -> VocalMask {
        VocalMask::new(43.0, vec![7; frames * nori_player::sing::bands()])
    }

    #[test]
    fn masks_kept_least_recently_used_out() {
        let dir = nori_testdir::TempDir::new("masks");
        store(&dir, "a", &mask(100)).unwrap();
        assert_eq!(load(&dir, "a"), Some(mask(100)));
        assert_eq!(load(&dir, "b"), None);
        let one = std::fs::metadata(path(&dir, "a")).unwrap().len();
        store(&dir, "b", &mask(100)).unwrap();
        // "a" used after "b" was kept: "b" is the one to go.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(path(&dir, "b")).unwrap().set_modified(old).unwrap();
        store(&dir, "c", &mask(100)).unwrap();
        prune(&dir.join("masks"), 2 * one);
        assert!(load(&dir, "b").is_none() && load(&dir, "a").is_some() && load(&dir, "c").is_some());
        // Another version is dropped, not read.
        std::fs::write(path(&dir, "a"), b"NVMK\x09rest").unwrap();
        assert_eq!(load(&dir, "a"), None);
        assert!(!path(&dir, "a").exists());
    }
}
