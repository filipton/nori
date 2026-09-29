//! LRCLIB's addresses and records, and the title cleaning every service uses.

use nori_model::{Lyrics, Song};
use serde_json::Value;

use crate::services::{text, truthy};
use crate::{formats, lyrics};

const BASE: &str = "https://lrclib.net/api";
/// A reported length further than this from the song's is another recording.
pub const DURATION_SLACK_S: f64 = 4.0;

// ---- cleaning titles ------------------------------------------------------------------------------------
// A port of the regular expressions the Kotlin app used, kept to the same matches.

/// `\s` as Java's regular expressions have it.
fn regex_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r') || separator(c)
}

/// Unicode's space, line and paragraph separators.
fn separator(c: char) -> bool {
    matches!(c, ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

/// What `.` and `$` treat as a line end.
fn line_end(c: char) -> bool {
    matches!(c, '\n' | '\x0b' | '\x0c' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Unicode case folding for the letters that matter here: the long s and the Kelvin sign.
fn fold(c: char) -> char {
    match c {
        '\u{17f}' => 's',
        '\u{212a}' => 'k',
        c => c.to_ascii_lowercase(),
    }
}

/// The byte length of `word` at the start of `s`, ignoring case; `#` stands for a digit.
fn starts_with(s: &str, word: &str) -> Option<usize> {
    let mut it = s.char_indices();
    for w in word.chars() {
        let (_, c) = it.next()?;
        let ok = if w == '#' { c.is_ascii_digit() } else { fold(c) == w };
        if !ok {
            return None;
        }
    }
    Some(it.next().map_or(s.len(), |(i, _)| i))
}

fn starts_with_any(s: &str, words: &[&str]) -> Option<usize> {
    words.iter().find_map(|w| starts_with(s, w))
}

/// What may open a bracketed note: `feat.?|ft.?|with|remaster(ed)?|\d{4} remaster|live|mono|stereo`.
const BRACKETED: [&str; 8] = ["feat", "ft", "with", "remaster", "#### remaster", "live", "mono", "stereo"];
/// What may follow " - ": `remaster(ed)?|\d{4} remaster|live|single version|radio edit`.
const DASHED: [&str; 5] = ["remaster", "#### remaster", "live", "single version", "radio edit"];

/// Drops every `\s*[(\[](<BRACKETED>)[^)\]]*[)\]]`.
fn drop_bracketed(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut i = 0;
    while i < title.len() {
        let rest = &title[i..];
        let ws = rest.find(|c| !regex_space(c)).unwrap_or(rest.len());
        let open = &rest[ws..];
        if open.starts_with(['(', '[']) && starts_with_any(&open[1..], &BRACKETED).is_some() {
            if let Some(close) = open[1..].find([')', ']']) {
                i += ws + 1 + close + 1;
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Cuts at the first `\s+-\s+(<DASHED>).*$`.
fn drop_dashed(title: &str) -> &str {
    for (i, c) in title.char_indices() {
        if !regex_space(c) || title[..i].ends_with(regex_space) {
            continue;
        }
        let rest = &title[i..];
        let ws = rest.find(|c| !regex_space(c)).unwrap_or(rest.len());
        let Some(after) = rest[ws..].strip_prefix('-') else { continue };
        let ws2 = after.find(|c| !regex_space(c)).unwrap_or(after.len());
        if ws2 == 0 {
            continue;
        }
        let Some(n) = starts_with_any(&after[ws2..], &DASHED) else { continue };
        // `.*$`: the rest must run to the end of the text, or to a last line break.
        let tail = &after[ws2 + n..];
        match tail.find(line_end) {
            None => return &title[..i],
            Some(j) if matches!(&tail[j..], "\n" | "\r" | "\r\n" | "\x0b" | "\x0c" | "\u{85}" | "\u{2028}" | "\u{2029}") => return &title[..i],
            Some(_) => {}
        }
    }
    title
}

/// Kotlin's `trim()`.
fn trim(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, '\t'..='\r' | '\x1c'..='\x1f') || separator(c))
}

/// The title without "(feat. X)", "- Remastered 2011" and the like, which services rarely carry.
pub fn clean(title: &str) -> String {
    trim(drop_dashed(&drop_bracketed(title))).to_string()
}

/// `application/x-www-form-urlencoded`.
pub fn form_encode(out: &mut String, v: &str) {
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => out.push(b as char),
            b' ' => out.push('+'),
            _ => {
                const H: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(H[(b >> 4) as usize] as char);
                out.push(H[(b & 15) as usize] as char);
            }
        }
    }
}

/// LRCLIB's exact lookup of `song` by its cleaned `title`.
pub fn get_url(song: &Song, title: &str) -> String {
    let mut u = format!("{BASE}/get?artist_name=");
    form_encode(&mut u, &song.artist);
    u.push_str("&track_name=");
    form_encode(&mut u, title);
    u.push_str("&album_name=");
    form_encode(&mut u, &song.album);
    u.push_str(&format!("&duration={}", song.duration));
    u
}

/// LRCLIB's search for `song` by its cleaned `title`.
pub fn search_url(song: &Song, title: &str) -> String {
    let mut u = format!("{BASE}/search?track_name=");
    form_encode(&mut u, title);
    u.push_str("&artist_name=");
    form_encode(&mut u, &song.artist);
    u
}

/// The lyrics of one LRCLIB record: none for an instrumental. Word timing lives only in the record's
/// lyricsfile, so that wins when it times words; otherwise the LRC, synced over plain.
pub fn pick(o: &Value) -> Option<Lyrics> {
    if truthy(o, "instrumental") {
        return None;
    }
    let file = text(o, "lyricsfile").map(formats::from_lyricsfile).filter(|l| !l.lines.is_empty());
    if file.as_ref().is_some_and(|f| f.word_timed) {
        return file;
    }
    let lrc = text(o, "syncedLyrics").or_else(|| text(o, "plainLyrics")).map(lyrics::from_lrc).filter(|l| !l.lines.is_empty());
    match (lrc, file) {
        (Some(l), Some(f)) if formats::timing(&f) > formats::timing(&l) => Some(f),
        (Some(l), _) => Some(l),
        (None, f) => f,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn titles_lose_credits_and_version_notes() {
        for (title, clean_title) in [
            ("Song (feat. Someone)", "Song"),
            ("Song [Ft Someone] (Live at Wembley)", "Song"),
            ("Song (2011 Remaster)", "Song"),
            ("Song - Remastered 2011", "Song"),
            ("Song - 2009 Remaster", "Song"),
            ("Song - Single Version", "Song"),
            ("Song - Radio Edit (x)", "Song"),
            // Any closing bracket; `with` starts `without`.
            ("Song (Without You]", "Song"),
            ("Song (Acoustic)", "Song (Acoustic)"),
            ("Song (feat. unclosed", "Song (feat. unclosed"),
            // A dash needs space on both sides.
            ("Song -Live", "Song -Live"),
            // The first match takes the rest of the line.
            ("Song - Liverpool - Live", "Song"),
            ("Song - Live\nmore", "Song - Live\nmore"),
            ("Song\u{a0}(ſtereo)  ", "Song"),
            ("  Zażółć (Mono) ", "Zażółć"),
        ] {
            assert_eq!(clean(title), clean_title, "{title:?}");
        }
    }

    #[test]
    fn query_strings_are_form_encoded() {
        let song = Song { artist: "AC/DC & Co".into(), album: "Ünï".into(), duration: 200, ..Default::default() };
        assert_eq!(get_url(&song, "It's *a*-b_c.d"), "https://lrclib.net/api/get?artist_name=AC%2FDC+%26+Co&track_name=It%27s+*a*-b_c.d&album_name=%C3%9Cn%C3%AF&duration=200");
        assert_eq!(search_url(&song, "t"), "https://lrclib.net/api/search?track_name=t&artist_name=AC%2FDC+%26+Co");
    }

    #[test]
    fn pick_finest_record() {
        assert!(pick(&json!({"instrumental": true, "syncedLyrics": "[00:01.00]x"})).is_none());
        assert!(pick(&json!({"instrumental": "TRUE", "plainLyrics": "x"})).is_none());
        assert!(pick(&json!({"syncedLyrics": "[00:01.00]x", "plainLyrics": "y"})).unwrap().synced);
        assert!(!pick(&json!({"syncedLyrics": null, "plainLyrics": "y"})).unwrap().synced);
        assert!(pick(&json!({"syncedLyrics": "  ", "plainLyrics": null})).is_none());
        let file = "version: '1.0'\nmetadata: {title: t, artist: a}\nlines:\n  - {text: hi there, start_ms: 1000, words: [{text: 'hi ', start_ms: 1000, end_ms: 1400}, {text: there, start_ms: 1400, end_ms: 2000}]}\n";
        assert!(pick(&json!({"syncedLyrics": "[00:01.00]hi there", "lyricsfile": file})).unwrap().word_timed);
        // A line-timed lyricsfile is no better than the LRC made from it, but beats plain words.
        let lined = "version: '1.0'\nmetadata: {title: t, artist: a}\nlines:\n  - {text: from file, start_ms: 1000}\n";
        assert_eq!(pick(&json!({"syncedLyrics": "[00:01.00]from lrc", "lyricsfile": lined})).unwrap().lines[0].text, "from lrc");
        assert!(pick(&json!({"plainLyrics": "words", "lyricsfile": lined})).unwrap().synced);
    }
}
