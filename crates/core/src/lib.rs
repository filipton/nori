//! nori music core: the `Core` a platform opens per server profile (database, server address, response
//! parsing into the index) and the domain calls over it. Re-exports the domain crates.

pub mod automix;
pub mod transfers;
pub mod queue;
pub mod bridge;
pub mod history;
pub mod m3u;
pub mod mixes;
pub mod profiles;
pub mod smart;
pub mod actions;
pub mod browse;
pub mod covers;
pub mod motion;
pub mod search;
pub mod shown;
pub mod client;
pub mod cache_policy;
pub mod race;
pub mod stream;
pub mod autofill;
pub mod car;
pub mod playlist;
pub mod library;
pub mod stage;
pub mod update;
#[cfg(feature = "neural-beats")]
pub mod beat_download;

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;

pub use nori_db::{self as db, background};
pub use nori_model::{alog, heap, lines, model, CoreError};
pub use nori_automix::beat_model;
pub use nori_devices::{autoeq, outputs};
pub use nori_library::{menus, pages, rows, stars};
pub use nori_lyrics::{formats, look, lrclib, lyrics, services};
pub use nori_net::{api, transport};
pub use nori_perf::perf_log;
pub use nori_queue::{heard, rules, scrobble};
pub use nori_transfers::stream_cache;
pub use nori_settings::{credits, dsp, lyrics_sources, settings, settings_model, settings_store};
pub use nori_settings::settings::parse_eq_preset;
pub use nori_model::model::*;
pub use nori_library::pages::{AlbumDetail, ArtistDetail, PlaylistDetail, Starred};

use nori_model::Result;

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ServerConfig {
    pub url: String,
    pub user: String,
    pub password: String,
    /// OpenSubsonic API key; when set it is used instead of user and password.
    pub api_key: Option<String>,
    pub legacy_auth: bool,
}

impl ServerConfig {
    /// The server these settings sign requests for.
    fn server(&self) -> api::Server {
        let auth = match (&self.api_key, self.legacy_auth) {
            (Some(k), _) if !k.is_empty() => api::Auth::ApiKey(k),
            (_, true) => api::Auth::Legacy { user: &self.user, password: &self.password },
            _ => api::Auth::Token { user: &self.user, password: &self.password },
        };
        api::Server::with(&self.url, auth)
    }
}

/// A write queued while the server was unreachable.
pub(crate) struct PendingCall {
    pub row_id: i64,
    pub endpoint: String,
    pub params: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Param {
    pub key: String,
    pub value: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiError {
    code: i32,
    message: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Found {
    artist: Vec<Artist>,
    album: Vec<Album>,
    song: Vec<Song>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AlbumWire {
    #[serde(flatten)]
    album: Album,
    song: Vec<Song>,
    #[serde(rename = "discTitles")]
    disc_titles: Vec<DiscTitle>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ArtistWire {
    #[serde(flatten)]
    artist: Artist,
    album: Vec<Album>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PlaylistWire {
    #[serde(flatten)]
    playlist: Playlist,
    entry: Vec<Song>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Albums {
    album: Vec<Album>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Songs {
    song: Vec<Song>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Index {
    artist: Vec<Artist>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Artists {
    index: Vec<Index>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Playlists {
    playlist: Vec<Playlist>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Genres {
    genre: Vec<Genre>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Stations {
    #[serde(rename = "internetRadioStation")]
    station: Vec<RadioStation>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ArtistInfoWire {
    biography: Option<String>,
    last_fm_url: Option<String>,
    music_brainz_id: Option<String>,
    large_image_url: Option<String>,
    medium_image_url: Option<String>,
    similar_artist: Vec<Artist>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LyricsList {
    structured_lyrics: Vec<lyrics::Structured>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Folders {
    music_folder: Vec<MusicFolder>,
}

/// getMusicDirectory mixes folders and songs in one `child` list, told apart by `isDir`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct DirectoryWire {
    #[serde(deserialize_with = "crate::model::id_string")]
    id: String,
    name: String,
    child: Vec<serde_json::Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Share {
    url: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Shares {
    share: Vec<Share>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct QueueWire {
    entry: Vec<Song>,
    current: Option<serde_json::Value>,
    position: u64,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Response {
    status: String,
    version: String,
    #[serde(rename = "type")]
    kind: String,
    server_version: String,
    open_subsonic: bool,
    error: Option<ApiError>,
    search_result3: Option<Found>,
    starred2: Option<Found>,
    album: Option<AlbumWire>,
    artist: Option<ArtistWire>,
    playlist: Option<PlaylistWire>,
    album_list2: Option<Albums>,
    artists: Option<Artists>,
    playlists: Option<Playlists>,
    genres: Option<Genres>,
    random_songs: Option<Songs>,
    songs_by_genre: Option<Songs>,
    similar_songs2: Option<Songs>,
    top_songs: Option<Songs>,
    song: Option<Song>,
    internet_radio_stations: Option<Stations>,
    artist_info2: Option<ArtistInfoWire>,
    lyrics_list: Option<LyricsList>,
    play_queue: Option<QueueWire>,
    shares: Option<Shares>,
    music_folders: Option<Folders>,
    indexes: Option<Artists>,
    directory: Option<DirectoryWire>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: Response,
}

fn parse(body: &[u8]) -> Result<Response> {
    let r = serde_json::from_slice::<Envelope>(body).map_err(|e| CoreError::Parse { reason: e.to_string() })?.response;
    if r.status != "ok" {
        let e = r.error.unwrap_or_default();
        return Err(CoreError::Api { code: e.code, reason: if e.message.is_empty() { "request failed".into() } else { e.message } });
    }
    Ok(r)
}

/// The newest core, for nori-engine and Android code that runs without a handle (the measurer).
// Global: reached from JNI/engine callbacks with no core handle.
static ACTIVE: Mutex<std::sync::Weak<Core>> = Mutex::new(std::sync::Weak::new());

/// The newest core, if alive.
pub fn active() -> Option<Arc<Core>> {
    ACTIVE.lock().upgrade()
}

#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct Core {
    /// Also [`nori_db::active`] while this is the newest core.
    db: Arc<Mutex<Connection>>,
    server: RwLock<api::Server>,
    /// The downloads table in memory and the platform's download reports (transfers.rs).
    downloads: Arc<transfers::Downloads>,
    /// The "For you" row (mixes/board.rs).
    board: Mutex<mixes::board::Board>,
    /// This session's star changes, overlaid on the server's favourites.
    stars: Mutex<stars::StarMarks>,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Opens the database at `db_path` (empty: in memory) for server profile `server`, and makes this the
    /// active core.
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new(db_path: String, server: String) -> Result<Arc<Self>> {
        let db = db::open(&db_path, &server)?;
        nori_automix::beat_model::set_home(&db_path);
        let db = Arc::new(Mutex::new(db));
        let core = Arc::new(Core {
            downloads: Arc::new(transfers::Downloads::load(&db)?),
            db,
            server: RwLock::new(api::Server::default()),
            board: Mutex::new(mixes::board::Board::default()),
            stars: Mutex::new(stars::StarMarks::default()),
        });
        *ACTIVE.lock() = Arc::downgrade(&core);
        nori_db::set_active(&core.db);
        core.downloads.activate();
        Ok(core)
    }

    /// Sets the server; returns the normalised base url. Clears the library when the profile now points
    /// at another server or user.
    pub fn configure(&self, config: ServerConfig) -> Result<String> {
        let s = config.server();
        let ident = format!("{}|{}", s.base, config.user);
        let db = self.db.lock();
        if db::kv_get(&db, "server")?.as_deref() != Some(&ident) {
            db::clear_library(&db)?;
            db::kv_put(&db, "server", &ident)?;
        }
        let base = s.base.clone();
        *self.server.write() = s;
        Ok(base)
    }

    pub fn url(&self, endpoint: String, params: Vec<Param>) -> String {
        let p: Vec<(String, String)> = params.into_iter().map(|p| (p.key, p.value)).collect();
        self.server.read().url(&endpoint, &p)
    }

    /// Signed url of `endpoint` without parameters, for callers appending `&id=..` per row.
    pub fn url_prefix(&self, endpoint: String) -> String {
        self.server.read().url(&endpoint, &[])
    }

    /// `max_bit_rate` 0 and empty `format` mean the original file. Transcodes ask for
    /// `estimateContentLength`: without a length the player cannot seek.
    pub fn stream_url(&self, id: String, max_bit_rate: u32, format: String) -> String {
        let transcode = max_bit_rate > 0 || !format.is_empty();
        let mut p = vec![("id".to_string(), id)];
        if max_bit_rate > 0 {
            p.push(("maxBitRate".into(), max_bit_rate.to_string()));
        }
        if !format.is_empty() {
            p.push(("format".into(), format));
        }
        if transcode {
            p.push(("estimateContentLength".into(), "true".to_string()));
        }
        self.server.read().url("stream", &p)
    }

    pub fn cover_url(&self, id: String, size: u32) -> String {
        let mut p = vec![("id".to_string(), id)];
        if size > 0 {
            p.push(("size".into(), size.to_string()));
        }
        self.server.read().url("getCoverArt", &p)
    }

    pub fn local_search(&self, query: String, limit: u32) -> Result<SearchResult> {
        Ok(db::search(&self.db.lock(), &query, limit)?)
    }

    pub fn index_size(&self) -> Result<IngestStats> {
        let c = self.db.lock();
        Ok(IngestStats { artists: db::count(&c, db::ARTIST)?, albums: db::count(&c, db::ALBUM)?, songs: db::count(&c, db::SONG)? })
    }

    pub fn save_queue(&self, queue: PlayQueue) -> Result<()> {
        self.save_queue_with_runs(queue, Vec::new())
    }

    pub fn load_queue(&self) -> Result<PlayQueue> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Q {
            songs: Vec<Song>,
            index: u32,
            position: u64,
            /// Parsed separately so an unknown origin kind loses only the origin.
            origin: serde_json::Value,
            /// Album run per song (`Playlist::album_runs`).
            runs: serde_json::Value,
        }
        let q: Q = db::kv_get(&self.db.lock(), "queue")?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        let index = q.index.min(q.songs.len().saturating_sub(1) as u32);
        crate::queue::queue_register(q.songs.clone());
        let origin = serde_json::from_value::<Option<crate::PageOrigin>>(q.origin).ok().flatten();
        let runs = serde_json::from_value::<Vec<u32>>(q.runs).unwrap_or_default();
        crate::playlist::playlist_put_back_runs(q.songs.iter().map(|s| s.id.clone()).collect(), runs);
        Ok(PlayQueue { songs: q.songs, index, position_ms: q.position, origin })
    }

    /// A sorted, filtered page of indexed songs. `sort`: title, artist, album, year, duration, created,
    /// playCount or userRating; anything else is index order.
    pub fn browse_songs(&self, sort: String, descending: bool, starred_only: bool, year_from: u32, year_to: u32, offset: u32, limit: u32) -> Result<Vec<Song>> {
        let key = match sort.as_str() {
            "title" | "artist" | "album" => format!("json_extract(json, '$.{sort}') COLLATE NOCASE"),
            "year" | "duration" | "created" | "playCount" | "userRating" => format!("json_extract(json, '$.{sort}')"),
            _ => "rowid".to_string(),
        };
        // The song kind written out, so the songs' sort indexes (db.rs) serve the order a page at a time.
        let mut sql = format!("SELECT json FROM items WHERE server=sid() AND kind={}", db::SONG);
        if starred_only {
            sql.push_str(" AND json_extract(json, '$.starred') = 1");
        }
        if year_to > 0 {
            sql.push_str(" AND json_extract(json, '$.year') BETWEEN ?3 AND ?4");
        }
        sql.push_str(&format!(" ORDER BY {key} {} LIMIT ?2 OFFSET ?1", if descending { "DESC" } else { "ASC" }));
        let c = self.db.lock();
        let mut st = c.prepare_cached(&sql)?;
        let map = |r: &rusqlite::Row| r.get::<_, String>(0);
        let rows: Vec<String> = if year_to > 0 {
            st.query_map(params![offset, limit, year_from, year_to], map)?.filter_map(|r| r.ok()).collect()
        } else {
            st.query_map(params![offset, limit], map)?.filter_map(|r| r.ok()).collect()
        };
        Ok(rows.iter().filter_map(|j| serde_json::from_str(j).ok()).collect())
    }

    /// The ids of [`Self::downloads`], same order.
    pub fn download_ids(&self, done: bool) -> Result<Vec<String>> {
        let c = self.db.lock();
        let mut st = c.prepare_cached("SELECT id FROM downloads WHERE server=sid() AND done=?1 ORDER BY ts DESC")?;
        let rows = st.query_map([done], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|id| id.ok()).collect())
    }

    pub fn downloads(&self, done: bool) -> Result<Vec<Song>> {
        let c = self.db.lock();
        let mut st = c.prepare_cached("SELECT json FROM downloads WHERE server=sid() AND done=?1 ORDER BY ts DESC")?;
        let rows = st.query_map([done], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|j| serde_json::from_str(&j.ok()?).ok()).collect())
    }

    pub fn autoeq_search(&self, query: String, limit: u32) -> Result<Vec<AutoEqEntry>> {
        Ok(autoeq::search(&self.db.lock(), &query, limit)?)
    }

    pub fn autoeq_count(&self) -> Result<u32> {
        Ok(autoeq::count(&self.db.lock())?)
    }

    pub fn profiles(&self) -> Result<Vec<SoundProfile>> {
        let c = self.db.lock();
        let mut st = c.prepare_cached("SELECT name, json, outputs FROM profiles ORDER BY name")?;
        let rows = st.query_map([], |r| {
            Ok(SoundProfile {
                name: r.get(0)?,
                json: r.get(1)?,
                outputs: r.get::<_, String>(2)?.lines().filter(|l| !l.is_empty()).map(str::to_string).collect(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn profile_delete(&self, name: String) -> Result<()> {
        self.db.lock().execute("DELETE FROM profiles WHERE name=?1", [name])?;
        Ok(())
    }

    pub fn search_history(&self) -> Result<Vec<String>> {
        let c = self.db.lock();
        let mut st = c.prepare_cached("SELECT query FROM searches WHERE server=sid() ORDER BY ts DESC")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn search_forget(&self) -> Result<()> {
        self.db.lock().execute("DELETE FROM searches WHERE server=sid()", [])?;
        Ok(())
    }
}

impl Core {
    /// [`Core::save_queue`] with each song's album run (dropped unless one per song).
    pub(crate) fn save_queue_with_runs(&self, queue: PlayQueue, runs: Vec<u32>) -> Result<()> {
        let runs = if runs.len() == queue.songs.len() { runs } else { Vec::new() };
        let json = serde_json::json!({ "songs": queue.songs, "index": queue.index, "position": queue.position_ms, "origin": queue.origin, "runs": runs });
        Ok(db::kv_put(&self.db.lock(), "queue", &json.to_string())?)
    }

    /// Points requests at another address of the same server (LAN vs WAN) without touching the index.
    pub(crate) fn use_address(&self, url: String) {
        let next = self.server.read().rebased(&url);
        *self.server.write() = next;
    }

    /// getIndexes: the folder tree's top level.
    pub(crate) fn parse_indexes(&self, body: Vec<u8>) -> Result<Vec<Artist>> {
        Ok(parse(&body)?.indexes.unwrap_or_default().index.into_iter().flat_map(|i| i.artist).collect())
    }

    pub fn parse_directory(&self, body: Vec<u8>) -> Result<Directory> {
        let d = parse(&body)?.directory.unwrap_or_default();
        let mut out = Directory { id: d.id, name: d.name, ..Default::default() };
        for c in d.child {
            if c.get("isDir").and_then(|v| v.as_bool()).unwrap_or(false) {
                if let Ok(mut a) = serde_json::from_value::<Artist>(c.clone()) {
                    if a.name.is_empty() {
                        a.name = c.get("title").and_then(|t| t.as_str()).unwrap_or_default().to_string();
                    }
                    out.folders.push(a);
                }
            } else if let Ok(s) = serde_json::from_value::<Song>(c) {
                out.songs.push(s);
            }
        }
        Ok(out)
    }

    pub fn parse_music_folders(&self, body: Vec<u8>) -> Result<Vec<MusicFolder>> {
        Ok(parse(&body)?.music_folders.unwrap_or_default().music_folder)
    }

    /// Validates any response; used for ping and for calls with no payload.
    pub(crate) fn parse_status(&self, body: Vec<u8>) -> Result<ServerInfo> {
        let r = parse(&body)?;
        Ok(ServerInfo { version: r.version, server_type: r.kind, server_version: r.server_version, open_subsonic: r.open_subsonic })
    }

    pub fn parse_search(&self, body: Vec<u8>) -> Result<SearchResult> {
        let f = parse(&body)?.search_result3.unwrap_or_default();
        Ok(SearchResult { artists: f.artist, albums: f.album, songs: f.song })
    }

    /// Library sync: indexes a search3 page and returns its counts, not its items.
    pub(crate) fn ingest_search(&self, body: Vec<u8>) -> Result<IngestStats> {
        let f = parse(&body)?.search_result3.unwrap_or_default();
        let mut st = db::index(&mut self.db.lock(), &f.artist, &f.album, &f.song)?;
        // Counts seen, not changed: callers page until an empty page.
        st.artists = f.artist.len() as u32;
        st.albums = f.album.len() as u32;
        st.songs = f.song.len() as u32;
        Ok(st)
    }

    pub fn parse_starred(&self, body: Vec<u8>) -> Result<Starred> {
        let f = parse(&body)?.starred2.unwrap_or_default();
        Ok(Starred::new(f.artist, f.album, f.song))
    }

    pub fn parse_album(&self, body: Vec<u8>) -> Result<AlbumDetail> {
        let a = parse(&body)?.album.unwrap_or_default();
        Ok(AlbumDetail::new(a.album, a.song, a.disc_titles))
    }

    pub fn parse_artist(&self, body: Vec<u8>) -> Result<ArtistDetail> {
        let a = parse(&body)?.artist.unwrap_or_default();
        Ok(ArtistDetail::new(a.artist, a.album))
    }

    pub fn parse_artist_info(&self, body: Vec<u8>) -> Result<ArtistInfo> {
        let i = parse(&body)?.artist_info2.unwrap_or_default();
        Ok(ArtistInfo {
            biography: i.biography.filter(|b| !b.trim().is_empty()),
            image_url: i.large_image_url.or(i.medium_image_url).filter(|u| !u.is_empty()),
            similar: i.similar_artist,
            last_fm_url: i.last_fm_url.filter(|u| u.starts_with("http")),
            music_brainz_id: i.music_brainz_id.filter(|m| !m.is_empty()),
        })
    }

    pub fn parse_album_list(&self, body: Vec<u8>) -> Result<Vec<Album>> {
        Ok(parse(&body)?.album_list2.unwrap_or_default().album)
    }

    pub fn parse_artists(&self, body: Vec<u8>) -> Result<Vec<Artist>> {
        Ok(parse(&body)?.artists.unwrap_or_default().index.into_iter().flat_map(|i| i.artist).collect())
    }

    /// randomSongs, songsByGenre, similarSongs2, topSongs and getSong all land here.
    pub(crate) fn parse_songs(&self, body: Vec<u8>) -> Result<Vec<Song>> {
        let r = parse(&body)?;
        Ok(r.random_songs.or(r.songs_by_genre).or(r.similar_songs2).or(r.top_songs).map(|s| s.song).or(r.song.map(|s| vec![s])).unwrap_or_default())
    }

    pub fn parse_playlists(&self, body: Vec<u8>) -> Result<Vec<Playlist>> {
        Ok(parse(&body)?.playlists.unwrap_or_default().playlist)
    }

    pub fn parse_playlist(&self, body: Vec<u8>) -> Result<PlaylistDetail> {
        let p = parse(&body)?.playlist.unwrap_or_default();
        Ok(PlaylistDetail::new(p.playlist, p.entry))
    }

    pub fn parse_genres(&self, body: Vec<u8>) -> Result<Vec<Genre>> {
        let mut g = parse(&body)?.genres.unwrap_or_default().genre;
        g.sort_by_key(|g| std::cmp::Reverse(g.song_count));
        Ok(g)
    }

    pub fn parse_radio(&self, body: Vec<u8>) -> Result<Vec<RadioStation>> {
        Ok(parse(&body)?.internet_radio_stations.unwrap_or_default().station)
    }

    /// Synced lyrics win over plain; word times from the server's cues or estimated (lyrics.rs).
    pub(crate) fn parse_lyrics(&self, body: Vec<u8>) -> Result<Lyrics> {
        Ok(lyrics::build(parse(&body)?.lyrics_list.unwrap_or_default().structured_lyrics))
    }

    pub fn parse_share(&self, body: Vec<u8>) -> Result<String> {
        parse(&body)?.shares.and_then(|s| s.share.into_iter().next()).map(|s| s.url).filter(|u| !u.is_empty()).ok_or(CoreError::Parse { reason: "no share in response".into() })
    }

    pub fn parse_play_queue(&self, body: Vec<u8>) -> Result<PlayQueue> {
        let q = parse(&body)?.play_queue.unwrap_or_default();
        let current = q.current.map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
        let index = current.and_then(|c| q.entry.iter().position(|s| s.id == c)).unwrap_or(0) as u32;
        Ok(PlayQueue { songs: q.entry, index, position_ms: q.position, origin: None })
    }

    pub fn cache_get(&self, key: String) -> Result<Option<Vec<u8>>> {
        let c = self.db.lock();
        let mut st = c.prepare_cached("SELECT body FROM cache WHERE server=sid() AND key=?1")?;
        Ok(st.query_row([key], |r| r.get(0)).optional()?)
    }

    /// Whether `key` was stored less than `max_age_ms` ago.
    pub(crate) fn cache_fresh(&self, key: String, max_age_ms: i64) -> Result<bool> {
        let c = self.db.lock();
        let ts: Option<i64> = c.prepare_cached("SELECT ts FROM cache WHERE server=sid() AND key=?1")?.query_row([key], |r| r.get(0)).optional()?;
        Ok(ts.is_some_and(|t| db::now_ms() - t < max_age_ms))
    }

    pub fn cache_put(&self, key: String, body: Vec<u8>) -> Result<()> {
        let c = self.db.lock();
        c.prepare_cached("INSERT OR REPLACE INTO cache(server, key, body, ts) VALUES(sid(), ?1, ?2, ?3)")?.execute(params![key, body, db::now_ms()])?;
        Ok(())
    }

    /// Drops cached responses whose key starts with `prefix`.
    pub(crate) fn cache_evict(&self, prefix: String) -> Result<()> {
        let c = self.db.lock();
        c.execute("DELETE FROM cache WHERE server=sid() AND key >= ?1 AND key < ?1 || x'ff'", [prefix])?;
        Ok(())
    }

    pub(crate) fn pending_add(&self, endpoint: &str, params: &[(String, String)]) -> Result<()> {
        let json = serde_json::to_string(params).unwrap_or_default();
        self.db.lock().execute("INSERT INTO pending(server, endpoint, params) VALUES(sid(), ?1, ?2)", params![endpoint, json])?;
        Ok(())
    }

    pub(crate) fn cache_drop(&self, key: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM cache WHERE server=sid() AND key=?1", [key])?;
        Ok(())
    }

    pub(crate) fn pending_any(&self) -> Result<bool> {
        Ok(self.db.lock().query_row("SELECT EXISTS(SELECT 1 FROM pending WHERE server=sid())", [], |r| r.get(0))?)
    }

    pub(crate) fn pending_list(&self) -> Result<Vec<PendingCall>> {
        let c = self.db.lock();
        let mut st = c.prepare_cached("SELECT rowid, endpoint, params FROM pending WHERE server=sid() ORDER BY rowid LIMIT 200")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        Ok(rows
            .filter_map(|r| r.ok())
            .map(|(row_id, endpoint, json)| {
                let pairs: Vec<(String, String)> = serde_json::from_str(&json).unwrap_or_default();
                PendingCall { row_id, endpoint, params: pairs }
            })
            .collect())
    }

    pub(crate) fn pending_done(&self, row_id: i64) -> Result<()> {
        self.db.lock().execute("DELETE FROM pending WHERE server=sid() AND rowid=?1", [row_id])?;
        Ok(())
    }

    pub fn download_remove(&self, id: String) -> Result<()> {
        self.download_settle(vec![id], vec![false])
    }

    /// AutoEQ curves matching an output device name, best first.
    pub(crate) fn autoeq_for_device(&self, device: String, limit: u32) -> Result<Vec<AutoEqEntry>> {
        Ok(autoeq::matching(&self.db.lock(), &device, limit)?)
    }

    pub fn profile_save(&self, profile: SoundProfile) -> Result<()> {
        let c = self.db.lock();
        c.prepare_cached("INSERT OR REPLACE INTO profiles(name, json, outputs) VALUES(?1, ?2, ?3)")?
            .execute(params![profile.name, profile.json, profile.outputs.join("\n")])?;
        Ok(())
    }

    /// Binds `output` to profile `name` only (None: unbinds it).
    pub(crate) fn profile_bind(&self, output: String, name: Option<String>) -> Result<()> {
        let mut c = self.db.lock();
        let tx = c.transaction()?;
        let rows: Vec<(String, String)> = {
            let mut st = tx.prepare("SELECT name, outputs FROM profiles")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            r
        };
        for (profile, outputs) in rows {
            let mut list: Vec<&str> = outputs.lines().filter(|l| !l.is_empty() && *l != output).collect();
            if name.as_deref() == Some(profile.as_str()) {
                list.push(&output);
            }
            let joined = list.join("\n");
            if joined != outputs {
                tx.execute("UPDATE profiles SET outputs=?1 WHERE name=?2", params![joined, profile])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The profile bound to `output`, if any.
    pub(crate) fn profile_for_output(&self, output: String) -> Result<Option<SoundProfile>> {
        Ok(self.profiles()?.into_iter().find(|p| p.outputs.contains(&output)))
    }

    pub fn search_remember(&self, query: String) -> Result<()> {
        let c = self.db.lock();
        c.execute("INSERT OR REPLACE INTO searches(server, query, ts) VALUES(sid(), ?1, ?2)", params![query.trim(), db::now_ms()])?;
        c.execute("DELETE FROM searches WHERE server=sid() AND query NOT IN (SELECT query FROM searches WHERE server=sid() ORDER BY ts DESC LIMIT 20)", [])?;
        Ok(())
    }
}

#[cfg(test)]
impl Core {
    /// Marks one queued download finished.
    pub(crate) fn download_done(&self, id: String) -> Result<()> {
        self.download_settle(vec![id], vec![true])
    }
}

#[cfg(test)]
pub mod tests;
