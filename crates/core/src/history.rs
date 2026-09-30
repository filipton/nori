//! Play history and taste model reads over the core's database. The model is nori-library's.

use rusqlite::params;

use crate::browse::HistoryAfter;
use crate::{db, model::*, Core, Result};

pub use nori_library::history::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Deletes all listens and song stats (not mix exclusions or smart playlists).
    pub fn history_clear(&self) -> Result<()> {
        self.db.lock().execute_batch("DELETE FROM plays WHERE server=sid(); DELETE FROM song_stats WHERE server=sid();")?;
        Ok(())
    }
}

impl Core {
    /// Up to `limit` listens older than `after` (None: the newest), newest first, omitting songs no longer
    /// indexed, and where the next page starts (None after the last).
    pub fn history_recent(&self, limit: u32, after: Option<HistoryAfter>, include_skipped: bool) -> Result<(Vec<HistoryEntry>, Option<HistoryAfter>)> {
        let after = after.unwrap_or(HistoryAfter { started_ms: i64::MAX, row: i64::MAX });
        let c = self.db.lock();
        let mut st = c.prepare_cached(
            "SELECT i.json, p.started_ms, p.heard_ms, p.completed, p.skipped, p.rowid FROM plays p JOIN items i ON i.server=sid() AND i.kind=2 AND i.id=p.song_id
             WHERE p.server=sid() AND p.skipped<=?1 AND (p.started_ms, p.rowid) < (?3, ?4) ORDER BY p.started_ms DESC, p.rowid DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![include_skipped, limit, after.started_ms, after.row], |r| {
            Ok((entry(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?), HistoryAfter { started_ms: r.get(1)?, row: r.get(5)? }))
        })?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let next = rows.last().filter(|_| rows.len() == limit as usize).map(|(_, at)| *at);
        Ok((rows.into_iter().filter_map(|(e, _)| e).collect(), next))
    }

    /// Stats of the played songs among `ids`.
    pub fn song_stats(&self, ids: Vec<String>) -> Result<Vec<SongStat>> {
        let now = db::now_ms();
        let c = self.db.lock();
        let mut st = c.prepare_cached(
            "SELECT s.song_id, s.plays, s.skips, s.last_played_ms, s.heard_ms_total, s.taste, i.json FROM song_stats s
             LEFT JOIN items i ON i.server=sid() AND i.kind=2 AND i.id=s.song_id WHERE s.server=sid() AND s.song_id IN (SELECT value FROM json_each(?1))",
        )?;
        let rows = st.query_map([serde_json::to_string(&ids).unwrap_or_default()], |r| {
            let song: Song = r.get::<_, Option<String>>(6)?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
            Ok(SongStat { song_id: r.get(0)?, plays: r.get(1)?, skips: r.get(2)?, last_played_ms: r.get(3)?, heard_ms_total: r.get(4)?, taste: taste(&song, r.get(5)?, now) })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const NOW: i64 = 1_788_000_000_000; // 2026-08-29

    pub(crate) const DAY: i64 = 86_400_000;

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

    /// A full listen at `at`.
    pub(crate) fn listen(core: &Core, s: &Song, at: i64) {
        assert!(record(&mut core.db.lock(), s, at, s.duration as i64 * 1000, 0, NOW).unwrap());
    }

    pub(crate) fn skip(core: &Core, s: &Song, at: i64) {
        assert!(record(&mut core.db.lock(), s, at, 5_000, 0, NOW).unwrap());
    }

    /// Records a listen; false when nothing was recorded.
    fn rec(core: &Core, s: Song, started_ms: i64, heard_ms: i64) -> bool {
        record(&mut core.db.lock(), &s, started_ms, heard_ms, 0, NOW).unwrap()
    }

    fn stats(core: &Core, from_ms: i64, to_ms: i64, top: u32) -> ListeningStats {
        summary(&core.db.lock(), from_ms, to_ms, top).unwrap()
    }

    #[test]
    fn record_indexes_song_and_rolls_up_stats() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let s = song("s1", "Dogs", "Pink Floyd", "Animals", "Rock", 1977);
        assert!(rec(&core, s.clone(), NOW - DAY, 200_000));
        assert!(rec(&core, s.clone(), NOW - DAY + 1, 5_000));
        assert!(!rec(&core, s.clone(), NOW, 1_500), "under 2 s");
        // Indexed by the listen alone.
        assert_eq!(core.index_size().unwrap().songs, 1);
        assert_eq!(core.local_search("dogs".into(), 5).unwrap().songs, vec![s.clone()]);

        let st = core.song_stats(vec!["s1".into(), "missing".into()]).unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!((st[0].plays, st[0].skips, st[0].last_played_ms, st[0].heard_ms_total), (1, 1, NOW - DAY, 205_000));
        assert!(core.song_stats(vec![]).unwrap().is_empty());

        let h = core.history_recent(10, None, true).unwrap().0;
        assert_eq!(h.len(), 2);
        assert!(h[0].skipped && !h[0].completed && h[1].completed);
        assert_eq!(h[1].song, s);
        assert_eq!(core.history_recent(10, None, false).unwrap().0.len(), 1);
        let (first, next) = core.history_recent(1, None, true).unwrap();
        assert!(first[0].skipped);
        assert!(!core.history_recent(1, next, true).unwrap().0[0].skipped);
        assert!(core.history_recent(0, None, true).unwrap().0.is_empty());

        core.history_clear().unwrap();
        assert!(core.history_recent(10, None, true).unwrap().0.is_empty());
        assert!(core.song_stats(vec!["s1".into()]).unwrap().is_empty());
    }

    #[test]
    fn record_keeps_newer_index_entry() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let mut s = song("s1", "Dogs", "Pink Floyd", "Animals", "Rock", 1977);
        s.starred = true;
        db::index(&mut core.db.lock(), &[], &[], std::slice::from_ref(&s)).unwrap();
        let stale = Song { starred: false, ..s.clone() };
        rec(&core, stale, NOW, 200_000);
        assert!(core.history_recent(1, None, true).unwrap().0[0].song.starred);
    }

    #[test]
    fn provider_tracks_are_not_recorded() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        for id in ["ext-deezer-song-7", "pl-deezer-9", ""] {
            assert!(!rec(&core, Song { id: id.into(), duration: 100, ..Default::default() }, NOW, 100_000));
        }
        assert!(!rec(&core, Song { id: "x".into(), is_external: true, ..Default::default() }, NOW, 100_000));
        assert_eq!(core.index_size().unwrap().songs, 0);
        assert!(core.history_recent(10, None, true).unwrap().0.is_empty());
    }

    #[test]
    fn taste_decays_and_follows_ratings() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let (fresh, old, skipped) = (song("a", "A", "X", "", "", 0), song("b", "B", "X", "", "", 0), song("c", "C", "X", "", "", 0));
        listen(&core, &fresh, NOW);
        listen(&core, &old, NOW - 30 * DAY);
        skip(&core, &skipped, NOW);
        let stored = |id: &str| -> f64 { core.db.lock().query_row("SELECT taste FROM song_stats WHERE server=sid() AND song_id=?1", [id], |r| r.get(0)).unwrap() };
        assert!((decayed(stored("a"), NOW) - 1.0).abs() < 1e-9);
        assert!((decayed(stored("b"), NOW) - 0.5).abs() < 1e-9);
        assert!((decayed(stored("c"), NOW) + 0.6).abs() < 1e-9);
        // Stored values already sort like today's.
        assert!(stored("a") > stored("b") && stored("b") > stored("c"));
        // Recording out of order gives the same result.
        listen(&core, &old, NOW - 60 * DAY);
        assert!((decayed(stored("b"), NOW) - 0.75).abs() < 1e-9);

        let plain = Song::default();
        assert_eq!(taste(&Song { starred: true, user_rating: 5, ..plain.clone() }, 0.0, NOW), 3.5);
        assert_eq!(taste(&Song { user_rating: 1, ..plain.clone() }, 0.0, NOW), -3.0);
        assert_eq!(taste(&Song { user_rating: 3, ..plain }, 0.0, NOW), 0.0);
    }

    #[test]
    fn absurd_timestamps_keep_taste_finite() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let s = song("a", "A", "X", "", "", 0);
        assert!(record(&mut core.db.lock(), &s, i64::MAX / 2, 200_000, 0, NOW).unwrap());
        assert!(record(&mut core.db.lock(), &s, -5, 200_000, 0, NOW).unwrap());
        let t = core.song_stats(vec!["a".into()]).unwrap()[0].taste;
        assert!(t.is_finite() && t < 3.0, "{t}");
    }

    #[test]
    fn empty_summary_has_full_charts() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let s = stats(&core, 0, i64::MAX, 10);
        assert_eq!((s.plays, s.listened_ms, s.longest_streak_days), (0, 0, 0));
        assert_eq!((s.plays_per_hour.len(), s.plays_per_weekday.len()), (24, 7));
        assert!(s.first_play.is_none() && s.top_songs.is_empty());
    }

    #[test]
    fn summary_counts_tops_and_charts() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let dogs = song("s1", "Dogs", "Pink Floyd", "Animals", "Rock", 1977);
        let pigs = song("s2", "Pigs", "Pink Floyd", "Animals", "Rock", 1977);
        let bjork = song("s3", "Jóga", "Björk", "Homogenic", "Electronic", 1997);
        // 2026-06-01 00:00 UTC, a Monday.
        let monday = 1_780_272_000_000;
        let at = |day: i64, hour: i64| monday + day * DAY + hour * 3_600_000;
        listen(&core, &dogs, at(0, 8));
        listen(&core, &dogs, at(1, 8));
        listen(&core, &pigs, at(2, 23));
        skip(&core, &pigs, at(2, 23) + 1);
        listen(&core, &bjork, at(4, 8));
        // Outside the period.
        listen(&core, &bjork, at(40, 8));

        let s = stats(&core, monday, monday + 7 * DAY, 2);
        assert_eq!((s.plays, s.skips, s.listened_ms), (4, 1, 805_000));
        assert_eq!((s.distinct_songs, s.distinct_artists, s.distinct_albums), (3, 2, 2));
        assert_eq!(s.top_songs.len(), 2);
        assert_eq!((s.top_songs[0].song.id.as_str(), s.top_songs[0].plays, s.top_songs[0].listened_ms), ("s1", 2, 400_000));
        assert_eq!((s.top_artists[0].name.as_str(), s.top_artists[0].plays, s.top_artists[0].id.as_str()), ("Pink Floyd", 3, "ar-pink floyd"));
        assert_eq!(s.top_artists[0].cover_art.as_deref(), Some("cv-s1"));
        assert_eq!(s.top_artists[1].name, "Björk");
        assert_eq!((s.top_albums[0].name.as_str(), s.top_albums[0].listened_ms), ("Animals", 605_000));
        assert_eq!((s.top_genres[0].name.as_str(), s.top_genres[0].plays), ("Rock", 3));
        assert_eq!((s.plays_per_hour[8], s.plays_per_hour[23], s.plays_per_hour.iter().sum::<u32>()), (3, 1, 4));
        assert_eq!(s.plays_per_weekday, vec![1, 1, 1, 0, 1, 0, 0]);
        assert_eq!((s.active_days, s.longest_streak_days), (4, 3));
        assert_eq!(s.first_play.unwrap().song, dogs);
    }

    #[test]
    fn charts_use_local_time() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let s = song("s1", "A", "X", "", "", 0);
        // Sunday 23:30 UTC is Monday 01:30 at UTC+2.
        let sunday_late = 1_780_272_000_000 - 30 * 60_000;
        record(&mut core.db.lock(), &s, sunday_late, 200_000, 2 * 3_600_000, NOW).unwrap();
        let st = stats(&core, 0, i64::MAX, 1);
        assert_eq!((st.plays_per_hour[1], st.plays_per_weekday[0]), (1, 1));
    }

    #[test]
    fn listens_survive_cleared_index() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        listen(&core, &song("s1", "A", "X", "", "", 0), NOW);
        db::clear_library(&core.db.lock()).unwrap();
        assert!(core.history_recent(10, None, true).unwrap().0.is_empty());
        let s = stats(&core, 0, i64::MAX, 5);
        assert_eq!((s.plays, s.distinct_songs, s.top_songs.len()), (1, 1, 0));
        assert_eq!(core.song_stats(vec!["s1".into()]).unwrap()[0].plays, 1);
    }
}
