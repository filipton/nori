//! The listen history is written on the background thread and the queue and scrobbler are process-wide,
//! so this gets its own test binary.

use std::sync::mpsc::channel;

use nori_core::scrobble::{scrobble_track, TrackChange};
use nori_core::{background, Core, Song};

#[test]
fn listen_kept_by_its_profile() {
    let first = Core::new(String::new(), "first".into()).unwrap();
    let song = Song { id: "s".into(), title: "Dogs".into(), duration: 600, ..Default::default() };
    nori_core::queue::queue_register(vec![song]);
    scrobble_track(Some("s".into()), TrackChange::Moved, true, 0, 1_000, 0);
    let (go, wait) = channel::<()>();
    background::run(move || {
        let _ = wait.recv();
    });
    scrobble_track(None, TrackChange::Ended, false, 120_000, 121_000, 0);
    let _second = Core::new(String::new(), "second".into()).unwrap();
    go.send(()).unwrap();
    background::flush();
    assert_eq!(first.history_recent(10, None, true).unwrap().0.len(), 1);
}
