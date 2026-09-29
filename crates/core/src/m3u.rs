//! Matching M3U entries against the index. Parsing and writing is nori-library's.

use crate::{model::*, Core, Result};

pub use nori_library::m3u::*;

impl Core {
    /// The indexed song for each entry (exact artist and title first, then full text); None if unmatched.
    pub fn m3u_match(&self, entries: Vec<M3uEntry>) -> Result<Vec<Option<Song>>> {
        let c = self.db.lock();
        Ok(entries.iter().map(|e| resolve(&c, e)).collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db;
    use crate::history::tests::song;

    fn entry(duration_s: i32, artist: &str, title: &str) -> M3uEntry {
        M3uEntry { duration_s, artist: artist.into(), title: title.into(), path: String::new() }
    }

    #[test]
    fn match_prefers_exact_then_falls_back_to_full_text() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let mut live = song("live", "Dogs", "Pink Floyd", "Live", "", 1977);
        live.duration = 900;
        let mut studio = song("studio", "Dogs", "Pink Floyd", "Animals", "", 1977);
        studio.duration = 1024;
        let songs = vec![
            live,
            studio,
            song("cover", "Dogs", "Some Tribute Band", "Covers", "", 2010),
            song("joga", "Jóga", "Björk", "Homogenic", "", 1997),
            song("remaster", "So What (2009 Remaster)", "Miles Davis", "Kind of Blue", "", 1959),
        ];
        db::index(&mut core.db.lock(), &[], &[], &songs).unwrap();
        let ids = |entries: Vec<M3uEntry>| -> Vec<Option<String>> { core.m3u_match(entries).unwrap().into_iter().map(|s| s.map(|s| s.id)).collect() };
        let some = |id: &str| Some(id.to_string());

        assert_eq!(
            ids(vec![
                entry(1020, "Pink Floyd", "Dogs"),     // exact, duration picks the studio cut
                entry(-1, "pink floyd", "DOGS"),       // exact without a duration: the first
                entry(-1, "BJÖRK", "jóga"),            // unicode case folding
                entry(-1, "Bjork", "Joga"),            // diacritics via the full-text index
                entry(-1, "Miles Davis", "So What"),   // not exact, every word matches
                entry(2000, "", "Dogs"),               // title only
                entry(-1, "Pink Floid", "Dogs"),       // misspelt artist: exact title
                entry(-1, "Miles Davis", "So"),        // a shortened title whose words all match
                entry(-1, "Nobody", "Nothing"),
                entry(-1, "", ""),
                entry(-1, "", "\"' OR * NEAR("),
            ]),
            [some("studio"), some("live"), some("joga"), some("joga"), some("remaster"), some("studio"), some("live"), some("remaster"), None, None, None]
        );
        assert!(core.m3u_match(vec![]).unwrap().is_empty());
    }
}
