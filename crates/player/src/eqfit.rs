//! A graphic equalizer curve turned into a parametric preset. A `GraphicEQ:` line (AutoEQ's
//! "GraphicEQ.txt", Wavelet's and Equalizer APO's format: frequency and gain pairs, a hundred or more of
//! them) is fitted once, when it is chosen, with the filters an AutoEQ parametric preset has: a low
//! shelf, a high shelf and eight peaks. What comes out is an ordinary preset, so playing it costs exactly
//! what any parametric preset costs; nothing here runs per sample.
//!
//! The fit minimises the squared difference in dB between the filters' response and the curve on a
//! log-frequency grid from 20 Hz to 20 kHz, the response being the chain's own biquads
//! (`dsp::band_coefficients`) at 48 kHz. A constant level is fitted alongside, because a graphic curve
//! carries its own overall level, which filters cannot and need not copy; the pre-amp is then set so
//! the preset boosts nowhere, as AutoEQ's own presets do. Levenberg-Marquardt over the filters'
//! log-frequency, log-Q and gain, started from a greedy placement: each new peak goes where the
//! remaining error is largest.

use crate::dsp::{band_coefficients, Band, CH_BOTH, HIGH_SHELF, LOW_SHELF, PEAKING};
use crate::types::{EqBand, EqKind};

/// The rate the responses are designed at: the common output rate, and the one AutoEQ designs for.
const RATE: f64 = 48_000.0;
/// Points on the grid the fit is measured on, log-spaced from `LOW_HZ` to `HIGH_HZ` (about 16 a third of an octave).
const POINTS: usize = 160;
const LOW_HZ: f64 = 20.0;
const HIGH_HZ: f64 = 20_000.0;
/// The peaks besides the two shelves: ten filters in all, as in AutoEQ's parametric presets.
pub const PEAKS: usize = 8;
/// The shelves' Q, as AutoEQ's; their corners move.
const SHELF_Q: f64 = 0.7;
/// Where a filter may go, and how wide or narrow a peak may be.
const PEAK_HZ: (f64, f64) = (20.0, 18_000.0);
const LOW_SHELF_HZ: (f64, f64) = (20.0, 500.0);
const HIGH_SHELF_HZ: (f64, f64) = (1_000.0, 16_000.0);
const PEAK_Q: (f64, f64) = (0.18, 6.0);
const GAIN_DB: f64 = 20.0;
/// What a decibel of any filter's gain costs, in decibels of error at one grid point: enough that two big
/// filters cancelling each other lose to two small ones that do the same, too little to cost accuracy.
const GAIN_COST: f64 = 0.1;

/// A fitted preset and how far it is from the curve, in dB after the overall level is taken out.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphicFit {
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
    /// Root mean square difference over 20 Hz to 20 kHz on the log-frequency grid.
    pub rms_db: f32,
    /// The largest difference anywhere on the grid.
    pub max_db: f32,
}

/// The points of a `GraphicEQ: 20 -4.9; 21 -5.2; ...` line, by frequency; none without such a line or
/// with fewer than two points in it.
pub fn parse_graphic(text: &str) -> Option<Vec<(f64, f64)>> {
    const KEY: &str = "graphiceq:";
    let line = text.lines().map(str::trim).find(|l| l.get(..KEY.len()).is_some_and(|k| k.eq_ignore_ascii_case(KEY)))?;
    let mut points: Vec<(f64, f64)> = line[KEY.len()..]
        .split(';')
        .filter_map(|pair| {
            let mut w = pair.split_whitespace();
            let f: f64 = w.next()?.parse().ok()?;
            let g: f64 = w.next()?.parse().ok()?;
            (f.is_finite() && g.is_finite() && f > 0.0).then_some((f, g))
        })
        .collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    points.dedup_by(|a, b| a.0 == b.0);
    (points.len() >= 2).then_some(points)
}

/// The curve through `points` at `f`: straight lines between them on a log-frequency axis, flat beyond
/// the first and last.
pub(crate) fn curve_at(points: &[(f64, f64)], f: f64) -> f64 {
    let i = points.partition_point(|p| p.0 < f);
    if i == 0 {
        return points[0].1;
    }
    if i == points.len() {
        return points[i - 1].1;
    }
    let ((f0, g0), (f1, g1)) = (points[i - 1], points[i]);
    let t = (f.ln() - f0.ln()) / (f1.ln() - f0.ln());
    g0 + (g1 - g0) * t
}

/// The frequencies the fit is measured at, with `cos ω` and `cos 2ω` for each, which is all a biquad's
/// magnitude needs.
struct Grid {
    hz: Vec<f64>,
    cos1: Vec<f64>,
    cos2: Vec<f64>,
}

impl Grid {
    fn new(points: usize) -> Self {
        let hz: Vec<f64> = (0..points).map(|i| LOW_HZ * (HIGH_HZ / LOW_HZ).powf(i as f64 / (points - 1) as f64)).collect();
        let w = |f: &f64| 2.0 * std::f64::consts::PI * f / RATE;
        Grid { cos1: hz.iter().map(|f| w(f).cos()).collect(), cos2: hz.iter().map(|f| (2.0 * w(f)).cos()).collect(), hz }
    }
}

#[derive(Clone, Copy, Debug)]
struct Filter {
    kind: i32,
    /// ln Hz, ln Q, dB: the three numbers the fit moves (a shelf's Q stays put).
    lnf: f64,
    lnq: f64,
    gain: f64,
}

impl Filter {
    fn band(&self) -> Band {
        Band { kind: self.kind, freq: self.lnf.exp(), gain_db: self.gain, q: self.lnq.exp(), channel: CH_BOTH }
    }

    fn hz_range(&self) -> (f64, f64) {
        match self.kind {
            LOW_SHELF => LOW_SHELF_HZ,
            HIGH_SHELF => HIGH_SHELF_HZ,
            _ => PEAK_HZ,
        }
    }

    /// Held inside where a filter may go.
    fn clamped(mut self) -> Self {
        let (lo, hi) = self.hz_range();
        self.lnf = self.lnf.clamp(lo.ln(), hi.ln());
        self.lnq = if self.kind == PEAKING { self.lnq.clamp(PEAK_Q.0.ln(), PEAK_Q.1.ln()) } else { SHELF_Q.ln() };
        self.gain = self.gain.clamp(-GAIN_DB, GAIN_DB);
        self
    }

    /// The filter's gain in dB at every point of `grid`, into `out`.
    fn response(&self, grid: &Grid, out: &mut [f64]) {
        let [b0, b1, b2, a1, a2] = band_coefficients(RATE, &self.band());
        let (n0, n1, n2) = (b0 * b0 + b1 * b1 + b2 * b2, 2.0 * (b0 * b1 + b1 * b2), 2.0 * b0 * b2);
        let (d0, d1, d2) = (1.0 + a1 * a1 + a2 * a2, 2.0 * (a1 + a1 * a2), 2.0 * a2);
        for (i, o) in out.iter_mut().enumerate() {
            let (c1, c2) = (grid.cos1[i], grid.cos2[i]);
            let num = (n0 + n1 * c1 + n2 * c2).max(1e-30);
            let den = (d0 + d1 * c1 + d2 * c2).max(1e-30);
            *o = 10.0 * (num / den).log10();
        }
    }

    /// The fit's parameter `p` (0 ln Hz, 1 ln Q, 2 dB), and whether it moves at all.
    fn get(&self, p: usize) -> f64 {
        [self.lnf, self.lnq, self.gain][p]
    }
    fn set(&mut self, p: usize, v: f64) {
        match p {
            0 => self.lnf = v,
            1 => self.lnq = v,
            _ => self.gain = v,
        }
    }
    fn moves(&self, p: usize) -> bool {
        p != 1 || self.kind == PEAKING
    }
}

/// The filters, the overall level and their responses on the grid, fitted to `target`.
struct Fit<'a> {
    grid: &'a Grid,
    target: &'a [f64],
    level: f64,
    filters: Vec<Filter>,
    /// Each filter's response on the grid, kept so a step recomputes only the filter it moves.
    responses: Vec<Vec<f64>>,
}

impl<'a> Fit<'a> {
    fn new(grid: &'a Grid, target: &'a [f64]) -> Self {
        let level = target.iter().sum::<f64>() / target.len() as f64;
        Fit { grid, target, level, filters: Vec::new(), responses: Vec::new() }
    }

    fn add(&mut self, f: Filter) {
        let f = f.clamped();
        let mut r = vec![0.0; self.grid.hz.len()];
        f.response(self.grid, &mut r);
        self.filters.push(f);
        self.responses.push(r);
    }

    /// What is left: the level and the filters, less the curve, at every point; then each filter's gain,
    /// weighed by `GAIN_COST`.
    fn residual(&self) -> Vec<f64> {
        Self::residual_of(self.target, self.level, &self.filters, &self.responses)
    }

    fn residual_of(target: &[f64], level: f64, filters: &[Filter], responses: &[Vec<f64>]) -> Vec<f64> {
        let fit = (0..target.len()).map(|i| level + responses.iter().map(|r| r[i]).sum::<f64>() - target[i]);
        fit.chain(filters.iter().map(|f| GAIN_COST * f.gain)).collect()
    }

    fn cost(residual: &[f64]) -> f64 {
        residual.iter().map(|e| e * e).sum()
    }

    /// Levenberg-Marquardt over everything that moves, at most `rounds` accepted steps.
    fn solve(&mut self, rounds: usize) {
        let n = self.target.len();
        let rows = n + self.filters.len();
        let mut free: Vec<(usize, usize)> = vec![(usize::MAX, 0)];
        for (k, f) in self.filters.iter().enumerate() {
            free.extend((0..3).filter(|&p| f.moves(p)).map(|p| (k, p)));
        }
        let m = free.len();
        let mut lambda = 1e-3;
        let mut residual = self.residual();
        let mut cost = Self::cost(&residual);
        let mut moved = vec![0.0; n];
        let mut jac = vec![0.0; rows * m];
        for _ in 0..rounds {
            // The Jacobian by forward differences, one filter's response at a time.
            for (j, &(k, p)) in free.iter().enumerate() {
                if k == usize::MAX {
                    (0..n).for_each(|i| jac[i * m + j] = 1.0);
                    continue;
                }
                let h = if p == 2 { 1e-3 } else { 1e-4 };
                let mut f = self.filters[k];
                f.set(p, f.get(p) + h);
                f.response(self.grid, &mut moved);
                (0..n).for_each(|i| jac[i * m + j] = (moved[i] - self.responses[k][i]) / h);
                (n..rows).for_each(|i| jac[i * m + j] = if p == 2 && i == n + k { GAIN_COST } else { 0.0 });
            }
            let mut a = vec![0.0; m * m];
            let mut g = vec![0.0; m];
            for i in 0..rows {
                let row = &jac[i * m..(i + 1) * m];
                for x in 0..m {
                    g[x] += row[x] * residual[i];
                    for y in x..m {
                        a[x * m + y] += row[x] * row[y];
                    }
                }
            }
            for x in 0..m {
                for y in 0..x {
                    a[x * m + y] = a[y * m + x];
                }
            }
            let mut better = false;
            for _ in 0..12 {
                let mut damped = a.clone();
                (0..m).for_each(|x| damped[x * m + x] += lambda * (a[x * m + x] + 1e-6));
                let Some(step) = solve_linear(damped, g.iter().map(|v| -v).collect(), m) else {
                    lambda *= 4.0;
                    continue;
                };
                let (level, filters) = self.stepped(&free, &step);
                let responses: Vec<Vec<f64>> = filters
                    .iter()
                    .map(|f| {
                        let mut r = vec![0.0; n];
                        f.response(self.grid, &mut r);
                        r
                    })
                    .collect();
                let trial = Self::residual_of(self.target, level, &filters, &responses);
                let c = Self::cost(&trial);
                if c < cost {
                    let gain = (cost - c) / cost.max(1e-12);
                    (self.level, self.filters, self.responses, residual, cost) = (level, filters, responses, trial, c);
                    lambda = (lambda / 3.0).max(1e-9);
                    better = gain > 1e-9;
                    break;
                }
                lambda *= 4.0;
            }
            if !better {
                break;
            }
        }
    }

    fn stepped(&self, free: &[(usize, usize)], step: &[f64]) -> (f64, Vec<Filter>) {
        let mut level = self.level;
        let mut filters = self.filters.clone();
        for (&(k, p), d) in free.iter().zip(step) {
            if k == usize::MAX {
                level += d;
            } else {
                let f = &mut filters[k];
                f.set(p, f.get(p) + d);
            }
        }
        (level, filters.into_iter().map(Filter::clamped).collect())
    }

    /// Where the error left is largest, smoothed over about a third of an octave so one narrow wiggle
    /// does not draw a peak.
    fn worst(&self) -> (f64, f64) {
        let e = self.residual();
        let n = self.target.len();
        let span = 3;
        let (lo, hi) = (PEAK_HZ.0.ln(), PEAK_HZ.1.ln());
        (0..n)
            .filter(|&i| (lo..=hi).contains(&self.grid.hz[i].ln()))
            .map(|i| {
                let (a, b) = (i.saturating_sub(span), (i + span).min(n - 1));
                (self.grid.hz[i], -e[a..=b].iter().sum::<f64>() / (b - a + 1) as f64)
            })
            .max_by(|x, y| x.1.abs().total_cmp(&y.1.abs()))
            .unwrap_or((1000.0, 0.0))
    }
}

/// `a x = b` for `x`, by Gaussian elimination with partial pivoting; none when `a` is singular.
fn solve_linear(mut a: Vec<f64>, mut b: Vec<f64>, m: usize) -> Option<Vec<f64>> {
    for col in 0..m {
        let pivot = (col..m).max_by(|&x, &y| a[x * m + col].abs().total_cmp(&a[y * m + col].abs()))?;
        if a[pivot * m + col].abs() < 1e-12 {
            return None;
        }
        if pivot != col {
            for k in 0..m {
                a.swap(col * m + k, pivot * m + k);
            }
            b.swap(col, pivot);
        }
        for row in col + 1..m {
            let factor = a[row * m + col] / a[col * m + col];
            if factor != 0.0 {
                for k in col..m {
                    a[row * m + k] -= factor * a[col * m + k];
                }
                b[row] -= factor * b[col];
            }
        }
    }
    let mut x = vec![0.0; m];
    for row in (0..m).rev() {
        let s: f64 = (row + 1..m).map(|k| a[row * m + k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row * m + row];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

fn round_to(v: f64, step: f64) -> f64 {
    (v / step).round() * step
}

/// The parametric preset closest to the curve through `points` (Hz, dB): a low shelf, eight peaks and a
/// high shelf, in the order they are written in AutoEQ's own presets, rounded the way those are written,
/// with a pre-amp that leaves no boost anywhere.
pub fn fit_graphic(points: &[(f64, f64)]) -> GraphicFit {
    let grid = Grid::new(POINTS);
    let target: Vec<f64> = grid.hz.iter().map(|&f| curve_at(points, f)).collect();
    let mut fit = Fit::new(&grid, &target);
    let shelf = |kind, hz: f64| Filter { kind, lnf: hz.ln(), lnq: SHELF_Q.ln(), gain: 0.0 };
    fit.add(shelf(LOW_SHELF, 105.0));
    fit.add(shelf(HIGH_SHELF, 10_000.0));
    fit.solve(30);
    for _ in 0..PEAKS {
        let (hz, gain) = fit.worst();
        fit.add(Filter { kind: PEAKING, lnf: hz.ln(), lnq: 1.0f64.ln(), gain });
        fit.solve(25);
    }
    fit.solve(300);

    // Written as AutoEQ writes them: whole hertz, tenths of a decibel, hundredths of Q.
    let mut filters: Vec<Filter> = fit
        .filters
        .iter()
        .map(|f| Filter { kind: f.kind, lnf: f.lnf.exp().round().ln(), lnq: round_to(f.lnq.exp(), 0.01).ln(), gain: round_to(f.gain, 0.1) }.clamped())
        .collect();
    let order = |f: &Filter| match f.kind {
        LOW_SHELF => 0,
        HIGH_SHELF => 2,
        _ => 1,
    };
    filters.sort_by(|a, b| (order(a), a.lnf).partial_cmp(&(order(b), b.lnf)).unwrap_or(std::cmp::Ordering::Equal));

    // The error of what is written, with the best level for it; the pre-amp from a finer grid.
    let mut total = vec![0.0; grid.hz.len()];
    let mut one = vec![0.0; grid.hz.len()];
    for f in &filters {
        f.response(&grid, &mut one);
        total.iter_mut().zip(&one).for_each(|(t, r)| *t += r);
    }
    let level = target.iter().zip(&total).map(|(t, h)| t - h).sum::<f64>() / target.len() as f64;
    let errors: Vec<f64> = total.iter().zip(&target).map(|(h, t)| level + h - t).collect();
    let rms = (errors.iter().map(|e| e * e).sum::<f64>() / errors.len() as f64).sqrt();
    let max = errors.iter().fold(0.0f64, |m, e| m.max(e.abs()));
    let fine = Grid::new(POINTS * 4);
    let mut peak = vec![0.0; fine.hz.len()];
    let mut one = vec![0.0; fine.hz.len()];
    for f in &filters {
        f.response(&fine, &mut one);
        peak.iter_mut().zip(&one).for_each(|(t, r)| *t += r);
    }
    let boost = peak.iter().fold(0.0f64, |m, v| m.max(*v));
    let preamp = -(boost * 10.0).ceil() / 10.0;

    let kind = |k| match k {
        LOW_SHELF => EqKind::LowShelf,
        HIGH_SHELF => EqKind::HighShelf,
        _ => EqKind::Peaking,
    };
    GraphicFit {
        preamp_db: if preamp == 0.0 { 0.0 } else { preamp as f32 },
        bands: filters.iter().map(|f| EqBand { kind: kind(f.kind), freq: f.lnf.exp().round() as f32, gain_db: f.gain as f32, q: round_to(f.lnq.exp(), 0.01) as f32 }).collect(),
        rms_db: rms as f32,
        max_db: max as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total_at(bands: &[EqBand], f: f64) -> f64 {
        let grid = Grid { hz: vec![f], cos1: vec![(2.0 * std::f64::consts::PI * f / RATE).cos()], cos2: vec![(4.0 * std::f64::consts::PI * f / RATE).cos()] };
        let mut out = [0.0];
        bands
            .iter()
            .map(|b| {
                let kind = match b.kind {
                    EqKind::LowShelf => LOW_SHELF,
                    EqKind::HighShelf => HIGH_SHELF,
                    _ => PEAKING,
                };
                Filter { kind, lnf: (b.freq as f64).ln(), lnq: (b.q as f64).ln(), gain: b.gain_db as f64 }.response(&grid, &mut out);
                out[0]
            })
            .sum()
    }

    #[test]
    fn graphic_lines_parse() {
        let p = parse_graphic("GraphicEQ: 20 -4.9; 21 -5.2;22 -5.6 ; 21 -1; nonsense; 0 3; 19000 2.5").unwrap();
        assert_eq!(p, vec![(20.0, -4.9), (21.0, -5.2), (22.0, -5.6), (19000.0, 2.5)], "sorted, repeats and nonsense dropped");
        assert!(parse_graphic("Preamp: -3 dB\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1").is_none());
        assert!(parse_graphic("graphiceq: 100 1").is_none(), "one point is not a curve");
        assert_eq!(curve_at(&p, 10.0), -4.9);
        assert_eq!(curve_at(&p, 30_000.0), 2.5);
        assert!((curve_at(&[(100.0, 0.0), (400.0, 6.0)], 200.0) - 3.0).abs() < 1e-9, "straight on a log axis");
    }

    /// A curve made by filters the fit can build comes back within a fraction of a decibel.
    #[test]
    fn a_curve_made_of_filters_round_trips() {
        let made = [
            EqBand { kind: EqKind::LowShelf, freq: 105.0, gain_db: 5.5, q: 0.7 },
            EqBand { kind: EqKind::Peaking, freq: 180.0, gain_db: -4.0, q: 0.9 },
            EqBand { kind: EqKind::Peaking, freq: 2_500.0, gain_db: 6.0, q: 2.0 },
            EqBand { kind: EqKind::Peaking, freq: 6_000.0, gain_db: -5.0, q: 3.0 },
            EqBand { kind: EqKind::HighShelf, freq: 10_000.0, gain_db: -3.0, q: 0.7 },
        ];
        // Printed the way a GraphicEQ file is, with an overall level of its own.
        let points: Vec<(f64, f64)> = (0..128).map(|i| 20.0 * 1000f64.powf(i as f64 / 127.0)).map(|f| (f, round_to(total_at(&made, f) - 7.0, 0.1))).collect();
        let fit = fit_graphic(&points);
        assert_eq!(fit.bands.len(), 2 + PEAKS);
        assert!(fit.rms_db < 0.25 && fit.max_db < 0.8, "rms {} max {}", fit.rms_db, fit.max_db);
        assert_eq!((fit.bands[0].kind, fit.bands[9].kind), (EqKind::LowShelf, EqKind::HighShelf));
        // The pre-amp takes the largest boost off, so the preset boosts nowhere.
        let boost = (0..400).map(|i| 20.0 * 1000f64.powf(i as f64 / 399.0)).map(|f| total_at(&fit.bands, f)).fold(f64::MIN, f64::max);
        assert!(fit.preamp_db <= 0.0 && boost + fit.preamp_db as f64 <= 0.05, "boost {boost} preamp {}", fit.preamp_db);
    }

    /// A smooth headphone-like curve no set of ten filters makes exactly still comes close.
    #[test]
    fn a_smooth_curve_is_followed_closely() {
        let points: Vec<(f64, f64)> = (0..128)
            .map(|i| 20.0 * 1000f64.powf(i as f64 / 127.0))
            .map(|f: f64| {
                let x = (f / 1000.0).log2();
                (f, -6.0 + 4.0 * (-(x + 3.0).powi(2)).exp() - 3.0 * (x * 1.3).sin() * (-(x * x) / 18.0).exp() + 0.6 * x)
            })
            .collect();
        let fit = fit_graphic(&points);
        assert!(fit.rms_db < 0.5 && fit.max_db < 1.5, "rms {} max {}", fit.rms_db, fit.max_db);
        assert!(fit.bands.iter().all(|b| b.freq >= 20.0 && b.freq <= 18_000.0 && b.gain_db.abs() <= 20.0 && (0.18..=6.0).contains(&b.q)));
    }

    #[test]
    fn a_flat_curve_needs_nothing() {
        let fit = fit_graphic(&[(20.0, -3.0), (20_000.0, -3.0)]);
        assert!(fit.max_db < 0.05, "max {}", fit.max_db);
        assert!(fit.bands.iter().all(|b| b.gain_db.abs() < 0.15), "{:?}", fit.bands);
    }

    /// AutoEQ's own parametric preset for a curve, read from its "Filter n: ON PK Fc .. Hz Gain .. dB Q .." lines.
    fn autoeq_parametric(text: &str) -> Vec<EqBand> {
        text.lines()
            .filter_map(|l| {
                let t: Vec<&str> = l.split_whitespace().collect();
                let at = |k: &str| t.iter().position(|w| *w == k).and_then(|i| t.get(i + 1)?.parse::<f32>().ok());
                let kind = match *t.get(3)? {
                    "LSC" => EqKind::LowShelf,
                    "HSC" => EqKind::HighShelf,
                    "PK" => EqKind::Peaking,
                    _ => return None,
                };
                Some(EqBand { kind, freq: at("Fc")?, gain_db: at("Gain")?, q: at("Q")? })
            })
            .collect()
    }

    /// The error of `bands` against the curve, with the best overall level, on the fit's own grid.
    fn error_of(bands: &[EqBand], points: &[(f64, f64)]) -> (f64, f64) {
        let grid = Grid::new(POINTS);
        let diff: Vec<f64> = grid.hz.iter().map(|&f| total_at(bands, f) - curve_at(points, f)).collect();
        let level = diff.iter().sum::<f64>() / diff.len() as f64;
        let rms = (diff.iter().map(|d| (d - level).powi(2)).sum::<f64>() / diff.len() as f64).sqrt();
        (rms, diff.iter().fold(0.0f64, |m, d| m.max((d - level).abs())))
    }

    /// Real AutoEQ curves (GraphicEQ.txt, with the ParametricEQ.txt AutoEQ fitted to the same measurement
    /// beside it): the fit follows each closely, and about as closely as AutoEQ's own ten filters.
    #[test]
    fn real_autoeq_curves_are_fitted_closely() {
        let cases = [
            ("Sony WH-1000XM6 (analog cable)", include_str!("../testdata/graphiceq/sony-wh-1000xm6-analog-cable.txt"), include_str!("../testdata/graphiceq/sony-wh-1000xm6-analog-cable.parametric.txt")),
            ("Sennheiser HD 600", include_str!("../testdata/graphiceq/sennheiser-hd-600.txt"), include_str!("../testdata/graphiceq/sennheiser-hd-600.parametric.txt")),
            ("64 Audio U12t", include_str!("../testdata/graphiceq/64-audio-u12t.txt"), include_str!("../testdata/graphiceq/64-audio-u12t.parametric.txt")),
        ];
        for (name, graphic, parametric) in cases {
            let points = parse_graphic(graphic).unwrap();
            let fit = fit_graphic(&points);
            let (theirs_rms, theirs_max) = error_of(&autoeq_parametric(parametric), &points);
            let (ours_rms, _) = error_of(&fit.bands, &points);
            println!("{name}: fitted rms {:.2} dB max {:.2} dB (AutoEQ's parametric: rms {theirs_rms:.2} max {theirs_max:.2}), preamp {} dB", fit.rms_db, fit.max_db, fit.preamp_db);
            assert!(((ours_rms - fit.rms_db as f64).abs() < 0.01), "{name}: the error reported is the preset's");
            assert!(fit.rms_db < 1.0 && fit.max_db < 4.0, "{name}: rms {} max {}", fit.rms_db, fit.max_db);
            assert!(ours_rms < theirs_rms + 0.5, "{name}: {ours_rms} against AutoEQ's {theirs_rms}");
        }
    }
}
