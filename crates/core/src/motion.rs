//! Moving covers: an album's motion artwork in Apple Music's catalogue, a short looping video of the
//! cover, found for the player's sleeve (Settings, Look: off by default, under "Look things up online").
//! The requests go through the client's transport, the answers are kept in the response cache, and the
//! platform only plays the video it is handed ([`Client::motion_video`]).
//!
//! The way there is three steps. The public iTunes Search API finds the album's catalogue id by artist
//! and title ([motion_album_candidates]). Apple's catalogue API, which its own web player reads with a
//! developer token it hands every visitor ([motion_bundle_paths], [motion_token]), answers the album
//! with its `editorialVideo` ([motion_square_video]). None of these shapes is documented by Apple for
//! this use: they are what the search API and the web player were seen to send in 2026. A change on
//! their side comes out here as no candidates, no token or a parse error - never as a wrong video.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cache_policy::{Page, Read};
use crate::client::Client;
use crate::transport::Exchange;
use crate::{CoreError, Song};

type Result<T> = std::result::Result<T, CoreError>;

fn unreadable(what: &str) -> CoreError {
    CoreError::Parse { reason: format!("motion artwork: {what}") }
}

/// One accented Latin letter to its plain one, for the letters a tag is most often typed without.
/// Lowercase only: [norm] lowers first.
fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        c => c,
    }
}

/// Lowercase words and nothing else: accents folded, apostrophes dropped inside a word (so "Don't"
/// and "Dont" agree), `&` read as "and", and every other run of punctuation one space.
fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for c in s.chars().flat_map(char::to_lowercase).map(fold) {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{02bc}' | '`' | '\u{b4}') {
            continue;
        }
        if c == '&' {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str("and");
            gap = true;
        } else if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// A title without what editions add to it: anything in brackets, and a trailing " - Single",
/// " - EP", " - Deluxe Edition" and the like, which is how the iTunes store names them.
fn base(s: &str) -> String {
    let mut plain = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            c if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let mut title = plain.as_str();
    while let Some((head, tail)) = title.rsplit_once(" - ") {
        let tail = norm(tail);
        let edition = tail == "single"
            || tail == "ep"
            || ["deluxe", "edition", "remaster", "remastered", "version", "expanded", "anniversary", "bonus"]
                .iter()
                .any(|w| tail.split(' ').any(|t| t == *w));
        if !edition {
            break;
        }
        title = head;
    }
    norm(title)
}

/// Everyone credited, one name each: "A feat. B", "A & B", "A, B", "A x B".
fn credited(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let mut parts = vec![lower.as_str()];
    for sep in [",", ";", "/", " feat. ", " feat ", " ft. ", " featuring ", " & ", " and ", " x ", " \u{d7} ", " with "] {
        parts = parts.iter().flat_map(|p| p.split(sep)).collect();
    }
    parts.into_iter().map(norm).filter(|p| !p.is_empty()).collect()
}

/// Same artist: the same name, or the first one credited on either side is credited on the other.
/// Returns how sure: 2 for the same name, 1 for a shared credit, 0 for someone else.
fn artist_match(want: &str, hit: &str) -> u8 {
    let (w, h) = (norm(want), norm(hit));
    if w.is_empty() || h.is_empty() {
        return 0;
    }
    if w == h {
        return 2;
    }
    let (wc, hc) = (credited(want), credited(hit));
    let shared = wc.first().is_some_and(|f| hc.contains(f)) || hc.first().is_some_and(|f| wc.contains(f));
    u8::from(shared)
}

/// The catalogue ids worth asking about, best first, from an iTunes Search API answer
/// (`/search?entity=album`). A hit must be by the same artist and carry the same title; one whose
/// title only agrees once edition words are taken off ("Better" against "Better - Single", an album
/// against its deluxe edition) must also have about as many songs, or the same year when the count is
/// not known, because a single or a reissue can have a different cover and so a different video - and
/// a video that is not this cover is worse than none. What is left is ranked by track count and year.
///
/// An answer that is not the search API's JSON is an error, so that the caller does not remember
/// "no motion artwork" for what was really a failed request.
pub fn motion_album_candidates(json: String, artist: String, album: String, tracks: u32, year: u32) -> Result<Vec<String>> {
    let v: Value = serde_json::from_str(&json).map_err(|e| unreadable(&e.to_string()))?;
    let results = v.get("results").and_then(Value::as_array).ok_or_else(|| unreadable("no results list"))?;
    let (want_exact, want_base) = (norm(&album), base(&album));
    let mut ranked: Vec<(i32, usize, String)> = Vec::new();
    for (order, r) in results.iter().enumerate() {
        if r.get("wrapperType").and_then(Value::as_str).is_some_and(|w| w != "collection") {
            continue;
        }
        let id = match r.get("collectionId") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => s.clone(),
            _ => continue,
        };
        let name = r.get("collectionName").and_then(Value::as_str).unwrap_or_default();
        let by = r.get("artistName").and_then(Value::as_str).unwrap_or_default();
        let who = artist_match(&artist, by);
        if who == 0 {
            continue;
        }
        let hit_tracks = r.get("trackCount").and_then(Value::as_u64).unwrap_or(0) as i64;
        let hit_year = r
            .get("releaseDate")
            .and_then(Value::as_str)
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse::<i64>().ok())
            .unwrap_or(0);
        let counted = tracks > 0 && hit_tracks > 0;
        let dated = year > 0 && hit_year > 0;
        let track_gap = (hit_tracks - tracks as i64).abs();
        let year_gap = (hit_year - year as i64).abs();
        let exact = !want_exact.is_empty() && norm(name) == want_exact;
        if !exact {
            let same_base = !want_base.is_empty() && base(name) == want_base;
            let backed = if counted { track_gap <= 2 } else { dated && year_gap <= 1 };
            if !same_base || !backed {
                continue;
            }
        }
        let mut score = (if exact { 30 } else { 18 }) + (if who == 2 { 10 } else { 5 });
        if counted {
            score += match track_gap {
                0 => 12,
                1 | 2 => 5,
                _ => -8,
            };
        }
        if dated {
            score += match year_gap {
                0 => 6,
                1 => 3,
                _ => 0,
            };
        }
        if !ranked.iter().any(|(_, _, seen)| *seen == id) {
            ranked.push((score, order, id));
        }
    }
    // Highest score first; among equals, the order the search itself gave.
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    Ok(ranked.into_iter().take(3).map(|(_, _, id)| id).collect())
}

/// The square motion artwork's HLS playlist from a catalogue album answer
/// (`/v1/catalog/{storefront}/albums/{id}?extend=editorialVideo`), or None when the album has none.
///
/// Square, and only square. The still cover is square and the sleeve shows it cropped at the sides, so
/// the square video lines up with it pixel for pixel and the fade between them shows no shift. The
/// tall one (3:4) is nearly the sleeve's own shape, but it is a different framing of the artwork, not
/// the square with more round it, so it cannot be laid over the still cover.
pub fn motion_square_video(json: String) -> Result<Option<String>> {
    let v: Value = serde_json::from_str(&json).map_err(|e| unreadable(&e.to_string()))?;
    let album = v.get("data").and_then(Value::as_array).and_then(|d| d.first()).ok_or_else(|| unreadable("no album in the answer"))?;
    let Some(video) = album.get("attributes").and_then(|a| a.get("editorialVideo")) else {
        return Ok(None);
    };
    let url = ["motionDetailSquare", "motionSquareVideo1x1"]
        .iter()
        .filter_map(|k| video.get(*k)?.get("video")?.as_str())
        .find(|u| u.starts_with("https://"));
    Ok(url.map(str::to_string))
}

/// The scripts on a page of Apple's web player that may carry its token: `/assets/index…js`, in the
/// order the page names them, each once.
pub fn motion_bundle_paths(html: String) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = html.as_str();
    while let Some(at) = rest.find("/assets/") {
        rest = &rest[at..];
        let end = rest.find(|c: char| c == '"' || c == '\'' || c == '<' || c == '>' || c == '(' || c == ')' || c.is_whitespace()).unwrap_or(rest.len());
        let path = &rest[..end];
        // Straight under /assets/: "/assets/vendor.js/assets/index.js" is two paths run together.
        let file = &path["/assets/".len()..];
        if !file.contains('/') && file.starts_with("index") && file.ends_with(".js") && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
        // Past the "/assets/" just looked at, whatever came of it.
        rest = &rest["/assets/".len()..];
    }
    out
}

/// base64url (or plain base64), padding optional.
fn b64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The developer token in one of the web player's scripts: a JWT whose payload names the web player
/// as its issuer (`"iss":"AMPWebPlay"`), or failing that any readable JWT, and in either case one that
/// has not expired by [now_s] (seconds since 1970) plus a minute. The scripts carry other JWTs, and
/// those are refused by the catalogue, which is why the issuer decides.
pub fn motion_token(js: Vec<u8>, now_s: i64) -> Option<String> {
    let mut fallback: Option<String> = None;
    let mut i = 0;
    while i + 3 <= js.len() {
        if &js[i..i + 3] != b"eyJ" {
            i += 1;
            continue;
        }
        let mut end = i;
        let mut dots = 0;
        while end < js.len() && (js[end].is_ascii_alphanumeric() || matches!(js[end], b'-' | b'_' | b'.')) {
            dots += usize::from(js[end] == b'.');
            end += 1;
        }
        // Every byte taken above is ASCII.
        let token = std::str::from_utf8(&js[i..end]).unwrap_or_default();
        i = end.max(i + 1);
        let parts: Vec<&str> = token.split('.').collect();
        if dots != 2 || token.len() < 80 || parts.iter().any(|p| p.is_empty()) {
            continue;
        }
        let read = |p: &str| b64(p).and_then(|b| serde_json::from_slice::<Value>(&b).ok()).filter(Value::is_object);
        let (Some(header), Some(payload)) = (read(parts[0]), read(parts[1])) else { continue };
        if payload.get("exp").and_then(Value::as_i64).is_some_and(|exp| exp <= now_s + 60) {
            continue;
        }
        let web = payload.get("iss").and_then(Value::as_str) == Some("AMPWebPlay")
            || header.get("kid").and_then(Value::as_str).is_some_and(|k| k.contains("WebPlay"));
        if web {
            return Some(token.to_string());
        }
        fallback.get_or_insert_with(|| token.to_string());
    }
    fallback
}

// ---- playing one ----------------------------------------------------------------------------------------

/// How much of a moving cover its player holds, as media3's `DefaultLoadControl` takes it: min and max
/// buffer (ms), buffer for playback, buffer after a rebuffer, and a byte cap. Left to media3's defaults,
/// a looping video is buffered 50 s ahead - several passes of the loop, at up to 1080 px, in the Java
/// heap - which was the largest part of the app's memory while the player was open. The loop comes out
/// of the moving-cover cache on disk, so a few seconds ahead is plenty and costs no network.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn motion_load_control() -> Vec<i64> {
    vec![4_000, 8_000, 1_000, 2_000, 4 * 1024 * 1024]
}

// ---- finding one ----------------------------------------------------------------------------------------

/// One storefront for all of it, the largest. Catalogue ids and their motion artwork are found the same
/// way from any country, and a device's own region is not always a store Apple has.
const STORE: &str = "us";
/// What the web player's token is kept under, so a restart does not look for it again.
const TOKEN_KEY: &str = "motiontoken|";
/// "No moving cover" is asked again after a week: Apple adds them to old albums.
const NONE_KEPT_MS: i64 = 7 * 24 * 3_600_000;
/// Finding the token reads a script of megabytes; after a failure it is not tried again for this long.
const TOKEN_RETRY: Duration = Duration::from_secs(30 * 60);
/// Each request may take this long.
const REQUEST_MS: u32 = 10_000;

/// What finding one came to. A failed request is never remembered as "none".
enum Found {
    Video(String),
    None,
    Failed,
}

/// The web player's token once found, when finding it last failed, and which cache entry each video
/// came from (so a video that has gone can be forgotten). Each client keeps its own.
#[derive(Default)]
pub(crate) struct Motion {
    token: Option<String>,
    token_failed: Option<Instant>,
    keys: Vec<(String, String)>,
}
/// How many videos are remembered for forgetting.
const KEYS_KEPT: usize = 32;

impl Motion {
    fn keep(&mut self, url: String, key: String) {
        self.keys.retain(|(u, _)| *u != url);
        if self.keys.len() >= KEYS_KEPT {
            self.keys.remove(0);
        }
        self.keys.push((url, key));
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// The square moving cover of `song`'s album, an HLS address, or none: switched off, not an album the
    /// library knows, none found, not asked (`metered` while it is kept to Wi-Fi), or a request that
    /// failed. Remembered like the lyrics: a video is kept, "none" is asked again after a week, and a
    /// failure is not remembered at all. The platform asks only while the player is open, once per album.
    pub async fn motion_video(&self, song: Song, metered: bool) -> Option<String> {
        let (on, wifi_only) = crate::settings_store::with_prefs(|p| (p.motion_artwork && p.third_party_lookups, p.motion_artwork_wifi_only)).unwrap_or((false, true));
        if !on || (wifi_only && metered) {
            return None;
        }
        self.motion_lookup(&song).await
    }

    /// `url` would not play because it is not there any more: the album is looked up again next time.
    pub fn motion_forget(&self, url: String) {
        let key = {
            let mut m = self.motion.lock();
            let at = m.keys.iter().position(|(u, _)| *u == url);
            at.map(|i| m.keys.remove(i).1)
        };
        if let Some(key) = key {
            let _ = self.core.cache_evict(key);
        }
    }
}

impl Client {
    async fn motion_lookup(&self, song: &Song) -> Option<String> {
        // Not for a provider's album: asking the server about an ext- id is octo-fiesta's business.
        let album_id = song.album_id.clone().filter(|id| !song.is_external && !crate::db::external(id));
        // Closed with a bar, so that forgetting album 12 cannot take album 123 with it (eviction is by
        // prefix). Looked at before anything is asked, the server included: the next song of an album
        // costs nothing.
        let key = format!("motion1|{}|", album_id.clone().unwrap_or_else(|| format!("{}|{}", song.artist, song.album)));
        if let Some(stored) = self.core.cache_get(key.clone()).ok().flatten() {
            if !stored.is_empty() {
                let url = String::from_utf8_lossy(&stored).into_owned();
                self.motion.lock().keep(url.clone(), key);
                return Some(url);
            }
            if self.core.cache_fresh(key.clone(), NONE_KEPT_MS).unwrap_or(false) {
                return None;
            }
        }
        // The server's own album, for its artist, song count and year, which tell a record from its single.
        let album = match &album_id {
            Some(id) => match self.first(Read::AlbumById { id: id.clone() }).await {
                Ok(Page::AlbumPage { v }) => Some(v.album),
                _ => None,
            },
            None => None,
        };
        let artist = album.as_ref().map(|a| a.artist.clone()).filter(|a| !a.trim().is_empty()).unwrap_or_else(|| song.artist.clone());
        let name = album.as_ref().map(|a| a.name.clone()).filter(|a| !a.trim().is_empty()).unwrap_or_else(|| song.album.clone());
        if artist.trim().is_empty() || name.trim().is_empty() {
            return None;
        }
        let tracks = album.as_ref().map_or(0, |a| a.song_count);
        let year = album.as_ref().map_or(song.year, |a| a.year);
        match self.motion_find(&artist, &name, tracks, year).await {
            Found::Video(url) => {
                let _ = self.core.cache_put(key.clone(), url.clone().into_bytes());
                self.motion.lock().keep(url.clone(), key);
                Some(url)
            }
            Found::None => {
                let _ = self.core.cache_put(key, Vec::new());
                None
            }
            Found::Failed => None,
        }
    }

    async fn motion_find(&self, artist: &str, album: &str, tracks: u32, year: u32) -> Found {
        let mut term = String::new();
        crate::lrclib::form_encode(&mut term, &format!("{artist} {album}"));
        // The iTunes Search API as publicly documented (term, media, entity, country, limit).
        let url = format!("https://itunes.apple.com/search?term={term}&media=music&entity=album&limit=10&country={STORE}");
        let Some((200, search)) = self.motion_get(&url, &[]).await else { return Found::Failed };
        let Ok(ids) = motion_album_candidates(search, artist.to_string(), album.to_string(), tracks, year) else { return Found::Failed };
        if ids.is_empty() {
            return Found::None;
        }
        let Some(mut token) = self.web_token(false).await else { return Found::Failed };
        // Two at most: a close edition is worth one more question, a long list of guesses is not.
        for id in ids.iter().take(2) {
            let Some(mut answer) = self.motion_catalogue(id, &token).await else { return Found::Failed };
            if matches!(answer.0, 401 | 403) {
                // Refused: the token has expired or been replaced. Find the current one and ask once more.
                self.motion.lock().token = None;
                let _ = self.core.cache_evict(TOKEN_KEY.into());
                let Some(fresh) = self.web_token(true).await else { return Found::Failed };
                token = fresh;
                let Some(again) = self.motion_catalogue(id, &token).await else { return Found::Failed };
                if matches!(again.0, 401 | 403) {
                    self.motion.lock().token_failed = Some(Instant::now());
                    return Found::Failed;
                }
                answer = again;
            }
            match answer.0 {
                200 => match motion_square_video(answer.1) {
                    Ok(Some(url)) => return Found::Video(url),
                    Ok(None) => {}
                    Err(_) => return Found::Failed,
                },
                // Not in this storefront: the next candidate, if there is one.
                404 => {}
                _ => return Found::Failed,
            }
        }
        Found::None
    }

    /// The catalogue's album, with the one extension that carries the video.
    async fn motion_catalogue(&self, id: &str, token: &str) -> Option<(u16, String)> {
        let url = format!("https://amp-api.music.apple.com/v1/catalog/{STORE}/albums/{id}?extend=editorialVideo");
        let bearer = format!("Bearer {token}");
        self.motion_get(&url, &[("Authorization", &bearer), ("Origin", "https://music.apple.com")]).await
    }

    /// The web player's developer token: remembered, or found again in the scripts of one of its pages
    /// when `fresh` (the remembered one was refused). A failed search is not repeated for half an hour.
    async fn web_token(&self, fresh: bool) -> Option<String> {
        if !fresh {
            if let Some(t) = self.motion.lock().token.clone() {
                return Some(t);
            }
            if let Some(t) = self.core.cache_get(TOKEN_KEY.into()).ok().flatten().filter(|t| !t.is_empty()) {
                let t = String::from_utf8_lossy(&t).into_owned();
                self.motion.lock().token = Some(t.clone());
                return Some(t);
            }
        }
        if self.motion.lock().token_failed.is_some_and(|at| at.elapsed() < TOKEN_RETRY) {
            return None;
        }
        let found = self.motion_scrape().await;
        let mut m = self.motion.lock();
        match &found {
            Some(t) => {
                m.token = Some(t.clone());
                drop(m);
                let _ = self.core.cache_put(TOKEN_KEY.into(), t.clone().into_bytes());
            }
            None => {
                m.token_failed = Some(Instant::now());
                crate::alog::info("motion artwork: no token in the web player's scripts");
            }
        }
        found
    }

    /// The page and script layout of Apple's web player as it was in 2026.
    async fn motion_scrape(&self) -> Option<String> {
        let (200, page) = self.motion_get(&format!("https://music.apple.com/{STORE}/browse"), &[]).await? else { return None };
        for path in motion_bundle_paths(page).into_iter().take(3) {
            let Ok(r) = self.transport.get(format!("https://music.apple.com{path}"), REQUEST_MS * 3).await else { continue };
            if r.status != 200 {
                continue;
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            if let Some(t) = motion_token(r.body, now) {
                return Some(t);
            }
        }
        None
    }

    /// One request and its answer as text; none when it got no answer at all.
    async fn motion_get(&self, url: &str, headers: &[(&str, &str)]) -> Option<(u16, String)> {
        let request = Exchange { url: url.to_string(), headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), json: None, timeout_ms: REQUEST_MS };
        match self.transport.send(request).await {
            Ok(r) => Some((r.status, String::from_utf8_lossy(&r.body).into_owned())),
            Err(e) => {
                crate::alog::info(&format!("motion artwork: {e}"));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moving_cover_is_buffered_seconds_ahead_not_a_minute() {
        let c = motion_load_control();
        assert!(c[0] <= c[1] && c[1] <= 10_000, "a few seconds of a loop that comes off the disk");
        assert!(c[2] <= c[0] && c[3] <= c[0]);
        assert!(c[4] <= 8 * 1024 * 1024, "a few MB of the Java heap at most");
    }

    fn enc(bytes: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
            for k in 0..=chunk.len() {
                out.push(A[(n >> (18 - 6 * k) & 63) as usize] as char);
            }
        }
        out
    }

    fn jwt(header: &str, payload: &str) -> String {
        format!("{}.{}.{}", enc(header.as_bytes()), enc(payload.as_bytes()), "s".repeat(43))
    }

    fn hit(id: u64, name: &str, artist: &str, tracks: u32, date: &str) -> String {
        format!(r#"{{"wrapperType":"collection","collectionType":"Album","collectionId":{id},"collectionName":"{name}","artistName":"{artist}","trackCount":{tracks},"releaseDate":"{date}"}}"#)
    }

    fn search(hits: &[String]) -> String {
        format!(r#"{{"resultCount":{},"results":[{}]}}"#, hits.len(), hits.join(","))
    }

    #[test]
    fn words_are_compared_without_their_dress() {
        assert_eq!(norm("Don't Stop Me Now"), "dont stop me now");
        assert_eq!(norm("Simon & Garfunkel"), "simon and garfunkel");
        assert_eq!(norm("Beyoncé"), "beyonce");
        assert_eq!(norm("Łąki Łan"), "laki lan");
        assert_eq!(norm("  AM  (Deluxe)"), "am deluxe");
        assert_eq!(base("Abbey Road (Remastered 2019)"), "abbey road");
        assert_eq!(base("Better - Single"), "better");
        assert_eq!(base("Rumours - Deluxe Edition"), "rumours");
        // A dash that is part of the title stays.
        assert_eq!(base("Songs - For the Deaf"), "songs for the deaf");
    }

    #[test]
    fn the_album_itself_comes_before_its_single_and_its_deluxe_edition() {
        let json = search(&[
            hit(1, "Better - Single", "Khalid", 1, "2018-09-14T07:00:00Z"),
            hit(2, "Better (Deluxe)", "Khalid", 14, "2019-04-01T07:00:00Z"),
            hit(3, "Better", "Khalid", 12, "2019-04-01T07:00:00Z"),
            hit(4, "Better", "Someone Else", 12, "2019-04-01T07:00:00Z"),
        ]);
        let ids = motion_album_candidates(json, "Khalid".into(), "Better".into(), 12, 2019).unwrap();
        assert_eq!(ids, vec!["3".to_string(), "2".to_string()]);
    }

    #[test]
    fn a_title_that_only_agrees_without_edition_words_needs_the_songs_to_agree_too() {
        let json = search(&[hit(9, "Better - Single", "Khalid", 1, "2018-09-14T07:00:00Z")]);
        // An album of twelve is not this single, whatever the words say.
        assert!(motion_album_candidates(json.clone(), "Khalid".into(), "Better".into(), 12, 2019).unwrap().is_empty());
        // A single of one is.
        assert_eq!(motion_album_candidates(json.clone(), "Khalid".into(), "Better".into(), 1, 2018).unwrap(), vec!["9".to_string()]);
        // With no count to go by, the year has to agree.
        assert_eq!(motion_album_candidates(json.clone(), "Khalid".into(), "Better".into(), 0, 2018).unwrap(), vec!["9".to_string()]);
        assert!(motion_album_candidates(json, "Khalid".into(), "Better".into(), 0, 0).unwrap().is_empty());
    }

    #[test]
    fn someone_else_with_the_same_title_is_never_a_match() {
        let json = search(&[hit(5, "Greatest Hits", "Queen", 17, "1981-10-26T08:00:00Z")]);
        assert!(motion_album_candidates(json, "ABBA".into(), "Greatest Hits".into(), 17, 1981).unwrap().is_empty());
    }

    #[test]
    fn a_shared_credit_is_the_same_artist() {
        let json = search(&[hit(6, "Watch the Throne", "JAY-Z & Kanye West", 12, "2011-08-08T07:00:00Z")]);
        let ids = motion_album_candidates(json, "Jay-Z".into(), "Watch The Throne".into(), 12, 2011).unwrap();
        assert_eq!(ids, vec!["6".to_string()]);
    }

    #[test]
    fn a_failed_request_is_not_an_empty_answer() {
        assert!(motion_album_candidates("<html>Too many requests</html>".into(), "a".into(), "b".into(), 0, 0).is_err());
        assert!(motion_album_candidates(r#"{"errorMessage":"Invalid value(s) for key(s): [country]"}"#.into(), "a".into(), "b".into(), 0, 0).is_err());
        assert!(motion_album_candidates(r#"{"resultCount":0,"results":[]}"#.into(), "a".into(), "b".into(), 0, 0).unwrap().is_empty());
    }

    #[test]
    fn the_square_video_and_nothing_else() {
        let found = r#"{"data":[{"id":"1","type":"albums","attributes":{"name":"X","editorialVideo":{
            "motionDetailTall":{"video":"https://mvod.itunes.apple.com/tall.m3u8"},
            "motionDetailSquare":{"video":"https://mvod.itunes.apple.com/square.m3u8"}}}}]}"#;
        assert_eq!(motion_square_video(found.into()).unwrap().as_deref(), Some("https://mvod.itunes.apple.com/square.m3u8"));
        let other_name = r#"{"data":[{"attributes":{"editorialVideo":{"motionSquareVideo1x1":{"video":"https://a/b.m3u8"}}}}]}"#;
        assert_eq!(motion_square_video(other_name.into()).unwrap().as_deref(), Some("https://a/b.m3u8"));
        let tall_only = r#"{"data":[{"attributes":{"editorialVideo":{"motionDetailTall":{"video":"https://a/t.m3u8"}}}}]}"#;
        assert_eq!(motion_square_video(tall_only.into()).unwrap(), None);
        let none = r#"{"data":[{"attributes":{"name":"Plain"}}]}"#;
        assert_eq!(motion_square_video(none.into()).unwrap(), None);
        // A refusal is not "this album has none".
        assert!(motion_square_video(r#"{"errors":[{"status":"401"}]}"#.into()).is_err());
        assert!(motion_square_video("".into()).is_err());
    }

    #[test]
    fn the_scripts_that_may_hold_the_token() {
        let html = r#"<link rel="modulepreload" href="/assets/index~8f2a1c.js"><script type="module" crossorigin src="/assets/index~8f2a1c.js"></script>
            <script nomodule src="/assets/index-legacy~77aa.js"></script><img src="/assets/logo.svg"> /assets/vendor~1.js/assets/index~z.js"#;
        assert_eq!(motion_bundle_paths(html.into()), vec!["/assets/index~8f2a1c.js", "/assets/index-legacy~77aa.js", "/assets/index~z.js"]);
        // A non-ASCII space after a path is only a separator.
        assert_eq!(motion_bundle_paths("x /assets/index~a.js\u{a0}y".into()), vec!["/assets/index~a.js"]);
    }

    #[test]
    fn the_web_players_own_token_is_the_one_taken() {
        let now = 1_790_000_000;
        let other = jwt(r#"{"alg":"ES256","kid":"OTHER"}"#, &format!(r#"{{"iss":"SomethingElse","exp":{}}}"#, now + 90_000));
        let web = jwt(r#"{"alg":"ES256","typ":"JWT","kid":"WebPlayKid"}"#, &format!(r#"{{"iss":"AMPWebPlay","iat":{now},"exp":{}}}"#, now + 90_000));
        let js = format!(r#"const a="{other}";function f(){{return"{web}"}}"#);
        assert_eq!(motion_token(js.into_bytes(), now).as_deref(), Some(web.as_str()));
        // Without the web player's own, any readable one is better than none.
        let js = format!(r#"x="{other}""#);
        assert_eq!(motion_token(js.into_bytes(), now).as_deref(), Some(other.as_str()));
    }

    #[test]
    fn an_expired_token_or_a_lookalike_is_not_taken() {
        let now = 1_790_000_000;
        let stale = jwt(r#"{"kid":"WebPlayKid"}"#, &format!(r#"{{"iss":"AMPWebPlay","exp":{}}}"#, now + 30));
        assert_eq!(motion_token(stale.into_bytes(), now), None);
        assert_eq!(motion_token(b"eyJshort.abc.def".to_vec(), now), None);
        assert_eq!(motion_token(format!("eyJ{}", "a".repeat(200)).into_bytes(), now), None);
        // Three segments of the right length that are not JSON.
        assert_eq!(motion_token(format!("eyJ{}.{}.{}", "a".repeat(40), "b".repeat(40), "c".repeat(40)).into_bytes(), now), None);
    }

    #[test]
    fn a_moving_cover_is_found_kept_and_forgotten() {
        use crate::client::tests::{block, client};
        let (c, fake) = client(crate::client::NetProfile { url: "h".into(), ..Default::default() });
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let web = jwt(r#"{"alg":"ES256","kid":"WebPlayKid"}"#, &format!(r#"{{"iss":"AMPWebPlay","exp":{}}}"#, now + 90_000));
        fake.answer(r#"{"subsonic-response":{"status":"ok","album":{"id":"al-1","name":"Better","artist":"Khalid","songCount":12,"year":2019,"song":[]}}}"#);
        fake.answer(&search(&[hit(3, "Better", "Khalid", 12, "2019-04-01T07:00:00Z")]));
        fake.answer(r#"<script type="module" src="/assets/index~1.js"></script>"#);
        fake.answer(&format!(r#"x="{web}""#));
        fake.answer(r#"{"data":[{"attributes":{"editorialVideo":{"motionDetailSquare":{"video":"https://mvod.itunes.apple.com/square.m3u8"}}}}]}"#);
        let s = Song { id: "s".into(), album_id: Some("al-1".into()), artist: "Khalid".into(), album: "Better".into(), ..Default::default() };
        let url = "https://mvod.itunes.apple.com/square.m3u8";
        assert_eq!(block(c.motion_lookup(&s)).as_deref(), Some(url));
        assert_eq!(fake.asked().len(), 5);
        let catalogue = fake.sent.lock().iter().find(|e| e.url.contains("amp-api.music.apple.com/v1/catalog/us/albums/3")).cloned().unwrap();
        assert_eq!(catalogue.headers["Authorization"], format!("Bearer {web}"));
        // Kept: the next song of the album asks nothing.
        assert_eq!(block(c.motion_lookup(&s)).as_deref(), Some(url));
        assert_eq!(fake.asked().len(), 5);
        // Gone from Apple's side: forgotten, and asked for again (here nothing answers: none, not kept).
        c.motion_forget(url.into());
        assert_eq!(block(c.motion_lookup(&s)), None);
        assert!(fake.asked().len() > 5);
        assert!(c.core.cache_get("motion1|al-1|".into()).unwrap().is_none(), "a failure is not remembered as none");
    }
}
