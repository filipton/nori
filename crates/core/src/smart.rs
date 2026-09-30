//! Smart playlist storage and evaluation over the index. Rules and evaluation are nori-library's.

use rusqlite::params;

use crate::{db, mixes, model::*, Core, Result};

pub use nori_library::smart::*;
use nori_library::smart::Field::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Stored smart playlists, newest first.
    pub fn smart_list(&self) -> Result<Vec<SmartPlaylist>> {
        let c = self.db.lock();
        let mut st = c.prepare_cached("SELECT id, name, json FROM smart_playlists WHERE server=sid() ORDER BY updated_ms DESC, id")?;
        let rows = st.query_map([], |r| Ok(SmartPlaylist { id: r.get(0)?, name: r.get(1)?, json: r.get(2)?, builtin: None }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Stores a validated definition; an empty `id` creates a new one. Returns the id.
    pub fn smart_save(&self, id: String, name: String, json: String) -> Result<String> {
        parse(&json)?;
        let c = self.db.lock();
        let now = db::now_ms();
        let id = if id.is_empty() {
            let stored: i64 = c.query_row("SELECT count(*) FROM smart_playlists WHERE server=sid()", [], |r| r.get(0))?;
            format!("sp-{:x}", mixes::Rng::new(now as u64 ^ (stored as u64) << 48).next())
        } else {
            id
        };
        c.execute("INSERT OR REPLACE INTO smart_playlists(server, id, name, json, updated_ms) VALUES(sid(), ?1, ?2, ?3, ?4)", params![id, name, json, now])?;
        Ok(id)
    }

    pub fn smart_delete(&self, id: String) -> Result<()> {
        self.db.lock().execute("DELETE FROM smart_playlists WHERE server=sid() AND id=?1", [id])?;
        Ok(())
    }

    /// The first `limit` songs and their total length.
    pub fn smart_page(&self, json: String, limit: u32) -> Result<SmartPage> {
        let songs = self.smart_evaluate(json, 0, limit)?;
        Ok(SmartPage { seconds: crate::pages::total_seconds(&songs), songs })
    }
}

impl Core {
    /// One page of the playlist, within its own `limit` / `limitMs`.
    pub(crate) fn smart_evaluate(&self, json: String, offset: u32, limit: u32) -> Result<Vec<Song>> {
        let def = parse(&json)?;
        let downloaded = self.downloaded_for(&def)?;
        Ok(run(&self.db.lock(), &def, &downloaded, offset as usize, limit as usize, false, db::now_ms())?.0)
    }

    /// The finished download ids, read only when a rule asks about downloads.
    fn downloaded_for(&self, def: &Def) -> Result<Vec<String>> {
        if !def.root.asks(IsDownloaded) {
            return Ok(Vec::new());
        }
        self.download_ids(true)
    }
}

#[cfg(test)]
impl Core {
    /// The playlist's song count, caps applied.
    pub(crate) fn smart_count(&self, json: String) -> Result<u32> {
        let def = parse(&json)?;
        let downloaded = self.downloaded_for(&def)?;
        Ok(run(&self.db.lock(), &def, &downloaded, 0, 0, true, db::now_ms())?.1 as u32)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::history::tests::{listen, skip, song, DAY, NOW};
    use crate::CoreError;
    use serde_json::{json, Value};

    fn one(field: &str, op: &str, value: Value) -> String {
        json!({ "match": { "rules": [{ "field": field, "op": op, "value": value }] } }).to_string()
    }

    fn ids(l: &[Song]) -> Vec<&str> {
        l.iter().map(|s| s.id.as_str()).collect()
    }

    /// Evaluates with SQL and with the Rust matcher and asserts they agree.
    fn eval(core: &Core, json: &str, downloaded: &[&str]) -> Vec<String> {
        let def = parse(json).unwrap();
        let downloaded: Vec<String> = downloaded.iter().map(|s| s.to_string()).collect();
        let c = core.db.lock();
        let (page, n) = run(&c, &def, &downloaded, 0, 10_000, false, NOW).unwrap();
        assert_eq!(n, page.len());
        assert_eq!(run(&c, &def, &downloaded, 0, 0, true, NOW).unwrap().1, n, "count agrees with evaluate");

        let env = Env { downloaded: downloaded.iter().map(String::as_str).collect(), now_ms: NOW };
        let mut by_rust: Vec<String> = Vec::new();
        let mut st = c.prepare("SELECT rowid FROM items WHERE kind=2").unwrap();
        for rowid in st.query_map([], |r| r.get::<_, i64>(0)).unwrap() {
            let row = load(&c, rowid.unwrap()).unwrap().unwrap();
            if matches(&def.root, &row, &env) {
                by_rust.push(row.song.id);
            }
        }
        if def.limit.is_none() && def.limit_ms.is_none() {
            let mut by_sql: Vec<String> = page.iter().map(|s| s.id.clone()).collect();
            by_sql.sort();
            by_rust.sort();
            assert_eq!(by_sql, by_rust, "SQL and Rust disagree on {json}");
        }
        page.into_iter().map(|s| s.id).collect()
    }

    fn library() -> std::sync::Arc<Core> {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let mut songs = vec![
            song("dogs", "Dogs", "Pink Floyd", "Animals", "Progressive Rock", 1977),
            song("pigs", "Pigs (Three Different Ones)", "Pink Floyd", "Animals", "Progressive Rock", 1977),
            song("joga", "Jóga", "Björk", "Homogenic", "Electronic", 1997),
            song("bach", "Hunter", "BJÖRK", "Homogenic", "Electronic", 1997),
            song("so", "So What", "Miles Davis", "Kind of Blue", "Jazz", 1959),
            song("pct", "100% Pure_Love", "Crystal Waters", "Storyteller", "House", 1994),
            Song { id: "bare".into(), title: "Untitled".into(), ..Default::default() },
        ];
        songs[0].duration = 1024;
        songs[0].starred = true;
        songs[1].user_rating = 5;
        songs[2].bit_rate = 320;
        songs[2].suffix = "mp3".into();
        songs[4].user_rating = 4;
        songs[4].starred = true;
        songs[4].sampling_rate = 96_000;
        songs[4].bit_depth = 24;
        songs[4].size = 123_456_789;
        db::index(&mut core.db.lock(), &[], &[], &songs).unwrap();
        // Keys Song does not carry yet.
        let c = core.db.lock();
        c.execute("UPDATE items SET json=json_set(json,'$.created','2026-08-20T10:00:00.000Z','$.playCount',40) WHERE id='dogs'", []).unwrap();
        c.execute("UPDATE items SET json=json_set(json,'$.created','2024-02-29T23:59:59Z','$.playCount',2) WHERE id='so'", []).unwrap();
        drop(c);
        for d in [1, 2, 3] {
            listen(&core, &songs[0], NOW - d * DAY);
        }
        listen(&core, &songs[4], NOW - 200 * DAY);
        skip(&core, &songs[5], NOW - DAY);
        skip(&core, &songs[5], NOW - 2 * DAY);
        core.mix_excluded_set("pigs".into(), true).unwrap();
        core
    }

    #[test]
    fn unicode_values_fall_back_to_rust_and_fold_case() {
        let core = library();
        assert_eq!(eval(&core, &one("artist", "is", json!("björk")), &[]), ["joga", "bach"]);
        assert_eq!(eval(&core, &one("title", "contains", json!("Ó")), &[]), ["joga"]);
        // Mixed with SQL rules, paged and counted.
        let def = json!({ "match": { "all": false, "rules": [
            { "field": "artist", "op": "is", "value": "BJÖRK" }, { "field": "year", "op": "less", "value": 1960 } ] },
            "sort": { "field": "title", "descending": true } })
        .to_string();
        assert_eq!(eval(&core, &def, &[]), ["bare", "so", "joga", "bach"], "no year is year 0");
        assert_eq!(ids(&core.smart_evaluate(def.clone(), 2, 1).unwrap()), ["joga"]);
        assert_eq!(core.smart_count(def).unwrap(), 4);
    }

    #[test]
    fn flag_fields() {
        let core = library();
        let flag = |field: &str, op: &str| json!({ "match": { "rules": [{ "field": field, "op": op }] } }).to_string();
        assert_eq!(eval(&core, &flag("starred", "isTrue"), &[]), ["dogs", "so"]);
        assert_eq!(eval(&core, &flag("starred", "isFalse"), &[]).len(), 5);
        assert_eq!(eval(&core, &flag("excludedFromMixes", "isTrue"), &[]), ["pigs"]);
        assert_eq!(eval(&core, &flag("excludedFromMixes", "isFalse"), &[]).len(), 6);
        assert_eq!(eval(&core, &flag("isDownloaded", "isTrue"), &["joga", "so", "gone"]), ["joga", "so"]);
        assert_eq!(eval(&core, &flag("isDownloaded", "isFalse"), &["joga", "so"]).len(), 5);
        assert!(eval(&core, &flag("isDownloaded", "isTrue"), &[]).is_empty());
        // Used twice, bound once.
        let both = json!({ "match": { "all": false, "rules": [{ "field": "isDownloaded", "op": "isTrue" }, { "all": true, "rules": [
            { "field": "isDownloaded", "op": "isFalse" }, { "field": "year", "op": "is", "value": 1959 }] }] } })
        .to_string();
        assert_eq!(eval(&core, &both, &["joga", "it's"]), ["joga", "so"]);
        // Through the core only finished downloads count.
        let songs: Vec<Song> = ["joga", "so"].map(|id| Song { id: id.into(), ..Default::default() }).to_vec();
        core.download_queue(songs).unwrap();
        core.download_done("joga".into()).unwrap();
        assert_eq!(ids(&core.smart_evaluate(flag("isDownloaded", "isTrue"), 0, 50).unwrap()), ["joga"]);
        assert_eq!(core.smart_count(flag("isDownloaded", "isFalse")).unwrap(), 6);
    }

    #[test]
    fn sort_limit_and_paging() {
        let core = library();
        let by = |field: &str, descending: bool| json!({ "sort": { "field": field, "descending": descending } }).to_string();
        assert_eq!(eval(&core, &by("title", false), &[])[..3], ["pct", "dogs", "bach"]);
        assert_eq!(eval(&core, &by("year", true), &[])[..2], ["joga", "bach"], "ties keep index order");
        assert_eq!(eval(&core, &by("playCount", true), &[])[..2], ["dogs", "so"]);
        assert_eq!(eval(&core, &by("lastPlayed", true), &[])[..2], ["dogs", "so"]);
        assert_eq!(eval(&core, &by("starred", true), &[])[..2], ["dogs", "so"]);

        let capped = json!({ "sort": { "field": "year" }, "limit": 3 }).to_string();
        assert_eq!(eval(&core, &capped, &[]), ["bare", "so", "dogs"]);
        assert_eq!(core.smart_count(capped.clone()).unwrap(), 3);
        assert_eq!(ids(&core.smart_evaluate(capped.clone(), 2, 50).unwrap()), ["dogs"], "paging stops at the limit");
        assert!(core.smart_evaluate(capped.clone(), 3, 50).unwrap().is_empty());
        assert!(core.smart_evaluate(capped, 0, 0).unwrap().is_empty());

        let pages: Vec<String> = (0..4).flat_map(|p| core.smart_evaluate(by("title", false), p * 2, 2).unwrap()).map(|s| s.id).collect();
        assert_eq!(pages, eval(&core, &by("title", false), &[]));
    }

    #[test]
    fn random_sort_is_stable_per_seed() {
        let core = library();
        let shuffled = |seed: u64| eval(&core, &json!({ "sort": { "field": "random", "seed": seed } }).to_string(), &[]);
        assert_eq!(shuffled(1), shuffled(1));
        assert_eq!(shuffled(1).len(), 7);
        assert!((2..12).any(|s| shuffled(s) != shuffled(1)));
        let paged: Vec<String> = (0..7).flat_map(|p| core.smart_evaluate(json!({ "sort": { "field": "random", "seed": 1 } }).to_string(), p, 1).unwrap()).map(|s| s.id).collect();
        assert_eq!(paged, shuffled(1), "pages tile");
    }

    #[test]
    fn duration_budget() {
        let core = library();
        // Durations: dogs 1024 s, five of 200 s, bare 0 s.
        let def = |ms: i64| json!({ "sort": { "field": "duration", "descending": true }, "limitMs": ms }).to_string();
        assert_eq!(eval(&core, &def(1_500_000), &[]), ["dogs", "pigs", "joga"]);
        assert_eq!(core.smart_count(def(1_500_000)).unwrap(), 3);
        assert_eq!(ids(&core.smart_evaluate(def(1_500_000), 1, 5).unwrap()), ["pigs", "joga"]);
        assert!(eval(&core, &def(1000), &[]).is_empty(), "first song exceeds it");
        assert_eq!(eval(&core, &def(100_000_000), &[]).len(), 7);
        let both = json!({ "sort": { "field": "duration", "descending": true }, "limitMs": 1_500_000, "limit": 2 }).to_string();
        assert_eq!(eval(&core, &both, &[]), ["dogs", "pigs"]);
        // With the Rust fallback too.
        let uni = json!({ "match": { "rules": [{ "field": "artist", "op": "is", "value": "björk" }] }, "limitMs": 250_000 }).to_string();
        assert_eq!(eval(&core, &uni, &[]), ["joga"]);
    }

    #[test]
    fn empty_index_yields_nothing() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        for d in smart_defaults() {
            assert!(core.smart_evaluate(d.json.clone(), 0, 50).unwrap().is_empty());
            assert_eq!(core.smart_count(d.json).unwrap(), 0);
        }
        assert!(core.smart_list().unwrap().is_empty());
    }

    #[test]
    fn validation_errors_name_path_and_problem() {
        use crate::SmartProblem::*;
        smart_validate(r#"{"match":null,"sort":null,"limit":null,"limitMs":0}"#.into()).unwrap();
        let rule = |r: &str| format!(r#"{{"match":{{"rules":[{r}]}}}}"#);
        let mut deep = r#"{"field":"year","op":"is","value":1}"#.to_string();
        for _ in 0..9 {
            deep = format!(r#"{{"rules":[{deep}]}}"#);
        }
        let cases = [
            ("nope".to_string(), "", NotJson),
            ("[]".into(), "", NotObject),
            (r#"{"macth":{}}"#.into(), ".macth", UnknownKey),
            (r#"{"match":{"field":"year","op":"is","value":1}}"#.into(), "match", SingleRule),
            (rule(r#"{"op":"is"}"#), "match.rules[0]", NotRuleOrGroup),
            (rule(r#"{"field":"year","op":"is","value":1},{"all":false,"rules":[{"field":"yeer","op":"is","value":1}]}"#), "match.rules[1].rules[0].field", UnknownField),
            (rule(r#"{"op":"is","value":1,"field":5}"#), "match.rules[0].field", NoField),
            (rule(r#"{"field":"year","value":1}"#), "match.rules[0].op", NoOperator),
            (rule(r#"{"field":"year","op":"contains","value":"19"}"#), "match.rules[0].op", WrongOperator),
            (rule(r#"{"field":"year","op":"roughly","value":1}"#), "match.rules[0].op", WrongOperator),
            (rule(r#"{"field":"starred","op":"is","value":true}"#), "match.rules[0].op", WrongOperator),
            (rule(r#"{"field":"year","op":"is","value":"soon"}"#), "match.rules[0].value", NotNumber),
            (rule(r#"{"field":"year","op":"is"}"#), "match.rules[0].value", NoValue),
            (rule(r#"{"field":"year","op":"between","value":[1]}"#), "match.rules[0].value", NoValue),
            (rule(r#"{"field":"year","op":"between","value":[1,"x"]}"#), "match.rules[0].value[1]", NotNumber),
            (rule(r#"{"field":"year","op":"between","value":[2000,1990]}"#), "match.rules[0].value", Backwards),
            (rule(r#"{"field":"added","op":"between","value":["2024-02-01","2024-01-01"]}"#), "match.rules[0].value", Backwards),
            (rule(r#"{"field":"title","op":"is","value":5}"#), "match.rules[0].value", NotText),
            (rule(r#"{"field":"starred","op":"isTrue","value":true}"#), "match.rules[0].value", TakesNoValue),
            (rule(r#"{"field":"added","op":"greater","value":"last week"}"#), "match.rules[0].value", NotDate),
            (rule(r#"{"field":"added","op":"greater","value":"2024-13-01"}"#), "match.rules[0].value", NotDate),
            (rule(r#"{"field":"added","op":"greater","value":"2024-12-01T25:00:00"}"#), "match.rules[0].value", NotDate),
            (rule(r#"{"field":"lastPlayed","op":"withinDays","value":-1}"#), "match.rules[0].value", DaysOutOfRange),
            (rule(r#"{"field":"year","op":"is","value":1,"extra":1}"#), "match.rules[0].extra", UnknownKey),
            (rule("5"), "match.rules[0]", NotObject),
            (r#"{"match":{"all":"yes","rules":[]}}"#.into(), "match.all", NotFlag),
            (r#"{"match":{"rules":{}}}"#.into(), "match.rules", NotList),
            (r#"{"sort":{"field":"isDownloaded"}}"#.into(), "sort.field", CannotSort),
            (r#"{"sort":{}}"#.into(), "sort.field", NoField),
            (r#"{"sort":[]}"#.into(), "sort", NotObject),
            (r#"{"sort":{"field":"year","descending":1}}"#.into(), "sort.descending", NotFlag),
            (r#"{"sort":{"field":"random","seed":"x"}}"#.into(), "sort.seed", NotNumber),
            (r#"{"limit":-1}"#.into(), "limit", Negative),
            (format!(r#"{{"match":{deep}}}"#), "match.rules[0].rules[0].rules[0].rules[0].rules[0].rules[0].rules[0]", TooDeep),
        ];
        for (json, path, problem) in cases {
            match smart_validate(json.clone()) {
                Err(CoreError::Smart { path: p, problem: q }) => assert_eq!((p.as_str(), q), (path, problem), "{json}"),
                other => panic!("{json}: {other:?}"),
            }
        }
        // Invalid definitions are refused by every entry point.
        let core = Core::new(String::new(), "t".into()).unwrap();
        assert!(matches!(core.smart_evaluate("{".into(), 0, 1), Err(CoreError::Smart { .. })));
        assert!(matches!(core.smart_count("{".into()), Err(CoreError::Smart { .. })));
        assert!(matches!(core.smart_save(String::new(), "x".into(), "{".into()), Err(CoreError::Smart { .. })));
        assert!(core.smart_list().unwrap().is_empty());
    }

    #[test]
    fn save_list_delete() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let def = one("genre", "is", json!("Jazz"));
        let a = core.smart_save(String::new(), "Jazz".into(), def.clone()).unwrap();
        let b = core.smart_save(String::new(), "Ünïcödé ✓".into(), "{}".into()).unwrap();
        assert!(a.starts_with("sp-") && a != b);
        assert_eq!(core.smart_save(a.clone(), "Jazz!".into(), def.clone()).unwrap(), a);
        let l = core.smart_list().unwrap();
        assert_eq!(l.len(), 2);
        assert!(l.contains(&SmartPlaylist { id: a.clone(), name: "Jazz!".into(), json: def, builtin: None }));
        assert!(l.iter().any(|p| p.name == "Ünïcödé ✓"));
        core.smart_delete(a).unwrap();
        core.smart_delete("missing".into()).unwrap();
        assert_eq!(core.smart_list().unwrap().len(), 1);
    }

    #[test]
    fn played_rules_scan_stats_first() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let c = core.db.lock();
        let plan = |json: &str| -> String {
            let def = parse(json).unwrap();
            let mut cp = Compiler { args: Vec::new(), downloaded: &[], downloaded_arg: None, now_ms: NOW, needs_rust: false };
            let cond = cp.node(&def.root);
            let sql = format!("EXPLAIN QUERY PLAN SELECT i.rowid {} WHERE {} AND {cond}", tables(&def.root), mixes::SONGS);
            let mut st = c.prepare(&sql).unwrap();
            let rows = st.query_map(rusqlite::params_from_iter(cp.args), |r| r.get::<_, String>(3)).unwrap();
            rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("\n")
        };
        let defaults = smart_defaults();
        let of = |id: &str| defaults.iter().find(|d| d.id == id).unwrap().json.clone();
        for id in ["default-most-played", "default-recently-played"] {
            let p = plan(&of(id));
            assert!(p.starts_with("SCAN s") || p.starts_with("SEARCH s "), "{id}: {p}");
        }
        assert!(plan(&one("genre", "is", json!("rock"))).contains("items_genre"));
        assert!(plan(&of("default-forgotten-favourites")).contains("items_starred"));
    }

    #[test]
    fn large_library_pages_counts_and_fills_time_budget() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        {
            let mut c = core.db.lock();
            let tx = c.transaction().unwrap();
            {
                let mut st = tx.prepare("INSERT INTO items(server, kind, id, json) VALUES(sid(), 2, ?1, ?2)").unwrap();
                for i in 0..10_000u32 {
                    let s = Song {
                        id: format!("s{i}"),
                        title: format!("Title {i}"),
                        artist: format!("Artist {}", i % 3000),
                        genre: Some(["Rock", "Jazz", "Folk", "Ambient"][(i % 4) as usize].into()),
                        year: 1960 + i % 60,
                        duration: 120 + i % 400,
                        starred: i % 100 == 0,
                        ..Default::default()
                    };
                    st.execute(params![s.id, serde_json::to_string(&s).unwrap()]).unwrap();
                }
            }
            tx.commit().unwrap();
        }
        let def = json!({ "match": { "rules": [
            { "field": "genre", "op": "is", "value": "jazz" }, { "field": "year", "op": "between", "value": [1990, 1999] },
            { "field": "title", "op": "contains", "value": "7" } ] },
            "sort": { "field": "duration", "descending": true }, "limit": 100 })
        .to_string();
        let first = core.smart_evaluate(def.clone(), 0, 50).unwrap();
        assert_eq!(first.len(), 50);
        assert!(first.windows(2).all(|w| w[0].duration >= w[1].duration));
        assert!(first.iter().all(|s| s.genre.as_deref() == Some("Jazz") && (1990..2000).contains(&s.year) && s.title.contains('7')));
        let last = core.smart_evaluate(def.clone(), 80, 50).unwrap();
        assert_eq!(last.len(), 20, "ends at the limit of 100");
        assert_eq!(core.smart_count(def).unwrap(), 100, "225 match, capped");

        let starred = json!({ "match": { "rules": [{ "field": "starred", "op": "isTrue" }] }, "sort": { "field": "random", "seed": 7 } }).to_string();
        assert_eq!(core.smart_count(starred.clone()).unwrap(), 100);
        assert_eq!(core.smart_evaluate(starred, 90, 50).unwrap().len(), 10);

        let budget = json!({ "sort": { "field": "random", "seed": 3 }, "limitMs": 3_600_000 }).to_string();
        let hour = core.smart_evaluate(budget.clone(), 0, 1000).unwrap();
        let total: u32 = hour.iter().map(|s| s.duration).sum();
        assert!(total <= 3600 && total > 3600 - 520, "{total} s in {} songs", hour.len());
        assert_eq!(core.smart_count(budget).unwrap() as usize, hour.len());

        let unicode = json!({ "match": { "rules": [{ "field": "artist", "op": "is", "value": "ärtist 5" }] }, "limit": 10 }).to_string();
        assert!(core.smart_evaluate(unicode, 0, 10).unwrap().is_empty());
    }

    #[test]
    fn added_uses_server_date() {
        let core = library();
        assert_eq!(eval(&core, &one("added", "withinDays", json!(30)), &[]), ["dogs"]);
        assert_eq!(eval(&core, &one("added", "notWithinDays", json!(30)), &[]).len(), 6, "no date counts as not within");
        assert_eq!(eval(&core, &one("added", "greater", json!("2024-02-29")), &[]), ["dogs"], "after the whole day");
        assert_eq!(eval(&core, &one("added", "greater", json!("2024-02-29T23:00:00")), &[]), ["dogs", "so"]);
        assert_eq!(eval(&core, &one("added", "between", json!(["2024-02-01", "2024-02-29"])), &[]), ["so"], "end day inclusive");
        assert_eq!(eval(&core, &one("added", "less", json!("2025-01-01")), &[]).len(), 6);
        let newest = json!({ "sort": { "field": "added", "descending": true }, "limit": 2 }).to_string();
        assert_eq!(eval(&core, &newest, &[]), ["dogs", "so"]);
    }

    #[test]
    fn built_in_definitions_validate_and_filter() {
        let core = library();
        let defaults = smart_defaults();
        let run = |id: &str| {
            let d = defaults.iter().find(|d| d.id == id).unwrap();
            smart_validate(d.json.clone()).unwrap();
            eval(&core, &d.json, &[])
        };
        assert_eq!(run("default-most-played"), ["dogs", "so"]);
        assert_eq!(run("default-recently-played"), ["dogs"]);
        assert_eq!(run("default-recently-added"), ["dogs"]);
        assert_eq!(run("default-never-played").len(), 5);
        assert_eq!(run("default-top-rated"), ["pigs", "so"]);
        assert_eq!(run("default-forgotten-favourites"), ["so"]);
        assert_eq!(run("default-long-tracks"), ["dogs"]);
    }

    #[test]
    fn nested_groups() {
        let core = library();
        let def = json!({ "match": { "all": false, "rules": [
            { "all": true, "rules": [
                { "field": "genre", "op": "contains", "value": "rock" },
                { "all": false, "rules": [{ "field": "starred", "op": "isTrue" }, { "field": "userRating", "op": "is", "value": 5 }] } ] },
            { "all": true, "rules": [
                { "field": "genre", "op": "is", "value": "jazz" },
                { "field": "lastPlayed", "op": "notWithinDays", "value": 30 } ] } ] } })
        .to_string();
        assert_eq!(eval(&core, &def, &[]), ["dogs", "pigs", "so"]);
        assert_eq!(eval(&core, r#"{"match":{"all":true,"rules":[]}}"#, &[]).len(), 7);
        assert!(eval(&core, r#"{"match":{"all":false,"rules":[]}}"#, &[]).is_empty());
        assert_eq!(eval(&core, "{}", &[]).len(), 7);
        assert_eq!(eval(&core, r#"{"match":{"rules":[{"all":false,"rules":[]},{"field":"year","op":"is","value":1977}]}}"#, &[]).len(), 0);
    }

    #[test]
    fn number_operators() {
        let core = library();
        assert_eq!(eval(&core, &one("year", "is", json!(1977)), &[]), ["dogs", "pigs"]);
        assert_eq!(eval(&core, &one("year", "isNot", json!(1977)), &[]).len(), 5);
        assert_eq!(eval(&core, &one("year", "between", json!([1959, 1977])), &[]), ["dogs", "pigs", "so"]);
        assert_eq!(eval(&core, &one("year", "greater", json!("1994")), &[]), ["joga", "bach"], "numeric string");
        assert_eq!(eval(&core, &one("duration", "greater", json!(600)), &[]), ["dogs"]);
        assert_eq!(eval(&core, &one("bitRate", "is", json!(320)), &[]), ["joga"]);
        assert_eq!(eval(&core, &one("sampleRate", "greater", json!(48_000)), &[]), ["so"]);
        assert_eq!(eval(&core, &one("bitDepth", "is", json!(24)), &[]), ["so"]);
        assert_eq!(eval(&core, &one("size", "greater", json!(100_000_000)), &[]), ["so"]);
        assert_eq!(eval(&core, &one("userRating", "greater", json!(3)), &[]), ["pigs", "so"]);
        assert_eq!(eval(&core, &one("track", "is", json!(0)), &[]).len(), 7);
        assert_eq!(eval(&core, &one("discNumber", "less", json!(1)), &[]).len(), 7);
    }

    #[test]
    fn statistics_fields() {
        let core = library();
        assert_eq!(eval(&core, &one("playCount", "greater", json!(0)), &[]), ["dogs", "so"]);
        assert_eq!(eval(&core, &one("playCount", "is", json!(3)), &[]), ["dogs"]);
        assert_eq!(eval(&core, &one("playCount", "is", json!(0)), &[]), ["pigs", "joga", "bach", "pct", "bare"]);
        assert_eq!(eval(&core, &one("playCount", "between", json!([1, 2])), &[]), ["so"]);
        assert_eq!(eval(&core, &one("playCount", "between", json!([0, 1])), &[]).len(), 6);
        assert_eq!(eval(&core, &one("playCount", "less", json!(1)), &[]).len(), 5);
        assert_eq!(eval(&core, &one("skipCount", "greater", json!(1)), &[]), ["pct"]);
        assert_eq!(eval(&core, &one("serverPlayCount", "greater", json!(1)), &[]), ["dogs", "so"]);
        assert_eq!(eval(&core, &one("serverPlayCount", "is", json!(0)), &[]).len(), 5, "missing key is 0");
        assert_eq!(eval(&core, &one("lastPlayed", "withinDays", json!(7)), &[]), ["dogs"]);
        assert_eq!(eval(&core, &one("lastPlayed", "withinDays", json!(365)), &[]), ["dogs", "so"]);
        assert_eq!(eval(&core, &one("lastPlayed", "notWithinDays", json!(7)), &[]).len(), 6, "never played counts");
        assert_eq!(eval(&core, &one("lastPlayed", "greater", json!("2026-08-01")), &[]), ["dogs"]);
        assert_eq!(eval(&core, &one("lastPlayed", "less", json!("2026-08-01")), &[]).len(), 6);
        assert_eq!(eval(&core, &one("lastPlayed", "between", json!(["2026-01-01", "2026-03-01"])), &[]), ["so"]);
    }

    #[test]
    fn text_operators() {
        let core = library();
        assert_eq!(eval(&core, &one("artist", "is", json!("pink floyd")), &[]), ["dogs", "pigs"]);
        assert_eq!(eval(&core, &one("artist", "isNot", json!("Pink Floyd")), &[]).len(), 5);
        assert_eq!(eval(&core, &one("genre", "contains", json!("rock")), &[]), ["dogs", "pigs"]);
        assert_eq!(eval(&core, &one("genre", "notContains", json!("rock")), &[]), ["joga", "bach", "so", "pct", "bare"], "no genre counts");
        assert_eq!(eval(&core, &one("title", "startsWith", json!("PIGS")), &[]), ["pigs"]);
        assert_eq!(eval(&core, &one("title", "endsWith", json!("ones)")), &[]), ["pigs"]);
        assert_eq!(eval(&core, &one("genre", "is", json!("")), &[]), ["bare"]);
        assert_eq!(eval(&core, &one("genre", "isNot", json!("")), &[]).len(), 6);
        assert_eq!(eval(&core, &one("suffix", "is", json!("MP3")), &[]), ["joga"]);
        // LIKE wildcards are literal.
        assert_eq!(eval(&core, &one("title", "contains", json!("100%")), &[]), ["pct"]);
        assert_eq!(eval(&core, &one("title", "contains", json!("e_l")), &[]), ["pct"]);
        assert_eq!(eval(&core, &one("title", "contains", json!("%")), &[]), ["pct"]);
        assert!(eval(&core, &one("title", "contains", json!("'; DROP TABLE items;--")), &[]).is_empty());
    }
}
