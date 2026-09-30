//! Transport behaviour: fades on play and pause, dips around seeks and skips, and the sleep timer. The
//! platform runs them; this decides them.

/// What a control does to the music.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// A new position in the same song.
    Seek,
    /// Straight to another song in the queue.
    ToSong,
    /// Next or previous.
    Skip,
}

/// Volume down, switch, back up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dip {
    pub down_ms: i32,
    pub up_ms: i32,
}

/// Longest fade-down before a switch, so it feels immediate.
const DIP_DOWN_MAX_MS: i32 = 150;
/// Minimum dip for a jump to another song even with fades off (a hard cut clicks).
const TO_SONG_FLOOR_MS: i32 = 120;

/// The dip around a switch while playing, `fade_ms` being the user's fade length (0 off). `None`:
/// switch at once.
pub fn switch_dip(fade_ms: i32, switch: Switch, playing: bool) -> Option<Dip> {
    let floor = if switch == Switch::ToSong { TO_SONG_FLOOR_MS } else { 0 };
    let ms = fade_ms.max(floor);
    (ms > 0 && playing).then_some(Dip { down_ms: ms.min(DIP_DOWN_MAX_MS), up_ms: ms })
}

/// How long a jump's target is shown while the engine dips before making it (longer than any dip plus
/// command latency).
pub const JUMP_SHOWN_MS: i64 = 1_000;

/// The position to show: the jump's target (`jump`: (ms, age ms)) until the engine has reported since
/// or, while `switching`, for [`JUMP_SHOWN_MS`]; else the engine's `engine_ms`. A dip unrelated to the
/// jump (a later resound) must not bring back the jump's old target.
pub fn shown_place(jump: Option<(i64, i64)>, reported_since: bool, switching: bool, engine_ms: i64) -> i64 {
    match jump {
        Some((ms, age)) if !reported_since || (switching && age <= JUMP_SHOWN_MS) => ms.max(0),
        _ => engine_ms.max(0),
    }
}

/// Fade-in length on play; `None` to start at full volume.
pub fn play_fade(fade_ms: i32, playing: bool) -> Option<i32> {
    (fade_ms > 0 && !playing).then_some(fade_ms)
}

/// Fade-out length before pausing; `None` to pause at once.
pub fn pause_fade(fade_ms: i32, playing: bool) -> Option<i32> {
    (fade_ms > 0 && playing).then_some(fade_ms)
}

/// Volume fade tick (one 60 Hz frame).
pub const FADE_TICK_MS: i64 = 16;

/// The volume of a fade from `from` to `to` started at `start_ms` lasting `ms`, and whether it is over.
/// A zero-length fade is over at once.
pub fn fade_step(from: f32, to: f32, start_ms: i64, now_ms: i64, ms: i32) -> (f32, bool) {
    if ms <= 0 {
        return (to, true);
    }
    let t = ((now_ms - start_ms) as f32 / ms as f32).clamp(0.0, 1.0);
    (from + (to - from) * t, t >= 1.0)
}

/// A user skip while paused starts playback (the player's own skips keep it paused).
pub fn skip_plays(play_when_ready: bool) -> bool {
    !play_when_ready
}

/// What the next button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextAction {
    Skip,
    /// Nothing next yet: remember the press until the queue refill lands.
    FillThenSkip,
}

pub fn next_action(has_next: bool) -> NextAction {
    if has_next {
        NextAction::Skip
    } else {
        NextAction::FillThenSkip
    }
}

/// Prefetch the following songs this far into a song.
pub const PRECACHE_AFTER_MS: i64 = 6_000;
/// Measure the new next song this long after a queue edit (debounce).
pub const MEASURE_AFTER_EDIT_MS: i64 = 2_000;
/// Measure upcoming songs this long after a sound settings change (AutoMix may have just been enabled).
pub const MEASURE_AFTER_SETTINGS_MS: i64 = 1_000;
/// Save the queue this long after its last change (debounce).
pub const SAVE_AFTER_MS: i64 = 1_500;

/// Sleep timer "after N songs": (pause at the end of this song, song changes left before that).
/// "End of track" equals one song.
pub fn sleep_after(songs: u32, end_of_track: bool) -> (bool, u32) {
    (end_of_track || songs == 1, songs.saturating_sub(1))
}

/// The equalizer screen wants the shallow buffer: visible, touched, equalizer on.
pub fn tuning_wanted(in_sight: bool, touched: bool, eq_on: bool) -> bool {
    in_sight && touched && eq_on
}

/// A song change with `left` changes to go: (remaining, pause at the end of this song).
pub fn sleep_song_changed(left: u32) -> (u32, bool) {
    match left {
        0 => (0, false),
        n => (n - 1, n == 1),
    }
}

/// The sleep timer's (delay, allowed slack for batching the wake-up), ms.
pub fn sleep_delay_ms(minutes: u32) -> (i64, i64) {
    (minutes as i64 * 60_000, 15_000)
}

/// The sleep timer display: (fire time or 0 without a clock timer, waits for a song to end).
pub fn sleep_shown(minutes: u32, end_of_track: bool, songs: u32, now_ms: i64) -> (i64, bool) {
    let at = if minutes > 0 { now_ms + sleep_delay_ms(minutes).0 } else { 0 };
    let (pause, left) = sleep_after(songs, end_of_track);
    (at, pause || left > 0)
}

/// Paused this long, the player releases the audio track and stops ticking so the phone can sleep.
pub const IDLE_RELEASE_MS: i64 = 5 * 60_000;

/// Read-ahead limits: [min buffer, max buffer, start, resume, byte cap] with the byte cap a quarter of the
/// memory class, 16 to 48 MB. Songs are fetched in seconds so the network sleeps.
pub fn load_control(memory_class_mb: u32) -> [i64; 5] {
    let mb = (memory_class_mb / 4).clamp(16, 48) as i64;
    [60_000, 600_000, 1_000, 2_000, mb * 1024 * 1024]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_target_shown_until_made() {
        assert_eq!(shown_place(None, true, true, 5_000), 5_000);
        assert_eq!(shown_place(Some((30_000, 10)), false, false, 5_000), 30_000, "not reported since");
        assert_eq!(shown_place(Some((30_000, 80)), true, true, 5_000), 30_000, "dipping before the jump");
        assert_eq!(shown_place(Some((30_000, 400)), true, false, 30_390), 30_390, "made");
        // A resound's dip a minute after the seek shows the engine's position.
        assert_eq!(shown_place(Some((12_000, 60_000)), true, true, 17_000), 17_000);
    }

    #[test]
    fn switch_dips() {
        assert_eq!(switch_dip(600, Switch::Seek, true), Some(Dip { down_ms: 150, up_ms: 600 }));
        assert_eq!(switch_dip(0, Switch::Seek, true), None, "fades off");
        assert_eq!(switch_dip(0, Switch::ToSong, true), Some(Dip { down_ms: 120, up_ms: 120 }), "another song always dips a little");
        assert_eq!(switch_dip(600, Switch::Skip, false), None, "paused: nothing to dip");
    }

    #[test]
    fn play_and_pause_fades() {
        assert_eq!(play_fade(400, false), Some(400));
        assert_eq!(play_fade(400, true), None);
        assert_eq!(pause_fade(400, true), Some(400));
        assert_eq!(pause_fade(0, true), None);
    }

    #[test]
    fn fade_steps() {
        assert_eq!(fade_step(1.0, 0.0, 100, 100, 200), (1.0, false));
        assert_eq!(fade_step(1.0, 0.0, 100, 200, 200), (0.5, false));
        assert_eq!(fade_step(1.0, 0.0, 100, 300, 200), (0.0, true));
        assert_eq!(fade_step(1.0, 0.0, 100, 900, 200), (0.0, true), "late ticks stay at the end");
        assert_eq!(fade_step(0.2, 0.8, 100, 50, 200), (0.2, false), "a clock before the start holds");
        assert_eq!(fade_step(1.0, 0.3, 100, 100, 0), (0.3, true), "no length: there at once");
    }

    #[test]
    fn sleep_timer_counts_songs() {
        assert_eq!(sleep_after(1, false), (true, 0));
        assert_eq!(sleep_after(3, false), (false, 2));
        assert_eq!(sleep_after(0, true), (true, 0));
        assert_eq!(sleep_song_changed(2), (1, false));
        assert_eq!(sleep_song_changed(1), (0, true));
        assert_eq!(sleep_song_changed(0), (0, false));
        assert_eq!(sleep_shown(30, false, 0, 1_000), (1_000 + 1_800_000, false));
        assert_eq!(sleep_shown(0, false, 0, 1_000), (0, false), "cancelled");
        assert_eq!(sleep_shown(0, true, 0, 1_000), (0, true));
        assert_eq!(sleep_shown(0, false, 1, 1_000), (0, true));
        assert_eq!(sleep_shown(0, false, 3, 1_000), (0, true));
        assert_eq!(load_control(256)[4], 48 * 1024 * 1024);
        assert_eq!(load_control(32)[4], 16 * 1024 * 1024);
    }
}
