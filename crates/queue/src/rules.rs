//! Queue and transport rules the platform asks for (`nori_player::queue`, `nori_player::transport`):
//! prefetch and analysis targets, playback errors, the buttons, the sleep timer and service timings.
//! Settings are read here, not passed in.

use nori_player::queue::{self as q, ErrorRun};
use nori_player::transport as t;

use crate::{shared, Session};

// Public so the uniffi scaffolding can name them.
pub use nori_model::model::PlaybackError;
pub use nori_player::queue::OnError;
pub use nori_player::transport::NextAction;


pub use nori_settings::settings_store::prefs;

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum OnError {
    GiveUpOffload,
    Bridge,
    Skip,
    Stop,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum NextAction {
    Skip,
    FillThenSkip,
}

/// Playback state the rules keep between calls.
#[derive(Default)]
pub(crate) struct Controls {
    errors: ErrorRun,
    /// The last playback error, until music plays again.
    last_error: Option<PlaybackError>,
    /// Song changes left before the "after N songs" sleep timer pauses.
    sleep_left: u32,
}

impl Controls {
    fn played(&mut self) {
        self.errors.played();
        self.last_error = None;
    }
}

/// Upcoming songs (current first) the prefetch and analysis look at.
const UPCOMING: usize = 8;

impl Session {
    /// The upcoming ids to prefetch on this network (`nori_player::queue::precache_range`).
    pub fn precache(&self, metered: bool) -> Vec<String> {
        let (count, mixing) = prefs(|p| {
            (q::precache_count(metered, p.precache_wifi, p.precache_mobile), q::mixing(nori_automix::planner::transitions_off(), p.crossfade_sec, p.auto_mix))
        });
        self.playlist(|p| {
            let Some((first, last)) = q::precache_range(count, mixing, p.shuffling()) else { return Vec::new() };
            p.upcoming().take(UPCOMING).skip(first).take(last + 1 - first).map(|i| p.ids()[i].clone()).collect()
        })
    }

    /// The prefetchable ids of `ids` ([`Session::precache`]'s output) that are not already being
    /// downloaded; a download would otherwise be cached twice.
    pub fn precache_list(&self, ids: Vec<String>, downloading: impl Fn(&str) -> bool) -> Vec<String> {
        let mut ids = self.fetchable(ids);
        ids.retain(|id| !downloading(id));
        ids
    }

    /// The upcoming analysable ids to analyse for AutoMix; empty while AutoMix is off.
    pub fn measure(&self) -> Vec<String> {
        let n = prefs(|p| q::measure_ahead(p.auto_mix));
        self.playlist(|p| p.upcoming().take(UPCOMING).take(n).map(|i| &p.ids()[i]).filter(|id| crate::queue::analysable(id)).cloned().collect())
    }

    /// A song failed to play: what to do (`nori_player::queue::on_error`). `bridge_ready`: the platform
    /// can hand a network failure to the offline bridge.
    pub fn error(&self, kind: PlaybackError, offload_refused: bool, bridge_ready: bool) -> OnError {
        let (skip, bridge) = prefs(|p| (p.skip_on_error, p.bridge_offline));
        let has_next = self.playlist(|p| p.next().is_some());
        let mut c = self.controls.lock();
        c.last_error = Some(kind);
        c.errors.failed(kind, offload_refused, bridge && bridge_ready, skip, has_next)
    }

    /// The offline bridge took a network failure over: ends the error run.
    pub fn bridged(&self) {
        self.controls.lock().played();
    }

    /// The last playback error until music plays again, so a player that stopped after an error run can
    /// say why.
    pub fn last_error(&self) -> Option<PlaybackError> {
        self.controls.lock().last_error
    }

    /// The offline bridge could not take a network failure: whether to skip it.
    pub fn bridge_failed(&self) -> bool {
        let skip = prefs(|p| p.skip_on_error);
        let has_next = self.playlist(|p| p.next().is_some());
        self.controls.lock().errors.bridge_failed(skip, has_next)
    }

    /// Music is actually playing: ends the error run. A new song alone does not, since an error's skip is one.
    pub fn playing(&self) {
        self.controls.lock().played();
    }

    /// Sets the sleep timer to `songs` songs or the end of this one (0/false cancel). Returns whether to
    /// pause at the end of the current song.
    pub fn sleep_set(&self, songs: u32, end_of_track: bool) -> bool {
        let (pause, left) = t::sleep_after(songs, end_of_track);
        self.controls.lock().sleep_left = left;
        pause
    }

    /// The song changed: whether the sleep timer now pauses at its end.
    pub fn sleep_song_changed(&self) -> bool {
        let mut c = self.controls.lock();
        let (left, pause) = t::sleep_song_changed(c.sleep_left);
        c.sleep_left = left;
        pause
    }

    /// Everything a new song asks of the platform. Stateful steps (refill fetch, sleep countdown) are
    /// taken here, so ask once per song and never on a repeat-one loop.
    pub fn song_arrived(&self) -> SongSteps {
        let on = prefs(|p| p.bridge_offline);
        let (bridging, parked) = self.playlist(|p| (p.bridging(), p.next_is_parked()));
        SongSteps {
            save_after_ms: queue_keep(QueueMoment::Song).save_after_ms,
            fill: self.autofill_start(),
            bridge: bridge_step(on, bridging, parked),
            precache_after_ms: t::PRECACHE_AFTER_MS,
            pause_at_end: self.sleep_song_changed(),
        }
    }
}

// ---- the platform's entry points, over the shared session ----

/// [`Session::last_error`].
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_last_error() -> Option<PlaybackError> {
    shared().last_error()
}

/// [`Session::bridge_failed`].
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_bridge_failed() -> bool {
    shared().bridge_failed()
}

/// [`Session::playing`].
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_playing() {
    shared().playing()
}

/// [`Session::sleep_set`].
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sleep_set(songs: u32, end_of_track: bool) -> bool {
    shared().sleep_set(songs, end_of_track)
}

/// [`Session::song_arrived`].
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn song_arrived() -> SongSteps {
    shared().song_arrived()
}

/// Whether previous restarts the current song (per "previous always skips").
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_previous_restarts(position_ms: i64, has_previous: bool) -> bool {
    q::previous_restarts(position_ms, has_previous, prefs(|p| p.previous_always_skips))
}

/// The repeat mode after a press of the button (media3 numbering: off 0, one 1, all 2).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_next_repeat(mode: u8) -> u8 {
    q::next_repeat(mode)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn next_action(has_next: bool) -> NextAction {
    t::next_action(has_next)
}

/// Whether a user skip starts playback.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn skip_plays(play_when_ready: bool) -> bool {
    t::skip_plays(play_when_ready)
}

/// The sleep timer for `minutes` as [delay ms, slack ms].
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sleep_delay(minutes: u32) -> Vec<i64> {
    let (d, s) = t::sleep_delay_ms(minutes);
    vec![d, s]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SleepShown {
    /// Monotonic time it pauses at (the clock of `now_ms`); 0 for none.
    pub at_ms: i64,
    pub at_end_of_track: bool,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sleep_shown(minutes: u32, end_of_track: bool, songs: u32, now_ms: i64) -> SleepShown {
    let (at_ms, at_end_of_track) = t::sleep_shown(minutes, end_of_track, songs, now_ms);
    SleepShown { at_ms, at_end_of_track }
}

/// Delays for the playback service's chores, shared by every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PlaybackTimings {
    pub precache_after_ms: i64,
    pub measure_after_edit_ms: i64,
    pub measure_after_settings_ms: i64,
    pub save_after_ms: i64,
    /// Paused this long, the output is released.
    pub idle_release_ms: i64,
    pub fade_tick_ms: i64,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playback_timings() -> PlaybackTimings {
    PlaybackTimings {
        precache_after_ms: t::PRECACHE_AFTER_MS,
        measure_after_edit_ms: t::MEASURE_AFTER_EDIT_MS,
        measure_after_settings_ms: t::MEASURE_AFTER_SETTINGS_MS,
        save_after_ms: t::SAVE_AFTER_MS,
        idle_release_ms: t::IDLE_RELEASE_MS,
        fade_tick_ms: t::FADE_TICK_MS,
    }
}

/// Player buffering: [min buffer ms, max ms, to start ms, to resume ms, target bytes].
pub fn load_control(memory_class_mb: u32) -> Vec<i64> {
    t::load_control(memory_class_mb).to_vec()
}

/// A moment at which the queue is saved or pushed to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum QueueMoment {
    /// A new song (not a repeat-one loop).
    Song,
    Edited,
    /// Paused by the user (not a stall).
    Paused,
    Closing,
}

/// Save the queue after `save_after_ms` (0: now; the timer restarts on the next moment), and whether to
/// push it to the server (`Client::playlist_push`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QueueKeep {
    pub save_after_ms: i64,
    pub push: bool,
}

/// Songs and edits save later (bursts save once); a pause saves and pushes now; closing saves now.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_keep(moment: QueueMoment) -> QueueKeep {
    match moment {
        QueueMoment::Song | QueueMoment::Edited => QueueKeep { save_after_ms: t::SAVE_AFTER_MS, push: false },
        QueueMoment::Paused => QueueKeep { save_after_ms: 0, push: true },
        QueueMoment::Closing => QueueKeep { save_after_ms: 0, push: false },
    }
}

/// What the offline bridge does as a song arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BridgeStep {
    /// The bridge setting is off.
    Off,
    /// Not bridging: stop watching the network.
    Idle,
    /// Bridging, with bridge songs left before the parked one.
    Bridging,
    /// The parked song is next: resume the queue if online, else add downloads (`Core::bridge_parked`).
    Parked,
}

fn bridge_step(on: bool, bridging: bool, next_is_parked: bool) -> BridgeStep {
    match (on, bridging, next_is_parked) {
        (false, ..) => BridgeStep::Off,
        (true, false, _) => BridgeStep::Idle,
        (true, true, false) => BridgeStep::Bridging,
        (true, true, true) => BridgeStep::Parked,
    }
}

/// Everything a new song asks of the platform. Stateful steps (refill fetch, sleep countdown) are taken
/// here, so ask once per song and never on a repeat-one loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SongSteps {
    pub save_after_ms: i64,
    /// Fetch songs for the queue's end now (`Client::autofill`, then `autofill_arrived`).
    pub fill: bool,
    pub bridge: BridgeStep,
    /// Prefetch upcoming songs after this delay (`Client::precache_targets`); nori-engine clients skip it.
    pub precache_after_ms: i64,
    /// The sleep timer's last song: pause at its end.
    pub pause_at_end: bool,
}

/// Whether the equalizer screen switches to the shallow buffer (`set_tuning`): screen visible, sound
/// changed on it, equalizer on.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn equalizer_tuning(in_sight: bool, touched: bool, eq_on: bool) -> bool {
    t::tuning_wanted(in_sight, touched, eq_on)
}

/// The volume step for slider `fraction` on an output with steps 0..=`max`, rounded half to even like
/// Kotlin's `round`; None without steps. Twin of `PlayerViewModel.setVolumeFraction`.
pub fn volume_step(fraction: f32, max: i32) -> Option<i32> {
    (max > 0).then(|| ((fraction.clamp(0.0, 1.0) * max as f32).round_ties_even() as i32).clamp(0, max))
}

/// The slider fraction for `step` of 0..=`max`; 0 without steps. Twin of `PlayerViewModel.volumeFraction`.
pub fn volume_fraction(step: i32, max: i32) -> f32 {
    if max > 0 { step as f32 / max as f32 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::tests::session;

    #[test]
    fn precache_targets_come_from_queue() {
        let s = session(&["pc1", "pc2", "pc3", "pc4", "ext-5"], 0);
        // Defaults: two ahead on Wi-Fi, one on metered; the player buffers the next song itself.
        assert_eq!(s.precache(false), ["pc3"]);
        assert!(s.precache(true).is_empty());
        assert!(s.measure().is_empty(), "AutoMix off");
    }

    #[test]
    fn precache_list_skips_downloads_and_providers() {
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let s = Session::default();
        assert_eq!(s.precache_list(ids(&["1", "2", "ext-3", "4"]), |id| id == "2"), ["1", "4"]);
        assert_eq!(s.precache_list(ids(&["1", "2"]), |_| false), ["1", "2"]);
    }

    /// The skip an error makes is no success: an unplayable queue stops.
    #[test]
    fn error_run_ends_only_when_playing() {
        for skips_move in [true, false] {
            let s = session(&["er1", "er2", "er3", "er4", "er5"], 0);
            s.playing();
            for i in 1..=3 {
                assert_eq!(s.error(PlaybackError::Other, false, true), OnError::Skip, "skips move: {skips_move}");
                if skips_move {
                    s.moved_to(i);
                }
            }
            assert_eq!(s.error(PlaybackError::Other, false, true), OnError::Stop, "skips move: {skips_move}");
            assert_eq!(s.last_error(), Some(PlaybackError::Other));
            s.playing();
            assert_eq!(s.last_error(), None);
        }
        let s = session(&["er1", "er2"], 0);
        s.playing();
        assert_eq!(s.error(PlaybackError::Network, false, true), OnError::Skip, "bridge off by default");
        s.moved_to(1);
        assert_eq!(s.error(PlaybackError::Other, false, true), OnError::Stop, "nothing after the last song");
        assert!(!s.bridge_failed());
        s.playing();
    }

    #[test]
    fn sleep_timer_counts_song_changes() {
        let s = session(&["sa1", "sa2"], 0);
        assert!(!s.sleep_set(3, false));
        let steps = s.song_arrived();
        assert_eq!((steps.save_after_ms, steps.bridge, steps.pause_at_end), (t::SAVE_AFTER_MS, BridgeStep::Off, false));
        assert!(s.song_arrived().pause_at_end, "the third song is the last");
        assert!(!s.song_arrived().pause_at_end);
        assert!(s.sleep_set(0, true));
        assert!(!s.sleep_song_changed(), "end of track counts nothing");
    }

}
