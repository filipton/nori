//! Mix calls over the index and history. The mixes themselves are nori-library's.

use crate::{db, model::Song, Core, Result};

pub mod board;

pub use nori_library::mixes::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Excludes a song from (or restores it to) every mix.
    pub fn mix_excluded_set(&self, song_id: String, excluded: bool) -> Result<()> {
        let c = self.db.lock();
        if excluded {
            c.execute("INSERT OR IGNORE INTO mix_excluded(server, song_id) VALUES(sid(), ?1)", [song_id])?;
        } else {
            c.execute("DELETE FROM mix_excluded WHERE server=sid() AND song_id=?1", [song_id])?;
        }
        Ok(())
    }
}

/// `seed` picks the draw: the same seed gives the same mix.
impl Core {
    /// Empty when the seed song is not indexed.
    pub(crate) fn mix_instant(&self, seed_song_id: String, limit: u32, seed: u64) -> Result<Vec<Song>> {
        Ok(instant(&self.db.lock(), &seed_song_id, limit as usize, seed, db::now_ms())?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::history::tests::{listen, skip, song, DAY, NOW};

    fn ids(l: &[Song]) -> Vec<&str> {
        l.iter().map(|s| s.id.as_str()).collect()
    }

    fn excluded(core: &Core) -> i64 {
        core.db.lock().query_row("SELECT count(*) FROM mix_excluded", [], |r| r.get(0)).unwrap()
    }

    fn adjacent_artists(l: &[Song]) -> usize {
        l.windows(2).filter(|w| w[0].artist == w[1].artist).count()
    }

    /// 4 genres x 5 artists x 10 songs, years spread over four decades.
    fn library(core: &Core) -> Vec<Song> {
        let genres = ["Rock", "Jazz", "Electronic", "Folk"];
        let mut all = Vec::new();
        for (g, genre) in genres.iter().enumerate() {
            for a in 0..5 {
                for t in 0..10 {
                    let artist = format!("{genre} Artist {a}");
                    all.push(song(&format!("{g}-{a}-{t}"), &format!("Track {t}"), &artist, &format!("{artist} LP{}", t / 5), genre, 1970 + (g as u32) * 10 + t as u32));
                }
            }
        }
        db::index(&mut core.db.lock(), &[], &[], &all).unwrap();
        all
    }

    #[test]
    fn mixes_are_empty_without_index() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        {
            let c = core.db.lock();
            assert!(quick_picks(&c, 20, 1, NOW).unwrap().is_empty());
            assert!(discover(&c, 20, 1, NOW).unwrap().is_empty());
            assert!(listen_again(&c, 20, 1, NOW).unwrap().is_empty());
            assert!(top(&c, 20, NOW).unwrap().is_empty());
        }
        assert!(core.mix_instant("nope".into(), 20, 1).unwrap().is_empty());
        assert_eq!(excluded(&core), 0);
    }

    #[test]
    fn picks_and_again() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let all = library(&core);
        let (loved, today, hated) = (&all[0], &all[1], &all[2]);
        for d in 4..8 {
            listen(&core, loved, NOW - d * DAY);
        }
        listen(&core, today, NOW - DAY);
        skip(&core, hated, NOW - 5 * DAY);
        let mut starred = all[60].clone();
        starred.starred = true;
        let mut rated = all[61].clone();
        rated.user_rating = 5;
        db::index(&mut core.db.lock(), &[], &[], &[starred, rated]).unwrap();

        let c = core.db.lock();
        let picks = quick_picks(&c, 10, 1, NOW).unwrap();
        let mut got = ids(&picks);
        got.sort();
        assert_eq!(got, vec![loved.id.as_str(), all[60].id.as_str(), all[61].id.as_str()]);
        assert_eq!(ids(&picks), ids(&quick_picks(&c, 10, 1, NOW).unwrap()));
        // Three days later, today's song has rested.
        assert!(ids(&quick_picks(&c, 10, 1, NOW + 3 * DAY).unwrap()).contains(&today.id.as_str()));
        assert!(quick_picks(&c, 0, 1, NOW).unwrap().is_empty());

        // Listen again is recent and top is by plays.
        let core = Core::new(String::new(), "t".into()).unwrap();
        let all = library(&core);
        for (i, s) in all.iter().step_by(10).take(5).enumerate() {
            for d in 0..=i as i64 {
                listen(&core, s, NOW - d * DAY);
            }
        }
        listen(&core, &all[55], NOW - 100 * DAY);
        let c = core.db.lock();
        assert_eq!(ids(&top(&c, 3, NOW).unwrap()), vec!["0-4-0", "0-3-0", "0-2-0"]);
        assert_eq!(top(&c, 50, NOW).unwrap().len(), 6);
        let again = listen_again(&c, 10, 2, NOW).unwrap();
        assert_eq!(again.len(), 5, "the one from 100 days ago is not recent");
        assert_eq!(ids(&again), ids(&listen_again(&c, 10, 2, NOW).unwrap()));
    }

    #[test]
    fn discover_mixes() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let all = library(&core);
        for s in all.iter().filter(|s| s.artist == "Jazz Artist 0").take(4) {
            for d in 0..3 {
                listen(&core, s, NOW - d * DAY);
            }
        }
        skip(&core, &all[0], NOW);
        let c = core.db.lock();
        let (mut jazz, mut total) = (0, 0);
        for seed in 0..20 {
            let mix = discover(&c, 20, seed, NOW).unwrap();
            assert_eq!(mix.len(), 20);
            assert_eq!(adjacent_artists(&mix), 0);
            assert!(mix.iter().all(|s| s.id != all[0].id), "skipped");
            assert!(mix.iter().all(|s| !(s.artist == "Jazz Artist 0" && s.title.as_str() < "Track 4")), "played");
            assert!(mix.iter().filter(|s| s.artist == "Jazz Artist 0").count() <= 4, "per-artist cap");
            jazz += mix.iter().filter(|s| s.genre.as_deref() == Some("Jazz")).count();
            total += mix.len();
        }
        assert!(jazz * 2 > total, "{jazz}/{total} jazz; a quarter of the library is");
        assert_eq!(ids(&discover(&c, 20, 5, NOW).unwrap()), ids(&discover(&c, 20, 5, NOW).unwrap()));
        assert_ne!(ids(&discover(&c, 20, 5, NOW).unwrap()), ids(&discover(&c, 20, 6, NOW).unwrap()));

        // Discover without history spans genres.
        let core = Core::new(String::new(), "t".into()).unwrap();
        library(&core);
        let mix = discover(&core.db.lock(), 30, 1, NOW).unwrap();
        assert_eq!(mix.len(), 30);
        assert!(mix.iter().map(|s| s.genre.clone()).collect::<HashSet<_>>().len() > 1);
    }

    #[test]
    fn instant_mix_stays_near_seed() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let all = library(&core);
        let seed_song = all.iter().find(|s| s.id == "2-1-3").unwrap();
        let mix = core.mix_instant(seed_song.id.clone(), 25, 1).unwrap();
        assert_eq!(mix.len(), 25);
        assert_eq!(&mix[0], seed_song);
        assert_eq!(mix.iter().filter(|s| s.id == seed_song.id).count(), 1);
        assert_ne!(mix[1].artist, seed_song.artist);
        let near = mix.iter().filter(|s| s.genre == seed_song.genre || s.year / 10 == seed_song.year / 10).count();
        assert_eq!(near, 25);
        assert!(mix.iter().filter(|s| s.genre == seed_song.genre).count() > 12);
        assert_eq!(ids(&mix), ids(&core.mix_instant(seed_song.id.clone(), 25, 1).unwrap()));
        assert_eq!(core.mix_instant(seed_song.id.clone(), 1, 1).unwrap(), vec![seed_song.clone()]);
        assert!(core.mix_instant(seed_song.id.clone(), 0, 1).unwrap().is_empty());

        // No genre or year: a mix of one.
        let bare = Song { id: "bare".into(), title: "Ünïcödé".into(), ..Default::default() };
        db::index(&mut core.db.lock(), &[], &[], std::slice::from_ref(&bare)).unwrap();
        assert_eq!(core.mix_instant("bare".into(), 10, 1).unwrap(), vec![bare]);
    }

    #[test]
    fn excluded_songs_stay_out_of_mixes() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let all = library(&core);
        let out: Vec<&Song> = all.iter().filter(|s| s.artist == "Rock Artist 0").collect();
        for s in &out {
            listen(&core, s, NOW - 10 * DAY);
            core.mix_excluded_set(s.id.clone(), true).unwrap();
        }
        core.mix_excluded_set(out[0].id.clone(), true).unwrap();
        assert_eq!(excluded(&core), 10);
        let c = core.db.lock();
        assert!(quick_picks(&c, 50, 1, NOW).unwrap().is_empty());
        assert!(listen_again(&c, 50, 1, NOW).unwrap().is_empty());
        assert!(top(&c, 50, NOW).unwrap().is_empty());
        drop(c);

        core.mix_excluded_set(out[0].id.clone(), false).unwrap();
        assert_eq!(ids(&top(&core.db.lock(), 50, NOW).unwrap()), vec![out[0].id.as_str()]);
        core.db.lock().execute("DELETE FROM mix_excluded", []).unwrap();
        assert_eq!(top(&core.db.lock(), 50, NOW).unwrap().len(), 10);
    }

    #[test]
    fn mix_queries_use_expression_indexes() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let c = core.db.lock();
        let plan = |cond: &str| -> String {
            let sql = format!("EXPLAIN QUERY PLAN SELECT i.json FROM items i LEFT JOIN song_stats s ON s.server=i.server AND s.song_id=i.id WHERE {SONGS} AND {cond}");
            let mut st = c.prepare(&sql).unwrap();
            let rows = st.query_map([], |r| r.get::<_, String>(3)).unwrap();
            rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("\n")
        };
        assert!(plan("json_extract(i.json,'$.genre')='x' COLLATE NOCASE").contains("items_genre"));
        assert!(plan("json_extract(i.json,'$.artistId')='x'").contains("items_artist"));
        assert!(plan("json_extract(i.json,'$.year') BETWEEN 1 AND 2").contains("items_year"));
        assert!(plan("json_extract(i.json,'$.starred')=1").contains("items_starred"));
        assert!(plan("json_extract(i.json,'$.userRating')>=4").contains("items_rated"));
    }
}
