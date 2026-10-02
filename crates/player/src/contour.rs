//! Volume-dependent loudness compensation from the ISO 226:2003 equal-loudness contours.
//!
//! Music is assumed balanced at a reference level (default 80 phon) at full volume and heard that many
//! dB lower as the volume drops ([`listening_phon`]). The missing response is the difference of the two
//! contours relative to 1 kHz ([`compensation_db`]), drawn by a least-squares-fitted low shelf and high
//! shelf ([`design`]; the small mid dip is ignored) with a pre-gain that prevents clipping. Computed on
//! volume or setting changes, never per buffer.

use crate::dsp::{band_coefficients, Band, CH_BOTH, HIGH_SHELF_SLOPE, LOW_SHELF_SLOPE};

/// ISO 226:2003 table 1: the frequencies, the exponent of loudness perception `αf`, the magnitude of the
/// linear transfer function normalised at 1 kHz `Lu` (dB) and the threshold of hearing `Tf` (dB).
const FREQ: [f64; 29] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0,
    2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0, 10000.0, 12500.0,
];
const ALPHA: [f64; 29] = [
    0.532, 0.506, 0.480, 0.455, 0.432, 0.409, 0.387, 0.367, 0.349, 0.330, 0.315, 0.301, 0.288, 0.276, 0.267, 0.259, 0.253, 0.250, 0.246,
    0.244, 0.243, 0.243, 0.243, 0.242, 0.242, 0.245, 0.254, 0.271, 0.301,
];
const LU: [f64; 29] = [
    -31.6, -27.2, -23.0, -19.1, -15.9, -13.0, -10.3, -8.1, -6.2, -4.5, -3.1, -2.0, -1.1, -0.4, 0.0, 0.3, 0.5, 0.0, -2.7, -4.1, -1.0, 1.7,
    2.5, 1.2, -2.1, -7.1, -11.2, -10.7, -3.1,
];
const TF: [f64; 29] = [
    78.5, 68.7, 59.5, 51.1, 44.0, 37.5, 31.5, 26.5, 22.1, 17.9, 14.4, 11.4, 8.6, 6.2, 4.4, 3.0, 2.2, 2.4, 3.5, 1.7, -1.3, -4.2, -6.0, -5.4,
    -1.5, 6.0, 12.6, 13.9, 12.3,
];

/// Loudness compensation settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    /// Phon, [`REFERENCE_PHON`].
    pub reference_phon: f64,
    /// 0 at full volume, negative below ([`volume_db`]).
    pub volume_db: f64,
}

/// Range the contours are defined for, phon.
pub const PHON: (f64, f64) = (20.0, 90.0);
/// Selectable reference levels, phon.
pub const REFERENCE_PHON: (f64, f64) = (60.0, 90.0);
/// Largest shelf boost, dB.
pub const MAX_DB: f64 = 18.0;

/// dB SPL at the table's `i`th frequency for `phon` (ISO 226:2003 4.1).
fn spl_at(i: usize, phon: f64) -> f64 {
    let af = 4.47e-3 * (10f64.powf(0.025 * phon) - 1.15) + (0.4 * 10f64.powf((TF[i] + LU[i]) / 10.0 - 9.0)).powf(ALPHA[i]);
    10.0 / ALPHA[i] * af.log10() - LU[i] + 94.0
}

/// The equal-loudness contour: dB SPL at `freq` as loud as 1 kHz at `phon`. Interpolated in log
/// frequency, held past the table's ends.
pub fn spl_db(phon: f64, freq: f64) -> f64 {
    let phon = phon.clamp(PHON.0, PHON.1);
    if !(freq > FREQ[0]) {
        return spl_at(0, phon);
    }
    if freq >= FREQ[28] {
        return spl_at(28, phon);
    }
    let i = FREQ.partition_point(|&f| f <= freq) - 1;
    let x = (freq / FREQ[i]).ln() / (FREQ[i + 1] / FREQ[i]).ln();
    spl_at(i, phon) + x * (spl_at(i + 1, phon) - spl_at(i, phon))
}

/// Boost needed at `freq` to hear `reference`-phon music at `listening` phon, dB (0 at 1 kHz).
pub fn compensation_db(listening: f64, reference: f64, freq: f64) -> f64 {
    let l = listening.clamp(PHON.0, PHON.1);
    let r = reference.clamp(PHON.0, PHON.1);
    (spl_db(l, freq) - l) - (spl_db(r, freq) - r)
}

/// Listening level: `reference` lowered by the volume attenuation, within [`PHON`].
pub fn listening_phon(reference: f64, volume_db: f64) -> f64 {
    let v = if volume_db.is_finite() { volume_db.min(0.0) } else { 0.0 };
    (reference + v).clamp(PHON.0, PHON.1)
}

/// Volume in dB for step `index` of `max`: `platform_db` (Android's `getStreamVolumeDb`) when valid,
/// else AOSP's `DEFAULT_MEDIA_VOLUME_CURVE` interpolated linearly. Step 0 reads as the curve's bottom.
pub fn volume_db(index: i32, max: i32, platform_db: f32) -> f64 {
    if platform_db.is_finite() && platform_db <= 0.0 {
        return (platform_db as f64).max(-96.0);
    }
    if max <= 0 {
        return 0.0;
    }
    const CURVE: [(f64, f64); 4] = [(1.0, -58.0), (20.0, -40.0), (60.0, -17.0), (100.0, 0.0)];
    let pct = (index.clamp(0, max) as f64 / max as f64 * 100.0).max(1.0);
    let k = CURVE.iter().position(|p| p.0 >= pct).unwrap_or(CURVE.len() - 1).max(1);
    let ((x0, y0), (x1, y1)) = (CURVE[k - 1], CURVE[k]);
    y0 + (pct - x0) / (x1 - x0) * (y1 - y0)
}

/// The shelves for one listening level (absent below 0.05 dB) and the pre-gain undoing their peak boost.
#[derive(Clone, Debug, PartialEq)]
pub struct Shelves {
    pub low: Option<Band>,
    pub high: Option<Band>,
    pub pre_db: f64,
}

/// Fitting rate (other chain rates shift the result by a fraction of a dB).
const FIT_RATE: f64 = 48_000.0;
/// Fit spans (Hz), candidate corners and slopes: a gentle low shelf, the steepest non-overshooting high one.
const LOW_FIT: (f64, f64) = (31.5, 1000.0);
const LOW_CORNERS: [f64; 6] = [80.0, 100.0, 125.0, 160.0, 200.0, 250.0];
const LOW_SLOPE: f64 = 0.4;
const HIGH_FIT: (f64, f64) = (4000.0, 12_500.0);
const HIGH_CORNERS: [f64; 4] = [6300.0, 8000.0, 10_000.0, 12_500.0];
const HIGH_SLOPE: f64 = 1.0;

fn db_at(rate: f64, band: &Band, freq: f64) -> f64 {
    let c = band_coefficients(rate, band);
    let w = std::f64::consts::TAU * freq / rate;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let (nr, ni) = (c[0] + c[1] * c1 + c[2] * c2, -(c[1] * s1 + c[2] * s2));
    let (dr, di) = (1.0 + c[3] * c1 + c[4] * c2, -(c[3] * s1 + c[4] * s2));
    10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).max(1e-30).log10()
}

/// Extra high-shelf fit points above the table (held at 12.5 kHz), so the fit cannot keep rising
/// above the table's end.
const ABOVE_TABLE: [f64; 2] = [16_000.0, 20_000.0];

/// Fits a shelf to `target` over the table points in `span` plus `extra`: per corner the least-squares
/// gain (the dB response scales almost linearly with gain), keeping the corner with the least error.
fn fit(kind: i32, corners: &[f64], slope: f64, span: (f64, f64), extra: &[f64], target: impl Fn(f64) -> f64) -> Option<Band> {
    let points: Vec<f64> = FREQ.iter().copied().filter(|f| *f >= span.0 && *f <= span.1).chain(extra.iter().copied()).collect();
    let want: Vec<f64> = points.iter().map(|f| target(*f)).collect();
    let mut best: Option<(f64, Band)> = None;
    for &corner in corners {
        let unit = Band { kind, freq: corner, gain_db: 6.0, q: slope, channel: CH_BOTH };
        let shape: Vec<f64> = points.iter().map(|f| db_at(FIT_RATE, &unit, *f) / 6.0).collect();
        let den: f64 = shape.iter().map(|s| s * s).sum();
        let gain = (shape.iter().zip(&want).map(|(s, w)| s * w).sum::<f64>() / den.max(1e-12)).max(0.0);
        let band = Band { gain_db: gain, ..unit };
        let err: f64 = points.iter().zip(&want).map(|(f, w)| (db_at(FIT_RATE, &band, *f) - w).powi(2)).sum();
        if best.as_ref().is_none_or(|(e, _)| err < *e) {
            best = Some((err, band));
        }
    }
    // Cap after choosing the corner: capping first would push the corner up into the low mids.
    best.map(|b| Band { gain_db: b.1.gain_db.min(MAX_DB), ..b.1 }).filter(|b| b.gain_db >= 0.05)
}

/// The shelves for `reference` phon music heard `volume_db` down.
pub fn design(reference: f64, volume_db: f64) -> Shelves {
    let reference = if reference.is_finite() { reference.clamp(REFERENCE_PHON.0, REFERENCE_PHON.1) } else { 80.0 };
    let listening = listening_phon(reference, volume_db);
    if reference - listening < 0.05 {
        return Shelves { low: None, high: None, pre_db: 0.0 };
    }
    let c = |f: f64| compensation_db(listening, reference, f);
    let low = fit(LOW_SHELF_SLOPE, &LOW_CORNERS, LOW_SLOPE, LOW_FIT, &[], c);
    let high = fit(HIGH_SHELF_SLOPE, &HIGH_CORNERS, HIGH_SLOPE, HIGH_FIT, &ABOVE_TABLE, c);
    // Peak combined boost on a sixth-octave grid, 20 Hz up.
    let bands: Vec<&Band> = low.iter().chain(high.iter()).collect();
    let most = (0..=60).map(|k| 20.0 * 2f64.powf(k as f64 / 6.0)).map(|f| bands.iter().map(|b| db_at(FIT_RATE, b, f)).sum::<f64>()).fold(0.0, f64::max);
    Shelves { low, high, pre_db: -(most * 10.0).ceil() / 10.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contours_match_iso_226() {
        // At 1 kHz a phon is a dB SPL, by definition.
        for phon in [20.0, 40.0, 60.0, 80.0, 90.0] {
            assert!((spl_db(phon, 1000.0) - phon).abs() < 0.1, "{phon} phon at 1 kHz: {}", spl_db(phon, 1000.0));
        }
        // Published points of the 2003 contours (the standard's own figure and its common tables).
        assert!((spl_db(40.0, 20.0) - 99.85).abs() < 0.1, "40 phon at 20 Hz: {}", spl_db(40.0, 20.0));
        assert!((spl_db(40.0, 100.0) - 64.4).abs() < 0.2, "40 phon at 100 Hz: {}", spl_db(40.0, 100.0));
        assert!((spl_db(80.0, 100.0) - 92.5).abs() < 0.2, "80 phon at 100 Hz: {}", spl_db(80.0, 100.0));
        // The ear is keenest near 3-4 kHz: a phon there is fewer dB than at 1 kHz.
        assert!(spl_db(60.0, 3500.0) < 60.0);
        // Between the table's points it is read smoothly, and held past its ends.
        let (a, b, m) = (spl_db(40.0, 100.0), spl_db(40.0, 125.0), spl_db(40.0, 112.0));
        assert!(m < a && m > b);
        assert_eq!(spl_db(40.0, 20_000.0), spl_db(40.0, 12_500.0));
        assert_eq!(spl_db(40.0, 5.0), spl_db(40.0, 20.0));
    }

    #[test]
    fn compensation_follows_volume() {
        assert!(compensation_db(80.0, 80.0, 50.0).abs() < 1e-9, "at the reference level nothing is missing");
        assert!(compensation_db(40.0, 80.0, 1000.0).abs() < 0.05, "and 1 kHz is where it is measured from");
        let bass = |l: f64| compensation_db(l, 80.0, 100.0);
        assert!((bass(40.0) - 11.9).abs() < 0.3, "40 against 80 phon at 100 Hz: {}", bass(40.0));
        assert!(bass(30.0) > bass(50.0) && bass(50.0) > bass(70.0) && bass(70.0) > 0.0);
        assert!(compensation_db(40.0, 80.0, 12_500.0) > 4.0, "the top octave a little");
        assert_eq!(listening_phon(80.0, -30.0), 50.0);
        assert_eq!(listening_phon(80.0, -90.0), PHON.0, "never under the contours");
        assert_eq!(listening_phon(80.0, 6.0), 80.0, "no louder than all the way up");
        assert_eq!(listening_phon(80.0, f64::NAN), 80.0);

        // Volume in db.
        assert_eq!(volume_db(7, 15, -12.5), -12.5, "the platform's own figure first");
        assert_eq!(volume_db(15, 15, f32::NAN), 0.0, "all the way up");
        assert!((volume_db(3, 15, f32::NAN) - (-40.0)).abs() < 1e-9, "a fifth of the way: the curve's 20 % point");
        assert!((volume_db(9, 15, f32::NAN) - (-17.0)).abs() < 1e-9, "60 %");
        assert_eq!(volume_db(0, 15, f32::NAN), -58.0, "off reads as the bottom");
        assert!(volume_db(8, 15, f32::NAN) < volume_db(9, 15, f32::NAN));
        assert_eq!(volume_db(5, 0, f32::NAN), 0.0, "no steps to read: all the way up");
    }

    /// The largest error of the two shelves against the compensation where they are fitted, dB.
    fn worst(reference: f64, volume_db: f64) -> (f64, Shelves) {
        let s = design(reference, volume_db);
        let l = listening_phon(reference, volume_db);
        let bands: Vec<Band> = s.low.iter().chain(s.high.iter()).cloned().collect();
        let worst = FREQ
            .iter()
            .chain(ABOVE_TABLE.iter())
            .filter(|f| (**f >= LOW_FIT.0 && **f <= 500.0) || **f >= 8000.0)
            .map(|f| (bands.iter().map(|b| db_at(FIT_RATE, b, *f)).sum::<f64>() - compensation_db(l, reference, *f)).abs())
            .fold(0.0, f64::max);
        (worst, s)
    }

    #[test]
    fn shelves_fit_compensation() {
        let none = design(80.0, 0.0);
        assert_eq!(none, Shelves { low: None, high: None, pre_db: 0.0 }, "all the way up: nothing");
        // Down to 50 phon the shelves fit; below, the bass exceeds MAX_DB.
        for volume in [-10.0, -20.0, -30.0] {
            let (w, s) = worst(80.0, volume);
            assert!(w < 3.0, "{volume} dB down: {w} dB off in the bass or the top");
            // The pre-gain pays back the most the two add anywhere from 20 Hz to 20 kHz.
            let peak = (0..2000).map(|k| 20.0 * 1000f64.powf(k as f64 / 1999.0)).map(|f| s.low.iter().chain(s.high.iter()).map(|b| db_at(FIT_RATE, b, f)).sum::<f64>()).fold(0.0, f64::max);
            assert!(s.pre_db < 0.0 && peak + s.pre_db <= 0.05, "{volume} dB: a {peak} dB boost against a {} dB pre-gain", s.pre_db);
        }
        // Quieter is more.
        let gain = |v: f64| design(80.0, v).low.map_or(0.0, |b| b.gain_db);
        assert!(gain(-40.0) > gain(-20.0) && gain(-20.0) > gain(-5.0));
        // A lower reference means less to make up at the same volume.
        assert!(design(70.0, -30.0).low.unwrap().gain_db < design(90.0, -30.0).low.unwrap().gain_db);
        // Held to its range: past about 40 phon the bass would want more than it is given.
        assert_eq!(design(90.0, -200.0).low.unwrap().gain_db, MAX_DB);
        assert!(design(90.0, -200.0).pre_db >= -MAX_DB - 0.1);
        assert_eq!(design(f64::NAN, -20.0), design(80.0, -20.0));
        // Capped, the shelf keeps a bass corner rather than climbing into the low middle to make up the area.
        for v in [-40.0, -50.0, -58.0] {
            let low = design(80.0, v).low.unwrap();
            assert!(low.freq <= 160.0, "{v} dB down: corner at {} Hz", low.freq);
        }
    }
}
