//! Keeps the music going past the end of the queue. What arrives is the user's choice twice over - songs
//! or a whole album, chosen by what the server calls similar or by the artist, genre or decade - and
//! every route here reads the library, so this never makes octo-fiesta download a provider track. When
//! to fetch (the end in sight: at most `FILL_AHEAD` songs left, one fetch at a time), what the fetch is
//! seeded with (the queue's last song, so the fetch is the same wherever the user is when it starts) and
//! a next pressed at the end while songs are on the way (taken if they land within `NEXT_KEPT_MS` of the
//! last press, once) are `nori_player::queue::Refill`'s, kept here over the core's own queue; the
//! platform fetches when told and appends what comes back.
//!
//! Taking turns is here too ([`rank`]). The next record or songs come from a list the server hands back
//! in the same order every time - an artist's albums, the songs it calls similar - and taking the first
//! one not already queued meant the same single led to the same album evening after evening. Every pick
//! is remembered (`autofill_picks`), and the candidates are ranked before one is taken: those neither
//! picked nor played lately first, in the list's order (the first of them drawn from the top few, so the
//! list's order does not decide every evening alike), then the rest by how long ago they were last used,
//! so an artist's records take turns. Nothing is ruled out, so there is always something to play.
//! "Played" comes from the listening history when it is kept (the taste model's switch); the picks are
//! remembered either way.

use std::collections::{HashMap, HashSet};

use nori_library::mixes::Rng;
use nori_model::Song;
use rusqlite::{params, Connection};
use nori_net::transport::NetError;
use nori_player::queue::{refillable, shuffle, Refill};
use parking_lot::Mutex;

use crate::queue;

/// How many loose songs one fill adds.
pub const SONGS: usize = 15;
/// How many candidate albums are tried before settling for a short one.
pub const ALBUM_TRIES: usize = 6;
/// An album shorter than this is a single: taken only if nothing longer is on offer.
pub const ALBUM_MIN: usize = 3;
/// Random albums one refill of a "shuffle albums" queue adds, each whole: the first of the library's own
/// among the [`crate::actions::SHUFFLE_ALBUMS`] asked for.
pub const RANDOM_ALBUMS: i32 = 3;

/// What a fetch from the server gives.
pub type Got<T> = Result<T, NetError>;

static REFILL: Mutex<Refill> = Mutex::new(Refill::new());

/// Milliseconds on a clock that only moves forward, for how old a next press is.
fn mono_ms() -> i64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(std::time::Instant::now).elapsed().as_millis() as i64
}

/// The queue's last song in play order: what a refill carries on from.
fn end_of(p: &nori_player::playlist::Playlist) -> Option<String> {
    let last = match p.shuffle_order() {
        Some(order) => order.last().copied(),
        None => p.len().checked_sub(1),
    };
    last.and_then(|i| p.ids().get(i).cloned())
}

/// The song a refill is seeded with: the queue's last, in play order.
pub fn autofill_seed() -> Option<String> {
    crate::playlist::with(end_of)
}

/// Whether refilling is on: the autoplay setting, or a queue started by a shuffle, which goes on with
/// random songs or albums whatever the setting says (the core's `autofill_from`).
fn refill_on() -> bool {
    refills(crate::playlist::playlist_origin().map(|o| o.kind), crate::rules::prefs(|p| p.auto_fill))
}

fn refills(origin: Option<nori_model::OriginKind>, auto_fill: bool) -> bool {
    auto_fill || matches!(origin, Some(nori_model::OriginKind::ShuffleSongs | nori_model::OriginKind::ShuffleAlbums))
}

/// Whether the queue may be refilled now, how many songs follow the current one, and its last song.
fn refill_facts() -> (bool, usize, Option<String>) {
    let setting = refill_on();
    crate::playlist::with(|p| {
        let cur = p.current_id();
        (refillable(cur.is_some(), cur.is_some_and(|c| c.starts_with(queue::RADIO_PREFIX)), p.repeat(), setting), p.songs_after(), end_of(p))
    })
}

/// The queue moved: whether to fetch songs for its end now ([`Client::autofill`]). A true answer is a
/// fetch on the wire until [`autofill_arrived`].
pub fn autofill_start() -> bool {
    let (ok, after, end) = refill_facts();
    REFILL.lock().start(ok, after, end.as_deref())
}

/// What a next press does now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FillNext {
    /// There is a song after: skip to it.
    Skip,
    /// Nothing after: fetch, and the skip is taken when the songs land ([`autofill_landed`]) if they
    /// land soon enough.
    Fetch,
    /// Nothing to do now: songs are already on the way (the press waits for them, as long as
    /// [`autofill_skip_waiting`] says), or the queue cannot be refilled.
    Wait,
}

/// Next pressed with the queue's end in sight.
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

/// The fetch came back with `count` songs: whether they go in (see `Refill::arrived`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn autofill_arrived(count: u32) -> bool {
    let end = autofill_seed();
    REFILL.lock().arrived(count as usize, end.as_deref())
}

/// The songs that arrived are in the queue: whether to take a next that was pressed while they were on
/// the way. Only a press on the song still playing, made within `NEXT_KEPT_MS` (2 s) of now, is taken;
/// an older one is forgotten, and the songs just join the queue.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn autofill_landed() -> bool {
    autofill_landed_at(mono_ms())
}

fn autofill_landed_at(now_ms: i64) -> bool {
    let (current, has_next) = crate::playlist::with(|p| (p.current_id().map(str::to_string), p.next().is_some()));
    REFILL.lock().landed(current.as_deref(), has_next, now_ms)
}

/// Whether a next pressed at the end is waiting for songs on the wire, and would be taken if they came
/// now. A client may show it (the next button busy for the moment); it turns false by itself
/// `NEXT_KEPT_MS` after the last press, so a screen showing it asks again then. Android does not show it
/// yet.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn autofill_skip_waiting() -> bool {
    let current = crate::playlist::with(|p| p.current_id().map(str::to_string));
    REFILL.lock().skip_waiting(current.as_deref(), mono_ms())
}

/// A seed for a draw, from the clock.
pub fn seed_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64)
}

/// `v` in an order drawn from the clock.
pub fn shuffled<T>(mut v: Vec<T>) -> Vec<T> {
    shuffle(&mut v, seed_now());
    v
}

/// The decade `seed` belongs to, for the era basis; none when the server gave no year.
pub fn era(seed: &Song) -> Option<(u32, u32)> {
    (seed.year > 0).then(|| (seed.year / 10 * 10, seed.year / 10 * 10 + 9))
}

// ---- taking turns ----------------------------------------------------------------------------------------

/// What was picked: a whole album, or loose songs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    Album = 0,
    Songs = 1,
}

/// Picked or played within this long ago counts as lately.
pub const LATELY_MS: i64 = 14 * 86_400_000;
/// The first pick is drawn from this many of the fresh candidates at the top of the list.
const TOP_DRAW: usize = 3;
/// Picks older than this are forgotten.
pub const FORGET_MS: i64 = 90 * 86_400_000;

/// `candidates` best first, as the list gave them; `used` the last time each was picked or played.
pub fn rank(candidates: Vec<String>, used: &HashMap<String, i64>, now_ms: i64, seed: u64) -> Vec<String> {
    let mut seen = HashSet::new();
    let (mut fresh, mut stale): (Vec<String>, Vec<String>) =
        candidates.into_iter().filter(|c| seen.insert(c.clone())).partition(|c| used.get(c).is_none_or(|&t| now_ms - t >= LATELY_MS));
    if !fresh.is_empty() {
        let first = (Rng::new(seed).next() % fresh.len().min(TOP_DRAW) as u64) as usize;
        let pick = fresh.remove(first);
        fresh.insert(0, pick);
    }
    // Longest unused first; the sort is stable, so equals keep the list's order.
    stale.sort_by_key(|c| used[c]);
    fresh.extend(stale);
    fresh
}

/// Keeps the later of two times for `id`.
fn latest(used: &mut HashMap<String, i64>, id: String, at: i64) {
    let t = used.entry(id).or_insert(at);
    *t = (*t).max(at);
}

/// When each id of `kind` was last picked, over the window that counts.
fn picked(c: &Connection, kind: Picked, now_ms: i64, used: &mut HashMap<String, i64>) -> rusqlite::Result<()> {
    let mut st = c.prepare_cached("SELECT id, picked_ms FROM autofill_picks WHERE server=sid() AND kind=?1 AND picked_ms>=?2")?;
    for row in st.query_map(params![kind as i64, now_ms - LATELY_MS], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, at) = row?;
        latest(used, id, at);
    }
    Ok(())
}

/// When each album was last picked, or had a song of it played, over the window that counts.
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

/// When each song was last picked or played, over the window that counts.
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

/// Remembers that `ids` were just picked, and forgets picks too old to matter.
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
    fn nothing_used_keeps_the_order_after_the_drawn_first() {
        let list = ids(&["a", "b", "c", "d", "e"]);
        for seed in 0..50 {
            let r = rank(list.clone(), &HashMap::new(), NOW, seed);
            assert_eq!(r.len(), 5);
            assert!(["a", "b", "c"].contains(&r[0].as_str()), "{r:?}");
            let rest: Vec<&String> = list.iter().filter(|x| **x != r[0]).collect();
            assert_eq!(r[1..].iter().collect::<Vec<_>>(), rest);
        }
        // Over many evenings every one of the top three leads at some point.
        let leads: HashSet<String> = (0..50).map(|s| rank(list.clone(), &HashMap::new(), NOW, s)[0].clone()).collect();
        assert_eq!(leads.len(), 3);
    }

    #[test]
    fn lately_used_go_last_longest_unused_first() {
        let used = HashMap::from([("a".to_string(), NOW - DAY), ("b".to_string(), NOW - 5 * DAY), ("c".to_string(), NOW - 20 * DAY)]);
        // c was used long enough ago to count as fresh again.
        let r = rank(ids(&["a", "b", "c", "d"]), &used, NOW, 1);
        assert!(["c", "d"].contains(&r[0].as_str()));
        assert_eq!(r[2..], ids(&["b", "a"]));
        // Everything used lately: all of it still comes back, the one left longest first.
        let all = HashMap::from([("a".to_string(), NOW - DAY), ("b".to_string(), NOW - 3 * DAY)]);
        assert_eq!(rank(ids(&["a", "b"]), &all, NOW, 7), ids(&["b", "a"]));
    }

    #[test]
    fn duplicates_and_empty() {
        assert!(rank(Vec::new(), &HashMap::new(), NOW, 3).is_empty());
        let r = rank(ids(&["a", "a", "b"]), &HashMap::from([("a".to_string(), NOW)]), NOW, 3);
        assert_eq!(r, ids(&["b", "a"]));
    }

    #[test]
    fn an_artists_records_take_turns() {
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
        // With every record picked lately, the next is the one picked longest ago.
        c.execute("UPDATE autofill_picks SET picked_ms=picked_ms-?1 WHERE id=?2", params![DAY, order[1]]).unwrap();
        assert_eq!(rank(records, &album_use(&c, NOW).unwrap(), NOW, 0)[0], order[1]);
    }

    #[test]
    fn played_albums_and_songs_count_as_used() {
        let mut c = nori_db::open("", "t").unwrap();
        let s = Song { id: "s1".into(), title: "One".into(), artist: "Artist".into(), album: "Heard".into(), album_id: Some("al-heard".into()), duration: 200, ..Default::default() };
        assert!(nori_library::history::record(&mut c, &s, NOW - DAY, 200_000, 0, NOW).unwrap());
        assert_eq!(rank(ids(&["al-heard", "al-other"]), &album_use(&c, NOW).unwrap(), NOW, 0), ids(&["al-other", "al-heard"]));
        assert_eq!(rank(ids(&["s1", "s2"]), &song_use(&c, NOW).unwrap(), NOW, 0), ids(&["s2", "s1"]));
        // A listen from before the window no longer counts.
        assert_eq!(album_use(&c, NOW + LATELY_MS + DAY).unwrap().get("al-heard"), None);
    }

    #[test]
    fn old_picks_are_forgotten_and_each_server_keeps_its_own() {
        let c = nori_db::open("", "t").unwrap();
        c.execute("INSERT INTO autofill_picks(server, kind, id, picked_ms) VALUES('t', 0, 'old', ?1)", params![NOW - FORGET_MS - DAY]).unwrap();
        c.execute("INSERT INTO autofill_picks(server, kind, id, picked_ms) VALUES('other', 0, 'theirs', ?1)", params![NOW]).unwrap();
        note(&c, Picked::Album, &ids(&["new"]), NOW).unwrap();
        let left: Vec<String> = c.prepare("SELECT id FROM autofill_picks WHERE server='t'").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(left, ids(&["new"]));
        assert!(!album_use(&c, NOW).unwrap().contains_key("theirs"), "another server's picks are not this one's");
    }

    #[test]
    fn a_shuffle_goes_on_with_autoplay_off() {
        use nori_model::OriginKind as K;
        assert!(refills(Some(K::ShuffleAlbums), false) && refills(Some(K::ShuffleSongs), false));
        assert!(!refills(Some(K::Album), false) && !refills(None, false), "any other queue ends when autoplay is off");
        assert!(refills(None, true));
    }

    #[test]
    fn when_to_refill_is_read_off_the_queue() {
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
        assert!(autofill_skip_waiting(), "a skip on its way");
        assert!(autofill_arrived(2), "the end is still rf4");
        crate::playlist::playlist_take(4, vec!["rf5".into(), "rf6".into()], vec![nori_player::playlist::Hand::No; 2], None);
        assert!(autofill_landed_at(1_500), "still on the song the press was made on, half a second later");
        crate::playlist::playlist_repeat(2);
        crate::playlist::playlist_moved_to(5);
        assert!(!autofill_start(), "a repeating queue has no end");
        crate::playlist::playlist_repeat(0);
        crate::playlist::playlist_set(vec!["radio:1".into()], 0, false, None);
        assert!(!autofill_start(), "a radio stream is not refilled");
    }

    /// Plays the end of a queue: `presses` nexts on the last song at those times, the fetch coming back at
    /// `arrive`, the player on `on` by then. Whether the skip is taken.
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
    fn a_next_at_the_end_skips_only_when_the_songs_come_soon() {
        assert!(end_of_queue("ea", &[10_000], 10_800, None), "a press, then a fast arrival: the skip is taken");
        assert!(!end_of_queue("eb", &[10_000], 14_600, None), "a press, then a slow arrival: the songs join, no skip");
        // Mashed next six times (the phone's log): one fetch, one skip, timed from the last press.
        let mash = [10_000, 10_130, 10_350, 10_460, 10_580, 10_750];
        assert!(end_of_queue("ec", &mash, 12_700, None));
        assert!(!end_of_queue("ed", &mash, 14_600, None));
        assert!(!end_of_queue("ee", &[10_000], 10_500, Some(0)), "the user went elsewhere meanwhile: no skip");
    }

    #[test]
    fn songs_fetched_for_an_end_that_moved_stay_out() {
        let _g = crate::playlist::tests::hold(&["em0", "em1", "em2"], 1);
        *REFILL.lock() = Refill::new();
        assert!(autofill_start());
        // Add to queue goes after the song playing (and the songs added before it), not after the end:
        // the fill still carries on from em2.
        crate::playlist::playlist_take(3, vec!["mine".into()], vec![nori_player::playlist::Hand::Last], None);
        assert_eq!(autofill_seed().as_deref(), Some("em2"));
        assert!(autofill_arrived(15));
        crate::playlist::playlist_take(4, vec!["em3".into()], vec![nori_player::playlist::Hand::No], None);
        assert!(!autofill_landed_at(0), "no next was waiting");
        // Songs put at the end from elsewhere (a controller's insert): the fill is no longer wanted there.
        crate::playlist::playlist_moved_to(3);
        assert!(autofill_start());
        crate::playlist::playlist_take(5, vec!["em4".into()], vec![nori_player::playlist::Hand::No], None);
        assert!(!autofill_arrived(15), "the queue's end moved meanwhile");
        assert!(autofill_start(), "and the next move may fetch again");
        // A new queue altogether.
        crate::playlist::playlist_set(vec!["n0".into(), "n1".into()], 0, false, None);
        assert!(!autofill_arrived(15));
    }
}
