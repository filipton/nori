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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations_counts_and_quality() {
        assert_eq!((duration(0), duration(187), duration(3723)), ("0:00".into(), "3:07".into(), "1:02:03".into()));
        assert_eq!((songs(1), songs_caption(2, 200)), ("1 song".into(), "2 songs · 3:20".into()));
        let s = Song { suffix: "flac".into(), bit_depth: 24, sampling_rate: 96000, ..Default::default() };
        assert_eq!(quality(&s).as_deref(), Some("FLAC 24/96.0"));
    }
}
