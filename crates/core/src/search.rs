//! Search from the index and the server. Result splitting is nori-library's.

use crate::{Core, Result, SearchResult};

pub use nori_library::search::*;

/// Live search state: each keystroke is answered from the index, and the server (which also knows
/// provider items) once typing pauses. Answers for a stale query are dropped, and an index answer never
/// replaces the server's for the same query.
#[derive(Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct SearchSession(parking_lot::Mutex<Session>);

#[cfg_attr(feature = "ffi", uniffi::export)]
impl SearchSession {
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// The field now holds `text`; blank shows the recent searches.
    pub fn typed(&self, text: String) -> SearchView {
        let mut s = self.0.lock();
        let blank = text.trim().is_empty();
        s.text = text;
        s.from_server = false;
        s.error = None;
        s.searching = !blank;
        if blank {
            s.split = None;
        }
        s.view()
    }

    /// Applies the index's answer to `query` unless stale or the server already answered.
    pub fn local(&self, core: std::sync::Arc<Core>, query: String, limit: u32) -> Result<Option<SearchView>> {
        let split = core.local_search_split(query.clone(), limit)?;
        let mut s = self.0.lock();
        if s.query() != query || s.from_server {
            return Ok(None);
        }
        s.split = Some(split);
        Ok(Some(s.view()))
    }

    /// Applies the server's answer to `query`; None if stale.
    pub fn server(&self, query: String, result: SearchResult) -> Option<SearchView> {
        let split = search_split(result);
        let mut s = self.0.lock();
        if s.query() != query {
            return None;
        }
        s.split = Some(split);
        s.from_server = true;
        s.searching = false;
        s.error = None;
        Some(s.view())
    }

    /// Asks the server and applies the answer ([`SearchSession::server`]); an error when the request fails.
    pub async fn ask(&self, client: std::sync::Arc<crate::client::Client>, query: String) -> crate::client::NetResult<Option<SearchView>> {
        let sizes = crate::browse::library_sizes();
        let read = crate::cache_policy::Read::Search { query: query.clone(), songs: sizes.search_songs, albums: sizes.search_albums, artists: sizes.search_artists };
        let crate::cache_policy::Page::Found { v } = client.read_now(read).await? else { return Ok(None) };
        Ok(self.server(query, v))
    }

    /// The server failed for `query`: keeps the index answer and reports the fallback.
    pub fn failed(&self, query: String, reason: Option<String>) -> Option<SearchView> {
        let mut s = self.0.lock();
        if s.query() != query {
            return None;
        }
        s.searching = false;
        s.error = Some(SearchFallback { reason });
        Some(s.view())
    }

    pub fn scope(&self, scope: SearchScope) -> SearchView {
        let mut s = self.0.lock();
        s.scope = scope;
        s.view()
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Remembers `query` and returns the history; None (not remembered) when too short.
    pub fn search_remember_recent(&self, query: String) -> Result<Option<Vec<String>>> {
        if query.encode_utf16().count() < REMEMBER_MIN_UTF16 {
            return Ok(None);
        }
        self.search_remember(query)?;
        Ok(Some(self.search_history()?))
    }
}

impl Core {
    /// Index search, split like the server's.
    pub(crate) fn local_search_split(&self, query: String, limit: u32) -> Result<SearchSplit> {
        Ok(split(self.local_search(query, limit)?))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{Album, Artist, Song};

    fn result() -> SearchResult {
        let s = |id: &str, ext: bool| Song { id: id.into(), is_external: ext, ..Default::default() };
        let a = |id: &str, ext: bool| Album { id: id.into(), is_external: ext, ..Default::default() };
        SearchResult {
            artists: vec![Artist { id: "ar".into(), ..Default::default() }],
            albums: vec![a("al", false), a("ext-al", true), a("ext-al", true)],
            songs: vec![s("1", false), s("ext-2", true), s("1", false), s("3", false)],
        }
    }

    #[test]
    fn search_session() {
        let core = Core::new(String::new(), "t".into(), Default::default()).unwrap();
        let s = SearchSession::new();
        let v = s.typed(" dogs ".into());
        assert_eq!((v.query.as_str(), v.searching, v.shown.is_none()), ("dogs", true, true));
        assert!(s.local(core.clone(), "dog".into(), 30).unwrap().is_none(), "stale");
        assert!(s.local(core.clone(), "dogs".into(), 30).unwrap().is_some());
        let v = s.server("dogs".into(), result()).unwrap();
        assert!(v.from_server && !v.searching && v.scopes_offered && !v.nothing_found);
        assert_eq!(v.shown.as_ref().unwrap().songs.len(), 3);
        assert!(s.local(core, "dogs".into(), 30).unwrap().is_none(), "server answer stands");
        assert_eq!(s.scope(SearchScope::Providers).shown.unwrap().songs.len(), 1);
        assert_eq!(s.scope(SearchScope::Library).shown.unwrap().songs.len(), 2);
        assert!(s.server("cats".into(), result()).is_none());
        let failed = s.failed("dogs".into(), Some("timeout".into())).unwrap();
        assert_eq!(failed.error, Some(SearchFallback { reason: Some("timeout".into()) }));
        let v = s.typed("  ".into());
        assert!(v.shown.is_none() && !v.searching && v.error.is_none());
        assert!(!s.scope(SearchScope::Everything).scopes_offered);
        let none = s.typed("x".into());
        assert!(!none.nothing_found);
        let v = s.server("x".into(), SearchResult::default()).unwrap();
        assert!(v.nothing_found);
        assert_eq!(s.scope(SearchScope::Library).shown.unwrap().songs.len(), 0);

        // Short queries are not remembered.
        let core = Core::new(String::new(), "t".into(), Default::default()).unwrap();
        assert_eq!(core.search_remember_recent("a".into()).unwrap(), None);
        assert!(core.search_history().unwrap().is_empty());
        // One emoji is two UTF-16 units.
        assert_eq!(core.search_remember_recent("🎵".into()).unwrap(), Some(vec!["🎵".to_string()]));
        let history = core.search_remember_recent("dogs".into()).unwrap().unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.contains(&"dogs".to_string()));
    }

}
