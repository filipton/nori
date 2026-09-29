//! Transition planning: a pure function from what is known about two tracks to a `TransitionPlan`.
//! The fallback ladder from the research, most to least informed:
//!
//! 1. far-apart keys: an echo-out, timed by the outgoing grid (two singers are kept apart by filters first, and
//!    echo out only when no beat-matched mix fits or the filters are off);
//! 2. both grids confident and stable, tempos within reach: beat-matched and entered on the drop (`drop_aligned`):
//!    the incoming song's drop (or the end of its intro, or a phrase line) lands on a downbeat of the outgoing one
//!    and the bass changes hands there, with an instrumental run-up of whole phrases laid under the outgoing song.
//!    Camelot distance shapes length and filter: same key = long natural blend, neighbour = soft LPF,
//!    farther = short + classic sweep (Apple iOS 27 / DJ.Studio Harmonize); a run-up without chords is exempt;
//! 3. something analysed, no usable grid: overlap from the MixRamp points and trimmed silence, with a filter sweep;
//! 4. nothing known: a fixed equal-power crossfade;
//! 5. same album in order: gapless, no mixing.
//!
//! A loudness gap or a timbre mismatch shortens whatever the ladder picks. Every gate only demotes:
//! a clash shortens or reroutes a transition, never upgrades one.
//!
//! Hard rule for every plan: at most `MAX_SKIP_MS` of either track's music goes unplayed (Apple's AutoMix is
//! criticised for skipping up to a minute to line tempos up). Silence is free: the gap before a hidden track, the
//! silence a file carries at either end. The outgoing song may be left before its end when the ending is not worth
//! playing through (`Ending`): a closing breakdown, or a hidden track after a long silence, as long as the music
//! left out fits the cap.

use super::structure::key_distance;
use super::tempo::{fold, match_ratio};
use crate::types::{AutoMixSettings, BassSwap, Echo, FadeCurve, Sweep, TrackAnalysis, TransitionKind, TransitionPlan, VocalDuck};

pub const MAX_SKIP_MS: i64 = 15_000;
pub const MIN_BPM_CONFIDENCE: f32 = 0.5;
pub const MIN_STABILITY: f32 = 0.6;
/// Varispeed moves the pitch 0.34 semitone per 2 %; beyond that it is audible as detuning.
pub const VARISPEED_MAX_PCT: f64 = 2.0;
/// Longest fixed crossfade.
const MAX_BLIND_FADE_MS: i64 = 12_000;
/// Shortest overlap that is still a fade rather than a click guard.
const MIN_FADE_MS: i64 = 300;
/// Shortest MixRamp overlap. MixRamp lays the songs together where the outgoing one has already gone
/// quiet, which on a song that fades out is its last nearly silent second or two - heard as no mix
/// at all ("MIX_RAMP_FADE 300 ms"). This much of the ending is always shared, fading as it goes.
const MIN_MIXRAMP_MS: i64 = 5_000;
const BASS_CUT_HZ: f32 = 180.0;
const SWEEP_FROM_HZ: f32 = 18_000.0;
/// Where the outgoing low-pass ends: beat-matched (lows already swapped out) and plain fades.
const SWEEP_TO_HZ_MATCHED: f32 = 400.0;
const SWEEP_TO_HZ_FADE: f32 = 500.0;
/// Camelot neighbour / relative: gentle muffling, not Apple iOS 26's "underwater" dump.
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

/// An analysis only counts when it describes this file: same length within 3 s.
fn usable(a: Option<&TrackAnalysis>, duration_ms: i64) -> Option<&TrackAnalysis> {
    a.filter(|a| sound(a) && (a.duration_ms <= 0 || duration_ms <= 0 || (a.duration_ms - duration_ms).abs() <= 3000))
}

/// The fastest tempo a stored grid may have and still be taken for one.
const MAX_BPM: f64 = 1_000.0;
/// Further than this from a song's start (ms, a hundred hours) is no place in it.
const MAX_MS: f64 = 3.6e8;

/// Whether every number of `a` a plan is made of is one: finite, tempos a song can have, places inside a
/// song's reach. A row that is not (a measurement gone wrong) is planned without, as a song not measured,
/// rather than into nonsense - or a panic on the engine's thread, which plans (a clamp between one and a
/// window rounded to nought, a count of beats past the integers).
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

/// Beats in a bar of `a`: 3 for a waltz, else 4 (also for rows from before metres were measured).
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

/// The song as the mix sees it at one end: its grid replaced by the one measured over that end alone.
/// A whole-song grid asks one tempo to fit four minutes, which a band without a click track never
/// does - every one of them scored no stability, so nothing was ever beat-matched - and a song that
/// changes tempo half way has the wrong answer at its end. The window's grid is used whenever it
/// holds; the whole song's only when it holds and the window's does not (too little music at that end).
/// An end read by Beat This! carries its own metre (`meter`, 0 when it has none of its own).
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

/// Prefer the analysis BPM folded toward a server/tag prior when that settles a half/double error.
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

/// Where the outgoing song's music worth hearing ends, and what a mix that stops hearing it early leaves out. The
/// hard rule is about music: at most `MAX_SKIP_MS` of it goes unplayed. Silence - a hidden track's gap, the silence
/// a file carries after its music - costs nothing, and is never what a mix is laid over.
#[derive(Clone, Copy, Debug)]
pub(super) struct Ending {
    /// End of the last audible music, ms.
    pub music_end: f64,
    /// The long silence inside the music, when there is one.
    pub gap: Option<(f64, f64)>,
    /// Where to leave: the start of a closing breakdown or of the gap before a hidden track short enough to skip;
    /// the end of the music otherwise.
    pub exit: f64,
    /// `exit` is a closing breakdown: music, only music not worth playing through.
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

    /// Where a mix may leave: the exit when what follows it fits the cap with `slack` of it still heard (the tail of a
    /// beat-matched mix plays on after the swap), else the end of the music.
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

/// What the two tracks sound like next to each other, from the stored overlap windows.
#[derive(Clone, Copy)]
struct Verdict {
    /// Two vocals, or two confident keys a tritone or more apart: do not blend.
    clash: bool,
    cause: &'static str,
    /// A large loudness gap or far-apart brightness: keep any overlap short.
    shorten: bool,
    short_cause: &'static str,
}

/// Confident keys this far apart on the Camelot wheel do not blend.
const KEY_FAR: i32 = 4;
/// Mean voice-band share that counts as "sung" in an overlap window.
pub(super) const VOCAL_MIN: f32 = 0.45;
/// Loudness gap that shortens an overlap, dB.
const LOUD_GAP_DB: f32 = 6.0;
/// Brightness ratio (octaves of centroid) that counts as a timbre mismatch.
const TIMBRE_OCTAVES: f64 = 1.0;

const VOCALS_OVERLAP: &str = "vocals overlap";
/// How far the incoming song's voice band is held down while the outgoing song still sings: 18 dB at 1 kHz, the
/// middle of the band, and about 6 dB at its edges (500 Hz, 2 kHz) - under the voice already there, not gone.
const VOCAL_DUCK_DB: f32 = -18.0;
const VOCAL_DUCK_HZ: f32 = 1_000.0;
/// The high-pass that takes the outgoing voice's body away after the swap, over the first half of what is left of
/// the mix: from below a voice to above its first formants, so what is left of it is breath and consonants under
/// the incoming singer while it falls away.
const VOCAL_HP_FROM_HZ: f32 = 200.0;
pub(super) const VOCAL_HP_TO_HZ: f32 = 2_000.0;

/// Two voices over a plain fade: the incoming one is held down through the first half, while the outgoing song
/// is still the louder, and released as the outgoing one falls away (its low-pass, when there is one, takes the
/// rest of its voice with it).
fn separate_in_fade(p: &mut TransitionPlan, s: &AutoMixSettings, sung_both: bool) {
    if !sung_both || !s.filter_effects || p.duration_ms < 2 * MIN_FADE_MS {
        return;
    }
    p.vocal_duck = Some(VocalDuck { until_ms: p.duration_ms / 2, release_ms: (p.duration_ms / 4).min(500), db: VOCAL_DUCK_DB, hz: VOCAL_DUCK_HZ });
    p.reason += ", voices kept apart";
}

fn pair_gate(a: &TrackAnalysis, b: &TrackAnalysis) -> Verdict {
    let mut v = Verdict { clash: false, cause: "", shorten: false, short_cause: "" };
    let keys_known = a.key_confidence >= 0.4 && b.key_confidence >= 0.4;
    if keys_known && key_distance(a.key, b.key) >= KEY_FAR {
        v.clash = true;
        v.cause = "keys far apart";
    } else if a.outro_vocal >= VOCAL_MIN && b.intro_vocal >= VOCAL_MIN {
        // A v1 row reads back as 0 and can never trip this; only a measured window can.
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

/// A clashing pair does not blend: the outgoing track exits into a beat-synced echo while the
/// incoming track fades in over its tail. Four outgoing beats, confined to the overlap the mixer
/// renders (the repeats decay inside it), starting on a downbeat. No tempo change, no bass swap:
/// the echo is the effect.
fn echo_out(a: &TrackAnalysis, b: &TrackAnalysis, out_dur: i64, in_dur: i64, max_len: i64, s: &AutoMixSettings, cause: &str) -> Option<TransitionPlan> {
    let beat = 60_000.0 / a.bpm;
    if !beat.is_finite() || beat <= 0.0 {
        return None;
    }
    // Slow tempos get a clamped delay rather than a cavern (the mixer caps at one second).
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
    // Outgoing dies in two beats; incoming rides the last three so the room is not empty
    // while the repeats decay (DJ.Studio-style echo-out into the next track).
    (p.out_fade_start_ms, p.out_fade_end_ms) = (0, (2 * beat_ms).min(dur));
    (p.in_fade_start_ms, p.in_fade_end_ms) = ((dur - 3 * beat_ms).max(0), dur);
    p.echo = Some(Echo { delay_ms: delay, feedback: 0.45, wet_db: -7.0 });
    p.in_gain_db = loudness_trim(Some(a), Some(b), s);
    p.reason = format!("echo-out over {beats} beats, {cause}");
    Some(p)
}

/// Camelot distance when both keys are trusted, else -1.
fn camelot_dist(a: &TrackAnalysis, b: &TrackAnalysis) -> i32 {
    if a.key_confidence >= 0.4 && b.key_confidence >= 0.4 {
        key_distance(a.key, b.key)
    } else {
        -1
    }
}

/// Low-pass shaping for MixRamp / one-grid fades: soft when keys agree, classic when they do not.
fn apply_fade_filter(p: &mut TransitionPlan, a: Option<&TrackAnalysis>, b: Option<&TrackAnalysis>, s: &AutoMixSettings, dur: i64) {
    if !s.filter_effects || dur < 2000 {
        return;
    }
    let soft = a.zip(b).is_some_and(|(a, b)| {
        let d = camelot_dist(a, b);
        d == 0 || d == 1
    });
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
    // The gates only demote: a clash reroutes to an echo-out (or shortens when the echo cannot
    // run), a loudness gap or timbre mismatch caps an overlap at 8 bars. Never an upgrade.
    let verdict = a.zip(b).map(|(a, b)| pair_gate(a, b));
    // Why this pair is shortened, for the reason line; empty when it is not.
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
            // Two singers are kept apart by the filters of a beat-matched mix before anything is rerouted; far
            // keys cannot be filtered apart, so they echo out first.
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
    // One grid survived (or the lock failed on two good ones): align to what exists rather than
    // fading blind. See one_grid.
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

/// Two grids locked together: everything a beat-matched window is built from.
struct Lock {
    ratio: f64,
    pct: f64,
    bpm_a: f64,
    /// The incoming tempo folded to the outgoing one's octave.
    b_bpm: f64,
    bpb: i64,
    /// Outgoing beat and bar, ms: also the incoming ones in wall time once it is stretched.
    beat: f64,
    bar: f64,
    dist: i32,
    mild_clash: bool,
    max_bars: i64,
    /// The filters may keep two voices apart (the user's filter switch).
    separate: bool,
}

/// What the incoming song's swap lands on, best first.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Landing {
    /// The arrangement arriving (`drop_ms`).
    Drop,
    /// The end of the intro, where it is not the drop.
    IntroEnd,
    /// A four-bar line from the first downbeat.
    Phrase,
    /// The first downbeat itself: no run-up, the song comes in whole.
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

/// One way through: the outgoing song from `start` (its time) for `dur`, the incoming one from `in_start` (its
/// time), the bass changing hands `swap` into it (on `landing` of the incoming song and a downbeat of the outgoing
/// one). All ms.
#[derive(Clone, Copy, Debug)]
struct Window {
    start: f64,
    dur: f64,
    swap: f64,
    in_start: f64,
    runup_bars: i64,
    landing: Landing,
    on_phrase: bool,
    /// The outgoing slice read round, ms; 0 when its own bars are played once.
    out_loop: f64,
    /// Voice-band share of the incoming song over the run-up and after the swap.
    runup_vocal: f32,
    after_vocal: f32,
    score: f64,
}

/// Per second of music skipped: of an instrumental intro or outro, which a listener does not know was there, and
/// of a sung one, which they do. And per second of the incoming intro left on its own after the mix - the energy
/// hole of mixing out of a full song into an intro.
const SKIP_COST: f64 = 0.06;
const SKIP_SUNG_COST: f64 = 0.15;
const DIP_COST: f64 = 0.1;
/// Per bar of run-up laid under the outgoing song (up to eight), and per bar of mix.
const RUNUP_WORTH: f64 = 0.1;
const LENGTH_WORTH: f64 = 0.02;
/// Landing the swap on a four-bar line of the outgoing song's sections.
const PHRASE_WORTH: f64 = 0.5;
/// Per second of sung run-up under a sung ending.
const SUNG_RUNUP_COST: f64 = 0.1;
/// Skipping over the incoming drop altogether.
const DROP_SKIP_COST: f64 = 2.0;
/// Per bar of a looped outgoing slice heard again.
const LOOP_COST: f64 = 0.1;
/// Per second of the outgoing song's silence under the run-up or heard alone before the mix.
const DEAD_AIR_COST: f64 = 0.2;
/// Landing the swap where the outgoing song's worthwhile music ends (a breakdown, a hidden track's gap).
const EXIT_WORTH: f64 = 0.5;
/// A run-up of whole four-bar phrases.
const PHRASE_START_WORTH: f64 = 0.3;
/// A run-up this far below the song's chord energy has no chords to clash.
const CHORDLESS_DB: f32 = 3.0;

/// Where to enter the incoming song and where to hand over, from what both songs are: the enter-on-the-drop
/// search. Every candidate landing of the incoming song (its drop, the end of its intro, its four-bar lines, its
/// first downbeat) is tried against every downbeat of the outgoing song's last sixteen bars, with run-ups of
/// sixteen bars down to none and a tail of a beat up to four bars after the swap. The swap is where the incoming
/// song's landing meets the outgoing downbeat, so the drop and the bass change hands together and both songs turn
/// a section on the same bar. Windows that skip more music than the cap allows, or do not fit, are never
/// considered; of the rest the one that skips least, leaves the incoming intro least on its own after the mix,
/// lays the longest run-up under the outgoing song and lands on the best point wins.
fn drop_aligned(a: &TrackAnalysis, b: &TrackAnalysis, out_dur: i64, in_dur: i64, max_len: i64, bpm_b: f64, k: &Lock) -> Option<Window> {
    let (beat, bar, ratio) = (k.beat, k.bar, k.ratio);
    let ending = Ending::of(a, out_dur);
    // The outgoing downbeats a swap can land on: the sixteen bars before the end of the music and, when the song
    // has an exit short of that (a closing breakdown, a hidden track's gap), the sixteen before the exit.
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
    // The incoming song on its own (native) grid.
    let beat_n = 60_000.0 / bpm_b;
    let bar_in = k.bar * ratio;
    let phase_b = (b.downbeat_phase as i64).rem_euclid(k.bpb);
    let mut nb = (((b.silence_start_ms as f64 - b.beat_offset_ms) / beat_n) - 0.25).ceil() as i64;
    while nb.rem_euclid(k.bpb) != phase_b {
        nb += 1;
    }
    let first = b.beat_offset_ms + nb as f64 * beat_n;
    // A cue from the analysis, on the nearest incoming downbeat.
    let snap = |t: f64| -> f64 {
        let m = ((t - first) / (k.bpb as f64 * beat_n)).round();
        let d = first + m * k.bpb as f64 * beat_n;
        if (d - t).abs() <= 0.5 * beat_n { d } else { t }
    };
    let drop = (b.drop_ms > 0).then(|| snap(b.drop_ms as f64));
    let intro_end = (b.intro_end_ms as f64 > first + 0.5 * bar_in).then(|| snap(b.intro_end_ms as f64));
    // Where the arrangement arrives, for how long the intro would be left on its own after the mix.
    let arrival = drop.or(intro_end);
    // Each landing with whether the incoming song sings in its run-up and after it. Only the drop has both measured;
    // elsewhere the intro's share stands for the run-up, and either share for what follows.
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
                // The outgoing song under the run-up: its own bars before the swap, or, when it has too few of
                // them and they are not sung, its last four or eight bars read round (a loop of a sung line is
                // the "broken record" Apple's AutoMix is criticised for).
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
                // Over a run-up without chords a key cannot clash (and a drum intro is meant to sit under something
                // else): the pair's caps on length then hold for the part where both songs are whole, after the
                // swap, and the run-up may be as long as a harmonic pair's.
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
                    // Whole phrases: the mix then starts on a phrase line of both songs, as a DJ starts the
                    // incoming track on the first beat of a phrase.
                    if runup > 0 && runup % 4 == 0 {
                        score += PHRASE_START_WORTH;
                    }
                    if on_phrase {
                        score += PHRASE_WORTH;
                    }
                    // A sung run-up under a sung ending: two voices, which the vocal duck halves.
                    if sung_end && runup_vocal >= VOCAL_MIN {
                        score -= if k.separate { 0.5 } else { 1.0 } * SUNG_RUNUP_COST * runup as f64 * bar / 1000.0;
                    }
                    if out_loop > 0.0 {
                        score -= LOOP_COST * (dur - out_loop) / bar;
                    }
                    // The outgoing song's dead ending: its silence (a gap, or after its music) under the run-up or
                    // heard on its own before the mix, and its closing breakdown there - the energy hole the exit
                    // is for. Landing the swap on the exit is what leaving there means.
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
    // A waltz does not lock to a four-beat song: every other bar would start on the wrong beat.
    let bpb = bar_beats(a);
    if bar_beats(b) != bpb {
        return Err(format!("metres differ ({}/4 against {}/4)", bpb, bar_beats(b)));
    }
    let beat_a = 60_000.0 / bpm_a;
    let bar = bpb as f64 * beat_a;
    let b_bpm = fold(bpm_a, bpm_b);
    // Camelot: ≤1 = harmonic (long natural blend), 2 = workable but short, >2 = mild clash.
    // DJ.Studio Harmonize and Apple both lengthen compatible pairs and shorten the rest.
    let dist = camelot_dist(a, b);
    let mild_clash = dist > 2;
    let max_bars = if mild_clash || !short_cause.is_empty() || dist == 2 { 8 } else { 16 };
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

/// How long the bass takes to change hands: the last sixteenth before the swap, so the incoming song's first
/// downbeat after it lands with all its low end and the outgoing one's last sixteenth has already let go.
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
    // The incoming song rises over its run-up (or comes in at once without one); the outgoing one falls after the
    // swap, over whatever tail it has.
    (p.in_fade_start_ms, p.in_fade_end_ms) = (0, if swap > 0 { swap } else { (beat_ms / 8).clamp(1, dur_ms.max(1)) });
    (p.out_fade_start_ms, p.out_fade_end_ms) = (swap.min(dur_ms - 1).max(0), dur_ms);
    if s.bass_swap {
        let len = swap_len(k.beat).round() as i64;
        p.bass_swap = Some(BassSwap { at_ms: (swap - len).max(0), len_ms: len, cut_hz: BASS_CUT_HZ });
    }
    // Harmonic pairs: skip or soften the LPF (iOS 27 moved off the predictable underwater dump).
    // Stretched keys: DJ filter-open (HPF). Farther / unknown: classic LPF after the bass hand-over.
    if s.filter_effects {
        match k.dist {
            0 => {}
            1 => {
                p.low_pass = Some(low_pass(((swap + dur_ms) / 2).max(swap), dur_ms, SWEEP_TO_HZ_SOFT));
            }
            2 => {
                p.high_pass = Some(Sweep { start_ms: 0, end_ms: swap.max(beat_ms * 4).min(dur_ms), from_hz: 40.0, to_hz: 1_200.0 });
            }
            _ => {
                p.low_pass = Some(low_pass(swap, dur_ms, SWEEP_TO_HZ_MATCHED));
            }
        }
    }
    // Two singers: while the outgoing song leads, the incoming voice band is held down, released over the beat
    // before the swap; after it, the outgoing voice is thinned by a rising high-pass while it falls away. Either
    // half only where both sing then.
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
    } else if k.dist == 2 {
        bits.push("keys stretch: filter-open".to_string());
    } else if !short_cause.is_empty() {
        bits.push(format!("short ({short_cause})"));
    } else if k.dist == 0 {
        bits.push("same key".to_string());
    } else if k.dist == 1 {
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

/// One side has a usable grid and the two could not be locked (the other grid is unreliable, the
/// tempos are too far apart, or no shared window fits): align to the grid that exists. An exit that
/// starts on the outgoing track's downbeats, or an entrance that lands on the incoming track's,
/// still sounds intentional where a blind fade sounds accidental. No tempo change: with only one
/// tempo known there is nothing to match to, so this is a well-placed fade, not a mix.
fn one_grid(a: Option<&TrackAnalysis>, b: Option<&TrackAnalysis>, out_dur: i64, in_dur: i64, max_len: i64, s: &AutoMixSettings, short: bool, why_not: &str) -> Option<TransitionPlan> {
    let tail = if why_not.is_empty() { String::new() } else { format!(" ({why_not})") };
    let ending = a.map(|x| Ending::of(x, out_dur));
    let end_a = ending.map_or(out_dur, |e| e.leave(0.0).round() as i64);
    let skipped = |heard_end: i64| ending.map_or((out_dur - heard_end) as f64, |e| e.skipped(heard_end as f64));
    // The outgoing grid carries the exit: 8 bars of it, then 4 (4 only when the gates shortened it).
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
    // Else the incoming grid carries the entrance: its first downbeat at or after its music starts.
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
    // How long the outgoing track is already quiet at its end, and the incoming one still quiet at its start. An
    // exit short of the end has no quiet stretch before it (the ramp measured is the real end's).
    let tail = a.map(|a| if a.mixramp_end_ms > 0 && a.mixramp_end_ms <= end_a { end_a - a.mixramp_end_ms } else { 0 });
    let head = b.map(|b| if b.mixramp_start_ms > 0 { (b.mixramp_start_ms - in_start).max(0) } else { 0 });
    let mut dur = tail.unwrap_or(0) + head.unwrap_or(0);
    if tail.is_none() || head.is_none() {
        // One side is unknown: give it an ordinary fade.
        dur = dur.max(max_len.min(4000));
    }
    let dur = dur.max(MIN_MIXRAMP_MS).clamp(MIN_FADE_MS.min(max_len), max_len);
    let mut p = blank(TransitionKind::MixRampFade, end_a - dur, in_start, dur, String::new());
    // A loud start comes in at once rather than being faded up; a quiet one rides its own ramp.
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

    /// A 4-minute, 128 BPM track, 4/4 from 0, with an 8-bar intro, a 16-bar outro and trimmed silence at both ends.
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

    /// The hard rule: at most `MAX_SKIP_MS` of either song's music goes unplayed; silence is free.
    fn check_skip(p: &TransitionPlan, a: &TrackAnalysis, b: &TrackAnalysis, out_dur: i64) {
        assert!(p.in_start_ms - b.silence_start_ms <= MAX_SKIP_MS, "{p:?}");
        // Outro remix: only the captured loop must stay inside the track; duration may wrap past it.
        let heard_end = p.out_start_ms + p.out_loop_ms.unwrap_or(p.duration_ms);
        assert!(Ending::of(a, out_dur).skipped(heard_end as f64) <= MAX_SKIP_MS as f64, "{p:?}");
        assert!(p.out_start_ms >= 0 && p.duration_ms >= 0 && heard_end <= out_dur, "{p:?}");
    }

    /// The same without analyses: nothing of either end is known to be silence.
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
    fn nothing_known_is_a_fixed_equal_power_fade() {
        let s = AutoMixSettings { max_transition_s: 8.0, ..Default::default() };
        let p = plan(None, None, 200_000, 180_000, &s);
        assert_eq!(p.kind, TransitionKind::EqualPowerFade);
        assert_eq!(p.fade_curve, FadeCurve::EqualPower);
        assert_eq!((p.out_start_ms, p.in_start_ms, p.duration_ms), (192_000, 0, 8000));
        assert_eq!((p.bass_swap, p.low_pass, p.tempo_ratio), (None, None, 1.0));
        check_skip_blind(&p, 200_000);
        // The blind fade is capped, and never longer than a third of a short track.
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
    fn confident_grids_beat_match_on_bars_with_a_bass_swap() {
        let s = AutoMixSettings::default();
        let (a, b) = (track(128.0), track(124.0));
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        assert!((p.tempo_ratio - 128.0 / 124.0).abs() < 1e-9);
        assert!(p.keep_pitch);
        assert_eq!(p.tempo_ramp_beats, 32, "a 3 % change ramps back over 8 bars");
        let (beat, bar) = (60_000.0 / 128.0, 4.0 * 60_000.0 / 128.0);
        // The incoming intro is eight bars: all of it is laid under the outgoing song, from its first downbeat, and
        // its end - where the incoming song arrives - is where the bass changes hands. A beat of the outgoing song
        // is left to fall away after it: 16 s holds eight bars and a beat of 128 BPM.
        assert_eq!(p.in_start_ms, 120);
        assert!((p.duration_ms as f64 - 8.0 * bar - beat).abs() <= 1.0, "{}: {}", p.duration_ms, p.reason);
        assert!((lands(&p, b.intro_end_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        // The mix starts on a downbeat of the outgoing track and the swap is on one too.
        assert!(on_downbeat(&a, p.out_start_ms as f64) && on_downbeat(&a, (p.out_start_ms + swap_at(&p)) as f64), "{}", p.out_start_ms);
        // The lows change hands over the sixteenth before the swap, which splits the fades.
        assert_eq!(p.bass_swap.unwrap().len_ms, (beat / 4.0).round() as i64);
        assert_eq!((p.in_fade_start_ms, p.in_fade_end_ms), (0, swap_at(&p)));
        assert_eq!((p.out_fade_start_ms, p.out_fade_end_ms), (swap_at(&p), p.duration_ms));
        assert_eq!(p.fade_curve, FadeCurve::SineSquared);
        assert_eq!(p.low_pass, None, "same key: no low-pass");
        assert_eq!(p.in_gain_db, 0.0, "loudness matching is off by default");
        assert!(p.reason.contains("same key") && p.reason.contains("end of the intro"), "{}", p.reason);

        // With room for more, the outgoing song keeps playing under the incoming one after the swap.
        let long = AutoMixSettings { max_transition_s: 40.0, ..Default::default() };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &long);
        assert!(p.duration_ms as f64 > 9.0 * bar && p.duration_ms <= 40_000, "{}", p.reason);
        assert!((lands(&p, b.intro_end_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn enters_on_the_drop() {
        // Four bars of drums, four more of drums and bass, then the arrangement (the drop) eight bars in: the drop,
        // not the end of the intro, is where the bass changes hands.
        let beat: f64 = 60_000.0 / 128.0;
        let b = TrackAnalysis { intro_end_ms: (120.0 + 16.0 * beat) as i64, drop_ms: (120.0 + 32.0 * beat) as i64, ..track(128.0) };
        let a = track(128.0);
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!((lands(&p, b.drop_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        assert!(p.reason.contains("on the drop"), "{}", p.reason);
        assert_eq!(p.in_start_ms, 120, "the whole run-up fits: nothing skipped");
        check_skip(&p, &a, &b, 240_000);

        // Twelve bars in: the run-up a 16 s mix can hold is eight bars, so four bars of intro (7.5 s) are skipped
        // to land on the drop. Twenty bars in, it would skip 22 s: never.
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
        // Longer mixes lay the whole intro under the outgoing song instead.
        let long = AutoMixSettings { max_transition_s: 45.0, ..Default::default() };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &long);
        assert!((lands(&p, b.drop_ms) - swap_at(&p) as f64).abs() <= 2.0, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn a_drum_intro_is_laid_under_any_key() {
        // Two fifths apart (Camelot 2): eight bars at most where both songs play chords. A drop sixteen bars in,
        // reached through a drum intro without chords, still gets its whole run-up; through a pad, it does not.
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
    fn a_run_up_is_whole_phrases_when_it_can_be() {
        // An intro of 16 bars and a 16 s mix: the run-up is four or eight bars (a phrase), never six, so both songs
        // start the mix on a phrase line.
        let beat: f64 = 60_000.0 / 128.0;
        let a = track(128.0);
        let b = TrackAnalysis { drop_ms: (120.0 + 64.0 * beat) as i64, intro_end_ms: (120.0 + 64.0 * beat) as i64, drop_runup_tonal_db: -12.0, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        let runup = swap_at(&p) as f64 / (4.0 * beat);
        assert!((runup - runup.round()).abs() < 0.01 && runup.round() as i64 % 4 == 0, "{runup}: {}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn a_song_that_starts_full_comes_in_on_a_phrase_line() {
        // No intro and no drop: the incoming song comes in on its first downbeat, under the outgoing one, and the
        // bass changes hands four bars on, on a four-bar line of both.
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
    fn harmonic_neighbour_gets_a_soft_filter_and_long_mix() {
        // C major -> G major: Camelot distance 1.
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
    fn stretched_keys_open_the_filter() {
        // C major -> D major: Camelot distance 2 (two fifths): eight bars at most.
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
    fn a_short_instrumental_outro_loops_and_a_sung_one_never_does() {
        // Five bars of music at the end, and an incoming intro of eight: the eight-bar run-up reads the outgoing
        // song's last four bars round rather than skip half the incoming intro.
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
        // Sung: the run-up is what the four bars hold, played once.
        let a = TrackAnalysis { outro_vocal: 0.7, ..a };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert_eq!(p.out_loop_ms, None, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn tag_bpm_settles_a_half_double() {
        // Analysis reported half tempo; the server tag is the true 128.
        let a = TrackAnalysis { bpm: 64.0, ..track(128.0) };
        let b = track(128.0);
        let s = AutoMixSettings { out_tag_bpm: 128.0, max_transition_s: 40.0, ..Default::default() };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!((p.tempo_ratio - 1.0).abs() < 0.01, "folded to tag: ratio {}", p.tempo_ratio);
    }

    #[test]
    fn half_and_double_tempo_still_match() {
        let p = plan(Some(&track(174.0)), Some(&track(88.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!((p.tempo_ratio - 174.0 / 176.0).abs() < 1e-9);
        assert_eq!(p.tempo_ramp_beats, 16);
    }

    #[test]
    fn tempo_gaps_and_varispeed_limits_fall_back_to_a_mixramp_fade() {
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
    fn a_band_that_drifts_is_matched_on_its_steady_ends() {
        // Played without a click: one grid over the whole song misses its last beats (no stability),
        // but its last and first forty seconds each hold a steady beat. That is a beat-matched mix.
        let a = TrackAnalysis { stability: 0.0, ..track(128.0) };
        let b = TrackAnalysis { stability: 0.0, ..track(126.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
    }

    #[test]
    fn a_song_that_changes_tempo_is_mixed_at_the_tempo_it_ends_on() {
        // 100 BPM for most of the song, 128 at the end: what the next song has to meet is 128.
        let a = TrackAnalysis { bpm: 100.0, ..track(128.0) };
        let p = plan(Some(&a), Some(&track(128.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert_eq!(p.tempo_ratio, 1.0, "the ends already agree: {}", p.reason);
    }

    #[test]
    fn unreliable_grids_are_not_beat_matched() {
        let a = track(128.0);
        // Unreliable over the whole song and over its intro alike.
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
    fn one_good_grid_aligns_the_fade_to_it() {
        // Outgoing grid known, incoming not: the exit starts on the outgoing track's downbeats.
        let (a, b) = (track(128.0), TrackAnalysis { bpm_confidence: 0.2, intro_bpm_confidence: 0.2, ..track(128.0) });
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::MixRampFade);
        assert!(p.reason.contains("downbeat-aligned") && p.reason.contains("bars out"), "{}", p.reason);
        check_skip(&p, &a, &b, 240_000);
        // 8 bars of 128 BPM from a downbeat of the outgoing track.
        let beat = 60_000.0 / 128.0;
        assert!((p.duration_ms as f64 - 8.0 * 4.0 * beat).abs() <= 1.0, "{}", p.duration_ms);
        let beats = (p.out_start_ms as f64 - 120.0) / beat;
        assert!((beats - beats.round()).abs() < 0.01 && (beats.round() as i64) % 4 == 0, "{beats}");

        // Incoming grid known, outgoing not: the entrance lands on the incoming track's downbeats.
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
        // 4 s of quiet tail plus 2 s of quiet head.
        assert_eq!((p.out_start_ms, p.in_start_ms, p.duration_ms), (224_000, 1_000, 6_000));
        assert_eq!(p.in_fade_end_ms, 2_000, "the incoming fade follows its own ramp");
        assert_eq!(p.low_pass.map(|f| (f.start_ms, f.to_hz)), Some((3_000, SWEEP_TO_HZ_SOFT)), "same key: soft low-pass in the second half");
        assert_eq!(p.in_gain_db, 6.0, "incoming 6 dB quieter gets 6 dB");
        check_skip(&p, &a, &b, 240_000);

        // An abrupt end into a loud start: still a mix you can hear - the last five seconds shared, the
        // outgoing song fading under the incoming one, which is at full level almost at once. It used to
        // be a 0.3 s click guard, which sounds like no mix at all.
        let a = TrackAnalysis { bpm: 0.0, outro_bpm: 0.0, silence_end_ms: 240_000, mixramp_end_ms: 240_000, ..track(128.0) };
        let b = TrackAnalysis { bpm: 0.0, intro_bpm: 0.0, silence_start_ms: 0, mixramp_start_ms: 0, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &s);
        assert_eq!((p.duration_ms, p.in_fade_end_ms, p.out_start_ms), (MIN_MIXRAMP_MS, MIN_FADE_MS, 240_000 - MIN_MIXRAMP_MS));

        // Only one side analysed: its points, and an ordinary fade length - no shorter than any MixRamp.
        let p = plan(Some(&a), None, 240_000, 240_000, &s);
        assert_eq!(p.kind, TransitionKind::MixRampFade);
        assert_eq!(p.duration_ms, MIN_MIXRAMP_MS);
        assert_eq!(p.in_start_ms, 0);
    }

    #[test]
    fn silence_is_free_and_never_mixed_over() {
        // 40 s of silence after the music, and a 25 s silent lead-in on the next track: the mix is laid over the
        // music at both ends, never over the silence, which is skipped whole.
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
            // What the incoming song rises under is music; the outgoing song's last beats may fall away into its
            // own silence after the swap.
            let swap = if p.bass_swap.is_some() { swap_at(&p) } else { p.duration_ms / 2 };
            assert!(p.out_start_ms + swap <= 200_000 + 500, "{}: {p:?}", p.reason);
            assert!(p.in_start_ms >= 25_000 - 500, "{}: {p:?}", p.reason);
        }
    }

    #[test]
    fn a_closing_breakdown_is_left_on_the_swap() {
        // The last six bars are a pad coda: the incoming song's arrival lands where the coda begins, the coda falls
        // away under it, and what is left of it (under the cap) is not played.
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
        // A coda too long to leave (the music after it over the cap even with four bars of tail) is played.
        let exit = (120.0 + bar * (((238_500.0 - 120.0) / bar).floor() - 14.0)) as i64;
        let a = TrackAnalysis { exit_ms: exit, ..track(128.0) };
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        check_skip(&p, &a, &b, 240_000);
        assert!(p.out_start_ms + swap_at(&p) > exit, "{}", p.reason);
    }

    #[test]
    fn a_hidden_track_is_left_at_its_gap_when_it_is_short() {
        // The song ends at 180 s, 40 s of silence, a 10 s hidden track: the mix happens at 180 s and the hidden
        // track (10 s of music) is left out. The silence costs nothing against the cap.
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
        // Thirty seconds of hidden music is too much to leave: it is played, and the gap with it.
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
        // With the echo off a clashing pair gets a short beat-matched mix instead.
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, echo_out: false, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.reason);
        assert!(p.reason.contains("clash"));
    }

    #[test]
    fn overlapping_vocals_are_kept_apart_and_gaps_shorten() {
        // Both sing over the whole overlap: a short beat-matched mix whose filters keep the voices apart - the
        // incoming voice band held down until the swap, the outgoing voice thinned by a rising high-pass after it.
        let sung = |t: TrackAnalysis| TrackAnalysis { outro_vocal: 0.7, intro_vocal: 0.7, exit_vocal: 0.7, ..t };
        let (a, b) = (sung(track(128.0)), sung(track(128.0)));
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!(p.reason.contains("voices kept apart") && p.reason.contains("vocals overlap"), "{}", p.reason);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.reason);
        let swap = swap_at(&p);
        if swap > 0 {
            let d = p.vocal_duck.expect(&p.reason);
            assert_eq!((d.until_ms, d.db), (swap, VOCAL_DUCK_DB), "{}", p.reason);
            assert!(d.release_ms > 0 && d.release_ms <= swap);
        }
        if p.duration_ms - swap >= (60_000.0f64 / 128.0) as i64 {
            let h = p.high_pass.expect(&p.reason);
            assert_eq!((h.start_ms, h.end_ms, h.to_hz), (swap, swap + (p.duration_ms - swap) / 2, VOCAL_HP_TO_HZ), "{}", p.reason);
        }
        check_skip(&p, &a, &b, 240_000);
        // Without the filters there is nothing to keep them apart with: the echo-out, as before.
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings { filter_effects: false, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::EchoOut, "{}", p.reason);
        assert!(p.reason.contains("vocals overlap"), "{}", p.reason);
        // Only one side sings: no separation.
        let p = plan(Some(&track(128.0)), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!((p.vocal_duck, p.high_pass), (None, None), "{}", p.reason);
        // Without a grid, a fade: the incoming voice is held down through its first half.
        let (a, b) = (TrackAnalysis { bpm: 0.0, outro_bpm: 0.0, ..a }, TrackAnalysis { bpm: 0.0, intro_bpm: 0.0, ..b });
        let p = plan(Some(&a), Some(&b), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::MixRampFade, "{}", p.reason);
        assert_eq!(p.vocal_duck.map(|d| d.until_ms), Some(p.duration_ms / 2), "{}", p.reason);
        // Quiet outro into a far louder intro: still beat-matched, but short.
        let b = TrackAnalysis { lufs: -2.0, ..track(128.0) };
        let p = plan(Some(&track(128.0)), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.reason);
        // Same for a timbre mismatch: a dark outro into a bright intro.
        let b = TrackAnalysis { intro_centroid: 5000.0, ..track(128.0) };
        let p = plan(Some(&track(128.0)), Some(&b), 240_000, 240_000, &AutoMixSettings { max_transition_s: 40.0, ..Default::default() });
        assert_eq!(p.kind, TransitionKind::BeatMatched);
        assert!(p.duration_ms as f64 <= 8.25 * 4.0 * 60_000.0 / 128.0 + 1.0, "{}", p.reason);
    }

    #[test]
    fn a_stale_analysis_of_a_different_file_is_ignored() {
        let a = TrackAnalysis { duration_ms: 300_000, ..track(128.0) };
        let p = plan(Some(&a), None, 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::EqualPowerFade);
    }

    #[test]
    fn a_waltz_mixes_in_bars_of_three_and_never_into_four() {
        let waltz = |bpm| TrackAnalysis { beats_per_bar: 3, ..track(bpm) };
        let p = plan(Some(&waltz(150.0)), Some(&waltz(150.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_eq!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        let bar = 3.0 * 60_000.0 / 150.0;
        let bars = p.duration_ms as f64 / bar;
        assert!((bars - bars.round()).abs() < 0.01, "{} ms is not whole bars of three", p.duration_ms);
        // The outgoing exit starts on one of its downbeats: grid beat n with n % 3 == phase.
        let n = (p.out_start_ms as f64 - 120.0) / (60_000.0 / 150.0);
        assert!((n - n.round()).abs() < 0.01 && (n.round() as i64).rem_euclid(3) == 0, "{}", p.out_start_ms);

        let p = plan(Some(&waltz(150.0)), Some(&track(150.0)), 240_000, 240_000, &AutoMixSettings::default());
        assert_ne!(p.kind, TransitionKind::BeatMatched, "{}", p.reason);
        assert!(p.reason.contains("metres differ"), "{}", p.reason);
        // A row from before metres were measured reads as 4/4.
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
