//! Settings pages, following Android's SettingsPages.kt minus phone-only settings. Settings, options and
//! values come from the core's `settings_model`; rows report back the setting name and the picked value.

use nori_core::settings::{EqLevel, GainMode, StoredPrefs, EQ_RANGES};
use nori_core::settings_model::{self, BeatModel, SettingsState};
use slint::{Color, ModelRc, SharedString, VecModel};

use crate::{words, SettingRow};

// Row kinds, as app.slint numbers them.
const HEADING: i32 = 0;
const TOGGLE: i32 = 1;
const CHOICE: i32 = 2;
const ACTION: i32 = 3;
const INFO: i32 = 4;
const SLIDER: i32 = 5;
const RANKED: i32 = 6;
const TEXT: i32 = 7;
const SERVER: i32 = 8;
const NOTE: i32 = 9;
const PALETTE: i32 = 10;
const LINK: i32 = 11;
const BUTTON: i32 = 12;

/// Non-setting facts the pages show (sizes, counts, devices).
#[derive(Default, Clone)]
pub struct Facts {
    pub analysed: u32,
    pub indexed: (u32, u32, u32),
    pub stream_bytes: u64,
    pub cover_bytes: u64,
    pub lyrics_bytes: u64,
    pub download_bytes: u64,
    pub download_songs: u32,
    pub database_bytes: u64,
    pub folders: Vec<(String, String)>,
    pub devices: Vec<String>,
    pub device: String,
    pub syncing: bool,
}

/// Row name of the output device choice (a desktop setting, not the core's).
const DEVICE: &str = "!device";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    Setting(&'a str),
    Device,
}

impl<'a> Target<'a> {
    pub fn of(name: &'a str) -> Target<'a> {
        if name == DEVICE { Target::Device } else { Target::Setting(name) }
    }
}

const EQUALIZER: &str = "equalizer";
const ADD_SERVER: &str = "add-server";

/// A link or button on the settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Equalizer,
    AddServer,
    /// Switch to the server with this id.
    Server(String),
    Chore(Chore),
}

impl Act {
    /// Parses a row name; server rows are `server:<id>`.
    pub fn of(name: &str) -> Option<Act> {
        if let Some(id) = name.strip_prefix("server:") {
            return Some(Act::Server(id.to_string()));
        }
        match name {
            EQUALIZER => Some(Act::Equalizer),
            ADD_SERVER => Some(Act::AddServer),
            _ => CHORES.into_iter().find(|c| chore_name(*c) == name).map(Act::Chore),
        }
    }
}

pub use nori_host::session::Chore;

const CHORES: [Chore; 6] = [Chore::SyncLibrary, Chore::DownloadLibrary, Chore::MeasureAgain, Chore::ClearStream, Chore::ClearCovers, Chore::ClearLyrics];

/// A chore's row name in the UI.
fn chore_name(c: Chore) -> &'static str {
    match c {
        Chore::SyncLibrary => "sync-library",
        Chore::DownloadLibrary => "download-library",
        Chore::MeasureAgain => "measure-again",
        Chore::ClearStream => "clear-stream",
        Chore::ClearCovers => "clear-covers",
        Chore::ClearLyrics => "clear-lyrics",
    }
}

#[derive(Default)]
struct Row {
    kind: i32,
    name: String,
    title: String,
    detail: String,
    on: bool,
    enabled: bool,
    options: Vec<(String, String)>,
    button: String,
    value: f32,
    min: f32,
    max: f32,
    centred: bool,
    swatches: Vec<u32>,
}

struct Build<'a> {
    p: &'a StoredPrefs,
    s: &'a SettingsState,
    f: &'a Facts,
}

fn options(name: &str) -> Vec<String> {
    settings_model::specs().into_iter().find(|s| s.name == name).map(|s| s.options).unwrap_or_default()
}

fn off_or(v: &str, words: impl Fn(&str) -> String) -> String {
    if v == "0" { "Off".into() } else { words(v) }
}

fn minus(v: &str) -> String {
    v.replace('-', "−")
}

fn one(v: f32) -> String {
    format!("{v:.1}")
}

fn percent(v: &str) -> String {
    format!("{v} %")
}

fn seconds(v: &str) -> String {
    format!("{v} s")
}

/// The accent to draw: the core's default (Android purple) becomes red here; others as picked.
pub fn accent_shown(argb: u32) -> u32 {
    let default = StoredPrefs::default().accent as u32;
    if argb == default { 0xFFFA2D48 } else { argb }
}

/// "850 B", "38 MB", "2.1 GB".
pub fn bytes(n: u64) -> String {
    let n = n as f64;
    if n < 1024.0 {
        format!("{n:.0} B")
    } else if n < 1_048_576.0 {
        format!("{:.0} KB", n / 1024.0)
    } else if n < 10_485_760.0 {
        format!("{:.1} MB", n / 1_048_576.0)
    } else if n < 1_073_741_824.0 {
        format!("{:.0} MB", n / 1_048_576.0)
    } else {
        format!("{:.1} GB", n / 1_073_741_824.0)
    }
}

impl Build<'_> {
    fn value(&self, name: &str) -> String {
        self.s.values.get(name).cloned().unwrap_or_default()
    }

    fn toggle(&self, name: &str, title: &str, detail: &str) -> Row {
        self.toggle_if(name, title, detail, true)
    }

    fn toggle_if(&self, name: &str, title: &str, detail: &str, enabled: bool) -> Row {
        Row { kind: TOGGLE, name: name.into(), title: title.into(), detail: detail.into(), on: self.value(name) == "true", enabled, ..Default::default() }
    }

    fn choice(&self, name: &str, title: &str, label: impl Fn(&str) -> String) -> Row {
        self.choice_if(name, title, true, label)
    }

    fn choice_if(&self, name: &str, title: &str, enabled: bool, label: impl Fn(&str) -> String) -> Row {
        let options = options(name).into_iter().map(|v| (label(&v), v)).collect();
        Row { kind: CHOICE, name: name.into(), title: title.into(), options, enabled, ..Default::default() }
    }

    fn choice_of(&self, name: &str, title: &str, options: Vec<(String, String)>) -> Row {
        Row { kind: CHOICE, name: name.into(), title: title.into(), options, enabled: true, ..Default::default() }
    }

    /// An enum setting with one label per option, in option order.
    fn named(&self, name: &str, title: &str, labels: &[&str]) -> Row {
        let names = options(name);
        self.choice(name, title, |v| names.iter().position(|n| n == v).and_then(|i| labels.get(i)).map_or_else(|| v.to_string(), |l| l.to_string()))
    }

    fn slider(&self, name: &str, label: String, value: f32, (min, max): (f32, f32), centred: bool) -> Row {
        Row { kind: SLIDER, name: name.into(), title: label, value: value.clamp(min, max), min, max, centred, enabled: true, ..Default::default() }
    }

    fn action(&self, title: &str, detail: String, button: &str, enabled: bool, chore: Chore) -> Row {
        Row { kind: ACTION, name: chore_name(chore).into(), title: title.into(), detail, button: button.into(), enabled, ..Default::default() }
    }

    fn info(&self, title: &str, detail: String) -> Row {
        Row { kind: INFO, title: title.into(), detail, ..Default::default() }
    }

    fn note(&self, text: &str) -> Row {
        Row { kind: NOTE, title: text.into(), ..Default::default() }
    }

    fn link(&self, title: &str, status: &str, act: &str) -> Row {
        Row { kind: LINK, name: act.into(), title: title.into(), detail: status.into(), enabled: true, ..Default::default() }
    }

    fn text(&self, name: &str, title: &str, detail: &str) -> Row {
        Row { kind: TEXT, name: name.into(), title: title.into(), detail: detail.into(), button: self.value(name), enabled: true, ..Default::default() }
    }

    fn general(&self) -> Vec<(&'static str, Vec<Row>)> {
        let p = self.p;
        let accents: Vec<u32> = options("accent").iter().filter_map(|c| c.parse::<i64>().ok()).map(|c| c as u32).collect();
        let look = vec![
            Row { kind: PALETTE, name: "accent".into(), title: "Accent color".into(), swatches: accents, value: p.accent as u32 as f32, enabled: true, ..Default::default() },
            self.toggle("coverColors", "Colors from the cover", "Pages take their colors from the artwork."),
            self.toggle("favouriteNotice", "Confirm favorites", "A short message when you favorite or unfavorite something."),
        ];
        let mut devices = vec![("The system's own".to_string(), String::new())];
        devices.extend(self.f.devices.iter().map(|d| (d.clone(), d.clone())));
        let output = vec![
            Row { kind: CHOICE, name: DEVICE.into(), title: "Play through".into(), options: devices, enabled: true, ..Default::default() },
            self.note("A new device is used from the next start of nori."),
        ];
        vec![("Look", look), ("Output device", output), ("About", vec![self.info("nori", format!("Version {}", env!("CARGO_PKG_VERSION")))])]
    }

    fn playing(&self) -> Vec<(&'static str, Vec<Row>)> {
        let p = self.p;
        let s = self.s;
        let live = !s.untouched;
        let mut between = Vec::new();
        if !p.auto_mix {
            between.push(self.choice_if("crossfadeSec", "Crossfade", live, |v| off_or(v, seconds)));
        }
        if !p.auto_mix && p.crossfade_sec > 0 {
            between.push(self.named("crossfadeCurve", "Crossfade curve", &["Equal power", "Linear", "S-curve"]));
            let part = |v: &str| if v == "0" { "The whole crossfade".to_string() } else { seconds(v) };
            between.push(self.choice_if("crossfadeInSec", "Fade in over", live, part));
            between.push(self.choice_if("crossfadeOutSec", "Fade out over", live, part));
        }
        between.push(self.toggle_if("autoMix", "AutoMix", "Blends songs like a DJ, matching the beat.", live));
        if p.auto_mix {
            between.push(self.choice_if("autoMixMaxS", "Longest mix", live, seconds));
            between.push(self.toggle_if("autoMixBeatMatch", "Match the beat", "Nudges the next song's speed so the beats line up.", live));
            if p.auto_mix_beat_match {
                between.push(self.choice_if("autoMixMaxTempoPct", "Biggest speed change", live, percent));
                between.push(self.toggle_if("autoMixKeepPitch", "Keep the pitch", "Off lets the pitch follow the speed, up to 2 %.", live));
            }
            between.push(self.toggle_if("autoMixBassSwap", "Swap the bass", "The new song's bass replaces the old one's.", live));
            between.push(self.toggle_if("autoMixFilters", "Muffle the ending", "The outgoing song fades out muffled.", live));
            between.push(self.toggle_if("autoMixEchoOut", "Echo out clashes", "Overlapping vocals end in an echo instead.", live));
            if s.beat_model != BeatModel::Unavailable {
                let mb = s.beat_model_mb;
                let detail = if !p.auto_mix_better_beats {
                    format!("Listens to the start and end of each song so mixes land on the beat. A one-time download of about {mb} MB, from the model's authors.")
                } else {
                    match &s.beat_model {
                        BeatModel::Ready => "On. Each song is listened to once, just before it plays.".into(),
                        BeatModel::Downloading => format!("Downloading (about {mb} MB)…"),
                        BeatModel::Failed { .. } => "The download didn't work. It tries again at the next song.".into(),
                        _ => format!("Downloaded (about {mb} MB) the next time AutoMix measures a song."),
                    }
                };
                between.push(self.toggle_if("autoMixBetterBeats", "Better beat detection", &detail, live));
            }
            let n = self.f.analysed;
            between.push(self.action("Measured songs", format!("{} measured for tempo and beats.", words::count(n, "song", "songs")), "Measure again", n > 0, Chore::MeasureAgain));
        }
        between.push(self.toggle_if("crossfadeKeepAlbums", "Keep albums gapless", "An album played or added to the queue whole plays without mixing between its songs.", live));
        between.push(self.choice("fadeMs", "Fade on play and pause", |v| off_or(v, |v| if v.parse::<u32>().is_ok_and(|n| n % 1000 == 0) { seconds(&(v.parse::<u32>().unwrap_or(0) / 1000).to_string()) } else { format!("{v} ms") })));

        let controls = vec![
            self.toggle("previousAlwaysSkips", "Previous goes back a song", "Instead of restarting the current one."),
            self.choice("speed", "Speed", |v| if v == "1" { "Normal".into() } else { format!("{v}×") }),
            self.choice("pitch", "Pitch", |v| {
                let pct = ((v.parse::<f32>().unwrap_or(1.0) - 1.0) * 100.0).round() as i32;
                match pct {
                    0 => "Normal".into(),
                    n if n < 0 => format!("−{} %", -n),
                    n => format!("+{n} %"),
                }
            }),
            self.toggle_if("skipSilence", "Skip silence", if s.untouched { "Off while the audio is played untouched." } else { "Cuts quiet gaps in and between songs." }, live),
        ];
        let mut queue = vec![
            self.toggle("skipExplicit", "Skip explicit songs", "Songs your server marks explicit."),
            self.toggle("autoFill", "Keep playing when the queue ends", "Adds more music automatically."),
        ];
        if p.auto_fill {
            queue.push(self.named("autoFillKind", "Carry on with", &["Songs", "Albums"]));
            queue.push(self.named("autoFillBasis", "Chosen by", &["Similar music", "The same artist", "The same genre", "The same era"]));
            queue.push(self.toggle(
                "autoFillRemote",
                "Include remote songs",
                "Songs not in your library yet, from the server's streaming providers. Each one played is downloaded into the library.",
            ));
        }
        let wrong = vec![
            self.toggle("skipOnError", "Skip songs that won't play", "Up to three in a row."),
            self.toggle("bridgeOffline", "Play downloads when offline", "If the server drops, keep playing from downloads."),
        ];
        vec![("Between songs", between), ("Controls", controls), ("Queue", queue), ("When something goes wrong", wrong)]
    }

    fn sound(&self) -> Vec<(&'static str, Vec<Row>)> {
        let p = self.p;
        let s = self.s;
        let status = if s.untouched { "Off now" } else if s.sound_chain_on { "On" } else { "Off" };
        let eq = vec![
            self.link("Equalizer and crossfeed", status, EQUALIZER),
            self.toggle("autoEqAuto", "AutoEQ for headphones", "Applies a known correction curve when headphones connect."),
            self.toggle("autoEqDownload", "Keep the AutoEQ list", "Downloads the headphone list (850 kB, from github.com), and again once a month, so headphones find their curve."),
            self.toggle("profilePerOutput", "Remember sound per device", "Each device keeps its own equalizer settings."),
            self.toggle("soundBypass", "No processing on this output", "No equalizer, crossfeed or effects reach this output. Saved with the device's sound."),
        ];
        let mut volume = vec![self.named("replayGain", "Even out volume", &["Off", "Per song", "Per album", "Automatic"])];
        if p.replay_gain != GainMode::Off {
            let r = EQ_RANGES.replay_gain_preamp;
            volume.push(self.slider("preampDb", format!("Overall level {} dB", crate::eq::signed(p.preamp_db)), p.preamp_db, (r.min, r.max), true));
            volume.push(self.choice("loudnessTarget", "Loudness target", |v| match v {
                "-18" => format!("{} LUFS (ReplayGain)", minus(v)),
                "-14" | "-16" => format!("{} LUFS (streaming)", minus(v)),
                "-23" => format!("{} LUFS (broadcast)", minus(v)),
                _ => format!("{} LUFS", minus(v)),
            }));
            volume.push(self.choice("gainBoostDb", "Turn quiet songs up", |v| off_or(v, |v| format!("Up to +{v} dB"))));
            volume.push(self.choice("untaggedGainDb", "Volume for songs without tags", |v| format!("{} dB", minus(v))));
            volume.push(self.toggle("gainMeasured", "Measure songs without tags", "Songs without tags play at the loudness AutoMix measured, once it has."));
        }
        let output = vec![
            self.toggle("hiRes", "High quality output", "Plays 24-bit files in full, and runs the equalizer and effects in floating point. Starts with the next song."),
            self.named("maxRate", "Highest sample rate", &["Each song's own", "48 kHz", "96 kHz", "192 kHz"]),
        ];
        vec![("Equalizer", eq), ("Effects", self.effects()), ("Volume", volume), ("Output", output)]
    }

    fn effects(&self) -> Vec<Row> {
        let p = self.p;
        let boost = |db: f32| if db <= 0.0 { "Off".to_string() } else { format!("{} dB", crate::eq::signed(db)) };
        let mut rows = vec![
            self.slider("bassBoostDb", format!("Bass boost: {}", boost(p.bass_boost_db)), p.bass_boost_db, (0.0, 12.0), false),
            self.slider(
                "virtualizer",
                format!("Virtualizer: {}", if p.virtualizer <= 0.0 { "Off".into() } else { percent(&format!("{:.0}", p.virtualizer * 100.0)) }),
                p.virtualizer,
                (0.0, 1.0),
                false,
            ),
            self.slider("volumeBoostDb", format!("Volume boost: {}", boost(p.volume_boost_db)), p.volume_boost_db, (0.0, 12.0), false),
            self.note("Boosts turn the limiter on with them, so loud songs do not distort."),
            self.toggle("compressor", "Compressor", "Brings quiet passages up and loud ones down, for noisy places or quiet listening."),
        ];
        if p.compressor {
            rows.push(self.choice("compressorPreset", "Strength", |v| match v {
                "GENTLE" => "Gentle".into(),
                "STRONG" => "Strong".into(),
                "BALANCED" => "Balanced".into(),
                _ => "Custom".into(),
            }));
            rows.push(self.slider("compThresholdDb", format!("Starts at {} dB", minus(&one(p.comp_threshold_db))), p.comp_threshold_db, (-60.0, 0.0), false));
            rows.push(self.slider("compRatio", format!("Ratio {}:1", one(p.comp_ratio)), p.comp_ratio, (1.0, 10.0), false));
            rows.push(self.slider("compAttackMs", format!("Reacts in {} ms", one(p.comp_attack_ms)), p.comp_attack_ms, (0.1, 100.0), false));
            rows.push(self.slider("compReleaseMs", format!("Lets go over {:.0} ms", p.comp_release_ms), p.comp_release_ms, (10.0, 1000.0), false));
            rows.push(self.slider("compMakeupDb", format!("Makes up {} dB", crate::eq::signed(p.comp_makeup_db)), p.comp_makeup_db, (0.0, 12.0), false));
            rows.push(self.slider("compKneeDb", format!("Softness {} dB", one(p.comp_knee_db)), p.comp_knee_db, (0.0, 12.0), false));
        }
        rows.push(self.toggle(
            "loudness",
            "Loudness compensation",
            "As you turn the volume down, brings the bass and the highest notes up the way the ear needs (ISO 226), so quiet listening does not sound thin.",
        ));
        if p.loudness {
            rows.push(self.choice("loudnessRefPhon", "Balanced at, all the way up", |v| format!("{v} phon")));
        }
        rows.push(self.toggle("expander", "Noise gate", "Turns hiss and hum down further when the music goes quiet. A high ratio closes it like a gate."));
        if p.expander {
            rows.push(self.slider("expThresholdDb", format!("Works under {} dB", minus(&one(p.exp_threshold_db))), p.exp_threshold_db, (-90.0, -10.0), false));
            rows.push(self.slider("expRatio", format!("Ratio 1:{}", one(p.exp_ratio)), p.exp_ratio, (1.0, 20.0), false));
            rows.push(self.slider("expAttackMs", format!("Opens in {} ms", one(p.exp_attack_ms)), p.exp_attack_ms, (0.1, 50.0), false));
            rows.push(self.slider("expReleaseMs", format!("Closes over {:.0} ms", p.exp_release_ms), p.exp_release_ms, (10.0, 1000.0), false));
        }
        rows
    }

    fn lyrics(&self) -> Vec<(&'static str, Vec<Row>)> {
        let display = vec![
            self.toggle("lyricsSweep", "Fill in words as they're sung", "For lyrics timed word by word."),
            self.toggle("lyricsTranslation", "Show translations", "When your server has them."),
        ];
        let online = self.value("lyricsOnline") == "true";
        let mut sources = vec![self.toggle(
            "lyricsOnline",
            "Find missing lyrics online",
            "When your server has no timed lyrics. Sends the artist, song and album name to the lyrics sources.",
        )];
        if online {
            sources.push(self.toggle("lyricsPreferWords", "Prefer word-by-word lyrics", "Keeps looking past lyrics timed line by line."));
        }
        let mut out = vec![("Display", display), ("Sources", sources)];
        if online {
            let mut ranked: Vec<Row> = self
                .s
                .lyrics_sources
                .iter()
                .filter_map(|src| {
                    let (title, about) = service(&src.id)?;
                    Some(Row { kind: RANKED, name: src.id.clone(), title: title.into(), detail: about.into(), on: src.on, enabled: true, ..Default::default() })
                })
                .collect();
            ranked.push(self.note(
                "The quick ones (PaxSenix, BiniLyrics, Unison and LRCLIB) are asked first, together, and the others only when those find nothing good. Every answer is scored on how well it matches the song, how finely it is timed and whether the other services agree, and the best is shown; the order here settles near ties. Your server's own timed lyrics always come before all of these.",
            ));
            out.push(("In the order they're asked", ranked));
            out.push((
                "Keys",
                vec![
                    self.text("paxSenixKey", "PaxSenix key", "Your own key, for its Spotify and Musixmatch lyrics."),
                    self.text("betterLyricsKey", "BetterLyrics key", "Without one it answers only for songs it has already stored."),
                ],
            ));
        }
        out
    }

    fn library(&self) -> Vec<(&'static str, Vec<Row>)> {
        let (songs, albums, artists) = self.f.indexed;
        let counts = format!("{} · {} · {} on this Mac", words::count(songs, "song", "songs"), words::count(albums, "album", "albums"), words::count(artists, "artist", "artists"));
        let search = vec![
            self.action("Offline search", counts, if self.f.syncing { "Updating…" } else { "Update" }, !self.f.syncing, Chore::SyncLibrary),
            self.choice("liveSearchDelayMs", "Search delay", |v| format!("{v} ms")),
        ];
        let mut history = vec![
            self.toggle("tasteModel", "Keep listening history", "Stored on this Mac. Powers mixes and stats."),
            self.toggle("scrobble", "Tell the server what you play", "Sends your plays to your server (scrobbling)."),
        ];
        if self.p.scrobble {
            history.push(self.choice("scrobblePercent", "Count a play after", |v| if v == "100" { "the whole song".into() } else { percent(v) }));
        }
        let online = vec![self.toggle(
            "thirdPartyLookups",
            "Look things up online",
            "Missing lyrics (sends the artist, song and album name) and the AutoEQ headphone list, each with its own switch. Off, nothing is asked.",
        )];
        vec![("Search", search), ("Listening history", history), ("Online", online)]
    }

    fn quality(&self, name: &str, title: &str) -> Row {
        self.choice(name, title, |v| {
            let (rate, format) = v.split_once(':').unwrap_or((v, ""));
            match format {
                "" => "Original".into(),
                "mp3" => format!("MP3 {rate}"),
                "opus" => format!("Opus {rate}"),
                _ => format!("{rate} {format}"),
            }
        })
    }

    fn data(&self) -> Vec<(&'static str, Vec<Row>)> {
        let f = self.f;
        let streaming = vec![self.quality("wifi", "Streaming quality")];
        let downloads = vec![
            self.quality("download", "Quality for downloads"),
            self.choice("parallelDownloads", "Downloads at once", |v| v.into()),
            self.action("Download the whole library", "Every song, at the download quality.".into(), "Download", f.indexed.0 > 0, Chore::DownloadLibrary),
        ];
        let ahead = vec![
            self.choice("precacheWifi", "Load ahead", |v| if v == "1" { "Next song".into() } else { format!("{v} songs") }),
            self.choice("coversAhead", "Load covers ahead", |v| off_or(v, |v| v.into())),
        ];
        let stored = format!(
            "{} streamed · {} covers · {} lyrics · {} in {} · {} library",
            bytes(f.stream_bytes),
            bytes(f.cover_bytes),
            bytes(f.lyrics_bytes),
            bytes(f.download_bytes),
            words::count(f.download_songs, "download", "downloads"),
            bytes(f.database_bytes)
        );
        let storage = vec![
            self.choice("cacheMb", "Space for streamed music", |v| match v.parse::<u32>() {
                Ok(mb) if mb >= 1024 && mb % 1024 == 0 => format!("{} GB", mb / 1024),
                _ => format!("{v} MB"),
            }),
            self.info("Stored on this Mac", stored),
            self.action("Streamed music", "Oldest goes first. Downloads stay.".into(), "Clear", f.stream_bytes > 0, Chore::ClearStream),
            self.action("Covers", "Fetched again when needed.".into(), "Clear", f.cover_bytes > 0, Chore::ClearCovers),
            self.action("Lyrics", format!("{} found online. Looked up again when needed.", bytes(f.lyrics_bytes)), "Clear", f.lyrics_bytes > 0, Chore::ClearLyrics),
        ];
        vec![("Streaming quality", streaming), ("Downloads", downloads), ("Loading ahead", ahead), ("Storage", storage)]
    }

    fn servers(&self) -> Vec<(&'static str, Vec<Row>)> {
        let p = self.p;
        let mut accounts: Vec<Row> = p
            .servers
            .iter()
            .map(|sv| {
                let active = sv.id == p.active_server_id;
                let who = if sv.user.is_empty() { "API key" } else { sv.user.as_str() };
                let mut detail = format!("{who} · {}", if active { "in use" } else { "not in use" });
                if !sv.alt_url.trim().is_empty() {
                    detail.push_str(" · second address");
                }
                Row { kind: SERVER, name: sv.id.clone(), title: nori_core::settings::label(&sv.name, &sv.url), detail, on: active, enabled: true, ..Default::default() }
            })
            .collect();
        accounts.push(Row { kind: BUTTON, name: ADD_SERVER.into(), title: "Add server".into(), enabled: true, ..Default::default() });
        let mut out = vec![("Accounts", accounts)];
        if self.f.folders.len() > 1 {
            let mut o = vec![("All".to_string(), String::new())];
            o.extend(self.f.folders.iter().cloned());
            out.push(("This server", vec![self.choice_of("musicFolder", "Music folder", o)]));
        }
        out
    }
}

/// Display name and description of a lyrics service by core id.
fn service(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "BINILYRICS" => ("BiniLyrics", "Apple Music's lyrics, syllable by syllable, from a volunteer's copy. Unofficial."),
        "BETTER_LYRICS" => ("BetterLyrics", "Apple Music's lyrics, syllable by syllable. Finds more with a key. Unofficial."),
        "PAXSENIX" => ("PaxSenix", "Apple Music's lyrics, syllable by syllable, looked up another way. Unofficial."),
        "LYRICS_PLUS" => ("LyricsPlus", "Apple Music's lyrics and others', syllable by syllable, from volunteers. Unofficial."),
        "PORTATO" => ("BetterLyrics Portato", "QQ Music's lyrics, word by word. Strong on Chinese music. Unofficial."),
        "PAXSENIX_MUSIXMATCH" => ("PaxSenix: Musixmatch", "Musixmatch's lyrics, often word by word. Needs a PaxSenix key."),
        "SIMPMUSIC" => ("SimpMusic", "Lyrics timed by listeners, often word by word, matched to the song on YouTube."),
        "UNISON" => ("Unison", "Open, written and timed by listeners. Many songs word by word."),
        "NETEASE" => ("NetEase Cloud Music", "The Chinese streaming service's own lyrics, often word by word. Unofficial."),
        "KUGOU" => ("KuGou", "Strong on Chinese, Japanese and Korean music, often word by word. Unofficial."),
        "LRCLIB" => ("LRCLIB", "Open and run by volunteers. Timed line by line, some songs word by word."),
        "PAXSENIX_SPOTIFY" => ("PaxSenix: Spotify", "Spotify's lyrics, timed line by line. Needs a PaxSenix key."),
        "YOUTUBE_CAPTIONS" => ("YouTube captions", "The captions of the song on YouTube, timed line by line. Unofficial."),
        "MEGALOBIZ" => ("Megalobiz", "Lyrics timed line by line by its users, read off its pages."),
        "YOUTUBE_MUSIC" => ("YouTube Music", "The words YouTube Music shows for the song. Not timed. Unofficial."),
        "GENIUS" => ("Genius", "The biggest catalogue of words, not timed. Asked only when nobody has timed lyrics."),
        _ => return None,
    })
}

/// Rows of settings tab `tab`: each group's heading, then its rows with first and last marked.
pub fn rows(p: &StoredPrefs, f: &Facts, tab: i32) -> ModelRc<SettingRow> {
    let s = settings_model::state(p, settings_model::Output::default(), &crate::session::app().settings.model);
    let b = Build { p, s: &s, f };
    let groups = match tab {
        1 => b.playing(),
        2 => b.sound(),
        3 => b.lyrics(),
        4 => b.library(),
        5 => b.data(),
        6 => b.servers(),
        _ => b.general(),
    };
    let mut out = Vec::new();
    for (title, rows) in groups {
        out.push(SettingRow { kind: HEADING, title: title.into(), ..Default::default() });
        let n = rows.len();
        for (i, r) in rows.into_iter().enumerate() {
            let v = match Target::of(&r.name) {
                Target::Device => f.device.clone(),
                Target::Setting(name) => b.value(name),
            };
            let chosen = r.options.iter().position(|o| o.1 == v).map_or(-1, |i| i as i32);
            let labels: Vec<SharedString> = r.options.iter().map(|o| o.0.as_str().into()).collect();
            let swatches: Vec<Color> = r.swatches.iter().map(|c| Color::from_argb_encoded(accent_shown(*c))).collect();
            let picked = r.swatches.iter().position(|c| *c == p.accent as u32).map_or(-1, |i| i as i32);
            out.push(SettingRow {
                kind: r.kind,
                name: r.name.into(),
                title: r.title.into(),
                detail: r.detail.into(),
                on: r.on,
                enabled: r.enabled,
                options: ModelRc::new(VecModel::from(labels)),
                chosen: if r.kind == PALETTE { picked } else { chosen },
                button: r.button.into(),
                value: r.value,
                min: r.min,
                max: r.max,
                centred: r.centred,
                swatches: ModelRc::new(VecModel::from(swatches)),
                first: i == 0,
                last: i + 1 == n,
            });
        }
    }
    ModelRc::new(VecModel::from(out))
}

/// The value of option `index` of `target`.
pub fn option_value(target: Target, index: usize, f: &Facts) -> Option<String> {
    match target {
        Target::Device => {
            if index == 0 {
                Some(String::new())
            } else {
                f.devices.get(index - 1).cloned()
            }
        }
        Target::Setting("musicFolder") => {
            if index == 0 {
                Some(String::new())
            } else {
                f.folders.get(index - 1).map(|x| x.1.clone())
            }
        }
        Target::Setting(name) => options(name).into_iter().nth(index),
    }
}

/// The in-place level a slider row edits.
pub fn level_of(name: &str) -> Option<EqLevel> {
    Some(match name {
        "preampDb" => EqLevel::ReplayGainPreamp,
        "bassBoostDb" => EqLevel::BassBoost,
        "virtualizer" => EqLevel::Virtualizer,
        "volumeBoostDb" => EqLevel::VolumeBoost,
        "compThresholdDb" => EqLevel::CompThreshold,
        "compRatio" => EqLevel::CompRatio,
        "compAttackMs" => EqLevel::CompAttack,
        "compReleaseMs" => EqLevel::CompRelease,
        "compMakeupDb" => EqLevel::CompMakeup,
        "compKneeDb" => EqLevel::CompKnee,
        "expThresholdDb" => EqLevel::ExpThreshold,
        "expRatio" => EqLevel::ExpRatio,
        "expAttackMs" => EqLevel::ExpAttack,
        "expReleaseMs" => EqLevel::ExpRelease,
        "eqPreamp" => EqLevel::Preamp,
        "balance" => EqLevel::Balance,
        "limiter" => EqLevel::Limiter,
        "crossfeed" => EqLevel::Crossfeed,
        "crossfeedCut" => EqLevel::CrossfeedCut,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_names_resolve() {
        let p = StoredPrefs { auto_mix: true, auto_fill: true, compressor: true, expander: true, loudness: true, scrobble: true, ..StoredPrefs::default() };
        let f = Facts::default();
        let s = settings_model::state(&p, settings_model::Output::default(), &crate::session::app().settings.model);
        let b = Build { p: &p, s: &s, f: &f };
        let specs: Vec<String> = settings_model::specs().into_iter().map(|s| s.name).collect();
        for (_, rows) in [b.general(), b.playing(), b.sound(), b.lyrics(), b.library(), b.data(), b.servers()].into_iter().flatten() {
            for r in rows.iter().filter(|r| matches!(r.kind, TOGGLE | CHOICE | TEXT) && r.name != DEVICE) {
                assert!(specs.contains(&r.name), "{} is not a core setting", r.name);
            }
        }
        for c in CHORES {
            assert_eq!(Act::of(chore_name(c)), Some(Act::Chore(c)));
        }
        assert_eq!(Act::of(EQUALIZER), Some(Act::Equalizer));
        assert_eq!(Act::of(ADD_SERVER), Some(Act::AddServer));
        assert_eq!(Act::of("server:7"), Some(Act::Server("7".into())));
        for r in b.sound().into_iter().flat_map(|g| g.1) {
            if r.kind == SLIDER {
                assert!(level_of(&r.name).is_some(), "{}", r.name);
            }
        }
    }
}
