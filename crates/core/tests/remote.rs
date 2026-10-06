//! Remote control and jams through the core's public calls, against a relay kept in memory that answers
//! as octo-fiesta's hub does (rooms, held polls, jam invites, guests acting with the host's rights): two
//! devices of one account controlling each other, and a jam with a host, an admin and a guest.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nori_core::client::{Client, NetProfile};
use nori_core::remote::{jam_join, Discovery, Playing, RelaySupport, Remote, RemoteMe, RemotePlayer, RemoteShown};
use nori_core::transport::{block_on, Exchange, FailureKind, Transport, TransportError, TransportResponse};
use nori_core::{Core, ServerConfig, Song};
use nori_remote::wire::{Answer, Body, DeviceKind, Event, Member, Op, Outgoing, Refusal, Role, Room};
use parking_lot::{Condvar, Mutex};

const SERVER: &str = "http://octo:5274";

/// Who a request is from, as the relay's checks find it.
#[derive(Clone)]
enum Caller {
    Account(String),
    /// A jam member, by room and member id.
    Guest(String, String),
    /// Someone holding an invite, not yet a member.
    Invited(String),
}

#[derive(Default)]
struct RoomState {
    jam: bool,
    /// The host's user and device, for a jam.
    host: Option<(String, String)>,
    members: Vec<Member>,
    /// (seq, from, to, body).
    events: Vec<(u64, String, Option<String>, Body)>,
}

#[derive(Default)]
struct Hub {
    seq: u64,
    rooms: HashMap<String, RoomState>,
    /// Invite keys and member keys: (room, member id; None for an invite).
    keys: HashMap<String, (String, Option<String>)>,
    /// Every endpoint asked, with its id parameter.
    asked: Vec<String>,
    closed: bool,
}

/// The relay in front of a server whose library is a few songs.
struct Relay {
    hub: Mutex<Hub>,
    changed: Condvar,
    next: AtomicU64,
    /// A plain Navidrome: no `noriRemote.*` at all.
    absent: bool,
}

fn decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn json(v: impl serde::Serialize) -> Vec<u8> {
    serde_json::to_vec(&v).unwrap()
}

fn subsonic_error(message: &str) -> Vec<u8> {
    format!(r#"{{"subsonic-response":{{"status":"failed","error":{{"code":40,"message":"{message}"}}}}}}"#).into_bytes()
}

/// What a jam guest's key may ask: reading the library through the host's account, and the jam.
const GUEST_ENDPOINTS: &[&str] = &["ping", "search3", "getCoverArt", "getSong", "getAlbum", "getArtist", "noriRemote.poll", "noriRemote.send", "noriRemote.leave"];

impl Relay {
    fn new() -> Arc<Relay> {
        Arc::new(Relay { hub: Mutex::default(), changed: Condvar::new(), next: AtomicU64::new(1), absent: false })
    }

    fn absent() -> Arc<Relay> {
        Arc::new(Relay { absent: true, ..Arc::into_inner(Relay::new()).unwrap() })
    }

    fn key(&self) -> String {
        format!("k{}", self.next.fetch_add(1, Ordering::Relaxed))
    }

    fn asked(&self) -> Vec<String> {
        self.hub.lock().asked.clone()
    }

    fn close(&self) {
        self.hub.lock().closed = true;
        self.changed.notify_all();
    }

    fn caller(&self, endpoint: &str, p: &HashMap<String, String>) -> Result<Caller, Vec<u8>> {
        if let Some(user) = p.get("u") {
            return Ok(Caller::Account(user.clone()));
        }
        let key = p.get("apiKey").and_then(|k| k.strip_prefix("nori-jam-")).ok_or_else(|| subsonic_error("no credentials"))?;
        match self.hub.lock().keys.get(key) {
            Some((room, None)) if endpoint == "noriRemote.join" => Ok(Caller::Invited(room.clone())),
            Some((room, Some(member))) if GUEST_ENDPOINTS.contains(&endpoint) => Ok(Caller::Guest(room.clone(), member.clone())),
            _ => Err(subsonic_error("not for a jam guest")),
        }
    }

    fn answer(&self, endpoint: &str, p: &HashMap<String, String>, body: Option<&str>) -> Vec<u8> {
        self.hub.lock().asked.push(format!("{endpoint} {} {}", p.get("id").map_or("", String::as_str), p.get("dev").map_or("", String::as_str)));
        if self.absent && endpoint.starts_with("noriRemote.") {
            return subsonic_error("not here");
        }
        let caller = match self.caller(endpoint, p) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let dev = p.get("dev").cloned().unwrap_or_default();
        let mut hub = self.hub.lock();
        let bump = |hub: &mut Hub| {
            hub.seq += 1;
            self.changed.notify_all();
        };
        match (endpoint, &caller) {
            ("noriRemote.poll", _) => {
                let (me, rooms) = match &caller {
                    Caller::Account(user) => {
                        let account = format!("u:{user}");
                        let room = hub.rooms.entry(account.clone()).or_default();
                        let listed = room.members.iter().position(|m| m.id == dev);
                        let serve = p.get("serve").map(String::as_str) == Some("1");
                        let mut changed = false;
                        match (listed, serve) {
                            (None, true) => {
                                let kind = serde_json::from_value(serde_json::Value::String(p["kind"].clone())).unwrap_or_default();
                                room.members.push(Member { id: dev.clone(), name: p["name"].clone(), kind, state: None });
                                changed = true;
                            }
                            (Some(at), false) => {
                                room.members.remove(at);
                                changed = true;
                            }
                            _ => {}
                        }
                        if changed {
                            bump(&mut hub);
                        }
                        let hosted = hub.rooms.iter().filter(|(_, r)| r.host.as_ref() == Some(&(user.clone(), dev.clone()))).map(|(id, _)| id.clone());
                        (dev.clone(), std::iter::once(account).chain(hosted).collect::<Vec<_>>())
                    }
                    Caller::Guest(room, member) => (member.clone(), vec![room.clone()]),
                    Caller::Invited(_) => unreachable!(),
                };
                let since = p.get("since").and_then(|s| s.parse::<u64>().ok());
                let mine = |hub: &Hub| -> Vec<Event> {
                    rooms
                        .iter()
                        .filter_map(|r| hub.rooms.get(r).map(|state| (r, state)))
                        .flat_map(|(r, state)| state.events.iter().map(move |e| (r, e)))
                        .filter(|(_, (seq, from, to, _))| since.is_some_and(|s| *seq > s) && *from != me && to.as_ref().is_none_or(|t| *t == me))
                        .map(|(r, (seq, from, _, body))| Event { seq: *seq, room: r.clone(), from: from.clone(), body: body.clone() })
                        .collect()
                };
                if let (Some(since), Some("1")) = (since, p.get("hold").map(String::as_str)) {
                    let until = Instant::now() + Duration::from_secs(50);
                    while hub.seq <= since && !hub.closed && !self.changed.wait_until(&mut hub, until).timed_out() {}
                }
                let answer = Answer {
                    seq: hub.seq,
                    you: me.clone(),
                    rooms: rooms.iter().filter_map(|r| hub.rooms.get(r).map(|s| Room { room: r.clone(), jam: s.jam, members: s.members.clone() })).collect(),
                    events: mine(&hub),
                };
                json(answer)
            }
            ("noriRemote.send", _) => {
                let out: Outgoing = serde_json::from_str(body.unwrap_or("{}")).unwrap();
                let (room, from) = match &caller {
                    Caller::Account(user) => (out.room.clone().unwrap_or(format!("u:{user}")), dev.clone()),
                    Caller::Guest(room, member) => (room.clone(), member.clone()),
                    Caller::Invited(_) => unreachable!(),
                };
                let seq = hub.seq + 1;
                let state = hub.rooms.entry(room).or_default();
                if let Some(s) = out.state {
                    // A device publishing in its account's room serves, polled or not yet.
                    if !state.jam && !state.members.iter().any(|m| m.id == from) {
                        let kind = serde_json::from_value(serde_json::Value::String(p["kind"].clone())).unwrap_or_default();
                        state.members.push(Member { id: from.clone(), name: p["name"].clone(), kind, state: None });
                    }
                    match state.members.iter_mut().find(|m| m.id == from) {
                        Some(m) => m.state = Some(s),
                        None => return subsonic_error("not a member"),
                    }
                }
                if let Some(b) = out.body {
                    state.events.push((seq, from, out.to, b));
                }
                bump(&mut hub);
                json(serde_json::json!({ "seq": seq }))
            }
            ("noriRemote.open", Caller::Account(user)) => {
                let (room, invite) = (format!("j{}", self.key()), self.key());
                let host = Member { id: dev.clone(), name: p["name"].clone(), kind: DeviceKind::Phone, state: None };
                hub.rooms.insert(room.clone(), RoomState { jam: true, host: Some((user.clone(), dev)), members: vec![host], events: Vec::new() });
                hub.keys.insert(invite.clone(), (room.clone(), None));
                bump(&mut hub);
                json(serde_json::json!({ "room": room, "invite": invite }))
            }
            ("noriRemote.join", Caller::Invited(room)) => {
                let (member, key) = (format!("m{}", self.key()), self.key());
                hub.keys.insert(key.clone(), (room.clone(), Some(member.clone())));
                hub.rooms.get_mut(room).unwrap().members.push(Member { id: member.clone(), name: p["name"].clone(), kind: DeviceKind::Guest, state: None });
                bump(&mut hub);
                json(serde_json::json!({ "room": room, "member": member, "key": key }))
            }
            ("noriRemote.kick", Caller::Account(_)) => {
                let (room, member) = (p["room"].clone(), p["member"].clone());
                hub.keys.retain(|_, (r, m)| !(*r == room && m.as_deref() == Some(member.as_str())));
                hub.rooms.get_mut(&room).unwrap().members.retain(|m| m.id != member);
                bump(&mut hub);
                json(serde_json::json!({}))
            }
            ("ping", _) => br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#.to_vec(),
            _ => subsonic_error("not here"),
        }
    }
}

#[async_trait::async_trait]
impl Transport for Relay {
    async fn get(&self, url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        self.send(Exchange { url, ..Default::default() }).await
    }

    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
        if request.url.starts_with("http://127.0.0.1:") {
            return Ok(lan_exchange(&request));
        }
        let Some(rest) = request.url.strip_prefix(&format!("{SERVER}/rest/")) else {
            return Err(TransportError::Failed { kind: FailureKind::Connect, detail: Some("unreachable".into()) });
        };
        let (endpoint, query) = rest.split_once('?').unwrap_or((rest, ""));
        let params = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), decode(v))).collect();
        Ok(TransportResponse { status: 200, body: self.answer(endpoint, &params, request.json.as_deref()) })
    }

    fn address_changed(&self) {}

    fn network(&self) -> nori_core::transport::Network {
        nori_core::transport::Network::Unmetered
    }
}

/// One HTTP/1.1 exchange with a door on this machine.
fn lan_exchange(request: &Exchange) -> TransportResponse {
    use std::io::{BufRead, BufReader, Read, Write};
    let rest = request.url.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, target) = rest.split_at(rest.find('/').unwrap());
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    let (method, body) = request.json.as_deref().map_or(("GET", ""), |b| ("POST", b));
    write!(c, "{method} {target} HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut r = BufReader::new(c);
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut length = 0;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.strip_prefix("Content-Length: ") {
            length = v.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    r.read_exact(&mut body).unwrap();
    TransportResponse { status, body }
}

/// What a device's discovery was asked to announce.
#[derive(Default)]
struct Announced(Mutex<Option<nori_core::remote::Announcement>>);

impl Discovery for Announced {
    fn announce(&self, door: Option<nori_core::remote::Announcement>) {
        *self.0.lock() = door;
    }

    fn browse(&self, _: bool) {}
}

/// The ops a device's player was told to carry out.
struct Player(Mutex<Sender<Op>>);

impl RemotePlayer for Player {
    fn apply(&self, op: Op) {
        let _ = self.0.lock().send(op);
    }
}

/// Counts the remote's change notices, so a test waits for news rather than for time.
struct Shown(Mutex<Sender<()>>);

impl RemoteShown for Shown {
    fn changed(&self) {
        let _ = self.0.lock().send(());
    }
}

struct Device {
    core: Arc<Core>,
    remote: Arc<Remote>,
    ops: Receiver<Op>,
    news: Receiver<()>,
}

impl Device {
    fn new(relay: &Arc<Relay>, config: ServerConfig, kind: DeviceKind, name: &str) -> Device {
        Device::found(relay, config, kind, name, None)
    }

    fn found(relay: &Arc<Relay>, config: ServerConfig, kind: DeviceKind, name: &str, discovery: Option<Arc<dyn Discovery>>) -> Device {
        let core = Core::new(String::new(), "remote".into(), Default::default()).unwrap();
        core.configure(config).unwrap();
        let client = Client::new(core.clone(), relay.clone(), Default::default());
        client.set_profile(NetProfile { url: SERVER.into(), ..Default::default() });
        let (ops_to, ops) = channel();
        let (news_to, news) = channel();
        let remote = Remote::new(client, RemoteMe { name: name.into(), kind }, Arc::new(Player(Mutex::new(ops_to))), Arc::new(Shown(Mutex::new(news_to))), discovery);
        Device { core, remote, ops, news }
    }

    fn account(relay: &Arc<Relay>, kind: DeviceKind, name: &str) -> Device {
        Device::new(relay, ServerConfig { url: SERVER.into(), user: "ann".into(), password: "pw".into(), ..Default::default() }, kind, name)
    }

    /// Waits for `ready` to hold, rechecking at each change notice.
    fn until<T>(&self, what: &str, mut ready: impl FnMut(&Remote) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(v) = ready(&self.remote) {
                return v;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(self.news.recv_timeout(left).is_ok(), "waited for {what}");
        }
    }

    fn told(&self) -> Op {
        self.ops.recv_timeout(Duration::from_secs(10)).expect("an op for the player")
    }

    fn playing(&self, ids: &[&str], start: u32) {
        let songs: Vec<Song> = ids.iter().map(|id| Song { id: id.to_string(), title: id.to_uppercase(), duration: 200, ..Default::default() }).collect();
        self.core.session.register(songs);
        self.core.session.set(ids.iter().map(|s| s.to_string()).collect(), Some(start), false, None);
        self.remote.clone().played(Playing { playing: true, position_ms: 5_000, index: None, volume: Some(40) });
    }
}

#[test]
fn two_devices_control_each_other_through_the_relay() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1", "s2", "s3"], 1);
    phone.remote.clone().serve(true);
    desk.remote.clone().watch(true);

    let state = desk.until("the phone's state", |r| r.devices().into_iter().find(|d| d.name == "Phone").and_then(|d| d.state));
    assert_eq!((state.index, state.playing, state.volume), (Some(1), true, Some(40)));
    assert_eq!(state.entries.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), ["S1", "S2", "S3"]);
    let phone_id = phone.remote.id();

    desk.remote.send(phone_id.clone(), Op::Next);
    assert_eq!(phone.told(), Op::Next);
    // The song the player says it arrived on is the one shown, before the queue's own current moves.
    phone.remote.clone().played(Playing { playing: true, position_ms: 0, index: Some(2), volume: Some(40) });
    let moved = desk.until("the next song", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state).filter(|s| s.index == Some(2)));
    assert_eq!(moved.entries.iter().find(|e| Some(e.index) == moved.index).map(|e| e.title.as_str()), Some("S3"));

    // An edit made against a queue that changed since is refused, and the controller is told.
    let stale = state.rev;
    phone.core.session.remove(2, 3);
    phone.remote.clone().played(Playing { playing: true, position_ms: 6_000, index: None, volume: Some(40) });
    desk.remote.send(phone_id.clone(), Op::Remove { index: 0, rev: stale });
    let refused = desk.until("the refusal", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.refused));
    assert_eq!(refused, Refusal::Stale);
    assert!(phone.ops.try_recv().is_err(), "nothing done");
    let fresh = desk.until("the new queue", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state).filter(|s| s.rev != stale));
    desk.remote.send(phone_id.clone(), Op::Remove { index: 0, rev: fresh.rev });
    assert_eq!(phone.told(), Op::Remove { index: 0, rev: fresh.rev });

    // Playing here: the phone hands over its queue and position, then pauses.
    desk.remote.send(phone_id.clone(), Op::Transfer { to: desk.remote.id() });
    match desk.told() {
        Op::Replace { songs, index, position_ms, play } => {
            assert_eq!(songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
            assert_eq!(index, 1);
            assert!(play && position_ms >= 6_000, "at {position_ms}");
        }
        op => panic!("{op:?}"),
    }
    assert_eq!(phone.told(), Op::Pause);

    // And back: the desk sends what it plays to the phone.
    desk.playing(&["s4"], 0);
    desk.remote.clone().hand_over(phone_id.clone());
    assert!(matches!(phone.told(), Op::Replace { songs, index: 0, play: true, .. } if songs[0].id == "s4"));
    assert_eq!(desk.told(), Op::Pause);

    // Not serving any more: the phone leaves the list.
    phone.remote.clone().serve(false);
    desk.until("the phone gone", |r| r.devices().iter().all(|d| d.id != phone_id).then_some(()));
    relay.close();
}

#[test]
fn a_jam_takes_requests_through_its_host() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    host.playing(&["s1", "s2"], 0);
    let link = block_on(host.remote.clone().jam_open()).unwrap();
    assert!(link.starts_with("nori://jam?s=http%3A%2F%2Focto%3A5274&k="), "{link}");

    let guest_of = |name: &str| {
        let pass = block_on(jam_join(relay.clone(), link.clone(), name.into())).unwrap();
        assert_eq!(pass.url, SERVER);
        let d = Device::new(&relay, ServerConfig { url: pass.url, api_key: Some(pass.api_key), ..Default::default() }, DeviceKind::Guest, name);
        d.remote.clone().watch(true);
        d
    };
    let gus = guest_of("Gus");
    let dee = guest_of("Dee");

    let view = gus.until("the host's queue", |r| r.jam_view().filter(|v| v.queue.is_some() && v.members.len() == 3));
    assert!(!view.hosting);
    assert_eq!(view.queue.unwrap().entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
    let roles: Vec<(String, Role)> = view.members.iter().map(|m| (m.name.clone(), m.role)).collect();
    assert_eq!(roles, [("Host".to_string(), Role::Host), ("Gus".into(), Role::Guest), ("Dee".into(), Role::Guest)]);
    let dee_id = view.members.iter().find(|m| m.name == "Dee").unwrap().id.clone();

    // The host makes Dee an admin.
    host.remote.clone().jam_act(Op::Promote { member: dee_id, admin: true });
    dee.until("Dee's role", |r| r.jam_view().filter(|v| v.members.iter().any(|m| m.name == "Dee" && m.role == Role::Admin)));

    // A guest's provider song waits, untouched, until an admin accepts it.
    let wish = Song { id: "ext-deezer-song-9".into(), title: "Wish".into(), is_external: true, ..Default::default() };
    gus.remote.clone().jam_act(Op::Request { song: wish.clone() });
    let pending = dee.until("the request", |r| r.jam_view().and_then(|v| v.pending.first().cloned()));
    assert!(pending.provider && pending.from_name == "Gus");
    assert!(host.ops.try_recv().is_err(), "not queued yet");

    // Dee's own request goes straight in.
    let blue = Song { id: "s5".into(), title: "Blue".into(), ..Default::default() };
    dee.remote.clone().jam_act(Op::Request { song: blue.clone() });
    assert_eq!(host.told(), Op::Add { songs: vec![blue], next: false });

    dee.remote.clone().jam_act(Op::Decide { request: pending.request, accept: true });
    assert_eq!(host.told(), Op::Add { songs: vec![wish], next: false });
    gus.until("the request gone", |r| r.jam_view().filter(|v| v.pending.is_empty()));

    // A guest cannot accept, and is told.
    gus.remote.clone().jam_act(Op::Request { song: Song { id: "s4".into(), ..Default::default() } });
    let pending = gus.until("Gus's second request", |r| r.jam_view().and_then(|v| v.pending.first().cloned()));
    gus.remote.clone().jam_act(Op::Decide { request: pending.request, accept: true });
    assert_eq!(gus.until("the refusal", |r| r.jam_view().and_then(|v| v.refused)), Refusal::NotAllowed);

    let provider_asked: Vec<String> = relay.asked().into_iter().filter(|a| a.contains("ext-")).collect();
    assert!(provider_asked.is_empty(), "the relay never looked the provider song up: {provider_asked:?}");
    relay.close();
}

#[test]
fn nearby_devices_need_no_relay() {
    let relay = Relay::absent();
    let ann = |password: &str| ServerConfig { url: SERVER.into(), user: "ann".into(), password: password.into(), ..Default::default() };
    let announced = Arc::new(Announced::default());
    let phone = Device::found(&relay, ann("pw"), DeviceKind::Phone, "Phone", Some(announced.clone()));
    phone.playing(&["s1", "s2"], 0);
    phone.remote.clone().serve(true);
    let door = announced.0.lock().clone().expect("the door announced");
    let txt = || door.txt.clone();

    // Another account's device on the network does not list the door.
    let other = Device::new(&relay, ServerConfig { user: "bob".into(), ..ann("pw") }, DeviceKind::Desktop, "Bob");
    other.remote.clone().watch(true);
    other.remote.clone().lan_found(door.name.clone(), "127.0.0.1".into(), door.port, txt());
    assert!(other.remote.devices().is_empty());

    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    desk.remote.clone().watch(true);
    desk.remote.clone().lan_found(door.name.clone(), "127.0.0.1".into(), door.port, txt());
    let seen = desk.until("the phone nearby", |r| r.devices().into_iter().find(|d| d.state.is_some()));
    assert!(seen.nearby && seen.name == "Phone");
    desk.remote.send(seen.id.clone(), Op::Pause);
    assert_eq!(phone.told(), Op::Pause);
    let through_relay = format!("noriRemote.send  {}", desk.remote.id());
    assert!(!relay.asked().contains(&through_relay), "the command went to the door, not the relay");

    // A device of the same user with an old password is not shown the door.
    let stranger = Device::new(&relay, ann("old"), DeviceKind::Desktop, "Old");
    stranger.remote.clone().watch(true);
    stranger.remote.clone().lan_found(door.name.clone(), "127.0.0.1".into(), door.port, txt());
    assert!(stranger.remote.devices().is_empty(), "the announced account tag differs");
}

#[test]
fn a_server_without_the_relay_is_asked_once() {
    let relay = Relay::absent();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    phone.until("the answer", |r| (r.relay() == RelaySupport::Unsupported).then_some(()));
    let asked = || relay.asked().iter().filter(|a| a.starts_with("noriRemote.")).count();
    assert_eq!(asked(), 1, "one probe");

    // Serving, watching and playing go on without the relay; a jam is refused before anything is asked.
    phone.playing(&["s1"], 0);
    phone.remote.clone().watch(true);
    phone.remote.clone().serve(true);
    phone.remote.clone().watch(false);
    phone.remote.clone().serve(false);
    phone.remote.send("elsewhere".into(), Op::Pause);
    assert!(block_on(phone.remote.clone().jam_open()).is_err());
    assert_eq!(asked(), 1, "nothing more asked of a server that has no relay");
    assert!(phone.remote.jam_view().is_none());
}
