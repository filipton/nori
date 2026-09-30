//! The settings record, its stored format (one value per key, plus the band list, sound profile and
//! server profile formats), defaults, ranges and edits. No database access (see settings_store.rs).

use std::collections::HashMap;

use nori_model::{EqBand, EqKind, EqPreset, NamedPreset, TransitionPrefs};
use serde_json::{Map, Value};

use crate::codec::{clamped, on, within, Choice, Custom, Picks, Preamp, Quality, Raw, Row, FLAG, FLOAT, INT, K, LONG, PICK, PICK_NEAREST, TEXT};
use crate::lyrics_sources::{self, LyricsService};
use crate::settings_store::{APPLY_AUDIO, APPLY_GAIN, CACHE_LIMIT, PLAYER, REPLAN, SOUND};

/// One stored value.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefValue {
    Flag { v: bool },
    Number { v: i32 },
    Big { v: i64 },
    Decimal { v: f32 },
    Text { v: String },
}

/// One equalizer filter. Kind and channel are stored as ordinals, so their order must not change.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SoundBand {
    pub kind: EqKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
    pub channel: BandChannel,
}

/// Which channel a band applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BandChannel {
    Both,
    Left,
    Right,
}

/// A band from kind and channel ordinals; out-of-range ordinals become the first variant.
pub fn band_from(kind: i32, freq: f32, gain_db: f32, q: f32, channel: i32) -> SoundBand {
    SoundBand { kind: EqKind::nth(kind).unwrap_or(EqKind::Peaking), freq, gain_db, q, channel: BandChannel::nth(channel).unwrap_or(BandChannel::Both) }
}

pub use nori_model::GainMode;

// Player enums as settings, stored by ordinal in declaration order.
impl Choice for GainMode {
    const ALL: &'static [Self] = &[GainMode::Off, GainMode::Track, GainMode::Album, GainMode::Auto];
    const NAMES: &'static [&'static str] = &["OFF", "TRACK", "ALBUM", "AUTO"];
}

impl Choice for EqKind {
    const ALL: &'static [Self] = &[
        EqKind::Peaking,
        EqKind::LowShelf,
        EqKind::HighShelf,
        EqKind::LowPass,
        EqKind::HighPass,
        EqKind::BandPass,
        EqKind::Notch,
        EqKind::AllPass,
        EqKind::LowShelfSlope,
        EqKind::HighShelfSlope,
    ];
    const NAMES: &'static [&'static str] =
        &["PEAKING", "LOW_SHELF", "HIGH_SHELF", "LOW_PASS", "HIGH_PASS", "BAND_PASS", "NOTCH", "ALL_PASS", "LOW_SHELF_SLOPE", "HIGH_SHELF_SLOPE"];
}

/// Which equalizer plays: parametric (free filters) or graphic (fixed ISO bands). Each keeps its settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum EqMode {
    Parametric,
    Graphic,
}

/// Whether downloads also run the beat model (`download_beats`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum DownloadBeats {
    Ask,
    Always,
    Never,
}

/// The plain crossfade's curve (`nori_player::transitions::shape_crossfade`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum CrossfadeCurve {
    /// Constant power.
    EqualPower,
    Linear,
    /// Sine-squared.
    SCurve,
}

impl CrossfadeCurve {
    pub fn player(self) -> nori_player::types::FadeCurve {
        match self {
            CrossfadeCurve::EqualPower => nori_player::types::FadeCurve::EqualPower,
            CrossfadeCurve::Linear => nori_player::types::FadeCurve::Linear,
            CrossfadeCurve::SCurve => nori_player::types::FadeCurve::SineSquared,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

/// When the status bar is hidden, by orientation (`wide` is sideways).
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum HideStatusBar {
    Never,
    Sideways,
    Upright,
    Always,
}

/// When the screen is kept on while the app is in front: by orientation and charging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum KeepAwake {
    Never,
    Sideways,
    Charging,
    SidewaysCharging,
    Always,
}

/// What a tap on a song in a list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TapAction {
    PlayList,
    PlayOne,
    Queue,
    PlayNext,
}

/// What dragging a song row sideways does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SwipeAction {
    None,
    Queue,
    PlayNext,
    Favourite,
    Download,
}

/// What autofill adds when the last song starts: songs, or one whole album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum AutoFillKind {
    Songs,
    Albums,
}

/// The highest output rate; above it songs are resampled down (`nori_player::policy::capped_rate`).
/// Part of a sound profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum MaxRate {
    Auto,
    Khz48,
    Khz96,
    Khz192,
}

impl MaxRate {
    /// Hz; 0 for no cap.
    pub fn hz(self) -> u32 {
        match self {
            MaxRate::Auto => 0,
            MaxRate::Khz48 => 48_000,
            MaxRate::Khz96 => 96_000,
            MaxRate::Khz192 => 192_000,
        }
    }
}

/// What autofill picks songs by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum AutoFillBasis {
    Similar,
    Artist,
    Genre,
    Era,
}

/// The home page's rows, stored by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum HomeRow {
    Pinned,
    Playlists,
    Recent,
    Newest,
    Frequent,
    TopSongs,
    Random,
    Starred,
}

/// One saved server profile. Each has its own rows in the index database.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SavedServer {
    pub id: String,
    pub name: String,
    pub url: String,
    pub alt_url: String,
    pub user: String,
    pub password: String,
    pub api_key: String,
    pub legacy_auth: bool,
    pub headers: HashMap<String, String>,
    pub allow_self_signed: bool,
    pub client_cert: String,
    pub client_cert_password: String,
    pub wifi_only: bool,
    pub music_folder_id: String,
    pub alt_max_bit_rate: i32,
}

/// One stream quality: `bit_rate` 0 and an empty `format` mean the original file.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SavedQuality {
    pub bit_rate: i32,
    pub format: String,
}

/// The stream qualities offered, the original file first.
const QUALITIES: &[&str] = &["0:", "320:mp3", "192:opus", "128:opus", "96:opus", "64:opus"];

/// Every setting. Each field's `#[setting(...)]` line declares its key, codec, default, name, client
/// spec and effect bits (see nori-settings-derive).
#[derive(Debug, Clone, PartialEq, nori_settings_derive::Settings)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StoredPrefs {
    // The servers.
    #[setting("servers", SERVERS, default = Vec::new(), hidden)]
    pub servers: Vec<SavedServer>,
    #[setting("activeServerId", TEXT, default = String::new(), hidden)]
    pub active_server_id: String,
    // Between songs.
    #[setting("crossfadeSec", INT, default = 0, show = K::Choice(&["0", "2", "4", "6", "8", "12"]), effect = APPLY_AUDIO | REPLAN, sound)]
    pub crossfade_sec: i32,
    #[setting("autoMix", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO | REPLAN)]
    pub auto_mix: bool,
    #[setting("autoMixMaxS", INT, default = 12, show = K::Choice(&["6", "8", "12", "16", "24"]), effect = REPLAN)]
    pub auto_mix_max_s: i32,
    #[setting("autoMixBeatMatch", FLAG, default = true, show = K::Switch, effect = REPLAN)]
    pub auto_mix_beat_match: bool,
    #[setting("autoMixMaxTempoPct", FLOAT, default = 6.0, show = K::Choice(&["2", "4", "6", "8"]), effect = REPLAN)]
    pub auto_mix_max_tempo_pct: f32,
    #[setting("autoMixKeepPitch", FLAG, default = true, show = K::Switch, effect = REPLAN)]
    pub auto_mix_keep_pitch: bool,
    #[setting("autoMixBassSwap", FLAG, default = true, show = K::Switch, effect = REPLAN)]
    pub auto_mix_bass_swap: bool,
    #[setting("autoMixFilters", FLAG, default = true, show = K::Switch, effect = REPLAN)]
    pub auto_mix_filters: bool,
    #[setting("autoMixEchoOut", FLAG, default = true, show = K::Switch, effect = REPLAN)]
    pub auto_mix_echo_out: bool,
    /// "Better beat detection": the Beat This! model on each upcoming song's ends, for AutoMix's beat
    /// grids. Needs the `neural-beats` feature and a one-time model download.
    #[setting("autoMixBetterBeats", FLAG, default = false, show = K::Switch)]
    pub auto_mix_better_beats: bool,
    /// The beat model may download over mobile data.
    #[setting("autoMixBeatsMobileData", FLAG, default = false, show = K::Switch)]
    pub auto_mix_beats_mobile_data: bool,
    #[setting("crossfadeKeepAlbums", FLAG, default = true, show = K::Switch, effect = REPLAN)]
    pub crossfade_keep_albums: bool,
    #[setting("crossfadeCurve", PICK, default = CrossfadeCurve::EqualPower, show = K::Named(CrossfadeCurve::NAMES), effect = REPLAN)]
    pub crossfade_curve: CrossfadeCurve,
    /// Fade-in/out length within a plain crossfade, seconds; 0 is the whole crossfade.
    #[setting("crossfadeInSec", clamped(0, 12), default = 0, show = K::Choice(&["0", "1", "2", "4", "6", "8"]), effect = REPLAN)]
    pub crossfade_in_sec: i32,
    #[setting("crossfadeOutSec", clamped(0, 12), default = 0, show = K::Choice(&["0", "1", "2", "4", "6", "8"]), effect = REPLAN)]
    pub crossfade_out_sec: i32,
    #[setting("fadeMs", within(0, 5000), default = 0, show = K::Choice(&["0", "150", "300", "500", "1000"]), effect = PLAYER)]
    pub fade_ms: i32,
    // Controls.
    #[setting("previousAlwaysSkips", FLAG, default = false, show = K::Switch)]
    pub previous_always_skips: bool,
    #[setting("speed", within(RATE.0, RATE.1), default = 1.0, show = K::Choice(&["0.75", "1", "1.25", "1.5", "2"]), effect = APPLY_AUDIO)]
    pub speed: f32,
    #[setting("pitch", within(RATE.0, RATE.1), default = 1.0, show = K::Choice(&["0.9", "0.95", "1", "1.05", "1.1"]), effect = APPLY_AUDIO)]
    pub pitch: f32,
    #[setting("skipSilence", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO)]
    pub skip_silence: bool,
    // The queue.
    #[setting("skipExplicit", FLAG, default = false, show = K::Switch)]
    pub skip_explicit: bool,
    #[setting("autoFill", FLAG, default = true, show = K::Switch)]
    pub auto_fill: bool,
    #[setting("autoFillKind", PICK, default = AutoFillKind::Songs, show = K::Named(AutoFillKind::NAMES))]
    pub auto_fill_kind: AutoFillKind,
    #[setting("autoFillBasis", PICK, default = AutoFillBasis::Similar, show = K::Named(AutoFillBasis::NAMES))]
    pub auto_fill_basis: AutoFillBasis,
    /// Autofill may queue provider songs (octo-fiesta downloads each one played into the library).
    #[setting("autoFillRemote", FLAG, default = false, show = K::Switch)]
    pub auto_fill_remote: bool,
    #[setting("skipOnError", FLAG, default = true, show = K::Switch)]
    pub skip_on_error: bool,
    #[setting("bridgeOffline", FLAG, default = false, show = K::Switch)]
    pub bridge_offline: bool,
    // Sound.
    #[setting("eqEnabled", FLAG, default = false, name = "eq", show = K::Switch, effect = APPLY_AUDIO | SOUND, sound)]
    pub eq_enabled: bool,
    #[setting("eqBands", BANDS, default = graphic(), hidden, effect = SOUND, sound)]
    pub eq_bands: Vec<SoundBand>,
    /// Graphic on a new install; older installs with a parametric setup keep it ([`load`]).
    #[setting("eqMode", PICK, default = EqMode::Graphic, show = K::Named(EqMode::NAMES), effect = SOUND, sound)]
    pub eq_mode: EqMode,
    /// Graphic sliders, dB, low to high: 5, 10, 15 or 31 (`nori_player::graphic`).
    #[setting("eqGraphic", GRAPHIC, default = vec![0.0; 10], hidden, effect = SOUND, sound)]
    pub eq_graphic: Vec<f32>,
    /// The headphone correction the sliders were fitted to (dB on `graphic::target_grid`), refitted on a
    /// layout change; empty once a slider is moved by hand.
    #[setting("eqGraphicTarget", TARGET, default = Vec::new(), hidden, effect = SOUND, sound)]
    pub eq_graphic_target: Vec<f32>,
    /// Low shelf gain, dB; 0 is off.
    #[setting("bassBoostDb", clamped(0.0, BASS_BOOST_MAX), default = 0.0, show = K::Level(0.0, BASS_BOOST_MAX), effect = SOUND, effects)]
    pub bass_boost_db: f32,
    /// Virtualizer strength, 0 (off) to 1.
    #[setting("virtualizer", clamped(0.0, 1.0), default = 0.0, show = K::Level(0.0, 1.0), effect = SOUND, effects)]
    pub virtualizer: f32,
    /// Extra gain, dB, with the limiter behind it; 0 is off.
    #[setting("volumeBoostDb", clamped(0.0, VOLUME_BOOST_MAX), default = 0.0, show = K::Level(0.0, VOLUME_BOOST_MAX), effect = SOUND, effects)]
    pub volume_boost_db: f32,
    #[setting("compressor", FLAG, default = false, show = K::Switch, effect = SOUND, effects)]
    pub compressor: bool,
    #[setting("compThresholdDb", clamped(-60.0, 0.0), default = -20.0, show = K::Level(-60.0, 0.0), effect = SOUND, effects)]
    pub comp_threshold_db: f32,
    #[setting("compRatio", clamped(1.0, 20.0), default = 3.0, show = K::Level(1.0, 20.0), effect = SOUND, effects)]
    pub comp_ratio: f32,
    #[setting("compAttackMs", clamped(0.1, 200.0), default = 10.0, show = K::Level(0.1, 200.0), effect = SOUND, effects)]
    pub comp_attack_ms: f32,
    #[setting("compReleaseMs", clamped(10.0, 2000.0), default = 180.0, show = K::Level(10.0, 2000.0), effect = SOUND, effects)]
    pub comp_release_ms: f32,
    #[setting("compMakeupDb", clamped(0.0, 24.0), default = 4.5, show = K::Level(0.0, 24.0), effect = SOUND, effects)]
    pub comp_makeup_db: f32,
    #[setting("compKneeDb", clamped(0.0, 24.0), default = 6.0, show = K::Level(0.0, 24.0), effect = SOUND, effects)]
    pub comp_knee_db: f32,
    /// Downward expander (a noise gate at high ratios), before the compressor.
    #[setting("expander", FLAG, default = false, show = K::Switch, effect = SOUND, effects)]
    pub expander: bool,
    #[setting("expThresholdDb", clamped(-90.0, -10.0), default = -50.0, show = K::Level(-90.0, -10.0), effect = SOUND, effects)]
    pub exp_threshold_db: f32,
    #[setting("expRatio", clamped(1.0, 20.0), default = 2.0, show = K::Level(1.0, 20.0), effect = SOUND, effects)]
    pub exp_ratio: f32,
    #[setting("expAttackMs", clamped(0.1, 100.0), default = 5.0, show = K::Level(0.1, 100.0), effect = SOUND, effects)]
    pub exp_attack_ms: f32,
    #[setting("expReleaseMs", clamped(10.0, 2000.0), default = 150.0, show = K::Level(10.0, 2000.0), effect = SOUND, effects)]
    pub exp_release_ms: f32,
    /// Volume-dependent loudness compensation (ISO 226, `nori_player::contour`). The platform reports
    /// its volume only while this is on.
    #[setting("loudness", FLAG, default = false, show = K::Switch, effect = SOUND, effects)]
    pub loudness: bool,
    /// The reference level at full volume, phon.
    #[setting("loudnessRefPhon", clamped(60, 90), default = 80, show = K::Choice(&["70", "75", "80", "85", "90"]), effect = SOUND, effects)]
    pub loudness_ref_phon: i32,
    #[setting("mono", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO | SOUND, sound)]
    pub mono: bool,
    #[setting("limiter", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO | SOUND, sound)]
    pub limiter: bool,
    #[setting("eqPreampDb", Preamp(EQ_RANGES.preamp), default = None, show = K::Level(EQ_RANGES.preamp.min, EQ_RANGES.preamp.max), effect = SOUND, sound)]
    pub eq_preamp_db: Option<f32>,
    #[setting("crossfeedDb", FLOAT, default = 0.0, show = K::Level(EQ_RANGES.crossfeed.min, EQ_RANGES.crossfeed.max), effect = SOUND, sound)]
    pub crossfeed_db: f32,
    /// Crossfeed cutoff (bs2b's `fcut`), Hz.
    #[setting("crossfeedHz", clamped(300.0, 2000.0), default = 700.0, show = K::Level(EQ_RANGES.crossfeed_cut.min, EQ_RANGES.crossfeed_cut.max), effect = SOUND, sound)]
    pub crossfeed_hz: f32,
    #[setting("balance", FLOAT, default = 0.0, hidden, effect = SOUND, sound)]
    pub balance: f32,
    #[setting("limiterThresholdDb", FLOAT, default = -1.0, show = K::Level(EQ_RANGES.limiter.min, EQ_RANGES.limiter.max), effect = SOUND, sound)]
    pub limiter_threshold_db: f32,
    #[setting("autoEqAuto", FLAG, default = false, show = K::Switch)]
    pub auto_eq_auto: bool,
    /// Keep the AutoEQ index on the device (`nori_devices::autoeq::index_due`).
    #[setting("autoEqDownload", FLAG, default = true, show = K::Switch, lookups)]
    pub auto_eq_download: bool,
    /// "No processing on this output": the sound chain is left out. ReplayGain and transitions still
    /// apply. Part of a sound profile.
    #[setting("soundBypass", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO | SOUND, sound = bypass)]
    pub sound_bypass: bool,
    #[setting("profilePerOutput", FLAG, default = true, show = K::Switch)]
    pub profile_per_output: bool,
    /// Also decides whether songs may be turned up (`gain_boost_db`), hence the SOUND bit.
    #[setting("replayGain", PICK_NEAREST, default = GainMode::Off, show = K::Named(GainMode::NAMES), effect = APPLY_GAIN | REPLAN | SOUND, sound)]
    pub replay_gain: GainMode,
    /// Target loudness, LUFS (`nori_player::gain`).
    #[setting("loudnessTarget", FLOAT, default = -18.0, show = K::Choice(&["-14", "-16", "-18", "-23"]), effect = APPLY_GAIN)]
    pub loudness_target: f32,
    /// Most a quiet song is turned up, dB. Above 0, boosted songs play through the CPU chain (float,
    /// limited).
    #[setting("gainBoostDb", FLOAT, default = 0.0, show = K::Choice(&["0", "3", "6", "9", "12"]), effect = APPLY_GAIN | SOUND)]
    pub gain_boost_db: f32,
    /// Untagged songs use the loudness measured by AutoMix's analysis, when there is one.
    #[setting("gainMeasured", FLAG, default = true, show = K::Switch, effect = APPLY_GAIN)]
    pub gain_measured: bool,
    #[setting("preampDb", within(REPLAY_GAIN_PREAMP.0, REPLAY_GAIN_PREAMP.1), default = 0.0, show = K::Level(EQ_RANGES.replay_gain_preamp.min, EQ_RANGES.replay_gain_preamp.max), effect = APPLY_GAIN, sound)]
    pub preamp_db: f32,
    #[setting("untaggedGainDb", FLOAT, default = -6.0, show = K::Choice(&["0", "-3", "-6", "-9", "-12"]), effect = APPLY_GAIN)]
    pub untagged_gain_db: f32,
    #[setting("hiRes", FLAG, default = false, show = K::Switch, effect = PLAYER, sound)]
    pub hi_res: bool,
    #[setting("maxRate", PICK, default = MaxRate::Auto, show = K::Named(MaxRate::NAMES), effect = PLAYER, sound)]
    pub max_rate: MaxRate,
    #[setting("bitPerfect", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO, sound)]
    pub bit_perfect: bool,
    #[setting("offload", FLAG, default = true, show = K::Switch, effect = APPLY_AUDIO)]
    pub offload: bool,
    // Appearance.
    #[setting("theme", PICK, default = ThemeMode::System, show = K::Named(ThemeMode::NAMES))]
    pub theme: ThemeMode,
    #[setting("hideStatusBar", PICK, default = HideStatusBar::Sideways, show = K::Named(HideStatusBar::NAMES))]
    pub hide_status_bar: HideStatusBar,
    #[setting("keepAwake", PICK, default = KeepAwake::Never, show = K::Named(KeepAwake::NAMES))]
    pub keep_awake: KeepAwake,
    #[setting("amoled", FLAG, default = false, show = K::Switch)]
    pub amoled: bool,
    /// With AMOLED black, which pages keep their cover colours ([`nori_look::sleeve::page_black`]).
    #[setting("playerColours", FLAG, default = true, show = K::Switch)]
    pub player_colours: bool,
    #[setting("albumColours", FLAG, default = false, show = K::Switch)]
    pub album_colours: bool,
    #[setting("artistColours", FLAG, default = false, show = K::Switch)]
    pub artist_colours: bool,
    #[setting("dynamicColor", FLAG, default = true, show = K::Switch)]
    pub dynamic_color: bool,
    #[setting("accent", LONG, default = 0xFF6750A4, show = K::Colour)]
    pub accent: i64,
    #[setting("coverColors", FLAG, default = true, show = K::Switch)]
    pub cover_colors: bool,
    #[setting("softSleeve", FLAG, default = true, show = K::Switch)]
    pub soft_sleeve: bool,
    /// Apple Music motion artwork in the player.
    #[setting("motionArtwork", FLAG, default = false, show = K::Switch, lookups)]
    pub motion_artwork: bool,
    /// Motion artwork only on unmetered networks (each is a few MB).
    #[setting("motionArtworkWifiOnly", FLAG, default = true, show = K::Switch)]
    pub motion_artwork_wifi_only: bool,
    #[setting("favouriteNotice", FLAG, default = true, show = K::Switch)]
    pub favourite_notice: bool,
    #[setting("uiScale", FLOAT, default = 0.0, show = K::Choice(&["0", "0.9", "1", "1.1"]))]
    pub ui_scale: f32,
    #[setting("reduceMotion", FLAG, default = false, show = K::Switch)]
    pub reduce_motion: bool,
    /// Animate even with Android's animations off. Stored under a new key so the old key's stored
    /// `false` does not apply.
    #[setting("animateAnyway", FLAG, default = true, name = "ignoreSystemMotion", show = K::Switch)]
    pub ignore_system_motion: bool,
    // Lyrics.
    #[setting("lyricsSweep", FLAG, default = true, show = K::Switch)]
    pub lyrics_sweep: bool,
    #[setting("lyricsSize", within(0, 2), default = 1, show = K::Choice(&["0", "1", "2"]))]
    pub lyrics_size: i32,
    #[setting("lyricsTranslation", FLAG, default = true, show = K::Switch)]
    pub lyrics_translation: bool,
    /// The button under the lyrics that opens the timing offset controls.
    #[setting("lyricsTimingButton", FLAG, default = true, show = K::Switch)]
    pub lyrics_timing_button: bool,
    #[setting("lyricsKeepScreenOn", FLAG, default = true, show = K::Switch)]
    pub lyrics_keep_screen_on: bool,
    /// Look lyrics up online when the server has no timed ones. Stored under its old key "lyricsLrclib".
    #[setting("lyricsLrclib", FLAG, default = true, name = "lyricsOnline", show = K::Switch, lookups)]
    pub lyrics_online: bool,
    /// Every lyrics service, in rank order; stored by name (`lyrics_sources`).
    #[setting("lyricsOrder", LYRICS_ORDER, default = lyrics_sources::default_order())]
    pub lyrics_order: Vec<LyricsService>,
    /// The lyrics services switched on (changed by `lyricsService:<name>`).
    #[setting("lyricsOn", LYRICS_ON, default = lyrics_sources::default_order(), hidden)]
    pub lyrics_on: Vec<LyricsService>,
    /// Keep asking lower-ranked services for word timing after a line-timed answer.
    #[setting("lyricsPreferWords", FLAG, default = true, show = K::Switch)]
    pub lyrics_prefer_words: bool,
    /// PaxSenix key, for its Spotify and Musixmatch lyrics; empty for none.
    #[setting("paxSenixKey", TEXT, default = String::new(), show = K::Text)]
    pub paxsenix_key: String,
    /// BetterLyrics key, for songs it has not cached; empty for none.
    #[setting("betterLyricsKey", TEXT, default = String::new(), show = K::Text)]
    pub better_lyrics_key: String,
    // The library.
    #[setting("tapAction", PICK, default = TapAction::PlayList, show = K::Named(TapAction::NAMES))]
    pub tap_action: TapAction,
    #[setting("swipeRight", PICK, default = SwipeAction::Queue, show = K::Named(SwipeAction::NAMES))]
    pub swipe_right: SwipeAction,
    #[setting("swipeLeft", PICK, default = SwipeAction::Favourite, show = K::Named(SwipeAction::NAMES))]
    pub swipe_left: SwipeAction,
    #[setting("liveSearchDelayMs", INT, default = 350, show = K::Choice(&["150", "250", "350", "500", "800"]))]
    pub live_search_delay_ms: i32,
    /// Show playlist descriptions ([`nori_library::pages::playlist_description`]).
    #[setting("playlistDescriptions", FLAG, default = true, show = K::Switch)]
    pub playlist_descriptions: bool,
    /// Hide the note the server leaves on playlists it imported from files.
    #[setting("hideImportNotes", FLAG, default = true, show = K::Switch)]
    pub hide_import_notes: bool,
    #[setting("tasteModel", FLAG, default = true, show = K::Switch)]
    pub taste_model: bool,
    #[setting("scrobble", FLAG, default = true, show = K::Switch)]
    pub scrobble: bool,
    #[setting("scrobblePercent", INT, default = 50, show = K::Choice(&["25", "50", "75", "90", "100"]))]
    pub scrobble_percent: i32,
    /// Master switch over third-party lookups (lyrics, AutoEQ, motion artwork), each with its own switch.
    #[setting("thirdPartyLookups", FLAG, default = true, show = K::Switch)]
    pub third_party_lookups: bool,
    /// Check GitHub for a new release at most daily (nori-core update.rs). Not under the lookups switch.
    #[setting("updateCheck", FLAG, default = true, show = K::Switch)]
    pub update_check: bool,
    /// The home page's rows, in order; unlisted rows are hidden.
    #[setting("homeRows", Picks, default = HomeRow::ALL.to_vec(), hidden)]
    pub home_rows: Vec<HomeRow>,
    #[setting("pinnedPlaylists", LINES, default = Vec::new(), hidden)]
    pub pinned_playlists: Vec<String>,
    #[setting("listPrefs", LIST_PREFS, default = HashMap::new(), hidden)]
    pub list_prefs: HashMap<String, String>,
    // Downloads and storage.
    #[setting("wifi", Quality, default = SavedQuality::default(), show = K::Choice(QUALITIES))]
    pub wifi: SavedQuality,
    #[setting("mobile", Quality, default = SavedQuality::default(), show = K::Choice(QUALITIES))]
    pub mobile: SavedQuality,
    #[setting("download", Quality, default = SavedQuality::default(), show = K::Choice(QUALITIES))]
    pub download: SavedQuality,
    /// Concurrent downloads; the rest queue in order.
    #[setting("parallelDownloads", clamped(1, 10), default = 5, show = K::Choice(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]))]
    pub parallel_downloads: i32,
    /// Run the beat model on downloaded songs so playback never needs to. Offered only with better beat
    /// detection on (`nori_transfers::transfers::beats_offer`).
    #[setting("downloadBeats", PICK, default = DownloadBeats::Ask, show = K::Named(DownloadBeats::NAMES))]
    pub download_beats: DownloadBeats,
    #[setting("precacheWifi", INT, default = 2, show = K::Choice(&["1", "2", "3", "5", "10"]))]
    pub precache_wifi: i32,
    #[setting("precacheMobile", INT, default = 1, show = K::Choice(&["1", "2", "3", "5"]))]
    pub precache_mobile: i32,
    /// Upcoming songs' covers fetched ahead.
    #[setting("coversAhead", clamped(0, 10), default = 3, show = K::Choice(&["0", "1", "2", "3", "5", "8", "10"]))]
    pub covers_ahead: i32,
    #[setting("cacheMb", within(256, 16384), default = 1024, show = K::Choice(&["256", "1024", "4096", "16384"]), effect = CACHE_LIMIT)]
    pub cache_mb: i32,
}

/// Offered settings that are not a field of their own; read and changed in [`value_of_special`] and
/// [`set_special`].
pub(crate) const SPECIAL_SPECS: &[(&str, K)] = &[
    ("motionArtworkMobile", K::Switch),
    ("musicFolder", K::Choice(&[])),
    ("altMaxBitRate", K::Choice(&["0", "320", "192", "128", "96"])),
    ("compressorPreset", K::Choice(&COMPRESSOR_PRESETS)),
    ("crossfeedPreset", K::Choice(&CROSSFEED_PRESETS)),
    ("eqLayout", K::Choice(&["5", "10", "15", "31"])),
];

const SERVERS: Custom<Vec<SavedServer>> = Custom {
    // An unreadable list, or a server without an id, loses the whole list.
    load: |t, _| {
        t.and_then(|json| serde_json::from_str::<Value>(json).ok())
            .and_then(|v| v.as_array().map(|a| a.iter().map(server_from).collect::<Option<Vec<_>>>()))
            .flatten()
            .unwrap_or_default()
    },
    save: |s| Value::Array(s.iter().map(server_json).collect()).to_string(),
    set: None,
    show: None,
};

/// "kind:freq:gain:q:channel" per band, bands joined by ';'.
const BANDS: Custom<Vec<SoundBand>> = Custom { load: |t, d| t.and_then(decode_bands).unwrap_or(d), save: |b| encode_bands(b), set: None, show: None };

/// The graphic sliders, comma-separated; anything but a valid layout loads as the default.
const GRAPHIC: Custom<Vec<f32>> = Custom { load: |t, d| t.and_then(decode_graphic).unwrap_or(d), save: |g| encode_floats(g), set: Some(decode_graphic), show: Some(|g| encode_floats(g)) };

/// A headphone correction's target, comma-separated, one value per grid point, or empty.
const TARGET: Custom<Vec<f32>> = Custom { load: |t, d| t.map_or(d, decode_target), save: |g| encode_floats(g), set: None, show: None };

fn encode_floats(g: &[f32]) -> String {
    g.iter().map(|v| kotlin_float(*v)).collect::<Vec<_>>().join(",")
}

/// A target from its text; empty when invalid.
pub(crate) fn decode_target(s: &str) -> Vec<f32> {
    let g: Option<Vec<f32>> = s.split(',').map(float).collect();
    g.filter(|g| g.len() == nori_player::graphic::TARGET_POINTS && g.iter().all(|v| v.is_finite())).unwrap_or_default()
}

/// Graphic sliders from their text: one number per band of a valid layout, held to the gain range.
pub(crate) fn decode_graphic(s: &str) -> Option<Vec<f32>> {
    let g: Vec<f32> = s.split(',').map(|v| float(v).map(|v| EQ_RANGES.gain.hold(v))).collect::<Option<_>>()?;
    nori_player::graphic::LAYOUTS.contains(&g.len()).then_some(g)
}

/// Bass and volume boost limits, dB.
const BASS_BOOST_MAX: f32 = nori_player::dsp::BASS_BOOST_MAX_DB as f32;
const VOLUME_BOOST_MAX: f32 = nori_player::dsp::VOLUME_BOOST_MAX_DB as f32;

/// The stored ranking, completed with any service it does not name.
const LYRICS_ORDER: Custom<Vec<LyricsService>> = Custom {
    load: |t, _| lyrics_sources::complete_order(&lyrics_sources::parse(t.unwrap_or_default())),
    save: |o| lyrics_sources::to_names(o),
    set: Some(|v| Some(lyrics_sources::complete_order(&lyrics_sources::parse(v)))),
    show: Some(|o| lyrics_sources::to_names(o)),
};

const LYRICS_ON: Custom<Vec<LyricsService>> = Custom { load: |t, d| t.map_or(d, lyrics_sources::parse), save: |o| lyrics_sources::to_names(o), set: None, show: None };

/// One per line, empty lines dropped.
const LINES: Custom<Vec<String>> =
    Custom { load: |t, _| t.map_or_else(Vec::new, |s| s.split('\n').filter(|p| !p.is_empty()).map(str::to_string).collect()), save: |l| l.join("\n"), set: None, show: None };

/// A JSON object of strings; empty when unreadable.
const LIST_PREFS: Custom<HashMap<String, String>> = Custom {
    load: |t, _| t.and_then(|j| serde_json::from_str::<Value>(j).ok()).and_then(|v| string_map(&v)).unwrap_or_default(),
    save: |m| serde_json::to_string(m).unwrap_or_else(|_| "{}".to_string()),
    set: None,
    show: None,
};

/// The part of the settings a sound profile remembers.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SoundSettings {
    pub eq_enabled: bool,
    pub eq_bands: Vec<SoundBand>,
    pub eq_mode: EqMode,
    pub eq_graphic: Vec<f32>,
    pub eq_graphic_target: Vec<f32>,
    pub eq_preamp_db: Option<f32>,
    pub crossfeed_db: f32,
    pub crossfeed_hz: f32,
    pub balance: f32,
    pub mono: bool,
    /// No processing on this output.
    pub bypass: bool,
    pub limiter: bool,
    pub limiter_threshold_db: f32,
    pub effects: SoundEffects,
    pub replay_gain: GainMode,
    pub preamp_db: f32,
    pub crossfade_sec: i32,
    pub hi_res: bool,
    pub max_rate: MaxRate,
    pub bit_perfect: bool,
}

/// The effects besides the equalizer.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SoundEffects {
    pub bass_boost_db: f32,
    pub virtualizer: f32,
    pub volume_boost_db: f32,
    pub compressor: bool,
    pub comp_threshold_db: f32,
    pub comp_ratio: f32,
    pub comp_attack_ms: f32,
    pub comp_release_ms: f32,
    pub comp_makeup_db: f32,
    pub comp_knee_db: f32,
    pub expander: bool,
    pub exp_threshold_db: f32,
    pub exp_ratio: f32,
    pub exp_attack_ms: f32,
    pub exp_release_ms: f32,
    pub loudness: bool,
    pub loudness_ref_phon: i32,
}

impl Default for SoundEffects {
    fn default() -> Self {
        StoredPrefs::default().effects()
    }
}

impl SoundEffects {
    /// Whether any effect alters the samples.
    pub fn on(&self) -> bool {
        self.player().on()
    }

    /// The compressor's controls, whether it is on or not.
    pub(crate) fn compressor_settings(&self) -> nori_player::compressor::CompressorSettings {
        nori_player::compressor::CompressorSettings {
            threshold_db: self.comp_threshold_db as f64,
            ratio: self.comp_ratio as f64,
            attack_ms: self.comp_attack_ms as f64,
            release_ms: self.comp_release_ms as f64,
            makeup_db: self.comp_makeup_db as f64,
            knee_db: self.comp_knee_db as f64,
        }
    }

    /// The expander's controls, whether it is on or not.
    pub(crate) fn expander_settings(&self) -> nori_player::compressor::ExpanderSettings {
        nori_player::compressor::ExpanderSettings {
            threshold_db: self.exp_threshold_db as f64,
            ratio: self.exp_ratio as f64,
            attack_ms: self.exp_attack_ms as f64,
            release_ms: self.exp_release_ms as f64,
        }
    }

    /// The chain's effects, with loudness compensation for `volume_db` (0 is full volume).
    pub fn player_at(&self, volume_db: f64) -> nori_player::dsp::Effects {
        nori_player::dsp::Effects {
            loudness: self.loudness.then(|| nori_player::contour::Loudness { reference_phon: self.loudness_ref_phon as f64, volume_db }),
            ..self.player()
        }
    }

    /// The chain's effects at full volume.
    pub fn player(&self) -> nori_player::dsp::Effects {
        nori_player::dsp::Effects {
            bass_boost_db: self.bass_boost_db as f64,
            compressor: self.compressor.then(|| self.compressor_settings()),
            expander: self.expander.then(|| self.expander_settings()),
            loudness: self.loudness.then(|| nori_player::contour::Loudness { reference_phon: self.loudness_ref_phon as f64, volume_db: 0.0 }),
            virtualizer: self.virtualizer as f64,
            boost_db: self.volume_boost_db as f64,
        }
    }

    /// The built-in compressor preset these match, if any.
    pub fn compressor_preset(&self) -> Option<nori_player::compressor::CompressorPreset> {
        let now = self.compressor_settings();
        nori_player::compressor::CompressorPreset::ALL.into_iter().find(|p| {
            let s = p.settings();
            [s.threshold_db - now.threshold_db, s.ratio - now.ratio, s.attack_ms - now.attack_ms, s.release_ms - now.release_ms, s.makeup_db - now.makeup_db, s.knee_db - now.knee_db]
                .iter()
                .all(|d| d.abs() < 1e-3)
        })
    }

    /// A compressor preset applied, and the compressor on.
    pub(crate) fn with_compressor_preset(self, p: nori_player::compressor::CompressorPreset) -> SoundEffects {
        let s = p.settings();
        SoundEffects {
            compressor: true,
            comp_threshold_db: s.threshold_db as f32,
            comp_ratio: s.ratio as f32,
            comp_attack_ms: s.attack_ms as f32,
            comp_release_ms: s.release_ms as f32,
            comp_makeup_db: s.makeup_db as f32,
            comp_knee_db: s.knee_db as f32,
            ..self
        }
    }
}

impl StoredPrefs {
    /// Whether the sound chain is needed (anything that alters samples is on and the output is not
    /// bypassed). Audio offload is off while it is.
    pub fn sound_chain_on(&self) -> bool {
        !self.sound_bypass && nori_player::sound::sound_on(self.eq_enabled, self.crossfeed_db, self.balance, self.mono, self.limiter, self.effects().on())
    }

    /// How songs are levelled (`nori_player::gain`).
    pub fn gain_prefs(&self) -> nori_player::gain::GainPrefs {
        nori_player::gain::GainPrefs {
            mode: self.replay_gain,
            preamp_db: self.preamp_db,
            untagged_db: self.untagged_gain_db,
            target_lufs: self.loudness_target,
            boost_max_db: self.gain_boost_db,
            measured: self.gain_measured,
        }
    }

    /// What the transition planner reads from the settings (`nori_automix::planner::settings_from`).
    pub(crate) fn transition_prefs(&self) -> TransitionPrefs {
        TransitionPrefs {
            auto_mix: self.auto_mix,
            crossfade_s: self.crossfade_sec,
            auto_mix_max_s: self.auto_mix_max_s,
            beat_match: self.auto_mix_beat_match,
            max_tempo_change_pct: self.auto_mix_max_tempo_pct,
            bass_swap: self.auto_mix_bass_swap,
            filter_effects: self.auto_mix_filters,
            echo_out: self.auto_mix_echo_out,
            keep_pitch: self.auto_mix_keep_pitch,
            keep_albums: self.crossfade_keep_albums,
            fade_curve: self.crossfade_curve.player(),
            fade_in_ms: self.crossfade_in_sec * 1000,
            fade_out_ms: self.crossfade_out_sec * 1000,
            // AutoMix skips its loudness matching under ReplayGain.
            replay_gain: self.replay_gain != GainMode::Off,
        }
    }
}

/// A failed sound edit. Messages are for the log.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error))]
#[cfg_attr(feature = "ffi", uniffi(flat_error))]
pub enum SoundError {
    #[error("no filters in the preset")]
    NoFilters,
    #[error("database: {0}")]
    Db(String),
}

impl From<rusqlite::Error> for SoundError {
    fn from(e: rusqlite::Error) -> Self {
        SoundError::Db(e.to_string())
    }
}

impl From<nori_model::CoreError> for SoundError {
    fn from(e: nori_model::CoreError) -> Self {
        SoundError::Db(e.to_string())
    }
}

/// Speed and pitch range.
const RATE: (f32, f32) = (0.25, 4.0);
/// ReplayGain pre-amp range, dB.
pub(crate) const REPLAY_GAIN_PREAMP: (f32, f32) = (-12.0, 6.0);

/// The ten default bands: flat peaking filters an octave apart.
pub fn graphic() -> Vec<SoundBand> {
    [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0]
        .into_iter()
        .map(|freq| SoundBand { kind: EqKind::Peaking, freq, gain_db: 0.0, q: 1.41, channel: BandChannel::Both })
        .collect()
}

/// Reads a band list ("kind:freq:gain:q[:channel]" joined by ';'). Unreadable bands are dropped; None
/// when none is left.
pub(crate) fn decode_bands(s: &str) -> Option<Vec<SoundBand>> {
    let bands: Vec<SoundBand> = s
        .split(';')
        .filter_map(|b| {
            let p: Vec<&str> = b.split(':').collect();
            if p.len() < 4 {
                return None;
            }
            let kind = EqKind::nth(p[0].parse().ok()?)?;
            let channel = match p.get(4) {
                Some(c) => BandChannel::nth(c.parse().ok()?)?,
                None => BandChannel::Both,
            };
            Some(SoundBand { kind, freq: float(p[1])?, gain_db: float(p[2])?, q: float(p[3])?, channel })
        })
        .collect();
    (!bands.is_empty()).then_some(bands)
}

pub fn encode_bands(bands: &[SoundBand]) -> String {
    let each: Vec<String> =
        bands.iter().map(|b| format!("{}:{}:{}:{}:{}", b.kind as i32, kotlin_float(b.freq), kotlin_float(b.gain_db), kotlin_float(b.q), b.channel as i32)).collect();
    each.join(";")
}

fn float(s: &str) -> Option<f32> {
    s.trim().parse().ok()
}

/// A float formatted as Kotlin's `toString` ("1000.0", "1.41", "1.0E-5"), the stored format.
pub(crate) fn kotlin_float(v: f32) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let a = v.abs();
    if a == 0.0 || (1e-3..1e7).contains(&a) {
        let s = v.to_string();
        if s.contains('.') { s } else { format!("{s}.0") }
    } else {
        let s = format!("{v:e}");
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        if m.contains('.') { format!("{m}E{e}") } else { format!("{m}.0E{e}") }
    }
}

fn opt_bool(o: &Map<String, Value>, k: &str) -> bool {
    o.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn opt_f64(o: &Map<String, Value>, k: &str, fallback: f64) -> f64 {
    o.get(k).and_then(Value::as_f64).unwrap_or(fallback)
}

fn opt_i32(o: &Map<String, Value>, k: &str) -> i32 {
    o.get(k).and_then(Value::as_i64).map_or(0, |i| i as i32)
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn opt_string(o: &Map<String, Value>, k: &str) -> String {
    o.get(k).map(text).unwrap_or_default()
}

fn string_map(v: &Value) -> Option<HashMap<String, String>> {
    Some(v.as_object()?.iter().map(|(k, v)| (k.clone(), text(v))).collect())
}

/// Reads a sound profile's JSON; None when it is not one. Missing fields take their defaults.
pub fn sound_from(json: &str) -> Option<SoundSettings> {
    let v: Value = serde_json::from_str(json).ok()?;
    let o = v.as_object()?;
    // A present pre-amp must be a number.
    let eq_preamp_db = match o.get("eqPreampDb") {
        Some(v) => Some(v.as_f64()? as f32),
        None => None,
    };
    let d = SoundEffects::default();
    let f = |k: &str, d: f32, lo: f32, hi: f32| (opt_f64(o, k, d as f64) as f32).clamp(lo, hi);
    Some(SoundSettings {
        eq_enabled: opt_bool(o, "eqEnabled"),
        eq_bands: decode_bands(&opt_string(o, "eqBands")).unwrap_or_else(graphic),
        eq_mode: EqMode::nth(opt_i32(o, "eqMode")).unwrap_or(EqMode::Parametric),
        eq_graphic: decode_graphic(&opt_string(o, "eqGraphic")).unwrap_or_else(|| vec![0.0; 10]),
        eq_graphic_target: decode_target(&opt_string(o, "eqGraphicTarget")),
        eq_preamp_db,
        crossfeed_db: opt_f64(o, "crossfeedDb", 0.0) as f32,
        crossfeed_hz: EQ_RANGES.crossfeed_cut.hold(opt_f64(o, "crossfeedHz", 700.0) as f32),
        balance: opt_f64(o, "balance", 0.0) as f32,
        bypass: opt_bool(o, "bypass"),
        mono: opt_bool(o, "mono"),
        limiter: opt_bool(o, "limiter"),
        limiter_threshold_db: opt_f64(o, "limiterThresholdDb", -1.0) as f32,
        effects: SoundEffects {
            bass_boost_db: f("bassBoostDb", 0.0, 0.0, BASS_BOOST_MAX),
            virtualizer: f("virtualizer", 0.0, 0.0, 1.0),
            volume_boost_db: f("volumeBoostDb", 0.0, 0.0, VOLUME_BOOST_MAX),
            compressor: opt_bool(o, "compressor"),
            comp_threshold_db: f("compThresholdDb", d.comp_threshold_db, -60.0, 0.0),
            comp_ratio: f("compRatio", d.comp_ratio, 1.0, 20.0),
            comp_attack_ms: f("compAttackMs", d.comp_attack_ms, 0.1, 200.0),
            comp_release_ms: f("compReleaseMs", d.comp_release_ms, 10.0, 2000.0),
            comp_makeup_db: f("compMakeupDb", d.comp_makeup_db, 0.0, 24.0),
            comp_knee_db: f("compKneeDb", d.comp_knee_db, 0.0, 24.0),
            expander: opt_bool(o, "expander"),
            exp_threshold_db: f("expThresholdDb", d.exp_threshold_db, -90.0, -10.0),
            exp_ratio: f("expRatio", d.exp_ratio, 1.0, 20.0),
            exp_attack_ms: f("expAttackMs", d.exp_attack_ms, 0.1, 100.0),
            exp_release_ms: f("expReleaseMs", d.exp_release_ms, 10.0, 2000.0),
            loudness: opt_bool(o, "loudness"),
            loudness_ref_phon: o.get("loudnessRefPhon").and_then(Value::as_i64).map_or(d.loudness_ref_phon, |v| v.clamp(60, 90) as i32),
        },
        replay_gain: GainMode::ALL[opt_i32(o, "replayGain").clamp(0, GainMode::ALL.len() as i32 - 1) as usize],
        preamp_db: opt_f64(o, "preampDb", 0.0) as f32,
        crossfade_sec: opt_i32(o, "crossfadeSec"),
        hi_res: opt_bool(o, "hiRes"),
        max_rate: MaxRate::nth(opt_i32(o, "maxRate")).unwrap_or(MaxRate::Auto),
        bit_perfect: opt_bool(o, "bitPerfect"),
    })
}

pub fn sound_json(s: &SoundSettings) -> String {
    let mut o = Map::new();
    o.insert("eqEnabled".into(), s.eq_enabled.into());
    o.insert("eqBands".into(), encode_bands(&s.eq_bands).into());
    if let Some(p) = s.eq_preamp_db {
        o.insert("eqPreampDb".into(), (p as f64).into());
    }
    o.insert("crossfeedDb".into(), (s.crossfeed_db as f64).into());
    o.insert("crossfeedHz".into(), (s.crossfeed_hz as f64).into());
    o.insert("balance".into(), (s.balance as f64).into());
    if s.bypass {
        o.insert("bypass".into(), true.into());
    }
    o.insert("mono".into(), s.mono.into());
    o.insert("limiter".into(), s.limiter.into());
    o.insert("limiterThresholdDb".into(), (s.limiter_threshold_db as f64).into());
    o.insert("eqMode".into(), s.eq_mode.ordinal().into());
    o.insert("eqGraphic".into(), s.eq_graphic.iter().map(|v| kotlin_float(*v)).collect::<Vec<_>>().join(",").into());
    if !s.eq_graphic_target.is_empty() {
        o.insert("eqGraphicTarget".into(), encode_floats(&s.eq_graphic_target).into());
    }
    let e = &s.effects;
    for (k, v) in [
        ("bassBoostDb", e.bass_boost_db),
        ("virtualizer", e.virtualizer),
        ("volumeBoostDb", e.volume_boost_db),
        ("compThresholdDb", e.comp_threshold_db),
        ("compRatio", e.comp_ratio),
        ("compAttackMs", e.comp_attack_ms),
        ("compReleaseMs", e.comp_release_ms),
        ("compMakeupDb", e.comp_makeup_db),
        ("compKneeDb", e.comp_knee_db),
        ("expThresholdDb", e.exp_threshold_db),
        ("expRatio", e.exp_ratio),
        ("expAttackMs", e.exp_attack_ms),
        ("expReleaseMs", e.exp_release_ms),
    ] {
        o.insert(k.into(), (v as f64).into());
    }
    o.insert("compressor".into(), e.compressor.into());
    o.insert("expander".into(), e.expander.into());
    o.insert("loudness".into(), e.loudness.into());
    o.insert("loudnessRefPhon".into(), e.loudness_ref_phon.into());
    o.insert("replayGain".into(), s.replay_gain.ordinal().into());
    o.insert("preampDb".into(), (s.preamp_db as f64).into());
    o.insert("crossfadeSec".into(), s.crossfade_sec.into());
    o.insert("hiRes".into(), s.hi_res.into());
    o.insert("maxRate".into(), s.max_rate.ordinal().into());
    o.insert("bitPerfect".into(), s.bit_perfect.into());
    Value::Object(o).to_string()
}

/// One server from the stored list; None without an id.
fn server_from(v: &Value) -> Option<SavedServer> {
    let o = v.as_object()?;
    Some(SavedServer {
        id: text(o.get("id")?),
        name: opt_string(o, "name"),
        url: opt_string(o, "url"),
        alt_url: opt_string(o, "altUrl"),
        user: opt_string(o, "user"),
        password: opt_string(o, "password"),
        api_key: opt_string(o, "apiKey"),
        legacy_auth: opt_bool(o, "legacyAuth"),
        headers: o.get("headers").and_then(string_map).unwrap_or_default(),
        allow_self_signed: opt_bool(o, "allowSelfSigned"),
        client_cert: opt_string(o, "clientCert"),
        client_cert_password: opt_string(o, "clientCertPassword"),
        wifi_only: opt_bool(o, "wifiOnly"),
        music_folder_id: opt_string(o, "musicFolderId"),
        alt_max_bit_rate: opt_i32(o, "altMaxBitRate"),
    })
}

fn server_json(s: &SavedServer) -> Value {
    serde_json::json!({
        "id": s.id, "name": s.name, "url": s.url, "altUrl": s.alt_url, "user": s.user, "password": s.password,
        "apiKey": s.api_key, "legacyAuth": s.legacy_auth, "headers": s.headers, "allowSelfSigned": s.allow_self_signed,
        "clientCert": s.client_cert, "clientCertPassword": s.client_cert_password, "wifiOnly": s.wifi_only,
        "musicFolderId": s.music_folder_id, "altMaxBitRate": s.alt_max_bit_rate,
    })
}

/// The settings from what is stored: defaults for missing or mistyped values, ranges enforced.
pub fn load(raw: &HashMap<String, PrefValue>) -> StoredPrefs {
    let r = Raw(raw);
    let mut p = StoredPrefs::default();
    for row in ROWS {
        (row.load)(&mut p, &r);
    }
    if !raw.contains_key(EQ_MODE_KEY) && parametric_set_up(&p) {
        p.eq_mode = EqMode::Parametric;
    }
    p
}

/// The equalizer mode's key; absent in settings stored before the graphic equalizer existed.
pub(crate) const EQ_MODE_KEY: &str = "eqMode";

/// Whether settings stored before the graphic equalizer have the parametric one set up (on, a pre-amp,
/// or bands other than the flat defaults); such installs keep it.
pub(crate) fn parametric_set_up(p: &StoredPrefs) -> bool {
    p.eq_enabled || p.eq_preamp_db.is_some() || p.eq_bands != graphic()
}

/// Whether a saved sound profile's JSON has a parametric equalizer set up.
pub(crate) fn profile_parametric(json: &str) -> bool {
    sound_from(json).is_some_and(|s| s.eq_enabled && (s.eq_bands != graphic() || s.eq_preamp_db.is_some()))
}

/// Everything to write for these settings.
pub fn save(p: &StoredPrefs) -> HashMap<String, PrefValue> {
    let mut put = HashMap::with_capacity(96);
    for row in ROWS {
        (row.save)(p, &mut put);
    }
    put
}

/// The result of a change by name. `server`: the active server's profile changed (the platform
/// reconnects). `effect`: `settings_store`'s effect bits, set once the change is kept.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SettingChange {
    pub prefs: StoredPrefs,
    pub server: bool,
    pub effect: u32,
}

pub(crate) fn row(name: &str) -> Option<&'static Row> {
    ROWS.iter().find(|r| r.name == Some(name))
}

/// Changes one setting by name (settings rows and the test bridge's `tools/app.sh set`). A switch reads
/// "true"/"1" as on; an enum its name or ordinal; an unreadable number keeps the value. None for an
/// unknown name or a refused value, so script typos fail loudly.
pub fn set_by_name(p: &StoredPrefs, name: &str, value: &str) -> Option<SettingChange> {
    let mut n = p.clone();
    let mut server = false;
    // The stored key, kept as an alias.
    let name = if name == "lyricsLrclib" { "lyricsOnline" } else { name };
    match set_special(p, &mut n, &mut server, name, value) {
        Some(done) => done?,
        None => {
            let row = row(name)?;
            (row.set)(&mut n, value)?;
            // Switching on an online lookup switches the master lookups switch on too.
            if row.lookups && on(value) {
                n.third_party_lookups = true;
            }
        }
    }
    Some(SettingChange { prefs: n, server, effect: 0 })
}

/// Changes by name that are not a single table row: None for a table name, else whether the value was
/// taken.
fn set_special(p: &StoredPrefs, n: &mut StoredPrefs, server: &mut bool, name: &str, value: &str) -> Option<Option<()>> {
    let service = |v: &str| {
        let (service, at) = v.split_once(':')?;
        Some((LyricsService::named(service)?, at.trim().to_string()))
    };
    Some(match name {
        // Test bridge: "raw" or "<format>:<kbps>" (e.g. "opus:128").
        "wifiQuality" => match value.split_once(':') {
            Some((format, kbps)) => kbps.parse().ok().map(|bit_rate| n.wifi = SavedQuality { bit_rate, format: format.to_string() }),
            None if value == "raw" => Some(n.wifi = SavedQuality::default()),
            None => None,
        },
        // Also switches online lyrics, both ways.
        "thirdPartyLookups" => Some((n.third_party_lookups, n.lyrics_online) = (on(value), on(value))),
        // Test bridge: back to the defaults.
        "lyricsSources" if value.trim().eq_ignore_ascii_case("default") => {
            n.lyrics_order = lyrics_sources::default_order();
            Some(n.lyrics_on = lyrics_sources::default_order())
        }
        // Test bridge: only these services, in this order (`lrclib,unison`).
        "lyricsSources" => {
            let on = lyrics_sources::parse(value);
            let rest = p.lyrics_order.iter().filter(|s| !on.contains(s)).copied();
            n.lyrics_order = on.iter().copied().chain(rest).collect();
            Some(n.lyrics_on = on)
        }
        // One service moved to a rank (drag and drop): `NETEASE:3`.
        "lyricsPlace" => service(value).and_then(|(s, to)| Some(n.lyrics_order = lyrics_sources::placed(p, s, to.parse().ok()?))),
        // One service moved by places: `NETEASE:-1`.
        "lyricsMove" => service(value).and_then(|(s, by)| Some(n.lyrics_order = lyrics_sources::moved(p, s, by.parse().ok()?))),
        // `lyricsService:NETEASE`: one service on or off.
        _ if name.starts_with("lyricsService:") => LyricsService::named(&name["lyricsService:".len()..]).map(|s| {
            n.lyrics_on.retain(|x| *x != s);
            if on(value) {
                n.lyrics_on.push(s);
            }
        }),
        "compressorPreset" => preset_named(&nori_player::compressor::CompressorPreset::ALL, &COMPRESSOR_PRESETS, value).map(|c| {
            let e = p.effects().with_compressor_preset(c);
            (n.compressor, n.comp_threshold_db, n.comp_ratio, n.comp_attack_ms, n.comp_release_ms, n.comp_makeup_db, n.comp_knee_db) =
                (e.compressor, e.comp_threshold_db, e.comp_ratio, e.comp_attack_ms, e.comp_release_ms, e.comp_makeup_db, e.comp_knee_db);
        }),
        // A bs2b preset sets cutoff and level; OFF keeps the cutoff.
        "crossfeedPreset" if value.trim().eq_ignore_ascii_case("OFF") => Some(n.crossfeed_db = 0.0),
        "crossfeedPreset" => preset_named(&nori_player::dsp::CrossfeedPreset::ALL, &CROSSFEED_PRESETS[1..], value).map(|c| {
            let (cut, level) = c.settings();
            (n.crossfeed_hz, n.crossfeed_db) = (cut as f32, level as f32);
        }),
        // Graphic band count: the curve redrawn on the new layout, or refitted to the correction.
        "eqLayout" => value.trim().parse::<usize>().ok().filter(|c| nori_player::graphic::LAYOUTS.contains(c)).map(|c| match fit_target(&p.eq_graphic_target, c) {
            Some((sliders, preamp)) => (n.eq_graphic, n.eq_preamp_db) = (sliders, Some(preamp)),
            None => n.eq_graphic = relayout_graphic(&p.eq_graphic, c),
        }),
        // The inverse of `motionArtworkWifiOnly`.
        "motionArtworkMobile" => Some(n.motion_artwork_wifi_only = !on(value)),
        // The active server profile's own settings.
        "musicFolder" | "altMaxBitRate" => n.servers.iter_mut().find(|s| s.id == p.active_server_id).map(|s| {
            if name == "musicFolder" {
                s.music_folder_id = value.to_string();
            } else {
                s.alt_max_bit_rate = value.trim().parse::<i32>().map_or(s.alt_max_bit_rate, |v| v.max(0));
            }
            *server = true;
        }),
        _ => return None,
    })
}

/// Values of the [`SPECIAL_SPECS`] settings.
pub(crate) fn value_of_special(p: &StoredPrefs, name: &str) -> Option<String> {
    let server = || p.servers.iter().find(|s| s.id == p.active_server_id);
    Some(match name {
        "motionArtworkMobile" => (!p.motion_artwork_wifi_only).to_string(),
        "compressorPreset" => p.effects().compressor_preset().map_or("", |c| preset_name(&nori_player::compressor::CompressorPreset::ALL, &COMPRESSOR_PRESETS, c)).to_string(),
        "eqLayout" => p.eq_graphic.len().to_string(),
        // "" is a custom crossfeed.
        "crossfeedPreset" if p.crossfeed_db <= 0.0 => "OFF".to_string(),
        "crossfeedPreset" => crossfeed_preset(p.crossfeed_hz, p.crossfeed_db).map_or("", |c| preset_name(&nori_player::dsp::CrossfeedPreset::ALL, &CROSSFEED_PRESETS[1..], c)).to_string(),
        "musicFolder" => server().map(|s| s.music_folder_id.clone()).unwrap_or_default(),
        "altMaxBitRate" => server().map_or(0, |s| s.alt_max_bit_rate).to_string(),
        _ => return None,
    })
}

/// A server's display name: its name, or else its address's host.
pub fn label(name: &str, url: &str) -> String {
    if !name.trim().is_empty() {
        return name.to_string();
    }
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or_default().to_string()
}

fn with_bands(s: SoundSettings, eq_bands: Vec<SoundBand>) -> SoundSettings {
    SoundSettings { eq_bands, ..s }
}

fn band_of(b: &nori_model::EqBand) -> SoundBand {
    SoundBand { kind: b.kind, freq: b.freq, gain_db: b.gain_db, q: b.q, channel: BandChannel::Both }
}

/// `CompressorPreset::ALL`'s names, in its order.
const COMPRESSOR_PRESETS: [&str; 3] = ["GENTLE", "BALANCED", "STRONG"];
/// Off, then `CrossfeedPreset::ALL`'s names in its order.
const CROSSFEED_PRESETS: [&str; 4] = ["OFF", "DEFAULT", "CHU_MOY", "JAN_MEIER"];

/// `p`'s name, `names` being those of `all` in order.
fn preset_name<P: PartialEq>(all: &[P], names: &[&'static str], p: P) -> &'static str {
    names[all.iter().position(|x| *x == p).unwrap_or(0)]
}

/// The preset named `name` (any case), `names` being those of `all` in order.
fn preset_named<P: Copy>(all: &[P], names: &[&str], name: &str) -> Option<P> {
    names.iter().position(|n| n.eq_ignore_ascii_case(name.trim())).map(|i| all[i])
}

/// Which of bs2b's settings the crossfeed is on, if any: none with it off (0 dB) or moved by hand.
pub fn crossfeed_preset(cut_hz: f32, level_db: f32) -> Option<nori_player::dsp::CrossfeedPreset> {
    if level_db <= 0.0 {
        return None;
    }
    nori_player::dsp::CrossfeedPreset::of(cut_hz as f64, level_db as f64)
}

/// The graphic layout in use; 10 when the sliders are not a valid layout.
fn layout_of(s: &SoundSettings) -> usize {
    if nori_player::graphic::LAYOUTS.contains(&s.eq_graphic.len()) { s.eq_graphic.len() } else { 10 }
}

/// Graphic sliders for another layout that draw the same curve; unchanged for an invalid `count`.
pub(crate) fn relayout_graphic(sliders: &[f32], count: usize) -> Vec<f32> {
    if !nori_player::graphic::LAYOUTS.contains(&count) {
        return sliders.to_vec();
    }
    let g: Vec<f64> = sliders.iter().map(|v| *v as f64).collect();
    nori_player::graphic::relayout(&g, count).into_iter().map(|v| EQ_RANGES.gain.hold(((v * 10.0).round() / 10.0) as f32)).collect()
}

/// One graphic slider moved, held to the gain range; an index past the end changes nothing. Clears the
/// correction target.
pub fn set_graphic(s: SoundSettings, index: u32, gain_db: f32) -> SoundSettings {
    let mut g = s.eq_graphic.clone();
    match g.get_mut(index as usize) {
        Some(v) => *v = EQ_RANGES.gain.hold(gain_db),
        None => return s,
    }
    SoundSettings { eq_graphic: g, eq_graphic_target: Vec::new(), ..s }
}

/// Sliders of a `count`-band layout fitted to a correction `target`, and the pre-amp that prevents
/// boosting; None without a target.
fn fit_target(target: &[f32], count: usize) -> Option<(Vec<f32>, f32)> {
    let t: Vec<f64> = target.iter().map(|v| *v as f64).collect();
    let fit = nori_player::graphic::fit_target(&t, count, EQ_RANGES.gain.max as f64)?;
    Some((fit.sliders.iter().map(|v| *v as f32).collect(), EQ_RANGES.preamp.hold(fit.preamp_db as f32)))
}

/// A headphone correction (AutoEQ text: a `GraphicEQ:` curve or filters) as a graphic fitting target;
/// None when the text has neither.
pub(crate) fn correction_target(text: &str) -> Option<Vec<f32>> {
    let target = match nori_player::eqfit::parse_graphic(text) {
        Some(points) => nori_player::graphic::target_from_points(&points),
        None => {
            let preset = parse_eq_preset(text.to_string());
            if preset.bands.is_empty() {
                return None;
            }
            let bands: Vec<nori_player::dsp::Band> = preset.bands.iter().map(nori_player::dsp::Band::from).collect();
            nori_player::graphic::target_from_bands(&bands)
        }
    };
    Some(target.iter().map(|v| *v as f32).collect())
}

/// A built-in curve applied to the equalizer in use, which is switched on. A pre-amp of 0 means
/// automatic; "Flat" (no bands) restores the defaults. On the graphic equalizer the sliders take the
/// curve's response and the pre-amp is automatic.
pub fn apply_preset(s: SoundSettings, p: &NamedPreset) -> SoundSettings {
    if s.eq_mode == EqMode::Graphic {
        let bands: Vec<nori_player::dsp::Band> = p.bands.iter().map(nori_player::dsp::Band::from).collect();
        let count = layout_of(&s);
        let r = EQ_RANGES.gain;
        let eq_graphic = nori_player::graphic::sliders_for(&bands, count, r.max as f64).into_iter().map(|v| r.hold(v as f32)).collect();
        return SoundSettings { eq_enabled: true, eq_preamp_db: None, eq_graphic, eq_graphic_target: Vec::new(), ..s };
    }
    let bands: Vec<SoundBand> = p.bands.iter().map(band_of).collect();
    SoundSettings {
        eq_enabled: true,
        eq_preamp_db: (p.preamp_db != 0.0).then_some(p.preamp_db),
        eq_bands: if bands.is_empty() { graphic() } else { bands },
        ..s
    }
}

/// Imports an AutoEQ / Equalizer APO preset and switches the equalizer on. On the graphic equalizer the
/// sliders are fitted to it; otherwise its filters replace the parametric bands. No filters is
/// [`SoundError::NoFilters`].
pub fn import(s: SoundSettings, text: &str) -> Result<SoundSettings, SoundError> {
    if s.eq_mode == EqMode::Graphic {
        let target = correction_target(text).ok_or(SoundError::NoFilters)?;
        let count = layout_of(&s);
        let (eq_graphic, preamp) = fit_target(&target, count).ok_or(SoundError::NoFilters)?;
        return Ok(SoundSettings { eq_enabled: true, eq_graphic, eq_graphic_target: target, eq_preamp_db: Some(preamp), ..s });
    }
    let preset = parse_eq_preset(text.to_string());
    if preset.bands.is_empty() {
        return Err(SoundError::NoFilters);
    }
    Ok(SoundSettings { eq_enabled: true, eq_mode: EqMode::Parametric, eq_preamp_db: Some(preset.preamp_db), eq_bands: preset.bands.iter().map(band_of).collect(), ..s })
}

/// Appends a flat 1 kHz peak.
pub fn add_band(s: SoundSettings) -> SoundSettings {
    let mut bands = s.eq_bands.clone();
    bands.push(SoundBand { kind: EqKind::Peaking, freq: 1000.0, gain_db: 0.0, q: 1.0, channel: BandChannel::Both });
    with_bands(s, bands)
}

/// Removes a band; removing the last one restores the ten defaults.
pub fn remove_band(s: SoundSettings, index: u32) -> SoundSettings {
    let bands: Vec<SoundBand> = s.eq_bands.iter().enumerate().filter(|(i, _)| *i != index as usize).map(|(_, b)| *b).collect();
    with_bands(s, if bands.is_empty() { graphic() } else { bands })
}

/// A control's range; every edit through the core is held inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Span {
    pub min: f32,
    pub max: f32,
}

impl Span {
    pub(crate) fn hold(self, v: f32) -> f32 {
        if v.is_nan() { self.min.max(0.0).min(self.max) } else { v.clamp(self.min, self.max) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct EqRanges {
    /// A band's boost or cut, dB.
    pub gain: Span,
    /// The manual equalizer pre-amp, dB.
    pub preamp: Span,
    /// -1 hard left, +1 hard right.
    pub balance: Span,
    /// Limiter ceiling, dB.
    pub limiter: Span,
    /// Crossfeed, dB; 0 is off.
    pub crossfeed: Span,
    /// Crossfeed cutoff, Hz.
    pub crossfeed_cut: Span,
    /// A band's width (or a shelf's slope).
    pub q: Span,
    /// A band's frequency, Hz.
    pub freq: Span,
    /// ReplayGain pre-amp, dB.
    pub replay_gain_preamp: Span,
}

pub const EQ_RANGES: EqRanges = EqRanges {
    gain: Span { min: -12.0, max: 12.0 },
    preamp: Span { min: -20.0, max: 6.0 },
    balance: Span { min: -1.0, max: 1.0 },
    limiter: Span { min: -12.0, max: 0.0 },
    // Fits Jan Meier's 9.5 dB; bs2b's own 15 dB maximum is barely stereo.
    crossfeed: Span { min: 0.0, max: 12.0 },
    crossfeed_cut: Span { min: nori_player::dsp::CROSSFEED_CUT_HZ.0 as f32, max: nori_player::dsp::CROSSFEED_CUT_HZ.1 as f32 },
    q: Span { min: 0.2, max: 8.0 },
    freq: Span { min: 20.0, max: 20_000.0 },
    replay_gain_preamp: Span { min: REPLAY_GAIN_PREAMP.0, max: REPLAY_GAIN_PREAMP.1 },
};

/// Editor facts about one `EqKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BandKindInfo {
    pub uses_gain: bool,
    /// A shelf given by its slope rather than a Q.
    pub slope: bool,
}

/// The equalizer editor's static facts: band kinds in `EqKind` order, and the ranges.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct EqModel {
    pub band_kinds: Vec<BandKindInfo>,
    pub eq_ranges: EqRanges,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn eq_model_get() -> EqModel {
    EqModel {
        band_kinds: EqKind::ALL
            .iter()
            .map(|k| BandKindInfo { uses_gain: nori_player::dsp::uses_gain(*k as i32), slope: matches!(k, EqKind::LowShelfSlope | EqKind::HighShelfSlope) })
            .collect(),
        eq_ranges: EQ_RANGES,
    }
}

/// A band with gain, width and frequency held inside [`EQ_RANGES`].
fn held(b: SoundBand) -> SoundBand {
    let r = EQ_RANGES;
    SoundBand {
        kind: b.kind,
        freq: r.freq.hold(b.freq),
        gain_db: r.gain.hold(b.gain_db),
        q: r.q.hold(b.q),
        channel: b.channel,
    }
}

/// One band changed, held in range; an index past the end changes nothing.
pub fn set_band(s: SoundSettings, index: u32, band: SoundBand) -> SoundSettings {
    let mut bands = s.eq_bands.clone();
    match bands.get_mut(index as usize) {
        Some(b) => *b = held(band),
        None => return s,
    }
    with_bands(s, bands)
}

impl SoundSettings {
    pub fn effective_preamp_db(&self) -> f32 {
        mode_preamp_db(self.eq_enabled, self.eq_preamp_db, self.eq_mode, &self.eq_graphic, &self.eq_bands)
    }
}

/// The pre-amp in effect for the equalizer in use: the manual one, or else automatic; 0 when it is off.
pub(crate) fn mode_preamp_db(eq_enabled: bool, eq_preamp_db: Option<f32>, mode: EqMode, graphic: &[f32], bands: &[SoundBand]) -> f32 {
    match mode {
        EqMode::Graphic => effective_preamp_db(eq_enabled, eq_preamp_db, graphic.iter().map(|g| (nori_player::dsp::PEAKING, *g))),
        EqMode::Parametric => effective_preamp_db(eq_enabled, eq_preamp_db, bands.iter().map(|b| (b.kind as i32, b.gain_db))),
    }
}

/// The pre-amp in effect from the equalizer's parts: on or off, the manual pre-amp (None for
/// automatic) and each band's kind and gain.
pub fn effective_preamp_db(eq_enabled: bool, eq_preamp_db: Option<f32>, bands: impl IntoIterator<Item = (i32, f32)>) -> f32 {
    if !eq_enabled {
        return 0.0;
    }
    eq_preamp_db.unwrap_or_else(|| nori_player::dsp::auto_preamp_db(bands))
}

/// The automatic pre-amp on or off; switched off, the manual pre-amp starts at the current level.
pub fn set_auto_preamp(s: SoundSettings, automatic: bool) -> SoundSettings {
    let eq_preamp_db = if automatic { None } else { Some(EQ_RANGES.preamp.hold(s.effective_preamp_db())) };
    SoundSettings { eq_preamp_db, ..s }
}

/// A level slider on the sound screens (`settings_store::edit_level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum EqLevel {
    Preamp,
    Balance,
    Limiter,
    Crossfeed,
    /// ReplayGain's pre-amp (`preamp_db`), not the equalizer's.
    ReplayGainPreamp,
    BassBoost,
    Virtualizer,
    VolumeBoost,
    CompThreshold,
    CompRatio,
    CompAttack,
    CompRelease,
    CompMakeup,
    CompKnee,
    CrossfeedCut,
    ExpThreshold,
    ExpRatio,
    ExpAttack,
    ExpRelease,
}

impl EqLevel {
    /// Every level in declaration order; JNI passes the index.
    pub const ALL: [EqLevel; 19] = [
        EqLevel::Preamp,
        EqLevel::Balance,
        EqLevel::Limiter,
        EqLevel::Crossfeed,
        EqLevel::ReplayGainPreamp,
        EqLevel::BassBoost,
        EqLevel::Virtualizer,
        EqLevel::VolumeBoost,
        EqLevel::CompThreshold,
        EqLevel::CompRatio,
        EqLevel::CompAttack,
        EqLevel::CompRelease,
        EqLevel::CompMakeup,
        EqLevel::CompKnee,
        EqLevel::CrossfeedCut,
        EqLevel::ExpThreshold,
        EqLevel::ExpRatio,
        EqLevel::ExpAttack,
        EqLevel::ExpRelease,
    ];

    pub fn of(self, s: &SoundSettings) -> f32 {
        let e = &s.effects;
        match self {
            EqLevel::Preamp => s.effective_preamp_db(),
            EqLevel::Balance => s.balance,
            EqLevel::Limiter => s.limiter_threshold_db,
            EqLevel::Crossfeed => s.crossfeed_db,
            EqLevel::ReplayGainPreamp => s.preamp_db,
            EqLevel::BassBoost => e.bass_boost_db,
            EqLevel::Virtualizer => e.virtualizer,
            EqLevel::VolumeBoost => e.volume_boost_db,
            EqLevel::CompThreshold => e.comp_threshold_db,
            EqLevel::CompRatio => e.comp_ratio,
            EqLevel::CompAttack => e.comp_attack_ms,
            EqLevel::CompRelease => e.comp_release_ms,
            EqLevel::CompMakeup => e.comp_makeup_db,
            EqLevel::CompKnee => e.comp_knee_db,
            EqLevel::CrossfeedCut => s.crossfeed_hz,
            EqLevel::ExpThreshold => e.exp_threshold_db,
            EqLevel::ExpRatio => e.exp_ratio,
            EqLevel::ExpAttack => e.exp_attack_ms,
            EqLevel::ExpRelease => e.exp_release_ms,
        }
    }
}

/// 0 below `least` (an effect slider near its bottom is off).
fn off_below(v: f32, least: f32) -> f32 {
    if v < least { 0.0 } else { v }
}

/// Balance within 0.04 of the centre snaps to 0.
pub(crate) fn balance_snap(v: f32) -> f32 {
    if v.abs() < 0.04 { 0.0 } else { v }
}

/// Crossfeed under 1 dB snaps to off.
pub(crate) fn crossfeed_snap(db: f32) -> f32 {
    if db < 1.0 { 0.0 } else { db }
}

/// A level set, held in range and snapped ([`balance_snap`], [`crossfeed_snap`], [`off_below`]).
pub fn set_level(s: SoundSettings, level: EqLevel, value: f32) -> SoundSettings {
    let r = EQ_RANGES;
    match level {
        EqLevel::Preamp => SoundSettings { eq_preamp_db: Some(r.preamp.hold(value)), ..s },
        EqLevel::Balance => SoundSettings { balance: balance_snap(r.balance.hold(value)), ..s },
        EqLevel::Limiter => SoundSettings { limiter_threshold_db: r.limiter.hold(value), ..s },
        EqLevel::Crossfeed => SoundSettings { crossfeed_db: crossfeed_snap(r.crossfeed.hold(value)), ..s },
        EqLevel::ReplayGainPreamp => SoundSettings { preamp_db: r.replay_gain_preamp.hold(value), ..s },
        // Whole hertz, so presets are recognised exactly.
        EqLevel::CrossfeedCut => SoundSettings { crossfeed_hz: r.crossfeed_cut.hold(value).round(), ..s },
        _ => {
            let v = if value.is_nan() { 0.0 } else { value };
            let mut e = s.effects.clone();
            match level {
                EqLevel::BassBoost => e.bass_boost_db = off_below(v.clamp(0.0, BASS_BOOST_MAX), 0.25),
                EqLevel::Virtualizer => e.virtualizer = off_below(v.clamp(0.0, 1.0), 0.02),
                EqLevel::VolumeBoost => e.volume_boost_db = off_below(v.clamp(0.0, VOLUME_BOOST_MAX), 0.25),
                EqLevel::CompThreshold => e.comp_threshold_db = v.clamp(-60.0, 0.0),
                EqLevel::CompRatio => e.comp_ratio = v.clamp(1.0, 20.0),
                EqLevel::CompAttack => e.comp_attack_ms = v.clamp(0.1, 200.0),
                EqLevel::CompRelease => e.comp_release_ms = v.clamp(10.0, 2000.0),
                EqLevel::CompMakeup => e.comp_makeup_db = v.clamp(0.0, 24.0),
                EqLevel::CompKnee => e.comp_knee_db = v.clamp(0.0, 24.0),
                EqLevel::ExpThreshold => e.exp_threshold_db = v.clamp(-90.0, -10.0),
                EqLevel::ExpRatio => e.exp_ratio = v.clamp(1.0, 20.0),
                EqLevel::ExpAttack => e.exp_attack_ms = v.clamp(0.1, 100.0),
                EqLevel::ExpRelease => e.exp_release_ms = v.clamp(10.0, 2000.0),
                _ => {}
            }
            SoundSettings { effects: e, ..s }
        }
    }
}

/// Why the sound chain is out of the path, so the equalizer screen can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[repr(u8)]
pub enum EqBypass {
    /// Bit-perfect USB output.
    BitPerfect,
    /// This output is set to no processing (`soundBypass`).
    Output,
}
pub fn eq_bypass(bit_perfect: bool, output: bool) -> Option<EqBypass> {
    if bit_perfect {
        Some(EqBypass::BitPerfect)
    } else if output {
        Some(EqBypass::Output)
    } else {
        None
    }
}

/// The mark a band's label shows after its frequency ("1k L", "63 ↙").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BandMark {
    None,
    /// One channel only.
    Left,
    Right,
    LowShelf,
    HighShelf,
    /// A notch or pass filter.
    NoGain,
}

/// Channel first, then shelf, then no gain.
pub fn band_mark(kind: i32, channel: i32) -> BandMark {
    let low = kind == EqKind::LowShelf as i32 || kind == EqKind::LowShelfSlope as i32;
    let high = kind == EqKind::HighShelf as i32 || kind == EqKind::HighShelfSlope as i32;
    match channel {
        1 => BandMark::Left,
        2 => BandMark::Right,
        _ if low => BandMark::LowShelf,
        _ if high => BandMark::HighShelf,
        _ if !nori_player::dsp::uses_gain(kind) => BandMark::NoGain,
        _ => BandMark::None,
    }
}

/// The saved servers and which one is in use.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ServerList {
    pub servers: Vec<SavedServer>,
    pub active_server_id: String,
}

/// `profile` made active: it replaces the saved one with its id, or is appended.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn servers_activated(list: ServerList, profile: SavedServer) -> ServerList {
    let id = profile.id.clone();
    let mut servers: Vec<SavedServer> = list.servers.into_iter().filter(|s| s.id != id).collect();
    servers.push(profile);
    ServerList { servers, active_server_id: id }
}

/// The profile a login saves. A new profile for an already saved address and user takes over that
/// profile's id (and its library and downloads), music folder, bitrate cap and, if the form's is empty,
/// name. A profile already saved is returned as it is.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_for_login(list: ServerList, profile: SavedServer) -> SavedServer {
    if list.servers.iter().any(|s| s.id == profile.id) {
        return profile;
    }
    let address = |url: &str| url.trim().trim_end_matches('/').to_ascii_lowercase();
    let same = |s: &&SavedServer| address(&s.url) == address(&profile.url) && s.user.trim().eq_ignore_ascii_case(profile.user.trim());
    match list.servers.iter().find(same) {
        Some(saved) => SavedServer {
            id: saved.id.clone(),
            name: if profile.name.trim().is_empty() { saved.name.clone() } else { profile.name },
            music_folder_id: saved.music_folder_id.clone(),
            alt_max_bit_rate: saved.alt_max_bit_rate,
            ..profile
        },
        None => profile,
    }
}

/// A saved profile changed in place; an unknown one is not added.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn servers_updated(list: ServerList, profile: SavedServer) -> ServerList {
    let servers = list.servers.into_iter().map(|s| if s.id == profile.id { profile.clone() } else { s }).collect();
    ServerList { servers, ..list }
}

/// A profile removed. Removing the active one activates the first one left, or none.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn servers_removed(list: ServerList, id: String) -> ServerList {
    let was_active = list.active_server_id == id;
    let servers: Vec<SavedServer> = list.servers.into_iter().filter(|s| s.id != id).collect();
    let active_server_id = if was_active { servers.first().map(|s| s.id.clone()).unwrap_or_default() } else { list.active_server_id };
    ServerList { servers, active_server_id }
}

/// The server id rows in the app database are kept under: the active profile's, or "default".
pub fn server_db_id(active_server_id: &str) -> String {
    if active_server_id.is_empty() { "default".into() } else { active_server_id.to_string() }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_db(active_server_id: String) -> String {
    server_db_id(&active_server_id)
}

/// A fresh profile id: eight hex digits.
pub fn new_server_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    // A counter so two ids made in the same instant differ.
    static N: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(N.fetch_add(1, Ordering::Relaxed));
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    format!("{:08x}", h.finish() as u32)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_new_id() -> String {
    new_server_id()
}

/// Extra HTTP headers as typed, one "Name: value" per line. Lines without a colon or name are skipped;
/// a repeated name keeps its last value.
pub(crate) fn parse_headers(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            (!k.trim().is_empty()).then(|| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Headers as the form shows them, sorted by name.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_headers_text(headers: HashMap<String, String>) -> String {
    let mut h: Vec<(&String, &String)> = headers.iter().collect();
    h.sort();
    h.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n")
}

/// The schemes to offer for an address typed without one, https first.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_url_schemes(url: String) -> Vec<String> {
    if url.is_empty() || url.contains("://") { Vec::new() } else { vec!["https://".into(), "http://".into()] }
}

/// Whether the login form can be sent: an address, and a user or an API key.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_ready(profile: SavedServer) -> bool {
    !profile.url.trim().is_empty() && (!profile.user.trim().is_empty() || !profile.api_key.trim().is_empty())
}

/// The profile as the form sends it: addresses trimmed, headers parsed.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_from_form(profile: SavedServer, headers: String) -> SavedServer {
    SavedServer { url: profile.url.trim().to_string(), alt_url: profile.alt_url.trim().to_string(), headers: parse_headers(&headers), ..profile }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_label(name: String, url: String) -> String {
    label(&name, &url)
}

/// The crossfeed preset name these settings match ("OFF", "DEFAULT", "CHU_MOY", "JAN_MEIER"), or "" for
/// a custom one.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn crossfeed_preset_of(prefs: StoredPrefs) -> String {
    value_of_special(&prefs, "crossfeedPreset").unwrap_or_default()
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn eq_bypass_reason(bit_perfect: bool, output: bool) -> Option<EqBypass> {
    eq_bypass(bit_perfect, output)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn prefs_sound(prefs: StoredPrefs) -> SoundSettings {
    prefs.sound()
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn prefs_with_sound(prefs: StoredPrefs, sound: SoundSettings) -> StoredPrefs {
    prefs.with_sound(sound)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sound_from_json(json: String) -> Option<SoundSettings> {
    sound_from(&json)
}

/// The equalizer in use reset to flat with the automatic pre-amp: the ten default bands (parametric)
/// or every slider at 0 in the same layout (graphic).
pub(crate) fn eq_reset_bands(sound: SoundSettings) -> SoundSettings {
    match sound.eq_mode {
        EqMode::Graphic => SoundSettings { eq_graphic: vec![0.0; sound.eq_graphic.len().max(1)], eq_graphic_target: Vec::new(), eq_preamp_db: None, ..sound },
        EqMode::Parametric => SoundSettings { eq_bands: graphic(), eq_preamp_db: None, ..sound },
    }
}

/// Indices into `names` of the app database's files (`nori.db` and its WAL and shared memory).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn storage_index_files(names: Vec<String>) -> Vec<u32> {
    names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.strip_prefix(nori_db::DB_FILE).is_some_and(|rest| ["", "-wal", "-shm"].contains(&rest)))
        .map(|(i, _)| i as u32)
        .collect()
}

/// Reads an AutoEQ "ParametricEQ.txt" / Equalizer APO preset (`Preamp: -6.2 dB`,
/// `Filter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70`); other lines are ignored. A file with no filters but
/// a `GraphicEQ:` curve is fitted with parametric filters (`nori_player::eqfit`).
pub fn parse_eq_preset(text: String) -> EqPreset {
    let mut preset = EqPreset::default();
    for line in text.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        let after = |key: &str| t.iter().position(|w| w.eq_ignore_ascii_case(key)).and_then(|i| t.get(i + 1)).and_then(|v| v.parse::<f32>().ok());
        if t.first().is_some_and(|w| w.eq_ignore_ascii_case("preamp:")) {
            preset.preamp_db = t.get(1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
        } else if t.first().is_some_and(|w| w.eq_ignore_ascii_case("filter")) {
            let Some(on) = t.iter().position(|w| w.eq_ignore_ascii_case("ON")) else { continue };
            let token = t.get(on + 1).map(|k| k.to_ascii_uppercase()).unwrap_or_default();
            let kind = match token.as_str() {
                "PK" | "PEQ" | "MODAL" => EqKind::Peaking,
                "LS" | "LSC" | "LSQ" => EqKind::LowShelf,
                "HS" | "HSC" | "HSQ" => EqKind::HighShelf,
                "LS6" => EqKind::LowShelfSlope,
                "HS6" => EqKind::HighShelfSlope,
                "LP" | "LPQ" => EqKind::LowPass,
                "HP" | "HPQ" => EqKind::HighPass,
                "BP" => EqKind::BandPass,
                "NO" | "NOTCH" => EqKind::Notch,
                "AP" => EqKind::AllPass,
                _ => continue,
            };
            let Some(freq) = after("Fc") else { continue };
            let gain_db = after("Gain").unwrap_or(0.0);
            // Shelves and peaks need a gain; pass filters have none.
            if matches!(kind, EqKind::Peaking | EqKind::LowShelf | EqKind::HighShelf | EqKind::LowShelfSlope | EqKind::HighShelfSlope) && after("Gain").is_none() {
                continue;
            }
            preset.bands.push(EqBand { kind, freq, gain_db, q: after("Q").unwrap_or(0.71) });
        }
    }
    if preset.bands.is_empty() {
        if let Some(points) = nori_player::eqfit::parse_graphic(&text) {
            let fit = nori_player::eqfit::fit_graphic(&points);
            return EqPreset { preamp_db: fit.preamp_db, bands: fit.bands };
        }
    }
    preset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(entries: &[(&str, PrefValue)]) -> HashMap<String, PrefValue> {
        entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }
    fn t(v: &str) -> PrefValue {
        PrefValue::Text { v: v.to_string() }
    }
    fn n(v: i32) -> PrefValue {
        PrefValue::Number { v }
    }

    #[test]
    fn keys_and_names_unique() {
        let saved = save(&StoredPrefs { eq_preamp_db: Some(0.0), ..StoredPrefs::default() });
        let mut keys: Vec<&str> = ROWS.iter().map(|r| r.key).collect();
        let mut names: Vec<&str> = ROWS.iter().filter_map(|r| r.name).chain(SPECIAL_SPECS.iter().map(|(n, _)| *n)).collect();
        for k in &keys {
            assert!(saved.contains_key(*k) || saved.contains_key(&format!("{k}BitRate")), "{k} is not stored under its key");
        }
        keys.sort_unstable();
        names.sort_unstable();
        let (k, n) = (keys.len(), names.len());
        keys.dedup();
        names.dedup();
        assert_eq!((keys.len(), names.len()), (k, n), "each key and name once");
    }

    #[test]
    fn load_empty_is_default() {
        assert_eq!(load(&HashMap::new()), StoredPrefs::default());
    }

    #[test]
    fn load_clamps_out_of_range() {
        let p = load(&raw(&[("parallelDownloads", n(40)), ("coversAhead", n(-1)), ("replayGain", n(9)), ("theme", n(7)), ("tapAction", n(-2)), ("swipeLeft", n(5))]));
        assert_eq!(p.parallel_downloads, 10);
        assert_eq!(p.covers_ahead, 0);
        assert_eq!(p.replay_gain, GainMode::Auto, "ReplayGain takes the nearest");
        assert_eq!(p.theme, ThemeMode::System);
        assert_eq!(p.tap_action, TapAction::PlayList);
        assert_eq!(p.swipe_left, SwipeAction::Favourite);
        assert_eq!(load(&raw(&[("parallelDownloads", n(0))])).parallel_downloads, 1);
        // A value of the wrong type is as good as missing.
        assert_eq!(load(&raw(&[("cacheMb", t("big"))])).cache_mb, 1024);
    }

    #[test]
    fn server_list_loads_whole_or_empty() {
        let p = load(&raw(&[("servers", t(r#"[{"id":"a","legacyAuth":true,"altMaxBitRate":128}]"#)), ("activeServerId", t("a"))]));
        assert_eq!((p.servers.len(), p.active_server_id.as_str()), (1, "a"));
        assert!(p.servers[0].legacy_auth);
        assert_eq!(p.servers[0].alt_max_bit_rate, 128);
        // A list that does not read, or a server without an id, loses the whole list.
        assert!(load(&raw(&[("servers", t(r#"[{"id":"a"},{"name":"x"}]"#))])).servers.is_empty());
        assert!(load(&raw(&[("servers", t("nope"))])).servers.is_empty());
    }

    #[test]
    fn load_home_rows_pins_list_prefs() {
        let p = load(&raw(&[("homeRows", t("RANDOM,NOPE,PINNED")), ("pinnedPlaylists", t("a\n\nb")), ("listPrefs", t(r#"{"x":"1","y":2}"#))]));
        assert_eq!(p.home_rows, [HomeRow::Random, HomeRow::Pinned]);
        assert_eq!(p.pinned_playlists, ["a", "b"]);
        assert_eq!(p.list_prefs.get("y").map(String::as_str), Some("2"));
        assert!(load(&raw(&[("homeRows", t(""))])).home_rows.is_empty(), "every row hidden");
        assert!(load(&raw(&[("listPrefs", t("{"))])).list_prefs.is_empty());
    }

    #[test]
    fn band_wire_format() {
        let b = [band_from(0, 1000.0, -2.5, 1.41, 0), band_from(9, 31.0, 0.0, 0.00001, 2)];
        assert_eq!(encode_bands(&b), "0:1000.0:-2.5:1.41:0;9:31.0:0.0:1.0E-5:2");
        assert_eq!(decode_bands(&encode_bands(&b)).unwrap(), b);
        // Four fields is a band on both channels; anything that does not read is dropped.
        assert_eq!(decode_bands("1:100:3:0.7").unwrap(), [band_from(1, 100.0, 3.0, 0.7, 0)]);
        assert_eq!(decode_bands("1:100:3:0.7;12:1:1:1;x:1:1:1;1:1:1;2:1:1:1:3").unwrap().len(), 1);
        assert_eq!(decode_bands(""), None);
        assert_eq!(decode_bands("1:1:1"), None);
        assert_eq!(kotlin_float(12_345_678.0), "1.2345678E7");
        assert_eq!(kotlin_float(-0.5), "-0.5");
    }

    #[test]
    fn sound_json_round_trip() {
        let s = SoundSettings {
            eq_enabled: true,
            eq_bands: vec![band_from(1, 105.0, -3.5, 0.7, 0)],
            eq_mode: EqMode::Graphic,
            eq_graphic: (0..15).map(|i| i as f32 - 7.5).collect(),
            eq_graphic_target: (0..96).map(|i| (i as f32 * 0.37).sin() * 4.0).collect(),
            eq_preamp_db: Some(-6.2),
            crossfeed_db: 3.0,
            crossfeed_hz: 820.0,
            bypass: true,
            balance: -0.25,
            mono: true,
            limiter: true,
            limiter_threshold_db: -2.0,
            effects: SoundEffects {
                bass_boost_db: 4.5,
                virtualizer: 0.6,
                volume_boost_db: 3.0,
                compressor: true,
                comp_threshold_db: -24.0,
                comp_ratio: 4.0,
                comp_attack_ms: 5.0,
                comp_release_ms: 300.0,
                comp_makeup_db: 6.0,
                comp_knee_db: 3.0,
                expander: true,
                exp_threshold_db: -60.0,
                exp_ratio: 8.0,
                exp_attack_ms: 1.5,
                exp_release_ms: 250.0,
                loudness: true,
                loudness_ref_phon: 75,
            },
            replay_gain: GainMode::Album,
            preamp_db: 1.5,
            crossfade_sec: 4,
            hi_res: true,
            max_rate: MaxRate::Khz96,
            bit_perfect: false,
        };
        assert_eq!(sound_from(&sound_json(&s)).unwrap(), s);
        assert_eq!(sound_from(&sound_json(&SoundSettings { eq_preamp_db: None, ..s.clone() })).unwrap().eq_preamp_db, None);
    }

    #[test]
    fn sound_json_defaults() {
        let s = sound_from("{}").unwrap();
        assert_eq!(s.eq_bands, graphic());
        assert_eq!(s.limiter_threshold_db, -1.0);
        assert_eq!(s.eq_preamp_db, None);
        assert_eq!(sound_from(r#"{"replayGain":7}"#).unwrap().replay_gain, GainMode::Auto);
        assert_eq!(sound_from(r#"{"replayGain":-1}"#).unwrap().replay_gain, GainMode::Off);
        assert_eq!(s.max_rate, MaxRate::Auto, "a profile saved before the maximum rate: the device's own");
        assert_eq!(sound_from(r#"{"maxRate":9}"#).unwrap().max_rate, MaxRate::Auto);
        assert_eq!(sound_from(r#"{"eqPreampDb":"x"}"#), None, "a pre-amp that is not a number");
        assert_eq!(sound_from(r#"{"eqPreampDb":null}"#), None);
        assert_eq!(sound_from("not json"), None);
        assert_eq!(sound_from("[]"), None);
    }

    #[test]
    fn server_labels() {
        assert_eq!(label("Home", "https://x"), "Home");
        assert_eq!(label(" ", "https://music.example.com:4533/navidrome"), "music.example.com:4533");
        assert_eq!(label("", "music.example.com/x"), "music.example.com");
        assert_eq!(label("", ""), "");
    }

    #[test]
    fn set_by_name_holds_ranges() {
        let p = StoredPrefs::default();
        assert!(set_by_name(&p, "limiter", "TRUE").unwrap().prefs.limiter);
        assert_eq!(set_by_name(&p, "eqPreampDb", "12").unwrap().prefs.eq_preamp_db, Some(6.0), "held to the equalizer's range");
        assert_eq!(set_by_name(&StoredPrefs { eq_preamp_db: Some(2.0), ..p.clone() }, "eqPreampDb", "auto").unwrap().prefs.eq_preamp_db, None);
        assert!(set_by_name(&p, "mono", "1").unwrap().prefs.mono);
        assert!(!set_by_name(&StoredPrefs { mono: true, ..p.clone() }, "mono", "yes").unwrap().prefs.mono);
        assert_eq!(set_by_name(&p, "parallelDownloads", "99").unwrap().prefs.parallel_downloads, 10);
        assert_eq!(set_by_name(&p, "coversAhead", "x").unwrap().prefs.covers_ahead, 3, "unreadable keeps the value");
        for not_a_number in ["NaN", "inf", "-infinity"] {
            let held = set_by_name(&p, "compThresholdDb", not_a_number).unwrap().prefs;
            assert_eq!(held.comp_threshold_db, p.comp_threshold_db, "{not_a_number}");
            assert_eq!(set_by_name(&p, "crossfeedDb", not_a_number).unwrap().prefs.crossfeed_db, p.crossfeed_db, "{not_a_number}");
        }
        assert_eq!(set_by_name(&p, "cacheMb", "10").unwrap().prefs.cache_mb, 256);
        assert_eq!(set_by_name(&p, "speed", "9").unwrap().prefs.speed, 4.0);
        assert_eq!(set_by_name(&p, "fadeMs", "-5").unwrap().prefs.fade_ms, 0);
        assert_eq!(set_by_name(&p, "autoFillKind", "albums").unwrap().prefs.auto_fill_kind, AutoFillKind::Albums);
        assert_eq!(set_by_name(&p, "autoFillBasis", "era").unwrap().prefs.auto_fill_basis, AutoFillBasis::Era);
        assert_eq!(set_by_name(&p, "autoFillBasis", "mood"), None);
        assert_eq!(set_by_name(&p, "nope", "1"), None);
    }

    #[test]
    fn set_by_name_parses_each_kind() {
        let p = StoredPrefs::default();
        let set = |name: &str, v: &str| set_by_name(&p, name, v).unwrap().prefs;
        assert_eq!(set("replayGain", "ALBUM").replay_gain, GainMode::Album);
        assert_eq!(set("replayGain", "3").replay_gain, GainMode::Auto);
        assert_eq!(set_by_name(&p, "replayGain", "9"), None, "an ordinal out of range is not a value");
        assert_eq!(set("theme", "dark").theme, ThemeMode::Dark);
        assert_eq!(set("tapAction", "PLAY_NEXT").tap_action, TapAction::PlayNext);
        assert_eq!(set("swipeLeft", "DOWNLOAD").swipe_left, SwipeAction::Download);
        assert_eq!(set("wifi", "320:mp3").wifi, SavedQuality { bit_rate: 320, format: "mp3".into() });
        assert_eq!(set("mobile", "0:").mobile, SavedQuality::default());
        assert_eq!(set_by_name(&p, "download", "flac"), None);
        assert_eq!(set("preampDb", "20").preamp_db, 6.0);
        assert_eq!(set("accent", "4280191205").accent, 0xFF1E88E5);
        assert_eq!(set("speed", "0.75").speed, 0.75);
        assert_eq!(set("lyricsSize", "7").lyrics_size, 2);
        assert_eq!(set("autoMixMaxTempoPct", "2.0").auto_mix_max_tempo_pct, 2.0);
    }

    #[test]
    fn autoeq_switches_lookups_on() {
        // Stored off stays off.
        let kept = load(&save(&StoredPrefs { third_party_lookups: false, auto_eq_download: false, ..StoredPrefs::default() }));
        assert!(!kept.third_party_lookups && !kept.auto_eq_download);
        let p = StoredPrefs { third_party_lookups: false, auto_eq_download: false, ..StoredPrefs::default() };
        let on = set_by_name(&p, "autoEqDownload", "true").unwrap().prefs;
        assert!(on.auto_eq_download && on.third_party_lookups, "the AutoEQ list switches lookups on");
        assert!(!set_by_name(&on, "autoEqDownload", "false").unwrap().prefs.auto_eq_download);
    }

    #[test]
    fn lyrics_online_and_lookups_switch() {
        let p = StoredPrefs::default();
        let on = set_by_name(&p, "lyricsOnline", "true").unwrap().prefs;
        assert!(on.lyrics_online && on.third_party_lookups, "lyrics online switches lookups on");
        let off = set_by_name(&on, "lyricsLrclib", "false").unwrap().prefs;
        assert!(!off.lyrics_online && off.third_party_lookups, "and off leaves the lookups alone");
        let all_off = set_by_name(&on, "thirdPartyLookups", "false").unwrap().prefs;
        assert!(!all_off.lyrics_online && !all_off.third_party_lookups);
        let all_on = set_by_name(&all_off, "thirdPartyLookups", "true").unwrap().prefs;
        assert!(all_on.lyrics_online && all_on.third_party_lookups);
    }

    #[test]
    fn lyrics_services_by_name() {
        let p = StoredPrefs::default();
        let all = p.lyrics_on.len();
        let off = set_by_name(&p, "lyricsService:portato", "false").unwrap().prefs;
        assert!(!off.lyrics_on.contains(&LyricsService::Portato) && off.lyrics_on.len() == all - 1);
        let on = set_by_name(&off, "lyricsService:portato", "true").unwrap().prefs;
        assert!(on.lyrics_on.contains(&LyricsService::Portato) && on.lyrics_on.len() == all);
        assert_eq!(on.lyrics_order, p.lyrics_order, "a switch never moves a service");
        let off = set_by_name(&on, "lyricsService:paxsenix", "false").unwrap().prefs;
        assert_eq!((off.lyrics_order.clone(), off.lyrics_on.len()), (p.lyrics_order.clone(), all - 1));
        assert!(set_by_name(&p, "lyricsService:nobody", "true").is_none(), "no such service");
        let at = |o: &[LyricsService]| o.iter().position(|x| *x == LyricsService::Lrclib).unwrap();
        let moved = set_by_name(&on, "lyricsMove", "LRCLIB:-1").unwrap().prefs;
        assert_eq!(at(&moved.lyrics_order), at(&on.lyrics_order) - 1, "one place, whoever is above it");
        let placed = set_by_name(&on, "lyricsPlace", "LRCLIB:0").unwrap().prefs;
        assert_eq!(crate::lyrics_sources::switched_on(&placed)[0].name(), "LRCLIB", "dropped first, asked first");
        assert_eq!(placed.lyrics_on, on.lyrics_on);
        assert!(set_by_name(&on, "lyricsPlace", "LRCLIB:x").is_none());
        let only = set_by_name(&p, "lyricsSources", "lrclib, kugou").unwrap().prefs;
        assert_eq!(only.lyrics_on, [LyricsService::Lrclib, LyricsService::Kugou]);
        assert_eq!(only.lyrics_order[..2], [LyricsService::Lrclib, LyricsService::Kugou]);
        let back = set_by_name(&only, "lyricsSources", "default").unwrap().prefs;
        assert_eq!((back.lyrics_on, back.lyrics_order), (p.lyrics_on.clone(), p.lyrics_order.clone()));
        let keyed = set_by_name(&placed, "paxSenixKey", "  k  ").unwrap().prefs;
        let back = load(&save(&keyed));
        assert_eq!((back.lyrics_on, back.lyrics_order, back.paxsenix_key), (keyed.lyrics_on.clone(), keyed.lyrics_order.clone(), "k".to_string()));
        assert_eq!(load(&HashMap::new()).lyrics_on, crate::lyrics_sources::default_order(), "nothing stored: the defaults, every service");
        assert_eq!(load(&HashMap::new()).lyrics_order, crate::lyrics_sources::default_order());
    }

    #[test]
    fn active_server_settings_by_name() {
        let a = SavedServer { id: "a".into(), ..SavedServer::default() };
        let b = SavedServer { id: "b".into(), ..SavedServer::default() };
        let p = StoredPrefs { servers: vec![a, b], active_server_id: "b".into(), ..StoredPrefs::default() };
        let c = set_by_name(&p, "musicFolder", "7").unwrap();
        assert!(c.server);
        assert_eq!((c.prefs.servers[0].music_folder_id.as_str(), c.prefs.servers[1].music_folder_id.as_str()), ("", "7"));
        assert_eq!(set_by_name(&p, "altMaxBitRate", "128").unwrap().prefs.servers[1].alt_max_bit_rate, 128);
        assert_eq!(set_by_name(&StoredPrefs::default(), "musicFolder", "7"), None, "no server in use");
        assert!(!set_by_name(&p, "mono", "1").unwrap().server);
    }

    #[test]
    fn set_band_holds_range() {
        let s = sound();
        let b = set_band(s.clone(), 3, band_from(42, 5.0, 30.0, 0.0, 7));
        assert_eq!(b.eq_bands[3], band_from(0, 20.0, 12.0, 0.2, 0));
        let ok = band_from(1, 120.0, -3.5, 0.7, 2);
        assert_eq!(set_band(s.clone(), 0, ok).eq_bands[0], ok);
        assert_eq!(set_band(s.clone(), 99, ok), s, "no such band");
    }

    #[test]
    fn auto_preamp_off_keeps_level() {
        let mut s = sound();
        s.eq_enabled = true;
        s.eq_bands[2].gain_db = 4.5;
        assert_eq!(s.effective_preamp_db(), -4.5);
        let manual = set_auto_preamp(s.clone(), false);
        assert_eq!(manual.eq_preamp_db, Some(-4.5));
        assert_eq!(set_auto_preamp(manual, true).eq_preamp_db, None);
        assert_eq!(SoundSettings { eq_enabled: false, ..s.clone() }.effective_preamp_db(), 0.0);
        assert_eq!(set_auto_preamp(SoundSettings { eq_enabled: false, ..s }, false).eq_preamp_db, Some(0.0));
    }

    #[test]
    fn set_level_snaps_and_holds() {
        let s = sound();
        assert_eq!(set_level(s.clone(), EqLevel::Balance, 0.03).balance, 0.0);
        assert_eq!(set_level(s.clone(), EqLevel::Balance, -3.0).balance, -1.0);
        assert_eq!(set_level(s.clone(), EqLevel::Crossfeed, 0.5).crossfeed_db, 0.0);
        assert_eq!(set_level(s.clone(), EqLevel::Crossfeed, 20.0).crossfeed_db, 12.0);
        assert_eq!(set_level(s.clone(), EqLevel::CrossfeedCut, 100.0).crossfeed_hz, 300.0);
        assert_eq!(set_level(s.clone(), EqLevel::CrossfeedCut, 912.4).crossfeed_hz, 912.0);
        assert_eq!(set_level(s.clone(), EqLevel::Limiter, 2.0).limiter_threshold_db, 0.0);
        assert_eq!(set_level(s, EqLevel::Preamp, -30.0).eq_preamp_db, Some(-20.0));
    }

    #[test]
    fn eq_bypass_precedence() {
        assert_eq!(eq_bypass(false, false), None);
        assert_eq!(eq_bypass(true, true), Some(EqBypass::BitPerfect));
        assert_eq!(eq_bypass(false, true), Some(EqBypass::Output));
    }

    #[test]
    fn band_marks() {
        assert_eq!(band_mark(0, 0), BandMark::None);
        assert_eq!(band_mark(1, 1), BandMark::Left);
        assert_eq!(band_mark(8, 0), BandMark::LowShelf);
        assert_eq!(band_mark(2, 0), BandMark::HighShelf);
        assert_eq!(band_mark(9, 2), BandMark::Right);
        assert_eq!(band_mark(6, 0), BandMark::NoGain);
    }

    #[test]
    fn server_list_edits() {
        let s = |id: &str, name: &str| SavedServer { id: id.into(), name: name.into(), ..SavedServer::default() };
        let list = ServerList { servers: vec![s("a", "A"), s("b", "B")], active_server_id: "b".into() };
        let l = servers_activated(list.clone(), s("a", "A2"));
        assert_eq!(l.servers.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["B", "A2"], "replaced, and moved to the end");
        assert_eq!(l.active_server_id, "a");
        let l = servers_updated(list.clone(), s("a", "A3"));
        assert_eq!((l.servers[0].name.as_str(), l.active_server_id.as_str()), ("A3", "b"));
        assert_eq!(servers_updated(list.clone(), s("z", "Z")).servers.len(), 2, "an unknown profile is not added");
        let l = servers_removed(list.clone(), "b".into());
        assert_eq!((l.servers.len(), l.active_server_id.as_str()), (1, "a"), "the first one left takes over");
        assert_eq!(servers_removed(list.clone(), "a".into()).active_server_id, "b");
        assert_eq!(servers_removed(ServerList { servers: vec![s("a", "")], active_server_id: "a".into() }, "a".into()).active_server_id, "");
        assert_eq!((server_db_id(""), server_db_id("x1")), ("default".into(), "x1".into()));
        let id = new_server_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_server_id(), new_server_id());
    }

    #[test]
    fn login_reuses_saved_profile() {
        let saved = SavedServer { id: "a1".into(), name: "Home".into(), url: "http://10.0.2.2:4534/".into(), user: "admin".into(), password: "old".into(), music_folder_id: "3".into(), ..SavedServer::default() };
        let other = SavedServer { id: "b2".into(), url: "http://10.0.2.2:4534".into(), user: "guest".into(), ..SavedServer::default() };
        let list = ServerList { servers: vec![saved.clone(), other], active_server_id: "b2".into() };
        let form = SavedServer { id: new_server_id(), url: " HTTP://10.0.2.2:4534 ".into(), user: "Admin".into(), password: "new".into(), ..SavedServer::default() };
        let kept = server_for_login(list.clone(), form.clone());
        assert_eq!((kept.id.as_str(), kept.password.as_str(), kept.name.as_str(), kept.music_folder_id.as_str()), ("a1", "new", "Home", "3"), "{kept:?}");
        let l = servers_activated(list.clone(), kept);
        assert_eq!((l.servers.len(), l.active_server_id.as_str()), (2, "a1"), "activated, not copied");
        // Another user of the same server, or another server, is a profile of its own.
        let stranger = SavedServer { user: "someone".into(), ..form.clone() };
        assert_eq!(server_for_login(list.clone(), stranger.clone()), stranger);
        let elsewhere = SavedServer { url: "https://music.example".into(), ..form.clone() };
        assert_eq!(server_for_login(list.clone(), elsewhere.clone()), elsewhere);
        // A saved profile edited keeps its own id, whatever the others are.
        let edited = SavedServer { id: "b2".into(), ..form };
        assert_eq!(server_for_login(list.clone(), edited.clone()), edited);
    }

    #[test]
    fn login_form() {
        let h = parse_headers("X-Auth: a:b\n: nope\nno colon\n  CF-Id :  x  \nX-Auth: c");
        assert_eq!(h.len(), 2);
        assert_eq!(h["X-Auth"], "c", "the last one wins");
        assert_eq!(h["CF-Id"], "x");
        assert_eq!(server_headers_text(parse_headers("b: 2\na: 1")), "a: 1\nb: 2");
        assert_eq!(server_url_schemes("music.local".into()), ["https://", "http://"]);
        assert!(server_url_schemes("".into()).is_empty() && server_url_schemes("http://x".into()).is_empty());
        let p = SavedServer { url: " https://x ".into(), alt_url: " y ".into(), ..SavedServer::default() };
        assert!(!server_ready(p.clone()), "a user or a key");
        assert!(server_ready(SavedServer { user: "u".into(), ..p.clone() }));
        assert!(server_ready(SavedServer { api_key: "k".into(), ..p.clone() }));
        assert!(!server_ready(SavedServer { url: "  ".into(), user: "u".into(), ..p.clone() }));
        let f = server_from_form(p, "A: 1".into());
        assert_eq!((f.url.as_str(), f.alt_url.as_str(), f.headers["A"].as_str()), ("https://x", "y", "1"));
    }

    fn sound() -> SoundSettings {
        sound_from("{}").unwrap()
    }

    #[test]
    fn parametric_edits() {
        let flat = NamedPreset { kind: nori_model::PresetKind::Flat, preamp_db: 0.0, bands: vec![] };
        let s = apply_preset(SoundSettings { eq_preamp_db: Some(-4.0), ..sound() }, &flat);
        assert!(s.eq_enabled);
        assert_eq!(s.eq_preamp_db, None, "a pre-amp of 0 is automatic");
        assert_eq!(s.eq_bands, graphic());
        let bass = NamedPreset { kind: nori_model::PresetKind::BassBoost, preamp_db: -6.0, bands: vec![nori_model::EqBand { kind: EqKind::LowShelf, freq: 100.0, gain_db: 6.0, q: 0.7 }] };
        let s = apply_preset(sound(), &bass);
        assert_eq!(s.eq_preamp_db, Some(-6.0));
        assert_eq!(s.eq_bands, [band_from(1, 100.0, 6.0, 0.7, 0)]);

        let added = add_band(sound());
        assert_eq!(added.eq_bands.len(), 11);
        assert_eq!(added.eq_bands[10], band_from(0, 1000.0, 0.0, 1.0, 0));
        assert_eq!(remove_band(added.clone(), 10).eq_bands, graphic());
        assert_eq!(remove_band(added, 99).eq_bands.len(), 11);
        let one = SoundSettings { eq_bands: vec![band_from(2, 5.0, 1.0, 1.0, 0)], ..sound() };
        assert_eq!(remove_band(one, 0).eq_bands, graphic(), "never an empty equalizer");
    }

    #[test]
    fn graphic_edits() {
        let g = SoundSettings { eq_mode: EqMode::Graphic, ..sound() };
        let moved = set_graphic(g.clone(), 3, 20.0);
        assert_eq!(moved.eq_graphic[3], 12.0, "held to the range");
        assert_eq!(moved.eq_bands, g.eq_bands, "the parametric bands are left alone");
        assert_eq!(set_graphic(g.clone(), 10, 3.0), g, "past the end: nothing");
        // Automatic pre-amp: the largest slider paid back.
        let loud = set_graphic(set_graphic(g.clone(), 0, 6.0), 5, -9.0);
        assert_eq!(SoundSettings { eq_enabled: true, ..loud.clone() }.effective_preamp_db(), -6.0);
        // A preset lands on the sliders; the bands are kept for when parametric comes back.
        let bass = NamedPreset { kind: nori_model::PresetKind::BassBoost, preamp_db: -6.0, bands: vec![nori_model::EqBand { kind: EqKind::LowShelf, freq: 100.0, gain_db: 6.0, q: 0.7 }] };
        let p = apply_preset(loud.clone(), &bass);
        assert!(p.eq_enabled && p.eq_preamp_db.is_none());
        assert!(p.eq_graphic[0] > 5.0 && p.eq_graphic[9].abs() < 0.1, "{:?}", p.eq_graphic);
        assert_eq!(p.eq_bands, loud.eq_bands);
        assert_eq!(eq_reset_bands(p.clone()).eq_graphic, vec![0.0; 10]);
        assert_eq!(eq_reset_bands(p.clone()).eq_bands, p.eq_bands, "reset is the one in use");
        // Filters imported on the parametric equalizer stay filters there; the graphic one fits them (below).
        let imported = import(SoundSettings { eq_mode: EqMode::Parametric, ..p }, "Filter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70\n").unwrap();
        assert_eq!(imported.eq_mode, EqMode::Parametric);
        // Another layout draws the same curve.
        assert_eq!(relayout_graphic(&[0.0, 2.0, 4.0, 6.0, 4.0, 2.0, 0.0, -2.0, -4.0, -6.0], 31).len(), 31);
        assert_eq!(decode_graphic("1,2,3"), None, "not a layout");
        assert_eq!(decode_graphic(&vec!["99"; 10].join(",")), Some(vec![12.0; 10]));
    }

    /// Settings as an install from before the graphic equalizer stored them: everything but the choice.
    fn stored_before(p: StoredPrefs) -> HashMap<String, PrefValue> {
        let mut raw = save(&p);
        raw.remove(EQ_MODE_KEY);
        raw
    }

    #[test]
    fn eq_mode_migration() {
        assert_eq!(load(&HashMap::new()).eq_mode, EqMode::Graphic, "a new install");
        // From before this version, with only the defaults: nothing was set up, so graphic.
        assert_eq!(load(&stored_before(StoredPrefs::default())).eq_mode, EqMode::Graphic);
        // From before, with a parametric equalizer in any form: it stays.
        let d = StoredPrefs::default();
        let mut moved = graphic();
        moved[3].gain_db = 2.5;
        let bass = vec![band_from(1, 100.0, 6.0, 0.7, 0)];
        for (what, p) in [
            ("a band moved", StoredPrefs { eq_bands: moved, ..d.clone() }),
            ("a preset or an AutoEQ curve", StoredPrefs { eq_bands: bass, eq_preamp_db: Some(-6.0), ..d.clone() }),
            ("a band added", StoredPrefs { eq_bands: add_band(d.sound()).eq_bands, ..d.clone() }),
            ("the equalizer on, flat", StoredPrefs { eq_enabled: true, ..d.clone() }),
            ("a pre-amp of its own", StoredPrefs { eq_preamp_db: Some(-3.0), ..d.clone() }),
        ] {
            assert_eq!(load(&stored_before(p)).eq_mode, EqMode::Parametric, "{what}");
        }
        // Once chosen, the choice is what is read, whatever else is stored.
        let chosen = StoredPrefs { eq_mode: EqMode::Graphic, eq_enabled: true, eq_bands: vec![band_from(1, 100.0, 6.0, 0.7, 0)], ..d.clone() };
        assert_eq!(load(&save(&chosen)).eq_mode, EqMode::Graphic);
        assert_eq!(load(&save(&StoredPrefs { eq_mode: EqMode::Parametric, ..d.clone() })).eq_mode, EqMode::Parametric);
        // A sound profile with a curve in it counts; a flat or switched-off one does not.
        assert!(profile_parametric(&sound_json(&SoundSettings { eq_enabled: true, eq_bands: vec![band_from(1, 100.0, 6.0, 0.7, 0)], ..sound() })));
        assert!(!profile_parametric(&sound_json(&SoundSettings { eq_enabled: false, eq_bands: vec![band_from(1, 100.0, 6.0, 0.7, 0)], ..sound() })));
        assert!(!profile_parametric(&sound_json(&SoundSettings { eq_enabled: true, ..sound() })));
        assert!(!profile_parametric("not json"));
    }

    #[test]
    fn correction_fits_graphic_sliders() {
        let graphic = include_str!("../../player/testdata/graphiceq/sennheiser-hd-600.txt");
        let parametric = include_str!("../../player/testdata/graphiceq/sennheiser-hd-600.parametric.txt");
        let g = SoundSettings { eq_mode: EqMode::Graphic, eq_enabled: false, ..sound() };
        let s = import(g.clone(), graphic).unwrap();
        assert!(s.eq_enabled && s.eq_mode == EqMode::Graphic, "it stays on the graphic equalizer");
        assert_eq!(s.eq_graphic.len(), 10);
        assert_eq!(s.eq_bands, g.eq_bands, "the parametric bands are left as they were");
        assert_eq!(s.eq_graphic_target.len(), nori_player::graphic::TARGET_POINTS);
        let preamp = s.eq_preamp_db.unwrap();
        assert!(preamp < -3.0, "the pre-amp pays back the boost: {preamp}");
        // The filters, where there is no curve, are a target too.
        assert!(import(g.clone(), parametric).unwrap().eq_graphic.iter().any(|v| *v != 0.0));
        assert!(matches!(import(g, "Preamp: 0 dB\n"), Err(SoundError::NoFilters)));
        // Another layout is fitted to the correction again, not stretched from the ten sliders.
        let p = StoredPrefs::default().with_sound(s.clone());
        let l = set_by_name(&p, "eqLayout", "31").unwrap().prefs;
        let t: Vec<f64> = s.eq_graphic_target.iter().map(|v| *v as f64).collect();
        let fitted = nori_player::graphic::fit_target(&t, 31, 12.0).unwrap();
        assert_eq!(l.eq_graphic, fitted.sliders.iter().map(|v| *v as f32).collect::<Vec<_>>());
        assert_eq!(l.eq_graphic_target, s.eq_graphic_target);
        // Moved by hand, a preset or a reset: the correction is gone.
        assert!(set_graphic(s.clone(), 0, 1.0).eq_graphic_target.is_empty());
        assert!(eq_reset_bands(s.clone()).eq_graphic_target.is_empty());
        // And it travels in a sound profile.
        assert_eq!(sound_from(&sound_json(&s)).unwrap(), s);
        // How closely it is followed, for the screen.
        let f = crate::dsp::graphic_follow(s.eq_graphic.clone(), s.eq_graphic_target.clone()).unwrap();
        assert!(f.rms_db > 0.0 && f.rms_db < 2.0 && f.max_db >= f.rms_db);
        assert!(crate::dsp::graphic_follow(s.eq_graphic, Vec::new()).is_none());
    }

    #[test]
    fn effect_levels_hold_and_snap_off() {
        for (i, l) in EqLevel::ALL.iter().enumerate() {
            assert_eq!(*l as usize, i, "the door's ordinal is the declaration's");
        }
        let s = sound();
        assert_eq!(set_level(s.clone(), EqLevel::VolumeBoost, 30.0).effects.volume_boost_db, 12.0);
        assert_eq!(set_level(s.clone(), EqLevel::VolumeBoost, 0.1).effects.volume_boost_db, 0.0, "the bottom is off");
        assert_eq!(set_level(s.clone(), EqLevel::Virtualizer, 0.01).effects.virtualizer, 0.0);
        assert_eq!(set_level(s.clone(), EqLevel::BassBoost, f32::NAN).effects.bass_boost_db, 0.0);
        let r = set_level(s.clone(), EqLevel::CompRatio, 0.5);
        assert_eq!((r.effects.comp_ratio, EqLevel::CompRatio.of(&r)), (1.0, 1.0));
        assert_eq!(set_level(s, EqLevel::CompRelease, 5000.0).effects.comp_release_ms, 2000.0);
    }

    #[test]
    fn effects_by_name() {
        let p = StoredPrefs::default();
        let c = set_by_name(&p, "compressorPreset", "strong").unwrap().prefs;
        assert!(c.compressor && c.comp_ratio == 5.0 && c.sound_chain_on());
        assert_eq!(value_of_special(&c, "compressorPreset").as_deref(), Some("STRONG"));
        let custom = set_by_name(&c, "compRatio", "7").unwrap().prefs;
        assert_eq!(value_of_special(&custom, "compressorPreset").as_deref(), Some(""), "moved: none of them");
        assert!(set_by_name(&p, "compressorPreset", "loud").is_none());
        assert_eq!(set_by_name(&p, "volumeBoostDb", "40").unwrap().prefs.volume_boost_db, 12.0);
        // The expander: on by its switch, its controls held and kept in a profile.
        let x = set_by_name(&p, "expander", "true").unwrap().prefs;
        assert!(x.sound_chain_on() && x.effects().player().expander == Some(nori_player::compressor::ExpanderSettings::default()));
        assert_eq!(set_level(x.sound(), EqLevel::ExpThreshold, -200.0).effects.exp_threshold_db, -90.0);
        let r = set_level(x.sound(), EqLevel::ExpRatio, 10.0);
        assert_eq!(sound_from(&sound_json(&r)).unwrap().effects, r.effects);
        // Loudness compensation: on, at the volume the platform says.
        let l = set_by_name(&p, "loudness", "true").unwrap().prefs;
        assert!(l.sound_chain_on());
        let fx = l.effects().player_at(-30.0);
        assert_eq!(fx.loudness, Some(nori_player::contour::Loudness { reference_phon: 80.0, volume_db: -30.0 }));
        assert_eq!(set_by_name(&l, "loudnessRefPhon", "85").unwrap().prefs.effects().player().loudness.unwrap().reference_phon, 85.0);
        let s = set_by_name(&l, "loudnessRefPhon", "70").unwrap().prefs.sound();
        assert_eq!(sound_from(&sound_json(&s)).unwrap().effects, s.effects, "a profile keeps it");
        assert!(set_by_name(&p, "virtualizer", "0.5").unwrap().prefs.sound_chain_on());
        let l = set_by_name(&p, "eqLayout", "31").unwrap().prefs;
        assert_eq!(l.eq_graphic.len(), 31);
        assert_eq!(value_of_special(&l, "eqLayout").as_deref(), Some("31"));
        assert!(set_by_name(&p, "eqLayout", "12").is_none());
        // Five bands are every other one of the ten: moved there, those sliders stay as they were.
        let ten = StoredPrefs { eq_graphic: (1..=10).map(|v| v as f32).collect(), ..p.clone() };
        let five = set_by_name(&ten, "eqLayout", "5").unwrap().prefs;
        assert_eq!(five.eq_graphic, [2.0, 4.0, 6.0, 8.0, 10.0]);
        assert_eq!(decode_graphic("1,2,3,4,5"), Some(vec![1.0, 2.0, 3.0, 4.0, 5.0]), "five sliders is a layout");
    }

    #[test]
    fn sound_bypass() {
        let busy = StoredPrefs { eq_enabled: true, crossfeed_db: 4.5, limiter: true, compressor: true, ..StoredPrefs::default() };
        assert!(busy.sound_chain_on());
        let bypassed = set_by_name(&busy, "soundBypass", "true").unwrap().prefs;
        assert!(!bypassed.sound_chain_on(), "nothing in the chain, so offload may play it");
        assert!(bypassed.eq_enabled && bypassed.compressor, "the settings are kept for when it is off again");
        let s = bypassed.sound();
        assert!(s.bypass);
        assert!(sound_from(&sound_json(&s)).unwrap().bypass, "a profile keeps it");
        assert!(!sound_from("{}").unwrap().bypass, "an old profile has it off");
        assert!(StoredPrefs::default().with_sound(s).sound_bypass);
        assert!(!sound_json(&busy.sound()).contains("bypass"), "left out when off, as before");
    }

    #[test]
    fn crossfeed_presets() {
        let p = StoredPrefs::default();
        assert_eq!(value_of_special(&p, "crossfeedPreset").as_deref(), Some("OFF"));
        let m = set_by_name(&p, "crossfeedPreset", "jan_meier").unwrap().prefs;
        assert_eq!((m.crossfeed_hz, m.crossfeed_db), (650.0, 9.5));
        assert!(m.sound_chain_on(), "a preset turns the crossfeed on");
        assert_eq!(value_of_special(&m, "crossfeedPreset").as_deref(), Some("JAN_MEIER"));
        let c = set_by_name(&m, "crossfeedPreset", "CHU_MOY").unwrap().prefs;
        assert_eq!((c.crossfeed_hz, c.crossfeed_db), (700.0, 6.0));
        let custom = set_by_name(&c, "crossfeedHz", "900").unwrap().prefs;
        assert_eq!(value_of_special(&custom, "crossfeedPreset").as_deref(), Some(""), "moved: custom");
        assert_eq!(set_by_name(&p, "crossfeedHz", "5000").unwrap().prefs.crossfeed_hz, 2000.0);
        assert!(set_by_name(&p, "crossfeedPreset", "loud").is_none());
        let off = set_by_name(&custom, "crossfeedPreset", "OFF").unwrap().prefs;
        assert_eq!((off.crossfeed_db, off.crossfeed_hz), (0.0, 900.0), "off keeps the cutoff for next time");
        // A sound profile keeps the cutoff, and an old one without it reads as bs2b's default.
        let s = sound_from(&sound_json(&custom.sound())).unwrap();
        assert_eq!(s.crossfeed_hz, 900.0);
        assert_eq!(sound_from("{\"crossfeedDb\": 4.5}").unwrap().crossfeed_hz, 700.0);
    }

    #[test]
    fn import_parametric_preset() {
        let s = import(sound(), "Preamp: -6.2 dB\nFilter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70\n").unwrap();
        assert!(s.eq_enabled);
        assert_eq!(s.eq_preamp_db, Some(-6.2));
        assert_eq!(s.eq_bands.len(), 1);
        assert!(matches!(import(sound(), "Preamp: 0 dB\n"), Err(SoundError::NoFilters)));
        let zero = import(sound(), "Filter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70\n").unwrap();
        assert_eq!(zero.eq_preamp_db, Some(0.0), "an imported pre-amp is kept even at 0");
    }

    #[test]
    fn storage_index_files_match_db() {
        let names = ["nori.db", "nori.db-wal", "nori.db-shm", "nori.db-journal", "certs", "other.db", "nori.db.bak"].map(String::from).to_vec();
        assert_eq!(storage_index_files(names), [0, 1, 2]);
    }
}
