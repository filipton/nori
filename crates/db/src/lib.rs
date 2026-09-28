//! Local index: every library item the app has seen, searchable offline with
//! FTS5, plus the response cache and the small persistent queues. Items are
//! written here straight from the response bytes, so a library sync never
//! materialises objects on the Kotlin side. The core's background thread, which writes it off the
//! caller's thread, is background.rs.

pub mod background;

use std::sync::{Arc, Weak};

use nori_model::*;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};

pub const ARTIST: i64 = 0;
pub const ALBUM: i64 = 1;
pub const SONG: i64 = 2;

/// One database for the whole app. What belongs to one server - its library, answers, pending writes,
/// downloads, history and the like - carries the server profile's id in `server`, and a connection
/// reads and writes only its own server's rows through `sid()`, the id it was opened for. The equalizer
/// profiles, the AutoEQ index and the settings are the app's, whatever the server.
const PRAGMAS: &str = "
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA temp_store=MEMORY;
PRAGMA busy_timeout=5000;
";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS items(rowid INTEGER PRIMARY KEY, server TEXT NOT NULL, kind INTEGER NOT NULL, id TEXT NOT NULL, json TEXT NOT NULL, UNIQUE(server, kind, id));
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(text, tokenize='unicode61 remove_diacritics 2', prefix='2 3');
CREATE TABLE IF NOT EXISTS cache(server TEXT NOT NULL, key TEXT NOT NULL, body BLOB NOT NULL, ts INTEGER NOT NULL, PRIMARY KEY(server, key)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS kv(server TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(server, key)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS pending(rowid INTEGER PRIMARY KEY, server TEXT NOT NULL, endpoint TEXT NOT NULL, params TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS downloads(server TEXT NOT NULL, id TEXT NOT NULL, json TEXT NOT NULL, ts INTEGER NOT NULL, done INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(server, id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS download_beats(server TEXT NOT NULL, id TEXT NOT NULL, PRIMARY KEY(server, id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS autoeq(rowid INTEGER PRIMARY KEY, name TEXT NOT NULL, source TEXT NOT NULL, form TEXT NOT NULL, target TEXT NOT NULL, path TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS autoeq_missing(path TEXT PRIMARY KEY) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS profiles(name TEXT PRIMARY KEY, json TEXT NOT NULL, outputs TEXT NOT NULL DEFAULT '') WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS app_kv(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS searches(server TEXT NOT NULL, query TEXT NOT NULL, ts INTEGER NOT NULL, PRIMARY KEY(server, query)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS plays(rowid INTEGER PRIMARY KEY, server TEXT NOT NULL, song_id TEXT NOT NULL, started_ms INTEGER NOT NULL, heard_ms INTEGER NOT NULL, duration_ms INTEGER NOT NULL, completed INTEGER NOT NULL, skipped INTEGER NOT NULL, hour INTEGER NOT NULL, day INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS plays_started ON plays(server, started_ms);
CREATE TABLE IF NOT EXISTS song_stats(server TEXT NOT NULL, song_id TEXT NOT NULL, plays INTEGER NOT NULL DEFAULT 0, skips INTEGER NOT NULL DEFAULT 0, last_played_ms INTEGER NOT NULL DEFAULT 0, heard_ms_total INTEGER NOT NULL DEFAULT 0, taste REAL NOT NULL DEFAULT 0, PRIMARY KEY(server, song_id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS mix_excluded(server TEXT NOT NULL, song_id TEXT NOT NULL, PRIMARY KEY(server, song_id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS autofill_picks(server TEXT NOT NULL, kind INTEGER NOT NULL, id TEXT NOT NULL, picked_ms INTEGER NOT NULL, PRIMARY KEY(server, kind, id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS smart_playlists(server TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL, json TEXT NOT NULL, updated_ms INTEGER NOT NULL, PRIMARY KEY(server, id)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS items_genre ON items(server, json_extract(json,'$.genre') COLLATE NOCASE) WHERE kind=2;
CREATE INDEX IF NOT EXISTS items_artist ON items(server, json_extract(json,'$.artistId')) WHERE kind=2;
CREATE INDEX IF NOT EXISTS items_year ON items(server, json_extract(json,'$.year')) WHERE kind=2;
CREATE INDEX IF NOT EXISTS items_starred ON items(server, json_extract(json,'$.starred')) WHERE kind=2 AND json_extract(json,'$.starred')=1;
CREATE INDEX IF NOT EXISTS items_rated ON items(server, json_extract(json,'$.userRating')) WHERE kind=2 AND json_extract(json,'$.userRating')>=4;
CREATE TABLE IF NOT EXISTS track_analysis(server TEXT NOT NULL, song_id TEXT NOT NULL, analysis_version INTEGER NOT NULL, duration_ms INTEGER NOT NULL, bpm REAL NOT NULL, bpm_confidence REAL NOT NULL, beat_offset_ms REAL NOT NULL, stability REAL NOT NULL, downbeat_phase INTEGER NOT NULL, downbeat_confidence REAL NOT NULL, lufs REAL NOT NULL, key INTEGER NOT NULL, key_confidence REAL NOT NULL, silence_start_ms INTEGER NOT NULL, silence_end_ms INTEGER NOT NULL, mixramp_start_ms INTEGER NOT NULL, mixramp_end_ms INTEGER NOT NULL, intro_end_ms INTEGER NOT NULL, outro_start_ms INTEGER NOT NULL, outro_vocal REAL NOT NULL, intro_vocal REAL NOT NULL, outro_centroid REAL NOT NULL, intro_centroid REAL NOT NULL, outro_bpm REAL NOT NULL, outro_bpm_confidence REAL NOT NULL, outro_beat_offset_ms REAL NOT NULL, outro_stability REAL NOT NULL, outro_downbeat_phase INTEGER NOT NULL, intro_bpm REAL NOT NULL, intro_bpm_confidence REAL NOT NULL, intro_beat_offset_ms REAL NOT NULL, intro_stability REAL NOT NULL, intro_downbeat_phase INTEGER NOT NULL, beats_per_bar INTEGER NOT NULL DEFAULT 0, drop_ms INTEGER NOT NULL DEFAULT 0, drop_runup_vocal REAL NOT NULL DEFAULT 0, drop_vocal REAL NOT NULL DEFAULT 0, exit_ms INTEGER NOT NULL DEFAULT 0, gap_ms INTEGER NOT NULL DEFAULT 0, gap_end_ms INTEGER NOT NULL DEFAULT 0, exit_vocal REAL NOT NULL DEFAULT 0, drop_runup_tonal_db REAL NOT NULL DEFAULT 0, intro_beats_per_bar INTEGER NOT NULL DEFAULT 0, outro_beats_per_bar INTEGER NOT NULL DEFAULT 0, intro_grid_source INTEGER NOT NULL DEFAULT 0, outro_grid_source INTEGER NOT NULL DEFAULT 0, analysed_ms INTEGER NOT NULL, PRIMARY KEY(server, song_id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS vocal_curve(server TEXT NOT NULL, song_id TEXT NOT NULL, curve BLOB NOT NULL, PRIMARY KEY(server, song_id)) WITHOUT ROWID;
";

/// The tables that belong to one server: what goes when its profile is removed.
const SERVER_TABLES: [&str; 14] = [
    "items", "cache", "kv", "pending", "downloads", "download_beats", "searches", "plays", "song_stats", "mix_excluded", "autofill_picks", "smart_playlists", "track_analysis",
    "vocal_curve",
];

/// The app's database, opened for `server`'s rows.
pub fn open(path: &str, server: &str) -> rusqlite::Result<Connection> {
    let c = if path.is_empty() { Connection::open_in_memory()? } else { Connection::open(path)? };
    c.execute_batch(PRAGMAS)?;
    let sid = server.to_string();
    c.create_scalar_function("sid", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC, move |_| {
        Ok(sid.clone())
    })?;
    drop_old_analysis(&c)?;
    c.execute_batch(SCHEMA)?;
    drop_old_lyrics(&c)?;
    Ok(c)
}

/// Lyrics kept from before every word timing was (LRC, under the old key `lrclib2|`) are not read any more:
/// this server's go as its rows are opened. The range is the key's own, so it costs a look at the index.
fn drop_old_lyrics(c: &Connection) -> rusqlite::Result<()> {
    c.execute("DELETE FROM cache WHERE server=sid() AND key >= 'lrclib2|' AND key < 'lrclib2|' || x'ff'", [])?;
    Ok(())
}

/// Song analyses are a cache: an older table layout is not carried over but dropped, and the songs are
/// measured again as they come up. The last column the layout gained says whether it is current.
fn drop_old_analysis(c: &Connection) -> rusqlite::Result<()> {
    let have: Vec<String> = c.prepare("PRAGMA table_info(track_analysis)")?.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
    if !have.is_empty() && !have.iter().any(|h| h == "outro_grid_source") {
        c.execute_batch("DROP TABLE track_analysis")?;
    }
    Ok(())
}

/// The app's database for what is the app's alone (the settings and the app's own values), with no server.
pub fn open_app(path: &str) -> rusqlite::Result<Connection> {
    let c = if path.is_empty() { Connection::open_in_memory()? } else { Connection::open(path)? };
    c.execute_batch(PRAGMAS)?;
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS app_kv(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;",
    )?;
    Ok(c)
}

/// The database of the core the app is using now, for the parts that run without one handed to them (the
/// transition planner on the audio thread, analyses finished in the background, downloads followed from
/// the platform). The core holds it; this only keeps it while the core does.
static ACTIVE: Mutex<Weak<Mutex<Connection>>> = Mutex::new(Weak::new());

/// `db` is the database of the core the app uses from now on: the newest one made.
pub fn set_active(db: &Arc<Mutex<Connection>>) {
    *ACTIVE.lock() = Arc::downgrade(db);
}

/// The database of the core the app is using now, if there is one.
pub fn active() -> Option<Arc<Mutex<Connection>>> {
    ACTIVE.lock().upgrade()
}

/// The app's one database file; every server profile has its rows in it.
pub const DB_FILE: &str = "nori.db";

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn db_file_name() -> String {
    DB_FILE.into()
}

/// A removed server profile's rows gone from the app's database at `db_path`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn db_forget_server(db_path: String, server: String) -> nori_model::Result<()> {
    Ok(forget_server(&open_app(&db_path)?, &server)?)
}

/// The rows of one server gone, for a server profile that was removed.
pub fn forget_server(c: &Connection, server: &str) -> rusqlite::Result<()> {
    c.execute("DELETE FROM fts WHERE rowid IN (SELECT rowid FROM items WHERE server=?1)", [server])?;
    for t in SERVER_TABLES {
        c.execute(&format!("DELETE FROM {t} WHERE server=?1"), [server])?;
    }
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Provider items from octo-fiesta are not library rows: they change id once
/// downloaded, so they are never indexed.
pub fn external(id: &str) -> bool {
    id.starts_with("ext-") || id.starts_with("pl-")
}

fn upsert<T: Serialize>(c: &Connection, kind: i64, id: &str, text: &str, item: &T) -> rusqlite::Result<bool> {
    if id.is_empty() || external(id) {
        return Ok(false);
    }
    let json = serde_json::to_string(item).unwrap_or_default();
    let old: Option<(i64, String)> = c
        .prepare_cached("SELECT rowid, json FROM items WHERE server=sid() AND kind=?1 AND id=?2")?
        .query_row(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    match old {
        Some((_, j)) if j == json => Ok(false),
        Some((rowid, _)) => {
            c.prepare_cached("UPDATE items SET json=?1 WHERE rowid=?2")?.execute(params![json, rowid])?;
            c.prepare_cached("INSERT OR REPLACE INTO fts(rowid, text) VALUES(?1, ?2)")?.execute(params![rowid, text])?;
            Ok(true)
        }
        None => {
            c.prepare_cached("INSERT INTO items(server, kind, id, json) VALUES(sid(), ?1, ?2, ?3)")?.execute(params![kind, id, json])?;
            let rowid = c.last_insert_rowid();
            c.prepare_cached("INSERT INTO fts(rowid, text) VALUES(?1, ?2)")?.execute(params![rowid, text])?;
            Ok(true)
        }
    }
}

pub fn index(c: &mut Connection, artists: &[Artist], albums: &[Album], songs: &[Song]) -> rusqlite::Result<IngestStats> {
    let tx = c.transaction()?;
    let mut st = IngestStats::default();
    for a in artists {
        st.artists += upsert(&tx, ARTIST, &a.id, &a.name, a)? as u32;
    }
    for a in albums {
        if a.is_external {
            continue;
        }
        st.albums += upsert(&tx, ALBUM, &a.id, &format!("{} {}", a.name, a.artist), a)? as u32;
    }
    for s in songs {
        if s.is_external {
            continue;
        }
        st.songs += upsert(&tx, SONG, &s.id, &format!("{} {} {}", s.title, s.artist, s.album), s)? as u32;
    }
    tx.commit()?;
    Ok(st)
}

/// Every token is a prefix match and all must match: "pin flo" finds Pink Floyd.
fn fts_query(q: &str) -> Option<String> {
    let toks: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    (!toks.is_empty()).then(|| toks.join(" "))
}

fn find<T: DeserializeOwned>(c: &Connection, kind: i64, q: &str, limit: u32) -> rusqlite::Result<Vec<T>> {
    let mut st = c.prepare_cached(
        "SELECT i.json FROM fts JOIN items i ON i.rowid = fts.rowid WHERE fts MATCH ?1 AND i.server = sid() AND i.kind = ?2 ORDER BY rank LIMIT ?3",
    )?;
    let rows = st.query_map(params![q, kind, limit], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|j| serde_json::from_str(&j.ok()?).ok()).collect())
}

pub fn search(c: &Connection, query: &str, limit: u32) -> rusqlite::Result<SearchResult> {
    let Some(q) = fts_query(query) else { return Ok(SearchResult::default()) };
    Ok(SearchResult {
        artists: find(c, ARTIST, &q, limit)?,
        albums: find(c, ALBUM, &q, limit)?,
        songs: find(c, SONG, &q, limit)?,
    })
}

pub fn count(c: &Connection, kind: i64) -> rusqlite::Result<u32> {
    c.query_row("SELECT count(*) FROM items WHERE server=sid() AND kind=?1", [kind], |r| r.get(0))
}

pub fn clear_library(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(
        "DELETE FROM fts WHERE rowid IN (SELECT rowid FROM items WHERE server=sid()); DELETE FROM items WHERE server=sid();
         DELETE FROM cache WHERE server=sid(); DELETE FROM pending WHERE server=sid(); DELETE FROM kv WHERE server=sid() AND key='queue';",
    )
}

pub fn kv_get(c: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    c.prepare_cached("SELECT value FROM kv WHERE server=sid() AND key=?1")?.query_row([key], |r| r.get(0)).optional()
}

pub fn kv_put(c: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    c.prepare_cached("INSERT OR REPLACE INTO kv(server, key, value) VALUES(sid(), ?1, ?2)")?.execute(params![key, value]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(c: &mut Connection, id: &str, title: &str) {
        let s: Song = serde_json::from_str(&format!(r#"{{"id":"{id}","title":"{title}"}}"#)).unwrap();
        index(c, &[], &[], &[s]).unwrap();
    }

    #[test]
    fn an_analysis_table_of_an_older_layout_is_made_again_not_carried_over() {
        let dir = nori_testdir::TempDir::new("db-old");
        let path = dir.join("old.db").to_string_lossy().into_owned();
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE track_analysis(server TEXT NOT NULL, song_id TEXT NOT NULL, bpm REAL NOT NULL, PRIMARY KEY(server, song_id)) WITHOUT ROWID; INSERT INTO track_analysis VALUES('s','a',120);").unwrap();
        }
        let c = open(&path, "s").unwrap();
        let cols: Vec<String> = c.prepare("PRAGMA table_info(track_analysis)").unwrap().query_map([], |r| r.get::<_, String>(1)).unwrap().map(|c| c.unwrap()).collect();
        assert!(cols.iter().any(|c| c == "outro_grid_source"), "the current layout");
        let rows: i64 = c.query_row("SELECT count(*) FROM track_analysis", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0, "nothing carried over: the songs are measured again");
    }

    #[test]
    fn lyrics_kept_under_the_old_key_go_as_a_server_is_opened_and_nothing_else_does() {
        let dir = nori_testdir::TempDir::new("db-lyrics");
        let path = dir.join("lyrics.db").to_string_lossy().into_owned();
        {
            let c = open(&path, "s").unwrap();
            c.execute_batch(
                "INSERT INTO cache VALUES('s','lrclib2|a',x'00',0), ('s','lrclib3|a',x'00',0), ('s','lrclib2',x'00',0), ('t','lrclib2|a',x'00',0);",
            )
            .unwrap();
        }
        let c = open(&path, "s").unwrap();
        let keys: Vec<(String, String)> = c.prepare("SELECT server, key FROM cache ORDER BY server, key").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
        let kept = |s: &str, k: &str| (s.to_string(), k.to_string());
        assert_eq!(keys, [kept("s", "lrclib2"), kept("s", "lrclib3|a"), kept("t", "lrclib2|a")], "only this server's old lyrics went");
    }

    #[test]
    fn each_server_reads_its_own_rows_and_forgetting_one_leaves_the_rest() {
        let dir = nori_testdir::TempDir::new("one-db");
        let path = dir.join("nori.db").display().to_string();

        let mut def = open(&path, "default").unwrap();
        let mut x1 = open(&path, "x1").unwrap();
        song(&mut def, "a1", "Airbag");
        song(&mut x1, "b1", "Bones");
        assert_eq!(search(&def, "airbag", 10).unwrap().songs.len(), 1);
        assert!(search(&def, "bones", 10).unwrap().songs.is_empty(), "another server's songs are not this one's");
        assert_eq!(search(&x1, "bones", 10).unwrap().songs[0].id, "b1");

        forget_server(&def, "x1").unwrap();
        assert!(search(&x1, "bones", 10).unwrap().songs.is_empty());
        assert_eq!(search(&def, "airbag", 10).unwrap().songs.len(), 1);
        drop((def, x1));
        assert_eq!(search(&open(&path, "default").unwrap(), "airbag", 10).unwrap().songs.len(), 1, "opened again, nothing lost");
    }
}
