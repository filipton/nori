//! Settings, the terminal's own: its groups, pages, rows and every word on them are this client's,
//! chosen for a terminal - only what makes sense at a desk, worded for one. What each setting is, the
//! values it offers and its value now are the core's settings model (`nori_settings::settings_model`); a
//! row sends back the setting's name with the value picked (`setting_set`), or moves a level in place
//! (`edit_level`). The client's own few settings (the mouse, covers, the volume, the output device) are
//! rows of the same kinds.

use crate::text;
use nori_core::settings::{EqLevel, EqMode, SoundBand, StoredPrefs, EQ_RANGES};
use nori_core::settings_model::{self, BeatModel, LyricsSource, SettingsState};
use nori_core::MusicFolder;

use crate::app::{Cmd, Overlay, Screen, Sel, SoundToolCmd};

/// The client's own group, first in the list.
pub const OWN: &str = "terminal";

/// What opening a row does.
pub enum Opened {
    Cmds(Vec<Cmd>),
    Overlay(Overlay),
    Screen(Screen),
    /// One of the client's own switches, by name.
    Own(&'static str),
    Login,
}

/// A group of settings: its page, listed on the left.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub id: &'static str,
    pub title: &'static str,
}

/// The groups, in the order they are listed.
pub const GROUPS: [Group; 8] = [
    Group { id: OWN, title: "Terminal" },
    Group { id: "sound", title: "Sound" },
    Group { id: "playback", title: "Playback" },
    Group { id: "library", title: "Library" },
    Group { id: "lyrics", title: "Lyrics" },
    Group { id: "server", title: "Server" },
    Group { id: "storage", title: "Storage" },
    Group { id: "about", title: "About" },
];

/// One row of a page.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Toggle { name: String, title: String, detail: String, on: bool, enabled: bool },
    /// `options` are (label, value); `shown` is the chosen one's label, or the value itself.
    Choice { name: String, title: String, options: Vec<(String, String)>, shown: String, enabled: bool },
    Note { text: String },
    /// Opens another screen (`action`), with a status at its end.
    Link { title: String, status: String, action: String },
    /// A line of text and a button at its end.
    Action { title: String, detail: String, button: String, enabled: bool, action: String },
    Info { title: String, detail: String },
    /// `level`: dragged through `edit_level` instead of by `name`.
    Slider { name: String, label: String, value: f32, min: f32, max: f32, centred: bool, level: Option<EqLevel> },
    /// Colour swatches, ARGB.
    Palette { name: String, colours: Vec<i64>, chosen: i64 },
    Server { id: String, label: String, detail: String, active: bool },
    Button { title: String, action: String },
    /// A lyrics service in the one ranked list: switched where it stands, moved a place with ← →.
    Ranked { name: String, id: String, title: String, detail: String, on: bool },
    /// Text typed in (a service's key).
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

/// What lives on this computer, in bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Storage {
    pub stream: i64,
    pub covers: i64,
    pub lyrics: i64,
    pub downloads: i64,
    pub download_songs: u32,
    pub database: i64,
}

/// What the pages show besides the settings, fetched on the backend's thread when Settings opens.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// Songs AutoMix has measured.
    pub analysed: u32,
    /// What the offline index holds: songs, albums, artists.
    pub indexed: (u32, u32, u32),
    pub storage: Storage,
    /// The active server's music folders.
    pub folders: Vec<MusicFolder>,
    /// The sound cards there are to play through.
    pub devices: Vec<String>,
}

/// A line of a page: a section's title, or one of its rows.
pub enum Line<'a> {
    Title(&'a str),
    Row(&'a Row),
}

#[derive(Default)]
pub struct SettingsView {
    /// 0 the groups, 1 the page's rows.
    pub pane: usize,
    pub group: Sel,
    pub row: Sel,
    page: Option<Page>,
    page_of: Option<String>,
    pub facts: Facts,
    pub facts_asked: bool,
    /// What the client's own page says: the image protocol, whether the mouse and covers are on, the volume.
    pub own: Own,
}

#[derive(Default, Clone)]
pub struct Own {
    pub mouse: bool,
    pub images: bool,
    pub volume: f32,
    pub protocol: String,
    pub data: String,
    /// The output device chosen for the next start; empty for the system's own.
    pub device: String,
}

impl SettingsView {
    pub fn groups(&self) -> &'static [Group] {
        &GROUPS
    }

    pub fn invalidate(&mut self) {
        self.page = None;
    }

    pub fn set_facts(&mut self, f: Facts) {
        self.facts = f;
        self.invalidate();
    }

    pub fn group_id(&self) -> &'static str {
        GROUPS.get(self.group.at).map_or("", |g| g.id)
    }

    /// The page of the group selected, worked out again only when something it shows changed.
    pub fn page(&mut self, prefs: &StoredPrefs) -> &Page {
        let id = self.group_id();
        if self.page.is_none() || self.page_of.as_deref() != Some(id) {
            let state = settings_model::state(prefs, settings_model::Output::default());
            self.page = Some(page(id, prefs, &state, &self.facts, &self.own));
            self.page_of = Some(id.to_string());
        }
        self.page.as_ref().expect("made above")
    }

    /// The page's lines, titles and rows, as drawn.
    pub fn lines(page: &Page) -> Vec<Line<'_>> {
        let mut out = Vec::new();
        for s in &page.sections {
            if !s.title.is_empty() {
                out.push(Line::Title(&s.title));
            }
            out.extend(s.rows.iter().map(Line::Row));
        }
        out
    }

    /// The list the keys move in: the groups, or the page's lines.
    pub fn list(&mut self) -> (&mut Sel, usize) {
        if self.pane == 0 {
            return (&mut self.group, GROUPS.len());
        }
        let n = self.page.as_ref().map_or(0, |p| Self::lines(p).len());
        (&mut self.row, n)
    }

    /// The row selected on the page, if the page is showing one.
    fn selected(&self) -> Option<&Row> {
        let page = self.page.as_ref()?;
        match Self::lines(page).into_iter().nth(self.row.at)? {
            Line::Row(r) => Some(r),
            Line::Title(_) => None,
        }
    }

    /// Whether ← and → change the row selected (rather than seek).
    pub fn adjustable(&self) -> bool {
        matches!(self.selected(), Some(Row::Choice { .. } | Row::Toggle { .. } | Row::Slider { .. } | Row::Palette { .. } | Row::Ranked { .. }))
    }

    /// Whether a single click does what the row does (a switch, a button), rather than only selecting it.
    pub fn clicks_open(&self) -> bool {
        matches!(self.selected(), Some(Row::Toggle { .. } | Row::Button { .. } | Row::Link { .. } | Row::Action { .. } | Row::Server { .. } | Row::Ranked { .. }))
    }

    /// Enter on the selection.
    pub fn open(&mut self, prefs: &StoredPrefs, _mouse: bool, _images: bool) -> Option<Opened> {
        if self.pane == 0 {
            self.pane = 1;
            self.row = Sel::default();
            self.page(prefs);
            self.skip_titles(true);
            return Some(Opened::Cmds(Vec::new()));
        }
        let row = self.selected()?.clone();
        Some(match row {
            Row::Toggle { name, on, enabled, .. } => match own_name(&name) {
                Some(key) => Opened::Own(key),
                None if enabled => Opened::Cmds(vec![Cmd::Setting(name, (!on).to_string())]),
                None => return None,
            },
            Row::Choice { name, title, options, shown, enabled: true } => {
                let at = options.iter().position(|o| o.0 == shown).unwrap_or(0);
                Opened::Overlay(Overlay::Picker { title, options, sel: Sel { at, top: 0 }, name })
            }
            Row::Link { action, .. } | Row::Action { action, enabled: true, .. } => match action.as_str() {
                "equalizer" => Opened::Screen(Screen::Equalizer),
                "downloads" => Opened::Screen(Screen::Downloads),
                _ => Opened::Cmds(vec![Cmd::Action(action)]),
            },
            Row::Server { id, active, .. } => {
                if active {
                    return None;
                }
                Opened::Cmds(vec![Cmd::SwitchServer(id)])
            }
            Row::Button { action, .. } if action == "add-server" => Opened::Login,
            Row::Button { action, .. } => Opened::Cmds(vec![Cmd::Action(action)]),
            Row::Ranked { name, on, .. } => Opened::Cmds(vec![Cmd::Setting(name, (!on).to_string())]),
            Row::Text { name, title, value, secret, .. } => Opened::Overlay(Overlay::Input { title, text: value, secret, name }),
            Row::Palette { name, colours, chosen } => {
                let at = colours.iter().position(|c| *c == chosen).map_or(0, |i| (i + 1) % colours.len());
                Opened::Cmds(vec![Cmd::Setting(name, colours[at].to_string())])
            }
            _ => return None,
        })
    }

    /// ← or → on the selection: the previous or next option, off or on, a step of a slider.
    pub fn step(&mut self, _prefs: &StoredPrefs, up: bool) -> Vec<Cmd> {
        let Some(row) = self.selected().cloned() else { return Vec::new() };
        match row {
            Row::Choice { name, options, shown, enabled: true, .. } => {
                let at = options.iter().position(|o| o.0 == shown);
                let to = match at {
                    Some(i) if up => (i + 1).min(options.len() - 1),
                    Some(i) => i.saturating_sub(1),
                    None => 0,
                };
                if Some(to) == at || options.is_empty() {
                    return Vec::new();
                }
                let value = options[to].1.clone();
                if name == "!device" {
                    return vec![Cmd::Device(value)];
                }
                vec![Cmd::Setting(name, value)]
            }
            Row::Toggle { name, on, enabled, .. } => {
                if on == up {
                    return Vec::new();
                }
                match own_name(&name) {
                    Some("mouse") => vec![Cmd::Mouse(up)],
                    Some("images") => vec![Cmd::Images(up)],
                    Some(_) => Vec::new(),
                    None if enabled => vec![Cmd::Setting(name, up.to_string())],
                    None => Vec::new(),
                }
            }
            Row::Slider { name, value, min, max, level, .. } => {
                let step = slider_step(min, max);
                let v = (value + if up { step } else { -step }).clamp(min, max);
                match (name.as_str(), level) {
                    ("!volume", _) => vec![Cmd::Volume(v)],
                    (_, Some(l)) => vec![Cmd::Level(l, v)],
                    (_, None) => vec![Cmd::Setting(name, v.to_string())],
                }
            }
            Row::Palette { name, colours, chosen } => {
                let at = colours.iter().position(|c| *c == chosen).unwrap_or(0) as isize + if up { 1 } else { -1 };
                let at = at.rem_euclid(colours.len() as isize) as usize;
                vec![Cmd::Setting(name, colours[at].to_string())]
            }
            // A lyrics service a place down (→) or up (←) the one list, on or off.
            Row::Ranked { id, .. } => vec![Cmd::Setting("lyricsMove".into(), format!("{id}:{}", if up { 1 } else { -1 }))],
            _ => Vec::new(),
        }
    }

    /// The selection moved off a section's title, onto a row.
    pub fn skip_titles(&mut self, down: bool) {
        let Some(page) = &self.page else { return };
        let lines = Self::lines(page);
        let title = |i: usize| matches!(lines.get(i), Some(Line::Title(_)));
        let mut at = self.row.at;
        while title(at) && at + 1 < lines.len() && down {
            at += 1;
        }
        while title(at) && at > 0 && !down {
            at -= 1;
        }
        // At either end, a title is left the other way.
        while title(at) && at + 1 < lines.len() {
            at += 1;
        }
        self.row.at = at;
    }
}

/// A slider's step: a fortieth of its range, in half decibels for the dB ranges.
pub fn slider_step(min: f32, max: f32) -> f32 {
    let span = max - min;
    if span >= 8.0 {
        0.5
    } else {
        (span / 40.0).max(0.01)
    }
}

fn own_name(name: &str) -> Option<&'static str> {
    match name {
        "!mouse" => Some("mouse"),
        "!images" => Some("images"),
        _ => None,
    }
}

// ---- the pages ----

/// Builds a page's rows from the settings, the core's state and the facts.
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
        Row::Toggle { name: name.into(), title: title.into(), detail: detail.into(), on: self.on(name), enabled }
    }

    /// The core's options for `name`, each worded by `label`.
    fn choice(&self, name: &str, title: &str, enabled: bool, label: impl Fn(&str) -> String) -> Row {
        let options: Vec<(String, String)> = options(name).into_iter().map(|v| (label(&v), v)).collect();
        self.choice_of(name, title, options, enabled)
    }

    fn choice_of(&self, name: &str, title: &str, options: Vec<(String, String)>, enabled: bool) -> Row {
        let v = self.value(name);
        let shown = options.iter().find(|o| o.1 == v).map_or(v, |o| o.0.clone());
        Row::Choice { name: name.into(), title: title.into(), options, shown, enabled }
    }

    /// An enum setting: its values by name, worded in the same order.
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

fn action(title: &str, detail: String, button: &str, enabled: bool, act: &str) -> Row {
    Row::Action { title: title.into(), detail, button: button.into(), enabled, action: act.into() }
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
        "about" => about(),
        _ => Vec::new(),
    };
    Page { title, sections }
}

/// The client's own page: rows of the same kinds, so they are drawn and changed like the rest.
fn own_page(b: &Build, o: &Own, f: &Facts) -> Vec<Section> {
    let toggle = |name: &str, title: &str, detail: &str, on: bool| Row::Toggle { name: name.into(), title: title.into(), detail: detail.into(), on, enabled: true };
    let mut devices = vec![("system default".to_string(), String::new())];
    devices.extend(f.devices.iter().map(|d| (d.clone(), d.clone())));
    if !o.device.is_empty() && !f.devices.contains(&o.device) {
        devices.push((format!("{} (not found)", o.device), o.device.clone()));
    }
    let shown = devices.iter().find(|d| d.1 == o.device).map_or_else(|| o.device.clone(), |d| d.0.clone());
    let rows = vec![
        toggle("!mouse", "Mouse", "Clicks, the wheel and dragging the seek bar. Off, the terminal selects text (m).", o.mouse),
        toggle("!images", "Covers", "Album art in the player and on album pages (I).", o.images),
        info("Pictures drawn with", o.protocol.clone()),
        Row::Slider { name: "!volume".into(), label: format!("Volume {} %", (o.volume * 100.0).round()), value: o.volume, min: 0.0, max: 1.0, centred: false, level: None },
        Row::Choice { name: "!device".into(), title: "Output device".into(), options: devices, shown, enabled: true },
        Row::Note { text: "The device is opened at start; --device overrides it for one run.".into() },
        info("Data kept in", o.data.clone()),
    ];
    let accents: Vec<i64> = options("accent").iter().filter_map(|c| c.parse().ok()).collect();
    let look = vec![
        b.toggle("coverColors", "Colours from the cover", "The page takes the playing cover's colours (with covers on)"),
        Row::Palette { name: "accent".into(), colours: accents, chosen: b.p.accent },
    ];
    vec![section("This client", rows), section("Look", look)]
}

fn sound(b: &Build) -> Vec<Section> {
    let p = b.p;
    let s = b.s;
    let live = !s.untouched;
    let eq_status = if s.untouched { "bypassed" } else if s.sound_chain_on { "on" } else { "off" };
    let eq = vec![Row::Link { title: "Equalizer, crossfeed, balance, limiter".into(), status: eq_status.into(), action: "equalizer".into() }];

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

    let tempo = vec![
        b.choice("speed", "Speed", true, |v| format!("{v}×")),
        b.choice("pitch", "Pitch", true, |v| format!("{v}×")),
        b.toggle_if("skipSilence", "Skip silence", "Shorten quiet gaps", live),
    ];
    let output = vec![b.toggle("hiRes", "Bit-exact output", "Float samples straight to the device; bypasses mixing and effects")];
    vec![section("Equalizer", eq), section("Effects", effects(b)), section("Levelling", levelling), section("Transitions", mixing), section("Tempo", tempo), section("Output", output)]
}

/// Bass boost, virtualizer, volume boost and the compressor: each a slider through the core's level edits.
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
    }
    let errors = vec![
        b.toggle("skipOnError", "Skip unplayable songs", "Up to three in a row"),
        b.toggle("bridgeOffline", "Offline fallback", "Play downloads while the server is unreachable"),
    ];
    let analysis = vec![action("Measured songs", format!("{} songs with tempo and beats", f.analysed), "Forget", f.analysed > 0, "measure-again")];
    vec![section("Queue", queue), section("Errors", errors), section("Analysis", analysis)]
}

fn library(b: &Build, f: &Facts) -> Vec<Section> {
    let (songs, albums, artists) = f.indexed;
    let index = vec![
        action("Offline index", format!("{songs} songs, {albums} albums, {artists} artists"), "Update", true, "sync-library"),
        b.choice("liveSearchDelayMs", "Search delay", true, |v| format!("{v} ms")),
    ];
    let mut history = vec![
        b.toggle("tasteModel", "Listening history", "Kept locally; feeds mixes and stats"),
        b.toggle("scrobble", "Scrobble", "Report plays to the server"),
    ];
    if b.p.scrobble {
        history.push(b.choice("scrobblePercent", "  Scrobble at", true, |v| format!("{v} %")));
    }
    let online = vec![b.toggle("thirdPartyLookups", "Third-party lookups", "Lyrics services and the AutoEQ list; off, nothing leaves for anyone but your server")];
    vec![section("Index and search", index), section("History", history), section("Online", online)]
}

/// A lyrics service's name and a short line about it, by the id the core stores it under.
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
        b.toggle("lyricsSweep", "Word fill", "Colour words in as they are sung (word-timed lyrics)"),
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
    accounts.push(Row::Button { title: "Add a server".into(), action: "add-server".into() });
    let mut out = vec![section("Accounts", accounts)];
    let current = p.servers.iter().find(|s| s.id == p.active_server_id);
    let mut this = Vec::new();
    if f.folders.len() > 1 {
        let mut o = vec![("all".to_string(), String::new())];
        o.extend(f.folders.iter().map(|m| (m.name.clone(), m.id.clone())));
        this.push(b.choice_of("musicFolder", "Music folder", o, true));
    }
    if current.is_some_and(|s| !s.alt_url.trim().is_empty()) {
        this.push(b.choice("altMaxBitRate", "Second address bitrate cap", true, |v| if v == "0" { "none".into() } else { format!("{v} kbps") }));
    }
    if !this.is_empty() {
        out.push(section("This server", this));
    }
    out
}

/// A stream quality as a value ("320:mp3") in a terminal's words.
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
        action("Download everything", "Every indexed song".into(), "Download", f.indexed.0 > 0, "download-library"),
        Row::Link { title: "Downloads".into(), status: format!("{} songs, {}", s.download_songs, bytes(s.downloads)), action: "downloads".into() },
    ];
    let cache = vec![
        b.choice("cacheMb", "Stream cache limit", true, |v| match v.parse::<i32>() {
            Ok(mb) if mb % 1024 == 0 => format!("{} GB", mb / 1024),
            _ => format!("{v} MB"),
        }),
        b.choice("coversAhead", "Covers fetched ahead", true, |v| v.to_string()),
        action("Stream cache", bytes(s.stream), "Clear", s.stream > 0, "clear-stream"),
        action("Cover cache", bytes(s.covers), "Clear", s.covers > 0, "clear-covers"),
        action("Lyrics cache", format!("{} of lyrics found online", bytes(s.lyrics)), "Clear", s.lyrics > 0, "clear-lyrics"),
        info("Database", bytes(s.database)),
    ];
    vec![section("Quality and downloads", quality_rows), section("Cache", cache)]
}

/// About: the version, and what the core is built from, from the core's own credits.
fn about() -> Vec<Section> {
    let version = Section {
        title: "nori".into(),
        rows: vec![info("Version", env!("CARGO_PKG_VERSION").into()), info("Terminal client", "ratatui over crossterm; covers through ratatui-image".into())],
    };
    let credits = nori_core::credits::core_credits().into_iter().map(|c| info(&c.name, format!("{} · {} · {}", c.what, c.copyright, c.licence))).collect();
    let tui = vec![
        info("ratatui, crossterm", "The screen and the keys · The Ratatui developers, Timon Post · MIT".into()),
        info("ratatui-image", "Covers in the terminal: kitty, sixel, iTerm2, half blocks · Benjamin Große · MIT".into()),
        info("image, icy_sixel", "Pictures handed to the terminal · The image-rs developers, Mike Krüger · MIT or Apache-2.0".into()),
    ];
    vec![version, section("The core", credits), section("The terminal client", tui)]
}

/// The words a row shows at its end: a switch, the option chosen, a slider's value.
pub fn row_value(row: &Row) -> String {
    match row {
        Row::Toggle { on, .. } | Row::Ranked { on, .. } => (if *on { "● on" } else { "○ off" }).into(),
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

/// A row's title and the line under it.
pub fn row_words(row: &Row) -> (String, String) {
    match row {
        Row::Toggle { title, detail, .. } | Row::Ranked { title, detail, .. } | Row::Text { title, detail, .. } | Row::Info { title, detail, .. } => (title.clone(), detail.clone()),
        Row::Choice { title, .. } | Row::Link { title, .. } => (title.clone(), String::new()),
        Row::Note { text } => (String::new(), text.clone()),
        Row::Action { title, detail, .. } => (title.clone(), detail.clone()),
        Row::Slider { label, .. } => (label.clone(), String::new()),
        Row::Palette { .. } => ("Accent colour".into(), String::new()),
        Row::Server { label, detail, .. } => (label.clone(), detail.clone()),
        Row::Button { .. } => (String::new(), String::new()),
    }
}

/// Whether the row is live: a setting the app is ignoring right now is drawn dimmed.
pub fn row_enabled(row: &Row) -> bool {
    match row {
        Row::Toggle { enabled, .. } | Row::Choice { enabled, .. } | Row::Action { enabled, .. } => *enabled,
        _ => true,
    }
}

/// A slider's place, 0 to 1.
pub fn slider_share(row: &Row) -> Option<(f32, bool)> {
    match row {
        Row::Slider { value, min, max, centred, .. } => Some((((value - min) / (max - min).max(1e-6)).clamp(0.0, 1.0), *centred)),
        _ => None,
    }
}

// ---- the equalizer ----

/// The equalizer screen's rows, from the settings: the switch, the presets, the pre-amp, every band,
/// then the rest of the chain.
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
    Crossfeed,
    Mono,
    Limiter,
    Ceiling,
}

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
    if graphic {
        rows.extend((0..p.eq_graphic.len()).map(EqRow::Slider));
    } else {
        rows.extend((0..p.eq_bands.len()).map(EqRow::Band));
        rows.push(EqRow::AddBand);
    }
    rows.extend([EqRow::Reset, EqRow::Balance, EqRow::Crossfeed, EqRow::Mono, EqRow::Limiter]);
    if p.limiter {
        rows.push(EqRow::Ceiling);
    }
    rows
}

impl EqRow {
    /// Its title and value, in the terminal's words (text.rs).
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
            EqRow::Crossfeed => ("Crossfeed".into(), if p.crossfeed_db > 0.0 { format!("{} dB", text::signed_db(p.crossfeed_db)) } else { "Off".into() }),
            EqRow::Mono => ("Mono".into(), on(p.mono)),
            EqRow::Limiter => ("Limiter".into(), on(p.limiter)),
            EqRow::Ceiling => ("Limiter ceiling".into(), text::ceiling(p.limiter_threshold_db)),
        }
    }

    /// ← or →; nothing when the value would stay as it is (held at the end of its range), so a key
    /// held there asks nothing of the settings or the engine.
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
                let to = if up { (at + 1).min(2) } else { at.saturating_sub(1) };
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
        let state = settings_model::state(prefs, settings_model::Output::default());
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
    fn the_clients_own_group_comes_first() {
        let v = SettingsView::default();
        assert_eq!(v.groups().iter().map(|g| g.id).collect::<Vec<_>>(), ["terminal", "sound", "playback", "library", "lyrics", "server", "storage", "about"]);
    }

    #[test]
    fn every_row_opens_or_steps_to_a_setting_the_core_takes_by_its_own_name() {
        let prefs = everything_on();
        let rows = every_row(&prefs);
        assert!(rows.len() > 40, "rows: {}", rows.len());
        for (group, row) in &rows {
            let mut v = SettingsView { pane: 1, ..Default::default() };
            v.page = Some(Page { title: group.to_string(), sections: vec![Section { title: String::new(), rows: vec![row.clone()] }] });
            v.page_of = None;
            match row {
                Row::Toggle { name, enabled: true, .. } if !name.starts_with('!') => {
                    let Some(Opened::Cmds(c)) = v.open(&prefs, true, true) else { panic!("{name} does not switch") };
                    assert!(matches!(&c[0], Cmd::Setting(n, _) if n == name), "{name}");
                }
                Row::Choice { name, enabled: true, options, .. } if !name.starts_with('!') => {
                    let Some(Opened::Overlay(Overlay::Picker { name: picked, options: shown, .. })) = v.open(&prefs, true, true) else { panic!("{name} offers no choice") };
                    assert_eq!(&picked, name);
                    assert_eq!(shown.len(), options.len());
                    let steps = [v.step(&prefs, true), v.step(&prefs, false)].concat();
                    assert!(steps.iter().all(|c| matches!(c, Cmd::Setting(n, _) if n == name)), "{name}");
                    // Each option is a value the core takes.
                    for (_, value) in options {
                        assert!(nori_core::settings::set_by_name(&prefs, name, value).is_some(), "{name} = {value}");
                    }
                }
                Row::Slider { name, level, .. } if !name.starts_with('!') => {
                    let c = v.step(&prefs, true);
                    assert!(matches!(&c[..], [Cmd::Level(l, _)] if Some(*l) == *level) || matches!(&c[..], [Cmd::Setting(n, _)] if n == name), "{name}");
                }
                _ => {}
            }
        }
    }

    #[test]
    fn a_choice_shows_its_value_in_the_terminals_words() {
        let d = StoredPrefs::default();
        let rows = every_row(&d);
        let shown = |name: &str| {
            rows.iter()
                .find_map(|(_, r)| match r {
                    Row::Choice { name: n, shown, .. } if n == name => Some(shown.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no {name}"))
        };
        assert_eq!(shown("replayGain"), "off");
        assert_eq!(shown("wifi"), "original");
        assert_eq!(shown("speed"), "1×");
        assert_eq!(quality("320:mp3"), "mp3 320k");
    }

    #[test]
    fn phone_only_settings_are_not_offered() {
        let names: Vec<String> = every_row(&everything_on())
            .into_iter()
            .filter_map(|(_, r)| match r {
                Row::Toggle { name, .. } | Row::Choice { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        for phone in ["swipeLeft", "swipeRight", "tapAction", "offload", "bitPerfect", "motionArtwork", "amoled", "dynamicColor", "softSleeve", "lyricsKeepScreenOn", "uiScale"] {
            assert!(!names.iter().any(|n| n == phone), "{phone} offered in a terminal");
        }
    }

    #[test]
    fn the_lyrics_sources_are_one_list_that_moves_and_switches() {
        // Every service is on out of the box; one is switched off here so there is an off row to press.
        let d = StoredPrefs::default();
        let prefs = StoredPrefs { lyrics_online: true, third_party_lookups: true, lyrics_on: d.lyrics_on.iter().filter(|n| *n != "GENIUS").cloned().collect(), ..d };
        let mut v = SettingsView::default();
        v.group.at = GROUPS.iter().position(|g| g.id == "lyrics").unwrap();
        v.pane = 1;
        let page = v.page(&prefs).clone();
        let lines = SettingsView::lines(&page);
        let ranked: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| matches!(l, Line::Row(Row::Ranked { .. }))).map(|(i, _)| i).collect();
        assert_eq!(ranked.len(), 16);
        assert!(lines.iter().all(|l| !matches!(l, Line::Row(Row::Ranked { title, .. }) if title == "?")), "every service has a name");
        let off = *ranked.iter().find(|i| matches!(lines[**i], Line::Row(Row::Ranked { on: false, .. }))).unwrap();
        let Line::Row(Row::Ranked { id, name, .. }) = lines[off] else { unreachable!() };
        let (id, name) = (id.clone(), name.clone());
        v.row.at = off;
        assert!(v.adjustable());
        assert!(matches!(&v.step(&prefs, false)[..], [Cmd::Setting(n, value)] if n == "lyricsMove" && *value == format!("{id}:-1")));
        let Some(Opened::Cmds(c)) = v.open(&prefs, true, true) else { panic!() };
        assert!(matches!(&c[..], [Cmd::Setting(n, value)] if *n == name && value == "true"));
        // Off, there is nothing to rank.
        let quiet = StoredPrefs { third_party_lookups: false, ..prefs };
        v.invalidate();
        let page = v.page(&quiet).clone();
        assert!(!SettingsView::lines(&page).iter().any(|l| matches!(l, Line::Row(Row::Ranked { .. }))));
    }

    #[test]
    fn the_graphic_equalizer_has_its_own_sliders_and_layouts() {
        let p = StoredPrefs { eq_mode: EqMode::Graphic, ..StoredPrefs::default() };
        let rows = eq_rows(&p);
        assert_eq!(rows.iter().filter(|r| matches!(r, EqRow::Slider(_))).count(), 10);
        assert!(!rows.iter().any(|r| matches!(r, EqRow::Band(_) | EqRow::AddBand)), "no parametric bands on the graphic one");
        assert_eq!(EqRow::Slider(0).words(&p), ("31.5".to_string(), "+0.0 dB".to_string()));
        assert!(matches!(EqRow::Slider(3).step(&p, true), Some(Cmd::Graphic(3, v)) if v == 0.5));
        assert!(matches!(EqRow::Layout.step(&p, true), Some(Cmd::Setting(n, v)) if n == "eqLayout" && v == "15"));
        assert!(EqRow::Layout.step(&p, false).is_none(), "ten is the fewest");
        assert!(matches!(EqRow::Mode.open(&p), Some(Cmd::Setting(n, v)) if n == "eqMode" && v == "PARAMETRIC"));
        assert!(eq_rows(&StoredPrefs::default()).contains(&EqRow::Layout), "a new install opens on the graphic equalizer");
        let parametric = eq_rows(&StoredPrefs { eq_mode: EqMode::Parametric, ..StoredPrefs::default() });
        assert!(parametric.contains(&EqRow::Mode) && !parametric.contains(&EqRow::Layout));
        // The effects on the sound page, the compressor's controls once it is on.
        let fx = effects(&Build { p: &StoredPrefs { compressor: true, ..StoredPrefs::default() }, s: &settings_model::state(&StoredPrefs::default(), settings_model::Output::default()) });
        assert!(fx.iter().any(|r| matches!(r, Row::Slider { level: Some(EqLevel::CompRatio), .. })));
    }

    #[test]
    fn the_equalizer_lists_every_band_and_steps_them_in_range() {
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
    }

}
