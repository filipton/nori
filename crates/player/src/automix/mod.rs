//! AutoMix: DJ-style transitions (see docs/research/automix.md).
//!
//! - `analysis` + `tempo` + `structure` + `loudness`: one pass over a track's PCM gives a `TrackAnalysis`
//!   (tempo and beat grid, downbeats, phrase cues, the drop, the exit, key, loudness, silence, a hidden track's gap
//!   and MixRamp points), streamed: from a whole song decoded ahead (nori-engine's measurer) or from the PCM the
//!   player already decodes (the transition engine's tap). The `track_analysis` table is nori-automix's.
//! - `beats` (+ `neural` with the `neural-beats` feature): Beat This!, an optional neural beat tracker, run over the
//!   first and last 30 s of a song; its grids replace the classical intro and outro grids where it is sure. Its
//!   network ships as a graph without weights; `weights` fills it from the authors' checkpoint, read by
//!   `checkpoint` (a zip and a restricted unpickler).
//! - `plan`: a pure function from two analyses and the user's settings to a `TransitionPlan`.
//! - `mixer` and `stretch`: per-buffer building blocks that render a plan (gain curves, bass swap, filter
//!   sweeps, vocal duck, echo; time-stretch of the incoming track).
//! - `eval` (tests only): the synthetic songs and pairs the analysis and the planned transitions are scored on.

pub mod analysis;
pub mod beats;
#[cfg(any(test, feature = "neural-beats"))]
pub mod checkpoint;
pub mod loudness;
pub mod mixer;
#[cfg(feature = "neural-beats")]
pub mod neural;
pub mod plan;
pub mod resample;
pub mod stretch;
pub mod structure;
pub mod tempo;
pub mod vocal;
#[cfg(feature = "neural-beats")]
pub mod weights;

#[cfg(any(test, feature = "synth"))]
pub mod synth;
#[cfg(any(test, feature = "synth"))]
pub mod eval;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod plan_fuzz;

use crate::types::TrackAnalysis;
#[cfg(any(test, feature = "synth"))]
use analysis::Analyzer;
use analysis::Features;

/// Bump when the analysis changes enough that stored rows should be redone. 10: the vocal activity curve
/// (`vocal`) is measured with it, so songs measured before get one. 11: the curve is measured in a narrower
/// band with a looser peak test (vocal.rs, tuned on a real library), so curves of 10 are measured again. 12: a
/// stereo song's curve is measured on the middle of its stereo image (vocal.rs); the rest of the analysis is as
/// it was. The curve's stored form did not change (`vocal::CURVE_VERSION`), so a song's old curve is still read
/// until the song is measured again.
pub const ANALYSIS_VERSION: i32 = 12;
/// How much music at each end the intro and outro grids are measured over: long enough for a steady
/// tempo estimate (dozens of beats at any tempo), short enough that a live band's drift inside it is
/// a fraction of a beat.
pub const GRID_WINDOW_S: f64 = 40.0;
/// A silence inside the music at least this long is a gap a mix may leave by (a hidden track's), not a rest.
const LONG_GAP_MS: i64 = 6_000;
/// Below these the grid is not used for cue placement either (cues fall back to the energy envelope).
const CUE_MIN_CONFIDENCE: f32 = 0.4;
const CUE_MIN_STABILITY: f32 = 0.5;

/// Mean of a per-frame `curve` over `[from_s, to_s)`. Too short a window to say anything (under a
/// second of frames) falls back to the whole track rather than to noise.
fn window_mean(curve: &[f32], fps: f64, t0: f64, from_s: f64, to_s: f64) -> f32 {
    if curve.is_empty() || !fps.is_finite() || fps <= 0.0 {
        return 0.0;
    }
    let idx = |t: f64| ((t - t0) * fps).round().max(0.0) as usize;
    let (mut a, mut b) = (idx(from_s).min(curve.len()), idx(to_s).min(curve.len()));
    if b.saturating_sub(a) < fps as usize {
        (a, b) = (0, curve.len());
    }
    if b <= a {
        return 0.0;
    }
    curve[a..b].iter().sum::<f32>() / (b - a) as f32
}

/// One stretch of music's beat grid; all zeros when it could not be measured.
#[derive(Default, Clone, Copy, Debug)]
pub struct Grid {
    pub bpm: f64,
    pub confidence: f32,
    pub offset_ms: f64,
    pub stability: f32,
    pub downbeat_phase: i32,
}

/// The beat grid of `[from_s, to_s)` alone: the same tempo estimate and downbeat search as the whole
/// track, on that stretch of the onset envelope. The first beat is given in track time, so the grid
/// `offset + n * period` lands on the same beats as it does inside the window.
pub fn window_grid(f: &Features, from_s: f64, to_s: f64, meter: i64) -> Grid {
    if !(f.fps > 0.0) || to_s - from_s < GRID_WINDOW_S / 2.0 {
        return Grid::default();
    }
    let a = (((from_s - f.t0) * f.fps).round().max(0.0) as usize).min(f.onset.len());
    let b = (((to_s - f.t0) * f.fps).round().max(0.0) as usize).min(f.onset.len());
    if b <= a {
        return Grid::default();
    }
    let t = tempo::estimate(&f.onset[a..b], f.fps, f.t0 + a as f64 / f.fps);
    if !(t.bpm > 0.0 && t.bpm.is_finite()) {
        return Grid::default();
    }
    let db = structure::downbeat(&t, f, (from_s, to_s), Some(meter));
    Grid { bpm: t.bpm, confidence: t.confidence, offset_ms: t.offset_s * 1000.0, stability: t.stability, downbeat_phase: db.phase }
}

/// Wall-clock time, ms since the epoch: when an analysis was made.
fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// A `TrackAnalysis` plus the working data behind it, for tests and diagnostics.
pub struct Analysis {
    pub track: TrackAnalysis,
    pub tempo: tempo::Tempo,
}

/// Runs the whole-track steps on the features of one track.
pub fn finish(song_id: &str, f: &Features) -> Analysis {
    let duration_ms = (f.duration_s * 1000.0).round() as i64;
    let (s0, s1) = loudness::silence_trim(&f.blocks_raw);
    let (s0, s1) = (s0.min(duration_ms), s1.min(duration_ms));
    let lufs = loudness::integrated(&f.blocks_k);
    let (mr0, mr1) = loudness::mixramp(&f.blocks_k, lufs).map_or((s0, s1), |(a, b)| (a.clamp(s0, s1.max(s0)), b.clamp(s0, s1.max(s0))));
    let silent = s1 <= s0;
    let music = if silent { (0.0, f.duration_s) } else { (s0 as f64 / 1000.0, s1 as f64 / 1000.0) };

    let t = if silent { tempo::Tempo::default() } else { tempo::estimate(&f.onset, f.fps, f.t0) };
    let db = structure::downbeat(&t, f, music, None);
    let meter = if db.beats_per_bar == 3 { 3 } else { 4 };
    let grid_ok = t.bpm > 0.0 && t.confidence >= CUE_MIN_CONFIDENCE && t.stability >= CUE_MIN_STABILITY;
    // A hidden track short enough to leave, after a long silence: the song proper ends at the silence, and its
    // outro, the grid a mix locks to and the voices over its end are measured there, not on the hidden track.
    // (The music after it counts against the skip cap; the silence does not.)
    let gap = if silent { None } else { loudness::last_gap(&f.blocks_raw, LONG_GAP_MS) };
    let leave = gap.filter(|(_, g1)| s1 - g1 <= plan::MAX_SKIP_MS);
    let song = leave.map_or(music, |(g0, _)| (music.0, g0 as f64 / 1000.0));
    let (intro, outro) = if silent { (0.0, 0.0) } else { structure::cues(&t, &db, f, song, grid_ok) };
    let (key, key_confidence) = if silent { (0, 0.0) } else { structure::key_of(structure::tuned_profile(f)) };
    // What the overlap windows sound like: vocal share and brightness of the outgoing outro and the
    // incoming intro, for the pair gates in `plan`. Silence has neither.
    let (outro_vocal, outro_centroid, intro_vocal, intro_centroid) = if silent {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (
            window_mean(&f.vocal, f.fps, f.t0, outro, song.1),
            window_mean(&f.centroid, f.fps, f.t0, outro, song.1),
            window_mean(&f.vocal, f.fps, f.t0, music.0, intro),
            window_mean(&f.centroid, f.fps, f.t0, music.0, intro),
        )
    };

    // Where the arrangement arrives, and whether the run-up to it and what follows it are sung.
    let bar_s = if t.bpm > 0.0 { 60.0 / t.bpm * meter as f64 } else { 0.0 };
    let found = if silent || !grid_ok { None } else { structure::drop_point(&t, &db, f, music) };
    let drop = found.map(|d| d.at);
    let (drop_runup_vocal, drop_vocal) = drop.map_or((0.0, 0.0), |d| {
        (
            window_mean(&f.vocal, f.fps, f.t0, (d - 8.0 * bar_s).max(music.0), d),
            window_mean(&f.vocal, f.fps, f.t0, d, (d + 8.0 * bar_s).min(music.1)),
        )
    });

    // Where the ending stops being worth playing: a closing breakdown, or the gap before a hidden track.
    let exit = if silent { None } else { structure::breakdown(&t, &db, f, music, song.1, grid_ok) }.or(leave.map(|(g0, _)| g0 as f64 / 1000.0));
    let last = exit.unwrap_or(music.1);
    let exit_vocal = if silent { 0.0 } else { window_mean(&f.vocal, f.fps, f.t0, (last - if bar_s > 0.0 { 8.0 * bar_s } else { 16.0 }).max(music.0), last) };

    let (outro_grid, intro_grid) = if silent {
        (Grid::default(), Grid::default())
    } else {
        (
            window_grid(f, (song.1 - GRID_WINDOW_S).max(music.0), song.1, meter),
            window_grid(f, music.0, (music.0 + GRID_WINDOW_S).min(music.1), meter),
        )
    };

    let track = TrackAnalysis {
        song_id: song_id.to_string(),
        analysis_version: ANALYSIS_VERSION,
        duration_ms,
        bpm: if t.bpm.is_finite() { t.bpm } else { 0.0 },
        bpm_confidence: t.confidence,
        beat_offset_ms: t.offset_s * 1000.0,
        stability: t.stability,
        downbeat_phase: db.phase,
        downbeat_confidence: db.confidence,
        beats_per_bar: if silent || t.bpm <= 0.0 { 0 } else { meter as i32 },
        lufs: lufs as f32,
        key,
        key_confidence,
        silence_start_ms: s0,
        silence_end_ms: s1,
        mixramp_start_ms: mr0,
        mixramp_end_ms: mr1,
        intro_end_ms: (intro * 1000.0).round() as i64,
        outro_start_ms: (outro * 1000.0).round() as i64,
        outro_vocal,
        intro_vocal,
        outro_centroid,
        intro_centroid,
        analysed_ms: now_ms(),
        outro_bpm: outro_grid.bpm,
        outro_bpm_confidence: outro_grid.confidence,
        outro_beat_offset_ms: outro_grid.offset_ms,
        outro_stability: outro_grid.stability,
        outro_downbeat_phase: outro_grid.downbeat_phase,
        intro_bpm: intro_grid.bpm,
        intro_bpm_confidence: intro_grid.confidence,
        intro_beat_offset_ms: intro_grid.offset_ms,
        intro_stability: intro_grid.stability,
        intro_downbeat_phase: intro_grid.downbeat_phase,
        drop_ms: drop.map_or(0, |d| (d * 1000.0).round() as i64),
        drop_runup_vocal,
        drop_vocal,
        drop_runup_tonal_db: found.map_or(0.0, |d| d.runup_tonal_db),
        exit_ms: exit.map_or(0, |e| (e * 1000.0).round() as i64),
        gap_ms: gap.map_or(0, |g| g.0),
        gap_end_ms: gap.map_or(0, |g| g.1),
        exit_vocal,
        intro_beats_per_bar: 0,
        outro_beats_per_bar: 0,
        intro_grid_source: beats::GRID_CLASSICAL,
        outro_grid_source: beats::GRID_CLASSICAL,
    };
    Analysis { track, tempo: t }
}

/// Analyses one whole track of mono samples at `sample_rate`.
#[cfg(any(test, feature = "synth"))]
pub fn analyse(song_id: &str, pcm: &[f32], sample_rate: u32) -> Analysis {
    let mut a = Analyzer::new(sample_rate, (pcm.len() as u64 * 1000) / sample_rate.max(1) as u64);
    a.feed(pcm);
    let f = a.take_features();
    finish(song_id, &f)
}

/// `C.ENCODING_PCM_16BIT` and `C.ENCODING_PCM_FLOAT`, as media3 numbers them.
pub const PCM_16: i32 = 2;
pub const PCM_FLOAT: i32 = 4;

/// Analyses interleaved little-endian PCM bytes, 16-bit or float, as a decoder hands them out.
#[cfg(any(test, feature = "synth"))]
pub fn analyse_bytes(song_id: &str, pcm: &[u8], sample_rate: i32, channels: i32, encoding: i32) -> Analysis {
    let ch = channels.clamp(1, 8) as usize;
    let rate = sample_rate.max(1) as u32;
    let width = if encoding == PCM_FLOAT { 4 } else { 2 };
    let frames = pcm.len() / width / ch;
    let mut a = Analyzer::new(rate, frames as u64 * 1000 / rate as u64);
    // Decoded in slices so a whole track never exists as f32 on top of the bytes.
    let chunk = 1024 * ch * width;
    for part in pcm.chunks(chunk) {
        let part = &part[..part.len() / (ch * width) * (ch * width)];
        if encoding == PCM_FLOAT {
            let mut s = [0f32; 1024 * 8];
            for (d, b) in s.iter_mut().zip(part.chunks_exact(4)) {
                *d = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
            a.feed_interleaved(&s[..part.len() / 4], ch, |v| v);
        } else {
            let mut s = [0i16; 1024 * 8];
            for (d, b) in s.iter_mut().zip(part.chunks_exact(2)) {
                *d = i16::from_le_bytes([b[0], b[1]]);
            }
            a.feed_interleaved(&s[..part.len() / 2], ch, |v| v as f32 / 32768.0);
        }
    }
    finish(song_id, &a.take_features())
}

