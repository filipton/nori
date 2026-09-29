//! Transition planning: a pure function from two analyses and the settings to a `TransitionPlan`.
//! The fallback ladder, most to least informed:
//!
//! 1. keys far apart: echo-out on the outgoing grid (two singers are first kept apart by filters in a
//!    beat-matched mix, and echo out only when that does not fit or filters are off);
//! 2. both grids confident, tempos within reach: beat-matched, the incoming drop (or intro end, or a phrase line)
//!    landing on an outgoing downbeat where the bass swaps. Camelot distance sets length and filter;
//! 3. an analysis but no usable grid: MixRamp overlap with a filter sweep;
//! 4. nothing known: a fixed equal-power crossfade;
//! 5. same album in order: gapless.
//!
//! Loudness or timbre gaps only shorten; gates never upgrade a transition.
//!
//! Hard rule: at most `MAX_SKIP_MS` of either track's music goes unplayed (silence is free). The outgoing song
//! may be left early at a closing breakdown or before a hidden track (`Ending`) within that cap.

use super::structure::key_distance;
use super::tempo::{fold, match_ratio};
use crate::types::{AutoMixSettings, BassSwap, Echo, FadeCurve, Sweep, TrackAnalysis, TransitionKind, TransitionPlan, VocalDuck};

pub const MAX_SKIP_MS: i64 = 15_000;
pub const MIN_BPM_CONFIDENCE: f32 = 0.5;
pub const MIN_STABILITY: f32 = 0.6;
/// Varispeed moves pitch 0.34 semitone per 2 %; beyond that detuning is audible.
pub const VARISPEED_MAX_PCT: f64 = 2.0;
/// Longest fixed crossfade.
const MAX_BLIND_FADE_MS: i64 = 12_000;
/// Shortest overlap that is still a fade rather than a click guard.
const MIN_FADE_MS: i64 = 300;
/// Shortest MixRamp overlap: shorter overlaps on a fading ending are heard as no mix at all.
const MIN_MIXRAMP_MS: i64 = 5_000;
const BASS_CUT_HZ: f32 = 180.0;
const SWEEP_FROM_HZ: f32 = 18_000.0;
/// Where the outgoing low-pass ends: beat-matched and plain fades.
const SWEEP_TO_HZ_MATCHED: f32 = 400.0;
const SWEEP_TO_HZ_FADE: f32 = 500.0;
/// Camelot neighbour or relative: gentle muffling.
const SWEEP_TO_HZ_SOFT: f32 = 2_500.0;

fn low_pass(start_ms: i64, end_ms: i64, to_hz: f32) -> Sweep {
    Sweep { start_ms, end_ms, from_hz: SWEEP_FROM_HZ, to_hz }
}

fn blank(kind: TransitionKind, out_start: i64, in_start: i64, duration: i64, reason: String) -> TransitionPlan {
    TransitionPlan {
        kind,
        out_start_ms: out_start,
        in_start_ms: in_start,
        duration_ms: duration,
        tempo_ratio: 1.0,
        tempo_ramp_beats: 0,
        tempo_ramp_ms: 0,
        keep_pitch: true,
        fade_curve: FadeCurve::EqualPower,
        out_fade_start_ms: 0,
        out_fade_end_ms: duration,
        in_fade_start_ms: 0,
        in_fade_end_ms: duration,
        out_gain_db: 0.0,
        in_gain_db: 0.0,
        bass_swap: None,
        low_pass: None,
        high_pass: None,
        echo: None,
        out_loop_ms: None,
        vocal_duck: None,
        reason,
    }
}

/// An analysis only counts when it is sane and describes this file (same length within 3 s).
fn usable(a: Option<&TrackAnalysis>, duration_ms: i64) -> Option<&TrackAnalysis> {
    a.filter(|a| sound(a) && (a.duration_ms <= 0 || duration_ms <= 0 || (a.duration_ms - duration_ms).abs() <= 3000))
}

/// The fastest tempo a stored grid may have and still be taken for one.
const MAX_BPM: f64 = 1_000.0;
/// Positions beyond this (ms, a hundred hours) are corrupt.
const MAX_MS: f64 = 3.6e8;

/// Whether every number a plan uses is finite and in range. A corrupt row is ignored rather than planned into
/// nonsense or a panic on the engine thread.
fn sound(a: &TrackAnalysis) -> bool {
    let tempo = |b: f64| b.is_finite() && (0.0..=MAX_BPM).contains(&b);
    let place = |ms: f64| ms.is_finite() && ms.abs() <= MAX_MS;
    let whole = |ms: i64| (ms as f64).abs() <= MAX_MS;
    let share = |v: f32| v.is_finite();
    [a.bpm, a.intro_bpm, a.outro_bpm].into_iter().all(tempo)
        && [a.beat_offset_ms, a.intro_beat_offset_ms, a.outro_beat_offset_ms].into_iter().all(place)
        && [a.duration_ms, a.silence_start_ms, a.silence_end_ms, a.mixramp_start_ms, a.mixramp_end_ms, a.intro_end_ms, a.outro_start_ms, a.drop_ms, a.exit_ms, a.gap_ms, a.gap_end_ms]
            .into_iter()
            .all(whole)
        && [
            a.bpm_confidence,
            a.stability,
            a.downbeat_confidence,
            a.lufs,
            a.key_confidence,
            a.outro_vocal,
            a.intro_vocal,
            a.outro_centroid,
            a.intro_centroid,
            a.outro_bpm_confidence,
            a.outro_stability,
            a.intro_bpm_confidence,
            a.intro_stability,
            a.drop_runup_vocal,
            a.drop_vocal,
            a.exit_vocal,
            a.drop_runup_tonal_db,
        ]
        .into_iter()
        .all(share)
}

/// Beats in a bar: 3 for a waltz, else 4 (also for rows without a measured metre).
pub(super) fn bar_beats(a: &TrackAnalysis) -> i64 {
    if a.beats_per_bar == 3 {
        3
    } else {
        4
    }
}

pub(super) fn grid_ok(a: &TrackAnalysis) -> bool {
    a.bpm > 0.0 && a.bpm.is_finite() && a.bpm_confidence >= MIN_BPM_CONFIDENCE && a.stability >= MIN_STABILITY
}

/// `a` with its grid replaced by the one measured over one end: live bands drift and songs change tempo, so
/// the whole-song grid is wrong at the ends. The end's grid wins unless only the whole-song grid is usable.
/// `meter` is the end's own metre (Beat This!), 0 for none.
fn with_grid(a: &TrackAnalysis, bpm: f64, confidence: f32, offset_ms: f64, stability: f32, phase: i32, meter: i32) -> TrackAnalysis {
    let beats_per_bar = if meter > 0 { meter } else { a.beats_per_bar };
    let w = TrackAnalysis { bpm, bpm_confidence: confidence, beat_offset_ms: offset_ms, stability, downbeat_phase: phase, beats_per_bar, ..a.clone() };
    let measured = bpm > 0.0 && bpm.is_finite();
    if measured && (grid_ok(&w) || !grid_ok(a)) { w } else { a.clone() }
}

/// The outgoing song, gridded over its last seconds.
pub(super) fn at_end(a: &TrackAnalysis) -> TrackAnalysis {
    with_grid(a, a.outro_bpm, a.outro_bpm_confidence, a.outro_beat_offset_ms, a.outro_stability, a.outro_downbeat_phase, a.outro_beats_per_bar)
}

/// The incoming song, gridded over its first seconds.
pub(super) fn at_start(b: &TrackAnalysis) -> TrackAnalysis {
    with_grid(b, b.intro_bpm, b.intro_bpm_confidence, b.intro_beat_offset_ms, b.intro_stability, b.intro_downbeat_phase, b.intro_beats_per_bar)
}

/// The analysis BPM, halved or doubled when that lands within 4 % of the tag BPM.
fn bpm_with_tag(analysis: f64, tag: f32) -> f64 {
    if !(analysis > 0.0 && analysis.is_finite()) {
        return analysis;
    }
    let tag = tag as f64;
    if !(tag > 0.0 && tag.is_finite()) {
        return analysis;
    }
    let mut best = analysis;
    let mut best_err = (analysis - tag).abs() / tag;
    for c in [analysis, analysis * 2.0, analysis / 2.0] {
        let err = (c - tag).abs() / tag;
        if err < best_err {
            best = c;
            best_err = err;
        }
    }
    if best_err <= 0.04 {
        best
    } else {
        analysis
    }
}

fn music_end(a: &TrackAnalysis, duration: i64) -> i64 {
    if a.silence_end_ms > a.silence_start_ms && a.silence_end_ms <= duration {
        a.silence_end_ms
    } else {
        duration
    }
}

/// The end of the outgoing song: where its music ends, a long silence inside it, and where a mix may leave.
/// Skipped silence costs nothing against `MAX_SKIP_MS`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Ending {
    /// End of the last audible music, ms.
    pub music_end: f64,
    /// The long silence inside the music, when there is one.
    pub gap: Option<(f64, f64)>,
    /// Where to leave: a closing breakdown or the gap before a hidden track; else `music_end`.
    pub exit: f64,
    /// `exit` starts a closing breakdown (music, not a gap).
    pub breakdown: bool,
}

impl Ending {
    pub fn of(a: &TrackAnalysis, duration: i64) -> Ending {
        let music_end = music_end(a, duration) as f64;
        let gap = (a.gap_ms > a.silence_start_ms && a.gap_end_ms > a.gap_ms && a.gap_end_ms as f64 <= music_end)
            .then_some((a.gap_ms as f64, a.gap_end_ms as f64));
        let mut e = Ending { music_end, gap, exit: music_end, breakdown: false };
        let x = a.exit_ms as f64;
        if a.exit_ms > a.silence_start_ms && x < music_end {
            e.exit = x;
            e.breakdown = gap.is_none_or(|(g0, _)| (g0 - x).abs() > 1.0);
        }
        e
    }

    /// Music (not silence) after `heard_end`, ms.
    pub fn skipped(&self, heard_end: f64) -> f64 {
        let total = (self.music_end - heard_end).max(0.0);
        let silent = self.gap.map_or(0.0, |(g0, g1)| (g1 - g0.max(heard_end)).max(0.0));
        (total - silent).max(0.0)
    }

    /// The exit when the music after it fits the cap plus `slack` (a mix tail heard after the swap), else the
    /// end of the music.
    pub fn leave(&self, slack: f64) -> f64 {
        if self.skipped(self.exit) <= MAX_SKIP_MS as f64 + slack {
            self.exit
        } else {
            self.music_end
        }
    }

    /// Silence (the gap, or after the music) inside `[t0, t1)`, ms.
    pub fn silence(&self, t0: f64, t1: f64) -> f64 {
        let over = |a: f64, b: f64| (t1.min(b) - t0.max(a)).max(0.0);
        self.gap.map_or(0.0, |(g0, g1)| over(g0, g1)) + over(self.music_end, f64::MAX)
    }

    /// The closing breakdown inside `[t0, t1)`, ms.
    pub fn coda(&self, t0: f64, t1: f64) -> f64 {
        if !self.breakdown {
            return 0.0;
        }
        let end = self.gap.map_or(self.music_end, |(g0, _)| if g0 > self.exit { g0 } else { self.music_end });
        (t1.min(end) - t0.max(self.exit)).max(0.0)
    }
}

fn loudness_trim(a: Option<&TrackAnalysis>, b: Option<&TrackAnalysis>, s: &AutoMixSettings) -> f32 {
    match (a, b) {
        (Some(a), Some(b)) if s.match_loudness && a.lufs > -60.0 && b.lufs > -60.0 => (a.lufs - b.lufs).clamp(-9.0, 9.0),
        _ => 0.0,
    }
}

/// How the two tracks sound together, from the stored overlap windows.
#[derive(Clone, Copy)]
struct Verdict {
    /// Two vocals, or confident keys far apart: do not blend.
    clash: bool,
    cause: &'static str,
    /// A large loudness or brightness gap: keep the overlap short.
    shorten: bool,
    short_cause: &'static str,
}

/// Key confidence a key is trusted at.
const KEY_MIN_CONFIDENCE: f32 = 0.4;
/// Confident keys this far apart on the Camelot wheel do not blend.
const KEY_FAR: i32 = 4;
/// Mean voice-band share that counts as "sung" in an overlap window.
pub(super) const VOCAL_MIN: f32 = 0.45;
/// Loudness gap that shortens an overlap, dB.
const LOUD_GAP_DB: f32 = 6.0;
/// Brightness ratio (octaves of centroid) that counts as a timbre mismatch.
const TIMBRE_OCTAVES: f64 = 1.0;

const VOCALS_OVERLAP: &str = "vocals overlap";
/// Incoming voice-band cut while the outgoing song sings: 18 dB at 1 kHz, about 6 dB at 500 Hz and 2 kHz.
const VOCAL_DUCK_DB: f32 = -18.0;
const VOCAL_DUCK_HZ: f32 = 1_000.0;
/// The high-pass that thins the outgoing voice after the swap, over the first half of the rest of the mix.
const VOCAL_HP_FROM_HZ: f32 = 200.0;
pub(super) const VOCAL_HP_TO_HZ: f32 = 2_000.0;

/// Two voices over a plain fade: duck the incoming voice band through the first half.
fn separate_in_fade(p: &mut TransitionPlan, s: &AutoMixSettings, sung_both: bool) {
    if !sung_both || !s.filter_effects || p.duration_ms < 2 * MIN_FADE_MS {
        return;
    }
    p.vocal_duck = Some(VocalDuck { until_ms: p.duration_ms / 2, release_ms: (p.duration_ms / 4).min(500), db: VOCAL_DUCK_DB, hz: VOCAL_DUCK_HZ });
    p.reason += ", voices kept apart";
}

fn pair_gate(a: &TrackAnalysis, b: &TrackAnalysis) -> Verdict {
    let mut v = Verdict { clash: false, cause: "", shorten: false, short_cause: "" };
    if camelot_dist(a, b).is_some_and(|d| d >= KEY_FAR) {
        v.clash = true;
        v.cause = "keys far apart";
    } else if a.outro_vocal >= VOCAL_MIN && b.intro_vocal >= VOCAL_MIN {
        v.clash = true;
        v.cause = VOCALS_OVERLAP;
    }
    if a.lufs > -60.0 && b.lufs > -60.0 && (a.lufs - b.lufs).abs() > LOUD_GAP_DB {
        v.shorten = true;
        v.short_cause = "loudness gap";
    } else if a.outro_centroid > 0.0
        && b.intro_centroid > 0.0
        && (a.outro_centroid as f64 / b.intro_centroid as f64).log2().abs() > TIMBRE_OCTAVES
    {
        v.shorten = true;
        v.short_cause = "timbre mismatch";
    }
    v
}

/// Echo-out for a clashing pair: four outgoing beats from a downbeat, the outgoing track into a beat-synced echo
/// while the incoming one fades in. No tempo change, no bass swap.
fn echo_out(a: &TrackAnalysis, b: &TrackAnalysis, out_dur: i64, in_dur: i64, max_len: i64, s: &AutoMixSettings, cause: &str) -> Option<TransitionPlan> {
    let beat = 60_000.0 / a.bpm;
    if !beat.is_finite() || beat <= 0.0 {
        return None;
    }
    let delay = beat.clamp(250.0, 1000.0).round() as i64;
    let beats = 4i64;
    let dur = (beats as f64 * beat).round() as i64;
    if dur > max_len || dur < MIN_FADE_MS {
        return None;
    }
    let ending = Ending::of(a, out_dur);
    let end_a = ending.leave(0.0);
    let bpb = bar_beats(a);
    let phase = (a.downbeat_phase as i64).rem_euclid(bpb);
    let mut n = ((end_a as f64 - dur as f64 - a.beat_offset_ms) / beat).floor() as i64;
    while n.rem_euclid(bpb) != phase {
        n -= 1;
    }
    let start = (a.beat_offset_ms + n as f64 * beat).round() as i64;
    if start < a.silence_start_ms || ending.skipped((start + dur) as f64) > MAX_SKIP_MS as f64 {
        return None;
    }
    let in_start = b.silence_start_ms.clamp(0, in_dur / 3);
    if dur > in_dur - in_start {
        return None;
    }
    let beat_ms = beat.round() as i64;
    let mut p = blank(TransitionKind::EchoOut, start, in_start, dur, String::new());
    p.fade_curve = FadeCurve::SineSquared;
    // Outgoing out in two beats; incoming in over the last three while the repeats decay.
    (p.out_fade_start_ms, p.out_fade_end_ms) = (0, (2 * beat_ms).min(dur));
    (p.in_fade_start_ms, p.in_fade_end_ms) = ((dur - 3 * beat_ms).max(0), dur);
    p.echo = Some(Echo { delay_ms: delay, feedback: 0.45, wet_db: -7.0 });
    p.in_gain_db = loudness_trim(Some(a), Some(b), s);
    p.reason = format!("echo-out over {beats} beats, {cause}");
    Some(p)
}

/// Camelot distance when both keys are trusted.
fn camelot_dist(a: &TrackAnalysis, b: &TrackAnalysis) -> Option<i32> {
    (a.key_confidence >= KEY_MIN_CONFIDENCE && b.key_confidence >= KEY_MIN_CONFIDENCE).then(|| key_distance(a.key, b.key))
}

/// Low-pass for MixRamp and one-grid fades: soft when keys are neighbours, full otherwise.
fn apply_fade_filter(p: &mut TransitionPlan, a: Option<&TrackAnalysis>, b: Option<&TrackAnalysis>, s: &AutoMixSettings, dur: i64) {
    if !s.filter_effects || dur < 2000 {
        return;
    }
    let soft = a.zip(b).is_some_and(|(a, b)| matches!(camelot_dist(a, b), Some(0 | 1)));
    p.low_pass = Some(if soft { low_pass(dur / 2, dur, SWEEP_TO_HZ_SOFT) } else { low_pass(0, dur, SWEEP_TO_HZ_FADE) });
}

pub fn plan(out: Option<&TrackAnalysis>, inc: Option<&TrackAnalysis>, out_duration_ms: i64, in_duration_ms: i64, s: &AutoMixSettings) -> TransitionPlan {
    let out_dur = if out_duration_ms > 0 { out_duration_ms } else { out.map_or(0, |a| a.duration_ms) };
    let in_dur = if in_duration_ms > 0 { in_duration_ms } else { inc.map_or(0, |a| a.duration_ms) };
    if s.same_album_in_order {
        return blank(TransitionKind::Gapless, out_dur.max(0), 0, 0, "same album in order: gapless".into());
    }
    let max_len = if s.max_transition_s.is_finite() { (s.max_transition_s.clamp(0.0, 60.0) * 1000.0) as i64 } else { 0 };
    if out_dur <= 0 || in_dur <= 0 || max_len < MIN_FADE_MS {
        return blank(TransitionKind::Gapless, out_dur.max(0), 0, 0, "no room for a transition: gapless".into());
    }
    // Never more than a third of either track.
    let max_len = max_len.min(out_dur / 3).min(in_dur / 3);
    if max_len < MIN_FADE_MS {
        return blank(TransitionKind::Gapless, out_dur, 0, 0, "tracks too short: gapless".into());
    }
    let (a, b) = (usable(out, out_dur).map(at_end), usable(inc, in_dur).map(at_start));
    let (a, b) = (a.as_ref(), b.as_ref());
    let mut why_not = String::new();
    let verdict = a.zip(b).map(|(a, b)| pair_gate(a, b));
    let short_cause = verdict.map(|v| if v.clash { v.cause } else { v.short_cause }).unwrap_or("");
    let short = !short_cause.is_empty();
    if let (Some(a), Some(b)) = (a, b) {
        if !s.beat_match {
            why_not = "beat matching off".into();
        } else if !grid_ok(a) || !grid_ok(b) {
            why_not = format!(
                "no reliable beat grid (out {:.0} BPM conf {:.2} stab {:.2}, in {:.0} BPM conf {:.2} stab {:.2}; needs conf ≥ {:.1}, stab ≥ {:.1})",
                a.bpm, a.bpm_confidence, a.stability, b.bpm, b.bpm_confidence, b.stability,
                MIN_BPM_CONFIDENCE, MIN_STABILITY
            );
        } else if let Some(v) = verdict {
            // Singers can be kept apart by filters, so try a beat-matched mix before an echo-out; far keys cannot.
            let separable = v.cause == VOCALS_OVERLAP && s.filter_effects;
            if v.clash && s.echo_out && !separable {
                if let Some(p) = echo_out(a, b, out_dur, in_dur, max_len, s, v.cause) {
                    return p;
                }
            }
            match beat_matched(a, b, out_dur, in_dur, max_len, s, short_cause) {
                Ok(p) => return p,
                Err(e) => why_not = e,
            }
            if v.clash && s.echo_out && separable {
                if let Some(p) = echo_out(a, b, out_dur, in_dur, max_len, s, v.cause) {
                    return p;
                }
            }
        } else {
            match beat_matched(a, b, out_dur, in_dur, max_len, s, "") {
                Ok(p) => return p,
                Err(e) => why_not = e,
            }
        }
    }
    let sung_both = verdict.is_some_and(|v| v.cause == VOCALS_OVERLAP);
    if s.beat_match && (a.is_some_and(grid_ok) || b.is_some_and(grid_ok)) {
        if let Some(mut p) = one_grid(a, b, out_dur, in_dur, max_len, s, short, &why_not) {
            separate_in_fade(&mut p, s, sung_both);
            return p;
        }
    }
    if a.is_some() || b.is_some() {
        let mut p = mixramp(a, b, out_dur, in_dur, max_len, s, &why_not);
        separate_in_fade(&mut p, s, sung_both);
        return p;
    }
    let dur = max_len.min(MAX_BLIND_FADE_MS);
    blank(TransitionKind::EqualPowerFade, out_dur - dur, 0, dur, "not analysed: equal-power fade".into())
}

/// Two grids locked together.
struct Lock {
    ratio: f64,
    pct: f64,
    bpm_a: f64,
    /// The incoming tempo folded to the outgoing octave.
    b_bpm: f64,
    bpb: i64,
    /// Outgoing beat and bar, ms (the incoming ones too once stretched).
    beat: f64,
    bar: f64,
    dist: Option<i32>,
    mild_clash: bool,
    max_bars: i64,
    /// Filters are on, so two voices can be kept apart.
    separate: bool,
}

/// Where in the incoming song the swap lands, best first.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Landing {
    /// `drop_ms`.
    Drop,
    /// The end of the intro, when it is not the drop.
    IntroEnd,
    /// A four-bar line from the first downbeat.
    Phrase,
    /// The first downbeat: no run-up.
    Start,
}

impl Landing {
    fn worth(self) -> f64 {
        match self {
            Landing::Drop => 4.0,
            Landing::IntroEnd => 3.0,
            Landing::Phrase => 1.0,
            Landing::Start => 0.0,
        }
    }

    fn words(self) -> &'static str {
        match self {
            Landing::Drop => "in on the drop",
            Landing::IntroEnd => "in at the end of the intro",
            Landing::Phrase => "on a phrase line",
            Landing::Start => "straight in",
        }
    }
}

/// A candidate beat-matched window, ms: the outgoing song from `start` for `dur`, the incoming from `in_start`,
/// the bass swapping `swap` into it.
#[derive(Clone, Copy, Debug)]
struct Window {
    start: f64,
    dur: f64,
    swap: f64,
    in_start: f64,
    runup_bars: i64,
    landing: Landing,
    on_phrase: bool,
    /// Length of the looped outgoing slice, ms; 0 for no loop.
    out_loop: f64,
    /// Voice-band share of the incoming song over the run-up and after the swap.
    runup_vocal: f32,
    after_vocal: f32,
    score: f64,
}

/// Score weights. Per second skipped of instrumental and of sung music, and per second of incoming intro heard
/// alone after the mix.
const SKIP_COST: f64 = 0.06;
const SKIP_SUNG_COST: f64 = 0.15;
const DIP_COST: f64 = 0.1;
/// Per bar of run-up (up to eight), and per bar of mix.
const RUNUP_WORTH: f64 = 0.1;
const LENGTH_WORTH: f64 = 0.02;
/// Swap on a four-bar line of the outgoing sections.
const PHRASE_WORTH: f64 = 0.5;
/// Per second of sung run-up under a sung ending.
const SUNG_RUNUP_COST: f64 = 0.1;
/// Skipping the incoming drop.
const DROP_SKIP_COST: f64 = 2.0;
/// Per bar of looped outgoing slice heard again.
const LOOP_COST: f64 = 0.1;
/// Per second of outgoing silence under the run-up or before the mix.
const DEAD_AIR_COST: f64 = 0.2;
/// Swap on the outgoing exit.
const EXIT_WORTH: f64 = 0.5;
/// Run-up of whole four-bar phrases.
const PHRASE_START_WORTH: f64 = 0.3;
/// A run-up this far below the song's chord energy, dB, has no chords to clash.
const CHORDLESS_DB: f32 = 3.0;

/// The best beat-matched window: every incoming landing (drop, intro end, phrase lines, first downbeat) against
/// every downbeat of the outgoing song's last sixteen bars (and those before its exit), with run-ups of 16 bars
/// down to none and tails of a beat to four bars. Windows over the skip cap or that do not fit are dropped; the
/// rest are scored.
fn drop_aligned(a: &TrackAnalysis, b: &TrackAnalysis, out_dur: i64, in_dur: i64, max_len: i64, bpm_b: f64, k: &Lock) -> Option<Window> {
    let (beat, bar, ratio) = (k.beat, k.bar, k.ratio);
    let ending = Ending::of(a, out_dur);
    let phase_a = (a.downbeat_phase as i64).rem_euclid(k.bpb);
    let downbeat_at = |t: f64| {
        let mut n = ((t + 0.5 * beat - a.beat_offset_ms) / beat).floor() as i64;
        while n.rem_euclid(k.bpb) != phase_a {
            n -= 1;
        }
        a.beat_offset_ms + n as f64 * beat
    };
    let exit = ending.leave(4.0 * bar);
    let mut anchors: Vec<(f64, usize)> = Vec::new();
    for end in [ending.music_end, exit] {
        let last = downbeat_at(end);
        for back in 0..=16usize {
            let at = last - back as f64 * bar;
            if anchors.iter().all(|(t, _)| (t - at).abs() > 0.5 * beat) {
                anchors.push((at, back));
            }
        }
    }
    let on_line = |t: f64, from: f64| {
        let x = (t - from) / (4.0 * bar);
        (x - x.round()).abs() * 4.0 < 0.05
    };
    // The incoming song on its native grid.
    let beat_n = 60_000.0 / bpm_b;
    let bar_in = k.bar * ratio;
    let phase_b = (b.downbeat_phase as i64).rem_euclid(k.bpb);
    let mut nb = (((b.silence_start_ms as f64 - b.beat_offset_ms) / beat_n) - 0.25).ceil() as i64;
    while nb.rem_euclid(k.bpb) != phase_b {
        nb += 1;
    }
    let first = b.beat_offset_ms + nb as f64 * beat_n;
    // An analysis cue snapped to the nearest incoming downbeat.
    let snap = |t: f64| -> f64 {
        let m = ((t - first) / (k.bpb as f64 * beat_n)).round();
        let d = first + m * k.bpb as f64 * beat_n;
        if (d - t).abs() <= 0.5 * beat_n { d } else { t }
    };
    let drop = (b.drop_ms > 0).then(|| snap(b.drop_ms as f64));
    let intro_end = (b.intro_end_ms as f64 > first + 0.5 * bar_in).then(|| snap(b.intro_end_ms as f64));
    let arrival = drop.or(intro_end);
    // Each landing with the vocal share of its run-up and of what follows (measured only for the drop).
    let after = b.intro_vocal.max(b.drop_vocal);
    let mut landings: Vec<(f64, Landing, f32, f32)> = Vec::new();
    if let Some(d) = drop {
        landings.push((d, Landing::Drop, b.drop_runup_vocal, b.drop_vocal));
    }
    if let Some(d) = intro_end.filter(|i| drop.is_none_or(|d| (d - i).abs() > 0.5 * bar_in)) {
        landings.push((d, Landing::IntroEnd, b.intro_vocal, after));
    }
    for m in 1..=4 {
        let d = first + (4 * m) as f64 * bar_in;
        if landings.iter().all(|(t, ..)| (t - d).abs() > 0.5 * bar_in) {
            landings.push((d, Landing::Phrase, b.intro_vocal, after));
        }
    }
    landings.push((first, Landing::Start, b.intro_vocal, after));
    let sung_end = a.outro_vocal.max(a.exit_vocal) >= VOCAL_MIN;
    let skip_cost_out = if sung_end { SKIP_SUNG_COST } else { SKIP_COST };
    let skip_cost_in = if b.intro_vocal >= VOCAL_MIN { SKIP_SUNG_COST } else { SKIP_COST };

    let mut best: Option<Window> = None;
    for &(d, landing, runup_vocal, after_vocal) in &landings {
        for &(at, back) in &anchors {
            let on_phrase = a.outro_start_ms > 0 && at >= a.outro_start_ms as f64 - 0.5 * beat && on_line(at, a.outro_start_ms as f64);
            for runup in [16i64, 12, 8, 6, 4, 2, 0] {
                if landing == Landing::Start && runup > 0 {
                    continue;
                }
                let in_start = d - runup as f64 * bar_in;
                if in_start < (first - 0.5 * beat_n).max(0.0) {
                    continue;
                }
                let skip_in = (in_start - b.silence_start_ms as f64).max(0.0);
                if skip_in > MAX_SKIP_MS as f64 {
                    continue;
                }
                // The outgoing song under the run-up: its own bars, or when too few and not sung, its last four
                // or eight bars looped (a looped sung line sounds like a broken record).
                let (start, out_loop) = if at - runup as f64 * bar >= a.silence_start_ms as f64 {
                    (at - runup as f64 * bar, 0.0)
                } else if back == 0 && !sung_end {
                    match [8i64, 4].into_iter().find(|&lb| lb < runup && at - lb as f64 * bar >= a.silence_start_ms as f64) {
                        Some(lb) => (at - lb as f64 * bar, lb as f64 * bar),
                        None => continue,
                    }
                } else {
                    continue;
                };
                // A run-up without chords cannot clash: the pair's length cap then applies only after the swap.
                let neutral = landing == Landing::Drop && b.drop_runup_tonal_db <= -CHORDLESS_DB && runup_vocal < VOCAL_MIN;
                for tail in [beat, bar, 2.0 * bar, 4.0 * bar] {
                    let dur = runup as f64 * bar + tail;
                    let bars = dur / bar;
                    let capped = if neutral { tail / bar } else { bars };
                    if bars < 2.0 - 1e-6 || capped > k.max_bars as f64 + 0.3 || bars > 16.3 || dur > max_len as f64 {
                        continue;
                    }
                    let heard_end = if out_loop > 0.0 { at } else { at + tail };
                    let skip_out = ending.skipped(heard_end);
                    if heard_end > out_dur as f64 || skip_out > MAX_SKIP_MS as f64 {
                        continue;
                    }
                    if (in_dur as f64 - in_start) < 2.0 * dur * ratio {
                        continue;
                    }
                    let mut score = landing.worth() - (skip_cost_out * skip_out + skip_cost_in * skip_in) / 1000.0;
                    if let Some(arr) = arrival {
                        let lands = (arr - in_start) / ratio;
                        if lands > dur {
                            score -= DIP_COST * (lands - dur) / 1000.0;
                        } else if lands < -0.5 * beat {
                            score -= DROP_SKIP_COST;
                        }
                    }
                    score += RUNUP_WORTH * runup.min(8) as f64 + LENGTH_WORTH * bars;
                    if runup > 0 && runup % 4 == 0 {
                        score += PHRASE_START_WORTH;
                    }
                    if on_phrase {
                        score += PHRASE_WORTH;
                    }
                    // A sung run-up under a sung ending; the vocal duck halves the cost.
                    if sung_end && runup_vocal >= VOCAL_MIN {
                        score -= if k.separate { 0.5 } else { 1.0 } * SUNG_RUNUP_COST * runup as f64 * bar / 1000.0;
                    }
                    if out_loop > 0.0 {
                        score -= LOOP_COST * (dur - out_loop) / bar;
                    }
                    // Outgoing silence and closing breakdown under the run-up or before the mix.
                    let dead = ending.silence(start, at) + ending.silence(0.0, start).min(ending.gap.map_or(0.0, |(g0, g1)| g1 - g0));
                    score -= DEAD_AIR_COST * dead / 1000.0 + DIP_COST * (ending.coda(start, at) + ending.coda(0.0, start)) / 1000.0;
                    if exit < ending.music_end && (at - exit).abs() <= 0.5 * beat {
                        score += EXIT_WORTH;
                    }
                    if best.is_none_or(|w| score > w.score + 1e-9) {
                        best = Some(Window {
                            start,
                            dur,
                            swap: runup as f64 * bar,
                            in_start,
                            runup_bars: runup,
                            landing,
                            on_phrase,
                            out_loop,
                            runup_vocal,
                            after_vocal,
                            score,
                        });
                    }
                }
            }
        }
    }
    best
}

fn beat_matched(a: &TrackAnalysis, b: &TrackAnalysis, out_dur: i64, in_dur: i64, max_len: i64, s: &AutoMixSettings, short_cause: &str) -> Result<TransitionPlan, String> {
    let bpm_a = bpm_with_tag(a.bpm, s.out_tag_bpm);
    let bpm_b = bpm_with_tag(b.bpm, s.in_tag_bpm);
    let mut ratio = match_ratio(bpm_a, bpm_b);
    let pct = (ratio - 1.0).abs() * 100.0;
    let mut max_pct = (s.max_tempo_change_pct as f64).clamp(0.0, 12.0);
    if !s.keep_pitch {
        max_pct = max_pct.min(VARISPEED_MAX_PCT);
    }
    if pct > max_pct + 1e-9 {
        return Err(format!("tempo gap {pct:.1} % over the {max_pct:.1} % limit"));
    }
    if (ratio - 1.0).abs() < 0.0005 {
        ratio = 1.0;
    }
    let bpb = bar_beats(a);
    if bar_beats(b) != bpb {
        return Err(format!("metres differ ({}/4 against {}/4)", bpb, bar_beats(b)));
    }
    let beat_a = 60_000.0 / bpm_a;
    let bar = bpb as f64 * beat_a;
    let b_bpm = fold(bpm_a, bpm_b);
    // Camelot distance 0-1: up to 16 bars; 2 or more (or a pair gate): 8.
    let dist = camelot_dist(a, b);
    let mild_clash = dist.is_some_and(|d| d > 2);
    let max_bars = if mild_clash || !short_cause.is_empty() || dist == Some(2) { 8 } else { 16 };
    let k = Lock { ratio, pct, bpm_a, b_bpm, bpb, beat: beat_a, bar, dist, mild_clash, max_bars, separate: s.filter_effects };
    let w = drop_aligned(a, b, out_dur, in_dur, max_len, bpm_b, &k).ok_or("no bar-aligned window fits")?;
    let ending = Ending::of(a, out_dur);
    let left = ending.exit < ending.music_end && (w.start + w.swap - ending.exit).abs() <= 0.5 * k.beat;
    let leaving = match (left, ending.breakdown) {
        (true, true) => "leaving on the closing breakdown",
        (true, false) => "leaving before the hidden track",
        _ => "",
    };
    let neutral = w.dur / k.bar > k.max_bars as f64 + 0.3;
    let bits = [
        w.landing.words(),
        if w.on_phrase { "on the outro phrase" } else { "" },
        leaving,
        if w.out_loop > 0.0 { "intro/outro remix" } else { "" },
        if neutral { "run-up without chords" } else { "" },
    ];
    Ok(finish_beat_matched(a, b, s, &k, &w, short_cause, &bits))
}

/// How long the bass takes to change hands: the sixteenth before the swap.
fn swap_len(beat: f64) -> f64 {
    (beat / 4.0).clamp(40.0, 150.0)
}

fn finish_beat_matched(a: &TrackAnalysis, b: &TrackAnalysis, s: &AutoMixSettings, k: &Lock, w: &Window, short_cause: &str, notes: &[&str]) -> TransitionPlan {
    let dur_ms = w.dur.round() as i64;
    let swap = (w.swap.round() as i64).clamp(0, dur_ms);
    let beat_ms = k.beat.round() as i64;
    let mut p = blank(TransitionKind::BeatMatched, w.start.round() as i64, w.in_start.round().max(0.0) as i64, dur_ms, String::new());
    p.tempo_ratio = k.ratio;
    p.keep_pitch = s.keep_pitch;
    if k.ratio != 1.0 {
        p.tempo_ramp_beats = if k.pct <= 2.0 { 16 } else { 32 };
        p.tempo_ramp_ms = (p.tempo_ramp_beats as f64 * (60_000.0 / k.b_bpm) / ((1.0 + k.ratio) / 2.0)).round() as i64;
    }
    p.fade_curve = FadeCurve::SineSquared;
    // Incoming rises over the run-up (at once without one); outgoing falls over the tail after the swap.
    (p.in_fade_start_ms, p.in_fade_end_ms) = (0, if swap > 0 { swap } else { (beat_ms / 8).clamp(1, dur_ms.max(1)) });
    (p.out_fade_start_ms, p.out_fade_end_ms) = (swap.min(dur_ms - 1).max(0), dur_ms);
    if s.bass_swap {
        let len = swap_len(k.beat).round() as i64;
        p.bass_swap = Some(BassSwap { at_ms: (swap - len).max(0), len_ms: len, cut_hz: BASS_CUT_HZ });
    }
    // Same key: no filter. Neighbours: soft low-pass. Distance 2: high-pass "filter open". Else: low-pass after
    // the swap.
    if s.filter_effects {
        match k.dist {
            Some(0) => {}
            Some(1) => {
                p.low_pass = Some(low_pass(((swap + dur_ms) / 2).max(swap), dur_ms, SWEEP_TO_HZ_SOFT));
            }
            Some(2) => {
                p.high_pass = Some(Sweep { start_ms: 0, end_ms: swap.max(beat_ms * 4).min(dur_ms), from_hz: 40.0, to_hz: 1_200.0 });
            }
            _ => {
                p.low_pass = Some(low_pass(swap, dur_ms, SWEEP_TO_HZ_MATCHED));
            }
        }
    }
    // Two singers: duck the incoming voice band until the swap, then thin the outgoing voice with a high-pass.
    let sung_end = a.outro_vocal.max(a.exit_vocal) >= VOCAL_MIN;
    let mut apart = false;
    if k.separate && sung_end {
        if swap > 0 && w.runup_vocal >= VOCAL_MIN {
            p.vocal_duck = Some(VocalDuck { until_ms: swap, release_ms: beat_ms.min(swap), db: VOCAL_DUCK_DB, hz: VOCAL_DUCK_HZ });
            apart = true;
        }
        if w.after_vocal >= VOCAL_MIN && dur_ms - swap >= beat_ms {
            p.high_pass = Some(Sweep { start_ms: swap, end_ms: swap + (dur_ms - swap) / 2, from_hz: VOCAL_HP_FROM_HZ, to_hz: VOCAL_HP_TO_HZ });
            apart = true;
        }
    }
    p.out_loop_ms = (w.out_loop > 0.0).then(|| w.out_loop.round() as i64);
    p.in_gain_db = loudness_trim(Some(a), Some(b), s);
    let mut bits: Vec<String> = notes.iter().filter(|n| !n.is_empty()).map(|n| n.to_string()).collect();
    if apart {
        bits.push("voices kept apart".to_string());
    }
    if w.runup_bars > 0 && p.out_loop_ms.is_none() {
        bits.push(format!("{}-bar run-up", w.runup_bars));
    }
    if k.mild_clash {
        bits.push("keys clash: short".to_string());
    } else if k.dist == Some(2) {
        bits.push("keys stretch: filter-open".to_string());
    } else if !short_cause.is_empty() {
        bits.push(format!("short ({short_cause})"));
    } else if k.dist == Some(0) {
        bits.push("same key".to_string());
    } else if k.dist == Some(1) {
        bits.push("harmonic".to_string());
    }
    let bars = w.dur / k.bar;
    let bars = if (bars - bars.round()).abs() < 0.01 { format!("{:.0}", bars) } else { format!("{bars:.2}") };
    p.reason = format!(
        "beat-matched {bars} bars, {:.1} -> {:.1} BPM ({:+.1} %), {}",
        k.b_bpm,
        k.bpm_a,
        (k.ratio - 1.0) * 100.0,
        bits.join(", ")
    );
    p
}

/// A fade aligned to the one usable grid when the two could not be locked: an exit on outgoing downbeats, or an
/// entrance on an incoming downbeat. No tempo change.
fn one_grid(a: Option<&TrackAnalysis>, b: Option<&TrackAnalysis>, out_dur: i64, in_dur: i64, max_len: i64, s: &AutoMixSettings, short: bool, why_not: &str) -> Option<TransitionPlan> {
    let tail = if why_not.is_empty() { String::new() } else { format!(" ({why_not})") };
    let ending = a.map(|x| Ending::of(x, out_dur));
    let end_a = ending.map_or(out_dur, |e| e.leave(0.0).round() as i64);
    let skipped = |heard_end: i64| ending.map_or((out_dur - heard_end) as f64, |e| e.skipped(heard_end as f64));
    // 8 bars, else 4 (only 4 when the pair gates shorten it).
    let out_bars: &[i64] = if short { &[4] } else { &[8, 4] };
    if let Some(g) = a.filter(|x| grid_ok(x)) {
        let beat = 60_000.0 / g.bpm;
        let bpb = bar_beats(g);
        let phase = (g.downbeat_phase as i64).rem_euclid(bpb);
        let in_start = b.map_or(0, |x| x.silence_start_ms.clamp(0, in_dur / 3));
        for bars in out_bars {
            let dur = (*bars as f64 * bpb as f64 * beat).round() as i64;
            if dur > max_len || dur < MIN_FADE_MS {
                continue;
            }
            let mut n = ((end_a as f64 - dur as f64 - g.beat_offset_ms) / beat).floor() as i64;
            while n.rem_euclid(bpb) != phase {
                n -= 1;
            }
            let start = (g.beat_offset_ms + n as f64 * beat).round() as i64;
            if start < g.silence_start_ms || skipped(start + dur) > MAX_SKIP_MS as f64 {
                continue;
            }
            let mut p = blank(TransitionKind::MixRampFade, start, in_start, dur, String::new());
            p.fade_curve = FadeCurve::SineSquared;
            apply_fade_filter(&mut p, a, b, s, dur);
            p.in_gain_db = loudness_trim(a, b, s);
            p.reason = format!("downbeat-aligned fade, {bars} bars out{tail}");
            return Some(p);
        }
    }
    // Else enter on the incoming song's first downbeat.
    if let Some(g) = b.filter(|x| grid_ok(x)) {
        let beat = 60_000.0 / g.bpm;
        let bpb = bar_beats(g);
        let phase = (g.downbeat_phase as i64).rem_euclid(bpb);
        let mut nb = (((g.silence_start_ms as f64 - g.beat_offset_ms) / beat) - 0.25).ceil() as i64;
        while nb.rem_euclid(bpb) != phase {
            nb += 1;
        }
        let in_start = (g.beat_offset_ms + nb as f64 * beat).round().max(0.0) as i64;
        if in_start - g.silence_start_ms > MAX_SKIP_MS {
            return None;
        }
        for bars in out_bars {
            let dur = (*bars as f64 * bpb as f64 * beat).round() as i64;
            if dur > max_len || dur < MIN_FADE_MS || dur > in_dur - in_start {
                continue;
            }
            let start = end_a - dur;
            if start < 0 || skipped(start + dur) > MAX_SKIP_MS as f64 {
                continue;
            }
            let mut p = blank(TransitionKind::MixRampFade, start, in_start, dur, String::new());
            p.fade_curve = FadeCurve::SineSquared;
            apply_fade_filter(&mut p, a, b, s, dur);
            p.in_gain_db = loudness_trim(a, b, s);
            p.reason = format!("downbeat-aligned fade, {bars} bars in{tail}");
            return Some(p);
        }
    }
    None
}

fn mixramp(a: Option<&TrackAnalysis>, b: Option<&TrackAnalysis>, out_dur: i64, in_dur: i64, max_len: i64, s: &AutoMixSettings, why_not: &str) -> TransitionPlan {
    let end_a = a.map_or(out_dur, |a| Ending::of(a, out_dur).leave(0.0).round() as i64);
    let in_start = b.map_or(0, |b| b.silence_start_ms.clamp(0, in_dur / 3));
    // The outgoing quiet tail and the incoming quiet head (an exit before the end has no quiet tail).
    let tail = a.map(|a| if a.mixramp_end_ms > 0 && a.mixramp_end_ms <= end_a { end_a - a.mixramp_end_ms } else { 0 });
    let head = b.map(|b| if b.mixramp_start_ms > 0 { (b.mixramp_start_ms - in_start).max(0) } else { 0 });
    let mut dur = tail.unwrap_or(0) + head.unwrap_or(0);
    if tail.is_none() || head.is_none() {
        dur = dur.max(max_len.min(4000));
    }
    let dur = dur.max(MIN_MIXRAMP_MS).clamp(MIN_FADE_MS.min(max_len), max_len);
    let mut p = blank(TransitionKind::MixRampFade, end_a - dur, in_start, dur, String::new());
    // A loud start comes in at once; a quiet one rides its own ramp.
    p.in_fade_end_ms = head.map_or(dur, |h| h.clamp(MIN_FADE_MS.min(dur), dur));
    apply_fade_filter(&mut p, a, b, s, dur);
    p.in_gain_db = loudness_trim(a, b, s);
    p.reason = format!("mixramp fade {:.1} s{}", dur as f64 / 1000.0, if why_not.is_empty() { String::new() } else { format!(" ({why_not})") });
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automix::structure::camelot;

    /// A 4-minute track, 4/4, grid from 120 ms, an 8-bar intro, a 16-bar outro and silence at both ends.
    fn track(bpm: f64) -> TrackAnalysis {
        let beat = 60_000.0 / bpm;
        TrackAnalysis {
            song_id: "t".into(),
            analysis_version: 1,
            duration_ms: 240_000,
            bpm,
            bpm_confidence: 0.9,
            beat_offset_ms: 120.0,
            stability: 0.9,
            downbeat_phase: 0,
            downbeat_confidence: 0.8,
            beats_per_bar: 4,
            lufs: -9.0,
            key: camelot(0, false),
            key_confidence: 0.8,
            silence_start_ms: 100,
            silence_end_ms: 238_500,
            mixramp_start_ms: 400,
            mixramp_end_ms: 236_000,
            intro_end_ms: (120.0 + 32.0 * beat) as i64,
            outro_start_ms: (120.0 + beat * 4.0 * (((238_500.0 - 120.0) / (4.0 * beat)).floor() - 16.0)) as i64,
            outro_vocal: 0.1,
            intro_vocal: 0.1,
            outro_centroid: 1200.0,
            intro_centroid: 1200.0,
            analysed_ms: 0,
            outro_bpm: bpm,
            outro_bpm_confidence: 0.9,
            outro_beat_offset_ms: 120.0,
            outro_stability: 0.9,
            outro_downbeat_phase: 0,
            intro_bpm: bpm,
            intro_bpm_confidence: 0.9,
            intro_beat_offset_ms: 120.0,
            intro_stability: 0.9,
            intro_downbeat_phase: 0,
            drop_ms: 0,
            drop_runup_vocal: 0.0,
            drop_vocal: 0.0,
            drop_runup_tonal_db: 0.0,
            exit_ms: 0,
            gap_ms: 0,
            gap_end_ms: 0,
            exit_vocal: 0.1,
            intro_beats_per_bar: 0,
            outro_beats_per_bar: 0,
            intro_grid_source: 0,
            outro_grid_source: 0,
        }
    }

    /// At most `MAX_SKIP_MS` of either song's music goes unplayed.
    fn check_skip(p: &TransitionPlan, a: &TrackAnalysis, b: &TrackAnalysis, out_dur: i64) {
        assert!(p.in_start_ms - b.silence_start_ms <= MAX_SKIP_MS, "{p:?}");
        let heard_end = p.out_start_ms + p.out_loop_ms.unwrap_or(p.duration_ms);
        assert!(Ending::of(a, out_dur).skipped(heard_end as f64) <= MAX_SKIP_MS as f64, "{p:?}");
        assert!(p.out_start_ms >= 0 && p.duration_ms >= 0 && heard_end <= out_dur, "{p:?}");
    }

    /// `check_skip` without analyses.
    fn check_skip_blind(p: &TransitionPlan, out_dur: i64) {
        assert!(p.in_start_ms <= MAX_SKIP_MS && out_dur - (p.out_start_ms + p.duration_ms) <= MAX_SKIP_MS, "{p:?}");
    }

    #[test]
    fn same_album_in_order_is_gapless() {
        let s = AutoMixSettings { same_album_in_order: true, ..Default::default() };
        let p = plan(Some(&track(128.0)), Some(&track(128.0)), 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::Gapless);
        assert_eq!((p.out_start_ms, p.in_start_ms, p.duration_ms), (240_000, 0, 0));
    }

    #[test]
    fn unanalysed_is_equal_power_fade() {
        let s = AutoMixSettings { max_transition_s: 8.0, ..Default::default() };
        let p = plan(None, None, 200_000, 180_000, &s);
        assert_eq!(p.kind, TransitionKind::EqualPowerFade);
        assert_eq!(p.fade_curve, FadeCurve::EqualPower);
        assert_eq!((p.out_start_ms, p.in_start_ms, p.duration_ms), (192_000, 0, 8000));
        assert_eq!((p.bass_swap, p.low_pass, p.tempo_ratio), (None, None, 1.0));
        check_skip_blind(&p, 200_000);
        // Capped, and at most a third of a short track.
        let p = plan(None, None, 200_000, 180_000, &AutoMixSettings { max_transition_s: 30.0, ..Default::default() });
        assert_eq!(p.duration_ms, 12_000);
        let p = plan(None, None, 200_000, 9_000, &AutoMixSettings { max_transition_s: 30.0, ..Default::default() });
        assert_eq!(p.duration_ms, 3_000);
        let p = plan(None, None, 200_000, 600, &s);
        assert_eq!(p.kind, TransitionKind::Gapless);
    }

    /// Where the bass has changed hands, relative to the start of the mix.
    fn swap_at(p: &TransitionPlan) -> i64 {
        p.bass_swap.map_or(-1, |s| s.at_ms + s.len_ms)
    }

    /// Where `t` of the incoming track (its own time) is heard, relative to the start of the mix.
    fn lands(p: &TransitionPlan, t: i64) -> f64 {
        (t - p.in_start_ms) as f64 / p.tempo_ratio
    }

    /// Whether `t` of the outgoing track is on one of its downbeats.
    fn on_downbeat(a: &TrackAnalysis, t: f64) -> bool {
        let beats = (t - a.beat_offset_ms) / (60_000.0 / a.bpm);
        (beats - beats.round()).abs() < 0.01 && (beats.round() as i64).rem_euclid(4) == a.downbeat_phase as i64
    }

    #[test]
    fn confident_grids_beat_match() {
        let s = AutoMixSettings::default();
        let (a, b) = (track(128.0), track(124.0));
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        assert!((p.tempo_ratio - 128.0 / 124.0).abs() < 1e-9);
        assert!(p.keep_pitch);
        assert_eq!(p.tempo_ramp_beats, 32, "a 3 % change ramps back over 8 bars");
        let (beat, bar) = (60_000.0 / 128.0, 4.0 * 60_000.0 / 128.0);
        // The whole 8-bar intro under the outgoing song, the swap at its end, one beat of tail (16 s at 128 BPM).
        assert_eq!(p.in_start_ms, 120);
        assert!((p.duration_ms as f64 - 8.0 * bar - beat).abs() <= 1.0, "{}: {}", p.duration_ms, p.reason);
        assert!((lands(&p, b.intro_end_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        assert!(on_downbeat(&a, p.out_start_ms as f64) && on_downbeat(&a, (p.out_start_ms + swap_at(&p)) as f64), "{}", p.out_start_ms);
        assert_eq!(p.bass_swap.unwrap().len_ms, (beat / 4.0).round() as i64);
        assert_eq!((p.in_fade_start_ms, p.in_fade_end_ms), (0, swap_at(&p)));
        assert_eq!((p.out_fade_start_ms, p.out_fade_end_ms), (swap_at(&p), p.duration_ms));
        assert_eq!(p.fade_curve, FadeCurve::SineSquared);
        assert_eq!(p.low_pass, None, "same key: no low-pass");
        assert_eq!(p.in_gain_db, 0.0, "loudness matching is off by default");
        assert!(p.reason.contains("same key") && p.reason.contains("end of the intro"), "{}", p.reason);

        // With room for more, a longer tail after the swap.
        let long = AutoMixSettings { max_transition_s: 40.0, ..Default::default() };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &long);
        assert!(p.duration_ms as f64 > 9.0 * bar && p.duration_ms <= 40_000, "{}", p.reason);
        assert!((lands(&p, b.intro_end_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn enters_on_the_drop() {
        // Intro ends 4 bars in, drop 8 bars in: the swap is on the drop.
        let beat: f64 = 60_000.0 / 128.0;
        let b = TrackAnalysis { intro_end_ms: (120.0 + 16.0 * beat) as i64, drop_ms: (120.0 + 32.0 * beat) as i64, ..track(128.0) };
        let a = track(128.0);
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!((lands(&p, b.drop_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        assert!(p.reason.contains("on the drop"), "{}", p.reason);
        assert_eq!(p.in_start_ms, 120, "the whole run-up fits: nothing skipped");
        check_skip(&p, &a, &b, 240_000);

        // Drop 12 bars in: 4 bars (7.5 s) skipped to reach it. 20 bars in would skip 22 s: never.
        let b = TrackAnalysis { drop_ms: (120.0 + 48.0 * beat) as i64, intro_end_ms: (120.0 + 48.0 * beat) as i64, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert!((lands(&p, b.drop_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        assert_eq!(p.in_start_ms, (120.0 + 16.0 * beat).round() as i64, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        let b = TrackAnalysis { drop_ms: (120.0 + 80.0 * beat) as i64, intro_end_ms: (120.0 + 80.0 * beat) as i64, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        assert!(lands(&p, b.drop_ms) > swap_at(&p) as f64, "the drop is out of reach: {}", p.reason);
        let long = AutoMixSettings { max_transition_s: 45.0, ..Default::default() };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &long);
        assert!((lands(&p, b.drop_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn chordless_runup_escapes_key_cap() {
        // Camelot 2 caps chords over chords at 8 bars; a 16-bar drum run-up still fits whole, a pad one does not.
        let beat: f64 = 60_000.0 / 128.0;
        let a = track(128.0);
        let drop = (120.0 + 64.0 * beat) as i64;
        let drums = TrackAnalysis { key: camelot(2, false), drop_ms: drop, intro_end_ms: drop, drop_runup_tonal_db: -12.0, ..track(128.0) };
        let long = AutoMixSettings { max_transition_s: 40.0, ..Default::default() };
        let p = plan(Some(&a), Some(&drums), 240_000, 240_000, &long);
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!((lands(&p, drop) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        assert_eq!(p.in_start_ms, 120, "the whole intro under the outgoing song: {}", p.reason);
        assert!(p.reason.contains("without chords"), "{}", p.reason);
        check_skip(&p, &a, &drums, 240_000);
        let pads = TrackAnalysis { drop_runup_tonal_db: -1.0, ..drums };
        let p = plan(Some(&a), Some(&pads), 240_000, 240_000, &long);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * beat + 1.0, "{}", p.reason);
        check_skip(&p, &a, &pads, 240_000);
    }

    #[test]
    fn runup_is_whole_phrases() {
        // A 16-bar intro into a 16 s mix: a run-up of 4 or 8 bars, never 6.
        let beat: f64 = 60_000.0 / 128.0;
        let a = track(128.0);
        let b = TrackAnalysis { drop_ms: (120.0 + 64.0 * beat) as i64, intro_end_ms: (120.0 + 64.0 * beat) as i64, drop_runup_tonal_db: -12.0, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        let runup = swap_at(&p) as f64 / (4.0 * beat);
        assert!((runup - runup.round()).abs() < 0.01 && runup.round() as i64 % 4 == 0, "{runup}: {}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn full_start_swaps_on_phrase_line() {
        // No intro or drop: in on the first downbeat, swap on a four-bar line.
        let b = TrackAnalysis { intro_end_ms: 100, ..track(128.0) };
        let a = track(128.0);
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert_eq!(p.in_start_ms, 120);
        let bar = 4.0 * 60_000.0 / 128.0;
        let swap_bars = swap_at(&p) as f64 / bar;
        assert!((swap_bars - swap_bars.round()).abs() < 0.01 && swap_bars.round() as i64 % 4 == 0, "{swap_bars}: {}", p.reason);
        assert!(on_downbeat(&a, (p.out_start_ms + swap_at(&p)) as f64));
    }

    #[test]
    fn harmonic_neighbour_gets_soft_filter_long_mix() {
        let a = track(128.0);
        let b = TrackAnalysis { key: camelot(7, false), ..track(124.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!(p.reason.contains("harmonic"), "{}", p.reason);
        let bar = 4.0 * 60_000.0 / 128.0;
        assert!(p.duration_ms as f64 > 9.0 * bar, "{}", p.duration_ms);
        assert_eq!(p.low_pass.map(|f| f.to_hz), Some(SWEEP_TO_HZ_SOFT), "soft low-pass");
    }

    #[test]
    fn key_distance_two_opens_high_pass() {
        let a = track(128.0);
        let b = TrackAnalysis { key: camelot(2, false), ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!(p.reason.contains("filter-open"), "{}", p.reason);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.duration_ms);
        assert!(p.high_pass.is_some_and(|f| f.to_hz > 500.0), "filter-open high-pass");
        assert_eq!(p.low_pass, None);
    }

    #[test]
    fn short_outro_loops_unless_sung() {
        // Five bars of outgoing music, an 8-bar incoming intro: the last four outgoing bars loop under the run-up.
        let beat = 60_000.0 / 128.0;
        let music = (5.0 * 4.0 * beat) as i64;
        let a = TrackAnalysis {
            silence_start_ms: 238_500 - music,
            silence_end_ms: 238_500,
            outro_start_ms: 238_500 - music,
            mixramp_end_ms: 238_500 - music / 2,
            ..track(128.0)
        };
        let b = track(128.0);
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        let out_loop = p.out_loop_ms.expect(&p.reason);
        assert!(p.reason.contains("remix"), "{}", p.reason);
        assert!(p.duration_ms > out_loop);
        assert_eq!(p.in_start_ms, 120, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        let a = TrackAnalysis { outro_vocal: 0.7, ..a };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert_eq!(p.out_loop_ms, None, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn tag_bpm_settles_a_half_double() {
        let a = TrackAnalysis { bpm: 64.0, ..track(128.0) };
        let b = track(128.0);
        let s = AutoMixSettings { out_tag_bpm: 128.0, max_transition_s: 40.0, ..Default::default() };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!((p.tempo_ratio - 1.0).abs() < 0.01, "folded to tag: ratio {}", p.tempo_ratio);
    }

    #[test]
    fn half_and_double_tempo_match() {
        let p = plan(Some(&track(174.0)), Some(&track(88.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!((p.tempo_ratio - 174.0 / 176.0).abs() < 1e-9);
        assert_eq!(p.tempo_ramp_beats, 16);
    }

    #[test]
    fn tempo_limits_fall_back_to_mixramp() {
        let (a, b) = (track(128.0), track(118.0)); // 8.5 %
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::MixRampFade);
        assert!(p.reason.contains("tempo gap"), "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);

        let b = track(125.0); // 2.4 %: fine for time-stretch, too much for varispeed
        let s = AutoMixSettings { keep_pitch: false, ..Default::default() };
        assert_eq!(plan(Some(&a), Some(&b), 240_000, 240_000, &s).kind, TransitionKind::MixRampFade);
        let b = track(126.0); // 1.6 %
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::BeatMatched);
        assert!(!p.keep_pitch);

        let off = AutoMixSettings { beat_match: false, ..Default::default() };
        let p = plan(Some(&a), Some(&a), 240_000, 240_000, &off);
        assert_eq!(p.kind, TransitionKind::MixRampFade);
        assert!(p.reason.contains("beat matching off"));
    }

    #[test]
    fn drifting_band_matched_on_end_grids() {
        let a = TrackAnalysis { stability: 0.0, ..track(128.0) };
        let b = TrackAnalysis { stability: 0.0, ..track(126.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn mixes_at_outro_tempo() {
        let a = TrackAnalysis { bpm: 100.0, ..track(128.0) };
        let p = plan(Some(&a), Some(&track(128.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert_eq!(p.tempo_ratio, 1.0, "the ends already agree: {}", p.reason);
    }

    #[test]
    fn unreliable_grids_are_not_beat_matched() {
        let a = track(128.0);
        for b in [
            TrackAnalysis { bpm_confidence: 0.2, intro_bpm_confidence: 0.2, ..track(128.0) },
            TrackAnalysis { stability: 0.3, intro_stability: 0.3, ..track(128.0) },
            TrackAnalysis { bpm: 0.0, intro_bpm: 0.0, ..track(128.0) },
        ] {
            let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
            assert_eq!(p.kind, TransitionKind::MixRampFade, "{b:?}");
            assert!(p.reason.contains("no reliable beat grid"));
        }
    }

    #[test]
    fn one_grid_aligns_fade() {
        // Outgoing grid only: 8 bars from an outgoing downbeat.
        let (a, b) = (track(128.0), TrackAnalysis { bpm_confidence: 0.2, intro_bpm_confidence: 0.2, ..track(128.0) });
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::MixRampFade);
        assert!(p.reason.contains("downbeat-aligned") && p.reason.contains("bars out"), "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        let beat = 60_000.0 / 128.0;
        assert!((p.duration_ms as f64 - 8.0 * 4.0 * beat).abs() <= 1.0, "{}", p.duration_ms);
        let beats = (p.out_start_ms as f64 - 120.0) / beat;
        assert!((beats - beats.round()).abs() < 0.01 && (beats.round() as i64) % 4 == 0, "{beats}");

        // Incoming grid only: in on its first downbeat.
        let (a, b) = (TrackAnalysis { stability: 0.3, outro_stability: 0.3, ..track(128.0) }, track(128.0));
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert!(p.reason.contains("bars in"), "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        assert_eq!(p.in_start_ms, 120);
    }

    #[test]
    fn mixramp_overlaps_the_quiet_ends() {
        let s = AutoMixSettings { max_transition_s: 12.0, match_loudness: true, ..Default::default() };
        let a = TrackAnalysis { bpm: 0.0, outro_bpm: 0.0, silence_end_ms: 230_000, mixramp_end_ms: 226_000, lufs: -8.0, ..track(128.0) };
        let b = TrackAnalysis { bpm: 0.0, intro_bpm: 0.0, silence_start_ms: 1_000, mixramp_start_ms: 3_000, lufs: -14.0, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::MixRampFade);
        assert_eq!((p.out_start_ms, p.in_start_ms, p.duration_ms), (224_000, 1_000, 6_000));
        assert_eq!(p.in_fade_end_ms, 2_000, "the incoming fade follows its own ramp");
        assert_eq!(p.low_pass.map(|f| (f.start_ms, f.to_hz)), Some((3_000, SWEEP_TO_HZ_SOFT)), "same key: soft low-pass in the second half");
        assert_eq!(p.in_gain_db, 6.0, "incoming 6 dB quieter gets 6 dB");
        check_skip(&p, &a, &b, 240_000);

        // An abrupt end into a loud start still shares the last five seconds.
        let a = TrackAnalysis { bpm: 0.0, outro_bpm: 0.0, silence_end_ms: 240_000, mixramp_end_ms: 240_000, ..track(128.0) };
        let b = TrackAnalysis { bpm: 0.0, intro_bpm: 0.0, silence_start_ms: 0, mixramp_start_ms: 0, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
        assert_eq!((p.duration_ms, p.in_fade_end_ms, p.out_start_ms), (MIN_MIXRAMP_MS, MIN_FADE_MS, 240_000 - MIN_MIXRAMP_MS));

        let p = plan(Some(&a), None, 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::MixRampFade);
        assert_eq!(p.duration_ms, MIN_MIXRAMP_MS);
        assert_eq!(p.in_start_ms, 0);
    }

    #[test]
    fn silence_not_mixed_over() {
        // 40 s of trailing silence and a 25 s silent lead-in are skipped whole.
        let a = TrackAnalysis { silence_end_ms: 200_000, mixramp_end_ms: 198_000, outro_start_ms: 170_000, ..track(128.0) };
        let b = TrackAnalysis {
            silence_start_ms: 25_000,
            mixramp_start_ms: 26_000,
            beat_offset_ms: 25_020.0 % (60_000.0 / 128.0),
            intro_beat_offset_ms: 25_020.0 % (60_000.0 / 128.0),
            intro_end_ms: 25_020 + 15_000,
            ..track(128.0)
        };
        for s in [AutoMixSettings::default(), AutoMixSettings { beat_match: false, ..Default::default() }] {
            let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
            check_skip(&p, &a, &b, 240_000);
            let swap = if p.bass_swap.is_some() { swap_at(&p) } else { p.duration_ms / 2 };
            assert!(p.out_start_ms + swap <= 200_000 + 500, "{}: {p:?}", p.reason);
            assert!(p.in_start_ms >= 25_000 - 500, "{}: {p:?}", p.reason);
        }
    }

    #[test]
    fn leaves_on_closing_breakdown() {
        // A six-bar coda: the swap lands where it begins.
        let bar: f64 = 4.0 * 60_000.0 / 128.0;
        let exit = (120.0 + bar * (((238_500.0 - 120.0) / bar).floor() - 6.0)) as i64;
        let a = TrackAnalysis { exit_ms: exit, ..track(128.0) };
        let b = track(128.0);
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!(((p.out_start_ms + swap_at(&p)) - exit).abs() <= 2, "{}: swap at {}", p.reason, p.out_start_ms + swap_at(&p));
        assert!(p.reason.contains("closing breakdown"), "{}", p.reason);
        assert!((lands(&p, b.intro_end_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        // A coda too long to skip is played.
        let exit = (120.0 + bar * (((238_500.0 - 120.0) / bar).floor() - 14.0)) as i64;
        let a = TrackAnalysis { exit_ms: exit, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        check_skip(&p, &a, &b, 240_000);
        assert!(p.out_start_ms + swap_at(&p) > exit, "{}", p.reason);
    }

    #[test]
    fn leaves_before_short_hidden_track() {
        // Song ends at 180 s, 40 s of silence, a 10 s hidden track: mixed at 180 s.
        let bar: f64 = 4.0 * 60_000.0 / 128.0;
        let end = (120.0 + bar * 96.0) as i64; // a downbeat, 180 s
        let a = TrackAnalysis {
            exit_ms: end,
            gap_ms: end,
            gap_end_ms: end + 40_000,
            silence_end_ms: end + 50_000,
            mixramp_end_ms: end + 49_000,
            outro_start_ms: end - (16.0 * bar) as i64,
            ..track(128.0)
        };
        let b = track(128.0);
        for s in [AutoMixSettings::default(), AutoMixSettings { beat_match: false, ..Default::default() }] {
            let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
            check_skip(&p, &a, &b, 240_000);
            assert!(p.out_start_ms + p.duration_ms <= end + 2_000, "{}: {p:?}", p.reason);
        }
        // 30 s of hidden music is over the cap: played.
        let a = TrackAnalysis { exit_ms: 0, silence_end_ms: end + 70_000, mixramp_end_ms: end + 69_000, ..a };
        let p = plan(Some(&a), Some(&b), 260_000, 240_000, &AutoMixSettings::default());
        check_skip(&p, &a, &b, 260_000);
        assert!(p.out_start_ms > end + 40_000, "{}: {p:?}", p.reason);
    }

    #[test]
    fn clashing_keys_echo_out() {
        let a = track(128.0);
        let b = TrackAnalysis { key: camelot(6, false), ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::EchoOut, "{}", p.reason);
        assert!(p.reason.contains("keys far apart"), "{}", p.reason);
        assert_eq!(p.echo, Some(Echo { delay_ms: (60_000.0_f64 / 128.0).round() as i64, feedback: 0.45, wet_db: -7.0 }));
        assert_eq!(p.bass_swap, None, "no bass swap on an echo-out");
        assert!((p.tempo_ratio - 1.0).abs() < 1e-9);
        check_skip(&p, &a, &b, 240_000);
        // Echo off: a short beat-matched mix.
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, echo_out: false, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.reason);
        assert!(p.reason.contains("clash"));
    }

    #[test]
    fn vocals_kept_apart_and_gaps_shorten() {
        // Both sing: a short beat-matched mix, incoming voice ducked until the swap, outgoing thinned after it.
        let sung = |t: TrackAnalysis| TrackAnalysis { outro_vocal: 0.7, intro_vocal: 0.7, exit_vocal: 0.7, ..t };
        let (a, b) = (sung(track(128.0)), sung(track(128.0)));
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!(p.reason.contains("voices kept apart") && p.reason.contains("vocals overlap"), "{}", p.reason);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.reason);
        let swap = swap_at(&p);
        assert!(swap > 0 && p.duration_ms - swap >= (60_000.0f64 / 128.0) as i64, "{}", p.reason);
        let d = p.vocal_duck.expect(&p.reason);
        assert_eq!((d.until_ms, d.db), (swap, VOCAL_DUCK_DB), "{}", p.reason);
        assert!(d.release_ms > 0 && d.release_ms <= swap);
        let h = p.high_pass.expect(&p.reason);
        assert_eq!((h.start_ms, h.end_ms, h.to_hz), (swap, swap + (p.duration_ms - swap) / 2, VOCAL_HP_TO_HZ), "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        // Filters off: echo-out.
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { filter_effects: false, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::EchoOut, "{}", p.reason);
        assert!(p.reason.contains("vocals overlap"), "{}", p.reason);
        let p = plan(Some(&track(128.0)), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!((p.vocal_duck, p.high_pass), (None, None), "{}", p.reason);
        // No grid: a fade with the incoming voice ducked through its first half.
        let (a, b) = (TrackAnalysis { bpm: 0.0, outro_bpm: 0.0, ..a }, TrackAnalysis { bpm: 0.0, intro_bpm: 0.0, ..b });
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::MixRampFade, "{}", p.reason);
        assert_eq!(p.vocal_duck.map(|d| d.until_ms), Some(p.duration_ms / 2), "{}", p.reason);
        // Loudness gap: short.
        let b = TrackAnalysis { lufs: -2.0, ..track(128.0) };
        let p = plan(Some(&track(128.0)), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.reason);
        // Timbre mismatch: short.
        let b = TrackAnalysis { intro_centroid: 5000.0, ..track(128.0) };
        let p = plan(Some(&track(128.0)), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.reason);
    }

    #[test]
    fn stale_analysis_ignored() {
        let a = TrackAnalysis { duration_ms: 300_000, ..track(128.0) };
        let p = plan(Some(&a), None, 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::EqualPowerFade);
    }

    #[test]
    fn waltz_mixes_in_threes_only() {
        let waltz = |bpm| TrackAnalysis { beats_per_bar: 3, ..track(bpm) };
        let p = plan(Some(&waltz(150.0)), Some(&waltz(150.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        let bar = 3.0 * 60_000.0 / 150.0;
        let bars = p.duration_ms as f64 / bar;
        assert!((bars - bars.round()).abs() < 0.01, "{} ms is not whole bars of three", p.duration_ms);
        let n = (p.out_start_ms as f64 - 120.0) / (60_000.0 / 150.0);
        assert!((n - n.round()).abs() < 0.01 && (n.round() as i64).rem_euclid(3) == 0, "{}", p.out_start_ms);

        let p = plan(Some(&waltz(150.0)), Some(&track(150.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_ne!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!(p.reason.contains("metres differ"), "{}", p.reason);
        // A row without a metre reads as 4/4.
        let p = plan(Some(&TrackAnalysis { beats_per_bar: 0, ..track(128.0) }), Some(&track(128.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched);
    }

    #[test]
    fn switches_turn_effects_off() {
        let s = AutoMixSettings { bass_swap: false, filter_effects: false, ..Default::default() };
        let p = plan(Some(&track(128.0)), Some(&track(128.0)), 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::BeatMatched);
        assert_eq!((p.bass_swap, p.low_pass, p.tempo_ratio, p.tempo_ramp_ms), (None, None, 1.0, 0));
    }
}
