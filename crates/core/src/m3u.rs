//! Matching M3U entries against the index. Parsing and writing is nori-library's.

use crate::{model::*, Core, Result};

pub use nori_library::m3u::*;

impl Core {
    /// The indexed song for each entry (exact artist and title first, then full text); None if unmatched.
    pub(crate) fn m3u_match(&self, entries: Vec<M3uEntry>) -> Result<Vec<Option<Song>>> {
        let c = self.db.lock();
        Ok(entries.iter().map(|e| resolve(&c, e)).collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db;
    use crate::history::tests::song;

    fn entry(duration_s: Option<u32>, artist: &str, title: &str) -> M3uEntry {
        M3uEntry { duration_s, artist: artist.into(), title: title.into(), path: String::new() }
    }

    #[test]
    fn match_exact_then_full_text() {
        let core = Core::new(String::new(), "t".into(), Default::default()).unwrap();
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
                entry(Some(1020), "Pink Floyd", "Dogs"),     // exact, duration picks the studio cut
                entry(None, "pink floyd", "DOGS"),       // exact without a duration: the first
                entry(None, "BJÖRK", "jóga"),            // unicode case folding
                entry(None, "Bjork", "Joga"),            // diacritics via the full-text index
                entry(None, "Miles Davis", "So What"),   // not exact, every word matches
                entry(Some(2000), "", "Dogs"),               // title only
                entry(None, "Pink Floid", "Dogs"),       // misspelt artist: exact title
                entry(None, "Miles Davis", "So"),        // a shortened title whose words all match
                entry(None, "Nobody", "Nothing"),
                entry(None, "", ""),
                entry(None, "", "\"' OR * NEAR("),
            ]),
            [some("studio"), some("live"), some("joga"), some("joga"), some("remaster"), some("studio"), some("live"), some("remaster"), None, None, None]
        );
        assert!(core.m3u_match(vec![]).unwrap().is_empty());
    }
}
