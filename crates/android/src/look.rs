//! How a page looks, as Kotlin reaches it: `nori_look` over JNI - a cover's page worked out from the
//! pixels the cover loader decodes (covers.rs), its wash drawn straight into a Bitmap and its look into a
//! small int array - so nothing is boxed, serialised or copied twice, and the UI only ever looks colours
//! up. The lyrics page is prepared once per song through uniffi (`nori_core::look`) and then asked every
//! frame here with primitives in and one packed `long` out, allocating nothing on either side.

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
        native!(c"seekPaceNew", c"()J", seek_pace_new),
        native!(c"seekPaceFree", c"(J)V", seek_pace_free),
        native!(c"seekPaceSync", c"(JJJ)V", seek_pace_sync),
        native!(c"seekPaceHold", c"(JFJJ)V", seek_pace_hold),
        native!(c"seekPaceStep", c"(JJJFFF)I", seek_pace_step),
        native!(c"seekPaceBar", c"(J)F", seek_pace_bar),
        native!(c"seekPaceTimes", c"(J)J", seek_pace_times),
        native!(c"seekPaceFrom", c"(J)J", seek_pace_from),
        native!(c"seekPaceFade", c"(J)F", seek_pace_fade),
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
        native!(c"at", c"(JJZZZ)J", lyrics_at),
        native!(c"shown", c"(J)J", lyrics_shown),
        native!(c"shownMs", c"(J)J", lyrics_shown_ms),
        native!(c"backingSung", c"(J)F", lyrics_backing_sung),
        native!(c"tap", c"(JI)J", lyrics_tap),
        native!(c"land", c"(JI)V", lyrics_land),
        native!(c"nudge", c"(JI)J", lyrics_nudge),
        native!(c"kept", c"(JJ)J", lyrics_kept),
        native!(c"strength", c"(ZII)F", lyrics_strength),
        native!(c"matchingLine", c"([JI[JZ)I", lyrics_matching_line),
    ],
};

/// A software ARGB_8888 `Bitmap`'s own memory (or RGB_565, where the caller says it takes one), written
/// in place through Android's bitmap API (libjnigraphics): no copy of the picture into a Java
/// array, and none across into Rust. Locked for as long as the value lives. Anything else - another
/// format, a hardware bitmap, a size that does not add up - is refused, and the caller gets nothing
/// rather than a guess.
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
        /// Two bytes a pixel, RGB_565 (only from `new_or_565`), rather than ARGB_8888's four.
        pub rgb565: bool,
    }

    impl Locked {
        pub fn new(env: &JNIEnv, bitmap: &JObject) -> Option<Locked> {
            Locked::lock(env, bitmap, false)
        }

        /// An ARGB_8888 Bitmap or an RGB_565 one; `rgb565` says which.
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

        /// The bytes of one row's pixels.
        fn row_bytes(&self) -> usize {
            self.width * if self.rgb565 { 2 } else { 4 }
        }

        pub fn row_mut(&mut self, y: usize) -> &mut [u8] {
            assert!(y < self.height);
            // SAFETY: the locked pixels are `height` rows `stride` bytes apart, each at least `row_bytes`
            // long (checked in `lock`), `y` is one of them, and `&mut self` keeps the row to one writer.
            unsafe { std::slice::from_raw_parts_mut(self.px.add(y * self.stride), self.row_bytes()) }
        }

        /// All the rows at once, `stride` bytes apart, the last one only as long as its pixels.
        pub fn pixels_mut(&mut self) -> &mut [u8] {
            // SAFETY: as in `row_mut`: the last row starts `(height - 1) * stride` bytes in and holds
            // `row_bytes`, and `&mut self` keeps the pixels to one writer.
            unsafe { std::slice::from_raw_parts_mut(self.px, (self.height - 1) * self.stride + self.row_bytes()) }
        }
    }

    impl Drop for Locked {
        fn drop(&mut self) {
            // SAFETY: the pixels were locked in `new` through the same JNIEnv and reference, which outlive
            // this value (it never leaves the call).
            unsafe { AndroidBitmap_unlockPixels(self.env, self.obj) };
        }
    }
}

/// ARGB to a Bitmap's own layout (R, G, B, A, premultiplied), as `Bitmap.setPixels` would store it.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn premultiplied(argb: u32, out: &mut [u8]) {
    let a = argb >> 24;
    let c = |v: u32| if a == 255 { v } else { (v * a + 127) / 255 };
    out[0] = c((argb >> 16) & 0xFF) as u8;
    out[1] = c((argb >> 8) & 0xFF) as u8;
    out[2] = c(argb & 0xFF) as u8;
    out[3] = a as u8;
}

/// The page for a cover's `pixels` (ARGB, `w` x `h`; the cover loader decodes them, covers.rs). `out`
/// gets the page's look (`dress::LEN` entries); the wash is drawn straight into `wash` (a mutable
/// `WASH_OUT`² ARGB_8888 bitmap). Returns 0 for no page, 1 for a page, 2 for a page with its wash (not on
/// AMOLED black).
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

/// Two looks mixed at `t` into `out` (`dress::mix`): a frame of the player's page cross-fading, so one
/// crossing and no allocation, three arrays copied on the stack.
extern "system" fn mix(env: JNIEnv, _: JClass, from: JIntArray, to: JIntArray, t: jfloat, out: JIntArray) {
    let (mut a, mut b) = ([0i32; dress::LEN], [0i32; dress::LEN]);
    if env.get_int_array_region(&from, 0, &mut a).is_err() || env.get_int_array_region(&to, 0, &mut b).is_err() {
        return;
    }
    let mut mixed = [0u32; dress::LEN];
    dress::mix(&a.map(|v| v as u32), &b.map(|v| v as u32), t, &mut mixed);
    let _ = env.set_int_array_region(&out, 0, &mixed.map(|v| v as i32));
}

/// The look of a page in the theme's own colours. `roles` is background, on surface, on surface variant,
/// primary, on primary, surface variant, surface container, surface container high, secondary container,
/// outline variant; `out` gets `dress::LEN` entries.
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

/// A theme's tones from one seed colour, into `out` (11 colours; see `nori_look::theme::seeded`).
extern "system" fn tones(env: JNIEnv, _: JClass, seed: jint, dark: jboolean, out: JIntArray) {
    let tones = nori_look::theme::seeded(seed as u32, dark != 0).map(|v| v as i32);
    let _ = env.set_int_array_region(&out, 0, &tones);
}

/// The surfaces AMOLED black puts in a dark scheme (`dress::AMOLED`, 8 colours), into `out`.
extern "system" fn amoled(env: JNIEnv, _: JClass, out: JIntArray) {
    let _ = env.set_int_array_region(&out, 0, &dress::AMOLED.map(|v| v as i32));
}

/// [`nori_core::stage::seek_times`] for a frame of a scrub: `at_s << 32 | left_s`. The times are asked
/// on every frame a finger moves the bar, and a uniffi record each frame would be garbage.
extern "system" fn seek_times(dragging: jboolean, drag: jfloat, held_ms: jlong, position_ms: jlong, duration_ms: jlong) -> jlong {
    let t = nori_core::stage::seek_times(dragging != 0, drag, held_ms, position_ms, duration_ms);
    ((t.at_s.clamp(0, u32::MAX as i64)) << 32) | t.left_s.clamp(0, u32::MAX as i64)
}

/// [`nori_look::cover::readable_either_way`]: `color` moved lighter or darker in its own hue until it
/// reads on `background`, else `fallback`. ARGB ints in and out.
extern "system" fn readable(color: jint, background: jint, fallback: jint) -> jint {
    nori_look::cover::readable_either_way(color as u32, background as u32, fallback as u32) as jint
}

/// Which glyph the play button shows (`stage::transport_glyph`), as its place in `TransportGlyph`: 0 play,
/// 1 pause, 2 spinner. Asked on every play and pause, so primitives only.
extern "system" fn transport_glyph(playing: jboolean, buffering: jboolean, waited: jboolean) -> jint {
    use nori_core::stage::TransportGlyph;
    match nori_core::stage::transport_glyph(playing != 0, buffering != 0, waited != 0) {
        TransportGlyph::Play => 0,
        TransportGlyph::Pause => 1,
        TransportGlyph::Spinner => 2,
    }
}

/// A page's Shuffle and Play (`pages::hero_buttons`), packed by `HeroButtons::pack`. Asked on every play
/// and pause, so primitives only.
extern "system" fn hero_buttons(here: jboolean, shuffle: jboolean, playing: jboolean, buffering: jboolean, can_play: jboolean, can_shuffle: jboolean) -> jint {
    nori_core::pages::hero_buttons(here != 0, shuffle != 0, playing != 0, buffering != 0, can_play != 0, can_shuffle != 0).pack()
}

/// One step of the seek bar (`nori_look::motion::seek_step`), asked each frame it moves: primitives
/// in, `bar bits << 32 | wait` out, nothing allocated.
extern "system" fn seek_step(bar: jfloat, target: jfloat, dt_s: jfloat, width_px: jfloat, speed: jfloat) -> jlong {
    let (b, wait) = nori_look::motion::seek_step(bar, target, dt_s, width_px, speed);
    ((b.to_bits() as i64) << 32) | (wait as u32 as i64)
}

// ---- the seek bar's pace (`nori_look::motion::SeekPace`), one per seek bar on screen -----------------------

fn pace<'a>(h: jlong) -> Option<&'a mut SeekPace> {
    // SAFETY: Kotlin passes 0 or a handle `seek_pace_new` made, only from the main thread, and never one it
    // has freed (SeekPace.kt).
    (h != 0).then(|| unsafe { &mut *(h as *mut SeekPace) })
}

extern "system" fn seek_pace_new() -> jlong {
    Box::into_raw(Box::new(SeekPace::new())) as jlong
}

extern "system" fn seek_pace_free(h: jlong) {
    if h != 0 {
        // SAFETY: made by `seek_pace_new`, freed once (Kotlin zeroes its handle).
        drop(unsafe { Box::from_raw(h as *mut SeekPace) });
    }
}

extern "system" fn seek_pace_sync(h: jlong, position_ms: jlong, duration_ms: jlong) {
    if let Some(p) = pace(h) {
        p.sync(position_ms, duration_ms);
    }
}

extern "system" fn seek_pace_hold(h: jlong, bar: jfloat, position_ms: jlong, duration_ms: jlong) {
    if let Some(p) = pace(h) {
        p.hold(bar, position_ms, duration_ms);
    }
}

extern "system" fn seek_pace_step(h: jlong, position_ms: jlong, duration_ms: jlong, dt_s: jfloat, width_px: jfloat, rate: jfloat) -> jint {
    pace(h).map_or(-1, |p| p.step(position_ms, duration_ms, dt_s, width_px, rate))
}

extern "system" fn seek_pace_bar(h: jlong) -> jfloat {
    pace(h).map_or(0.0, |p| p.bar())
}

fn pack_times((at, left): (i64, i64)) -> jlong {
    (at << 32) | (left & 0xFFFF_FFFF)
}

/// The times shown, `at_s << 32 | left_s`.
extern "system" fn seek_pace_times(h: jlong) -> jlong {
    pace(h).map_or(0, |p| pack_times(p.times()))
}

/// The times fading out, packed as [`seek_pace_times`]; -1 with none.
extern "system" fn seek_pace_from(h: jlong) -> jlong {
    pace(h).and_then(|p| p.fading()).map_or(-1, |(t, _)| pack_times(t))
}

/// How strongly the times now are drawn, 0..1 (1: nothing fading).
extern "system" fn seek_pace_fade(h: jlong) -> jfloat {
    pace(h).map_or(1.0, |p| p.fade())
}

// ---- lyrics -------------------------------------------------------------------------------------------------

fn clock<'a>(h: jlong) -> Option<&'a LyricClock> {
    // SAFETY: Kotlin passes 0 or a handle `lyrics_clock` or `kept` made, and never one it has destroyed.
    unsafe { nori_core::look::clock(h) }
}

/// A clock on the lyrics the core read under `key` (`nori_core::look::kept_clock`); 0 when they are no
/// longer kept.
extern "system" fn lyrics_kept(key: jlong, position_ms: jlong) -> jlong {
    nori_core::look::kept_clock(key as u64, position_ms)
}

/// How lit line `line` is while `active` is sung (`nori_look::lyrics::line_strength`): asked for every
/// line on the page whenever the line being sung changes, so primitives only.
extern "system" fn lyrics_strength(synced: jboolean, line: jint, active: jint) -> jfloat {
    nori_look::lyrics::line_strength(synced != 0, line, active)
}

/// The line of the new lyrics that stands for line `at` of the old ones (`nori_look::lyrics::matching_line`),
/// each given as its lines' starts. Asked once when finer lyrics replace the ones on screen: `@FastNative`,
/// the arrays copied once.
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
    // SAFETY: `h` came from `lyrics_clock` or `kept`, and Kotlin destroys it once.
    unsafe { nori_core::look::free_clock(h) }
}

/// Whether the active line can fill in word by word at all (the listener's switch aside).
extern "system" fn lyrics_sweeps(h: jlong) -> jboolean {
    clock(h).is_some_and(|c| c.timing().sweeps()) as jboolean
}

/// The per-frame question, `LyricClock::advance` packed by `Step::pack`. A freed handle answers
/// "nothing lit, never ask again".
extern "system" fn lyrics_at(h: jlong, position_ms: jlong, sweep: jboolean, lively: jboolean, force: jboolean) -> jlong {
    clock(h).map_or(0, |c| c.advance(position_ms, sweep != 0, lively != 0, force != 0).pack())
}

/// The moment on screen, for the words' rise and glow, drawn in Kotlin.
extern "system" fn lyrics_shown_ms(h: jlong) -> jlong {
    clock(h).map_or(0, |c| c.shown_ms())
}

/// How far into the lit line's backing vocals the singing is, at the moment on screen.
extern "system" fn lyrics_backing_sung(h: jlong) -> jfloat {
    clock(h).map_or(0.0, |c| c.backing_sung())
}

/// What is on screen now, packed by `Frame::pack`.
extern "system" fn lyrics_shown(h: jlong) -> jlong {
    clock(h).map_or(0, |c| c.shown().pack())
}

/// A tap on `line`: shows it and returns where to seek the player to.
extern "system" fn lyrics_tap(h: jlong, line: jint) -> jlong {
    clock(h).map_or(0, |c| c.tap(line.max(0) as usize))
}

/// Lyrics replacing the ones on screen for the same song start on `line`, the one standing for the line
/// that was lit (`LyricClock::land`).
extern "system" fn lyrics_land(h: jlong, line: jint) {
    if let (Some(c), Ok(line)) = (clock(h), usize::try_from(line)) {
        c.land(line);
    }
}

/// Sooner (> 0), later (< 0) or back to none (0); returns the nudge in ms.
extern "system" fn lyrics_nudge(h: jlong, dir: jint) -> jlong {
    clock(h).map_or(0, |c| c.nudge(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bitmaps_pixels_are_written_like_set_pixels() {
        let mut px = [0u8; 4];
        premultiplied(0xFF123456, &mut px);
        assert_eq!(px, [0x12, 0x34, 0x56, 0xFF]);
        premultiplied(0x80FFFFFF, &mut px);
        assert_eq!(px, [0x80, 0x80, 0x80, 0x80]);
    }
}
