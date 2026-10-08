//! Remote control and jams over a client (nori-remote has the protocol). A [`Remote`] exists only while
//! remote control or jams are switched on, or the profile is a jam guest's; it polls the relay only
//! while it serves (this device is controllable), watches (a device picker or jam screen is open),
//! mirrors the account's active device or hosts a jam, and holds each poll up to [`HOLD_MS`], so a quiet
//! device wakes about once a minute. Nearby devices are reached through their LAN door instead of the relay.
//!
//! One device of the account plays at a time: the active one. This one is it until a transfer moves the
//! playback elsewhere ([`Remote::pick`], or another device's transfer); it then mirrors that device
//! ([`Remote::active`]) until playback comes back, a transfer moves it on, or the device goes. While it
//! mirrors, it keeps learning how the device's clock stands to its own (nori-remote's clock.rs): a burst
//! of time exchanges, then one every quarter minute, so the playhead shown is the one heard there.

use std::collections::HashMap;
use std::future::Future;
use std::ops::Range;
use std::sync::mpsc;
use std::sync::{Arc, Weak};
use std::task::{Poll, Waker};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use nori_remote::clock::{self, ClockSync};
use nori_remote::device::{admit, is_jam, Sender};
use nori_remote::jam::{By, Jam};
use nori_remote::lan::{self, Door};
use nori_remote::wire::{Answer, Body, DeviceKind, DeviceState, Entry, JamMember, Member, Op, Outgoing, Pending, Refusal, Role, Room, HOLD_MS};
use parking_lot::{Condvar, Mutex};
use serde::Deserialize;

use crate::client::Client;
use crate::transport::{self, block_on, Exchange, NetError, Transport};
use crate::{api, db, Param, Song};

/// The frames, for clients that speak them (the terminal and desktop host).
pub use nori_remote::wire;
/// The invite's QR code, for clients that draw it themselves.
pub use nori_remote::{qr_code, QrCode};

/// Songs before and after the current one a published state lists.
const ENTRIES_BEFORE: usize = 10;
const ENTRIES_AFTER: usize = 40;

/// Songs a controller asks for at a time beyond that window, and the most a device answers with.
const PAGE: u32 = 100;
const PAGE_MAX: u32 = 200;

/// Back this far into a song, a controller's previous restarts it rather than going to the song before
/// (as the player's own rule does, before the device's next state says what it did).
const PREVIOUS_RESTARTS_MS: i64 = 3_000;

/// After a failed poll, the relay is asked again this much later (or when something changes here).
const RETRY_MS: u64 = 30_000;

/// A held poll's own timeout: the hold plus the way there and back.
const POLL_TIMEOUT_MS: u32 = HOLD_MS + 15_000;

/// A published position this close to where the last one runs on to is not sent again.
const POSITION_SLACK_MS: i64 = 5;

/// Time exchanges of a burst go out this far apart: more than most round trips through a relay, so an
/// answer does not wait behind the one before.
const BURST_GAP_MS: u64 = 250;

/// What the platform's player does for the remote control.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait RemotePlayer: Send + Sync {
    /// Carries out an op the core admitted: never a jam op or a transfer.
    fn apply(&self, op: Op);
}

/// Told whenever the devices, their states, the jam or a refusal changed; the platform reads them again.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait RemoteShown: Send + Sync {
    fn changed(&self);
}

/// The platform's mDNS (NsdManager on Android, mdns-sd on the desktop).
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait Discovery: Send + Sync {
    /// Announces this device's door ([`nori_remote::lan::SERVICE`]), or withdraws it (None).
    fn announce(&self, door: Option<Announcement>);
    /// Looks for other doors while on, reporting them to [`Remote::lan_found`] and [`Remote::lan_lost`].
    fn browse(&self, on: bool);
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Announcement {
    /// The service instance name.
    pub name: String,
    pub port: u16,
    pub txt: Vec<Param>,
}

/// This device as others list it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RemoteMe {
    pub name: String,
    pub kind: DeviceKind,
}

/// The platform player's side of a published state; the queue's side is the core's.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Playing {
    pub playing: bool,
    /// Waiting for the song's bytes while it should play.
    pub buffering: bool,
    pub position_ms: i64,
    /// The list index heard: the queue's own current song moves only once the player says it arrived.
    pub index: Option<u32>,
    /// The media volume, 0 to 100, when it can be set.
    pub volume: Option<u8>,
}

/// Another device of the account, as a picker lists it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RemoteDevice {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub state: Option<DeviceState>,
    /// How long ago `state` arrived (`nori_remote::position_now`).
    pub age_ms: i64,
    /// Reached on this network rather than through the relay.
    pub nearby: bool,
    /// Its answer to the last command sent to it, when it refused.
    pub refused: Option<Refusal>,
}

/// The account's active device while it is another one: what this device mirrors in its own player.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Mirror {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    /// The queue in play order, as far as it is known here: the whole of it once its pages are in.
    pub rows: Vec<MirrorRow>,
    /// The row playing.
    pub at: Option<u32>,
    /// Songs in the device's queue.
    pub len: u32,
    /// The queue's revision, for [`Op::Jump`], [`Op::Remove`] and [`Op::Move`].
    pub rev: u64,
    pub playing: bool,
    pub buffering: bool,
    /// Where the song was at `at_us`; it runs on at one times from there while `playing`.
    pub position_ms: i64,
    /// When the device's listener heard `position_ms`, on this device's clock (`nori_remote::clock::now_us`,
    /// Android's `SystemClock.elapsedRealtimeNanos` / 1000).
    pub at_us: i64,
    pub shuffle: bool,
    pub repeat: u8,
    /// The device's volume, 0 to 100, when it can be set.
    pub volume: Option<u8>,
    /// Its answer to the last command sent to it, when it refused.
    pub refused: Option<Refusal>,
}

impl Mirror {
    /// Where the song is at `now_us` on this device's clock, within the song.
    pub fn position_at(&self, now_us: i64) -> i64 {
        if !self.playing {
            return self.position_ms;
        }
        let length = self.at.and_then(|a| self.rows.get(a as usize)).filter(|r| r.song.duration > 0).map_or(i64::MAX, |r| r.song.duration as i64 * 1000);
        (self.position_ms + (now_us - self.at_us) / 1000).clamp(0, length)
    }

    /// Where the song is now: what the device's listener hears at this moment.
    pub fn position_now(&self) -> i64 {
        self.position_at(clock::now_us())
    }
}

/// A song of a mirrored queue, and the list index commands name it by.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MirrorRow {
    pub index: u32,
    pub song: Song,
}

/// A jam this device hosts or is a guest in.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct JamView {
    pub hosting: bool,
    /// The invite link (and QR code) while hosting.
    pub link: Option<String>,
    /// This device's member id.
    pub you: String,
    pub members: Vec<JamMember>,
    pub pending: Vec<Pending>,
    /// The host's playback and queue.
    pub queue: Option<DeviceState>,
    pub age_ms: i64,
    /// The host's answer to this guest's last request, when it refused.
    pub refused: Option<Refusal>,
}

/// What a guest's profile signs in with.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct JamPass {
    pub url: String,
    pub api_key: String,
}

/// Where a command came from, and so how to answer it.
#[derive(Debug, Clone)]
enum Via {
    /// The relay; the room is a jam's (None: the account's).
    Relay(Option<String>),
    /// This device's door.
    Door,
    /// The door of a nearby device this one watches.
    Peer(String),
}

/// Where a send goes: the relay, or a nearby door's address.
#[derive(Debug, Clone)]
enum Link {
    Relay,
    Lan(String),
}

enum Out {
    Send(Link, Box<Outgoing>),
    /// A GET to the relay (leaving, closing a jam, sending a member out).
    Get(String),
}

impl Out {
    fn send(link: Link, outgoing: Outgoing) -> Out {
        Out::Send(link, Box::new(outgoing))
    }
}

/// Whether the server relays remote control and jams (octo-fiesta's `noriRemote.*`), as asked once when
/// the remote is made for a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum RelaySupport {
    /// Not known yet (not asked, or the server was unreachable).
    #[default]
    Unknown,
    Supported,
    /// Plain Navidrome, or an octo-fiesta without the hub: nearby devices only, no jams. Never asked again.
    Unsupported,
}

/// What a relay request's answer says of the relay; None when the network, not the server, answered.
fn support(got: &Result<Vec<u8>, NetError>) -> Option<RelaySupport> {
    match got {
        Ok(body) => Some(if answer(body).is_some() { RelaySupport::Supported } else { RelaySupport::Unsupported }),
        Err(NetError::Http { status: 404 | 405 } | NetError::Api { .. } | NetError::Parse { .. }) => Some(RelaySupport::Unsupported),
        Err(_) => None,
    }
}

struct Peer {
    service: String,
    /// Where its door was seen (mDNS tells each of the device's addresses), the one answering first.
    bases: Vec<String>,
    /// Polls failed in a row, each at the next address.
    failed: usize,
    member: Member,
    /// When its state last changed, on this device's clock.
    received: i64,
    since: Option<u64>,
    polling: bool,
}

struct Hosted {
    jam: Jam,
    link: String,
}

/// What this device knows of the active device it mirrors.
struct Mirrored {
    id: String,
    /// Its state as it last arrived.
    heard: Option<DeviceState>,
    /// That state with this device's own commands since then foreseen in it.
    shown: Option<DeviceState>,
    /// When `shown`'s position was right, on this device's clock: as it arrived, or was foreseen here.
    at: i64,
    /// The same moment on the device's clock, as it said (None: foreseen here, or an older device).
    device_at: Option<i64>,
    /// How the device's clock stands to this one's.
    clock: ClockSync,
    /// The queue by turn, at `shown`'s revision, as its pages arrive.
    pages: Vec<Option<Entry>>,
    /// The turn the page asked for and not answered yet starts at.
    asking: Option<u32>,
}

impl Mirrored {
    fn new(id: String) -> Mirrored {
        Mirrored { id, heard: None, shown: None, at: clock::now_us(), device_at: None, clock: ClockSync::default(), pages: Vec::new(), asking: None }
    }

    /// A state arrived from the device (`at`: when, on this device's clock). False when it is the one
    /// already heard.
    fn heard(&mut self, state: &DeviceState, at: i64) -> bool {
        if self.heard.as_ref() == Some(state) {
            return false;
        }
        let rev = self.shown.as_ref().map(|s| (s.rev, s.len));
        if rev != Some((state.rev, state.len)) {
            self.pages = vec![None; state.len as usize];
            self.asking = None;
        }
        for e in &state.entries {
            if let Some(slot @ None) = self.pages.get_mut(e.turn as usize) {
                *slot = Some(e.clone());
            }
        }
        self.heard = Some(state.clone());
        self.shown = Some(state.clone());
        self.at = at;
        self.device_at = state.at_us;
        true
    }

    /// When `shown`'s position was right, on this device's clock: the device's own word, once its clock
    /// is known here.
    fn shown_at(&self) -> i64 {
        match (self.device_at, self.clock.offset_at(self.at)) {
            (Some(there), Some(offset)) => there - offset,
            _ => self.at,
        }
    }

    /// A page of the queue arrived.
    fn page(&mut self, rev: u64, from: u32, entries: Vec<Entry>) {
        if self.shown.as_ref().map(|s| s.rev) != Some(rev) {
            return;
        }
        self.asking = None;
        for (k, e) in entries.into_iter().enumerate() {
            if let Some(slot) = self.pages.get_mut(from as usize + k) {
                *slot = Some(e);
            }
        }
    }

    /// The page to ask for next: where the first song not known yet is, unless one is being asked for.
    fn wanted(&self) -> Option<u32> {
        if self.asking.is_some() {
            return None;
        }
        self.pages.iter().position(Option::is_none).map(|p| p as u32)
    }

    /// The queue in play order: every page once all are in, the published window (newer) over them.
    fn rows(&self) -> Vec<Entry> {
        let Some(st) = &self.shown else { return Vec::new() };
        if self.pages.is_empty() || self.pages.iter().any(Option::is_none) {
            return st.entries.clone();
        }
        let mut all: Vec<Entry> = self.pages.iter().flatten().cloned().collect();
        for e in &st.entries {
            if let Some(slot) = all.get_mut(e.turn as usize).filter(|slot| slot.index == e.index) {
                *slot = e.clone();
            }
        }
        all
    }

    /// `op`, sent to the device, as this device expects it to come out; the device's next state says
    /// what it really did.
    fn foresee(&mut self, op: &Op) {
        let rows = self.rows();
        let at = self.shown_at();
        let Some(st) = &mut self.shown else { return };
        let here = clock::now_us();
        let now = nori_remote::position_now(st, (here - at) / 1000);
        let at = rows.iter().position(|e| Some(e.index) == st.index);
        let to = |k: Option<usize>| k.and_then(|k| rows.get(k)).map(|e| e.index);
        let (index, position) = match *op {
            Op::Play => {
                st.playing = true;
                (st.index, now)
            }
            Op::Pause => {
                st.playing = false;
                st.buffering = false;
                (st.index, now)
            }
            Op::Seek { ms } => (st.index, ms),
            Op::Next => match to(at.map(|a| a + 1)).or_else(|| (st.repeat != 0).then(|| to(Some(0))).flatten()) {
                Some(i) => (Some(i), 0),
                None => return,
            },
            Op::Previous if now > PREVIOUS_RESTARTS_MS => (st.index, 0),
            Op::Previous => (to(at.and_then(|a| a.checked_sub(1))).or(st.index), 0),
            Op::Jump { index, .. } => (Some(index), 0),
            Op::Shuffle { on } => {
                st.shuffle = on;
                return;
            }
            Op::Repeat { mode } => {
                st.repeat = mode;
                return;
            }
            Op::Volume { percent } => {
                st.volume = Some(percent);
                return;
            }
            Op::Star { ref id, on } => {
                for e in st.entries.iter_mut().chain(self.pages.iter_mut().flatten()).filter(|e| e.id == *id) {
                    e.starred = on;
                }
                return;
            }
            _ => return,
        };
        st.index = index;
        st.position_ms = position;
        self.at = here;
        self.device_at = None;
    }

    fn view(&self, name: String, kind: DeviceKind, refused: Option<Refusal>) -> Option<Mirror> {
        let st = self.shown.as_ref()?;
        let rows = self.rows();
        Some(Mirror {
            id: self.id.clone(),
            name,
            kind,
            at: rows.iter().position(|e| Some(e.index) == st.index).map(|p| p as u32),
            rows: rows.iter().map(|e| MirrorRow { index: e.index, song: e.song() }).collect(),
            len: st.len,
            rev: st.rev,
            playing: st.playing,
            buffering: st.buffering,
            position_ms: st.position_ms,
            at_us: self.shown_at(),
            shuffle: st.shuffle,
            repeat: st.repeat,
            volume: st.volume,
            refused,
        })
    }
}

#[derive(Default)]
struct Inner {
    serving: bool,
    watching: bool,
    relay: RelaySupport,
    /// Bumped to end the running relay poller (each runs while its generation is current).
    generation: u64,
    relay_polling: bool,
    /// Bumped to end the nearby doors' pollers.
    lan_generation: u64,
    since: Option<u64>,
    you: String,
    rooms: Vec<Room>,
    /// When each other member's state last changed here, on this device's clock.
    received: HashMap<String, i64>,
    peers: Vec<Peer>,
    hosted: Option<Hosted>,
    playing: Playing,
    /// When `playing` was said, on this device's clock: its position runs on from then.
    playing_at: Option<i64>,
    /// The device this one last handed its playback to, until it plays again.
    handed_to: Option<String>,
    /// The account's active device, while it is another one.
    mirror: Option<Mirrored>,
    /// Bumped to end the running time keeper (each runs while its generation is current).
    timing: u64,
    published: Option<(DeviceState, Instant)>,
    /// The last published state's [`DeviceState::seq`].
    seq: u64,
    next_id: u64,
    refused: HashMap<String, Refusal>,
    door: Option<Door>,
    /// [`Remote::stop`] ran: nothing starts again.
    stopped: bool,
    /// The threads waiting for a request's answer, woken to give it up when the remote stops.
    waiting: HashMap<ThreadId, Waker>,
}

impl Peer {
    fn base(&self) -> String {
        self.bases[0].clone()
    }
}

impl Inner {
    fn wants_relay(&self) -> bool {
        self.relay != RelaySupport::Unsupported && (self.serving || self.watching || self.mirror.is_some() || self.hosted.is_some())
    }

    /// Whether the door of nearby device `id` is followed: every one while a picker is open, else only
    /// the one mirrored.
    fn follows_peer(&self, id: &str) -> bool {
        self.watching || self.mirror.as_ref().is_some_and(|m| m.id == id)
    }

    /// Device `id`'s last state and when it arrived; a nearby device's door says it first.
    fn state_of(&self, id: &str) -> Option<(&DeviceState, i64)> {
        if let Some(p) = self.peers.iter().find(|p| p.member.id == id) {
            return p.member.state.as_ref().map(|s| (s, p.received));
        }
        let m = self.rooms.iter().filter(|r| !r.jam).flat_map(|r| &r.members).find(|m| m.id == id)?;
        m.state.as_ref().map(|s| (s, self.received.get(id).copied().unwrap_or_else(clock::now_us)))
    }

    /// Whether device `id` is still there to be followed (it serves, nearby or through the relay).
    fn listed(&self, id: &str) -> bool {
        self.peers.iter().any(|p| p.member.id == id) || self.rooms.iter().filter(|r| !r.jam).flat_map(|r| &r.members).any(|m| m.id == id)
    }

    /// Device `id`'s name and kind, as it is listed.
    fn who(&self, id: &str) -> Option<(String, DeviceKind)> {
        let m = self.peers.iter().map(|p| &p.member).chain(self.rooms.iter().filter(|r| !r.jam).flat_map(|r| &r.members)).find(|m| m.id == id)?;
        Some((m.name.clone(), m.kind))
    }

    fn jam_room(&self) -> Option<&str> {
        self.hosted.as_ref().map(|h| h.jam.room.as_str())
    }

    /// The jam this device is a guest in, and its host.
    fn joined(&self) -> Option<(&Room, Option<&Member>)> {
        let room = self.rooms.iter().find(|r| r.jam && Some(r.room.as_str()) != self.jam_room())?;
        Some((room, room.members.iter().find(|m| m.state.as_ref().is_some_and(|s| s.jam.is_some()))))
    }

    fn age(&self, id: &str) -> i64 {
        self.received.get(id).map_or(0, |t| (clock::now_us() - t) / 1000)
    }

    fn since_played_ms(&self, now_us: i64) -> i64 {
        self.playing_at.map_or(0, |t| (now_us - t) / 1000)
    }

    /// Where this device's song is now, run on from when the platform last said.
    fn position_now(&self) -> i64 {
        let elapsed = self.since_played_ms(clock::now_us());
        if self.playing.playing { self.playing.position_ms + elapsed } else { self.playing.position_ms }
    }

    /// Where this device's song is at `now_us`, held to the end of `state`'s song.
    fn position_at_of(&self, state: &DeviceState, now_us: i64) -> i64 {
        nori_remote::position_now(&DeviceState { position_ms: self.playing.position_ms, ..state.clone() }, self.since_played_ms(now_us))
    }
}

/// Remote control and jams for one client. See the module.
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct Remote {
    client: Arc<Client>,
    id: String,
    me: RemoteMe,
    player: Arc<dyn RemotePlayer>,
    shown: Arc<dyn RemoteShown>,
    discovery: Option<Arc<dyn Discovery>>,
    inner: Mutex<Inner>,
    /// Wakes a relay poller waiting to try again.
    retry: Condvar,
    /// Wakes the time keeper when the device mirrored changes.
    timing: Condvar,
    /// None once stopped: the sender ends with what was queued.
    out: Mutex<Option<mpsc::Sender<Out>>>,
    /// The pollers, the time keeper and the probe, joined when the remote stops.
    threads: Mutex<Vec<JoinHandle<()>>>,
}

/// The app's key for this device's id.
const DEVICE_ID: &str = "remoteDeviceId";

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Remote {
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new(client: Arc<Client>, me: RemoteMe, player: Arc<dyn RemotePlayer>, shown: Arc<dyn RemoteShown>, discovery: Option<Arc<dyn Discovery>>) -> Arc<Remote> {
        let settings = &client.core.session.settings;
        let id = settings.app_value(DEVICE_ID).filter(|v| !v.is_empty()).unwrap_or_else(|| {
            let id = nori_remote::new_id();
            settings.keep_app_value(DEVICE_ID, id.clone());
            id
        });
        let (out, rx) = mpsc::channel();
        let remote = Arc::new(Remote { client, id, me, player, shown, discovery, inner: Mutex::default(), retry: Condvar::new(), timing: Condvar::new(), out: Mutex::new(Some(out)), threads: Mutex::default() });
        let (client, who) = (remote.client.clone(), remote.who());
        // Sends one at a time, in order; it ends once the remote stops or goes.
        let _ = std::thread::Builder::new().name("nori-remote-out".into()).spawn(move || {
            for o in rx {
                deliver(&client, &who, o);
            }
        });
        // Whether the server relays, asked once: one poll that is not held.
        let me = remote.clone();
        remote.spawn("nori-remote-probe", move || {
            let Some(got) = me.get(me.poll_url(None, false), 0) else { return };
            let serving = {
                let mut i = me.inner.lock();
                if let Some(found) = support(&got) {
                    if i.relay == RelaySupport::Unknown {
                        i.relay = found;
                    }
                }
                // The probe says this device does not serve, and may have reached the relay after its
                // first state did: serving by now, it says its state again.
                if i.serving {
                    i.published = None;
                }
                i.serving
            };
            if serving {
                me.publish();
            }
            me.keep_polling();
        });
        remote
    }

    /// Whether the server relays remote control and jams; the screens offer jams and far devices only then.
    pub fn relay(&self) -> RelaySupport {
        self.inner.lock().relay
    }

    /// This device's id, as other devices and the relay know it.
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// Makes this device controllable (its playback service is up) or not.
    pub fn serve(self: Arc<Self>, on: bool) {
        let announce = {
            let mut i = self.inner.lock();
            if i.serving == on {
                return;
            }
            i.serving = on;
            i.published = None;
            i.door = None;
            if on {
                i.door = self.open_door();
            }
            i.door.as_ref().map(|d| self.announcement(d.port()))
        };
        if let Some(d) = &self.discovery {
            d.announce(announce);
        }
        if on {
            self.publish();
        } else if self.relay() != RelaySupport::Unsupported {
            self.out(Out::Get(self.poll_url(None, false)));
        }
        self.keep_polling();
    }

    /// A device picker or jam screen is open: other devices' states are followed while on.
    pub fn watch(self: Arc<Self>, on: bool) {
        {
            let mut i = self.inner.lock();
            if i.watching == on {
                return;
            }
            i.watching = on;
            if !on {
                i.lan_generation += 1;
                i.peers.iter_mut().for_each(|p| p.polling = false);
            }
        }
        if let Some(d) = &self.discovery {
            d.browse(on);
        }
        self.keep_polling();
    }

    /// The platform's playback changed (play, pause, seek, another song, the queue, the volume). Music
    /// starting here makes this the active device again.
    pub fn played(self: Arc<Self>, playing: Playing) {
        let (publish, started) = {
            let mut i = self.inner.lock();
            let started = playing.playing && !i.playing.playing;
            if started {
                i.handed_to = None;
            }
            i.playing = playing;
            i.playing_at = Some(clock::now_us());
            (i.serving || i.hosted.is_some(), started && i.mirror.is_some())
        };
        if started {
            self.set_active(None);
        }
        if publish {
            self.publish();
        }
    }

    /// The device's volume moved by itself (its own keys), 0 to 100; None when it cannot be set.
    pub fn volume_changed(self: Arc<Self>, volume: Option<u8>) {
        let publish = {
            let mut i = self.inner.lock();
            if i.playing.volume == volume {
                return;
            }
            i.playing.volume = volume;
            i.serving || i.hosted.is_some()
        };
        if publish {
            self.publish();
        }
    }

    /// The account's active device while it is another one, as this device mirrors it; None while this
    /// one is the active device.
    pub fn active(&self) -> Option<Mirror> {
        let i = self.inner.lock();
        let m = i.mirror.as_ref()?;
        let (name, kind) = i.who(&m.id)?;
        m.view(name, kind, i.refused.get(&m.id).copied())
    }

    /// Moves the playback to `device`, or here (None): the active device hands its queue over and
    /// pauses. With nothing queued here, the device is only followed.
    pub fn pick(self: Arc<Self>, device: Option<String>) {
        let (active, idle) = {
            let i = self.inner.lock();
            let m = i.mirror.as_ref();
            (m.map(|m| m.id.clone()), m.and_then(|m| m.shown.as_ref()).is_some_and(|s| s.index.is_none()))
        };
        let here = self.client.core.session.playlist(|p| p.current().is_some());
        match (active, device) {
            (None, None) => {}
            (Some(_), None) if idle => self.set_active(None),
            // Followed until the queue arrives (its transfer replaces the queue here).
            (Some(a), None) => self.send(a, Op::Transfer { to: self.id.clone() }),
            (None, Some(d)) if here => self.hand_over(d),
            (None, Some(d)) => self.set_active(Some(d)),
            (Some(a), Some(d)) if a == d => {}
            (Some(a), Some(d)) => {
                self.send(a, Op::Transfer { to: d.clone() });
                self.set_active(Some(d));
            }
        }
    }

    /// The account's other devices, nearby ones first.
    pub fn devices(&self) -> Vec<RemoteDevice> {
        let i = self.inner.lock();
        let seen = |m: &Member, age_ms: i64, nearby: bool| RemoteDevice { id: m.id.clone(), name: m.name.clone(), kind: m.kind, state: m.state.clone(), age_ms, nearby, refused: i.refused.get(&m.id).copied() };
        let mut out: Vec<RemoteDevice> = i.peers.iter().filter(|p| p.member.state.is_some()).map(|p| seen(&p.member, (clock::now_us() - p.received) / 1000, true)).collect();
        for room in i.rooms.iter().filter(|r| !r.jam) {
            for m in room.members.iter().filter(|m| m.id != self.id && m.state.is_some()) {
                if !out.iter().any(|d| d.id == m.id) {
                    out.push(seen(m, i.age(&m.id), false));
                }
            }
        }
        out
    }

    /// Sends `op` to device `device`; its answer shows in [`Remote::devices`]. Sent to the mirrored
    /// device, it shows in [`Remote::active`] at once, as it is expected to come out.
    pub fn send(&self, device: String, op: Op) {
        let link = {
            let mut i = self.inner.lock();
            if let Some(m) = i.mirror.as_mut().filter(|m| m.id == device) {
                m.foresee(&op);
            }
            i.refused.remove(&device);
            let lan = i.peers.iter().find(|p| p.member.id == device).map(|p| Link::Lan(p.base()));
            match lan {
                Some(l) => l,
                None if i.relay == RelaySupport::Unsupported => return,
                None => Link::Relay,
            }
        };
        let id = self.next_id();
        self.out(Out::send(link, Outgoing { to: Some(device), body: Some(Body::Command { id, op: Box::new(op) }), ..Default::default() }));
        self.shown.changed();
    }

    /// Hands this device's queue and position to device `to`, which plays on; this one pauses.
    pub fn hand_over(self: Arc<Self>, to: String) {
        let me = self.id.clone();
        self.transfer(&Via::Relay(None), &me, to);
    }

    /// A door the platform's discovery found: service `service` at `host`:`port` with TXT `txt`.
    pub fn lan_found(self: Arc<Self>, service: String, host: String, port: u16, txt: Vec<Param>) {
        let get = |k: &str| txt.iter().find(|p| p.key == k).map(|p| p.value.clone()).unwrap_or_default();
        let Some(tag) = self.account_tag() else { return };
        let id = get("id");
        if get("acct") != tag || id.is_empty() || id == self.id {
            return;
        }
        let kind = serde_json::from_value(serde_json::Value::String(get("kind"))).unwrap_or_default();
        let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host };
        let base = format!("http://{host}:{port}");
        {
            let mut i = self.inner.lock();
            // Seen again, maybe at another of its addresses: its poller goes on, and tries that one too.
            if let Some(p) = i.peers.iter_mut().find(|p| p.service == service && p.member.id == id) {
                if !p.bases.contains(&base) {
                    p.bases.push(base);
                }
                return;
            }
            i.peers.retain(|p| p.service != service && p.member.id != id);
            let member = Member { id, name: get("name"), kind, state: None };
            i.peers.push(Peer { service, bases: vec![base], failed: 0, member, received: clock::now_us(), since: None, polling: false });
        }
        self.keep_polling();
    }

    /// A door went away.
    pub fn lan_lost(self: Arc<Self>, service: String) {
        self.inner.lock().peers.retain(|p| p.service != service);
        self.follow_active();
        self.shown.changed();
    }

    /// Starts hosting a jam; its invite link. Needs the relay.
    pub async fn jam_open(self: Arc<Self>) -> Result<String, NetError> {
        #[derive(Deserialize)]
        struct Opened {
            room: String,
            invite: String,
        }
        if self.relay() == RelaySupport::Unsupported {
            return Err(NetError::Http { status: 404 });
        }
        let url = self.relay_url("noriRemote.open", &[("dev", self.id.clone()), ("name", self.me.name.clone())]);
        let body = transport::get(&*self.client.transport, url, 0).await?;
        let opened: Opened = serde_json::from_slice(&body).map_err(|e| NetError::Parse { reason: e.to_string() })?;
        let server = self.client.profile.read().url.clone();
        let link = nori_remote::invite_link(&server, &opened.invite);
        {
            let mut i = self.inner.lock();
            i.relay = RelaySupport::Supported;
            i.hosted = Some(Hosted { jam: Jam::new(opened.room, opened.invite, self.id.clone(), self.me.name.clone()), link: link.clone() });
            i.published = None;
        }
        self.publish();
        self.keep_polling();
        Ok(link)
    }

    /// Ends the jam this device hosts.
    pub fn jam_close(self: Arc<Self>) {
        let Some(h) = self.inner.lock().hosted.take() else { return };
        self.out(Out::Get(self.relay_url("noriRemote.close", &[("room", h.jam.room)])));
        self.keep_polling();
        self.shown.changed();
    }

    /// A jam op from this device: the host's own (accept, decline, promote, send out, add straight in),
    /// or a guest's request or decision, sent to the host.
    pub fn jam_act(self: Arc<Self>, op: Op) {
        let host = {
            let mut i = self.inner.lock();
            if i.hosted.is_none() {
                let to = i.joined().and_then(|(room, host)| Some((room.room.clone(), host?.id.clone())));
                if let Some((_, host)) = &to {
                    i.refused.remove(host);
                }
                to
            } else {
                None
            }
        };
        match host {
            None => {
                let _ = self.carry_out(&Via::Relay(None), &self.id.clone(), op);
                self.shown.changed();
            }
            Some((room, host)) => {
                let id = self.next_id();
                self.out(Out::send(Link::Relay, Outgoing { room: Some(room), to: Some(host), body: Some(Body::Command { id, op: Box::new(op) }), state: None }));
            }
        }
    }

    /// The jam this device hosts or is a guest in.
    pub fn jam_view(&self) -> Option<JamView> {
        let i = self.inner.lock();
        if let Some(h) = &i.hosted {
            let st = h.jam.state();
            let queue = i.published.as_ref().map(|(s, _)| DeviceState { jam: None, ..s.clone() });
            let age_ms = i.published.as_ref().map_or(0, |(_, at)| at.elapsed().as_millis() as i64);
            return Some(JamView { hosting: true, link: Some(h.link.clone()), you: self.id.clone(), members: st.members, pending: st.pending, queue, age_ms, refused: None });
        }
        let (_, host) = i.joined()?;
        let state = host.and_then(|h| h.state.clone());
        let jam = state.as_ref().and_then(|s| s.jam.clone()).unwrap_or_default();
        Some(JamView {
            hosting: false,
            link: None,
            you: i.you.clone(),
            members: jam.members,
            pending: jam.pending,
            age_ms: host.map_or(0, |h| i.age(&h.id)),
            refused: host.and_then(|h| i.refused.get(&h.id).copied()),
            queue: state.map(|s| DeviceState { jam: None, ..s }),
        })
    }

    /// Ends remote control here for good: leaves the relay, withdraws the door, ends a hosted jam, stops
    /// mirroring, and ends every thread this remote started, a held poll at once. Queued sends still go.
    pub fn stop(self: Arc<Self>) {
        self.clone().serve(false);
        self.clone().watch(false);
        self.clone().jam_close();
        let waiting = {
            let mut i = self.inner.lock();
            i.stopped = true;
            i.mirror = None;
            i.generation += 1;
            i.relay_polling = false;
            i.lan_generation += 1;
            i.timing += 1;
            std::mem::take(&mut i.waiting)
        };
        self.retry.notify_all();
        self.timing.notify_all();
        waiting.into_values().for_each(Waker::wake);
        self.out.lock().take();
        let here = std::thread::current().id();
        for t in std::mem::take(&mut *self.threads.lock()) {
            if t.thread().id() != here {
                let _ = t.join();
            }
        }
        self.shown.changed();
    }

    /// Leaves the jam this guest profile is in; the app then drops the profile.
    pub async fn jam_leave(&self) -> Result<(), NetError> {
        transport::get(&*self.client.transport, self.relay_url("noriRemote.leave", &[]), 0).await.map(|_| ())
    }
}

/// Joins the jam `link` invites to as `name`; what the guest's profile signs in with.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub async fn jam_join(transport: Arc<dyn Transport>, link: String, name: String) -> Result<JamPass, NetError> {
    #[derive(Deserialize)]
    struct Joined {
        key: String,
    }
    let (server, invite) = nori_remote::parse_invite(&link).ok_or_else(|| NetError::Parse { reason: "not a jam invite".into() })?;
    let url = api::Server::with(&server, api::Auth::ApiKey(&nori_remote::guest_key(&invite))).url("noriRemote.join", &[("name".into(), name)]);
    let body = transport::get(&*transport, url, 0).await?;
    let joined: Joined = serde_json::from_slice(&body).map_err(|e| NetError::Parse { reason: e.to_string() })?;
    Ok(JamPass { url: server, api_key: nori_remote::guest_key(&joined.key) })
}

/// An answer from the relay; None when the server has no relay (a Subsonic error for an unknown endpoint).
fn answer(body: &[u8]) -> Option<Answer> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    if v.get("subsonic-response").is_some() {
        return None;
    }
    serde_json::from_value(v).ok()
}

/// Sends `o`; `who` is this device's [`Remote::who`].
fn deliver(client: &Client, who: &[(String, String)], o: Out) {
    let (url, json) = match o {
        Out::Get(url) => (url, None),
        Out::Send(link, outgoing) => {
            let json = serde_json::to_string(&outgoing).unwrap_or_default();
            let url = match link {
                Link::Relay => client.core.server.read().url("noriRemote.send", who),
                Link::Lan(base) => {
                    let Some((_, secret)) = client.core.account.read().clone() else { return };
                    let dev = &who[0].1;
                    format!("{base}{}", lan::signed(&secret, "POST", &format!("/rest/noriRemote.send?dev={dev}"), json.as_bytes(), db::now_ms()))
                }
            };
            (url, Some(json))
        }
    };
    let sent = block_on(client.transport.send(Exchange { url, json, timeout_ms: 15_000, ..Default::default() }));
    if let Err(e) = sent {
        crate::alog::info(&format!("remote: not sent: {e}"));
    }
}

impl Remote {
    /// The member who asked for song `id` in the jam this device hosts, for "added by" on a queue too
    /// long for the published window.
    pub fn jam_added_by(&self, id: &str) -> Option<String> {
        self.inner.lock().hosted.as_ref()?.jam.added_by(id)
    }

    fn out(&self, o: Out) {
        if let Some(out) = &*self.out.lock() {
            let _ = out.send(o);
        }
    }

    /// Starts a thread [`Remote::stop`] joins; none once stopped.
    fn spawn(&self, name: &str, run: impl FnOnce() + Send + 'static) {
        let mut threads = self.threads.lock();
        if self.inner.lock().stopped {
            return;
        }
        threads.retain(|t| !t.is_finished());
        if let Ok(t) = std::thread::Builder::new().name(name.into()).spawn(run) {
            threads.push(t);
        }
    }

    /// A GET through the client, on this thread; None when the remote stopped meanwhile (the request is
    /// cancelled).
    fn get(&self, url: String, timeout_ms: u32) -> Option<Result<Vec<u8>, NetError>> {
        let me = std::thread::current().id();
        let mut got = std::pin::pin!(transport::get(&*self.client.transport, url, timeout_ms));
        let out = block_on(std::future::poll_fn(|cx| {
            {
                let mut i = self.inner.lock();
                if i.stopped {
                    return Poll::Ready(None);
                }
                i.waiting.insert(me, cx.waker().clone());
            }
            got.as_mut().poll(cx).map(Some)
        }));
        self.inner.lock().waiting.remove(&me);
        out
    }

    fn next_id(&self) -> u64 {
        let mut i = self.inner.lock();
        i.next_id += 1;
        i.next_id
    }

    fn relay_url(&self, endpoint: &str, params: &[(&str, String)]) -> String {
        let p: Vec<(String, String)> = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.client.core.server.read().url(endpoint, &p)
    }

    /// This device's id, name and kind, as the relay is told them (the device id first).
    fn who(&self) -> Vec<(String, String)> {
        let kind = serde_json::to_value(self.me.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        vec![("dev".into(), self.id.clone()), ("name".into(), self.me.name.clone()), ("kind".into(), kind)]
    }

    fn poll_url(&self, since: Option<u64>, serve: bool) -> String {
        let mut p = self.who();
        p.push(("serve".into(), (serve as u8).to_string()));
        if let Some(s) = since {
            p.push(("since".into(), s.to_string()));
            p.push(("hold".into(), "1".into()));
        }
        self.client.core.server.read().url("noriRemote.poll", &p)
    }

    fn account_tag(&self) -> Option<String> {
        self.client.core.account.read().as_ref().map(|(user, secret)| lan::account_tag(user, secret))
    }

    fn announcement(&self, port: u16) -> Announcement {
        let mut txt = self.who();
        txt[0].0 = "id".into();
        txt.push(("acct".into(), self.account_tag().unwrap_or_default()));
        Announcement { name: format!("nori-{}", self.id), port, txt: txt.into_iter().map(|(key, value)| Param { key, value }).collect() }
    }

    fn open_door(self: &Arc<Self>) -> Option<Door> {
        let (_, secret) = self.client.core.account.read().clone()?;
        let me = Member { id: self.id.clone(), name: self.me.name.clone(), kind: self.me.kind, state: None };
        let weak: Weak<Remote> = Arc::downgrade(self);
        let opened = Door::open(
            me,
            secret,
            Box::new(move |from, body| {
                if let (Some(r), Body::Command { id, op }) = (weak.upgrade(), body) {
                    r.obey(Via::Door, from.to_string(), id, *op, clock::now_us());
                }
            }),
        );
        opened.map_err(|e| crate::alog::info(&format!("remote: no door: {e}"))).ok()
    }

    /// Starts the pollers that should run and are not.
    fn keep_polling(self: &Arc<Self>) {
        let mut start_relay = None;
        let mut start_peers = Vec::new();
        {
            let mut i = self.inner.lock();
            if i.wants_relay() {
                if !i.relay_polling {
                    i.relay_polling = true;
                    start_relay = Some(i.generation);
                }
            } else if i.relay_polling {
                i.generation += 1;
                i.relay_polling = false;
            }
            let generation = i.lan_generation;
            let follow: Vec<bool> = i.peers.iter().map(|p| !p.polling && i.follows_peer(&p.member.id)).collect();
            for (p, _) in i.peers.iter_mut().zip(follow).filter(|(_, f)| *f) {
                p.polling = true;
                start_peers.push((p.service.clone(), generation));
            }
        }
        self.retry.notify_all();
        if let Some(generation) = start_relay {
            let me = self.clone();
            self.spawn("nori-remote", move || me.poll_relay(generation));
        }
        for (service, generation) in start_peers {
            let me = self.clone();
            self.spawn("nori-remote-lan", move || me.poll_peer(service, generation));
        }
        self.shown.changed();
    }

    fn poll_relay(self: Arc<Self>, generation: u64) {
        loop {
            let url = {
                let i = self.inner.lock();
                if i.generation != generation || !i.wants_relay() {
                    return;
                }
                self.poll_url(i.since, i.serving)
            };
            let Some(got) = self.get(url, POLL_TIMEOUT_MS) else { return };
            let received = clock::now_us();
            match (support(&got), got) {
                (Some(RelaySupport::Supported), Ok(body)) => {
                    self.inner.lock().relay = RelaySupport::Supported;
                    self.took(answer(&body).unwrap_or_default(), received);
                }
                (Some(_), _) => return self.no_relay(generation),
                (None, _) => {
                    let mut i = self.inner.lock();
                    if i.generation == generation {
                        self.retry.wait_for(&mut i, Duration::from_millis(RETRY_MS));
                    }
                }
            }
        }
    }

    fn no_relay(&self, generation: u64) {
        let mut i = self.inner.lock();
        if i.generation == generation {
            i.relay = RelaySupport::Unsupported;
            i.relay_polling = false;
        }
        drop(i);
        self.shown.changed();
    }

    fn poll_peer(self: Arc<Self>, service: String, generation: u64) {
        loop {
            let (base, since) = {
                let mut i = self.inner.lock();
                let Some(at) = i.peers.iter().position(|p| p.service == service) else { return };
                if i.lan_generation != generation {
                    return;
                }
                if !i.follows_peer(&i.peers[at].member.id) {
                    i.peers[at].polling = false;
                    return;
                }
                (i.peers[at].base(), i.peers[at].since)
            };
            let Some((_, secret)) = self.client.core.account.read().clone() else { return };
            let query = match since {
                Some(s) => format!("/rest/noriRemote.poll?dev={}&since={s}&hold=1", self.id),
                None => format!("/rest/noriRemote.poll?dev={}", self.id),
            };
            let url = format!("{base}{}", lan::signed(&secret, "GET", &query, b"", db::now_ms()));
            let Some(got) = self.get(url, POLL_TIMEOUT_MS) else { return };
            let got = got.ok().and_then(|b| serde_json::from_slice::<Answer>(&b).ok());
            let received = clock::now_us();
            let Some(a) = got else {
                // Not there at this address: the next one it was seen at, until none answers.
                let gone = {
                    let mut i = self.inner.lock();
                    let Some(p) = i.peers.iter_mut().find(|p| p.service == service) else { return };
                    p.failed += 1;
                    p.bases.rotate_left(1);
                    p.failed >= p.bases.len()
                };
                if gone {
                    self.lan_lost(service);
                    return;
                }
                continue;
            };
            let mut commands = Vec::new();
            let (from, base) = {
                let mut i = self.inner.lock();
                let Some(p) = i.peers.iter_mut().find(|p| p.service == service) else { return };
                p.since = Some(a.seq);
                if let Some(m) = a.rooms.into_iter().flat_map(|r| r.members).next() {
                    if m.state != p.member.state {
                        p.received = received;
                    }
                    p.member.state = m.state;
                }
                p.failed = 0;
                let (from, base) = (p.member.id.clone(), p.base());
                for e in a.events {
                    match e.body {
                        Body::Command { id, op } => commands.push((id, *op)),
                        Body::Ack { refusal, .. } => note(&mut i.refused, &from, refusal),
                        Body::Page { rev, from: turn, entries, .. } => paged(&mut i, &from, rev, turn, entries),
                        Body::Clock { t1, t2, t3 } => timed(&mut i, &from, clock::Exchange { t1, t2, t3, t4: received }),
                    }
                }
                (from, base)
            };
            for (id, op) in commands {
                self.obey(Via::Peer(base.clone()), from.clone(), id, op, received);
            }
            self.follow_active();
            self.shown.changed();
        }
    }

    /// A relay poll's answer, received at `received` (this device's clock).
    fn took(self: &Arc<Self>, a: Answer, received: i64) {
        let mut commands = Vec::new();
        let mut republish = false;
        {
            let mut i = self.inner.lock();
            // Commands are heard from the first answer on: only then is this device's state said to the
            // relay, so no controller sends it one before it can hear it.
            if i.since.is_none() && i.serving {
                i.published = None;
                republish = true;
            }
            i.since = Some(a.seq);
            i.you = a.you;
            for m in a.rooms.iter().flat_map(|r| &r.members).filter(|m| m.id != self.id) {
                let before = i.rooms.iter().flat_map(|r| &r.members).find(|o| o.id == m.id).map(|o| &o.state);
                if before != Some(&m.state) {
                    i.received.insert(m.id.clone(), received);
                }
            }
            if let Some(h) = &mut i.hosted {
                if let Some(r) = a.rooms.iter().find(|r| r.room == h.jam.room) {
                    republish = h.jam.present(&r.members);
                }
            }
            for e in a.events {
                let jam = a.rooms.iter().any(|r| r.jam && r.room == e.room);
                match e.body {
                    Body::Command { id, op } => commands.push((Via::Relay(jam.then_some(e.room)), e.from, id, *op)),
                    Body::Ack { refusal, .. } => note(&mut i.refused, &e.from, refusal),
                    Body::Page { rev, from, entries, .. } => paged(&mut i, &e.from, rev, from, entries),
                    Body::Clock { t1, t2, t3 } => timed(&mut i, &e.from, clock::Exchange { t1, t2, t3, t4: received }),
                }
            }
            i.rooms = a.rooms;
        }
        for (via, from, id, op) in commands {
            self.obey(via, from, id, op, received);
        }
        if republish {
            self.publish();
        }
        self.follow_active();
        self.shown.changed();
    }

    /// Makes `to` the active device (None: this one), and follows it while it is another.
    fn set_active(self: &Arc<Self>, to: Option<String>) {
        let keeper = {
            let mut i = self.inner.lock();
            if i.mirror.as_ref().map(|m| &m.id) == to.as_ref() {
                return;
            }
            i.mirror = to.map(Mirrored::new);
            i.timing += 1;
            i.mirror.is_some().then_some(i.timing)
        };
        self.timing.notify_all();
        if let Some(generation) = keeper {
            let me = self.clone();
            self.spawn("nori-remote-clock", move || me.keep_time(generation));
        }
        self.follow_active();
        self.keep_polling();
    }

    /// While the device mirrored stays the same (`generation`), learns how its clock stands to this one's:
    /// a burst of time exchanges, then one every [`clock::EVERY_US`]. Through its door when it is near,
    /// else through the relay, whose answer comes with a poll.
    fn keep_time(self: Arc<Self>, generation: u64) {
        for sent in 0.. {
            let (id, link) = {
                let mut i = self.inner.lock();
                if sent > 0 {
                    let wait = if sent < clock::BURST { Duration::from_millis(BURST_GAP_MS) } else { Duration::from_micros(clock::EVERY_US as u64) };
                    let until = Instant::now() + wait;
                    while i.timing == generation && !self.timing.wait_until(&mut i, until).timed_out() {}
                }
                if i.timing != generation {
                    return;
                }
                let Some(id) = i.mirror.as_ref().map(|m| m.id.clone()) else { return };
                let link = match i.peers.iter().find(|p| p.member.id == id) {
                    Some(p) => Some(Link::Lan(p.base())),
                    None => (i.relay != RelaySupport::Unsupported).then_some(Link::Relay),
                };
                (id, link)
            };
            match link {
                Some(Link::Lan(base)) => {
                    let Some((_, secret)) = self.client.core.account.read().clone() else { return };
                    let t1 = clock::now_us();
                    let url = format!("{base}{}", lan::signed(&secret, "GET", &format!("/rest/noriRemote.time?dev={}&t1={t1}", self.id), b"", db::now_ms()));
                    let Some(got) = self.get(url, 5_000) else { return };
                    let t4 = clock::now_us();
                    if let Some(Body::Clock { t1, t2, t3 }) = got.ok().and_then(|b| serde_json::from_slice(&b).ok()) {
                        timed(&mut self.inner.lock(), &id, clock::Exchange { t1, t2, t3, t4 });
                        self.shown.changed();
                    }
                }
                Some(Link::Relay) => {
                    let command = Body::Command { id: self.next_id(), op: Box::new(Op::Clock { t1: clock::now_us() }) };
                    self.out(Out::send(Link::Relay, Outgoing { to: Some(id), body: Some(command), ..Default::default() }));
                }
                None => {}
            }
        }
    }

    /// Takes the mirrored device's newest state, follows it on to the device it handed its playback to,
    /// lets it go once it is gone, and asks for the next page of its queue.
    fn follow_active(self: &Arc<Self>) {
        let mut page = None;
        let next = {
            let mut i = self.inner.lock();
            let Some(id) = i.mirror.as_ref().map(|m| m.id.clone()) else { return };
            if !i.listed(&id) {
                i.mirror = None;
                i.timing += 1;
                drop(i);
                self.timing.notify_all();
                self.keep_polling();
                return;
            }
            let heard = i.state_of(&id).map(|(s, at)| (s.clone(), at));
            let me = self.id.clone();
            let m = i.mirror.as_mut().expect("mirrored");
            let moved = match heard {
                Some((st, at)) if m.heard(&st, at) => st.handed_to.filter(|to| *to != me && *to != id),
                _ => None,
            };
            if moved.is_none() {
                if let Some(from) = m.wanted() {
                    m.asking = Some(from);
                    page = Some((id, from));
                }
            }
            moved
        };
        if let Some(to) = next {
            return self.set_active(Some(to));
        }
        if let Some((id, from)) = page {
            self.send(id, Op::Page { from, count: PAGE });
        }
    }

    /// Carries out a command, received at `received` (this device's clock), and answers it.
    fn obey(self: &Arc<Self>, via: Via, from: String, id: u64, op: Op, received: i64) {
        let body = match op {
            Op::Clock { t1 } => Body::Clock { t1, t2: received, t3: clock::now_us() },
            Op::Page { from: turn, count } if !matches!(via, Via::Relay(Some(_))) => {
                let q = self.read_queue(|_, len| turn as usize..(turn.saturating_add(count.min(PAGE_MAX)) as usize).min(len));
                Body::Page { id, rev: q.rev, from: turn, entries: q.entries }
            }
            op => Body::Ack { id, refusal: self.carry_out(&via, &from, op).err() },
        };
        self.answer_to(via, from, body);
    }

    fn answer_to(&self, via: Via, to: String, body: Body) {
        match via {
            Via::Relay(room) => self.out(Out::send(Link::Relay, Outgoing { room, to: Some(to), body: Some(body), state: None })),
            Via::Door => {
                if let Some(d) = &self.inner.lock().door {
                    d.reply(&to, body);
                }
            }
            Via::Peer(base) => self.out(Out::send(Link::Lan(base), Outgoing { to: Some(to), body: Some(body), ..Default::default() })),
        }
    }

    fn carry_out(self: &Arc<Self>, via: &Via, from: &str, op: Op) -> Result<(), Refusal> {
        let in_jam = matches!(via, Via::Relay(Some(_)));
        if is_jam(&op) {
            let kick = match &op {
                Op::Kick { member } => Some(member.clone()),
                _ => None,
            };
            let (song, room) = {
                let mut i = self.inner.lock();
                let h = i.hosted.as_mut().ok_or(Refusal::Unknown)?;
                let by = if in_jam { By::Member(from) } else { By::Host };
                (h.jam.apply(by, op)?, h.jam.room.clone())
            };
            if let Some(song) = song {
                self.player.apply(Op::Add { songs: vec![song], next: false });
            }
            if let Some(member) = kick {
                self.out(Out::Get(self.relay_url("noriRemote.kick", &[("room", room), ("member", member)])));
            }
            self.publish();
            return Ok(());
        }
        let sender = if in_jam { Sender::Member(Role::Guest) } else { Sender::Owner };
        let (rev, len) = self.client.core.session.playlist(|p| (p.rev(), p.len() as u32));
        admit(&op, sender, rev, len)?;
        match op {
            Op::Transfer { to } => self.transfer(via, from, to),
            op => {
                // A queue sent here plays here: this is the active device.
                if matches!(op, Op::Replace { .. }) {
                    self.set_active(None);
                }
                self.player.apply(op)
            }
        }
        Ok(())
    }

    /// Hands the queue, its play order, shuffle, repeat and the position to device `to`, then pauses here
    /// and follows `to`, the active device now.
    fn transfer(self: &Arc<Self>, via: &Via, from: &str, to: String) {
        let session = &self.client.core.session;
        // A weighted shuffle's list is already in play order: shown shuffled, it goes as that order.
        let (ids, index, order, shuffle, repeat) = session.playlist(|p| (p.ids().to_vec(), p.current(), p.lit().then(|| p.play_order().map(|i| i as u32).collect()), p.lit(), p.repeat()));
        let Some(index) = index else { return };
        let songs = ids.into_iter().map(|id| session.song(&id).unwrap_or_else(|| Song::only_id(id))).collect();
        let (playing, position_ms) = {
            let mut i = self.inner.lock();
            i.handed_to = Some(to.clone());
            (i.playing.playing, i.position_now())
        };
        let op = Op::Replace { songs, index: index as u32, position_ms, play: playing, order, shuffle, repeat };
        let id = self.next_id();
        let command = Body::Command { id, op: Box::new(op) };
        if to == from {
            self.answer_to(via.clone(), to.clone(), command);
        } else {
            let link = self.inner.lock().peers.iter().find(|p| p.member.id == to).map_or(Link::Relay, |p| Link::Lan(p.base()));
            self.out(Out::send(link, Outgoing { to: Some(to.clone()), body: Some(command), ..Default::default() }));
        }
        self.player.apply(Op::Pause);
        self.set_active(Some(to));
        self.publish();
    }

    /// The songs at the turns of the play order `span` picks (from where the song heard is, and how many
    /// there are), with the queue's revision, length, shuffle and repeat.
    fn read_queue(&self, span: impl FnOnce(Option<usize>, usize) -> Range<usize>) -> QueueRead {
        let session = &self.client.core.session;
        let heard = self.inner.lock().playing.index.map(|i| i as usize);
        let (picked, index, rev, len, shuffle, repeat) = session.playlist(|p| {
            let current = heard.filter(|&i| i < p.len()).or(p.current());
            let order: Vec<usize> = p.play_order().collect();
            let at = current.and_then(|c| order.iter().position(|&o| o == c));
            let r = span(at, order.len());
            let picked: Vec<(u32, u32, String)> = order[r.clone()].iter().zip(r.start..).map(|(&i, turn)| (i as u32, turn as u32, p.ids()[i].clone())).collect();
            (picked, current.map(|c| c as u32), p.rev(), p.len() as u32, p.lit(), p.repeat())
        });
        let i = self.inner.lock();
        let jam = i.hosted.as_ref().map(|h| &h.jam);
        // Hearts as this device shows them: pressed here or by another device since the song was read.
        let stars = self.client.core.stars.lock();
        let entries = picked
            .into_iter()
            .map(|(index, turn, id)| {
                let by = jam.and_then(|j| j.added_by(&id));
                let mut song = session.song(&id).unwrap_or_else(|| Song::only_id(id));
                song.starred = stars.starred(crate::client::Starrable::Song, &song.id, song.starred);
                Entry::of(index, turn, &song, by)
            })
            .collect();
        QueueRead { entries, index, rev, len, shuffle, repeat }
    }

    /// This device's state now, its position where the song is at this moment.
    fn state_now(&self) -> DeviceState {
        let q = self.read_queue(|at, len| {
            let at = at.unwrap_or(0);
            at.saturating_sub(ENTRIES_BEFORE)..(at + ENTRIES_AFTER + 1).min(len)
        });
        let i = self.inner.lock();
        let p = i.playing;
        let mut st = DeviceState {
            seq: i.seq,
            playing: p.playing,
            buffering: p.buffering,
            position_ms: p.position_ms,
            index: q.index,
            rev: q.rev,
            len: q.len,
            entries: q.entries,
            volume: p.volume,
            shuffle: q.shuffle,
            repeat: q.repeat,
            jam: i.hosted.as_ref().map(|h| h.jam.state()),
            handed_to: i.handed_to.clone(),
            at_us: None,
        };
        let now = clock::now_us();
        st.position_ms = i.position_at_of(&st, now);
        st.at_us = Some(now);
        st
    }

    /// Publishes this device's state where it is followed, unless only time moved on.
    fn publish(self: &Arc<Self>) {
        let mut state = self.state_now();
        let (serving, jam_room, relay) = {
            let mut i = self.inner.lock();
            if let Some((last, _)) = &i.published {
                if same_but_time(last, &state) {
                    return;
                }
            }
            i.seq += 1;
            state.seq = i.seq;
            i.published = Some((state.clone(), Instant::now()));
            if let Some(d) = &i.door {
                d.publish(state.clone());
            }
            (i.serving && i.since.is_some(), i.jam_room().map(str::to_string), i.relay != RelaySupport::Unsupported)
        };
        if serving && relay {
            self.out(Out::send(Link::Relay, Outgoing { state: Some(state.clone()), ..Default::default() }));
        }
        if let Some(room) = jam_room {
            self.out(Out::send(Link::Relay, Outgoing { room: Some(room), state: Some(state), ..Default::default() }));
        }
        self.shown.changed();
    }
}

/// What [`Remote::read_queue`] read.
struct QueueRead {
    entries: Vec<Entry>,
    index: Option<u32>,
    rev: u64,
    len: u32,
    shuffle: bool,
    repeat: u8,
}

/// A page of the queue of device `from` arrived.
fn paged(i: &mut Inner, from: &str, rev: u64, turn: u32, entries: Vec<Entry>) {
    if let Some(m) = i.mirror.as_mut().filter(|m| m.id == from) {
        m.page(rev, turn, entries);
    }
}

/// A time exchange with device `from` came back.
fn timed(i: &mut Inner, from: &str, exchange: clock::Exchange) {
    if let Some(m) = i.mirror.as_mut().filter(|m| m.id == from) {
        m.clock.add(exchange);
    }
}

fn note(refused: &mut HashMap<String, Refusal>, from: &str, refusal: Option<Refusal>) {
    match refusal {
        Some(r) => refused.insert(from.to_string(), r),
        None => refused.remove(from),
    };
}

/// Whether `now` is `last` with only its position run on as time passed.
fn same_but_time(last: &DeviceState, now: &DeviceState) -> bool {
    let elapsed_ms = now.at_us.zip(last.at_us).map_or(0, |(now, last)| (now - last) / 1000);
    let expected = nori_remote::position_now(last, elapsed_ms);
    (expected - now.position_ms).abs() <= POSITION_SLACK_MS && DeviceState { position_ms: now.position_ms, seq: now.seq, at_us: now.at_us, ..last.clone() } == *now
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_time_moving_is_not_news() {
        let last = DeviceState { playing: true, position_ms: 10_000, rev: 2, at_us: Some(1_000_000), ..Default::default() };
        let later = |position_ms: i64| DeviceState { position_ms, at_us: Some(6_000_000), ..last.clone() };
        assert!(same_but_time(&last, &later(15_003)));
        assert!(!same_but_time(&last, &later(15_020)), "a place 20 ms off is said again");
        assert!(!same_but_time(&last, &later(40_000)), "a seek");
        assert!(!same_but_time(&last, &DeviceState { rev: 3, ..later(15_000) }), "the queue changed");
        assert!(!same_but_time(&last, &DeviceState { playing: false, ..later(15_000) }), "paused");
    }

    #[test]
    fn a_mirrored_playhead_runs_on_from_when_it_was_heard_there() {
        let entries = vec![Entry { index: 0, duration: 300, ..Default::default() }];
        let st = DeviceState { seq: 4, playing: true, position_ms: 10_000, index: Some(0), len: 1, entries, at_us: Some(50_000_000), ..Default::default() };
        let mut m = Mirrored::new("desk".into());
        let view = |m: &Mirrored| m.view("Desk".into(), DeviceKind::Desktop, None).unwrap();
        // Its clock not known yet: as of when the state arrived.
        assert!(m.heard(&st, 2_000_000));
        assert_eq!((view(&m).position_ms, view(&m).at_us), (10_000, 2_000_000));
        // The same state read again (every poll answer carries it) does not start the clock again.
        assert!(!m.heard(&st, 3_000_000));
        assert_eq!(view(&m).at_us, 2_000_000);
        // Its clock 49.2 s ahead of this one's: heard there at 0.8 s here, though it arrived at 2 s.
        m.clock.add(clock::Exchange { t1: 1_000_000, t2: 50_201_000, t3: 50_201_500, t4: 1_002_500 });
        let v = view(&m);
        assert_eq!(v.at_us, 800_000);
        assert_eq!(v.position_at(1_800_000), 11_000);
        assert_eq!(v.position_at(400_000_000), 300_000, "held at the song's end");
        // A command foreseen here runs on from when it was sent.
        m.foresee(&Op::Pause);
        assert!(!view(&m).playing && view(&m).position_at(900_000_000) == view(&m).position_ms);
    }

    #[test]
    fn a_server_without_a_relay_answers_with_an_error() {
        assert!(answer(br#"{"subsonic-response":{"status":"failed","error":{"code":0}}}"#).is_none());
        assert_eq!(answer(br#"{"seq":3,"you":"a"}"#).map(|a| a.seq), Some(3));
    }
}
