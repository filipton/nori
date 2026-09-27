//! Which sound an output device gets when music moves to it. A device can have a sound of its own (a
//! saved profile bound to it), be marked quiet (never offered a curve), or have nothing chosen; with
//! nothing chosen, headphones whose name matches an AutoEQ curve get it offered, or applied straight
//! away when the user asked for that. The platform looks things up and applies the answer.

/// What is known about the device as it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    /// A profile is bound to this device.
    pub bound: bool,
    /// The user wants each device to keep its own sound.
    pub per_output: bool,
    /// The phone's own speaker: never offered a curve.
    pub speaker: bool,
    /// The user said this device should never be offered a curve.
    pub quiet: bool,
    /// Apply a matching curve without asking.
    pub auto_apply: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveStep {
    /// Leave it: no curve is looked for.
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
    /// Bring back the sound from before a bound device took over, if one was kept.
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

/// The profile "Flat": the equalizer off on a device, everything else as it was when it was made.
pub const FLAT: &str = "Flat";
/// The profile "No processing": nothing in the sample chain on a device (its sound's `bypass`), so what
/// reaches it is the music as it comes and audio offload can play it.
pub const BYPASS: &str = "No processing";

/// Loading a device's own sound. The first time one replaces a sound nobody bound to a device, that
/// sound is kept, so it comes back when the music goes to such a device again (the DAC unplugged, back
/// to the speaker). True: keep the sound playing now before loading the device's.
pub fn keep_loose(per_output: bool, loose_kept: bool) -> bool {
    per_output && !loose_kept
}

/// What a device in the list gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKind {
    /// Nothing chosen: a matching AutoEQ curve is offered (or applied, with the setting on).
    Automatic,
    /// Nothing chosen and nothing offered.
    Quiet,
    /// The equalizer off on this device, everything else as it is now.
    Flat,
    /// A saved profile.
    Profile,
    /// No processing on this device: no equalizer and no effects at all.
    Bypass,
}

/// One output device in the equalizer's device list: `output` is its key, `port` and `name` where it is
/// plugged in and what it calls itself (none when it gave no name), and `choice` what it gets, with the
/// profile's name for [`ChoiceKind::Profile`]. The client words the rest ("Automatic", "Leave as is").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub output: String,
    pub port: crate::outputs::OutputPort,
    pub name: Option<String>,
    pub current: bool,
    pub choice: ChoiceKind,
    /// The profile bound to it, for `ChoiceKind::Profile`.
    pub profile: Option<String>,
}

/// Every output seen, the one playing now included, each with the sound it gets: the speaker first,
/// then by the name its key gives it. `profiles` are (name, the outputs bound to it).
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
            // In the order the list has always had: by the name after "USB: " or "Bluetooth: ", else the key.
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
    fn device_rows_say_what_each_device_gets() {
        use crate::outputs::OutputPort;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let known = s(&["USB: K3", SPEAKER, "Bluetooth: buds", "Wired headphones", "USB: DAC"]);
        let flat = s(&["Wired headphones"]);
        let warm = s(&["USB: K3", "Bluetooth: Other"]);
        let profiles: [(&str, &[String]); 2] = [(FLAT, &flat), ("Warm", &warm)];
        let rows = rows(&known, "Bluetooth: Other", &profiles, &s(&["Bluetooth: buds"]));
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
    }

    #[test]
    fn a_device_left_unprocessed_says_so() {
        let dac = vec!["USB: DAC".to_string()];
        let profiles: [(&str, &[String]); 1] = [(BYPASS, &dac)];
        let rows = rows(&dac, SPEAKER, &profiles, &[]);
        let r = rows.iter().find(|r| r.output == "USB: DAC").unwrap();
        assert_eq!((r.choice, r.profile.as_deref()), (ChoiceKind::Bypass, None));
    }

    #[test]
    fn the_current_device_is_listed_once() {
        let known = vec![SPEAKER.to_string()];
        assert_eq!(rows(&known, SPEAKER, &[], &[]).len(), 1);
    }

    #[test]
    fn the_loose_sound_is_kept_once() {
        assert!(keep_loose(true, false));
        assert!(!keep_loose(true, true));
        assert!(!keep_loose(false, false));
    }

    fn arrival() -> Arrival {
        Arrival { bound: false, per_output: true, speaker: false, quiet: false, auto_apply: false }
    }

    #[test]
    fn a_bound_device_gets_its_own_sound_and_nothing_else() {
        assert_eq!(on_arrival(Arrival { bound: true, ..arrival() }), ArrivalPlan { load_bound: true, restore: false, curve: CurveStep::None });
        assert!(!on_arrival(Arrival { bound: true, per_output: false, ..arrival() }).load_bound, "per-device sound off");
    }

    #[test]
    fn an_unbound_device_gets_the_sound_from_before_and_maybe_a_curve() {
        assert_eq!(on_arrival(arrival()), ArrivalPlan { load_bound: false, restore: true, curve: CurveStep::Offer });
        assert_eq!(on_arrival(Arrival { auto_apply: true, ..arrival() }).curve, CurveStep::Apply);
        assert_eq!(on_arrival(Arrival { auto_apply: true, per_output: false, ..arrival() }).curve, CurveStep::Offer, "applying needs per-device sound");
    }

    #[test]
    fn the_speaker_and_quiet_devices_are_never_offered_a_curve() {
        assert_eq!(on_arrival(Arrival { speaker: true, ..arrival() }).curve, CurveStep::None);
        assert_eq!(on_arrival(Arrival { quiet: true, auto_apply: true, ..arrival() }).curve, CurveStep::None);
    }
}
