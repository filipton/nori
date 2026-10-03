//! Smart playlists: a rule tree evaluated against the local index and the play statistics.
//!
//! uniffi records cannot nest themselves, so a definition crosses the FFI as a JSON string:
//!
//! ```json
//! {
//!   "match": { "all": true, "rules": [
//!     { "field": "genre", "op": "is", "value": "Rock" },
//!     { "all": false, "rules": [
//!       { "field": "year", "op": "between", "value": [1970, 1979] },
//!       { "field": "starred", "op": "isTrue" } ] } ] },
//!   "sort": { "field": "playCount", "descending": true },
//!   "limit": 50,
//!   "limitMs": 3600000
//! }
//! ```
//!
//! - `match` (optional, default: every song) is a group. A group is `{ "all": bool, "rules": [node..] }`:
//!   `all` true (the default) means every node must match, false means any. A node is a rule or another
//!   group, up to 8 levels deep. An empty group matches everything when `all`, nothing otherwise.
//! - A rule is `{ "field", "op", "value" }`. What `value` must be depends on the field's type:
//!
//!   | type   | fields | operators and their value |
//!   |--------|--------|---------------------------|
//!   | text   | `title` `album` `artist` `genre` `suffix` | `is` `isNot` `contains` `notContains` `startsWith` `endsWith`: a string, compared without regard to case |
//!   | number | `year` `duration` (s) `track` `discNumber` `bitRate` (kbps) `sampleRate` (Hz) `bitDepth` `size` (bytes) `userRating` (0-5) `playCount` `skipCount` (this device) `serverPlayCount` | `is` `isNot` `greater` `less`: an integer; `between`: `[low, high]`, inclusive |
//!   | date   | `lastPlayed` (this device) `added` (server) | `withinDays` `notWithinDays`: a number of days; `greater` (after) `less` (before): `"YYYY-MM-DD"` or `"YYYY-MM-DDTHH:MM:SS"`, UTC; `between`: two of those, a bare end date includes its whole day. A song never played, or with no added date, is "not within" and "before" everything |
//!   | flag   | `starred` `isDownloaded` `excludedFromMixes` | `isTrue` `isFalse`: no value |
//!
//! - `sort` (optional, default: index order): `field` is any text, number or date field, `starred`, or
//!   `"random"`; `descending` (default false); `seed` (default 0) makes `random` a stable order, so
//!   paging works and a new seed is a reshuffle.
//! - `limit` (optional) caps the number of songs, `limitMs` (optional) the total duration: the playlist
//!   ends before the first song that would exceed it. 0 or absent means no cap.
//!
//! Unknown keys, fields and operators are errors, with the path of the offending node in the message.
//!
//! Evaluation. The tree becomes one SQL condition over `items` joined with `song_stats`, and the
//! statement returns rowids only, in order, so neither the sorter nor Rust ever holds 100k JSON
//! documents; the JSON of the requested page is then fetched by rowid. Two things SQL cannot do are
//! finished in Rust while streaming those rowids: text rules with a non-ASCII value (SQLite folds
//! case for ASCII only; such a rule is relaxed to "true" in SQL and every candidate re-checked here),
//! and the `limitMs` running total.

use std::collections::HashSet;

use nori_model::model::*;
use nori_model::{CoreError, Result, SmartProblem};
use rusqlite::{types::Value as Sql, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::browse::DAY_MS;
use crate::mixes;

/// The editor's flat form of a definition.
pub mod draft;

const MAX_DEPTH: usize = 8;

// ---- the definition ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Num,
    /// Epoch milliseconds from `song_stats`; 0 is never.
    DateMs,
    /// ISO-8601 string from the song JSON; compares as text. Missing is ''.
    DateIso,
    Flag,
}

/// What a rule looks at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Field {
    Title,
    Album,
    Artist,
    Genre,
    Suffix,
    Year,
    Duration,
    Track,
    DiscNumber,
    BitRate,
    SampleRate,
    BitDepth,
    Size,
    UserRating,
    PlayCount,
    SkipCount,
    ServerPlayCount,
    LastPlayed,
    Added,
    Starred,
    IsDownloaded,
    ExcludedFromMixes,
}

use Field::*;

const FIELDS: [Field; 22] = [
    Title, Album, Artist, Genre, Suffix, Year, Duration, Track, DiscNumber, BitRate, SampleRate, BitDepth, Size, UserRating, PlayCount, SkipCount, ServerPlayCount, LastPlayed, Added, Starred,
    IsDownloaded, ExcludedFromMixes,
];

impl Field {
    /// (name in the JSON, type, SQL expression). Text expressions are nullable; the rest are not.
    fn def(self) -> (&'static str, Kind, &'static str) {
        match self {
            Title => ("title", Kind::Text, "json_extract(i.json,'$.title')"),
            Album => ("album", Kind::Text, "json_extract(i.json,'$.album')"),
            Artist => ("artist", Kind::Text, "json_extract(i.json,'$.artist')"),
            Genre => ("genre", Kind::Text, "json_extract(i.json,'$.genre')"),
            Suffix => ("suffix", Kind::Text, "json_extract(i.json,'$.suffix')"),
            Year => ("year", Kind::Num, "json_extract(i.json,'$.year')"),
            Duration => ("duration", Kind::Num, "json_extract(i.json,'$.duration')"),
            Track => ("track", Kind::Num, "json_extract(i.json,'$.track')"),
            DiscNumber => ("discNumber", Kind::Num, "json_extract(i.json,'$.discNumber')"),
            BitRate => ("bitRate", Kind::Num, "json_extract(i.json,'$.bitRate')"),
            SampleRate => ("sampleRate", Kind::Num, "json_extract(i.json,'$.samplingRate')"),
            BitDepth => ("bitDepth", Kind::Num, "json_extract(i.json,'$.bitDepth')"),
            Size => ("size", Kind::Num, "json_extract(i.json,'$.size')"),
            UserRating => ("userRating", Kind::Num, "json_extract(i.json,'$.userRating')"),
            PlayCount => ("playCount", Kind::Num, "coalesce(s.plays,0)"),
            SkipCount => ("skipCount", Kind::Num, "coalesce(s.skips,0)"),
            ServerPlayCount => ("serverPlayCount", Kind::Num, "coalesce(json_extract(i.json,'$.playCount'),0)"),
            LastPlayed => ("lastPlayed", Kind::DateMs, "coalesce(s.last_played_ms,0)"),
            Added => ("added", Kind::DateIso, "coalesce(json_extract(i.json,'$.created'),'')"),
            Starred => ("starred", Kind::Flag, "json_extract(i.json,'$.starred')"),
            IsDownloaded => ("isDownloaded", Kind::Flag, ""),
            ExcludedFromMixes => ("excludedFromMixes", Kind::Flag, ""),
        }
    }

    fn name(self) -> &'static str {
        self.def().0
    }

    fn kind(self) -> Kind {
        self.def().1
    }

    /// The `song_stats` column behind the field.
    fn stats_column(self) -> Option<&'static str> {
        match self {
            PlayCount => Some("s.plays"),
            SkipCount => Some("s.skips"),
            LastPlayed => Some("s.last_played_ms"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Is,
    IsNot,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    Greater,
    Less,
    Between,
    WithinDays,
    NotWithinDays,
    IsTrue,
    IsFalse,
}

const OPS: [(&str, Op); 13] = [
    ("is", Op::Is),
    ("isNot", Op::IsNot),
    ("contains", Op::Contains),
    ("notContains", Op::NotContains),
    ("startsWith", Op::StartsWith),
    ("endsWith", Op::EndsWith),
    ("greater", Op::Greater),
    ("less", Op::Less),
    ("between", Op::Between),
    ("withinDays", Op::WithinDays),
    ("notWithinDays", Op::NotWithinDays),
    ("isTrue", Op::IsTrue),
    ("isFalse", Op::IsFalse),
];

fn ops_of(kind: Kind) -> &'static [Op] {
    match kind {
        Kind::Text => &[Op::Is, Op::IsNot, Op::Contains, Op::NotContains, Op::StartsWith, Op::EndsWith],
        Kind::Num => &[Op::Is, Op::IsNot, Op::Greater, Op::Less, Op::Between],
        Kind::DateMs | Kind::DateIso => &[Op::WithinDays, Op::NotWithinDays, Op::Greater, Op::Less, Op::Between],
        Kind::Flag => &[Op::IsTrue, Op::IsFalse],
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Val {
    None,
    Text(String),
    Num(i64),
    Range(i64, i64),
    /// A point in time, both as the text `added` compares with and the milliseconds `lastPlayed` compares with.
    Date(String, i64),
    DateRange((String, i64), (String, i64)),
}

/// One rule of a definition: a field, how it compares and with what.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    field: Field,
    op: Op,
    value: Val,
}

impl Rule {
    /// True when a song without a `song_stats` row can never pass ("played more than 0 times", "played
    /// this month"). The rule then reads the bare column, where NULL fails it, and a playlist that
    /// requires such a rule is evaluated from the small stats table instead of from the whole index.
    fn needs_stats(&self) -> bool {
        self.field.stats_column().is_some()
            && match (self.op, &self.value) {
                (Op::WithinDays, _) => true,
                (Op::Is, Val::Num(v)) => *v > 0,
                (Op::Greater, Val::Num(v)) => *v >= 0,
                (Op::Between, Val::Range(low, _)) => *low > 0,
                (Op::Greater, Val::Date(_, ms)) => *ms >= 0,
                (Op::Between, Val::DateRange(from, _)) => from.1 > 0,
                _ => false,
            }
    }
}

/// A definition's rule tree: a rule, or a group of them joined by all or any.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Group { all: bool, rules: Vec<Node> },
    Rule(Rule),
}

impl Node {
    /// Whether a rule anywhere in here asks about `field`.
    pub fn asks(&self, field: Field) -> bool {
        match self {
            Node::Rule(r) => r.field == field,
            Node::Group { rules, .. } => rules.iter().any(|n| n.asks(field)),
        }
    }

    fn needs_stats(&self) -> bool {
        match self {
            Node::Rule(r) => r.needs_stats(),
            Node::Group { all: true, rules } => rules.iter().any(Node::needs_stats),
            Node::Group { all: false, rules } => !rules.is_empty() && rules.iter().all(Node::needs_stats),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Sort {
    Index,
    Random(u64),
    By(Field, bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Def {
    pub root: Node,
    sort: Sort,
    pub limit: Option<u32>,
    pub limit_ms: Option<i64>,
}

// ---- parsing, with errors saying where and what ------------------------------

type Parsed<T> = Result<T>;

fn bad<T>(path: &str, problem: SmartProblem) -> Parsed<T> {
    Err(CoreError::Smart { path: path.to_string(), problem })
}

fn only_keys(o: &Map<String, Value>, allowed: &[&str], path: &str) -> Parsed<()> {
    match o.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => bad(&format!("{path}.{k}"), SmartProblem::UnknownKey),
        None => Ok(()),
    }
}

fn integer(v: &Value, path: &str) -> Parsed<i64> {
    match v.as_i64().or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f.round() as i64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())) {
        Some(n) => Ok(n),
        None => bad(path, SmartProblem::NotNumber),
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// The calendar date and the second of the day of `ms` since 1970, both as UTC counts them: the year,
/// the month (1-12), the day (1-31) and the seconds since midnight.
pub fn civil_from_ms(ms: i64) -> (i64, i64, i64, i64) {
    let (days, rest) = (ms.div_euclid(DAY_MS), ms.rem_euclid(DAY_MS) / 1000);
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + (m <= 2) as i64;
    (y, m, d, rest)
}

/// "YYYY-MM-DDTHH:MM:SS", the prefix every ISO-8601 UTC timestamp shares, so it compares against them as text.
fn iso_from_ms(ms: i64) -> String {
    let (y, m, d, rest) = civil_from_ms(ms);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", rest / 3600, rest / 60 % 60, rest % 60)
}

/// A date like "2024-05-31" or "2024-05-31T18:00:00"; `end` makes a bare date mean the last instant of
/// its day.
fn date(v: &Value, end: bool, path: &str) -> Parsed<(String, i64)> {
    let not_date = || bad(path, SmartProblem::NotDate);
    let Some(s) = v.as_str().map(str::trim) else { return not_date() };
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|p| p.parse::<i64>().ok());
    let b = s.as_bytes();
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else { return not_date() };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return not_date();
    }
    let day = days_from_civil(y, m, d) * DAY_MS;
    if b.len() == 10 {
        // Second 60 sorts after every timestamp of the day whatever its precision ("...59Z", "...59.999Z").
        return Ok(if end { (format!("{s}T23:59:60"), day + DAY_MS - 1) } else { (s.to_string(), day) });
    }
    let (Some(h), Some(mi), Some(sec)) = (num(11..13), num(14..16), num(17..19)) else { return not_date() };
    if b[10] != b'T' || h > 23 || mi > 59 || sec > 60 {
        return not_date();
    }
    Ok((s[..19].to_string(), day + ((h * 60 + mi) * 60 + sec) * 1000))
}

fn rule(o: &Map<String, Value>, path: &str) -> Parsed<Rule> {
    only_keys(o, &["field", "op", "value"], path)?;
    let fpath = format!("{path}.field");
    let Some(name) = o.get("field").and_then(Value::as_str) else { return bad(&fpath, SmartProblem::NoField) };
    let Some(&field) = FIELDS.iter().find(|f| f.name() == name) else { return bad(&fpath, SmartProblem::UnknownField) };
    let opath = format!("{path}.op");
    let Some(op_text) = o.get("op").and_then(Value::as_str) else { return bad(&opath, SmartProblem::NoOperator) };
    let Some(op) = OPS.iter().find(|(n, _)| *n == op_text).map(|(_, o)| *o).filter(|o| ops_of(field.kind()).contains(o)) else {
        return bad(&opath, SmartProblem::WrongOperator);
    };
    let vpath = format!("{path}.value");
    let v = o.get("value").filter(|v| !v.is_null());
    let need = || v.map_or_else(|| bad(&vpath, SmartProblem::NoValue), Ok);
    let pair = || -> Parsed<(&Value, &Value)> {
        match need()?.as_array().map(Vec::as_slice) {
            Some([a, b]) => Ok((a, b)),
            _ => bad(&vpath, SmartProblem::NoValue),
        }
    };
    let at = |i: usize| format!("{vpath}[{i}]");
    let is_date = matches!(field.kind(), Kind::DateMs | Kind::DateIso);
    let value = match op {
        Op::IsTrue | Op::IsFalse if v.is_some() => return bad(&vpath, SmartProblem::TakesNoValue),
        Op::IsTrue | Op::IsFalse => Val::None,
        Op::WithinDays | Op::NotWithinDays => match integer(need()?, &vpath)? {
            n if (0..=100_000).contains(&n) => Val::Num(n),
            _ => return bad(&vpath, SmartProblem::DaysOutOfRange),
        },
        Op::Between if is_date => {
            let (a, b) = pair()?;
            let (a, b) = (date(a, false, &at(0))?, date(b, true, &at(1))?);
            if a.1 > b.1 {
                return bad(&vpath, SmartProblem::Backwards);
            }
            Val::DateRange(a, b)
        }
        Op::Between => {
            let (a, b) = pair()?;
            let (a, b) = (integer(a, &at(0))?, integer(b, &at(1))?);
            if a > b {
                return bad(&vpath, SmartProblem::Backwards);
            }
            Val::Range(a, b)
        }
        // "after" a bare date means after that day is over
        Op::Greater | Op::Less if is_date => {
            let (text, ms) = date(need()?, op == Op::Greater, &vpath)?;
            Val::Date(text, ms)
        }
        _ if field.kind() == Kind::Num => Val::Num(integer(need()?, &vpath)?),
        _ => match need()?.as_str() {
            Some(s) => Val::Text(s.to_string()),
            None => return bad(&vpath, SmartProblem::NotText),
        },
    };
    Ok(Rule { field, op, value })
}

fn node(v: &Value, path: &str, depth: usize) -> Parsed<Node> {
    let Some(o) = v.as_object() else { return bad(path, SmartProblem::NotObject) };
    if o.contains_key("field") {
        return rule(o, path).map(Node::Rule);
    }
    if !o.contains_key("rules") {
        return bad(path, SmartProblem::NotRuleOrGroup);
    }
    if depth >= MAX_DEPTH {
        return bad(path, SmartProblem::TooDeep);
    }
    only_keys(o, &["all", "rules"], path)?;
    let all = match o.get("all").map(Value::as_bool) {
        None => true,
        Some(Some(a)) => a,
        Some(None) => return bad(&format!("{path}.all"), SmartProblem::NotFlag),
    };
    let Some(list) = o["rules"].as_array() else { return bad(&format!("{path}.rules"), SmartProblem::NotList) };
    let rules = list.iter().enumerate().map(|(i, r)| node(r, &format!("{path}.rules[{i}]"), depth + 1)).collect::<Parsed<_>>()?;
    Ok(Node::Group { all, rules })
}

/// A definition read from its JSON; an error says where and what is wrong with it.
pub fn parse(text: &str) -> Parsed<Def> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return bad("", SmartProblem::NotJson) };
    let Some(o) = v.as_object() else { return bad("", SmartProblem::NotObject) };
    only_keys(o, &["match", "sort", "limit", "limitMs"], "")?;
    let root = match o.get("match").filter(|m| !m.is_null()) {
        None => Node::Group { all: true, rules: vec![] },
        Some(m) if m.get("field").is_some() => return bad("match", SmartProblem::SingleRule),
        Some(m) => node(m, "match", 1)?,
    };
    let sort = match o.get("sort").filter(|s| !s.is_null()) {
        None => Sort::Index,
        Some(s) => {
            let Some(so) = s.as_object() else { return bad("sort", SmartProblem::NotObject) };
            only_keys(so, &["field", "descending", "seed"], "sort")?;
            let descending = match so.get("descending").map(Value::as_bool) {
                None => false,
                Some(Some(d)) => d,
                Some(None) => return bad("sort.descending", SmartProblem::NotFlag),
            };
            match so.get("field").and_then(Value::as_str) {
                Some("random") => Sort::Random(match so.get("seed") {
                    None => 0,
                    Some(s) => match s.as_u64().or_else(|| s.as_i64().map(|i| i as u64)) {
                        Some(seed) => seed,
                        None => return bad("sort.seed", SmartProblem::NotNumber),
                    },
                }),
                Some(name) => match FIELDS.iter().find(|f| f.name() == name && (f.kind() != Kind::Flag || **f == Starred)) {
                    Some(f) => Sort::By(*f, descending),
                    None => return bad("sort.field", SmartProblem::CannotSort),
                },
                None => return bad("sort.field", SmartProblem::NoField),
            }
        }
    };
    let cap = |key: &str| -> Parsed<Option<i64>> {
        match o.get(key).filter(|l| !l.is_null()) {
            None => Ok(None),
            Some(l) => match integer(l, key)? {
                n if n < 0 => bad(key, SmartProblem::Negative),
                0 => Ok(None),
                n => Ok(Some(n)),
            },
        }
    };
    Ok(Def { root, sort, limit: cap("limit")?.map(|n| n.min(u32::MAX as i64) as u32), limit_ms: cap("limitMs")? })
}
// ---- to SQL ----------------------------------------------------------------

/// A definition on its way to SQL: the arguments bound so far and what the rules are evaluated against.
pub struct Compiler<'a> {
    pub args: Vec<Sql>,
    pub downloaded: &'a [String],
    pub downloaded_arg: Option<usize>,
    pub now_ms: i64,
    /// A rule was relaxed to "true": SQL yields a superset (there is no NOT node, so relaxing a leaf can
    /// only add rows) and `matches` has the last word.
    pub needs_rust: bool,
}

impl Compiler<'_> {
    fn arg(&mut self, v: Sql) -> String {
        self.args.push(v);
        format!("?{}", self.args.len())
    }

    pub fn node(&mut self, n: &Node) -> String {
        match n {
            Node::Rule(r) => self.rule(r),
            Node::Group { all, rules } if rules.is_empty() => if *all { "1" } else { "0" }.to_string(),
            Node::Group { all, rules } => {
                let parts: Vec<String> = rules.iter().map(|r| self.node(r)).collect();
                format!("({})", parts.join(if *all { " AND " } else { " OR " }))
            }
        }
    }

    fn rule(&mut self, r: &Rule) -> String {
        let (_, kind, x) = r.field.def();
        let x = r.field.stats_column().filter(|_| r.needs_stats()).unwrap_or(x);
        match (&r.value, kind) {
            (Val::Text(v), _) => {
                if !v.is_ascii() {
                    self.needs_rust = true;
                    return "1".into();
                }
                let like = |v: &str| v.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
                match r.op {
                    // The bare expression so that the genre index applies; NULL can only equal the empty string.
                    Op::Is if !v.is_empty() => format!("{x}={} COLLATE NOCASE", self.arg(Sql::Text(v.clone()))),
                    Op::Is => format!("coalesce({x},'')=''"),
                    Op::IsNot => format!("coalesce({x},'')<>{} COLLATE NOCASE", self.arg(Sql::Text(v.clone()))),
                    op => {
                        let pattern = match op {
                            Op::StartsWith => format!("{}%", like(v)),
                            Op::EndsWith => format!("%{}", like(v)),
                            _ => format!("%{}%", like(v)),
                        };
                        let not = if op == Op::NotContains { "NOT " } else { "" };
                        format!("coalesce({x},'') {not}LIKE {} ESCAPE '\\'", self.arg(Sql::Text(pattern)))
                    }
                }
            }
            (Val::Num(days), Kind::DateMs | Kind::DateIso) => {
                let cutoff = self.now_ms - days * DAY_MS;
                let within = r.op == Op::WithinDays;
                // Never played is 0 and must stay "not within" however far back the cutoff goes.
                let p = if kind == Kind::DateMs { self.arg(Sql::Integer(cutoff.max(1))) } else { self.arg(Sql::Text(iso_from_ms(cutoff))) };
                format!("{x}{}{p}", if within { ">=" } else { "<" })
            }
            (Val::Num(v), _) => {
                let op = match r.op {
                    Op::Is => "=",
                    Op::IsNot => "<>",
                    Op::Greater => ">",
                    _ => "<",
                };
                format!("{x}{op}{}", self.arg(Sql::Integer(*v)))
            }
            (Val::Range(a, b), _) => {
                format!("{x} BETWEEN {} AND {}", self.arg(Sql::Integer(*a)), self.arg(Sql::Integer(*b)))
            }
            (Val::Date(text, ms), _) => {
                let p = if kind == Kind::DateMs { self.arg(Sql::Integer(*ms)) } else { self.arg(Sql::Text(text.clone())) };
                format!("{x}{}{p}", if r.op == Op::Greater { ">" } else { "<" })
            }
            (Val::DateRange(a, b), _) => {
                let (a, b) = if kind == Kind::DateMs { (Sql::Integer(a.1), Sql::Integer(b.1)) } else { (Sql::Text(a.0.clone()), Sql::Text(b.0.clone())) };
                format!("{x} BETWEEN {} AND {}", self.arg(a), self.arg(b))
            }
            (Val::None, _) => {
                let yes = r.op == Op::IsTrue;
                match r.field {
                    IsDownloaded => {
                        let n = match self.downloaded_arg {
                            Some(n) => n,
                            None => {
                                self.args.push(Sql::Text(serde_json::to_string(self.downloaded).unwrap_or_else(|_| "[]".into())));
                                *self.downloaded_arg.insert(self.args.len())
                            }
                        };
                        format!("i.id {}IN (SELECT value FROM json_each(?{n}))", if yes { "" } else { "NOT " })
                    }
                    ExcludedFromMixes => format!("i.id {}IN (SELECT song_id FROM mix_excluded WHERE server=sid())", if yes { "" } else { "NOT " }),
                    // Written exactly like the partial index on starred songs.
                    _ if yes => format!("{x}=1"),
                    _ => format!("coalesce({x},0)<>1"),
                }
            }
        }
    }
}

// ---- the same rules in Rust, for what SQL was told to let through -----------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Extra {
    created: Option<String>,
    play_count: Option<i64>,
}

pub struct Row {
    pub song: Song,
    extra: Extra,
    plays: i64,
    skips: i64,
    last_played_ms: i64,
    excluded: bool,
}

/// What a rule is evaluated against besides the song: the downloaded songs and the clock.
pub struct Env<'a> {
    pub downloaded: HashSet<&'a str>,
    pub now_ms: i64,
}

fn number(f: Field, r: &Row) -> i64 {
    let s = &r.song;
    match f {
        Year => s.year as i64,
        Duration => s.duration as i64,
        Track => s.track as i64,
        DiscNumber => s.disc_number as i64,
        BitRate => s.bit_rate as i64,
        SampleRate => s.sampling_rate as i64,
        BitDepth => s.bit_depth as i64,
        Size => s.size as i64,
        UserRating => s.user_rating as i64,
        PlayCount => r.plays,
        SkipCount => r.skips,
        ServerPlayCount => r.extra.play_count.unwrap_or(0),
        LastPlayed => r.last_played_ms,
        _ => 0,
    }
}

fn string(f: Field, r: &Row) -> &str {
    let s = &r.song;
    match f {
        Title => &s.title,
        Album => &s.album,
        Artist => &s.artist,
        Genre => s.genre.as_deref().unwrap_or(""),
        Suffix => &s.suffix,
        Added => r.extra.created.as_deref().unwrap_or(""),
        _ => "",
    }
}

/// Whether `row` passes the tree `n`: the last word where SQL was relaxed.
pub fn matches(n: &Node, row: &Row, env: &Env) -> bool {
    let r = match n {
        Node::Group { all: true, rules } => return rules.iter().all(|n| matches(n, row, env)),
        Node::Group { all: false, rules } => return rules.iter().any(|n| matches(n, row, env)),
        Node::Rule(r) => r,
    };
    let iso = r.field.kind() == Kind::DateIso;
    match &r.value {
        Val::Text(v) => {
            let (have, want) = (string(r.field, row).to_lowercase(), v.to_lowercase());
            match r.op {
                Op::Is => have == want,
                Op::IsNot => have != want,
                Op::Contains => have.contains(&want),
                Op::NotContains => !have.contains(&want),
                Op::StartsWith => have.starts_with(&want),
                _ => have.ends_with(&want),
            }
        }
        Val::Num(days) if matches!(r.op, Op::WithinDays | Op::NotWithinDays) => {
            let cutoff = env.now_ms - days * DAY_MS;
            let within = if iso { string(r.field, row) >= iso_from_ms(cutoff).as_str() } else { number(r.field, row) >= cutoff.max(1) };
            within == (r.op == Op::WithinDays)
        }
        Val::Num(v) => {
            let have = number(r.field, row);
            match r.op {
                Op::Is => have == *v,
                Op::IsNot => have != *v,
                Op::Greater => have > *v,
                _ => have < *v,
            }
        }
        Val::Range(a, b) => (*a..=*b).contains(&number(r.field, row)),
        Val::Date(text, ms) => {
            let ord = if iso { string(r.field, row).cmp(text.as_str()) } else { number(r.field, row).cmp(ms) };
            ord == if r.op == Op::Greater { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less }
        }
        Val::DateRange(a, b) => {
            if iso {
                let have = string(r.field, row);
                have >= a.0.as_str() && have <= b.0.as_str()
            } else {
                (a.1..=b.1).contains(&number(r.field, row))
            }
        }
        Val::None => {
            let have = match r.field {
                IsDownloaded => env.downloaded.contains(row.song.id.as_str()),
                ExcludedFromMixes => row.excluded,
                _ => row.song.starred,
            };
            have == (r.op == Op::IsTrue)
        }
    }
}

// ---- evaluation --------------------------------------------------------------

/// The index row `rowid` with its play statistics, as the rules read it.
pub fn load(c: &Connection, rowid: i64) -> rusqlite::Result<Option<Row>> {
    let mut st = c.prepare_cached(
        "SELECT i.json, coalesce(s.plays,0), coalesce(s.skips,0), coalesce(s.last_played_ms,0), EXISTS(SELECT 1 FROM mix_excluded e WHERE e.server=i.server AND e.song_id=i.id)
         FROM items i LEFT JOIN song_stats s ON s.server=i.server AND s.song_id=i.id WHERE i.rowid=?1",
    )?;
    let row = st.query_row([rowid], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()?;
    Ok(row.and_then(|(json, plays, skips, last_played_ms, excluded)| {
        Some(Row { song: serde_json::from_str(&json).ok()?, extra: serde_json::from_str(&json).unwrap_or_default(), plays, skips, last_played_ms, excluded })
    }))
}

/// CROSS JOIN pins the join order: without ANALYZE data the planner would still start from the index.
pub fn tables(root: &Node) -> &'static str {
    if root.needs_stats() {
        "FROM song_stats s CROSS JOIN items i ON s.server=sid() AND i.server=sid() AND i.kind=2 AND i.id=s.song_id"
    } else {
        "FROM items i LEFT JOIN song_stats s ON s.server=i.server AND s.song_id=i.id"
    }
}

/// The songs of the playlist at positions `offset..offset+limit`, and how many positions were walked.
/// With `count_all` the walk does not stop at the end of the page, so the second number is the playlist's length.
pub fn run(c: &Connection, def: &Def, downloaded: &[String], offset: usize, limit: usize, count_all: bool, now_ms: i64) -> rusqlite::Result<(Vec<Song>, usize)> {
    let mut cp = Compiler { args: Vec::new(), downloaded, downloaded_arg: None, now_ms, needs_rust: false };
    let cond = cp.node(&def.root);
    let from = format!("{} WHERE {} AND {cond}", tables(&def.root), mixes::SONGS);
    let cap = def.limit.map(|l| l as usize);
    let sql_is_exact = !cp.needs_rust && def.limit_ms.is_none();

    if sql_is_exact && count_all {
        let n: i64 = c.prepare_cached(&format!("SELECT count(*) {from}"))?.query_row(rusqlite::params_from_iter(cp.args), |r| r.get(0))?;
        return Ok((Vec::new(), cap.map_or(n as usize, |cap| (n as usize).min(cap))));
    }
    let order = match def.sort {
        Sort::Index => "i.rowid".to_string(),
        Sort::Random(seed) => {
            cp.args.extend(mixes::shuffled_params(seed));
            format!("{}, i.rowid", mixes::shuffled_order("i.rowid", cp.args.len() - 1, cp.args.len()))
        }
        Sort::By(f, descending) => {
            let collate = if f.kind() == Kind::Text { " COLLATE NOCASE" } else { "" };
            format!("{}{collate}{}, i.rowid", f.def().2, if descending { " DESC" } else { "" })
        }
    };
    // A LIMIT lets SQLite keep a top-N heap instead of sorting every match.
    let window = if sql_is_exact {
        let take = cap.map_or(limit, |cap| limit.min(cap.saturating_sub(offset)));
        cp.args.push(Sql::Integer(take as i64));
        cp.args.push(Sql::Integer(offset as i64));
        format!(" LIMIT ?{} OFFSET ?{}", cp.args.len() - 1, cp.args.len())
    } else if let Some(cap) = cap.filter(|_| !cp.needs_rust) {
        cp.args.push(Sql::Integer(cap as i64));
        format!(" LIMIT ?{}", cp.args.len())
    } else {
        String::new()
    };
    let duration = if def.limit_ms.is_some() { "json_extract(i.json,'$.duration')" } else { "0" };
    let mut st = c.prepare_cached(&format!("SELECT i.rowid, {duration} {from} ORDER BY {order}{window}"))?;
    let mut rows = st.query(rusqlite::params_from_iter(cp.args))?;

    let env = Env { downloaded: if cp.needs_rust { downloaded.iter().map(String::as_str).collect() } else { HashSet::new() }, now_ms };
    let budget = def.limit_ms.unwrap_or(i64::MAX);
    let (mut page, mut position, mut total_ms) = (Vec::new(), if sql_is_exact { offset } else { 0 }, 0i64);
    while let Some(r) = rows.next()? {
        if cap.is_some_and(|cap| position >= cap) || (!count_all && page.len() >= limit) {
            break;
        }
        let (rowid, duration_s): (i64, Option<i64>) = (r.get(0)?, r.get(1)?);
        let mut row = None;
        if cp.needs_rust {
            match load(c, rowid)? {
                Some(l) if matches(&def.root, &l, &env) => row = Some(l),
                _ => continue,
            }
        }
        total_ms = total_ms.saturating_add(duration_s.unwrap_or(0) * 1000);
        if total_ms > budget {
            break;
        }
        if position >= offset && page.len() < limit {
            let row = match row {
                Some(r) => Some(r),
                None => load(c, rowid)?,
            };
            page.extend(row.map(|r| r.song));
        }
        position += 1;
    }
    Ok((page, position))
}

/// The built-in definitions the UI offers as starting points; not stored until saved.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_defaults() -> Vec<SmartPlaylist> {
    let one = |field: &str, op: &str, value: Value| json!({ "all": true, "rules": [{ "field": field, "op": op, "value": value }] });
    let sorted = |field: &str, descending: bool| json!({ "field": field, "descending": descending });
    let defs = [
        ("default-most-played", SmartBuiltin::MostPlayed, json!({ "match": one("playCount", "greater", json!(0)), "sort": sorted("playCount", true), "limit": 100 })),
        ("default-recently-played", SmartBuiltin::RecentlyPlayed, json!({ "match": one("lastPlayed", "withinDays", json!(30)), "sort": sorted("lastPlayed", true), "limit": 100 })),
        ("default-recently-added", SmartBuiltin::RecentlyAdded, json!({ "match": one("added", "withinDays", json!(90)), "sort": sorted("added", true), "limit": 200 })),
        ("default-never-played", SmartBuiltin::NeverPlayed, json!({ "match": one("playCount", "is", json!(0)), "sort": { "field": "random", "seed": 0 }, "limit": 100 })),
        ("default-top-rated", SmartBuiltin::TopRated, json!({ "match": one("userRating", "greater", json!(3)), "sort": sorted("userRating", true), "limit": 200 })),
        (
            "default-forgotten-favourites",
            SmartBuiltin::ForgottenFavourites,
            json!({ "match": { "all": true, "rules": [{ "field": "starred", "op": "isTrue" }, { "field": "lastPlayed", "op": "notWithinDays", "value": 180 }] },
                    "sort": sorted("lastPlayed", false), "limit": 100 }),
        ),
        ("default-long-tracks", SmartBuiltin::LongTracks, json!({ "match": one("duration", "greater", json!(600)), "sort": sorted("duration", true), "limit": 100 })),
    ];
    defs.into_iter().map(|(id, builtin, json)| SmartPlaylist { id: id.into(), name: String::new(), json: json.to_string(), builtin: Some(builtin) }).collect()
}

/// A smart playlist as its page shows it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SmartPage {
    pub songs: Vec<Song>,
    /// The songs' summed length in seconds.
    pub seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::tests::{DAY, NOW};

    #[test]
    fn dates_convert_both_ways() {
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00");
        assert_eq!(iso_from_ms(1_709_251_199_000), "2024-02-29T23:59:59");
        assert_eq!(iso_from_ms(NOW), "2026-08-29T10:40:00");
        assert_eq!(date(&json!("2024-02-29T23:59:59.123Z"), false, "").unwrap(), ("2024-02-29T23:59:59".to_string(), 1_709_251_199_000));
        assert_eq!(date(&json!("1970-01-01"), false, "").unwrap().1, 0);
        assert_eq!(date(&json!("1970-01-01"), true, "").unwrap(), ("1970-01-01T23:59:60".to_string(), DAY - 1));
        for bad in ["2024", "2024/01/01", "2024-00-10", "2024-01-01T25:00:00", "2024-01-01 10:00:00", "ünï-cö-dé"] {
            assert!(date(&json!(bad), false, "").is_err(), "{bad}");
        }
    }
}
