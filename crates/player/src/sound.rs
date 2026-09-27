//! What the sound settings (the part a sound profile remembers) mean for the chain.

/// Something in the sample domain is switched on: the equalizer, crossfeed, balance off centre, mono,
/// the limiter, or one of the effects (bass boost, compressor, virtualizer, volume boost). Anything here
/// stops audio offload but not burst playback.
pub fn sound_on(eq_enabled: bool, crossfeed_db: f32, balance: f32, mono: bool, limiter: bool, effects: bool) -> bool {
    eq_enabled || crossfeed_db > 0.0 || balance != 0.0 || mono || limiter || effects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_effect_switches_the_sound_chain_on() {
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
