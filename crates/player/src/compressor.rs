//! A feed-forward compressor to even out dynamics, after Giannoulis, Massberg and Reiss, "Digital
//! Dynamic Range Compressor Design - A Tutorial and Analysis" (JAES 2012), the design they recommend:
//!
//! - the level is the frame's peak, the larger of the channels, so both sides are turned down together
//!   and the stereo image does not lean (stereo-linked);
//! - the static curve works in dB with a soft knee: below `threshold - knee/2` nothing, above
//!   `threshold + knee/2` every dB in gives `1/ratio` dB out, and a quadratic joins the two;
//! - the gain reduction the curve asks for is smoothed in dB by a *smooth decoupled peak detector*: an
//!   instant rise held by the release (`y1 = max(c, αR y1 + (1-αR) c)`), then the attack's one-pole
//!   on that (`y = αA y + (1-αA) y1`). A peak detector because a sine's zero crossings must not read as
//!   silence; decoupled so the release does not also slow the attack, and smooth so the gain has no
//!   corner where it turns from attack to release;
//! - make-up gain after it.
//!
//! The time constants are the usual ones: `α = exp(-1 / (τ fs))`, so after `τ` a step has moved 63 % of
//! the way. Below the knee with the detector at rest a frame costs a compare and a multiply; the
//! logarithm and the power are only taken while the compressor is working.

/// The compressor's controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompressorSettings {
    /// Peak level where compression starts, dBFS.
    pub threshold_db: f64,
    /// dB in per dB out above the threshold, at least 1.
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    /// Gain after the compression, dB.
    pub makeup_db: f64,
    /// Width of the soft knee around the threshold, dB; 0 is a hard knee.
    pub knee_db: f64,
}

/// The built-in settings; the client names each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CompressorPreset {
    /// Light levelling that leaves transients alone.
    Gentle,
    /// Everyday evening-out: quiet passages come up, loud ones go down.
    Balanced,
    /// Loud and even, for noisy places or low volume at night.
    Strong,
}

impl CompressorPreset {
    pub const ALL: [CompressorPreset; 3] = [CompressorPreset::Gentle, CompressorPreset::Balanced, CompressorPreset::Strong];

    /// Make-up gain is about half the reduction a peak at -6 dBFS gets, so the level stays near where it
    /// was and the limiter catches what goes over.
    pub fn settings(self) -> CompressorSettings {
        match self {
            CompressorPreset::Gentle => CompressorSettings { threshold_db: -16.0, ratio: 2.0, attack_ms: 20.0, release_ms: 250.0, makeup_db: 2.5, knee_db: 6.0 },
            CompressorPreset::Balanced => CompressorSettings { threshold_db: -20.0, ratio: 3.0, attack_ms: 10.0, release_ms: 180.0, makeup_db: 4.5, knee_db: 6.0 },
            CompressorPreset::Strong => CompressorSettings { threshold_db: -28.0, ratio: 5.0, attack_ms: 5.0, release_ms: 120.0, makeup_db: 8.0, knee_db: 8.0 },
        }
    }
}

impl Default for CompressorSettings {
    fn default() -> Self {
        CompressorPreset::Balanced.settings()
    }
}

/// How far each control goes; the settings hold them to it and so does the compressor.
pub const THRESHOLD_DB: (f64, f64) = (-60.0, 0.0);
pub const RATIO: (f64, f64) = (1.0, 20.0);
pub const ATTACK_MS: (f64, f64) = (0.1, 200.0);
pub const RELEASE_MS: (f64, f64) = (10.0, 2000.0);
pub const MAKEUP_DB: (f64, f64) = (0.0, 24.0);
pub const KNEE_DB: (f64, f64) = (0.0, 24.0);

fn held(v: f64, (lo, hi): (f64, f64), fallback: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { fallback }
}

impl CompressorSettings {
    /// Every control inside its range; anything that is not a number is the default's.
    pub fn held(self) -> CompressorSettings {
        let d = CompressorSettings::default();
        CompressorSettings {
            threshold_db: held(self.threshold_db, THRESHOLD_DB, d.threshold_db),
            ratio: held(self.ratio, RATIO, d.ratio),
            attack_ms: held(self.attack_ms, ATTACK_MS, d.attack_ms),
            release_ms: held(self.release_ms, RELEASE_MS, d.release_ms),
            makeup_db: held(self.makeup_db, MAKEUP_DB, d.makeup_db),
            knee_db: held(self.knee_db, KNEE_DB, d.knee_db),
        }
    }

    /// The static curve: the gain reduction (dB, 0 or more) for a steady peak level of `x_db`.
    pub fn reduction_db(&self, x_db: f64) -> f64 {
        let (t, w) = (self.threshold_db, self.knee_db);
        let slope = 1.0 - 1.0 / self.ratio;
        let over = x_db - t;
        if 2.0 * over <= -w {
            0.0
        } else if 2.0 * over.abs() <= w && w > 0.0 {
            slope * (over + w / 2.0).powi(2) / (2.0 * w)
        } else {
            slope * over
        }
    }
}

/// The compressor's running state. Its gain moves by itself, so retuning it while music plays is
/// smooth; only make-up gain would step, and the chain fades over that like any other change.
#[derive(Clone, Debug)]
pub struct Compressor {
    s: CompressorSettings,
    /// Level below which the curve is certainly 0 (the bottom of the knee), linear.
    quiet: f64,
    attack: f64,
    release: f64,
    makeup: f64,
    /// The detector's two stages, dB of reduction.
    y1: f64,
    y: f64,
    /// Largest reduction in the buffer just processed, dB; the meter.
    pub meter_db: f64,
}

impl Compressor {
    pub fn new(rate: f64, s: CompressorSettings) -> Self {
        let mut c = Compressor { s, quiet: 0.0, attack: 0.0, release: 0.0, makeup: 1.0, y1: 0.0, y: 0.0, meter_db: 0.0 };
        c.tune(rate, s);
        c
    }

    /// New settings, the detector's state kept.
    pub fn tune(&mut self, rate: f64, s: CompressorSettings) {
        let s = s.held();
        let coeff = |ms: f64| (-1.0 / (ms / 1000.0 * rate.max(1.0))).exp();
        self.s = s;
        self.quiet = 10f64.powf((s.threshold_db - s.knee_db / 2.0) / 20.0);
        self.attack = coeff(s.attack_ms);
        self.release = coeff(s.release_ms);
        self.makeup = 10f64.powf(s.makeup_db / 20.0);
    }

    pub fn settings(&self) -> CompressorSettings {
        self.s
    }

    /// One frame, all channels, turned down together.
    #[inline]
    pub fn frame(&mut self, f: &mut [f64]) {
        let peak = f.iter().fold(0f64, |m, v| m.max(v.abs()));
        let c = if peak > self.quiet { self.s.reduction_db(20.0 * peak.log10()) } else { 0.0 };
        self.y1 = c.max(self.release * self.y1 + (1.0 - self.release) * c);
        self.y = self.attack * self.y + (1.0 - self.attack) * self.y1;
        if self.y < 1e-9 {
            // At rest: settle to exactly nothing, and skip the power.
            self.y = 0.0;
            if self.makeup != 1.0 {
                f.iter_mut().for_each(|v| *v *= self.makeup);
            }
            return;
        }
        self.meter_db = self.meter_db.max(self.y);
        let g = self.makeup * 10f64.powf(-self.y / 20.0);
        f.iter_mut().for_each(|v| *v *= g);
    }

    pub fn reset(&mut self) {
        (self.y1, self.y, self.meter_db) = (0.0, 0.0, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn hard(threshold_db: f64, ratio: f64) -> CompressorSettings {
        CompressorSettings { threshold_db, ratio, attack_ms: 5.0, release_ms: 100.0, makeup_db: 0.0, knee_db: 0.0 }
    }

    /// A stereo sine at `db` dBFS peak for `secs`, through `c`: the output's gain in dB over its last
    /// tenth, and the gain frame by frame (from the envelope of the left channel).
    fn run(c: &mut Compressor, db: f64, secs: f64, freq: f64) -> (f64, Vec<f64>) {
        let a = 10f64.powf(db / 20.0);
        let n = (secs * RATE) as usize;
        let mut gains = Vec::with_capacity(n);
        let mut out_peak = 0f64;
        for i in 0..n {
            let x = a * (std::f64::consts::TAU * freq * i as f64 / RATE).sin();
            let mut f = [x, x];
            c.frame(&mut f);
            gains.push(if x.abs() > 1e-12 { f[0] / x } else { f64::NAN });
            if i >= n - n / 10 {
                out_peak = out_peak.max(f[0].abs());
            }
        }
        (20.0 * (out_peak / a).log10(), gains)
    }

    #[test]
    fn the_static_curve_is_threshold_ratio_and_knee() {
        let s = hard(-20.0, 4.0);
        assert_eq!(s.reduction_db(-30.0), 0.0);
        assert_eq!(s.reduction_db(-20.0), 0.0);
        assert!((s.reduction_db(-8.0) - 9.0).abs() < 1e-12, "12 dB over at 4:1 is 3 dB over: 9 dB off");
        let soft = CompressorSettings { knee_db: 8.0, ..s };
        assert_eq!(soft.reduction_db(-24.0), 0.0, "the bottom of the knee");
        assert!((soft.reduction_db(-16.0) - 3.0).abs() < 1e-12, "the top of the knee joins the line");
        let mid = soft.reduction_db(-20.0);
        assert!(mid > 0.0 && mid < 3.0, "inside the knee it bends: {mid}");
        // Continuous, and never more reduction than the hard knee's line would give past the knee.
        for x in -30..0 {
            let (a, b) = (soft.reduction_db(x as f64), soft.reduction_db(x as f64 + 0.01));
            assert!(b >= a && b - a < 0.01, "at {x} dB");
        }
        assert_eq!(CompressorSettings { ratio: 1.0, ..s }.reduction_db(0.0), 0.0, "1:1 does nothing");
    }

    #[test]
    fn steady_tones_come_out_on_the_curve() {
        let s = CompressorSettings { knee_db: 6.0, ..hard(-20.0, 3.0) };
        for db in [-40.0, -22.0, -20.0, -12.0, -6.0, 0.0] {
            let mut c = Compressor::new(RATE, s);
            let (gain, _) = run(&mut c, db, 2.0, 220.0);
            let want = -s.reduction_db(db);
            assert!((gain - want).abs() < 0.2, "{db} dBFS: {gain} dB, the curve says {want}");
        }
    }

    #[test]
    fn attack_and_release_take_their_time() {
        let s = CompressorSettings { attack_ms: 10.0, release_ms: 200.0, ..hard(-30.0, 4.0) };
        let mut c = Compressor::new(RATE, s);
        // Quiet, then a step up to -6 dBFS: 18 dB of reduction once it has settled.
        run(&mut c, -40.0, 0.5, 1000.0);
        assert_eq!(c.y, 0.0, "nothing below the threshold");
        let (_, g) = run(&mut c, -6.0, 0.5, 1000.0);
        let reduction = |g: &[f64], ms: f64| {
            let at = (ms / 1000.0 * RATE) as usize;
            // The largest gain around there (a sample near a zero crossing reads nothing).
            -20.0 * g[at..at + 48].iter().filter(|v| v.is_finite()).fold(0f64, |m, v| m.max(*v)).log10()
        };
        let full = 18.0;
        let at_attack = reduction(&g, 10.0);
        assert!((at_attack / full - 0.63).abs() < 0.08, "after one attack time {at_attack} dB of {full}");
        assert!(reduction(&g, 1.0) < full * 0.2, "not at once");
        assert!((reduction(&g, 100.0) - full).abs() < 0.2, "settled");
        // Back to quiet: the release lets go over its own time, far slower than the attack.
        let (_, g) = run(&mut c, -40.0, 1.0, 1000.0);
        let r = |ms: f64| reduction(&g, ms);
        assert!(r(20.0) > full * 0.8, "20 ms into the release it still holds {}", r(20.0));
        let at_release = r(200.0);
        assert!(at_release > full * 0.25 && at_release < full * 0.5, "after one release time {at_release} dB of {full}");
        assert!(r(900.0) < 0.3, "and it lets go: {}", r(900.0));
    }

    #[test]
    fn both_channels_are_turned_down_together() {
        let mut c = Compressor::new(RATE, hard(-20.0, 4.0));
        for i in 0..48_000 {
            let x = (std::f64::consts::TAU * 440.0 * i as f64 / RATE).sin();
            let mut f = [x, 0.01 * x];
            c.frame(&mut f);
            if i > 24_000 && x.abs() > 0.1 {
                assert!((f[1] / f[0] - 0.01).abs() < 1e-9, "the quiet side follows the loud one");
            }
        }
    }

    #[test]
    fn below_the_knee_it_is_only_the_makeup() {
        let mut c = Compressor::new(RATE, CompressorSettings { makeup_db: 0.0, ..hard(-10.0, 4.0) });
        let mut f = [0.1, -0.05];
        for _ in 0..1000 {
            f = [0.1, -0.05];
            c.frame(&mut f);
        }
        assert_eq!(f, [0.1, -0.05], "bit for bit");
        let mut c = Compressor::new(RATE, CompressorSettings { makeup_db: 6.0206, ..hard(-10.0, 4.0) });
        let mut f = [0.1, -0.05];
        c.frame(&mut f);
        assert!((f[0] - 0.2).abs() < 1e-4 && (f[1] + 0.1).abs() < 1e-4);
    }

    #[test]
    fn the_presets_even_out_loud_and_quiet() {
        for p in CompressorPreset::ALL {
            let s = p.settings();
            assert_eq!(s.held(), s, "{p:?} is inside the ranges");
            // A quiet passage and a loud one end up closer together than they went in.
            let (mut a, mut b) = (Compressor::new(RATE, s), Compressor::new(RATE, s));
            let (quiet, _) = run(&mut a, -30.0, 2.0, 220.0);
            let (loud, _) = run(&mut b, -3.0, 2.0, 220.0);
            let spread = (-3.0 + loud) - (-30.0 + quiet);
            assert!(spread < 27.0 - 4.0, "{p:?}: 27 dB apart went in, {spread} came out");
        }
    }

    #[test]
    fn bad_settings_are_held() {
        let s = CompressorSettings { threshold_db: f64::NAN, ratio: 0.2, attack_ms: -1.0, release_ms: f64::INFINITY, makeup_db: 99.0, knee_db: -3.0 }.held();
        assert_eq!(s.threshold_db, CompressorSettings::default().threshold_db);
        assert_eq!((s.ratio, s.attack_ms, s.release_ms, s.makeup_db, s.knee_db), (1.0, 0.1, CompressorSettings::default().release_ms, 24.0, 0.0));
    }
}
