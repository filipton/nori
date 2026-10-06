//! Remote control and jams. One account's devices control each other, and a jam's members ask its host
//! for songs, through one protocol (wire.rs): octo-fiesta's relay hub with rooms (the account's own
//! room, or a jam's), or straight over the LAN through a device's door (lan.rs). What a controlled device
//! admits is device.rs; a jam's members, roles and requests are jam.rs. No I/O but the door's sockets.

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

/// The scheme and host of an invite link; the app opens such links.
const INVITE: &str = "nori://jam";

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

/// The link (and QR code) a guest joins with: the relay's address and the jam's invite key.
pub fn invite_link(server: &str, invite: &str) -> String {
    format!("{INVITE}?s={}&k={}", encode(server), encode(invite))
}

/// The relay's address and the invite key of an invite link; None for anything else.
pub fn parse_invite(link: &str) -> Option<(String, String)> {
    let query = link.trim().strip_prefix(INVITE)?.strip_prefix('?')?;
    let get = |key: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).and_then(|(_, v)| decode(v));
    let (server, invite) = (get("s")?, get("k")?);
    (!server.is_empty() && !invite.is_empty()).then_some((server, invite))
}

/// Whether `link` is a jam invite, for the app to offer joining it.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn is_invite(link: &str) -> bool {
    parse_invite(link).is_some()
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
    if !state.playing {
        return state.position_ms;
    }
    let length = state.entries.iter().find(|e| Some(e.index) == state.index).map_or(i64::MAX, |e| e.duration as i64 * 1000);
    (state.position_ms + elapsed_ms.max(0)).min(length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::{DeviceState, Entry};

    #[test]
    fn invites_read_back() {
        let link = invite_link("https://music.example.com:8443/octo", "k3y/+");
        assert!(is_invite(&link));
        assert_eq!(parse_invite(&link), Some(("https://music.example.com:8443/octo".into(), "k3y/+".into())));
        for other in ["https://example.com", "nori://jam?s=x", "nori://jam?s=&k=k", "nori://jam?s=%zz&k=k"] {
            assert_eq!(parse_invite(other), None, "{other}");
        }
        assert!(is_guest_key(&guest_key("abc")) && !is_guest_key("abc"));
        assert_ne!(new_id(), new_id());
    }

    #[test]
    fn invite_fits_a_qr_code() {
        let link = "nori://jam?s=https%3A%2F%2Fmusic.example.com&k=0123456789abcdef0123456789abcdef";
        let q = qr_code(link.into()).unwrap();
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
        assert_eq!(position_now(&DeviceState { playing: false, ..st }, 1_500), 2_000);
    }
}
