//! The lyrics page's clock: prepared once per song (`nori_look::lyrics`), then asked every frame with
//! primitives in and one packed number out, allocating nothing. A clock is handed out as a handle, so a
//! platform across a language boundary can keep it; how a page looks otherwise is `nori_look`'s own.

use nori_look::lyrics::{Line, LyricClock, LyricTiming, Word};

// ---- lyrics -------------------------------------------------------------------------------------------------

/// Prepares a song's lyrics for timing (`nori_look::lyrics`), showing the moment `position_ms`. Once per
/// set of lyrics, so it takes the record as it is; the answer is a handle for `LyricsJni`, which the
/// platform frees with `LyricsJni.destroy`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn lyrics_clock(lyrics: nori_model::Lyrics, position_ms: i64) -> i64 {
    let lines = lyrics.lines.iter().map(timed);
    Box::into_raw(Box::new(LyricClock::with_offset(LyricTiming::new(lyrics.synced, lyrics.word_timed, lines), position_ms, lyrics.offset_ms))) as i64
}

fn timed(l: &nori_model::LyricLine) -> Line {
    let word = |w: &nori_model::LyricWord| Word { start_ms: w.start_ms, end_ms: w.end_ms, start: w.start, end: w.end };
    Line {
        start_ms: l.start_ms,
        end_ms: l.end_ms,
        len: l.text.encode_utf16().count() as u32,
        words: l.words.iter().map(word).collect(),
        backing_len: l.backing.encode_utf16().count() as u32,
        backing: l.backing_words.iter().map(word).collect(),
    }
}

/// What the timing needs of one set of lyrics read, under the key the record was given.
struct Kept {
    key: u64,
    synced: bool,
    word_timed: bool,
    offset_ms: i64,
    lines: Vec<Line>,
}

/// The last few sets of lyrics read. The page that shows lyrics shows the ones it was just handed, so a
/// handful is plenty; lyrics that have gone from here are handed over whole instead ([`lyrics_clock`]).
static KEPT: parking_lot::Mutex<Vec<Kept>> = parking_lot::Mutex::new(Vec::new());
const KEEP: usize = 4;
static NEXT_KEY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Keeps what the timing needs of `lyrics` as they are read and gives them the key to ask for it by, so
/// the lyrics page starts its clock with one number (`LyricsJni.kept`) instead of handing every line
/// back. Lyrics with no lines are not kept: there is nothing to hand back.
pub fn keep(lyrics: &mut nori_model::Lyrics) {
    if lyrics.lines.is_empty() {
        return;
    }
    let key = NEXT_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    lyrics.key = key;
    let kept = Kept { key, synced: lyrics.synced, word_timed: lyrics.word_timed, offset_ms: lyrics.offset_ms, lines: lyrics.lines.iter().map(timed).collect() };
    let mut all = KEPT.lock();
    if all.len() == KEEP {
        all.remove(0);
    }
    all.push(kept);
}

/// A clock on the lyrics read under `key`, showing the moment `position_ms`, as [`lyrics_clock`] makes
/// one; 0 when they are no longer kept.
pub fn kept_clock(key: u64, position_ms: i64) -> i64 {
    let all = KEPT.lock();
    let Some(k) = all.iter().find(|k| k.key == key) else { return 0 };
    Box::into_raw(Box::new(LyricClock::with_offset(LyricTiming::new(k.synced, k.word_timed, k.lines.iter().cloned()), position_ms, k.offset_ms))) as i64
}

/// The clock behind a handle from [`lyrics_clock`] or [`kept_clock`]; none for 0.
///
/// # Safety
/// `h` is 0 or a handle one of those made that has not been given to [`free_clock`].
pub unsafe fn clock<'a>(h: i64) -> Option<&'a LyricClock> {
    // SAFETY: the caller's promise: a live handle is a boxed clock.
    (h != 0).then(|| unsafe { &*(h as *const LyricClock) })
}

/// Lets a clock from [`lyrics_clock`] or [`kept_clock`] go.
///
/// # Safety
/// `h` is 0 or a live handle one of those made, and is not used again.
pub unsafe fn free_clock(h: i64) {
    if h != 0 {
        // SAFETY: the caller's promise: `h` is a boxed clock nobody else will free.
        drop(unsafe { Box::from_raw(h as *mut LyricClock) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_model::LyricLine;
use nori_model::Lyrics;
    use nori_look::lyrics::Step;

    #[test]
    fn a_clock_from_the_cores_lyrics_counts_utf16_and_frees() {
        let line = |start_ms, text: &str| LyricLine { start_ms, end_ms: start_ms + 2000, text: text.into(), words: vec![], translation: None, ..Default::default() };
        let h = lyrics_clock(Lyrics { synced: true, word_timed: false, lines: vec![line(1000, "Żółć 🎵"), line(4000, "x")], key: 0, offset_ms: 0 }, 0);
        let c = unsafe { clock(h) }.unwrap();
        assert!(!c.timing().sweeps());
        // A line without words is all lit once reached: seven UTF-16 units, the note being two.
        let s = Step::unpack(c.advance(1500, true, false, true).pack());
        assert_eq!((s.frame.active, s.frame.sung, s.redraw), (0, 7.0, true));
        unsafe { free_clock(h) };
        assert!(unsafe { clock(0) }.is_none());
    }

    #[test]
    fn lyrics_read_are_kept_for_a_clock_by_key() {
        let line = |start_ms, text: &str| LyricLine { start_ms, end_ms: start_ms + 2000, text: text.into(), words: vec![], translation: None, ..Default::default() };
        let mut l = Lyrics { synced: true, word_timed: false, lines: vec![line(1000, "Żółć 🎵"), line(4000, "x")], key: 0, offset_ms: 0 };
        keep(&mut l);
        assert_ne!(l.key, 0);
        let h = kept_clock(l.key, 0);
        let s = Step::unpack(unsafe { clock(h) }.unwrap().advance(1500, true, false, true).pack());
        assert_eq!((s.frame.active, s.frame.sung), (0, 7.0));
        // The last line ends where the lyrics say (4 s + 2 s), and then it is drawn as sung with the rest.
        let s = Step::unpack(unsafe { clock(h) }.unwrap().advance(6000, true, false, false).pack());
        assert_eq!(s.frame.active, 2);
        unsafe { free_clock(h) };
        let first = l.key;
        for _ in 0..KEEP {
            keep(&mut l);
        }
        assert_eq!(kept_clock(first, 0), 0, "only the last few are kept");
        let mut none = Lyrics::default();
        keep(&mut none);
        assert_eq!(none.key, 0);
    }
}
