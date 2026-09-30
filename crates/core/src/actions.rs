//! Play-action calls: the songs a radio, instant mix, artist, shuffle or M3U import plays. Taps and
//! shuffle order are nori-queue's.

use crate::autofill::seed_now;
use crate::cache_policy::{Page, Read};
use crate::client::{Client, NetResult};
use crate::{m3u, Album, Core, Result, Song};

pub use nori_queue::actions::*;

impl Client {
    /// Song `id` from the queue store, else the downloads, else the cache or server.
    pub(crate) async fn song_of(&self, id: String) -> NetResult<Song> {
        if let Some(s) = crate::queue::queue_song(id.clone()) {
            return Ok(s);
        }
        if let Some(s) = self.core.download_song(&id) {
            return Ok(s);
        }
        Ok(match self.first(Read::SongById { id: id.clone() }).await? {
            Page::OneSong { v: Some(v) } => v,
            _ => Song::only_id(id),
        })
    }

    /// The library songs of the first `wanted` albums that have any; provider albums and songs are skipped
    /// (playing one makes the server download it).
    pub(crate) async fn library_albums(&self, albums: Vec<Album>, wanted: usize) -> Vec<Song> {
        let mut out = Vec::new();
        let mut found = 0;
        for a in albums.into_iter().filter(|a| !a.is_provider()) {
            let mut songs: Vec<Song> = self.songs(Read::AlbumSongs { id: a.id }).await.unwrap_or_default();
            songs.retain(|s| !s.is_provider());
            if songs.is_empty() {
                continue;
            }
            out.extend(songs);
            found += 1;
            if found == wanted {
                break;
            }
        }
        out
    }
}

impl Core {
    /// The finished download `id`, as stored.
    pub fn download_song(&self, id: &str) -> Option<Song> {
        use rusqlite::OptionalExtension;
        let c = self.db.lock();
        let json: String = c.query_row("SELECT json FROM downloads WHERE server=sid() AND id=?1 AND done=1", [id], |r| r.get(0)).optional().ok()??;
        serde_json::from_str(&json).ok()
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// A radio from song `id`: similar songs, else random songs of its genre ([radio_queue]).
    pub async fn radio(&self, id: String) -> NetResult<Vec<Song>> {
        let similar = self.songs(Read::SimilarSongs { id: id.clone(), count: RADIO }).await?;
        let seed = self.song_of(id).await?;
        if let Some(queue) = radio_queue(seed.clone(), similar) {
            return Ok(queue);
        }
        let random = self.songs(Read::RandomSongs { size: RADIO, genre: seed.genre.clone() }).await?;
        Ok(radio_queue(seed.clone(), random).unwrap_or_else(|| vec![seed]))
    }

    /// A mix around song `id` from the index and history; the radio when that yields only the song.
    pub async fn instant_mix(&self, id: String) -> NetResult<Vec<Song>> {
        let mix = self.core.mix_instant(id.clone(), INSTANT_MIX, seed_now())?;
        if mix.len() > 1 {
            return Ok(mix);
        }
        self.radio(id).await
    }

    /// The library songs of `albums` in order, skipping provider and unreadable albums.
    pub async fn artist_songs(&self, albums: Vec<Album>) -> Vec<Song> {
        self.library_albums(albums, usize::MAX).await
    }

    /// Random library songs.
    pub async fn shuffle_all(&self) -> NetResult<Vec<Song>> {
        let mut songs = self.songs(Read::RandomSongs { size: SHUFFLE_ALL, genre: None }).await?;
        songs.retain(|s| !s.is_provider());
        Ok(songs)
    }

    /// One random library album, whole; refills add more ([`crate::OriginKind::ShuffleAlbums`]).
    pub async fn shuffle_albums(&self) -> NetResult<Vec<Song>> {
        let albums = self.read_now(Read::AlbumList { kind: "random".into(), size: SHUFFLE_ALBUMS, offset: 0, genre: None }).await?.albums();
        Ok(self.library_albums(albums, 1).await)
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Matches an M3U against the index; unmatched entries are only counted.
    pub fn m3u_import(&self, text: String) -> Result<M3uImport> {
        let matched = self.m3u_match(m3u::m3u_parse(text))?;
        let song_ids: Vec<String> = matched.iter().flatten().map(|s| s.id.clone()).collect();
        Ok(M3uImport { entries: matched.len() as u32, song_ids })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::tests::{block, client};
    use crate::client::NetProfile;
    use crate::{db, history::tests::song};

    fn songs_json(key: &str, ids: &[&str]) -> String {
        let s: Vec<String> = ids.iter().map(|i| format!(r#"{{"id":"{i}","title":"{i}","isDir":false}}"#)).collect();
        format!(r#"{{"subsonic-response":{{"status":"ok","{key}":{{"song":[{}]}}}}}}"#, s.join(","))
    }

    #[test]
    fn radio_falls_back_to_random_genre_songs() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let seed = Song { id: "r".into(), genre: Some("Jazz".into()), ..Default::default() };
        crate::queue::queue_register(vec![seed.clone()]);
        fake.answer(&songs_json("similarSongs2", &["r"]));
        fake.answer(&songs_json("randomSongs", &["x", "r", "ext-deezer-song-1", "y"]));
        let got = block(c.radio(seed.id.clone())).unwrap();
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["r", "x", "y"]);
        let asked = fake.asked();
        assert!(asked[1].contains("getRandomSongs") && asked[1].contains("genre=Jazz"), "{asked:?}");
        // Nothing indexed near the song: the instant mix is the radio.
        fake.answer(&songs_json("similarSongs2", &["s"]));
        assert_eq!(block(c.instant_mix(seed.id)).unwrap().len(), 2);
    }

    #[test]
    fn song_of_unknown_id_reads_server() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","song":{"id":"not-queued","title":"Q","genre":"Rock","isDir":false}}}"#);
        let s = block(c.song_of("not-queued".into())).unwrap();
        assert_eq!((s.title.as_str(), s.genre.as_deref()), ("Q", Some("Rock")));
        assert!(fake.asked()[0].contains("getSong"), "{:?}", fake.asked());
    }

    #[test]
    fn artist_and_shuffle_skip_providers() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let album = |id: &str, is_external| Album { id: id.into(), is_external, ..Default::default() };
        fake.answer(r#"{"subsonic-response":{"status":"ok","album":{"id":"a1","name":"a1","song":[{"id":"1","title":"1","isDir":false},{"id":"ext-deezer-song-2","title":"2","isDir":false}]}}}"#);
        let got = block(c.artist_songs(vec![album("a1", false), album("ext-2", true), album("pl-deezer-3", false)]));
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["1"]);
        assert_eq!(fake.asked().len(), 1);
        fake.answer(&songs_json("randomSongs", &["x", "ext-deezer-song-1"]));
        assert_eq!(block(c.shuffle_all()).unwrap().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["x"]);
    }

    #[test]
    fn shuffle_albums_plays_one_library_album() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","albumList2":{"album":[{"id":"b","name":"B"},{"id":"ext-applemusic-album-1","name":"X","isExternal":true},{"id":"a","name":"A"}]}}}"#);
        let album = |id: &str, songs: &[&str]| {
            let s: Vec<String> = songs.iter().map(|i| format!(r#"{{"id":"{i}","title":"{i}","isDir":false}}"#)).collect();
            format!(r#"{{"subsonic-response":{{"status":"ok","album":{{"id":"{id}","name":"{id}","song":[{}]}}}}}}"#, s.join(","))
        };
        fake.answer(&album("b", &["b1", "ext-deezer-song-9", "b2", "b3"]));
        fake.answer(&album("a", &["a1", "a2"]));
        let got = block(c.shuffle_albums()).unwrap();
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b1", "b2", "b3"]);
        let asked = fake.asked();
        assert!(asked[0].contains("getAlbumList2") && asked[0].contains("type=random"), "{asked:?}");
        assert_eq!(asked.len(), 2, "{asked:?}");
    }

    #[test]
    fn m3u_import_counts_entries_and_matches() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        db::index(&mut core.db.lock(), &[], &[], &[song("1", "Dogs", "Pink Floyd", "Animals", "", 1977)]).unwrap();
        let text = "#EXTM3U\n#EXTINF:200,Pink Floyd - Dogs\na.flac\n#EXTINF:100,Nobody - Nothing\nb.flac\n";
        let r = core.m3u_import(text.into()).unwrap();
        assert_eq!((r.song_ids.as_slice(), r.entries), (["1".to_string()].as_slice(), 2));
        let none = core.m3u_import("#EXTM3U\n#EXTINF:1,X - Y\nc.mp3\n".into()).unwrap();
        assert!(none.song_ids.is_empty());
        assert_eq!(none.entries, 1);
    }
}
