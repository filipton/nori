//! The browse tree of a car (Android Auto) or other remote browser.

use nori_model::Song;

/// One of the tree's own folders, which the client names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[repr(u8)]
pub enum CarFolder {
    /// The tree's root (the app itself).
    Root,
    RecentlyPlayed,
    RecentlyAdded,
    MostPlayed,
    Playlists,
    Favourites,
    Random,
    Downloads,
}

/// A folder in the tree: `id` is what is asked for next (the client's `browse_children`).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BrowseFolder {
    pub id: String,
    /// Which of the tree's own folders this is; none for an album or a playlist, named by `title`.
    pub kind: Option<CarFolder>,
    /// The album's or playlist's name; empty for the tree's own folders.
    pub title: String,
    /// An album's artist.
    pub subtitle: Option<String>,
    /// A playlist's number of songs, for the client to say.
    pub songs: Option<u32>,
    /// Signed artwork url, when the folder has a cover.
    pub art: Option<String>,
}

/// What a folder holds: more folders, or songs to play.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BrowsePage {
    pub folders: Vec<BrowseFolder>,
    pub songs: Vec<Song>,
}

/// The tree's root, as the platform names it.
pub const ROOT: &str = "root";
/// How large a cover a car draws a folder with.
pub const ART: u32 = 300;

/// One of the tree's own folders: no subtitle and no cover.
pub fn folder(id: &str, kind: CarFolder) -> BrowseFolder {
    BrowseFolder { id: id.into(), kind: Some(kind), title: String::new(), subtitle: None, songs: None, art: None }
}

/// The folders at the tree's root, in the order a car lists them.
pub fn root() -> Vec<BrowseFolder> {
    vec![
        folder("albums:recent", CarFolder::RecentlyPlayed),
        folder("albums:newest", CarFolder::RecentlyAdded),
        folder("albums:frequent", CarFolder::MostPlayed),
        folder("playlists", CarFolder::Playlists),
        folder("starred", CarFolder::Favourites),
        folder("random", CarFolder::Random),
        folder("downloads", CarFolder::Downloads),
    ]
}

/// The rows of page `page` of `page_size` out of `len`; empty past the end. Twin of the paging in
/// Android's `PlaybackService` (`drop(page * pageSize).take(pageSize)`).
pub fn page(len: usize, page: u32, page_size: u32) -> std::ops::Range<usize> {
    let from = (page as usize).saturating_mul(page_size as usize).min(len);
    from..from.saturating_add(page_size as usize).min(len)
}
