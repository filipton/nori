//! Autofill fetches (songs or albums from the server). When to fetch is nori-queue's.

use std::collections::{HashMap, HashSet};

use futures_util::future::join_all;

use crate::cache_policy::{Page, Read};
use crate::client::Client;
use crate::settings::{AutoFillBasis, AutoFillKind};
use crate::Song;

pub use nori_queue::autofill::*;

/// Songs autofill appends, and the `from` a client passes to `playlist_take`.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Refill {
    pub songs: Vec<Song>,
    /// The album (one album run) or the "shuffle albums" origin (a run per album); None for loose songs.
    pub from: Option<crate::PageOrigin>,
}

impl Refill {
    fn songs(songs: Vec<Song>) -> Refill {
        Refill { songs, from: None }
    }
}

impl Client {
    /// The cached page if any, else the server's.
    pub(crate) async fn first(&self, read: Read) -> Got<Page> {
        if let Ok(stored) = self.read_stored(read.clone()) {
            if let Some(p) = stored.page {
                return Ok(p);
            }
        }
        self.read_now(read).await
    }

    pub(crate) async fn songs(&self, read: Read) -> Got<Vec<Song>> {
        Ok(self.read_now(read).await?.songs())
    }

    async fn artist_albums(&self, seed: &Song) -> Got<Vec<crate::Album>> {
        let Some(id) = seed.artist_id.clone() else { return Ok(Vec::new()) };
        Ok(self.first(Read::ArtistById { id }).await?.albums())
    }

    /// Loose songs following `seed` by `basis`, in the source's order.
    async fn next_songs(&self, seed: &Song, basis: AutoFillBasis) -> Got<Vec<Song>> {
        Ok(match basis {
            // Top songs, else songs of the artist's first three albums.
            AutoFillBasis::Artist => {
                let top = self.first(Read::TopSongs { artist: seed.artist.clone() }).await?.songs();
                if !top.is_empty() {
                    return Ok(top);
                }
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
            // From the local index: Subsonic has no songs-by-decade call.
            AutoFillBasis::Era => match era(seed) {
                Some((from, to)) => shuffled(self.core.browse_songs("playCount".into(), true, false, from, to, 0, 100).unwrap_or_default()),
                None => Vec::new(),
            },
            _ => self.songs(Read::SimilarSongs { id: seed.id.clone(), count: 25 }).await?,
        })
    }

    /// Candidate album ids for `basis`.
    async fn album_candidates(&self, seed: &Song, basis: AutoFillBasis, remote: bool) -> Got<Vec<String>> {
        let ids = |v: Vec<crate::Album>| v.into_iter().filter(|a| remote || !a.is_provider()).map(|a| a.id).collect::<Vec<_>>();
        Ok(match basis {
            AutoFillBasis::Artist => ids(self.artist_albums(seed).await?),
            AutoFillBasis::Genre => match &seed.genre {
                Some(g) => shuffled(ids(self.first(Read::AlbumList { kind: "byGenre".into(), size: 30, offset: 0, genre: Some(g.clone()) }).await?.albums())),
                None => Vec::new(),
            },
            AutoFillBasis::Era => match era(seed) {
                Some((from, to)) => shuffled(ids(self.first(Read::AlbumsByYear { from: from as i32, to: to as i32, size: 30, offset: 0 }).await?.albums())),
                None => Vec::new(),
            },
            // Albums of the similar songs.
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

    /// One whole album not already queued or played, ranked by [`rank`]. The first with at least
    /// [`ALBUM_MIN`] songs wins; the longest short one is the fallback.
    async fn next_album(&self, seed: &Song, basis: AutoFillBasis, queued: &HashSet<&str>, played: &HashSet<String>, remote: bool) -> Refill {
        let candidates = self.album_candidates(seed, basis, remote).await.unwrap_or_default();
        let pool: Vec<String> = candidates.into_iter().filter(|a| seed.album_id.as_ref() != Some(a) && !played.contains(a)).collect();
        let ranked = self.turns(Picked::Album, pool);
        let mut short = Vec::new();
        let mut short_id = None;
        // Fetched concurrently, chosen in ranked order.
        let picks: Vec<String> = ranked.into_iter().take(ALBUM_TRIES).collect();
        let reads = join_all(picks.iter().map(|pick| self.songs(Read::AlbumSongs { id: pick.clone() }))).await;
        for (pick, read) in picks.into_iter().zip(reads) {
            let songs: Vec<Song> = read.unwrap_or_default().into_iter().filter(|s| !queued.contains(s.id.as_str()) && allowed(s, remote)).collect();
            if songs.len() >= ALBUM_MIN {
                self.picked(Picked::Album, std::slice::from_ref(&pick));
                return Refill { songs, from: Some(crate::PageOrigin::new(crate::OriginKind::Album, pick)) };
            }
            if songs.len() > short.len() {
                short = songs;
                short_id = Some(pick);
            }
        }
        if let Some(id) = &short_id {
            self.picked(Picked::Album, std::slice::from_ref(id));
        }
        let from = short_id.filter(|_| !short.is_empty()).map(|id| crate::PageOrigin::new(crate::OriginKind::Album, id));
        Refill { songs: short, from }
    }

    /// `candidates` ranked by recent use ([`rank`]); unchanged if the database read fails.
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

    /// Keeps `ids` as this fetch's picks, recorded by [`Client::autofill_arrived`] once appended.
    fn picked(&self, kind: Picked, ids: &[String]) {
        *self.autofill_picks.lock() = Some((kind, ids.to_vec()));
    }

    /// A library seed for `last`: itself if in the library, else a library song by the same artist, else
    /// the queue's last library song. Provider seeds would yield provider songs, which the server downloads.
    fn library_seed(&self, last: Song, ids: &[String]) -> Option<Song> {
        if in_library(&last) {
            return Some(last);
        }
        let artist = last.artist.trim();
        let same_artist = (!artist.is_empty())
            .then(|| self.core.local_search(artist.to_string(), 50).ok())
            .flatten()
            .and_then(|found| found.songs.into_iter().find(|s| in_library(s) && s.artist.trim().eq_ignore_ascii_case(artist)));
        same_artist.or_else(|| ids.iter().rev().filter_map(|id| self.core.session.song(id)).find(in_library))
    }

    /// Whole random library albums not already queued ("shuffle albums" refill).
    async fn random_albums(&self, ids: &[String]) -> Vec<Song> {
        let queued: HashSet<String> = self.core.session.albums(ids.to_vec()).into_iter().collect();
        let read = Read::AlbumList { kind: "random".into(), size: crate::actions::SHUFFLE_ALBUMS, offset: 0, genre: None };
        let albums = self.read_now(read).await.map(Page::albums).unwrap_or_default().into_iter().filter(|a| !queued.contains(&a.id)).collect();
        let fresh = self.library_albums(albums, RANDOM_ALBUMS as usize).await;
        self.core.session.register(fresh.clone());
        fresh
    }

    /// Random library songs not already queued.
    async fn random_library_songs(&self, queued: &HashSet<&str>, remote: bool) -> Vec<Song> {
        let offered = self.songs(Read::RandomSongs { size: 50, genre: None }).await.unwrap_or_default();
        let fresh: Vec<Song> = offered.into_iter().filter(|s| !queued.contains(s.id.as_str()) && allowed(s, remote)).take(SONGS).collect();
        self.core.session.register(fresh.clone());
        fresh
    }
}

/// Whether `s` is a library song, not a provider's (playing one makes the server download it).
fn in_library(s: &Song) -> bool {
    !s.is_provider()
}

/// Whether autofill may queue `s`.
fn allowed(s: &Song, remote: bool) -> bool {
    remote || in_library(s)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// What to append after the queue's end, per the autofill settings. Empty on failure or an unknown seed.
    pub async fn autofill(&self) -> Refill {
        let (kind, basis, remote) =
            self.settings().prefs(|p| (p.auto_fill_kind, p.auto_fill_basis, p.auto_fill_remote));
        self.autofill_as(kind, basis, remote).await
    }

    /// The fetch returned `count` songs: whether to append them. When so, what it picked counts as used
    /// for the rotation; a fetch whose songs are dropped leaves no trace.
    pub fn autofill_arrived(&self, count: u32) -> bool {
        let take = self.core.session.autofill_arrived(count);
        if let (true, Some((kind, ids))) = (take, self.autofill_picks.lock().take()) {
            let _ = note(&self.core.db.lock(), kind, &ids, crate::db::now_ms());
        }
        take
    }
}

impl Client {
    async fn autofill_as(&self, kind: AutoFillKind, basis: AutoFillBasis, remote: bool) -> Refill {
        let began = std::time::Instant::now();
        *self.autofill_picks.lock() = None;
        let fresh = self.autofill_from(kind, basis, remote).await;
        crate::alog::info(&format!("autofill: {} songs in {} ms (kind {kind:?}, basis {basis:?})", fresh.songs.len(), began.elapsed().as_millis()));
        fresh
    }

    /// `remote`: provider songs may be queued and may seed; falls back to a library seed if that finds nothing.
    async fn autofill_from(&self, kind: AutoFillKind, basis: AutoFillBasis, remote: bool) -> Refill {
        let (_, ids) = self.core.session.snapshot();
        // Shuffle queues continue as they began, from the library only, whatever the setting.
        match self.core.session.origin().map(|o| o.kind) {
            Some(crate::OriginKind::ShuffleSongs) => {
                let queued: HashSet<&str> = ids.iter().map(String::as_str).collect();
                return Refill::songs(self.random_library_songs(&queued, false).await);
            }
            Some(crate::OriginKind::ShuffleAlbums) => {
                return Refill { songs: self.random_albums(&ids).await, from: Some(crate::PageOrigin::new(crate::OriginKind::ShuffleAlbums, "")) };
            }
            _ => {}
        }
        // Seeded by the queue's last song: the fetch starts before the end is reached.
        let Some(last) = self.core.session.autofill_seed().and_then(|id| self.core.session.song(&id)) else { return Refill::default() };
        let queued: HashSet<&str> = ids.iter().map(String::as_str).collect();
        if remote && !in_library(&last) {
            let fresh = self.fresh_from(&last, kind, basis, &ids, &queued, remote).await;
            if !fresh.songs.is_empty() {
                return fresh;
            }
        }
        let Some(seed) = self.library_seed(last, &ids) else { return Refill::songs(self.random_library_songs(&queued, remote).await) };
        self.fresh_from(&seed, kind, basis, &ids, &queued, remote).await
    }

    /// The refill following `seed`.
    async fn fresh_from(&self, seed: &Song, kind: AutoFillKind, basis: AutoFillBasis, ids: &[String], queued: &HashSet<&str>, remote: bool) -> Refill {
        let fresh = if kind == AutoFillKind::Albums {
            let played: HashSet<String> = self.core.session.albums(ids.to_vec()).into_iter().collect();
            self.next_album(seed, basis, queued, &played, remote).await
        } else {
            let offered: Vec<Song> = self.next_songs(seed, basis).await.unwrap_or_default().into_iter().filter(|s| !queued.contains(s.id.as_str()) && allowed(s, remote)).collect();
            let order = self.turns(Picked::Songs, offered.iter().map(|s| s.id.clone()).collect());
            let mut by_id: HashMap<String, Song> = offered.into_iter().map(|s| (s.id.clone(), s)).collect();
            let fresh: Vec<Song> = order.into_iter().filter_map(|id| by_id.remove(&id)).take(SONGS).collect();
            self.picked(Picked::Songs, &fresh.iter().map(|s| s.id.clone()).collect::<Vec<_>>());
            Refill::songs(fresh)
        };
        self.core.session.register(fresh.songs.clone());
        fresh
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::tests::{block, client};
    use crate::client::NetProfile;

    /// Queues `ids` on `c` from `start`.
    fn queued(c: &Client, ids: &[&str], start: u32) {
        c.core.session.set(ids.iter().map(|s| s.to_string()).collect(), Some(start), false, None);
    }

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
    fn similar_songs_skip_and_rotate() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![song("af-seed", "al0"), song("af-q", "al0")]);
        fake.answer(&songs_json(&[("af-q", "x"), ("s2", "x"), ("s1", "y")]));
        queued(&c, &["af-seed", "af-q"], 0);
        // s1 was picked by the last refill: it goes after s2.
        note(&c.core.db.lock(), Picked::Songs, &["s1".to_string()], crate::db::now_ms()).unwrap();
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s2", "s1"]);
        assert!(fake.asked.lock()[0].0.contains("getSimilarSongs2"));
        assert_eq!(c.autofill_picks.lock().as_ref().map(|p| p.1.clone()), Some(vec!["s2".to_string(), "s1".into()]));
    }

    #[test]
    fn dropped_refill_not_picked() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![song("dr-seed", "al0")]);
        fake.answer(&songs_json(&[("dr-1", "x")]));
        queued(&c, &["dr-seed"], 0);
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.len(), 1);
        // No fetch was waited for at this end: the songs are not appended.
        assert!(!c.autofill_arrived(1));
        assert!(!song_use(&c.core.db.lock(), crate::db::now_ms()).unwrap().contains_key("dr-1"));
    }

    fn random_json(ids: &[&str]) -> String {
        let s: Vec<String> = ids.iter().map(|i| format!(r#"{{"id":"{i}","title":"{i}","albumId":"r-{i}","isDir":false}}"#)).collect();
        format!(r#"{{"subsonic-response":{{"status":"ok","randomSongs":{{"song":[{}]}}}}}}"#, s.join(","))
    }

    /// Queues `ids` on `c`, started from `kind`.
    fn shuffled(c: &Client, ids: &[&str], kind: crate::OriginKind) {
        c.core.session.set(ids.iter().map(|s| s.to_string()).collect(), Some(0), false, Some(crate::PageOrigin::new(kind, "")));
    }

    #[test]
    fn shuffle_refills() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![song("sh-1", "al1")]);
        shuffled(&c, &["sh-1"], crate::OriginKind::ShuffleSongs);
        fake.answer(&random_json(&["sh-1", "r1", "r2"]));
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["r1", "r2"]);
        assert!(fake.asked.lock()[0].0.contains("getRandomSongs"), "{:?}", fake.asked.lock());

        // Shuffle albums refills whole albums.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![song("sa-1", "al-playing")]);
        shuffled(&c, &["sa-1"], crate::OriginKind::ShuffleAlbums);
        fake.answer(r#"{"subsonic-response":{"status":"ok","albumList2":{"album":[{"id":"al-playing","name":"P"},{"id":"al-next","name":"N"}]}}}"#);
        fake.answer(&album_json("al-next", &["n1", "n2", "n3"]));
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["n1", "n2", "n3"]);
        let asked = fake.asked.lock();
        assert!(asked[0].0.contains("getAlbumList2") && asked[0].0.contains("type=random"), "{asked:?}");
    }

    #[test]
    fn album_refills() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![song("af3-seed", "mine")]);
        note(&c.core.db.lock(), Picked::Album, &["first".to_string()], crate::db::now_ms()).unwrap();
        fake.answer(&songs_json(&[("x1", "first"), ("x2", "second")]));
        fake.answer(&album_json("second", &["b1", "b2", "b3"]));
        queued(&c, &["af3-seed"], 0);
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b1", "b2", "b3"]);
        assert_eq!(*c.autofill_picks.lock(), Some((Picked::Album, vec!["second".to_string()])));

        // Album prefers full record over single.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![song("af2-seed", "mine")]);
        // "record" was picked before, so the single ranks first and is passed over.
        note(&c.core.db.lock(), Picked::Album, &["record".to_string()], crate::db::now_ms() - 1).unwrap();
        fake.answer(&songs_json(&[("x1", "mine"), ("x2", "single"), ("x3", "record")]));
        fake.answer(&album_json("single", &["t1"]));
        fake.answer(&album_json("record", &["r1", "r2", "r3"]));
        queued(&c, &["af2-seed"], 0);
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["r1", "r2", "r3"]);
        assert_eq!(fake.asked.lock().len(), 3, "the seed's own album is not fetched");

        // Regression: an autofilled album used to be added without an album run.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![song("af3-seed", "mine")]);
        fake.answer(&songs_json(&[("y1", "whole")]));
        fake.answer(&album_json("whole", &["w1", "w2", "w3", "w4"]));
        queued(&c, &["af3-seed"], 0);
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false));
        assert_eq!(got.from, Some(crate::PageOrigin::new(crate::OriginKind::Album, "whole")));
        let add = |r: &Refill| c.core.session.take(9, r.songs.iter().map(|s| s.id.clone()).collect(), vec![nori_player::playlist::Hand::No; r.songs.len()], r.from.clone()).at.unwrap() as usize;
        let at = add(&got);
        let runs = c.core.session.playlist(|p| p.album_runs().to_vec());
        assert!(runs[at] > 0 && runs[at..at + 4] == [runs[at]; 4], "{runs:?}");

        fake.answer(&songs_json(&[("z1", "a"), ("z2", "b")]));
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false));
        assert_eq!((got.songs.len(), &got.from), (2, &None));
        let at = add(&got);
        assert_eq!(c.core.session.playlist(|p| p.album_runs()[at..at + 2].to_vec()), [0, 0]);
    }

    fn provider(id: &str, artist: &str) -> Song {
        Song { id: id.into(), artist: artist.into(), album_id: Some("ext-deezer-album-1".into()), is_external: true, ..Default::default() }
    }

    #[test]
    fn provider_seeds() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let mine = Song { id: "afp-lib".into(), title: "Dogs".into(), artist: "The Band".into(), album_id: Some("afp-al".into()), ..Default::default() };
        crate::db::index(&mut c.core.db.lock(), &[], &[], &[mine]).unwrap();
        c.core.session.register(vec![provider("ext-deezer-afp-1", "The Band")]);
        fake.answer(
            r#"{"subsonic-response":{"status":"ok","similarSongs2":{"song":[
                {"id":"ext-deezer-afp-2","title":"x","isExternal":true},{"id":"afp-s1","title":"s1"},{"id":"afp-s2","title":"s2","isExternal":true}]}}}"#,
        );
        queued(&c, &["ext-deezer-afp-1"], 0);
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["afp-s1"]);
        let asked = fake.asked.lock();
        assert!(asked[0].0.contains("getSimilarSongs2") && asked[0].0.contains("id=afp-lib"), "seeded by the artist's song in the library: {}", asked[0].0);

        // Provider seed falls back to queued library song.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![song("afq-lib", "al0"), provider("ext-deezer-afq-1", "Nobody Here")]);
        fake.answer(&songs_json(&[("afq-s1", "x")]));
        queued(&c, &["afq-lib", "ext-deezer-afq-1"], 1);
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["afq-s1"]);
        assert!(fake.asked.lock()[0].0.contains("id=afq-lib"));

        // Provider only queue refills random library songs.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![provider("ext-deezer-afr-1", "Nobody Here")]);
        fake.answer(r#"{"subsonic-response":{"status":"ok","randomSongs":{"song":[{"id":"afr-s1","title":"a"},{"id":"ext-deezer-afr-2","title":"b"}]}}}"#);
        queued(&c, &["ext-deezer-afr-1"], 0);
        let got = block(c.autofill_as(AutoFillKind::Albums, AutoFillBasis::Similar, false)).songs;
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["afr-s1"]);
        assert!(fake.asked.lock()[0].0.contains("getRandomSongs"));

        // Remote allows provider seed and songs.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.session.register(vec![provider("ext-deezer-afs-1", "The Band")]);
        fake.answer(r#"{"subsonic-response":{"status":"ok","similarSongs2":{"song":[{"id":"ext-deezer-afs-2","title":"x","isExternal":true},{"id":"afs-s1","title":"s1"}]}}}"#);
        queued(&c, &["ext-deezer-afs-1"], 0);
        let got = block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, true)).songs;
        let mut ids: Vec<&str> = got.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["afs-s1", "ext-deezer-afs-2"]);
        assert!(fake.asked.lock()[0].0.contains("id=ext-deezer-afs-1"), "the provider's song is the seed");
    }

    #[test]
    fn unknown_seed_fetches_nothing() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        queued(&c, &["af-unknown"], 0);
        assert!(block(c.autofill_as(AutoFillKind::Songs, AutoFillBasis::Similar, false)).songs.is_empty());
        assert!(fake.asked.lock().is_empty());
    }
}
