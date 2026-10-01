//! Downloads: the in-memory mirror of the downloads table ([`Held`]) and the tracker of what the
//! platform reports ([`Tracker`]): per-song progress, the batch, speed and time left, and the processing
//! after the bytes (lyrics, analysis, beat model). The platform moves the bytes and words the facts.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Poll, Waker};
use std::time::Instant;

use nori_model::{alog, DownloadPhase, Song};
use parking_lot::{Mutex, MutexGuard};
use rusqlite::Connection;

/// Finished songs the downloads screen keeps listing this session.
pub const RECENT: usize = 50;
/// Progress is passed on at most this often, and only after a whole percent.
const GATE_MS: i64 = 250;
const GATE_STEP: f32 = 0.01;
/// Progress against an estimated size stops short of full, since estimates can be low.
const ESTIMATE_CEILING: f32 = 0.97;
const UNKNOWN_SONG_BYTES: f64 = 8_000_000.0;
/// The batch speed's averaging time constant, its shortest sample, and how long it is measured before
/// a time left is given.
const RATE_TAU_MS: f64 = 10_000.0;
const RATE_SAMPLE_MS: i64 = 500;
const RATE_WARM_MS: f64 = 1_500.0;
/// Step guesses weigh as this many songs against timed steps.
const PACE_PRIOR: f64 = 2.0;
/// How long an analysis measured while downloading may run past the last byte.
const ANALYSIS_TAIL_S: f64 = 2.0;
/// Prior share of songs needing an analysis from disk.
const FROM_DISK_GUESS: f64 = 0.5;
/// How long songs may wait in a lane with no step running (its worker gone) before they are let go.
const LANE_IDLE_MS: i64 = 60_000;

// media3's `Download.STATE_*`.
pub const QUEUED: i32 = 0;
const STOPPED: i32 = 1;
pub const DOWNLOADING: i32 = 2;
pub const COMPLETED: i32 = 3;
pub const FAILED: i32 = 4;
pub const REMOVING: i32 = 5;
const RESTARTING: i32 = 7;

/// Flags [`Tracker::followed`] and [`Tracker::removed`] return.
pub const NEW_BATCH: i32 = 1;
pub const DRAINED: i32 = 2;
pub const MARKS: i32 = 4;

/// Work after the bytes. Lyrics run in one lane, analysis and beats share another (one decode feeds
/// both); each lane takes one song at a time, the two lanes in parallel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Lyrics,
    Analysis,
    Beats,
}

impl Work {
    const ALL: [Work; 3] = [Work::Lyrics, Work::Analysis, Work::Beats];

    fn lane(self) -> usize {
        (self != Work::Lyrics) as usize
    }

    /// How long one song's step may run before it is given up. Generous: steps run at the lowest
    /// priority, so this only catches a stuck step.
    fn limit_ms(self) -> i64 {
        [30_000, 180_000, 600_000][self as usize]
    }

    fn guess_ms(self) -> f64 {
        [3_000.0, 5_000.0, 40_000.0][self as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Failed,
    Done,
    /// Saved, with work still pending, by [`Work`].
    Processing([bool; 3]),
}

impl Phase {
    /// The phase the screens show; processing shows lyrics, else analysis, else beats.
    pub fn shown(self) -> DownloadPhase {
        match self {
            Phase::Downloading => DownloadPhase::Downloading,
            Phase::Failed => DownloadPhase::Failed,
            Phase::Done => DownloadPhase::Done,
            Phase::Processing([true, ..]) => DownloadPhase::FindingLyrics,
            Phase::Processing([_, true, _]) => DownloadPhase::Analysing,
            Phase::Processing(_) => DownloadPhase::DetectingBeats,
        }
    }

    fn processing(pending: [bool; 3]) -> Phase {
        if pending.contains(&true) { Phase::Processing(pending) } else { Phase::Done }
    }

    fn pending(self) -> [bool; 3] {
        if let Phase::Processing(p) = self { p } else { [false; 3] }
    }

    fn waits(self, work: Work) -> bool {
        self.pending()[work as usize]
    }

    fn without(self, work: Work) -> Phase {
        match self {
            Phase::Processing(mut p) => {
                p[work as usize] = false;
                Phase::processing(p)
            }
            p => p,
        }
    }
}

/// A song's state as its audio is saved, for [`needs`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Saved {
    /// Not a provider's song or a stream.
    pub analysable: bool,
    /// Being analysed as its bytes arrive, not finished yet.
    pub measuring: bool,
    /// Has an analysis of the current version.
    pub analysed: bool,
    /// The beat model is available and enabled.
    pub model_on: bool,
    /// The beat model is wanted for this download ([`beats_offer`]).
    pub beats_wanted: bool,
    /// The model has read both ends of the current analysis.
    pub beats_done: bool,
}

/// What a saved song still needs after its bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    pub analysis: bool,
    pub beats: bool,
}

/// What `s` needs once saved. Always an analysis (also used for lyrics sync and untagged loudness); the
/// beat model when on and wanted and not done for the current analysis.
pub fn needs(s: Saved) -> Needs {
    if !s.analysable {
        return Needs::default();
    }
    let fresh = s.measuring || !s.analysed;
    Needs { analysis: fresh, beats: s.model_on && s.beats_wanted && (fresh || !s.beats_done) }
}

/// What pressing Download does about the beat model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BeatsOffer {
    /// The model is off or not in this build.
    Off,
    /// Ask, with a way to remember the answer.
    Ask,
    Yes,
    No,
}

pub fn beats_offer(model_on: bool, choice: nori_settings::settings::DownloadBeats) -> BeatsOffer {
    use nori_settings::settings::DownloadBeats;
    match (model_on, choice) {
        (false, _) => BeatsOffer::Off,
        (true, DownloadBeats::Ask) => BeatsOffer::Ask,
        (true, DownloadBeats::Always) => BeatsOffer::Yes,
        (true, DownloadBeats::Never) => BeatsOffer::No,
    }
}

impl BeatsOffer {
    /// Whether the downloads get the model, given the user's `answer` when asked.
    pub fn wants(self, answer: bool) -> bool {
        self == BeatsOffer::Yes || self == BeatsOffer::Ask && answer
    }
}

/// The setting a remembered answer sets.
pub fn beats_remembered(yes: bool) -> nori_settings::settings::DownloadBeats {
    use nori_settings::settings::DownloadBeats;
    if yes { DownloadBeats::Always } else { DownloadBeats::Never }
}

/// Monotonic ms for timing steps.
fn mono_ms() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as i64
}

/// A song's details, read once.
#[derive(Debug, Clone, Default)]
struct Info {
    title: String,
    album: String,
    artist: String,
    /// Expected size, bytes (0 unknown).
    estimate: i64,
}

#[derive(Debug, Clone)]
struct Slot {
    id: String,
    estimate: i64,
    length: i64,
    bytes: i64,
    started_at: i64,
    /// The last progress passed on, NaN before the first.
    gate_value: f32,
    gate_at: i64,
    speed_bytes: i64,
    speed_at: i64,
    rate: f64,
    live: bool,
}

/// Everything queued since the queue was last empty. Its total stays fixed while songs finish ("12 of 49").
#[derive(Debug, Default)]
struct Batch {
    open: HashSet<String>,
    failed_ids: HashSet<String>,
    labels: HashMap<String, String>,
    total: i32,
    done: i32,
    failed: i32,
    /// Total size and count of sized finished songs, for guessing the rest.
    done_bytes: i64,
    done_sized: i64,
}

impl Batch {
    fn finished(&self) -> i32 {
        self.done + self.failed
    }

    /// A song queued; true when it starts a new batch. A failed song queued again is not counted twice.
    fn queued(&mut self, id: &str, label: &str) -> bool {
        if self.open.contains(id) {
            return false;
        }
        let fresh = self.open.is_empty();
        if fresh {
            *self = Batch::default();
        }
        if self.failed_ids.remove(id) {
            self.failed -= 1;
        } else {
            self.total += 1;
        }
        self.open.insert(id.to_string());
        if !label.trim().is_empty() {
            self.labels.insert(id.to_string(), label.to_string());
        }
        fresh
    }

    fn completed(&mut self, id: &str) {
        if self.open.remove(id) {
            self.done += 1;
        }
    }

    fn failed(&mut self, id: &str) {
        if self.open.remove(id) {
            self.failed += 1;
            self.failed_ids.insert(id.to_string());
        }
    }

    /// Cancelled or a failure given up on: no longer counted.
    fn removed(&mut self, id: &str) {
        if self.open.remove(id) {
            self.total -= 1;
        } else if self.failed_ids.remove(id) {
            self.failed -= 1;
            self.total -= 1;
        }
        self.labels.remove(id);
    }

    /// The album every song of the batch shares, if any.
    fn label(&self) -> Option<&str> {
        if (self.labels.len() as i32) < self.total {
            return None;
        }
        let mut names = self.labels.values();
        let first = names.next()?;
        names.all(|n| n == first).then_some(first.as_str())
    }

    /// Progress 0..1: finished songs plus in-flight fractions.
    fn fraction(&self, in_flight: f64) -> f32 {
        if self.total <= 0 {
            return 0.0;
        }
        (((self.finished() as f64 + in_flight) / self.total as f64) as f32).clamp(0.0, 1.0)
    }
}

/// The running batch's notification facts ([`Tracker::notice`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Notice {
    pub kind: NoticeKind,
    /// The song in flight, from 1, of `total`.
    pub position: i32,
    pub total: i32,
    pub permille: i32,
    /// Bytes a second, and seconds left (-1 unknown).
    pub speed_bps: i64,
    pub eta_s: i64,
    /// The in-flight song's title, or empty.
    pub current: String,
    /// The batch's shared album when it has several songs, or empty.
    pub label: String,
}

/// Which running-batch notification title applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeKind {
    /// "Waiting for a network".
    #[default]
    Waiting,
    /// "Downloading “Title”".
    OneNamed,
    /// "Downloading 1 song".
    One,
    /// "Downloading: 12 of 49".
    Many,
}

/// The batch speed: an exponential average normalised by its accumulated weight, so the first seconds
/// are the plain average.
#[derive(Debug, Default)]
struct Throughput {
    ema: f64,
    weight: f64,
    at: i64,
    bytes: i64,
}

impl Throughput {
    /// Starts the next sample here, discarding the idle stretch.
    fn restart(&mut self, now: i64, bytes: i64) {
        (self.at, self.bytes) = (now, bytes);
    }

    /// `bytes` is the running total received at `now`.
    fn sample(&mut self, now: i64, bytes: i64) {
        let dt = now - self.at;
        if dt < RATE_SAMPLE_MS {
            return;
        }
        let instant = (bytes - self.bytes).max(0) as f64 * 1000.0 / dt as f64;
        let a = 1.0 - (-(dt as f64) / RATE_TAU_MS).exp();
        self.ema += a * (instant - self.ema);
        self.weight += a * (1.0 - self.weight);
        self.restart(now, bytes);
    }

    fn rate(&self) -> f64 {
        if self.weight > 0.0 { self.ema / self.weight } else { 0.0 }
    }

    fn settled(&self) -> bool {
        self.weight >= 1.0 - (-RATE_WARM_MS / RATE_TAU_MS).exp()
    }
}

/// Mean duration of one song's step of a [`Work`], blended with its guess. Lyrics that end without a step
/// count as 0 ms, so a platform that looks none up converges to nothing.
#[derive(Debug, Default, Clone, Copy)]
struct Pace {
    took_ms: i64,
    done: i64,
}

impl Pace {
    fn per_song_s(&self, work: Work) -> f64 {
        ((self.took_ms as f64 + work.guess_ms() * PACE_PRIOR) / (self.done as f64 + PACE_PRIOR) / 1000.0).min(work.limit_ms() as f64 / 1000.0)
    }
}

/// One lane: the running step (song, work, start) and when the lane last moved.
#[derive(Debug, Default)]
struct Lane {
    step: Option<(String, Work, i64)>,
    since: i64,
}

/// The displayed time left: counts down steadily while the fresh estimate stays within 10 % (or 2 s),
/// easing towards it; jumps to it otherwise.
#[derive(Debug)]
struct Countdown {
    /// Seconds, negative unknown.
    secs: f64,
    at: i64,
}

impl Default for Countdown {
    fn default() -> Self {
        Countdown { secs: -1.0, at: 0 }
    }
}

impl Countdown {
    fn next(&mut self, fresh: f64, now: i64) -> i64 {
        if fresh <= 0.0 {
            self.secs = -1.0;
            return -1;
        }
        let mut secs = fresh;
        if self.secs >= 0.0 {
            let expected = (self.secs - (now - self.at) as f64 / 1000.0).max(0.0);
            if (fresh - expected).abs() <= (expected * 0.1).max(2.0) {
                secs = expected + (fresh - expected) * 0.2;
            }
        }
        (self.secs, self.at) = (secs, now);
        secs.ceil() as i64
    }
}

/// Input to [`time_left`], by [`Work`]: songs waiting, songs still to come that will need it, and
/// seconds per song. `tail`: an analysis measured while downloading may run past the last byte.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct After {
    waiting: [f64; 3],
    to_come: [f64; 3],
    per: [f64; 3],
    tail: bool,
}

/// Seconds until everything is done (-1: unknown). `download_s`: the remaining bytes at the batch speed.
/// Each lane runs serially, the lanes in parallel; the last song's steps follow its last byte.
fn time_left(download_s: Option<f64>, a: &After) -> f64 {
    let Some(dl) = download_s else { return -1.0 };
    let mut end = dl;
    for lane in 0..2 {
        let (mut queue, mut last) = (0.0, 0.0);
        for i in Work::ALL.into_iter().filter(|w| w.lane() == lane).map(|w| w as usize) {
            queue += (a.waiting[i] + a.to_come[i]) * a.per[i];
            last += a.to_come[i].min(1.0) * a.per[i];
        }
        if queue > 0.0 {
            end = end.max(if last > 0.0 { queue.max(dl + last) } else { queue });
        }
    }
    if a.tail {
        end = end.max(dl + ANALYSIS_TAIL_S);
    }
    end
}

/// Expected download size: duration at the transcode bitrate, or the file size at original quality.
fn expected_bytes(size_bytes: i64, duration_s: i64, bitrate_kbps: i32) -> i64 {
    if bitrate_kbps > 0 && duration_s > 0 { duration_s * bitrate_kbps as i64 * 125 } else { size_bytes }
}

/// Progress against the stated length, else the estimate (capped below 1), else -1.
pub fn fraction(length: i64, bytes: i64, estimate: i64) -> f32 {
    if length > 0 {
        (bytes as f64 / length as f64).clamp(0.0, 1.0) as f32
    } else if estimate > 0 {
        ((bytes as f64 / estimate as f64) as f32).clamp(0.0, ESTIMATE_CEILING)
    } else {
        -1.0
    }
}

/// Whether progress `f` (negative: unknown) is worth drawing after `last` (NaN: nothing yet) shown at
/// `last_at`: the first value, a switch to or from unknown, the finish, else a whole percent at most every
/// [`GATE_MS`].
fn gate(last: f32, last_at: i64, f: f32, now: i64) -> bool {
    if last.is_nan() || (f < 0.0) != (last < 0.0) {
        true
    } else if f < 0.0 {
        false
    } else if f >= 1.0 {
        last < 1.0
    } else {
        now - last_at >= GATE_MS && (f - last).abs() >= GATE_STEP
    }
}

/// A running song's row facts on the downloads screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowFacts {
    /// Whole percent, -1 when the size is not known.
    pub percent: i32,
    /// Bytes a second (0 unknown) and seconds left (-1 unknown).
    pub speed_bps: i64,
    pub eta_s: i64,
}

/// Saved songs waiting for each kind of work, and seconds left (-1 unknown), for the notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Processing {
    pub lyrics: i32,
    pub analysing: i32,
    pub beats: i32,
    pub eta_s: i64,
}

/// Which finished-batch notification title applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryTitle {
    /// "`failed` songs couldn’t be downloaded".
    Failed,
    /// "“`label`” downloaded": more than one song, all from one album, none failed.
    Album,
    /// "`done` songs downloaded".
    Downloaded,
}

/// The line under a finished batch's title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryText {
    None,
    /// Some failed, some did not: "`done` downloaded · tap to see what failed".
    SomeFailed,
    /// All failed: "Tap to try again".
    TryAgain,
}

/// How a batch went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub title: SummaryTitle,
    pub text: SummaryText,
    pub done: i32,
    pub failed: i32,
    /// The album, for [`SummaryTitle::Album`]; empty otherwise.
    pub label: String,
}

/// Parallel lists of ids, shown phases (None: removed) and when each began.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadMarks {
    pub ids: Vec<String>,
    pub phases: Vec<Option<DownloadPhase>>,
    pub at: Vec<i64>,
}

/// Every reported download, the batch, the processing after the bytes and the notification state.
#[derive(Debug, Default)]
pub struct Tracker {
    /// Where song details are read from when [`Tracker::know`] was not told.
    db: Option<Arc<Mutex<Connection>>>,
    slots: Vec<Slot>,
    batch: Batch,
    marks: HashMap<String, (Phase, i64)>,
    /// Songs whose mark changed since [`Tracker::marks_changed`].
    changed: HashSet<String>,
    info: HashMap<String, Info>,
    /// Songs being analysed as their bytes arrive.
    analysing: HashSet<String>,
    /// Downloads the beat model reads once saved (mirrors the download_beats table).
    beats_wanted: HashSet<String>,
    /// Waiters on mark changes, woken after the lock is released when `wake` is set.
    wakers: Vec<Waker>,
    wake: bool,
    download_kbps: i32,
    speed_bps: i64,
    remaining_bytes: i64,
    eta_s: i64,
    notice: Notice,
    /// Bytes received in this process (a resumed download's earlier bytes excluded).
    received: i64,
    rate: Throughput,
    paces: [Pace; 3],
    lanes: [Lane; 2],
    /// Songs saved in this process, and how many needed an analysis from disk.
    saved: i64,
    from_disk: i64,
    countdown: Countdown,
    /// The platform's last `now` and when it was given, to extrapolate the platform clock.
    clock: Option<(i64, Instant)>,
    /// Actual vs estimated size of finished songs, to scale remaining estimates.
    sized_actual: i64,
    sized_estimate: i64,
    /// Overrides [`mono_ms`] in tests.
    test_clock: Option<i64>,
}

impl Tracker {
    /// The queued `songs`' titles and sizes, so reports about them never wait for the database.
    pub fn know(&mut self, songs: &[Song]) {
        songs.iter().for_each(|s| self.keep_info(&s.id, Some(s)));
    }

    /// Follows the download bitrate setting for size estimates; a change drops the cached estimates.
    pub fn follow_quality(&mut self, kbps: i32) {
        if self.download_kbps != kbps {
            self.download_kbps = kbps;
            self.info.clear();
            (self.sized_actual, self.sized_estimate) = (0, 0);
        }
    }

    /// What [`Tracker::know`] was told about `id`, else what the database says when it is free: callers
    /// include the main thread and `@CriticalNative` doors, which must not wait. None while it is busy.
    fn info(&mut self, id: &str) -> Option<&Info> {
        if !self.info.contains_key(id) {
            let c = self.db.as_ref()?.try_lock()?;
            let json: Option<String> = c.query_row("SELECT json FROM downloads WHERE server=sid() AND id=?1", [id], |r| r.get(0)).ok();
            drop(c);
            self.keep_info(id, json.and_then(|j| serde_json::from_str::<Song>(&j).ok()).as_ref());
        }
        self.info.get(id)
    }

    fn keep_info(&mut self, id: &str, found: Option<&Song>) {
        let info = found.map_or_else(Info::default, |s| Info {
            estimate: expected_bytes(s.size as i64, s.duration as i64, self.download_kbps),
            title: s.title.replace('\n', " "),
            album: s.album.replace('\n', " "),
            artist: s.artist.replace('\n', " "),
        });
        self.info.insert(id.to_string(), info);
    }

    fn slot_of(&self, id: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.live && s.id == id)
    }

    fn close(&mut self, id: &str) {
        if let Some(i) = self.slot_of(id) {
            self.slots[i].live = false;
        }
    }

    fn mark(&mut self, id: &str, phase: Phase, now: i64) -> bool {
        if phase == Phase::Downloading && self.marks.get(id).is_some_and(|m| m.0 == phase) {
            return false;
        }
        self.marks.insert(id.to_string(), (phase, now));
        self.changed.insert(id.to_string());
        self.wake = true;
        if phase == Phase::Done {
            self.recent_only();
        }
        true
    }

    fn unmark(&mut self, id: &str) -> bool {
        let had = self.marks.remove(id).is_some();
        if had {
            self.changed.insert(id.to_string());
            self.wake = true;
        }
        had
    }

    /// Drops finished marks beyond the latest [`RECENT`].
    fn recent_only(&mut self) {
        let mut done: Vec<(i64, String)> = self.marks.iter().filter(|(_, m)| m.0 == Phase::Done).map(|(id, m)| (m.1, id.clone())).collect();
        if done.len() > RECENT {
            done.sort();
            for (_, id) in &done[..done.len() - RECENT] {
                self.unmark(id);
            }
        }
    }

    pub fn phase(&self, id: &str) -> Option<DownloadPhase> {
        self.marks.get(id).map(|m| m.0.shown())
    }

    /// Songs marked finished or processing this session.
    pub fn saved_ids(&self) -> Vec<String> {
        self.marks.iter().filter(|(_, m)| matches!(m.0, Phase::Done | Phase::Processing(_))).map(|(id, _)| id.clone()).collect()
    }

    /// Saved songs waiting for each [`Work`].
    fn processing(&self) -> [i32; 3] {
        let mut n = [0; 3];
        for m in self.marks.values() {
            for w in Work::ALL {
                n[w as usize] += m.0.waits(w) as i32;
            }
        }
        n
    }

    /// The work left after the bytes: the saved songs', plus `to_come` songs needing lyrics (and an
    /// analysis at the observed from-disk share) and `beats_to_come` needing the model. `tail`: a
    /// downloading song is being analysed as it arrives.
    fn after(&self, to_come: i32, beats_to_come: i32, tail: bool) -> After {
        let n = self.processing();
        // Songs analysed while downloading only wait for a short tail, not a full analysis.
        let arriving = self.marks.iter().filter(|(id, m)| m.0.waits(Work::Analysis) && self.analysing.contains(*id)).count() as f64;
        let share = (self.from_disk as f64 + FROM_DISK_GUESS) / (self.saved as f64 + 1.0);
        After {
            waiting: [n[0] as f64, (n[1] as f64 - arriving).max(0.0), n[2] as f64],
            to_come: [to_come as f64, to_come as f64 * share, beats_to_come as f64],
            per: Work::ALL.map(|w| self.paces[w as usize].per_song_s(w)),
            tail: tail || arriving > 0.0,
        }
    }

    fn mono(&self) -> i64 {
        self.test_clock.unwrap_or_else(mono_ms)
    }

    /// The platform reported `id` in `state` at its clock's `now`. Returns [`NEW_BATCH`], [`DRAINED`]
    /// and [`MARKS`] flags.
    pub fn followed(&mut self, id: &str, state: i32, now: i64) -> i32 {
        self.clock = Some((now, Instant::now()));
        let was_open = self.batch.open.contains(id);
        let mut flags = 0;
        match state {
            QUEUED | DOWNLOADING | RESTARTING | STOPPED => {
                let label = self.info(id).map(|i| i.album.clone()).unwrap_or_default();
                if self.batch.queued(id, &label) {
                    flags |= NEW_BATCH;
                }
            }
            COMPLETED => {
                // Its size, for estimating the rest.
                if let (true, Some(i)) = (was_open, self.slot_of(id)) {
                    let s = &self.slots[i];
                    let size = if s.length > 0 { s.length } else { s.bytes };
                    if size > 0 {
                        self.batch.done_bytes += size;
                        self.batch.done_sized += 1;
                        if s.estimate > 0 {
                            self.sized_actual += size;
                            self.sized_estimate += s.estimate;
                        }
                    }
                }
                self.batch.completed(id)
            }
            FAILED => self.batch.failed(id),
            _ => {}
        }
        let phase = match state {
            DOWNLOADING => Some(Phase::Downloading),
            // Provider songs get no lyrics lookup; other needs come later through `plan`.
            COMPLETED => Some(Phase::processing([!nori_model::is_provider_id(id), self.analysing.contains(id), false])),
            FAILED => Some(Phase::Failed),
            _ => None,
        };
        if matches!(state, COMPLETED | FAILED) {
            self.close(id);
        }
        if let Some(p) = phase {
            self.entered(p.pending());
        }
        let moved = match phase {
            Some(p) => self.mark(id, p, now),
            None => self.unmark(id),
        };
        if moved {
            flags |= MARKS;
            match phase {
                Some(Phase::Downloading) => alog::info(&format!("start {id}: {} downloading at once", self.marks.values().filter(|m| m.0 == Phase::Downloading).count())),
                Some(Phase::Failed) => alog::info(&format!("failed {id}")),
                _ => {}
            }
        }
        flags | self.drained_if(was_open)
    }

    /// [`DRAINED`] when the batch's last open song (`was_open`) settled; speed and time left reset then,
    /// as [`Tracker::notice`] no longer updates them.
    fn drained_if(&mut self, was_open: bool) -> i32 {
        if !was_open || !self.batch.open.is_empty() {
            return 0;
        }
        (self.speed_bps, self.eta_s) = (0, -1);
        self.countdown = Countdown::default();
        DRAINED
    }

    /// Work arrived; an idle lane starts its idle timer now.
    fn entered(&mut self, pending: [bool; 3]) {
        let now = self.mono();
        for (lane, came) in [(0, pending[0]), (1, pending[1])] {
            if came && self.lanes[lane].step.is_none() {
                self.lanes[lane].since = now;
            }
        }
    }

    /// Speed and time left at `now`. While bytes arrive [`Tracker::notice`] computes them; after, this
    /// counts down the remaining processing (speed 0, -1 when nothing is left).
    fn speed_eta_at(&mut self, now: i64) -> (i64, i64) {
        if self.batch.open.is_empty() {
            let fresh = time_left(Some(0.0), &self.after(0, 0, false));
            self.speed_bps = 0;
            self.eta_s = self.countdown.next(fresh, now);
        }
        (self.speed_bps, self.eta_s)
    }

    /// The batch speed (bytes a second) and seconds left (-1 unknown), at the extrapolated platform clock.
    pub fn speed_eta(&mut self) -> (i64, i64) {
        match self.clock {
            Some((at, instant)) => self.speed_eta_at(at + instant.elapsed().as_millis() as i64),
            None => (self.speed_bps, self.eta_s),
        }
    }

    /// `id` is being analysed as it downloads, until [`Tracker::analysing_ended`].
    pub fn analysing_began(&mut self, id: &str) {
        self.analysing.insert(id.to_string());
    }

    /// The streaming analysis of `id` ended. If `stored`, the song no longer waits for an analysis;
    /// otherwise it waits for one from disk.
    pub fn analysing_ended(&mut self, id: &str, stored: bool) {
        self.analysing.remove(id);
        if stored {
            self.work_done(id, Work::Analysis);
        }
    }

    /// `id`'s step of `work` starts; its lane is busy and timed until [`Tracker::work_done`].
    pub fn working(&mut self, id: &str, work: Work) {
        let now = self.mono();
        self.lanes[work.lane()] = Lane { step: Some((id.to_string(), work, now)), since: now };
    }

    /// Whether saved song `id` still waits for `work`.
    pub fn waits(&self, id: &str, work: Work) -> bool {
        self.marks.get(id).is_some_and(|m| m.0.waits(work))
    }

    /// `work` ended for `id` (succeeded or not); true when its phase changed.
    pub fn work_done(&mut self, id: &str, work: Work) -> bool {
        let now = self.mono();
        let lane = &mut self.lanes[work.lane()];
        let timed = match &lane.step {
            Some((s, w, at)) if s == id && *w == work => Some(now - at),
            _ => None,
        };
        if timed.is_some() {
            *lane = Lane { step: None, since: now };
        }
        let Some(&(phase, at)) = self.marks.get(id).filter(|m| m.0.waits(work)) else { return false };
        // A lyrics lookup ended without a step took 0 ms; an unneeded analysis or model run is not timed.
        if let Some(ms) = timed.or((work == Work::Lyrics).then_some(0)) {
            let pace = &mut self.paces[work as usize];
            pace.took_ms += ms.max(0);
            pace.done += 1;
        }
        self.mark(id, phase.without(work), at)
    }

    /// Adds `needs` to saved song `id`'s processing. `saved`: Some(from_disk) for a fresh download (teaches
    /// the from-disk share), None for a song reprocessed later.
    pub fn plan(&mut self, id: &str, needs: Needs, saved: Option<bool>) -> bool {
        if let Some(from_disk) = saved {
            self.saved += 1;
            self.from_disk += from_disk as i64;
        }
        if !needs.analysis && !needs.beats {
            return false;
        }
        let (phase, at) = match self.marks.get(id) {
            Some(&(p @ (Phase::Processing(_) | Phase::Done), at)) => (p, at),
            // Downloading or failed: nothing saved yet.
            Some(_) => return false,
            None => (Phase::Done, self.clock.map_or(0, |(at, instant)| at + instant.elapsed().as_millis() as i64)),
        };
        let [lyrics, analysing, beats] = phase.pending();
        self.entered([false, true, false]);
        self.mark(id, Phase::processing([lyrics, analysing || needs.analysis, beats || needs.beats]), at)
    }

    /// Gives up steps past their [`Work::limit_ms`] and releases songs in a lane idle for
    /// [`LANE_IDLE_MS`]. Returns ms until the next deadline, -1 when nothing is processing.
    pub fn expire(&mut self) -> i64 {
        let now = self.mono();
        let mut next: Option<i64> = None;
        for lane in 0..2 {
            let works = Work::ALL.into_iter().filter(|w| w.lane() == lane);
            let waiting: Vec<(String, Work)> = self.marks.iter().flat_map(|(id, m)| works.clone().filter(|w| m.0.waits(*w)).map(|w| (id.clone(), w))).collect();
            if waiting.is_empty() {
                self.lanes[lane].step = None;
                continue;
            }
            let step = self.lanes[lane].step.clone();
            let limit = step.as_ref().map_or(LANE_IDLE_MS, |s| s.1.limit_ms());
            let deadline = self.lanes[lane].since + limit;
            let wait = if now < deadline {
                deadline - now
            } else {
                match step.filter(|(id, w, _)| self.waits(id, *w)) {
                    Some((id, w, _)) => {
                        alog::info(&format!("{w:?} of {id} took over {} s: given up", limit / 1000));
                        self.work_done(&id, w);
                        self.lanes[lane].step = Some((id, w, now));
                    }
                    // Idle for its whole spell: the worker is gone; release everything waiting.
                    None => {
                        alog::info(&format!("{} songs left waiting with nothing working on them: let go", waiting.len()));
                        for (id, w) in waiting {
                            if let Some(&(phase, at)) = self.marks.get(&id) {
                                self.mark(&id, phase.without(w), at);
                            }
                        }
                        self.lanes[lane].step = None;
                    }
                }
                self.lanes[lane].since = now;
                limit
            };
            next = Some(next.map_or(wait, |n| n.min(wait)));
        }
        if self.processing() == [0; 3] { -1 } else { next.unwrap_or(0) }
    }

    /// The pending processing at the platform's `now`; None when there is none.
    pub fn processing_at(&mut self, now: i64) -> Option<Processing> {
        let [lyrics, analysing, beats] = self.processing();
        if lyrics + analysing + beats == 0 {
            return None;
        }
        self.clock = Some((now, Instant::now()));
        let (_, eta_s) = self.speed_eta_at(now);
        Some(Processing { lyrics, analysing, beats, eta_s })
    }

    /// The marks changed since the last call, with their phase now.
    pub fn marks_changed(&mut self) -> DownloadMarks {
        let mut m = DownloadMarks { ids: Vec::new(), phases: Vec::new(), at: Vec::new() };
        for id in self.changed.drain() {
            let (p, at) = self.marks.get(&id).map_or((None, 0), |(p, at)| (Some(p.shown()), *at));
            m.ids.push(id);
            m.phases.push(p);
            m.at.push(at);
        }
        m
    }

    /// Replaces or changes the set of downloads the beat model reads once saved.
    pub fn want_beats(&mut self, ids: &[String], on: bool) {
        for id in ids {
            if on {
                self.beats_wanted.insert(id.clone());
            } else {
                self.beats_wanted.remove(id);
            }
        }
    }

    pub fn wants_beats(&self, id: &str) -> bool {
        self.beats_wanted.contains(id)
    }

    /// An earlier process left `id` failed: marked so, unless this one already marked it. Returns its
    /// progress from the platform's `length` and `bytes`.
    pub fn failed_before(&mut self, id: &str, length: i64, bytes: i64) -> f32 {
        let estimate = self.info(id).map_or(0, |i| i.estimate);
        if !self.marks.contains_key(id) {
            self.mark(id, Phase::Failed, 0);
        }
        fraction(length, bytes, estimate)
    }

    /// `id` left the queue for good. Returns flags as [`Tracker::followed`].
    pub fn removed(&mut self, id: &str) -> i32 {
        let was_open = self.batch.open.contains(id);
        self.batch.removed(id);
        self.info.remove(id);
        self.forget(id) | self.drained_if(was_open)
    }

    /// Forgets `id`'s mark and slot (requeued or cancelled); [`MARKS`] when it had a mark.
    pub fn forget(&mut self, id: &str) -> i32 {
        self.close(id);
        if self.unmark(id) { MARKS } else { 0 }
    }

    /// Initial progress: 0, or -1 when the size is unknown.
    pub fn start_fraction(&mut self, id: &str) -> f32 {
        if self.info(id).is_some_and(|i| i.estimate > 0) { 0.0 } else { -1.0 }
    }

    /// A download starts transferring; returns the slot for [`Tracker::note`].
    pub fn open(&mut self, id: &str, now: i64) -> i32 {
        // A resumed download replaces its old slot.
        self.close(id);
        // After an idle stretch, measure speed from now.
        if !self.slots.iter().any(|s| s.live) && self.received == self.rate.bytes {
            self.rate.restart(now, self.received);
        }
        let estimate = self.info(id).map_or(0, |i| i.estimate);
        let slot = Slot { id: id.to_string(), estimate, length: 0, bytes: 0, started_at: now, gate_value: f32::NAN, gate_at: 0, speed_bytes: 0, speed_at: now, rate: 0.0, live: true };
        match self.slots.iter().position(|s| !s.live) {
            Some(i) => {
                self.slots[i] = slot;
                i as i32
            }
            None => {
                self.slots.push(slot);
                self.slots.len() as i32 - 1
            }
        }
    }

    /// A chunk arrived on `slot`: `bytes` so far of `length` (0 unknown). Returns the progress to show, or
    /// NaN when not worth redrawing. Called per chunk: allocates nothing.
    pub fn note(&mut self, slot: i32, length: i64, bytes: i64, now: i64) -> f32 {
        let Some(s) = usize::try_from(slot).ok().and_then(|i| self.slots.get_mut(i)).filter(|s| s.live) else { return f32::NAN };
        // A resumed download's first report is its existing bytes, not new ones.
        if s.gate_value.is_nan() && s.bytes == 0 {
            s.speed_bytes = bytes;
        } else if bytes > s.bytes {
            self.received += bytes - s.bytes;
        }
        s.bytes = bytes;
        if length > 0 {
            s.length = length;
        }
        // Per-song rate: samples of at least 0.4 s in a running average.
        let dt = (now - s.speed_at) as f64 / 1000.0;
        if dt >= 0.4 && bytes >= s.speed_bytes {
            let instant = (bytes - s.speed_bytes) as f64 / dt;
            s.rate = if s.rate <= 0.0 { instant } else { s.rate * 0.7 + instant * 0.3 };
            (s.speed_bytes, s.speed_at) = (bytes, now);
        }
        let f = fraction(s.length, bytes, s.estimate);
        if !gate(s.gate_value, s.gate_at, f, now) {
            return f32::NAN;
        }
        (s.gate_value, s.gate_at) = (f, now);
        f
    }

    /// Updates the notification facts for `listed` downloads the platform knows (`waiting`: no network).
    /// Returns 0 unchanged, 1 changed (read [`Tracker::notice_facts`]), 2 batch over. Called every
    /// second; allocates only when the title or album changes.
    pub fn notice(&mut self, listed: i32, waiting: bool, now: i64) -> i32 {
        let total = self.batch.total.max(self.batch.finished() + listed);
        if total == 0 {
            return 2;
        }
        let (mut in_flight, mut current) = (0f64, None::<usize>);
        for (i, s) in self.slots.iter().enumerate().filter(|(_, s)| s.live) {
            in_flight += fraction(s.length, s.bytes, s.estimate).max(0.0) as f64;
            if current.is_none_or(|c| s.started_at < self.slots[c].started_at) {
                current = Some(i);
            }
        }
        // Batch speed from all bytes received (not the sum of per-song rates, which dip between songs).
        // Waiting for a network, or idle, is not sampled.
        let idle = waiting || current.is_none() && self.received == self.rate.bytes;
        if idle {
            self.rate.restart(now, self.received);
        } else {
            self.rate.sample(now, self.received);
        }
        let rate = self.rate.rate();
        self.speed_bps = if waiting || current.is_none() && self.received == self.rate.bytes { 0 } else { rate as i64 };
        let permille = (self.batch.fraction(in_flight) * 1000.0) as i32;
        let position = (self.batch.finished() + 1).min(total.max(1));
        // Remaining bytes: each open song's length (else its scaled estimate) minus its bytes, and the
        // average song size for unsized and unreported songs.
        let ratio = if self.sized_estimate > 0 { (self.sized_actual as f64 / self.sized_estimate as f64).clamp(0.5, 2.0) } else { 1.0 };
        let (mut remaining, mut known_sum, mut known_n, mut sizeless, mut unsized_had) = (0f64, self.batch.done_bytes as f64, self.batch.done_sized, 0i64, 0i64);
        let (mut lyrics_to_come, mut beats_to_come, mut analysing) = (0, 0, false);
        for id in &self.batch.open {
            // Provider songs get no lyrics lookup or beat model.
            if !nori_model::is_provider_id(id) {
                lyrics_to_come += 1;
                beats_to_come += self.beats_wanted.contains(id) as i32;
            }
            analysing |= self.analysing.contains(id);
            let (length, bytes) = self.slots.iter().find(|s| s.live && s.id == *id).map_or((0, 0), |s| (s.length, s.bytes));
            let size = if length > 0 { length as f64 } else { self.info.get(id).map_or(0, |i| i.estimate) as f64 * ratio };
            if size > 0.0 {
                known_sum += size;
                known_n += 1;
                remaining += (size - bytes as f64).max(0.0);
            } else {
                sizeless += 1;
                unsized_had += bytes;
            }
        }
        let avg = if known_n == 0 { UNKNOWN_SONG_BYTES } else { known_sum / known_n as f64 };
        let unreported = (total - self.batch.finished() - listed).max(0);
        lyrics_to_come += unreported;
        remaining += (sizeless as f64 * avg - unsized_had as f64).max(0.0) + unreported as f64 * avg;
        self.remaining_bytes = remaining as i64;
        let after = self.after(lyrics_to_come, beats_to_come, analysing);
        self.clock = Some((now, Instant::now()));
        let download_s = if remaining < 1.0 {
            Some(0.0)
        } else {
            (rate > 0.0 && self.rate.settled()).then(|| remaining / rate)
        };
        self.eta_s = self.countdown.next(if waiting { -1.0 } else { time_left(download_s, &after) }, now);
        let current = current.and_then(|i| self.info.get(&self.slots[i].id)).map_or("", |i| i.title.as_str());
        let kind = match (waiting, total) {
            (true, _) => NoticeKind::Waiting,
            (false, 1) if !current.is_empty() => NoticeKind::OneNamed,
            (false, 1) => NoticeKind::One,
            _ => NoticeKind::Many,
        };
        let label = if total != 1 { self.batch.label().unwrap_or("") } else { "" };
        let n = &mut self.notice;
        let facts = (kind, position, total, permille, self.speed_bps, self.eta_s);
        if (n.kind, n.position, n.total, n.permille, n.speed_bps, n.eta_s) == facts && n.current == current && n.label == label {
            return 0;
        }
        // Reuse the kept strings' buffers.
        if n.current != current {
            n.current.clear();
            n.current.push_str(current);
        }
        if n.label != label {
            n.label.clear();
            n.label.push_str(label);
        }
        (n.kind, n.position, n.total, n.permille, n.speed_bps, n.eta_s) = facts;
        1
    }

    /// The notification facts as [`Tracker::notice`] last computed them.
    pub fn notice_facts(&self) -> &Notice {
        &self.notice
    }

    /// How the batch's downloads went; None when nothing finished.
    pub fn summary(&self) -> Option<Summary> {
        let (done, failed) = (self.batch.done, self.batch.failed);
        if done == 0 && failed == 0 {
            return None;
        }
        let album = self.batch.label().filter(|_| done > 1 && failed == 0);
        let title = match (failed > 0, album) {
            (true, _) => SummaryTitle::Failed,
            (false, Some(_)) => SummaryTitle::Album,
            (false, None) => SummaryTitle::Downloaded,
        };
        let text = match (failed > 0, done > 0) {
            (true, true) => SummaryText::SomeFailed,
            (true, false) => SummaryText::TryAgain,
            (false, _) => SummaryText::None,
        };
        Some(Summary { title, text, done, failed, label: album.unwrap_or("").to_string() })
    }

    /// A downloads screen row: the song's artist (empty when unknown; never blocks on the database) and
    /// its facts while running.
    pub fn row(&mut self, id: &str) -> (&str, Option<RowFacts>) {
        let facts = row_facts(&self.slots, id);
        (self.info(id).map_or("", |i| i.artist.as_str()), facts)
    }

    /// The downloads screen's lists: `pending` (newest first) split into active, queued and failed in queue
    /// order (oldest first), and this session's finished songs newest first, found in `pending` or `done`
    /// so a just-finished song shows before the index catches up.
    pub fn sections<T: Clone>(&self, pending: &[T], done: &[T], id: impl Fn(&T) -> &str) -> [Vec<T>; 4] {
        sections(pending, done, &self.marks, id)
    }
}

fn row_facts(slots: &[Slot], id: &str) -> Option<RowFacts> {
    let s = slots.iter().find(|s| s.live && s.id == id)?;
    let f = fraction(s.length, s.bytes, s.estimate);
    let total = if s.length > 0 { s.length } else { s.estimate };
    let speed = s.rate as i64;
    let eta = if speed > 0 && total > s.bytes { (total - s.bytes) / speed } else { -1 };
    Some(RowFacts { percent: if f >= 0.0 { (f * 100.0).round() as i32 } else { -1 }, speed_bps: speed, eta_s: eta })
}

fn sections<T: Clone>(pending: &[T], done: &[T], marks: &HashMap<String, (Phase, i64)>, id: impl Fn(&T) -> &str) -> [Vec<T>; 4] {
    let phase = |song: &T| marks.get(id(song)).copied();
    // Saved songs still processing come first.
    let mut active: Vec<T> = done.iter().filter(|s| matches!(phase(s), Some((Phase::Processing(_), _)))).cloned().collect();
    let (mut queued, mut failed) = (Vec::new(), Vec::new());
    for song in pending.iter().rev() {
        match phase(song).map(|m| m.0) {
            Some(Phase::Downloading | Phase::Processing(_)) => active.push(song.clone()),
            Some(Phase::Failed) => failed.push(song.clone()),
            Some(Phase::Done) => {}
            None => queued.push(song.clone()),
        }
    }
    let mut finished: Vec<(i64, &T)> = pending.iter().chain(done).filter_map(|s| phase(s).filter(|m| m.0 == Phase::Done).map(|m| (m.1, s))).collect();
    finished.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut seen = HashSet::new();
    let finished = finished.into_iter().filter(|(_, s)| seen.insert(id(s).to_string())).map(|(_, s)| s.clone()).collect();
    [active, queued, failed, finished]
}

/// In-memory mirror of the downloads table (id -> finished), so rows and tracks are answered without
/// the database. Every table write updates it.
#[derive(Debug, Default)]
pub struct Held {
    ids: HashMap<String, bool>,
    done: u32,
}

/// Bumped on every downloads table change of any core, so the platform detects changed counts. Global:
/// one counter across cores, so a new server's counts never match the old one's version.
static HELD_VERSION: AtomicU64 = AtomicU64::new(1);

/// A song's state in the downloads table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldState {
    Absent,
    /// Queued or failed.
    Pending,
    Done,
}

impl HeldState {
    /// The code Kotlin's `DownloadsJni` reads.
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Downloaded and pending counts, with the version they were read at.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadCounts {
    pub done: u32,
    pub pending: u32,
    pub version: u64,
}

impl Held {
    fn load(c: &Connection) -> nori_model::Result<Held> {
        let mut st = c.prepare("SELECT id, done FROM downloads WHERE server=sid()")?;
        let ids: HashMap<String, bool> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect();
        let done = ids.values().filter(|d| **d).count() as u32;
        HELD_VERSION.fetch_add(1, Ordering::Relaxed);
        Ok(Held { ids, done })
    }

    pub fn state(&self, id: &str) -> HeldState {
        self.ids.get(id).map_or(HeldState::Absent, |d| if *d { HeldState::Done } else { HeldState::Pending })
    }

    pub fn counts(&self) -> DownloadCounts {
        DownloadCounts { done: self.done, pending: self.ids.len() as u32 - self.done, version: HELD_VERSION.load(Ordering::Relaxed) }
    }

    pub fn queued(&mut self, id: &str) {
        if !self.ids.contains_key(id) {
            self.ids.insert(id.to_string(), false);
            HELD_VERSION.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn finished(&mut self, id: &str) {
        if let Some(d) = self.ids.get_mut(id).filter(|d| !**d) {
            *d = true;
            self.done += 1;
            HELD_VERSION.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn removed(&mut self, id: &str) {
        if let Some(d) = self.ids.remove(id) {
            self.done -= d as u32;
            HELD_VERSION.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// One core's downloads: its table's mirror and the tracker of what the platform reports.
#[derive(Debug)]
pub struct Downloads {
    tracker: Mutex<Tracker>,
    held: Mutex<Held>,
}

/// The active core's downloads. Global: JNI doors and engine callbacks carry no core handle.
static ACTIVE: Mutex<Weak<Downloads>> = Mutex::new(Weak::new());

impl Downloads {
    pub fn load(db: &Arc<Mutex<Connection>>) -> nori_model::Result<Downloads> {
        let c = db.lock();
        let held = Held::load(&c)?;
        let mut st = c.prepare("SELECT id FROM download_beats WHERE server=sid()")?;
        let beats_wanted = st.query_map([], |r| r.get(0))?.filter_map(|r| r.ok()).collect();
        let tracker = Tracker { db: Some(db.clone()), beats_wanted, ..Tracker::default() };
        Ok(Downloads { tracker: Mutex::new(tracker), held: Mutex::new(held) })
    }

    /// Makes these the downloads the platform's reports reach. Waiters on the previous ones are woken,
    /// so they wait on these from now on.
    pub fn activate(self: &Arc<Self>) {
        let old = std::mem::replace(&mut *ACTIVE.lock(), Arc::downgrade(self));
        if let Some(old) = old.upgrade() {
            old.with(|t| t.wake = true);
        }
    }

    /// Runs `f` on the tracker, then wakes mark waiters after the lock is released.
    pub fn with<R>(&self, f: impl FnOnce(&mut Tracker) -> R) -> R {
        let mut t = self.tracker.lock();
        let r = f(&mut t);
        let wakers = if std::mem::take(&mut t.wake) { std::mem::take(&mut t.wakers) } else { Vec::new() };
        drop(t);
        wakers.into_iter().for_each(Waker::wake);
        r
    }

    pub fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock()
    }

    /// Resolves once a mark changed since the last [`Tracker::marks_changed`].
    pub async fn marks_moved(&self) {
        std::future::poll_fn(|cx| self.poll_moved(cx)).await
    }

    fn poll_moved(&self, cx: &mut std::task::Context) -> Poll<()> {
        let mut t = self.tracker.lock();
        if t.changed.is_empty() {
            t.wakers.push(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

pub fn active() -> Option<Arc<Downloads>> {
    ACTIVE.lock().upgrade()
}

/// Runs `f` on the active tracker; None without an active core.
pub fn with<R>(f: impl FnOnce(&mut Tracker) -> R) -> Option<R> {
    active().map(|d| d.with(f))
}

/// `id`'s state in the active core's downloads table.
pub fn held(id: &str) -> HeldState {
    active().map_or(HeldState::Absent, |d| d.held().state(id))
}

// Thin adapters over the active tracker for the engine's and Android's callers.
pub fn followed(id: &str, state: i32, now: i64) -> i32 {
    with(|t| t.followed(id, state, now)).unwrap_or(0)
}
pub fn open(id: &str, now: i64) -> i32 {
    with(|t| t.open(id, now)).unwrap_or(-1)
}
pub fn note(slot: i32, length: i64, bytes: i64, now: i64) -> f32 {
    with(|t| t.note(slot, length, bytes, now)).unwrap_or(f32::NAN)
}
pub fn working(id: &str, work: Work) {
    with(|t| t.working(id, work));
}
pub fn work_done(id: &str, work: Work) -> bool {
    with(|t| t.work_done(id, work)).unwrap_or(false)
}
pub fn waits(id: &str, work: Work) -> bool {
    with(|t| t.waits(id, work)).unwrap_or(false)
}
pub fn plan(id: &str, needs: Needs, saved: Option<bool>) -> bool {
    with(|t| t.plan(id, needs, saved)).unwrap_or(false)
}
pub fn analysing_began(id: &str) {
    with(|t| t.analysing_began(id));
}
pub fn analysing_ended(id: &str, stored: bool) {
    with(|t| t.analysing_ended(id, stored));
}
pub fn wants_beats(id: &str) -> bool {
    with(|t| t.wants_beats(id)).unwrap_or(false)
}
pub fn download_phase(id: String) -> Option<DownloadPhase> {
    with(|t| t.phase(&id)).flatten()
}
pub fn processing(now: i64) -> Option<Processing> {
    with(|t| t.processing_at(now)).flatten()
}
pub fn removed(id: &str) -> i32 {
    with(|t| t.removed(id)).unwrap_or(0)
}
pub fn unmark(id: &str) -> i32 {
    with(|t| t.forget(id)).unwrap_or(0)
}
pub fn start_fraction(id: &str) -> f32 {
    with(|t| t.start_fraction(id)).unwrap_or(-1.0)
}
pub fn notice(listed: i32, waiting: bool, now: i64) -> i32 {
    with(|t| t.notice(listed, waiting, now)).unwrap_or(2)
}
pub fn notice_facts<R>(f: impl FnOnce(&Notice) -> R) -> R {
    match active() {
        Some(d) => d.with(|t| f(t.notice_facts())),
        None => f(&Notice::default()),
    }
}
pub fn summary() -> Option<Summary> {
    with(|t| t.summary()).flatten()
}
pub fn row<R>(id: &str, f: impl FnOnce(&str, Option<RowFacts>) -> R) -> R {
    match active() {
        Some(d) => d.with(|t| {
            let (artist, facts) = t.row(id);
            f(artist, facts)
        }),
        None => f("", None),
    }
}
pub fn speed_eta() -> (i64, i64) {
    with(|t| t.speed_eta()).unwrap_or((0, -1))
}

/// Resolves once a mark changed since the last [`download_marks_changed`], so the platform follows
/// processing without polling.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub async fn download_marks_moved() {
    std::future::poll_fn(|cx| active().map_or(Poll::Ready(()), |d| d.poll_moved(cx))).await
}

/// The marks changed since the last call, with their phase now (None: removed).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_marks_changed() -> DownloadMarks {
    with(Tracker::marks_changed).unwrap_or(DownloadMarks { ids: Vec::new(), phases: Vec::new(), at: Vec::new() })
}

/// Gives up stuck processing steps; `now` is the platform clock. Returns ms until the next deadline,
/// -1 when nothing is processing.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_processing_expire(now: i64) -> i64 {
    with(|t| {
        t.clock = Some((now, Instant::now()));
        t.expire()
    })
    .unwrap_or(-1)
}

/// [`speed_eta`] as a list, for checks.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_speed_eta() -> Vec<i64> {
    let (speed, eta) = speed_eta();
    vec![speed, eta]
}

/// The result of queueing songs: `fresh` were added in order; `again` were already queued but unfinished
/// and are requested again. Finished songs are skipped.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadQueued {
    pub fresh: Vec<String>,
    pub again: Vec<String>,
}

/// A download in the platform's queue: media3 state, length (-1 unknown) and bytes.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadKnown {
    pub id: String,
    pub state: i32,
    pub length: i64,
    pub bytes: i64,
}

/// A download an earlier process left failed, with its progress.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadFailed {
    pub id: String,
    pub progress: f32,
}

/// An earlier process's unfinished downloads, sorted out (see `Core::download_recover`).
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadRecovery {
    /// Never reached the platform's queue: request again.
    pub lost: Vec<String>,
    /// Failed there: marked failed here.
    pub failed: Vec<DownloadFailed>,
    /// Finished there but not recorded here: recorded now (their streamed copies can go).
    pub finished: Vec<String>,
    /// Some are queued or interrupted: the platform's queue must be started.
    pub unfinished: bool,
}

/// The downloads screen's lists, in queue order.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadSections {
    pub active: Vec<Song>,
    pub queued: Vec<Song>,
    pub failed: Vec<Song>,
    /// This session's finished songs, newest first.
    pub finished: Vec<Song>,
}

/// Queues the new `rows` (id, song json) behind everything queued; unfinished ones already queued are
/// returned as `again`, finished ones skipped.
pub fn queue_rows(c: &mut Connection, rows: impl IntoIterator<Item = (String, String)>) -> nori_model::Result<DownloadQueued> {
    use rusqlite::OptionalExtension;
    let tx = c.transaction()?;
    let mut out = DownloadQueued::default();
    {
        // The queue is ordered by `ts`; counting on from the newest keeps order even if the clock goes back.
        let newest: i64 = tx.query_row("SELECT coalesce(max(ts), 0) FROM downloads WHERE server=sid()", [], |r| r.get(0))?;
        let mut ts = newest.max(nori_db::now_ms());
        let mut done = tx.prepare_cached("SELECT done FROM downloads WHERE server=sid() AND id=?1")?;
        let mut add = tx.prepare_cached("INSERT INTO downloads(server, id, json, ts) VALUES(sid(), ?1, ?2, ?3)")?;
        let mut seen = HashSet::new();
        for (id, json) in rows.into_iter().filter(|(id, _)| seen.insert(id.clone())) {
            match done.query_row([&id], |r| r.get::<_, bool>(0)).optional()? {
                None => {
                    ts += 1;
                    add.execute(rusqlite::params![id, json, ts])?;
                    out.fresh.push(id);
                }
                Some(false) => out.again.push(id),
                Some(true) => {}
            }
        }
    }
    tx.commit()?;
    Ok(out)
}

/// Sorts `pending` by the platform queue's states; failed ones are returned with length and bytes.
pub fn recovery(pending: &[String], known: &[DownloadKnown]) -> (DownloadRecovery, Vec<(String, i64, i64)>) {
    let by_id: HashMap<&str, &DownloadKnown> = known.iter().map(|k| (k.id.as_str(), k)).collect();
    let mut r = DownloadRecovery::default();
    let mut failed = Vec::new();
    for id in pending {
        match by_id.get(id.as_str()) {
            None => r.lost.push(id.clone()),
            Some(k) => match k.state {
                REMOVING => r.lost.push(id.clone()),
                COMPLETED => r.finished.push(id.clone()),
                FAILED => failed.push((id.clone(), k.length, k.bytes)),
                _ => r.unfinished = true,
            },
        }
    }
    (r, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_counts() {
        let mut b = Batch::default();
        assert!(b.queued("a", "Blue"), "the first song starts a batch");
        assert!(!b.queued("a", "Blue"), "counted once");
        for id in ["b", "c", "d"] {
            b.queued(id, "Blue");
        }
        assert_eq!(b.label(), Some("Blue"));
        b.completed("a");
        assert!((b.fraction(0.5) - 0.375).abs() < 1e-6);
        b.failed("b");
        assert!((b.fraction(1.0) - 0.75).abs() < 1e-6);
        b.queued("b", "Blue");
        assert_eq!((b.total, b.done, b.failed), (4, 1, 0), "a retry is the same song");
        b.failed("b");
        b.removed("b");
        b.removed("c");
        b.removed("x");
        assert_eq!((b.total, b.failed), (2, 0), "cancelled songs leave the count");
        b.queued("e", "");
        assert_eq!(b.label(), None, "a song without an album name");
        b.completed("d");
        b.completed("e");
        assert!(b.queued("f", "Red"), "a drained batch starts over");
        assert_eq!((b.total, b.done, b.label()), (1, 0, Some("Red")));
    }

    #[test]
    fn gate_limits_rate() {
        let (mut last, mut at) = (f32::NAN, 0i64);
        let mut offer = |f: f32, now: i64| {
            let pass = gate(last, at, f, now);
            if pass {
                (last, at) = (f, now);
            }
            pass
        };
        assert!(offer(0.0, 1_000), "the first figure always shows");
        assert!(!offer(0.05, 1_100), "too soon");
        assert!(offer(0.05, 1_250));
        assert!(!offer(0.055, 1_600), "under a percent");
        assert!(offer(0.07, 1_600));
        let passed = (0..2_000).filter(|&t| offer(0.07 + t as f32 * 0.0004, 2_000 + t as i64)).count();
        assert!(passed <= 8, "{passed} updates in two seconds");
    }

    #[test]
    fn gate_passes_finish_and_unknown() {
        assert!(gate(0.995, 0, 1.0, 10), "the finish is not held back by the interval");
        assert!(!gate(1.0, 10, 1.0, 1_000), "and only once");
        assert!(gate(f32::NAN, 0, -1.0, 0));
        assert!(!gate(-1.0, 0, -1.0, 5_000), "still unknown: nothing to redraw");
        assert!(gate(-1.0, 5_000, 0.2, 5_001), "the size arriving shows at once");
        assert!(gate(0.2, 5_001, -1.0, 5_002));
    }

    #[test]
    fn fraction_uses_length_then_estimate() {
        assert_eq!(fraction(200, 100, 999), 0.5);
        assert_eq!(fraction(-1, 100, 400), 0.25);
        assert!(fraction(-1, 900, 400) < 1.0, "an estimate that is too low never reads as finished");
        assert_eq!(fraction(-1, 100, 0), -1.0);
        assert_eq!(expected_bytes(9_000_000, 240, 0), 9_000_000);
        assert_eq!(expected_bytes(9_000_000, 240, 192), 240 * 192 * 125);
        assert_eq!(expected_bytes(9_000_000, 0, 192), 9_000_000);
    }

    fn marks(list: &[(&str, Phase, i64)]) -> HashMap<String, (Phase, i64)> {
        list.iter().map(|&(id, p, at)| (id.to_string(), (p, at))).collect()
    }

    #[test]
    fn sections_run_in_queue_order() {
        let pending = ["e", "d", "c", "b", "a"].map(String::from);
        let done = ["x", "y", "old"].map(String::from);
        let m = marks(&[("a", Phase::Downloading, 0), ("c", Phase::Downloading, 0), ("b", Phase::Failed, 0), ("x", Phase::Done, 5), ("y", Phase::Done, 9)]);
        let [active, queued, failed, finished] = sections(&pending, &done, &m, |s: &String| s.as_str());
        assert_eq!(active, ["a", "c"]);
        assert_eq!(queued, ["d", "e"], "a failure does not hold up the songs behind it");
        assert_eq!(failed, ["b"]);
        assert_eq!(finished, ["y", "x"], "newest first, and only this session's");
    }

    #[test]
    fn just_finished_song_listed() {
        let pending = ["b", "a"].map(String::from);
        let [_, queued, _, finished] = sections(&pending, &[], &marks(&[("a", Phase::Done, 0)]), |s: &String| s.as_str());
        assert_eq!((queued, finished), (vec!["b".to_string()], vec!["a".to_string()]));
    }

    #[test]
    fn processing_lasts_until_lyrics_analysis_and_beats_end() {
        let mut t = Tracker::default();
        t.analysing_began("a");
        t.followed("a", DOWNLOADING, 0);
        assert_eq!(t.phase("a"), Some(DownloadPhase::Downloading));
        t.followed("a", COMPLETED, 1_000);
        assert_eq!(t.marks["a"], (Phase::Processing([true, true, false]), 1_000));
        assert!(t.plan("a", Needs { analysis: true, beats: true }, Some(false)));
        assert_eq!(t.phase("a"), Some(DownloadPhase::FindingLyrics));
        assert!(t.work_done("a", Work::Lyrics));
        assert_eq!(t.phase("a"), Some(DownloadPhase::Analysing));
        // The streaming analysis was not stored: it waits for one from disk.
        t.analysing_ended("a", false);
        assert!(t.waits("a", Work::Analysis));
        assert!(t.work_done("a", Work::Analysis));
        assert_eq!(t.phase("a"), Some(DownloadPhase::DetectingBeats));
        assert!(t.work_done("a", Work::Beats));
        assert_eq!(t.marks["a"], (Phase::Done, 1_000), "done, at its saved time");
        assert!(!t.work_done("a", Work::Lyrics), "done stays done");

        t.followed("ext-b", COMPLETED, 0);
        assert_eq!(t.phase("ext-b"), Some(DownloadPhase::Done), "provider songs get no lyrics");

        // A stored streaming analysis leaves nothing to analyse.
        t.analysing_began("c");
        t.followed("c", COMPLETED, 0);
        t.analysing_ended("c", true);
        assert!(!t.waits("c", Work::Analysis));
        assert_eq!(t.phase("c"), Some(DownloadPhase::FindingLyrics));
        assert_eq!(t.sections(&[], &["c".to_string()], |s: &String| s.as_str())[0], ["c"], "listed as active");

        // A downloaded song reprocessed later: no lyrics step.
        assert!(t.plan("d", Needs { analysis: true, beats: false }, None));
        assert_eq!(t.phase("d"), Some(DownloadPhase::Analysing));
        assert!(!t.plan("e", Needs::default(), None), "nothing to do: no mark");
        assert_eq!(t.phase("e"), None);
    }

    #[test]
    fn needs_rules() {
        let base = Saved { analysable: true, ..Saved::default() };
        let needs_of = |s: Saved| (needs(s).analysis, needs(s).beats);
        assert_eq!(needs_of(Saved { measuring: true, ..base }), (true, false), "streaming analysis in progress");
        assert_eq!(needs_of(base), (true, false), "no current analysis");
        assert_eq!(needs_of(Saved { analysed: true, ..base }), (false, false));
        let ml = Saved { model_on: true, beats_wanted: true, ..base };
        assert_eq!(needs_of(ml), (true, true));
        assert_eq!(needs_of(Saved { analysed: true, ..ml }), (false, true));
        assert_eq!(needs_of(Saved { analysed: true, beats_done: true, ..ml }), (false, false));
        assert_eq!(needs_of(Saved { analysed: true, beats_done: true, measuring: true, ..ml }), (true, true), "measured again: its ends too");
        assert_eq!(needs_of(Saved { beats_wanted: false, ..ml }), (true, false), "not wanted for this download");
        assert_eq!(needs_of(Saved { model_on: false, ..ml }), (true, false), "the model is off");
        assert_eq!(needs_of(Saved { analysable: false, ..ml }), (false, false), "provider song or stream");
    }

    /// A timed-out step is given up; an idle lane is released; a slow live step finishes and is timed.
    #[test]
    fn expire_gives_up_stuck_steps() {
        let mut t = Tracker { test_clock: Some(0), ..Tracker::default() };
        for id in ["ex-a", "ex-b"] {
            t.followed(id, COMPLETED, 0);
            t.plan(id, Needs { analysis: true, beats: id == "ex-a" }, Some(true));
        }
        assert_eq!(t.processing(), [2, 2, 1]);
        // A lookup that never answers, and an analysis that runs.
        t.working("ex-a", Work::Lyrics);
        t.working("ex-a", Work::Analysis);
        t.test_clock = Some(20_000);
        let lyrics_ms = Work::Lyrics.limit_ms();
        assert_eq!(t.expire(), lyrics_ms - 20_000, "the lookup's deadline comes first");
        t.test_clock = Some(lyrics_ms);
        t.expire();
        assert!(!t.waits("ex-a", Work::Lyrics), "the lookup is given up");
        assert!(t.waits("ex-a", Work::Analysis), "the analysis runs on");
        assert!(t.waits("ex-b", Work::Lyrics), "the next lookup waits its turn");
        // The analysis is slow but alive: it ends well past the lookup's time, within its own.
        t.test_clock = Some(100_000);
        assert!(t.work_done("ex-a", Work::Analysis));
        t.working("ex-a", Work::Beats);
        assert!((t.paces[Work::Analysis as usize].per_song_s(Work::Analysis) - (100.0 + 2.0 * 5.0) / 3.0).abs() < 1e-9, "learned from the step");
        // The lyrics lane moved on to nothing for its whole spell: what waits there is let go.
        t.expire();
        assert!(!t.waits("ex-b", Work::Lyrics));
        assert!(t.waits("ex-b", Work::Analysis), "the measuring lane is busy with ex-a's beats, not stuck");
        // The beat model's run ends; ex-b's analysis follows.
        t.test_clock = Some(160_000);
        assert!(t.work_done("ex-a", Work::Beats));
        assert_eq!(t.marks["ex-a"].0, Phase::Done);
        t.working("ex-b", Work::Analysis);
        t.test_clock = Some(160_000 + Work::Analysis.limit_ms());
        assert_eq!(t.expire(), -1, "given up at its time: nothing is left processing");
        assert_eq!(t.marks["ex-b"].0, Phase::Done);
    }

    #[test]
    fn marks_changed_reports_only_moved_marks() {
        let mut t = Tracker::default();
        t.mark("a", Phase::Downloading, 1);
        t.mark("b", Phase::Failed, 2);
        let mine = |m: &DownloadMarks, id: &str| m.ids.iter().position(|i| i == id).map(|i| m.phases[i]);
        let m = t.marks_changed();
        assert_eq!((mine(&m, "a"), mine(&m, "b")), (Some(Some(DownloadPhase::Downloading)), Some(Some(DownloadPhase::Failed))));
        t.unmark("a");
        let m = t.marks_changed();
        assert_eq!((mine(&m, "a"), mine(&m, "b")), (Some(None), None), "removed, and b did not move");
    }

    #[test]
    fn no_slot_notes_nothing() {
        let mut t = Tracker::default();
        let slot = t.open("a", 0);
        assert!(t.note(-1, 1000, 500, 10).is_nan());
        assert_eq!(t.slots[slot as usize].bytes, 0);
    }

    #[test]
    fn row_facts_only_while_running() {
        let slot = Slot { id: "r".into(), estimate: 0, length: 1000, bytes: 450, started_at: 0, gate_value: 0.0, gate_at: 0, speed_bytes: 0, speed_at: 0, rate: 0.0, live: true };
        assert_eq!(row_facts(std::slice::from_ref(&slot), "r"), Some(RowFacts { percent: 45, speed_bps: 0, eta_s: -1 }));
        assert_eq!(row_facts(std::slice::from_ref(&slot), "other"), None);
    }

    /// A tracker knowing `ids` weigh `size` each (0 unknown).
    fn tracker(ids: &[&str], size: i64) -> Tracker {
        let mut t = Tracker::default();
        for id in ids {
            t.info.insert(id.to_string(), Info { estimate: size, ..Info::default() });
        }
        t
    }

    /// Two concurrent songs: the batch speed sums them and the time left follows.
    #[test]
    fn two_songs_speed_and_eta() {
        // Provider songs: no lyrics are looked up after them.
        let mut t = tracker(&["ext-sp-a", "ext-sp-b"], 1_000_000);
        for id in ["ext-sp-a", "ext-sp-b"] {
            t.followed(id, DOWNLOADING, 0);
        }
        let (a, b) = (t.open("ext-sp-a", 0), t.open("ext-sp-b", 0));
        for k in 0..=4i64 {
            t.note(a, 1_000_000, k * 100_000, k * 500);
            t.note(b, 1_000_000, k * 50_000, k * 500);
        }
        t.notice(2, false, 2_000);
        let (speed, eta) = (t.speed_bps, t.eta_s);
        assert!((290_000..=310_000).contains(&speed), "200 kB/s and 100 kB/s together: {speed}");
        // 600 kB and 800 kB still to come at 300 kB/s.
        assert!((4..=5).contains(&eta), "{eta} s left");
    }

    /// Six 2 MB songs, two at a time at 200 kB/s each: speed stays ~400 kB/s and the time left counts
    /// down steadily as songs end and start.
    #[test]
    fn speed_and_eta_steady_across_songs() {
        let ids: Vec<String> = (0..6).map(|i| format!("ext-st-{i}")).collect();
        let names: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut t = tracker(&names, 2_000_000);
        for id in &names {
            t.followed(id, QUEUED, 0);
        }
        let mut next = 0;
        // (song, slot, bytes)
        let mut running: Vec<(usize, i32, i64)> = Vec::new();
        let mut start = |t: &mut Tracker, running: &mut Vec<(usize, i32, i64)>, now: i64| {
            if next < names.len() {
                t.followed(names[next], DOWNLOADING, now);
                let slot = t.open(names[next], now);
                t.note(slot, 2_000_000, 0, now);
                running.push((next, slot, 0));
                next += 1;
            }
        };
        start(&mut t, &mut running, 0);
        start(&mut t, &mut running, 0);
        let mut said = Vec::new();
        for tick in 1..=120i64 {
            let now = tick * 250;
            for r in running.iter_mut() {
                r.2 += 50_000;
                t.note(r.1, 2_000_000, r.2, now);
            }
            while let Some(i) = running.iter().position(|r| r.2 >= 2_000_000) {
                let (song, _, _) = running.remove(i);
                t.followed(names[song], COMPLETED, now);
                start(&mut t, &mut running, now);
            }
            if now % 1_000 == 0 && !running.is_empty() {
                t.notice(names.len() as i32 - t.batch.done, false, now);
                said.push((now, t.speed_bps, t.eta_s));
            }
        }
        assert!(said.len() > 20);
        for &(now, speed, eta) in said.iter().filter(|s| s.0 >= 2_000) {
            assert!((380_000..=420_000).contains(&speed), "{speed} B/s at {now} ms");
            let truth = 30 - now / 1_000;
            assert!((eta - truth).abs() <= 2, "{eta} s left at {now} ms, {truth} s really");
        }
        for w in said.windows(2).filter(|w| w[0].0 >= 2_000) {
            let step = w[0].2 - w[1].2;
            assert!((0..=2).contains(&step), "a second on, the time left went from {} to {}", w[0].2, w[1].2);
        }
    }

    /// Unstarted songs are sized by finished ones: estimates scaled by actual/estimate, else the average.
    #[test]
    fn remaining_sized_from_finished() {
        let mut t = tracker(&["ext-w-a", "ext-w-b"], 4_000_000);
        t.info.insert("ext-w-c".into(), Info::default());
        for id in ["ext-w-a", "ext-w-b", "ext-w-c"] {
            t.followed(id, QUEUED, 0);
        }
        t.followed("ext-w-a", DOWNLOADING, 0);
        let a = t.open("ext-w-a", 0);
        t.note(a, 5_000_000, 0, 0);
        t.note(a, 5_000_000, 5_000_000, 10_000);
        t.followed("ext-w-a", COMPLETED, 10_000);
        t.notice(2, false, 10_000);
        // b was estimated at 4 MB, and a, estimated the same, weighed 5; c has no estimate: the average.
        assert_eq!(t.remaining_bytes, 5_000_000 + 5_000_000);
    }

    #[test]
    fn resumed_download_no_speed_burst() {
        let mut t = tracker(&["ext-r"], 8_000_000);
        t.followed("ext-r", DOWNLOADING, 0);
        let slot = t.open("ext-r", 0);
        for k in 0..=6i64 {
            t.note(slot, 8_000_000, 3_000_000 + k * 100_000, k * 500);
        }
        t.notice(1, false, 3_000);
        assert!((190_000..=210_000).contains(&t.speed_bps), "{} B/s", t.speed_bps);
        // Asked for again after it stopped: one slot, not two.
        t.open("ext-r", 3_000);
        assert_eq!(t.slots.iter().filter(|s| s.live).count(), 1);
    }

    #[test]
    fn time_left_lanes() {
        let per = [3.0, 5.0, 40.0];
        let after = |waiting: [f64; 3], to_come: [f64; 3], tail: bool| After { waiting, to_come, per, tail };
        assert_eq!(time_left(None, &after([1.0; 3], [1.0; 3], true)), -1.0, "no speed yet");
        assert_eq!(time_left(Some(10.0), &after([0.0; 3], [0.0; 3], false)), 10.0);
        // The last song's lookup comes after its last byte.
        assert_eq!(time_left(Some(10.0), &after([0.0; 3], [2.0, 0.0, 0.0], false)), 13.0);
        // Many lookups queued up take longer than the bytes.
        assert_eq!(time_left(Some(10.0), &after([4.0, 0.0, 0.0], [2.0, 0.0, 0.0], false)), 18.0);
        assert_eq!(time_left(Some(0.0), &after([3.0, 0.0, 0.0], [0.0; 3], false)), 9.0, "the bytes are in; the lookups are left");
        assert_eq!(time_left(Some(5.0), &after([0.0; 3], [0.0; 3], true)), 5.0 + ANALYSIS_TAIL_S);
        // The measuring lane runs beside the lookups: two analyses and three model runs, one song at a time.
        assert_eq!(time_left(Some(0.0), &after([3.0, 2.0, 3.0], [0.0; 3], false)), 2.0 * 5.0 + 3.0 * 40.0, "the longer lane");
        // Songs still to come: the last one's analysis and model run come after the last byte.
        assert_eq!(time_left(Some(100.0), &after([0.0; 3], [0.0, 0.5, 1.0], false)), 100.0 + 0.5 * 5.0 + 40.0);
        assert_eq!(time_left(Some(10.0), &after([0.0, 0.0, 4.0], [0.0, 0.0, 2.0], false)), 6.0 * 40.0, "the model's queue outlasts the bytes");
    }

    /// After the bytes, the time left follows each lane's work, and step times are learned (instant
    /// lyrics lookups pull the lyrics estimate towards zero).
    #[test]
    fn processing_counted_down_and_timed() {
        let mut t = tracker(&["ly-a", "ly-b"], 1_000_000);
        t.test_clock = Some(0);
        for id in ["ly-a", "ly-b"] {
            t.followed(id, DOWNLOADING, 0);
            let slot = t.open(id, 0);
            t.note(slot, 1_000_000, 0, 0);
            t.note(slot, 1_000_000, 1_000_000, 2_000);
        }
        t.notice(2, false, 2_000);
        for id in ["ly-a", "ly-b"] {
            t.followed(id, COMPLETED, 2_000);
            // Neither was measured as it came; both are wanted by the beat model.
            t.plan(id, Needs { analysis: true, beats: true }, Some(true));
        }
        assert_eq!((t.saved, t.from_disk), (2, 2));
        let s = t.summary().unwrap();
        assert_eq!((s.title, s.done), (SummaryTitle::Downloaded, 2));
        assert_eq!(t.processing(), [2, 2, 2], "saved, not done");
        // At the guesses: two lookups beside two analyses and two model runs.
        t.notice(0, false, 3_000);
        assert_eq!(t.eta_s, (2.0 * 5.0 + 2.0f64 * 40.0) as i64);
        // Song by song: each lookup takes 4 s, each analysis 2 s, each model run 20 s.
        let mut clock = 0;
        let mut step = |t: &mut Tracker, id: &str, w: Work, ms: i64| {
            t.working(id, w);
            clock += ms;
            t.test_clock = Some(clock);
            assert!(t.work_done(id, w));
        };
        step(&mut t, "ly-a", Work::Lyrics, 4_000);
        step(&mut t, "ly-a", Work::Analysis, 2_000);
        assert_eq!(t.processing(), [1, 1, 2], "the counts go down song by song");
        step(&mut t, "ly-a", Work::Beats, 20_000);
        assert_eq!(t.marks["ly-a"].0, Phase::Done);
        step(&mut t, "ly-b", Work::Lyrics, 4_000);
        step(&mut t, "ly-b", Work::Analysis, 2_000);
        assert_eq!(t.processing(), [0, 0, 1]);
        assert_eq!(t.phase("ly-b"), Some(DownloadPhase::DetectingBeats), "detecting beats");
        step(&mut t, "ly-b", Work::Beats, 20_000);
        assert_eq!(t.processing(), [0, 0, 0]);
        // Learned: 8 s over two lookups, 4 s over two analyses, 40 s over two model runs, each with its guess.
        let per = |t: &Tracker, w: Work| t.paces[w as usize].per_song_s(w);
        assert!((per(&t, Work::Lyrics) - (8.0 + 2.0 * 3.0) / 4.0).abs() < 1e-9);
        assert!((per(&t, Work::Analysis) - (4.0 + 2.0 * 5.0) / 4.0).abs() < 1e-9);
        assert!((per(&t, Work::Beats) - (40.0 + 2.0 * 40.0) / 4.0).abs() < 1e-9);
        // The next batch's allowance is the learned one.
        t.followed("ly-c", COMPLETED, 60_000);
        t.plan("ly-c", Needs { analysis: true, beats: true }, Some(true));
        let a = t.after(0, 0, false);
        assert_eq!(a.waiting, [1.0, 1.0, 1.0]);
        assert!((time_left(Some(0.0), &a) - (per(&t, Work::Analysis) + per(&t, Work::Beats))).abs() < 1e-9);
        for w in Work::ALL {
            t.work_done("ly-c", w);
        }
        let before = per(&t, Work::Lyrics);
        for i in 0..20 {
            let id = format!("ly-d{i}");
            t.marks.insert(id.clone(), (Phase::Processing([true, false, false]), 0));
            t.work_done(&id, Work::Lyrics);
        }
        assert!(per(&t, Work::Lyrics) < before / 4.0, "lookups ended at once weigh it down: {}", per(&t, Work::Lyrics));
        // An analysis found done without a step (measured as it came) says nothing of how long one takes.
        let analysis = per(&t, Work::Analysis);
        t.marks.insert("ly-e".into(), (Phase::Processing([false, true, false]), 0));
        t.work_done("ly-e", Work::Analysis);
        assert_eq!(per(&t, Work::Analysis), analysis);
    }

    #[test]
    fn processing_facts_follow_the_marks() {
        let mut t = Tracker::default();
        assert!(t.processing_at(0).is_none());
        t.plan("a", Needs { analysis: true, beats: true }, None);
        t.plan("b", Needs { analysis: false, beats: true }, None);
        let p = t.processing_at(1_000).unwrap();
        assert_eq!((p.lyrics, p.analysing, p.beats), (0, 1, 2));
        assert!(p.eta_s > 0);
        let moved = t.marks_changed();
        let b = moved.ids.iter().position(|i| i == "b").unwrap();
        assert!(moved.ids.contains(&"a".to_string()) && moved.phases[b] == Some(DownloadPhase::DetectingBeats));
        for id in ["a", "b"] {
            t.work_done(id, Work::Analysis);
            t.work_done(id, Work::Beats);
        }
        assert_eq!(t.marks["b"].0, Phase::Done);
        assert!(t.processing_at(2_000).is_none());
    }

    /// After the last byte: no speed, the time left counts down the pending lyrics, then -1.
    #[test]
    fn eta_after_last_byte() {
        let mut t = tracker(&["af-a", "af-b"], 1_000_000);
        for id in ["af-a", "af-b"] {
            t.followed(id, DOWNLOADING, 0);
            let slot = t.open(id, 0);
            t.note(slot, 1_000_000, 0, 0);
            t.note(slot, 1_000_000, 1_000_000, 2_000);
        }
        t.notice(2, false, 2_000);
        assert!(t.speed_bps > 0);
        for id in ["af-a", "af-b"] {
            t.followed(id, COMPLETED, 2_000);
        }
        // The platform's last notice, asked after the bytes were in.
        t.notice(0, false, 2_500);
        assert_eq!(t.speed_bps, 0, "nothing is coming");
        // Two lookups at the 3 s guess, read a second apart: it counts down.
        let (speed, first) = t.speed_eta_at(3_000);
        assert_eq!((speed, first), (0, 6));
        assert_eq!(t.speed_eta_at(4_000), (0, 5));
        t.work_done("af-a", Work::Lyrics);
        let (_, one_left) = t.speed_eta_at(5_000);
        assert!((1..=4).contains(&one_left), "one lookup left: {one_left}");
        t.work_done("af-b", Work::Lyrics);
        assert_eq!(t.speed_eta_at(6_000), (0, -1), "all processed");
        assert_eq!(t.speed_eta_at(60_000), (0, -1), "and it stays so");
    }

    #[test]
    fn beats_offer_answers() {
        use BeatsOffer::*;
        for (offer, asked, wants) in [(Off, true, false), (No, true, false), (Yes, false, true), (Ask, true, true), (Ask, false, false)] {
            assert_eq!(offer.wants(asked), wants, "{offer:?} {asked}");
        }
    }

    /// The notification's title, the last song settling the batch, and the finished marks kept.
    #[test]
    fn notice_kinds_drain_and_recent_marks() {
        let mut t = tracker(&["ext-a", "ext-b"], 1_000_000);
        t.info.get_mut("ext-a").unwrap().title = "Song".into();
        assert_eq!((t.start_fraction("ext-a"), t.start_fraction("ext-x")), (0.0, -1.0));
        assert_eq!(t.followed("ext-a", QUEUED, 0), NEW_BATCH);
        t.open("ext-a", 0);
        t.notice(1, false, 0);
        assert_eq!(t.notice.kind, NoticeKind::OneNamed);
        t.notice(1, true, 500);
        assert_eq!(t.notice.kind, NoticeKind::Waiting);
        t.followed("ext-b", QUEUED, 0);
        t.notice(2, false, 1_000);
        assert_eq!((t.notice.kind, t.notice.total), (NoticeKind::Many, 2));
        assert_eq!(t.removed("ext-b"), 0, "one still open");
        t.followed("ext-a", DOWNLOADING, 0);
        assert_eq!(t.removed("ext-a"), MARKS | DRAINED);
        assert_eq!(t.forget("ext-a"), 0);
        for i in 0..RECENT as i64 + 2 {
            t.followed(&format!("ext-d{i}"), COMPLETED, i);
        }
        assert_eq!(t.marks.len(), RECENT, "only the latest finished marks stay");
        assert!(!t.marks.contains_key("ext-d0") && t.marks.contains_key("ext-d2"));
    }

    /// A server switch wakes the platform's waiter, so it waits on the new core's marks.
    #[test]
    fn switch_wakes_mark_waiter() {
        use std::future::Future;
        struct Woke(std::sync::atomic::AtomicBool);
        impl std::task::Wake for Woke {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let open = || Arc::new(Downloads::load(&Arc::new(Mutex::new(nori_db::open("", "t").unwrap()))).unwrap());
        let (old, new) = (open(), open());
        old.activate();
        let woke = Arc::new(Woke(Default::default()));
        let waker = Waker::from(woke.clone());
        let mut moved = std::pin::pin!(download_marks_moved());
        assert!(moved.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
        new.activate();
        assert!(woke.0.load(Ordering::SeqCst));
        new.with(|t| t.mark("a", Phase::Failed, 0));
        assert!(moved.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_ready(), "the new core's marks");
    }

    #[test]
    fn countdown_smooths_jitter() {
        let mut c = Countdown::default();
        let mut said = vec![c.next(60.0, 0)];
        for k in 1..=20i64 {
            let jitter = if k % 2 == 0 { 1.5 } else { -1.5 };
            said.push(c.next(60.0 - k as f64 + jitter, k * 1_000));
        }
        for w in said.windows(2) {
            assert!((0..=2).contains(&(w[0] - w[1])), "{said:?}");
        }
        // Twice as far to go: said at once.
        assert_eq!(c.next(80.0, 21_000), 80);
        assert_eq!(c.next(-1.0, 22_000), -1);
    }
}
