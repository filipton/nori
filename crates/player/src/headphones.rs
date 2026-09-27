//! Headphones taken off and put back on. Taking them off reaches the phone as the headphones going away
//! (the audio becoming noisy as the route leaves them) or as the headphones saying pause themselves (a
//! media key: Bluetooth AVRCP, which is how wear detection such as Sony's arrives). Either way the music
//! stops at once, never fading out: a fade would only make the pause slower. With the user's "resume when
//! headphones are put back on", the same headphones back within [`RESUME_WITHIN_MS`] bring the music back
//! with a short fade in: when they connect again, or when they say play themselves. Only a pause they
//! made is taken back this way; any other control forgets it. The platform reports what happened; this
//! decides.

use crate::outputs::{parts, OutputPort, SPEAKER};

/// How long after they came off the headphones' return still resumes: a coffee, not the next morning.
pub const RESUME_WITHIN_MS: i64 = 30 * 60_000;
/// How long the music comes up over when it resumes by itself: short, but no jolt in the ears.
pub const FADE_IN_MS: i32 = 1_000;
/// Headphones that connected again are given this long before the music starts, so the route has
/// moved to them and the first second is not heard from the speaker.
pub const SETTLE_MS: i64 = 1_000;
/// Noisy can come just after the route has already left the headphones for the speaker: the output
/// before counts as the one that went if it left this recently.
const JUST_LEFT_MS: i64 = 3_000;

/// What the headphones connecting again (or the output changing at all) asks of the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Back {
    Nothing,
    /// The headphones that came off are back but only just: ask again after this long.
    AskAgain { ms: i64 },
    /// Play, fading in over this long.
    Resume { fade_ms: i32 },
}

/// Whether music could be heard on an output privately, so that it leaving is headphones coming off.
fn private(key: &str) -> bool {
    matches!(parts(key).0, OutputPort::Wired | OutputPort::Bluetooth | OutputPort::Usb)
}

#[derive(Debug, Clone, Default)]
pub struct Headphones {
    /// The output now, since when, and the one before it.
    now: Option<String>,
    since_ms: i64,
    before: Option<String>,
    /// The headphones whose coming off paused the music, and when.
    off: Option<(String, i64)>,
}

impl Headphones {
    pub const fn new() -> Self {
        Headphones { now: None, since_ms: 0, before: None, off: None }
    }

    /// The output media goes to is `key` (reported on every change, and again when [`Back::AskAgain`]
    /// said so); `paused`: the music is not wanted now; `on`: the user's setting. Whether to play.
    pub fn output(&mut self, key: &str, paused: bool, on: bool, now_ms: i64) -> Back {
        if self.now.as_deref() != Some(key) {
            self.before = self.now.replace(key.to_string());
            self.since_ms = now_ms;
        }
        let Some((gone, at)) = self.off.as_ref() else { return Back::Nothing };
        if !on || !paused || now_ms - at > RESUME_WITHIN_MS {
            self.off = None;
            return Back::Nothing;
        }
        if gone != key {
            return Back::Nothing;
        }
        let waited = now_ms - self.since_ms;
        if waited < SETTLE_MS {
            return Back::AskAgain { ms: SETTLE_MS - waited };
        }
        self.off = None;
        Back::Resume { fade_ms: FADE_IN_MS }
    }

    /// The headphones came off (noisy, or a pause key from them) while the music was `playing`. Whether
    /// to pause, at once. Remembered only for music that was playing on headphones.
    pub fn off(&mut self, playing: bool, now_ms: i64) -> bool {
        if !playing {
            return false;
        }
        let just_left = self.now.as_deref() == Some(SPEAKER) && now_ms - self.since_ms <= JUST_LEFT_MS;
        let gone = if just_left { self.before.as_deref() } else { self.now.as_deref() };
        self.off = gone.filter(|k| private(k)).map(|k| (k.to_string(), now_ms));
        true
    }

    /// The headphones said play themselves (put back on, most of them do). The fade to play with when this
    /// takes back their own pause and the user wants that; `None`: an ordinary play.
    pub fn play_key(&mut self, on: bool, now_ms: i64) -> Option<i32> {
        let (gone, at) = self.off.take()?;
        (on && now_ms - at <= RESUME_WITHIN_MS && self.now.as_deref() == Some(gone.as_str())).then_some(FADE_IN_MS)
    }

    /// Any other control (play or pause from the app, the notification, the car; another player taking the
    /// sound): the headphones' pause is no longer theirs to take back.
    pub fn forget(&mut self) {
        self.off = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XM6: &str = "Bluetooth: WH-1000XM6";
    const MIN: i64 = 60_000;

    fn on_headphones() -> Headphones {
        let mut h = Headphones::new();
        assert_eq!(h.output(XM6, false, true, 0), Back::Nothing);
        h
    }

    #[test]
    fn headphones_that_went_away_come_back_fading_in_once_the_route_settles() {
        let mut h = on_headphones();
        assert!(h.off(true, 10 * MIN), "noisy while playing pauses");
        assert_eq!(h.output(SPEAKER, true, true, 10 * MIN + 500), Back::Nothing, "the speaker does not resume");
        let back = 20 * MIN;
        assert_eq!(h.output(XM6, true, true, back), Back::AskAgain { ms: SETTLE_MS });
        assert_eq!(h.output(XM6, true, true, back + 400), Back::AskAgain { ms: SETTLE_MS - 400 });
        assert_eq!(h.output(XM6, true, true, back + SETTLE_MS), Back::Resume { fade_ms: FADE_IN_MS });
        assert_eq!(h.output(XM6, true, true, back + 2 * SETTLE_MS), Back::Nothing, "once");
    }

    #[test]
    fn noisy_after_the_route_left_blames_the_headphones_not_the_speaker() {
        let mut h = on_headphones();
        h.output(SPEAKER, false, true, MIN);
        assert!(h.off(true, MIN + 200));
        h.output(XM6, true, true, 2 * MIN);
        assert_eq!(h.output(XM6, true, true, 2 * MIN + SETTLE_MS), Back::Resume { fade_ms: FADE_IN_MS });
    }

    #[test]
    fn only_the_same_headphones_within_half_an_hour_with_the_setting_on() {
        let mut h = on_headphones();
        h.off(true, 0);
        h.output(SPEAKER, true, true, 1);
        assert_eq!(h.output("Bluetooth: Car", true, true, MIN), Back::Nothing, "other headphones");
        assert_eq!(h.output("Bluetooth: Car", true, true, MIN + SETTLE_MS), Back::Nothing);

        let mut h = on_headphones();
        h.off(true, 0);
        h.output(SPEAKER, true, true, 1);
        assert_eq!(h.output(XM6, true, true, RESUME_WITHIN_MS + 1), Back::Nothing, "too late");
        assert_eq!(h.output(XM6, true, true, RESUME_WITHIN_MS + 1 + SETTLE_MS), Back::Nothing, "and forgotten");

        let mut h = on_headphones();
        h.off(true, 0);
        h.output(SPEAKER, true, false, 1);
        h.output(XM6, true, false, MIN);
        assert_eq!(h.output(XM6, true, false, MIN + SETTLE_MS), Back::Nothing, "the setting is off");
    }

    #[test]
    fn only_a_pause_the_headphones_made() {
        // Paused by the user, then the headphones went: nothing to take back.
        let mut h = on_headphones();
        assert!(!h.off(false, 0), "not playing: nothing to pause");
        h.output(SPEAKER, true, true, 1);
        h.output(XM6, true, true, MIN);
        assert_eq!(h.output(XM6, true, true, MIN + SETTLE_MS), Back::Nothing);

        // The headphones' pause, then the user played and paused again from the phone.
        let mut h = on_headphones();
        h.off(true, 0);
        h.forget();
        h.output(SPEAKER, true, true, 1);
        h.output(XM6, true, true, MIN);
        assert_eq!(h.output(XM6, true, true, MIN + SETTLE_MS), Back::Nothing);

        // Playing again already (on the speaker) when they come back: left as it is.
        let mut h = on_headphones();
        h.off(true, 0);
        h.output(SPEAKER, true, true, 1);
        h.output(XM6, false, true, MIN);
        assert_eq!(h.output(XM6, true, true, MIN + SETTLE_MS), Back::Nothing);

        // Music on the speaker paused by a media key is no headphones coming off.
        let mut h = Headphones::new();
        h.output(SPEAKER, false, true, 0);
        assert!(h.off(true, MIN));
        assert_eq!(h.play_key(true, 2 * MIN), None);
    }

    #[test]
    fn headphones_that_say_play_themselves_fade_in() {
        // Wear detection: pause on the way off, play on the way on, connected throughout.
        let mut h = on_headphones();
        assert!(h.off(true, 0));
        assert_eq!(h.play_key(true, 5 * MIN), Some(FADE_IN_MS));
        assert_eq!(h.play_key(true, 6 * MIN), None, "once");

        let mut h = on_headphones();
        h.off(true, 0);
        assert_eq!(h.play_key(false, MIN), None, "setting off: an ordinary play");
        let mut h = on_headphones();
        h.off(true, 0);
        assert_eq!(h.play_key(true, RESUME_WITHIN_MS + 1), None, "too late: an ordinary play");
        let mut h = on_headphones();
        h.off(true, 0);
        h.forget();
        assert_eq!(h.play_key(true, MIN), None, "the user paused since");
    }

    #[test]
    fn a_reconnect_and_a_play_key_resume_once_between_them() {
        let mut h = on_headphones();
        h.off(true, 0);
        h.output(SPEAKER, true, true, 1);
        assert_eq!(h.output(XM6, true, true, MIN), Back::AskAgain { ms: SETTLE_MS });
        assert_eq!(h.play_key(true, MIN + 100), Some(FADE_IN_MS));
        assert_eq!(h.output(XM6, false, true, MIN + SETTLE_MS), Back::Nothing);
    }
}
