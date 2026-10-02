//! Covers: nori-covers' loader over the core's cover transport (the app's OkHttp), with a disk cache,
//! decoding straight into Bitmaps sized for their view. One `CoverPixels.Waiter.done` callback per
//! request, on a loader thread; cancelling drops the ticket. Kotlin keeps the memory cache (Bitmaps are
//! Java objects).
//!
//! Bitmaps are software ARGB_8888, or RGB_565 for opaque pictures when allowed. With `hardware`, the
//! picture is drawn into a reused software Bitmap and copied to the GPU here, off the UI thread. Reused
//! Bitmaps and buffers are released when the loader rests.
// Bitmaps exist only on Android; elsewhere the natives build and refuse every Bitmap.
#![cfg_attr(not(target_os = "android"), allow(dead_code, unused_variables))]

use std::cell::RefCell;
use std::io::Read;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jni::objects::{GlobalRef, JClass, JIntArray, JMethodID, JObject, JStaticMethodID, JString, JValue};
use jni::signature::{Primitive, ReturnType};
use jni::sys::{jboolean, jint, jlong};
use jni::{JNIEnv, JavaVM};
use nori_core::transport::{Exchange, FailureKind, Transport, TransportError, TransportResponse};
use nori_covers::{Alpha, Config, DecodeError, Decoder, Loader, Paint, Target, Ticket};
use parking_lot::Mutex;

use crate::{cleared, native, with_str, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/look/CoverPixels",
    methods: &[
        native!(c"open", c"(Ljava/lang/String;JZZ)J", open),
        native!(c"close", c"(J)V", close),
        native!(c"request", c"(JLjava/lang/String;IILdev/nori/music/look/CoverPixels$Waiter;)J", request),
        native!(c"cancel", c"(J)V", cancel),
        native!(c"warm", c"(JLjava/lang/String;)V", warm),
        native!(c"clear", c"(J)V", clear),
        native!(c"rest", c"(J)V", rest),
        native!(c"show", c"(JZ)V", show),
        native!(c"colours", c"(JLjava/lang/String;IZ[ILandroid/graphics/Bitmap;[I)I", colours),
        native!(c"decodeFile", c"(Ljava/lang/String;Landroid/graphics/Bitmap;Z)I", decode_file),
        native!(c"isProvider", c"(Ljava/lang/String;)Z", is_provider),
    ],
};

/// Status codes passed to Kotlin.
const OK: jint = 0;
/// Not a mutable software ARGB_8888/RGB_565 Bitmap, or none could be made.
const BAD_BITMAP: jint = 1;
const UNREADABLE: jint = 2;
/// Not JPEG, PNG, WebP or GIF.
const UNKNOWN: jint = 3;
/// Corrupt, or larger than any cover.
const BROKEN: jint = 4;
/// Cancelled or loader closed; asking again works.
const CLOSED: jint = 5;
/// Plus the transport's `FailureKind` ordinal.
const NETWORK: jint = 100;
/// Plus the HTTP status.
const HTTP: jint = 1000;

/// Java classes and methods, looked up when the first loader opens. Global: loader threads call back
/// with no handle to it.
struct Java {
    vm: JavaVM,
    bitmap: GlobalRef,
    create: JStaticMethodID,
    copy: JMethodID,
    set_has_alpha: JMethodID,
    reconfigure: JMethodID,
    allocation: JMethodID,
    recycle: JMethodID,
    argb: GlobalRef,
    rgb565: GlobalRef,
    hardware: GlobalRef,
    done: JMethodID,
}

static JAVA: OnceLock<Java> = OnceLock::new();

fn look_up(env: &mut JNIEnv) -> jni::errors::Result<Java> {
    let bitmap = env.find_class("android/graphics/Bitmap")?;
    let config = env.find_class("android/graphics/Bitmap$Config")?;
    let waiter = env.find_class("dev/nori/music/look/CoverPixels$Waiter")?;
    let mut value = |name: &str| -> jni::errors::Result<GlobalRef> {
        let v = env.get_static_field(&config, name, "Landroid/graphics/Bitmap$Config;")?.l()?;
        env.new_global_ref(v)
    };
    let (argb, rgb565, hardware) = (value("ARGB_8888")?, value("RGB_565")?, value("HARDWARE")?);
    Ok(Java {
        vm: env.get_java_vm()?,
        create: env.get_static_method_id(&bitmap, "createBitmap", "(IILandroid/graphics/Bitmap$Config;)Landroid/graphics/Bitmap;")?,
        copy: env.get_method_id(&bitmap, "copy", "(Landroid/graphics/Bitmap$Config;Z)Landroid/graphics/Bitmap;")?,
        set_has_alpha: env.get_method_id(&bitmap, "setHasAlpha", "(Z)V")?,
        reconfigure: env.get_method_id(&bitmap, "reconfigure", "(IILandroid/graphics/Bitmap$Config;)V")?,
        allocation: env.get_method_id(&bitmap, "getAllocationByteCount", "()I")?,
        recycle: env.get_method_id(&bitmap, "recycle", "()V")?,
        done: env.get_method_id(&waiter, "done", "(Landroid/graphics/Bitmap;I)V")?,
        bitmap: env.new_global_ref(&bitmap)?,
        argb,
        rgb565,
        hardware,
    })
}

/// A decoded cover and its size in bytes.
#[derive(Clone)]
struct Drawn {
    bitmap: GlobalRef,
    bytes: usize,
}

/// Largest scratch buffer or Bitmap kept for reuse (a 512 x 512 cover; list and grid covers fit).
const KEEP_BYTES: usize = 512 * 512 * 4;

/// Reusable per-decode buffers: RGBA rows for RGB_565 packing, and the software Bitmap drawn into before
/// the GPU copy. One per concurrent decode.
#[derive(Default)]
struct Scratch {
    rgba: Vec<u8>,
    drawn: Option<GlobalRef>,
}

/// The loader's painter: covers into Bitmaps.
struct Bitmaps {
    hardware: bool,
    rgb565: bool,
    idle: Mutex<Vec<Scratch>>,
    /// Reusable buffers for [`colours`].
    colours: Mutex<Vec<Colours>>,
}

/// Why a Bitmap was not drawn.
enum Fail {
    Java,
    Decode(DecodeError),
}

impl From<jni::errors::Error> for Fail {
    fn from(_: jni::errors::Error) -> Fail {
        Fail::Java
    }
}

impl Paint for Bitmaps {
    type Picture = Drawn;

    fn paint(&self, decoder: &mut Decoder, bytes: &[u8], width: u32, height: u32) -> Result<Drawn, DecodeError> {
        let head = nori_covers::header(bytes)?;
        let (w, h) = head.fill(width as usize, height as usize);
        let java = JAVA.get().ok_or(DecodeError::Target)?;
        let mut env = crate::attached(&java.vm).ok_or(DecodeError::Target)?;
        let opaque = head.opaque();
        let small = opaque && self.rgb565;
        let (config, pixel) = if small { (&java.rgb565, 2) } else { (&java.argb, 4) };
        let mut scratch = self.idle.lock().pop().unwrap_or_default();
        let drawn = env.with_local_frame(4, |env| -> Result<GlobalRef, Fail> {
            let (w, h) = (w as jint, h as jint);
            if !self.hardware {
                let b = create(env, java, w, h, config)?;
                draw(env, decoder, &mut scratch.rgba, bytes, &b, false)?;
                has_alpha(env, java, &b, !opaque)?;
                return Ok(env.new_global_ref(b)?);
            }
            let b = scratch.bitmap(env, java, w, h, config, pixel)?;
            draw(env, decoder, &mut scratch.rgba, bytes, b.as_obj(), false)?;
            has_alpha(env, java, b.as_obj(), !opaque)?;
            let args = [JValue::Object(java.hardware.as_obj()).as_jni(), JValue::Bool(0).as_jni()];
            // SAFETY: `copy` is Bitmap's `(Bitmap.Config, boolean) -> Bitmap`, called on a Bitmap with those arguments.
            let gpu = unsafe { env.call_method_unchecked(b.as_obj(), java.copy, ReturnType::Object, &args) }.and_then(|v| v.l());
            match gpu {
                Ok(gpu) if !gpu.is_null() => Ok(env.new_global_ref(gpu)?),
                // No GPU copy (out of graphics memory): hand over the software Bitmap and stop reusing it.
                _ => {
                    cleared(env);
                    scratch.drawn = None;
                    Ok(b)
                }
            }
        });
        cleared(&mut env);
        scratch.drop_oversized(&mut env, java);
        self.idle.lock().push(scratch);
        let bytes = w * h * pixel;
        match drawn {
            Ok(bitmap) => Ok(Drawn { bitmap, bytes }),
            Err(Fail::Decode(e)) => Err(e),
            Err(Fail::Java) => Err(DecodeError::Target),
        }
    }

    fn bytes(picture: &Drawn) -> usize {
        picture.bytes
    }

    fn rest(&self) {
        let kept = std::mem::take(&mut *self.idle.lock());
        self.colours.lock().clear();
        let (Some(java), false) = (JAVA.get(), kept.is_empty()) else { return };
        let Some(mut env) = crate::attached(&java.vm) else { return };
        for mut scratch in kept {
            scratch.recycle(&mut env, java);
        }
        cleared(&mut env);
    }
}

/// A new mutable software Bitmap.
fn create<'l>(env: &mut JNIEnv<'l>, java: &Java, w: jint, h: jint, config: &GlobalRef) -> Result<JObject<'l>, Fail> {
    let args = [JValue::Int(w).as_jni(), JValue::Int(h).as_jni(), JValue::Object(config.as_obj()).as_jni()];
    // SAFETY: `createBitmap` is the static `(int, int, Bitmap.Config) -> Bitmap`, called on Bitmap's class with those.
    let b = unsafe { env.call_static_method_unchecked(<&JClass>::from(java.bitmap.as_obj()), java.create, ReturnType::Object, &args) }?.l()?;
    if b.is_null() {
        return Err(Fail::Java);
    }
    Ok(b)
}

/// `setHasAlpha`: opaque Bitmaps skip blending when drawn.
fn has_alpha(env: &mut JNIEnv, java: &Java, b: &JObject, alpha: bool) -> Result<(), Fail> {
    // SAFETY: `setHasAlpha` is Bitmap's `(boolean) -> void`.
    unsafe { env.call_method_unchecked(b, java.set_has_alpha, ReturnType::Primitive(Primitive::Void), &[JValue::Bool(alpha.into()).as_jni()]) }?;
    Ok(())
}

impl Scratch {
    /// The kept Bitmap reconfigured to `w` x `h` in `config` if its allocation fits, else a new one.
    fn bitmap(&mut self, env: &mut JNIEnv, java: &Java, w: jint, h: jint, config: &GlobalRef, pixel: usize) -> Result<GlobalRef, Fail> {
        let need = w as usize * h as usize * pixel;
        if let Some(b) = &self.drawn {
            // SAFETY: `getAllocationByteCount` is Bitmap's `() -> int`.
            let has = unsafe { env.call_method_unchecked(b.as_obj(), java.allocation, ReturnType::Primitive(Primitive::Int), &[]) }?.i()?;
            if has as usize >= need {
                let args = [JValue::Int(w).as_jni(), JValue::Int(h).as_jni(), JValue::Object(config.as_obj()).as_jni()];
                // SAFETY: `reconfigure` is Bitmap's `(int, int, Bitmap.Config) -> void`, on a mutable Bitmap large enough.
                unsafe { env.call_method_unchecked(b.as_obj(), java.reconfigure, ReturnType::Primitive(Primitive::Void), &args) }?;
                return Ok(b.clone());
            }
        }
        self.recycle(env, java);
        let b = create(env, java, w, h, config)?;
        let b = env.new_global_ref(b)?;
        self.drawn = Some(b.clone());
        Ok(b)
    }

    /// Drops buffers and Bitmaps larger than [`KEEP_BYTES`].
    fn drop_oversized(&mut self, env: &mut JNIEnv, java: &Java) {
        if self.rgba.capacity() > KEEP_BYTES {
            self.rgba = Vec::new();
        }
        let Some(b) = &self.drawn else { return };
        // SAFETY: as in `bitmap`.
        let has = unsafe { env.call_method_unchecked(b.as_obj(), java.allocation, ReturnType::Primitive(Primitive::Int), &[]) }.and_then(|v| v.i());
        if has.map_or(true, |n| n as usize > KEEP_BYTES) {
            self.recycle(env, java);
        }
        cleared(env);
    }

    fn recycle(&mut self, env: &mut JNIEnv, java: &Java) {
        if let Some(b) = self.drawn.take() {
            // SAFETY: `recycle` is Bitmap's `() -> void`; nothing else holds this Bitmap.
            let _ = unsafe { env.call_method_unchecked(b.as_obj(), java.recycle, ReturnType::Primitive(Primitive::Void), &[]) };
        }
    }
}

/// Packs tight RGBA rows into RGB_565 rows `stride` bytes apart (each channel's top bits, as Android
/// does). Alpha is dropped: only opaque covers use RGB_565.
fn pack_565(rgba: &[u8], width: usize, height: usize, out: &mut [u8], stride: usize) {
    for y in 0..height {
        let from = &rgba[y * width * 4..(y + 1) * width * 4];
        let to = &mut out[y * stride..y * stride + width * 2];
        for (p, q) in from.as_chunks::<4>().0.iter().zip(to.chunks_exact_mut(2)) {
            let v = (u16::from(p[0]) >> 3) << 11 | (u16::from(p[1]) >> 2) << 5 | u16::from(p[2]) >> 3;
            q.copy_from_slice(&v.to_le_bytes());
        }
    }
}

/// Decodes `bytes` to fill `bitmap` (`idct`: `Decoder::set_idct_scaling`). RGB_565 goes through `rgba`.
fn draw(env: &JNIEnv, decoder: &mut Decoder, rgba: &mut Vec<u8>, bytes: &[u8], bitmap: &JObject, idct: bool) -> Result<(), Fail> {
    #[cfg(target_os = "android")]
    {
        let Some(mut b) = crate::look::bitmap::Locked::new_or_565(env, bitmap) else { return Err(Fail::Decode(DecodeError::Target)) };
        let (width, height, stride) = (b.width, b.height, b.stride);
        decoder.set_idct_scaling(idct);
        if b.rgb565 {
            rgba.reserve_exact((width * height * 4).saturating_sub(rgba.len()));
            rgba.resize(width * height * 4, 0);
            decoder.decode_into(bytes, Target { px: rgba, width, height, stride: width * 4 }, Alpha::Premultiplied).map_err(Fail::Decode)?;
            pack_565(rgba, width, height, b.pixels_mut(), stride);
            Ok(())
        } else {
            decoder.decode_into(bytes, Target { px: b.pixels_mut(), width, height, stride }, Alpha::Premultiplied).map_err(Fail::Decode)
        }
    }
    #[cfg(not(target_os = "android"))]
    Err(Fail::Decode(DecodeError::Target))
}

fn code(e: &nori_covers::Error) -> jint {
    match e {
        nori_covers::Error::Decode(DecodeError::Unknown) => UNKNOWN,
        nori_covers::Error::Decode(DecodeError::Target) => BAD_BITMAP,
        nori_covers::Error::Decode(_) | nori_covers::Error::Panicked(_) => BROKEN,
        nori_covers::Error::Closed => CLOSED,
        nori_covers::Error::Transport { kind, .. } => NETWORK + *kind as jint,
        nori_covers::Error::Status(s) => HTTP + jint::from(*s),
    }
}

/// Calls the waiter's `done`, on the loader thread.
fn deliver(waiter: &GlobalRef, r: Result<Drawn, nori_covers::Error>) {
    let Some(java) = JAVA.get() else { return };
    let Some(mut env) = crate::attached(&java.vm) else { return };
    let null = JObject::null();
    let (bitmap, status) = match &r {
        Ok(d) => (d.bitmap.as_obj(), OK),
        Err(e) => (&null, code(e)),
    };
    let args = [JValue::Object(bitmap).as_jni(), JValue::Int(status).as_jni()];
    // SAFETY: `done` is `CoverPixels.Waiter`'s `(Bitmap, int) -> void`, and `waiter` implements it.
    let _ = unsafe { env.call_method_unchecked(waiter.as_obj(), java.done, ReturnType::Primitive(Primitive::Void), &args) };
    cleared(&mut env);
}

/// The app's cover transport (`set_cover_transport`), resolved lazily: the loader opens on the main
/// thread, before the HTTP client exists.
struct Platform;

fn cover_transport() -> Result<Arc<dyn Transport>, TransportError> {
    nori_core::covers::cover_transport(Duration::from_secs(30)).ok_or_else(|| TransportError::Failed { kind: FailureKind::Other, detail: Some("no transport for covers".into()) })
}

#[async_trait::async_trait]
impl Transport for Platform {
    async fn get(&self, url: String, timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        cover_transport()?.get(url, timeout_ms).await
    }

    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
        cover_transport()?.send(request).await
    }

    fn address_changed(&self) {}
}

type Covers = Loader<Bitmaps>;

fn loader<'a>(h: jlong) -> Option<&'a Covers> {
    // SAFETY: a non-zero `h` came from `open`; Kotlin never passes it after `close`.
    (h != 0).then(|| unsafe { &*(h as *const Covers) })
}

/// A loader with a disk cache in `dir` of at most `disk_bytes`. Cheap: nothing is read until the first
/// request. 0 when the Java side is missing.
extern "system" fn open(mut env: JNIEnv, _: JClass, dir: JString, disk_bytes: jlong, hardware: jboolean, rgb565: jboolean) -> jlong {
    if JAVA.get().is_none() {
        match look_up(&mut env) {
            Ok(j) => {
                let _ = JAVA.set(j);
            }
            Err(e) => {
                cleared(&mut env);
                nori_core::alog::info(&format!("covers: the Java side is missing: {e}"));
                return 0;
            }
        }
    }
    let Some(dir) = with_str(&env, &dir, |d| PathBuf::from(d)) else { return 0 };
    let config = Config { disk_bytes: disk_bytes.max(0) as u64, memory_bytes: 0, ..Config::new(dir) };
    let paint = Bitmaps { hardware: hardware != 0, rgb565: rgb565 != 0, idle: Mutex::new(Vec::new()), colours: Mutex::new(Vec::new()) };
    Box::into_raw(Box::new(Loader::with_paint(config, Arc::new(Platform), paint))) as jlong
}

/// Stops the loader after current work; pending waiters get `CLOSED`.
extern "system" fn close(_: JNIEnv, _: JClass, h: jlong) {
    if h != 0 {
        // SAFETY: `h` came from `open` and Kotlin closes it once.
        drop(unsafe { Box::from_raw(h as *mut Covers) });
    }
}

/// Requests the cover at `url` filling `width` x `height` (0 x 0: natural size, at most 2048 a side);
/// `waiter.done` is called once with the Bitmap or null and a status. Returns a ticket handle that
/// [`cancel`] must take back exactly once; 0 when there is no loader.
extern "system" fn request(env: JNIEnv, _: JClass, h: jlong, url: JString, width: jint, height: jint, waiter: JObject) -> jlong {
    let Some(loader) = loader(h) else { return 0 };
    let Ok(waiter) = env.new_global_ref(&waiter) else { return 0 };
    let (w, h) = (width.max(0) as u32, height.max(0) as u32);
    let ticket = with_str(&env, &url, |url| loader.request(url, w, h, move |r| deliver(&waiter, r)));
    ticket.map_or(0, |t| Box::into_raw(Box::new(t)) as jlong)
}

/// Releases a request's ticket. A callback already in flight may still arrive; Kotlin ignores it
/// (`CoverLoader.Request`). `@FastNative`: never waits.
extern "system" fn cancel(_: JNIEnv, _: JClass, ticket: jlong) {
    if ticket != 0 {
        // SAFETY: `ticket` came from `request` and Kotlin hands each back once.
        drop(unsafe { Box::from_raw(ticket as *mut Ticket) });
    }
}

/// `covers::is_provider_cover`, per list row: `@FastNative`, allocation-free.
extern "system" fn is_provider(env: JNIEnv, _: JClass, url: JString) -> jboolean {
    with_str(&env, &url, nori_core::covers::is_provider_cover).unwrap_or(false) as jboolean
}

/// Fetches `url` to disk without decoding (a download's covers).
extern "system" fn warm(env: JNIEnv, _: JClass, h: jlong, url: JString) {
    if let Some(loader) = loader(h) {
        with_str(&env, &url, |url| loader.warm(url));
    }
}

/// `Loader::rest`: idle threads end and reused Bitmaps and buffers are freed (low memory).
extern "system" fn rest(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(loader) = loader(h) {
        loader.rest();
    }
}

/// `Loader::show`: whether any app screen is visible; hidden, the loader rests.
extern "system" fn show(_: JNIEnv, _: JClass, h: jlong, shown: jboolean) {
    if let Some(loader) = loader(h) {
        loader.show(shown != 0);
    }
}

/// Deletes the disk cache. Off the main thread.
extern "system" fn clear(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(d) = loader(h).and_then(Loader::disk) {
        d.clear();
    }
}

/// Reusable buffers for [`colours`].
#[derive(Default)]
struct Colours {
    decoder: Decoder,
    bytes: Vec<u8>,
    rgba: Vec<u8>,
    argb: Vec<u32>,
}

/// Bits of [`colours`]'s result.
const PLAIN: jint = 1;
const WASH: jint = 2;
const BLACK: jint = 4;

/// Page colours (`look::page`) for the cover at `url`, decoded to fit `side` x `side` without a Bitmap:
/// the plain theme into `out` and `wash`, AMOLED black into `black`, each when not null (one decode for
/// both). Returns the bits written; 0 for no cover. May hit the network: off the main thread.
#[allow(clippy::too_many_arguments)]
extern "system" fn colours(env: JNIEnv, _: JClass, h: jlong, url: JString, side: jint, dark: jboolean, out: JIntArray, wash: JObject, black: JIntArray) -> jint {
    let Some(loader) = loader(h) else { return 0 };
    let mut c = loader.paint().colours.lock().pop().unwrap_or_default();
    // Unwinding out of a JNI native aborts the app.
    let answer = panic::catch_unwind(AssertUnwindSafe(|| {
        let read = with_str(&env, &url, |url| loader.read(url, &mut c.bytes).is_ok());
        if read != Some(true) {
            return 0;
        }
        let Ok(head) = nori_covers::header(&c.bytes) else { return 0 };
        let k = (side.max(1) as f64 / head.width.max(head.height) as f64).min(1.0);
        let (w, h) = (((head.width as f64 * k).round() as usize).max(1), ((head.height as f64 * k).round() as usize).max(1));
        c.rgba.reserve_exact((w * h * 4).saturating_sub(c.rgba.len()));
        c.rgba.resize(w * h * 4, 0);
        if c.decoder.decode_into(&c.bytes, Target { px: &mut c.rgba, width: w, height: h, stride: w * 4 }, Alpha::Straight).is_err() {
            return 0;
        }
        c.argb.clear();
        c.argb.reserve_exact(w * h);
        c.argb.extend(c.rgba.as_chunks::<4>().0.iter().map(|p| u32::from(p[3]) << 24 | u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])));
        let mut answer = 0;
        if !out.is_null() {
            answer |= match crate::look::page(&env, &c.argb, w, h, dark != 0, false, &out, &wash) {
                0 => 0,
                1 => PLAIN,
                _ => PLAIN | WASH,
            };
        }
        if !black.is_null() && crate::look::page(&env, &c.argb, w, h, dark != 0, true, &black, &JObject::null()) != 0 {
            answer |= BLACK;
        }
        answer
    }))
    .unwrap_or_else(|_| {
        c = Colours::default();
        0
    });
    if c.bytes.capacity() > KEEP_BYTES {
        c.bytes = Vec::new();
    }
    loader.paint().colours.lock().push(c);
    answer
}

/// Decoder state reused by [`decode_file`].
struct Kept {
    decoder: Decoder,
    rgba: Vec<u8>,
    bytes: Vec<u8>,
}

thread_local! {
    /// Thread-local: `decode_file` takes no handle, and the benchmark should not time allocations.
    static KEPT: RefCell<Kept> = RefCell::new(Kept { decoder: Decoder::new(), rgba: Vec::new(), bytes: Vec::new() });
}

/// Decodes the file at `path` into `bitmap` (mutable software ARGB_8888/RGB_565), for the debug build's
/// `coverbench`. Not on the main thread.
extern "system" fn decode_file(mut env: JNIEnv, _: JClass, path: JString, bitmap: JObject, idct: jboolean) -> jint {
    panic::catch_unwind(AssertUnwindSafe(|| decode_kept(&mut env, &path, &bitmap, idct))).unwrap_or_else(|_| {
        KEPT.replace(Kept { decoder: Decoder::new(), rgba: Vec::new(), bytes: Vec::new() });
        BROKEN
    })
}

fn decode_kept(env: &mut JNIEnv, path: &JString, bitmap: &JObject, idct: jboolean) -> jint {
    KEPT.with_borrow_mut(|kept| {
        let read = with_str(env, path, |p| {
            kept.bytes.clear();
            std::fs::File::open(p).and_then(|mut f| f.read_to_end(&mut kept.bytes)).is_ok()
        });
        if read != Some(true) {
            return UNREADABLE;
        }
        match draw(env, &mut kept.decoder, &mut kept.rgba, &kept.bytes, bitmap, idct != 0) {
            Ok(()) => OK,
            Err(Fail::Decode(DecodeError::Unknown)) => UNKNOWN,
            Err(Fail::Decode(DecodeError::Target)) | Err(Fail::Java) => BAD_BITMAP,
            Err(Fail::Decode(_)) => BROKEN,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kotlin_glue() {
        let rgba = [255, 255, 255, 255, 0, 0, 0, 255, 0xF8, 0x04, 0x08, 255, 0x07, 0xFC, 0xF7, 255];
        // 2x2 into rows padded to three pixels; padding untouched.
        let mut out = [0xAAu8; 12];
        pack_565(&rgba, 2, 2, &mut out, 6);
        let px = |i: usize| u16::from_le_bytes([out[i], out[i + 1]]);
        assert_eq!((px(0), px(2), px(4)), (0xFFFF, 0x0000, 0xAAAA));
        assert_eq!(px(6), 0b11111_000001_00001, "red's top five, green's top six, blue's top five");
        assert_eq!(px(8), 0b00000_111111_11110);
        assert_eq!(px(10), 0xAAAA);

        // Error codes for kotlin.
        assert_eq!(code(&nori_covers::Error::Decode(DecodeError::Unknown)), UNKNOWN);
        assert_eq!(code(&nori_covers::Error::Decode(DecodeError::Corrupt("x".into()))), BROKEN);
        assert_eq!(code(&nori_covers::Error::Status(404)), 1404);
        assert_eq!(code(&nori_covers::Error::Closed), CLOSED);
        let timeout = nori_covers::Error::Transport { kind: nori_core::transport::FailureKind::Timeout, detail: None };
        assert_eq!(code(&timeout), NETWORK + 4, "FailureKind's own order, which Kotlin reads its name from");
    }

}
