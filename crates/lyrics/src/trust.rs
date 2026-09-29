//! How far an answer is trusted to be this song's lyrics, and how good they are: one score from 0 to 1
//! per answer, from which race.rs picks the lyrics to show. It is made of
//!
//! - the metadata match: how close the title, artist and album the service named are to the song's
//!   (normalised, versions and features left out), and its length to the song's;
//! - the timing: word by word over line by line over not timed, and whether the times are plausible
//!   (in order, within the song, no long silences, the last line near the song's end);
//! - the agreement: how many of the other answers are the same words ([`agree`]), since many sources
//!   agreeing is strong evidence;
//! - the service's own reliability (`LyricsService::prior`);
//! - less for junk: lines repeated over and over, few lines, words in another script than every other
//!   answer's, disagreeing with every other answer while they agree among themselves, credits or
//!   placeholders left in the middle, and a service naming another title;
//! - once the song has been measured (its vocal activity curve, with the AutoMix analysis), whether the
//!   times fit where the voice is heard ([`with_sync`], sync.rs): a term of its own, and less for timing
//!   made for another version of the song or fitting the voice nowhere. Before that the score is as above.

use std::collections::HashSet;

use nori_model::{Lyrics, Song};
use serde::{Deserialize, Serialize};

use crate::credits::credits_inside;
use crate::fit::{agree, norm};
use crate::formats::{timing, Timing};
use crate::lrclib::clean;
use crate::sync::{SyncCheck, SyncKind};

/// What a service said about the song it found, where it said anything: none of it is known for a
/// service that answers with the words alone (it matched the song on its own side).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Named {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Seconds.
    pub duration_s: Option<f64>,
}

impl Named {
    pub fn new(title: &str, artist: &str, album: &str, duration_s: f64) -> Self {
        let some = |v: &str| Some(v.trim().to_string()).filter(|v| !v.is_empty() && v != "null");
        Named { title: some(title), artist: some(artist), album: some(album), duration_s: Some(duration_s).filter(|d| *d > 0.0) }
    }

    /// A length in seconds, or in milliseconds when it is that large.
    pub fn seconds(reported: f64) -> f64 {
        if reported > 10_000.0 {
            reported / 1000.0
        } else {
            reported
        }
    }
}

/// An answer's score and its parts, each 0 to 1 (the penalty is taken off).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Trust {
    pub score: f64,
    pub meta: f64,
    pub timing: f64,
    pub shape: f64,
    pub agreement: f64,
    pub prior: f64,
    pub penalty: f64,
    /// How well the times fit the voice (sync.rs), where they were checked; None otherwise.
    pub sync: Option<f64>,
}

const W_META: f64 = 0.20;
const W_TIMING: f64 = 0.20;
const W_AGREE: f64 = 0.30;
const W_PRIOR: f64 = 0.15;
const W_SHAPE: f64 = 0.15;

/// A part nothing is known about: the service matched the song on its side, which is worth something.
const UNKNOWN: f64 = 0.7;

/// Whether every letter is Latin (or not a letter): two such names can be compared.
pub(crate) fn latin(v: &str) -> bool {
    v.chars().all(|c| !c.is_alphabetic() || c <= '\u{024F}')
}

/// How alike two names are, 0 to 1: the same once normalised (and "feat." and version notes gone) is 1,
/// one containing the other 0.85, otherwise the share of words they have in common. Names in two scripts
/// cannot be compared: the part is unknown.
pub fn name_alike(a: &str, b: &str) -> f64 {
    let (x, y) = (norm(&clean(a)), norm(&clean(b)));
    let (x, y) = if x.is_empty() || y.is_empty() { (norm(a), norm(b)) } else { (x, y) };
    if x.is_empty() || y.is_empty() {
        return UNKNOWN;
    }
    if latin(&x) != latin(&y) {
        return UNKNOWN;
    }
    if x == y {
        return 1.0;
    }
    if x.contains(&y) || y.contains(&x) {
        return 0.85;
    }
    let (wx, wy): (HashSet<&str>, HashSet<&str>) = (x.split(' ').collect(), y.split(' ').collect());
    let shared = wx.intersection(&wy).count() as f64;
    2.0 * shared / (wx.len() + wy.len()) as f64 * 0.8
}

/// How well what the service named matches the song: title, artist, album and length, each weighed, an
/// unknown one counted as [`UNKNOWN`].
fn meta(song: &Song, named: &Named) -> f64 {
    let part = |v: Option<f64>| v.unwrap_or(UNKNOWN);
    let title = named.title.as_deref().map(|t| name_alike(t, &song.title));
    let artist = named.artist.as_deref().filter(|_| !song.artist.trim().is_empty()).map(|a| name_alike(a, &song.artist));
    let album = named.album.as_deref().filter(|_| !song.album.trim().is_empty()).map(|a| name_alike(a, &song.album));
    let length = named.duration_s.filter(|_| song.duration > 0).map(|d| {
        let off = (Named::seconds(d) - song.duration as f64).abs();
        (1.0 - (off - 1.0).max(0.0) / 9.0).clamp(0.0, 1.0)
    });
    0.45 * part(title) + 0.25 * part(artist) + 0.05 * part(album) + 0.25 * part(length)
}

/// Words over lines over untimed; without `prefer_words`, lines are nearly as good as words.
fn timing_part(l: &Lyrics, prefer_words: bool) -> f64 {
    match timing(l) {
        Timing::Words => 1.0,
        Timing::Lines if prefer_words => 0.7,
        Timing::Lines => 0.95,
        Timing::Untimed => 0.35,
        Timing::Empty => 0.0,
    }
}

/// Whether the times make sense for this song: in order, within its length, without long silences
/// between lines, starting in its first half and ending near its end. Untimed words: unknown.
fn shape(l: &Lyrics, song: &Song) -> f64 {
    let t: Vec<i64> = if l.synced { l.lines.iter().filter(|x| x.start_ms >= 0 && !x.text.trim().is_empty()).map(|x| x.start_ms).collect() } else { Vec::new() };
    if t.len() < 2 {
        return UNKNOWN;
    }
    let pairs = (t.len() - 1) as f64;
    let ordered = t.windows(2).filter(|w| w[1] >= w[0]).count() as f64 / pairs;
    let gaps = t.windows(2).filter(|w| (w[1] - w[0]).abs() <= 60_000).count() as f64 / pairs;
    let song_ms = i64::from(song.duration) * 1000;
    if song_ms <= 0 {
        return (ordered + gaps) / 2.0;
    }
    let within = t.iter().filter(|s| **s <= song_ms + 2_000).count() as f64 / t.len() as f64;
    let (first, last) = (*t.iter().min().unwrap_or(&0), *t.iter().max().unwrap_or(&0));
    let tail = song_ms - last;
    let end = match tail {
        _ if tail < -2_000 => 0.0,
        ..=45_000 => 1.0,
        _ => (1.0 - (tail - 45_000) as f64 / 75_000.0 * 0.7).max(0.3),
    };
    let start = if first as f64 <= song_ms as f64 * 0.6 { 1.0 } else { 0.3 };
    (ordered + gaps + within + end + start) / 5.0
}

/// The script most of the words are written in: Latin, Cyrillic, Greek, Arabic, Hebrew, Han, kana,
/// Hangul or Thai (a Japanese answer's kanji counts as kana when it has any).
fn script(l: &Lyrics) -> Option<u8> {
    let mut counts = [0usize; 9];
    for c in l.lines.iter().flat_map(|x| x.text.chars()).filter(|c| c.is_alphabetic()) {
        let k = match c {
            '\u{0}'..='\u{024F}' => 0,
            '\u{0400}'..='\u{04FF}' => 1,
            '\u{0370}'..='\u{03FF}' => 2,
            '\u{0600}'..='\u{06FF}' => 3,
            '\u{0590}'..='\u{05FF}' => 4,
            '\u{3040}'..='\u{30FF}' => 6,
            '\u{2E80}'..='\u{2FDF}' | '\u{3400}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' => 5,
            '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' => 7,
            '\u{0E00}'..='\u{0E7F}' => 8,
            _ => continue,
        };
        counts[k] += 1;
    }
    if counts[6] > 0 && counts[5] > 0 {
        counts[6] += counts[5];
        counts[5] = 0;
    }
    let (k, n) = counts.iter().enumerate().max_by_key(|(_, n)| **n)?;
    (*n > 0).then_some(k as u8)
}

/// Lines repeated over and over, and too few lines: what a fragment of another song, or a service's
/// filler, looks like.
fn junk(l: &Lyrics) -> f64 {
    let sung: Vec<String> = l.lines.iter().map(|x| norm(&x.text)).filter(|t| !t.is_empty()).collect();
    let distinct = sung.iter().collect::<HashSet<_>>().len();
    let mut p = 0.0;
    if sung.len() >= 8 {
        let ratio = distinct as f64 / sung.len() as f64;
        p += ((0.25 - ratio) / 0.25).max(0.0) * 0.2;
    }
    if distinct < 6 {
        p += (6 - distinct) as f64 / 6.0 * 0.1;
    }
    p + (credits_inside(l) as f64 * 0.05).min(0.15)
}

/// Taken off an answer whose service named a title that is not this song's (and in the same script).
const OTHER_TITLE: f64 = 0.15;
/// Taken off an answer in another script than every other answer.
const OTHER_SCRIPT: f64 = 0.2;
/// Taken off an answer that agrees with none of the others while at least two of them agree with each
/// other: the majority has other words.
const OUTVOTED: f64 = 0.2;

/// The score of `l`, from a service trusted `prior`, which named `named`; `others` are the other answers
/// to the same song, whose agreement counts.
pub fn score(song: &Song, l: &Lyrics, named: &Named, prior: f64, others: &[(&Lyrics, &Named)], prefer_words: bool) -> Trust {
    // An answer with the same words as one naming this song is this song's as surely, nearly: a service
    // that names nothing borrows the match of one that agrees with it.
    let backing = others.iter().filter(|(o, _)| agree(l, o)).map(|(_, n)| meta(song, n) * 0.95).fold(0.0, f64::max);
    let meta = meta(song, named).max(backing);
    let others: Vec<&Lyrics> = others.iter().map(|(o, _)| *o).collect();
    let timing = timing_part(l, prefer_words);
    let shape = shape(l, song);
    let agreeing = others.iter().filter(|o| agree(l, o)).count();
    let agreement = (agreeing as f64 + 0.5) / (others.len() as f64 + 1.0);
    let mut penalty = junk(l);
    if named.title.as_deref().is_some_and(|t| latin(t) == latin(&song.title) && name_alike(t, &song.title) < 0.5) {
        penalty += OTHER_TITLE;
    }
    let mine = script(l);
    let theirs: Vec<Option<u8>> = others.iter().map(|o| script(o)).collect();
    if !theirs.is_empty() && theirs.iter().all(|s| s.is_some() && *s != mine && *s == theirs[0]) && agreeing == 0 {
        penalty += OTHER_SCRIPT;
    }
    let outvoted = agreeing == 0 && others.iter().enumerate().any(|(i, a)| others[i + 1..].iter().any(|b| agree(a, b)));
    if outvoted {
        penalty += OUTVOTED;
    }
    let raw = W_META * meta + W_TIMING * timing + W_AGREE * agreement + W_PRIOR * prior + W_SHAPE * shape - penalty;
    Trust { score: raw.clamp(0.0, 1.0), meta, timing, shape, agreement, prior, penalty, sync: None }
}

/// How much of the score the sync check takes, once there is one. Measured on a real library (sync_tune.rs):
/// with another song's timing put in as a rival trusted 0.05 more than a song's best good answer, the good
/// one came out on top in 18 of 26 songs at 0.1, 21 at 0.25 and at 0.4; but above 0.25 answers the other
/// services agreed on lost their place to lone ones more often (19 of 26 on top at 0.25, 17 at 0.3).
const W_SYNC: f64 = 0.25;
/// Taken off lyrics timed for another version of the song (the halves want different offsets)...
const DRIFTS: f64 = 0.1;
/// ...and off lyrics that fit the voice at no offset.
const POOR_FIT: f64 = 0.1;

/// `t` with the sync check of its lyrics against the song's vocal curve taken in, when there is one: the
/// score so far keeps `1 - W_SYNC` of its weight and the check's score (at the offset shown) takes the rest,
/// less [`DRIFTS`] or [`POOR_FIT`]. An unsure check, or none (not timed, the song not measured yet), leaves
/// the score as it was.
pub fn with_sync(mut t: Trust, check: Option<&SyncCheck>) -> Trust {
    let Some(c) = check.filter(|c| c.kind != SyncKind::Unsure) else { return t };
    let off = match c.kind {
        SyncKind::Drifts => DRIFTS,
        SyncKind::Poor => POOR_FIT,
        _ => 0.0,
    };
    t.sync = Some(c.score);
    t.penalty += off;
    t.score = ((1.0 - W_SYNC) * t.score + W_SYNC * c.score - off).clamp(0.0, 1.0);
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::tests::{song_words, timed};

    fn song() -> Song {
        Song { title: "Glass Harbour".into(), artist: "The Lanterns".into(), album: "Paper Boats".into(), duration: 180, ..Default::default() }
    }

    fn named(title: &str, secs: f64) -> Named {
        Named::new(title, "The Lanterns", "", secs)
    }

    #[test]
    fn names_are_compared_once_normalised() {
        assert_eq!(name_alike("Glass Harbour (feat. Someone)", "glass harbour"), 1.0);
        assert_eq!(name_alike("Glass Harbour - 2011 Remaster", "Glass Harbour"), 1.0);
        assert!(name_alike("Glass Harbour Reprise", "Glass Harbour") >= 0.85);
        assert!(name_alike("Whisky on the Table", "Glass Harbour") < 0.2);
        assert_eq!(name_alike("ガラスの港", "Glass Harbour"), UNKNOWN, "two scripts: unknown");
    }

    #[test]
    fn word_timing_beats_line_timing_and_both_beat_plain_words() {
        let s = song();
        let n = named("Glass Harbour", 180.0);
        let words = score(&s, &song_words(true), &n, 0.85, &[], true).score;
        let lines = score(&s, &song_words(false), &n, 0.85, &[], true).score;
        let mut plain = song_words(false);
        plain.synced = false;
        let untimed = score(&s, &plain, &n, 0.85, &[], true).score;
        assert!(words > lines && lines > untimed, "{words} {lines} {untimed}");
        let even = score(&s, &song_words(false), &n, 0.85, &[], false).score;
        assert!(words - even < 0.02, "without preferring words, lines are nearly as good");
    }

    #[test]
    fn a_length_or_title_that_is_not_the_songs_costs() {
        let s = song();
        let right = score(&s, &song_words(true), &named("Glass Harbour", 180.0), 0.8, &[], true).score;
        let long = score(&s, &song_words(true), &named("Glass Harbour", 188.0), 0.8, &[], true).score;
        let other = score(&s, &song_words(true), &named("Whisky on the Table", 180.0), 0.8, &[], true).score;
        assert!(right > long && long > other, "{right} {long} {other}");
        assert!(right - other > 0.2);
    }

    #[test]
    fn times_past_the_end_out_of_order_or_ending_early_cost() {
        let s = song();
        let good = score(&s, &song_words(true), &Named::default(), 0.8, &[], true).score;
        let early: Vec<(i64, &str)> = (0..18).map(|i| (5_000 + i * 4_000, ["line one here", "line two here", "a third line", "line four", "the fifth one", "and a sixth"][i as usize % 6])).collect();
        let short = score(&s, &timed(&early, true), &Named::default(), 0.8, &[], true).score;
        let mut shuffled = song_words(true);
        shuffled.lines.reverse();
        let jumbled = score(&s, &shuffled, &Named::default(), 0.8, &[], true).score;
        assert!(good > short && good > jumbled, "{good} {short} {jumbled}");
    }

    #[test]
    fn answers_that_agree_lift_each_other() {
        let s = song();
        let (a, b) = (song_words(true), song_words(false));
        let other: Vec<(i64, &str)> = (0..18).map(|i| (10_000 + i * 9_000, ["something else entirely", "nothing alike at all", "words of another tune"][i as usize % 3])).collect();
        let other = timed(&other, true);
        let alone = score(&s, &a, &Named::default(), 0.8, &[], true).score;
        let backed = score(&s, &a, &Named::default(), 0.8, &[(&b, &Named::default())], true).score;
        let doubted = score(&s, &a, &Named::default(), 0.8, &[(&other, &Named::default())], true).score;
        assert!(backed > alone && alone > doubted, "{backed} {alone} {doubted}");
    }

    #[test]
    fn another_script_than_every_other_answer_costs() {
        let s = song();
        let lines: Vec<(i64, &str)> = (0..18).map(|i| (10_000 + i * 9_000, ["紙の舟が行く", "港の灯り", "波が遠く", "朝が来る", "水をつかむ", "光をつかむ"][i as usize % 6])).collect();
        let japanese = timed(&lines, true);
        let (a, b) = (song_words(true), song_words(false));
        let odd = score(&s, &japanese, &Named::default(), 0.8, &[(&a, &Named::default()), (&b, &Named::default())], true);
        assert!(odd.penalty >= OTHER_SCRIPT);
        let alone = score(&s, &japanese, &Named::default(), 0.8, &[], true);
        assert!(alone.penalty < OTHER_SCRIPT, "alone, a script is no evidence");
    }
}
