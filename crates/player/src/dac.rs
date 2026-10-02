//! Bit-perfect USB DAC output: which DAC mode plays a song untouched, or why none can.

/// One of the DAC's modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DacMode {
    pub rate: u32,
    pub bits: u32,
    /// Float samples: 32 bits, but not the same mode as 32-bit integers.
    pub float: bool,
}

/// Why no mode plays a song bit-perfect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DacBlock {
    /// The DAC has no bit-perfect mode at the song's `rate`.
    NoModeAtRate { rate: u32 },
    /// The DAC has `rate` only in depths Android's output cannot write (it writes 16-bit or float);
    /// needs a USB exclusive driver. `depths` is a set of [`DEPTH_16`], [`DEPTH_24`], [`DEPTH_32`].
    NeedsExclusive { rate: u32, depths: u8 },
    /// The platform cannot drive a DAC bit-perfect at all (Android before 14).
    PlatformTooOld,
    /// The platform refused the mode it was asked for.
    Refused,
}

/// Flags of [`DacBlock::NeedsExclusive`]'s depth set.
pub const DEPTH_16: u8 = 1;
pub const DEPTH_24: u8 = 2;
pub const DEPTH_32: u8 = 4;

/// A bit depth's flag in [`DacBlock::NeedsExclusive`]'s set; 0 for other depths.
fn depth_bit(bits: u32) -> u8 {
    match bits {
        16 => DEPTH_16,
        24 => DEPTH_24,
        32 => DEPTH_32,
        _ => 0,
    }
}

/// The index of the mode that plays `playing` bit-perfect, or why none does (`None`: off, no modes,
/// or nothing playing).
fn choose(enabled: bool, modes: &[DacMode], playing: DacMode) -> Result<usize, Option<DacBlock>> {
    let rate = playing.rate;
    if !enabled || modes.is_empty() || rate == 0 {
        return Err(None);
    }
    if let Some(i) = modes.iter().position(|m| *m == playing) {
        return Ok(i);
    }
    let depths = modes.iter().filter(|m| m.rate == rate).fold(0u8, |set, m| set | depth_bit(m.bits));
    Err(Some(if modes.iter().any(|m| m.rate == rate) { DacBlock::NeedsExclusive { rate, depths } } else { DacBlock::NoModeAtRate { rate } }))
}

/// What the platform does with the DAC after a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DacStep {
    /// Clear the preferred mode: none is usable.
    Release,
    /// The mode already applied is the one wanted; leave everything as it is.
    Keep,
    /// Clear what is applied and ask for this mode (an index into the modes).
    Prefer { index: u32 },
}

/// The decision about an attached DAC, and the facts shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DacDecision {
    pub step: DacStep,
    /// The DAC's name, trimmed; `None` when blank.
    pub device: Option<String>,
    /// The device offers at least one bit-perfect mode.
    pub supported: bool,
    /// Every bit-perfect mode the device offers.
    pub modes: Vec<DacMode>,
    /// The format playing, `None` before anything plays.
    pub playing: Option<DacMode>,
    pub bits: u32,
    pub blocked_by: Option<DacBlock>,
}

/// Decides the DAC's mode. `platform_ok`: the platform can do bit-perfect (Android 14+). `applied`: the
/// modes of the port a preferred mode is held for, if it is this port. Every step but `Keep` clears the
/// preferred mode first: a stale mode routes the output somewhere silent.
pub fn decide(
    enabled: bool, platform_ok: bool, name: &str, modes: &[DacMode], playing: DacMode, applied: Option<&[DacMode]>, was_bit_perfect: bool,
) -> DacDecision {
    let device = Some(name.trim()).filter(|n| !n.is_empty()).map(str::to_string);
    let shown = (playing.rate != 0).then_some(playing);
    let base = DacDecision { step: DacStep::Release, device, supported: false, modes: Vec::new(), playing: shown, bits: playing.bits, blocked_by: None };
    if !platform_ok {
        return DacDecision { blocked_by: Some(DacBlock::PlatformTooOld), ..base };
    }
    let index = match choose(enabled, modes, playing) {
        Ok(i) => i,
        Err(blocked_by) => return DacDecision { supported: !modes.is_empty(), modes: modes.to_vec(), blocked_by, ..base },
    };
    let step = if was_bit_perfect && applied.is_some_and(|a| a.contains(&modes[index])) { DacStep::Keep } else { DacStep::Prefer { index: index as u32 } };
    DacDecision { step, supported: true, modes: modes.to_vec(), ..base }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_and_applied_mode() {
        let d = decide(true, true, " K3 ", &MODES, m(48_000, 24), None, false);
        assert_eq!(d.step, DacStep::Prefer { index: 1 });
        assert_eq!(d.device.as_deref(), Some("K3"));
        assert!(d.supported);
        assert_eq!(d.modes, MODES);
        assert_eq!(d.playing, Some(m(48_000, 24)));
        assert_eq!(d.bits, 24);
        assert_eq!(d.blocked_by, None);
        assert_eq!(decide(true, true, "  ", &MODES, m(0, 16), None, false).device, None, "a nameless DAC is the client's to name");
        assert_eq!(decide(true, true, "K3", &MODES, m(0, 16), None, false).playing, None);

        // Applied mode is kept.
        assert_eq!(decide(true, true, "K3", &MODES, m(48_000, 24), Some(&MODES), true).step, DacStep::Keep);
        assert_eq!(decide(true, true, "K3", &MODES, m(48_000, 24), Some(&MODES), false).step, DacStep::Prefer { index: 1 }, "not bit-perfect yet");
        assert_eq!(decide(true, true, "K3", &MODES, m(48_000, 24), Some(&MODES[..1]), true).step, DacStep::Prefer { index: 1 }, "another mode applied");
    }

    const fn m(rate: u32, bits: u32) -> DacMode {
        DacMode { rate, bits, float: false }
    }

    const MODES: [DacMode; 3] = [m(44_100, 16), m(48_000, 24), m(96_000, 32)];

    #[test]
    fn exact_mode_is_chosen() {
        assert_eq!(choose(true, &MODES, m(44_100, 16)), Ok(0));
        assert_eq!(choose(true, &MODES, m(96_000, 32)), Ok(2));
        assert!(choose(true, &MODES, DacMode { float: true, ..m(96_000, 32) }).is_err(), "float is not 32-bit integer");

        // Missing depth needs exclusive.
        assert_eq!(choose(true, &MODES, m(48_000, 16)), Err(Some(DacBlock::NeedsExclusive { rate: 48_000, depths: DEPTH_24 })));
        let two = [m(48_000, 24), m(48_000, 32)];
        assert_eq!(choose(true, &two, m(48_000, 16)), Err(Some(DacBlock::NeedsExclusive { rate: 48_000, depths: DEPTH_24 | DEPTH_32 })));
    }

    #[test]
    fn nothing_chosen_or_unusable() {
        assert_eq!(choose(false, &MODES, m(44_100, 16)), Err(None));
        assert_eq!(choose(true, &[], m(44_100, 16)), Err(None));
        assert_eq!(choose(true, &MODES, m(0, 16)), Err(None));

        // Unusable releases with reason.
        let d = decide(true, true, "K3", &MODES, m(88_200, 16), None, false);
        assert_eq!(d.step, DacStep::Release);
        assert!(d.supported);
        assert_eq!(d.modes.len(), 3);
        assert_eq!(d.blocked_by, Some(DacBlock::NoModeAtRate { rate: 88_200 }));
        let old = decide(true, false, "K3", &MODES, m(44_100, 16), None, false);
        assert_eq!(old.step, DacStep::Release);
        assert!(!old.supported && old.modes.is_empty());
        assert_eq!(old.blocked_by, Some(DacBlock::PlatformTooOld));
        assert_eq!(old.playing, Some(m(44_100, 16)));
        let none = decide(true, true, "K3", &[], m(44_100, 16), None, false);
        assert!(!none.supported && none.blocked_by.is_none());
    }

}
