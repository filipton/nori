//! Remote control on the iPod: Bonjour through the app (`NetService`, `ios/Sources/Devices.swift`) for
//! the account's nearby devices.

use std::ffi::{c_char, CString};
use std::sync::{Arc, Mutex};

use nori_core::remote::{Announcement, Discovery, Remote};
use nori_core::Param;
use serde_json::{Map, Value};

use crate::session::{c_text, with_session};

/// Announces this device's door: `name`, `port` and its TXT record as a JSON object of strings; `name`
/// NULL withdraws it.
pub type AnnounceFn = unsafe extern "C" fn(name: *const c_char, port: u16, txt: *const c_char);
/// Looks for other doors while `on` is 1, reporting them to [`nori_ios_lan_found`] and [`nori_ios_lan_lost`].
pub type BrowseFn = unsafe extern "C" fn(on: i32);

/// The app's Bonjour. C callbacks carry no handle, so they live in a process-wide slot.
static HOOKS: Mutex<Option<(AnnounceFn, BrowseFn)>> = Mutex::new(None);

fn hooks() -> Option<(AnnounceFn, BrowseFn)> {
    *HOOKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the remote's mDNS goes. NULL clears it.
///
/// # Safety
/// Both, when set, are functions the process keeps while they stay set.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_on_bonjour(announce: Option<AnnounceFn>, browse: Option<BrowseFn>) {
    *HOOKS.lock().unwrap_or_else(|e| e.into_inner()) = announce.zip(browse);
}

fn c(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).unwrap_or_default()
}

/// [`Discovery`] over the app's Bonjour.
pub(crate) struct Bonjour;

impl Discovery for Bonjour {
    fn announce(&self, door: Option<Announcement>) {
        let Some((announce, _)) = hooks() else { return };
        match door {
            Some(d) => {
                let txt: Map<String, Value> = d.txt.into_iter().map(|p| (p.key, Value::String(p.value))).collect();
                let (name, txt) = (c(&d.name), c(&Value::Object(txt).to_string()));
                // SAFETY: the strings live through the call; the app copies what it keeps.
                unsafe { announce(name.as_ptr(), d.port, txt.as_ptr()) };
            }
            // SAFETY: NULL is the withdrawal the hook expects.
            None => unsafe { announce(std::ptr::null(), 0, std::ptr::null()) },
        }
    }

    fn browse(&self, on: bool) {
        if let Some((_, browse)) = hooks() {
            // SAFETY: a plain call into the app.
            unsafe { browse(i32::from(on)) };
        }
    }
}

fn remote() -> Option<Arc<Remote>> {
    with_session(|s| s.remote()).flatten()
}

/// A door Bonjour resolved: service `service` at `host`:`port`, its TXT record as a JSON object of strings.
///
/// # Safety
/// The strings are NUL-terminated UTF-8, or NULL.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_lan_found(service: *const c_char, host: *const c_char, port: u16, txt: *const c_char) {
    let txt: Map<String, Value> = serde_json::from_str(&c_text(txt)).unwrap_or_default();
    let txt = txt.into_iter().filter_map(|(key, v)| v.as_str().map(|v| Param { key, value: v.into() })).collect();
    if let Some(r) = remote() {
        r.lan_found(c_text(service), c_text(host), port, txt);
    }
}

/// A door Bonjour saw go.
///
/// # Safety
/// `service` is NUL-terminated UTF-8, or NULL.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_lan_lost(service: *const c_char) {
    if let Some(r) = remote() {
        r.lan_lost(c_text(service));
    }
}
