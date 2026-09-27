//! Settings, the window's own: grouped lists as the Mac's own settings draw them, every word on them this
//! client's. What each setting is, the values it offers and its value now are the core's settings model
//! (`nori_settings::settings_model`); a row sends back the setting's name with the value picked
//! (`setting_set`). Which settings are offered, and in what words, follows the terminal client's choice
//! of what makes sense at a desk.

use nori_core::settings::StoredPrefs;
use nori_core::settings_model;
use slint::{ModelRc, SharedString, VecModel};

use crate::SettingRow;

// Row kinds, as app.slint draws them.
const HEADING: i32 = 0;
const TOGGLE: i32 = 1;
const CHOICE: i32 = 2;
const ACTION: i32 = 3;
const INFO: i32 = 4;

/// One row before it is placed in its group.
struct Row {
    kind: i32,
    name: String,
    title: String,
    detail: String,
    on: bool,
    options: Vec<(String, String)>,
    button: String,
}

fn value(p: &StoredPrefs, name: &str) -> String {
    settings_model::value_of(p, name).unwrap_or_default()
}

fn options(name: &str) -> Vec<String> {
    settings_model::specs().into_iter().find(|s| s.name == name).map(|s| s.options).unwrap_or_default()
}

fn toggle(p: &StoredPrefs, name: &str, title: &str, detail: &str) -> Row {
    Row { kind: TOGGLE, name: name.into(), title: title.into(), detail: detail.into(), on: value(p, name) == "true", options: Vec::new(), button: String::new() }
}

/// The core's options for `name`, each worded by `label`.
fn choice(name: &str, title: &str, label: impl Fn(&str) -> String) -> Row {
    let options = options(name).into_iter().map(|v| (label(&v), v)).collect();
    Row { kind: CHOICE, name: name.into(), title: title.into(), detail: String::new(), on: false, options, button: String::new() }
}

/// An enum setting: its values by name, worded in the same order.
fn named(name: &str, title: &str, labels: &[&str]) -> Row {
    let names = options(name);
    choice(name, title, |v| names.iter().position(|n| n == v).and_then(|i| labels.get(i)).map_or_else(|| v.to_string(), |l| l.to_string()))
}

fn action(title: &str, detail: String, button: &str, act: &str) -> Row {
    Row { kind: ACTION, name: act.into(), title: title.into(), detail, on: false, options: Vec::new(), button: button.into() }
}

fn info(title: &str, detail: String) -> Row {
    Row { kind: INFO, name: String::new(), title: title.into(), detail, on: false, options: Vec::new(), button: String::new() }
}

fn off_or(v: &str, words: impl Fn(&str) -> String) -> String {
    if v == "0" { "Off".into() } else { words(v) }
}

fn quality(v: &str) -> String {
    match v.split_once(':') {
        Some((_, "")) | None => "Original".into(),
        Some((rate, format)) => format!("{} {rate} kbps", format.to_uppercase()),
    }
}

/// The page's groups, each a heading and its rows.
fn groups(p: &StoredPrefs, server: &str) -> Vec<(&'static str, Vec<Row>)> {
    let mut mixing = Vec::new();
    if !p.auto_mix {
        mixing.push(choice("crossfadeSec", "Crossfade", |v| off_or(v, |v| format!("{v} seconds"))));
    }
    mixing.push(toggle(p, "autoMix", "AutoMix", "Beat-matched transitions between songs"));
    mixing.push(toggle(p, "crossfadeKeepAlbums", "Gapless albums", "Never mix songs of the same album"));
    mixing.push(choice("fadeMs", "Fade on play and pause", |v| off_or(v, |v| format!("{v} ms"))));

    let mut sound = vec![named("replayGain", "Sound Check", &["Off", "Track", "Album", "Automatic"])];
    if p.replay_gain != nori_core::settings::GainMode::Off {
        sound.push(choice("loudnessTarget", "Loudness target", |v| format!("{v} LUFS")));
    }
    sound.push(toggle(p, "loudness", "Loudness compensation", "Turned down, the bass comes up as the ear needs"));
    sound.push(toggle(p, "hiRes", "High quality output", "Float samples to the device; 24-bit files kept whole"));

    vec![
        (
            "Playback",
            vec![
                toggle(p, "autoFill", "Autoplay", "Keep playing similar music when the queue runs out"),
                toggle(p, "previousAlwaysSkips", "Previous always skips", "Never restart the current song"),
                toggle(p, "skipOnError", "Skip unplayable songs", "Up to three in a row"),
                toggle(p, "skipExplicit", "Skip explicit songs", "Songs the server marks explicit"),
                toggle(p, "bridgeOffline", "Offline fallback", "Play downloads while the server is unreachable"),
            ],
        ),
        ("Transitions", mixing),
        ("Sound", sound),
        (
            "Lyrics",
            vec![
                toggle(p, "lyricsOnline", "Find lyrics online", "When the server has no timed lyrics; sends the artist, title and album"),
                toggle(p, "lyricsTranslation", "Translations", "When the server has them"),
            ],
        ),
        (
            "Streaming and downloads",
            vec![
                choice("wifi", "Streaming quality", quality),
                choice("download", "Download quality", quality),
                choice("cacheMb", "Stream cache limit", |v| match v.parse::<i32>() {
                    Ok(mb) if mb % 1024 == 0 => format!("{} GB", mb / 1024),
                    _ => format!("{v} MB"),
                }),
            ],
        ),
        (
            "History and privacy",
            vec![
                toggle(p, "scrobble", "Scrobble", "Report what you play to the server"),
                toggle(p, "tasteModel", "Listening history", "Kept on this Mac; it shapes Autoplay and the mixes"),
                toggle(p, "thirdPartyLookups", "Third-party lookups", "Lyrics services; off, nothing leaves for anyone but your server"),
            ],
        ),
        (
            "Storage",
            vec![
                action("Library index", "Read the library from the server again".into(), "Refresh", "sync-library"),
                action("Streamed music", "Songs kept from streaming".into(), "Clear", "clear-stream"),
                action("Artwork", "Covers kept on this Mac".into(), "Clear", "clear-covers"),
                action("Lyrics", "Lyrics found online".into(), "Clear", "clear-lyrics"),
            ],
        ),
        ("Server", vec![info("Signed in to", server.to_string()), action("Servers", "Add another server or switch".into(), "Manage…", "servers")]),
        ("About", vec![info("nori", format!("Version {}", env!("CARGO_PKG_VERSION")))]),
    ]
}

/// The rows as the page draws them: each group's heading, then its rows with the first and last marked
/// (they round the group's corners).
pub fn rows(p: &StoredPrefs, server: &str) -> ModelRc<SettingRow> {
    let mut out = Vec::new();
    for (title, rows) in groups(p, server) {
        out.push(SettingRow { kind: HEADING, title: title.into(), ..Default::default() });
        let n = rows.len();
        for (i, r) in rows.into_iter().enumerate() {
            let v = value(p, &r.name);
            let chosen = r.options.iter().position(|o| o.1 == v).map_or(-1, |i| i as i32);
            let labels: Vec<SharedString> = r.options.iter().map(|o| o.0.as_str().into()).collect();
            out.push(SettingRow {
                kind: r.kind,
                name: r.name.into(),
                title: r.title.into(),
                detail: r.detail.into(),
                on: r.on,
                options: ModelRc::new(VecModel::from(labels)),
                chosen,
                button: r.button.into(),
                first: i == 0,
                last: i + 1 == n,
            });
        }
    }
    ModelRc::new(VecModel::from(out))
}

/// The value of option `index` of setting `name`, to hand to `setting_set`.
pub fn option_value(name: &str, index: usize) -> Option<String> {
    options(name).into_iter().nth(index)
}
