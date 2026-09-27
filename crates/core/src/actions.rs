//! What playing something means, as the core's and the client's calls: the songs a radio, an instant mix,
//! a whole artist or an M3U playlist play, read from the index or the server. What a tap does and how a
//! shuffle is drawn are nori-queue's.

use crate::autofill::seed_now;
use crate::cache_policy::{Page, Read};
use crate::client::{Client, NetResult};
use crate::{m3u, Album, Core, Result, Song};

pub use nori_queue::actions::*;

impl Client {
    /// Song `id` where it is already to hand: the queue's copy (the song playing, a song in the player's
    /// menu), else the answer read before, else the server's. The platform names a song by its id and
    /// never sends the record back.
    pub(crate) async fn song_of(&self, id: String) -> NetResult<Song> {
        if let Some(s) = crate::queue::queue_song(id.clone()) {
            return Ok(s);
        }
        // A downloaded song is known without the network.
        if let Some(s) = self.core.download_song(&id) {
            return Ok(s);
        }
        Ok(match self.first(Read::SongById { id: id.clone() }).await? {
            Page::OneSong { v: Some(v) } => v,
            _ => Song::only_id(id),
        })
    }
}

/// Asked only in Rust, so not exported to Kotlin.
impl Core {
    /// The song downloaded under `id`, as it was kept with its download; None when it is not downloaded
    /// (or not finished).
    pub fn download_song(&self, id: &str) -> Option<Song> {
        use rusqlite::OptionalExtension;
        let c = self.db.lock();
        let json: String = c.query_row("SELECT json FROM downloads WHERE server=sid() AND id=?1 AND done=1", [id], |r| r.get(0)).optional().ok()??;
        serde_json::from_str(&json).ok()
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// An endless-ish mix seeded from song `id` ([`Self::song_of`]): [radio_queue], else [radio_fallback].
    pub async fn radio(&self, id: String) -> NetResult<Vec<Song>> {
        let similar = self.songs(Read::SimilarSongs { id: id.clone(), count: RADIO }).await?;
        let seed = self.song_of(id).await?;
        if let Some(queue) = radio_queue(seed.clone(), similar) {
            return Ok(queue);
        }
        let random = self.songs(Read::RandomSongs { size: RADIO, genre: seed.genre.clone() }).await?;
        Ok(radio_fallback(seed, random))
    }

    /// A mix around song `id` from the index and the listening history, or the radio when the index has
    /// nothing to go with it: the song is not indexed, or nothing indexed is near it and the mix would be
    /// the song alone.
    pub async fn instant_mix(&self, id: String) -> NetResult<Vec<Song>> {
        let mix = self.core.mix_instant(id.clone(), INSTANT_MIX, seed_now())?;
        if mix.len() > 1 {
            return Ok(mix);
        }
        self.radio(id).await
    }

    /// Every album of an artist, in order, as one list of songs. A provider's albums are left out:
    /// asking for them would make octo-fiesta download them. An album that cannot be read is skipped.
    pub async fn artist_songs(&self, albums: Vec<Album>) -> Vec<Song> {
        let mut out = Vec::new();
        for a in albums.into_iter().filter(|a| !a.is_external) {
            out.extend(self.songs(Read::AlbumSongs { id: a.id }).await.unwrap_or_default());
        }
        out
    }

    /// "Shuffle all": the server's random songs.
    pub async fn shuffle_all(&self) -> NetResult<Vec<Song>> {
        self.songs(Read::RandomSongs { size: SHUFFLE_ALL, genre: None }).await
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Tracks that are not in the index are reported, not guessed.
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
    fn a_radio_with_nothing_similar_goes_on_with_random_songs() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let seed = Song { id: "r".into(), genre: Some("Jazz".into()), ..Default::default() };
        // Queued: the song is the queue's copy, named by its id.
        crate::queue::queue_register(vec![seed.clone()]);
        fake.answer(&songs_json("similarSongs2", &["r"]));
        fake.answer(&songs_json("randomSongs", &["x", "y"]));
        let got = block(c.radio(seed.id.clone())).unwrap();
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["r", "x", "y"]);
        let asked = fake.asked();
        assert!(asked[1].contains("getRandomSongs") && asked[1].contains("genre=Jazz"), "{asked:?}");
        // An instant mix the index cannot draw is the radio: the song is indexed now (what the server
        // answers is), but nothing near it is.
        fake.answer(&songs_json("similarSongs2", &["s"]));
        assert_eq!(block(c.instant_mix(seed.id)).unwrap().len(), 2);
    }

    #[test]
    fn a_song_named_by_id_is_read_where_the_core_does_not_hold_it() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","song":{"id":"not-queued","title":"Q","genre":"Rock","isDir":false}}}"#);
        let s = block(c.song_of("not-queued".into())).unwrap();
        assert_eq!((s.title.as_str(), s.genre.as_deref()), ("Q", Some("Rock")));
        assert!(fake.asked()[0].contains("getSong"), "{:?}", fake.asked());
    }

    #[test]
    fn an_artist_plays_its_own_albums_only() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let album = |id: &str, is_external| Album { id: id.into(), is_external, ..Default::default() };
        fake.answer(r#"{"subsonic-response":{"status":"ok","album":{"id":"a1","name":"a1","song":[{"id":"1","title":"1","isDir":false}]}}}"#);
        let got = block(c.artist_songs(vec![album("a1", false), album("ext-2", true)]));
        assert_eq!(got.len(), 1);
        assert_eq!(fake.asked().len(), 1, "a provider's album is never asked for");
    }

    #[test]
    fn m3u_import_counts_what_it_found() {
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
