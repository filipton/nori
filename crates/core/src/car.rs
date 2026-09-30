//! Android Auto browse tree reads. The tree itself is nori-library's.

use crate::cache_policy::{Page, Read};
use crate::client::Client;

pub use nori_library::car::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// The root folder of the tree.
    pub fn browse_root(&self) -> BrowseFolder {
        folder(ROOT, CarFolder::Root)
    }

    /// Page `page` of `page_size` entries of folder `parent`, its folders first; empty if unknown or
    /// unreadable. Later pages come from the folder read for the first, so they neither ask again nor
    /// differ (a random folder draws once).
    pub async fn browse_children(&self, parent: String, page: u32, page_size: u32) -> BrowsePage {
        let kept = self.car_folder.lock().clone().filter(|(p, _)| page > 0 && *p == parent);
        let all = match kept {
            Some((_, all)) => all,
            None => {
                let all = self.folder(&parent).await;
                *self.car_folder.lock() = Some((parent, all.clone()));
                all
            }
        };
        page_of(&all, page, page_size)
    }
}

impl Client {
    async fn folder(&self, parent: &str) -> BrowsePage {
        let (kind, arg) = parent.split_once(':').unwrap_or((parent, ""));
        let art = |id: &Option<String>| id.as_ref().map(|c| self.core.cover_url(c.clone(), ART));
        let songs = |p: Result<Page, _>| match p {
            Ok(Page::Songs { v }) => v,
            _ => Vec::new(),
        };
        match kind {
            ROOT => BrowsePage { folders: root(), songs: Vec::new() },
            "albums" => match self.first(Read::AlbumList { kind: arg.to_lowercase(), size: 40, offset: 0, genre: None }).await {
                Ok(Page::Albums { v }) => BrowsePage {
                    folders: v
                        .iter()
                        .map(|a| BrowseFolder { id: format!("album:{}", a.id), kind: None, title: a.name.clone(), subtitle: Some(a.artist.clone()), songs: None, art: art(&a.cover_art) })
                        .collect(),
                    songs: Vec::new(),
                },
                _ => BrowsePage::default(),
            },
            "album" => BrowsePage { folders: Vec::new(), songs: match self.first(Read::AlbumById { id: arg.into() }).await {
                    Ok(Page::AlbumPage { v }) => v.songs,
                    _ => Vec::new(),
                },
            },
            "playlists" => match self.first(Read::PlaylistList).await {
                Ok(Page::Playlists { v }) => BrowsePage {
                    folders: v
                        .iter()
                        .map(|p| BrowseFolder {
                            id: format!("playlist:{}", p.id),
                            kind: None,
                            title: p.name.clone(),
                            subtitle: None,
                            songs: Some(p.song_count),
                            art: art(&p.cover_art),
                        })
                        .collect(),
                    songs: Vec::new(),
                },
                _ => BrowsePage::default(),
            },
            "playlist" => BrowsePage { folders: Vec::new(), songs: match self.first(Read::PlaylistById { id: arg.into() }).await {
                    Ok(Page::PlaylistPage { v }) => v.songs,
                    _ => Vec::new(),
                },
            },
            "starred" => match self.first(Read::StarredItems).await {
                Ok(Page::StarredPage { v }) => BrowsePage { folders: Vec::new(), songs: v.songs },
                _ => BrowsePage::default(),
            },
            "random" => BrowsePage { folders: Vec::new(), songs: songs(self.read_now(Read::RandomSongs { size: 50, genre: None }).await) },
            "downloads" => BrowsePage { folders: Vec::new(), songs: self.core.downloads(true).unwrap_or_default() },
            _ => BrowsePage::default(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::tests::{block, client};
    use crate::client::NetProfile;

    #[test]
    fn root_lists_folders_without_network() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        let p = block(c.browse_children(ROOT.into(), 0, 100));
        assert_eq!(p.folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["albums:recent", "albums:newest", "albums:frequent", "playlists", "starred", "random", "downloads"]);
        assert!(fake.asked.lock().is_empty());
    }

    #[test]
    fn playlists_folder_lists_song_counts() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","playlists":{"playlist":[{"id":"p1","name":"Evening","songCount":12}]}}}"#);
        let p = block(c.browse_children("playlists".into(), 0, 100));
        assert_eq!(p.folders, vec![BrowseFolder { id: "playlist:p1".into(), kind: None, title: "Evening".into(), subtitle: None, songs: Some(12), art: None }]);
        assert!(block(c.browse_children("nonsense".into(), 0, 100)).folders.is_empty());
    }

    #[test]
    fn a_folder_is_read_once_for_all_its_pages() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","randomSongs":{"song":[{"id":"x","isDir":false},{"id":"y","isDir":false},{"id":"z","isDir":false}]}}}"#);
        let ids = |p: BrowsePage| p.songs.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(block(c.browse_children("random".into(), 0, 2))), ["x", "y"]);
        assert_eq!(ids(block(c.browse_children("random".into(), 1, 2))), ["z"]);
        assert_eq!(fake.asked().len(), 1, "the second page is of the same draw");
        let root = block(c.browse_children(ROOT.into(), 1, 4));
        assert_eq!(root.folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["starred", "random", "downloads"]);
    }

    #[test]
    fn an_album_opens_offline_from_the_cache() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        c.core.cache_put("getAlbum&id=a".into(), br#"{"subsonic-response":{"status":"ok","album":{"id":"a","name":"A","song":[{"id":"s","isDir":false}]}}}"#.to_vec()).unwrap();
        fake.fail(crate::transport::FailureKind::Connect);
        assert_eq!(block(c.browse_children("album:a".into(), 0, 10)).songs.len(), 1);
    }
}
