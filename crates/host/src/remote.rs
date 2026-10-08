//! Remote control on the terminal and desktop clients: the session's player as the core's
//! [`RemotePlayer`], the active device while it is another one ([`Elsewhere`]), which the session's
//! controls then act on, and mDNS through mdns-sd for nearby devices (`desktop` feature). A [`Remote`] is
//! made only while remote control or jams are switched on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(feature = "desktop")]
use std::sync::Weak;
use std::time::Instant;

use nori_core::remote::{Mirror, MirrorRow, Playing, Remote, RemotePlayer, RemoteShown};
#[cfg(feature = "desktop")]
use nori_core::remote::{Announcement, Discovery};
use nori_core::Song;
use nori_engine::{Engine, State, Status};
use nori_remote::wire::Op;
use parking_lot::Mutex;

use crate::session::{Handle, Out, Said};
use crate::Level;

/// The session's remote, while there is one.
pub struct Remotes {
    slot: Mutex<Option<Arc<Remote>>>,
    level: Arc<Level>,
    /// The engine waits for a song's bytes with nothing left to play.
    buffering: AtomicBool,
    /// Another device plays, as last seen.
    mirroring: AtomicBool,
}

impl Remotes {
    pub(crate) fn new(level: Arc<Level>) -> Remotes {
        Remotes { slot: Mutex::default(), level, buffering: AtomicBool::new(false), mirroring: AtomicBool::new(false) }
    }

    /// The active device while it is another one: the player shows and controls it instead of this one.
    pub fn elsewhere(&self) -> Option<Elsewhere> {
        let remote = self.get()?;
        let mirror = remote.active()?;
        Some(Elsewhere { mirror, read: Instant::now(), remote })
    }

    /// Notes whether another device plays now; true when it just started to.
    fn mirroring(&self, on: bool) -> bool {
        let was = self.mirroring.swap(on, Ordering::Relaxed);
        on && !was
    }

    pub(crate) fn buffering(&self, on: bool) {
        self.buffering.store(on, Ordering::Relaxed);
    }

    pub fn get(&self) -> Option<Arc<Remote>> {
        self.slot.lock().clone()
    }

    pub(crate) fn set(&self, remote: Option<Arc<Remote>>) {
        // Out of the slot first: going, it tells [`Shown`], which reads the slot.
        let old = std::mem::replace(&mut *self.slot.lock(), remote);
        if let Some(old) = old {
            old.clone().serve(false);
            old.clone().watch(false);
            old.jam_close();
        }
    }

    /// Tells the remote (if any) where the engine is now.
    pub fn played(&self, engine: &Engine) {
        let Some(r) = self.get() else { return };
        r.played(self.playing(&engine.status()));
    }

    /// The playback other devices see, from the engine's status `s`.
    fn playing(&self, s: &Status) -> Playing {
        Playing {
            playing: s.state == State::Playing,
            buffering: self.buffering.load(Ordering::Relaxed),
            position_ms: s.position_now().max(0),
            index: s.index.map(|i| i as u32),
            volume: self.level.percent(),
        }
    }
}

/// What another device asks of this one, done to the session's queue and engine as its own keys would.
pub(crate) struct HostPlayer(pub(crate) Handle);

impl RemotePlayer for HostPlayer {
    fn apply(&self, op: Op) {
        let h = &self.0;
        match op {
            Op::Play => {
                h.engine.play();
            }
            Op::Pause => {
                h.engine.pause();
            }
            Op::Seek { ms } => {
                h.engine.seek(ms);
            }
            Op::Next => {
                h.engine.next();
            }
            Op::Previous => {
                h.engine.previous();
            }
            Op::Jump { index, .. } => {
                h.engine.play_at(index as usize, 0);
            }
            Op::Remove { index, .. } => h.remove(index as usize),
            Op::Move { from, to, .. } => h.move_song(from as usize, to as usize),
            Op::Add { songs, next } => h.enqueue(songs, next),
            Op::Replace { songs, index, position_ms, play, order, shuffle, repeat } => h.replace(songs, index as usize, position_ms, play, order, shuffle, repeat),
            Op::Shuffle { on } => h.shuffle(on),
            Op::Repeat { mode } => h.repeat(mode),
            Op::Volume { percent } => return h.volume_from_afar(percent as f32 / 100.0),
            Op::Star { id, on } => h.star(id, on),
            // The core keeps transfers, pages and jam ops to itself.
            Op::Transfer { .. } | Op::Page { .. } | Op::Request { .. } | Op::Decide { .. } | Op::Promote { .. } | Op::Kick { .. } => {}
        }
        h.remotes.played(&h.engine);
    }
}

/// Tells the client to read the devices or the jam again. Once another device plays, this one's engine
/// stops and lets its output go.
pub(crate) struct Shown {
    pub(crate) out: Out,
    pub(crate) remotes: Arc<Remotes>,
    pub(crate) engine: Arc<Engine>,
}

impl RemoteShown for Shown {
    fn changed(&self) {
        let elsewhere = self.remotes.get().is_some_and(|r| r.active().is_some());
        if self.remotes.mirroring(elsewhere) {
            self.engine.release_now();
        }
        (self.out)(Said::Remote);
    }
}

/// What a client's player asks of the music.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Press {
    Toggle,
    Next,
    Previous,
    Seek(i64),
    /// The song at this list index (a queue row's).
    Jump(u32),
    Shuffle(bool),
    /// Off 0, one 1, all 2.
    Repeat(u8),
    /// 0 to 1.
    Volume(f32),
}

/// The account's active device while it is another one, as read at one moment: what the player shows
/// instead of this device's own playback, and where its controls go.
#[derive(Clone)]
pub struct Elsewhere {
    pub mirror: Mirror,
    /// When `mirror` was read: its place runs on from then.
    read: Instant,
    remote: Arc<Remote>,
}

impl Elsewhere {
    /// The song playing there.
    pub fn song(&self) -> Option<&Song> {
        self.row().map(|r| &r.song)
    }

    fn row(&self) -> Option<&MirrorRow> {
        self.mirror.at.and_then(|a| self.mirror.rows.get(a as usize))
    }

    /// Where the song is now: the device's word run on from when it was read, held within the song.
    pub fn position_ms(&self) -> i64 {
        self.position_after(self.read.elapsed().as_millis() as i64)
    }

    fn position_after(&self, elapsed_ms: i64) -> i64 {
        let m = &self.mirror;
        let ran = if m.playing { m.position_ms + elapsed_ms } else { m.position_ms };
        let end = self.song().map_or(0, |s| s.duration as i64 * 1000);
        if end > 0 { ran.clamp(0, end) } else { ran.max(0) }
    }

    /// The songs after the one playing, in play order.
    pub fn upcoming(&self) -> &[MirrorRow] {
        let from = self.mirror.at.map_or(0, |a| a as usize + 1);
        self.mirror.rows.get(from..).unwrap_or_default()
    }

    /// The device's volume, 0 to 1, when it can be set.
    pub fn volume(&self) -> Option<f32> {
        self.mirror.volume.map(|v| v as f32 / 100.0)
    }

    /// `press` as a command for the device; None when there is nothing to ask.
    fn op(&self, press: Press) -> Option<Op> {
        let m = &self.mirror;
        Some(match press {
            Press::Toggle if m.playing => Op::Pause,
            Press::Toggle => Op::Play,
            Press::Next => Op::Next,
            Press::Previous => Op::Previous,
            Press::Seek(ms) => Op::Seek { ms: ms.max(0) },
            Press::Jump(index) => Op::Jump { index, rev: m.rev },
            Press::Shuffle(on) => Op::Shuffle { on },
            Press::Repeat(mode) => Op::Repeat { mode },
            Press::Volume(v) => {
                let percent = (v.clamp(0.0, 1.0) * 100.0).round() as u8;
                m.volume.filter(|&now| now != percent)?;
                Op::Volume { percent }
            }
        })
    }

    /// Asks the device for `press`; it shows at once, as it is expected to come out.
    pub(crate) fn press(&self, press: Press) {
        if let Some(op) = self.op(press) {
            self.send(op);
        }
    }

    /// `songs` from `start` (or shuffled) as the device's queue, playing, its repeat kept.
    fn replace(&self, songs: Vec<Song>, start: usize, shuffle: bool) -> Op {
        Op::Replace { songs, index: start as u32, position_ms: 0, play: true, order: None, shuffle, repeat: self.mirror.repeat }
    }

    pub(crate) fn play(&self, songs: Vec<Song>, start: usize, shuffle: bool) {
        self.send(self.replace(songs, start, shuffle));
    }

    pub(crate) fn send(&self, op: Op) {
        self.remote.send(self.mirror.id.clone(), op);
    }
}

/// The media keys and the system's media controls: the active device while it is another one, else this
/// one's engine.
#[cfg(feature = "desktop")]
pub(crate) struct Keys {
    pub(crate) here: crate::Controls,
    pub(crate) remotes: Arc<Remotes>,
}

#[cfg(feature = "desktop")]
impl Keys {
    fn on(&self, press: Press, here: impl FnOnce(&crate::Controls)) {
        match self.remotes.elsewhere() {
            Some(e) => e.press(press),
            None => here(&self.here),
        }
    }
}

#[cfg(feature = "desktop")]
impl nori_mpris::Controls for Keys {
    fn play(&self) {
        match self.remotes.elsewhere() {
            Some(e) => e.send(Op::Play),
            None => self.here.play(),
        }
    }
    fn pause(&self) {
        match self.remotes.elsewhere() {
            Some(e) => e.send(Op::Pause),
            None => self.here.pause(),
        }
    }
    fn toggle(&self) {
        self.on(Press::Toggle, |c| c.toggle());
    }
    fn next(&self) {
        self.on(Press::Next, |c| c.next());
    }
    fn previous(&self) {
        self.on(Press::Previous, |c| c.previous());
    }
    fn seek(&self, ms: i64) {
        self.on(Press::Seek(ms), |c| c.seek(ms));
    }
    fn now(&self) -> nori_mpris::Now {
        let Some(e) = self.remotes.elsewhere() else { return self.here.now() };
        let row = e.row();
        let song = row.map(|r| r.song.clone()).unwrap_or_default();
        nori_mpris::Now {
            playing: e.mirror.playing,
            loaded: row.is_some() && !e.mirror.playing,
            index: row.map(|r| r.index as usize),
            length_ms: song.duration as i64 * 1000,
            position_ms: e.position_ms(),
            title: song.title,
            artist: song.artist,
            album: song.album,
        }
    }
}

/// mDNS for nearby doors, over mdns-sd's own thread; made with the remote, so nothing runs while it is off.
#[cfg(feature = "desktop")]
pub(crate) struct Mdns {
    daemon: mdns_sd::ServiceDaemon,
    remote: std::sync::OnceLock<Weak<Remote>>,
    announced: Mutex<Option<String>>,
}

#[cfg(feature = "desktop")]
const SERVICE_TYPE: &str = "_nori._tcp.local.";

#[cfg(feature = "desktop")]
impl Mdns {
    pub(crate) fn start() -> Option<Arc<Mdns>> {
        let daemon = mdns_sd::ServiceDaemon::new().ok()?;
        Some(Arc::new(Mdns { daemon, remote: Default::default(), announced: Mutex::new(None) }))
    }

    pub(crate) fn serve(&self, remote: &Arc<Remote>) {
        let _ = self.remote.set(Arc::downgrade(remote));
    }
}

#[cfg(feature = "desktop")]
impl Drop for Mdns {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

#[cfg(feature = "desktop")]
impl Discovery for Mdns {
    fn announce(&self, door: Option<Announcement>) {
        if let Some(old) = self.announced.lock().take() {
            let _ = self.daemon.unregister(&old);
        }
        let Some(door) = door else { return };
        let host = format!("{}.local.", door.name);
        let txt: std::collections::HashMap<String, String> = door.txt.into_iter().map(|p| (p.key, p.value)).collect();
        let Ok(info) = mdns_sd::ServiceInfo::new(SERVICE_TYPE, &door.name, &host, "", door.port, txt) else { return };
        let info = info.enable_addr_auto();
        let name = info.get_fullname().to_string();
        if self.daemon.register(info).is_ok() {
            *self.announced.lock() = Some(name);
        }
    }

    fn browse(&self, on: bool) {
        if !on {
            let _ = self.daemon.stop_browse(SERVICE_TYPE);
            return;
        }
        let (Ok(events), Some(remote)) = (self.daemon.browse(SERVICE_TYPE), self.remote.get().cloned()) else { return };
        crate::spawn("nori-mdns", move || {
            while let Ok(e) = events.recv() {
                let Some(r) = remote.upgrade() else { return };
                match e {
                    mdns_sd::ServiceEvent::ServiceResolved(s) => {
                        let Some(ip) = s.addresses.iter().map(|a| a.to_ip_addr()).find(|a| a.is_ipv4()).or_else(|| s.addresses.iter().next().map(|a| a.to_ip_addr())) else { continue };
                        let txt = s.txt_properties.iter().map(|p| nori_core::Param { key: p.key().to_string(), value: p.val_str().to_string() }).collect();
                        r.lan_found(s.fullname.clone(), ip.to_string(), s.port, txt);
                    }
                    mdns_sd::ServiceEvent::ServiceRemoved(_, name) => r.lan_lost(name),
                    mdns_sd::ServiceEvent::SearchStopped(_) => return,
                    _ => {}
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_devices_see_and_set_the_volume() {
        let card = Arc::new(Mutex::new(0.0f32));
        let to_card = card.clone();
        let remotes = Remotes::new(Level::new(0.4, Some(Box::new(move |v| *to_card.lock() = v))));
        let status = Status { state: State::Playing, ..Default::default() };
        assert_eq!(remotes.playing(&status).volume, Some(40));
        assert!(remotes.level.set(0.25), "loudness moves with it");
        assert_eq!((*card.lock(), remotes.playing(&status).volume), (0.25, Some(25)));
        remotes.buffering(true);
        assert!(remotes.playing(&status).buffering);

        // A volume the system keeps is followed, not offered.
        let system = Remotes::new(Level::new(0.4, None));
        assert_eq!(system.playing(&status).volume, None);
    }

    struct Nothing;

    impl RemotePlayer for Nothing {
        fn apply(&self, _: Op) {}
    }

    impl RemoteShown for Nothing {
        fn changed(&self) {}
    }

    /// The desk mirrored playing the second of three songs (200 s each) 10 s in, at volume 40.
    fn desk() -> Elsewhere {
        let core = nori_core::Core::new(String::new(), "t".into(), Default::default()).unwrap();
        let client = nori_core::client::Client::new(core, Arc::new(crate::session::Offline), Default::default());
        let me = nori_core::remote::RemoteMe { name: "Mac".into(), kind: nori_remote::wire::DeviceKind::Desktop };
        let remote = Remote::new(client, me, Arc::new(Nothing), Arc::new(Nothing), None);
        let row = |index: u32, id: &str| MirrorRow { index, song: Song { id: id.into(), duration: 200, ..Default::default() } };
        let mirror = Mirror {
            id: "desk".into(),
            name: "Desk".into(),
            kind: nori_remote::wire::DeviceKind::Desktop,
            rows: vec![row(2, "a"), row(0, "b"), row(1, "c")],
            at: Some(1),
            len: 3,
            rev: 7,
            playing: true,
            buffering: false,
            position_ms: 10_000,
            shuffle: true,
            repeat: 0,
            volume: Some(40),
            refused: None,
        };
        Elsewhere { mirror, read: Instant::now(), remote }
    }

    #[test]
    fn a_device_elsewhere_runs_on_within_its_song() {
        let mut e = desk();
        assert_eq!(e.song().map(|s| s.id.as_str()), Some("b"));
        assert_eq!(e.upcoming().iter().map(|r| r.index).collect::<Vec<_>>(), [1], "after the song playing, in play order");
        assert_eq!((e.position_after(2_500), e.position_after(500_000)), (12_500, 200_000), "held at the song's end");
        e.mirror.playing = false;
        assert_eq!(e.position_after(2_500), 10_000, "paused");
        e.mirror.at = None;
        assert_eq!(e.upcoming().len(), 3, "nothing playing: the whole queue is to come");
        assert_eq!(e.volume(), Some(0.4));
    }

    #[test]
    fn the_players_controls_become_the_devices_commands() {
        let mut e = desk();
        let pressed = [
            (Press::Toggle, Some(Op::Pause)),
            (Press::Seek(-40), Some(Op::Seek { ms: 0 })),
            (Press::Jump(1), Some(Op::Jump { index: 1, rev: 7 })),
            (Press::Repeat(2), Some(Op::Repeat { mode: 2 })),
            (Press::Volume(0.254), Some(Op::Volume { percent: 25 })),
            (Press::Volume(0.4), None),
        ];
        for (press, op) in pressed {
            assert_eq!(e.op(press), op, "{press:?}");
        }
        e.mirror.playing = false;
        assert_eq!(e.op(Press::Toggle), Some(Op::Play));
        e.mirror.volume = None;
        assert_eq!(e.op(Press::Volume(0.9)), None, "a volume the device does not offer");
        let songs = vec![Song { id: "x".into(), ..Default::default() }];
        assert!(matches!(e.replace(songs, 0, false), Op::Replace { play: true, position_ms: 0, shuffle: false, repeat: 0, .. }));
    }

    #[test]
    fn the_engine_lets_go_once_each_time_another_device_starts_playing() {
        let remotes = Remotes::new(Level::new(1.0, None));
        let seen: Vec<bool> = [false, true, true, false, true].into_iter().map(|on| remotes.mirroring(on)).collect();
        assert_eq!(seen, [false, true, false, false, true]);
    }
}
