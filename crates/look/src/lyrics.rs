//! How a page of lyrics moves with the song: when each line takes over and how long its change lasts,
//! how far into the active line the singing is, when a platform has to look again, and whether what it
//! would draw has changed at all. Lyrics are shown, not heard - nothing here touches what plays - so this
//! sits with the rest of how a page looks rather than in the player.
//!
//! A platform prepares a [`LyricClock`] once per set of lyrics and then asks it one question per frame
//! (or per wake-up) with the playhead: [`LyricClock::advance`]. The answer fits in one `i64`
//! ([`Step::pack`]), so it can cross any FFI as a primitive, and asking allocates nothing.
//!
//! Text offsets are UTF-16 units, as the core hands them out and as Java and JavaScript strings index.

use core::sync::atomic::{AtomicI64, Ordering::Relaxed};

/// How long a line change takes at most, scroll and colour together so they arrive at the same moment.
/// Long enough that the eye follows the words up rather than losing its place; short enough that a fast
/// verse, a line every second or so, is never still catching up.
pub const GLIDE_MS: i32 = 620;
/// The shortest a line change gets, on the fastest verse. Below this it stops reading as movement.
pub const MIN_GLIDE_MS: i32 = 160;
/// A change is this share of the gap to the next line, so it finishes before the next one starts.
const GLIDE_SHARE: f64 = 0.85;
/// Gaps longer than this count as this long; they all end at [`GLIDE_MS`] anyway.
const LONGEST_GAP_MS: i64 = 10_000;

/// Without the sweep the lyrics sleep until the next line takes over and wake once, rather than looking
/// three times a second and changing up to 300 ms late. Never sooner than this...
const WAKE_MIN_MS: i64 = 8;
/// ...and never later, so a seek is picked up within half a second.
const WAKE_MAX_MS: i64 = 500;
/// With the sweep on, every second display frame is plenty for a text fill, and half the redraws.
pub const SWEEP_FRAMES: u32 = 2;
/// A sweep that moved less than this many characters is not redrawn: between words, and while a held
/// note keeps the boundary still, nothing on screen changes.
const SWEEP_STEP: f32 = 0.04;

/// A sung word rises as it is sung, over at least this long, so a quick syllable does not jump...
pub const RISE_MIN_MS: i64 = 180;
/// ...and settles back over this long once it is done, so a finished line is flat again and nothing
/// drops when the next line takes over.
pub const SETTLE_MS: i64 = 420;
/// A note held this long or longer glows while it is held...
pub const HELD_MS: i64 = 900;
/// ...and its glow fades over this long once it ends.
pub const GLOW_FADE_MS: i64 = 500;
/// How long after a word ends it still moves (settling, its glow fading): while any word of the line
/// being sung is inside this, the platform draws every frame rather than every second one.
pub const MOTION_TAIL_MS: i64 = if SETTLE_MS > GLOW_FADE_MS { SETTLE_MS } else { GLOW_FADE_MS };

/// How long a word is sung when its source says only when it starts, by its length in UTF-16 units:
/// the last word of an LRC line, whose end the file leaves to the next line - and after a long pause
/// that is many seconds away, over which the word used to creep and stay part filled. Only a word
/// that runs on to the next line and longer than any word is guessed to be sung shows it: a held note
/// that stops where the next line starts is left as it was given.
pub fn word_ms_estimate(units: u32) -> i64 {
    (i64::from(units) * WORD_MS_PER_UNIT + WORD_MS_BASE).clamp(WORD_MS_MIN, WORD_MS_MAX)
}
const WORD_MS_PER_UNIT: i64 = 110;
const WORD_MS_BASE: i64 = 250;
const WORD_MS_MIN: i64 = 400;
const WORD_MS_MAX: i64 = 2_000;

/// How long the last line is sung when nothing says when it ends: as long as a line-timed file's last
/// line is taken to last (nori-lyrics `build`).
pub const LAST_LINE_MS: i64 = 5_000;

/// One press of "Sooner" or "Later", for the few songs whose timings are wrong.
pub const NUDGE_STEP_MS: i64 = 250;

/// A tapped line is sought to its first moment, and a player lands a seek where it can: a frame, a
/// packet or a keyframe early, or on a playhead that ran on from the tap for a moment and was then set
/// back to where the seek really landed. Readings up to this far before the tapped line still count as
/// being on it...
pub const LAND_EARLY_MS: i64 = 1_000;
/// ...and the words never go back while the player catches up with what is on screen, by up to this much...
pub const LAND_HOLD_MS: i64 = 1_500;
/// ...for this long into the tapped line; after that the player's word is the truth again.
pub const LANDING_MS: i64 = 3_000;
/// No tap being landed.
const NOT_LANDING: i64 = i64::MIN;

/// How long the words stay where a finger left them before they come back to the line being sung.
pub const READING_MS: i64 = 4_000;

/// How lit a line is before it is reached...
pub const NEXT_LINE: f32 = 0.35;
/// ...and once it has been sung: dimmer again, so the eye goes forward.
pub const PAST_LINE: f32 = NEXT_LINE * 0.55;
/// How lit the words of the line being sung are before they are sung, when it fills word by word: well
/// above the other lines, so the line reads as the one sung, and well below its sung words, which are
/// fully lit. A line fading between two strengths keeps its unsung words at the lower of this and its
/// strength, so a finished line dims from where it was without a step.
pub const UNSUNG: f32 = 0.55;

/// The line of new lyrics (`next`, each line's start in ms) that stands for line `at` of the old ones
/// (`old`), when finer lyrics replace the ones on screen: the one starting nearest to it (the earlier of
/// two as near), or the same number when either is not `timed` (or the old line has no time). Services
/// split a song's words into lines their own way, so the numbers need not agree. 0 when `next` is empty.
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

/// How lit line `line` is with `active` the line being sung (-1 before the first). Untimed words are all
/// fully lit: nothing says which one is being sung.
pub fn line_strength(synced: bool, line: i32, active: i32) -> f32 {
    if !synced || line == active {
        1.0
    } else if line < active {
        PAST_LINE
    } else {
        NEXT_LINE
    }
}

/// Whether the screen is kept on for the lyrics: the listener asked for it, the player is on screen, and
/// the music plays.
pub fn keeps_screen_on(asked: bool, shown: bool, playing: bool) -> bool {
    asked && shown && playing
}

/// Lines past this are never lit: the index has to fit its field in [`Step::pack`], and so does one past
/// the last line ([`Frame::active`] once the lyrics are over). A song has a few hundred at most.
pub const MAX_LINES: usize = (1 << ACTIVE_BITS) - 2;

/// One word (or syllable) of a line and when it is sung; `start`/`end` index the line's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Word {
    pub start_ms: i64,
    pub end_ms: i64,
    pub start: u32,
    pub end: u32,
}

/// A line as the timing needs it: when it is sung and when it ends (`end_ms`, 0 or less when its source
/// does not say), its length in UTF-16 units and its words, and the same of the backing vocals sung over
/// it (drawn under it, filled on their own).
#[derive(Debug, Clone, Default)]
pub struct Line {
    pub start_ms: i64,
    pub end_ms: i64,
    pub len: u32,
    pub words: Vec<Word>,
    pub backing_len: u32,
    pub backing: Vec<Word>,
}

/// What the lyrics look like at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// The line lit and scrolled to, or -1: before the first line, or lyrics that are not timed. Once the
    /// last line is over it is the number of lines: every line has been sung, and none is lit.
    pub active: i32,
    /// How long the change into [`Frame::active`] takes; the scroll and every line's fade use it.
    pub glide_ms: i32,
    /// How far into the active line the singing is, in UTF-16 units with a fraction: 7.5 is half of the
    /// character at index 7.
    pub sung: f32,
}

/// One answer to [`LyricClock::advance`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    /// What to draw: the new moment when `redraw`, otherwise what is already on screen.
    pub frame: Frame,
    /// When to ask again: display frames while sweeping, milliseconds otherwise or when `still`; 0 for
    /// never (the lyrics are not timed, so nothing moves).
    pub wait: u32,
    /// Sweeping, but nothing will change for `wait` milliseconds: between two words, or once a line is
    /// sung and the next one has not started. The platform sleeps instead of counting display frames.
    pub still: bool,
    /// Whether anything on screen changed; when not, the platform should not invalidate.
    pub redraw: bool,
}

/// When each line takes over, how long its change lasts, and where its words are sung.
pub struct LyricTiming {
    synced: bool,
    word_timed: bool,
    starts: Vec<i64>,
    /// Starts never go backwards, so the line being sung can be found by halving.
    sorted: bool,
    switch_at: Vec<i64>,
    glide: Vec<i32>,
    lens: Vec<u32>,
    /// Each line's words, as a range of `words`: one allocation for the whole song.
    spans: Vec<(u32, u32)>,
    /// The same for each line's backing vocals, in the same `words`.
    backing_lens: Vec<u32>,
    backing_spans: Vec<(u32, u32)>,
    words: Vec<Word>,
    /// When the last line is over and goes dim with the rest; none without lines.
    over_at: Option<i64>,
}

impl LyricTiming {
    /// Each line's change is as long as the line allows and is centred on the moment it is sung, so the
    /// line is fully lit as the singing starts rather than a third of a second after. A fixed 620 ms
    /// glide that began on the timestamp both lagged every line and, on a verse faster than that, never
    /// finished - the next change always arrived first.
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
        // A line's last word that runs on to the next line's start is a guess made from a file that
        // said only when the word starts (lyrics read before the parsers estimated it themselves, and
        // kept): it is sung for as long as a word that long is, not through the whole pause after it.
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
        // A line may not take over before the one ahead of it has, however short its own gap.
        let mut switch_at: Vec<i64> = Vec::with_capacity(n);
        for i in 0..n {
            let at = starts[i] - (glide[i] / 2) as i64;
            switch_at.push(if i == 0 { at } else { at.max(switch_at[i - 1] + 1) });
        }
        // Every other line goes dim as the next one takes over; the last has none after it, and stayed lit
        // for as long as the page was open. It is over at its end (its source's, else a line-timed file's
        // guess), once its words and backing vocals have been sung and have stopped moving, and never
        // before it has been lit.
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

    /// Whether the active line may fill in word by word. Only when the lyrics carry real per-word times
    /// (enhanced LRC, or a server's structured cues): spreading a line's duration across its words by
    /// length looks right for a beat and then drifts badly on a held note or a fast line, which reads as
    /// broken sync - a line at a time is honest and stays in step.
    pub fn sweeps(&self) -> bool {
        self.synced && self.word_timed
    }

    /// The line whose change has begun by `t`, or -1 before the first, whether or not the lyrics are timed.
    pub fn line_at(&self, t: i64) -> i32 {
        self.switch_at.partition_point(|&s| s <= t) as i32 - 1
    }

    /// Whether the last line is over at `t`: every line has been sung.
    pub fn over(&self, t: i64) -> bool {
        self.over_at.is_some_and(|at| t >= at)
    }

    /// The line lit at `t`: the one whose change has begun, -1 before the first, and the number of lines
    /// once the last is over, when every line is drawn as one sung.
    pub fn line_lit(&self, t: i64) -> i32 {
        if self.over(t) {
            self.starts.len() as i32
        } else {
            self.line_at(t)
        }
    }

    /// When the line after the one at `t` takes over, or the last line is over.
    pub fn next_switch_after(&self, t: i64) -> Option<i64> {
        let next = (self.line_at(t) + 1) as usize;
        self.switch_at.get(next).copied().or_else(|| self.over_at.filter(|&at| next == self.starts.len() && t < at))
    }

    /// How long the change into `line` takes; [`GLIDE_MS`] for no line.
    pub fn glide_ms(&self, line: i32) -> i32 {
        usize::try_from(line).ok().and_then(|i| self.glide.get(i)).copied().unwrap_or(GLIDE_MS)
    }

    /// The line whose timestamp is the last one reached by `t` - the one being sung, as opposed to the
    /// one lit, which takes over half a change earlier.
    pub fn sung_line(&self, t: i64) -> Option<usize> {
        if self.sorted {
            self.starts.partition_point(|&s| s <= t).checked_sub(1)
        } else {
            self.starts.iter().rposition(|&s| s <= t)
        }
    }

    /// How far into `line` the singing is at `ms`. Inside a word the sweep is linear; between words it
    /// rests at the word's end. A line without words is all or nothing. In `f32`, step for step as the
    /// app drew it, so the boundary lands on the same pixel.
    pub fn sung_offset(&self, line: usize, ms: i64) -> f32 {
        let (Some(&span), Some(&len), Some(&start)) = (self.spans.get(line), self.lens.get(line), self.starts.get(line)) else {
            return 0.0;
        };
        self.sung_in(span, len, start, ms)
    }

    /// How far into `line`'s backing vocals the singing is at `ms`, as [`LyricTiming::sung_offset`] for
    /// the line itself: each part fills on its own time.
    pub fn backing_sung(&self, line: usize, ms: i64) -> f32 {
        let (Some(&span), Some(&len), Some(&start)) = (self.backing_spans.get(line), self.backing_lens.get(line), self.starts.get(line)) else {
            return 0.0;
        };
        self.sung_in(span, len, start, ms)
    }

    /// The last word runs to the end of the text (a closing bracket, a stop the source left outside
    /// it), so a line whose words are all sung is filled all the way, and gets there within the word
    /// rather than with a step after it.
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

    /// Whether anything in the line being sung at `t` moves on its own at `t`: a word or a backing word
    /// being sung, or rising, settling or glowing within [`MOTION_TAIL_MS`] of its end.
    pub fn moving(&self, t: i64) -> bool {
        let Some(line) = self.sung_line(t) else { return false };
        let alive = |&(from, to): &(u32, u32)| self.words[from as usize..to as usize].iter().any(|w| t >= w.start_ms && t <= w.end_ms + MOTION_TAIL_MS);
        self.spans.get(line).is_some_and(alive) || self.backing_spans.get(line).is_some_and(alive)
    }

    /// The lyrics as they look at `t`.
    pub fn frame(&self, t: i64) -> Frame {
        let active = if self.synced { self.line_lit(t) } else { -1 };
        let sung = if active >= 0 { self.sung_offset(active as usize, t) } else { 0.0 };
        Frame { active, glide_ms: self.glide_ms(active), sung }
    }

    /// Whether moving what is drawn from `shown` to `t` changes anything on screen. Without the sweep a
    /// wake-up only happens when a line is due, so it always does.
    ///
    /// The test follows the line being sung, not the one lit. Where a line's last word ends well before
    /// the next line's timestamp, the sweep therefore holds the page still until that timestamp, and the
    /// next change starts on it rather than half a change ahead as it does without the sweep. That is how
    /// the lyrics have always moved, and it is kept.
    ///
    /// With `lively` (words rise and glow as they are sung), anything moving is a change too. The last line
    /// being over, or no longer over (a seek back), always is.
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

    /// While sweeping, how long after `t` nothing in the line being sung changes, in milliseconds (within
    /// 8 to 500): between its words, or once it is sung until the next line. None while a word or a
    /// backing word is being sung, or, `lively`, while one still rises, settles or glows. Before the first
    /// line it is until the first change; on the last line, until it is over.
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

    /// How long until the page may look different after `t`: see [`Step::wait`]. With `lively`, every
    /// display frame while something in the line moves: a soft edge crossing a quick syllable at half the
    /// frame rate walks in steps, and a rising word judders.
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

/// A [`LyricTiming`] with what is on screen: the moment last drawn and the listener's nudge. Both are
/// atomics, so the clock can be shared with a platform's UI thread without a lock.
pub struct LyricClock {
    timing: LyricTiming,
    shown: AtomicI64,
    nudge: AtomicI64,
    /// How much later than the song the lyrics' times run (the sync check's offset): added to the playhead
    /// before the nudge, which stays the listener's own.
    offset: i64,
    /// The start of the line last tapped, while the player lands there ([`NOT_LANDING`] otherwise).
    landing: AtomicI64,
}

impl LyricClock {
    /// Starts showing the moment `position_ms`, un-nudged.
    pub fn new(timing: LyricTiming, position_ms: i64) -> Self {
        Self::with_offset(timing, position_ms, 0)
    }

    /// Starts showing the moment `position_ms` of lyrics whose times run `offset_ms` later than the song
    /// (`Lyrics::offset_ms`), un-nudged.
    pub fn with_offset(timing: LyricTiming, position_ms: i64, offset_ms: i64) -> Self {
        LyricClock { timing, shown: AtomicI64::new(position_ms + offset_ms), nudge: AtomicI64::new(0), offset: offset_ms, landing: AtomicI64::new(NOT_LANDING) }
    }

    pub fn timing(&self) -> &LyricTiming {
        &self.timing
    }

    /// The per-frame question: the player is at `position_ms`; `sweep` is whether the listener wants the
    /// fill (it only happens when the lyrics allow it), `lively` whether the words may rise and glow as
    /// they are sung (not with movement reduced). `force` draws the new moment whatever changed, for the
    /// first look after starting or resuming.
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

    /// What is on screen now.
    pub fn shown(&self) -> Frame {
        self.timing.frame(self.shown.load(Relaxed))
    }

    /// The moment on screen, the nudge in it: what a word's rise and glow are drawn for.
    pub fn shown_ms(&self) -> i64 {
        self.shown.load(Relaxed)
    }

    /// How far into the lit line's backing vocals the singing is, at the moment on screen.
    pub fn backing_sung(&self) -> f32 {
        let t = self.shown.load(Relaxed);
        match usize::try_from(self.timing.line_at(t)) {
            Ok(line) if self.timing.synced => self.timing.backing_sung(line, t),
            _ => 0.0,
        }
    }

    /// A tap on `line`: shows it at once and returns where the player should seek to, which is the line's
    /// timestamp less the offset and the nudge, so the words land where they are drawn. Until the player is clearly
    /// playing the line, a reading a little before it (a seek landed early, a playhead set back after
    /// running on) shows what is on screen rather than the line before or its words emptied again: the
    /// tapped line becomes the one sung, its first word fills from the start, and nothing goes back.
    pub fn tap(&self, line: usize) -> i64 {
        let Some(&start) = self.timing.starts.get(line) else { return self.shown.load(Relaxed).max(0) };
        self.shown.store(start, Relaxed);
        self.landing.store(start, Relaxed);
        (start - self.offset - self.nudge.load(Relaxed)).max(0)
    }

    /// Lyrics taking the place of the ones on screen for the same song (finer words, or the same words
    /// timed again against the song's voice) start on `line`, the one standing for the line that was lit
    /// ([`matching_line`]). Started from the playhead alone, a new timing a moment behind the old one lit
    /// the line before it: the page went back a line, and on again as the song got there. Instead the line
    /// is shown at once and landed on as a tap is, with no seek: a reading up to [`LAND_EARLY_MS`] before
    /// it holds it while the song gets there, and one further back is the new timing's word, shown as it
    /// is. Nothing is held when the new timing is already on the line or past it.
    pub fn land(&self, line: usize) {
        let Some(&start) = self.timing.starts.get(line) else { return };
        let shown = self.shown.load(Relaxed);
        if !self.timing.synced || self.timing.line_at(shown) >= line as i32 || shown < start - LAND_EARLY_MS {
            return;
        }
        self.shown.store(start, Relaxed);
        self.landing.store(start, Relaxed);
    }

    /// The moment to show for a reading `t`, while a tap is being landed: see [`LyricClock::tap`].
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
        // Well before it, or far behind what is shown: somewhere else altogether (another seek).
        self.landing.store(NOT_LANDING, Relaxed);
        t
    }

    /// `dir` > 0 moves the words sooner, < 0 later, 0 puts them back; returns the nudge in ms. Picked up
    /// by the next [`LyricClock::advance`].
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

// ---- one i64 per answer -------------------------------------------------------------------------------------

const SUNG_BITS: u32 = 29;
/// Fraction bits of the sung offset: it crosses within a quarter of a millionth of a character of what was
/// worked out, a ten-thousandth of a pixel on the widest glyph, for lines of up to 2047 characters.
pub const SUNG_FRAC: u32 = 18;
const ACTIVE_BITS: u32 = 13;
const GLIDE_BITS: u32 = 10;
const WAIT_BITS: u32 = 9;
const ACTIVE_AT: u32 = SUNG_BITS;
const GLIDE_AT: u32 = ACTIVE_AT + ACTIVE_BITS;
const WAIT_AT: u32 = GLIDE_AT + GLIDE_BITS;
const STILL_AT: u32 = WAIT_AT + WAIT_BITS;
const REDRAW_AT: u32 = STILL_AT + 1;
/// The bits of a packed [`Step`] that describe the [`Frame`]; the rest say when to ask again.
pub const FRAME_BITS: u32 = WAIT_AT;

fn field(v: i64, bits: u32) -> i64 {
    v.clamp(0, (1 << bits) - 1)
}

impl Frame {
    /// `sung` in the low 29 bits (fixed point, [`SUNG_FRAC`] fraction bits), then `active + 1` in 13 and
    /// `glide_ms` in 10.
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
    /// The frame's bits ([`Frame::pack`]), then `wait` in 9, `still` in 1 and `redraw` in 1: 63 bits,
    /// never negative.
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
    fn finer_lyrics_carry_the_line_on_screen_over_by_its_time() {
        let lines = [0, 4_000, 8_000, 12_000, 16_000];
        // Split differently: the line starting nearest to the old one's start.
        let finer = [0, 2_000, 4_100, 6_000, 7_900, 10_000, 12_050];
        assert_eq!(matching_line(&lines, 1, &finer, true), 2);
        assert_eq!(matching_line(&lines, 2, &finer, true), 4);
        assert_eq!(matching_line(&lines, 4, &finer, true), 6);
        assert_eq!(matching_line(&[0, 3_000], 1, &[0, 2_000, 4_000], true), 1, "the earlier of two as near");
        // Untimed words keep the same number, inside the new list.
        assert_eq!(matching_line(&lines, 3, &finer, false), 3);
        assert_eq!(matching_line(&lines, 4, &[-1, -1], false), 1);
        assert_eq!(matching_line(&[-1, -1], 1, &finer, true), 1, "an old line with no time");
        assert_eq!(matching_line(&[], 9, &finer, true), 6);
        assert_eq!(matching_line(&lines, 4, &[], true), 0);
    }

    #[test]
    fn lines_are_lit_by_where_the_singing_is() {
        assert_eq!(line_strength(true, 3, 3), 1.0);
        assert_eq!(line_strength(true, 2, 3), 0.35 * 0.55);
        assert_eq!(line_strength(true, 4, 3), 0.35);
        assert_eq!(line_strength(true, 0, -1), 0.35, "before the first line, all are still to come");
        assert_eq!(line_strength(false, 0, 5), 1.0);
        assert!(keeps_screen_on(true, true, true) && !keeps_screen_on(true, true, false) && !keeps_screen_on(false, true, true) && !keeps_screen_on(true, false, true));
        assert_eq!(READING_MS, 4_000);
    }

    fn lines(starts: &[i64]) -> Vec<Line> {
        starts.iter().map(|&s| Line { start_ms: s, len: 10, words: Vec::new(), ..Default::default() }).collect()
    }

    fn w(start_ms: i64, end_ms: i64, start: u32, end: u32) -> Word {
        Word { start_ms, end_ms, start, end }
    }

    #[test]
    fn a_change_is_85_percent_of_the_gap_between_160_and_620_ms() {
        let t = LyricTiming::new(true, false, lines(&[0, 100, 400, 1000, 11_000, 12_000]));
        // 100 ms gap -> 85 -> 160; 300 -> 255; 600 -> 510; 10 s -> 620; 1 s -> 850 -> 620; last -> 620.
        assert_eq!(t.glide, [160, 255, 510, 620, 620, 620]);
        assert_eq!((t.glide_ms(-1), t.glide_ms(6), t.glide_ms(2)), (GLIDE_MS, GLIDE_MS, 510));
        // 0.85 of 301 ms is 255.85, truncated as the app did.
        assert_eq!(LyricTiming::new(true, false, lines(&[0, 301])).glide[0], 255);
    }

    #[test]
    fn a_line_takes_over_half_its_change_early_but_never_before_the_one_ahead() {
        let t = LyricTiming::new(true, false, lines(&[1000, 1100, 1110, 5000]));
        // Glides 160, 160, 620 (3890 gap -> 3306 -> 620), 620: leads 80, 80, 310, 310.
        assert_eq!(t.switch_at, [920, 1020, 1021, 4690]);
        assert_eq!((t.line_at(919), t.line_at(920), t.line_at(1020), t.line_at(1021), t.line_at(4689), t.line_at(4690)), (-1, 0, 1, 2, 2, 3));
        // After the last line takes over, the next change is its end (no end given: `LAST_LINE_MS` on).
        assert_eq!((t.next_switch_after(0), t.next_switch_after(1021), t.next_switch_after(4690)), (Some(920), Some(4690), Some(5000 + LAST_LINE_MS)));
        assert_eq!(t.next_switch_after(5000 + LAST_LINE_MS), None);
    }

    #[test]
    fn the_sweep_is_linear_in_a_word_and_rests_between_words() {
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 1000, len: 11, words: vec![w(1000, 1400, 0, 5), w(1600, 2000, 6, 11)], ..Default::default() }]);
        let at = |ms| t.sung_offset(0, ms);
        assert_eq!((at(900), at(1000), at(1200), at(1400), at(1500), at(1600), at(1700), at(2000), at(9000)), (0.0, 0.0, 2.5, 5.0, 5.0, 5.0, 7.25, 11.0, 11.0));
        // A word with no length is done the moment it starts; a line without words is all or nothing.
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 0, len: 4, words: vec![w(500, 500, 0, 4)], ..Default::default() }, Line { start_ms: 800, len: 7, words: vec![], ..Default::default() }]);
        assert_eq!((t.sung_offset(0, 499), t.sung_offset(0, 500), t.sung_offset(1, 799), t.sung_offset(1, 800)), (0.0, 4.0, 0.0, 7.0));
    }

    #[test]
    fn a_last_word_before_a_long_pause_is_filled_by_its_own_end_not_the_next_lines_start() {
        // "Hold on tonight" at 10 s, the last word's end left to the next line, 30 s later.
        let words = vec![w(10_000, 10_400, 0, 4), w(10_400, 10_800, 5, 7), w(10_800, 40_000, 8, 15)];
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 10_000, len: 15, words, ..Default::default() }, Line { start_ms: 40_000, len: 5, ..Default::default() }]);
        let full_by = 10_800 + word_ms_estimate(7);
        assert!(full_by < 12_000, "a seven-letter word is sung in about a second");
        assert_eq!(t.sung_offset(0, full_by), 15.0, "full by its own end");
        assert_eq!(t.sung_offset(0, 25_000), 15.0, "and stays full through the pause");
        // On its way there it moves a little every frame, never a step (the space before it is crossed
        // as the word starts, as between any two words).
        let mut was = t.sung_offset(0, 10_801);
        for ms in (10_816..=full_by).step_by(16) {
            let now = t.sung_offset(0, ms);
            assert!(now >= was && now - was < 0.25, "{was} -> {now} at {ms}");
            was = now;
        }
        // A note held up to the next line, as long as a note is held, is left alone.
        let held = vec![w(1_000, 1_300, 0, 4), w(1_300, 3_000, 5, 9)];
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 1_000, len: 9, words: held, ..Default::default() }, Line { start_ms: 3_000, len: 3, ..Default::default() }]);
        assert_eq!(t.sung_offset(0, 2_150), 7.0);
    }

    #[test]
    fn a_line_whose_words_are_sung_is_filled_to_its_end() {
        // The source's last word stops before the closing bracket.
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 0, len: 10, words: vec![w(0, 500, 1, 4), w(500, 1_000, 5, 9)], ..Default::default() }]);
        assert_eq!(t.sung_offset(0, 1_000), 10.0);
        assert_eq!(t.sung_offset(0, 750), 7.5, "the bracket is filled within the last word");
    }

    #[test]
    fn the_line_sung_follows_timestamps_and_the_line_lit_leads_it() {
        let t = LyricTiming::new(true, true, lines(&[1000, 3000]));
        assert_eq!((t.sung_line(999), t.sung_line(1000), t.sung_line(2999), t.sung_line(3000)), (None, Some(0), Some(0), Some(1)));
        assert_eq!(t.line_at(2700), 1, "lit 310 ms before it is sung");
        // Out of order starts still find the last one reached, as the app's indexOfLast did.
        let t = LyricTiming::new(true, true, lines(&[1000, 500, 2000]));
        assert_eq!((t.sung_line(600), t.sung_line(1500)), (Some(1), Some(1)));
    }

    #[test]
    fn waking_is_at_the_next_change_within_8_to_500_ms_and_every_second_frame_when_sweeping() {
        let t = LyricTiming::new(true, false, lines(&[1000, 1200, 5000]));
        // Glides 170, 620, 620; switch_at: 1000-85=915, 1200-310=890 -> 916, 5000-310=4690.
        assert_eq!((t.wait(0, false, false), t.wait(700, false, false), t.wait(914, false, false), t.wait(921, false, false), t.wait(4700, false, false)), (500, 215, 8, 500, 500));
        assert_eq!(t.wait(0, true, false), SWEEP_FRAMES);
        assert_eq!(LyricTiming::new(false, false, lines(&[-1, -1])).wait(0, false, false), 0, "untimed lyrics never wake");
    }

    #[test]
    fn only_a_visible_change_of_the_sweep_redraws() {
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 1000, len: 10, words: vec![w(1000, 2000, 0, 10)], ..Default::default() }, Line { start_ms: 5000, len: 3, words: vec![], ..Default::default() }]);
        assert!(t.moved(0, 500, false, false), "without the sweep every wake-up is a change");
        assert!(t.moved(0, 500, true, false), "before the first line");
        assert!(!t.moved(1000, 1003, true, false), "0.03 characters");
        assert!(t.moved(1000, 1005, true, false), "0.05 characters");
        assert!(!t.moved(2000, 4000, true, false), "between words nothing moves");
        // The lit line changes at 4690 but the sung one only at 5000: the page holds until then.
        assert!(!t.moved(2000, 4800, true, false) && t.line_at(4800) == 1);
        assert!(t.moved(2000, 5000, true, false));
    }

    #[test]
    fn a_clock_draws_only_what_changed_and_nudges_and_taps() {
        let timing = LyricTiming::new(true, true, vec![Line { start_ms: 1000, len: 10, words: vec![w(1000, 2000, 0, 10)], ..Default::default() }, Line { start_ms: 5000, len: 3, words: vec![], ..Default::default() }]);
        let c = LyricClock::new(timing, 0);
        assert_eq!(c.shown(), Frame { active: -1, glide_ms: GLIDE_MS, sung: 0.0 });
        let s = c.advance(1500, true, false, false);
        assert_eq!(s, Step { frame: Frame { active: 0, glide_ms: 620, sung: 5.0 }, wait: SWEEP_FRAMES, still: false, redraw: true });
        let s = c.advance(1502, true, false, false);
        assert!(!s.redraw && s.frame.sung == 5.0, "what is on screen stays");
        assert!(c.advance(1502, true, false, true).redraw);
        assert_eq!(c.nudge(1), 250);
        assert_eq!(c.advance(1500, true, false, false).frame.sung, 7.5, "sooner: the words are ahead of the player");
        assert_eq!((c.nudge(-1), c.nudge(-1), c.nudge(-1)), (0, -250, -500));
        assert_eq!(c.tap(1), 5500, "seek to where the line is drawn");
        assert_eq!(c.shown().active, 1);
        assert_eq!(c.nudge(0), 0);
        assert_eq!(c.tap(0), 1000);
        c.nudge(1);
        c.nudge(1);
        assert_eq!(c.tap(0), 500);
        // Words not timed: no sweep whatever the listener asked for, and a wake-up at the next line.
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&[1000, 5000])), 0);
        assert_eq!(c.advance(0, true, false, false).wait, 500);
        assert_eq!(c.advance(4400, true, false, false).wait, 290);
    }

    #[test]
    fn lyrics_that_run_late_are_shown_on_time_and_tapped_where_they_are_sung() {
        // The lines' times run 1.5 s later than the song: the line timed at 5000 is sung at 3500.
        let c = LyricClock::with_offset(LyricTiming::new(true, false, lines(&[1000, 5000])), 0, 1500);
        assert_eq!(c.advance(2500, false, false, false).frame.active, 0, "the line timed at 1000 while the song is at 2500");
        assert_eq!(c.advance(3600, false, false, false).frame.active, 1, "the line timed at 5000 lit as it is sung");
        assert_eq!(c.tap(1), 3500, "a tap seeks to where the line is sung");
        c.nudge(0);
        assert_eq!(c.nudge(1), 250, "the listener's nudge is their own, on top");
        assert_eq!(c.tap(1), 3250);
    }

    #[test]
    fn a_tapped_line_is_the_one_sung_even_when_the_seek_lands_just_before_it() {
        // Line 1 starts at 5000 with its first word from 5000 to 5400; line 0 is sung before it.
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
        // The seek landed 26 ms early (an MP3 frame): still the tapped line, its words not yet begun, and
        // not the line before for a frame.
        let s = c.advance(4974, true, false, false);
        assert_eq!((s.frame.active, s.frame.sung), (1, 0.0));
        assert_eq!(c.shown_ms(), 5000);
        // As the music reaches the line, its first word fills from the start.
        let s = c.advance(5100, true, false, false);
        assert_eq!((s.frame.active, s.frame.sung), (1, 1.0));
        // The player's count ran on from the tap and is set back to where the seek really landed: the fill
        // holds where it is until the music catches up, rather than emptying and filling again.
        let s = c.advance(5000, true, false, false);
        assert_eq!((s.frame.active, s.frame.sung), (1, 1.0), "never back to the start of the line");
        assert!(!s.redraw);
        let s = c.advance(5200, true, false, false);
        assert_eq!((s.frame.active, s.frame.sung), (1, 2.0));
        // Once well into the line the player's word is the truth again: a seek back is shown as it is.
        c.advance(8100, true, false, false);
        assert_eq!(c.advance(4500, true, false, false).frame.active, 0);
        // A seek somewhere else entirely during a landing is shown at once.
        c.tap(1);
        assert_eq!(c.advance(1500, true, false, false).frame.active, 0, "well before the tapped line");
        c.tap(1);
        assert_eq!(c.advance(9100, true, false, false).frame.active, 2, "later is always shown");
        // A tap on the first line, landed a little early, is not "before the lyrics".
        let c = LyricClock::new(LyricTiming::new(true, true, vec![Line { start_ms: 800, len: 4, words: vec![w(800, 1200, 0, 4)], ..Default::default() }]), 30_000);
        assert_eq!(c.tap(0), 800);
        assert_eq!(c.advance(0, true, false, false).frame.active, 0, "seek to 800 landed at 0");
    }

    #[test]
    fn backing_vocals_fill_on_their_own_time_and_moving_words_draw_every_frame() {
        let line = Line { start_ms: 1000, len: 5, words: vec![w(1000, 1500, 0, 5)], backing_len: 4, backing: vec![w(1600, 2000, 0, 4)], ..Default::default() };
        let t = LyricTiming::new(true, true, vec![line, Line { start_ms: 9000, len: 3, ..Default::default() }]);
        assert_eq!((t.backing_sung(0, 1500), t.backing_sung(0, 1800), t.backing_sung(0, 2000)), (0.0, 2.0, 4.0));
        assert_eq!(t.sung_offset(1, 9000), 3.0, "the backing words are not the next line's");
        assert!(t.moving(1200) && t.moving(2000 + MOTION_TAIL_MS) && !t.moving(2001 + MOTION_TAIL_MS));
        assert_eq!((t.wait(1200, true, true), t.wait(1200, true, false), t.wait(5000, true, true)), (1, SWEEP_FRAMES, SWEEP_FRAMES));
        // A word settling after the fill is done is a change only while words move at all.
        assert!(t.moved(2100, 2110, true, true) && !t.moved(2100, 2110, true, false));
        let c = LyricClock::new(t, 0);
        c.advance(1800, true, true, false);
        assert_eq!((c.shown_ms(), c.backing_sung()), (1800, 2.0));
    }

    #[test]
    fn a_sweep_sleeps_while_nothing_on_its_line_changes() {
        let line = Line { start_ms: 1000, len: 11, words: vec![w(1000, 1400, 0, 5), w(1600, 2000, 6, 11)], backing_len: 3, backing: vec![w(2200, 2300, 0, 3)], ..Default::default() };
        let t = LyricTiming::new(true, true, vec![line, Line { start_ms: 2600, len: 3, ..Default::default() }]);
        // Before the first line: until it takes over (310 ms ahead of its timestamp).
        assert_eq!(t.quiet_ms(0, false), Some(500));
        assert_eq!(t.quiet_ms(600, false), Some(90));
        // Inside a word, and inside a backing word: every frame.
        assert_eq!((t.quiet_ms(1200, false), t.quiet_ms(2250, false)), (None, None));
        // Between words, until the next one; then the backing words; then the next line.
        assert_eq!((t.quiet_ms(1450, false), t.quiet_ms(2000, false), t.quiet_ms(2300, false)), (Some(150), Some(200), Some(300)));
        // Words that still settle or glow keep it drawing every frame.
        assert_eq!((t.quiet_ms(1450, true), t.quiet_ms(2300, true)), (None, None));
        let c = LyricClock::new(t, 0);
        let s = c.advance(1450, true, false, false);
        assert_eq!((s.wait, s.still), (150, true));
        let s = c.advance(1450, false, false, false);
        assert!(!s.still, "without the sweep the wait is milliseconds anyway");
        assert_eq!((c.advance(1200, true, false, false).wait, c.advance(1200, true, false, false).still), (SWEEP_FRAMES, false));
    }

    #[test]
    fn the_last_line_is_drawn_as_sung_once_it_is_over() {
        // Two lines timed by the line; the last one ends at 8 s, and nothing follows it.
        let two = vec![Line { start_ms: 1000, len: 5, ..Default::default() }, Line { start_ms: 5000, end_ms: 8000, len: 5, ..Default::default() }];
        let t = LyricTiming::new(true, false, two.clone());
        assert_eq!((t.frame(7999).active, t.frame(8000).active, t.frame(600_000).active), (1, 2, 2), "lit while sung, then past, for good");
        assert_eq!(line_strength(true, 1, t.frame(8000).active), PAST_LINE, "dimmed as a sung line is");
        assert_eq!(t.frame(8000).sung, 0.0);
        assert_eq!(t.frame(6000).active, 1, "a seek back into it lights it again");
        // The page wakes as it ends, not up to half a second later, and not before.
        assert_eq!((t.wait(7700, false, false), t.next_switch_after(7700), t.next_switch_after(8000)), (300, Some(8000), None));
        let c = LyricClock::new(t, 7000);
        assert_eq!(c.advance(7000, false, false, true).frame.active, 1);
        assert_eq!(c.advance(8000, false, false, false).frame.active, 2);
        // A source that does not say when it ends: as long as a line-timed file's last line is taken to last.
        let t = LyricTiming::new(true, false, lines(&[1000, 5000]));
        assert_eq!((t.frame(5000 + LAST_LINE_MS - 1).active, t.frame(5000 + LAST_LINE_MS).active), (1, 2));
        // Word by word: over once its words are sung and have stopped moving, and the sweep, which holds
        // the page while nothing in the line moves, still draws it going dim.
        let words = vec![w(5000, 5500, 0, 2), w(5500, 6000, 3, 5)];
        let t = LyricTiming::new(true, true, vec![Line { start_ms: 1000, len: 5, words: vec![w(1000, 2000, 0, 5)], ..Default::default() }, Line { start_ms: 5000, end_ms: 6000, len: 5, words, ..Default::default() }]);
        assert_eq!((t.frame(6000 + MOTION_TAIL_MS - 1).active, t.frame(6000 + MOTION_TAIL_MS).active), (1, 2));
        assert_eq!(t.quiet_ms(6100, false), Some((MOTION_TAIL_MS - 100) as u32), "asleep until it is over");
        let c = LyricClock::new(t, 0);
        assert_eq!(c.advance(5800, true, false, true).frame.active, 1);
        let s = c.advance(6000 + MOTION_TAIL_MS, true, false, false);
        assert!(s.redraw && s.frame.active == 2, "{s:?}");
        let s = c.advance(9000, true, false, false);
        assert!(!s.redraw && s.frame.active == 2, "and nothing more to draw: {s:?}");
        // Lyrics that are not timed have no last line to be over.
        assert_eq!(LyricTiming::new(false, false, lines(&[-1, -1])).frame(600_000).active, -1);
    }

    #[test]
    fn lyrics_timed_again_for_the_same_song_start_on_the_line_that_was_lit() {
        // The words on screen: the second line lit 200 ms into it.
        let old = [1000, 5000, 9000];
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&old)), 0);
        assert_eq!(c.advance(5200, false, false, true).frame.active, 1);
        // The same song's words again, timed 800 ms later (a better fit to the song's voice). Started from
        // the playhead alone they lit the first line again, and the second only as the song got there:
        // the page went back a line and on again.
        let next = [1800, 5800, 9800];
        let line = matching_line(&old, 1, &next, true);
        assert_eq!(line, 1);
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&next)), 5200);
        c.land(line as usize);
        assert_eq!(c.shown().active, 1, "on the line that was lit, at once");
        for ms in (5200..6500).step_by(16) {
            assert_eq!(c.advance(ms, false, false, ms == 5200).frame.active, 1, "never back a line, at {ms} ms");
        }
        assert_eq!(c.advance(10_000, false, false, false).frame.active, 2, "and on with the song");
        assert_eq!(c.advance(2_500, false, false, false).frame.active, 0, "a seek back is shown as it is");
        // A new timing that puts the song well before the line is the new timing's word: nothing is held.
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&[3000, 7000, 11_000])), 5200);
        c.land(1);
        assert_eq!(c.advance(5200, false, false, true).frame.active, 0);
        // Nor when the new timing is already on the line: it goes on from where it is.
        let c = LyricClock::new(LyricTiming::new(true, false, lines(&[500, 4000, 9000])), 5200);
        c.land(1);
        assert_eq!(c.shown_ms(), 5200);
    }

    #[test]
    fn untimed_lyrics_light_nothing_and_never_wake() {
        let c = LyricClock::new(LyricTiming::new(false, false, lines(&[-1, -1, -1])), 0);
        let s = c.advance(10_000, true, false, true);
        assert_eq!(s, Step { frame: Frame { active: -1, glide_ms: GLIDE_MS, sung: 0.0 }, wait: 0, still: false, redraw: true });
    }

    #[test]
    fn a_packed_step_comes_back() {
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
