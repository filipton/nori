//! Whether the sound settings put anything in the sample domain.

/// Any sample-domain processing is on. Anything here disables audio offload (not bursts).
pub fn sound_on(eq_enabled: bool, crossfeed_db: f32, balance: f32, mono: bool, limiter: bool, effects: bool) -> bool {
    eq_enabled || crossfeed_db > 0.0 || balance != 0.0 || mono || limiter || effects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_effect_turns_sound_on() {
        assert!(!sound_on(false, 0.0, 0.0, false, false, false));
        assert!(sound_on(true, 0.0, 0.0, false, false, false));
        assert!(sound_on(false, 3.0, 0.0, false, false, false));
        assert!(!sound_on(false, -3.0, 0.0, false, false, false), "crossfeed is only on above 0 dB");
        assert!(sound_on(false, 0.0, -0.2, false, false, false));
        assert!(sound_on(false, 0.0, 0.0, true, false, false));
        assert!(sound_on(false, 0.0, 0.0, false, true, false));
        assert!(sound_on(false, 0.0, 0.0, false, false, true));
    }
}
