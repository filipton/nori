//! Seek retry. A seek on a source still being opened can be dropped (the track starts from 0), and a
//! media controller reports the requested position before it is real. So a seek is re-checked on
//! player events and a short timer until it clearly landed or the song changed, and re-issued if dropped.

/// How long a seek is watched; extended while the position converges on it.
pub const KEEP_MS: i64 = 15_000;
/// Poll interval while a seek is watched.
pub const LOOK_EVERY_MS: i64 = 300;
/// Distance from the target that counts as there.
const NEAR_MS: i64 = 1_500;
/// Re-issues of a dropped seek.
const TRIES: u32 = 3;

/// What to do after a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Keep watching.
    Watch,
    /// Stop watching: landed, gave up, or the song changed.
    Forget,
    /// Dropped: seek to this position again and keep watching.
    SeekAgain(i64),
}

#[derive(Debug, Clone, Default)]
pub struct SeekKeeper {
    wanted: Option<Wanted>,
}

#[derive(Debug, Clone)]
struct Wanted {
    target: i64,
    until: i64,
    tries: u32,
    /// Position before the seek (at request if ready, else at the first ready check).
    /// `low`: lowest position seen since; `last`: the previous check's.
    from: Option<i64>,
    low: Option<i64>,
    last: Option<i64>,
}

impl SeekKeeper {
    pub fn new() -> Self {
        SeekKeeper::default()
    }

    /// The target being watched, for the seek bar to hold.
    pub fn pending(&self) -> Option<i64> {
        self.wanted.as_ref().map(|w| w.target)
    }

    /// Starts watching a seek to `target`. `pos` is only meaningful if `ready` (an opening player reports
    /// 0), otherwise the anchor waits for the first ready check. A `pos` already near the target anchors
    /// at the target, since a controller may report the requested position immediately.
    pub fn ask(&mut self, target: i64, now: i64, ready: bool, pos: i64) {
        let from = ready.then(|| if (pos - target).abs() <= NEAR_MS { target } else { pos });
        self.wanted = Some(Wanted { target, until: now + KEEP_MS, tries: 0, from, low: from, last: None });
    }

    pub fn forget(&mut self) {
        self.wanted = None;
    }

    /// Checks the player's state against the pending seek.
    pub fn look(&mut self, now: i64, same_song: bool, ready: bool, pos: i64, playing: bool) -> Verdict {
        let v = self.judge(now, same_song, ready, pos, playing);
        if v == Verdict::Forget {
            self.wanted = None;
        }
        v
    }

    fn judge(&mut self, now: i64, same_song: bool, ready: bool, pos: i64, playing: bool) -> Verdict {
        let Some(w) = self.wanted.as_mut() else { return Verdict::Forget };
        if !same_song {
            return Verdict::Forget;
        }
        if !ready {
            return if now > w.until { Verdict::Forget } else { Verdict::Watch };
        }
        let Some(from) = w.from else {
            // First ready check: anchor here.
            let at = if (pos - w.target).abs() <= NEAR_MS { w.target } else { pos };
            (w.from, w.low, w.until) = (Some(at), Some(at), now + KEEP_MS);
            return Verdict::Watch;
        };
        if now > w.until {
            return Verdict::Forget;
        }
        let target = w.target;
        let near = (pos - target).abs() <= NEAR_MS;
        // Paused near the target: landed.
        if near && !playing {
            return Verdict::Forget;
        }
        let low = w.low.map_or(pos, |l| l.min(pos));
        w.low = Some(low);
        // Converging in stages (transcoded streams): extend the window.
        let converging = w.last.is_some_and(|last| (pos - target).abs() + 250 < (last - target).abs());
        w.last = Some(pos);
        if converging {
            w.until = now + KEEP_MS;
            return Verdict::Watch;
        }
        let came_down = low < from - 500;
        let again = |w: &mut Wanted| {
            if w.tries >= TRIES {
                Verdict::Forget
            } else {
                w.tries += 1;
                Verdict::SeekAgain(target)
            }
        };
        if target >= from {
            // Forward: played past it means landed.
            if pos > target + 400 {
                return Verdict::Forget;
            }
            if near {
                return Verdict::Watch;
            }
            // Still at the start: dropped. Elsewhere: moved on without it.
            return if pos <= from + NEAR_MS { again(w) } else { Verdict::Forget };
        }
        // Backward: landed once the position came down and plays on from the target.
        if playing && came_down && pos >= target - NEAR_MS {
            return Verdict::Forget;
        }
        if near {
            return Verdict::Watch;
        }
        // Never moved: dropped. Anything else is stale.
        if !came_down && pos >= from - NEAR_MS {
            again(w)
        } else {
            Verdict::Forget
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_forward_seek_is_retried() {
        let mut k = SeekKeeper::new();
        k.ask(60_000, 0, true, 5_000);
        assert_eq!(k.look(300, true, true, 5_300, true), Verdict::SeekAgain(60_000), "still at the start");
        assert_eq!(k.look(600, true, true, 60_100, true), Verdict::Watch, "playing through it");
        assert_eq!(k.look(900, true, true, 60_500, true), Verdict::Forget, "past it: kept");
        assert_eq!(k.pending(), None);
    }

    #[test]
    fn retries_are_limited() {
        let mut k = SeekKeeper::new();
        k.ask(60_000, 0, true, 5_000);
        for t in 1..=3 {
            assert_eq!(k.look(t * 300, true, true, 5_000, true), Verdict::SeekAgain(60_000));
        }
        assert_eq!(k.look(1_200, true, true, 5_000, true), Verdict::Forget);
    }

    #[test]
    fn seek_while_opening_anchors_on_first_ready() {
        let mut k = SeekKeeper::new();
        k.ask(90_000, 0, false, 0);
        assert_eq!(k.look(300, true, false, 0, false), Verdict::Watch, "not ready yet");
        // The session restored to 40 s and dropped the seek: anchored there, then asked again.
        assert_eq!(k.look(600, true, true, 40_000, true), Verdict::Watch);
        assert_eq!(k.look(900, true, true, 40_300, true), Verdict::SeekAgain(90_000));
    }

    #[test]
    fn paused_near_target_or_song_change_ends_watch() {
        let mut k = SeekKeeper::new();
        k.ask(30_000, 0, true, 10_000);
        assert_eq!(k.look(300, true, true, 30_200, false), Verdict::Forget);
        k.ask(30_000, 0, true, 10_000);
        assert_eq!(k.look(300, false, true, 0, true), Verdict::Forget);
    }

    #[test]
    fn backward_seek_lands_after_coming_down() {
        let mut k = SeekKeeper::new();
        k.ask(20_000, 0, true, 120_000);
        assert_eq!(k.look(300, true, true, 120_300, true), Verdict::SeekAgain(20_000), "never moved");
        assert_eq!(k.look(600, true, true, 20_100, true), Verdict::Watch, "a jump that close is still arriving");
        assert_eq!(k.look(900, true, true, 20_400, true), Verdict::Forget, "came down to it and plays on");
    }

    #[test]
    fn staged_seek_extends_window() {
        let mut k = SeekKeeper::new();
        k.ask(200_000, 0, true, 10_000);
        let mut t = 0;
        for pos in [10_000, 80_000, 150_000] {
            t += 1_000;
            let v = k.look(t, true, true, pos, true);
            assert!(v != Verdict::Forget, "{pos}: {v:?}");
        }
        assert_eq!(k.look(t + 14_000, true, true, 150_100, true), Verdict::Forget, "played on without it after converging");
    }

    #[test]
    fn gives_up_after_window() {
        let mut k = SeekKeeper::new();
        k.ask(60_000, 0, false, 0);
        assert_eq!(k.look(KEEP_MS + 1, true, false, 0, false), Verdict::Forget);
    }
}
