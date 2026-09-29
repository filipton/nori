//! Client request values: the server profile, writes and the cached answers each makes stale.

use nori_model::IngestStats;

use crate::transport::NetError;

pub type NetResult<T> = std::result::Result<T, NetError>;

/// The endpoints that take `musicFolderId`.
pub const FOLDERED: [&str; 7] = ["getAlbumList2", "getArtists", "search3", "getRandomSongs", "getStarred2", "getSongsByGenre", "getIndexes"];

/// The parts of a server profile the client uses; headers, certificates and Wi-Fi only are the platform's.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct NetProfile {
    pub url: String,
    /// A second address of the same server, tried when `url` does not answer.
    pub alt_url: String,
    /// Restricts browsing and search to this music folder; empty means all.
    pub music_folder_id: String,
    /// Bitrate ceiling while connected through `alt_url`; 0 means none.
    pub alt_max_bit_rate: u32,
}

/// Kotlin's `isBlank`: nothing but whitespace.
pub fn blank(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

pub fn pairs(p: &[(&str, String)]) -> Vec<(String, String)> {
    p.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SyncStep {
    pub total: IngestStats,
    pub next_offset: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Starrable {
    Song,
    Album,
    Artist,
}

impl Starrable {
    pub fn param(self) -> &'static str {
        match self {
            Starrable::Song => "id",
            Starrable::Album => "albumId",
            Starrable::Artist => "artistId",
        }
    }
}

/// Every change the app asks the server for.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Write {
    Star { kind: Starrable, id: String, on: bool },
    CreatePlaylist { name: String, song_ids: Vec<String> },
    AddToPlaylist { id: String, song_ids: Vec<String> },
    RemoveFromPlaylist { id: String, index: i32 },
    DeletePlaylist { id: String },
    CreateRadio { name: String, stream_url: String },
    DeleteRadio { id: String },
    /// `submission` false marks "now playing"; true counts the play.
    Scrobble { id: String, submission: bool, time_ms: Option<i64> },
    /// The queue handed to another device.
    SaveQueue { ids: Vec<String>, current: Option<String>, position_ms: i64 },
}

/// Cached answers a star change makes stale, as key prefixes. The starred album list prefix leaves the
/// other album lists cached.
const STAR_STALE: [&str; 5] = ["getStarred2", "getAlbum", "getArtist", "getPlaylist", "getAlbumList2&type=starred"];

/// The endpoint, parameters and stale cache prefixes of a write.
pub fn request(w: Write) -> (&'static str, Vec<(String, String)>, &'static [&'static str]) {
    let one = |k: &str, v: String| vec![(k.to_string(), v)];
    let many = |k: &str, ids: Vec<String>| ids.into_iter().map(|v| (k.to_string(), v)).collect::<Vec<_>>();
    match w {
        Write::Star { kind, id, on } => (if on { "star" } else { "unstar" }, one(kind.param(), id), &STAR_STALE),
        Write::CreatePlaylist { name, song_ids } => {
            let mut p = one("name", name);
            p.extend(many("songId", song_ids));
            ("createPlaylist", p, &["getPlaylist"])
        }
        Write::AddToPlaylist { id, song_ids } => {
            let mut p = one("playlistId", id);
            p.extend(many("songIdToAdd", song_ids));
            ("updatePlaylist", p, &["getPlaylist"])
        }
        Write::RemoveFromPlaylist { id, index } => {
            ("updatePlaylist", pairs(&[("playlistId", id), ("songIndexToRemove", index.to_string())]), &["getPlaylist"])
        }
        Write::DeletePlaylist { id } => ("deletePlaylist", one("id", id), &["getPlaylist"]),
        Write::CreateRadio { name, stream_url } => {
            ("createInternetRadioStation", pairs(&[("name", name), ("streamUrl", stream_url)]), &["getInternetRadioStations"])
        }
        Write::DeleteRadio { id } => ("deleteInternetRadioStation", one("id", id), &["getInternetRadioStations"]),
        Write::Scrobble { id, submission, time_ms } => {
            let mut p = pairs(&[("id", id), ("submission", submission.to_string())]);
            if let Some(t) = time_ms {
                p.push(("time".into(), t.to_string()));
            }
            ("scrobble", p, &[])
        }
        Write::SaveQueue { ids, current, position_ms } => {
            let mut p = many("id", ids);
            if let Some(c) = current {
                p.push(("current".into(), c));
            }
            p.push(("position".into(), position_ms.to_string()));
            ("savePlayQueue", p, &[])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_requests() {
        let (_, p, s) = request(Write::SaveQueue { ids: vec!["a".into(), "b".into()], current: None, position_ms: 7 });
        assert_eq!(p, pairs(&[("id", "a".into()), ("id", "b".into()), ("position", "7".into())]));
        assert!(s.is_empty());
        let (e, p, s) = request(Write::Star { kind: Starrable::Artist, id: "x".into(), on: false });
        assert_eq!((e, p), ("unstar", pairs(&[("artistId", "x".into())])));
        assert_eq!(s, &STAR_STALE);
        let (e, p, _) = request(Write::Scrobble { id: "s".into(), submission: false, time_ms: None });
        assert_eq!((e, p), ("scrobble", pairs(&[("id", "s".into()), ("submission", "false".into())])));
    }
}
