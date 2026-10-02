//! Which sound profile an output device gets when playback moves to it: its bound profile, or with
//! none bound an AutoEQ curve offered or applied, unless it is marked quiet.

/// Facts about a newly active output device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    /// A profile is bound to this device.
    pub bound: bool,
    /// The user wants each device to keep its own sound.
    pub per_output: bool,
    /// The phone's own speaker: never offered a curve.
    pub speaker: bool,
    /// Never offer this device a curve.
    pub quiet: bool,
    /// Apply a matching curve without asking.
    pub auto_apply: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveStep {
    /// No curve lookup.
    None,
    /// Look for a curve; offer it if one matches.
    Offer,
    /// Look for a curve; apply it if one matches (an offer if fetching it fails).
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrivalPlan {
    /// Load the device's bound profile.
    pub load_bound: bool,
    /// Restore the unbound sound saved when a bound device took over.
    pub restore: bool,
    pub curve: CurveStep,
}

pub fn on_arrival(a: Arrival) -> ArrivalPlan {
    if a.bound {
        return ArrivalPlan { load_bound: a.per_output, restore: false, curve: CurveStep::None };
    }
    let curve = if a.speaker || a.quiet {
        CurveStep::None
    } else if a.auto_apply && a.per_output {
        CurveStep::Apply
    } else {
        CurveStep::Offer
    };
    ArrivalPlan { load_bound: false, restore: a.per_output, curve }
}

/// Built-in profile: equalizer off, everything else unchanged.
pub const FLAT: &str = "Flat";
/// Built-in profile: no sample processing at all (the sound's `bypass`), so offload can play.
pub const BYPASS: &str = "No processing";

/// Whether to save the current unbound sound before loading a device's own, so it can be restored
/// when playback returns to an unbound device. Saved once.
pub fn keep_loose(per_output: bool, loose_kept: bool) -> bool {
    per_output && !loose_kept
}

/// What a listed device gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKind {
    /// Nothing chosen: a matching AutoEQ curve is offered or applied.
    Automatic,
    /// Nothing chosen and nothing offered.
    Quiet,
    /// [`FLAT`].
    Flat,
    /// A saved profile.
    Profile,
    /// [`BYPASS`].
    Bypass,
}

/// One row of the equalizer's device list. `output` is the device key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub output: String,
    pub port: crate::outputs::OutputPort,
    pub name: Option<String>,
    pub current: bool,
    pub choice: ChoiceKind,
    /// The bound profile's name, for [`ChoiceKind::Profile`].
    pub profile: Option<String>,
}

/// Every known output plus the current one, speaker first, then by name. `profiles`: (name, bound outputs).
pub fn rows(known: &[String], current: &str, profiles: &[(&str, &[String])], quiet: &[String]) -> Vec<DeviceRow> {
    let mut outputs: Vec<&str> = Vec::with_capacity(known.len() + 1);
    for o in known.iter().map(String::as_str).chain(std::iter::once(current)) {
        if !outputs.contains(&o) {
            outputs.push(o);
        }
    }
    let mut rows: Vec<(String, DeviceRow)> = outputs
        .into_iter()
        .map(|o| {
            let bound = profiles.iter().find(|(_, outs)| outs.iter().any(|x| x == o)).map(|(n, _)| *n);
            let choice = match bound {
                Some(FLAT) => ChoiceKind::Flat,
                Some(BYPASS) => ChoiceKind::Bypass,
                Some(_) => ChoiceKind::Profile,
                None if quiet.iter().any(|q| q == o) => ChoiceKind::Quiet,
                None => ChoiceKind::Automatic,
            };
            let (port, name) = crate::outputs::parts(o);
            // Sort by the name after "USB: " / "Bluetooth: ", else the key.
            let order = o.split_once(": ").map_or(o, |(_, n)| n).to_lowercase();
            let row = DeviceRow {
                output: o.to_string(),
                port,
                name: name.map(str::to_string),
                current: o == current,
                choice,
                profile: bound.filter(|_| choice == ChoiceKind::Profile).map(str::to_string),
            };
            (order, row)
        })
        .collect();
    rows.sort_by(|a, b| (a.1.output != crate::outputs::SPEAKER, &a.0).cmp(&(b.1.output != crate::outputs::SPEAKER, &b.0)));
    rows.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outputs::SPEAKER;

    #[test]
    fn device_rows() {
        use crate::outputs::OutputPort;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let known = s(&["USB: K3", SPEAKER, "Bluetooth: buds", "Wired headphones", "USB: DAC"]);
        let flat = s(&["Wired headphones"]);
        let warm = s(&["USB: K3", "Bluetooth: Other"]);
        let profiles: [(&str, &[String]); 2] = [(FLAT, &flat), ("Warm", &warm)];
        let rows = super::rows(&known, "Bluetooth: Other", &profiles, &s(&["Bluetooth: buds"]));
        let got: Vec<(&str, OutputPort, Option<&str>, bool, ChoiceKind, Option<&str>)> =
            rows.iter().map(|r| (r.output.as_str(), r.port, r.name.as_deref(), r.current, r.choice, r.profile.as_deref())).collect();
        assert_eq!(
            got,
            [
                (SPEAKER, OutputPort::Speaker, None, false, ChoiceKind::Automatic, None),
                ("Bluetooth: buds", OutputPort::Bluetooth, Some("buds"), false, ChoiceKind::Quiet, None),
                ("USB: DAC", OutputPort::Usb, None, false, ChoiceKind::Automatic, None),
                ("USB: K3", OutputPort::Usb, Some("K3"), false, ChoiceKind::Profile, Some("Warm")),
                ("Bluetooth: Other", OutputPort::Bluetooth, Some("Other"), true, ChoiceKind::Profile, Some("Warm")),
                ("Wired headphones", OutputPort::Wired, None, false, ChoiceKind::Flat, None),
            ]
        );

        // Bypass profile row.
        let dac = vec!["USB: DAC".to_string()];
        let profiles: [(&str, &[String]); 1] = [(BYPASS, &dac)];
        let rows = super::rows(&dac, SPEAKER, &profiles, &[]);
        let r = rows.iter().find(|r| r.output == "USB: DAC").unwrap();
        assert_eq!((r.choice, r.profile.as_deref()), (ChoiceKind::Bypass, None));

        // Current device listed once.
        let known = vec![SPEAKER.to_string()];
        assert_eq!(super::rows(&known, SPEAKER, &[], &[]).len(), 1);
    }

    fn arrival() -> Arrival {
        Arrival { bound: false, per_output: true, speaker: false, quiet: false, auto_apply: false }
    }

    #[test]
    fn profiles_on_arrival() {
        assert_eq!(on_arrival(Arrival { bound: true, ..arrival() }), ArrivalPlan { load_bound: true, restore: false, curve: CurveStep::None });
        assert!(!on_arrival(Arrival { bound: true, per_output: false, ..arrival() }).load_bound, "per-device sound off");

        // Unbound device restores and offers curve.
        assert_eq!(on_arrival(arrival()), ArrivalPlan { load_bound: false, restore: true, curve: CurveStep::Offer });
        assert_eq!(on_arrival(Arrival { auto_apply: true, ..arrival() }).curve, CurveStep::Apply);
        assert_eq!(on_arrival(Arrival { auto_apply: true, per_output: false, ..arrival() }).curve, CurveStep::Offer, "applying needs per-device sound");

        // Speaker and quiet get no curve.

        assert_eq!(on_arrival(Arrival { speaker: true, ..arrival() }).curve, CurveStep::None);
        assert_eq!(on_arrival(Arrival { quiet: true, auto_apply: true, ..arrival() }).curve, CurveStep::None);
    }

}
