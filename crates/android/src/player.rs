//! The Android player: nori-engine playing the core's queue into an AudioTrack (track.rs). Kotlin's
//! `EnginePlayer` is a media3 player over these natives.
//!
//! Platform pieces come from Kotlin's `RustBridge`: AudioTracks (`openTrack`, `openOffload`), song bytes
//! through media3's data sources and caches (`open`/`read`, `openLive` for radio), stream cache queries for
//! fetching ahead (`kept`, `busy`), the event wake-up (`signal`), the wake lock (`cpu`) and offload support
//! (`offloadSupport`). Classes and methods are looked up once, in `create`, on a thread that sees the app's
//! classes; Rust threads calling them stay attached for life.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::Thread;
use std::time::Instant;

use jni::objects::{GlobalRef, JByteArray, JClass, JFieldID, JMethodID, JObject, JStaticMethodID, JString, JValue, JValueOwned};
use jni::signature::{Primitive, ReturnType};
use jni::sys::{jboolean, jfloat, jint, jlong, jstring, jvalue};
use jni::{JNIEnv, JavaVM};
use nori_engine::ahead::{Ahead, Entry, Keeping};
use nori_engine::arriving::Listening;
use nori_engine::core::{ahead_songs, is_radio, key_format, measure_as_it_comes, measuring_ahead, settings, CoreApp, CoreQueue, OutputVolume};
use nori_engine::{Body, ByteSource, Cancel, Coded, Coding, Config, Device, Engine, Event, Library, Located, OffloadOutput, OpenError, OutputFacts, OutputFormat, Source, State, Support};
use nori_player::transitions::WindowSong;
use parking_lot::Mutex;

use crate::track::{mono_ns, packed24, sample_bytes, HeadCount, Opened, Opener, Route, Shared, Sink, TrackOutput, CHUNK_BYTES};
use crate::{cleared, java_string, native, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/RustPlayerJni",
    methods: &[
        native!(c"create", c"(IZI)J", create),
        native!(c"destroy", c"(J)V", destroy),
        native!(c"goTo", c"(JIJ)J", go_to),
        native!(c"pauseAtEnd", c"(JZ)V", pause_at_end),
        native!(c"play", c"(J)V", play),
        native!(c"pause", c"(J)V", pause),
        native!(c"pauseNow", c"(J)V", pause_now),
        native!(c"queueChanged", c"(J)V", queue_changed),
        native!(c"setRepeat", c"(JI)V", set_repeat),
        native!(c"replan", c"(J)V", replan),
        native!(c"gainChanged", c"(J)V", gain_changed),
        native!(c"setTuning", c"(JZ)V", set_tuning),
        native!(c"applySettings", c"(J)V", apply_settings),
        native!(c"positionMs", c"(J)J", position_ms),
        native!(c"shownMs", c"(JI)J", shown_ms),
        native!(c"look", c"(J)V", look),
        native!(c"mixing", c"(J)Z", mixing),
        native!(c"chainIn", c"(J)Z", chain_in),
        native!(c"onCpu", c"(J)Z", on_cpu),
        native!(c"gainReductionDb", c"(J)F", gain_reduction_db),
        native!(c"compressionDb", c"(J)F", compression_db),
        native!(c"setVolume", c"(JIIF)V", set_volume),
        native!(c"bytesWritten", c"(J)J", bytes_written),
        native!(c"event", c"(J)J", event),
        native!(c"eventText", c"(J)Ljava/lang/String;", event_text),
        native!(c"eventJumps", c"(J)J", event_jumps),
        native!(c"device", c"(JILjava/lang/String;)V", device),
        native!(c"setOutput", c"(JZZ)V", set_output),
        native!(c"offloadEvent", c"(JI)V", offload_event),
        native!(c"offloaded", c"(J)Z", offloaded),
        native!(c"offloadWanted", c"(J)Z", offload_wanted),
        native!(c"pcmWhy", c"(J)Ljava/lang/String;", pcm_why),
        native!(c"radio", c"(JLjava/lang/String;Ljava/lang/String;)V", radio),
    ],
};

/// Java classes, methods and fields, looked up once.
struct Java {
    vm: JavaVM,
    bridge: GlobalRef,
    open_track: JStaticMethodID,
    open: JStaticMethodID,
    cancel: JStaticMethodID,
    open_live: JStaticMethodID,
    kept: JStaticMethodID,
    busy: JStaticMethodID,
    disk: JStaticMethodID,
    forget: JStaticMethodID,
    signal: JStaticMethodID,
    cpu: JStaticMethodID,
    offload_support: JStaticMethodID,
    open_offload: JStaticMethodID,
    body_read: JMethodID,
    body_close: JMethodID,
    body_buffer: JFieldID,
    body_length: JFieldID,
    body_icy: JFieldID,
    body_past: JFieldID,
    body_status: JFieldID,
    position: JMethodID,
    track: TrackMethods,
    /// Android 10+.
    offload: Option<OffloadMethods>,
    timestamp: GlobalRef,
    timestamp_new: JMethodID,
    frame_position: JFieldID,
    nano_time: JFieldID,
}

struct OffloadMethods {
    delay_padding: JMethodID,
    end_of_stream: JMethodID,
}

struct TrackMethods {
    write: JMethodID,
    play: JMethodID,
    pause: JMethodID,
    flush: JMethodID,
    stop: JMethodID,
    set_volume: JMethodID,
    get_timestamp: JMethodID,
    head: JMethodID,
    release: JMethodID,
    buffer_frames: JMethodID,
    set_buffer_frames: JMethodID,
    /// `setStartThresholdInFrames`, Android 12+.
    set_start_threshold: Option<JMethodID>,
    underruns: JMethodID,
    capacity_frames: JMethodID,
    session: JMethodID,
    routed_device: JMethodID,
    min_buffer_size: JStaticMethodID,
    /// Hidden `getLatency()` (on the greylist, as ExoPlayer uses it); None where refused.
    latency: Option<JMethodID>,
    /// `AudioDeviceInfo.getType()`.
    device_type: JMethodID,
    class: GlobalRef,
}

/// Global: natives other than `create` and the engine's callbacks have no handle to reach it through.
static JAVA: OnceLock<Java> = OnceLock::new();

fn look_up(env: &mut JNIEnv) -> jni::errors::Result<Java> {
    let bridge = env.find_class("dev/nori/music/playback/RustBridge")?;
    let body = env.find_class("dev/nori/music/playback/RustBody")?;
    let track = env.find_class("android/media/AudioTrack")?;
    let buffer = env.find_class("java/nio/Buffer")?;
    let timestamp = env.find_class("android/media/AudioTimestamp")?;
    // Optional by API level: each lookup may throw.
    let mut optional = |name: &str, sig: &str| {
        let m = env.get_method_id(&track, name, sig).ok();
        cleared(env);
        m
    };
    let delay_padding = optional("setOffloadDelayPadding", "(II)V");
    let end_of_stream = optional("setOffloadEndOfStream", "()V");
    let start_threshold = optional("setStartThresholdInFrames", "(I)I");
    let latency = optional("getLatency", "()I");
    let device_info = env.find_class("android/media/AudioDeviceInfo")?;
    let offload = delay_padding.zip(end_of_stream).map(|(delay_padding, end_of_stream)| OffloadMethods { delay_padding, end_of_stream });
    Ok(Java {
        vm: env.get_java_vm()?,
        open_track: env.get_static_method_id(&bridge, "openTrack", "(IIIII)Landroid/media/AudioTrack;")?,
        open: env.get_static_method_id(&bridge, "open", "(Ljava/lang/String;Ljava/lang/String;JJ)Ldev/nori/music/playback/RustBody;")?,
        cancel: env.get_static_method_id(&bridge, "cancel", "(J)V")?,
        open_live: env.get_static_method_id(&bridge, "openLive", "(Ljava/lang/String;)Ldev/nori/music/playback/RustBody;")?,
        kept: env.get_static_method_id(&bridge, "kept", "(Ljava/lang/String;)Z")?,
        busy: env.get_static_method_id(&bridge, "busy", "(Ljava/lang/String;)Z")?,
        disk: env.get_static_method_id(&bridge, "disk", "(Ljava/lang/String;)Ljava/lang/String;")?,
        forget: env.get_static_method_id(&bridge, "forget", "(Ljava/lang/String;)Ljava/lang/String;")?,
        signal: env.get_static_method_id(&bridge, "signal", "()Z")?,
        cpu: env.get_static_method_id(&bridge, "cpu", "(Z)V")?,
        offload_support: env.get_static_method_id(&bridge, "offloadSupport", "(III)I")?,
        open_offload: env.get_static_method_id(&bridge, "openOffload", "(IIII)Landroid/media/AudioTrack;")?,
        bridge: env.new_global_ref(&bridge)?,
        body_read: env.get_method_id(&body, "read", "(I)I")?,
        body_close: env.get_method_id(&body, "close", "()V")?,
        body_buffer: env.get_field_id(&body, "buffer", "[B")?,
        body_length: env.get_field_id(&body, "length", "J")?,
        body_icy: env.get_field_id(&body, "icy", "I")?,
        body_past: env.get_field_id(&body, "past", "Z")?,
        body_status: env.get_field_id(&body, "status", "I")?,
        position: env.get_method_id(&buffer, "position", "(I)Ljava/nio/Buffer;")?,
        offload,
        track: TrackMethods {
            write: env.get_method_id(&track, "write", "(Ljava/nio/ByteBuffer;II)I")?,
            play: env.get_method_id(&track, "play", "()V")?,
            pause: env.get_method_id(&track, "pause", "()V")?,
            flush: env.get_method_id(&track, "flush", "()V")?,
            stop: env.get_method_id(&track, "stop", "()V")?,
            set_volume: env.get_method_id(&track, "setVolume", "(F)I")?,
            get_timestamp: env.get_method_id(&track, "getTimestamp", "(Landroid/media/AudioTimestamp;)Z")?,
            head: env.get_method_id(&track, "getPlaybackHeadPosition", "()I")?,
            release: env.get_method_id(&track, "release", "()V")?,
            buffer_frames: env.get_method_id(&track, "getBufferSizeInFrames", "()I")?,
            set_buffer_frames: env.get_method_id(&track, "setBufferSizeInFrames", "(I)I")?,
            set_start_threshold: start_threshold,
            underruns: env.get_method_id(&track, "getUnderrunCount", "()I")?,
            capacity_frames: env.get_method_id(&track, "getBufferCapacityInFrames", "()I")?,
            session: env.get_method_id(&track, "getAudioSessionId", "()I")?,
            routed_device: env.get_method_id(&track, "getRoutedDevice", "()Landroid/media/AudioDeviceInfo;")?,
            min_buffer_size: env.get_static_method_id(&track, "getMinBufferSize", "(III)I")?,
            latency,
            device_type: env.get_method_id(&device_info, "getType", "()I")?,
            class: env.new_global_ref(&track)?,
        },
        timestamp_new: env.get_method_id(&timestamp, "<init>", "()V")?,
        frame_position: env.get_field_id(&timestamp, "framePosition", "J")?,
        nano_time: env.get_field_id(&timestamp, "nanoTime", "J")?,
        timestamp: env.new_global_ref(&timestamp)?,
    })
}

/// This thread's JNIEnv, attached for life.
fn env() -> Option<(&'static Java, JNIEnv<'static>)> {
    let java = JAVA.get()?;
    let env = crate::attached(&java.vm)?;
    Some((java, env))
}

fn log(message: &str) {
    nori_core::alog::info(&format!("rust player: {message}"));
}

fn bridge(java: &Java) -> &JClass<'static> {
    <&JClass>::from(java.bridge.as_obj())
}

// Every ID in `Java` was looked up in `look_up` on the class of the object it is used with, with the
// signature its call site passes (return type and arguments). The helpers below rely on that.

/// Calls instance method `m`, clearing any exception; None if it threw.
fn call<'l>(env: &mut JNIEnv<'l>, obj: &JObject, m: JMethodID, ret: ReturnType, args: &[jvalue]) -> Option<JValueOwned<'l>> {
    // SAFETY: `m` belongs to `obj`'s class and `ret`/`args` match its signature (see above).
    let v = unsafe { env.call_method_unchecked(obj, m, ret, args) };
    cleared(env);
    v.ok()
}

fn call_int(env: &mut JNIEnv, obj: &JObject, m: JMethodID, args: &[jvalue]) -> Option<i32> {
    call(env, obj, m, ReturnType::Primitive(Primitive::Int), args)?.i().ok()
}

fn call_bool(env: &mut JNIEnv, obj: &JObject, m: JMethodID, args: &[jvalue]) -> Option<bool> {
    call(env, obj, m, ReturnType::Primitive(Primitive::Boolean), args)?.z().ok()
}

/// True unless it threw.
fn call_void(env: &mut JNIEnv, obj: &JObject, m: JMethodID, args: &[jvalue]) -> bool {
    call(env, obj, m, ReturnType::Primitive(Primitive::Void), args).is_some()
}

/// Calls static method `m` of `class`, clearing any exception; None if it threw.
fn call_static<'l>(env: &mut JNIEnv<'l>, class: &JClass, m: JStaticMethodID, ret: ReturnType, args: &[jvalue]) -> Option<JValueOwned<'l>> {
    // SAFETY: `m` belongs to `class` and `ret`/`args` match its signature (see above).
    let v = unsafe { env.call_static_method_unchecked(class, m, ret, args) };
    cleared(env);
    v.ok()
}

/// `AudioTrack.write(ByteBuffer, len, WRITE_NON_BLOCKING)` of `buffer` from byte `from`: bytes taken, or
/// the error code (a throw counts as `ERROR_DEAD_OBJECT`).
fn write_direct(env: &mut JNIEnv, java: &Java, track: &JObject, buffer: &JObject, from: i32, len: i32) -> Result<usize, i32> {
    if let Some(b) = call(env, buffer, java.position, ReturnType::Object, &[JValue::Int(from).as_jni()]).and_then(|v| v.l().ok()) {
        let _ = env.delete_local_ref(b);
    }
    let args = [JValue::Object(buffer).as_jni(), JValue::Int(len).as_jni(), JValue::Int(WRITE_NON_BLOCKING).as_jni()];
    match call_int(env, track, java.track.write, &args).unwrap_or(ERROR_DEAD_OBJECT) {
        n if n >= 0 => Ok(n as usize),
        code => Err(code),
    }
}

/// `AudioTrack.getTimestamp` into `stamp`: (frames presented, CLOCK_MONOTONIC ns).
fn read_timestamp(env: &mut JNIEnv, java: &Java, track: &JObject, stamp: &JObject) -> Option<(i64, i64)> {
    if !call_bool(env, track, java.track.get_timestamp, &[JValue::Object(stamp).as_jni()])? {
        return None;
    }
    let mut long = |f| env.get_field_unchecked(stamp, f, ReturnType::Primitive(Primitive::Long)).and_then(|v| v.j());
    let read = long(java.frame_position).and_then(|frames| Ok((frames, long(java.nano_time)?)));
    cleared(env);
    read.ok()
}

fn set_volume_on(env: &mut JNIEnv, java: &Java, track: &JObject, volume: f32) {
    // The perf build's self test plays quieter.
    let volume = volume * nori_perf::invariants::quiet();
    call(env, track, java.track.set_volume, ReturnType::Primitive(Primitive::Int), &[JValue::Float(volume).as_jni()]);
}

// ---- AudioTrack ----

/// An AudioTrack opened by Kotlin, written through a direct ByteBuffer over `staging`.
struct JavaTrack {
    track: GlobalRef,
    buffer: GlobalRef,
    staging: Vec<f32>,
    timestamp: GlobalRef,
    /// The 32-bit play head, unwrapped.
    head: HeadCount,
    /// Last flush or start: an older timestamp describes music that is gone.
    since_ns: i64,
    released: bool,
    rate: u32,
    channels: usize,
    /// `AudioFormat.ENCODING_*`.
    encoding: i32,
}

/// Releases a track never released explicitly (writer thread failed or panicked): it holds a mixer slot.
impl Drop for JavaTrack {
    fn drop(&mut self) {
        if !self.released {
            self.release();
        }
    }
}

impl JavaTrack {
    fn void(&mut self, m: impl FnOnce(&TrackMethods) -> JMethodID) {
        let Some((java, mut env)) = env() else { return };
        call_void(&mut env, &self.track, m(&java.track), &[]);
    }

    fn int(&self, m: impl FnOnce(&TrackMethods) -> JMethodID) -> Option<i32> {
        let (java, mut env) = env()?;
        call_int(&mut env, &self.track, m(&java.track), &[])
    }
}

impl Sink for JavaTrack {
    fn staging(&mut self) -> &mut [f32] {
        &mut self.staging
    }

    fn write(&mut self, from: usize, len: usize) -> Result<usize, i32> {
        let Some((java, mut env)) = env() else { return Ok(0) };
        let (Ok(from), Ok(len)) = (i32::try_from(from), i32::try_from(len)) else { return Ok(0) };
        write_direct(&mut env, java, &self.track, &self.buffer, from, len)
    }

    fn play(&mut self) {
        self.void(|t| t.play);
        self.since_ns = mono_ns();
    }

    fn pause(&mut self) {
        self.void(|t| t.pause);
    }

    fn flush(&mut self) {
        self.void(|t| t.flush);
        self.head = HeadCount::default();
        self.since_ns = mono_ns();
    }

    fn stop(&mut self) {
        self.void(|t| t.stop);
    }

    fn set_volume(&mut self, volume: f32) {
        let Some((java, mut env)) = env() else { return };
        set_volume_on(&mut env, java, &self.track, volume);
    }

    /// Playing: the device's timestamp when it has a fresh one; otherwise the play head, now.
    fn heard(&mut self, playing: bool) -> Option<(u64, i64)> {
        if playing {
            if let Some(stamp) = self.stamp() {
                return Some(stamp);
            }
        }
        let (java, mut env) = env()?;
        let head = call_int(&mut env, &self.track, java.track.head, &[])?;
        Some((self.head.read(head as u32), mono_ns()))
    }

    /// A timestamp from before the last flush or start describes music that is gone.
    fn stamp(&mut self) -> Option<(u64, i64)> {
        let (java, mut env) = env()?;
        let (frames, ns) = read_timestamp(&mut env, java, &self.track, &self.timestamp).filter(|&(_, ns)| ns >= self.since_ns)?;
        Some((frames.max(0) as u64, ns))
    }

    fn session(&mut self) -> i32 {
        self.int(|t| t.session).unwrap_or(0)
    }

    /// `setBufferSizeInFrames`, then the start threshold (Android 12+) set to a quarter second or the
    /// whole new size if smaller, so a flush while shallow does not wait for more than fits.
    fn resize(&mut self, frames: u64) -> u64 {
        let Some((java, mut env)) = env() else { return frames };
        let asked = frames.min(i32::MAX as u64) as i32;
        let given = match call_int(&mut env, &self.track, java.track.set_buffer_frames, &[JValue::Int(asked).as_jni()]) {
            Some(n) if n > 0 => n,
            other => {
                log(&format!("the AudioTrack kept its size: setBufferSizeInFrames({asked}) answered {other:?}"));
                return call_int(&mut env, &self.track, java.track.buffer_frames, &[]).unwrap_or(asked).max(1) as u64;
            }
        };
        if let Some(m) = java.track.set_start_threshold {
            let threshold = (self.rate as i32 / 4).min(given).max(1);
            call_int(&mut env, &self.track, m, &[JValue::Int(threshold).as_jni()]);
        }
        given as u64
    }

    /// Min buffer for this format on the current route, the route's latency past the track, and its kind.
    fn route(&mut self) -> Route {
        let Some((java, mut env)) = env() else { return Route::default() };
        let mask = if self.channels == 1 { CHANNEL_OUT_MONO } else { CHANNEL_OUT_STEREO };
        let args = [JValue::Int(self.rate as i32).as_jni(), JValue::Int(mask).as_jni(), JValue::Int(self.encoding).as_jni()];
        let class = <&JClass>::from(java.track.class.as_obj());
        let min_bytes = call_static(&mut env, class, java.track.min_buffer_size, ReturnType::Primitive(Primitive::Int), &args).and_then(|v| v.i().ok());
        let frame = self.channels * sample_bytes(self.encoding == ENCODING_PCM_FLOAT, self.encoding == ENCODING_PCM_24BIT_PACKED);
        let min_frames = min_bytes.filter(|&b| b > 0).map(|b| b as u64 / frame.max(1) as u64);
        // getLatency() is the output's latency plus the whole track buffer, in ms.
        let latency_ms = java.track.latency.and_then(|m| call_int(&mut env, &self.track, m, &[]));
        let latency_frames = match (latency_ms, call_int(&mut env, &self.track, java.track.capacity_frames, &[])) {
            (Some(ms), Some(cap)) if ms > 0 && cap > 0 => Some((ms as i64 - cap as i64 * 1000 / self.rate.max(1) as i64).max(0) as u64 * self.rate as u64 / 1000),
            _ => None,
        };
        let device = call(&mut env, &self.track, java.track.routed_device, ReturnType::Object, &[]).and_then(|v| v.l().ok());
        let name = device.filter(|d| !d.is_null()).and_then(|d| {
            let kind = call_int(&mut env, &d, java.track.device_type, &[]);
            let _ = env.delete_local_ref(d);
            kind.map(device_name)
        });
        Route { min_frames, latency_frames, name }
    }

    fn underruns(&mut self) -> Option<u64> {
        self.int(|t| t.underruns).filter(|&n| n >= 0).map(|n| n as u64)
    }

    fn consumed(&mut self) -> Option<u64> {
        let head = self.int(|t| t.head)?;
        Some(self.head.read(head as u32))
    }

    fn release(&mut self) {
        self.void(|t| t.release);
        self.released = true;
    }
}

/// `AudioFormat.CHANNEL_OUT_MONO` and `CHANNEL_OUT_STEREO`.
const CHANNEL_OUT_MONO: i32 = 4;
const CHANNEL_OUT_STEREO: i32 = 12;

/// An `AudioDeviceInfo.TYPE_*` for the log.
fn device_name(kind: i32) -> &'static str {
    match kind {
        1 => "the earpiece",
        2 => "the phone speaker",
        3..=5 => "wired headphones",
        7 | 8 | 26 | 27 | 30 => "Bluetooth",
        23 => "a hearing aid",
        9 | 10 => "HDMI",
        11 | 12 | 22 => "USB",
        _ => "this output",
    }
}

/// `AudioTrack.WRITE_NON_BLOCKING`.
const WRITE_NON_BLOCKING: i32 = 1;
/// `AudioTrack.ERROR_DEAD_OBJECT`.
const ERROR_DEAD_OBJECT: i32 = -6;
/// `AudioFormat.ENCODING_*`.
const ENCODING_PCM_16BIT: i32 = 2;
const ENCODING_PCM_FLOAT: i32 = 4;
const ENCODING_PCM_24BIT_PACKED: i32 = 21;
const ENCODING_MP3: i32 = 9;
const ENCODING_AAC_LC: i32 = 10;
const ENCODING_OPUS: i32 = 20;

// ---- offloaded AudioTrack ----

/// The offloaded track's `StreamEventCallback` flags (set by [`offload_event`]) and the engine thread to wake.
#[derive(Default)]
struct OffloadEvents {
    /// `onDataRequest` since the engine last asked.
    wants: AtomicBool,
    /// `onPresentationEnded`.
    ended: AtomicBool,
    /// `onTearDown`.
    torn: AtomicBool,
    engine: Mutex<Option<Thread>>,
}

impl OffloadEvents {
    fn wake(&self) {
        if let Some(t) = &*self.engine.lock() {
            t.unpark();
        }
    }
}

/// Most bytes one write to the offloaded track moves.
const OFFLOAD_CHUNK: usize = 320 * 1024;

/// An AudioTrack opened for offload, fed compressed packets from the engine thread through a direct
/// ByteBuffer over `staging`.
struct JavaOffload {
    events: Arc<OffloadEvents>,
    track: Option<GlobalRef>,
    buffer: Option<GlobalRef>,
    staging: Vec<u8>,
    /// The open track's buffer size in bytes: no write moves more.
    held: usize,
    rate: u32,
    timestamp: Option<GlobalRef>,
    /// Last timestamp (frames presented, ns) and whether the platform gave it since playback last
    /// started; only a fresh one is extrapolated by the clock.
    stamp: Option<(u64, i64, bool)>,
    playing: bool,
    /// Last open, flush or play: older timestamps are ignored.
    since_ns: i64,
    /// The platform's answer for each format asked about, for the log.
    said: Vec<(Coded, String)>,
}

impl JavaOffload {
    fn new(events: Arc<OffloadEvents>) -> JavaOffload {
        JavaOffload { events, track: None, buffer: None, staging: Vec::new(), held: 0, rate: 1, timestamp: None, stamp: None, playing: false, since_ns: 0, said: Vec::new() }
    }

    /// The last timestamp, extrapolated by the clock while playing and fresh.
    fn stamped_now(&self) -> Option<u64> {
        let (frames, ns, fresh) = self.stamp?;
        if !self.playing || !fresh {
            return Some(frames);
        }
        let run = (mono_ns() - ns).max(0) as u128 * self.rate as u128 / 1_000_000_000;
        Some(frames + run as u64)
    }

    fn void(&mut self, m: impl FnOnce(&Java) -> JMethodID) {
        let Some(track) = &self.track else { return };
        let Some((java, mut env)) = env() else { return };
        call_void(&mut env, track, m(java), &[]);
    }
}

fn encoding_of(c: Coding) -> i32 {
    match c {
        Coding::Mp3 => ENCODING_MP3,
        Coding::Aac => ENCODING_AAC_LC,
        Coding::Opus => ENCODING_OPUS,
    }
}

/// Decodes `RustBridge.offloadSupport`'s answer as media3 1.11 reads the platform
/// (`DefaultAudioOffloadSupportProvider`). High byte: the API used (3 `getDirectPlaybackSupport`,
/// Android 13+; 2 `getPlaybackOffloadSupport`, 12; 1 `isOffloadedPlaybackSupported`, 10-11); low byte: its
/// answer; -1: not asked. Gapless only from 13 on, as in media3 (b/191950723).
fn offload_support(answer: i32) -> (Support, String) {
    if answer < 0 {
        return (Support::No, "the platform could not be asked".into());
    }
    let (call, v) = (answer >> 8, answer & 0xFF);
    match call {
        3 => {
            let (support, name) = match v & 3 {
                3 => (Support::Gapless, "OFFLOAD_GAPLESS_SUPPORTED"),
                1 => (Support::Plain, "OFFLOAD_SUPPORTED"),
                _ => (Support::No, "NOT_SUPPORTED"),
            };
            let bitstream = if v & 4 != 0 { " | BITSTREAM_SUPPORTED" } else { "" };
            (support, format!("getDirectPlaybackSupport: {name}{bitstream}"))
        }
        2 => match v {
            0 => (Support::No, "getPlaybackOffloadSupport: NOT_SUPPORTED".into()),
            2 => (Support::Plain, "getPlaybackOffloadSupport: GAPLESS_SUPPORTED, taken as without gaps before Android 13".into()),
            _ => (Support::Plain, "getPlaybackOffloadSupport: SUPPORTED".into()),
        },
        _ if v != 0 => (Support::Plain, "isOffloadedPlaybackSupported: true, which says nothing of gaps".into()),
        _ => (Support::No, "isOffloadedPlaybackSupported: false".into()),
    }
}

impl OffloadOutput for JavaOffload {
    fn supports(&mut self, coded: Coded) -> Support {
        let Some((java, mut env)) = env() else { return Support::No };
        let args = [JValue::Int(encoding_of(coded.coding)).as_jni(), JValue::Int(coded.rate as i32).as_jni(), JValue::Int(coded.channels as i32).as_jni()];
        let answer = call_static(&mut env, bridge(java), java.offload_support, ReturnType::Primitive(Primitive::Int), &args).and_then(|v| v.i().ok()).unwrap_or(-1);
        let (s, words) = offload_support(answer);
        log(&format!("offload of {} at {} Hz x{}: {words}: {s:?}", coded.coding.name(), coded.rate, coded.channels));
        self.said.retain(|(c, _)| *c != coded);
        self.said.push((coded, words));
        s
    }

    fn said(&mut self, coded: Coded) -> Option<String> {
        self.said.iter().find(|(c, _)| *c == coded).map(|(_, w)| w.clone())
    }

    fn open(&mut self, coded: Coded, bytes: usize) -> Result<usize, String> {
        self.close();
        let (java, mut env) = env().ok_or("no JVM")?;
        if java.offload.is_none() {
            return Err("no offload before Android 10".into());
        }
        self.staging = vec![0u8; OFFLOAD_CHUNK];
        let staging = self.staging.as_mut_ptr();
        let opened = env.with_local_frame(4, |env| -> jni::errors::Result<Option<(GlobalRef, GlobalRef, usize)>> {
            let args = [
                JValue::Int(encoding_of(coded.coding)).as_jni(),
                JValue::Int(coded.rate as i32).as_jni(),
                JValue::Int(coded.channels as i32).as_jni(),
                JValue::Int(bytes.min(i32::MAX as usize) as i32).as_jni(),
            ];
            // SAFETY: RustBridge.openOffload(int, int, int, int), looked up with this signature.
            let track = unsafe { env.call_static_method_unchecked(bridge(java), java.open_offload, ReturnType::Object, &args) }?.l()?;
            if track.is_null() {
                return Ok(None);
            }
            // A compressed track counts its buffer in bytes.
            let held = call_int(env, &track, java.track.buffer_frames, &[]).unwrap_or(0).max(0) as usize;
            // SAFETY: `staging` is kept, never resized, for as long as the buffer lives.
            let buffer = unsafe { env.new_direct_byte_buffer(staging, OFFLOAD_CHUNK) }?;
            Ok(Some((env.new_global_ref(&track)?, env.new_global_ref(&buffer)?, held)))
        });
        cleared(&mut env);
        let Ok(Some((track, buffer, held))) = opened else { return Err("the offloaded AudioTrack would not open".into()) };
        self.track = Some(track);
        self.buffer = Some(buffer);
        self.rate = coded.rate.max(1);
        self.stamp = None;
        self.playing = false;
        self.since_ns = mono_ns();
        if self.timestamp.is_none() {
            // SAFETY: AudioTimestamp's no-argument constructor.
            let made = unsafe { env.new_object_unchecked(<&JClass>::from(java.timestamp.as_obj()), java.timestamp_new, &[]) };
            self.timestamp = made.and_then(|t| env.new_global_ref(t)).ok();
            cleared(&mut env);
        }
        self.events.torn.store(false, Ordering::Release);
        self.events.ended.store(false, Ordering::Release);
        self.events.wants.store(false, Ordering::Release);
        *self.events.engine.lock() = Some(std::thread::current());
        self.held = if held > 0 { held } else { bytes };
        log(&format!("offloaded {:?} track: {} KB of the {} KB asked", coded.coding, self.held / 1024, bytes / 1024));
        Ok(self.held)
    }

    fn write(&mut self, data: &[u8], _frames: u64) -> Result<usize, i32> {
        let (Some(track), Some(buffer)) = (&self.track, &self.buffer) else { return Err(ERROR_DEAD_OBJECT) };
        let Some((java, mut env)) = env() else { return Ok(0) };
        let mut done = 0;
        while done < data.len() {
            let n = (data.len() - done).min(self.staging.len()).min(self.held.max(1));
            self.staging[..n].copy_from_slice(&data[done..done + n]);
            let k = match write_direct(&mut env, java, track, buffer, 0, n as i32) {
                Ok(k) => k.min(n),
                // Bytes taken before the error count; the error surfaces on the next write.
                Err(_) if done > 0 => return Ok(done),
                Err(code) => return Err(code),
            };
            done += k;
            if k < n {
                break;
            }
        }
        Ok(done)
    }

    fn delay_padding(&mut self, delay: u32, padding: u32) {
        let (Some(track), Some((java, mut env))) = (&self.track, env()) else { return };
        let Some(m) = java.offload.as_ref().map(|o| o.delay_padding) else { return };
        let args = [JValue::Int(delay.min(i32::MAX as u32) as i32).as_jni(), JValue::Int(padding.min(i32::MAX as u32) as i32).as_jni()];
        call_void(&mut env, track, m, &args);
    }

    /// `setOffloadEndOfStream` throws unless playing: false then, and the engine retries once it plays.
    fn end_of_stream(&mut self) -> bool {
        // A previous end of stream's presentation is not this one's.
        self.events.ended.store(false, Ordering::Release);
        let (Some(track), Some((java, mut env))) = (&self.track, env()) else { return false };
        let Some(m) = java.offload.as_ref().map(|o| o.end_of_stream) else { return false };
        call_void(&mut env, track, m, &[])
    }

    fn play(&mut self) {
        self.void(|j| j.track.play);
        // Hold the paused position until the platform gives a timestamp for this play.
        self.stamp = self.stamped_now().map(|f| (f, mono_ns(), false));
        self.playing = true;
        self.since_ns = mono_ns();
    }

    fn pause(&mut self) {
        self.void(|j| j.track.pause);
        self.stamp = self.stamped_now().map(|f| (f, mono_ns(), false));
        self.playing = false;
    }

    fn flush(&mut self) {
        self.events.ended.store(false, Ordering::Release);
        self.void(|j| j.track.flush);
        self.stamp = None;
        self.since_ns = mono_ns();
    }

    fn set_volume(&mut self, volume: f32) {
        let (Some(track), Some((java, mut env))) = (&self.track, env()) else { return };
        set_volume_on(&mut env, java, track, volume);
    }

    /// Frames presented since open or flush (restarts at a gapless join). None when the call failed, which
    /// the engine must not read as 0.
    fn head(&mut self) -> Option<u64> {
        let (Some(track), Some((java, mut env))) = (&self.track, env()) else { return None };
        call_int(&mut env, track, java.track.head, &[]).map(|h| h as u32 as u64)
    }

    /// `getTimestamp`, which works where an offloaded play head does not (Galaxy S22), extrapolated by the
    /// clock between readings as media3 does.
    fn timestamp(&mut self) -> Option<u64> {
        if self.playing {
            if let (Some(track), Some(stamp), Some((java, mut env))) = (&self.track, &self.timestamp, env()) {
                // Only a reading taken after the last open/flush/play and not in the future is a new anchor
                // (the platform may return a stale one after a flush).
                if let Some((frames, ns)) = read_timestamp(&mut env, java, track, stamp).filter(|&(_, ns)| ns > self.since_ns && ns <= mono_ns()) {
                    if self.stamp.is_none_or(|(_, last, fresh)| !fresh || ns > last) {
                        self.stamp = Some((frames.max(0) as u64, ns, true));
                    }
                }
            }
        }
        self.stamped_now()
    }

    fn data_requested(&mut self) -> bool {
        self.events.wants.swap(false, Ordering::AcqRel)
    }

    fn presented(&mut self) -> bool {
        self.events.ended.load(Ordering::Acquire)
    }

    fn torn_down(&mut self) -> bool {
        self.events.torn.load(Ordering::Acquire)
    }

    /// Logged, and an "offload" event on the perf timeline.
    fn note(&mut self, what: &str) {
        log(&format!("offload: {what}"));
        let wall_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        nori_perf::perf_log::perf_note(wall_ms, nori_perf::perf_log::PerfNote::Offload { detail: what.to_string() });
    }

    fn close(&mut self) {
        if self.track.is_some() {
            self.void(|j| j.track.release);
            log("offloaded track released");
        }
        self.track = None;
        self.buffer = None;
        self.stamp = None;
        self.playing = false;
        self.since_ns = mono_ns();
    }
}

impl Drop for JavaOffload {
    fn drop(&mut self) {
        self.close();
    }
}

/// Opens AudioTracks through `RustBridge.openTrack`.
struct JavaOpener {
    /// API level: before 31 a track has no start threshold and starts only once full.
    sdk: i32,
}

impl Opener for JavaOpener {
    fn open(&mut self, format: OutputFormat, float: bool, frames: u64) -> Result<Opened, String> {
        self.track(format, float, frames, 0)
    }

    fn beside(&mut self, format: OutputFormat, float: bool, frames: u64, session: i32) -> Result<Opened, String> {
        if session == 0 {
            return Err("the track has no audio session".into());
        }
        self.track(format, float, frames, session)
    }
}

impl JavaOpener {
    /// `beside`: the audio session of the track a second one is opened beside; 0 for the first.
    fn track(&mut self, format: OutputFormat, float: bool, frames: u64, beside: i32) -> Result<Opened, String> {
        let (java, mut env) = env().ok_or("no JVM")?;
        let encoding = if float {
            ENCODING_PCM_FLOAT
        } else if packed24(format, float) {
            ENCODING_PCM_24BIT_PACKED
        } else {
            ENCODING_PCM_16BIT
        };
        let opened = env.with_local_frame(8, |env| -> jni::errors::Result<Result<Opened, String>> {
            let args = [
                JValue::Int(format.rate as i32).as_jni(),
                JValue::Int(format.channels as i32).as_jni(),
                JValue::Int(encoding).as_jni(),
                JValue::Int(frames.min(i32::MAX as u64) as i32).as_jni(),
                JValue::Int(beside).as_jni(),
            ];
            let track = call_static(env, bridge(java), java.open_track, ReturnType::Object, &args).and_then(|v| v.l().ok());
            let Some(track) = track.filter(|t| !t.is_null()) else { return Ok(Err("the AudioTrack would not open".into())) };
            let frames = call_int(env, &track, java.track.buffer_frames, &[]).unwrap_or(0).max(0) as u64;
            let mut staging = vec![0f32; CHUNK_BYTES / 4];
            // SAFETY: `staging` moves into the sink with the buffer, never resized, for as long as the buffer lives.
            let buffer = unsafe { env.new_direct_byte_buffer(staging.as_mut_ptr() as *mut u8, CHUNK_BYTES) }?;
            // SAFETY: AudioTimestamp's no-argument constructor.
            let timestamp = unsafe { env.new_object_unchecked(<&JClass>::from(java.timestamp.as_obj()), java.timestamp_new, &[]) }?;
            let sink = JavaTrack {
                track: env.new_global_ref(&track)?,
                buffer: env.new_global_ref(&buffer)?,
                staging,
                timestamp: env.new_global_ref(&timestamp)?,
                head: HeadCount::default(),
                since_ns: mono_ns(),
                released: false,
                rate: format.rate,
                channels: format.channels,
                encoding,
            };
            Ok(Ok(Opened { sink: Box::new(sink), frames, starts_full: self.sdk < 31 }))
        });
        cleared(&mut env);
        opened.map_err(|e| e.to_string())?
    }
}

// ---- song bytes ----

/// A song's bytes through Kotlin's data sources under the core's cache key. Takes the song over from the
/// fetch-ahead first ([`Ahead::take_over`]) so it crosses the network once.
struct JavaBytes {
    /// Empty for radio.
    key: String,
    ahead: Arc<Ahead>,
}

impl ByteSource for JavaBytes {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        self.open_cancellable(url, None, from, &Cancel::new())
    }

    fn open_cancellable(&self, url: &str, _key: Option<&str>, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
        if !self.key.is_empty() {
            self.ahead.take_over(&self.key);
        }
        open_java(url, &self.key, from, cancel)
    }

    /// A radio stream through `RustBridge.openLive` (uncached, ICY metadata on); also returns `icy-metaint`.
    fn open_live(&self, url: &str) -> Result<(Body, Option<usize>), String> {
        let (java, mut env) = env().ok_or("no JVM")?;
        let body = env.with_local_frame(4, |env| -> jni::errors::Result<Option<(GlobalRef, i32)>> {
            let url = env.new_string(url)?;
            // SAFETY: RustBridge.openLive(String), looked up with this signature.
            let body = unsafe { env.call_static_method_unchecked(bridge(java), java.open_live, ReturnType::Object, &[JValue::Object(&url).as_jni()]) }?.l()?;
            if body.is_null() {
                return Ok(None);
            }
            let icy = env.get_field_unchecked(&body, java.body_icy, ReturnType::Primitive(Primitive::Int))?.i()?;
            Ok(Some((env.new_global_ref(&body)?, icy)))
        });
        cleared(&mut env);
        match body {
            Ok(Some((body, icy))) => {
                log(&format!("a station's stream opens, announcements every {icy} bytes"));
                Ok((Body { start: 0, len: None, reader: Box::new(JavaBody(body)) }, (icy > 0).then_some(icy as usize)))
            }
            _ => Err("the station's stream would not come".into()),
        }
    }
}

/// Request ids for `RustBridge.open`/`cancel`. Global: Kotlin's request table is process-wide.
static TICKETS: AtomicI64 = AtomicI64::new(1);

/// `RustBridge.open` from byte `from` (download, then stream cache, then network; what is read is cached
/// under `key`). `cancel` calls `RustBridge.cancel`, which fails a pending open or read at once.
fn open_java(url: &str, key: &str, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
    let ticket = TICKETS.fetch_add(1, Ordering::Relaxed);
    cancel.on_cancel(move || {
        if let Some((java, mut env)) = env() {
            call_static(&mut env, bridge(java), java.cancel, ReturnType::Primitive(Primitive::Void), &[JValue::Long(ticket).as_jni()]);
        }
    });
    let (java, mut env) = env().ok_or("no JVM")?;
    let body = env.with_local_frame(6, |env| -> jni::errors::Result<Option<Result<(GlobalRef, i64), OpenError>>> {
        let (url, key) = (env.new_string(url)?, env.new_string(key)?);
        let args = [JValue::Object(&url).as_jni(), JValue::Object(&key).as_jni(), JValue::Long(from as i64).as_jni(), JValue::Long(ticket).as_jni()];
        // SAFETY: RustBridge.open(String, String, long, long), looked up with this signature.
        let body = unsafe { env.call_static_method_unchecked(bridge(java), java.open, ReturnType::Object, &args) }?.l()?;
        if body.is_null() {
            return Ok(None);
        }
        let length = env.get_field_unchecked(&body, java.body_length, ReturnType::Primitive(Primitive::Long))?.j()?;
        // Range past the end: no body; `length` is the song's when the server said it.
        if env.get_field_unchecked(&body, java.body_past, ReturnType::Primitive(Primitive::Boolean))?.z()? {
            return Ok(Some(Err(OpenError::PastEnd { len: (length >= 0).then_some(length as u64) })));
        }
        let status = env.get_field_unchecked(&body, java.body_status, ReturnType::Primitive(Primitive::Int))?.i()?;
        if status > 0 {
            return Ok(Some(Err(OpenError::Status(status.min(u16::MAX as i32) as u16))));
        }
        Ok(Some(Ok((env.new_global_ref(&body)?, length))))
    });
    cleared(&mut env);
    match body {
        Ok(Some(Err(e))) => {
            log(&format!("{key} from byte {from}: {e}"));
            Err(e)
        }
        Ok(Some(Ok((body, length)))) => {
            log(&format!("{key} from byte {from}: {} bytes come", if length >= 0 { length.to_string() } else { "unknown".into() }));
            Ok(Body { start: from, len: (length >= 0).then(|| from + length as u64), reader: Box::new(JavaBody(body)) })
        }
        _ => Err("the song's bytes would not come".into()),
    }
}

// ---- fetching ahead ----

/// The fetch-ahead's byte source: `RustBridge.open` under the key it is given.
struct AheadBytes;

impl ByteSource for AheadBytes {
    fn open(&self, _url: &str, _from: u64) -> Result<Body, OpenError> {
        Err("asked without its cache key".into())
    }

    fn open_keyed(&self, url: &str, key: &str, from: u64) -> Result<Body, OpenError> {
        open_java(url, key, from, &Cancel::new())
    }

    fn open_cancellable(&self, url: &str, key: Option<&str>, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
        match key {
            Some(key) => open_java(url, key, from, cancel),
            None => Err("asked without its cache key".into()),
        }
    }
}

/// media3's stream cache, which stores what [`AheadBytes`] reads by itself; `kept`/`busy` ask Kotlin.
struct Media3Cache;

impl Media3Cache {
    fn ask(key: &str, method: fn(&Java) -> JStaticMethodID) -> Option<bool> {
        let (java, mut env) = env()?;
        let answer = env.with_local_frame(2, |env| -> jni::errors::Result<Option<bool>> {
            let key = env.new_string(key)?;
            Ok(call_static(env, bridge(java), method(java), ReturnType::Primitive(Primitive::Boolean), &[JValue::Object(&key).as_jni()]).and_then(|v| v.z().ok()))
        });
        cleared(&mut env);
        answer.ok().flatten()
    }
}

impl Keeping for Media3Cache {
    /// Unknown counts as kept: nothing is fetched on a guess.
    fn kept(&self, key: &str) -> bool {
        Media3Cache::ask(key, |j| j.kept).unwrap_or(true)
    }

    fn busy(&self, key: &str) -> bool {
        Media3Cache::ask(key, |j| j.busy).unwrap_or(true)
    }

    fn entry(&self, _key: &str) -> Option<Box<dyn Entry>> {
        Some(Box::new(Counted(0)))
    }
}

/// An entry media3 writes itself: only counted here.
struct Counted(u64);

impl Entry for Counted {
    fn write(&mut self, from: u64, bytes: &[u8]) -> bool {
        if from != self.0 {
            return false;
        }
        self.0 += bytes.len() as u64;
        true
    }

    fn written(&self) -> u64 {
        self.0
    }

    fn finish(self: Box<Self>, len: u64) -> bool {
        self.0 == len
    }
}

/// `RustBridge.disk(key)` / `forget(key)`: Kotlin's description of the stream cache entry (and, for
/// `forget`, removes it). None when Kotlin could not be asked.
fn cache_words(key: &str, method: fn(&Java) -> JStaticMethodID) -> Option<String> {
    let (java, mut env) = env()?;
    let words = env.with_local_frame(4, |env| -> jni::errors::Result<String> {
        let key = env.new_string(key)?;
        // SAFETY: RustBridge.disk(String) / forget(String): String, looked up with this signature.
        let said = unsafe { env.call_static_method_unchecked(bridge(java), method(java), ReturnType::Object, &[JValue::Object(&key).as_jni()]) }?.l()?;
        if said.is_null() {
            return Ok(String::new());
        }
        Ok(env.get_string(&JString::from(said))?.into())
    });
    cleared(&mut env);
    words.ok()
}

/// What the stream cache holds of song `id`, for the perf build's silent-break report.
pub(crate) fn disk_words(id: &str) -> String {
    let Some(target) = nori_core::stream::resolve_now(id) else { return format!("{id}: no server to resolve it") };
    if target.key == nori_core::stream::download_key(id.to_string()) {
        return format!("{id}: downloaded");
    }
    cache_words(&target.key, |j| j.disk).unwrap_or_else(|| format!("{}: Kotlin could not be asked", target.key))
}

/// A `RustBody`: reads through `read(int)` and its `buffer` field; closed on drop.
struct JavaBody(GlobalRef);

impl Read for JavaBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let (java, mut env) = env().ok_or_else(|| io::Error::other("no JVM"))?;
        let max = buf.len().min(i32::MAX as usize) as i32;
        let n = match call_int(&mut env, &self.0, java.body_read, &[JValue::Int(max).as_jni()]) {
            Some(-1) => return Ok(0),
            Some(n) if n > 0 => (n as usize).min(buf.len()),
            _ => return Err(io::Error::other("the song's bytes stopped coming")),
        };
        let copied = env.with_local_frame(2, |env| -> jni::errors::Result<()> {
            let array = JByteArray::from(env.get_field_unchecked(&self.0, java.body_buffer, ReturnType::Array)?.l()?);
            // SAFETY: i8 and u8 have the same size and alignment, and the slice is the caller's.
            let into = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut i8, n) };
            env.get_byte_array_region(&array, 0, into)
        });
        cleared(&mut env);
        copied.map(|_| n).map_err(|e| io::Error::other(e.to_string()))
    }
}

impl Drop for JavaBody {
    fn drop(&mut self) {
        if let Some((java, mut env)) = env() {
            call_void(&mut env, &self.0, java.body_close, &[]);
        }
    }
}

// ---- library ----

/// Songs open at the URL and cache key the core resolves; radio stations at the address Kotlin handed
/// over with [`radio`].
struct AndroidLibrary {
    queue: Arc<nori_core::queue::Session>,
    stations: Arc<Mutex<Vec<(String, String)>>>,
    ahead: Arc<Ahead>,
}

impl Library for AndroidLibrary {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        if is_radio(id) {
            let url = self.stations.lock().iter().find(|(s, _)| s == id).map(|(_, u)| u.clone()).ok_or("a station with no address")?;
            log(&format!("{id} is a station's stream"));
            return Ok(Located { source: Source::Live { url, bytes: Arc::new(JavaBytes { key: String::new(), ahead: self.ahead.clone() }) }, hint: None, duration_ms: None, estimated: false });
        }
        let song = self.queue.song(id);
        let duration_ms = song.as_ref().map(|s| s.duration as i64 * 1000).filter(|&d| d > 0);
        // Format hint from a transcoded stream's key; a download (`dl:<id>`) may be transcoded too, so its
        // file is sniffed instead.
        let target = nori_core::stream::resolve_now(id).ok_or("no server to play from")?;
        let hint = if target.key == nori_core::stream::download_key(id.to_string()) {
            None
        } else {
            key_format(&target.key).or_else(|| song.map(|s| s.suffix)).filter(|s| !s.is_empty())
        };
        log(&format!("{id} opens from {} as {}", target.key, hint.as_deref().unwrap_or("whatever it is")));
        let estimated = nori_core::stream::length_estimated(&target.url);
        Ok(Located { source: Source::Url { url: target.url, bytes: Arc::new(JavaBytes { key: target.key, ahead: self.ahead.clone() }) }, hint, duration_ms, estimated })
    }

    fn about(&self, id: &str) -> WindowSong {
        nori_engine::core::about(&self.queue, id)
    }

    fn fetch_ahead(&self, id: &str) -> bool {
        nori_engine::core::fetch_ahead(&self.queue, id)
    }

    /// Fetches the core's precache targets except `next` (the engine loads that) into media3's cache.
    fn ahead(&mut self, next: &str) {
        self.ahead.ask(Arc::new(Media3Cache), Arc::new(AheadBytes), ahead_songs(nori_core::stream::precache_now(), next), Some(measuring_ahead(self.queue.clone())));
    }

    fn taker(&self, id: &str, hint: Option<&str>) -> Option<Listening> {
        measure_as_it_comes(id, hint, false)
    }

    /// The song played nothing and will be refetched: drops its stream cache entry (never a download).
    fn forget(&mut self, id: &str) {
        if is_radio(id) {
            return;
        }
        let Some(target) = nori_core::stream::resolve_now(id) else { return };
        if target.key == nori_core::stream::download_key(id.to_string()) {
            return;
        }
        let said = cache_words(&target.key, |j| j.forget).unwrap_or_else(|| "Kotlin could not be asked".into());
        log(&format!("{id} is fetched anew: its stream cache entry goes ({said})"));
    }
}

// ---- player ----

/// Engine events queued for Kotlin, which is signalled once per batch.
#[derive(Default)]
struct Events {
    /// (kind, index, entry, text, jumps): jumps is `Song`/`Looped`/`Position`'s `jumps`, or `Stopped`/`Bridge`'s `plays`.
    queue: Mutex<VecDeque<(i32, i32, Option<u64>, String, u64)>>,
    signalled: AtomicBool,
    /// Text and jumps of the event [`event`] last returned.
    text: Mutex<String>,
    jumps: AtomicI64,
}

/// Event kinds as Kotlin reads them.
const EVENT_STATE: i32 = 0;
const EVENT_SONG: i32 = 1;
const EVENT_ERROR: i32 = 2;
const EVENT_OUTPUT: i32 = 3;
const EVENT_STOPPED: i32 = 4;
const EVENT_BUFFERING: i32 = 5;
const EVENT_LOOPED: i32 = 6;
const EVENT_TITLE: i32 = 7;
const EVENT_BRIDGE: i32 = 8;
const EVENT_MIXING: i32 = 9;
const EVENT_PLACED: i32 = 10;
const EVENT_LANDED: i32 = 11;

impl Events {
    fn push(&self, e: Event) {
        if let Event::Awake(awake) = e {
            cpu(awake);
            return;
        }
        match &e {
            Event::State(s) => log(&format!("{s:?}")),
            Event::Song { index, id, jumps, .. } => log(&format!("song {index} ({id}) is heard, after jump {jumps}")),
            Event::Output { name } => log(&format!("playing to {name}")),
            Event::Stopped { plays } => log(&format!("stopped by itself, after play {plays}")),
            Event::Buffering(on) => log(if *on { "waits for the song's bytes" } else { "the song's bytes came" }),
            Event::Looped { index, .. } => log(&format!("song {index} again (repeat one)")),
            Event::Bridge { plays } => log(&format!("the network would not bring the song: the offline bridge takes over, after play {plays}")),
            Event::Placed { index, ms } => log(&format!("song {index} goes on at {ms} ms on another path")),
            _ => {}
        }
        let jumps = match &e {
            Event::Song { jumps, .. } | Event::Looped { jumps, .. } | Event::Position { jumps, .. } => *jumps,
            Event::Stopped { plays } | Event::Bridge { plays } => *plays,
            _ => 0,
        };
        let seq = match &e {
            Event::Song { seq, .. } | Event::Looped { seq, .. } => *seq,
            _ => None,
        };
        let (kind, index, text) = match e {
            Event::State(s) => (EVENT_STATE, state_code(s), String::new()),
            Event::Song { index, id, .. } => (EVENT_SONG, index as i32, id),
            Event::Looped { index, id, .. } => (EVENT_LOOPED, index as i32, id),
            Event::Title(t) => (EVENT_TITLE, -1, t),
            Event::Bridge { .. } => (EVENT_BRIDGE, -1, String::new()),
            Event::Mixing(on) => (EVENT_MIXING, on as i32, String::new()),
            // Kotlin reads the position from the status.
            Event::Placed { index, .. } => (EVENT_PLACED, index as i32, String::new()),
            Event::Error { id, message } => (EVENT_ERROR, -1, if id.is_empty() { message } else { format!("{id}: {message}") }),
            Event::Output { name } => (EVENT_OUTPUT, -1, name),
            Event::Stopped { .. } => (EVENT_STOPPED, -1, String::new()),
            Event::Buffering(on) => (EVENT_BUFFERING, on as i32, String::new()),
            // A seek or jump landed (Android asks for no periodic positions).
            Event::Position { index, .. } => (EVENT_LANDED, index as i32, String::new()),
            Event::Awake(_) => return,
        };
        let first = {
            let mut q = self.queue.lock();
            q.push_back((kind, index, seq, text, jumps));
            !self.signalled.swap(true, Ordering::AcqRel)
        };
        // No player registered yet to take the signal (the engine's first events): signal again with the
        // next event; the player drains once when it registers.
        if first && !signal() {
            self.signalled.store(false, Ordering::Release);
        }
    }
}

/// `RustBridge.signal()`: whether a player took the wake-up.
fn signal() -> bool {
    env().is_some_and(|(java, mut env)| call_static(&mut env, bridge(java), java.signal, ReturnType::Primitive(Primitive::Boolean), &[]).and_then(|v| v.z().ok()).unwrap_or(false))
}

/// `RustBridge.cpu`: take (true) or release the wake lock. Called on the engine thread before the work
/// it is for.
fn cpu(awake: bool) {
    let Some((java, mut env)) = env() else { return };
    call_static(&mut env, bridge(java), java.cpu, ReturnType::Primitive(Primitive::Void), &[JValue::Bool(awake as jboolean).as_jni()]);
}

fn state_code(s: State) -> i32 {
    match s {
        State::Idle => 0,
        State::Playing => 1,
        State::Paused => 2,
        State::Ended => 3,
    }
}

struct Player {
    engine: Engine,
    shared: Arc<Shared>,
    events: Arc<Events>,
    offload: Arc<OffloadEvents>,
    /// Queued radio stations: (id, url).
    stations: Arc<Mutex<Vec<(String, String)>>>,
    /// The last seek target and when it was asked: shown as the position until the engine has taken it,
    /// since media3 reads the position right after a seek returns.
    jumped: Mutex<Option<(i64, Instant)>>,
    /// Last [`look_now`], monotonic ms.
    looked_ms: AtomicI64,
    /// Music volume for loudness compensation ([`set_volume`]).
    volume: Arc<OutputVolume>,
    ahead: Arc<Ahead>,
}

impl Player {
    /// The position to show: the pending jump's target, or the engine's `now`.
    fn shown_place(&self, at: Instant, switching: bool, now: i64) -> i64 {
        let jumped = *self.jumped.lock();
        let jump = jumped.map(|(ms, when)| (ms, when.elapsed().as_millis() as i64));
        nori_player::transport::shown_place(jump, jumped.is_none_or(|(_, when)| at >= when), switching, now)
    }
}

/// Live players by Kotlin handle. Handles are numbers, not pointers, so a native called with a destroyed
/// handle (a late callback) finds nothing, and one that found a player keeps it alive until it returns.
static PLAYERS: Mutex<Vec<(jlong, Arc<Player>)>> = Mutex::new(Vec::new());
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn player(h: jlong) -> Option<Arc<Player>> {
    if h == 0 {
        return None;
    }
    PLAYERS.lock().iter().find(|(k, _)| *k == h).map(|(_, p)| p.clone())
}

/// Starts an engine over the core's queue and settings. `sdk`: API level; `float`: high quality output;
/// `memory_mb`: the app's memory class. 0 when the Java side is missing.
extern "system" fn create(mut env: JNIEnv, _: JClass, sdk: jint, float: jboolean, memory_mb: jint) -> jlong {
    if JAVA.get().is_none() {
        match look_up(&mut env) {
            Ok(j) => {
                let _ = JAVA.set(j);
            }
            Err(e) => {
                cleared(&mut env);
                log(&format!("the Java side is missing: {e}"));
                return 0;
            }
        }
    }
    let shared = Arc::new(Shared::default());
    let output = TrackOutput::new(Box::new(JavaOpener { sdk }), float != 0, shared.clone());
    let stations = Arc::new(Mutex::new(Vec::new()));
    let ahead = Ahead::new();
    let library = AndroidLibrary { queue: nori_core::queue::shared().clone(), stations: stations.clone(), ahead: ahead.clone() };
    // Full volume until Kotlin reports one (only while loudness compensation is on).
    let volume = Arc::new(OutputVolume::default());
    let sound = nori_core::settings_store::shared().current().map(|p| settings(&p, volume.db())).unwrap_or_default();
    let watch = Some(nori_engine::watch::Watcher(Arc::new(crate::PerfWatch)));
    let config = Config { memory_mb: memory_mb.max(16) as u32, settings: sound, watch, ..Config::default() };
    let events = Arc::new(Events::default());
    let tell = events.clone();
    let offload = Arc::new(OffloadEvents::default());
    let can_offload = JAVA.get().is_some_and(|j| j.offload.is_some()) && sdk >= 29;
    let offloaded: Option<Box<dyn OffloadOutput>> = can_offload.then(|| Box::new(JavaOffload::new(offload.clone())) as Box<dyn OffloadOutput>);
    log(&format!("the engine starts: API {sdk}, {} output, {} MB of memory, offload {}", if float != 0 { "float" } else { "16-bit" }, config.memory_mb, if can_offload { "possible" } else { "not on this Android" }));
    let queue = nori_core::queue::shared();
    let app = CoreApp::new(queue.clone()).bridging().volume(volume.clone());
    let engine = Engine::start(library, app, CoreQueue(queue.clone()), Box::new(output), offloaded, config, move |e| tell.push(e));
    let h = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    PLAYERS.lock().push((h, Arc::new(Player { engine, shared, events, offload, stations, jumped: Mutex::new(None), looked_ms: AtomicI64::new(i64::MIN / 2), volume, ahead })));
    h
}

/// Unregisters the player and stops it on a thread of its own (stopping joins engine threads, and media3
/// calls this on the main thread).
extern "system" fn destroy(_: JNIEnv, _: JClass, h: jlong) {
    let gone = {
        let mut players = PLAYERS.lock();
        players.iter().position(|(k, _)| *k == h).map(|i| players.remove(i).1)
    };
    if let Some(p) = gone {
        // Stop this player's own fetch-ahead (a newer player keeps its own).
        p.ahead.ask(Arc::new(Media3Cache), Arc::new(AheadBytes), Vec::new(), None);
        // If the thread fails to start, the closure drops `p` here instead.
        let _ = std::thread::Builder::new().name("nori-release".into()).spawn(move || drop(p));
    }
}

/// A seek, skip or tap (`Engine::go_to`). Returns the jump number song events carry ([`event_jumps`]);
/// 0 when nothing was sent.
extern "system" fn go_to(h: jlong, index: jint, ms: jlong) -> jlong {
    let (Some(p), Ok(i)) = (player(h), usize::try_from(index)) else { return 0 };
    log(&format!("to song {i} at {} ms, playing or not as it was", ms.max(0)));
    *p.jumped.lock() = Some((ms.max(0), Instant::now()));
    p.engine.go_to(i, ms.max(0)) as jlong
}

/// Sleep timer's "end of song".
extern "system" fn pause_at_end(h: jlong, on: jboolean) {
    if let Some(p) = player(h) {
        p.engine.pause_at_end(on != 0);
    }
}

extern "system" fn play(h: jlong) {
    if let Some(p) = player(h) {
        p.engine.play();
    }
}

extern "system" fn pause(h: jlong) {
    if let Some(p) = player(h) {
        p.engine.pause();
    }
}

/// Audio becoming noisy (headphones unplugged): pause without a fade.
extern "system" fn pause_now(h: jlong) {
    if let Some(p) = player(h) {
        log("headphones gone: pause at once");
        p.engine.pause_now();
    }
}

extern "system" fn queue_changed(h: jlong) {
    if let Some(p) = player(h) {
        p.engine.queue_changed();
    }
}

extern "system" fn set_repeat(h: jlong, mode: jint) {
    if let Some(p) = player(h) {
        p.engine.set_repeat(mode.clamp(0, 2) as u8);
    }
}

extern "system" fn replan(h: jlong) {
    if let Some(p) = player(h) {
        p.engine.replan();
    }
}

extern "system" fn gain_changed(h: jlong) {
    if let Some(p) = player(h) {
        p.engine.gain_changed();
    }
}

/// Equalizer screen open (shallow buffer) or closed.
extern "system" fn set_tuning(h: jlong, on: jboolean) {
    if let Some(p) = player(h) {
        p.engine.set_tuning(on != 0);
    }
}

extern "system" fn apply_settings(h: jlong) {
    if let (Some(p), Some(prefs)) = (player(h), nori_core::settings_store::shared().current()) {
        p.engine.set_settings(settings(&prefs, p.volume.db()));
    }
}

/// Current playback position: the engine's reading extrapolated, or a pending jump's target.
extern "system" fn position_ms(h: jlong) -> jlong {
    let Some(p) = player(h) else { return 0 };
    // Read in place: called every frame, and a status copy clones the song id.
    let (at, switching, now) = p.engine.status_with(|s| (s.at, s.switching, s.position_now()));
    p.shown_place(at, switching, now)
}

/// [`position_ms`] for the seek bar of queue index `index`; -1 when the engine is on another song. Asks
/// the engine to re-read its output when the reading is stale (`Status::screen_now`).
pub(crate) extern "system" fn shown_ms(h: jlong, index: jint) -> jlong {
    let Some(p) = player(h) else { return -1 };
    let (song, at, switching, (now, stale)) = p.engine.status_with(|s| (s.index, s.at, s.switching, s.screen_now()));
    if song.is_none_or(|i| i as jint != index) {
        return -1;
    }
    if stale {
        look_now(&p);
    }
    p.shown_place(at, switching, now)
}

/// Screen coming back: re-read the output once.
extern "system" fn look(h: jlong) {
    if let Some(p) = player(h) {
        look_now(&p);
    }
}

/// [`Engine::look`], at most once per `nori_player::heard::LOOK_AFTER_MS`.
fn look_now(p: &Player) {
    let now = mono_ns() / 1_000_000;
    let last = p.looked_ms.load(Ordering::Relaxed);
    if now - last >= nori_player::heard::LOOK_AFTER_MS && p.looked_ms.compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
        p.engine.look();
    }
}

extern "system" fn mixing(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.mixing)) as jboolean
}

/// Whether the sound chain processes the samples.
extern "system" fn chain_in(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.chain)) as jboolean
}

/// `Status::on_cpu`.
extern "system" fn on_cpu(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.on_cpu)) as jboolean
}

/// Limiter gain reduction on the last buffer, dB.
extern "system" fn gain_reduction_db(h: jlong) -> jfloat {
    player(h).map_or(0.0, |p| p.engine.status_with(|s| s.gain_reduction_db))
}

/// Music volume step `index` of `max` (`db`: the platform's figure, NaN if none); only reported while
/// loudness compensation is on. Rebuilds the chain when that changes the sound.
extern "system" fn set_volume(h: jlong, index: jint, max: jint, db: jfloat) {
    let db = nori_player::contour::volume_db(index, max, db);
    let Some(p) = player(h) else { return };
    if !p.volume.set(db) {
        return;
    }
    let Some(prefs) = nori_core::settings_store::shared().current().filter(|p| p.loudness) else { return };
    let s = nori_player::contour::design(prefs.loudness_ref_phon as f64, db);
    nori_core::alog::info(&format!(
        "loudness: volume {index}/{max} at {db:.1} dB, bass {:+.1} dB at {} Hz, treble {:+.1} dB, pre-gain {:.1} dB",
        s.low.map_or(0.0, |b| b.gain_db),
        s.low.map_or(0.0, |b| b.freq),
        s.high.map_or(0.0, |b| b.gain_db),
        s.pre_db
    ));
    p.engine.set_settings(settings(&prefs, p.volume.db()));
}

/// Compressor gain reduction on the last buffer, dB.
extern "system" fn compression_db(h: jlong) -> jfloat {
    player(h).map_or(0.0, |p| p.engine.status_with(|s| s.compression_db))
}

extern "system" fn bytes_written(h: jlong) -> jlong {
    player(h).map_or(0, |p| p.shared.bytes.load(Ordering::Relaxed) as jlong)
}

/// Next event as `kind << 32 | index` (state events: the state code), its text kept for [`event_text`];
/// -1 when empty, after which the next event signals Kotlin again.
extern "system" fn event(h: jlong) -> jlong {
    let Some(p) = player(h) else { return -1 };
    let mut q = p.events.queue.lock();
    // Drop stops from before a play Kotlin has since asked for (`Engine::superseded`): Kotlin would show
    // paused over music playing.
    while let Some(&(kind, _, _, _, plays)) = q.front() {
        let stop = match kind {
            EVENT_STOPPED => Event::Stopped { plays },
            EVENT_BRIDGE => Event::Bridge { plays },
            _ => break,
        };
        if !p.engine.superseded(&stop) {
            break;
        }
        log(&format!("{stop:?} is from before the last play asked for: passed over"));
        q.pop_front();
    }
    match q.pop_front() {
        Some((kind, index, seq, text, jumps)) => {
            // Kotlin's queue is the core's as it is now: a song is where its entry is now (-1: gone).
            let index = match seq {
                Some(s) => nori_core::queue::shared().playlist(|q| q.index_of(s)).map_or(-1, |i| i as i32),
                None => index,
            };
            *p.events.text.lock() = text;
            p.events.jumps.store(jumps as i64, Ordering::Relaxed);
            ((kind as i64) << 32) | (index as u32 as i64)
        }
        None => {
            p.events.signalled.store(false, Ordering::Release);
            -1
        }
    }
}

/// Text of the last [`event`] (song id, error, output name). `@FastNative`: only the main thread takes
/// this lock, and no Java is called.
extern "system" fn event_text(env: JNIEnv, _: JClass, h: jlong) -> jstring {
    let Some(p) = player(h) else { return std::ptr::null_mut() };
    let text = std::mem::take(&mut *p.events.text.lock());
    java_string(&env, &text)
}

/// Jump count of the last [`event`], so Kotlin can drop song events from before its latest jump.
extern "system" fn event_jumps(h: jlong) -> jlong {
    player(h).map_or(0, |p| p.events.jumps.load(Ordering::Relaxed))
}

/// USB device attached (no offload) and bit-perfect DAC output.
extern "system" fn set_output(h: jlong, usb: jboolean, bit_perfect: jboolean) {
    if let Some(p) = player(h) {
        p.engine.set_output(OutputFacts { usb: usb != 0, bit_perfect: bit_perfect != 0 });
    }
}

/// The offloaded track's `StreamEventCallback`: `kind` 0 `onDataRequest`, 1 `onPresentationEnded`,
/// 2 `onTearDown`. Wakes the engine thread.
extern "system" fn offload_event(h: jlong, kind: jint) {
    let Some(p) = player(h) else { return };
    match kind {
        0 => {
            p.offload.wants.store(true, Ordering::Release);
            nori_perf::perf_log::count_data_request();
        }
        1 => p.offload.ended.store(true, Ordering::Release),
        2 => {
            log("the offloaded track was torn down");
            p.offload.torn.store(true, Ordering::Release);
        }
        _ => {}
    }
    p.offload.wake();
}

extern "system" fn offloaded(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.offloaded)) as jboolean
}

/// Whether settings and output allow offload.
extern "system" fn offload_wanted(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.offload_wanted)) as jboolean
}

/// `Status::pcm_why`: why playback is not offloaded; null while offloaded.
extern "system" fn pcm_why(env: JNIEnv, _: JClass, h: jlong) -> jstring {
    match player(h).and_then(|p| p.engine.status_with(|s| s.pcm_why.clone())) {
        Some(why) => java_string(&env, &why),
        None => std::ptr::null_mut(),
    }
}

/// Registers the stream `url` of the radio station queued as `id`.
extern "system" fn radio(env: JNIEnv, _: JClass, h: jlong, id: JString, url: JString) {
    let Some(p) = player(h) else { return };
    let (Some(id), Some(url)) = (crate::string(&env, &id), crate::string(&env, &url)) else { return };
    let mut s = p.stations.lock();
    s.retain(|(i, _)| *i != id);
    s.push((id, url));
}

/// Route changed: `kind` is `AudioDeviceInfo.TYPE_*`, `name` the product name.
extern "system" fn device(env: JNIEnv, _: JClass, h: jlong, kind: jint, name: JString) {
    let Some(p) = player(h) else { return };
    let name = crate::string(&env, &name).unwrap_or_default();
    let watch = p.shared.watch.lock();
    if let Some(watch) = &*watch {
        watch(Device { kind: nori_core::outputs::kind(kind), name });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offload_support_decodes_like_media3() {
        let cases = [
            (3 << 8, Support::No, "getDirectPlaybackSupport: NOT_SUPPORTED"),
            ((3 << 8) | 1, Support::Plain, "getDirectPlaybackSupport: OFFLOAD_SUPPORTED"),
            ((3 << 8) | 3, Support::Gapless, "getDirectPlaybackSupport: OFFLOAD_GAPLESS_SUPPORTED"),
            ((3 << 8) | 7, Support::Gapless, "getDirectPlaybackSupport: OFFLOAD_GAPLESS_SUPPORTED | BITSTREAM_SUPPORTED"),
            ((3 << 8) | 4, Support::No, "getDirectPlaybackSupport: NOT_SUPPORTED | BITSTREAM_SUPPORTED"),
            // Android 12: gapless not trusted.
            (2 << 8, Support::No, "getPlaybackOffloadSupport: NOT_SUPPORTED"),
            ((2 << 8) | 1, Support::Plain, "getPlaybackOffloadSupport: SUPPORTED"),
            ((2 << 8) | 2, Support::Plain, "getPlaybackOffloadSupport: GAPLESS_SUPPORTED, taken as without gaps before Android 13"),
            (1 << 8, Support::No, "isOffloadedPlaybackSupported: false"),
            ((1 << 8) | 1, Support::Plain, "isOffloadedPlaybackSupported: true, which says nothing of gaps"),
            (-1, Support::No, "the platform could not be asked"),
        ];
        for (answer, support, words) in cases {
            assert_eq!(offload_support(answer), (support, words.to_string()), "{answer:#x}");
        }
    }
}
