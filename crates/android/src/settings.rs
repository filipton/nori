//! The equalizer screen over plain JNI: the edits are asked on every step of a slider's drag, so each is
//! primitives in and out (`nori_core::settings_store`'s edits). The screen's words and figures are
//! Kotlin's own; the one thing it asks here is what kind of band a label marks.

use jni::objects::{JClass, JFloatArray};
use jni::sys::{jfloat, jint, jlong};
use jni::JNIEnv;
use nori_core::settings::{BandMark, EqLevel};

use crate::{native, Class};

pub(crate) static EQ_BANDS: Class = Class {
    name: c"dev/nori/music/settings/EqBands",
    methods: &[native!(c"mark", c"(II)I", band_mark)],
};

pub(crate) static SOUND_EDIT: Class = Class {
    name: c"dev/nori/music/settings/SoundEdit",
    methods: &[native!(c"setBand", c"(I[F)I", set_band), native!(c"setLevel", c"(IF)J", set_level), native!(c"setGraphic", c"(IF)J", set_graphic)],
};

/// What a band's label marks after its frequency (`settings::band_mark`), as its place in `BandMark`: 0
/// nothing, 1 left, 2 right, 3 low shelf, 4 high shelf, 5 no gain.
extern "system" fn band_mark(kind: jint, channel: jint) -> jint {
    match nori_core::settings::band_mark(kind, channel) {
        BandMark::None => 0,
        BandMark::Left => 1,
        BandMark::Right => 2,
        BandMark::LowShelf => 3,
        BandMark::HighShelf => 4,
        BandMark::NoGain => 5,
    }
}

/// `settings_store::edit_band` on every step of a slider: the band comes in as `[kind, freq, gain, q,
/// channel]` and goes back out the same way as it was kept. A drag builds no settings record and sends
/// none across. Returns what the player has to apply again, or -1 when nothing changed.
extern "system" fn set_band(env: JNIEnv, _: JClass, index: jint, band: JFloatArray) -> jint {
    let mut b = [0f32; 5];
    if index < 0 || env.get_float_array_region(&band, 0, &mut b).is_err() {
        return -1;
    }
    let asked = nori_core::settings::band_from(b[0] as i32, b[1], b[2], b[3], b[4] as i32);
    let Some((effect, kept)) = nori_core::settings_store::edit_band(index as u32, asked) else { return -1 };
    let out = [kept.kind as i32 as f32, kept.freq, kept.gain_db, kept.q, kept.channel as i32 as f32];
    if env.set_float_array_region(&band, 0, &out).is_err() {
        return -1;
    }
    effect as jint
}

/// `settings_store::edit_level` (`level` an [`EqLevel`] ordinal): the value as it was kept as float bits
/// in the high 32, what the player has to apply again in the low; -1 when nothing changed.
extern "system" fn set_level(level: jint, value: jfloat) -> jlong {
    let Some(level) = usize::try_from(level).ok().and_then(|l| EqLevel::ALL.get(l)) else { return -1 };
    match nori_core::settings_store::edit_level(*level, value) {
        Some((effect, kept)) => ((kept.to_bits() as jlong) << 32) | effect as jlong,
        None => -1,
    }
}

/// `settings_store::edit_graphic` on every step of a graphic equalizer slider: the value as it was kept
/// as float bits in the high 32, what the player has to apply again in the low; -1 when nothing changed.
extern "system" fn set_graphic(index: jint, value: jfloat) -> jlong {
    if index < 0 {
        return -1;
    }
    match nori_core::settings_store::edit_graphic(index as u32, value) {
        Some((effect, kept)) => ((kept.to_bits() as jlong) << 32) | effect as jlong,
        None => -1,
    }
}
