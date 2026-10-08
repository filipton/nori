//! Remote control on the terminal and desktop clients: the session's player as the core's
//! [`RemotePlayer`], and mDNS through mdns-sd for nearby devices (`desktop` feature). A [`Remote`] is
//! made only while remote control or jams are switched on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(feature = "desktop")]
use std::sync::Weak;

use nori_core::remote::{Playing, Remote, RemotePlayer, RemoteShown};
#[cfg(feature = "desktop")]
use nori_core::remote::{Announcement, Discovery};
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
}

impl Remotes {
    pub(crate) fn new(level: Arc<Level>) -> Remotes {
        Remotes { slot: Mutex::default(), level, buffering: AtomicBool::new(false) }
    }

    pub(crate) fn buffering(&self, on: bool) {
        self.buffering.store(on, Ordering::Relaxed);
    }

    pub fn get(&self) -> Option<Arc<Remote>> {
        self.slot.lock().clone()
    }

    pub(crate) fn set(&self, remote: Option<Arc<Remote>>) {
        if let Some(old) = std::mem::replace(&mut *self.slot.lock(), remote) {
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

/// Tells the client to read the devices or the jam again.
pub(crate) struct Shown(pub(crate) Out);

impl RemoteShown for Shown {
    fn changed(&self) {
        (self.0)(Said::Remote);
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
}
