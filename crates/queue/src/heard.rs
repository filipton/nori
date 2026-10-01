//! The audible song and position for the seek bar and now-playing page (`nori_player::heard` over the
//! player's own report). Asked every frame, so answers are packed into an `i64` and nothing allocates.

use nori_player::engine::Heard;
use nori_player::heard::{HeardTracker, PlayerNow, Playhead, Seen};

/// No held ending, no mix: the player's report stands.
const NOTHING: Heard = Heard {
    id: None,
    us: 0,
    at_ms: 0,
    until_us: i64::MAX,
    mixing: false,
    next_from_us: 0,
    next_rate: 1.0,
    from: None,
    audible_us: i64::MAX,
};

const MS_BITS: u32 = 43;

/// The audible queue index (None: the player's own), whether it changed since the last call, and the
/// position in ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeardAt {
    pub index: Option<usize>,
    pub changed: bool,
    pub ms: i64,
}

impl HeardAt {
    /// `(index + 1) << 44 | changed << 43 | ms`, index none being 0.
    pub fn pack(self) -> i64 {
        let index = self.index.map_or(0, |i| i as i64 + 1);
        (index << (MS_BITS + 1)) | ((self.changed as i64) << MS_BITS) | self.ms.clamp(0, (1 << MS_BITS) - 1)
    }

    /// Inverse of [`pack`](Self::pack). Twin of `PlayerConnection.read` (PlayerConnection.kt).
    pub fn unpack(r: i64) -> HeardAt {
        let index = ((r as u64 >> (MS_BITS + 1)) & 0x7FFFF) as i64 - 1;
        HeardAt { index: (index >= 0).then_some(index as usize), changed: (r >> MS_BITS) & 1 != 0, ms: r & ((1 << MS_BITS) - 1) }
    }
}

/// The page row to highlight while the audible song differs from the player's (held ending, mix).
/// `heard` indexes the core's `queue`; `page` is what the page lists, which may trail the core's queue,
/// so the nearest copy of the heard song there is taken (the earlier on a tie). None when there is no
/// heard row, the page lacks the song, or it is the `playing` song.
pub fn shown_row<Q: AsRef<str>, P: AsRef<str>>(heard: Option<usize>, queue: &[Q], page: &[P], playing: Option<&str>) -> Option<usize> {
    let at = heard?;
    let id = queue.get(at)?.as_ref();
    let row = if page.get(at).is_some_and(|p| p.as_ref() == id) {
        at
    } else {
        page.iter().enumerate().filter(|(_, p)| p.as_ref() == id).min_by_key(|(i, _)| i.abs_diff(at))?.0
    };
    (Some(id) != playing).then_some(row)
}

/// [`shown_row`] over the core's queue.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn heard_shown_row(heard: Option<u32>, page: Vec<String>, playing: Option<String>) -> Option<u32> {
    crate::playlist::with(|p| shown_row(heard.map(|h| h as usize), p.ids(), &page, playing.as_deref())).map(|r| r as u32)
}

/// The heard tracker over the core's queue.
pub struct HeardClock {
    t: HeardTracker,
    /// The queue revision the tracker last saw.
    rev: u64,
    /// What the seek bar last showed.
    head: Playhead,
}

impl Default for HeardClock {
    fn default() -> Self {
        HeardClock { t: HeardTracker::new(), rev: u64::MAX, head: Playhead::new() }
    }
}

impl HeardClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// The audible song at the player's `position_ms`.
    pub fn at(&mut self, now_ms: i64, playing: bool, position_ms: i64) -> HeardAt {
        let s = self.seen(now_ms, playing, position_ms);
        at(s, s.ms)
    }

    /// [`HeardClock::at`] for the seek bar of a page showing index `shown`: the position is held while the
    /// page has not followed the audible song yet (`Playhead`). `engine_ms` is preferred to `position_ms`,
    /// which through a media controller is only extrapolated from the last event.
    pub fn position(&mut self, now_ms: i64, playing: bool, on: Option<usize>, position_ms: i64, shown: Option<usize>, engine_ms: Option<i64>) -> HeardAt {
        let position_ms = engine_ms.unwrap_or(position_ms);
        let s = self.seen(now_ms, playing, position_ms);
        let ms = self.head.show_for(&self.t, s, shown, now_ms, on, position_ms, playing);
        at(s, ms)
    }

    /// The user seeked: the next reading is shown as is, even if it goes back (`Playhead::jumped`).
    pub fn jumped(&mut self) {
        self.head.jumped();
    }

    /// The seek bar position while the player is unreachable: the last shown, advanced if `playing`.
    pub fn run_on(&self, now_ms: i64, playing: bool) -> i64 {
        self.head.run_on(now_ms, playing)
    }

    fn seen(&mut self, now_ms: i64, playing: bool, position_ms: i64) -> Seen {
        let rev = crate::playlist::playlist_rev();
        if self.rev != rev {
            self.rev = rev;
            self.t.set_queue(crate::playlist::with(|p| crate::queue::durations(p.ids())));
        }
        self.t.at(&NOTHING, PlayerNow { now_ms, playing, on: None, position_ms }, &|_| None)
    }
}

fn at(s: Seen, ms: i64) -> HeardAt {
    HeardAt { index: s.index, changed: s.changed, ms }
}

#[cfg(test)]
mod tests {
    use super::shown_row;

    const Q: &[&str] = &["a", "b", "c", "b", "d"];
    /// Q with an "x" the core inserted at 0 that the page does not show yet.
    const INSERTED: &[&str] = &["x", "a", "b", "c", "b", "d"];

    #[test]
    fn shown_rows() {
        let cases: [(&str, Option<usize>, &[&str], &[&str], Option<&str>, Option<usize>); 10] = [
            ("the tracker's row", Some(3), Q, Q, Some("d"), Some(3)),
            ("the tracker's row, another song playing", Some(1), Q, Q, Some("c"), Some(1)),
            ("nothing heard", None, Q, Q, Some("a"), None),
            ("the player's own song", Some(2), Q, Q, Some("c"), None),
            ("past the end", Some(9), Q, Q, None, None),
            ("page lacks the song", Some(4), Q, &["a", "b", "c"], None, None),
            ("empty page", Some(0), Q, &[], None, None),
            ("trailing page", Some(4), INSERTED, Q, Some("d"), Some(3)),
            ("trailing page, nearest copy", Some(2), INSERTED, Q, Some("c"), Some(1)),
            ("a tie: the earlier", Some(2), &["b", "a", "b"], &["b", "a", "c", "a", "b"], None, Some(0)),
        ];
        for (what, heard, queue, page, playing, want) in cases {
            assert_eq!(shown_row(heard, queue, page, playing), want, "{what}");
        }
    }
}
