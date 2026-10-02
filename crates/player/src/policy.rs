//! Which parts of the chain may run for the current settings and output: bit-perfect output forbids
//! touching samples, offload forbids needing them. Also the output rate cap.
//!
//! With high quality output on a float-capable device the chain runs in float end to end. Otherwise it
//! runs on 16-bit samples and dithers whatever it changes. Float is not the default: most phone outputs
//! mix into a 16-bit device, where the platform would round a float track without dither (worse than
//! our dithered 16-bit), and float doubles the buffer's memory.

/// User settings relevant to the chain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioPrefs {
    /// The equalizer and the rest of the sound chain.
    pub dsp: bool,
    pub skip_silence: bool,
    /// Let the audio chip decode when nothing needs the samples.
    pub offload: bool,
    pub crossfade_s: i32,
    pub auto_mix: bool,
    pub speed: f32,
    pub pitch: f32,
}

/// Facts about the current output.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OutputState {
    /// High quality output on a float-capable device.
    pub hi_res: bool,
    /// A USB DAC in bit-perfect mode.
    pub bit_perfect: bool,
    /// Something USB is attached (offload would play silence).
    pub usb: bool,
    /// An offloaded track was refused once; stay on the CPU.
    pub offload_refused: bool,
}

/// What the chain may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPolicy {
    /// The file's samples must reach the output unchanged.
    pub untouched: bool,
    /// Decode to float, process in float, open the device for float (hi-res or bit-perfect).
    pub float: bool,
    /// The sound chain processes the samples.
    pub processing: bool,
    /// Crossfades and AutoMix are disabled.
    pub transitions_off: bool,
    /// Latch the output format to the first track's (see the transition engine).
    pub lock_rate: bool,
    pub skip_silence: bool,
    /// The audio chip decodes.
    pub offload: bool,
    /// Keep the (possibly flat) sound chain in the path so enabling it later needs no rebuild.
    pub processor_in_chain: bool,
}

/// Bit-perfect disables everything that touches samples; hi-res only raises precision. Offload needs
/// nothing to touch the samples and no USB output.
pub fn audio_policy(p: &AudioPrefs, o: &OutputState) -> AudioPolicy {
    let untouched = o.bit_perfect;
    let offload = offload_blocked(p, o).is_none();
    AudioPolicy {
        untouched,
        float: o.hi_res || o.bit_perfect,
        processing: p.dsp && !untouched,
        transitions_off: untouched,
        lock_rate: !untouched,
        skip_silence: p.skip_silence && !untouched,
        offload,
        processor_in_chain: !offload && !untouched,
    }
}

/// The first reason offload cannot run, for reports; `None` when it can.
pub fn offload_blocked(p: &AudioPrefs, o: &OutputState) -> Option<&'static str> {
    Some(if !p.offload {
        "offload is off in the settings"
    } else if p.dsp && !o.bit_perfect {
        "the equalizer or another sound setting is on"
    } else if o.usb {
        "something USB is attached, which the audio chip cannot reach"
    } else if o.offload_refused {
        "the offloaded track failed, so offload is given up until the player starts again"
    } else if p.crossfade_s != 0 {
        "a crossfade is set"
    } else if p.auto_mix {
        "AutoMix is on"
    } else if p.skip_silence {
        "silence skipping is on"
    } else if p.speed != 1.0 || p.pitch != 1.0 {
        "the speed or pitch is changed"
    } else {
        return None;
    })
}

/// The rate to play a `rate` Hz song at under a `max` Hz cap (0: no cap): halved within its family
/// (so conversion is by two) while above the cap but not below 44.1 kHz; otherwise the cap itself.
pub fn capped_rate(rate: u32, max: u32) -> u32 {
    if max == 0 || rate <= max {
        return rate;
    }
    let mut r = rate;
    while r > max && r % 2 == 0 && r / 2 >= 44_100 {
        r /= 2;
    }
    r.min(max)
}

/// How ReplayGain picks between track and album gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainMode {
    Off,
    Track,
    Album,
    /// Album gain inside an album played in order, track gain otherwise.
    Auto,
}

/// A track's ReplayGain tags (dB, linear peaks).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GainTags {
    pub track_gain: Option<f32>,
    pub album_gain: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_peak: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> AudioPrefs {
        AudioPrefs { dsp: false, skip_silence: false, offload: true, crossfade_s: 0, auto_mix: false, speed: 1.0, pitch: 1.0 }
    }

    #[test]
    fn offload_decision() {
        let p = audio_policy(&prefs(), &OutputState::default());
        assert!(p.offload && !p.processor_in_chain && p.lock_rate && !p.transitions_off);

        // Sample features block offload.
        for p in [
            AudioPrefs { dsp: true, ..prefs() },
            AudioPrefs { skip_silence: true, ..prefs() },
            AudioPrefs { crossfade_s: 4, ..prefs() },
            AudioPrefs { auto_mix: true, ..prefs() },
            AudioPrefs { speed: 1.25, ..prefs() },
            AudioPrefs { pitch: 0.95, ..prefs() },
        ] {
            let a = audio_policy(&p, &OutputState::default());
            assert!(!a.offload && a.processor_in_chain, "{p:?}");
        }

        // Usb or refusal blocks offload.
        assert!(!audio_policy(&prefs(), &OutputState { usb: true, ..Default::default() }).offload);
        assert!(!audio_policy(&prefs(), &OutputState { offload_refused: true, ..Default::default() }).offload);

        // Offload reasons.
        let o = OutputState::default();
        assert_eq!(offload_blocked(&prefs(), &o), None);
        assert!(audio_policy(&prefs(), &o).offload);
        let cases: [(AudioPrefs, OutputState, &str); 8] = [
            (AudioPrefs { offload: false, ..prefs() }, o, "offload is off"),
            (AudioPrefs { dsp: true, ..prefs() }, o, "equalizer"),
            (prefs(), OutputState { usb: true, ..o }, "USB"),
            (prefs(), OutputState { offload_refused: true, ..o }, "failed"),
            (AudioPrefs { crossfade_s: 4, ..prefs() }, o, "crossfade"),
            (AudioPrefs { auto_mix: true, ..prefs() }, o, "AutoMix"),
            (AudioPrefs { skip_silence: true, ..prefs() }, o, "silence"),
            (AudioPrefs { speed: 1.5, ..prefs() }, o, "speed"),
        ];
        for (p, out, words) in cases {
            assert!(!audio_policy(&p, &out).offload, "{words}");
            let why = offload_blocked(&p, &out).unwrap_or_default();
            assert!(why.contains(words), "{why}");
        }
        // Bit-perfect ignores the equalizer setting.
        let bit_perfect = OutputState { bit_perfect: true, ..o };
        assert_eq!(offload_blocked(&AudioPrefs { dsp: true, ..prefs() }, &bit_perfect), None);
        assert!(audio_policy(&AudioPrefs { dsp: true, ..prefs() }, &bit_perfect).offload);
    }

    #[test]
    fn bit_perfect_disables_processing() {
        let p = AudioPrefs { dsp: true, skip_silence: true, auto_mix: true, ..prefs() };
        let a = audio_policy(&p, &OutputState { bit_perfect: true, ..Default::default() });
        assert!(a.untouched && a.float && !a.processing && a.transitions_off && !a.lock_rate && !a.skip_silence && !a.processor_in_chain);
    }

    #[test]
    fn chain_depth() {
        let p = AudioPrefs { dsp: true, skip_silence: true, auto_mix: true, ..prefs() };
        let hi = OutputState { hi_res: true, ..Default::default() };
        let a = audio_policy(&p, &hi);
        assert!(!a.untouched && a.float && a.processing && !a.transitions_off && a.lock_rate && a.skip_silence && a.processor_in_chain, "{a:?}");
        assert!(audio_policy(&prefs(), &hi).offload);
        assert_eq!(offload_blocked(&p, &hi), Some("the equalizer or another sound setting is on"));

        // Without hi res chain is 16 bit.
        for p in [prefs(), AudioPrefs { dsp: true, ..prefs() }, AudioPrefs { dsp: true, auto_mix: true, skip_silence: true, ..prefs() }] {
            let a = audio_policy(&p, &OutputState::default());
            assert!(!a.float && !a.untouched && a.lock_rate, "{p:?}");
        }
    }

    #[test]
    fn capped_rate_halves_within_family() {
        assert_eq!(capped_rate(192_000, 0), 192_000, "no maximum: the song's own");
        assert_eq!(capped_rate(44_100, 48_000), 44_100);
        assert_eq!(capped_rate(48_000, 48_000), 48_000);
        assert_eq!(capped_rate(96_000, 48_000), 48_000);
        assert_eq!(capped_rate(192_000, 48_000), 48_000);
        assert_eq!(capped_rate(88_200, 48_000), 44_100, "88.2 kHz to 44.1, not to 48");
        assert_eq!(capped_rate(176_400, 48_000), 44_100);
        assert_eq!(capped_rate(176_400, 96_000), 88_200);
        assert_eq!(capped_rate(192_000, 96_000), 96_000);
        assert_eq!(capped_rate(352_800, 192_000), 176_400);
        assert_eq!(capped_rate(384_000, 192_000), 192_000);
        assert_eq!(capped_rate(64_000, 48_000), 48_000, "not halved below 44.1 kHz");
    }
}
