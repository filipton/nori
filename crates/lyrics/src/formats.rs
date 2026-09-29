//! Third-party lyrics formats read into the app's shape: LRCLIB's lyricsfile (YAML), Apple-style TTML,
//! NetEase's YRC, KuGou's KRC, QQ Music's QRC, and the response cache's form. Words are timed only
//! where the format times them; a line timed as a whole stays whole. JSON is json.rs's, HTML html.rs's.

use nori_model::{LyricLine, LyricWord, Lyrics};
use yaml_rust2::{Yaml, YamlLoader};

use crate::lyrics::utf16_at;

/// One line as a source gives it, offsets still in bytes.
#[derive(Default)]
pub(crate) struct Timed {
    pub(crate) start: i64,
    pub(crate) end: Option<i64>,
    pub(crate) text: String,
    pub(crate) words: Vec<Word>,
    /// Backing vocals sung over the line ([`keep_backing`]).
    pub(crate) backing: Option<Box<Timed>>,
    /// Nothing but backing vocals, shown as a line of its own.
    pub(crate) background: bool,
    /// The singer's agent id, as the source names it ([`voices`]).
    pub(crate) agent: Option<String>,
    /// The duet side: 0 the first, 1 the other, 2 everyone together.
    pub(crate) voice: u8,
}

/// A sung word or syllable: its times and its byte range in the line's text.
pub(crate) struct Word {
    start: i64,
    end: Option<i64>,
    from: usize,
    to: usize,
}

impl Timed {
    /// Appends `piece`; a timed piece becomes a word over its letters, not the spaces around them.
    pub(crate) fn push(&mut self, piece: &str, time: Option<(i64, Option<i64>)>) {
        let lead = piece.len() - piece.trim_start().len();
        let at = self.text.len() + lead;
        self.text.push_str(piece);
        let body = piece.trim();
        if let (Some((start, end)), false) = (time, body.is_empty()) {
            self.words.push(Word { start, end, from: at, to: at + body.len() });
        }
    }
}

/// Lines in time order with their ends filled in: a line without an end runs to its last word or the
/// next line, a word without an end to the next word. Words keep their written order.
pub(crate) fn finish(mut lines: Vec<Timed>) -> Lyrics {
    for l in &mut lines {
        let kept = l.text.trim_end().len();
        l.text.truncate(kept);
    }
    lines.retain(|l| !l.text.trim().is_empty());
    lines.sort_by_key(|l| l.start);
    let starts: Vec<i64> = lines.iter().map(|l| l.start).collect();
    let word_timed = lines.iter().any(|l| !l.words.is_empty() || l.backing.as_ref().is_some_and(|b| !b.words.is_empty()));
    /// The words in UTF-16, each to its own end or the next word's start. A last word without an end
    /// runs to the line's given `end`, else for as long as a word that long is sung, never past `until`.
    fn timed(words: &[Word], text: &str, end: Option<i64>, until: i64) -> Vec<LyricWord> {
        words
            .iter()
            .enumerate()
            .map(|(k, w)| {
                let (from, to) = (utf16_at(text, w.from), utf16_at(text, w.to));
                let guessed = || end.unwrap_or_else(|| (w.start + nori_look::lyrics::word_ms_estimate(to - from)).min(until));
                let ends = w.end.or_else(|| words.get(k + 1).map(|n| n.start)).unwrap_or_else(guessed);
                LyricWord { start_ms: w.start, end_ms: ends.max(w.start), start: from, end: to }
            })
            .collect()
    }
    let out = lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let next_line = starts[i + 1..].iter().copied().find(|n| *n > l.start);
            let given = l.end.filter(|e| *e > l.start);
            let until = next_line.unwrap_or(l.start + 5_000);
            let words = timed(&l.words, &l.text, given, until);
            let (backing, backing_words) = match l.backing {
                Some(b) => {
                    let w = timed(&b.words, &b.text, given, until);
                    (b.text, w)
                }
                None => (String::new(), Vec::new()),
            };
            let last_word = words.iter().chain(&backing_words).map(|w| w.end_ms).max();
            let end = given.or(last_word.filter(|e| *e > l.start)).unwrap_or(until);
            LyricLine { start_ms: l.start, end_ms: end, text: l.text, words, translation: None, background: l.background, backing, backing_words, voice: l.voice }
        })
        .collect::<Vec<_>>();
    Lyrics { synced: !out.is_empty(), word_timed, lines: out, ..Default::default() }
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

pub fn timing(l: &Lyrics) -> Timing {
    match (l.lines.is_empty(), l.synced, l.word_timed) {
        (true, _, _) => Timing::Empty,
        (_, false, _) => Timing::Untimed,
        (_, true, true) => Timing::Words,
        _ => Timing::Lines,
    }
}

/// Untimed text, one line per line, blank lines kept between verses.
pub(crate) fn plain(text: &str) -> Lyrics {
    let lines: Vec<LyricLine> = text.lines().map(|l| LyricLine { start_ms: -1, end_ms: -1, text: l.trim().to_string(), ..Default::default() }).collect();
    if lines.iter().all(|l| l.text.is_empty()) {
        return Lyrics::default();
    }
    Lyrics { synced: false, word_timed: false, lines, ..Default::default() }
}

// ---- LRCLIB's lyricsfile ---------------------------------------------------------------------------

/// A lyricsfile (LRCLIB's and LRCGET's YAML, version 1.0). A file with only `plain` text is untimed; an
/// unknown version, an instrumental or anything that does not parse is empty.
pub fn from_lyricsfile(text: &str) -> Lyrics {
    // Lyrics are kilobytes; the cap stops a hostile file nesting the parser deep.
    if text.len() > 256 * 1024 {
        return Lyrics::default();
    }
    let Ok(docs) = YamlLoader::load_from_str(text) else { return Lyrics::default() };
    let Some(doc) = docs.first() else { return Lyrics::default() };
    // The spec: an unknown version is not 1.0. Unquoted, `1.0` reads as a number.
    let version = match &doc["version"] {
        Yaml::String(v) | Yaml::Real(v) => v.trim().to_string(),
        _ => String::new(),
    };
    if version != "1.0" || doc["metadata"]["instrumental"].as_bool() == Some(true) {
        return Lyrics::default();
    }
    let ms = |y: &Yaml| y.as_i64().filter(|v| *v >= 0);
    let mut lines = Vec::new();
    for l in doc["lines"].as_vec().map(Vec::as_slice).unwrap_or_default() {
        let (Some(text), Some(start)) = (l["text"].as_str(), ms(&l["start_ms"])) else { continue };
        let mut line = Timed { start, end: ms(&l["end_ms"]), ..Default::default() };
        let words: Vec<(&str, i64, Option<i64>)> = l["words"]
            .as_vec()
            .map(|ws| ws.iter().filter_map(|w| Some((w["text"].as_str()?, ms(&w["start_ms"])?, ms(&w["end_ms"])))).collect())
            .unwrap_or_default();
        let joined: String = words.iter().map(|w| w.0).collect();
        if !words.is_empty() && joined.trim() == text.trim() {
            for (piece, s, e) in words {
                line.push(piece, Some((s, e)));
            }
        } else {
            // Words that do not spell the line cannot be placed in it: the line stays line-timed.
            line.push(text, None);
        }
        lines.push(line);
    }
    if !lines.is_empty() {
        return finish(lines);
    }
    doc["plain"].as_str().map(plain).unwrap_or_default()
}

// ---- Apple-style TTML (Unison, BiniLyrics) -------------------------------------------------------

/// A TTML time: `12.345`, `1:02.345`, `1:02:03.456`, or with a unit, `12.3s`, `450ms`, `2m`, `1h`.
fn clock(s: &str) -> Option<i64> {
    let s = s.trim();
    let num = |v: &str| v.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x >= 0.0);
    let whole = |v: &str| v.trim().parse::<u32>().ok().map(f64::from);
    let ms = if let Some(v) = s.strip_suffix("ms") {
        num(v)?
    } else if let Some(v) = s.strip_suffix('s') {
        num(v)? * 1_000.0
    } else if let Some(v) = s.strip_suffix('m') {
        num(v)? * 60_000.0
    } else if let Some(v) = s.strip_suffix('h') {
        num(v)? * 3_600_000.0
    } else {
        match s.split(':').collect::<Vec<_>>().as_slice() {
            [sec] => num(sec)? * 1_000.0,
            [m, sec] => (whole(m)? * 60.0 + num(sec)?) * 1_000.0,
            [h, m, sec] => (whole(h)? * 3_600.0 + whole(m)? * 60.0 + num(sec)?) * 1_000.0,
            _ => return None,
        }
    };
    Some(ms.round() as i64)
}

/// An attribute by its local name, whatever its namespace prefix (`ttm:role`).
fn attr<'a>(n: roxmltree::Node<'a, '_>, local: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == local).map(|a| a.value())
}

/// Adds text to a line with each run of whitespace as one space and none at the start, so layout in
/// the source is not part of the words.
pub(crate) fn append(t: &mut Timed, raw: &str, time: Option<(i64, Option<i64>)>) {
    let mut piece = String::with_capacity(raw.len());
    for (i, part) in raw.split(char::is_whitespace).enumerate() {
        if i > 0 && !piece.ends_with(' ') {
            piece.push(' ');
        }
        piece.push_str(part);
    }
    let piece = if t.text.is_empty() || t.text.ends_with(' ') { piece.trim_start() } else { piece.as_str() };
    t.push(piece, time);
}

/// Walks one `<p>`: timed spans become words (syllables; a space between spans ends a word), backing
/// vocals (`x-bg`) go to `backing`, inline translations and romanisations are left out.
fn walk(node: roxmltree::Node, line: &mut Timed, backing: &mut Timed, in_bg: bool, depth: usize) {
    // Real documents nest two deep; the limit stops a hostile one overflowing the stack.
    if depth > 16 {
        return;
    }
    for child in node.children() {
        let target = if in_bg { &mut *backing } else { &mut *line };
        if child.is_text() {
            append(target, child.text().unwrap_or(""), None);
            continue;
        }
        if !child.is_element() {
            continue;
        }
        match child.tag_name().name() {
            "br" => append(target, " ", None),
            "span" => match attr(child, "role") {
                Some("x-bg") => walk(child, line, backing, true, depth + 1),
                Some(r) if r.starts_with("x-translation") || r.starts_with("x-roman") => {}
                _ => {
                    let begin = attr(child, "begin").and_then(clock);
                    let finer = child.descendants().skip(1).any(|d| d.is_element() && attr(d, "begin").is_some());
                    match begin {
                        Some(b) if !finer => {
                            let text: String = child.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect();
                            append(target, &text, Some((b, attr(child, "end").and_then(clock))));
                        }
                        _ => walk(child, line, backing, in_bg, depth + 1),
                    }
                }
            },
            _ => {}
        }
    }
}

/// Keeps backing vocals inside their line with their own times, since the player lights one line at a
/// time; a line of nothing but backing vocals becomes a line marked as backing.
pub(crate) fn keep_backing(line: &mut Timed, mut backing: Timed) {
    let sung = backing.text.trim_end().len();
    backing.text.truncate(sung);
    if backing.text.trim().is_empty() {
        return;
    }
    if line.text.trim().is_empty() {
        line.text = backing.text;
        line.words = backing.words;
        line.background = true;
        return;
    }
    line.backing = Some(Box::new(backing));
}

/// Whether an agent id is everyone together: declared a group, or Apple's undeclared `v1000`.
fn together(id: &str, kinds: &std::collections::HashMap<String, String>) -> bool {
    match kinds.get(id) {
        Some(kind) => kind == "group",
        None => id == "v1000",
    }
}

/// Duet sides from each line's agent (`kinds`: each id's declared type; undeclared is a person). The
/// side flips whenever the singer changes, so three voices still alternate. Everyone together is side 2
/// and does not flip; a line naming nobody keeps the current side. Under two singers, all is side 0; a
/// song nearly all on side 1 is flipped.
pub(crate) fn voices(lines: &mut [Timed], kinds: &std::collections::HashMap<String, String>) {
    lines.sort_by_key(|l| l.start);
    let people: std::collections::HashSet<&str> =
        lines.iter().filter_map(|l| l.agent.as_deref()).filter(|a| !together(a, kinds)).collect();
    let duet = people.len() >= 2;
    let mut side = 0u8;
    let mut last: Option<String> = None;
    for l in lines.iter_mut() {
        l.voice = match l.agent.as_deref() {
            Some(a) if together(a, kinds) => 2,
            Some(a) if duet => {
                if last.as_deref().is_some_and(|p| p != a) {
                    side ^= 1;
                }
                last = Some(a.to_string());
                side
            }
            Some(_) => 0,
            None => side,
        };
    }
    let sung = lines.iter().filter(|l| l.voice < 2).count();
    let right = lines.iter().filter(|l| l.voice == 1).count();
    if sung > 0 && right * 5 > sung * 4 {
        for l in lines.iter_mut().filter(|l| l.voice < 2) {
            l.voice ^= 1;
        }
    }
}

/// Apple-style TTML (Unison, BiniLyrics): a `<p begin end>` per line, a `<span begin end>` per syllable.
/// Untimed lines make untimed lyrics; line times alone give no words.
pub fn from_ttml(text: &str) -> Lyrics {
    if text.len() > 2 * 1024 * 1024 {
        return Lyrics::default();
    }
    let Ok(doc) = roxmltree::Document::parse(text) else { return Lyrics::default() };
    let mut lines = Vec::new();
    let mut untimed = Vec::new();
    // Each agent's declared type; a line's agent is on the line or its section.
    let kinds: std::collections::HashMap<String, String> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "agent")
        .filter_map(|n| Some((attr(n, "id")?.to_string(), attr(n, "type").unwrap_or("person").to_string())))
        .collect();
    for p in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "p") {
        let (mut line, mut backing) = (Timed::default(), Timed::default());
        walk(p, &mut line, &mut backing, false, 0);
        keep_backing(&mut line, backing);
        line.agent = p.ancestors().find_map(|n| attr(n, "agent")).map(str::to_string);
        match attr(p, "begin").and_then(clock).or(line.words.first().map(|w| w.start)) {
            Some(start) => {
                line.start = start;
                line.end = attr(p, "end").and_then(clock);
                lines.push(line);
            }
            None => untimed.push(line.text),
        }
    }
    // An untimed line in a timed document has no place: it is left out.
    if lines.is_empty() {
        return plain(&untimed.join("\n"));
    }
    voices(&mut lines, &kinds);
    finish(lines)
}

// ---- karaoke lines: NetEase's YRC, KuGou's KRC --------------------------------------------------

/// A numeric tag at the start of `s` (`[12,34]`, `(12,34,0)`, `<12,34,0>`): its numbers and byte length.
/// Other bracketed text (`(ooh)`) is not a tag.
fn tag(s: &str, open: char, close: char) -> Option<(Vec<i64>, usize)> {
    let body = s.strip_prefix(open)?;
    let end = body.find(close)?;
    let nums = body[..end].split(',').map(|n| n.trim().parse::<i64>().ok()).collect::<Option<Vec<_>>>()?;
    (2..=3).contains(&nums.len()).then_some((nums, open.len_utf8() + end + close.len_utf8()))
}

/// One karaoke line: `[start,length]`, then each word led by its tag: `(start,length,0)` from the song's
/// start in YRC, `<offset,length,0>` from the line's start in KRC.
fn karaoke_line(raw: &str, open: char, close: char, from_line: bool) -> Option<Timed> {
    let (head, used) = tag(raw, '[', ']')?;
    let start = head[0].max(0);
    let mut line = Timed { start, end: Some(start + head[1].max(0)), ..Default::default() };
    let mut rest = &raw[used..];
    let mut time: Option<(i64, Option<i64>)> = None;
    loop {
        let next = rest.char_indices().filter(|(_, c)| *c == open).find_map(|(i, _)| tag(&rest[i..], open, close).map(|t| (i, t)));
        let upto = next.as_ref().map_or(rest.len(), |(i, _)| *i);
        append(&mut line, &decode_html(&rest[..upto]), time);
        let Some((i, (nums, len))) = next else { break };
        let at = nums[0].max(0) + if from_line { start } else { 0 };
        time = Some((at, Some(at + nums[1].max(0))));
        rest = &rest[i + len..];
    }
    Some(line)
}

/// Every line `line` reads, shifted by the file's `[offset:]` (positive is sooner, as in LRC).
fn karaoke(text: &str, line: impl Fn(&str) -> Option<Timed>) -> Vec<Timed> {
    let mut offset = 0i64;
    let mut lines = Vec::new();
    for raw in text.lines() {
        let raw = raw.trim_start_matches('\u{feff}').trim();
        if let Some(v) = raw.strip_prefix("[offset:").and_then(|r| r.strip_suffix(']')) {
            offset = v.trim().parse().unwrap_or(0);
        } else if let Some(l) = line(raw) {
            lines.push(l);
        }
    }
    if offset != 0 {
        for l in &mut lines {
            l.start = (l.start - offset).max(0);
            l.end = l.end.map(|e| (e - offset).max(0));
            for w in &mut l.words {
                w.start = (w.start - offset).max(0);
                w.end = w.end.map(|e| (e - offset).max(0));
            }
        }
    }
    lines
}

/// Karaoke lines without the credits at their ends (`title` spots a header naming the song).
fn finish_karaoke(mut lines: Vec<Timed>, title: &str) -> Lyrics {
    crate::credits::strip_lines(&mut lines, |l| l.text.as_str(), title, "");
    finish(lines)
}

/// LRC without the credits at its ends; empty when nothing is left.
fn lrc_without_credits(text: &str, title: &str) -> Lyrics {
    let mut lrc = crate::lyrics::from_lrc(text);
    crate::credits::strip_lines(&mut lrc.lines, |l| l.text.as_str(), title, "");
    if lrc.lines.iter().all(|l| l.text.trim().is_empty()) {
        return Lyrics::default();
    }
    lrc
}

/// NetEase's lyrics: its YRC (word-timed, from the song's start) when it times words, else its LRC
/// without the JSON credit lines (`{"t":0,"c":[…]}`).
pub fn from_netease(yrc: &str, lrc: &str, title: &str) -> Lyrics {
    let yrc = finish_karaoke(karaoke(yrc, |l| karaoke_line(l, '(', ')', false)), title);
    if yrc.word_timed {
        return yrc;
    }
    let lrc: String = lrc.lines().filter(|l| !l.trim_start().starts_with('{')).collect::<Vec<_>>().join("\n");
    lrc_without_credits(&lrc, title)
}

/// The public key every KuGou client XORs a KRC file with.
const KRC_KEY: [u8; 16] = [0x40, 0x47, 0x61, 0x77, 0x5e, 0x32, 0x74, 0x47, 0x51, 0x36, 0x31, 0x2d, 0xce, 0xd2, 0x6e, 0x69];

/// Standard or URL-safe base64, padding and line breaks allowed.
fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// KuGou's KRC as its download API sends it: base64 of `krc1` and a zlib stream XORed with [KRC_KEY].
/// Translations are not used. An error means the content is not KRC.
pub fn from_krc(content: &str, title: &str) -> Result<Lyrics, String> {
    let bytes = base64(content.trim()).ok_or("not base64")?;
    let body = bytes.strip_prefix(b"krc1".as_slice()).ok_or("not a KRC file")?;
    let packed: Vec<u8> = body.iter().enumerate().map(|(i, b)| b ^ KRC_KEY[i % KRC_KEY.len()]).collect();
    // The limit stops a hostile stream inflating without bound.
    let text = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&packed, 4 << 20).map_err(|e| format!("inflate: {e}"))?;
    Ok(finish_karaoke(karaoke(&String::from_utf8_lossy(&text), |l| karaoke_line(l, '<', '>', true)), title))
}

// ---- QQ Music's QRC (through BetterLyrics' Portato) ------------------------------------------------

/// The named references lyrics pages use, beyond XML's five.
const NAMED: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", ' '),
    ("rsquo", '\u{2019}'),
    ("lsquo", '\u{2018}'),
    ("rdquo", '\u{201d}'),
    ("ldquo", '\u{201c}'),
    ("hellip", '\u{2026}'),
    ("mdash", '\u{2014}'),
    ("ndash", '\u{2013}'),
];

/// Decodes XML's entities, numeric references and [`NAMED`] in one pass (so `&amp;#39;` stays `&#39;`);
/// a no-break space becomes a plain one. Anything else is left as written.
pub(crate) fn decode_html(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let name = rest[1..].find(';').filter(|e| *e <= 10).map(|e| &rest[1..1 + e]);
        let c = name.and_then(|n| {
            let code = match n.strip_prefix('#') {
                Some(num) => match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse::<u32>().ok(),
                },
                None => return NAMED.iter().find(|(k, _)| *k == n).map(|(_, c)| *c),
            };
            code.and_then(char::from_u32).map(|c| if c == '\u{a0}' { ' ' } else { c })
        });
        match (c, name) {
            (Some(c), Some(n)) => {
                out.push(c);
                rest = &rest[n.len() + 2..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A QRC document's text: the `LyricContent` attribute of QQ's XML wrapper, or the text itself.
fn qrc_body(text: &str) -> String {
    const ATTR: &str = "LyricContent=\"";
    match text.find(ATTR) {
        Some(at) => {
            let rest = &text[at + ATTR.len()..];
            decode_html(&rest[..rest.find('"').unwrap_or(rest.len())])
        }
        None => text.to_string(),
    }
}

/// One QRC line: `[start,length]`, then each word followed (unlike YRC) by its `(start,length)` from the
/// song's start; text after the last tag is untimed.
fn qrc_line(raw: &str) -> Option<Timed> {
    let (head, used) = tag(raw, '[', ']')?;
    let start = head[0].max(0);
    let mut line = Timed { start, end: Some(start + head[1].max(0)), ..Default::default() };
    let mut rest = &raw[used..];
    loop {
        let next = rest
            .char_indices()
            .filter(|(_, c)| *c == '(')
            .find_map(|(i, _)| tag(&rest[i..], '(', ')').filter(|(n, _)| n.len() == 2).map(|t| (i, t)));
        let Some((i, (nums, len))) = next else {
            append(&mut line, &decode_html(rest), None);
            break;
        };
        let at = nums[0].max(0);
        append(&mut line, &decode_html(&rest[..i]), Some((at, Some(at + nums[1].max(0)))));
        rest = &rest[i + len..];
    }
    Some(line)
}

/// QQ Music's QRC as Portato sends it (decrypted, bare or wrapped); a song without QRC comes as LRC.
pub fn from_qrc(text: &str, title: &str) -> Lyrics {
    if text.len() > 4 << 20 {
        return Lyrics::default();
    }
    let body = qrc_body(text);
    let qrc = finish_karaoke(karaoke(&body, qrc_line), title);
    if !qrc.lines.is_empty() {
        return qrc;
    }
    lrc_without_credits(&body, title)
}

// ---- the cache ------------------------------------------------------------------------------------

/// Marks a cache entry as this crate's JSON; older entries without it are LRC.
const CACHE_MARK: &str = "nori-lyrics-1\n";

/// Lyrics as the response cache keeps them, every word timing included.
pub fn to_cache(lyrics: &Lyrics) -> String {
    format!("{CACHE_MARK}{}", serde_json::to_string(lyrics).unwrap_or_default())
}

pub fn from_cache(text: &str) -> Lyrics {
    match text.strip_prefix(CACHE_MARK) {
        Some(json) => serde_json::from_str(json).unwrap_or_default(),
        None => crate::lyrics::from_lrc(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LYRICSFILE: &str = include_str!("../testdata/lrclib.lyricsfile.yaml");

    #[test]
    fn lyricsfile_words_are_real_and_utf16() {
        let l = from_lyricsfile(LYRICSFILE);
        assert!(l.synced && l.word_timed);
        assert_eq!(l.lines.len(), 3);
        let w = &l.lines[0].words;
        assert_eq!(w.len(), 4);
        assert_eq!((w[0].start, w[0].end, w[1].start, w[1].end), (0, 1, 2, 14), "the space after a word is not part of it");
        assert_eq!((w[3].start_ms, w[3].end_ms), (14800, 15500), "a word without an end runs to its line's end");
        let polish = &l.lines[1];
        assert_eq!(polish.end_ms, 17500, "a line without an end runs to its last word");
        assert_eq!((polish.words[1].start, polish.words[1].end), (7, 12), "UTF-16 offsets, not bytes");
        assert!(l.lines[2].words.is_empty(), "a line timed as a whole is never given word times");
        assert_eq!(l.lines[2].end_ms, 23000);
    }

    #[test]
    fn open_last_word_gets_a_sung_length() {
        let l = from_lyricsfile("version: '1.0'\nmetadata: {title: t, artist: a}\nlines:\n  - {text: hold on tonight, start_ms: 10000, words: [{text: 'hold ', start_ms: 10000}, {text: 'on ', start_ms: 10400}, {text: tonight, start_ms: 10800}]}\n  - {text: again, start_ms: 40000}\n");
        let last = l.lines[0].words.last().unwrap().clone();
        assert!(last.end_ms > 10_800 && last.end_ms <= 12_800, "{last:?}");
        assert_eq!(l.lines[0].end_ms, last.end_ms, "the line ends with it");
    }

    #[test]
    fn lyricsfile_edges() {
        // Words that do not spell the line: the line keeps its timing, the words are dropped.
        let off = from_lyricsfile("version: \"1.0\"\nmetadata: {title: t, artist: a}\nlines:\n  - {text: hello there, start_ms: 1000, words: [{text: 'bye ', start_ms: 1000}]}\n");
        assert!(off.synced && !off.word_timed && off.lines[0].words.is_empty());
        // Line timing only.
        let lined = from_lyricsfile("version: 1.0\nmetadata: {title: t, artist: a}\nlines:\n  - {text: one, start_ms: 2000}\n  - {text: two, start_ms: 1000}\n");
        assert!(lined.synced && !lined.word_timed);
        assert_eq!(lined.lines.iter().map(|l| l.start_ms).collect::<Vec<_>>(), [1000, 2000], "sorted by time");
        // Plain only, and blank lines kept between verses.
        let p = from_lyricsfile("version: '1.0'\nmetadata: {title: t, artist: a}\nplain: |\n  first\n\n  second\n");
        assert!(!p.synced && p.lines.len() == 3 && p.lines[1].text.is_empty());
        assert!(from_lyricsfile("version: '2.0'\nmetadata: {title: t, artist: a}\nplain: x\n").lines.is_empty(), "unknown version");
        assert!(from_lyricsfile("version: '1.0'\nmetadata: {title: t, artist: a, instrumental: true}\n").lines.is_empty());
        assert!(from_lyricsfile("version: '1.0'\nversion: '1.0'\n").lines.is_empty(), "duplicate keys are refused");
        assert!(from_lyricsfile("not: [yaml").lines.is_empty());
        assert!(from_lyricsfile("").lines.is_empty());
        // Zero-length and negative-length words stay in place and never run backwards.
        let z = from_lyricsfile("version: '1.0'\nmetadata: {title: t, artist: a}\nlines:\n  - {text: ab, start_ms: 0, words: [{text: a, start_ms: 0, end_ms: 0}, {text: b, start_ms: 500, end_ms: 400}]}\n");
        assert_eq!(z.lines[0].words.iter().map(|w| (w.start_ms, w.end_ms)).collect::<Vec<_>>(), [(0, 0), (500, 500)]);
    }

    /// The shape Apple Music's lyrics come in (and so BiniLyrics' and Unison's word-synced entries):
    /// syllables as spans, words ending where a space falls between spans, a duet agent, backing vocals.
    const TTML: &str = include_str!("../testdata/apple.ttml");

    #[test]
    fn ttml_syllables_words_and_backing_vocals() {
        let l = from_ttml(TTML);
        assert!(l.synced && l.word_timed);
        assert_eq!(l.lines.len(), 2);
        let first = &l.lines[0];
        assert_eq!(first.text, "Every time rock & roll");
        assert_eq!((first.start_ms, first.end_ms), (12_345, 15_678));
        let spans: Vec<(i64, u32, u32)> = first.words.iter().map(|w| (w.start_ms, w.start, w.end)).collect();
        // "Ev" + "ery" are two syllables of one word.
        assert_eq!(spans, [(12_345, 0, 2), (12_900, 2, 5), (13_200, 6, 10), (13_600, 11, 22)]);
        assert_eq!(first.words[3].end_ms, 14_100);
        // The backing vocal is kept apart from the lead, with its own times and offsets into its own text.
        assert_eq!(first.backing, "(ooh, ooh)");
        assert_eq!(first.backing_words.iter().map(|w| (w.start_ms, w.end_ms, w.start, w.end)).collect::<Vec<_>>(), [(14_200, 14_800, 0, 5), (14_800, 15_500, 6, 10)]);
        // Two singers: the first on the first side, the second on the other.
        assert_eq!(l.lines.iter().map(|x| x.voice).collect::<Vec<_>>(), [0, 1]);
        let cjk = &l.lines[1];
        assert_eq!((cjk.text.as_str(), cjk.start_ms), ("夜に駆ける", 62_500));
        assert_eq!(cjk.words.iter().map(|w| (w.start, w.end, w.end_ms - w.start_ms)).collect::<Vec<_>>(), [(0, 1, 500), (1, 2, 0), (2, 5, 900)], "a zero-length syllable stays zero");
    }

    fn sung_by(agents: &[Option<&str>]) -> Vec<Timed> {
        agents.iter().enumerate().map(|(i, a)| Timed { start: i as i64 * 1_000, text: format!("line {i}"), agent: a.map(str::to_string), ..Default::default() }).collect()
    }

    fn sides(lines: &mut [Timed], kinds: &[(&str, &str)]) -> Vec<u8> {
        let kinds = kinds.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        voices(lines, &kinds);
        lines.iter().map(|l| l.voice).collect()
    }

    #[test]
    fn duet_sides() {
        // One singer, or none named: every line on the first side.
        assert_eq!(sides(&mut sung_by(&[Some("v1"), Some("v1"), None]), &[]), [0, 0, 0]);
        // The side changes with the singer, so a third voice takes a turn instead of sharing a side.
        assert_eq!(sides(&mut sung_by(&[Some("v1"), Some("v2"), Some("v3"), Some("v1"), Some("v1")]), &[]), [0, 1, 0, 1, 1]);
        // Everyone together is its own kind, drawn on the first side, and does not change whose turn it is.
        let kinds = [("v1", "person"), ("v2", "person"), ("choir", "group")];
        assert_eq!(sides(&mut sung_by(&[Some("v1"), Some("choir"), Some("v2"), Some("v1000"), Some("v2")]), &kinds), [0, 2, 1, 2, 1]);
        // A line that names nobody stays on the side whose turn it is.
        assert_eq!(sides(&mut sung_by(&[Some("v1"), Some("v2"), None, Some("v1"), Some("v2")]), &[]), [0, 1, 1, 0, 1]);
        // Nearly all on the other side is turned round.
        assert_eq!(sides(&mut sung_by(&[Some("v1"), Some("v2"), Some("v2"), Some("v2"), Some("v2"), Some("v2")]), &[]), [1, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn backing_only_lines_become_lines() {
        let mut line = Timed { start: 1_000, ..Default::default() };
        let mut backing = Timed::default();
        backing.push("(la la)", Some((1_000, Some(1_500))));
        keep_backing(&mut line, backing);
        assert!(line.background && line.backing.is_none());
        let l = finish(vec![line]);
        assert_eq!((l.lines[0].text.as_str(), l.lines[0].background, l.lines[0].words.len()), ("(la la)", true, 1));
        // Nothing but spaces is no backing at all.
        let mut lead = Timed { start: 0, text: "Paper boats".into(), ..Default::default() };
        let mut blank = Timed::default();
        blank.push("  ", None);
        keep_backing(&mut lead, blank);
        assert!(lead.backing.is_none() && !lead.background);
    }

    #[test]
    fn ttml_line_timed_plain_laid_out_and_broken() {
        let lined = from_ttml(r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="00:00:05.000" end="00:00:07.500">One line</p><p begin="7.5s" end="9000ms">Two</p></div></body></tt>"#);
        assert!(lined.synced && !lined.word_timed);
        assert_eq!(lined.lines.iter().map(|l| (l.start_ms, l.end_ms)).collect::<Vec<_>>(), [(5_000, 7_500), (7_500, 9_000)]);
        assert!(lined.lines.iter().all(|l| l.words.is_empty()), "line timing is never turned into word timing");
        let untimed = from_ttml(r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p>First</p><p>Second<br/>half</p></div></body></tt>"#);
        assert!(!untimed.synced);
        assert_eq!(untimed.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["First", "Second half"]);
        // Written over several lines with indentation: the layout is not part of the words.
        let laid_out = from_ttml(
            "<tt xmlns=\"http://www.w3.org/ns/ttml\">\n  <body>\n    <p begin=\"1\" end=\"3\">\n      <span begin=\"1\" end=\"2\">Hello</span>\n      <span begin=\"2\" end=\"3\">there</span>\n    </p>\n  </body>\n</tt>",
        );
        assert_eq!(laid_out.lines[0].text, "Hello there");
        assert_eq!(laid_out.lines[0].words.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(), [(0, 5), (6, 11)]);
        assert!(from_ttml("<tt><body><p begin='1'>unclosed</body></tt>").lines.is_empty());
        assert!(from_ttml("").lines.is_empty());
        assert_eq!((clock("1:02:03.5"), clock("bogus"), clock("-1")), (Some(3_723_500), None, None));
    }

    /// What NetEase's `yrc.lyric` looks like: JSON credit lines, then `[start,length]` lines whose words
    /// each carry `(start,length,0)` from the start of the song, with the space inside the word.
    const YRC: &str = include_str!("../testdata/netease.yrc");

    #[test]
    fn yrc_words_are_absolute_and_credits_go() {
        let l = from_netease(YRC, "[00:16.21]When you were here", "Creep");
        assert!(l.synced && l.word_timed);
        assert_eq!(l.lines.len(), 3, "the JSON credit lines are not lyrics");
        let first = &l.lines[0];
        assert_eq!((first.text.as_str(), first.start_ms, first.end_ms), ("When you were here", 16_210, 19_670));
        assert_eq!(first.words.iter().map(|w| (w.start_ms, w.end_ms, w.start, w.end)).collect::<Vec<_>>(), [(16_210, 16_880, 0, 4), (16_880, 17_290, 5, 8), (17_290, 17_830, 9, 13), (17_830, 19_670, 14, 18)]);
        let second = &l.lines[1];
        assert_eq!(second.text, "I don't care(ooh)", "a bracket that is not a time tag is text");
        assert_eq!((second.words[1].start_ms, second.words[1].end_ms), (21_100, 21_100), "zero-length words stay zero");
        assert_eq!(second.words.len(), 4);
        assert_eq!(l.lines[2].words.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(), [(0, 1), (1, 2), (2, 5)]);
    }

    #[test]
    fn netease_lrc_fallback() {
        let lrc = "{\"t\":0,\"c\":[{\"tx\":\"作词: Someone\"}]}\n[00:00.000] 作词 : Thom Yorke\n[00:01.000] 作曲 : Radiohead\n[00:02.000] Produced by: Someone\n[00:22.500]When you were here before\n[00:26.000]Couldn't look you in the eye\n[03:50.000]Mixed by: Someone\n";
        let l = from_netease("", lrc, "Creep");
        assert!(l.synced && !l.word_timed, "line timing stays line timing");
        assert_eq!(l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["When you were here before", "Couldn't look you in the eye"]);
        // A YRC without a single timed word is no better than the LRC.
        assert!(!from_netease("[1000,500]just text", lrc, "").word_timed);
        assert!(from_netease("", "", "x").lines.is_empty());
        // Lines that merely contain a colon, further in, are sung words.
        let sung = from_netease("", "[00:01.00]First line\n[00:02.00]She said: stay\n", "");
        assert_eq!(sung.lines.len(), 2);
        assert!(crate::credits::is_credit("编曲：某人", "", ""));
    }

    /// Packs KRC text the way KuGou's download API does: zlib, XOR with the key, `krc1`, base64.
    fn pack_krc(text: &str) -> String {
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(text.as_bytes(), 6);
        let mut bytes = b"krc1".to_vec();
        bytes.extend(packed.iter().enumerate().map(|(i, b)| b ^ KRC_KEY[i % 16]));
        crate::services::base64(&bytes)
    }

    const KRC: &str = include_str!("../testdata/kugou.krc.txt");

    #[test]
    fn krc_decrypts_and_times_words_from_the_line() {
        let l = from_krc(&pack_krc(KRC), "Creep").unwrap();
        assert!(l.synced && l.word_timed);
        assert_eq!(l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["When you were here", "我爱你"], "the title line and credits go");
        let w = &l.lines[0].words;
        assert_eq!(w.iter().map(|w| (w.start_ms, w.end_ms)).collect::<Vec<_>>(), [(22_000, 22_420), (22_420, 22_720), (22_720, 23_220), (23_220, 25_450)]);
        assert_eq!(l.lines[1].words.iter().map(|w| (w.start, w.end, w.end_ms - w.start_ms)).collect::<Vec<_>>(), [(0, 1, 500), (1, 2, 0), (2, 3, 1000)]);
        // An offset moves everything, as in LRC: positive is sooner.
        let early = from_krc(&pack_krc(&KRC.replace("[offset:0]", "[offset:500]")), "Creep").unwrap();
        assert_eq!((early.lines[0].start_ms, early.lines[0].words[0].start_ms), (21_500, 21_500));
    }

    #[test]
    fn krc_that_is_not_one_is_an_error() {
        assert!(from_krc("!!!", "").is_err());
        assert!(from_krc("aGVsbG8=", "").is_err(), "base64, but not krc1");
        let short: String = pack_krc("x").chars().take(8).collect();
        assert!(from_krc(&short, "").is_err(), "a cut-off stream");
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVs\nbG8").unwrap(), b"hello");
    }

    /// QQ's QRC inside its XML wrapper, as a Portato answer carries it (invented words and names): a
    /// title line and a credit line QQ times as if sung, each word followed by its own time, a space
    /// timed on its own, a bracket that is text, an entity in the attribute, and CJK.
    const QRC: &str = include_str!("../testdata/qq.qrc.xml");

    #[test]
    fn qrc_words_follow_their_times() {
        let l = from_qrc(QRC, "Glass Harbour");
        assert!(l.synced && l.word_timed);
        assert_eq!(l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["Paper boats drift home", "We're here (la)", "紙の舟"], "the title line and the credit go");
        let w = |i: usize| l.lines[i].words.iter().map(|w| (w.start_ms, w.end_ms, w.start, w.end)).collect::<Vec<_>>();
        assert_eq!(w(0), [(22_000, 22_420, 0, 5), (22_420, 22_720, 6, 11), (22_720, 23_220, 12, 17), (23_220, 25_450, 18, 22)]);
        assert_eq!(w(1), [(26_000, 26_600, 0, 5), (26_600, 27_300, 6, 10), (27_300, 28_000, 11, 15)], "a space timed alone is no word; '(la)' is text");
        assert_eq!(w(2).iter().map(|x| (x.2, x.3, x.1 - x.0)).collect::<Vec<_>>(), [(0, 1, 500), (1, 2, 0), (2, 3, 1000)]);
        // Bare QRC, no wrapper; and an offset, as in LRC.
        let bare = from_qrc("[offset:500]\n[1000,1000]Hi (1000,500)there(1500,500)", "");
        assert_eq!((bare.lines[0].start_ms, bare.lines[0].words[1].start_ms), (500, 1000));
    }

    #[test]
    fn qrc_falls_back_to_lrc_and_plain_lines() {
        let lrc = from_qrc("[ti:x]\n[00:01.00]作词：Someone\n[00:05.00]Paper boats\n[00:09.00]La la la", "x");
        assert!(lrc.synced && !lrc.word_timed);
        assert_eq!(lrc.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["Paper boats", "La la la"]);
        // `[start,length]` lines with no word times stay timed by line.
        let lined = from_qrc("[1000,2000]Paper boats\n[3000,2000]La la la", "");
        assert!(lined.synced && !lined.word_timed && lined.lines[0].end_ms == 3000);
        assert!(from_qrc("", "").lines.is_empty());
        assert!(from_qrc("<QrcInfos><Lyric_1 LyricContent=\"\"/></QrcInfos>", "").lines.is_empty());
    }

    #[test]
    fn cache_keeps_every_word_and_reads_old_lrc() {
        let l = from_lyricsfile(LYRICSFILE);
        let back = from_cache(&to_cache(&l));
        assert!(back.word_timed);
        assert_eq!(back.lines.len(), l.lines.len());
        assert_eq!(back.lines[1].words, l.lines[1].words);
        let old = from_cache("[00:01.00]from an older version");
        assert!(old.synced && !old.word_timed && old.lines[0].start_ms == 1000);
    }
}
