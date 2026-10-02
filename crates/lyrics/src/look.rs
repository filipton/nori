//! The lyrics page's clock (`nori_look::lyrics`), handed out as a raw handle so a platform across a
//! language boundary can hold it.

use nori_look::lyrics::{Line, LyricClock, LyricTiming, Word};

/// A clock on `lyrics` at `position_ms`, as a handle the platform frees with `LyricsJni.destroy`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn lyrics_clock(lyrics: nori_model::Lyrics, position_ms: i64) -> i64 {
    Box::into_raw(Box::new(clock_on(&lyrics, position_ms))) as i64
}

/// A clock on `lyrics` at `position_ms`.
pub fn clock_on(lyrics: &nori_model::Lyrics, position_ms: i64) -> LyricClock {
    LyricClock::with_offset(LyricTiming::new(lyrics.synced, lyrics.word_timed, lyrics.lines.iter().map(timed)), position_ms, lyrics.offset_ms)
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

/// The clock behind a handle; none for 0.
///
/// # Safety
/// `h` is 0 or a handle from [`lyrics_clock`] not yet given to [`free_clock`].
pub unsafe fn clock<'a>(h: i64) -> Option<&'a LyricClock> {
    // SAFETY: a live handle is a boxed clock (the caller's promise).
    (h != 0).then(|| unsafe { &*(h as *const LyricClock) })
}

/// Frees a clock handle.
///
/// # Safety
/// `h` is 0 or a live handle from [`lyrics_clock`], not used again.
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
    fn clocks() {
        let h = lyrics_clock(lyrics(), 0);
        assert!(!unsafe { clock(h) }.unwrap().timing().sweeps());
        // A line without words is lit whole: seven UTF-16 units, the note being two.
        let s = step(h, 1500, true);
        assert_eq!((s.frame.active, s.frame.sung, s.redraw), (0, 7.0, true));
        unsafe { free_clock(h) };
        assert!(unsafe { clock(0) }.is_none());
    }

}
