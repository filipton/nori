//! The one open session. C entry points have no handle to carry one, so it lives in a process-wide slot.

use std::collections::VecDeque;
use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, Once, OnceLock};

use nori_core::remote::Discovery;
use nori_core::rules::QueueMoment;
use nori_core::settings::SavedServer;
use nori_core::settings::StoredPrefs;
use nori_engine::{AudioOutput, Event, State};
use nori_host::session::{Note, Open, Out, Said, Session};
use nori_host::Level;
use nori_http::Http;

/// How much decoded audio the engine may hold. The iPod has 1 GB.
const MEMORY_MB: u32 = 96;

/// This iPod, as the account's other devices and a jam list it.
pub(crate) const DEVICE_NAME: &str = "iPod touch";

/// [`Report::kind`]: the engine's state changed. [`Report::state`] is a [`STATE_IDLE`] value.
pub const REPORT_STATE: i32 = 1;
/// The audible song changed. [`Report::id`] is the song, [`Report::index`] its place, [`Report::jumps`]
/// how many jumps had been asked for.
pub const REPORT_SONG: i32 = 2;
/// The song restarted (repeat one). Fields as [`REPORT_SONG`].
pub const REPORT_LOOPED: i32 = 3;
/// A seek or jump landed, or a position tick. [`Report::ms`] is the position.
pub const REPORT_POSITION: i32 = 4;
/// A song or the output failed. [`Report::id`] may be empty; [`Report::text`] is the message.
pub const REPORT_ERROR: i32 = 5;
/// The music changed output. [`Report::text`] is the device name.
pub const REPORT_OUTPUT: i32 = 6;
/// Playback ran dry (`flag` 1) or resumed (`flag` 0).
pub const REPORT_BUFFERING: i32 = 7;
/// Playback stopped by itself. [`Report::jumps`] is the play number.
pub const REPORT_STOPPED: i32 = 8;
/// Something done or failed, for a status line. [`Report::text`] is a short English note.
pub const REPORT_NOTE: i32 = 9;
/// The server answered (`flag` 1) or did not (`flag` 0, [`Report::text`] is why).
pub const REPORT_REACHABLE: i32 = 10;
/// Lyrics arrived for [`Report::id`]. The record itself is a later call.
pub const REPORT_LYRICS: i32 = 11;
/// A search answer. [`Report::text`] is the query.
pub const REPORT_SEARCH: i32 = 12;
/// A live stream's title. [`Report::text`].
pub const REPORT_TITLE: i32 = 13;
/// A mix became audible (`flag` 1) or ended (`flag` 0).
pub const REPORT_MIXING: i32 = 14;
/// An unreachable song is waiting for the offline bridge. [`Report::jumps`] is the play number.
pub const REPORT_BRIDGE: i32 = 15;
/// The place moved without a jump. [`Report::index`] and [`Report::ms`].
pub const REPORT_PLACED: i32 = 16;
/// Whether the CPU must stay awake (`flag` 1) or may sleep (`flag` 0).
pub const REPORT_AWAKE: i32 = 17;
/// The other devices changed, or the one playing (remote control): read what plays and the devices again.
pub const REPORT_REMOTE: i32 = 18;
/// Another device set the volume; the app sets the system's. [`Report::ms`] is it in thousandths.
pub const REPORT_VOLUME: i32 = 19;
/// A jam joined (`flag` 1: the guest profile is the active one, to open) or not (`flag` 0: [`Report::index`]
/// is why, a `NORI_LOGIN_*` code, [`Report::text`] its detail).
pub const REPORT_JAM_JOINED: i32 = 20;
/// The jam left: `flag` 1 when the profile to go back to is the active one (to open), 0 when there is none.
pub const REPORT_JAM_LEFT: i32 = 21;

pub const STATE_IDLE: i32 = 0;
pub const STATE_PLAYING: i32 = 1;
pub const STATE_PAUSED: i32 = 2;
pub const STATE_ENDED: i32 = 3;

/// One report. `id` and `text` are valid only for the duration of the callback.
#[repr(C)]
pub struct Report {
    pub kind: i32,
    pub state: i32,
    pub index: i32,
    pub ms: i64,
    pub jumps: u64,
    pub flag: i32,
    pub id: *const c_char,
    pub text: *const c_char,
}

pub type ReportFn = unsafe extern "C" fn(*const Report);

struct Hold {
    session: Mutex<Session>,
}

struct Mail {
    q: Mutex<VecDeque<Said>>,
    cv: Condvar,
}

static SESSION: Mutex<Option<Arc<Hold>>> = Mutex::new(None);
static MAIL: OnceLock<Mail> = OnceLock::new();
static OUT: Once = Once::new();
static HOOK: Mutex<Option<ReportFn>> = Mutex::new(None);

fn mail() -> &'static Mail {
    MAIL.get_or_init(|| Mail {
        q: Mutex::new(VecDeque::new()),
        cv: Condvar::new(),
    })
}

fn enqueue(said: Said) {
    let mail = mail();
    mail.q
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(said);
    mail.cv.notify_one();
}

fn deliver() {
    let mail = mail();
    loop {
        let batch: Vec<Said> = {
            let mut q = mail.q.lock().unwrap_or_else(|e| e.into_inner());
            while q.is_empty() {
                q = mail.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
            q.drain(..).collect()
        };
        for said in batch {
            deliver_one(said);
        }
    }
}

fn deliver_one(said: Said) {
    match &said {
        Said::Engine(e) => {
            with_session(|s| {
                s.followed(e);
                if let Event::Song { index, .. } = e {
                    crate::pages::warm_around(s, *index);
                }
            });
        }
        Said::Lyrics { song, pick } => crate::pages::lyrics_arrived(song, &pick.lyrics),
        Said::JamEnded { host } => return crate::jam::left(Some(host.clone())),
        _ => {}
    }
    emit(&said);
}

fn emit(said: &Said) {
    emit_packed(pack(said));
}

/// A report of the app's own (a jam joined or left), from any thread: `flag`, `index` and `text` as its
/// kind says.
pub(crate) fn report(kind: i32, flag: i32, index: i32, text: &str) {
    emit_packed(Packed { kind, state: 0, index, ms: 0, jumps: 0, flag, id: c(""), text: c(text) });
}

fn emit_packed(packed: Packed) {
    let cb = *HOOK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(cb) = cb else { return };
    let report = Report {
        kind: packed.kind,
        state: packed.state,
        index: packed.index,
        ms: packed.ms,
        jumps: packed.jumps,
        flag: packed.flag,
        id: packed.id.as_ptr(),
        text: packed.text.as_ptr(),
    };
    unsafe { cb(&report) };
}

struct Packed {
    kind: i32,
    state: i32,
    index: i32,
    ms: i64,
    jumps: u64,
    flag: i32,
    id: CString,
    text: CString,
}

fn c(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).unwrap_or_else(|_| CString::new("").unwrap())
}

fn ix(i: usize) -> i32 {
    i32::try_from(i).unwrap_or(-1)
}

fn state_code(s: State) -> i32 {
    match s {
        State::Idle => STATE_IDLE,
        State::Playing => STATE_PLAYING,
        State::Paused => STATE_PAUSED,
        State::Ended => STATE_ENDED,
    }
}

fn pack(said: &Said) -> Packed {
    let mut p = Packed {
        kind: REPORT_NOTE,
        state: 0,
        index: 0,
        ms: 0,
        jumps: 0,
        flag: 0,
        id: c(""),
        text: c(""),
    };
    match said {
        Said::Engine(e) => pack_event(&mut p, e),
        Said::Lyrics { song, .. } => {
            p.kind = REPORT_LYRICS;
            p.id = c(song);
        }
        Said::Search(view) => {
            p.kind = REPORT_SEARCH;
            p.text = c(&view.query);
        }
        Said::Note(n) => {
            p.kind = REPORT_NOTE;
            let (code, count, detail) = note(n);
            p.flag = code;
            p.index = count;
            p.text = c(&detail);
        }
        Said::Reachable(Ok(())) => {
            p.kind = REPORT_REACHABLE;
            p.flag = 1;
        }
        Said::Reachable(Err(e)) => {
            p.kind = REPORT_REACHABLE;
            p.text = c(&e.to_string());
        }
        Said::Remote => p.kind = REPORT_REMOTE,
        // Reported as the guest profile is dropped (jam::left).
        Said::JamEnded { .. } => {}
        Said::Volume(v) => {
            p.kind = REPORT_VOLUME;
            p.ms = (v * 1000.0).round() as i64;
        }
        // Hearts are read again when a page or the player is.
        Said::Starred(_) => {}
    }
    p
}

fn pack_event(p: &mut Packed, e: &Event) {
    match e {
        Event::State(s) => {
            p.kind = REPORT_STATE;
            p.state = state_code(*s);
        }
        Event::Song {
            index, id, jumps, ..
        } => {
            p.kind = REPORT_SONG;
            p.index = ix(*index);
            p.jumps = *jumps;
            p.id = c(id);
        }
        Event::Looped {
            index, id, jumps, ..
        } => {
            p.kind = REPORT_LOOPED;
            p.index = ix(*index);
            p.jumps = *jumps;
            p.id = c(id);
        }
        Event::Position { index, ms, jumps } => {
            p.kind = REPORT_POSITION;
            p.index = ix(*index);
            p.ms = *ms;
            p.jumps = *jumps;
        }
        Event::Error { id, message } => {
            p.kind = REPORT_ERROR;
            p.id = c(id);
            p.text = c(message);
        }
        Event::Output { name } => {
            p.kind = REPORT_OUTPUT;
            p.text = c(name);
        }
        Event::Buffering(on) => {
            p.kind = REPORT_BUFFERING;
            p.flag = i32::from(*on);
        }
        Event::Stopped { plays } => {
            p.kind = REPORT_STOPPED;
            p.jumps = *plays;
        }
        Event::Title(t) => {
            p.kind = REPORT_TITLE;
            p.text = c(t);
        }
        Event::Mixing(on) => {
            p.kind = REPORT_MIXING;
            p.flag = i32::from(*on);
        }
        Event::Bridge { plays } => {
            p.kind = REPORT_BRIDGE;
            p.jumps = *plays;
        }
        Event::Placed { index, ms } => {
            p.kind = REPORT_PLACED;
            p.index = ix(*index);
            p.ms = *ms;
        }
        Event::Awake(on) => {
            p.kind = REPORT_AWAKE;
            p.flag = i32::from(*on);
        }
        // Listening along paused or played here, or the host did: the jam's controls are read again.
        Event::Following(_) => p.kind = REPORT_REMOTE,
    }
}

pub const NOTE_QUEUED_NEXT: i32 = 1;
pub const NOTE_QUEUED_LAST: i32 = 2;
pub const NOTE_NOTHING_TO_PLAY: i32 = 3;
pub const NOTE_SONGS_FAILED: i32 = 4;
pub const NOTE_NOTHING_TO_PUT_BACK: i32 = 5;
pub const NOTE_DOWNLOADING: i32 = 6;
pub const NOTE_DOWNLOAD_FAILED: i32 = 7;
pub const NOTE_STARRED: i32 = 8;
pub const NOTE_UNSTARRED: i32 = 9;
pub const NOTE_STAR_FAILED: i32 = 10;
pub const NOTE_INDEXING: i32 = 11;
pub const NOTE_INDEXED: i32 = 12;
pub const NOTE_INDEX_STOPPED: i32 = 13;
pub const NOTE_DONE: i32 = 14;
pub const NOTE_FORGOT: i32 = 15;

/// A note as its code, a count where it has one, and the failure's English detail for the log.
fn note(n: &Note) -> (i32, i32, String) {
    let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    match n {
        Note::Queued { next: true, songs } => (NOTE_QUEUED_NEXT, count(*songs), String::new()),
        Note::Queued { next: false, songs } => (NOTE_QUEUED_LAST, count(*songs), String::new()),
        Note::NothingToPlay => (NOTE_NOTHING_TO_PLAY, 0, String::new()),
        Note::SongsFailed(e) => (NOTE_SONGS_FAILED, 0, e.to_string()),
        Note::NothingToPutBack => (NOTE_NOTHING_TO_PUT_BACK, 0, String::new()),
        Note::Downloading(n) => (NOTE_DOWNLOADING, count(*n), String::new()),
        Note::DownloadFailed(e) => (NOTE_DOWNLOAD_FAILED, 0, e.to_string()),
        Note::Starred(true) => (NOTE_STARRED, 0, String::new()),
        Note::Starred(false) => (NOTE_UNSTARRED, 0, String::new()),
        Note::StarFailed(e) => (NOTE_STAR_FAILED, 0, e.to_string()),
        Note::Indexing => (NOTE_INDEXING, 0, String::new()),
        Note::Indexed(t) => (NOTE_INDEXED, count(t.songs as usize), String::new()),
        Note::IndexStopped(e) => (NOTE_INDEX_STOPPED, 0, e.to_string()),
        Note::Done(_) => (NOTE_DONE, 0, String::new()),
        Note::Forgot(n) => (NOTE_FORGOT, count(*n as usize), String::new()),
    }
}

/// The output went away or the system took the audio: the engine's answer. Called from the shim's
/// notifications on the device.
#[cfg(target_os = "ios")]
pub(crate) fn audio_changed() {
    use crate::output::{take_interruption, take_route_lost, Interrupt};
    let lost = take_route_lost();
    let interrupt = take_interruption();
    with_session(|s| {
        if lost {
            s.engine.pause();
        }
        match interrupt {
            Some(Interrupt::Began) => s.engine.pause_now(),
            Some(Interrupt::Ended { resume: true }) => {
                s.engine.play();
            }
            _ => {}
        }
    });
}

/// The system's volume, which the app follows. A process-wide slot: the system tells it before a
/// session opens.
fn level() -> &'static Arc<Level> {
    static LEVEL: OnceLock<Arc<Level>> = OnceLock::new();
    // Nothing of nori's multiplies samples for volume; another device setting it is told to the app
    // ([`REPORT_VOLUME`]), which sets the system's.
    LEVEL.get_or_init(|| Level::new(1.0, Some(Box::new(|_| {}))))
}

/// The system volume moved (0 to 1): loudness compensation follows it, and the account's other devices
/// see it.
#[no_mangle]
pub extern "C" fn nori_ios_volume(fraction: f32) {
    if with_session(|s| s.volume_followed(fraction)).is_none() {
        level().set(fraction);
    }
}

fn saved(prefs: &StoredPrefs, id: &str) -> Option<SavedServer> {
    let id = if id.is_empty() {
        prefs.active_server_id.as_str()
    } else {
        id
    };
    prefs
        .servers
        .iter()
        .find(|s| s.id == id && !s.id.is_empty())
        .cloned()
}

fn device_output() -> Result<Box<dyn AudioOutput>, String> {
    #[cfg(target_os = "ios")]
    {
        Ok(Box::new(crate::IosOutput::device()))
    }
    #[cfg(not(target_os = "ios"))]
    {
        Err("this host has no sound card".into())
    }
}

/// Opens `profile` on `output`, finding nearby devices through `discovery`.
#[cfg(test)]
pub(crate) fn start(
    dir: &Path,
    profile: SavedServer,
    output: Box<dyn AudioOutput>,
    offline: bool,
    discovery: Option<Arc<dyn Discovery>>,
) -> Result<(), String> {
    let queue = Arc::new(nori_core::queue::Session::new(
        nori_core::settings_store::Settings::new(),
    ));
    queue
        .settings
        .open(&nori_host::db_path(dir))
        .map_err(|e| format!("the database: {e}"))?;
    start_queue(dir, queue, profile, output, offline, discovery)
}

fn start_queue(
    dir: &Path,
    queue: Arc<nori_core::queue::Session>,
    profile: SavedServer,
    output: Box<dyn AudioOutput>,
    offline: bool,
    discovery: Option<Arc<dyn Discovery>>,
) -> Result<(), String> {
    if held().is_some() {
        return Err("already open".into());
    }
    let volume = level().clone();
    let out: Out = Arc::new(enqueue);
    let session = Session::open(Open {
        queue,
        data: dir,
        http: Http::new(),
        profile,
        output,
        volume,
        memory_mb: MEMORY_MB,
        covers: true,
        offline,
        mpris: None,
        device: nori_core::remote::RemoteMe { name: DEVICE_NAME.into(), kind: nori_core::remote::wire::DeviceKind::Phone },
        discovery,
        out,
    })?;
    if !offline {
        session.check();
    }
    {
        let mut slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            session.close();
            return Err("already open".into());
        }
        *slot = Some(Arc::new(Hold {
            session: Mutex::new(session),
        }));
    }
    OUT.call_once(|| {
        std::thread::Builder::new()
            .name("nori-ios-out".into())
            .spawn(deliver)
            .ok();
    });
    Ok(())
}

fn open_saved(dir: &Path, server_id: &str) -> Result<(), String> {
    if held().is_some() {
        return Err("already open".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let queue = Arc::new(nori_core::queue::Session::new(
        nori_core::settings_store::Settings::new(),
    ));
    let prefs = queue
        .settings
        .open(&nori_host::db_path(dir))
        .map_err(|e| format!("the database: {e}"))?;
    let profile = saved(&prefs, server_id).ok_or_else(|| "no saved server".to_string())?;
    start_queue(dir, queue, profile, device_output()?, false, Some(Arc::new(crate::remote::Bonjour)))
}

fn text(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return Some(String::new());
    }
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

fn err_ptr(msg: &str) -> *mut c_char {
    c(msg).into_raw()
}

/// `p` as an owned string; empty for NULL or invalid UTF-8.
pub(crate) fn c_text(p: *const c_char) -> String {
    text(p).unwrap_or_default()
}

fn held() -> Option<Arc<Hold>> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn with_session<R>(f: impl FnOnce(&Session) -> R) -> Option<R> {
    held().map(|h| {
        let s = h.session.lock().unwrap_or_else(|p| p.into_inner());
        f(&s)
    })
}

/// Saves the queue, stops playback and empties the slot.
fn close() {
    let gone = SESSION.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(h) = gone {
        h.session.lock().unwrap_or_else(|p| p.into_inner()).close();
    }
}

/// Where reports go. `NULL` clears it. Called from any thread; the callback runs on `nori-ios-out`,
/// and may re-enter a control.
///
/// # Safety
/// `cb`, when set, is a function the process keeps for as long as it stays set.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_on_report(cb: Option<ReportFn>) {
    *HOOK.lock().unwrap_or_else(|e| e.into_inner()) = cb;
}

/// Opens the saved server `server_id` (empty: the active one) under `data_dir` and starts playback's
/// session. `NULL` on success; otherwise an English error to free with `nori_ios_free`. A failure
/// leaves no session open.
///
/// # Safety
/// Both pointers are NUL-terminated UTF-8, or `NULL`.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_open(
    data_dir: *const c_char,
    server_id: *const c_char,
) -> *mut c_char {
    let Some(dir) = text(data_dir).filter(|d| !d.is_empty()) else {
        return err_ptr("unreadable path");
    };
    let id = text(server_id).unwrap_or_default();
    match open_saved(Path::new(&dir), &id) {
        Ok(()) => std::ptr::null_mut(),
        Err(e) => err_ptr(&e),
    }
}

/// Keeps the core's log in `dir/nori.log` (`alog_persist`), local times at `utc_offset_min`.
///
/// # Safety
/// `dir` is NUL-terminated UTF-8, or `NULL`.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_keep_log(dir: *const c_char, utc_offset_min: i32) {
    if let Some(dir) = text(dir).filter(|d| !d.is_empty()) {
        nori_core::alog::alog_persist(dir, utc_offset_min);
    }
}

/// Closes the open session, if any, and opens the active saved server in its place. As
/// [`nori_ios_open`] for the answer.
///
/// # Safety
/// `data_dir` is NUL-terminated UTF-8, or `NULL`.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_reopen(data_dir: *const c_char) -> *mut c_char {
    let Some(dir) = text(data_dir).filter(|d| !d.is_empty()) else {
        return err_ptr("unreadable path");
    };
    close();
    crate::pages::forget_lists();
    match open_saved(Path::new(&dir), "") {
        Ok(()) => std::ptr::null_mut(),
        Err(e) => err_ptr(&e),
    }
}

/// Plays queue index `index` from `ms`, on the device playing. Returns the jump number, or 0 when
/// nothing is open or another device plays.
#[no_mangle]
pub extern "C" fn nori_ios_play_at(index: i32, ms: i64) -> u64 {
    let index = index.max(0) as usize;
    // A jam guest's queue is its host's, which it plays nothing of.
    with_session(|s| {
        if s.elsewhere().is_some() || s.guest {
            s.jump(index);
            0
        } else {
            s.engine.play_at(index, ms)
        }
    })
    .unwrap_or(0)
}

/// The controls act on the device playing: this one, or the other device it mirrors.
#[no_mangle]
pub extern "C" fn nori_ios_toggle() {
    with_session(|s| s.toggle());
}

#[no_mangle]
pub extern "C" fn nori_ios_next() {
    with_session(|s| s.next());
}

#[no_mangle]
pub extern "C" fn nori_ios_previous() {
    with_session(|s| s.previous());
}

#[no_mangle]
pub extern "C" fn nori_ios_seek(ms: i64) {
    with_session(|s| s.seek(ms));
}

/// Goes to `index` at `ms`, playing or paused as before. Returns the jump number, or 0 when nothing is open.
#[no_mangle]
pub extern "C" fn nori_ios_go_to(index: i32, ms: i64) -> u64 {
    with_session(|s| s.engine.go_to(index.max(0) as usize, ms)).unwrap_or(0)
}

/// Repeat off (0), one (1) or all (2).
#[no_mangle]
pub extern "C" fn nori_ios_set_repeat(mode: i32) {
    if (0..=2).contains(&mode) {
        with_session(|s| s.repeat(mode as u8));
    }
}

/// `on` is 0 or 1.
#[no_mangle]
pub extern "C" fn nori_ios_shuffle(on: i32) {
    with_session(|s| s.shuffle(on != 0));
}

#[no_mangle]
pub extern "C" fn nori_ios_remove(index: i32) {
    if index >= 0 {
        with_session(|s| s.remove(index as usize));
    }
}

#[no_mangle]
pub extern "C" fn nori_ios_clear_upcoming() {
    with_session(|s| s.clear_upcoming());
}

/// # Safety
/// `id` is NUL-terminated UTF-8, or `NULL`.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_put_back(id: *const c_char) {
    let Some(id) = text(id).filter(|s| !s.is_empty()) else {
        return;
    };
    with_session(|s| s.put_back(&id));
}

#[no_mangle]
pub extern "C" fn nori_ios_move(from: i32, to: i32) {
    if from >= 0 && to >= 0 {
        with_session(|s| s.move_song(from as usize, to as usize));
    }
}

/// Saves the queue now, as the app leaves the foreground. Does not push it to the server.
#[no_mangle]
pub extern "C" fn nori_ios_background() {
    with_session(|s| s.keep(QueueMoment::Closing));
}

/// Drops decoded covers.
#[no_mangle]
pub extern "C" fn nori_ios_memory_warning() {
    with_session(|s| {
        if let Some(covers) = &s.covers {
            covers.trim();
        }
    });
}

/// 1 when a playback session is open.
#[no_mangle]
pub extern "C" fn nori_ios_is_open() -> i32 {
    i32::from(held().is_some())
}

#[cfg(test)]
mod tests {
    use std::ffi::{CStr, CString};
    use std::sync::{Condvar, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use nori_core::settings::SavedServer;
    use nori_core::Song;
    use nori_engine::WavOutput;

    use super::{
        Report, ReportFn, REPORT_ERROR, REPORT_POSITION, REPORT_SONG, REPORT_STATE, REPORT_VOLUME,
        STATE_PAUSED, STATE_PLAYING,
    };

    struct Sink {
        got: Mutex<Vec<Rec>>,
        cv: Condvar,
    }

    #[derive(Clone, Debug)]
    struct Rec {
        kind: i32,
        state: i32,
        flag: i32,
        index: i32,
        ms: i64,
        id: String,
        text: String,
    }

    static SINK: OnceLock<Sink> = OnceLock::new();

    unsafe extern "C" fn on_report(r: *const Report) {
        let r = unsafe { &*r };
        let rec = Rec {
            kind: r.kind,
            state: r.state,
            flag: r.flag,
            index: r.index,
            ms: r.ms,
            id: copy_c(r.id),
            text: copy_c(r.text),
        };
        let sink = SINK.get().unwrap();
        sink.got.lock().unwrap().push(rec);
        sink.cv.notify_all();
    }

    fn copy_c(p: *const std::ffi::c_char) -> String {
        if p.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(p) }
            .to_str()
            .unwrap_or("")
            .to_string()
    }

    fn mark() -> usize {
        SINK.get().unwrap().got.lock().unwrap().len()
    }

    fn wait_new(from: usize, pred: impl Fn(&Rec) -> bool) -> Rec {
        let sink = SINK.get().unwrap();
        let mut g = sink.got.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(r) = g.iter().skip(from).find(|r| pred(r)) {
                return r.clone();
            }
            let now = Instant::now();
            assert!(now < deadline, "timed out; saw {g:?}");
            let (next, timeout) = sink.cv.wait_timeout(g, deadline - now).unwrap();
            g = next;
            if timeout.timed_out() {
                if let Some(r) = g.iter().skip(from).find(|r| pred(r)) {
                    return r.clone();
                }
                panic!("timed out; saw {g:?}");
            }
        }
    }

    fn expect(from: usize, kind: i32, pred: impl Fn(&Rec) -> bool) -> Rec {
        let r = wait_new(from, |r| {
            r.kind == REPORT_ERROR || (r.kind == kind && pred(r))
        });
        assert_ne!(r.kind, REPORT_ERROR, "{}", r.text);
        r
    }

    fn song_of(from: usize, id: &str, index: i32) -> Rec {
        let r = expect(from, REPORT_SONG, |r| r.id == id);
        assert_eq!((r.id.as_str(), r.index), (id, index));
        r
    }

    fn song(id: &str) -> Song {
        Song {
            id: id.into(),
            title: id.into(),
            duration: 20,
            suffix: "wav".into(),
            ..Song::default()
        }
    }

    fn album_order() -> String {
        let raw = crate::pages::nori_ios_sorts(crate::pages::PAGE_ALBUMS);
        let v: serde_json::Value =
            serde_json::from_str(unsafe { CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
        unsafe { crate::nori_ios_free(raw) };
        v["now"].as_str().unwrap().to_string()
    }

    struct Close;

    impl Drop for Close {
        fn drop(&mut self) {
            super::close();
        }
    }

    fn hear_reports() {
        SINK.get_or_init(|| Sink {
            got: Mutex::new(Vec::new()),
            cv: Condvar::new(),
        });
        unsafe { super::nori_ios_on_report(Some(on_report as ReportFn)) };
    }

    /// The process holds one session: the tests that open it take turns.
    fn one_session() -> std::sync::MutexGuard<'static, ()> {
        static ONE: Mutex<()> = Mutex::new(());
        ONE.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn profile() -> SavedServer {
        SavedServer {
            id: "test".into(),
            name: "Test".into(),
            url: "http://127.0.0.1:9".into(),
            user: "u".into(),
            password: "p".into(),
            ..SavedServer::default()
        }
    }

    /// Puts `songs` in the open session's downloads as `seconds` long WAV files.
    fn downloaded(songs: &[Song], seconds: u32) {
        let bytes = crate::output::sim::wav(seconds);
        let h = super::held().unwrap();
        let s = h.session.lock().unwrap();
        s.core.download_queue(songs.to_vec()).unwrap();
        s.core
            .download_settle(songs.iter().map(|s| s.id.clone()).collect(), vec![true; songs.len()])
            .unwrap();
        for song in songs {
            let path = s.store.download_path(&song.id);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
    }

    #[test]
    fn the_controls_play_a_queue_of_files() {
        let _one = one_session();
        hear_reports();

        let empty = nori_testdir::TempDir::new("ios-open");
        let path = CString::new(empty.path().to_str().unwrap()).unwrap();
        let err = unsafe { super::nori_ios_open(path.as_ptr(), std::ptr::null()) };
        assert!(!err.is_null());
        let msg = unsafe { CStr::from_ptr(err) }.to_str().unwrap().to_string();
        unsafe { crate::nori_ios_free(err) };
        assert!(msg.contains("no saved server"), "{msg}");
        assert!(super::held().is_none());

        let dir = nori_testdir::TempDir::new("ios-play");
        let _close = Close;
        super::start(
            dir.path(),
            profile(),
            Box::new(WavOutput::new(dir.path().join("heard.wav"), 2.0)),
            true,
            None,
        )
        .unwrap();

        let songs = vec![song("a"), song("b")];
        downloaded(&songs, 20);
        let at = mark();
        {
            let h = super::held().unwrap();
            let s = h.session.lock().unwrap();
            s.play(songs, 0, false, None);
        }

        song_of(at, "a", 0);

        let at = mark();
        super::nori_ios_toggle();
        expect(at, REPORT_STATE, |r| r.state == STATE_PAUSED);

        let at = mark();
        super::nori_ios_toggle();
        expect(at, REPORT_STATE, |r| r.state == STATE_PLAYING);

        let at = mark();
        super::nori_ios_seek(4_000);
        let pos = expect(at, REPORT_POSITION, |r| (3_500..4_500).contains(&r.ms));
        assert!(pos.ms >= 3_500, "{pos:?}");

        let at = mark();
        super::nori_ios_next();
        song_of(at, "b", 1);

        let at = mark();
        super::nori_ios_previous();
        song_of(at, "a", 0);

        let at = mark();
        assert!(super::nori_ios_play_at(1, 0) > 0);
        song_of(at, "b", 1);

        let at = mark();
        assert!(super::nori_ios_go_to(0, 0) > 0);
        song_of(at, "a", 0);

        super::nori_ios_set_repeat(2);
        super::nori_ios_shuffle(1);
        {
            let h = super::held().unwrap();
            let s = h.session.lock().unwrap();
            let (repeat, lit, len) = s.core.session.playlist(|p| (p.repeat(), p.lit(), p.len()));
            assert_eq!((repeat, lit, len), (2, true, 2));
        }

        let at = mark();
        super::nori_ios_toggle();
        expect(at, REPORT_STATE, |r| r.state == STATE_PAUSED);

        let first = {
            let h = super::held().unwrap();
            let s = h.session.lock().unwrap();
            s.core
                .session
                .playlist(|p| p.ids().first().cloned().unwrap())
        };
        super::nori_ios_remove(0);
        {
            let h = super::held().unwrap();
            let s = h.session.lock().unwrap();
            assert_eq!(s.core.session.playlist(|p| p.len()), 1);
        }
        let back = CString::new(first).unwrap();
        unsafe { super::nori_ios_put_back(back.as_ptr()) };
        super::nori_ios_move(0, 1);
        let ids = {
            let h = super::held().unwrap();
            let s = h.session.lock().unwrap();
            s.core.session.playlist(|p| p.ids().to_vec())
        };
        assert_eq!(ids.len(), 2);

        super::nori_ios_background();
        {
            let h = super::held().unwrap();
            let s = h.session.lock().unwrap();
            let saved = s.core.load_queue().unwrap();
            let saved_ids: Vec<_> = saved.songs.iter().map(|song| song.id.clone()).collect();
            assert_eq!(saved_ids, ids);
        }

        let newest = CString::new("newest").unwrap();
        let unknown = CString::new("byMood").unwrap();
        unsafe {
            crate::pages::nori_ios_sort(crate::pages::PAGE_ALBUMS, newest.as_ptr());
            crate::pages::nori_ios_sort(crate::pages::PAGE_ALBUMS, unknown.as_ptr());
        }
        assert_eq!(album_order(), "newest");

        let kinds = crate::pages::nori_ios_presets();
        let bass = serde_json::from_str::<Vec<u8>>(unsafe { CStr::from_ptr(kinds) }.to_str().unwrap())
            .unwrap()
            .iter()
            .position(|k| *k == nori_core::PresetKind::BassBoost as u8)
            .unwrap();
        unsafe { crate::nori_ios_free(kinds) };
        assert_eq!(crate::pages::nori_ios_preset(bass as u32), 1);
        assert_eq!(crate::pages::nori_ios_preset(bass as u32), 0, "the same curve again changes nothing");
        assert_eq!(crate::pages::nori_ios_preset(999), 0);
        {
            let h = super::held().unwrap();
            let s = h.session.lock().unwrap();
            let p = s.core.session.settings.current().unwrap();
            assert!(p.eq_enabled);
            assert!(p.eq_graphic[0] > p.eq_graphic[p.eq_graphic.len() - 1], "{:?}", p.eq_graphic);
        }

        super::nori_ios_memory_warning();
        let at = mark();
        super::nori_ios_toggle();
        expect(at, REPORT_STATE, |r| r.state == STATE_PLAYING);

        super::close();
        assert_eq!(super::nori_ios_is_open(), 0);
        super::start(
            dir.path(),
            SavedServer {
                id: "test".into(),
                url: "http://127.0.0.1:9".into(),
                user: "u".into(),
                ..SavedServer::default()
            },
            Box::new(WavOutput::new(dir.path().join("again.wav"), 2.0)),
            true,
            None,
        )
        .unwrap();
        assert_eq!(album_order(), "newest");
        let reopened = super::held().unwrap();
        let s = reopened.session.lock().unwrap();
        assert_eq!(s.core.load_queue().unwrap().songs.len(), ids.len());
    }

    /// A jam guest's session: the host's library, nothing of the account's, a tap on the queue playing
    /// nothing here; leaving drops the guest profile for the user's own, and a join that cannot reach the
    /// server says why.
    #[test]
    fn a_jam_guest_plays_nothing_itself_and_leaves_for_its_own_profile() {
        use super::{REPORT_JAM_JOINED, REPORT_JAM_LEFT};
        let _one = one_session();
        hear_reports();
        let dir = nori_testdir::TempDir::new("ios-jam");
        let _close = Close;
        let queue = std::sync::Arc::new(nori_core::queue::Session::new(nori_core::settings_store::Settings::new()));
        let mut prefs = queue.settings.open(&nori_host::db_path(dir.path())).unwrap();
        prefs.servers = vec![profile()];
        prefs.active_server_id = "test".into();
        queue.settings.put(prefs);
        let pass = nori_core::remote::JamPass { url: "http://127.0.0.1:9".into(), api_key: nori_remote::guest_key("k") };
        let guest = nori_host::jam_joined(&queue.settings, pass, "Jam");
        nori_core::background::flush();
        super::start_queue(dir.path(), queue.clone(), guest, Box::new(WavOutput::new(dir.path().join("heard.wav"), 2.0)), true, None).unwrap();

        let json = |p: *mut std::ffi::c_char| -> serde_json::Value {
            let v = serde_json::from_str(unsafe { CStr::from_ptr(p) }.to_str().unwrap()).unwrap();
            unsafe { crate::nori_ios_free(p) };
            v
        };
        let rules = json(crate::jam::nori_ios_rules());
        assert_eq!((rules["asks"].as_bool(), rules["account"].as_bool()), (Some(true), Some(false)));
        let now = json(crate::pages::nori_ios_now());
        assert_eq!((now["jam"].as_bool(), now["remote"].as_bool(), now.get("device")), (Some(true), Some(false), None), "{now}");
        assert_eq!(super::nori_ios_play_at(0, 0), 0, "the host's queue plays nothing here");

        let link = CString::new("http://127.0.0.1:9/nori/jam#s=http%3A%2F%2F127.0.0.1%3A9&k=abc").unwrap();
        let name = CString::new("Jam").unwrap();
        let nope = CString::new("hello").unwrap();
        assert_eq!(unsafe { crate::jam::nori_ios_jam_join(nope.as_ptr(), name.as_ptr()) }, crate::jam::JOIN_NOT_AN_INVITE);
        let at = mark();
        assert_eq!(unsafe { crate::jam::nori_ios_jam_join(link.as_ptr(), name.as_ptr()) }, crate::jam::JOIN_STARTED);
        let joined = wait_new(at, |r| r.kind == REPORT_JAM_JOINED);
        assert_eq!((joined.flag, joined.index), (0, crate::account::LOGIN_UNREACHABLE), "{joined:?}");

        let at = mark();
        crate::jam::nori_ios_jam_leave();
        assert_eq!(wait_new(at, |r| r.kind == REPORT_JAM_LEFT).flag, 1);
        let p = queue.settings.current().unwrap();
        let ids: Vec<&str> = p.servers.iter().map(|s| s.id.as_str()).collect();
        assert_eq!((ids, p.active_server_id.as_str()), (vec!["test"], "test"), "the user's own again");
    }

    /// What the iPod's remote asked Bonjour to announce.
    #[derive(Default)]
    struct Announced(Mutex<Option<nori_core::remote::Announcement>>);

    impl nori_core::remote::Discovery for Announced {
        fn announce(&self, door: Option<nori_core::remote::Announcement>) {
            *self.0.lock().unwrap() = door;
        }

        fn browse(&self, _: bool) {}
    }

    struct Obeys;

    impl nori_core::remote::RemotePlayer for Obeys {
        fn apply(&self, _: nori_core::remote::wire::Op) {}
    }

    /// Change notices, so the test waits for news rather than for time.
    struct News(Mutex<std::sync::mpsc::Sender<()>>);

    impl nori_core::remote::RemoteShown for News {
        fn changed(&self) {
            let _ = self.0.lock().unwrap().send(());
        }

        fn jam_ended(&self, _: Option<String>) {}
    }

    /// The iPod's session as a device of the account, on the simulated audio unit called back in real time,
    /// and a phone controlling it over the network: it starts the iPod playing, and the playhead it mirrors
    /// is the place the iPod's listener hears.
    #[test]
    fn a_phone_controls_the_ipod_and_mirrors_the_place_heard() {
        use crate::output::sim::Sim;
        use crate::output::{Clock, HostClock};
        use nori_core::remote::wire::{DeviceKind, Op};
        use nori_core::remote::{Remote, RemoteMe};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let _one = one_session();
        hear_reports();
        let dir = nori_testdir::TempDir::new("ios-remote");
        let _close = Close;
        let sim = Sim::new();
        let out = crate::IosOutput::new(Box::new(sim.clone()), HostClock);
        let render = out.render.clone();
        let announced = Arc::new(Announced::default());
        super::start(dir.path(), profile(), Box::new(out), false, Some(announced.clone())).unwrap();
        // The audio unit, calling back a buffer ahead of each one it plays.
        let stop = Arc::new(AtomicBool::new(false));
        let unit = {
            let (sim, render, stop) = (sim.clone(), render.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut due = HostClock.now_us();
                while !stop.load(Ordering::Relaxed) {
                    let now = HostClock.now_us();
                    if now < due {
                        std::thread::sleep(Duration::from_micros(due - now));
                        continue;
                    }
                    sim.callback(&render, due);
                    due += sim.period_us(&render);
                }
            })
        };
        let songs = vec![Song { duration: 60, ..song("a") }];
        downloaded(&songs, 60);
        let ipod = {
            let h = super::held().unwrap();
            let s = h.session.lock().unwrap();
            assert!(s.setting("remoteControl", "true").is_some());
            s.remote().expect("remote control on").id()
        };
        let door = announced.0.lock().unwrap().clone().expect("the door announced");

        // The phone, signed in to the same account, finds the door as Bonjour would.
        let core = nori_core::Core::new(String::new(), "phone".into(), Default::default()).unwrap();
        core.configure(nori_host::config(&profile())).unwrap();
        let client = nori_core::client::Client::new(core, nori_http::Http::new(), Default::default());
        client.set_profile(nori_host::net(&profile()));
        let (news_to, news) = std::sync::mpsc::channel();
        let me = RemoteMe { name: "Phone".into(), kind: DeviceKind::Phone };
        let phone = Remote::new(client, me, Arc::new(Obeys), Arc::new(News(Mutex::new(news_to))), None);
        phone.clone().watch(true);
        phone.clone().lan_found(door.name.clone(), "127.0.0.1".into(), door.port, door.txt.clone());
        let until = |what: &str, ready: &dyn Fn(&Remote) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !ready(&phone) {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(news.recv_timeout(left).is_ok(), "waited for {what}");
            }
        };
        until("the iPod listed", &|r| r.devices().iter().any(|d| d.id == ipod && d.name == "iPod touch"));
        phone.clone().pick(Some(ipod.clone()));
        until("the iPod mirrored", &|r| r.active().is_some());
        phone.clone().send(ipod.clone(), Op::Replace { songs, index: 0, position_ms: 0, play: true, order: None, shuffle: false, repeat: 0 });

        let heard_ms = || sim.heard(&render, HostClock.now_us()) as i64 * 1_000 / 44_100;
        until("three seconds heard", &|r| r.active().is_some_and(|m| m.playing) && heard_ms() > 3_000);
        for _ in 0..5 {
            let _ = news.recv_timeout(Duration::from_millis(300));
            let m = phone.active().expect("still mirrored");
            let (shown, heard) = (m.position_now(), heard_ms());
            assert!((shown - heard).abs() <= 20, "the phone shows {shown} ms, {heard} ms heard on the iPod");
        }

        // The volume: set from the phone, the app is told to set the system's; moved by the iPod's own
        // buttons, the phone sees it.
        let volume = |r: &Remote| r.devices().into_iter().find(|d| d.id == ipod).and_then(|d| d.state?.volume);
        let at = mark();
        phone.clone().send(ipod.clone(), Op::Volume { percent: 30 });
        expect(at, REPORT_VOLUME, |r| r.ms == 300);
        super::nori_ios_volume(0.55);
        until("the iPod's own volume", &|r| volume(r) == Some(55));
        // The iPod first: its door closing answers the phone's held poll.
        super::close();
        phone.clone().stop();
        stop.store(true, Ordering::Relaxed);
        unit.join().unwrap();
    }
}
