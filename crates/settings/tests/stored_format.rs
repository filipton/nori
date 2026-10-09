//! Golden test of the settings' stored format, values by name and specs (testdata/stored_format.txt),
//! for a record with every setting away from its default.
//!
//! `NORI_BLESS=1 cargo test -p nori-settings --test stored_format` rewrites the file; only for an
//! intended format change, stated in the commit.

use std::collections::{BTreeMap, HashMap};

use nori_settings::settings::{band_from, load, save, AutoFillBasis, AutoFillKind, ControlsSide, CrossfadeCurve, DownloadBeats, EqMode, GainMode, HideStatusBar, HomeRow, KeepAwake, MaxRate, PrefValue, SavedQuality, SavedServer, StoredPrefs, SwipeAction, TapAction, ThemeMode};
use nori_settings::settings_model::{specs, value_of};

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/stored_format.txt");

/// Every setting away from its default.
fn sample() -> StoredPrefs {
    StoredPrefs {
        servers: vec![SavedServer {
            id: "a1".into(),
            name: "Home".into(),
            url: "https://music.example.com".into(),
            alt_url: "http://10.0.0.2:4533".into(),
            user: "me".into(),
            password: "pw".into(),
            api_key: "key".into(),
            legacy_auth: true,
            headers: [("X-Auth".to_string(), "t".to_string())].into(),
            allow_self_signed: true,
            client_cert: "c.p12".into(),
            client_cert_password: "cp".into(),
            wifi_only: true,
            music_folder_id: "3".into(),
            alt_max_bit_rate: 192,
        }],
        active_server_id: "a1".into(),
        wifi: SavedQuality { bit_rate: 320, format: "mp3".into() },
        mobile: SavedQuality { bit_rate: 96, format: "opus".into() },
        download: SavedQuality { bit_rate: 128, format: "opus".into() },
        parallel_downloads: 7,
        download_beats: DownloadBeats::Never,
        covers_ahead: 8,
        cache_mb: 4096,
        replay_gain: GainMode::Auto,
        preamp_db: -2.5,
        untagged_gain_db: -9.0,
        loudness_target: -14.0,
        gain_boost_db: 3.0,
        gain_measured: false,
        fade_ms: 300,
        pitch: 1.05,
        previous_always_skips: true,
        precache_wifi: 5,
        precache_mobile: 3,
        skip_on_error: false,
        crossfade_keep_albums: false,
        crossfade_curve: CrossfadeCurve::SCurve,
        crossfade_in_sec: 2,
        crossfade_out_sec: 6,
        offload: false,
        bit_perfect: true,
        hi_res: true,
        max_rate: MaxRate::Khz96,
        scrobble: false,
        playlist_descriptions: false,
        hide_import_notes: false,
        auto_fill: false,
        bridge_offline: true,
        auto_fill_kind: AutoFillKind::Albums,
        auto_fill_basis: AutoFillBasis::Genre,
        auto_fill_remote: true,
        eq_enabled: true,
        eq_bands: vec![band_from(1, 105.0, -3.5, 0.7, 1), band_from(9, 12_345_678.0, 2.0, 0.00001, 2)],
        eq_preamp_db: Some(-4.25),
        eq_mode: EqMode::Parametric,
        eq_graphic: vec![1.5, -2.0, 0.0, 3.0, 4.5, 6.0, -12.0, 12.0, 0.5, -0.5],
        eq_graphic_target: (0..96).map(|i| i as f32 * 0.25 - 12.0).collect(),
        bass_boost_db: 6.0,
        virtualizer: 0.4,
        volume_boost_db: 3.5,
        compressor: true,
        comp_threshold_db: -30.0,
        comp_ratio: 6.0,
        comp_attack_ms: 2.5,
        comp_release_ms: 400.0,
        comp_makeup_db: 7.0,
        comp_knee_db: 10.0,
        expander: true,
        exp_threshold_db: -45.0,
        exp_ratio: 6.0,
        exp_attack_ms: 3.0,
        exp_release_ms: 300.0,
        loudness: true,
        loudness_ref_phon: 85,
        crossfeed_db: 3.0,
        crossfeed_hz: 650.0,
        sound_bypass: true,
        balance: -0.25,
        mono: true,
        limiter: true,
        limiter_threshold_db: -2.0,
        crossfade_sec: 6,
        auto_mix: true,
        auto_mix_max_s: 16,
        auto_mix_beat_match: false,
        auto_mix_max_tempo_pct: 4.0,
        auto_mix_bass_swap: false,
        auto_mix_filters: false,
        auto_mix_echo_out: false,
        auto_mix_keep_pitch: false,
        auto_mix_better_beats: true,
        auto_mix_beats_mobile_data: true,
        speed: 1.25,
        skip_silence: true,
        scrobble_percent: 75,
        live_search_delay_ms: 500,
        taste_model: false,
        third_party_lookups: false,
        update_check: false,
        update_beta: true,
        remote_control: true,
        jam: true,
        jam_along: false,
        profile_per_output: false,
        auto_eq_auto: true,
        auto_eq_download: false,
        lyrics_sweep: false,
        soft_sleeve: false,
        motion_artwork: true,
        motion_artwork_wifi_only: false,
        favourite_notice: false,
        lyrics_keep_screen_on: false,
        lyrics_translation: false,
        lyrics_size: 2,
        lyrics_online: false,
        lyrics_order: {
            let mut o = nori_settings::lyrics_sources::default_order();
            o.reverse();
            o
        },
        lyrics_on: vec![nori_settings::lyrics_sources::LyricsService::Lrclib, nori_settings::lyrics_sources::LyricsService::Kugou],
        lyrics_prefer_words: false,
        paxsenix_key: "pax".into(),
        better_lyrics_key: "better".into(),
        theme: ThemeMode::Dark,
        hide_status_bar: HideStatusBar::Always,
        lyrics_timing_button: false,
        keep_awake: KeepAwake::SidewaysCharging,
        controls_side: ControlsSide::Left,
        amoled: true,
        player_colours: false,
        album_colours: true,
        artist_colours: true,
        dynamic_color: false,
        accent: 0xFF1E88E5,
        cover_colors: false,
        reduce_motion: true,
        ignore_system_motion: false,
        ui_scale: 1.1,
        tap_action: TapAction::PlayNext,
        swipe_right: SwipeAction::Download,
        swipe_left: SwipeAction::PlayNext,
        skip_explicit: true,
        home_rows: vec![HomeRow::Random, HomeRow::Pinned, HomeRow::Newest],
        pinned_playlists: vec!["p1".into(), "p2".into()],
        list_prefs: [("albums".to_string(), "grid".to_string())].into(),
    }
}

/// One stored value as text, with its kind.
fn stored(v: &PrefValue) -> String {
    match v {
        PrefValue::Flag { v } => format!("flag {v}"),
        PrefValue::Number { v } => format!("int {v}"),
        PrefValue::Big { v } => format!("long {v}"),
        PrefValue::Decimal { v } => format!("float {v:?}"),
        PrefValue::Text { v } => format!("text {v:?}"),
    }
}

/// JSON fields compared as JSON, since key order is not stable.
fn normal(key: &str, v: &PrefValue) -> String {
    match (key, v) {
        ("servers" | "listPrefs", PrefValue::Text { v }) => format!("json {}", serde_json::from_str::<serde_json::Value>(v).unwrap()),
        _ => stored(v),
    }
}

fn section(title: &str, rows: BTreeMap<String, String>) -> String {
    let mut s = format!("## {title}\n");
    for (k, v) in rows {
        s.push_str(&format!("{k} = {v}\n"));
    }
    s
}

fn render() -> String {
    let p = sample();
    let saved = |p: &StoredPrefs| save(p).iter().map(|(k, v)| (k.clone(), normal(k, v))).collect::<BTreeMap<_, _>>();
    let values = specs().iter().map(|s| (s.name.clone(), format!("{:?}", value_of(&p, &s.name).unwrap()))).collect();
    let spec_rows = specs()
        .iter()
        .map(|s| (s.name.clone(), format!("{:?} {:?} {:?}..{:?} default {:?}", s.kind, s.options, s.min, s.max, s.default)))
        .collect();
    [
        section("stored, every setting changed", saved(&p)),
        section("stored, the defaults", saved(&StoredPrefs::default())),
        section("values by name, every setting changed", values),
        section("specs", spec_rows),
    ]
    .join("\n")
}

#[test]
fn stored_format_matches_golden() {
    let now = render();
    if std::env::var_os("NORI_BLESS").is_some() {
        std::fs::write(GOLDEN, &now).unwrap();
    }
    let golden = std::fs::read_to_string(GOLDEN).expect("testdata/stored_format.txt");
    for (a, b) in golden.lines().zip(now.lines()) {
        assert_eq!(a, b, "the stored format or the model moved");
    }
    assert_eq!(golden.lines().count(), now.lines().count());
}

#[test]
fn sample_round_trips_and_differs() {
    let p = sample();
    let raw: HashMap<String, PrefValue> = save(&p);
    assert_eq!(load(&raw), p);
    // A key no setting reads any more (a removed one) is ignored.
    let mut old = raw.clone();
    old.insert("scrollTitles".into(), PrefValue::Flag { v: false });
    assert_eq!(load(&old), p);
    // The defaults write every key too, except the pre-amp, stored only when manual.
    let defaults = save(&StoredPrefs::default());
    let mut keys: Vec<&String> = raw.keys().collect();
    let mut default_keys: Vec<&String> = defaults.keys().collect();
    let preamp = "eqPreampDb".to_string();
    default_keys.push(&preamp);
    keys.sort();
    default_keys.sort();
    assert_eq!(keys, default_keys);
    // Every value differs from its default, so the golden copy covers each codec.
    let same: Vec<&String> = raw.iter().filter(|(k, v)| defaults.get(*k) == Some(v)).map(|(k, _)| k).collect();
    assert!(same.is_empty(), "sample leaves these at their defaults: {same:?}");
}

#[test]
fn removed_settings_still_load() {
    // Sing's keys stay in settings stored before it was removed.
    let mut raw = save(&sample());
    raw.insert("sing".into(), PrefValue::Flag { v: true });
    raw.insert("singVocalLevel".into(), PrefValue::Decimal { v: 0.25 });
    assert_eq!(load(&raw), sample());
}
