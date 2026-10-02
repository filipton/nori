//! Search results split once per answer into everything, the library's and the providers' (`isExternal`),
//! and the search screen's state.

use std::collections::HashSet;

use nori_model::SearchResult;

#[derive(Debug, Clone, Default)]
pub struct SearchSplit {
    pub everything: SearchResult,
    /// Only the library's items; None when that is everything (there are no provider items).
    pub library: Option<SearchResult>,
    /// Only the providers' items; None when there are none.
    pub providers: Option<SearchResult>,
}

pub fn split(r: SearchResult) -> SearchSplit {
    let has_providers = r.songs.iter().any(|s| s.is_external) || r.albums.iter().any(|a| a.is_external) || r.artists.iter().any(|a| a.is_external);
    if !has_providers {
        return SearchSplit { everything: r, library: None, providers: None };
    }
    let part = |external: bool| SearchResult {
        artists: r.artists.iter().filter(|a| a.is_external == external).cloned().collect(),
        albums: r.albums.iter().filter(|a| a.is_external == external).cloned().collect(),
        songs: r.songs.iter().filter(|s| s.is_external == external).cloned().collect(),
    };
    let (library, providers) = (part(false), part(true));
    SearchSplit { everything: r, library: Some(library), providers: Some(providers) }
}

/// The first of each id.
pub fn distinct<T>(list: impl IntoIterator<Item = T>, id: impl Fn(&T) -> &str) -> Vec<T> {
    let mut seen = HashSet::new();
    list.into_iter().filter(|x| seen.insert(id(x).to_string())).collect()
}

/// The server's answer split, the first of each repeated id kept (merged provider results repeat ids).
pub fn search_split(result: SearchResult) -> SearchSplit {
    split(SearchResult {
        artists: distinct(result.artists, |a| &a.id),
        albums: distinct(result.albums, |a| &a.id),
        songs: distinct(result.songs, |s| &s.id),
    })
}

/// Which of the answer the search screen shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SearchScope {
    #[default]
    Everything,
    /// Only what is in the library already; everything when there are no provider items.
    Library,
    /// Only what the providers offer; nothing when there is none.
    Providers,
}

/// The scope chips, in their order; the client names each.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn search_scopes() -> Vec<SearchScope> {
    vec![SearchScope::Everything, SearchScope::Library, SearchScope::Providers]
}

/// The server could not be asked and the offline answer stays; `reason` as `SearchSession::failed` gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchFallback {
    pub reason: Option<String>,
}

impl SearchSplit {
    /// The answer narrowed to `scope`.
    fn shown(&self, scope: SearchScope) -> SearchResult {
        match scope {
            SearchScope::Everything => self.everything.clone(),
            SearchScope::Library => self.library.clone().unwrap_or_else(|| self.everything.clone()),
            SearchScope::Providers => self.providers.clone().unwrap_or_default(),
        }
    }
}

/// What the search screen shows now.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchView {
    /// What is in the field, as typed.
    pub text: String,
    /// The query that is asked: the text trimmed. Empty for none.
    pub query: String,
    /// The answer narrowed to the scope; None with no query (the recent searches show instead).
    pub shown: Option<SearchResult>,
    /// The answer is the server's rather than the offline index's.
    pub from_server: bool,
    /// The server is being asked.
    pub searching: bool,
    /// The server could not be asked; the offline answer stays on screen.
    pub error: Option<SearchFallback>,
    pub scope: SearchScope,
    /// The scope chips are offered: there are provider items, or a scope other than everything is on.
    pub scopes_offered: bool,
    /// The server answered with nothing at all.
    pub nothing_found: bool,
}

/// What the search field holds and what answers it has had: the state of the core's `SearchSession`.
#[derive(Default)]
pub struct Session {
    pub text: String,
    pub split: Option<SearchSplit>,
    pub from_server: bool,
    pub searching: bool,
    pub error: Option<SearchFallback>,
    pub scope: SearchScope,
}

impl Session {
    /// The query the field holds: its text without the spaces around it.
    pub fn query(&self) -> &str {
        self.text.trim()
    }

    /// What the screen shows now.
    pub fn view(&self) -> SearchView {
        let shown = self.split.as_ref().map(|s| s.shown(self.scope));
        let has_providers = self.split.as_ref().is_some_and(|s| s.providers.is_some());
        let empty = shown.as_ref().is_some_and(|r| r.songs.is_empty() && r.albums.is_empty() && r.artists.is_empty());
        SearchView {
            text: self.text.clone(),
            query: self.query().to_string(),
            nothing_found: self.from_server && empty,
            shown,
            from_server: self.from_server,
            searching: self.searching,
            error: self.error.clone(),
            scope: self.scope,
            scopes_offered: has_providers || self.scope != SearchScope::Everything,
        }
    }
}

/// Shorter than this, a query is a keystroke on the way to one, not one worth remembering.
pub const REMEMBER_MIN_UTF16: usize = 2;

/// Live search's wait after a keystroke: none for a blank field, else `delay_ms`. Twin of Android's
/// `SearchViewModel` debounce.
pub fn live_delay_ms(query: &str, delay_ms: i64) -> i64 {
    if query.chars().all(kotlin_whitespace) { 0 } else { delay_ms }
}

/// Kotlin's `Char.isWhitespace`, which differs from Rust's (U+001C-U+001F count, U+0085 does not).
pub fn kotlin_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r' | '\u{1c}'..='\u{20}' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_model::{Album, Artist, Song};

    fn result() -> SearchResult {
        let s = |id: &str, ext: bool| Song { id: id.into(), is_external: ext, ..Default::default() };
        let a = |id: &str, ext: bool| Album { id: id.into(), is_external: ext, ..Default::default() };
        SearchResult {
            artists: vec![Artist { id: "ar".into(), ..Default::default() }],
            albums: vec![a("al", false), a("ext-al", true), a("ext-al", true)],
            songs: vec![s("1", false), s("ext-2", true), s("1", false), s("3", false)],
        }
    }

    fn ids<T>(l: &[T], id: impl Fn(&T) -> &str) -> Vec<&str> {
        l.iter().map(id).collect()
    }

    #[test]
    fn server_answers() {
        let r = search_split(result());
        assert!(r.providers.is_some());
        assert_eq!(ids(&r.everything.songs, |s| &s.id), ["1", "ext-2", "3"]);
        assert_eq!(ids(&r.everything.albums, |a| &a.id), ["al", "ext-al"]);
        let (lib, prov) = (r.library.unwrap(), r.providers.unwrap());
        assert_eq!(ids(&lib.songs, |s| &s.id), ["1", "3"]);
        assert_eq!(lib.artists.len(), 1);
        assert_eq!(ids(&prov.songs, |s| &s.id), ["ext-2"]);
        assert_eq!(ids(&prov.albums, |a| &a.id), ["ext-al"]);
        assert!(prov.artists.is_empty());

        // A library only answer is not copied.
        let mut r = result();
        r.songs.retain(|s| !s.is_external);
        r.albums.retain(|a| !a.is_external);
        let s = search_split(r);
        assert!(s.library.is_none() && s.providers.is_none());
    }

}
