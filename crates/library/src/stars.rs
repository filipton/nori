//! This session's star changes, laid over favourites the server sent earlier: an unstarred item leaves at
//! once; a newly starred one arrives with the next answer (the old one lacks its details).

use std::collections::HashMap;

use nori_model::{Album, Artist, Song};
use nori_net::requests::Starrable;

use crate::pages::Starred;

/// This session's marks, one id -> starred map per kind; each core keeps its own.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StarMarks {
    pub songs: HashMap<String, bool>,
    pub albums: HashMap<String, bool>,
    pub artists: HashMap<String, bool>,
}

/// The marks after a press, and the mark before it for [`StarMarks::restore`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StarMarked {
    pub previous: Option<bool>,
    pub marks: StarMarks,
}

impl StarMarks {
    fn of(&self, kind: Starrable) -> &HashMap<String, bool> {
        match kind {
            Starrable::Song => &self.songs,
            Starrable::Album => &self.albums,
            Starrable::Artist => &self.artists,
        }
    }

    fn of_mut(&mut self, kind: Starrable) -> &mut HashMap<String, bool> {
        match kind {
            Starrable::Song => &mut self.songs,
            Starrable::Album => &mut self.albums,
            Starrable::Artist => &mut self.artists,
        }
    }

    /// A heart pressed: marked at once, before the server is asked.
    pub fn mark(&mut self, kind: Starrable, id: String, on: bool) -> StarMarked {
        let previous = self.of_mut(kind).insert(id, on);
        StarMarked { previous, marks: self.clone() }
    }

    /// The server refused the press that marked `pressed`: the mark from before comes back, unless a
    /// later press changed it. Returns the marks now.
    pub fn restore(&mut self, kind: Starrable, id: String, pressed: bool, previous: Option<bool>) -> StarMarks {
        let marks = self.of_mut(kind);
        if marks.get(&id) == Some(&pressed) {
            match previous {
                Some(on) => marks.insert(id, on),
                None => marks.remove(&id),
            };
        }
        self.clone()
    }

    /// False only when this session unstarred the item; an item without a mark keeps whatever the list says.
    pub fn kept(&self, kind: Starrable, id: &str) -> bool {
        self.of(kind).get(id) != Some(&false)
    }

    /// The favourites without what this session unstarred.
    pub fn overlay(&self, starred: Starred) -> Starred {
        let artists: Vec<Artist> = starred.artists.into_iter().filter(|a| self.kept(Starrable::Artist, &a.id)).collect();
        let songs: Vec<Song> = starred.songs.into_iter().filter(|s| self.kept(Starrable::Song, &s.id)).collect();
        Starred::new(artists, self.overlay_albums(starred.albums), songs)
    }

    /// Albums without those this session unstarred (the home page's favourites shelf).
    pub fn overlay_albums(&self, albums: Vec<Album>) -> Vec<Album> {
        albums.into_iter().filter(|a| self.kept(Starrable::Album, &a.id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_marks() {
        let s = |id: &str| Song { id: id.into(), ..Default::default() };
        let a = |id: &str| Album { id: id.into(), ..Default::default() };
        let r = |id: &str| Artist { id: id.into(), ..Default::default() };
        let starred = Starred::new(vec![r("1"), r("2")], vec![a("1"), a("2")], vec![s("1"), s("2"), s("3")]);
        // The same id under another kind does not count: an album "1" unstarred is not song "1".
        let mut m = StarMarks::default();
        m.mark(Starrable::Song, "2".into(), false);
        m.mark(Starrable::Song, "3".into(), true);
        m.mark(Starrable::Album, "1".into(), false);
        m.mark(Starrable::Artist, "9".into(), false);
        let out = m.overlay(starred);
        assert_eq!(out.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["1", "3"]);
        assert_eq!(out.albums.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["2"]);
        assert_eq!(out.artists.len(), 2);
        assert_eq!(out.library_songs, 2, "counted after the overlay");
        assert_eq!(m.overlay_albums(vec![a("1"), a("2")]).len(), 1);
        assert_eq!(StarMarks::default().overlay_albums(vec![a("1")]).len(), 1);

        // Refused star restores mark.
        let mut m = StarMarks::default();
        let first = m.mark(Starrable::Album, "1".into(), true);
        assert_eq!(first.previous, None);
        assert_eq!(first.marks.albums.get("1"), Some(&true));
        let second = m.mark(Starrable::Album, "1".into(), false);
        assert_eq!(second.previous, Some(true));
        assert_eq!(m.restore(Starrable::Album, "1".into(), true, first.previous).albums.get("1"), Some(&false), "a later press stays");
        assert_eq!(m.restore(Starrable::Album, "1".into(), false, second.previous).albums.get("1"), Some(&true));
        assert_eq!(m.restore(Starrable::Album, "1".into(), true, first.previous).albums.get("1"), None);
        // Split by kind, keyed by the id alone: a song and an album may share an id.
        let m2 = m.mark(Starrable::Song, "1".into(), true).marks;
        assert_eq!((m2.songs.get("1"), m2.albums.get("1"), m2.artists.get("1")), (Some(&true), None, None));
        m.mark(Starrable::Album, "2".into(), false);
        assert!(m.overlay_albums(vec![Album { id: "2".into(), ..Default::default() }]).is_empty());
    }

}
