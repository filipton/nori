//! The settings, owned here: read once when the app starts, kept in memory, and written back to the
//! app's database (`settings`, one row per key, the app's and not a server's) whenever they change. The
//! platform only shows and changes them. Codec, defaults and ranges are settings.rs's.

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

struct Kept {
    db: Arc<Mutex<Connection>>,
    prefs: StoredPrefs,
}

static KEPT: RwLock<Option<Kept>> = RwLock::new(None);
/// Bumped on every change; a write that finds a newer change waiting leaves the writing to that one.
static CHANGES: AtomicU64 = AtomicU64::new(0);

fn to_json(v: &PrefValue) -> String {
    match v {
        PrefValue::Flag { v } => json!({ "b": v }),
        PrefValue::Number { v } => json!({ "i": v }),
        PrefValue::Big { v } => json!({ "l": v }),
        PrefValue::Decimal { v } => json!({ "f": v }),
        PrefValue::Text { v } => json!({ "s": v }),
        PrefValue::Texts { v } => json!({ "ss": v }),
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
        ("ss", Value::Array(a)) => PrefValue::Texts { v: a.into_iter().filter_map(|x| x.as_str().map(str::to_string)).collect() },
        _ => return None,
    })
}

fn read(c: &Connection) -> rusqlite::Result<HashMap<String, PrefValue>> {
    let mut st = c.prepare("SELECT key, value FROM settings")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.filter_map(|r| r.ok()).filter_map(|(k, v)| Some((k, from_json(&v)?))).collect())
}

/// Every value these settings store, in one transaction; keys no longer written go.
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

/// The settings kept in the app's database at `db_path`. The first time there are none, the defaults
/// become them.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn settings_open(db_path: String) -> nori_model::Result<StoredPrefs> {
    let mut c = db::open_app(&db_path)?;
    let raw = read(&c)?;
    let prefs = load(&raw);
    if raw.is_empty() {
        write(&mut c, &prefs)?;
    }
    let mut k = KEPT.write();
    *k = Some(Kept { db: Arc::new(Mutex::new(c)), prefs: prefs.clone() });
    changed(&prefs);
    drop(k);
    nori_automix::planner::settings_from(|| with_prefs(StoredPrefs::transition_prefs));
    Ok(prefs)
}

/// One of the app's own values (`app_kv`), read from the database the settings are kept in; none before
/// the settings are open.
pub fn app_value(key: &str) -> Option<String> {
    let db = KEPT.read().as_ref()?.db.clone();
    let c = db.lock();
    c.query_row("SELECT value FROM app_kv WHERE key=?1", [key], |r| r.get(0)).optional().ok().flatten()
}

/// The app's database the settings were opened from, for the few app-wide tables kept beside them
/// (perf_log.rs); none before the settings are open.
pub fn app_db() -> Option<Arc<Mutex<Connection>>> {
    KEPT.read().as_ref().map(|k| k.db.clone())
}

/// One of the app's own values kept, written on the core's background thread.
pub fn keep_app_value(key: &'static str, value: String) {
    let Some(db) = KEPT.read().as_ref().map(|k| k.db.clone()) else { return };
    background::run(move || {
        if let Err(e) = db.lock().execute("INSERT OR REPLACE INTO app_kv(key, value) VALUES(?1, ?2)", params![key, value]) {
            alog::info(&format!("{key}: could not write: {e}"));
        }
    });
}

/// [`settings_put`]'s answer: what the platform's player has to apply again. The transition planner's
/// settings follow by themselves.
/// Which parts of the output chain may run, speed and pitch.
pub const APPLY_AUDIO: u32 = 1;
/// The ReplayGain volume.
pub const APPLY_GAIN: u32 = 2;
/// A plan already made for the song playing is asked for again.
pub const REPLAN: u32 = 4;
/// The sound chain's values moved (a band, the pre-amp, balance, crossfeed, the limiter): the player
/// (nori-engine, which keeps its own chain) is handed them. Only a change in which parts may run is
/// [`APPLY_AUDIO`] as well.
pub const SOUND: u32 = 8;
/// The fades on play, pause and switches, or high quality output: the player (nori-engine, which keeps
/// its own copy) is handed them, or it went on with the ones it started with until the app was started
/// again.
pub const PLAYER: u32 = 16;

/// What a change from `a` to `b` asks of the player: each setting's own bits from the table
/// (settings.rs), and the output chain rebuilt when the sound chain starts or stops being needed.
/// Balance and crossfeed are read by the chain as it runs, so dragging them matters only then: a drag's
/// every step used to rebuild the audio policy, the transitions and the track selection.
fn effects(a: &StoredPrefs, b: &StoredPrefs) -> u32 {
    let chain = if a.sound_chain_on() != b.sound_chain_on() { APPLY_AUDIO } else { 0 };
    crate::settings::ROWS.iter().filter(|r| r.effect != 0 && (r.changed)(a, b)).fold(chain, |e, r| e | r.effect)
}

/// The settings changed; kept now and written on the core's background thread. Returns what the
/// platform's player has to apply again ([`APPLY_AUDIO`], [`APPLY_GAIN`], [`REPLAN`], [`SOUND`], [`PLAYER`]); 0 for a change
/// only screens care about.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn settings_put(prefs: StoredPrefs) -> u32 {
    edit(|_| prefs).unwrap_or(0)
}

/// The kept settings replaced by what `make` makes of them, and written as [`settings_put`] writes
/// them. Returns what the player has to apply again, or none when nothing changed.
fn edit(make: impl FnOnce(&StoredPrefs) -> StoredPrefs) -> Option<u32> {
    let (db, effect) = {
        let mut k = KEPT.write();
        let k = k.as_mut()?;
        let prefs = make(&k.prefs);
        if k.prefs == prefs {
            return None;
        }
        let effect = effects(&k.prefs, &prefs);
        // Told under the lock: two changes on two threads reach what follows them in the order they were
        // kept, never the older one last.
        changed(&prefs);
        k.prefs = prefs;
        (k.db.clone(), effect)
    };
    let change = CHANGES.fetch_add(1, Ordering::SeqCst) + 1;
    background::run(move || {
        // A newer change is queued behind this one and writes everything anyway.
        if CHANGES.load(Ordering::SeqCst) != change {
            return;
        }
        if let Some(p) = current() {
            if let Err(e) = write(&mut db.lock(), &p) {
                alog::info(&format!("settings: could not write: {e}"));
            }
        }
    });
    Some(effect)
}

/// One band of the equalizer moved (`settings::set_band`), edited where the settings are kept: what the
/// player has to apply again and the band as it was kept, held in its ranges; none when nothing changed.
pub fn edit_band(index: u32, asked: SoundBand) -> Option<(u32, SoundBand)> {
    let mut kept = asked;
    let effect = edit(|p| {
        let s = set_band(p.sound(), index, asked);
        if let Some(k) = s.eq_bands.get(index as usize) {
            kept = *k;
        }
        p.clone().with_sound(s)
    })?;
    Some((effect, kept))
}

/// One graphic equalizer slider moved (`settings::set_graphic`), edited where the settings are kept:
/// what the player has to apply again and the value as it was kept; none when nothing changed.
pub fn edit_graphic(index: u32, gain_db: f32) -> Option<(u32, f32)> {
    let mut kept = gain_db;
    let effect = edit(|p| {
        let s = crate::settings::set_graphic(p.sound(), index, gain_db);
        if let Some(k) = s.eq_graphic.get(index as usize) {
            kept = *k;
        }
        p.clone().with_sound(s)
    })?;
    Some((effect, kept))
}

/// Pre-amp, balance, limiter ceiling or crossfeed moved (`settings::set_level`), edited where the
/// settings are kept: what the player has to apply again and the value as it was kept, held in range
/// and snapped; none when nothing changed.
pub fn edit_level(level: EqLevel, value: f32) -> Option<(u32, f32)> {
    let mut kept = value;
    let effect = edit(|p| {
        let s = set_level(p.sound(), level, value);
        kept = match level {
            EqLevel::Preamp => s.eq_preamp_db.unwrap_or(value),
            other => other.of(&s),
        };
        p.clone().with_sound(s)
    })?;
    Some((effect, kept))
}

/// A change by name (`settings::set_by_name`) made where the settings are kept, answered once with the
/// settings after it and what the player has to apply again: the platform takes them in and sends
/// nothing back. The active server's own settings are left as they are (see `SettingChange::server`).
/// `None` for a name that is not a setting.
pub fn edit_by_name(name: &str, value: &str) -> Option<SettingChange> {
    let mut change = None;
    let effect = edit(|p| {
        let Some(c) = set_by_name(p, name, value) else { return p.clone() };
        let next = if c.server { p.clone() } else { c.prefs.clone() };
        change = Some(c);
        next
    });
    match change {
        Some(c) => Some(SettingChange { effect: effect.unwrap_or(0), ..c }),
        // Before the app opened them there is nothing to keep: the change is said, against the defaults.
        None if with_prefs(|_| ()).is_none() => set_by_name(&StoredPrefs::default(), name, value),
        None => None,
    }
}

/// One of the equalizer screen's tools (not a slider: those are [`edit_band`] and [`edit_level`]).
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

/// What a [`SoundTool`] made of the sound, and what the player has to apply again.
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

/// One of the equalizer screen's tools used on the settings where they are kept: only the sound part
/// comes back, and the platform sends nothing back. `None` when nothing changed (or before the app
/// opened the settings); an import with no filters in it says so.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn settings_sound_tool(tool: SoundTool) -> Result<Option<SoundChange>, SoundError> {
    let mut made = Ok(None);
    let effect = edit(|p| match tool.apply(p.sound()) {
        Ok(s) => {
            made = Ok(Some(s.clone()));
            p.clone().with_sound(s)
        }
        Err(e) => {
            made = Err(e);
            p.clone()
        }
    });
    Ok(match (made?, effect) {
        (Some(sound), Some(effect)) => Some(SoundChange { sound, effect }),
        _ => None,
    })
}

/// What in the core follows the settings by itself, told at once.
fn changed(prefs: &StoredPrefs) {
    nori_automix::planner::settings_changed(prefs.transition_prefs());
    nori_automix::beat_model::switched(prefs.auto_mix_better_beats);
}

/// The settings as they are kept now, for a Rust client that edits them and puts them back; none
/// before the app opened them.
pub fn settings_current() -> Option<StoredPrefs> {
    current()
}

/// The settings as they are now, for the core's own decisions; none before the app opened them.
pub fn current() -> Option<StoredPrefs> {
    with_prefs(StoredPrefs::clone)
}

/// One answer from the settings as they are now, without copying them; none before the app opened them.
pub fn with_prefs<R>(f: impl FnOnce(&StoredPrefs) -> R) -> Option<R> {
    KEPT.read().as_ref().map(|k| f(&k.prefs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_value_survives_the_database() {
        for v in [
            PrefValue::Flag { v: true },
            PrefValue::Number { v: -3 },
            PrefValue::Big { v: 1 << 40 },
            PrefValue::Decimal { v: 0.1 },
            PrefValue::Text { v: "x\"y".into() },
            PrefValue::Texts { v: vec!["1".into(), "2".into()] },
        ] {
            assert_eq!(from_json(&to_json(&v)), Some(v));
        }
    }

    #[test]
    fn only_what_the_player_uses_asks_it_to_apply_again() {
        let a = StoredPrefs::default();
        let theme = StoredPrefs { amoled: !a.amoled, ..a.clone() };
        assert_eq!(effects(&a, &theme), 0, "a screen's setting");
        let bands = StoredPrefs { eq_bands: vec![crate::settings::band_from(0, 100.0, 3.0, 1.0, 0)], ..a.clone() };
        assert_eq!(effects(&a, &bands), SOUND, "the sound chain follows its bands by itself");
        assert_eq!(effects(&a, &StoredPrefs { eq_enabled: true, ..a.clone() }), APPLY_AUDIO | SOUND);
        assert_eq!(effects(&a, &StoredPrefs { replay_gain: crate::settings::GainMode::Track, ..a.clone() }), APPLY_GAIN | REPLAN | SOUND, "and whether songs may be turned up");
        assert_eq!(effects(&a, &StoredPrefs { loudness_target: -14.0, ..a.clone() }), APPLY_GAIN);
        assert_eq!(effects(&a, &StoredPrefs { gain_boost_db: 0.0, ..a.clone() }), APPLY_GAIN | SOUND, "the chain reads floats and limits only while songs may be turned up");
        assert_eq!(effects(&a, &StoredPrefs { crossfade_sec: 6, ..a.clone() }), APPLY_AUDIO | REPLAN);
        assert_eq!(effects(&a, &StoredPrefs { auto_mix_bass_swap: !a.auto_mix_bass_swap, ..a.clone() }), REPLAN);
        // Read once when the player started, these went unheard until the app was started again.
        assert_eq!(effects(&a, &StoredPrefs { fade_ms: a.fade_ms + 300, ..a.clone() }), PLAYER, "the fades");
        assert_eq!(effects(&a, &StoredPrefs { hi_res: !a.hi_res, ..a.clone() }), PLAYER, "high quality output");
    }

    #[test]
    fn dragging_balance_or_crossfeed_only_rebuilds_when_the_chain_starts_or_stops() {
        let a = StoredPrefs::default();
        assert!(!a.sound_chain_on(), "the defaults run no chain");
        let off_centre = StoredPrefs { balance: -0.4, ..a.clone() };
        assert_eq!(effects(&a, &off_centre), APPLY_AUDIO | SOUND, "the chain starts");
        assert_eq!(effects(&off_centre, &StoredPrefs { balance: -0.5, ..a.clone() }), SOUND, "a drag step: the chain reads it as it runs");
        assert_eq!(effects(&off_centre, &a), APPLY_AUDIO | SOUND, "back in the middle, the chain stops");
        let feed = StoredPrefs { crossfeed_db: 3.0, ..a.clone() };
        assert_eq!(effects(&a, &feed), APPLY_AUDIO | SOUND);
        assert_eq!(effects(&feed, &StoredPrefs { crossfeed_db: 4.5, ..a.clone() }), SOUND);
        let eq = StoredPrefs { eq_enabled: true, ..a.clone() };
        assert_eq!(effects(&eq, &StoredPrefs { balance: 0.3, ..eq.clone() }), SOUND, "the chain was running already");
        assert_eq!(effects(&eq, &StoredPrefs { limiter_threshold_db: -3.0, ..eq.clone() }), SOUND);
    }

    /// The tests that open the store take turns: it is one for the whole process.
    static OPEN: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn a_slider_edits_the_kept_settings_in_place() {
        let _turn = OPEN.lock();
        let dir = nori_testdir::TempDir::new("settings-edit");
        settings_open(dir.join("nori.db").display().to_string()).unwrap();
        let band = current().unwrap().eq_bands[2];
        let (effect, kept) = edit_band(2, SoundBand { gain_db: 99.0, ..band }).unwrap();
        assert_eq!(effect, SOUND, "the sound chain follows its bands by itself");
        assert_eq!(kept.gain_db, crate::settings::EQ_RANGES.gain.max, "held in range");
        assert_eq!(current().unwrap().eq_bands[2], kept);
        assert_eq!(edit_band(2, kept), None, "the same band again changes nothing");
        assert_eq!(edit_band(99, band), None, "a band that is not there");
        assert_eq!(edit_level(EqLevel::Balance, 0.02), None, "near the middle is the middle, as it was");
        assert_eq!(edit_level(EqLevel::Balance, -0.5), Some((APPLY_AUDIO | SOUND, -0.5)));
        assert_eq!(current().unwrap().balance, -0.5);
        assert_eq!(edit_level(EqLevel::Balance, -0.6), Some((SOUND, -0.6)), "a drag step");
        assert_eq!(edit_level(EqLevel::ReplayGainPreamp, 20.0), Some((APPLY_GAIN, 6.0)), "the overall level, held in range");
        assert_eq!(current().unwrap().preamp_db, 6.0);
    }

    #[test]
    fn a_change_by_name_is_kept_where_it_is_made_and_answered_once() {
        let _turn = OPEN.lock();
        let dir = nori_testdir::TempDir::new("settings-by-name");
        settings_open(dir.join("nori.db").display().to_string()).unwrap();
        let c = edit_by_name("limiter", "true").unwrap();
        assert!(c.prefs.limiter && current().unwrap().limiter, "kept, with nothing put back");
        assert_eq!(c.effect, APPLY_AUDIO | SOUND);
        assert_eq!(c.prefs, current().unwrap());
        let again = edit_by_name("limiter", "true").unwrap();
        assert_eq!(again.effect, 0, "nothing changed");
        assert_eq!(edit_by_name("cacheMb", "512").map(|c| (c.apply_cache_limit, c.effect)), Some((true, 0)));
        assert!(edit_by_name("noSuchSetting", "1").is_none());
        // The active server's own settings go through the platform's server update, which connects again.
        let before = current().unwrap();
        let mut with_server = before.clone();
        with_server.servers = vec![crate::settings::SavedServer { id: "s1".into(), ..Default::default() }];
        with_server.active_server_id = "s1".into();
        settings_put(with_server.clone());
        let c = edit_by_name("musicFolder", "7").unwrap();
        assert!(c.server);
        assert_eq!(c.prefs.servers[0].music_folder_id, "7", "the change is said");
        assert_eq!(current().unwrap(), with_server, "but not kept here");
        assert_eq!(c.effect, 0);

        let tool = settings_sound_tool(SoundTool::AddBand).unwrap().unwrap();
        assert_eq!(tool.sound, current().unwrap().sound(), "kept, and only the sound part answered");
        assert_eq!(tool.effect, SOUND);
        let n = tool.sound.eq_bands.len();
        let removed = settings_sound_tool(SoundTool::RemoveBand { index: n as u32 - 1 }).unwrap().unwrap();
        assert_eq!(removed.sound.eq_bands.len(), n - 1);
        assert!(matches!(settings_sound_tool(SoundTool::Import { text: "nothing here".into() }), Err(SoundError::NoFilters)));
        assert_eq!(current().unwrap().eq_bands.len(), n - 1, "a failed import changes nothing");
        assert_eq!(settings_sound_tool(SoundTool::RemoveBand { index: 999 }).unwrap(), None, "no band there: nothing changed");
    }

    #[test]
    fn the_defaults_first_then_the_database_is_the_settings() {
        let _turn = OPEN.lock();
        let dir = nori_testdir::TempDir::new("settings");
        let path = dir.join("nori.db").display().to_string();
        assert_eq!(settings_open(path.clone()).unwrap(), StoredPrefs::default());
        let mut p = current().unwrap();
        p.fade_ms = 400;
        // Written straight away here rather than through the background thread.
        write(&mut KEPT.read().as_ref().unwrap().db.lock(), &p).unwrap();
        assert_eq!(settings_open(path).unwrap().fade_ms, 400, "what was saved comes back");
    }
}
