//! Perf build invariant watchdogs, checked on existing events (engine wakes, track writes, player and
//! UI callbacks; no timers) and reported as "invariant" timeline events and log lines. With the watch
//! off each hook costs one atomic read. Invariants while playing:
//!
//! - an output holding unplayed music advances within [`STILL_MS`] ("stalled", "offload-starved");
//! - an output is fed: not idle and unfed for [`STARVED_MS`] unless the engine is waiting ("starved");
//! - with no output open, the position does not stand still for [`SILENT_MS`] ("silent");
//! - one skip press moves one song;
//! - the shown song and lyrics are the heard song's, within [`DIFFER_MS`];
//! - the seek bar and controller positions are within [`PLACE_MS`] of the engine's;
//! - with AutoMix on, every queued song has a duration;
//! - a changed setting reaches the engine within a second (see [`settings_judged`]).
//!
//! [`Watch`] is the testable bookkeeping; the functions below drive one process-wide instance.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

/// An output presenting nothing new for longer than this has stalled.
pub(crate) const STILL_MS: i64 = 2_000;
/// An output neither presenting nor fed for longer than this is starved (longer than [`STILL_MS`] so
/// slightly late first bytes do not count).
pub(crate) const STARVED_MS: i64 = 5_000;
/// Position standing still this long with no output open and nothing downloading is "silent".
pub(crate) const SILENT_MS: i64 = 5_000;
/// Allowed lag of the screen behind playback.
pub(crate) const DIFFER_MS: i64 = 1_000;
/// Allowed seek bar / controller position error (for longer than [`DIFFER_MS`]).
pub(crate) const PLACE_MS: i64 = 2_000;
/// Skip presses closer than this form one run.
pub(crate) const RUN_MS: i64 = 3_000;
/// Settings are not judged for this long after the player service starts or ends.
pub const SETTLE_MS: i64 = 3_000;
/// Maximum breaks kept for the self test.
pub(crate) const MOST_BREAKS: usize = 50;

/// A violated invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Break {
    pub kind: &'static str,
    pub detail: String,
}

impl Break {
    fn new(kind: &'static str, detail: String) -> Break {
        Break { kind, detail }
    }

    /// "kind: detail", as logged.
    pub fn line(&self) -> String {
        format!("{}: {}", self.kind, self.detail)
    }
}

/// An output progress reading; `presented` and `written` count units at `rate` per second. A new `song`
/// or a count going back (flush) resets tracking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moving {
    pub now_ms: i64,
    pub playing: bool,
    pub offloaded: bool,
    pub song: Option<usize>,
    pub written: u64,
    pub presented: u64,
    pub rate: u32,
}

#[derive(Debug, Clone, Copy)]
struct Progress {
    song: Option<usize>,
    presented: u64,
    /// When `presented` last advanced.
    since_ms: i64,
    written: u64,
    /// When `written` last advanced.
    fed_ms: i64,
    said: bool,
}

impl Progress {
    fn new(m: &Moving) -> Progress {
        Progress { song: m.song, presented: m.presented, since_ms: m.now_ms, written: m.written, fed_ms: m.now_ms, said: false }
    }
}

/// A run of skip presses from queue index `from`.
#[derive(Debug, Clone, Copy)]
struct Skips {
    from: i64,
    presses: u32,
    last_ms: i64,
    said: bool,
}

/// Invariant bookkeeping. Methods take the current time (wall ms, or the output's clock for
/// [`Watch::output`]) and return a break when one is detected; each break is reported once.
#[derive(Default)]
pub struct Watch {
    outputs: Vec<(String, Progress)>,
    heard: Option<(String, i64)>,
    shown: Option<String>,
    differ_since: Option<i64>,
    differ_said: bool,
    /// The screen is not visible, so it is not compared (it does not update).
    hidden: bool,
    skips: Option<Skips>,
    queue_said: Vec<String>,
    bar_off: Off,
    word_off: Off,
    /// The engine reported not playing (waiting for data, a jump's dip, or paused): its track is not
    /// expected to be fed.
    engine_quiet: bool,
    /// "silent" was reported for the current silent stretch.
    silent_said: bool,
}

/// Tracks how long a position has been off, reporting once per episode.
#[derive(Default, Clone, Copy)]
struct Off {
    since: Option<i64>,
    said: bool,
}

impl Off {
    /// True once when `far` has held for longer than [`DIFFER_MS`].
    fn follow(&mut self, now: i64, far: bool) -> bool {
        if !far {
            *self = Off::default();
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if self.said || now - since <= DIFFER_MS {
            return false;
        }
        self.said = true;
        true
    }
}

impl Watch {
    /// Checks output `key` ("engine", "track") for "stalled" (holding unplayed music without advancing for
    /// [`STILL_MS`]) or "starved" (neither advancing nor fed for [`STARVED_MS`]).
    pub fn output(&mut self, key: &str, m: &Moving) -> Option<Break> {
        let i = match self.outputs.iter().position(|(k, _)| k == key) {
            Some(i) => i,
            None => {
                self.outputs.push((key.to_string(), Progress::new(m)));
                return None;
            }
        };
        let p = &mut self.outputs[i].1;
        if !m.playing || m.song != p.song || m.presented < p.presented || m.written < p.written {
            *p = Progress::new(m);
            return None;
        }
        if m.written > p.written {
            p.written = m.written;
            p.fed_ms = m.now_ms;
        }
        if m.presented > p.presented {
            *p = Progress { presented: m.presented, since_ms: m.now_ms, said: false, ..*p };
            return None;
        }
        let still = m.now_ms - p.since_ms;
        let unfed = m.now_ms - p.fed_ms;
        if p.said {
            return None;
        }
        let ms = |n: u64| n as i128 * 1000 / m.rate.max(1) as i128;
        let what = if m.offloaded { "the offloaded track" } else { "the output" };
        if still > STILL_MS && m.written > m.presented {
            p.said = true;
            let held = ms(m.written - m.presented);
            let kind = if m.offloaded { "offload-starved" } else { "stalled" };
            return Some(Break::new(
                kind,
                format!("{key}: playing, but {what} presented nothing new for {still} ms, with {held} ms written and not presented (at {} ms of {} ms written)", ms(m.presented), ms(m.written)),
            ));
        }
        if still > STARVED_MS && unfed > STARVED_MS {
            p.said = true;
            return Some(Break::new(
                "starved",
                format!("{key}: playing, but {what} presented nothing new for {still} ms and was given nothing new for {unfed} ms (at {} ms presented of {} ms written)", ms(m.presented), ms(m.written)),
            ));
        }
        None
    }

    /// Records whether the engine is producing music at its latest wake.
    pub fn engine(&mut self, playing: bool) {
        if self.engine_quiet && playing {
            // Restart the track's tracking from its next reading.
            self.outputs.retain(|(k, _)| k != "track");
        }
        self.engine_quiet = !playing;
    }

    /// "silent": playing, position still for `quiet_ms` >= [`SILENT_MS`] with no output open. Reported once
    /// per stretch, quoting the engine `state`; the caller appends the stream cache.
    pub fn silent(&mut self, quiet_ms: i64, output_open: bool, song: Option<&str>, state: &str) -> Option<Break> {
        if quiet_ms < SILENT_MS || output_open {
            if quiet_ms == 0 {
                self.silent_said = false;
            }
            return None;
        }
        if self.silent_said {
            return None;
        }
        self.silent_said = true;
        let song = song.unwrap_or("no song");
        Some(Break::new(
            "silent",
            format!("playing, but {song} stood still for {quiet_ms} ms with no output open and none of its bytes on their way; the engine: {state}"),
        ))
    }

    /// [`Watch::output`] for the CPU track, only judged while the engine reports playing (waiting for data
    /// is buffering, not starvation).
    pub fn track(&mut self, m: &Moving) -> Option<Break> {
        let m = Moving { playing: m.playing && !self.engine_quiet, ..*m };
        self.output("track", &m)
    }

    /// The player service reports song `id` playing.
    pub fn heard(&mut self, now: i64, id: &str) -> Option<Break> {
        if self.heard.as_ref().is_none_or(|(h, _)| h != id) {
            self.heard = Some((id.to_string(), now));
        }
        self.compare(now)
    }

    /// The screen shows `id` (or nothing).
    pub fn shown(&mut self, now: i64, id: Option<&str>) -> Option<Break> {
        self.shown = id.map(str::to_string);
        self.compare(now)
    }

    /// Screen visibility. A mismatch is timed only while visible, from when it became visible.
    pub fn visible(&mut self, now: i64, on: bool) -> Option<Break> {
        if !on {
            self.hidden = true;
            self.differ_since = None;
            return None;
        }
        if self.hidden {
            self.hidden = false;
            self.differ_since = None;
        }
        self.compare(now)
    }

    /// "shown-heard": shown and heard songs differ for longer than [`DIFFER_MS`].
    pub fn compare(&mut self, now: i64) -> Option<Break> {
        let (Some((heard, _)), Some(shown)) = (&self.heard, &self.shown) else {
            self.differ_since = None;
            return None;
        };
        if self.hidden {
            return None;
        }
        if heard == shown {
            self.differ_since = None;
            self.differ_said = false;
            return None;
        }
        let since = *self.differ_since.get_or_insert(now);
        if self.differ_said || now - since <= DIFFER_MS {
            return None;
        }
        self.differ_said = true;
        Some(Break::new("shown-heard", format!("the screen showed {shown} while {heard} was heard, for {} ms", now - since)))
    }

    /// "place": while playing, the seek bar (`shown_ms`) or controller (`word_ms`) position is more than
    /// [`PLACE_MS`] from the engine's for over [`DIFFER_MS`]. `engine_ms` < 0: unknown, not judged.
    pub fn place(&mut self, now: i64, playing: bool, shown_ms: i64, engine_ms: i64, word_ms: i64) -> Option<Break> {
        let judged = playing && engine_ms >= 0;
        let bar = self.bar_off.follow(now, judged && (shown_ms - engine_ms).abs() > PLACE_MS);
        let word = self.word_off.follow(now, judged && (word_ms - engine_ms).abs() > PLACE_MS);
        let s = |ms: i64| format!("{:.1} s", ms as f64 / 1000.0);
        if bar {
            return Some(Break::new("place", format!("the seek bar showed {} while the engine was at {} (the controller's word {})", s(shown_ms), s(engine_ms), s(word_ms))));
        }
        word.then(|| Break::new("place", format!("the controller ran on to {} while the engine was at {} (the seek bar showed {})", s(word_ms), s(engine_ms), s(shown_ms))))
    }

    /// Next or previous pressed on queue index `index`.
    pub fn skip(&mut self, now: i64, index: i64) {
        match &mut self.skips {
            Some(s) if now - s.last_ms <= RUN_MS => {
                s.presses += 1;
                s.last_ms = now;
            }
            _ => self.skips = Some(Skips { from: index, presses: 1, last_ms: now, said: false }),
        }
    }

    /// "skip": a run of presses moved further than it was pressed. Not judged for `auto` advances or
    /// under shuffle.
    pub fn arrived(&mut self, now: i64, index: i64, auto: bool, shuffled: bool) -> Option<Break> {
        if auto || shuffled {
            self.skips = None;
            return None;
        }
        let s = self.skips.as_mut()?;
        if now - s.last_ms > RUN_MS {
            self.skips = None;
            return None;
        }
        let moved = (index - s.from).unsigned_abs();
        if moved <= s.presses as u64 || s.said {
            return None;
        }
        s.said = true;
        let presses = s.presses;
        Some(Break::new("skip", format!("{presses} skip press{} moved {moved} songs, from queue place {} to {index}", if presses == 1 { "" } else { "es" }, s.from)))
    }

    /// "lyrics": lyrics of another song than the heard one are shown.
    pub fn lyrics(&mut self, now: i64, id: &str) -> Option<Break> {
        let (heard, since) = self.heard.as_ref()?;
        // Grace period right after a song change.
        if heard == id || now - since <= DIFFER_MS {
            return None;
        }
        Some(Break::new("lyrics", format!("lyrics of {id} shown while {heard} is heard (since {} ms)", now - since)))
    }

    /// "queue-duration": AutoMix on and songs without a duration, reported once per set.
    pub fn queue(&mut self, auto_mix: bool, missing: &[String], total: u32) -> Option<Break> {
        if !auto_mix || missing.is_empty() {
            self.queue_said.clear();
            return None;
        }
        if self.queue_said == missing {
            return None;
        }
        self.queue_said = missing.to_vec();
        let named: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
        let more = if missing.len() > 5 { format!(" and {} more", missing.len() - 5) } else { String::new() };
        Some(Break::new("queue-duration", format!("AutoMix is on and {} of {total} songs in the queue have no length: {}{more}", missing.len(), named.join(", "))))
    }
}

/// Whether settings are judged now: playing through an open output and more than [`SETTLE_MS`] after
/// the player service started or ended (`engine_since`).
pub(crate) fn settings_judged(now: i64, playing: bool, output_open: bool, engine_since: Option<i64>) -> bool {
    playing && output_open && engine_since.is_none_or(|t| now - t > SETTLE_MS)
}

/// (name, expected, actual) pairs to check: offload always; the sound chain (required with the
/// equalizer on) only while CPU-decoded audio plays through the engine's output (`on_cpu`).
pub(crate) fn settings_pairs(eq_enabled: bool, want_offload: bool, offload_wanted: bool, chain_in: bool, on_cpu: bool) -> Vec<(&'static str, bool, bool)> {
    let mut pairs = vec![("offload wanted", want_offload, offload_wanted)];
    if eq_enabled && on_cpu {
        pairs.push(("sound chain in the path", true, chain_in));
    }
    pairs
}

/// "setting": a break listing each pair that disagrees.
pub(crate) fn settings_held(expected: &[(&str, bool, bool)]) -> Option<Break> {
    let off: Vec<String> = expected.iter().filter(|(_, want, got)| want != got).map(|(what, want, got)| format!("{what}: {got}, expected {want}")).collect();
    if off.is_empty() {
        return None;
    }
    Some(Break::new("setting", format!("a second after the settings changed the engine still shows {}", off.join("; "))))
}

// Process-wide watch: its entry points are FFI calls and engine hooks with no handle to carry state.

/// Whether the watch is on; every caller checks it first (one atomic read outside the perf build).
static ON: AtomicBool = AtomicBool::new(false);
/// Self-test volume factor for every output, as f32 bits.
static QUIET: AtomicU32 = AtomicU32::new(0x3F80_0000);
/// Platform hook describing what the stream cache holds for a song id.
static DISK: OnceLock<fn(&str) -> String> = OnceLock::new();
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// The process watch's state.
#[derive(Default)]
struct State {
    watch: Watch,
    /// Breaks said so far, for the self test (at most [`MOST_BREAKS`]).
    breaks: Vec<String>,
    /// What the engine's thread saw at its last wake.
    engine: Option<PerfEngineSeen>,
    /// The engine's own description of its state at its last wake, and when (wall ms).
    engine_state: (i64, String),
    /// When the player service last started or ended (wall ms).
    engine_since: Option<i64>,
}

impl State {
    /// Appends the engine's last self-description to an output break.
    fn quote_engine(&self, mut b: Break, now: i64) -> Break {
        let (at, state) = &self.engine_state;
        if state.is_empty() {
            b.detail.push_str("; the engine has said nothing yet");
        } else {
            b.detail.push_str(&format!("; the engine at its last wake, {} ms before: {state}", now - at));
        }
        b
    }
}

/// Whether the watch is on.
#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

pub(crate) fn wall_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Runs `f` on the process state, ignoring poisoning: the watch must never stop the app.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(State::default))
}

fn with_watch<R>(f: impl FnOnce(&mut Watch) -> R) -> R {
    with_state(|s| f(&mut s.watch))
}

/// Reports a break: log, perf timeline, recent log lines, and the self test's list.
fn said(t: i64, b: Option<Break>) {
    let Some(b) = b else { return };
    let line = b.line();
    nori_model::alog::info(&format!("invariant: {line}"));
    crate::perf_log::note_invariant(t, &line);
    crate::perf_log::keep_break_log(t, &line);
    let kept = format!("{} invariant: {line}", crate::perf_log::clock(t));
    with_state(|s| {
        if s.breaks.len() >= MOST_BREAKS {
            s.breaks.remove(0);
        }
        s.breaks.push(kept);
    });
}

/// Records the track's account of the equalizer screen's shallow buffer as a "tuning" timeline event.
pub fn tuning_said(detail: &str) {
    if on() {
        crate::perf_log::note_output(wall_ms(), "tuning", detail);
    }
}

/// Switches the watch on (perf build start) or off.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

/// The engine thread's last observation, read by the self test.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfEngineSeen {
    pub wall_ms: i64,
    pub playing: bool,
    pub offloaded: bool,
    /// Queue index heard, -1 for none (FFI record).
    pub index: i64,
    pub position_ms: i64,
    pub in_output_ms: i64,
}

/// Installs the platform's stream cache description hook, quoted by "silent" breaks.
pub fn describe_disk(describe: fn(&str) -> String) {
    let _ = DISK.set(describe);
}

fn disk_of(id: Option<&str>) -> String {
    match (id, DISK.get()) {
        (Some(id), Some(describe)) => describe(id),
        (None, _) => "no song".into(),
        (_, None) => "not known here".into(),
    }
}

/// One engine wake (nori-engine's `watch::Seen`, forwarded by the Android library). `quiet_ms`: how
/// long the playback position has not moved while it should; `state`: the engine's self-description.
#[derive(Debug, Clone, Default)]
pub struct EngineLook<'a> {
    pub now_ms: i64,
    pub playing: bool,
    pub offloaded: bool,
    pub index: Option<usize>,
    pub id: Option<&'a str>,
    pub position_ms: i64,
    pub in_output_ms: i64,
    pub quiet_ms: i64,
    pub output_open: bool,
    pub state: &'a str,
}

/// An engine wake: keeps its state for quoting, checks [`Watch::silent`], and watches the offloaded
/// output (the CPU track is watched by its writer, [`track_seen`]).
pub fn engine_seen(l: &EngineLook) {
    let EngineLook { now_ms, playing, offloaded, index, position_ms, in_output_ms, state, .. } = *l;
    let t = wall_ms();
    let (silent, output) = with_state(|s| {
        s.engine = Some(PerfEngineSeen { wall_ms: t, playing, offloaded, index: index.map_or(-1, |i| i as i64), position_ms, in_output_ms });
        s.watch.engine(playing);
        s.engine_state.0 = t;
        s.engine_state.1.clear();
        s.engine_state.1.push_str(state);
        let silent = s.watch.silent(l.quiet_ms, l.output_open, l.id, state);
        let output = if offloaded {
            let pos = position_ms.max(0) as u64;
            let m = Moving { now_ms, playing, offloaded, song: index, written: pos + in_output_ms.max(0) as u64, presented: pos, rate: 1000 };
            s.watch.output("engine", &m).map(|b| s.quote_engine(b, t))
        } else {
            None
        };
        (silent, output)
    });
    // The platform hook calls into Kotlin, so it runs with the state unlocked.
    let silent = silent.map(|mut b| {
        b.detail = format!("{}; the stream cache: {}", b.detail, disk_of(l.id));
        b
    });
    said(t, silent);
    said(t, output);
}

/// Reports a panic (from the platform's panic hook) as a break, when the watch is on.
pub fn panicked(thread: &str, what: &str) {
    if !on() {
        return;
    }
    said(wall_ms(), Some(Break::new("panic", format!("on {thread}: {what}"))));
}

/// The engine thread's last observation; None before its first wake with the watch on.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_engine_seen() -> Option<PerfEngineSeen> {
    with_state(|s| s.engine)
}

/// The AudioTrack writer's device reading: `written` and `presented` frames at `rate`, monotonic `now_ms`.
pub fn track_seen(now_ms: i64, playing: bool, written: u64, presented: u64, rate: u32) {
    let m = Moving { now_ms, playing, offloaded: false, song: None, written, presented, rate };
    let t = wall_ms();
    let b = with_state(|s| s.watch.track(&m).map(|b| s.quote_engine(b, t)));
    said(t, b);
}

/// The player service arrived on song `id`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_heard(wall_ms: i64, id: String) {
    if on() {
        said(wall_ms, with_watch(|w| w.heard(wall_ms, &id)));
    }
}

/// The player screen shows song `id`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_shown(wall_ms: i64, id: Option<String>) {
    if on() {
        said(wall_ms, with_watch(|w| w.shown(wall_ms, id.as_deref())));
    }
}

/// The screen became visible or hidden.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_visible(wall_ms: i64, visible: bool) {
    if on() {
        said(wall_ms, with_watch(|w| w.visible(wall_ms, visible)));
    }
}

/// Any other platform wake: compares screen and playback now.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_look(wall_ms: i64) {
    if on() {
        said(wall_ms, with_watch(|w| w.compare(wall_ms)));
    }
}

/// A seek bar draw (the caller checks [`on`]); see [`Watch::place`]. `now_ms` is monotonic.
pub fn place_seen(now_ms: i64, playing: bool, shown_ms: i64, engine_ms: i64, word_ms: i64) {
    let b = with_watch(|w| w.place(now_ms, playing, shown_ms, engine_ms, word_ms));
    if b.is_some() {
        said(wall_ms(), b);
    }
}

/// Next or previous pressed on queue index `index`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_skip(wall_ms: i64, index: i64) {
    if on() {
        with_watch(|w| w.skip(wall_ms, index));
    }
}

/// The player arrived on queue index `index`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_arrived(wall_ms: i64, index: i64, auto: bool, shuffled: bool) {
    if on() {
        said(wall_ms, with_watch(|w| w.arrived(wall_ms, index, auto, shuffled)));
    }
}

/// Lyrics of song `id` were shown.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_lyrics(wall_ms: i64, id: String) {
    if on() {
        said(wall_ms, with_watch(|w| w.lyrics(wall_ms, &id)));
    }
}

/// The queue changed: ids of songs without a duration, of `total`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_queue(wall_ms: i64, missing: Vec<String>, total: u32) {
    if !on() {
        return;
    }
    let auto_mix = nori_settings::settings_store::settings_current().is_some_and(|p| p.auto_mix);
    said(wall_ms, with_watch(|w| w.queue(auto_mix, &missing, total)));
}

/// A second after a settings change: checks the engine's offload request and sound chain against the
/// settings (`usb`: offload never applies), when [`settings_judged`] allows.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_settings(wall_ms: i64, offload_wanted: bool, chain_in: bool, on_cpu: bool, usb: bool, playing: bool, output_open: bool) {
    if !on() {
        return;
    }
    let since = with_state(|s| s.engine_since);
    if !settings_judged(wall_ms, playing, output_open, since) {
        return;
    }
    let Some(s) = nori_settings::settings_store::settings_current() else { return };
    let want_offload = s.offload && !usb && crate::perf_log::offload_reason().is_none();
    said(wall_ms, settings_held(&settings_pairs(s.eq_enabled, want_offload, offload_wanted, chain_in, on_cpu)));
}

/// The player service started or ended at `wall_ms`.
pub(crate) fn engine_changed(wall_ms: i64) {
    with_state(|s| s.engine_since = Some(wall_ms));
}

/// Breaks said so far, oldest first, as "21:05:12 invariant: kind: detail".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_invariant_breaks() -> Vec<String> {
    with_state(|s| s.breaks.clone())
}

/// Sets the self test's player volume factor (0..1) on every output; the system volume is untouched.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_quiet(level: f32) {
    QUIET.store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

/// The volume factor for every output: 1 outside the self test.
#[inline]
pub fn quiet() -> f32 {
    f32::from_bits(QUIET.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(now_ms: i64, presented: u64, written: u64) -> Moving {
        Moving { now_ms, playing: true, offloaded: false, song: Some(0), written, presented, rate: 1000 }
    }

    #[test]
    fn stalled_output_reported_once() {
        let mut w = Watch::default();
        assert_eq!(w.output("track", &at(0, 1000, 10_000)), None);
        assert_eq!(w.output("track", &at(1000, 2000, 10_000)), None, "it moved");
        assert_eq!(w.output("track", &at(2500, 2000, 10_000)), None, "still for 1.5 s: not yet");
        let b = w.output("track", &at(3100, 2000, 10_000)).expect("still for 2.1 s");
        assert_eq!(b.kind, "stalled");
        assert!(b.detail.contains("2100 ms") && b.detail.contains("8000 ms written and not presented"), "{}", b.detail);
        assert_eq!(w.output("track", &at(9000, 2000, 10_000)), None, "said once");
        assert_eq!(w.output("track", &at(9100, 2100, 10_000)), None, "moving again");
        assert!(w.output("track", &at(11_200, 2100, 10_000)).is_some(), "a second stall is said again");
    }

    #[test]
    fn starved_output_reported_once() {
        let mut w = Watch::default();
        w.output("track", &at(0, 5000, 5000));
        assert_eq!(w.output("track", &at(4000, 5000, 5000)), None, "a song's first bytes a moment late");
        let b = w.output("track", &at(5100, 5000, 5000)).expect("given nothing for 5.1 s");
        assert_eq!(b.kind, "starved");
        assert_eq!(b.detail, "track: playing, but the output presented nothing new for 5100 ms and was given nothing new for 5100 ms (at 5000 ms presented of 5000 ms written)");
        assert_eq!(w.output("track", &at(9000, 5000, 5000)), None, "said once");
        // Presented count past written (after a flush), never moving: starved.
        let mut w = Watch::default();
        w.output("track", &at(0, 90_000, 100));
        assert_eq!(w.output("track", &at(3000, 90_000, 100)), None);
        assert_eq!(w.output("track", &at(5500, 90_000, 100)).map(|b| b.kind), Some("starved"));
        // Fed but not playing: stalled.
        let mut w = Watch::default();
        w.output("track", &at(0, 0, 100));
        assert_eq!(w.output("track", &at(2500, 0, 200)).map(|b| b.kind), Some("stalled"));
    }

    #[test]
    fn track_not_starved_while_engine_waits() {
        // Regression: a provider song still downloading was reported as starved.
        let mut w = Watch::default();
        w.engine(false);
        w.track(&at(0, 0, 0));
        assert_eq!(w.track(&at(5_019, 0, 0)), None);
        assert_eq!(w.track(&at(12_000, 0, 0)), None);
        // Once playing, the full STARVED_MS counts from then.
        w.engine(true);
        assert_eq!(w.track(&at(13_000, 0, 0)), None);
        assert_eq!(w.track(&at(17_500, 0, 0)), None);
        let b = w.track(&at(18_100, 0, 0)).expect("starved");
        assert_eq!(b.kind, "starved");
        let mut w = Watch::default();
        w.engine(true);
        w.track(&at(0, 5000, 5000));
        assert_eq!(w.track(&at(5_100, 5000, 5000)).map(|b| b.kind), Some("starved"));
        // Holding unplayed music once the engine plays: stalled.
        let mut w = Watch::default();
        w.engine(false);
        w.track(&at(0, 0, 100));
        w.engine(true);
        w.track(&at(100, 0, 100));
        assert_eq!(w.track(&at(2_500, 0, 200)).map(|b| b.kind), Some("stalled"));
    }

    #[test]
    fn silent_reported_once_per_stretch() {
        let mut w = Watch::default();
        assert!(w.silent(SILENT_MS - 1, false, Some("s1"), "Playing").is_none());
        assert!(w.silent(9_000, true, Some("s1"), "Playing").is_none(), "output open");
        let b = w.silent(SILENT_MS, false, Some("s1"), "Playing; loaders: no loaders").expect("silent");
        assert_eq!(b.kind, "silent");
        assert!(b.detail.contains("s1 stood still for 5000 ms") && b.detail.ends_with("the engine: Playing; loaders: no loaders"), "{}", b.detail);
        assert!(w.silent(8_000, false, Some("s1"), "Playing").is_none(), "said once");
        // Moving again resets.
        assert!(w.silent(0, false, Some("s1"), "Playing").is_none());
        assert!(w.silent(6_000, false, Some("s2"), "Playing").is_some());
    }

    /// The tests that go through the global watch state.
    static GLOBAL: Mutex<()> = Mutex::new(());

    #[test]
    fn disk_hook_runs_outside_state_lock() {
        let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        // The Android hook calls into Kotlin, which may read the watch.
        describe_disk(|id| format!("{id}: {} breaks", perf_invariant_breaks().len()));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            engine_seen(&EngineLook { playing: true, id: Some("hooked"), quiet_ms: SILENT_MS, state: "Playing", ..EngineLook::default() });
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).expect("engine_seen deadlocked on its own hook");
        assert!(perf_invariant_breaks().iter().any(|l| l.contains("hooked stood still") && l.contains("the stream cache: hooked: ")));
    }

    #[test]
    fn track_break_quotes_engine_state() {
        let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let state = "Playing; playing on 16 (s16) at 51 ms; reading 16 (s16) at 11000 ms; transition engine passing; loaders: s16: 0..90 of 90 bytes";
        engine_seen(&EngineLook { playing: true, index: Some(16), position_ms: 51, in_output_ms: 100, state, ..EngineLook::default() });
        track_seen(0, true, 4410, 90_000, 44_100);
        track_seen(6_000, true, 4410, 90_000, 44_100);
        let breaks = perf_invariant_breaks();
        let line = breaks.iter().find(|l| l.contains("starved: track:")).unwrap_or_else(|| panic!("a starved track: {breaks:?}"));
        assert!(line.contains("; the engine at its last wake, ") && line.ends_with(state), "{line}");
    }

    #[test]
    fn standing_still_is_fine_when_paused_flushed_or_drained() {
        let mut w = Watch::default();
        w.output("track", &at(0, 5000, 5000));
        assert_eq!(w.output("track", &at(4500, 5000, 5000)), None, "drained");
        let paused = Moving { playing: false, ..at(10_000, 5000, 9000) };
        assert_eq!(w.output("track", &paused), None);
        assert_eq!(w.output("track", &at(11_000, 5000, 9000)), None, "pause resets");
        assert_eq!(w.output("track", &at(12_000, 0, 4000)), None, "flush resets");
        assert_eq!(w.output("track", &at(13_500, 0, 4000)), None);
        let song = Moving { song: Some(1), ..at(16_000, 0, 4000) };
        assert_eq!(w.output("track", &song), None, "new song resets");
    }

    #[test]
    fn offloaded_stall_is_offload_starved() {
        let mut w = Watch::default();
        let m = |t, p| Moving { offloaded: true, ..at(t, p, 20_000) };
        w.output("engine", &m(0, 900));
        let b = w.output("engine", &m(2600, 900)).unwrap();
        assert_eq!(b.kind, "offload-starved");
        assert!(b.detail.starts_with("engine: playing, but the offloaded track"), "{}", b.detail);
        assert_eq!(w.output("track", &at(0, 1, 2)), None, "outputs are independent");
    }

    #[test]
    fn skip_run_moves_one_song_per_press() {
        let mut w = Watch::default();
        w.skip(0, 4);
        assert_eq!(w.arrived(100, 5, false, false), None);
        w.skip(200, 5);
        w.skip(350, 6);
        assert_eq!(w.arrived(400, 6, false, false), None);
        assert_eq!(w.arrived(500, 7, false, false), None, "3 presses, 3 songs");
        let b = w.arrived(700, 8, false, false).expect("4 songs for 3 presses");
        assert_eq!(b.kind, "skip");
        assert_eq!(b.detail, "3 skip presses moved 4 songs, from queue place 4 to 8");
        assert_eq!(w.arrived(800, 9, false, false), None, "once per run");
    }

    #[test]
    fn auto_advance_and_shuffle_are_not_judged() {
        let mut w = Watch::default();
        w.skip(0, 0);
        assert_eq!(w.arrived(100, 3, false, true), None, "shuffled");
        w.skip(1000, 3);
        assert_eq!(w.arrived(1100, 4, true, false), None);
        assert_eq!(w.arrived(1200, 6, false, false), None, "auto advance ended the run");
        w.skip(10_000, 6);
        assert_eq!(w.arrived(14_000, 9, false, false), None, "after RUN_MS");
        w.skip(20_000, 9);
        assert_eq!(w.arrived(20_100, 8, false, false), None, "previous");
    }

    #[test]
    fn screen_may_lag_playback_by_differ_ms() {
        let mut w = Watch::default();
        assert_eq!(w.heard(0, "a"), None);
        assert_eq!(w.shown(10, Some("a")), None);
        assert_eq!(w.heard(1000, "b"), None);
        assert_eq!(w.compare(1900), None, "0.9 s");
        assert_eq!(w.shown(1950, Some("b")), None);
        assert_eq!(w.heard(5000, "c"), None);
        let b = w.compare(6100).expect("1.1 s");
        assert_eq!(b.kind, "shown-heard");
        assert_eq!(b.detail, "the screen showed b while c was heard, for 1100 ms");
        assert_eq!(w.compare(9000), None, "once");
        assert_eq!(w.shown(9100, Some("c")), None);
    }

    #[test]
    fn seek_bar_off_for_over_a_second_is_a_break() {
        let mut w = Watch::default();
        // Regression: bar stuck at the song's end, 14 s ahead of the engine.
        assert_eq!(w.place(0, true, 600_000, 586_000, 600_000), None);
        assert_eq!(w.place(900, true, 600_000, 586_900, 600_000), None);
        let b = w.place(1_100, true, 600_000, 587_100, 600_000).expect("off > 1 s");
        assert_eq!(b.line(), "place: the seek bar showed 600.0 s while the engine was at 587.1 s (the controller's word 600.0 s)");
        assert_eq!(w.place(1_200, true, 600_000, 587_200, 600_000), None, "once");
        // Back in range resets.
        assert_eq!(w.place(1_300, true, 587_300, 587_300, 587_300), None);
        w.place(2_000, true, 590_000, 587_000, 587_000);
        assert!(w.place(3_100, true, 591_100, 588_100, 588_100).is_some());
    }

    #[test]
    fn controller_position_off_is_a_break() {
        let mut w = Watch::default();
        w.place(0, true, 586_000, 586_000, 600_000);
        let b = w.place(1_500, true, 587_500, 587_500, 600_000).expect("off 1.5 s");
        assert_eq!(b.detail, "the controller ran on to 600.0 s while the engine was at 587.5 s (the seek bar showed 587.5 s)");
    }

    #[test]
    fn place_judged_only_playing_with_known_engine_position() {
        let mut w = Watch::default();
        for t in (0..5_000).step_by(100) {
            assert_eq!(w.place(t, false, 600_000, 100_000, 600_000), None, "paused");
            assert_eq!(w.place(t, true, 600_000, -1, 600_000), None, "engine position unknown");
        }
        // Within PLACE_MS: never a break.
        for t in (5_000..10_000).step_by(100) {
            assert_eq!(w.place(t, true, 100_000 + t, 100_000 + t - PLACE_MS, 100_000 + t), None);
        }
    }

    #[test]
    fn hidden_screen_is_not_judged() {
        let mut w = Watch::default();
        w.heard(0, "a");
        w.shown(10, Some("a"));
        assert_eq!(w.visible(20, false), None);
        w.heard(1000, "b");
        assert_eq!(w.compare(60_000), None, "hidden");
        assert_eq!(w.visible(64_000, true), None, "grace starts on becoming visible");
        assert_eq!(w.compare(64_900), None);
        let b = w.compare(65_100).expect("1.1 s after becoming visible");
        assert_eq!(b.detail, "the screen showed a while b was heard, for 1100 ms");
        // Catching up in time: nothing.
        let mut w = Watch::default();
        w.heard(0, "a");
        w.shown(10, Some("a"));
        w.visible(20, false);
        w.heard(1000, "b");
        w.visible(64_000, true);
        assert_eq!(w.shown(64_300, Some("b")), None);
        assert_eq!(w.compare(70_000), None);
    }

    #[test]
    fn lyrics_must_match_heard_song() {
        let mut w = Watch::default();
        assert_eq!(w.lyrics(0, "a"), None);
        w.heard(1000, "a");
        assert_eq!(w.lyrics(1500, "a"), None);
        w.heard(2000, "b");
        assert_eq!(w.lyrics(2500, "a"), None, "grace after song change");
        let b = w.lyrics(4000, "a").unwrap();
        assert_eq!(b.kind, "lyrics");
        assert_eq!(b.detail, "lyrics of a shown while b is heard (since 2000 ms)");
    }

    #[test]
    fn automix_requires_durations() {
        let mut w = Watch::default();
        let missing = vec!["x".to_string(), "y".to_string()];
        assert_eq!(w.queue(false, &missing, 10), None);
        let b = w.queue(true, &missing, 10).unwrap();
        assert_eq!(b.detail, "AutoMix is on and 2 of 10 songs in the queue have no length: x, y");
        assert_eq!(w.queue(true, &missing, 10), None, "once per set");
        assert_eq!(w.queue(true, &[], 10), None);
        assert!(w.queue(true, &missing, 10).is_some(), "again after being complete");
    }

    #[test]
    fn settings_judged_conditions() {
        assert!(!settings_judged(10_000, false, true, Some(0)), "paused");
        assert!(!settings_judged(10_000, true, false, Some(0)), "no output");
        assert!(!settings_judged(SETTLE_MS, true, true, Some(0)), "settling");
        assert!(settings_judged(SETTLE_MS + 1, true, true, Some(0)));
        assert!(settings_judged(5, true, true, None));
    }

    #[test]
    fn settings_held_reports_mismatches() {
        assert_eq!(settings_held(&[("offload wanted", true, true)]), None);
        let b = settings_held(&[("offload wanted", false, true), ("sound chain in the path", true, true)]).unwrap();
        assert_eq!(b.kind, "setting");
        assert_eq!(b.detail, "a second after the settings changed the engine still shows offload wanted: true, expected false");
    }

    #[test]
    fn sound_chain_checked_only_on_cpu_output() {
        // Equalizer on but not playing CPU audio: not judged.
        assert_eq!(settings_held(&settings_pairs(true, false, false, false, false)), None);
        let b = settings_held(&settings_pairs(true, false, false, false, true)).unwrap();
        assert_eq!(b.detail, "a second after the settings changed the engine still shows sound chain in the path: false, expected true");
        assert_eq!(settings_held(&settings_pairs(false, false, false, false, true)), None, "equalizer off");
        assert!(settings_held(&settings_pairs(true, false, true, false, false)).is_some(), "offload always judged");
    }

    #[test]
    fn quiet_is_clamped() {
        perf_quiet(0.001);
        assert!((quiet() - 0.001).abs() < 1e-6);
        perf_quiet(3.0);
        assert_eq!(quiet(), 1.0);
    }
}
