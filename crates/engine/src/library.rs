//! Where songs come from: the client locates each id as a file, a URL (through a [`ByteSource`],
//! optionally via the stream cache) or a live stream. The engine opens it, keeps its bytes while it
//! plays, and fetches the next song in the same network wake.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::Thread;

use nori_player::pcm::Encoding;
use nori_player::pipeline::Songs;
use nori_player::transitions::WindowSong;

use crate::arriving::Listening;
use crate::demux::Demuxed;
use crate::source::{ByteSource, Keep, Loader, Waits};
use crate::store::Store;

/// Where one song's bytes are.
#[derive(Clone)]
pub enum Source {
    File(PathBuf),
    Url { url: String, bytes: Arc<dyn ByteSource> },
    /// A URL cached under `key`: read from disk when whole there, else written there as it loads.
    Cached { url: String, bytes: Arc<dyn ByteSource>, store: Arc<Store>, key: String },
    /// Internet radio: never cached or fetched ahead (`Loader::live`).
    Live { url: String, bytes: Arc<dyn ByteSource> },
}

/// A located song: its source, a container hint (extension or MIME type), and its tagged length.
#[derive(Clone)]
pub struct Located {
    pub source: Source,
    pub hint: Option<String>,
    pub duration_ms: Option<i64>,
    /// The server's byte length is a live transcode's estimate (Navidrome's `estimateContentLength`):
    /// nothing is asked past what is arriving (`Demuxed::load`), and the length is `duration_ms` until
    /// the bytes end.
    pub estimated: bool,
}

/// The client's side of the songs.
pub trait Library: Send + 'static {
    fn locate(&mut self, id: &str) -> Result<Located, String>;
    /// Length, album and track number of `id`, for the planner and seek bar.
    fn about(&self, id: &str) -> WindowSong;
    /// Whether `id` may be fetched unasked. Never a provider's song (octo-fiesta downloads it on request).
    fn fetch_ahead(&self, _id: &str) -> bool {
        true
    }

    /// `next` is being fetched (a song started, or the queue changed): the moment to fetch later songs
    /// to disk in the same network wake ([`crate::ahead`]).
    fn ahead(&mut self, _next: &str) {}

    /// What hears `id`'s bytes as it is fetched ahead (AutoMix measuring). Must not block the fetch
    /// (`Listening`'s `wait` off): the song may play meanwhile.
    fn taker(&self, _id: &str, _hint: Option<&str>) -> Option<Listening> {
        None
    }

    /// `id` played nothing and is restarted: drop its stream cache entry (a download stays).
    fn forget(&mut self, _id: &str) {}
}

/// Songs whose bytes are kept: the current and the next. The previous one is on disk by then, and
/// keeping it cost a whole song of memory for a rarely pressed button.
const KEPT: usize = 2;

/// The engine's [`Songs`]: the library's songs, their loaders kept while needed.
pub struct Sources<L: Library> {
    pub library: L,
    /// Decoded to float for high quality output, else 16-bit.
    pub encoding: Encoding,
    load: [i64; 5],
    waits: Waits,
    engine: Thread,
    loaders: Vec<(String, Arc<Loader>)>,
}

impl<L: Library> Sources<L> {
    /// `load` from `nori_player::transport::load_control`; `engine` is woken when awaited bytes arrive.
    pub fn new(library: L, load: [i64; 5], waits: Waits, engine: Thread) -> Sources<L> {
        Sources { library, encoding: Encoding::Pcm16, load, waits, engine, loaders: Vec::new() }
    }

    /// The loader of `id`, started if needed (writing `keep`'s cache entry, fed to `taker`), holding at
    /// most `budget` bytes (None: its window's cap).
    #[allow(clippy::too_many_arguments)]
    fn loader(&mut self, id: &str, url: &str, bytes: &Arc<dyn ByteSource>, duration_ms: Option<i64>, keep: Option<(&Arc<Store>, &str)>, budget: Option<u64>, taker: impl FnOnce() -> Option<Listening>) -> Arc<Loader> {
        // A failed loader is replaced: the network may be back.
        self.loaders.retain(|(i, l)| i != id || l.error().is_none());
        if let Some(k) = self.loaders.iter().position(|(i, _)| i == id) {
            let l = self.loaders.remove(k);
            let loader = l.1.clone();
            self.loaders.push(l);
            loader.limit(budget);
            return loader;
        }
        // Made on the loader's thread: the fetching ahead may have to hand the song over.
        let keep = keep.map(|(store, key)| {
            let (store, key) = (store.clone(), key.to_string());
            Box::new(move || store.writer_for_player(&key)) as Keep
        });
        let loader = Loader::start_within(bytes.clone(), url.to_string(), self.load, duration_ms, keep, budget, taker(), self.waits);
        self.keep(id, loader)
    }

    /// A live stream's loader, new on each open: what an earlier connection held is past.
    fn live(&mut self, id: &str, url: &str, bytes: &Arc<dyn ByteSource>) -> Arc<Loader> {
        self.loaders.retain(|(i, _)| i != id);
        let loader = Loader::live(bytes.clone(), url.to_string(), self.waits);
        self.keep(id, loader)
    }

    /// Keeps `loader` as the most recent, dropping the oldest past [`KEPT`].
    fn keep(&mut self, id: &str, loader: Arc<Loader>) -> Arc<Loader> {
        self.loaders.push((id.to_string(), loader.clone()));
        if self.loaders.len() > KEPT {
            self.loaders.remove(0);
        }
        loader
    }

    /// Song `id` from `from_ms` as undecoded packets; not for a live stream. `ahead`: only probed while
    /// another song plays, so it gets what that one leaves of the cap.
    pub fn open_packets(&mut self, id: &str, from_ms: i64, ahead: bool) -> Result<Demuxed, String> {
        self.open_as(id, from_ms, Some(ahead))
    }

    /// Song `id` from `from_ms`, decoded, or as packets (`Some(ahead)`, see [`Sources::open_packets`]).
    fn open_as(&mut self, id: &str, from_ms: i64, packets: Option<bool>) -> Result<Demuxed, String> {
        let at = self.library.locate(id)?;
        let (encoding, engine) = (self.encoding, self.engine.clone());
        let file = |path: &PathBuf| {
            let file = Box::new(std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
            let hint = at.hint.clone().or_else(|| path.extension().map(|e| e.to_string_lossy().into_owned()));
            match packets {
                Some(_) => Demuxed::open_packets(file, hint.as_deref(), from_ms, at.duration_ms),
                None => Demuxed::open(file, hint.as_deref(), from_ms, at.duration_ms, encoding),
            }
        };
        let load = |loader: Arc<Loader>, from_ms: i64, duration_ms: Option<i64>, estimated: bool| match packets {
            Some(_) => Demuxed::load_packets(loader, engine.clone(), at.hint.as_deref(), from_ms, duration_ms, estimated),
            None => Demuxed::load(loader, engine.clone(), at.hint.as_deref(), from_ms, duration_ms, estimated, encoding),
        };
        // Opened to play: the whole cap, whatever its budget was.
        let budget = packets.filter(|&ahead| ahead).map(|_| self.left_for(id));
        match &at.source {
            Source::File(path) => file(path),
            Source::Cached { store, key, .. } if self.loading(id).is_none() && store.cached(key).is_some() => file(&store.cached(key).expect("checked")),
            Source::Url { url, bytes } => Ok(load(self.loader(id, url, bytes, at.duration_ms, None, budget, || None), from_ms, at.duration_ms, at.estimated)),
            Source::Cached { url, bytes, store, key } => Ok(load(self.loader(id, url, bytes, at.duration_ms, Some((store, key)), budget, || None), from_ms, at.duration_ms, at.estimated)),
            // A live stream starts where the station is now.
            Source::Live { url, bytes } if packets.is_none() => Ok(load(self.live(id, url, bytes), 0, None, false)),
            Source::Live { .. } => Err("a live stream is decoded here".into()),
        }
    }

    /// Drops every song's bytes (a long pause).
    pub fn let_go(&mut self) {
        self.loaders.clear();
    }

    /// Drops `id`'s bytes in memory and its cache entry ([`Library::forget`]).
    pub fn forget(&mut self, id: &str) {
        self.loaders.retain(|(i, _)| i != id);
        self.library.forget(id);
    }

    /// Every loader's state in words ([`Loader::words`]), for a stall report.
    pub fn words(&self) -> String {
        if self.loaders.is_empty() {
            return "no loaders".into();
        }
        self.loaders.iter().map(|(id, l)| format!("{id}: {}", l.words())).collect::<Vec<_>>().join("; ")
    }

    /// The loader of `id`, if kept.
    pub fn loading(&self, id: &str) -> Option<&Arc<Loader>> {
        self.loaders.iter().find(|(i, _)| i == id).map(|(_, l)| l)
    }

    /// The library's taker for `id`, asked only when a new loader starts.
    fn taker(&self, id: &str, hint: Option<&str>) -> Option<Listening> {
        self.loading(id).is_none().then(|| self.library.taker(id, hint)).flatten()
    }

    /// What the other kept songs leave of the memory cap for `id`, fetched ahead. At least a sixth of
    /// it, so a mix into `id` has its start at hand.
    fn left_for(&self, id: &str) -> u64 {
        let cap = self.load[4].max(1) as u64;
        let others: u64 = self.loaders.iter().filter(|(i, _)| i != id).map(|(_, l)| l.holding()).sum();
        cap.saturating_sub(others).max(cap / 6)
    }
}

impl<L: Library> Songs for Sources<L> {
    type Reading = Demuxed;

    fn open(&mut self, id: &str, from_ms: i64) -> Result<Demuxed, String> {
        self.open_as(id, from_ms, None)
    }

    fn about(&self, id: &str) -> WindowSong {
        self.library.about(id)
    }

    fn upcoming(&mut self, id: &str) {
        self.library.ahead(id);
        if !self.library.fetch_ahead(id) {
            return;
        }
        match self.library.locate(id) {
            Ok(Located { source: Source::Url { url, bytes }, duration_ms, hint, .. }) => {
                let budget = self.left_for(id);
                let taker = self.taker(id, hint.as_deref());
                self.loader(id, &url, &bytes, duration_ms, None, Some(budget), || taker);
            }
            Ok(Located { source: Source::Cached { url, bytes, store, key }, duration_ms, hint, .. }) if store.cached(&key).is_none() => {
                let budget = self.left_for(id);
                let taker = self.taker(id, hint.as_deref());
                self.loader(id, &url, &bytes, duration_ms, Some((&store, &key)), Some(budget), || taker);
            }
            _ => {}
        }
    }
}
