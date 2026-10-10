//! Remote control and jams. One account's devices control each other, and a jam's members ask its host
//! for songs, through one protocol (wire.rs): octo-fiesta's relay hub with rooms (the account's own
//! room, or a jam's), or straight over the LAN through a device's door (lan.rs). What a controlled device
//! admits is device.rs; a jam's members, roles and requests are jam.rs. No I/O but the door's sockets.

pub mod clock;
pub mod device;
pub mod jam;
pub mod lan;
pub mod wire;

/// What a jam guest's API key starts with: the relay takes such a key for the host's account, scoped
/// to the jam.
const GUEST_KEY: &str = "nori-jam-";

/// The API key a jam guest's profile signs with, from the key the relay gave it.
pub fn guest_key(key: &str) -> String {
    format!("{GUEST_KEY}{key}")
}

/// Whether a profile's API key is a jam guest's: the app then shows only the jam.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn is_guest_key(api_key: &str) -> bool {
    api_key.starts_with(GUEST_KEY)
}

/// The app's own invite link, which Android opens nori with; the invite page hands the invite over as one.
const APP_INVITE: &str = "nori://jam";

/// The path, under the server's address, of octo-fiesta's invite page, which hands the invite to the app.
const INVITE_PAGE: &str = "/nori/jam";

fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(v: &str) -> Option<String> {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(v.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The link (and QR code) a guest joins with: the relay's invite page, with the relay's address and the
/// jam's invite key in the fragment, which a browser keeps out of requests and so out of access logs.
pub fn invite_link(server: &str, invite: &str) -> String {
    format!("{}{INVITE_PAGE}#s={}&k={}", server.trim_end_matches('/'), encode(server), encode(invite))
}

/// The relay's address and the invite key of an invite link, the page's or the app's; None for anything else.
pub fn parse_invite(link: &str) -> Option<(String, String)> {
    let link = link.trim();
    let query = match link.strip_prefix(APP_INVITE) {
        Some(rest) => rest.strip_prefix('?')?,
        None => {
            let (page, fragment) = link.split_once('#')?;
            let web = ["https://", "http://"].iter().any(|s| page.get(..s.len()).is_some_and(|p| p.eq_ignore_ascii_case(s)));
            (web && page.trim_end_matches('/').ends_with(INVITE_PAGE)).then_some(fragment)?
        }
    };
    let get = |key: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).and_then(|(_, v)| decode(v));
    let (server, invite) = (get("s")?, get("k")?);
    (!server.is_empty() && !invite.is_empty()).then_some((server, invite))
}

/// Whether `link` is a jam invite, for the app to offer joining it.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn is_invite(link: &str) -> bool {
    parse_invite(link).is_some()
}

/// Whether server address `url` can be reached only on a home network (a private, loopback or
/// link-local address, or a name only such a network resolves), so an invite to it fails elsewhere.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn is_home_only(url: &str) -> bool {
    use std::net::{IpAddr, Ipv6Addr};
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None if host_port.matches(':').count() == 1 => host_port.split(':').next().unwrap_or_default(),
        None => host_port,
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    let v6_home = |a: &Ipv6Addr| a.is_loopback() || a.is_unspecified() || (a.segments()[0] & 0xfe00) == 0xfc00 || (a.segments()[0] & 0xffc0) == 0xfe80;
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => a.is_private() || a.is_loopback() || a.is_link_local() || a.is_unspecified(),
        Ok(IpAddr::V6(a)) => v6_home(&a) || a.to_ipv4_mapped().is_some_and(|v4| v4.is_private() || v4.is_loopback() || v4.is_link_local()),
        // A single label (a NAS's name) resolves only where the home network's DNS answers.
        Err(_) => !host.contains('.') || [".local", ".localhost", ".home.arpa"].iter().any(|s| host.ends_with(s)),
    }
}

/// A new random id (a device's, kept by the app).
pub fn new_id() -> String {
    use sha2::{Digest, Sha256};
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let here = &t as *const u128 as usize;
    let h = Sha256::digest(format!("{t}:{}:{here}", std::process::id()));
    h[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// A QR code's modules, row by row.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QrCode {
    /// Modules per side.
    pub size: u32,
    pub dark: Vec<bool>,
}

/// `text` as a QR code (medium error correction); None when it is too long for one.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn qr_code(text: String) -> Option<QrCode> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let dark = code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
    Some(QrCode { size: code.width() as u32, dark })
}

/// Where `state`'s position is now, `elapsed_ms` after it was received, within the current song.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn position_now(state: &wire::DeviceState, elapsed_ms: i64) -> i64 {
    let pace = pace(state);
    if pace == 0.0 {
        return state.position_ms;
    }
    let length = state.entries.iter().find(|e| Some(e.index) == state.index).map_or(i64::MAX, |e| e.duration as i64 * 1000);
    (state.position_ms + (elapsed_ms.max(0) as f64 * pace) as i64).min(length)
}

/// How fast the audible playhead moves; held while paused or waiting for audio.
pub fn pace(state: &wire::DeviceState) -> f64 {
    if state.playing && !state.buffering { rate(state) } else { 0.0 }
}

/// How fast `state`'s place moves while it plays, song ms per real ms.
pub fn rate(state: &wire::DeviceState) -> f64 {
    state.rate.filter(|r| r.is_finite() && *r > 0.0).map_or(1.0, f64::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::{DeviceState, Entry};

    #[test]
    fn invites_read_back() {
        for server in ["https://music.example.com:8443/octo", "https://music.example.com/octo/", "http://10.0.2.2:5274"] {
            let link = invite_link(server, "k3y/+");
            assert!(link.starts_with(&format!("{}/nori/jam#s=", server.trim_end_matches('/'))), "{link}");
            assert_eq!(parse_invite(&link), Some((server.into(), "k3y/+".into())), "{link}");
        }
        assert!(is_guest_key(&guest_key("abc")) && !is_guest_key("abc"));
        assert_ne!(new_id(), new_id());
    }

    #[test]
    fn invite_links_parse() {
        let octo = Some(("https://music.example.com/octo".to_string(), "k3y".to_string()));
        let cases = [
            ("https://music.example.com/octo/nori/jam#s=https%3A%2F%2Fmusic.example.com%2Focto&k=k3y", octo.clone()),
            (" HTTPS://music.example.com/octo/nori/jam/#k=k3y&s=https%3A%2F%2Fmusic.example.com%2Focto\n", octo.clone()),
            ("http://192.168.1.5:5274/nori/jam#s=http%3A%2F%2F192.168.1.5%3A5274&k=abc", Some(("http://192.168.1.5:5274".into(), "abc".into()))),
            ("nori://jam?s=https%3A%2F%2Fmusic.example.com%2Focto&k=k3y", octo),
            ("https://music.example.com/nori/jam#s=https%3A%2F%2Fmusic.example.com", None),
            ("https://music.example.com/nori/jam#s=https%3A%2F%2Fmusic.example.com&k=", None),
            ("https://music.example.com/nori/jam?s=https%3A%2F%2Fmusic.example.com&k=k3y", None),
            ("https://music.example.com/other#s=x&k=k3y", None),
            ("ftp://music.example.com/nori/jam#s=x&k=k3y", None),
            ("nori://jam?s=x", None),
            ("nori://jam?s=&k=k", None),
            ("nori://jam?s=%zz&k=k", None),
            ("https://example.com", None),
            ("hello", None),
            ("", None),
        ];
        for (link, parsed) in cases {
            assert_eq!(parse_invite(link), parsed, "{link}");
            assert_eq!(is_invite(link), parsed.is_some(), "{link}");
        }
    }

    #[test]
    fn home_only_addresses() {
        let cases = [
            ("http://192.168.1.5:4533", true),
            ("http://10.0.0.2", true),
            ("https://172.16.4.1/navidrome", true),
            ("http://172.32.0.1", false),
            ("http://127.0.0.1:5274", true),
            ("http://localhost:4533", true),
            ("http://169.254.10.3", true),
            ("http://nas.local:4533", true),
            ("http://nas:4533", true),
            ("http://music.home.arpa", true),
            ("http://[fd12:3456::1]:4533", true),
            ("http://[fe80::1]", true),
            ("http://[::1]:4533", true),
            ("http://[::ffff:192.168.0.9]", true),
            ("http://[2001:db8::1]:4533", false),
            ("https://music.example.com", false),
            ("https://user:pw@music.example.com:8443/octo", false),
            ("http://100.101.102.103", false),
            ("http://8.8.8.8", false),
            ("", false),
        ];
        for (url, home) in cases {
            assert_eq!(is_home_only(url), home, "{url}");
        }
    }

    #[test]
    fn invite_fits_a_qr_code() {
        let q = qr_code(invite_link("https://music.example.com/octo", "0123456789abcdef0123456789abcdef")).unwrap();
        assert_eq!(q.dark.len(), (q.size * q.size) as usize);
        // The three finder patterns' corners are dark.
        let at = |x: u32, y: u32| q.dark[(y * q.size + x) as usize];
        assert!(at(0, 0) && at(q.size - 1, 0) && at(0, q.size - 1));
    }

    #[test]
    fn position_runs_on_while_playing() {
        let entries = vec![Entry { index: 4, duration: 10, ..Default::default() }];
        let st = DeviceState { playing: true, position_ms: 2_000, index: Some(4), entries, ..Default::default() };
        assert_eq!(position_now(&st, 1_500), 3_500);
        assert_eq!(position_now(&st, 60_000), 10_000, "not past the song's end");
        assert_eq!(position_now(&DeviceState { rate: Some(1.25), ..st.clone() }, 2_000), 4_500, "at the device's speed");
        assert_eq!(position_now(&DeviceState { buffering: true, ..st.clone() }, 1_500), 2_000, "waiting for audio");
        assert_eq!(position_now(&DeviceState { playing: false, ..st }, 1_500), 2_000);
    }
}
