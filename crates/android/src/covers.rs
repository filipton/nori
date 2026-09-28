//! The app's covers: nori-covers' loader over the core's transport (the app's OkHttp, so covers ride the
//! API's connection), keeping the server's files on disk and decoding each straight into a Bitmap made at
//! the size its view draws it at. Kotlin asks with a request per view and gets one call back per cover
//! (`CoverPixels.Waiter.done`, on a loader thread; Kotlin posts it to the main thread), and cancels by
//! handing the request's handle back, which drops its ticket. Kotlin keeps the Bitmaps it has been given:
//! a Bitmap is a Java object, so its memory cache has to be there, and this loader keeps none.
//!
//! A Bitmap is software ARGB_8888, or RGB_565 for a JPEG where the app allows it (what Coil made for
//! the app before). With `hardware`, the picture is decoded into a software Bitmap kept for the next
//! cover and copied to the GPU, which is what the screens draw: the upload happens here, on a loader
//! thread, not on the first frame that draws it.
//!
//! What a loader thread keeps between covers goes when the loader rests (20 s without a cover asked for,
//! the app out of sight, memory short; see nori-covers' loader): the thread itself, with whatever the
//! graphics driver keeps for each thread that has copied a Bitmap to the GPU (4.5 MB on the emulator's),
//! and here the kept software Bitmaps and the colours' buffers.
// Bitmaps are only there on Android; elsewhere the doors build and refuse every Bitmap.
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

/// What a cover's call back says, or a door answers: 0 for a cover drawn, or what stopped it.
const OK: jint = 0;
/// Not a mutable software ARGB_8888 or RGB_565 Bitmap, or no Bitmap could be made.
const BAD_BITMAP: jint = 1;
/// The file could not be read or fetched.
const UNREADABLE: jint = 2;
/// Not a JPEG, PNG, WebP or GIF picture (a HEIF one, say).
const UNKNOWN: jint = 3;
/// A picture the decoder refused: broken, or larger than any cover.
const BROKEN: jint = 4;
/// Nobody waited for the cover any more when its file came (or the loader closed): not a failure of
/// the cover, and asking again gets it.
const CLOSED: jint = 5;
/// The request did not come back: this plus the transport's `FailureKind`, in its order.
const NETWORK: jint = 100;
/// The server answered with an error: this plus the HTTP status.
const HTTP: jint = 1000;

/// The Java side, looked up once, when the first loader is opened.
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

/// A decoded cover as Kotlin gets it: the Bitmap, and how many bytes it holds.
#[derive(Clone)]
struct Drawn {
    bitmap: GlobalRef,
    bytes: usize,
}

/// RGBA above this many bytes (a 512 x 512 cover) is given back after its picture is packed, and a kept
/// software Bitmap larger than this is not kept: the player's cover should not hold its memory until
/// the next one, and every list and grid cover fits.
const KEEP_BYTES: usize = 512 * 512 * 4;

/// What one decode borrows: the RGBA rows an RGB_565 picture is decoded into before it is packed, and
/// the software Bitmap a hardware one is decoded into before it is copied to the GPU. Lent per cover
/// being decoded, so there are only as many as the loader has threads.
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
    /// Lent per page being worked out (`colours`), as the decoders are per cover.
    colours: Mutex<Vec<Colours>>,
}

/// Why a Bitmap was not drawn: Java said no (it has thrown, or made nothing), or the decoder did.
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
                // No GPU copy (out of graphics memory, say): the software picture is drawn instead, and it
                // is the view's now, so the next cover gets a Bitmap of its own.
                _ => {
                    cleared(env);
                    scratch.drawn = None;
                    Ok(b)
                }
            }
        });
        cleared(&mut env);
        scratch.let_go(&mut env, java);
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

/// Says whether the picture has alpha. Saying a JPEG has none (as Android's own decoders do) lets drawing
/// skip blending it, and the GPU copy takes it along.
fn has_alpha(env: &mut JNIEnv, java: &Java, b: &JObject, alpha: bool) -> Result<(), Fail> {
    // SAFETY: `setHasAlpha` is Bitmap's `(boolean) -> void`.
    unsafe { env.call_method_unchecked(b, java.set_has_alpha, ReturnType::Primitive(Primitive::Void), &[JValue::Bool(alpha.into()).as_jni()]) }?;
    Ok(())
}

impl Scratch {
    /// The kept software Bitmap made `w` x `h` in `config`, in the memory it already has when that is
    /// enough; otherwise a new one.
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

    /// A player-sized Bitmap or buffer is not kept: that many bytes idle for the next list cover is a
    /// poor trade.
    fn let_go(&mut self, env: &mut JNIEnv, java: &Java) {
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

/// RGBA rows (`width` x `height`, tight) packed into RGB_565 rows `stride` bytes apart, each channel's
/// top bits, as Android's own conversion keeps them. Alpha is dropped: only an opaque cover is asked for
/// in RGB_565.
fn pack_565(rgba: &[u8], width: usize, height: usize, out: &mut [u8], stride: usize) {
    for y in 0..height {
        let from = &rgba[y * width * 4..(y + 1) * width * 4];
        let to = &mut out[y * stride..y * stride + width * 2];
        for (p, q) in from.chunks_exact(4).zip(to.chunks_exact_mut(2)) {
            let v = (u16::from(p[0]) >> 3) << 11 | (u16::from(p[1]) >> 2) << 5 | u16::from(p[2]) >> 3;
            q.copy_from_slice(&v.to_le_bytes());
        }
    }
}

/// Decodes `bytes` into `bitmap`, filling it; the IDCT shrinks big JPEGs when `idct` (see
/// `Decoder::set_idct_scaling`). An RGB_565 Bitmap's picture is decoded into `rgba` first and packed.
fn draw(env: &JNIEnv, decoder: &mut Decoder, rgba: &mut Vec<u8>, bytes: &[u8], bitmap: &JObject, idct: bool) -> Result<(), Fail> {
    #[cfg(target_os = "android")]
    {
        let Some(mut b) = crate::look::bitmap::Locked::new_or_565(env, bitmap) else { return Err(Fail::Decode(DecodeError::Target)) };
        let (width, height, stride) = (b.width, b.height, b.stride);
        decoder.set_idct_scaling(idct);
        if b.rgb565 {
            // Exactly this picture's rows, not twice the largest one's.
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

/// Hands a finished cover to its Kotlin waiter, on the loader thread that finished it.
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

/// The transport the app hands the core (`set_cover_transport`), found when a cover first reaches for the
/// network: the loader is opened from the main thread, which must not wait for the app's HTTP client to be
/// built, and is built on the thread that warms the app up.
struct Platform;

#[async_trait::async_trait]
impl Transport for Platform {
    async fn get(&self, url: String, timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        let Some(t) = nori_core::covers::cover_transport(Duration::from_secs(30)) else {
            return Err(TransportError::Failed { kind: FailureKind::Other, detail: Some("no transport for covers".into()) });
        };
        t.get(url, timeout_ms).await
    }

    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
        let Some(t) = nori_core::covers::cover_transport(Duration::from_secs(30)) else {
            return Err(TransportError::Failed { kind: FailureKind::Other, detail: Some("no transport for covers".into()) });
        };
        t.send(request).await
    }

    fn address_changed(&self) {}
}

type Covers = Loader<Bitmaps>;

fn loader<'a>(h: jlong) -> Option<&'a Covers> {
    // SAFETY: a non-zero `h` is a pointer `open` made with `Box::into_raw`, and Kotlin never passes one
    // on after `close`.
    (h != 0).then(|| unsafe { &*(h as *const Covers) })
}

/// A loader keeping covers in `dir`, at most `disk_bytes` of them, fetching through the transport the
/// app hands the core (`set_cover_transport`). Cheap: nothing is read until the first cover is asked
/// for, on a loader thread. 0 when the Java side is missing.
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
    let Some(dir) = with_str(&mut env, &dir, |d| PathBuf::from(d)) else { return 0 };
    let config = Config { disk_bytes: disk_bytes.max(0) as u64, memory_bytes: 0, ..Config::new(dir) };
    let paint = Bitmaps { hardware: hardware != 0, rgb565: rgb565 != 0, idle: Mutex::new(Vec::new()), colours: Mutex::new(Vec::new()) };
    Box::into_raw(Box::new(Loader::with_paint(config, Arc::new(Platform), paint))) as jlong
}

/// Stops the loader's threads once they finish what they are on; whoever waits is called back with
/// nothing.
extern "system" fn close(_: JNIEnv, _: JClass, h: jlong) {
    if h != 0 {
        // SAFETY: `h` came from `open` and Kotlin closes it once.
        drop(unsafe { Box::from_raw(h as *mut Covers) });
    }
}

/// Asks for the cover at `url` to fill `width` x `height` (0 x 0: at its own size, at most 2048 a side),
/// for `waiter`, whose
/// `done` is called once with it, or with null and why not. The handle is the request's: [`cancel`]
/// takes it back, once, whether the cover came or not. 0 when there is no loader.
extern "system" fn request(mut env: JNIEnv, _: JClass, h: jlong, url: JString, width: jint, height: jint, waiter: JObject) -> jlong {
    let Some(loader) = loader(h) else { return 0 };
    let Ok(waiter) = env.new_global_ref(&waiter) else { return 0 };
    let (w, h) = (width.max(0) as u32, height.max(0) as u32);
    let ticket = with_str(&mut env, &url, |url| loader.request(url, w, h, move |r| deliver(&waiter, r)));
    ticket.map_or(0, |t| Box::into_raw(Box::new(t)) as jlong)
}

/// Lets a request go: the view no longer wants its cover, or has it. Its waiter is not called for a cover
/// finished after this; one finished as it runs may still be on its way, and Kotlin drops it
/// (`CoverLoader.Request`). Waiting here for that call back would be a `@FastNative` door waiting on
/// Java. A lock and a few frees, so `@FastNative`.
extern "system" fn cancel(_: JNIEnv, _: JClass, ticket: jlong) {
    if ticket != 0 {
        // SAFETY: `ticket` came from `request` and Kotlin hands each back once.
        drop(unsafe { Box::from_raw(ticket as *mut Ticket) });
    }
}

/// Whether `url` is a provider's cover, never kept (`covers::is_provider_cover`). Asked for every cover a
/// list draws: `@FastNative`, nothing allocated; 0.2 µs against 0.5 µs and 32 bytes for the same test in
/// Kotlin.
extern "system" fn is_provider(env: JNIEnv, _: JClass, url: JString) -> jboolean {
    with_str(&env, &url, nori_core::covers::is_provider_cover).unwrap_or(false) as jboolean
}

/// Fetches the cover at `url` onto the disk without decoding it (a download's covers).
extern "system" fn warm(mut env: JNIEnv, _: JClass, h: jlong, url: JString) {
    if let Some(loader) = loader(h) {
        with_str(&mut env, &url, |url| loader.warm(url));
    }
}

/// Lets go of what the loader keeps for covers to come (`Loader::rest`): its threads end once they have
/// nothing to do, and the kept Bitmaps and buffers go. For memory running short; the next cover starts a
/// thread again.
extern "system" fn rest(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(loader) = loader(h) {
        loader.rest();
    }
}

/// Whether any of the app's screens is in sight (`Loader::show`): out of sight the loader rests, and
/// keeps no thread waiting for the next cover.
extern "system" fn show(_: JNIEnv, _: JClass, h: jlong, shown: jboolean) {
    if let Some(loader) = loader(h) {
        loader.show(shown != 0);
    }
}

/// Deletes every cover on the disk. Disk work: off the main thread.
extern "system" fn clear(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(d) = loader(h).and_then(Loader::disk) {
        d.clear();
    }
}

/// What a thread working out a page's colours keeps between covers.
#[derive(Default)]
struct Colours {
    decoder: Decoder,
    bytes: Vec<u8>,
    rgba: Vec<u8>,
    argb: Vec<u32>,
}

/// What `colours` answers, one bit for each thing it wrote.
const PLAIN: jint = 1;
const WASH: jint = 2;
const BLACK: jint = 4;

/// The pages for the cover at `url` (`look::page`), worked out in Rust from the cover's file: read from
/// the disk or fetched, decoded whole to fit `side` x `side`, straight colours, with no Bitmap in
/// between. Into `out` and `wash` the page in the plain theme and into `black` the one on AMOLED black,
/// each when it is not null: a screen that shows both (the bar black, the player in the record's
/// colours) gets them from one decode. Answers `PLAIN`, `WASH` and `BLACK` for what it wrote, 0 for no
/// cover. Waits for the network when the cover is not on the disk: off the main thread.
#[allow(clippy::too_many_arguments)]
extern "system" fn colours(mut env: JNIEnv, _: JClass, h: jlong, url: JString, side: jint, dark: jboolean, out: JIntArray, wash: JObject, black: JIntArray) -> jint {
    let Some(loader) = loader(h) else { return 0 };
    let mut c = loader.paint().colours.lock().pop().unwrap_or_default();
    // A panic unwinding out of a JNI door aborts the app: a cover that breaks the decoder is a page
    // without colours instead.
    let answer = panic::catch_unwind(AssertUnwindSafe(|| {
        let read = with_str(&mut env, &url, |url| loader.read(url, &mut c.bytes).is_ok());
        if read != Some(true) {
            return 0;
        }
        let Ok(head) = nori_covers::header(&c.bytes) else { return 0 };
        // The whole picture, shrunk to fit: a page takes its colours from the cover's own bottom rows.
        let k = (side.max(1) as f64 / head.width.max(head.height) as f64).min(1.0);
        let (w, h) = (((head.width as f64 * k).round() as usize).max(1), ((head.height as f64 * k).round() as usize).max(1));
        c.rgba.reserve_exact((w * h * 4).saturating_sub(c.rgba.len()));
        c.rgba.resize(w * h * 4, 0);
        if c.decoder.decode_into(&c.bytes, Target { px: &mut c.rgba, width: w, height: h, stride: w * 4 }, Alpha::Straight).is_err() {
            return 0;
        }
        c.argb.clear();
        c.argb.reserve_exact(w * h);
        c.argb.extend(c.rgba.chunks_exact(4).map(|p| u32::from(p[3]) << 24 | u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])));
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
        // Its buffers may be anywhere mid-picture.
        c = Colours::default();
        0
    });
    if c.bytes.capacity() > KEEP_BYTES {
        c.bytes = Vec::new();
    }
    loader.paint().colours.lock().push(c);
    answer
}

/// What the benchmark's thread keeps between covers: a decoder and the file's bytes.
struct Kept {
    decoder: Decoder,
    rgba: Vec<u8>,
    bytes: Vec<u8>,
}

thread_local! {
    static KEPT: RefCell<Kept> = RefCell::new(Kept { decoder: Decoder::new(), rgba: Vec::new(), bytes: Vec::new() });
}

/// The picture in the file at `path`, into `bitmap` (a mutable software ARGB_8888 or RGB_565 one), at
/// its size: the decode alone, for the debug build's `coverbench`. A read and a decode, so a plain JNI
/// call, never on the main thread.
extern "system" fn decode_file(mut env: JNIEnv, _: JClass, path: JString, bitmap: JObject, idct: jboolean) -> jint {
    // As in `colours`: a panic is this file's failure, not the app's end.
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
    fn rgba_is_packed_into_565_rows_by_each_channels_top_bits() {
        let rgba = [255, 255, 255, 255, 0, 0, 0, 255, 0xF8, 0x04, 0x08, 255, 0x07, 0xFC, 0xF7, 255];
        // Two rows of two, into rows padded to three pixels; the padding is left alone.
        let mut out = [0xAAu8; 12];
        pack_565(&rgba, 2, 2, &mut out, 6);
        let px = |i: usize| u16::from_le_bytes([out[i], out[i + 1]]);
        assert_eq!((px(0), px(2), px(4)), (0xFFFF, 0x0000, 0xAAAA));
        assert_eq!(px(6), 0b11111_000001_00001, "red's top five, green's top six, blue's top five");
        assert_eq!(px(8), 0b00000_111111_11110);
        assert_eq!(px(10), 0xAAAA);
    }

    #[test]
    fn a_failure_is_told_to_kotlin_as_what_stopped_it() {
        assert_eq!(code(&nori_covers::Error::Decode(DecodeError::Unknown)), UNKNOWN);
        assert_eq!(code(&nori_covers::Error::Decode(DecodeError::Corrupt("x".into()))), BROKEN);
        assert_eq!(code(&nori_covers::Error::Status(404)), 1404);
        assert_eq!(code(&nori_covers::Error::Closed), CLOSED);
        let timeout = nori_covers::Error::Transport { kind: nori_core::transport::FailureKind::Timeout, detail: None };
        assert_eq!(code(&timeout), NETWORK + 4, "FailureKind's own order, which Kotlin reads its name from");
    }
}
