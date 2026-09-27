//! Which sound each output device gets, as whole steps: a device arriving, a curve adopted for it, an
//! undo, a choice from the device list. The decision is nori-player's (`nori_player::device`); this
//! reads the settings, looks up the profiles, saves and binds them, keeps the two small things the
//! steps need between them (the sound from before a device took over, and the devices never to be
//! offered a curve), and says what the platform should do next as one `DeviceEffect`: which sound to
//! load, whether to read the profiles again, whether to run the device's arrival again. The platform
//! fetches an AutoEQ preset when asked (that is transport) and applies the effect.

use nori_player::device::{self, keep_loose, BYPASS, FLAT};
use nori_player::outputs::SPEAKER;

// Public, like model.rs's, since the uniffi scaffolding in crates/android names them by a public path.
pub use nori_player::device::{ChoiceKind, DeviceRow};
pub use nori_player::outputs::OutputPort;

use nori_settings::settings::{sound_json, SoundSettings, StoredPrefs};
use nori_model::AutoEqEntry;
use nori_model::CurveStep;
use nori_model::SoundProfile;

/// The sound playing now, kept from before a bound device took over (`app_kv`).
pub const LOOSE: &str = "looseSound";
/// The devices the user said should never be offered a curve, as a JSON list (`app_kv`).
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

/// What happens to the sound kept from before a device took over (see `nori_player::device::keep_loose`).
#[derive(Debug, Clone, PartialEq)]
pub enum LooseChange {
    Keep,
    /// Keep this sound: it is what plays now, and nobody bound it to a device.
    Store { json: String },
    /// It has been used, or is no longer wanted.
    Clear,
}

/// What a step decided, before the core settles its own part of it (the quiet mark and the kept sound).
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

/// What the platform does after a step, in this order: read the profiles (and the quiet devices) again,
/// load the sound, and run the device's arrival again.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeviceEffect {
    pub refresh: bool,
    pub apply: Option<SoundSettings>,
    pub arrive: bool,
    /// The step made a new profile (an AutoEQ curve not saved before): undo deletes it again.
    pub created: bool,
}

impl DeviceEffect {
    pub fn none() -> Self {
        DeviceEffect { refresh: false, apply: None, arrive: false, created: false }
    }
}

/// What the settings say about device sound right now.
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

/// A device arriving: the effect of its bound profile or of the sound from before, and whether a curve
/// is offered or applied (`entry`, with the url of its preset, when one matches).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeviceArrival {
    pub effect: DeviceEffect,
    pub curve: CurveStep,
    pub entry: Option<AutoEqEntry>,
    pub preset_url: Option<String>,
}

/// Loads a device's own sound, keeping the sound playing now when it is the first one replaced.
pub fn loaded(sound: SoundSettings, current: &SoundSettings, per_output: bool, loose_kept: bool) -> (Option<SoundSettings>, LooseChange) {
    let loose = if keep_loose(per_output, loose_kept) { LooseChange::Store { json: sound_json(current) } } else { LooseChange::Keep };
    (Some(sound), loose)
}

/// What an output's own name says about the headphones behind it: "Bluetooth: LE_WH-1000XM5" is
/// "LE_WH-1000XM5". Empty for the speaker and anything else without a name of its own.
pub fn device_name(output: &str) -> &str {
    output.split_once(": ").map_or("", |(_, n)| n)
}

/// Every output seen, the one playing now included, each with the sound it gets.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn device_rows(known: Vec<String>, current: String, profiles: Vec<SoundProfile>, quiet: Vec<String>) -> Vec<DeviceRow> {
    let bound: Vec<(&str, &[String])> = profiles.iter().map(|p| (p.name.as_str(), p.outputs.as_slice())).collect();
    device::rows(&known, &current, &bound, &quiet)
}

/// Where an output is plugged in, from its key (`nori_player::outputs::parts`): for a platform that asks
/// its own audio system something per kind of device (the volume curve it uses).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn output_port(output: String) -> OutputPort {
    nori_player::outputs::parts(&output).0
}

/// What the test bridge asks a device to get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SpecKind {
    Automatic,
    Quiet,
    Flat,
    Profile,
    /// The first AutoEQ curve found for `arg`.
    Curve,
    /// No processing on the device.
    Bypass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeviceSpec {
    pub output: String,
    pub kind: SpecKind,
    pub arg: String,
}

/// The test bridge's `set deviceSound "<output>=flat|bypass|auto|quiet|profile:<name>|curve:<search>"`; anything
/// else after the last '=' is automatic.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn device_spec(value: String) -> DeviceSpec {
    let (output, spec) = value.rsplit_once('=').unwrap_or((&value, &value));
    let (kind, arg) = if spec == "flat" {
        (SpecKind::Flat, "")
    } else if spec == "quiet" {
        (SpecKind::Quiet, "")
    } else if spec == "bypass" {
        (SpecKind::Bypass, "")
    } else if let Some(name) = spec.strip_prefix("profile:") {
        (SpecKind::Profile, name)
    } else if let Some(search) = spec.strip_prefix("curve:") {
        (SpecKind::Curve, search)
    } else {
        (SpecKind::Automatic, "")
    };
    DeviceSpec { output: output.to_string(), kind, arg: arg.to_string() }
}

// ---- the device list and the AutoEQ browser, worded ----

/// Fewer than two characters (UTF-16 units, as the platform counts them) is not searched.
pub fn autoeq_too_short(query: &str) -> bool {
    query.trim().encode_utf16().count() < 2
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AutoEqFound {
    pub too_short: bool,
    pub hits: Vec<AutoEqEntry>,
}

/// What a device's sheet offers; the client words it (its introduction by where the device is plugged
/// in, the line under "Automatic" by whether a curve is applied or offered).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeviceSheet {
    /// The saved profiles it can be given; "Flat" and "No processing" are rows of their own, so they are
    /// not among them.
    pub profiles: Vec<String>,
    /// Neither the one playing now nor the phone's speaker, which are always there.
    pub can_forget: bool,
}

pub fn sheet(output: &str, current: bool, profiles: &[String]) -> DeviceSheet {
    DeviceSheet { profiles: profiles.iter().filter(|p| *p != FLAT && *p != BYPASS).cloned().collect(), can_forget: !current && output != SPEAKER }
}

/// The sheet of the device `output` (the one playing now or not), given the saved profiles' names.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn device_sheet(output: String, current: bool, profiles: Vec<String>) -> DeviceSheet {
    sheet(&output, current, &profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_specs() {
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
