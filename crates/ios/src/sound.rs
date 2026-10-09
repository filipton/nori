//! The Sound page's doors: each output with the sound it gets, a device's choices, and the AutoEQ
//! browser. Choices go through the core's `device_*` calls, as Android's `DeviceSound`.

use std::ffi::c_char;

use nori_core::profiles::{device_rows, output_port, sheet, ChoiceKind, OutputPort};
use nori_core::settings_store::SoundTool;
use nori_core::{AutoEqEntry, Core};
use nori_host::session::Session;
use serde_json::{json, Value};

use crate::pages::owned;
use crate::output::route_key;
use crate::session::{c_text, with_session};

/// AutoEQ curves a device's sheet offers.
const SHEET_CURVES: u32 = 5;

fn port_code(p: OutputPort) -> i32 {
    match p {
        OutputPort::Speaker => 0,
        OutputPort::Wired => 1,
        OutputPort::Usb => 2,
        OutputPort::Bluetooth => 3,
        OutputPort::Other => 4,
    }
}

fn choice_code(c: ChoiceKind) -> i32 {
    match c {
        ChoiceKind::Automatic => 0,
        ChoiceKind::Quiet => 1,
        ChoiceKind::Flat => 2,
        ChoiceKind::Profile => 3,
        ChoiceKind::Bypass => 4,
    }
}

fn choice_of(code: i32) -> Option<ChoiceKind> {
    Some(match code {
        0 => ChoiceKind::Automatic,
        1 => ChoiceKind::Quiet,
        2 => ChoiceKind::Flat,
        3 => ChoiceKind::Profile,
        4 => ChoiceKind::Bypass,
        _ => return None,
    })
}

fn entry_json(e: &AutoEqEntry) -> Value {
    json!({ "name": e.name, "source": e.source, "form": e.form, "target": e.target, "path": e.path })
}

/// The outputs seen (the speaker always), the current one and every output something was chosen for.
fn devices_json(core: &Core, current: &str) -> Value {
    let profiles = core.profiles().unwrap_or_default();
    let quiet = core.device_quiet();
    let mut known = nori_core::outputs::outputs_known(core.session.settings.clone());
    known.extend(profiles.iter().flat_map(|p| p.outputs.iter().cloned()));
    known.extend(quiet.iter().cloned());
    let rows = device_rows(known, current.to_string(), profiles, quiet);
    Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "output": r.output, "port": port_code(r.port), "name": r.name, "current": r.current,
                    "choice": choice_code(r.choice), "profile": r.profile,
                })
            })
            .collect(),
    )
}

/// What `output`'s sheet offers: saved profiles, AutoEQ curves its name points at, and whether it can
/// be taken out of the list.
fn sheet_json(core: &Core, output: &str, current: &str) -> Value {
    let names: Vec<String> = core
        .profiles()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.name)
        .collect();
    let s = sheet(output, output == current, &names);
    let curves = core.autoeq_for_output(output.to_string(), SHEET_CURVES);
    json!({
        "port": port_code(output_port(output.to_string())),
        "profiles": s.profiles,
        "forget": s.can_forget,
        "curves": curves.iter().map(entry_json).collect::<Vec<_>>(),
    })
}

fn browse_json(core: &Core, query: String) -> Value {
    let found = core.autoeq_browse(query);
    json!({
        "short": found.too_short,
        "count": core.autoeq_count().unwrap_or(0),
        "hits": found.hits.iter().map(entry_json).collect::<Vec<_>>(),
    })
}

/// Fetches `entry`'s curve. `Ok(None)`: AutoEQ has none, and the core hides the entry from now on.
fn curve(s: &Session, entry: AutoEqEntry) -> Result<Option<String>, ()> {
    nori_core::transport::block_on(s.client.autoeq_curve(entry)).map_err(|_| ())
}

/// 1 curve applied, 0 AutoEQ has none for it, -1 the request failed.
fn fetched(r: Result<Option<String>, ()>, apply: impl FnOnce(String) -> bool) -> i32 {
    match r {
        Ok(Some(text)) => i32::from(apply(text)),
        Ok(None) => 0,
        Err(()) => -1,
    }
}

/// # Safety
/// Every pointer is NUL-terminated UTF-8.
unsafe fn entry_of(
    name: *const c_char,
    source: *const c_char,
    form: *const c_char,
    target: *const c_char,
    path: *const c_char,
) -> AutoEqEntry {
    AutoEqEntry {
        name: c_text(name),
        source: c_text(source),
        form: c_text(form),
        target: c_text(target),
        path: c_text(path),
    }
}

/// The outputs and the sound each gets, as JSON to free:
/// `{current, rows: [{output, port, name, current, choice, profile}]}`.
#[no_mangle]
pub extern "C" fn nori_ios_devices() -> *mut c_char {
    let current = route_key();
    with_session(|s| owned(&json!({ "current": current, "rows": devices_json(&s.core, &current) })))
        .unwrap_or(std::ptr::null_mut())
}

/// `output`'s choices, as JSON to free: `{port, profiles, forget, curves}`.
///
/// # Safety
/// `output` is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_device_sheet(output: *const c_char) -> *mut c_char {
    let output = c_text(output);
    let current = route_key();
    with_session(|s| owned(&sheet_json(&s.core, &output, &current))).unwrap_or(std::ptr::null_mut())
}

/// Gives `output` choice `choice` (`profile` names a saved profile for choice 3); loaded at once when it
/// is the output playing. 1 when kept.
///
/// # Safety
/// Both strings are NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_device_assign(
    output: *const c_char,
    choice: i32,
    profile: *const c_char,
) -> i32 {
    let Some(choice) = choice_of(choice) else { return 0 };
    let (output, profile) = (c_text(output), c_text(profile));
    let live = output == route_key();
    with_session(|s| match s.core.device_assign(output.clone(), choice, profile, live) {
        Ok(effect) => {
            s.device_effect(&output, effect);
            1
        }
        Err(_) => 0,
    })
    .unwrap_or(0)
}

/// Fetches AutoEQ curve (name, source, form, target, path) and saves it as `output`'s own sound,
/// loaded at once when it is the output playing. Blocks on the network: call it off the main thread.
/// 1 kept, 0 AutoEQ has no curve for it, -1 the request failed.
///
/// # Safety
/// Every string is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_device_adopt(
    output: *const c_char,
    name: *const c_char,
    source: *const c_char,
    form: *const c_char,
    target: *const c_char,
    path: *const c_char,
) -> i32 {
    let output = c_text(output);
    let entry = unsafe { entry_of(name, source, form, target, path) };
    let live = output == route_key();
    with_session(|s| {
        let name = entry.name.clone();
        fetched(curve(s, entry), |text| match s.core.device_adopt(output.clone(), name, text, live) {
            Ok(effect) => {
                s.device_effect(&output, effect);
                true
            }
            Err(_) => false,
        })
    })
    .unwrap_or(0)
}

/// Takes `output` out of the list with whatever was chosen for it.
///
/// # Safety
/// `output` is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_device_forget(output: *const c_char) {
    let output = c_text(output);
    with_session(|s| {
        let effect = s.core.device_forget(output.clone());
        s.device_effect(&output, effect);
    });
}

/// The answer to the last [`crate::session::REPORT_CURVE`]: the curve offered applied, or the one applied
/// undone. Fetching runs on a thread of its own.
#[no_mangle]
pub extern "C" fn nori_ios_curve_answer() {
    with_session(Session::curve_answer);
}

/// The AutoEQ list's hits for `query`, as JSON to free: `{short, count, hits}`.
///
/// # Safety
/// `query` is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_autoeq_browse(query: *const c_char) -> *mut c_char {
    let query = c_text(query);
    with_session(|s| owned(&browse_json(&s.core, query))).unwrap_or(std::ptr::null_mut())
}

/// Downloads the AutoEQ list now. Blocks on the network: call it off the main thread. The headphones
/// kept, or -1 when the request failed.
#[no_mangle]
pub extern "C" fn nori_ios_autoeq_update() -> i32 {
    with_session(|s| {
        match nori_core::transport::block_on(s.client.autoeq_update(true, false)) {
            // None: a download already running; the count is what is kept so far.
            Ok(n) => n.or_else(|| s.core.autoeq_count().ok()).map_or(0, |n| n as i32),
            Err(_) => -1,
        }
    })
    .unwrap_or(-1)
}

/// Fetches AutoEQ curve (name, source, form, target, path) and makes it the current sound, as
/// Android's browser does. Blocks on the network. 1 applied, 0 AutoEQ has none, -1 the request failed.
///
/// # Safety
/// Every string is NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_autoeq_apply(
    name: *const c_char,
    source: *const c_char,
    form: *const c_char,
    target: *const c_char,
    path: *const c_char,
) -> i32 {
    let entry = unsafe { entry_of(name, source, form, target, path) };
    with_session(|s| {
        fetched(curve(s, entry), |text| {
            match s.core.session.settings.sound_tool(SoundTool::Import { text }) {
                Ok(change) => {
                    if let Some(change) = change {
                        s.applied(change.effect);
                    }
                    true
                }
                Err(_) => false,
            }
        })
    })
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_core::SoundProfile;

    const BUDS: &str = "Bluetooth: Buds";
    const WIRED: &str = "Wired headphones";
    const SPEAKER: &str = "Phone speaker";

    fn core() -> std::sync::Arc<Core> {
        Core::new(String::new(), "t".into(), Default::default()).unwrap()
    }

    #[test]
    fn the_list_holds_the_speaker_the_current_output_and_every_bound_one() {
        let core = core();
        core.profile_save(SoundProfile {
            name: "Warm".into(),
            json: "{}".into(),
            outputs: vec![BUDS.into()],
        })
        .unwrap();
        let rows = devices_json(&core, WIRED);
        let outputs: Vec<&str> = rows.as_array().unwrap().iter().map(|r| r["output"].as_str().unwrap()).collect();
        assert_eq!(outputs.len(), 3, "{rows}");
        for o in [SPEAKER, WIRED, BUDS] {
            assert!(outputs.contains(&o), "{o} missing from {rows}");
        }
        let buds = rows.as_array().unwrap().iter().find(|r| r["output"] == BUDS).unwrap();
        assert_eq!((buds["port"].as_i64(), buds["name"].as_str()), (Some(3), Some("Buds")));
        assert_eq!((buds["choice"].as_i64(), buds["profile"].as_str()), (Some(3), Some("Warm")));
        let wired = rows.as_array().unwrap().iter().find(|r| r["output"] == WIRED).unwrap();
        assert_eq!((wired["current"].as_bool(), wired["choice"].as_i64()), (Some(true), Some(0)));
    }

    #[test]
    fn only_a_device_not_playing_and_not_the_speaker_can_be_forgotten() {
        let core = core();
        assert_eq!(sheet_json(&core, BUDS, WIRED)["forget"], true);
        assert_eq!(sheet_json(&core, BUDS, BUDS)["forget"], false);
        assert_eq!(sheet_json(&core, SPEAKER, WIRED)["forget"], false);
    }

    #[test]
    fn a_one_letter_query_is_too_short_to_search() {
        let core = core();
        assert_eq!(browse_json(&core, "a".into())["short"], true);
        assert_eq!(browse_json(&core, "hd".into())["short"], false);
    }

    #[test]
    fn choice_codes_round_trip_and_unknown_codes_are_refused() {
        for c in [ChoiceKind::Automatic, ChoiceKind::Quiet, ChoiceKind::Flat, ChoiceKind::Profile, ChoiceKind::Bypass] {
            assert_eq!(choice_of(choice_code(c)), Some(c));
        }
        assert_eq!(choice_of(9), None);
    }

    #[test]
    fn a_curve_answer_maps_to_kept_missing_or_failed() {
        assert_eq!(fetched(Ok(Some("x".into())), |_| true), 1);
        assert_eq!(fetched(Ok(Some("x".into())), |_| false), 0);
        assert_eq!(fetched(Ok(None), |_| unreachable!()), 0);
        assert_eq!(fetched(Err(()), |_| unreachable!()), -1);
    }
}
