//! The terminal client's English: formatted times, sizes, decibels, counts, captions and messages.

use nori_core::beat_model::BeatFailure;
use nori_core::lyrics_sources::LyricsOrigin;
use nori_core::settings::{BandMark, EqBypass};
use nori_core::transport::{FailureKind, NetError};
use nori_core::{AlbumDetail, PlaylistDetail, PresetKind, Song};

/// "3:07", or "1:02:03" from an hour.
pub fn duration(s: i64) -> String {
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// A playback position or length in ms, as [`duration`].
pub fn clock(ms: i64) -> String {
    duration(ms.max(0) / 1000)
}

/// "1 song", "12 songs".
pub fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A list's caption: "12 songs · 48:10".
pub fn songs_caption(count: u32, seconds: u64) -> String {
    format!("{} · {}", self::count(count as u64, "song", "songs"), duration(seconds as i64))
}

/// A record's format from one of its songs: "FLAC 24/96.0", "MP3 320 kbps".
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

/// An album page's caption: "2019 · 12 songs · 48:10 · FLAC 16/44.1 · explicit".
pub fn album_caption(d: &AlbumDetail) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(5);
    if d.album.year > 0 {
        parts.push(d.album.year.to_string());
    }
    parts.push(count(d.songs.len() as u64, "song", "songs"));
    parts.push(duration(d.seconds as i64));
    if let Some(q) = d.songs.first().and_then(quality) {
        parts.push(q);
    }
    if d.album.explicit_status == "explicit" {
        parts.push("explicit".into());
    }
    parts.join(" · ")
}

/// A playlist page's caption: "12 songs · 48:10".
pub fn playlist_caption(d: &PlaylistDetail) -> String {
    songs_caption(d.songs.len() as u32, d.seconds)
}

/// Under an artist's name: "12 albums".
pub fn albums(n: u32) -> String {
    count(n as u64, "album", "albums")
}

/// `v` with `places` decimals, halves rounded up on the shortest decimal form like Java's `%.nf`
/// (62.5 is "63"; Rust's `{:.0}` rounds to even).
pub fn fixed(v: f64, places: usize, plus: bool) -> String {
    let rounded = half_up(v, places);
    let body = format!("{:.*}", places, rounded.abs());
    let sign = if v.is_sign_negative() { "-" } else if plus { "+" } else { "" };
    format!("{sign}{body}")
}

/// `v` rounded half up at `places` decimals, on its shortest decimal form.
fn half_up(v: f64, places: usize) -> f64 {
    let shortest = format!("{}", v.abs());
    let Some((whole, frac)) = shortest.split_once('.') else { return v };
    if frac.len() <= places {
        return v;
    }
    let up = frac.as_bytes()[places] >= b'5';
    let kept: f64 = format!("{whole}.{}", &frac[..places]).parse().unwrap_or(v.abs());
    let step = 10f64.powi(-(places as i32));
    let r = if up { kept + step } else { kept };
    if v.is_sign_negative() { -r } else { r }
}

/// A size: "850 B", "38 KB", "2.1 MB", "38 MB", "2.1 GB".
pub fn bytes(bytes: i64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1_048_576 => format!("{} KB", fixed(b as f64 / 1024.0, 0, false)),
        b if b < 10_485_760 => format!("{} MB", fixed(b as f64 / 1_048_576.0, 1, false)),
        b if b < 1_073_741_824 => format!("{} MB", fixed(b as f64 / 1_048_576.0, 0, false)),
        b => format!("{} GB", fixed(b as f64 / 1_073_741_824.0, 1, false)),
    }
}

/// A decibel figure with its sign: "+3.5", "-1.0", "+0.0".
pub fn signed_db(db: f32) -> String {
    fixed(if db == 0.0 { 0.0 } else { db as f64 }, 1, true)
}

/// How far the lyrics are nudged: "+0.5 s".
pub fn nudge(ms: i64) -> String {
    format!("{} s", fixed((ms as f32 / 1000.0) as f64, 1, true))
}

/// A band's frequency: "63", "1k", "2.5k", "12.5k".
pub fn hz(f: f32) -> String {
    if f >= 1000.0 {
        let k = format!("{:.3}", (f / 1000.0) as f64);
        format!("{}k", k.trim_end_matches('0').trim_end_matches('.'))
    } else {
        fixed(f as f64, 0, false)
    }
}

/// A band's label: its frequency and its mark ("1k L", "63 low shelf").
pub fn band(freq: f32, mark: BandMark) -> String {
    let mark = match mark {
        BandMark::None => "",
        BandMark::Left => " L",
        BandMark::Right => " R",
        BandMark::LowShelf => " ↙",
        BandMark::HighShelf => " ↗",
        BandMark::NoGain => " ∿",
    };
    format!("{}{mark}", hz(freq))
}

/// "Pre-amp -3.5 dB (automatic)".
pub fn preamp(db: f32, automatic: bool) -> String {
    format!("{} dB{}", signed_db(db), if automatic { " (automatic)" } else { "" })
}

/// "center", "L 30%", "R 5%".
pub fn balance(balance: f32) -> String {
    if balance == 0.0 {
        return "center".into();
    }
    format!("{} {}%", if balance < 0.0 { "L" } else { "R" }, fixed((balance.abs() * 100.0) as f64, 0, false))
}

/// "-1.0 dB".
pub fn ceiling(db: f32) -> String {
    format!("{} dB", fixed(db as f64, 1, false))
}

/// Where lyrics came from, for the credit line: "your server", "LRCLIB".
pub fn lyrics_origin(origin: LyricsOrigin) -> &'static str {
    match origin {
        LyricsOrigin::Server => "your server",
        LyricsOrigin::Binilyrics => "BiniLyrics",
        LyricsOrigin::BetterLyrics | LyricsOrigin::Portato => "BetterLyrics",
        LyricsOrigin::Paxsenix | LyricsOrigin::PaxsenixMusixmatch | LyricsOrigin::PaxsenixSpotify => "PaxSenix",
        LyricsOrigin::LyricsPlus => "LyricsPlus",
        LyricsOrigin::Simpmusic => "SimpMusic",
        LyricsOrigin::Unison => "Unison",
        LyricsOrigin::Netease => "NetEase",
        LyricsOrigin::Kugou => "KuGou",
        LyricsOrigin::Lrclib => "LRCLIB",
        LyricsOrigin::YoutubeCaptions => "YouTube",
        LyricsOrigin::Megalobiz => "Megalobiz",
        LyricsOrigin::YoutubeMusic => "YouTube Music",
        LyricsOrigin::Genius => "Genius",
    }
}

/// The credit line for lyrics from a lyrics service: "LRCLIB", "LRCLIB · not timed".
pub fn lyrics_credit(origin: LyricsOrigin, synced: bool) -> String {
    let source = lyrics_origin(origin);
    if synced { source.to_string() } else { format!("{source} · not timed") }
}

/// A built-in equalizer curve's name.
pub fn preset(kind: PresetKind) -> &'static str {
    match kind {
        PresetKind::Flat => "Flat",
        PresetKind::BassBoost => "Bass boost",
        PresetKind::BassCut => "Bass cut",
        PresetKind::TrebleBoost => "Treble boost",
        PresetKind::TrebleCut => "Treble cut",
        PresetKind::VocalBoost => "Vocal boost",
        PresetKind::Loudness => "Loudness",
        PresetKind::SmallSpeakers => "Small speakers",
    }
}

/// Why nothing on the equalizer screen reaches the sound.
pub fn eq_bypass(why: EqBypass) -> &'static str {
    match why {
        EqBypass::BitPerfect => "Bit-perfect USB output is active, so nothing here touches the audio.",
        EqBypass::Output => "No processing on this output: nothing here changes the sound. Turn it off in Settings, under Sound.",
    }
}

/// Why the beat model's download did not work.
pub fn beat_failure(why: BeatFailure) -> &'static str {
    match why {
        BeatFailure::Network => "the network failed",
        BeatFailure::WrongFile => "the file that arrived was not the right one",
        BeatFailure::Storage => "it could not be saved",
    }
}

/// A server or network failure in actionable words; unknown transport failures show their detail.
pub fn net_error(e: &NetError) -> String {
    let said = match e {
        NetError::Transport { kind, .. } => match kind {
            FailureKind::Metered => "This server is set to Wi-Fi only",
            FailureKind::UnknownHost => "Server not found. Check the address.",
            FailureKind::Connect => "Nothing is answering at that address. Is the port right, and is the server running?",
            FailureKind::Timeout => "The server did not answer in time.",
            FailureKind::Tls => {
                "The server's certificate was not accepted. If it is self-signed, turn on \"Accept self-signed certificate\"; if it needs a client certificate, import one."
            }
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

/// The server search failed; the offline results stay.
pub fn search_fallback(reason: Option<&str>) -> String {
    match reason {
        Some(r) => format!("Server search failed: {r} — showing offline results"),
        None => "Server search failed — showing offline results".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!((duration(0), duration(187), duration(3723)), ("0:00".into(), "3:07".into(), "1:02:03".into()));
        assert_eq!((signed_db(-0.0), signed_db(3.25), signed_db(-1.0)), ("+0.0".into(), "+3.3".into(), "-1.0".into()));
        assert_eq!((fixed(0.15, 1, false), fixed(62.5, 0, false), fixed(9.96, 1, false)), ("0.2".into(), "63".into(), "10.0".into()));
        assert_eq!((hz(62.5), hz(1000.0), hz(2500.0), hz(12_500.0)), ("63".into(), "1k".into(), "2.5k".into(), "12.5k".into()));
        assert_eq!(band(1000.0, BandMark::LowShelf), "1k ↙");
        assert_eq!((balance(0.0), balance(-0.3), balance(0.05)), ("center".into(), "L 30%".into(), "R 5%".into()));
        assert_eq!(nudge(-250), "-0.3 s");
        assert_eq!((bytes(850), bytes(38 * 1_048_576), bytes(2_254_857_830)), ("850 B".into(), "38 MB".into(), "2.1 GB".into()));
        assert_eq!((songs_caption(1, 200), albums(2)), ("1 song · 3:20".into(), "2 albums".into()));
        let s = Song { suffix: "flac".into(), bit_depth: 24, sampling_rate: 96000, ..Default::default() };
        assert_eq!(quality(&s).as_deref(), Some("FLAC 24/96.0"));
    }

    #[test]
    fn net_errors() {
        let t = |kind, detail: Option<&str>| net_error(&NetError::Transport { kind, detail: detail.map(str::to_string) });
        assert_eq!(t(FailureKind::Metered, None), "This server is set to Wi-Fi only");
        assert_eq!(t(FailureKind::UnknownHost, Some("x")), "Server not found. Check the address.");
        assert_eq!(t(FailureKind::Io, Some("raw")), "raw");
        assert_eq!(net_error(&NetError::Http { status: 522 }), "HTTP 522");
        assert_eq!(net_error(&NetError::Api { code: 40, reason: "x".into() }), "Wrong user name or password.");
        assert_eq!(net_error(&NetError::Api { code: 70, reason: "gone".into() }), "gone");
        assert!(net_error(&NetError::Parse { reason: "x".into() }).starts_with("That address answered"));
    }

    #[test]
    fn lyrics_credit_says_untimed() {
        assert_eq!(lyrics_credit(LyricsOrigin::Lrclib, true), "LRCLIB");
        assert_eq!(lyrics_credit(LyricsOrigin::Lrclib, false), "LRCLIB · not timed");
    }
}
