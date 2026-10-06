//! Remote control without the relay, between devices on one network. A controllable device opens a door:
//! a small HTTP listener speaking the relay's poll and send for itself alone. Every request carries an
//! HMAC-SHA256 over it, keyed with the account's Subsonic secret (password or API key), which only the
//! account's own devices hold. Discovery (mDNS `_nori._tcp`) is the platform's; the TXT record carries
//! [`account_tag`] so a device lists only its account's doors.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use sha2::{Digest, Sha256};

use crate::wire::{Answer, Body, DeviceState, Event, Member, Outgoing, Room, HOLD_MS};

/// The mDNS service type a door is announced as.
pub const SERVICE: &str = "_nori._tcp";

/// How far a request's time may be from the door's: a captured request cannot be sent again later.
const SKEW_MS: i64 = 60_000;

/// Replies a door keeps for controllers that have not polled them yet.
const KEPT_EVENTS: usize = 64;

/// The room a door's answers name.
pub const ROOM: &str = "lan";

fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a door announces so the account's devices recognise it: a keyed hash of the user name, so
/// another account's devices on the network neither match it nor learn the name.
pub fn account_tag(user: &str, secret: &str) -> String {
    hex(&hmac(secret.as_bytes(), format!("nori-account:{user}").as_bytes()))[..16].to_string()
}

fn signature(secret: &str, method: &str, target: &str, body: &[u8]) -> String {
    let mut msg = format!("{method} {target}\n").into_bytes();
    msg.extend_from_slice(body);
    hex(&hmac(secret.as_bytes(), &msg))
}

/// `path?query` signed for a door: `ts` and `sig` appended.
pub fn signed(secret: &str, method: &str, path_query: &str, body: &[u8], now_ms: i64) -> String {
    let target = format!("{path_query}{}ts={now_ms}", if path_query.contains('?') { '&' } else { '?' });
    let sig = signature(secret, method, &target, body);
    format!("{target}&sig={sig}")
}

/// Whether `target` (path and query, `sig` last) was signed with `secret` within [`SKEW_MS`] of `now_ms`.
fn verified(secret: &str, method: &str, target: &str, body: &[u8], now_ms: i64) -> bool {
    let Some((signed, sig)) = target.rsplit_once("&sig=") else { return false };
    let fresh = query(signed, "ts").and_then(|t| t.parse::<i64>().ok()).is_some_and(|ts| (now_ms - ts).abs() <= SKEW_MS);
    fresh && signature(secret, method, signed, body) == sig
}

/// Parameter `key` of a request target, undecoded (doors are only sent unreserved characters).
fn query<'a>(target: &'a str, key: &str) -> Option<&'a str> {
    target.split_once('?')?.1.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| v)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// What a door does with a command that came through it: (the sending device's id, the body).
pub type Handler = Box<dyn Fn(&str, Body) + Send + Sync>;

struct Inside {
    seq: u64,
    me: Member,
    /// Replies by sequence, each to one controller.
    events: VecDeque<(u64, String, Body)>,
}

struct Shared {
    secret: String,
    closed: AtomicBool,
    inside: Mutex<Inside>,
    changed: Condvar,
    handler: Handler,
}

/// A controllable device's listener. Its thread blocks in `accept` (no wakeups); each connection gets a
/// thread of its own while it lasts.
pub struct Door {
    port: u16,
    shared: Arc<Shared>,
}

impl Door {
    /// Listens on every interface, on a port the system picks.
    pub fn open(me: Member, secret: String, handler: Handler) -> std::io::Result<Door> {
        let listener = TcpListener::bind(("0.0.0.0", 0))?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared { secret, closed: AtomicBool::new(false), inside: Mutex::new(Inside { seq: 1, me, events: VecDeque::new() }), changed: Condvar::new(), handler });
        let s = shared.clone();
        std::thread::Builder::new().name("nori-door".into()).spawn(move || {
            for conn in listener.incoming() {
                if s.closed.load(Ordering::Acquire) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let s = s.clone();
                let _ = std::thread::Builder::new().name("nori-door-in".into()).spawn(move || serve(&s, conn));
            }
        })?;
        Ok(Door { port, shared })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    fn change(&self, f: impl FnOnce(&mut Inside)) {
        let mut i = self.shared.inside.lock();
        i.seq += 1;
        f(&mut i);
        self.shared.changed.notify_all();
    }

    /// This device's state, as polls answer it from now on.
    pub fn publish(&self, state: DeviceState) {
        self.change(|i| i.me.state = Some(state));
    }

    /// Sends `body` to controller `to` with its next poll.
    pub fn reply(&self, to: &str, body: Body) {
        self.change(|i| {
            let seq = i.seq;
            i.events.push_back((seq, to.to_string(), body));
            if i.events.len() > KEPT_EVENTS {
                i.events.pop_front();
            }
        });
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        // Wakes the accepting thread so it sees the door closed.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(s: &Shared, conn: TcpStream) {
    // An idle kept-alive connection ends rather than holding its thread.
    let _ = conn.set_read_timeout(Some(Duration::from_millis(HOLD_MS as u64 * 2)));
    let Ok(mut out) = conn.try_clone() else { return };
    let mut reader = BufReader::new(conn);
    while !s.closed.load(Ordering::Acquire) {
        let Some((method, target, body)) = read_request(&mut reader) else { break };
        let (status, json) = answer(s, &method, &target, &body);
        let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", json.len());
        if out.write_all(head.as_bytes()).and_then(|_| out.write_all(json.as_bytes())).is_err() {
            break;
        }
    }
    let _ = out.shutdown(Shutdown::Both);
}

/// Method, target and body of the next request on a connection; None at its end.
fn read_request(r: &mut impl BufRead) -> Option<(String, String, Vec<u8>)> {
    let mut line = String::new();
    r.read_line(&mut line).ok().filter(|n| *n > 0)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok().filter(|n| *n > 0)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().ok()?;
            }
        }
    }
    // A command is small; anything larger is not one.
    if length > 1 << 20 {
        return None;
    }
    let mut body = vec![0; length];
    r.read_exact(&mut body).ok()?;
    Some((method, target, body))
}

fn answer(s: &Shared, method: &str, target: &str, body: &[u8]) -> (&'static str, String) {
    if !verified(&s.secret, method, target, body, now_ms()) {
        return ("401 Unauthorized", "{}".into());
    }
    let Some(dev) = query(target, "dev").map(str::to_string) else { return ("400 Bad Request", "{}".into()) };
    let path = target.split('?').next().unwrap_or_default();
    match path {
        "/rest/noriRemote.poll" => {
            let since = query(target, "since").and_then(|v| v.parse::<u64>().ok());
            let hold = query(target, "hold") == Some("1");
            let mut i = s.inside.lock();
            if let (Some(since), true) = (since, hold) {
                let until = std::time::Instant::now() + Duration::from_millis(HOLD_MS as u64);
                while i.seq <= since && !s.closed.load(Ordering::Acquire) {
                    if s.changed.wait_until(&mut i, until).timed_out() {
                        break;
                    }
                }
            }
            let events = i
                .events
                .iter()
                .filter(|(seq, to, _)| since.is_some_and(|since| *seq > since) && *to == dev)
                .map(|(seq, _, body)| Event { seq: *seq, room: ROOM.into(), from: i.me.id.clone(), body: body.clone() })
                .collect();
            let a = Answer { seq: i.seq, rooms: vec![Room { room: ROOM.into(), jam: false, members: vec![i.me.clone()] }], events };
            ("200 OK", serde_json::to_string(&a).unwrap_or_default())
        }
        "/rest/noriRemote.send" if method == "POST" => {
            let Ok(out) = serde_json::from_slice::<Outgoing>(body) else { return ("400 Bad Request", "{}".into()) };
            if let Some(b) = out.body {
                (s.handler)(&dev, b);
            }
            let seq = s.inside.lock().seq;
            ("200 OK", format!(r#"{{"seq":{seq}}}"#))
        }
        _ => ("404 Not Found", "{}".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Op;
    use std::io::Read;

    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(hex(&hmac(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        let long_key = [0xaa; 131];
        assert_eq!(hex(&hmac(&long_key, b"Test Using Larger Than Block-Size Key - Hash Key First")), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
    }

    #[test]
    fn requests_need_the_account_secret_and_a_fresh_time() {
        let t = signed("sesame", "POST", "/rest/noriRemote.send?dev=a", b"{}", 1_000_000);
        assert!(verified("sesame", "POST", &t, b"{}", 1_000_000 + SKEW_MS));
        assert!(!verified("other", "POST", &t, b"{}", 1_000_000), "another secret");
        assert!(!verified("sesame", "POST", &t, b"{\"x\":1}", 1_000_000), "another body");
        assert!(!verified("sesame", "GET", &t, b"{}", 1_000_000), "another method");
        assert!(!verified("sesame", "POST", &t.replace("dev=a", "dev=b"), b"{}", 1_000_000), "another device");
        assert!(!verified("sesame", "POST", &t, b"{}", 1_000_001 + SKEW_MS), "too old");
        assert_eq!(account_tag("ann", "pw"), account_tag("ann", "pw"));
        assert_ne!(account_tag("ann", "pw"), account_tag("ann", "pw2"));
        assert_ne!(account_tag("ann", "pw"), account_tag("bob", "pw"));
    }

    fn request(port: u16, secret: &str, method: &str, path_query: &str, body: &str) -> (u16, String) {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let target = signed(secret, method, path_query, body.as_bytes(), now_ms());
        write!(c, "{method} {target} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut r = BufReader::new(c);
        let mut status = String::new();
        r.read_line(&mut status).unwrap();
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
        let mut b = vec![0; length];
        r.read_exact(&mut b).unwrap();
        (status.split_whitespace().nth(1).unwrap().parse().unwrap(), String::from_utf8(b).unwrap())
    }

    #[test]
    fn a_door_takes_commands_and_answers_polls() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let me = Member { id: "desk".into(), name: "Desk".into(), ..Default::default() };
        let door = Door::open(me, "pw".into(), Box::new(move |from, body| g.lock().push((from.to_string(), body)))).unwrap();
        door.publish(DeviceState { playing: true, rev: 3, ..Default::default() });
        door.reply("tablet", Body::Ack { id: 9, refusal: None });

        let send = serde_json::to_string(&Outgoing { body: Some(Body::Command { id: 1, op: Box::new(Op::Pause) }), ..Default::default() }).unwrap();
        assert_eq!(request(door.port(), "wrong", "POST", "/rest/noriRemote.send?dev=phone", &send).0, 401);
        assert!(got.lock().is_empty());
        assert_eq!(request(door.port(), "pw", "POST", "/rest/noriRemote.send?dev=phone", &send).0, 200);
        assert_eq!(*got.lock(), [("phone".to_string(), Body::Command { id: 1, op: Box::new(Op::Pause) })]);

        let (_, first) = request(door.port(), "pw", "GET", "/rest/noriRemote.poll?dev=phone", "");
        let first: Answer = serde_json::from_str(&first).unwrap();
        assert_eq!(first.rooms[0].members[0].state.as_ref().map(|s| s.rev), Some(3));

        // A held poll comes back with the reply as soon as there is one.
        let port = door.port();
        let since = first.seq;
        let held = std::thread::spawn(move || request(port, "pw", "GET", &format!("/rest/noriRemote.poll?dev=phone&since={since}&hold=1"), "").1);
        door.reply("phone", Body::Ack { id: 1, refusal: None });
        let a: Answer = serde_json::from_str(&held.join().unwrap()).unwrap();
        let bodies: Vec<&Body> = a.events.iter().map(|e| &e.body).collect();
        assert_eq!(bodies, [&Body::Ack { id: 1, refusal: None }], "only its own replies");
    }
}
