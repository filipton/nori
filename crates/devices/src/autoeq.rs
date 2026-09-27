//! The AutoEQ headphone database, kept on the device. Its `results/INDEX.md` is one 850 KB request
//! listing every measurement; it is parsed here into a table so searching 8000+ headphones is a local
//! FTS query rather than a network call per keystroke. Only the index and the chosen preset are ever
//! fetched: the index once on an unmetered network when it is missing or a month old (`index_due`) or
//! when the user asks, a preset when one is chosen or offered.
//!
//! Every measurement has a parametric preset and a graphic curve beside it. A preset is read from
//! "ParametricEQ.txt"; where that is missing or has no filter in it the graphic curve is fitted instead
//! (`nori_player::eqfit`, through `parse_eq_preset`); an entry with neither is remembered as having no
//! curve (`mark_missing`) and is left out of every list from then on.

use nori_model::model::AutoEqEntry;
use rusqlite::{params, Connection};

pub const INDEX_URL: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/INDEX.md";
const RAW: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results";
/// The index is fetched again when it is this old (and the network is unmetered): AutoEQ adds headphones
/// every few weeks.
pub const INDEX_STALE_MS: i64 = 30 * 24 * 3_600_000;
/// Where the index's time and fingerprint are kept, in the app's own values (`app_kv`).
const FETCHED_KEY: &str = "autoeq.fetched";
const DIGEST_KEY: &str = "autoeq.digest";

/// `- [Name](./source/form/Name) by source on target`. The path's own parentheses are not escaped
/// ("Sony WH-1000XM6%20(analog%20cable)"), so the link ends at the parenthesis that balances its opening
/// one, not at the first one.
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
    // The path is percent-encoded; the labels a person reads are not.
    let source = decode(parts.next()?);
    let form = decode(parts.next().unwrap_or_default());
    let target = tail.split_once(" on ").map(|(_, t)| t.trim().to_string()).unwrap_or_default();
    Some(AutoEqEntry { name: name.trim().to_string(), source, form, target, path: path.to_string() })
}

/// Where the parametric preset of an entry lives. The file repeats the folder name, percent-encoded.
pub fn preset_url(e: &AutoEqEntry) -> String {
    file_url(e, "ParametricEQ")
}

/// Where the graphic curve of an entry lives, beside its parametric preset.
pub fn graphic_url(e: &AutoEqEntry) -> String {
    file_url(e, "GraphicEQ")
}

fn file_url(e: &AutoEqEntry, kind: &str) -> String {
    let leaf = e.path.rsplit('/').next().unwrap_or_default();
    format!("{RAW}/{}/{} {kind}.txt", e.path, decode(leaf)).replace(' ', "%20")
}

/// The index stores paths already percent-encoded; the file name inside needs the decoded form.
fn decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    // Decoded bytes are collected so multi-byte UTF-8 (e.g. %C3%A9) survives.
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                bytes.push(v);
                i += 3;
                continue;
            }
        }
        bytes.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The index in `markdown` as the local table, fetched at `now_ms`; how many headphones it lists that have
/// a curve. An index the same as the one kept only has its time moved on. Entries known to have no curve
/// stay known while the index lists them. Text with no entry in it at all (an error page) changes nothing.
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

/// FNV-1a over the index, to tell an unchanged one from a new one without keeping it.
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

/// When the index kept on the device was fetched; none when it never was (or was kept before this was
/// noted, which is as good as never).
pub fn fetched_ms(c: &Connection) -> rusqlite::Result<Option<i64>> {
    Ok(kv(c, FETCHED_KEY)?.and_then(|v| v.parse().ok()))
}

/// Whether the index should be fetched now without the user asking: the automatic download is on, the
/// network is unmetered, and the index is missing or at least `INDEX_STALE_MS` old. A clock that went
/// backwards counts as old.
pub fn index_due(auto: bool, metered: bool, stored: u32, fetched_ms: Option<i64>, now_ms: i64) -> bool {
    auto && !metered && (stored == 0 || fetched_ms.is_none_or(|t| now_ms < t || now_ms - t >= INDEX_STALE_MS))
}

/// The entry at `path` has no curve in either format: it is left out of every list from now on.
pub fn mark_missing(c: &Connection, path: &str) -> rusqlite::Result<()> {
    c.execute("INSERT OR IGNORE INTO autoeq_missing(path) VALUES(?1)", [path]).map(|_| ())
}

/// A curve in text the preset reader takes: a parametric preset, or a graphic curve it fits.
fn usable(text: &str) -> bool {
    !nori_settings::settings::parse_eq_preset(text.to_string()).bands.is_empty()
}

/// What asking AutoEQ for an entry's curve came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Curve {
    /// The text of a preset with filters in it, or of a graphic curve (fitted when it is read).
    Found(String),
    /// Neither file is there with a usable filter in it.
    Missing,
}

/// An entry's curve through the platform's transport: its "ParametricEQ.txt", or where that is missing
/// or has no filter, its "GraphicEQ.txt". A network failure or a server error is an error, never
/// "missing": only an answer that the file is not there (404, 410) or has nothing usable in it is.
pub async fn fetch_curve(transport: &dyn nori_net::transport::Transport, e: &AutoEqEntry, graphic_first: bool) -> Result<Curve, nori_net::transport::NetError> {
    // The graphic equalizer is fitted to AutoEQ's own dense curve; the parametric one takes its filters.
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
    let like = format!("%{}%", query.trim().replace(' ', "%"));
    let mut st = c.prepare_cached(
        // Shortest name first, so "HD 600" beats "HD 600 (with pads)" when both match.
        "SELECT name, source, form, target, path FROM autoeq WHERE name LIKE ?1 ESCAPE '\\' AND path NOT IN (SELECT path FROM autoeq_missing) ORDER BY length(name), name LIMIT ?2",
    )?;
    let rows = st.query_map(params![like, limit], |r| {
        Ok(AutoEqEntry { name: r.get(0)?, source: r.get(1)?, form: r.get(2)?, target: r.get(3)?, path: r.get(4)? })
    })?;
    rows.collect()
}

/// Letters and digits only, lowercased: "WH-1000XM5", "wh1000xm5" and "WH 1000 XM5" are one name.
fn compact(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Words that say what kind of thing a device is, not which one. A name made only of these ("USB Audio",
/// "USB-C to 3.5mm Headphone Jack Adapter", "Headset") cannot be looked up.
const GENERIC: &[&str] = &[
    "usb", "usbc", "c", "type", "typec", "audio", "device", "dac", "digital", "analog", "analogue", "headset", "headsets",
    "headphone", "headphones", "earphone", "earphones", "earbuds", "speaker", "speakers", "adapter", "adaptor", "jack",
    "to", "35mm", "3", "5mm", "the", "stereo", "wireless", "bluetooth", "le", "bt", "hifi", "hi", "fi", "out", "output",
];

/// The part of a device's own name that can be looked up: "LE_WH-1000XM5" -> "WH-1000XM5", "Filip's
/// AirPods Pro" -> "AirPods Pro", "Galaxy Buds2 Pro (1A2B)" -> "Galaxy Buds2 Pro". None when nothing is
/// left that names a model.
pub fn device_query(device: &str) -> Option<String> {
    let mut name = device.trim().replace('_', " ");
    for prefix in ["LE ", "LE-", "BT ", "BT-"] {
        if name.len() > prefix.len() && name.is_char_boundary(prefix.len()) && name[..prefix.len()].eq_ignore_ascii_case(prefix) {
            name = name[prefix.len()..].to_string();
        }
    }
    // "Filip's AirPods Pro": the owner is not part of the model.
    for mark in ["'s ", "\u{2019}s "] {
        if let Some(i) = name.find(mark) {
            name = name[i + mark.len()..].to_string();
        }
    }
    // A pairing suffix: "(1A2B)", "[LE]".
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

/// Measurements from these rigs are the ones AutoEQ itself recommends first.
fn source_rank(source: &str) -> u8 {
    match source {
        "oratory1990" => 0,
        "crinacle" => 1,
        "Rtings" => 2,
        _ => 3,
    }
}

/// The curves that are this device, best first. Unlike [search] this ignores spacing and punctuation
/// ("WH1000XM5" finds "Sony WH-1000XM5") and prefers the exact model over a longer one that contains it.
/// A short name must be a whole model name (with or without its brand): "Buds" alone matches nothing
/// rather than every Galaxy, Pixel and Nothing earbud.
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

/// How many headphones the list offers: those known to have no curve are not counted.
pub fn count(c: &Connection) -> rusqlite::Result<u32> {
    c.query_row("SELECT count(*) FROM autoeq WHERE path NOT IN (SELECT path FROM autoeq_missing)", [], |r| r.get(0))
}

/// The AutoEQ index or one of its presets, read from `url` (`Core::autoeq_index_url`,
/// `Core::autoeq_preset_url`) through the platform's transport: an error status with nothing in it
/// fails, and the body is read as UTF-8 with anything malformed replaced, the way the JVM reads it
/// (`text`). `Client::autoeq_update` keeps the index with it; a preset comes through [fetch_curve].
///
/// Twin of `Http.get(..).decodeToString()`, as the app fetched both before the core did.
pub async fn fetch_text(transport: &dyn nori_net::transport::Transport, url: String) -> Result<String, nori_net::transport::NetError> {
    Ok(text(&nori_net::transport::get(transport, url, 0).await?))
}

/// A body as text, the way Kotlin's `decodeToString` reads it on the JVM: UTF-8, each bad stretch one
/// U+FFFD by UTF-8's own rule, except a surrogate written out (ED A0..BF, with its last byte when that is
/// a continuation byte), which the JVM takes as one bad character where the rule makes two or three.
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
        // Storing again replaces rather than duplicates.
        assert_eq!(store(&mut c, MD, 1).unwrap(), 3);
        assert_eq!(count(&c).unwrap(), 3);
    }

    #[test]
    fn device_names_are_cleaned_before_lookup() {
        assert_eq!(device_query("LE_WH-1000XM5").as_deref(), Some("WH-1000XM5"));
        assert_eq!(device_query("Filip's AirPods Pro").as_deref(), Some("AirPods Pro"));
        assert_eq!(device_query("Filip\u{2019}s AirPods Pro").as_deref(), Some("AirPods Pro"));
        assert_eq!(device_query("Galaxy Buds2 Pro (1A2B)").as_deref(), Some("Galaxy Buds2 Pro"));
        for generic in ["USB Audio", "USB-C to 3.5mm Headphone Jack Adapter", "DAC", "device", "Headset", "BT", ""] {
            assert_eq!(device_query(generic), None, "{generic}");
        }
    }

    #[test]
    fn devices_find_their_curve() {
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
    fn preset_url_repeats_the_decoded_leaf() {
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
    fn a_path_with_parentheses_is_read_whole() {
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
    fn the_index_is_due_when_missing_or_a_month_old_on_an_unmetered_network() {
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
    fn entries_without_a_curve_are_left_out_until_the_index_drops_them() {
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
        fn the_graphic_equalizer_takes_the_graphic_curve_first() {
            let web = Web { pages: vec![("ParametricEQ.txt", 200, PARAMETRIC), ("GraphicEQ.txt", 200, GRAPHIC)], asked: Mutex::new(Vec::new()) };
            let e = entry(REAL.lines().nth(2).unwrap()).unwrap();
            assert_eq!(block(fetch_curve(&web, &e, true)).unwrap(), Curve::Found(GRAPHIC.into()));
            let web = Web { pages: vec![("ParametricEQ.txt", 200, PARAMETRIC), ("GraphicEQ.txt", 404, "")], asked: Mutex::new(Vec::new()) };
            assert_eq!(block(fetch_curve(&web, &e, true)).unwrap(), Curve::Found(PARAMETRIC.into()), "and the filters where there is none");
        }

        #[test]
        fn the_parametric_preset_is_taken_first() {
            let (got, asked) = curve(vec![("ParametricEQ.txt", 200, PARAMETRIC), ("GraphicEQ.txt", 200, GRAPHIC)]);
            assert_eq!(got.unwrap(), Curve::Found(PARAMETRIC.into()));
            assert_eq!(asked, 1);
        }

        #[test]
        fn a_graphic_curve_stands_in_for_a_missing_or_empty_preset() {
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
        fn neither_file_means_missing_but_a_failure_never_does() {
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
