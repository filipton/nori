//! AutoMix analysis ahead of playback: nori-engine's `Measurer` over media3's caches. Kotlin reports
//! where a complete song's files are (`MeasureBridge.whole`) and when one completes (`arrived`); the
//! files are read directly. Also measures downloads as they are written, and post-download processing.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jni::objects::{GlobalRef, JByteArray, JClass, JObjectArray, JStaticMethodID, JString, JValue};
use jni::sys::{jboolean, jint, jlong};
use jni::signature::{Primitive, ReturnType};
use jni::{JNIEnv, JavaVM};
use nori_core::client::CurrentClient;
use nori_engine::core::{key_format, Analyses, Measurer, Shelf, Whole};

use crate::{cleared, native, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/MeasureJni",
    methods: &[
        native!(c"analyses", c"(J)J", create_analyses),
        native!(c"start", c"(J)J", start),
        native!(c"update", c"(J)V", update),
        native!(c"arrived", c"(J)V", arrived),
        native!(c"stop", c"(J)V", stop),
        native!(c"downloadOpen", c"(JLjava/lang/String;)J", download_open),
        native!(c"downloadTake", c"(J[BI)V", download_take),
        native!(c"downloadEnd", c"(JZ)V", download_end),
        native!(c"processStart", c"(J)V", process_start),
        native!(c"processSaved", c"(J[Ljava/lang/String;)V", process_saved),
        native!(c"processAnalyse", c"(J[Ljava/lang/String;)I", process_analyse),
    ],
};

/// `MeasureBridge`, looked up once.
struct Java {
    vm: JavaVM,
    bridge: GlobalRef,
    whole: JStaticMethodID,
    measured: JStaticMethodID,
}

/// Global: `MeasureBridge`'s classes and methods, looked up once for every caller.
static JAVA: OnceLock<Java> = OnceLock::new();

/// The analyses behind `h` (from [`create_analyses`], held by Kotlin for the process's life); None for 0.
pub(crate) fn analyses(h: jlong) -> Option<Arc<Analyses>> {
    // SAFETY: a non-zero `h` came from `create_analyses` and is never freed.
    (h != 0).then(|| unsafe {
        Arc::increment_strong_count(h as *const Analyses);
        Arc::from_raw(h as *const Analyses)
    })
}

/// AutoMix's analyses over `current`'s client (a `CurrentClient.uniffiCloneHandle()`, taken over), for
/// the player, the measurer and the downloads; Kotlin holds the handle for the process's life.
extern "system" fn create_analyses(_: JNIEnv, _: JClass, current: jlong) -> jlong {
    // SAFETY: Kotlin passes `CurrentClient.uniffiCloneHandle()`, once.
    let current: Arc<CurrentClient> = unsafe { crate::uniffi_object(current) };
    Arc::into_raw(Analyses::new(move || current.get())) as jlong
}

fn look_up(env: &mut JNIEnv) -> jni::errors::Result<Java> {
    let bridge = env.find_class("dev/nori/music/playback/MeasureBridge")?;
    Ok(Java {
        vm: env.get_java_vm()?,
        whole: env.get_static_method_id(&bridge, "whole", "(Ljava/lang/String;)[Ljava/lang/String;")?,
        measured: env.get_static_method_id(&bridge, "measured", "()V")?,
        bridge: env.new_global_ref(&bridge)?,
    })
}

/// This thread's JNIEnv, attached for life.
fn env() -> Option<(&'static Java, JNIEnv<'static>)> {
    let java = JAVA.get()?;
    Some((java, crate::attached(&java.vm)?))
}

/// Song locations in media3's caches, asked of Kotlin.
struct Media3;

impl Shelf for Media3 {
    fn whole(&self, id: &str) -> Option<Whole> {
        let (java, mut env) = env()?;
        let parts = env.with_local_frame(8, |env| -> jni::errors::Result<Option<Vec<String>>> {
            let id = env.new_string(id)?;
            let bridge = <&JClass>::from(java.bridge.as_obj());
            // SAFETY: MeasureBridge.whole(String): String[], looked up with this signature.
            let found = unsafe { env.call_static_method_unchecked(bridge, java.whole, ReturnType::Object, &[JValue::Object(&id).as_jni()]) }?.l()?;
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
            key_format(&key).or_else(|| nori_core::queue::shared().song(id).map(|s| s.suffix)).filter(|s| !s.is_empty())
        };
        Some(Whole { files: parts.into_iter().map(PathBuf::from).collect(), hint })
    }
}

/// `MeasureBridge.measured()`: a song was measured, so transitions are replanned.
fn notify_measured() {
    if let Some((java, mut env)) = env() {
        let bridge = <&JClass>::from(java.bridge.as_obj());
        // SAFETY: MeasureBridge.measured(), looked up with this signature.
        let _ = unsafe { env.call_static_method_unchecked(bridge, java.measured, ReturnType::Primitive(Primitive::Void), &[]) };
        cleared(&mut env);
    }
}

/// The measurer behind `h` (from [`start`], until [`stop`]).
fn measurer<'a>(h: jlong) -> Option<&'a Arc<Measurer>> {
    // SAFETY: a non-zero `h` came from `start` and `stop` has not taken it back.
    (h != 0).then(|| unsafe { &*(h as *const Arc<Measurer>) })
}

/// Looks `MeasureBridge` up on first use; false when it is missing.
fn ensure_java(env: &mut JNIEnv) -> bool {
    if JAVA.get().is_some() {
        return true;
    }
    match look_up(env) {
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

/// Playback service started: the (idle) measurer over `analyses`, as a handle [`stop`] takes back; 0 when
/// the Java side is missing.
extern "system" fn start(mut env: JNIEnv, _: JClass, analyses: jlong) -> jlong {
    let Some(analyses) = self::analyses(analyses) else { return 0 };
    if !ensure_java(&mut env) {
        return 0;
    }
    Box::into_raw(Box::new(Measurer::on_shelf(analyses, Box::new(Media3), Some(Box::new(notify_measured))))) as jlong
}

/// The upcoming songs may have changed: asks for the queue's `measure` (empty with AutoMix off).
extern "system" fn update(h: jlong) {
    if let Some(m) = measurer(h) {
        m.ask(nori_core::queue::shared().measure());
    }
}

/// A song became complete in one of the caches.
extern "system" fn arrived(h: jlong) {
    if let Some(m) = measurer(h) {
        m.arrived();
    }
}

/// Playback service stopped: abandons the current song and drops the measurer.
extern "system" fn stop(h: jlong) {
    if h != 0 {
        // SAFETY: `h` came from `start`; Kotlin stops it once.
        let m = unsafe { Box::from_raw(h as *mut Arc<Measurer>) };
        m.ask(Vec::new());
    }
}

// ---- downloads measured as they are written ----

/// Starts measuring a download from its bytes as media3 writes them (Kotlin's `MeasuringSink`;
/// `measure_download_as_it_comes`). Returns a handle, 0 when nothing is to be measured.
extern "system" fn download_open(env: JNIEnv, _: JClass, analyses: jlong, key: JString) -> jlong {
    let Some(analyses) = self::analyses(analyses) else { return 0 };
    let Some(key) = crate::string(&env, &key) else { return 0 };
    let Some(id) = key.strip_prefix("dl:") else { return 0 };
    let hint = nori_core::queue::shared().song(id).map(|s| s.suffix).filter(|s| !s.is_empty());
    match analyses.measure_download_as_it_comes(id, hint.as_deref()) {
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

/// Installs the download cache reader for processing (works without the playback service).
extern "system" fn process_start(mut env: JNIEnv, _: JClass, analyses: jlong) {
    if let Some(a) = self::analyses(analyses).filter(|_| ensure_java(&mut env)) {
        a.install(Box::new(Media3));
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
    if let Some(a) = self::analyses(analyses) {
        a.saved(ids);
    }
}

/// "Analyse downloaded songs": queues `ids`, returns how many were queued.
extern "system" fn process_analyse(mut env: JNIEnv, _: JClass, analyses: jlong, array: JObjectArray) -> jint {
    let ids = ids(&mut env, &array);
    self::analyses(analyses).map_or(0, |a| a.analyse(ids) as jint)
}
