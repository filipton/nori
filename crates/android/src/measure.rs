//! AutoMix analysis ahead of playback: nori-engine's `Measurer` over media3's caches. Kotlin reports
//! where a complete song's files are (`MeasureBridge.whole`, the bridge each shelf is handed) and when one
//! completes (`arrived`); the files are read directly. Also measures downloads as they are written, and
//! post-download processing.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jni::objects::{GlobalRef, JByteArray, JClass, JMethodID, JObject, JObjectArray, JString, JValue};
use jni::sys::{jboolean, jint, jlong};
use jni::signature::{Primitive, ReturnType};
use jni::{JNIEnv, JavaVM};
use nori_core::client::CurrentClient;
use nori_core::queue::Session;
use nori_engine::core::{key_format, Analyses, Measurer, Shelf, Whole};

use crate::{cleared, native, Class, Handles};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/MeasureJni",
    methods: &[
        native!(c"analyses", c"(JJ)J", create_analyses),
        native!(c"start", c"(Ldev/nori/music/playback/MeasureBridge;J)J", start),
        native!(c"update", c"(J)V", update),
        native!(c"arrived", c"(J)V", arrived),
        native!(c"stop", c"(J)V", stop),
        native!(c"downloadOpen", c"(JLjava/lang/String;)J", download_open),
        native!(c"downloadTake", c"(J[BI)V", download_take),
        native!(c"downloadEnd", c"(JZ)V", download_end),
        native!(c"processStart", c"(Ldev/nori/music/playback/MeasureBridge;J)V", process_start),
        native!(c"processSaved", c"(J[Ljava/lang/String;)V", process_saved),
        native!(c"processAnalyse", c"(J[Ljava/lang/String;)I", process_analyse),
    ],
};

/// `MeasureBridge`'s methods, looked up once.
struct Java {
    vm: JavaVM,
    whole: JMethodID,
    measured: JMethodID,
}

/// Global: `MeasureBridge`'s methods, looked up once for every caller.
static JAVA: OnceLock<Java> = OnceLock::new();

/// AutoMix's analyses over the client in use, and the app's queue session the songs come from.
pub(crate) struct Measuring {
    pub(crate) analyses: Arc<Analyses>,
    pub(crate) session: Arc<Session>,
}

/// What [`create_analyses`] made (held by Kotlin for the process's life); None for 0.
pub(crate) fn measuring<'a>(h: jlong) -> Option<&'a Measuring> {
    // SAFETY: a non-zero `h` came from `create_analyses` and is never freed.
    (h != 0).then(|| unsafe { &*(h as *const Measuring) })
}

/// AutoMix's analyses over `current`'s client (a `CurrentClient.uniffiCloneHandle()`, taken over) and the
/// queue session `session` (`crate::kept`), for the player, the measurer and the downloads; Kotlin holds
/// the handle for the process's life.
extern "system" fn create_analyses(_: JNIEnv, _: JClass, current: jlong, session: jlong) -> jlong {
    // SAFETY: Kotlin passes `CurrentClient.uniffiCloneHandle()`, once.
    let current: Arc<CurrentClient> = unsafe { crate::uniffi_object(current) };
    Box::into_raw(Box::new(Measuring { analyses: Analyses::new(move || current.get()), session: crate::kept(session) })) as jlong
}

fn look_up(env: &mut JNIEnv, bridge: &JObject) -> jni::errors::Result<Java> {
    let class = env.get_object_class(bridge)?;
    Ok(Java {
        vm: env.get_java_vm()?,
        whole: env.get_method_id(&class, "whole", "(Ljava/lang/String;)[Ljava/lang/String;")?,
        measured: env.get_method_id(&class, "measured", "()V")?,
    })
}

/// This thread's JNIEnv, attached for life.
fn env() -> Option<(&'static Java, JNIEnv<'static>)> {
    let java = JAVA.get()?;
    Some((java, crate::attached(&java.vm)?))
}

/// Song locations in media3's caches, asked of the shelf's own `MeasureBridge`; the queue `session` knows
/// the songs' formats.
struct Media3 {
    session: Arc<Session>,
    bridge: GlobalRef,
}

impl Shelf for Media3 {
    fn whole(&self, id: &str) -> Option<Whole> {
        let (java, mut env) = env()?;
        let parts = env.with_local_frame(8, |env| -> jni::errors::Result<Option<Vec<String>>> {
            let id = env.new_string(id)?;
            // SAFETY: MeasureBridge.whole(String): String[], looked up with this signature.
            let found = unsafe { env.call_method_unchecked(self.bridge.as_obj(), java.whole, ReturnType::Object, &[JValue::Object(&id).as_jni()]) }?.l()?;
            if found.is_null() {
                return Ok(None);
            }
            let found = JObjectArray::from(found);
            let n = env.get_array_length(&found)?;
            let mut parts = Vec::with_capacity(n as usize);
            for i in 0..n {
                let s = JString::from(env.get_object_array_element(&found, i)?);
                parts.push(String::from(env.get_string(&s)?));
                env.delete_local_ref(s)?;
            }
            Ok(Some(parts))
        });
        cleared(&mut env);
        let mut parts = parts.ok().flatten()?;
        if parts.len() < 2 {
            return None;
        }
        // The cache key, then the files. A stream's key names its format; a download's does not (it may
        // be transcoded), so its file is sniffed.
        let key = parts.remove(0);
        let hint = if key == nori_core::stream::download_key(id.to_string()) {
            None
        } else {
            key_format(&key).or_else(|| self.session.song(id).map(|s| s.suffix)).filter(|s| !s.is_empty())
        };
        Some(Whole { files: parts.into_iter().map(PathBuf::from).collect(), hint })
    }
}

/// `bridge.measured()`: a song was measured, so transitions are replanned.
fn notify_measured(bridge: &GlobalRef) {
    if let Some((java, mut env)) = env() {
        // SAFETY: MeasureBridge.measured(), looked up with this signature.
        let _ = unsafe { env.call_method_unchecked(bridge.as_obj(), java.measured, ReturnType::Primitive(Primitive::Void), &[]) };
        cleared(&mut env);
    }
}

/// A measurer while the playback service runs, and the queue session whose songs it measures.
struct Running {
    measurer: Arc<Measurer>,
    session: Arc<Session>,
}

/// Measurers by Kotlin handle: a cache writer's `arrived` racing `stop` finds nothing.
static MEASURERS: Handles<Running> = Handles::new();

/// `bridge` as a shelf's own, `MeasureBridge`'s methods looked up on first use; None when they are missing.
fn bridge_of(env: &mut JNIEnv, bridge: &JObject) -> Option<GlobalRef> {
    let held = env.new_global_ref(bridge).ok();
    held.filter(|_| ensure_java(env, bridge))
}

fn ensure_java(env: &mut JNIEnv, bridge: &JObject) -> bool {
    if JAVA.get().is_some() {
        return true;
    }
    match look_up(env, bridge) {
        Ok(j) => {
            let _ = JAVA.set(j);
            true
        }
        Err(e) => {
            cleared(env);
            nori_core::alog::info(&format!("measuring: the Java side is missing: {e}"));
            false
        }
    }
}

/// Playback service started: the (idle) measurer over `analyses`, asking `bridge` (its `AutoMixPrefetch`'s),
/// as a handle [`stop`] takes back; 0 when the Java side is missing.
extern "system" fn start(mut env: JNIEnv, _: JClass, bridge: JObject, analyses: jlong) -> jlong {
    let Some(m) = measuring(analyses) else { return 0 };
    let Some(bridge) = bridge_of(&mut env, &bridge) else { return 0 };
    let told = bridge.clone();
    let measurer = Measurer::on_shelf(m.analyses.clone(), Box::new(Media3 { session: m.session.clone(), bridge }), Some(Box::new(move || notify_measured(&told))));
    MEASURERS.add(Arc::new(Running { measurer, session: m.session.clone() }))
}

/// The upcoming songs may have changed: asks for the queue's `measure` (empty with AutoMix off).
extern "system" fn update(h: jlong) {
    if let Some(r) = MEASURERS.get(h) {
        r.measurer.ask(r.session.measure());
    }
}

/// A song became complete in one of the caches.
extern "system" fn arrived(h: jlong) {
    if let Some(r) = MEASURERS.get(h) {
        r.measurer.arrived();
    }
}

/// Playback service stopped: abandons the current song and drops the measurer.
extern "system" fn stop(h: jlong) {
    if let Some(r) = MEASURERS.remove(h) {
        r.measurer.ask(Vec::new());
    }
}

// ---- downloads measured as they are written ----

/// Starts measuring a download from its bytes as media3 writes them (Kotlin's `MeasuringSink`;
/// `measure_download_as_it_comes`). Returns a handle, 0 when nothing is to be measured.
extern "system" fn download_open(env: JNIEnv, _: JClass, analyses: jlong, key: JString) -> jlong {
    let Some(m) = measuring(analyses) else { return 0 };
    let Some(key) = crate::string(&env, &key) else { return 0 };
    let Some(id) = key.strip_prefix("dl:") else { return 0 };
    let hint = m.session.song(id).map(|s| s.suffix).filter(|s| !s.is_empty());
    match m.analyses.measure_download_as_it_comes(id, hint.as_deref()) {
        Some(listening) => Box::into_raw(Box::new(Taking { listening, buf: Vec::new() })) as jlong,
        None => 0,
    }
}

/// One download being measured; `buf` is reused across calls.
struct Taking {
    listening: nori_engine::arriving::Listening,
    buf: Vec<u8>,
}

/// The download's next `len` bytes.
extern "system" fn download_take(mut env: JNIEnv, _: JClass, h: jlong, bytes: JByteArray, len: jint) {
    if h == 0 || len <= 0 {
        return;
    }
    // SAFETY: a handle download_open made and download_end has not taken back.
    let taking = unsafe { &mut *(h as *mut Taking) };
    taking.buf.resize(len as usize, 0);
    // SAFETY: i8 and u8 have the same size and alignment.
    let into = unsafe { std::slice::from_raw_parts_mut(taking.buf.as_mut_ptr() as *mut i8, taking.buf.len()) };
    if env.get_byte_array_region(&bytes, 0, into).is_ok() {
        taking.listening.take(&taking.buf);
    } else {
        cleared(&mut env);
    }
}

/// The download ended (`whole`: complete and kept). Frees the handle.
extern "system" fn download_end(_: JNIEnv, _: JClass, h: jlong, whole: jboolean) {
    if h == 0 {
        return;
    }
    // SAFETY: a handle download_open made, taken back once.
    let taking = unsafe { Box::from_raw(h as *mut Taking) };
    taking.listening.end(whole != 0);
}

// ---- post-download processing (nori-engine's `processing`) ----

/// Installs the download cache reader for processing, asking `bridge` (works without the playback service).
extern "system" fn process_start(mut env: JNIEnv, _: JClass, bridge: JObject, analyses: jlong) {
    let Some(m) = measuring(analyses) else { return };
    if let Some(bridge) = bridge_of(&mut env, &bridge) {
        m.analyses.install(Box::new(Media3 { session: m.session.clone(), bridge }));
    }
}

fn ids(env: &mut JNIEnv, array: &JObjectArray) -> Vec<String> {
    let n = env.get_array_length(array).unwrap_or(0);
    let mut out = Vec::with_capacity(n.max(0) as usize);
    for i in 0..n {
        let Ok(o) = env.get_object_array_element(array, i) else { break };
        let s = JString::from(o);
        if let Some(id) = crate::string(env, &s) {
            out.push(id);
        }
        let _ = env.delete_local_ref(s);
    }
    cleared(env);
    out
}

/// Newly saved downloads: queues the processing each needs. Off the main thread.
extern "system" fn process_saved(mut env: JNIEnv, _: JClass, analyses: jlong, array: JObjectArray) {
    let ids = ids(&mut env, &array);
    if let Some(m) = measuring(analyses) {
        m.analyses.saved(ids);
    }
}

/// "Analyse downloaded songs": queues `ids`, returns how many were queued.
extern "system" fn process_analyse(mut env: JNIEnv, _: JClass, analyses: jlong, array: JObjectArray) -> jint {
    let ids = ids(&mut env, &array);
    measuring(analyses).map_or(0, |m| m.analyses.analyse(ids) as jint)
}
