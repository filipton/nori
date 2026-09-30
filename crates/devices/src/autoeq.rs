//! The AutoEQ headphone index kept on the device: `results/INDEX.md` (one ~850 KB request) parsed into a
//! table so searches are local. Only the index (`index_due`) and chosen curves are fetched. A curve is
//! "ParametricEQ.txt", or else the fitted "GraphicEQ.txt"; entries with neither are hidden
//! (`mark_missing`).

use nori_model::model::AutoEqEntry;
use rusqlite::{params, Connection};

pub const INDEX_URL: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/INDEX.md";
const RAW: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results";
/// Age after which the index is refetched on an unmetered network.
pub(crate) const INDEX_STALE_MS: i64 = 30 * 24 * 3_600_000;
/// `app_kv` keys for the index's fetch time and fingerprint.
const FETCHED_KEY: &str = "autoeq.fetched";
const DIGEST_KEY: &str = "autoeq.digest";

/// Parses `- [Name](./source/form/Name) by source on target`. Paths contain unescaped parentheses, so
/// the link ends at the balancing `)`.
fn entry(line: &str) -> Option<AutoEqEntry> {
    let rest = line.strip_prefix("- [")?;
    let (name, rest) = rest.split_once("](")?;
    let mut depth = 0usize;
    let end = rest.char_indices().find_map(|(i, c)| match c {
        '(' => {
            depth += 1;
            None
        }
        ')' if depth == 0 => Some(i),
        ')' => {
            depth -= 1;
            None
        }
        _ => None,
    })?;
    let (path, tail) = (&rest[..end], &rest[end + 1..]);
    let path = path.trim_start_matches("./");
    let mut parts = path.split('/');
    // The path is percent-encoded; the labels are decoded.
    let source = decode(parts.next()?);
    let form = decode(parts.next().unwrap_or_default());
    let target = tail.split_once(" on ").map(|(_, t)| t.trim().to_string()).unwrap_or_default();
    Some(AutoEqEntry { name: name.trim().to_string(), source, form, target, path: path.to_string() })
}

/// An entry's parametric preset URL (the file name repeats the folder name).
pub fn preset_url(e: &AutoEqEntry) -> String {
    file_url(e, "ParametricEQ")
}

/// An entry's graphic curve URL.
pub(crate) fn graphic_url(e: &AutoEqEntry) -> String {
    file_url(e, "GraphicEQ")
}

fn file_url(e: &AutoEqEntry, kind: &str) -> String {
    let leaf = e.path.rsplit('/').next().unwrap_or_default();
    format!("{RAW}/{}/{} {kind}.txt", e.path, decode(leaf)).replace(' ', "%20")
}

/// Percent-decodes `s` (lossy UTF-8).
fn decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |at: usize| b.get(at).and_then(|&d| (d as char).to_digit(16));
        if let (b'%', Some(high), Some(low)) = (b[i], hex(i + 1), hex(i + 2)) {
            bytes.push((high * 16 + low) as u8);
            i += 3;
            continue;
        }
        bytes.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Stores the index fetched at `now_ms` and returns the count of entries with a curve. An unchanged
/// index only updates its time; missing marks stay while their entry is listed; text without entries
/// (an error page) changes nothing.
pub fn store(c: &mut Connection, markdown: &str, now_ms: i64) -> rusqlite::Result<u32> {
    if !markdown.lines().any(|l| entry(l).is_some()) {
        return count(c);
    }
    let digest = format!("{:016x}-{}", fingerprint(markdown.as_bytes()), markdown.len());
    let tx = c.transaction()?;
    let same = kv(&tx, DIGEST_KEY)?.as_deref() == Some(digest.as_str()) && tx.query_row("SELECT count(*) FROM autoeq", [], |r| r.get::<_, u32>(0))? > 0;
    if !same {
        tx.execute("DELETE FROM autoeq", [])?;
        let mut st = tx.prepare("INSERT INTO autoeq(name, source, form, target, path) VALUES(?1, ?2, ?3, ?4, ?5)")?;
        for e in markdown.lines().filter_map(entry) {
            st.execute(params![e.name, e.source, e.form, e.target, e.path])?;
        }
        drop(st);
        tx.execute("DELETE FROM autoeq_missing WHERE path NOT IN (SELECT path FROM autoeq)", [])?;
    }
    set_kv(&tx, DIGEST_KEY, &digest)?;
    set_kv(&tx, FETCHED_KEY, &now_ms.to_string())?;
    tx.commit()?;
    count(c)
}

/// FNV-1a, to detect an unchanged index.
fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3))
}

fn kv(c: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    c.query_row("SELECT value FROM app_kv WHERE key=?1", [key], |r| r.get(0)).optional()
}

fn set_kv(c: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    c.execute("INSERT OR REPLACE INTO app_kv(key, value) VALUES(?1, ?2)", [key, value]).map(|_| ())
}

/// When the stored index was fetched; None if unknown.
pub fn fetched_ms(c: &Connection) -> rusqlite::Result<Option<i64>> {
    Ok(kv(c, FETCHED_KEY)?.and_then(|v| v.parse().ok()))
}

/// Whether to fetch the index automatically: download on, unmetered, and the index missing or
/// [`INDEX_STALE_MS`] old (a clock that went backwards counts as old).
pub fn index_due(auto: bool, metered: bool, stored: u32, fetched_ms: Option<i64>, now_ms: i64) -> bool {
    auto && !metered && (stored == 0 || fetched_ms.is_none_or(|t| now_ms < t || now_ms - t >= INDEX_STALE_MS))
}

/// Hides the entry at `path`, which has no usable curve.
pub fn mark_missing(c: &Connection, path: &str) -> rusqlite::Result<()> {
    c.execute("INSERT OR IGNORE INTO autoeq_missing(path) VALUES(?1)", [path]).map(|_| ())
}

/// Whether the preset reader gets filters from the text (directly or by fitting a graphic curve).
fn usable(text: &str) -> bool {
    !nori_settings::settings::parse_eq_preset(text.to_string()).bands.is_empty()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Curve {
    /// A preset or graphic curve's text.
    Found(String),
    /// Neither file exists with usable filters.
    Missing,
}

/// Fetches an entry's curve: one file, then the other when the first is 404/410 or unusable
/// (`graphic_first` for the graphic equalizer). Network and server errors are errors, never `Missing`.
pub async fn fetch_curve(transport: &dyn nori_net::transport::Transport, e: &AutoEqEntry, graphic_first: bool) -> Result<Curve, nori_net::transport::NetError> {
    let urls = if graphic_first { [graphic_url(e), preset_url(e)] } else { [preset_url(e), graphic_url(e)] };
    for url in urls {
        let r = transport.get(url, 0).await?;
        if r.status == 404 || r.status == 410 {
            continue;
        }
        if !(200..300).contains(&r.status) {
            return Err(nori_net::transport::NetError::Http { status: r.status });
        }
        let t = text(&r.body);
        if usable(&t) {
            return Ok(Curve::Found(t));
        }
    }
    Ok(Curve::Missing)
}

pub fn search(c: &Connection, query: &str, limit: u32) -> rusqlite::Result<Vec<AutoEqEntry>> {
    let typed = query.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let like = format!("%{}%", typed.replace(' ', "%"));
    let mut st = c.prepare_cached(
        // Shortest name first.
        "SELECT name, source, form, target, path FROM autoeq WHERE name LIKE ?1 ESCAPE '\\' AND path NOT IN (SELECT path FROM autoeq_missing) ORDER BY length(name), name LIMIT ?2",
    )?;
    let rows = st.query_map(params![like, limit], |r| {
        Ok(AutoEqEntry { name: r.get(0)?, source: r.get(1)?, form: r.get(2)?, target: r.get(3)?, path: r.get(4)? })
    })?;
    rows.collect()
}

/// Lowercase letters and digits only ("WH-1000XM5" is "wh1000xm5").
fn compact(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Words naming a kind of device, not a model; a name of only these is not looked up.
const GENERIC: &[&str] = &[
    "usb", "usbc", "c", "type", "typec", "audio", "device", "dac", "digital", "analog", "analogue", "headset", "headsets",
    "headphone", "headphones", "earphone", "earphones", "earbuds", "speaker", "speakers", "adapter", "adaptor", "jack",
    "to", "35mm", "3", "5mm", "the", "stereo", "wireless", "bluetooth", "le", "bt", "hifi", "hi", "fi", "out", "output",
];

/// The model part of a device name: "LE_WH-1000XM5" -> "WH-1000XM5", "Filip's AirPods Pro" -> "AirPods
/// Pro", "Galaxy Buds2 Pro (1A2B)" -> "Galaxy Buds2 Pro". None when no model is left.
pub(crate) fn device_query(device: &str) -> Option<String> {
    let mut name = device.trim().replace('_', " ");
    for prefix in ["LE ", "LE-", "BT ", "BT-"] {
        if name.len() > prefix.len() && name.is_char_boundary(prefix.len()) && name[..prefix.len()].eq_ignore_ascii_case(prefix) {
            name = name[prefix.len()..].to_string();
        }
    }
    // Drop an owner's name.
    for mark in ["'s ", "\u{2019}s "] {
        if let Some(i) = name.find(mark) {
            name = name[i + mark.len()..].to_string();
        }
    }
    // Drop pairing suffixes: "(1A2B)", "[LE]".
    while let Some(open) = name.rfind(['(', '[']) {
        if open == 0 {
            break;
        }
        name.truncate(open);
    }
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let meaningful = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !GENERIC.contains(&w.to_lowercase().as_str()))
        .map(str::len)
        .sum::<usize>();
    (meaningful >= 3 && compact(&name).len() >= 3).then_some(name)
}

/// AutoEQ's own preference among measurement sources.
fn source_rank(source: &str) -> u8 {
    match source {
        "oratory1990" => 0,
        "crinacle" => 1,
        "Rtings" => 2,
        _ => 3,
    }
}

/// The curves for a device name, best first. Ignores spacing and punctuation, prefers the exact model,
/// and requires names under 5 characters to match a whole model ("Buds" matches nothing).
pub fn matching(c: &Connection, device: &str, limit: u32) -> rusqlite::Result<Vec<AutoEqEntry>> {
    let Some(query) = device_query(device) else { return Ok(Vec::new()) };
    let q = compact(&query);
    let mut st = c.prepare_cached("SELECT name, source, form, target, path FROM autoeq WHERE path NOT IN (SELECT path FROM autoeq_missing)")?;
    let rows = st.query_map([], |r| Ok(AutoEqEntry { name: r.get(0)?, source: r.get(1)?, form: r.get(2)?, target: r.get(3)?, path: r.get(4)? }))?;
    let mut hits: Vec<(u8, usize, u8, AutoEqEntry)> = Vec::new();
    for e in rows {
        let e = e?;
        let name = compact(&e.name);
        let model = e.name.split_once(' ').map(|(_, m)| compact(m)).unwrap_or_default();
        let fit = if name == q || model == q {
            0
        } else if q.len() >= 5 && name.ends_with(&q) {
            1
        } else if q.len() >= 5 && name.contains(&q) {
            2
        } else {
            continue;
        };
        hits.push((fit, name.len(), source_rank(&e.source), e));
    }
    hits.sort_by(|a, b| (a.0, a.1, a.2, &a.3.name).cmp(&(b.0, b.1, b.2, &b.3.name)));
    Ok(hits.into_iter().take(limit as usize).map(|h| h.3).collect())
}

/// The number of entries with a curve.
pub fn count(c: &Connection) -> rusqlite::Result<u32> {
    c.query_row("SELECT count(*) FROM autoeq WHERE path NOT IN (SELECT path FROM autoeq_missing)", [], |r| r.get(0))
}

/// GETs `url` and decodes the body with [`text`].
pub async fn fetch_text(transport: &dyn nori_net::transport::Transport, url: String) -> Result<String, nori_net::transport::NetError> {
    Ok(text(&nori_net::transport::get(transport, url, 0).await?))
}

/// UTF-8 decoding as the JVM's `decodeToString` does it: lossy, except an encoded surrogate (ED A0..BF
/// plus a continuation byte) becomes one U+FFFD rather than several.
pub fn text(body: &[u8]) -> String {
    let surrogate = |b: &[u8]| b.len() >= 2 && b[0] == 0xED && (0xA0..=0xBF).contains(&b[1]);
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while !rest.is_empty() {
        if surrogate(rest) {
            out.push('\u{FFFD}');
            rest = &rest[if rest.get(2).is_some_and(|b| b & 0xC0 == 0x80) { 3 } else { 2 }..];
            continue;
        }
        let end = (1..rest.len()).find(|&i| surrogate(&rest[i..])).unwrap_or(rest.len());
        out.push_str(&String::from_utf8_lossy(&rest[..end]));
        rest = &rest[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "# Index\nnot an entry\n- [64 Audio U12t](./crinacle/711%20in-ear/64%20Audio%20U12t) by crinacle on 711\n- [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser%20HD%20600) by oratory1990 on Harman over-ear 2018\n- [Sennheiser HD 600 balanced](./Filk/over-ear/Sennheiser%20HD%20600%20balanced) by Filk\n";

    #[test]
    fn index_parses_and_searches() {
        let mut c = nori_db::open("", "t").unwrap();
        assert_eq!(store(&mut c, MD, 1).unwrap(), 3);
        let hits = search(&c, "hd 600", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "Sennheiser HD 600", "the shorter name ranks first");
        assert_eq!((hits[0].source.as_str(), hits[0].form.as_str(), hits[0].target.as_str()), ("oratory1990", "over-ear", "Harman over-ear 2018"));
        let encoded = entry("- [X](./crinacle/GRAS%2043AG-7%20over-ear/X) by crinacle on GRAS").unwrap();
        assert_eq!(encoded.form, "GRAS 43AG-7 over-ear");
        assert_eq!(search(&c, "u12t", 10).unwrap()[0].source, "crinacle");
        assert!(search(&c, "nothing here", 10).unwrap().is_empty());
        assert!(search(&c, "hd_600", 10).unwrap().is_empty(), "wildcards typed are letters");
        assert_eq!(entry("- [X](./crinacle/%a\u{e9}%zz%2/X) by crinacle").unwrap().form, "%a\u{e9}%zz%2", "not an escape");
        // Storing again replaces rather than duplicates.
        assert_eq!(store(&mut c, MD, 1).unwrap(), 3);
        assert_eq!(count(&c).unwrap(), 3);
    }

    #[test]
    fn device_query_strips_noise() {
        assert_eq!(device_query("LE_WH-1000XM5").as_deref(), Some("WH-1000XM5"));
        assert_eq!(device_query("Filip's AirPods Pro").as_deref(), Some("AirPods Pro"));
        assert_eq!(device_query("Filip\u{2019}s AirPods Pro").as_deref(), Some("AirPods Pro"));
        assert_eq!(device_query("Galaxy Buds2 Pro (1A2B)").as_deref(), Some("Galaxy Buds2 Pro"));
        for generic in ["USB Audio", "USB-C to 3.5mm Headphone Jack Adapter", "DAC", "device", "Headset", "BT", ""] {
            assert_eq!(device_query(generic), None, "{generic}");
        }
    }

    #[test]
    fn matching_ranks_models() {
        let mut c = nori_db::open("", "t").unwrap();
        let md = "- [Sony WH-1000XM5](./Rtings/over-ear/Sony%20WH-1000XM5) by Rtings\n\
- [Sony WH-1000XM5](./oratory1990/over-ear/Sony%20WH-1000XM5) by oratory1990\n\
- [Sony WH-1000XM5 (ANC off)](./crinacle/over-ear/Sony%20WH-1000XM5%20(ANC%20off)) by crinacle\n\
- [Apple AirPods Pro](./crinacle/in-ear/Apple%20AirPods%20Pro) by crinacle\n\
- [Apple AirPods Pro 2](./crinacle/in-ear/Apple%20AirPods%20Pro%202) by crinacle\n\
- [Samsung Galaxy Buds2 Pro](./Rtings/in-ear/Samsung%20Galaxy%20Buds2%20Pro) by Rtings\n\
- [Google Pixel Buds](./Rtings/in-ear/Google%20Pixel%20Buds) by Rtings\n";
        store(&mut c, md, 1).unwrap();
        let hit = |d: &str| matching(&c, d, 5).unwrap().first().map(|e| (e.name.clone(), e.source.clone()));
        assert_eq!(hit("LE_WH1000XM5"), Some(("Sony WH-1000XM5".into(), "oratory1990".into())), "spacing ignored, best rig first");
        assert_eq!(hit("Filip's AirPods Pro"), Some(("Apple AirPods Pro".into(), "crinacle".into())), "the exact model beats the Pro 2");
        assert_eq!(hit("Galaxy Buds2 Pro"), Some(("Samsung Galaxy Buds2 Pro".into(), "Rtings".into())));
        assert_eq!(hit("Buds"), None, "a short name has to be a whole model");
        assert_eq!(hit("USB Audio"), None);
    }

    #[test]
    fn preset_url_repeats_leaf() {
        let e = entry("- [64 Audio U12t](./crinacle/711%20in-ear/64%20Audio%20U12t) by crinacle on 711").unwrap();
        assert_eq!(preset_url(&e), "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/crinacle/711%20in-ear/64%20Audio%20U12t/64%20Audio%20U12t%20ParametricEQ.txt");
    }

    /// Lines as they are in AutoEQ's INDEX.md (2026-09-25), parentheses and all.
    const REAL: &str = "- [Sony WH-1000XM6](./Kuulokenurkka/over-ear/Sony%20WH-1000XM6) by Kuulokenurkka\n\
- [Sony WH-1000XM6](./Super%20Review/over-ear/Sony%20WH-1000XM6) by Super Review\n\
- [Sony WH-1000XM6 (analog cable)](./Super%20Review/over-ear/Sony%20WH-1000XM6%20(analog%20cable)) by Super Review\n\
- [1MORE Aero (ANC Off)](./HypetheSonics/GRAS%20RA0045%20in-ear/1MORE%20Aero%20(ANC%20Off)) by HypetheSonics on GRAS RA0045\n\
- [Apple AirPods Pro 2 (51dB + ANC)](./crinacle/711%20in-ear/Apple%20AirPods%20Pro%202%20(51dB%20+%20ANC)) by crinacle on 711\n";

    #[test]
    fn entry_keeps_parenthesised_paths() {
        let e: Vec<AutoEqEntry> = REAL.lines().filter_map(entry).collect();
        assert_eq!(e.len(), 5);
        assert_eq!(e[2].name, "Sony WH-1000XM6 (analog cable)");
        assert_eq!(e[2].path, "Super%20Review/over-ear/Sony%20WH-1000XM6%20(analog%20cable)", "not cut at the first parenthesis");
        assert_eq!(e[3].target, "GRAS RA0045");
        assert_eq!(e[4].path, "crinacle/711%20in-ear/Apple%20AirPods%20Pro%202%20(51dB%20+%20ANC)");
        assert_eq!(
            preset_url(&e[2]),
            "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/Super%20Review/over-ear/Sony%20WH-1000XM6%20(analog%20cable)/Sony%20WH-1000XM6%20(analog%20cable)%20ParametricEQ.txt"
        );
        assert_eq!(
            graphic_url(&e[2]),
            "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/Super%20Review/over-ear/Sony%20WH-1000XM6%20(analog%20cable)/Sony%20WH-1000XM6%20(analog%20cable)%20GraphicEQ.txt"
        );
        assert!(entry("- [Broken](./a/b/Broken%20(open").is_none(), "a link that never closes is not an entry");
    }

    #[test]
    fn index_due_rules() {
        let day = 24 * 3_600_000;
        let now = 100 * day;
        assert!(index_due(true, false, 0, None, now), "never fetched");
        assert!(index_due(true, false, 0, Some(now), now), "fetched but empty");
        assert!(index_due(true, false, 8000, None, now), "kept before its time was noted");
        assert!(!index_due(true, false, 8000, Some(now - 29 * day), now));
        assert!(index_due(true, false, 8000, Some(now - 30 * day), now));
        assert!(index_due(true, false, 8000, Some(now + day), now), "a clock that went backwards");
        assert!(!index_due(true, true, 0, None, now), "never on a metered network");
        assert!(!index_due(false, false, 0, None, now), "never with the download switched off");
    }

    #[test]
    fn missing_marks_follow_index() {
        let mut c = nori_db::open("", "t").unwrap();
        assert_eq!(fetched_ms(&c).unwrap(), None);
        assert_eq!(store(&mut c, REAL, 10).unwrap(), 5);
        assert_eq!(fetched_ms(&c).unwrap(), Some(10));
        mark_missing(&c, "Super%20Review/over-ear/Sony%20WH-1000XM6%20(analog%20cable)").unwrap();
        mark_missing(&c, "Super%20Review/over-ear/Sony%20WH-1000XM6%20(analog%20cable)").unwrap();
        assert_eq!(count(&c).unwrap(), 4);
        assert!(search(&c, "analog cable", 10).unwrap().is_empty());
        assert_eq!(search(&c, "WH-1000XM6", 10).unwrap().len(), 2);
        assert!(matching(&c, "WH-1000XM6 (analog cable)", 5).unwrap().iter().all(|e| !e.name.contains("analog")));
        // The same index again only moves its time on; the mark stays.
        assert_eq!(store(&mut c, REAL, 20).unwrap(), 4);
        assert_eq!(fetched_ms(&c).unwrap(), Some(20));
        // A new index that still lists it keeps it hidden; one that no longer does forgets the mark.
        let more = format!("{REAL}- [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser%20HD%20600) by oratory1990\n");
        assert_eq!(store(&mut c, &more, 30).unwrap(), 5);
        assert_eq!(store(&mut c, "<html>Rate limited</html>", 35).unwrap(), 5, "an error page changes nothing");
        assert_eq!(fetched_ms(&c).unwrap(), Some(30));
        let fewer: String = REAL.lines().filter(|l| !l.contains("analog")).map(|l| format!("{l}\n")).collect();
        assert_eq!(store(&mut c, &fewer, 40).unwrap(), 4);
        let left: u32 = c.query_row("SELECT count(*) FROM autoeq_missing", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
    }

    mod fetching {
        use super::*;
        use nori_net::transport::{Exchange, FailureKind, Transport, TransportError, TransportResponse};
        use std::future::Future;
        use std::pin::pin;
        use std::sync::Mutex;
        use std::task::{Context, Poll, Waker};

        const PARAMETRIC: &str = "Preamp: -6.3 dB\nFilter 1: ON LSC Fc 105 Hz Gain 6.5 dB Q 0.70\nFilter 2: ON PK Fc 125 Hz Gain -2.7 dB Q 0.55\n";
        const GRAPHIC: &str = include_str!("../../player/testdata/graphiceq/sony-wh-1000xm6-analog-cable.txt");

        /// Answers by the end of the address: (".. ParametricEQ.txt", status, body); anything else fails to connect.
        struct Web {
            pages: Vec<(&'static str, u16, &'static str)>,
            asked: Mutex<Vec<String>>,
        }

        #[async_trait::async_trait]
        impl Transport for Web {
            async fn get(&self, url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
                self.asked.lock().unwrap().push(url.clone());
                match self.pages.iter().find(|(end, _, _)| url.ends_with(end)) {
                    Some((_, status, body)) => Ok(TransportResponse { status: *status, body: body.as_bytes().to_vec() }),
                    None => Err(TransportError::Failed { kind: FailureKind::Connect, detail: None }),
                }
            }
            async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
                self.get(request.url, request.timeout_ms).await
            }
            fn address_changed(&self) {}
        }

        fn block<F: Future>(f: F) -> F::Output {
            let mut f = pin!(f);
            let mut cx = Context::from_waker(Waker::noop());
            loop {
                if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                    return v;
                }
            }
        }

        fn curve(pages: Vec<(&'static str, u16, &'static str)>) -> (Result<Curve, nori_net::transport::NetError>, usize) {
            let web = Web { pages, asked: Mutex::new(Vec::new()) };
            let e = entry(REAL.lines().nth(2).unwrap()).unwrap();
            let got = block(fetch_curve(&web, &e, false));
            let asked = web.asked.lock().unwrap().len();
            (got, asked)
        }

        #[test]
        fn graphic_first_order() {
            let web = Web { pages: vec![("ParametricEQ.txt", 200, PARAMETRIC), ("GraphicEQ.txt", 200, GRAPHIC)], asked: Mutex::new(Vec::new()) };
            let e = entry(REAL.lines().nth(2).unwrap()).unwrap();
            assert_eq!(block(fetch_curve(&web, &e, true)).unwrap(), Curve::Found(GRAPHIC.into()));
            let web = Web { pages: vec![("ParametricEQ.txt", 200, PARAMETRIC), ("GraphicEQ.txt", 404, "")], asked: Mutex::new(Vec::new()) };
            assert_eq!(block(fetch_curve(&web, &e, true)).unwrap(), Curve::Found(PARAMETRIC.into()), "and the filters where there is none");
        }

        #[test]
        fn parametric_first() {
            let (got, asked) = curve(vec![("ParametricEQ.txt", 200, PARAMETRIC), ("GraphicEQ.txt", 200, GRAPHIC)]);
            assert_eq!(got.unwrap(), Curve::Found(PARAMETRIC.into()));
            assert_eq!(asked, 1);
        }

        #[test]
        fn graphic_fallback() {
            for parametric in [(404, "404: Not Found"), (200, "Preamp: -3 dB\n")] {
                let (got, asked) = curve(vec![("ParametricEQ.txt", parametric.0, parametric.1), ("GraphicEQ.txt", 200, GRAPHIC)]);
                assert_eq!(got.unwrap(), Curve::Found(GRAPHIC.into()), "{parametric:?}");
                assert_eq!(asked, 2);
                // And the text read as a preset is ten fitted filters.
                let preset = nori_settings::settings::parse_eq_preset(GRAPHIC.into());
                assert_eq!(preset.bands.len(), 10);
                assert!(preset.preamp_db < 0.0);
            }
        }

        #[test]
        fn missing_vs_error() {
            let (got, _) = curve(vec![("ParametricEQ.txt", 404, "404: Not Found"), ("GraphicEQ.txt", 404, "404: Not Found")]);
            assert_eq!(got.unwrap(), Curve::Missing);
            let (got, _) = curve(vec![("ParametricEQ.txt", 200, "nothing"), ("GraphicEQ.txt", 200, "GraphicEQ: 20 1")]);
            assert_eq!(got.unwrap(), Curve::Missing, "files with nothing usable in them");
            assert!(curve(vec![]).0.is_err(), "no connection");
            assert!(curve(vec![("ParametricEQ.txt", 503, "busy")]).0.is_err(), "a server error");
            assert!(curve(vec![("ParametricEQ.txt", 404, "404: Not Found")]).0.is_err(), "the graphic curve could not be asked");
        }
    }
}
