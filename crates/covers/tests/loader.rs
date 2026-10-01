//! Loader tests on real worker threads over a transport that records requests and can hold them.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use nori_covers::{header, Alpha, Config, DecodeError, Decoder, Error, Image, Key, Loader, Paint};
use nori_core::transport::{Exchange, FailureKind, Transport, TransportError, TransportResponse};
use parking_lot::{Condvar, Mutex};

const PHOTO: &str = "http://s/rest/getCoverArt.view?u=a&t=b&s=c&id=al-1&size=320";

struct Server {
    calls: Count,
    /// Requested URLs, in order.
    asked: Mutex<Vec<String>>,
    open: Mutex<bool>,
    opened: Condvar,
    status: u16,
    body: Mutex<Vec<u8>>,
}

impl Server {
    fn new(status: u16) -> Arc<Server> {
        let body = std::fs::read(format!("{}/testdata/photo.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
        Arc::new(Server { calls: Count::default(), asked: Mutex::new(Vec::new()), open: Mutex::new(true), opened: Condvar::new(), status, body: Mutex::new(body) })
    }

    fn hold(&self) {
        *self.open.lock() = false;
    }

    fn release(&self) {
        *self.open.lock() = true;
        self.opened.notify_all();
    }

    fn calls(&self) -> usize {
        self.calls.get()
    }

    /// Waits until `n` requests arrived.
    fn wait_calls(&self, n: usize) {
        self.calls.until(|c| c >= n);
    }
}

#[async_trait::async_trait]
impl Transport for Server {
    async fn get(&self, url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        self.asked.lock().push(url);
        self.calls.add(1);
        let mut open = self.open.lock();
        while !*open {
            self.opened.wait(&mut open);
        }
        if self.status == 0 {
            return Err(TransportError::Failed { kind: FailureKind::Connect, detail: Some("refused".into()) });
        }
        Ok(TransportResponse { status: self.status, body: self.body.lock().clone() })
    }

    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
        self.get(request.url, request.timeout_ms).await
    }

    fn address_changed(&self) {}
}

fn dir(name: &str) -> nori_testdir::TempDir {
    nori_testdir::TempDir::new(&format!("covers-loader-{name}"))
}

fn config(dir: Option<PathBuf>, workers: usize) -> Config {
    Config { dir, disk_bytes: 1 << 20, memory_bytes: 1 << 20, workers, alpha: Alpha::Straight, timeout_ms: 0, idle: Duration::from_secs(20) }
}

/// Receives `n` answers (10 s max each).
fn answers(rx: &mpsc::Receiver<Result<Arc<Image>, Error>>, n: usize) -> Vec<Result<Arc<Image>, Error>> {
    (0..n).map(|_| rx.recv_timeout(Duration::from_secs(10)).expect("an answer")).collect()
}

#[test]
fn concurrent_requests_share_one_fetch_and_decode() {
    let server = Server::new(200);
    server.hold();
    let loader = Loader::new(config(None, 3), server.clone());
    let (tx, rx) = mpsc::channel();
    let tickets: Vec<_> = (0..5)
        .map(|_| {
            let tx = tx.clone();
            loader.request(PHOTO, 16, 16, move |r| tx.send(r).unwrap())
        })
        .collect();
    server.release();
    let got = answers(&rx, 5);
    assert_eq!(server.calls(), 1);
    let first = got[0].as_ref().unwrap();
    assert_eq!((first.width, first.height, first.pixels.len()), (16, 16, 16 * 16 * 4));
    assert!(got.iter().all(|r| Arc::ptr_eq(r.as_ref().unwrap(), first)));
    // Served from memory afterwards.
    assert!(Arc::ptr_eq(&loader.cached(PHOTO, 16, 16).unwrap(), first));
    assert!(Arc::ptr_eq(&loader.load(PHOTO, 16, 16).unwrap(), first));
    assert_eq!(server.calls(), 1);
    // Another size decodes again.
    assert_eq!(loader.load(PHOTO, 8, 8).unwrap().width, 8);
    drop(tickets);
}

#[test]
fn cancelled_request_is_not_fetched_or_answered() {
    let server = Server::new(200);
    server.hold();
    let loader = Loader::new(config(None, 1), server.clone());
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    // The single worker is held on `a`; `b` is queued and cancelled.
    let first = loader.request("http://s/a", 8, 8, move |r| tx.send(r).unwrap());
    server.wait_calls(1);
    let second = loader.request("http://s/b", 8, 8, move |r| tx2.send(r).unwrap());
    second.cancel();
    server.release();
    assert!(answers(&rx, 1)[0].is_ok());
    assert!(loader.load("http://s/c", 8, 8).is_ok());
    assert_eq!(*server.asked.lock(), ["http://s/a", "http://s/c"], "b was never fetched");
    // The single worker served c after b's slot, so b would have been answered by now.
    assert!(rx.try_recv().is_err(), "the cancelled request was answered");
    first.detach();
}

#[test]
fn dropping_one_of_two_tickets_still_answers_the_other() {
    let server = Server::new(200);
    server.hold();
    let loader = Loader::new(config(None, 1), server.clone());
    let (tx, rx) = mpsc::channel();
    let busy = loader.request("http://s/busy", 8, 8, |_| {});
    server.wait_calls(1);
    let tx2 = tx.clone();
    let gone = loader.request(PHOTO, 8, 8, move |r| tx.send(r).unwrap());
    let kept = loader.request(PHOTO, 8, 8, move |r| tx2.send(r).unwrap());
    drop(gone);
    server.release();
    let got = answers(&rx, 1).remove(0).expect("the view still there gets the cover");
    assert_eq!((got.width, got.height), (8, 8));
    // The worker has moved on, so the dropped ticket would have been called back by now.
    loader.load("http://s/after", 8, 8).unwrap();
    assert!(rx.try_recv().is_err(), "the view that let go was called back");
    assert_eq!(*server.asked.lock(), ["http://s/busy", PHOTO, "http://s/after"], "one fetch for the two views");
    drop((busy, kept));
}

#[test]
fn disk_cache_survives_restart_except_provider_covers() {
    let d = dir("disk");
    let provider = "http://s/rest/getCoverArt.view?u=a&id=ext-deezer-1&size=320";
    {
        let loader = Loader::new(config(Some(d.to_path_buf()), 2), Server::new(200));
        loader.load(PHOTO, 8, 8).unwrap();
        loader.load(provider, 8, 8).unwrap();
        let disk = loader.disk().unwrap();
        assert!(disk.contains(Key::of(PHOTO)) && disk.path(Key::of(PHOTO)).exists());
        assert!(!disk.contains(Key::of(provider)));
    }
    // Server down: the kept cover loads, the provider's fails.
    let down = Server::new(0);
    let loader = Loader::new(config(Some(d.to_path_buf()), 2), down.clone());
    assert_eq!(loader.load(PHOTO, 8, 8).unwrap().width, 8);
    assert_eq!(down.calls(), 0);
    assert!(matches!(loader.load(provider, 8, 8), Err(Error::Transport { kind: FailureKind::Connect, .. })));
    drop(loader);
}

#[test]
fn an_answer_that_is_no_picture_is_not_kept() {
    let d = dir("no-picture");
    let server = Server::new(200);
    let photo = std::mem::replace(&mut *server.body.lock(), br#"{"subsonic-response":{"status":"failed"}}"#.to_vec());
    let loader = Loader::new(config(Some(d.to_path_buf()), 1), server.clone());
    assert!(matches!(loader.load(PHOTO, 8, 8), Err(Error::Decode(DecodeError::Unknown))));
    *server.body.lock() = photo;
    assert_eq!(loader.load(PHOTO, 8, 8).unwrap().width, 8);
    assert_eq!(server.calls(), 2);
}

/// Playlist covers (`pl-<id>_<changed>`) are kept like albums', and a cached cover is found offline
/// under another token/salt or the server's alternate address, but not at another size.
#[test]
fn playlist_cover_found_offline_across_auth_and_address() {
    let d = dir("playlist");
    nori_core::covers::cover_address_alike("http://lan.test:4533", "https://wan.test");
    let id = "pl-6b2d0c1e-5f7a-4e21-9d3c-0a1b2c3d4e5f_65f0a1b2";
    let at = |base: &str, t: &str, s: &str, size: u32| format!("{base}/rest/getCoverArt?u=a&t={t}&s={s}&v=1.16.1&c=nori&f=json&id={id}&size={size}");
    let first = at("http://lan.test:4533", "tok1", "salt1", 320);
    {
        let loader = Loader::new(config(Some(d.to_path_buf()), 2), Server::new(200));
        loader.load(&first, 8, 8).unwrap();
        assert!(loader.disk().unwrap().contains(Key::of(&first)), "the playlist's cover is kept");
    }
    let down = Server::new(0);
    let loader = Loader::new(config(Some(d.to_path_buf()), 2), down.clone());
    for url in [first.clone(), at("http://lan.test:4533", "tok2", "salt2", 320), at("https://wan.test", "tok1", "salt1", 320), at("http://wan.test", "tok3", "salt3", 320)] {
        assert_eq!(loader.load(&url, 8, 8).map(|p| p.width), Ok(8), "{url}");
    }
    assert_eq!(down.calls(), 0);
    assert!(loader.load(&at("http://lan.test:4533", "tok1", "salt1", 800), 8, 8).is_err(), "another size is another file");
    assert!(loader.load(&at("http://elsewhere.test", "tok1", "salt1", 320), 8, 8).is_err(), "another server's is its own");
    // Provider playlists are never kept.
    let provider = "http://lan.test:4533/rest/getCoverArt?u=a&id=pl-deezer-9&size=320";
    let up = Loader::new(config(Some(d.to_path_buf()), 1), Server::new(200));
    up.load(provider, 8, 8).unwrap();
    assert!(!up.disk().unwrap().contains(Key::of(provider)));
    drop((loader, up));
}

#[test]
fn legacy_full_url_key_is_found_and_migrated() {
    let d = dir("legacy");
    let bytes = std::fs::read(format!("{}/testdata/photo.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    nori_covers::DiskCache::open(d.to_path_buf(), 1 << 20).unwrap().put(Key::of_address(PHOTO), &bytes).unwrap();
    let down = Server::new(0);
    let loader = Loader::new(config(Some(d.to_path_buf()), 1), down.clone());
    assert_eq!(loader.load(PHOTO, 8, 8).map(|p| p.width), Ok(8));
    assert_eq!(down.calls(), 0);
    let disk = loader.disk().unwrap();
    assert!(disk.contains(Key::of(PHOTO)) && !disk.contains(Key::of_address(PHOTO)));
    drop(loader);
}

#[test]
fn error_status_is_not_cached() {
    let d = dir("error");
    let server = Server::new(404);
    let loader = Loader::new(config(Some(d.to_path_buf()), 1), server.clone());
    assert_eq!(loader.load(PHOTO, 8, 8), Err(Error::Status(404)));
    assert_eq!(loader.disk().unwrap().bytes(), 0);
    // Failures are not cached either.
    assert_eq!(loader.load(PHOTO, 8, 8), Err(Error::Status(404)));
    assert_eq!(server.calls(), 2);
    drop(loader);
}

#[test]
fn dropping_loader_answers_waiters_with_closed() {
    let server = Server::new(200);
    server.hold();
    let loader = Loader::new(config(None, 1), server.clone());
    let (tx, rx) = mpsc::channel();
    let _held = loader.request("http://s/a", 8, 8, |_| {});
    server.wait_calls(1);
    let _waiting = loader.request("http://s/b", 8, 8, move |r| tx.send(r).unwrap());
    drop(loader);
    assert_eq!(answers(&rx, 1)[0], Err(Error::Closed));
    server.release();
}

/// Custom painter recording the requested and computed sizes and counting decodes.
struct Counted(Arc<AtomicUsize>);

#[derive(Debug, Clone, PartialEq)]
struct Picture {
    asked: (u32, u32),
    got: (usize, usize),
}

impl Paint for Counted {
    type Picture = Arc<Picture>;

    fn paint(&self, _: &mut Decoder, bytes: &[u8], width: u32, height: u32) -> Result<Arc<Picture>, DecodeError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let h = header(bytes)?;
        Ok(Arc::new(Picture { asked: (width, height), got: h.fill(width as usize, height as usize) }))
    }

    fn bytes(_: &Arc<Picture>) -> usize {
        100
    }
}

#[test]
fn custom_painter_decodes_once_for_all_waiters() {
    let server = Server::new(200);
    server.hold();
    let painted = Arc::new(AtomicUsize::new(0));
    let loader = Loader::with_paint(Config { memory_bytes: 0, ..config(None, 2) }, server.clone(), Counted(painted.clone()));
    let (tx, rx) = mpsc::channel();
    let tickets: Vec<_> = (0..3)
        .map(|i| {
            let tx = tx.clone();
            loader.request(PHOTO, 30, 30, move |r| tx.send((i, r, std::thread::current().name().map(String::from))).unwrap())
        })
        .collect();
    server.release();
    let mut got: Vec<_> = (0..3).map(|_| rx.recv_timeout(Duration::from_secs(10)).expect("a callback")).collect();
    got.sort_by_key(|(i, ..)| *i);
    assert_eq!(got.iter().map(|(i, ..)| *i).collect::<Vec<_>>(), [0, 1, 2], "one call back each");
    let first = got[0].1.as_ref().unwrap();
    assert!(got.iter().all(|(_, r, _)| Arc::ptr_eq(r.as_ref().unwrap(), first)));
    // Callbacks run on a worker thread.
    assert!(got.iter().all(|(.., thread)| thread.as_deref() == Some("nori-covers")));
    // photo.jpg is 40x30.
    assert_eq!(**first, Picture { asked: (30, 30), got: (30, 30) });
    assert_eq!(painted.load(Ordering::SeqCst), 1);
    // memory_bytes 0: nothing cached, so it decodes again.
    assert!(loader.cached(PHOTO, 30, 30).is_none());
    loader.load(PHOTO, 30, 30).unwrap();
    assert_eq!(painted.load(Ordering::SeqCst), 2);
    drop(tickets);
}

#[test]
fn rerequest_after_drop_during_fetch_is_answered() {
    // Regression: skipping away and back while the cover downloads left the placeholder forever. The
    // new request must be answered whether it joins the running fetch or comes after it ended unwatched.
    let d = dir("again");
    let server = Server::new(200);
    server.hold();
    let loader = Loader::new(Config { memory_bytes: 0, ..config(Some(d.path().to_path_buf()), 1) }, server.clone());
    let (tx, rx) = mpsc::channel();
    let first = loader.request(PHOTO, 8, 8, |_| {});
    server.wait_calls(1);
    drop(first);
    let tx2 = tx.clone();
    let back = loader.request(PHOTO, 8, 8, move |r| tx2.send(r).unwrap());
    server.release();
    assert!(answers(&rx, 1)[0].is_ok(), "joined the fetch that was out");
    drop(back);
    server.hold();
    let gone = loader.request("http://s/other", 8, 8, |_| {});
    server.wait_calls(2);
    drop(gone);
    server.release();
    // The unwatched fetch has ended; a new request is served from disk.
    loader.load("http://s/next", 8, 8).unwrap();
    let again = loader.request("http://s/other", 8, 8, move |r| tx.send(r).unwrap());
    assert!(answers(&rx, 1)[0].is_ok(), "asked again after the flight ended with nobody waiting");
    assert_eq!(server.calls(), 3, "the second ask was answered from the disk");
    drop(again);
}

#[test]
fn ticket_dropped_during_fetch_skips_decode_and_callback() {
    let server = Server::new(200);
    server.hold();
    let painted = Arc::new(AtomicUsize::new(0));
    let loader = Loader::with_paint(Config { memory_bytes: 0, ..config(None, 1) }, server.clone(), Counted(painted.clone()));
    let (tx, rx) = mpsc::channel::<()>();
    let ticket = loader.request(PHOTO, 8, 8, move |_| tx.send(()).unwrap());
    server.wait_calls(1);
    drop(ticket);
    server.release();
    // Once the worker is free, the dropped cover was neither decoded nor answered.
    loader.load("http://s/next", 8, 8).unwrap();
    assert!(rx.try_recv().is_err(), "the view that left was called back");
    assert_eq!(painted.load(Ordering::SeqCst), 1, "decoded only the cover still wanted");
}

#[test]
fn warm_fetches_to_disk_once_without_decoding() {
    let d = dir("warm");
    let server = Server::new(200);
    let painted = Arc::new(AtomicUsize::new(0));
    let loader = Loader::with_paint(Config { memory_bytes: 0, ..config(Some(d.to_path_buf()), 1) }, server.clone(), Counted(painted.clone()));
    server.hold();
    // Worker busy; warm-ups and a later view queue behind it.
    let busy = loader.request("http://s/busy", 8, 8, |_| {});
    server.wait_calls(1);
    loader.warm(PHOTO);
    loader.warm("http://s/rest/getCoverArt.view?u=a&id=ext-deezer-1&size=320");
    let late = loader.request("http://s/late", 8, 8, |_| {});
    // Warm-ups run in order on the one worker: once `next` is asked for, the photo is on the disk.
    let next = "http://s/rest/getCoverArt.view?u=a&id=al-2&size=320";
    loader.warm(next);
    server.release();
    server.wait_calls(4);
    assert!(loader.disk().unwrap().contains(Key::of(PHOTO)), "warmed");
    // The view went before the warm-ups, the provider cover was skipped, nothing warmed was decoded.
    assert_eq!(*server.asked.lock(), ["http://s/busy", "http://s/late", PHOTO, next]);
    assert_eq!(painted.load(Ordering::SeqCst), 2);
    // Warming a cached cover does not refetch: once `then` is asked for, the repeated warm was processed.
    loader.warm(PHOTO);
    let then = "http://s/rest/getCoverArt.view?u=a&id=al-3&size=320";
    loader.warm(then);
    server.wait_calls(5);
    assert_eq!(*server.asked.lock(), ["http://s/busy", "http://s/late", PHOTO, next, then], "the warmed cover not fetched again");
    // `read` serves the raw bytes from disk.
    let mut bytes = Vec::new();
    loader.read(PHOTO, &mut bytes).unwrap();
    assert_eq!(bytes, *server.body.lock());
    assert_eq!(server.calls(), 5, "read from the disk, not fetched: {:?}", server.asked.lock());
    drop((busy, late));
    drop(loader);
}

/// Painter that panics at width 13.
struct Fragile;

impl Paint for Fragile {
    type Picture = (usize, usize);

    fn paint(&self, _: &mut Decoder, bytes: &[u8], width: u32, height: u32) -> Result<(usize, usize), DecodeError> {
        assert!(width != 13, "a decoder bug");
        Ok(header(bytes)?.fill(width as usize, height as usize))
    }

    fn bytes(_: &(usize, usize)) -> usize {
        100
    }
}

#[test]
fn panic_fails_only_that_cover() {
    let d = dir("panic");
    let server = Server::new(200);
    let loader = Loader::with_paint(Config { memory_bytes: 0, ..config(Some(d.to_path_buf()), 1) }, server.clone(), Fragile);
    for _ in 0..3 {
        assert!(matches!(loader.load(PHOTO, 13, 13), Err(Error::Panicked(why)) if why == "a decoder bug"));
        // The file is dropped from disk: it may be what broke the decoder.
        assert!(!loader.disk().unwrap().contains(Key::of(PHOTO)));
        assert_eq!(loader.load(PHOTO, 8, 8), Ok((8, 8)));
    }
    // A panicking callback does not kill the worker.
    let (tx, rx) = mpsc::channel();
    let t = loader.request(PHOTO, 9, 9, move |_| {
        tx.send(()).unwrap();
        panic!("a client bug")
    });
    rx.recv_timeout(Duration::from_secs(10)).expect("the call back ran");
    drop(t);
    assert_eq!(loader.load(PHOTO, 10, 10), Ok((10, 10)));
    drop(loader);
}

/// Painter counting live worker threads (via a thread-local guard) and `rest` calls.
struct Threads {
    alive: Arc<Count>,
    rests: Arc<AtomicUsize>,
}

struct Alive(Arc<Count>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.add(-1);
    }
}

thread_local! {
    static ALIVE: std::cell::RefCell<Option<Alive>> = const { std::cell::RefCell::new(None) };
}

impl Paint for Threads {
    type Picture = ();

    fn paint(&self, _: &mut Decoder, _: &[u8], _: u32, _: u32) -> Result<(), DecodeError> {
        ALIVE.with_borrow_mut(|a| {
            if a.is_none() {
                self.alive.add(1);
                *a = Some(Alive(self.alive.clone()));
            }
        });
        Ok(())
    }

    fn bytes(_: &()) -> usize {
        0
    }

    fn rest(&self) {
        self.rests.fetch_add(1, Ordering::SeqCst);
    }
}

/// A count tests wait on.
#[derive(Default)]
struct Count(Mutex<usize>, Condvar);

impl Count {
    fn add(&self, n: isize) {
        let mut c = self.0.lock();
        *c = c.checked_add_signed(n).expect("a count stays positive");
        self.1.notify_all();
    }

    fn get(&self) -> usize {
        *self.0.lock()
    }

    /// Waits until `done` holds of the count.
    fn until(&self, done: impl Fn(usize) -> bool) {
        let mut c = self.0.lock();
        while !done(*c) {
            self.1.wait(&mut c);
        }
    }
}

fn until(count: &Count, n: usize) {
    count.until(|c| c == n);
}

/// Starts three workers by holding three requests at the server at once.
fn three_at_once(loader: &Loader<Threads>, server: &Server) {
    server.hold();
    let calls = server.calls();
    let (tx, rx) = mpsc::channel();
    let tickets: Vec<_> = (0..3u32)
        .map(|i| {
            let tx = tx.clone();
            loader.request(&format!("http://s/{i}"), 8, 8, move |r| tx.send(r).unwrap())
        })
        .collect();
    server.wait_calls(calls + 3);
    server.release();
    for _ in 0..3 {
        rx.recv_timeout(Duration::from_secs(10)).expect("an answer").unwrap();
    }
    drop(tickets);
}

#[test]
fn rest_ends_workers_and_next_request_restarts_one() {
    let server = Server::new(200);
    let (alive, rests) = (Arc::new(Count::default()), Arc::new(AtomicUsize::new(0)));
    let loader = Loader::with_paint(Config { memory_bytes: 0, ..config(None, 3) }, server.clone(), Threads { alive: alive.clone(), rests: rests.clone() });
    three_at_once(&loader, &server);
    assert_eq!(alive.get(), 3);
    loader.rest();
    until(&alive, 0);
    assert_eq!(rests.load(Ordering::SeqCst), 1);
    loader.load("http://s/after", 8, 8).unwrap();
    assert_eq!(alive.get(), 1);
    // Shorter than `idle`: the thread stays.
    loader.load("http://s/again", 8, 8).unwrap();
    assert_eq!((alive.get(), rests.load(Ordering::SeqCst)), (1, 1));
}

#[test]
fn idle_loader_rests() {
    let server = Server::new(200);
    let (alive, rests) = (Arc::new(Count::default()), Arc::new(AtomicUsize::new(0)));
    let config = Config { memory_bytes: 0, idle: Duration::from_millis(500), ..config(None, 3) };
    let loader = Loader::with_paint(config, server.clone(), Threads { alive: alive.clone(), rests: rests.clone() });
    three_at_once(&loader, &server);
    // Requests closer together than `idle` keep the threads.
    for i in 0..5 {
        loader.load(&format!("http://s/soon-{i}"), 8, 8).unwrap();
    }
    assert_eq!((alive.get(), rests.load(Ordering::SeqCst)), (3, 0));
    until(&alive, 0);
    assert_eq!(rests.load(Ordering::SeqCst), 1);
    loader.load("http://s/after", 8, 8).unwrap();
    assert_eq!(alive.get(), 1);
}

#[test]
fn hidden_loader_rests_and_keeps_no_idle_thread() {
    let server = Server::new(200);
    let (alive, rests) = (Arc::new(Count::default()), Arc::new(AtomicUsize::new(0)));
    let loader = Loader::with_paint(Config { memory_bytes: 0, ..config(None, 3) }, server.clone(), Threads { alive: alive.clone(), rests: rests.clone() });
    three_at_once(&loader, &server);
    loader.show(false);
    until(&alive, 0);
    assert_eq!(rests.load(Ordering::SeqCst), 1);
    // A request while hidden (a notification's cover): its worker exits right after.
    loader.load("http://s/notification", 8, 8).unwrap();
    until(&alive, 0);
    loader.show(true);
    loader.load("http://s/shown", 8, 8).unwrap();
    // Shown, the worker stays for the next: that one takes the same thread.
    loader.load("http://s/shown-again", 8, 8).unwrap();
    assert_eq!((alive.get(), rests.load(Ordering::SeqCst)), (1, 1));
}
