//! The settings as they are stored, one value per key (settings_store.rs keeps them in the app's
//! database). Everything here is a free function with no database: what is stored goes in as it is and
//! one checked record comes back (defaults and ranges dealt with); saving is one call that says what to
//! write. The wire formats (the band list, a sound profile's JSON, a server profile's JSON) live here
//! too, so another player reading the same store gets the same settings.

use std::collections::HashMap;

use nori_model::{EqBand, EqKind, EqPreset, NamedPreset, TransitionPrefs};
use serde_json::{Map, Value};

use crate::codec::{clamped, names, on, within, Choice, Custom, Picks, Preamp, Quality, Raw, Row, FLAG, FLOAT, INT, K, LONG, PICK, PICK_NEAREST, TEXT};
use crate::lyrics_sources;
use crate::settings_store::{APPLY_AUDIO, APPLY_GAIN, PLAYER, REPLAN, SOUND};

/// One stored value.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefValue {
    Flag { v: bool },
    Number { v: i32 },
    Big { v: i64 },
    Decimal { v: f32 },
    Text { v: String },
    Texts { v: Vec<String> },
}

/// One equalizer filter. It is stored with its kind's and channel's ordinals (the order of each is the
/// wire format, so it must not change).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SoundBand {
    pub kind: EqKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
    pub channel: BandChannel,
}

/// Which side a band applies to; the client names each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BandChannel {
    Both,
    Left,
    Right,
}

/// A band as the platform's plain arrays carry it: its kind's and channel's ordinals, a kind or a channel
/// out of range the first one.
pub fn band_from(kind: i32, freq: f32, gain_db: f32, q: f32, channel: i32) -> SoundBand {
    SoundBand { kind: EqKind::nth(kind).unwrap_or(EqKind::Peaking), freq, gain_db, q, channel: BandChannel::nth(channel).unwrap_or(BandChannel::Both) }
}

/// ReplayGain: off, the track's gain, the album's, or (auto) the album's while the neighbours in the queue
/// are from the same album and the track's otherwise.
pub use nori_model::GainMode;

// The enums the player defines, as settings: stored by their ordinals, in the order they are declared.
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

/// Which equalizer plays: the parametric one (filters of any kind, each with its own gain) or the graphic
/// one (fixed ISO bands whose sliders are the response). Each keeps its own settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum EqMode {
    Parametric,
    Graphic,
}

/// The light or dark look: the system's, or always one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ThemeMode {
    System,
    Light,
    Dark,
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

/// What the queue is extended with when the last song starts: songs, or one whole album at a time.
/// Someone who listens to records wants the next record, not fifteen loose songs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum AutoFillKind {
    Songs,
    Albums,
}

/// What the songs the queue is extended with are chosen by: what the server thinks is similar, or the
/// artist, genre or decade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, nori_settings_derive::Choice)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum AutoFillBasis {
    Similar,
    Artist,
    Genre,
    Era,
}

/// The home page's shelves, stored by name; the client names each.
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

/// One saved server. Each profile has its own index database, so switching is instant and nothing is re-synced.
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

// ---- the settings, one line each (codec.rs says how to read a line) ----

/// The stream qualities offered, the original file first.
const QUALITIES: &[&str] = &["0:", "320:mp3", "192:opus", "128:opus", "96:opus", "64:opus"];

/// Every setting. Each field's `#[setting(...)]` line is all there is to say about it: the key it is stored
/// under, its codec, its default, the name it is changed and read by, what a client offers for it and what a
/// change asks of the player (see nori-settings-derive). The enums are stored as their ordinals.
#[derive(Debug, Clone, PartialEq, nori_settings_derive::Settings)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StoredPrefs {
    // The servers.
    #[setting("servers", SERVERS, default = Vec::new(), hidden)]
    pub servers: Vec<SavedServer>,
    #[setting("activeServerId", TEXT, default = String::new(), hidden)]
    pub active_server_id: String,
    // Between songs.
    #[setting("crossfadeSec", INT, default = 0, show = K::Choice(&["0", "2", "4", "6", "8", "12"]), effect = APPLY_AUDIO | REPLAN)]
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
    /// "Better beat detection": Beat This!, a neural beat tracker, reads the first and last half minute of the
    /// songs coming up for AutoMix's beat grids, once per song. Only in a build with the `neural-beats` feature;
    /// its model is downloaded once. Off by default.
    #[setting("autoMixBetterBeats", FLAG, default = false, show = K::Switch)]
    pub auto_mix_better_beats: bool,
    /// The beat model may be downloaded over mobile data; otherwise it waits for Wi-Fi.
    #[setting("autoMixBeatsMobileData", FLAG, default = false, show = K::Switch)]
    pub auto_mix_beats_mobile_data: bool,
    #[setting("crossfadeKeepAlbums", FLAG, default = true, show = K::Switch, effect = REPLAN)]
    pub crossfade_keep_albums: bool,
    #[setting("fadeMs", within(0, 5000), default = 0, show = K::Choice(&["0", "150", "300", "500", "1000"]), effect = PLAYER)]
    pub fade_ms: i32,
    // Controls.
    #[setting("previousAlwaysSkips", FLAG, default = false, show = K::Switch)]
    pub previous_always_skips: bool,
    /// Headphones taken off paused the music (they went away, or said pause themselves): the same ones
    /// put back on within a while bring it back, fading in (nori_player::headphones). Off by default.
    #[setting("headphonesResume", FLAG, default = false, show = K::Switch)]
    pub headphones_resume: bool,
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
    #[setting("skipOnError", FLAG, default = true, show = K::Switch)]
    pub skip_on_error: bool,
    #[setting("bridgeOffline", FLAG, default = false, show = K::Switch)]
    pub bridge_offline: bool,
    // Sound.
    #[setting("eqEnabled", FLAG, default = false, name = "eq", show = K::Switch, effect = APPLY_AUDIO | SOUND)]
    pub eq_enabled: bool,
    #[setting("eqBands", BANDS, default = graphic(), hidden, effect = SOUND)]
    pub eq_bands: Vec<SoundBand>,
    /// Which equalizer plays while it is on; the other keeps its settings. Graphic on a new install; an
    /// install from before there was a choice keeps the parametric equalizer it has set up ([`load`]).
    #[setting("eqMode", PICK, default = EqMode::Graphic, show = K::Named(EqMode::NAMES), effect = SOUND)]
    pub eq_mode: EqMode,
    /// The graphic equalizer's sliders, dB, low to high: 10, 15 or 31 of them (`nori_player::graphic`).
    #[setting("eqGraphic", GRAPHIC, default = vec![0.0; 10], hidden, effect = SOUND)]
    pub eq_graphic: Vec<f32>,
    /// The headphone correction the graphic sliders were fitted to (`nori_player::graphic::target_grid`,
    /// dB), so another layout is fitted to it again and the screen says how closely it is followed; empty
    /// once the sliders are the listener's own.
    #[setting("eqGraphicTarget", TARGET, default = Vec::new(), hidden, effect = SOUND)]
    pub eq_graphic_target: Vec<f32>,
    /// A low shelf of this many dB; 0 is off.
    #[setting("bassBoostDb", clamped(0.0, BASS_BOOST_MAX), default = 0.0, show = K::Level(0.0, BASS_BOOST_MAX), effect = SOUND)]
    pub bass_boost_db: f32,
    /// The virtualizer's strength, 0 (off) to 1.
    #[setting("virtualizer", clamped(0.0, 1.0), default = 0.0, show = K::Level(0.0, 1.0), effect = SOUND)]
    pub virtualizer: f32,
    /// Louder than the music is, dB, with the limiter behind it; 0 is off.
    #[setting("volumeBoostDb", clamped(0.0, VOLUME_BOOST_MAX), default = 0.0, show = K::Level(0.0, VOLUME_BOOST_MAX), effect = SOUND)]
    pub volume_boost_db: f32,
    #[setting("compressor", FLAG, default = false, show = K::Switch, effect = SOUND)]
    pub compressor: bool,
    #[setting("compThresholdDb", clamped(-60.0, 0.0), default = -20.0, show = K::Level(-60.0, 0.0), effect = SOUND)]
    pub comp_threshold_db: f32,
    #[setting("compRatio", clamped(1.0, 20.0), default = 3.0, show = K::Level(1.0, 20.0), effect = SOUND)]
    pub comp_ratio: f32,
    #[setting("compAttackMs", clamped(0.1, 200.0), default = 10.0, show = K::Level(0.1, 200.0), effect = SOUND)]
    pub comp_attack_ms: f32,
    #[setting("compReleaseMs", clamped(10.0, 2000.0), default = 180.0, show = K::Level(10.0, 2000.0), effect = SOUND)]
    pub comp_release_ms: f32,
    #[setting("compMakeupDb", clamped(0.0, 24.0), default = 4.5, show = K::Level(0.0, 24.0), effect = SOUND)]
    pub comp_makeup_db: f32,
    #[setting("compKneeDb", clamped(0.0, 24.0), default = 6.0, show = K::Level(0.0, 24.0), effect = SOUND)]
    pub comp_knee_db: f32,
    #[setting("mono", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO | SOUND)]
    pub mono: bool,
    #[setting("limiter", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO | SOUND)]
    pub limiter: bool,
    #[setting("eqPreampDb", Preamp(EQ_RANGES.preamp), default = None, show = K::Level(EQ_RANGES.preamp.min, EQ_RANGES.preamp.max), effect = SOUND)]
    pub eq_preamp_db: Option<f32>,
    #[setting("crossfeedDb", FLOAT, default = 0.0, show = K::Level(EQ_RANGES.crossfeed.min, EQ_RANGES.crossfeed.max), effect = SOUND)]
    pub crossfeed_db: f32,
    /// The crossfeed's cutoff (bs2b's `fcut`), Hz: 700 is bs2b's default; `crossfeedPreset` sets it and the
    /// level together.
    #[setting("crossfeedHz", clamped(300.0, 2000.0), default = 700.0, show = K::Level(EQ_RANGES.crossfeed_cut.min, EQ_RANGES.crossfeed_cut.max), effect = SOUND)]
    pub crossfeed_hz: f32,
    /// Read by the sound chain as it runs: a change only rebuilds the chain when it starts or stops it.
    #[setting("balance", FLOAT, default = 0.0, hidden, effect = SOUND)]
    pub balance: f32,
    #[setting("limiterThresholdDb", FLOAT, default = -1.0, show = K::Level(EQ_RANGES.limiter.min, EQ_RANGES.limiter.max), effect = SOUND)]
    pub limiter_threshold_db: f32,
    #[setting("autoEqAuto", FLAG, default = false, show = K::Switch)]
    pub auto_eq_auto: bool,
    /// Keep the AutoEQ headphone list on the device: fetched on an unmetered network when it is missing
    /// or a month old (`nori_devices::autoeq::index_due`). Needs `third_party_lookups`. On by default.
    #[setting("autoEqDownload", FLAG, default = true, show = K::Switch, lookups)]
    pub auto_eq_download: bool,
    #[setting("profilePerOutput", FLAG, default = true, show = K::Switch)]
    pub profile_per_output: bool,
    #[setting("replayGain", PICK_NEAREST, default = GainMode::Off, show = K::Named(GainMode::NAMES), effect = APPLY_GAIN | REPLAN)]
    pub replay_gain: GainMode,
    #[setting("preampDb", within(REPLAY_GAIN_PREAMP.0, REPLAY_GAIN_PREAMP.1), default = 0.0, show = K::Level(EQ_RANGES.replay_gain_preamp.min, EQ_RANGES.replay_gain_preamp.max), effect = APPLY_GAIN)]
    pub preamp_db: f32,
    #[setting("untaggedGainDb", FLOAT, default = -6.0, show = K::Choice(&["0", "-3", "-6", "-9", "-12"]), effect = APPLY_GAIN)]
    pub untagged_gain_db: f32,
    #[setting("hiRes", FLAG, default = false, show = K::Switch, effect = PLAYER)]
    pub hi_res: bool,
    #[setting("bitPerfect", FLAG, default = false, show = K::Switch, effect = APPLY_AUDIO)]
    pub bit_perfect: bool,
    #[setting("offload", FLAG, default = true, show = K::Switch, effect = APPLY_AUDIO)]
    pub offload: bool,
    // Appearance.
    #[setting("theme", PICK, default = ThemeMode::System, show = K::Named(ThemeMode::NAMES))]
    pub theme: ThemeMode,
    #[setting("amoled", FLAG, default = false, show = K::Switch)]
    pub amoled: bool,
    #[setting("playerColours", FLAG, default = true, show = K::Switch)]
    pub player_colours: bool,
    #[setting("dynamicColor", FLAG, default = true, show = K::Switch)]
    pub dynamic_color: bool,
    #[setting("accent", LONG, default = 0xFF6750A4, show = K::Colour)]
    pub accent: i64,
    #[setting("coverColors", FLAG, default = true, show = K::Switch)]
    pub cover_colors: bool,
    #[setting("softSleeve", FLAG, default = true, show = K::Switch)]
    pub soft_sleeve: bool,
    /// Moving covers: an album's motion artwork from Apple Music plays in the player's sleeve, where it
    /// has one. Needs `third_party_lookups`. Off by default; off, nothing of it is built.
    #[setting("motionArtwork", FLAG, default = false, show = K::Switch, lookups)]
    pub motion_artwork: bool,
    /// Moving covers only on unmetered networks: each is a few megabytes.
    #[setting("motionArtworkWifiOnly", FLAG, default = true, show = K::Switch)]
    pub motion_artwork_wifi_only: bool,
    #[setting("favouriteNotice", FLAG, default = true, show = K::Switch)]
    pub favourite_notice: bool,
    #[setting("uiScale", FLOAT, default = 0.0, show = K::Choice(&["0", "0.9", "1", "1.1"]))]
    pub ui_scale: f32,
    #[setting("reduceMotion", FLAG, default = false, show = K::Switch)]
    pub reduce_motion: bool,
    /// Animate even with Android's animations off; on unless the listener turns it off. Stored as
    /// "animateAnyway": the old "ignoreSystemMotion" was written off on every phone before this was the
    /// default, and it would have kept the animations off there.
    #[setting("animateAnyway", FLAG, default = true, name = "ignoreSystemMotion", show = K::Switch)]
    pub ignore_system_motion: bool,
    // Lyrics.
    #[setting("lyricsSweep", FLAG, default = true, show = K::Switch)]
    pub lyrics_sweep: bool,
    #[setting("lyricsSize", within(0, 2), default = 1, show = K::Choice(&["0", "1", "2"]))]
    pub lyrics_size: i32,
    #[setting("lyricsTranslation", FLAG, default = true, show = K::Switch)]
    pub lyrics_translation: bool,
    #[setting("lyricsKeepScreenOn", FLAG, default = true, show = K::Switch)]
    pub lyrics_keep_screen_on: bool,
    /// Look lyrics up online when the server has no timed ones; needs `third_party_lookups`. Stored as
    /// "lyricsLrclib", from when LRCLIB was the only place asked (and still changed by that name).
    #[setting("lyricsLrclib", FLAG, default = true, name = "lyricsOnline", show = K::Switch, lookups)]
    pub lyrics_online: bool,
    /// Every lyrics service by name, in the order they rank (`lyrics_sources`).
    #[setting("lyricsOrder", LYRICS_ORDER, default = lyrics_sources::default_order())]
    pub lyrics_order: Vec<String>,
    /// The lyrics services switched on, by name (changed by `lyricsService:<name>`).
    #[setting("lyricsOn", LYRICS_ON, default = lyrics_sources::default_on(), hidden)]
    pub lyrics_on: Vec<String>,
    /// Keep asking past lyrics timed line by line for lyrics timed word by word, whoever ranks higher.
    #[setting("lyricsPreferWords", FLAG, default = true, show = K::Switch)]
    pub lyrics_prefer_words: bool,
    /// The user's own PaxSenix key, for its Spotify and Musixmatch lyrics; empty for none.
    #[setting("paxSenixKey", TEXT, default = String::new(), show = K::Text)]
    pub paxsenix_key: String,
    /// A BetterLyrics key, with which it looks up songs it has not stored yet; empty for none.
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
    #[setting("tasteModel", FLAG, default = true, show = K::Switch)]
    pub taste_model: bool,
    #[setting("scrobble", FLAG, default = true, show = K::Switch)]
    pub scrobble: bool,
    #[setting("scrobblePercent", INT, default = 50, show = K::Choice(&["25", "50", "75", "90", "100"]))]
    pub scrobble_percent: i32,
    /// "Look things up online": the switch over everything the app asks a third party for by itself -
    /// missing lyrics, the AutoEQ list, moving covers - each of which has its own switch under it. On for a
    /// new install (lyrics and the AutoEQ list are wanted out of the box; moving covers stay off).
    #[setting("thirdPartyLookups", FLAG, default = true, show = K::Switch)]
    pub third_party_lookups: bool,
    /// "Check for updates": when the app starts, at most once a day, it asks GitHub for its latest release
    /// (nori-core update.rs). Its own switch, not under the lookups one: it is the app's, not a service's.
    #[setting("updateCheck", FLAG, default = true, show = K::Switch)]
    pub update_check: bool,
    /// The home page's shelves, in order; a row that is not listed is hidden.
    #[setting("homeRows", Picks, default = HomeRow::ALL.to_vec(), hidden)]
    pub home_rows: Vec<HomeRow>,
    #[setting("pinnedPlaylists", LINES, default = Vec::new(), hidden)]
    pub pinned_playlists: Vec<String>,
    #[setting("listPrefs", LIST_PREFS, default = HashMap::new(), hidden)]
    pub list_prefs: HashMap<String, String>,
    // Downloads and storage.
    #[setting("wifi", Quality, default = SavedQuality::default(), show = K::Choice(QUALITIES))]
    pub wifi: SavedQuality,
    #[setting("mobile", Quality, default = SavedQuality { bit_rate: 192, format: "opus".to_string() }, show = K::Choice(QUALITIES))]
    pub mobile: SavedQuality,
    #[setting("download", Quality, default = SavedQuality::default(), show = K::Choice(QUALITIES))]
    pub download: SavedQuality,
    /// Songs downloaded at the same time; the rest wait their turn in the order they were asked for.
    #[setting("parallelDownloads", clamped(1, 10), default = 5, show = K::Choice(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]))]
    pub parallel_downloads: i32,
    #[setting("precacheWifi", INT, default = 2, show = K::Choice(&["1", "2", "3", "5", "10"]))]
    pub precache_wifi: i32,
    #[setting("precacheMobile", INT, default = 1, show = K::Choice(&["1", "2", "3", "5"]))]
    pub precache_mobile: i32,
    /// Covers of the songs coming up fetched ahead; the one before is always kept.
    #[setting("coversAhead", clamped(0, 10), default = 3, show = K::Choice(&["0", "1", "2", "3", "5", "8", "10"]))]
    pub covers_ahead: i32,
    #[setting("cacheMb", within(256, 16384), default = 1024, show = K::Choice(&["256", "1024", "4096", "16384"]))]
    pub cache_mb: i32,
}

/// The settings a client can offer that are not a field of their own: "on mobile data" is "Wi-Fi only"
/// turned round, and the server in use's own (which music folder it browses, and the cap on its second
/// address). Read and changed in [`value_of_special`] and [`set_special`].
pub(crate) const SPECIAL_SPECS: &[(&str, K)] = &[
    ("motionArtworkMobile", K::Switch),
    ("musicFolder", K::Choice(&[])),
    ("altMaxBitRate", K::Choice(&["0", "320", "192", "128", "96"])),
    ("compressorPreset", K::Choice(&["GENTLE", "BALANCED", "STRONG"])),
    ("crossfeedPreset", K::Choice(&["OFF", "DEFAULT", "CHU_MOY", "JAN_MEIER"])),
    ("eqLayout", K::Choice(&["10", "15", "31"])),
];

const SERVERS: Custom<Vec<SavedServer>> = Custom {
    // A list that does not read, or a server without an id, loses the whole list.
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

/// The graphic sliders, comma-separated; a list that is not a layout's length, or does not read, is the
/// default's.
const GRAPHIC: Custom<Vec<f32>> = Custom {
    load: |t, d| t.and_then(decode_graphic).unwrap_or(d),
    save: |g| g.iter().map(|v| kotlin_float(*v)).collect::<Vec<_>>().join(","),
    set: Some(decode_graphic),
    show: Some(|g| g.iter().map(|v| kotlin_float(*v)).collect::<Vec<_>>().join(",")),
};

/// A headphone correction's target, comma-separated: as many numbers as the target's grid has, or none.
const TARGET: Custom<Vec<f32>> = Custom { load: |t, d| t.map_or(d, decode_target), save: |g| encode_floats(g), set: None, show: None };

fn encode_floats(g: &[f32]) -> String {
    g.iter().map(|v| kotlin_float(*v)).collect::<Vec<_>>().join(",")
}

/// A target from its text; anything that is not one is none.
pub fn decode_target(s: &str) -> Vec<f32> {
    let g: Option<Vec<f32>> = s.split(',').map(float).collect();
    g.filter(|g| g.len() == nori_player::graphic::TARGET_POINTS && g.iter().all(|v| v.is_finite())).unwrap_or_default()
}

/// Graphic sliders from their text: one number per band of a layout, each held to the gain range.
pub fn decode_graphic(s: &str) -> Option<Vec<f32>> {
    let g: Vec<f32> = s.split(',').map(|v| float(v).map(|v| EQ_RANGES.gain.hold(v))).collect::<Option<_>>()?;
    nori_player::graphic::LAYOUTS.contains(&g.len()).then_some(g)
}

/// How far the bass boost and the volume boost go, dB (the player's own limits).
const BASS_BOOST_MAX: f32 = nori_player::dsp::BASS_BOOST_MAX_DB as f32;
const VOLUME_BOOST_MAX: f32 = nori_player::dsp::VOLUME_BOOST_MAX_DB as f32;

/// Every service, by name: the stored ranking completed with any service it does not name.
const LYRICS_ORDER: Custom<Vec<String>> = Custom {
    load: |t, _| lyrics_sources::complete_order(&t.map(names).unwrap_or_default()),
    save: |o| o.join(","),
    set: Some(|v| Some(lyrics_sources::complete_order(&names(v)))),
    show: Some(|o| o.join(",")),
};

const LYRICS_ON: Custom<Vec<String>> = Custom { load: |t, d| t.map_or(d, |s| lyrics_sources::known(&names(s))), save: |o| o.join(","), set: None, show: None };

/// One per line, the empty ones left out.
const LINES: Custom<Vec<String>> =
    Custom { load: |t, _| t.map_or_else(Vec::new, |s| s.split('\n').filter(|p| !p.is_empty()).map(str::to_string).collect()), save: |l| l.join("\n"), set: None, show: None };

/// A JSON object of texts; anything that does not read is empty.
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
    pub limiter: bool,
    pub limiter_threshold_db: f32,
    pub effects: SoundEffects,
    pub replay_gain: GainMode,
    pub preamp_db: f32,
    pub crossfade_sec: i32,
    pub hi_res: bool,
    pub bit_perfect: bool,
}

/// The effects besides the equalizer: bass boost, virtualizer, volume boost and the compressor.
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
}

impl Default for SoundEffects {
    fn default() -> Self {
        StoredPrefs::default().effects()
    }
}

impl SoundEffects {
    /// Whether any of them touches the samples.
    pub fn on(&self) -> bool {
        self.player().on()
    }

    /// The compressor's controls, on or not.
    pub fn compressor_settings(&self) -> nori_player::compressor::CompressorSettings {
        nori_player::compressor::CompressorSettings {
            threshold_db: self.comp_threshold_db as f64,
            ratio: self.comp_ratio as f64,
            attack_ms: self.comp_attack_ms as f64,
            release_ms: self.comp_release_ms as f64,
            makeup_db: self.comp_makeup_db as f64,
            knee_db: self.comp_knee_db as f64,
        }
    }

    /// As the sound chain takes them.
    pub fn player(&self) -> nori_player::dsp::Effects {
        nori_player::dsp::Effects {
            bass_boost_db: self.bass_boost_db as f64,
            compressor: self.compressor.then(|| self.compressor_settings()),
            virtualizer: self.virtualizer as f64,
            boost_db: self.volume_boost_db as f64,
        }
    }

    /// Which built-in compressor setting these are, if any.
    pub fn compressor_preset(&self) -> Option<nori_player::compressor::CompressorPreset> {
        let now = self.compressor_settings();
        nori_player::compressor::CompressorPreset::ALL.into_iter().find(|p| {
            let s = p.settings();
            [s.threshold_db - now.threshold_db, s.ratio - now.ratio, s.attack_ms - now.attack_ms, s.release_ms - now.release_ms, s.makeup_db - now.makeup_db, s.knee_db - now.knee_db]
                .iter()
                .all(|d| d.abs() < 1e-3)
        })
    }

    /// A built-in compressor setting taken, and the compressor switched on.
    pub fn with_compressor_preset(self, p: nori_player::compressor::CompressorPreset) -> SoundEffects {
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
    /// The part of the settings a sound profile remembers.
    pub fn sound(&self) -> SoundSettings {
        SoundSettings {
            eq_enabled: self.eq_enabled,
            eq_bands: self.eq_bands.clone(),
            eq_mode: self.eq_mode,
            eq_graphic: self.eq_graphic.clone(),
            eq_graphic_target: self.eq_graphic_target.clone(),
            eq_preamp_db: self.eq_preamp_db,
            crossfeed_db: self.crossfeed_db,
            crossfeed_hz: self.crossfeed_hz,
            balance: self.balance,
            mono: self.mono,
            limiter: self.limiter,
            limiter_threshold_db: self.limiter_threshold_db,
            effects: self.effects(),
            replay_gain: self.replay_gain,
            preamp_db: self.preamp_db,
            crossfade_sec: self.crossfade_sec,
            hi_res: self.hi_res,
            bit_perfect: self.bit_perfect,
        }
    }

    /// These settings with the part a sound profile remembers taken from `s`.
    pub fn with_sound(self, s: SoundSettings) -> StoredPrefs {
        StoredPrefs {
            eq_enabled: s.eq_enabled,
            eq_bands: s.eq_bands,
            eq_mode: s.eq_mode,
            eq_graphic: s.eq_graphic,
            eq_graphic_target: s.eq_graphic_target,
            eq_preamp_db: s.eq_preamp_db,
            crossfeed_db: s.crossfeed_db,
            crossfeed_hz: s.crossfeed_hz,
            balance: s.balance,
            mono: s.mono,
            limiter: s.limiter,
            limiter_threshold_db: s.limiter_threshold_db,
            bass_boost_db: s.effects.bass_boost_db,
            virtualizer: s.effects.virtualizer,
            volume_boost_db: s.effects.volume_boost_db,
            compressor: s.effects.compressor,
            comp_threshold_db: s.effects.comp_threshold_db,
            comp_ratio: s.effects.comp_ratio,
            comp_attack_ms: s.effects.comp_attack_ms,
            comp_release_ms: s.effects.comp_release_ms,
            comp_makeup_db: s.effects.comp_makeup_db,
            comp_knee_db: s.effects.comp_knee_db,
            replay_gain: s.replay_gain,
            preamp_db: s.preamp_db,
            crossfade_sec: s.crossfade_sec,
            hi_res: s.hi_res,
            bit_perfect: s.bit_perfect,
            ..self
        }
    }

    /// The effects besides the equalizer, as a sound profile keeps them.
    pub fn effects(&self) -> SoundEffects {
        SoundEffects {
            bass_boost_db: self.bass_boost_db,
            virtualizer: self.virtualizer,
            volume_boost_db: self.volume_boost_db,
            compressor: self.compressor,
            comp_threshold_db: self.comp_threshold_db,
            comp_ratio: self.comp_ratio,
            comp_attack_ms: self.comp_attack_ms,
            comp_release_ms: self.comp_release_ms,
            comp_makeup_db: self.comp_makeup_db,
            comp_knee_db: self.comp_knee_db,
        }
    }

    /// Whether anything in the sample domain is on: the equalizer, crossfeed, balance, mono, the limiter
    /// or an effect. It then sits in the chain and audio offload stands down.
    pub fn sound_chain_on(&self) -> bool {
        nori_player::sound::sound_on(self.eq_enabled, self.crossfeed_db, self.balance, self.mono, self.limiter, self.effects().on())
    }

    /// What the transition planner takes from the settings (`nori_automix::planner::settings_changed`).
    pub fn transition_prefs(&self) -> TransitionPrefs {
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
            // AutoMix's loudness matching stands down under ReplayGain.
            replay_gain: self.replay_gain != GainMode::Off,
        }
    }
}

/// A sound that cannot be made: a preset with no filters in it, or the profiles not reachable. The
/// client words each (the messages here are for the log).
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

// ---- ranges, shared by loading and by the test bridge's setter ----

/// Speed and pitch.
const RATE: (f32, f32) = (0.25, 4.0);
/// ReplayGain's overall level, as its slider offers it.
pub(crate) const REPLAY_GAIN_PREAMP: (f32, f32) = (-12.0, 6.0);

/// The ten default bands: peaking filters an octave apart.
pub fn graphic() -> Vec<SoundBand> {
    [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0]
        .into_iter()
        .map(|freq| SoundBand { kind: EqKind::Peaking, freq, gain_db: 0.0, q: 1.41, channel: BandChannel::Both })
        .collect()
}

// ---- the band list: "kind:freq:gain:q:channel" per band, bands joined by ';' ----

/// Reads the stored band list. A band that does not read is dropped; `None` when none is left (the
/// caller falls back to the ten graphic bands).
pub fn decode_bands(s: &str) -> Option<Vec<SoundBand>> {
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

/// A float written the way the Kotlin side always wrote it ("1000.0", "1.41", "1.0E-5"), so what is
/// stored does not change shape when a different side saves it.
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

// ---- JSON read with a default for what is missing ----

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

// ---- a sound profile's JSON ----

/// Reads a saved sound; `None` when it is not a sound at all.
pub fn sound_from(json: &str) -> Option<SoundSettings> {
    let v: Value = serde_json::from_str(json).ok()?;
    let o = v.as_object()?;
    // A pre-amp that is there must be a number: null or anything else and the sound is not read.
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
        },
        replay_gain: GainMode::ALL[opt_i32(o, "replayGain").clamp(0, GainMode::ALL.len() as i32 - 1) as usize],
        preamp_db: opt_f64(o, "preampDb", 0.0) as f32,
        crossfade_sec: opt_i32(o, "crossfadeSec"),
        hi_res: opt_bool(o, "hiRes"),
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
    ] {
        o.insert(k.into(), (v as f64).into());
    }
    o.insert("compressor".into(), e.compressor.into());
    o.insert("replayGain".into(), s.replay_gain.ordinal().into());
    o.insert("preampDb".into(), (s.preamp_db as f64).into());
    o.insert("crossfadeSec".into(), s.crossfade_sec.into());
    o.insert("hiRes".into(), s.hi_res.into());
    o.insert("bitPerfect".into(), s.bit_perfect.into());
    Value::Object(o).to_string()
}

// ---- a server profile's JSON ----

/// One server from the stored list; `None` without an id.
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

// ---- loading, saving and changing by name, through the table ----

/// Everything stored, as it is, into the settings: defaults for what is missing or of the wrong type,
/// ranges enforced.
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

/// Where the equalizer's choice is stored. Settings stored before there was one have no such key.
pub const EQ_MODE_KEY: &str = "eqMode";

/// Whether these settings (stored before the equalizer had a graphic mode) have a parametric equalizer
/// set up: switched on, a pre-amp of its own, or bands that are not the ten flat ones it starts with (a
/// preset, an AutoEQ curve, a band moved or added). Such an install keeps the parametric equalizer; any
/// other gets the graphic one, as a new install does.
pub fn parametric_set_up(p: &StoredPrefs) -> bool {
    p.eq_enabled || p.eq_preamp_db.is_some() || p.eq_bands != graphic()
}

/// The same for a saved sound profile's JSON (`profiles.json`): one with a parametric equalizer in it
/// keeps an install on the parametric equalizer too.
pub fn profile_parametric(json: &str) -> bool {
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

/// What a change by name did: the settings after it, whether the stream cache has to shrink to a new
/// limit now, and whether the active server's own profile changed (its connection is set up again).
/// `effect` is what the player has to apply again (`settings_store`'s bits), once the change is kept.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SettingChange {
    pub prefs: StoredPrefs,
    pub apply_cache_limit: bool,
    pub server: bool,
    pub effect: u32,
}

/// The setting a client changes and reads by `name`, from the table.
pub(crate) fn row(name: &str) -> Option<&'static Row> {
    ROWS.iter().find(|r| r.name == Some(name))
}

/// Changes one setting by name: the settings screen's rows (each row says which name it sets) and the
/// debug test bridge (`tools/app.sh set <name> <value>`). A switch reads "true"/"1" as on; an enum reads
/// its name or its ordinal; a number that does not read leaves the setting as it is. Anything that is
/// not a setting is `None`, so a typo in a script fails loudly instead of silently doing nothing.
pub fn set_by_name(p: &StoredPrefs, name: &str, value: &str) -> Option<SettingChange> {
    let mut n = p.clone();
    let mut server = false;
    // Lyrics online still answer to the name they are stored under.
    let name = if name == "lyricsLrclib" { "lyricsOnline" } else { name };
    match set_special(p, &mut n, &mut server, name, value) {
        Some(done) => done?,
        None => {
            let row = row(name)?;
            (row.set)(&mut n, value)?;
            // A switch over something looked up online switches the lookups on with it; off leaves them.
            if row.lookups && on(value) {
                n.third_party_lookups = true;
            }
        }
    }
    // "Space for streamed music" is applied at once instead of at the next track.
    Some(SettingChange { prefs: n, apply_cache_limit: name == "cacheMb", server, effect: 0 })
}

/// The changes by name the table cannot say line by line: `None` for a name that is the table's, else
/// whether the value was taken.
fn set_special(p: &StoredPrefs, n: &mut StoredPrefs, server: &mut bool, name: &str, value: &str) -> Option<Option<()>> {
    let service = |v: &str| {
        let (service, at) = v.split_once(':')?;
        Some((lyrics_sources::LyricsService::named(service)?, at.trim().to_string()))
    };
    Some(match name {
        // "raw" or "<format>:<kbps>" (e.g. "opus:128"): what streams on Wi-Fi, for checks of a codec.
        "wifiQuality" => match value.split_once(':') {
            Some((format, kbps)) => kbps.parse().ok().map(|bit_rate| n.wifi = SavedQuality { bit_rate, format: format.to_string() }),
            None if value == "raw" => Some(n.wifi = SavedQuality::default()),
            None => None,
        },
        // The lookups switch covers the lyrics services too, so it takes the lyrics half with it both ways.
        "thirdPartyLookups" => Some((n.third_party_lookups, n.lyrics_online) = (on(value), on(value))),
        // Back to how they come out of the box (the test bridge).
        "lyricsSources" if value.trim().eq_ignore_ascii_case("default") => {
            n.lyrics_order = lyrics_sources::default_order();
            Some(n.lyrics_on = lyrics_sources::default_on())
        }
        // The services asked, in this order, and no others (the test bridge): `lyricsSources lrclib,unison`.
        "lyricsSources" => {
            let on = lyrics_sources::known(&names(value));
            let rest = p.lyrics_order.iter().filter(|s| !on.contains(s)).cloned();
            n.lyrics_order = on.iter().cloned().chain(rest).collect();
            Some(n.lyrics_on = on)
        }
        // One service dropped at a place in the ranking, held by its handle and dragged: `NETEASE:3`.
        "lyricsPlace" => service(value).and_then(|(s, to)| Some(n.lyrics_order = lyrics_sources::placed(p, s, to.parse().ok()?))),
        // One service a place up or down in the ranking (the terminal's keys): `NETEASE:-1`.
        "lyricsMove" => service(value).and_then(|(s, by)| Some(n.lyrics_order = lyrics_sources::moved(p, s, by.parse().ok()?))),
        // One lyrics service switched on or off: `lyricsService:NETEASE`.
        _ if name.starts_with("lyricsService:") => lyrics_sources::LyricsService::named(&name["lyricsService:".len()..]).map(|s| {
            n.lyrics_on.retain(|x| x != s.name());
            if on(value) {
                n.lyrics_on.push(s.name().to_string());
            }
        }),
        // A built-in compressor setting, taken whole, and the compressor on.
        "compressorPreset" => compressor_preset_named(value).map(|c| {
            let e = p.effects().with_compressor_preset(c);
            (n.compressor, n.comp_threshold_db, n.comp_ratio, n.comp_attack_ms, n.comp_release_ms, n.comp_makeup_db, n.comp_knee_db) =
                (e.compressor, e.comp_threshold_db, e.comp_ratio, e.comp_attack_ms, e.comp_release_ms, e.comp_makeup_db, e.comp_knee_db);
        }),
        // One of bs2b's settings, its cutoff and level taken whole, and the crossfeed on; or off, the
        // cutoff kept.
        "crossfeedPreset" if value.trim().eq_ignore_ascii_case("OFF") => Some(n.crossfeed_db = 0.0),
        "crossfeedPreset" => crossfeed_preset_named(value).map(|c| {
            let (cut, level) = c.settings();
            (n.crossfeed_hz, n.crossfeed_db) = (cut as f32, level as f32);
        }),
        // How many graphic bands: the curve drawn again on the new layout.
        // With a headphone correction on it, the new layout is fitted to the correction again.
        "eqLayout" => value.trim().parse::<usize>().ok().filter(|c| nori_player::graphic::LAYOUTS.contains(c)).map(|c| match fit_target(&p.eq_graphic_target, c) {
            Some((sliders, preamp)) => (n.eq_graphic, n.eq_preamp_db) = (sliders, Some(preamp)),
            None => n.eq_graphic = relayout_graphic(&p.eq_graphic, c),
        }),
        // The row says "on mobile data", the setting "Wi-Fi only": the one is the other turned round.
        "motionArtworkMobile" => Some(n.motion_artwork_wifi_only = !on(value)),
        // The active server's own settings: which music folder it browses, and the bitrate cap on its
        // second address.
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

/// The values by name the table cannot say line by line ([`SPECIAL_SPECS`]).
pub(crate) fn value_of_special(p: &StoredPrefs, name: &str) -> Option<String> {
    let server = || p.servers.iter().find(|s| s.id == p.active_server_id);
    Some(match name {
        "motionArtworkMobile" => (!p.motion_artwork_wifi_only).to_string(),
        "compressorPreset" => p.effects().compressor_preset().map_or("", compressor_preset_name).to_string(),
        "eqLayout" => p.eq_graphic.len().to_string(),
        // "" is a crossfeed of the listener's own (the client's "Custom").
        "crossfeedPreset" if p.crossfeed_db <= 0.0 => "OFF".to_string(),
        "crossfeedPreset" => crossfeed_preset(p.crossfeed_hz, p.crossfeed_db).map_or("", crossfeed_preset_name).to_string(),
        "musicFolder" => server().map(|s| s.music_folder_id.clone()).unwrap_or_default(),
        "altMaxBitRate" => server().map_or(0, |s| s.alt_max_bit_rate).to_string(),
        _ => return None,
    })
}

/// What a server is called in lists: its name, or else the host of its address.
pub fn label(name: &str, url: &str) -> String {
    if !name.trim().is_empty() {
        return name.to_string();
    }
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or_default().to_string()
}

// ---- equalizer edits ----

fn with_bands(s: SoundSettings, eq_bands: Vec<SoundBand>) -> SoundSettings {
    SoundSettings { eq_bands, ..s }
}

fn band_of(b: &nori_model::EqBand) -> SoundBand {
    SoundBand { kind: b.kind, freq: b.freq, gain_db: b.gain_db, q: b.q, channel: BandChannel::Both }
}

fn compressor_preset_name(p: nori_player::compressor::CompressorPreset) -> &'static str {
    use nori_player::compressor::CompressorPreset as C;
    match p {
        C::Gentle => "GENTLE",
        C::Balanced => "BALANCED",
        C::Strong => "STRONG",
    }
}

fn crossfeed_preset_name(p: nori_player::dsp::CrossfeedPreset) -> &'static str {
    use nori_player::dsp::CrossfeedPreset as C;
    match p {
        C::Default => "DEFAULT",
        C::ChuMoy => "CHU_MOY",
        C::JanMeier => "JAN_MEIER",
    }
}

fn crossfeed_preset_named(name: &str) -> Option<nori_player::dsp::CrossfeedPreset> {
    nori_player::dsp::CrossfeedPreset::ALL.into_iter().find(|p| crossfeed_preset_name(*p).eq_ignore_ascii_case(name.trim()))
}

/// Which of bs2b's settings the crossfeed is on, if any: none with it off (0 dB) or moved by hand.
pub fn crossfeed_preset(cut_hz: f32, level_db: f32) -> Option<nori_player::dsp::CrossfeedPreset> {
    if level_db <= 0.0 {
        return None;
    }
    nori_player::dsp::CrossfeedPreset::of(cut_hz as f64, level_db as f64)
}

fn compressor_preset_named(name: &str) -> Option<nori_player::compressor::CompressorPreset> {
    nori_player::compressor::CompressorPreset::ALL.into_iter().find(|p| compressor_preset_name(*p).eq_ignore_ascii_case(name.trim()))
}

/// Graphic sliders for another layout (10, 15 or 31 bands) that draw the same curve; the sliders as
/// they are for a count that is not a layout.
pub fn relayout_graphic(sliders: &[f32], count: usize) -> Vec<f32> {
    if !nori_player::graphic::LAYOUTS.contains(&count) {
        return sliders.to_vec();
    }
    let g: Vec<f64> = sliders.iter().map(|v| *v as f64).collect();
    nori_player::graphic::relayout(&g, count).into_iter().map(|v| EQ_RANGES.gain.hold(((v * 10.0).round() / 10.0) as f32)).collect()
}

/// One graphic slider moved, held to the gain range. An index past the end changes nothing.
pub fn set_graphic(s: SoundSettings, index: u32, gain_db: f32) -> SoundSettings {
    let mut g = s.eq_graphic.clone();
    match g.get_mut(index as usize) {
        Some(v) => *v = EQ_RANGES.gain.hold(gain_db),
        None => return s,
    }
    // Moved by hand, the sliders are no longer the headphone correction.
    SoundSettings { eq_graphic: g, eq_graphic_target: Vec::new(), ..s }
}

/// Sliders of a `count`-band layout fitted to a headphone correction's `target`, and the pre-amp that
/// keeps them from boosting; none without a target.
fn fit_target(target: &[f32], count: usize) -> Option<(Vec<f32>, f32)> {
    let t: Vec<f64> = target.iter().map(|v| *v as f64).collect();
    let fit = nori_player::graphic::fit_target(&t, count, EQ_RANGES.gain.max as f64)?;
    Some((fit.sliders.iter().map(|v| *v as f32).collect(), EQ_RANGES.preamp.hold(fit.preamp_db as f32)))
}

/// A headphone correction (AutoEQ's text: a `GraphicEQ:` curve, or filters) as the target the graphic
/// equalizer is fitted to; none when the text has neither.
pub fn correction_target(text: &str) -> Option<Vec<f32>> {
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

/// A built-in curve, switched on, for the equalizer in use. Its pre-amp of 0 means automatic; "Flat"
/// has no bands and gets the ten graphic ones back. On the graphic equalizer the sliders take the
/// curve's response at their centres, and the pre-amp is automatic.
pub fn apply_preset(s: SoundSettings, p: &NamedPreset) -> SoundSettings {
    if s.eq_mode == EqMode::Graphic {
        let bands: Vec<nori_player::dsp::Band> = p.bands.iter().map(nori_player::dsp::Band::from).collect();
        let count = if nori_player::graphic::LAYOUTS.contains(&s.eq_graphic.len()) { s.eq_graphic.len() } else { 10 };
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

/// An AutoEQ "ParametricEQ.txt" / Equalizer APO preset, switched on with its own pre-amp. A file with no
/// filters in it is refused ([`SoundError::NoFilters`]).
pub fn import(s: SoundSettings, text: &str) -> Result<SoundSettings, SoundError> {
    // On the graphic equalizer a headphone correction sets its sliders: fitted, in the layout in use, so
    // what it plays follows the correction's curve.
    if s.eq_mode == EqMode::Graphic {
        let target = correction_target(text).ok_or(SoundError::NoFilters)?;
        let count = if nori_player::graphic::LAYOUTS.contains(&s.eq_graphic.len()) { s.eq_graphic.len() } else { 10 };
        let (eq_graphic, preamp) = fit_target(&target, count).ok_or(SoundError::NoFilters)?;
        return Ok(SoundSettings { eq_enabled: true, eq_graphic, eq_graphic_target: target, eq_preamp_db: Some(preamp), ..s });
    }
    let preset = parse_eq_preset(text.to_string());
    if preset.bands.is_empty() {
        return Err(SoundError::NoFilters);
    }
    // A headphone correction is filters: it goes to the parametric equalizer, which then plays.
    Ok(SoundSettings { eq_enabled: true, eq_mode: EqMode::Parametric, eq_preamp_db: Some(preset.preamp_db), eq_bands: preset.bands.iter().map(band_of).collect(), ..s })
}

/// A new band: a neutral peak in the middle of the range.
pub fn add_band(s: SoundSettings) -> SoundSettings {
    let mut bands = s.eq_bands.clone();
    bands.push(SoundBand { kind: EqKind::Peaking, freq: 1000.0, gain_db: 0.0, q: 1.0, channel: BandChannel::Both });
    with_bands(s, bands)
}

/// Removing the last band gives the ten graphic ones back rather than an empty equalizer.
pub fn remove_band(s: SoundSettings, index: u32) -> SoundSettings {
    let bands: Vec<SoundBand> = s.eq_bands.iter().enumerate().filter(|(i, _)| *i != index as usize).map(|(_, b)| *b).collect();
    with_bands(s, if bands.is_empty() { graphic() } else { bands })
}

/// How far each equalizer control goes. The screen's sliders span exactly this, and every edit made
/// through the core is held inside it, so a value from anywhere else cannot leave the range either.
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
    /// The equalizer's own pre-amp when it is not automatic, dB.
    pub preamp: Span,
    /// -1 hard left, +1 hard right.
    pub balance: Span,
    /// The limiter's ceiling, dB.
    pub limiter: Span,
    /// Crossfeed, dB; 0 is off.
    pub crossfeed: Span,
    /// The crossfeed's cutoff, Hz.
    pub crossfeed_cut: Span,
    /// A band's width (or a shelf's slope).
    pub q: Span,
    /// A band's frequency, Hz: the frequency slider's 20 Hz to 20 kHz.
    pub freq: Span,
    /// ReplayGain's overall level, dB.
    pub replay_gain_preamp: Span,
}

pub const EQ_RANGES: EqRanges = EqRanges {
    gain: Span { min: -12.0, max: 12.0 },
    preamp: Span { min: -20.0, max: 6.0 },
    balance: Span { min: -1.0, max: 1.0 },
    limiter: Span { min: -12.0, max: 0.0 },
    // Up to 12 dB, so Jan Meier's 9.5 fits (bs2b itself goes to 15, which is barely stereo any more).
    crossfeed: Span { min: 0.0, max: 12.0 },
    crossfeed_cut: Span { min: nori_player::dsp::CROSSFEED_CUT_HZ.0 as f32, max: nori_player::dsp::CROSSFEED_CUT_HZ.1 as f32 },
    q: Span { min: 0.2, max: 8.0 },
    freq: Span { min: 20.0, max: 20_000.0 },
    replay_gain_preamp: Span { min: REPLAY_GAIN_PREAMP.0, max: REPLAY_GAIN_PREAMP.1 },
};

/// One kind of band (`EqKind`, by its ordinal) as the editor needs it: whether it has a gain to set, and
/// whether its width is a slope. The client names each kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BandKindInfo {
    pub uses_gain: bool,
    /// A shelf given by its slope rather than a Q.
    pub slope: bool,
}

/// What the equalizer editor needs of the core besides the bands: each band kind's facts, in `EqKind`'s
/// order, and the sliders' ranges; asked once. Every word on the screen is the client's.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct EqModel {
    pub band_kinds: Vec<BandKindInfo>,
    pub eq_ranges: EqRanges,
}

pub fn eq_model() -> EqModel {
    EqModel {
        band_kinds: EqKind::ALL
            .iter()
            .map(|k| BandKindInfo { uses_gain: nori_player::dsp::uses_gain(*k as i32), slope: matches!(k, EqKind::LowShelfSlope | EqKind::HighShelfSlope) })
            .collect(),
        eq_ranges: EQ_RANGES,
    }
}

/// A band as the edit leaves it: a kind and a channel that exist, and gain, width and frequency held
/// inside [`EQ_RANGES`].
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

/// One band changed. An index past the end changes nothing.
pub fn set_band(s: SoundSettings, index: u32, band: SoundBand) -> SoundSettings {
    let mut bands = s.eq_bands.clone();
    match bands.get_mut(index as usize) {
        Some(b) => *b = held(band),
        None => return s,
    }
    with_bands(s, bands)
}

impl SoundSettings {
    /// The pre-amp in effect: the one set, or the automatic one for the equalizer in use; none with the
    /// equalizer off.
    pub fn effective_preamp_db(&self) -> f32 {
        if self.eq_mode == EqMode::Graphic {
            return effective_preamp_db(self.eq_enabled, self.eq_preamp_db, self.eq_graphic.iter().map(|g| (nori_player::dsp::PEAKING, *g)));
        }
        effective_preamp_db(self.eq_enabled, self.eq_preamp_db, self.eq_bands.iter().map(|b| (b.kind as i32, b.gain_db)))
    }
}

/// [`SoundSettings::effective_preamp_db`] from its parts: whether the equalizer is on, the pre-amp set
/// (none for automatic) and each band's kind and gain.
pub fn effective_preamp_db(eq_enabled: bool, eq_preamp_db: Option<f32>, bands: impl IntoIterator<Item = (i32, f32)>) -> f32 {
    if !eq_enabled {
        return 0.0;
    }
    eq_preamp_db.unwrap_or_else(|| nori_player::dsp::auto_preamp_db(bands))
}

/// The automatic pre-amp switched on, or off - and then it starts from the level it was at, so the
/// sound does not jump when the switch is flipped.
pub fn set_auto_preamp(s: SoundSettings, automatic: bool) -> SoundSettings {
    let eq_preamp_db = if automatic { None } else { Some(EQ_RANGES.preamp.hold(s.effective_preamp_db())) };
    SoundSettings { eq_preamp_db, ..s }
}

/// One of the equalizer screen's other controls, or the settings' "Overall level" slider: a level a
/// slider drags, edited in place on every step (`settings_store::edit_level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum EqLevel {
    Preamp,
    Balance,
    Limiter,
    Crossfeed,
    /// The level ReplayGain plays at (`preamp_db`), not the equalizer's pre-amp.
    ReplayGainPreamp,
    // The effects (the sound settings page's sliders).
    BassBoost,
    Virtualizer,
    VolumeBoost,
    CompThreshold,
    CompRatio,
    CompAttack,
    CompRelease,
    CompMakeup,
    CompKnee,
    /// The crossfeed's cutoff, Hz.
    CrossfeedCut,
}

impl EqLevel {
    /// Every level, in order: the ordinal a platform's door carries.
    pub const ALL: [EqLevel; 15] = [
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
    ];

    /// The level's value in these settings.
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
        }
    }
}

/// An effect's slider near its bottom is off: a boost under a quarter of a dB, a virtualizer under 2 %.
fn off_below(v: f32, least: f32) -> f32 {
    if v < least { 0.0 } else { v }
}

/// A balance near the middle is the middle: within 4 % of it the slider snaps to 0.
pub fn balance_snap(v: f32) -> f32 {
    if v.abs() < 0.04 { 0.0 } else { v }
}

/// Crossfeed under a decibel is none at all.
pub fn crossfeed_snap(db: f32) -> f32 {
    if db < 1.0 { 0.0 } else { db }
}

/// A level moved on the equalizer screen, held in its range; balance near the middle and crossfeed
/// under a decibel snap to none ([`balance_snap`], [`crossfeed_snap`]).
pub fn set_level(s: SoundSettings, level: EqLevel, value: f32) -> SoundSettings {
    let r = EQ_RANGES;
    match level {
        EqLevel::Preamp => SoundSettings { eq_preamp_db: Some(r.preamp.hold(value)), ..s },
        EqLevel::Balance => SoundSettings { balance: balance_snap(r.balance.hold(value)), ..s },
        EqLevel::Limiter => SoundSettings { limiter_threshold_db: r.limiter.hold(value), ..s },
        EqLevel::Crossfeed => SoundSettings { crossfeed_db: crossfeed_snap(r.crossfeed.hold(value)), ..s },
        EqLevel::ReplayGainPreamp => SoundSettings { preamp_db: r.replay_gain_preamp.hold(value), ..s },
        // Whole hertz: a slider's step is a few of them, and a preset is recognised to the hertz.
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
                _ => {}
            }
            SoundSettings { effects: e, ..s }
        }
    }
}

/// Why nothing on the equalizer screen reaches the sound; the client says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[repr(u8)]
pub enum EqBypass {
    /// Bit-perfect USB output is active.
    BitPerfect,
    /// High quality output is on (and can be turned off in the settings).
    HiRes,
}

/// Why nothing on the equalizer screen reaches the sound, or `None` when it does. Bit-perfect output
/// and high quality output both hand the file's samples to the DAC untouched, so the whole chain is
/// out of the path; without this the screen looks broken.
pub fn eq_bypass(hi_res: bool, bit_perfect: bool) -> Option<EqBypass> {
    if bit_perfect {
        Some(EqBypass::BitPerfect)
    } else if hi_res {
        Some(EqBypass::HiRes)
    } else {
        None
    }
}

/// What a band's label marks after its frequency (the client draws it: "1k L", "63 ↙").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum BandMark {
    None,
    /// One channel only.
    Left,
    Right,
    LowShelf,
    HighShelf,
    /// A band with no gain (a notch, a pass).
    NoGain,
}

/// A band's mark: its channel, a shelf, or no gain, in that order of precedence.
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

// ---- server profiles ----

/// The saved servers and which one is in use.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ServerList {
    pub servers: Vec<SavedServer>,
    pub active_server_id: String,
}

/// `profile` made the server in use: it replaces the saved one with its id, or joins the end.
pub fn servers_activate(list: ServerList, profile: SavedServer) -> ServerList {
    let id = profile.id.clone();
    let mut servers: Vec<SavedServer> = list.servers.into_iter().filter(|s| s.id != id).collect();
    servers.push(profile);
    ServerList { servers, active_server_id: id }
}

/// The profile a login keeps: the form as filled in, under the id of a saved profile for the same address
/// and user when there is one, so that logging in to a server again (the add-server form, the test bridge)
/// takes up that profile, its library and downloads with it, instead of saving a copy with none. The saved
/// one keeps what the form does not ask (the music folder, the second address's bitrate cap) and its name
/// when the form's is empty. A profile already saved (edited in place) is left as it is.
pub fn servers_login(list: &ServerList, profile: SavedServer) -> SavedServer {
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

/// A saved profile changed in place; which one is in use does not change.
pub fn servers_update(list: ServerList, profile: SavedServer) -> ServerList {
    let servers = list.servers.into_iter().map(|s| if s.id == profile.id { profile.clone() } else { s }).collect();
    ServerList { servers, ..list }
}

/// A profile removed. Removing the one in use puts the first one left in its place, or none.
pub fn servers_remove(list: ServerList, id: &str) -> ServerList {
    let was_active = list.active_server_id == id;
    let servers: Vec<SavedServer> = list.servers.into_iter().filter(|s| s.id != id).collect();
    let active_server_id = if was_active { servers.first().map(|s| s.id.clone()).unwrap_or_default() } else { list.active_server_id };
    ServerList { servers, active_server_id }
}

/// Whose rows in the app's database are open: the active profile's, and "default" before there is one.
pub fn server_db_id(active_server_id: &str) -> String {
    if active_server_id.is_empty() { "default".into() } else { active_server_id.to_string() }
}

/// A fresh profile id: eight hex digits, like the start of a random UUID.
pub fn new_server_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(N.fetch_add(1, Ordering::Relaxed));
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    format!("{:08x}", h.finish() as u32)
}

/// Extra HTTP headers as typed, one per line, "Name: value". A line without a colon or a name is
/// skipped; name and value are trimmed; a name given twice keeps its last value.
pub fn parse_headers(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            (!k.trim().is_empty()).then(|| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// The headers back as the field shows them, by name so the order is always the same.
pub fn format_headers(headers: &HashMap<String, String>) -> String {
    let mut h: Vec<(&String, &String)> = headers.iter().collect();
    h.sort();
    h.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n")
}

/// An address typed without its scheme: the schemes offered in front of it, https first.
pub fn url_schemes(url: &str) -> Vec<String> {
    if url.is_empty() || url.contains("://") { Vec::new() } else { vec!["https://".into(), "http://".into()] }
}

/// Whether the form can be sent: an address, and a user or an API key.
pub fn profile_ready(p: &SavedServer) -> bool {
    !p.url.trim().is_empty() && (!p.user.trim().is_empty() || !p.api_key.trim().is_empty())
}

/// The profile as the form sends it: addresses trimmed, headers read from what was typed.
pub fn profile_from_form(p: SavedServer, headers: &str) -> SavedServer {
    SavedServer { url: p.url.trim().to_string(), alt_url: p.alt_url.trim().to_string(), headers: parse_headers(headers), ..p }
}

// ---- the doors ----

/// The band kinds' facts and the equalizer's ranges; asked once.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn eq_model_get() -> EqModel {
    eq_model()
}

/// Which crossfeed preset these are, by the name `crossfeedPreset` takes ("OFF", "DEFAULT", "CHU_MOY",
/// "JAN_MEIER"), or "" for the listener's own; asked once per change, for the equalizer screen's chips.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn crossfeed_preset_of(prefs: StoredPrefs) -> String {
    value_of_special(&prefs, "crossfeedPreset").unwrap_or_default()
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn eq_bypass_reason(hi_res: bool, bit_perfect: bool) -> Option<EqBypass> {
    eq_bypass(hi_res, bit_perfect)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn servers_activated(list: ServerList, profile: SavedServer) -> ServerList {
    servers_activate(list, profile)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_for_login(list: ServerList, profile: SavedServer) -> SavedServer {
    servers_login(&list, profile)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn servers_updated(list: ServerList, profile: SavedServer) -> ServerList {
    servers_update(list, profile)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn servers_removed(list: ServerList, id: String) -> ServerList {
    servers_remove(list, &id)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_db(active_server_id: String) -> String {
    server_db_id(&active_server_id)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_new_id() -> String {
    new_server_id()
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_headers_text(headers: HashMap<String, String>) -> String {
    format_headers(&headers)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_url_schemes(url: String) -> Vec<String> {
    url_schemes(&url)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_ready(profile: SavedServer) -> bool {
    profile_ready(&profile)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_from_form(profile: SavedServer, headers: String) -> SavedServer {
    profile_from_form(profile, &headers)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn server_label(name: String, url: String) -> String {
    label(&name, &url)
}

/// The part of the settings a sound profile remembers ([`StoredPrefs::sound`]).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn prefs_sound(prefs: StoredPrefs) -> SoundSettings {
    prefs.sound()
}

/// The settings with the part a sound profile remembers taken from `sound` ([`StoredPrefs::with_sound`]).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn prefs_with_sound(prefs: StoredPrefs, sound: SoundSettings) -> StoredPrefs {
    prefs.with_sound(sound)
}

/// A saved profile's sound; `None` when the JSON is not one.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn sound_from_json(json: String) -> Option<SoundSettings> {
    sound_from(&json)
}

/// The equalizer in use back to flat, with the automatic pre-amp: the ten graphic bands for the
/// parametric one, every slider at 0 (in the same layout) for the graphic one.
pub fn eq_reset_bands(sound: SoundSettings) -> SoundSettings {
    match sound.eq_mode {
        EqMode::Graphic => SoundSettings { eq_graphic: vec![0.0; sound.eq_graphic.len().max(1)], eq_graphic_target: Vec::new(), eq_preamp_db: None, ..sound },
        EqMode::Parametric => SoundSettings { eq_bands: graphic(), eq_preamp_db: None, ..sound },
    }
}

/// Which of the app's own files are the app's database: `nori.db` with its write-ahead log and shared
/// memory. Indices into `names`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn storage_index_files(names: Vec<String>) -> Vec<u32> {
    names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.strip_prefix(nori_db::DB_FILE).is_some_and(|rest| ["", "-wal", "-shm"].contains(&rest)))
        .map(|(i, _)| i as u32)
        .collect()
}

/// Reads an AutoEQ "ParametricEQ.txt" / Equalizer APO preset:
/// `Preamp: -6.2 dB` and `Filter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70` lines; anything else is ignored.
/// A file with no filters but a `GraphicEQ:` curve (AutoEQ's "GraphicEQ.txt", Wavelet's) is fitted here,
/// once, with ten parametric filters (`nori_player::eqfit`), so it plays as any parametric preset does.
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
                "LSC 6DB" | "LS 6DB" | "LS6" => EqKind::LowShelfSlope,
                "HSC 6DB" | "HS 6DB" | "HS6" => EqKind::HighShelfSlope,
                "LP" | "LPQ" => EqKind::LowPass,
                "HP" | "HPQ" => EqKind::HighPass,
                "BP" => EqKind::BandPass,
                "NO" | "NOTCH" => EqKind::Notch,
                "AP" => EqKind::AllPass,
                _ => continue,
            };
            // Only the shelving and peaking kinds carry a gain; a pass filter line has none.
            let Some(freq) = after("Fc") else { continue };
            let gain_db = after("Gain").unwrap_or(0.0);
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
    fn each_setting_is_stored_under_its_own_key_and_read_by_its_own_name() {
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
    fn nothing_stored_is_the_defaults() {
        let p = load(&HashMap::new());
        assert_eq!(p, StoredPrefs::default());
        assert_eq!(p.eq_bands.len(), 10);
        assert_eq!(p.home_rows, HomeRow::ALL);
        assert_eq!(p.swipe_left, SwipeAction::Favourite, "the left swipe favourites");
    }

    #[test]
    fn what_is_saved_loads_back_the_same() {
        let mut p = StoredPrefs::default();
        p.servers = vec![SavedServer {
            id: "a1".into(),
            name: "Home".into(),
            url: "https://music.example.com".into(),
            headers: [("X-Auth".to_string(), "t".to_string())].into(),
            alt_max_bit_rate: 320,
            legacy_auth: true,
            ..SavedServer::default()
        }];
        p.active_server_id = "a1".into();
        p.eq_preamp_db = Some(-3.5);
        p.eq_bands = vec![band_from(2, 1234.5, -2.25, 0.7, 1)];
        p.home_rows = vec![HomeRow::TopSongs, HomeRow::Pinned];
        p.pinned_playlists = vec!["p1".into(), "p2".into()];
        p.list_prefs = [("albums".to_string(), "grid".to_string())].into();
        p.accent = 0xFF112233;
        p.theme = ThemeMode::Dark;
        assert_eq!(load(&save(&p)), p);
        assert!(!save(&StoredPrefs { eq_preamp_db: None, ..p }).contains_key("eqPreampDb"));
    }

    #[test]
    fn values_out_of_range_are_brought_back() {
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
    fn the_server_list_reads_whole_or_not_at_all() {
        let p = load(&raw(&[("servers", t(r#"[{"id":"a","legacyAuth":true,"altMaxBitRate":128}]"#)), ("activeServerId", t("a"))]));
        assert_eq!((p.servers.len(), p.active_server_id.as_str()), (1, "a"));
        assert!(p.servers[0].legacy_auth);
        assert_eq!(p.servers[0].alt_max_bit_rate, 128);
        // A list that does not read, or a server without an id, loses the whole list.
        assert!(load(&raw(&[("servers", t(r#"[{"id":"a"},{"name":"x"}]"#))])).servers.is_empty());
        assert!(load(&raw(&[("servers", t("nope"))])).servers.is_empty());
    }

    #[test]
    fn home_rows_pins_and_list_prefs() {
        let p = load(&raw(&[("homeRows", t("RANDOM,NOPE,PINNED")), ("pinnedPlaylists", t("a\n\nb")), ("listPrefs", t(r#"{"x":"1","y":2}"#))]));
        assert_eq!(p.home_rows, [HomeRow::Random, HomeRow::Pinned]);
        assert_eq!(p.pinned_playlists, ["a", "b"]);
        assert_eq!(p.list_prefs.get("y").map(String::as_str), Some("2"));
        assert!(load(&raw(&[("homeRows", t(""))])).home_rows.is_empty(), "every row hidden");
        assert!(load(&raw(&[("listPrefs", t("{"))])).list_prefs.is_empty());
    }

    #[test]
    fn bands_keep_their_wire_format() {
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
    fn a_sound_round_trips_through_its_json() {
        let s = SoundSettings {
            eq_enabled: true,
            eq_bands: vec![band_from(1, 105.0, -3.5, 0.7, 0)],
            eq_mode: EqMode::Graphic,
            eq_graphic: (0..15).map(|i| i as f32 - 7.5).collect(),
            eq_graphic_target: (0..96).map(|i| (i as f32 * 0.37).sin() * 4.0).collect(),
            eq_preamp_db: Some(-6.2),
            crossfeed_db: 3.0,
            crossfeed_hz: 820.0,
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
            },
            replay_gain: GainMode::Album,
            preamp_db: 1.5,
            crossfade_sec: 4,
            hi_res: true,
            bit_perfect: false,
        };
        assert_eq!(sound_from(&sound_json(&s)).unwrap(), s);
        assert_eq!(sound_from(&sound_json(&SoundSettings { eq_preamp_db: None, ..s.clone() })).unwrap().eq_preamp_db, None);
    }

    #[test]
    fn a_sound_takes_defaults_for_what_is_missing() {
        let s = sound_from("{}").unwrap();
        assert_eq!(s.eq_bands, graphic());
        assert_eq!(s.limiter_threshold_db, -1.0);
        assert_eq!(s.eq_preamp_db, None);
        assert_eq!(sound_from(r#"{"replayGain":7}"#).unwrap().replay_gain, GainMode::Auto);
        assert_eq!(sound_from(r#"{"replayGain":-1}"#).unwrap().replay_gain, GainMode::Off);
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
    fn the_test_bridge_sets_within_the_same_ranges() {
        let p = StoredPrefs::default();
        assert!(set_by_name(&p, "limiter", "TRUE").unwrap().prefs.limiter);
        assert_eq!(set_by_name(&p, "eqPreampDb", "12").unwrap().prefs.eq_preamp_db, Some(6.0), "held to the equalizer's range");
        assert_eq!(set_by_name(&StoredPrefs { eq_preamp_db: Some(2.0), ..p.clone() }, "eqPreampDb", "auto").unwrap().prefs.eq_preamp_db, None);
        assert!(set_by_name(&p, "mono", "1").unwrap().prefs.mono);
        assert!(!set_by_name(&StoredPrefs { mono: true, ..p.clone() }, "mono", "yes").unwrap().prefs.mono);
        assert_eq!(set_by_name(&p, "parallelDownloads", "99").unwrap().prefs.parallel_downloads, 10);
        assert_eq!(set_by_name(&p, "coversAhead", "x").unwrap().prefs.covers_ahead, 3, "unreadable keeps the value");
        let cache = set_by_name(&p, "cacheMb", "10").unwrap();
        assert_eq!(cache.prefs.cache_mb, 256);
        assert!(cache.apply_cache_limit);
        assert!(!set_by_name(&p, "speed", "9").unwrap().apply_cache_limit);
        assert_eq!(set_by_name(&p, "speed", "9").unwrap().prefs.speed, 4.0);
        assert_eq!(set_by_name(&p, "fadeMs", "-5").unwrap().prefs.fade_ms, 0);
        assert_eq!(set_by_name(&p, "autoFillKind", "albums").unwrap().prefs.auto_fill_kind, AutoFillKind::Albums);
        assert_eq!(set_by_name(&p, "autoFillBasis", "era").unwrap().prefs.auto_fill_basis, AutoFillBasis::Era);
        assert_eq!(set_by_name(&p, "autoFillBasis", "mood"), None);
        assert_eq!(set_by_name(&p, "nope", "1"), None);
    }

    #[test]
    fn every_row_of_the_settings_screen_sets_by_name() {
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
    fn a_new_install_finds_lyrics_and_keeps_the_autoeq_list_but_no_moving_covers() {
        let fresh = load(&HashMap::new());
        assert!(fresh.third_party_lookups && fresh.lyrics_online && fresh.auto_eq_download);
        assert!(!fresh.motion_artwork, "moving covers are heavier and stay off");
        let asked: Vec<&str> = crate::lyrics_sources::lyrics_lookup(&fresh).services.iter().map(|s| s.name()).collect();
        let keyless: Vec<String> = crate::lyrics_sources::default_order().into_iter().filter(|n| crate::lyrics_sources::LyricsService::named(n).is_some_and(|s| s.needs().is_none())).collect();
        assert_eq!(asked, keyless, "every service on; the ones that need a key wait for it");
        assert_eq!(fresh, StoredPrefs::default());
        // An install that stored the lookups off keeps them off: no migration.
        let kept = load(&save(&StoredPrefs { third_party_lookups: false, auto_eq_download: false, ..StoredPrefs::default() }));
        assert!(!kept.third_party_lookups && !kept.auto_eq_download);
        let p = StoredPrefs { third_party_lookups: false, auto_eq_download: false, ..StoredPrefs::default() };
        let on = set_by_name(&p, "autoEqDownload", "true").unwrap().prefs;
        assert!(on.auto_eq_download && on.third_party_lookups, "the AutoEQ list switches lookups on");
        assert!(!set_by_name(&on, "autoEqDownload", "false").unwrap().prefs.auto_eq_download);
    }

    #[test]
    fn the_lyrics_lookup_and_the_lookups_switch_go_together() {
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
    fn lyrics_services_are_switched_ranked_and_kept() {
        let p = StoredPrefs::default();
        let all = p.lyrics_on.len();
        let off = set_by_name(&p, "lyricsService:portato", "false").unwrap().prefs;
        assert!(!off.lyrics_on.iter().any(|n| n == "PORTATO") && off.lyrics_on.len() == all - 1);
        let on = set_by_name(&off, "lyricsService:portato", "true").unwrap().prefs;
        assert!(on.lyrics_on.iter().any(|n| n == "PORTATO") && on.lyrics_on.len() == all);
        assert_eq!(on.lyrics_order, p.lyrics_order, "a switch never moves a service");
        let off = set_by_name(&on, "lyricsService:paxsenix", "false").unwrap().prefs;
        assert_eq!((off.lyrics_order.clone(), off.lyrics_on.len()), (p.lyrics_order.clone(), all - 1));
        assert!(set_by_name(&p, "lyricsService:nobody", "true").is_none(), "no such service");
        let at = |o: &[String], n: &str| o.iter().position(|x| x == n).unwrap();
        let moved = set_by_name(&on, "lyricsMove", "LRCLIB:-1").unwrap().prefs;
        assert_eq!(at(&moved.lyrics_order, "LRCLIB"), at(&on.lyrics_order, "LRCLIB") - 1, "one place, whoever is above it");
        let placed = set_by_name(&on, "lyricsPlace", "LRCLIB:0").unwrap().prefs;
        assert_eq!(crate::lyrics_sources::switched_on(&placed)[0].name(), "LRCLIB", "dropped first, asked first");
        assert_eq!(placed.lyrics_on, on.lyrics_on);
        assert!(set_by_name(&on, "lyricsPlace", "LRCLIB:x").is_none());
        let only = set_by_name(&p, "lyricsSources", "lrclib, kugou").unwrap().prefs;
        assert_eq!(only.lyrics_on, ["LRCLIB", "KUGOU"]);
        assert_eq!(only.lyrics_order[..2], ["LRCLIB", "KUGOU"]);
        let back = set_by_name(&only, "lyricsSources", "default").unwrap().prefs;
        assert_eq!((back.lyrics_on, back.lyrics_order), (p.lyrics_on.clone(), p.lyrics_order.clone()));
        let keyed = set_by_name(&placed, "paxSenixKey", "  k  ").unwrap().prefs;
        let back = load(&save(&keyed));
        assert_eq!((back.lyrics_on, back.lyrics_order, back.paxsenix_key), (keyed.lyrics_on.clone(), keyed.lyrics_order.clone(), "k".to_string()));
        assert_eq!(load(&HashMap::new()).lyrics_on, crate::lyrics_sources::default_order(), "nothing stored: the defaults, every service");
        assert_eq!(load(&HashMap::new()).lyrics_order, crate::lyrics_sources::default_order());
    }

    #[test]
    fn the_active_servers_own_settings() {
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
    fn the_band_kinds_facts() {
        let kinds: Vec<(bool, bool)> = eq_model().band_kinds.iter().map(|k| (k.uses_gain, k.slope)).collect();
        assert_eq!(
            kinds,
            [(true, false), (true, false), (true, false), (false, false), (false, false), (false, false), (false, false), (false, false), (true, true), (true, true)]
        );
    }

    #[test]
    fn a_band_edit_stays_in_range() {
        let s = sound();
        let b = set_band(s.clone(), 3, band_from(42, 5.0, 30.0, 0.0, 7));
        assert_eq!(b.eq_bands[3], band_from(0, 20.0, 12.0, 0.2, 0));
        let ok = band_from(1, 120.0, -3.5, 0.7, 2);
        assert_eq!(set_band(s.clone(), 0, ok).eq_bands[0], ok);
        assert_eq!(set_band(s.clone(), 99, ok), s, "no such band");
    }

    #[test]
    fn switching_the_automatic_pre_amp_off_keeps_the_level() {
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
    fn levels_snap_and_hold() {
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
    fn why_the_equalizer_does_nothing() {
        assert_eq!(eq_bypass(false, false), None);
        assert_eq!(eq_bypass(true, true), Some(EqBypass::BitPerfect));
        assert_eq!(eq_bypass(true, false), Some(EqBypass::HiRes));
    }

    #[test]
    fn band_marks_and_snaps() {
        assert_eq!(band_mark(0, 0), BandMark::None);
        assert_eq!(band_mark(1, 1), BandMark::Left);
        assert_eq!(band_mark(8, 0), BandMark::LowShelf);
        assert_eq!(band_mark(2, 0), BandMark::HighShelf);
        assert_eq!(band_mark(9, 2), BandMark::Right);
        assert_eq!(band_mark(6, 0), BandMark::NoGain);
        assert_eq!((balance_snap(0.03), crossfeed_snap(0.9), crossfeed_snap(2.0)), (0.0, 0.0, 2.0));
    }

    #[test]
    fn server_list_edits() {
        let s = |id: &str, name: &str| SavedServer { id: id.into(), name: name.into(), ..SavedServer::default() };
        let list = ServerList { servers: vec![s("a", "A"), s("b", "B")], active_server_id: "b".into() };
        let l = servers_activate(list.clone(), s("a", "A2"));
        assert_eq!(l.servers.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["B", "A2"], "replaced, and moved to the end");
        assert_eq!(l.active_server_id, "a");
        let l = servers_update(list.clone(), s("a", "A3"));
        assert_eq!((l.servers[0].name.as_str(), l.active_server_id.as_str()), ("A3", "b"));
        assert_eq!(servers_update(list.clone(), s("z", "Z")).servers.len(), 2, "an unknown profile is not added");
        let l = servers_remove(list.clone(), "b");
        assert_eq!((l.servers.len(), l.active_server_id.as_str()), (1, "a"), "the first one left takes over");
        assert_eq!(servers_remove(list.clone(), "a").active_server_id, "b");
        assert_eq!(servers_remove(ServerList { servers: vec![s("a", "")], active_server_id: "a".into() }, "a").active_server_id, "");
        assert_eq!((server_db_id(""), server_db_id("x1")), ("default".into(), "x1".into()));
        let id = new_server_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_server_id(), new_server_id());
    }

    #[test]
    fn a_login_to_a_server_already_saved_takes_up_its_profile() {
        let saved = SavedServer { id: "a1".into(), name: "Home".into(), url: "http://10.0.2.2:4534/".into(), user: "admin".into(), password: "old".into(), music_folder_id: "3".into(), ..SavedServer::default() };
        let other = SavedServer { id: "b2".into(), url: "http://10.0.2.2:4534".into(), user: "guest".into(), ..SavedServer::default() };
        let list = ServerList { servers: vec![saved.clone(), other], active_server_id: "b2".into() };
        let form = SavedServer { id: new_server_id(), url: " HTTP://10.0.2.2:4534 ".into(), user: "Admin".into(), password: "new".into(), ..SavedServer::default() };
        let kept = servers_login(&list, form.clone());
        assert_eq!((kept.id.as_str(), kept.password.as_str(), kept.name.as_str(), kept.music_folder_id.as_str()), ("a1", "new", "Home", "3"), "{kept:?}");
        let l = servers_activate(list.clone(), kept);
        assert_eq!((l.servers.len(), l.active_server_id.as_str()), (2, "a1"), "activated, not copied");
        // Another user of the same server, or another server, is a profile of its own.
        let stranger = SavedServer { user: "someone".into(), ..form.clone() };
        assert_eq!(servers_login(&list, stranger.clone()), stranger);
        let elsewhere = SavedServer { url: "https://music.example".into(), ..form.clone() };
        assert_eq!(servers_login(&list, elsewhere.clone()), elsewhere);
        // A saved profile edited keeps its own id, whatever the others are.
        let edited = SavedServer { id: "b2".into(), ..form };
        assert_eq!(servers_login(&list, edited.clone()), edited);
    }

    #[test]
    fn the_login_form() {
        let h = parse_headers("X-Auth: a:b\n: nope\nno colon\n  CF-Id :  x  \nX-Auth: c");
        assert_eq!(h.len(), 2);
        assert_eq!(h["X-Auth"], "c", "the last one wins");
        assert_eq!(h["CF-Id"], "x");
        assert_eq!(format_headers(&parse_headers("b: 2\na: 1")), "a: 1\nb: 2");
        assert_eq!(url_schemes("music.local"), ["https://", "http://"]);
        assert!(url_schemes("").is_empty() && url_schemes("http://x").is_empty());
        let p = SavedServer { url: " https://x ".into(), alt_url: " y ".into(), ..SavedServer::default() };
        assert!(!profile_ready(&p), "a user or a key");
        assert!(profile_ready(&SavedServer { user: "u".into(), ..p.clone() }));
        assert!(profile_ready(&SavedServer { api_key: "k".into(), ..p.clone() }));
        assert!(!profile_ready(&SavedServer { url: "  ".into(), user: "u".into(), ..p.clone() }));
        let f = profile_from_form(p, "A: 1");
        assert_eq!((f.url.as_str(), f.alt_url.as_str(), f.headers["A"].as_str()), ("https://x", "y", "1"));
    }

    fn sound() -> SoundSettings {
        sound_from("{}").unwrap()
    }

    #[test]
    fn equalizer_edits() {
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
    fn the_graphic_equalizer_keeps_its_own_sliders() {
        let g = SoundSettings { eq_mode: EqMode::Graphic, ..sound() };
        assert_eq!(g.eq_graphic, vec![0.0; 10], "ten flat sliders out of the box");
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
    fn a_new_install_gets_the_graphic_equalizer_and_an_old_one_keeps_what_it_set_up() {
        assert_eq!(load(&HashMap::new()).eq_mode, EqMode::Graphic, "a new install");
        assert_eq!(StoredPrefs::default().eq_mode, EqMode::Graphic);
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
    fn a_headphone_correction_sets_the_graphic_sliders() {
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
    fn effect_levels_are_held_and_snap_off() {
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
    fn effects_by_name_and_the_compressor_presets() {
        let p = StoredPrefs::default();
        assert!(!p.sound_chain_on() && !p.effects().on(), "every effect off out of the box");
        assert_eq!(p.effects().compressor_preset(), Some(nori_player::compressor::CompressorPreset::Balanced), "the defaults are the balanced preset");
        let c = set_by_name(&p, "compressorPreset", "strong").unwrap().prefs;
        assert!(c.compressor && c.comp_ratio == 5.0 && c.sound_chain_on());
        assert_eq!(value_of_special(&c, "compressorPreset").as_deref(), Some("STRONG"));
        let custom = set_by_name(&c, "compRatio", "7").unwrap().prefs;
        assert_eq!(value_of_special(&custom, "compressorPreset").as_deref(), Some(""), "moved: none of them");
        assert!(set_by_name(&p, "compressorPreset", "loud").is_none());
        assert_eq!(set_by_name(&p, "volumeBoostDb", "40").unwrap().prefs.volume_boost_db, 12.0);
        assert!(set_by_name(&p, "virtualizer", "0.5").unwrap().prefs.sound_chain_on());
        let l = set_by_name(&p, "eqLayout", "31").unwrap().prefs;
        assert_eq!(l.eq_graphic.len(), 31);
        assert_eq!(value_of_special(&l, "eqLayout").as_deref(), Some("31"));
        assert!(set_by_name(&p, "eqLayout", "12").is_none());
    }

    #[test]
    fn the_crossfeed_presets_by_name() {
        let p = StoredPrefs::default();
        assert_eq!((p.crossfeed_db, p.crossfeed_hz), (0.0, 700.0), "off, at bs2b's cutoff");
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
    fn importing_a_preset() {
        let s = import(sound(), "Preamp: -6.2 dB\nFilter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70\n").unwrap();
        assert!(s.eq_enabled);
        assert_eq!(s.eq_preamp_db, Some(-6.2));
        assert_eq!(s.eq_bands.len(), 1);
        assert!(matches!(import(sound(), "Preamp: 0 dB\n"), Err(SoundError::NoFilters)));
        let zero = import(sound(), "Filter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70\n").unwrap();
        assert_eq!(zero.eq_preamp_db, Some(0.0), "an imported pre-amp is kept even at 0");
    }

    #[test]
    fn index_files_are_the_databases() {
        let names = ["nori.db", "nori.db-wal", "nori.db-shm", "nori.db-journal", "certs", "other.db", "nori.db.bak"].map(String::from).to_vec();
        assert_eq!(storage_index_files(names), [0, 1, 2]);
    }
}
