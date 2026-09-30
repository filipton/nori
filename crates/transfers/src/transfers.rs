//! Download tracking: per-song progress, batch speed and time left, the processing after the bytes
//! (lyrics, analysis, beat model), and the notification's facts. The platform moves the bytes and words
//! the facts. The per-chunk report ([`note`]) looks nothing up by name and allocates nothing.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Poll, Waker};

use nori_model::{alog, DownloadPhase, Song};
use parking_lot::Mutex;

/// Finished songs the downloads screen keeps listing this session.
pub const RECENT: usize = 50;
/// Progress is passed on at most this often, and only after a whole percent.
const GATE_MS: i64 = 250;
const GATE_STEP: f32 = 0.01;
/// Progress against an estimated size stops short of full, since estimates can be low.
const ESTIMATE_CEILING: f32 = 0.97;
/// A song's guessed size when nothing in the batch gives one.
const UNKNOWN_SONG_BYTES: i64 = 8_000_000;
/// The batch speed's averaging time constant.
const RATE_TAU_MS: f64 = 10_000.0;
/// The shortest speed sample.
const RATE_SAMPLE_MS: i64 = 500;
/// No time left is given before the speed has been measured this long.
const RATE_WARM_MS: f64 = 1_500.0;
/// Prior per-song duration of each [`Work`] step, weighted as `PACE_PRIOR` songs against timed steps.
const LYRICS_GUESS_MS: f64 = 3_000.0;
const ANALYSIS_GUESS_MS: f64 = 5_000.0;
const BEATS_GUESS_MS: f64 = 40_000.0;
const PACE_PRIOR: f64 = 2.0;
/// How long an analysis measured while downloading may run past the last byte.
const ANALYSIS_TAIL_S: f64 = 2.0;
/// Prior share of songs needing an analysis from disk, weighted as `FROM_DISK_PRIOR` songs.
const FROM_DISK_GUESS: f64 = 0.5;
const FROM_DISK_PRIOR: f64 = 1.0;

// media3's `Download.STATE_*`.
pub const QUEUED: i32 = 0;
pub(crate) const STOPPED: i32 = 1;
pub const DOWNLOADING: i32 = 2;
pub const COMPLETED: i32 = 3;
pub const FAILED: i32 = 4;
pub(crate) const RESTARTING: i32 = 7;

/// Flags [`followed`] and [`removed`] return: a batch started, the batch drained, marks changed.
pub const NEW_BATCH: i32 = 1;
pub const DRAINED: i32 = 2;
pub const MARKS: i32 = 4;

/// How long one song's step may run before it is given up. Generous: steps run at the lowest priority,
/// so a limit only catches a stuck step.
pub(crate) const LYRICS_STEP_MS: i64 = 30_000;
pub(crate) const ANALYSIS_STEP_MS: i64 = 180_000;
pub(crate) const BEATS_STEP_MS: i64 = 600_000;
/// How long songs may wait in a lane with no step running (its worker gone) before they are let go.
pub(crate) const LANE_IDLE_MS: i64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Failed,
    Done,
    /// Saved, with work after the bytes still pending.
    Processing { analysing: bool, lyrics: bool, beats: bool },
}

impl Phase {
    /// The phase the screens show; processing shows lyrics, else analysis, else beats.
    pub fn shown(self) -> DownloadPhase {
        match self {
            Phase::Downloading => DownloadPhase::Downloading,
            Phase::Failed => DownloadPhase::Failed,
            Phase::Done => DownloadPhase::Done,
            Phase::Processing { lyrics: true, .. } => DownloadPhase::FindingLyrics,
            Phase::Processing { analysing: true, .. } => DownloadPhase::Analysing,
            Phase::Processing { .. } => DownloadPhase::DetectingBeats,
        }
    }

    /// `Processing`, or `Done` when nothing is pending.
    fn processing(analysing: bool, lyrics: bool, beats: bool) -> Phase {
        if analysing || lyrics || beats { Phase::Processing { analysing, lyrics, beats } } else { Phase::Done }
    }

    /// Whether `work` is still to do.
    fn waits(self, work: Work) -> bool {
        match (self, work) {
            (Phase::Processing { analysing, .. }, Work::Analysis) => analysing,
            (Phase::Processing { lyrics, .. }, Work::Lyrics) => lyrics,
            (Phase::Processing { beats, .. }, Work::Beats) => beats,
            _ => false,
        }
    }

    /// This phase with `work` over.
    fn without(self, work: Work) -> Phase {
        match self {
            Phase::Processing { analysing, lyrics, beats } => match work {
                Work::Analysis => Phase::processing(false, lyrics, beats),
                Work::Lyrics => Phase::processing(analysing, false, beats),
                Work::Beats => Phase::processing(analysing, lyrics, false),
            },
            p => p,
        }
    }
}

/// Work after the bytes. Lyrics run in one lane, analysis and beats share another (one decode feeds
/// both); each lane takes one song at a time, the two lanes in parallel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Analysis,
    Lyrics,
    Beats,
}

impl Work {
    const ALL: [Work; 3] = [Work::Lyrics, Work::Analysis, Work::Beats];

    fn index(self) -> usize {
        match self {
            Work::Lyrics => 0,
            Work::Analysis => 1,
            Work::Beats => 2,
        }
    }

    /// 0: lyrics, 1: analysis and beats.
    fn lane(self) -> usize {
        match self {
            Work::Lyrics => 0,
            Work::Analysis | Work::Beats => 1,
        }
    }

    fn limit_ms(self) -> i64 {
        match self {
            Work::Lyrics => LYRICS_STEP_MS,
            Work::Analysis => ANALYSIS_STEP_MS,
            Work::Beats => BEATS_STEP_MS,
        }
    }

    fn guess_ms(self) -> f64 {
        match self {
            Work::Lyrics => LYRICS_GUESS_MS,
            Work::Analysis => ANALYSIS_GUESS_MS,
            Work::Beats => BEATS_GUESS_MS,
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

/// [`BeatsOffer`] from whether the model is on and the `download_beats` setting.
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
        match self {
            BeatsOffer::Off | BeatsOffer::No => false,
            BeatsOffer::Yes => true,
            BeatsOffer::Ask => answer,
        }
    }
}

/// The setting a remembered answer sets.
pub fn beats_remembered(yes: bool) -> nori_settings::settings::DownloadBeats {
    if yes {
        nori_settings::settings::DownloadBeats::Always
    } else {
        nori_settings::settings::DownloadBeats::Never
    }
}

/// Monotonic ms for timing steps. Global: the process epoch.
fn mono_ms() -> i64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as i64
}

/// A downloading song's details, read once from the downloads table.
#[derive(Debug, Clone, Default)]
pub struct Info {
    title: String,
    album: String,
    artist: String,
    /// Expected size, bytes (0 unknown).
    pub estimate: i64,
}

#[derive(Debug, Clone)]
struct Slot {
    id: String,
    estimate: i64,
    length: i64,
    bytes: i64,
    started_at: i64,
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
            self.failed_ids.clear();
            self.labels.clear();
            (self.total, self.done, self.failed, self.done_bytes, self.done_sized) = (0, 0, 0, 0, 0);
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
}

/// The running batch's notification facts ([`notice`]).
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

/// Every reported download, the batch and the notification state.
#[derive(Debug, Default)]
pub struct Tracker {
    slots: Vec<Slot>,
    batch: Batch,
    pub marks: HashMap<String, (Phase, i64)>,
    /// Songs whose mark changed since [`download_marks_changed`] was last called.
    pub changed: HashSet<String>,
    info: HashMap<String, Info>,
    /// Songs being analysed as their bytes arrive.
    analysing: HashSet<String>,
    /// Waiters on mark changes ([`download_marks_moved`]), woken when `wake` is set.
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
    /// Per-[`Work`] step timing, by [`Work::index`].
    paces: [Pace; 3],
    /// The step running in each [`Work::lane`].
    lanes: [Lane; 2],
    /// Downloads the beat model reads once saved (mirrors the downloads table).
    beats_wanted: HashSet<String>,
    /// Songs saved in this process, and how many needed an analysis from disk.
    saved: i64,
    from_disk: i64,
    countdown: Countdown,
    /// The platform's last `now` and the local instant it was given, to extrapolate the platform clock.
    clock: Option<(i64, std::time::Instant)>,
    /// Actual vs estimated size of finished songs, to scale remaining estimates.
    sized_actual: i64,
    sized_estimate: i64,
    /// Overrides [`mono_ms`] in tests.
    test_clock: Option<i64>,
}

/// The batch speed: an exponential average over [`RATE_TAU_MS`], normalised by its accumulated weight so
/// the first seconds are the plain average.
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

    /// Bytes a second, 0 unknown.
    fn rate(&self) -> f64 {
        if self.weight > 0.0 {
            self.ema / self.weight
        } else {
            0.0
        }
    }

    /// Measured for at least [`RATE_WARM_MS`].
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
    fn took(&mut self, ms: i64) {
        self.took_ms += ms.max(0);
        self.done += 1;
    }

    fn per_song_s(&self, work: Work) -> f64 {
        ((self.took_ms as f64 + work.guess_ms() * PACE_PRIOR) / (self.done as f64 + PACE_PRIOR) / 1000.0).min(work.limit_ms() as f64 / 1000.0)
    }
}

/// One lane: the running step (song, work, start on [`mono_ms`]) and when the lane last moved.
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

/// Input to [`time_left`], per [`Work::index`]: songs waiting, songs still to come that will need it, and
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
        for w in Work::ALL.into_iter().filter(|w| w.lane() == lane) {
            let i = w.index();
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

/// Global: the platform reports downloads through JNI/FFI entry points with no handle.
static TRACKER: Mutex<Option<Tracker>> = Mutex::new(None);

/// Runs `f` on the tracker, then wakes waiters after the lock is released.
pub fn with<R>(f: impl FnOnce(&mut Tracker) -> R) -> R {
    let mut guard = TRACKER.lock();
    let t = guard.get_or_insert_with(Tracker::default);
    let r = f(t);
    let wakers = if std::mem::take(&mut t.wake) { std::mem::take(&mut t.wakers) } else { Vec::new() };
    drop(guard);
    wakers.into_iter().for_each(Waker::wake);
    r
}

/// Expected download size: duration at the transcode bitrate, or the file size at original quality.
pub(crate) fn expected_bytes(size_bytes: i64, duration_s: i64, bitrate_kbps: i32) -> i64 {
    if bitrate_kbps > 0 && duration_s > 0 {
        duration_s * bitrate_kbps as i64 * 125
    } else {
        size_bytes
    }
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

fn download_song(c: &rusqlite::Connection, id: &str) -> Option<Song> {
    let json: String = c.query_row("SELECT json FROM downloads WHERE server=sid() AND id=?1", [id], |r| r.get(0)).ok()?;
    serde_json::from_str::<Song>(&json).ok()
}

/// The queued `songs`' titles and sizes, so reports about them never wait for the database.
pub fn know(songs: &[Song]) {
    with(|t| songs.iter().for_each(|s| t.keep_info(&s.id, Some(s))));
}

impl Tracker {
    /// What [`know`] was told about `id`, else what the database says when it is free: the tracker's
    /// callers include the main thread and `@CriticalNative` doors, which must not wait for it. None while
    /// it is busy (nothing cached then).
    pub fn info(&mut self, id: &str) -> Option<&Info> {
        if !self.info.contains_key(id) {
            let db = nori_db::active()?;
            let c = db.try_lock()?;
            let found = download_song(&c, id);
            drop(c);
            self.keep_info(id, found.as_ref());
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

    pub fn close(&mut self, id: &str) {
        if let Some(i) = self.slot_of(id) {
            self.slots[i].live = false;
        }
    }

    fn mark(&mut self, id: &str, phase: Option<Phase>, now: i64) -> bool {
        match phase {
            Some(p) if self.marks.get(id).map(|m| m.0) == Some(p) && p == Phase::Downloading => false,
            Some(p) => {
                self.marks.insert(id.to_string(), (p, now));
                self.changed.insert(id.to_string());
                self.wake = true;
                if p == Phase::Done {
                    self.recent_only();
                }
                true
            }
            None => self.unmark(id),
        }
    }

    /// An earlier process left `id` failed: marked so, unless this one already marked it.
    pub fn failed_before(&mut self, id: &str) {
        if !self.marks.contains_key(id) {
            self.mark(id, Some(Phase::Failed), 0);
        }
    }

    /// Takes `id`'s mark away; true when it had one.
    pub fn unmark(&mut self, id: &str) -> bool {
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
        if done.len() <= RECENT {
            return;
        }
        done.sort();
        for (_, id) in done.iter().take(done.len() - RECENT) {
            self.unmark(id);
        }
    }

    fn running(&self) -> usize {
        self.marks.values().filter(|m| m.0 == Phase::Downloading).count()
    }

    /// Saved songs waiting for each [`Work`], by [`Work::index`].
    fn processing(&self) -> [i32; 3] {
        let mut n = [0; 3];
        for m in self.marks.values() {
            for w in Work::ALL {
                if m.0.waits(w) {
                    n[w.index()] += 1;
                }
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
        let share = (self.from_disk as f64 + FROM_DISK_GUESS * FROM_DISK_PRIOR) / (self.saved as f64 + FROM_DISK_PRIOR);
        After {
            waiting: [n[0] as f64, (n[1] as f64 - arriving).max(0.0), n[2] as f64],
            to_come: [to_come as f64, to_come as f64 * share, beats_to_come as f64],
            per: Work::ALL.map(|w| self.paces[w.index()].per_song_s(w)),
            tail: tail || arriving > 0.0,
        }
    }

    fn mono(&self) -> i64 {
        self.test_clock.unwrap_or_else(mono_ms)
    }
}

/// Reads the download bitrate setting for size estimates; a change drops the cached estimates.
pub fn follow_quality() {
    let kbps = nori_settings::settings_store::prefs(|p| p.download.bit_rate);
    with(|t| {
        if t.download_kbps != kbps {
            t.download_kbps = kbps;
            t.info.clear();
            (t.sized_actual, t.sized_estimate) = (0, 0);
        }
    });
}

/// media3 reported `id` in `state`. Returns [`NEW_BATCH`], [`DRAINED`] and [`MARKS`] flags.
pub fn followed(id: &str, state: i32, now: i64) -> i32 {
    with(|t| t.followed(id, state, now))
}

impl Tracker {
    fn followed(&mut self, id: &str, state: i32, now: i64) -> i32 {
        let t = self;
        t.clock = Some((now, std::time::Instant::now()));
        let id = id.to_string();
        let was_open = t.batch.open.contains(&id);
        let mut flags = 0;
        match state {
            QUEUED | DOWNLOADING | RESTARTING | STOPPED => {
                let label = t.info(&id).map(|i| i.album.clone()).unwrap_or_default();
                if t.batch.queued(&id, &label) {
                    flags |= NEW_BATCH;
                }
            }
            COMPLETED => {
                // Record its size for estimating the rest.
                if let (true, Some(i)) = (t.batch.open.contains(&id), t.slot_of(&id)) {
                    let s = &t.slots[i];
                    let size = if s.length > 0 { s.length } else { s.bytes };
                    if size > 0 {
                        t.batch.done_bytes += size;
                        t.batch.done_sized += 1;
                        if s.estimate > 0 {
                            t.sized_actual += size;
                            t.sized_estimate += s.estimate;
                        }
                    }
                }
                t.batch.completed(&id)
            }
            FAILED => t.batch.failed(&id),
            _ => {}
        }
        let phase = match state {
            DOWNLOADING => Some(Phase::Downloading),
            // Provider songs get no lyrics lookup; other needs come later through `plan`.
            COMPLETED => Some(Phase::processing(t.analysing.contains(&id), !nori_model::is_provider_id(&id), false)),
            FAILED => Some(Phase::Failed),
            _ => None,
        };
        if matches!(state, COMPLETED | FAILED) {
            t.close(&id);
        }
        if let Some(Phase::Processing { analysing, lyrics, .. }) = phase {
            t.entered(lyrics, analysing);
        }
        if t.mark(&id, phase, now) {
            flags |= MARKS;
            match phase {
                Some(Phase::Downloading) => alog::info(&format!("start {id}: {} downloading at once", t.running())),
                Some(Phase::Failed) => alog::info(&format!("failed {id}")),
                _ => {}
            }
        }
        if was_open && t.batch.open.is_empty() {
            flags |= DRAINED;
            t.drained();
        }
        flags
    }

    /// The batch's last song settled: reset speed and time left, which [`notice`] no longer updates.
    fn drained(&mut self) {
        (self.speed_bps, self.eta_s) = (0, -1);
        self.countdown = Countdown::default();
    }

    /// Work arrived for a lane; an idle lane starts its idle timer now.
    fn entered(&mut self, lyrics: bool, measuring: bool) {
        let now = self.mono();
        for (lane, came) in [(0, lyrics), (1, measuring)] {
            if came && self.lanes[lane].step.is_none() {
                self.lanes[lane].since = now;
            }
        }
    }

    /// Speed and time left at `now`. While bytes arrive [`notice`] computes them; after, this counts down
    /// the remaining processing (speed 0, -1 when nothing is left).
    fn speed_eta_at(&mut self, now: i64) -> (i64, i64) {
        if self.batch.open.is_empty() {
            let fresh = time_left(Some(0.0), &self.after(0, 0, false));
            self.speed_bps = 0;
            self.eta_s = self.countdown.next(fresh, now);
        }
        (self.speed_bps, self.eta_s)
    }

    /// The platform clock extrapolated from its last report.
    fn now(&self) -> Option<i64> {
        self.clock.map(|(at, instant)| at + instant.elapsed().as_millis() as i64)
    }
}

/// `id` is being analysed as it downloads, until [`analysing_ended`].
pub fn analysing_began(id: &str) {
    with(|t| t.analysing_began(id))
}

/// The streaming analysis of `id` ended. If `stored`, the song no longer waits for an analysis;
/// otherwise it waits for one from disk.
pub fn analysing_ended(id: &str, stored: bool) {
    with(|t| t.analysing_ended(id, stored))
}

/// `work` ended for `id` (succeeded or not); true when its phase changed.
pub fn work_done(id: &str, work: Work) -> bool {
    with(|t| t.work_done(id, work))
}

/// `id`'s step of `work` starts; its lane is busy and timed until [`work_done`].
pub fn working(id: &str, work: Work) {
    with(|t| t.working(id, work))
}

/// Whether saved song `id` still waits for `work`.
pub fn waits(id: &str, work: Work) -> bool {
    with(|t| t.waits(id, work))
}

/// Adds [`needs`] to saved song `id`'s processing. `saved`: Some(from_disk) for a fresh download (teaches
/// the from-disk share), None for a song reprocessed later.
pub fn plan(id: &str, needs: Needs, saved: Option<bool>) -> bool {
    with(|t| t.plan(id, needs, saved))
}

impl Tracker {
    fn waits(&self, id: &str, work: Work) -> bool {
        self.marks.get(id).is_some_and(|m| m.0.waits(work))
    }

    fn analysing_began(&mut self, id: &str) {
        self.analysing.insert(id.to_string());
    }

    fn analysing_ended(&mut self, id: &str, stored: bool) {
        self.analysing.remove(id);
        if stored {
            self.work_done(id, Work::Analysis);
        }
    }

    fn working(&mut self, id: &str, work: Work) {
        let now = self.mono();
        let lane = &mut self.lanes[work.lane()];
        lane.step = Some((id.to_string(), work, now));
        lane.since = now;
    }

    fn work_done(&mut self, id: &str, work: Work) -> bool {
        let now = self.mono();
        let lane = &mut self.lanes[work.lane()];
        let timed = match &lane.step {
            Some((s, w, at)) if s == id && *w == work => Some(now - at),
            _ => None,
        };
        if timed.is_some() {
            lane.step = None;
            lane.since = now;
        }
        let Some(&(phase, at)) = self.marks.get(id) else { return false };
        if !phase.waits(work) {
            return false;
        }
        // A lyrics lookup ended without a step took 0 ms; an unneeded analysis or model run is not timed.
        match timed {
            Some(ms) => self.paces[work.index()].took(ms),
            None if work == Work::Lyrics => self.paces[work.index()].took(0),
            None => {}
        }
        self.mark(id, Some(phase.without(work)), at)
    }

    fn plan(&mut self, id: &str, needs: Needs, saved: Option<bool>) -> bool {
        if let Some(from_disk) = saved {
            self.saved += 1;
            self.from_disk += from_disk as i64;
        }
        if !needs.analysis && !needs.beats {
            return false;
        }
        let (phase, at) = match self.marks.get(id) {
            Some(&(p @ (Phase::Processing { .. } | Phase::Done), at)) => (p, at),
            // Downloading or failed: nothing saved yet.
            Some(_) => return false,
            None => (Phase::Done, self.now().unwrap_or(0)),
        };
        let (analysing, lyrics, beats) = match phase {
            Phase::Processing { analysing, lyrics, beats } => (analysing, lyrics, beats),
            _ => (false, false, false),
        };
        self.entered(false, true);
        self.mark(id, Some(Phase::processing(analysing || needs.analysis, lyrics, beats || needs.beats)), at)
    }

    /// See [`download_processing_expire`].
    fn expire(&mut self) -> i64 {
        let now = self.mono();
        let mut next = -1;
        for lane in 0..2 {
            let works: Vec<Work> = Work::ALL.into_iter().filter(|w| w.lane() == lane).collect();
            let waiting: Vec<(String, Work)> = self.marks.iter().flat_map(|(id, m)| works.iter().filter(|w| m.0.waits(**w)).map(|w| (id.clone(), *w))).collect();
            if waiting.is_empty() {
                self.lanes[lane].step = None;
                continue;
            }
            let step = self.lanes[lane].step.clone();
            let limit = step.as_ref().map_or(LANE_IDLE_MS, |s| s.1.limit_ms());
            let deadline = self.lanes[lane].since + limit;
            if now < deadline {
                next = if next < 0 { deadline - now } else { next.min(deadline - now) };
                continue;
            }
            match step.filter(|(id, w, _)| waiting.contains(&(id.clone(), *w))) {
                // The step timed out: give it up for that song and give the lane another spell.
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
                            self.mark(&id, Some(phase.without(w)), at);
                        }
                    }
                    self.lanes[lane].step = None;
                }
            }
            self.lanes[lane].since = now;
            next = if next < 0 { limit } else { next.min(limit) };
        }
        if self.processing().iter().all(|n| *n == 0) {
            -1
        } else {
            next.max(0)
        }
    }
}

/// Gives up steps past their [`Work::limit_ms`] and releases songs in a lane idle for [`LANE_IDLE_MS`].
/// `now` is the platform clock. Returns ms until the next deadline, -1 when nothing is processing.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_processing_expire(now: i64) -> i64 {
    with(|t| {
        t.clock = Some((now, std::time::Instant::now()));
        t.expire()
    })
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

/// The pending processing at the platform's `now`; None when there is none.
pub fn processing(now: i64) -> Option<Processing> {
    with(|t| t.processing_at(now))
}

impl Tracker {
    fn processing_at(&mut self, now: i64) -> Option<Processing> {
        let [lyrics, analysing, beats] = self.processing();
        if lyrics + analysing + beats == 0 {
            return None;
        }
        self.clock = Some((now, std::time::Instant::now()));
        let (_, eta_s) = self.speed_eta_at(now);
        Some(Processing { lyrics, analysing, beats, eta_s })
    }
}

/// Resolves once a mark changed since the last [`download_marks_changed`], so the platform follows
/// processing without polling.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub async fn download_marks_moved() {
    std::future::poll_fn(|cx| {
        with(|t| {
            if t.changed.is_empty() {
                t.wakers.push(cx.waker().clone());
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
    })
    .await
}

/// Replaces the set of downloads the beat model reads once saved (from the downloads table).
pub fn set_beats_wanted(ids: Vec<String>) {
    with(|t| t.beats_wanted = ids.into_iter().collect());
}

/// Adds (`on`) or removes `ids` from the beat model's downloads.
pub fn want_beats(ids: &[String], on: bool) {
    with(|t| {
        for id in ids {
            if on {
                t.beats_wanted.insert(id.clone());
            } else {
                t.beats_wanted.remove(id);
            }
        }
    });
}

/// Whether the beat model reads download `id` once saved.
pub fn wants_beats(id: &str) -> bool {
    with(|t| t.beats_wanted.contains(id))
}

/// `id` left the queue for good. Returns flags as [`followed`].
pub fn removed(id: &str) -> i32 {
    with(|t| {
        let was_open = t.batch.open.contains(id);
        t.batch.removed(id);
        t.close(id);
        t.info.remove(id);
        let mut flags = if t.unmark(id) { MARKS } else { 0 };
        if was_open && t.batch.open.is_empty() {
            flags |= DRAINED;
            t.drained();
        }
        flags
    })
}

/// Forgets `id`'s mark and slot (requeued or cancelled).
pub fn unmark(id: &str) -> i32 {
    with(|t| {
        t.close(id);
        if t.unmark(id) {
            MARKS
        } else {
            0
        }
    })
}

/// Initial progress: 0, or -1 when the size is unknown.
pub fn start_fraction(id: &str) -> f32 {
    with(|t| if t.info(id).is_some_and(|i| i.estimate > 0) { 0.0 } else { -1.0 })
}

/// A download starts transferring; returns the slot for [`note`].
pub fn open(id: &str, now: i64) -> i32 {
    with(|t| t.open(id, now))
}

impl Tracker {
    fn open(&mut self, id: &str, now: i64) -> i32 {
        let t = self;
        // A resumed download replaces its old slot.
        t.close(id);
        // After an idle stretch, measure speed from now.
        if !t.slots.iter().any(|s| s.live) && t.received == t.rate.bytes {
            t.rate.restart(now, t.received);
        }
        let estimate = t.info(id).map_or(0, |i| i.estimate);
        let slot = Slot {
            id: id.to_string(),
            estimate,
            length: 0,
            bytes: 0,
            started_at: now,
            gate_value: f32::NAN,
            gate_at: 0,
            speed_bytes: 0,
            speed_at: now,
            rate: 0.0,
            live: true,
        };
        match t.slots.iter().position(|s| !s.live) {
            Some(i) => {
                t.slots[i] = slot;
                i as i32
            }
            None => {
                t.slots.push(slot);
                t.slots.len() as i32 - 1
            }
        }
    }
}

/// A chunk arrived on `slot`: `bytes` so far of `length` (0 unknown). Returns the progress to show, or
/// NaN when not worth redrawing. Called per chunk: no allocation, and no tracker is created here.
pub fn note(slot: i32, length: i64, bytes: i64, now: i64) -> f32 {
    TRACKER.lock().as_mut().map_or(f32::NAN, |t| t.note(slot, length, bytes, now))
}

impl Tracker {
    fn note(&mut self, slot: i32, length: i64, bytes: i64, now: i64) -> f32 {
        let Some(s) = self.slots.get_mut(slot.max(0) as usize).filter(|s| s.live) else { return f32::NAN };
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
            s.speed_bytes = bytes;
            s.speed_at = now;
        }
        let f = fraction(s.length, bytes, s.estimate);
        if gate(s.gate_value, s.gate_at, f, now) {
            s.gate_value = f;
            s.gate_at = now;
            f
        } else {
            f32::NAN
        }
    }
}

/// Whether progress `f` (negative: unknown) is worth drawing after `last` (NaN: nothing yet) shown at
/// `last_at`: the first value, a switch to or from unknown, the finish, else a whole percent at most every
/// [`GATE_MS`].
fn gate(last: f32, last_at: i64, f: f32, now: i64) -> bool {
    if last.is_nan() {
        true
    } else if f < 0.0 {
        last >= 0.0
    } else if last < 0.0 {
        true
    } else if f >= 1.0 {
        last < 1.0
    } else {
        now - last_at >= GATE_MS && (f - last).abs() >= GATE_STEP
    }
}

impl Batch {
    #[cfg(test)]
    fn position(&self) -> i32 {
        (self.finished() + 1).min(self.total)
    }

    /// Batch progress 0..1: finished songs plus in-flight fractions (capped at the open count).
    fn fraction(&self, in_flight: f64) -> f32 {
        if self.total <= 0 {
            return 0.0;
        }
        (((self.finished() as f64 + in_flight.clamp(0.0, self.open.len() as f64)) / self.total as f64) as f32).clamp(0.0, 1.0)
    }
}

/// The downloads screen's lists: `pending` (newest first) split into active, queued and failed in queue
/// order (oldest first), and this session's finished songs newest first, found in `pending` or `done`
/// so a just-finished song shows before the index catches up.
pub fn sections<'a, T: Clone>(pending: &'a [T], done: &'a [T], marks: &HashMap<String, (Phase, i64)>, id: impl Fn(&T) -> &str) -> [Vec<T>; 4] {
    let (mut active, mut queued, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    for song in pending.iter().rev() {
        match marks.get(id(song)).map(|m| m.0) {
            Some(Phase::Downloading | Phase::Processing { .. }) => active.push(song.clone()),
            Some(Phase::Failed) => failed.push(song.clone()),
            Some(Phase::Done) => {}
            None => queued.push(song.clone()),
        }
    }
    // Saved songs still processing come first.
    let saved = done.iter().filter(|s| matches!(marks.get(id(s)), Some((Phase::Processing { .. }, _))));
    active.splice(0..0, saved.cloned());
    let mut finished: Vec<(i64, &T)> = pending
        .iter()
        .chain(done.iter())
        .filter_map(|song| marks.get(id(song)).filter(|m| m.0 == Phase::Done).map(|m| (m.1, song)))
        .collect();
    finished.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut seen = HashSet::new();
    let finished = finished.into_iter().filter(|(_, song)| seen.insert(id(song).to_string())).map(|(_, song)| song.clone()).collect();
    [active, queued, failed, finished]
}

/// Updates the notification facts for `listed` downloads media3 knows (`waiting`: no network). Returns
/// 0 unchanged, 1 changed (read [`notice_facts`]), 2 batch over. Called every second; allocates only
/// when the title or album changes.
pub fn notice(listed: i32, waiting: bool, now: i64) -> i32 {
    let mut guard = TRACKER.lock();
    guard.get_or_insert_with(Tracker::default).notice(listed, waiting, now)
}

impl Tracker {
    fn notice(&mut self, listed: i32, waiting: bool, now: i64) -> i32 {
        let t = self;
        let total = t.batch.total.max(t.batch.finished() + listed);
        if total == 0 {
            return 2;
        }
        let (mut in_flight, mut live, mut current) = (0f64, false, None::<usize>);
        for (i, s) in t.slots.iter().enumerate().filter(|(_, s)| s.live) {
            in_flight += fraction(s.length, s.bytes, s.estimate).max(0.0) as f64;
            live = true;
            if current.is_none_or(|c| s.started_at < t.slots[c].started_at) {
                current = Some(i);
            }
        }
        // Batch speed from all bytes received (not the sum of per-song rates, which dip between songs).
        // Waiting for a network, or idle, is not sampled.
        if waiting || (!live && t.received == t.rate.bytes) {
            t.rate.restart(now, t.received);
        } else {
            t.rate.sample(now, t.received);
        }
        let rate = t.rate.rate();
        t.speed_bps = if waiting || !live && t.received == t.rate.bytes { 0 } else { rate as i64 };
        let permille = (t.batch.fraction(in_flight) * 1000.0) as i32;
        let position = (t.batch.finished() + 1).min(total.max(1));
        // Remaining bytes: each open song's length (else its scaled estimate) minus its bytes, and the
        // average song size for unsized and unreported songs.
        let ratio = if t.sized_estimate > 0 { (t.sized_actual as f64 / t.sized_estimate as f64).clamp(0.5, 2.0) } else { 1.0 };
        let (mut remaining, mut known_sum, mut known_n, mut sizeless, mut unsized_had) = (0f64, t.batch.done_bytes as f64, t.batch.done_sized, 0i64, 0i64);
        let (mut lyrics_to_come, mut beats_to_come) = (0, 0);
        let mut analysing = false;
        for id in &t.batch.open {
            // Provider songs get no lyrics lookup or beat model.
            if !nori_model::is_provider_id(id) {
                lyrics_to_come += 1;
                beats_to_come += t.beats_wanted.contains(id) as i32;
            }
            analysing |= t.analysing.contains(id);
            let slot = t.slots.iter().find(|s| s.live && s.id == *id);
            let (length, bytes) = slot.map_or((0, 0), |s| (s.length, s.bytes));
            let size = if length > 0 { length as f64 } else { t.info.get(id).map_or(0, |i| i.estimate) as f64 * ratio };
            if size > 0.0 {
                known_sum += size;
                known_n += 1;
                remaining += (size - bytes as f64).max(0.0);
            } else {
                sizeless += 1;
                unsized_had += bytes;
            }
        }
        let avg = if known_n == 0 { UNKNOWN_SONG_BYTES as f64 } else { known_sum / known_n as f64 };
        let unreported = (total - t.batch.finished() - listed).max(0);
        lyrics_to_come += unreported;
        remaining += (sizeless as f64 * avg - unsized_had as f64).max(0.0) + unreported as f64 * avg;
        t.remaining_bytes = remaining as i64;
        let after = t.after(lyrics_to_come, beats_to_come, analysing);
        t.clock = Some((now, std::time::Instant::now()));
        let download_s = if remaining < 1.0 {
            Some(0.0)
        } else if rate > 0.0 && t.rate.settled() {
            Some(remaining / rate)
        } else {
            None
        };
        let fresh = time_left(download_s, &after);
        t.eta_s = if waiting { t.countdown.next(-1.0, now) } else { t.countdown.next(fresh, now) };
        let current_title = current.and_then(|i| t.info.get(&t.slots[i].id)).map(|i| i.title.as_str()).filter(|s| !s.is_empty());
        let kind = if waiting {
            NoticeKind::Waiting
        } else if total == 1 {
            if current_title.is_some() { NoticeKind::OneNamed } else { NoticeKind::One }
        } else {
            NoticeKind::Many
        };
        let label = if total != 1 { t.batch.label() } else { None };
        let n = &t.notice;
        if n.kind == kind
            && n.position == position
            && n.total == total
            && n.permille == permille
            && n.speed_bps == t.speed_bps
            && n.eta_s == t.eta_s
            && n.current == current_title.unwrap_or("")
            && n.label == label.unwrap_or("")
        {
            return 0;
        }
        let (current, label) = (current_title.unwrap_or(""), label.unwrap_or(""));
        let n = &mut t.notice;
        // Reuse the kept strings' buffers.
        if n.current != current {
            n.current.clear();
            n.current.push_str(current);
        }
        if n.label != label {
            n.label.clear();
            n.label.push_str(label);
        }
        (n.kind, n.position, n.total, n.permille, n.speed_bps, n.eta_s) = (kind, position, total, permille, t.speed_bps, t.eta_s);
        1
    }
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

/// The notification facts as [`notice`] last computed them, lent to `f`.
pub fn notice_facts<R>(f: impl FnOnce(&Notice) -> R) -> R {
    with(|t| f(&t.notice))
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

/// How the batch's downloads went; None when nothing finished.
pub fn summary() -> Option<Summary> {
    with(|t| t.summary())
}

impl Tracker {
    fn summary(&self) -> Option<Summary> {
        let t = self;
        let (done, failed) = (t.batch.done, t.batch.failed);
        if done == 0 && failed == 0 {
            return None;
        }
        let album = t.batch.label().filter(|_| done > 1);
        let title = if failed > 0 {
            SummaryTitle::Failed
        } else if album.is_some() {
            SummaryTitle::Album
        } else {
            SummaryTitle::Downloaded
        };
        let text = if failed > 0 && done > 0 {
            SummaryText::SomeFailed
        } else if failed > 0 {
            SummaryText::TryAgain
        } else {
            SummaryText::None
        };
        let label = if title == SummaryTitle::Album { album.unwrap_or("").to_string() } else { String::new() };
        Some(Summary { title, text, done, failed, label })
    }
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

/// media3's `Download.STATE_REMOVING`.
pub const REMOVING: i32 = 5;

/// In-memory mirror of the downloads table (id -> finished), so rows and tracks are answered without
/// the database. Loaded when the core opens; every table write updates it under the database lock.
#[derive(Debug, Default)]
pub struct Held {
    pub ids: HashMap<String, bool>,
    pub done: u32,
}

/// Bumped on every downloads table change of any core, so the platform detects changed counts. Global:
/// one counter across cores, so a new server's counts never match the old one's version.
pub static HELD_VERSION: AtomicU64 = AtomicU64::new(1);

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
        match self {
            HeldState::Absent => 0,
            HeldState::Pending => 1,
            HeldState::Done => 2,
        }
    }
}

impl Held {
    pub fn load(c: &rusqlite::Connection) -> nori_model::Result<Held> {
        let mut st = c.prepare("SELECT id, done FROM downloads WHERE server=sid()")?;
        let ids: HashMap<String, bool> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect();
        let done = ids.values().filter(|d| **d).count() as u32;
        HELD_VERSION.fetch_add(1, Ordering::Relaxed);
        Ok(Held { ids, done })
    }

    pub fn state(&self, id: &str) -> HeldState {
        self.ids.get(id).map_or(HeldState::Absent, |d| if *d { HeldState::Done } else { HeldState::Pending })
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

/// Downloaded and pending counts, with the [`HELD_VERSION`] they were read at.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadCounts {
    pub done: u32,
    pub pending: u32,
    pub version: u64,
}

/// Queues the new `rows` (id, song json) behind everything queued; unfinished ones already queued are
/// returned as `again`, finished ones skipped.
pub fn queue_rows(c: &mut rusqlite::Connection, rows: impl IntoIterator<Item = (String, String)>) -> nori_model::Result<DownloadQueued> {
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
        for (id, json) in rows {
            if !seen.insert(id.clone()) {
                continue;
            }
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
        match by_id.get(id.as_str()).map(|k| (k.state, k)) {
            None | Some((REMOVING, _)) => r.lost.push(id.clone()),
            Some((COMPLETED, _)) => r.finished.push(id.clone()),
            Some((FAILED, k)) => failed.push((id.clone(), k.length, k.bytes)),
            Some(_) => r.unfinished = true,
        }
    }
    (r, failed)
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

/// A download's shown phase; None without a mark.
pub fn download_phase(id: String) -> Option<DownloadPhase> {
    with(|t| t.marks.get(&id).map(|m| m.0.shown()))
}

/// Parallel lists of ids, shown phases and when each began.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadMarks {
    pub ids: Vec<String>,
    pub phases: Vec<Option<DownloadPhase>>,
    pub at: Vec<i64>,
}

/// The marks changed since the last call, with their phase now (None: removed).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_marks_changed() -> DownloadMarks {
    with(Tracker::marks_changed)
}

impl Tracker {
    fn marks_changed(&mut self) -> DownloadMarks {
        let mut m = DownloadMarks { ids: Vec::new(), phases: Vec::new(), at: Vec::new() };
        for id in self.changed.drain() {
            let (p, at) = self.marks.get(&id).map_or((None, 0), |(p, at)| (Some(p.shown()), *at));
            m.ids.push(id);
            m.phases.push(p);
            m.at.push(at);
        }
        m
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

fn row_facts(slots: &[Slot], id: &str) -> Option<RowFacts> {
    let s = slots.iter().find(|s| s.live && s.id == id)?;
    let f = fraction(s.length, s.bytes, s.estimate);
    let total = if s.length > 0 { s.length } else { s.estimate };
    let speed = s.rate as i64;
    let eta = if speed > 0 && total > s.bytes { (total - s.bytes) / speed } else { -1 };
    Some(RowFacts { percent: if f >= 0.0 { (f * 100.0).round() as i32 } else { -1 }, speed_bps: speed, eta_s: eta })
}

/// A downloads screen row: the song's artist (empty when unknown; never blocks on the database) and its
/// facts while running, lent to `f`.
pub fn row<R>(id: &str, f: impl FnOnce(&str, Option<RowFacts>) -> R) -> R {
    let mut guard = TRACKER.lock();
    let t = guard.get_or_insert_with(Tracker::default);
    let facts = row_facts(&t.slots, id);
    let artist = t.info(id).map(|i| i.artist.as_str()).unwrap_or("");
    f(artist, facts)
}

/// The active core's [`Held`], weakly. Global: JNI row queries have no core handle.
static ACTIVE_HELD: Mutex<Weak<Mutex<Held>>> = Mutex::new(Weak::new());

pub fn set_active_held(held: &Arc<Mutex<Held>>) {
    *ACTIVE_HELD.lock() = Arc::downgrade(held);
}

/// `id`'s state in the active core's downloads table, from memory.
pub fn held(id: &str) -> HeldState {
    ACTIVE_HELD.lock().upgrade().map_or(HeldState::Absent, |held| held.lock().state(id))
}

/// [`speed_eta`] as a list, for checks.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_speed_eta() -> Vec<i64> {
    let (speed, eta) = speed_eta();
    vec![speed, eta]
}

/// The batch speed (bytes a second) and seconds left (-1 unknown).
pub fn speed_eta() -> (i64, i64) {
    with(|t| match t.now() {
        Some(now) => t.speed_eta_at(now),
        None => (t.speed_bps, t.eta_s),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_total_stays_fixed() {
        let mut b = Batch::default();
        for i in 0..49 {
            b.queued(&format!("s{i}"), "");
        }
        assert_eq!((b.position(), b.total), (1, 49));
        b.completed("s0");
        b.completed("s1");
        assert_eq!((b.position(), b.total), (3, 49));
        for i in 0..49 {
            b.completed(&format!("s{i}"));
        }
        assert_eq!((b.done, b.position(), b.total), (49, 49, 49));
        assert!(b.open.is_empty());
    }

    #[test]
    fn requeue_counts_once_and_drained_batch_resets() {
        let mut b = Batch::default();
        assert!(b.queued("a", ""), "the first song starts a batch");
        assert!(!b.queued("a", ""));
        b.queued("b", "");
        assert_eq!(b.total, 2);
        let mut b = Batch::default();
        b.queued("a", "");
        b.completed("a");
        assert!(b.open.is_empty() && b.done == 1);
        assert!(b.queued("b", ""));
        assert_eq!((b.total, b.done), (1, 0));
    }

    #[test]
    fn retry_is_same_song() {
        let mut b = Batch::default();
        b.queued("a", "");
        b.queued("b", "");
        b.failed("a");
        assert_eq!(b.failed, 1);
        b.queued("a", "");
        assert_eq!((b.failed, b.total), (0, 2));
        b.completed("a");
        b.completed("b");
        assert_eq!(b.done, 2);
    }

    #[test]
    fn cancelled_songs_leave_count() {
        let mut b = Batch::default();
        for id in ["a", "b", "c"] {
            b.queued(id, "");
        }
        b.failed("b");
        b.removed("c");
        b.removed("b");
        assert_eq!((b.total, b.failed), (1, 0));
        b.removed("x");
        assert_eq!(b.total, 1, "never part of it");
    }

    #[test]
    fn batch_fraction() {
        let mut b = Batch::default();
        for i in 0..4 {
            b.queued(&format!("s{i}"), "");
        }
        assert_eq!(b.fraction(0.0), 0.0);
        b.completed("s0");
        assert!((b.fraction(0.5) - 0.375).abs() < 1e-6);
        assert!((b.fraction(99.0) - 1.0).abs() < 1e-6, "running songs never claim more than the open ones");
    }

    #[test]
    fn batch_label_needs_one_album() {
        let mut b = Batch::default();
        b.queued("a", "Blue");
        b.queued("b", "Blue");
        assert_eq!(b.label(), Some("Blue"));
        b.queued("c", "Red");
        assert_eq!(b.label(), None);
        b.removed("c");
        assert_eq!(b.label(), Some("Blue"));
        b.queued("d", "");
        assert_eq!(b.label(), None, "a song without an album name");
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
        let phase = |t: &Tracker, id: &str| t.marks.get(id).map(|m| m.0.shown());
        t.analysing_began("a");
        t.followed("a", DOWNLOADING, 0);
        assert_eq!(phase(&t, "a"), Some(DownloadPhase::Downloading));
        t.followed("a", COMPLETED, 1_000);
        assert_eq!(t.marks["a"], (Phase::Processing { analysing: true, lyrics: true, beats: false }, 1_000));
        assert!(t.plan("a", Needs { analysis: true, beats: true }, Some(false)));
        assert_eq!(phase(&t, "a"), Some(DownloadPhase::FindingLyrics));
        assert!(t.work_done("a", Work::Lyrics));
        assert_eq!(phase(&t, "a"), Some(DownloadPhase::Analysing));
        // The streaming analysis was not stored: it waits for one from disk.
        t.analysing_ended("a", false);
        assert!(t.waits("a", Work::Analysis));
        assert!(t.work_done("a", Work::Analysis));
        assert_eq!(phase(&t, "a"), Some(DownloadPhase::DetectingBeats));
        assert!(t.work_done("a", Work::Beats));
        assert_eq!(t.marks["a"], (Phase::Done, 1_000), "done, at its saved time");
        assert!(!t.work_done("a", Work::Lyrics), "done stays done");

        t.followed("ext-b", COMPLETED, 0);
        assert_eq!(phase(&t, "ext-b"), Some(DownloadPhase::Done), "provider songs get no lyrics");

        // A stored streaming analysis leaves nothing to analyse.
        t.analysing_began("c");
        t.followed("c", COMPLETED, 0);
        t.analysing_ended("c", true);
        assert!(!t.waits("c", Work::Analysis));
        assert_eq!(phase(&t, "c"), Some(DownloadPhase::FindingLyrics));
        assert_eq!(sections(&[], &["c".to_string()], &t.marks, |s: &String| s.as_str())[0], ["c"], "listed as active");

        // A downloaded song reprocessed later: no lyrics step.
        assert!(t.plan("d", Needs { analysis: true, beats: false }, None));
        assert_eq!(phase(&t, "d"), Some(DownloadPhase::Analysing));
        assert!(!t.plan("e", Needs::default(), None), "nothing to do: no mark");
        assert_eq!(phase(&t, "e"), None);
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

    #[test]
    fn beats_offer_rules() {
        use nori_settings::settings::DownloadBeats;
        for choice in [DownloadBeats::Ask, DownloadBeats::Always, DownloadBeats::Never] {
            assert_eq!(beats_offer(false, choice), BeatsOffer::Off, "the model off: nothing appears");
        }
        assert_eq!(beats_offer(true, DownloadBeats::Ask), BeatsOffer::Ask);
        assert_eq!(beats_offer(true, DownloadBeats::Always), BeatsOffer::Yes);
        assert_eq!(beats_offer(true, DownloadBeats::Never), BeatsOffer::No);
        assert!(BeatsOffer::Ask.wants(true) && !BeatsOffer::Ask.wants(false), "asked: the answer");
        assert!(BeatsOffer::Yes.wants(false), "always: no question asked");
        assert!(!BeatsOffer::No.wants(true) && !BeatsOffer::Off.wants(true));
        assert_eq!(beats_remembered(true), DownloadBeats::Always);
        assert_eq!(beats_remembered(false), DownloadBeats::Never);
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
        assert_eq!(t.expire(), LYRICS_STEP_MS - 20_000, "the lookup's deadline comes first");
        t.test_clock = Some(LYRICS_STEP_MS);
        t.expire();
        assert!(!t.marks["ex-a"].0.waits(Work::Lyrics), "the lookup is given up");
        assert!(t.marks["ex-a"].0.waits(Work::Analysis), "the analysis runs on");
        assert!(t.marks["ex-b"].0.waits(Work::Lyrics), "the next lookup waits its turn");
        // The analysis is slow but alive: it ends well past the lookup's time, within its own.
        t.test_clock = Some(100_000);
        assert!(t.work_done("ex-a", Work::Analysis));
        t.working("ex-a", Work::Beats);
        assert!((t.paces[Work::Analysis.index()].per_song_s(Work::Analysis) - (100.0 + 2.0 * 5.0) / 3.0).abs() < 1e-9, "learned from the step");
        // The lyrics lane moved on to nothing for its whole spell: what waits there is let go.
        t.expire();
        assert!(!t.marks["ex-b"].0.waits(Work::Lyrics));
        assert!(t.marks["ex-b"].0.waits(Work::Analysis), "the measuring lane is busy with ex-a's beats, not stuck");
        // The beat model's run ends; ex-b's analysis follows.
        t.test_clock = Some(160_000);
        assert!(t.work_done("ex-a", Work::Beats));
        assert_eq!(t.marks["ex-a"].0, Phase::Done);
        t.working("ex-b", Work::Analysis);
        t.test_clock = Some(160_000 + ANALYSIS_STEP_MS);
        assert_eq!(t.expire(), -1, "given up at its time: nothing is left processing");
        assert_eq!(t.marks["ex-b"].0, Phase::Done);
    }

    #[test]
    fn marks_changed_reports_only_moved_marks() {
        let mut t = Tracker::default();
        t.mark("a", Some(Phase::Downloading), 1);
        t.mark("b", Some(Phase::Failed), 2);
        let mine = |m: &DownloadMarks, id: &str| m.ids.iter().position(|i| i == id).map(|i| m.phases[i]);
        let m = t.marks_changed();
        assert_eq!((mine(&m, "a"), mine(&m, "b")), (Some(Some(DownloadPhase::Downloading)), Some(Some(DownloadPhase::Failed))));
        t.unmark("a");
        let m = t.marks_changed();
        assert_eq!((mine(&m, "a"), mine(&m, "b")), (Some(None), None), "removed, and b did not move");
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
        assert_eq!(t.marks["ly-b"].0.shown(), DownloadPhase::DetectingBeats, "detecting beats");
        step(&mut t, "ly-b", Work::Beats, 20_000);
        assert_eq!(t.processing(), [0, 0, 0]);
        // Learned: 8 s over two lookups, 4 s over two analyses, 40 s over two model runs, each with its guess.
        let per = |t: &Tracker, w: Work| t.paces[w.index()].per_song_s(w);
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
            t.marks.insert(id.clone(), (Phase::Processing { analysing: false, lyrics: true, beats: false }, 0));
            t.work_done(&id, Work::Lyrics);
        }
        assert!(per(&t, Work::Lyrics) < before / 4.0, "lookups ended at once weigh it down: {}", per(&t, Work::Lyrics));
        // An analysis found done without a step (measured as it came) says nothing of how long one takes.
        let analysis = per(&t, Work::Analysis);
        t.marks.insert("ly-e".into(), (Phase::Processing { analysing: true, lyrics: false, beats: false }, 0));
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
