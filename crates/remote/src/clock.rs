//! Time between devices. A device says where its song was together with when, on its own clock
//! ([`now_us`]); a controller learns how that clock stands to its own from time exchanges, as NTP does:
//! it sends at `t1`, the device receives at `t2` and answers at `t3`, the answer arrives at `t4`. Of the
//! exchanges kept, those with the shortest round trips are the ones least delayed one way more than the
//! other: a line through their offsets, the faster weighing more, gives the offset and the clocks'
//! drift. One more exchange moves it a little, never by what one exchange's delays say.

use std::collections::VecDeque;

/// This device's clock for remote timing, µs: monotonic, and running on while the device sleeps
/// (CLOCK_BOOTTIME on Android and Linux, which Android's `SystemClock.elapsedRealtimeNanos` reads).
/// On Apple's systems CLOCK_MONOTONIC_RAW, `mach_continuous_time`: their CLOCK_MONOTONIC is the wall
/// clock less the boot time, so NTP's corrections slew it, and `mach_absolute_time` (CLOCK_UPTIME_RAW,
/// the audio timestamps' clock) stops while the device sleeps.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast, reason = "time_t and c_long are 32 bits on 32-bit Android")]
pub fn now_us() -> i64 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(target_vendor = "apple")]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC_RAW;
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `t` is a valid timespec to write into.
    unsafe { libc::clock_gettime(CLOCK, &mut t) };
    t.tv_sec as i64 * 1_000_000 + t.tv_nsec as i64 / 1_000
}

#[cfg(not(unix))]
pub fn now_us() -> i64 {
    // The clock's start: a static, as every caller on the device reads the one clock.
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_micros() as i64
}

/// One time exchange, µs: sent (`t1`) and answered (`t4`) on the controller's clock, received (`t2`) and
/// answered (`t3`) on the device's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exchange {
    pub t1: i64,
    pub t2: i64,
    pub t3: i64,
    pub t4: i64,
}

impl Exchange {
    /// The device's clock minus the controller's, exact when both ways took as long.
    fn offset(&self) -> i64 {
        ((self.t2 - self.t1) + (self.t3 - self.t4)) / 2
    }

    /// Time on the way there and back.
    fn round_trip(&self) -> i64 {
        (self.t4 - self.t1) - (self.t3 - self.t2)
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    /// The controller's time halfway through the exchange.
    at: i64,
    offset: i64,
    round_trip: i64,
}

/// Exchanges kept: at one every [`EVERY_US`], drift is followed over the last few minutes.
const KEPT: usize = 32;

/// Exchanges a controller makes when it starts mirroring, and then one this often while it does.
pub const BURST: usize = 8;
pub const EVERY_US: i64 = 15_000_000;

/// Drift is reckoned once the exchanges used span this long (the burst and two more); before that, the
/// clocks are taken as running at one pace.
const DRIFT_SPAN_US: i64 = 30_000_000;

/// Exchanges whose round trip took this much (or half again) more than the fastest's are not used, µs.
const NEAR_US: i64 = 2_000;

/// An exchange this far off the estimate, beyond what its own round trip explains, means the device's
/// clock started again (it restarted): what was learned of the old one is dropped.
const JUMP_US: i64 = 100_000;

/// How a device's clock stands to this one's, from the time exchanges with it.
#[derive(Debug, Clone, Default)]
pub struct ClockSync {
    samples: VecDeque<Sample>,
}

impl ClockSync {
    /// The offset after the startup burst can discard delayed exchanges.
    pub fn settled_offset_at(&self, at: i64) -> Option<i64> {
        (self.samples.len() >= BURST).then(|| self.offset_at(at)).flatten()
    }

    pub fn add(&mut self, e: Exchange) {
        let s = Sample { at: (e.t1 + e.t4) / 2, offset: e.offset(), round_trip: e.round_trip().max(0) };
        if self.offset_at(s.at).is_some_and(|o| (s.offset - o).abs() > s.round_trip + JUMP_US) {
            self.samples.clear();
        }
        if self.samples.len() == KEPT {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }

    /// The device's clock minus this one's, at this one's time `at`; None before any exchange.
    pub fn offset_at(&self, at: i64) -> Option<i64> {
        let fastest = self.samples.iter().map(|s| s.round_trip).min()?;
        // An exchange's offset is off by at most half what its round trip took beyond the fastest's:
        // the slower ones are left out, the rest weigh less the slower they were.
        let near = fastest + (fastest / 2).max(NEAR_US);
        let good: Vec<(f64, f64, f64)> = self.samples.iter().filter(|s| s.round_trip <= near).map(|s| (s.at as f64, s.offset as f64, 1.0 / ((s.round_trip - fastest + 1_000) as f64).powi(2))).collect();
        let total: f64 = good.iter().map(|g| g.2).sum();
        let mean_at = good.iter().map(|g| g.2 * g.0).sum::<f64>() / total;
        let mean = good.iter().map(|g| g.2 * g.1).sum::<f64>() / total;
        let span = good.iter().map(|g| g.0).fold(f64::MIN, f64::max) - good.iter().map(|g| g.0).fold(f64::MAX, f64::min);
        // The drift, once the exchanges span long enough to tell it from their jitter.
        let (mut num, mut den) = (0.0, 0.0);
        for g in good.iter().filter(|_| span >= DRIFT_SPAN_US as f64) {
            num += g.2 * (g.0 - mean_at) * (g.1 - mean);
            den += g.2 * (g.0 - mean_at).powi(2);
        }
        let slope = if den > 0.0 { num / den } else { 0.0 };
        Some((mean + slope * (at as f64 - mean_at)).round() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Times exchanges go out as a controller sends them: a burst from `start`, then one every
    /// [`EVERY_US`], `n` in all.
    fn sent(start: i64, n: usize) -> impl Iterator<Item = i64> {
        (0..n as i64).map(move |k| if k < BURST as i64 { start + k * 250_000 } else { start + (k - BURST as i64 + 1) * EVERY_US })
    }

    /// A controller and a device whose clock reads `skew` µs more and runs `ppm` faster, exchanging at
    /// `times`, with each way's delay picked by `delays` (there, back) for exchange `k`.
    fn exchanges(skew: i64, ppm: f64, times: impl Iterator<Item = i64>, delays: impl Fn(usize) -> (i64, i64)) -> (ClockSync, impl Fn(i64) -> i64) {
        let device = move |t: i64| t + skew + (t as f64 * ppm / 1e6) as i64;
        let mut sync = ClockSync::default();
        for (k, t1) in times.enumerate() {
            let (there, back) = delays(k);
            let t2 = device(t1 + there);
            let t3 = t2 + 300;
            let t4 = t1 + there + 300 + back;
            sync.add(Exchange { t1, t2, t3, t4 });
        }
        (sync, device)
    }

    /// A small deterministic jitter, 0 to `max` µs.
    fn jitter(k: usize, salt: u64, max: i64) -> i64 {
        let x = (k as u64 + 1).wrapping_mul(6364136223846793005).wrapping_add(salt).rotate_left(29);
        (x % (max as u64 + 1)) as i64
    }

    #[test]
    fn a_skewed_clock_is_found_through_lopsided_delays() {
        // Seven seconds ahead; the answers wait up to 400 ms longer than the commands, but for a quarter of them.
        let (sync, device) = exchanges(7_000_000, 0.0, sent(1_000_000, BURST), |k| (2_000 + jitter(k, 1, 3_000), 2_000 + if k % 4 == 1 { 0 } else { 100_000 + jitter(k, 2, 300_000) }));
        let at = 2_000_000;
        let err = sync.offset_at(at).unwrap() - (device(at) - at);
        assert!(err.abs() <= 2_000, "{err} µs off");
    }

    #[test]
    fn drift_is_followed() {
        // (ppm, exchanges, read this long after the last, most off µs). 40 ppm is 2.4 ms a minute; 400
        // ppm (an emulator's clock) is 6 ms between two exchanges.
        for (ppm, n, after, most) in [(40.0, 40, 60_000_000, 1_500), (400.0, BURST + 3, EVERY_US, 1_500), (-400.0, BURST + 20, EVERY_US, 1_500)] {
            let (sync, device) = exchanges(-3_000_000, ppm, sent(0, n), |k| (1_000 + jitter(k, 3, 2_000), 1_000 + jitter(k, 4, 2_000)));
            let at = sent(0, n).last().unwrap() + after;
            let err = sync.offset_at(at).unwrap() - (device(at) - at);
            assert!(err.abs() <= most, "{ppm} ppm, {n} exchanges: {err} µs off");
        }
    }

    #[test]
    fn each_exchange_moves_the_offset_little() {
        // Through a relay: 30 to 70 ms either way, two answers in three waiting up to 200 ms more for a
        // poll. An exchange faster than those before may move it by what it shows; the rest hardly do.
        let device = |t: i64| t + 5_000_000 + (t as f64 * 40.0 / 1e6) as i64;
        let mut sync = ClockSync::default();
        let mut moves = Vec::new();
        for (k, t1) in sent(1_000_000, 60).enumerate() {
            let there = 30_000 + jitter(k, 1, 40_000);
            let back = 30_000 + jitter(k, 2, 40_000) + if k % 3 == 0 { 0 } else { jitter(k, 3, 200_000) };
            let t2 = device(t1 + there);
            let was = sync.offset_at(t1);
            sync.add(Exchange { t1, t2, t3: t2 + 300, t4: t1 + there + 300 + back });
            let offset = sync.offset_at(t1).unwrap();
            let err = offset - (device(t1) - t1);
            assert!(err.abs() <= 20_000, "exchange {k}: {err} µs off");
            if let Some(was) = was.filter(|_| k >= BURST) {
                moves.push((offset - was).abs());
            }
        }
        let mean = moves.iter().sum::<i64>() / moves.len() as i64;
        let most = moves.iter().max().unwrap();
        assert!(mean <= 2_000 && *most <= 15_000, "moved {mean} µs on average, {most} µs at most");
    }

    #[test]
    fn a_restarted_clock_is_learned_again() {
        let (mut sync, _) = exchanges(5_000_000, 0.0, sent(0, BURST), |_| (1_000, 1_000));
        let t1 = 10_000_000;
        sync.add(Exchange { t1, t2: t1 + 1_000 + 42, t3: t1 + 1_300 + 42, t4: t1 + 2_300 });
        assert_eq!(sync.offset_at(t1), Some(42));
        assert_eq!(ClockSync::default().offset_at(t1), None);
    }
}
