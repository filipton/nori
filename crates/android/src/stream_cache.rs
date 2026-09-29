//! `nori_core::stream_cache` (eviction order of media3's stream cache) for Kotlin.

use jni::objects::{JClass, JObject, JObjectArray, JString};
use jni::sys::{jobjectArray, jstring};
use jni::JNIEnv;
use nori_core::stream_cache;

use crate::{java_string, native, with_str, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/StreamCacheJni",
    methods: &[
        native!(c"touch", c"(Ljava/lang/String;)V", touch),
        native!(c"seed", c"([Ljava/lang/String;)V", seed),
        native!(c"next", c"()Ljava/lang/String;", next),
        native!(c"copies", c"(Ljava/lang/String;)[Ljava/lang/String;", copies),
        native!(c"clear", c"()V", clear),
    ],
};

/// A cache span was read or written. Allocation-free for a known key.
extern "system" fn touch(env: JNIEnv, _: JClass, key: JString) {
    with_str(&env, &key, stream_cache::touch);
}

/// The keys in the cache at startup. Called once.
extern "system" fn seed(mut env: JNIEnv, _: JClass, keys: JObjectArray) {
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
    stream_cache::seed(held.iter().map(String::as_str));
}

/// The next key to evict, or null.
extern "system" fn next(env: JNIEnv, _: JClass) -> jstring {
    stream_cache::next().map_or(std::ptr::null_mut(), |k| java_string(&env, &k))
}

/// Cache keys of song `id`'s streamed copies.
extern "system" fn copies(mut env: JNIEnv, _: JClass, id: JString) -> jobjectArray {
    let keys = with_str(&env, &id, stream_cache::copies).unwrap_or_default();
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

extern "system" fn clear() {
    stream_cache::clear();
}
