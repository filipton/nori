//! The car's browse tree as the client's calls: each folder read as a screen reads it (the stored answer
//! first, so the car works from what the phone has when the server is out of reach), the queue a picked
//! row plays, the car's search and its spoken requests. The tree and how its rows read are nori-library's.

use std::collections::{HashSet, VecDeque};

use crate::cache_policy::{Page, Read};
use crate::client::Client;
use crate::mixes::board::{MixLookup, FAVOURITES_MIX};
use crate::Song;

pub use nori_library::car::*;

/// The songs of the folders last listed, newest last: a picked row plays the very list it was in.
#[derive(Default)]
pub struct Shown(VecDeque<(String, Vec<Song>)>);

impl Shown {
    /// How many folders are remembered: a car lists a few levels deep at a time.
    const KEPT: usize = 8;

    fn keep(&mut self, parent: &str, songs: &[Song]) {
        self.0.retain(|(p, _)| p != parent);
        self.0.push_back((parent.into(), songs.to_vec()));
        while self.0.len() > Self::KEPT {
            self.0.pop_front();
        }
    }

    fn get(&self, parent: &str) -> Option<Vec<Song>> {
        self.0.iter().find(|(p, _)| p == parent).map(|(_, s)| s.clone())
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// The root folder of the tree.
    pub fn browse_root(&self) -> BrowseFolder {
        folder(ROOT, CarFolder::Root)
    }

    /// The root's tabs, `limit` at most, the downloads first while `offline` (see nori-library's `root`).
    pub fn car_root(&self, limit: u32, offline: bool) -> BrowsePage {
        BrowsePage { folders: root(limit, offline), ..Default::default() }
    }

    /// What the folder `parent` holds; nothing for one that is not known, and `failed` for one that
    /// cannot be read now.
    pub async fn browse_children(&self, parent: String) -> BrowsePage {
        let (kind, arg) = parent.split_once(':').unwrap_or((parent.as_str(), ""));
        let page = match kind {
            ROOT => BrowsePage { folders: root(4, false), ..Default::default() },
            "home" => self.car_home().await,
            "library" => BrowsePage { folders: library(), ..Default::default() },
            "albums" => match self.first(Read::AlbumList { kind: arg.into(), size: 100, offset: 0, genre: None }).await {
                Ok(Page::Albums { v }) => BrowsePage { folders: album_folders(&v, None, 100), ..Default::default() },
                _ => failed(),
            },
            "artists" => match self.first(Read::ArtistIndex).await {
                Ok(Page::Artists { v }) => BrowsePage { folders: v.iter().filter(|a| !a.is_external).map(|a| artist_folder(a, None)).collect(), ..Default::default() },
                _ => failed(),
            },
            "artist" => match self.first(Read::ArtistById { id: arg.into() }).await {
                Ok(Page::ArtistPage { v }) => BrowsePage { folders: album_folders(&v.albums, None, usize::MAX), actions: whole(), ..Default::default() },
                _ => failed(),
            },
            "playlists" => match self.first(Read::PlaylistList).await {
                Ok(Page::Playlists { v }) => BrowsePage { folders: v.iter().map(playlist_folder).collect(), ..Default::default() },
                _ => failed(),
            },
            "genres" => match self.first(Read::GenreList).await {
                Ok(Page::Genres { v }) => BrowsePage { folders: v.iter().filter(|g| g.song_count > 0).map(genre_folder).collect(), ..Default::default() },
                _ => failed(),
            },
            "genre" => match self.first(Read::AlbumList { kind: "byGenre".into(), size: 100, offset: 0, genre: Some(arg.into()) }).await {
                Ok(Page::Albums { v }) => BrowsePage { folders: album_folders(&v, None, 100), actions: whole(), ..Default::default() },
                _ => failed(),
            },
            "starred" => match self.first(Read::StarredItems).await {
                Ok(Page::StarredPage { v }) => {
                    let mut p = self.songs_page(&parent, library_songs(v.songs), true);
                    p.folders = album_folders(&v.albums, Some(CarGroup::Albums), usize::MAX);
                    p.folders.extend(v.artists.iter().filter(|a| !a.is_external).map(|a| artist_folder(a, Some(CarGroup::Artists))));
                    if !p.folders.is_empty() {
                        p.songs_group = Some(CarGroup::Songs);
                    }
                    p
                }
                _ => failed(),
            },
            "search" => self.car_search(arg.into()).await,
            _ => match self.folder_songs(&parent).await {
                Some(songs) => self.songs_page(&parent, songs, kind != "random"),
                None if matches!(kind, "album" | "playlist" | "mix" | "random" | "downloads") => failed(),
                None => BrowsePage::default(),
            },
        };
        page
    }

    /// What a picked row plays: a song of a folder plays the folder from it, Play and Shuffle the whole
    /// of it. None for an id that is not one of the tree's rows.
    pub async fn car_queue(&self, row: String) -> Option<CarQueue> {
        let row = car_row(row)?;
        let (kind, arg) = row.parent.split_once(':').unwrap_or((row.parent.as_str(), ""));
        // Played whole, an artist is all their songs and a genre its songs, not the albums listed.
        let songs = match kind {
            "artist" if row.song.is_none() => self.artist_songs_of(arg.into()).await.unwrap_or_default(),
            "genre" if row.song.is_none() => self.songs(Read::SongsByGenre { genre: arg.into(), count: 100 }).await.unwrap_or_default(),
            _ => {
                // Out of the lock before any wait: it is not held across one.
                let shown = self.car.lock().get(&row.parent);
                match shown {
                    Some(s) => s,
                    None => self.folder_songs(&row.parent).await.unwrap_or_default(),
                }
            }
        };
        Some(queue_for(&row, library_songs(songs)))
    }

    /// The car's search: the artists, albums and songs the server finds for `query`, under their headings.
    pub async fn car_search(&self, query: String) -> BrowsePage {
        let sizes = crate::browse::library_sizes();
        let read = Read::Search { query: query.clone(), songs: sizes.search_songs, albums: sizes.search_albums, artists: sizes.search_artists };
        let Ok(Page::Found { v }) = self.read_now(read).await else { return failed() };
        let mut p = self.songs_page(&format!("search:{query}"), library_songs(v.songs), false);
        p.folders = v.artists.iter().filter(|a| !a.is_external).map(|a| artist_folder(a, Some(CarGroup::Artists))).collect();
        p.folders.extend(album_folders(&v.albums, Some(CarGroup::Albums), usize::MAX));
        p.songs_group = Some(CarGroup::Songs);
        p
    }

    /// What a spoken request plays (nori-library's `voice_pick`): an artist's songs and a genre's shuffled,
    /// an album or a playlist from its start as its page's queue, the songs found from the first. Empty
    /// when nothing matches or the server cannot be asked.
    pub async fn car_voice(&self, ask: VoiceAsk) -> CarQueue {
        let term = voice_term(&ask);
        let queue = |songs: Vec<Song>, kind: Option<nori_model::OriginKind>, id: String, shuffle: bool| CarQueue {
            songs: library_songs(songs),
            index: 0,
            origin: kind.map(|kind| nori_model::PageOrigin { kind, id }),
            shuffle,
        };
        if ask.focus == VoiceFocus::Genre {
            let songs = self.songs(Read::SongsByGenre { genre: term.clone(), count: 100 }).await.unwrap_or_default();
            return queue(songs, None, term, true);
        }
        let sizes = crate::browse::library_sizes();
        let found = match self.read_now(Read::Search { query: term, songs: sizes.search_songs, albums: sizes.search_albums, artists: sizes.search_artists }).await {
            Ok(Page::Found { v }) => v,
            _ => Default::default(),
        };
        let playlists = match ask.focus {
            VoiceFocus::Any | VoiceFocus::Playlist => match self.first(Read::PlaylistList).await {
                Ok(Page::Playlists { v }) => v,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        use nori_model::OriginKind as K;
        match voice_pick(&ask, &found, &playlists) {
            VoicePick::Artist(id) => queue(self.artist_songs_of(id.clone()).await.unwrap_or_default(), Some(K::Artist), id, true),
            VoicePick::Album(id) => queue(self.folder_songs(&format!("album:{id}")).await.unwrap_or_default(), Some(K::Album), id, false),
            VoicePick::Playlist(id) => queue(self.folder_songs(&format!("playlist:{id}")).await.unwrap_or_default(), Some(K::Playlist), id, false),
            VoicePick::Genre(g) => queue(self.songs(Read::SongsByGenre { genre: g.clone(), count: 100 }).await.unwrap_or_default(), None, g, true),
            VoicePick::Songs(songs) => queue(songs, None, String::new(), false),
            VoicePick::Nothing => CarQueue::default(),
        }
    }
}

/// A folder that could not be read.
fn failed() -> BrowsePage {
    BrowsePage { failed: true, ..Default::default() }
}

/// Play and Shuffle, for a folder that plays whole.
fn whole() -> Vec<CarAction> {
    vec![CarAction::Play, CarAction::Shuffle]
}

impl Client {
    /// Home: favourites and today's mixes as on the phone's Home, then what was played and added lately.
    async fn car_home(&self) -> BrowsePage {
        let taste = crate::settings_store::current().is_none_or(|p| p.taste_model);
        // The starred songs handed to the mixes, as the Home row hands them before it is drawn.
        if let Ok(h) = self.mix_favourites_stored() {
            if !h.fresh {
                let _ = self.mix_favourites_refresh(h.digest).await;
            }
        }
        if taste {
            self.mix_warm_all().await;
        }
        let mut folders: Vec<BrowseFolder> = self.core.mix_cards(taste).iter().filter(|t| !t.covers.is_empty()).map(mix_folder).collect();
        let mut read = false;
        for (kind, group) in [("recent", CarGroup::RecentlyPlayed), ("newest", CarGroup::RecentlyAdded)] {
            if let Ok(Page::Albums { v }) = self.first(Read::AlbumList { kind: kind.into(), size: SHELF as i32, offset: 0, genre: None }).await {
                read = true;
                folders.extend(album_folders(&v, Some(group), SHELF));
            }
        }
        BrowsePage { failed: folders.is_empty() && !read, folders, ..Default::default() }
    }

    /// The songs folder `parent` lists and plays, as its page has them; none when it cannot be read or
    /// is not a folder of songs.
    async fn folder_songs(&self, parent: &str) -> Option<Vec<Song>> {
        let (kind, arg) = parent.split_once(':').unwrap_or((parent, ""));
        let songs = match kind {
            "album" => match self.first(Read::AlbumById { id: arg.into() }).await.ok()? {
                Page::AlbumPage { v } => v.songs,
                _ => return None,
            },
            "playlist" => match self.first(Read::PlaylistById { id: arg.into() }).await.ok()? {
                Page::PlaylistPage { v } => v.songs,
                _ => return None,
            },
            "mix" => {
                if arg != FAVOURITES_MIX {
                    self.mix_ensure(arg.into(), false).await;
                }
                match self.core.mix_page(arg.into()) {
                    MixLookup::Ready { sheet } => sheet.songs,
                    _ => return None,
                }
            }
            "starred" => match self.first(Read::StarredItems).await.ok()? {
                Page::StarredPage { v } => v.songs,
                _ => return None,
            },
            "random" => self.songs(Read::RandomSongs { size: 50, genre: None }).await.ok()?,
            "downloads" => self.core.downloads(true).ok()?,
            "search" => {
                let sizes = crate::browse::library_sizes();
                match self.read_now(Read::Search { query: arg.into(), songs: sizes.search_songs, albums: 0, artists: 0 }).await.ok()? {
                    Page::Found { v } => v.songs,
                    _ => return None,
                }
            }
            _ => return None,
        };
        Some(library_songs(songs))
    }

    /// `songs` as folder `parent`'s rows, each marked downloaded or not, with Play and Shuffle above them
    /// when `actions`; remembered, so a row picked plays this very list.
    fn songs_page(&self, parent: &str, songs: Vec<Song>, actions: bool) -> BrowsePage {
        self.car.lock().keep(parent, &songs);
        let kept: HashSet<String> = self.core.downloads(true).unwrap_or_default().into_iter().map(|s| s.id).collect();
        BrowsePage {
            downloaded: songs.iter().map(|s| kept.contains(&s.id)).collect(),
            actions: if actions && !songs.is_empty() { whole() } else { Vec::new() },
            songs,
            ..Default::default()
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::tests::{block, client};
    use crate::client::NetProfile;

    #[test]
    fn the_root_lists_the_cars_tabs_and_asks_nothing() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let p = block(c.browse_children(ROOT.into()));
        assert_eq!(p.folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["home", "library", "starred", "downloads"]);
        assert_eq!(c.car_root(3, true).folders[0].id, "downloads");
        assert!(block(c.browse_children("library".into())).folders.iter().any(|f| f.id == "artists"));
        assert!(fake.asked.lock().is_empty(), "the root and the library's lists ask nothing of the server");
    }

    #[test]
    fn a_playlist_folder_has_its_count_of_songs() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","playlists":{"playlist":[{"id":"p1","name":"Evening","songCount":12}]}}}"#);
        let p = block(c.browse_children("playlists".into()));
        assert_eq!((p.folders[0].id.as_str(), p.folders[0].title.as_str(), p.folders[0].songs, p.folders[0].playable), ("playlist:p1", "Evening", Some(12), true));
        assert!(block(c.browse_children("nonsense".into())).folders.is_empty());
    }

    const ALBUM: &str = r#"{"subsonic-response":{"status":"ok","album":{"id":"a1","name":"Monster","artist":"Future","song":[
        {"id":"s1","title":"One","album":"Monster"},{"id":"s2","title":"Two","album":"Monster"},{"id":"x","title":"Theirs","isExternal":true}]}}}"#;

    #[test]
    fn an_album_lists_play_shuffle_and_its_songs_and_a_song_picked_plays_the_album_from_it() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(ALBUM);
        let p = block(c.browse_children("album:a1".into()));
        assert_eq!(p.actions, [CarAction::Play, CarAction::Shuffle]);
        assert_eq!(p.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s1", "s2"], "a provider's song is left out");
        assert_eq!(p.downloaded, [false, false]);
        let asked = fake.asked().len();
        let q = block(c.car_queue(car_song_row("album:a1".into(), "s2".into()))).unwrap();
        assert_eq!((q.songs.len(), q.index, q.shuffle), (2, 1, false));
        assert_eq!(q.origin.unwrap().id, "a1");
        assert_eq!(fake.asked().len(), asked, "the list shown is played, not asked for again");
        assert!(block(c.car_queue("s1".into())).is_none(), "a bare song id is not a row");
    }

    #[test]
    fn a_folder_the_server_cannot_answer_says_so() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.fail(crate::transport::FailureKind::Connect);
        assert!(block(c.browse_children("album:a1".into())).failed);
        fake.fail(crate::transport::FailureKind::Connect);
        assert!(block(c.browse_children("albums:newest".into())).failed);
    }

    #[test]
    fn a_spoken_album_plays_as_the_albums_queue() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","searchResult3":{"album":[{"id":"a1","name":"Monster","artist":"Future"}]}}}"#);
        fake.answer(ALBUM);
        let q = block(c.car_voice(VoiceAsk { query: "monster".into(), focus: VoiceFocus::Album, album: Some("Monster".into()), ..Default::default() }));
        assert_eq!(q.songs.len(), 2);
        assert_eq!(q.origin, Some(nori_model::PageOrigin { kind: nori_model::OriginKind::Album, id: "a1".into() }));
        assert!(!q.shuffle);
    }
}
