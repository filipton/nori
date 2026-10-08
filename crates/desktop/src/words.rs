//! User-facing wording for numbers and errors (mirrors the terminal's text.rs).

use nori_core::transport::{FailureKind, NetError};
use nori_core::Song;

/// "3:07", "1:02:03".
pub fn duration(seconds: i64) -> String {
    let s = seconds.max(0);
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

pub fn count(n: impl Into<u64>, one: &str, many: &str) -> String {
    let n = n.into();
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn songs(n: usize) -> String {
    count(n as u64, "song", "songs")
}

pub fn albums(n: u32) -> String {
    count(n, "album", "albums")
}

/// "12 songs · 48:10".
pub fn songs_caption(n: usize, seconds: u64) -> String {
    format!("{} · {}", songs(n), duration(seconds as i64))
}

/// "FLAC 24/96.0", "MP3 320 kbps".
pub fn quality(s: &Song) -> Option<String> {
    let suffix = s.suffix.to_lowercase();
    let lossless = matches!(suffix.as_str(), "flac" | "alac" | "wav" | "aiff" | "ape" | "wv" | "dsf" | "dff");
    let detail = if lossless && s.bit_depth > 0 {
        Some(format!("{}/{:?}", s.bit_depth, s.sampling_rate as f64 / 1000.0))
    } else {
        (s.bit_rate > 0).then(|| format!("{} kbps", s.bit_rate))
    };
    let parts: Vec<String> = [(!s.suffix.is_empty()).then(|| s.suffix.to_uppercase()), detail].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub fn net_error(e: &NetError) -> String {
    let said = match e {
        NetError::Transport { kind, .. } => match kind {
            FailureKind::Metered => "This server is set to Wi-Fi only",
            FailureKind::UnknownHost => "Server not found. Check the address.",
            FailureKind::Connect => "Nothing is answering at that address. Is the port right, and is the server running?",
            FailureKind::Timeout => "The server did not answer in time.",
            FailureKind::Tls => "The server's certificate was not accepted.",
            FailureKind::Cleartext => "Cleartext HTTP was refused; use https://",
            _ => "",
        },
        NetError::Http { status } => return format!("HTTP {status}"),
        NetError::Api { code: 40, .. } => "Wrong user name or password.",
        NetError::Api { code: 41, .. } => "This server does not support token authentication.",
        NetError::Api { code: 50, .. } => "This user is not allowed to do that.",
        NetError::Api { reason, .. } => return reason.clone(),
        NetError::Parse { .. } => "That address answered, but not like a Subsonic server. Check the URL (and any reverse-proxy path).",
        NetError::Db { reason } => return format!("database: {reason}"),
    };
    if said.is_empty() {
        let NetError::Transport { detail, .. } = e else { return String::new() };
        return detail.clone().unwrap_or_default();
    }
    said.to_string()
}

/// A line of a song's menu, for the actions the desktop's menu offers beyond playing.
pub fn song_action(a: &nori_core::menus::SongAction) -> Option<String> {
    use nori_core::menus::SongAction as A;
    Some(match a {
        A::Favourite { on: true } => "Add to Favorites".into(),
        A::Favourite { on: false } => "Remove from Favorites".into(),
        _ => return None,
    })
}

/// A mix's name, as its tile and page say it.
pub fn mix_name(n: nori_core::mixes::board::MixName) -> &'static str {
    use nori_core::mixes::board::MixName as M;
    match n {
        M::Favourites => "Favorites",
        M::QuickPicks => "Quick picks",
        M::Discover => "Discover",
        M::DiscoverWeekly => "Discover Weekly",
        M::ListenAgain => "Listen again",
        M::Top => "Your top songs",
    }
}

/// Under a mix's name: what it is made from.
pub fn mix_caption(favourites: bool) -> &'static str {
    if favourites { "Your favorite songs" } else { "Made for you" }
}


/// The devices panel's first row, which brings the music back.
pub const THIS_COMPUTER: &str = "This computer";

/// The player's strip while another device plays.
pub fn playing_on(device: &str) -> String {
    format!("Playing on {device}")
}

/// A place the music can play, as the devices panel lists it: this computer (`d` None) or a device of the
/// account with what it plays and its last refusal; ticked while the music plays there.
pub fn device_row(d: Option<&nori_core::remote::RemoteDevice>, active: bool) -> crate::DeviceRow {
    use nori_core::remote::wire::{DeviceKind, Refusal};
    let Some(d) = d else {
        return crate::DeviceRow { name: THIS_COMPUTER.into(), active, ..Default::default() };
    };
    let state = d.state.as_ref();
    let now = state.and_then(|s| s.entries.iter().find(|e| Some(e.index) == s.index));
    let line = now.map_or_else(|| "Not playing".to_string(), |e| format!("{} · {}", e.title, e.artist));
    let note = match d.refused {
        Some(Refusal::Stale) => "The queue changed there. Try again.",
        Some(Refusal::NotAllowed) => "That device said no.",
        Some(Refusal::Unknown) => "That is no longer there.",
        Some(Refusal::TooMany) => "Too many songs waiting.",
        None => "",
    };
    crate::DeviceRow {
        id: d.id.clone().into(),
        name: d.name.clone().into(),
        line: line.into(),
        kind: match d.kind {
            DeviceKind::Desktop => 0,
            DeviceKind::Phone | DeviceKind::Guest => 1,
            DeviceKind::Terminal => 2,
        },
        active,
        note: note.into(),
    }
}

/// Who listens in the jam: "2 listening".
pub fn jam_listening(n: usize) -> String {
    match n {
        0 => "No one yet".into(),
        n => format!("{n} listening"),
    }
}

/// The jam as the player's strip says it: "Jam · 2 listening".
pub fn jam_strip(n: usize) -> String {
    format!("Jam · {}", jam_listening(n).to_lowercase())
}

pub fn jam_asked(from: &str) -> String {
    format!("Asked by {from}")
}

/// Under a provider's song asked for: accepting it makes the server download it.
pub const JAM_DOWNLOADS: &str = "Downloaded to your server if accepted";

pub fn jam_role(role: nori_core::remote::wire::Role) -> &'static str {
    use nori_core::remote::wire::Role;
    match role {
        Role::Host => "Host",
        Role::Admin => "Admin",
        Role::Guest => "Guest",
    }
}

pub const JAM_UNSUPPORTED: &str = "Your server doesn't support jams yet. They need octo-fiesta with nori support in front of it.";
pub const JAM_FAILED: &str = "Couldn't start the jam. Jams need octo-fiesta in front of your server.";
pub const LINK_COPIED: &str = "Link copied";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations_and_counts() {
        assert_eq!((duration(0), duration(187), duration(3723)), ("0:00".into(), "3:07".into(), "1:02:03".into()));
        assert_eq!((songs(1), songs_caption(2, 200)), ("1 song".into(), "2 songs · 3:20".into()));
        let s = Song { suffix: "flac".into(), bit_depth: 24, sampling_rate: 96000, ..Default::default() };
        assert_eq!(quality(&s).as_deref(), Some("FLAC 24/96.0"));
    }
}
