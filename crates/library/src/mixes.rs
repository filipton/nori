//! Mixes built from the local index and the taste model in `history.rs`; no server round trip.
//!
//! The index can hold 100k songs, so no mix reads it whole. SQL narrows to a candidate pool of a
//! few times `limit` (through `song_stats`, or through the expression indexes on genre, artist,
//! year, starred and rating), and only that pool is parsed, scored and sampled in Rust.
//!
//! Everything random derives from the caller's `seed`: the same seed over the same data gives the
//! same mix, so a screen can be rebuilt without its content changing, and a "refresh" is a new seed.
//! SQLite's `random()` cannot be seeded, so pools are drawn in the order of a seeded linear
//! congruence over `rowid` (`shuffled_order`), which needs no JSON parsing and no temp table.

use std::collections::{HashMap, HashSet, VecDeque};

use nori_model::model::Song;
use rusqlite::{types::Value, Connection, OptionalExtension};

use crate::history;

/// The "For you" row built on these draws.
pub mod board;

const DAY_MS: i64 = 86_400_000;

/// splitmix64: tiny, no state to warm up, good enough to shuffle music.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// In (0, 1], so its logarithm is finite.
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) + 1) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const LCG_P: i64 = 2_147_483_647;

/// An `ORDER BY` term that is a seeded permutation of `rowid` (a bijection modulo a prime), and its two parameters.
/// `a` and `b` are the 1-based numbers of those parameters in the statement.
pub(crate) fn shuffled_order(rowid: &str, a: usize, b: usize) -> String {
    format!("(({rowid} % {LCG_P}) * ?{a} + ?{b}) % {LCG_P}")
}

pub(crate) fn shuffled_params(seed: u64) -> [Value; 2] {
    let mut r = Rng::new(seed);
    [Value::Integer(1 + (r.next() % (LCG_P as u64 - 1)) as i64), Value::Integer((r.next() % LCG_P as u64) as i64)]
}

/// This server's songs. Most rows of `items` are songs: saying so keeps the planner from treating
/// `kind=2` as the selective term and ignoring the expression indexes (there is no ANALYZE data on a phone).
pub const SONGS: &str = "i.server=sid() AND likelihood(i.kind=2, 0.9)";

struct Cand {
    song: Song,
    plays: u32,
    skips: u32,
    taste: f64,
}

enum Order {
    Shuffled(u64),
    Taste,
    Plays,
}

/// Up to `n` songs matching `cond` (which numbers its own parameters from ?1), never an excluded one.
/// `played_only` drives the query from `song_stats`, which is small, instead of from the index.
fn pool(c: &Connection, played_only: bool, cond: &str, mut args: Vec<Value>, order: Order, n: usize, now_ms: i64) -> rusqlite::Result<Vec<Cand>> {
    let from = if played_only { "song_stats s JOIN items i ON s.server=sid() AND i.server=sid() AND i.kind=2 AND i.id=s.song_id" } else { "items i LEFT JOIN song_stats s ON s.server=i.server AND s.song_id=i.id" };
    let order = match order {
        Order::Shuffled(seed) => {
            args.extend(shuffled_params(seed));
            shuffled_order("i.rowid", args.len() - 1, args.len())
        }
        Order::Taste => "s.taste DESC, i.rowid".to_string(),
        Order::Plays => "s.plays DESC, s.heard_ms_total DESC, i.rowid".to_string(),
    };
    args.push(Value::Integer(n as i64));
    let sql = format!(
        "SELECT i.json, coalesce(s.plays,0), coalesce(s.skips,0), coalesce(s.taste,0) FROM {from}
         WHERE {SONGS} AND {cond} AND NOT EXISTS(SELECT 1 FROM mix_excluded e WHERE e.server=i.server AND e.song_id=i.id) ORDER BY {order} LIMIT ?{}",
        args.len()
    );
    let mut st = c.prepare_cached(&sql)?;
    let mut rows = st.query(rusqlite::params_from_iter(args))?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let Ok(song) = serde_json::from_str::<Song>(&r.get::<_, String>(0)?) else { continue };
        let taste = history::taste(&song, r.get(3)?, now_ms);
        out.push(Cand { song, plays: r.get(1)?, skips: r.get(2)?, taste });
    }
    Ok(out)
}

fn artist_key(s: &Song) -> Option<String> {
    match s.artist_id.as_deref() {
        Some(id) if !id.is_empty() => Some(id.to_string()),
        _ => (!s.artist.is_empty()).then(|| s.artist.to_lowercase()),
    }
}

fn album_key(s: &Song) -> Option<String> {
    match s.album_id.as_deref() {
        Some(id) if !id.is_empty() => Some(id.to_string()),
        _ => (!s.album.is_empty()).then(|| format!("{}\0{}", s.artist, s.album).to_lowercase()),
    }
}

/// Weighted sampling without replacement (Efraimidis-Spirakis: the largest `ln(u)/w` win), at most
/// `per_artist` songs of one artist so that one discography cannot fill a mix.
fn sample(cands: Vec<(Song, f64)>, limit: usize, per_artist: usize, rng: &mut Rng) -> Vec<Song> {
    let mut seen = HashSet::new();
    let mut keyed: Vec<(f64, Song)> = cands.into_iter().filter(|(s, _)| seen.insert(s.id.clone())).map(|(s, w)| (rng.unit().ln() / w.max(1e-6), s)).collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut by_artist: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::with_capacity(limit.min(keyed.len()));
    for (_, s) in keyed {
        if out.len() == limit {
            break;
        }
        if let Some(k) = artist_key(&s) {
            let n = by_artist.entry(k).or_default();
            if *n >= per_artist {
                continue;
            }
            *n += 1;
        }
        out.push(s);
    }
    out
}

/// A seeded shuffle that keeps songs of one artist apart, and when it can also songs of one album and an
/// artist heard two songs ago. `after` is what plays right before the result.
///
/// Two songs of an artist end up adjacent only when that cannot be avoided, that is when the artist has
/// more than half of what is left: the artist with the most songs left is forced as soon as postponing
/// it would make a collision certain; every other position takes the next song of the plain shuffle that fits.
pub(crate) fn spread(songs: Vec<Song>, rng: &mut Rng, after: Option<&Song>) -> Vec<Song> {
    let order = spread_order(&songs, rng, after);
    let mut slots: Vec<Option<Song>> = songs.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

/// [`spread`] as positions in `songs`, for a caller that holds the songs already.
pub(crate) fn spread_order(songs: &[Song], rng: &mut Rng, after: Option<&Song>) -> Vec<usize> {
    let n = songs.len();
    let mut shuffled: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        shuffled.swap(i, rng.below(i + 1));
    }
    let songs: Vec<&Song> = shuffled.iter().map(|&i| &songs[i]).collect();
    let (mut artists, mut albums): (HashMap<String, usize>, HashMap<String, usize>) = (HashMap::new(), HashMap::new());
    let number = |map: &mut HashMap<String, usize>, key: Option<String>, i: usize| {
        // No name: not the same as anything.
        let key = key.unwrap_or_else(|| format!("\0{i}"));
        let next = map.len();
        *map.entry(key).or_insert(next)
    };
    let artist_of: Vec<usize> = songs.iter().enumerate().map(|(i, s)| number(&mut artists, artist_key(s), i)).collect();
    let album_of: Vec<usize> = songs.iter().enumerate().map(|(i, s)| number(&mut albums, album_key(s), i)).collect();
    let mut queues: Vec<VecDeque<usize>> = vec![VecDeque::new(); artists.len()];
    for (i, &a) in artist_of.iter().enumerate() {
        queues[a].push_back(i);
    }
    let mut last_artist = after.and_then(artist_key).and_then(|k| artists.get(&k).copied());
    let mut last_album = after.and_then(album_key).and_then(|k| albums.get(&k).copied());
    let mut prev_artist = None;

    let mut slots: Vec<Option<usize>> = shuffled.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    for left in (1..=n).rev() {
        let dominant = (0..queues.len()).max_by_key(|&a| queues[a].len()).unwrap_or(0);
        let pick = if queues[dominant].len() * 2 > left && Some(dominant) != last_artist {
            queues[dominant][0]
        } else {
            let (mut fits, mut looked) = (None, 0);
            for i in start..n {
                if slots[i].is_none() {
                    continue;
                }
                let a = Some(artist_of[i]);
                if a != last_artist {
                    if a != prev_artist && Some(album_of[i]) != last_album {
                        fits = Some(i);
                        break;
                    }
                    fits = fits.or(Some(i));
                }
                looked += 1;
                // The nice-to-haves are not worth a quadratic scan of a long queue.
                if looked >= 24 && fits.is_some() {
                    break;
                }
            }
            fits.unwrap_or(start)
        };
        let a = artist_of[pick];
        if let Some(p) = queues[a].iter().position(|&i| i == pick) {
            queues[a].remove(p);
        }
        out.extend(slots[pick].take());
        (prev_artist, last_artist, last_album) = (last_artist, Some(a), Some(album_of[pick]));
        while start < n && slots[start].is_none() {
            start += 1;
        }
    }
    out
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

/// Songs the user likes and has not heard in the last three days. Empty until there is some history, a star or a rating.
pub fn quick_picks(c: &Connection, limit: usize, seed: u64, now_ms: i64) -> rusqlite::Result<Vec<Song>> {
    let mut rng = Rng::new(seed);
    let rested = || vec![Value::Integer(now_ms - 3 * DAY_MS)];
    let mut cands = pool(c, true, "s.last_played_ms<?1 AND s.taste>0", rested(), Order::Taste, (limit * 4).max(100), now_ms)?;
    // Two statements because each can only use its own partial index.
    cands.extend(pool(c, false, "json_extract(i.json,'$.starred')=1 AND coalesce(s.last_played_ms,0)<?1", rested(), Order::Shuffled(rng.next()), limit * 2, now_ms)?);
    cands.extend(pool(c, false, "json_extract(i.json,'$.userRating')>=4 AND coalesce(s.last_played_ms,0)<?1", rested(), Order::Shuffled(rng.next()), limit * 2, now_ms)?);
    let picked = sample(cands.into_iter().filter(|c| c.taste > 0.0).map(|c| (c.song, c.taste)).collect(), limit, (limit / 5).max(2), &mut rng);
    Ok(spread(picked, &mut rng, None))
}

/// Recent favourites: played in the last 45 days, the better liked the more likely.
pub fn listen_again(c: &Connection, limit: usize, seed: u64, now_ms: i64) -> rusqlite::Result<Vec<Song>> {
    let mut rng = Rng::new(seed);
    let cands = pool(c, true, "s.plays>0 AND s.last_played_ms>=?1", vec![Value::Integer(now_ms - 45 * DAY_MS)], Order::Taste, (limit * 4).max(100), now_ms)?;
    let picked = sample(cands.into_iter().filter(|c| c.taste > 0.0).map(|c| (c.song, c.taste * (1.0 + c.plays as f64).ln())).collect(), limit, (limit / 4).max(2), &mut rng);
    Ok(spread(picked, &mut rng, None))
}

/// Most played, in order; the one mix that is a ranking and not a draw.
pub fn top(c: &Connection, limit: usize, now_ms: i64) -> rusqlite::Result<Vec<Song>> {
    Ok(pool(c, true, "s.plays>0", vec![], Order::Plays, limit, now_ms)?.into_iter().map(|c| c.song).collect())
}

/// Songs never played or played once and never skipped, leaning towards the artists and genres the
/// user plays. With no history it is a random walk through the library.
pub fn discover(c: &Connection, limit: usize, seed: u64, now_ms: i64) -> rusqlite::Result<Vec<Song>> {
    let mut rng = Rng::new(seed);
    let mut liked = pool(c, true, "1", vec![], Order::Taste, 500, now_ms)?;
    liked.extend(pool(c, false, "json_extract(i.json,'$.starred')=1", vec![], Order::Shuffled(rng.next()), 200, now_ms)?);
    let (mut artists, mut genres): (HashMap<String, f64>, HashMap<String, f64>) = (HashMap::new(), HashMap::new());
    let mut seen = HashSet::new();
    for l in liked.iter().filter(|l| seen.insert(l.song.id.clone())) {
        if let Some(id) = l.song.artist_id.as_deref().filter(|id| !id.is_empty()) {
            *artists.entry(id.to_string()).or_default() += l.taste;
        }
        if let Some(g) = l.song.genre.as_deref().filter(|g| !g.is_empty()) {
            *genres.entry(g.to_lowercase()).or_default() += l.taste;
        }
    }
    let best = |m: &HashMap<String, f64>, n: usize| {
        let mut l: Vec<(String, f64)> = m.iter().filter(|(_, &v)| v > 0.0).map(|(k, &v)| (k.clone(), v)).collect();
        l.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        l.truncate(n);
        l
    };
    let (top_artists, top_genres) = (best(&artists, 8), best(&genres, 4));

    const FRESH: &str = "coalesce(s.plays,0)<=1 AND coalesce(s.skips,0)=0";
    let mut cands = Vec::new();
    for (id, _) in &top_artists {
        cands.extend(pool(c, false, &format!("json_extract(i.json,'$.artistId')=?1 AND {FRESH}"), vec![text(id)], Order::Shuffled(rng.next()), limit, now_ms)?);
    }
    for (g, _) in &top_genres {
        cands.extend(pool(c, false, &format!("json_extract(i.json,'$.genre')=?1 COLLATE NOCASE AND {FRESH}"), vec![text(g)], Order::Shuffled(rng.next()), limit * 2, now_ms)?);
    }
    cands.extend(pool(c, false, FRESH, vec![], Order::Shuffled(rng.next()), limit * 2, now_ms)?);

    // Affinity relative to the favourite, so the weights mean the same for a light and a heavy listener.
    let norm = |m: &HashMap<String, f64>, l: &[(String, f64)], k: Option<String>| -> f64 {
        let max = l.first().map(|x| x.1).unwrap_or(1.0);
        k.and_then(|k| m.get(&k).copied()).map(|v| (v / max).clamp(-1.0, 1.0)).unwrap_or(0.0)
    };
    let weighted = cands
        .into_iter()
        .filter(|c| c.taste >= 0.0)
        .map(|c| {
            let a = norm(&artists, &top_artists, c.song.artist_id.clone());
            let g = norm(&genres, &top_genres, c.song.genre.as_ref().map(|g| g.to_lowercase()));
            let w = 0.3 + 3.0 * a.max(0.0) + 2.0 * g.max(0.0);
            (c.song, if a < 0.0 || g < 0.0 { w * 0.2 } else { w })
        })
        .collect();
    let picked = sample(weighted, limit, (limit / 5).max(2), &mut rng);
    Ok(spread(picked, &mut rng, None))
}

/// A draw from `cond`, liked songs more likely and skipped ones less. Shared by the genre, artist and decade mixes.
pub fn themed(c: &Connection, cond: &str, args: Vec<Value>, limit: usize, per_artist: usize, seed: u64, now_ms: i64) -> rusqlite::Result<Vec<Song>> {
    let mut rng = Rng::new(seed);
    let cands = pool(c, false, cond, args, Order::Shuffled(rng.next()), limit * 3, now_ms)?;
    let picked = sample(cands.into_iter().map(|c| (c.song, affinity(c.taste, c.skips))).collect(), limit, per_artist, &mut rng);
    Ok(spread(picked, &mut rng, None))
}

fn affinity(taste: f64, skips: u32) -> f64 {
    if taste < 0.0 {
        0.2 / (1.0 + skips as f64)
    } else {
        1.0 + taste.min(4.0)
    }
}

/// The SQL condition of a decade mix: the songs of one decade.
pub fn decade_cond() -> &'static str {
    "json_extract(i.json,'$.year') BETWEEN ?1 AND ?2"
}

/// The seed song, then its neighbourhood: same genre first, then same artist and same decade.
pub fn instant(c: &Connection, seed_song_id: &str, limit: usize, seed: u64, now_ms: i64) -> rusqlite::Result<Vec<Song>> {
    let first: Option<Song> = c.prepare_cached("SELECT json FROM items WHERE server=sid() AND kind=2 AND id=?1")?.query_row([seed_song_id], |r| r.get::<_, String>(0)).optional()?.and_then(|j| serde_json::from_str(&j).ok());
    let Some(first) = first else { return Ok(Vec::new()) };
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut rng = Rng::new(seed);
    let decade = (first.year / 10 * 10) as i64;
    let mut cands = Vec::new();
    if let Some(g) = first.genre.as_deref().filter(|g| !g.is_empty()) {
        cands.extend(pool(c, false, "json_extract(i.json,'$.genre')=?1 COLLATE NOCASE", vec![text(g)], Order::Shuffled(rng.next()), limit * 3, now_ms)?);
    }
    if let Some(a) = first.artist_id.as_deref().filter(|a| !a.is_empty()) {
        cands.extend(pool(c, false, "json_extract(i.json,'$.artistId')=?1", vec![text(a)], Order::Shuffled(rng.next()), limit, now_ms)?);
    }
    if first.year > 0 {
        cands.extend(pool(c, false, decade_cond(), vec![Value::Integer(decade), Value::Integer(decade + 9)], Order::Shuffled(rng.next()), limit * 2, now_ms)?);
    }
    let genre = first.genre.as_ref().map(|g| g.to_lowercase());
    let weighted = cands
        .into_iter()
        .filter(|c| c.song.id != first.id && c.taste > -0.5)
        .map(|c| {
            let s = &c.song;
            let mut w = 0.2;
            if genre.is_some() && s.genre.as_ref().map(|g| g.to_lowercase()) == genre {
                w += 3.0;
            }
            if first.artist_id.is_some() && s.artist_id == first.artist_id {
                w += 2.0;
            }
            if first.year > 0 && s.year as i64 / 10 * 10 == decade {
                w += 1.0;
            }
            (c.song, w * affinity(c.taste, c.skips))
        })
        .collect();
    let picked = sample(weighted, limit - 1, (limit / 4).max(3), &mut rng);
    let mut out = Vec::with_capacity(picked.len() + 1);
    let rest = spread(picked, &mut rng, Some(&first));
    out.push(first);
    out.extend(rest);
    Ok(out)
}

/// Shuffle for a play queue: seeded, and songs of one artist (and, when possible, of one album) are kept apart.
pub fn weighted_shuffle(songs: Vec<Song>, seed: u64) -> Vec<Song> {
    spread(songs, &mut Rng::new(seed), None)
}

/// [`weighted_shuffle`] as positions in `songs`.
pub fn weighted_shuffle_order(songs: &[Song], seed: u64) -> Vec<usize> {
    spread_order(songs, &mut Rng::new(seed), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::tests::song;

    fn ids(l: &[Song]) -> Vec<&str> {
        l.iter().map(|s| s.id.as_str()).collect()
    }

    fn adjacent_artists(l: &[Song]) -> usize {
        l.windows(2).filter(|w| w[0].artist == w[1].artist).count()
    }

    #[test]
    fn rng_is_deterministic_and_uniformish() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        assert_eq!((0..5).map(|_| a.next()).collect::<Vec<_>>(), (0..5).map(|_| b.next()).collect::<Vec<_>>());
        assert_ne!(Rng::new(1).next(), Rng::new(2).next());
        let mut r = Rng::new(0);
        let mean = (0..10_000).map(|_| r.unit()).sum::<f64>() / 10_000.0;
        assert!((mean - 0.5).abs() < 0.02, "{mean}");
        assert!((0..1000).all(|_| r.below(3) < 3));
        assert_eq!(r.below(0), 0);
    }

    #[test]
    fn shuffle_keeps_artists_apart_when_it_can() {
        let mut l = Vec::new();
        for (artist, n) in [("A", 10), ("B", 6), ("C", 3), ("D", 1)] {
            for i in 0..n {
                l.push(song(&format!("{artist}{i}"), "t", artist, &format!("{artist}-album-{}", i % 2), "", 0));
            }
        }
        for seed in 0..50 {
            let out = weighted_shuffle(l.clone(), seed);
            assert_eq!(out.len(), l.len());
            let mut sorted = ids(&out);
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), l.len(), "a permutation");
            assert_eq!(adjacent_artists(&out), 0, "seed {seed}: {:?}", ids(&out));
        }
        assert_eq!(ids(&weighted_shuffle(l.clone(), 3)), ids(&weighted_shuffle(l.clone(), 3)));
        assert_ne!(ids(&weighted_shuffle(l.clone(), 3)), ids(&weighted_shuffle(l, 4)));
    }

    #[test]
    fn shuffle_at_the_limit_of_what_is_possible() {
        // exactly half plus one: only A_A_A_A works
        let mut l: Vec<Song> = (0..4).map(|i| song(&format!("a{i}"), "t", "A", "", "", 0)).collect();
        l.extend((0..3).map(|i| song(&format!("b{i}"), "t", ["B", "C", "B"][i], "", "", 0)));
        for seed in 0..50 {
            assert_eq!(adjacent_artists(&weighted_shuffle(l.clone(), seed)), 0, "seed {seed}");
        }
        // impossible: 5 of 6 by one artist. Still a permutation, and the odd one out splits the run.
        let mut l: Vec<Song> = (0..5).map(|i| song(&format!("a{i}"), "t", "A", "", "", 0)).collect();
        l.push(song("b", "t", "B", "", "", 0));
        for seed in 0..20 {
            let out = weighted_shuffle(l.clone(), seed);
            assert_eq!(out.len(), 6);
            assert!(out[0].artist == "A" && out[5].artist == "A", "{:?}", ids(&out));
        }
    }

    #[test]
    fn shuffle_edge_cases() {
        assert!(weighted_shuffle(vec![], 1).is_empty());
        let l = vec![song("x", "t", "A", "b", "", 0), song("y", "t", "B", "b", "", 0), song("z", "t", "A", "c", "", 0)];
        let by_order: Vec<Song> = weighted_shuffle_order(&l, 5).into_iter().map(|i| l[i].clone()).collect();
        assert_eq!(by_order, weighted_shuffle(l, 5), "the positions are the same shuffle");
        let one = vec![song("x", "t", "A", "", "", 0)];
        assert_eq!(weighted_shuffle(one.clone(), 1), one);
        // no artist at all: nothing is "the same artist"
        let anon: Vec<Song> = (0..5).map(|i| Song { id: i.to_string(), ..Default::default() }).collect();
        assert_eq!(weighted_shuffle(anon, 9).len(), 5);
        // artists that differ only by id stay distinct, same name without id is one artist
        let mut l: Vec<Song> = (0..3).map(|i| Song { id: format!("n{i}"), artist: "Björk".into(), ..Default::default() }).collect();
        l.extend((0..3).map(|i| Song { id: format!("m{i}"), artist: "BJÖRK".into(), ..Default::default() }));
        l.extend((0..6).map(|i| Song { id: format!("o{i}"), artist: "Other".into(), ..Default::default() }));
        let out = weighted_shuffle(l, 5);
        assert_eq!(out.windows(2).filter(|w| w[0].artist.to_lowercase() == w[1].artist.to_lowercase()).count(), 0);
    }

    #[test]
    fn shuffle_avoids_the_same_album_too() {
        // two artists cannot avoid alternating; albums within can
        let mut l = Vec::new();
        for artist in ["A", "B", "C"] {
            for i in 0..6 {
                l.push(song(&format!("{artist}{i}"), "t", artist, &format!("{artist}{}", i % 3), "", 0));
            }
        }
        let out = weighted_shuffle(l, 11);
        assert_eq!(adjacent_artists(&out), 0);
        assert_eq!(out.windows(2).filter(|w| w[0].album == w[1].album).count(), 0);
    }

    #[test]
    fn long_queue_shuffles_fast() {
        // Five thousand songs: a shuffle that compared every song with every other would take seconds here.
        let l: Vec<Song> = (0..5_000).map(|i| song(&i.to_string(), "t", &format!("artist {}", i % 1500), "", "", 0)).collect();
        let started = std::time::Instant::now();
        let out = weighted_shuffle(l, 1);
        assert_eq!((out.len(), adjacent_artists(&out)), (5_000, 0));
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "5,000 songs took {:?} to shuffle", started.elapsed());
    }
}
