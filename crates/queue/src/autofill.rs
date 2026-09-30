//! Refilling the queue past its end. When and from which seed to fetch, and a next pressed while the
//! fetch is in flight, are `nori_player::queue::Refill`'s; the platform fetches and appends. Every route
//! reads the library, so a provider track is never downloaded.
//!
//! [`rank`] rotates candidates: each pick is stored (`autofill_picks`), and candidates neither picked nor
//! played within [`LATELY_MS`] come first (the first drawn from the top few), the rest by least recent use.

use std::collections::{HashMap, HashSet};

use nori_library::mixes::Rng;
use nori_model::Song;
use rusqlite::{params, Connection};
use nori_net::transport::NetError;
use nori_player::queue::{refillable, shuffle, Refill};
use parking_lot::Mutex;

use crate::queue;

/// Songs per fill.
pub const SONGS: usize = 15;
/// Candidate albums tried before settling for a short one.
pub const ALBUM_TRIES: usize = 6;
/// Albums shorter than this are singles, taken only as a last resort.
pub const ALBUM_MIN: usize = 3;
/// Whole albums one "shuffle albums" refill adds.
pub const RANDOM_ALBUMS: i32 = 3;

/// A server fetch result.
pub type Got<T> = Result<T, NetError>;

// Global: the uniffi entry points have no handle.
static REFILL: Mutex<Refill> = Mutex::new(Refill::new());

/// Monotonic ms since first use.
fn mono_ms() -> i64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(std::time::Instant::now).elapsed().as_millis() as i64
}

/// The queue's last song in play order.
fn end_of(p: &nori_player::playlist::Playlist) -> Option<String> {
    let last = match p.shuffle_order() {
        Some(order) => order.last().copied(),
        None => p.len().checked_sub(1),
    };
    last.and_then(|i| p.ids().get(i).cloned())
}

/// The refill seed: the queue's last song in play order.
pub fn autofill_seed() -> Option<String> {
    crate::playlist::with(end_of)
}

/// Whether refilling is on: the autoplay setting, or always for a shuffle-started queue.
fn refill_on() -> bool {
    refills(crate::playlist::playlist_origin().map(|o| o.kind), crate::rules::prefs(|p| p.auto_fill))
}

fn refills(origin: Option<nori_model::OriginKind>, auto_fill: bool) -> bool {
    auto_fill || matches!(origin, Some(nori_model::OriginKind::ShuffleSongs | nori_model::OriginKind::ShuffleAlbums))
}

/// (may refill now, songs after the current one, last song).
fn refill_facts() -> (bool, usize, Option<String>) {
    let setting = refill_on();
    crate::playlist::with(|p| {
        let cur = p.current_id();
        (refillable(cur.is_some(), cur.is_some_and(|c| c.starts_with(queue::RADIO_PREFIX)), p.repeat(), setting), p.songs_after(), end_of(p))
    })
}

/// The queue moved: whether to fetch songs for its end now. True means a fetch is in flight until
/// [`autofill_arrived`].
pub(crate) fn autofill_start() -> bool {
    let (ok, after, end) = refill_facts();
    REFILL.lock().start(ok, after, end.as_deref())
}

/// What a next press does now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FillNext {
    Skip,
    /// Nothing after: fetch; the skip happens in [`autofill_landed`] if the songs land soon enough.
    Fetch,
    /// A fetch is already in flight, or the queue cannot be refilled.
    Wait,
}

/// Next pressed near the queue's end.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn autofill_next() -> FillNext {
    autofill_next_at(mono_ms())
}

fn autofill_next_at(now_ms: i64) -> FillNext {
    let setting = refill_on();
    let (has_next, repeat_off, current) =
        crate::playlist::with(|p| (p.next().is_some(), p.repeat() == nori_player::playlist::REPEAT_OFF, p.current_id().map(str::to_string)));
    if REFILL.lock().next(has_next, setting && repeat_off, current.as_deref(), now_ms) {
        return FillNext::Skip;
    }
    if !(setting && repeat_off) {
        return FillNext::Wait;
    }
    if autofill_start() {
        FillNext::Fetch
    } else {
        FillNext::Wait
    }
}

/// The fetch returned `count` songs: whether to append them (`Refill::arrived`). Clients ask through
/// `Client::autofill_arrived`, which also records the picks.
pub fn autofill_arrived(count: u32) -> bool {
    let end = autofill_seed();
    REFILL.lock().arrived(count as usize, end.as_deref())
}

/// The fetched songs are appended: whether to perform a next pressed meanwhile (only on the same song,
/// within `NEXT_KEPT_MS`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn autofill_landed() -> bool {
    autofill_landed_at(mono_ms())
}

fn autofill_landed_at(now_ms: i64) -> bool {
    let (current, has_next) = crate::playlist::with(|p| (p.current_id().map(str::to_string), p.next().is_some()));
    REFILL.lock().landed(current.as_deref(), has_next, now_ms)
}

/// A random seed from the clock.
pub fn seed_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64)
}

/// `v` shuffled with a clock seed.
pub fn shuffled<T>(mut v: Vec<T>) -> Vec<T> {
    shuffle(&mut v, seed_now());
    v
}

/// The decade of `seed`'s year; None without a year.
pub fn era(seed: &Song) -> Option<(u32, u32)> {
    (seed.year > 0).then(|| (seed.year / 10 * 10, seed.year / 10 * 10 + 9))
}

/// Kind of autofill pick (stored as its number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    Album = 0,
    Songs = 1,
}

/// Used within this long counts as recent.
pub(crate) const LATELY_MS: i64 = 14 * 86_400_000;
/// The first pick is drawn from this many top fresh candidates.
const TOP_DRAW: usize = 3;
/// Picks older than this are deleted.
pub(crate) const FORGET_MS: i64 = 90 * 86_400_000;

/// Ranks `candidates` (best first, deduplicated); `used` is each one's last pick or play time.
pub fn rank(candidates: Vec<String>, used: &HashMap<String, i64>, now_ms: i64, seed: u64) -> Vec<String> {
    let mut seen = HashSet::new();
    let (mut fresh, mut stale): (Vec<String>, Vec<String>) =
        candidates.into_iter().filter(|c| seen.insert(c.clone())).partition(|c| used.get(c).is_none_or(|&t| now_ms - t >= LATELY_MS));
    if !fresh.is_empty() {
        let first = (Rng::new(seed).next() % fresh.len().min(TOP_DRAW) as u64) as usize;
        let pick = fresh.remove(first);
        fresh.insert(0, pick);
    }
    // Stable: equal times keep the list's order.
    stale.sort_by_key(|c| used[c]);
    fresh.extend(stale);
    fresh
}

/// Records `at` for `id` if later than the stored time.
fn latest(used: &mut HashMap<String, i64>, id: String, at: i64) {
    let t = used.entry(id).or_insert(at);
    *t = (*t).max(at);
}

/// Adds recent picks of `kind` to `used`.
fn picked(c: &Connection, kind: Picked, now_ms: i64, used: &mut HashMap<String, i64>) -> rusqlite::Result<()> {
    let mut st = c.prepare_cached("SELECT id, picked_ms FROM autofill_picks WHERE server=sid() AND kind=?1 AND picked_ms>=?2")?;
    for row in st.query_map(params![kind as i64, now_ms - LATELY_MS], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, at) = row?;
        latest(used, id, at);
    }
    Ok(())
}

/// Recent use of each album: picked, or one of its songs played.
pub fn album_use(c: &Connection, now_ms: i64) -> rusqlite::Result<HashMap<String, i64>> {
    let mut used = HashMap::new();
    picked(c, Picked::Album, now_ms, &mut used)?;
    let mut st = c.prepare_cached(
        "SELECT json_extract(i.json,'$.albumId'), max(p.started_ms) FROM plays p JOIN items i ON i.server=sid() AND i.kind=2 AND i.id=p.song_id
         WHERE p.server=sid() AND p.started_ms>=?1 AND json_extract(i.json,'$.albumId') IS NOT NULL GROUP BY 1",
    )?;
    for row in st.query_map(params![now_ms - LATELY_MS], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, at) = row?;
        latest(&mut used, id, at);
    }
    Ok(used)
}

/// Recent use of each song: picked or played.
pub fn song_use(c: &Connection, now_ms: i64) -> rusqlite::Result<HashMap<String, i64>> {
    let mut used = HashMap::new();
    picked(c, Picked::Songs, now_ms, &mut used)?;
    let mut st = c.prepare_cached("SELECT song_id, max(started_ms) FROM plays WHERE server=sid() AND started_ms>=?1 GROUP BY song_id")?;
    for row in st.query_map(params![now_ms - LATELY_MS], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, at) = row?;
        latest(&mut used, id, at);
    }
    Ok(used)
}

/// Stores `ids` as just picked and deletes picks older than [`FORGET_MS`].
pub fn note(c: &Connection, kind: Picked, ids: &[String], now_ms: i64) -> rusqlite::Result<()> {
    let mut st = c.prepare_cached(
        "INSERT INTO autofill_picks(server, kind, id, picked_ms) VALUES(sid(), ?1, ?2, ?3) ON CONFLICT(server, kind, id) DO UPDATE SET picked_ms=excluded.picked_ms",
    )?;
    for id in ids.iter().filter(|id| !id.is_empty()) {
        st.execute(params![kind as i64, id, now_ms])?;
    }
    c.prepare_cached("DELETE FROM autofill_picks WHERE server=sid() AND picked_ms<?1")?.execute(params![now_ms - FORGET_MS])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_788_000_000_000;
    const DAY: i64 = 86_400_000;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rank_unused_keeps_order_after_drawn_first() {
        let list = ids(&["a", "b", "c", "d", "e"]);
        for seed in 0..50 {
            let r = rank(list.clone(), &HashMap::new(), NOW, seed);
            assert_eq!(r.len(), 5);
            assert!(["a", "b", "c"].contains(&r[0].as_str()), "{r:?}");
            let rest: Vec<&String> = list.iter().filter(|x| **x != r[0]).collect();
            assert_eq!(r[1..].iter().collect::<Vec<_>>(), rest);
        }
        let leads: HashSet<String> = (0..50).map(|s| rank(list.clone(), &HashMap::new(), NOW, s)[0].clone()).collect();
        assert_eq!(leads.len(), 3);
    }

    #[test]
    fn rank_recent_last_oldest_first() {
        let used = HashMap::from([("a".to_string(), NOW - DAY), ("b".to_string(), NOW - 5 * DAY), ("c".to_string(), NOW - 20 * DAY)]);
        // c is older than LATELY_MS: fresh again.
        let r = rank(ids(&["a", "b", "c", "d"]), &used, NOW, 1);
        assert!(["c", "d"].contains(&r[0].as_str()));
        assert_eq!(r[2..], ids(&["b", "a"]));
        let all = HashMap::from([("a".to_string(), NOW - DAY), ("b".to_string(), NOW - 3 * DAY)]);
        assert_eq!(rank(ids(&["a", "b"]), &all, NOW, 7), ids(&["b", "a"]));
    }

    #[test]
    fn rank_dedups() {
        assert!(rank(Vec::new(), &HashMap::new(), NOW, 3).is_empty());
        let r = rank(ids(&["a", "a", "b"]), &HashMap::from([("a".to_string(), NOW)]), NOW, 3);
        assert_eq!(r, ids(&["b", "a"]));
    }

    #[test]
    fn album_picks_rotate() {
        let c = nori_db::open("", "t").unwrap();
        let records = ids(&["al-1", "al-2", "al-3"]);
        let mut order = Vec::new();
        for _ in 0..3 {
            let first = rank(records.clone(), &album_use(&c, NOW).unwrap(), NOW, 0)[0].clone();
            note(&c, Picked::Album, std::slice::from_ref(&first), NOW).unwrap();
            order.push(first);
        }
        let distinct: HashSet<&String> = order.iter().collect();
        assert_eq!(distinct.len(), 3, "{order:?}");
        c.execute("UPDATE autofill_picks SET picked_ms=picked_ms-?1 WHERE id=?2", params![DAY, order[1]]).unwrap();
        assert_eq!(rank(records, &album_use(&c, NOW).unwrap(), NOW, 0)[0], order[1]);
    }

    #[test]
    fn plays_count_as_use() {
        let mut c = nori_db::open("", "t").unwrap();
        let s = Song { id: "s1".into(), title: "One".into(), artist: "Artist".into(), album: "Heard".into(), album_id: Some("al-heard".into()), duration: 200, ..Default::default() };
        assert!(nori_library::history::record(&mut c, &s, NOW - DAY, 200_000, 0, NOW).unwrap());
        assert_eq!(rank(ids(&["al-heard", "al-other"]), &album_use(&c, NOW).unwrap(), NOW, 0), ids(&["al-other", "al-heard"]));
        assert_eq!(rank(ids(&["s1", "s2"]), &song_use(&c, NOW).unwrap(), NOW, 0), ids(&["s2", "s1"]));
        assert_eq!(album_use(&c, NOW + LATELY_MS + DAY).unwrap().get("al-heard"), None);
    }

    #[test]
    fn picks_expire_and_are_per_server() {
        let c = nori_db::open("", "t").unwrap();
        c.execute("INSERT INTO autofill_picks(server, kind, id, picked_ms) VALUES('t', 0, 'old', ?1)", params![NOW - FORGET_MS - DAY]).unwrap();
        c.execute("INSERT INTO autofill_picks(server, kind, id, picked_ms) VALUES('other', 0, 'theirs', ?1)", params![NOW]).unwrap();
        note(&c, Picked::Album, &ids(&["new"]), NOW).unwrap();
        let left: Vec<String> = c.prepare("SELECT id FROM autofill_picks WHERE server='t'").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(left, ids(&["new"]));
        assert!(!album_use(&c, NOW).unwrap().contains_key("theirs"));
    }

    #[test]
    fn shuffle_queues_refill_with_autoplay_off() {
        use nori_model::OriginKind as K;
        assert!(refills(Some(K::ShuffleAlbums), false) && refills(Some(K::ShuffleSongs), false));
        assert!(!refills(Some(K::Album), false) && !refills(None, false));
        assert!(refills(None, true));
    }

    #[test]
    fn refill_timing_follows_queue() {
        let _g = crate::playlist::tests::hold(&["rf1", "rf2", "rf3", "rf4"], 0);
        *REFILL.lock() = Refill::new();
        assert!(!autofill_start(), "three songs still follow");
        assert_eq!(autofill_next(), FillNext::Skip);
        crate::playlist::playlist_moved_to(1);
        assert_eq!(autofill_seed().as_deref(), Some("rf4"), "seeded with the queue's end, wherever the user is");
        assert!(autofill_start(), "two left: the fetch starts ahead of a fast run of nexts");
        assert!(!autofill_start(), "one fetch at a time");
        crate::playlist::playlist_moved_to(3);
        assert_eq!(autofill_next_at(1_000), FillNext::Wait, "the last song: the press waits for the fetch out");
        assert!(autofill_arrived(2), "the end is still rf4");
        crate::playlist::playlist_take(4, vec!["rf5".into(), "rf6".into()], vec![nori_player::playlist::Hand::No; 2], None);
        assert!(autofill_landed_at(1_500), "still on the song the press was made on, half a second later");
        crate::playlist::playlist_repeat(2);
        crate::playlist::playlist_moved_to(5);
        assert!(!autofill_start(), "a repeating queue has no end");
        crate::playlist::playlist_repeat(0);
        crate::playlist::playlist_set(vec!["radio:1".into()], Some(0), false, None);
        assert!(!autofill_start(), "a radio stream is not refilled");
    }

    /// Presses next on the last song at `presses`, lands the fetch at `arrive` (after moving to `on`):
    /// whether the skip is taken.
    fn end_of_queue(tag: &str, presses: &[i64], arrive: i64, on: Option<u32>) -> bool {
        let ids: Vec<String> = (0..3).map(|k| format!("{tag}{k}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let _g = crate::playlist::tests::hold(&refs, 2);
        *REFILL.lock() = Refill::new();
        let mut fetches = 0;
        for &t in presses {
            match autofill_next_at(t) {
                FillNext::Fetch => fetches += 1,
                FillNext::Wait => {}
                FillNext::Skip => panic!("nothing after the last song"),
            }
        }
        assert_eq!(fetches, 1, "one fetch however many presses");
        if let Some(i) = on {
            crate::playlist::playlist_moved_to(i as i32);
        }
        assert!(autofill_arrived(15), "the songs go in either way");
        crate::playlist::playlist_take(3, vec![format!("{tag}-a"), format!("{tag}-b")], vec![nori_player::playlist::Hand::No; 2], None);
        autofill_landed_at(arrive)
    }

    #[test]
    fn next_at_end_skips_only_if_songs_land_soon() {
        assert!(end_of_queue("ea", &[10_000], 10_800, None), "a press, then a fast arrival: the skip is taken");
        assert!(!end_of_queue("eb", &[10_000], 14_600, None), "a press, then a slow arrival: the songs join, no skip");
        // Six fast presses: one fetch, timed from the last press.
        let mash = [10_000, 10_130, 10_350, 10_460, 10_580, 10_750];
        assert!(end_of_queue("ec", &mash, 12_700, None));
        assert!(!end_of_queue("ed", &mash, 14_600, None));
        assert!(!end_of_queue("ee", &[10_000], 10_500, Some(0)), "moved elsewhere meanwhile");
    }

    #[test]
    fn fetch_for_moved_end_is_dropped() {
        let _g = crate::playlist::tests::hold(&["em0", "em1", "em2"], 1);
        *REFILL.lock() = Refill::new();
        assert!(autofill_start());
        // Add to queue inserts after the current song, so the end is still em2.
        crate::playlist::playlist_take(3, vec!["mine".into()], vec![nori_player::playlist::Hand::Last], None);
        assert_eq!(autofill_seed().as_deref(), Some("em2"));
        assert!(autofill_arrived(15));
        crate::playlist::playlist_take(4, vec!["em3".into()], vec![nori_player::playlist::Hand::No], None);
        assert!(!autofill_landed_at(0), "no next was waiting");
        // Songs appended elsewhere move the end: the fill is dropped.
        crate::playlist::playlist_moved_to(3);
        assert!(autofill_start());
        crate::playlist::playlist_take(5, vec!["em4".into()], vec![nori_player::playlist::Hand::No], None);
        assert!(!autofill_arrived(15));
        assert!(autofill_start());
        crate::playlist::playlist_set(vec!["n0".into(), "n1".into()], Some(0), false, None);
        assert!(!autofill_arrived(15));
    }
}
