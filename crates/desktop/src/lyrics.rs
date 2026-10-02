//! Lyrics state for the current song: the core's pick and the `nori_look::lyrics` clock that says which
//! line is active and when to check again.

use nori_core::lyrics_sources::LyricsOrigin;
use nori_core::race::{lyrics_replaces, LyricsPick};
use nori_look::lyrics::LyricClock;
use skia_safe::Typeface;
use slint::{ModelRc, SharedString, VecModel};

use crate::sung::{self, Laid};
use crate::LyricPiece;

pub struct SongLyrics {
    pick: LyricsPick,
    clock: LyricClock,
    face: Option<Typeface>,
    /// Active line as last laid out, per view (side panel, Now Playing).
    laid: [Option<Laid>; 2],
}

impl SongLyrics {
    pub fn new(pick: LyricsPick, position_ms: i64, face: Option<Typeface>) -> SongLyrics {
        let clock = nori_core::look::clock_on(&pick.lyrics, position_ms);
        SongLyrics { pick, clock, face, laid: [None, None] }
    }

    pub fn lines(&self) -> ModelRc<SharedString> {
        ModelRc::new(VecModel::from(self.pick.lyrics.lines.iter().map(|l| SharedString::from(l.text.as_str())).collect::<Vec<_>>()))
    }

    pub fn synced(&self) -> bool {
        self.pick.lyrics.synced
    }

    pub fn is_empty(&self) -> bool {
        self.pick.lyrics.lines.is_empty()
    }

    /// Source credit; empty for the server's own lyrics.
    pub fn credit(&self) -> String {
        let name = match self.pick.origin {
            LyricsOrigin::Server => return String::new(),
            LyricsOrigin::Lrclib => "LRCLIB".to_string(),
            other => format!("{other:?}"),
        };
        format!("Lyrics from {name}")
    }

    /// Whether `next` should replace these (`lyrics_replaces`).
    pub fn replaced_by(&self, next: &LyricsPick) -> bool {
        lyrics_replaces(Some(&self.pick), next)
    }

    /// State at `position_ms`: active line, fill progress, and when to call again.
    pub fn advance(&self, position_ms: i64, force: bool) -> Now {
        let step = self.clock.advance(position_ms, true, true, force);
        let sweeping = self.clock.timing().sweeps();
        let wait = match step.wait {
            0 => None,
            // While a word fills, the clock counts 60 Hz frames.
            n if sweeping && !step.still => Some((n as u64 * 16).max(16)),
            n => Some(n as u64),
        };
        let line = usize::try_from(step.frame.active).ok().and_then(|i| self.pick.lyrics.lines.get(i));
        let (sung, now, mix, rest) = match line {
            Some(l) if sweeping => split(&l.text, step.frame.sung),
            _ => Default::default(),
        };
        Now { active: step.frame.active, sweeping, sung, now, mix, rest, wait, at: step.frame.sung, ms: self.clock.shown_ms() }
    }

    /// The active line's pieces for `view` (0 side panel, 1 Now Playing); re-laid only when line, size
    /// or width change.
    pub fn pieces(&mut self, view: usize, now: &Now, size: f32, width: f32, lit: f32, dim: f32) -> Vec<LyricPiece> {
        let Some(face) = &self.face else { return Vec::new() };
        let Some((i, line)) = usize::try_from(now.active).ok().and_then(|i| self.pick.lyrics.lines.get(i).map(|l| (i, l))) else { return Vec::new() };
        let slot = &mut self.laid[view];
        if !slot.as_ref().is_some_and(|l| l.is_for(i, size, width)) {
            *slot = Some(sung::lay(face, i, line, size, width));
        }
        slot.as_ref().map_or_else(Vec::new, |l| sung::frame(l, line, now.at, now.ms, lit, dim, size * 0.6))
    }

    /// Seek target for a click on `line`.
    pub fn tap(&self, line: usize) -> i64 {
        self.clock.tap(line)
    }
}

/// Lyrics state at one moment ([`SongLyrics::advance`]).
pub struct Now {
    pub active: i32,
    /// Word-by-word fill: the active line is `sung` + `now` (the current character, `mix` filled) + `rest`.
    pub sweeping: bool,
    pub sung: String,
    pub now: String,
    pub mix: f32,
    pub rest: String,
    pub wait: Option<u64>,
    /// Fill position in the active line (UTF-16 units), and the lyric-time ms it corresponds to.
    pub at: f32,
    pub ms: i64,
}

/// Splits `text` at `at` UTF-16 units (7.5 = half of char 7): before, current char, its fill, after.
fn split(text: &str, at: f32) -> (String, String, f32, String) {
    let mut units = 0.0f32;
    for (i, c) in text.char_indices() {
        let w = c.len_utf16() as f32;
        if at < units + w {
            let end = i + c.len_utf8();
            return (text[..i].to_string(), text[i..end].to_string(), ((at - units) / w).clamp(0.0, 1.0), text[end..].to_string());
        }
        units += w;
    }
    (text.to_string(), String::new(), 0.0, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_core::{LyricLine, LyricWord, Lyrics};

    fn pick() -> LyricsPick {
        let line = |start_ms, end_ms, text: &str| LyricLine { start_ms, end_ms, text: text.into(), ..Default::default() };
        let lyrics = Lyrics { synced: true, word_timed: false, lines: vec![line(1000, 2000, "one"), line(3000, 4000, "two")], key: 0, offset_ms: 0 };
        LyricsPick { lyrics, origin: LyricsOrigin::Server }
    }

    #[test]
    fn lyric_lines() {
        let l = SongLyrics::new(pick(), 0, None);
        assert_eq!(l.advance(1500, true).active, 0);
        assert_eq!(l.advance(3500, true).active, 1);
        let wait = l.advance(2500, true).wait;
        assert!(wait.is_some_and(|ms| ms <= 500), "the next line is due at 3000");

        // Split at fill position.
        assert_eq!(split("héllo", 1.5), ("h".into(), "é".into(), 0.5, "llo".into()));
        assert_eq!(split("ab", 0.0), ("".into(), "a".into(), 0.0, "b".into()));
        assert_eq!(split("ab", 2.0), ("ab".into(), "".into(), 0.0, "".into()));

        // Timed words fill the line.
        let word = |start_ms, end_ms, start, end| LyricWord { start_ms, end_ms, start, end };
        let line = LyricLine { start_ms: 1000, end_ms: 3000, text: "one two".into(), words: vec![word(1000, 2000, 0, 3), word(2000, 3000, 4, 7)], ..Default::default() };
        let lyrics = Lyrics { synced: true, word_timed: true, lines: vec![line], key: 0, offset_ms: 0 };
        let l = SongLyrics::new(LyricsPick { lyrics, origin: LyricsOrigin::Server }, 0, None);
        let now = l.advance(2500, true);
        assert!(now.sweeping);
        assert_eq!((now.sung.as_str(), now.now.as_str(), now.rest.as_str()), ("one t", "w", "o"));
    }

}
