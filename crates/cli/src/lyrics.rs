//! Lyrics state: the core's pick plus `nori_look::lyrics::LyricClock` (shared with Android), which gives
//! the active line, the word fill and the next wake. The terminal fills whole characters.

use nori_core::race::{lyrics_replaces, LyricsPick};
use nori_core::lyrics_sources::LyricsOrigin;
use nori_look::lyrics::{LyricClock, Step};

/// Redraw interval while a word-timed line fills (the clock counts in frames then).
pub const FRAME_MS: u64 = 50;

pub struct SongLyrics {
    pub pick: LyricsPick,
    pub clock: LyricClock,
    /// Per line, each char's UTF-16 offset: the clock's fill is in UTF-16 units.
    pub offsets: Vec<Vec<u32>>,
}

impl SongLyrics {
    pub fn new(pick: LyricsPick, position_ms: i64) -> SongLyrics {
        let clock = nori_core::look::clock_on(&pick.lyrics, position_ms);
        let offsets = pick
            .lyrics
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

    /// Characters of `line` sung at `sung` UTF-16 units.
    pub fn sung_chars(&self, line: usize, sung: f32) -> usize {
        let Some(o) = self.offsets.get(line) else { return 0 };
        o.iter().take_while(|&&u| (u as f32) < sung.floor()).count()
    }

    /// The credit line; none for the server's lyrics.
    pub fn credit(&self) -> Option<String> {
        let origin = self.pick.origin;
        (origin != LyricsOrigin::Server).then(|| crate::text::lyrics_credit(origin, self.pick.lyrics.synced))
    }

    /// Whether `next` should replace these (`race::lyrics_replaces`: the same lyrics again do not).
    pub fn replaced_by(&self, next: &LyricsPick) -> bool {
        lyrics_replaces(Some(&self.pick), next)
    }

    /// Advances the clock to `position_ms`: what to draw, and ms until the next wake (None: never).
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
        Lyrics { synced: true, word_timed: true, lines: vec![l1, l2], offset_ms: 0 }
    }

    #[test]
    fn lyrics_view() {
        let l = SongLyrics::new(server(lyrics()), 0);
        let (step, _) = l.advance(1250, true, true);
        assert_eq!(step.frame.active, 0);
        assert_eq!(l.sung_chars(0, step.frame.sung), 2, "half of Hello");
        let (step, wait) = l.advance(1750, true, false);
        assert!(step.redraw);
        assert!(l.sung_chars(0, step.frame.sung) >= 8);
        assert!(wait.is_some());

        // Untimed lyrics never wake.
        let mut plain = lyrics();
        plain.synced = false;
        plain.word_timed = false;
        let l = SongLyrics::new(server(plain), 0);
        assert_eq!(l.advance(1000, true, true).1, None);

        // Same lyrics do not replace.
        let l = SongLyrics::new(server(Lyrics::default()), 0);
        assert!(l.replaced_by(&server(lyrics())));
        let l = SongLyrics::new(server(lyrics()), 0);
        assert!(!l.replaced_by(&server(lyrics())), "read again");
        assert!(l.replaced_by(&LyricsPick { lyrics: lyrics(), origin: LyricsOrigin::Lrclib }));
    }

}
