//! How the queue moves and the controls behave (`nori_player::queue`, `nori_player::transport`), as the
//! platform asks: what is fetched and measured ahead, what to do when a song will not play (the run of
//! failures is counted here), the previous, next and repeat buttons, a switch waiting out its dip, the
//! sleep timer, and how long the service waits before each of its chores. The queue itself is
//! playlist.rs. One call per user action or player event; the settings are read here, not handed in.

use nori_player::queue::{self as q, ErrorRun};
use nori_player::transport as t;
use parking_lot::Mutex;

// Public, like model.rs's, since the uniffi scaffolding in crates/android names them by a public path.
pub use nori_model::model::PlaybackError;
pub use nori_player::queue::OnError;
pub use nori_player::transport::NextAction;

use nori_settings::settings::StoredPrefs;

/// One answer from the settings as they are now; the defaults before the app opened them.
pub fn prefs<R>(f: impl Fn(&StoredPrefs) -> R) -> R {
    nori_settings::settings_store::with_prefs(&f).unwrap_or_else(|| f(&StoredPrefs::default()))
}

// ---- described again for uniffi ----

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

// ---- fetched and measured ahead ----

/// How many songs coming up (the current one first) the chores below look at.
const UPCOMING: usize = 8;

/// The songs coming up that are fetched ahead now, on a metered network or not: the count is the
/// user's setting for that network, and a crossfade or AutoMix brings the next song in early
/// (`nori_player::queue::precache_range`). Empty: nothing to fetch.
pub fn queue_precache(metered: bool) -> Vec<String> {
    let (count, mixing) = prefs(|p| {
        (q::precache_count(metered, p.precache_wifi, p.precache_mobile), q::mixing(nori_automix::planner::transitions_off(), p.crossfade_sec, p.auto_mix))
    });
    crate::playlist::with(|p| {
        let Some((first, last)) = q::precache_range(count, mixing, p.shuffling()) else { return Vec::new() };
        p.upcoming().take(UPCOMING).skip(first).take(last + 1 - first).map(|i| p.ids()[i].clone()).collect()
    })
}

/// The songs coming up (the one playing first) to measure for AutoMix, those that can be measured at
/// all; none while AutoMix is off.
pub fn queue_measure() -> Vec<String> {
    let n = prefs(|p| q::measure_ahead(p.auto_mix));
    crate::playlist::with(|p| p.upcoming().take(UPCOMING).take(n).map(|i| &p.ids()[i]).filter(|id| crate::queue::analysable(id)).cloned().collect())
}

// ---- a song that will not play ----

static ERRORS: Mutex<ErrorRun> = Mutex::new(ErrorRun::new());

/// A song would not play: what to do (`nori_player::queue::on_error`, with the run of failures counted
/// here). `bridge_ready` whether the platform has an offline bridge to hand a network failure to; the
/// user's settings decide whether it is used, and whether a failure skips.
pub fn queue_error(kind: PlaybackError, offload_refused: bool, bridge_ready: bool) -> OnError {
    *LAST_ERROR.lock() = Some(kind);
    let (skip, bridge) = prefs(|p| (p.skip_on_error, p.bridge_offline));
    let has_next = crate::playlist::with(|p| p.next().is_some());
    ERRORS.lock().failed(kind, offload_refused, bridge && bridge_ready, skip, has_next)
}

/// The offline bridge took a network failure over: the run of failures is broken.
pub fn queue_bridged() {
    ERRORS.lock().played();
    *LAST_ERROR.lock() = None;
}

/// What the last song that would not play failed of, until music plays again: for a player that stops
/// by itself after a run of them (nori-engine's `Event::Stopped`) to say why. None when the stop was anything else (the sleep timer's end of a song).
static LAST_ERROR: Mutex<Option<PlaybackError>> = Mutex::new(None);

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_last_error() -> Option<PlaybackError> {
    *LAST_ERROR.lock()
}

/// The offline bridge could not take a network failure over: whether to skip it like any other.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_bridge_failed() -> bool {
    let skip = prefs(|p| p.skip_on_error);
    let has_next = crate::playlist::with(|p| p.next().is_some());
    ERRORS.lock().bridge_failed(skip, has_next)
}

/// Music is really playing: the run of failures is broken. Not merely a new song - the skip a failure
/// makes is a new song too, and counting that as success meant the run never passed one, so a queue
/// of unplayable songs was skipped through for ever instead of stopping after three.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_playing() {
    ERRORS.lock().played();
    *LAST_ERROR.lock() = None;
}


// ---- the buttons ----

/// Whether previous restarts the song playing (else the player's own previous decides), as the user's
/// "previous always skips" says.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_previous_restarts(position_ms: i64, has_previous: bool) -> bool {
    q::previous_restarts(position_ms, has_previous, prefs(|p| p.previous_always_skips))
}

/// The repeat mode after the button (media3's numbering: off 0, one 1, all 2).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn queue_next_repeat(mode: u8) -> u8 {
    q::next_repeat(mode)
}

/// What the next button does, with or without a song after the one playing.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn next_action(has_next: bool) -> NextAction {
    t::next_action(has_next)
}

/// Whether a skip the user asked for starts the music (it was paused).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn skip_plays(play_when_ready: bool) -> bool {
    t::skip_plays(play_when_ready)
}

// ---- the sleep timer ----

/// Song changes still to go before the sleep timer "after N songs" pauses.
static SLEEP_LEFT: Mutex<u32> = Mutex::new(0);

/// The sleep timer set to `songs` songs (or the end of this one; both 0/false cancel it): whether to
/// pause at the end of the song playing now. The song changes still to go are kept here.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sleep_set(songs: u32, end_of_track: bool) -> bool {
    let (pause, left) = t::sleep_after(songs, end_of_track);
    *SLEEP_LEFT.lock() = left;
    pause
}

/// The song changed: whether the sleep timer now pauses at the end of this one.
pub fn sleep_song_changed() -> bool {
    let mut left = SLEEP_LEFT.lock();
    let (still, pause) = t::sleep_song_changed(*left);
    *left = still;
    pause
}

/// The sleep timer in minutes as [delay ms, slack ms].
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sleep_delay(minutes: u32) -> Vec<i64> {
    let (d, s) = t::sleep_delay_ms(minutes);
    vec![d, s]
}

/// What the sleep timer shows once set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SleepShown {
    /// The clock time (the platform's monotonic one, as `now_ms` was) it pauses at; 0 for none.
    pub at_ms: i64,
    /// It waits for a song to end.
    pub at_end_of_track: bool,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sleep_shown(minutes: u32, end_of_track: bool, songs: u32, now_ms: i64) -> SleepShown {
    let (at_ms, at_end_of_track) = t::sleep_shown(minutes, end_of_track, songs, now_ms);
    SleepShown { at_ms, at_end_of_track }
}

// ---- when the service does its chores ----

/// How long the service waits before each of its chores, so every platform paces them the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PlaybackTimings {
    /// Into a song, before the songs after it are fetched ahead.
    pub precache_after_ms: i64,
    /// After the queue was edited, before the songs coming up are measured for AutoMix.
    pub measure_after_edit_ms: i64,
    /// After the sound settings changed, before the songs coming up are measured.
    pub measure_after_settings_ms: i64,
    /// After the queue changed, before it is saved.
    pub save_after_ms: i64,
    /// Paused this long, the output is let go (`nori_player::transport::IDLE_RELEASE_MS`).
    pub idle_release_ms: i64,
    /// One tick of a running volume fade.
    pub fade_tick_ms: i64,
    /// How often a seek being made to stick is looked at (`nori_player::seek::LOOK_EVERY_MS`).
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

/// How much the player reads ahead: [min buffer ms, max ms, to start ms, to resume ms, target bytes].
pub fn load_control(memory_class_mb: u32) -> Vec<i64> {
    t::load_control(memory_class_mb).to_vec()
}

/// What the precacher fetches, in order, of the songs [`queue_precache`] names: those that can be fetched
/// at all (`queue_fetchable`), less those the download queue is already fetching (`downloading`): a song
/// on its way into the downloads arrives for good, and pulling it into the rolling cache too keeps it
/// twice. A song already downloaded is passed over when its turn comes, since that can change meanwhile.
/// Android asks for it through `Client::precache_targets`.
pub fn precache_list(ids: Vec<String>, downloading: impl Fn(&str) -> bool) -> Vec<String> {
    let mut ids = crate::queue::queue_fetchable(ids);
    ids.retain(|id| !downloading(id));
    ids
}

// ---- what a moment asks of the service ----

/// A moment the queue is kept at, or handed to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum QueueMoment {
    /// A new song arrived (not a repeat-one loop).
    Song,
    /// The queue's list was edited.
    Edited,
    /// Playback paused by the listener (not a stall): the moment another device may pick the queue up.
    Paused,
    /// The service or the program is closing.
    Closing,
}

/// What to do with the queue at a [`QueueMoment`]: save it (`Core::playlist_save`) now (`save_after_ms`
/// 0) or once this long has passed with nothing else changing it (the platform's timer, restarted by the
/// next moment), and whether to hand it to the server too (`Client::playlist_push`, which itself does
/// nothing unless the settings say so).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QueueKeep {
    pub save_after_ms: i64,
    pub push: bool,
}

/// When the queue is kept, and when it is handed to the server: a song or an edit saves it a moment
/// later (a burst of edits saves once), a pause saves it at once and hands it over (the listener may be
/// about to pick it up elsewhere), closing saves it at once.
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
    /// Nothing: the bridge is switched off.
    Off,
    /// No bridge is playing: stop watching the network, if it was watched.
    Idle,
    /// A bridge plays and has songs of its own left before the parked one.
    Bridging,
    /// The parked song is next: the queue comes back if the network has, more downloads go in before it
    /// if not (`Core::bridge_parked`).
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

/// Everything a new song asks of the platform, in one answer. The steps with a state of their own are
/// taken here (the refill's fetch counted as on the wire, the sleep timer's songs counted down), so this
/// is asked once per song, and never on a repeat-one loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SongSteps {
    /// Save the queue after this long ([`queue_keep`] of [`QueueMoment::Song`]).
    pub save_after_ms: i64,
    /// Fetch songs for the queue's end now (`Client::autofill`, then `autofill_arrived`).
    pub fill: bool,
    pub bridge: BridgeStep,
    /// Fetch the songs coming up ahead (`Client::precache_targets`) after this long, once the song
    /// playing has been fetched. A client on nori-engine with a store has the engine do it
    /// (`CoreLibrary::ahead`, as the next song is fetched) and skips this.
    pub precache_after_ms: i64,
    /// The sleep timer's last song: pause at its end.
    pub pause_at_end: bool,
}

/// A new song arrived (the ear is on it): what to do now.
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

/// Whether the equalizer screen trades the deep buffer for the shallow one now (`set_tuning`): only
/// while the screen is in sight (`in_sight`, the client's own call), after a change of the sound made on
/// it (`touched`), with the equalizer on - with it off, nothing it changes is heard. A change counts as
/// touching it when this is true of it with `touched` true.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn equalizer_tuning(in_sight: bool, touched: bool, eq_on: bool) -> bool {
    t::tuning_wanted(in_sight, touched, eq_on)
}

// ---- the volume slider -------------------------------------------------------------------------------------

/// The output's volume step for a slider at `fraction` (0..1) of an output with steps 0..=`max`: the
/// nearest step, not the one below - truncating made the bar jump back a notch every time it was let
/// go. Halves go to the even step, as Kotlin's `round` does. None when the output has no steps.
///
/// Twin of `PlayerViewModel.setVolumeFraction` (app/.../vm/PlayerViewModel.kt), which Android keeps next to
/// its `AudioManager`.
pub fn volume_step(fraction: f32, max: i32) -> Option<i32> {
    (max > 0).then(|| ((fraction.clamp(0.0, 1.0) * max as f32).round_ties_even() as i32).clamp(0, max))
}

/// Where the slider stands for step `step` of 0..=`max`; 0 for an output with no steps.
///
/// Twin of `PlayerViewModel.volumeFraction` (app/.../vm/PlayerViewModel.kt).
pub fn volume_fraction(step: i32, max: i32) -> f32 {
    if max > 0 { step as f32 / max as f32 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::tests::hold;

    #[test]
    fn the_songs_ahead_come_from_the_queue() {
        let _g = hold(&["pc1", "pc2", "pc3", "pc4", "ext-5"], 0);
        // Defaults: two ahead on Wi-Fi, one on a metered network, no transition.
        assert_eq!(queue_precache(false), ["pc3"], "the player buffers the next song itself");
        assert!(queue_precache(true).is_empty(), "one ahead is the player's own");
        assert!(queue_measure().is_empty(), "AutoMix off: nothing measured");
    }

    #[test]
    fn the_precacher_leaves_downloads_and_providers_alone() {
        let _g = hold(&[], 0);
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(precache_list(ids(&["1", "2", "ext-3", "4"]), |id| id == "2"), ["1", "4"]);
        assert_eq!(precache_list(ids(&["1", "2"]), |_| false), ["1", "2"]);
        assert!(precache_list(ids(&["4"]), |id| id == "4").is_empty());
        assert!(precache_list(Vec::new(), |_| true).is_empty());
    }

    #[test]
    fn the_skip_a_failure_makes_does_not_break_the_run() {
        let _g = hold(&["sk1", "sk2", "sk3", "sk4", "sk5"], 0);
        queue_playing();
        for i in 1..=3 {
            assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Skip);
            // The skip lands on the next song, which fails too: no music in between.
            crate::playlist::playlist_transition(i, false);
        }
        assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Stop, "three in a row, then it stops");
    }

    #[test]
    fn errors_are_counted_here_and_music_playing_breaks_the_run() {
        let _g = hold(&["er1", "er2"], 0);
        ERRORS.lock().played();
        for _ in 0..3 {
            assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Skip);
        }
        assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Stop);
        queue_playing();
        assert_eq!(queue_error(PlaybackError::Network, false, true), OnError::Skip, "the bridge is off by default");
        crate::playlist::playlist_moved_to(1);
        assert_eq!(queue_error(PlaybackError::Other, false, true), OnError::Stop, "nothing after the last song");
        assert!(!queue_bridge_failed());
        queue_playing();
    }

    #[test]
    fn the_sleep_timer_counts_songs_here() {
        // The count is the process's, as the queue is: taken in turns with the song steps' test.
        let _g = hold(&[], 0);
        assert!(!sleep_set(3, false));
        assert!(!sleep_song_changed());
        assert!(sleep_song_changed(), "the third song is the last");
        assert!(!sleep_song_changed());
        assert!(sleep_set(0, true));
        assert!(!sleep_song_changed(), "end of track: nothing counted");
        assert_eq!(sleep_shown(1, false, 0, 10), SleepShown { at_ms: 60_010, at_end_of_track: false });
        assert_eq!(playback_timings().seek_look_ms, 300);
    }

    #[test]
    fn a_pause_saves_and_hands_the_queue_over_and_a_song_or_an_edit_saves_it_later() {
        assert_eq!(queue_keep(QueueMoment::Paused), QueueKeep { save_after_ms: 0, push: true });
        assert_eq!(queue_keep(QueueMoment::Closing), QueueKeep { save_after_ms: 0, push: false });
        for m in [QueueMoment::Song, QueueMoment::Edited] {
            assert_eq!(queue_keep(m), QueueKeep { save_after_ms: t::SAVE_AFTER_MS, push: false });
        }
    }

    #[test]
    fn the_bridge_step_follows_the_setting_and_the_parked_song() {
        assert_eq!(bridge_step(false, true, true), BridgeStep::Off);
        assert_eq!(bridge_step(true, false, true), BridgeStep::Idle);
        assert_eq!(bridge_step(true, true, false), BridgeStep::Bridging);
        assert_eq!(bridge_step(true, true, true), BridgeStep::Parked);
    }

    #[test]
    fn a_song_arriving_counts_the_sleep_timer_down_and_saves_later() {
        let _g = hold(&["sa1", "sa2"], 0);
        assert!(!sleep_set(3, false));
        let s = song_arrived();
        assert_eq!((s.save_after_ms, s.precache_after_ms, s.bridge, s.pause_at_end), (t::SAVE_AFTER_MS, t::PRECACHE_AFTER_MS, BridgeStep::Off, false));
        assert!(song_arrived().pause_at_end, "the third song is the sleep timer's last");
        assert!(!song_arrived().pause_at_end);
    }

    #[test]
    fn the_equalizer_tunes_only_in_sight_touched_and_on() {
        assert!(equalizer_tuning(true, true, true));
        assert!(!equalizer_tuning(true, true, false), "the equalizer off: nothing to hear at once");
        assert!(!equalizer_tuning(false, true, true));
        assert!(!equalizer_tuning(true, false, true), "opened to look is not a reason");
    }

    #[test]
    fn previous_reads_the_setting_itself() {
        assert!(queue_previous_restarts(5_000, true), "the default does not always skip");
        assert!(!queue_previous_restarts(1_000, true));
    }
}
