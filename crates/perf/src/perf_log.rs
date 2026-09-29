//! Perf build recorder: one row per "stretch" (a period in one state: screen off playing, charging, ...),
//! kept for two weeks in the app database. The platform supplies counter readings at each stretch's
//! ends; this module computes the differences, the busiest threads, the audio output, network and
//! memory, collects a per-stretch event timeline ([`perf_note`]), and renders the Performance page and
//! the shared plain-text report (whose format tools and testers read).
//!
//! Only the perf build calls this; tables are created lazily, so other builds never have them.

use std::collections::HashMap;

use nori_settings::settings::PrefValue;
use nori_settings::settings_store;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::memory::{memory_line, PerfMemory};

/// Retention of stretches after they end.
const KEEP_MS: i64 = 14 * 24 * 3600 * 1000;

/// Shorter stretches are transitions between states and are not kept.
const SHORTEST_MS: i64 = 3_000;

/// State keys and display names, in page order.
const STATES: [(&str, &str); 7] = [
    ("off-playing", "Screen off, playing"),
    ("off-paused", "Screen off, paused"),
    ("on-playing-player", "Screen on, playing, player open"),
    ("on-playing-app", "Screen on, playing, other page"),
    ("on-playing-away", "Screen on, playing, another app"),
    ("on-paused", "Screen on, paused"),
    (CHARGING, "Charging"),
];

/// Excluded from battery figures.
const CHARGING: &str = "charging";

/// One recorded stretch, stored as JSON with the historical short keys. `uah`: charge used per the
/// battery counter (None if the phone has none); `pct`: battery level drop. Optional fields are None
/// (or empty) in rows recorded before they existed.
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
    /// The [`TOP_THREADS`] threads with most wakeups, most first.
    #[serde(rename = "th", default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<PerfThreadUse>,
    /// The audio output at the end; None without a player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<PerfOutput>,
    /// Network bytes received and sent, where the platform counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<i64>,
    /// Timeline events, oldest first (at most [`MOST_EVENTS`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ev: Vec<PerfEvent>,
    /// Number of oldest events dropped past [`MOST_EVENTS`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub evx: i64,
    /// Time an offloaded output was open, ms.
    #[serde(rename = "om", default, skip_serializing_if = "Option::is_none")]
    pub offloaded_ms: Option<i64>,
    /// Memory breakdown at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<PerfMemory>,
    /// Time the player's CPU wake lock was held, ms.
    #[serde(rename = "wl", default, skip_serializing_if = "Option::is_none")]
    pub wake_lock_ms: Option<i64>,
    /// Wakeups of the [`ENGINE_THREAD`], if alive at both ends.
    #[serde(rename = "ew", default, skip_serializing_if = "Option::is_none")]
    pub engine_wakeups: Option<i64>,
    /// Offloaded track `onDataRequest` callbacks (the platform's pace, which engine wakes follow).
    #[serde(rename = "dr", default, skip_serializing_if = "Option::is_none")]
    pub data_requests: Option<i64>,
}

/// The player engine's thread name.
const ENGINE_THREAD: &str = "nori-engine";

/// `onDataRequest` count since process start; a static atomic because it is bumped from the audio
/// callback, which must not take the timeline lock.
static DATA_REQUESTS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Counts one offloaded `onDataRequest`.
pub fn count_data_request() {
    DATA_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// Threads kept per stretch.
const TOP_THREADS: usize = 6;

/// One app thread's cumulative counters at a reading.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfThread {
    pub tid: i32,
    pub name: String,
    /// CPU time, user and system.
    pub cpu_ms: i64,
    /// Voluntary context switches (wakeups).
    pub switches: i64,
}

/// One thread's use over a stretch. `born`: started during it (its whole count is the stretch's).
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

/// The audio output as the platform reports it: requested vs granted buffer and performance mode,
/// offload, route, underruns. -1 for figures the platform does not give.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfOutput {
    /// The player that opened the track ("rust" or "exoplayer").
    #[serde(rename = "e")]
    pub engine: String,
    pub rate: i32,
    #[serde(rename = "ch")]
    pub channels: i32,
    /// `AudioFormat.ENCODING_*`.
    #[serde(rename = "enc")]
    pub encoding: i32,
    /// Requested buffer, bytes.
    #[serde(rename = "ask")]
    pub asked_bytes: i64,
    /// `getBufferSizeInFrames` and `getBufferCapacityInFrames`.
    #[serde(rename = "size")]
    pub size_frames: i64,
    #[serde(rename = "cap")]
    pub capacity_frames: i64,
    /// `AudioTrack.PERFORMANCE_MODE_*`, requested and applied.
    #[serde(rename = "pma")]
    pub mode_asked: i32,
    #[serde(rename = "pm")]
    pub mode: i32,
    /// `isOffloadedPlayback`.
    #[serde(rename = "off")]
    pub offloaded: bool,
    /// Route: `AudioDeviceInfo.TYPE_*` (0 unknown) and device name.
    #[serde(rename = "dt")]
    pub device_type: i32,
    #[serde(rename = "dn", default, skip_serializing_if = "String::is_empty")]
    pub device_name: String,
    /// `getUnderrunCount`.
    #[serde(rename = "ur")]
    pub underruns: i32,
    /// `getPlayState`: 1 stopped, 2 paused, 3 playing.
    #[serde(rename = "st")]
    pub play_state: i32,
    /// The player's reason for PCM instead of offload, or, when offloaded, what the chip leaves in
    /// (encoder delay/padding without gapless offload). Empty if not given.
    #[serde(rename = "wp", default, skip_serializing_if = "String::is_empty")]
    pub pcm_why: String,
}

/// One reading of the platform's counters.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfCounters {
    /// Clock including deep sleep (`elapsedRealtime`).
    pub elapsed_ms: i64,
    pub wall_ms: i64,
    /// Process CPU time, user + system.
    pub cpu_ms: i64,
    pub threads: Vec<PerfThread>,
    pub alloc_bytes: i64,
    pub gcs: i64,
    pub pss_kb: i64,
    /// Remaining battery charge, µAh.
    pub charge_uah: Option<i64>,
    pub capacity_pct: i32,
    /// Fuel gauge current (average if available), µA, sign varies by maker.
    pub gauge_ua: Option<i64>,
    /// Battery temperature, tenths of a degree.
    pub temp_deci: i32,
    /// Network bytes since boot.
    pub rx_bytes: Option<i64>,
    pub tx_bytes: Option<i64>,
    pub memory: Option<PerfMemory>,
}

/// Frames drawn over a stretch.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfFrames {
    pub frames: i64,
    pub janky: i64,
    pub worst_ns: i64,
}

/// One row of the Performance page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfFigures {
    pub title: String,
    pub detail: String,
    /// A stretch's timeline lines (foldable); empty for other rows.
    pub events: Vec<String>,
}

/// The Performance page's figures.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfPage {
    /// Per-state totals.
    pub totals: Vec<PerfFigures>,
    /// Frame totals; None before any frame.
    pub frames: Option<PerfFigures>,
    /// The current stretch.
    pub live: Option<PerfFigures>,
    /// Kept stretches, newest first.
    pub stretches: Vec<PerfFigures>,
}

/// Device and build, for the report header.
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

// Storage.

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

/// Stores a stretch that ended at `ended_ms` and prunes rows older than [`KEEP_MS`]. No-op before the
/// app database is open.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_log_add(ended_ms: i64, stretch: PerfStretch) {
    let Some(db) = settings_store::app_db() else { return };
    let Ok(row) = serde_json::to_string(&stretch) else { return };
    let written = add(&db.lock(), ended_ms, &row);
    if let Err(e) = written {
        nori_model::alog::info(&format!("perf log: could not write: {e}"));
    }
}

/// Stretches that ended at or after `since_ms`, oldest first; unreadable rows are skipped.
fn perf_log_rows(since_ms: i64) -> Vec<PerfStretch> {
    let Some(db) = settings_store::app_db() else { return Vec::new() };
    let read = rows(&db.lock(), since_ms);
    parsed(read.unwrap_or_default())
}

fn parsed(rows: Vec<String>) -> Vec<PerfStretch> {
    rows.iter().filter_map(|r| serde_json::from_str(r).ok()).collect()
}

/// "Start fresh": deletes all stretches, crashes, the self test and break logs, and resets the timeline.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_log_clear() {
    let Some(db) = settings_store::app_db() else { return };
    let c = db.lock();
    if table(&c).is_ok() {
        let _ = c.execute("DELETE FROM perf_stretches", []);
    }
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

// Measuring.

/// The state key: charging, then screen, then playback, then app position.
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

/// One-line summary of cost-relevant settings; a change ends the stretch. `engine`: the running
/// service's player (default "rust").
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

/// Builds the stretch between readings `a` and `b`. `offloaded`: offload was wanted at the end;
/// `output`: the output then. None when shorter than [`SHORTEST_MS`], unless `live` (the current one,
/// which only peeks at the timeline).
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
            // A dropped short stretch: its events carry over to the next one.
            let mut t = timeline();
            t.close(a.wall_ms, b.wall_ms, false);
            t.kept(a.wall_ms, b.wall_ms, true);
            t.why_at_start = Some(offload_reason());
        }
        return None;
    }
    let before: HashMap<i32, &PerfThread> = a.threads.iter().map(|t| (t.tid, t)).collect();
    // Only threads alive at both ends (an exited thread's count is gone).
    let wakeups = b.threads.iter().filter_map(|t| before.get(&t.tid).map(|m| t.switches - m.switches)).sum();
    let engine: Vec<i64> = b.threads.iter().filter(|t| t.name == ENGINE_THREAD).filter_map(|t| before.get(&t.tid).filter(|m| m.name == t.name).map(|m| t.switches - m.switches)).collect();
    let engine_wakeups = (!engine.is_empty()).then(|| engine.iter().sum());
    let gauge: Vec<i64> = [a.gauge_ua, b.gauge_ua].into_iter().flatten().collect();
    let gauge_ma = (!gauge.is_empty()).then(|| (gauge.iter().map(|g| g.abs()).sum::<i64>() / gauge.len() as i64) as f64 / 1000.0);
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

/// Top threads by wakeups, then CPU. A thread only in `after` (or a reused tid with another name) is
/// `born` and counted whole.
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

// Totals.

/// Sums of one state's stretches.
#[derive(Debug, Default)]
struct Totals {
    state: String,
    count: i64,
    ms: i64,
    cpu_ms: i64,
    wakeups: i64,
    alloc_bytes: i64,
    gcs: i64,
    /// Last value (a level, not a sum).
    pss_kb: i64,
    /// Charge used and the duration of stretches that measured it.
    uah: i64,
    uah_ms: i64,
    pct: i64,
    frames: i64,
    janky: i64,
    /// Offloaded time and the duration of stretches with an output that counted it.
    off_ms: i64,
    off_of_ms: i64,
    /// Wake lock, engine wakeups and data requests over offloaded stretches that counted them, and their
    /// duration.
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

    /// "offloaded 45 min 00 s of 1 h 00 min (75 %)[; awake figures]", where counted.
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

    /// The state's summary line (page and report).
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

/// Totals by state: [`STATES`] order, then unknown states in order of appearance; empty states omitted.
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

// Formatting.

/// Java's `String.format("%.{places}f")` in `Locale.ROOT`: rounds half up on the shortest decimal form
/// (0.15 -> "0.2"), unlike Rust's `{:.1}`.
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

/// "09-24 21:05" in local time.
fn when(wall_ms: i64) -> String {
    let local = wall_ms + nori_library::library::local_offset_s(wall_ms.div_euclid(1000)) * 1000;
    let (_, m, d, secs) = nori_library::smart::civil_from_ms(local);
    format!("{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs / 60 % 60)
}

/// "12.3 MB", "840 KB".
fn bytes(n: i64) -> String {
    if n.abs() >= 1024 * 1024 {
        format!("{} MB", fixed(n as f64 / 1024.0 / 1024.0, 1))
    } else {
        format!("{} KB", n / 1024)
    }
}

/// "threads by wakeups: nori-track 2.1/s 40 ms, ..."; new threads marked "(new)".
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

/// `AudioFormat.ENCODING_*` name and PCM sample size in bytes (None if not PCM).
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

/// `AudioTrack.PERFORMANCE_MODE_*` name.
fn mode(m: i32) -> &'static str {
    match m {
        0 => "none",
        1 => "low latency",
        2 => "power saving",
        _ => "unknown",
    }
}

/// The "output: ..." line.
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
    out.push_str(if o.offloaded { ", offload given" } else { ", PCM" });
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

/// Detail lines under a stretch: threads, output, memory.
fn why_lines(s: &PerfStretch) -> Vec<String> {
    threads_line(s).into_iter().chain(s.out.as_ref().map(output_line)).chain(s.mem.as_ref().map(memory_line)).collect()
}

/// A stretch's summary line.
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

/// Kept stretches plus the current one, oldest first.
fn all(kept: Vec<PerfStretch>, live: Option<PerfStretch>) -> Vec<PerfStretch> {
    let mut all = kept;
    all.extend(live);
    all
}

/// The Performance page, with `live` as the current stretch.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_page(live: Option<PerfStretch>) -> PerfPage {
    page(perf_log_rows(0), live)
}

/// Summary line plus detail lines.
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

/// The shared plain-text report. `calls`/`covers`: benchmark results (empty if not run); `logs`: logcat
/// at share time, appended with kept crashes.
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

/// Per-state totals as padded columns; "-" where not applicable.
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

/// The report's first section: invariant breaks, newest first.
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

/// Stores the self test result (replacing the previous one), shown near the report's top.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_selftest_keep(at_ms: i64, text: String) {
    let Some(db) = settings_store::app_db() else { return };
    let c = db.lock();
    let kept = selftest_table(&c).and_then(|_| c.execute("INSERT OR REPLACE INTO perf_selftest(id, at_ms, text) VALUES(1, ?1, ?2)", params![at_ms, text]));
    if let Err(e) = kept {
        nori_model::alog::info(&format!("perf log: could not keep the self test: {e}"));
    }
}

/// The stored self test result.
fn perf_selftest_kept() -> Option<String> {
    let db = settings_store::app_db()?;
    let c = db.lock();
    selftest_table(&c).ok()?;
    c.query_row("SELECT text FROM perf_selftest WHERE id = 1", [], |r| r.get(0)).ok()
}

// Timeline.

/// Events kept per stretch; older ones are dropped and counted.
const MOST_EVENTS: usize = 150;

/// Formats of upcoming songs (decoded ahead) kept until the song starts.
const FORMATS_AHEAD: usize = 4;

/// A timeline event: wall time, kind ("song", "settings", "engine", "output", "offload", "underruns",
/// "error", "tuning", "format", "invariant", ...) and text.
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

/// A song that started playing: server file info and data source. Unknown figures are 0 or empty.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfSong {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// Server-reported suffix, bit rate (kbps), sample rate, bit depth, channels.
    pub suffix: String,
    pub bit_rate: i32,
    pub sampling_rate: i32,
    pub bit_depth: i32,
    pub channels: i32,
    /// Played from a finished download.
    pub downloaded: bool,
    /// Stream cache key (`<id>:<quality>`, empty if none) and whether the entry is complete.
    pub cache_key: String,
    pub cached_whole: bool,
}

/// Decoder input format from the demuxer; -1 for unknown figures.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfFormat {
    /// Codec and container MIME types.
    pub codec: String,
    pub container: String,
    pub rate: i32,
    pub channels: i32,
    /// Bits per second.
    pub bitrate: i32,
    /// Gapless trim frames at start and end.
    pub delay: i32,
    pub padding: i32,
}

/// A platform event for the current stretch's timeline.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum PerfNote {
    /// A song started playing.
    Song { song: PerfSong },
    /// Decoder format for song `id`, possibly ahead of it playing.
    Format { id: String, format: PerfFormat },
    /// The player service started with `engine`, or ended (None).
    Engine { engine: Option<String> },
    /// Output `key` opened, or released (None).
    Output { key: i64, output: Option<PerfOutput> },
    /// Output `key`'s underrun count.
    Underruns { key: i64, count: i32 },
    /// Playback error.
    Error { message: String },
    /// Equalizer tuning mode (shallow buffer) on or off.
    Tuning { on: bool },
    /// A message from the offload path.
    Offload { detail: String },
    /// Wake lock acquired or released (timed, not listed).
    WakeLock { held: bool },
}

/// The current stretch's events plus the state needed to describe them.
#[derive(Default)]
struct Timeline {
    events: Vec<PerfEvent>,
    dropped: i64,
    /// Last seen settings; None before the first read.
    settings: Option<HashMap<String, PrefValue>>,
    /// Keys changed by the last settings event and their prior values, so a slider drag updates one
    /// event instead of adding many.
    run: Option<(Vec<String>, HashMap<String, PrefValue>)>,
    song: Option<String>,
    ahead: Vec<(String, PerfFormat)>,
    /// Open output key and whether it is offloaded.
    track: Option<(i64, bool)>,
    /// Last underrun reading: output key, count, time.
    underruns: Option<(i64, i32, i64)>,
    /// Start of the current offloaded period, and offloaded time accumulated this stretch.
    offloaded_from: Option<i64>,
    offloaded_ms: i64,
    /// Settings' offload block reason at the stretch start (outer None: not yet recorded).
    why_at_start: Option<Option<&'static str>>,
    /// Start of the current wake lock hold, and hold time accumulated this stretch.
    lock_from: Option<i64>,
    lock_ms: i64,
    /// [`DATA_REQUESTS`] at the stretch start.
    requests_at_start: i64,
}

/// Global because the FFI entry points that feed it carry no handle.
static TIMELINE: std::sync::LazyLock<std::sync::Mutex<Timeline>> = std::sync::LazyLock::new(Default::default);

fn timeline() -> std::sync::MutexGuard<'static, Timeline> {
    TIMELINE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Settings whose values never appear in a report (only "changed").
const PRIVATE: [&str; 4] = ["servers", "activeServerId", "paxSenixKey", "betterLyricsKey"];

impl Timeline {
    fn push(&mut self, wall_ms: i64, kind: &str, detail: String) {
        if self.events.len() >= MOST_EVENTS {
            self.events.remove(0);
            self.dropped += 1;
        }
        self.events.push(PerfEvent { wall_ms, kind: kind.into(), detail });
    }

    /// `settings_why`: the settings' offload block reason, used when leaving offload without a reason.
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

    /// Records an underrun reading; an increase is an event citing the previous reading's time.
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

    /// Records a settings change against the last seen settings (the first call only stores them).
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

    /// Ends the stretch `start..end`: returns its events and dropped count (empty unless `kept`; dropped
    /// stretches pass events on) and its offloaded time.
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

    /// Wake lock time in `start..end` and data requests since the stretch start; `close` resets both for
    /// the next stretch.
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

    /// Like `close` without modifying anything (for the live stretch).
    fn so_far(&self, start: i64, now: i64) -> (Vec<PerfEvent>, i64, i64) {
        let off = self.offloaded_ms + self.offloaded_from.map_or(0, |f| (now - f.max(start)).max(0));
        (self.events.clone(), self.dropped, off)
    }

    /// Clears the current stretch.
    fn forget(&mut self) {
        self.events.clear();
        self.dropped = 0;
        self.run = None;
        self.offloaded_ms = 0;
        self.lock_ms = 0;
        self.requests_at_start = DATA_REQUESTS.load(std::sync::atomic::Ordering::Relaxed);
    }
}

/// Adds a platform event to the current timeline.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_note(wall_ms: i64, note: PerfNote) {
    if matches!(note, PerfNote::Engine { .. }) {
        crate::invariants::engine_changed(wall_ms);
    }
    let why = offload_reason();
    timeline().note(wall_ms, note, why);
}

/// Records the current settings (a change becomes an event).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_note_settings(wall_ms: i64) {
    let Some(p) = settings_store::settings_current() else { return };
    timeline().settings(wall_ms, nori_settings::settings::save(&p));
}

/// Adds a core decision (e.g. chosen lyrics and score) to the timeline, when the watch is on.
pub fn note_core(kind: &str, line: &str) {
    if crate::invariants::on() {
        timeline().push(crate::invariants::wall_ms(), kind, line.chars().take(600).collect());
    }
}

/// Adds an invariant break (allowed to be long: it quotes the engine state).
pub(crate) fn note_invariant(wall_ms: i64, line: &str) {
    timeline().push(wall_ms, "invariant", line.chars().take(2_000).collect());
}

/// Adds an output self-report under `kind`.
pub(crate) fn note_output(wall_ms: i64, kind: &str, line: &str) {
    timeline().push(wall_ms, kind, line.chars().take(400).collect());
}

/// Event lines since `since_ms` across stored stretches and the current one (for the self test).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_events_since(since_ms: i64) -> Vec<String> {
    let mut events: Vec<PerfEvent> = perf_log_rows(since_ms).into_iter().flat_map(|s| s.ev).collect();
    events.extend(timeline().events.iter().cloned());
    events.retain(|e| e.wall_ms >= since_ms);
    events.sort_by_key(|e| e.wall_ms);
    events.dedup();
    events.iter().map(|e| format!("{} {}: {}", clock(e.wall_ms), e.kind, e.detail)).collect()
}

/// Why the settings block offload (`nori_player::policy::offload_blocked`); None if allowed or unknown.
pub(crate) fn offload_reason() -> Option<&'static str> {
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

/// The cfg tag's offload part: whether offload was wanted (not whether it was granted).
fn offload_tag(wanted: bool, settings_why: Option<&str>) -> String {
    match settings_why {
        Some(why) => format!("offload not wanted: {why}"),
        None if wanted => "offload wanted".into(),
        None => "offload not wanted: the output (something USB, or a track it refused)".into(),
    }
}

/// A stored cfg tag with legacy offload wording translated.
fn cfg_words(cfg: &str) -> String {
    if let Some(head) = cfg.strip_suffix(", offloaded") {
        format!("{head}, offload wanted")
    } else if let Some(head) = cfg.strip_suffix(", on the CPU") {
        format!("{head}, offload not wanted")
    } else {
        cfg.to_string()
    }
}

/// "wake lock held 12 s of 10 min 00 s (2 %), nori-engine 0.30 wakeups/s, the chip asked for more 0.25
/// times/s".
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

/// "eqEnabled off → on", or "key changed" for private or long values.
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

/// "21:05:12" in local time.
pub(crate) fn clock(wall_ms: i64) -> String {
    let local = wall_ms + nori_library::library::local_offset_s(wall_ms.div_euclid(1000)) * 1000;
    let secs = local.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Timeline lines: "21:05:12 song: ...".
fn event_lines(s: &PerfStretch) -> Vec<String> {
    let dropped = (s.evx > 0).then(|| format!("({} earlier events not kept)", s.evx));
    dropped.into_iter().chain(s.ev.iter().map(|e| format!("{} {}: {}", clock(e.wall_ms), e.kind, e.detail))).collect()
}

// Logs and crashes.

/// Logcat text read on demand: this process's lines and the crash buffer (all runs). Either may be empty
/// or an error message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PerfLogs {
    pub app: String,
    pub crash: String,
}

/// Report size limits in characters: the log's tail and a crash's head.
const LOG_CHARS: usize = 60_000;
const CRASH_CHARS: usize = 16_000;

/// A crash stored in the database to outlive logcat: `kind` "exception" (uncaught exception handler)
/// or "buffer" (crash buffer as last seen).
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeptCrash {
    kind: String,
    at_ms: i64,
    text: String,
}

fn crash_table(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS perf_crashes(kind TEXT PRIMARY KEY, at_ms INTEGER NOT NULL, text TEXT NOT NULL)")
}

/// Stores the last crash of `kind`; identical text keeps its first-seen time.
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

/// Stores a crash synchronously (the process may be dying). Ignored for empty `text` or before the
/// database is open.
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

/// The log section, as at the end of the report.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn perf_log_text(logs: PerfLogs) -> String {
    log_section(&logs, &perf_crashes_kept())
}

// App log lines at invariant breaks.

/// Number of latest breaks that keep their log lines.
const BREAK_LOGS: usize = 3;

fn break_log_table(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS perf_break_logs(at_ms INTEGER NOT NULL, line TEXT NOT NULL, text TEXT NOT NULL)")
}

/// "21:05:12.345".
fn clock_ms(wall_ms: i64) -> String {
    format!("{}.{:03}", clock(wall_ms), wall_ms.rem_euclid(1000))
}

/// Stores the app's recent log lines (`nori_model::alog`) with break `line`, since logcat turns over in
/// minutes. Lines are copied now; the database write runs on its own thread because breaks can be seen
/// on audio threads. Only the latest [`BREAK_LOGS`] are kept.
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

/// The break log section, newest first; empty when none kept.
fn break_log_section(kept: &[(i64, String, String)]) -> String {
    let mut out = String::new();
    for (at, line, text) in kept {
        out.push_str(&format!("The app's own lines as an invariant broke, {} ({} lines): {line}\n", clock_ms(*at), text.lines().count()));
        out.push_str(if text.is_empty() { "(none)" } else { text });
        out.push_str("\n\n");
    }
    out
}

/// The last `chars` characters of `text`, starting at a line boundary.
fn tail(text: &str, chars: usize) -> &str {
    let n = text.chars().count();
    if n <= chars {
        return text;
    }
    let from = text.char_indices().nth(n - chars).map_or(0, |(i, _)| i);
    let cut = &text[from..];
    cut.find('\n').map_or(cut, |i| &cut[i + 1..])
}

/// The report's last part: crashes first, then the log.
fn log_section(logs: &PerfLogs, kept: &[KeptCrash]) -> String {
    let mut out = String::new();
    let buffer = logs.crash.trim();
    if !buffer.is_empty() {
        out.push_str(&format!("Crash buffer (this and earlier runs of the app)\n{}\n\n", tail(buffer, CRASH_CHARS).trim_end()));
    }
    for k in kept {
        match k.kind.as_str() {
            "exception" => out.push_str(&format!("Last uncaught exception, {}\n{}\n\n", when(k.at_ms), k.text.trim_end())),
            // The stored buffer, only when logcat no longer has it.
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

    /// Java `String.format` output (OpenJDK 21, root locale): value bits, places, plus sign, result.
    const JAVA_FIXED: &[(u64, i32, bool, &str)] = include!("../testdata/java_fixed.in");

    #[test]
    fn fixed_matches_java() {
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
    fn rows_ordered_and_pruned() {
        let c = Connection::open_in_memory().unwrap();
        assert!(rows(&c, 0).unwrap().is_empty(), "no table yet");
        add(&c, 1_000, "a").unwrap();
        add(&c, 2_000, "b").unwrap();
        assert_eq!(rows(&c, 0).unwrap(), vec!["a", "b"]);
        assert_eq!(rows(&c, 1_500).unwrap(), vec!["b"]);
        // A row past KEEP_MS prunes the older ones.
        add(&c, 2_001 + KEEP_MS, "c").unwrap();
        assert_eq!(rows(&c, 0).unwrap(), vec!["c"]);
    }

    #[test]
    fn legacy_row_round_trips() {
        // A row as the former Kotlin recorder wrote it.
        let old = r#"{"s":"off-playing","t0":1700000000000,"ms":600000,"cpu":1200,"wk":3000,"al":2048,"gc":1,"pss":150000,"uah":42000,"pct":1,"gma":152.5,"tmin":301,"tmax":305,"fr":0,"jk":0,"worst":41.5,"cfg":"engine exoplayer, on the CPU"}"#;
        let s = &parsed(vec![old.into(), "not a stretch".into()])[..];
        assert_eq!(s.len(), 1, "unreadable row skipped");
        assert_eq!((s[0].state.as_str(), s[0].uah, s[0].gauge_ma, s[0].worst_ms), ("off-playing", Some(42_000), Some(152.5), 41.5));
        let back: serde_json::Value = serde_json::from_str(&serde_json::to_string(&s[0]).unwrap()).unwrap();
        assert_eq!(back, serde_json::from_str::<serde_json::Value>(old).unwrap());
        let none = PerfStretch { uah: None, gauge_ma: None, ..s[0].clone() };
        assert!(!serde_json::to_string(&none).unwrap().contains("uah"));
        let why = PerfStretch {
            threads: vec![PerfThreadUse { name: "nori-track".into(), cpu_ms: 3, wakeups: 90, born: true }],
            out: Some(output()),
            rx: Some(1),
            tx: Some(0),
            ..s[0].clone()
        };
        assert_eq!(parsed(vec![serde_json::to_string(&why).unwrap()]), [why]);
    }

    #[test]
    fn state_priority() {
        assert_eq!(perf_state(true, false, true, false, false), "charging");
        assert_eq!(perf_state(false, false, true, true, true), "off-playing");
        assert_eq!(perf_state(false, false, false, true, false), "off-paused");
        assert_eq!(perf_state(false, true, false, true, true), "on-paused");
        assert_eq!(perf_state(false, true, true, false, true), "on-playing-away");
        assert_eq!(perf_state(false, true, true, true, true), "on-playing-player");
        assert_eq!(perf_state(false, true, true, true, false), "on-playing-app");
        assert_eq!(state_name("on-playing-away"), "Screen on, playing, another app");
        assert_eq!(state_name("new"), "new", "unknown key shown as is");
    }

    #[test]
    fn config_line() {
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
        assert!(config(Some("other".into()), &p).starts_with("engine other, "));
    }

    #[test]
    fn stretch_from_two_readings() {
        let a = counters(10_000, &[(1, 100), (2, 50), (3, 7)]);
        let b = counters(610_000, &[(1, 400), (2, 60), (4, 9)]);
        let drawn = PerfFrames { frames: 120, janky: 3, worst_ns: 41_500_000 };
        let s = perf_stretch("off-playing".into(), "engine rust".into(), a.clone(), b.clone(), drawn, true, Some(output()), false).unwrap();
        assert_eq!((s.ms, s.start_wall, s.cpu_ms), (600_000, 11_000, 12_000));
        assert_eq!(s.wakeups, 310, "threads alive at both ends only");
        assert_eq!((s.uah, s.pct, s.pss_kb), (Some(60_000), 1, 106_100));
        // Gauge readings averaged in whole µA, then mA.
        assert_eq!(s.gauge_ma, Some(((150_010 + 150_610) / 2) as f64 / 1000.0));
        assert_eq!((s.temp_min, s.temp_max, s.frames, s.janky, s.worst_ms), (300, 310, 120, 3, 41.5));
        assert_eq!(s.cfg, "engine rust, offload wanted");
        assert_eq!(s.offloaded_ms, Some(0));
        assert_eq!((s.rx, s.tx), (Some(60_000_000), Some(600_000)));
        assert_eq!(s.out, Some(output()));
        let blink = counters(12_000, &[]);
        assert_eq!(perf_stretch("x".into(), String::new(), a.clone(), blink.clone(), drawn, false, None, false), None, "too short");
        assert!(perf_stretch("x".into(), String::new(), a, blink, drawn, false, None, true).is_some(), "live is always shown");
    }

    #[test]
    fn busiest_threads() {
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
        assert_eq!(s.wakeups, 60 + 1_200 + 35);
        let names: Vec<(&str, i64, i64, bool)> = s.threads.iter().map(|t| (t.name.as_str(), t.wakeups, t.cpu_ms, t.born)).collect();
        assert_eq!(
            names,
            [("nori-track", 1_200, 30, false), ("nori-load", 600, 90, true), ("main", 60, 20, false), ("binder:1_3", 40, 3, true), ("c", 1, 2, true), ("a", 1, 1, true)],
            "sorted; new and reused-tid threads counted whole; idle threads omitted"
        );
        assert_eq!(
            threads_line(&s).unwrap(),
            "threads by wakeups: nori-track 20.0/s 30 ms, nori-load 10.0/s 90 ms (new), main 1.0/s 20 ms, binder:1_3 0.7/s 3 ms (new), c 0.0/s 2 ms (new), a 0.0/s 1 ms (new)"
        );
    }

    #[test]
    fn output_line_format() {
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
        let why = PerfOutput { pcm_why: "MP3 with an encoder delay of 576 and padding of 1000 needs gapless offload, which the output does not do".into(), ..output() };
        assert!(
            output_line(&why).contains(", PCM (MP3 with an encoder delay of 576 and padding of 1000 needs gapless offload, which the output does not do), to Phone speaker"),
            "{}",
            output_line(&why)
        );
    }

    #[test]
    fn totals_by_state_in_page_order() {
        let mut playing = stretch("off-playing", 3_600_000);
        playing.uah = Some(40_000);
        let t = totals(&[stretch("on-paused", 60_000), playing.clone(), stretch("new-state", 5_000), playing, stretch("charging", 90_000)]);
        let states: Vec<&str> = t.iter().map(|t| t.state.as_str()).collect();
        assert_eq!(states, ["off-playing", "on-paused", "charging", "new-state"]);
        assert_eq!(t[0].line(), "2 h 00 min, CPU 1.00 %, 10.0 wakeups/s, 60 KB/min allocated, 4 GCs, PSS 150 MB, 80.0 mAh (40.0 mAh/h)");
        assert_eq!(t[1].line(), "1 min 00 s, CPU 1.00 %, 10.0 wakeups/s, 60 KB/min allocated, 2 GCs, PSS 150 MB, 3 % (180.00 %/h)");
        assert_eq!(t[2].line(), "1 min 30 s, CPU 1.00 %, 10.0 wakeups/s, 60 KB/min allocated, 2 GCs, PSS 150 MB", "no battery figures while charging");
    }

    #[test]
    fn stretch_line_format() {
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
    }

    #[test]
    fn page_frames_and_order() {
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
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[1], "threads by wakeups: nori-track 24.0/s 12 ms");
        assert!(lines[2].starts_with("output: rust, 44100 Hz"));
    }

    #[test]
    fn stretch_keeps_end_memory() {
        let a = counters(0, &[]);
        let mut b = counters(60_000, &[]);
        b.memory = Some(PerfMemory { java_kb: 20 * 1024, native_kb: 80 * 1024, covers: 3, ..Default::default() });
        let s = perf_stretch("off-playing".into(), "engine rust".into(), a, b, PerfFrames { frames: 0, janky: 0, worst_ns: 0 }, false, None, true).unwrap();
        assert_eq!(s.mem.as_ref().map(|m| m.native_kb), Some(80 * 1024));
        let lines: Vec<String> = page(vec![s.clone()], None).stretches[0].detail.lines().map(String::from).collect();
        assert!(lines.last().unwrap().starts_with("memory: PSS 100 MB = Java 20, native 80,"), "{lines:?}");
        let back: PerfStretch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.mem, s.mem);
    }

    #[test]
    fn report_layout() {
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
        assert_eq!(lines[6], "Invariant breaks: none recorded");
        assert_eq!(lines[8], "By state");
        assert_eq!(lines[9], "state                      time  CPU %  wakeups/s  KB/min  GCs  PSS MB   mAh  mAh/h   %/h  frames  janky %");
        assert_eq!(lines[10], "Screen off, playing  1 h 00 min   1.00       10.0      60    2     150  40.0   40.0  3.00       -        -");
        assert_eq!(lines[11], "Charging             1 min 30 s   1.00       10.0      60    2     150     -      -     -       -        -");
        assert_eq!(lines[13], "Stretches, newest first");
        assert!(lines[14].starts_with("Charging: ") && lines[14].contains(", network 3.3 MB in, 20 KB out"), "{}", lines[14]);
        assert!(lines[15].starts_with("    output: rust, "), "{}", lines[15]);
        assert!(lines[16].starts_with("Screen off, playing: "));
        assert_eq!(&lines[17..], ["", "Cover benchmark: 12 ns"], "benchmarks not run are omitted");
    }

    #[test]
    fn report_starts_with_breaks_and_self_test() {
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
    fn when_uses_local_time() {
        let offset = nori_library::library::local_offset_s(0) * 1000;
        assert_eq!(when(-offset), "01-01 00:00");
        assert_eq!(when(-offset + 86_400_000 * 31 + 3_600_000 * 13 + 60_000 * 7), "02-01 13:07");
    }

    /// Wall ms reading as local `h:m:s` on 1970-01-01.
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
    fn song_events_describe_file_and_source() {
        let mut t = Timeline::default();
        // A format read ahead waits for its song.
        t.note(at(21, 0, 0), PerfNote::Format { id: "b".into(), format: mp3() }, None);
        t.note(at(21, 0, 1), PerfNote::Song { song: PerfSong { downloaded: true, ..song("a") } }, None);
        t.note(at(21, 3, 0), PerfNote::Song { song: PerfSong { cache_key: "b:320mp3".into(), cached_whole: true, ..song("b") } }, None);
        // A format for the current song is its own event.
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
    fn offload_time_from_output_events() {
        let mut t = Timeline::default();
        let offloaded = PerfOutput { offloaded: true, encoding: 9, ..output() };
        t.note(at(10, 0, 0), PerfNote::Engine { engine: Some("exoplayer".into()) }, None);
        t.note(at(10, 0, 1), PerfNote::Output { key: 1, output: Some(offloaded.clone()) }, None);
        // Another offloaded output is not a new offload entry.
        t.note(at(10, 5, 0), PerfNote::Output { key: 2, output: Some(offloaded) }, None);
        t.note(at(10, 10, 1), PerfNote::Output { key: 3, output: Some(output()) }, Some("a crossfade is set"));
        let (ev, dropped, off) = t.close(at(10, 0, 0), at(10, 20, 0), true);
        assert_eq!((dropped, off), (0, 10 * 60_000));
        let said: Vec<String> = ev.iter().map(|e| format!("{} {}: {}", clock(e.wall_ms), e.kind, e.detail)).collect();
        assert_eq!(said[0], "10:00:00 engine: the player service started with the exoplayer engine");
        assert!(said[1].starts_with("10:00:01 output: opened: rust, 44100 Hz stereo MP3,") && said[1].contains(", offload given,"), "{}", said[1]);
        assert_eq!(said[2], "10:00:01 offload: entered: the audio chip decodes");
        assert!(said[3].starts_with("10:05:00 output: reopened: "));
        assert!(said[4].starts_with("10:10:01 output: reopened: rust, 44100 Hz stereo 16-bit") && said[4].contains(", PCM,"), "{}", said[4]);
        assert_eq!(said[5], "10:10:01 offload: left: a crossfade is set", "settings reason as fallback");
        assert_eq!(ev.len(), 6);
        assert!(t.events.is_empty());

        // Offload spanning stretch ends: each counts its own part.
        t.note(at(11, 0, 0), PerfNote::Output { key: 4, output: Some(PerfOutput { offloaded: true, ..output() }) }, None);
        assert_eq!(t.close(at(10, 50, 0), at(11, 30, 0), true).2, 30 * 60_000);
        assert_eq!(t.so_far(at(11, 30, 0), at(11, 45, 0)).2, 15 * 60_000);
        // A dropped stretch passes its events on, not its time.
        t.note(at(11, 50, 0), PerfNote::Tuning { on: true }, None);
        assert_eq!(t.close(at(11, 30, 0), at(11, 50, 1), false), (Vec::new(), 0, 20 * 60_000 + 1_000));
        t.note(at(11, 51, 0), PerfNote::Output { key: 4, output: None }, None);
        let (ev, _, off) = t.close(at(11, 50, 1), at(12, 0, 0), true);
        assert_eq!(off, 59_000);
        assert_eq!(ev.iter().map(|e| e.detail.as_str()).collect::<Vec<_>>(), ["on: the equalizer screen is open, the output takes a shallow buffer", "let go"]);
        // The player's reason wins over the settings'.
        let mut t = Timeline::default();
        t.note(0, PerfNote::Output { key: 1, output: Some(PerfOutput { offloaded: true, ..output() }) }, None);
        let flac = PerfOutput { pcm_why: "FLAC is not decoded by this output".into(), ..output() };
        t.note(1, PerfNote::Output { key: 2, output: Some(flac) }, Some("AutoMix is on"));
        assert_eq!(t.events.last().unwrap().detail, "left: FLAC is not decoded by this output");
        t.note(2, PerfNote::Offload { detail: "a ended by the play head".into() }, None);
        let e = t.events.last().unwrap();
        assert_eq!((e.kind.as_str(), e.detail.as_str()), ("offload", "a ended by the play head"));
    }

    #[test]
    fn underrun_increases_are_events() {
        let mut t = Timeline::default();
        t.underruns(at(8, 0, 0), 7, 0);
        t.underruns(at(8, 4, 0), 7, 0);
        t.underruns(at(8, 7, 30), 7, 3);
        t.underruns(at(8, 9, 0), 7, 3);
        // A new output counts from zero.
        t.underruns(at(8, 12, 0), 8, 1);
        assert_eq!(
            lines(&t),
            ["08:07:30 underruns: 3 more, 3 on this output, since 08:04:00", "08:12:00 underruns: 1 more, 1 on this output, since the output opened"]
        );
    }

    #[test]
    fn settings_changes_and_drag_merging() {
        use nori_settings::settings::{save, StoredPrefs};
        let mut t = Timeline::default();
        let p = StoredPrefs::default();
        t.settings(at(9, 0, 0), save(&p));
        assert!(t.events.is_empty(), "first read is the baseline");
        let eq = StoredPrefs { eq_enabled: !p.eq_enabled, crossfade_sec: p.crossfade_sec + 6, ..p.clone() };
        t.settings(at(9, 1, 0), save(&eq));
        let on = |b: bool| if b { "on" } else { "off" };
        assert_eq!(
            lines(&t),
            [format!("09:01:00 settings: crossfadeSec {} → {}, eqEnabled {} → {}", p.crossfade_sec, eq.crossfade_sec, on(p.eq_enabled), on(!p.eq_enabled))]
        );
        // A slider drag is one event.
        for (i, v) in [0.1f32, 0.2, 0.3].into_iter().enumerate() {
            t.settings(at(9, 2, i as i64), save(&StoredPrefs { balance: v, ..eq.clone() }));
        }
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.events[1].detail, format!("balance {} → 0.30 (last at 09:02:02)", fixed(p.balance as f64, 2)));
        t.settings(at(9, 3, 0), save(&StoredPrefs { paxsenix_key: "secret".into(), balance: 0.3, ..eq }));
        assert_eq!(t.events[2].detail, "paxSenixKey changed", "private value hidden");
    }

    #[test]
    fn timeline_bounded_and_printed() {
        let mut t = Timeline::default();
        for i in 0..MOST_EVENTS as i64 + 5 {
            t.note(at(7, 0, i), PerfNote::Error { message: format!("e{i}") }, None);
        }
        let (ev, dropped, _) = t.close(at(7, 0, 0), at(8, 0, 0), true);
        assert_eq!((ev.len(), dropped, ev[0].detail.as_str()), (MOST_EVENTS, 5, "e5"));
        let mut s = stretch("off-playing", 3_600_000);
        s.ev = ev;
        s.evx = dropped;
        s.out = Some(PerfOutput { offloaded: true, ..output() });
        s.offloaded_ms = Some(2_700_000);
        let back: PerfStretch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let printed = event_lines(&s);
        assert_eq!(printed[0], "(5 earlier events not kept)");
        assert_eq!(printed[1], "07:00:05 error: e5");
        assert!(stretch_line(&s).contains(", offloaded 45 min 00 s of 1 h 00 min (75 %) [engine exoplayer, offload not wanted]"), "{}", stretch_line(&s));
        let p = page(vec![s.clone()], None);
        assert_eq!(p.stretches[0].events.len(), MOST_EVENTS + 1);
        assert!(p.totals[0].detail.ends_with(", offloaded 45 min 00 s of 1 h 00 min (75 %)"), "{}", p.totals[0].detail);
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

    #[test]
    fn offloaded_stretch_reports_wake_lock_and_engine_wakeups() {
        let mut t = Timeline { requests_at_start: DATA_REQUESTS.load(std::sync::atomic::Ordering::Relaxed), ..Timeline::default() };
        t.note(at(9, 0, 0), PerfNote::WakeLock { held: true }, None);
        t.note(at(9, 0, 5), PerfNote::WakeLock { held: false }, None);
        t.note(at(9, 1, 0), PerfNote::WakeLock { held: true }, None);
        for _ in 0..3 {
            count_data_request();
        }
        assert_eq!(t.kept(at(9, 0, 0), at(9, 0, 30), false), (5_000, 3), "peek");
        assert_eq!(t.kept(at(9, 0, 0), at(9, 1, 10), true), (15_000, 3), "still held");
        assert_eq!(t.kept(at(9, 1, 10), at(9, 1, 20), true), (10_000, 0), "next stretch starts fresh");
        let mut s = stretch("off-playing", 600_000);
        s.out = Some(PerfOutput { offloaded: true, ..output() });
        s.offloaded_ms = Some(600_000);
        s.wake_lock_ms = Some(12_000);
        s.engine_wakeups = Some(180);
        s.data_requests = Some(150);
        let back: PerfStretch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
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
            "awake figures from offloaded stretches only"
        );
    }

    #[test]
    fn offload_tag_and_legacy_cfg() {
        assert_eq!(offload_tag(true, None), "offload wanted");
        assert_eq!(offload_tag(false, Some("AutoMix is on")), "offload not wanted: AutoMix is on");
        assert_eq!(offload_tag(true, Some("a crossfade is set")), "offload not wanted: a crossfade is set");
        assert!(offload_tag(false, None).starts_with("offload not wanted: the output"));
        assert_eq!(cfg_words("engine rust, eq off, offloaded"), "engine rust, eq off, offload wanted");
        assert_eq!(cfg_words("engine rust, offload wanted"), "engine rust, offload wanted");
    }

    #[test]
    fn break_logs_keep_latest() {
        let c = Connection::open_in_memory().unwrap();
        for k in 0..BREAK_LOGS as i64 + 2 {
            keep_break(&c, 1_000 * k, &format!("silent: break {k}"), &format!("10:00:0{k}.000 nori: said {k}\n10:00:0{k}.500 nori: then {k}")).unwrap();
        }
        let kept = break_logs(&c).unwrap();
        assert_eq!(kept.iter().map(|k| k.0).collect::<Vec<_>>(), [4_000, 3_000, 2_000]);
        let s = break_log_section(&kept);
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with("The app's own lines as an invariant broke, ") && lines[0].ends_with("(2 lines): silent: break 4"), "{}", lines[0]);
        assert_eq!(lines[1..3], ["10:00:04.000 nori: said 4", "10:00:04.500 nori: then 4"]);
        assert!(break_log_section(&[]).is_empty());
    }

    #[test]
    fn log_section_crashes_then_log() {
        let c = Connection::open_in_memory().unwrap();
        keep_crash(&c, "exception", 5_000, "java.lang.IllegalStateException: boom\n\tat A.b(A.kt:1)\n").unwrap();
        keep_crash(&c, "buffer", 1_000, "F DEBUG: signal 11").unwrap();
        keep_crash(&c, "buffer", 9_000, "F DEBUG: signal 11").unwrap();
        let kept = crashes(&c).unwrap();
        assert_eq!(
            kept.iter().map(|k| (k.kind.as_str(), k.at_ms)).collect::<Vec<_>>(),
            [("exception", 5_000), ("buffer", 1_000)],
            "identical crash keeps first-seen time"
        );
        let log: String = (0..5_000).map(|i| format!("09-24 21:00:00.000 1 2 I nori: line {i}\n")).collect();
        let s = log_section(&PerfLogs { app: log, crash: String::new() }, &kept);
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with("Last uncaught exception, "), "{}", lines[0]);
        assert_eq!(lines[1], "java.lang.IllegalStateException: boom");
        assert!(lines[4].starts_with("Crash buffer as kept at "), "{}", lines[4]);
        let head = lines.iter().position(|l| l.starts_with("Log (this process")).unwrap();
        assert!(s.len() < LOG_CHARS + 1_000);
        assert!(lines[head + 1].starts_with("09-24 21:00:00.000"), "{}", lines[head + 1]);
        assert_eq!(*lines.last().unwrap(), "09-24 21:00:00.000 1 2 I nori: line 4999");
        let now = log_section(&PerfLogs { app: String::new(), crash: "F libc: Fatal signal 6\n".into() }, &kept);
        assert!(now.starts_with("Crash buffer (this and earlier runs of the app)\nF libc: Fatal signal 6\n"));
        assert!(!now.contains("as kept at"), "logcat copy preferred");
        assert!(now.ends_with("Log (this process, the last 0 lines)\n(empty)\n"));
        assert_eq!(tail("ab\ncd\nef", 4), "ef");
    }
}
