//! Play counting. Listening time is summed from play/pause edges and judged when the song is left (no
//! timer). The song left is recorded in the local history in the background; the platform is told what
//! to send the server.

use nori_db as db;
use nori_db::background;
use nori_model::Song;
use parking_lot::Mutex;

use crate::queue;

#[derive(Default)]
struct Scrobbler {
    /// The current song, kept from its start: the queue may be replaced before it is left.
    song: Option<Song>,
    started_at: i64,
    heard_ms: i64,
    /// Monotonic ms playback last started; None while paused.
    playing_since: Option<i64>,
}

// Global: the uniffi entry points have no handle.
static SCROBBLER: Mutex<Scrobbler> = Mutex::new(Scrobbler { song: None, started_at: 0, heard_ms: 0, playing_since: None });

impl Scrobbler {
    fn edge(&mut self, playing: bool, now: i64) {
        match (playing, self.playing_since) {
            (true, None) => self.playing_since = Some(now),
            (false, Some(since)) => {
                self.heard_ms += now - since;
                self.playing_since = None;
            }
            _ => {}
        }
    }

    /// Moves to `next`; returns the song left, how long it was heard and when it started.
    fn switch(&mut self, next: Option<Song>, playing: bool, now_ms: i64, wall_ms: i64) -> (Option<Song>, i64, i64) {
        self.edge(false, now_ms);
        let done = std::mem::replace(&mut self.song, next);
        let heard = std::mem::take(&mut self.heard_ms);
        let at = std::mem::replace(&mut self.started_at, wall_ms);
        self.edge(playing, now_ms);
        (done, heard, at)
    }
}

/// What to tell the server when a song is left.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ScrobbleSend {
    /// The song left, if heard long enough to count, and when it started.
    pub submit_id: Option<String>,
    pub submit_at: i64,
    /// For the server's "now playing".
    pub now_playing_id: Option<String>,
}

/// Playback started or stopped at `now_ms` (monotonic).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn scrobble_playing(playing: bool, now_ms: i64) {
    SCROBBLER.lock().edge(playing, now_ms);
}

/// Listening needed for a play to count: `percent` (clamped to 10..100) of `duration_s`, within 10 s..4 min.
pub fn needed_ms(duration_s: i64, percent: i32) -> i64 {
    (duration_s * 10 * percent.clamp(10, 100) as i64).clamp(10_000, 240_000)
}

/// Why the player's song changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TrackChange {
    /// Onto another song, or none.
    Moved,
    /// The same song again under repeat one: a separate play.
    Looped,
    Ended,
}

/// The id to follow after a change: none once ended; a radio stream only when it loops.
fn followed(id: Option<String>, why: TrackChange) -> Option<String> {
    match why {
        TrackChange::Ended => None,
        TrackChange::Looped => id,
        TrackChange::Moved => id.filter(|i| !i.starts_with(queue::RADIO_PREFIX)),
    }
}

/// The player's song changed to `id`. Records the song left in the history when the taste model is on,
/// and returns what to scrobble when scrobbling is on.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn scrobble_track(id: Option<String>, why: TrackChange, playing: bool, now_ms: i64, wall_ms: i64, tz_offset_ms: i32) -> ScrobbleSend {
    let (taste_model, scrobble, percent) = nori_settings::settings_store::with_prefs(|p| (p.taste_model, p.scrobble, p.scrobble_percent)).unwrap_or((true, true, 50));
    let next = followed(id, why);
    let (done, heard, at) = SCROBBLER.lock().switch(next.clone().and_then(queue::queue_song), playing, now_ms, wall_ms);
    if let (Some(song), true) = (done.clone(), taste_model) {
        background::run(move || {
            if let Some(db) = nori_db::active() {
                let _ = nori_library::history::record(&mut db.lock(), &song, at, heard, tz_offset_ms, db::now_ms());
            }
        });
    }
    if !scrobble {
        return ScrobbleSend { submit_id: None, submit_at: 0, now_playing_id: None };
    }
    let submit_id = done.filter(|s| heard >= needed_ms(s.duration as i64, percent)).map(|s| s.id);
    ScrobbleSend { submit_id, submit_at: at, now_playing_id: next }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needed_ms_bounds() {
        assert_eq!(needed_ms(200, 50), 100_000);
        assert_eq!(needed_ms(600, 50), 240_000);
        assert_eq!(needed_ms(10, 50), 10_000);
        assert_eq!(needed_ms(200, 5), 20_000, "percent clamped to 10");
    }

    #[test]
    fn followed_ids() {
        let id = |s: &str| Some(s.to_string());
        assert_eq!(followed(id("s1"), TrackChange::Moved), id("s1"));
        assert_eq!(followed(id("radio:4"), TrackChange::Moved), None);
        assert_eq!(followed(id("radio:4"), TrackChange::Looped), id("radio:4"));
        assert_eq!(followed(id("s1"), TrackChange::Ended), None);
    }

    #[test]
    fn heard_time_sums_play_edges_only() {
        let mut s = Scrobbler::default();
        let song = Song::only_id("a".into());
        s.switch(Some(song.clone()), true, 1_000, 50);
        s.edge(true, 2_000);
        s.edge(false, 5_000);
        s.edge(false, 9_000);
        s.edge(true, 10_000);
        let (done, heard, at) = s.switch(None, false, 12_000, 60);
        assert_eq!((done, heard, at), (Some(song), 6_000, 50));
    }
}
