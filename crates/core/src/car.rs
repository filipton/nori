//! Android Auto's calls: folders read as a screen reads them (stored answer first, so they open offline),
//! the queue a picked row plays, search and spoken requests. The tree itself is nori-library's.

use std::collections::VecDeque;

use crate::cache_policy::{Page, Read};
use crate::client::Client;
use crate::mixes::board::{MixLookup, FAVOURITES_MIX};
use crate::Song;

pub use nori_library::car::*;

/// The folders last listed, newest last: their later pages and the list a picked row plays come from
/// the one read, so a random folder draws once.
#[derive(Default)]
pub struct Shown(VecDeque<(String, BrowsePage)>);

impl Shown {
    /// How many folders are remembered: a car lists a few levels deep at a time.
    const KEPT: usize = 8;

    fn keep(&mut self, parent: &str, page: &BrowsePage) {
        self.0.retain(|(p, _)| p != parent);
        self.0.push_back((parent.into(), page.clone()));
        while self.0.len() > Self::KEPT {
            self.0.pop_front();
        }
    }

    fn get(&self, parent: &str) -> Option<BrowsePage> {
        self.0.iter().find(|(p, _)| p == parent).map(|(_, s)| s.clone())
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// The root folder of the tree.
    pub fn browse_root(&self) -> BrowseFolder {
        folder(ROOT, CarFolder::Root)
    }

    /// Page `page` of `page_size` rows of the root's tabs, `limit` at most, the downloads first while
    /// `offline` (nori-library's `root`).
    pub fn car_root(&self, limit: u32, offline: bool, page: u32, page_size: u32) -> BrowsePage {
        page_of(&BrowsePage { folders: root(limit, offline), ..Default::default() }, page, page_size)
    }

    /// Page `page` of `page_size` rows of folder `parent`: nothing for one that is not known, `failed`
    /// for one that cannot be read now. Pages after the first come from the first's read, and a
    /// search's results from [`Client::car_search`].
    pub async fn browse_children(&self, parent: String, page: u32, page_size: u32) -> BrowsePage {
        let kept = if page > 0 || parent.starts_with("search:") { self.car.lock().get(&parent) } else { None };
        let all = match kept {
            Some(all) => all,
            None => self.listed(&parent).await,
        };
        page_of(&all, page, page_size)
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
                let shown = self.car.lock().get(&row.parent).map(|p| p.songs);
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
        self.listed(&format!("search:{query}")).await
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

/// The most albums a Subsonic server lists at once.
const ALBUMS_PER_READ: usize = 500;

/// A folder that could not be read.
fn failed() -> BrowsePage {
    BrowsePage { failed: true, ..Default::default() }
}

/// Play and Shuffle, for a folder that plays whole.
fn whole() -> Vec<CarAction> {
    vec![CarAction::Play, CarAction::Shuffle]
}

impl Client {
    /// Everything folder `parent` holds, kept for its later pages and picks unless it could not be read.
    async fn listed(&self, parent: &str) -> BrowsePage {
        let (kind, arg) = parent.split_once(':').unwrap_or((parent, ""));
        let page = match kind {
            ROOT => BrowsePage { folders: root(4, false), ..Default::default() },
            "home" => self.car_home().await,
            "library" => BrowsePage { folders: library(), ..Default::default() },
            "albums" => match crate::browse::AlbumSort::of_api(arg) {
                Some(crate::browse::AlbumSort::ByName) => match self.every_album(crate::browse::AlbumSort::ByName, None).await {
                    Some(v) => BrowsePage { folders: album_folders(&v, None, usize::MAX), ..Default::default() },
                    None => failed(),
                },
                Some(sort) => match self.first(Read::AlbumList { kind: sort, size: 100, offset: 0, genre: None }).await {
                    Ok(Page::Albums { v }) => BrowsePage { folders: album_folders(&v, None, 100), ..Default::default() },
                    _ => failed(),
                },
                None => BrowsePage::default(),
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
            "genre" => match self.every_album(crate::browse::AlbumSort::ByGenre, Some(arg)).await {
                Some(v) => BrowsePage { folders: album_folders(&v, None, usize::MAX), actions: whole(), ..Default::default() },
                None => failed(),
            },
            "starred" => match self.first(Read::StarredItems).await {
                Ok(Page::StarredPage { v }) => {
                    let mut p = self.songs_page(library_songs(v.songs), true);
                    p.folders = album_folders(&v.albums, Some(CarGroup::Albums), usize::MAX);
                    p.folders.extend(v.artists.iter().filter(|a| !a.is_external).map(|a| artist_folder(a, Some(CarGroup::Artists))));
                    if !p.folders.is_empty() {
                        p.songs_group = Some(CarGroup::Songs);
                    }
                    p
                }
                _ => failed(),
            },
            "search" => self.search_page(arg).await,
            _ => match self.folder_songs(parent).await {
                Some(songs) => self.songs_page(songs, kind != "random"),
                None if matches!(kind, "album" | "playlist" | "mix" | "random" | "downloads") => failed(),
                None => BrowsePage::default(),
            },
        };
        if !page.failed {
            self.car.lock().keep(parent, &page);
        }
        page
    }

    /// Every album in `kind`'s order (of `genre`, if given), a server's longest list at a time; what
    /// could be read if a later list cannot be, none if the first cannot.
    async fn every_album(&self, kind: crate::browse::AlbumSort, genre: Option<&str>) -> Option<Vec<crate::Album>> {
        let mut all = Vec::new();
        loop {
            let read = Read::AlbumList { kind, size: ALBUMS_PER_READ as i32, offset: all.len() as i32, genre: genre.map(Into::into) };
            match self.first(read).await {
                Ok(Page::Albums { v }) => {
                    let more = v.len() == ALBUMS_PER_READ;
                    all.extend(v);
                    if !more {
                        return Some(all);
                    }
                }
                _ if all.is_empty() => return None,
                _ => return Some(all),
            }
        }
    }

    async fn search_page(&self, query: &str) -> BrowsePage {
        let sizes = crate::browse::library_sizes();
        let read = Read::Search { query: query.into(), songs: sizes.search_songs, albums: sizes.search_albums, artists: sizes.search_artists };
        let Ok(Page::Found { v }) = self.read_now(read).await else { return failed() };
        let mut p = self.songs_page(library_songs(v.songs), false);
        p.folders = v.artists.iter().filter(|a| !a.is_external).map(|a| artist_folder(a, Some(CarGroup::Artists))).collect();
        p.folders.extend(album_folders(&v.albums, Some(CarGroup::Albums), usize::MAX));
        p.songs_group = Some(CarGroup::Songs);
        p
    }

    /// Home: favourites and today's mixes as on the phone's Home, then what was played and added lately.
    async fn car_home(&self) -> BrowsePage {
        let taste = self.settings().prefs(|p| p.taste_model);
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
        for (kind, group) in [(crate::browse::AlbumSort::Recent, CarGroup::RecentlyPlayed), (crate::browse::AlbumSort::Newest, CarGroup::RecentlyAdded)] {
            if let Ok(Page::Albums { v }) = self.first(Read::AlbumList { kind, size: SHELF as i32, offset: 0, genre: None }).await {
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
            "album" => self.first(Read::AlbumById { id: arg.into() }).await.ok()?.songs(),
            "playlist" => self.first(Read::PlaylistById { id: arg.into() }).await.ok()?.songs(),
            "mix" => {
                if arg != FAVOURITES_MIX {
                    self.mix_ensure(arg.into(), false).await;
                }
                match self.core.mix_page(arg.into()) {
                    MixLookup::Ready { sheet } => sheet.songs,
                    _ => return None,
                }
            }
            "starred" => self.first(Read::StarredItems).await.ok()?.songs(),
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

    /// `songs` as a folder's rows, each marked downloaded or not, with Play and Shuffle above them when
    /// `actions`.
    fn songs_page(&self, songs: Vec<Song>, actions: bool) -> BrowsePage {
        let downloaded = {
            let held = self.core.transfers().held();
            songs.iter().map(|s| held.state(&s.id) == crate::transfers::HeldState::Done).collect()
        };
        BrowsePage {
            downloaded,
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
    fn folders_list() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let p = block(c.browse_children(ROOT.into(), 0, 100));
        assert_eq!(p.folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["home", "library", "starred", "downloads"]);
        assert_eq!(c.car_root(3, true, 0, 100).folders[0].id, "downloads");
        assert!(block(c.browse_children("library".into(), 0, 100)).folders.iter().any(|f| f.id == "artists"));
        assert!(fake.asked.lock().is_empty(), "the root and the library's lists ask nothing of the server");

        // Playlists folder lists song counts.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","playlists":{"playlist":[{"id":"p1","name":"Evening","songCount":12}]}}}"#);
        let p = block(c.browse_children("playlists".into(), 0, 100));
        assert_eq!((p.folders[0].id.as_str(), p.folders[0].title.as_str(), p.folders[0].songs, p.folders[0].playable), ("playlist:p1", "Evening", Some(12), true));
        assert!(block(c.browse_children("nonsense".into(), 0, 100)).folders.is_empty());
    }

    const ALBUM: &str = r#"{"subsonic-response":{"status":"ok","album":{"id":"a1","name":"Monster","artist":"Future","song":[
        {"id":"s1","title":"One","album":"Monster"},{"id":"s2","title":"Two","album":"Monster"},{"id":"x","title":"Theirs","isExternal":true}]}}}"#;

    #[test]
    fn albums_play_as_queue() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.download_queue(["s1", "s2"].map(|id| Song { id: id.into(), ..Default::default() }).to_vec()).unwrap();
        c.core.download_settle(vec!["s2".into()], vec![true]).unwrap();
        fake.answer(ALBUM);
        let p = block(c.browse_children("album:a1".into(), 0, 100));
        assert_eq!(p.actions, [CarAction::Play, CarAction::Shuffle]);
        assert_eq!(p.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s1", "s2"], "a provider's song is left out");
        assert_eq!(p.downloaded, [false, true], "only a finished download is marked");
        let asked = fake.asked().len();
        let q = block(c.car_queue(car_song_row("album:a1".into(), "s2".into()))).unwrap();
        assert_eq!((q.songs.len(), q.index, q.shuffle), (2, 1, false));
        assert_eq!(q.origin.unwrap().id, "a1");
        assert_eq!(fake.asked().len(), asked, "the list shown is played, not asked for again");
        assert!(block(c.car_queue("s1".into())).is_none(), "a bare song id is not a row");

        // A spoken album plays as its queue.
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","searchResult3":{"album":[{"id":"a1","name":"Monster","artist":"Future"}]}}}"#);
        fake.answer(ALBUM);
        let q = block(c.car_voice(VoiceAsk { query: "monster".into(), focus: VoiceFocus::Album, album: Some("Monster".into()), ..Default::default() }));
        assert_eq!(q.songs.len(), 2);
        assert_eq!(q.origin, Some(nori_model::PageOrigin { kind: nori_model::OriginKind::Album, id: "a1".into() }));
        assert!(!q.shuffle);
    }

    #[test]
    fn an_unreadable_folder_says_it_failed() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.fail(crate::transport::FailureKind::Connect);
        assert!(block(c.browse_children("album:a1".into(), 0, 100)).failed);
        fake.fail(crate::transport::FailureKind::Connect);
        assert!(block(c.browse_children("albums:newest".into(), 0, 100)).failed);
    }

    #[test]
    fn folder_read_once_per_paging() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","randomSongs":{"song":[{"id":"x","isDir":false},{"id":"y","isDir":false},{"id":"z","isDir":false}]}}}"#);
        let ids = |p: BrowsePage| p.songs.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(block(c.browse_children("random".into(), 0, 2))), ["x", "y"]);
        assert_eq!(ids(block(c.browse_children("random".into(), 1, 2))), ["z"]);
        let q = block(c.car_queue(car_song_row("random".into(), "z".into()))).unwrap();
        assert_eq!((q.songs.len(), q.index), (3, 2));
        assert_eq!(fake.asked().len(), 1, "later pages and the pick are of the same draw");
        let root = c.car_root(4, false, 1, 3);
        assert_eq!(root.folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["downloads"]);
    }

    #[test]
    fn search_results_listed_from_the_search() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[{"id":"s1","isDir":false},{"id":"s2","isDir":false}]}}}"#);
        let found = block(c.car_search("q".into()));
        let listed = block(c.browse_children("search:q".into(), 0, 50));
        assert_eq!(listed.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
        assert_eq!(listed.songs.len(), found.songs.len());
        assert_eq!(fake.asked().len(), 1, "the car's listing of the results asks the server again");
    }

    #[test]
    fn every_album_listed() {
        let albums = |from: usize, n: usize| {
            let v: Vec<String> = (from..from + n).map(|i| format!(r#"{{"id":"a{i}","name":"A{i}"}}"#)).collect();
            format!(r#"{{"subsonic-response":{{"status":"ok","albumList2":{{"album":[{}]}}}}}}"#, v.join(","))
        };
        for parent in ["albums:alphabeticalByName", "genre:Rock"] {
            let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
            fake.answer(&albums(1, 500));
            fake.answer(&albums(501, 20));
            let p = block(c.browse_children(parent.into(), 10, 50));
            assert_eq!(p.folders.last().map(|f| f.id.as_str()), Some("album:a520"), "{parent}");
            assert_eq!(fake.asked().len(), 2, "{parent}");
        }
    }

    #[test]
    fn album_opens_offline_from_cache() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.cache_put("getAlbum&id=a".into(), br#"{"subsonic-response":{"status":"ok","album":{"id":"a","name":"A","song":[{"id":"s","isDir":false}]}}}"#.to_vec()).unwrap();
        fake.fail(crate::transport::FailureKind::Connect);
        assert_eq!(block(c.browse_children("album:a".into(), 0, 10)).songs.len(), 1);
    }
}
