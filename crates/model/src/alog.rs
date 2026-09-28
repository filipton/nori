//! Logging to Android's log (logcat, tag `nori`) straight from Rust, so the core can say what it is
//! doing from any thread without calling back into Kotlin. Elsewhere (tests, a desktop app) it goes to
//! stderr. Only for events - never per buffer - since each line is formatted and copied once.
//!
//! The last [`KEPT`] lines are also kept in memory, each with the wall clock it was said at, whatever
//! logcat does with them: logcat's buffer is shared with the whole system and a codec's chatter turns it
//! over in minutes, so a report written after something broke would no longer find what the app said as
//! it broke. The perf build's invariant watch takes a copy of them the moment one breaks ([`recent`]).
//! Keeping one costs a lock and the line's copy, on a line that was formatted anyway.
//!
//! Once a client says where ([`alog_persist`]), each line is also appended to a file there, with its local
//! time: `nori.log`, moved to `nori.log.1` when it passes [`JOURNAL_BYTES`], so the two hold the last day or
//! so of what the app said, across the process ending. A problem heard once in hours (a gap in the music)
//! is found there afterwards, in a release build too ([`alog_journal`], the report a client copies). One
//! write per line, unbuffered, so a line said just before the process died is kept; lines are events only.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(target_os = "android")]
mod sys {
    use std::ffi::{c_char, c_int};
    #[link(name = "log")]
    extern "C" {
        pub fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }
}

/// How many of the latest lines are kept in memory.
pub const KEPT: usize = 500;

/// The latest lines, oldest first, each with when it was said (wall clock ms).
static LINES: Mutex<VecDeque<(i64, String)>> = Mutex::new(VecDeque::new());

/// How big `nori.log` grows before it becomes `nori.log.1` (the one before it going).
pub const JOURNAL_BYTES: u64 = 1 << 20;

/// The file the lines are appended to, once a client has said where.
struct Journal {
    dir: PathBuf,
    file: File,
    bytes: u64,
    /// Minutes east of UTC, for the lines' local time.
    offset_min: i64,
}

static JOURNAL: Mutex<Option<Journal>> = Mutex::new(None);

fn wall_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// `ms` since the epoch, `offset_min` east of UTC, as `2026-09-28 13:22:05.123`.
pub fn local_time(ms: i64, offset_min: i64) -> String {
    let ms = ms + offset_min * 60_000;
    let (days, of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let s = of_day / 1000;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}", s / 3600, s / 60 % 60, s % 60, of_day % 1000)
}

fn open_journal(dir: &std::path::Path) -> Option<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(dir.join("nori.log")).ok()?;
    let bytes = file.metadata().map_or(0, |m| m.len());
    Some((file, bytes))
}

/// The line appended to the journal, if there is one; moved on to a new file once this one is full.
fn journal(ms: i64, message: &str) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(journal) = j.as_mut() else { return };
    let line = format!("{} {message}\n", local_time(ms, journal.offset_min));
    if journal.bytes + line.len() as u64 > JOURNAL_BYTES {
        let _ = std::fs::rename(journal.dir.join("nori.log"), journal.dir.join("nori.log.1"));
        match open_journal(&journal.dir) {
            Some((file, bytes)) => (journal.file, journal.bytes) = (file, bytes),
            None => {
                *j = None;
                return;
            }
        }
    }
    if journal.file.write_all(line.as_bytes()).is_ok() {
        journal.bytes += line.len() as u64;
    }
}

/// A line kept in memory only, as said elsewhere (the app's own Kotlin, which writes to logcat itself).
pub fn keep(message: &str) {
    let ms = wall_ms();
    journal(ms, message);
    let line = (ms, message.to_string());
    // A panic while it was held leaves the lines as they were: the log must never stop the app.
    let mut kept = LINES.lock().unwrap_or_else(|e| e.into_inner());
    if kept.len() >= KEPT {
        kept.pop_front();
    }
    kept.push_back(line);
}

/// From now on every line is also appended to `nori.log` in `dir` (made if it is not there), with its
/// time `utc_offset_min` east of UTC; asked again, it moves there. False when the file cannot be opened.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn alog_persist(dir: String, utc_offset_min: i32) -> bool {
    let dir = PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&dir);
    let Some((file, bytes)) = open_journal(&dir) else { return false };
    *JOURNAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Journal { dir, file, bytes, offset_min: utc_offset_min as i64 });
    true
}

/// What the journal holds, oldest first (both files), or empty when there is none.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn alog_journal() -> String {
    let Some(dir) = JOURNAL.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|j| j.dir.clone()) else { return String::new() };
    let read = |name: &str| std::fs::read(dir.join(name)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    read("nori.log.1") + &read("nori.log")
}

/// The lines kept, oldest first, with when each was said (wall clock ms).
pub fn recent() -> Vec<(i64, String)> {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
}

/// One line at INFO under the app's tag.
pub fn info(message: &str) {
    keep(message);
    #[cfg(target_os = "android")]
    {
        const INFO: std::ffi::c_int = 4;
        // Interior NULs would end the line early; there are none in what the core logs, but never panic.
        if let Ok(text) = std::ffi::CString::new(message) {
            unsafe { sys::__android_log_write(INFO, c"nori".as_ptr(), text.as_ptr()) };
        }
    }
    #[cfg(not(target_os = "android"))]
    eprintln!("nori: {message}");
}

/// One line the app's Kotlin wrote to logcat under the `nori` tag, kept with the core's own so that a
/// copy of the latest lines has both.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn alog_keep(line: String) {
    keep(&line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_s_local_time_is_the_calendar_s() {
        // 2026-09-28 11:22:05.123 UTC, in CEST.
        assert_eq!(local_time(1_790_594_525_123, 120), "2026-09-28 13:22:05.123");
        assert_eq!(local_time(0, 0), "1970-01-01 00:00:00.000");
        assert_eq!(local_time(951_782_400_000, 0), "2000-02-29 00:00:00.000");
        assert_eq!(local_time(0, -60), "1969-12-31 23:00:00.000");
    }

    #[test]
    fn the_journal_keeps_the_lines_across_files_and_goes_on_in_a_new_one_when_full() {
        let dir = nori_testdir::TempDir::new("alog-journal");
        assert!(alog_persist(dir.to_string_lossy().into_owned(), 120));
        let long = "x".repeat(1000);
        for k in 0..2500 {
            keep(&format!("journal-test {k} {long}"));
        }
        let all = alog_journal();
        let mine: Vec<usize> = all.lines().filter_map(|l| l.split("journal-test ").nth(1)?.split(' ').next()?.parse().ok()).collect();
        assert_eq!(mine.last(), Some(&2499), "the newest line is there");
        assert!(mine.windows(2).all(|w| w[1] == w[0] + 1), "in order, across the two files");
        assert!(mine.len() > 1000 && !mine.contains(&0), "a full file's worth and more, the oldest gone: {}", mine.len());
        assert!(std::fs::metadata(dir.join("nori.log")).unwrap().len() <= JOURNAL_BYTES);
        assert!(all.lines().all(|l| l.as_bytes().get(4) == Some(&b'-') && l.as_bytes().get(10) == Some(&b' ')), "each with its time");
        *JOURNAL.lock().unwrap() = None;
    }

    #[test]
    fn the_latest_lines_are_kept_in_order_and_no_more_than_kept() {
        for k in 0..KEPT + 20 {
            info(&format!("alog-test line {k}"));
        }
        let mine: Vec<String> = recent().into_iter().map(|(_, l)| l).filter(|l| l.starts_with("alog-test line ")).collect();
        assert!(mine.len() <= KEPT);
        assert_eq!(mine.last().map(String::as_str), Some(format!("alog-test line {}", KEPT + 19).as_str()));
        // In order, and the oldest gone first.
        let numbers: Vec<usize> = mine.iter().map(|l| l.rsplit(' ').next().unwrap().parse().unwrap()).collect();
        assert!(numbers.windows(2).all(|w| w[1] == w[0] + 1), "in order: {numbers:?}");
        assert!(!mine.contains(&"alog-test line 0".to_string()), "the oldest went first");
    }
}
