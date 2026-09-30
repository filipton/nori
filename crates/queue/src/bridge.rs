//! The offline bridge: while the server is unreachable and the next song is not downloaded, play
//! downloads, then resume the queue where it was parked. This picks the songs.

use nori_model::Song;
use nori_player::queue::shuffle;

/// Downloads added per bridge batch.
pub const BATCH: u32 = 12;

/// Similarity of a download to `seed`: artist name, album, genre, artist id, then starred.
fn score(seed: Option<&Song>, s: &Song) -> i32 {
    let mut v = 0;
    if let Some(seed) = seed {
        if !seed.artist.trim().is_empty() && s.artist.to_lowercase() == seed.artist.to_lowercase() {
            v += 4;
        }
        if seed.album_id.is_some() && s.album_id == seed.album_id {
            v += 3;
        }
        if seed.genre.as_deref().is_some_and(|g| !g.trim().is_empty()) && s.genre == seed.genre {
            v += 2;
        }
        if seed.artist_id.is_some() && s.artist_id == seed.artist_id {
            v += 2;
        }
    }
    if s.starred {
        v += 1;
    }
    v
}

/// Up to `n` of `pool`, most similar first (shuffled among equals), excluding `exclude` and provider songs.
pub fn pick(seed: Option<&Song>, pool: &[Song], exclude: &[String], n: usize, rng_seed: u64) -> Vec<Song> {
    let mut scored: Vec<(i32, &Song)> = pool
        .iter()
        .filter(|s| !exclude.contains(&s.id) && !s.is_provider())
        .map(|s| (score(seed, s), s))
        .collect();
    // Shuffle, then stable sort: equal scores stay shuffled.
    shuffle(&mut scored, rng_seed);
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    scored.into_iter().take(n).map(|(_, s)| s.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, artist: &str, album: &str, starred: bool) -> Song {
        Song { id: id.into(), artist: artist.into(), album_id: Some(album.into()), starred, ..Default::default() }
    }

    #[test]
    fn pick_orders_by_similarity_and_excludes_queued() {
        let seed = song("s", "Radiohead", "ok", false);
        let pool = [song("a", "Muse", "x", false), song("b", "radiohead", "kid", false), song("c", "Radiohead", "ok", false), song("d", "Muse", "y", true), song("q", "Radiohead", "ok", false)];
        let p = pick(Some(&seed), &pool, &["q".to_string()], 3, 7);
        let ids: Vec<&str> = p.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(&ids[..2], &["c", "b"], "artist and album, then artist");
        assert_eq!(ids[2], "d", "then starred");
        assert!(!ids.contains(&"q"));
    }
}
