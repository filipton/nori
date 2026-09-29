//! This session's star changes, kept by the core that made them, and laid over a list the server sent
//! earlier.
//!
//! A heart changes the moment it is pressed, long before the server has answered and the favourites
//! list has been asked for again. Until then the list on screen is a snapshot from before the press, so
//! the marks (per kind, the item's id -> starred) are applied on top of it: an item whose mark says it is
//! no longer starred leaves the list under the finger. A mark that says "starred" adds nothing, because
//! the snapshot does not hold the item's details; the re-asked list brings it.

use std::collections::HashMap;

use nori_model::{Album, Artist, Song};
use nori_net::requests::Starrable;

use crate::pages::Starred;

/// This session's marks, one map per kind keyed by the item's id: a screen asks "is this song
/// starred" by id alone. Each core keeps its own, so a server profile never wears another's.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StarMarks {
    pub songs: HashMap<String, bool>,
    pub albums: HashMap<String, bool>,
    pub artists: HashMap<String, bool>,
}

/// A heart pressed: the marks as they are after it, and what [`StarMarks::restore`] puts back should the
/// server refuse it.
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

    /// A heart pressed: the mark goes up at once, before the server is asked, so the heart fills under
    /// the finger.
    pub fn mark(&mut self, kind: Starrable, id: String, on: bool) -> StarMarked {
        let previous = self.of_mut(kind).insert(id, on);
        StarMarked { previous, marks: self.clone() }
    }

    /// The server refused a star change (being offline is not refusing: those are kept and sent later), so
    /// the heart must not keep showing it: the mark from before comes back. Returns the marks as they are now.
    pub fn restore(&mut self, kind: Starrable, id: String, previous: Option<bool>) -> StarMarks {
        match previous {
            Some(on) => self.of_mut(kind).insert(id, on),
            None => self.of_mut(kind).remove(&id),
        };
        self.clone()
    }

    /// False only when this session unstarred the item; an item without a mark keeps whatever the list says.
    pub fn kept(&self, kind: Starrable, id: &str) -> bool {
        self.of(kind).get(id) != Some(&false)
    }

    /// The favourites answer as it is read: artists, albums and songs this session unstarred are left out.
    pub fn overlay(&self, starred: Starred) -> Starred {
        let artists: Vec<Artist> = starred.artists.into_iter().filter(|a| self.kept(Starrable::Artist, &a.id)).collect();
        let songs: Vec<Song> = starred.songs.into_iter().filter(|s| self.kept(Starrable::Song, &s.id)).collect();
        Starred::new(artists, self.overlay_albums(starred.albums), songs)
    }

    /// The favourite albums shelf of the home page, which is an album list rather than a favourites answer.
    pub fn overlay_albums(&self, albums: Vec<Album>) -> Vec<Album> {
        albums.into_iter().filter(|a| self.kept(Starrable::Album, &a.id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unstarred_mark_removes() {
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
    }

    #[test]
    fn refused_star_restores_mark() {
        let mut m = StarMarks::default();
        let first = m.mark(Starrable::Album, "1".into(), true);
        assert_eq!(first.previous, None);
        assert_eq!(first.marks.albums.get("1"), Some(&true));
        let second = m.mark(Starrable::Album, "1".into(), false);
        assert_eq!(second.previous, Some(true));
        assert_eq!(m.restore(Starrable::Album, "1".into(), second.previous).albums.get("1"), Some(&true));
        assert_eq!(m.restore(Starrable::Album, "1".into(), first.previous).albums.get("1"), None);
        // Split by kind, keyed by the id alone: a song and an album may share an id.
        let m2 = m.mark(Starrable::Song, "1".into(), true).marks;
        assert_eq!((m2.songs.get("1"), m2.albums.get("1"), m2.artists.get("1")), (Some(&true), None, None));
        m.mark(Starrable::Album, "2".into(), false);
        assert!(m.overlay_albums(vec![Album { id: "2".into(), ..Default::default() }]).is_empty());
    }
}
