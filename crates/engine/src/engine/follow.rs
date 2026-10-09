//! Following another device's playback place for place (a jam guest listening along). The leader's
//! place rules: this engine starts where the leader is heard at the moment its own music is heard, and
//! starts again where it is when it strays far or the leader jumps. In between it stays in step as a
//! clock loop does: the output slips a frame in or leaves one out now and then ([`crate::output`]), at
//! the rate this device drifts from the leader (learned from the gaps over the last minute), plus a
//! little to close a gap beyond a few ms. Song time is never moved: the place heard stays exact.
//! Here are the decisions; the worker carries them out.

use std::collections::VecDeque;

use crate::output::Slipped;

/// Most closing a gap adds to the slip either way, ms a ms.
pub(super) const CLOSE_MAX: f64 = 0.005;
/// Most the drift is made up for either way, ms a ms.
const DRIFT_MAX: f64 = 0.003;
/// Gaps within this are left to the drift's slip alone, ms.
const DEADBAND_MS: f64 = 1.0;
/// A gap past this once what is written is heard is closed from what the output can still replace
/// (written again), not only from what is written next, ms.
const REMAKE_MS: f64 = 2.0;
/// Slips closer than this to the one written are not changed to.
const SLIP_STEP: f64 = 0.000_02;
/// While in step the place is read this often, µs.
const LOOK_EVERY_US: i64 = 2_000_000;
/// The drift is learned from the readings of this long, µs, once they span [`DRIFT_FROM_US`].
const DRIFT_OVER_US: i64 = 60_000_000;
const DRIFT_FROM_US: i64 = 8_000_000;
/// Readings this far off where the last runs on to start the drift's readings afresh, ms.
const STEP_MS: f64 = 3.0;
/// The gap now is the median of this many readings, each run on to now.
const LEVEL_OF: usize = 3;
/// Further than this from the leader, ms, the place is started again rather than slipped back.
const STRAY_MS: f64 = 150.0;
/// After a start the place heard is judged only once two readings this far apart agree (an output's
/// first readings after it starts are estimates: Android's before its first timestamp), µs.
const SETTLE_US: i64 = 250_000;
/// Readings agreeing within this have settled, ms.
const SETTLED_MS: f64 = 3.0;
/// A start into a mix heard this far off its place once the mix is over is made again, ms.
const MIXED_OFF_MS: f64 = 15.0;
/// Most a start is planned ahead of its moment for an output slow to start, µs.
const START_DELAY_MAX_US: i64 = 1_000_000;
/// The leader's place moving this far from where its last word runs on is a jump there (a seek, a
/// skipped silence), ms: followed at once.
const LEAP_MS: f64 = 30.0;
/// Words of the leader this soon after a start move its place without being a jump, µs.
const LEAP_QUIET_US: i64 = 1_500_000;
/// From deciding to start to the music being heard: the song opened, decoded and in the output, µs;
/// twice as long after each start whose music was not ready in time, up to [`START_LEAD_MAX_US`].
pub(super) const START_LEAD_US: i64 = 300_000;
const START_LEAD_MAX_US: i64 = 4_800_000;
/// On another song than the leader's for this long (its word on a song change comes late), µs, this
/// one starts on the leader's.
const ELSEWHERE_US: i64 = 600_000;

/// The leader's playback: song `index` (in this engine's queue) was at `ms` at `at_us` on the engine's
/// clock, moving at `rate`.
#[derive(Debug, Clone, PartialEq)]
pub struct Led {
    pub index: usize,
    pub ms: f64,
    pub at_us: i64,
    pub rate: f64,
    pub playing: bool,
}

impl Led {
    /// Where the leader's listener is at `us`, ms.
    pub fn place_at(&self, us: i64) -> f64 {
        if self.playing {
            self.ms + (us - self.at_us) as f64 / 1000.0 * self.rate
        } else {
            self.ms
        }
    }
}

/// What this engine hears now.
#[derive(Debug, Clone, Copy)]
pub(super) struct Here {
    pub playing: bool,
    /// The song and place heard.
    pub at: Option<(usize, f64)>,
    /// A mix is heard.
    pub mixing: bool,
    /// Time the output slipped in (less what it left out) since the last start: heard, and written but
    /// not heard yet; and how long until what is written now is heard, ms.
    pub slipped: Slipped,
    /// What the device holds (which is not written again), ms.
    pub held_ms: f64,
}

/// What to do now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Step {
    Stay,
    Pause,
    /// Start song `index` at `ms`, heard from `at_us`.
    Start { index: usize, ms: f64, at_us: i64 },
    /// Slip `rate` frames in per frame written from now on (leave them out when below zero), and
    /// `owed_ms` besides at most [`CLOSE_MAX`] more; with `remake`, from the first frame the output can
    /// still replace.
    Slip { rate: f64, owed_ms: f64, remake: bool },
}

/// A start under way: the music made ready from its place, heard from `at_us`; while the music playing
/// fades down, the song, place and moment to make it ready at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Starting {
    pub at_us: i64,
    pub jump: Option<(usize, f64, i64)>,
}

/// The place heard after a start, as far as it can be trusted.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Settling {
    Settled,
    /// Not read since the start.
    Started,
    /// Read at `at_us`, `gap` ms from the leader with `slipped` ms slipped in: compared with the next
    /// reading.
    Read { at_us: i64, gap: f64, slipped: f64 },
}

#[derive(Debug)]
pub(super) struct Following {
    pub led: Led,
    /// The leader jumped since this one last started.
    leapt: bool,
    /// The slip written from now on.
    pub slip: f64,
    /// How much faster this one's place runs than the leader's by itself, ms a ms: what the slip makes
    /// up for.
    drift: f64,
    /// Since the place settled after the last start: when it was read, µs, and the gap with the time
    /// slipped in until then, ms (which moves only by the drift).
    readings: VecDeque<(i64, f64)>,
    pub starting: Option<Starting>,
    /// When to look again, µs.
    pub look_at: Option<i64>,
    /// When this one last started, µs.
    started_at: i64,
    /// How long after its moment a start is heard (an output starting: a sound server, Bluetooth), as
    /// the settled place heard after a start showed it, µs.
    start_delay_us: i64,
    /// Whether the place heard since the last start can be judged.
    settling: Settling,
    /// How long a start is planned ahead.
    start_lead_us: i64,
    /// A mix was heard before the last start was looked at.
    mixed: bool,
    /// Since when this one plays another song than the leader's, µs.
    elsewhere: Option<i64>,
}

impl Following {
    pub fn new(led: Led) -> Following {
        Following { led, leapt: true, slip: 0.0, drift: 0.0, readings: VecDeque::new(), starting: None, look_at: None, started_at: i64::MIN / 2, start_delay_us: 0, settling: Settling::Settled, mixed: false, start_lead_us: START_LEAD_US, elsewhere: None }
    }

    /// The last start's music was not ready at its moment (the network slow to bring it): the next is
    /// planned further ahead.
    pub fn not_ready(&mut self) {
        self.start_lead_us = (self.start_lead_us * 2).min(START_LEAD_MAX_US);
    }

    /// The last start began on time.
    pub fn started(&mut self) {
        self.start_lead_us = START_LEAD_US;
    }

    /// The leader's newer word, at `now_us`.
    pub fn lead(&mut self, led: Led, now_us: i64) {
        let old = std::mem::replace(&mut self.led, led);
        let new = &self.led;
        // Just after a start the leader's own words still settle (its output after a seek): the newest
        // is followed by slipping, not by starting again.
        let settled = now_us - self.started_at > LEAP_QUIET_US;
        self.leapt |= settled && old.playing && new.playing && old.index == new.index && (new.place_at(now_us) - old.place_at(now_us)).abs() > LEAP_MS;
        // Another pace there moves the gap once, as each side hears it: not a drift.
        if old.rate != new.rate {
            self.readings.clear();
        }
    }

    /// Where to start now: the leader's song and its place when the music made now is heard.
    fn start(&mut self, now_us: i64) -> Step {
        self.leapt = false;
        self.look_at = None;
        self.elsewhere = None;
        let at_us = now_us + self.start_lead_us;
        self.started_at = at_us;
        self.settling = Settling::Started;
        self.readings.clear();
        self.mixed = false;
        Step::Start { index: self.led.index, ms: self.led.place_at(at_us + self.start_delay_us), at_us }
    }

    /// The step at `now_us` for what is heard `here`.
    pub fn step(&mut self, now_us: i64, here: Here) -> Step {
        if !self.led.playing {
            self.starting = None;
            self.leapt = true;
            return if here.playing { Step::Pause } else { Step::Stay };
        }
        if self.starting.is_some() {
            return Step::Stay;
        }
        let Some((index, ms)) = here.at.filter(|_| here.playing) else { return self.start(now_us) };
        if self.leapt {
            return self.start(now_us);
        }
        if index != self.led.index {
            // Mixing into the leader's song, or about to: its word on the change may be on its way.
            let since = *self.elsewhere.get_or_insert(now_us);
            if now_us - since >= ELSEWHERE_US && !here.mixing {
                return self.start(now_us);
            }
            self.look_at = Some(since + ELSEWHERE_US);
            return Step::Stay;
        }
        self.elsewhere = None;
        // Through a mix the place heard is the seek bar's reckoning: looked at again once it is over (a
        // start into a mix is judged then, its gap the mix's, not the output's).
        if here.mixing {
            self.mixed |= self.settling != Settling::Settled;
            return Step::Stay;
        }
        let gap = ms - self.led.place_at(now_us);
        let slipped = here.slipped.heard_ms;
        if self.settling != Settling::Settled {
            // The gap moves by the drift and the slip since the reading compared with.
            let agrees = |at: i64, was: f64, was_slipped: f64| (gap + slipped - was - was_slipped - self.drift * (now_us - at) as f64 / 1000.0).abs() <= SETTLED_MS;
            match self.settling {
                Settling::Read { at_us: at, gap: was, slipped: was_slipped } if now_us - at >= SETTLE_US && agrees(at, was, was_slipped) => {
                    self.settling = Settling::Settled;
                    if std::mem::take(&mut self.mixed) {
                        // A start into a mix lands as the mix's reckoning had it: off that, it starts
                        // again out of it.
                        if gap.abs() > MIXED_OFF_MS {
                            return self.start(now_us);
                        }
                    } else {
                        // How late (or early) the start was heard: the next one allows for it.
                        self.start_delay_us = (self.start_delay_us - (gap * 1000.0) as i64).clamp(0, START_DELAY_MAX_US);
                    }
                }
                Settling::Read { at_us: at, .. } if now_us - at < SETTLE_US => {
                    self.look_at = Some(at + SETTLE_US);
                    return Step::Stay;
                }
                _ => {
                    self.settling = Settling::Read { at_us: now_us, gap, slipped };
                    self.look_at = Some(now_us + SETTLE_US);
                    return Step::Stay;
                }
            }
        }
        if let Some(&(at, _)) = self.readings.back().filter(|r| now_us - r.0 < LOOK_EVERY_US) {
            self.look_at = Some(at + LOOK_EVERY_US);
            return Step::Stay;
        }
        self.look_at = Some(now_us + LOOK_EVERY_US);
        self.read(now_us, gap + slipped);
        // The gap now, steadier than one reading.
        let mut level: Vec<f64> = self.readings.iter().rev().take(LEVEL_OF).map(|&(at, u)| u + self.drift * (now_us - at) as f64 / 1000.0 - slipped).collect();
        level.sort_by(f64::total_cmp);
        let level = level[level.len() / 2];
        if level.abs() > STRAY_MS {
            return self.start(now_us);
        }
        // The gap once what is written now and what is owed are heard.
        let Slipped { ahead_ms, owed_ms, written_ms, .. } = here.slipped;
        let then = level + self.drift * written_ms - ahead_ms - owed_ms;
        // Frames slipped put the place back by the song time they would have carried.
        let rate = (self.drift / self.led.rate).clamp(-DRIFT_MAX, DRIFT_MAX);
        if then.abs() > REMAKE_MS {
            // Owed from as the device's music ends (its slips taken as spread evenly).
            let held = here.held_ms.min(written_ms);
            let at = level + self.drift * held - ahead_ms * held / written_ms.max(1.0);
            return Step::Slip { rate, owed_ms: at, remake: true };
        }
        if then.abs() >= DEADBAND_MS {
            return Step::Slip { rate, owed_ms: owed_ms + then, remake: false };
        }
        if (rate - self.slip).abs() >= SLIP_STEP {
            return Step::Slip { rate, owed_ms, remake: false };
        }
        Step::Stay
    }

    /// Notes `u`, the gap with the time slipped in, read at `now_us`, and learns the drift from the
    /// readings since the last step: the slope of a least squares line through them.
    fn read(&mut self, now_us: i64, u: f64) {
        // A step (the output's own delay said anew, the leader's clock learned better) is not drift.
        if self.readings.back().is_some_and(|&(at, last)| (u - last - self.drift * (now_us - at) as f64 / 1000.0).abs() > STEP_MS) {
            self.readings.clear();
        }
        self.readings.push_back((now_us, u));
        while self.readings.front().is_some_and(|r| now_us - r.0 > DRIFT_OVER_US) {
            self.readings.pop_front();
        }
        let first = self.readings[0].0;
        if now_us - first < DRIFT_FROM_US {
            return;
        }
        let n = self.readings.len() as f64;
        let (mt, mu) = self.readings.iter().fold((0.0, 0.0), |(t, u), r| (t + (r.0 - first) as f64 / 1000.0 / n, u + r.1 / n));
        let (num, den) = self.readings.iter().fold((0.0, 0.0), |(num, den), r| {
            let dt = (r.0 - first) as f64 / 1000.0 - mt;
            (num + dt * (r.1 - mu), den + dt * dt)
        });
        if den > 0.0 {
            self.drift = num / den;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn led(ms: f64, at_us: i64) -> Led {
        Led { index: 2, ms, at_us, rate: 1.25, playing: true }
    }

    fn here(at: Option<(usize, f64)>) -> Here {
        Here { playing: at.is_some(), at, mixing: false, slipped: Slipped::default(), held_ms: 0.0 }
    }

    #[test]
    fn starts_where_the_leader_is_when_it_is_heard() {
        let mut f = Following::new(led(10_000.0, 0));
        assert_eq!(f.step(1_000_000, here(None)), Step::Start { index: 2, ms: 11_250.0 + START_LEAD_US as f64 / 1000.0 * 1.25, at_us: 1_000_000 + START_LEAD_US });
    }

    #[test]
    fn a_start_not_ready_in_time_is_planned_further_ahead() {
        let mut f = Following::new(led(10_000.0, 0));
        f.not_ready();
        assert!(matches!(f.step(0, here(None)), Step::Start { at_us, .. } if at_us == 2 * START_LEAD_US));
        f.started();
        f.starting = None;
        assert!(matches!(f.step(0, here(None)), Step::Start { at_us, .. } if at_us == START_LEAD_US));
    }

    #[test]
    fn small_gaps_are_slipped_away_and_large_ones_started_again() {
        // (ms ahead of the leader, what is done): ahead, a frame is slipped in now and then (it plays
        // slower); behind, one left out.
        let owes = |owed: f64, ms: f64| (owed - ms).abs() < 0.01;
        let cases: &[(f64, &dyn Fn(Step) -> bool)] = &[
            (1.5, &|s| matches!(s, Step::Slip { owed_ms, remake: false, .. } if owes(owed_ms, 1.5))),
            (-1.5, &|s| matches!(s, Step::Slip { owed_ms, remake: false, .. } if owes(owed_ms, -1.5))),
            (20.0, &|s| matches!(s, Step::Slip { owed_ms, remake: true, .. } if owes(owed_ms, 20.0))),
            (0.3, &|s| s == Step::Stay),
            (200.0, &|s| matches!(s, Step::Start { .. })),
        ];
        for (ahead, done) in cases {
            let mut f = Following::new(led(10_000.0, 0));
            f.leapt = false;
            let step = f.step(1_000_000, here(Some((2, 11_250.0 + ahead))));
            assert!(done(step), "{ahead} ms ahead: {step:?}");
        }
    }

    #[test]
    fn a_slip_still_to_be_heard_counts() {
        let mut f = Following::new(led(10_000.0, 0));
        f.leapt = false;
        // 20 ms ahead, but 12 ms are slipped in already in what the device holds, and 8 more owed: in step
        // once those are heard.
        let slipped = Slipped { heard_ms: 0.0, ahead_ms: 12.0, owed_ms: 8.0, written_ms: 7_000.0 };
        assert_eq!(f.step(1_000_000, Here { slipped, ..here(Some((2, 11_270.0))) }), Step::Stay);
    }

    #[test]
    fn the_drift_is_learned_and_slipped_away() {
        // This one's place runs 300 ppm fast by itself, 5 ms ahead to begin with.
        let mut f = Following::new(Led { rate: 1.0, ..led(0.0, 0) });
        f.leapt = false;
        let (mut gap, mut heard, mut owed, mut now) = (5.0, 0.0, 0.0, 0);
        for _ in 0..90 {
            let slipped = Slipped { heard_ms: heard, owed_ms: owed, ..Slipped::default() };
            if let Step::Slip { rate, owed_ms, .. } = f.step(now, Here { slipped, ..here(Some((2, gap + now as f64 / 1000.0))) }) {
                (f.slip, owed) = (rate, owed_ms);
            }
            let paid: f64 = owed.clamp(-CLOSE_MAX * 2_000.0, CLOSE_MAX * 2_000.0);
            owed -= paid;
            heard += f.slip * 2_000.0 + paid;
            gap += 0.0003 * 2_000.0 - f.slip * 2_000.0 - paid;
            now += 2_000_000;
        }
        assert!(gap.abs() < DEADBAND_MS, "{gap} ms off");
        assert!((f.drift - 0.0003).abs() < 0.000_02, "drift {}", f.drift);
    }

    #[test]
    fn a_leap_is_followed_at_once_and_a_song_change_after_a_grace() {
        let mut f = Following::new(led(10_000.0, 0));
        f.leapt = false;
        f.lead(led(10_040.0, 0), 1_000_000);
        assert!(matches!(f.step(1_000_000, here(Some((2, 11_250.0)))), Step::Start { .. }), "the leader jumped 40 ms");
        f.lead(Led { index: 3, ..led(0.0, 2_000_000) }, 2_000_000);
        assert_eq!(f.step(2_100_000, here(Some((2, 13_000.0)))), Step::Stay, "its own mix may still be coming");
        assert!(matches!(f.step(2_100_000 + ELSEWHERE_US, here(Some((2, 13_700.0)))), Step::Start { index: 3, .. }));
    }

    #[test]
    fn a_start_is_judged_once_its_place_settles() {
        let mut f = Following::new(led(10_000.0, 0));
        let Step::Start { ms, at_us, .. } = f.step(0, here(None)) else { panic!() };
        // The output's first reading has it on time; a quarter second later it says 120 ms late, and
        // again later: settled. Slipped back, not started again (each start is a gap).
        let heard_at = |us: i64, late: f64| here(Some((2, ms + (us - at_us) as f64 / 1000.0 * 1.25 - late)));
        assert_eq!(f.step(at_us, heard_at(at_us, 0.0)), Step::Stay);
        assert_eq!(f.step(at_us + SETTLE_US, heard_at(at_us + SETTLE_US, 120.0)), Step::Stay, "not settled yet");
        assert_eq!(f.look_at, Some(at_us + 2 * SETTLE_US));
        assert!(matches!(f.step(at_us + 2 * SETTLE_US, heard_at(at_us + 2 * SETTLE_US, 120.0)), Step::Slip { owed_ms, remake: true, .. } if (owed_ms + 120.0).abs() < 0.01));
        // The next start allows for how late this one was heard.
        f.lead(led(20_000.0, 5_000_000), 5_000_000);
        f.leapt = true;
        let Step::Start { ms: again, at_us: at2, .. } = f.step(5_000_000, heard_at(5_000_000, 120.0)) else { panic!() };
        assert!((again - f.led.place_at(at2 + 120_000)).abs() < 0.01, "{again}");
    }

    #[test]
    fn a_paused_leader_pauses_and_resumes_with_a_start() {
        let mut f = Following::new(led(10_000.0, 0));
        f.leapt = false;
        f.lead(Led { playing: false, ..led(10_500.0, 400_000) }, 1_000_000);
        assert_eq!(f.step(1_000_000, here(Some((2, 11_250.0)))), Step::Pause);
        f.lead(led(10_500.0, 2_000_000), 2_000_000);
        assert!(matches!(f.step(2_000_000, here(None)), Step::Start { ms, .. } if (ms - (10_500.0 + 375.0)).abs() < 1e-6));
    }
}
