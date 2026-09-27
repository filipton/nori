//! Refilling the queue as the client's calls: the songs or albums fetched from the server. When to fetch
//! and what to ask for are nori-queue's.

use std::collections::{HashMap, HashSet};

use futures_util::future::join_all;

use crate::cache_policy::{Page, Read};
use crate::client::Client;
use crate::settings::{AutoFillBasis, AutoFillKind};
use crate::{queue, Song};

pub use nori_queue::autofill::*;

impl Client {
    /// The first answer a screen would show: the stored one when there is one, else the server's.
    pub(crate) async fn first(&self, read: Read) -> Got<Page> {
        if let Ok(stored) = self.read_stored(read.clone()) {
            if let Some(p) = stored.page {
                return Ok(p);
            }
        }
        self.read_now(read).await
    }

    pub(crate) async fn songs(&self, read: Read) -> Got<Vec<Song>> {
        match self.read_now(read).await? {
            Page::Songs { v } => Ok(v),
            _ => Ok(Vec::new()),
        }
    }

    async fn artist_albums(&self, seed: &Song) -> Got<Vec<crate::Album>> {
        let Some(id) = seed.artist_id.clone() else { return Ok(Vec::new()) };
        match self.first(Read::ArtistById { id }).await? {
            Page::ArtistPage { v } => Ok(v.albums),
            _ => Ok(Vec::new()),
        }
    }

    /// Loose songs to carry on with. Whatever the basis, the order they come back in is kept.
    async fn next_songs(&self, seed: &Song, basis: AutoFillBasis) -> Got<Vec<Song>> {
        Ok(match basis {
            // The artist's best-known songs first, then the rest of their records, so a long evening
            // does not stop after ten tracks.
            AutoFillBasis::Artist => {
                let top = match self.first(Read::TopSongs { artist: seed.artist.clone() }).await? {
                    Page::Songs { v } => v,
                    _ => Vec::new(),
                };
                if !top.is_empty() {
                    return Ok(top);
                }
                // Side by side: three round trips one after another were three times the wait.
                let albums = self.artist_albums(seed).await?;
                let reads = albums.into_iter().take(3).map(|a| self.songs(Read::AlbumSongs { id: a.id }));
                let mut all = Vec::new();
                for songs in join_all(reads).await {
                    all.extend(songs?);
                }
                all
            }
            AutoFillBasis::Genre => match &seed.genre {
                Some(g) => shuffled(self.songs(Read::SongsByGenre { genre: g.clone(), count: 100 }).await?),
                None => Vec::new(),
            },
            // Out of the offline index rather than the server: nothing in Subsonic asks for a decade of songs.
            AutoFillBasis::Era => match era(seed) {
                Some((from, to)) => shuffled(self.core.browse_songs("playCount".into(), true, false, from, to, 0, 100).unwrap_or_default()),
                None => Vec::new(),
            },
            _ => self.songs(Read::SimilarSongs { id: seed.id.clone(), count: 25 }).await?,
        })
    }

    /// The albums one whole record is picked from, by the same four bases.
    async fn album_candidates(&self, seed: &Song, basis: AutoFillBasis, remote: bool) -> Got<Vec<String>> {
        let ids = |v: Vec<crate::Album>| v.into_iter().filter(|a| remote || (!a.is_external && !crate::db::external(&a.id))).map(|a| a.id).collect::<Vec<_>>();
        let albums = |p: Page| match p {
            Page::Albums { v } => v,
            _ => Vec::new(),
        };
        Ok(match basis {
            AutoFillBasis::Artist => ids(self.artist_albums(seed).await?),
            AutoFillBasis::Genre => match &seed.genre {
                Some(g) => shuffled(ids(albums(
                    self.first(Read::AlbumList { kind: "byGenre".into(), size: 30, offset: 0, genre: Some(g.clone()) }).await?,
                ))),
                None => Vec::new(),
            },
            AutoFillBasis::Era => match era(seed) {
                Some((from, to)) => shuffled(ids(albums(self.first(Read::AlbumsByYear { from: from as i32, to: to as i32, size: 30, offset: 0 }).await?))),
                None => Vec::new(),
            },
            // The records the songs the server calls similar come from.
            _ => {
                let mut seen = HashSet::new();
                self.songs(Read::SimilarSongs { id: seed.id.clone(), count: 50 })
                    .await?
                    .into_iter()
                    .filter(|s| allowed(s, remote))
                    .filter_map(|s| s.album_id)
                    .filter(|a| seen.insert(a.clone()))
                    .collect()
            }
        })
    }

    /// One whole album, in its own order, for someone who listens to records. One already in the queue
    /// is passed over, so an evening moves on rather than playing the same record twice. A single is an
    /// album as far as the server is concerned, and stopping the evening on one track is not what
    /// "carry on with albums" means: the first record with a side to it wins, and a short one is only
    /// taken if nothing else is on offer. The records picked or played lately go to the back, so the
    /// same single does not lead to the same album every time (see nori-queue's `rank`).
    async fn next_album(&self, seed: &Song, basis: AutoFillBasis, queued: &HashSet<&str>, played: &HashSet<String>, remote: bool) -> Vec<Song> {
        let candidates = self.album_candidates(seed, basis, remote).await.unwrap_or_default();
        let pool: Vec<String> = candidates.into_iter().filter(|a| seed.album_id.as_ref() != Some(a) && !played.contains(a)).collect();
        let ranked = self.turns(Picked::Album, pool);
        let mut short = Vec::new();
        let mut short_id = None;
        // The candidates are read side by side (six round trips one after another could be a second or
        // more on a phone) and still chosen in their ranked order.
        let picks: Vec<String> = ranked.into_iter().take(ALBUM_TRIES).collect();
        let reads = join_all(picks.iter().map(|pick| self.songs(Read::AlbumSongs { id: pick.clone() }))).await;
        for (pick, read) in picks.into_iter().zip(reads) {
            let songs: Vec<Song> = read.unwrap_or_default().into_iter().filter(|s| !queued.contains(s.id.as_str()) && allowed(s, remote)).collect();
            if songs.len() >= ALBUM_MIN {
                self.picked(Picked::Album, &[pick]);
                return songs;
            }
            if songs.len() > short.len() {
                short = songs;
                short_id = Some(pick);
            }
        }
        if let Some(id) = short_id {
            self.picked(Picked::Album, &[id]);
        }
        short
    }

    /// `candidates` ranked so that what was picked or played lately comes last; as they are when the
    /// database will not say.
    fn turns(&self, kind: Picked, candidates: Vec<String>) -> Vec<String> {
        let c = self.core.db.lock();
        let now = crate::db::now_ms();
        let used = match kind {
            Picked::Album => album_use(&c, now),
            Picked::Songs => song_use(&c, now),
        };
        match used {
            Ok(used) => rank(candidates, &used, now, seed_now()),
            Err(_) => candidates,
        }
    }

    /// Remembers what was just queued, so the next refill takes turns with it.
    fn picked(&self, kind: Picked, ids: &[String]) {
        let _ = note(&self.core.db.lock(), kind, ids, crate::db::now_ms());
    }

    /// What the queue carries on from when its last song is `last`. A provider's song (octo-fiesta) is
    /// no seed: the server's similar songs, top songs and albums for it are the provider's too, and each
    /// one played would be downloaded into the library. The library's side of it is: a song of the
    /// same artist in the library, else the queue's last song from the library. None when there is
    /// neither.
    fn library_seed(&self, last: Song, ids: &[String]) -> Option<Song> {
        if in_library(&last) {
            return Some(last);
        }
        let artist = last.artist.trim();
        let same_artist = (!artist.is_empty())
            .then(|| self.core.local_search(artist.to_string(), 50).ok())
            .flatten()
            .and_then(|found| found.songs.into_iter().find(|s| in_library(s) && s.artist.trim().eq_ignore_ascii_case(artist)));
        same_artist.or_else(|| ids.iter().rev().filter_map(|id| queue::queue_song(id.clone())).find(in_library))
    }

    /// Random albums of the library, whole, none already in the queue: a "shuffle albums" queue's refill.
    async fn random_albums(&self, ids: &[String]) -> Vec<Song> {
        let queued: HashSet<String> = queue::queue_albums(ids.to_vec()).into_iter().collect();
        let albums = match self.read_now(Read::AlbumList { kind: "random".into(), size: RANDOM_ALBUMS, offset: 0, genre: None }).await {
            Ok(Page::Albums { v }) => v.into_iter().filter(|a| !queued.contains(&a.id)).collect(),
            _ => Vec::new(),
        };
        let fresh = self.library_albums(albums, RANDOM_ALBUMS as usize).await;
        queue::queue_register(fresh.clone());
        fresh
    }

    /// Songs of the library at random, for a queue of nothing but a provider's songs, and a "shuffle
    /// songs" queue's refill.
    async fn random_library_songs(&self, queued: &HashSet<&str>, remote: bool) -> Vec<Song> {
        let offered = self.songs(Read::RandomSongs { size: 50, genre: None }).await.unwrap_or_default();
        let fresh: Vec<Song> = offered.into_iter().filter(|s| !queued.contains(s.id.as_str()) && allowed(s, remote)).take(SONGS).collect();
        queue::queue_register(fresh.clone());
        fresh
    }
}

/// Whether `s` is the library's own: never a provider's song (octo-fiesta's `isExternal`, `ext-` and `pl-`
/// ids), which autoplay must not queue - playing one makes the server download it into the library.
fn in_library(s: &Song) -> bool {
    !s.is_external && !crate::db::external(&s.id)
}

/// Whether autoplay may queue `s`: the library's own, or anything when a provider's songs are let in.
fn allowed(s: &Song, remote: bool) -> bool {
    remote || in_library(s)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// What to append to the queue after the song playing, as the settings say (songs or an album, and
    /// chosen by what), a provider's song only when `autoFillRemote` says so. After a provider's song it
    /// carries on from the library ([`Client::library_seed`]) unless that setting is on. Nothing for a song the queue does not know; a failed read is nothing too.
    pub async fn autofill(&self) -> Vec<Song> {
        let (kind, basis, remote) =
            crate::settings_store::current().map_or((AutoFillKind::Songs, AutoFillBasis::Similar, false), |p| (p.auto_fill_kind, p.auto_fill_basis, p.auto_fill_remote));
        self.autofill_as(kind, basis, remote).await
    }
}

impl Client {
    async fn autofill_as(&self, kind: AutoFillKind, basis: AutoFillBasis, remote: bool) -> Vec<Song> {
        let began = std::time::Instant::now();
        let fresh = self.autofill_from(kind, basis, remote).await;
        // The perf report's word on how long the end of the queue waited (a server asking Last.fm for
        // similar songs takes seconds).
        crate::alog::info(&format!("autofill: {} songs in {} ms (kind {kind:?}, basis {basis:?})", fresh.len(), began.elapsed().as_millis()));
        fresh
    }

    /// `remote`: a provider's songs may be queued too (the `autoFillRemote` setting), and a provider's song
    /// seeds as it is; what it finds nothing for is carried on from the library as when it is off.
    async fn autofill_from(&self, kind: AutoFillKind, basis: AutoFillBasis, remote: bool) -> Vec<Song> {
        let (_, ids) = crate::playlist::snapshot();
        // A shuffle goes on as it began, whatever the setting says: random songs one by one, or random
        // albums whole. The library's only, as the shuffles themselves are.
        match crate::playlist::playlist_origin().map(|o| o.kind) {
            Some(crate::OriginKind::ShuffleSongs) => {
                let queued: HashSet<&str> = ids.iter().map(String::as_str).collect();
                return self.random_library_songs(&queued, false).await;
            }
            Some(crate::OriginKind::ShuffleAlbums) => return self.random_albums(&ids).await,
            _ => {}
        }
        // Carried on from the queue's last song, not the one playing: the fetch starts a song or two
        // ahead of the end, and what comes follows the end.
        let Some(last) = autofill_seed().and_then(queue::queue_song) else { return Vec::new() };
        let queued: HashSet<&str> = ids.iter().map(String::as_str).collect();
        if remote && !in_library(&last) {
            let fresh = self.fresh_from(&last, kind, basis, &ids, &queued, remote).await;
            if !fresh.is_empty() {
                return fresh;
            }
        }
        let Some(seed) = self.library_seed(last, &ids) else { return self.random_library_songs(&queued, remote).await };
        self.fresh_from(&seed, kind, basis, &ids, &queued, remote).await
    }

    /// What follows `seed`: the library's songs, and a provider's too when `remote`.
    async fn fresh_from(&self, seed: &Song, kind: AutoFillKind, basis: AutoFillBasis, ids: &[String], queued: &HashSet<&str>, remote: bool) -> Vec<Song> {
        let fresh = if kind == AutoFillKind::Albums {
            let played: HashSet<String> = queue::queue_albums(ids.to_vec()).into_iter().collect();
            self.next_album(seed, basis, queued, &played, remote).await
        } else {
            // The songs picked or played lately go to the back, as the albums do; the similar and
            // same-artist lists come back in the same order every time.
            let offered: Vec<Song> = self.next_songs(seed, basis).await.unwrap_or_default().into_iter().filter(|s| !queued.contains(s.id.as_str()) && allowed(s, remote)).collect();
            let order = self.turns(Picked::Songs, offered.iter().map(|s| s.id.clone()).collect());
            let mut by_id: HashMap<String, Song> = offered.into_iter().map(|s| (s.id.clone(), s)).collect();
            let fresh: Vec<Song> = order.into_iter().filter_map(|id| by_id.remove(&id)).take(SONGS).collect();
            self.picked(Picked::Songs, &fresh.iter().map(|s| s.id.clone()).collect::<Vec<_>>());
            fresh
        };
        // Kept for the queue they are about to join, so the platform does not hand them straight back.
        queue::queue_register(fresh.clone());
        fresh
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::tests::{block, client};
    use crate::client::NetProfile;

    fn songs_json(ids: &[(&str, &str)]) -> String {
        let s: Vec<String> = ids.iter().map(|(i, a)| format!(r#"{{"id":"{i}","title":"{i}","albumId":"{a}","isDir":false}}"#)).collect();
        format!(r#"{{"subsonic-response":{{"status":"ok","similarSongs2":{{"song":[{}]}}}}}}"#, s.join(","))
    }

    fn song(id: &str, album: &str) -> Song {
        Song { id: id.into(), album_id: Some(album.into()), artist: "A".into(), artist_id: Some("ar".into()), ..Default::default() }
    }

    fn album_json(id: &str, songs: &[&str]) -> String {
        let s: Vec<String> = songs.iter().map(|i| format!(r#"{{"id":"{i}","title":"{i}","albumId":"{id}","isDir":false}}"#)).collect();
        format!(r#"{{"subsonic-response":{{"status":"ok","album":{{"id":"{id}","name":"{id}","song":[{}]}}}}}}"#, s.join(","))
    }

    #[test]
    fn similar_songs_skip_what_is_queued_and_keep_their_order() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queue::queue_register(vec![song("af-seed", "al0"), song("af-q", "al0")]);
        fake.answer(&songs_json(&[("af-q", "x"), ("s2", "x"), ("s1", "y")]));
        let _g = crate::playlist::tests::hold(&["af-seed", "af-q"], 0);
        // s1 was queued by the last refill: it takes its turn after s2.
        note(&c.core.db.lock(), Picked::Songs, &["s1".to_string()], crate::db::now_ms()).unwrap();
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s2", "s1"]);
        assert!(fake.asked.lock()[0].0.contains("getSimilarSongs2"));
        let used = song_use(&c.core.db.lock(), crate::db::now_ms()).unwrap();
        assert!(used.contains_key("s1") && used.contains_key("s2"), "what was queued is remembered");
    }

    fn random_json(ids: &[&str]) -> String {
        let s: Vec<String> = ids.iter().map(|i| format!(r#"{{"id":"{i}","title":"{i}","albumId":"r-{i}","isDir":false}}"#)).collect();
        format!(r#"{{"subsonic-response":{{"status":"ok","randomSongs":{{"song":[{}]}}}}}}"#, s.join(","))
    }

    /// The queue `ids`, started by `kind` (Home's shuffles).
    fn shuffled(ids: &[&str], kind: crate::OriginKind) -> parking_lot::MutexGuard<'static, ()> {
        let g = crate::playlist::tests::hold(ids, 0);
        crate::playlist::playlist_set(ids.iter().map(|s| s.to_string()).collect(), 0, false, Some(crate::PageOrigin::new(kind, "")));
        g
    }

    #[test]
    fn shuffled_songs_go_on_one_random_song_at_a_time_even_when_albums_are_asked_for() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queue::queue_register(vec![song("sh-1", "al1")]);
        let _g = shuffled(&["sh-1"], crate::OriginKind::ShuffleSongs);
        fake.answer(&random_json(&["sh-1", "r1", "r2"]));
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["r1", "r2"], "loose random songs, the queued one left out");
        assert!(fake.asked.lock()[0].0.contains("getRandomSongs"), "{:?}", fake.asked.lock());
    }

    #[test]
    fn shuffled_albums_go_on_with_whole_albums_even_when_songs_are_asked_for() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queue::queue_register(vec![song("sa-1", "al-playing")]);
        let _g = shuffled(&["sa-1"], crate::OriginKind::ShuffleAlbums);
        fake.answer(r#"{"subsonic-response":{"status":"ok","albumList2":{"album":[{"id":"al-playing","name":"P"},{"id":"al-next","name":"N"}]}}}"#);
        fake.answer(&album_json("al-next", &["n1", "n2", "n3"]));
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["n1", "n2", "n3"], "a whole album, not the one already queued");
        let asked = fake.asked.lock();
        assert!(asked[0].0.contains("getAlbumList2") && asked[0].0.contains("type=random"), "{asked:?}");
    }

    #[test]
    fn an_album_picked_lately_waits_its_turn() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queue::queue_register(vec![song("af3-seed", "mine")]);
        note(&c.core.db.lock(), Picked::Album, &["first".to_string()], crate::db::now_ms()).unwrap();
        fake.answer(&songs_json(&[("x1", "first"), ("x2", "second")]));
        fake.answer(&album_json("second", &["b1", "b2", "b3"]));
        let _g = crate::playlist::tests::hold(&["af3-seed"], 0);
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b1", "b2", "b3"]);
        let used = album_use(&c.core.db.lock(), crate::db::now_ms()).unwrap();
        assert!(used["second"] >= used["first"], "the album queued is the one picked last");
    }

    #[test]
    fn an_album_prefers_one_with_a_side_to_it_and_skips_the_seeds() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queue::queue_register(vec![song("af2-seed", "mine")]);
        // The full record was picked a while ago, so the single is tried first and passed over.
        note(&c.core.db.lock(), Picked::Album, &["record".to_string()], crate::db::now_ms() - 1).unwrap();
        // Similar songs from the seed's own album, a single and a full record.
        fake.answer(&songs_json(&[("x1", "mine"), ("x2", "single"), ("x3", "record")]));
        fake.answer(&album_json("single", &["t1"]));
        fake.answer(&album_json("record", &["r1", "r2", "r3"]));
        let _g = crate::playlist::tests::hold(&["af2-seed"], 0);
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["r1", "r2", "r3"]);
        assert_eq!(fake.asked.lock().len(), 3, "the seed's own album is never fetched");
    }

    fn provider(id: &str, artist: &str) -> Song {
        Song { id: id.into(), artist: artist.into(), album_id: Some("ext-deezer-album-1".into()), is_external: true, ..Default::default() }
    }

    #[test]
    fn after_a_providers_song_the_library_carries_on_and_no_provider_song_is_queued() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let mine = Song { id: "afp-lib".into(), title: "Dogs".into(), artist: "The Band".into(), album_id: Some("afp-al".into()), ..Default::default() };
        crate::db::index(&mut c.core.db.lock(), &[], &[], &[mine]).unwrap();
        queue::queue_register(vec![provider("ext-deezer-afp-1", "The Band")]);
        fake.answer(
            r#"{"subsonic-response":{"status":"ok","similarSongs2":{"song":[
                {"id":"ext-deezer-afp-2","title":"x","isExternal":true},{"id":"afp-s1","title":"s1"},{"id":"afp-s2","title":"s2","isExternal":true}]}}}"#,
        );
        let _g = crate::playlist::tests::hold(&["ext-deezer-afp-1"], 0);
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["afp-s1"]);
        let asked = fake.asked.lock();
        assert!(asked[0].0.contains("getSimilarSongs2") && asked[0].0.contains("id=afp-lib"), "seeded by the artist's song in the library: {}", asked[0].0);
    }

    #[test]
    fn after_a_providers_song_of_an_artist_not_in_the_library_the_queues_own_song_seeds() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queue::queue_register(vec![song("afq-lib", "al0"), provider("ext-deezer-afq-1", "Nobody Here")]);
        fake.answer(&songs_json(&[("afq-s1", "x")]));
        let _g = crate::playlist::tests::hold(&["afq-lib", "ext-deezer-afq-1"], 1);
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["afq-s1"]);
        assert!(fake.asked.lock()[0].0.contains("id=afq-lib"));
    }

    #[test]
    fn a_queue_of_only_providers_songs_goes_on_with_the_librarys_at_random() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queue::queue_register(vec![provider("ext-deezer-afr-1", "Nobody Here")]);
        fake.answer(r#"{"subsonic-response":{"status":"ok","randomSongs":{"song":[{"id":"afr-s1","title":"a"},{"id":"ext-deezer-afr-2","title":"b"}]}}}"#);
        let _g = crate::playlist::tests::hold(&["ext-deezer-afr-1"], 0);
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["afr-s1"]);
        assert!(fake.asked.lock()[0].0.contains("getRandomSongs"));
    }

    #[test]
    fn with_remote_songs_let_in_a_providers_song_seeds_and_theirs_are_queued() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queue::queue_register(vec![provider("ext-deezer-afs-1", "The Band")]);
        fake.answer(r#"{"subsonic-response":{"status":"ok","similarSongs2":{"song":[{"id":"ext-deezer-afs-2","title":"x","isExternal":true},{"id":"afs-s1","title":"s1"}]}}}"#);
        let _g = crate::playlist::tests::hold(&["ext-deezer-afs-1"], 0);
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, true));
        let mut ids: Vec<&str> = got.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["afs-s1", "ext-deezer-afs-2"]);
        assert!(fake.asked.lock()[0].0.contains("id=ext-deezer-afs-1"), "the provider's song is the seed");
    }

    #[test]
    fn nothing_for_a_song_the_queue_does_not_know() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let _g = crate::playlist::tests::hold(&["af-unknown"], 0);
        assert!(block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false)).is_empty());
        assert!(fake.asked.lock().is_empty());
    }
}
