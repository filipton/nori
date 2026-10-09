//! Remote control and jams through the core's public calls, against a relay kept in memory that answers
//! as octo-fiesta's hub does (rooms, held polls, jam invites, guests acting with the host's rights): two
//! devices of one account controlling each other, and a jam with a host, an admin and a guest.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use nori_core::client::{Client, NetProfile, Starrable};
use nori_core::library::StarsShown;
use nori_core::remote::{jam_join, Discovery, Playing, RelaySupport, Remote, RemoteMe, RemotePlayer, RemoteShown};
use nori_core::transport::{block_on, Exchange, FailureKind, Transport, TransportError, TransportResponse};
use nori_core::{Core, ServerConfig, Song};
use nori_remote::clock;
use nori_remote::wire::{Answer, Body, DeviceKind, Event, Member, Op, Outgoing, Refusal, Role, Room};
use parking_lot::Mutex;

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
    /// The seq of its last change: a held poll wakes only for a room it listens to.
    touched: u64,
}

/// Marks room `room` changed at `seq`.
fn room_touched(hub: &mut Hub, room: &str, seq: u64) {
    if let Some(r) = hub.rooms.get_mut(room) {
        r.touched = seq;
    }
}

/// The rooms a poll by `caller` from device `dev` listens to: its account's and the jams it hosts, or a
/// guest's jam.
fn listened(hub: &Hub, caller: &Caller, dev: &str) -> Vec<String> {
    match caller {
        Caller::Account(user) => {
            let hosted = hub.rooms.iter().filter(|(_, r)| r.host.as_ref().is_some_and(|(u, d)| u == user && d == dev)).map(|(id, _)| id.clone());
            std::iter::once(format!("u:{user}")).chain(hosted).collect()
        }
        Caller::Guest(room, _) => vec![room.clone()],
        Caller::Invited(_) => Vec::new(),
    }
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
    /// Held polls waiting for news; dropping one gives it up, as a cancelled request.
    waiting: Mutex<Vec<Waker>>,
    next: AtomicU64,
    /// A plain Navidrome: no `noriRemote.*` at all.
    absent: bool,
    /// A device whose clock reads this much ahead (µs), and whose sends reach the relay late: see
    /// [`Relay::lagging`].
    lagging: Mutex<Option<(String, i64)>>,
    /// Sends of the lagging device so far.
    lagged: AtomicU64,
    /// A door's poll answers come back 250 ms late.
    door_late: std::sync::atomic::AtomicBool,
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
        Arc::new(Relay { hub: Mutex::default(), waiting: Mutex::default(), next: AtomicU64::new(1), absent: false, lagging: Mutex::new(None), lagged: AtomicU64::new(0), door_late: Default::default() })
    }

    fn absent() -> Arc<Relay> {
        Arc::new(Relay { absent: true, ..Arc::into_inner(Relay::new()).unwrap() })
    }

    /// Device `dev`'s times read `skew_us` ahead as the relay passes them on. Its states reach the relay
    /// 250 ms late; its other sends every other one at once, the rest 60 to 170 ms late.
    fn lag(&self, dev: &str, skew_us: i64) {
        *self.lagging.lock() = Some((dev.to_string(), skew_us));
    }

    /// The lagging device's send `body`, as it reaches the relay (late) and with its clock read ahead.
    fn lagged(&self, dev: &str, body: Option<String>) -> Option<String> {
        let Some(skew) = self.lagging.lock().as_ref().filter(|(d, _)| d == dev).map(|(_, s)| *s) else { return body };
        let mut out: Outgoing = serde_json::from_str(body.as_deref()?).unwrap();
        let k = self.lagged.fetch_add(1, Ordering::Relaxed);
        let late = if out.state.is_some() { 250 } else if k % 2 == 1 { 0 } else { 60 + k * 53 % 110 };
        std::thread::sleep(Duration::from_millis(late));
        if let Some(at) = out.state.as_mut().and_then(|s| s.at_us.as_mut()) {
            *at += skew;
        }
        if let Some(Body::Clock { t2, t3, .. }) = &mut out.body {
            *t2 += skew;
            *t3 += skew;
        }
        Some(serde_json::to_string(&out).unwrap())
    }

    fn key(&self) -> String {
        format!("k{}", self.next.fetch_add(1, Ordering::Relaxed))
    }

    fn asked(&self) -> Vec<String> {
        self.hub.lock().asked.clone()
    }

    fn close(&self) {
        let mut hub = self.hub.lock();
        hub.closed = true;
        self.wake();
    }

    /// Wakes the held polls; called with the hub locked.
    fn wake(&self) {
        self.waiting.lock().drain(..).for_each(Waker::wake);
    }

    /// An account device's poll says whether it serves: it joins or leaves the account's room as the poll
    /// arrives, before any hold.
    fn arrived(&self, p: &HashMap<String, String>) {
        let (Some(user), Some(dev), false) = (p.get("u"), p.get("dev"), self.absent) else { return };
        let mut hub = self.hub.lock();
        let room = hub.rooms.entry(format!("u:{user}")).or_default();
        let listed = room.members.iter().position(|m| m.id == *dev);
        match (listed, p.get("serve").map(String::as_str) == Some("1")) {
            (None, true) => {
                let kind = serde_json::from_value(serde_json::Value::String(p["kind"].clone())).unwrap_or_default();
                room.members.push(Member { id: dev.clone(), name: p["name"].clone(), kind, state: None });
            }
            (Some(at), false) => {
                room.members.remove(at);
            }
            _ => return,
        }
        hub.seq += 1;
        let seq = hub.seq;
        room_touched(&mut hub, &format!("u:{user}"), seq);
        self.wake();
    }

    /// A held poll's wait: until there is news after `since` in one of `rooms`, or the relay closed.
    async fn news(&self, since: u64, rooms: &[String]) {
        std::future::poll_fn(|cx| {
            let hub = self.hub.lock();
            if rooms.iter().any(|r| hub.rooms.get(r).is_some_and(|s| s.touched > since)) || hub.closed {
                return Poll::Ready(());
            }
            self.waiting.lock().push(cx.waker().clone());
            Poll::Pending
        })
        .await
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
        let bump = |hub: &mut Hub, room: &str| {
            hub.seq += 1;
            let seq = hub.seq;
            room_touched(hub, room, seq);
            self.wake();
        };
        match (endpoint, &caller) {
            ("noriRemote.poll", _) => {
                let me = match &caller {
                    Caller::Account(_) => dev.clone(),
                    Caller::Guest(_, member) => member.clone(),
                    Caller::Invited(_) => unreachable!(),
                };
                let rooms = listened(&hub, &caller, &dev);
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
                let state = hub.rooms.entry(room.clone()).or_default();
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
                bump(&mut hub, &room);
                json(serde_json::json!({ "seq": seq }))
            }
            ("noriRemote.open", Caller::Account(user)) => {
                let (room, invite) = (format!("j{}", self.key()), self.key());
                let host = Member { id: dev.clone(), name: p["name"].clone(), kind: DeviceKind::Phone, state: None };
                hub.rooms.insert(room.clone(), RoomState { jam: true, host: Some((user.clone(), dev)), members: vec![host], ..Default::default() });
                hub.keys.insert(invite.clone(), (room.clone(), None));
                bump(&mut hub, &room);
                json(serde_json::json!({ "room": room, "invite": invite }))
            }
            ("noriRemote.join", Caller::Invited(room)) => {
                let (member, key) = (format!("m{}", self.key()), self.key());
                hub.keys.insert(key.clone(), (room.clone(), Some(member.clone())));
                hub.rooms.get_mut(room).unwrap().members.push(Member { id: member.clone(), name: p["name"].clone(), kind: DeviceKind::Guest, state: None });
                bump(&mut hub, room);
                json(serde_json::json!({ "room": room, "member": member, "key": key }))
            }
            ("noriRemote.kick", Caller::Account(_)) => {
                let (room, member) = (p["room"].clone(), p["member"].clone());
                hub.keys.retain(|_, (r, m)| !(*r == room && m.as_deref() == Some(member.as_str())));
                hub.rooms.get_mut(&room).unwrap().members.retain(|m| m.id != member);
                bump(&mut hub, &room);
                json(serde_json::json!({}))
            }
            ("ping" | "star" | "unstar", _) => br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#.to_vec(),
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
            self.hub.lock().asked.push(format!("lan {}", request.url));
            let late = self.door_late.load(Ordering::Relaxed) && request.url.contains("noriRemote.poll");
            return off_thread(move || {
                let answer = lan_exchange(&request);
                if late {
                    std::thread::sleep(Duration::from_millis(250));
                }
                answer
            })
            .await;
        }
        let Some(rest) = request.url.strip_prefix(&format!("{SERVER}/rest/")) else {
            return Err(TransportError::Failed { kind: FailureKind::Connect, detail: Some("unreachable".into()) });
        };
        let (endpoint, query) = rest.split_once('?').unwrap_or((rest, ""));
        let params: HashMap<String, String> = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), decode(v))).collect();
        if endpoint == "noriRemote.poll" {
            self.arrived(&params);
            if let (Some(since), Some("1")) = (params.get("since").and_then(|s| s.parse().ok()), params.get("hold").map(String::as_str)) {
                // The rooms it listens to as it arrives: a jam opened meanwhile is not one of them.
                let rooms = self.caller(endpoint, &params).map(|c| listened(&self.hub.lock(), &c, params.get("dev").map_or("", String::as_str))).unwrap_or_default();
                self.news(since, &rooms).await;
            }
        }
        let json = match endpoint {
            "noriRemote.send" => self.lagged(params.get("dev").map_or("", String::as_str), request.json),
            _ => request.json,
        };
        Ok(TransportResponse { status: 200, body: self.answer(endpoint, &params, json.as_deref()) })
    }

    fn address_changed(&self) {}

    fn network(&self) -> nori_core::transport::Network {
        nori_core::transport::Network::Unmetered
    }
}

/// `f` on a thread of its own, awaited as a request is: dropping the future gives it up.
async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let slot = Arc::new(Mutex::new((None, None::<Waker>)));
    let done = slot.clone();
    std::thread::spawn(move || {
        let v = f();
        let mut d = done.lock();
        d.0 = Some(v);
        if let Some(w) = d.1.take() {
            w.wake();
        }
    });
    std::future::poll_fn(|cx| {
        let mut s = slot.lock();
        match s.0.take() {
            Some(v) => Poll::Ready(v),
            None => {
                s.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    })
    .await
}

/// One HTTP/1.1 exchange with a door on this machine; refused where no door listens.
fn lan_exchange(request: &Exchange) -> Result<TransportResponse, TransportError> {
    use std::io::{BufRead, BufReader, Read, Write};
    let rest = request.url.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, target) = rest.split_at(rest.find('/').unwrap());
    let refused = |_| TransportError::Failed { kind: FailureKind::Connect, detail: Some("refused".into()) };
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).map_err(refused)?;
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
    Ok(TransportResponse { status, body })
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

struct NoMarks;

impl StarsShown for NoMarks {
    fn marks(&self, _: nori_core::stars::StarMarks) {}
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
    client: Arc<Client>,
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
        let remote = Remote::new(client.clone(), RemoteMe { name: name.into(), kind }, Arc::new(Player(Mutex::new(ops_to))), Arc::new(Shown(Mutex::new(news_to))), discovery);
        Device { core, client, remote, ops, news }
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
        self.remote.clone().played(Playing { playing: true, position_ms: 5_000, index: None, volume: Some(40), ..Default::default() });
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
    phone.remote.clone().played(Playing { playing: true, position_ms: 0, index: Some(2), volume: Some(40), ..Default::default() });
    let moved = desk.until("the next song", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state).filter(|s| s.index == Some(2)));
    assert_eq!(moved.entries.iter().find(|e| Some(e.index) == moved.index).map(|e| e.title.as_str()), Some("S3"));

    // An edit made against a queue that changed since is refused, and the controller is told.
    let stale = state.rev;
    phone.core.session.remove(2, 3);
    phone.remote.clone().played(Playing { playing: true, position_ms: 6_000, index: None, volume: Some(40), ..Default::default() });
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
        Op::Replace { songs, index, position_ms, play, .. } => {
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
    let added = host.remote.jam_added();
    assert_eq!(added.get("ext-deezer-song-9").map(String::as_str), Some("Gus"), "added by who asked, not who accepted");
    assert_eq!(added.get("s5").map(String::as_str), Some("Dee"));
    assert_eq!(added.len(), 2, "the host's own songs are nobody's: {added:?}");

    // A guest cannot accept, and is told.
    gus.remote.clone().jam_act(Op::Request { song: Song { id: "s4".into(), ..Default::default() } });
    let pending = gus.until("Gus's second request", |r| r.jam_view().and_then(|v| v.pending.first().cloned()));
    gus.remote.clone().jam_act(Op::Decide { request: pending.request, accept: true });
    assert_eq!(gus.until("the refusal", |r| r.jam_view().and_then(|v| v.refused)), Refusal::NotAllowed);

    host.remote.clone().jam_close();
    assert!(host.remote.jam_added().is_empty(), "no jam, no names");
    assert!(host.remote.jam_view().is_none(), "the host is in no jam once it ended it");

    let provider_asked: Vec<String> = relay.asked().into_iter().filter(|a| a.contains("ext-")).collect();
    assert!(provider_asked.is_empty(), "the relay never looked the provider song up: {provider_asked:?}");
    relay.close();
}

#[test]
fn a_jam_opened_while_a_poll_is_held_hears_its_first_request() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    host.playing(&["s1"], 0);
    // The devices sheet is open: a poll of the account's room is held when the jam opens from it.
    host.remote.clone().watch(true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while relay.waiting.lock().is_empty() {
        assert!(Instant::now() < deadline, "the held poll");
        std::thread::yield_now();
    }
    let link = block_on(host.remote.clone().jam_open()).unwrap();

    // A guest asks at once, before anything else happens in the account's room.
    let pass = block_on(jam_join(relay.clone(), link, "Gus".into())).unwrap();
    let gus = Device::new(&relay, ServerConfig { url: pass.url, api_key: Some(pass.api_key), ..Default::default() }, DeviceKind::Guest, "Gus");
    gus.remote.clone().watch(true);
    gus.until("the host's state", |r| r.jam_view().filter(|v| v.queue.is_some()));
    gus.remote.clone().jam_act(Op::Request { song: Song { id: "s2".into(), title: "S2".into(), ..Default::default() } });
    let asked = host.until("Gus's request", |r| r.jam_view().and_then(|v| v.pending.first().cloned()));
    assert_eq!((asked.from_name.as_str(), asked.song.id.as_str()), ("Gus", "s2"));
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

    // Mirrored with the picker closed: its door is still followed, and its whole queue read through it.
    desk.remote.clone().watch(false);
    desk.remote.clone().pick(Some(seen.id.clone()));
    phone.remote.clone().played(Playing { playing: false, position_ms: 9_000, index: None, volume: None, ..Default::default() });
    desk.until("the phone's newer word", |r| r.active().filter(|m| m.position_ms == 9_000 && m.rows.len() == 2));

    // The door seen again (mDNS reports it once per network interface) is still followed by the one poller.
    let first_polls = || relay.asked().iter().filter(|a| a.starts_with("lan ") && a.contains("noriRemote.poll") && !a.contains("since=")).count();
    let before = first_polls();
    for _ in 0..3 {
        desk.remote.clone().lan_found(door.name.clone(), "127.0.0.1".into(), door.port, txt());
    }
    phone.remote.clone().played(Playing { playing: true, position_ms: 1_000, index: None, volume: None, ..Default::default() });
    desk.until("the phone playing", |r| r.active().filter(|m| m.playing));
    assert_eq!(first_polls(), before, "no poller started over");

    // Seen at an address of this machine's no door answers at (another network interface): the phone is
    // still followed where it answers.
    let nowhere = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    desk.remote.clone().lan_found(door.name.clone(), "127.0.0.1".into(), nowhere, txt());
    phone.remote.clone().played(Playing { playing: false, position_ms: 3_000, index: None, volume: None, ..Default::default() });
    desk.until("the phone paused", |r| r.active().filter(|m| !m.playing));
    phone.remote.clone().played(Playing { playing: true, position_ms: 4_000, index: None, volume: None, ..Default::default() });
    desk.until("the phone playing again", |r| r.active().filter(|m| m.playing));

    // Its word comes late through the door; its clock, learned there at once, says when it was heard.
    relay.door_late.store(true, Ordering::Relaxed);
    let said = clock::now_us();
    phone.remote.clone().played(Playing { playing: true, position_ms: 12_000, index: None, volume: None, ..Default::default() });
    let m = desk.until("the phone's place", |r| r.active().filter(|m| m.playing && m.position_ms >= 12_000));
    let now = clock::now_us();
    let off = m.position_at(now) - heard_at(12_000, said, now);
    assert!(off.abs() <= 5, "the desk shows the phone {off} ms off what is heard there");
    relay.door_late.store(false, Ordering::Relaxed);

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

impl Device {
    /// Polls this device has asked of the relay.
    fn polls(&self, relay: &Relay) -> usize {
        relay.asked().iter().filter(|a| a.starts_with("noriRemote.poll") && a.ends_with(&self.remote.id())).count()
    }
}

#[test]
fn a_device_playing_elsewhere_is_mirrored_whole() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    let ids: Vec<String> = (0..130).map(|n| format!("s{n}")).collect();
    phone.playing(&ids.iter().map(String::as_str).collect::<Vec<_>>(), 60);
    phone.core.session.shuffle(true);
    phone.remote.clone().played(Playing { playing: true, position_ms: 5_000, index: None, volume: Some(40), ..Default::default() });
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();

    // Nothing queued on the desk: picking the phone only follows it, without a picker open.
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().watch(false);
    desk.remote.clone().pick(Some(phone_id.clone()));
    let order: Vec<u32> = phone.core.session.playlist(|p| p.play_order().map(|i| i as u32).collect());
    let m = desk.until("the whole queue", |r| r.active().filter(|m| m.rows.len() == 130));
    assert_eq!(m.rows.iter().map(|r| r.index).collect::<Vec<_>>(), order, "in play order, by list index");
    assert_eq!(m.at.map(|a| m.rows[a as usize].song.id.clone()), Some("s60".to_string()));
    assert!(m.shuffle && m.playing && m.len == 130);
    assert!(phone.ops.try_recv().is_err(), "pages are answered by the core, not the player");

    // A command shows at once, before the phone's own word.
    desk.remote.send(phone_id.clone(), Op::Pause);
    let m = desk.remote.active().unwrap();
    assert!(!m.playing);
    assert_eq!(phone.told(), Op::Pause);
    desk.remote.send(phone_id.clone(), Op::Volume { percent: 15 });
    desk.remote.send(phone_id.clone(), Op::Star { id: "s60".into(), on: true });
    let m = desk.remote.active().unwrap();
    assert_eq!(m.volume, Some(15));
    assert!(m.rows[m.at.unwrap() as usize].song.starred);
    assert_eq!((phone.told(), phone.told()), (Op::Volume { percent: 15 }, Op::Star { id: "s60".into(), on: true }));
    // The phone's player stars it, as its own heart would: once, on the server.
    block_on(phone.client.star(Starrable::Song, "s60".into(), true, Arc::new(NoMarks))).unwrap();
    assert_eq!(relay.asked().iter().filter(|a| a.starts_with("star ")).count(), 1);

    // The phone's next word corrects what was foreseen, and keeps the heart its record does not have yet.
    phone.remote.clone().played(Playing { playing: false, position_ms: 7_000, index: None, volume: Some(30), ..Default::default() });
    let m = desk.until("the phone's volume", |r| r.active().filter(|m| m.volume == Some(30) && m.position_ms == 7_000));
    assert!(m.rows[m.at.unwrap() as usize].song.starred);
    // Its own keys move it too.
    phone.remote.clone().volume_changed(Some(22));
    desk.until("the phone's keys", |r| r.active().filter(|m| m.volume == Some(22) && m.position_ms == 7_000));

    // Gone: no longer followed, and nothing more is asked of the relay for it.
    phone.remote.clone().serve(false);
    desk.until("the phone gone", |r| r.active().is_none().then_some(()));
    let asked = desk.polls(&relay);
    let other = Device::account(&relay, DeviceKind::Phone, "Other");
    other.playing(&["s1"], 0);
    other.remote.clone().serve(true);
    other.until("serving", |_| (relay.asked().iter().filter(|a| a.ends_with(&other.remote.id())).count() >= 2).then_some(()));
    assert_eq!(desk.polls(&relay), asked, "the desk stopped polling with nothing to follow");
    relay.close();
}

#[test]
fn a_mirrored_queue_is_cleared_and_a_removed_song_put_back_in_its_place() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1", "s2", "s3", "s4", "s5"], 1);
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().pick(Some(phone_id.clone()));
    let m = desk.until("the phone mirrored", |r| r.active().filter(|m| m.rows.len() == 5));

    // The third song taken out and put back: it goes back where it was, not after the song playing.
    desk.remote.send(phone_id.clone(), Op::Remove { index: 2, rev: m.rev });
    assert_eq!(phone.told(), Op::Remove { index: 2, rev: m.rev });
    phone.core.session.remove(2, 3);
    assert!(!desk.remote.put_back(phone_id.clone(), "s4".into()), "only a song taken out from here");
    assert!(desk.remote.put_back(phone_id.clone(), "s3".into()));
    match phone.told() {
        Op::Restore { song, index } => {
            assert_eq!((song.id.as_str(), song.title.as_str(), index), ("s3", "S3", 2));
            assert!(phone.core.session.restore(song.id).at.is_some(), "the phone's own undo knows its place");
        }
        op => panic!("{op:?}"),
    }
    assert_eq!(phone.core.session.playlist(|p| p.ids().to_vec()), ["s1", "s2", "s3", "s4", "s5"]);
    assert!(!desk.remote.put_back(phone_id.clone(), "s3".into()), "put back once");

    // Clear: the phone's player removes what plays after the current song, from the end.
    desk.remote.send(phone_id.clone(), Op::Clear);
    let removed: Vec<u32> = (0..3).map(|_| match phone.told() {
        Op::Remove { index, .. } => index,
        op => panic!("{op:?}"),
    }).collect();
    assert_eq!(removed, [4, 3, 2]);
    assert!(phone.ops.recv_timeout(Duration::from_millis(300)).is_err(), "the song playing stays");
    relay.close();
}

#[test]
fn a_transfer_keeps_the_play_order_shuffle_and_repeat() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1", "s2", "s3", "s4", "s5"], 2);
    phone.core.session.shuffle(true);
    phone.core.session.repeat(2);
    phone.remote.clone().serve(true);
    desk.remote.clone().serve(true);
    let (phone_id, desk_id) = (phone.remote.id(), desk.remote.id());
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    // The phone follows only a device it lists.
    phone.until("the desk", |r| r.devices().into_iter().find(|d| d.id == desk_id).and_then(|d| d.state));
    desk.remote.clone().pick(Some(phone_id.clone()));
    desk.until("the phone mirrored", |r| r.active());

    // "This device": the phone hands its queue over as it plays, and follows the desk.
    desk.remote.clone().pick(None);
    let order: Vec<u32> = phone.core.session.playlist(|p| p.play_order().map(|i| i as u32).collect());
    match desk.told() {
        Op::Replace { songs, index, play, order: sent, shuffle, repeat, .. } => {
            assert_eq!(songs.len(), 5);
            assert_eq!((index, play, shuffle, repeat), (2, true, true, 2));
            assert_eq!(sent, Some(order));
        }
        op => panic!("{op:?}"),
    }
    assert_eq!(phone.told(), Op::Pause);
    desk.until("playing here", |r| r.active().is_none().then_some(()));
    let m = phone.until("the desk mirrored", |r| r.active());
    assert_eq!(m.id, desk_id);
    relay.close();
}

#[test]
fn controllers_follow_playback_to_where_it_was_handed() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    let tablet = Device::account(&relay, DeviceKind::Phone, "Tablet");
    phone.playing(&["s1", "s2"], 0);
    for d in [&phone, &desk] {
        d.remote.clone().serve(true);
    }
    let (phone_id, desk_id) = (phone.remote.id(), desk.remote.id());
    tablet.remote.clone().watch(true);
    tablet.until("both", |r| (r.devices().iter().filter(|d| d.state.is_some()).count() == 2).then_some(()));
    // A device hears commands from its first answer on: the desk has had one once it lists the phone.
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    tablet.remote.clone().pick(Some(phone_id.clone()));
    tablet.until("the phone mirrored", |r| r.active().filter(|m| m.id == phone_id));

    // Moved on from the tablet: the phone hands over to the desk, and the tablet follows the desk.
    tablet.remote.clone().pick(Some(desk_id.clone()));
    assert_eq!(phone.told(), Op::Pause);
    assert!(matches!(desk.told(), Op::Replace { play: true, .. }));
    assert_eq!(tablet.until("the desk mirrored", |r| r.active()).id, desk_id);

    // The desk handing its playback on by itself is followed too.
    let elsewhere = Device::account(&relay, DeviceKind::Desktop, "Elsewhere");
    elsewhere.remote.clone().serve(true);
    let elsewhere_id = elsewhere.remote.id();
    desk.remote.clone().watch(true);
    desk.until("elsewhere", |r| r.devices().into_iter().find(|d| d.id == elsewhere_id && d.state.is_some()));
    desk.playing(&["s1", "s2"], 1);
    desk.remote.clone().hand_over(elsewhere_id.clone());
    assert_eq!(tablet.until("followed on", |r| r.active().filter(|m| m.id == elsewhere_id)).name, "Elsewhere");
    relay.close();
}

/// Where the device's listener is at `now_us`, playing on from `position_ms` said at `said_us`.
fn heard_at(position_ms: i64, said_us: i64, now_us: i64) -> i64 {
    position_ms + (now_us - said_us) / 1000
}

#[test]
fn a_mirrored_playhead_is_where_the_device_is_heard() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    // The phone's clock reads seven seconds ahead of the desk's, and its word reaches the relay late.
    relay.lag(&phone.remote.id(), 7_000_000);
    phone.playing(&["s1", "s2"], 0);
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().pick(Some(phone_id.clone()));
    desk.until("the phone mirrored", |r| r.active());

    let said = clock::now_us();
    phone.remote.clone().played(Playing { playing: true, position_ms: 30_000, index: None, volume: Some(40), ..Default::default() });
    desk.until("the phone's new place", |r| r.active().filter(|m| m.position_ms >= 30_000));
    // The burst of time exchanges is over within three seconds.
    let settled = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settled {
        let _ = desk.news.recv_timeout(Duration::from_millis(100));
    }
    let m = desk.remote.active().expect("still mirrored");
    let now = clock::now_us();
    let off = m.position_at(now) - heard_at(30_000, said, now);
    assert!(off.abs() <= 10, "the desk shows the phone {off} ms off what is heard there");
    relay.close();
}

#[test]
fn a_stopped_remote_lets_go_of_everything() {
    let relay = Relay::new();
    let announced = Arc::new(Announced::default());
    let ann = ServerConfig { url: SERVER.into(), user: "ann".into(), password: "pw".into(), ..Default::default() };
    let phone = Device::found(&relay, ann, DeviceKind::Phone, "Phone", Some(announced.clone()));
    phone.playing(&["s1", "s2"], 0);
    phone.remote.clone().serve(true);
    let door = announced.0.lock().clone().expect("the door announced");
    let phone_id = phone.remote.id();

    // The desk mirrors the phone through the relay and its door, learning its clock there.
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().lan_found(door.name.clone(), "127.0.0.1".into(), door.port, door.txt.clone());
    desk.until("the phone nearby", |r| r.devices().into_iter().find(|d| d.id == phone_id && d.nearby));
    desk.remote.clone().watch(false);
    desk.remote.clone().pick(Some(phone_id.clone()));
    desk.until("the phone mirrored", |r| r.active());
    let desk_id = desk.remote.id();
    let asked = || relay.asked().into_iter().filter(|a| (a.starts_with("noriRemote.poll") && a.ends_with(&desk_id)) || (a.starts_with("lan ") && a.contains(&format!("dev={desk_id}")) && !a.contains("noriRemote.send"))).collect::<Vec<_>>();
    desk.until("a time exchange through the door", |_| asked().iter().any(|a| a.contains("noriRemote.time")).then_some(()));
    desk.until("a held poll at the door", |_| asked().iter().any(|a| a.contains("since=")).then_some(()));

    // Its held polls are given up at once, not waited out.
    let (done_to, done) = channel();
    let remote = desk.remote.clone();
    std::thread::spawn(move || {
        remote.stop();
        let _ = done_to.send(());
    });
    assert!(done.recv_timeout(Duration::from_secs(10)).is_ok(), "stopped without waiting out a held poll");
    assert!(desk.remote.active().is_none(), "the player plays here again");
    assert_eq!(Arc::strong_count(&desk.remote), 1, "no thread of the remote runs");

    // News from the phone reaches another device, and nothing more is asked for the desk.
    let before = asked();
    let tablet = Device::account(&relay, DeviceKind::Phone, "Tablet");
    tablet.remote.clone().watch(true);
    phone.remote.clone().played(Playing { playing: false, position_ms: 20_000, index: None, volume: None, ..Default::default() });
    tablet.until("the phone's news", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state).filter(|s| s.position_ms == 20_000));
    assert_eq!(asked(), before, "no poll or time exchange after the stop");
    relay.close();
}

#[derive(Clone, Copy, Debug)]
enum Did {
    /// Plays a queue of its own.
    Played,
    Paused,
    /// Has a queue it has not played.
    Queued,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Picked {
    /// The desk hands its queue over and pauses.
    Transfers,
    /// The desk only follows the phone.
    Mirrors,
}

impl Device {
    /// Has songs `ids` queued, paused at the first.
    fn queued(&self, ids: &[&str]) {
        self.core.session.register(ids.iter().map(|id| Song { id: id.to_string(), duration: 200, ..Default::default() }).collect());
        self.core.session.set(ids.iter().map(|s| s.to_string()).collect(), Some(0), false, None);
        self.remote.clone().played(Playing { playing: false, position_ms: 6_000, ..Default::default() });
    }
}

/// Picking the phone from the desk moves the active device's playback there: the desk's when the desk
/// is the active device, none when the phone is.
#[test]
fn picking_a_device_moves_the_active_ones_playback() {
    use Did::*;
    /// (on the desk, what it did), in order.
    type Steps = &'static [(bool, Did)];
    let cases: &[(&str, Steps, Picked)] = &[
        ("the phone plays", &[(true, Played), (true, Paused), (false, Played)], Picked::Mirrors),
        ("the desk plays", &[(false, Played), (false, Paused), (true, Played)], Picked::Transfers),
        ("the desk played last", &[(false, Played), (false, Paused), (true, Played), (true, Paused)], Picked::Transfers),
        ("the phone played last", &[(true, Played), (true, Paused), (false, Played), (false, Paused)], Picked::Mirrors),
        ("the phone is idle", &[(true, Played), (true, Paused)], Picked::Transfers),
        ("neither played", &[(true, Queued), (false, Queued)], Picked::Mirrors),
    ];
    for (case, steps, want) in cases {
        let relay = Relay::new();
        let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
        let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
        phone.remote.clone().serve(true);
        desk.remote.clone().serve(true);
        let phone_id = phone.remote.id();
        let heard = |what: &str, ok: &dyn Fn(&nori_remote::wire::DeviceState) -> bool| {
            desk.until(&format!("{case}: {what}"), |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state).filter(|s| ok(s)));
        };
        for &(on_desk, did) in steps.iter() {
            let (d, ids) = if on_desk { (&desk, ["d1", "d2"]) } else { (&phone, ["p1", "p2"]) };
            match did {
                Played => d.playing(&ids, 0),
                Paused => d.remote.clone().played(Playing { playing: false, position_ms: 6_000, ..Default::default() }),
                Queued => d.queued(&ids),
            }
            if !on_desk {
                // The desk hears each of the phone's states.
                let playing = matches!(did, Played);
                heard("the phone's state", &|s| s.playing == playing && s.index.is_some());
            }
        }
        heard("the phone", &|_| true);
        desk.remote.clone().pick(Some(phone_id.clone()));
        let picked = if desk.ops.try_recv() == Ok(Op::Pause) { Picked::Transfers } else { Picked::Mirrors };
        assert_eq!(picked, *want, "{case}");
        assert_eq!(desk.until(case, |r| r.active()).id, phone_id, "{case}: followed");
        match want {
            Picked::Transfers => {
                assert!(matches!(phone.told(), Op::Replace { songs, .. } if songs[0].id == "d1"), "{case}: the desk's queue goes");
                phone.queued(&["d1", "d2"]);
                heard("the queue there", &|s| s.entries.first().is_some_and(|e| e.id == "d1"));
                // Picked back, the queue comes here again.
                desk.remote.clone().pick(None);
                assert!(matches!(desk.told(), Op::Replace { songs, .. } if songs[0].id == "d1"), "{case}: pulled back");
            }
            Picked::Mirrors => assert!(phone.ops.recv_timeout(Duration::from_millis(300)).is_err(), "{case}: the phone's queue stays"),
        }
        relay.close();
    }
}
