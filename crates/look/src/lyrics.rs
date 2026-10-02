//! Synced lyrics timing: active line, line change duration, fill position within the line, when to wake
//! next and whether anything visible changed.
//!
//! A platform builds a [`LyricClock`] per lyrics and calls [`LyricClock::advance`] each frame or wake-up;
//! the answer packs into one `i64` ([`Step::pack`]) for FFI, without allocating. Text offsets are UTF-16
//! units.

use core::sync::atomic::{AtomicI64, Ordering::Relaxed};

/// Maximum line change duration (scroll and colour).
pub const GLIDE_MS: i32 = 620;
/// Minimum line change duration.
pub(crate) const MIN_GLIDE_MS: i32 = 160;
/// A change lasts this share of the gap to the next line.
const GLIDE_SHARE: f64 = 0.85;
/// Gap cap for the glide computation.
const LONGEST_GAP_MS: i64 = 10_000;

/// Wake bounds without the sweep; the max bounds how late a seek is noticed.
const WAKE_MIN_MS: i64 = 8;
const WAKE_MAX_MS: i64 = 500;
/// With the sweep, redraw every this many display frames.
pub(crate) const SWEEP_FRAMES: u32 = 2;
/// Minimum fill movement (characters) that triggers a redraw.
const SWEEP_STEP: f32 = 0.04;

/// Minimum duration of a sung word's rise.
pub const RISE_MIN_MS: i64 = 180;
/// Duration of a word settling back after it is sung.
pub const SETTLE_MS: i64 = 420;
/// Words held at least this long glow.
pub const HELD_MS: i64 = 900;
/// Glow fade-out after a held word ends.
pub const GLOW_FADE_MS: i64 = 500;
/// How long after its end a word still animates; the platform draws every frame meanwhile.
pub(crate) const MOTION_TAIL_MS: i64 = if SETTLE_MS > GLOW_FADE_MS { SETTLE_MS } else { GLOW_FADE_MS };

/// Estimated duration of a word of `units` UTF-16 units, for words whose end is unknown (an LRC line's
/// last word otherwise runs until the next line, possibly many seconds later).
pub fn word_ms_estimate(units: u32) -> i64 {
    (i64::from(units) * WORD_MS_PER_UNIT + WORD_MS_BASE).clamp(WORD_MS_MIN, WORD_MS_MAX)
}
const WORD_MS_PER_UNIT: i64 = 110;
const WORD_MS_BASE: i64 = 250;
const WORD_MS_MIN: i64 = 400;
const WORD_MS_MAX: i64 = 2_000;

/// Assumed duration of the last line when its end is unknown (matches nori-lyrics `build`).
pub(crate) const LAST_LINE_MS: i64 = 5_000;

/// One "Sooner"/"Later" nudge step.
pub(crate) const NUDGE_STEP_MS: i64 = 250;

/// After a tap, seeks land imprecisely (a frame or keyframe early, or the playhead is set back).
/// Positions up to this far before the tapped line still count as on it,
pub(crate) const LAND_EARLY_MS: i64 = 1_000;
/// the display never moves back by up to this much,
pub(crate) const LAND_HOLD_MS: i64 = 1_500;
/// for this long into the tapped line.
pub(crate) const LANDING_MS: i64 = 3_000;
/// `landing` value when no tap is being landed (an atomic, so no `Option`).
const NOT_LANDING: i64 = i64::MIN;

/// How long a manually scrolled view stays before returning to the active line.
pub const READING_MS: i64 = 4_000;

/// Strength of upcoming lines.
pub(crate) const NEXT_LINE: f32 = 0.35;
/// Strength of sung lines.
pub(crate) const PAST_LINE: f32 = NEXT_LINE * 0.55;
/// Strength of the active line's unsung words when filling word by word. During a fade, unsung words
/// use the lower of this and the line's strength so there is no step.
pub const UNSUNG: f32 = 0.55;

/// Maps line `at` of `old` to a line of `next` (line starts in ms) when replacing lyrics: the nearest
/// start (earlier on ties), or the same index (clamped) when untimed. 0 when `next` is empty.
pub fn matching_line(old: &[i64], at: i32, next: &[i64], timed: bool) -> i32 {
    if next.is_empty() {
        return 0;
    }
    let same = at.clamp(0, next.len() as i32 - 1);
    let i = at.clamp(0, (old.len() as i32 - 1).max(0)) as usize;
    let Some(&from) = old.get(i).filter(|&&t| timed && t >= 0) else { return same };
    let mut best = 0;
    for (k, &t) in next.iter().enumerate() {
        if (t - from).abs() < (next[best] - from).abs() {
            best = k;
        }
    }
    best as i32
}

/// Strength of `line` given the `active` line (-1 before the first). Unsynced lyrics are fully lit.
pub fn line_strength(synced: bool, line: i32, active: i32) -> f32 {
    if !synced || line == active {
        1.0
    } else if line < active {
        PAST_LINE
    } else {
        NEXT_LINE
    }
}

/// Whether the lyrics keep the screen on.
pub fn keeps_screen_on(asked: bool, shown: bool, playing: bool) -> bool {
    asked && shown && playing
}

/// Maximum lines, so `active` (up to one past the last) fits its [`Step::pack`] field.
pub(crate) const MAX_LINES: usize = (1 << ACTIVE_BITS) - 2;

/// A timed word or syllable; `start`/`end` index the line's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Word {
    pub start_ms: i64,
    pub end_ms: i64,
    pub start: u32,
    pub end: u32,
}

/// A timed line (`end_ms` <= 0: unknown), its length and words, plus its backing vocals.
#[derive(Debug, Clone, Default)]
pub struct Line {
    pub start_ms: i64,
    pub end_ms: i64,
    pub len: u32,
    pub words: Vec<Word>,
    pub backing_len: u32,
    pub backing: Vec<Word>,
}

/// Lyrics display state at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Lit line; -1 before the first or when unsynced; the line count once the last line is over.
    pub active: i32,
    /// Duration of the change into `active`.
    pub glide_ms: i32,
    /// Fill position in the active line, fractional UTF-16 units.
    pub sung: f32,
}

/// Result of [`LyricClock::advance`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    /// Frame to draw (the previous one unless `redraw`).
    pub frame: Frame,
    /// Next call: in display frames while sweeping, else (or when `still`) in ms; 0 = never.
    pub wait: u32,
    /// Sweeping but idle for `wait` ms (between words or lines): sleep instead of counting frames.
    pub still: bool,
    /// Whether anything visible changed.
    pub redraw: bool,
}

/// Precomputed line switch times, glide durations and word timings.
pub struct LyricTiming {
    synced: bool,
    word_timed: bool,
    starts: Vec<i64>,
    /// Starts are non-decreasing (binary search allowed).
    sorted: bool,
    switch_at: Vec<i64>,
    glide: Vec<i32>,
    lens: Vec<u32>,
    /// Each line's range in `words`.
    spans: Vec<(u32, u32)>,
    backing_lens: Vec<u32>,
    backing_spans: Vec<(u32, u32)>,
    words: Vec<Word>,
    /// When the last line is over; None without lines.
    over_at: Option<i64>,
}

impl LyricTiming {
    /// Each change lasts [`GLIDE_SHARE`] of the gap to the next line (clamped) and is centred on the line's
    /// start, so the line is fully lit as it is sung.
    pub fn new(synced: bool, word_timed: bool, lines: impl IntoIterator<Item = Line>) -> Self {
        let mut starts = Vec::new();
        let mut lens = Vec::new();
        let mut spans = Vec::new();
        let mut backing_lens = Vec::new();
        let mut backing_spans = Vec::new();
        let mut words = Vec::new();
        let mut last_end = 0;
        for l in lines.into_iter().take(MAX_LINES) {
            last_end = l.end_ms;
            starts.push(l.start_ms);
            lens.push(l.len);
            let from = words.len() as u32;
            words.extend_from_slice(&l.words);
            spans.push((from, words.len() as u32));
            backing_lens.push(l.backing_len);
            let from = words.len() as u32;
            words.extend_from_slice(&l.backing);
            backing_spans.push((from, words.len() as u32));
        }
        let n = starts.len();
        // A last word ending at the next line's start and longer than any word is a guessed end (older
        // cached lyrics): shorten it to an estimate.
        for i in 0..n.saturating_sub(1) {
            let next = starts[i + 1];
            for &(from, to) in [&spans[i], &backing_spans[i]] {
                if let Some(w) = (from < to).then(|| &mut words[to as usize - 1]) {
                    let longest = word_ms_estimate(w.end.saturating_sub(w.start));
                    if next > starts[i] && w.end_ms >= next && w.end_ms - w.start_ms > WORD_MS_MAX {
                        w.end_ms = w.start_ms + longest;
                    }
                }
            }
        }
        let glide: Vec<i32> = (0..n)
            .map(|i| {
                let gap = if i + 1 < n { starts[i + 1].wrapping_sub(starts[i]) } else { i64::MAX };
                ((gap.min(LONGEST_GAP_MS) as f64 * GLIDE_SHARE) as i32).clamp(MIN_GLIDE_MS, GLIDE_MS)
            })
            .collect();
        // Switch times strictly increase.
        let mut switch_at: Vec<i64> = Vec::with_capacity(n);
        for i in 0..n {
            let at = starts[i] - (glide[i] / 2) as i64;
            switch_at.push(if i == 0 { at } else { at.max(switch_at[i - 1] + 1) });
        }
        // The last line is over at its end (given or LAST_LINE_MS), after its words stop animating, and
        // never before it has been lit.
        let over_at = n.checked_sub(1).map(|last| {
            let sung = [spans[last], backing_spans[last]].iter().flat_map(|&(from, to)| &words[from as usize..to as usize]).map(|w| w.end_ms + MOTION_TAIL_MS).max();
            let end = if last_end > starts[last] { last_end } else { starts[last] + LAST_LINE_MS };
            end.max(sung.unwrap_or(end)).max(switch_at[last] + 1)
        });
        let sorted = starts.windows(2).all(|w| w[0] <= w[1]);
        LyricTiming { synced, word_timed, starts, sorted, switch_at, glide, lens, spans, backing_lens, backing_spans, words, over_at }
    }

    pub fn synced(&self) -> bool {
        self.synced
    }

    /// Whether the active line fills word by word: only with real per-word times (interpolating by length
    /// drifts visibly).
    pub fn sweeps(&self) -> bool {
        self.synced && self.word_timed
    }

    /// The line whose change has begun by `t`, or -1.
    pub(crate) fn line_at(&self, t: i64) -> i32 {
        self.switch_at.partition_point(|&s| s <= t) as i32 - 1
    }

    /// Whether the last line is over at `t`.
    pub fn over(&self, t: i64) -> bool {
        self.over_at.is_some_and(|at| t >= at)
    }

    /// [`LyricTiming::line_at`], or the line count once the last line is over.
    pub(crate) fn line_lit(&self, t: i64) -> i32 {
        if self.over(t) {
            self.starts.len() as i32
        } else {
            self.line_at(t)
        }
    }

    /// The next switch after `t` (including the last line ending).
    pub(crate) fn next_switch_after(&self, t: i64) -> Option<i64> {
        let next = (self.line_at(t) + 1) as usize;
        self.switch_at.get(next).copied().or_else(|| self.over_at.filter(|&at| next == self.starts.len() && t < at))
    }

    /// Change duration into `line`; [`GLIDE_MS`] for no line.
    pub fn glide_ms(&self, line: i32) -> i32 {
        usize::try_from(line).ok().and_then(|i| self.glide.get(i)).copied().unwrap_or(GLIDE_MS)
    }

    /// The line whose start was last reached by `t` (the lit line switches half a glide earlier).
    pub(crate) fn sung_line(&self, t: i64) -> Option<usize> {
        if self.sorted {
            self.starts.partition_point(|&s| s <= t).checked_sub(1)
        } else {
            self.starts.iter().rposition(|&s| s <= t)
        }
    }

    /// Fill position in `line` at `ms`: linear within a word, resting between words; all or nothing
    /// without words. `f32` maths matches the former Kotlin exactly.
    pub(crate) fn sung_offset(&self, line: usize, ms: i64) -> f32 {
        let (Some(&span), Some(&len), Some(&start)) = (self.spans.get(line), self.lens.get(line), self.starts.get(line)) else {
            return 0.0;
        };
        self.sung_in(span, len, start, ms)
    }

    /// [`LyricTiming::sung_offset`] for the line's backing vocals.
    pub fn backing_sung(&self, line: usize, ms: i64) -> f32 {
        let (Some(&span), Some(&len), Some(&start)) = (self.backing_spans.get(line), self.backing_lens.get(line), self.starts.get(line)) else {
            return 0.0;
        };
        self.sung_in(span, len, start, ms)
    }

    /// The last word extends to the end of the text (e.g. a closing bracket) so the line fills fully.
    fn sung_in(&self, (from, to): (u32, u32), len: u32, start: i64, ms: i64) -> f32 {
        if from == to {
            return if ms >= start { len as f32 } else { 0.0 };
        }
        let mut at = 0f32;
        let last = to as usize - 1;
        for (i, w) in self.words.iter().enumerate().take(to as usize).skip(from as usize) {
            let end = if i == last { w.end.max(len) } else { w.end };
            if ms >= w.end_ms {
                at = end as f32;
                continue;
            }
            if ms > w.start_ms {
                let through = (ms - w.start_ms) as f32 / (w.end_ms - w.start_ms).max(1) as f32;
                at = w.start as f32 + (end as i32 - w.start as i32) as f32 * through;
            }
            break;
        }
        at
    }

    /// Whether a word of the sung line is being sung or still animating ([`MOTION_TAIL_MS`]).
    pub fn moving(&self, t: i64) -> bool {
        let Some(line) = self.sung_line(t) else { return false };
        let alive = |&(from, to): &(u32, u32)| self.words[from as usize..to as usize].iter().any(|w| t >= w.start_ms && t <= w.end_ms + MOTION_TAIL_MS);
        self.spans.get(line).is_some_and(alive) || self.backing_spans.get(line).is_some_and(alive)
    }

    pub fn frame(&self, t: i64) -> Frame {
        let active = if self.synced { self.line_lit(t) } else { -1 };
        let sung = if active >= 0 { self.sung_offset(active as usize, t) } else { 0.0 };
        Frame { active, glide_ms: self.glide_ms(active), sung }
    }

    /// Whether going from `shown` to `t` changes anything visible. Always true without the sweep (wakes
    /// only happen at line changes). With the sweep it follows the sung line, not the lit one, so the
    /// next change starts on the line's timestamp (long-standing behaviour). With `lively`, animation
    /// counts as change.
    pub fn moved(&self, shown: i64, t: i64, sweep: bool, lively: bool) -> bool {
        if !sweep || self.over(shown) != self.over(t) {
            return true;
        }
        let Some(line) = self.sung_line(t) else { return true };
        self.sung_line(shown) != Some(line)
            || (self.sung_offset(line, t) - self.sung_offset(line, shown)).abs() >= SWEEP_STEP
            || (self.backing_sung(line, t) - self.backing_sung(line, shown)).abs() >= SWEEP_STEP
            || (lively && (self.moving(t) || self.moving(shown)))
    }

    /// While sweeping, ms (8..=500) until something in the sung line changes; None while a word is being
    /// sung or (with `lively`) animating.
    pub fn quiet_ms(&self, t: i64, lively: bool) -> Option<u32> {
        let next = match self.sung_line(t) {
            None => self.switch_at.iter().chain(&self.starts).copied().filter(|&s| s > t).min(),
            Some(line) => {
                if lively && self.moving(t) {
                    return None;
                }
                let over = self.over_at.filter(|&at| at > t && line + 1 == self.starts.len());
                let mut next = self.starts.get(line + 1).copied().filter(|&s| s > t).or(over);
                for &(from, to) in [self.spans.get(line), self.backing_spans.get(line)].into_iter().flatten() {
                    for w in &self.words[from as usize..to as usize] {
                        if t >= w.start_ms && t < w.end_ms {
                            return None;
                        }
                        if w.start_ms > t {
                            next = Some(next.map_or(w.start_ms, |n| n.min(w.start_ms)));
                        }
                    }
                }
                next
            }
        };
        Some(next.map_or(WAKE_MAX_MS, |n| (n - t).clamp(WAKE_MIN_MS, WAKE_MAX_MS)) as u32)
    }

    /// [`Step::wait`] at `t`; every frame while `lively` words animate (half rate judders).
    pub fn wait(&self, t: i64, sweep: bool, lively: bool) -> u32 {
        if !self.synced {
            0
        } else if sweep && lively && self.moving(t) {
            1
        } else if sweep {
            SWEEP_FRAMES
        } else {
            self.next_switch_after(t).map_or(WAKE_MAX_MS, |next| (next - t).clamp(WAKE_MIN_MS, WAKE_MAX_MS)) as u32
        }
    }
}

/// [`LyricTiming`] plus display state (last drawn moment, nudge, tap landing), in atomics so the UI
/// thread can share it without a lock.
pub struct LyricClock {
    timing: LyricTiming,
    shown: AtomicI64,
    nudge: AtomicI64,
    /// How much later than the song the lyric times run; added to the playhead with the nudge.
    offset: i64,
    /// Start of the tapped line while landing, else [`NOT_LANDING`].
    landing: AtomicI64,
}

impl LyricClock {
    pub fn new(timing: LyricTiming, position_ms: i64) -> Self {
        Self::with_offset(timing, position_ms, 0)
    }

    /// A clock for lyrics whose times run `offset_ms` later than the song (`Lyrics::offset_ms`).
    pub fn with_offset(timing: LyricTiming, position_ms: i64, offset_ms: i64) -> Self {
        LyricClock { timing, shown: AtomicI64::new(position_ms + offset_ms), nudge: AtomicI64::new(0), offset: offset_ms, landing: AtomicI64::new(NOT_LANDING) }
    }

    pub fn timing(&self) -> &LyricTiming {
        &self.timing
    }

    /// Per-frame update at `position_ms`. `sweep`: word fill wanted (if the lyrics allow); `lively`: word
    /// animations enabled; `force`: redraw regardless (first call after start or resume).
    pub fn advance(&self, position_ms: i64, sweep: bool, lively: bool, force: bool) -> Step {
        let sweep = sweep && self.timing.sweeps();
        let t = self.landed(position_ms + self.offset + self.nudge.load(Relaxed));
        let redraw = force || self.timing.moved(self.shown.load(Relaxed), t, sweep, lively);
        if redraw {
            self.shown.store(t, Relaxed);
        }
        let (wait, still) = match self.timing.quiet_ms(t, lively) {
            Some(ms) if sweep => (ms, true),
            _ => (self.timing.wait(t, sweep, lively), false),
        };
        Step { frame: self.timing.frame(self.shown.load(Relaxed)), wait, still, redraw }
    }

    pub fn shown(&self) -> Frame {
        self.timing.frame(self.shown.load(Relaxed))
    }

    /// The displayed moment (offset and nudge applied), for word animations.
    pub fn shown_ms(&self) -> i64 {
        self.shown.load(Relaxed)
    }

    /// Backing vocal fill of the lit line at the displayed moment.
    pub fn backing_sung(&self) -> f32 {
        let t = self.shown.load(Relaxed);
        match usize::try_from(self.timing.line_at(t)) {
            Ok(line) if self.timing.synced => self.timing.backing_sung(line, t),
            _ => 0.0,
        }
    }

    /// Shows `line` immediately and returns the seek position (its start minus offset and nudge). While
    /// landing, positions slightly before it hold the display instead of flashing the previous line.
    pub fn tap(&self, line: usize) -> i64 {
        let Some(&start) = self.timing.starts.get(line) else { return self.shown.load(Relaxed).max(0) };
        self.shown.store(start, Relaxed);
        self.landing.store(start, Relaxed);
        (start - self.offset - self.nudge.load(Relaxed)).max(0)
    }

    /// After replacing lyrics, shows `line` (from [`matching_line`]) and lands on it like a tap without
    /// seeking, so a slightly later timing does not flash the previous line. No-op when already at or
    /// past the line, or more than [`LAND_EARLY_MS`] before it.
    pub fn land(&self, line: usize) {
        let Some(&start) = self.timing.starts.get(line) else { return };
        let shown = self.shown.load(Relaxed);
        if !self.timing.synced || self.timing.line_at(shown) >= line as i32 || shown < start - LAND_EARLY_MS {
            return;
        }
        self.shown.store(start, Relaxed);
        self.landing.store(start, Relaxed);
    }

    /// The moment to show for position `t`, applying tap landing.
    fn landed(&self, t: i64) -> i64 {
        let at = self.landing.load(Relaxed);
        if at == NOT_LANDING {
            return t;
        }
        let shown = self.shown.load(Relaxed);
        if t >= shown {
            if t - at > LANDING_MS {
                self.landing.store(NOT_LANDING, Relaxed);
            }
            return t;
        }
        if t >= at - LAND_EARLY_MS && shown - t <= LAND_HOLD_MS {
            return shown;
        }
        // Far from the landing: a different seek.
        self.landing.store(NOT_LANDING, Relaxed);
        t
    }

    /// `dir` > 0: sooner, < 0: later, 0: reset. Returns the nudge in ms.
    pub fn nudge(&self, dir: i32) -> i64 {
        let step = NUDGE_STEP_MS * dir.signum() as i64;
        if dir == 0 {
            self.nudge.store(0, Relaxed);
            0
        } else {
            self.nudge.fetch_add(step, Relaxed) + step
        }
    }
}

// Packing into one i64.

const SUNG_BITS: u32 = 29;
/// Fraction bits of `sung` (lines up to 2047 units).
pub(crate) const SUNG_FRAC: u32 = 18;
const ACTIVE_BITS: u32 = 13;
const GLIDE_BITS: u32 = 10;
const WAIT_BITS: u32 = 9;
const ACTIVE_AT: u32 = SUNG_BITS;
const GLIDE_AT: u32 = ACTIVE_AT + ACTIVE_BITS;
const WAIT_AT: u32 = GLIDE_AT + GLIDE_BITS;
const STILL_AT: u32 = WAIT_AT + WAIT_BITS;
const REDRAW_AT: u32 = STILL_AT + 1;
/// Low bits of a packed [`Step`] holding the [`Frame`].
#[cfg(test)]
pub(crate) const FRAME_BITS: u32 = WAIT_AT;

fn field(v: i64, bits: u32) -> i64 {
    v.clamp(0, (1 << bits) - 1)
}

impl Frame {
    /// `sung` (29 bits, fixed point), `active + 1` (13), `glide_ms` (10).
    pub fn pack(&self) -> i64 {
        let sung = field((self.sung.max(0.0) * (1u32 << SUNG_FRAC) as f32) as i64, SUNG_BITS);
        sung | field(self.active as i64 + 1, ACTIVE_BITS) << ACTIVE_AT | field(self.glide_ms as i64, GLIDE_BITS) << GLIDE_AT
    }

    pub fn unpack(v: i64) -> Frame {
        Frame {
            active: ((v >> ACTIVE_AT) & ((1 << ACTIVE_BITS) - 1)) as i32 - 1,
            glide_ms: ((v >> GLIDE_AT) & ((1 << GLIDE_BITS) - 1)) as i32,
            sung: (v & ((1 << SUNG_BITS) - 1)) as f32 / (1u32 << SUNG_FRAC) as f32,
        }
    }
}

impl Step {
    /// Frame bits, then `wait` (9), `still` (1), `redraw` (1): 63 bits, non-negative.
    pub fn pack(&self) -> i64 {
        self.frame.pack() | field(self.wait as i64, WAIT_BITS) << WAIT_AT | (self.still as i64) << STILL_AT | (self.redraw as i64) << REDRAW_AT
    }

    pub fn unpack(v: i64) -> Step {
        Step {
            frame: Frame::unpack(v),
            wait: ((v >> WAIT_AT) & ((1 << WAIT_BITS) - 1)) as u32,
            still: (v >> STILL_AT) & 1 == 1,
            redraw: (v >> REDRAW_AT) & 1 == 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_matching() {
        let lines = [0, 4_000, 8_000, 12_000, 16_000];
        let finer = [0, 2_000, 4_100, 6_000, 7_900, 10_000, 12_050];
        assert_eq!(matching_line(&lines, 1, &finer, true), 2);
        assert_eq!(matching_line(&lines, 2, &finer, true), 4);
        assert_eq!(matching_line(&lines, 4, &finer, true), 6);
        assert_eq!(matching_line(&[0, 3_000], 1, &[0, 2_000, 4_000], true), 1, "tie: earlier");
        // Untimed: same index, clamped.
        assert_eq!(matching_line(&lines, 3, &finer, false), 3);
        assert_eq!(matching_line(&lines, 4, &[-1, -1], false), 1);
        assert_eq!(matching_line(&[-1, -1], 1, &finer, true), 1, "old line untimed");
        assert_eq!(matching_line(&[], 9, &finer, true), 6);
        assert_eq!(matching_line(&lines, 4, &[], true), 0);

        // Line strength by position.
        assert_eq!(line_strength(true, 3, 3), 1.0);
        assert_eq!(line_strength(true, 2, 3), 0.35 * 0.55);
        assert_eq!(line_strength(true, 4, 3), 0.35);
        assert_eq!(line_strength(true, 0, -1), 0.35);
        assert_eq!(line_strength(false, 0, 5), 1.0);
    }

    fn lines(starts: &[i64]) -> Vec<Line> {
        starts.iter().map(|&s| Line { start_ms: s, len: 10, words: Vec::new(), ..Default::default() }).collect()
    }

    fn w(start_ms: i64, end_ms: i64, start: u32, end: u32) -> Word {
        Word { start_ms, end_ms, start, end }
    }

    #[test]
    fn switch_timing() {
        let t = LyricTiming::new(true, false, lines(&[0, 100, 400, 1000, 11_000, 12_000]));
        // 100 ms gap -> 85 -> 160; 300 -> 255; 600 -> 510; 10 s -> 620; 1 s -> 850 -> 620; last -> 620.
        assert_eq!(t.glide, [160, 255, 510, 620, 620, 620]);
        assert_eq!((t.glide_ms(-1), t.glide_ms(6), t.glide_ms(2)), (GLIDE_MS, GLIDE_MS, 510));
        // 255.85 truncates.
        assert_eq!(LyricTiming::new(true, false, lines(&[0, 301])).glide[0], 255);

        // Switch leads by half glide and stays ordered.
        let t = LyricTiming::new(true, false, lines(&[1000, 1100, 1110, 5000]));
        // Glides 160, 160, 620 (3890 gap -> 3306 -> 620), 620: leads 80, 80, 310, 310.
        assert_eq!(t.switch_at, [920, 1020, 1021, 4690]);
        assert_eq!((t.line_at(919), t.line_at(920), t.line_at(1020), t.line_at(1021), t.line_at(4689), t.line_at(4690)), (-1, 0, 1, 2, 2, 3));
        // After the last line, the next switch is its end (LAST_LINE_MS when unknown).
        assert_eq!((t.next_switch_after(0), t.next_switch_after(1021), t.next_switch_after(4690)), (Some(920), Some(4690), Some(5000 + LAST_LINE_MS)));
        assert_eq!(t.next_switch_after(5000 + LAST_LINE_MS), None);
    }

    #[test]
    fn word_fill() {
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 1000, len: 11, words: vec![w(1000, 1400, 0, 5), w(1600, 2000, 6, 11)], ..Default::default() }]);
        let at = |ms| t.sung_offset(0, ms);
        assert_eq!((at(900), at(1000), at(1200), at(1400), at(1500), at(1600), at(1700), at(2000), at(9000)), (0.0, 0.0, 2.5, 5.0, 5.0, 5.0, 7.25, 11.0, 11.0));
        // A zero-length word completes at its start; a wordless line is all or nothing.
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 0, len: 4, words: vec![w(500, 500, 0, 4)], ..Default::default() }, Line { start_ms: 800, len: 7, words: vec![], ..Default::default() }]);
        assert_eq!((t.sung_offset(0, 499), t.sung_offset(0, 500), t.sung_offset(1, 799), t.sung_offset(1, 800)), (0.0, 4.0, 0.0, 7.0));

        // Guessed last word end is estimated.
        // Last word's end is the next line's start, 30 s later.
        let words = vec![w(10_000, 10_400, 0, 4), w(10_400, 10_800, 5, 7), w(10_800, 40_000, 8, 15)];
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 10_000, len: 15, words, ..Default::default() }, Line { start_ms: 40_000, len: 5, ..Default::default() }]);
        let full_by = 10_800 + word_ms_estimate(7);
        assert!(full_by < 12_000);
        assert_eq!(t.sung_offset(0, full_by), 15.0);
        assert_eq!(t.sung_offset(0, 25_000), 15.0);
        // Fills smoothly, no steps.
        let mut was = t.sung_offset(0, 10_801);
        for ms in (10_816..=full_by).step_by(16) {
            let now = t.sung_offset(0, ms);
            assert!(now >= was && now - was < 0.25, "{was} -> {now} at {ms}");
            was = now;
        }
        // A plausible held note up to the next line is left alone.
        let held = vec![w(1_000, 1_300, 0, 4), w(1_300, 3_000, 5, 9)];
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 1_000, len: 9, words: held, ..Default::default() }, Line { start_ms: 3_000, len: 3, ..Default::default() }]);
        assert_eq!(t.sung_offset(0, 2_150), 7.0);

        // Last word fills to end of text.
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 0, len: 10, words: vec![w(0, 500, 1, 4), w(500, 1_000, 5, 9)], ..Default::default() }]);
        assert_eq!(t.sung_offset(0, 1_000), 10.0);
        assert_eq!(t.sung_offset(0, 750), 7.5);
    }

    #[test]
    fn sung_line_follows_starts() {
        let t = LyricTiming::new(true, true, lines(&[1000, 3000]));
        assert_eq!((t.sung_line(999), t.sung_line(1000), t.sung_line(2999), t.sung_line(3000)), (None, Some(0), Some(0), Some(1)));
        assert_eq!(t.line_at(2700), 1, "lit 310 ms early");
        // Unsorted starts: last one reached, like Kotlin's indexOfLast.
        let t = LyricTiming::new(true, true, lines(&[1000, 500, 2000]));
        assert_eq!((t.sung_line(600), t.sung_line(1500)), (Some(1), Some(1)));
    }

    #[test]
    fn wakes() {
        let t = LyricTiming::new(true, false, lines(&[1000, 1200, 5000]));
        // Glides 170, 620, 620; switch_at: 1000-85=915, 1200-310=890 -> 916, 5000-310=4690.
        assert_eq!((t.wait(0, false, false), t.wait(700, false, false), t.wait(914, false, false), t.wait(921, false, false), t.wait(4700, false, false)), (500, 215, 8, 500, 500));
        assert_eq!(t.wait(0, true, false), SWEEP_FRAMES);
        assert_eq!(LyricTiming::new(false, false, lines(&[-1, -1])).wait(0, false, false), 0);

        // Moved only on visible change.
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 1000, len: 10, words: vec![w(1000, 2000, 0, 10)], ..Default::default() }, Line { start_ms: 5000, len: 3, words: vec![], ..Default::default() }]);
        assert!(t.moved(0, 500, false, false), "no sweep: always");
        assert!(t.moved(0, 500, true, false), "before the first line");
        assert!(!t.moved(1000, 1003, true, false), "0.03 characters");
        assert!(t.moved(1000, 1005, true, false), "0.05 characters");
        assert!(!t.moved(2000, 4000, true, false), "between words");
        // Lit line switches at 4690, sung line at 5000: hold until 5000.
        assert!(!t.moved(2000, 4800, true, false) && t.line_at(4800) == 1);
        assert!(t.moved(2000, 5000, true, false));

        // Quiet ms sleeps between changes.
        let line = Line { start_ms: 1000, len: 11, words: vec![w(1000, 1400, 0, 5), w(1600, 2000, 6, 11)], backing_len: 3, backing: vec![w(2200, 2300, 0, 3)], ..Default::default() };
        let t = LyricTiming::new(true, true, vec![line, Line { start_ms: 2600, len: 3, ..Default::default() }]);
        // Before the first line: until its switch (310 ms before its start).
        assert_eq!(t.quiet_ms(0, false), Some(500));
        assert_eq!(t.quiet_ms(600, false), Some(90));
        // Inside a word or backing word: None.
        assert_eq!((t.quiet_ms(1200, false), t.quiet_ms(2250, false)), (None, None));
        // Between words: until the next word, backing word, or line.
        assert_eq!((t.quiet_ms(1450, false), t.quiet_ms(2000, false), t.quiet_ms(2300, false)), (Some(150), Some(200), Some(300)));
        // Animating words (lively): None.
        assert_eq!((t.quiet_ms(1450, true), t.quiet_ms(2300, true)), (None, None));
        let c = LyricClock::new(t, 0);
        let s = c.advance(1450, true, false, false);
        assert_eq!((s.wait, s.still), (150, true));
        let s = c.advance(1450, false, false, false);
        assert!(!s.still, "no sweep: never still");
        assert_eq!((c.advance(1200, true, false, false).wait, c.advance(1200, true, false, false).still), (SWEEP_FRAMES, false));

        // Unsynced lyrics never wake.
        let c = LyricClock::new(LyricTiming::new(false, false, lines(&[-1, -1, -1])), 0);
        let s = c.advance(10_000, true, false, true);
        assert_eq!(s, Step { frame: Frame { active: -1, glide_ms: GLIDE_MS, sung: 0.0 }, wait: 0, still: false, redraw: true });
    }

    #[test]
    fn taps() {
        let timing = LyricTiming::new(true, true, vec![Line { start_ms: 1000, len: 10, words: vec![w(1000, 2000, 0, 10)], ..Default::default() }, Line { start_ms: 5000, len: 3, words: vec![], ..Default::default() }]);
        let c = LyricClock::new(timing, 0);
        assert_eq!(c.shown(), Frame { active: -1, glide_ms: GLIDE_MS, sung: 0.0 });
        let s = c.advance(1500, true, false, false);
        assert_eq!(s, Step { frame: Frame { active: 0, glide_ms: 620, sung: 5.0 }, wait: SWEEP_FRAMES, still: false, redraw: true });
        let s = c.advance(1502, true, false, false);
        assert!(!s.redraw && s.frame.sung == 5.0);
        assert!(c.advance(1502, true, false, true).redraw);
        assert_eq!(c.nudge(1), 250);
        assert_eq!(c.advance(1500, true, false, false).frame.sung, 7.5, "sooner");
        assert_eq!((c.nudge(-1), c.nudge(-1), c.nudge(-1)), (0, -250, -500));
        assert_eq!(c.tap(1), 5500, "seek compensates the nudge");
        assert_eq!(c.shown().active, 1);
        assert_eq!(c.nudge(0), 0);
        assert_eq!(c.tap(0), 1000);
        c.nudge(1);
        c.nudge(1);
        assert_eq!(c.tap(0), 500);
        // Without word times the sweep is ignored; wake at the next line.
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&[1000, 5000])), 0);
        assert_eq!(c.advance(0, true, false, false).wait, 500);
        assert_eq!(c.advance(4400, true, false, false).wait, 290);

        // Offset applies to display and tap.
        // Lyric times run 1.5 s late: the line timed at 5000 is sung at 3500.
        let c = LyricClock::with_offset(LyricTiming::new(true, false, lines(&[1000, 5000])), 0, 1500);
        assert_eq!(c.advance(2500, false, false, false).frame.active, 0);
        assert_eq!(c.advance(3600, false, false, false).frame.active, 1);
        assert_eq!(c.tap(1), 3500);
        c.nudge(0);
        assert_eq!(c.nudge(1), 250, "nudge stacks on the offset");
        assert_eq!(c.tap(1), 3250);

        // Tap holds line when seek lands early.
        let timing = LyricTiming::new(
            true,
            true,
            vec![
                Line { start_ms: 1000, len: 10, words: vec![w(1000, 4000, 0, 10)], ..Default::default() },
                Line { start_ms: 5000, len: 8, words: vec![w(5000, 5400, 0, 4), w(5400, 6000, 4, 8)], ..Default::default() },
                Line { start_ms: 9000, len: 3, words: vec![w(9000, 9500, 0, 3)], ..Default::default() },
            ],
        );
        let c = LyricClock::new(timing, 4500);
        assert_eq!(c.advance(4500, true, false, true).frame.active, 0);
        assert_eq!(c.tap(1), 5000);
        assert_eq!(c.shown(), Frame { active: 1, glide_ms: 620, sung: 0.0 });
        // Seek landed 26 ms early (one MP3 frame): stays on the tapped line.
        let s = c.advance(4974, true, false, false);
        assert_eq!((s.frame.active, s.frame.sung), (1, 0.0));
        assert_eq!(c.shown_ms(), 5000);
        let s = c.advance(5100, true, false, false);
        assert_eq!((s.frame.active, s.frame.sung), (1, 1.0));
        // Playhead set back after running on: the fill holds instead of going back.
        let s = c.advance(5000, true, false, false);
        assert_eq!((s.frame.active, s.frame.sung), (1, 1.0));
        assert!(!s.redraw);
        let s = c.advance(5200, true, false, false);
        assert_eq!((s.frame.active, s.frame.sung), (1, 2.0));
        // After LANDING_MS, a seek back is shown as is.
        c.advance(8100, true, false, false);
        assert_eq!(c.advance(4500, true, false, false).frame.active, 0);
        // A distant seek during landing is shown at once.
        c.tap(1);
        assert_eq!(c.advance(1500, true, false, false).frame.active, 0);
        c.tap(1);
        assert_eq!(c.advance(9100, true, false, false).frame.active, 2);
        // Tapping the first line and landing early does not show "before the lyrics".
        let c = LyricClock::new(LyricTiming::new(true, true, vec![Line { start_ms: 800, len: 4, words: vec![w(800, 1200, 0, 4)], ..Default::default() }]), 30_000);
        assert_eq!(c.tap(0), 800);
        assert_eq!(c.advance(0, true, false, false).frame.active, 0);
    }

    #[test]
    fn backing_vocals_fill_apart() {
        let line = Line { start_ms: 1000, len: 5, words: vec![w(1000, 1500, 0, 5)], backing_len: 4, backing: vec![w(1600, 2000, 0, 4)], ..Default::default() };
        let t = LyricTiming::new(true, true, vec![line, Line { start_ms: 9000, len: 3, ..Default::default() }]);
        assert_eq!((t.backing_sung(0, 1500), t.backing_sung(0, 1800), t.backing_sung(0, 2000)), (0.0, 2.0, 4.0));
        assert_eq!(t.sung_offset(1, 9000), 3.0, "backing words do not leak into the next line");
        assert!(t.moving(1200) && t.moving(2000 + MOTION_TAIL_MS) && !t.moving(2001 + MOTION_TAIL_MS));
        assert_eq!((t.wait(1200, true, true), t.wait(1200, true, false), t.wait(5000, true, true)), (1, SWEEP_FRAMES, SWEEP_FRAMES));
        // Settling counts as change only when lively.
        assert!(t.moved(2100, 2110, true, true) && !t.moved(2100, 2110, true, false));
        let c = LyricClock::new(t, 0);
        c.advance(1800, true, true, false);
        assert_eq!((c.shown_ms(), c.backing_sung()), (1800, 2.0));
    }

    #[test]
    fn last_line() {
        // Line-timed; the last line ends at 8 s.
        let two = vec![Line { start_ms: 1000, len: 5, ..Default::default() }, Line { start_ms: 5000, end_ms: 8000, len: 5, ..Default::default() }];
        let t = LyricTiming::new(true, false, two.clone());
        assert_eq!((t.frame(7999).active, t.frame(8000).active, t.frame(600_000).active), (1, 2, 2));
        assert_eq!(line_strength(true, 1, t.frame(8000).active), PAST_LINE);
        assert_eq!(t.frame(8000).sung, 0.0);
        assert_eq!(t.frame(6000).active, 1, "seek back relights it");
        // Wakes exactly at the end.
        assert_eq!((t.wait(7700, false, false), t.next_switch_after(7700), t.next_switch_after(8000)), (300, Some(8000), None));
        let c = LyricClock::new(t, 7000);
        assert_eq!(c.advance(7000, false, false, true).frame.active, 1);
        assert_eq!(c.advance(8000, false, false, false).frame.active, 2);
        // Unknown end: LAST_LINE_MS.
        let t = LyricTiming::new(true, false, lines(&[1000, 5000]));
        assert_eq!((t.frame(5000 + LAST_LINE_MS - 1).active, t.frame(5000 + LAST_LINE_MS).active), (1, 2));
        // Word-timed: over once words stop animating, and the sweep still redraws at that moment.
        let words = vec![w(5000, 5500, 0, 2), w(5500, 6000, 3, 5)];
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 1000, len: 5, words: vec![w(1000, 2000, 0, 5)], ..Default::default() }, Line { start_ms: 5000, end_ms: 6000, len: 5, words, ..Default::default() }]);
        assert_eq!((t.frame(6000 + MOTION_TAIL_MS - 1).active, t.frame(6000 + MOTION_TAIL_MS).active), (1, 2));
        assert_eq!(t.quiet_ms(6100, false), Some((MOTION_TAIL_MS - 100) as u32));
        let c = LyricClock::new(t, 0);
        assert_eq!(c.advance(5800, true, false, true).frame.active, 1);
        let s = c.advance(6000 + MOTION_TAIL_MS, true, false, false);
        assert!(s.redraw && s.frame.active == 2, "{s:?}");
        let s = c.advance(9000, true, false, false);
        assert!(!s.redraw && s.frame.active == 2, "{s:?}");
        // Unsynced lyrics never end.
        assert_eq!(LyricTiming::new(false, false, lines(&[-1, -1])).frame(600_000).active, -1);

        // Land keeps lit line after retiming.
        let old = [1000, 5000, 9000];
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&old)), 0);
        assert_eq!(c.advance(5200, false, false, true).frame.active, 1);
        // Regression: the same lyrics retimed 800 ms later briefly went back a line.
        let next = [1800, 5800, 9800];
        let line = matching_line(&old, 1, &next, true);
        assert_eq!(line, 1);
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&next)), 5200);
        c.land(line as usize);
        assert_eq!(c.shown().active, 1);
        for ms in (5200..6500).step_by(16) {
            assert_eq!(c.advance(ms, false, false, ms == 5200).frame.active, 1, "at {ms} ms");
        }
        assert_eq!(c.advance(10_000, false, false, false).frame.active, 2);
        assert_eq!(c.advance(2_500, false, false, false).frame.active, 0, "seek back");
        // Far before the line: nothing held.
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&[3000, 7000, 11_000])), 5200);
        c.land(1);
        assert_eq!(c.advance(5200, false, false, true).frame.active, 0);
        // Already on the line: nothing held.
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&[500, 4000, 9000])), 5200);
        c.land(1);
        assert_eq!(c.shown_ms(), 5200);
    }

    #[test]
    fn step_pack_round_trips() {
        let third = Step::unpack(Step { frame: Frame { active: 0, glide_ms: 620, sung: 1.0 / 3.0 }, wait: 0, still: false, redraw: false }.pack()).frame.sung;
        assert!((third - 1.0 / 3.0).abs() < 1.0 / (1 << SUNG_FRAC) as f32);
        for sung in [0.0f32, 7.5, 7.25, 63.999_99, 1234.567, 2047.99] {
            for (active, glide_ms, wait, still, redraw) in [(-1, 620, 0, false, false), (0, 160, 2, false, true), (8190, 594, 500, true, true), (3, 300, 17, true, false)] {
                let s = Step { frame: Frame { active, glide_ms, sung }, wait, still, redraw };
                assert!(s.pack() >= 0);
                assert_eq!(Step::unpack(s.pack()), s);
                assert_eq!(Frame::unpack(s.pack() & ((1 << FRAME_BITS) - 1)), s.frame);
            }
        }
    }
}
