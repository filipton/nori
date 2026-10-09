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
use nori_core::remote::{jam_join, Controls, JamJoin, JamStart, Discovery, Follower, JamControls, Lead, Listening, Playing, Reach, RelaySupport, Remote, RemoteMe, RemotePlayer, RemoteShown, Sight};
use nori_core::transport::{block_on, Exchange, FailureKind, Transport, TransportError, TransportResponse};
use nori_player::playlist::Hand;
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
    absent: std::sync::atomic::AtomicBool,
    /// A device whose clock reads this much ahead (µs), and whose sends reach the relay late: see
    /// [`Relay::lagging`].
    lagging: Mutex<Option<(String, i64, bool)>>,
    /// Sends of the lagging device so far.
    lagged: AtomicU64,
    /// A door's poll answers come back 250 ms late.
    door_late: std::sync::atomic::AtomicBool,
    /// A device's poll saying it no longer serves reaches the relay 250 ms late.
    leave_late: std::sync::atomic::AtomicBool,
    /// The server cannot be reached; held polls fail once woken.
    down: std::sync::atomic::AtomicBool,
    /// Lets jam guests stream the host's queue (listening along).
    along: std::sync::atomic::AtomicBool,
    /// Tells its time at `nori/time`; its clock reads [`SERVER_SKEW_US`] ahead.
    time: std::sync::atomic::AtomicBool,
    /// The lagging device's answers to time exchanges reach the relay this late, ms (0: as its other sends).
    answers_late: AtomicU64,
    /// Closing a jam goes unanswered: the request is lost on the way.
    lose_closes: std::sync::atomic::AtomicBool,
    /// Opening a jam takes a while, each one asked a little less than the one before, so their answers
    /// come back in the reverse order.
    opens_late: std::sync::atomic::AtomicBool,
    opens: AtomicU64,
}

const SERVER_SKEW_US: i64 = 9_000_000;

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
        Arc::new(Relay { hub: Mutex::default(), waiting: Mutex::default(), next: AtomicU64::new(1), absent: Default::default(), lagging: Mutex::new(None), lagged: AtomicU64::new(0), door_late: Default::default(), leave_late: Default::default(), down: Default::default(), along: std::sync::atomic::AtomicBool::new(true), time: Default::default(), answers_late: AtomicU64::new(0), lose_closes: Default::default(), opens_late: Default::default(), opens: AtomicU64::new(0) })
    }

    fn absent() -> Arc<Relay> {
        Arc::new(Relay { absent: true.into(), ..Arc::into_inner(Relay::new()).unwrap() })
    }

    /// Device `dev`'s times read `skew_us` ahead as the relay passes them on. Its states reach the relay
    /// 250 ms late; its other sends every other one at once, the rest 60 to 170 ms late.
    fn lag(&self, dev: &str, skew_us: i64) {
        *self.lagging.lock() = Some((dev.to_string(), skew_us, true));
    }

    /// Device `dev`'s times read `skew_us` ahead as the relay passes them on; its sends go at once.
    fn skew(&self, dev: &str, skew_us: i64) {
        *self.lagging.lock() = Some((dev.to_string(), skew_us, false));
    }

    /// The lagging device's send `body`, as it reaches the relay (late) and with its clock read ahead.
    fn lagged(&self, dev: &str, body: Option<String>) -> Option<String> {
        let Some((skew, slow)) = self.lagging.lock().as_ref().filter(|(d, ..)| d == dev).map(|(_, s, l)| (*s, *l)) else { return body };
        let mut out: Outgoing = serde_json::from_str(body.as_deref()?).unwrap();
        let k = self.lagged.fetch_add(1, Ordering::Relaxed);
        let answers = self.answers_late.load(Ordering::Relaxed);
        let late = match &out.body {
            _ if out.state.is_some() => 250,
            Some(Body::Clock { .. }) if answers > 0 => answers,
            _ if k % 2 == 1 => 0,
            _ => 60 + k * 53 % 110,
        };
        if slow {
            std::thread::sleep(Duration::from_millis(late));
        }
        if let Some(at) = out.state.as_mut().and_then(|s| s.at_us.as_mut()) {
            *at += skew;
        }
        // The relay's clock less the device's, as its own clock reads.
        if let Some(server) = out.state.as_mut().and_then(|s| s.server_us.as_mut()) {
            *server -= skew;
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

    /// How many times `endpoint` was asked.
    fn asked_for(&self, endpoint: &str) -> usize {
        self.hub.lock().asked.iter().filter(|a| a.split(' ').next() == Some(endpoint)).count()
    }

    /// The jams it keeps open.
    fn jams(&self) -> usize {
        self.hub.lock().rooms.values().filter(|r| r.jam).count()
    }

    fn close(&self) {
        let mut hub = self.hub.lock();
        hub.closed = true;
        self.wake();
    }

    /// Starts afresh, as after a restart: every room and key is gone, and held polls answer.
    fn restart(&self) {
        let mut hub = self.hub.lock();
        hub.rooms.clear();
        hub.keys.clear();
        self.wake();
    }

    /// The server cannot be reached (`down`), or can again.
    fn go_down(&self, down: bool) {
        let _hub = self.hub.lock();
        self.down.store(down, Ordering::Relaxed);
        self.wake();
    }

    /// Wakes the held polls; called with the hub locked.
    fn wake(&self) {
        self.waiting.lock().drain(..).for_each(Waker::wake);
    }

    /// An account device's poll says whether it serves: it joins or leaves the account's room as the poll
    /// arrives, before any hold.
    fn arrived(&self, p: &HashMap<String, String>) {
        let (Some(user), Some(dev), false) = (p.get("u"), p.get("dev"), self.absent.load(Ordering::Relaxed)) else { return };
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

    /// A held poll's wait: until there is news after `since` in one of `rooms`, or the relay closed or
    /// went down.
    async fn news(&self, since: u64, rooms: &[String]) {
        std::future::poll_fn(|cx| {
            let hub = self.hub.lock();
            if rooms.iter().any(|r| hub.rooms.get(r).is_none_or(|s| s.touched > since)) || hub.closed || self.down.load(Ordering::Relaxed) {
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
        if self.absent.load(Ordering::Relaxed) && endpoint.starts_with("noriRemote.") {
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
                    along: self.along.load(Ordering::Relaxed),
                    time: self.time.load(Ordering::Relaxed),
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
            ("noriRemote.close", Caller::Account(_)) => {
                let room = p["room"].clone();
                hub.rooms.remove(&room);
                hub.keys.retain(|_, (r, _)| *r != room);
                bump(&mut hub, &room);
                json(serde_json::json!({}))
            }
            ("noriRemote.leave", Caller::Guest(room, member)) => {
                let (room, member) = (room.clone(), member.clone());
                hub.keys.retain(|_, (r, m)| !(*r == room && m.as_deref() == Some(member.as_str())));
                hub.rooms.get_mut(&room).unwrap().members.retain(|m| m.id != member);
                bump(&mut hub, &room);
                json(serde_json::json!({}))
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
        let unreachable = || Err(TransportError::Failed { kind: FailureKind::Connect, detail: Some("unreachable".into()) });
        if let Some(t1) = request.url.strip_prefix(&format!("{SERVER}/nori/time?t1=")).filter(|_| self.time.load(Ordering::Relaxed)) {
            self.hub.lock().asked.push("nori/time".into());
            let now = clock::now_us() + SERVER_SKEW_US;
            let body = Body::Clock { t1: t1.parse().unwrap(), t2: now, t3: now };
            return Ok(TransportResponse { status: 200, body: json(body) });
        }
        let Some(rest) = request.url.strip_prefix(&format!("{SERVER}/rest/")).filter(|_| !self.down.load(Ordering::Relaxed)) else {
            return unreachable();
        };
        let (endpoint, query) = rest.split_once('?').unwrap_or((rest, ""));
        let params: HashMap<String, String> = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), decode(v))).collect();
        if endpoint == "noriRemote.close" && self.lose_closes.load(Ordering::Relaxed) {
            self.hub.lock().asked.push("lost close".into());
            return unreachable();
        }
        if endpoint == "noriRemote.open" && self.opens_late.load(Ordering::Relaxed) {
            let k = self.opens.fetch_add(1, Ordering::Relaxed);
            self.hub.lock().asked.push("open waits".into());
            std::thread::sleep(Duration::from_millis(300 - 50 * k.min(5)));
        }
        if endpoint == "noriRemote.poll" {
            if self.leave_late.load(Ordering::Relaxed) && params.get("serve").map(String::as_str) == Some("0") {
                std::thread::sleep(Duration::from_millis(250));
            }
            self.arrived(&params);
            if let (Some(since), Some("1")) = (params.get("since").and_then(|s| s.parse().ok()), params.get("hold").map(String::as_str)) {
                // The rooms it listens to as it arrives: a jam opened meanwhile is not one of them. A key
                // no longer known is refused at once.
                if let Ok(c) = self.caller(endpoint, &params) {
                    let rooms = listened(&self.hub.lock(), &c, params.get("dev").map_or("", String::as_str));
                    self.news(since, &rooms).await;
                }
                if self.down.load(Ordering::Relaxed) {
                    return unreachable();
                }
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

/// Counts the remote's change notices, so a test waits for news rather than for time, and keeps each
/// jam end it was told of (the host's name).
struct Shown(Mutex<Sender<()>>, Arc<Mutex<Vec<Option<String>>>>);

impl RemoteShown for Shown {
    fn changed(&self) {
        let _ = self.0.lock().send(());
    }

    fn jam_ended(&self, host: Option<String>) {
        self.1.lock().push(host);
        let _ = self.0.lock().send(());
    }
}

fn ann() -> ServerConfig {
    ServerConfig { url: SERVER.into(), user: "ann".into(), password: "pw".into(), ..Default::default() }
}

/// Opens a jam on `d`; its invite.
fn opened(d: &Device) -> String {
    block_on(d.remote.clone().jam_open()).unwrap().expect("a jam opened")
}

/// Joins the jam `link` invites to from a device of no account, as `name`; its pass.
fn joined(relay: &Arc<Relay>, link: String, name: String) -> nori_core::remote::JamPass {
    let settings = nori_core::settings_store::Settings::new();
    match block_on(jam_join(relay.clone(), settings, None, link, name)).unwrap() {
        JamJoin::Joined { pass } => pass,
        other => panic!("not joined: {other:?}"),
    }
}

/// Waits for `ready`, for what does not tell a device's change notices (the relay's own state).
fn eventually(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "waited for {what}");
        std::thread::yield_now();
    }
}

struct Device {
    core: Arc<Core>,
    client: Arc<Client>,
    remote: Arc<Remote>,
    ops: Receiver<Op>,
    news: Receiver<()>,
    ended: Arc<Mutex<Vec<Option<String>>>>,
}

impl Device {
    fn new(relay: &Arc<Relay>, config: ServerConfig, kind: DeviceKind, name: &str) -> Device {
        Device::found(relay, config, kind, name, None)
    }

    fn found(relay: &Arc<Relay>, config: ServerConfig, kind: DeviceKind, name: &str, discovery: Option<Arc<dyn Discovery>>) -> Device {
        Device::with(relay, config, kind, name, discovery, Default::default())
    }

    fn with(relay: &Arc<Relay>, config: ServerConfig, kind: DeviceKind, name: &str, discovery: Option<Arc<dyn Discovery>>, session: Arc<nori_queue::Session>) -> Device {
        let core = Core::new(String::new(), "remote".into(), session).unwrap();
        core.configure(config).unwrap();
        let client = Client::new(core.clone(), relay.clone(), Default::default());
        client.set_profile(NetProfile { url: SERVER.into(), ..Default::default() });
        let (ops_to, ops) = channel();
        let (news_to, news) = channel();
        let ended = Arc::new(Mutex::new(Vec::new()));
        let remote = Remote::new(client.clone(), RemoteMe { name: name.into(), kind }, Arc::new(Player(Mutex::new(ops_to))), Arc::new(Shown(Mutex::new(news_to), ended.clone())), discovery);
        Device { core, client, remote, ops, news, ended }
    }

    fn account(relay: &Arc<Relay>, kind: DeviceKind, name: &str) -> Device {
        Device::new(relay, ann(), kind, name)
    }

    /// An account's device whose app keeps its settings in `dir`, jams on; made again from the same
    /// `dir`, it is the same device after the app started again.
    fn kept(relay: &Arc<Relay>, dir: &nori_testdir::TempDir, kind: DeviceKind, name: &str) -> Device {
        let settings = nori_core::settings_store::Settings::new();
        let mut prefs = settings.open(&dir.path().join("app.db").to_string_lossy()).unwrap();
        prefs.jam = true;
        settings.put(prefs);
        Device::with(relay, ann(), kind, name, None, Arc::new(nori_queue::Session::new(settings)))
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

    desk.remote.clone().send(phone_id.clone(), Op::Next);
    assert_eq!(phone.told(), Op::Next);
    // The song the player says it arrived on is the one shown, before the queue's own current moves.
    phone.remote.clone().played(Playing { playing: true, position_ms: 0, index: Some(2), volume: Some(40), ..Default::default() });
    let moved = desk.until("the next song", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state).filter(|s| s.index == Some(2)));
    assert_eq!(moved.entries.iter().find(|e| Some(e.index) == moved.index).map(|e| e.title.as_str()), Some("S3"));

    // An edit made against a queue that changed since is refused, and the controller is told.
    let stale = state.rev;
    phone.core.session.remove(2, 3);
    phone.remote.clone().played(Playing { playing: true, position_ms: 6_000, index: None, volume: Some(40), ..Default::default() });
    desk.remote.clone().send(phone_id.clone(), Op::Remove { index: 0, rev: stale });
    let refused = desk.until("the refusal", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.refused));
    assert_eq!(refused, Refusal::Stale);
    assert!(phone.ops.try_recv().is_err(), "nothing done");
    let fresh = desk.until("the new queue", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state).filter(|s| s.rev != stale));
    desk.remote.clone().send(phone_id.clone(), Op::Remove { index: 0, rev: fresh.rev });
    assert_eq!(phone.told(), Op::Remove { index: 0, rev: fresh.rev });

    // Playing here: the phone hands over its queue and position, then pauses.
    desk.remote.clone().send(phone_id.clone(), Op::Transfer { to: desk.remote.id() });
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
    let link = opened(&host);
    assert!(link.starts_with("http://octo:5274/nori/jam#s=http%3A%2F%2Focto%3A5274&k="), "{link}");

    let guest_of = |name: &str| {
        let pass = joined(&relay, link.clone(), name.into());
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
    assert_eq!(host.told(), Op::Add { songs: vec![blue.clone()], next: false });

    // What the host plays, as a guest's player shows it: the song, the queue around it, who added each.
    host.core.session.register(vec![blue]);
    host.core.session.take(2, vec!["s5".into()], vec![Hand::Last]);
    host.remote.clone().played(Playing { playing: true, position_ms: 9_000, index: Some(0), ..Default::default() });
    let m = gus.until("the host's queue in Gus's player", |r| r.jam_playing().filter(|m| m.rows.len() == 3));
    assert_eq!(m.rows.iter().map(|r| r.song.id.as_str()).collect::<Vec<_>>(), ["s1", "s5", "s2"], "in play order: what was asked for plays next");
    assert_eq!((m.name.as_str(), m.at, m.playing, m.position_ms), ("Host", Some(0), true, 9_000));
    assert_eq!(gus.remote.jam_added().get("s5").map(String::as_str), Some("Dee"));
    assert!(host.remote.jam_playing().is_none(), "the host plays it itself");

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
    let link = opened(&host);

    // A guest asks at once, before anything else happens in the account's room.
    let pass = joined(&relay, link, "Gus".into());
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
    desk.remote.clone().send(seen.id.clone(), Op::Pause);
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
fn a_server_without_the_relay_is_asked_again_only_by_a_picker() {
    let relay = Relay::absent();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    phone.until("the answer", |r| (r.relay() == RelaySupport::Unsupported).then_some(()));
    let asked = || relay.asked().iter().filter(|a| a.starts_with("noriRemote.")).count();
    assert_eq!(asked(), 1, "one probe");

    // Serving, watching and playing go on without the relay; a jam is refused before anything is asked.
    phone.playing(&["s1"], 0);
    phone.remote.clone().watch(true);
    phone.until("the picker's probe", |_| (asked() == 2).then_some(()));
    phone.remote.clone().serve(true);
    phone.remote.clone().watch(false);
    phone.remote.clone().serve(false);
    phone.remote.clone().send("elsewhere".into(), Op::Pause);
    assert!(block_on(phone.remote.clone().jam_open()).is_err());
    assert_eq!(asked(), 2, "nothing more asked of a server that has no relay");
    assert!(phone.remote.jam_view().is_none());
}

#[test]
fn a_picker_opened_asks_again_whether_the_server_relays() {
    let relay = Relay::absent();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    phone.until("the answer", |r| (r.relay() == RelaySupport::Unsupported).then_some(()));
    let asked = || relay.asked().iter().filter(|a| a.starts_with("noriRemote.")).count();

    phone.remote.clone().watch(true);
    phone.until("asked again", |_| (asked() == 2).then_some(()));
    phone.remote.clone().watch(false);
    assert_eq!(phone.remote.relay(), RelaySupport::Unsupported);

    // The server gained the relay: the next opening finds it, and the picker follows the account's devices.
    relay.absent.store(false, Ordering::Relaxed);
    phone.remote.clone().watch(true);
    phone.until("the relay", |r| (r.relay() == RelaySupport::Supported).then_some(()));
    phone.until("polling", |_| (phone.polls(&relay) >= 3).then_some(()));
    relay.close();
}

impl Relay {
    /// Time exchanges sent through the relay so far.
    fn clocks(&self) -> usize {
        let hub = self.hub.lock();
        hub.rooms.values().flat_map(|r| &r.events).filter(|(_, _, _, b)| matches!(b, Body::Command { op, .. } if matches!(**op, Op::Clock { .. }))).count()
    }
}

#[test]
fn a_mirrored_device_is_timed_only_while_it_plays_on_screen() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1", "s2"], 0);
    phone.remote.clone().played(Playing { playing: false, position_ms: 5_000, ..Default::default() });
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().watch(false);
    desk.remote.clone().pick(Some(phone_id.clone()));
    desk.until("the phone mirrored, paused", |r| r.active().filter(|m| !m.playing));
    // A whole burst's time.
    std::thread::sleep(Duration::from_millis(2_500));
    assert_eq!(relay.clocks(), 0, "nothing wakes for a playhead standing still");

    // Playing, with only the notification showing it (the screen off).
    desk.remote.clone().sight(Sight::Notification);
    phone.remote.clone().played(Playing { playing: true, position_ms: 5_000, ..Default::default() });
    desk.until("the phone playing", |r| r.active().filter(|m| m.playing));
    std::thread::sleep(Duration::from_millis(2_500));
    assert_eq!(relay.clocks(), 0, "nor for a playhead nothing shows");

    desk.remote.clone().sight(Sight::Screen);
    desk.until("timed once it plays on screen", |_| (relay.clocks() > 0).then_some(()));
    relay.close();
}

#[test]
fn a_paused_device_out_of_sight_is_not_followed() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1", "s2"], 0);
    phone.remote.clone().played(Playing { playing: false, position_ms: 5_000, ..Default::default() });
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().watch(false);
    desk.remote.clone().pick(Some(phone_id.clone()));
    desk.until("the phone mirrored, paused", |r| r.active().filter(|m| !m.playing));

    // Nothing shows the phone on the desk any more: its held poll ends, and the phone playing is not heard.
    desk.remote.clone().sight(Sight::Nothing);
    let polls = desk.polls(&relay);
    phone.remote.clone().played(Playing { playing: true, position_ms: 5_000, ..Default::default() });
    let plays = || relay.hub.lock().rooms.values().flat_map(|r| &r.members).any(|m| m.id == phone_id && m.state.as_ref().is_some_and(|s| s.playing));
    phone.until("the relay has the phone playing", |_| plays().then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(desk.remote.active().is_some_and(|m| !m.playing), "the desk did not hear it");
    assert_eq!(desk.polls(&relay), polls, "nor asked");

    desk.remote.clone().sight(Sight::Screen);
    desk.until("followed again, playing", |r| r.active().filter(|m| m.playing));
    relay.close();
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
    desk.remote.clone().send(phone_id.clone(), Op::Pause);
    let m = desk.remote.active().unwrap();
    assert!(!m.playing);
    assert_eq!(phone.told(), Op::Pause);
    desk.remote.clone().send(phone_id.clone(), Op::Volume { percent: 15 });
    assert!(desk.remote.clone().star_where_playing("s60".into(), true), "a song of its queue is starred there");
    assert!(!desk.remote.clone().star_where_playing("elsewhere".into(), true), "any other here");
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
    desk.remote.clone().send(phone_id.clone(), Op::Remove { index: 2, rev: m.rev });
    assert_eq!(phone.told(), Op::Remove { index: 2, rev: m.rev });
    phone.core.session.remove(2, 3);
    assert!(!desk.remote.clone().put_back(phone_id.clone(), "s4".into()), "only a song taken out from here");
    assert!(desk.remote.clone().put_back(phone_id.clone(), "s3".into()));
    match phone.told() {
        Op::Restore { song, index } => {
            assert_eq!((song.id.as_str(), song.title.as_str(), index), ("s3", "S3", 2));
            assert!(phone.core.session.restore(song.id).at.is_some(), "the phone's own undo knows its place");
        }
        op => panic!("{op:?}"),
    }
    assert_eq!(phone.core.session.playlist(|p| p.ids().to_vec()), ["s1", "s2", "s3", "s4", "s5"]);
    assert!(!desk.remote.clone().put_back(phone_id.clone(), "s3".into()), "put back once");

    // Clear: the phone's player removes what plays after the current song, from the end.
    desk.remote.clone().send(phone_id.clone(), Op::Clear);
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

/// A state the device published before it carried out a command, arriving after the command was sent,
/// does not undo what the controller shows; the device's own word does, or time when it says nothing.
#[test]
fn a_command_shows_until_the_device_says_it_carried_it_out() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1", "s2"], 0);
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().pick(Some(phone_id.clone()));
    desk.until("the phone mirrored", |r| r.active().filter(|m| m.playing));
    // The phone's states reach the relay late.
    relay.lag(&phone_id, 0);

    phone.remote.clone().played(Playing { playing: true, position_ms: 8_000, index: None, volume: Some(40), ..Default::default() });
    desk.remote.clone().send(phone_id.clone(), Op::Pause);
    desk.remote.clone().send(phone_id.clone(), Op::Volume { percent: 10 });
    desk.until("the state from before", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state).filter(|s| s.position_ms >= 8_000));
    let m = desk.remote.active().unwrap();
    assert_eq!((m.playing, m.volume), (false, Some(10)), "still as commanded");

    assert_eq!((phone.told(), phone.told()), (Op::Pause, Op::Volume { percent: 10 }));
    phone.remote.clone().played(Playing { playing: false, position_ms: 8_400, index: None, volume: Some(12), ..Default::default() });
    desk.until("the phone's word", |r| r.active().filter(|m| !m.playing && m.position_ms == 8_400 && m.volume == Some(12)));

    // A command the phone never carries out shows only for a while.
    desk.remote.clone().send(phone_id.clone(), Op::Play);
    assert!(desk.remote.active().unwrap().playing);
    assert_eq!(phone.told(), Op::Play);
    desk.until("the phone as it is", |r| r.active().filter(|m| !m.playing));
    relay.close();
}

/// The device says it carried a pause out in a state published before its player has paused (the player
/// takes a moment): the controller goes on showing the pause until the player's own word.
#[test]
fn a_pause_stays_shown_through_the_devices_word_that_it_got_it() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1", "s2"], 0);
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().pick(Some(phone_id.clone()));
    desk.until("the phone mirrored", |r| r.active().filter(|m| m.playing));

    desk.remote.clone().send(phone_id.clone(), Op::Pause);
    assert_eq!(phone.told(), Op::Pause);
    // The phone's next state carries the answer, its player still playing.
    phone.remote.clone().volume_changed(Some(41));
    let m = desk.until("the phone's answer", |r| r.active().filter(|m| m.volume == Some(41)));
    assert!(!m.playing, "the pause is still shown");
    phone.remote.clone().played(Playing { playing: false, position_ms: 5_400, index: None, volume: Some(41), ..Default::default() });
    desk.until("the phone paused", |r| r.active().filter(|m| !m.playing && m.position_ms == 5_400));
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

#[test]
fn a_stopped_device_leaves_the_lists_at_once() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1"], 0);
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));

    // Its leaving reaches the relay late: stopping waits for it, as a client quits right after.
    relay.leave_late.store(true, Ordering::Relaxed);
    phone.remote.clone().stop();
    let listed = relay.hub.lock().rooms["u:ann"].members.iter().any(|m| m.id == phone_id);
    assert!(!listed, "the relay was told before the stop returned");
    desk.until("the phone gone", |r| r.devices().is_empty().then_some(()));
    relay.close();
}

#[test]
fn far_devices_are_hidden_while_the_relay_cannot_be_reached() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1"], 0);
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    let listed = |r: &Remote| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state);
    desk.until("the phone", listed);

    // What the relay last said is not shown once it no longer answers: the phone may have gone since.
    relay.go_down(true);
    desk.until("the phone hidden", |r| r.devices().is_empty().then_some(()));

    // Back as soon as the relay answers, without waiting for news.
    relay.go_down(false);
    desk.remote.clone().watch(false);
    desk.remote.clone().watch(true);
    desk.until("the phone again", listed);
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

/// A relay that tells its time: a jam's host and its guests each learn its clock, and a guest plays the
/// host's place by it, however late the host's words and answers reach it (a guest's own exchanges with
/// the host come out as lopsided as those delays).
#[test]
fn jam_guests_listen_along_by_the_relays_clock() {
    let relay = Relay::new();
    relay.time.store(true, Ordering::Relaxed);
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    // The host's clock reads four seconds behind the guest's; its words reach the relay late, and its
    // answers to the guest's time exchanges all a fifth of a second late.
    relay.lag(&host.remote.id(), -4_000_000);
    relay.answers_late.store(200, Ordering::Relaxed);
    host.playing(&["s1", "s2"], 0);
    let link = opened(&host);
    host.remote.clone().jam_along(true);
    let pass = joined(&relay, link, "Gus".into());
    let gus = Device::new(&relay, ServerConfig { url: pass.url, api_key: Some(pass.api_key), ..Default::default() }, DeviceKind::Guest, "Gus");
    let leads = Arc::new(Leads::default());
    gus.remote.follow_with(Some(leads.clone()));
    gus.remote.clone().listen(true);

    let said = clock::now_us();
    host.remote.clone().played(Playing { playing: true, position_ms: 30_000, rate: 1.25, index: Some(0), volume: None, ..Default::default() });
    gus.until("the host's place", |_| leads.last().flatten().filter(|l| l.ms >= 30_000.0));
    // Both bursts of time exchanges are over within three seconds.
    let settled = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settled {
        let _ = gus.news.recv_timeout(Duration::from_millis(100));
    }
    let l = leads.last().flatten().expect("a lead");
    let now = clock::now_us();
    let off = l.ms + (now - l.at_us) as f64 / 1000.0 * l.rate - (30_000.0 + (now - said) as f64 / 1000.0 * 1.25);
    assert!(off.abs() <= 5.0, "the guest plays {off:.1} ms off the host");
    assert!(relay.asked().iter().filter(|a| *a == "nori/time").count() >= 2 * nori_remote::clock::BURST, "each learned the relay's clock");
    relay.close();
}

/// The relay still lists a device from before it restarted, by its own id: it neither lists nor follows
/// itself.
#[test]
fn a_device_never_follows_itself() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    let me = phone.remote.id();
    let before = nori_remote::wire::DeviceState { playing: true, index: Some(0), ..Default::default() };
    relay.hub.lock().rooms.entry("u:ann".into()).or_default().members.push(Member { id: me.clone(), name: "Phone".into(), kind: DeviceKind::Phone, state: Some(before) });
    desk.remote.clone().serve(true);
    phone.remote.clone().serve(true);
    phone.remote.clone().watch(true);
    phone.until("the desk", |r| r.devices().into_iter().find(|d| d.name == "Desk"));
    assert!(phone.remote.devices().iter().all(|d| d.id != me), "not listed");
    phone.remote.clone().pick(Some(me));
    assert!(phone.remote.active().is_none(), "not followed");
    relay.close();
}

/// The leads a guest's player was given, newest last.
#[derive(Default)]
struct Leads(Mutex<Vec<Option<Lead>>>);

impl Follower for Leads {
    fn lead(&self, lead: Option<Lead>) {
        self.0.lock().push(lead);
    }
}

impl Leads {
    fn last(&self) -> Option<Option<Lead>> {
        self.0.lock().last().cloned()
    }
}

#[test]
fn jam_guests_listen_along_where_the_host_is_heard() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    // The host's clock reads four seconds behind the guests'. (Its clock learned through late words is
    // nori-remote's clock.rs and the engine's along.rs.)
    relay.skew(&host.remote.id(), -4_000_000);
    host.playing(&["s1", "s2"], 0);
    let link = opened(&host);
    host.remote.clone().jam_along(true);
    let guest_of = |name: &str| {
        let pass = joined(&relay, link.clone(), name.into());
        let d = Device::new(&relay, ServerConfig { url: pass.url, api_key: Some(pass.api_key), ..Default::default() }, DeviceKind::Guest, name);
        let leads = Arc::new(Leads::default());
        d.remote.follow_with(Some(leads.clone()));
        d.remote.clone().listen(true);
        (d, leads)
    };
    let (gus, gus_leads) = guest_of("Gus");
    let (dee, dee_leads) = guest_of("Dee");

    let said = clock::now_us();
    host.remote.clone().played(Playing { playing: true, position_ms: 30_000, rate: 1.25, index: Some(0), volume: None, ..Default::default() });
    for (g, leads) in [(&gus, &gus_leads), (&dee, &dee_leads)] {
        g.until("the host's place", |_| leads.last().flatten().filter(|l| l.ms >= 30_000.0));
        assert_eq!(g.remote.jam_view().map(|v| v.listening), Some(Listening::Playing));
    }
    // The burst of time exchanges is over within three seconds.
    let settled = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settled {
        let _ = gus.news.recv_timeout(Duration::from_millis(100));
    }
    for leads in [&gus_leads, &dee_leads] {
        let l = leads.last().flatten().expect("a lead");
        assert_eq!((l.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), l.index, l.rate, l.playing), (vec!["s1", "s2"], 0, 1.25, true));
        let now = clock::now_us();
        let off = l.ms + (now - l.at_us) as f64 / 1000.0 * l.rate - (30_000.0 + (now - said) as f64 / 1000.0 * 1.25);
        assert!(off.abs() <= 10.0, "the guest plays {off:.1} ms off the host");
    }
    // A word reaching them a quarter second late: their pages (the seek bar, the lyrics) show the place
    // heard, not where it was as the word arrived.
    relay.lag(&host.remote.id(), -4_000_000);
    let said = clock::now_us();
    host.remote.clone().played(Playing { playing: true, position_ms: 40_000, rate: 1.25, index: Some(0), volume: None, ..Default::default() });
    for g in [&gus, &dee] {
        let m = g.until("the host's new place", |r| r.jam_playing().filter(|m| m.position_ms >= 40_000));
        let now = clock::now_us();
        let off = m.position_at(now) as f64 - (40_000.0 + (now - said) as f64 / 1000.0 * 1.25);
        assert!(off.abs() <= 10.0, "the guest shows the host's place {off:.1} ms off");
    }
    relay.skew(&host.remote.id(), -4_000_000);

    // The host stops letting its guests listen along: they stop.
    host.remote.clone().jam_along(false);
    for (g, leads) in [(&gus, &gus_leads), (&dee, &dee_leads)] {
        g.until("the host's no", |r| r.jam_view().filter(|v| v.listening == Listening::HostOff));
        assert_eq!(leads.last(), Some(None), "nothing to follow");
    }
    host.remote.clone().jam_along(true);
    gus.until("listening again", |r| r.jam_view().filter(|v| v.listening == Listening::Playing));

    // A server that does not let guests stream: unavailable, the jam itself still there.
    relay.along.store(false, Ordering::Relaxed);
    host.remote.clone().played(Playing { playing: false, position_ms: 31_000, rate: 1.25, index: Some(0), volume: None, ..Default::default() });
    let view = gus.until("the server's no", |r| r.jam_view().filter(|v| v.listening == Listening::ServerOff));
    assert_eq!(view.members.len(), 3);
    assert_eq!(gus_leads.last(), Some(None));
    gus.remote.clone().listen(false);
    assert_eq!(gus.remote.jam_view().map(|v| v.listening), Some(Listening::Watching));
    relay.close();
}

#[test]
fn a_guest_leaving_its_jam_stops_playing_along_at_once() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    host.playing(&["s1", "s2"], 0);
    let link = opened(&host);
    host.remote.clone().jam_along(true);
    let pass = joined(&relay, link, "Gus".into());
    let gus = Device::new(&relay, ServerConfig { url: pass.url, api_key: Some(pass.api_key), ..Default::default() }, DeviceKind::Guest, "Gus");
    let leads = Arc::new(Leads::default());
    gus.remote.follow_with(Some(leads.clone()));
    gus.remote.clone().listen(true);
    host.remote.clone().played(Playing { playing: true, position_ms: 30_000, rate: 1.0, index: Some(0), volume: None, ..Default::default() });
    gus.until("the host's place", |_| leads.last().flatten());

    gus.remote.clone().jam_leave();
    assert_eq!(leads.last(), Some(None), "nothing to follow once left");
    let given = leads.0.lock().len();
    host.remote.clone().played(Playing { playing: true, position_ms: 60_000, rate: 1.0, index: Some(1), volume: None, ..Default::default() });
    gus.remote.clone().stop();
    assert_eq!(leads.0.lock().len(), given, "the host is followed no more");
    relay.close();
}

/// Its own invite, to the jam open or one it ended (also after the app started again), changes nothing
/// and asks nothing; anyone's ended jam is told apart from a failure by the relay's answer.
#[test]
fn a_device_knows_its_own_jams_invites_and_ended_ones() {
    let dir = nori_testdir::TempDir::new("remote-invites");
    let relay = Relay::new();
    let host = Device::kept(&relay, &dir, DeviceKind::Phone, "Host");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    let join = |d: &Device, link: &str| block_on(jam_join(relay.clone(), d.core.session.settings.clone(), Some(d.remote.clone()), link.into(), "Me".into())).unwrap();
    let link = opened(&host);
    let app_link = link.replacen("http://octo:5274/nori/jam#", "nori://jam?", 1);
    assert_eq!((join(&host, &link), join(&host, &app_link)), (JamJoin::Own, JamJoin::Own));

    host.remote.clone().jam_close();
    assert_eq!(join(&host, &link), JamJoin::Ended);
    host.remote.clone().stop();
    let again = Device::kept(&relay, &dir, DeviceKind::Phone, "Host");
    assert_eq!(join(&again, &link), JamJoin::Ended, "after the app started again");
    assert_eq!(relay.asked_for("noriRemote.join"), 0, "the relay was not asked");
    assert!(again.remote.jam_view().is_none());

    let desks = opened(&desk);
    assert!(matches!(join(&again, &desks), JamJoin::Joined { .. }), "another device of the account may join");
    desk.remote.clone().jam_close();
    eventually("the desk's jam closed", || relay.jams() == 0);
    assert_eq!(join(&again, &desks), JamJoin::Ended);
    relay.close();
}

/// A jam whose close did not reach the relay is still listed in this device's polls: it is closed again,
/// never shown as a jam this device is in.
#[test]
fn a_jam_ended_while_the_relay_missed_it_is_closed_again() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    host.playing(&["s1"], 0);
    host.remote.clone().watch(true);
    opened(&host);
    relay.lose_closes.store(true, Ordering::Relaxed);
    host.remote.clone().jam_close();
    eventually("the close lost", || relay.asked().iter().any(|a| a == "lost close"));
    relay.lose_closes.store(false, Ordering::Relaxed);

    // News in the account's room: the host's held poll answers, the jam still listed in it.
    desk.remote.clone().serve(true);
    host.until("the desk", |r| r.devices().into_iter().find(|d| d.name == "Desk"));
    assert!(host.remote.jam_view().is_none(), "no jam shown");
    eventually("the jam closed at the relay", || relay.jams() == 0);
    relay.close();
}

/// Start pressed again and again while the relay is slow to open: one jam is asked for, and once it is
/// ended none comes back.
#[test]
fn a_jam_is_started_once_and_stays_ended() {
    let dir = nori_testdir::TempDir::new("remote-starts");
    let relay = Relay::new();
    let host = Device::kept(&relay, &dir, DeviceKind::Phone, "Host");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    host.playing(&["s1"], 0);
    host.remote.clone().watch(true);
    eventually("the held poll", || !relay.waiting.lock().is_empty());
    relay.opens_late.store(true, Ordering::Relaxed);
    let starts: Vec<_> = (0..5).map(|_| {
        let r = host.remote.clone();
        std::thread::spawn(move || block_on(r.jam_open()))
    }).collect();
    eventually("the start asked", || relay.asked().iter().any(|a| a == "open waits"));
    assert_eq!(host.remote.jam_start(), JamStart::Starting);
    let links: Vec<_> = starts.into_iter().map(|s| s.join().unwrap().unwrap()).collect();
    assert_eq!(links.iter().flatten().count(), 1, "{links:?}");
    assert_eq!(relay.asked_for("noriRemote.open"), 1);
    assert_eq!(host.remote.jam_start(), JamStart::Hosting);

    host.remote.clone().jam_close();
    desk.remote.clone().serve(true);
    host.until("the desk", |r| r.devices().into_iter().find(|d| d.name == "Desk"));
    eventually("no jam at the relay", || relay.jams() == 0);
    assert!(host.remote.jam_view().is_none());
    assert_eq!(host.remote.jam_start(), JamStart::Offered);
    relay.close();
}

/// Ended while the relay was still opening it: the jam it opens is closed again, and none is hosted.
#[test]
fn ending_a_jam_being_started_cancels_it() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    relay.opens_late.store(true, Ordering::Relaxed);
    let r = host.remote.clone();
    let start = std::thread::spawn(move || block_on(r.jam_open()));
    eventually("the start asked", || relay.asked().iter().any(|a| a == "open waits"));
    host.remote.clone().jam_close();
    assert_eq!(start.join().unwrap().unwrap(), None);
    assert!(host.remote.jam_view().is_none());
    eventually("its jam closed at the relay", || relay.jams() == 0);
    relay.close();
}

/// A guest of `host`'s jam `link`, listening along: its leads.
fn listening_guest(relay: &Arc<Relay>, host: &Device, link: String) -> (Device, Arc<Leads>) {
    host.remote.clone().jam_along(true);
    let pass = joined(relay, link, "Gus".into());
    let gus = Device::new(relay, ServerConfig { url: pass.url, api_key: Some(pass.api_key), ..Default::default() }, DeviceKind::Guest, "Gus");
    let leads = Arc::new(Leads::default());
    gus.remote.follow_with(Some(leads.clone()));
    gus.remote.clone().listen(true);
    host.remote.clone().played(Playing { playing: true, position_ms: 30_000, rate: 1.0, index: Some(0), volume: None, ..Default::default() });
    gus.until("the host's place", |_| leads.last().flatten());
    (gus, leads)
}

#[test]
fn a_guest_hears_its_jam_end_and_stops_playing_along() {
    // Ended by the host, this guest sent out by it, or forgotten by the relay (its key no longer signs in).
    for how in ["closed", "kicked", "restarted"] {
        let relay = Relay::new();
        let host = Device::account(&relay, DeviceKind::Phone, "Host");
        host.playing(&["s1", "s2"], 0);
        let link = opened(&host);
        let (gus, leads) = listening_guest(&relay, &host, link);
        match how {
            "closed" => host.remote.clone().jam_close(),
            "kicked" => {
                let you = gus.until("its place in the jam", |r| r.jam_view()).you;
                host.until("its guest", |r| r.jam_view().filter(|v| v.members.iter().any(|m| m.id == you)));
                host.remote.clone().jam_act(Op::Kick { member: you });
            }
            _ => relay.restart(),
        }
        gus.until(&format!("the end, {how}"), |_| (!gus.ended.lock().is_empty()).then_some(()));
        assert_eq!(*gus.ended.lock(), [Some("Host".to_string())], "{how}");
        assert_eq!(leads.last(), Some(None), "nothing to follow");
        assert!(gus.remote.jam_view().is_none());
        gus.remote.clone().stop();
        assert_eq!(gus.ended.lock().len(), 1, "told once");
        assert!(host.ended.lock().is_empty(), "the host was in no one's jam");
        relay.close();
    }
}

#[test]
fn a_guest_profile_opened_after_its_jam_ended_hears_so_at_once() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    let link = opened(&host);
    let pass = joined(&relay, link, "Gus".into());
    host.remote.clone().jam_close();
    // The app opens again on the guest profile: its first look at the jam finds it gone.
    let gus = Device::new(&relay, ServerConfig { url: pass.url, api_key: Some(pass.api_key), ..Default::default() }, DeviceKind::Guest, "Gus");
    gus.remote.clone().watch(true);
    gus.until("the end", |_| (!gus.ended.lock().is_empty()).then_some(()));
    assert_eq!(*gus.ended.lock(), [None], "no host was ever seen");
    relay.close();
}

#[test]
fn a_guest_leaves_at_once_with_the_relay_down() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    host.playing(&["s1", "s2"], 0);
    let link = opened(&host);
    let (gus, leads) = listening_guest(&relay, &host, link);
    relay.go_down(true);
    gus.remote.clone().jam_leave();
    assert_eq!(leads.last(), Some(None), "its music stops here at once");
    gus.remote.clone().stop();
    assert!(gus.ended.lock().is_empty(), "left, not ended by the host");
    relay.close();
}

/// As in Spotify's Jam: an admin's player controls reach the host's playback, every listener's; a plain
/// guest's pause holds only its own listening (the platform's to carry out), and it skips nothing.
#[test]
fn jam_controls_reach_by_role() {
    let relay = Relay::new();
    let host = Device::account(&relay, DeviceKind::Phone, "Host");
    host.playing(&["s1", "s2"], 0);
    let link = opened(&host);
    host.remote.clone().jam_along(true);
    let guest_of = |name: &str| {
        let pass = joined(&relay, link.clone(), name.into());
        let d = Device::new(&relay, ServerConfig { url: pass.url, api_key: Some(pass.api_key), ..Default::default() }, DeviceKind::Guest, name);
        d.remote.follow_with(Some(Arc::new(Leads::default())));
        d.remote.clone().listen(true);
        d.remote.clone().played(Playing { playing: true, position_ms: 30_000, index: Some(0), ..Default::default() });
        d
    };
    let gus = guest_of("Gus");
    let dee = guest_of("Dee");
    host.remote.clone().played(Playing { playing: true, position_ms: 30_000, index: Some(0), ..Default::default() });
    let dee_id = dee.until("the jam", |r| r.jam_view().filter(|v| v.members.len() == 3)).you;
    host.remote.clone().jam_act(Op::Promote { member: dee_id, admin: true });
    let admin = dee.until("Dee's role", |r| r.jam_controls().filter(|c| c.controls.skip == Reach::Jam));
    assert_eq!(admin, JamControls { controls: Controls::of(Role::Admin, true), playing: true, paused_here: false });

    // The guest's own pause: carried out here, nothing sent; the jam plays on.
    gus.until("Gus's controls", |r| r.jam_controls().filter(|c| c.playing));
    assert_eq!(gus.remote.clone().jam_press(Op::Pause), Reach::Here);
    gus.remote.clone().played(Playing { playing: false, position_ms: 31_000, index: Some(0), ..Default::default() });
    let paused = gus.until("paused here", |r| r.jam_controls().filter(|c| c.paused_here));
    assert_eq!(paused, JamControls { controls: Controls::of(Role::Guest, true), playing: false, paused_here: true });
    for op in [Op::Next, Op::Seek { ms: 1_000 }, Op::Move { from: 0, to: 1, rev: 0 }] {
        assert_eq!(gus.remote.clone().jam_press(op), Reach::Nowhere);
    }
    assert_eq!(gus.remote.clone().jam_press(Op::Play), Reach::Here, "play joins the jam again");
    // A guest's control sent all the same (an older client) is refused.
    gus.remote.clone().jam_act(Op::Next);
    assert_eq!(gus.until("the refusal", |r| r.jam_view().and_then(|v| v.refused)), Refusal::NotAllowed);
    assert!(host.ops.try_recv().is_err(), "nothing from the plain guest");

    // The admin's pause and skip are the host's.
    assert_eq!(dee.remote.clone().jam_press(Op::Pause), Reach::Jam);
    assert_eq!(host.told(), Op::Pause);
    assert_eq!(dee.remote.clone().jam_press(Op::Next), Reach::Jam);
    assert_eq!(host.told(), Op::Next);
    relay.close();
}


/// A jam plays here and nowhere else: starting one brings the playback back to this device, and while it
/// runs, the playback cannot be moved to another device.
#[test]
fn a_jam_and_playing_on_another_device_exclude_each_other() {
    let relay = Relay::new();
    let phone = Device::account(&relay, DeviceKind::Phone, "Phone");
    let desk = Device::account(&relay, DeviceKind::Desktop, "Desk");
    phone.playing(&["s1", "s2"], 0);
    phone.remote.clone().serve(true);
    let phone_id = phone.remote.id();
    desk.remote.clone().watch(true);
    desk.until("the phone", |r| r.devices().into_iter().find(|d| d.id == phone_id).and_then(|d| d.state));
    desk.remote.clone().pick(Some(phone_id.clone()));
    desk.until("the phone mirrored", |r| r.active().filter(|m| m.id == phone_id));

    block_on(desk.remote.clone().jam_open()).unwrap();
    assert_eq!(phone.told(), Op::Pause);
    assert!(matches!(desk.told(), Op::Replace { play: true, .. }));
    desk.until("playing here", |r| r.active().is_none().then_some(()));

    desk.remote.clone().pick(Some(phone_id.clone()));
    assert!(desk.remote.active().is_none(), "no moving the playback while the jam runs");
    desk.remote.clone().jam_close();
    desk.remote.clone().pick(Some(phone_id.clone()));
    assert!(desk.remote.active().is_some(), "free to move it again once the jam is over");
    relay.close();
}
