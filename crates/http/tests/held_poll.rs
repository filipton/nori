//! A remote stopped while the desktop's transport holds a relay poll, against a relay on this machine
//! that answers a first poll at once and holds the later ones for as long as the client keeps them.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

use nori_core::client::{Client, NetProfile};
use nori_core::remote::wire::{DeviceKind, Op};
use nori_core::remote::{Remote, RemoteMe, RemotePlayer, RemoteShown};
use nori_core::{Core, ServerConfig};
use nori_http::Http;

#[derive(Debug, PartialEq)]
enum Heard {
    /// A poll arrived that the relay holds.
    Held,
    /// The client hung up a held poll.
    HungUp,
}

/// Reads up to the end of a request's head.
fn head(c: &mut TcpStream) -> Option<String> {
    let mut got = Vec::new();
    let mut b = [0; 1];
    while !got.ends_with(b"\r\n\r\n") {
        if c.read(&mut b).ok()? == 0 {
            return None;
        }
        got.push(b[0]);
    }
    Some(String::from_utf8_lossy(&got).into_owned())
}

fn exchange(mut c: TcpStream, heard: Sender<Heard>) {
    let Some(head) = head(&mut c) else { return };
    if head.contains("hold=1") {
        let _ = heard.send(Heard::Held);
        // Holds until the client hangs up.
        while c.read(&mut [0; 64]).is_ok_and(|n| n > 0) {}
        let _ = heard.send(Heard::HungUp);
        return;
    }
    let body = r#"{"seq":1,"you":"me","rooms":[],"events":[]}"#;
    let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

fn relay(heard: Sender<Heard>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in listener.incoming().flatten() {
            let heard = heard.clone();
            std::thread::spawn(move || exchange(c, heard));
        }
    });
    port
}

struct Nothing;

impl RemotePlayer for Nothing {
    fn apply(&self, _: Op) {}
}

impl RemoteShown for Nothing {
    fn changed(&self) {}
}

#[test]
fn stopping_ends_a_held_poll_at_once() {
    let (heard_to, heard) = channel();
    let url = format!("http://127.0.0.1:{}", relay(heard_to));
    let core = Core::new(String::new(), "held".into(), Default::default()).unwrap();
    core.configure(ServerConfig { url: url.clone(), user: "ann".into(), password: "pw".into(), ..Default::default() }).unwrap();
    let client = Client::new(core, Http::new(), Default::default());
    client.set_profile(NetProfile { url, ..Default::default() });
    let remote = Remote::new(client, RemoteMe { name: "Desk".into(), kind: DeviceKind::Desktop }, Arc::new(Nothing), Arc::new(Nothing), None);
    remote.clone().watch(true);
    assert_eq!(heard.recv_timeout(Duration::from_secs(10)), Ok(Heard::Held));

    let (done_to, done) = channel();
    std::thread::spawn(move || {
        remote.stop();
        let _ = done_to.send(());
    });
    assert!(done.recv_timeout(Duration::from_secs(2)).is_ok(), "stopped without waiting out the held poll");
    assert_eq!(heard.recv_timeout(Duration::from_secs(2)), Ok(Heard::HungUp), "the request ended");
}
