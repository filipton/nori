//! Queue movement rules: placing hand-added songs, shuffle, prefetch and measure-ahead ranges, error
//! handling, refilling past the end, and previous/repeat. Works on indexes; the platform holds the list.

/// Where added songs go and, when shuffling, the new play order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// List index to insert at.
    pub at: usize,
    /// The play order afterwards (list indexes, new songs included), when shuffling.
    pub order: Option<Vec<usize>>,
}

/// Play next (`last` false: right after `cur`) and Add to queue (`last`: after the run of hand-added
/// songs following `cur`), as Apple does. Under shuffle the new songs are placed in the play order the
/// same way. `hand[i]`: song i was added by hand; `count`: songs being added.
pub fn place(n: usize, cur: usize, hand: &[bool], order: Option<&[usize]>, last: bool, count: usize) -> Placement {
    // Last hand-added song after `cur` along `walk` (or `cur` for play next).
    let run_end = |walk: &dyn Fn(usize) -> Option<usize>| {
        let mut end = cur;
        if !last {
            return end;
        }
        let mut i = walk(cur);
        while let Some(x) = i {
            if !hand.get(x).copied().unwrap_or(false) {
                break;
            }
            end = x;
            i = walk(x);
        }
        end
    };
    let in_list = |i: usize| (i + 1 < n).then_some(i + 1);
    let at = run_end(&in_list) + 1;
    let Some(order) = order else { return Placement { at, order: None } };
    let pos_of = |x: usize| order.iter().position(|&o| o == x);
    let in_order = |i: usize| pos_of(i).and_then(|p| order.get(p + 1).copied());
    let end = run_end(&in_order);
    let shift = |i: usize| if i >= at { i + count } else { i };
    let mut next: Vec<usize> = order.iter().map(|&i| shift(i)).collect();
    let after = next.iter().position(|&i| i == shift(end)).map_or(next.len(), |p| p + 1);
    next.splice(after..after, at..at + count);
    Placement { at, order: Some(next) }
}

/// The play order when shuffle is turned on: the current song first, the hand-added run after it in
/// order, the rest shuffled.
pub fn shuffle_around(n: usize, cur: usize, hand: &[bool], seed: u64) -> Vec<usize> {
    let mut kept = vec![cur];
    let mut i = cur + 1;
    while i < n && hand.get(i).copied().unwrap_or(false) {
        kept.push(i);
        i += 1;
    }
    let mut rest: Vec<usize> = (0..n).filter(|x| !kept.contains(x)).collect();
    shuffle(&mut rest, seed);
    kept.extend(rest);
    kept
}

/// Deterministic Fisher-Yates over xorshift.
pub fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut s = seed | 1;
    for i in (1..items.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        items.swap(i, (s % (i as u64 + 1)) as usize);
    }
}

/// The upcoming songs to prefetch (0 = playing), inclusive. The player buffers the next song itself,
/// except that a mix needs it early. Under shuffle only the next song. `None`: nothing.
pub fn precache_range(count: usize, mixing: bool, shuffling: bool) -> Option<(usize, usize)> {
    let first = if mixing { 1 } else { 2 };
    let last = (if shuffling { 0 } else { count }).max(if mixing { 1 } else { 0 });
    (last >= first).then_some((first, last))
}

/// Whether transitions will mix songs, for [`precache_range`].
pub fn mixing(transitions_off: bool, crossfade_s: i32, auto_mix: bool) -> bool {
    !transitions_off && (crossfade_s > 0 || auto_mix)
}

/// Prefetch count for the current network.
pub fn precache_count(metered: bool, wifi: i32, mobile: i32) -> usize {
    (if metered { mobile } else { wifi }).max(0) as usize
}

/// Upcoming songs (0 = playing) measured ahead for AutoMix.
pub const MEASURE_AHEAD: usize = 3;

/// Songs to measure ahead: none with AutoMix off.
pub fn measure_ahead(auto_mix: bool) -> usize {
    if auto_mix {
        MEASURE_AHEAD
    } else {
        0
    }
}

/// A song would not play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackError {
    /// The output refused the stream (typically offloaded).
    Output,
    /// The server could not be reached.
    Network,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    /// Disable offload, rebuild on the CPU path and retry the song.
    GiveUpOffload,
    /// Hand over to the offline bridge.
    Bridge,
    Skip,
    Stop,
}

/// What to do when a song will not play: an offload refusal drops offload, a network error goes to the
/// bridge if enabled, anything else skips (at most three in a row), else stop.
pub fn on_error(kind: PlaybackError, offload_refused: bool, bridge: bool, skip_on_error: bool, has_next: bool, errors_in_a_row: u32) -> OnError {
    match kind {
        PlaybackError::Output if !offload_refused => OnError::GiveUpOffload,
        PlaybackError::Network if bridge => OnError::Bridge,
        _ if skip_on_error && has_next && errors_in_a_row < 3 => OnError::Skip,
        _ => OnError::Stop,
    }
}

/// Consecutive skips for errors; reset when a song plays or the bridge takes over.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ErrorRun {
    in_a_row: u32,
}

impl ErrorRun {
    pub const fn new() -> Self {
        ErrorRun { in_a_row: 0 }
    }

    /// [`on_error`] with the run so far; skips are counted.
    pub fn failed(&mut self, kind: PlaybackError, offload_refused: bool, bridge: bool, skip_on_error: bool, has_next: bool) -> OnError {
        let d = on_error(kind, offload_refused, bridge, skip_on_error, has_next, self.in_a_row);
        if d == OnError::Skip {
            self.in_a_row += 1;
        }
        d
    }

    /// The bridge could not take over: skip under the same limit.
    pub fn bridge_failed(&mut self, skip_on_error: bool, has_next: bool) -> bool {
        let skip = on_error(PlaybackError::Other, true, false, skip_on_error, has_next, self.in_a_row) == OnError::Skip;
        if skip {
            self.in_a_row += 1;
        }
        skip
    }

    /// A song played or the bridge took over.
    pub fn played(&mut self) {
        self.in_a_row = 0;
    }

    pub fn in_a_row(&self) -> u32 {
        self.in_a_row
    }
}

/// Refilling past the end of the queue. One fetch at a time starts once at most [`FILL_AHEAD`] songs
/// remain; the results go in only if the queue's end is unchanged.
///
/// A next pressed with nothing after is taken when the songs land, unless the user moved on or the last
/// press is older than [`NEXT_KEPT_MS`] (a late skip would surprise).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refill {
    in_flight: bool,
    /// The queue's last entry (play order, `Playlist::seqs`) when the fetch started.
    end: Option<u64>,
    /// The entry a next was pressed on with nothing after, and the latest press time.
    pending_from: Option<u64>,
    pending_at_ms: i64,
}

/// Songs remaining after the current one when refilling starts.
pub const FILL_AHEAD: usize = 2;
/// How long a next pressed at the end waits for songs.
pub const NEXT_KEPT_MS: i64 = 2_000;

/// A queue can be refilled: a non-radio song playing, repeat off, setting on.
pub fn refillable(song: bool, radio: bool, repeat: u8, setting: bool) -> bool {
    song && !radio && repeat == crate::playlist::REPEAT_OFF && setting
}

impl Refill {
    pub const fn new() -> Self {
        Refill { in_flight: false, end: None, pending_from: None, pending_at_ms: 0 }
    }

    /// Whether to start a fetch now (`after` songs follow, `end` is last); true until [`Refill::arrived`].
    pub fn start(&mut self, refillable: bool, after: usize, end: Option<u64>) -> bool {
        if !refillable || after > FILL_AHEAD || self.in_flight {
            return false;
        }
        self.in_flight = true;
        self.end = end;
        true
    }

    /// Next pressed: true to skip now; with nothing after, remembers the press if refillable.
    pub fn next(&mut self, has_next: bool, can_refill: bool, current: Option<u64>, now_ms: i64) -> bool {
        if has_next {
            self.pending_from = None;
            return true;
        }
        if can_refill {
            self.pending_from = current;
            self.pending_at_ms = now_ms;
        }
        false
    }

    /// The fetch returned `count` songs: whether to insert them (the end is unchanged). Otherwise the
    /// fetch and any waiting next are dropped.
    pub fn arrived(&mut self, count: usize, end: Option<u64>) -> bool {
        let keep = count > 0 && end.is_some() && self.end == end;
        if !keep {
            self.pending_from = None;
            self.in_flight = false;
            self.end = None;
        }
        keep
    }

    /// The songs are in: whether to take the waiting next now.
    pub fn landed(&mut self, current: Option<u64>, has_next: bool, now_ms: i64) -> bool {
        let waiting = self.skip_waiting(current, now_ms);
        self.in_flight = false;
        self.end = None;
        self.pending_from = None;
        waiting && has_next
    }

    /// A next is waiting and would be taken now (shown as a busy next button).
    pub fn skip_waiting(&self, current: Option<u64>, now_ms: i64) -> bool {
        self.in_flight && self.pending_from.is_some_and(|p| Some(p) == current) && now_ms - self.pending_at_ms <= NEXT_KEPT_MS
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }
}

/// What arriving on a song means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Onto {
    /// Explicit with the skip setting on and somewhere to go.
    Skip,
    /// The same song again under repeat.
    Loop,
    Song,
}

/// Classifies an arrival (`song` false: onto nothing; `looped`: the player's repeat).
pub fn arrival(song: bool, skip_explicit: bool, explicit: bool, has_next: bool, looped: bool) -> Onto {
    if song && skip_explicit && explicit && has_next {
        Onto::Skip
    } else if song && looped {
        Onto::Loop
    } else {
        Onto::Song
    }
}

/// Past this, previous restarts the song (media3's rule) unless set to always skip.
pub const PREVIOUS_REWINDS_AFTER_MS: i64 = 3_000;

/// Whether previous restarts the current song.
pub fn previous_restarts(position_ms: i64, has_previous: bool, always_skips: bool) -> bool {
    !(always_skips && has_previous) && position_ms > PREVIOUS_REWINDS_AFTER_MS
}

/// Cycles repeat off -> all -> one -> off.
pub fn next_repeat(mode: u8) -> u8 {
    use crate::playlist::{REPEAT_ALL, REPEAT_OFF, REPEAT_ONE};
    match mode {
        REPEAT_OFF => REPEAT_ALL,
        REPEAT_ALL => REPEAT_ONE,
        _ => REPEAT_OFF,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_added_placement() {
        let hand = [false, false, true, false, false];
        assert_eq!(place(5, 1, &hand, None, false, 2), Placement { at: 2, order: None });

        // Add to queue after hand added run.
        let hand = [false, false, true, true, false];
        assert_eq!(place(5, 1, &hand, None, true, 1).at, 4);
    }

    #[test]
    fn shuffled_placement() {
        // Order 3, 0, 4, 1, 2; two songs inserted at 4 shift the rest and follow 3 in the order.
        let order = [3, 0, 4, 1, 2];
        let p = place(5, 3, &[false; 5], Some(&order), false, 2);
        assert_eq!(p.at, 4);
        assert_eq!(p.order, Some(vec![3, 4, 5, 0, 6, 1, 2]));

        // Shuffle keeps current and hand added first.
        let hand = [false, false, true, true, false, false, false];
        let o = shuffle_around(7, 1, &hand, 42);
        assert_eq!(&o[..3], &[1, 2, 3]);
        let mut rest = o[3..].to_vec();
        rest.sort();
        assert_eq!(rest, vec![0, 4, 5, 6]);
    }

    #[test]
    fn precache_range_cases() {
        assert_eq!(precache_range(3, false, false), Some((2, 3)));
        assert_eq!(precache_range(3, true, false), Some((1, 3)), "a mix needs the next song early");
        assert_eq!(precache_range(3, false, true), None, "shuffle: nothing deep");
        assert_eq!(precache_range(3, true, true), Some((1, 1)), "but the next song is the next song");
    }

    #[test]
    fn error_handling() {
        assert_eq!(on_error(PlaybackError::Output, false, false, true, true, 0), OnError::GiveUpOffload);
        assert_eq!(on_error(PlaybackError::Output, true, false, true, true, 0), OnError::Skip, "already off offload");
        assert_eq!(on_error(PlaybackError::Network, false, true, true, true, 0), OnError::Bridge);
        assert_eq!(on_error(PlaybackError::Other, false, true, true, true, 2), OnError::Skip);
        assert_eq!(on_error(PlaybackError::Other, false, true, true, true, 3), OnError::Stop);
        assert_eq!(on_error(PlaybackError::Other, false, false, false, true, 0), OnError::Stop);

        // Error run counts skips.
        let mut r = ErrorRun::new();
        for _ in 0..3 {
            assert_eq!(r.failed(PlaybackError::Other, false, false, true, true), OnError::Skip);
        }
        assert_eq!(r.failed(PlaybackError::Other, false, false, true, true), OnError::Stop, "three in a row, then it stops");
        assert_eq!(r.in_a_row(), 3);
        r.played();
        assert_eq!(r.failed(PlaybackError::Network, false, true, true, true), OnError::Bridge);
        assert_eq!(r.in_a_row(), 0, "handing to the bridge is not a skip");
        assert!(r.bridge_failed(true, true));
        assert!(!r.bridge_failed(false, true), "skip on error off");
        assert!(!r.bridge_failed(true, false), "nothing after");
        assert!(r.bridge_failed(true, true) && r.bridge_failed(true, true));
        assert!(!r.bridge_failed(true, true), "the bridge's skips count toward the same three");
        assert_eq!(r.failed(PlaybackError::Output, false, false, true, true), OnError::GiveUpOffload);
        assert_eq!(r.in_a_row(), 3);
    }

    #[test]
    fn fetch_and_measure_counts() {
        assert!(mixing(false, 4, false) && mixing(false, 0, true));
        assert!(!mixing(false, 0, false), "no transition");
        assert!(!mixing(true, 4, true), "the output forbids it");
        assert_eq!((precache_count(true, 3, 1), precache_count(false, 3, 1), precache_count(false, -1, 1)), (1, 3, 0));
        assert_eq!((measure_ahead(true), measure_ahead(false)), (3, 0));
    }

    #[test]
    fn explicit_skipped_only_with_next() {
        assert_eq!(arrival(true, true, true, true, false), Onto::Skip);
        assert_eq!(arrival(true, true, true, true, true), Onto::Skip, "even looping: the setting wins");
        assert_eq!(arrival(true, true, true, false, false), Onto::Song, "the last song plays");
        assert_eq!(arrival(true, false, true, true, false), Onto::Song, "setting off");
        assert_eq!(arrival(true, true, false, true, false), Onto::Song);
        assert_eq!(arrival(true, false, false, true, true), Onto::Loop);
        assert_eq!(arrival(false, true, true, true, true), Onto::Song, "onto nothing");
    }

    #[test]
    fn refill_starts_once_near_end() {
        assert!(refillable(true, false, 0, true));
        assert!(!refillable(false, false, 0, true), "nothing playing");
        assert!(!refillable(true, true, 0, true), "a radio stream");
        assert!(!refillable(true, false, 2, true), "repeat all");
        assert!(!refillable(true, false, 1, true), "repeat one");
        assert!(!refillable(true, false, 0, false), "setting off");
        let mut f = Refill::new();
        assert!(!f.start(true, FILL_AHEAD + 1, Some(1)), "plenty left");
        assert!(!f.start(false, 0, Some(1)), "not refillable");
        assert!(f.start(true, FILL_AHEAD, Some(1)), "the end in sight: fetch now");
        assert!(!f.start(true, 0, Some(1)), "already on the wire");
        assert!(f.arrived(5, Some(1)), "the end is where it was, however far the user is from it");
        assert!(!f.landed(Some(2), true, 0), "no next was waiting");
        assert!(!f.in_flight());
        assert!(f.start(true, 0, Some(1)));
        assert!(!f.arrived(0, Some(1)), "nothing came");
        assert!(f.start(true, 0, Some(1)), "and the next move may try again");
        assert!(!f.arrived(3, Some(3)), "the queue was given songs meanwhile");
        assert!(f.start(true, 1, Some(1)));
        assert!(!f.arrived(3, None), "the queue was emptied");
    }

    #[test]
    fn waiting_next() {
        let mut f = Refill::new();
        assert!(!f.next(false, true, Some(4), 0));
        assert!(f.start(true, 0, Some(4)));
        assert!(f.skip_waiting(Some(4), 10), "a screen may show the skip on its way");
        assert!(f.arrived(3, Some(4)));
        assert!(f.landed(Some(4), true, 800));
        // Moved on meanwhile (previous, a jump): the press is dropped.
        assert!(!f.next(false, true, Some(4), 1_000));
        assert!(f.start(true, 0, Some(4)));
        assert!(!f.skip_waiting(Some(5), 1_010));
        assert!(f.arrived(3, Some(4)));
        assert!(!f.landed(Some(5), true, 1_200));
        // A press while a fetch is out waits for it; a press with a song after clears the waiting one.
        assert!(f.start(true, 1, Some(6)));
        assert!(!f.next(false, true, Some(7), 2_000));
        assert!(!f.start(true, 0, Some(6)), "one on the wire");
        assert!(f.next(true, true, Some(7), 2_100), "there is a next now");
        assert!(f.arrived(2, Some(6)));
        assert!(!f.landed(Some(7), true, 2_200), "that press was already taken");
        // A queue that cannot be refilled remembers nothing.
        assert!(!f.next(false, false, Some(8), 3_000));
        assert!(!f.skip_waiting(Some(8), 3_000));
        assert!(f.start(true, 0, Some(8)));
        assert!(f.arrived(1, Some(8)));
        assert!(!f.landed(Some(8), true, 3_100));
        // Nothing came: the waiting next goes with the fetch.
        assert!(!f.next(false, true, Some(6), 4_000));
        assert!(f.start(true, 0, Some(6)));
        assert!(!f.arrived(0, Some(6)));
        assert!(f.start(true, 0, Some(6)));
        assert!(f.arrived(1, Some(6)));
        assert!(!f.landed(Some(6), true, 4_100));
        // Landed with nowhere to go (the songs went in but the player cannot step): no skip.
        assert!(!f.next(false, true, Some(9), 5_000));
        assert!(f.start(true, 0, Some(9)));
        assert!(f.arrived(1, Some(9)));
        assert!(!f.landed(Some(9), false, 5_100));

        // Waiting next expires and counts once.
        // Songs took 4.6 s: they go in, no skip.
        let mut f = Refill::new();
        assert!(!f.next(false, true, Some(10), 10_000));
        assert!(f.start(true, 0, Some(10)));
        assert!(f.skip_waiting(Some(10), 10_000 + NEXT_KEPT_MS));
        assert!(!f.skip_waiting(Some(10), 10_001 + NEXT_KEPT_MS), "the wait shows no longer than it holds");
        assert!(f.arrived(15, Some(10)), "the songs still go in");
        assert!(!f.landed(Some(10), true, 14_600), "a press 4.6 s old is not taken");
        // A fast answer: taken.
        assert!(!f.next(false, true, Some(2), 20_000));
        assert!(f.start(true, 0, Some(2)));
        assert!(f.arrived(15, Some(2)));
        assert!(f.landed(Some(2), true, 20_000 + NEXT_KEPT_MS), "at the edge of the window still");
        // Mashed: six presses, one skip, the window counted from the last of them.
        assert!(!f.next(false, true, Some(11), 30_000));
        assert!(f.start(true, 0, Some(11)));
        for k in 1..6 {
            assert!(!f.next(false, true, Some(11), 30_000 + k * 150));
            assert!(!f.start(true, 0, Some(11)), "one fetch for all of them");
        }
        assert!(f.arrived(15, Some(11)));
        assert!(f.landed(Some(11), true, 30_750 + NEXT_KEPT_MS - 1), "within the window of the last press");
        assert!(!f.landed(Some(11), true, 30_750 + NEXT_KEPT_MS - 1), "and only once");
        // Mashed and then the answer was slow: nothing.
        assert!(!f.next(false, true, Some(12), 40_000));
        assert!(f.start(true, 0, Some(12)));
        assert!(!f.next(false, true, Some(12), 40_300));
        assert!(f.arrived(15, Some(12)));
        assert!(!f.landed(Some(12), true, 40_301 + NEXT_KEPT_MS));
    }

    #[test]
    fn previous_and_repeat() {
        assert!(previous_restarts(5_000, true, false));
        assert!(!previous_restarts(1_000, true, false));
        assert!(!previous_restarts(5_000, true, true), "always skips");
        assert!(previous_restarts(5_000, false, true), "nothing before to skip to: restart");
        assert_eq!((next_repeat(0), next_repeat(2), next_repeat(1)), (2, 1, 0));
    }
}
