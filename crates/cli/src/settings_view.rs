//! The settings page (one scrolling page of groups, sections and rows) and the equalizer's rows.
//! Values and options come from the core's `settings_model`; rows send back a setting name and value
//! (`setting_set`) or a level edit (`edit_level`). Phone-only settings are left out (core defaults apply).

use crate::text;
use nori_core::settings::{EqLevel, EqMode, SoundBand, StoredPrefs, EQ_RANGES};
use nori_core::settings_model::{self, BeatModel, LyricsSource, SettingsState};
use nori_core::MusicFolder;

use crate::app::{Cmd, Overlay, Sel, SoundToolCmd, Target, View};

/// The terminal client's own group id.
pub const OWN: &str = "terminal";

/// What opening a row does.
pub enum Opened {
    Cmds(Vec<Cmd>),
    Overlay(Overlay),
    View(View),
    /// A terminal-only switch (never `Switch::Setting`).
    Own(Switch),
    Login,
}

/// What a toggle row switches: a core setting by name, or a terminal-only switch.
#[derive(Debug, Clone, PartialEq)]
pub enum Switch {
    Setting(String),
    Mouse,
    Images,
    CardCovers,
}

/// A settings group: a headed part of the page.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub id: &'static str,
    pub title: &'static str,
}

/// The groups in page order.
pub const GROUPS: [Group; 8] = [
    Group { id: OWN, title: "Interface" },
    Group { id: "sound", title: "Sound" },
    Group { id: "playback", title: "Playback" },
    Group { id: "library", title: "Library" },
    Group { id: "lyrics", title: "Lyrics" },
    Group { id: "server", title: "Server" },
    Group { id: "storage", title: "Storage" },
    Group { id: "about", title: "About" },
];

/// What a link or button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Equalizer,
    Downloads,
    /// The login, for another server.
    AddServer,
    Chore(Chore),
}

pub use nori_host::session::Chore;

/// One row of a page.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Toggle { switch: Switch, title: String, detail: String, on: bool, enabled: bool },
    /// `options` are (label, value); `shown` is the chosen label, or the raw value if none matches.
    Choice { target: Target, title: String, options: Vec<(String, String)>, shown: String, enabled: bool },
    Note { text: String },
    /// Opens another screen, with a status at the end.
    Link { title: String, status: String, action: Act },
    /// Text with a button at the end.
    Action { title: String, detail: String, button: String, enabled: bool, action: Act },
    Info { title: String, detail: String },
    /// With `level`, changed through `edit_level` instead of by `name`.
    Slider { name: String, label: String, value: f32, min: f32, max: f32, centred: bool, level: Option<EqLevel> },
    /// Colour swatches, ARGB.
    Palette { name: String, colours: Vec<i64>, chosen: i64 },
    Server { id: String, label: String, detail: String, active: bool },
    Button { title: String, action: Act },
    /// A lyrics service in the ranked list: enter switches it, ← → move it.
    Ranked { name: String, id: String, title: String, detail: String, on: bool },
    /// Free text (a service's API key).
    Text { name: String, title: String, detail: String, value: String, secret: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: String,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub title: String,
    pub sections: Vec<Section>,
}

/// Local storage use in bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Storage {
    pub stream: i64,
    pub covers: i64,
    pub lyrics: i64,
    pub downloads: i64,
    pub download_songs: u32,
    pub database: i64,
}

/// Non-setting facts shown on the page, loaded when Settings opens.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// Songs AutoMix has measured.
    pub analysed: u32,
    /// Offline index counts: songs, albums, artists.
    pub indexed: (u32, u32, u32),
    pub storage: Storage,
    /// The active server's music folders.
    pub folders: Vec<MusicFolder>,
    /// Output device names.
    pub devices: Vec<String>,
}

/// A page line: group heading, section title or row.
pub enum Line<'a> {
    Group(&'a str),
    Title(&'a str),
    Row(&'a Row),
}

#[derive(Default)]
pub struct SettingsView {
    pub row: Sel,
    /// Cached group pages; rebuilt after [`SettingsView::invalidate`].
    pages: Option<Vec<Page>>,
    pub facts: Facts,
    pub facts_asked: bool,
    /// State shown by the terminal's own rows.
    pub own: Own,
}

#[derive(Default, Clone)]
pub struct Own {
    pub mouse: bool,
    pub images: bool,
    pub card_covers: bool,
    pub protocol: String,
    pub data: String,
    /// Output device for the next start; empty for the system default.
    pub device: String,
}

impl SettingsView {
    pub fn invalidate(&mut self) {
        self.pages = None;
    }

    pub fn set_facts(&mut self, f: Facts) {
        self.facts = f;
        self.invalidate();
    }

    /// Every group's page, built if not cached.
    pub fn pages(&mut self, prefs: &StoredPrefs) -> &[Page] {
        if self.pages.is_none() {
            let state = settings_model::state(prefs, settings_model::Output::default(), &crate::backend::app().settings.model, &crate::backend::app().settings.sing_model);
            self.pages = Some(GROUPS.iter().map(|g| page(g.id, prefs, &state, &self.facts, &self.own)).collect());
        }
        self.pages.as_deref().expect("made above")
    }

    /// All lines as drawn.
    pub fn lines(pages: &[Page]) -> Vec<Line<'_>> {
        let mut out = Vec::new();
        for p in pages {
            out.push(Line::Group(&p.title));
            for s in &p.sections {
                if !s.title.is_empty() {
                    out.push(Line::Title(&s.title));
                }
                out.extend(s.rows.iter().map(Line::Row));
            }
        }
        out
    }

    /// The selection and line count, for list navigation.
    pub fn list(&mut self) -> (&mut Sel, usize) {
        let n = self.pages.as_deref().map_or(0, |p| Self::lines(p).len());
        (&mut self.row, n)
    }

    /// Index in `GROUPS` of the selected line's group.
    pub fn group_at(&self) -> usize {
        let Some(pages) = self.pages.as_deref() else { return 0 };
        Self::lines(pages).iter().take(self.row.at + 1).filter(|l| matches!(l, Line::Group(_))).count().saturating_sub(1)
    }

    /// Selects a group's first row.
    pub fn jump(&mut self, group: usize) {
        let Some(pages) = self.pages.as_deref() else { return };
        let lines = Self::lines(pages);
        if let Some(at) = lines.iter().enumerate().filter(|(_, l)| matches!(l, Line::Group(_))).nth(group).map(|(i, _)| i) {
            self.row.at = at;
            self.row.top = at;
            self.skip_titles(true);
        }
    }

    /// The selected row, if a row (not a heading) is selected.
    fn selected(&self) -> Option<&Row> {
        match Self::lines(self.pages.as_deref()?).into_iter().nth(self.row.at)? {
            Line::Row(r) => Some(r),
            _ => None,
        }
    }

    /// Whether a single click activates the row rather than only selecting it.
    pub fn clicks_open(&self) -> bool {
        matches!(self.selected(), Some(Row::Toggle { .. } | Row::Button { .. } | Row::Link { .. } | Row::Action { .. } | Row::Server { .. } | Row::Ranked { .. }))
    }

    /// Enter on the selection.
    pub fn open(&mut self) -> Option<Opened> {
        let row = self.selected()?.clone();
        Some(match row {
            Row::Toggle { switch: Switch::Setting(name), on, enabled, .. } => {
                if !enabled {
                    return None;
                }
                Opened::Cmds(vec![Cmd::Setting(name, (!on).to_string())])
            }
            Row::Toggle { switch, .. } => Opened::Own(switch),
            Row::Choice { target, title, options, shown, enabled: true } => {
                let at = options.iter().position(|o| o.0 == shown).unwrap_or(0);
                Opened::Overlay(Overlay::Picker { title, options, sel: Sel { at, top: 0 }, target })
            }
            Row::Link { action, .. } | Row::Action { action, enabled: true, .. } | Row::Button { action, .. } => match action {
                Act::Equalizer => Opened::View(View::Equalizer),
                Act::Downloads => Opened::View(View::Downloads),
                Act::AddServer => Opened::Login,
                Act::Chore(c) => Opened::Cmds(vec![Cmd::Action(c)]),
            },
            Row::Server { id, active, .. } => {
                if active {
                    return None;
                }
                Opened::Cmds(vec![Cmd::SwitchServer(id)])
            }
            Row::Ranked { name, on, .. } => Opened::Cmds(vec![Cmd::Setting(name, (!on).to_string())]),
            Row::Text { name, title, value, secret, .. } => Opened::Overlay(Overlay::Input { title, text: value, secret, name }),
            Row::Palette { name, colours, chosen } => {
                let at = colours.iter().position(|c| *c == chosen).map_or(0, |i| (i + 1) % colours.len());
                Opened::Cmds(vec![Cmd::Setting(name, colours[at].to_string())])
            }
            _ => return None,
        })
    }

    /// ← or → on the selection: previous or next option, off or on, a slider step.
    pub fn step(&mut self, up: bool) -> Vec<Cmd> {
        let Some(row) = self.selected().cloned() else { return Vec::new() };
        match row {
            Row::Choice { target, options, shown, enabled: true, .. } => {
                let at = options.iter().position(|o| o.0 == shown);
                let to = match at {
                    Some(i) if up => (i + 1).min(options.len() - 1),
                    Some(i) => i.saturating_sub(1),
                    None => 0,
                };
                if Some(to) == at || options.is_empty() {
                    return Vec::new();
                }
                target.cmd(options[to].1.clone()).into_iter().collect()
            }
            Row::Toggle { switch, on, enabled, .. } => {
                if on == up {
                    return Vec::new();
                }
                match switch {
                    Switch::Setting(name) if enabled => vec![Cmd::Setting(name, up.to_string())],
                    Switch::Setting(_) => Vec::new(),
                    Switch::Mouse => vec![Cmd::Mouse(up)],
                    Switch::Images => vec![Cmd::Images(up)],
                    Switch::CardCovers => vec![Cmd::CardCovers(up)],
                }
            }
            Row::Slider { name, value, min, max, level, .. } => {
                let step = slider_step(min, max);
                let v = (value + if up { step } else { -step }).clamp(min, max);
                match level {
                    Some(l) => vec![Cmd::Level(l, v)],
                    None => vec![Cmd::Setting(name, v.to_string())],
                }
            }
            Row::Palette { name, colours, chosen } => {
                let at = colours.iter().position(|c| *c == chosen).unwrap_or(0) as isize + if up { 1 } else { -1 };
                let at = at.rem_euclid(colours.len() as isize) as usize;
                vec![Cmd::Setting(name, colours[at].to_string())]
            }
            Row::Ranked { id, .. } => vec![Cmd::Setting("lyricsMove".into(), format!("{id}:{}", if up { 1 } else { -1 }))],
            _ => Vec::new(),
        }
    }

    /// Moves the selection off a heading onto a row, preferring direction `down`.
    pub fn skip_titles(&mut self, down: bool) {
        let Some(pages) = &self.pages else { return };
        let lines = Self::lines(pages);
        let title = |i: usize| matches!(lines.get(i), Some(Line::Title(_) | Line::Group(_)));
        let mut at = self.row.at;
        while title(at) && at + 1 < lines.len() && down {
            at += 1;
        }
        while title(at) && at > 0 && !down {
            at -= 1;
        }
        // Stuck on a heading at an end: go the other way.
        while title(at) && at + 1 < lines.len() {
            at += 1;
        }
        self.row.at = at;
    }
}

/// Slider step: 0.5 for wide (dB) ranges, else a fortieth of the range.
pub fn slider_step(min: f32, max: f32) -> f32 {
    let span = max - min;
    if span >= 8.0 {
        0.5
    } else {
        (span / 40.0).max(0.01)
    }
}

// ---- the pages ----

/// Row builders over the settings and the core's model state.
struct Build<'a> {
    p: &'a StoredPrefs,
    s: &'a SettingsState,
}

impl Build<'_> {
    fn value(&self, name: &str) -> String {
        self.s.values.get(name).cloned().unwrap_or_default()
    }

    fn on(&self, name: &str) -> bool {
        self.value(name) == "true"
    }

    fn toggle(&self, name: &str, title: &str, detail: &str) -> Row {
        self.toggle_if(name, title, detail, true)
    }

    fn toggle_if(&self, name: &str, title: &str, detail: &str, enabled: bool) -> Row {
        Row::Toggle { switch: Switch::Setting(name.into()), title: title.into(), detail: detail.into(), on: self.on(name), enabled }
    }

    /// The core's options for `name`, each worded by `label`.
    fn choice(&self, name: &str, title: &str, enabled: bool, label: impl Fn(&str) -> String) -> Row {
        let options: Vec<(String, String)> = options(name).into_iter().map(|v| (label(&v), v)).collect();
        self.choice_of(name, title, options, enabled)
    }

    fn choice_of(&self, name: &str, title: &str, options: Vec<(String, String)>, enabled: bool) -> Row {
        let v = self.value(name);
        let shown = options.iter().find(|o| o.1 == v).map_or(v, |o| o.0.clone());
        Row::Choice { target: Target::Setting(name.into()), title: title.into(), options, shown, enabled }
    }

    /// An enum setting; `labels` word its options in order.
    fn named(&self, name: &str, title: &str, labels: &[&str]) -> Row {
        let names = options(name);
        self.choice(name, title, true, |v| names.iter().position(|n| n == v).and_then(|i| labels.get(i)).map_or_else(|| v.to_string(), |l| l.to_string()))
    }
}

fn options(name: &str) -> Vec<String> {
    settings_model::specs().into_iter().find(|s| s.name == name).map(|s| s.options).unwrap_or_default()
}

fn section(title: &str, rows: Vec<Row>) -> Section {
    Section { title: title.into(), rows }
}

fn info(title: &str, detail: String) -> Row {
    Row::Info { title: title.into(), detail }
}

fn action(title: &str, detail: String, button: &str, enabled: bool, chore: Chore) -> Row {
    Row::Action { title: title.into(), detail, button: button.into(), enabled, action: Act::Chore(chore) }
}

fn off_or(v: &str, words: impl Fn(&str) -> String) -> String {
    if v == "0" { "off".into() } else { words(v) }
}

/// One group's page.
pub fn page(id: &str, p: &StoredPrefs, s: &SettingsState, f: &Facts, own: &Own) -> Page {
    let b = Build { p, s };
    let title = GROUPS.iter().find(|g| g.id == id).map_or(id, |g| g.title).to_string();
    let sections = match id {
        OWN => own_page(&b, own, f),
        "sound" => sound(&b),
        "playback" => playback(&b, f),
        "library" => library(&b, f),
        "lyrics" => lyrics(&b),
        "server" => server(&b, f),
        "storage" => storage(&b, f),
        "about" => about(own),
        _ => Vec::new(),
    };
    Page { title, sections }
}

/// The terminal's own group.
fn own_page(b: &Build, o: &Own, f: &Facts) -> Vec<Section> {
    let toggle = |switch, title: &str, detail: &str, on, enabled| Row::Toggle { switch, title: title.into(), detail: detail.into(), on, enabled };
    let mut devices = vec![("system default".to_string(), String::new())];
    devices.extend(f.devices.iter().map(|d| (d.clone(), d.clone())));
    if !o.device.is_empty() && !f.devices.contains(&o.device) {
        devices.push((format!("{} (not found)", o.device), o.device.clone()));
    }
    let shown = devices.iter().find(|d| d.1 == o.device).map_or_else(|| o.device.clone(), |d| d.0.clone());
    let rows = vec![
        toggle(Switch::Mouse, "Mouse", "Clicks, the wheel and dragging the bars. Off, the terminal selects text (m)", o.mouse, true),
        toggle(Switch::Images, "Covers", "Album art in the player and on album pages (I)", o.images, true),
        toggle(Switch::CardCovers, "Covers on album cards", "Small pictures on Home and Albums; off keeps a slow link or terminal light", o.card_covers, o.images),
        Row::Choice { target: Target::Device, title: "Output device".into(), options: devices, shown, enabled: true },
        Row::Note { text: "The device is opened at start; --device overrides it for one run.".into() },
    ];
    let accents: Vec<i64> = options("accent").iter().filter_map(|c| c.parse().ok()).collect();
    let look = vec![
        b.toggle("coverColors", "Colors from the cover", "The page takes the playing cover's colors (with covers on)"),
        Row::Palette { name: "accent".into(), colours: accents, chosen: b.p.accent },
    ];
    vec![section("", rows), section("Look", look)]
}

fn sound(b: &Build) -> Vec<Section> {
    let p = b.p;
    let s = b.s;
    let live = !s.untouched;
    let eq_status = if s.untouched { "bypassed" } else if s.sound_chain_on { "on" } else { "off" };
    let eq_status = if p.sound_bypass { "no processing" } else { eq_status };
    let eq = vec![Row::Link { title: "Equalizer, crossfeed, balance, limiter".into(), status: eq_status.into(), action: Act::Equalizer }];

    let mut levelling = vec![b.named("replayGain", "ReplayGain", &["off", "track", "album", "auto"])];
    if p.replay_gain != nori_core::settings::GainMode::Off {
        let r = EQ_RANGES.replay_gain_preamp;
        levelling.push(Row::Slider {
            name: "preampDb".into(),
            label: format!("Pre-amp {} dB", text::signed_db(p.preamp_db)),
            value: p.preamp_db,
            min: r.min,
            max: r.max,
            centred: true,
            level: Some(EqLevel::ReplayGainPreamp),
        });
        levelling.push(b.choice("loudnessTarget", "Loudness target", true, |v| format!("{v} LUFS")));
        levelling.push(b.choice("gainBoostDb", "Turn quiet songs up", true, |v| off_or(v, |v| format!("up to +{v} dB"))));
        levelling.push(b.choice("untaggedGainDb", "Untagged files", true, |v| format!("{v} dB")));
        levelling.push(b.toggle("gainMeasured", "Measure untagged files", "Play songs without tags at the loudness AutoMix measured"));
    }

    let mut mixing = Vec::new();
    if s.untouched {
        mixing.push(Row::Note { text: "Bit-exact output is on: no mixing, skipping or effects.".into() });
    }
    if !p.auto_mix {
        mixing.push(b.choice("crossfadeSec", "Crossfade", live, |v| off_or(v, |v| format!("{v} s"))));
        if p.crossfade_sec > 0 {
            mixing.push(b.named("crossfadeCurve", "  Curve", &["equal power", "linear", "S-curve"]));
            let part = |v: &str| if v == "0" { "whole crossfade".to_string() } else { format!("{v} s") };
            mixing.push(b.choice("crossfadeInSec", "  Fade in over", live, part));
            mixing.push(b.choice("crossfadeOutSec", "  Fade out over", live, part));
        }
    }
    mixing.push(b.toggle_if("autoMix", "AutoMix", "Beat-matched transitions planned per song pair", live));
    if p.auto_mix {
        mixing.push(b.choice("autoMixMaxS", "  Longest mix", live, |v| format!("{v} s")));
        mixing.push(b.toggle_if("autoMixBeatMatch", "  Beat-match", "Stretch the incoming song so the beats line up", live));
        if p.auto_mix_beat_match {
            mixing.push(b.choice("autoMixMaxTempoPct", "    Max stretch", live, |v| format!("±{v} %")));
            mixing.push(b.toggle_if("autoMixKeepPitch", "    Keep pitch", "Off: pitch follows the stretch (≤ 2 %)", live));
        }
        mixing.push(b.toggle_if("autoMixBassSwap", "  Bass swap", "Hand the low end over at the drop", live));
        mixing.push(b.toggle_if("autoMixFilters", "  Filter out", "Low-pass the outgoing song", live));
        mixing.push(b.toggle_if("autoMixEchoOut", "  Echo out", "Cut clashing vocals with an echo", live));
        if s.beat_model != BeatModel::Unavailable {
            let state = match &s.beat_model {
                BeatModel::Ready => "model ready".to_string(),
                BeatModel::Downloading => "downloading".to_string(),
                BeatModel::WaitingForWifi => "waiting for Wi-Fi".to_string(),
                BeatModel::Failed { why } => format!("download failed: {}", crate::text::beat_failure(*why)),
                _ => format!("{} MB model, fetched on first use", s.beat_model_mb),
            };
            mixing.push(b.toggle_if("autoMixBetterBeats", "  Neural beat tracking", &format!("Beat This! ({state})"), live));
        }
    }
    mixing.push(b.toggle_if("crossfadeKeepAlbums", "Gapless albums", "Never mix songs of the same album", live));
    mixing.push(b.choice("fadeMs", "Fade on play/pause", true, |v| off_or(v, |v| format!("{v} ms"))));

    let mut tempo = vec![
        b.choice("speed", "Speed", true, |v| format!("{v}×")),
        b.choice("pitch", "Pitch", true, |v| format!("{v}×")),
        b.toggle_if("skipSilence", "Skip silence", "Shorten quiet gaps", live),
    ];
    if s.sing_model != BeatModel::Unavailable {
        tempo.push(b.toggle_if("sing", "Sing", &format!("Turn the vocals down (Open-Unmix, a {} MB model fetched on first use)", s.sing_model_mb), live));
        if p.sing {
            let label = format!("  Vocals {:.0} %", p.sing_vocal_level * 100.0);
            tempo.push(Row::Slider { name: "singVocalLevel".into(), label, value: p.sing_vocal_level, min: 0.0, max: 1.0, centred: false, level: None });
        }
    }
    let output = vec![
        b.toggle("hiRes", "High quality output", "Float samples to the device; 24-bit files kept whole, effects in float"),
        b.named("maxRate", "Highest sample rate", &["each song's own", "48 kHz", "96 kHz", "192 kHz"]),
    ];
    vec![section("Equalizer", eq), section("Effects", effects(b)), section("Levelling", levelling), section("Transitions", mixing), section("Tempo", tempo), section("Output", output)]
}

/// The effects section: boosts, virtualizer, compressor, loudness and noise gate.
fn effects(b: &Build) -> Vec<Row> {
    let p = b.p;
    let slider = |name: &str, label: String, value: f32, (min, max): (f32, f32), level: EqLevel| Row::Slider { name: name.into(), label, value, min, max, centred: false, level: Some(level) };
    let db_or_off = |v: f32| if v > 0.0 { format!("+{v:.1} dB") } else { "off".into() };
    let mut rows = vec![
        slider("bassBoostDb", format!("Bass boost {}", db_or_off(p.bass_boost_db)), p.bass_boost_db, (0.0, 12.0), EqLevel::BassBoost),
        slider(
            "virtualizer",
            format!("Virtualizer {}", if p.virtualizer > 0.0 { format!("{:.0} %", p.virtualizer * 100.0) } else { "off".into() }),
            p.virtualizer,
            (0.0, 1.0),
            EqLevel::Virtualizer,
        ),
        slider("volumeBoostDb", format!("Volume boost {}", db_or_off(p.volume_boost_db)), p.volume_boost_db, (0.0, 12.0), EqLevel::VolumeBoost),
        Row::Note { text: "Boosts bring the limiter with them, so nothing clips.".into() },
        b.toggle("compressor", "Compressor", "Evens out loud and quiet passages"),
    ];
    if p.compressor {
        rows.push(b.choice("compressorPreset", "  Preset", true, |v| match v {
            "GENTLE" => "gentle".into(),
            "BALANCED" => "balanced".into(),
            "STRONG" => "strong".into(),
            _ => "custom".into(),
        }));
        rows.extend([
            slider("compThresholdDb", format!("  Threshold {:.1} dB", p.comp_threshold_db), p.comp_threshold_db, (-60.0, 0.0), EqLevel::CompThreshold),
            slider("compRatio", format!("  Ratio {:.1}:1", p.comp_ratio), p.comp_ratio, (1.0, 20.0), EqLevel::CompRatio),
            slider("compAttackMs", format!("  Attack {:.1} ms", p.comp_attack_ms), p.comp_attack_ms, (0.1, 200.0), EqLevel::CompAttack),
            slider("compReleaseMs", format!("  Release {:.0} ms", p.comp_release_ms), p.comp_release_ms, (10.0, 2000.0), EqLevel::CompRelease),
            slider("compMakeupDb", format!("  Make-up +{:.1} dB", p.comp_makeup_db), p.comp_makeup_db, (0.0, 24.0), EqLevel::CompMakeup),
            slider("compKneeDb", format!("  Knee {:.1} dB", p.comp_knee_db), p.comp_knee_db, (0.0, 24.0), EqLevel::CompKnee),
        ]);
    }
    rows.push(b.toggle("loudness", "Loudness compensation", "Turned down, the bass comes up as the ear needs (ISO 226); follows this client's volume"));
    if p.loudness {
        rows.push(b.choice("loudnessRefPhon", "  Balanced at", true, |v| format!("{v} phon")));
    }
    rows.push(b.toggle("expander", "Noise gate", "A downward expander: hiss and hum go further down in quiet parts"));
    if p.expander {
        rows.extend([
            slider("expThresholdDb", format!("  Threshold {:.1} dB", p.exp_threshold_db), p.exp_threshold_db, (-90.0, -10.0), EqLevel::ExpThreshold),
            slider("expRatio", format!("  Ratio 1:{:.1}", p.exp_ratio), p.exp_ratio, (1.0, 20.0), EqLevel::ExpRatio),
            slider("expAttackMs", format!("  Attack {:.1} ms", p.exp_attack_ms), p.exp_attack_ms, (0.1, 100.0), EqLevel::ExpAttack),
            slider("expReleaseMs", format!("  Release {:.0} ms", p.exp_release_ms), p.exp_release_ms, (10.0, 2000.0), EqLevel::ExpRelease),
        ]);
    }
    rows
}

fn playback(b: &Build, f: &Facts) -> Vec<Section> {
    let p = b.p;
    let mut queue = vec![
        b.toggle("previousAlwaysSkips", "Previous always skips", "Never restart the current song"),
        b.toggle("skipExplicit", "Skip explicit", "Songs the server tags explicit"),
        b.toggle("autoFill", "Autoplay", "Keep adding music when the queue runs out"),
    ];
    if p.auto_fill {
        queue.push(b.named("autoFillKind", "  Add", &["songs", "albums"]));
        queue.push(b.named("autoFillBasis", "  Picked by", &["similarity", "artist", "genre", "era"]));
        queue.push(b.toggle("autoFillRemote", "  Include remote songs", "Not in the library yet; each one played is downloaded to it"));
    }
    let errors = vec![
        b.toggle("skipOnError", "Skip unplayable songs", "Up to three in a row"),
        b.toggle("bridgeOffline", "Offline fallback", "Play downloads while the server is unreachable"),
    ];
    let analysis = vec![action("Measured songs", format!("{} songs with tempo and beats", f.analysed), "Forget", f.analysed > 0, Chore::MeasureAgain)];
    vec![section("Queue", queue), section("Errors", errors), section("Analysis", analysis)]
}

fn library(b: &Build, f: &Facts) -> Vec<Section> {
    let (songs, albums, artists) = f.indexed;
    let index = vec![action("Offline index", format!("{songs} songs, {albums} albums, {artists} artists"), "Update", true, Chore::SyncLibrary)];
    let mut history = vec![
        b.toggle("tasteModel", "Listening history", "Kept locally; feeds mixes and stats"),
        b.toggle("scrobble", "Scrobble", "Report plays to the server"),
    ];
    if b.p.scrobble {
        history.push(b.choice("scrobblePercent", "  Scrobble at", true, |v| format!("{v} %")));
    }
    let online = vec![b.toggle("thirdPartyLookups", "Third-party lookups", "Lyrics services and the AutoEQ list; off, nothing leaves for anyone but your server")];
    let devices = vec![b.toggle("remoteControl", "Remote control", "Your other devices with nori control what plays here: on this network directly, elsewhere through octo-fiesta")];
    vec![section("Index and search", index), section("History", history), section("Online", online), section("Other devices", devices)]
}

/// A lyrics service's name and description by its core id.
fn service(id: &str) -> (&'static str, &'static str) {
    match id {
        "PAXSENIX" => ("PaxSenix", "Apple Music, syllable-timed · unofficial"),
        "BINILYRICS" => ("BiniLyrics", "Apple Music, syllable-timed · unofficial"),
        "UNISON" => ("Unison", "open, community-timed"),
        "BETTER_LYRICS" => ("BetterLyrics", "Apple Music, syllable-timed · key finds more"),
        "KUGOU" => ("KuGou", "word-timed, CJK · unofficial"),
        "NETEASE" => ("NetEase", "word-timed, Chinese · unofficial"),
        "LYRICS_PLUS" => ("LyricsPlus", "YouLy+, syllable-timed · unofficial"),
        "SIMPMUSIC" => ("SimpMusic", "community-timed, via YouTube"),
        "PORTATO" => ("BetterLyrics Portato", "QQ Music, word-timed · unofficial"),
        "PAXSENIX_MUSIXMATCH" => ("PaxSenix Musixmatch", "word-timed · needs a PaxSenix key"),
        "LRCLIB" => ("LRCLIB", "open, line-timed"),
        "PAXSENIX_SPOTIFY" => ("PaxSenix Spotify", "line-timed · needs a PaxSenix key"),
        "YOUTUBE_CAPTIONS" => ("YouTube captions", "line-timed · unofficial"),
        "MEGALOBIZ" => ("Megalobiz", "line-timed, scraped"),
        "YOUTUBE_MUSIC" => ("YouTube Music", "untimed · unofficial"),
        "GENIUS" => ("Genius", "untimed, asked last"),
        _ => ("?", ""),
    }
}

fn lyrics(b: &Build) -> Vec<Section> {
    let display = vec![
        b.toggle("lyricsSweep", "Word fill", "Color words in as they are sung (word-timed lyrics)"),
        b.toggle("lyricsTranslation", "Translations", "When the server has them"),
    ];
    let online = b.on("lyricsOnline");
    let mut lookup = vec![b.toggle("lyricsOnline", "Online lookups", "When the server has no timed lyrics; sends artist, title and album")];
    if !online {
        return vec![section("Display", display), section("Online", lookup)];
    }
    lookup.push(b.toggle("lyricsPreferWords", "Prefer word timing", "Keep asking past line-timed answers"));
    let mut sources: Vec<Row> = b
        .s
        .lyrics_sources
        .iter()
        .map(|LyricsSource { id, on, .. }| {
            let (title, detail) = service(id);
            Row::Ranked { name: format!("lyricsService:{id}"), id: id.clone(), title: title.into(), detail: detail.into(), on: *on }
        })
        .collect();
    sources.push(Row::Note { text: "Enter switches a source; ← → move it. The best-scored answer wins; order breaks near ties.".into() });
    let keys = vec![
        Row::Text { name: "paxSenixKey".into(), title: "PaxSenix key".into(), detail: "For Spotify and Musixmatch".into(), value: b.value("paxSenixKey"), secret: true },
        Row::Text { name: "betterLyricsKey".into(), title: "BetterLyrics key".into(), detail: "For songs it has not cached".into(), value: b.value("betterLyricsKey"), secret: true },
    ];
    vec![section("Display", display), section("Online", lookup), section("Sources, in order", sources), section("Keys", keys)]
}

fn server(b: &Build, f: &Facts) -> Vec<Section> {
    let p = b.p;
    let mut accounts: Vec<Row> = p
        .servers
        .iter()
        .map(|s| {
            let active = s.id == p.active_server_id;
            let who = if s.user.is_empty() { "API key" } else { s.user.as_str() };
            let mut detail = format!("{who} · {}", s.url);
            if s.wifi_only {
                detail.push_str(" · Wi-Fi only");
            }
            Row::Server { id: s.id.clone(), label: nori_core::settings::label(&s.name, &s.url), detail, active }
        })
        .collect();
    accounts.push(Row::Button { title: "Add a server".into(), action: Act::AddServer });
    let mut out = vec![section("Accounts", accounts)];
    let mut this = Vec::new();
    if f.folders.len() > 1 {
        let mut o = vec![("all".to_string(), String::new())];
        o.extend(f.folders.iter().map(|m| (m.name.clone(), m.id.clone())));
        this.push(b.choice_of("musicFolder", "Music folder", o, true));
    }
    if !this.is_empty() {
        out.push(section("This server", this));
    }
    out
}

/// A stream quality value ("320:mp3") in words.
fn quality(v: &str) -> String {
    match v.split_once(':') {
        Some((_, "")) | None => "original".into(),
        Some((rate, format)) => format!("{format} {rate}k"),
    }
}

fn storage(b: &Build, f: &Facts) -> Vec<Section> {
    let s = &f.storage;
    let bytes = text::bytes;
    let quality_rows = vec![
        b.choice("wifi", "Streaming quality", true, quality),
        b.choice("download", "Download quality", true, quality),
        b.choice("parallelDownloads", "Parallel downloads", true, |v| v.to_string()),
        action("Download everything", "Every indexed song".into(), "Download", f.indexed.0 > 0, Chore::DownloadLibrary),
        Row::Link { title: "Downloads".into(), status: format!("{} songs, {}", s.download_songs, bytes(s.downloads)), action: Act::Downloads },
    ];
    let cache = vec![
        b.choice("cacheMb", "Stream cache limit", true, |v| match v.parse::<i32>() {
            Ok(mb) if mb % 1024 == 0 => format!("{} GB", mb / 1024),
            _ => format!("{v} MB"),
        }),
        action("Stream cache", bytes(s.stream), "Clear", s.stream > 0, Chore::ClearStream),
        action("Cover cache", bytes(s.covers), "Clear", s.covers > 0, Chore::ClearCovers),
        action("Lyrics cache", format!("{} of lyrics found online", bytes(s.lyrics)), "Clear", s.lyrics > 0, Chore::ClearLyrics),
        info("Database", bytes(s.database)),
    ];
    vec![section("Quality and downloads", quality_rows), section("Cache", cache)]
}

/// Version and credits.
fn about(own: &Own) -> Vec<Section> {
    let version = Section {
        title: "nori".into(),
        rows: vec![
            info("Version", env!("CARGO_PKG_VERSION").into()),
            info("Terminal client", "ratatui over crossterm; covers through ratatui-image".into()),
            info("Pictures drawn with", own.protocol.clone()),
            info("Data kept in", own.data.clone()),
        ],
    };
    let credits = nori_core::credits::core_credits().into_iter().map(|c| info(&c.name, format!("{} · {} · {}", c.what, c.copyright, c.licence))).collect();
    let tui = vec![
        info("ratatui, crossterm", "The screen and the keys · The Ratatui developers, Timon Post · MIT".into()),
        info("ratatui-image", "Covers in the terminal: kitty, sixel, iTerm2, half blocks · Benjamin Große · MIT".into()),
        info("image, icy_sixel", "Pictures handed to the terminal · The image-rs developers, Mike Krüger · MIT or Apache-2.0".into()),
    ];
    vec![version, section("The core", credits), section("The terminal client", tui)]
}

/// The value shown at a row's end (toggles are drawn by the ui).
pub fn row_value(row: &Row) -> String {
    match row {
        Row::Choice { shown, .. } => format!("‹ {shown} ›"),
        Row::Link { status, .. } => format!("{status} ›"),
        Row::Action { button, .. } => format!("[ {button} ]"),
        Row::Button { title, .. } => format!("[ {title} ]"),
        Row::Server { active, .. } => (if *active { "● in use" } else { "" }).into(),
        Row::Text { value, secret, .. } => {
            if value.is_empty() {
                "none".into()
            } else if *secret {
                "••••••".into()
            } else {
                value.clone()
            }
        }
        _ => String::new(),
    }
}

/// A row's title and detail.
pub fn row_words(row: &Row) -> (String, String) {
    match row {
        Row::Toggle { title, detail, .. } | Row::Ranked { title, detail, .. } | Row::Text { title, detail, .. } | Row::Info { title, detail, .. } => (title.clone(), detail.clone()),
        Row::Choice { title, .. } | Row::Link { title, .. } => (title.clone(), String::new()),
        Row::Note { text } => (String::new(), text.clone()),
        Row::Action { title, detail, .. } => (title.clone(), detail.clone()),
        Row::Slider { label, .. } => (label.clone(), String::new()),
        Row::Palette { .. } => ("Accent color".into(), String::new()),
        Row::Server { label, detail, .. } => (label.clone(), detail.clone()),
        Row::Button { .. } => (String::new(), String::new()),
    }
}

/// Whether the row is live; inactive settings are drawn dimmed.
pub fn row_enabled(row: &Row) -> bool {
    match row {
        Row::Toggle { enabled, .. } | Row::Choice { enabled, .. } | Row::Action { enabled, .. } => *enabled,
        _ => true,
    }
}

/// A slider's position (0 to 1) and whether it is centred.
pub fn slider_share(row: &Row) -> Option<(f32, bool)> {
    match row {
        Row::Slider { value, min, max, centred, .. } => Some((((value - min) / (max - min).max(1e-6)).clamp(0.0, 1.0), *centred)),
        _ => None,
    }
}

// ---- the equalizer ----

/// Equalizer screen controls: toolbar, bands, then the rest of the chain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EqRow {
    Enabled,
    /// Graphic or parametric.
    Mode,
    /// How many graphic bands.
    Layout,
    Presets,
    AutoPreamp,
    Preamp,
    Band(usize),
    /// A graphic slider.
    Slider(usize),
    AddBand,
    Reset,
    Balance,
    /// bs2b preset (Off, Default, Chu Moy, Jan Meier) or custom.
    CrossfeedPreset,
    Crossfeed,
    CrossfeedCut,
    Mono,
    Limiter,
    Ceiling,
}

/// Crossfeed presets in stepping order.
const CROSSFEED_PRESETS: [&str; 4] = ["OFF", "DEFAULT", "CHU_MOY", "JAN_MEIER"];

/// The controls in ← → order.
pub fn eq_rows(p: &StoredPrefs) -> Vec<EqRow> {
    let graphic = p.eq_mode == EqMode::Graphic;
    let mut rows = vec![EqRow::Enabled, EqRow::Mode];
    if graphic {
        rows.push(EqRow::Layout);
    }
    rows.extend([EqRow::Presets, EqRow::AutoPreamp]);
    if p.eq_preamp_db.is_some() {
        rows.push(EqRow::Preamp);
    }
    if !graphic {
        rows.push(EqRow::AddBand);
    }
    rows.push(EqRow::Reset);
    if graphic {
        rows.extend((0..p.eq_graphic.len()).map(EqRow::Slider));
    } else {
        rows.extend((0..p.eq_bands.len()).map(EqRow::Band));
    }
    rows.extend([EqRow::Balance, EqRow::CrossfeedPreset, EqRow::Crossfeed]);
    if p.crossfeed_db > 0.0 {
        rows.push(EqRow::CrossfeedCut);
    }
    rows.extend([EqRow::Mono, EqRow::Limiter]);
    if p.limiter {
        rows.push(EqRow::Ceiling);
    }
    rows
}

impl EqRow {
    /// Title and value.
    pub fn words(&self, p: &StoredPrefs) -> (String, String) {
        let on = |b: bool| (if b { "● on" } else { "○ off" }).to_string();
        match *self {
            EqRow::Enabled => ("Equalizer".into(), on(p.eq_enabled)),
            EqRow::Mode => ("Kind".into(), if p.eq_mode == EqMode::Graphic { "‹ graphic ›" } else { "‹ parametric ›" }.into()),
            EqRow::Layout => ("Bands".into(), format!("‹ {} ›", p.eq_graphic.len())),
            EqRow::Slider(i) => {
                let label = nori_core::dsp::graphic_bands(p.eq_graphic.len() as u32).get(i).map_or(0.0, |b| b.label_hz);
                (if label.fract() != 0.0 { format!("{label:.1}") } else { text::hz(label) }, format!("{} dB", text::signed_db(p.eq_graphic.get(i).copied().unwrap_or(0.0))))
            }
            EqRow::Presets => ("Presets".into(), "choose ›".into()),
            EqRow::AutoPreamp => ("Automatic pre-amp".into(), on(p.eq_preamp_db.is_none())),
            EqRow::Preamp => ("Pre-amp".into(), text::preamp(p.eq_preamp_db.unwrap_or(0.0), false)),
            EqRow::Band(i) => {
                let b = p.eq_bands.get(i).copied().unwrap_or(nori_core::settings::band_from(0, 0.0, 0.0, 1.0, 0));
                (text::band(b.freq, nori_core::settings::band_mark(b.kind as i32, b.channel as i32)), format!("{} dB", text::signed_db(b.gain_db)))
            }
            EqRow::AddBand => ("Add a band".into(), "[ Add ]".into()),
            EqRow::Reset => ("Back to flat".into(), "[ Reset ]".into()),
            EqRow::Balance => ("Balance".into(), text::balance(p.balance)),
            EqRow::CrossfeedPreset => {
                let name = match nori_core::settings::crossfeed_preset(p.crossfeed_hz, p.crossfeed_db) {
                    _ if p.crossfeed_db <= 0.0 => "Off",
                    Some(nori_core::dsp::CrossfeedPreset::Default) => "Default (bs2b)",
                    Some(nori_core::dsp::CrossfeedPreset::ChuMoy) => "Chu Moy",
                    Some(nori_core::dsp::CrossfeedPreset::JanMeier) => "Jan Meier",
                    None => "Custom",
                };
                ("Crossfeed".into(), format!("‹ {name} ›"))
            }
            EqRow::Crossfeed => ("  Level".into(), if p.crossfeed_db > 0.0 { format!("{} dB", text::signed_db(p.crossfeed_db)) } else { "Off".into() }),
            EqRow::CrossfeedCut => ("  Cutoff".into(), format!("{} Hz", text::hz(p.crossfeed_hz))),
            EqRow::Mono => ("Mono".into(), on(p.mono)),
            EqRow::Limiter => ("Limiter".into(), on(p.limiter)),
            EqRow::Ceiling => ("Limiter ceiling".into(), text::ceiling(p.limiter_threshold_db)),
        }
    }

    /// For a band fader: its index and gain.
    pub fn band(&self, p: &StoredPrefs) -> Option<(usize, f32)> {
        match *self {
            EqRow::Slider(i) => Some((i, *p.eq_graphic.get(i)?)),
            EqRow::Band(i) => Some((i, p.eq_bands.get(i)?.gain_db)),
            _ => None,
        }
    }

    /// Whether it is in the toolbar above the bands.
    pub fn above_bands(&self) -> bool {
        matches!(self, EqRow::Enabled | EqRow::Mode | EqRow::Layout | EqRow::Presets | EqRow::AutoPreamp | EqRow::Preamp | EqRow::AddBand | EqRow::Reset)
    }

    /// Sets a band fader to `db`, rounded to 0.5 dB; None if unchanged.
    pub fn set_gain(&self, p: &StoredPrefs, db: f32) -> Option<Cmd> {
        let r = EQ_RANGES.gain;
        let db = ((db * 2.0).round() / 2.0).clamp(r.min, r.max);
        match *self {
            EqRow::Slider(i) => (p.eq_graphic.get(i).copied()? != db).then_some(Cmd::Graphic(i as u32, db)),
            EqRow::Band(i) => {
                let b = *p.eq_bands.get(i)?;
                (b.gain_db != db).then_some(Cmd::Band(i as u32, SoundBand { gain_db: db, ..b }))
            }
            _ => None,
        }
    }

    /// Whether a single click activates it.
    pub fn clicks(&self) -> bool {
        matches!(self, EqRow::Enabled | EqRow::Mode | EqRow::Presets | EqRow::AutoPreamp | EqRow::AddBand | EqRow::Reset | EqRow::Mono | EqRow::Limiter)
    }

    /// Chip label.
    pub fn chip(&self, p: &StoredPrefs) -> String {
        let (title, value) = self.words(p);
        match self {
            EqRow::Enabled => (if p.eq_enabled { "● Equalizer on" } else { "○ Equalizer off" }).into(),
            EqRow::Presets => "Presets ▾".into(),
            EqRow::AddBand => "+ Add a band".into(),
            EqRow::Reset => "↺ Flat".into(),
            EqRow::Mode => (if p.eq_mode == EqMode::Graphic { "Graphic ‹›" } else { "Parametric ‹›" }).into(),
            EqRow::Layout => format!("{} bands ‹›", p.eq_graphic.len()),
            EqRow::Crossfeed => format!("Crossfeed level {value}"),
            EqRow::CrossfeedCut => format!("Crossfeed cutoff {value}"),
            _ => format!("{} {}", title.trim(), value),
        }
    }

    /// ↑ or ↓ on it; None when the value would not change, so a key held at a range end sends nothing.
    pub fn step(&self, p: &StoredPrefs, up: bool) -> Option<Cmd> {
        let d = if up { 0.5 } else { -0.5 };
        let r = EQ_RANGES;
        let level = |level: EqLevel, was: f32, to: f32| (to != was).then_some(Cmd::Level(level, to));
        match *self {
            EqRow::Enabled => (p.eq_enabled != up).then_some(Cmd::Setting("eq".into(), up.to_string())),
            EqRow::Mode => {
                let want = if up { EqMode::Graphic } else { EqMode::Parametric };
                (p.eq_mode != want).then(|| Cmd::Setting("eqMode".into(), if up { "GRAPHIC" } else { "PARAMETRIC" }.into()))
            }
            EqRow::Layout => {
                let at = nori_core::dsp::graphic::LAYOUTS.iter().position(|n| *n == p.eq_graphic.len()).unwrap_or(0);
                let to = if up { (at + 1).min(nori_core::dsp::graphic::LAYOUTS.len() - 1) } else { at.saturating_sub(1) };
                (to != at).then(|| Cmd::Setting("eqLayout".into(), nori_core::dsp::graphic::LAYOUTS[to].to_string()))
            }
            EqRow::Slider(i) => {
                let was = *p.eq_graphic.get(i)?;
                let to = (was + d).clamp(r.gain.min, r.gain.max);
                (to != was).then_some(Cmd::Graphic(i as u32, to))
            }
            EqRow::Mono => (p.mono != up).then_some(Cmd::Setting("mono".into(), up.to_string())),
            EqRow::Limiter => (p.limiter != up).then_some(Cmd::Setting("limiter".into(), up.to_string())),
            EqRow::AutoPreamp => (p.eq_preamp_db.is_none() != up).then_some(Cmd::Sound(SoundToolCmd::AutoPreamp(up))),
            EqRow::Preamp => {
                let was = p.eq_preamp_db.unwrap_or(0.0);
                level(EqLevel::Preamp, was, (was + d).clamp(r.preamp.min, r.preamp.max))
            }
            EqRow::Band(i) => {
                let b = *p.eq_bands.get(i)?;
                let gain_db = (b.gain_db + d).clamp(r.gain.min, r.gain.max);
                (gain_db != b.gain_db).then_some(Cmd::Band(i as u32, SoundBand { gain_db, ..b }))
            }
            EqRow::Balance => level(EqLevel::Balance, p.balance, (p.balance + d / 10.0).clamp(r.balance.min, r.balance.max)),
            EqRow::Crossfeed => level(EqLevel::Crossfeed, p.crossfeed_db, (p.crossfeed_db.max(if up { 0.5 } else { 0.0 }) + d).clamp(r.crossfeed.min, r.crossfeed.max)),
            EqRow::CrossfeedCut => level(EqLevel::CrossfeedCut, p.crossfeed_hz, (p.crossfeed_hz + d * 100.0).clamp(r.crossfeed_cut.min, r.crossfeed_cut.max)),
            EqRow::CrossfeedPreset => {
                // From custom, up goes to the last preset and down to the first.
                let now = nori_core::settings::crossfeed_preset(p.crossfeed_hz, p.crossfeed_db);
                let at = if p.crossfeed_db <= 0.0 { Some(0) } else { now.and_then(|c| nori_core::dsp::CrossfeedPreset::ALL.iter().position(|x| *x == c)).map(|i| i + 1) };
                let to = match (at, up) {
                    (Some(i), true) => (i + 1).min(CROSSFEED_PRESETS.len() - 1),
                    (Some(i), false) => i.saturating_sub(1),
                    (None, true) => CROSSFEED_PRESETS.len() - 1,
                    (None, false) => 1,
                };
                (Some(to) != at).then(|| Cmd::Setting("crossfeedPreset".into(), CROSSFEED_PRESETS[to].into()))
            }
            EqRow::Ceiling => level(EqLevel::Limiter, p.limiter_threshold_db, (p.limiter_threshold_db + d).clamp(r.limiter.min, r.limiter.max)),
            EqRow::Presets | EqRow::AddBand | EqRow::Reset => None,
        }
    }

    /// Enter.
    pub fn open(&self, p: &StoredPrefs) -> Option<Cmd> {
        match *self {
            EqRow::Enabled => Some(Cmd::Setting("eq".into(), (!p.eq_enabled).to_string())),
            EqRow::Mode => self.step(p, p.eq_mode != EqMode::Graphic),
            EqRow::Mono => Some(Cmd::Setting("mono".into(), (!p.mono).to_string())),
            EqRow::Limiter => Some(Cmd::Setting("limiter".into(), (!p.limiter).to_string())),
            EqRow::AutoPreamp => Some(Cmd::Sound(SoundToolCmd::AutoPreamp(p.eq_preamp_db.is_some()))),
            EqRow::AddBand => Some(Cmd::Sound(SoundToolCmd::AddBand)),
            EqRow::Reset => Some(Cmd::Sound(SoundToolCmd::ResetBands)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_row(prefs: &StoredPrefs) -> Vec<(&'static str, Row)> {
        let state = settings_model::state(prefs, settings_model::Output::default(), &crate::backend::app().settings.model, &crate::backend::app().settings.sing_model);
        let facts = Facts { folders: vec![MusicFolder { id: "1".into(), name: "A".into() }, MusicFolder { id: "2".into(), name: "B".into() }], ..Facts::default() };
        GROUPS
            .iter()
            .flat_map(|g| page(g.id, prefs, &state, &facts, &Own::default()).sections.into_iter().flat_map(move |s| s.rows.into_iter().map(move |r| (g.id, r))))
            .collect()
    }

    /// Settings that open every row there is.
    fn everything_on() -> StoredPrefs {
        let server = nori_core::settings::SavedServer { id: "a".into(), url: "https://a".into(), alt_url: "https://b".into(), ..Default::default() };
        StoredPrefs {
            auto_mix: true,
            auto_mix_beat_match: true,
            auto_fill: true,
            replay_gain: nori_core::settings::GainMode::Track,
            scrobble: true,
            lyrics_online: true,
            third_party_lookups: true,
            servers: vec![server],
            active_server_id: "a".into(),
            ..StoredPrefs::default()
        }
    }

    #[test]
    fn rows_and_labels() {
        let prefs = everything_on();
        let rows = every_row(&prefs);
        assert!(rows.len() > 40, "rows: {}", rows.len());
        for (group, row) in &rows {
            let mut v = SettingsView::default();
            v.pages = Some(vec![Page { title: group.to_string(), sections: vec![Section { title: String::new(), rows: vec![row.clone()] }] }]);
            v.row.at = 1;
            match row {
                Row::Toggle { switch: Switch::Setting(name), enabled: true, .. } => {
                    let Some(Opened::Cmds(c)) = v.open() else { panic!("{name} does not switch") };
                    assert!(matches!(&c[0], Cmd::Setting(n, _) if n == name), "{name}");
                }
                Row::Choice { target: Target::Setting(name), enabled: true, options, .. } => {
                    let Some(Opened::Overlay(Overlay::Picker { target: picked, options: shown, .. })) = v.open() else { panic!("{name} offers no choice") };
                    assert_eq!(picked, Target::Setting(name.clone()));
                    assert_eq!(shown.len(), options.len());
                    let steps = [v.step(true), v.step(false)].concat();
                    assert!(steps.iter().all(|c| matches!(c, Cmd::Setting(n, _) if n == name)), "{name}");
                    for (_, value) in options {
                        assert!(nori_core::settings::set_by_name(&prefs, name, value).is_some(), "{name} = {value}");
                    }
                }
                Row::Slider { name, level, .. } => {
                    let c = v.step(true);
                    assert!(matches!(&c[..], [Cmd::Level(l, _)] if Some(*l) == *level) || matches!(&c[..], [Cmd::Setting(n, _)] if n == name), "{name}");
                }
                _ => {}
            }
        }

        // Choice labels.
        let d = StoredPrefs::default();
        let rows = every_row(&d);
        let shown = |name: &str| {
            rows.iter()
                .find_map(|(_, r)| match r {
                    Row::Choice { target: Target::Setting(n), shown, .. } if n == name => Some(shown.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no {name}"))
        };
        assert_eq!(shown("replayGain"), "off");
        assert_eq!(shown("wifi"), "original");
        assert_eq!(shown("speed"), "1×");
        assert_eq!(quality("320:mp3"), "mp3 320k");

        // Phone only settings hidden.
        let names: Vec<String> = every_row(&everything_on())
            .into_iter()
            .filter_map(|(_, r)| match r {
                Row::Toggle { switch: Switch::Setting(name), .. } | Row::Choice { target: Target::Setting(name), .. } => Some(name),
                _ => None,
            })
            .collect();
        for phone in ["swipeLeft", "swipeRight", "tapAction", "offload", "bitPerfect", "motionArtwork", "amoled", "dynamicColor", "softSleeve", "lyricsKeepScreenOn", "uiScale", "soundBypass", "coversAhead", "altMaxBitRate", "liveSearchDelayMs"] {
            assert!(!names.iter().any(|n| n == phone), "{phone} offered in a terminal");
        }
    }

    #[test]
    fn lyrics_sources_move_and_switch() {
        // All services default on; one is switched off to have an off row.
        let d = StoredPrefs::default();
        let prefs = StoredPrefs { lyrics_online: true, third_party_lookups: true, lyrics_on: d.lyrics_on.iter().filter(|s| s.name() != "GENIUS").copied().collect(), ..d };
        let mut v = SettingsView::default();
        let pages = v.pages(&prefs).to_vec();
        let lines = SettingsView::lines(&pages);
        let ranked: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| matches!(l, Line::Row(Row::Ranked { .. }))).map(|(i, _)| i).collect();
        assert_eq!(ranked.len(), 16);
        assert!(lines.iter().all(|l| !matches!(l, Line::Row(Row::Ranked { title, .. }) if title == "?")), "every service has a name");
        let off = *ranked.iter().find(|i| matches!(lines[**i], Line::Row(Row::Ranked { on: false, .. }))).unwrap();
        let Line::Row(Row::Ranked { id, name, .. }) = lines[off] else { unreachable!() };
        let (id, name) = (id.clone(), name.clone());
        v.row.at = off;
        assert!(matches!(&v.step(false)[..], [Cmd::Setting(n, value)] if n == "lyricsMove" && *value == format!("{id}:-1")));
        let Some(Opened::Cmds(c)) = v.open() else { panic!() };
        assert!(matches!(&c[..], [Cmd::Setting(n, value)] if *n == name && value == "true"));
        // Third-party lookups off: no sources listed.
        let quiet = StoredPrefs { third_party_lookups: false, ..prefs };
        v.invalidate();
        let pages = v.pages(&quiet).to_vec();
        assert!(!SettingsView::lines(&pages).iter().any(|l| matches!(l, Line::Row(Row::Ranked { .. }))));
    }

    #[test]
    fn sound_controls() {
        let p = StoredPrefs { eq_mode: EqMode::Graphic, ..StoredPrefs::default() };
        let rows = eq_rows(&p);
        assert_eq!(rows.iter().filter(|r| matches!(r, EqRow::Slider(_))).count(), 10);
        assert!(!rows.iter().any(|r| matches!(r, EqRow::Band(_) | EqRow::AddBand)), "no parametric bands on the graphic one");
        assert_eq!(EqRow::Slider(0).words(&p), ("31.5".to_string(), "+0.0 dB".to_string()));
        assert!(matches!(EqRow::Slider(3).step(&p, true), Some(Cmd::Graphic(3, v)) if v == 0.5));
        assert!(matches!(EqRow::Layout.step(&p, true), Some(Cmd::Setting(n, v)) if n == "eqLayout" && v == "15"));
        assert!(matches!(EqRow::Layout.step(&p, false), Some(Cmd::Setting(n, v)) if n == "eqLayout" && v == "5"));
        let five = StoredPrefs { eq_graphic: vec![0.0; 5], ..p.clone() };
        assert!(EqRow::Layout.step(&five, false).is_none(), "five is the fewest");
        assert_eq!(eq_rows(&five).iter().filter(|r| matches!(r, EqRow::Slider(_))).count(), 5);
        assert_eq!(EqRow::Slider(0).words(&five).0, "63");
        assert!(matches!(EqRow::Mode.open(&p), Some(Cmd::Setting(n, v)) if n == "eqMode" && v == "PARAMETRIC"));
        assert!(rows.contains(&EqRow::Layout));
        let parametric = eq_rows(&StoredPrefs { eq_mode: EqMode::Parametric, ..StoredPrefs::default() });
        assert!(parametric.contains(&EqRow::Mode) && !parametric.contains(&EqRow::Layout));
        let fx = effects(&Build { p: &StoredPrefs { compressor: true, ..StoredPrefs::default() }, s: &settings_model::state(&StoredPrefs::default(), settings_model::Output::default(), &crate::backend::app().settings.model, &crate::backend::app().settings.sing_model) });
        assert!(fx.iter().any(|r| matches!(r, Row::Slider { level: Some(EqLevel::CompRatio), .. })));

        // Parametric bands step in range.
        let prefs = StoredPrefs { eq_mode: EqMode::Parametric, eq_bands: nori_core::settings::graphic(), ..StoredPrefs::default() };
        let rows = eq_rows(&prefs);
        assert_eq!(rows.iter().filter(|r| matches!(r, EqRow::Band(_))).count(), prefs.eq_bands.len());
        let Some(Cmd::Band(0, b)) = EqRow::Band(0).step(&prefs, true) else { panic!() };
        assert_eq!(b.gain_db, prefs.eq_bands[0].gain_db + 0.5);
        let loud = StoredPrefs { eq_bands: prefs.eq_bands.iter().map(|b| SoundBand { gain_db: 12.0, ..*b }).collect(), ..prefs.clone() };
        assert_eq!(EqRow::Band(0).step(&loud, true), None, "held at the top of the core's range: nothing asked");
        let Some(Cmd::Band(_, b)) = EqRow::Band(0).step(&loud, false) else { panic!() };
        assert_eq!(b.gain_db, 11.5);
        let flat = StoredPrefs { crossfeed_db: 0.0, ..prefs.clone() };
        assert_eq!(EqRow::Crossfeed.step(&flat, false), None, "crossfeed off stays off");

        // Crossfeed presets and cutoff.
        let off = StoredPrefs::default();
        assert!(!eq_rows(&off).contains(&EqRow::CrossfeedCut), "no cutoff to set with crossfeed off");
        assert_eq!(EqRow::CrossfeedPreset.words(&off).1, "‹ Off ›");
        assert!(matches!(EqRow::CrossfeedPreset.step(&off, true), Some(Cmd::Setting(n, v)) if n == "crossfeedPreset" && v == "DEFAULT"));
        assert_eq!(EqRow::CrossfeedPreset.step(&off, false), None);
        let meier = StoredPrefs { crossfeed_db: 9.5, crossfeed_hz: 650.0, ..off.clone() };
        assert!(eq_rows(&meier).contains(&EqRow::CrossfeedCut));
        assert_eq!(EqRow::CrossfeedPreset.words(&meier).1, "‹ Jan Meier ›");
        assert_eq!(EqRow::CrossfeedPreset.step(&meier, true), None, "the last one");
        assert_eq!(EqRow::CrossfeedCut.words(&meier).1, "650 Hz");
        assert!(matches!(EqRow::CrossfeedCut.step(&meier, true), Some(Cmd::Level(EqLevel::CrossfeedCut, v)) if v == 700.0));
        let custom = StoredPrefs { crossfeed_db: 5.0, ..off };
        assert_eq!(EqRow::CrossfeedPreset.words(&custom).1, "‹ Custom ›");
        assert!(matches!(EqRow::CrossfeedPreset.step(&custom, false), Some(Cmd::Setting(_, v)) if v == "DEFAULT"));
    }

}
