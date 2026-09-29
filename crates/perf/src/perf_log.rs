//! What the perf build measured, stretch by stretch: one row per stretch of the app's life (screen off
//! and playing, the player open, charging, ...), kept in the app's database for a couple of weeks so
//! a battery test of several days can be read back and shared. The platform reads its own counters
//! (CPU time, context switches, the battery, the frames it drew); which state a stretch is filed under,
//! what two readings make, how the stretches add up and how the page and the shared report say it are
//! here, so a desktop client's recorder files, sums and reports the same way.
//!
//! Each stretch also keeps why it cost what it did: the threads that woke most, with their names and CPU
//! time, the audio output as it stood at the end (the track's format, the buffer asked for and given,
//! the performance mode asked for and applied, offload, the route, underruns), and the bytes the app
//! moved over the network. Read at the stretch's two ends only, as everything else is.
//!
//! And what happened during it, its timeline: songs with their format and where they came from, settings
//! changed, the output opened and offload entered or left, underruns as they grew, errors. The platform
//! tells each as it happens ([`perf_note`]); the words and the bookkeeping are here. The report ends with
//! the app's own log and the last crash, which the platform reads from logcat only when it is shared.
//!
//! Only the perf build calls these. The table is made by the first row, so the database of every
//! other build never has it.

use std::collections::HashMap;

use nori_settings::settings::PrefValue;
use nori_settings::settings_store;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::memory::{memory_line, PerfMemory};

/// How long a stretch is kept after it ended.
pub const KEEP_MS: i64 = 14 * 24 * 3600 * 1000;

/// Stretches shorter than this are the blinks between two states (the screen going off stops the
/// activity too): they are not kept.
pub const SHORTEST_MS: i64 = 3_000;

/// Every state a stretch is filed under, in the order the page lists them, with its name.
const STATES: [(&str, &str); 7] = [
    ("off-playing", "Screen off, playing"),
    ("off-paused", "Screen off, paused"),
    ("on-playing-player", "Screen on, playing, player open"),
    ("on-playing-app", "Screen on, playing, other page"),
    ("on-playing-away", "Screen on, playing, another app"),
    ("on-paused", "Screen on, paused"),
    (CHARGING, "Charging"),
];

/// Charging runs the battery the other way, so it is left out of every battery figure.
const CHARGING: &str = "charging";

/// One stretch as it is kept: the state it was in, how long, and what it cost. `uah` is the charge used
/// by the battery's own counter, none where the phone does not keep one; `pct` is the drop in the battery
/// level, which every phone reports. Kept as JSON under the short names the rows have always had.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfStretch {
    #[serde(rename = "s")]
    pub state: String,
    #[serde(rename = "t0")]
    pub start_wall: i64,
    pub ms: i64,
    #[serde(rename = "cpu")]
    pub cpu_ms: i64,
    #[serde(rename = "wk")]
    pub wakeups: i64,
    #[serde(rename = "al")]
    pub alloc_bytes: i64,
    #[serde(rename = "gc")]
    pub gcs: i64,
    #[serde(rename = "pss")]
    pub pss_kb: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uah: Option<i64>,
    pub pct: i32,
    #[serde(rename = "gma", default, skip_serializing_if = "Option::is_none")]
    pub gauge_ma: Option<f64>,
    #[serde(rename = "tmin")]
    pub temp_min: i32,
    #[serde(rename = "tmax")]
    pub temp_max: i32,
    #[serde(rename = "fr")]
    pub frames: i64,
    #[serde(rename = "jk")]
    pub janky: i64,
    #[serde(rename = "worst")]
    pub worst_ms: f64,
    pub cfg: String,
    /// The threads that woke most over the stretch, the most first ([`TOP_THREADS`] of them); none in
    /// rows from before they were kept.
    #[serde(rename = "th", default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<PerfThreadUse>,
    /// The audio output as the stretch ended; none with no player, or in rows from before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<PerfOutput>,
    /// Bytes the app received and sent over the network during the stretch, where the platform counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<i64>,
    /// What happened during the stretch, oldest first ([`MOST_EVENTS`] at most); none in rows from before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ev: Vec<PerfEvent>,
    /// Events that happened past [`MOST_EVENTS`], the oldest, and were not kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub evx: i64,
    /// How long an offloaded output was open during the stretch, ms: the fact behind "offload wanted".
    /// None in rows from before it was counted.
    #[serde(rename = "om", default, skip_serializing_if = "Option::is_none")]
    pub offloaded_ms: Option<i64>,
    /// Where the memory was as the stretch ended; none in rows from before it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<PerfMemory>,
    /// How long the player held its CPU wake lock during the stretch, ms (the one it lets go while the
    /// songs are offloaded and nothing but the platform's word is due). None in rows from before.
    #[serde(rename = "wl", default, skip_serializing_if = "Option::is_none")]
    pub wake_lock_ms: Option<i64>,
    /// The player engine's thread's wakeups over the stretch (`nori-engine`), none without one alive at
    /// both ends or in rows from before.
    #[serde(rename = "ew", default, skip_serializing_if = "Option::is_none")]
    pub engine_wakeups: Option<i64>,
    /// Times an offloaded track asked for more (`onDataRequest`) during the stretch: the platform's own
    /// pace, which the engine's wakes follow. None in rows from before.
    #[serde(rename = "dr", default, skip_serializing_if = "Option::is_none")]
    pub data_requests: Option<i64>,
}

/// The name of the player engine's thread, whose wakeups a stretch keeps apart.
pub const ENGINE_THREAD: &str = "nori-engine";

/// Every time an offloaded track asked for more since the process started: the platform tells it
/// ([`count_data_request`]), and a stretch takes the difference.
static DATA_REQUESTS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// An offloaded track asked for more (`onDataRequest`): counted for the perf report, one atomic add.
pub fn count_data_request() {
    DATA_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// How many of the threads that woke most a stretch keeps.
pub const TOP_THREADS: usize = 6;

/// One thread of the app at one reading: its name as the system has it, and what it has done since it
/// started.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfThread {
    pub tid: i32,
    pub name: String,
    /// CPU time, user and system.
    pub cpu_ms: i64,
    /// Voluntary context switches: each is the thread going to sleep and being woken again.
    pub switches: i64,
}

/// What one thread did over a stretch. `born` is a thread that started during it, whose whole count
/// is the stretch's; one that ended during it is not seen at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfThreadUse {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "cpu")]
    pub cpu_ms: i64,
    #[serde(rename = "wk")]
    pub wakeups: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub born: bool,
}

/// The audio output as the platform describes it, read when a stretch ends. What was asked of the track
/// against what the platform made of it: a buffer smaller than asked, or a power-saving mode not
/// applied, is what makes a writer wake more than the design says. -1 for a figure the platform does
/// not give.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfOutput {
    /// The player that opened the track, "rust" or "exoplayer".
    #[serde(rename = "e")]
    pub engine: String,
    pub rate: i32,
    #[serde(rename = "ch")]
    pub channels: i32,
    /// `AudioFormat.ENCODING_*`.
    #[serde(rename = "enc")]
    pub encoding: i32,
    /// The buffer asked for, bytes.
    #[serde(rename = "ask")]
    pub asked_bytes: i64,
    /// The buffer the track uses (`getBufferSizeInFrames`) and the most it could (`getBufferCapacityInFrames`).
    #[serde(rename = "size")]
    pub size_frames: i64,
    #[serde(rename = "cap")]
    pub capacity_frames: i64,
    /// `AudioTrack.PERFORMANCE_MODE_*`: asked for, and what the track got.
    #[serde(rename = "pma")]
    pub mode_asked: i32,
    #[serde(rename = "pm")]
    pub mode: i32,
    /// Played by the audio chip rather than mixed on the CPU (`isOffloadedPlayback`).
    #[serde(rename = "off")]
    pub offloaded: bool,
    /// Where the track is routed: `AudioDeviceInfo.TYPE_*` (0 unknown) and the device's own name.
    #[serde(rename = "dt")]
    pub device_type: i32,
    #[serde(rename = "dn", default, skip_serializing_if = "String::is_empty")]
    pub device_name: String,
    /// `getUnderrunCount`: times the track ran dry since it was made.
    #[serde(rename = "ur")]
    pub underruns: i32,
    /// `getPlayState`: 1 stopped, 2 paused, 3 playing.
    #[serde(rename = "st")]
    pub play_state: i32,
    /// Why the player plays on the CPU (PCM) rather than handing the song to the output's decoder, as it
    /// says itself: the setting that keeps offload off, or what came of offering the song (its
    /// compression, the platform's answer). Offloaded, what the chip leaves in (a song's encoder delay and
    /// padding, on an output without gapless offload). Empty when the player does not say.
    #[serde(rename = "wp", default, skip_serializing_if = "String::is_empty")]
    pub pcm_why: String,
}

/// One reading of every counter the platform keeps.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfCounters {
    /// A clock that counts deep sleep (Android's elapsedRealtime).
    pub elapsed_ms: i64,
    pub wall_ms: i64,
    /// The process's CPU time, user and system.
    pub cpu_ms: i64,
    /// Every thread of the app: its name, CPU time and voluntary context switches.
    pub threads: Vec<PerfThread>,
    pub alloc_bytes: i64,
    pub gcs: i64,
    pub pss_kb: i64,
    /// What is left in the battery, where the phone counts it (µAh).
    pub charge_uah: Option<i64>,
    pub capacity_pct: i32,
    /// The fuel gauge's current, its own average where it keeps one (µA, either sign by maker).
    pub gauge_ua: Option<i64>,
    /// Battery temperature in tenths of a degree.
    pub temp_deci: i32,
    /// Bytes the app has received and sent over the network since boot, where the platform counts them.
    pub rx_bytes: Option<i64>,
    pub tx_bytes: Option<i64>,
    /// Where the memory is, where the platform reads it.
    pub memory: Option<PerfMemory>,
}

/// What the frames drawn over a stretch came to, counted by the platform as they were drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfFrames {
    pub frames: i64,
    pub janky: i64,
    pub worst_ns: i64,
}

/// One row of the Performance page: what it is, and its figures on the line under it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfFigures {
    pub title: String,
    pub detail: String,
    /// A stretch's timeline, one timestamped line per event, for the page to fold away; empty elsewhere.
    pub events: Vec<String>,
}

/// The Performance page's figures.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfPage {
    /// Each state's stretches added up ("By state"); empty when nothing is recorded yet.
    pub totals: Vec<PerfFigures>,
    /// Every frame counted; none before the app has been on screen.
    pub frames: Option<PerfFigures>,
    /// The stretch under way.
    pub live: Option<PerfFigures>,
    /// The stretches kept, newest first.
    pub stretches: Vec<PerfFigures>,
}

/// The phone and the build, for the report's head.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfDevice {
    pub manufacturer: String,
    pub model: String,
    pub device: String,
    pub release: String,
    pub sdk: i32,
    pub version: String,
    pub sha: String,
    pub build_type: String,
}

// ---- keeping them ----

fn table(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS perf_stretches(ended_ms INTEGER NOT NULL, row TEXT NOT NULL)")
}

fn add(c: &Connection, ended_ms: i64, row: &str) -> rusqlite::Result<()> {
    table(c)?;
    c.execute("INSERT INTO perf_stretches(ended_ms, row) VALUES(?1, ?2)", params![ended_ms, row])?;
    c.execute("DELETE FROM perf_stretches WHERE ended_ms < ?1", [ended_ms - KEEP_MS])?;
    Ok(())
}

fn rows(c: &Connection, since_ms: i64) -> rusqlite::Result<Vec<String>> {
    table(c)?;
    let mut st = c.prepare("SELECT row FROM perf_stretches WHERE ended_ms >= ?1 ORDER BY ended_ms, rowid")?;
    let out = st.query_map([since_ms], |r| r.get(0))?.collect();
    out
}

/// A stretch that ended at `ended_ms` (wall clock). Stretches older than [`KEEP_MS`] go at the same
/// time. Written on the calling thread; nothing before the settings are open.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_log_add(ended_ms: i64, stretch: PerfStretch) {
    let Some(db) = settings_store::app_db() else { return };
    let Ok(row) = serde_json::to_string(&stretch) else { return };
    let written = add(&db.lock(), ended_ms, &row);
    if let Err(e) = written {
        nori_model::alog::info(&format!("perf log: could not write: {e}"));
    }
}

/// The stretches that ended at or after `since_ms`, oldest first; a row that does not read is passed over.
pub fn perf_log_rows(since_ms: i64) -> Vec<PerfStretch> {
    let Some(db) = settings_store::app_db() else { return Vec::new() };
    let read = rows(&db.lock(), since_ms);
    parsed(read.unwrap_or_default())
}

fn parsed(rows: Vec<String>) -> Vec<PerfStretch> {
    rows.iter().filter_map(|r| serde_json::from_str(r).ok()).collect()
}

/// Every stretch forgotten: "Start fresh".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_log_clear() {
    let Some(db) = settings_store::app_db() else { return };
    let c = db.lock();
    if table(&c).is_ok() {
        let _ = c.execute("DELETE FROM perf_stretches", []);
    }
    // A crash from before the series would read as one of it, and the stretch under way starts again.
    if crash_table(&c).is_ok() {
        let _ = c.execute("DELETE FROM perf_crashes", []);
    }
    if selftest_table(&c).is_ok() {
        let _ = c.execute("DELETE FROM perf_selftest", []);
    }
    if break_log_table(&c).is_ok() {
        let _ = c.execute("DELETE FROM perf_break_logs", []);
    }
    timeline().forget();
}

// ---- measuring them ----

/// The state a stretch is filed under: charging first, then the screen, then the music, then where
/// the app is.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_state(charging: bool, screen_on: bool, playing: bool, foreground: bool, player_open: bool) -> String {
    let key = if charging {
        CHARGING
    } else if !screen_on {
        if playing { "off-playing" } else { "off-paused" }
    } else if !playing {
        "on-paused"
    } else if !foreground {
        "on-playing-away"
    } else if player_open {
        "on-playing-player"
    } else {
        "on-playing-app"
    };
    key.into()
}

/// The settings that change what playing costs, in one line, the playback path first: `engine` is the
/// player the running service built ("rust"; with no service yet, the one it will start). A change of
/// this line ends a stretch as a change of state does.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_config(engine: Option<String>) -> String {
    let p = settings_store::settings_current().unwrap_or_default();
    config(engine, &p)
}

fn config(engine: Option<String>, p: &nori_settings::settings::StoredPrefs) -> String {
    let on = |b: bool| if b { "on" } else { "off" };
    let engine = engine.unwrap_or_else(|| "rust".into());
    format!(
        "engine {engine}, eq {}, automix {}, crossfade {} s, offload {}, hi-res {}, bit-perfect {}",
        on(p.eq_enabled),
        on(p.auto_mix),
        p.crossfade_sec,
        on(p.offload),
        on(p.hi_res),
        on(p.bit_perfect)
    )
}

/// The difference between two readings, filed under `state` with the settings line `cfg` it began
/// with and the frames drawn meanwhile; `offloaded` is whether the music went to the audio chip when it
/// ended, `output` the audio output as it stood then. None for a blink shorter than [`SHORTEST_MS`],
/// unless it is the `live` one, which is shown however short.
#[cfg_attr(feature = "ffi", uniffi::export)]
#[allow(clippy::too_many_arguments)]
pub fn perf_stretch(
    state: String,
    cfg: String,
    a: PerfCounters,
    b: PerfCounters,
    drawn: PerfFrames,
    offloaded: bool,
    output: Option<PerfOutput>,
    live: bool,
) -> Option<PerfStretch> {
    let ms = b.elapsed_ms - a.elapsed_ms;
    if ms < if live { 0 } else { SHORTEST_MS } {
        if !live {
            // The next stretch starts where this blink ends; its events wait for it.
            let mut t = timeline();
            t.close(a.wall_ms, b.wall_ms, false);
            t.kept(a.wall_ms, b.wall_ms, true);
            t.why_at_start = Some(offload_reason());
        }
        return None;
    }
    let before: HashMap<i32, &PerfThread> = a.threads.iter().map(|t| (t.tid, t)).collect();
    // Only threads alive at both ends: one that ended in between would take its whole count with it.
    let wakeups = b.threads.iter().filter_map(|t| before.get(&t.tid).map(|m| t.switches - m.switches)).sum();
    let engine: Vec<i64> = b.threads.iter().filter(|t| t.name == ENGINE_THREAD).filter_map(|t| before.get(&t.tid).filter(|m| m.name == t.name).map(|m| t.switches - m.switches)).collect();
    let engine_wakeups = (!engine.is_empty()).then(|| engine.iter().sum());
    let gauge: Vec<i64> = [a.gauge_ua, b.gauge_ua].into_iter().flatten().collect();
    let gauge_ma = (!gauge.is_empty()).then(|| (gauge.iter().map(|g| g.abs()).sum::<i64>() / gauge.len() as i64) as f64 / 1000.0);
    // What happened meanwhile, and how long the output was really offloaded. A blink's events go on to
    // the stretch after it; the one under way only looks.
    let mut t = timeline();
    let settings_why = offload_reason();
    let (wake_lock_ms, data_requests) = t.kept(a.wall_ms, b.wall_ms, !live);
    let (ev, evx, off_ms, why) = if live {
        let (ev, evx, off) = t.so_far(a.wall_ms, b.wall_ms);
        (ev, evx, off, t.why_at_start.unwrap_or(settings_why))
    } else {
        let (ev, evx, off) = t.close(a.wall_ms, b.wall_ms, true);
        (ev, evx, off, t.why_at_start.replace(settings_why).unwrap_or(settings_why))
    };
    drop(t);
    Some(PerfStretch {
        state,
        start_wall: a.wall_ms,
        ms,
        cpu_ms: b.cpu_ms - a.cpu_ms,
        wakeups,
        alloc_bytes: b.alloc_bytes - a.alloc_bytes,
        gcs: b.gcs - a.gcs,
        pss_kb: b.pss_kb,
        uah: a.charge_uah.zip(b.charge_uah).map(|(x, y)| x - y),
        pct: a.capacity_pct - b.capacity_pct,
        gauge_ma,
        temp_min: a.temp_deci.min(b.temp_deci),
        temp_max: a.temp_deci.max(b.temp_deci),
        frames: drawn.frames,
        janky: drawn.janky,
        worst_ms: drawn.worst_ns as f64 / 1e6,
        cfg: format!("{cfg}, {}", offload_tag(offloaded, why)),
        threads: busiest(&before, &b.threads),
        out: output,
        rx: a.rx_bytes.zip(b.rx_bytes).map(|(x, y)| y - x),
        tx: a.tx_bytes.zip(b.tx_bytes).map(|(x, y)| y - x),
        ev,
        evx,
        offloaded_ms: Some(off_ms.clamp(0, ms)),
        mem: b.memory,
        wake_lock_ms: Some(wake_lock_ms.clamp(0, ms)),
        engine_wakeups,
        data_requests: Some(data_requests),
    })
}

/// The threads that woke most between two readings, the most first, then by CPU time. A thread only in
/// the second reading started in between, and all it did is the stretch's; a thread id taken again by
/// another thread (the name says so) counts as that other thread's start.
fn busiest(before: &HashMap<i32, &PerfThread>, after: &[PerfThread]) -> Vec<PerfThreadUse> {
    let mut used: Vec<PerfThreadUse> = after
        .iter()
        .map(|t| match before.get(&t.tid).filter(|a| a.name == t.name) {
            Some(a) => PerfThreadUse { name: t.name.clone(), cpu_ms: t.cpu_ms - a.cpu_ms, wakeups: t.switches - a.switches, born: false },
            None => PerfThreadUse { name: t.name.clone(), cpu_ms: t.cpu_ms, wakeups: t.switches, born: true },
        })
        .filter(|u| u.wakeups > 0 || u.cpu_ms > 0)
        .collect();
    used.sort_by(|x, y| y.wakeups.cmp(&x.wakeups).then(y.cpu_ms.cmp(&x.cpu_ms)));
    used.truncate(TOP_THREADS);
    used
}

// ---- adding them up ----

/// Every stretch of one state added up. Battery figures leave charging out: it runs the other way.
#[derive(Debug, Default)]
struct Totals {
    state: String,
    count: i64,
    ms: i64,
    cpu_ms: i64,
    wakeups: i64,
    alloc_bytes: i64,
    gcs: i64,
    /// The last stretch's: memory is a level, not something spent.
    pss_kb: i64,
    /// Charge used, over the stretches the counter measured, and how long those were.
    uah: i64,
    uah_ms: i64,
    pct: i64,
    frames: i64,
    janky: i64,
    /// Time an offloaded output was open, over the stretches that had an output and counted it, and
    /// how long those were.
    off_ms: i64,
    off_of_ms: i64,
    /// The wake lock held, the engine's wakeups and the platform's asks for more, over the stretches that
    /// counted them and an offloaded output was open in, and how long those were.
    lock_ms: i64,
    engine_wakeups: i64,
    requests: i64,
    counted_ms: i64,
}

impl Totals {
    fn add(&mut self, s: &PerfStretch) {
        self.count += 1;
        self.ms += s.ms;
        self.cpu_ms += s.cpu_ms;
        self.wakeups += s.wakeups;
        self.alloc_bytes += s.alloc_bytes;
        self.gcs += s.gcs;
        self.pss_kb = s.pss_kb;
        self.pct += s.pct as i64;
        self.frames += s.frames;
        self.janky += s.janky;
        if let Some(u) = s.uah {
            self.uah += u;
            self.uah_ms += s.ms;
        }
        if let Some(off) = s.offloaded_ms.filter(|_| s.out.is_some()) {
            self.off_ms += off;
            self.off_of_ms += s.ms;
            if let (Some(lock), Some(engine), true) = (s.wake_lock_ms, s.engine_wakeups, off > 0) {
                self.lock_ms += lock;
                self.engine_wakeups += engine;
                self.requests += s.data_requests.unwrap_or(0);
                self.counted_ms += s.ms;
            }
        }
    }

    /// "offloaded 45 min of 1 h 00 min (75 %)": how much of the time the audio chip really played, where
    /// it was counted.
    fn offloaded(&self) -> Option<String> {
        (self.off_of_ms > 0).then(|| {
            let mut out = offloaded_words(self.off_ms, self.off_of_ms);
            if self.counted_ms > 0 {
                out.push_str(&format!("; {}", awake_words(self.lock_ms, self.engine_wakeups, self.requests, self.counted_ms)));
            }
            out
        })
    }

    fn battery(&self) -> bool {
        self.state != CHARGING
    }

    fn mah(&self) -> f64 {
        self.uah as f64 / 1000.0
    }

    fn mah_per_h(&self) -> Option<f64> {
        (self.uah_ms > 0).then(|| self.mah() / (self.uah_ms as f64 / 3_600_000.0))
    }

    fn pct_per_h(&self) -> f64 {
        if self.ms > 0 { self.pct as f64 / (self.ms as f64 / 3_600_000.0) } else { 0.0 }
    }

    fn jank_pct(&self) -> Option<f64> {
        (self.frames > 0).then(|| self.janky as f64 * 100.0 / self.frames as f64)
    }

    /// One state's figures on one line, as the page and the report show them.
    fn line(&self) -> String {
        let mut out = duration(self.ms);
        out.push_str(&format!(
            ", CPU {} %, {} wakeups/s, {} KB/min allocated, {} GCs, PSS {} MB",
            fixed(cpu_pct(self.cpu_ms, self.ms), 2),
            fixed(per_s(self.wakeups, self.ms), 1),
            fixed(kb_per_min(self.alloc_bytes, self.ms), 0),
            self.gcs,
            self.pss_kb / 1024
        ));
        if self.battery() {
            match self.mah_per_h() {
                Some(h) => out.push_str(&format!(", {} mAh ({} mAh/h)", fixed(self.mah(), 1), fixed(h, 1))),
                None => out.push_str(&format!(", {} % ({} %/h)", self.pct, fixed(self.pct_per_h(), 2))),
            }
        }
        if let Some(j) = self.jank_pct() {
            out.push_str(&format!(", {} frames, {} % janky", self.frames, fixed(j, 1)));
        }
        if let Some(off) = self.offloaded() {
            out.push_str(&format!(", {off}"));
        }
        out
    }
}

/// The stretches added up by state, in [`STATES`]' order, then any state not listed there in the order
/// it came; states never seen left out.
fn totals(stretches: &[PerfStretch]) -> Vec<Totals> {
    let mut by: Vec<Totals> = STATES.iter().map(|(k, _)| Totals { state: k.to_string(), ..Totals::default() }).collect();
    for s in stretches {
        match by.iter_mut().find(|t| t.state == s.state) {
            Some(t) => t.add(s),
            None => {
                let mut t = Totals { state: s.state.clone(), ..Totals::default() };
                t.add(s);
                by.push(t);
            }
        }
    }
    by.retain(|t| t.count > 0);
    by
}

fn state_name(key: &str) -> &str {
    STATES.iter().find(|(k, _)| *k == key).map_or(key, |(_, name)| name)
}

// ---- saying them ----

/// Java's `"%.{places}f"` as `Locale.ROOT` writes it: the report reads the same on every phone. Halves
/// round up on the number's shortest decimal form (0.15 is "0.2"), as Java's do and Rust's `{:.1}` does not.
fn fixed(v: f64, places: i32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let places = places.max(0) as usize;
    let shortest = format!("{}", v.abs());
    let a = match shortest.split_once('.') {
        Some((whole, frac)) if frac.len() > places => {
            let kept: f64 = format!("{whole}.{}", &frac[..places]).parse().unwrap_or(v.abs());
            if frac.as_bytes()[places] >= b'5' { kept + 10f64.powi(-(places as i32)) } else { kept }
        }
        _ => v.abs(),
    };
    format!("{}{:.*}", if v.is_sign_negative() { "-" } else { "" }, places, a)
}

fn cpu_pct(cpu_ms: i64, ms: i64) -> f64 {
    if ms > 0 { cpu_ms as f64 * 100.0 / ms as f64 } else { 0.0 }
}

fn per_s(n: i64, ms: i64) -> f64 {
    if ms > 0 { n as f64 * 1000.0 / ms as f64 } else { 0.0 }
}

fn kb_per_min(bytes: i64, ms: i64) -> f64 {
    if ms > 0 { bytes as f64 / 1024.0 / (ms as f64 / 60_000.0) } else { 0.0 }
}

/// "1 h 05 min", "3 min 07 s", "42 s".
fn duration(ms: i64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{} h {:02} min", s / 3600, s / 60 % 60)
    } else if s >= 60 {
        format!("{} min {:02} s", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

/// "09-24 21:05": when a stretch began, by the phone's own clock.
fn when(wall_ms: i64) -> String {
    let local = wall_ms + nori_library::library::local_offset_s(wall_ms.div_euclid(1000)) * 1000;
    let (_, m, d, secs) = nori_library::smart::civil_from_ms(local);
    format!("{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs / 60 % 60)
}

/// "12.3 MB", "840 KB": bytes moved over the network.
fn bytes(n: i64) -> String {
    if n.abs() >= 1024 * 1024 {
        format!("{} MB", fixed(n as f64 / 1024.0 / 1024.0, 1))
    } else {
        format!("{} KB", n / 1024)
    }
}

/// The threads that woke most, on one line: "nori-track 2.1/s 40 ms, ...". A thread that started
/// during the stretch is marked so.
fn threads_line(s: &PerfStretch) -> Option<String> {
    if s.threads.is_empty() {
        return None;
    }
    let each: Vec<String> = s
        .threads
        .iter()
        .map(|t| format!("{} {}/s {} ms{}", t.name, fixed(per_s(t.wakeups, s.ms), 1), t.cpu_ms, if t.born { " (new)" } else { "" }))
        .collect();
    Some(format!("threads by wakeups: {}", each.join(", ")))
}

/// `AudioFormat.ENCODING_*` as a person says it, and the bytes one sample of it takes (none when it is
/// not PCM).
fn encoding(e: i32) -> (String, Option<i64>) {
    match e {
        2 => ("16-bit".into(), Some(2)),
        3 => ("8-bit".into(), Some(1)),
        4 => ("float".into(), Some(4)),
        21 => ("24-bit".into(), Some(3)),
        22 => ("32-bit".into(), Some(4)),
        5 => ("AC-3".into(), None),
        6 => ("E-AC-3".into(), None),
        9 => ("MP3".into(), None),
        10 => ("AAC".into(), None),
        20 => ("Opus".into(), None),
        _ => (format!("encoding {e}"), None),
    }
}

/// `AudioTrack.PERFORMANCE_MODE_*` as a person says it.
fn mode(m: i32) -> &'static str {
    match m {
        0 => "none",
        1 => "low latency",
        2 => "power saving",
        _ => "unknown",
    }
}

/// The audio output on one line: what was asked of the track against what the platform made of it.
fn output_line(o: &PerfOutput) -> String {
    let (enc, width) = encoding(o.encoding);
    let channels = match o.channels {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        n => format!("{n} channels"),
    };
    let mut out = format!("output: {}, {} Hz {channels} {enc}", o.engine, o.rate);
    let frame_ms = |f: i64| if o.rate > 0 { f * 1000 / o.rate as i64 } else { 0 };
    let asked = width.filter(|_| o.asked_bytes > 0 && o.channels > 0).map(|w| frame_ms(o.asked_bytes / (w * o.channels as i64)));
    if o.size_frames >= 0 {
        out.push_str(&format!(", buffer {} ms", frame_ms(o.size_frames)));
        match asked {
            Some(a) => out.push_str(&format!(" of {a} ms asked")),
            None if o.asked_bytes > 0 => out.push_str(&format!(" ({} KB asked)", o.asked_bytes / 1024)),
            None => {}
        }
        if o.capacity_frames > o.size_frames {
            out.push_str(&format!(", up to {} ms", frame_ms(o.capacity_frames)));
        }
    }
    out.push_str(&format!(", mode {} asked, {} given", mode(o.mode_asked), mode(o.mode)));
    // Whether the audio chip took the stream, as the track says of itself: the answer that matters for
    // a stretch's battery.
    out.push_str(if o.offloaded { ", offload given" } else { ", PCM" });
    // Offloaded, the player may still say what the chip leaves in (an encoder's delay and padding).
    if !o.pcm_why.is_empty() {
        out.push_str(&format!(" ({})", o.pcm_why));
    }
    if o.device_type != 0 || !o.device_name.is_empty() {
        out.push_str(&format!(", to {}", nori_player::outputs::key(nori_devices::outputs::kind(o.device_type), &o.device_name)));
    }
    out.push_str(&format!(", {} underruns", o.underruns));
    out.push_str(match o.play_state {
        1 => ", stopped",
        2 => ", paused",
        3 => ", playing",
        _ => "",
    });
    out
}

/// The lines that say why a stretch cost what it did, under its figures: the threads, the output.
fn why_lines(s: &PerfStretch) -> Vec<String> {
    threads_line(s).into_iter().chain(s.out.as_ref().map(output_line)).chain(s.mem.as_ref().map(memory_line)).collect()
}

/// One stretch's figures on one line.
fn stretch_line(s: &PerfStretch) -> String {
    let mut out = format!("{}  {}", when(s.start_wall), duration(s.ms));
    out.push_str(&format!(
        ", CPU {} %, {} wakeups/s, {} KB/min, {} GCs, PSS {} MB",
        fixed(cpu_pct(s.cpu_ms, s.ms), 2),
        fixed(per_s(s.wakeups, s.ms), 1),
        fixed(kb_per_min(s.alloc_bytes, s.ms), 0),
        s.gcs,
        s.pss_kb / 1024
    ));
    if s.state != CHARGING {
        match s.uah {
            Some(u) => {
                let per_h = if s.ms > 0 { u as f64 / 1000.0 / (s.ms as f64 / 3_600_000.0) } else { 0.0 };
                out.push_str(&format!(", {} mAh ({} mAh/h)", fixed(u as f64 / 1000.0, 1), fixed(per_h, 1)));
            }
            None => out.push_str(&format!(", {} %", s.pct)),
        }
    }
    if let (Some(rx), Some(tx)) = (s.rx, s.tx) {
        out.push_str(&format!(", network {} in, {} out", bytes(rx), bytes(tx)));
    }
    if let Some(g) = s.gauge_ma {
        out.push_str(&format!(", gauge {} mA", fixed(g, 0)));
    }
    out.push_str(&format!(", {}-{} °C", fixed(s.temp_min as f64 / 10.0, 1), fixed(s.temp_max as f64 / 10.0, 1)));
    if s.frames > 0 {
        out.push_str(&format!(", {} frames, {} janky, worst {} ms", s.frames, s.janky, fixed(s.worst_ms, 0)));
    }
    if let Some(off) = s.offloaded_ms.filter(|_| s.out.is_some()) {
        out.push_str(&format!(", {}", offloaded_words(off, s.ms)));
        if let (Some(lock), Some(engine), true) = (s.wake_lock_ms, s.engine_wakeups, off > 0) {
            out.push_str(&format!(" ({})", awake_words(lock, engine, s.data_requests.unwrap_or(0), s.ms)));
        }
    }
    out.push_str(&format!(" [{}]", cfg_words(&s.cfg)));
    out
}

/// The kept stretches and the one under way, oldest first.
fn all(kept: Vec<PerfStretch>, live: Option<PerfStretch>) -> Vec<PerfStretch> {
    let mut all = kept;
    all.extend(live);
    all
}

/// The Performance page for the stretches kept and `live`, the one under way.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_page(live: Option<PerfStretch>) -> PerfPage {
    page(perf_log_rows(0), live)
}

/// A stretch's figures and, on the lines under them, why it cost that.
fn stretch_detail(s: &PerfStretch) -> String {
    std::iter::once(stretch_line(s)).chain(why_lines(s)).collect::<Vec<_>>().join("\n")
}

fn page(kept: Vec<PerfStretch>, live: Option<PerfStretch>) -> PerfPage {
    let now = live.as_ref().map(|s| PerfFigures { title: format!("Now: {}", state_name(&s.state)), detail: stretch_detail(s), events: event_lines(s) });
    let stretches = kept
        .iter()
        .rev()
        .take(40)
        .map(|s| PerfFigures { title: state_name(&s.state).into(), detail: stretch_detail(s), events: event_lines(s) })
        .collect();
    let all = all(kept, live);
    let totals = totals(&all)
        .iter()
        .map(|t| {
            let name = state_name(&t.state);
            let title = if t.count > 1 { format!("{name} ({})", t.count) } else { name.to_string() };
            PerfFigures { title, detail: t.line(), events: Vec::new() }
        })
        .collect();
    let drawn: i64 = all.iter().map(|s| s.frames).sum();
    let janky: i64 = all.iter().map(|s| s.janky).sum();
    let frames = (drawn > 0).then(|| PerfFigures {
        title: format!("{drawn} frames, {janky} janky ({} %)", fixed(janky as f64 * 100.0 / drawn as f64, 1)),
        detail: format!("The slowest took {} ms", fixed(all.iter().map(|s| s.worst_ms).fold(f64::MIN, f64::max), 0)),
        events: Vec::new(),
    });
    PerfPage { totals, frames, live: now, stretches }
}

/// The report the Share button sends: plain text, so it reads the same in a chat, a mail or an issue.
/// `calls` and `covers` are the benchmarks' results, empty when they were not run; `logs` what logcat
/// had when it was shared, which goes at the end with any crash kept.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_report(live: Option<PerfStretch>, device: PerfDevice, calls: String, covers: String, logs: PerfLogs) -> String {
    let test = perf_selftest_kept();
    let mut out = report(all(perf_log_rows(0), live), &device, &calls, &covers, test.as_deref());
    out.push('\n');
    out.push_str(&break_log_section(&break_logs_kept()));
    out.push_str(&log_section(&logs, &perf_crashes_kept()));
    out
}

fn report(all: Vec<PerfStretch>, d: &PerfDevice, calls: &str, covers: &str, selftest: Option<&str>) -> String {
    let mut out = String::from("nori perf report\n");
    out.push_str(&format!("Device: {} {} ({}), Android {} (API {})\n", d.manufacturer, d.model, d.device, d.release, d.sdk));
    out.push_str(&format!("Build: {} ({}, {})\n", d.version, d.sha, d.build_type));
    if let (Some(first), Some(last)) = (all.first(), all.last()) {
        out.push_str(&format!("Recorded: {} to {}, {} stretches\n", when(first.start_wall), when(last.start_wall + last.ms), all.len()));
    }
    let counter = if all.iter().any(|s| s.uah.is_some()) { "yes (mAh)" } else { "no (battery % only)" };
    out.push_str(&format!("Battery counter: {counter}\n\n"));
    out.push_str(&invariant_section(&all));
    if let Some(test) = selftest {
        out.push('\n');
        out.push_str(test.trim_end());
        out.push('\n');
    }
    out.push_str("\nBy state\n");
    let by_state = totals(&all);
    out.push_str(&columns(&by_state));
    let offloaded: Vec<String> = by_state.iter().filter_map(|t| t.offloaded().map(|o| format!("{}: {o}", state_name(&t.state)))).collect();
    if !offloaded.is_empty() {
        out.push_str("\nReally offloaded (the output as it was opened, not the settings)\n");
        for line in offloaded {
            out.push_str(&format!("{line}\n"));
        }
    }
    out.push_str("\nStretches, newest first\n");
    for s in all.iter().rev().take(60) {
        out.push_str(&format!("{}: {}\n", state_name(&s.state), stretch_line(s)));
        for line in why_lines(s) {
            out.push_str(&format!("    {line}\n"));
        }
        for line in event_lines(s) {
            out.push_str(&format!("      {line}\n"));
        }
    }
    if !calls.is_empty() {
        out.push_str(&format!("\nCall benchmark: {calls}\n"));
    }
    if !covers.is_empty() {
        out.push_str(&format!("\nCover benchmark: {covers}\n"));
    }
    out
}

/// The states side by side in padded columns, for a monospaced reader; "-" where a figure does not apply.
fn columns(totals: &[Totals]) -> String {
    let head = ["state", "time", "CPU %", "wakeups/s", "KB/min", "GCs", "PSS MB", "mAh", "mAh/h", "%/h", "frames", "janky %"];
    let dash = || "-".to_string();
    let mut rows: Vec<Vec<String>> = vec![head.iter().map(|h| h.to_string()).collect()];
    for t in totals {
        rows.push(vec![
            state_name(&t.state).to_string(),
            duration(t.ms),
            fixed(cpu_pct(t.cpu_ms, t.ms), 2),
            fixed(per_s(t.wakeups, t.ms), 1),
            fixed(kb_per_min(t.alloc_bytes, t.ms), 0),
            t.gcs.to_string(),
            (t.pss_kb / 1024).to_string(),
            if t.battery() && t.uah_ms > 0 { fixed(t.mah(), 1) } else { dash() },
            t.mah_per_h().filter(|_| t.battery()).map_or_else(dash, |h| fixed(h, 1)),
            if t.battery() { fixed(t.pct_per_h(), 2) } else { dash() },
            if t.frames > 0 { t.frames.to_string() } else { dash() },
            t.jank_pct().map_or_else(dash, |j| fixed(j, 1)),
        ]);
    }
    let widths: Vec<usize> = (0..head.len()).map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for r in &rows {
        let cells: Vec<String> =
            r.iter().enumerate().map(|(i, c)| if i == 0 { format!("{c:<w$}", w = widths[i]) } else { format!("{c:>w$}", w = widths[i]) }).collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// The report's first section: every invariant break the stretches recorded, newest first, or that none did.
fn invariant_section(all: &[PerfStretch]) -> String {
    let breaks: Vec<&PerfEvent> = all.iter().flat_map(|s| s.ev.iter()).filter(|e| e.kind == "invariant").collect();
    if breaks.is_empty() {
        return "Invariant breaks: none recorded\n".into();
    }
    let mut out = format!("Invariant breaks: {} (newest first)\n", breaks.len());
    for e in breaks.iter().rev().take(40) {
        out.push_str(&format!("  {} {} {}\n", when(e.wall_ms).split(' ').next().unwrap_or_default(), clock(e.wall_ms), e.detail));
    }
    if breaks.len() > 40 {
        out.push_str(&format!("  ({} older ones in the stretches below)\n", breaks.len() - 40));
    }
    out
}

fn selftest_table(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS perf_selftest(id INTEGER PRIMARY KEY CHECK (id = 1), at_ms INTEGER NOT NULL, text TEXT NOT NULL)")
}

/// The self test's result, kept in place of the last one: the report carries it near the top.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_selftest_keep(at_ms: i64, text: String) {
    let Some(db) = settings_store::app_db() else { return };
    let c = db.lock();
    let kept = selftest_table(&c).and_then(|_| c.execute("INSERT OR REPLACE INTO perf_selftest(id, at_ms, text) VALUES(1, ?1, ?2)", params![at_ms, text]));
    if let Err(e) = kept {
        nori_model::alog::info(&format!("perf log: could not keep the self test: {e}"));
    }
}

/// The last self test's result, as kept; none before the first.
pub fn perf_selftest_kept() -> Option<String> {
    let db = settings_store::app_db()?;
    let c = db.lock();
    selftest_table(&c).ok()?;
    c.query_row("SELECT text FROM perf_selftest WHERE id = 1", [], |r| r.get(0)).ok()
}

// ---- what happened in them ----

/// The most events a stretch keeps; past it the oldest go, and the stretch says how many.
pub const MOST_EVENTS: usize = 150;

/// Formats read ahead of their song (the decoder takes the next song's first bytes before the ear gets
/// there) that are kept until it arrives.
const FORMATS_AHEAD: usize = 4;

/// One thing that happened during a stretch, in words, as the page and the report print it under the
/// stretch: when (wall clock), what kind ("song", "settings", "engine", "output", "offload",
/// "underruns", "error", "tuning", "format") and what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfEvent {
    #[serde(rename = "t")]
    pub wall_ms: i64,
    #[serde(rename = "k")]
    pub kind: String,
    #[serde(rename = "d")]
    pub detail: String,
}

/// The song the ear arrived on: what the server says of the file, and where its bytes come from.
/// Figures the platform does not know are 0 or empty.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfSong {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// The file's suffix and bit rate (kbps), sample rate, bit depth and channels, as the server has them.
    pub suffix: String,
    pub bit_rate: i32,
    pub sampling_rate: i32,
    pub bit_depth: i32,
    pub channels: i32,
    /// A finished download plays it.
    pub downloaded: bool,
    /// The stream cache's copy of it, by its key (`<id>:<quality>`), empty with none; and whether that
    /// copy is whole.
    pub cache_key: String,
    pub cached_whole: bool,
}

/// What the decoder was handed for a song, as the player's demuxer read it. -1 for a figure it does not give.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfFormat {
    /// The codec's and the container's MIME types.
    pub codec: String,
    pub container: String,
    pub rate: i32,
    pub channels: i32,
    /// Bits a second.
    pub bitrate: i32,
    /// Frames cut from the start and from the end (gapless).
    pub delay: i32,
    pub padding: i32,
}

/// Something the platform saw happen, for the stretch under way's timeline. Told as it happens, never
/// polled for.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum PerfNote {
    /// The ear arrived on a song.
    Song { song: PerfSong },
    /// The decoder was handed a song's format (`id`, the queue's), perhaps ahead of the song.
    Format { id: String, format: PerfFormat },
    /// The player service started with this engine ("rust", "exoplayer"), or ended (none).
    Engine { engine: Option<String> },
    /// The player opened an output (`key` tells one track from another), or let it go (none).
    Output { key: i64, output: Option<PerfOutput> },
    /// The output `key` has run dry `count` times since it was made.
    Underruns { key: i64, count: i32 },
    /// Playback failed, in the player's words.
    Error { message: String },
    /// The equalizer screen's tuning mode (a shallow buffer, so a band is heard at once) came on or off.
    Tuning { on: bool },
    /// The offload path's own account of something that matters to it: why a song ended, a play head
    /// that made no sense, offload given up.
    Offload { detail: String },
    /// The player took its CPU wake lock (`held`), or let it go: counted, not listed.
    WakeLock { held: bool },
}

/// What happened since the stretch under way began, and what it takes to say it: the settings as last
/// seen, the song playing, the output and its underruns. The platform tells it as things happen; a
/// stretch takes its events when it ends.
#[derive(Default)]
struct Timeline {
    events: Vec<PerfEvent>,
    dropped: i64,
    /// The settings as last seen, from which a change is told; none before the first look.
    settings: Option<HashMap<String, PrefValue>>,
    /// The settings the last event changed and their values before it: more changes to the same ones
    /// (a band dragged) make that one event say more rather than many events.
    run: Option<(Vec<String>, HashMap<String, PrefValue>)>,
    song: Option<String>,
    ahead: Vec<(String, PerfFormat)>,
    /// The output open, by its key, and whether it is offloaded.
    track: Option<(i64, bool)>,
    /// The underruns last read: the output's key, the count and when.
    underruns: Option<(i64, i32, i64)>,
    /// Since when an offloaded output has been open, and how long one was before that in this stretch.
    offloaded_from: Option<i64>,
    offloaded_ms: i64,
    /// Why the settings kept offload off when the stretch under way began (none: they did not).
    why_at_start: Option<Option<&'static str>>,
    /// Since when the player's wake lock has been held, and how long it was before that in this stretch.
    lock_from: Option<i64>,
    lock_ms: i64,
    /// [`DATA_REQUESTS`] as the stretch under way began.
    requests_at_start: i64,
}

static TIMELINE: std::sync::LazyLock<std::sync::Mutex<Timeline>> = std::sync::LazyLock::new(Default::default);

fn timeline() -> std::sync::MutexGuard<'static, Timeline> {
    TIMELINE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Settings whose values stay out of a report: the servers and their keys. Only that they changed is said.
const PRIVATE: [&str; 4] = ["servers", "activeServerId", "paxSenixKey", "betterLyricsKey"];

impl Timeline {
    fn push(&mut self, wall_ms: i64, kind: &str, detail: String) {
        if self.events.len() >= MOST_EVENTS {
            self.events.remove(0);
            self.dropped += 1;
        }
        self.events.push(PerfEvent { wall_ms, kind: kind.into(), detail });
    }

    /// `settings_why` is why the settings keep offload off now, for an output that left it without saying.
    fn note(&mut self, t: i64, note: PerfNote, settings_why: Option<&str>) {
        match note {
            PerfNote::Song { song } => {
                let format = self.ahead.iter().position(|(id, _)| *id == song.id).map(|i| self.ahead.remove(i).1);
                let mut d = song_words(&song);
                if let Some(f) = format {
                    d.push_str(&format!("; decoder input {}", format_words(&f)));
                }
                self.song = Some(song.id);
                self.push(t, "song", d);
            }
            PerfNote::Format { id, format } => {
                if self.song.as_deref() == Some(id.as_str()) {
                    self.push(t, "format", format!("decoder input {}", format_words(&format)));
                } else {
                    self.ahead.retain(|(i, _)| *i != id);
                    self.ahead.push((id, format));
                    if self.ahead.len() > FORMATS_AHEAD {
                        self.ahead.remove(0);
                    }
                }
            }
            PerfNote::Engine { engine } => match engine {
                Some(e) => self.push(t, "engine", format!("the player service started with the {e} engine")),
                None => self.push(t, "engine", "the player service ended".into()),
            },
            PerfNote::Output { key, output } => self.output(t, key, output, settings_why),
            PerfNote::Underruns { key, count } => self.underruns(t, key, count),
            PerfNote::Error { message } => {
                let short: String = message.chars().take(400).collect();
                self.push(t, "error", short);
            }
            PerfNote::Tuning { on } => {
                let d = if on { "on: the equalizer screen is open, the output takes a shallow buffer" } else { "off: the deep buffer is back" };
                self.push(t, "tuning", d.into());
            }
            PerfNote::Offload { detail } => {
                let short: String = detail.chars().take(400).collect();
                self.push(t, "offload", short);
            }
            PerfNote::WakeLock { held } => {
                if held {
                    self.lock_from.get_or_insert(t);
                } else if let Some(from) = self.lock_from.take() {
                    self.lock_ms += (t - from).max(0);
                }
            }
        }
    }

    fn output(&mut self, t: i64, key: i64, output: Option<PerfOutput>, settings_why: Option<&str>) {
        let was = self.track.is_some_and(|(_, off)| off);
        let Some(o) = output else {
            if self.track.take().is_some() {
                self.offload_ends(t);
                self.push(t, "output", "let go".into());
            }
            return;
        };
        let again = self.track.replace((key, o.offloaded)).is_some();
        let line = output_line(&o);
        let line = line.strip_prefix("output: ").unwrap_or(&line);
        self.push(t, "output", format!("{} {line}", if again { "reopened:" } else { "opened:" }));
        if o.offloaded && !was {
            self.offloaded_from = Some(t);
            self.push(t, "offload", "entered: the audio chip decodes".into());
        } else if !o.offloaded && was {
            self.offload_ends(t);
            let why = if !o.pcm_why.is_empty() { o.pcm_why.as_str() } else { settings_why.unwrap_or("the platform opened the output for PCM") };
            self.push(t, "offload", format!("left: {why}"));
        }
    }

    fn offload_ends(&mut self, t: i64) {
        if let Some(from) = self.offloaded_from.take() {
            self.offloaded_ms += (t - from).max(0);
        }
    }

    /// The output's underrun count as read now: an increase is an event, said with the reading before,
    /// between which and now it first happened.
    fn underruns(&mut self, t: i64, key: i64, count: i32) {
        let (before, since) = match self.underruns {
            Some((k, n, at)) if k == key => (n, Some(at)),
            _ => (0, None),
        };
        self.underruns = Some((key, count, t));
        if count > before {
            let since = since.map_or_else(|| "since the output opened".to_string(), |at| format!("since {}", clock(at)));
            self.push(t, "underruns", format!("{} more, {count} on this output, {since}", count - before));
        }
    }

    /// A change of the settings, told against the ones seen before; the first look only sets them.
    fn settings(&mut self, t: i64, now: HashMap<String, PrefValue>) {
        let Some(before) = self.settings.replace(now.clone()) else { return };
        let mut keys: Vec<String> = now.keys().chain(before.keys()).filter(|k| before.get(*k) != now.get(*k)).cloned().collect();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            return;
        }
        let same_run = self.events.last().is_some_and(|e| e.kind == "settings") && self.run.as_ref().is_some_and(|(k, _)| *k == keys);
        let old = match self.run.take() {
            Some((_, old)) if same_run => old,
            _ => keys.iter().filter_map(|k| before.get(k).map(|v| (k.clone(), v.clone()))).collect(),
        };
        let detail = keys.iter().map(|k| setting_change(k, old.get(k), now.get(k))).collect::<Vec<_>>().join(", ");
        if same_run {
            if let Some(e) = self.events.last_mut() {
                e.detail = format!("{detail} (last at {})", clock(t));
            }
        } else {
            self.push(t, "settings", detail);
        }
        self.run = Some((keys, old));
    }

    /// The stretch from `start` to `end` ends: its events (none for a blink, whose events go on to the
    /// next stretch), how many were not kept, and how long an offloaded output was open in it.
    fn close(&mut self, start: i64, end: i64, kept: bool) -> (Vec<PerfEvent>, i64, i64) {
        let off = self.offloaded_ms + self.offloaded_from.map_or(0, |f| (end - f.max(start)).max(0));
        self.offloaded_ms = 0;
        if self.offloaded_from.is_some() {
            self.offloaded_from = Some(end);
        }
        self.run = None;
        if !kept {
            return (Vec::new(), 0, off);
        }
        (std::mem::take(&mut self.events), std::mem::take(&mut self.dropped), off)
    }

    /// How long the wake lock was held from `start` to `end`, and the platform's asks for more since the
    /// stretch began; `close`: the stretch ends there, and the next one counts from `end`.
    fn kept(&mut self, start: i64, end: i64, close: bool) -> (i64, i64) {
        let held = self.lock_ms + self.lock_from.map_or(0, |f| (end - f.max(start)).max(0));
        let total = DATA_REQUESTS.load(std::sync::atomic::Ordering::Relaxed);
        let requests = total - self.requests_at_start;
        if close {
            self.lock_ms = 0;
            if self.lock_from.is_some() {
                self.lock_from = Some(end);
            }
            self.requests_at_start = total;
        }
        (held, requests)
    }

    /// The same for the stretch under way, left as it is.
    fn so_far(&self, start: i64, now: i64) -> (Vec<PerfEvent>, i64, i64) {
        let off = self.offloaded_ms + self.offloaded_from.map_or(0, |f| (now - f.max(start)).max(0));
        (self.events.clone(), self.dropped, off)
    }

    /// "Start fresh": the stretch under way begins again with nothing in it.
    fn forget(&mut self) {
        self.events.clear();
        self.dropped = 0;
        self.run = None;
        self.offloaded_ms = 0;
        self.lock_ms = 0;
        self.requests_at_start = DATA_REQUESTS.load(std::sync::atomic::Ordering::Relaxed);
    }
}

/// Something happened, at `wall_ms`: it goes on the timeline of the stretch under way.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_note(wall_ms: i64, note: PerfNote) {
    if matches!(note, PerfNote::Engine { .. }) {
        crate::invariants::engine_changed(wall_ms);
    }
    let why = offload_reason();
    timeline().note(wall_ms, note, why);
}

/// The settings changed (or are read for the first time): which ones, and to what, go on the timeline.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_note_settings(wall_ms: i64) {
    let Some(p) = settings_store::settings_current() else { return };
    timeline().settings(wall_ms, nori_settings::settings::save(&p));
}

/// Something the core decided that a report should show (the lyrics chosen for a song, with their
/// score), on the timeline of the stretch under way. Only while the perf build's watch is on: every other
/// build keeps nothing.
pub fn note_core(kind: &str, line: &str) {
    if !crate::invariants::on() {
        return;
    }
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    timeline().push(t, kind, line.chars().take(600).collect());
}

/// An invariant that did not hold (invariants.rs), on the timeline of the stretch under way: longer than
/// other events, since a stall quotes the engine's whole account of where it stood.
pub(crate) fn note_invariant(wall_ms: i64, line: &str) {
    timeline().push(wall_ms, "invariant", line.chars().take(2_000).collect());
}

/// Something the output said of itself (the equalizer screen's shallow track: how deep, and why), on the
/// timeline of the stretch under way under `kind`.
pub(crate) fn note_output(wall_ms: i64, kind: &str, line: &str) {
    timeline().push(wall_ms, kind, line.chars().take(400).collect());
}

/// "21:05:12", for the invariants' own list.
pub(crate) fn clock_words(wall_ms: i64) -> String {
    clock(wall_ms)
}

/// Why the settings keep offload off, for the invariants' look at the engine.
pub(crate) fn offload_blocked() -> Option<&'static str> {
    offload_reason()
}

/// The timeline since `since_ms` (wall clock), one line per event as the report prints them: the
/// stretches that ended since and the one under way. For the self test, which quotes what happened
/// while a check ran.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_events_since(since_ms: i64) -> Vec<String> {
    let mut events: Vec<PerfEvent> = perf_log_rows(since_ms).into_iter().flat_map(|s| s.ev).collect();
    events.extend(timeline().events.iter().cloned());
    events.retain(|e| e.wall_ms >= since_ms);
    events.sort_by_key(|e| e.wall_ms);
    events.dedup();
    events.iter().map(|e| format!("{} {}: {}", clock(e.wall_ms), e.kind, e.detail)).collect()
}

/// Why the settings keep the audio chip from decoding (`nori_player::policy::offload_blocked`, over the
/// settings as the output policy reads them); none when they allow it, or before they are open.
fn offload_reason() -> Option<&'static str> {
    let s = settings_store::settings_current()?;
    let prefs = nori_model::AudioPrefs {
        dsp: s.sound_chain_on(),
        skip_silence: s.skip_silence,
        offload: s.offload,
        crossfade_s: s.crossfade_sec,
        auto_mix: s.auto_mix,
        speed: s.speed,
        pitch: s.pitch,
    };
    let output = nori_model::OutputState { hi_res: s.hi_res, ..Default::default() };
    nori_player::policy::offload_blocked(&prefs, &output)
}

/// What the stretch's tag says of offload: whether the settings and the output asked for it, which is
/// not whether the output took it (the output line and the offloaded time say that).
fn offload_tag(wanted: bool, settings_why: Option<&str>) -> String {
    match settings_why {
        Some(why) => format!("offload not wanted: {why}"),
        None if wanted => "offload wanted".into(),
        None => "offload not wanted: the output (something USB, or a track it refused)".into(),
    }
}

/// A stretch's settings tag, with the words older rows had for offload said as they are now: their bare
/// "offloaded" was only ever the settings asking for it.
fn cfg_words(cfg: &str) -> String {
    if let Some(head) = cfg.strip_suffix(", offloaded") {
        format!("{head}, offload wanted")
    } else if let Some(head) = cfg.strip_suffix(", on the CPU") {
        format!("{head}, offload not wanted")
    } else {
        cfg.to_string()
    }
}

/// "wake lock held 12 s of 10 min 00 s (2 %), nori-engine 0.3 wakeups/s, the chip asked for more 0.3
/// times/s": what an offloaded stretch cost the CPU, beside the platform's own pace.
fn awake_words(lock_ms: i64, engine_wakeups: i64, requests: i64, of_ms: i64) -> String {
    let pct = if of_ms > 0 { lock_ms as f64 * 100.0 / of_ms as f64 } else { 0.0 };
    format!(
        "wake lock held {} of {} ({} %), {ENGINE_THREAD} {} wakeups/s, the chip asked for more {} times/s",
        duration(lock_ms),
        duration(of_ms),
        fixed(pct, 0),
        fixed(per_s(engine_wakeups, of_ms), 2),
        fixed(per_s(requests, of_ms), 2)
    )
}

/// "offloaded 45 min 00 s of 1 h 00 min (75 %)".
fn offloaded_words(off_ms: i64, of_ms: i64) -> String {
    let pct = if of_ms > 0 { off_ms as f64 * 100.0 / of_ms as f64 } else { 0.0 };
    format!("offloaded {} of {} ({} %)", duration(off_ms), duration(of_ms), fixed(pct, 0))
}

/// "Title by Artist, FLAC 44100 Hz 16-bit stereo 1011 kbps on the server, from the download".
fn song_words(s: &PerfSong) -> String {
    let mut out = if s.artist.is_empty() { s.title.clone() } else { format!("{} by {}", s.title, s.artist) };
    let mut file = Vec::new();
    if !s.suffix.is_empty() {
        file.push(s.suffix.to_uppercase());
    }
    if s.sampling_rate > 0 {
        file.push(format!("{} Hz", s.sampling_rate));
    }
    if s.bit_depth > 0 {
        file.push(format!("{}-bit", s.bit_depth));
    }
    match s.channels {
        1 => file.push("mono".into()),
        2 => file.push("stereo".into()),
        n if n > 2 => file.push(format!("{n} channels")),
        _ => {}
    }
    if s.bit_rate > 0 {
        file.push(format!("{} kbps", s.bit_rate));
    }
    if !file.is_empty() {
        out.push_str(&format!(", {} on the server", file.join(" ")));
    }
    let quality = s.cache_key.rsplit_once(':').map_or(s.cache_key.as_str(), |(_, q)| q);
    out.push_str(&if s.downloaded {
        ", from the download".to_string()
    } else if s.cache_key.is_empty() {
        ", streamed from the network".to_string()
    } else if s.cached_whole {
        format!(", from the stream cache ({quality})")
    } else {
        format!(", streamed ({quality}), in the stream cache in part")
    });
    out
}

/// "audio/mpeg, 44100 Hz stereo, 320 kbps, encoder delay 576, padding 1152", the container named when it
/// is not the codec's own.
fn format_words(f: &PerfFormat) -> String {
    let mut out = f.codec.clone();
    if !f.container.is_empty() && f.container != f.codec {
        out.push_str(&format!(" in {}", f.container));
    }
    if f.rate > 0 {
        out.push_str(&format!(", {} Hz", f.rate));
    }
    match f.channels {
        1 => out.push_str(" mono"),
        2 => out.push_str(" stereo"),
        n if n > 2 => out.push_str(&format!(" {n} channels")),
        _ => {}
    }
    if f.bitrate > 0 {
        out.push_str(&format!(", {} kbps", f.bitrate / 1000));
    }
    if f.delay >= 0 || f.padding >= 0 {
        out.push_str(&format!(", encoder delay {}, padding {}", f.delay.max(0), f.padding.max(0)));
    }
    out
}

/// One setting's change: "eqEnabled off → on"; a private or long value only as changed.
fn setting_change(key: &str, before: Option<&PrefValue>, after: Option<&PrefValue>) -> String {
    let said = |v: Option<&PrefValue>| -> Option<String> {
        match v {
            None => Some("none".into()),
            Some(PrefValue::Flag { v }) => Some(if *v { "on" } else { "off" }.into()),
            Some(PrefValue::Number { v }) => Some(v.to_string()),
            Some(PrefValue::Big { v }) => Some(v.to_string()),
            Some(PrefValue::Decimal { v }) => Some(fixed(*v as f64, 2)),
            Some(PrefValue::Text { v }) if v.chars().count() <= 24 && !v.contains('\n') => Some(if v.is_empty() { "empty".into() } else { v.clone() }),
            Some(_) => None,
        }
    };
    match (said(before), said(after)) {
        (Some(a), Some(b)) if !PRIVATE.contains(&key) => format!("{key} {a} → {b}"),
        _ => format!("{key} changed"),
    }
}

/// "21:05:12": when something happened, by the phone's own clock.
fn clock(wall_ms: i64) -> String {
    let local = wall_ms + nori_library::library::local_offset_s(wall_ms.div_euclid(1000)) * 1000;
    let secs = local.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// A stretch's timeline, one line per event: "21:05:12 song: ...".
fn event_lines(s: &PerfStretch) -> Vec<String> {
    let dropped = (s.evx > 0).then(|| format!("({} earlier events not kept)", s.evx));
    dropped.into_iter().chain(s.ev.iter().map(|e| format!("{} {}: {}", clock(e.wall_ms), e.kind, e.detail))).collect()
}

// ---- the app's own log and crashes ----

/// What logcat had for the app when it was asked (the platform reads it only when the report is shared
/// or the page's log is opened): the process's own recent lines, and the crash buffer, which holds the
/// app's earlier processes too. Either may be empty, or say why it could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfLogs {
    pub app: String,
    pub crash: String,
}

/// How much of each a report carries, in characters: the log's last lines, a crash's first.
pub const LOG_CHARS: usize = 60_000;
pub const CRASH_CHARS: usize = 16_000;

/// A crash kept in the app's database, so it outlives logcat's buffer: `kind` "exception" is the one the
/// uncaught exception handler wrote as the process died, "buffer" the crash buffer as last seen.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeptCrash {
    kind: String,
    at_ms: i64,
    text: String,
}

fn crash_table(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS perf_crashes(kind TEXT PRIMARY KEY, at_ms INTEGER NOT NULL, text TEXT NOT NULL)")
}

/// Keeps `text` as the last crash of its kind; the same text again keeps the time it was first seen.
fn keep_crash(c: &Connection, kind: &str, at_ms: i64, text: &str) -> rusqlite::Result<()> {
    crash_table(c)?;
    let text: String = text.chars().take(CRASH_CHARS).collect();
    let same: Option<String> = c.query_row("SELECT text FROM perf_crashes WHERE kind=?1", [kind], |r| r.get(0)).ok();
    if same.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    c.execute("INSERT OR REPLACE INTO perf_crashes(kind, at_ms, text) VALUES(?1, ?2, ?3)", params![kind, at_ms, text])?;
    Ok(())
}

fn crashes(c: &Connection) -> rusqlite::Result<Vec<KeptCrash>> {
    crash_table(c)?;
    let mut st = c.prepare("SELECT kind, at_ms, text FROM perf_crashes ORDER BY at_ms DESC")?;
    let out = st.query_map([], |r| Ok(KeptCrash { kind: r.get(0)?, at_ms: r.get(1)?, text: r.get(2)? }))?.collect();
    out
}

/// A crash, kept at once on the calling thread (the process may be about to die): the uncaught exception
/// handler's trace ("exception"), or the crash buffer as read at start ("buffer"). Nothing before the
/// settings are open, or with empty `text`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_crash_keep(kind: String, at_ms: i64, text: String) {
    if text.trim().is_empty() {
        return;
    }
    let Some(db) = settings_store::app_db() else { return };
    let kept = keep_crash(&db.lock(), &kind, at_ms, &text);
    if let Err(e) = kept {
        nori_model::alog::info(&format!("perf log: could not keep a crash: {e}"));
    }
}

fn perf_crashes_kept() -> Vec<KeptCrash> {
    let Some(db) = settings_store::app_db() else { return Vec::new() };
    let read = crashes(&db.lock());
    read.unwrap_or_default()
}

/// The page's log section, as the report ends with it.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_log_text(logs: PerfLogs) -> String {
    log_section(&logs, &perf_crashes_kept())
}

// ---- the app's own lines as an invariant broke ----

/// How many breaks keep the app's own lines with them: the latest.
pub const BREAK_LOGS: usize = 3;

fn break_log_table(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS perf_break_logs(at_ms INTEGER NOT NULL, line TEXT NOT NULL, text TEXT NOT NULL)")
}

/// "21:05:12.345", for a line of the app's own log.
fn clock_ms(wall_ms: i64) -> String {
    format!("{}.{:03}", clock(wall_ms), wall_ms.rem_euclid(1000))
}

/// The app's latest lines (nori_model::alog keeps them, the core's and the Kotlin's under the `nori` tag)
/// as the break `line` happened at `at_ms`, kept in the app's database with it: logcat's buffer is the
/// whole system's and turns over in minutes, and the app may be started again before the report is
/// shared. Only the latest [`BREAK_LOGS`] are kept.
/// The lines are copied on the calling thread, as they stand at the break; the database is written on a
/// thread of its own, since a break may be seen on the audio threads.
pub(crate) fn keep_break_log(at_ms: i64, line: &str) {
    let lines = nori_model::alog::recent();
    let line = line.to_string();
    let write = move || {
        let text = lines.iter().map(|(t, l)| format!("{} {l}", clock_ms(*t))).collect::<Vec<_>>().join("\n");
        let Some(db) = settings_store::app_db() else { return };
        let kept = keep_break(&db.lock(), at_ms, &line, &text);
        if let Err(e) = kept {
            nori_model::alog::info(&format!("perf log: could not keep the lines of a break: {e}"));
        }
    };
    if std::thread::Builder::new().name("nori-perf-log".into()).spawn(write).is_err() {
        nori_model::alog::info("perf log: no thread to keep the lines of a break");
    }
}

fn keep_break(c: &Connection, at_ms: i64, line: &str, text: &str) -> rusqlite::Result<()> {
    break_log_table(c)?;
    let line: String = line.chars().take(600).collect();
    c.execute("INSERT INTO perf_break_logs(at_ms, line, text) VALUES(?1, ?2, ?3)", params![at_ms, line, text])?;
    c.execute("DELETE FROM perf_break_logs WHERE rowid NOT IN (SELECT rowid FROM perf_break_logs ORDER BY at_ms DESC, rowid DESC LIMIT ?1)", [BREAK_LOGS as i64])?;
    Ok(())
}

fn break_logs(c: &Connection) -> rusqlite::Result<Vec<(i64, String, String)>> {
    break_log_table(c)?;
    let mut st = c.prepare("SELECT at_ms, line, text FROM perf_break_logs ORDER BY at_ms DESC, rowid DESC")?;
    let out = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect();
    out
}

fn break_logs_kept() -> Vec<(i64, String, String)> {
    let Some(db) = settings_store::app_db() else { return Vec::new() };
    let read = break_logs(&db.lock());
    read.unwrap_or_default()
}

/// The app's own lines as each of the latest breaks happened, newest first; nothing when none was kept.
fn break_log_section(kept: &[(i64, String, String)]) -> String {
    let mut out = String::new();
    for (at, line, text) in kept {
        out.push_str(&format!("The app's own lines as an invariant broke, {} ({} lines): {line}\n", clock_ms(*at), text.lines().count()));
        out.push_str(if text.is_empty() { "(none)" } else { text });
        out.push_str("\n\n");
    }
    out
}

/// The last `chars` characters of `text`, from the start of a line.
fn tail(text: &str, chars: usize) -> &str {
    let n = text.chars().count();
    if n <= chars {
        return text;
    }
    let from = text.char_indices().nth(n - chars).map_or(0, |(i, _)| i);
    let cut = &text[from..];
    cut.find('\n').map_or(cut, |i| &cut[i + 1..])
}

/// The crashes and the log, the report's last part: a crash first, since it is what a log is read for.
fn log_section(logs: &PerfLogs, kept: &[KeptCrash]) -> String {
    let mut out = String::new();
    let buffer = logs.crash.trim();
    if !buffer.is_empty() {
        out.push_str(&format!("Crash buffer (this and earlier runs of the app)\n{}\n\n", tail(buffer, CRASH_CHARS).trim_end()));
    }
    for k in kept {
        match k.kind.as_str() {
            "exception" => out.push_str(&format!("Last uncaught exception, {}\n{}\n\n", when(k.at_ms), k.text.trim_end())),
            // The crash buffer as it was kept, once logcat no longer has it.
            "buffer" if buffer.is_empty() => out.push_str(&format!("Crash buffer as kept at {}\n{}\n\n", when(k.at_ms), k.text.trim_end())),
            _ => {}
        }
    }
    let app = tail(logs.app.trim_end(), LOG_CHARS);
    out.push_str(&format!("Log (this process, the last {} lines)\n", app.lines().count()));
    out.push_str(if app.is_empty() { "(empty)" } else { app });
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stretch(state: &str, ms: i64) -> PerfStretch {
        PerfStretch {
            state: state.into(),
            start_wall: 0,
            ms,
            cpu_ms: ms / 100,
            wakeups: ms / 100,
            alloc_bytes: 60 * 1024 * ms / 60_000,
            gcs: 2,
            pss_kb: 150 * 1024,
            uah: None,
            pct: 3,
            gauge_ma: None,
            temp_min: 301,
            temp_max: 314,
            frames: 0,
            janky: 0,
            worst_ms: 0.0,
            cfg: "engine exoplayer, on the CPU".into(),
            threads: Vec::new(),
            out: None,
            rx: None,
            tx: None,
            ev: Vec::new(),
            evx: 0,
            offloaded_ms: None,
            mem: None,
            wake_lock_ms: None,
            engine_wakeups: None,
            data_requests: None,
        }
    }

    fn thread(tid: i32, name: &str, cpu_ms: i64, switches: i64) -> PerfThread {
        PerfThread { tid, name: name.into(), cpu_ms, switches }
    }

    fn counters(elapsed_ms: i64, switches: &[(i32, i64)]) -> PerfCounters {
        PerfCounters {
            elapsed_ms,
            wall_ms: 1_000 + elapsed_ms,
            cpu_ms: elapsed_ms / 50,
            threads: switches.iter().map(|&(tid, n)| thread(tid, &format!("t{tid}"), n / 10, n)).collect(),
            alloc_bytes: elapsed_ms * 10,
            gcs: elapsed_ms / 10_000,
            pss_kb: 100_000 + elapsed_ms / 100,
            charge_uah: Some(4_000_000 - elapsed_ms / 10),
            capacity_pct: 90 - (elapsed_ms / 600_000) as i32,
            gauge_ua: Some(-150_000 - elapsed_ms / 1000),
            temp_deci: 300 + (elapsed_ms / 60_000) as i32,
            rx_bytes: Some(elapsed_ms * 100),
            tx_bytes: Some(elapsed_ms),
            memory: None,
        }
    }

    fn output() -> PerfOutput {
        PerfOutput {
            engine: "rust".into(),
            rate: 44_100,
            channels: 2,
            encoding: 2,
            asked_bytes: 507_150 * 4,
            size_frames: 22_050,
            capacity_frames: 22_050,
            mode_asked: 2,
            mode: 0,
            offloaded: false,
            device_type: 2,
            device_name: String::new(),
            underruns: 3,
            play_state: 3,
            pcm_why: String::new(),
        }
    }

    /// Printed by Java's `String.format` (OpenJDK 21, root locale): value bits (f64), places, plus, result.
    const JAVA_FIXED: &[(u64, i32, bool, &str)] = include!("../testdata/java_fixed.in");

    #[test]
    fn figures_round_as_java_does() {
        for &(bits, places, plus, want) in JAVA_FIXED {
            let v = f64::from_bits(bits);
            let got = fixed(v, places);
            let got = if plus && !v.is_sign_negative() { format!("+{got}") } else { got };
            assert_eq!(got, want, "{v} to {places}");
        }
        assert_eq!((fixed(0.15, 1), fixed(62.5, 0), fixed(-0.04, 1), fixed(9.96, 1)), ("0.2".into(), "63".into(), "-0.0".into(), "10.0".into()));
        assert_eq!((fixed(1e-7, 3), fixed(0.0005, 3), fixed(f64::NAN, 1)), ("0.000".into(), "0.001".into(), "0.0".into()));
    }

    #[test]
    fn keeps_rows_in_order_and_forgets_old_ones() {
        let c = Connection::open_in_memory().unwrap();
        add(&c, 1_000, "a").unwrap();
        add(&c, 2_000, "b").unwrap();
        assert_eq!(rows(&c, 0).unwrap(), vec!["a", "b"]);
        assert_eq!(rows(&c, 1_500).unwrap(), vec!["b"]);
        // A row two weeks and a bit later takes the first two with it.
        add(&c, 2_001 + KEEP_MS, "c").unwrap();
        assert_eq!(rows(&c, 0).unwrap(), vec!["c"]);
    }

    #[test]
    fn reads_nothing_from_a_database_without_the_table() {
        let c = Connection::open_in_memory().unwrap();
        assert!(rows(&c, 0).unwrap().is_empty());
    }

    #[test]
    fn a_row_reads_back_as_the_platform_wrote_it_before() {
        // The shape the Kotlin recorder wrote, keys and all: the rows already on phones read as they are.
        let old = r#"{"s":"off-playing","t0":1700000000000,"ms":600000,"cpu":1200,"wk":3000,"al":2048,"gc":1,"pss":150000,"uah":42000,"pct":1,"gma":152.5,"tmin":301,"tmax":305,"fr":0,"jk":0,"worst":41.5,"cfg":"engine exoplayer, on the CPU"}"#;
        let s = &parsed(vec![old.into(), "not a stretch".into()])[..];
        assert_eq!(s.len(), 1, "a row that does not read is passed over");
        assert_eq!((s[0].state.as_str(), s[0].uah, s[0].gauge_ma, s[0].worst_ms), ("off-playing", Some(42_000), Some(152.5), 41.5));
        let back: serde_json::Value = serde_json::from_str(&serde_json::to_string(&s[0]).unwrap()).unwrap();
        assert_eq!(back, serde_json::from_str::<serde_json::Value>(old).unwrap(), "and is written the same way");
        let none = PerfStretch { uah: None, gauge_ma: None, ..s[0].clone() };
        assert!(!serde_json::to_string(&none).unwrap().contains("uah"), "a figure the phone has not is left out");
        let why = PerfStretch {
            threads: vec![PerfThreadUse { name: "nori-track".into(), cpu_ms: 3, wakeups: 90, born: true }],
            out: Some(output()),
            rx: Some(1),
            tx: Some(0),
            ..s[0].clone()
        };
        assert_eq!(parsed(vec![serde_json::to_string(&why).unwrap()]), [why], "the threads, the output and the network are kept too");
    }

    #[test]
    fn a_stretch_is_filed_by_charging_then_the_screen_then_the_music_then_the_app() {
        assert_eq!(perf_state(true, false, true, false, false), "charging");
        assert_eq!(perf_state(false, false, true, true, true), "off-playing");
        assert_eq!(perf_state(false, false, false, true, false), "off-paused");
        assert_eq!(perf_state(false, true, false, true, true), "on-paused");
        assert_eq!(perf_state(false, true, true, false, true), "on-playing-away");
        assert_eq!(perf_state(false, true, true, true, true), "on-playing-player");
        assert_eq!(perf_state(false, true, true, true, false), "on-playing-app");
        assert_eq!(state_name("on-playing-away"), "Screen on, playing, another app");
        assert_eq!(state_name("new"), "new", "a state this build does not know keeps its key");
    }

    #[test]
    fn the_settings_line_names_the_path_that_plays() {
        let p = nori_settings::settings::StoredPrefs {
            eq_enabled: true,
            auto_mix: false,
            crossfade_sec: 6,
            offload: true,
            hi_res: false,
            bit_perfect: false,
            ..Default::default()
        };
        assert_eq!(config(None, &p), "engine rust, eq on, automix off, crossfade 6 s, offload on, hi-res off, bit-perfect off");
        assert!(config(Some("other".into()), &p).starts_with("engine other, "), "the running service's own path wins");
    }

    #[test]
    fn two_readings_make_a_stretch() {
        let a = counters(10_000, &[(1, 100), (2, 50), (3, 7)]);
        let b = counters(610_000, &[(1, 400), (2, 60), (4, 9)]);
        let drawn = PerfFrames { frames: 120, janky: 3, worst_ns: 41_500_000 };
        let s = perf_stretch("off-playing".into(), "engine rust".into(), a.clone(), b.clone(), drawn, true, Some(output()), false).unwrap();
        assert_eq!((s.ms, s.start_wall, s.cpu_ms), (600_000, 11_000, 12_000));
        assert_eq!(s.wakeups, 310, "only the threads alive at both ends");
        assert_eq!((s.uah, s.pct, s.pss_kb), (Some(60_000), 1, 106_100));
        // The gauge's two readings averaged in whole µA before becoming mA, as the recorder did.
        assert_eq!(s.gauge_ma, Some(((150_010 + 150_610) / 2) as f64 / 1000.0));
        assert_eq!((s.temp_min, s.temp_max, s.frames, s.janky, s.worst_ms), (300, 310, 120, 3, 41.5));
        assert_eq!(s.cfg, "engine rust, offload wanted", "the settings asking for offload, not the output taking it");
        assert_eq!(s.offloaded_ms, Some(0), "no output was opened offloaded");
        assert_eq!((s.rx, s.tx), (Some(60_000_000), Some(600_000)));
        assert_eq!(s.out, Some(output()));
        let blink = counters(12_000, &[]);
        assert_eq!(perf_stretch("x".into(), String::new(), a.clone(), blink.clone(), drawn, false, None, false), None, "a blink is not kept");
        assert!(perf_stretch("x".into(), String::new(), a, blink, drawn, false, None, true).is_some(), "but the one under way is shown");
    }

    #[test]
    fn a_stretch_keeps_the_threads_that_woke_most() {
        let mut a = counters(0, &[]);
        a.threads = vec![thread(1, "main", 500, 1_000), thread(2, "nori-track", 10, 100), thread(3, "Thread-3", 0, 5), thread(9, "gone", 0, 0)];
        let mut b = counters(60_000, &[]);
        b.threads = vec![
            thread(1, "main", 520, 1_060),
            thread(2, "nori-track", 40, 1_300),
            thread(3, "binder:1_3", 3, 40),
            thread(4, "nori-load", 90, 600),
            thread(5, "idle", 0, 0),
            thread(6, "a", 1, 1),
            thread(7, "b", 1, 1),
            thread(8, "c", 2, 1),
        ];
        let s = perf_stretch("off-playing".into(), String::new(), a, b, PerfFrames { frames: 0, janky: 0, worst_ns: 0 }, false, None, false).unwrap();
        assert_eq!(s.wakeups, 60 + 1_200 + 35, "the total is still the threads alive at both ends");
        let names: Vec<(&str, i64, i64, bool)> = s.threads.iter().map(|t| (t.name.as_str(), t.wakeups, t.cpu_ms, t.born)).collect();
        assert_eq!(
            names,
            [("nori-track", 1_200, 30, false), ("nori-load", 600, 90, true), ("main", 60, 20, false), ("binder:1_3", 40, 3, true), ("c", 1, 2, true), ("a", 1, 1, true)],
            "the most woken first, a new thread with all it did, a thread id taken again as a new thread, none that did nothing"
        );
        assert_eq!(
            threads_line(&s).unwrap(),
            "threads by wakeups: nori-track 20.0/s 30 ms, nori-load 10.0/s 90 ms (new), main 1.0/s 20 ms, binder:1_3 0.7/s 3 ms (new), c 0.0/s 2 ms (new), a 0.0/s 1 ms (new)"
        );
    }

    #[test]
    fn the_output_says_what_was_asked_and_what_was_given() {
        assert_eq!(
            output_line(&output()),
            "output: rust, 44100 Hz stereo 16-bit, buffer 500 ms of 11500 ms asked, mode power saving asked, none given, PCM, to Phone speaker, 3 underruns, playing"
        );
        let exo = PerfOutput {
            engine: "exoplayer".into(),
            encoding: 9,
            asked_bytes: 300 * 1024,
            size_frames: 441_000,
            capacity_frames: 882_000,
            mode_asked: 0,
            mode: 0,
            offloaded: true,
            device_type: 8,
            device_name: "Buds".into(),
            underruns: 0,
            play_state: 2,
            ..output()
        };
        assert_eq!(
            output_line(&exo),
            "output: exoplayer, 44100 Hz stereo MP3, buffer 10000 ms (300 KB asked), up to 20000 ms, mode none asked, none given, offload given, to Bluetooth: Buds, 0 underruns, paused"
        );
        // Offload wanted and the music on the CPU all the same: the player says why.
        let why = PerfOutput { pcm_why: "MP3 with an encoder delay of 576 and padding of 1000 needs gapless offload, which the output does not do".into(), ..output() };
        assert!(
            output_line(&why).contains(", PCM (MP3 with an encoder delay of 576 and padding of 1000 needs gapless offload, which the output does not do), to Phone speaker"),
            "{}",
            output_line(&why)
        );
    }

    #[test]
    fn stretches_add_up_by_state_in_the_page_order() {
        let mut playing = stretch("off-playing", 3_600_000);
        playing.uah = Some(40_000);
        let t = totals(&[stretch("on-paused", 60_000), playing.clone(), stretch("new-state", 5_000), playing, stretch("charging", 90_000)]);
        let states: Vec<&str> = t.iter().map(|t| t.state.as_str()).collect();
        assert_eq!(states, ["off-playing", "on-paused", "charging", "new-state"]);
        assert_eq!(t[0].line(), "2 h 00 min, CPU 1.00 %, 10.0 wakeups/s, 60 KB/min allocated, 4 GCs, PSS 150 MB, 80.0 mAh (40.0 mAh/h)");
        assert_eq!(t[1].line(), "1 min 00 s, CPU 1.00 %, 10.0 wakeups/s, 60 KB/min allocated, 2 GCs, PSS 150 MB, 3 % (180.00 %/h)");
        assert_eq!(t[2].line(), "1 min 30 s, CPU 1.00 %, 10.0 wakeups/s, 60 KB/min allocated, 2 GCs, PSS 150 MB", "charging has no battery figures");
    }

    #[test]
    fn a_stretch_reads_on_one_line() {
        let mut s = stretch("on-playing-player", 187_000);
        s.frames = 900;
        s.janky = 12;
        s.worst_ms = 48.4;
        s.gauge_ma = Some(212.6);
        assert!(stretch_line(&s).ends_with(
            "  3 min 07 s, CPU 1.00 %, 10.0 wakeups/s, 60 KB/min, 2 GCs, PSS 150 MB, 3 %, gauge 213 mA, 30.1-31.4 °C, 900 frames, 12 janky, worst 48 ms [engine exoplayer, offload not wanted]"
        ));
        s.uah = Some(9_350);
        assert!(stretch_line(&s).contains(", 9.4 mAh (180.0 mAh/h)"));
        assert_eq!(duration(42_999), "42 s");
        assert_eq!(duration(3_900_000), "1 h 05 min");
        assert_eq!(fixed(0.125, 2), "0.13", "Java's rounding, not the float's");
    }

    #[test]
    fn the_page_counts_the_frames_and_lists_the_newest_first() {
        let mut a = stretch("on-playing-app", 60_000);
        a.frames = 1_000;
        a.janky = 25;
        a.worst_ms = 33.3;
        let b = stretch("on-playing-app", 120_000);
        let mut live = stretch("off-playing", 10_000);
        live.worst_ms = 70.0;
        live.frames = 0;
        let p = page(vec![a, b], Some(live));
        assert_eq!(p.totals.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Screen off, playing", "Screen on, playing, other page (2)"]);
        let f = p.frames.unwrap();
        assert_eq!((f.title.as_str(), f.detail.as_str()), ("1000 frames, 25 janky (2.5 %)", "The slowest took 70 ms"));
        assert_eq!(p.live.unwrap().title, "Now: Screen off, playing");
        assert!(p.stretches[0].detail.contains("  2 min 00 s,"), "newest first");
        assert_eq!(page(Vec::new(), None), PerfPage { totals: Vec::new(), frames: None, live: None, stretches: Vec::new() });
        let mut why = stretch("off-playing", 60_000);
        why.threads = vec![PerfThreadUse { name: "nori-track".into(), cpu_ms: 12, wakeups: 1_440, born: false }];
        why.out = Some(output());
        let lines: Vec<String> = page(vec![why], None).stretches[0].detail.lines().map(String::from).collect();
        assert_eq!(lines.len(), 3, "the figures, then the threads and the output under them: {lines:?}");
        assert_eq!(lines[1], "threads by wakeups: nori-track 24.0/s 12 ms");
        assert!(lines[2].starts_with("output: rust, 44100 Hz"));
    }

    #[test]
    fn a_stretch_says_where_its_memory_was_as_it_ended() {
        let a = counters(0, &[]);
        let mut b = counters(60_000, &[]);
        b.memory = Some(PerfMemory { java_kb: 20 * 1024, native_kb: 80 * 1024, covers: 3, ..Default::default() });
        let s = perf_stretch("off-playing".into(), "engine rust".into(), a, b, PerfFrames { frames: 0, janky: 0, worst_ns: 0 }, false, None, true).unwrap();
        assert_eq!(s.mem.as_ref().map(|m| m.native_kb), Some(80 * 1024), "the end reading's");
        let lines: Vec<String> = page(vec![s.clone()], None).stretches[0].detail.lines().map(String::from).collect();
        assert!(lines.last().unwrap().starts_with("memory: PSS 100 MB = Java 20, native 80,"), "{lines:?}");
        // Kept and read back, as rows are.
        let back: PerfStretch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.mem, s.mem);
    }

    #[test]
    fn the_report_is_plain_text_in_columns() {
        let mut playing = stretch("off-playing", 3_600_000);
        playing.uah = Some(40_000);
        let d = PerfDevice {
            manufacturer: "Google".into(),
            model: "Pixel 8".into(),
            device: "shiba".into(),
            release: "16".into(),
            sdk: 36,
            version: "0.3.4".into(),
            sha: "abc1234".into(),
            build_type: "perf".into(),
        };
        let mut charging = stretch("charging", 90_000);
        charging.out = Some(output());
        charging.rx = Some(3 * 1024 * 1024 + 300 * 1024);
        charging.tx = Some(20 * 1024);
        let r = report(vec![playing, charging], &d, "", "12 ns", None);
        let lines: Vec<&str> = r.lines().collect();
        assert_eq!(lines[0], "nori perf report");
        assert_eq!(lines[1], "Device: Google Pixel 8 (shiba), Android 16 (API 36)");
        assert_eq!(lines[2], "Build: 0.3.4 (abc1234, perf)");
        assert!(lines[3].starts_with("Recorded: ") && lines[3].ends_with(", 2 stretches"));
        assert_eq!(lines[4], "Battery counter: yes (mAh)");
        assert_eq!(lines[6], "Invariant breaks: none recorded", "the breaks come first, even when there are none");
        assert_eq!(lines[8], "By state");
        assert_eq!(lines[9], "state                      time  CPU %  wakeups/s  KB/min  GCs  PSS MB   mAh  mAh/h   %/h  frames  janky %");
        assert_eq!(lines[10], "Screen off, playing  1 h 00 min   1.00       10.0      60    2     150  40.0   40.0  3.00       -        -");
        assert_eq!(lines[11], "Charging             1 min 30 s   1.00       10.0      60    2     150     -      -     -       -        -");
        assert_eq!(lines[13], "Stretches, newest first");
        assert!(lines[14].starts_with("Charging: ") && lines[14].contains(", network 3.3 MB in, 20 KB out"), "{}", lines[14]);
        assert!(lines[15].starts_with("    output: rust, "), "the output under its stretch: {}", lines[15]);
        assert!(lines[16].starts_with("Screen off, playing: "));
        assert_eq!(&lines[17..], ["", "Cover benchmark: 12 ns"], "a benchmark not run is left out");
    }

    #[test]
    fn the_report_opens_with_the_invariant_breaks_and_the_self_test() {
        let d = PerfDevice {
            manufacturer: "Samsung".into(),
            model: "SM-S901B".into(),
            device: "r0s".into(),
            release: "16".into(),
            sdk: 36,
            version: "0.3.4".into(),
            sha: "abc1234".into(),
            build_type: "perf".into(),
        };
        let mut older = stretch("off-playing", 600_000);
        older.ev = vec![PerfEvent { wall_ms: at(21, 0, 0), kind: "invariant".into(), detail: "skip: 3 skip presses moved 4 songs, from queue place 0 to 4".into() }];
        let mut newer = stretch("on-playing-app", 600_000);
        newer.ev = vec![
            PerfEvent { wall_ms: at(22, 0, 0), kind: "song".into(), detail: "a".into() },
            PerfEvent { wall_ms: at(22, 0, 5), kind: "invariant".into(), detail: "offload-starved: engine: playing, but ...".into() },
        ];
        let r = report(vec![older, newer], &d, "", "", Some("Self test: 20 passed, 1 failed\nFAIL  Rust: offload\n"));
        let lines: Vec<&str> = r.lines().collect();
        assert_eq!(lines[6], "Invariant breaks: 2 (newest first)");
        assert!(lines[7].ends_with("22:00:05 offload-starved: engine: playing, but ..."), "{}", lines[7]);
        assert!(lines[8].ends_with("21:00:00 skip: 3 skip presses moved 4 songs, from queue place 0 to 4"), "{}", lines[8]);
        assert_eq!(lines[10], "Self test: 20 passed, 1 failed");
        assert_eq!(lines[11], "FAIL  Rust: offload");
        assert_eq!(lines[13], "By state");
    }

    #[test]
    fn a_start_is_written_by_the_phone_clock() {
        let offset = nori_library::library::local_offset_s(0) * 1000;
        assert_eq!(when(-offset), "01-01 00:00");
        assert_eq!(when(-offset + 86_400_000 * 31 + 3_600_000 * 13 + 60_000 * 7), "02-01 13:07");
    }

    /// A wall-clock time that reads as `h:m:s` on the phone's clock, on the first day of 1970.
    fn at(h: i64, m: i64, s: i64) -> i64 {
        -nori_library::library::local_offset_s(0) * 1000 + (h * 3600 + m * 60 + s) * 1000
    }

    fn song(id: &str) -> PerfSong {
        PerfSong {
            id: id.into(),
            title: "Song".into(),
            artist: "Band".into(),
            suffix: "mp3".into(),
            bit_rate: 320,
            sampling_rate: 44_100,
            bit_depth: 0,
            channels: 2,
            downloaded: false,
            cache_key: String::new(),
            cached_whole: false,
        }
    }

    fn mp3() -> PerfFormat {
        PerfFormat { codec: "audio/mpeg".into(), container: "audio/mpeg".into(), rate: 44_100, channels: 2, bitrate: 320_000, delay: 576, padding: 1_152 }
    }

    fn lines(t: &Timeline) -> Vec<String> {
        t.events.iter().map(|e| format!("{} {}: {}", clock(e.wall_ms), e.kind, e.detail)).collect()
    }

    #[test]
    fn a_song_says_what_it_is_and_where_it_comes_from() {
        let mut t = Timeline::default();
        // The decoder reads the next song's format before the ear gets there: it waits for the song.
        t.note(at(21, 0, 0), PerfNote::Format { id: "b".into(), format: mp3() }, None);
        t.note(at(21, 0, 1), PerfNote::Song { song: PerfSong { downloaded: true, ..song("a") } }, None);
        t.note(at(21, 3, 0), PerfNote::Song { song: PerfSong { cache_key: "b:320mp3".into(), cached_whole: true, ..song("b") } }, None);
        // A format for the song playing (the first one after a start, or the output rebuilt) is said by itself.
        let bare = PerfFormat { container: String::new(), bitrate: -1, delay: -1, padding: -1, ..mp3() };
        t.note(at(21, 3, 1), PerfNote::Format { id: "b".into(), format: bare }, None);
        t.note(at(21, 6, 0), PerfNote::Song { song: PerfSong { cache_key: "c:128opus".into(), ..song("c") } }, None);
        let radio = PerfSong { title: "Radio".into(), artist: String::new(), suffix: String::new(), bit_rate: 0, sampling_rate: 0, channels: 0, ..song("r") };
        t.note(at(21, 9, 0), PerfNote::Song { song: radio }, None);
        assert_eq!(
            lines(&t),
            [
                "21:00:01 song: Song by Band, MP3 44100 Hz stereo 320 kbps on the server, from the download",
                "21:03:00 song: Song by Band, MP3 44100 Hz stereo 320 kbps on the server, from the stream cache (320mp3); decoder input audio/mpeg, 44100 Hz stereo, 320 kbps, encoder delay 576, padding 1152",
                "21:03:01 format: decoder input audio/mpeg, 44100 Hz stereo",
                "21:06:00 song: Song by Band, MP3 44100 Hz stereo 320 kbps on the server, streamed (128opus), in the stream cache in part",
                "21:09:00 song: Radio, streamed from the network",
            ]
        );
        let flac = PerfFormat { codec: "audio/flac".into(), container: "audio/mp4".into(), bitrate: -1, delay: 0, padding: 0, ..mp3() };
        assert_eq!(format_words(&flac), "audio/flac in audio/mp4, 44100 Hz stereo, encoder delay 0, padding 0");
    }

    #[test]
    fn offload_is_timed_from_the_outputs_opened() {
        let mut t = Timeline::default();
        let offloaded = PerfOutput { offloaded: true, encoding: 9, ..output() };
        t.note(at(10, 0, 0), PerfNote::Engine { engine: Some("exoplayer".into()) }, None);
        t.note(at(10, 0, 1), PerfNote::Output { key: 1, output: Some(offloaded.clone()) }, None);
        // Another offloaded track (a new song's format) is not offload entered again.
        t.note(at(10, 5, 0), PerfNote::Output { key: 2, output: Some(offloaded) }, None);
        t.note(at(10, 10, 1), PerfNote::Output { key: 3, output: Some(output()) }, Some("a crossfade is set"));
        let (ev, dropped, off) = t.close(at(10, 0, 0), at(10, 20, 0), true);
        assert_eq!((dropped, off), (0, 10 * 60_000), "offloaded from the first track to the PCM one");
        let said: Vec<String> = ev.iter().map(|e| format!("{} {}: {}", clock(e.wall_ms), e.kind, e.detail)).collect();
        assert_eq!(said[0], "10:00:00 engine: the player service started with the exoplayer engine");
        assert!(said[1].starts_with("10:00:01 output: opened: rust, 44100 Hz stereo MP3,") && said[1].contains(", offload given,"), "{}", said[1]);
        assert_eq!(said[2], "10:00:01 offload: entered: the audio chip decodes");
        assert!(said[3].starts_with("10:05:00 output: reopened: "));
        assert!(said[4].starts_with("10:10:01 output: reopened: rust, 44100 Hz stereo 16-bit") && said[4].contains(", PCM,"), "{}", said[4]);
        assert_eq!(said[5], "10:10:01 offload: left: a crossfade is set", "the settings say why when the player does not");
        assert_eq!(ev.len(), 6);
        assert!(t.events.is_empty(), "a stretch takes its events with it");

        // Offloaded across a stretch's end: each stretch counts its own part, and the one under way so far.
        t.note(at(11, 0, 0), PerfNote::Output { key: 4, output: Some(PerfOutput { offloaded: true, ..output() }) }, None);
        assert_eq!(t.close(at(10, 50, 0), at(11, 30, 0), true).2, 30 * 60_000);
        assert_eq!(t.so_far(at(11, 30, 0), at(11, 45, 0)).2, 15 * 60_000);
        // A blink keeps its events for the next stretch, but not its time.
        t.note(at(11, 50, 0), PerfNote::Tuning { on: true }, None);
        assert_eq!(t.close(at(11, 30, 0), at(11, 50, 1), false), (Vec::new(), 0, 20 * 60_000 + 1_000));
        t.note(at(11, 51, 0), PerfNote::Output { key: 4, output: None }, None);
        let (ev, _, off) = t.close(at(11, 50, 1), at(12, 0, 0), true);
        assert_eq!(off, 59_000);
        assert_eq!(ev.iter().map(|e| e.detail.as_str()).collect::<Vec<_>>(), ["on: the equalizer screen is open, the output takes a shallow buffer", "let go"]);
        // The player's own reason wins over the settings'.
        let mut t = Timeline::default();
        t.note(0, PerfNote::Output { key: 1, output: Some(PerfOutput { offloaded: true, ..output() }) }, None);
        let flac = PerfOutput { pcm_why: "FLAC is not decoded by this output".into(), ..output() };
        t.note(1, PerfNote::Output { key: 2, output: Some(flac) }, Some("AutoMix is on"));
        assert_eq!(t.events.last().unwrap().detail, "left: FLAC is not decoded by this output");
        // The offload path's own words go on the timeline as they are.
        t.note(2, PerfNote::Offload { detail: "a ended by the play head".into() }, None);
        let e = t.events.last().unwrap();
        assert_eq!((e.kind.as_str(), e.detail.as_str()), ("offload", "a ended by the play head"));
    }

    #[test]
    fn underruns_are_noted_when_they_grow_with_when_they_first_appeared() {
        let mut t = Timeline::default();
        t.underruns(at(8, 0, 0), 7, 0);
        t.underruns(at(8, 4, 0), 7, 0);
        t.underruns(at(8, 7, 30), 7, 3);
        t.underruns(at(8, 9, 0), 7, 3);
        // A new output counts from its own start.
        t.underruns(at(8, 12, 0), 8, 1);
        assert_eq!(
            lines(&t),
            ["08:07:30 underruns: 3 more, 3 on this output, since 08:04:00", "08:12:00 underruns: 1 more, 1 on this output, since the output opened"]
        );
    }

    #[test]
    fn a_settings_change_says_which_and_a_drag_is_one_event() {
        use nori_settings::settings::{save, StoredPrefs};
        let mut t = Timeline::default();
        let p = StoredPrefs::default();
        t.settings(at(9, 0, 0), save(&p));
        assert!(t.events.is_empty(), "the first look is where changes count from");
        let eq = StoredPrefs { eq_enabled: !p.eq_enabled, crossfade_sec: p.crossfade_sec + 6, ..p.clone() };
        t.settings(at(9, 1, 0), save(&eq));
        let on = |b: bool| if b { "on" } else { "off" };
        assert_eq!(
            lines(&t),
            [format!("09:01:00 settings: crossfadeSec {} → {}, eqEnabled {} → {}", p.crossfade_sec, eq.crossfade_sec, on(p.eq_enabled), on(!p.eq_enabled))]
        );
        // A slider dragged: one event, from where it was to where it ended.
        for (i, v) in [0.1f32, 0.2, 0.3].into_iter().enumerate() {
            t.settings(at(9, 2, i as i64), save(&StoredPrefs { balance: v, ..eq.clone() }));
        }
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.events[1].detail, format!("balance {} → 0.30 (last at 09:02:02)", fixed(p.balance as f64, 2)));
        t.settings(at(9, 3, 0), save(&StoredPrefs { paxsenix_key: "secret".into(), balance: 0.3, ..eq }));
        assert_eq!(t.events[2].detail, "paxSenixKey changed", "a key is never in a report");
    }

    #[test]
    fn a_stretch_keeps_its_timeline_bounded_and_prints_it_under_itself() {
        let mut t = Timeline::default();
        for i in 0..MOST_EVENTS as i64 + 5 {
            t.note(at(7, 0, i), PerfNote::Error { message: format!("e{i}") }, None);
        }
        let (ev, dropped, _) = t.close(at(7, 0, 0), at(8, 0, 0), true);
        assert_eq!((ev.len(), dropped, ev[0].detail.as_str()), (MOST_EVENTS, 5, "e5"), "the oldest go");
        let mut s = stretch("off-playing", 3_600_000);
        s.ev = ev;
        s.evx = dropped;
        s.out = Some(PerfOutput { offloaded: true, ..output() });
        s.offloaded_ms = Some(2_700_000);
        let back: PerfStretch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s, "kept with the stretch");
        let printed = event_lines(&s);
        assert_eq!(printed[0], "(5 earlier events not kept)");
        assert_eq!(printed[1], "07:00:05 error: e5");
        assert!(stretch_line(&s).contains(", offloaded 45 min 00 s of 1 h 00 min (75 %) [engine exoplayer, offload not wanted]"), "{}", stretch_line(&s));
        let p = page(vec![s.clone()], None);
        assert_eq!(p.stretches[0].events.len(), MOST_EVENTS + 1, "the page folds them under the stretch");
        assert!(p.totals[0].detail.ends_with(", offloaded 45 min 00 s of 1 h 00 min (75 %)"), "{}", p.totals[0].detail);
        // The report: the summary with the time really offloaded, then the stretch, its output and its events.
        let d = PerfDevice {
            manufacturer: "samsung".into(),
            model: "SM-S901B".into(),
            device: "r0s".into(),
            release: "15".into(),
            sdk: 35,
            version: "0.3.4".into(),
            sha: "abc1234".into(),
            build_type: "perf".into(),
        };
        let r = report(vec![s], &d, "", "", None);
        let lines: Vec<&str> = r.lines().collect();
        let at = lines.iter().position(|l| *l == "Really offloaded (the output as it was opened, not the settings)").unwrap();
        assert_eq!(lines[at + 1], "Screen off, playing: offloaded 45 min 00 s of 1 h 00 min (75 %)");
        let head = lines.iter().position(|l| l.starts_with("Screen off, playing: ") && l.contains(" [")).unwrap();
        assert!(lines[head + 1].starts_with("    output: "));
        assert_eq!(lines[head + 2], "      (5 earlier events not kept)");
        assert_eq!(lines[head + 3], "      07:00:05 error: e5");
    }

    /// An offloaded stretch says how long the wake lock was held, how often the engine's thread woke and
    /// how often the platform asked for more, so a tester's report shows what offload cost the CPU.
    #[test]
    fn an_offloaded_stretch_says_the_wake_lock_held_and_the_engine_s_wakeups() {
        let mut t = Timeline { requests_at_start: DATA_REQUESTS.load(std::sync::atomic::Ordering::Relaxed), ..Timeline::default() };
        t.note(at(9, 0, 0), PerfNote::WakeLock { held: true }, None);
        t.note(at(9, 0, 5), PerfNote::WakeLock { held: false }, None);
        t.note(at(9, 1, 0), PerfNote::WakeLock { held: true }, None);
        for _ in 0..3 {
            count_data_request();
        }
        assert_eq!(t.kept(at(9, 0, 0), at(9, 0, 30), false), (5_000, 3), "the stretch under way only looks");
        assert_eq!(t.kept(at(9, 0, 0), at(9, 1, 10), true), (15_000, 3), "held from 9:01:00 on, still held");
        assert_eq!(t.kept(at(9, 1, 10), at(9, 1, 20), true), (10_000, 0), "the next stretch counts from where it began");
        let mut s = stretch("off-playing", 600_000);
        s.out = Some(PerfOutput { offloaded: true, ..output() });
        s.offloaded_ms = Some(600_000);
        s.wake_lock_ms = Some(12_000);
        s.engine_wakeups = Some(180);
        s.data_requests = Some(150);
        let back: PerfStretch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s, "kept with the stretch");
        let line = stretch_line(&s);
        assert!(
            line.contains("offloaded 10 min 00 s of 10 min 00 s (100 %) (wake lock held 12 s of 10 min 00 s (2 %), nori-engine 0.30 wakeups/s, the chip asked for more 0.25 times/s)"),
            "{line}"
        );
        let mut cpu = stretch("off-playing", 300_000);
        cpu.out = Some(output());
        cpu.offloaded_ms = Some(0);
        cpu.wake_lock_ms = Some(300_000);
        cpu.engine_wakeups = Some(40);
        cpu.data_requests = Some(0);
        let by_state = totals(&[s, cpu]);
        assert_eq!(
            by_state[0].offloaded().unwrap(),
            "offloaded 10 min 00 s of 15 min 00 s (67 %); wake lock held 12 s of 10 min 00 s (2 %), nori-engine 0.30 wakeups/s, the chip asked for more 0.25 times/s",
            "the wake lock and wakeups of the offloaded stretches only"
        );
    }

    #[test]
    fn the_offload_tag_says_wanted_not_given() {
        assert_eq!(offload_tag(true, None), "offload wanted");
        assert_eq!(offload_tag(false, Some("AutoMix is on")), "offload not wanted: AutoMix is on");
        assert_eq!(offload_tag(true, Some("a crossfade is set")), "offload not wanted: a crossfade is set", "the settings as they were win");
        assert!(offload_tag(false, None).starts_with("offload not wanted: the output"));
        assert_eq!(cfg_words("engine rust, eq off, offloaded"), "engine rust, eq off, offload wanted", "an older row's words");
        assert_eq!(cfg_words("engine rust, offload wanted"), "engine rust, offload wanted");
    }

    #[test]
    fn the_apps_own_lines_are_kept_with_the_latest_breaks_newest_first() {
        let c = Connection::open_in_memory().unwrap();
        for k in 0..BREAK_LOGS as i64 + 2 {
            keep_break(&c, 1_000 * k, &format!("silent: break {k}"), &format!("10:00:0{k}.000 nori: said {k}\n10:00:0{k}.500 nori: then {k}")).unwrap();
        }
        let kept = break_logs(&c).unwrap();
        assert_eq!(kept.iter().map(|k| k.0).collect::<Vec<_>>(), [4_000, 3_000, 2_000], "the latest, newest first");
        let s = break_log_section(&kept);
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with("The app's own lines as an invariant broke, ") && lines[0].ends_with("(2 lines): silent: break 4"), "{}", lines[0]);
        assert_eq!(lines[1..3], ["10:00:04.000 nori: said 4", "10:00:04.500 nori: then 4"]);
        assert!(break_log_section(&[]).is_empty(), "nothing when no break kept any");
    }

    #[test]
    fn the_report_ends_with_the_crashes_and_the_log() {
        let c = Connection::open_in_memory().unwrap();
        keep_crash(&c, "exception", 5_000, "java.lang.IllegalStateException: boom\n\tat A.b(A.kt:1)\n").unwrap();
        keep_crash(&c, "buffer", 1_000, "F DEBUG: signal 11").unwrap();
        keep_crash(&c, "buffer", 9_000, "F DEBUG: signal 11").unwrap();
        let kept = crashes(&c).unwrap();
        assert_eq!(
            kept.iter().map(|k| (k.kind.as_str(), k.at_ms)).collect::<Vec<_>>(),
            [("exception", 5_000), ("buffer", 1_000)],
            "the same crash keeps when it was first seen"
        );
        let log: String = (0..5_000).map(|i| format!("09-24 21:00:00.000 1 2 I nori: line {i}\n")).collect();
        let s = log_section(&PerfLogs { app: log, crash: String::new() }, &kept);
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with("Last uncaught exception, "), "{}", lines[0]);
        assert_eq!(lines[1], "java.lang.IllegalStateException: boom");
        assert!(lines[4].starts_with("Crash buffer as kept at "), "logcat has it no more: {}", lines[4]);
        let head = lines.iter().position(|l| l.starts_with("Log (this process")).unwrap();
        assert!(s.len() < LOG_CHARS + 1_000, "bounded");
        assert!(lines[head + 1].starts_with("09-24 21:00:00.000"), "from a line's start: {}", lines[head + 1]);
        assert_eq!(*lines.last().unwrap(), "09-24 21:00:00.000 1 2 I nori: line 4999", "the newest kept");
        let now = log_section(&PerfLogs { app: String::new(), crash: "F libc: Fatal signal 6\n".into() }, &kept);
        assert!(now.starts_with("Crash buffer (this and earlier runs of the app)\nF libc: Fatal signal 6\n"));
        assert!(!now.contains("as kept at"), "logcat's own copy is the one shown");
        assert!(now.ends_with("Log (this process, the last 0 lines)\n(empty)\n"));
        assert_eq!(tail("ab\ncd\nef", 4), "ef");
    }
}
