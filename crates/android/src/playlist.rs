//! `nori_core::playlist` queries made on every player event, primitives only.

use jni::objects::{JClass, JIntArray};
use jni::sys::{jboolean, jint, jlong};
use jni::JNIEnv;
use nori_core::playlist;

use crate::{native, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/PlaylistJni",
    methods: &[
        native!(c"rev", c"()J", rev),
        native!(c"origin", c"()I", origin),
        native!(c"shuffleShown", c"()Z", shuffle_shown),
        native!(c"order", c"([I)I", order),
    ],
};

extern "system" fn shuffle_shown() -> jboolean {
    playlist::playlist_shuffle_shown() as jboolean
}

/// Writes the shuffle order into `out` if it has exactly that length. Returns the order's length; -1
/// when not shuffling.
extern "system" fn order(env: JNIEnv, _: JClass, out: JIntArray) -> jint {
    let Ok(cap) = env.get_array_length(&out) else { return -1 };
    playlist::playlist_shuffle_order(|o| {
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
extern "system" fn rev() -> jlong {
    playlist::playlist_rev() as jlong
}

/// Changes whenever a new queue is set (`playlist_origin_gen`).
extern "system" fn origin() -> jint {
    playlist::playlist_origin_gen() as jint
}
