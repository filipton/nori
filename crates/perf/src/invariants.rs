//! The perf build's invariant watchdogs: things that must always hold while the app plays, checked as
//! the app's own events and wakes go by, and said loudly on the perf timeline ("invariant" events, which
//! the report lists first) and in the log when one does not.
//!
//! Nothing here ticks. Each check is made when something already happened: the engine's thread woke
//! (nori-engine's watch hook), the AudioTrack's writer woke to top the track up, the player service said
//! a song or a skip, the settings changed, the screen was told a song. With the watch off (every build
//! but the perf one) each of those costs one atomic read. What holds:
//!
//! - an output that holds music plays it: while playing, the frames it presented move on within
//!   [`STILL_MS`] (an offloaded track that does not is starved: the S22's silent offload);
//! - an output is given music while it plays: presenting nothing new and given nothing new for
//!   [`STARVED_MS`] is the music stopped while the player says it plays (the S22's silence after next was
//!   pressed fast and long) - but not while the engine says it waits: for a song's bytes (a provider's
//!   song still coming, said to be buffering), or out a jump's dip. Either break quotes the engine's own
//!   account of where it stood at its last wake (the song, what it reads and waits for, the transition
//!   engine, every loader);
//! - one press of a skip moves one song;
//! - the song on the screen is the one heard, give or take [`DIFFER_MS`];
//! - the lyrics shown are the song heard's;
//! - playing, the seek bar's place is the engine's, give or take [`PLACE_MS`], and so is the place a
//!   controller runs on from (the S22's bar at the end of a song with 14 s left, after the phone was
//!   unlocked: a controller's word, taken ahead and never put right);
//! - with AutoMix on, every song in the queue has a length (the planner has nothing to plan from without);
//! - a setting changed is in the engine a second later, judged only while it plays through an open output
//!   and [`SETTLE_MS`] after the player service started or ended (an engine switch restarts it).
//!
//! [`Watch`] is the bookkeeping, plain and testable; the functions below keep one for the process and are
//! what the engine's hook, the track and the platform call.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};

use std::sync::{Mutex, MutexGuard};

/// Playing, an output that presents nothing new for longer than this has stopped.
pub const STILL_MS: i64 = 2_000;
/// Playing, an output that presents nothing new and is given nothing new for longer than this was left
/// without music: longer than [`STILL_MS`], so a song's first bytes a moment late are not a break.
pub const STARVED_MS: i64 = 5_000;
/// Playing, the place heard standing still this long with no output open and no song's bytes on their
/// way is the engine playing nothing while it says it plays (the S22's classical playlist, 2026-09-26).
pub const SILENT_MS: i64 = 5_000;
/// The screen may trail the ear by this much at a song change.
pub const DIFFER_MS: i64 = 1_000;
/// Playing, the seek bar's place (or a controller's) further than this from the engine's own for longer
/// than [`DIFFER_MS`] is a break.
pub const PLACE_MS: i64 = 2_000;
/// Presses closer together than this are one run of skips.
pub const RUN_MS: i64 = 3_000;
/// After the player service starts or ends (an engine switch does both) its settings are not judged for
/// this long: the service is still building its player, and a batch of settings arrived with it.
pub const SETTLE_MS: i64 = 3_000;
/// The most breaks kept for the self test to read back.
pub const MOST_BREAKS: usize = 50;

/// One invariant that did not hold: which, and what was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Break {
    pub kind: &'static str,
    pub detail: String,
}

impl Break {
    fn new(kind: &'static str, detail: String) -> Break {
        Break { kind, detail }
    }

    /// "offload-starved: ..." as the timeline and the log say it.
    pub fn line(&self) -> String {
        format!("{}: {}", self.kind, self.detail)
    }
}

/// How far an output has come, as one reading: `presented` and `written` in units of which there are
/// `rate` a second (frames, or ms). A new `song` (or a count that went back: a flush) starts afresh.
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
    /// When the count of what it presented last moved.
    since_ms: i64,
    written: u64,
    /// When it was last given anything new.
    fed_ms: i64,
    said: bool,
}

impl Progress {
    fn new(m: &Moving) -> Progress {
        Progress { song: m.song, presented: m.presented, since_ms: m.now_ms, written: m.written, fed_ms: m.now_ms, said: false }
    }
}

/// A run of skips: the song it started from, the presses since and when the last was.
#[derive(Debug, Clone, Copy)]
struct Skips {
    from: i64,
    presses: u32,
    last_ms: i64,
    said: bool,
}

/// The watch's bookkeeping. Every method takes the time it is told at (wall clock, ms, or the output's
/// own clock for [`Watch::output`]) and answers the invariant that did not hold, if one did not.
#[derive(Default)]
pub struct Watch {
    outputs: Vec<(String, Progress)>,
    heard: Option<(String, i64)>,
    shown: Option<String>,
    differ_since: Option<i64>,
    differ_said: bool,
    /// Nobody can see the screen (the app is in the background, or the screen is off): what it shows is
    /// not compared, since nothing on it is drawn or brought up to date until it is seen again.
    hidden: bool,
    skips: Option<Skips>,
    queue_said: Vec<String>,
    /// Since when the bar's place, and a controller's, have been far from the engine's, and whether that
    /// was said.
    bar_off: Off,
    word_off: Off,
    /// The engine said at its last wake that it plays nothing now, though its track may be started: it
    /// waits for a song's bytes, or out a jump's dip, or it is paused. Its CPU track is not expected to
    /// be given anything meanwhile.
    engine_quiet: bool,
    /// The engine playing nothing with no output open was said, for the stretch of silence under way.
    silent_said: bool,
}

/// A place far from the engine's: since when, and whether it was said.
#[derive(Default, Clone, Copy)]
struct Off {
    since: Option<i64>,
    said: bool,
}

impl Off {
    /// Whether a place `far` from the engine's at `now` has been so for longer than [`DIFFER_MS`], said
    /// once for each time it goes far.
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
    /// An output's reading, under `key` ("engine", "track"): playing, its count of what it presented must
    /// move while it holds music written and not presented ("stalled"), and it must be given music to
    /// play: presenting nothing new for [`STARVED_MS`] while nothing new is written to it either is the
    /// music stopped with the player saying it plays ("starved": the engine gave the output nothing, or
    /// the output took nothing more; a device whose count stopped past what it was given falls here too).
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

    /// The engine woke and said whether it plays: music it makes reaching its output, not paused, not
    /// waiting for a song's bytes nor out a jump's dip.
    pub fn engine(&mut self, playing: bool) {
        if self.engine_quiet && playing {
            // The track's count starts again from its next reading.
            self.outputs.retain(|(k, _)| k != "track");
        }
        self.engine_quiet = !playing;
    }

    /// The engine's own look at whether the music moves: playing, its place heard has stood still for
    /// `quiet_ms` with no song's bytes on their way (the engine counts none while they are). With no
    /// output open either for [`SILENT_MS`], nothing can be heard and nothing is coming: said once for each
    /// stretch of it, with `state`, the engine's own account, and `disk`, what the stream cache keeps of the
    /// song.
    pub fn silent(&mut self, quiet_ms: i64, output_open: bool, song: Option<&str>, state: &str, disk: impl FnOnce() -> String) -> Option<Break> {
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
            format!("playing, but {song} stood still for {quiet_ms} ms with no output open and none of its bytes on their way; the engine: {state}; the stream cache: {}", disk()),
        ))
    }

    /// The CPU's track, fed by the engine: [`Watch::output`], watched only while the engine says it plays.
    /// Given nothing while it waits for a song's bytes is buffering, which the player says, not music
    /// stopped behind its back; once it says it plays again, the output has the whole [`STARVED_MS`] from
    /// then to be given something.
    pub fn track(&mut self, m: &Moving) -> Option<Break> {
        let m = Moving { playing: m.playing && !self.engine_quiet, ..*m };
        self.output("track", &m)
    }

    /// The song heard changed to `id` (the player service's word).
    pub fn heard(&mut self, now: i64, id: &str) -> Option<Break> {
        if self.heard.as_ref().is_none_or(|(h, _)| h != id) {
            self.heard = Some((id.to_string(), now));
        }
        self.compare(now)
    }

    /// The screen shows `id` now (none: nothing).
    pub fn shown(&mut self, now: i64, id: Option<&str>) -> Option<Break> {
        self.shown = id.map(str::to_string);
        self.compare(now)
    }

    /// Whether the screen can be seen: the app in the foreground with the screen on. A difference is only
    /// counted while it can, and from the moment it came back, so the time it spent off is not the
    /// screen's lateness, and it has the same [`DIFFER_MS`] to catch up as after any change of song.
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

    /// Whether the screen and the ear agree, looked at now: a difference said once, when it has lasted
    /// longer than [`DIFFER_MS`]. The player says itself which song is heard, a mix's too, so the screen
    /// follows the ear through one with no grace of its own.
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

    /// The seek bar was drawn at `now` (ms, any clock that runs on): it showed `shown_ms`, the engine's own
    /// place was `engine_ms` (negative: none to go by - another song, a seek on its way) and the
    /// controller's `word_ms`. Judged only while `playing` with the page on the player's song. Either place
    /// further than [`PLACE_MS`] from the engine's for longer than [`DIFFER_MS`] is a break, said once.
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

    /// The user pressed next or previous on the song at `index` (its place in the queue).
    pub fn skip(&mut self, now: i64, index: i64) {
        match &mut self.skips {
            Some(s) if now - s.last_ms <= RUN_MS => {
                s.presses += 1;
                s.last_ms = now;
            }
            _ => self.skips = Some(Skips { from: index, presses: 1, last_ms: now, said: false }),
        }
    }

    /// The player arrived on the song at `index`: by itself (`auto`, a song that ended), or by a jump.
    /// A run of skips that moved further than it was pressed is a break. Under shuffle the places in the
    /// queue say nothing of how far the order moved, and nothing is judged.
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

    /// Lyrics of the song `id` went up on the screen.
    pub fn lyrics(&mut self, now: i64, id: &str) -> Option<Break> {
        let (heard, since) = self.heard.as_ref()?;
        // Shown just as the song changed: the screen follows a moment later, and takes them down.
        if heard == id || now - since <= DIFFER_MS {
            return None;
        }
        Some(Break::new("lyrics", format!("lyrics of {id} shown while {heard} is heard (since {} ms)", now - since)))
    }

    /// The queue as it is now: with AutoMix on, the songs in it without a length (`missing`, ids) of
    /// `total`. Said once for each set of songs.
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

/// Whether the engine can be held to the settings now: only while it plays through an open output (paused,
/// or with the output let go, the sound chain is not in any path) and not within [`SETTLE_MS`] of the
/// player service starting or ending (`engine_since`).
pub fn settings_judged(now: i64, playing: bool, output_open: bool, engine_since: Option<i64>) -> bool {
    playing && output_open && engine_since.is_none_or(|t| now - t > SETTLE_MS)
}

/// The settings' pairs the engine is held to now: whether it wants offload, always; whether the sound
/// chain is in the samples' path (it must be with the equalizer on) only while the ear is on music the CPU
/// made through the engine's own output (`on_cpu`): offloaded, let go, or waiting for a song's bytes, the
/// chain is in no path, and the engine does not say where it is.
pub fn settings_pairs(eq_enabled: bool, want_offload: bool, offload_wanted: bool, chain_in: bool, on_cpu: bool) -> Vec<(&'static str, bool, bool)> {
    let mut pairs = vec![("offload wanted", want_offload, offload_wanted)];
    if eq_enabled && on_cpu {
        pairs.push(("sound chain in the path", true, chain_in));
    }
    pairs
}

/// A setting a second after it changed, against what the engine shows: each pair that disagrees.
pub fn settings_held(expected: &[(&str, bool, bool)]) -> Option<Break> {
    let off: Vec<String> = expected.iter().filter(|(_, want, got)| want != got).map(|(what, want, got)| format!("{what}: {got}, expected {want}")).collect();
    if off.is_empty() {
        return None;
    }
    Some(Break::new("setting", format!("a second after the settings changed the engine still shows {}", off.join("; "))))
}

// ---- the process's watch ----

static ON: AtomicBool = AtomicBool::new(false);
static WATCH: Mutex<Option<Watch>> = Mutex::new(None);
static BREAKS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ENGINE: Mutex<Option<PerfEngineSeen>> = Mutex::new(None);
/// The engine's own words for where it stood at its last wake, and when (wall ms).
static ENGINE_STATE: Mutex<(i64, String)> = Mutex::new((0, String::new()));
/// The self test's volume on every output, as f32 bits: 1 unless it is running quietly.
/// When the player service last started or ended, wall ms; `i64::MIN` for never.
static ENGINE_SINCE: AtomicI64 = AtomicI64::new(i64::MIN);
static QUIET: AtomicU32 = AtomicU32::new(0x3F80_0000);

/// Whether the watch is on: one atomic read, for every caller to ask first.
#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// A lock that a panic while it was held does not poison for good: the watch must never stop the app.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn with<R>(f: impl FnOnce(&mut Watch) -> R) -> R {
    f(lock(&WATCH).get_or_insert_with(Watch::default))
}

/// A break goes on the timeline and in the log, loudly, and is kept for the self test.
fn said(t: i64, b: Option<Break>) {
    let Some(b) = b else { return };
    let line = b.line();
    nori_model::alog::info(&format!("invariant: {line}"));
    crate::perf_log::note_invariant(t, &line);
    // What the app said as it broke, kept before logcat turns it over.
    crate::perf_log::keep_break_log(t, &line);
    let mut kept = lock(&BREAKS);
    if kept.len() >= MOST_BREAKS {
        kept.remove(0);
    }
    kept.push(format!("{} invariant: {line}", crate::perf_log::clock_words(t)));
}

/// The track's own account of the equalizer screen's shallow buffer (how deep it was made for the output
/// it plays on and why, and any growth after it ran dry): a "tuning" event on the perf timeline. Nothing
/// outside the perf build.
pub fn tuning_said(detail: &str) {
    if on() {
        crate::perf_log::note_output(wall_ms(), "tuning", detail);
    }
}

/// The perf build switches the watch on as it starts; nothing is watched before.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

/// What the engine's thread saw last, while the watch is on: for the self test, which reads the engine
/// through it rather than through doors of its own.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfEngineSeen {
    /// When, wall clock ms.
    pub wall_ms: i64,
    pub playing: bool,
    pub offloaded: bool,
    /// Queue index heard, -1 none.
    pub index: i64,
    pub position_ms: i64,
    pub in_output_ms: i64,
}

/// What the stream cache keeps of a song (by its id): whether it has an entry, its length, the spans
/// cached and the length its metadata gives. The platform's, told once ([`describe_disk`]).
static DISK: std::sync::OnceLock<fn(&str) -> String> = std::sync::OnceLock::new();

/// The platform says how to describe what its stream cache keeps of a song, for a silent break.
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

/// What the engine's thread saw at one wake (nori-engine's `watch::Seen`, handed on by the Android
/// library): whether it plays, which song and where, what the output holds, how long the music has stood
/// still while it should move (`quiet_ms`) and whether an output is open, and where it stands in its own
/// words (`state`).
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

/// The engine's thread woke (nori-engine's watch hook, through the Android library); its words for where
/// it stands are kept to be quoted by a break of the output it feeds. The engine playing nothing with no
/// output open is a break of its own ([`Watch::silent`]).
pub fn engine_seen(l: &EngineLook) {
    let EngineLook { now_ms, playing, offloaded, index, position_ms, in_output_ms, state, .. } = *l;
    let t = wall_ms();
    *lock(&ENGINE) = Some(PerfEngineSeen { wall_ms: t, playing, offloaded, index: index.map_or(-1, |i| i as i64), position_ms, in_output_ms });
    with(|w| w.engine(playing));
    {
        let mut kept = lock(&ENGINE_STATE);
        kept.0 = t;
        kept.1.clear();
        kept.1.push_str(state);
    }
    let b = with(|w| w.silent(l.quiet_ms, l.output_open, l.id, state, || disk_of(l.id)));
    said(t, b);
    // The CPU's output is watched where it is written (the track's own writer, which reads the device);
    // the engine's word counts for the offloaded one, whose play head only the engine reads.
    if !offloaded {
        return;
    }
    let pos = position_ms.max(0) as u64;
    let m = Moving { now_ms, playing, offloaded, song: index, written: pos + in_output_ms.max(0) as u64, presented: pos, rate: 1000 };
    let b = with(|w| w.output("engine", &m));
    said(t, b.map(|b| with_engine(b, t)));
}

/// A break of an output, with the engine's own account of where it stood at its last wake.
fn with_engine(mut b: Break, now: i64) -> Break {
    let kept = lock(&ENGINE_STATE);
    if kept.1.is_empty() {
        b.detail.push_str("; the engine has said nothing yet");
    } else {
        b.detail.push_str(&format!("; the engine at its last wake, {} ms before: {}", now - kept.0, kept.1));
    }
    b
}

/// A thread panicked (`thread`'s name, `what` and where): said as a break, with the app's own lines as it
/// happened. The platform's panic hook calls this for every panic, caught or not; nothing unless the
/// watch is on.
pub fn panicked(thread: &str, what: &str) {
    if !on() {
        return;
    }
    said(wall_ms(), Some(Break::new("panic", format!("on {thread}: {what}"))));
}

/// The last thing the engine's thread saw; none before it woke with the watch on.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_engine_seen() -> Option<PerfEngineSeen> {
    *lock(&ENGINE)
}

/// The AudioTrack's writer read the device: `written` frames handed to it and `presented` played, at
/// `rate` a second, `now_ms` by the monotonic clock.
pub fn track_seen(now_ms: i64, playing: bool, written: u64, presented: u64, rate: u32) {
    let m = Moving { now_ms, playing, offloaded: false, song: None, written, presented, rate };
    let b = with(|w| w.track(&m));
    let t = wall_ms();
    said(t, b.map(|b| with_engine(b, t)));
}

/// The player service arrived on song `id`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_heard(wall_ms: i64, id: String) {
    if !on() {
        return;
    }
    let b = with(|w| w.heard(wall_ms, &id));
    said(wall_ms, b);
}

/// The screen's player shows song `id` now.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_shown(wall_ms: i64, id: Option<String>) {
    if !on() {
        return;
    }
    let b = with(|w| w.shown(wall_ms, id.as_deref()));
    said(wall_ms, b);
}

/// The screen can be seen (the app in the foreground, the screen on), or no longer can.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_visible(wall_ms: i64, visible: bool) {
    if !on() {
        return;
    }
    let b = with(|w| w.visible(wall_ms, visible));
    said(wall_ms, b);
}

/// Anything else woke the platform's watcher: the screen and the ear compared at this moment too.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_look(wall_ms: i64) {
    if !on() {
        return;
    }
    let b = with(|w| w.compare(wall_ms));
    said(wall_ms, b);
}

/// The seek bar was drawn (the Android library's seek bar door, which asks [`on`] first): see
/// [`Watch::place`]. `now_ms` times how long a place stays off; the timeline gets the wall clock.
pub fn place_seen(now_ms: i64, playing: bool, shown_ms: i64, engine_ms: i64, word_ms: i64) {
    let b = with(|w| w.place(now_ms, playing, shown_ms, engine_ms, word_ms));
    if b.is_some() {
        said(wall_ms(), b);
    }
}

/// The user pressed next or previous on the song at queue place `index`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_skip(wall_ms: i64, index: i64) {
    if on() {
        with(|w| w.skip(wall_ms, index));
    }
}

/// The player arrived on queue place `index`, by itself (`auto`) or by a jump.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_arrived(wall_ms: i64, index: i64, auto: bool, shuffled: bool) {
    if !on() {
        return;
    }
    let b = with(|w| w.arrived(wall_ms, index, auto, shuffled));
    said(wall_ms, b);
}

/// Lyrics of song `id` went up on the screen.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_lyrics(wall_ms: i64, id: String) {
    if !on() {
        return;
    }
    let b = with(|w| w.lyrics(wall_ms, &id));
    said(wall_ms, b);
}

/// The queue changed: the ids of its songs without a length, of `total`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_queue(wall_ms: i64, missing: Vec<String>, total: u32) {
    if !on() {
        return;
    }
    let auto_mix = nori_settings::settings_store::settings_current().is_some_and(|p| p.auto_mix);
    let b = with(|w| w.queue(auto_mix, &missing, total));
    said(wall_ms, b);
}

/// A second after the settings changed: what the engine shows (whether it asks for offload, whether the
/// sound chain is in the samples' path) against what the settings say it should, over the output the
/// platform sees (`usb`: something USB attached, where offload never goes). Judged only as
/// [`settings_judged`] says: `playing` through an output that is open (`output_open`); the chain only while
/// the engine says the ear is on music the CPU made (`on_cpu`, see [`settings_pairs`]).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_watch_settings(wall_ms: i64, offload_wanted: bool, chain_in: bool, on_cpu: bool, usb: bool, playing: bool, output_open: bool) {
    if !on() {
        return;
    }
    let since = ENGINE_SINCE.load(Ordering::Relaxed);
    if !settings_judged(wall_ms, playing, output_open, (since != i64::MIN).then_some(since)) {
        return;
    }
    let Some(s) = nori_settings::settings_store::settings_current() else { return };
    let want_offload = s.offload && !usb && crate::perf_log::offload_blocked().is_none();
    // With the equalizer on, the chain is in the samples' path; off, it may stay in, flat.
    said(wall_ms, settings_held(&settings_pairs(s.eq_enabled, want_offload, offload_wanted, chain_in, on_cpu)));
}

/// The player service started or ended at `wall_ms` (the perf timeline's engine note).
pub(crate) fn engine_changed(wall_ms: i64) {
    ENGINE_SINCE.store(wall_ms, Ordering::Relaxed);
}

/// The invariant breaks this process said, oldest first, each "21:05:12 invariant: kind: detail".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_invariant_breaks() -> Vec<String> {
    lock(&BREAKS).clone()
}

/// The self test's volume on every output of both players (0 to 1); 1 is the music as it is. A player
/// volume, never the phone's: the other apps and the volume keys are left alone.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_quiet(level: f32) {
    QUIET.store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

/// What every volume an output is set to is multiplied by: 1 outside the self test.
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
    fn an_output_that_holds_music_and_does_not_play_it_is_a_break_once() {
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
    fn an_output_given_nothing_to_play_while_playing_is_a_break_once() {
        let mut w = Watch::default();
        w.output("track", &at(0, 5000, 5000));
        assert_eq!(w.output("track", &at(4000, 5000, 5000)), None, "a song's first bytes a moment late");
        let b = w.output("track", &at(5100, 5000, 5000)).expect("given nothing for 5.1 s");
        assert_eq!(b.kind, "starved");
        assert_eq!(b.detail, "track: playing, but the output presented nothing new for 5100 ms and was given nothing new for 5100 ms (at 5000 ms presented of 5000 ms written)");
        assert_eq!(w.output("track", &at(9000, 5000, 5000)), None, "said once");
        // What the phone's track did: counted as holding a tenth of a second, never started, the device's
        // count standing past what it was given since the flush.
        let mut w = Watch::default();
        w.output("track", &at(0, 90_000, 100));
        assert_eq!(w.output("track", &at(3000, 90_000, 100)), None);
        assert_eq!(w.output("track", &at(5500, 90_000, 100)).map(|b| b.kind), Some("starved"));
        // Given music now and then, playing nothing of it: the stall is the output's.
        let mut w = Watch::default();
        w.output("track", &at(0, 0, 100));
        assert_eq!(w.output("track", &at(2500, 0, 200)).map(|b| b.kind), Some("stalled"));
    }

    #[test]
    fn a_track_given_nothing_while_the_engine_waits_for_a_songs_bytes_is_buffering_not_starved() {
        // The report's: a provider's song still on its way, the engine switching to it and waiting for
        // its bytes, the track started and given nothing for five seconds and more.
        let mut w = Watch::default();
        w.engine(false);
        w.track(&at(0, 0, 0));
        assert_eq!(w.track(&at(5_019, 0, 0)), None, "waiting for a song's bytes is buffering");
        assert_eq!(w.track(&at(12_000, 0, 0)), None, "however long the network takes");
        // The bytes came and the engine plays: the track has the whole time from then to be given music.
        w.engine(true);
        assert_eq!(w.track(&at(13_000, 0, 0)), None);
        assert_eq!(w.track(&at(17_500, 0, 0)), None, "4.5 s since the engine said it plays");
        let b = w.track(&at(18_100, 0, 0)).expect("the engine says it plays and gives the track nothing");
        assert_eq!(b.kind, "starved");
        // The engine playing, with music in hand, and nothing reaching the output: said as before.
        let mut w = Watch::default();
        w.engine(true);
        w.track(&at(0, 5000, 5000));
        assert_eq!(w.track(&at(5_100, 5000, 5000)).map(|b| b.kind), Some("starved"));
        // A track holding music it does not play is a stall once the engine plays: the output's own.
        let mut w = Watch::default();
        w.engine(false);
        w.track(&at(0, 0, 100));
        w.engine(true);
        w.track(&at(100, 0, 100));
        assert_eq!(w.track(&at(2_500, 0, 200)).map(|b| b.kind), Some("stalled"));
    }

    #[test]
    fn the_engine_playing_nothing_with_no_output_open_is_said_once_a_stretch() {
        let mut w = Watch::default();
        let disk = || "no entry".to_string();
        assert!(w.silent(SILENT_MS - 1, false, Some("s1"), "Playing", disk).is_none(), "not yet");
        assert!(w.silent(9_000, true, Some("s1"), "Playing", disk).is_none(), "an open output is the track's own watch");
        let b = w.silent(SILENT_MS, false, Some("s1"), "Playing; loaders: no loaders", disk).expect("silent");
        assert_eq!(b.kind, "silent");
        assert!(b.detail.contains("s1 stood still for 5000 ms") && b.detail.ends_with("loaders: no loaders; the stream cache: no entry"), "{}", b.detail);
        assert!(w.silent(8_000, false, Some("s1"), "Playing", disk).is_none(), "said once");
        // The music moved again: the next stretch of it is said again.
        assert!(w.silent(0, false, Some("s1"), "Playing", disk).is_none());
        assert!(w.silent(6_000, false, Some("s2"), "Playing", disk).is_some());
    }

    #[test]
    fn a_stall_of_the_track_quotes_where_the_engine_last_stood() {
        let state = "Playing; playing on 16 (s16) at 51 ms; reading 16 (s16) at 11000 ms; transition engine passing; loaders: s16: 0..90 of 90 bytes";
        engine_seen(&EngineLook { playing: true, index: Some(16), position_ms: 51, in_output_ms: 100, state, ..EngineLook::default() });
        track_seen(0, true, 4410, 90_000, 44_100);
        track_seen(6_000, true, 4410, 90_000, 44_100);
        let breaks = perf_invariant_breaks();
        let line = breaks.iter().find(|l| l.contains("starved: track:")).unwrap_or_else(|| panic!("a starved track: {breaks:?}"));
        assert!(line.contains("; the engine at its last wake, ") && line.ends_with(state), "{line}");
    }

    #[test]
    fn standing_still_is_fine_paused_after_a_flush_and_with_nothing_written_ahead() {
        let mut w = Watch::default();
        w.output("track", &at(0, 5000, 5000));
        assert_eq!(w.output("track", &at(4500, 5000, 5000)), None, "everything written was played: waiting for music");
        let paused = Moving { playing: false, ..at(10_000, 5000, 9000) };
        assert_eq!(w.output("track", &paused), None);
        assert_eq!(w.output("track", &at(11_000, 5000, 9000)), None, "the pause started the count again");
        assert_eq!(w.output("track", &at(12_000, 0, 4000)), None, "a flush sets the count back");
        assert_eq!(w.output("track", &at(13_500, 0, 4000)), None);
        let song = Moving { song: Some(1), ..at(16_000, 0, 4000) };
        assert_eq!(w.output("track", &song), None, "a new song starts afresh");
    }

    #[test]
    fn a_starved_offloaded_track_is_named_so() {
        let mut w = Watch::default();
        let m = |t, p| Moving { offloaded: true, ..at(t, p, 20_000) };
        w.output("engine", &m(0, 900));
        let b = w.output("engine", &m(2600, 900)).unwrap();
        assert_eq!(b.kind, "offload-starved");
        assert!(b.detail.starts_with("engine: playing, but the offloaded track"), "{}", b.detail);
        assert_eq!(w.output("track", &at(0, 1, 2)), None, "each output is watched on its own");
    }

    #[test]
    fn a_skip_moves_one_song_and_three_quick_ones_three() {
        let mut w = Watch::default();
        w.skip(0, 4);
        assert_eq!(w.arrived(100, 5, false, false), None);
        w.skip(200, 5);
        w.skip(350, 6);
        assert_eq!(w.arrived(400, 6, false, false), None);
        assert_eq!(w.arrived(500, 7, false, false), None, "three presses from 4, three songs");
        let b = w.arrived(700, 8, false, false).expect("a fourth song for three presses");
        assert_eq!(b.kind, "skip");
        assert_eq!(b.detail, "3 skip presses moved 4 songs, from queue place 4 to 8");
        assert_eq!(w.arrived(800, 9, false, false), None, "said once for the run");
    }

    #[test]
    fn a_song_ending_by_itself_or_a_shuffled_queue_is_not_a_skip() {
        let mut w = Watch::default();
        w.skip(0, 0);
        assert_eq!(w.arrived(100, 3, false, true), None, "shuffled: places say nothing");
        w.skip(1000, 3);
        assert_eq!(w.arrived(1100, 4, true, false), None);
        assert_eq!(w.arrived(1200, 6, false, false), None, "the run ended with the song that ended");
        w.skip(10_000, 6);
        assert_eq!(w.arrived(14_000, 9, false, false), None, "long after the press: not its doing");
        w.skip(20_000, 9);
        assert_eq!(w.arrived(20_100, 8, false, false), None, "previous moves one back");
    }

    #[test]
    fn the_screen_may_trail_the_ear_a_second_and_a_mix_longer() {
        let mut w = Watch::default();
        assert_eq!(w.heard(0, "a"), None);
        assert_eq!(w.shown(10, Some("a")), None);
        assert_eq!(w.heard(1000, "b"), None);
        assert_eq!(w.compare(1900), None, "0.9 s behind");
        assert_eq!(w.shown(1950, Some("b")), None);
        assert_eq!(w.heard(5000, "c"), None);
        let b = w.compare(6100).expect("1.1 s behind");
        assert_eq!(b.kind, "shown-heard");
        assert_eq!(b.detail, "the screen showed b while c was heard, for 1100 ms");
        assert_eq!(w.compare(9000), None, "said once");
        assert_eq!(w.shown(9100, Some("c")), None);
    }

    #[test]
    fn the_seek_bar_far_from_the_engine_for_a_second_is_a_break() {
        let mut w = Watch::default();
        // The S22's report: the controller's word 14 s ahead, the bar at the end of the song, frame by frame.
        assert_eq!(w.place(0, true, 600_000, 586_000, 600_000), None, "a frame off is not yet a break");
        assert_eq!(w.place(900, true, 600_000, 586_900, 600_000), None);
        let b = w.place(1_100, true, 600_000, 587_100, 600_000).expect("off for over a second");
        assert_eq!(b.line(), "place: the seek bar showed 600.0 s while the engine was at 587.1 s (the controller's word 600.0 s)");
        assert_eq!(w.place(1_200, true, 600_000, 587_200, 600_000), None, "said once");
        // Back with the engine: the next time it goes off is said again.
        assert_eq!(w.place(1_300, true, 587_300, 587_300, 587_300), None);
        w.place(2_000, true, 590_000, 587_000, 587_000);
        assert!(w.place(3_100, true, 591_100, 588_100, 588_100).is_some());
    }

    #[test]
    fn a_controller_s_word_far_from_the_engine_s_is_a_break_even_with_the_bar_right() {
        let mut w = Watch::default();
        w.place(0, true, 586_000, 586_000, 600_000);
        let b = w.place(1_500, true, 587_500, 587_500, 600_000).expect("the word off for 1.5 s");
        assert_eq!(b.detail, "the controller ran on to 600.0 s while the engine was at 587.5 s (the seek bar showed 587.5 s)");
    }

    #[test]
    fn the_place_is_judged_only_playing_with_the_engine_s_to_go_by() {
        let mut w = Watch::default();
        for t in (0..5_000).step_by(100) {
            assert_eq!(w.place(t, false, 600_000, 100_000, 600_000), None, "paused");
            assert_eq!(w.place(t, true, 600_000, -1, 600_000), None, "no engine's place: another song, or a seek on its way");
        }
        // A glide within the bound is not a break however long.
        for t in (5_000..10_000).step_by(100) {
            assert_eq!(w.place(t, true, 100_000 + t, 100_000 + t - PLACE_MS, 100_000 + t), None);
        }
    }

    #[test]
    fn the_screen_is_not_late_while_nobody_can_see_it() {
        let mut w = Watch::default();
        w.heard(0, "a");
        w.shown(10, Some("a"));
        assert_eq!(w.visible(20, false), None, "the screen goes off");
        w.heard(1000, "b");
        assert_eq!(w.compare(60_000), None, "a minute off: nothing is drawn, nothing is late");
        assert_eq!(w.visible(64_000, true), None, "back on: the second to catch up starts now");
        assert_eq!(w.compare(64_900), None);
        let b = w.compare(65_100).expect("still the old song 1.1 s after coming back");
        assert_eq!(b.detail, "the screen showed a while b was heard, for 1100 ms");
        // Caught up in time: nothing said.
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
    fn lyrics_belong_to_the_song_heard() {
        let mut w = Watch::default();
        assert_eq!(w.lyrics(0, "a"), None, "nothing heard yet");
        w.heard(1000, "a");
        assert_eq!(w.lyrics(1500, "a"), None);
        w.heard(2000, "b");
        assert_eq!(w.lyrics(2500, "a"), None, "a's lyrics arriving just as b started");
        let b = w.lyrics(4000, "a").unwrap();
        assert_eq!(b.kind, "lyrics");
        assert_eq!(b.detail, "lyrics of a shown while b is heard (since 2000 ms)");
    }

    #[test]
    fn automix_wants_every_song_s_length() {
        let mut w = Watch::default();
        let missing = vec!["x".to_string(), "y".to_string()];
        assert_eq!(w.queue(false, &missing, 10), None, "AutoMix off");
        let b = w.queue(true, &missing, 10).unwrap();
        assert_eq!(b.detail, "AutoMix is on and 2 of 10 songs in the queue have no length: x, y");
        assert_eq!(w.queue(true, &missing, 10), None, "the same songs said once");
        assert_eq!(w.queue(true, &[], 10), None);
        assert!(w.queue(true, &missing, 10).is_some(), "again after the queue was whole");
    }

    #[test]
    fn a_setting_is_judged_only_playing_through_an_open_output_and_after_the_service_settled() {
        // The self test's restore: the service had just started with a batch of settings, paused.
        assert!(!settings_judged(1_000, false, false, Some(0)), "paused with no output, just started");
        assert!(!settings_judged(10_000, false, true, Some(0)), "paused: the chain is in no path");
        assert!(!settings_judged(10_000, true, false, Some(0)), "playing, but no output open yet");
        assert!(!settings_judged(2_000, true, true, Some(0)), "the service started two seconds ago");
        assert!(!settings_judged(SETTLE_MS, true, true, Some(0)), "still settling at the edge");
        assert!(settings_judged(SETTLE_MS + 1, true, true, Some(0)));
        assert!(settings_judged(5, true, true, None), "no service start seen: judged");
    }

    #[test]
    fn a_setting_is_in_the_engine_a_second_later() {
        assert_eq!(settings_held(&[("offload wanted", true, true)]), None);
        let b = settings_held(&[("offload wanted", false, true), ("sound chain in the path", true, true)]).unwrap();
        assert_eq!(b.kind, "setting");
        assert_eq!(b.detail, "a second after the settings changed the engine still shows offload wanted: true, expected false");
    }

    #[test]
    fn the_chain_is_held_to_the_equalizer_only_while_the_cpu_plays_through_the_engine_s_output() {
        // As the phone had it: the equalizer on, the first song's bytes still coming and the output not open
        // yet (or the chip playing, or the output let go): the chain is in no path, and that is no break.
        assert_eq!(settings_held(&settings_pairs(true, false, false, false, false)), None);
        let b = settings_held(&settings_pairs(true, false, false, false, true)).unwrap();
        assert_eq!(b.detail, "a second after the settings changed the engine still shows sound chain in the path: false, expected true");
        assert_eq!(settings_held(&settings_pairs(false, false, false, false, true)), None, "off, the chain may stay in, flat, or not");
        assert!(settings_held(&settings_pairs(true, false, true, false, false)).is_some(), "offload is held to the settings whatever plays");
    }

    #[test]
    fn quiet_is_a_factor_held_in_range() {
        perf_quiet(0.001);
        assert!((quiet() - 0.001).abs() < 1e-6);
        perf_quiet(3.0);
        assert_eq!(quiet(), 1.0);
    }
}
