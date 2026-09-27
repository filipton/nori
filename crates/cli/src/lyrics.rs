//! The lyrics page's state: the song's lyrics as the core picked them, and the clock that says which
//! line is sung, how far into it the singing is, and when to look again (`nori_look::lyrics`, the same
//! clock Android's lyrics page runs). The terminal draws a character at a time, so the fill moves in
//! whole characters; the pacing is the clock's.

use nori_core::race::{lyrics_replaces, LyricsPick};
use nori_core::lyrics_sources::LyricsOrigin;
use nori_look::lyrics::{Line, LyricClock, LyricTiming, Step, Word};

/// How long a display frame is, when the clock counts in frames (while a word-timed line fills): the
/// terminal redraws at most this often, and only while the lyrics are on screen and music plays.
pub const FRAME_MS: u64 = 50;

pub struct SongLyrics {
    /// The lyrics and where they came from, as the core handed them over.
    pub pick: LyricsPick,
    pub clock: LyricClock,
    /// Each line's text as chars, and each char's UTF-16 offset, so the fill (in UTF-16 units) lands on a
    /// character without counting again every frame.
    pub offsets: Vec<Vec<u32>>,
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
        let offsets = lyrics
            .lines
            .iter()
            .map(|l| {
                let mut at = 0u32;
                l.text
                    .chars()
                    .map(|c| {
                        let here = at;
                        at += c.len_utf16() as u32;
                        here
                    })
                    .collect()
            })
            .collect();
        SongLyrics { pick, clock, offsets }
    }

    /// How many characters of line `line` are sung at `sung` UTF-16 units: the fill in whole characters.
    pub fn sung_chars(&self, line: usize, sung: f32) -> usize {
        let Some(o) = self.offsets.get(line) else { return 0 };
        o.iter().take_while(|&&u| (u as f32) < sung.floor()).count()
    }

    /// Who the lyrics are from, as the page credits it.
    pub fn credit(&self) -> Option<String> {
        match self.pick.origin {
            LyricsOrigin::Server => None,
            s => crate::text::lyrics_credit(s, self.pick.lyrics.synced),
        }
    }

    /// Whether `next` goes on screen in place of these: the core hands over only better answers, and
    /// the same lyrics again are not new (`nori_lyrics::race::lyrics_replaces`).
    pub fn replaced_by(&self, next: &LyricsPick) -> bool {
        lyrics_replaces(Some(&self.pick), next)
    }

    /// The per-wake question: where the music is now; what to draw, and when to ask again (ms), if at all.
    pub fn advance(&self, position_ms: i64, sweep: bool, force: bool) -> (Step, Option<u64>) {
        let step = self.clock.advance(position_ms, sweep, false, force);
        let wait = match step.wait {
            0 => None,
            w if step.still || !(sweep && self.clock.timing().sweeps()) => Some(w as u64),
            frames => Some(frames as u64 * FRAME_MS),
        };
        (step, wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_core::{LyricLine, LyricWord, Lyrics};

    fn server(lyrics: Lyrics) -> LyricsPick {
        LyricsPick { lyrics, origin: LyricsOrigin::Server }
    }

    fn lyrics() -> Lyrics {
        let words = vec![LyricWord { start_ms: 1000, end_ms: 1500, start: 0, end: 5 }, LyricWord { start_ms: 1500, end_ms: 2000, start: 6, end: 11 }];
        let l1 = LyricLine { start_ms: 1000, end_ms: 2000, text: "Hello world".into(), words, ..Default::default() };
        let l2 = LyricLine { start_ms: 3000, end_ms: 4000, text: "Żółć ok".into(), ..Default::default() };
        Lyrics { synced: true, word_timed: true, lines: vec![l1, l2], key: 0, offset_ms: 0 }
    }

    #[test]
    fn the_fill_follows_the_words_in_whole_characters() {
        let l = SongLyrics::new(server(lyrics()), 0);
        let (step, _) = l.advance(1250, true, true);
        assert_eq!(step.frame.active, 0);
        assert_eq!(l.sung_chars(0, step.frame.sung), 2, "half of Hello");
        let (step, wait) = l.advance(1750, true, false);
        assert!(step.redraw);
        assert!(l.sung_chars(0, step.frame.sung) >= 8);
        assert!(wait.is_some());
    }

    #[test]
    fn untimed_lyrics_never_wake_the_screen() {
        let mut plain = lyrics();
        plain.synced = false;
        plain.word_timed = false;
        let l = SongLyrics::new(server(plain), 0);
        assert_eq!(l.advance(1000, true, true).1, None);
    }

    #[test]
    fn the_same_lyrics_again_are_not_new() {
        let l = SongLyrics::new(server(Lyrics::default()), 0);
        assert!(l.replaced_by(&server(lyrics())));
        let l = SongLyrics::new(server(lyrics()), 0);
        assert!(!l.replaced_by(&server(Lyrics { key: 9, ..lyrics() })), "read again under another key");
        assert!(l.replaced_by(&LyricsPick { lyrics: lyrics(), origin: LyricsOrigin::Lrclib }));
    }
}
