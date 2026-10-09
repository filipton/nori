//! The live settings: loaded from the app database's `settings` table (one row per key), kept in memory
//! and written back on the background thread after each change.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nori_db as db;
use nori_db::background;
use nori_model::alog;
use parking_lot::{Mutex, RwLock};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::settings::{load, save, set_band, set_by_name, set_level, EqLevel, PrefValue, SettingChange, SoundBand, SoundError, SoundSettings, StoredPrefs};

// Effect bits: what a settings change asks the player (or client) to apply again.
/// Which parts of the output chain may run, speed and pitch.
pub const APPLY_AUDIO: u32 = 1;
/// The ReplayGain volume.
pub const APPLY_GAIN: u32 = 2;
/// The transition plan for the playing song.
pub const REPLAN: u32 = 4;
/// The sound chain's values (bands, pre-amp, balance, crossfeed, limiter, effects).
pub const SOUND: u32 = 8;
/// The player's fades and high quality output.
pub const PLAYER: u32 = 16;
/// The stream cache's size limit, applied at once.
pub const CACHE_LIMIT: u32 = 32;

/// The open settings and the database they are kept in.
struct SettingsStore {
    db: Arc<Mutex<Connection>>,
    prefs: StoredPrefs,
    /// Bumped per change; a queued write skips itself when a newer one is queued behind it.
    writes: Arc<AtomicU64>,
}

fn to_json(v: &PrefValue) -> String {
    match v {
        PrefValue::Flag { v } => json!({ "b": v }),
        PrefValue::Number { v } => json!({ "i": v }),
        PrefValue::Big { v } => json!({ "l": v }),
        PrefValue::Decimal { v } => json!({ "f": v }),
        PrefValue::Text { v } => json!({ "s": v }),
    }
    .to_string()
}

fn from_json(text: &str) -> Option<PrefValue> {
    let Value::Object(o) = serde_json::from_str(text).ok()? else { return None };
    let (t, v) = o.into_iter().next()?;
    Some(match (t.as_str(), v) {
        ("b", Value::Bool(v)) => PrefValue::Flag { v },
        ("i", Value::Number(n)) => PrefValue::Number { v: n.as_i64()? as i32 },
        ("l", Value::Number(n)) => PrefValue::Big { v: n.as_i64()? },
        ("f", Value::Number(n)) => PrefValue::Decimal { v: n.as_f64()? as f32 },
        ("s", Value::String(v)) => PrefValue::Text { v },
        _ => return None,
    })
}

fn read(c: &Connection) -> rusqlite::Result<HashMap<String, PrefValue>> {
    let mut st = c.prepare("SELECT key, value FROM settings")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.filter_map(|r| r.ok()).filter_map(|(k, v)| Some((k, from_json(&v)?))).collect())
}

/// Whether any saved sound profile has a parametric equalizer; false without a profiles table.
fn profiles_parametric(c: &Connection) -> bool {
    let Ok(mut st) = c.prepare("SELECT json FROM profiles") else { return false };
    let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) else { return false };
    let jsons: Vec<String> = rows.filter_map(|r| r.ok()).collect();
    jsons.iter().any(|json| crate::settings::profile_parametric(json))
}

/// Replaces every stored setting in one transaction.
fn write(c: &mut Connection, prefs: &StoredPrefs) -> rusqlite::Result<()> {
    let put = save(prefs);
    let tx = c.transaction()?;
    tx.execute("DELETE FROM settings", [])?;
    {
        let mut st = tx.prepare("INSERT INTO settings(key, value) VALUES(?1, ?2)")?;
        for (k, v) in &put {
            st.execute(params![k, to_json(v)])?;
        }
    }
    tx.commit()
}

/// What a change from `a` to `b` asks the player to apply again: each changed setting's effect bits, plus
/// [`APPLY_AUDIO`] when the sound chain starts or stops being needed.
fn effects(a: &StoredPrefs, b: &StoredPrefs) -> u32 {
    let chain = if a.sound_chain_on() != b.sound_chain_on() { APPLY_AUDIO } else { 0 };
    crate::settings::ROWS.iter().filter(|r| r.effect != 0 && (r.changed)(a, b)).fold(chain, |e, r| e | r.effect)
}

impl SettingsStore {
    /// The settings in the app database at `db_path`; the defaults are written the first time.
    fn open(db_path: &str) -> nori_model::Result<Self> {
        let mut c = db::open_app(db_path)?;
        let raw = read(&c)?;
        let mut prefs = load(&raw);
        // Stored before the equalizer had a graphic mode: a saved profile with a parametric curve keeps
        // the parametric equalizer too. The choice is written at once.
        let chosen = raw.contains_key(crate::settings::EQ_MODE_KEY);
        if !raw.is_empty() && !chosen && profiles_parametric(&c) {
            prefs.eq_mode = crate::settings::EqMode::Parametric;
        }
        if raw.is_empty() || !chosen {
            write(&mut c, &prefs)?;
        }
        Ok(SettingsStore { db: Arc::new(Mutex::new(c)), prefs, writes: Arc::new(AtomicU64::new(0)) })
    }

    /// Replaces the settings with what `make` makes of them and queues the write. Returns the effect
    /// bits, or None when nothing changed.
    fn edit(&mut self, make: impl FnOnce(&StoredPrefs) -> StoredPrefs) -> Option<u32> {
        let prefs = make(&self.prefs);
        if prefs == self.prefs {
            return None;
        }
        let effect = effects(&self.prefs, &prefs);
        self.prefs = prefs;
        let (db, writes, prefs) = (self.db.clone(), self.writes.clone(), self.prefs.clone());
        let n = writes.fetch_add(1, Ordering::SeqCst) + 1;
        background::run(move || {
            if writes.load(Ordering::SeqCst) != n {
                return;
            }
            if let Err(e) = write(&mut db.lock(), &prefs) {
                alog::info(&format!("settings: could not write: {e}"));
            }
        });
        Some(effect)
    }

    fn edit_sound(&mut self, sound: SoundSettings) -> Option<u32> {
        self.edit(|p| p.clone().with_sound(sound))
    }

    fn edit_band(&mut self, index: u32, asked: SoundBand) -> Option<(u32, SoundBand)> {
        let s = set_band(self.prefs.sound(), index, asked);
        let kept = *s.eq_bands.get(index as usize)?;
        Some((self.edit_sound(s)?, kept))
    }

    fn edit_graphic(&mut self, index: u32, gain_db: f32) -> Option<(u32, f32)> {
        let s = crate::settings::set_graphic(self.prefs.sound(), index, gain_db);
        let kept = *s.eq_graphic.get(index as usize)?;
        Some((self.edit_sound(s)?, kept))
    }

    fn edit_level(&mut self, level: EqLevel, value: f32) -> Option<(u32, f32)> {
        let s = set_level(self.prefs.sound(), level, value);
        let kept = match level {
            EqLevel::Preamp => s.eq_preamp_db.unwrap_or(value),
            other => other.of(&s),
        };
        Some((self.edit_sound(s)?, kept))
    }

    /// The active server's own settings (`SettingChange::server`) are returned but not kept: the
    /// platform applies them through its server update.
    fn edit_by_name(&mut self, name: &str, value: &str) -> Option<SettingChange> {
        let mut change = None;
        let effect = self.edit(|p| {
            let Some(c) = set_by_name(p, name, value) else { return p.clone() };
            let next = if c.server { p.clone() } else { c.prefs.clone() };
            change = Some(c);
            next
        });
        change.map(|c| SettingChange { effect: effect.unwrap_or(0), ..c })
    }

    fn sound_tool(&mut self, tool: SoundTool) -> Result<Option<SoundChange>, SoundError> {
        let sound = tool.apply(self.prefs.sound())?;
        Ok(self.edit_sound(sound.clone()).map(|effect| SoundChange { sound, effect }))
    }

    fn app_value(&self, key: &str) -> Option<String> {
        self.db.lock().query_row("SELECT value FROM app_kv WHERE key=?1", [key], |r| r.get(0)).optional().ok().flatten()
    }
}

/// The live settings of one app, empty (the defaults) until opened, and the beat model's file, which
/// follows its switch. The platform holds one for the app and hands it to the queue's session.
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct Settings {
    kept: RwLock<Option<SettingsStore>>,
    pub model: nori_automix::beat_model::ModelFile,
}

impl Default for Settings {
    fn default() -> Self {
        use nori_automix::beat_model::{ModelFile, BEAT_THIS};
        Settings { kept: RwLock::default(), model: ModelFile::new(&BEAT_THIS) }
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Settings {
    /// Empty (the defaults) until [`Settings::open`].
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new() -> Arc<Settings> {
        Arc::default()
    }

    /// Opens the settings kept in the app database at `db_path` and makes them the live ones.
    pub fn open(&self, db_path: &str) -> nori_model::Result<StoredPrefs> {
        let store = SettingsStore::open(db_path)?;
        let prefs = store.prefs.clone();
        self.write(|k| *k = Some(store));
        Ok(prefs)
    }

    /// Replaces the settings. Returns the effect bits; 0 when nothing changed or the settings are not open.
    pub fn put(&self, prefs: StoredPrefs) -> u32 {
        self.with_store(|s| s.edit(|_| prefs)).unwrap_or(0)
    }

    /// Applies an equalizer tool to the live settings. None when nothing changed or the settings are not
    /// open; an import without filters is an error.
    pub fn sound_tool(&self, tool: SoundTool) -> Result<Option<SoundChange>, SoundError> {
        self.write(|k| k.as_mut().map_or(Ok(None), |s| s.sound_tool(tool)))
    }
}

impl Settings {
    /// `f` over the open settings, the beat model's file following its switch under the same lock, so
    /// concurrent edits reach it in the order they were kept.
    fn write<R>(&self, f: impl FnOnce(&mut Option<SettingsStore>) -> R) -> R {
        let mut k = self.kept.write();
        let r = f(&mut k);
        if let Some(s) = k.as_ref() {
            self.model.switched(s.prefs.auto_mix_better_beats);
        }
        r
    }

    fn with_store<R>(&self, f: impl FnOnce(&mut SettingsStore) -> Option<R>) -> Option<R> {
        self.write(|k| k.as_mut().and_then(f))
    }

    /// A value from the app database's `app_kv` table; None before the settings are open.
    pub fn app_value(&self, key: &str) -> Option<String> {
        self.kept.read().as_ref()?.app_value(key)
    }

    /// The app database the settings were opened from, for other app-wide tables; None before it is open.
    pub fn app_db(&self) -> Option<Arc<Mutex<Connection>>> {
        self.kept.read().as_ref().map(|k| k.db.clone())
    }

    /// Stores an `app_kv` value on the background thread.
    pub fn keep_app_value(&self, key: &'static str, value: String) {
        let Some(db) = self.app_db() else { return };
        background::run(move || {
            if let Err(e) = db.lock().execute("INSERT OR REPLACE INTO app_kv(key, value) VALUES(?1, ?2)", params![key, value]) {
                alog::info(&format!("{key}: could not write: {e}"));
            }
        });
    }

    /// Removes an `app_kv` value on the background thread.
    pub fn forget_app_value(&self, key: &'static str) {
        let Some(db) = self.app_db() else { return };
        background::run(move || {
            if let Err(e) = db.lock().execute("DELETE FROM app_kv WHERE key=?1", [key]) {
                alog::info(&format!("{key}: could not remove: {e}"));
            }
        });
    }

    /// One parametric band changed (`settings::set_band`): the effect bits and the band as kept (held in
    /// range); None when nothing changed.
    pub fn edit_band(&self, index: u32, asked: SoundBand) -> Option<(u32, SoundBand)> {
        self.with_store(|s| s.edit_band(index, asked))
    }

    /// One graphic slider changed (`settings::set_graphic`): the effect bits and the value as kept.
    pub fn edit_graphic(&self, index: u32, gain_db: f32) -> Option<(u32, f32)> {
        self.with_store(|s| s.edit_graphic(index, gain_db))
    }

    /// A level slider changed (`settings::set_level`): the effect bits and the value as kept (held in
    /// range and snapped).
    pub fn edit_level(&self, level: EqLevel, value: f32) -> Option<(u32, f32)> {
        self.with_store(|s| s.edit_level(level, value))
    }

    /// A change by name (`settings::set_by_name`), kept, with the settings after it and the effect bits.
    /// None for an unknown name. Before the settings are open it is applied to the defaults and not kept.
    pub fn edit_by_name(&self, name: &str, value: &str) -> Option<SettingChange> {
        self.write(|k| match k.as_mut() {
            Some(s) => s.edit_by_name(name, value),
            None => set_by_name(&StoredPrefs::default(), name, value),
        })
    }

    /// A copy of the live settings; None before they are open.
    pub fn current(&self) -> Option<StoredPrefs> {
        self.with_prefs(StoredPrefs::clone)
    }

    /// `f` applied to the live settings without copying them; None before they are open.
    pub fn with_prefs<R>(&self, f: impl FnOnce(&StoredPrefs) -> R) -> Option<R> {
        self.kept.read().as_ref().map(|k| f(&k.prefs))
    }

    /// `f` applied to the live settings, or to the defaults before they are open.
    pub fn prefs<R>(&self, f: impl FnOnce(&StoredPrefs) -> R) -> R {
        match self.kept.read().as_ref() {
            Some(k) => f(&k.prefs),
            None => f(&StoredPrefs::default()),
        }
    }
}

/// An equalizer screen tool (sliders are [`Settings::edit_band`] and [`Settings::edit_level`]).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SoundTool {
    Preset { preset: nori_model::NamedPreset },
    AutoPreamp { automatic: bool },
    AddBand,
    RemoveBand { index: u32 },
    ResetBands,
    /// AutoEQ "ParametricEQ.txt" or Equalizer APO text.
    Import { text: String },
}

/// The sound after a [`SoundTool`], and the effect bits.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SoundChange {
    pub sound: SoundSettings,
    pub effect: u32,
}

impl SoundTool {
    fn apply(self, s: SoundSettings) -> Result<SoundSettings, SoundError> {
        use crate::settings as st;
        Ok(match self {
            SoundTool::Preset { preset } => st::apply_preset(s, &preset),
            SoundTool::AutoPreamp { automatic } => st::set_auto_preamp(s, automatic),
            SoundTool::AddBand => st::add_band(s),
            SoundTool::RemoveBand { index } => st::remove_band(s, index),
            SoundTool::ResetBands => st::eq_reset_bands(s),
            SoundTool::Import { text } => st::import(s, &text)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{band_from, sound_json, EqMode, GainMode, SavedServer, EQ_MODE_KEY, EQ_RANGES};

    fn open(dir: &nori_testdir::TempDir) -> SettingsStore {
        SettingsStore::open(&dir.join("nori.db").display().to_string()).unwrap()
    }

    #[test]
    fn round_trips() {
        for v in [
            PrefValue::Flag { v: true },
            PrefValue::Number { v: -3 },
            PrefValue::Big { v: 1 << 40 },
            PrefValue::Decimal { v: 0.1 },
            PrefValue::Text { v: "x\"y".into() },
        ] {
            assert_eq!(from_json(&to_json(&v)), Some(v));
        }

        // Saved settings load back.
        let dir = nori_testdir::TempDir::new("settings");
        let s = open(&dir);
        assert_eq!(s.prefs, StoredPrefs::default());
        write(&mut s.db.lock(), &StoredPrefs { fade_ms: 400, ..s.prefs.clone() }).unwrap();
        drop(s);
        assert_eq!(open(&dir).prefs.fade_ms, 400);
    }

    #[test]
    fn effects_and_rebuilds() {
        let a = StoredPrefs::default();
        let cases = [
            (StoredPrefs { amoled: !a.amoled, ..a.clone() }, 0),
            (StoredPrefs { eq_bands: vec![band_from(0, 100.0, 3.0, 1.0, 0)], ..a.clone() }, SOUND),
            (StoredPrefs { eq_enabled: true, ..a.clone() }, APPLY_AUDIO | SOUND),
            (StoredPrefs { replay_gain: GainMode::Track, ..a.clone() }, APPLY_GAIN | REPLAN | SOUND),
            (StoredPrefs { loudness_target: -14.0, ..a.clone() }, APPLY_GAIN),
            (StoredPrefs { gain_boost_db: 6.0, ..a.clone() }, APPLY_GAIN | SOUND),
            (StoredPrefs { crossfade_sec: 6, ..a.clone() }, APPLY_AUDIO | REPLAN),
            (StoredPrefs { auto_mix_bass_swap: !a.auto_mix_bass_swap, ..a.clone() }, REPLAN),
            (StoredPrefs { fade_ms: a.fade_ms + 300, ..a.clone() }, PLAYER),
            (StoredPrefs { hi_res: !a.hi_res, ..a.clone() }, PLAYER),
            (StoredPrefs { previous_always_skips: true, ..a.clone() }, PLAYER),
            (StoredPrefs { cache_mb: 4096, ..a.clone() }, CACHE_LIMIT),
        ];
        for (b, want) in cases {
            assert_eq!(effects(&a, &b), want, "{b:?}");
        }

        // Balance and crossfeed rebuild only when chain toggles.
        let a = StoredPrefs::default();
        assert!(!a.sound_chain_on());
        let off_centre = StoredPrefs { balance: -0.4, ..a.clone() };
        assert_eq!(effects(&a, &off_centre), APPLY_AUDIO | SOUND, "chain starts");
        assert_eq!(effects(&off_centre, &StoredPrefs { balance: -0.5, ..a.clone() }), SOUND, "drag step");
        assert_eq!(effects(&off_centre, &a), APPLY_AUDIO | SOUND, "chain stops");
        let feed = StoredPrefs { crossfeed_db: 3.0, ..a.clone() };
        assert_eq!(effects(&a, &feed), APPLY_AUDIO | SOUND);
        assert_eq!(effects(&feed, &StoredPrefs { crossfeed_db: 4.5, ..a.clone() }), SOUND);
        let eq = StoredPrefs { eq_enabled: true, ..a.clone() };
        assert_eq!(effects(&eq, &StoredPrefs { balance: 0.3, ..eq.clone() }), SOUND, "chain already running");
        assert_eq!(effects(&eq, &StoredPrefs { limiter_threshold_db: -3.0, ..eq.clone() }), SOUND);
    }

    #[test]
    fn slider_edits() {
        let dir = nori_testdir::TempDir::new("settings-edit");
        let mut s = open(&dir);
        let band = s.prefs.eq_bands[2];
        let (effect, kept) = s.edit_band(2, SoundBand { gain_db: 99.0, ..band }).unwrap();
        assert_eq!(effect, SOUND);
        assert_eq!(kept.gain_db, EQ_RANGES.gain.max, "held in range");
        assert_eq!(s.prefs.eq_bands[2], kept);
        assert_eq!(s.edit_band(2, kept), None, "unchanged");
        assert_eq!(s.edit_band(99, band), None, "no such band");
        assert_eq!(s.edit_graphic(0, 20.0), Some((SOUND, 12.0)), "held in range");
        assert_eq!(s.prefs.eq_graphic[0], 12.0);
        assert_eq!(s.edit_level(EqLevel::Balance, 0.02), None, "snaps to the centre");
        assert_eq!(s.edit_level(EqLevel::Balance, -0.5), Some((APPLY_AUDIO | SOUND, -0.5)));
        assert_eq!(s.prefs.balance, -0.5);
        assert_eq!(s.edit_level(EqLevel::Balance, -0.6), Some((SOUND, -0.6)));
        assert_eq!(s.edit_level(EqLevel::ReplayGainPreamp, 20.0), Some((APPLY_GAIN, 6.0)));
        assert_eq!(s.prefs.preamp_db, 6.0);
    }

    /// An install from before the graphic equalizer: the mode is chosen from its settings and sound
    /// profiles, and written at once.
    #[test]
    fn old_install_keeps_parametric() {
        let open_with = |name: &str, prefs: &StoredPrefs, profile: Option<String>| {
            let dir = nori_testdir::TempDir::new(name);
            let path = dir.join("nori.db").display().to_string();
            {
                let mut c = db::open_app(&path).unwrap();
                write(&mut c, &StoredPrefs { eq_mode: EqMode::Graphic, ..prefs.clone() }).unwrap();
                c.execute("DELETE FROM settings WHERE key = ?1", [EQ_MODE_KEY]).unwrap();
                if let Some(json) = profile {
                    c.execute_batch("CREATE TABLE profiles(name TEXT PRIMARY KEY, json TEXT NOT NULL, outputs TEXT NOT NULL DEFAULT '') WITHOUT ROWID").unwrap();
                    c.execute("INSERT INTO profiles(name, json) VALUES('Mine', ?1)", [json]).unwrap();
                }
            }
            let opened = SettingsStore::open(&path).unwrap().prefs.eq_mode;
            let raw = read(&db::open_app(&path).unwrap()).unwrap();
            (opened, raw.contains_key(EQ_MODE_KEY), load(&raw).eq_mode)
        };
        let d = StoredPrefs::default();
        assert_eq!(open_with("settings-old-flat", &d, None), (EqMode::Graphic, true, EqMode::Graphic));
        let with_bands = StoredPrefs { eq_enabled: true, eq_bands: vec![band_from(1, 100.0, 6.0, 0.7, 0)], ..d.clone() };
        assert_eq!(open_with("settings-old-bands", &with_bands, None), (EqMode::Parametric, true, EqMode::Parametric));
        let profile = sound_json(&with_bands.sound());
        assert_eq!(open_with("settings-old-profile", &d, Some(profile)), (EqMode::Parametric, true, EqMode::Parametric));
        let dir = nori_testdir::TempDir::new("settings-new-install");
        let s = open(&dir);
        assert_eq!(s.prefs.eq_mode, EqMode::Graphic);
        assert!(read(&s.db.lock()).unwrap().contains_key(EQ_MODE_KEY), "a new install writes the mode");
    }

    #[test]
    fn edit_by_name_and_sound_tools() {
        let dir = nori_testdir::TempDir::new("settings-by-name");
        let mut s = open(&dir);
        let c = s.edit_by_name("limiter", "true").unwrap();
        assert!(c.prefs.limiter && s.prefs.limiter);
        assert_eq!(c.effect, APPLY_AUDIO | SOUND);
        assert_eq!(c.prefs, s.prefs);
        assert_eq!(s.edit_by_name("limiter", "true").unwrap().effect, 0, "unchanged");
        assert_eq!(s.edit_by_name("cacheMb", "512").unwrap().effect, CACHE_LIMIT);
        assert!(s.edit_by_name("noSuchSetting", "1").is_none());
        // The active server's own settings are returned, not kept.
        let with_server = StoredPrefs { servers: vec![SavedServer { id: "s1".into(), ..Default::default() }], active_server_id: "s1".into(), ..s.prefs.clone() };
        s.edit(|_| with_server.clone());
        let c = s.edit_by_name("musicFolder", "7").unwrap();
        assert!(c.server);
        assert_eq!(c.prefs.servers[0].music_folder_id, "7");
        assert_eq!(s.prefs, with_server);
        assert_eq!(c.effect, 0);

        let tool = s.sound_tool(SoundTool::AddBand).unwrap().unwrap();
        assert_eq!(tool.sound, s.prefs.sound());
        assert_eq!(tool.effect, SOUND);
        let n = tool.sound.eq_bands.len();
        assert_eq!(s.sound_tool(SoundTool::RemoveBand { index: n as u32 - 1 }).unwrap().unwrap().sound.eq_bands.len(), n - 1);
        assert!(matches!(s.sound_tool(SoundTool::Import { text: "nothing here".into() }), Err(SoundError::NoFilters)));
        assert_eq!(s.prefs.eq_bands.len(), n - 1, "a failed import changes nothing");
        assert_eq!(s.sound_tool(SoundTool::RemoveBand { index: 999 }).unwrap(), None);
    }
}
