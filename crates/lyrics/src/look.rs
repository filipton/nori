//! The lyrics page's clock (`nori_look::lyrics`), handed out as a raw handle so a platform across a
//! language boundary can hold it.

use nori_look::lyrics::{Line, LyricClock, LyricTiming, Word};
use parking_lot::Mutex;

/// A clock on `lyrics` at `position_ms`, as a handle the platform frees with `LyricsJni.destroy`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn lyrics_clock(lyrics: nori_model::Lyrics, position_ms: i64) -> i64 {
    Box::into_raw(Box::new(clock_on(&lyrics, position_ms))) as i64
}

/// A clock on `lyrics` at `position_ms`.
pub fn clock_on(lyrics: &nori_model::Lyrics, position_ms: i64) -> LyricClock {
    LyricClock::with_offset(LyricTiming::new(lyrics.synced, lyrics.word_timed, lyrics.lines.iter().map(timed)), position_ms, lyrics.offset_ms)
}

fn new_clock(timing: LyricTiming, position_ms: i64, offset_ms: i64) -> i64 {
    Box::into_raw(Box::new(LyricClock::with_offset(timing, position_ms, offset_ms))) as i64
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

/// The timing of one set of lyrics, under the key the record was given.
struct Kept {
    key: u64,
    synced: bool,
    word_timed: bool,
    offset_ms: i64,
    lines: Vec<Line>,
}

/// The last few sets of lyrics read, so the page starts a clock from a key instead of every line.
struct KeptLyrics {
    kept: Mutex<(u64, Vec<Kept>)>,
}

/// How many sets are kept: the page shows the ones it was just handed.
const KEEP: usize = 4;

impl KeptLyrics {
    const fn new() -> Self {
        KeptLyrics { kept: Mutex::new((0, Vec::new())) }
    }

    /// Keeps the timing of `lyrics` and sets their `key`; lyrics with no lines keep key 0.
    fn keep(&self, lyrics: &mut nori_model::Lyrics) {
        if lyrics.lines.is_empty() {
            return;
        }
        let mut kept = self.kept.lock();
        kept.0 += 1;
        lyrics.key = kept.0;
        if kept.1.len() == KEEP {
            kept.1.remove(0);
        }
        let lines = lyrics.lines.iter().map(timed).collect();
        kept.1.push(Kept { key: lyrics.key, synced: lyrics.synced, word_timed: lyrics.word_timed, offset_ms: lyrics.offset_ms, lines });
    }

    /// A clock on the lyrics kept under `key`, as [`lyrics_clock`] makes one; 0 when no longer kept.
    fn clock(&self, key: u64, position_ms: i64) -> i64 {
        let kept = self.kept.lock();
        let Some(k) = kept.1.iter().find(|k| k.key == key) else { return 0 };
        new_clock(LyricTiming::new(k.synced, k.word_timed, k.lines.iter().cloned()), position_ms, k.offset_ms)
    }
}

/// Global because `LyricsJni.kept` is a static JNI entry with no core handle.
static KEPT: KeptLyrics = KeptLyrics::new();

/// [`KeptLyrics::keep`] on the process-wide store.
pub fn keep(lyrics: &mut nori_model::Lyrics) {
    KEPT.keep(lyrics)
}

/// [`KeptLyrics::clock`] on the process-wide store.
pub fn kept_clock(key: u64, position_ms: i64) -> i64 {
    KEPT.clock(key, position_ms)
}

/// The clock behind a handle; none for 0.
///
/// # Safety
/// `h` is 0 or a handle from [`lyrics_clock`] or [`kept_clock`] not yet given to [`free_clock`].
pub unsafe fn clock<'a>(h: i64) -> Option<&'a LyricClock> {
    // SAFETY: a live handle is a boxed clock (the caller's promise).
    (h != 0).then(|| unsafe { &*(h as *const LyricClock) })
}

/// Frees a clock handle.
///
/// # Safety
/// `h` is 0 or a live handle from [`lyrics_clock`] or [`kept_clock`], not used again.
pub unsafe fn free_clock(h: i64) {
    if h != 0 {
        // SAFETY: `h` is a boxed clock nobody else frees (the caller's promise).
        drop(unsafe { Box::from_raw(h as *mut LyricClock) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_look::lyrics::Step;
    use nori_model::{LyricLine, Lyrics};

    fn lyrics() -> Lyrics {
        let line = |start_ms, text: &str| LyricLine { start_ms, end_ms: start_ms + 2000, text: text.into(), ..Default::default() };
        Lyrics { synced: true, lines: vec![line(1000, "Żółć 🎵"), line(4000, "x")], ..Default::default() }
    }

    fn step(h: i64, at_ms: i64, force: bool) -> Step {
        Step::unpack(unsafe { clock(h) }.unwrap().advance(at_ms, true, false, force).pack())
    }

    #[test]
    fn clock_counts_utf16() {
        let h = lyrics_clock(lyrics(), 0);
        assert!(!unsafe { clock(h) }.unwrap().timing().sweeps());
        // A line without words is lit whole: seven UTF-16 units, the note being two.
        let s = step(h, 1500, true);
        assert_eq!((s.frame.active, s.frame.sung, s.redraw), (0, 7.0, true));
        unsafe { free_clock(h) };
        assert!(unsafe { clock(0) }.is_none());
    }

    #[test]
    fn kept_lyrics_start_a_clock_by_key() {
        let store = KeptLyrics::new();
        let mut l = lyrics();
        store.keep(&mut l);
        assert_ne!(l.key, 0);
        let h = store.clock(l.key, 0);
        assert_eq!(step(h, 1500, true).frame.sung, 7.0);
        // The last line ends at 4 s + 2 s; after it every line is sung.
        assert_eq!(step(h, 6000, false).frame.active, 2);
        unsafe { free_clock(h) };
        let first = l.key;
        for _ in 0..KEEP {
            store.keep(&mut l);
        }
        assert_eq!(store.clock(first, 0), 0, "only the last few are kept");
        let mut none = Lyrics::default();
        store.keep(&mut none);
        assert_eq!(none.key, 0);
    }
}
