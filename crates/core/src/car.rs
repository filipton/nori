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

    /// The contents of folder `parent`; empty if unknown or unreadable.
    pub async fn browse_children(&self, parent: String) -> BrowsePage {
        let (kind, arg) = parent.split_once(':').unwrap_or((parent.as_str(), ""));
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
            "album" => BrowsePage { folders: Vec::new(), songs: songs(self.read_now(Read::AlbumSongs { id: arg.into() }).await) },
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
            "playlist" => BrowsePage { folders: Vec::new(), songs: songs(self.read_now(Read::PlaylistSongs { id: arg.into() }).await) },
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
        let p = block(c.browse_children(ROOT.into()));
        assert_eq!(p.folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["albums:recent", "albums:newest", "albums:frequent", "playlists", "starred", "random", "downloads"]);
        assert!(fake.asked.lock().is_empty());
    }

    #[test]
    fn playlists_folder_lists_song_counts() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(r#"{"subsonic-response":{"status":"ok","playlists":{"playlist":[{"id":"p1","name":"Evening","songCount":12}]}}}"#);
        let p = block(c.browse_children("playlists".into()));
        assert_eq!(p.folders, vec![BrowseFolder { id: "playlist:p1".into(), kind: None, title: "Evening".into(), subtitle: None, songs: Some(12), art: None }]);
        assert!(block(c.browse_children("nonsense".into())).folders.is_empty());
    }
}
