//! The audible song and position for the seek bar and now-playing page. During a transition the player
//! runs ahead of what is audible; this turns the engine's sparse [`Heard`] readings into a position
//! that advances in real time, switches to the next stream at takeover (`until_us`), and does not flash
//! back to the old one between the engine releasing the mix and the player moving on. Streams are told
//! apart by serial, so the same song twice in a row is two streams.

use crate::engine::Heard;

/// The player's own state when asked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerNow {
    pub now_ms: i64,
    pub playing: bool,
    /// The serial of the stream the player is on.
    pub on: Option<u64>,
    pub position_ms: i64,
}

/// The audible song (queue index; `None` when the player's own position is right), position ms, and
/// whether the song changed since the last call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub index: Option<usize>,
    pub ms: i64,
    pub changed: bool,
}

/// A stream to look up: by serial, or the stream after a serial (the incoming side of a mix, which may
/// not be announced yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamAt {
    Serial(u64),
    After(u64),
}

/// Finds a stream's queue index and length in ms.
pub type Lookup<'a> = &'a dyn Fn(StreamAt) -> Option<(usize, i64)>;

/// The queue's songs for [`Playhead`], and the audible stream. Called every frame; allocates nothing.
#[derive(Debug, Default)]
pub struct HeardTracker {
    queue: Vec<(String, i64)>,
    ear: Ear,
}

#[derive(Debug, Default)]
struct Ear {
    /// (serial mixed out of, ms, at ms, the player's stream then): the stream after it stays shown after
    /// the engine lets go.
    carry: Option<(u64, i64, i64, Option<u64>)>,
    /// The serial mixed out of whose next stream the player has reached (its mix is done).
    consumed: Option<u64>,
    /// The audible stream at the last call.
    before: Option<StreamAt>,
    /// (takeover µs, entry µs) of the transition already crossed: never cross back.
    crossed: Option<(i64, i64)>,
}

/// How long the page stays carried on the new stream after the engine released the mix, waiting for
/// the player (normally milliseconds).
const CARRY_GRACE_MS: i64 = 1_000;

fn duration(find: Lookup, s: StreamAt) -> i64 {
    find(s).map_or(i64::MAX, |(_, ms)| ms)
}

/// Position in the stream after `from` `ms_in` after takeover.
fn into_next(find: Lookup, h: &Heard, from: u64, ms_in: i64) -> i64 {
    (h.next_from_us / 1000 + (ms_in as f64 * h.next_rate as f64) as i64).clamp(0, duration(find, StreamAt::After(from)))
}

impl HeardTracker {
    pub fn new() -> HeardTracker {
        HeardTracker::default()
    }

    /// The queue as (id, length ms), for [`HeardTracker::differs`].
    pub fn set_queue<I: IntoIterator<Item = (String, i64)>>(&mut self, songs: I) {
        self.queue.clear();
        self.queue.extend(songs);
    }

    /// The audible stream given the engine's `h` and the player's `p`; `find` places streams in the queue.
    pub fn at(&mut self, h: &Heard, p: PlayerNow, find: Lookup) -> Seen {
        self.ear.at(find, h, p)
    }

    /// Whether queue entries `heard` and `shown` are different songs (false if `shown` is out of range).
    pub fn differs(&self, heard: usize, shown: usize) -> bool {
        match self.queue.get(shown) {
            None => false,
            Some((id, _)) => self.queue.get(heard).is_none_or(|(h, _)| h != id),
        }
    }
}

/// The seek bar position. Holds while the page is a song behind the audible one, and runs on while
/// nothing can be asked (reconnecting).
///
/// Within a song it never steps back or leaps: a reading up to [`GLIDE_MS`] behind is approached at half
/// speed (paused: it stands), up to [`GLIDE_MS`] ahead at double speed. User jumps ([`Playhead::jumped`]),
/// song changes and the first reading after [`STALE_MS`] unasked are shown as they are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Playhead {
    ms: i64,
    at_ms: i64,
    /// Queue index the position was shown for (the page's, else the audible song's).
    song: Option<usize>,
    /// Show the next reading as is, even behind.
    jumped: bool,
}

/// Largest difference glided rather than jumped.
pub const GLIDE_MS: i64 = 2_000;
/// Most elapsed time one call counts towards a glide.
pub const GLIDE_STEP_MS: i64 = 250;
/// A position shown longer ago than this while playing is not glided from (the bar was hidden).
pub const STALE_MS: i64 = 2_000;
/// Longest run-on past the last reading ([`screen_place`], [`Playhead::run_on`]).
pub const RUN_ON_MS: i64 = 2_000;
/// Readings older than this (while playing) should be refreshed.
pub const LOOK_AFTER_MS: i64 = 1_000;
/// A controller position further than this from the engine's reading has drifted and must be re-sent.
pub const DRIFT_MS: i64 = 2_000;

/// The bar position from a reading `age_ms` old at `speed` (run on at most [`RUN_ON_MS`]), and whether
/// to ask for a fresh reading ([`LOOK_AFTER_MS`]). Paused, the reading stands.
pub fn screen_place(reading_ms: i64, age_ms: i64, speed: f32, playing: bool) -> (i64, bool) {
    if !playing {
        return (reading_ms.max(0), false);
    }
    let age = age_ms.max(0);
    let run = (age.min(RUN_ON_MS) as f64 * speed.max(0.0) as f64) as i64;
    ((reading_ms + run).max(0), age >= LOOK_AFTER_MS)
}

/// Whether a controller's position `word_ms` drifted from the engine's `engine_ms` (negative: nothing to
/// compare; kept as a sentinel for the JNI caller).
pub fn drifted(word_ms: i64, engine_ms: i64) -> bool {
    engine_ms >= 0 && (word_ms - engine_ms).abs() > DRIFT_MS
}

impl Playhead {
    pub const fn new() -> Self {
        Playhead { ms: 0, at_ms: 0, song: None, jumped: false }
    }

    /// The bar position for `seen` while the page shows queue index `shown`.
    pub fn show(&mut self, t: &HeardTracker, seen: Seen, shown: Option<usize>, now_ms: i64) -> i64 {
        self.show_for(t, seen, shown, now_ms, None, 0, true)
    }

    /// The user asked for a position (seek, lyric tap): show the next reading as is.
    pub fn jumped(&mut self) {
        self.jumped = true;
    }

    /// [`Playhead::show`] with the player's song `on` and position. Holds while the page is a song behind
    /// the audible one, except when the page shows the player's own song (e.g. reopened mid-mix): then
    /// it shows the player's position.
    #[allow(clippy::too_many_arguments)]
    pub fn show_for(&mut self, t: &HeardTracker, seen: Seen, shown: Option<usize>, now_ms: i64, on: Option<usize>, position_ms: i64, playing: bool) -> i64 {
        if let (Some(h), Some(s)) = (seen.index, shown) {
            if t.differs(h, s) {
                if on.is_none_or(|o| t.differs(o, s)) {
                    return self.ms;
                }
                return self.put(now_ms, shown, position_ms.max(0), playing);
            }
        }
        self.put(now_ms, shown.or(seen.index), seen.ms, playing)
    }

    fn put(&mut self, now_ms: i64, song: Option<usize>, ms: i64, playing: bool) -> i64 {
        let stale = playing && now_ms - self.at_ms > STALE_MS;
        let same = !self.jumped && !stale && self.at_ms > 0 && self.song == song;
        let step = (now_ms - self.at_ms).max(0);
        let run = self.ms + step;
        let shown = if same && ms < self.ms && self.ms - ms <= GLIDE_MS {
            // Behind: half the pace, never back.
            if playing { ms.max(self.ms + step.min(GLIDE_STEP_MS) / 2) } else { self.ms }
        } else if same && playing && ms > run && ms - run <= GLIDE_MS {
            // Ahead: twice the pace, never past the reading.
            ms.min(run + step.min(GLIDE_STEP_MS))
        } else {
            ms
        };
        self.jumped = false;
        self.song = song;
        self.ms = shown;
        self.at_ms = now_ms;
        shown
    }

    /// The last position run on to `now_ms` (at most [`RUN_ON_MS`]) when nothing can be asked.
    pub fn run_on(&self, now_ms: i64, playing: bool) -> i64 {
        let elapsed = if playing && self.at_ms > 0 { (now_ms - self.at_ms).clamp(0, RUN_ON_MS) } else { 0 };
        (self.ms + elapsed).max(0)
    }
}

/// The duration to show: the audible song's (s) when it is not the player's, else the player's
/// measurement, else the tagged length.
pub fn shown_duration_ms(heard_s: Option<i64>, player_ms: i64, tagged_ms: i64) -> i64 {
    match heard_s {
        Some(s) => s * 1000,
        None if player_ms > 0 => player_ms,
        None => tagged_ms,
    }
}

impl Ear {
    /// Already crossed this takeover: a fresh reading landing slightly behind must not switch back.
    fn over(&self, h: &Heard, until_us: i64) -> bool {
        self.crossed == Some((until_us, h.next_from_us))
    }

    fn at(&mut self, q: Lookup, h: &Heard, p: PlayerNow) -> Seen {
        let next = h.from.map(StreamAt::After);
        let result: Option<(StreamAt, i64)> = if let Some(id) = h.id {
            let held = StreamAt::Serial(id);
            let since = if p.playing { p.now_ms - h.at_ms } else { 0 };
            let ms = h.us / 1000 + since;
            let until = h.until_us / 1000;
            match h.from {
                Some(from) if ms >= until || self.over(h, h.until_us) => Some((StreamAt::After(from), into_next(q, h, from, (ms - until).max(0)))),
                Some(_) => Some((held, ms.clamp(0, duration(q, held)))),
                None if ms < until => Some((held, ms.clamp(0, duration(q, held)))),
                None => None,
            }
        } else if let (Some(from), Some(on)) = (h.from, p.on) {
            // No hold: the player's clock is right, but past the takeover the next stream is audible.
            let until = h.audible_us / 1000;
            (on == from && self.consumed != Some(from) && (p.position_ms >= until || self.over(h, h.audible_us)))
                .then(|| (StreamAt::After(from), into_next(q, h, from, (p.position_ms - until).max(0))))
        } else {
            None
        };
        match (result, next) {
            (Some((shown, _)), Some(n)) if shown == n => {
                self.crossed = Some((if h.id.is_some() { h.until_us } else { h.audible_us }, h.next_from_us));
            }
            (_, None) => self.crossed = None,
            _ => {}
        }
        // Stay on the next stream until the player has left the old one (the engine may release first).
        let shown = result.or_else(|| {
            let (from, ms, at, on_then) = self.carry?;
            let left = p.on == h.from || h.from.is_none() && p.on == on_then && p.now_ms - at < CARRY_GRACE_MS;
            (left && self.consumed != Some(from)).then(|| {
                let since = if p.playing { p.now_ms - at } else { 0 };
                let s = StreamAt::After(from);
                (s, (ms + since).clamp(0, duration(q, s)))
            })
        });
        let (shown_at, shown_ms) = shown.map_or((None, p.position_ms), |(s, ms)| (Some(s), ms));
        let index = shown_at.and_then(q).map(|(i, _)| i);
        let changed = self.before != shown_at;
        self.before = shown_at;
        // The player is on a stream after `from`: its mix is done.
        let reached = |from: u64| p.on.is_some_and(|on| on > from);
        if let Some(from) = h.from.filter(|&f| shown_at == Some(StreamAt::After(f)) && !reached(f)) {
            self.carry = Some((from, shown_ms, p.now_ms, p.on));
        }
        if let Some(from) = h.from.filter(|&f| reached(f)) {
            self.consumed = Some(from);
            self.carry = None;
        }
        Seen { index, ms: shown_ms, changed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: usize = 0;
    const B: usize = 1;

    impl Seen {
        fn seen(self) -> Option<(usize, i64)> {
            self.index.map(|i| (i, self.ms))
        }
    }

    /// Streams: `a` (serial 1, queue index 0, 200 s) then `b` (serial 2, index 1, 180 s).
    fn find(s: StreamAt) -> Option<(usize, i64)> {
        match s {
            StreamAt::Serial(1) => Some((A, 200_000)),
            StreamAt::Serial(2) | StreamAt::After(1) => Some((B, 180_000)),
            _ => None,
        }
    }

    impl HeardTracker {
        fn ab(&mut self, h: &Heard, p: PlayerNow) -> Seen {
            self.at(h, p, &find)
        }
    }

    fn tracker() -> HeardTracker {
        let mut t = HeardTracker::new();
        t.set_queue([("a".to_string(), 200_000), ("b".to_string(), 180_000)]);
        t
    }

    /// Held on the last seconds of `a`, which mix into `b` from 5 s in; the player is already on `b`.
    fn holding(us: i64, at_ms: i64) -> Heard {
        Heard { id: Some(1), us, at_ms, until_us: 194_000_000, mixing: false, next_from_us: 5_000_000, next_rate: 1.0, from: Some(1), audible_us: 194_000_000 }
    }

    fn now(ms: i64, on: &str, pos: i64) -> PlayerNow {
        PlayerNow { now_ms: ms, playing: true, on: Some(if on == "a" { 1 } else { 2 }), position_ms: pos }
    }

    #[test]
    fn same_song_twice_is_two_streams() {
        let mut t = HeardTracker::new();
        // `a` queued twice: streams 1 (index 0) and 2 (index 1).
        let find = |s: StreamAt| match s {
            StreamAt::Serial(1) => Some((0, 200_000)),
            StreamAt::Serial(2) | StreamAt::After(1) => Some((1, 200_000)),
            _ => None,
        };
        let p = |ms, on| PlayerNow { now_ms: ms, playing: true, on: Some(on), position_ms: 0 };
        assert_eq!(t.at(&holding(190_000_000, 0), p(1_000, 2), &find).index, Some(0), "the held first copy");
        assert_eq!(t.at(&holding(190_000_000, 0), p(5_000, 2), &find).index, Some(1), "the second copy after takeover");
    }

    #[test]
    fn held_ending_then_next_song() {
        let mut t = tracker();
        let s = t.ab(&holding(190_000_000, 1_000), now(3_000, "b", 800));
        assert_eq!((s.index, s.ms), (Some(A), 192_000), "the held ending runs on");
        assert!(s.changed);
        assert!(!t.ab(&holding(190_000_000, 1_000), now(3_100, "b", 900)).changed, "same song, no change");
        // 5.5 s since the reading: 195.5 s into a, 1.5 s past the audible point, so 6.5 s into b.
        let s = t.ab(&holding(190_000_000, 1_000), now(6_500, "b", 900));
        assert_eq!((s.index, s.ms), (Some(B), 6_500));
        assert!(s.changed);
        // A stretched mix runs into b at b's rate.
        let h = Heard { next_rate: 0.5, ..holding(194_000_000, 0) };
        assert_eq!(tracker().ab(&h, now(2_000, "b", 0)).seen(), Some((B, 6_000)));
    }

    #[test]
    fn no_flash_back_before_player_moves_on() {
        let mut t = tracker();
        // Heard on b already, the player not yet moved on from a.
        assert_eq!(t.ab(&holding(195_000_000, 0), now(0, "a", 195_000)).seen(), Some((B, 6_000)));
        // The engine let go (no hold any more) but the player is still on a: the page stays on b, moving.
        let released = Heard { id: None, from: None, ..holding(0, 0) };
        assert_eq!(t.ab(&released, now(500, "a", 195_500)).seen(), Some((B, 6_500)));
        // The player reached b: its own word is the truth again.
        let s = t.ab(&Heard { id: None, ..holding(0, 0) }, now(2_100, "b", 8_100));
        assert_eq!((s.index, s.ms), (None, 8_100), "the player's own position");
        assert!(s.changed);
    }

    #[test]
    fn late_readings_stay_on_next_song() {
        let mut t = tracker();
        // Run on from a reading at 193.9 s, the page crosses into b at 194 s.
        assert_eq!(t.ab(&holding(193_900_000, 0), now(150, "b", 0)).seen(), Some((B, 5_050)));
        // The next reading, taken a little later, finds the ending 30 ms short of the takeover (an
        // output's clock read in steps, or corrected): the page stays on b, at the point it entered.
        let s = t.ab(&holding(193_970_000, 200), now(200, "b", 0));
        assert_eq!(s.seen(), Some((B, 5_000)));
        assert!(!s.changed, "no second change of song");
        // And moves on with it from there.
        assert_eq!(t.ab(&holding(193_970_000, 200), now(300, "b", 0)).seen(), Some((B, 5_070)));
        // The player's own clock stepping back 20 ms does not cross back either.
        let mut t = tracker();
        let direct = Heard { id: None, ..holding(0, 0) };
        assert_eq!(t.ab(&direct, now(0, "a", 194_010)).seen(), Some((B, 5_010)));
        let s = t.ab(&direct, now(16, "a", 193_990));
        assert_eq!(s.seen(), Some((B, 5_000)));
        assert!(!s.changed);
    }

    #[test]
    fn released_mix_carries_briefly() {
        let mut t = tracker();
        let direct = Heard { id: None, ..holding(0, 0) };
        assert_eq!(t.ab(&direct, now(0, "a", 196_000)).seen(), Some((B, 7_000)));
        // The engine lets go of the mix and forgets which song it left, a moment before the player
        // moves on: the page stays on b.
        let released = Heard { from: None, ..direct };
        let s = t.ab(&released, now(40, "a", 196_040));
        assert_eq!(s.seen(), Some((B, 7_040)));
        assert!(!s.changed);
        let s = t.ab(&released, now(60, "b", 7_060));
        assert_eq!((s.index, s.ms), (None, 7_060), "the player's own word once it is on b");
        // Not for long, though: a player still on a a second later has been sent back there.
        let mut t = tracker();
        t.ab(&direct, now(0, "a", 196_000));
        assert_eq!(t.ab(&released, now(1_500, "a", 10_000)).seen(), None);
    }

    #[test]
    fn no_hold_uses_player_clock() {
        let mut t = tracker();
        let direct = Heard { id: None, ..holding(0, 0) };
        assert_eq!(t.ab(&direct, now(0, "a", 190_000)).seen(), None, "before the mix is audible");
        assert_eq!(t.ab(&direct, now(0, "a", 196_000)).seen(), Some((B, 7_000)));
        // After the player moved on to b, a later visit to a is not mistaken for the mix.
        t.ab(&direct, now(0, "b", 7_000));
        assert_eq!(t.ab(&direct, now(0, "a", 196_000)).seen(), None);
    }

    #[test]
    fn paused_stands_and_clamps() {
        let mut t = tracker();
        let p = PlayerNow { playing: false, ..now(60_000, "b", 0) };
        assert_eq!(t.ab(&holding(190_000_000, 0), p).seen(), Some((A, 190_000)));
        let late = Heard { until_us: i64::MAX, ..holding(199_000_000, 0) };
        assert_eq!(t.ab(&late, now(60_000, "b", 0)).seen(), Some((A, 200_000)), "clamped to the song's length");
    }

    #[test]
    fn bar_holds_while_page_behind() {
        let mut t = HeardTracker::new();
        t.set_queue([("a".to_string(), 200_000), ("b".to_string(), 180_000), ("a".to_string(), 200_000)]);
        let mut p = Playhead::new();
        let seen = |index, ms| Seen { index, ms, changed: false };
        assert_eq!(p.show(&t, seen(None, 5_000), Some(A), 100), 5_000, "the player's own word");
        assert_eq!(p.show(&t, seen(Some(A), 6_000), Some(A), 1_100), 6_000, "heard on the song shown");
        assert_eq!(p.show(&t, seen(Some(2), 7_000), Some(A), 2_100), 7_000, "another copy of the same song is the same song");
        assert_eq!(p.show(&t, seen(Some(B), 1_000), Some(A), 2_200), 7_000, "the ear moved to b, the page is still on a: hold");
        assert_eq!(p.show(&t, seen(Some(B), 1_100), None, 2_300), 1_100, "a page showing nothing does not hold");
        assert_eq!(p.show(&t, seen(Some(B), 1_200), Some(9), 2_400), 1_200, "nor one the queue does not have");
        assert_eq!(p.run_on(3_400, true), 2_200, "reconnecting while playing: runs on from when it was taken");
        assert_eq!(p.run_on(3_400, false), 1_200, "paused: stands");
        assert_eq!(Playhead::new().run_on(5_000, true), 0, "never shown: zero, not the clock");
    }

    #[test]
    fn shown_duration() {
        assert_eq!(shown_duration_ms(Some(180), 200_000, 199_000), 180_000, "the ear is a song behind the player");
        assert_eq!(shown_duration_ms(None, 200_123, 199_000), 200_123, "measured");
        assert_eq!(shown_duration_ms(None, 0, 199_000), 199_000, "not measured yet");
        assert_eq!(shown_duration_ms(None, i64::MIN + 1, 199_000), 199_000, "unknown (media3's TIME_UNSET)");
    }

    #[test]
    fn page_on_player_song_mid_mix_shows_player_position() {
        let mut t = HeardTracker::new();
        t.set_queue([("a".to_string(), 200_000), ("b".to_string(), 180_000)]);
        let mut p = Playhead::new();
        let seen = |index, ms| Seen { index, ms, changed: false };
        // The bar last drawn on a, 150 s in; the screen goes away.
        assert_eq!(p.show_for(&t, seen(Some(A), 150_000), Some(A), 1_000, Some(A), 150_000, true), 150_000);
        // It comes back 45 s later mid-mix: the ear on a's ending, the player on b and 2 s into it, and the
        // page put on b (the player's song). The bar shows b's place, not a's held from before.
        assert_eq!(p.show_for(&t, seen(Some(A), 195_000), Some(B), 46_000, Some(B), 2_000, true), 2_000);
        assert_eq!(p.show_for(&t, seen(Some(A), 195_100), Some(B), 46_100, Some(B), 2_100, true), 2_100, "and runs on with it");
        // The page catching up to the ear holds for a moment only, as before.
        assert_eq!(p.show_for(&t, seen(Some(A), 195_200), Some(A), 46_200, Some(B), 2_200, true), 195_200);
        assert_eq!(p.show_for(&t, seen(Some(B), 1_000), Some(A), 46_300, Some(B), 1_000, true), 195_200, "the ear moved to b, the page still on a: held");
        // A page on a song that is neither the ear's nor the player's holds too.
        let mut q = Playhead::new();
        q.show_for(&t, seen(Some(A), 10_000), Some(A), 1_000, Some(A), 10_000, true);
        assert_eq!(q.show_for(&t, seen(Some(A), 50_000), Some(B), 41_000, Some(A), 50_000, true), 10_000);
    }

    /// A reading 250 ms behind at mix start: the bar never steps back and catches up within two steps.
    #[test]
    fn position_never_steps_back_at_mix_start() {
        let t = tracker();
        let mut p = Playhead::new();
        let player = |index, ms| Seen { index, ms, changed: false };
        let mut last = 0;
        let mut caught = None;
        for now in (1_000..6_000).step_by(16) {
            let reading = 189_000 + now - if now >= 3_000 { 250 } else { 0 };
            let shown = p.show_for(&t, player(None, reading), Some(A), now, Some(A), reading, true);
            assert!(shown >= last, "back from {last} to {shown} at {now} ms (reading {reading})");
            assert!(shown - reading <= 250, "never further ahead than the step");
            if now >= 3_000 && shown == reading && caught.is_none() {
                caught = Some(now);
            }
            last = shown;
        }
        let caught = caught.expect("caught up with the reading");
        assert!(caught - 3_000 <= 520, "caught up within twice the step, at {caught}");
        // Same with engine readings of a held ending stepping back 120 ms.
        let mut t = tracker();
        let mut p = Playhead::new();
        let mut last = 0;
        for (i, at) in (60_000..61_000).step_by(16).enumerate() {
            let us = 190_000_000 + (at - 60_000) * 1000 - if i >= 30 { 120_000 } else { 0 };
            let seen = t.ab(&holding(us, at), now(at, "b", 0));
            let shown = p.show_for(&t, seen, Some(A), at, Some(B), 0, true);
            assert!(shown >= last, "back from {last} to {shown} at {at} ms");
            last = shown;
        }
        assert_eq!(p.show_for(&t, player(None, 5_300), Some(B), 61_100, Some(B), 5_300, true), 5_300);
    }

    #[test]
    fn seek_back_shown_and_paused_stands() {
        let t = tracker();
        let mut p = Playhead::new();
        let player = |ms| Seen { index: None, ms, changed: false };
        assert_eq!(p.show_for(&t, player(50_000), Some(A), 1_000, Some(A), 50_000, true), 50_000);
        p.jumped();
        assert_eq!(p.show_for(&t, player(49_200), Some(A), 1_016, Some(A), 49_200, true), 49_200);
        assert_eq!(p.show_for(&t, player(49_216), Some(A), 1_032, Some(A), 49_216, true), 49_216);
        // Paused: a reading slightly behind leaves it standing.
        assert_eq!(p.show_for(&t, player(49_100), Some(A), 1_100, Some(A), 49_100, false), 49_216);
        assert_eq!(p.show_for(&t, player(49_100), Some(A), 9_000, Some(A), 49_100, false), 49_216);
        // Further back than a glide: a jump.
        assert_eq!(p.show_for(&t, player(30_000), Some(A), 9_016, Some(A), 30_000, true), 30_000);
        // A glide counts at most one step of the wait.
        assert_eq!(p.show_for(&t, player(29_900), Some(A), 10_500, Some(A), 29_900, true), 30_000 + GLIDE_STEP_MS / 2);
        // Stale: the reading as is.
        assert_eq!(p.show_for(&t, player(29_900), Some(A), 60_000, Some(A), 29_900, true), 29_900);
    }

    /// The audible reading takes over 400 ms ahead of the player's, then hands back: no leap, no step back.
    #[test]
    fn handover_neither_leaps_nor_steps_back() {
        let t = tracker();
        let mut p = Playhead::new();
        let mut last = 0;
        let mut caught = None;
        for now in (60_000..70_000).step_by(16) {
            let player = 180_000 + now - 60_000;
            let seen = if (61_000..69_000).contains(&now) { Seen { index: Some(A), ms: player + 400, changed: false } } else { Seen { index: None, ms: player, changed: false } };
            let shown = p.show_for(&t, seen, Some(A), now, Some(A), player, true);
            assert!(shown >= last, "back from {last} to {shown} at {now} ms");
            assert!(last == 0 || shown - last <= 2 * 16 + 1, "leapt from {last} to {shown} at {now} ms");
            if now >= 61_000 && shown == seen.ms && caught.is_none() {
                caught = Some(now);
            }
            last = shown;
        }
        assert!(caught.is_some_and(|c| c - 61_000 <= 420), "caught up with the ear within the step: {caught:?}");
        assert_eq!(last, 180_000 + 9_984, "back on the player's clock by the end");
    }
}

/// The seek bar after the app was hidden and brought back (regression: the bar sat at the song's end
/// with 14 s left). The simulated engine reads its output every `wake_ms` and shortly after a look.
#[cfg(test)]
mod away {
    use super::*;

    const SONG_MS: i64 = 600_000;
    /// The song's place at the wall clock's 0.
    const FROM_MS: i64 = 100_000;
    const FRAME_MS: i64 = 16;
    /// Delay of a requested look.
    const LOOK_TAKES_MS: i64 = 2;
    const A: usize = 0;

    fn truth(now: i64) -> i64 {
        (FROM_MS + now).min(SONG_MS)
    }

    struct Engine {
        wake_ms: i64,
        /// (position, taken at).
        reading: (i64, i64),
        next_wake: i64,
        look_at: Option<i64>,
        /// Error of the next reading only.
        off_next: i64,
    }

    impl Engine {
        fn new(wake_ms: i64) -> Engine {
            Engine { wake_ms, reading: (FROM_MS, 0), next_wake: wake_ms, look_at: None, off_next: 0 }
        }

        /// Runs every wake due by `now`.
        fn to(&mut self, now: i64) {
            loop {
                let due = self.look_at.map_or(self.next_wake, |l| l.min(self.next_wake));
                if due > now {
                    return;
                }
                self.reading = (truth(due) + std::mem::take(&mut self.off_next), due);
                self.next_wake = due + self.wake_ms;
                self.look_at = None;
            }
        }

        fn look(&mut self, now: i64) {
            self.look_at.get_or_insert(now + LOOK_TAKES_MS);
        }

        /// [`screen_place`], requesting a look when the reading is old.
        fn screen(&mut self, now: i64) -> i64 {
            self.to(now);
            let (ms, look) = screen_place(self.reading.0, now - self.reading.1, 1.0, true);
            if look {
                self.look(now);
            }
            ms
        }
    }

    fn clock() -> HeardTracker {
        let mut t = HeardTracker::new();
        t.set_queue([("a".to_string(), SONG_MS)]);
        t
    }

    fn show(p: &mut Playhead, t: &HeardTracker, now: i64, ms: i64) -> i64 {
        p.show_for(t, Seen { index: None, ms, changed: false }, Some(A), now, Some(A), ms, true)
    }

    /// 1 s on screen, `away_ms` hidden, then on screen to the end with a look on return; the first reading
    /// after return is off by `off_ms`. Returns (time, shown, true) per frame after the return.
    fn fixed(wake_ms: i64, away_ms: i64, off_ms: i64) -> Vec<(i64, i64, i64)> {
        let (t, mut p, mut e) = (clock(), Playhead::new(), Engine::new(wake_ms));
        let mut now = 0;
        while now < 1_000 {
            show(&mut p, &t, now, e.screen(now));
            now += FRAME_MS;
        }
        let back = now + away_ms;
        e.to(back - 1);
        e.off_next = off_ms;
        e.look(back); // as MainActivity.onStart does
        let mut frames = Vec::new();
        now = back + FRAME_MS;
        while truth(now) < SONG_MS {
            let shown = show(&mut p, &t, now, e.screen(now));
            frames.push((now, shown, truth(now)));
            now += FRAME_MS;
        }
        frames
    }


    const OFFLOADED: i64 = 180_000;
    const DEEP_BUFFER: i64 = 8_000;
    const MIX_HOLD: i64 = 250;
    const MINUTES: i64 = 5 * 60_000;
    const PATHS: [(&str, i64); 3] = [("offloaded", OFFLOADED), ("deep buffer", DEEP_BUFFER), ("mix hold", MIX_HOLD)];

    /// Frames showing the end while more than a second remained.
    fn at_the_end(frames: &[(i64, i64, i64)]) -> i64 {
        frames.iter().filter(|(_, shown, truth)| *shown >= SONG_MS && *truth < SONG_MS - 1_000).count() as i64
    }

    #[test]
    fn right_after_return() {
        for (what, wake) in PATHS {
            for away in [10_000, MINUTES, 30 * 60_000 / 6] {
                let frames = fixed(wake, away, 0);
                let (_, first, truth) = frames[0];
                assert!((first - truth).abs() <= FRAME_MS + LOOK_TAKES_MS, "{what}, away {away} ms: first frame {first}, the song at {truth}");
                for &(now, shown, truth) in &frames {
                    assert!((shown - truth).abs() <= 2 * FRAME_MS, "{what}, away {away} ms: {shown} shown at {now} with the song at {truth}");
                }
                assert_eq!(at_the_end(&frames), 0, "{what}");
            }
            // A first reading 14 s ahead with 14 s left is corrected by the next look.
            let away = SONG_MS - FROM_MS - 1_000 - 14_000 - FRAME_MS;
            let frames = fixed(wake, away, 14_000);
            let wrong: Vec<_> = frames.iter().filter(|(_, shown, truth)| (shown - truth).abs() > 2 * FRAME_MS).collect();
            let last = wrong.last().map_or(0, |w| w.0 - frames[0].0);
            assert!(last <= LOOK_AFTER_MS + FRAME_MS, "{what}: off for {last} ms after the return");
            assert!(at_the_end(&frames) * FRAME_MS <= LOOK_AFTER_MS + FRAME_MS, "{what}: at the end for {} frames", at_the_end(&frames));
        }
    }

    #[test]
    fn silent_engine_run_on_is_bounded() {
        let (ms, look) = screen_place(570_000, 60_000, 1.0, true);
        assert_eq!(ms, 570_000 + RUN_ON_MS);
        assert!(look, "and asks again");
        assert_eq!(screen_place(570_000, 500, 1.0, true), (570_500, false), "a fresh reading runs on as it is");
        assert_eq!(screen_place(570_000, 60_000, 1.0, false), (570_000, false), "paused, it stands");
        assert_eq!(screen_place(570_000, 1_000, 2.0, true), (572_000, true), "at the playing speed");
        let (t, mut p) = (clock(), Playhead::new());
        show(&mut p, &t, 1_000, 100_000);
        assert_eq!(p.run_on(1_500, true), 100_500);
        assert_eq!(p.run_on(1_000 + MINUTES, true), 100_000 + RUN_ON_MS, "not over the minutes the app was away");
    }

    #[test]
    fn drift_detection() {
        assert!(!drifted(586_000, 586_000 - DRIFT_MS));
        assert!(drifted(600_000, 586_000), "the S22's bar at the end with 14 s left");
        assert!(drifted(570_000, 586_000));
        assert!(!drifted(600_000, -1), "nothing to compare: another song, or a seek on its way");
    }
}
