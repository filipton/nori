//! Local play history and the taste model (the server's counts are per account and know no skips).
//!
//! One `plays` row per listen, and `song_stats`, a per-song roll-up written in the same transaction.
//! A listen under 2 s is dropped; under 30 s and 30 % is a skip; 90 % is complete; else partial.
//!
//! Taste: each listen weighs 1 complete, its heard fraction (0.3..1) partial, 0.5 with no known length,
//! -0.6 skipped, decayed with a 30-day half-life. `song_stats.taste` stores the sum scaled to a fixed
//! epoch, so recording is one addition and `ORDER BY taste` is today's order; [`taste`] adds stars and
//! ratings. Provider songs are never recorded.

use std::collections::HashMap;

use nori_db as db;
use nori_model::model::*;
use rusqlite::{params, Connection, OptionalExtension};

use crate::browse::DAY_MS;

const HALF_LIFE_MS: f64 = 30.0 * 86_400_000.0;
/// 2020-01-01. f64 holds `2^((t - EPOCH) / HALF_LIFE)` until the 2100s.
const TASTE_EPOCH_MS: i64 = 1_577_836_800_000;
const MIN_HEARD_MS: i64 = 2_000;

/// (completed, skipped, taste weight)
fn classify(heard_ms: i64, duration_ms: i64) -> (bool, bool, f64) {
    if duration_ms <= 0 {
        let skipped = heard_ms < 30_000;
        return (false, skipped, if skipped { -0.6 } else { 0.5 });
    }
    let part = heard_ms as f64 / duration_ms as f64;
    if heard_ms < 30_000 && part < 0.3 {
        (false, true, -0.6)
    } else if part >= 0.9 {
        (true, false, 1.0)
    } else {
        (false, false, part.clamp(0.3, 1.0))
    }
}

fn scale(t_ms: i64) -> f64 {
    ((t_ms - TASTE_EPOCH_MS) as f64 / HALF_LIFE_MS).exp2()
}

/// `song_stats.taste` as of `now_ms`.
pub fn decayed(stored: f64, now_ms: i64) -> f64 {
    stored / scale(now_ms)
}

/// The full score: decayed plays and skips plus the explicit signals.
pub fn taste(song: &Song, stored: f64, now_ms: i64) -> f64 {
    let rating = match song.user_rating {
        1 => -3.0,
        2 => -1.5,
        4 => 1.0,
        5 => 2.0,
        _ => 0.0,
    };
    decayed(stored, now_ms) + if song.starred { 1.5 } else { 0.0 } + rating
}

/// False when the listen was not worth recording.
pub fn record(c: &mut Connection, song: &Song, started_ms: i64, heard_ms: i64, tz_offset_ms: i32, now_ms: i64) -> rusqlite::Result<bool> {
    if song.id.is_empty() || song.is_provider() || heard_ms < MIN_HEARD_MS {
        return Ok(false);
    }
    // A wrong clock must not mint a score that outlives everything.
    let started_ms = started_ms.clamp(TASTE_EPOCH_MS, now_ms.max(TASTE_EPOCH_MS) + DAY_MS);
    let known: Option<i64> = c.prepare_cached("SELECT 1 FROM items WHERE server=sid() AND kind=?1 AND id=?2")?.query_row(params![db::SONG, song.id], |r| r.get(0)).optional()?;
    if known.is_none() {
        db::index(c, &[], &[], std::slice::from_ref(song))?;
    }
    let duration_ms = song.duration as i64 * 1000;
    let (completed, skipped, weight) = classify(heard_ms, duration_ms);
    let local = started_ms + tz_offset_ms as i64;
    let tx = c.transaction()?;
    tx.prepare_cached("INSERT INTO plays(server, song_id, started_ms, heard_ms, duration_ms, completed, skipped, hour, day) VALUES(sid(), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?.execute(params![
        song.id,
        started_ms,
        heard_ms,
        duration_ms,
        completed,
        skipped,
        local.rem_euclid(DAY_MS) / 3_600_000,
        local.div_euclid(DAY_MS)
    ])?;
    tx.prepare_cached(
        "INSERT INTO song_stats(server, song_id, plays, skips, last_played_ms, heard_ms_total, taste) VALUES(sid(), ?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(server, song_id) DO UPDATE SET plays=plays+excluded.plays, skips=skips+excluded.skips,
           last_played_ms=max(last_played_ms, excluded.last_played_ms), heard_ms_total=heard_ms_total+excluded.heard_ms_total, taste=taste+excluded.taste",
    )?
    .execute(params![song.id, !skipped, skipped, if skipped { 0 } else { started_ms }, heard_ms, weight * scale(started_ms)])?;
    tx.commit()?;
    Ok(true)
}

/// A `plays` row as the history lists it; none when its song no longer reads.
pub fn entry(json: String, started_ms: i64, heard_ms: i64, completed: bool, skipped: bool) -> Option<HistoryEntry> {
    Some(HistoryEntry { song: serde_json::from_str(&json).ok()?, started_ms, heard_ms, completed, skipped })
}

fn tally(map: &mut HashMap<String, TopEntry>, key: String, id: &str, name: &str, song: &TopSong) {
    let t = map.entry(key).or_insert_with(|| TopEntry { id: id.to_string(), name: name.to_string(), ..Default::default() });
    // Songs arrive most played first, so the first cover seen is the one of the entry's top song.
    if t.cover_art.is_none() {
        t.cover_art = song.song.cover_art.clone();
    }
    t.plays += song.plays;
    t.listened_ms += song.listened_ms;
}

/// (how many there are, the first `top` of them)
fn ranked(map: HashMap<String, TopEntry>, top: usize) -> (u32, Vec<TopEntry>) {
    let n = map.len() as u32;
    let mut l: Vec<TopEntry> = map.into_values().collect();
    l.sort_by(|a, b| b.plays.cmp(&a.plays).then(b.listened_ms.cmp(&a.listened_ms)).then_with(|| a.name.cmp(&b.name)));
    l.truncate(top);
    (n, l)
}

/// The listening statistics of the plays between `from_ms` and `to_ms`, with the `top` songs, artists
/// and albums.
pub fn summary(c: &Connection, from_ms: i64, to_ms: i64, top: u32) -> rusqlite::Result<ListeningStats> {
    let top = top as usize;
    let mut out = ListeningStats { plays_per_hour: vec![0; 24], plays_per_weekday: vec![0; 7], ..Default::default() };

    // Per song in SQL, so a year of listening crosses into Rust as one row per distinct song.
    let mut songs: Vec<TopSong> = Vec::new();
    {
        let mut st = c.prepare_cached(
            "SELECT i.json, p.plays, p.skips, p.ms FROM
               (SELECT song_id, sum(skipped=0) plays, sum(skipped) skips, sum(heard_ms) ms FROM plays WHERE server=sid() AND started_ms>=?1 AND started_ms<?2 GROUP BY song_id) p
             LEFT JOIN items i ON i.server=sid() AND i.kind=2 AND i.id=p.song_id ORDER BY p.plays DESC, p.ms DESC, p.song_id",
        )?;
        let mut rows = st.query(params![from_ms, to_ms])?;
        while let Some(r) = rows.next()? {
            let (json, plays, skips, ms): (Option<String>, u32, u32, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            out.plays += plays;
            out.skips += skips;
            out.listened_ms += ms;
            if plays == 0 {
                continue;
            }
            out.distinct_songs += 1;
            // No json: the index was dropped (server change) after the listen. It still counts, it just has no name.
            if let Some(song) = json.and_then(|j| serde_json::from_str::<Song>(&j).ok()) {
                songs.push(TopSong { song, plays, listened_ms: ms });
            }
        }
    }
    let (mut artists, mut albums, mut genres) = (HashMap::new(), HashMap::new(), HashMap::new());
    for t in &songs {
        let s = &t.song;
        if !s.artist.is_empty() || s.artist_id.is_some() {
            let id = s.artist_id.as_deref().unwrap_or("");
            tally(&mut artists, if id.is_empty() { s.artist.to_lowercase() } else { id.to_string() }, id, &s.artist, t);
        }
        if !s.album.is_empty() || s.album_id.is_some() {
            let id = s.album_id.as_deref().unwrap_or("");
            tally(&mut albums, if id.is_empty() { format!("{}\0{}", s.artist, s.album).to_lowercase() } else { id.to_string() }, id, &s.album, t);
        }
        if let Some(g) = s.genre.as_deref().filter(|g| !g.is_empty()) {
            tally(&mut genres, g.to_lowercase(), "", g, t);
        }
    }
    (out.distinct_artists, out.top_artists) = ranked(artists, top);
    (out.distinct_albums, out.top_albums) = ranked(albums, top);
    (_, out.top_genres) = ranked(genres, top);
    songs.truncate(top);
    out.top_songs = songs;

    let mut st = c.prepare_cached("SELECT hour, count(*) FROM plays WHERE server=sid() AND started_ms>=?1 AND started_ms<?2 AND skipped=0 GROUP BY hour")?;
    let mut rows = st.query(params![from_ms, to_ms])?;
    while let Some(r) = rows.next()? {
        let (hour, n): (i64, u32) = (r.get(0)?, r.get(1)?);
        out.plays_per_hour[hour.clamp(0, 23) as usize] += n;
    }

    let mut st = c.prepare_cached("SELECT day, count(*) FROM plays WHERE server=sid() AND started_ms>=?1 AND started_ms<?2 AND skipped=0 GROUP BY day ORDER BY day")?;
    let mut rows = st.query(params![from_ms, to_ms])?;
    let (mut prev, mut run) = (i64::MIN, 0u32);
    while let Some(r) = rows.next()? {
        let (day, n): (i64, u32) = (r.get(0)?, r.get(1)?);
        // Day 0 (1970-01-01) was a Thursday.
        out.plays_per_weekday[(day + 3).rem_euclid(7) as usize] += n;
        out.active_days += 1;
        run = if day == prev.wrapping_add(1) { run + 1 } else { 1 };
        out.longest_streak_days = out.longest_streak_days.max(run);
        prev = day;
    }

    out.first_play = c
        .prepare_cached(
            "SELECT i.json, p.started_ms, p.heard_ms, p.completed, p.skipped FROM plays p JOIN items i ON i.server=sid() AND i.kind=2 AND i.id=p.song_id
             WHERE p.server=sid() AND p.started_ms>=?1 AND p.started_ms<?2 AND p.skipped=0 ORDER BY p.started_ms, p.rowid LIMIT 1",
        )?
        .query_row(params![from_ms, to_ms], |r| Ok(entry(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .optional()?
        .flatten();
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const NOW: i64 = 1_788_000_000_000; // 2026-08-29

    pub(crate) const DAY: i64 = DAY_MS;

    pub(crate) fn song(id: &str, title: &str, artist: &str, album: &str, genre: &str, year: u32) -> Song {
        Song {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            artist_id: (!artist.is_empty()).then(|| format!("ar-{}", artist.to_lowercase())),
            album_id: (!album.is_empty()).then(|| format!("al-{}", album.to_lowercase())),
            cover_art: Some(format!("cv-{id}")),
            genre: (!genre.is_empty()).then(|| genre.to_string()),
            year,
            duration: 200,
            suffix: "flac".into(),
            ..Default::default()
        }
        .dressed()
    }

    #[test]
    fn listens_are_classified() {
        assert_eq!(classify(200_000, 200_000), (true, false, 1.0));
        assert_eq!(classify(180_000, 200_000), (true, false, 1.0));
        assert_eq!(classify(100_000, 200_000), (false, false, 0.5));
        assert_eq!(classify(10_000, 200_000), (false, true, -0.6));
        // long track: 40 s is under 30 % but no longer a reflex skip
        assert_eq!(classify(40_000, 600_000), (false, false, 0.3));
        // short interlude mostly heard
        assert_eq!(classify(15_000, 20_000), (false, false, 0.75));
        // unknown duration
        assert_eq!(classify(10_000, 0), (false, true, -0.6));
        assert_eq!(classify(60_000, 0), (false, false, 0.5));
    }
}
