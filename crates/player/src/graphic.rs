//! The graphic equalizer: fixed ISO bands whose sliders say what the response is at each band's centre,
//! as opposed to the parametric equalizer, where each band is a filter and its gain is that filter's own.
//!
//! **The filters.** One peaking biquad per band (the RBJ design the parametric chain runs,
//! `dsp::band_coefficients`), centred on the ISO 266 frequencies: two octaves apart from 63 Hz (5
//! bands: every other band of the ten, 63, 250, 1k, 4k and 16k), octaves from 31.5 Hz (10 bands),
//! two-thirds of an octave from 25 Hz (15) or thirds from 20 Hz (31). The five are the ISO ones rather
//! than Android's own 60, 230, 910, 3.6k and 14k (the platform equalizer's default, a chip vendor's
//! choice and no standard): the spacing is the same two octaves, but on the ISO series a band's label
//! is its true centre, a curve moved between five and ten bands keeps its sliders where the bands are
//! shared, and a headphone correction is fitted on the same grid as every other layout. They are *constant-Q*: every
//! bell has the same width in octaves whatever its gain (RBJ's Q holds the bandwidth between the
//! half-gain points), the same for every band of a layout. The alternative, proportional-Q (the width
//! shrinks as the gain grows, as many analogue boxes do), makes small moves broad and sweet, but how two
//! neighbours add up then depends on how far each is moved, which is what makes its sliders lie. With
//! constant-Q the dB response of a band is very nearly its gain times a fixed shape, so how the bands
//! interact is a matrix known in advance, and it can be undone.
//!
//! **The interaction.** Neighbouring bells overlap: ten octave bells all at +6 dB add up to some +11.5 dB
//! at every centre, and a single slider at +12 lifts its neighbours by several dB. So the filters are
//! not given the sliders' values. Following Välimäki and Liski's accurate cascade graphic equalizer
//! (IEEE SPL 2017), the dB response is modelled as `B g`, where column `n` of `B` is band `n`'s response,
//! per dB of its gain, sampled at every band centre and at the geometric midpoint between each pair of
//! neighbours; the target at a centre is its slider and at a midpoint the average of the two sliders
//! around it. The filter gains `g` are the weighted least-squares solution (midpoints count
//! [`MIDPOINT_WEIGHT`] against a centre's 1), with `B` measured once at a prototype gain of 12 dB and
//! then again at the gains the first solve found, since a bell's shape moves a little with its gain.
//! Three Gauss-Newton steps against the exact response of the filters as designed take out what is
//! left. The midpoints are what keeps the curve between the centres from rippling: solved on the centres
//! alone the sliders are hit exactly and the response swings between them.
//!
//! **The width** is one and a half band spacings between the half-gain points (1.5 octaves for octave
//! bands, half an octave for thirds): measured (`sweep`, below) over random curves, a flat +12 dB, one
//! slider, an adjacent +12/-12 pair and a full zigzag, it is the widest bell that still draws a zigzag;
//! wider bells are smoother between the centres but cannot make neighbours differ by 24 dB, narrower
//! ones ripple. The width is kept in octaves as the sample rate sees it: a bell's Q is prewarped by
//! `w / sin w` (RBJ's bandwidth form), or the bilinear transform squeezes the top octaves' bells narrow
//! and the curve between 8 and 20 kHz ripples by several dB. A bilinear bell is 0 dB at Nyquist whatever
//! its gain, so a top band in the last fifth of the band the rate carries (the 20 kHz third at 44.1 and
//! 48 kHz) is a high shelf instead.
//!
//! Measured at 44.1 and 48 kHz over 40 random curves in ±12 dB per layout: at most 0.3 dB off at the
//! centres and 0.7 dB between them (up to 16 kHz); a flat +12 is within 0.4 dB everywhere. The five
//! bands (bells 1.25 spacings wide, see [`WIDTH_FIVE`]): 0.2 dB at the centres, 0.7 between, a full
//! ±12 zigzag 0.34 off; they follow a real AutoEQ correction to about 2 dB rms (the ten: 1 to 1.3).
//!
//! All of this runs when a slider moves, never per buffer: a 31-band solve is some thirty thousand
//! multiply-adds, done a dozen times.

use crate::dsp::{band_coefficients, Band, CH_BOTH, HIGH_SHELF, PEAKING};

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

/// The exact (base 2) centre frequencies of a layout, low to high; empty for a count that is not one.
/// Octaves run 31.25 Hz to 16 kHz, two-thirds 24.8 Hz to 16 kHz, thirds 19.7 Hz to 20.2 kHz; the
/// nominal ISO labels (31.5, 63, 125 ... 16k) are [`nominal`]'s.
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

/// The frequency a band is labelled with: the ISO 266 preferred number nearest its exact centre.
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

/// How wide each bell is, in band spacings (see the module's notes).
const WIDTH_IN_SPACINGS: f64 = 1.5;
/// The five-band layout's bells are narrower, 1.25 spacings (two and a half octaves): at 1.5 its zigzag
/// was 3.2 dB off at the centres (the `sweep`), at 1.25 it is 0.3, and the rest hardly moves.
const WIDTH_FIVE: f64 = 1.25;

fn width_for(count: usize) -> f64 {
    if count == 5 { WIDTH_FIVE } else { WIDTH_IN_SPACINGS }
}

/// The dB gain of one biquad at `freq`.
fn biquad_db(c: &[f64; 5], rate: f64, freq: f64) -> f64 {
    let w = std::f64::consts::TAU * freq / rate;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let (nr, ni) = (c[0] + c[1] * c1 + c[2] * c2, -(c[1] * s1 + c[2] * s2));
    let (dr, di) = (1.0 + c[3] * c1 + c[4] * c2, -(c[3] * s1 + c[4] * s2));
    10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).max(1e-30).log10()
}

/// The response in dB of `bands` in cascade at `freq`, as the chain plays them at `rate`.
pub fn response_db(rate: f64, bands: &[Band], freq: f64) -> f64 {
    bands.iter().map(|b| biquad_db(&band_coefficients(rate, b), rate, freq)).sum()
}

fn bell(freq: f64, gain_db: f64, q: f64) -> Band {
    Band { kind: PEAKING, freq, gain_db, q, channel: CH_BOTH }
}

/// The largest gain a filter is given. A zigzag of ±12 dB between neighbours needs opposing bells of
/// some 28 dB; past this the sliders ask for something the layout cannot draw.
pub const MAX_FILTER_DB: f64 = 30.0;
/// The gain `B` is first measured at (the paper's prototype gain).
const PROTOTYPE_DB: f64 = 12.0;
/// How much a midpoint counts against a centre in the fit: the sliders are hit almost exactly, the
/// ripple between them is kept down.
const MIDPOINT_WEIGHT: f64 = 0.35;
/// Ridge on the filter gains, per dB²: next to nothing, raised only for sliders the filters cannot
/// follow within [`MAX_FILTER_DB`].
const RIDGE: f64 = 1e-4;
/// The top band's shelf, when it is one: as steep as a shelf goes without overshoot.
const SHELF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// Refinement steps with the exact response after the two solves.
const REFINE: usize = 3;

/// The filters for a graphic equalizer with these slider values (dB, one per band of a layout, low to
/// high) at `rate`: one constant-Q bell per band whose centre is below 0.47 of the rate, gains solved so
/// the response follows the sliders. Sliders all at 0 give no filters at all; a count that is not a
/// layout gives none either.
pub fn design(rate: f64, sliders: &[f64]) -> Vec<Band> {
    design_with(rate, sliders, width_for(sliders.len()), MIDPOINT_WEIGHT)
}

fn design_with(rate: f64, sliders: &[f64], width: f64, midpoint_weight: f64) -> Vec<Band> {
    let count = sliders.len();
    let Some(per_octave) = per_octave(count) else { return Vec::new() };
    if !(rate > 0.0) {
        return Vec::new();
    }
    let octaves = width / per_octave;
    // A band too close to Nyquist cannot be drawn; the ones below still follow their sliders.
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
    // Where the fit looks: the centres, and the midpoints between neighbours.
    let mut points: Vec<(f64, f64, f64)> = Vec::with_capacity(2 * n); // (freq, target dB, weight)
    for i in 0..n {
        points.push((freqs[i], kept[i].1, 1.0));
        if i + 1 < n {
            points.push(((freqs[i] * freqs[i + 1]).sqrt(), (kept[i].1 + kept[i + 1].1) / 2.0, midpoint_weight));
        }
    }
    // Column n of B: band n's response per dB at every point, its shape measured at `at[n]` dB.
    let shape = |at: &[f64]| -> Vec<Vec<f64>> {
        points
            .iter()
            .map(|&(f, _, _)| {
                (0..n)
                    .map(|j| {
                        let g = if at[j].abs() < 1.0 { PROTOTYPE_DB } else { at[j] };
                        biquad_db(&band_coefficients(rate, &filter(j, g)), rate, f) / g
                    })
                    .collect()
            })
            .collect()
    };
    let targets: Vec<f64> = points.iter().map(|p| p.1).collect();
    let b0 = shape(&vec![PROTOTYPE_DB; n]);
    let zero = vec![0.0; n];
    // Sliders the layout cannot draw (a zigzag between neighbours) ask for ever larger opposing filters;
    // rather than clip those (which leaves a curve that is neither), the ridge is raised until the
    // filters fit, which gives the nearest curve the layout can draw.
    let mut ridge = RIDGE;
    let mut g = zero.clone();
    for _ in 0..8 {
        g = solve(&b0, &points, &targets, &zero, ridge);
        let b1 = shape(&g);
        g = solve(&b1, &points, &targets, &zero, ridge);
        // What the solve could not see: each filter's exact shape at its own gain. Gauss-Newton steps
        // with B as the Jacobian, on the same regularised error.
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

/// Weighted, ridge-regularised least squares: the `x` minimising `Σ w (B x - t)² + ridge |x + bias|²`
/// (a Gauss-Newton step from `bias` keeps the ridge on the gains, not the step), from its normal
/// equations by Gaussian elimination with partial pivoting. There are at most 31 unknowns.
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

/// Sliders for another layout that draw the same curve: each new band takes the old curve at its centre,
/// read between the old centres in log frequency (and held flat past the ends).
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

/// Sliders that draw a curve made of filters (a parametric preset): the curve's response at each band's
/// centre, held to `limit` dB either way.
pub fn sliders_for(bands: &[Band], count: usize, limit: f64) -> Vec<f64> {
    centres(count).into_iter().map(|f| (response_db(48_000.0, bands, f) * 10.0).round() / 10.0).map(|g| g.clamp(-limit, limit)).collect()
}

// ---- a headphone correction on the graphic equalizer ----
//
// AutoEQ's curve for a headphone (its GraphicEQ.txt, a hundred-odd points; or the response of its
// parametric preset where there is none) is kept as a *target*: its level on a fixed log grid. The
// sliders are fitted so that what the graphic equalizer actually plays - the corrected cascade
// `design` makes of them, not the sliders read as points - follows the target over 20 Hz to 20 kHz,
// by least squares in dB with the curve's overall level taken out. The fit is linear in the sliders
// to within a fraction of a dB (`design` is a least-squares projection of them), so its Jacobian is
// measured once per layout, one slider at a time, and a few Gauss-Newton steps against the exact
// response take out the rest. The pre-amp is set so the result boosts nowhere.

/// The target's grid: this many points, log-spaced from 20 Hz to 20 kHz (about a tenth of an octave apart).
pub const TARGET_POINTS: usize = 96;
/// The rate a target is fitted at: the common output rate, and the one AutoEQ designs for.
const FIT_RATE: f64 = 48_000.0;

/// The frequencies of the target's grid.
pub fn target_grid() -> Vec<f64> {
    (0..TARGET_POINTS).map(|i| 20.0 * 1000f64.powf(i as f64 / (TARGET_POINTS - 1) as f64)).collect()
}

/// A target from a curve's points (Hz, dB), read between them in log frequency and flat past its ends.
pub fn target_from_points(points: &[(f64, f64)]) -> Vec<f64> {
    if points.is_empty() {
        return Vec::new();
    }
    target_grid().into_iter().map(|f| crate::eqfit::curve_at(points, f)).collect()
}

/// A target from filters (a parametric preset): their response on the grid.
pub fn target_from_bands(bands: &[Band]) -> Vec<f64> {
    target_grid().into_iter().map(|f| response_db(FIT_RATE, bands, f)).collect()
}

/// Sliders fitted to a target, and how closely the graphic equalizer then follows it.
#[derive(Debug, Clone, PartialEq)]
pub struct CurveFit {
    pub sliders: Vec<f64>,
    /// The pre-amp that leaves no boost anywhere, 0 or below.
    pub preamp_db: f64,
    /// Root mean square and largest difference, dB, over the grid, the overall level taken out.
    pub rms_db: f64,
    pub max_db: f64,
}

/// How far what `sliders` play is from `target`, rms and largest, with the best overall level.
pub fn follow(sliders: &[f64], target: &[f64]) -> (f64, f64) {
    let played = played(sliders);
    level_error(&played, target)
}

/// What the graphic equalizer plays for `sliders`, on the target's grid.
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

/// Ridge on the sliders, per dB², in the fit: enough to keep a slider the curve does not need near 0.
const FIT_RIDGE: f64 = 1e-3;
/// What the sliders' mean costs, per dB² of it, against the error summed over the grid.
const COMMON_COST: f64 = 1.0;

/// The sliders of a `count`-band layout, each within `limit` dB, whose played response follows `target`
/// most closely; `None` for a count that is not a layout or a target not on [`target_grid`].
pub fn fit_target(target: &[f64], count: usize, limit: f64) -> Option<CurveFit> {
    let n = centres(count).len();
    if n == 0 || target.len() != TARGET_POINTS || target.iter().any(|t| !t.is_finite()) {
        return None;
    }
    // The Jacobian: what each slider alone, at 6 dB, does to the played response, per dB.
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
        // A step from `s` towards the target, the overall level free (the last unknown).
        let residual: Vec<f64> = target.iter().zip(&now).map(|(t, p)| t - p).collect();
        let step = level_free_step(&columns, &residual, &s);
        for (v, d) in s.iter_mut().zip(&step) {
            *v = (*v + d).clamp(-limit, limit);
        }
        now = played(&s);
    }
    // Written as the screen shows them, a tenth of a decibel each.
    let sliders: Vec<f64> = s.iter().map(|v| ((v * 10.0).round() / 10.0).clamp(-limit, limit)).collect();
    let now = played(&sliders);
    let (rms_db, max_db) = level_error(&now, target);
    // The pre-amp from a finer look than the grid: the largest boost anywhere, rounded up.
    let bands = design(FIT_RATE, &sliders);
    let peak = (0..400).map(|i| response_db(FIT_RATE, &bands, 20.0 * 1000f64.powf(i as f64 / 399.0))).fold(0.0f64, f64::max);
    let preamp_db = -(peak * 10.0).ceil() / 10.0;
    Some(CurveFit { sliders, preamp_db: if preamp_db == 0.0 { 0.0 } else { preamp_db }, rms_db, max_db })
}

/// The least-squares step `d` (with an overall level alongside it) that minimises
/// `|J d + c - r|² + FIT_RIDGE |s + d|²`, by its normal equations.
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
    // All the sliders up together is nearly the same curve as the overall level up: that common part
    // goes to the level, which is free, and the sliders keep their range for the shape.
    let sum: f64 = s.iter().sum();
    for j in 0..n {
        a[j][j] += FIT_RIDGE;
        a[j][m] -= FIT_RIDGE * s[j];
        for k in 0..n {
            a[j][k] += COMMON_COST / n as f64;
        }
        a[j][m] -= COMMON_COST / n as f64 * sum;
    }
    for c in 0..m {
        let p = (c..m).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs())).unwrap_or(c);
        a.swap(c, p);
        let d = a[c][c];
        if d.abs() < 1e-12 {
            continue;
        }
        for row in 0..m {
            if row != c {
                let f = a[row][c] / d;
                if f != 0.0 {
                    for k in c..=m {
                        a[row][k] -= f * a[c][k];
                    }
                }
            }
        }
    }
    (0..n).map(|i| if a[i][i].abs() < 1e-12 { 0.0 } else { a[i][m] / a[i][i] }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How far the curve `bands` draws is from the sliders, up to `top` Hz: the largest error at a
    /// centre, and the largest at a midpoint against the average of the two sliders around it.
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

    /// The sliders given straight to the same bells, uncorrected: how badly they lie.
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

    /// Curves as people set them: a random walk from band to band, held in ±12 dB, in half dB steps.
    fn random_curve(r: &mut Lcg, n: usize) -> Vec<f64> {
        let mut v: f64 = 0.0;
        (0..n)
            .map(|_| {
                v = (v + r.next() * 6.0).clamp(-12.0, 12.0);
                (v * 2.0).round() / 2.0
            })
            .collect()
    }

    /// The measurement the width and the midpoint weight were chosen by:
    /// `cargo test -p nori-player --lib graphic::tests::sweep -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn sweep() {
        for n in LAYOUTS {
            for width in [1.0, 1.25, 1.5, 1.75, 2.0] {
                for mw in [0.25, 0.35, 0.5] {
                    let d = |rate: f64, s: &[f64]| design_with(rate, s, width, mw);
                    let mut r = Lcg(7);
                    let (mut wc, mut wm) = (0f64, 0f64);
                    for _ in 0..40 {
                        let s = random_curve(&mut r, n);
                        for rate in [44_100.0, 48_000.0] {
                            let (c, m) = errors_of(rate, &d(rate, &s), &s, 16_500.0);
                            (wc, wm) = (wc.max(c), wm.max(m));
                        }
                    }
                    let zig: Vec<f64> = (0..n).map(|i| if i % 2 == 0 { 12.0 } else { -12.0 }).collect();
                    let (zc, _) = errors_of(48_000.0, &d(48_000.0, &zig), &zig, 16_500.0);
                    let all = vec![12.0; n];
                    let (ac, am) = errors_of(48_000.0, &d(48_000.0, &all), &all, 16_500.0);
                    let mut pair = vec![0.0; n];
                    (pair[n / 2], pair[n / 2 + 1]) = (12.0, -12.0);
                    let (pc, pm) = errors_of(48_000.0, &d(48_000.0, &pair), &pair, 16_500.0);
                    eprintln!("{n} bands, width {width}, midpoints {mw}: random {wc:.2}/{wm:.2} zigzag {zc:.2} flat +12 {ac:.2}/{am:.2} pair {pc:.2}/{pm:.2}");
                }
            }
        }
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

    /// Real AutoEQ curves (GraphicEQ.txt) on every layout: how closely the graphic equalizer follows
    /// each, that the pre-amp stops every boost, and how AutoEQ's own ten-band FixedBandEQ.txt does
    /// against the same curve, played either as the plain bells it is written for or as our sliders.
    #[test]
    fn real_autoeq_curves_on_the_graphic_equalizer() {
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
                eprintln!("{name}, {count} bands: rms {:.2} dB, max {:.2} dB, pre-amp {:.1} dB", fit.rms_db, fit.max_db, fit.preamp_db);
                assert!(fit.rms_db < rms_bound && fit.max_db < max_bound, "{name}, {count} bands: rms {} max {}", fit.rms_db, fit.max_db);
                // Where the octave layouts have bands: their error above 16 kHz, where they have none, is most of their max.
                let g = target_grid();
                let below: Vec<usize> = (0..g.len()).filter(|&i| g[i] <= 16_000.0).collect();
                let p = played(&fit.sliders);
                let (rms16, max16) = level_error(&below.iter().map(|&i| p[i]).collect::<Vec<_>>(), &below.iter().map(|&i| target[i]).collect::<Vec<_>>());
                eprintln!("    up to 16 kHz: rms {rms16:.2} dB, max {max16:.2} dB");
                assert!(fit.sliders.iter().all(|v| v.abs() <= 12.0));
                // With its pre-amp the correction boosts nowhere.
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
            eprintln!(
                "{name}: FixedBandEQ as plain bells rms {:.2} max {:.2}; as our sliders rms {:.2} max {:.2}; our 10-band fit rms {:.2} max {:.2}",
                as_bells.0, as_bells.1, as_sliders.0, as_sliders.1, ours.rms_db, ours.max_db
            );
            assert!(ours.rms_db <= as_bells.0 && ours.rms_db <= as_sliders.0, "{name}: our fit is the closest");
            // A parametric preset's response is a target too, where a GraphicEQ.txt is missing.
            let from_preset = target_from_bands(&autoeq_filters(parametric));
            let fit = fit_target(&from_preset, 31, 12.0).unwrap();
            assert!(fit.rms_db < 0.6, "{name}: 31 bands on the parametric preset's response: rms {}", fit.rms_db);
        }
    }

    #[test]
    fn a_target_off_the_grid_or_a_count_off_the_layouts_is_no_fit() {
        assert!(fit_target(&[1.0; 10], 10, 12.0).is_none());
        assert!(fit_target(&vec![0.0; TARGET_POINTS], 12, 12.0).is_none());
        let flat = fit_target(&vec![-3.0; TARGET_POINTS], 10, 12.0).unwrap();
        assert!(flat.sliders.iter().all(|v| *v == 0.0) && flat.preamp_db == 0.0, "a flat curve at any level is no correction: {flat:?}");
    }

    #[test]
    fn the_layouts_are_the_iso_bands() {
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
    }

    #[test]
    fn flat_sliders_are_no_filters() {
        for n in LAYOUTS {
            assert!(design(48_000.0, &vec![0.0; n]).is_empty());
            assert!(design(48_000.0, &vec![f64::NAN; n]).is_empty(), "a slider that is not a number is 0");
        }
    }

    #[test]
    fn uncorrected_bells_pile_up_and_the_solve_undoes_it() {
        // All ten at +6 dB: the bells alone reach far past +6 at every centre.
        let all = [6.0; 10];
        let raw = uncorrected(48_000.0, &all);
        eprintln!("ten bands at +6 uncorrected: {raw:.2} dB off");
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
    fn the_response_follows_random_sliders_in_every_layout() {
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
            eprintln!("{n} bands: worst {worst_c:.2} dB at the centres, {worst_m:.2} dB between, {worst_top:.2} at any centre");
            assert!(worst_c < 0.5 && worst_m < 1.0 && worst_top < 1.5, "{n} bands: {worst_c} at the centres, {worst_m} between, {worst_top} to 20 kHz");
        }
    }

    #[test]
    fn neighbours_apart_and_a_full_zigzag_are_drawn() {
        for n in LAYOUTS {
            let mut pair = vec![0.0; n];
            (pair[n / 2], pair[n / 2 + 1]) = (12.0, -12.0);
            let (c, _) = errors(48_000.0, &pair);
            assert!(c < 0.5, "{n} bands: +12 next to -12 is {c} dB off");
            let zig: Vec<f64> = (0..n).map(|i| if i % 2 == 0 { 12.0 } else { -12.0 }).collect();
            let bands = design(48_000.0, &zig);
            assert!(bands.iter().all(|b| b.gain_db.abs() <= MAX_FILTER_DB));
            let (c, _) = errors(48_000.0, &zig);
            eprintln!("{n} bands zigzag: {c:.2} dB off at the centres");
            assert!(c < 3.0, "{n} bands: zigzag {c} dB off");
        }
    }

    #[test]
    fn relayout_keeps_the_curve() {
        let ten = [0.0, 2.0, 4.0, 6.0, 4.0, 2.0, 0.0, -2.0, -4.0, -6.0];
        assert_eq!(relayout(&ten, 15).len(), 15);
        let back = relayout(&relayout(&ten, 31), 10);
        for (a, b) in ten.iter().zip(&back) {
            assert!((a - b).abs() < 1e-9, "{ten:?} -> {back:?}");
        }
        assert_eq!(relayout(&[1.0; 7], 10), vec![0.0; 10], "not a layout: flat");
    }

    #[test]
    fn a_parametric_preset_becomes_sliders() {
        let shelf = [Band { kind: crate::dsp::LOW_SHELF, freq: 100.0, gain_db: 6.0, q: 0.7, channel: CH_BOTH }];
        let s = sliders_for(&shelf, 10, 12.0);
        assert!(s[0] > 5.0 && s[9].abs() < 0.1, "{s:?}");
    }
}
