//! Lyrics from the lyrics services for a song the server has no timed lyrics for: each answer scored
//! (trust.rs) and the best shown.
//!
//! The choice from last time is shown at once and kept unless it scored below [`LOW`], when the services
//! are asked again after [`LOW_RETRY_MS`]. Each service's own answers are cached (a hit for good, a miss
//! for [`MISS_KEPT_MS`]); the rest are asked in waves: the first wave together, the others only when it
//! missed or scored below [`WIDEN_BELOW`], untimed services only when nobody timed anything. A failing
//! service is rested, never cached as a miss. Provider songs are never looked up. Once the song is
//! measured, timed answers are also checked against its voice (sync.rs), and a sure offset goes out
//! with the lyrics. The lookup is one future: no thread.

use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use nori_model::{Lyrics, Song};
use nori_net::transport::Transport;
use nori_player::automix::vocal::VocalCurve;
use nori_settings::lyrics_sources::{LyricsLookup, LyricsOrigin, LyricsService};
use serde::{Deserialize, Serialize};

use crate::credits::strip_edges;
use crate::fit::{agree, plausible};
use crate::formats::{from_cache, timing, Timing};
use crate::services::{self, Ask, LyricsMemory, Lookup, Shared};
use crate::sync::{self, SyncCheck, SyncKind};
use crate::trust::{name_alike, score, with_sync, Named, Trust};

/// How many services are asked at once.
pub const AT_ONCE: usize = 6;
/// A miss is asked again after a week.
pub const MISS_KEPT_MS: i64 = 7 * 24 * 3_600_000;
/// An answer this sure is shown at once when evidenced (it names the song, or another agrees);
/// otherwise it waits for the first wave.
pub const SURE: f64 = 0.75;
/// Below this, the first wave's best has the other services asked.
pub const WIDEN_BELOW: f64 = 0.82;
/// Never shown below this.
pub const FLOOR: f64 = 0.4;
/// What is shown is replaced by an answer that agrees with it and scores this much more...
pub const MARGIN: f64 = 0.03;
/// ...or by one that does not agree and scores this much more.
pub const OVERRULE: f64 = 0.2;
/// A choice scored below this is asked about again after [`LOW_RETRY_MS`].
pub const LOW: f64 = 0.7;
pub const LOW_RETRY_MS: i64 = 3 * 24 * 3_600_000;
/// The user's ranking breaks near ties: the first service gets this much, the last nothing.
const RANK_BONUS: f64 = 0.03;
/// A service that failed for a song is not asked about it again for this long.
const FAILED_REST: Duration = Duration::from_secs(30 * 60);
/// A service that failed this many songs in a row rests...
const FAILURES_IN_A_ROW: u32 = 3;
/// ...for this long, for every song.
const SERVICE_REST: Duration = Duration::from_secs(10 * 60);

/// Lyrics to show, and where they came from.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct LyricsPick {
    pub lyrics: Lyrics,
    pub origin: LyricsOrigin,
}

/// Where the platform is handed each set of lyrics to show.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait LyricsShown: Send + Sync {
    fn show(&self, pick: LyricsPick);
}

/// Whether `a` and `b` show the same: words, timing, offset and origin; the clock key does not count.
pub fn same_lyrics(a: &LyricsPick, b: &LyricsPick) -> bool {
    let (x, y) = (&a.lyrics, &b.lyrics);
    a.origin == b.origin && x.synced == y.synced && x.word_timed == y.word_timed && x.offset_ms == y.offset_ms && x.lines == y.lines
}

/// [`same_lyrics`] for a platform holding the answers itself.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn lyrics_same(a: LyricsPick, b: LyricsPick) -> bool {
    same_lyrics(&a, &b)
}

/// Whether `next` replaces what is `shown`: not when it is the same lyrics read again, which would
/// restart their fade and clock.
pub fn lyrics_replaces(shown: Option<&LyricsPick>, next: &LyricsPick) -> bool {
    shown.is_none_or(|s| !same_lyrics(s, next))
}

/// Where answers are remembered: the core's response cache.
pub trait LyricsCache: Send + Sync {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    /// Whether `key` was kept less than `max_age_ms` ago.
    fn fresh(&self, key: &str, max_age_ms: i64) -> bool;
    fn put(&self, key: &str, body: Vec<u8>);
    /// The song's vocal activity curve; none before it is measured.
    fn voice(&self, _song: &Song) -> Option<VocalCurve> {
        None
    }
}

/// The prefix of every lookup cache entry: what "Clear lyrics cache" empties.
pub const CACHE_PREFIX: &str = "lyrics|";

/// One service in a race: the finest timing it can answer with, its trust, and whether it is first wave.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub best: Timing,
    pub prior: f64,
    pub first_wave: bool,
}

impl Entry {
    pub fn of(s: LyricsService) -> Self {
        let best = match s.best() {
            3 => Timing::Words,
            2 => Timing::Lines,
            _ => Timing::Untimed,
        };
        Entry { best, prior: s.prior(), first_wave: s.first_wave() }
    }
}

/// Which answer is shown as answers come in. Services are ranked by position (0 best), which only breaks
/// near ties. Answers timed no better than the server's own lyrics (`server_timing`) never count.
pub struct Race {
    song: Song,
    entries: Vec<Entry>,
    prefer_words: bool,
    server_timing: Timing,
    done: Vec<bool>,
    waiting: Vec<usize>,
    answers: Vec<Option<(Lyrics, Named)>>,
    /// The rank on screen.
    shown: Option<usize>,
    /// The song's vocal curve, once measured, and each answer checked against it.
    voice: Option<VocalCurve>,
    checks: Vec<Option<SyncCheck>>,
}

impl Race {
    pub fn new(song: &Song, entries: Vec<Entry>, prefer_words: bool, server_timing: Timing) -> Self {
        let n = entries.len();
        Race { song: song.clone(), entries, prefer_words, server_timing, done: vec![false; n], waiting: (0..n).collect(), answers: vec![None; n], shown: None, voice: None, checks: vec![None; n] }
    }

    /// The song's vocal curve: every timed answer, in and to come, is checked against it.
    pub fn hear(&mut self, curve: VocalCurve) {
        self.checks = self.answers.iter().map(|a| a.as_ref().and_then(|(l, _)| sync::check(l, &curve))).collect();
        self.voice = Some(curve);
    }

    pub fn check(&self, rank: usize) -> Option<SyncCheck> {
        self.checks.get(rank).copied().flatten()
    }

    /// `rank`'s lyrics with the offset their check is sure of.
    fn to_screen(&self, rank: usize, mut lyrics: Lyrics) -> Lyrics {
        lyrics.offset_ms = self.check(rank).map_or(0, |c| c.applied_ms());
        lyrics
    }

    /// Every rank's score; none without an answer or one timed no better than the server's.
    pub fn scores(&self) -> Vec<Option<Trust>> {
        let n = self.entries.len().max(1) as f64;
        (0..self.entries.len())
            .map(|r| {
                let (l, named) = self.answers[r].as_ref()?;
                if timing(l) <= self.server_timing {
                    return None;
                }
                let others: Vec<(&Lyrics, &Named)> = self.answers.iter().enumerate().filter(|(o, _)| *o != r).filter_map(|(_, a)| a.as_ref().map(|a| (&a.0, &a.1))).collect();
                let mut t = with_sync(score(&self.song, l, named, self.entries[r].prior, &others, self.prefer_words), self.checks[r].as_ref());
                t.score = (t.score + RANK_BONUS * (1.0 - r as f64 / n)).min(1.0);
                Some(t)
            })
            .collect()
    }

    /// The best (rank, score), lowest rank on ties, among `ranks`.
    fn best_of(scores: &[Option<Trust>], ranks: impl Fn(usize) -> bool) -> Option<(usize, f64)> {
        scores.iter().enumerate().filter(|(r, _)| ranks(*r)).filter_map(|(r, t)| t.map(|t| (r, t.score))).fold(None, |best, (r, s)| match best {
            Some((_, b)) if b >= s => best,
            _ => Some((r, s)),
        })
    }

    /// The best answer clearing the [`FLOOR`].
    fn leader_in(scores: &[Option<Trust>]) -> Option<(usize, f64)> {
        Self::best_of(scores, |_| true).filter(|(_, s)| *s >= FLOOR)
    }

    pub fn leader(&self) -> Option<(usize, f64)> {
        Self::leader_in(&self.scores())
    }

    fn first_wave_done(&self) -> bool {
        (0..self.entries.len()).all(|r| self.done[r] || !self.entries[r].first_wave || self.entries[r].best <= self.server_timing)
    }

    fn timed_done(&self) -> bool {
        (0..self.entries.len()).all(|r| self.done[r] || self.entries[r].best < Timing::Lines)
    }

    /// `rank` answered, with lyrics and what it named, or with nothing.
    pub fn answer(&mut self, rank: usize, found: Option<(Lyrics, Named)>) {
        if self.done[rank] {
            return;
        }
        self.done[rank] = true;
        self.waiting.retain(|w| *w != rank);
        self.answers[rank] = found.filter(|(l, _)| !l.lines.is_empty());
        self.checks[rank] = self.voice.as_ref().zip(self.answers[rank].as_ref()).and_then(|(v, (l, _))| sync::check(l, v));
    }

    /// Lyrics already on screen as `rank`'s answer: last time's choice.
    pub fn shown_already(&mut self, rank: usize, lyrics: Lyrics, named: Named) {
        self.answer(rank, Some((lyrics, named)));
        self.shown = Some(rank);
    }

    /// Whether `rank`, not asked yet, may be asked now.
    fn may_ask(&self, rank: usize, leader: Option<(usize, f64)>) -> bool {
        let e = self.entries[rank];
        if e.best <= self.server_timing {
            return false;
        }
        if e.best < Timing::Lines {
            return self.timed_done() && leader.is_none();
        }
        e.first_wave || (self.first_wave_done() && (leader.is_none_or(|(_, s)| s < WIDEN_BELOW) || self.wants_words(rank, leader)))
    }

    /// Whether `rank` is still worth asking for word timing: words preferred, the leader only
    /// line-timed, and `rank` able to time words.
    fn wants_words(&self, rank: usize, leader: Option<(usize, f64)>) -> bool {
        self.prefer_words
            && self.entries[rank].best == Timing::Words
            && leader.is_some_and(|(r, _)| self.answers[r].as_ref().is_some_and(|(l, _)| timing(l) < Timing::Words))
    }

    /// Whether `rank`, not asked yet, can never be asked with the answers in hand.
    fn hopeless(&self, rank: usize, leader: Option<(usize, f64)>) -> bool {
        let e = self.entries[rank];
        e.best <= self.server_timing
            || (e.best >= Timing::Lines && !e.first_wave && self.first_wave_done() && leader.is_some_and(|(_, s)| s >= WIDEN_BELOW) && !self.wants_words(rank, leader))
            || (e.best < Timing::Lines && self.timed_done() && leader.is_some())
    }

    /// The ranks to ask now, best first, so that no more than `pool` are out; hopeless ones are dropped.
    pub fn next(&mut self, running: usize, pool: usize) -> Vec<usize> {
        let leader = self.leader();
        let hopeless: Vec<usize> = self.waiting.iter().copied().filter(|r| self.hopeless(*r, leader)).collect();
        for r in hopeless {
            self.answer(r, None);
        }
        let start: Vec<usize> = self.waiting.iter().copied().filter(|r| self.may_ask(*r, leader)).take(pool.saturating_sub(running)).collect();
        self.waiting.retain(|w| !start.contains(w));
        start
    }

    /// What to put on screen now. With nothing shown: the leader once sure, once the first wave is in, or
    /// at the `last`. Otherwise the leader only when strictly better and agreeing, or far better.
    pub fn to_show(&mut self, last: bool) -> Option<(usize, Lyrics)> {
        let scores = self.scores();
        let (leader, best) = Self::leader_in(&scores)?;
        let take = match self.shown {
            Some(r) if r == leader => false,
            Some(r) => {
                let had = scores[r].map_or(0.0, |t| t.score);
                let same = self.answers[r].as_ref().zip(self.answers[leader].as_ref()).is_some_and(|(a, b)| agree(&a.0, &b.0));
                (best > had + MARGIN && same) || best > had + OVERRULE
            }
            None => (best >= SURE && self.evidenced(leader)) || (self.first_wave_done() && best >= WIDEN_BELOW) || last,
        };
        if !take {
            return None;
        }
        self.shown = Some(leader);
        self.answers[leader].as_ref().map(|a| (leader, self.to_screen(leader, a.0.clone())))
    }

    /// Whether `rank`'s answer is backed by more than its service: it names this title, or another
    /// answer has the same words.
    fn evidenced(&self, rank: usize) -> bool {
        let Some((l, named)) = self.answers[rank].as_ref() else { return false };
        named.title.as_deref().is_some_and(|t| name_alike(t, &self.song.title) >= 0.85)
            || self.answers.iter().enumerate().any(|(o, a)| o != rank && a.as_ref().is_some_and(|a| agree(l, &a.0)))
    }

    /// What is on screen, with its score and what its service named.
    pub fn chosen(&self) -> Option<(usize, Trust, &Lyrics, &Named)> {
        let r = self.shown?;
        let t = self.scores()[r]?;
        let (l, n) = self.answers[r].as_ref()?;
        Some((r, t, l, n))
    }

    /// The best of the rest.
    pub fn runner_up(&self) -> Option<(usize, f64)> {
        Self::best_of(&self.scores(), |r| Some(r) != self.shown)
    }
}

/// A sync check in the log: "sync 0.93 shifted +500 ms (sure 0.80), drift +0 ms".
pub fn sync_words(c: &SyncCheck) -> String {
    let kind = match c.kind {
        SyncKind::Unsure => "unsure",
        SyncKind::Fits => "fits",
        SyncKind::Shifted => "shifted",
        SyncKind::Drifts => "drifts",
        SyncKind::Poor => "poor",
    };
    format!("sync {:.2} {kind} {:+} ms (sure {:.2}), drift {:+} ms", c.score, c.offset_ms, c.confidence, c.drift_ms)
}

/// A service's cache key for a song. With a BetterLyrics key given, a miss without one is asked again.
fn cache_key(service: LyricsService, song: &Song, lookup: &LyricsLookup) -> String {
    let keyed = matches!(service, LyricsService::BetterLyrics | LyricsService::Portato) && !lookup.better_lyrics_key.is_empty();
    format!("{CACHE_PREFIX}{}{}|{}|{}|{}", service.name(), if keyed { "+key" } else { "" }, song.artist, song.title, song.duration)
}

/// The cache key of the lyrics chosen for a song.
pub fn best_key(song: &Song) -> String {
    format!("{CACHE_PREFIX}BEST|{}|{}|{}", song.artist, song.title, song.duration)
}

/// Marks a cached answer (words and what the service named); older entries are formats.rs's cache form.
const ANSWER_MARK: &str = "nori-answer1:";

#[derive(Serialize, Deserialize)]
struct Kept {
    lyrics: Lyrics,
    #[serde(default)]
    named: Named,
    /// The chosen lyrics only: their service and score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
}

fn kept(lyrics: &Lyrics, named: &Named, chosen: Option<(LyricsService, f64)>) -> Vec<u8> {
    let k = Kept { lyrics: lyrics.clone(), named: named.clone(), source: chosen.map(|c| c.0.name().to_string()), score: chosen.map(|c| c.1) };
    format!("{ANSWER_MARK}{}", serde_json::to_string(&k).unwrap_or_default()).into_bytes()
}

fn read_kept(body: &[u8]) -> Option<Kept> {
    let text = String::from_utf8_lossy(body);
    match text.strip_prefix(ANSWER_MARK) {
        Some(json) => serde_json::from_str(json).ok(),
        None => Some(Kept { lyrics: from_cache(&text), named: Named::default(), source: None, score: None }),
    }
}

/// What a service answered before.
enum Remembered {
    Hit(Lyrics, Named),
    Miss,
    Unknown,
}

fn remembered(cache: &dyn LyricsCache, key: &str, song: &Song) -> Remembered {
    match cache.get(key) {
        // Credits are stripped again for entries cached before stripping; an entry that no longer reads
        // is asked again, and one that cannot be this song's is the miss it should have been.
        Some(b) if !b.is_empty() => match read_kept(&b).map(|mut k| {
            strip_edges(&mut k.lyrics, &song.title, &song.artist);
            k
        }) {
            Some(k) if k.lyrics.lines.is_empty() => Remembered::Unknown,
            Some(k) if plausible(&k.lyrics, song) => Remembered::Hit(k.lyrics, k.named),
            Some(_) => Remembered::Miss,
            None => Remembered::Unknown,
        },
        Some(_) if cache.fresh(key, MISS_KEPT_MS) => Remembered::Miss,
        _ => Remembered::Unknown,
    }
}

/// Last time's choice for `song`, while its service is still asked: rank, words, what it named, score.
fn chosen_before(cache: &dyn LyricsCache, song: &Song, services: &[LyricsService]) -> Option<(usize, Lyrics, Named, f64)> {
    let k = read_kept(&cache.get(&best_key(song))?)?;
    let rank = services.iter().position(|s| Some(s.name()) == k.source.as_deref())?;
    plausible(&k.lyrics, song).then(|| (rank, k.lyrics, k.named, k.score.unwrap_or(0.0)))
}

/// Services that failed lately: for which song (cache key) and when, and each one's failures in a row.
#[derive(Default)]
pub(crate) struct Failures {
    songs: Vec<(String, Instant)>,
    services: Vec<(LyricsService, u32, Option<Instant>)>,
}
/// How many songs' failures are remembered.
const FAILURES_KEPT: usize = 64;

impl Failures {
    /// Whether `service` should not be asked about the song under `key` now.
    fn resting(&self, service: LyricsService, key: &str, now: Instant) -> bool {
        let song = self.songs.iter().any(|(k, at)| k == key && now.duration_since(*at) < FAILED_REST);
        let all = self.services.iter().any(|(s, _, until)| *s == service && until.is_some_and(|u| now < u));
        song || all
    }

    fn failed(&mut self, service: LyricsService, key: String, now: Instant) {
        if self.songs.len() >= FAILURES_KEPT {
            self.songs.remove(0);
        }
        self.songs.push((key, now));
        match self.services.iter_mut().find(|(s, _, _)| *s == service) {
            Some(e) => {
                e.1 += 1;
                if e.1 >= FAILURES_IN_A_ROW {
                    e.1 = 0;
                    e.2 = Some(now + SERVICE_REST);
                }
            }
            None => self.services.push((service, 1, None)),
        }
    }

    fn answered(&mut self, service: LyricsService) {
        self.services.retain(|(s, _, _)| *s != service);
    }
}

/// Looks `song` up with the services `lookup` names, the server's own lyrics (described by
/// `server_has_lines`, `server_synced`) already shown. Each choice goes to `shown`; with nothing from
/// the server or anyone, an empty server answer goes out at the end. Returns the log line of the choice
/// ("lyrics: chose BiniLyrics (0.91, word-timed), runner-up LRCLIB (0.84)").
#[allow(clippy::too_many_arguments)]
pub async fn lookup(
    transport: &dyn Transport,
    cache: &dyn LyricsCache,
    song: &Song,
    server_has_lines: bool,
    server_synced: bool,
    lookup: &LyricsLookup,
    shown: &dyn LyricsShown,
    memory: &LyricsMemory,
) -> Option<String> {
    let none = || LyricsPick { lyrics: Lyrics::default(), origin: LyricsOrigin::Server };
    let mut services: Vec<LyricsService> = Vec::new();
    for s in &lookup.services {
        if !services.contains(s) {
            services.push(*s);
        }
    }
    if services.is_empty() || song.is_provider() || (server_has_lines && server_synced) {
        if !server_has_lines {
            shown.show(none());
        }
        return None;
    }
    let server_timing = if server_has_lines { Timing::Untimed } else { Timing::Empty };
    let mut race = Race::new(song, services.iter().map(|s| Entry::of(*s)).collect(), lookup.prefer_words, server_timing);
    let voice = cache.voice(song);
    if let Some(v) = voice.clone() {
        race.hear(v);
    }
    // Last time's choice: shown at once, and final unless it scored low (or no longer fits the voice)
    // and a few days passed.
    let mut before: Option<(usize, f64)> = None;
    if let Some((rank, lyrics, named, was)) = chosen_before(cache, song, &services).filter(|c| timing(&c.1) > server_timing) {
        let check = voice.as_ref().and_then(|v| sync::check(&lyrics, v));
        let mut on_screen = lyrics.clone();
        on_screen.offset_ms = check.map_or(0, |c| c.applied_ms());
        shown.show(LyricsPick { lyrics: on_screen, origin: services[rank].origin() });
        let mut line = format!("lyrics: kept {} ({was:.2}, {})", services[rank].title(), timing(&lyrics).words());
        if let Some(c) = &check {
            line.push_str(&format!(", {}", sync_words(c)));
        }
        let misfit = check.is_some_and(|c| matches!(c.kind, SyncKind::Drifts | SyncKind::Poor));
        if (was >= LOW && !misfit) || cache.fresh(&best_key(song), LOW_RETRY_MS) {
            nori_model::alog::info(&line);
            return Some(line);
        }
        race.shown_already(rank, lyrics, named);
        before = Some((rank, was));
    }
    let keys: Vec<String> = services.iter().map(|s| cache_key(*s, song, lookup)).collect();
    let now = Instant::now();
    for rank in 0..services.len() {
        if race.done[rank] || race.entries[rank].best <= server_timing {
            continue;
        }
        match remembered(cache, &keys[rank], song) {
            Remembered::Hit(l, n) => race.answer(rank, Some((l, n))),
            Remembered::Miss => race.answer(rank, None),
            Remembered::Unknown if memory.failures.lock().resting(services[rank], &keys[rank], now) => race.answer(rank, None),
            Remembered::Unknown => {}
        }
    }
    let shared = Shared::over(memory);
    let mut out = FuturesUnordered::new();
    loop {
        for rank in race.next(out.len(), AT_ONCE) {
            let (service, key, shared) = (services[rank], keys[rank].as_str(), &shared);
            out.push(async move { (rank, ask(transport, cache, lookup, shared, service, key, song).await) });
        }
        let last = out.is_empty();
        if let Some((rank, lyrics)) = race.to_show(last) {
            shown.show(LyricsPick { lyrics, origin: services[rank].origin() });
        }
        if last {
            break;
        }
        let Some((rank, answer)) = out.next().await else { break };
        race.answer(rank, answer);
    }
    drop(out);
    let Some((rank, trust, lyrics, named)) = race.chosen() else {
        if !server_has_lines {
            shown.show(none());
        }
        return None;
    };
    cache.put(&best_key(song), kept(lyrics, named, Some((services[rank], trust.score))));
    let mut line = format!("lyrics: chose {} ({:.2}, {})", services[rank].title(), trust.score, timing(lyrics).words());
    if let Some((r, s)) = race.runner_up() {
        line.push_str(&format!(", runner-up {} ({s:.2})", services[r].title()));
    }
    if let Some((r, was)) = before {
        line.push_str(&format!(", was {} ({was:.2})", services[r].title()));
    }
    for (r, service) in services.iter().enumerate() {
        if let Some(c) = race.check(r) {
            line.push_str(&format!("; {} {}", service.title(), sync_words(&c)));
        }
    }
    nori_model::alog::info(&line);
    Some(line)
}

/// Asks one service and caches its answer, credits stripped; a failure is only rested in memory.
async fn ask(transport: &dyn Transport, cache: &dyn LyricsCache, lookup: &LyricsLookup, shared: &Shared, service: LyricsService, key: &str, song: &Song) -> Option<(Lyrics, Named)> {
    let a = Ask::new(transport, lookup, shared, service);
    match services::ask(service, &a, song).await {
        Lookup::Found(mut l, named) => {
            shared.memory.failures.lock().answered(service);
            strip_edges(&mut l, &song.title, &song.artist);
            if l.lines.is_empty() || !plausible(&l, song) {
                nori_model::alog::info(&format!("{} lyrics do not fit the song: taken as a miss", service.name()));
                cache.put(key, Vec::new());
                return None;
            }
            cache.put(key, kept(&l, &named, None));
            Some((l, named))
        }
        Lookup::Missing => {
            shared.memory.failures.lock().answered(service);
            cache.put(key, Vec::new());
            None
        }
        Lookup::Failed => {
            shared.memory.failures.lock().failed(service, key.to_string(), Instant::now());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::tests::timed;
    use crate::formats::to_cache;
    use crate::services::tests::{block, song, Web};
    use parking_lot::Mutex;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};

    // ---- what the screen takes ---------------------------------------------------------------------------

    fn pick(text: &str, key: u64, origin: LyricsOrigin) -> LyricsPick {
        let line = nori_model::LyricLine { start_ms: 1000, end_ms: 2000, text: text.into(), ..Default::default() };
        LyricsPick { lyrics: Lyrics { synced: true, word_timed: false, lines: vec![line], key, offset_ms: 0 }, origin }
    }

    #[test]
    fn the_same_words_read_again_under_another_key_are_not_new() {
        let shown = pick("hold on", 7, LyricsOrigin::Lrclib);
        assert!(same_lyrics(&shown, &pick("hold on", 12, LyricsOrigin::Lrclib)), "the lookup run again keeps them under a new key");
        assert!(!same_lyrics(&shown, &pick("let go", 7, LyricsOrigin::Lrclib)), "other words");
        assert!(!same_lyrics(&shown, &pick("hold on", 7, LyricsOrigin::Unison)), "another source's");
        let mut untimed = pick("hold on", 7, LyricsOrigin::Lrclib);
        untimed.lyrics.synced = false;
        assert!(!same_lyrics(&shown, &untimed), "other timing");
        assert!(lyrics_replaces(None, &shown), "anything over nothing");
        assert!(!lyrics_replaces(Some(&shown), &pick("hold on", 12, LyricsOrigin::Lrclib)));
        assert!(lyrics_replaces(Some(&shown), &pick("hold on", 7, LyricsOrigin::Binilyrics)));
        assert!(lyrics_same(shown.clone(), shown));
    }

    // ---- the race, answer by answer ----------------------------------------------------------------------

    /// Six invented lines, and six others: two songs' words.
    const OURS: [&str; 6] = ["first line of the song", "line two goes here", "a third one follows", "and then the fourth", "the chorus comes around", "and the chorus ends"];
    const THEIRS: [&str; 6] = ["something else entirely", "nothing alike at all", "words of another tune", "a verse we never heard", "somebody else's chorus", "and a different ending"];

    fn tune() -> Song {
        Song { title: "Glass Harbour".into(), artist: "The Lanterns".into(), album: "Low Tide".into(), duration: 180, ..Default::default() }
    }

    /// `lines` over the song, a line every nine seconds from ten, word-timed or line-timed.
    fn words(lines: &[&str], word_timed: bool) -> Lyrics {
        let at: Vec<(i64, &str)> = (0..18).map(|i| (10_000 + i * 9_000, lines[i as usize % lines.len()])).collect();
        timed(&at, word_timed)
    }

    fn naming(title: &str) -> Named {
        Named::new(title, "The Lanterns", "", 180.0)
    }

    const W: Timing = Timing::Words;
    const L: Timing = Timing::Lines;
    const U: Timing = Timing::Untimed;

    fn entry(prior: f64, first_wave: bool, best: Timing) -> Entry {
        Entry { best, prior, first_wave }
    }

    #[test]
    fn a_wrong_song_from_a_loose_source_is_not_chosen() {
        // A service that matches loosely and names nothing answers first, word by word, with another song;
        // LRCLIB (line by line, naming the song) and Unison (word by word) have this one's words.
        let mut r = Race::new(&tune(), vec![entry(0.9, true, W), entry(0.85, true, W), entry(0.85, true, W)], true, Timing::Empty);
        assert_eq!(r.next(0, 6), [0, 1, 2]);
        r.answer(0, Some((words(&THEIRS, true), Named::default())));
        assert_eq!(r.to_show(false), None, "a lone answer naming nothing waits for the first wave");
        r.answer(1, Some((words(&OURS, false), naming("Glass Harbour"))));
        r.answer(2, Some((words(&OURS, true), Named::default())));
        let scores = r.scores();
        assert!(scores[0].unwrap().score + OVERRULE < scores[2].unwrap().score, "outvoted: {:?}", scores[0]);
        let (rank, _) = r.to_show(false).expect("shown");
        assert_eq!(rank, 2, "the word-timed answer the others agree with");
        assert!(r.runner_up().is_some_and(|(r, _)| r == 1));
    }

    #[test]
    fn a_good_line_timed_first_wave_still_asks_the_services_that_time_words() {
        // First wave: two services agreeing on this song's words, timed by line, so their best scores
        // well. Second wave: one that times words. With words preferred it is asked; without, the good
        // line-timed answer ends the search, as it did for every song before (the owner saw word-timed
        // lyrics only once every service was on).
        let wave = || vec![entry(0.95, true, W), entry(0.9, true, W), entry(0.75, false, W)];
        let answered = |prefer: bool| {
            let mut r = Race::new(&tune(), wave(), prefer, Timing::Empty);
            assert_eq!(r.next(0, 6), [0, 1]);
            r.answer(0, Some((words(&OURS, false), naming("Glass Harbour"))));
            r.answer(1, Some((words(&OURS, false), naming("Glass Harbour"))));
            r
        };
        let mut r = answered(true);
        assert!(r.leader().is_some_and(|(_, s)| s >= WIDEN_BELOW), "agreeing line-timed answers score well: {:?}", r.leader());
        assert_eq!(r.next(0, 6), [2], "the word-timing service is asked all the same");
        let mut lines_will_do = answered(false);
        assert!(lines_will_do.next(0, 6).is_empty(), "words not preferred: a good line-timed answer is enough");
    }

    #[test]
    fn a_line_timed_answer_naming_the_song_beats_word_timed_words_nobody_backs() {
        let mut r = Race::new(&tune(), vec![entry(0.85, true, W), entry(0.85, true, W)], true, Timing::Empty);
        r.next(0, 6);
        r.answer(0, Some((words(&OURS, false), naming("Glass Harbour"))));
        r.answer(1, Some((words(&THEIRS, true), Named::default())));
        assert_eq!(r.to_show(true).map(|x| x.0), Some(0), "the answer naming the song, though timed by line");
        // A third answer with the named one's words, timed word by word, then wins over both.
        let mut three = Race::new(&tune(), vec![entry(0.85, true, W), entry(0.85, true, W), entry(0.9, true, W)], true, Timing::Empty);
        three.next(0, 6);
        three.answer(0, Some((words(&OURS, false), naming("Glass Harbour"))));
        three.answer(1, Some((words(&THEIRS, true), Named::default())));
        three.answer(2, Some((words(&OURS, true), naming("Glass Harbour"))));
        assert_eq!(three.to_show(true).map(|x| x.0), Some(2));
    }

    #[test]
    fn a_junk_answer_loses_to_clean_lyrics() {
        // Word-timed, but one line over and over with a credit and a placeholder left in the middle.
        let junk: Vec<(i64, &str)> = (0..18).map(|i| (10_000 + i * 9_000, match i {
            6 => "Lyrics by: Some Person",
            12 => "[Instrumental]",
            _ => "la la la",
        })).collect();
        let mut r = Race::new(&tune(), vec![entry(0.9, true, W), entry(0.85, true, W)], true, Timing::Empty);
        r.next(0, 6);
        r.answer(0, Some((timed(&junk, true), naming("Glass Harbour"))));
        r.answer(1, Some((words(&OURS, false), naming("Glass Harbour"))));
        let s = r.scores();
        assert!(s[0].unwrap().penalty > 0.2, "{:?}", s[0]);
        assert_eq!(r.to_show(true).map(|x| x.0), Some(1));
    }

    #[test]
    fn the_second_wave_is_asked_only_when_the_first_misses_or_scores_low() {
        let entries = vec![entry(0.95, true, W), entry(0.85, true, L), entry(0.75, false, W), entry(0.7, false, U)];
        let mut good = Race::new(&tune(), entries.clone(), true, Timing::Empty);
        assert_eq!(good.next(0, 6), [0, 1], "the first wave only");
        good.answer(0, Some((words(&OURS, true), naming("Glass Harbour"))));
        good.answer(1, Some((words(&OURS, false), naming("Glass Harbour"))));
        assert!(good.leader().unwrap().1 >= WIDEN_BELOW);
        assert!(good.next(0, 6).is_empty(), "nobody else asked");
        let mut missed = Race::new(&tune(), entries.clone(), true, Timing::Empty);
        missed.next(0, 6);
        missed.answer(0, None);
        assert!(missed.next(1, 6).is_empty(), "not while the first wave is out");
        missed.answer(1, None);
        assert_eq!(missed.next(0, 6), [2], "the first wave missed: the next one, still timed");
        missed.answer(2, None);
        assert_eq!(missed.next(0, 6), [3], "untimed words last, when nobody timed anything");
        let mut low = Race::new(&tune(), entries, true, Timing::Empty);
        low.next(0, 6);
        low.answer(0, None);
        low.answer(1, Some((words(&OURS, false), Named::default())));
        assert!(low.leader().unwrap().1 < WIDEN_BELOW);
        assert_eq!(low.next(0, 6), [2], "a lone line-timed answer naming nothing: widened");
    }

    #[test]
    fn whats_shown_is_replaced_only_by_a_strictly_better_answer_that_agrees() {
        let mut r = Race::new(&tune(), vec![entry(0.85, true, W), entry(0.85, true, W), entry(0.85, true, W)], true, Timing::Empty);
        r.next(0, 6);
        r.answer(1, Some((words(&OURS, false), naming("Glass Harbour"))));
        assert_eq!(r.to_show(false).map(|x| x.0), Some(1), "sure, and naming the song: shown at once");
        let mut alike = words(&OURS, false);
        alike.lines[0].text = "first line of this song".into();
        r.answer(0, Some((alike, naming("Glass Harbour"))));
        assert_eq!(r.to_show(false), None, "timed alike and scored alike: no swap");
        r.answer(2, Some((words(&OURS, true), naming("Glass Harbour"))));
        assert_eq!(r.to_show(false).map(|x| x.0), Some(2), "word timing of the same words: strictly better");
    }

    // ---- checked against the song's voice ----------------------------------------------------------------

    /// A synthetic song with a sung line at known times, its curve, and a song record of its length.
    fn measured() -> (Song, nori_player::automix::vocal::VocalCurve, Vec<Vec<(f64, f64)>>) {
        use nori_player::automix::eval::{Song as Synthetic, Style, FULL, SUNG};
        let synthetic = Synthetic { sections: vec![(4, FULL), (12, SUNG), (6, FULL), (12, SUNG), (4, FULL)], ..Synthetic::new("race", Style::Backbeat, 112.0, 2, false) };
        let (curve, phrases, secs) = crate::sync::tests::sung(&synthetic);
        (Song { duration: secs.round() as u32, ..tune() }, curve, phrases)
    }

    #[test]
    fn with_the_songs_voice_timing_that_fits_it_beats_timing_that_does_not() {
        let (s, curve, phrases) = measured();
        let named = Named::new("Glass Harbour", "The Lanterns", "", s.duration as f64);
        let fits = crate::sync::tests::lyrics(&phrases, true, false, &|t| t);
        // The same words, timed by word, but a bar and a half late in the second half: another version's.
        let mid = phrases[phrases.len() / 2][0].0 - 0.1;
        let other_version = crate::sync::tests::lyrics(&phrases, true, true, &|t| if t < mid { t } else { t + 3.2 });
        let race = |voice: bool| {
            let mut r = Race::new(&s, vec![entry(0.9, true, W), entry(0.85, true, W)], true, Timing::Empty);
            if voice {
                r.hear(curve.clone());
            }
            r.next(0, 6);
            r.answer(0, Some((other_version.clone(), named.clone())));
            r.answer(1, Some((fits.clone(), named.clone())));
            r
        };
        let mut deaf = race(false);
        assert_eq!(deaf.to_show(true).map(|x| x.0), Some(0), "unmeasured, word timing wins as before");
        assert!(deaf.scores()[0].unwrap().sync.is_none());
        let mut hearing = race(true);
        let scores = hearing.scores();
        assert_eq!(hearing.check(0).map(|c| c.kind), Some(SyncKind::Drifts), "{:?}", hearing.check(0));
        assert_eq!(hearing.check(1).map(|c| c.kind), Some(SyncKind::Fits), "{:?}", hearing.check(1));
        assert!(scores[1].unwrap().score > scores[0].unwrap().score, "{scores:?}");
        assert_eq!(hearing.to_show(true).map(|x| x.0), Some(1), "the timing that fits the voice");
    }

    #[test]
    fn lyrics_that_run_late_go_out_with_their_offset() {
        let (s, curve, phrases) = measured();
        let late = crate::sync::tests::lyrics(&phrases, true, false, &|t| t + 1.0);
        let mut r = Race::new(&s, vec![entry(0.9, true, W)], true, Timing::Empty);
        r.hear(curve);
        r.next(0, 6);
        r.answer(0, Some((late, Named::new("Glass Harbour", "The Lanterns", "", s.duration as f64))));
        let (_, shown) = r.to_show(true).expect("shown");
        assert!((shown.offset_ms - 1000).abs() < 120, "{}", shown.offset_ms);
        assert!(sync_words(&r.check(0).unwrap()).starts_with("sync 0."), "{}", sync_words(&r.check(0).unwrap()));
        assert!(!same_lyrics(&LyricsPick { lyrics: shown.clone(), origin: LyricsOrigin::Lrclib }, &LyricsPick { lyrics: Lyrics { offset_ms: 0, ..shown }, origin: LyricsOrigin::Lrclib }), "another offset is other lyrics to show");
    }

    #[test]
    fn the_servers_own_untimed_words_are_never_replaced_by_untimed_ones() {
        let mut r = Race::new(&tune(), vec![entry(0.85, true, W), entry(0.7, false, U)], true, Timing::Untimed);
        assert_eq!(r.next(0, 6), [0]);
        r.answer(0, None);
        assert!(r.next(0, 6).is_empty(), "an untimed service cannot beat the server's words");
        let mut plain = words(&OURS, false);
        plain.synced = false;
        let mut s = Race::new(&tune(), vec![entry(0.85, true, U)], true, U);
        s.answer(0, Some((plain, Named::default())));
        assert_eq!(s.to_show(true), None);
    }

    // ---- the lookup, through the fake web and cache ----------------------------------------------------

    /// The response cache, in memory; keys in `old` are as old as can be.
    #[derive(Default)]
    struct Kept(Mutex<HashMap<String, Vec<u8>>>, Mutex<HashSet<String>>);

    impl LyricsCache for Kept {
        fn get(&self, key: &str) -> Option<Vec<u8>> {
            self.0.lock().get(key).cloned()
        }
        fn fresh(&self, key: &str, _max_age_ms: i64) -> bool {
            self.0.lock().contains_key(key) && !self.1.lock().contains(key)
        }
        fn put(&self, key: &str, body: Vec<u8>) {
            self.1.lock().remove(key);
            self.0.lock().insert(key.to_string(), body);
        }
    }

    #[derive(Default)]
    struct Screen(Mutex<Vec<LyricsPick>>);

    impl LyricsShown for Screen {
        fn show(&self, pick: LyricsPick) {
            self.0.lock().push(pick);
        }
    }

    fn asked(services: &[LyricsService]) -> LyricsLookup {
        LyricsLookup { services: services.to_vec(), prefer_words: true, paxsenix_key: String::new(), better_lyrics_key: String::new() }
    }

    fn run_saying(web: &Web, cache: &Kept, s: &Song, server: (bool, bool), l: &LyricsLookup) -> (Vec<LyricsPick>, Option<String>) {
        run_remembering(web, cache, s, server, l, &LyricsMemory::default())
    }

    /// A lookup by a client that remembers `memory` of its lookups before.
    fn run_remembering(web: &Web, cache: &Kept, s: &Song, server: (bool, bool), l: &LyricsLookup, memory: &LyricsMemory) -> (Vec<LyricsPick>, Option<String>) {
        let screen = Screen::default();
        let said = block(lookup(web, cache, s, server.0, server.1, l, &screen, memory));
        (screen.0.into_inner(), said)
    }

    fn run(web: &Web, cache: &Kept, s: &Song, server: (bool, bool), l: &LyricsLookup) -> Vec<LyricsPick> {
        run_saying(web, cache, s, server, l).0
    }

    const VERSE: [&str; 6] = OURS;

    /// `lines` as LRC across the song, a line every ten seconds; `words` times each word inline.
    fn lrc(lines: &[&str], words: bool) -> String {
        let at = |ms: usize| format!("{:02}:{:02}.{:02}", ms / 60_000, ms / 1000 % 60, ms % 1000 / 10);
        let mut out = String::new();
        for (i, line) in lines.iter().cycle().take(18).enumerate() {
            let ms = 10_000 + i * 10_000;
            out.push_str(&format!("[{}]", at(ms)));
            if words {
                for (k, w) in line.split(' ').enumerate() {
                    out.push_str(&format!("<{}>{w} ", at(ms + k * 400)));
                }
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        out
    }

    fn lrclib_synced(web: &Web, body: &str) {
        web.answer("https://lrclib.net/api/get", 200, &json!({"syncedLyrics": body}).to_string());
    }

    fn unison(web: &Web, body: &str) {
        web.answer("https://unison.boidu.dev/", 200, &json!({"success": true, "data": {"lyrics": body, "format": "lrc", "duration": 239, "title": "Glass Harbour", "artist": "The Lanterns"}}).to_string());
    }

    #[test]
    fn a_song_is_asked_once_and_the_choice_is_kept_with_its_score() {
        let (web, cache) = (Web::default(), Kept::default());
        unison(&web, &lrc(&VERSE, true));
        // LRCLIB has nothing: a 404 to the exact lookup and an empty search (a miss, not a failure).
        web.answer("https://lrclib.net/api/search", 200, "[]");
        web.answer("https://lrclib.net/", 404, r#"{"statusCode":404}"#);
        let s = Song { title: "Remembered".into(), ..song() };
        let l = asked(&[LyricsService::Unison, LyricsService::Lrclib]);
        let s = Song { title: "Glass Harbour".into(), id: "remembered".into(), ..s };
        let (first, said) = run_saying(&web, &cache, &s, (false, false), &l);
        assert_eq!(first.len(), 1);
        assert!(first[0].lyrics.word_timed && first[0].origin == LyricsOrigin::Unison);
        let said = said.unwrap();
        assert!(said.starts_with("lyrics: chose Unison (0.") && said.contains("word-timed"), "{said}");
        let best = read_kept(&cache.get(&best_key(&s)).unwrap()).unwrap();
        assert_eq!((best.source.as_deref(), best.lyrics.lines.len()), (Some("UNISON"), 18));
        assert!(best.score.unwrap() >= LOW);
        let asked_first = web.asked().len();
        let (again, kept) = run_saying(&web, &cache, &s, (false, false), &l);
        assert_eq!(again, first, "the same answer");
        assert!(kept.unwrap().starts_with("lyrics: kept Unison"));
        assert_eq!(web.asked().len(), asked_first, "and nothing asked again: it came out of the cache");
    }

    #[test]
    fn nothing_is_asked_for_synced_server_lyrics_a_provider_song_or_with_no_service() {
        let (web, cache) = (Web::default(), Kept::default());
        let l = asked(&[LyricsService::Unison]);
        assert!(run(&web, &cache, &song(), (true, true), &l).is_empty());
        let ext = Song { id: "ext-deezer-1".into(), ..song() };
        assert_eq!(run(&web, &cache, &ext, (false, false), &l), [LyricsPick { lyrics: Lyrics::default(), origin: LyricsOrigin::Server }]);
        assert_eq!(run(&web, &cache, &song(), (false, false), &asked(&[])).len(), 1, "nothing asked: the empty answer, once");
        assert!(web.asked().is_empty());
    }

    #[test]
    fn a_failure_is_not_kept_but_not_asked_again_at_once() {
        let (web, cache) = (Web::default(), Kept::default());
        let s = Song { title: "Failing".into(), ..song() };
        let l = asked(&[LyricsService::Unison]);
        web.answer("https://unison.boidu.dev/", 503, "busy");
        let memory = LyricsMemory::default();
        assert_eq!(run_remembering(&web, &cache, &s, (false, false), &l, &memory).0[0].origin, LyricsOrigin::Server);
        assert!(cache.0.lock().is_empty(), "a failure is never an answer");
        run_remembering(&web, &cache, &s, (false, false), &l, &memory);
        assert_eq!(web.asked().len(), 1, "the same song is not asked again straight away");
        // Another client's lookups remember nothing of it.
        run_remembering(&web, &cache, &s, (false, false), &l, &LyricsMemory::default());
        assert_eq!(web.asked().len(), 2, "each client its own memory");
    }

    #[test]
    fn word_timed_words_win_over_line_timed_ones() {
        let (web, cache) = (Web::default(), Kept::default());
        unison(&web, &lrc(&VERSE, true));
        lrclib_synced(&web, &lrc(&VERSE, false));
        let s = Song { title: "Glass Harbour".into(), id: "both".into(), ..song() };
        let picks = run(&web, &cache, &s, (true, false), &asked(&[LyricsService::Lrclib, LyricsService::Unison]));
        assert_eq!(picks.last().unwrap().origin, LyricsOrigin::Unison);
        assert!(picks.last().unwrap().lyrics.word_timed);
    }

    #[test]
    fn another_songs_words_never_replace_the_lyrics_shown() {
        let (web, cache) = (Web::default(), Kept::default());
        // LRCLIB's lines; Unison, below it, times words - but of another song, and a fragment of one.
        lrclib_synced(&web, &lrc(&VERSE, false));
        unison(&web, &lrc(&THEIRS[..3], true));
        let s = Song { title: "Glass Harbour".into(), id: "stable".into(), ..song() };
        let picks = run(&web, &cache, &s, (false, false), &asked(&[LyricsService::Lrclib, LyricsService::Unison]));
        assert_eq!(picks.len(), 1, "shown once: {:?}", picks.iter().map(|p| p.origin).collect::<Vec<_>>());
        assert_eq!((picks[0].origin, picks[0].lyrics.lines[0].text.as_str()), (LyricsOrigin::Lrclib, VERSE[0]));
    }

    #[test]
    fn a_fragment_is_a_miss_and_one_kept_before_the_check_is_not_shown() {
        let (web, cache) = (Web::default(), Kept::default());
        let s = Song { title: "Fragment".into(), ..song() };
        let only = asked(&[LyricsService::Unison]);
        unison(&web, "[00:05.00]<00:05.00>la <00:05.50>la\n[00:09.00]<00:09.00>line <00:09.50>two\n[00:13.00]three\n[00:17.00]<00:17.00>la <00:17.50>la\n");
        let picks = run(&web, &cache, &s, (false, false), &only);
        assert_eq!(picks, [LyricsPick { lyrics: Lyrics::default(), origin: LyricsOrigin::Server }], "nothing found");
        assert_eq!(cache.0.lock().values().next(), Some(&Vec::new()), "kept as a miss");
        // The same fragment, kept for good by a build before the check: not shown either.
        let (web, cache) = (Web::default(), Kept::default());
        let junk = crate::lyrics::from_lrc("[00:05.00]la la\n[00:09.00]line two\n[00:13.00]la la\n");
        cache.put(&cache_key(LyricsService::Unison, &s, &only), to_cache(&junk).into_bytes());
        assert_eq!(run(&web, &cache, &s, (false, false), &only)[0].origin, LyricsOrigin::Server);
        assert!(web.asked().is_empty(), "a miss for the week, not asked again");
    }

    #[test]
    fn credits_are_stripped_before_the_answer_is_scored_or_kept() {
        let (web, cache) = (Web::default(), Kept::default());
        let s = Song { title: "Glass Harbour".into(), id: "credited".into(), ..song() };
        let body = format!("[00:00.00]Lyrics by: Some Person\n[00:02.00]Composed by: Another Person\n{}[03:50.00]Transcribed by A. Listener\n", lrc(&VERSE, false));
        unison(&web, &body);
        let picks = run(&web, &cache, &s, (false, false), &asked(&[LyricsService::Unison]));
        let l = &picks.last().unwrap().lyrics;
        assert_eq!((l.lines.len(), l.lines[0].text.as_str(), l.lines[0].start_ms), (18, VERSE[0], 10_000));
        let kept = read_kept(&cache.get(&cache_key(LyricsService::Unison, &s, &asked(&[LyricsService::Unison]))).unwrap()).unwrap();
        assert_eq!(kept.lyrics.lines.len(), 18, "the cached copy is already clean");
    }

    #[test]
    fn the_chosen_lyrics_are_served_without_asking_even_with_more_services_on() {
        let (web, cache) = (Web::default(), Kept::default());
        let s = Song { title: "Glass Harbour".into(), id: "chosen".into(), ..song() };
        unison(&web, &lrc(&VERSE, true));
        run(&web, &cache, &s, (false, false), &asked(&[LyricsService::Unison]));
        let n = web.asked().len();
        lrclib_synced(&web, &lrc(&VERSE, false));
        let picks = run(&web, &cache, &s, (false, false), &asked(&[LyricsService::Lrclib, LyricsService::Unison]));
        assert_eq!(picks.iter().map(|p| p.origin).collect::<Vec<_>>(), [LyricsOrigin::Unison]);
        assert_eq!(web.asked().len(), n, "no network");
        // Its service switched off: the choice no longer stands, and the others are asked.
        let picks = run(&web, &cache, &s, (false, false), &asked(&[LyricsService::Lrclib]));
        assert_eq!(picks.last().unwrap().origin, LyricsOrigin::Lrclib);
    }

    #[test]
    fn a_low_scored_choice_is_asked_about_again_only_after_some_days() {
        let (web, cache) = (Web::default(), Kept::default());
        let s = Song { title: "Glass Harbour".into(), id: "low".into(), ..song() };
        let l = asked(&[LyricsService::Lrclib, LyricsService::Unison]);
        let lines = crate::lyrics::from_lrc(&lrc(&VERSE, false));
        cache.put(&best_key(&s), kept(&lines, &Named::default(), Some((LyricsService::Lrclib, 0.5))));
        unison(&web, &lrc(&VERSE, true));
        let picks = run(&web, &cache, &s, (false, false), &l);
        assert_eq!(picks.iter().map(|p| p.origin).collect::<Vec<_>>(), [LyricsOrigin::Lrclib], "shown at once");
        assert!(web.asked().is_empty(), "low, but kept only lately: not asked again yet");
        cache.1.lock().insert(best_key(&s));
        let (picks, said) = run_saying(&web, &cache, &s, (false, false), &l);
        assert_eq!(picks.iter().map(|p| p.origin).collect::<Vec<_>>(), [LyricsOrigin::Lrclib, LyricsOrigin::Unison], "some days later: better lyrics replace them");
        assert!(said.unwrap().contains("was LRCLIB (0.50)"));
        let best = read_kept(&cache.get(&best_key(&s)).unwrap()).unwrap();
        assert_eq!(best.source.as_deref(), Some("UNISON"), "and the better choice is what is kept");
    }
}
