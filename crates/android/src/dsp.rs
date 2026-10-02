//! The effective equalizer pre-amp, for the settings screen.

use jni::objects::{JClass, JFloatArray, JIntArray};
use jni::sys::{jboolean, jfloat};
use jni::JNIEnv;

use crate::{native, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/Dsp",
    methods: &[
        native!(c"effectivePreampDb", c"(ZFZ[I[F)F", effective_preamp_db),
    ],
};

/// `nori_core::settings::effective_preamp_db`, called on every step of a band drag; bands come as kind
/// and gain arrays.
extern "system" fn effective_preamp_db(
    env: JNIEnv, _: JClass, eq_enabled: jboolean, eq_preamp_db: jfloat, automatic: jboolean, kinds: JIntArray, gains: JFloatArray,
) -> jfloat {
    let n = env.get_array_length(&kinds).unwrap_or(0).max(0) as usize;
    let (mut k, mut g) = (vec![0i32; n], vec![0f32; n]);
    if env.get_int_array_region(&kinds, 0, &mut k).is_err() || env.get_float_array_region(&gains, 0, &mut g).is_err() {
        return 0.0;
    }
    nori_core::settings::effective_preamp_db(eq_enabled != 0, (automatic == 0).then_some(eq_preamp_db), k.into_iter().zip(g))
}
