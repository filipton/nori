//! Lyrics in one shape, whatever the server had: word cues (OpenSubsonic enhanced lyrics), inline
//! `<mm:ss.xx>` word tags (enhanced LRC), or plain line timing, in which case the words of a line get
//! estimated times so the player can sweep through them all the same. Text offsets are UTF-16, which
//! is what a Kotlin `String` indexes by.

use nori_model::model::{LyricLine, LyricWord, Lyrics};
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
    start: Option<i64>,
    value: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Cue {
    start: i64,
    end: i64,
    byte_start: usize,
    byte_end: usize,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct CueLine {
    index: usize,
    start: i64,
    end: i64,
    value: String,
    agent_id: Option<String>,
    cue: Vec<Cue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Agent {
    id: String,
    role: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Structured {
    synced: bool,
    kind: Option<String>,
    offset: i64,
    line: Vec<Line>,
    cue_line: Vec<CueLine>,
    agents: Vec<Agent>,
}

pub(crate) fn utf16_at(text: &str, byte: usize) -> u32 {
    let mut b = byte.min(text.len());
    while !text.is_char_boundary(b) {
        b -= 1;
    }
    text[..b].encode_utf16().count() as u32
}

/// `<01:02.50>` -> 62500
fn tag_ms(tag: &str) -> Option<i64> {
    let (m, rest) = tag.split_once(':')?;
    let secs: f64 = rest.parse().ok()?;
    Some(m.parse::<i64>().ok()? * 60_000 + (secs * 1000.0).round() as i64)
}

/// Splits enhanced-LRC word tags out of a line: returns the clean text and the timed words found.
fn inline_words(raw: &str) -> (String, Vec<(i64, usize)>) {
    let mut text = String::with_capacity(raw.len());
    let mut marks = Vec::new();
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else { break };
        match tag_ms(&rest[open + 1..open + close]) {
            Some(ms) => {
                text.push_str(&rest[..open]);
                marks.push((ms, text.len()));
            }
            None => text.push_str(&rest[..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    (text, marks)
}

/// No word timing from anywhere: spread the line's duration over its words by length, leaving the last
/// tenth as breath, which is how a sung line usually sits inside its slot.
fn estimate(text: &str, start: i64, end: i64) -> Vec<LyricWord> {
    let spans: Vec<(usize, usize)> = {
        let mut v = Vec::new();
        let mut at = None;
        for (i, c) in text.char_indices() {
            match (c.is_whitespace(), at) {
                (false, None) => at = Some(i),
                (true, Some(s)) => {
                    v.push((s, i));
                    at = None;
                }
                _ => {}
            }
        }
        if let Some(s) = at {
            v.push((s, text.len()));
        }
        v
    };
    let weight: usize = spans.iter().map(|(s, e)| text[*s..*e].chars().count() + 1).sum();
    if weight == 0 || end <= start {
        return Vec::new();
    }
    let usable = ((end - start) as f64 * 0.9).min(12_000.0);
    let mut t = start as f64;
    spans
        .iter()
        .map(|(s, e)| {
            let d = usable * (text[*s..*e].chars().count() + 1) as f64 / weight as f64;
            let w = LyricWord { start_ms: t.round() as i64, end_ms: (t + d).round() as i64, start: utf16_at(text, *s), end: utf16_at(text, *e) };
            t += d;
            w
        })
        .collect()
}

/// `[mm:ss.xx]` -> ms. Also accepts `[mm:ss]` and `[mm:ss:xx]`.
fn stamp(tag: &str) -> Option<i64> {
    let (m, rest) = tag.split_once(':')?;
    let rest = rest.replacen(':', ".", 1);
    let secs: f64 = rest.parse().ok()?;
    Some(m.trim().parse::<i64>().ok()? * 60_000 + (secs * 1000.0).round() as i64)
}

/// Plain LRC text (what LRCLIB, sidecar files and most providers return) in the same shape as the server's
/// structured lyrics, so the same line and word timing applies. Lines with several timestamps repeat;
/// `[offset:+n]` is honoured; `[ar:]`-style tags are skipped.
pub fn from_lrc(text: &str) -> Lyrics {
    let mut lines: Vec<Line> = Vec::new();
    let mut offset = 0i64;
    for raw in text.lines() {
        let mut rest = raw.trim_start_matches('\u{feff}').trim();
        let mut starts = Vec::new();
        while let Some(body) = rest.strip_prefix('[') {
            let Some(close) = body.find(']') else { break };
            let tag = &body[..close];
            if let Some(v) = tag.strip_prefix("offset:") {
                offset = v.trim().parse().unwrap_or(0);
            } else if let Some(ms) = stamp(tag) {
                starts.push(ms);
            }
            rest = body[close + 1..].trim_start();
        }
        for s in starts {
            lines.push(Line { start: Some(s), value: rest.to_string() });
        }
    }
    if lines.is_empty() {
        // Not LRC at all: plain text lyrics.
        let plain: Vec<Line> = text.lines().map(|l| Line { start: None, value: l.trim().to_string() }).collect();
        return build(vec![Structured { synced: false, line: plain, ..Default::default() }]);
    }
    lines.sort_by_key(|l| l.start);
    // LRC offset is positive = lyrics come sooner, the same convention as OpenSubsonic's field.
    build(vec![Structured { synced: true, offset, line: lines, ..Default::default() }])
}

pub fn build(mut all: Vec<Structured>) -> Lyrics {
    // The main layer: synced beats unsynced, and a translation is never the main text.
    all.sort_by_key(|l| (l.kind.as_deref() == Some("translation") || l.kind.as_deref() == Some("pronunciation"), !l.synced));
    let mut layers = all.into_iter();
    let Some(main) = layers.next().filter(|m| !m.line.is_empty()) else { return Lyrics::default() };
    let translation: Option<Structured> = layers.find(|l| l.kind.as_deref() == Some("translation") && l.synced == main.synced);
    let synced = main.synced;
    let offset = main.offset;
    let background: Vec<&str> = main.agents.iter().filter(|a| a.role == "bg").map(|a| a.id.as_str()).collect();
    let mut word_timed = false;

    let starts: Vec<i64> = main.line.iter().map(|l| l.start.unwrap_or(0) - offset).collect();
    let mut lines: Vec<LyricLine> = Vec::with_capacity(main.line.len());
    for (i, l) in main.line.iter().enumerate() {
        let start = if synced { starts[i] } else { -1 };
        let next = starts.get(i + 1).copied().filter(|n| *n > starts[i]).unwrap_or(starts[i] + 5_000);
        let cues = main.cue_line.iter().find(|c| c.index == i && !c.cue.is_empty());
        let (text, end, words, bg) = if let Some(c) = cues {
            word_timed = true;
            let words = c.cue.iter().map(|w| LyricWord { start_ms: w.start - offset, end_ms: w.end - offset, start: utf16_at(&c.value, w.byte_start), end: utf16_at(&c.value, w.byte_end + 1) }).collect();
            (c.value.clone(), if c.end > c.start { c.end - offset } else { next }, words, c.agent_id.as_deref().is_some_and(|a| background.contains(&a)))
        } else {
            let (text, marks) = inline_words(&l.value);
            if synced && !marks.is_empty() {
                word_timed = true;
                // A word runs to the next word's mark; the last one, unless a closing mark ends it, for
                // as long as a word that long is sung - not to the next line, which after a pause is
                // seconds away.
                let words = marks.iter().enumerate().filter_map(|(k, (ms, at))| {
                    let to = marks.get(k + 1).map(|m| m.1).unwrap_or(text.len());
                    let (start, end) = (utf16_at(&text, *at), utf16_at(&text, to));
                    let guessed = || (ms - offset + nori_look::lyrics::word_ms_estimate(end - start)).min(next.max(ms - offset));
                    let end_ms = marks.get(k + 1).map(|m| m.0 - offset).unwrap_or_else(guessed);
                    (to > *at).then(|| LyricWord { start_ms: ms - offset, end_ms, start, end })
                }).collect();
                (text, next, words, false)
            } else {
                let words = if synced { estimate(&text, start, next) } else { Vec::new() };
                (text, next, words, false)
            }
        };
        let translated = translation.as_ref().and_then(|t| t.line.iter().find(|x| synced && x.start == l.start).or_else(|| (!synced).then(|| t.line.get(i)).flatten())).map(|x| x.value.clone()).filter(|v| !v.trim().is_empty());
        lines.push(LyricLine { start_ms: start, end_ms: if synced { end } else { -1 }, text, words, translation: translated, background: bg, ..Default::default() });
    }
    Lyrics { synced, word_timed, lines, key: 0, offset_ms: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Lyrics {
        build(serde_json::from_str::<Vec<Structured>>(json).unwrap())
    }

    #[test]
    fn server_word_cues_win_and_offsets_are_utf16() {
        let l = parse(r#"[{"synced":true,"line":[{"start":1000,"value":"Żółć and I"}],
          "cueLine":[{"index":0,"start":1000,"end":4000,"value":"Żółć and I","cue":[
            {"start":1000,"end":1800,"value":"Żółć ","byteStart":0,"byteEnd":7},{"start":1800,"end":2400,"value":"and ","byteStart":9,"byteEnd":11},{"start":2400,"end":3200,"value":"I","byteStart":13,"byteEnd":13}]}]}]"#);
        assert!(l.word_timed);
        let w = &l.lines[0].words;
        assert_eq!((w[0].start, w[0].end), (0, 4), "four letters, eight bytes");
        assert_eq!((w[1].start, w[1].end, w[2].start, w[2].end), (5, 8, 9, 10));
        assert_eq!(l.lines[0].end_ms, 4000);
    }

    #[test]
    fn plain_lrc_gets_estimated_words_inside_the_line() {
        let l = parse(r#"[{"synced":true,"line":[{"start":0,"value":"one three"},{"start":4000,"value":"next"}]}]"#);
        assert!(!l.word_timed);
        let w = &l.lines[0].words;
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].start_ms, w[0].start, w[0].end), (0, 0, 3));
        assert!(w[1].end_ms <= 3600 && w[1].end_ms > w[1].start_ms && w[0].end_ms == w[1].start_ms);
        assert!(w[1].end_ms - w[1].start_ms > w[0].end_ms - w[0].start_ms, "the longer word takes longer");
    }

    #[test]
    fn inline_tags_offset_translation_and_unsynced() {
        let l = parse(r#"[{"kind":"translation","synced":true,"line":[{"start":1500,"value":"cześć"}]},
          {"synced":true,"offset":500,"line":[{"start":1500,"value":"<00:01.50>hel<00:01.90>lo <b>x"}]}]"#);
        assert_eq!(l.lines[0].text, "hello <b>x");
        assert_eq!(l.lines[0].start_ms, 1000);
        assert_eq!((l.lines[0].words[0].start_ms, l.lines[0].words[1].start_ms, l.lines[0].words[1].start), (1000, 1400, 3));
        assert_eq!(l.lines[0].translation.as_deref(), Some("cześć"));
        let plain = parse(r#"[{"synced":false,"line":[{"value":"just text"}]}]"#);
        assert_eq!((plain.synced, plain.lines[0].start_ms, plain.lines[0].words.len()), (false, -1, 0));
        assert!(parse("[]").lines.is_empty());
    }

    #[test]
    fn an_lrc_lines_last_word_before_a_long_pause_ends_by_itself() {
        // The last word has no closing mark and the next line is thirty seconds away.
        let l = from_lrc("[00:10.00]<00:10.00>Hold <00:10.40>on <00:10.80>tonight\n[00:40.00]<00:40.00>Again\n");
        let last = l.lines[0].words.last().unwrap().clone();
        assert_eq!(last.start_ms, 10_800);
        assert!(last.end_ms > 10_800 && last.end_ms <= 10_800 + 2_000, "sung for about as long as the word: {last:?}");
        assert_eq!(l.lines[0].words[1].end_ms, 10_800, "a word before it runs to the next mark");
        // A closing mark says when it ends; a next line sooner than the guess ends it there.
        let closed = from_lrc("[00:10.00]<00:10.00>Hold <00:10.40>on <00:10.80>tonight <00:14.00>\n[00:40.00]Again\n");
        assert_eq!(closed.lines[0].words.last().unwrap().end_ms, 14_000);
        let quick = from_lrc("[00:10.00]<00:10.00>Hold <00:10.40>on <00:10.80>tonight\n[00:11.00]Again\n");
        assert_eq!(quick.lines[0].words.last().unwrap().end_ms, 11_000);
    }

    #[test]
    fn lrc_text_with_repeats_offset_and_plain_fallback() {
        let l = from_lrc("[ar:Someone]\n[offset:+200]\n[00:01.00][00:10.50]chorus\n[00:05.25]verse\n\n");
        assert!(l.synced);
        let got: Vec<(i64, &str)> = l.lines.iter().map(|x| (x.start_ms, x.text.as_str())).collect();
        assert_eq!(got, [(800, "chorus"), (5050, "verse"), (10300, "chorus")]);
        assert!(!l.lines[0].words.is_empty(), "words are estimated for plain LRC too");
        let plain = from_lrc("just\nwords");
        assert!(!plain.synced);
        assert_eq!(plain.lines.len(), 2);
    }
}
