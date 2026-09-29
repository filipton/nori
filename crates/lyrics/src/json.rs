//! Lyrics that arrive as JSON: LyricsPlus, the answers of PaxSenix and BetterLyrics (whose Spotify and
//! Musixmatch routes pass those services' own shapes through), Musixmatch's rich sync and subtitles,
//! and the YouTube pages the YouTube-matched services need: a song search, the lyrics tab, the captions.
//! As everywhere in these files, words are timed only where the source times them; a line a source
//! timed as a whole stays a whole line.

use nori_model::Lyrics;
use serde_json::{Map, Value};

use crate::formats::{append, decode_html, finish, from_netease, from_qrc, from_ttml, keep_backing, plain, timing, voices, Timed, Timing};

/// A number written as a number or as a string of one; negative and non-finite ones are not times.
fn number(v: &Value) -> Option<f64> {
    let f = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    (f.is_finite() && f >= 0.0).then_some(f)
}

/// Milliseconds.
fn ms(v: &Value) -> Option<i64> {
    number(v).map(|f| f.round() as i64)
}

/// Seconds, fractional or not, as milliseconds.
fn secs(v: &Value) -> Option<i64> {
    number(v).map(|f| (f * 1000.0).round() as i64)
}

/// A text field as YouTube and others write one: a string, `{simpleText}`, or `{runs: [{text}]}`.
fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => {
            if let Some(s) = o.get("simpleText").and_then(Value::as_str) {
                return Some(s.to_string());
            }
            let runs = o.get("runs")?.as_array()?;
            Some(runs.iter().filter_map(|r| r.get("text").and_then(Value::as_str)).collect())
        }
        _ => None,
    }
}

/// Every object held under the key `name`, anywhere below `v`, in the order arrays list them.
fn objects<'a>(v: &'a Value, name: &str, out: &mut Vec<&'a Map<String, Value>>, depth: usize) {
    // serde_json already refuses deeper nesting than 128 when parsing; this keeps the walk honest too.
    if depth > 128 {
        return;
    }
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if k == name {
                    if let Value::Object(inner) = x {
                        out.push(inner);
                    }
                }
                objects(x, name, out, depth + 1);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| objects(x, name, out, depth + 1)),
        _ => {}
    }
}

fn all<'a>(v: &'a Value, name: &str) -> Vec<&'a Map<String, Value>> {
    let mut out = Vec::new();
    objects(v, name, &mut out, 0);
    out
}

fn parse(json: &str) -> Option<Value> {
    // Lyrics documents are kilobytes and a search page a megabyte or so; anything far beyond is not one.
    if json.len() > 16 << 20 {
        return None;
    }
    serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()
}

// ---- syllables --------------------------------------------------------------------------------------

/// A syllable or word as the JSON formats give one, before it is laid into its line.
struct Syl<'a> {
    text: &'a str,
    start: Option<i64>,
    end: Option<i64>,
    /// `part: true` runs on into the next syllable; `false` ends a word. Absent: the text's own spaces say.
    part: Option<bool>,
    backing: bool,
}

/// Lays syllables into a line, backing vocals into `backing`, each timed when `timed`. A word ends where a
/// syllable's text ends in a space; a source that writes no spaces but marks syllables that run on
/// (`part`) has one put in after every syllable that does not.
fn lay(line: &mut Timed, backing: &mut Timed, syls: &[Syl], timed: bool) {
    for (k, s) in syls.iter().enumerate() {
        let target = if s.backing { &mut *backing } else { &mut *line };
        let time = if timed { s.start.map(|st| (st, s.end.filter(|e| *e >= st))) } else { None };
        append(target, s.text, time);
        let next = syls[k + 1..].iter().find(|n| n.backing == s.backing);
        if s.part == Some(false) && !s.text.ends_with(char::is_whitespace) && next.is_some_and(|n| !n.text.starts_with(char::is_whitespace)) {
            append(target, " ", None);
        }
    }
}

// ---- LyricsPlus -------------------------------------------------------------------------------------

/// LyricsPlus (the backend of the YouLy+ extension), `/v2/lyrics/get`: `{type, lyrics: [{time, duration,
/// text, syllabus: [{time, duration, text, isBackground}]}]}`, in milliseconds. `type` says what the
/// timing is: "Line" times lines only, whatever the syllabus holds. A line whose only syllable has no
/// length is a line timed as a whole, not a one-word line. Backing vocals go at the end of their line.
pub fn from_lyricsplus(json: &str) -> Lyrics {
    parse(json).and_then(|v| lyricsplus(&v)).unwrap_or_default()
}

fn lyricsplus(v: &Value) -> Option<Lyrics> {
    let rows = v.get("lyrics").and_then(Value::as_array).or_else(|| v.get("data")?.get("lyrics")?.as_array())?;
    if !rows.iter().any(|r| r.get("time").is_some()) {
        return None;
    }
    let by_line = v.get("type").and_then(Value::as_str).is_some_and(|t| t.eq_ignore_ascii_case("line"));
    let mut lines = Vec::new();
    for r in rows {
        let text = r.get("text").and_then(Value::as_str).unwrap_or("");
        let Some(start) = r.get("time").and_then(ms) else { continue };
        let end = r.get("endTime").and_then(ms).or_else(|| r.get("duration").and_then(ms).map(|d| start + d));
        let mut line = Timed { start, end: end.filter(|e| *e > start), ..Default::default() };
        let mut backing = Timed::default();
        let list = r.get("syllabus").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let syls: Vec<Syl> = list
            .iter()
            .filter_map(|s| {
                let time = s.get("time").and_then(ms);
                Some(Syl {
                    text: s.get("text")?.as_str()?,
                    start: time,
                    end: time.zip(s.get("duration").and_then(ms)).map(|(t, d)| t + d),
                    part: s.get("part").and_then(Value::as_bool),
                    backing: s.get("isBackground").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();
        let whole = syls.len() == 1 && syls[0].end.zip(syls[0].start).is_none_or(|(e, s)| e <= s);
        if syls.is_empty() || ((by_line || whole) && !text.trim().is_empty()) {
            append(&mut line, text, None);
        } else {
            lay(&mut line, &mut backing, &syls, !by_line && !whole);
        }
        keep_backing(&mut line, backing);
        line.agent = r.get("element").and_then(|e| e.get("singer")).and_then(Value::as_str).map(str::to_string);
        lines.push(line);
    }
    // The singers LyricsPlus declares, `metadata.agents: {v1: {type: "person"}}`, for the duet sides.
    let kinds: std::collections::HashMap<String, String> = v
        .get("metadata")
        .and_then(|m| m.get("agents"))
        .and_then(Value::as_object)
        .map(|a| a.iter().map(|(id, o)| (id.clone(), o.get("type").and_then(Value::as_str).unwrap_or("person").to_string())).collect())
        .unwrap_or_default();
    voices(&mut lines, &kinds);
    Some(finish(lines))
}

// ---- PaxSenix's Apple Music JSON --------------------------------------------------------------------

/// PaxSenix's own JSON for Apple Music lyrics: `{type, content: [{timestamp, endtime, text: [{text, part,
/// timestamp, endtime}], backgroundText: [...]}]}`, in milliseconds. "Syllable" timing gives words, "Line"
/// does not; a document whose lines all start at nought is not timed at all.
fn apple_json(v: &Value) -> Option<Lyrics> {
    let rows = v.get("content")?.as_array()?;
    if !rows.iter().any(|r| r.get("timestamp").is_some() && r.get("text").is_some_and(Value::is_array)) {
        return None;
    }
    let by_line = v.get("type").and_then(Value::as_str).is_some_and(|t| t.eq_ignore_ascii_case("line"));
    fn syls(list: Option<&Value>, backing: bool) -> Vec<Syl<'_>> {
        list.and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some(Syl {
                            text: s.get("text")?.as_str()?,
                            start: s.get("timestamp").and_then(ms),
                            end: s.get("endtime").and_then(ms),
                            part: s.get("part").and_then(Value::as_bool),
                            backing,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    let untimed = rows.iter().all(|r| r.get("timestamp").and_then(ms).unwrap_or(0) == 0 && r.get("endtime").and_then(ms).unwrap_or(0) == 0);
    let mut lines = Vec::new();
    let mut text_only = Vec::new();
    for r in rows {
        let mut all = syls(r.get("text"), false);
        all.extend(syls(r.get("backgroundText"), true));
        let (mut line, mut backing) = (Timed::default(), Timed::default());
        let start = r.get("timestamp").and_then(ms).unwrap_or(0);
        let end = r.get("endtime").and_then(ms);
        // One piece spanning the whole line is the line's own time, not a word's.
        let whole = all.len() == 1 && all[0].start == Some(start) && (all[0].end == end || end.is_none());
        lay(&mut line, &mut backing, &all, !by_line && !untimed && !whole);
        keep_backing(&mut line, backing);
        // PaxSenix says the side itself: a line sung from the other side of a duet is its "opposite turn".
        line.voice = u8::from(r.get("oppositeTurn").and_then(Value::as_bool).unwrap_or(false));
        if untimed {
            text_only.push(line.text);
        } else {
            line.start = start;
            line.end = end.filter(|e| *e > start);
            lines.push(line);
        }
    }
    Some(if untimed { plain(&text_only.join("\n")) } else { finish(lines) })
}

// ---- Spotify, and PaxSenix's line format -------------------------------------------------------------

/// Spotify's lyrics as its web player receives them (and as PaxSenix passes them on): `{lyrics: {syncType,
/// lines: [{startTimeMs, words, endTimeMs}]}}`, the times as strings. Timed by line; a "♪" line is the
/// gap between two, and ends the line before it.
fn spotify(v: &Value) -> Option<Lyrics> {
    let o = v.get("lyrics").filter(|l| l.is_object()).unwrap_or(v);
    let rows = o.get("lines")?.as_array()?;
    if !rows.iter().any(|r| r.get("startTimeMs").is_some()) {
        return None;
    }
    let words = |r: &Value| r.get("words").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if o.get("syncType").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case("unsynced")) {
        return Some(plain(&rows.iter().map(words).collect::<Vec<_>>().join("\n")));
    }
    let mut lines: Vec<Timed> = Vec::new();
    for r in rows {
        let Some(start) = r.get("startTimeMs").and_then(ms) else { continue };
        let text = words(r);
        if text.is_empty() || text.chars().all(|c| c == '♪' || c.is_whitespace()) {
            if let Some(prev) = lines.last_mut().filter(|p| p.end.is_none() && start > p.start) {
                prev.end = Some(start);
            }
            continue;
        }
        let mut line = Timed { start, end: r.get("endTimeMs").and_then(ms).filter(|e| *e > start), ..Default::default() };
        append(&mut line, &text, None);
        lines.push(line);
    }
    Some(finish(lines))
}

/// PaxSenix's own line format for Musixmatch: `{syncType, lines: [{timeTag: "00:12.34", words}]}`.
fn time_tags(v: &Value) -> Option<Lyrics> {
    let rows = v.get("lines")?.as_array()?;
    if !rows.iter().any(|r| r.get("timeTag").is_some()) {
        return None;
    }
    let words = |r: &Value| r.get("words").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if v.get("syncType").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case("unsynced")) {
        return Some(plain(&rows.iter().map(words).collect::<Vec<_>>().join("\n")));
    }
    let lrc: String = rows
        .iter()
        .filter_map(|r| Some(format!("[{}]{}\n", r.get("timeTag")?.as_str()?.trim(), words(r))))
        .collect();
    Some(crate::lyrics::from_lrc(&lrc))
}

// ---- Musixmatch -------------------------------------------------------------------------------------

/// Musixmatch's rich sync body: `[{ts, te, l: [{c, o}], x}]`, a line from `ts` to `te` seconds and each
/// of its pieces `o` seconds into it. Spaces are pieces of their own; a word runs until the next piece.
fn richsync(v: &Value) -> Option<Lyrics> {
    let rows = v.as_array()?;
    if !rows.iter().any(|r| r.get("ts").is_some() && r.get("l").is_some_and(Value::is_array)) {
        return None;
    }
    let mut lines = Vec::new();
    for r in rows {
        let Some(start) = r.get("ts").and_then(secs) else { continue };
        let end = r.get("te").and_then(secs).filter(|e| *e > start);
        let mut line = Timed { start, end, ..Default::default() };
        let pieces: Vec<(&str, Option<i64>)> = r
            .get("l")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|p| Some((p.get("c")?.as_str()?, p.get("o").and_then(secs).map(|o| start + o)))).collect())
            .unwrap_or_default();
        if pieces.is_empty() {
            append(&mut line, r.get("x").and_then(Value::as_str).unwrap_or(""), None);
        }
        for (k, (c, at)) in pieces.iter().enumerate() {
            let until = pieces[k + 1..].iter().find_map(|(_, t)| *t).or(end);
            append(&mut line, c, at.map(|a| (a, until.filter(|u| *u >= a))));
        }
        lines.push(line);
    }
    Some(finish(lines))
}

/// Musixmatch's subtitle body in its own `mxm` format: `[{text, time: {total}}]`, a line from `total`
/// seconds. An empty line is a gap and ends the one before it.
fn subtitle(v: &Value) -> Option<Lyrics> {
    let rows = v.as_array()?;
    if !rows.iter().any(|r| r.get("time").and_then(|t| t.get("total")).is_some()) {
        return None;
    }
    let mut lines: Vec<Timed> = Vec::new();
    for r in rows {
        let Some(start) = r.get("time").and_then(|t| t.get("total")).and_then(secs) else { continue };
        let text = r.get("text").and_then(Value::as_str).unwrap_or("").trim();
        if text.is_empty() || text.chars().all(|c| c == '♪' || c.is_whitespace()) {
            if let Some(prev) = lines.last_mut().filter(|p| p.end.is_none() && start > p.start) {
                prev.end = Some(start);
            }
            continue;
        }
        let mut line = Timed { start, ..Default::default() };
        append(&mut line, text, None);
        lines.push(line);
    }
    Some(finish(lines))
}

/// Musixmatch's plain lyrics without the notice it closes them with ("******* This Lyrics is NOT for
/// Commercial use *******") and the number after it.
fn without_notice(text: &str) -> String {
    let kept: Vec<&str> = text.lines().take_while(|l| !l.trim_start().starts_with("*******")).collect();
    kept.join("\n").trim_end().to_string()
}

// ---- any provider -----------------------------------------------------------------------------------

/// Text that is not JSON: Apple-style TTML (escaped or not), QQ's QRC (bare or in its XML), NetEase-style
/// karaoke lines, or LRC in any of its forms, plain text included.
fn from_text(raw: &str, title: &str) -> Lyrics {
    let unescaped;
    let mut t = raw.trim();
    if t.starts_with("&lt;") {
        unescaped = decode_html(t);
        t = unescaped.trim();
    }
    if t.starts_with('<') {
        return if t.contains("LyricContent=") { from_qrc(t, title) } else { from_ttml(t) };
    }
    // `[start,length]` lines: QRC puts each word's `(start,length)` after it, NetEase's YRC puts
    // `(start,length,0)` before it.
    let karaoke = t.lines().map(str::trim).find(|l| {
        l.strip_prefix('[').and_then(|r| r.split_once(']')).is_some_and(|(head, _)| {
            let nums: Vec<&str> = head.split(',').collect();
            nums.len() == 2 && nums.iter().all(|n| !n.trim().is_empty() && n.trim().bytes().all(|b| b.is_ascii_digit()))
        })
    });
    if let Some(line) = karaoke {
        let body = line.split_once(']').map_or("", |(_, b)| b).trim_start();
        let yrc = body.starts_with('(') && body[1..].split(')').next().is_some_and(|tag| tag.split(',').count() == 3);
        return if yrc { from_netease(t, "", title) } else { from_qrc(t, title) };
    }
    crate::lyrics::from_lrc(&without_notice(t))
}

/// An answer that says it is a refusal or an error rather than lyrics.
fn refused(v: &Value) -> bool {
    let flag = |k: &str| v.get(k).and_then(Value::as_bool);
    flag("isError") == Some(true)
        || flag("ok") == Some(false)
        || flag("success") == Some(false)
        || match v.get("error") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Object(_)) => true,
            _ => false,
        }
}

/// Fields that hold lyrics (as text, or as more JSON), in the order they are worth reading.
const FIELDS: &[&str] = &[
    "ttml", "ttmlContent", "elrc", "richsync", "richsync_body", "richSyncLyrics", "lrc", "syncedLyrics", "subtitle", "subtitle_body",
    "lyrics", "lyric", "content", "text", "plainLyrics", "plain", "lyrics_body", "data", "result", "response", "message", "body",
];

fn from_value(v: &Value, title: &str, depth: usize) -> Option<Lyrics> {
    if depth > 5 || refused(v) {
        return None;
    }
    let mut best: Option<Lyrics> = None;
    let mut offer = |l: Lyrics| {
        if timing(&l) > best.as_ref().map_or(Timing::Empty, timing) {
            best = Some(l);
        }
    };
    // Shapes known by their structure first.
    for found in [lyricsplus(v), apple_json(v), spotify(v), time_tags(v), richsync(v), subtitle(v)].into_iter().flatten() {
        offer(found);
    }
    match v {
        Value::Object(o) => {
            for key in FIELDS {
                match o.get(*key) {
                    Some(Value::String(s)) if !s.trim().is_empty() => match serde_json::from_str::<Value>(s.trim()) {
                        Ok(inner @ (Value::Object(_) | Value::Array(_))) => {
                            if let Some(l) = from_value(&inner, title, depth + 1) {
                                offer(l);
                            }
                        }
                        _ => offer(from_text(s, title)),
                    },
                    Some(x @ (Value::Object(_) | Value::Array(_))) => {
                        if let Some(l) = from_value(x, title, depth + 1) {
                            offer(l);
                        }
                    }
                    _ => {}
                }
            }
        }
        // A list of answers (several matches): the first is the one the service ranked best.
        Value::Array(a) => {
            if let Some(l) = a.first().and_then(|x| from_value(x, title, depth + 1)) {
                offer(l);
            }
        }
        Value::String(s) => offer(from_text(s, title)),
        _ => {}
    }
    best
}

/// Whatever a lyrics service answered, read the best way it can be: the services that relay other
/// catalogues (PaxSenix, BetterLyrics) pass their shapes through, sometimes wrapped in one or two JSON
/// envelopes, and do not say which. Known shapes are read by their structure (LyricsPlus, PaxSenix's
/// Apple JSON, Spotify's, Musixmatch's); otherwise the fields that carry lyrics are tried, TTML and
/// QRC and LRC recognised by their text. Of everything found, the best timed wins.
pub fn from_provider(body: &str, title: &str) -> Lyrics {
    let t = body.trim_start_matches('\u{feff}').trim();
    if t.is_empty() || t.len() > 8 << 20 {
        return Lyrics::default();
    }
    match serde_json::from_str::<Value>(t) {
        Ok(v) => from_value(&v, title, 0).unwrap_or_default(),
        Err(_) => from_text(t, title),
    }
}

// ---- search results ---------------------------------------------------------------------------------

/// A song as a search answered it, for the caller to match on name, artist and length.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// 0 when the service did not say.
    pub duration_ms: i64,
}

fn first_str(o: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match o.get(*k)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// A name, or a list of names, or objects with one.
fn names(v: &Value) -> Option<String> {
    let name = |x: &Value| match x {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => first_str(o, &["name", "artistName", "title"]),
        _ => None,
    };
    let joined = match v {
        Value::Array(a) => a.iter().filter_map(name).collect::<Vec<_>>().join(", "),
        x => name(x)?,
    };
    (!joined.is_empty()).then_some(joined)
}

fn collect_tracks(v: &Value, out: &mut Vec<FoundTrack>, depth: usize) {
    if depth > 32 || out.len() >= 50 {
        return;
    }
    match v {
        Value::Object(o) => {
            let details = o.get("attributes").and_then(Value::as_object).unwrap_or(o);
            let id = first_str(o, &["id", "trackId", "track_id"]).or_else(|| first_str(details, &["id", "trackId", "track_id"]));
            let title = first_str(details, &["name", "title", "trackName", "track_name"]);
            if let (Some(id), Some(title)) = (id, title) {
                let artist = first_str(details, &["artistName", "artist_name"])
                    .or_else(|| details.get("artists").and_then(names))
                    .or_else(|| details.get("artist").and_then(names))
                    .unwrap_or_default();
                let key = ["durationInMillis", "durationMs", "duration_ms", "duration"].iter().find_map(|k| details.get(*k).and_then(ms)).unwrap_or(0);
                // Seconds when small: nobody's song is ten seconds long in milliseconds.
                let duration_ms = if key > 0 && key < 10_000 { key * 1000 } else { key };
                out.push(FoundTrack { id, title, artist, duration_ms });
                // A track's own album and artists are not more tracks.
                return;
            }
            o.values().for_each(|x| collect_tracks(x, out, depth + 1));
        }
        Value::Array(a) => a.iter().for_each(|x| collect_tracks(x, out, depth + 1)),
        _ => {}
    }
}

/// The tracks in a search answer whose shape is not fixed (PaxSenix's searches pass on the catalogue's
/// own): any object with an id and a name, its artist and length where it has them, Apple's
/// `attributes` included. A track's album and artists inside it are not listed as tracks.
pub fn found_tracks(json: &str) -> Vec<FoundTrack> {
    let mut out = Vec::new();
    if let Some(v) = parse(json) {
        collect_tracks(&v, &mut out, 0);
    }
    out
}

// ---- YouTube ----------------------------------------------------------------------------------------

/// `3:45` or `1:02:03` as milliseconds.
fn clock_ms(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some(parts.iter().try_fold(0i64, |acc, p| p.parse::<i64>().ok().map(|n| acc * 60 + n))? * 1000)
}

/// Words YouTube Music puts in a result's second line that are not an artist.
const ROW_TYPES: &[&str] = &["song", "video", "single", "ep", "album", "episode", "podcast"];

/// The songs in a YouTube Music search answer (the `search` endpoint with the songs filter), in its
/// order: each row's video id, title, artists (the runs that link to an artist, or the first part of
/// the second line) and length (the part of the second line that reads as one).
pub fn youtube_songs(json: &str) -> Vec<FoundTrack> {
    let Some(v) = parse(json) else { return Vec::new() };
    let mut out: Vec<FoundTrack> = Vec::new();
    for r in all(&v, "musicResponsiveListItemRenderer") {
        let r = Value::Object(r.clone());
        let id = r
            .pointer("/playlistItemData/videoId")
            .or_else(|| r.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId"))
            .and_then(Value::as_str);
        let Some(id) = id.filter(|i| !i.is_empty()) else { continue };
        let column = |i: usize| r.pointer(&format!("/flexColumns/{i}/musicResponsiveListItemFlexColumnRenderer/text"));
        let Some(title) = column(0).and_then(text_of).filter(|t| !t.trim().is_empty()) else { continue };
        let second = column(1);
        let credited: Vec<&str> = second
            .and_then(|c| c.get("runs"))
            .and_then(Value::as_array)
            .map(|runs| {
                runs.iter()
                    .filter(|run| {
                        run.pointer("/navigationEndpoint/browseEndpoint/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType")
                            .and_then(Value::as_str)
                            == Some("MUSIC_PAGE_TYPE_ARTIST")
                    })
                    .filter_map(|run| run.get("text").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let line = second.and_then(text_of).unwrap_or_default();
        let parts: Vec<&str> = line.split(" • ").map(str::trim).filter(|p| !p.is_empty()).collect();
        let fixed = r.pointer("/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text").and_then(text_of);
        let duration_ms = parts.iter().rev().find_map(|p| clock_ms(p)).or_else(|| fixed.as_deref().and_then(clock_ms)).unwrap_or(0);
        let artist = if credited.is_empty() {
            parts
                .iter()
                .find(|p| clock_ms(p).is_none() && !ROW_TYPES.contains(&p.to_lowercase().as_str()) && !p.to_lowercase().ends_with("plays"))
                .map(|p| p.to_string())
                .unwrap_or_default()
        } else {
            credited.join(", ")
        };
        if out.iter().all(|t| t.id != id) {
            out.push(FoundTrack { id: id.to_string(), title: title.trim().to_string(), artist, duration_ms });
        }
    }
    out
}

/// Where a YouTube Music song's lyrics tab points: the page to `browse` for them.
#[derive(Debug, Clone, PartialEq)]
pub struct YoutubePage {
    pub browse_id: String,
    pub params: Option<String>,
}

/// The lyrics tab of a `next` answer (what YouTube Music's player asks when a song starts): the tab
/// titled "Lyrics", or failing that a page whose id is a lyrics page's (`MPLYt…`). None when the song
/// has none, which YouTube shows as a tab that cannot be opened.
pub fn youtube_lyrics_page(json: &str) -> Option<YoutubePage> {
    let v = parse(json)?;
    let page = |e: &Map<String, Value>| {
        Some(YoutubePage { browse_id: e.get("browseId")?.as_str()?.to_string(), params: e.get("params").and_then(Value::as_str).map(str::to_string) })
    };
    let titled = all(&v, "tabRenderer").into_iter().find_map(|tab| {
        let title = tab.get("title").and_then(text_of)?;
        if !title.trim().eq_ignore_ascii_case("lyrics") {
            return None;
        }
        tab.get("endpoint")?.get("browseEndpoint")?.as_object().and_then(page)
    });
    titled.or_else(|| all(&v, "browseEndpoint").into_iter().filter_map(page).find(|p| p.browse_id.starts_with("MPLYt")))
}

/// The lyrics a YouTube Music lyrics page shows: the text of its description shelf, not timed.
pub fn from_youtube_music(json: &str) -> Lyrics {
    let Some(v) = parse(json) else { return Lyrics::default() };
    all(&v, "musicDescriptionShelfRenderer")
        .into_iter()
        .find_map(|shelf| shelf.get("description").and_then(text_of).filter(|t| !t.trim().is_empty()))
        .map(|t| plain(t.trim()))
        .unwrap_or_default()
}

/// A caption's words without the marks captions put round music: "♪", "[Music]", "(Applause)".
fn caption(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut bracket = String::new();
    for c in text.chars() {
        match c {
            '[' | '(' if depth == 0 => {
                depth = 1;
                bracket.clear();
                bracket.push(c);
            }
            ']' | ')' if depth == 1 => {
                depth = 0;
                bracket.push(c);
                let inner = bracket[1..bracket.len() - 1].trim().to_lowercase();
                // Only a sound in brackets goes; sung words in brackets ("(ooh)") stay.
                if !matches!(inner.as_str(), "music" | "applause" | "laughter" | "instrumental" | "silence" | "cheering" | "no audio") {
                    out.push_str(&bracket);
                }
            }
            _ if depth == 1 => bracket.push(c),
            '♪' | '♫' | '\u{266c}' => {}
            c if c.is_whitespace() => out.push(' '),
            c => out.push(c),
        }
    }
    if depth == 1 {
        out.push_str(&bracket);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A YouTube video's captions as lyrics, timed line by line: the transcript `get_transcript` answers
/// (`transcriptCueRenderer` with `startOffsetMs` and `durationMs`, or the newer `transcriptSegmentRenderer`
/// with `startMs` and `endMs`), or timed text in YouTube's `json3` (`events` with `tStartMs`,
/// `dDurationMs` and `segs`). Captions that time each word of speech are still read a line at a time:
/// they time the speech recogniser, not the singing.
pub fn from_youtube_captions(json: &str) -> Lyrics {
    let Some(v) = parse(json) else { return Lyrics::default() };
    let mut cues: Vec<(i64, Option<i64>, String)> = Vec::new();
    for c in all(&v, "transcriptCueRenderer") {
        let (Some(start), Some(text)) = (c.get("startOffsetMs").and_then(ms), c.get("cue").and_then(text_of)) else { continue };
        cues.push((start, c.get("durationMs").and_then(ms).map(|d| start + d), text));
    }
    for c in all(&v, "transcriptSegmentRenderer") {
        let (Some(start), Some(text)) = (c.get("startMs").and_then(ms), c.get("snippet").and_then(text_of)) else { continue };
        cues.push((start, c.get("endMs").and_then(ms), text));
    }
    if let Some(events) = v.get("events").and_then(Value::as_array) {
        for e in events {
            let Some(start) = e.get("tStartMs").and_then(ms) else { continue };
            let Some(segs) = e.get("segs").and_then(Value::as_array) else { continue };
            let text: String = segs.iter().filter_map(|s| s.get("utf8").and_then(Value::as_str)).collect();
            cues.push((start, e.get("dDurationMs").and_then(ms).map(|d| start + d), text));
        }
    }
    let lines = cues
        .into_iter()
        .filter_map(|(start, end, text)| {
            let words = caption(&text);
            (!words.is_empty()).then(|| {
                let mut line = Timed { start, end: end.filter(|e| *e > start), ..Default::default() };
                append(&mut line, &words, None);
                line
            })
        })
        .collect();
    finish(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(l: &Lyrics) -> Vec<&str> {
        l.lines.iter().map(|x| x.text.as_str()).collect()
    }

    fn words(l: &Lyrics, i: usize) -> Vec<(i64, i64, u32, u32)> {
        l.lines[i].words.iter().map(|w| (w.start_ms, w.end_ms, w.start, w.end)).collect()
    }

    /// LyricsPlus' v2 answer for a song Apple times by syllable: syllables carrying their own trailing
    /// space, a backing vocal, and one line that is only timed as a whole.
    const LYRICSPLUS: &str = include_str!("../testdata/lyricsplus.json");

    #[test]
    fn lyricsplus_syllables_backing_and_whole_lines() {
        let l = from_lyricsplus(LYRICSPLUS);
        assert!(l.synced && l.word_timed);
        assert_eq!(texts(&l), ["Paper boats drift", "紙の舟", "Whole line"]);
        assert_eq!(words(&l, 0), [(12_345, 12_900, 0, 2), (12_900, 13_200, 2, 5), (13_200, 13_600, 6, 11), (13_600, 14_100, 12, 17)]);
        // The backing vocal is kept under its line, timed on its own.
        assert_eq!(l.lines[0].backing, "(hey)");
        assert_eq!(l.lines[0].backing_words.iter().map(|w| (w.start_ms, w.end_ms, w.start, w.end)).collect::<Vec<_>>(), [(14_200, 14_800, 0, 5)]);
        // One singer declared: no duet.
        assert!(l.lines.iter().all(|x| x.voice == 0));
        assert_eq!((l.lines[0].start_ms, l.lines[0].end_ms), (12_345, 15_678));
        assert_eq!(words(&l, 1).iter().map(|w| w.1 - w.0).collect::<Vec<_>>(), [500, 0, 1000], "a zero-length syllable stays zero");
        assert!(l.lines[2].words.is_empty(), "one syllable with no length is the line's own time");
        // "Line" timing says the syllabus is not word timing, whatever it holds.
        let lined = from_lyricsplus(&LYRICSPLUS.replace(r#""type":"Word""#, r#""type":"Line""#));
        assert!(lined.synced && !lined.word_timed);
        assert_eq!(lined.lines[0].text, "Paper boats drift");
        // Syllables without spaces but with `part`: a word ends where `part` is false.
        let parted = from_lyricsplus(r#"{"type":"Syllable","lyrics":[{"time":0,"duration":900,"text":"","syllabus":[{"time":0,"duration":300,"text":"Ri","part":true},{"time":300,"duration":300,"text":"ver","part":false},{"time":600,"duration":300,"text":"song","part":false}]}]}"#);
        assert_eq!(parted.lines[0].text, "River song");
        assert!(from_lyricsplus(r#"{"lyrics":[]}"#).lines.is_empty());
        assert!(from_lyricsplus("not json").lines.is_empty());
    }

    /// PaxSenix's JSON for an Apple Music song: syllables with `part`, a backing line, and fields the
    /// generic reader must not prefer over the syllables (its LRC).
    const PAXSENIX_APPLE: &str = include_str!("../testdata/paxsenix-apple.json");

    #[test]
    fn paxsenix_apple_json_times_syllables() {
        let l = from_provider(PAXSENIX_APPLE, "Song");
        assert!(l.word_timed, "the syllables beat the LRC beside them");
        assert_eq!(texts(&l), ["Quiet night", "Again"]);
        assert_eq!(words(&l, 0), [(12_340, 12_800, 0, 3), (12_800, 13_100, 3, 5), (13_100, 14_000, 6, 11)]);
        assert_eq!(l.lines[0].backing, "la");
        assert_eq!(l.lines[0].backing_words.iter().map(|w| (w.start_ms, w.end_ms, w.start, w.end)).collect::<Vec<_>>(), [(14_100, 14_900, 0, 2)]);
        // PaxSenix says the side itself.
        assert_eq!(l.lines.iter().map(|x| x.voice).collect::<Vec<_>>(), [0, 0]);
        let turned = from_provider(&PAXSENIX_APPLE.replace(r#""oppositeTurn":false"#, r#""oppositeTurn":true"#), "Song");
        assert_eq!(turned.lines.iter().map(|x| x.voice).collect::<Vec<_>>(), [1, 0]);
        assert!(l.lines[1].words.is_empty(), "one piece covering the whole line is the line's time");
        let lined = from_provider(&PAXSENIX_APPLE.replace(r#""type":"Syllable""#, r#""type":"Line""#), "Song");
        assert!(lined.synced && !lined.word_timed);
        // Unsynced: every line at nought.
        let flat = from_provider(r#"{"type":"None","content":[{"timestamp":0,"endtime":0,"text":[{"text":"just words","timestamp":0,"endtime":0}]}]}"#, "");
        assert!(!flat.synced && flat.lines[0].text == "just words");
    }

    #[test]
    fn provider_envelopes_and_text_shapes() {
        // BetterLyrics: `{ttml, score}`.
        let ttml = r#"{"ttml":"<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div><p begin=\"1.0\" end=\"2.0\"><span begin=\"1.0\" end=\"1.5\">Hi</span> <span begin=\"1.5\" end=\"2.0\">there</span></p></div></body></tt>","score":0.93}"#;
        let l = from_provider(ttml, "");
        assert!(l.word_timed && l.lines[0].text == "Hi there");
        // Portato: `{lyrics, provider: "qq"}` with QRC text inside.
        let qq = r#"{"lyrics":"[ti:Song]\n[1000,2000]Hi (1000,500)there(1500,1500)","provider":"qq"}"#;
        let q = from_provider(qq, "Song");
        assert!(q.word_timed);
        assert_eq!(words(&q, 0), [(1000, 1500, 0, 2), (1500, 3000, 3, 8)]);
        // Wrapped twice, as a string holding JSON; and escaped TTML.
        let nested = r#"{"data":"{\"lrc\":\"[00:01.00]one\\n[00:02.00]two\"}"}"#;
        assert_eq!(texts(&from_provider(nested, "")), ["one", "two"]);
        let escaped = r#"{"lyrics":"&lt;tt&gt;&lt;body&gt;&lt;p begin=\"1\" end=\"2\"&gt;Hello&lt;/p&gt;&lt;/body&gt;&lt;/tt&gt;"}"#;
        let e = from_provider(escaped, "");
        assert!(e.synced && e.lines[0].text == "Hello");
        // LRC beats the plain text beside it; an error is nothing.
        let both = r#"{"plainLyrics":"one\ntwo","syncedLyrics":"[00:01.00]one\n[00:02.00]two"}"#;
        assert!(from_provider(both, "").synced);
        assert!(from_provider(r#"{"error":"No lyrics found","lyrics":"[00:01.00]x"}"#, "").lines.is_empty());
        assert!(from_provider(r#"{"isError":true}"#, "").lines.is_empty());
        // Not JSON at all: raw LRC, raw TTML, raw YRC.
        assert!(from_provider("[00:01.00]raw", "").synced);
        assert!(from_provider("<tt><body><p begin=\"1\" end=\"2\">x</p></body></tt>", "").synced);
        assert!(from_provider("[1000,1000](1000,500,0)Hi (1500,500,0)there", "").word_timed);
        assert!(from_provider("", "").lines.is_empty());
    }

    /// Spotify's lyrics JSON as PaxSenix passes it on: times as strings, a "♪" gap.
    const SPOTIFY: &str = include_str!("../testdata/spotify.json");

    #[test]
    fn spotify_lines_and_gaps() {
        let l = from_provider(SPOTIFY, "");
        assert!(l.synced && !l.word_timed);
        assert_eq!(texts(&l), ["Paper boats on a quiet river", "La la la line two", "Back again"]);
        assert_eq!(l.lines[1].end_ms, 9000, "the gap ends the line before it");
        assert!(l.lines.iter().all(|x| x.words.is_empty()));
        let unsynced = from_provider(&SPOTIFY.replace("LINE_SYNCED", "UNSYNCED"), "");
        assert!(!unsynced.synced);
        // PaxSenix's own line format.
        let tags = from_provider(r#"{"error":false,"syncType":"LINE_SYNCED","lines":[{"timeTag":"00:00.96","words":"One"},{"timeTag":"00:04.02","words":"Two"}]}"#, "");
        assert!(tags.synced && tags.lines[1].start_ms == 4020);
    }

    /// Musixmatch's rich sync, as PaxSenix's Musixmatch route may pass it on: seconds, pieces offset from
    /// their line, spaces as pieces.
    const RICHSYNC: &str = include_str!("../testdata/musixmatch-richsync.json");
    const SUBTITLE: &str = include_str!("../testdata/musixmatch-subtitle.json");

    #[test]
    fn musixmatch_richsync_subtitle_and_plain_shapes() {
        let l = from_provider(RICHSYNC, "");
        assert!(l.word_timed);
        assert_eq!(texts(&l), ["We fold maps", "Żółć"]);
        assert_eq!(words(&l, 0), [(27_390, 27_540, 0, 2), (27_540, 27_740, 3, 7), (27_740, 29_500, 8, 12)]);
        assert_eq!((l.lines[0].start_ms, l.lines[0].end_ms), (27_390, 29_500));
        assert_eq!(words(&l, 1), [(30_000, 31_000, 0, 4)], "UTF-16 offsets");
        let sub = from_provider(SUBTITLE, "");
        assert!(sub.synced && !sub.word_timed);
        assert_eq!(sub.lines[0].end_ms, 29_500, "an empty line is a gap");
        let text = from_provider(r#"{"lyrics_body":"First\nSecond\n\n******* This Lyrics is NOT for Commercial use *******\n(1409623253212)"}"#, "");
        assert!(!text.synced);
        assert_eq!(texts(&text), ["First", "Second"], "the closing notice and the gap before it go");
        // The same rich sync inside Musixmatch's envelope, as a string, through the generic reader.
        let envelope = format!(r#"{{"message":{{"header":{{"status_code":200}},"body":{{"richsync":{{"richsync_body":{}}}}}}}}}"#, serde_json::to_string(RICHSYNC).unwrap());
        assert!(from_provider(&envelope, "").word_timed);
    }

    #[test]
    fn tracks_from_any_search_shape() {
        let apple = r#"{"results":{"songs":{"data":[{"id":"1000000001","type":"songs","attributes":{"name":"Glass Harbour","artistName":"The Lanterns","durationInMillis":238640,"albumName":"Low Tide"}}]}}}"#;
        assert_eq!(found_tracks(apple), [FoundTrack { id: "1000000001".into(), title: "Glass Harbour".into(), artist: "The Lanterns".into(), duration_ms: 238_640 }]);
        let spotify = r#"{"ok":true,"tracks":[{"id":"0aBcDeFgHiJkLmNoPqRsTu","name":"Glass Harbour","artists":[{"id":"4Z8W","name":"The Lanterns"}],"album":{"id":"6AZv","name":"Low Tide"},"duration":238}]}"#;
        let t = found_tracks(spotify);
        assert_eq!(t.len(), 1, "the album and artists inside a track are not tracks");
        assert_eq!((t[0].artist.as_str(), t[0].duration_ms), ("The Lanterns", 238_000));
        assert!(found_tracks("[]").is_empty() && found_tracks("nope").is_empty());
    }

    /// A trimmed YouTube Music search answer with the songs filter: two rows, the first with its artist
    /// as a linked run, the second with a plain second line.
    const YT_SEARCH: &str = include_str!("../testdata/youtube-search.json");

    #[test]
    fn youtube_search_rows() {
        let songs = youtube_songs(YT_SEARCH);
        assert_eq!(songs.len(), 2);
        assert_eq!(songs[0], FoundTrack { id: "AbCdEfGhIjK".into(), title: "Glass Harbour".into(), artist: "The Lanterns".into(), duration_ms: 239_000 });
        assert_eq!((songs[1].artist.as_str(), songs[1].duration_ms), ("Someone Else", 3_845_000));
        assert!(youtube_songs("{}").is_empty());
    }

    #[test]
    fn youtube_lyrics_tab_and_page() {
        let next = r#"{"contents":{"singleColumnMusicWatchNextResultsRenderer":{"tabbedRenderer":{"watchNextTabbedResultsRenderer":{"tabs":[
          {"tabRenderer":{"title":"Up next","content":{}}},
          {"tabRenderer":{"title":"Lyrics","endpoint":{"browseEndpoint":{"browseId":"MPLYt_abc123","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_TRACK_LYRICS"}}}}}},
          {"tabRenderer":{"title":"Related","endpoint":{"browseEndpoint":{"browseId":"MPTRt_x"}}}}]}}}}}"#;
        assert_eq!(youtube_lyrics_page(next), Some(YoutubePage { browse_id: "MPLYt_abc123".into(), params: None }));
        let none = r#"{"tabs":[{"tabRenderer":{"title":"Lyrics","unselectable":true}}]}"#;
        assert_eq!(youtube_lyrics_page(none), None, "a tab that cannot be opened: no lyrics");
        let page = r#"{"contents":{"sectionListRenderer":{"contents":[{"musicDescriptionShelfRenderer":{"description":{"runs":[{"text":"Paper boats on a quiet river\nLa la la line two\n\nCounting lamps along the pier"}]},"footer":{"runs":[{"text":"Source: Somewhere"}]}}}]}}}"#;
        let l = from_youtube_music(page);
        assert!(!l.synced);
        assert_eq!(texts(&l), ["Paper boats on a quiet river", "La la la line two", "", "Counting lamps along the pier"]);
        assert!(from_youtube_music("{}").lines.is_empty());
    }

    #[test]
    fn youtube_captions_in_three_shapes() {
        let transcript = r#"{"actions":[{"updateEngagementPanelAction":{"content":{"transcriptRenderer":{"body":{"transcriptBodyRenderer":{"cueGroups":[
          {"transcriptCueGroupRenderer":{"cues":[{"transcriptCueRenderer":{"cue":{"simpleText":"♪ Paper boats on a quiet river ♪"},"startOffsetMs":"16210","durationMs":"3460"}}]}},
          {"transcriptCueGroupRenderer":{"cues":[{"transcriptCueRenderer":{"cue":{"simpleText":"[Music]"},"startOffsetMs":"19670","durationMs":"2000"}}]}},
          {"transcriptCueGroupRenderer":{"cues":[{"transcriptCueRenderer":{"cue":{"runs":[{"text":"counting lamps\nalong the pier (hey)"}]},"startOffsetMs":"22000","durationMs":"3000"}}]}}]}}}}}}]}"#;
        let l = from_youtube_captions(transcript);
        assert!(l.synced && !l.word_timed);
        assert_eq!(texts(&l), ["Paper boats on a quiet river", "counting lamps along the pier (hey)"]);
        assert_eq!((l.lines[0].start_ms, l.lines[0].end_ms), (16_210, 19_670));
        let segments = r#"{"transcriptSegmentRenderer":{"startMs":"1000","endMs":"2500","snippet":{"runs":[{"text":"Hello"}]}}}"#;
        assert_eq!(from_youtube_captions(segments).lines[0].end_ms, 2500);
        let json3 = r#"{"wireMagic":"pb3","events":[{"tStartMs":0,"dDurationMs":1000},{"tStartMs":1200,"dDurationMs":2000,"segs":[{"utf8":"hel"},{"utf8":"lo","tOffsetMs":400}]}]}"#;
        let j = from_youtube_captions(json3);
        assert_eq!(texts(&j), ["hello"]);
        assert!(j.lines[0].words.is_empty(), "speech-recognition word offsets are not singing");
    }
}
