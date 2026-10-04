//! Saved servers. Login checks the address, then writes the profile; it does not open playback.

use std::ffi::{c_char, CStr, CString};
use std::path::Path;

use nori_core::client::login_check;
use nori_core::settings::{
    server_for_login, servers_activated, servers_removed, SavedServer, ServerList, StoredPrefs,
};
use nori_core::settings_store::Settings;
use nori_core::transport::{block_on, FailureKind, NetError};
use nori_http::Http;

pub const LOGIN_OK: i32 = 0;
pub const LOGIN_INCOMPLETE: i32 = 1;
pub const LOGIN_NOT_FOUND: i32 = 2;
pub const LOGIN_UNREACHABLE: i32 = 3;
pub const LOGIN_TIMEOUT: i32 = 4;
pub const LOGIN_CERTIFICATE: i32 = 5;
pub const LOGIN_HTTP: i32 = 6;
pub const LOGIN_PASSWORD: i32 = 7;
pub const LOGIN_FORBIDDEN: i32 = 8;
pub const LOGIN_NOT_SUBSONIC: i32 = 9;
pub const LOGIN_DATABASE: i32 = 10;
pub const LOGIN_OTHER: i32 = 11;
pub const LOGIN_CLEARTEXT: i32 = 12;
pub const LOGIN_METERED: i32 = 13;

fn owned(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).unwrap_or_else(|_| CString::new("").unwrap())
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .unwrap_or("")
        .to_string()
}

/// `https://` when the typed address has no scheme.
fn address(raw: &str) -> String {
    let t = raw.trim().trim_end_matches('/');
    if t.contains("://") {
        t.to_string()
    } else if t.is_empty() {
        String::new()
    } else {
        format!("https://{t}")
    }
}

fn opened(dir: &Path) -> Result<(std::sync::Arc<Settings>, StoredPrefs), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let settings = Settings::new();
    let prefs = settings
        .open(&nori_host::db_path(dir))
        .map_err(|e| e.to_string())?;
    Ok((settings, prefs))
}

fn label(s: &SavedServer) -> String {
    let name = s.name.trim();
    if !name.is_empty() {
        name.to_string()
    } else if !s.user.trim().is_empty() {
        s.user.trim().to_string()
    } else {
        s.url.clone()
    }
}

/// The active server's label, or empty when none are saved.
pub(crate) fn active(dir: &Path) -> Result<String, String> {
    let (_, prefs) = opened(dir)?;
    let id = &prefs.active_server_id;
    let server = prefs
        .servers
        .iter()
        .find(|s| &s.id == id)
        .or_else(|| prefs.servers.first());
    Ok(server.map(label).unwrap_or_default())
}

/// Writes `profile` as the active server and waits until the database has it.
pub(crate) fn keep(dir: &Path, profile: SavedServer) -> Result<(), String> {
    let (settings, mut prefs) = opened(dir)?;
    let list = servers_activated(list_of(&mut prefs), profile);
    prefs.servers = list.servers;
    prefs.active_server_id = list.active_server_id;
    settings.put(prefs);
    nori_core::background::flush();
    Ok(())
}

fn list_of(prefs: &mut StoredPrefs) -> ServerList {
    ServerList {
        servers: std::mem::take(&mut prefs.servers),
        active_server_id: prefs.active_server_id.clone(),
    }
}

/// The saved servers as `(id, label, address, active)`, in the order they were saved.
pub(crate) fn servers(dir: &Path) -> Result<Vec<(String, String, String, bool)>, String> {
    let (_, prefs) = opened(dir)?;
    Ok(prefs
        .servers
        .iter()
        .map(|s| (s.id.clone(), label(s), s.url.clone(), s.id == prefs.active_server_id))
        .collect())
}

/// Makes saved server `id` the active one. False when no server has that id.
pub(crate) fn pick(dir: &Path, id: &str) -> Result<bool, String> {
    let (settings, mut prefs) = opened(dir)?;
    let Some(profile) = prefs.servers.iter().find(|s| s.id == id).cloned() else {
        return Ok(false);
    };
    let list = servers_activated(list_of(&mut prefs), profile);
    prefs.servers = list.servers;
    prefs.active_server_id = list.active_server_id;
    settings.put(prefs);
    nori_core::background::flush();
    Ok(true)
}

/// Forgets saved server `id`; the first one left becomes active if it was.
pub(crate) fn remove(dir: &Path, id: &str) -> Result<(), String> {
    let (settings, mut prefs) = opened(dir)?;
    let list = servers_removed(list_of(&mut prefs), id.to_string());
    prefs.servers = list.servers;
    prefs.active_server_id = list.active_server_id;
    settings.put(prefs);
    nori_core::background::flush();
    Ok(())
}

/// A failure as a login code, with the detail a code words itself.
pub(crate) fn fail(e: NetError) -> (i32, Option<String>) {
    match e {
        NetError::Transport { kind, detail } => match kind {
            FailureKind::UnknownHost => (LOGIN_NOT_FOUND, None),
            FailureKind::Connect | FailureKind::NoRoute => (LOGIN_UNREACHABLE, None),
            FailureKind::Timeout | FailureKind::Interrupted => (LOGIN_TIMEOUT, None),
            FailureKind::Tls => (LOGIN_CERTIFICATE, None),
            FailureKind::Cleartext => (LOGIN_CLEARTEXT, None),
            FailureKind::Metered => (LOGIN_METERED, None),
            FailureKind::Io | FailureKind::Other => (LOGIN_OTHER, detail),
        },
        NetError::Http { status } => (LOGIN_HTTP, Some(status.to_string())),
        NetError::Api { code: 40, .. } => (LOGIN_PASSWORD, None),
        NetError::Api { code: 50, .. } => (LOGIN_FORBIDDEN, None),
        NetError::Api { reason, .. } => (LOGIN_OTHER, Some(reason)),
        NetError::Parse { .. } => (LOGIN_NOT_SUBSONIC, None),
        NetError::Db { reason } => (LOGIN_DATABASE, Some(reason)),
    }
}

/// The login form as the app sends it. `name`, `alt` (the second address) and `key` (an API key) are
/// the advanced fields and may be empty.
#[derive(Debug, Default)]
pub(crate) struct Form {
    pub url: String,
    pub user: String,
    pub password: String,
    pub name: String,
    pub alt: String,
    pub key: String,
}

impl Form {
    /// The app's JSON `{url, user, password, name, alt, key}`; a missing field is empty.
    fn parse(json: &str) -> Form {
        let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
        let field = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Form {
            url: field("url"),
            user: field("user"),
            password: field("password"),
            name: field("name"),
            alt: field("alt"),
            key: field("key"),
        }
    }
}

/// The profile `form` asks for, or None when it lacks an address, or both a user and an API key.
fn draft(form: &Form) -> Option<SavedServer> {
    let url = address(&form.url);
    let user = form.user.trim();
    let key = form.key.trim();
    if url.len() < 8 || (user.is_empty() && key.is_empty()) {
        return None;
    }
    let alt = form.alt.trim();
    Some(SavedServer {
        id: nori_core::settings::new_server_id(),
        name: form.name.trim().to_string(),
        url,
        alt_url: if alt.is_empty() { String::new() } else { address(alt) },
        user: user.to_string(),
        password: form.password.clone(),
        api_key: key.to_string(),
        ..SavedServer::default()
    })
}

/// Pings the server and, when it answers, saves the profile as the active one.
pub(crate) fn login(dir: &Path, form: &Form) -> (i32, Option<String>) {
    let Some(draft) = draft(form) else {
        return (LOGIN_INCOMPLETE, None);
    };
    let (_, prefs) = match opened(dir) {
        Ok(v) => v,
        Err(e) => return (LOGIN_DATABASE, Some(e)),
    };
    let draft = server_for_login(
        ServerList {
            servers: prefs.servers,
            active_server_id: prefs.active_server_id,
        },
        draft,
    );
    let legacy = match block_on(login_check(
        Http::new(),
        nori_host::config(&draft),
        draft.alt_url.clone(),
    )) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let profile = SavedServer {
        legacy_auth: legacy || draft.legacy_auth,
        ..draft
    };
    match keep(dir, profile) {
        Ok(()) => (LOGIN_OK, None),
        Err(e) => (LOGIN_DATABASE, Some(e)),
    }
}

/// The active server's label. Empty when none are saved; NULL for an unreadable path.
///
/// # Safety
/// `data_dir` is NUL-terminated UTF-8, or NULL.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_active(data_dir: *const c_char) -> *mut c_char {
    let dir = text(data_dir);
    if dir.is_empty() {
        return std::ptr::null_mut();
    }
    match active(Path::new(&dir)) {
        Ok(label) => owned(&label).into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Checks the address and saves the profile as the active server. Does not open playback.
/// Returns a [`LOGIN_OK`] code. `detail`, when not NULL, receives a string to free, or NULL.
///
/// # Safety
/// The strings are NUL-terminated UTF-8, or NULL. `detail` is NULL or a pointer this function writes.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_login(
    data_dir: *const c_char,
    form: *const c_char,
    detail: *mut *mut c_char,
) -> i32 {
    if !detail.is_null() {
        unsafe { *detail = std::ptr::null_mut() };
    }
    let dir = text(data_dir);
    if dir.is_empty() {
        return LOGIN_DATABASE;
    }
    let form = Form::parse(&text(form));
    let (code, extra) = login(Path::new(&dir), &form);
    if !detail.is_null() {
        if let Some(extra) = extra.filter(|s| !s.is_empty()) {
            unsafe { *detail = owned(&extra).into_raw() };
        }
    }
    code
}

/// The saved servers as JSON to free: `[{id, label, url, active}]`. NULL for an unreadable path.
///
/// # Safety
/// `data_dir` is NUL-terminated UTF-8, or NULL.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_servers(data_dir: *const c_char) -> *mut c_char {
    let dir = text(data_dir);
    let Ok(list) = servers(Path::new(&dir)) else {
        return std::ptr::null_mut();
    };
    let rows: Vec<serde_json::Value> = list
        .into_iter()
        .map(|(id, label, url, active)| {
            serde_json::json!({ "id": id, "label": label, "url": url, "active": active })
        })
        .collect();
    owned(&serde_json::Value::from(rows).to_string()).into_raw()
}

/// Makes saved server `id` active (1) or finds none with that id (0). The open session keeps playing
/// the old one until `nori_ios_reopen`.
///
/// # Safety
/// Both are NUL-terminated UTF-8, or NULL.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_pick_server(data_dir: *const c_char, id: *const c_char) -> i32 {
    i32::from(pick(Path::new(&text(data_dir)), &text(id)).unwrap_or(false))
}

/// Forgets saved server `id`. Its downloads and index stay in the database.
///
/// # Safety
/// Both are NUL-terminated UTF-8, or NULL.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_remove_server(data_dir: *const c_char, id: *const c_char) {
    let _ = remove(Path::new(&text(data_dir)), &text(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, user: &str) -> SavedServer {
        SavedServer {
            id: id.into(),
            url: format!("http://127.0.0.1:9/{id}"),
            user: user.into(),
            ..SavedServer::default()
        }
    }

    #[test]
    fn a_picked_server_becomes_active_and_a_removed_one_hands_over() {
        let dir = nori_testdir::TempDir::new("ios-pick");
        keep(dir.path(), profile("one", "ada")).unwrap();
        keep(dir.path(), profile("two", "bob")).unwrap();
        assert_eq!(active(dir.path()).unwrap(), "bob");
        assert!(pick(dir.path(), "one").unwrap());
        assert!(!pick(dir.path(), "nope").unwrap());
        assert_eq!(active(dir.path()).unwrap(), "ada");
        remove(dir.path(), "one").unwrap();
        let left = servers(dir.path()).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!((left[0].0.as_str(), left[0].3), ("two", true));
    }

    #[test]
    fn a_saved_server_is_the_active_one() {
        let dir = nori_testdir::TempDir::new("ios-keep");
        let profile = SavedServer {
            id: "one".into(),
            url: "http://127.0.0.1:9".into(),
            user: "ada".into(),
            password: "secret".into(),
            ..SavedServer::default()
        };
        keep(dir.path(), profile).unwrap();
        assert_eq!(active(dir.path()).unwrap(), "ada");
    }

    #[test]
    fn an_unreachable_server_is_not_saved() {
        let dir = nori_testdir::TempDir::new("ios-login");
        let form = Form { url: "http://127.0.0.1:1".into(), user: "ada".into(), password: "secret".into(), ..Form::default() };
        let (code, _) = login(dir.path(), &form);
        assert_eq!(code, LOGIN_UNREACHABLE);
        assert_eq!(active(dir.path()).unwrap(), "");
    }

    #[test]
    fn the_advanced_fields_reach_the_profile_and_a_key_stands_in_for_a_user() {
        let form = Form {
            url: "music.example.org".into(),
            name: " Home ".into(),
            alt: "192.168.1.5:4533".into(),
            key: "k1".into(),
            ..Form::default()
        };
        let p = draft(&form).unwrap();
        assert_eq!((p.name.as_str(), p.api_key.as_str(), p.user.as_str()), ("Home", "k1", ""));
        assert_eq!(p.alt_url, address("192.168.1.5:4533"));
        assert!(p.url.starts_with("http"), "{}", p.url);
        assert!(draft(&Form { key: String::new(), ..form }).is_none(), "neither a user nor a key");
        let parsed = Form::parse(r#"{"url":"u","user":"ada","alt":"a"}"#);
        assert_eq!((parsed.user.as_str(), parsed.alt.as_str(), parsed.key.as_str()), ("ada", "a", ""));
    }

    #[test]
    fn a_blank_login_is_not_saved() {
        let dir = nori_testdir::TempDir::new("ios-blank");
        let (code, _) = login(dir.path(), &Form::default());
        assert_eq!(code, LOGIN_INCOMPLETE);
        assert!(dir.path().read_dir().unwrap().next().is_none());
    }
}
