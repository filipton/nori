//! The lyrics panel's state: the song's lyrics as the core picked them, and the clock that says which line
//! is sung and when to look again (`nori_look::lyrics`, the clock Android's lyrics page and the terminal
//! run). The window lights a line at a time; the pacing is the clock's.

use nori_core::lyrics_sources::LyricsOrigin;
use nori_core::race::{lyrics_replaces, LyricsPick};
use nori_look::lyrics::{Line, LyricClock, LyricTiming, Word};
use slint::{ModelRc, SharedString, VecModel};

pub struct SongLyrics {
    pick: LyricsPick,
    clock: LyricClock,
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
        SongLyrics { pick, clock }
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

    /// Where the music is now: the line lit (-1 for none), and in how many ms to ask again, if at all.
    pub fn advance(&self, position_ms: i64, force: bool) -> (i32, Option<u64>) {
        let step = self.clock.advance(position_ms, false, false, force);
        (step.frame.active, (step.wait > 0).then_some(step.wait as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_core::{LyricLine, Lyrics};

    fn pick() -> LyricsPick {
        let line = |start_ms, end_ms, text: &str| LyricLine { start_ms, end_ms, text: text.into(), ..Default::default() };
        let lyrics = Lyrics { synced: true, word_timed: false, lines: vec![line(1000, 2000, "one"), line(3000, 4000, "two")], key: 0, offset_ms: 0 };
        LyricsPick { lyrics, origin: LyricsOrigin::Server }
    }

    #[test]
    fn the_line_sung_is_lit_and_the_next_is_waited_for() {
        let l = SongLyrics::new(pick(), 0);
        assert_eq!(l.advance(1500, true).0, 0);
        assert_eq!(l.advance(3500, true).0, 1);
        let (_, wait) = l.advance(2500, true);
        assert!(wait.is_some_and(|ms| ms <= 500), "the next line is due at 3000");
    }
}
