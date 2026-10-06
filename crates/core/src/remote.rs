//! Remote control and jams over a client (nori-remote has the protocol). A [`Remote`] exists only while
//! remote control or jams are switched on, or the profile is a jam guest's; it polls the relay only
//! while it serves (this device is controllable), watches (a device picker or jam screen is open) or
//! hosts a jam, and holds each poll up to [`HOLD_MS`], so a quiet device wakes about once a minute.
//! Nearby devices are reached through their LAN door instead of the relay.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

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

/// Songs before and after the current one a published state lists.
const ENTRIES_BEFORE: usize = 10;
const ENTRIES_AFTER: usize = 40;

/// After a failed poll, the relay is asked again this much later (or when something changes here).
const RETRY_MS: u64 = 30_000;

/// A held poll's own timeout: the hold plus the way there and back.
const POLL_TIMEOUT_MS: u32 = HOLD_MS + 15_000;

/// A published position this close to where the last one runs on to is not sent again.
const POSITION_SLACK_MS: i64 = 1_500;

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
    Send(Link, Outgoing),
    /// A GET to the relay (leaving, closing a jam, sending a member out).
    Get(String),
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
    base: String,
    member: Member,
    received: Instant,
    since: Option<u64>,
    polling: bool,
}

struct Hosted {
    jam: Jam,
    link: String,
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
    /// When each other member's state last changed here.
    received: HashMap<String, Instant>,
    peers: Vec<Peer>,
    hosted: Option<Hosted>,
    playing: Playing,
    published: Option<(DeviceState, Instant)>,
    next_id: u64,
    refused: HashMap<String, Refusal>,
    door: Option<Door>,
}

impl Inner {
    fn wants_relay(&self) -> bool {
        self.relay != RelaySupport::Unsupported && (self.serving || self.watching || self.hosted.is_some())
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
        self.received.get(id).map_or(0, |t| t.elapsed().as_millis() as i64)
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
    out: mpsc::Sender<Out>,
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
        let remote = Arc::new(Remote { client, id, me, player, shown, discovery, inner: Mutex::default(), retry: Condvar::new(), out });
        let (client, who) = (remote.client.clone(), remote.who());
        // Sends one at a time, in order; it ends with the remote.
        let _ = std::thread::Builder::new().name("nori-remote-out".into()).spawn(move || {
            for o in rx {
                deliver(&client, &who, o);
            }
        });
        // Whether the server relays, asked once: one poll that is not held.
        let me = remote.clone();
        let _ = std::thread::Builder::new().name("nori-remote-probe".into()).spawn(move || {
            let got = block_on(transport::get(&*me.client.transport, me.poll_url(None, false), 0));
            if let Some(found) = support(&got) {
                let mut i = me.inner.lock();
                if i.relay == RelaySupport::Unknown {
                    i.relay = found;
                }
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

    /// The platform's playback changed (play, pause, seek, another song, the queue, the volume).
    pub fn played(self: Arc<Self>, playing: Playing) {
        let publish = {
            let mut i = self.inner.lock();
            i.playing = playing;
            i.serving || i.hosted.is_some()
        };
        if publish {
            self.publish();
        }
    }

    /// The account's other devices, nearby ones first.
    pub fn devices(&self) -> Vec<RemoteDevice> {
        let i = self.inner.lock();
        let seen = |m: &Member, age_ms: i64, nearby: bool| RemoteDevice { id: m.id.clone(), name: m.name.clone(), kind: m.kind, state: m.state.clone(), age_ms, nearby, refused: i.refused.get(&m.id).copied() };
        let mut out: Vec<RemoteDevice> = i.peers.iter().filter(|p| p.member.state.is_some()).map(|p| seen(&p.member, p.received.elapsed().as_millis() as i64, true)).collect();
        for room in i.rooms.iter().filter(|r| !r.jam) {
            for m in room.members.iter().filter(|m| m.id != self.id && m.state.is_some()) {
                if !out.iter().any(|d| d.id == m.id) {
                    out.push(seen(m, i.age(&m.id), false));
                }
            }
        }
        out
    }

    /// Sends `op` to device `device`; its answer shows in [`Remote::devices`].
    pub fn send(&self, device: String, op: Op) {
        let link = {
            let mut i = self.inner.lock();
            i.refused.remove(&device);
            let lan = i.peers.iter().find(|p| p.member.id == device).map(|p| Link::Lan(p.base.clone()));
            match lan {
                Some(l) => l,
                None if i.relay == RelaySupport::Unsupported => return,
                None => Link::Relay,
            }
        };
        let id = self.next_id();
        self.out(Out::Send(link, Outgoing { to: Some(device), body: Some(Body::Command { id, op: Box::new(op) }), ..Default::default() }));
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
        {
            let mut i = self.inner.lock();
            i.peers.retain(|p| p.service != service && p.member.id != id);
            let member = Member { id, name: get("name"), kind, state: None };
            i.peers.push(Peer { service, base: format!("http://{host}:{port}"), member, received: Instant::now(), since: None, polling: false });
        }
        self.keep_polling();
    }

    /// A door went away.
    pub fn lan_lost(&self, service: String) {
        self.inner.lock().peers.retain(|p| p.service != service);
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
                self.out(Out::Send(Link::Relay, Outgoing { room: Some(room), to: Some(host), body: Some(Body::Command { id, op: Box::new(op) }), state: None }));
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
    fn out(&self, o: Out) {
        let _ = self.out.send(o);
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
                    r.obey(Via::Door, from.to_string(), id, *op);
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
            if i.watching {
                let generation = i.lan_generation;
                for p in i.peers.iter_mut().filter(|p| !p.polling) {
                    p.polling = true;
                    start_peers.push((p.service.clone(), generation));
                }
            }
        }
        self.retry.notify_all();
        if let Some(generation) = start_relay {
            let me = self.clone();
            let _ = std::thread::Builder::new().name("nori-remote".into()).spawn(move || me.poll_relay(generation));
        }
        for (service, generation) in start_peers {
            let me = self.clone();
            let _ = std::thread::Builder::new().name("nori-remote-lan".into()).spawn(move || me.poll_peer(service, generation));
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
            let got = block_on(transport::get(&*self.client.transport, url, POLL_TIMEOUT_MS));
            match (support(&got), got) {
                (Some(RelaySupport::Supported), Ok(body)) => {
                    self.inner.lock().relay = RelaySupport::Supported;
                    self.took(answer(&body).unwrap_or_default());
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
                let i = self.inner.lock();
                let Some(p) = i.peers.iter().find(|p| p.service == service) else { return };
                if i.lan_generation != generation || !i.watching {
                    return;
                }
                (p.base.clone(), p.since)
            };
            let Some((_, secret)) = self.client.core.account.read().clone() else { return };
            let query = match since {
                Some(s) => format!("/rest/noriRemote.poll?dev={}&since={s}&hold=1", self.id),
                None => format!("/rest/noriRemote.poll?dev={}", self.id),
            };
            let url = format!("{base}{}", lan::signed(&secret, "GET", &query, b"", db::now_ms()));
            let got = block_on(transport::get(&*self.client.transport, url, POLL_TIMEOUT_MS)).ok().and_then(|b| serde_json::from_slice::<Answer>(&b).ok());
            let Some(a) = got else {
                self.lan_lost(service);
                return;
            };
            let mut commands = Vec::new();
            let (from, base) = {
                let mut i = self.inner.lock();
                let Some(p) = i.peers.iter_mut().find(|p| p.service == service) else { return };
                p.since = Some(a.seq);
                if let Some(m) = a.rooms.into_iter().flat_map(|r| r.members).next() {
                    if m.state != p.member.state {
                        p.received = Instant::now();
                    }
                    p.member.state = m.state;
                }
                let (from, base) = (p.member.id.clone(), p.base.clone());
                for e in a.events {
                    match e.body {
                        Body::Command { id, op } => commands.push((id, *op)),
                        Body::Ack { refusal, .. } => note(&mut i.refused, &from, refusal),
                    }
                }
                (from, base)
            };
            for (id, op) in commands {
                self.obey(Via::Peer(base.clone()), from.clone(), id, op);
            }
            self.shown.changed();
        }
    }

    fn took(self: &Arc<Self>, a: Answer) {
        let mut commands = Vec::new();
        let mut republish = false;
        {
            let mut i = self.inner.lock();
            i.since = Some(a.seq);
            i.you = a.you;
            for m in a.rooms.iter().flat_map(|r| &r.members).filter(|m| m.id != self.id) {
                let before = i.rooms.iter().flat_map(|r| &r.members).find(|o| o.id == m.id).map(|o| &o.state);
                if before != Some(&m.state) {
                    i.received.insert(m.id.clone(), Instant::now());
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
                }
            }
            i.rooms = a.rooms;
        }
        for (via, from, id, op) in commands {
            self.obey(via, from, id, op);
        }
        if republish {
            self.publish();
        }
        self.shown.changed();
    }

    /// Carries out a command and answers it.
    fn obey(self: &Arc<Self>, via: Via, from: String, id: u64, op: Op) {
        let refusal = self.carry_out(&via, &from, op).err();
        self.answer_to(via, from, Body::Ack { id, refusal });
    }

    fn answer_to(&self, via: Via, to: String, body: Body) {
        match via {
            Via::Relay(room) => self.out(Out::Send(Link::Relay, Outgoing { room, to: Some(to), body: Some(body), state: None })),
            Via::Door => {
                if let Some(d) = &self.inner.lock().door {
                    d.reply(&to, body);
                }
            }
            Via::Peer(base) => self.out(Out::Send(Link::Lan(base), Outgoing { to: Some(to), body: Some(body), ..Default::default() })),
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
            op => self.player.apply(op),
        }
        Ok(())
    }

    /// Hands the queue and position to device `to`, then pauses here.
    fn transfer(self: &Arc<Self>, via: &Via, from: &str, to: String) {
        let session = &self.client.core.session;
        let (ids, index) = session.playlist(|p| (p.ids().to_vec(), p.current()));
        let Some(index) = index else { return };
        let songs = ids.into_iter().map(|id| session.song(&id).unwrap_or_else(|| Song::only_id(id))).collect();
        let (playing, age) = {
            let i = self.inner.lock();
            (i.playing, i.published.as_ref().map_or(0, |(_, at)| at.elapsed().as_millis() as i64))
        };
        let position_ms = if playing.playing { playing.position_ms + age } else { playing.position_ms };
        let op = Op::Replace { songs, index: index as u32, position_ms, play: playing.playing };
        let id = self.next_id();
        let command = Body::Command { id, op: Box::new(op) };
        if to == from {
            self.answer_to(via.clone(), to, command);
        } else {
            let link = self.inner.lock().peers.iter().find(|p| p.member.id == to).map_or(Link::Relay, |p| Link::Lan(p.base.clone()));
            self.out(Out::Send(link, Outgoing { to: Some(to), body: Some(command), ..Default::default() }));
        }
        self.player.apply(Op::Pause);
    }

    /// This device's state now.
    fn state_now(&self) -> DeviceState {
        let session = &self.client.core.session;
        let heard = self.inner.lock().playing.index.map(|i| i as usize);
        let (window, index, rev, shuffle, repeat) = session.playlist(|p| {
            let current = heard.filter(|&i| i < p.len()).or(p.current());
            let order: Vec<usize> = p.play_order().collect();
            let at = current.and_then(|c| order.iter().position(|&o| o == c)).unwrap_or(0);
            let window: Vec<(usize, String)> = order[at.saturating_sub(ENTRIES_BEFORE)..(at + ENTRIES_AFTER + 1).min(order.len())].iter().map(|&i| (i, p.ids()[i].clone())).collect();
            (window, current.map(|c| c as u32), p.rev(), p.lit(), p.repeat())
        });
        let i = self.inner.lock();
        let jam = i.hosted.as_ref().map(|h| &h.jam);
        let entries = window
            .into_iter()
            .map(|(index, id)| {
                let by = jam.and_then(|j| j.added_by(&id));
                Entry::of(index as u32, &session.song(&id).unwrap_or_else(|| Song::only_id(id)), by)
            })
            .collect();
        DeviceState { playing: i.playing.playing, position_ms: i.playing.position_ms, index, rev, entries, volume: i.playing.volume, shuffle, repeat, jam: jam.map(Jam::state) }
    }

    /// Publishes this device's state where it is followed, unless only time moved on.
    fn publish(self: &Arc<Self>) {
        let state = self.state_now();
        let (serving, jam_room, relay) = {
            let mut i = self.inner.lock();
            if let Some((last, at)) = &i.published {
                if same_but_time(last, &state, at.elapsed().as_millis() as i64) {
                    return;
                }
            }
            i.published = Some((state.clone(), Instant::now()));
            if let Some(d) = &i.door {
                d.publish(state.clone());
            }
            (i.serving, i.jam_room().map(str::to_string), i.relay != RelaySupport::Unsupported)
        };
        if serving && relay {
            self.out(Out::Send(Link::Relay, Outgoing { state: Some(state.clone()), ..Default::default() }));
        }
        if let Some(room) = jam_room {
            self.out(Out::Send(Link::Relay, Outgoing { room: Some(room), state: Some(state), ..Default::default() }));
        }
        self.shown.changed();
    }
}

fn note(refused: &mut HashMap<String, Refusal>, from: &str, refusal: Option<Refusal>) {
    match refusal {
        Some(r) => refused.insert(from.to_string(), r),
        None => refused.remove(from),
    };
}

/// Whether `now` is `last` with only its position run on as time passed.
fn same_but_time(last: &DeviceState, now: &DeviceState, elapsed_ms: i64) -> bool {
    let expected = nori_remote::position_now(last, elapsed_ms);
    (expected - now.position_ms).abs() <= POSITION_SLACK_MS && DeviceState { position_ms: now.position_ms, ..last.clone() } == *now
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_time_moving_is_not_news() {
        let last = DeviceState { playing: true, position_ms: 10_000, rev: 2, ..Default::default() };
        assert!(same_but_time(&last, &DeviceState { position_ms: 15_200, ..last.clone() }, 5_000));
        assert!(!same_but_time(&last, &DeviceState { position_ms: 40_000, ..last.clone() }, 5_000), "a seek");
        assert!(!same_but_time(&last, &DeviceState { position_ms: 15_000, rev: 3, ..last.clone() }, 5_000), "the queue changed");
        let paused = DeviceState { playing: false, ..last.clone() };
        assert!(!same_but_time(&last, &DeviceState { position_ms: 15_000, ..paused }, 5_000), "paused");
    }

    #[test]
    fn a_server_without_a_relay_answers_with_an_error() {
        assert!(answer(br#"{"subsonic-response":{"status":"failed","error":{"code":0}}}"#).is_none());
        assert_eq!(answer(br#"{"seq":3,"you":"a"}"#).map(|a| a.seq), Some(3));
    }
}
