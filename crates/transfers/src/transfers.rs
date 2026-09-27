//! Downloads as they run: how far each song is, how fast the bytes arrive, the batch the notification
//! counts ("12 of 49"), which of the notification's messages applies and how the batch went. The
//! platform moves the bytes (media3 on Android), reports to this, and words what this says: the facts
//! come out as numbers and kinds. The per-chunk report is a slot number and three numbers - nothing is
//! looked up by name or allocated while bytes flow - and the once-a-second notification is only rebuilt
//! when its facts actually change.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Poll, Waker};

use nori_model::{alog, Song};
use parking_lot::Mutex;

/// Finished songs the downloads screen keeps listing this session.
pub const RECENT: usize = 50;
/// A progress figure is passed on at most this often, and only when it moved a whole percent.
const GATE_MS: i64 = 250;
const GATE_STEP: f32 = 0.01;
/// Against an estimated size, progress is held short of full: an estimate can be low, and a ring sitting
/// at 100 % while bytes still arrive looks stuck.
const ESTIMATE_CEILING: f32 = 0.97;
/// What an unlisted song is guessed to weigh when nothing in the batch says otherwise.
const UNKNOWN_SONG_BYTES: i64 = 8_000_000;
/// The batch's speed is an average over about this long, so a song starting or ending, or one slow
/// second, does not throw the time left about.
const RATE_TAU_MS: f64 = 10_000.0;
/// The shortest stretch the speed is measured over.
const RATE_SAMPLE_MS: i64 = 500;
/// No time left is said before the speed has been measured this long.
const RATE_WARM_MS: f64 = 1_500.0;
/// What one song's lyrics lookup is guessed to take before any was timed, and how many songs that guess
/// weighs against the lookups timed since.
const LYRICS_GUESS_MS: f64 = 3_000.0;
const LYRICS_PRIOR: f64 = 2.0;
/// What a song's analysis for AutoMix may still take once its last bytes are in: it is measured as they come.
const ANALYSIS_TAIL_S: f64 = 2.0;

// media3's `Download.STATE_*`, which the platform reports downloads in.
pub const QUEUED: i32 = 0;
pub const STOPPED: i32 = 1;
pub const DOWNLOADING: i32 = 2;
pub const COMPLETED: i32 = 3;
pub const FAILED: i32 = 4;
pub const RESTARTING: i32 = 7;

/// What [`followed`] and [`removed`] tell the platform to do.
pub const NEW_BATCH: i32 = 1;
pub const DRAINED: i32 = 2;
pub const MARKS: i32 = 4;

/// How long a song may stay [`Phase::Processing`] once its audio is saved: then it is done whatever is left.
pub const PROCESSING_MS: i64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Failed,
    Done,
    /// Saved, and its analysis for AutoMix (still being measured as it came) or its lyrics lookup not over yet.
    Processing { analysing: bool, lyrics: bool },
}

impl Phase {
    /// The number the platform gets (`download_phase`): 1 downloading, 2 failed, 3 done, 4 finding lyrics,
    /// 5 analysing (only once no lyrics are awaited).
    pub fn code(self) -> i32 {
        match self {
            Phase::Downloading => 1,
            Phase::Failed => 2,
            Phase::Done => 3,
            Phase::Processing { lyrics: true, .. } => 4,
            Phase::Processing { .. } => 5,
        }
    }

    /// Done once nothing is left of the processing.
    fn processing(analysing: bool, lyrics: bool) -> Phase {
        if analysing || lyrics { Phase::Processing { analysing, lyrics } } else { Phase::Done }
    }
}

/// What a saved song is still waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Analysis,
    Lyrics,
}

/// What the screens say about a song being downloaded, read once from the downloads table.
#[derive(Debug, Clone, Default)]
pub struct Info {
    title: String,
    album: String,
    artist: String,
    /// What the song should weigh, bytes (0 unknown).
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

/// One run of the queue: everything queued since it was last empty. Its total holds still while songs
/// finish ("12 of 49", never "1 of 47").
#[derive(Debug, Default)]
struct Batch {
    open: HashSet<String>,
    failed_ids: HashSet<String>,
    labels: HashMap<String, String>,
    total: i32,
    done: i32,
    failed: i32,
    /// What the finished songs weighed, and how many were weighed: the songs still to come are guessed from them.
    done_bytes: i64,
    done_sized: i64,
}

impl Batch {
    fn finished(&self) -> i32 {
        self.done + self.failed
    }

    /// A song entered the queue; true when this starts a new batch.
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
        // Tried again: the same song, not one more.
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

    /// Stopped before it finished, or a failure given up on: it no longer counts at all.
    fn removed(&mut self, id: &str) {
        if self.open.remove(id) {
            self.total -= 1;
        } else if self.failed_ids.remove(id) {
            self.failed -= 1;
            self.total -= 1;
        }
        self.labels.remove(id);
    }

    /// The one name every song of the batch shares (an album), when they all share one.
    fn label(&self) -> Option<&str> {
        if (self.labels.len() as i32) < self.total {
            return None;
        }
        let mut names = self.labels.values();
        let first = names.next()?;
        names.all(|n| n == first).then_some(first.as_str())
    }
}

/// What the running batch's notification is made from ([`notice`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Notice {
    pub kind: NoticeKind,
    /// Which song of the batch is in flight, from 1, and how many the batch has.
    pub position: i32,
    pub total: i32,
    /// The bar, in thousandths.
    pub permille: i32,
    /// Bytes a second, and seconds left (-1 unknown).
    pub speed_bps: i64,
    pub eta_s: i64,
    /// The song in flight's title; empty when none is known.
    pub current: String,
    /// The album the batch is, when it has more than one song and all are from it; empty otherwise.
    pub label: String,
}

/// Every download the platform has reported, the batch they make and what the notification says.
#[derive(Debug, Default)]
pub struct Tracker {
    slots: Vec<Slot>,
    batch: Batch,
    pub marks: HashMap<String, (Phase, i64)>,
    /// The songs whose mark changed since the platform last asked (see [`download_marks_changed`]).
    pub changed: HashSet<String>,
    info: HashMap<String, Info>,
    /// Songs being measured for AutoMix as their bytes come.
    analysing: HashSet<String>,
    /// Whoever waits for songs to leave [`Phase::Processing`] ([`processed`]), woken once a mark changed.
    wakers: Vec<Waker>,
    wake: bool,
    download_kbps: i32,
    speed_bps: i64,
    remaining_bytes: i64,
    eta_s: i64,
    notice: Notice,
    /// Every byte the downloads brought in this process, as it came (a resumed download's earlier bytes
    /// not among them): what the batch's speed is measured on.
    received: i64,
    rate: Throughput,
    lyrics: LyricsPace,
    countdown: Countdown,
    /// When [`notice`] was last asked.
    asked_at: Option<i64>,
    /// The platform's clock (`now`, as [`followed`] and [`notice`] get it) at an instant of this process's:
    /// the time left is worked out again when read, with no platform to ask for the time.
    clock: Option<(i64, std::time::Instant)>,
    /// What the finished songs that had an estimate weighed, and what they were estimated at: songs known
    /// only by their estimate are weighed by the same ratio.
    sized_actual: i64,
    sized_estimate: i64,
}

/// The batch's speed: every byte the downloads bring, averaged over [`RATE_TAU_MS`], the stretches the
/// average started from weighed in (so its first seconds are the plain average so far, not a slow start).
#[derive(Debug, Default)]
struct Throughput {
    ema: f64,
    weight: f64,
    at: i64,
    bytes: i64,
}

impl Throughput {
    /// Measures from here: nothing came in since the last stretch, and nothing was meant to.
    fn restart(&mut self, now: i64, bytes: i64) {
        (self.at, self.bytes) = (now, bytes);
    }

    /// `bytes` came in by `now` in all.
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

    /// Measured long enough to say a time left from.
    fn settled(&self) -> bool {
        self.weight >= 1.0 - (-RATE_WARM_MS / RATE_TAU_MS).exp()
    }
}

/// How long one song's lyrics lookup takes here: the time songs spent waiting for lyrics (one lookup runs
/// at a time) over the lookups that ended, a guess weighed in until there are some. A platform that looks
/// none up ends each at once, and the figure falls towards nothing.
#[derive(Debug, Default)]
struct LyricsPace {
    busy_ms: i64,
    done: i64,
    waiting: bool,
}

impl LyricsPace {
    fn per_song_s(&self) -> f64 {
        ((self.busy_ms as f64 + LYRICS_GUESS_MS * LYRICS_PRIOR) / (self.done as f64 + LYRICS_PRIOR) / 1000.0).min(PROCESSING_MS as f64 / 1000.0)
    }
}

/// The time left as said: it counts down a second a second while the figure worked out agrees within a
/// little, drawn slowly towards it, and moves to it at once when they part by more.
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

/// When everything still to do should be over, seconds from now (negative: cannot be told). `download_s`
/// is the bytes still to come at the batch's speed (none when the speed is not known yet); `lyrics_waiting`
/// songs are saved and waiting for their lyrics, `lyrics_to_come` more will be once downloaded, each taking
/// `lyrics_s`, one lookup at a time, none past the [`PROCESSING_MS`] a saved song may wait; `analysing`:
/// some song's analysis may still run past its last byte.
fn time_left(download_s: Option<f64>, lyrics_waiting: i32, lyrics_to_come: i32, lyrics_s: f64, analysing: bool) -> f64 {
    let Some(dl) = download_s else { return -1.0 };
    let mut end = dl;
    if lyrics_waiting + lyrics_to_come > 0 {
        let queue = (lyrics_waiting + lyrics_to_come) as f64 * lyrics_s;
        let lyrics = if lyrics_to_come > 0 { queue.max(dl + lyrics_s) } else { queue };
        end = end.max(lyrics.min(dl + PROCESSING_MS as f64 / 1000.0));
    }
    if analysing {
        end = end.max(dl + ANALYSIS_TAIL_S);
    }
    end
}

static TRACKER: Mutex<Option<Tracker>> = Mutex::new(None);

/// The one download tracker, lent to `f`.
pub fn with<R>(f: impl FnOnce(&mut Tracker) -> R) -> R {
    let mut guard = TRACKER.lock();
    let t = guard.get_or_insert_with(Tracker::default);
    let r = f(t);
    // Woken with the tracker let go: a waiter polled again at once asks it.
    let wakers = if std::mem::take(&mut t.wake) { std::mem::take(&mut t.wakers) } else { Vec::new() };
    drop(guard);
    wakers.into_iter().for_each(Waker::wake);
    r
}

/// What a song should weigh once downloaded: its length at the transcoded bitrate, or the file itself at
/// the original quality. A transcoding server rarely sends a length, so this is what the bar fills against.
pub fn expected_bytes(size_bytes: i64, duration_s: i64, bitrate_kbps: i32) -> i64 {
    if bitrate_kbps > 0 && duration_s > 0 {
        duration_s * bitrate_kbps as i64 * 125
    } else {
        size_bytes
    }
}

/// How far along: against the stated length when there is one, else against the estimate (held short of
/// full), else unknown (negative).
pub fn fraction(length: i64, bytes: i64, estimate: i64) -> f32 {
    if length > 0 {
        (bytes as f64 / length as f64).clamp(0.0, 1.0) as f32
    } else if estimate > 0 {
        ((bytes as f64 / estimate as f64) as f32).clamp(0.0, ESTIMATE_CEILING)
    } else {
        -1.0
    }
}

/// Song `id` as the downloads table keeps it.
fn download_song(c: &rusqlite::Connection, id: &str) -> Option<Song> {
    let json: String = c.query_row("SELECT json FROM downloads WHERE server=sid() AND id=?1", [id], |r| r.get(0)).ok()?;
    serde_json::from_str::<Song>(&json).ok()
}

impl Tracker {
    pub fn info(&mut self, id: &str) -> &Info {
        if !self.info.contains_key(id) {
            let found = nori_db::active().and_then(|db| download_song(&db.lock(), id));
            self.keep_info(id, found);
        }
        &self.info[id]
    }

    /// [`Self::info`] without waiting for the database: none while another thread holds it (a sync
    /// writing a page), and nothing remembered then, so the next ask reads it. For the downloads
    /// screen's line, asked through a door that must not block.
    fn info_now(&mut self, id: &str) -> Option<&Info> {
        if !self.info.contains_key(id) {
            let db = nori_db::active()?;
            let c = db.try_lock()?;
            let found = download_song(&c, id);
            drop(c);
            self.keep_info(id, found);
        }
        self.info.get(id)
    }

    fn keep_info(&mut self, id: &str, found: Option<Song>) {
        {
            let info = found.map_or_else(Info::default, |s| Info {
                estimate: expected_bytes(s.size as i64, s.duration as i64, self.download_kbps),
                title: s.title.replace('\n', " "),
                album: s.album.replace('\n', " "),
                artist: s.artist.replace('\n', " "),
            });
            self.info.insert(id.to_string(), info);
        }
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

    /// Takes `id`'s mark away; true when it had one.
    pub fn unmark(&mut self, id: &str) -> bool {
        let had = self.marks.remove(id).is_some();
        if had {
            self.changed.insert(id.to_string());
            self.wake = true;
        }
        had
    }

    /// Finished marks beyond the latest [`RECENT`] go; the song's own "downloaded" state carries on.
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

    /// The saved songs still waiting for their lyrics, and those waiting only for their analysis.
    fn processing(&self) -> (i32, i32) {
        self.marks.values().fold((0, 0), |(l, a), m| match m.0 {
            Phase::Processing { lyrics: true, .. } => (l + 1, a),
            Phase::Processing { .. } => (l, a + 1),
            _ => (l, a),
        })
    }
}

// ---- what the platform reports as downloads run ------------------------------------------------------------

/// Takes the download quality from the settings (0: the original file), for what songs should weigh;
/// songs weighed at another quality are weighed again. Asked whenever songs are queued, and when an
/// earlier process's queue is picked up.
pub fn follow_quality() {
    let kbps = nori_settings::settings_store::with_prefs(|p| p.download.bit_rate).unwrap_or(0);
    with(|t| {
        if t.download_kbps != kbps {
            t.download_kbps = kbps;
            t.info.clear();
            (t.sized_actual, t.sized_estimate) = (0, 0);
        }
    });
}

/// media3 reported `id` in `state`. Returns [`NEW_BATCH`] (a batch starts: the last one's result goes),
/// [`DRAINED`] (the last song settled: say how it went) and [`MARKS`] (the phases changed).
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
                let label = t.info(&id).album.clone();
                if t.batch.queued(&id, &label) {
                    flags |= NEW_BATCH;
                }
            }
            COMPLETED => {
                // What it weighed, for guessing what the songs still to come weigh.
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
            // A provider's song has no lyrics looked up (`lyrics_for_downloads`).
            COMPLETED => Some(Phase::processing(t.analysing.contains(&id), !id.starts_with("ext-"))),
            FAILED => Some(Phase::Failed),
            _ => None,
        };
        if matches!(state, COMPLETED | FAILED) {
            t.close(&id);
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

    /// The last song of the batch settled: no bytes are coming, and the notice that worked out the speed and
    /// time left is no longer asked, so neither stands at its last figure.
    fn drained(&mut self) {
        (self.speed_bps, self.eta_s) = (0, -1);
        self.countdown = Countdown::default();
    }

    /// The speed and time left at `now`. While bytes come they are [`notice`]'s; once the batch's bytes are
    /// in nobody asks that, so they are worked out here from what the saved songs still wait for: no speed,
    /// and a time left that counts down through the lyrics lookups and ends (-1) when nothing is left.
    fn speed_eta_at(&mut self, now: i64) -> (i64, i64) {
        if self.batch.open.is_empty() {
            let (lyrics, analysing) = self.processing();
            let fresh = time_left(Some(0.0), lyrics, 0, self.lyrics.per_song_s(), analysing > 0);
            self.speed_bps = 0;
            self.eta_s = self.countdown.next(fresh, now);
        }
        (self.speed_bps, self.eta_s)
    }

    /// The platform's clock now, as far as this process can tell from when it was last given it.
    fn now(&self) -> Option<i64> {
        self.clock.map(|(at, instant)| at + instant.elapsed().as_millis() as i64)
    }
}

/// `id` is being measured for AutoMix as it comes (`on`), or that is over, stored or not: whichever,
/// a saved song stops waiting for it.
pub fn analysing(id: &str, on: bool) {
    if on {
        with(|t| t.analysing.insert(id.to_string()));
    } else {
        with(|t| t.analysing.remove(id));
        work_done(id, Work::Analysis);
    }
}

/// `work` is over for `id`, found or failed; true when its phase changed (read the marks again).
pub fn work_done(id: &str, work: Work) -> bool {
    with(|t| t.work_done(id, work))
}

impl Tracker {
    fn work_done(&mut self, id: &str, work: Work) -> bool {
        let Some(&(Phase::Processing { analysing, lyrics }, at)) = self.marks.get(id) else { return false };
        let next = if work == Work::Analysis { Phase::processing(false, lyrics) } else { Phase::processing(analysing, false) };
        if work == Work::Lyrics && lyrics {
            self.lyrics.done += 1;
        }
        self.mark(id, Some(next), at)
    }
}

/// Ends the processing of every song saved [`PROCESSING_MS`] or more before `now`, whatever is left of it.
/// Returns how long until the next one would be ended, -1 when none is processing.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_processing_expire(now: i64) -> i64 {
    with(|t| {
        let due: Vec<(String, i64)> = t.marks.iter().filter(|(_, m)| matches!(m.0, Phase::Processing { .. })).map(|(id, m)| (id.clone(), m.1)).collect();
        let mut next = -1;
        for (id, at) in due {
            if now - at >= PROCESSING_MS {
                t.mark(&id, Some(Phase::Done), at);
            } else if next < 0 || at + PROCESSING_MS - now < next {
                next = at + PROCESSING_MS - now;
            }
        }
        next
    })
}

/// Returns once none of `ids` is processing any more.
pub async fn processed(ids: &[String]) {
    std::future::poll_fn(|cx| {
        with(|t| {
            if ids.iter().any(|id| matches!(t.marks.get(id), Some((Phase::Processing { .. }, _)))) {
                t.wakers.push(cx.waker().clone());
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
    })
    .await
}

/// `id` left the queue for good. Returns flags as [`followed`] does.
pub fn removed(id: &str) -> i32 {
    with(|t| {
        let was_open = t.batch.open.contains(id);
        t.batch.removed(id);
        t.close(id);
        let mut flags = if t.unmark(id) { MARKS } else { 0 };
        if was_open && t.batch.open.is_empty() {
            flags |= DRAINED;
            t.drained();
        }
        flags
    })
}

/// Forgets `id`'s mark and figures (it is being asked for again, or cancelled).
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

/// Where a ring starts before any bytes arrive: 0, or negative when the size cannot be told.
pub fn start_fraction(id: &str) -> f32 {
    with(|t| if t.info(id).estimate > 0 { 0.0 } else { -1.0 })
}

/// A download's bytes start moving: the slot its chunks are reported against.
pub fn open(id: &str, now: i64) -> i32 {
    with(|t| t.open(id, now))
}

impl Tracker {
    fn open(&mut self, id: &str, now: i64) -> i32 {
        let t = self;
        // Taken up again after it stopped (no network, the service let go): the old slot goes, or the song
        // would count twice.
        t.close(id);
        // Nothing was downloading: the speed is measured from here, not over the time nothing was meant to come.
        if !t.slots.iter().any(|s| s.live) && t.received == t.rate.bytes {
            t.rate.restart(now, t.received);
        }
        let estimate = t.info(id).estimate;
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
/// NaN when it has not moved enough to be worth drawing. Called per chunk; allocates nothing.
pub fn note(slot: i32, length: i64, bytes: i64, now: i64) -> f32 {
    TRACKER.lock().as_mut().map_or(f32::NAN, |t| t.note(slot, length, bytes, now))
}

impl Tracker {
    fn note(&mut self, slot: i32, length: i64, bytes: i64, now: i64) -> f32 {
        let Some(s) = self.slots.get_mut(slot.max(0) as usize).filter(|s| s.live) else { return f32::NAN };
        // The first report of a download taken up half way counts what it had as already there: those bytes
        // did not come now, and counting them would read as a burst of speed.
        if s.gate_value.is_nan() && s.bytes == 0 {
            s.speed_bytes = bytes;
        } else if bytes > s.bytes {
            self.received += bytes - s.bytes;
        }
        s.bytes = bytes;
        if length > 0 {
            s.length = length;
        }
        // The rate: each gap of at least 0.4 s folded into a running average, so one slow chunk does not
        // make the figure jump.
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

/// Whether progress `f` (negative: unknown) at `now` is worth drawing after `last` shown at `last_at`
/// (NaN: nothing shown yet): the first figure, a switch to or from unknown, the finish, otherwise a
/// whole percent no sooner than a quarter second after the last. Bytes arrive in chunks of a few
/// kilobytes; drawing each would redraw a list hundreds of times a second for a ring that moves a pixel.
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
    /// "12 of 49": the song being worked on now, counted from one, never past the total.
    #[cfg(test)]
    fn position(&self) -> i32 {
        (self.finished() + 1).min(self.total)
    }

    /// How far the whole batch is, 0..1: finished songs plus the running ones' fractions, which can
    /// never claim more than the songs still open.
    fn fraction(&self, in_flight: f64) -> f32 {
        if self.total <= 0 {
            return 0.0;
        }
        (((self.finished() as f64 + in_flight.clamp(0.0, self.open.len() as f64)) / self.total as f64) as f32).clamp(0.0, 1.0)
    }
}

/// The downloads screen's split of `pending` (newest first, as the index keeps them) into downloading,
/// waiting and failed, in the order the queue runs them (oldest first), and this session's finished
/// songs newest first. `done` and `pending` together are where a finished song's details come from, so
/// one that has just completed is not missing from every list while the index catches up.
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
    // Saved and still being processed: first, as they were the first to arrive.
    let saved = done.iter().filter(|s| matches!(marks.get(id(s)), Some((Phase::Processing { .. }, _))));
    active.splice(0..0, saved.cloned());
    let mut finished: Vec<(i64, &T)> = pending
        .iter()
        .chain(done.iter())
        .filter_map(|song| marks.get(id(song)).filter(|m| m.0 == Phase::Done).map(|m| (m.1, song)))
        .collect();
    finished.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen = HashSet::new();
    let finished = finished.into_iter().filter(|(_, song)| seen.insert(id(song).to_string())).map(|(_, song)| song.clone()).collect();
    [active, queued, failed, finished]
}

/// The notification's facts and bar for `listed` downloads media3 knows of (`waiting`: no network yet).
/// Returns 0 when nothing changed since the last call (keep the last notification), 1 when something did
/// (read [`notice_facts`]; the platform words them), 2 when the batch is over (the "complete"
/// notification). Asked once a second: it allocates nothing unless the song or the album changes.
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
        // The speed: every byte the batch brought, averaged over the last several seconds - not the sum of the
        // songs' own rates, which fell each time a song ended and started again at nothing with the next.
        // Waiting for a network, or nothing running and nothing come, is not a slow stretch.
        if waiting || (!live && t.received == t.rate.bytes) {
            t.rate.restart(now, t.received);
        } else {
            t.rate.sample(now, t.received);
        }
        let rate = t.rate.rate();
        // Nothing running and nothing come: no speed, whatever the last stretch measured.
        t.speed_bps = if waiting || !live && t.received == t.rate.bytes { 0 } else { rate as i64 };
        let permille = (t.batch.fraction(in_flight) * 1000.0) as i32;
        let position = (t.batch.finished() + 1).min(total.max(1));
        // What remains: each open song's size less what it has - its stated length, else its estimate as the
        // finished songs bore estimates out - and the average song for any whose size is not known and any the
        // platform lists but has not reported yet.
        let ratio = if t.sized_estimate > 0 { (t.sized_actual as f64 / t.sized_estimate as f64).clamp(0.5, 2.0) } else { 1.0 };
        let (mut remaining, mut known_sum, mut known_n, mut sizeless, mut unsized_had) = (0f64, t.batch.done_bytes as f64, t.batch.done_sized, 0i64, 0i64);
        let mut lyrics_to_come = 0;
        let mut analysing = false;
        for id in &t.batch.open {
            // A provider's song has no lyrics looked up (`lyrics_for_downloads`).
            if !id.starts_with("ext-") {
                lyrics_to_come += 1;
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
        // The time left: the bytes still to come at that speed, and the lyrics and analysis the songs wait for
        // once saved, which the platform works through after the bytes.
        let (lyrics_waiting, only_analysing) = t.processing();
        if t.lyrics.waiting {
            if let Some(at) = t.asked_at {
                // One lookup runs at a time; while songs waited, one was running. Asked about once a second: a
                // longer gap (the platform stopped asking) is not all lookup.
                t.lyrics.busy_ms += (now - at).clamp(0, 5_000);
            }
        }
        t.lyrics.waiting = lyrics_waiting > 0;
        t.asked_at = Some(now);
        t.clock = Some((now, std::time::Instant::now()));
        let download_s = if remaining < 1.0 {
            Some(0.0)
        } else if rate > 0.0 && t.rate.settled() {
            Some(remaining / rate)
        } else {
            None
        };
        let fresh = time_left(download_s, lyrics_waiting, lyrics_to_come, t.lyrics.per_song_s(), analysing || only_analysing > 0);
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
        // The strings are written into the ones kept, only when they changed.
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

/// What the notification says while a batch runs, as [`notice`] last found it: which title applies and
/// the facts the platform words it from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeKind {
    /// No network yet: "Waiting for a network".
    #[default]
    Waiting,
    /// One song, whose title is known: "Downloading “Title”".
    OneNamed,
    /// One song, not named yet: "Downloading 1 song".
    One,
    /// More: "Downloading: 12 of 49".
    Many,
}

/// The notification's facts as [`notice`] last found them, lent to `f`: the song in flight's title (empty
/// when none) and the batch's album (empty unless the batch has more than one song, all from it), with
/// the numbers. One crossing for all of it.
pub fn notice_facts<R>(f: impl FnOnce(&Notice) -> R) -> R {
    with(|t| f(&t.notice))
}

/// How a finished batch went, for its notification's title: which one applies.
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
    /// The album the batch was, for [`SummaryTitle::Album`]; empty otherwise.
    pub label: String,
    /// Saved songs still waiting for their lyrics, and those waiting only for their analysis for AutoMix:
    /// while either is above nothing the batch is not over yet, whatever its bytes did.
    pub lyrics: i32,
    pub analysing: i32,
}

/// How the batch went, once its bytes are in; none when there is nothing to say. The caller keeps the
/// notification when something failed, and asks again as the marks change while songs are still processing
/// ([`Summary::lyrics`], [`Summary::analysing`]).
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
        let (lyrics, analysing) = t.processing();
        Some(Summary { title, text, done, failed, label, lyrics, analysing })
    }
}

// ---- uniffi: which songs are queued, and picking up an earlier process's queue -----------------------------

/// What queuing songs did. `fresh` went into the queue now, in the order asked; `again` were in it
/// already but unfinished - failed, or lost to a process that died before the platform heard of them -
/// and are asked for again, so the download button always does something. Finished songs are left as
/// they are.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadQueued {
    pub fresh: Vec<String>,
    pub again: Vec<String>,
}

/// One download as the platform's own queue remembers it: media3's `Download.STATE_*`, the length (-1
/// unknown) and the bytes it has.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadKnown {
    pub id: String,
    pub state: i32,
    pub length: i64,
    pub bytes: i64,
}

/// A download an earlier process left failed, and how far it got.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadFailed {
    pub id: String,
    pub progress: f32,
}

/// What an earlier process left unfinished, sorted out (see [`Core::download_recover`]).
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadRecovery {
    /// Never reached the platform's queue (the add was still in flight): to be asked for again.
    pub lost: Vec<String>,
    /// Failed: marked failed here, so each reads as failed rather than waiting.
    pub failed: Vec<DownloadFailed>,
    /// Finished there but not recorded here (the process went between the two): recorded now. Each
    /// song's streamed copy is the same bytes twice and can go.
    pub finished: Vec<String>,
    /// Queued or interrupted mid-download: the platform's queue has to be started to resume them.
    pub unfinished: bool,
}

/// media3's `Download.STATE_REMOVING`: a download being taken back.
pub const REMOVING: i32 = 5;

/// Which songs the downloads table holds and whether each has finished, kept beside the table so one
/// song can be asked about - by every row a list draws, every track opened - without the database, and
/// the table counted without reading it. Read from the table once, when the core opens; every write to
/// the table updates it while the database is still locked, so the two never disagree.
#[derive(Debug, Default)]
pub struct Held {
    pub ids: HashMap<String, bool>,
    pub done: u32,
}

/// Moves on whenever any core's downloads table changes, so a platform's copy of the counts can tell a
/// change from the same answer asked twice. One counter for every core: a new server's numbers never
/// read as the old one's.
pub static HELD_VERSION: AtomicU64 = AtomicU64::new(1);

impl Held {
    pub fn load(c: &rusqlite::Connection) -> nori_model::Result<Held> {
        let mut st = c.prepare("SELECT id, done FROM downloads WHERE server=sid()")?;
        let ids: HashMap<String, bool> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect();
        let done = ids.values().filter(|d| **d).count() as u32;
        HELD_VERSION.fetch_add(1, Ordering::Relaxed);
        Ok(Held { ids, done })
    }

    /// 0 not in the table, 1 queued or failed, 2 finished.
    pub fn state(&self, id: &str) -> i32 {
        self.ids.get(id).map_or(0, |d| if *d { 2 } else { 1 })
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

/// How many songs are downloaded and how many are still to come, and which version of the table that is.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadCounts {
    pub done: u32,
    pub pending: u32,
    pub version: u64,
}

/// Adds the `rows` (id, song json) that are not in the queue yet, behind everything queued before, and
/// sorts the rest into asked again (unfinished) and left out (finished).
pub fn queue_rows(c: &mut rusqlite::Connection, rows: impl IntoIterator<Item = (String, String)>) -> nori_model::Result<DownloadQueued> {
    use rusqlite::OptionalExtension;
    let tx = c.transaction()?;
    let mut out = DownloadQueued::default();
    {
        // The queue is listed by this stamp. Counting on from the newest keeps a new batch behind the
        // last one, and its own songs in the order asked, whatever the clock does.
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

/// What the platform's queue says about the songs still `pending` here; the failed ones come back
/// separately with their length and bytes.
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

// ---- uniffi: what the downloads screen shows ----------------------------------------------------------------

/// The downloads screen's lists, in the order the queue will run them.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadSections {
    pub active: Vec<Song>,
    pub queued: Vec<Song>,
    pub failed: Vec<Song>,
    /// This session's finished songs, newest first.
    pub finished: Vec<Song>,
}

/// A download's phase for the screen: 0 waiting (or nothing), else [`Phase::code`].
pub fn download_phase(id: String) -> i32 {
    with(|t| t.marks.get(&id).map_or(0, |m| m.0.code()))
}

/// The ids with a phase, and each one's phase (as [`download_phase`]) and when it began.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DownloadMarks {
    pub ids: Vec<String>,
    pub phases: Vec<i32>,
    pub at: Vec<i64>,
}

/// The marks that changed since the last call, each with its phase now (0: it has none any more). The
/// platform keeps its own copy of the marks and only hears what moved.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_marks_changed() -> DownloadMarks {
    with(|t| {
        let mut m = DownloadMarks { ids: Vec::new(), phases: Vec::new(), at: Vec::new() };
        for id in t.changed.drain() {
            let (p, at) = t.marks.get(&id).map_or((0, 0), |(p, at)| (p.code(), *at));
            m.ids.push(id);
            m.phases.push(p);
            m.at.push(at);
        }
        m
    })
}

/// Where a running song stands, for its row on the downloads screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowFacts {
    /// Whole percent, -1 when the size is not known.
    pub percent: i32,
    /// Bytes a second (0 unknown) and seconds left (-1 unknown).
    pub speed_bps: i64,
    pub eta_s: i64,
}

/// Where `id` stands if it is running; none when it is not.
fn row_facts(slots: &[Slot], id: &str) -> Option<RowFacts> {
    let s = slots.iter().find(|s| s.live && s.id == id)?;
    let f = fraction(s.length, s.bytes, s.estimate);
    let total = if s.length > 0 { s.length } else { s.estimate };
    let speed = s.rate as i64;
    let eta = if speed > 0 && total > s.bytes { (total - s.bytes) / speed } else { -1 };
    Some(RowFacts { percent: if f >= 0.0 { (f * 100.0).round() as i32 } else { -1 }, speed_bps: speed, eta_s: eta })
}

// ---- the downloads screen's facts -----------------------------------------------------------------------------

/// A song's row on the downloads screen: its artist (the downloads table's, read once per song; empty when
/// unknown) and, while it runs, where it stands. Asked whenever its ring moves; the artist is lent to `f`.
pub fn row<R>(id: &str, f: impl FnOnce(&str, Option<RowFacts>) -> R) -> R {
    let mut guard = TRACKER.lock();
    let t = guard.get_or_insert_with(Tracker::default);
    let facts = row_facts(&t.slots, id);
    let artist = t.info_now(id).map(|i| i.artist.as_str()).unwrap_or("");
    f(artist, facts)
}
// ---- whether a song is downloaded -----------------------------------------------------------------------------

/// The downloads table's ids of the core the app is using now; the core holds them, this only keeps them
/// while it does.
static ACTIVE_HELD: Mutex<Weak<Mutex<Held>>> = Mutex::new(Weak::new());

/// `held` are the downloads of the core the app uses from now on: the newest one made.
pub fn set_active_held(held: &Arc<Mutex<Held>>) {
    *ACTIVE_HELD.lock() = Arc::downgrade(held);
}

/// Whether `id` is in the active core's downloads table: 0 no, 1 queued or failed, 2 finished. Asked by
/// every row a list draws and every track opened; answered from memory.
pub fn held(id: &str) -> i32 {
    ACTIVE_HELD.lock().upgrade().map_or(0, |held| held.lock().state(id))
}

/// The download statistics for checks: bytes a second over the last several seconds, and seconds left (-1 unknown).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_speed_eta() -> Vec<i64> {
    let (speed, eta) = speed_eta();
    vec![speed, eta]
}

/// The batch's bytes a second over the last several seconds, and seconds left (-1 unknown): the downloads
/// screen's summary line asks once a second while it is open.
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
    fn the_total_holds_still_while_songs_finish() {
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
    fn a_song_queued_twice_counts_once_and_a_drained_batch_starts_over() {
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
    fn a_retry_inside_the_batch_is_the_same_song() {
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
    fn cancelled_songs_leave_the_count() {
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
    fn progress_counts_finished_songs_and_the_running_ones() {
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
    fn a_batch_is_named_after_an_album_only_when_every_song_is_from_it() {
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
    fn the_gate_lets_through_at_most_four_updates_a_second_of_whole_percents() {
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
    fn the_gate_always_shows_the_finish_and_the_switch_to_unknown() {
        assert!(gate(0.995, 0, 1.0, 10), "the finish is not held back by the interval");
        assert!(!gate(1.0, 10, 1.0, 1_000), "and only once");
        assert!(gate(f32::NAN, 0, -1.0, 0));
        assert!(!gate(-1.0, 0, -1.0, 5_000), "still unknown: nothing to redraw");
        assert!(gate(-1.0, 5_000, 0.2, 5_001), "the size arriving shows at once");
        assert!(gate(0.2, 5_001, -1.0, 5_002));
    }

    #[test]
    fn progress_uses_the_stated_length_then_the_estimate() {
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
    fn a_song_just_finished_is_listed_before_the_index_catches_up() {
        let pending = ["b", "a"].map(String::from);
        let [_, queued, _, finished] = sections(&pending, &[], &marks(&[("a", Phase::Done, 0)]), |s: &String| s.as_str());
        assert_eq!((queued, finished), (vec!["b".to_string()], vec!["a".to_string()]));
    }

    #[test]
    fn a_saved_song_is_processing_until_its_analysis_and_lyrics_are_over_or_its_time_is_up() {
        use std::future::Future;
        let phase = |id: &str| download_phase(id.into());
        analysing("pr-a", true);
        followed("pr-a", DOWNLOADING, 0);
        assert_eq!(phase("pr-a"), 1);
        followed("pr-a", COMPLETED, 1_000);
        assert_eq!(with(|t| t.marks["pr-a"]), (Phase::Processing { analysing: true, lyrics: true }, 1_000));
        let ids = vec!["pr-a".to_string()];
        let mut waiting = std::pin::pin!(processed(&ids));
        let mut cx = std::task::Context::from_waker(Waker::noop());
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
        assert_eq!(phase("pr-a"), 4, "finding lyrics");
        // The lookup failed: no lyrics, and still being analysed.
        assert!(work_done("pr-a", Work::Lyrics));
        assert_eq!(phase("pr-a"), 5, "analysing");
        analysing("pr-a", false);
        assert_eq!(with(|t| t.marks["pr-a"]), (Phase::Done, 1_000), "done, where it was saved");
        assert!(waiting.as_mut().poll(&mut cx).is_ready());
        assert!(!work_done("pr-a", Work::Lyrics), "done stays done");

        followed("ext-pr-b", COMPLETED, 0);
        assert_eq!(phase("ext-pr-b"), 3, "a provider's song not measured has nothing to wait for");

        // A lookup that never answers: the song is done once its time is up all the same.
        followed("pr-c", COMPLETED, 1_000_000);
        assert_eq!(download_processing_expire(1_010_000), PROCESSING_MS - 10_000);
        assert_eq!(phase("pr-c"), 4);
        let saved = ["pr-c".to_string()];
        assert_eq!(sections(&[], &saved, &with(|t| t.marks.clone()), |s: &String| s.as_str())[0], ["pr-c"], "listed among the active ones");
        assert_eq!(download_processing_expire(1_000_000 + PROCESSING_MS), -1);
        assert_eq!(phase("pr-c"), 3);
    }

    #[test]
    fn only_the_marks_that_moved_come_over() {
        with(|t| {
            t.mark("mk-a", Some(Phase::Downloading), 1);
            t.mark("mk-b", Some(Phase::Failed), 2);
        });
        let m = download_marks_changed();
        let mine = |m: &DownloadMarks, id: &str| m.ids.iter().position(|i| i == id).map(|i| m.phases[i]);
        assert_eq!((mine(&m, "mk-a"), mine(&m, "mk-b")), (Some(1), Some(2)));
        with(|t| {
            t.unmark("mk-a");
        });
        let m = download_marks_changed();
        assert_eq!((mine(&m, "mk-a"), mine(&m, "mk-b")), (Some(0), None), "gone, and the other one did not move");
        with(|t| {
            t.unmark("mk-b");
        });
    }

    #[test]
    fn a_rows_facts_are_given_only_while_it_runs() {
        let slot = Slot { id: "r".into(), estimate: 0, length: 1000, bytes: 450, started_at: 0, gate_value: 0.0, gate_at: 0, speed_bytes: 0, speed_at: 0, rate: 0.0, live: true };
        assert_eq!(row_facts(std::slice::from_ref(&slot), "r"), Some(RowFacts { percent: 45, speed_bps: 0, eta_s: -1 }));
        assert_eq!(row_facts(std::slice::from_ref(&slot), "other"), None);
    }

    /// A tracker of the test's own, knowing the songs `ids` weigh `size` each (0 unknown).
    fn tracker(ids: &[&str], size: i64) -> Tracker {
        let mut t = Tracker::default();
        for id in ids {
            t.info.insert(id.to_string(), Info { estimate: size, ..Info::default() });
        }
        t
    }

    /// What tools/feature-e2e.sh used to read off a phone mid-batch: two songs running side by side give
    /// the batch a speed (both songs' bytes) and a time left (what is still to come at that speed).
    #[test]
    fn two_songs_running_give_the_batch_a_speed_and_a_time_left() {
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

    /// Six songs of 2 MB, two at a time, 200 kB/s each: the speed holds at 400 kB/s and the time left
    /// counts down a second a second while songs end and the next ones start at nothing.
    #[test]
    fn the_speed_and_time_left_hold_while_songs_end_and_start() {
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

    /// Songs not started yet weigh what the finished ones did: their estimates as far as those bore them
    /// out, the average finished song when there is no estimate at all.
    #[test]
    fn the_songs_still_to_come_are_weighed_by_the_ones_finished() {
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

    /// A download taken up half way reports the bytes it had first: they did not just come.
    #[test]
    fn a_download_taken_up_again_is_no_burst_of_speed() {
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
    fn the_time_left_takes_in_the_lyrics_and_analysis_after_the_bytes() {
        assert_eq!(time_left(None, 1, 1, 3.0, true), -1.0, "no speed yet");
        assert_eq!(time_left(Some(10.0), 0, 0, 3.0, false), 10.0);
        // The last song's lookup comes after its last byte.
        assert_eq!(time_left(Some(10.0), 0, 2, 3.0, false), 13.0);
        // Many lookups queued up take longer than the bytes.
        assert_eq!(time_left(Some(10.0), 4, 2, 3.0, false), 18.0);
        assert_eq!(time_left(Some(0.0), 3, 0, 3.0, false), 9.0, "the bytes are in; the lookups are left");
        assert_eq!(time_left(Some(0.0), 50, 0, 3.0, false), PROCESSING_MS as f64 / 1000.0, "none waits past its time");
        assert_eq!(time_left(Some(5.0), 0, 0, 3.0, true), 5.0 + ANALYSIS_TAIL_S);
    }

    /// Once the bytes are in, the summary and the time left wait for the lyrics, and the lookups are timed:
    /// a platform that looks none up (the desktop ends each at once) soon adds next to nothing.
    #[test]
    fn saved_songs_finding_lyrics_are_counted_and_timed() {
        let mut t = tracker(&["ly-a", "ly-b"], 1_000_000);
        for id in ["ly-a", "ly-b"] {
            t.followed(id, DOWNLOADING, 0);
            let slot = t.open(id, 0);
            t.note(slot, 1_000_000, 0, 0);
            t.note(slot, 1_000_000, 1_000_000, 2_000);
        }
        t.notice(2, false, 2_000);
        for id in ["ly-a", "ly-b"] {
            t.followed(id, COMPLETED, 2_000);
        }
        let s = t.summary().unwrap();
        assert_eq!((s.title, s.done, s.lyrics, s.analysing), (SummaryTitle::Downloaded, 2, 2, 0), "saved, not done");
        // Both wait; the lookups take 4 s each.
        t.notice(0, false, 3_000);
        assert_eq!(t.eta_s, 6, "two lookups at the guess");
        t.notice(0, false, 7_000);
        t.work_done("ly-a", Work::Lyrics);
        t.notice(0, false, 11_000);
        t.work_done("ly-b", Work::Lyrics);
        t.notice(0, false, 11_000);
        assert_eq!(t.summary().unwrap().lyrics, 0);
        // 8 s over two lookups, weighed with the guess.
        assert!((t.lyrics.per_song_s() - (8.0 + 2.0 * 3.0) / 4.0).abs() < 1e-9, "{}", t.lyrics.per_song_s());
        let before = t.lyrics.per_song_s();
        for i in 0..20 {
            let id = format!("ly-d{i}");
            t.marks.insert(id.clone(), (Phase::Processing { analysing: false, lyrics: true }, 0));
            t.work_done(&id, Work::Lyrics);
        }
        assert!(t.lyrics.per_song_s() < before / 5.0, "lookups ended at once weigh it down: {}", t.lyrics.per_song_s());
    }

    /// Once the bytes are in, the platform stops asking for the notice; the downloads screen and the checks
    /// still read the speed and time left. They say no speed, count down through the lyrics still to find,
    /// and end once every saved song is processed.
    #[test]
    fn after_the_last_byte_the_time_left_counts_down_the_lyrics_and_then_ends() {
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
    fn the_time_left_counts_down_through_a_jittery_figure_and_follows_a_real_change() {
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

    #[test]
    fn a_rows_artist_and_the_notification_come_from_the_tracker() {
        with(|t| {
            t.info.insert("rw-1".into(), Info { artist: "Nils".into(), ..Info::default() });
        });
        assert_eq!(row("rw-1", |a, f| (a.to_string(), f)), ("Nils".into(), None), "known, and not running: the artist alone");
        assert_eq!(row("rw-unknown", |a, f| (a.to_string(), f)), (String::new(), None), "no core to read it from: nothing, and nothing kept");
        with(|t| {
            t.notice.kind = NoticeKind::Many;
            t.notice.label = "Album".into();
        });
        assert_eq!(notice_facts(|n| (n.kind, n.label.clone())), (NoticeKind::Many, "Album".into()));
    }
}
