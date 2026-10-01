//! Android's device types and PCM encodings mapped onto `nori_player::outputs` and `nori_player::dac`,
//! and the calls Kotlin makes on device and format changes.

use nori_model::DacMode;
#[cfg(feature = "ffi")]
#[allow(unused_imports)]
use nori_model::DacBlock;
use nori_player::dac;
use crate::profiles::OutputPort;
use nori_player::outputs::{self, OutputKind};
use nori_settings::settings_store;

// Public: the uniffi scaffolding in crates/android names them by path.
pub use nori_player::dac::DacDecision;
#[cfg(any(feature = "ffi", test))]
pub use nori_player::dac::DacStep;
pub use nori_player::outputs::Seen;

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct Seen {
    pub current: String,
    pub known: Option<Vec<String>>,
    pub usb: bool,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum DacStep {
    Release,
    Keep,
    Prefer { index: u32 },
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct DacDecision {
    pub step: DacStep,
    pub device: Option<String>,
    pub supported: bool,
    pub modes: Vec<DacMode>,
    pub playing: Option<DacMode>,
    pub bits: u32,
    pub blocked_by: Option<DacBlock>,
}

/// `AudioDeviceInfo.TYPE_*`.
pub fn kind(t: i32) -> OutputKind {
    match t {
        11 | 22 => OutputKind::Usb,          // USB_DEVICE, USB_HEADSET
        12 => OutputKind::UsbAccessory,      // USB_ACCESSORY
        3 | 4 => OutputKind::Wired,          // WIRED_HEADSET, WIRED_HEADPHONES
        8 | 26 | 27 => OutputKind::Bluetooth, // BLUETOOTH_A2DP, BLE_HEADSET, BLE_SPEAKER
        13 | 9 | 19 => OutputKind::Line,     // DOCK, HDMI, AUX_LINE
        2 => OutputKind::Speaker,            // BUILTIN_SPEAKER
        _ => OutputKind::Other,
    }
}

/// The player's output button icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum OutputGlyph {
    Headphones,
    Bluetooth,
    /// The speaker and anything else.
    Cast,
}

/// The output button's state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct OutputLook {
    pub glyph: OutputGlyph,
    /// Not the phone's speaker (the glyph is accented).
    pub elsewhere: bool,
    pub port: OutputPort,
    /// The device's own name, if it gave one.
    pub name: Option<String>,
}

/// The output button for an output key ("USB: …", "Bluetooth: …", "Wired headphones", the speaker, ...).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn output_look(output: String) -> OutputLook {
    let (port, name) = outputs::parts(&output);
    let glyph = match port {
        OutputPort::Usb | OutputPort::Wired => OutputGlyph::Headphones,
        OutputPort::Bluetooth => OutputGlyph::Bluetooth,
        _ => OutputGlyph::Cast,
    };
    OutputLook { glyph, elsewhere: output != outputs::SPEAKER, port, name: name.map(str::to_string) }
}

/// `AudioFormat.ENCODING_*` in bits per sample; float counts as 32.
fn bits(encoding: i32) -> u32 {
    match encoding {
        2 => 16,       // PCM_16BIT
        21 => 24,      // PCM_24BIT_PACKED
        22 | 4 => 32,  // PCM_32BIT, PCM_FLOAT
        _ => 0,
    }
}

const PCM_FLOAT: i32 = 4;

fn mode(rate: u32, encoding: i32) -> DacMode {
    DacMode { rate, bits: bits(encoding), float: encoding == PCM_FLOAT }
}

fn modes(rates: &[u32], encodings: &[i32]) -> Vec<DacMode> {
    rates.iter().zip(encodings).map(|(r, e)| mode(*r, *e)).collect()
}

/// `app_kv` key: every output seen, as a JSON list, so unplugged devices can still get a sound.
const KNOWN: &str = "knownOutputs";

fn keep(known: &[String]) {
    settings_store::shared().keep_app_value(KNOWN, serde_json::to_string(known).unwrap_or_default());
}

/// The attached devices (parallel `AudioDeviceInfo` types and product names) against the known list; a
/// changed list is stored.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn outputs_refresh(types: Vec<i32>, names: Vec<String>, known: Vec<String>, fake_usb: Option<String>) -> Seen {
    let attached: Vec<(OutputKind, &str)> = types.iter().zip(&names).map(|(t, n)| (kind(*t), n.as_str())).collect();
    let seen = outputs::refresh(&attached, &known, fake_usb.as_deref());
    if let Some(k) = &seen.known {
        keep(k);
    }
    seen
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn outputs_speaker() -> String {
    nori_player::outputs::SPEAKER.into()
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn device_flat() -> String {
    nori_player::device::FLAT.into()
}

/// Every output seen, as stored.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn outputs_known() -> Vec<String> {
    let stored: Vec<String> = settings_store::shared().app_value(KNOWN).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    outputs::initial_known(&stored)
}

/// The known list without `output`, stored; None when unchanged.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn outputs_forget(known: Vec<String>, current: String, output: String) -> Option<Vec<String>> {
    let next = outputs::forget(&known, &current, &output);
    if let Some(k) = &next {
        keep(k);
    }
    next
}

/// `nori_player::dac::decide` over parallel lists of rates and `AudioFormat` encodings; `applied_*` are
/// the modes of the port a preferred mode is held for.
#[cfg_attr(feature = "ffi", uniffi::export)]
#[allow(clippy::too_many_arguments)]
pub fn dac_decide(
    enabled: bool, platform_ok: bool, name: String, rates: Vec<u32>, encodings: Vec<i32>, playing_rate: u32, playing_encoding: i32,
    applied_rates: Option<Vec<u32>>, applied_encodings: Option<Vec<i32>>, was_bit_perfect: bool,
) -> DacDecision {
    let applied = applied_rates.zip(applied_encodings).map(|(r, e)| modes(&r, &e));
    dac::decide(enabled, platform_ok, &name, &modes(&rates, &encodings), mode(playing_rate, playing_encoding), applied.as_deref(), was_bit_perfect)
}

/// A fake DAC for the test bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MockDac {
    pub name: String,
    pub rates: Vec<u32>,
    /// `AudioFormat` encodings, one per rate.
    pub encodings: Vec<i32>,
}

/// Parses `name@44100/16,96000/24` (depth "16", "24", "32" or "float", default 16); unreadable modes are
/// skipped. Without `@` or a name, the name is "Mock DAC".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn dac_mock(spec: String) -> MockDac {
    let name = match spec.split_once('@') {
        Some((n, _)) if !n.is_empty() => n.to_string(),
        _ => "Mock DAC".to_string(),
    };
    let (mut rates, mut encodings) = (Vec::new(), Vec::new());
    for m in spec.split_once('@').map_or("", |(_, m)| m).split(',') {
        let (rate, depth) = m.split_once('/').unwrap_or((m, "16"));
        let Ok(rate) = rate.trim().parse::<u32>() else { continue };
        let encoding = match depth.trim() {
            "16" => 2,
            "24" => 21,
            "32" => 22,
            "float" => PCM_FLOAT,
            _ => continue,
        };
        rates.push(rate);
        encodings.push(encoding);
    }
    MockDac { name, rates, encodings }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_look_by_port() {
        let l = |o: &str| {
            let l = output_look(o.into());
            (l.glyph, l.elsewhere)
        };
        assert_eq!(l("USB: K3"), (OutputGlyph::Headphones, true));
        assert_eq!(l("Wired headphones"), (OutputGlyph::Headphones, true));
        assert_eq!(l("Bluetooth: buds"), (OutputGlyph::Bluetooth, true));
        assert_eq!(l(outputs::SPEAKER), (OutputGlyph::Cast, false));
        assert_eq!(l("HDMI"), (OutputGlyph::Cast, true));
        let buds = output_look("Bluetooth: buds".into());
        assert_eq!((buds.port, buds.name.as_deref()), (OutputPort::Bluetooth, Some("buds")));
    }

    #[test]
    fn android_types_map_to_kinds() {
        let seen = outputs_refresh(vec![18, 2, 8, 22], vec!["".into(), "".into(), "Buds".into(), " K3 ".into()], vec![], None);
        assert_eq!(seen.current, "USB: K3");
        assert!(seen.usb);
        assert_eq!(seen.known.unwrap(), ["Bluetooth: Buds", "Phone speaker", "USB: K3"]);
        assert!(outputs_refresh(vec![12, 2], vec!["".into(), "".into()], vec![], None).usb, "a USB accessory");
        assert_eq!(outputs_refresh(vec![4], vec!["".into()], vec![], None).current, "Wired headphones");
    }

    #[test]
    fn dac_encodings_to_bits() {
        let d = dac_decide(true, true, "K3".into(), vec![44_100, 96_000], vec![2, 4], 96_000, 4, None, None, false);
        assert_eq!(d.step, DacStep::Prefer { index: 1 });
        let d = dac_decide(true, true, "K3".into(), vec![96_000], vec![22], 96_000, 4, None, None, false);
        assert_eq!(d.step, DacStep::Release, "float is not 32-bit integer");
        let d = dac_decide(true, true, "K3".into(), vec![96_000], vec![22], 96_000, 22, Some(vec![96_000]), Some(vec![22]), true);
        assert_eq!(d.step, DacStep::Keep);
    }

    #[test]
    fn dac_mock_parses() {
        assert_eq!(dac_mock("K3@44100/16,96000/24, 48000/float".into()), MockDac { name: "K3".into(), rates: vec![44_100, 96_000, 48_000], encodings: vec![2, 21, 4] });
        assert_eq!(dac_mock("@44100".into()), MockDac { name: "Mock DAC".into(), rates: vec![44_100], encodings: vec![2] });
        assert_eq!(dac_mock("K3".into()), MockDac { name: "Mock DAC".into(), rates: vec![], encodings: vec![] });
        assert_eq!(dac_mock("K3@x/16,44100/8".into()).rates, Vec::<u32>::new());
    }

    #[test]
    fn known_outputs_persist() {
        use nori_db::background;
        use nori_settings::settings_store::settings_open;

        let dir = nori_testdir::TempDir::new("outputs");
        let path = dir.join("nori.db").display().to_string();
        settings_open(path.clone()).unwrap();
        let speaker = outputs_speaker();
        assert_eq!(outputs_known(), std::slice::from_ref(&speaker), "the speaker the first time");
        let seen = outputs_refresh(vec![8], vec!["Buds".into()], vec![speaker.clone()], None);
        assert_eq!(seen.known.unwrap(), ["Bluetooth: Buds", speaker.as_str()]);
        background::flush();
        settings_open(path).unwrap();
        assert_eq!(outputs_known(), ["Bluetooth: Buds", speaker.as_str()]);
    }
}
