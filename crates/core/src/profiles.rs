//! Per-device sound profile calls over the core's database: arrivals, AutoEQ curves, undo and device
//! list choices. The decisions are nori-devices'.

use nori_player::device::{self, BYPASS, FLAT};
use nori_player::outputs::SPEAKER;
use rusqlite::OptionalExtension;

use crate::settings::{self, sound_from, sound_json, SoundError, SoundSettings};
use crate::{alog, autoeq, Arrival, AutoEqEntry, Core, CoreError, CurveStep, SoundProfile};

pub use nori_devices::profiles::*;

use std::sync::atomic::{AtomicBool, Ordering};

use crate::client::{Client, NetResult};

/// An AutoEQ index fetch is running; concurrent triggers are dropped.
// Global: the index is app-wide, shared by every client.
static INDEX_FETCHING: AtomicBool = AtomicBool::new(false);

/// Clears [INDEX_FETCHING] on drop, including a dropped future.
struct Fetching;

impl Drop for Fetching {
    fn drop(&mut self) {
        INDEX_FETCHING.store(false, Ordering::Release);
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Refreshes the AutoEQ index when due (`autoeq::index_due`) or when `asked` by the user (any network).
    /// Returns the headphone count fetched; None when not due or already fetching.
    pub async fn autoeq_update(&self, asked: bool, metered: bool) -> NetResult<Option<u32>> {
        let now = crate::db::now_ms();
        if !asked {
            let auto = crate::settings_store::with_prefs(|p| p.auto_eq_download && p.third_party_lookups).unwrap_or(false);
            let (stored, fetched) = {
                let c = self.core.db.lock();
                (autoeq::count(&c)?, autoeq::fetched_ms(&c)?)
            };
            if !autoeq::index_due(auto, metered, stored, fetched, now) {
                return Ok(None);
            }
        }
        if INDEX_FETCHING.swap(true, Ordering::AcqRel) {
            return Ok(None);
        }
        let _fetching = Fetching;
        let markdown = autoeq::fetch_text(&*self.transport, autoeq::INDEX_URL.to_string()).await?;
        let n = autoeq::store(&mut self.core.db.lock(), &markdown, now)?;
        alog::info(&format!("autoeq: index kept, {n} headphones"));
        Ok(Some(n))
    }

    /// `entry`'s preset text (parametric, else graphic). None when AutoEQ has neither, which hides the
    /// entry from now on; a failed request is an error.
    pub async fn autoeq_curve(&self, entry: AutoEqEntry) -> NetResult<Option<String>> {
        let graphic = crate::settings_store::current().is_some_and(|p| p.eq_mode == crate::settings::EqMode::Graphic);
        match autoeq::fetch_curve(&*self.transport, &entry, graphic).await? {
            autoeq::Curve::Found(text) => Ok(Some(text)),
            autoeq::Curve::Missing => {
                alog::info(&format!("autoeq: no curve for {} ({})", entry.name, entry.path));
                autoeq::mark_missing(&self.core.db.lock(), &entry.path)?;
                Ok(None)
            }
        }
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Music now plays to `output`.
    pub fn device_arrive(&self, output: String) -> DeviceArrival {
        self.arrive_as(output, &Now::read())
    }

    /// Applies AutoEQ curve `name` (`preset` text) over the current sound, saves it as profile `name`
    /// bound to `output` and, if `live`, loads it. Errors when the preset has no filters.
    pub fn device_adopt(&self, output: String, name: String, preset: String, live: bool) -> Result<DeviceEffect, SoundError> {
        let step = self.adopt_as(&output, &name, &preset, live, &Now::read())?;
        Ok(self.settle(&output, step))
    }

    /// Undoes an auto-applied curve: unbinds, deletes the profile if `created` and unused, restores
    /// `before`, and stops offering this device curves.
    pub fn device_undo(&self, output: String, curve: String, created: bool, before: SoundSettings) -> DeviceEffect {
        let unbind = || -> Result<(), CoreError> {
            self.profile_bind(output.clone(), None)?;
            if created && self.profiles()?.iter().any(|p| p.name == curve && p.outputs.is_empty()) {
                self.profile_delete(curve.clone())?;
            }
            Ok(())
        };
        let _ = unbind();
        let effect = DeviceEffect { refresh: true, apply: Some(before), ..DeviceEffect::none() };
        self.settle(&output, Step { quiet: Some(true), loose: LooseChange::Clear, effect })
    }

    /// A device list choice (curves go through `device_adopt`); `live` loads it now.
    pub fn device_assign(&self, output: String, choice: ChoiceKind, profile: String, live: bool) -> Result<DeviceEffect, SoundError> {
        let step = self.assign_as(&output, choice, profile, live, &Now::read())?;
        Ok(self.settle(&output, step))
    }

    /// Forgets a device's binding and its "never ask" mark.
    pub fn device_forget(&self, output: String) -> DeviceEffect {
        let _ = self.profile_bind(output.clone(), None);
        let effect = DeviceEffect { refresh: true, ..DeviceEffect::none() };
        self.settle(&output, Step { quiet: Some(false), loose: LooseChange::Keep, effect })
    }

    /// Devices never offered a curve.
    pub fn device_quiet(&self) -> Vec<String> {
        self.quiet_list()
    }

    /// Saves `sound` as profile `name` (trimmed), keeping an existing profile's devices.
    pub fn profile_save_sound(&self, name: String, sound: SoundSettings) -> Result<(), SoundError> {
        let name = name.trim().to_string();
        let kept = self.profiles()?.into_iter().find(|p| p.name == name).map(|p| p.outputs).unwrap_or_default();
        self.profile_save(SoundProfile { name, json: sound_json(&sound), outputs: kept })?;
        Ok(())
    }

    /// AutoEQ curves matching the output's device name, best first; empty for speaker or placeholder names.
    pub fn autoeq_for_output(&self, output: String, limit: u32) -> Vec<AutoEqEntry> {
        let Some(name) = headphones_name(&output) else { return Vec::new() };
        self.autoeq_for_device(name.to_string(), limit).unwrap_or_default()
    }

    /// The AutoEQ browser search.
    pub fn autoeq_browse(&self, query: String) -> AutoEqFound {
        let too_short = autoeq_too_short(&query);
        AutoEqFound { too_short, hits: self.autoeq_find(query) }
    }
}

impl Core {
    /// Up to 40 AutoEQ hits; none for queries under two characters.
    pub fn autoeq_find(&self, query: String) -> Vec<AutoEqEntry> {
        if autoeq_too_short(&query) {
            return Vec::new();
        }
        self.autoeq_search(query, 40).unwrap_or_default()
    }

    fn arrive_as(&self, output: String, now: &Now) -> DeviceArrival {
        let bound = self.profile_for_output(output.clone()).ok().flatten();
        alog::info(&format!("device sound: {output} -> {}", bound.as_ref().map_or("nothing chosen", |p| p.name.as_str())));
        let loose = self.loose();
        let quiet = self.quiet_list().contains(&output);
        let plan = device::on_arrival(Arrival { bound: bound.is_some(), per_output: now.per_output, speaker: output == SPEAKER, quiet, auto_apply: now.auto_apply });
        let mut step = Step::none();
        if plan.load_bound {
            if let Some(sound) = bound.and_then(|b| sound_from(&b.json)) {
                (step.effect.apply, step.loose) = loaded(sound, &now.sound, now.per_output, loose.is_some());
            }
        }
        if plan.restore {
            if let Some(json) = loose {
                step.loose = LooseChange::Clear;
                step.effect.apply = sound_from(&json);
            }
        }
        let entry = if plan.curve == CurveStep::None { None } else { self.autoeq_for_output(output.clone(), 5).into_iter().next() };
        let preset_url = entry.as_ref().map(autoeq::preset_url);
        DeviceArrival { effect: self.settle(&output, step), curve: plan.curve, entry, preset_url }
    }

    fn adopt_as(&self, output: &str, name: &str, preset: &str, live: bool, now: &Now) -> Result<Step, SoundError> {
        let sound = settings::import(now.sound.clone(), preset)?;
        let created = self.save_bound(name, &sound, output)?;
        let (apply, loose) = if live { loaded(sound, &now.sound, now.per_output, self.loose().is_some()) } else { (None, LooseChange::Keep) };
        if live {
            alog::info(&format!("device sound: applied AutoEQ {name} to {output}"));
        }
        Ok(Step { quiet: Some(false), loose, effect: DeviceEffect { refresh: true, apply, arrive: false, created } })
    }

    fn assign_as(&self, output: &str, choice: ChoiceKind, profile: String, live: bool, now: &Now) -> Result<Step, SoundError> {
        let name = match choice {
            ChoiceKind::Automatic | ChoiceKind::Quiet => {
                let _ = self.profile_bind(output.to_string(), None);
                let effect = DeviceEffect { refresh: true, arrive: live, ..DeviceEffect::none() };
                return Ok(Step { quiet: Some(choice == ChoiceKind::Quiet), loose: LooseChange::Keep, effect });
            }
            ChoiceKind::Flat => {
                // Created on first use: the current sound with the equalizer off.
                if !self.profiles()?.iter().any(|p| p.name == FLAT) {
                    let flat = SoundSettings { eq_enabled: false, ..now.sound.clone() };
                    self.profile_save(SoundProfile { name: FLAT.to_string(), json: sound_json(&flat), outputs: Vec::new() })?;
                }
                FLAT.to_string()
            }
            ChoiceKind::Bypass => {
                // Created on first use: the current sound with the chain bypassed.
                if !self.profiles()?.iter().any(|p| p.name == BYPASS) {
                    let none = SoundSettings { bypass: true, ..now.sound.clone() };
                    self.profile_save(SoundProfile { name: BYPASS.to_string(), json: sound_json(&none), outputs: Vec::new() })?;
                }
                BYPASS.to_string()
            }
            ChoiceKind::Profile => profile,
        };
        self.profile_bind(output.to_string(), Some(name.clone()))?;
        let mut step = Step { quiet: Some(false), loose: LooseChange::Keep, effect: DeviceEffect { refresh: true, ..DeviceEffect::none() } };
        if live {
            if let Some(sound) = self.profiles()?.into_iter().find(|p| p.name == name).and_then(|p| sound_from(&p.json)) {
                (step.effect.apply, step.loose) = loaded(sound, &now.sound, now.per_output, self.loose().is_some());
            }
        }
        Ok(step)
    }

    /// Stores the step's quiet mark and saved sound; returns the platform's effect.
    fn settle(&self, output: &str, step: Step) -> DeviceEffect {
        if let Some(on) = step.quiet {
            self.set_quiet(output, on);
        }
        self.set_loose(step.loose);
        step.effect
    }

    fn kv(&self, key: &str) -> Option<String> {
        let c = self.db.lock();
        c.query_row("SELECT value FROM app_kv WHERE key=?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    fn set_kv(&self, key: &str, value: Option<&str>) {
        let c = self.db.lock();
        let done = match value {
            Some(v) => c.execute("INSERT OR REPLACE INTO app_kv(key, value) VALUES(?1, ?2)", [key, v]),
            None => c.execute("DELETE FROM app_kv WHERE key=?1", [key]),
        };
        if let Err(e) = done {
            alog::info(&format!("device sound: could not keep {key}: {e}"));
        }
    }

    fn loose(&self) -> Option<String> {
        self.kv(LOOSE)
    }

    fn set_loose(&self, change: LooseChange) {
        match change {
            LooseChange::Keep => {}
            LooseChange::Store { json } => self.set_kv(LOOSE, Some(&json)),
            LooseChange::Clear => self.set_kv(LOOSE, None),
        }
    }

    fn quiet_list(&self) -> Vec<String> {
        self.kv(QUIET).and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
    }

    fn set_quiet(&self, output: &str, on: bool) {
        let mut list = self.quiet_list();
        let had = list.iter().any(|o| o == output);
        if had == on {
            return;
        }
        if on {
            list.push(output.to_string());
        } else {
            list.retain(|o| o != output);
        }
        self.set_kv(QUIET, Some(&serde_json::to_string(&list).unwrap_or_default()));
    }

    /// Saves profile `name` (keeping its devices) and binds `output` to it; true if new.
    fn save_bound(&self, name: &str, sound: &SoundSettings, output: &str) -> Result<bool, CoreError> {
        let old = self.profiles()?.into_iter().find(|p| p.name == name);
        let created = old.is_none();
        self.profile_save(SoundProfile { name: name.to_string(), json: sound_json(sound), outputs: old.map(|p| p.outputs).unwrap_or_default() })?;
        self.profile_bind(output.to_string(), Some(name.to_string()))?;
        Ok(created)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn sound() -> SoundSettings {
        sound_from("{}").unwrap()
    }

    fn now(sound: SoundSettings) -> Now {
        Now { sound, per_output: true, auto_apply: false }
    }

    const PRESET: &str = "Preamp: -6.2 dB\nFilter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70\n";

    fn core() -> std::sync::Arc<Core> {
        Core::new(String::new(), "t".into()).unwrap()
    }

    #[test]
    fn bound_device_loads_sound_and_saves_previous() {
        let c = core();
        let warm = SoundSettings { crossfeed_db: 3.0, ..sound() };
        c.profile_save(SoundProfile { name: "Warm".into(), json: sound_json(&warm), outputs: vec!["USB: K3".into()] }).unwrap();
        let playing = SoundSettings { balance: 0.5, ..sound() };
        let a = c.arrive_as("USB: K3".into(), &now(playing.clone()));
        assert_eq!(a.effect.apply, Some(warm.clone()));
        assert_eq!(c.loose(), Some(sound_json(&playing)));
        assert_eq!(a.curve, CurveStep::None);
        // An already saved sound is not overwritten.
        c.arrive_as("USB: K3".into(), &now(sound()));
        assert_eq!(c.loose(), Some(sound_json(&playing)));
        let off = Now { per_output: false, ..now(playing) };
        assert_eq!(c.arrive_as("USB: K3".into(), &off).effect.apply, None);
    }

    #[test]
    fn quiet_device_is_not_offered_a_curve() {
        let c = core();
        c.set_quiet("Bluetooth: Buds", true);
        c.set_quiet("Bluetooth: Buds", true);
        assert_eq!(c.device_quiet(), ["Bluetooth: Buds"]);
        assert_eq!(c.arrive_as("Bluetooth: Buds".into(), &now(sound())).curve, CurveStep::None);
        c.device_forget("Bluetooth: Buds".into());
        assert!(c.device_quiet().is_empty());
        assert_eq!(c.arrive_as("Bluetooth: Buds".into(), &now(sound())).curve, CurveStep::Offer);
    }

    #[test]
    fn adopt_saves_binds_and_loads_curve() {
        let c = core();
        c.set_quiet("Bluetooth: X", true);
        let playing = SoundSettings { balance: 0.5, ..sound() };
        let step = c.adopt_as("Bluetooth: X", "Sony", PRESET, true, &now(playing.clone())).unwrap();
        let e = c.settle("Bluetooth: X", step);
        assert!(e.created);
        assert!(e.refresh);
        assert!(c.device_quiet().is_empty());
        let applied = e.apply.unwrap();
        assert!(applied.eq_enabled);
        assert_eq!(applied.eq_preamp_db, Some(-6.2));
        assert_eq!(applied.balance, 0.5, "over the current sound");
        assert_eq!(c.loose(), Some(sound_json(&playing)));
        assert_eq!(c.profile_for_output("Bluetooth: X".into()).unwrap().unwrap().name, "Sony");
        // The same curve for another device reuses the profile.
        let again = c.adopt_as("USB: Y", "Sony", PRESET, false, &now(playing)).unwrap();
        assert!(!again.effect.created);
        assert_eq!(again.effect.apply, None);
        let p = c.profiles().unwrap().into_iter().find(|p| p.name == "Sony").unwrap();
        assert_eq!(p.outputs, ["Bluetooth: X", "USB: Y"]);
        let err = c.adopt_as("x", "Empty", "nothing", true, &now(sound())).unwrap_err();
        assert!(matches!(err, SoundError::NoFilters));
    }

    #[test]
    fn unbound_device_restores_saved_sound() {
        let c = core();
        let kept = SoundSettings { mono: true, ..sound() };
        c.set_loose(LooseChange::Store { json: sound_json(&kept) });
        let a = c.arrive_as(SPEAKER.into(), &now(sound()));
        assert_eq!(a.effect.apply, Some(kept));
        assert_eq!(c.loose(), None);
        assert_eq!(a.curve, CurveStep::None);
        c.set_loose(LooseChange::Store { json: "garbage".into() });
        let a = c.arrive_as(SPEAKER.into(), &now(sound()));
        assert_eq!((a.effect.apply, c.loose()), (None, None), "unreadable saved sound is dropped");
        let a = c.arrive_as("Bluetooth: Buds".into(), &now(sound()));
        assert_eq!(a.curve, CurveStep::Offer);
        assert_eq!(a.entry, None, "no index");
        assert_eq!(a.effect, DeviceEffect::none());
    }

    #[test]
    fn device_list_choices() {
        let c = core();
        c.set_loose(LooseChange::Store { json: "{}".into() });
        let playing = SoundSettings { eq_enabled: true, crossfeed_db: 2.0, ..sound() };
        let s = c.assign_as("USB: K3", ChoiceKind::Flat, String::new(), true, &now(playing.clone())).unwrap();
        let flat = s.effect.apply.clone().unwrap();
        assert!(!flat.eq_enabled);
        assert_eq!(flat.crossfeed_db, 2.0);
        assert_eq!(s.loose, LooseChange::Keep, "a sound was already saved");
        assert_eq!(s.quiet, Some(false));
        assert_eq!(c.profile_for_output("USB: K3".into()).unwrap().unwrap().name, FLAT);
        // "Flat" is created once.
        c.assign_as("Wired headphones", ChoiceKind::Flat, String::new(), false, &now(sound())).unwrap();
        assert_eq!(sound_from(&c.profiles().unwrap()[0].json).unwrap().crossfeed_db, 2.0);

        let q = c.assign_as("USB: K3", ChoiceKind::Quiet, String::new(), true, &now(sound())).unwrap();
        assert_eq!((q.quiet, q.effect.refresh, q.effect.arrive, q.effect.apply), (Some(true), true, true, None));
        assert!(c.profile_for_output("USB: K3".into()).unwrap().is_none());
        let a = c.assign_as("USB: K3", ChoiceKind::Automatic, String::new(), false, &now(sound())).unwrap();
        assert_eq!((a.quiet, a.effect.arrive), (Some(false), false));

        c.profile_save_sound(" Warm ".into(), playing.clone()).unwrap();
        let p = c.assign_as("USB: K3", ChoiceKind::Profile, "Warm".into(), false, &now(sound())).unwrap();
        assert_eq!(p.effect.apply, None, "not live");
        assert_eq!(c.profile_for_output("USB: K3".into()).unwrap().unwrap().name, "Warm");
        c.profile_save_sound("Warm".into(), sound()).unwrap();
        assert_eq!(c.profile_for_output("USB: K3".into()).unwrap().unwrap().json, sound_json(&sound()));
        c.device_forget("USB: K3".into());
        assert!(c.profile_for_output("USB: K3".into()).unwrap().is_none());

        // "No processing": like "Flat" but with the chain bypassed.
        let b = c.assign_as("USB: DAC", ChoiceKind::Bypass, String::new(), true, &now(playing.clone())).unwrap();
        let none = b.effect.apply.unwrap();
        assert!(none.bypass && none.eq_enabled && none.crossfeed_db == 2.0);
        assert!(!crate::settings::StoredPrefs::default().with_sound(none).sound_chain_on());
        assert_eq!(c.profile_for_output("USB: DAC".into()).unwrap().unwrap().name, BYPASS);
        let rows = device_rows(vec!["USB: DAC".into()], SPEAKER.into(), c.profiles().unwrap(), Vec::new());
        assert_eq!(rows.iter().find(|r| r.output == "USB: DAC").unwrap().choice, ChoiceKind::Bypass);
    }

    #[test]
    fn curves_are_matched_by_device_name_only() {
        // Stored directly: fetches are one per process and another test fetches.
        let c = core();
        let list = "- [Sony WH-1000XM6](./Super%20Review/over-ear/Sony%20WH-1000XM6) by Super Review\n";
        assert_eq!(autoeq::store(&mut c.db.lock(), list, crate::db::now_ms()).unwrap(), 1);
        assert_eq!(c.autoeq_for_output("Bluetooth: WH-1000XM6".into(), 5).len(), 1);
        assert!(c.autoeq_for_output(SPEAKER.into(), 5).is_empty());
        assert!(c.autoeq_for_output("USB: ".into(), 5).is_empty());
        assert_eq!(headphones_name("Bluetooth: LE_WH-1000XM5"), Some("LE_WH-1000XM5"));
        // Placeholder names are not models.
        for nameless in ["USB: DAC", "Bluetooth: device", "Wired headphones", "Other output", "HDMI TV"] {
            assert_eq!(headphones_name(nameless), None, "{nameless}");
        }
        assert!(c.autoeq_find("a".into()).is_empty());
    }

    #[test]
    fn device_sheet_and_curve_search() {
        let names = vec![FLAT.to_string(), "Warm".to_string()];
        let s = sheet("USB: K3", false, &names);
        assert_eq!(s.profiles, ["Warm"]);
        assert!(s.can_forget);
        assert!(!sheet("x", true, &[]).can_forget, "playing");
        assert!(!sheet(SPEAKER, false, &[]).can_forget);
        assert!(autoeq_too_short("a") && autoeq_too_short("") && !autoeq_too_short("hd"));
        let c = core();
        assert!(c.autoeq_browse("h".into()).too_short);
        assert!(!c.autoeq_browse("hd".into()).too_short);
    }

    #[test]
    fn undo_restores_everything() {
        let c = core();
        let step = c.adopt_as("Bluetooth: X", "Sony", PRESET, true, &now(sound())).unwrap();
        c.settle("Bluetooth: X", step);
        let before = SoundSettings { mono: true, ..sound() };
        let e = c.device_undo("Bluetooth: X".into(), "Sony".into(), true, before.clone());
        assert_eq!(e, DeviceEffect { refresh: true, apply: Some(before), arrive: false, created: false });
        assert!(c.profiles().unwrap().is_empty());
        assert_eq!(c.device_quiet(), ["Bluetooth: X"]);
        assert_eq!(c.loose(), None);
        // A pre-existing profile stays.
        let step = c.adopt_as("Bluetooth: X", "Sony", PRESET, true, &now(sound())).unwrap();
        c.settle("Bluetooth: X", step);
        c.device_undo("Bluetooth: X".into(), "Sony".into(), false, sound());
        assert_eq!(c.profiles().unwrap().len(), 1);
    }

    #[test]
    fn autoeq_update_and_missing_curves() {
        use crate::client::tests::{block, client};
        let (c, fake) = client(crate::client::NetProfile { url: "h".into(), ..Default::default() });
        let index = "- [Sony WH-1000XM6](./Super%20Review/over-ear/Sony%20WH-1000XM6) by Super Review\n\
- [Sony WH-1000XM6 (analog cable)](./Super%20Review/over-ear/Sony%20WH-1000XM6%20(analog%20cable)) by Super Review\n";
        assert_eq!(block(c.autoeq_update(false, true)).unwrap(), None, "not automatically on metered");
        assert!(fake.asked().is_empty());
        fake.answer(index);
        assert_eq!(block(c.autoeq_update(true, true)).unwrap(), Some(2), "asked: any network");
        assert_eq!(fake.asked(), [autoeq::INDEX_URL]);

        let cable = c.core.autoeq_search("analog".into(), 5).unwrap().remove(0);
        fake.answers.lock().push_back(Ok((404, b"404: Not Found".to_vec())));
        fake.answers.lock().push_back(Ok((404, b"404: Not Found".to_vec())));
        assert_eq!(block(c.autoeq_curve(cable.clone())).unwrap(), None);
        assert!(c.core.autoeq_search("analog".into(), 5).unwrap().is_empty(), "hidden");
        assert_eq!(c.core.autoeq_count().unwrap(), 1);

        let plain = c.core.autoeq_search("WH-1000XM6".into(), 5).unwrap().remove(0);
        fake.fail(crate::transport::FailureKind::Timeout);
        assert!(block(c.autoeq_curve(plain.clone())).is_err());
        assert_eq!(c.core.autoeq_count().unwrap(), 1);
        fake.answer("Preamp: -4.6 dB\nFilter 1: ON LSC Fc 105 Hz Gain -8.3 dB Q 0.70\n");
        assert!(block(c.autoeq_curve(plain)).unwrap().unwrap().starts_with("Preamp: -4.6 dB"));
    }
}
