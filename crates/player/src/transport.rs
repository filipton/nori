//! Transport behaviour: fades on play and pause, dips around seeks and skips, when a sound chain change
//! rebuilds the output, and the sleep timer. The platform runs them; this decides them.

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

/// Facts about a settings change, for [`rebuild`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChainChange {
    /// The audio chip is decoding the current song.
    pub offloaded: bool,
    /// Offload is wanted from now on (`AudioPolicy::offload`).
    pub offload: bool,
    pub offload_changed: bool,
    /// Something USB is attached (an offloaded song plays silence there).
    pub usb: bool,
    /// The output refused an offloaded song once.
    pub offload_refused: bool,
    pub tempo_changed: bool,
    /// A processor joins or leaves the chain.
    pub processor_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebuild {
    None,
    /// The current path is broken or silent: rebuild now.
    Now,
    /// Wait for the next song boundary, where the cut is inaudible.
    AtBoundary,
}

/// Whether and when the output must be rebuilt for a change.
pub fn rebuild(c: ChainChange) -> Rebuild {
    // Offload to USB plays silence, and the chip ignores speed changes: neither can wait.
    if (c.offloaded && !c.offload && (c.usb || c.offload_refused)) || (c.tempo_changed && c.offloaded) {
        return Rebuild::Now;
    }
    if !(c.processor_changed || (c.offload_changed && c.offloaded)) {
        return Rebuild::None;
    }
    // Leaving offload: the offloaded path cannot take the new chain.
    if c.offloaded && !c.offload {
        Rebuild::Now
    } else {
        Rebuild::AtBoundary
    }
}

/// Output rebuilds for the equalizer screen's shallow buffer (a band change is audible within half a
/// second) and back. While playing, the swap waits for the next boundary or pause; bursts stop at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Chain {
    /// The equalizer screen is open: shallow buffer, no bursts.
    pub tuning: bool,
    /// A rebuild waits for the next song boundary.
    pub swap_pending: bool,
    /// The buffer depth changes at the next pause.
    pub deep_at_next_pause: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainAct {
    Nothing,
    Rebuild,
}

impl Chain {
    /// The equalizer screen opened (`on`) or closed. `eq`: the equalizer is in the chain; `idle`: nothing
    /// loaded; `playing`: playback wanted.
    pub fn tuning(&mut self, on: bool, eq: bool, idle: bool, playing: bool) -> ChainAct {
        let entering = on && !self.tuning && eq;
        let leaving = !on && self.tuning;
        if !entering && !leaving {
            return ChainAct::Nothing;
        }
        self.tuning = entering;
        if leaving {
            self.deep_at_next_pause = true;
        }
        if idle {
            ChainAct::Nothing
        } else if playing {
            self.swap_pending = true;
            self.deep_at_next_pause = true;
            ChainAct::Nothing
        } else {
            ChainAct::Rebuild
        }
    }

    /// Defers a rebuild to the boundary; true if it was not already pending (log once).
    pub fn defer(&mut self) -> bool {
        !std::mem::replace(&mut self.swap_pending, true)
    }

    /// A song boundary. A repeat-one loop restarts seamlessly, so the swap waits for a real one. A mix
    /// planned into this song is lost to the rebuild.
    pub fn boundary(&mut self, repeat_one: bool) -> ChainAct {
        if self.swap_pending && !repeat_one {
            self.swap_pending = false;
            self.deep_at_next_pause = false;
            ChainAct::Rebuild
        } else {
            ChainAct::Nothing
        }
    }

    /// The user paused (not buffering): a pending depth change can happen now.
    pub fn paused(&mut self) -> ChainAct {
        if self.deep_at_next_pause {
            self.deep_at_next_pause = false;
            self.swap_pending = false;
            ChainAct::Rebuild
        } else {
            ChainAct::Nothing
        }
    }

    /// Feed in bursts: not while offloaded or tuning.
    pub fn bursting(&self, offloaded: bool) -> bool {
        !offloaded && !self.tuning
    }
}

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
    fn rebuild_waits_for_boundary_unless_broken() {
        assert_eq!(rebuild(ChainChange { processor_changed: true, ..Default::default() }), Rebuild::AtBoundary);
        assert_eq!(rebuild(ChainChange::default()), Rebuild::None);
        let silent = ChainChange { offloaded: true, offload: false, usb: true, ..Default::default() };
        assert_eq!(rebuild(silent), Rebuild::Now, "an offloaded song on usb plays silence");
        assert_eq!(rebuild(ChainChange { offloaded: true, offload: true, tempo_changed: true, ..Default::default() }), Rebuild::Now);
        assert_eq!(rebuild(ChainChange { offloaded: true, offload: false, processor_changed: true, ..Default::default() }), Rebuild::Now, "leaving offload");
        assert_eq!(rebuild(ChainChange { offloaded: false, offload: true, offload_changed: true, ..Default::default() }), Rebuild::None, "offload waits for its next song");
    }

    #[test]
    fn tuning_swap_waits_while_playing() {
        let mut c = Chain::default();
        assert_eq!(c.tuning(true, true, false, true), ChainAct::Nothing);
        assert!(c.tuning && c.swap_pending && c.deep_at_next_pause && !c.bursting(false));
        assert_eq!(c.boundary(true), ChainAct::Nothing, "a repeat-one loop restarts seamlessly");
        assert_eq!(c.boundary(false), ChainAct::Rebuild);
        assert_eq!(c.paused(), ChainAct::Nothing, "the rebuild carried the deep buffer too");
        assert_eq!(c.tuning(false, true, false, false), ChainAct::Rebuild, "paused: at once");
        assert!(c.deep_at_next_pause);
        assert_eq!(c.paused(), ChainAct::Rebuild);
        assert!(c.defer() && !c.defer(), "said once");
        let mut off = Chain::default();
        assert_eq!(off.tuning(true, false, false, true), ChainAct::Nothing, "no equalizer in the chain: nothing to tune");
        assert!(!off.tuning);
        let mut paused = Chain::default();
        assert_eq!(paused.tuning(true, true, false, false), ChainAct::Rebuild, "paused: at once");
        assert!(!paused.deep_at_next_pause && !paused.swap_pending);
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
