//! Queue and transport rules the platform asks for (`nori_player::queue`, `nori_player::transport`):
//! prefetch and analysis targets, playback errors, the buttons, the sleep timer and service timings.
//! Settings are read here, not passed in.

use nori_player::queue::{self as q, ErrorRun};
use nori_player::transport as t;
use parking_lot::Mutex;

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
struct Controls {
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

// Global: the uniffi entry points have no handle.
static CONTROLS: Mutex<Controls> = Mutex::new(Controls { errors: ErrorRun::new(), last_error: None, sleep_left: 0 });

/// Upcoming songs (current first) the prefetch and analysis look at.
const UPCOMING: usize = 8;

/// The upcoming ids to prefetch on this network (`nori_player::queue::precache_range`).
pub fn queue_precache(metered: bool) -> Vec<String> {
    let (count, mixing) = prefs(|p| {
        (q::precache_count(metered, p.precache_wifi, p.precache_mobile), q::mixing(nori_automix::planner::transitions_off(), p.crossfade_sec, p.auto_mix))
    });
    crate::playlist::with(|p| {
        let Some((first, last)) = q::precache_range(count, mixing, p.shuffling()) else { return Vec::new() };
        p.upcoming().take(UPCOMING).skip(first).take(last + 1 - first).map(|i| p.ids()[i].clone()).collect()
    })
}

/// The upcoming analysable ids to analyse for AutoMix; empty while AutoMix is off.
pub fn queue_measure() -> Vec<String> {
    let n = prefs(|p| q::measure_ahead(p.auto_mix));
    crate::playlist::with(|p| p.upcoming().take(UPCOMING).take(n).map(|i| &p.ids()[i]).filter(|id| crate::queue::analysable(id)).cloned().collect())
}

/// A song failed to play: what to do (`nori_player::queue::on_error`). `bridge_ready`: the platform can
/// hand a network failure to the offline bridge.
pub fn queue_error(kind: PlaybackError, offload_refused: bool, bridge_ready: bool) -> OnError {
    let (skip, bridge) = prefs(|p| (p.skip_on_error, p.bridge_offline));
    let has_next = crate::playlist::with(|p| p.next().is_some());
    let mut c = CONTROLS.lock();
    c.last_error = Some(kind);
    c.errors.failed(kind, offload_refused, bridge && bridge_ready, skip, has_next)
}

/// The offline bridge took a network failure over: ends the error run.
pub fn queue_bridged() {
    CONTROLS.lock().played();
}

/// The last playback error until music plays again, so a player that stopped after an error run can say why.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_last_error() -> Option<PlaybackError> {
    CONTROLS.lock().last_error
}

/// The offline bridge could not take a network failure: whether to skip it.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_bridge_failed() -> bool {
    let skip = prefs(|p| p.skip_on_error);
    let has_next = crate::playlist::with(|p| p.next().is_some());
    CONTROLS.lock().errors.bridge_failed(skip, has_next)
}

/// Music is actually playing: ends the error run. A new song alone does not, since an error's skip is one.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_playing() {
    CONTROLS.lock().played();
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

/// Sets the sleep timer to `songs` songs or the end of this one (0/false cancel). Returns whether to
/// pause at the end of the current song.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sleep_set(songs: u32, end_of_track: bool) -> bool {
    let (pause, left) = t::sleep_after(songs, end_of_track);
    CONTROLS.lock().sleep_left = left;
    pause
}

/// The song changed: whether the sleep timer now pauses at its end.
pub fn sleep_song_changed() -> bool {
    let mut c = CONTROLS.lock();
    let (left, pause) = t::sleep_song_changed(c.sleep_left);
    c.sleep_left = left;
    pause
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
    /// Poll interval while making a seek stick.
    pub seek_look_ms: i64,
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
        seek_look_ms: nori_player::seek::LOOK_EVERY_MS,
    }
}

/// Player buffering: [min buffer ms, max ms, to start ms, to resume ms, target bytes].
pub fn load_control(memory_class_mb: u32) -> Vec<i64> {
    t::load_control(memory_class_mb).to_vec()
}

/// The prefetchable ids of `ids` ([`queue_precache`]'s output) that are not already being downloaded;
/// a download would otherwise be cached twice.
pub fn precache_list(ids: Vec<String>, downloading: impl Fn(&str) -> bool) -> Vec<String> {
    let mut ids = crate::queue::queue_fetchable(ids);
    ids.retain(|id| !downloading(id));
    ids
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

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn song_arrived() -> SongSteps {
    let on = prefs(|p| p.bridge_offline);
    let (bridging, parked) = crate::playlist::with(|p| (p.bridging(), p.next_is_parked()));
    SongSteps {
        save_after_ms: queue_keep(QueueMoment::Song).save_after_ms,
        fill: crate::autofill::autofill_start(),
        bridge: bridge_step(on, bridging, parked),
        precache_after_ms: t::PRECACHE_AFTER_MS,
        pause_at_end: sleep_song_changed(),
    }
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
    use crate::playlist::tests::hold;

    #[test]
    fn precache_targets_come_from_queue() {
        let _g = hold(&["pc1", "pc2", "pc3", "pc4", "ext-5"], 0);
        // Defaults: two ahead on Wi-Fi, one on metered; the player buffers the next song itself.
        assert_eq!(queue_precache(false), ["pc3"]);
        assert!(queue_precache(true).is_empty());
        assert!(queue_measure().is_empty(), "AutoMix off");
    }

    #[test]
    fn precache_list_skips_downloads_and_providers() {
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(precache_list(ids(&["1", "2", "ext-3", "4"]), |id| id == "2"), ["1", "4"]);
        assert_eq!(precache_list(ids(&["1", "2"]), |_| false), ["1", "2"]);
    }

    /// Regression: the skip an error makes used to count as success, so unplayable queues never stopped.
    #[test]
    fn error_skips_do_not_break_the_run() {
        let _g = hold(&["sk1", "sk2", "sk3", "sk4", "sk5"], 0);
        queue_playing();
        for i in 1..=3 {
            assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Skip);
            crate::playlist::playlist_moved_to(i);
        }
        assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Stop);
        assert_eq!(queue_last_error(), Some(PlaybackError::Other));
        queue_playing();
        assert_eq!(queue_last_error(), None);
    }

    #[test]
    fn playing_breaks_the_error_run() {
        let _g = hold(&["er1", "er2"], 0);
        queue_playing();
        for _ in 0..3 {
            assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Skip);
        }
        assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Stop);
        queue_playing();
        assert_eq!(queue_error(PlaybackError::Network, false, true), OnError::Skip, "bridge off by default");
        crate::playlist::playlist_moved_to(1);
        assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Stop, "nothing after the last song");
        assert!(!queue_bridge_failed());
        queue_playing();
    }

    #[test]
    fn sleep_timer_counts_song_changes() {
        let _g = hold(&["sa1", "sa2"], 0);
        assert!(!sleep_set(3, false));
        let s = song_arrived();
        assert_eq!((s.save_after_ms, s.bridge, s.pause_at_end), (t::SAVE_AFTER_MS, BridgeStep::Off, false));
        assert!(song_arrived().pause_at_end, "the third song is the last");
        assert!(!song_arrived().pause_at_end);
        assert!(sleep_set(0, true));
        assert!(!sleep_song_changed(), "end of track counts nothing");
    }

    #[test]
    fn queue_keep_per_moment() {
        assert_eq!(queue_keep(QueueMoment::Paused), QueueKeep { save_after_ms: 0, push: true });
        assert_eq!(queue_keep(QueueMoment::Closing), QueueKeep { save_after_ms: 0, push: false });
        for m in [QueueMoment::Song, QueueMoment::Edited] {
            assert_eq!(queue_keep(m), QueueKeep { save_after_ms: t::SAVE_AFTER_MS, push: false });
        }
    }

    #[test]
    fn bridge_step_table() {
        assert_eq!(bridge_step(false, true, true), BridgeStep::Off);
        assert_eq!(bridge_step(true, false, true), BridgeStep::Idle);
        assert_eq!(bridge_step(true, true, false), BridgeStep::Bridging);
        assert_eq!(bridge_step(true, true, true), BridgeStep::Parked);
    }
}
