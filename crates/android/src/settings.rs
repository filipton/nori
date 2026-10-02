//! Equalizer screen edits (`nori_core::settings_store`), called on every slider step: primitives only. `s`
//! is the app's settings' handle (`crate::kept`).

use jni::objects::{JClass, JFloatArray};
use jni::sys::{jfloat, jint, jlong};
use jni::JNIEnv;
use nori_core::settings::{BandMark, EqLevel};
use nori_core::settings_store::Settings;

use crate::{native, Class};

pub(crate) static EQ_BANDS: Class = Class {
    name: c"dev/nori/music/settings/EqBands",
    methods: &[native!(c"mark", c"(II)I", band_mark)],
};

pub(crate) static SOUND_EDIT: Class = Class {
    name: c"dev/nori/music/settings/SoundEdit",
    methods: &[native!(c"setBand", c"(JI[F)I", set_band), native!(c"setLevel", c"(JIF)J", set_level), native!(c"setGraphic", c"(JIF)J", set_graphic)],
};

/// `settings::band_mark` as its `BandMark` ordinal.
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

/// `settings_store::edit_band`: `band` is `[kind, freq, gain, q, channel]` in, the stored band out.
/// Returns the effect flags to apply, or -1 when nothing changed.
extern "system" fn set_band(env: JNIEnv, _: JClass, s: jlong, index: jint, band: JFloatArray) -> jint {
    let mut b = [0f32; 5];
    if index < 0 || env.get_float_array_region(&band, 0, &mut b).is_err() {
        return -1;
    }
    let asked = nori_core::settings::band_from(b[0] as i32, b[1], b[2], b[3], b[4] as i32);
    let Some((effect, kept)) = crate::kept::<Settings>(s).edit_band(index as u32, asked) else { return -1 };
    let out = [kept.kind as i32 as f32, kept.freq, kept.gain_db, kept.q, kept.channel as i32 as f32];
    if env.set_float_array_region(&band, 0, &out).is_err() {
        return -1;
    }
    effect as jint
}

/// Packs an edit result: stored value's float bits high, effect flags low; -1 when nothing changed.
fn pack_edit(edit: Option<(u32, f32)>) -> jlong {
    edit.map_or(-1, |(effect, kept)| ((kept.to_bits() as jlong) << 32) | effect as jlong)
}

/// `settings_store::edit_level`; `level` is an [`EqLevel`] ordinal.
extern "system" fn set_level(s: jlong, level: jint, value: jfloat) -> jlong {
    let Some(level) = usize::try_from(level).ok().and_then(|l| EqLevel::ALL.get(l)) else { return -1 };
    pack_edit(crate::kept::<Settings>(s).edit_level(*level, value))
}

/// `settings_store::edit_graphic`.
extern "system" fn set_graphic(s: jlong, index: jint, value: jfloat) -> jlong {
    let Ok(index) = u32::try_from(index) else { return -1 };
    pack_edit(crate::kept::<Settings>(s).edit_graphic(index, value))
}
