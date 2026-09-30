//! The smart playlist editor: a flat list of rules, as a form shows them, to and from the definition JSON.
//!
//! The editor only writes one group of plain rules; a definition with nested groups is not something it
//! can show, and reading one back says so (None) so the caller keeps it as it is. What it writes is the
//! same JSON a hand-written definition would be (see the top of `smart.rs`).

use std::collections::HashMap;

use nori_model::{CoreError, SmartPlaylist};
use serde_json::{Map, Value};

use super::{parse, FIELDS, Kind};

/// One condition as the form holds it: `value` is what was typed.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SmartEditRule {
    pub field: String,
    pub op: String,
    pub value: String,
}

/// A smart playlist as the editor holds it. `limit` 0 is no limit.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SmartEdit {
    pub id: String,
    pub name: String,
    pub all: bool,
    pub rules: Vec<SmartEditRule>,
    pub sort_field: String,
    pub descending: bool,
    pub limit: i32,
}

/// What the editor offers: the fields by type, what can be sorted by, and the operators of each field
/// in the order the form lists them (the first is what a new choice of field starts with).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SmartSchema {
    pub texts: Vec<String>,
    pub numbers: Vec<String>,
    pub dates: Vec<String>,
    pub flags: Vec<String>,
    /// The operators that take no value.
    pub flag_ops: Vec<String>,
    pub sorts: Vec<String>,
    /// Field name -> its operators. A field not in here is a flag.
    pub ops: HashMap<String, Vec<String>>,
}

/// A draft ready to be stored: the rules for a new id and a missing name applied, and the definition
/// checked. `error` says what is wrong; nothing is to be stored then.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SmartPrepared {
    pub id: String,
    pub name: String,
    pub json: String,
    pub error: Option<String>,
}

const TEXT_OPS: [&str; 6] = ["contains", "is", "isNot", "notContains", "startsWith", "endsWith"];
const NUMBER_OPS: [&str; 5] = ["is", "isNot", "greater", "less", "between"];
const DATE_OPS: [&str; 4] = ["withinDays", "notWithinDays", "greater", "less"];
const FLAG_OPS: [&str; 2] = ["isTrue", "isFalse"];

fn names(of: impl Fn(Kind) -> bool) -> impl Iterator<Item = &'static str> {
    FIELDS.into_iter().filter(move |f| of(f.kind())).map(|f| f.name())
}

fn is_number(field: &str) -> bool {
    names(|k| k == Kind::Num).any(|n| n == field)
}

/// What a person types between two numbers: "1970-1979", "1970, 1979", "1970 1979".
fn range_parts(value: &str) -> impl Iterator<Item = &str> {
    value.split([',', '-', ' ', '\t', '\n', '\u{0B}', '\u{0C}', '\r']).filter(|p| !p.trim().is_empty())
}

/// A number where one is typed, the text otherwise; the definition's parser says if that is wrong.
fn number_or_text(text: &str) -> Value {
    text.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::from(text))
}

fn value_of(r: &SmartEditRule) -> Value {
    if r.op == "between" {
        return Value::Array(range_parts(&r.value).map(number_or_text).collect());
    }
    // A number only where the field takes one: a title of "1999" stays text.
    match r.value.parse::<i64>() {
        Ok(n) if is_number(&r.field) || r.op.ends_with("Days") => Value::from(n),
        _ => Value::from(r.value.as_str()),
    }
}

fn quoted(s: &str) -> String {
    Value::from(s).to_string()
}

/// Written by hand rather than through a map so the keys come out in the order a person reads them.
fn to_json(d: &SmartEdit) -> String {
    let flag = |op: &str| FLAG_OPS.contains(&op);
    // A rule with nothing typed in is left out, unless it is one that takes no value.
    let rules: Vec<String> = d
        .rules
        .iter()
        .filter(|r| !r.value.trim().is_empty() || flag(&r.op))
        .map(|r| {
            let value = if flag(&r.op) { String::new() } else { format!(",\"value\":{}", value_of(r)) };
            format!("{{\"field\":{},\"op\":{}{value}}}", quoted(&r.field), quoted(&r.op))
        })
        .collect();
    let limit = if d.limit > 0 { format!(",\"limit\":{}", d.limit) } else { String::new() };
    format!(
        "{{\"match\":{{\"all\":{},\"rules\":[{}]}},\"sort\":{{\"field\":{},\"descending\":{},\"seed\":1}}{limit}}}",
        d.all,
        rules.join(","),
        quoted(&d.sort_field),
        d.descending
    )
}

/// A value as text, the way the form shows it again: a string as it is, anything else as JSON.
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// "true" / "false" as text count too; anything else is `fallback`.
fn flag_of(v: Option<&Value>, fallback: bool) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => true,
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => false,
        _ => fallback,
    }
}

/// A whole number of anything numeric (a fraction is cut off, a numeric text is read), else `fallback`.
fn int_of(v: Option<&Value>, fallback: i32) -> i32 {
    match v {
        Some(Value::Number(n)) => n.as_i64().map(|i| i as i32).or_else(|| n.as_f64().map(|f| f as i32)).unwrap_or(fallback),
        Some(Value::String(s)) => s.trim().parse::<f64>().map(|f| f as i32).unwrap_or(fallback),
        _ => fallback,
    }
}

fn default_rule() -> SmartEditRule {
    SmartEditRule { field: "genre".into(), op: "contains".into(), value: String::new() }
}

fn read(p: &SmartPlaylist) -> Option<SmartEdit> {
    let o: Map<String, Value> = serde_json::from_str(&p.json).ok()?;
    let m = o.get("match").and_then(Value::as_object);
    let mut rules = Vec::new();
    if let Some(list) = m.and_then(|m| m.get("rules")).and_then(Value::as_array) {
        for r in list {
            let r = r.as_object()?;
            // A nested group: not something a flat form can show.
            if r.contains_key("rules") {
                return None;
            }
            let value = match r.get("value") {
                None => String::new(),
                Some(Value::Array(a)) => a.iter().map(text_of).collect::<Vec<_>>().join(" "),
                Some(v) => text_of(v),
            };
            rules.push(SmartEditRule { field: text_of(r.get("field")?), op: text_of(r.get("op")?), value });
        }
    }
    if rules.is_empty() {
        rules.push(default_rule());
    }
    let s = o.get("sort").and_then(Value::as_object);
    Some(SmartEdit {
        id: p.id.clone(),
        name: p.name.clone(),
        all: m.is_none_or(|m| flag_of(m.get("all"), true)),
        rules,
        sort_field: s.and_then(|s| s.get("field")).map_or_else(|| "random".into(), text_of),
        descending: s.is_some_and(|s| flag_of(s.get("descending"), false)),
        limit: int_of(o.get("limit"), 0),
    })
}

/// A new, empty draft: one rule waiting for a genre, a random order and 100 songs.
pub(crate) fn smart_edit_new() -> SmartEdit {
    SmartEdit { id: String::new(), name: String::new(), all: true, rules: vec![default_rule()], sort_field: "random".into(), descending: false, limit: 100 }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_edit_schema() -> SmartSchema {
    let list = |k: Kind| names(move |x| x == k).map(String::from).collect::<Vec<_>>();
    let dates: Vec<String> = names(|k| matches!(k, Kind::DateMs | Kind::DateIso)).map(String::from).collect();
    let (texts, numbers, flags) = (list(Kind::Text), list(Kind::Num), list(Kind::Flag));
    let owned = |ops: &[&str]| ops.iter().map(|o| o.to_string()).collect::<Vec<_>>();
    let mut ops = HashMap::new();
    for (fields, of) in [(&texts, &TEXT_OPS[..]), (&numbers, &NUMBER_OPS[..]), (&dates, &DATE_OPS[..]), (&flags, &FLAG_OPS[..])] {
        for f in fields {
            ops.insert(f.clone(), owned(of));
        }
    }
    let sorts = std::iter::once("random".to_string()).chain(texts.iter().chain(&numbers).chain(&dates).cloned()).collect();
    SmartSchema { texts, numbers, dates, flags, flag_ops: owned(&FLAG_OPS), sorts, ops }
}

/// The draft the editor opens with: `playlist`'s own when this editor can show it (a built-in one as a
/// copy, with no id, so saving makes a playlist of the user's own), otherwise a new one.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_edit_open(playlist: Option<SmartPlaylist>) -> SmartEdit {
    let Some(mut e) = playlist.as_ref().and_then(read) else { return smart_edit_new() };
    if e.id.starts_with("default-") {
        e.id = String::new();
    }
    e
}

/// Rule `at` now compares `field`: its operator goes back to that field's first, since the old one may
/// not apply.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_edit_field(mut edit: SmartEdit, at: u32, field: String) -> SmartEdit {
    let schema = smart_edit_schema();
    if let Some(r) = edit.rules.get_mut(at as usize) {
        r.op = schema.ops.get(&field).and_then(|o| o.first()).cloned().unwrap_or_else(|| FLAG_OPS[0].into());
        r.field = field;
    }
    edit
}

/// The operators rule `field` offers.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_edit_ops(field: String) -> Vec<String> {
    smart_edit_schema().ops.remove(&field).unwrap_or_else(|| FLAG_OPS.map(String::from).to_vec())
}

/// Every field, in the order the editor lists them.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_edit_fields() -> Vec<String> {
    let s = smart_edit_schema();
    s.texts.into_iter().chain(s.numbers).chain(s.dates).chain(s.flags).collect()
}

/// Rule `at` taken away; the form always keeps one rule to fill in.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_edit_remove(mut edit: SmartEdit, at: u32) -> SmartEdit {
    if (at as usize) < edit.rules.len() {
        edit.rules.remove(at as usize);
    }
    if edit.rules.is_empty() {
        edit.rules.push(default_rule());
    }
    edit
}

/// A new empty rule at the end.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_edit_add(mut edit: SmartEdit) -> SmartEdit {
    edit.rules.push(default_rule());
    edit
}

/// What the value field of a rule with operator `op` says while empty; None when the operator takes no
/// value, and there is no field.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_value_hint(op: String) -> Option<String> {
    if FLAG_OPS.contains(&op.as_str()) {
        None
    } else if op == "between" {
        Some("from to".into())
    } else if op.ends_with("Days") {
        Some("days".into())
    } else {
        Some("value".into())
    }
}

/// The limit as typed: a number, or none (0) for anything else.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_limit_typed(text: String) -> i32 {
    text.parse().unwrap_or(0)
}

/// The limit as the field shows it: empty for none.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_limit_text(limit: i32) -> String {
    if limit > 0 { limit.to_string() } else { String::new() }
}

/// The draft ready to store: a built-in ("default-…") saves as a new playlist, a blank name comes back
/// empty for the client to word, and the definition is checked.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn smart_edit_prepare(edit: SmartEdit) -> SmartPrepared {
    let json = to_json(&edit);
    // Worded exactly as the app has always shown it, which is the message of the error as it came across the FFI.
    let error = parse(&json).err().map(|e| match e {
        CoreError::Parse { reason } => format!("reason={reason}"),
        other => other.to_string(),
    });
    let id = if edit.id.starts_with("default-") { String::new() } else { edit.id };
    let name = if edit.name.trim().is_empty() { String::new() } else { edit.name };
    SmartPrepared { id, name, json, error }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(field: &str, op: &str, value: &str) -> SmartEditRule {
        SmartEditRule { field: field.into(), op: op.into(), value: value.into() }
    }

    #[test]
    fn the_forms_own_rules() {
        let new = smart_edit_open(None);
        assert_eq!((new.rules.len(), new.sort_field.as_str(), new.limit, new.all), (1, "random", 100, true));
        let year = smart_edit_field(new.clone(), 0, "year".into());
        assert_eq!((year.rules[0].field.as_str(), year.rules[0].op.as_str()), ("year", "is"));
        let empty = smart_edit_remove(new.clone(), 0);
        assert_eq!(empty.rules, vec![rule("genre", "contains", "")]);
        assert_eq!(smart_edit_add(new).rules.len(), 2);
        assert_eq!(smart_value_hint("isTrue".into()), None);
        assert_eq!(smart_value_hint("between".into()).as_deref(), Some("from to"));
        assert_eq!(smart_value_hint("withinDays".into()).as_deref(), Some("days"));
        assert_eq!(smart_value_hint("is".into()).as_deref(), Some("value"));
        assert_eq!((smart_limit_typed("25".into()), smart_limit_typed("x".into())), (25, 0));
        assert_eq!((smart_limit_text(0), smart_limit_text(7)), (String::new(), "7".to_string()));
        assert_eq!(smart_edit_fields().first().map(String::as_str), smart_edit_schema().texts.first().map(String::as_str));
    }

    fn edit(rules: Vec<SmartEditRule>) -> SmartEdit {
        SmartEdit { rules, ..smart_edit_new() }
    }

    #[test]
    fn writes_what_the_form_holds() {
        let e = edit(vec![
            rule("genre", "contains", "Rock"),
            rule("year", "between", "1970 - 1979"),
            rule("title", "is", "1999"),
            rule("year", "greater", "1990"),
            rule("added", "withinDays", "30"),
            rule("starred", "isTrue", "ignored"),
            rule("artist", "is", "  "),
            rule("year", "between", "1970,abc"),
        ]);
        assert_eq!(
            to_json(&e),
            r#"{"match":{"all":true,"rules":[{"field":"genre","op":"contains","value":"Rock"},{"field":"year","op":"between","value":[1970,1979]},{"field":"title","op":"is","value":"1999"},{"field":"year","op":"greater","value":1990},{"field":"added","op":"withinDays","value":30},{"field":"starred","op":"isTrue"},{"field":"year","op":"between","value":[1970,"abc"]}]},"sort":{"field":"random","descending":false,"seed":1},"limit":100}"#
        );
        let none = SmartEdit { limit: 0, all: false, sort_field: "year".into(), descending: true, ..edit(vec![]) };
        assert_eq!(to_json(&none), r#"{"match":{"all":false,"rules":[]},"sort":{"field":"year","descending":true,"seed":1}}"#);
    }

    #[test]
    fn reads_back_what_it_wrote() {
        let e = SmartEdit { id: "sp-1".into(), name: "Old".into(), ..edit(vec![rule("genre", "is", "Jazz"), rule("year", "between", "1970 1979"), rule("starred", "isFalse", "")]) };
        let p = SmartPlaylist { id: e.id.clone(), name: e.name.clone(), json: to_json(&e), builtin: None };
        assert_eq!(read(&p), Some(e));
        let nested = SmartPlaylist { json: r#"{"match":{"rules":[{"all":false,"rules":[]}]}}"#.into(), ..Default::default() };
        assert_eq!(read(&nested), None);
        assert_eq!(read(&SmartPlaylist { json: "not json".into(), ..Default::default() }), None);
        assert_eq!(read(&SmartPlaylist { json: r#"{"match":{"rules":[{"op":"is"}]}}"#.into(), ..Default::default() }), None);
        // Nothing but defaults: one empty genre rule, random order, no limit.
        let bare = read(&SmartPlaylist { json: "{}".into(), ..Default::default() }).unwrap();
        assert_eq!(bare, SmartEdit { limit: 0, ..smart_edit_new() });
        let loose = read(&SmartPlaylist { json: r#"{"match":{"all":"false"},"sort":{"field":"year","descending":true},"limit":"50.7"}"#.into(), ..Default::default() }).unwrap();
        assert!(!loose.all && loose.descending);
        assert_eq!((loose.sort_field.as_str(), loose.limit), ("year", 50));
    }

    #[test]
    fn builtins_read_into_form() {
        for p in super::super::smart_defaults() {
            let e = read(&p).unwrap();
            assert!(!e.rules.is_empty(), "{}", p.id);
        }
    }

    #[test]
    fn prepare_applies_the_save_rules_and_checks() {
        let p = smart_edit_prepare(SmartEdit { id: "default-most-played".into(), name: " ".into(), ..edit(vec![rule("year", "greater", "1990")]) });
        assert_eq!((p.id.as_str(), p.name.as_str(), p.error), ("", "", None));
        let p = smart_edit_prepare(SmartEdit { id: "sp-2".into(), name: "Mine".into(), ..edit(vec![rule("year", "greater", "soon")]) });
        assert_eq!((p.id.as_str(), p.name.as_str()), ("sp-2", "Mine"));
        assert!(p.error.unwrap().starts_with("reason=smart playlist: match.rules[0]"));
    }

    #[test]
    fn schema_is_the_editor_tables() {
        let s = smart_edit_schema();
        assert_eq!(s.texts, ["title", "album", "artist", "genre", "suffix"]);
        assert_eq!(
            s.numbers,
            ["year", "duration", "track", "discNumber", "bitRate", "sampleRate", "bitDepth", "size", "userRating", "playCount", "skipCount", "serverPlayCount"]
        );
        assert_eq!(s.dates, ["lastPlayed", "added"]);
        assert_eq!(s.flags, ["starred", "isDownloaded", "excludedFromMixes"]);
        assert_eq!(s.sorts.len(), 1 + 5 + 12 + 2);
        assert_eq!(s.sorts[0], "random");
        assert_eq!(s.ops["genre"][0], "contains");
        assert_eq!(s.ops["added"], DATE_OPS);
        assert_eq!(s.ops["starred"], FLAG_OPS);
    }
}
