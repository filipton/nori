//! The app's queue's queries made on every player event, primitives only. `s` is the queue session's
//! handle (`crate::kept`).

use jni::objects::{JClass, JIntArray};
use jni::sys::{jboolean, jint, jlong};
use jni::JNIEnv;
use nori_core::queue::Session;

use crate::{kept, native, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/PlaylistJni",
    methods: &[
        native!(c"rev", c"(J)J", rev),
        native!(c"origin", c"(J)I", origin),
        native!(c"shuffleShown", c"(J)Z", shuffle_shown),
        native!(c"order", c"(J[I)I", order),
    ],
};

extern "system" fn shuffle_shown(s: jlong) -> jboolean {
    kept::<Session>(s).shuffle_shown() as jboolean
}

/// Writes the shuffle order into `out` if it has exactly that length. Returns the order's length; -1
/// when not shuffling.
extern "system" fn order(env: JNIEnv, _: JClass, s: jlong, out: JIntArray) -> jint {
    let Ok(cap) = env.get_array_length(&out) else { return -1 };
    kept::<Session>(s).shuffle_order(|o| {
        let Some(o) = o else { return -1 };
        if o.len() == cap as usize {
            // Copied through a stack buffer: no allocation.
            let mut page = [0 as jint; 256];
            for (k, chunk) in o.chunks(page.len()).enumerate() {
                for (d, &i) in page.iter_mut().zip(chunk) {
                    *d = i as jint;
                }
                if env.set_int_array_region(&out, (k * 256) as jint, &page[..chunk.len()]).is_err() {
                    return -1;
                }
            }
        }
        o.len() as jint
    })
}

/// Queue revision: unchanged means no need to copy the queue again.
extern "system" fn rev(s: jlong) -> jlong {
    kept::<Session>(s).rev() as jlong
}

/// Changes whenever a new queue is set (`Session::origin_gen`).
extern "system" fn origin(s: jlong) -> jint {
    kept::<Session>(s).origin_gen() as jint
}
