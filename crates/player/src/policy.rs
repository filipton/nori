//! The small decisions that shape what is heard, apart from the samples themselves: which parts of the
//! chain may run at all (a bit-perfect output forbids everything that touches samples; offload forbids
//! everything that needs them), how loud a track plays under ReplayGain, and the shape of a fade.
//! Pure functions of settings and facts about the output: the platform applies the answers.

/// What the user asked for, as far as the chain is concerned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioPrefs {
    /// The equalizer and the rest of the sound chain.
    pub dsp: bool,
    pub skip_silence: bool,
    /// Let the phone's audio chip decode when nothing needs the samples.
    pub offload: bool,
    pub crossfade_s: i32,
    pub auto_mix: bool,
    pub speed: f32,
    pub pitch: f32,
}

/// Facts about the output right now.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OutputState {
    /// 24-bit output at the file's own format: samples go out untouched.
    pub hi_res: bool,
    /// A USB DAC in bit-perfect mode: samples go out untouched.
    pub bit_perfect: bool,
    /// Something USB is attached: the audio chip has no path to it, so offload would play silence.
    pub usb: bool,
    /// The output refused an offloaded track once; decoding stays on the CPU.
    pub offload_refused: bool,
}

/// What the chain may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPolicy {
    /// The file's own samples must reach the output: nothing may be mixed, converted or processed.
    pub untouched: bool,
    /// The sound chain processes the samples.
    pub processing: bool,
    /// Crossfades and AutoMix stand down.
    pub transitions_off: bool,
    /// The output format is pinned to the first track's (see the transition engine).
    pub lock_rate: bool,
    pub skip_silence: bool,
    /// The audio chip decodes.
    pub offload: bool,
    /// The sound chain sits in the path (flat, it is an identity copy) so that switching it on later
    /// needs no rebuild of the output.
    pub processor_in_chain: bool,
}

/// Bit-perfect and hi-res output mean exactly the file's samples reach the output, so nothing that
/// touches samples may run: not the equalizer, not a transition, not the format lock, not silence
/// skipping. Offload hands the compressed stream to the audio chip, so it is only possible while
/// nothing needs the samples at all - and never to a USB output, which the chip cannot reach.
pub fn audio_policy(p: &AudioPrefs, o: &OutputState) -> AudioPolicy {
    let untouched = o.hi_res || o.bit_perfect;
    let processing = p.dsp && !untouched;
    let offload = p.offload
        && !processing
        && !o.usb
        && !o.offload_refused
        && p.crossfade_s == 0
        && !p.auto_mix
        && !p.skip_silence
        && p.speed == 1.0
        && p.pitch == 1.0;
    AudioPolicy {
        untouched,
        processing,
        transitions_off: untouched,
        lock_rate: !untouched,
        skip_silence: p.skip_silence && !untouched,
        offload,
        processor_in_chain: !offload && !untouched,
    }
}

/// Why [`audio_policy`] keeps the audio chip from decoding, in words for a report: the first of its
/// conditions that says no, none when offload may run.
pub fn offload_blocked(p: &AudioPrefs, o: &OutputState) -> Option<&'static str> {
    let untouched = o.hi_res || o.bit_perfect;
    Some(if !p.offload {
        "offload is off in the settings"
    } else if p.dsp && !untouched {
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

/// How ReplayGain picks between a track's and its album's gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainMode {
    Off,
    Track,
    Album,
    /// Album gain inside an album played in order (the tracks keep their relative levels), track gain
    /// everywhere else (so unrelated songs even out).
    Auto,
}

/// A track's ReplayGain tags, dB and linear peaks; any may be missing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GainTags {
    pub track_gain: Option<f32>,
    pub album_gain: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_peak: Option<f32>,
}

/// The volume a track plays at under ReplayGain, 0..1: attenuation only at ReplayGain's own -18 LUFS, so
/// it can be applied as the player's volume (free, and it survives offload). `tags` is `None` for a track
/// with no tags at all, which plays at `untagged_db`; `in_album_run` is whether it sits inside an album
/// played in order. Radio and a bit-perfect output play at full volume: the one has no track, the other
/// must not be touched. Targets and positive gain: [`crate::gain::song_gain`].
pub fn replay_gain(mode: GainMode, tags: Option<&GainTags>, in_album_run: bool, preamp_db: f32, untagged_db: f32, radio: bool, bit_perfect: bool) -> f32 {
    let song = crate::gain::SongLoudness { tags: tags.copied(), ..Default::default() };
    crate::gain::song_gain(&crate::gain::GainPrefs::attenuating(mode, preamp_db, untagged_db), &song, in_album_run, radio, bit_perfect)
}

/// Where a volume fade from `from` to `to` stands at `t` (0..1) of its length.
pub fn fade(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> AudioPrefs {
        AudioPrefs { dsp: false, skip_silence: false, offload: true, crossfade_s: 0, auto_mix: false, speed: 1.0, pitch: 1.0 }
    }

    #[test]
    fn nothing_needing_samples_means_the_chip_decodes() {
        let p = audio_policy(&prefs(), &OutputState::default());
        assert!(p.offload && !p.processor_in_chain && p.lock_rate && !p.transitions_off);
    }

    #[test]
    fn anything_that_needs_samples_keeps_decoding_on_the_cpu() {
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
    }

    #[test]
    fn a_usb_output_is_never_offloaded() {
        assert!(!audio_policy(&prefs(), &OutputState { usb: true, ..Default::default() }).offload);
        assert!(!audio_policy(&prefs(), &OutputState { offload_refused: true, ..Default::default() }).offload);
    }

    #[test]
    fn untouched_output_stands_everything_down() {
        let p = AudioPrefs { dsp: true, skip_silence: true, auto_mix: true, ..prefs() };
        for o in [OutputState { bit_perfect: true, ..Default::default() }, OutputState { hi_res: true, ..Default::default() }] {
            let a = audio_policy(&p, &o);
            assert!(a.untouched && !a.processing && a.transitions_off && !a.lock_rate && !a.skip_silence && !a.processor_in_chain, "{o:?}");
        }
    }

    #[test]
    fn the_reason_offload_stands_down_is_the_policy_s_own() {
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
        // Nothing touches the samples of a bit-perfect output: its equalizer does not stand in the way.
        let bit_perfect = OutputState { bit_perfect: true, ..o };
        assert_eq!(offload_blocked(&AudioPrefs { dsp: true, ..prefs() }, &bit_perfect), None);
        assert!(audio_policy(&AudioPrefs { dsp: true, ..prefs() }, &bit_perfect).offload);
    }

    #[test]
    fn replay_gain_picks_the_right_tag() {
        let t = GainTags { track_gain: Some(-6.0), album_gain: Some(-3.0), track_peak: None, album_peak: None };
        let track = replay_gain(GainMode::Track, Some(&t), true, 0.0, -6.0, false, false);
        let album = replay_gain(GainMode::Album, Some(&t), false, 0.0, -6.0, false, false);
        assert!((track - 10f32.powf(-6.0 / 20.0)).abs() < 1e-6);
        assert!((album - 10f32.powf(-3.0 / 20.0)).abs() < 1e-6);
        assert_eq!(replay_gain(GainMode::Auto, Some(&t), true, 0.0, -6.0, false, false), album, "inside an album run: album gain");
        assert_eq!(replay_gain(GainMode::Auto, Some(&t), false, 0.0, -6.0, false, false), track, "elsewhere: track gain");
    }

    #[test]
    fn replay_gain_never_boosts_and_respects_peaks() {
        let loud = GainTags { track_gain: Some(6.0), ..Default::default() };
        assert_eq!(replay_gain(GainMode::Track, Some(&loud), false, 0.0, -6.0, false, false), 1.0, "attenuation only");
        let peaky = GainTags { track_gain: Some(-1.0), track_peak: Some(1.25), ..Default::default() };
        assert!((replay_gain(GainMode::Track, Some(&peaky), false, 0.0, -6.0, false, false) - 0.8).abs() < 1e-6, "no clipping");
    }

    #[test]
    fn untagged_tracks_and_exceptions() {
        assert!((replay_gain(GainMode::Track, None, false, -3.0, -6.0, false, false) - 10f32.powf(-6.0 / 20.0)).abs() < 1e-6, "no tags: untagged level, no preamp");
        let empty = GainTags::default();
        assert!((replay_gain(GainMode::Track, Some(&empty), false, -3.0, -6.0, false, false) - 10f32.powf(-9.0 / 20.0)).abs() < 1e-6, "tags without gains: untagged plus preamp");
        let t = GainTags { track_gain: Some(-6.0), ..Default::default() };
        assert_eq!(replay_gain(GainMode::Off, Some(&t), false, 0.0, -6.0, false, false), 1.0);
        assert_eq!(replay_gain(GainMode::Track, Some(&t), false, 0.0, -6.0, true, false), 1.0, "radio");
        assert_eq!(replay_gain(GainMode::Track, Some(&t), false, 0.0, -6.0, false, true), 1.0, "bit perfect");
    }
}
