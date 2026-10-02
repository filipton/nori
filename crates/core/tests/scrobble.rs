//! A listen is written on the background thread: a profile opened over the same queue session meanwhile
//! does not take it.

use std::sync::mpsc::channel;
use std::sync::Arc;

use nori_core::queue::Session;
use nori_core::scrobble::TrackChange;
use nori_core::{background, Core, Song};

#[test]
fn listen_kept_by_its_profile() {
    let session = Arc::new(Session::default());
    let first = Core::new(String::new(), "first".into(), session.clone()).unwrap();
    let song = Song { id: "s".into(), title: "Dogs".into(), duration: 600, ..Default::default() };
    session.register(vec![song]);
    session.scrobble_track(Some("s".into()), TrackChange::Moved, true, 0, 1_000, 0);
    let (go, wait) = channel::<()>();
    background::run(move || {
        let _ = wait.recv();
    });
    session.scrobble_track(None, TrackChange::Ended, false, 120_000, 121_000, 0);
    let _second = Core::new(String::new(), "second".into(), session).unwrap();
    go.send(()).unwrap();
    background::flush();
    assert_eq!(first.history_recent(10, None, true).unwrap().0.len(), 1);
}
