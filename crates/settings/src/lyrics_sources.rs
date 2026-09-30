//! The lyrics services the settings switch and rank, and which ones a lookup asks ([`lyrics_lookup`]).
//! Querying them is nori-lyrics'.

use crate::settings::StoredPrefs;

/// Where lyrics came from, for the credit line: the server or a [`LyricsService`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum LyricsOrigin {
    Server,
    Binilyrics,
    BetterLyrics,
    Paxsenix,
    LyricsPlus,
    Portato,
    PaxsenixMusixmatch,
    Simpmusic,
    Unison,
    Netease,
    Kugou,
    Lrclib,
    PaxsenixSpotify,
    YoutubeCaptions,
    Megalobiz,
    YoutubeMusic,
    Genius,
}

/// How finely lyrics are timed, worst to best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Timing {
    Empty,
    Untimed,
    Lines,
    Words,
}

impl Timing {
    /// The words for it in the log.
    pub fn words(self) -> &'static str {
        match self {
            Timing::Words => "word-timed",
            Timing::Lines => "line-timed",
            Timing::Untimed => "not timed",
            Timing::Empty => "empty",
        }
    }
}

/// A third-party lyrics service, asked after the server's own lyrics. Declared in default rank order,
/// best first: word-timed, then line-timed from LRCLIB on, untimed last (docs/features.md, "Lyrics
/// sources"). Rankings and switches are stored by [`name`](Self::name), so this order can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum LyricsService {
    Paxsenix,
    Binilyrics,
    Unison,
    BetterLyrics,
    Kugou,
    Netease,
    LyricsPlus,
    Simpmusic,
    Portato,
    PaxsenixMusixmatch,
    Lrclib,
    PaxsenixSpotify,
    YoutubeCaptions,
    Megalobiz,
    YoutubeMusic,
    Genius,
}

impl LyricsService {
    pub const ALL: [LyricsService; 16] = [
        LyricsService::Paxsenix,
        LyricsService::Binilyrics,
        LyricsService::Unison,
        LyricsService::BetterLyrics,
        LyricsService::Kugou,
        LyricsService::Netease,
        LyricsService::LyricsPlus,
        LyricsService::Simpmusic,
        LyricsService::Portato,
        LyricsService::PaxsenixMusixmatch,
        LyricsService::Lrclib,
        LyricsService::PaxsenixSpotify,
        LyricsService::YoutubeCaptions,
        LyricsService::Megalobiz,
        LyricsService::YoutubeMusic,
        LyricsService::Genius,
    ];

    /// Its stored and cache name.
    pub fn name(self) -> &'static str {
        match self {
            LyricsService::Binilyrics => "BINILYRICS",
            LyricsService::BetterLyrics => "BETTER_LYRICS",
            LyricsService::Paxsenix => "PAXSENIX",
            LyricsService::LyricsPlus => "LYRICS_PLUS",
            LyricsService::Portato => "PORTATO",
            LyricsService::PaxsenixMusixmatch => "PAXSENIX_MUSIXMATCH",
            LyricsService::Simpmusic => "SIMPMUSIC",
            LyricsService::Unison => "UNISON",
            LyricsService::Netease => "NETEASE",
            LyricsService::Kugou => "KUGOU",
            LyricsService::Lrclib => "LRCLIB",
            LyricsService::PaxsenixSpotify => "PAXSENIX_SPOTIFY",
            LyricsService::YoutubeCaptions => "YOUTUBE_CAPTIONS",
            LyricsService::Megalobiz => "MEGALOBIZ",
            LyricsService::YoutubeMusic => "YOUTUBE_MUSIC",
            LyricsService::Genius => "GENIUS",
        }
    }

    /// The service stored under `name`, in any case.
    pub fn named(name: &str) -> Option<LyricsService> {
        LyricsService::ALL.into_iter().find(|s| s.name().eq_ignore_ascii_case(name.trim()))
    }

    /// Its name in log lines.
    pub fn title(self) -> &'static str {
        match self {
            LyricsService::Binilyrics => "BiniLyrics",
            LyricsService::BetterLyrics => "BetterLyrics",
            LyricsService::Paxsenix => "PaxSenix",
            LyricsService::LyricsPlus => "LyricsPlus",
            LyricsService::Portato => "BetterLyrics Portato",
            LyricsService::PaxsenixMusixmatch => "PaxSenix: Musixmatch",
            LyricsService::Simpmusic => "SimpMusic",
            LyricsService::Unison => "Unison",
            LyricsService::Netease => "NetEase Cloud Music",
            LyricsService::Kugou => "KuGou",
            LyricsService::Lrclib => "LRCLIB",
            LyricsService::PaxsenixSpotify => "PaxSenix: Spotify",
            LyricsService::YoutubeCaptions => "YouTube captions",
            LyricsService::Megalobiz => "Megalobiz",
            LyricsService::YoutubeMusic => "YouTube Music",
            LyricsService::Genius => "Genius",
        }
    }

    /// The finest timing it can answer with.
    pub fn best(self) -> Timing {
        match self {
            LyricsService::YoutubeCaptions | LyricsService::Megalobiz => Timing::Lines,
            LyricsService::YoutubeMusic | LyricsService::Genius => Timing::Untimed,
            _ => Timing::Words,
        }
    }

    /// Needs the PaxSenix key; skipped without it.
    pub(crate) fn needs_key(self) -> bool {
        matches!(self, LyricsService::PaxsenixMusixmatch | LyricsService::PaxsenixSpotify)
    }

    /// Asked in a lookup's first wave: cheap and good. The rest are asked only when the first wave misses,
    /// scores low or lacks word timing (nori-lyrics' race.rs).
    pub fn first_wave(self) -> bool {
        matches!(
            self,
            LyricsService::Paxsenix | LyricsService::Binilyrics | LyricsService::Unison | LyricsService::Kugou | LyricsService::Simpmusic | LyricsService::Lrclib
        )
    }

    /// Prior trust in its answers, 0 to 1 (part of nori-lyrics' trust.rs score).
    pub fn prior(self) -> f64 {
        match self {
            LyricsService::Paxsenix => 0.95,
            LyricsService::Binilyrics | LyricsService::BetterLyrics => 0.9,
            LyricsService::Unison | LyricsService::Lrclib | LyricsService::PaxsenixMusixmatch | LyricsService::PaxsenixSpotify => 0.85,
            LyricsService::LyricsPlus => 0.8,
            LyricsService::Kugou | LyricsService::Netease | LyricsService::Simpmusic | LyricsService::Portato => 0.75,
            LyricsService::Genius => 0.7,
            LyricsService::YoutubeMusic => 0.6,
            LyricsService::Megalobiz => 0.55,
            LyricsService::YoutubeCaptions => 0.45,
        }
    }

    /// The credit line's name for it.
    pub fn origin(self) -> LyricsOrigin {
        match self {
            LyricsService::Binilyrics => LyricsOrigin::Binilyrics,
            LyricsService::BetterLyrics => LyricsOrigin::BetterLyrics,
            LyricsService::Paxsenix => LyricsOrigin::Paxsenix,
            LyricsService::LyricsPlus => LyricsOrigin::LyricsPlus,
            LyricsService::Portato => LyricsOrigin::Portato,
            LyricsService::PaxsenixMusixmatch => LyricsOrigin::PaxsenixMusixmatch,
            LyricsService::Simpmusic => LyricsOrigin::Simpmusic,
            LyricsService::Unison => LyricsOrigin::Unison,
            LyricsService::Netease => LyricsOrigin::Netease,
            LyricsService::Kugou => LyricsOrigin::Kugou,
            LyricsService::Lrclib => LyricsOrigin::Lrclib,
            LyricsService::PaxsenixSpotify => LyricsOrigin::PaxsenixSpotify,
            LyricsService::YoutubeCaptions => LyricsOrigin::YoutubeCaptions,
            LyricsService::Megalobiz => LyricsOrigin::Megalobiz,
            LyricsService::YoutubeMusic => LyricsOrigin::YoutubeMusic,
            LyricsService::Genius => LyricsOrigin::Genius,
        }
    }
}

/// Every service in default rank order. Also the default set switched on.
pub fn default_order() -> Vec<LyricsService> {
    LyricsService::ALL.to_vec()
}

/// A ranking completed: duplicates removed, and services missing from it inserted after their default
/// predecessor.
pub(crate) fn complete_order(stored: &[LyricsService]) -> Vec<LyricsService> {
    let mut order = distinct(stored.iter().copied());
    for (i, s) in LyricsService::ALL.into_iter().enumerate() {
        if !order.contains(&s) {
            let above = LyricsService::ALL[..i].iter().rev().find_map(|a| order.iter().position(|o| o == a));
            order.insert(above.map_or(0, |at| at + 1), s);
        }
    }
    order
}

/// The services a stored list names (`LRCLIB,KUGOU`), each once, in order; unknown names dropped.
pub fn parse(names: &str) -> Vec<LyricsService> {
    distinct(names.split(',').filter_map(LyricsService::named))
}

/// The stored form of a list: names joined by commas.
pub(crate) fn to_names(services: &[LyricsService]) -> String {
    services.iter().map(|s| s.name()).collect::<Vec<_>>().join(",")
}

fn distinct(services: impl Iterator<Item = LyricsService>) -> Vec<LyricsService> {
    let mut out = Vec::new();
    for s in services {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// The services switched on, in rank order.
pub(crate) fn switched_on(p: &StoredPrefs) -> Vec<LyricsService> {
    p.lyrics_order.iter().copied().filter(|s| p.lyrics_on.contains(s)).collect()
}

/// The ranking with `service` moved `by` places (-1 up), past switched-off neighbours too.
pub fn moved(p: &StoredPrefs, service: LyricsService, by: i32) -> Vec<LyricsService> {
    let Some(at) = p.lyrics_order.iter().position(|s| *s == service) else { return p.lyrics_order.clone() };
    placed(p, service, (at as i64 + by as i64).max(0) as usize)
}

/// The ranking with `service` moved to place `to` (past the end is last).
pub fn placed(p: &StoredPrefs, service: LyricsService, to: usize) -> Vec<LyricsService> {
    let mut order = complete_order(&p.lyrics_order);
    order.retain(|s| *s != service);
    order.insert(to.min(order.len()), service);
    order
}

/// What one lookup asks: the services in rank order, and the options and keys.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricsLookup {
    pub services: Vec<LyricsService>,
    pub prefer_words: bool,
    pub paxsenix_key: String,
    pub better_lyrics_key: String,
}

/// The lookup the settings allow: none unless both the lookups and online lyrics switches are on, and
/// no service whose key is missing.
pub fn lyrics_lookup(p: &StoredPrefs) -> LyricsLookup {
    let paxsenix_key = p.paxsenix_key.trim().to_string();
    let better_lyrics_key = p.better_lyrics_key.trim().to_string();
    let services = if p.third_party_lookups && p.lyrics_online { switched_on(p).into_iter().filter(|s| !s.needs_key() || !paxsenix_key.is_empty()).collect() } else { Vec::new() };
    LyricsLookup { services, prefer_words: p.lyrics_prefer_words, paxsenix_key, better_lyrics_key }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(v: &[&str]) -> Vec<LyricsService> {
        v.iter().map(|s| LyricsService::named(s).unwrap()).collect()
    }

    #[test]
    fn names_round_trip() {
        for s in LyricsService::ALL {
            assert_eq!(LyricsService::named(s.name()), Some(s));
            assert_eq!(LyricsService::named(&s.name().to_lowercase()), Some(s));
            assert!(!s.title().is_empty());
        }
        assert_eq!(LyricsService::named("MUSIXMATCH"), None, "a test build's service is not read");
    }

    #[test]
    fn default_lookup() {
        let on = StoredPrefs::default();
        assert!(lyrics_lookup(&StoredPrefs { third_party_lookups: false, ..on.clone() }).services.is_empty(), "off under the lookups switch");
        let first: Vec<LyricsService> = lyrics_lookup(&on).services.into_iter().filter(|s| s.first_wave()).collect();
        let mut want = vec![LyricsService::Paxsenix, LyricsService::Binilyrics, LyricsService::Unison, LyricsService::Kugou, LyricsService::Simpmusic, LyricsService::Lrclib];
        want.sort_by_key(|s| default_order().iter().position(|n| n == s));
        assert_eq!(first, want, "the cheap and good ones first");
        assert!(lyrics_lookup(&on).services.iter().all(|s| !s.needs_key()), "keyed services wait for a key");
        let off = StoredPrefs { lyrics_online: false, ..on };
        assert!(lyrics_lookup(&off).services.is_empty());
    }

    #[test]
    fn default_order_by_timing() {
        let order = default_order();
        assert_eq!(order.len(), 16);
        let rank = |s: LyricsService| order.iter().position(|o| *o == s).unwrap();
        // Word timing first, LRCLIB the first of those that time lines, untimed words last.
        let lrclib = rank(LyricsService::Lrclib);
        assert!(order[..lrclib].iter().all(|s| s.best() == Timing::Words), "only services that time words rank above LRCLIB");
        assert!(order[lrclib..].windows(2).all(|w| w[0].best() >= w[1].best() || w[0] == LyricsService::Lrclib), "then by line, then untimed");
        assert_eq!(order[14..], [LyricsService::YoutubeMusic, LyricsService::Genius]);
        assert!(LyricsService::ALL.into_iter().filter(|s| s.first_wave()).all(|s| s.best() == Timing::Words || s == LyricsService::Lrclib));
        assert!(LyricsService::ALL.into_iter().all(|s| (0.0..=1.0).contains(&s.prior())));
    }

    #[test]
    fn keyed_service_waits_for_key() {
        let p = StoredPrefs { third_party_lookups: true, lyrics_on: named(&["PAXSENIX_SPOTIFY", "LRCLIB"]), ..StoredPrefs::default() };
        assert_eq!(lyrics_lookup(&p).services, [LyricsService::Lrclib]);
        let keyed = StoredPrefs { paxsenix_key: " abc ".into(), ..p };
        let l = lyrics_lookup(&keyed);
        assert_eq!(l.services, [LyricsService::Lrclib, LyricsService::PaxsenixSpotify]);
        assert_eq!(l.paxsenix_key, "abc");
    }

    #[test]
    fn complete_order_inserts_new_services() {
        let order = complete_order(&parse("LRCLIB, UNISON,MUSIXMATCH,,LRCLIB"));
        assert_eq!(order.len(), 16);
        assert_eq!(order[..2], named(&["PAXSENIX", "BINILYRICS"]), "the ones ranked above everything stored come first");
        let at = |n: &str| order.iter().position(|o| o.name() == n).unwrap();
        assert!(at("LRCLIB") < at("UNISON"), "the stored order stands");
        assert_eq!(at("PAXSENIX_SPOTIFY"), at("LRCLIB") + 1, "put in after the one above it out of the box");
        assert_eq!(complete_order(&[]), default_order());
    }

    #[test]
    fn move_passes_switched_off_services() {
        let p = StoredPrefs { lyrics_on: named(&["NETEASE", "LRCLIB", "GENIUS"]), ..StoredPrefs::default() };
        let at = |o: &[LyricsService], n: &str| o.iter().position(|x| x.name() == n).unwrap();
        let order = moved(&p, LyricsService::Lrclib, -1);
        assert_eq!(at(&order, "LRCLIB"), at(&p.lyrics_order, "LRCLIB") - 1, "one place, past a service that is off");
        assert_eq!(at(&order, "PAXSENIX_MUSIXMATCH"), at(&p.lyrics_order, "LRCLIB"));
        let first = StoredPrefs { lyrics_order: placed(&p, LyricsService::Lrclib, 0), ..p.clone() };
        assert_eq!(first.lyrics_order[0], LyricsService::Lrclib);
        assert_eq!(moved(&first, LyricsService::Lrclib, -1), first.lyrics_order, "the first stays first");
        assert_eq!(switched_on(&first), [LyricsService::Lrclib, LyricsService::Netease, LyricsService::Genius]);
        let off = moved(&p, LyricsService::Kugou, 1);
        assert_eq!(at(&off, "KUGOU"), at(&p.lyrics_order, "KUGOU") + 1, "one switched off moves too");
    }

    #[test]
    fn place_moves_one_service() {
        let p = StoredPrefs::default();
        let last = placed(&p, LyricsService::Paxsenix, 99);
        assert_eq!(last.len(), 16);
        assert_eq!(last[15], LyricsService::Paxsenix);
        assert_eq!(last[..15], p.lyrics_order[1..]);
        let back = StoredPrefs { lyrics_order: last, ..p.clone() };
        assert_eq!(placed(&back, LyricsService::Paxsenix, 0), p.lyrics_order);
        let mid = placed(&p, LyricsService::Genius, 3);
        assert_eq!(mid[3], LyricsService::Genius);
        assert_eq!(mid.iter().filter(|n| **n == LyricsService::Genius).count(), 1);
    }
}
