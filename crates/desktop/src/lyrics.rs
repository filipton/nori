//! The lyrics panel's state: the song's lyrics as the core picked them, and the clock that says which line
//! is sung and when to look again (`nori_look::lyrics`, the clock Android's lyrics page and the terminal
//! run). The window lights a line at a time; the pacing is the clock's.

use nori_core::lyrics_sources::LyricsOrigin;
use nori_core::race::{lyrics_replaces, LyricsPick};
use nori_look::lyrics::{Line, LyricClock, LyricTiming, Word};
use slint::{ModelRc, SharedString, VecModel};
use std::cell::RefCell;

use crate::sung::{self, Laid};
use crate::LyricPiece;

pub struct SongLyrics {
    pick: LyricsPick,
    clock: LyricClock,
    /// The line being sung as last laid out, in the panel and in Now Playing (sung.rs).
    laid: RefCell<[Option<Laid>; 2]>,
}

fn word(w: &nori_core::LyricWord) -> Word {
    Word { start_ms: w.start_ms, end_ms: w.end_ms, start: w.start, end: w.end }
}

impl SongLyrics {
    pub fn new(pick: LyricsPick, position_ms: i64) -> SongLyrics {
        let lyrics = &pick.lyrics;
        let lines = lyrics.lines.iter().map(|l| Line {
            start_ms: l.start_ms,
            end_ms: l.end_ms,
            len: l.text.encode_utf16().count() as u32,
            words: l.words.iter().map(word).collect(),
            backing_len: l.backing.encode_utf16().count() as u32,
            backing: l.backing_words.iter().map(word).collect(),
        });
        // The sync check's offset (lyrics that run late or early against the song) is the clock's to apply.
        let clock = LyricClock::with_offset(LyricTiming::new(lyrics.synced, lyrics.word_timed, lines), position_ms, lyrics.offset_ms);
        SongLyrics { pick, clock, laid: RefCell::new([None, None]) }
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

    /// Who the lyrics are from, when not the server.
    pub fn credit(&self) -> String {
        let name = match self.pick.origin {
            LyricsOrigin::Server => return String::new(),
            LyricsOrigin::Lrclib => "LRCLIB".to_string(),
            other => format!("{other:?}"),
        };
        format!("Lyrics from {name}")
    }

    /// Whether `next` goes on screen in place of these: the core hands over only better answers, and the
    /// same lyrics again are not new (`nori_lyrics::race::lyrics_replaces`).
    pub fn replaced_by(&self, next: &LyricsPick) -> bool {
        lyrics_replaces(Some(&self.pick), next)
    }

    /// Where the music is now: the line lit, how far its words are sung, and in how many ms to ask again, if
    /// at all. Lyrics with real word times fill in word by word; others a line at a time.
    pub fn advance(&self, position_ms: i64, force: bool) -> Now {
        let step = self.clock.advance(position_ms, true, true, force);
        let sweeping = self.clock.timing().sweeps();
        let wait = match step.wait {
            0 => None,
            // While a word fills, the clock counts display frames; a frame is a sixtieth of a second, and a
            // thirtieth is smooth enough for a fill.
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

    /// The lit line as pieces to draw (sung.rs), in the panel (`view` 0) or Now Playing (1), at `size`
    /// across `width`; laid out again only when any of those change.
    pub fn pieces(&self, view: usize, now: &Now, size: f32, width: f32, lit: f32, dim: f32) -> Vec<LyricPiece> {
        let Some((i, line)) = usize::try_from(now.active).ok().and_then(|i| self.pick.lyrics.lines.get(i).map(|l| (i, l))) else { return Vec::new() };
        let mut laid = self.laid.borrow_mut();
        let slot = &mut laid[view];
        if !slot.as_ref().is_some_and(|l| l.is(i, size, width)) {
            *slot = Some(sung::lay(i, line, size, width));
        }
        slot.as_ref().map_or_else(Vec::new, |l| sung::frame(l, line, now.at, now.ms, lit, dim, size * 0.6))
    }

    /// A click on `line`: where the song should go to sing it.
    pub fn tap(&self, line: usize) -> i64 {
        self.clock.tap(line)
    }
}

/// The lyrics at one moment, as [`SongLyrics::advance`] has them.
pub struct Now {
    pub active: i32,
    /// The lyrics fill word by word; the lit line is `sung`, then `now` (the character being sung, `mix` of
    /// the way), then `rest`.
    pub sweeping: bool,
    pub sung: String,
    pub now: String,
    pub mix: f32,
    pub rest: String,
    pub wait: Option<u64>,
    /// How far into the lit line the singing is (UTF-16 units), and the moment shown (the words' own time).
    pub at: f32,
    pub ms: i64,
}

/// `text` cut where the singing is, `at` UTF-16 units in (7.5 is half of the character at 7): the part sung,
/// the character being sung and how far, and the rest.
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
    fn the_line_sung_is_lit_and_the_next_is_waited_for() {
        let l = SongLyrics::new(pick(), 0);
        assert_eq!(l.advance(1500, true).active, 0);
        assert_eq!(l.advance(3500, true).active, 1);
        let wait = l.advance(2500, true).wait;
        assert!(wait.is_some_and(|ms| ms <= 500), "the next line is due at 3000");
    }

    #[test]
    fn a_line_is_cut_where_the_singing_is() {
        assert_eq!(split("héllo", 1.5), ("h".into(), "é".into(), 0.5, "llo".into()));
        assert_eq!(split("ab", 0.0), ("".into(), "a".into(), 0.0, "b".into()));
        assert_eq!(split("ab", 2.0), ("ab".into(), "".into(), 0.0, "".into()));
    }

    #[test]
    fn timed_words_fill_the_line() {
        let word = |start_ms, end_ms, start, end| LyricWord { start_ms, end_ms, start, end };
        let line = LyricLine { start_ms: 1000, end_ms: 3000, text: "one two".into(), words: vec![word(1000, 2000, 0, 3), word(2000, 3000, 4, 7)], ..Default::default() };
        let lyrics = Lyrics { synced: true, word_timed: true, lines: vec![line], key: 0, offset_ms: 0 };
        let l = SongLyrics::new(LyricsPick { lyrics, origin: LyricsOrigin::Server }, 0);
        let now = l.advance(2500, true);
        assert!(now.sweeping);
        assert_eq!((now.sung.as_str(), now.now.as_str(), now.rest.as_str()), ("one t", "w", "o"));
    }
}
