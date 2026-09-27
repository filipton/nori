//! AutoMix's measuring ahead on Android, for both players: nori-engine's `Measurer` (the core picks the
//! songs, each is decoded once, whole, on a thread of the lowest priority) over media3's caches. Kotlin
//! only says where a song's bytes are (`MeasureBridge.whole`: the files of a download, or of a copy in
//! the stream cache, once every byte is there) and when one has become whole (`arrived`, from the
//! caches' own callbacks); the files are then read straight, half a megabyte at a time.
//!
//! This replaced a Kotlin measurer over MediaExtractor, which read each packet through the platform's
//! extractor and each of the extractor's reads back through media3's data sources: a small blocking
//! step per packet, so the thread woke tens of times a second for as long as a song took, and it was
//! started again on every loading burst.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jni::objects::{GlobalRef, JByteArray, JClass, JObjectArray, JStaticMethodID, JString, JValue};
use jni::sys::{jboolean, jint, jlong};
use jni::signature::{Primitive, ReturnType};
use jni::{JNIEnv, JavaVM};
use nori_engine::core::{key_format, Measurer, Shelf, Whole};
use parking_lot::Mutex;

use crate::{cleared, native, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/MeasureJni",
    methods: &[
        native!(c"start", c"()V", start),
        native!(c"update", c"()V", update),
        native!(c"arrived", c"()V", arrived),
        native!(c"stop", c"()V", stop),
        native!(c"downloadOpen", c"(Ljava/lang/String;)J", download_open),
        native!(c"downloadTake", c"(J[BI)V", download_take),
        native!(c"downloadEnd", c"(JZ)V", download_end),
        native!(c"processStart", c"()V", process_start),
        native!(c"processSaved", c"([Ljava/lang/String;)V", process_saved),
        native!(c"processAnalyse", c"([Ljava/lang/String;)I", process_analyse),
    ],
};

/// `MeasureBridge`, looked up once.
struct Java {
    vm: JavaVM,
    bridge: GlobalRef,
    whole: JStaticMethodID,
    measured: JStaticMethodID,
}

static JAVA: OnceLock<Java> = OnceLock::new();

/// The measurer while the playback service runs.
static MEASURER: Mutex<Option<Arc<Measurer>>> = Mutex::new(None);

fn look_up(env: &mut JNIEnv) -> jni::errors::Result<Java> {
    let bridge = env.find_class("dev/nori/music/playback/MeasureBridge")?;
    Ok(Java {
        vm: env.get_java_vm()?,
        whole: env.get_static_method_id(&bridge, "whole", "(Ljava/lang/String;)[Ljava/lang/String;")?,
        measured: env.get_static_method_id(&bridge, "measured", "()V")?,
        bridge: env.new_global_ref(&bridge)?,
    })
}

/// The measuring thread's JNIEnv, attached under its own name.
fn env() -> Option<(&'static Java, JNIEnv<'static>)> {
    let java = JAVA.get()?;
    Some((java, crate::attached(&java.vm)?))
}

/// Where media3 keeps a song: asked of Kotlin, which knows its caches.
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
        // The cache key first, then the files. A download may have been transcoded, and its key says
        // nothing: the file says what it is. A stream's key names its format.
        let key = parts.remove(0);
        let hint = if key == nori_core::stream::download_key(id.to_string()) {
            None
        } else {
            key_format(&key).or_else(|| nori_core::queue::queue_song(id.to_string()).map(|s| s.suffix)).filter(|s| !s.is_empty())
        };
        Some(Whole { files: parts.into_iter().map(PathBuf::from).collect(), hint })
    }
}

/// A song was measured: the transitions around it are planned again (Kotlin's `MeasureBridge.measured`).
fn told() {
    if let Some((java, mut env)) = env() {
        let bridge = <&JClass>::from(java.bridge.as_obj());
        // SAFETY: MeasureBridge.measured(), looked up with this signature.
        let _ = unsafe { env.call_static_method_unchecked(bridge, java.measured, ReturnType::Primitive(Primitive::Void), &[]) };
        cleared(&mut env);
    }
}

fn measurer() -> Option<Arc<Measurer>> {
    MEASURER.lock().clone()
}

/// `MeasureBridge`, looked up the first time a door needs it; false when it is missing.
fn java(env: &mut JNIEnv) -> bool {
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

/// The playback service started: the measurer exists from now on, idle until it is asked for songs.
extern "system" fn start(mut env: JNIEnv, _: JClass) {
    if !java(&mut env) {
        return;
    }
    let mut m = MEASURER.lock();
    if m.is_none() {
        *m = Some(Measurer::on_shelf(nori_core::active, Box::new(Media3), Some(Box::new(told))));
    }
}

/// The songs coming up may have changed: the core names them (`queue_measure`, none while AutoMix is
/// off), and the same songs as before change nothing.
extern "system" fn update() {
    if let Some(m) = measurer() {
        m.ask(nori_core::rules::queue_measure());
    }
}

/// A song has become whole in one of the caches.
extern "system" fn arrived() {
    if let Some(m) = measurer() {
        m.arrived();
    }
}

/// The playback service stops: a song being measured is left, and the measurer goes.
extern "system" fn stop() {
    if let Some(m) = MEASURER.lock().take() {
        m.ask(Vec::new());
    }
}

// ---- a download measured as it comes ----

/// What hears a download's bytes as media3 writes them (Kotlin's `MeasuringSink`), from the first: a song
/// downloaded for offline listening is measured as it downloads, whatever AutoMix says (nori-engine's
/// `measure_download_as_it_comes`), so neither a later mix nor the lyrics' sync needs a pass of its own.
/// A handle, 0 when nothing is measured (the song measured already, an MP4, not a download's key).
extern "system" fn download_open(env: JNIEnv, _: JClass, key: JString) -> jlong {
    let Some(key) = crate::string(&env, &key) else { return 0 };
    let Some(id) = key.strip_prefix("dl:") else { return 0 };
    let hint = nori_core::queue::queue_song(id.to_string()).map(|s| s.suffix).filter(|s| !s.is_empty());
    match nori_engine::core::measure_download_as_it_comes(id, hint.as_deref()) {
        Some(t) => Box::into_raw(Box::new(t)) as jlong,
        None => 0,
    }
}

/// The next `len` bytes of the download, from Kotlin's buffer: a quarter megabyte at a time.
extern "system" fn download_take(mut env: JNIEnv, _: JClass, h: jlong, bytes: JByteArray, len: jint) {
    if h == 0 || len <= 0 {
        return;
    }
    // SAFETY: a handle download_open made and download_end has not taken back.
    let taker = unsafe { &mut *(h as *mut nori_engine::arriving::Listening) };
    let mut buf = std::mem::take(&mut *BUF.lock());
    buf.resize(len as usize, 0);
    // SAFETY: i8 and u8 have the same size and alignment.
    let into = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut i8, buf.len()) };
    if env.get_byte_array_region(&bytes, 0, into).is_ok() {
        taker.take(&buf);
    } else {
        cleared(&mut env);
    }
    *BUF.lock() = buf;
}

/// The download ended: `whole` when every byte came and was kept. The handle is gone after this.
extern "system" fn download_end(_: JNIEnv, _: JClass, h: jlong, whole: jboolean) {
    if h == 0 {
        return;
    }
    // SAFETY: a handle download_open made, taken back once.
    let taker = unsafe { Box::from_raw(h as *mut nori_engine::arriving::Listening) };
    taker.end(whole != 0);
}

/// The copy of a download's bytes handed over, kept between calls: nothing is allocated per piece.
static BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());

// ---- the work after a download's bytes (nori-engine's `processing`) ----

/// The downloads can be read back from media3's download cache (`MeasureBridge.whole`, which answers for a
/// download whether or not the playback service runs).
extern "system" fn process_start(mut env: JNIEnv, _: JClass) {
    if java(&mut env) {
        nori_engine::processing::install(Box::new(Media3));
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

/// Downloads just saved and settled: what each needs besides its lyrics is decided and marked, and those to read
/// back from the disk join the line. Off the main thread.
extern "system" fn process_saved(mut env: JNIEnv, _: JClass, array: JObjectArray) {
    let ids = ids(&mut env, &array);
    nori_engine::processing::saved(ids);
}

/// Downloads asked for again ("Analyse downloaded songs", `Core::download_unanalysed`): how many joined the line.
extern "system" fn process_analyse(mut env: JNIEnv, _: JClass, array: JObjectArray) -> jint {
    let ids = ids(&mut env, &array);
    nori_engine::processing::analyse(ids) as jint
}
