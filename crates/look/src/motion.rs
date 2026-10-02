//! Seek bar animation ([`SeekPace`]). [`seek_step`] is the older easing, still exposed over JNI for the
//! benchmarks.

/// Easing time constant, seconds.
const EASE_S: f32 = 0.14;
/// Below this gap the bar snaps to the target in one draw.
const SETTLED_PX: f32 = 2.0;
/// Below this gap the bar does not move (absorbs position jitter).
const STILL_PX: f32 = 0.5;
/// Wait bounds between draws while keeping up: one frame to one second.
const MIN_WAIT_MS: i32 = 16;
const MAX_WAIT_MS: i32 = 1000;

/// Eases `bar` towards `target` (both 0..1) after `dt_s`. `speed` is bar lengths per second (0 when
/// paused). Returns the new bar and the wait before the next step: 0 = next frame, ms once caught up
/// (one draw per pixel), -1 = paused and settled. The -1 sentinel is kept for the packed JNI return.
pub fn seek_step(bar: f32, target: f32, dt_s: f32, width_px: f32, speed: f32) -> (f32, i32) {
    let width = width_px.max(1.0);
    let gap_px = (target - bar) * width;
    let next = if gap_px.abs() < STILL_PX {
        bar
    } else if gap_px.abs() < SETTLED_PX {
        target
    } else {
        let eased = bar + (target - bar) * (1.0 - (-dt_s / EASE_S).exp());
        // Snap instead of leaving a sub-threshold remainder for another frame.
        if ((target - eased) * width).abs() < SETTLED_PX { target } else { return (eased, 0) }
    };
    if speed <= 0.0 {
        return (next, -1);
    }
    let ahead_px = ((target - next) * width).clamp(0.0, 1.0);
    let px_s = 1.0 / (width * speed);
    (next, (((1.0 - ahead_px) * px_s * 1000.0) as i32).clamp(MIN_WAIT_MS, MAX_WAIT_MS))
}

/// Glide and cross-fade duration after a jump (new song, seek, crossfade handover).
pub(crate) const GLIDE_S: f32 = 0.32;
/// A move this many pixels beyond normal playback progress counts as a jump and glides.
const JUMP_PX: f32 = 3.0;
/// A position this far from the predicted one cross-fades the time labels.
const JUMP_MS: i64 = 1_500;

/// Smoothstep.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Seek bar position and time labels. During normal playback the bar tracks the song exactly, redrawn
/// once per pixel or label second. A jump glides the bar over [`GLIDE_S`] (chasing the moving target)
/// and cross-fades the labels. Call [`SeekPace::sync`] when a page reappears so it does not glide from a
/// stale position.
#[derive(Debug, Clone, PartialEq)]
pub struct SeekPace {
    bar: f32,
    /// Bar position where the current glide started.
    glide_from: Option<f32>,
    glide_s: f32,
    /// Song position at the last step, 0..1.
    target: f32,
    /// Position and duration the labels show.
    at_ms: i64,
    duration_ms: i64,
    /// Position and duration of the labels fading out.
    fade_from: Option<(i64, i64)>,
    fade_s: f32,
    synced: bool,
}

impl Default for SeekPace {
    fn default() -> Self {
        SeekPace::new()
    }
}

fn fraction(position_ms: i64, duration_ms: i64) -> f32 {
    (position_ms.max(0) as f64 / duration_ms.max(1) as f64).clamp(0.0, 1.0) as f32
}

fn times(position_ms: i64, duration_ms: i64) -> (i64, i64) {
    let at = position_ms.clamp(0, duration_ms.max(0));
    (at / 1000, (duration_ms - at).max(0) / 1000)
}

impl SeekPace {
    pub const fn new() -> Self {
        SeekPace { bar: 0.0, glide_from: None, glide_s: 0.0, target: 0.0, at_ms: 0, duration_ms: 0, fade_from: None, fade_s: 0.0, synced: false }
    }

    /// Jumps straight to the song's position with no glide or fade.
    pub fn sync(&mut self, position_ms: i64, duration_ms: i64) {
        let bar = fraction(position_ms, duration_ms);
        *self = SeekPace { bar, target: bar, at_ms: position_ms.max(0), duration_ms, synced: true, ..SeekPace::new() };
    }

    /// Holds the bar at `bar` (0..1), e.g. where a released scrub left it.
    pub fn hold(&mut self, bar: f32, position_ms: i64, duration_ms: i64) {
        self.sync(position_ms, duration_ms);
        self.bar = bar.clamp(0.0, 1.0);
        self.target = self.bar;
    }

    /// Advances by `dt_s` with the song at `position_ms` of `duration_ms`, playing at `rate` (0: paused).
    /// Returns the wait before the next step: 0 = next frame, ms while playing, -1 = idle until the
    /// inputs change (JNI-packed).
    pub fn step(&mut self, position_ms: i64, duration_ms: i64, dt_s: f32, width_px: f32, rate: f32) -> i32 {
        if !self.synced {
            self.sync(position_ms, duration_ms);
        }
        let width = width_px.max(1.0);
        let dt = dt_s.max(0.0);
        let rate = rate.max(0.0);
        let target = fraction(position_ms, duration_ms);
        let played_ms = (dt * 1000.0 * rate) as i64;
        let expected_px = if duration_ms > 0 { played_ms as f32 / duration_ms as f32 * width } else { 0.0 };

        // Labels: a new duration or an unexpected position starts a cross-fade.
        let predicted = self.at_ms + played_ms;
        if duration_ms != self.duration_ms || (position_ms - predicted).abs() >= JUMP_MS {
            if times(self.at_ms, self.duration_ms) != times(position_ms, duration_ms) {
                self.fade_from = Some((self.at_ms, self.duration_ms));
                self.fade_s = 0.0;
            }
        } else if self.fade_from.is_some() {
            self.fade_s += dt;
        }
        if self.fade_s >= GLIDE_S {
            self.fade_from = None;
        }
        self.at_ms = position_ms.max(0);
        self.duration_ms = duration_ms;

        // Bar: a jump starts a glide from the drawn position (restarting any glide in progress).
        let gap_px = (target - self.bar) * width;
        let gliding = self.glide_from.is_some();
        let jumped_again = gliding && ((target - self.target) * width).abs() > JUMP_PX + expected_px;
        if gliding && !jumped_again {
            self.glide_s += dt;
        }
        self.target = target;
        if jumped_again || !gliding && gap_px.abs() > JUMP_PX + expected_px {
            self.glide_from = Some(self.bar);
            self.glide_s = 0.0;
        }
        if let Some(from) = self.glide_from {
            let t = self.glide_s / GLIDE_S;
            if t >= 1.0 {
                self.bar = target;
                self.glide_from = None;
            } else {
                // The glide's end follows the moving song.
                self.bar = from + (target - from) * ease(t);
                return 0;
            }
        } else if gap_px.abs() >= STILL_PX {
            self.bar = target;
        }
        if self.fade_from.is_some() {
            return 0;
        }
        if rate <= 0.0 {
            return -1;
        }
        // Next draw at the next pixel or label second, whichever comes first.
        let ahead_px = ((target - self.bar) * width).clamp(0.0, 1.0);
        let px_ms = if duration_ms > 0 { duration_ms as f32 / (width * rate) } else { MAX_WAIT_MS as f32 };
        let pixel = (1.0 - ahead_px) * px_ms;
        let second = (1000 - position_ms.rem_euclid(1000)) as f32 / rate;
        (pixel.min(second) as i32).clamp(MIN_WAIT_MS, MAX_WAIT_MS)
    }

    /// Drawn bar position, 0..1.
    pub fn bar(&self) -> f32 {
        self.bar
    }

    /// Whether a glide or fade is in progress.
    pub fn moving(&self) -> bool {
        self.glide_from.is_some() || self.fade_from.is_some()
    }

    /// Label times (elapsed, remaining) in whole seconds.
    pub fn times(&self) -> (i64, i64) {
        times(self.at_ms, self.duration_ms)
    }

    /// Outgoing label times and the incoming labels' eased opacity, during a cross-fade.
    pub fn fading(&self) -> Option<((i64, i64), f32)> {
        self.fade_from.map(|(at, duration)| (times(at, duration), ease(self.fade_s / GLIDE_S)))
    }

    /// Opacity of the current labels, 0..1.
    pub fn fade(&self) -> f32 {
        self.fading().map_or(1.0, |(_, f)| f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_steps() {
        // Far from the target: ease, next frame.
        let (b, w) = seek_step(0.0, 0.5, 0.016, 1000.0, 1.0 / 366.0);
        assert!(b > 0.0 && b < 0.5 && w == 0);
        // One pixel behind: snap; a 366 s song over 1000 px waits ~366 ms.
        let (b, w) = seek_step(0.5, 0.501, 0.016, 1000.0, 1.0 / 366.0);
        assert_eq!((b, w), (0.501, 366));
        assert_eq!(seek_step(0.3, 0.3, 0.016, 1000.0, 0.0), (0.3, -1));
        // Waits clamp to one frame and one second.
        assert_eq!(seek_step(0.3, 0.3, 0.016, 3000.0, 1.0).1, MIN_WAIT_MS);
        assert_eq!(seek_step(0.3, 0.3, 0.016, 10.0, 0.0001).1, MAX_WAIT_MS);

        // Seek step ignores sub pixel jitter.
        // A quarter pixel either way: stay, and wait for the rest of the pixel.
        let (b, w) = seek_step(0.5, 0.50025, 0.4, 1000.0, 1.0 / 366.0);
        assert_eq!(b, 0.5);
        assert!((270..=280).contains(&w), "{w}");
        assert_eq!(seek_step(0.5, 0.49975, 0.4, 1000.0, 1.0 / 366.0).0, 0.5);

        // Seek step draws once per pixel.
        // 366 s over 900 px, stepped with the returned waits: no sub-pixel draws.
        let (width, secs) = (900.0f32, 366.0f32);
        let speed = 1.0 / secs;
        let (mut bar, mut t, mut draws) = (0.2f32, 0.0f32, 0);
        let mut dt = 0.016;
        while t < 10.0 {
            t += dt;
            let target = 0.2 + t * speed;
            let (next, wait) = seek_step(bar, target, dt, width, speed);
            if next != bar {
                draws += 1;
                assert!((next - bar) * width >= STILL_PX, "a sub-pixel draw at {t}");
            }
            bar = next;
            dt = if wait > 0 { wait as f32 / 1000.0 } else { 0.016 };
        }
        // 10 s is ~24.6 px.
        assert!((20..=28).contains(&draws), "{draws} draws");
    }

    /// Steps `pace` at 60 fps for `secs` with the song at `pos(t)`; returns the bar after each step.
    fn frames(pace: &mut SeekPace, secs: f32, duration_ms: i64, pos: impl Fn(f32) -> i64) -> Vec<f32> {
        let mut out = Vec::new();
        let mut t = 0.0;
        while t < secs {
            t += 0.016;
            pace.step(pos(t), duration_ms, 0.016, 1000.0, 1.0);
            out.push(pace.bar());
        }
        out
    }

    #[test]
    fn playback_and_labels() {
        let mut p = SeekPace::new();
        p.sync(100_000, 200_000);
        // Stepped with the returned waits: never more than a pixel behind.
        let (mut t, mut dt) = (0.0f32, 0.016f32);
        while t < 20.0 {
            t += dt;
            let pos = 100_000 + (t * 1000.0) as i64;
            let wait = p.step(pos, 200_000, dt, 1000.0, 1.0);
            assert!(((fraction(pos, 200_000) - p.bar()) * 1000.0).abs() <= 1.01, "lagging at {t}");
            assert!(!p.moving(), "treated as a jump at {t}");
            assert!(wait > 0);
            dt = wait as f32 / 1000.0;
        }
        assert_eq!(p.times(), (120, 79));

        // Wait ends at next label second.
        let mut p = SeekPace::new();
        p.sync(10_000, 3_600_000);
        // An hour over 1000 px is 3.6 s per pixel; the labels still tick each second.
        assert_eq!(p.step(10_250, 3_600_000, 0.25, 1000.0, 1.0), 750);
    }

    #[test]
    fn new_song_glide() {
        let mut p = SeekPace::new();
        p.sync(150_000, 200_000);
        let bars = frames(&mut p, 0.6, 180_000, |t| (t * 1000.0) as i64);
        let mut last = 0.75f32;
        for (i, b) in bars.iter().enumerate() {
            assert!((last - b) * 1000.0 <= 80.0, "teleport at frame {i}: {last} -> {b}");
            last = *b;
        }
        assert!(bars[0] > 0.7, "first frame starts from the old position: {}", bars[0]);
        let landed = bars.iter().position(|b| *b < 0.01).unwrap();
        assert!((15..=22).contains(&landed), "landed at frame {landed}");
        assert!(!p.moving());
        assert_eq!(p.times(), (0, 179));

        // Labels fade from old times.
        let mut p = SeekPace::new();
        p.sync(63_000, 395_000);
        assert_eq!(p.step(0, 259_000, 0.016, 1000.0, 1.0), 0);
        let ((old_at, old_left), f) = p.fading().unwrap();
        assert_eq!((old_at, old_left), (63, 332));
        assert!(f < 0.05, "{f}");
        assert_eq!(p.times(), (0, 259));
        frames(&mut p, 0.4, 259_000, |t| (t * 1000.0) as i64);
        assert!(p.fading().is_none());
        assert_eq!(p.fade(), 1.0);

        // Sync skips glide.
        let mut p = SeekPace::new();
        p.sync(150_000, 200_000);
        p.sync(4_000, 180_000);
        assert_eq!(p.bar(), fraction(4_000, 180_000));
        assert!(!p.moving());
        assert!(p.step(4_016, 180_000, 0.016, 1000.0, 1.0) > 0);
        assert!(!p.moving());
    }

    #[test]
    fn seeks_glide() {
        let mut p = SeekPace::new();
        p.sync(150_000, 200_000);
        frames(&mut p, 0.1, 200_000, |_| 0);
        let mid = p.bar();
        assert!(mid > 0.1 && mid < 0.7, "{mid}");
        let bars = frames(&mut p, 0.5, 200_000, |_| 180_000);
        assert!((bars[0] - mid).abs() * 1000.0 <= 80.0, "{mid} -> {}", bars[0]);
        assert_eq!(p.bar(), 0.9);

        // Paused seek glides then stops.
        let mut p = SeekPace::new();
        p.sync(50_000, 200_000);
        assert_eq!(p.step(50_000, 200_000, 0.016, 1000.0, 0.0), -1);
        let mut waits = Vec::new();
        for _ in 0..40 {
            waits.push(p.step(100_000, 200_000, 0.016, 1000.0, 0.0));
        }
        assert_eq!(waits[0], 0);
        assert_eq!(*waits.last().unwrap(), -1);
        assert_eq!(p.bar(), 0.5);

        // Small seek does not glide.
        let mut p = SeekPace::new();
        p.sync(100_000, 200_000);
        // +0.5 s over 1000 px is 2.5 px, under the jump threshold.
        p.step(100_516, 200_000, 0.016, 1000.0, 1.0);
        assert!(!p.moving());
    }

}
