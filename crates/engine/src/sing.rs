//! Sing's vocal masks for the engine: made on a thread of their own from the samples the player feeds as it reads
//! the song playing and the next (`nori_player::pipeline::App::sing_feed`), only while Sing is on and its model is
//! in; kept on disk once whole, in the vocals model's directory up to [`DISK_BYTES`] (the least recently used go
//! first), and the near songs' held in memory for the player (`CoreApp::vocal_mask`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::Thread;

use nori_player::sing::{Feed, Feeding, MaskMaker, Separator, VocalMask};
use parking_lot::Mutex;

use crate::core::Analyses;

/// Masks kept on disk at most (about 2 MB a song).
pub const DISK_BYTES: u64 = 64 << 20;
/// Songs whose masks are held in memory: the current and the next.
pub const AHEAD: usize = 2;
/// Seconds of samples a feed holds for the mask maker: what the player reads ahead of the rows, and more.
const FEED_SECONDS: u32 = 16;
/// How long the mask maker's thread keeps the model with no feed (it takes a second or two to load on a phone).
const LINGER: std::time::Duration = std::time::Duration::from_secs(60);

/// The near songs' masks in memory, and those being made.
#[derive(Default)]
pub struct VocalMasks {
    held: Mutex<Vec<(String, Arc<VocalMask>)>>,
    /// News so far (a mask came or went, the model came), so each engine reading them sees new ones.
    made: AtomicU64,
    /// The engine's thread, woken by news.
    engine: Mutex<Option<Thread>>,
    making: Mutex<Making>,
}

/// The feeds the mask maker's thread has yet to take, and that thread while it runs.
#[derive(Default)]
struct Making {
    feeds: Vec<Arc<Feed>>,
    thread: Option<Thread>,
}

impl VocalMasks {
    pub fn get(&self, id: &str) -> Option<Arc<VocalMask>> {
        self.held.lock().iter().find(|(i, _)| i == id).map(|(_, m)| m.clone())
    }

    fn put(&self, id: &str, mask: Arc<VocalMask>) {
        let mut held = self.held.lock();
        held.retain(|(i, _)| i != id);
        held.push((id.to_string(), mask));
    }

    pub(crate) fn made(&self) -> u64 {
        self.made.load(Ordering::Acquire)
    }

    /// The engine looks again: a mask came or went, or the model came.
    pub(crate) fn news(&self) {
        self.made.fetch_add(1, Ordering::AcqRel);
        if let Some(t) = self.engine.lock().as_ref() {
            t.unpark();
        }
    }

    /// The engine's thread is the calling one.
    pub(crate) fn engine_is_here(&self) {
        let mut engine = self.engine.lock();
        if engine.as_ref().is_none_or(|t| t.id() != std::thread::current().id()) {
            *engine = Some(std::thread::current());
        }
    }

    /// Lets go of every mask but those of `ids`; of all of them, and of the model kept for the next, with none.
    pub fn keep_only(&self, ids: &[String]) {
        self.held.lock().retain(|(i, _)| ids.contains(i));
        if let Some(t) = self.making.lock().thread.as_ref().filter(|_| ids.is_empty()) {
            t.unpark();
        }
    }
}

/// Sing's vocals model: the one downloaded ([`Downloaded`]), or a test's.
pub trait Vocals: Send + Sync {
    /// It can be loaded now, without waiting for a download.
    fn ready(&self, client: &nori_core::client::Client) -> bool;
    /// Fetches it where the settings allow (waiting for it); whether it is in.
    fn fetch(&self, client: &nori_core::client::Client) -> bool;
    fn load(&self, client: &nori_core::client::Client) -> Option<Box<dyn Separator>>;
}

/// Open-Unmix from the file the user downloaded (`neural-beats`; without it, never there).
pub struct Downloaded;

impl Vocals for Downloaded {
    fn ready(&self, client: &nori_core::client::Client) -> bool {
        cfg!(feature = "neural-beats") && client.session().settings.sing_model.ready().is_some()
    }

    #[cfg(feature = "neural-beats")]
    fn fetch(&self, client: &nori_core::client::Client) -> bool {
        nori_core::model_download::ensure_sing(client).is_some()
    }

    #[cfg(not(feature = "neural-beats"))]
    fn fetch(&self, _: &nori_core::client::Client) -> bool {
        false
    }

    #[cfg(feature = "neural-beats")]
    fn load(&self, client: &nori_core::client::Client) -> Option<Box<dyn Separator>> {
        let file = nori_core::model_download::ensure_sing(client)?;
        let t0 = std::time::Instant::now();
        let loaded = nori_core::model_download::read(&nori_core::beat_model::UMX, &file).and_then(|b| nori_player::sing::model::Unmix::from_weights(&b).map_err(|e| e.to_string()));
        match loaded {
            Ok(m) => {
                nori_core::alog::info(&format!("vocals model loaded in {} ms", t0.elapsed().as_millis()));
                Some(Box::new(m))
            }
            Err(e) => {
                nori_core::alog::info(&format!("loading the vocals model: {e}"));
                None
            }
        }
    }

    #[cfg(not(feature = "neural-beats"))]
    fn load(&self, _: &nori_core::client::Client) -> Option<Box<dyn Separator>> {
        None
    }
}

/// A feed of `id`'s samples from song frame `from` at `rate` while its mask is made, if Sing is on, the model is in
/// and the mask is not whole; the mask is held from then on.
pub(crate) fn feed(analyses: &Arc<Analyses>, id: &str, from: u64, rate: u32, duration_us: i64) -> Option<Feeding> {
    let client = analyses.client()?;
    if duration_us <= 0 || !client.session().settings.prefs(|p| p.sing) || !analyses.vocals().ready(&client) {
        return None;
    }
    let masks = &analyses.masks;
    let mask = match masks.get(id) {
        Some(m) if m.whole() => return None,
        Some(m) if !m.given_up() => m,
        _ => {
            let fps = MaskMaker::fps(rate);
            // Room for a song a little longer than listed.
            let m = Arc::new(VocalMask::growing(fps, (duration_us as f64 / 1e6 * 1.05 * fps as f64) as usize + 64));
            masks.put(id, m.clone());
            m
        }
    };
    let mut making = masks.making.lock();
    let maker = match &making.thread {
        Some(t) => t.clone(),
        None => {
            let a = analyses.clone();
            let t = std::thread::Builder::new().name("nori-sing".into()).spawn(move || make(a)).ok()?.thread().clone();
            making.thread = Some(t.clone());
            t
        }
    };
    let feed = Arc::new(Feed::new(id, mask, from, rate, FEED_SECONDS, maker.clone(), std::thread::current()));
    making.feeds.push(feed.clone());
    maker.unpark();
    Some(Feeding(feed))
}

/// One feed in the making.
struct Made {
    feed: Arc<Feed>,
    maker: MaskMaker,
    /// Looked for on the disk, and kept there once whole.
    looked: bool,
    stored: bool,
    /// CPU time its rows took, ms.
    cpu_ms: u64,
}

/// The mask maker's thread: takes what each feed holds and makes what rows it can, loading the model when first
/// needed; ends (dropping the model) once no feed is left.
fn make(analyses: Arc<Analyses>) {
    crate::arriving::lower_priority();
    let masks = &analyses.masks;
    let mut model: Option<Box<dyn Separator>> = None;
    let mut making: Vec<Made> = Vec::new();
    let mut x = Vec::new();
    let mut lingered = false;
    loop {
        {
            let mut m = masks.making.lock();
            making.extend(m.feeds.drain(..).map(|feed| Made { maker: MaskMaker::new(feed.rate, feed.from), feed, looked: false, stored: false, cpu_ms: 0 }));
            if making.is_empty() {
                if lingered {
                    m.thread = None;
                    break;
                }
                // The next song's feed usually comes soon: the model is kept for it a while.
                drop(m);
                lingered = true;
                std::thread::park_timeout(LINGER);
                continue;
            }
            lingered = false;
        }
        let client = analyses.client();
        let dir = client.as_ref().and_then(|c| c.session().settings.sing_model.dir());
        let mut worked = false;
        let mut failed = false;
        making.retain_mut(|m| {
            if failed {
                return true;
            }
            let (feed, mask) = (&m.feed, &m.feed.mask);
            if !std::mem::replace(&mut m.looked, true) && !mask.begun() {
                if let Some(stored) = dir.as_deref().and_then(|d| load(d, &feed.id)) {
                    masks.put(&feed.id, Arc::new(stored));
                    mask.give_up();
                    masks.news();
                    feed.rows_came();
                    return false;
                }
            }
            x.clear();
            let n = feed.take(&mut x);
            worked |= n > 0;
            m.maker.feed(&x);
            if feed.ended() && feed.done() {
                mask.end(m.maker.end());
            }
            // A run when it reads more new frames than history, or rows are waited for.
            let run = m.maker.waiting() > 0 && (m.maker.due() || feed.done() || mask.awaited());
            if run {
                if model.is_none() {
                    model = client.as_deref().and_then(|c| analyses.vocals().load(c));
                }
                let Some(model) = model.as_deref() else {
                    failed = true;
                    return false;
                };
                let t0 = crate::arriving::thread_cpu_ms();
                while m.maker.waiting() > 0 {
                    if let Err(e) = m.maker.answer(model, mask) {
                        nori_core::alog::info(&format!("vocal mask of {}: {e}", feed.id));
                        mask.give_up();
                        masks.news();
                        break;
                    }
                }
                if let (Some(a), Some(b)) = (t0, crate::arriving::thread_cpu_ms()) {
                    m.cpu_ms += b.saturating_sub(a);
                }
            }
            mask.made(n);
            if run || feed.done() {
                feed.rows_came();
            }
            if !m.stored && mask.whole() {
                m.stored = true;
                let kept = match dir.as_deref().map(|d| store(d, &feed.id, mask)) {
                    Some(Ok(())) => "kept".to_string(),
                    Some(Err(e)) => format!("not kept: {e}"),
                    None => "not kept: no model directory".to_string(),
                };
                nori_core::alog::info(&format!("vocal mask of {} whole, {} frames made in {} ms of CPU, {kept}", feed.id, mask.frames(), m.cpu_ms));
            }
            !feed.done()
        });
        if failed {
            nori_core::alog::info("no vocals model: Sing's masks are given up");
            let mut m = masks.making.lock();
            for f in making.drain(..).map(|m| m.feed).chain(m.feeds.drain(..)) {
                f.mask.give_up();
                f.rows_came();
            }
            m.thread = None;
            masks.news();
            break;
        }
        if !worked {
            std::thread::park();
        }
    }
    if model.take().is_some() {
        crate::arriving::give_memory_back();
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
