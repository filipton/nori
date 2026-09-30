//! Whether an answer can be this song's lyrics at all, and whether two answers are the same words:
//! a loose service answers with another song's words as if they were these.

use std::collections::HashSet;

use nori_model::{Lyrics, Song};

/// Timed lyrics may run this far past the song's end and still be its.
const PAST_END_MS: i64 = 10_000;
/// Fewer distinct lines than this is a fragment, not a song's lyrics...
const FEWEST_LINES: usize = 4;
/// ...unless timed across at least this share of the song (a chant repeating one line).
const SPREAD_SHARE: f64 = 0.5;
/// Two sets of lyrics are the same song's when each has at least this share of its words in the other.
const SHARED_WORDS: f64 = 0.5;

/// Text as it is compared: lower case, letters, digits and the marks on them, one space between words.
pub(crate) fn norm(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut gap = false;
    for c in v.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || matches!(c, '\u{300}'..='\u{36f}' | '\u{3099}'..='\u{309a}') {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// Scripts written without spaces between words: each of their characters counts as a word.
fn unspaced(c: char) -> bool {
    matches!(c, '\u{2E80}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{0E00}'..='\u{0E7F}')
}

/// The distinct words of a set of lyrics.
pub(crate) fn words(l: &Lyrics) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in &l.lines {
        for w in norm(&line.text).split(' ').filter(|w| !w.is_empty()) {
            if w.chars().any(unspaced) {
                out.extend(w.chars().map(String::from));
            } else {
                out.insert(w.to_string());
            }
        }
    }
    out
}

/// Whether `l` can be `song`'s lyrics: some words, no line past the song's end, and more than a
/// fragment unless sung across the song.
pub fn plausible(l: &Lyrics, song: &Song) -> bool {
    let texts: HashSet<String> = l.lines.iter().map(|x| norm(&x.text)).filter(|t| !t.is_empty()).collect();
    if texts.is_empty() {
        return false;
    }
    let timed: Vec<i64> = if l.synced { l.lines.iter().filter(|x| x.start_ms >= 0 && !x.text.trim().is_empty()).map(|x| x.start_ms).collect() } else { Vec::new() };
    let song_ms = i64::from(song.duration) * 1000;
    if song_ms > 0 && timed.iter().any(|t| *t > song_ms + PAST_END_MS) {
        return false;
    }
    if texts.len() >= FEWEST_LINES {
        return true;
    }
    let (Some(first), Some(last)) = (timed.iter().min(), timed.iter().max()) else { return false };
    song_ms > 0 && (last - first) as f64 >= song_ms as f64 * SPREAD_SHARE
}

/// Whether `a` and `b` are the same song's words: each has at least half its words in the other.
pub fn agree(a: &Lyrics, b: &Lyrics) -> bool {
    same_words(&words(a), &words(b))
}

/// [`agree`] over the [`words`] of each.
pub(crate) fn same_words(x: &HashSet<String>, y: &HashSet<String>) -> bool {
    if x.is_empty() || y.is_empty() {
        return false;
    }
    let shared = x.intersection(y).count() as f64;
    shared >= x.len() as f64 * SHARED_WORDS && shared >= y.len() as f64 * SHARED_WORDS
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nori_model::LyricLine;

    pub(crate) fn timed(lines: &[(i64, &str)], words: bool) -> Lyrics {
        let lines = lines.iter().map(|(ms, t)| LyricLine { start_ms: *ms, text: t.to_string(), ..Default::default() }).collect();
        Lyrics { synced: true, word_timed: words, lines, ..Default::default() }
    }

    /// A verse and a chorus over three minutes.
    pub(crate) fn song_words(words: bool) -> Lyrics {
        let text = [
            "Paper boats drift down the harbour",
            "Lanterns burning low tonight",
            "Every wave that takes you farther",
            "Brings the morning into sight",
            "Hold on, hold on to the water",
            "Hold on, hold on to the light",
        ];
        let lines: Vec<(i64, &str)> = (0..18).map(|i| (10_000 + i * 9_000, text[i as usize % text.len()])).collect();
        timed(&lines, words)
    }

    fn song() -> Song {
        Song { title: "Glass Harbour".into(), artist: "The Lanterns".into(), duration: 180, ..Default::default() }
    }

    #[test]
    fn repeated_fragment_is_implausible() {
        let junk: Vec<(i64, &str)> = (0..6).map(|i| (5_000 + i * 4_000, ["Pour another glass", "The whisky's on the table", "Drink until the morning"][i as usize % 3])).collect();
        assert!(!plausible(&timed(&junk, true), &song()), "three lines over thirty seconds of a three-minute song");
        assert!(plausible(&song_words(false), &song()));
        let chant: Vec<(i64, &str)> = (0..40).map(|i| (4_000 + i * 4_000, "Around the harbour")).collect();
        assert!(plausible(&timed(&chant, false), &song()), "one line sung all the way through is still the song");
        assert!(!plausible(&Lyrics::default(), &song()));
        assert!(!plausible(&timed(&[(1_000, "  "), (2_000, "♪")], false), &song()), "no words");
    }

    #[test]
    fn lines_past_end_are_implausible() {
        let mut long = song_words(true);
        long.lines.push(LyricLine { start_ms: 260_000, text: "a longer song".into(), ..Default::default() });
        assert!(!plausible(&long, &song()));
        assert!(plausible(&long, &Song { duration: 0, ..song() }), "an unknown length checks nothing");
    }

    #[test]
    fn agree_same_song_only() {
        let lines = song_words(false);
        let mut words = song_words(true);
        words.lines[0].text = "Paper boats drift down the harbor (ooh)".into();
        assert!(agree(&lines, &words));
        let other: Vec<(i64, &str)> = (0..6).map(|i| (5_000 + i * 4_000, ["Pour another glass", "The whisky's on the table", "Drink until the morning"][i as usize % 3])).collect();
        assert!(!agree(&lines, &timed(&other, true)));
        let kanji = timed(&[(1_000, "紙の舟が流れる"), (2_000, "港の灯り")], true);
        let spaced = timed(&[(1_000, "紙の 舟が 流れる"), (2_000, "港の灯り")], false);
        assert!(agree(&kanji, &spaced), "words without spaces are compared by character");
    }
}
