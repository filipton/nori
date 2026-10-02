//! Whether synced lyrics fit the song's audio, checked against its vocal activity curve (AutoMix
//! analysis): a score (lines starting where a voice is heard, less for lines over instrumental stretches
//! and singing with no line), the best global offset within ±[`REACH_MS`] with its confidence, and drift
//! (two parts of the song wanting different offsets: another version's timing). The result is a
//! [`SyncKind`]; trust.rs weighs the score and a sure offset is applied when the lyrics are shown.

use nori_model::Lyrics;
use nori_player::automix::vocal::VocalCurve;

/// How far the lines are slid each way for the best fit.
pub(crate) const REACH_MS: i64 = 3_000;
/// Each part is slid further: a version's timing can be off by more than the whole's.
const PART_REACH_MS: i64 = 5_000;
/// Fewer timed lines than this say too little.
const MIN_LINES: usize = 4;
// The thresholds were tuned on 59 songs of a real library (340 service answers), not demoting good ones.
/// A smaller offset is left alone.
pub(crate) const OFFSET_MIN_MS: i64 = 250;
/// An offset is applied only this sure; lower lets another song's lines slide to where they fit.
pub(crate) const OFFSET_SURE: f64 = 0.4;
/// Parts whose offsets are this far apart drift, when each is [`PART_SURE`] of its own...
pub const DRIFT_MS: i64 = 700;
const PART_SURE: f64 = 0.3;
/// ...and they fit this much better apart, per line, than together.
const PART_GAIN: f64 = 0.1;
/// Below this score the lines do not fit the voice...
pub(crate) const POOR: f64 = 0.35;
/// ...nor when they fit less than this better than slid far off: with a band as busy as the voice,
/// any timing scores well.
const LIFT: f64 = 0.06;
/// The curve hears a word at its pitched vowel, this long after the timing's first consonant...
const LAG_MS: i64 = 100;
/// ...and services start a line-timed line this much before the voice, to be read.
const LINE_LEAD_MS: i64 = 100;
/// How much the voice rising at line starts counts beside the spans being sung.
const ONSET_WEIGHT: f64 = 1.0;
/// Seconds of activity either side of a line's start that show the voice starting.
const RISE_S: f64 = 0.4;
/// A line's first seconds, where the voice should be heard.
const ONSET_S: f64 = 0.6;
/// A line with no end of its own is sung for about this long per character, within [`SPAN_MIN_S`, `SPAN_MAX_S`].
const SECS_PER_CHAR: f64 = 0.09;
const SPAN_MIN_S: f64 = 1.5;
const SPAN_MAX_S: f64 = 6.0;
/// A line that starts this soon after the one before ended needs no rise of its own: the voice ran on.
const RUN_ON_S: f64 = 0.6;
/// Activity (0 to 1) below this is an instrumental stretch...
const QUIET: f32 = 0.2;
/// ...and above this singing.
const SUNG: f32 = 0.5;
/// Stretches of singing with no line count against the lyrics only when at least this long.
const UNCOVERED_S: f64 = 3.0;

/// What the check made of the lyrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKind {
    /// Too little to go on: few timed lines, or a song with no voice the curve could find.
    Unsure,
    /// The lines start where the voice does.
    Fits,
    /// They fit once shifted by the offset, which is applied when they are shown.
    Shifted,
    /// The halves want different offsets: timed for another version of the song.
    Drifts,
    /// The lines do not fit the voice at any offset.
    Poor,
}

/// The check of one set of lyrics against one song's curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncCheck {
    pub kind: SyncKind,
    /// 0 to 1, at the offset applied (none unless `kind` is `Shifted`).
    pub score: f64,
    /// 0 to 1, at the best offset whether it is applied or not.
    pub best_score: f64,
    /// What to add to the playhead to find the lyrics' time: the lyrics run this much later than the audio.
    /// The best offset found, applied only when `kind` is `Shifted`.
    pub offset_ms: i64,
    /// 0 to 1: how clearly the best offset stands out from every other.
    pub confidence: f64,
    /// The second half's best offset less the first half's; 0 when either half is unsure.
    pub drift_ms: i64,
}

impl SyncCheck {
    /// The offset to show the lyrics with.
    pub(crate) fn applied_ms(&self) -> i64 {
        if self.kind == SyncKind::Shifted {
            self.offset_ms
        } else {
            0
        }
    }
}

/// A timed stretch of the lyrics, in curve frames (fractional): where a voice should sound.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: f64,
    end: f64,
    /// Whether the line before ran straight into this one.
    run_on: bool,
}

/// The curve made ready: smoothed, scaled to 0..1 between its quiet and its sung level, with running sums.
struct Voice {
    fps: f64,
    t0: f64,
    act: Vec<f32>,
    /// `sum[k]` is the sum of `act[..k]`.
    sum: Vec<f64>,
    /// The activity over a second, for telling instrumental stretches and sung ones.
    slow: Vec<f32>,
}

fn box_mean(x: &[f32], r: usize) -> Vec<f32> {
    let mut c = vec![0f64; x.len() + 1];
    for (i, v) in x.iter().enumerate() {
        c[i + 1] = c[i] + *v as f64;
    }
    (0..x.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(r), (i + r + 1).min(x.len()));
            ((c[b] - c[a]) / (b - a) as f64) as f32
        })
        .collect()
}

fn percentile(v: &[f32], p: f64) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s.get(((s.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0)
}

impl Voice {
    fn new(c: &VocalCurve) -> Option<Self> {
        let fps = c.fps as f64;
        if fps.is_nan() || fps <= 0.0 || c.level.len() < (fps * 20.0) as usize {
            return None;
        }
        let raw: Vec<f32> = c.level.iter().map(|v| *v as f32).collect();
        let smooth = box_mean(&raw, (0.08 * fps).round() as usize);
        // 0 is the song's quiet stretches, 1 its busiest singing; saturating lower makes a busy band fit anywhere.
        let (lo, hi) = (percentile(&smooth, 0.15), percentile(&smooth, 0.9));
        // A curve that hardly moves has nothing to say: no voice, or voice all through.
        if hi - lo < 12.0 {
            return None;
        }
        let act: Vec<f32> = smooth.iter().map(|v| ((v - lo) / (hi - lo)).clamp(0.0, 1.0)).collect();
        let mut sum = vec![0f64; act.len() + 1];
        for (i, v) in act.iter().enumerate() {
            sum[i + 1] = sum[i] + *v as f64;
        }
        let slow = box_mean(&act, (0.5 * fps).round() as usize);
        Some(Voice { fps, t0: c.t0 as f64, act, sum, slow })
    }

    fn len(&self) -> usize {
        self.act.len()
    }

    /// Frame of `ms`, fractional.
    fn frame(&self, ms: f64) -> f64 {
        (ms / 1000.0 - self.t0) * self.fps
    }

    /// Mean activity over frames `[a, b)`, clipped to the curve; None when nothing of it is on the curve.
    fn mean(&self, a: f64, b: f64) -> Option<f64> {
        let n = self.len() as f64;
        let (a, b) = (a.max(0.0).min(n), b.max(0.0).min(n));
        let (ia, ib) = (a.round() as usize, b.round() as usize);
        (ib > ia).then(|| (self.sum[ib] - self.sum[ia]) / (ib - ia) as f64)
    }

    /// Sum of activity and frames over `[a, b)`, clipped.
    fn total(&self, a: f64, b: f64) -> (f64, f64) {
        let n = self.len() as f64;
        let (ia, ib) = (a.max(0.0).min(n).round() as usize, b.max(0.0).min(n).round() as usize);
        if ib > ia {
            (self.sum[ib] - self.sum[ia], (ib - ia) as f64)
        } else {
            (0.0, 0.0)
        }
    }
}

/// The lyrics' sung stretches, in curve frames, in order: each word of word-timed lyrics, else each line from
/// its start to its end (or the next line), no longer than its words take to sing.
fn spans(l: &Lyrics, v: &Voice) -> Vec<Span> {
    let lines: Vec<&nori_model::LyricLine> = l.lines.iter().filter(|x| x.start_ms >= 0 && !x.text.trim().is_empty() && !x.background).collect();
    let mut out: Vec<Span> = Vec::new();
    let mut last_end = f64::NEG_INFINITY;
    for (i, line) in lines.iter().enumerate() {
        let start = line.start_ms as f64;
        let next = lines.get(i + 1).map(|n| n.start_ms as f64).filter(|n| *n > start);
        let chars = line.text.chars().filter(|c| c.is_alphanumeric()).count() as f64;
        let singing = (chars * SECS_PER_CHAR).clamp(SPAN_MIN_S, SPAN_MAX_S) * 1000.0;
        let words: Vec<(f64, f64)> = if l.word_timed { line.words.iter().filter(|w| w.end_ms > w.start_ms).map(|w| (w.start_ms as f64, w.end_ms as f64)).collect() } else { Vec::new() };
        let end = if let Some(w) = words.last() {
            w.1
        } else {
            let given = (line.end_ms as f64 > start).then_some(line.end_ms as f64);
            let bound = [given, next].into_iter().flatten().fold(f64::INFINITY, f64::min);
            bound.min(start + singing)
        };
        let run_on = start - last_end <= RUN_ON_S * 1000.0;
        if words.is_empty() {
            out.push(Span { start: v.frame(start), end: v.frame(end.max(start + 200.0)), run_on });
        } else {
            for (k, (a, b)) in words.iter().enumerate() {
                out.push(Span { start: v.frame(*a), end: v.frame(*b), run_on: k > 0 || run_on });
            }
        }
        last_end = end;
    }
    out
}

/// The line starts, in curve frames, each with whether the line before ran into it.
fn starts(l: &Lyrics, spans: &[Span], v: &Voice) -> Vec<(f64, bool)> {
    if !l.word_timed {
        return spans.iter().map(|s| (s.start, s.run_on)).collect();
    }
    // Word-timed: the first word of each line.
    let mut out = Vec::new();
    let mut last_end = f64::NEG_INFINITY;
    for line in l.lines.iter().filter(|x| x.start_ms >= 0 && !x.text.trim().is_empty() && !x.background) {
        let start = line.words.first().map_or(line.start_ms, |w| w.start_ms) as f64;
        out.push((v.frame(start), start - last_end <= RUN_ON_S * 1000.0));
        last_end = line.words.last().map_or(line.end_ms, |w| w.end_ms) as f64;
    }
    out
}

/// How much the voice rises at frame `f`: the activity over the [`RISE_S`] after it less the one before.
fn rise(v: &Voice, f: f64) -> f64 {
    let r = RISE_S * v.fps;
    match (v.mean(f - r, f), v.mean(f, f + r)) {
        (Some(b), Some(a)) => a - b,
        _ => 0.0,
    }
}

/// How well `spans` (and `starts`) fit the voice shifted by `shift` frames: the mean activity inside the
/// spans less the mean around them, plus the mean rise at the starts ([`ONSET_WEIGHT`]).
fn fit(v: &Voice, spans: &[Span], starts: &[(f64, bool)], shift: f64) -> f64 {
    let (Some(first), Some(last)) = (spans.first(), spans.last()) else { return 0.0 };
    let margin = 2.0 * v.fps;
    let (all, all_n) = v.total(first.start + shift - margin, last.end + shift + margin);
    let (mut inside, mut inside_n) = (0.0, 0.0);
    for s in spans {
        let (a, n) = v.total(s.start + shift, s.end + shift);
        inside += a;
        inside_n += n;
    }
    let out_n = all_n - inside_n;
    if inside_n < 1.0 || out_n < 1.0 {
        return 0.0;
    }
    let contrast = inside / inside_n - (all - inside) / out_n;
    let onsets = starts.iter().filter(|(_, run_on)| !run_on).map(|(f, _)| rise(v, f + shift)).sum::<f64>() / starts.len().max(1) as f64;
    contrast + ONSET_WEIGHT * onsets
}

/// The best shift in frames within `reach` frames each way (to a fraction of a frame), its fit, and how
/// clearly it beats every shift more than half a second from it (0 to 1).
fn best_shift(v: &Voice, spans: &[Span], starts: &[(f64, bool)], reach: i64) -> (f64, f64, f64) {
    let fits: Vec<f64> = (-reach..=reach).map(|s| fit(v, spans, starts, s as f64)).collect();
    let (i, best) = fits.iter().enumerate().fold((0, f64::MIN), |b, (i, f)| if *f > b.1 { (i, *f) } else { b });
    // A parabola through the best and its neighbours puts the peak between frames.
    let frac = match (i.checked_sub(1).and_then(|j| fits.get(j)), fits.get(i + 1)) {
        (Some(a), Some(c)) => {
            let d = a - 2.0 * best + c;
            if d < -1e-12 {
                (0.5 * (a - c) / d).clamp(-0.5, 0.5)
            } else {
                0.0
            }
        }
        _ => 0.0,
    };
    let apart = (0.5 * v.fps).ceil() as usize;
    let rival = fits.iter().enumerate().filter(|(j, _)| j.abs_diff(i) > apart).map(|(_, f)| *f).fold(f64::MIN, f64::max);
    let confidence = if best <= 0.0 {
        0.0
    } else if rival == f64::MIN {
        1.0
    } else {
        ((best - rival) / (0.3 * best)).clamp(0.0, 1.0)
    };
    (i as f64 - reach as f64 + frac, best, confidence)
}

/// The score under a shift of `shift` frames: see the module's comment.
fn score_at(v: &Voice, spans: &[Span], starts: &[(f64, bool)], shift: f64) -> f64 {
    if starts.is_empty() {
        return 0.0;
    }
    let on = ONSET_S * v.fps;
    // Lines that start where the voice is heard, having risen (or run on from the line before).
    let hits: f64 = starts
        .iter()
        .map(|(f, run_on)| {
            let f = f + shift;
            let present = v.mean(f, f + on).map_or(0.0, |a| (a / 0.5).clamp(0.0, 1.0));
            let risen = if *run_on { 1.0 } else { (0.5 + 2.0 * rise(v, f)).clamp(0.0, 1.0) };
            present * risen
        })
        .sum::<f64>()
        / starts.len() as f64;
    // Sung stretches laid over instrumental ones.
    let n = v.len() as f64;
    let (mut quiet, mut total) = (0.0, 0.0);
    let mut covered = vec![false; v.len()];
    for s in spans {
        let (a, b) = ((s.start + shift).max(0.0).min(n) as usize, (s.end + shift).max(0.0).min(n) as usize);
        if b > a {
            total += (b - a) as f64;
            quiet += v.slow[a..b].iter().filter(|x| **x < QUIET).count() as f64;
            covered[a..b].fill(true);
        }
    }
    let instrumental = if total > 0.0 { quiet / total } else { 1.0 };
    // Singing with no line, over the lyrics' own stretch of the song, in runs long enough to matter.
    let (first, last) = (spans.first().map_or(0.0, |s| s.start + shift), spans.last().map_or(0.0, |s| s.end + shift));
    let (a, b) = (first.max(0.0).min(n) as usize, last.max(0.0).min(n) as usize);
    let pad = v.fps as usize;
    let near: Vec<bool> = (0..v.len()).map(|k| covered[k.saturating_sub(pad)..(k + pad + 1).min(v.len())].iter().any(|c| *c)).collect();
    let (mut sung, mut bare, mut run) = (0.0, 0.0, 0usize);
    let long = (UNCOVERED_S * v.fps) as usize;
    for (slow, near) in v.slow[a..b.max(a)].iter().zip(&near[a..b.max(a)]) {
        let loud = *slow > SUNG;
        sung += f64::from(loud);
        if loud && !near {
            run += 1;
        } else {
            if run >= long {
                bare += run as f64;
            }
            run = 0;
        }
    }
    if run >= long {
        bare += run as f64;
    }
    let uncovered = if sung > 0.0 { bare / sung } else { 0.0 };
    (hits * (1.0 - instrumental) * (1.0 - 0.5 * uncovered)).clamp(0.0, 1.0)
}

/// A part of the lyrics slid on its own: its best shift, how sure it is of it, and its fit there and at the
/// whole's shift, each times its lines.
type Part = (f64, f64, f64, f64);

/// How much later the lyrics after some line run than those before it: the line cut at is the one under which
/// the two parts, each slid on its own, fit the voice best together (each part at least [`MIN_LINES`] lines).
/// Only when both parts are sure of their own offsets and fit clearly better apart than together at the
/// whole's offset `whole` (frames); 0 otherwise.
fn drift(v: &Voice, spans: &[Span], starts: &[(f64, bool)], whole: f64) -> Option<Drift> {
    let n = starts.len();
    if n < 2 * MIN_LINES {
        return None;
    }
    let reach = (PART_REACH_MS as f64 / 1000.0 * v.fps).round() as i64;
    // Every line a cut for up to 40 lines, then 40 cuts spread over them.
    let step = ((n - 2 * MIN_LINES) / 40).max(1);
    let part = |cut: f64, first: bool| -> Part {
        let keep = |f: f64| (f < cut) == first;
        let sp: Vec<Span> = spans.iter().copied().filter(|s| keep(s.start)).collect();
        let st: Vec<(f64, bool)> = starts.iter().copied().filter(|s| keep(s.0)).collect();
        let (shift, best, sure) = best_shift(v, &sp, &st, reach);
        let k = st.len() as f64;
        (shift, sure, best * k, fit(v, &sp, &st, whole) * k)
    };
    let mut best: Option<(f64, Part, Part)> = None;
    for c in (MIN_LINES..=n - MIN_LINES).step_by(step) {
        let cut = starts[c].0 - 1e-6;
        let (a, b) = (part(cut, true), part(cut, false));
        if best.is_none_or(|x| a.2 + b.2 > x.0) {
            best = Some((a.2 + b.2, a, b));
        }
    }
    let (apart, a, b) = best?;
    Some(Drift { ms: -((b.0 - a.0) / v.fps * 1000.0).round() as i64, sure: a.1.min(b.1), gain: (apart - a.3 - b.3) / n as f64 })
}

/// The best cut's parts: the second's offset less the first's, the less sure part's confidence, and how
/// much better per line the parts fit apart than together.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drift {
    ms: i64,
    sure: f64,
    gain: f64,
}

/// Checks timed lyrics against a song's vocal curve; none for untimed lyrics or a curve too short or
/// too flat to say anything.
pub fn check(l: &Lyrics, curve: &VocalCurve) -> Option<SyncCheck> {
    if !l.synced {
        return None;
    }
    let v = Voice::new(curve)?;
    let spans = spans(l, &v);
    let starts = starts(l, &spans, &v);
    if starts.len() < MIN_LINES {
        return Some(SyncCheck { kind: SyncKind::Unsure, score: 0.0, best_score: 0.0, offset_ms: 0, confidence: 0.0, drift_ms: 0 });
    }
    let reach = (REACH_MS as f64 / 1000.0 * v.fps).round() as i64;
    let (shift, _, confidence) = best_shift(&v, &spans, &starts, reach);
    // Lines in place sit later than the fit by the curve's lag, and line-timed ones also lead the voice.
    let bias_ms = LAG_MS + if l.word_timed { 0 } else { LINE_LEAD_MS };
    let offset_ms = bias_ms - (shift / v.fps * 1000.0).round() as i64;
    let given = score_at(&v, &spans, &starts, bias_ms as f64 / 1000.0 * v.fps);
    let best_score = score_at(&v, &spans, &starts, shift).max(given);
    // What any timing scores here: the lines slid 7 to 23 s off.
    let far = [-23.0, -19.0, -17.0, -13.0, -11.0, -7.0, 7.0, 11.0, 13.0, 17.0, 19.0, 23.0];
    let null = far.iter().map(|s| score_at(&v, &spans, &starts, shift + s * v.fps)).sum::<f64>() / far.len() as f64;
    let drift_ms = drift(&v, &spans, &starts, shift).filter(|d| d.sure >= PART_SURE && d.gain >= PART_GAIN).map_or(0, |d| d.ms);
    let kind = if best_score < POOR || best_score - null < LIFT {
        SyncKind::Poor
    } else if drift_ms.abs() >= DRIFT_MS {
        SyncKind::Drifts
    } else if offset_ms.abs() >= OFFSET_MIN_MS && confidence >= OFFSET_SURE && best_score > given {
        SyncKind::Shifted
    } else {
        SyncKind::Fits
    };
    let score = if kind == SyncKind::Shifted { best_score } else { given };
    Some(SyncCheck { kind, score, best_score, offset_ms, confidence, drift_ms })
}

#[cfg(test)]
pub(crate) mod tests {
    //! Synthetic songs with a sung line at known times (nori-player's AutoMix evaluation songs), and made-up
    //! lyric timings over them: exact, shifted, drifted, another song's, at random. The words are invented
    //! placeholders.

    use super::*;
    use nori_model::{LyricLine, LyricWord};
    use nori_player::automix::analysis::Analyzer;
    use nori_player::automix::eval::{Rng, Song, Style, FULL, SUNG};

    /// Invented words, one per sung note: about as many letters a second as a sung line has.
    const SYLLABLES: [&str; 12] = ["lomira", "teshvan", "korupel", "sumidah", "netori", "falquen", "birosa", "mekanu", "dovelin", "saruto", "pemial", "quorest"];

    /// A song's curve and its sung phrases: each a run of notes with no gap of a breath, as (start, end) of
    /// every note, seconds.
    pub(crate) fn sung(song: &Song) -> (VocalCurve, Vec<Vec<(f64, f64)>>, f64) {
        let (x, truth) = song.render();
        let mut a = Analyzer::new(song.rate, 0);
        a.feed(&x);
        let curve = a.take_features().voice_curve();
        let mut phrases: Vec<Vec<(f64, f64)>> = Vec::new();
        for n in truth.voice {
            match phrases.last_mut() {
                Some(p) if n.0 - p.last().unwrap().1 < 0.15 => p.push(n),
                _ => phrases.push(vec![n]),
            }
        }
        (curve, phrases, x.len() as f64 / song.rate as f64)
    }

    /// A word is timed from its first consonant, this long before the synthetic voice's pitched note (the
    /// synthetic voice has no consonants; a real one does, and real word timings start with it).
    const CONSONANT_S: f64 = LAG_MS as f64 / 1000.0;

    /// Lines over `phrases` (a line per phrase, or two when `split`: the second starting mid-phrase with no
    /// breath), every time mapped through `at` (seconds to seconds), timed by word or by line, as real
    /// services time them: words from their consonant ([`CONSONANT_S`]), and a line-timed line a little
    /// before its first word ([`LINE_LEAD_MS`]). A line-timed line ends where the next starts, as LRC has it.
    pub(crate) fn lyrics(phrases: &[Vec<(f64, f64)>], split: bool, word_timed: bool, at: &dyn Fn(f64) -> f64) -> Lyrics {
        let mut groups: Vec<&[(f64, f64)]> = Vec::new();
        for p in phrases {
            if split && p.len() >= 4 {
                let (a, b) = p.split_at(p.len() / 2);
                groups.extend([a, b]);
            } else {
                groups.push(p);
            }
        }
        let mut lines: Vec<LyricLine> = Vec::new();
        for (g, notes) in groups.iter().enumerate() {
            let mut text = String::new();
            let mut words = Vec::new();
            for (k, (a, b)) in notes.iter().enumerate() {
                if !text.is_empty() {
                    text.push(' ');
                }
                let start = text.encode_utf16().count() as u32;
                text.push_str(SYLLABLES[(g * 5 + k) % SYLLABLES.len()]);
                let end = text.encode_utf16().count() as u32;
                words.push(LyricWord { start_ms: ((at(*a) - CONSONANT_S) * 1000.0).round() as i64, end_ms: (at(*b) * 1000.0).round() as i64, start, end });
            }
            let start_ms = words[0].start_ms - if word_timed { 0 } else { LINE_LEAD_MS };
            lines.push(LyricLine { start_ms, end_ms: words.last().unwrap().end_ms, text, words: if word_timed { words } else { Vec::new() }, ..Default::default() });
        }
        if !word_timed {
            for i in 0..lines.len().saturating_sub(1) {
                lines[i].end_ms = lines[i + 1].start_ms;
            }
        }
        Lyrics { synced: true, word_timed, lines, key: 0, ..Default::default() }
    }

    /// Lines at random times over `secs`, as many as `n`: lyrics that are nobody's timing of this song.
    fn random(n: usize, secs: f64, seed: u64) -> Lyrics {
        let mut rng = Rng(seed);
        let mut t: Vec<f64> = (0..n).map(|_| 5.0 + rng.unit() * (secs - 10.0)).collect();
        t.sort_by(f64::total_cmp);
        let phrases: Vec<Vec<(f64, f64)>> = t.iter().map(|a| vec![(*a, a + 1.0), (a + 1.0, a + 2.0), (a + 2.0, a + 3.0)]).collect();
        lyrics(&phrases, false, false, &|x| x)
    }

    fn song() -> Song {
        Song { sections: vec![(4, FULL), (12, SUNG), (6, FULL), (12, SUNG), (4, FULL)], ..Song::new("sync", Style::Backbeat, 112.0, 2, false) }
    }

    #[test]
    fn fits() {
        let (curve, phrases, _) = sung(&song());
        let exact = check(&lyrics(&phrases, false, false, &|t| t), &curve).unwrap();
        assert_eq!(exact.kind, SyncKind::Fits, "{exact:?}");
        assert!(exact.score > 0.7 && exact.offset_ms.abs() < 120 && exact.applied_ms() == 0, "{exact:?}");
        for late in [-2.0, -1.0, 1.0, 2.0] {
            let c = check(&lyrics(&phrases, false, false, &|t| t + late), &curve).unwrap();
            assert_eq!(c.kind, SyncKind::Shifted, "{late}: {c:?}");
            assert!((c.applied_ms() as f64 - late * 1000.0).abs() < 120.0, "{late} s late read as {c:?}");
            assert!(c.score > exact.score - 0.1, "put right, they fit as well as the exact ones: {c:?}");
        }
        let words = check(&lyrics(&phrases, true, true, &|t| t + 0.5), &curve).unwrap();
        assert!((words.offset_ms - 500).abs() < 120 && words.kind == SyncKind::Shifted, "word-timed: {words:?}");

        // Drift and poor fit.
        let (curve, phrases, secs) = sung(&song());
        let mid = phrases[phrases.len() / 2][0].0;
        let edit = check(&lyrics(&phrases, true, false, &|t| if t < mid - 0.1 { t } else { t + 2.0 }), &curve).unwrap();
        assert_eq!(edit.kind, SyncKind::Drifts, "{edit:?}");
        assert!((edit.drift_ms - 2000).abs() < 250 && edit.applied_ms() == 0, "{edit:?}");
        let wrong = check(&random(2 * phrases.len(), secs, 7), &curve).unwrap();
        // Poor or drifting, demoted the same either way (trust.rs): at random, some part of it fits somewhere.
        assert!(matches!(wrong.kind, SyncKind::Poor | SyncKind::Drifts), "{wrong:?}");
        let mut plain = lyrics(&phrases, false, false, &|t| t);
        plain.synced = false;
        assert_eq!(check(&plain, &curve), None, "untimed words are not checked");
        let few = Lyrics { lines: plain.lines[..3].to_vec(), synced: true, ..plain.clone() };
        assert_eq!(check(&few, &curve).map(|c| c.kind), Some(SyncKind::Unsure), "three lines say too little");
        let flat = VocalCurve { fps: curve.fps, t0: curve.t0, level: vec![40; curve.level.len()] };
        assert_eq!(check(&lyrics(&phrases, false, false, &|t| t), &flat), None, "a curve with no voice in it says nothing");
    }

}
