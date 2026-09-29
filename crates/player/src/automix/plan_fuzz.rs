//! The planner runs on the engine thread over rows read from the database: no row, however corrupt, may make it
//! panic (that silenced playback until restart). Seeded random rows, sensible to absurd.

use crate::automix::plan::plan;
use crate::transitions::engine_plan;
use crate::types::{AutoMixSettings, TrackAnalysis};

/// Seeded xorshift: the same rows every run.
struct R(u64);

impl R {
    fn n(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[(self.n() % xs.len() as u64) as usize]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Rows {
    /// Plausible values and their edges.
    Sensible,
    /// Anywhere in a song and any tempo up to 5000 bpm.
    Anywhere,
    /// NaN, infinities, negatives, absurd tempos.
    Absurd,
}

fn row(r: &mut R, dur: i64, how: Rows) -> TrackAnalysis {
    let ms = |r: &mut R| match how {
        Rows::Anywhere => (r.n() % (dur.max(1) as u64 + 22_000)) as i64 - 2000,
        Rows::Absurd => r.pick(&[0i64, -1, 1, dur / 2, dur - 1, dur, dur + 5000, 30_000, 3_000_000, -30_000, i64::MAX / 4, i64::MIN / 4]),
        Rows::Sensible => r.pick(&[0i64, 1, dur / 2, dur - 1, dur, dur - 30_000, 30_000, 1000, dur - 5000, dur / 10]),
    };
    let f = |r: &mut R| match how {
        Rows::Anywhere => if r.n() % 5 == 0 { 0.0 } else { (r.n() % 5_000_000) as f64 / 1000.0 },
        Rows::Absurd => r.pick(&[0.0f64, 120.0, 60.0, 240.0, 1.0, 0.001, 1e9, -5.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 33.3, 400.0]),
        Rows::Sensible => r.pick(&[0.0f64, 120.0, 60.0, 240.0, 33.3, 400.0, 71.3, 180.0, 12.0, 500.0, 0.5]),
    };
    let c = |r: &mut R| if how == Rows::Absurd { r.pick(&[0.0f32, 1.0, 0.5, 0.9, -1.0, f32::NAN, f32::INFINITY, 2.0]) } else { r.pick(&[0.0f32, 1.0, 0.5, 0.9, 0.1]) };
    let i = |r: &mut R| if how == Rows::Absurd { r.pick(&[0i32, 1, 2, 3, 4, 5, 7, -1, 12, 24, 100, i32::MIN, i32::MAX]) } else { r.pick(&[0i32, 1, 2, 3, 4, 5, 7, 12, 24]) };
    TrackAnalysis {
        song_id: "x".into(),
        analysis_version: 99,
        duration_ms: r.pick(&[dur, 0, dur + 1000, dur / 3, dur - 400]),
        bpm: f(r),
        bpm_confidence: c(r),
        beat_offset_ms: f(r),
        stability: c(r),
        downbeat_phase: i(r),
        downbeat_confidence: c(r),
        beats_per_bar: i(r),
        lufs: r.pick(&[-70.0f32, -14.0, 0.0, -30.0, -40.0]),
        key: i(r),
        key_confidence: c(r),
        silence_start_ms: ms(r),
        silence_end_ms: ms(r),
        mixramp_start_ms: ms(r),
        mixramp_end_ms: ms(r),
        intro_end_ms: ms(r),
        outro_start_ms: ms(r),
        outro_vocal: c(r),
        intro_vocal: c(r),
        outro_centroid: c(r) * 3000.0,
        intro_centroid: c(r) * 3000.0,
        outro_bpm: f(r),
        outro_bpm_confidence: c(r),
        outro_beat_offset_ms: f(r),
        outro_stability: c(r),
        outro_downbeat_phase: i(r),
        intro_bpm: f(r),
        intro_bpm_confidence: c(r),
        intro_beat_offset_ms: f(r),
        intro_stability: c(r),
        intro_downbeat_phase: i(r),
        drop_ms: ms(r),
        drop_runup_vocal: c(r),
        drop_vocal: c(r),
        exit_ms: ms(r),
        gap_ms: ms(r),
        gap_end_ms: ms(r),
        exit_vocal: c(r),
        drop_runup_tonal_db: c(r) * -20.0,
        intro_beats_per_bar: i(r),
        outro_beats_per_bar: i(r),
        intro_grid_source: i(r),
        outro_grid_source: i(r),
        analysed_ms: 0,
    }
}

/// Plans `n` pairs of rows drawn `how` and reports each distinct panic with its first pair.
fn planned(how: Rows, seed: u64, n: usize) {
    let mut r = R(seed);
    let mut panics = std::collections::BTreeMap::new();
    for k in 0..n {
        let durations = [240_000i64, 900_000, 60_000, 31_000, 0, 1_200_000];
        let (d1, d2) = (r.pick(&durations), r.pick(&durations));
        let (a, b) = (row(&mut r, d1, how), row(&mut r, d2, how));
        let s = AutoMixSettings { max_transition_s: r.pick(&[16.0f32, 8.0, 30.0, 1.0]), keep_pitch: r.pick(&[true, false]), beat_match: r.pick(&[true, false]), ..AutoMixSettings::default() };
        let (told1, told2) = (r.pick(&[d1, d1 + 2000, 0]), r.pick(&[d2, d2 - 2000, 0]));
        let made = std::panic::catch_unwind(|| {
            let t = plan(Some(&a), Some(&b), told1, told2, &s);
            engine_plan(&t, "y")
        });
        if let Err(e) = made {
            let what = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            panics.entry(what).or_insert_with(|| format!("pair {k}, lengths {told1}/{told2}: {a:?} into {b:?}"));
        }
    }
    assert!(panics.is_empty(), "the planner panicked: {panics:#?}");
}

#[test]
fn planner_never_panics() {
    for (how, seed, n) in [(Rows::Sensible, 0x9E37_79B9_7F4A_7C15, 50_000), (Rows::Anywhere, 0xABCD_EF12_345, 100_000), (Rows::Absurd, 0x123_4567, 50_000)] {
        planned(how, seed, n);
    }
}
