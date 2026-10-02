//! Output device keys ("USB: <name>", "Bluetooth: <name>", ...) that sound profiles bind to, which
//! attached output media is routed to, and the list of every output seen.

/// The speaker's key; always in the known list. Keys are stored in settings, so they never change.
pub const SPEAKER: &str = "Phone speaker";
const WIRED: &str = "Wired headphones";
const USB: &str = "USB: ";
const BLUETOOTH: &str = "Bluetooth: ";
/// Names used when a device reports none.
const NAMELESS_USB: &str = "DAC";
const NAMELESS_BLUETOOTH: &str = "device";
const OTHER: &str = "Other output";

/// How an output is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPort {
    Speaker,
    Wired,
    Usb,
    Bluetooth,
    /// Dock, HDMI, line out or anything else.
    Other,
}

/// Splits a key into its port and the device's own name (`None` for placeholders, speaker, wired).
pub fn parts(key: &str) -> (OutputPort, Option<&str>) {
    fn named_or<'a>(nameless: &str, name: &'a str) -> Option<&'a str> {
        (name != nameless).then_some(name)
    }
    if key == SPEAKER {
        (OutputPort::Speaker, None)
    } else if key == WIRED {
        (OutputPort::Wired, None)
    } else if let Some(name) = key.strip_prefix(USB) {
        (OutputPort::Usb, named_or(NAMELESS_USB, name))
    } else if let Some(name) = key.strip_prefix(BLUETOOTH) {
        (OutputPort::Bluetooth, named_or(NAMELESS_BLUETOOTH, name))
    } else {
        (OutputPort::Other, named_or(OTHER, key))
    }
}

/// An attached output's type, mapped from the platform's device types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    /// A USB DAC or USB headset.
    Usb,
    /// USB, so it blocks offload, but media is not routed to it.
    UsbAccessory,
    /// Wired headphones or a wired headset.
    Wired,
    /// A Bluetooth A2DP or LE audio device.
    Bluetooth,
    /// A dock, HDMI or an aux line out.
    Line,
    Speaker,
    /// Telephony, virtual sinks, the earpiece.
    Other,
}

/// Routing precedence, lowest wins. `Other` ranks below the speaker: every phone lists a telephony
/// output, which must not count as current.
fn rank(kind: OutputKind) -> u8 {
    match kind {
        OutputKind::Usb => 0,
        OutputKind::Wired => 1,
        OutputKind::Bluetooth => 2,
        OutputKind::Line => 3,
        OutputKind::Speaker => 8,
        OutputKind::UsbAccessory | OutputKind::Other => 9,
    }
}

/// The key a device is stored by.
pub fn key(kind: OutputKind, name: &str) -> String {
    let name = name.trim();
    let or = |fallback: &str| if name.is_empty() { fallback.to_string() } else { name.to_string() };
    match kind {
        OutputKind::Speaker => SPEAKER.to_string(),
        OutputKind::Wired => WIRED.to_string(),
        OutputKind::Usb => format!("{USB}{}", or(NAMELESS_USB)),
        OutputKind::Bluetooth => format!("{BLUETOOTH}{}", or(NAMELESS_BLUETOOTH)),
        OutputKind::UsbAccessory | OutputKind::Line | OutputKind::Other => or(OTHER),
    }
}

/// Output state after a device was attached or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// Where media goes now.
    pub current: String,
    /// Every output seen, if it changed.
    pub known: Option<Vec<String>>,
    /// Something USB is attached: offload must be off (an offloaded track routed to USB plays silence).
    pub usb: bool,
}

/// Recomputes outputs from the attached (kind, name) list. `fake_usb` simulates an attached USB device.
pub fn refresh(attached: &[(OutputKind, &str)], known: &[String], fake_usb: Option<&str>) -> Seen {
    let fake = fake_usb.map(|n| format!("{USB}{n}"));
    // First of the lowest rank, as Android routes.
    let best = attached.iter().reduce(|b, d| if rank(d.0) < rank(b.0) { d } else { b });
    let current = fake.clone().or_else(|| best.map(|d| key(d.0, d.1))).unwrap_or_else(|| SPEAKER.to_string());
    let mut next: Vec<String> = known.to_vec();
    next.extend(attached.iter().filter(|d| rank(d.0) < rank(OutputKind::Speaker)).map(|d| key(d.0, d.1)));
    next.push(SPEAKER.to_string());
    next.extend(fake.clone());
    let next = sorted_distinct(next);
    let usb = fake.is_some() || attached.iter().any(|d| matches!(d.0, OutputKind::Usb | OutputKind::UsbAccessory));
    Seen { current, known: (next != known).then_some(next), usb }
}

/// The stored known list, with the speaker added.
pub fn initial_known(stored: &[String]) -> Vec<String> {
    let mut all = stored.to_vec();
    all.push(SPEAKER.to_string());
    sorted_distinct(all)
}

/// Removes a device from the known list (it returns when next connected). The speaker and the current
/// output stay. `None` when nothing changes.
pub fn forget(known: &[String], current: &str, output: &str) -> Option<Vec<String>> {
    if output == SPEAKER || output == current {
        return None;
    }
    let next: Vec<String> = known.iter().filter(|o| *o != output).cloned().collect();
    (next != known).then_some(next)
}

fn sorted_distinct(mut list: Vec<String>) -> Vec<String> {
    list.sort();
    list.dedup();
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn keys() {
        assert_eq!(key(OutputKind::Usb, "  FiiO K3 "), "USB: FiiO K3");
        assert_eq!(key(OutputKind::Usb, " "), "USB: DAC");
        assert_eq!(key(OutputKind::Bluetooth, "WH-1000XM5"), "Bluetooth: WH-1000XM5");
        assert_eq!(key(OutputKind::Bluetooth, ""), "Bluetooth: device");
        assert_eq!(key(OutputKind::Wired, "anything"), "Wired headphones");
        assert_eq!(key(OutputKind::Speaker, "whatever"), SPEAKER);
        assert_eq!(key(OutputKind::Line, "TV"), "TV");
        assert_eq!(key(OutputKind::Other, ""), "Other output");

        // Parts invert key.
        assert_eq!(parts("USB: FiiO K3"), (OutputPort::Usb, Some("FiiO K3")));
        assert_eq!(parts("USB: DAC"), (OutputPort::Usb, None));
        assert_eq!(parts("Bluetooth: device"), (OutputPort::Bluetooth, None));
        assert_eq!(parts("Bluetooth: WH-1000XM5"), (OutputPort::Bluetooth, Some("WH-1000XM5")));
        assert_eq!(parts(SPEAKER), (OutputPort::Speaker, None));
        assert_eq!(parts("Wired headphones"), (OutputPort::Wired, None));
        assert_eq!(parts("TV"), (OutputPort::Other, Some("TV")));
        assert_eq!(parts("Other output"), (OutputPort::Other, None));
        for (k, n) in [(OutputKind::Usb, "K3"), (OutputKind::Usb, ""), (OutputKind::Bluetooth, ""), (OutputKind::Line, "TV"), (OutputKind::Other, "")] {
            let key = key(k, n);
            let (port, name) = parts(&key);
            assert_eq!(name.unwrap_or(""), n, "{k:?} {port:?}");
        }
    }

    #[test]
    fn current_is_best_ranked() {
        let attached = [(OutputKind::Other, "Telephony"), (OutputKind::Speaker, ""), (OutputKind::Bluetooth, "Buds"), (OutputKind::Wired, "")];
        let seen = refresh(&attached, &[], None);
        assert_eq!(seen.current, "Wired headphones");
        assert!(!seen.usb);
        // Telephony ranks below the speaker, so it is neither current nor remembered.
        assert_eq!(seen.known, Some(s(&["Bluetooth: Buds", SPEAKER, "Wired headphones"])));
        assert_eq!(refresh(&[(OutputKind::Other, "Telephony"), (OutputKind::Speaker, "")], &[], None).current, SPEAKER);
        assert_eq!(refresh(&[], &[], None).current, SPEAKER);
    }

    #[test]
    fn known_changes_only_for_new_devices() {
        let known = s(&["Bluetooth: Buds", SPEAKER]);
        assert_eq!(refresh(&[(OutputKind::Bluetooth, "Buds")], &known, None).known, None);
        let seen = refresh(&[(OutputKind::Usb, "K3")], &known, None);
        assert_eq!(seen.current, "USB: K3");
        assert!(seen.usb);
        assert_eq!(seen.known, Some(s(&["Bluetooth: Buds", SPEAKER, "USB: K3"])));
    }

    #[test]
    fn usb() {
        let seen = refresh(&[(OutputKind::UsbAccessory, "Hub"), (OutputKind::Speaker, "")], &s(&[SPEAKER]), None);
        assert_eq!(seen.current, SPEAKER);
        assert!(seen.usb);
        assert_eq!(seen.known, None);

        // Fake usb is current and known.
        let seen = refresh(&[(OutputKind::Speaker, "")], &s(&[SPEAKER]), Some("Mock DAC"));
        assert_eq!(seen.current, "USB: Mock DAC");
        assert!(seen.usb);
        assert_eq!(seen.known, Some(s(&[SPEAKER, "USB: Mock DAC"])));
    }

    #[test]
    fn forget_keeps_speaker_and_current() {
        let known = s(&["Bluetooth: Buds", SPEAKER, "USB: K3"]);
        assert_eq!(forget(&known, "USB: K3", SPEAKER), None);
        assert_eq!(forget(&known, "USB: K3", "USB: K3"), None);
        assert_eq!(forget(&known, "USB: K3", "Bluetooth: Buds"), Some(s(&[SPEAKER, "USB: K3"])));
        assert_eq!(forget(&known, "USB: K3", "Wired headphones"), None);
        assert_eq!(initial_known(&s(&["USB: K3"])), s(&[SPEAKER, "USB: K3"]));
    }
}
