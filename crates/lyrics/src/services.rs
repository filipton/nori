//! The lyrics services: a few requests each through the core's `Transport`, sending only the song's
//! artist, title, album and length, and the answer read by formats.rs, json.rs or html.rs. Every answer
//! that names a song is matched on title, artist and length, so a service that changes shows up as a
//! miss or a failure, never as another song's words. Which to ask and whose answer wins is race.rs's.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use futures_util::lock::Mutex as AsyncMutex;
use futures_util::stream::{FuturesUnordered, StreamExt};
use nori_model::{alog, Lyrics, Song};
use nori_net::transport::{Exchange, Transport};
use nori_settings::lyrics_sources::{LyricsLookup, LyricsService};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::fit::norm;
use crate::lrclib::{self, clean, form_encode, DURATION_SLACK_S};
use crate::trust::{latin, Named};
use crate::{formats, html, json as answers, lyrics};

/// A service's answer. `Failed` (unreachable, refused, unknown shape, too slow) is never kept as a miss.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// The words, and what the service said about the song it found them for.
    Found(Lyrics, Named),
    Missing,
    Failed,
}

/// Longest one request may take...
const REQUEST_MS: u32 = 6_000;
/// ...except PaxSenix's keyed routes, which ask Spotify or Musixmatch before answering.
const PAXSENIX_REQUEST_MS: u32 = 15_000;
/// A request is not started with less than this left of the service's time.
const LEAST_MS: u64 = 250;

/// How long one service may take over one song, all its requests together.
pub(crate) fn deadline_ms(service: LyricsService) -> u64 {
    match service {
        LyricsService::PaxsenixSpotify | LyricsService::PaxsenixMusixmatch => 2 * PAXSENIX_REQUEST_MS as u64,
        _ => 12_000,
    }
}

/// Why a request brought no answer to read.
#[derive(Debug)]
enum Fail {
    Status(u16),
    Unreachable,
    /// The service's time was up.
    Late,
    /// An answer of a shape this code does not know.
    Shape(String),
}

type Asked<T> = Result<T, Fail>;

fn shape(what: &str) -> Fail {
    Fail::Shape(what.to_string())
}

/// `v` form-encoded for a query string.
fn enc(v: &str) -> String {
    let mut out = String::with_capacity(v.len() * 3);
    form_encode(&mut out, v);
    out
}

/// `title_key=<cleaned title>&artist_key=<artist>`, then the album and length under their keys when the
/// song has them and the service takes them.
fn song_query(song: &Song, title_key: &str, artist_key: &str, album_key: Option<&str>, duration_key: Option<&str>) -> String {
    let mut q = format!("{title_key}={}&{artist_key}={}", enc(&clean(&song.title)), enc(&song.artist));
    if let Some(k) = album_key.filter(|_| !song.album.trim().is_empty()) {
        q.push_str(&format!("&{k}={}", enc(&song.album)));
    }
    if let Some(k) = duration_key.filter(|_| song.duration > 0) {
        q.push_str(&format!("&{k}={}", song.duration));
    }
    q
}

/// What the services of one lookup share: the song's YouTube video, found once, and the client's memory.
#[derive(Default)]
pub struct Shared {
    youtube: AsyncMutex<Option<Option<String>>>,
    pub(crate) memory: LyricsMemory,
}

impl Shared {
    pub fn over(memory: &LyricsMemory) -> Shared {
        Shared { youtube: AsyncMutex::new(None), memory: memory.clone() }
    }
}

/// What one client's lookups remember of each other, in memory only; a clone shares it.
#[derive(Clone, Default)]
pub struct LyricsMemory {
    /// Services that failed lately (race.rs).
    pub(crate) failures: Arc<Mutex<crate::race::Failures>>,
    /// Songs already matched on YouTube Music: (artist, title, length) key and video id.
    youtube: Arc<Mutex<Vec<YoutubeMatch>>>,
    /// The LyricsPlus server that answered last, asked alone first.
    lyrics_plus_host: Arc<Mutex<Option<&'static str>>>,
}

/// A song key (artist, title, length) and its YouTube video, if any.
type YoutubeMatch = (String, Option<String>);

/// One service asking about one song: the transport, the keys, and the time it has left.
pub struct Ask<'a> {
    transport: &'a dyn Transport,
    lookup: &'a LyricsLookup,
    shared: &'a Shared,
    started: Instant,
    budget_ms: u64,
}

impl<'a> Ask<'a> {
    pub fn new(transport: &'a dyn Transport, lookup: &'a LyricsLookup, shared: &'a Shared, service: LyricsService) -> Self {
        Ask { transport, lookup, shared, started: Instant::now(), budget_ms: deadline_ms(service) }
    }

    /// One request with whatever status came: a JSON body is POSTed, and it takes no longer than
    /// `request_ms` or the service's time left.
    async fn fetch(&self, url: &str, headers: &[(&str, &str)], json: Option<String>, request_ms: u32) -> Asked<(u16, String)> {
        let left = self.budget_ms.saturating_sub(self.started.elapsed().as_millis() as u64);
        if left < LEAST_MS {
            return Err(Fail::Late);
        }
        let request = Exchange {
            url: url.to_string(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            json,
            timeout_ms: (request_ms as u64).min(left) as u32,
        };
        let r = self.transport.send(request).await.map_err(|_| Fail::Unreachable)?;
        Ok((r.status, String::from_utf8_lossy(&r.body).into_owned()))
    }

    /// [`Ask::fetch`] with an error status as a failure, so "404, no such song" is told from "503".
    async fn send(&self, url: &str, headers: &[(&str, &str)], json: Option<String>, request_ms: u32) -> Asked<String> {
        let (status, body) = self.fetch(url, headers, json, request_ms).await?;
        if !(200..300).contains(&status) {
            return Err(Fail::Status(status));
        }
        Ok(body)
    }

    async fn get(&self, url: &str, headers: &[(&str, &str)]) -> Asked<String> {
        self.send(url, headers, None, REQUEST_MS).await
    }

    async fn get_json(&self, url: &str, headers: &[(&str, &str)]) -> Asked<Value> {
        parse(&self.get(url, headers).await?)
    }
}

fn parse(body: &str) -> Asked<Value> {
    serde_json::from_str(body.trim_start_matches('\u{feff}')).map_err(|e| Fail::Shape(e.to_string()))
}

/// A lookup with its error sorted: a 404 is a miss, anything else a failure (logged).
fn settle(service: LyricsService, r: Asked<Lookup>) -> Lookup {
    match r {
        Ok(l) => l,
        Err(Fail::Status(404)) => Lookup::Missing,
        Err(e) => {
            let why = match e {
                Fail::Status(status) => format!("HTTP {status}"),
                Fail::Unreachable => "unreachable".into(),
                Fail::Late => "too slow".into(),
                Fail::Shape(what) => what,
            };
            alog::info(&format!("{} lyrics failed: {why}", service.name()));
            Lookup::Failed
        }
    }
}

/// Found, or a miss when there are no lines.
fn found(l: Lyrics, named: Named) -> Lookup {
    if l.lines.is_empty() {
        Lookup::Missing
    } else {
        Lookup::Found(l, named)
    }
}

// ---- reading answers and matching them -----------------------------------------------------------------

/// A field as text: a string, or a number written out; empty when missing.
pub(crate) fn str_of<'v>(o: &'v Value, k: &str) -> std::borrow::Cow<'v, str> {
    match o.get(k) {
        Some(Value::String(v)) => std::borrow::Cow::Borrowed(v),
        Some(Value::Number(n)) => std::borrow::Cow::Owned(n.to_string()),
        _ => std::borrow::Cow::Borrowed(""),
    }
}

/// A string field with text in it: none when missing, blank or "null".
pub(crate) fn text<'v>(o: &'v Value, k: &str) -> Option<&'v str> {
    o.get(k).and_then(Value::as_str).filter(|t| !t.trim().is_empty() && *t != "null")
}

/// A number field, as a number or a numeric string; 0 when missing.
pub(crate) fn num(o: &Value, k: &str) -> f64 {
    match o.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(v)) => v.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// A flag field, as a bool or "true".
pub(crate) fn truthy(o: &Value, k: &str) -> bool {
    match o.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(v)) => v.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn list<'v>(o: &'v Value, k: &str) -> Option<&'v Vec<Value>> {
    o.get(k).and_then(Value::as_array)
}

/// The `name` of every object in the array `k`.
fn names(o: &Value, k: &str) -> Vec<String> {
    list(o, k).map(|a| a.iter().filter_map(|x| x.get("name")?.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// How far a reported length (seconds, or milliseconds when that large) is from the song's; none when
/// either is unknown.
fn off(reported: f64, song: &Song) -> Option<f64> {
    (reported > 0.0 && song.duration > 0).then(|| (Named::seconds(reported) - song.duration as f64).abs())
}

/// Whether a reported length is this song's within [`DURATION_SLACK_S`]; unknown passes.
fn same_length(reported: f64, song: &Song) -> bool {
    off(reported, song).is_none_or(|d| d <= DURATION_SLACK_S)
}

/// Candidates' sort key: the closest length first, unknown lengths last.
fn distance(reported: f64, song: &Song) -> f64 {
    off(reported, song).unwrap_or(f64::MAX)
}

/// Whether two names are the same once case and punctuation are gone, one containing the other
/// ("Creep" and "Creep (Acoustic)": the length check tells those apart).
fn alike(a: &str, b: &str) -> bool {
    let (x, y) = (norm(a), norm(b));
    !x.is_empty() && !y.is_empty() && (x.contains(&y) || y.contains(&x))
}

/// Whether a search result is this song: length within the slack, title (against the cleaned `title`)
/// and artist alike where given. An artist in another script than the song's cannot be compared.
fn fits(song: &Song, title: &str, name: &str, artist: &str, length: f64) -> bool {
    same_length(length, song)
        && (name.trim().is_empty() || alike(name, title))
        && (artist.trim().is_empty() || alike(artist, &song.artist) || !latin(artist) || !latin(&song.artist))
}

/// A length written as a clock ("5:54.320", "3:05"), in seconds; 0 when it is not one.
fn clock_seconds(v: &str) -> f64 {
    let mut total = 0.0;
    for part in v.trim().split(':') {
        let Ok(n) = part.trim().parse::<f64>() else { return 0.0 };
        total = total * 60.0 + n;
    }
    total
}

const TITLE_KEYS: [&str; 5] = ["title", "song", "trackName", "track_name", "name"];
const ARTIST_KEYS: [&str; 4] = ["artist", "artistName", "artist_name", "singer"];
const ALBUM_KEYS: [&str; 4] = ["album", "albumName", "album_name", "collectionName"];

fn first_text<'v>(meta: &'v Value, keys: &[&str]) -> Option<&'v str> {
    keys.iter().find_map(|k| text(meta, k))
}

/// The length an answer reports under the usual keys, seconds or ms; 0 when none.
fn reported_length(meta: &Value) -> f64 {
    ["duration", "durationMs", "totalDuration", "length"]
        .iter()
        .map(|k| match meta.get(*k) {
            Some(Value::String(v)) if v.contains(':') => clock_seconds(v),
            _ => num(meta, k),
        })
        .find(|d| *d > 0.0)
        .unwrap_or(0.0)
}

/// What an answer names under the usual keys.
fn named_in(meta: &Value) -> Named {
    let first = |keys: &[&str]| first_text(meta, keys).unwrap_or_default();
    Named::new(first(&TITLE_KEYS), first(&ARTIST_KEYS), first(&ALBUM_KEYS), Named::seconds(reported_length(meta)))
}

/// Whether the song an answer names is this one. Loose services answer with the nearest song, often the
/// same artist's other one, so the title must match; a title in another script passes on the artist.
/// An answer naming nothing passes.
fn names_this(meta: &Value, song: &Song) -> bool {
    if !same_length(reported_length(meta), song) {
        return false;
    }
    let named = first_text(meta, &TITLE_KEYS);
    let title_fits = named.is_none_or(|t| alike(t, &clean(&song.title)) || alike(t, &song.title));
    let artist_fits = first_text(meta, &ARTIST_KEYS[..3]).is_none_or(|a| alike(a, &song.artist));
    title_fits || (artist_fits && !(latin(named.unwrap_or_default()) && latin(&song.title)))
}

/// Each once, in the order first seen.
fn distinct<T: Clone + Eq + std::hash::Hash>(v: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut seen = HashSet::new();
    v.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

// ---- the services ---------------------------------------------------------------------------------------

/// Asks `service` about `song`.
pub async fn ask(service: LyricsService, a: &Ask<'_>, song: &Song) -> Lookup {
    let keys = a.lookup;
    let r = match service {
        LyricsService::Lrclib => return lrclib(a, song).await,
        LyricsService::Binilyrics => bini_lyrics(a, song).await,
        LyricsService::BetterLyrics => better_lyrics(a, song, "/getLyrics", &keys.better_lyrics_key).await,
        LyricsService::Portato => better_lyrics(a, song, "/qq/getLyrics", &keys.better_lyrics_key).await,
        LyricsService::Paxsenix => paxsenix(a, song).await,
        LyricsService::PaxsenixSpotify => paxsenix_spotify(a, song, &keys.paxsenix_key).await,
        LyricsService::PaxsenixMusixmatch => paxsenix_musixmatch(a, song, &keys.paxsenix_key).await,
        LyricsService::LyricsPlus => return lyrics_plus(a, song).await,
        LyricsService::Simpmusic => simpmusic(a, song).await,
        LyricsService::Unison => unison(a, song).await,
        LyricsService::Netease => netease(a, song).await,
        LyricsService::Kugou => kugou(a, song).await,
        LyricsService::YoutubeCaptions => youtube_captions(a, song).await,
        LyricsService::Megalobiz => megalobiz(a, song).await,
        LyricsService::YoutubeMusic => youtube_music(a, song).await,
        LyricsService::Genius => genius(a, song).await,
    };
    settle(service, r)
}

/// LRCLIB: the exact lookup first (LRCLIB matches the length itself), whose synced hit is the answer;
/// otherwise a search, ranked by timing and then by closeness of length.
async fn lrclib(a: &Ask<'_>, song: &Song) -> Lookup {
    let title = clean(&song.title);
    // "Not found" is a 404 with a JSON body: an answer, not a failure.
    let exact = match a.fetch(&lrclib::get_url(song, &title), &[], None, REQUEST_MS).await.and_then(|(_, b)| parse(&b)) {
        Ok(o) if o.is_object() => o,
        Ok(_) => return settle(LyricsService::Lrclib, Err(shape("not an object"))),
        Err(e) => return settle(LyricsService::Lrclib, Err(e)),
    };
    let exact_hit = if exact.get("statusCode").is_some() { None } else { lrclib::pick(&exact).map(|l| (l, named_in(&exact))) };
    if let Some((hit, n)) = exact_hit.as_ref().filter(|h| h.0.synced) {
        return Lookup::Found(hit.clone(), n.clone());
    }
    let hits = match a.fetch(&lrclib::search_url(song, &title), &[], None, REQUEST_MS).await.and_then(|(_, b)| parse(&b)) {
        Ok(Value::Array(v)) => v,
        other => {
            if let Some((hit, n)) = exact_hit {
                return Lookup::Found(hit, n);
            }
            return settle(LyricsService::Lrclib, other.and(Err(shape("not an array"))));
        }
    };
    let gap = |o: &Value| (num(o, "duration") - song.duration as f64).abs();
    let best = hits
        .iter()
        .filter(|o| o.is_object() && (song.duration == 0 || gap(o) <= DURATION_SLACK_S))
        .filter_map(|o| lrclib::pick(o).map(|l| (l, gap(o), named_in(o))))
        .min_by(|x, y| formats::timing(&y.0).cmp(&formats::timing(&x.0)).then(x.1.total_cmp(&y.1)))
        .map(|(l, _, n)| (l, n));
    match (best, exact_hit) {
        (Some((b, n)), None) => Lookup::Found(b, n),
        (Some((b, n)), Some(_)) if b.synced => Lookup::Found(b, n),
        (_, Some((e, n))) => Lookup::Found(e, n),
        (None, None) => Lookup::Missing,
    }
}

/// Unison: an open database (ODbL) of listener-timed lyrics; one request, matched on length by Unison.
/// An entry is TTML (word by word), LRC or plain text.
async fn unison(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let url = format!("https://unison.boidu.dev/lyrics?{}", song_query(song, "song", "artist", Some("album"), Some("duration")));
    let o = a.get_json(&url, &[]).await?;
    if o.get("success").is_none() {
        return Err(shape("no success flag"));
    }
    let Some(d) = o.get("data").filter(|d| d.is_object()) else { return Ok(Lookup::Missing) };
    let Some(words) = text(d, "lyrics") else { return Ok(Lookup::Missing) };
    if !truthy(&o, "success") || !same_length(num(d, "duration"), song) || !names_this(d, song) {
        return Ok(Lookup::Missing);
    }
    let l = if str_of(d, "format").eq_ignore_ascii_case("ttml") { formats::from_ttml(words) } else { lyrics::from_lrc(words) };
    Ok(found(l, named_in(d)))
}

/// Some NetEase endpoints refuse a request without its own Referer.
const NETEASE: [(&str, &str); 1] = [("Referer", "https://music.163.com/")];

/// NetEase Cloud Music (unofficial web endpoints, no key): a search, then the words (YRC word by word, or
/// LRC) of the two closest matches. A refusal outside China is a 200 with its own code: a failure.
async fn netease(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let title = clean(&song.title);
    let search = format!("https://music.163.com/api/search/get?s={}&type=1&limit=10&offset=0", enc(&format!("{title} {}", song.artist)));
    let o = a.get_json(&search, &NETEASE).await?;
    let code = num(&o, "code");
    if code != 0.0 && code != 200.0 {
        return Err(Fail::Shape(format!("code {code}")));
    }
    let Some(songs) = o.get("result").and_then(|r| list(r, "songs")) else { return Ok(Lookup::Missing) };
    let mut hits: Vec<&Value> = songs.iter().filter(|x| fits(song, &title, &str_of(x, "name"), &names(x, "artists").join(", "), num(x, "duration"))).collect();
    hits.sort_by(|x, y| distance(num(x, "duration"), song).total_cmp(&distance(num(y, "duration"), song)));
    for hit in hits.iter().filter(|x| num(x, "id") as i64 > 0).take(2) {
        let url = format!("https://music.163.com/api/song/lyric/v1?id={}&cp=false&lv=0&kv=0&tv=0&rv=0&yv=0&ytv=0&yrv=0", num(hit, "id") as i64);
        let l = a.get_json(&url, &NETEASE).await?;
        if truthy(&l, "pureMusic") || truthy(&l, "nolyric") {
            continue;
        }
        let part = |k: &str| l.get(k).and_then(|p| text(p, "lyric")).unwrap_or_default().to_string();
        let words = formats::from_netease(&part("yrc"), &part("lrc"), &song.title);
        if !words.lines.is_empty() {
            let album = hit.get("album").map(|a| str_of(a, "name").into_owned()).unwrap_or_default();
            let artist = names(hit, "artists").into_iter().next().unwrap_or_default();
            return Ok(Lookup::Found(words, Named::new(&str_of(hit, "name"), &artist, &album, Named::seconds(num(hit, "duration")))));
        }
    }
    Ok(Lookup::Missing)
}

/// KuGou (unofficial desktop endpoints, no key): a search by artist, title and length, then the closest
/// candidate's KRC. Content that is not KRC is a failure.
async fn kugou(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let title = clean(&song.title);
    let search = format!(
        "https://lyrics.kugou.com/search?ver=1&man=yes&client=pc&keyword={}&duration={}",
        enc(&format!("{} - {title}", song.artist)),
        song.duration as u64 * 1000
    );
    let status = |v: &Value| match num(v, "status") {
        0.0 | 200.0 => Ok(()),
        s => Err(Fail::Shape(format!("status {s}"))),
    };
    let o = a.get_json(&search, &[]).await?;
    status(&o)?;
    let Some(candidates) = list(&o, "candidates") else { return Ok(Lookup::Missing) };
    // Its singers are mostly in Chinese, so an artist in another script does not pass here.
    let singer_fits = |x: &Value| str_of(x, "singer").trim().is_empty() || alike(&str_of(x, "singer"), &song.artist);
    let best = candidates
        .iter()
        .filter(|x| fits(song, &title, &str_of(x, "song"), &str_of(x, "singer"), num(x, "duration")) && singer_fits(x))
        .min_by(|x, y| distance(num(x, "duration"), song).total_cmp(&distance(num(y, "duration"), song)));
    let Some(best) = best else { return Ok(Lookup::Missing) };
    let (id, key) = (str_of(best, "id"), str_of(best, "accesskey"));
    if id.is_empty() || key.is_empty() {
        return Ok(Lookup::Missing);
    }
    let url = format!("https://lyrics.kugou.com/download?ver=1&client=pc&fmt=krc&charset=utf8&id={}&accesskey={}", enc(&id), enc(&key));
    let file = a.get_json(&url, &[]).await?;
    status(&file)?;
    let Some(content) = text(&file, "content") else { return Ok(Lookup::Missing) };
    let named = Named::new(&str_of(best, "song"), &str_of(best, "singer"), "", Named::seconds(num(best, "duration")));
    Ok(found(formats::from_krc(content, &song.title).map_err(Fail::Shape)?, named))
}

/// BiniLyrics: Apple Music's syllable-timed TTML on a volunteer's site. A search, then the document of
/// the first result that fits the song.
async fn bini_lyrics(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let title = clean(&song.title);
    let url = format!("https://lyrics-api.binimum.org/?{}", song_query(song, "track", "artist", Some("album"), Some("duration")));
    let o = a.get_json(&url, &[]).await?;
    // A miss is a 404 or an empty list; no list is an unknown shape.
    let results = list(&o, "results").ok_or_else(|| shape("no results"))?;
    let link = results
        .iter()
        .filter(|x| fits(song, &title, &str_of(x, "track_name"), &str_of(x, "artist_name"), num(x, "duration")))
        .find_map(|x| text(x, "lyricsUrl").filter(|u| u.starts_with("https://")).map(|u| (u, x)));
    let Some((link, hit)) = link else { return Ok(Lookup::Missing) };
    Ok(found(formats::from_ttml(&a.get(link, &[]).await?), named_in(hit)))
}

/// BetterLyrics' documented host, then the one its extension used; the second only when the first
/// cannot be reached.
const BETTER_LYRICS_HOSTS: [&str; 2] = ["https://api.betterlyrics.org", "https://lyrics-api.boidu.dev"];

/// BetterLyrics: Apple Music's TTML (`/getLyrics`) or QQ Music's QRC (`/qq/getLyrics`, "Portato").
/// A song not stored yet needs a key; without one its 401 is a miss (race.rs keys the cache on the key).
async fn better_lyrics(a: &Ask<'_>, song: &Song, path: &str, key: &str) -> Asked<Lookup> {
    let query = song_query(song, "s", "a", Some("al"), Some("d"));
    let headers: Vec<(&str, &str)> = if key.is_empty() { Vec::new() } else { vec![("X-API-Key", key)] };
    let mut answer = Err(Fail::Unreachable);
    for host in BETTER_LYRICS_HOSTS {
        answer = a.get(&format!("{host}{path}?{query}"), &headers).await;
        if !matches!(answer, Err(Fail::Unreachable)) {
            break;
        }
    }
    match answer {
        Err(Fail::Status(401)) if key.is_empty() => Ok(Lookup::Missing),
        other => Ok(found(answers::from_provider(&other?, &song.title), Named::default())),
    }
}

/// PaxSenix without a key: Apple Music's lyrics by the song's Apple id, found with the iTunes Search API.
async fn paxsenix(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let title = clean(&song.title);
    let search = format!("https://itunes.apple.com/search?term={}&media=music&entity=song&limit=10&country=us", enc(&format!("{} {title}", song.artist)));
    let o = a.get_json(&search, &[]).await?;
    let results = list(&o, "results").ok_or_else(|| shape("no results"))?;
    let mut hits: Vec<&Value> = results.iter().filter(|x| fits(song, &title, &str_of(x, "trackName"), &str_of(x, "artistName"), num(x, "trackTimeMillis"))).collect();
    hits.sort_by(|x, y| distance(num(x, "trackTimeMillis"), song).total_cmp(&distance(num(y, "trackTimeMillis"), song)));
    for id in distinct(hits.iter().map(|x| num(x, "trackId") as i64).filter(|id| *id > 0)).into_iter().take(2) {
        let body = match a.get(&format!("https://lyrics.paxsenix.org/apple-music/lyrics?id={id}&ttml=true"), &[]).await {
            Err(Fail::Status(404)) => continue,
            other => other?,
        };
        let words = answers::from_provider(&body, &song.title);
        if !words.lines.is_empty() {
            let named = hits.iter().find(|x| num(x, "trackId") as i64 == id).map_or_else(Named::default, |x| named_in(x));
            return Ok(Lookup::Found(words, named));
        }
    }
    Ok(Lookup::Missing)
}

const PAXSENIX_API: &str = "https://api.paxsenix.org";

/// PaxSenix's key as it wants it, whether pasted with "Bearer " or not.
fn bearer(key: &str) -> String {
    format!("Bearer {}", key.trim().trim_start_matches("Bearer ").trim())
}

fn paxsenix_headers(auth: &str) -> [(&str, &str); 2] {
    [("Authorization", auth), ("Accept", "application/json")]
}

/// PaxSenix with the user's key: Spotify's lyrics for the Spotify track its search finds.
async fn paxsenix_spotify(a: &Ask<'_>, song: &Song, key: &str) -> Asked<Lookup> {
    if key.is_empty() {
        return Err(shape("no key"));
    }
    let auth = bearer(key);
    let headers = paxsenix_headers(&auth);
    let title = clean(&song.title);
    let search = a.send(&format!("{PAXSENIX_API}/spotify/search?q={}", enc(&format!("{title} {}", song.artist))), &headers, None, PAXSENIX_REQUEST_MS).await?;
    let mut tracks: Vec<answers::FoundTrack> = answers::found_tracks(&search).into_iter().filter(|t| fits(song, &title, &t.title, &t.artist, t.duration_ms as f64)).collect();
    tracks.sort_by(|x, y| distance(x.duration_ms as f64, song).total_cmp(&distance(y.duration_ms as f64, song)));
    for id in distinct(tracks.iter().map(|t| t.id.clone())).into_iter().take(2) {
        let body = a.send(&format!("{PAXSENIX_API}/lyrics/spotify?id={}", enc(&id)), &headers, None, PAXSENIX_REQUEST_MS).await?;
        let words = answers::from_provider(&body, &song.title);
        if !words.lines.is_empty() {
            let named = tracks.iter().find(|t| t.id == id).map_or_else(Named::default, |t| Named::new(&t.title, &t.artist, "", t.duration_ms as f64 / 1000.0));
            return Ok(Lookup::Found(words, named));
        }
    }
    Ok(Lookup::Missing)
}

/// PaxSenix with the user's key: Musixmatch's lyrics by title, artist and length, in one request.
async fn paxsenix_musixmatch(a: &Ask<'_>, song: &Song, key: &str) -> Asked<Lookup> {
    if key.is_empty() {
        return Err(shape("no key"));
    }
    let auth = bearer(key);
    let url = format!("{PAXSENIX_API}/lyrics/musixmatch?{}", song_query(song, "t", "a", None, Some("d")));
    let body = a.send(&url, &paxsenix_headers(&auth), None, PAXSENIX_REQUEST_MS).await?;
    Ok(found(answers::from_provider(&body, &song.title), Named::default()))
}

/// LyricsPlus' volunteer servers, as other clients of it listed them in September 2026.
const LYRICS_PLUS_HOSTS: [&str; 6] = [
    "https://lyricsplus.prjktla.my.id",
    "https://lyricsplus.atomix.one",
    "https://lyricsplus.binimum.org",
    "https://lyricsplus.prjktla.workers.dev",
    "https://lyricsplus-seven.vercel.app",
    "https://lyrics-plus-backend.vercel.app",
];

/// LyricsPlus (YouLy+'s backend): the server that answered last alone, then the others together, the
/// first real answer winning. A miss needs two servers saying so: one server's miss can be its own trouble.
async fn lyrics_plus(a: &Ask<'_>, song: &Song) -> Lookup {
    let query = song_query(song, "title", "artist", Some("album"), Some("duration"));
    let mut misses = 0;
    let first = *a.shared.memory.lyrics_plus_host.lock();
    if let Some(host) = first {
        match lyrics_plus_from(a, host, &query, song).await {
            Lookup::Found(l, n) => return Lookup::Found(l, n),
            Lookup::Missing => misses += 1,
            Lookup::Failed => {}
        }
    }
    let query = query.as_str();
    let mut asking: FuturesUnordered<_> =
        LYRICS_PLUS_HOSTS.into_iter().filter(|h| Some(*h) != first).map(|host| async move { (host, lyrics_plus_from(a, host, query, song).await) }).collect();
    while let Some((host, answer)) = asking.next().await {
        match answer {
            Lookup::Found(l, n) => {
                *a.shared.memory.lyrics_plus_host.lock() = Some(host);
                return Lookup::Found(l, n);
            }
            Lookup::Missing => misses += 1,
            Lookup::Failed => {}
        }
    }
    if misses >= 2 {
        Lookup::Missing
    } else {
        alog::info("LYRICS_PLUS lyrics failed: no server answered");
        Lookup::Failed
    }
}

/// One LyricsPlus server's answer; one naming another song (`metadata`) is a miss.
async fn lyrics_plus_from(a: &Ask<'_>, host: &str, query: &str, song: &Song) -> Lookup {
    let body = match a.get(&format!("{host}/v2/lyrics/get?{query}"), &[]).await {
        // A server that has gone answers with some web page.
        Ok(body) if !body.trim_start().starts_with('{') => return Lookup::Failed,
        Ok(body) => body,
        Err(Fail::Status(404)) => return Lookup::Missing,
        Err(_) => return Lookup::Failed,
    };
    let meta = parse(&body).ok().and_then(|v| v.get("metadata").cloned());
    if meta.as_ref().is_some_and(|m| !names_this(m, song)) {
        alog::info("LYRICS_PLUS answered with another song: a miss");
        return Lookup::Missing;
    }
    found(answers::from_lyricsplus(&body), meta.as_ref().map(named_in).unwrap_or_default())
}

/// YouTube Music's player API as its web player calls it, with the client version open-source clients
/// of 2026 fall back on.
const YOUTUBE_MUSIC: &str = "https://music.youtube.com/youtubei/v1";
const YOUTUBE_MUSIC_VERSION: &str = "1.20250101.01.00";
/// The search filter of YouTube Music's "Songs" chip.
const YOUTUBE_SONGS: &str = "EgWKAQIIAWoKEAkQChAFEAMQBA==";

/// One YouTube Music player API call, without an account.
async fn youtube(a: &Ask<'_>, endpoint: &str, mut body: Value) -> Asked<String> {
    body["context"] = json!({ "client": { "clientName": "WEB_REMIX", "clientVersion": YOUTUBE_MUSIC_VERSION, "hl": "en", "gl": "US" } });
    let headers = [
        ("Origin", "https://music.youtube.com"),
        ("Referer", "https://music.youtube.com/"),
        ("X-YouTube-Client-Name", "67"),
        ("X-YouTube-Client-Version", YOUTUBE_MUSIC_VERSION),
    ];
    a.send(&format!("{YOUTUBE_MUSIC}/{endpoint}?prettyPrint=false"), &headers, Some(body.to_string()), REQUEST_MS).await
}

/// How many songs' YouTube matches [`LyricsMemory`] keeps.
const YOUTUBE_KEPT: usize = 32;

/// This song's YouTube Music video, for SimpMusic, the captions and the lyrics tab: a Songs search, the
/// closest fitting result. Found once per lookup and remembered per client; a failed search is not.
async fn youtube_id(a: &Ask<'_>, song: &Song) -> Asked<Option<String>> {
    let mut mine = a.shared.youtube.lock().await;
    if let Some(known) = mine.as_ref() {
        return Ok(known.clone());
    }
    let key = format!("{}\n{}\n{}", song.artist, song.title, song.duration);
    if let Some((_, id)) = a.shared.memory.youtube.lock().iter().find(|(k, _)| *k == key) {
        *mine = Some(id.clone());
        return Ok(id.clone());
    }
    let title = clean(&song.title);
    let answer = youtube(a, "search", json!({ "query": format!("{} {title}", song.artist), "params": YOUTUBE_SONGS })).await?;
    let id = answers::youtube_songs(&answer)
        .into_iter()
        .filter(|t| fits(song, &title, &t.title, &t.artist, t.duration_ms as f64))
        .min_by(|x, y| distance(x.duration_ms as f64, song).total_cmp(&distance(y.duration_ms as f64, song)))
        .map(|t| t.id);
    let mut kept = a.shared.memory.youtube.lock();
    if kept.len() >= YOUTUBE_KEPT {
        kept.remove(0);
    }
    kept.push((key, id.clone()));
    *mine = Some(id.clone());
    Ok(id)
}

/// SimpMusic's lyrics database, keyed on the YouTube video: of the entries within the length slack, the
/// closest, and its finest timed text (served HTML-escaped).
async fn simpmusic(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let Some(id) = youtube_id(a, song).await? else { return Ok(Lookup::Missing) };
    let o = a.get_json(&format!("https://api-lyrics.simpmusic.org/v1/{}", enc(&id)), &[]).await?;
    let Some(entries) = list(&o, "data").filter(|_| truthy(&o, "success")) else { return Ok(Lookup::Missing) };
    let entry = entries.iter().filter(|e| same_length(num(e, "duration"), song)).min_by(|x, y| distance(num(x, "duration"), song).total_cmp(&distance(num(y, "duration"), song)));
    let Some(entry) = entry else { return Ok(Lookup::Missing) };
    let best = ["richSyncLyrics", "syncedLyrics", "plainLyrics"]
        .iter()
        .filter_map(|k| text(entry, k))
        .map(html::from_escaped_lrc)
        .filter(|l| !l.lines.is_empty())
        .max_by_key(formats::timing);
    Ok(best.map_or(Lookup::Missing, |l| Lookup::Found(l, Named { duration_s: Some(num(entry, "duration")).filter(|d| *d > 0.0), ..Named::default() })))
}

/// Standard padded base64.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for k in 0..4 {
            out.push(if k <= chunk.len() { ABC[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// The captions of the song's YouTube video, a line at a time (often a speech recogniser's).
async fn youtube_captions(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let Some(id) = youtube_id(a, song).await? else { return Ok(Lookup::Missing) };
    // get_transcript takes a one-field protobuf message (field 1: the video id), base64.
    let mut message = vec![0x0a, id.len() as u8];
    message.extend_from_slice(id.as_bytes());
    match youtube(a, "get_transcript", json!({ "params": base64(&message) })).await {
        // A video without a transcript is refused with a 400.
        Err(Fail::Status(400)) => Ok(Lookup::Missing),
        other => Ok(found(answers::from_youtube_captions(&other?), Named::default())),
    }
}

/// The untimed words of YouTube Music's Lyrics tab, found from the `next` call and then browsed.
async fn youtube_music(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let Some(id) = youtube_id(a, song).await? else { return Ok(Lookup::Missing) };
    let next = youtube(a, "next", json!({ "videoId": id, "isAudioOnly": true })).await?;
    let Some(page) = answers::youtube_lyrics_page(&next) else { return Ok(Lookup::Missing) };
    let mut request = json!({ "browseId": page.browse_id });
    if let Some(params) = page.params {
        request["params"] = Value::String(params);
    }
    Ok(found(answers::from_youtube_music(&youtube(a, "browse", request).await?), Named::default()))
}

/// Megalobiz: user-made LRC shown on pages. The first two result pages naming the song; a page gives no
/// length, so its last line must come before the song ends.
async fn megalobiz(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let title = clean(&song.title);
    let search = a.get(&format!("https://www.megalobiz.com/searchall?qry={}", enc(&format!("{} {title}", song.artist))), &[]).await?;
    for link in html::megalobiz_links(&search, &title, &song.artist).into_iter().take(2) {
        let words = html::from_megalobiz(&a.get(&format!("https://www.megalobiz.com{link}"), &[]).await?);
        let Some(last) = words.lines.last().map(|l| l.start_ms) else { continue };
        if song.duration == 0 || last <= song.duration as i64 * 1000 + 4_000 {
            return Ok(Lookup::Found(words, Named::default()));
        }
    }
    Ok(Lookup::Missing)
}

/// Genius: untimed words, asked last. Its site search, the first song result with this title and artist
/// that is not a translation or an instrumental, then that page's words.
async fn genius(a: &Ask<'_>, song: &Song) -> Asked<Lookup> {
    let title = clean(&song.title);
    let o = a.get_json(&format!("https://genius.com/api/search/multi?q={}", enc(&format!("{} {title}", song.artist))), &[]).await?;
    let sections = o.get("response").and_then(|r| list(r, "sections")).ok_or_else(|| shape("no sections"))?;
    let url = sections
        .iter()
        .filter(|x| str_of(x, "type") == "song")
        .flat_map(|x| list(x, "hits").into_iter().flatten())
        .filter_map(|h| h.get("result"))
        .filter(|r| alike(&str_of(r, "title"), &title) && alike(&str_of(r, "artist_names"), &song.artist) && !truthy(r, "instrumental") && !str_of(r, "path").to_lowercase().contains("translation"))
        .find_map(|r| text(r, "url").filter(|u| u.starts_with("https://genius.com/")).map(|u| (u, Named::new(&str_of(r, "title"), &str_of(r, "artist_names"), "", 0.0))));
    let Some((url, named)) = url else { return Ok(Lookup::Missing) };
    Ok(found(html::from_genius(&a.get(url, &[]).await?), named))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use nori_net::transport::{TransportError, TransportResponse};

    /// Answers by address, and remembers every request.
    #[derive(Default)]
    pub(crate) struct Web {
        pub(crate) pages: Mutex<Vec<(String, u16, String)>>,
        pub(crate) sent: Mutex<Vec<Exchange>>,
    }

    impl Web {
        pub(crate) fn answer(&self, starts: &str, status: u16, body: &str) {
            self.pages.lock().push((starts.to_string(), status, body.to_string()));
        }
        pub(crate) fn asked(&self) -> Vec<String> {
            self.sent.lock().iter().map(|e| e.url.clone()).collect()
        }
    }

    #[async_trait::async_trait]
    impl Transport for Web {
        async fn get(&self, url: String, timeout_ms: u32) -> Result<TransportResponse, TransportError> {
            self.send(Exchange { url, timeout_ms, ..Default::default() }).await
        }
        async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
            self.sent.lock().push(request.clone());
            let pages = self.pages.lock();
            match pages.iter().find(|(p, _, _)| request.url.starts_with(p.as_str())) {
                Some((_, status, body)) => Ok(TransportResponse { status: *status, body: body.as_bytes().to_vec() }),
                None => Err(TransportError::Failed { kind: nori_net::transport::FailureKind::Connect, detail: None }),
            }
        }
        fn address_changed(&self) {}
    }

    /// The fake answers at once, so a future here never waits.
    pub(crate) fn block<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    pub(crate) fn song() -> Song {
        Song { id: "1".into(), title: "Glass Harbour (feat. Someone)".into(), artist: "The Lanterns".into(), album: "Low Tide".into(), duration: 239, ..Default::default() }
    }

    fn keys() -> LyricsLookup {
        LyricsLookup { services: Vec::new(), prefer_words: true, paxsenix_key: String::new(), better_lyrics_key: String::new() }
    }

    fn asking(web: &Web, service: LyricsService) -> Lookup {
        let (k, shared) = (keys(), Shared::default());
        let a = Ask::new(web, &k, &shared, service);
        block(ask(service, &a, &song()))
    }

    #[test]
    fn names_match_loosely_and_lengths_closely() {
        assert!(alike("Creep", "Creep (Acoustic)") && alike("AC/DC", "ac dc") && !alike("", "x"));
        assert!(alike("Beyoncé", "BEYONCÉ") && !alike("Paper", "Boats"));
        let s = Song { duration: 200, ..Default::default() };
        assert!(same_length(203.0, &s) && same_length(203_500.0, &s) && !same_length(205.0, &s));
        assert!(same_length(0.0, &s) && same_length(f64::NAN, &s), "unknown passes");
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(bearer("Bearer  k "), "Bearer k");
    }

    #[test]
    fn names_this_rejects_other_songs() {
        let s = song();
        assert!(names_this(&json!({"title": "Glass Harbour", "artist": "The Lanterns", "totalDuration": "3:59.320"}), &s));
        assert!(names_this(&json!({}), &s), "an answer that names nothing passes");
        assert!(!names_this(&json!({"title": "Whisky on the Table", "artist": "The Lanterns"}), &s), "the same artist's other song");
        assert!(!names_this(&json!({"title": "Glass Harbour", "totalDuration": "5:54.320"}), &s), "another cut");
        assert!(names_this(&json!({"title": "ガラスの港", "artist": "The Lanterns"}), &s), "a title in another script: the artist decides");
        assert!(!names_this(&json!({"title": "ガラスの港", "artist": "Someone Else"}), &s));
        assert_eq!(clock_seconds("5:54.320"), 354.32);
    }

    #[test]
    fn lyrics_plus_other_song_is_a_miss() {
        let web = Web::default();
        let body = include_str!("../testdata/lyricsplus.json").replace(r#""source":"Apple","#, r#""source":"Apple","title":"Whisky on the Table","artist":"The Lanterns","#);
        web.answer("https://lyricsplus", 200, &body);
        assert_eq!(asking(&web, LyricsService::LyricsPlus), Lookup::Missing);
        let web = Web::default();
        let body = include_str!("../testdata/lyricsplus.json").replace(r#""source":"Apple","#, r#""source":"Apple","title":"Glass Harbour","artist":"The Lanterns","totalDuration":"3:59.000","#);
        web.answer("https://lyricsplus", 200, &body);
        assert!(matches!(asking(&web, LyricsService::LyricsPlus), Lookup::Found(..)));
    }

    #[test]
    fn bini_lyrics_takes_only_this_artists_song() {
        let ttml = include_str!("../testdata/apple.ttml");
        let result = |artist: &str| json!({"results": [{"track_name": "Glass Harbour", "artist_name": artist, "duration": 239, "lyricsUrl": "https://lyrics-storage.binimum.org/X.ttml"}]}).to_string();
        let web = Web::default();
        web.answer("https://lyrics-api.binimum.org/", 200, &result("Someone Else"));
        web.answer("https://lyrics-storage.binimum.org/X.ttml", 200, ttml);
        assert_eq!(asking(&web, LyricsService::Binilyrics), Lookup::Missing, "the same title by another artist");
        let web = Web::default();
        web.answer("https://lyrics-api.binimum.org/", 200, &result("The Lanterns"));
        web.answer("https://lyrics-storage.binimum.org/X.ttml", 200, ttml);
        let Lookup::Found(l, _) = asking(&web, LyricsService::Binilyrics) else { panic!("found") };
        assert!(l.word_timed);
    }

    #[test]
    fn kugou_skips_other_artist_in_other_script() {
        let web = Web::default();
        let found = json!({"status": 200, "candidates": [{"id": "1", "accesskey": "k", "song": "Glass Harbour", "singer": "周杰伦", "duration": 239_000}]});
        web.answer("https://lyrics.kugou.com/search", 200, &found.to_string());
        assert_eq!(asking(&web, LyricsService::Kugou), Lookup::Missing);
        assert_eq!(web.asked().len(), 1, "no download for another artist");
    }

    #[test]
    fn unison_miss_and_failure() {
        let web = Web::default();
        let ttml = include_str!("../testdata/apple.ttml");
        web.answer("https://unison.boidu.dev/lyrics?song=Glass+Harbour&artist=The+Lanterns&album=Low+Tide&duration=239", 200, &json!({"success": true, "data": {"lyrics": ttml, "format": "ttml", "duration": 239}}).to_string());
        let Lookup::Found(l, _) = asking(&web, LyricsService::Unison) else { panic!("found") };
        assert!(l.word_timed);
        let web = Web::default();
        web.answer("https://unison.boidu.dev/", 200, r#"{"success": false, "data": null}"#);
        assert_eq!(asking(&web, LyricsService::Unison), Lookup::Missing);
        let web = Web::default();
        web.answer("https://unison.boidu.dev/", 200, r#"{"something": "else"}"#);
        assert_eq!(asking(&web, LyricsService::Unison), Lookup::Failed, "a shape not known is a failure");
        let web = Web::default();
        web.answer("https://unison.boidu.dev/", 503, "busy");
        assert_eq!(asking(&web, LyricsService::Unison), Lookup::Failed);
        let web = Web::default();
        web.answer("https://unison.boidu.dev/", 404, "");
        assert_eq!(asking(&web, LyricsService::Unison), Lookup::Missing);
    }

    #[test]
    fn netease_matches_and_sends_referer() {
        let web = Web::default();
        let found = json!({"code": 200, "result": {"songs": [
            {"id": 7, "name": "Glass Harbour (Live)", "duration": 300_000, "artists": [{"name": "The Lanterns"}]},
            {"id": 8, "name": "Glass Harbour", "duration": 239_500, "artists": [{"name": "The Lanterns"}]}]}});
        web.answer("https://music.163.com/api/search/get", 200, &found.to_string());
        let yrc = include_str!("../testdata/netease.yrc");
        web.answer("https://music.163.com/api/song/lyric/v1?id=8", 200, &json!({"yrc": {"lyric": yrc}, "lrc": {"lyric": ""}}).to_string());
        let Lookup::Found(l, _) = asking(&web, LyricsService::Netease) else { panic!("found") };
        assert!(l.word_timed);
        let sent = web.sent.lock();
        assert!(sent.iter().all(|e| e.headers.get("Referer").map(String::as_str) == Some("https://music.163.com/")));
        assert!(!sent.iter().any(|e| e.url.contains("id=7")), "a live take is another length");
        drop(sent);
        let web = Web::default();
        web.answer("https://music.163.com/api/search/get", 200, r#"{"code": -460, "message": "Cheating"}"#);
        assert_eq!(asking(&web, LyricsService::Netease), Lookup::Failed, "a refusal is not a miss");
    }

    #[test]
    fn better_lyrics_host_fallback() {
        let web = Web::default();
        web.answer("https://api.betterlyrics.org/getLyrics", 401, "");
        assert_eq!(asking(&web, LyricsService::BetterLyrics), Lookup::Missing);
        assert_eq!(web.asked().len(), 1, "an answer, even an error, is the service's answer");
        let web = Web::default();
        let ttml = include_str!("../testdata/apple.ttml");
        web.answer("https://lyrics-api.boidu.dev/getLyrics", 200, &json!({"ttml": ttml, "score": 0.9}).to_string());
        let Lookup::Found(l, _) = asking(&web, LyricsService::BetterLyrics) else { panic!("found on the second host") };
        assert!(l.word_timed);
        assert_eq!(web.asked().len(), 2);
    }

    #[test]
    fn lrclib_prefers_word_timed_hit() {
        let web = Web::default();
        web.answer("https://lrclib.net/api/get", 404, r#"{"statusCode":404,"message":"not found"}"#);
        let file = include_str!("../testdata/lrclib.lyricsfile.yaml");
        let hits = json!([
            {"duration": 240, "syncedLyrics": "[00:01.00]by line"},
            {"duration": 238, "syncedLyrics": "[00:01.00]from lrc", "lyricsfile": file},
            {"duration": 300, "syncedLyrics": "[00:01.00]too long"}]);
        web.answer("https://lrclib.net/api/search", 200, &hits.to_string());
        let Lookup::Found(l, _) = asking(&web, LyricsService::Lrclib) else { panic!("found") };
        assert!(l.word_timed);
        assert!(web.asked()[0].contains("track_name=Glass+Harbour&"), "the title cleaned");
        let web = Web::default();
        assert_eq!(asking(&web, LyricsService::Lrclib), Lookup::Failed, "unreachable");
    }

    #[test]
    fn youtube_search_shared() {
        let web = Web::default();
        web.answer("https://music.youtube.com/youtubei/v1/search", 200, include_str!("../testdata/youtube-search.json"));
        web.answer("https://music.youtube.com/youtubei/v1/get_transcript", 400, "");
        web.answer("https://api-lyrics.simpmusic.org/v1/AbCdEfGhIjK", 200, &json!({"success": true, "data": [{"duration": 239, "syncedLyrics": "[00:01.00]It&#x27;s here"}]}).to_string());
        let (k, shared) = (keys(), Shared::default());
        let s = Song { title: "Glass Harbour".into(), ..song() };
        let caption = block(ask(LyricsService::YoutubeCaptions, &Ask::new(&web, &k, &shared, LyricsService::YoutubeCaptions), &s));
        assert_eq!(caption, Lookup::Missing, "no transcript");
        let Lookup::Found(l, _) = block(ask(LyricsService::Simpmusic, &Ask::new(&web, &k, &shared, LyricsService::Simpmusic), &s)) else { panic!("found") };
        assert_eq!(l.lines[0].text, "It's here");
        assert_eq!(web.asked().iter().filter(|u| u.contains("/search")).count(), 1);
        let body: Value = serde_json::from_str(web.sent.lock()[0].json.as_deref().unwrap()).unwrap();
        assert_eq!(body["context"]["client"]["clientName"], "WEB_REMIX");
    }

    #[test]
    fn paxsenix_key_is_required_and_sent_as_bearer() {
        let web = Web::default();
        assert_eq!(asking(&web, LyricsService::PaxsenixMusixmatch), Lookup::Failed);
        assert!(web.asked().is_empty());
        let web = Web::default();
        web.answer("https://api.paxsenix.org/lyrics/musixmatch", 200, include_str!("../testdata/musixmatch-richsync.json"));
        let k = LyricsLookup { paxsenix_key: "abc".into(), ..keys() };
        let shared = Shared::default();
        let got = block(ask(LyricsService::PaxsenixMusixmatch, &Ask::new(&web, &k, &shared, LyricsService::PaxsenixMusixmatch), &song()));
        assert!(matches!(got, Lookup::Found(l, _) if l.word_timed));
        assert_eq!(web.sent.lock()[0].headers["Authorization"], "Bearer abc");
        assert_eq!(web.sent.lock()[0].timeout_ms, PAXSENIX_REQUEST_MS);
    }
}
