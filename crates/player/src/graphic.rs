//! The graphic equalizer: fixed ISO 266 bands (5, 10, 15 or 31) whose sliders set the response at each
//! band centre.
//!
//! One constant-Q peaking biquad per band, 1.5 band spacings wide (1.25 for five bands), Q prewarped by
//! `w / sin w` so top-octave bells keep their width; a top band past 0.4 × rate is a high shelf. Since
//! neighbouring bells overlap, filter gains are solved rather than copied from the sliders (Välimäki and
//! Liski's accurate cascade graphic EQ, IEEE SPL 2017): the dB response is modelled as `B g` at band
//! centres and midpoints (midpoints weighted [`MIDPOINT_WEIGHT`], target the average of the two
//! sliders), solved by weighted least squares, re-measured at the found gains, then refined by
//! Gauss-Newton on the exact response. Accuracy over random ±12 dB curves: ~0.3 dB at centres and
//! 0.7 dB between (to 16 kHz). Runs when a slider moves, never per buffer.

use crate::dsp::{band_db, Band, CH_BOTH, HIGH_SHELF, PEAKING};

/// The layouts offered, by band count.
pub const LAYOUTS: [usize; 4] = [5, 10, 15, 31];

/// Bands per octave for a layout's band count; `None` for a count that is not a layout.
fn per_octave(count: usize) -> Option<f64> {
    match count {
        5 => Some(0.5),
        10 => Some(1.0),
        15 => Some(1.5),
        31 => Some(3.0),
        _ => None,
    }
}

/// Exact (base 2) centre frequencies of a layout, low to high; empty for a non-layout. [`nominal`]
/// gives the ISO labels.
pub fn centres(count: usize) -> Vec<f64> {
    let (first, step): (i32, f64) = match count {
        5 => (-2, 2.0),
        10 => (-5, 1.0),
        15 => (-8, 2.0 / 3.0),
        31 => (-17, 1.0 / 3.0),
        _ => return Vec::new(),
    };
    (0..count as i32).map(|k| 1000.0 * 2f64.powf((first + k) as f64 * step)).collect()
}

/// A band's label: the ISO 266 preferred number nearest its exact centre.
pub fn nominal(freq: f64) -> f64 {
    const R10: [f64; 11] = [10.0, 12.5, 16.0, 20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0];
    if !(freq > 0.0 && freq.is_finite()) {
        return 0.0;
    }
    let decade = 10f64.powf(freq.log10().floor());
    let m = freq / decade * 10.0; // 10 <= m < 100
    let pick = R10.iter().copied().min_by(|a, b| (a.ln() - m.ln()).abs().total_cmp(&(b.ln() - m.ln()).abs())).unwrap_or(m);
    pick * decade / 10.0
}

/// RBJ Q for a bell `octaves` wide between its half-gain points.
fn q_for(octaves: f64) -> f64 {
    1.0 / (2.0 * (std::f64::consts::LN_2 / 2.0 * octaves).sinh())
}

/// Bell width in band spacings: the widest that still draws a ±12 dB zigzag.
const WIDTH_IN_SPACINGS: f64 = 1.5;
/// Five-band bells are narrower (at 1.5 the zigzag was 3.2 dB off, at 1.25 0.3 dB).
const WIDTH_FIVE: f64 = 1.25;

fn width_for(count: usize) -> f64 {
    if count == 5 { WIDTH_FIVE } else { WIDTH_IN_SPACINGS }
}

/// The dB response of `bands` in cascade at `freq` and `rate`.
pub fn response_db(rate: f64, bands: &[Band], freq: f64) -> f64 {
    bands.iter().map(|b| band_db(rate, b, freq)).sum()
}

fn bell(freq: f64, gain_db: f64, q: f64) -> Band {
    Band { kind: PEAKING, freq, gain_db, q, channel: CH_BOTH }
}

/// Largest filter gain (a ±12 dB zigzag needs about 28 dB).
pub const MAX_FILTER_DB: f64 = 30.0;
/// The gain `B` is first measured at (the paper's prototype gain).
const PROTOTYPE_DB: f64 = 12.0;
/// A midpoint's weight against a centre's 1 in the fit.
const MIDPOINT_WEIGHT: f64 = 0.35;
/// Ridge on the filter gains per dB², raised only when the sliders cannot be drawn within [`MAX_FILTER_DB`].
const RIDGE: f64 = 1e-4;
/// The top-band shelf's Q (steepest without overshoot).
const SHELF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// Gauss-Newton refinement steps after the two solves.
const REFINE: usize = 3;

/// The filters for `sliders` (dB, one per band of a layout, low to high) at `rate`. Bands above 0.47 ×
/// rate are dropped. All-zero sliders, or a count that is not a layout, give no filters.
pub fn design(rate: f64, sliders: &[f64]) -> Vec<Band> {
    let count = sliders.len();
    let Some(per_octave) = per_octave(count) else { return Vec::new() };
    if !(rate > 0.0) {
        return Vec::new();
    }
    let octaves = width_for(count) / per_octave;
    let kept: Vec<(f64, f64)> = centres(count).into_iter().zip(sliders.iter().map(|g| if g.is_finite() { *g } else { 0.0 })).filter(|(f, _)| *f < 0.47 * rate).collect();
    if kept.is_empty() || kept.iter().all(|(_, g)| g.abs() < 0.01) {
        return Vec::new();
    }
    let n = kept.len();
    let freqs: Vec<f64> = kept.iter().map(|k| k.0).collect();
    let shelf_top = n > 1 && freqs[n - 1] > 0.4 * rate;
    let filter = |j: usize, g: f64| {
        if shelf_top && j == n - 1 {
            Band { kind: HIGH_SHELF, freq: (freqs[j] * freqs[j - 1]).sqrt(), gain_db: g, q: SHELF_Q, channel: CH_BOTH }
        } else {
            let w = std::f64::consts::TAU * freqs[j] / rate;
            bell(freqs[j], g, q_for(octaves * w / w.sin()))
        }
    };
    // Fit points: centres and midpoints between neighbours.
    let mut points: Vec<(f64, f64, f64)> = Vec::with_capacity(2 * n); // (freq, target dB, weight)
    for i in 0..n {
        points.push((freqs[i], kept[i].1, 1.0));
        if i + 1 < n {
            points.push(((freqs[i] * freqs[i + 1]).sqrt(), (kept[i].1 + kept[i + 1].1) / 2.0, MIDPOINT_WEIGHT));
        }
    }
    // Column j of B: band j's response per dB at each point, measured at gain `at[j]`.
    let shape = |at: &[f64]| -> Vec<Vec<f64>> {
        points
            .iter()
            .map(|&(f, _, _)| {
                (0..n)
                    .map(|j| {
                        let g = if at[j].abs() < 1.0 { PROTOTYPE_DB } else { at[j] };
                        band_db(rate, &filter(j, g), f) / g
                    })
                    .collect()
            })
            .collect()
    };
    let targets: Vec<f64> = points.iter().map(|p| p.1).collect();
    let b0 = shape(&vec![PROTOTYPE_DB; n]);
    let zero = vec![0.0; n];
    // Undrawable sliders ask for ever larger opposing filters: raise the ridge until they fit rather
    // than clip.
    let mut ridge = RIDGE;
    let mut g = zero.clone();
    for _ in 0..8 {
        g = solve(&b0, &points, &targets, &zero, ridge);
        let b1 = shape(&g);
        g = solve(&b1, &points, &targets, &zero, ridge);
        // Gauss-Newton on the exact response, B as the Jacobian.
        for _ in 0..REFINE {
            let bands: Vec<Band> = (0..n).map(|j| filter(j, g[j])).collect();
            let err: Vec<f64> = points.iter().map(|&(f, t, _)| t - response_db(rate, &bands, f)).collect();
            let step = solve(&b1, &points, &err, &g, ridge);
            for j in 0..n {
                g[j] += step[j];
            }
        }
        if g.iter().all(|x| x.abs() <= MAX_FILTER_DB) {
            break;
        }
        ridge *= 8.0;
    }
    (0..n).map(|j| filter(j, g[j].clamp(-MAX_FILTER_DB, MAX_FILTER_DB))).collect()
}

/// Weighted ridge least squares: the `x` minimising `Σ w (B x - t)² + ridge |x + bias|²` (a step from
/// `bias` keeps the ridge on the gains), by its normal equations.
fn solve(b: &[Vec<f64>], points: &[(f64, f64, f64)], t: &[f64], bias: &[f64], ridge: f64) -> Vec<f64> {
    let n = bias.len();
    let mut a = vec![vec![0.0; n + 1]; n];
    for (m, row) in b.iter().enumerate() {
        let w = points[m].2;
        for i in 0..n {
            for j in 0..n {
                a[i][j] += w * row[i] * row[j];
            }
            a[i][n] += w * row[i] * t[m];
        }
    }
    for (i, r) in a.iter_mut().enumerate() {
        r[i] += ridge;
        r[n] -= ridge * bias[i];
    }
    gauss_jordan(a)
}

/// Solves the augmented system `a` (n rows of n + 1) by Gauss-Jordan with partial pivoting; a singular
/// unknown is 0.
fn gauss_jordan(mut a: Vec<Vec<f64>>) -> Vec<f64> {
    let n = a.len();
    for c in 0..n {
        let p = (c..n).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs())).unwrap_or(c);
        a.swap(c, p);
        let d = a[c][c];
        if d.abs() < 1e-12 {
            continue;
        }
        for r in 0..n {
            if r != c {
                let f = a[r][c] / d;
                if f != 0.0 {
                    for k in c..=n {
                        a[r][k] -= f * a[c][k];
                    }
                }
            }
        }
    }
    (0..n).map(|i| if a[i][i].abs() < 1e-12 { 0.0 } else { a[i][n] / a[i][i] }).collect()
}

/// Sliders for another layout drawing the same curve (interpolated in log frequency, flat past the ends).
pub fn relayout(sliders: &[f64], count: usize) -> Vec<f64> {
    let from = centres(sliders.len());
    let to = centres(count);
    if from.is_empty() {
        return vec![0.0; to.len()];
    }
    to.iter()
        .map(|&f| {
            let i = from.partition_point(|&c| c <= f);
            if i == 0 {
                sliders[0]
            } else if i == from.len() {
                sliders[from.len() - 1]
            } else {
                let x = (f / from[i - 1]).ln() / (from[i] / from[i - 1]).ln();
                sliders[i - 1] + x * (sliders[i] - sliders[i - 1])
            }
        })
        .collect()
}

/// Sliders for a parametric preset: its response at each centre, within ±`limit` dB.
pub fn sliders_for(bands: &[Band], count: usize, limit: f64) -> Vec<f64> {
    centres(count).into_iter().map(|f| (response_db(48_000.0, bands, f) * 10.0).round() / 10.0).map(|g| g.clamp(-limit, limit)).collect()
}

// ---- fitting a headphone correction target ----
//
// A target (AutoEQ's GraphicEQ curve, or a parametric preset's response) on a fixed log grid. Sliders
// are fitted so the response `design` actually plays follows it, least squares in dB with the overall
// level free. The response is nearly linear in the sliders, so the Jacobian is measured once per
// layout and a few Gauss-Newton steps finish. The pre-amp removes any boost.

/// Target grid size, log-spaced 20 Hz to 20 kHz.
pub const TARGET_POINTS: usize = 96;
/// Fitting rate (AutoEQ's).
const FIT_RATE: f64 = 48_000.0;

/// The target grid's frequencies.
pub fn target_grid() -> Vec<f64> {
    (0..TARGET_POINTS).map(|i| 20.0 * 1000f64.powf(i as f64 / (TARGET_POINTS - 1) as f64)).collect()
}

/// A target from curve points (Hz, dB).
pub fn target_from_points(points: &[(f64, f64)]) -> Vec<f64> {
    if points.is_empty() {
        return Vec::new();
    }
    target_grid().into_iter().map(|f| crate::eqfit::curve_at(points, f)).collect()
}

/// A target from a parametric preset's response.
pub fn target_from_bands(bands: &[Band]) -> Vec<f64> {
    target_grid().into_iter().map(|f| response_db(FIT_RATE, bands, f)).collect()
}

/// Sliders fitted to a target and the remaining error.
#[derive(Debug, Clone, PartialEq)]
pub struct CurveFit {
    pub sliders: Vec<f64>,
    /// Pre-amp leaving no boost, <= 0.
    pub preamp_db: f64,
    /// Error over the grid, overall level removed, dB.
    pub rms_db: f64,
    pub max_db: f64,
}

/// (rms, max) error of what `sliders` play against `target`, overall level removed.
pub fn follow(sliders: &[f64], target: &[f64]) -> (f64, f64) {
    let played = played(sliders);
    level_error(&played, target)
}

/// The played response for `sliders` on the target grid.
fn played(sliders: &[f64]) -> Vec<f64> {
    let bands = design(FIT_RATE, sliders);
    target_grid().into_iter().map(|f| response_db(FIT_RATE, &bands, f)).collect()
}

fn level_error(played: &[f64], target: &[f64]) -> (f64, f64) {
    if played.len() != target.len() || target.is_empty() {
        return (0.0, 0.0);
    }
    let diff: Vec<f64> = played.iter().zip(target).map(|(p, t)| p - t).collect();
    let level = diff.iter().sum::<f64>() / diff.len() as f64;
    let rms = (diff.iter().map(|d| (d - level).powi(2)).sum::<f64>() / diff.len() as f64).sqrt();
    (rms, diff.iter().fold(0.0f64, |m, d| m.max((d - level).abs())))
}

/// Ridge per dB² keeping unneeded sliders near 0.
const FIT_RIDGE: f64 = 1e-3;
/// Cost per dB² of the sliders' mean (the free level should take it).
const COMMON_COST: f64 = 1.0;

/// The `count`-band sliders within ±`limit` dB best following `target`; `None` for a non-layout or
/// a target not on [`target_grid`].
pub fn fit_target(target: &[f64], count: usize, limit: f64) -> Option<CurveFit> {
    let n = centres(count).len();
    if n == 0 || target.len() != TARGET_POINTS || target.iter().any(|t| !t.is_finite()) {
        return None;
    }
    // Jacobian: each slider alone at 6 dB, per dB.
    let columns: Vec<Vec<f64>> = (0..n)
        .map(|k| {
            let mut s = vec![0.0; n];
            s[k] = 6.0;
            played(&s).into_iter().map(|v| v / 6.0).collect()
        })
        .collect();
    let mut s = vec![0.0; n];
    let mut now = vec![0.0; TARGET_POINTS];
    for _ in 0..6 {
        let residual: Vec<f64> = target.iter().zip(&now).map(|(t, p)| t - p).collect();
        let step = level_free_step(&columns, &residual, &s);
        for (v, d) in s.iter_mut().zip(&step) {
            *v = (*v + d).clamp(-limit, limit);
        }
        now = played(&s);
    }
    // Rounded to 0.1 dB as shown.
    let sliders: Vec<f64> = s.iter().map(|v| ((v * 10.0).round() / 10.0).clamp(-limit, limit)).collect();
    let now = played(&sliders);
    let (rms_db, max_db) = level_error(&now, target);
    // Pre-amp from the peak boost on a finer grid, rounded up.
    let bands = design(FIT_RATE, &sliders);
    let peak = (0..400).map(|i| response_db(FIT_RATE, &bands, 20.0 * 1000f64.powf(i as f64 / 399.0))).fold(0.0f64, f64::max);
    let preamp_db = -(peak * 10.0).ceil() / 10.0;
    Some(CurveFit { sliders, preamp_db: if preamp_db == 0.0 { 0.0 } else { preamp_db }, rms_db, max_db })
}

/// The step `d` (and a free overall level `c`) minimising `|J d + c - r|² + FIT_RIDGE |s + d|²` plus the
/// common-mode cost.
fn level_free_step(columns: &[Vec<f64>], r: &[f64], s: &[f64]) -> Vec<f64> {
    let n = columns.len();
    let m = n + 1; // the level is the last unknown, with no ridge on it
    let col = |j: usize, i: usize| if j < n { columns[j][i] } else { 1.0 };
    let mut a = vec![vec![0.0; m + 1]; m];
    for i in 0..r.len() {
        for p in 0..m {
            let cp = col(p, i);
            for q in 0..m {
                a[p][q] += cp * col(q, i);
            }
            a[p][m] += cp * r[i];
        }
    }
    // Penalise the sliders' mean so the free level takes it, leaving the sliders' range for the shape.
    let sum: f64 = s.iter().sum();
    for j in 0..n {
        a[j][j] += FIT_RIDGE;
        a[j][m] -= FIT_RIDGE * s[j];
        for k in 0..n {
            a[j][k] += COMMON_COST / n as f64;
        }
        a[j][m] -= COMMON_COST / n as f64 * sum;
    }
    let mut x = gauss_jordan(a);
    x.truncate(n);
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Largest error up to `top` Hz at centres, and at midpoints against the neighbours' average.
    fn errors_of(rate: f64, bands: &[Band], sliders: &[f64], top: f64) -> (f64, f64) {
        let c = centres(sliders.len());
        let top = top.min(0.47 * rate);
        let (mut at_centres, mut between) = (0f64, 0f64);
        for i in 0..c.len() {
            if c[i] > top {
                continue;
            }
            at_centres = at_centres.max((response_db(rate, bands, c[i]) - sliders[i]).abs());
            if i + 1 < c.len() && c[i + 1] <= top {
                let m = (c[i] * c[i + 1]).sqrt();
                between = between.max((response_db(rate, bands, m) - (sliders[i] + sliders[i + 1]) / 2.0).abs());
            }
        }
        (at_centres, between)
    }

    fn errors(rate: f64, sliders: &[f64]) -> (f64, f64) {
        errors_of(rate, &design(rate, sliders), sliders, 16_500.0)
    }

    /// Centre error of the sliders fed straight to the bells, uncorrected.
    fn uncorrected(rate: f64, sliders: &[f64]) -> f64 {
        let q = q_for(width_for(sliders.len()) / per_octave(sliders.len()).unwrap());
        let bands: Vec<Band> = centres(sliders.len()).into_iter().zip(sliders).map(|(f, g)| bell(f, *g, q)).collect();
        errors_of(rate, &bands, sliders, 16_500.0).0
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        }
    }

    /// A random walk across bands within ±12 dB, in 0.5 dB steps.
    fn random_curve(r: &mut Lcg, n: usize) -> Vec<f64> {
        let mut v: f64 = 0.0;
        (0..n)
            .map(|_| {
                v = (v + r.next() * 6.0).clamp(-12.0, 12.0);
                (v * 2.0).round() / 2.0
            })
            .collect()
    }

    /// AutoEQ's filter lines ("Filter 1: ON PK Fc 31 Hz Gain 6.9 dB Q 1.41"), as bands.
    fn autoeq_filters(text: &str) -> Vec<Band> {
        text.lines()
            .filter_map(|l| {
                let t: Vec<&str> = l.split_whitespace().collect();
                let at = |k: &str| t.iter().position(|w| *w == k).and_then(|i| t.get(i + 1)?.parse::<f64>().ok());
                let kind = match *t.get(3)? {
                    "LSC" => crate::dsp::LOW_SHELF,
                    "HSC" => HIGH_SHELF,
                    "PK" => PEAKING,
                    _ => return None,
                };
                Some(Band { kind, freq: at("Fc")?, gain_db: at("Gain")?, q: at("Q")?, channel: CH_BOTH })
            })
            .collect()
    }

    /// Real AutoEQ curves on every layout: within bounds, no boost after the pre-amp, and closer than
    /// AutoEQ's own FixedBandEQ (as plain bells or as our sliders).
    #[test]
    fn real_autoeq_targets_fit() {
        let cases = [
            ("Sennheiser HD 600", include_str!("../testdata/graphiceq/sennheiser-hd-600.txt"), include_str!("../testdata/graphiceq/sennheiser-hd-600.fixedband.txt"), include_str!("../testdata/graphiceq/sennheiser-hd-600.parametric.txt")),
            ("Sony WH-1000XM6 (analog cable)", include_str!("../testdata/graphiceq/sony-wh-1000xm6-analog-cable.txt"), include_str!("../testdata/graphiceq/sony-wh-1000xm6-analog-cable.fixedband.txt"), include_str!("../testdata/graphiceq/sony-wh-1000xm6-analog-cable.parametric.txt")),
            ("64 Audio U12t", include_str!("../testdata/graphiceq/64-audio-u12t.txt"), include_str!("../testdata/graphiceq/64-audio-u12t.fixedband.txt"), include_str!("../testdata/graphiceq/64-audio-u12t.parametric.txt")),
        ];
        // rms and max bounds per layout, dB, over 20 Hz to 20 kHz. The octave layouts end at 16 kHz and
        // cannot follow a curve that falls or rises steeply above it (the U12t's): their max is there.
        let bounds = [(5usize, 3.0, 13.5), (10, 2.0, 10.0), (15, 1.7, 10.5), (31, 0.8, 5.0)];
        for (name, graphic, fixed, parametric) in cases {
            let target = target_from_points(&crate::eqfit::parse_graphic(graphic).unwrap());
            for (count, rms_bound, max_bound) in bounds {
                let fit = fit_target(&target, count, 12.0).unwrap();
                assert!(fit.rms_db < rms_bound && fit.max_db < max_bound, "{name}, {count} bands: rms {} max {}", fit.rms_db, fit.max_db);
                assert!(fit.sliders.iter().all(|v| v.abs() <= 12.0));
                let bands = design(FIT_RATE, &fit.sliders);
                let worst = (0..2000).map(|i| response_db(FIT_RATE, &bands, 20.0 * 1000f64.powf(i as f64 / 1999.0))).fold(f64::MIN, f64::max);
                assert!(worst + fit.preamp_db <= 1e-9, "{name}, {count} bands: {worst} dB of boost against a {} dB pre-amp", fit.preamp_db);
                assert_eq!(follow(&fit.sliders, &target), (fit.rms_db, fit.max_db));
            }
            // AutoEQ's FixedBandEQ.txt: ten bells at the octave centres, Q 1.41, gains fitted for plain bells.
            let theirs = autoeq_filters(fixed);
            let as_bells = level_error(&target_grid().into_iter().map(|f| response_db(FIT_RATE, &theirs, f)).collect::<Vec<_>>(), &target);
            let as_sliders = follow(&theirs.iter().map(|b| b.gain_db).collect::<Vec<_>>(), &target);
            let ours = fit_target(&target, 10, 12.0).unwrap();

            assert!(ours.rms_db <= as_bells.0 && ours.rms_db <= as_sliders.0, "{name}: our fit is the closest");
            let from_preset = target_from_bands(&autoeq_filters(parametric));
            let fit = fit_target(&from_preset, 31, 12.0).unwrap();
            assert!(fit.rms_db < 0.6, "{name}: 31 bands on the parametric preset's response: rms {}", fit.rms_db);
        }
    }

    #[test]
    fn layouts() {
        // Two octaves apart, every other band of the ten: 63, 250, 1k, 4k, 16k.
        let n: Vec<f64> = centres(5).into_iter().map(nominal).collect();
        assert_eq!(n, [63.0, 250.0, 1000.0, 4000.0, 16000.0]);
        let ten = centres(10);
        assert!(centres(5).iter().all(|f| ten.contains(f)), "each of them one of the ten's");
        let n: Vec<f64> = centres(10).into_iter().map(nominal).collect();
        assert_eq!(n, [31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0]);
        let n: Vec<f64> = centres(15).into_iter().map(nominal).collect();
        assert_eq!(n, [25.0, 40.0, 63.0, 100.0, 160.0, 250.0, 400.0, 630.0, 1000.0, 1600.0, 2500.0, 4000.0, 6300.0, 10000.0, 16000.0]);
        let n: Vec<f64> = centres(31).into_iter().map(nominal).collect();
        assert_eq!(n.first(), Some(&20.0));
        assert_eq!(n[17], 1000.0);
        assert_eq!(n.last(), Some(&20000.0));
        assert!(centres(12).is_empty() && design(48_000.0, &[3.0; 12]).is_empty(), "not a layout: no filters");

        // Flat sliders are no filters.
        for n in LAYOUTS {
            assert!(design(48_000.0, &vec![0.0; n]).is_empty());
            assert!(design(48_000.0, &vec![f64::NAN; n]).is_empty(), "a slider that is not a number is 0");
        }

        // Invalid target or layout is no fit.
        assert!(fit_target(&[1.0; 10], 10, 12.0).is_none());
        assert!(fit_target(&vec![0.0; TARGET_POINTS], 12, 12.0).is_none());
        let flat = fit_target(&vec![-3.0; TARGET_POINTS], 10, 12.0).unwrap();
        assert!(flat.sliders.iter().all(|v| *v == 0.0) && flat.preamp_db == 0.0, "a flat curve at any level is no correction: {flat:?}");
    }

    #[test]
    fn solve_undoes_bell_overlap() {
        // All ten at +6 dB: the bells alone reach far past +6 at every centre.
        let all = [6.0; 10];
        let raw = uncorrected(48_000.0, &all);
        assert!(raw > 3.0, "uncorrected error {raw}");
        let (c, m) = errors(48_000.0, &all);
        assert!(c < 0.2 && m < 0.3, "corrected: {c} at the centres, {m} between");
        // One slider alone: it lands on its value, and the bands two away stay near zero.
        let mut one = [0.0; 10];
        one[5] = 12.0;
        let bands = design(48_000.0, &one);
        let c = centres(10);
        assert!((response_db(48_000.0, &bands, c[5]) - 12.0).abs() < 0.3);
        assert!(response_db(48_000.0, &bands, c[3]).abs() < 0.3 && response_db(48_000.0, &bands, c[7]).abs() < 0.3);
    }

    #[test]
    fn response_follows_random_sliders() {
        let mut r = Lcg(7);
        for n in LAYOUTS {
            let (mut worst_c, mut worst_m, mut worst_top) = (0f64, 0f64, 0f64);
            for _ in 0..40 {
                let s = random_curve(&mut r, n);
                for rate in [44_100.0, 48_000.0, 96_000.0] {
                    let bands = design(rate, &s);
                    let (c, m) = errors_of(rate, &bands, &s, 16_500.0);
                    worst_c = worst_c.max(c);
                    worst_m = worst_m.max(m);
                    // Up to the top band, 20 kHz included where the rate carries it.
                    let (c, _) = errors_of(rate, &bands, &s, 21_000.0);
                    worst_top = worst_top.max(c);
                }
            }
            assert!(worst_c < 0.5 && worst_m < 1.0 && worst_top < 1.5, "{n} bands: {worst_c} at the centres, {worst_m} between, {worst_top} to 20 kHz");
        }
    }

    #[test]
    fn draws_neighbour_pair_and_zigzag() {
        for n in LAYOUTS {
            let mut pair = vec![0.0; n];
            (pair[n / 2], pair[n / 2 + 1]) = (12.0, -12.0);
            let (c, _) = errors(48_000.0, &pair);
            assert!(c < 0.5, "{n} bands: +12 next to -12 is {c} dB off");
            let zig: Vec<f64> = (0..n).map(|i| if i % 2 == 0 { 12.0 } else { -12.0 }).collect();
            let bands = design(48_000.0, &zig);
            assert!(bands.iter().all(|b| b.gain_db.abs() <= MAX_FILTER_DB));
            let (c, _) = errors(48_000.0, &zig);
            assert!(c < 3.0, "{n} bands: zigzag {c} dB off");
        }
    }

    #[test]
    fn relayout_keeps_curve() {
        let ten = [0.0, 2.0, 4.0, 6.0, 4.0, 2.0, 0.0, -2.0, -4.0, -6.0];
        assert_eq!(relayout(&ten, 15).len(), 15);
        let back = relayout(&relayout(&ten, 31), 10);
        for (a, b) in ten.iter().zip(&back) {
            assert!((a - b).abs() < 1e-9, "{ten:?} -> {back:?}");
        }
        assert_eq!(relayout(&[1.0; 7], 10), vec![0.0; 10], "not a layout: flat");
    }

    #[test]
    fn parametric_preset_to_sliders() {
        let shelf = [Band { kind: crate::dsp::LOW_SHELF, freq: 100.0, gain_db: 6.0, q: 0.7, channel: CH_BOTH }];
        let s = sliders_for(&shelf, 10, 12.0);
        assert!(s[0] > 5.0 && s[9].abs() < 0.1, "{s:?}");
    }
}
