//! Values a door leaves in memory Kotlin reads with no call: a direct ByteBuffer Kotlin allocates and
//! keeps (so the memory outlives every write), whose address a per-frame door takes and writes into.

use jni::objects::{JByteBuffer, JClass};
use jni::sys::jlong;
use jni::JNIEnv;

use crate::{native, Class};

pub(crate) static CLASS: Class = Class { name: c"dev/nori/music/NativeViewJni", methods: &[native!(c"address", c"(Ljava/nio/ByteBuffer;)J", address)] };

/// The address of direct buffer `buffer`; 0 if it is not one.
extern "system" fn address(env: JNIEnv, _: JClass, buffer: JByteBuffer) -> jlong {
    env.get_direct_buffer_address(&buffer).map_or(0, |p| p as jlong)
}

/// Writes `value` at `at`, an [`address`] of a buffer at least `size_of::<T>()` long; nothing at 0.
///
/// # Safety
/// `at` is 0 or the address of a live direct buffer that long, which nothing else writes meanwhile.
pub(crate) unsafe fn put<T>(at: jlong, value: T) {
    if at != 0 {
        // SAFETY: as the caller promises; unaligned, as a ByteBuffer's memory may be.
        unsafe { (at as *mut T).write_unaligned(value) }
    }
}
