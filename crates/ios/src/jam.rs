//! A jam this iPod is a guest in: joining with the host's invite link (a guest profile of its own, the
//! user's kept to go back to), what the profile offers, the jam as the queue sheet shows it, listening
//! along, and leaving. The core keeps the jam; this only hands it over.

use std::ffi::c_char;

use nori_core::browse::ProfileRules;
use nori_core::remote::{Controls, JamControls, JamView, Listening, Reach};
use serde_json::{json, Value};

use crate::pages::owned;
use crate::session::{c_text, report, with_session, DEVICE_NAME, REPORT_JAM_JOINED, REPORT_JAM_LEFT};

/// [`nori_ios_jam_join`]: joining started; its answer is a `REPORT_JAM_JOINED`.
pub const JOIN_STARTED: i32 = 0;
/// The link is no jam invite.
pub const JOIN_NOT_AN_INVITE: i32 = 1;
/// No session is open.
pub const JOIN_CLOSED: i32 = 2;
/// The invite is to the jam this iPod hosts.
pub const JOIN_OWN: i32 = 3;

/// `r` for the app: `{"asks", "account", "sections": [LibrarySection as numbers, in order], "settings":
/// [SettingsPart as numbers]}`.
fn rules_json(r: &ProfileRules) -> Value {
    let sections: Vec<u8> = r.sections.iter().map(|s| *s as u8).collect();
    let settings: Vec<u8> = r.settings.iter().map(|s| *s as u8).collect();
    json!({ "asks": r.asks, "account": r.account, "sections": sections, "settings": settings })
}

/// What the open profile offers (a jam guest's: the host's library, its picks asked of the host), as JSON
/// to free ([`rules_json`]); NULL with no session.
#[no_mangle]
pub extern "C" fn nori_ios_rules() -> *mut c_char {
    with_session(|s| owned(&rules_json(&s.rules))).unwrap_or(std::ptr::null_mut())
}

fn listening_code(l: Listening) -> u8 {
    match l {
        Listening::Watching => 0,
        Listening::Playing => 1,
        Listening::HostOff => 2,
        Listening::ServerOff => 3,
    }
}

fn reach_code(r: Reach) -> u8 {
    match r {
        Reach::Nowhere => 0,
        Reach::Here => 1,
        Reach::Jam => 2,
    }
}

/// A guest's jam `v` for the app: `{"host", "listeners": [names], "asked": [song ids], "asks": [{"t", "s",
/// "c"}], "listening": 0 only shown, 1 playing here, 2 asked but the host lets no one, 3 asked but the
/// server lets no guest, "play", "skip", "seek": what each control reaches by its role (0 offered not,
/// 1 this iPod's own listening, 2 the host's playback), "playing": what the play button shows,
/// "pausedHere": paused here while the jam plays on}`. Its asks and asked songs are its own requests the
/// host has yet to take.
fn jam_json(v: &JamView, controls: Option<JamControls>) -> Value {
    let listeners: Vec<&str> = v.listeners().map(|m| m.name.as_str()).collect();
    let asks: Vec<Value> = v.asks().map(|p| json!({ "t": p.song.title, "s": p.song.artist, "c": p.song.cover_art.as_deref().unwrap_or("") })).collect();
    let asked: Vec<&str> = v.asks().map(|p| p.song.id.as_str()).collect();
    let c = controls.map(|c| c.controls);
    let code = |f: fn(&Controls) -> Reach| c.as_ref().map_or(0, |c| reach_code(f(c)));
    json!({
        "host": v.host(), "listeners": listeners, "asked": asked, "asks": asks, "listening": listening_code(v.listening),
        "play": code(|c| c.play_pause), "skip": code(|c| c.skip), "seek": code(|c| c.seek),
        "playing": controls.is_some_and(|c| c.playing), "pausedHere": controls.is_some_and(|c| c.paused_here),
    })
}

/// The jam this iPod is a guest in, as JSON to free ([`jam_json`]); NULL in none.
#[no_mangle]
pub extern "C" fn nori_ios_jam() -> *mut c_char {
    with_session(|s| s.remote().filter(|_| s.guest).and_then(|r| r.jam_view().map(|v| owned(&jam_json(&v, r.jam_controls())))))
        .flatten()
        .unwrap_or(std::ptr::null_mut())
}

/// Joins the jam invite `link` is to, its guest profile named `name`, on a thread of its own: a
/// [`JOIN_STARTED`] answer comes as a `REPORT_JAM_JOINED`, after which the app opens the active profile.
///
/// # Safety
/// Both are NUL-terminated UTF-8, or NULL.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_jam_join(link: *const c_char, name: *const c_char) -> i32 {
    let (link, name) = (c_text(link).trim().to_string(), c_text(name));
    if !nori_core::remote::is_invite(&link) {
        return JOIN_NOT_AN_INVITE;
    }
    let Some((app, remote)) = with_session(|s| (s.core.session.clone(), s.remote())) else { return JOIN_CLOSED };
    if remote.as_ref().is_some_and(|r| r.hosts_invite(link.clone())) {
        return JOIN_OWN;
    }
    nori_host::spawn("nori-ios-jam-join", move || match nori_host::jam_join(nori_http::Http::new(), &app.settings, link, DEVICE_NAME, remote.as_deref()) {
        Ok(pass) => {
            nori_host::jam_joined(&app.settings, pass, &name);
            report(REPORT_JAM_JOINED, 1, 0, "");
        }
        // Answered above, before joining started.
        Err(nori_host::JoinError::Own) => {}
        Err(nori_host::JoinError::Failed(e)) => {
            let (code, detail) = crate::account::fail(e);
            report(REPORT_JAM_JOINED, 0, code, &detail.unwrap_or_default());
        }
    });
    JOIN_STARTED
}

/// Leaves the jam this guest is in, at once (the relay is told on the way); a `REPORT_JAM_LEFT`.
#[no_mangle]
pub extern "C" fn nori_ios_jam_leave() {
    let Some(remote) = with_session(|s| s.remote()) else { return };
    if let Some(r) = remote {
        r.jam_leave();
    }
    left(None);
}

/// Drops the guest profile, and reports `REPORT_JAM_LEFT`: flag 1 when there is a profile to open, index
/// 1 when the jam ended (`ended`) rather than was left, text then its host's name if seen.
pub(crate) fn left(ended: Option<Option<String>>) {
    let Some(app) = with_session(|s| s.core.session.clone()) else { return };
    let back = nori_host::jam_left(&app.settings);
    let host = ended.clone().flatten().unwrap_or_default();
    report(REPORT_JAM_LEFT, i32::from(back.is_some()), i32::from(ended.is_some()), &host);
}

/// Listens along (`on` 1: the host's music plays here, in step), or only shows the jam.
#[no_mangle]
pub extern "C" fn nori_ios_jam_listen(on: i32) {
    if let Some(r) = with_session(|s| s.remote()).flatten() {
        r.listen(on != 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nori_core::remote::wire::{Entry, JamMember, Pending, Role};

    #[test]
    fn a_guest_has_the_hosts_library_and_no_account() {
        let guest = rules_json(&nori_core::browse::profile_rules(true));
        assert_eq!(guest, json!({ "asks": true, "account": false, "sections": [0, 2, 3, 7], "settings": [0, 2, 3, 4, 5, 7, 9, 10] }));
        let account = rules_json(&nori_core::browse::profile_rules(false));
        assert_eq!((account["asks"].clone(), account["account"].clone(), account["sections"].as_array().map(Vec::len)), (json!(false), json!(true), Some(12)));
    }

    #[test]
    fn a_guest_sees_its_host_its_listeners_and_its_own_requests() {
        let member = |id: &str, name: &str, role| JamMember { id: id.into(), name: name.into(), role };
        let ask = |request, from: &str, id: &str| Pending { request, from: from.into(), from_name: from.into(), song: Entry { id: id.into(), title: id.to_uppercase(), artist: "Band".into(), cover_art: Some(format!("al-{id}")), ..Default::default() }, provider: false };
        let v = JamView {
            hosting: false,
            link: None,
            you: "pod".into(),
            members: vec![member("desk", "Desk", Role::Host), member("pod", "iPod", Role::Guest), member("dee", "Dee", Role::Admin)],
            pending: vec![ask(1, "pod", "x"), ask(2, "dee", "y")],
            queue: None,
            age_ms: 0,
            refused: None,
            along: false,
            listening: Listening::HostOff,
        };
        // Paused here, a plain guest's play and pause are its own; it skips and seeks nothing.
        let controls = JamControls { controls: Controls::of(Role::Guest, true), playing: false, paused_here: true };
        assert_eq!(
            jam_json(&v, Some(controls)),
            json!({
                "host": "Desk", "listeners": ["iPod", "Dee"], "asked": ["x"], "asks": [{ "t": "X", "s": "Band", "c": "al-x" }], "listening": 2,
                "play": 1, "skip": 0, "seek": 0, "playing": false, "pausedHere": true,
            })
        );
        let admin = JamControls { controls: Controls::of(Role::Admin, false), playing: true, paused_here: false };
        let j = jam_json(&v, Some(admin));
        assert_eq!((&j["play"], &j["skip"], &j["seek"], &j["playing"]), (&json!(2), &json!(2), &json!(2), &json!(true)), "an admin's reach the host");
    }
}
