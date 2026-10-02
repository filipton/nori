//! `nori_look` for Kotlin: page colours from cover pixels (wash drawn straight into a Bitmap, look into an
//! int array), seek bar pacing, and per-frame lyric clock queries (primitives in, packed `long` out).

use jni::objects::{JClass, JIntArray, JLongArray, JObject};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::JNIEnv;
use nori_look::cover::derive;
#[cfg(target_os = "android")]
use nori_look::cover::WASH_OUT;
use nori_look::dress;
use nori_look::lyrics::LyricClock;
use nori_look::motion::SeekPace;

use crate::{native, Class};

pub(crate) static COVER: Class = Class {
    name: c"dev/nori/music/look/CoverLook",
    methods: &[
        native!(c"mix", c"([I[IF[I)V", mix),
        native!(c"seekTimes", c"(ZFJJJ)J", seek_times),
        native!(c"seekStep", c"(FFFFF)J", seek_step),
        native!(c"seekPaceNew", c"(J)J", seek_pace_new),
        native!(c"seekPaceFree", c"(J)V", seek_pace_free),
        native!(c"seekPaceSync", c"(JJJ)V", seek_pace_sync),
        native!(c"seekPaceHold", c"(JFJJ)V", seek_pace_hold),
        native!(c"seekPaceStep", c"(JJJFFF)I", seek_pace_step),
        native!(c"transportGlyph", c"(ZZZ)I", transport_glyph),
        native!(c"readable", c"(III)I", readable),
        native!(c"heroButtons", c"(ZZZZZZ)I", hero_buttons),
        native!(c"plain", c"([I[I)V", plain),
        native!(c"tones", c"(IZ[I)V", tones),
        native!(c"amoled", c"([I)V", amoled),
    ],
};

pub(crate) static LYRICS: Class = Class {
    name: c"dev/nori/music/look/LyricsJni",
    methods: &[
        native!(c"destroy", c"(J)V", lyrics_destroy),
        native!(c"sweeps", c"(J)Z", lyrics_sweeps),
        native!(c"at", c"(JJZZZJ)J", lyrics_at),
        native!(c"shown", c"(J)J", lyrics_shown),
        native!(c"shownMs", c"(J)J", lyrics_shown_ms),
        native!(c"tap", c"(JI)J", lyrics_tap),
        native!(c"land", c"(JI)V", lyrics_land),
        native!(c"nudge", c"(JI)J", lyrics_nudge),
        native!(c"strength", c"(ZII)F", lyrics_strength),
        native!(c"matchingLine", c"([JI[JZ)I", lyrics_matching_line),
    ],
};

/// A software ARGB_8888 (or, if accepted, RGB_565) Bitmap's pixels, locked through libjnigraphics for the
/// value's lifetime. Other formats, hardware Bitmaps and inconsistent sizes are refused.
#[cfg(target_os = "android")]
pub(crate) mod bitmap {
    use jni::objects::JObject;
    use jni::JNIEnv;
    use std::ffi::c_void;

    #[repr(C)]
    struct Info {
        width: u32,
        height: u32,
        stride: u32,
        format: i32,
        flags: u32,
    }
    const RGBA_8888: i32 = 1;
    const RGB_565: i32 = 4;

    #[link(name = "jnigraphics")]
    extern "C" {
        fn AndroidBitmap_getInfo(env: *mut jni::sys::JNIEnv, bitmap: jni::sys::jobject, info: *mut Info) -> i32;
        fn AndroidBitmap_lockPixels(env: *mut jni::sys::JNIEnv, bitmap: jni::sys::jobject, addr: *mut *mut c_void) -> i32;
        fn AndroidBitmap_unlockPixels(env: *mut jni::sys::JNIEnv, bitmap: jni::sys::jobject) -> i32;
    }

    pub struct Locked {
        env: *mut jni::sys::JNIEnv,
        obj: jni::sys::jobject,
        pub px: *mut u8,
        pub width: usize,
        pub height: usize,
        pub stride: usize,
        /// RGB_565 (only from `new_or_565`).
        pub rgb565: bool,
    }

    impl Locked {
        pub fn new(env: &JNIEnv, bitmap: &JObject) -> Option<Locked> {
            Locked::lock(env, bitmap, false)
        }

        /// Also accepts RGB_565; `rgb565` says which it is.
        pub fn new_or_565(env: &JNIEnv, bitmap: &JObject) -> Option<Locked> {
            Locked::lock(env, bitmap, true)
        }

        fn lock(env: &JNIEnv, bitmap: &JObject, take_565: bool) -> Option<Locked> {
            let (e, o) = (env.get_raw(), bitmap.as_raw());
            if o.is_null() {
                return None;
            }
            let mut info = Info { width: 0, height: 0, stride: 0, format: 0, flags: 0 };
            // SAFETY: `e` is this call's live JNIEnv and `o` a non-null Bitmap reference it holds.
            if unsafe { AndroidBitmap_getInfo(e, o, &mut info) } != 0 {
                return None;
            }
            let rgb565 = match info.format {
                RGBA_8888 => false,
                RGB_565 if take_565 => true,
                _ => return None,
            };
            let (width, height, stride) = (info.width as usize, info.height as usize, info.stride as usize);
            if width == 0 || height == 0 || stride < width * if rgb565 { 2 } else { 4 } {
                return None;
            }
            let mut addr: *mut c_void = std::ptr::null_mut();
            // SAFETY: as above; the pixels stay locked until `drop` unlocks them.
            if unsafe { AndroidBitmap_lockPixels(e, o, &mut addr) } != 0 || addr.is_null() {
                return None;
            }
            Some(Locked { env: e, obj: o, px: addr as *mut u8, width, height, stride, rgb565 })
        }

        fn row_bytes(&self) -> usize {
            self.width * if self.rgb565 { 2 } else { 4 }
        }

        pub fn row_mut(&mut self, y: usize) -> &mut [u8] {
            assert!(y < self.height);
            // SAFETY: the locked pixels are `height` rows `stride` bytes apart, each at least `row_bytes`
            // long (checked in `lock`), `y` is one of them, and `&mut self` keeps the row to one writer.
            unsafe { std::slice::from_raw_parts_mut(self.px.add(y * self.stride), self.row_bytes()) }
        }

        /// All rows, `stride` bytes apart; the last one without padding.
        pub fn pixels_mut(&mut self) -> &mut [u8] {
            // SAFETY: as in `row_mut`: the last row starts `(height - 1) * stride` bytes in and holds
            // `row_bytes`, and `&mut self` keeps the pixels to one writer.
            unsafe { std::slice::from_raw_parts_mut(self.px, (self.height - 1) * self.stride + self.row_bytes()) }
        }
    }

    impl Drop for Locked {
        fn drop(&mut self) {
            // SAFETY: locked in `lock` through the same JNIEnv and reference, which outlive this value.
            unsafe { AndroidBitmap_unlockPixels(self.env, self.obj) };
        }
    }
}

/// ARGB to a Bitmap's premultiplied RGBA bytes, as `Bitmap.setPixels` stores it.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn premultiplied(argb: u32, out: &mut [u8]) {
    let a = argb >> 24;
    let c = |v: u32| if a == 255 { v } else { (v * a + 127) / 255 };
    out[0] = c((argb >> 16) & 0xFF) as u8;
    out[1] = c((argb >> 8) & 0xFF) as u8;
    out[2] = c(argb & 0xFF) as u8;
    out[3] = a as u8;
}

/// The page for a cover's ARGB `pixels`: its look into `out` (`dress::LEN` ints), its wash into `wash` (a
/// mutable `WASH_OUT`² ARGB_8888 Bitmap). Returns 0 no page, 1 page, 2 page and wash.
pub(crate) fn page(env: &JNIEnv, pixels: &[u32], w: usize, h: usize, dark: bool, amoled: bool, out: &JIntArray, wash: &JObject) -> jint {
    let c = derive(pixels, w, h, dark, amoled);
    let look = dress::page(c.edge, c.background, c.on, c.accent, c.wash_edge).map(|v| v as i32);
    if env.set_int_array_region(out, 0, &look).is_err() {
        return 0;
    }
    let Some(pixels) = c.wash else { return 1 };
    #[cfg(target_os = "android")]
    {
        let Some(mut w) = bitmap::Locked::new(env, wash) else { return 1 };
        if w.width != WASH_OUT || w.height != WASH_OUT {
            return 1;
        }
        for y in 0..WASH_OUT {
            let row = w.row_mut(y);
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                premultiplied(pixels[y * WASH_OUT + x], px);
            }
        }
        2
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (wash, pixels);
        1
    }
}

/// `dress::mix` of two looks at `t`, per cross-fade frame; no allocation.
extern "system" fn mix(env: JNIEnv, _: JClass, from: JIntArray, to: JIntArray, t: jfloat, out: JIntArray) {
    let (mut a, mut b) = ([0i32; dress::LEN], [0i32; dress::LEN]);
    if env.get_int_array_region(&from, 0, &mut a).is_err() || env.get_int_array_region(&to, 0, &mut b).is_err() {
        return;
    }
    let mut mixed = [0u32; dress::LEN];
    dress::mix(&a.map(|v| v as u32), &b.map(|v| v as u32), t, &mut mixed);
    let _ = env.set_int_array_region(&out, 0, &mixed.map(|v| v as i32));
}

/// `dress::plain`: `roles` is the 10 `dress::Scheme` colours in field order.
extern "system" fn plain(env: JNIEnv, _: JClass, roles: JIntArray, out: JIntArray) {
    let mut r = [0i32; 10];
    if env.get_int_array_region(&roles, 0, &mut r).is_err() {
        return;
    }
    let r = r.map(|v| v as u32);
    let s = dress::Scheme {
        background: r[0], on: r[1], on_variant: r[2], primary: r[3], on_primary: r[4], surface_variant: r[5],
        surface_container: r[6], surface_container_high: r[7], secondary_container: r[8], outline_variant: r[9],
    };
    let _ = env.set_int_array_region(&out, 0, &dress::plain(&s).map(|v| v as i32));
}

/// `nori_look::theme::seeded` into `out` (11 colours).
extern "system" fn tones(env: JNIEnv, _: JClass, seed: jint, dark: jboolean, out: JIntArray) {
    let tones = nori_look::theme::seeded(seed as u32, dark != 0).map(|v| v as i32);
    let _ = env.set_int_array_region(&out, 0, &tones);
}

/// `dress::AMOLED` (8 colours) into `out`.
extern "system" fn amoled(env: JNIEnv, _: JClass, out: JIntArray) {
    let _ = env.set_int_array_region(&out, 0, &dress::AMOLED.map(|v| v as i32));
}

/// [`nori_core::stage::seek_times`] per scrub frame, packed `at_s << 32 | left_s`.
extern "system" fn seek_times(dragging: jboolean, drag: jfloat, held_ms: jlong, position_ms: jlong, duration_ms: jlong) -> jlong {
    let t = nori_core::stage::seek_times(dragging != 0, drag, held_ms, position_ms, duration_ms);
    ((t.at_s.clamp(0, u32::MAX as i64)) << 32) | t.left_s.clamp(0, u32::MAX as i64)
}

/// [`nori_look::cover::readable_either_way`], ARGB ints.
extern "system" fn readable(color: jint, background: jint, fallback: jint) -> jint {
    nori_look::cover::readable_either_way(color as u32, background as u32, fallback as u32) as jint
}

/// `stage::transport_glyph` as its `TransportGlyph` ordinal.
extern "system" fn transport_glyph(playing: jboolean, buffering: jboolean, waited: jboolean) -> jint {
    use nori_core::stage::TransportGlyph;
    match nori_core::stage::transport_glyph(playing != 0, buffering != 0, waited != 0) {
        TransportGlyph::Play => 0,
        TransportGlyph::Pause => 1,
        TransportGlyph::Spinner => 2,
    }
}

/// `pages::hero_buttons`, packed by `HeroButtons::pack`.
extern "system" fn hero_buttons(here: jboolean, shuffle: jboolean, playing: jboolean, buffering: jboolean, can_play: jboolean, can_shuffle: jboolean) -> jint {
    nori_core::pages::hero_buttons(here != 0, shuffle != 0, playing != 0, buffering != 0, can_play != 0, can_shuffle != 0).pack()
}

/// `nori_look::motion::seek_step`, packed `bar bits << 32 | wait`.
extern "system" fn seek_step(bar: jfloat, target: jfloat, dt_s: jfloat, width_px: jfloat, speed: jfloat) -> jlong {
    let (b, wait) = nori_look::motion::seek_step(bar, target, dt_s, width_px, speed);
    ((b.to_bits() as i64) << 32) | (wait as u32 as i64)
}

// ---- `nori_look::motion::SeekPace`, one per seek bar ----

/// What a seek bar shows, left in SeekPace.kt's view after every change: the bar (0..1), the current
/// times' opacity (0..1), the times (`at_s << 32 | left_s`) and those fading out (-1: none).
#[repr(C)]
struct PaceShown {
    bar: f32,
    fade: f32,
    times: i64,
    from: i64,
}

/// A seek pace and the view it shows into.
struct Pace {
    pace: SeekPace,
    view: jlong,
}

impl Pace {
    fn show(&self) {
        let p = &self.pace;
        let shown = PaceShown { bar: p.bar(), fade: p.fade(), times: pack_times(p.times()), from: p.fading().map_or(-1, |(t, _)| pack_times(t)) };
        // SAFETY: `view` is SeekPace.kt's buffer, kept with the pace and written only here, on its thread.
        unsafe { crate::view::put(self.view, shown) }
    }
}

fn pace<'a>(h: jlong) -> Option<&'a mut Pace> {
    // SAFETY: 0 or a live `seek_pace_new` handle, used only from the main thread (SeekPace.kt).
    (h != 0).then(|| unsafe { &mut *(h as *mut Pace) })
}

fn pack_times((at, left): (i64, i64)) -> jlong {
    (at << 32) | (left & 0xFFFF_FFFF)
}

/// A pace showing into `view` (a `NativeView` address of 24 bytes).
extern "system" fn seek_pace_new(view: jlong) -> jlong {
    let p = Pace { pace: SeekPace::new(), view };
    p.show();
    Box::into_raw(Box::new(p)) as jlong
}

extern "system" fn seek_pace_free(h: jlong) {
    if h != 0 {
        // SAFETY: made by `seek_pace_new`, freed once (Kotlin zeroes its handle).
        drop(unsafe { Box::from_raw(h as *mut Pace) });
    }
}

extern "system" fn seek_pace_sync(h: jlong, position_ms: jlong, duration_ms: jlong) {
    if let Some(p) = pace(h) {
        p.pace.sync(position_ms, duration_ms);
        p.show();
    }
}

extern "system" fn seek_pace_hold(h: jlong, bar: jfloat, position_ms: jlong, duration_ms: jlong) {
    if let Some(p) = pace(h) {
        p.pace.hold(bar, position_ms, duration_ms);
        p.show();
    }
}

extern "system" fn seek_pace_step(h: jlong, position_ms: jlong, duration_ms: jlong, dt_s: jfloat, width_px: jfloat, rate: jfloat) -> jint {
    pace(h).map_or(-1, |p| {
        let wait = p.pace.step(position_ms, duration_ms, dt_s, width_px, rate);
        p.show();
        wait
    })
}

// ---- lyrics ----

fn clock<'a>(h: jlong) -> Option<&'a LyricClock> {
    // SAFETY: 0 or a live handle from `lyrics_clock`.
    unsafe { nori_core::look::clock(h) }
}

/// `nori_look::lyrics::line_strength`.
extern "system" fn lyrics_strength(synced: jboolean, line: jint, active: jint) -> jfloat {
    nori_look::lyrics::line_strength(synced != 0, line, active)
}

/// `nori_look::lyrics::matching_line`; both lyrics as their line start times.
extern "system" fn lyrics_matching_line(env: JNIEnv, _: JClass, old: JLongArray, at: jint, next: JLongArray, timed: jboolean) -> jint {
    let read = |a: &JLongArray| -> Vec<i64> {
        let n = env.get_array_length(a).unwrap_or(0).max(0) as usize;
        let mut v = vec![0i64; n];
        if env.get_long_array_region(a, 0, &mut v).is_err() {
            v.clear();
        }
        v
    };
    nori_look::lyrics::matching_line(&read(&old), at, &read(&next), timed != 0)
}

extern "system" fn lyrics_destroy(h: jlong) {
    // SAFETY: `h` came from `lyrics_clock`; Kotlin destroys it once.
    unsafe { nori_core::look::free_clock(h) }
}

/// Whether the lyrics have word timing.
extern "system" fn lyrics_sweeps(h: jlong) -> jboolean {
    clock(h).is_some_and(|c| c.timing().sweeps()) as jboolean
}

/// `LyricClock::advance`, packed by `Step::pack`; 0 for a null handle.
/// The moment on screen and how far the lit line's backing vocals are sung there, left in
/// LyricsClock.kt's view by [`lyrics_at`].
#[repr(C)]
struct LyricsShown {
    ms: i64,
    backing_sung: f32,
}

/// Also leaves [`LyricsShown`] in `view` (a `NativeView` address of 16 bytes).
extern "system" fn lyrics_at(h: jlong, position_ms: jlong, sweep: jboolean, lively: jboolean, force: jboolean, view: jlong) -> jlong {
    clock(h).map_or(0, |c| {
        let step = c.advance(position_ms, sweep != 0, lively != 0, force != 0).pack();
        // SAFETY: `view` is LyricsClock.kt's buffer, kept with the clock and written only here, on its thread.
        unsafe { crate::view::put(view, LyricsShown { ms: c.shown_ms(), backing_sung: c.backing_sung() }) };
        step
    })
}

/// The displayed time, for Kotlin's word animations.
extern "system" fn lyrics_shown_ms(h: jlong) -> jlong {
    clock(h).map_or(0, |c| c.shown_ms())
}

/// The displayed frame, packed by `Frame::pack`.
extern "system" fn lyrics_shown(h: jlong) -> jlong {
    clock(h).map_or(0, |c| c.shown().pack())
}

/// A tap on `line`: shows it; returns the position to seek to.
extern "system" fn lyrics_tap(h: jlong, line: jint) -> jlong {
    clock(h).map_or(0, |c| c.tap(line.max(0) as usize))
}

/// `LyricClock::land`: replacement lyrics start on `line`.
extern "system" fn lyrics_land(h: jlong, line: jint) {
    if let (Some(c), Ok(line)) = (clock(h), usize::try_from(line)) {
        c.land(line);
    }
}

/// Timing nudge: sooner (> 0), later (< 0) or reset (0); returns the nudge in ms.
extern "system" fn lyrics_nudge(h: jlong, dir: jint) -> jlong {
    clock(h).map_or(0, |c| c.nudge(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_matches_set_pixels() {
        let mut px = [0u8; 4];
        premultiplied(0xFF123456, &mut px);
        assert_eq!(px, [0x12, 0x34, 0x56, 0xFF]);
        premultiplied(0x80FFFFFF, &mut px);
        assert_eq!(px, [0x80, 0x80, 0x80, 0x80]);
    }
}
