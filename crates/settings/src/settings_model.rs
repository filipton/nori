//! The settings as a model for a client's settings screen: each setting's spec by name, current values
//! in the options' form, and derived state. Layout and wording are the client's.

use std::collections::HashMap;

use nori_automix::beat_model;
use nori_player::automix::beats;

use crate::lyrics_sources;
use crate::codec::K;
use crate::settings::{row, value_of_special, SettingChange, StoredPrefs, ROWS, SPECIAL_SPECS};

/// What a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SettingKind {
    /// "true" or "false".
    Switch,
    /// One of [`SettingSpec::options`] (for the music folder: the server's folder ids, "" for all).
    Choice,
    /// A number from [`SettingSpec::min`] to [`SettingSpec::max`].
    Level,
    Text,
    /// One of [`SettingSpec::options`], as ARGB numbers.
    Colour,
}

/// One setting: its name for [`setting_set`], kind, offered values, range and default. Values are in
/// the form [`SettingsState::values`] uses, so they compare as strings.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SettingSpec {
    pub name: String,
    pub kind: SettingKind,
    pub options: Vec<String>,
    pub min: f32,
    pub max: f32,
    pub default: String,
}

fn spec(name: &str, k: &K) -> SettingSpec {
    let (kind, options, (min, max)): (SettingKind, Vec<String>, (f32, f32)) = match k {
        K::Switch => (SettingKind::Switch, vec!["false".into(), "true".into()], (0.0, 0.0)),
        K::Choice(o) | K::Named(o) => (SettingKind::Choice, o.iter().map(|s| s.to_string()).collect(), (0.0, 0.0)),
        K::Level(lo, hi) => (SettingKind::Level, Vec::new(), (*lo, *hi)),
        K::Text => (SettingKind::Text, Vec::new(), (0.0, 0.0)),
        K::Colour => (SettingKind::Colour, nori_look::theme::ACCENTS.iter().map(|c| (*c as i64).to_string()).collect(), (0.0, 0.0)),
    };
    let default = value_of(&StoredPrefs::default(), name).unwrap_or_default();
    SettingSpec { name: name.into(), kind, options, min, max, default }
}

/// Every offered setting, in a stable order: the table's, then [`SPECIAL_SPECS`].
pub fn specs() -> Vec<SettingSpec> {
    let table = ROWS.iter().filter_map(|r| Some(spec(r.name?, r.spec.as_ref()?)));
    table.chain(SPECIAL_SPECS.iter().map(|(n, k)| spec(n, k))).collect()
}

/// A setting's value in its options' form ("1", "0.75"). Lookup switches read off while the master
/// lookups switch is off.
pub fn value_of(p: &StoredPrefs, name: &str) -> Option<String> {
    if let Some(v) = value_of_special(p, name) {
        return Some(v);
    }
    let r = row(name)?;
    Some(if r.lookups && !p.third_party_lookups { false.to_string() } else { (r.show)(p) })
}

/// One lyrics service in rank order. Changed by name with `lyricsService:<id>`, `lyricsPlace`
/// (`<id>:<place>`) and `lyricsMove` (`<id>:-1`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct LyricsSource {
    pub id: String,
    pub on: bool,
    /// Asked only with the PaxSenix key.
    pub needs_key: bool,
}

/// The beat model's download state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BeatModel {
    /// Not in this build.
    Unavailable,
    /// Not downloaded yet; fetched the next time AutoMix measures a song.
    Absent,
    WaitingForWifi,
    Downloading,
    Ready,
    Failed { why: nori_automix::beat_model::BeatFailure },
}

/// Settings screen state derived by the core's rules.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SettingsState {
    /// Every setting's value ([`value_of`]), by name.
    pub values: HashMap<String, String>,
    /// Bit-perfect DAC output: transitions, silence skipping and the sound chain are out of the path.
    pub untouched: bool,
    pub sound_chain_on: bool,
    /// Offload is wanted but off because the sound chain is needed.
    pub offload_paused: bool,
    /// Every lyrics service in rank order.
    pub lyrics_sources: Vec<LyricsSource>,
    pub beat_model: BeatModel,
    /// Approximate beat model download size, MB.
    pub beat_model_mb: u32,
}

/// The output as the platform reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Output {
    /// A USB DAC in bit-perfect mode.
    pub dac_bit_perfect: bool,
    /// Something USB is attached.
    pub usb: bool,
}

fn beat_model_now() -> BeatModel {
    if !beats::AVAILABLE {
        return BeatModel::Unavailable;
    }
    match beat_model::state() {
        beat_model::State::Absent => BeatModel::Absent,
        beat_model::State::WaitingForWifi => BeatModel::WaitingForWifi,
        beat_model::State::Downloading => BeatModel::Downloading,
        // Ready only while the file is there.
        beat_model::State::Ready if beat_model::ready().is_some() => BeatModel::Ready,
        beat_model::State::Ready => BeatModel::Absent,
        beat_model::State::Failed(why) => BeatModel::Failed { why },
    }
}

/// [`SettingsState`] for these settings and this output.
pub fn state(p: &StoredPrefs, out: Output) -> SettingsState {
    let dsp = p.sound_chain_on();
    let prefs = nori_model::AudioPrefs {
        dsp,
        skip_silence: p.skip_silence,
        offload: p.offload,
        crossfade_s: p.crossfade_sec,
        auto_mix: p.auto_mix,
        speed: p.speed,
        pitch: p.pitch,
    };
    // A refused offload is only known to the playback service.
    let output = nori_model::OutputState { hi_res: p.hi_res, bit_perfect: out.dac_bit_perfect, usb: out.usb, offload_refused: false };
    let policy = nori_player::policy::audio_policy(&prefs, &output);
    let on = lyrics_sources::switched_on(p);
    let lyrics_sources = lyrics_sources::complete_order(&p.lyrics_order)
        .into_iter()
        .map(|s| LyricsSource { id: s.name().into(), on: on.contains(&s), needs_key: s.needs_key() })
        .collect();
    SettingsState {
        values: specs().into_iter().filter_map(|s| Some((s.name.clone(), value_of(p, &s.name)?))).collect(),
        untouched: out.dac_bit_perfect,
        sound_chain_on: dsp,
        offload_paused: !out.usb && p.offload && !policy.offload,
        lyrics_sources,
        beat_model: beat_model_now(),
        beat_model_mb: beat_model::SIZE_MB,
    }
}

/// The settings that differ from their defaults, one `name = value` per line, for a problem report.
/// Text settings (keys) are left out.
pub fn changed(p: &StoredPrefs) -> String {
    specs()
        .into_iter()
        .filter(|s| s.kind != SettingKind::Text)
        .filter_map(|s| {
            let v = value_of(p, &s.name)?;
            (v != s.default).then(|| format!("{} = {v}\n", s.name))
        })
        .collect()
}

/// [`changed`] for the live settings.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn settings_changed() -> String {
    changed(&crate::settings_store::settings_current().unwrap_or_default())
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn setting_specs() -> Vec<SettingSpec> {
    specs()
}

/// [`SettingsState`] for the live settings and the platform's output.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn settings_state(dac_bit_perfect: bool, usb: bool) -> SettingsState {
    let p = crate::settings_store::settings_current().unwrap_or_default();
    state(&p, Output { dac_bit_perfect, usb })
}

/// A change by name kept in the live settings (`settings_store::edit_by_name`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn setting_set(name: String, value: String) -> Option<SettingChange> {
    crate::settings_store::edit_by_name(&name, &value)
}

/// Whether the interface is dark for the theme setting and the system's mode.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn theme_is_dark(theme: crate::settings::ThemeMode, system_dark: bool) -> bool {
    use crate::settings::ThemeMode;
    match theme {
        ThemeMode::System => system_dark,
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
    }
}

/// Whether the status bar is hidden; `wide` is sideways.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn status_bar_hidden(hide: crate::settings::HideStatusBar, wide: bool) -> bool {
    use crate::settings::HideStatusBar::*;
    match hide {
        Never => false,
        Sideways => wide,
        Upright => !wide,
        Always => true,
    }
}

/// Whether the screen is kept on; `wide` is sideways.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn keep_awake(keep: crate::settings::KeepAwake, wide: bool, charging: bool) -> bool {
    use crate::settings::KeepAwake::*;
    match keep {
        Never => false,
        Sideways => wide,
        Charging => charging,
        SidewaysCharging => wide && charging,
        Always => true,
    }
}

/// Whether the setting depends on charging (only then is the charger watched).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn keep_awake_watches_charging(keep: crate::settings::KeepAwake) -> bool {
    use crate::settings::KeepAwake::*;
    matches!(keep, Charging | SidewaysCharging)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{set_by_name, SavedServer};

    #[test]
    fn changed_report_skips_text() {
        let p = StoredPrefs::default();
        assert_eq!(changed(&p), "");
        let switch = specs().into_iter().find(|s| s.kind == SettingKind::Switch).unwrap();
        let other = if switch.default == "true" { "false" } else { "true" };
        let p = set_by_name(&p, &switch.name, other).unwrap().prefs;
        let text = specs().into_iter().find(|s| s.kind == SettingKind::Text).unwrap();
        let p = set_by_name(&p, &text.name, "secret-key").unwrap().prefs;
        let said = changed(&p);
        assert!(said.lines().any(|l| l == format!("{} = {other}", switch.name)), "{said}");
        assert!(!said.contains("secret-key"), "{said}");
    }

    #[test]
    fn keep_awake_rules() {
        use crate::settings::KeepAwake::*;
        // (upright, on battery), (upright, charging), (sideways, on battery), (sideways, charging)
        let all = |k| [keep_awake(k, false, false), keep_awake(k, false, true), keep_awake(k, true, false), keep_awake(k, true, true)];
        assert_eq!(all(Never), [false, false, false, false]);
        assert_eq!(all(Sideways), [false, false, true, true]);
        assert_eq!(all(Charging), [false, true, false, true]);
        assert_eq!(all(SidewaysCharging), [false, false, false, true]);
        assert_eq!(all(Always), [true, true, true, true]);
        assert_eq!(
            [Never, Sideways, Charging, SidewaysCharging, Always].map(keep_awake_watches_charging),
            [false, false, true, true, false],
        );
    }

    #[test]
    fn status_bar_rules() {
        use crate::settings::HideStatusBar::*;
        let both = |h| (status_bar_hidden(h, false), status_bar_hidden(h, true));
        assert_eq!([both(Never), both(Sideways), both(Upright), both(Always)], [(false, false), (false, true), (true, false), (true, true)]);
    }

    #[test]
    fn every_offered_value_round_trips() {
        let p = StoredPrefs {
            servers: vec![SavedServer { id: "a".into(), ..SavedServer::default() }],
            active_server_id: "a".into(),
            ..StoredPrefs::default()
        };
        let all = specs();
        assert!(all.len() > 70, "{}", all.len());
        let mut names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len(), "each once");
        for s in &all {
            assert!(value_of(&p, &s.name).is_some(), "{} has no value", s.name);
            // Each value offered is taken by name, and reads back as itself.
            for o in &s.options {
                let after = set_by_name(&p, &s.name, o).unwrap_or_else(|| panic!("{} does not take {o}", s.name)).prefs;
                assert_eq!(value_of(&after, &s.name).as_deref(), Some(o.as_str()), "{} = {o}", s.name);
            }
            // Out of the box, a choice's value is one it offers.
            if s.kind == SettingKind::Choice && !s.options.is_empty() {
                assert!(s.options.contains(&s.default), "{}: {} is not offered", s.name, s.default);
            }
            if s.kind == SettingKind::Level {
                assert!(s.min < s.max, "{}", s.name);
                let mid = ((s.min + s.max) / 2.0).to_string();
                assert!(set_by_name(&p, &s.name, &mid).is_some(), "{}", s.name);
            }
        }
        assert!(set_by_name(&p, "lyricsService:LRCLIB", "false").is_some());
    }

    #[test]
    fn values_match_option_form() {
        let d = StoredPrefs::default();
        assert_eq!(value_of(&d, "speed").as_deref(), Some("1"));
        assert_eq!(value_of(&d, "mobile").as_deref(), Some("0:"), "the original file on mobile data out of the box");
        assert_eq!(value_of(&d, "wifi").as_deref(), Some("0:"));
        assert_eq!(value_of(&d, "theme").as_deref(), Some("SYSTEM"));
        assert_eq!(value_of(&d, "swipeLeft").as_deref(), Some("FAVOURITE"));
        assert_eq!(value_of(&d, "untaggedGainDb").as_deref(), Some("-6"));
        assert_eq!(value_of(&StoredPrefs { pitch: 0.9, ..d.clone() }, "pitch").as_deref(), Some("0.9"));
        assert_eq!(value_of(&d, "nope"), None);
        // In effect: off while looking things up is off.
        let off = StoredPrefs { third_party_lookups: false, ..d.clone() };
        assert_eq!(value_of(&d, "lyricsOnline").as_deref(), Some("true"));
        assert_eq!(value_of(&off, "lyricsOnline").as_deref(), Some("false"));
        assert_eq!(value_of(&off, "autoEqDownload").as_deref(), Some("false"));
        assert_eq!(value_of(&StoredPrefs { motion_artwork: true, ..off }, "motionArtwork").as_deref(), Some("false"));
    }

    #[test]
    fn state_rules() {
        let d = StoredPrefs::default();
        let s = state(&d, Output::default());
        assert!(!s.untouched && !s.sound_chain_on && !s.offload_paused);
        assert_eq!(s.values["crossfadeSec"], d.crossfade_sec.to_string());
        assert!(!state(&StoredPrefs { hi_res: true, ..d.clone() }, Output::default()).untouched, "high quality output keeps the chain");
        assert!(state(&d, Output { dac_bit_perfect: true, usb: true }).untouched);
        assert!(state(&StoredPrefs { mono: true, ..d.clone() }, Output::default()).sound_chain_on);
        // The battery saver stands down while an effect is on, but not over USB, where it is not offered.
        let eq = StoredPrefs { eq_enabled: true, offload: true, ..d.clone() };
        assert!(state(&eq, Output::default()).offload_paused);
        assert!(!state(&eq, Output { dac_bit_perfect: false, usb: true }).offload_paused);
        // No processing on this output: the effects are kept but out of the path, so offload comes back.
        let none = state(&StoredPrefs { sound_bypass: true, ..eq.clone() }, Output::default());
        assert!(!none.sound_chain_on && !none.offload_paused);
        // Every lyrics service, in the order they are asked, each on or off where it stands.
        let order: Vec<&str> = d.lyrics_order.iter().map(|s| s.name()).collect();
        assert_eq!(s.lyrics_sources.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), order);
        let on: Vec<&str> = s.lyrics_sources.iter().filter(|l| l.on).map(|l| l.id.as_str()).collect();
        assert_eq!(on, order, "every service on");
        let off = set_by_name(&d, "lyricsService:BINILYRICS", "false").unwrap().prefs;
        let s2 = state(&off, Output::default());
        assert_eq!(s2.lyrics_sources[1], LyricsSource { id: "BINILYRICS".into(), on: false, needs_key: false }, "switched off where it stands");
        assert!(s.lyrics_sources.iter().any(|l| l.needs_key));
        assert_eq!(s.beat_model == BeatModel::Unavailable, !beats::AVAILABLE);
        assert_eq!(s.beat_model_mb, beat_model::SIZE_MB);
    }
}
