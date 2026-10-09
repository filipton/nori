//! Following another device's playback place for place (a jam guest listening along). The leader's
//! place rules: this engine starts where the leader is heard at the moment its own music is heard, trims
//! its rate a little to stay there, and starts again where it is when it strays or the leader jumps.
//! Here are the decisions; the worker carries them out.

/// Most the rate is trimmed either way: a pitch change no ear hears.
pub(super) const TRIM_MAX: f64 = 0.005;
/// A trim takes the place this long to close the gap it is set for, ms; the engine looks again when it
/// has.
const TRIM_OVER_MS: f64 = 4_000.0;
/// Gaps under this are left alone, ms.
const TRIM_FROM_MS: f64 = 1.0;
/// A trim is changed at most this often, µs (each makes the music again).
const TRIM_EVERY_US: i64 = 2_000_000;
/// Trims closer than this to the one playing are not made (each makes the music again).
const TRIM_STEP: f64 = 0.0002;
/// Further than this from the leader, ms, the place is started again rather than trimmed back.
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
    /// How long music made now takes to be heard (what the device holds), µs.
    pub held_us: i64,
}

/// What to do now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Step {
    Stay,
    Pause,
    /// Start song `index` at `ms`, heard from `at_us`.
    Start { index: usize, ms: f64, at_us: i64 },
    /// Play at this rate trim from the first music that can still change.
    Trim(f64),
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
    /// Read at `at_us`, `gap` ms from the leader: compared with the next reading.
    Read { at_us: i64, gap: f64 },
}

#[derive(Debug)]
pub(super) struct Following {
    pub led: Led,
    /// The leader jumped since this one last started.
    leapt: bool,
    pub trim: f64,
    pub starting: Option<Starting>,
    /// When the trim playing has closed its gap, µs: looked at again then.
    pub look_at: Option<i64>,
    /// When the trim last changed, and when this one last started, µs.
    trimmed_at: i64,
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
        Following { led, leapt: true, trim: 0.0, starting: None, look_at: None, trimmed_at: i64::MIN / 2, started_at: i64::MIN / 2, start_delay_us: 0, settling: Settling::Settled, mixed: false, start_lead_us: START_LEAD_US, elsewhere: None }
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
        // is followed by trimming, not by starting again.
        let settled = now_us - self.started_at > LEAP_QUIET_US;
        self.leapt |= settled && old.playing && new.playing && old.index == new.index && (new.place_at(now_us) - old.place_at(now_us)).abs() > LEAP_MS;
    }

    /// Where to start now: the leader's song and its place when the music made now is heard.
    fn start(&mut self, now_us: i64) -> Step {
        self.leapt = false;
        self.look_at = None;
        self.elsewhere = None;
        let at_us = now_us + self.start_lead_us;
        self.started_at = at_us;
        self.settling = Settling::Started;
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
        if self.settling != Settling::Settled {
            // The gap a trim playing moves on its own, since the reading compared with.
            let agrees = |at: i64, was: f64| (gap - was - self.trim * (now_us - at) as f64 / 1000.0).abs() <= SETTLED_MS;
            match self.settling {
                Settling::Read { at_us: at, gap: was } if now_us - at >= SETTLE_US && agrees(at, was) => {
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
                    self.settling = Settling::Read { at_us: now_us, gap };
                    self.look_at = Some(now_us + SETTLE_US);
                    return Step::Stay;
                }
            }
        }
        if gap.abs() > STRAY_MS {
            return self.start(now_us);
        }
        // The trim playing now holds on until a new one is heard.
        let then = gap + self.trim * here.held_us as f64 / 1000.0;
        let trim = if then.abs() < TRIM_FROM_MS { 0.0 } else { (-then / TRIM_OVER_MS).clamp(-TRIM_MAX, TRIM_MAX) };
        if ((trim - self.trim).abs() < TRIM_STEP && (trim == 0.0) == (self.trim == 0.0)) || now_us - self.trimmed_at < TRIM_EVERY_US {
            return Step::Stay;
        }
        self.trimmed_at = now_us;
        self.look_at = (trim != 0.0).then(|| now_us + here.held_us + ((then.abs() / trim.abs()) as i64 * 1000).max(TRIM_EVERY_US));
        Step::Trim(trim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn led(ms: f64, at_us: i64) -> Led {
        Led { index: 2, ms, at_us, rate: 1.25, playing: true }
    }

    fn here(at: Option<(usize, f64)>) -> Here {
        Here { playing: at.is_some(), at, mixing: false, held_us: 0 }
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
    fn small_gaps_are_trimmed_and_large_ones_started_again() {
        let mut f = Following::new(led(10_000.0, 0));
        f.leapt = false;
        // 20 ms ahead: slower, within what is not heard.
        let Step::Trim(t) = f.step(1_000_000, here(Some((2, 11_270.0)))) else { panic!() };
        assert!((-TRIM_MAX..0.0).contains(&t), "{t}");
        f.trim = t;
        f.trimmed_at = i64::MIN / 2;
        // In step: nothing new.
        assert_eq!(f.step(1_000_000, here(Some((2, 11_250.3)))), Step::Trim(0.0));
        f.trim = 0.0;
        assert_eq!(f.step(1_000_000, here(Some((2, 11_250.3)))), Step::Stay);
        assert!(matches!(f.step(1_000_000, here(Some((2, 11_500.0)))), Step::Start { .. }), "strayed");
    }

    #[test]
    fn a_trim_still_to_be_heard_counts() {
        let mut f = Following::new(led(10_000.0, 0));
        f.leapt = false;
        f.trim = -0.004;
        // 20 ms ahead, but the device holds 5 s more at the trim: 0 ms ahead once a new one is heard.
        assert_eq!(f.step(1_000_000, Here { held_us: 5_000_000, ..here(Some((2, 11_270.0))) }), Step::Trim(0.0));
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
        // again later: settled. Trimmed, not started again (each start is a gap).
        let heard_at = |us: i64, late: f64| here(Some((2, ms + (us - at_us) as f64 / 1000.0 * 1.25 - late)));
        assert_eq!(f.step(at_us, heard_at(at_us, 0.0)), Step::Stay);
        assert_eq!(f.step(at_us + SETTLE_US, heard_at(at_us + SETTLE_US, 120.0)), Step::Stay, "not settled yet");
        assert_eq!(f.look_at, Some(at_us + 2 * SETTLE_US));
        assert!(matches!(f.step(at_us + 2 * SETTLE_US, heard_at(at_us + 2 * SETTLE_US, 120.0)), Step::Trim(t) if t == TRIM_MAX));
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
