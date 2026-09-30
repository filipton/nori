//! Per-output sound profiles: the types for a device step (arrival, curve adopted, undo, choice) and its
//! resulting [`DeviceEffect`]. The decisions are `nori_player::device`'s; the steps run in nori-core.

use nori_player::device::{self, keep_loose, BYPASS, FLAT};
use nori_player::outputs::SPEAKER;

// Public: the uniffi scaffolding in crates/android names them by path.
pub use nori_player::device::{ChoiceKind, DeviceRow};
pub use nori_player::outputs::OutputPort;

use nori_settings::settings::{sound_json, SoundSettings, StoredPrefs};
use nori_model::AutoEqEntry;
use nori_model::CurveStep;
use nori_model::SoundProfile;

/// `app_kv` key: the sound from before a bound device took over.
pub const LOOSE: &str = "looseSound";
/// `app_kv` key: outputs never to be offered a curve, as a JSON list.
pub const QUIET: &str = "quietOutputs";

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum ChoiceKind {
    Automatic,
    Quiet,
    Flat,
    Profile,
    Bypass,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum OutputPort {
    Speaker,
    Wired,
    Usb,
    Bluetooth,
    Other,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct DeviceRow {
    pub output: String,
    pub port: OutputPort,
    pub name: Option<String>,
    pub current: bool,
    pub choice: ChoiceKind,
    pub profile: Option<String>,
}

/// What happens to the [`LOOSE`] sound (see `nori_player::device::keep_loose`).
#[derive(Debug, Clone, PartialEq)]
pub enum LooseChange {
    Keep,
    /// Store the unbound sound playing now.
    Store { json: String },
    Clear,
}

/// A step's decision: the quiet mark, the loose sound and the platform's effect.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub quiet: Option<bool>,
    pub loose: LooseChange,
    pub effect: DeviceEffect,
}

impl Step {
    pub fn none() -> Self {
        Step { quiet: None, loose: LooseChange::Keep, effect: DeviceEffect::none() }
    }
}

/// What the platform does after a step, in order: reload profiles and quiet devices, apply the sound,
/// rerun the device's arrival.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeviceEffect {
    pub refresh: bool,
    pub apply: Option<SoundSettings>,
    pub arrive: bool,
    /// The step created a profile (a new AutoEQ curve), which undo deletes.
    pub created: bool,
}

impl DeviceEffect {
    pub fn none() -> Self {
        DeviceEffect { refresh: false, apply: None, arrive: false, created: false }
    }
}

/// The device sound settings now.
pub struct Now {
    pub sound: SoundSettings,
    pub per_output: bool,
    pub auto_apply: bool,
}

impl Now {
    fn of(p: &StoredPrefs) -> Self {
        Now { sound: p.sound(), per_output: p.profile_per_output, auto_apply: p.auto_eq_auto }
    }

    pub fn read() -> Self {
        nori_settings::settings_store::with_prefs(Now::of).unwrap_or_else(|| Now::of(&StoredPrefs::default()))
    }
}

/// A device's arrival: the effect, and the AutoEQ curve offered or applied (`entry` and its preset URL
/// when one matches).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeviceArrival {
    pub effect: DeviceEffect,
    pub curve: CurveStep,
    pub entry: Option<AutoEqEntry>,
    pub preset_url: Option<String>,
}

/// Applies a device's sound, storing the current one as loose if it is the first replaced.
pub fn loaded(sound: SoundSettings, current: &SoundSettings, per_output: bool, loose_kept: bool) -> (Option<SoundSettings>, LooseChange) {
    let loose = if keep_loose(per_output, loose_kept) { LooseChange::Store { json: sound_json(current) } } else { LooseChange::Keep };
    (Some(sound), loose)
}

/// The headphone model in a USB or Bluetooth output key ("Bluetooth: LE_WH-1000XM5" is
/// "LE_WH-1000XM5"); None for other outputs and for nameless devices.
pub fn headphones_name(output: &str) -> Option<&str> {
    use nori_player::outputs::{parts, OutputPort};
    match parts(output) {
        (OutputPort::Usb | OutputPort::Bluetooth, name) => name.filter(|n| !n.trim().is_empty()),
        _ => None,
    }
}

/// Every known output, the current one included, with the sound each gets.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn device_rows(known: Vec<String>, current: String, profiles: Vec<SoundProfile>, quiet: Vec<String>) -> Vec<DeviceRow> {
    let bound: Vec<(&str, &[String])> = profiles.iter().map(|p| (p.name.as_str(), p.outputs.as_slice())).collect();
    device::rows(&known, &current, &bound, &quiet)
}

/// An output key's port (`nori_player::outputs::parts`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn output_port(output: String) -> OutputPort {
    nori_player::outputs::parts(&output).0
}

/// The sound the test bridge sets for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SpecKind {
    Automatic,
    Quiet,
    Flat,
    Profile,
    /// The first AutoEQ curve found for `arg`.
    Curve,
    Bypass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeviceSpec {
    pub output: String,
    pub kind: SpecKind,
    pub arg: String,
}

/// Parses the test bridge's `"<output>=flat|bypass|auto|quiet|profile:<name>|curve:<search>"`; anything
/// else after the last '=' is automatic.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn device_spec(value: String) -> DeviceSpec {
    let (output, spec) = value.rsplit_once('=').unwrap_or((&value, &value));
    let (kind, arg) = match (spec, spec.strip_prefix("profile:"), spec.strip_prefix("curve:")) {
        ("flat", ..) => (SpecKind::Flat, ""),
        ("quiet", ..) => (SpecKind::Quiet, ""),
        ("bypass", ..) => (SpecKind::Bypass, ""),
        (_, Some(name), _) => (SpecKind::Profile, name),
        (_, _, Some(search)) => (SpecKind::Curve, search),
        _ => (SpecKind::Automatic, ""),
    };
    DeviceSpec { output: output.to_string(), kind, arg: arg.to_string() }
}

/// Queries under two UTF-16 units are not searched.
pub fn autoeq_too_short(query: &str) -> bool {
    query.trim().encode_utf16().count() < 2
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AutoEqFound {
    pub too_short: bool,
    pub hits: Vec<AutoEqEntry>,
}

/// What a device's sheet offers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeviceSheet {
    /// Saved profiles, without "Flat" and "No processing" (rows of their own).
    pub profiles: Vec<String>,
    /// Not the current output nor the speaker.
    pub can_forget: bool,
}

pub fn sheet(output: &str, current: bool, profiles: &[String]) -> DeviceSheet {
    DeviceSheet { profiles: profiles.iter().filter(|p| *p != FLAT && *p != BYPASS).cloned().collect(), can_forget: !current && output != SPEAKER }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn device_sheet(output: String, current: bool, profiles: Vec<String>) -> DeviceSheet {
    sheet(&output, current, &profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_spec_parses() {
        let s = |v: &str| {
            let d = device_spec(v.into());
            (d.output, d.kind, d.arg)
        };
        assert_eq!(s("USB: K3=flat"), ("USB: K3".into(), SpecKind::Flat, String::new()));
        assert_eq!(s("a=b=quiet"), ("a=b".into(), SpecKind::Quiet, String::new()));
        assert_eq!(s("USB: K3=profile:Warm"), ("USB: K3".into(), SpecKind::Profile, "Warm".into()));
        assert_eq!(s("x=curve:HD 600"), ("x".into(), SpecKind::Curve, "HD 600".into()));
        assert_eq!(s("x=auto"), ("x".into(), SpecKind::Automatic, String::new()));
        assert_eq!(s("USB: K3=bypass"), ("USB: K3".into(), SpecKind::Bypass, String::new()));
        assert_eq!(sheet("USB: K3", false, &["Warm".into(), FLAT.into(), BYPASS.into()]).profiles, ["Warm"]);
        assert_eq!(s("flat"), ("flat".into(), SpecKind::Flat, String::new()));
    }
}
