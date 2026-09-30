//! MPRIS (`org.mpris.MediaPlayer2`) media controls on the Linux session bus, served on one thread.
//! Position is read on request, so nothing ticks. Elsewhere [`Mpris::start`] returns an error.

use std::sync::{Arc, RwLock};

/// Client callbacks, called on the bus thread.
pub trait Controls: Send + Sync + 'static {
    fn play(&self);
    fn pause(&self);
    fn toggle(&self);
    fn next(&self);
    fn previous(&self);
    /// Seeks the current song to `ms`.
    fn seek(&self, ms: i64);
    fn now(&self) -> Now;
}

/// Playback state and current song.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Now {
    pub playing: bool,
    /// A song is loaded: paused rather than stopped.
    pub loaded: bool,
    /// Queue index, used as the track id.
    pub index: Option<usize>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub length_ms: i64,
    pub position_ms: i64,
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use dbus::arg::{PropMap, RefArg, Variant};
    use dbus::blocking::stdintf::org_freedesktop_dbus::PropertiesPropertiesChanged;
    use dbus::blocking::SyncConnection;
    use dbus::channel::{MatchingReceiver, Sender};
    use dbus::message::{MatchRule, SignalArgs};
    use dbus::Path;
    use dbus_crossroads::Crossroads;

    use super::{Controls, Now};

    const OBJECT: &str = "/org/mpris/MediaPlayer2";
    const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

    fn status(n: &Now) -> String {
        match (n.playing, n.loaded) {
            (true, _) => "Playing",
            (false, true) => "Paused",
            _ => "Stopped",
        }
        .to_string()
    }

    fn track_id(n: &Now) -> Path<'static> {
        match n.index {
            Some(i) => Path::new(format!("/org/nori/track/{i}")).expect("a valid path"),
            None => Path::new("/org/mpris/MediaPlayer2/TrackList/NoTrack").expect("a valid path"),
        }
    }

    fn metadata(n: &Now) -> PropMap {
        let mut m: PropMap = HashMap::new();
        let mut put = |k: &str, v: Box<dyn RefArg>| {
            m.insert(k.to_string(), Variant(v));
        };
        put("mpris:trackid", Box::new(track_id(n)));
        if n.index.is_some() {
            put("mpris:length", Box::new(n.length_ms * 1000));
            put("xesam:title", Box::new(n.title.clone()));
            put("xesam:artist", Box::new(vec![n.artist.clone()]));
            put("xesam:album", Box::new(n.album.clone()));
        }
        m
    }

    pub struct Served {
        conn: Arc<SyncConnection>,
        controls: Arc<dyn Controls>,
    }

    impl Served {
        pub fn start(name: &str, controls: Arc<dyn Controls>) -> Result<Served, String> {
            let conn = Arc::new(SyncConnection::new_session().map_err(|e| e.to_string())?);
            conn.request_name(format!("org.mpris.MediaPlayer2.{name}"), false, true, false).map_err(|e| e.to_string())?;
            let mut cr = Crossroads::new();
            let identity = name.to_string();
            let root = cr.register("org.mpris.MediaPlayer2", move |b| {
                let id = identity.clone();
                b.property("Identity").get(move |_, _: &mut Arc<dyn Controls>| Ok(id.clone()));
                b.property("CanQuit").get(|_, _: &mut Arc<dyn Controls>| Ok(false));
                b.property("CanRaise").get(|_, _: &mut Arc<dyn Controls>| Ok(false));
                b.property("HasTrackList").get(|_, _: &mut Arc<dyn Controls>| Ok(false));
                b.property("SupportedUriSchemes").get(|_, _: &mut Arc<dyn Controls>| Ok(Vec::<String>::new()));
                b.property("SupportedMimeTypes").get(|_, _: &mut Arc<dyn Controls>| Ok(Vec::<String>::new()));
                b.method("Raise", (), (), |_, _: &mut Arc<dyn Controls>, _: ()| Ok(()));
                b.method("Quit", (), (), |_, _: &mut Arc<dyn Controls>, _: ()| Ok(()));
            });
            let player = cr.register(PLAYER, |b| {
                b.property("PlaybackStatus").get(|_, c: &mut Arc<dyn Controls>| Ok(status(&c.now())));
                b.property("Metadata").get(|_, c: &mut Arc<dyn Controls>| Ok(metadata(&c.now())));
                b.property("Position").emits_changed_false().get(|_, c: &mut Arc<dyn Controls>| Ok(c.now().position_ms * 1000));
                b.property("Rate").get(|_, _: &mut Arc<dyn Controls>| Ok(1.0f64));
                b.property("MinimumRate").get(|_, _: &mut Arc<dyn Controls>| Ok(1.0f64));
                b.property("MaximumRate").get(|_, _: &mut Arc<dyn Controls>| Ok(1.0f64));
                b.property("Volume").get(|_, _: &mut Arc<dyn Controls>| Ok(1.0f64));
                for name in ["CanGoNext", "CanGoPrevious", "CanPlay", "CanPause", "CanSeek", "CanControl"] {
                    b.property(name).get(|_, _: &mut Arc<dyn Controls>| Ok(true));
                }
                b.method("Play", (), (), |_, c: &mut Arc<dyn Controls>, _: ()| {
                    c.play();
                    Ok(())
                });
                b.method("Pause", (), (), |_, c: &mut Arc<dyn Controls>, _: ()| {
                    c.pause();
                    Ok(())
                });
                b.method("PlayPause", (), (), |_, c: &mut Arc<dyn Controls>, _: ()| {
                    c.toggle();
                    Ok(())
                });
                b.method("Stop", (), (), |_, c: &mut Arc<dyn Controls>, _: ()| {
                    c.pause();
                    Ok(())
                });
                b.method("Next", (), (), |_, c: &mut Arc<dyn Controls>, _: ()| {
                    c.next();
                    Ok(())
                });
                b.method("Previous", (), (), |_, c: &mut Arc<dyn Controls>, _: ()| {
                    c.previous();
                    Ok(())
                });
                b.method("Seek", ("Offset",), (), |_, c: &mut Arc<dyn Controls>, (offset,): (i64,)| {
                    let at = c.now().position_ms + offset / 1000;
                    c.seek(at.max(0));
                    Ok(())
                });
                b.method("SetPosition", ("TrackId", "Position"), (), |_, c: &mut Arc<dyn Controls>, (track, at): (Path<'static>, i64)| {
                    // Ignored for a stale track id.
                    if track == track_id(&c.now()) {
                        c.seek(at / 1000);
                    }
                    Ok(())
                });
            });
            cr.insert(OBJECT, &[root, player], controls.clone());
            let served = conn.clone();
            std::thread::Builder::new()
                .name("nori-mpris".into())
                .spawn(move || {
                    let cr = std::sync::Mutex::new(cr);
                    served.start_receive(
                        MatchRule::new_method_call(),
                        Box::new(move |msg, conn| {
                            if let Ok(mut cr) = cr.lock() {
                                let _ = cr.handle_message(msg, conn);
                            }
                            true
                        }),
                    );
                    while served.process(Duration::from_secs(3600)).is_ok() {}
                })
                .map_err(|e| e.to_string())?;
            Ok(Served { conn, controls })
        }

        pub fn changed(&self) {
            let n = self.controls.now();
            let mut changed: PropMap = HashMap::new();
            changed.insert("PlaybackStatus".into(), Variant(Box::new(status(&n))));
            changed.insert("Metadata".into(), Variant(Box::new(metadata(&n))));
            let sig = PropertiesPropertiesChanged { interface_name: PLAYER.into(), changed_properties: changed, invalidated_properties: Vec::new() };
            let _ = self.conn.send(sig.to_emit_message(&Path::new(OBJECT).expect("a valid path")));
        }
    }
}

/// The controls of the session open now; nothing while none is.
#[derive(Default)]
struct Current(RwLock<Option<Arc<dyn Controls>>>);

impl Current {
    fn with(&self, f: impl FnOnce(&dyn Controls)) {
        if let Some(c) = self.0.read().unwrap_or_else(|p| p.into_inner()).as_deref() {
            f(c);
        }
    }
}

impl Controls for Current {
    fn play(&self) {
        self.with(|c| c.play());
    }
    fn pause(&self) {
        self.with(|c| c.pause());
    }
    fn toggle(&self) {
        self.with(|c| c.toggle());
    }
    fn next(&self) {
        self.with(|c| c.next());
    }
    fn previous(&self) {
        self.with(|c| c.previous());
    }
    fn seek(&self, ms: i64) {
        self.with(|c| c.seek(ms));
    }
    fn now(&self) -> Now {
        let mut now = Now::default();
        self.with(|c| now = c.now());
        now
    }
}

/// Served as `org.mpris.MediaPlayer2.<name>` for the process: one bus name, whichever session is open.
pub struct Mpris {
    current: Arc<Current>,
    #[cfg(target_os = "linux")]
    served: linux::Served,
}

impl Mpris {
    /// Errors without a session bus, or off Linux.
    pub fn start(name: &str) -> Result<Mpris, String> {
        let current = Arc::new(Current::default());
        #[cfg(target_os = "linux")]
        return Ok(Mpris { served: linux::Served::start(name, current.clone())?, current });
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (name, current);
            Err("MPRIS is Linux's".into())
        }
    }

    /// Media keys drive `controls` from now on; None while no session is open.
    pub fn serve(&self, controls: Option<Arc<dyn Controls>>) {
        *self.current.0.write().unwrap_or_else(|p| p.into_inner()) = controls;
        self.changed();
    }

    /// Emits PropertiesChanged for status and metadata.
    pub fn changed(&self) {
        #[cfg(target_os = "linux")]
        self.served.changed();
    }
}

#[cfg(test)]
mod current_tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Presses(Mutex<u32>);

    impl Controls for Presses {
        fn play(&self) {
            *self.0.lock().unwrap() += 1;
        }
        fn pause(&self) {}
        fn toggle(&self) {}
        fn next(&self) {}
        fn previous(&self) {}
        fn seek(&self, _: i64) {}
        fn now(&self) -> Now {
            Now { playing: true, ..Now::default() }
        }
    }

    #[test]
    fn keys_reach_the_session_open_now() {
        let current = Current::default();
        current.play();
        assert!(!current.now().playing, "no session");
        let (old, new) = (Arc::new(Presses::default()), Arc::new(Presses::default()));
        *current.0.write().unwrap() = Some(old.clone());
        current.play();
        *current.0.write().unwrap() = Some(new.clone());
        current.play();
        current.play();
        assert_eq!((*old.0.lock().unwrap(), *new.0.lock().unwrap()), (1, 2));
        assert!(current.now().playing);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
    use dbus::blocking::Connection;

    #[derive(Default)]
    struct Asked(Mutex<Vec<String>>);

    impl Controls for Asked {
        fn play(&self) {
            self.0.lock().unwrap().push("play".into());
        }
        fn pause(&self) {
            self.0.lock().unwrap().push("pause".into());
        }
        fn toggle(&self) {
            self.0.lock().unwrap().push("toggle".into());
        }
        fn next(&self) {
            self.0.lock().unwrap().push("next".into());
        }
        fn previous(&self) {
            self.0.lock().unwrap().push("previous".into());
        }
        fn seek(&self, ms: i64) {
            self.0.lock().unwrap().push(format!("seek {ms}"));
        }
        fn now(&self) -> Now {
            Now { playing: true, loaded: true, index: Some(2), title: "T".into(), artist: "A".into(), album: "B".into(), length_ms: 200_000, position_ms: 30_000 }
        }
    }

    #[test]
    fn controls_and_properties_over_dbus() {
        let Ok(client) = Connection::new_session() else {
            eprintln!("no session bus: skipped");
            return;
        };
        let name = format!("nori_test_{}", std::process::id());
        let asked = Arc::new(Asked::default());
        let m = Mpris::start(&name).unwrap();
        m.serve(Some(asked.clone()));
        let p = client.with_proxy(format!("org.mpris.MediaPlayer2.{name}"), "/org/mpris/MediaPlayer2", Duration::from_secs(5));
        let status: String = p.get("org.mpris.MediaPlayer2.Player", "PlaybackStatus").unwrap();
        assert_eq!(status, "Playing");
        let position: i64 = p.get("org.mpris.MediaPlayer2.Player", "Position").unwrap();
        assert_eq!(position, 30_000_000, "µs");
        let _: () = p.method_call("org.mpris.MediaPlayer2.Player", "PlayPause", ()).unwrap();
        let _: () = p.method_call("org.mpris.MediaPlayer2.Player", "Next", ()).unwrap();
        let _: () = p.method_call("org.mpris.MediaPlayer2.Player", "Seek", (5_000_000i64,)).unwrap();
        let track = dbus::Path::new("/org/nori/track/2").unwrap();
        let _: () = p.method_call("org.mpris.MediaPlayer2.Player", "SetPosition", (track, 90_000_000i64)).unwrap();
        let stale = dbus::Path::new("/org/nori/track/1").unwrap();
        let _: () = p.method_call("org.mpris.MediaPlayer2.Player", "SetPosition", (stale, 10_000_000i64)).unwrap();
        assert_eq!(*asked.0.lock().unwrap(), ["toggle", "next", "seek 35000", "seek 90000"]);
        m.changed();
    }
}
