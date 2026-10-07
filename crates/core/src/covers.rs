//! Cover prefetching (downloads, queue neighbours), cover URLs and cache keys. Prefetches use the same
//! URLs the UI requests, so a warmed cover is a cache hit.

use crate::Core;

/// Row and card size. Shared because servers render each requested size on demand (over 1 s each).
const ROW: u32 = 320;
/// Player, notification and lock screen size.
const FULL: u32 = 800;

const SIZES: [u32; 2] = [ROW, FULL];

/// The rendition to fetch for a cover drawn `px` square: the row size up to it, else the full size.
/// The client decodes the rendition to its own size, so every view of a cover shares one of two
/// downloads (and the disk cache keeps two files per cover, not one per view size).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn cover_rendition(px: u32) -> u32 {
    if px <= ROW {
        ROW
    } else {
        FULL
    }
}

/// Whether cover id `id` (up to the next `&`) is an octo-fiesta provider's: `ext-...` or
/// `pl-<provider>-<id>`. Navidrome's own playlist covers (`pl-<id>_<timestamp>`) are not.
fn provider_cover_id(id: &str) -> bool {
    let id = id.split('&').next().unwrap_or(id);
    if id.starts_with("ext-") {
        return true;
    }
    let Some(rest) = id.strip_prefix("pl-") else { return false };
    if rest.contains('_') {
        return false;
    }
    match rest.split_once('-') {
        Some((provider, _)) => !provider.is_empty() && provider.bytes().all(|b| b.is_ascii_lowercase()),
        None => false,
    }
}

/// Cover sizes, URL parts and cache budgets, read once by the client, which builds URLs itself from
/// `Core::url_prefix`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct CoverRules {
    pub row: u32,
    pub card: u32,
    pub full: u32,
    /// Appended to the signed `getCoverArt` prefix: `&id=<encoded>` then `&size=`.
    pub id_param: String,
    pub size_param: String,
    /// Memory share for decoded covers, and the disk cache size.
    pub memory_share: f64,
    pub disk_bytes: u64,
    /// Share of that memory kept while the app is not visible.
    pub hidden_share: f64,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn cover_rules() -> CoverRules {
    CoverRules {
        row: ROW,
        card: ROW,
        full: FULL,
        id_param: "&id=".into(),
        size_param: "&size=".into(),
        memory_share: 0.15,
        disk_bytes: 256 * 1024 * 1024,
        hidden_share: 0.25,
    }
}

/// A cover id at one size.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct CoverWant {
    pub id: String,
    pub size: u32,
}

/// `arts` at both sizes, deduplicated, provider covers skipped, at most `cap` covers.
pub(crate) fn cover_wants(arts: Vec<String>, cap: u32) -> Vec<CoverWant> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for art in arts.iter().filter(|a| !provider_cover_id(a)) {
        if seen.len() == cap as usize {
            break;
        }
        if seen.insert(art.as_str()) {
            out.extend(SIZES.iter().map(|&size| CoverWant { id: art.clone(), size }));
        }
    }
    out
}

/// Cover cap per download batch.
const DOWNLOAD_COVERS: u32 = 500;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Cover URLs to warm for songs being downloaded, so they have artwork offline ([`cover_wants`]).
    pub fn download_cover_urls(&self, arts: Vec<String>) -> Vec<String> {
        let prefix = self.url_prefix("getCoverArt".into());
        cover_wants(arts, DOWNLOAD_COVERS)
            .into_iter()
            .map(|w| {
                let mut out = String::new();
                cover_url_into(&mut out, &prefix, &w.id, w.size as i32);
                out
            })
            .collect()
    }
}

impl Core {
    /// The URL of cover `id` at `size` px ([`cover_url_into`]).
    pub fn cover_address(&self, id: String, size: u32) -> String {
        let mut out = String::new();
        cover_url_into(&mut out, &self.url_prefix("getCoverArt".into()), &id, size as i32);
        out
    }
}

/// Queue positions to prefetch around `index`, nearest first: the skip targets `previous` and `next`
/// (media3 indexes, -1 for none), then outwards both ways up to `ahead` steps (`ahead` 0: only
/// `previous`). In range, excluding `index`, deduplicated.
pub(crate) fn cover_neighbours(index: i32, previous: i32, next: i32, ahead: i32, len: u32) -> Vec<u32> {
    let ahead = ahead.max(0);
    let mut around = vec![previous, next];
    for d in 2..=ahead {
        around.push(index + d);
        around.push(index - d);
    }
    around.truncate(if ahead == 0 { 1 } else { ahead as usize * 2 });
    let mut out: Vec<u32> = Vec::with_capacity(around.len());
    for p in around {
        if p != index && p >= 0 && (p as u32) < len && !out.contains(&(p as u32)) {
            out.push(p as u32);
        }
    }
    out
}

/// Covers to colour and prefetch around the current song.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct CoversAround {
    /// The current cover then the skip targets' (provider covers included), for colour extraction.
    pub near: Vec<String>,
    /// Prefetches: [`cover_wants`] of [`cover_neighbours`].
    pub wants: Vec<CoverWant>,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// [`CoversAround`] over the queue; positions are media3 indexes, -1 for none.
    pub fn covers_around(&self, index: i32, previous: i32, next: i32, ahead: i32) -> CoversAround {
        // Only the ids asked about are looked at, not the whole queue.
        let session = &self.session;
        session.playlist(|p| {
            let ids = p.ids();
            around(ids.len() as u32, |i| session.cover_art(&ids[i as usize]), index, previous, next, ahead)
        })
    }
}

/// [`CoversAround`] for a queue of `len`, `art` giving position `i`'s cover.
fn around(len: u32, art: impl Fn(u32) -> Option<String>, index: i32, previous: i32, next: i32, ahead: i32) -> CoversAround {
    let at = |i: u32| (i < len).then(|| art(i)).flatten();
    let current = u32::try_from(index).ok().and_then(at);
    let near = current.into_iter().chain(cover_neighbours(index, previous, next, 1, len).into_iter().filter_map(at)).collect();
    let wants = cover_wants(cover_neighbours(index, previous, next, ahead, len).into_iter().filter_map(at).collect(), u32::MAX);
    CoversAround { near, wants }
}

/// `base` without scheme, trailing slash or `/rest`.
fn host_of(base: &str) -> &str {
    let base = base.trim().trim_end_matches('/');
    let base = base.strip_suffix("/rest").unwrap_or(base);
    base.split_once("://").map_or(base, |(_, rest)| rest)
}

/// Auth and protocol query params, excluded from cover keys.
const SIGNATURE: [&str; 8] = ["u", "t", "s", "p", "apiKey", "v", "c", "f"];

/// Whether cover URL `url` is a provider's ([`provider_cover_id`]); such covers are not cached since they
/// change once the item is downloaded. Allocation-free (Android calls it per cover via `@FastNative`).
pub fn is_provider_cover(url: &str) -> bool {
    url.match_indices("&id=").any(|(at, mark)| provider_cover_id(&url[at + mark.len()..]))
}

/// What the cover loader (nori-covers) fetches through and keys covers by: the platform's transport,
/// which may come after the loader opens (Android opens it on the main thread, before its HTTP client
/// exists), and the servers' second addresses, so a cover cached through one address is found through
/// the other.
#[derive(Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct CoverNet {
    transport: std::sync::Mutex<Option<std::sync::Arc<dyn crate::transport::Transport>>>,
    transport_set: std::sync::Condvar,
    /// (alt host, primary host) pairs.
    alike: std::sync::RwLock<Vec<(String, String)>>,
}

/// How long a cover fetch waits for [`CoverNet::set_transport`] at startup.
const TRANSPORT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg_attr(feature = "ffi", uniffi::export)]
impl CoverNet {
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new() -> std::sync::Arc<CoverNet> {
        std::sync::Arc::default()
    }

    /// Sets the transport covers are fetched through.
    pub fn set_transport(&self, transport: std::sync::Arc<dyn crate::transport::Transport>) {
        *self.transport.lock().unwrap_or_else(|e| e.into_inner()) = Some(transport);
        self.transport_set.notify_all();
    }
}

impl CoverNet {
    /// Fetching through `transport` from the start.
    pub fn over(transport: std::sync::Arc<dyn crate::transport::Transport>) -> std::sync::Arc<CoverNet> {
        let net = CoverNet::new();
        net.set_transport(transport);
        net
    }

    /// Registers `alt` as another address of `primary` for [`CoverNet::key_parts`].
    pub fn alike(&self, primary: &str, alt: &str) {
        let (p, a) = (host_of(primary), host_of(alt));
        if a.is_empty() || p.is_empty() || a == p {
            return;
        }
        let mut alike = self.alike.write().unwrap_or_else(|e| e.into_inner());
        alike.retain(|(x, _)| x != a);
        alike.push((a.to_string(), p.to_string()));
    }

    /// Feeds a cover URL's cache key to `part` without allocating: the primary host, the path and every
    /// param except [`SIGNATURE`], so the key survives new tokens, passwords and the other address.
    pub fn key_parts(&self, url: &str, mut part: impl FnMut(&[u8])) {
        let (head, query) = url.split_once('?').unwrap_or((url, ""));
        let (base, path) = match head.find("/rest/") {
            Some(i) => (&head[..i], &head[i..]),
            None => (head, ""),
        };
        let host = host_of(base);
        {
            let alike = self.alike.read().unwrap_or_else(|e| e.into_inner());
            part(alike.iter().find(|(a, _)| a == host).map_or(host, |(_, p)| p.as_str()).as_bytes());
        }
        part(path.as_bytes());
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let name = pair.split_once('=').map_or(pair, |(k, _)| k);
            if !SIGNATURE.contains(&name) {
                part(b"&");
                part(pair.as_bytes());
            }
        }
    }

    /// The transport, waiting up to [`TRANSPORT_WAIT`] for [`CoverNet::set_transport`].
    fn transport(&self) -> Result<std::sync::Arc<dyn crate::transport::Transport>, crate::transport::TransportError> {
        let set = self.transport.lock().unwrap_or_else(|e| e.into_inner());
        let (set, _) = self.transport_set.wait_timeout_while(set, TRANSPORT_WAIT, |t| t.is_none()).unwrap_or_else(|e| e.into_inner());
        set.clone().ok_or_else(|| crate::transport::TransportError::Failed { kind: crate::transport::FailureKind::Other, detail: Some("no transport for covers".into()) })
    }
}

#[async_trait::async_trait]
impl crate::transport::Transport for CoverNet {
    async fn get(&self, url: String, timeout_ms: u32) -> Result<crate::transport::TransportResponse, crate::transport::TransportError> {
        self.transport()?.get(url, timeout_ms).await
    }

    async fn send(&self, request: crate::transport::Exchange) -> Result<crate::transport::TransportResponse, crate::transport::TransportError> {
        self.transport()?.send(request).await
    }

    fn address_changed(&self) {}

    fn network(&self) -> crate::transport::Network {
        self.transport().map_or(crate::transport::Network::Unmetered, |t| t.network())
    }
}

/// Writes cover `id`'s URL at `size` into `out` (cleared): `prefix` (`Core::url_prefix`), then the id
/// escaped like Android's `Uri.encode` (not `api::encode`, which differs on `!'()*`), then the size.
/// Twin of `Library.coverUrl` (Library.kt).
pub fn cover_url_into(out: &mut String, prefix: &str, id: &str, size: i32) {
    use std::fmt::Write;
    out.clear();
    out.push_str(prefix);
    out.push_str("&id=");
    uri_encode(out, id);
    out.push_str("&size=");
    let _ = write!(out, "{size}");
}

/// Android's `Uri.encode`: keeps alphanumerics and `_-!.~'()*`, percent-encodes other UTF-8 bytes.
fn uri_encode(out: &mut String, s: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "_-!.~'()*".contains(c) {
            out.push(c);
        } else {
            let mut utf8 = [0u8; 4];
            for &b in c.encode_utf8(&mut utf8).as_bytes() {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 15) as usize] as char);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_keys() {
        let core = crate::Core::new(String::new(), "t".into(), Default::default()).unwrap();
        core.configure(crate::ServerConfig { url: "http://m".into(), user: "u".into(), password: "p".into(), ..Default::default() }).unwrap();
        let urls = core.download_cover_urls(["al 1", "ext-2", "al 1"].map(String::from).to_vec());
        let prefix = core.url_prefix("getCoverArt".into());
        assert_eq!(urls, [format!("{prefix}&id=al%201&size=320"), format!("{prefix}&id=al%201&size=800")]);
        assert_eq!(core.cover_address("al 1".into(), 320), urls[0]);

        // Cover key ignores signature and address.
        let core = crate::Core::new(String::new(), "t".into(), Default::default()).unwrap();
        core.configure(crate::ServerConfig { url: "https://keys.example".into(), user: "u".into(), password: "one".into(), ..Default::default() }).unwrap();
        let before = core.cover_address("pl-6b2d_65f0".into(), 320);
        core.configure(crate::ServerConfig { url: "https://keys.example".into(), user: "u".into(), password: "two".into(), ..Default::default() }).unwrap();
        let after = core.cover_address("pl-6b2d_65f0".into(), 320);
        assert_ne!(before, after);
        assert_eq!(key(&before), key(&after));
        assert_eq!(key(&before), b"keys.example/rest/getCoverArt&id=pl-6b2d_65f0&size=320");
        assert_ne!(key(&before), key(&core.cover_address("pl-6b2d_65f0".into(), 800)));
        assert_ne!(key(&before), key(&core.cover_address("al-1".into(), 320)));
        let net = CoverNet::default();
        let key = |url: &str| {
            let mut out = Vec::new();
            net.key_parts(url, |p| out.extend_from_slice(p));
            out
        };
        net.alike("http://keys.lan:4533/", "https://keys.example");
        assert_eq!(key("http://keys.lan:4533/rest/getCoverArt?u=a&t=x&s=y&id=al-1&size=320"), key("https://keys.example/rest/getCoverArt?u=b&id=al-1&size=320"));
        assert_ne!(key("https://other.example/rest/getCoverArt?id=al-1&size=320"), key("https://keys.example/rest/getCoverArt?id=al-1&size=320"));
    }

    #[test]
    fn every_view_size_shares_one_of_the_two_renditions() {
        for px in [1, 80, 264, ROW] {
            assert_eq!(cover_rendition(px), ROW, "{px}");
        }
        for px in [ROW + 1, 440, 544, FULL, 2400] {
            assert_eq!(cover_rendition(px), FULL, "{px}");
        }
    }

    #[test]
    fn cover_wants_dedup_and_cap() {
        let arts = ["a", "ext-1", "b", "a", "pl-deezer-2", "c"].map(String::from).to_vec();
        let w = cover_wants(arts.clone(), 500);
        assert_eq!(w.iter().map(|w| (w.id.as_str(), w.size)).collect::<Vec<_>>(), [("a", 320), ("a", 800), ("b", 320), ("b", 800), ("c", 320), ("c", 800)]);
        assert_eq!(cover_wants(arts, 2).len(), 4);
    }

    #[test]
    fn provider_cover_detection() {
        for (url, provider) in [
            ("https://m.example/rest/getCoverArt.view?u=a&t=b&s=c&id=ext-deezer-song-1&size=320", true),
            ("https://m.example/rest/getCoverArt.view?u=a&id=pl-deezer-12&size=800", true),
            // Navidrome's own playlists.
            ("https://m.example/rest/getCoverArt.view?u=a&id=pl-6b2d0c1e-5f7a-4e21-9d3c-0a1b2c3d4e5f_65f0a1b2&size=800", false),
            ("https://m.example/rest/getCoverArt.view?u=a&id=pl-abcdefab-5f7a_0&size=800", false),
            ("&id=pl-12", false),
            ("https://m.example/rest/getCoverArt.view?u=a&id=al-3&size=320", false),
            ("https://m.example/rest/getCoverArt.view?id=ext-1", false),
            ("https://m.example/ext-1?xid=ext-2", false),
            ("&id=pl-", false),
            ("&id=pl-qobuz-7&size=320", true),
            ("&id=p", false),
            ("", false),
        ] {
            assert_eq!(is_provider_cover(url), provider, "{url}");
        }
    }

    fn key(url: &str) -> Vec<u8> {
        let mut out = Vec::new();
        CoverNet::default().key_parts(url, |p| out.extend_from_slice(p));
        out
    }

    #[test]
    fn near_covers_wanted() {
        assert_eq!(cover_neighbours(5, 4, 6, 3, 100), [4, 6, 7, 3, 8, 2]);
        // Shuffle: skip targets anywhere, steps still from the current song.
        assert_eq!(cover_neighbours(5, 40, 12, 2, 100), [40, 12, 7, 3]);
        assert_eq!(cover_neighbours(5, 4, 6, 1, 100), [4, 6]);
        assert_eq!(cover_neighbours(5, 4, 6, 0, 100), [4]);
        assert_eq!(cover_neighbours(0, -1, 1, 3, 3), [1, 2]);
        assert_eq!(cover_neighbours(1, 0, 0, 2, 2), [0]);

        // Around lists near and wants.
        let arts: Vec<Option<String>> = ["a", "b", "c", "ext-d", "b"].iter().map(|s| Some(s.to_string())).chain([None]).collect();
        let r = around(arts.len() as u32, |i| arts[i as usize].clone(), 1, 0, 2, 3);
        assert_eq!(r.near, ["b", "a", "c"]);
        assert_eq!(r.wants.iter().map(|w| (w.id.as_str(), w.size)).collect::<Vec<_>>(), [("a", 320), ("a", 800), ("c", 320), ("c", 800), ("b", 320), ("b", 800)]);
        assert_eq!(around(0, |_| unreachable!(), -1, -1, -1, 2), CoversAround { near: vec![], wants: vec![] });
    }

}
