//! The lyrics page's clock (`nori_look::lyrics::LyricClock`) behind a handle the page holds: which line
//! is lit, how far it is sung and when to look again. The page draws; nothing here keeps time.

use std::ffi::c_char;

use nori_look::lyrics::{line_strength, LyricClock, UNSUNG};

use crate::pages::held_lyrics;
use crate::session::c_text;

/// One [`LyricClock::advance`]: the frame to draw and when to ask again.
#[repr(C)]
pub struct LyricStep {
    /// Lit line; -1 before the first or unsynced; the line count once the last is over.
    pub active: i32,
    /// How far the lit line is sung, in fractional UTF-16 units of its text.
    pub sung: f32,
    /// How long the change into `active` takes.
    pub glide_ms: i32,
    /// Next call: in display frames while words fill (`still` 0), else in ms; 0 never.
    pub wait: u32,
    pub still: i32,
    /// Whether anything visible changed.
    pub redraw: i32,
}

/// A clock on the lyrics held for `song` at `position_ms`; NULL when none are held. Free it with
/// [`nori_ios_lyric_free`].
///
/// # Safety
/// `song` is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_lyric_clock(song: *const c_char, position_ms: i64) -> *mut LyricClock {
    held_lyrics(&c_text(song)).map_or(std::ptr::null_mut(), |l| {
        Box::into_raw(Box::new(nori_core::look::clock_on(&l, position_ms)))
    })
}

/// # Safety
/// `clock` is NULL or a live handle from [`nori_ios_lyric_clock`].
unsafe fn clock<'a>(clock: *mut LyricClock) -> Option<&'a LyricClock> {
    // SAFETY: the caller's promise.
    unsafe { clock.as_ref() }
}

/// Whether the lyrics fill word by word (real word times only).
///
/// # Safety
/// As [`clock`].
#[no_mangle]
pub unsafe extern "C" fn nori_ios_lyric_sweeps(c: *mut LyricClock) -> i32 {
    unsafe { clock(c) }.is_some_and(|c| c.timing().sweeps()).into()
}

/// The frame at `position_ms` into `out`. `sweep`: fill word by word where the lyrics allow; `force`:
/// draw even if nothing moved (a first draw, a seek, a resume).
///
/// # Safety
/// As [`clock`]; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_lyric_advance(
    c: *mut LyricClock,
    position_ms: i64,
    sweep: i32,
    force: i32,
    out: *mut LyricStep,
) {
    let (Some(c), Some(out)) = (unsafe { clock(c) }, unsafe { out.as_mut() }) else { return };
    // Word animations (rise, glow) are off: only the fill moves, drawn from the frame.
    let s = c.advance(position_ms, sweep != 0, false, force != 0);
    *out = LyricStep {
        active: s.frame.active,
        sung: s.frame.sung,
        glide_ms: s.frame.glide_ms,
        wait: s.wait,
        still: s.still.into(),
        redraw: s.redraw.into(),
    };
}

/// A tap on `line`: lit at once; the position to seek to.
///
/// # Safety
/// As [`clock`].
#[no_mangle]
pub unsafe extern "C" fn nori_ios_lyric_tap(c: *mut LyricClock, line: i32) -> i64 {
    unsafe { clock(c) }.map_or(0, |c| c.tap(line.max(0) as usize))
}

/// # Safety
/// `c` is NULL or a live handle, not used again.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_lyric_free(c: *mut LyricClock) {
    if !c.is_null() {
        // SAFETY: made by `Box::into_raw` in `nori_ios_lyric_clock`, freed once (the caller's promise).
        drop(unsafe { Box::from_raw(c) });
    }
}

/// How strongly `line` is drawn with `active` lit; unsynced lyrics are fully lit.
#[no_mangle]
pub extern "C" fn nori_ios_lyric_strength(synced: i32, line: i32, active: i32) -> f32 {
    line_strength(synced != 0, line, active)
}

/// The strength of the lit line's words not yet sung.
#[no_mangle]
pub extern "C" fn nori_ios_lyric_unsung() -> f32 {
    UNSUNG
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_core::{LyricLine, LyricWord, Lyrics};
    use std::ffi::CString;

    fn timed() -> Lyrics {
        let word = |start_ms, end_ms, start, end| LyricWord { start_ms, end_ms, start, end };
        Lyrics {
            synced: true,
            word_timed: true,
            lines: vec![
                LyricLine {
                    start_ms: 1_000,
                    end_ms: 3_000,
                    text: "one two".into(),
                    words: vec![word(1_000, 2_000, 0, 3), word(2_000, 3_000, 4, 7)],
                    ..LyricLine::default()
                },
                LyricLine { start_ms: 5_000, end_ms: 6_000, text: "three".into(), ..LyricLine::default() },
            ],
            ..Lyrics::default()
        }
    }

    fn step(c: *mut LyricClock, ms: i64) -> LyricStep {
        let mut s = LyricStep { active: 0, sung: 0.0, glide_ms: 0, wait: 0, still: 0, redraw: 0 };
        unsafe { nori_ios_lyric_advance(c, ms, 1, 1, &mut s) };
        s
    }

    #[test]
    fn the_clock_lights_the_sung_line_and_fills_it_word_by_word() {
        crate::pages::lyrics_arrived("w", &timed());
        let song = CString::new("w").unwrap();
        let c = unsafe { nori_ios_lyric_clock(song.as_ptr(), 0) };
        assert!(!c.is_null());
        assert_eq!(unsafe { nori_ios_lyric_sweeps(c) }, 1);
        let mid = step(c, 1_500);
        assert_eq!(mid.active, 0);
        assert!(mid.sung > 0.0 && mid.sung < 3.0, "half way through the first word: {}", mid.sung);
        assert_eq!(step(c, 5_200).active, 1);
        assert_eq!(unsafe { nori_ios_lyric_tap(c, 1) }, 5_000);
        unsafe { nori_ios_lyric_free(c) };
        let other = CString::new("other").unwrap();
        assert!(unsafe { nori_ios_lyric_clock(other.as_ptr(), 0) }.is_null());
    }
}
