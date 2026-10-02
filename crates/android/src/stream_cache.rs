//! `nori_core::stream_cache` (eviction order of media3's stream cache) for Kotlin, whose evictor holds the
//! order as a handle.

use jni::objects::{JClass, JObject, JObjectArray, JString};
use jni::sys::{jlong, jobjectArray, jstring};
use jni::JNIEnv;
use nori_core::stream_cache::CacheOrder;
use parking_lot::Mutex;

use crate::{java_string, native, with_str, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/StreamCacheJni",
    methods: &[
        native!(c"create", c"()J", create),
        native!(c"touch", c"(JLjava/lang/String;)V", touch),
        native!(c"seed", c"(J[Ljava/lang/String;)V", seed),
        native!(c"next", c"(J)Ljava/lang/String;", next),
        native!(c"copies", c"(JLjava/lang/String;)[Ljava/lang/String;", copies),
        native!(c"clear", c"(J)V", clear),
    ],
};

/// A new, empty order; it lives as long as the cache whose evictor holds it.
extern "system" fn create(_: JNIEnv, _: JClass) -> jlong {
    Box::into_raw(Box::new(Mutex::new(CacheOrder::default()))) as jlong
}

fn order<'a>(h: jlong) -> &'a Mutex<CacheOrder> {
    // SAFETY: `h` came from `create` and is never freed.
    unsafe { &*(h as *const Mutex<CacheOrder>) }
}

/// A cache span was read or written. Allocation-free for a known key.
extern "system" fn touch(env: JNIEnv, _: JClass, h: jlong, key: JString) {
    with_str(&env, &key, |k| order(h).lock().touch(k));
}

/// The keys in the cache at startup. Called once.
extern "system" fn seed(mut env: JNIEnv, _: JClass, h: jlong, keys: JObjectArray) {
    let n = env.get_array_length(&keys).unwrap_or(0);
    let mut held = Vec::with_capacity(n.max(0) as usize);
    for i in 0..n {
        let Ok(o) = env.get_object_array_element(&keys, i) else { return };
        let s = JString::from(o);
        let Some(v) = crate::string(&env, &s) else { return };
        held.push(v);
        // Thousands of keys would overflow the local reference table.
        let _ = env.delete_local_ref(s);
    }
    order(h).lock().seed(held.iter().map(String::as_str));
}

/// The next key to evict, or null.
extern "system" fn next(env: JNIEnv, _: JClass, h: jlong) -> jstring {
    let key = order(h).lock().pop_oldest();
    key.map_or(std::ptr::null_mut(), |k| java_string(&env, &k))
}

/// Cache keys of song `id`'s streamed copies.
extern "system" fn copies(mut env: JNIEnv, _: JClass, h: jlong, id: JString) -> jobjectArray {
    let keys = with_str(&env, &id, |id| order(h).lock().copies(id)).unwrap_or_default();
    let Ok(out) = env.new_object_array(keys.len() as i32, "java/lang/String", JObject::null()) else { return std::ptr::null_mut() };
    for (i, k) in keys.iter().enumerate() {
        let Ok(s) = env.new_string(k) else { return std::ptr::null_mut() };
        if env.set_object_array_element(&out, i as i32, &s).is_err() {
            return std::ptr::null_mut();
        }
        let _ = env.delete_local_ref(s);
    }
    out.into_raw()
}

extern "system" fn clear(h: jlong) {
    order(h).lock().clear();
}
