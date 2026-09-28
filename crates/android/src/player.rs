//! The Rust playback path: nori-engine playing the core's queue (`CoreQueue`, `CoreApp`) into an
//! AudioTrack (track.rs): the app's one player. Kotlin's `EnginePlayer` is a media3 player over these
//! doors, so the media session, the notification and the screens follow it as they would any player.
//!
//! What only the platform has comes from Kotlin through a few calls into `RustBridge`, each made rarely:
//! - the AudioTrack, opened by Kotlin (`openTrack`: the attributes, the DAC's preferred device and
//!   mixer attributes, the route listener), then written and driven from Rust;
//! - a song's bytes (`open`, then `read` per 256 KB, each filled whole), at the URL and under the cache
//!   key the core resolves (`nori_core::stream::resolve_now`, over the network state Kotlin tells the
//!   core): through media3's data sources on the app's one OkHttp client, so the TLS settings, client
//!   certificates and headers of the profile apply, and a song downloaded, cached or fetched ahead plays
//!   from the disk;
//! - the songs after the next, fetched ahead into media3's stream cache by nori-engine's one fetcher of the
//!   songs coming up (`nori_engine::ahead`, each player's [`Ahead`]) through the same doors: whether a song is whole in
//!   the cache already (`kept`) and whether the player is writing it (`busy`) are asked of Kotlin;
//! - a wake for the events (`signal`): one call per batch of engine events, however many there are,
//!   and Kotlin takes them from here on its own thread;
//! - audio offload (Android 10 and later): whether the phone's audio chip decodes a song's compression
//!   where the music goes now (`offloadSupport`), and an AudioTrack opened for offload (`openOffload`),
//!   whose stream events (it wants more, it played what it was given, it was torn down) Kotlin hands back
//!   through `offloadEvent`, which wakes the engine's thread; the engine writes the song's packets into it
//!   from Rust ([`JavaOffload`]);
//! - an internet radio station's stream (`openLive`), with the station's announcements asked for.
//!
//! Every class and method is looked up once, in `create`, on a thread that sees the app's classes; the
//! threads that call them (the engine's, the track's, the loaders') are attached for their whole life.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::Thread;
use std::time::Instant;

use jni::objects::{GlobalRef, JByteArray, JClass, JFieldID, JMethodID, JStaticMethodID, JString, JValue};
use jni::signature::{Primitive, ReturnType};
use jni::sys::{jboolean, jfloat, jint, jlong, jstring};
use jni::{JNIEnv, JavaVM};
use nori_engine::ahead::{Ahead, Entry, Keeping};
use nori_engine::arriving::Listening;
use nori_engine::core::{ahead_songs, is_radio, key_format, measure_as_it_comes, measuring_ahead, settings, CoreApp, CoreQueue, OutputVolume};
use nori_engine::{Body, ByteSource, Cancel, Coded, Coding, Config, Device, Engine, Event, Library, Located, OffloadOutput, OpenError, OutputFacts, OutputFormat, Source, State, Support};
use nori_player::transitions::WindowSong;
use parking_lot::Mutex;

use crate::track::{mono_ns, packed24, sample_bytes, HeadCount, Opened, Opener, Route, Shared, Sink, TrackOutput, CHUNK_BYTES};
use crate::{cleared, java_string, native, with_str, Class};

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

/// The Java side, looked up once.
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
    /// AudioTrack's offload calls, which Android 10 added: none before it.
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
    /// `setStartThresholdInFrames`, which Android 12 added: none before it.
    set_start_threshold: Option<JMethodID>,
    underruns: JMethodID,
    capacity_frames: JMethodID,
    routed_device: JMethodID,
    /// `AudioTrack.getMinBufferSize(int, int, int)`, static.
    min_buffer_size: JStaticMethodID,
    /// `AudioTrack.getLatency()`, which is hidden (on the list of those apps may still call, as ExoPlayer
    /// does): none where the platform refuses it.
    latency: Option<JMethodID>,
    /// `AudioDeviceInfo.getType()`.
    device_type: JMethodID,
    class: GlobalRef,
}

static JAVA: OnceLock<Java> = OnceLock::new();

fn look_up(env: &mut JNIEnv) -> jni::errors::Result<Java> {
    let bridge = env.find_class("dev/nori/music/playback/RustBridge")?;
    let body = env.find_class("dev/nori/music/playback/RustBody")?;
    let track = env.find_class("android/media/AudioTrack")?;
    let buffer = env.find_class("java/nio/Buffer")?;
    let timestamp = env.find_class("android/media/AudioTimestamp")?;
    // Looked up apart: a phone before Android 10 has neither, and throws for each.
    let delay_padding = env.get_method_id(&track, "setOffloadDelayPadding", "(II)V");
    cleared(env);
    let end_of_stream = env.get_method_id(&track, "setOffloadEndOfStream", "()V");
    cleared(env);
    let start_threshold = env.get_method_id(&track, "setStartThresholdInFrames", "(I)I").ok();
    cleared(env);
    let latency = env.get_method_id(&track, "getLatency", "()I").ok();
    cleared(env);
    let device_info = env.find_class("android/media/AudioDeviceInfo")?;
    let offload = match (delay_padding, end_of_stream) {
        (Ok(delay_padding), Ok(end_of_stream)) => Some(OffloadMethods { delay_padding, end_of_stream }),
        _ => None,
    };
    Ok(Java {
        vm: env.get_java_vm()?,
        open_track: env.get_static_method_id(&bridge, "openTrack", "(IIII)Landroid/media/AudioTrack;")?,
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

/// This thread's JNIEnv, attached under its own name for the rest of its life (it is detached when it ends).
fn env() -> Option<(&'static Java, JNIEnv<'static>)> {
    let java = JAVA.get()?;
    let env = crate::attached(&java.vm)?;
    Some((java, env))
}

/// One line in the app's log.
fn log(message: &str) {
    nori_core::alog::info(&format!("rust player: {message}"));
}

fn bridge(java: &Java) -> &JClass<'static> {
    <&JClass>::from(java.bridge.as_obj())
}

// ---- the AudioTrack ----

/// An AudioTrack Kotlin opened, written from the track's thread through a direct buffer over memory
/// kept for its whole life.
struct JavaTrack {
    track: GlobalRef,
    buffer: GlobalRef,
    staging: Vec<f32>,
    timestamp: GlobalRef,
    /// The play head, unwrapped: the platform's is 32 bits.
    head: HeadCount,
    /// When the track was last flushed or started: a timestamp from before then is the old music's.
    since_ns: i64,
    /// Released already: dropping it does not release it again.
    released: bool,
    /// Its sample rate, for the start threshold kept inside its size.
    rate: u32,
    /// Its channels and `AudioFormat.ENCODING_*`, for the least a new track there is given.
    channels: usize,
    encoding: i32,
}

/// A track dropped without being released - its writer thread would not start, or panicked - is released
/// here: the platform's AudioTrack holds a mixer slot until it is.
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
        // SAFETY: the method was looked up on AudioTrack with this signature, and takes no arguments.
        let _ = unsafe { env.call_method_unchecked(&self.track, m(&java.track), ReturnType::Primitive(Primitive::Void), &[]) };
        cleared(&mut env);
    }
}

impl Sink for JavaTrack {
    fn staging(&mut self) -> &mut [f32] {
        &mut self.staging
    }

    fn write(&mut self, from: usize, len: usize) -> Result<usize, i32> {
        let Some((java, mut env)) = env() else { return Ok(0) };
        let (Ok(from), Ok(len)) = (i32::try_from(from), i32::try_from(len)) else { return Ok(0) };
        // SAFETY: Buffer.position(int) and AudioTrack.write(ByteBuffer, int, int), looked up with these
        // signatures; the buffer is the direct one over `staging`, whose range the writer keeps inside it.
        let taken = unsafe {
            if let Ok(b) = env.call_method_unchecked(&self.buffer, java.position, ReturnType::Object, &[JValue::Int(from).as_jni()]) {
                if let Ok(b) = b.l() {
                    let _ = env.delete_local_ref(b);
                }
            }
            let args = [JValue::Object(self.buffer.as_obj()).as_jni(), JValue::Int(len).as_jni(), JValue::Int(WRITE_NON_BLOCKING).as_jni()];
            env.call_method_unchecked(&self.track, java.track.write, ReturnType::Primitive(Primitive::Int), &args).and_then(|v| v.i())
        };
        cleared(&mut env);
        // A write that threw is a dead track, as one answering ERROR_DEAD_OBJECT is. Every error is the
        // writer's to act on: counted as nothing taken, it read as a full track and was asked again at
        // once, for ever, in silence.
        match taken.unwrap_or(ERROR_DEAD_OBJECT) {
            n if n >= 0 => Ok(n as usize),
            code => Err(code),
        }
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
        // The perf build's self test plays quietly: a player volume under the engine's own.
        let volume = volume * nori_perf::invariants::quiet();
        // SAFETY: AudioTrack.setVolume(float), looked up with this signature.
        let _ = unsafe { env.call_method_unchecked(&self.track, java.track.set_volume, ReturnType::Primitive(Primitive::Int), &[JValue::Float(volume).as_jni()]) };
        cleared(&mut env);
    }

    /// While playing, the frame the device presented and when (the AudioTimestamp); otherwise, or
    /// before it has one, the frame the track has handed on.
    fn heard(&mut self, playing: bool) -> Option<(u64, i64)> {
        let (java, mut env) = env()?;
        if playing {
            // SAFETY: AudioTrack.getTimestamp(AudioTimestamp) and the timestamp's two long fields, looked
            // up with these signatures.
            let stamped = unsafe {
                env.call_method_unchecked(&self.track, java.track.get_timestamp, ReturnType::Primitive(Primitive::Boolean), &[JValue::Object(self.timestamp.as_obj()).as_jni()])
                    .and_then(|v| v.z())
                    .unwrap_or(false)
            };
            cleared(&mut env);
            if stamped {
                let long = |env: &mut JNIEnv, f| env.get_field_unchecked(&self.timestamp, f, ReturnType::Primitive(Primitive::Long)).and_then(|v| v.j());
                match (long(&mut env, java.frame_position), long(&mut env, java.nano_time)) {
                    // One taken before the last flush or start is the old music's: the head says instead.
                    (Ok(frames), Ok(ns)) if ns >= self.since_ns => return Some((frames.max(0) as u64, ns)),
                    (Ok(_), Ok(_)) => {}
                    _ => cleared(&mut env),
                }
            }
        }
        // SAFETY: AudioTrack.getPlaybackHeadPosition(), looked up with this signature.
        let head = unsafe { env.call_method_unchecked(&self.track, java.track.head, ReturnType::Primitive(Primitive::Int), &[]).and_then(|v| v.i()) };
        cleared(&mut env);
        // An unsigned count that wraps, as the platform documents it.
        head.ok().map(|h| (self.head.read(h as u32), mono_ns()))
    }

    /// `setBufferSizeInFrames`, which keeps the track playing and what it holds, and the start threshold
    /// (Android 12 on) kept inside the new size: a quarter of a second, as the track was opened with, or
    /// all of a size smaller than that, so a flush while shallow does not wait for more than it may hold.
    fn resize(&mut self, frames: u64) -> u64 {
        let Some((java, mut env)) = env() else { return frames };
        let asked = frames.min(i32::MAX as u64) as i32;
        // SAFETY: AudioTrack.setBufferSizeInFrames(int), looked up with this signature.
        let given = unsafe { env.call_method_unchecked(&self.track, java.track.set_buffer_frames, ReturnType::Primitive(Primitive::Int), &[JValue::Int(asked).as_jni()]) }.and_then(|v| v.i());
        cleared(&mut env);
        let given = match given {
            Ok(n) if n > 0 => n,
            other => {
                log(&format!("the AudioTrack kept its size: setBufferSizeInFrames({asked}) answered {other:?}"));
                // SAFETY: AudioTrack.getBufferSizeInFrames(), looked up with this signature.
                let now = unsafe { env.call_method_unchecked(&self.track, java.track.buffer_frames, ReturnType::Primitive(Primitive::Int), &[]) }.and_then(|v| v.i());
                cleared(&mut env);
                return now.unwrap_or(asked).max(1) as u64;
            }
        };
        if let Some(m) = java.track.set_start_threshold {
            let threshold = (self.rate as i32 / 4).min(given).max(1);
            // SAFETY: AudioTrack.setStartThresholdInFrames(int), looked up with this signature.
            let _ = unsafe { env.call_method_unchecked(&self.track, m, ReturnType::Primitive(Primitive::Int), &[JValue::Int(threshold).as_jni()]) };
            cleared(&mut env);
        }
        given as u64
    }

    /// The least a new track of this format is given where music plays now, the latency of that output
    /// past this track, and what kind of output it is.
    fn route(&mut self) -> Route {
        let Some((java, mut env)) = env() else { return Route::default() };
        let int = |env: &mut JNIEnv, m: JMethodID| -> Option<i32> {
            // SAFETY: a method of AudioTrack looked up with the signature `()I`.
            let v = unsafe { env.call_method_unchecked(&self.track, m, ReturnType::Primitive(Primitive::Int), &[]) }.and_then(|v| v.i());
            cleared(env);
            v.ok()
        };
        let mask = if self.channels == 1 { CHANNEL_OUT_MONO } else { CHANNEL_OUT_STEREO };
        let args = [JValue::Int(self.rate as i32).as_jni(), JValue::Int(mask).as_jni(), JValue::Int(self.encoding).as_jni()];
        let class = <&JClass>::from(java.track.class.as_obj());
        // SAFETY: AudioTrack.getMinBufferSize(int, int, int), static, looked up with this signature.
        let min_bytes = unsafe { env.call_static_method_unchecked(class, java.track.min_buffer_size, ReturnType::Primitive(Primitive::Int), &args) }.and_then(|v| v.i());
        cleared(&mut env);
        let frame = self.channels * sample_bytes(self.encoding == ENCODING_PCM_FLOAT, self.encoding == ENCODING_PCM_24BIT_PACKED);
        let min_frames = min_bytes.ok().filter(|&b| b > 0).map(|b| b as u64 / frame.max(1) as u64);
        // getLatency is the output's latency and the whole buffer the track was opened with, in ms.
        let latency_frames = match (java.track.latency.and_then(|m| int(&mut env, m)), int(&mut env, java.track.capacity_frames)) {
            (Some(ms), Some(cap)) if ms > 0 && cap > 0 => Some((ms as i64 - cap as i64 * 1000 / self.rate.max(1) as i64).max(0) as u64 * self.rate as u64 / 1000),
            _ => None,
        };
        // SAFETY: AudioTrack.getRoutedDevice(), looked up with this signature.
        let device = unsafe { env.call_method_unchecked(&self.track, java.track.routed_device, ReturnType::Object, &[]) }.and_then(|v| v.l());
        cleared(&mut env);
        let name = match device {
            Ok(d) if !d.is_null() => {
                // SAFETY: AudioDeviceInfo.getType(), looked up with this signature, on an AudioDeviceInfo.
                let kind = unsafe { env.call_method_unchecked(&d, java.track.device_type, ReturnType::Primitive(Primitive::Int), &[]) }.and_then(|v| v.i());
                cleared(&mut env);
                let _ = env.delete_local_ref(d);
                kind.ok().map(device_words)
            }
            _ => None,
        };
        Route { min_frames, latency_frames, name }
    }

    fn underruns(&mut self) -> Option<u64> {
        let (java, mut env) = env()?;
        // SAFETY: AudioTrack.getUnderrunCount(), looked up with this signature.
        let n = unsafe { env.call_method_unchecked(&self.track, java.track.underruns, ReturnType::Primitive(Primitive::Int), &[]) }.and_then(|v| v.i());
        cleared(&mut env);
        n.ok().filter(|&n| n >= 0).map(|n| n as u64)
    }

    fn consumed(&mut self) -> Option<u64> {
        let (java, mut env) = env()?;
        // SAFETY: AudioTrack.getPlaybackHeadPosition(), looked up with this signature.
        let head = unsafe { env.call_method_unchecked(&self.track, java.track.head, ReturnType::Primitive(Primitive::Int), &[]).and_then(|v| v.i()) };
        cleared(&mut env);
        head.ok().map(|h| self.head.read(h as u32))
    }

    fn release(&mut self) {
        self.void(|t| t.release);
        self.released = true;
    }
}

/// `AudioFormat.CHANNEL_OUT_MONO` and `CHANNEL_OUT_STEREO`.
const CHANNEL_OUT_MONO: i32 = 4;
const CHANNEL_OUT_STEREO: i32 = 12;

/// An `AudioDeviceInfo.TYPE_*` in the log's words.
fn device_words(kind: i32) -> &'static str {
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
/// `AudioTrack.ERROR_DEAD_OBJECT`: what a write that threw is counted as.
const ERROR_DEAD_OBJECT: i32 = -6;
/// `AudioFormat.ENCODING_*`.
const ENCODING_PCM_16BIT: i32 = 2;
const ENCODING_PCM_FLOAT: i32 = 4;
const ENCODING_PCM_24BIT_PACKED: i32 = 21;
const ENCODING_MP3: i32 = 9;
const ENCODING_AAC_LC: i32 = 10;
const ENCODING_OPUS: i32 = 20;

// ---- the offloaded AudioTrack ----

/// What an offloaded track's stream events said since the engine last looked (Kotlin's
/// `StreamEventCallback`, through [`offload_event`]), and the engine's thread they wake.
#[derive(Default)]
struct OffloadEvents {
    /// It wants more (`onDataRequest`), since the engine last asked.
    wants: AtomicBool,
    ended: AtomicBool,
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

/// The bytes a write to the offloaded track moves at most: the engine stages about this much at once.
const OFFLOAD_CHUNK: usize = 320 * 1024;

/// An AudioTrack opened for offload, written from the engine's thread: the song's packets, copied into
/// memory a direct buffer lies over for the track's whole life.
struct JavaOffload {
    events: Arc<OffloadEvents>,
    track: Option<GlobalRef>,
    buffer: Option<GlobalRef>,
    staging: Vec<u8>,
    /// The bytes the open track holds: no write can move more.
    held: usize,
    /// The open track's frames a second.
    rate: u32,
    /// The AudioTimestamp the platform fills.
    timestamp: Option<GlobalRef>,
    /// The last timestamp taken (frames presented, and when, CLOCK_MONOTONIC ns), and whether the
    /// platform gave it since the track last began playing: only then is it moved on by the clock. One
    /// held over a pause stands where the pause left it until the platform gives a fresh one.
    stamp: Option<(u64, i64, bool)>,
    /// The track plays, and since when (ns): a timestamp from before is not moved on.
    playing: bool,
    /// When the track was last opened, flushed or began playing, ns: a timestamp the platform took
    /// before then is about music that is gone or a count that stood still, and is not taken.
    since_ns: i64,
    /// What the platform said of each compression it was asked about, in its words.
    said: Vec<(Coded, String)>,
}

impl JavaOffload {
    fn new(events: Arc<OffloadEvents>) -> JavaOffload {
        JavaOffload { events, track: None, buffer: None, staging: Vec::new(), held: 0, rate: 1, timestamp: None, stamp: None, playing: false, since_ns: 0, said: Vec::new() }
    }

    /// The last timestamp, moved on by the clock while the track plays and the timestamp is this
    /// play's; as it was otherwise (paused, or no timestamp yet since it began playing again).
    fn stamped_now(&self) -> Option<u64> {
        let (frames, ns, fresh) = self.stamp?;
        if !self.playing || !fresh {
            return Some(frames);
        }
        let run = (mono_ns() - ns).max(0) as u128 * self.rate as u128 / 1_000_000_000;
        Some(frames + run as u64)
    }

    fn void(&mut self, m: impl FnOnce(&Java) -> Option<JMethodID>) {
        let Some(track) = &self.track else { return };
        let Some((java, mut env)) = env() else { return };
        let Some(m) = m(java) else { return };
        // SAFETY: an AudioTrack method taking no arguments, looked up with that signature.
        let _ = unsafe { env.call_method_unchecked(track, m, ReturnType::Primitive(Primitive::Void), &[]) };
        cleared(&mut env);
    }
}

fn encoding_of(c: Coding) -> i32 {
    match c {
        Coding::Mp3 => ENCODING_MP3,
        Coding::Aac => ENCODING_AAC_LC,
        Coding::Opus => ENCODING_OPUS,
    }
}

/// What `RustBridge.offloadSupport` answered, read as media3 1.11 reads the platform
/// (`DefaultAudioOffloadSupportProvider`), and in words. The call made is in the answer's high byte: 3
/// `getDirectPlaybackSupport` (Android 13 and later), 2 `getPlaybackOffloadSupport` (12), 1
/// `isOffloadedPlaybackSupported` (10 and 11); the platform's answer in its low byte; -1 when it could not
/// be asked. Gapless offload only from Android 13 on, as media3 has it: before, a track's position went
/// wrong after a gapless join (media3's b/191950723). Whether a song needs it is the engine's
/// (`Offload::refuses`: only one with an encoder delay or padding).
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
    /// `RustBridge.offloadSupport`, asked as media3 asks: see [`offload_support`].
    fn supports(&mut self, coded: Coded) -> Support {
        let Some((java, mut env)) = env() else { return Support::No };
        let args = [JValue::Int(encoding_of(coded.coding)).as_jni(), JValue::Int(coded.rate as i32).as_jni(), JValue::Int(coded.channels as i32).as_jni()];
        // SAFETY: RustBridge.offloadSupport(int, int, int), looked up with this signature.
        let answer = unsafe { env.call_static_method_unchecked(bridge(java), java.offload_support, ReturnType::Primitive(Primitive::Int), &args) }.and_then(|v| v.i()).unwrap_or(-1);
        cleared(&mut env);
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
            // SAFETY: AudioTrack.getBufferSizeInFrames(), looked up with this signature; a compressed
            // track counts its buffer in bytes.
            let held = unsafe { env.call_method_unchecked(&track, java.track.buffer_frames, ReturnType::Primitive(Primitive::Int), &[]) }.and_then(|v| v.i()).unwrap_or(0).max(0) as usize;
            // SAFETY: the memory is `staging`'s, kept, never resized, beside the buffer for as long as it lives.
            let buffer = unsafe { env.new_direct_byte_buffer(staging, OFFLOAD_CHUNK) }?;
            Ok(Some((env.new_global_ref(&track)?, env.new_global_ref(&buffer)?, held)))
        });
        cleared(&mut env);
        match opened {
            Ok(Some((track, buffer, held))) => {
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
                let held = if held > 0 { held } else { bytes };
                self.held = held;
                log(&format!("offloaded {:?} track: {} KB of the {} KB asked", coded.coding, held / 1024, bytes / 1024));
                Ok(held)
            }
            _ => Err("the offloaded AudioTrack would not open".into()),
        }
    }

    fn write(&mut self, data: &[u8], _frames: u64) -> Result<usize, i32> {
        let (Some(track), Some(buffer)) = (&self.track, &self.buffer) else { return Err(ERROR_DEAD_OBJECT) };
        let Some((java, mut env)) = env() else { return Ok(0) };
        let mut done = 0;
        while done < data.len() {
            // No more than the track holds: a 64 KB track takes no more than that, whatever is staged.
            let n = (data.len() - done).min(self.staging.len()).min(self.held.max(1));
            self.staging[..n].copy_from_slice(&data[done..done + n]);
            // SAFETY: Buffer.position(int) and AudioTrack.write(ByteBuffer, int, int), looked up with these
            // signatures; the buffer is the direct one over `staging`, which holds the `n` bytes written.
            let taken = unsafe {
                if let Ok(b) = env.call_method_unchecked(buffer, java.position, ReturnType::Object, &[JValue::Int(0).as_jni()]) {
                    if let Ok(b) = b.l() {
                        let _ = env.delete_local_ref(b);
                    }
                }
                let args = [JValue::Object(buffer.as_obj()).as_jni(), JValue::Int(n as i32).as_jni(), JValue::Int(WRITE_NON_BLOCKING).as_jni()];
                env.call_method_unchecked(track, java.track.write, ReturnType::Primitive(Primitive::Int), &args).and_then(|v| v.i())
            };
            cleared(&mut env);
            let k = match taken.unwrap_or(ERROR_DEAD_OBJECT) {
                k if k >= 0 => (k as usize).min(n),
                // What was taken before the error still counts; the error is the next write's to say.
                _ if done > 0 => return Ok(done),
                code => return Err(code),
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
        // SAFETY: AudioTrack.setOffloadDelayPadding(int, int), looked up with this signature.
        let _ = unsafe { env.call_method_unchecked(track, m, ReturnType::Primitive(Primitive::Void), &args) };
        cleared(&mut env);
    }

    /// `AudioTrack.setOffloadEndOfStream`, which throws unless the track plays: false then, and the
    /// engine says it again once it does.
    fn end_of_stream(&mut self) -> bool {
        // What the platform said of an end of stream before this one is not about this one.
        self.events.ended.store(false, Ordering::Release);
        let (Some(track), Some((java, mut env))) = (&self.track, env()) else { return false };
        let Some(m) = java.offload.as_ref().map(|o| o.end_of_stream) else { return false };
        // SAFETY: AudioTrack.setOffloadEndOfStream(), looked up with this signature.
        let said = unsafe { env.call_method_unchecked(track, m, ReturnType::Primitive(Primitive::Void), &[]) }.is_ok() && !env.exception_check().unwrap_or(true);
        cleared(&mut env);
        said
    }

    fn play(&mut self) {
        self.void(|j| Some(j.track.play));
        // The last timestamp stays where the pause left it, not moved on, until the platform gives one
        // of this play.
        self.stamp = self.stamped_now().map(|f| (f, mono_ns(), false));
        self.playing = true;
        self.since_ns = mono_ns();
    }

    fn pause(&mut self) {
        self.void(|j| Some(j.track.pause));
        self.stamp = self.stamped_now().map(|f| (f, mono_ns(), false));
        self.playing = false;
    }

    fn flush(&mut self) {
        self.events.ended.store(false, Ordering::Release);
        self.void(|j| Some(j.track.flush));
        // What the platform said of the music flushed is not about what comes.
        self.stamp = None;
        self.since_ns = mono_ns();
    }

    fn set_volume(&mut self, volume: f32) {
        let (Some(track), Some((java, mut env))) = (&self.track, env()) else { return };
        let volume = volume * nori_perf::invariants::quiet();
        // SAFETY: AudioTrack.setVolume(float), looked up with this signature.
        let _ = unsafe { env.call_method_unchecked(track, java.track.set_volume, ReturnType::Primitive(Primitive::Int), &[JValue::Float(volume).as_jni()]) };
        cleared(&mut env);
    }

    /// The frames presented since the track was opened or flushed; an offloaded track's count starts
    /// again at a song joined without a gap, which the engine reads for what it is.
    /// None when it could not be asked: a failed call is no reading, which the engine does not take for
    /// nought (the start of a song joined without a gap).
    fn head(&mut self) -> Option<u64> {
        let (Some(track), Some((java, mut env))) = (&self.track, env()) else { return None };
        // SAFETY: AudioTrack.getPlaybackHeadPosition(), looked up with this signature.
        let head = unsafe { env.call_method_unchecked(track, java.track.head, ReturnType::Primitive(Primitive::Int), &[]).and_then(|v| v.i()) };
        let threw = env.exception_check().unwrap_or(true);
        cleared(&mut env);
        head.ok().filter(|_| !threw).map(|h| h as u32 as u64)
    }

    /// `AudioTrack.getTimestamp`, which an offloaded track answers from the chip's own count where its
    /// play head (`getRenderPosition`) fails, as on a Galaxy S22: the last one taken since the track
    /// began playing, moved on by the clock, as media3 does between its polls of the timestamp.
    fn timestamp(&mut self) -> Option<u64> {
        if self.playing {
            if let (Some(track), Some(stamp), Some((java, mut env))) = (&self.track, &self.timestamp, env()) {
                // SAFETY: AudioTrack.getTimestamp(AudioTimestamp) and the timestamp's two long fields,
                // looked up with these signatures.
                let got = unsafe {
                    env.call_method_unchecked(track, java.track.get_timestamp, ReturnType::Primitive(Primitive::Boolean), &[JValue::Object(stamp.as_obj()).as_jni()]).and_then(|v| v.z()).unwrap_or(false)
                };
                cleared(&mut env);
                if got {
                    let long = |env: &mut JNIEnv, f| env.get_field_unchecked(stamp, f, ReturnType::Primitive(Primitive::Long)).and_then(|v| v.j());
                    match (long(&mut env, java.frame_position), long(&mut env, java.nano_time)) {
                        // One from before the track was opened, flushed or began playing again is the old
                        // count's (the platform may hand it back after a flush), and one from the future
                        // is none: only a fresh one is a new anchor, the newest the clock moves on from.
                        (Ok(frames), Ok(ns)) if ns > self.since_ns && ns <= mono_ns() => {
                            if self.stamp.is_none_or(|(_, last, fresh)| !fresh || ns > last) {
                                self.stamp = Some((frames.max(0) as u64, ns, true));
                            }
                        }
                        (Ok(_), Ok(_)) => {}
                        _ => cleared(&mut env),
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

    /// In the log, and on the perf report's timeline as an "offload" event.
    fn note(&mut self, what: &str) {
        log(&format!("offload: {what}"));
        let wall_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        nori_perf::perf_log::perf_note(wall_ms, nori_perf::perf_log::PerfNote::Offload { detail: what.to_string() });
    }

    fn close(&mut self) {
        if self.track.is_some() {
            self.void(|j| Some(j.track.release));
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

/// Opens AudioTracks through Kotlin's `RustBridge.openTrack`.
struct JavaOpener {
    /// Android's API level: before 31 a track has no start threshold and starts only once full.
    sdk: i32,
}

impl Opener for JavaOpener {
    fn open(&mut self, format: OutputFormat, float: bool, frames: u64) -> Result<Opened, String> {
        let (java, mut env) = env().ok_or("no JVM")?;
        // `AudioFormat.ENCODING_*`: float, 24 bits packed for a song of more than 16 played as it is, or 16.
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
            ];
            // SAFETY: RustBridge.openTrack(int, int, int, int), looked up with this signature.
            let track = unsafe { env.call_static_method_unchecked(bridge(java), java.open_track, ReturnType::Object, &args) }.and_then(|v| v.l());
            let track = match track {
                Ok(t) if !t.is_null() => t,
                _ => {
                    cleared(env);
                    return Ok(Err("the AudioTrack would not open".into()));
                }
            };
            // SAFETY: AudioTrack.getBufferSizeInFrames(), looked up with this signature.
            let frames = unsafe { env.call_method_unchecked(&track, java.track.buffer_frames, ReturnType::Primitive(Primitive::Int), &[]) }.and_then(|v| v.i()).unwrap_or(0).max(0) as u64;
            cleared(env);
            let mut staging = vec![0f32; CHUNK_BYTES / 4];
            // SAFETY: the memory is `staging`'s, which is kept, never resized, beside the buffer for as
            // long as the buffer lives.
            let buffer = unsafe { env.new_direct_byte_buffer(staging.as_mut_ptr() as *mut u8, CHUNK_BYTES) }?;
            let timestamp_class = <&JClass>::from(java.timestamp.as_obj());
            // SAFETY: AudioTimestamp's no-argument constructor.
            let timestamp = unsafe { env.new_object_unchecked(timestamp_class, java.timestamp_new, &[]) }?;
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

// ---- the songs' bytes ----

/// A song's bytes through Kotlin's data sources, under the cache key the core resolved with its URL.
/// The player's own: a song the fetching ahead is on is handed over first ([`Ahead::take_over`]), so
/// that its bytes cross the network once.
struct JavaBytes {
    key: String,
    /// Its player's fetching ahead, which hands over a song it is on.
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

    /// A radio station's stream through `RustBridge.openLive`, uncached, asking for its announcements:
    /// the body, and the bytes of music between two of them (`icy-metaint`).
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
                Ok((Body { start: 0, len: None, reader: Box::new(JavaBody { body, open: true }) }, (icy > 0).then_some(icy as usize)))
            }
            _ => Err("the station's stream would not come".into()),
        }
    }
}

/// Numbers for the requests made through `RustBridge.open`, by which `RustBridge.cancel` calls one off.
static TICKETS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

/// A song's bytes from `from` on through `RustBridge.open`: a download, then media3's stream cache, then
/// the network, whatever is read written into the stream cache under `key`. The request goes by a ticket
/// of its own: `cancel` calls it off through `RustBridge.cancel` (the OkHttp call cancelled, a wait for the
/// cache entry's lock interrupted), which fails the open or the body's read at once.
fn open_java(url: &str, key: &str, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
    let ticket = TICKETS.fetch_add(1, Ordering::Relaxed);
    cancel.on_cancel(move || {
        if let Some((java, mut env)) = env() {
            // SAFETY: RustBridge.cancel(long), looked up with this signature.
            let _ = unsafe { env.call_static_method_unchecked(bridge(java), java.cancel, ReturnType::Primitive(Primitive::Void), &[JValue::Long(ticket).as_jni()]) };
            cleared(&mut env);
        }
    });
    let (java, mut env) = env().ok_or("no JVM")?;
    let body = env.with_local_frame(6, |env| -> jni::errors::Result<Option<Result<(GlobalRef, i64), OpenError>>> {
        let (url, key) = (env.new_string(url)?, env.new_string(key)?);
        // SAFETY: RustBridge.open(String, String, long, long), looked up with this signature.
        let args = [JValue::Object(&url).as_jni(), JValue::Object(&key).as_jni(), JValue::Long(from as i64).as_jni(), JValue::Long(ticket).as_jni()];
        let body = unsafe { env.call_static_method_unchecked(bridge(java), java.open, ReturnType::Object, &args) }?.l()?;
        if body.is_null() {
            return Ok(None);
        }
        let length = env.get_field_unchecked(&body, java.body_length, ReturnType::Primitive(Primitive::Long))?.j()?;
        // A range from past the song's end: no body, and `length` is the whole song's when the server said.
        if env.get_field_unchecked(&body, java.body_past, ReturnType::Primitive(Primitive::Boolean))?.z()? {
            return Ok(Some(Err(OpenError::PastEnd { len: (length >= 0).then_some(length as u64) })));
        }
        // The server answered with an error status: no body, and it was reached.
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
            Ok(Body { start: from, len: (length >= 0).then(|| from + length as u64), reader: Box::new(JavaBody { body, open: true }) })
        }
        _ => Err("the song's bytes would not come".into()),
    }
}

// ---- the songs after the next, fetched ahead ----

/// The fetching ahead's own door: through `RustBridge.open` as the player's, under the key it is told.
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

/// media3's stream cache, which keeps what is read through [`AheadBytes`] by itself: whether a song is
/// whole there, or being written by the player, is asked of Kotlin, once per song.
struct Media3Cache;

impl Media3Cache {
    fn ask(key: &str, method: fn(&Java) -> JStaticMethodID) -> Option<bool> {
        let (java, mut env) = env()?;
        let answer = env.with_local_frame(2, |env| -> jni::errors::Result<bool> {
            let key = env.new_string(key)?;
            // SAFETY: RustBridge.kept(String) / busy(String): boolean, looked up with this signature.
            unsafe { env.call_static_method_unchecked(bridge(java), method(java), ReturnType::Primitive(Primitive::Boolean), &[JValue::Object(&key).as_jni()]) }?.z()
        });
        cleared(&mut env);
        answer.ok()
    }
}

impl Keeping for Media3Cache {
    fn kept(&self, key: &str) -> bool {
        // Not known: taken as there, so nothing is fetched on a guess.
        Media3Cache::ask(key, |j| j.kept).unwrap_or(true)
    }

    fn busy(&self, key: &str) -> bool {
        Media3Cache::ask(key, |j| j.busy).unwrap_or(true)
    }

    fn entry(&self, _key: &str) -> Option<Box<dyn Entry>> {
        Some(Box::new(Counted(0)))
    }
}

/// An entry media3 writes itself as the bytes are read: only counted here.
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

/// `RustBridge.disk(key)` or `forget(key)`: Kotlin's words for what media3's stream cache keeps of `key`
/// (and, for `forget`, that it went). None when Kotlin could not be asked.
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

/// What the stream cache keeps of song `id`, in words, for the perf build's silent break
/// (nori_perf::invariants::describe_disk): its entry, length, spans and the length its metadata gives.
pub(crate) fn disk_words(id: &str) -> String {
    let Some(target) = nori_core::stream::resolve_now(id) else { return format!("{id}: no server to resolve it") };
    if target.key == nori_core::stream::download_key(id.to_string()) {
        return format!("{id}: downloaded");
    }
    cache_words(&target.key, |j| j.disk).unwrap_or_else(|| format!("{}: Kotlin could not be asked", target.key))
}

struct JavaBody {
    body: GlobalRef,
    open: bool,
}

impl Read for JavaBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let (java, mut env) = env().ok_or_else(|| io::Error::other("no JVM"))?;
        let max = buf.len().min(i32::MAX as usize) as i32;
        // SAFETY: RustBody.read(int), looked up with this signature.
        let n = unsafe { env.call_method_unchecked(&self.body, java.body_read, ReturnType::Primitive(Primitive::Int), &[JValue::Int(max).as_jni()]) }.and_then(|v| v.i());
        cleared(&mut env);
        let n = match n {
            Ok(-1) => return Ok(0),
            Ok(n) if n > 0 => (n as usize).min(buf.len()),
            _ => return Err(io::Error::other("the song's bytes stopped coming")),
        };
        let copied = env.with_local_frame(2, |env| -> jni::errors::Result<()> {
            let array = JByteArray::from(env.get_field_unchecked(&self.body, java.body_buffer, ReturnType::Array)?.l()?);
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
        if !std::mem::take(&mut self.open) {
            return;
        }
        if let Some((java, mut env)) = env() {
            // SAFETY: RustBody.close(), looked up with this signature.
            let _ = unsafe { env.call_method_unchecked(&self.body, java.body_close, ReturnType::Primitive(Primitive::Void), &[]) };
            cleared(&mut env);
        }
    }
}

// ---- where songs are ----

/// Every song is opened where the core resolves it - its URL and cache key - through [`JavaBytes`]; the
/// data source behind it reads a download or the stream cache first. An internet radio station is a
/// live stream at the address its queue item carries, which Kotlin hands over as it queues it ([`radio`]).
struct AndroidLibrary {
    stations: Arc<Mutex<Vec<(String, String)>>>,
    /// The player's one fetcher of the songs after the next (nori-engine's), into media3's stream cache.
    ahead: Arc<Ahead>,
}

impl Library for AndroidLibrary {
    fn locate(&mut self, id: &str) -> Result<Located, String> {
        if is_radio(id) {
            let url = self.stations.lock().iter().find(|(s, _)| s == id).map(|(_, u)| u.clone()).ok_or("a station with no address")?;
            log(&format!("{id} is a station's stream"));
            return Ok(Located { source: Source::Live { url, bytes: Arc::new(JavaBytes { key: String::new(), ahead: self.ahead.clone() }) }, hint: None, duration_ms: None, estimated: false });
        }
        let song = nori_core::queue::queue_song(id.to_string());
        let duration_ms = song.as_ref().map(|s| s.duration as i64 * 1000).filter(|&d| d > 0);
        // The container the resolved copy is in: a transcoded stream says it in its cache key. A download
        // may have been transcoded too, and its key (`dl:<id>`) says nothing: the file says what it is,
        // as on the desktop.
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
        nori_engine::core::about(id)
    }

    fn fetch_ahead(&self, id: &str) -> bool {
        nori_engine::core::fetch_ahead(id)
    }

    /// The songs the core names to fetch ahead (`precache_targets` for the network last told), but `next`,
    /// which the engine's loader fetches: into media3's stream cache, measured as they come with AutoMix on.
    fn ahead(&mut self, next: &str) {
        self.ahead.ask(Arc::new(Media3Cache), Arc::new(AheadBytes), ahead_songs(nori_core::stream::precache_now(), next), Some(measuring_ahead()));
    }

    fn taker(&self, id: &str, hint: Option<&str>) -> Option<Listening> {
        measure_as_it_comes(id, hint, false)
    }

    /// The song played nothing and is opened again from scratch: its stream cache entry goes (what it
    /// held is said first), a download stays.
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


// ---- the player ----

/// The engine's events, kept until Kotlin takes them: one call into Kotlin per batch.
#[derive(Default)]
struct Events {
    /// Each event as (kind, index, words, the jumps made when it was said: `Event::Song`'s `jumps`; for a
    /// stop, `Event::Stopped`'s and `Event::Bridge`'s `plays`).
    queue: Mutex<VecDeque<(i32, i32, String, u64)>>,
    signalled: AtomicBool,
    text: Mutex<String>,
    jumps: AtomicI64,
}

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

impl Events {
    fn push(&self, e: Event) {
        if let Event::Awake(awake) = e {
            cpu(awake);
            return;
        }
        match &e {
            Event::State(s) => log(&format!("{s:?}")),
            Event::Song { index, id, jumps } => log(&format!("song {index} ({id}) is heard, after jump {jumps}")),
            Event::Output { name } => log(&format!("playing to {name}")),
            Event::Stopped { plays } => log(&format!("stopped by itself, after play {plays}")),
            Event::Buffering(on) => log(if *on { "waits for the song's bytes" } else { "the song's bytes came" }),
            Event::Looped { index, .. } => log(&format!("song {index} again (repeat one)")),
            Event::Bridge { plays } => log(&format!("the network would not bring the song: the offline bridge takes over, after play {plays}")),
            Event::Placed { index, ms } => log(&format!("song {index} goes on at {ms} ms on another path")),
            _ => {}
        }
        let jumps = match &e {
            Event::Song { jumps, .. } | Event::Looped { jumps, .. } => *jumps,
            Event::Stopped { plays } | Event::Bridge { plays } => *plays,
            _ => 0,
        };
        let (kind, index, text) = match e {
            Event::State(s) => (EVENT_STATE, state_code(s), String::new()),
            Event::Song { index, id, .. } => (EVENT_SONG, index as i32, id),
            Event::Looped { index, id, .. } => (EVENT_LOOPED, index as i32, id),
            Event::Title(t) => (EVENT_TITLE, -1, t),
            Event::Bridge { .. } => (EVENT_BRIDGE, -1, String::new()),
            Event::Mixing(on) => (EVENT_MIXING, on as i32, String::new()),
            // The place is read from the status when the player builds its state again: only the index here.
            Event::Placed { index, .. } => (EVENT_PLACED, index as i32, String::new()),
            Event::Error { id, message } => (EVENT_ERROR, -1, if id.is_empty() { message } else { format!("{id}: {message}") }),
            Event::Output { name } => (EVENT_OUTPUT, -1, name),
            Event::Stopped { .. } => (EVENT_STOPPED, -1, String::new()),
            Event::Buffering(on) => (EVENT_BUFFERING, on as i32, String::new()),
            Event::Position { .. } | Event::Awake(_) => return,
        };
        let e = (kind, index, text, jumps);
        let first = {
            let mut q = self.queue.lock();
            q.push_back(e);
            !self.signalled.swap(true, Ordering::AcqRel)
        };
        if first {
            // Whether a player took the wake. None did (the engine's first events come before Kotlin has
            // registered it): the next event signals again rather than waiting for a drain that never
            // comes, and the player drains once as it registers.
            let taken = env().is_some_and(|(java, mut env)| {
                // SAFETY: RustBridge.signal(), looked up with this signature.
                let taken = unsafe { env.call_static_method_unchecked(bridge(java), java.signal, ReturnType::Primitive(Primitive::Boolean), &[]) }.and_then(|v| v.z()).unwrap_or(false);
                cleared(&mut env);
                taken
            });
            if !taken {
                self.signalled.store(false, Ordering::Release);
            }
        }
    }
}

/// `RustBridge.cpu`: the engine needs the CPU kept awake (the player takes its wake lock, if music is
/// wanted), or can let it sleep while the chip plays (it lets the lock go, unless a song's bytes are
/// being fetched). Called on the engine's thread, before it goes on: a lock asked for is held before
/// the work it is for.
fn cpu(awake: bool) {
    let Some((java, mut env)) = env() else { return };
    // SAFETY: RustBridge.cpu(boolean), looked up with this signature.
    let _ = unsafe { env.call_static_method_unchecked(bridge(java), java.cpu, ReturnType::Primitive(Primitive::Void), &[JValue::Bool(awake as jboolean).as_jni()]) };
    cleared(&mut env);
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
    /// What the offloaded track's stream events said, for the engine.
    offload: Arc<OffloadEvents>,
    /// The radio stations queued, by id, with their addresses.
    stations: Arc<Mutex<Vec<(String, String)>>>,
    /// The place the last jump asked for, and when: until the engine has looked at it, that is the
    /// place. media3 reads the position the moment a seek returns, and a controller runs its seek bar
    /// on from that reading, so the place before the jump would stay on screen.
    jumped: Mutex<Option<(i64, Instant)>>,
    /// When the engine was last asked to look ([`look_now`]), monotonic ms.
    looked_ms: AtomicI64,
    /// The music volume as Kotlin last told it ([`set_volume`]), for loudness compensation.
    volume: Arc<OutputVolume>,
    /// Its fetching ahead ([`AndroidLibrary::ahead`]), stopped when it goes.
    ahead: Arc<Ahead>,
}

/// The players alive, by the handle Kotlin holds. A handle is a number, never a pointer: a door called
/// with one that was destroyed (a routing callback or a test's read still on its way) finds nothing,
/// and a door that found one keeps it alive until it returns.
static PLAYERS: Mutex<Vec<(jlong, Arc<Player>)>> = Mutex::new(Vec::new());
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn player(h: jlong) -> Option<Arc<Player>> {
    if h == 0 {
        return None;
    }
    PLAYERS.lock().iter().find(|(k, _)| *k == h).map(|(_, p)| p.clone())
}

/// Starts the engine over the core's queue and settings. `sdk` is Android's API level; `float` the high
/// quality output setting, read once; `memory_mb` the app's memory class,
/// which sizes how much of a song is kept loaded. 0 when the Java side could not be found.
extern "system" fn create(mut env: JNIEnv, _: JClass, sdk: jint, float: jboolean, memory_mb: jint) -> jlong {
    if JAVA.get().is_none() {
        match look_up(&mut env) {
            Ok(j) => {
                let _ = JAVA.set(j);
            }
            Err(e) => {
                cleared(&mut env);
                nori_core::alog::info(&format!("rust player: the Java side is missing: {e}"));
                return 0;
            }
        }
    }
    let shared = Arc::new(Shared::default());
    let output = TrackOutput::new(Box::new(JavaOpener { sdk }), float != 0, shared.clone());
    let stations = Arc::new(Mutex::new(Vec::new()));
    let ahead = Ahead::new();
    let library = AndroidLibrary { stations: stations.clone(), ahead: ahead.clone() };
    // Kotlin tells the volume once loudness compensation is watched (VolumeWatch): all the way up until then.
    let volume = Arc::new(OutputVolume::default());
    let sound = nori_core::settings_store::settings_current().map(|p| settings(&p, volume.db())).unwrap_or_default();
    let watch = Some(nori_engine::watch::Watcher(Arc::new(crate::PerfWatch)));
    let config = Config { memory_mb: memory_mb.max(16) as u32, settings: sound, watch, ..Config::default() };
    let events = Arc::new(Events::default());
    let tell = events.clone();
    // Offload is Android 10's: before it the engine has no such output, and plays everything on the CPU.
    let offload = Arc::new(OffloadEvents::default());
    let chip = JAVA.get().is_some_and(|j| j.offload.is_some()) && sdk >= 29;
    let offloaded: Option<Box<dyn OffloadOutput>> = chip.then(|| Box::new(JavaOffload::new(offload.clone())) as Box<dyn OffloadOutput>);
    log(&format!("the engine starts: API {sdk}, {} output, {} MB of memory, offload {}", if float != 0 { "float" } else { "16-bit" }, config.memory_mb, if chip { "possible" } else { "not on this Android" }));
    let app = CoreApp::new().bridging().volume(volume.clone());
    let engine = Engine::start(library, app, CoreQueue, Box::new(output), offloaded, config, move |e| tell.push(e));
    let h = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    PLAYERS.lock().push((h, Arc::new(Player { engine, shared, events, offload, stations, jumped: Mutex::new(None), looked_ms: AtomicI64::new(i64::MIN / 2), volume, ahead })));
    h
}

/// Stops the engine: its thread ends and the AudioTrack is released. The handle names nothing from here
/// on; a door still running with the player lets it go when it returns. Stopping joins the engine's
/// threads, which can take a burst's decode: that is done on a thread of its own, not on the caller's
/// (media3 releases the player on the main thread).
extern "system" fn destroy(_: JNIEnv, _: JClass, h: jlong) {
    let gone = {
        let mut players = PLAYERS.lock();
        players.iter().position(|(k, _)| *k == h).map(|i| players.remove(i).1)
    };
    if let Some(p) = gone {
        // Nothing is fetched ahead for a player that is gone: a song on its way is left where it got to. Its
        // own fetching only: a player made since, whose songs are coming, keeps its.
        p.ahead.ask(Arc::new(Media3Cache), Arc::new(AheadBytes), Vec::new(), None);
        // A thread that will not start hands the player back, and it is let go here after all.
        let _ = std::thread::Builder::new().name("nori-release".into()).spawn(move || drop(p));
    }
}

/// A seek, a skip or a tap on a song: made at once while music plays, held until play while paused
/// (nori-engine's rule, `Engine::go_to`).
/// Answers the jump's number, which the song events it leads to carry ([`event_jumps`]); 0 when nothing
/// was sent.
extern "system" fn go_to(h: jlong, index: jint, ms: jlong) -> jlong {
    let (Some(p), Ok(i)) = (player(h), usize::try_from(index)) else { return 0 };
    log(&format!("to song {i} at {} ms, playing or not as it was", ms.max(0)));
    *p.jumped.lock() = Some((ms.max(0), Instant::now()));
    p.engine.go_to(i, ms.max(0)) as jlong
}

/// The sleep timer's "end of this song": the engine pauses there, on the next song.
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

/// Headphones pulled out (the audio became noisy): pause at once, no fade.
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

/// The equalizer's screen opened or closed: the shallow buffer while it is open.
extern "system" fn set_tuning(h: jlong, on: jboolean) {
    if let Some(p) = player(h) {
        p.engine.set_tuning(on != 0);
    }
}

/// The sound and the controls' fades as the core's settings are now.
extern "system" fn apply_settings(h: jlong) {
    if let (Some(p), Some(prefs)) = (player(h), nori_core::settings_store::settings_current()) {
        p.engine.set_settings(settings(&prefs, p.volume.db()));
    }
}

/// Where the ear is in the song heard, now: the engine's last reading run on at the playing speed, or
/// the place a jump asked for while the engine has not made it yet (it has not looked, or the music is
/// still dipping before it).
extern "system" fn position_ms(h: jlong) -> jlong {
    let Some(p) = player(h) else { return 0 };
    // Read in place: this is asked every frame the seek bar draws, and a copy of the status is a copy of
    // the song's id.
    let (at, switching, now) = p.engine.status_with(|s| (s.at, s.switching, s.position_now()));
    let jumped = *p.jumped.lock();
    let jump = jumped.map(|(ms, when)| (ms, when.elapsed().as_millis() as i64));
    nori_player::transport::shown_place(jump, jumped.is_none_or(|(_, when)| at >= when), switching, now)
}

/// [`position_ms`] for the seek bar on screen, in queue index `index` (-1 when the engine's song is another,
/// the page not having followed yet): the engine's last reading run on for at most
/// `nori_player::heard::RUN_ON_MS`, and the engine asked to read its output again when that reading is a
/// second old (`Status::screen_now`) - once a second at most, whether or not it answers.
extern "system" fn shown_ms(h: jlong, index: jint) -> jlong {
    let Some(p) = player(h) else { return -1 };
    let (song, at, switching, (now, stale)) = p.engine.status_with(|s| (s.index, s.at, s.switching, s.screen_now()));
    if song.is_none_or(|i| i as jint != index) {
        return -1;
    }
    if stale {
        look_now(&p);
    }
    let jumped = *p.jumped.lock();
    let jump = jumped.map(|(ms, when)| (ms, when.elapsed().as_millis() as i64));
    nori_player::transport::shown_place(jump, jumped.is_none_or(|(_, when)| at >= when), switching, now)
}

/// The engine reads its output once, now: the screen is coming back.
extern "system" fn look(h: jlong) {
    if let Some(p) = player(h) {
        look_now(&p);
    }
}

/// [`Engine::look`], at most once per `nori_player::heard::LOOK_AFTER_MS`: an engine that has nothing to
/// say (let go, waiting for a song) is not woken every frame for it.
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

/// Whether the sound chain is in the samples' path.
extern "system" fn chain_in(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.chain)) as jboolean
}

/// Whether the ear is on music the CPU made, through the engine's own output (`Status::on_cpu`).
extern "system" fn on_cpu(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.on_cpu)) as jboolean
}

/// The limiter's meter: what it took off the last buffer through the chain, dB.
extern "system" fn gain_reduction_db(h: jlong) -> jfloat {
    player(h).map_or(0.0, |p| p.engine.status_with(|s| s.gain_reduction_db))
}

/// The music volume is now step `index` of `max` (`db` the platform's own figure for it, NaN without
/// one), told only while loudness compensation is on: the chain is set up again when that moves the
/// sound (`OutputVolume::set`), and not otherwise.
extern "system" fn set_volume(h: jlong, index: jint, max: jint, db: jfloat) {
    let db = nori_player::contour::volume_db(index, max, db);
    let Some(p) = player(h) else { return };
    if !p.volume.set(db) {
        return;
    }
    if let Some(prefs) = nori_core::settings_store::settings_current() {
        if prefs.loudness {
            // Once per step of the volume keys at most: the line says what the chain was set up for.
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
    }
}

/// The compressor's meter: what it took off the last buffer through the chain, dB.
extern "system" fn compression_db(h: jlong) -> jfloat {
    player(h).map_or(0.0, |p| p.engine.status_with(|s| s.compression_db))
}

extern "system" fn bytes_written(h: jlong) -> jlong {
    player(h).map_or(0, |p| p.shared.bytes.load(Ordering::Relaxed) as jlong)
}

/// The next event, `kind << 32 | index` (state: its code), its words kept for [`event_text`]; -1 when
/// there is none, and the next event after that calls Kotlin again.
extern "system" fn event(h: jlong) -> jlong {
    let Some(p) = player(h) else { return -1 };
    let mut q = p.events.queue.lock();
    // A stop said before a play Kotlin has asked for since is over: that play starts the music again, and
    // Kotlin, letting its "wants to play" go on it, would show paused over music playing
    // (`Engine::superseded`). It is not handed over.
    while let Some(&(kind, _, _, plays)) = q.front() {
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
        Some((kind, index, text, jumps)) => {
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

/// The words of the event [`event`] last gave: the song's id, the error, the output's name. `@FastNative`:
/// only the main thread takes this lock (here and in [`event`]), and nothing Java is called.
extern "system" fn event_text(env: JNIEnv, _: JClass, h: jlong) -> jstring {
    let Some(p) = player(h) else { return std::ptr::null_mut() };
    let text = std::mem::take(&mut *p.events.text.lock());
    java_string(&env, &text)
}

/// How many jumps the engine had made when it said the event [`event`] last gave (a song's or a loop's):
/// one from before the last jump Kotlin asked for is from the place it has already left.
extern "system" fn event_jumps(h: jlong) -> jlong {
    player(h).map_or(0, |p| p.events.jumps.load(Ordering::Relaxed))
}

/// What the platform knows of the output: a USB device attached (offload stands down), a DAC playing
/// bit-perfect (nothing touches the samples).
extern "system" fn set_output(h: jlong, usb: jboolean, bit_perfect: jboolean) {
    if let Some(p) = player(h) {
        p.engine.set_output(OutputFacts { usb: usb != 0, bit_perfect: bit_perfect != 0 });
    }
}

/// The offloaded track's `StreamEventCallback`, on the platform's callback thread: `kind` 0 it wants more
/// (`onDataRequest`), 1 it played everything up to the end of stream (`onPresentationEnded`), 2 it was
/// torn down (`onTearDown`). The engine's thread is woken to act on it.
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

/// Whether the songs go to the audio chip now: for the perf report and the test bridge.
extern "system" fn offloaded(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.offloaded)) as jboolean
}

/// Whether the settings and the output let the songs go to the audio chip.
extern "system" fn offload_wanted(h: jlong) -> jboolean {
    player(h).is_some_and(|p| p.engine.status_with(|s| s.offload_wanted)) as jboolean
}

/// Why the music plays on the CPU and not on the audio chip, in words (the engine's `Status::pcm_why`):
/// for the perf report. Null while offloaded.
extern "system" fn pcm_why(env: JNIEnv, _: JClass, h: jlong) -> jstring {
    match player(h).and_then(|p| p.engine.status_with(|s| s.pcm_why.clone())) {
        Some(why) => java_string(&env, &why),
        None => std::ptr::null_mut(),
    }
}

/// A radio station queued as `id`, whose stream is at `url` (the queue item's address).
extern "system" fn radio(mut env: JNIEnv, _: JClass, h: jlong, id: JString, url: JString) {
    let Some(p) = player(h) else { return };
    let (Some(id), Some(url)) = (with_str(&mut env, &id, str::to_string), with_str(&mut env, &url, str::to_string)) else { return };
    let mut s = p.stations.lock();
    s.retain(|(i, _)| *i != id);
    s.push((id, url));
}

/// The track's route changed: `kind` is the device's `AudioDeviceInfo.TYPE_*`, `name` its product name.
extern "system" fn device(mut env: JNIEnv, _: JClass, h: jlong, kind: jint, name: JString) {
    let Some(p) = player(h) else { return };
    let name = with_str(&mut env, &name, str::to_string).unwrap_or_default();
    let watch = p.shared.watch.lock();
    if let Some(watch) = &*watch {
        watch(Device { kind: nori_core::outputs::kind(kind), name });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_s_offload_answer_is_read_as_media3_reads_it() {
        let cases = [
            // Android 13 and later: getDirectPlaybackSupport, a set of flags.
            (3 << 8, Support::No, "getDirectPlaybackSupport: NOT_SUPPORTED"),
            ((3 << 8) | 1, Support::Plain, "getDirectPlaybackSupport: OFFLOAD_SUPPORTED"),
            ((3 << 8) | 3, Support::Gapless, "getDirectPlaybackSupport: OFFLOAD_GAPLESS_SUPPORTED"),
            ((3 << 8) | 7, Support::Gapless, "getDirectPlaybackSupport: OFFLOAD_GAPLESS_SUPPORTED | BITSTREAM_SUPPORTED"),
            ((3 << 8) | 4, Support::No, "getDirectPlaybackSupport: NOT_SUPPORTED | BITSTREAM_SUPPORTED"),
            // Android 12: getPlaybackOffloadSupport, whose gapless answer media3 does not trust there.
            (2 << 8, Support::No, "getPlaybackOffloadSupport: NOT_SUPPORTED"),
            ((2 << 8) | 1, Support::Plain, "getPlaybackOffloadSupport: SUPPORTED"),
            ((2 << 8) | 2, Support::Plain, "getPlaybackOffloadSupport: GAPLESS_SUPPORTED, taken as without gaps before Android 13"),
            // Android 10 and 11: yes or no, and nothing of gaps.
            (1 << 8, Support::No, "isOffloadedPlaybackSupported: false"),
            ((1 << 8) | 1, Support::Plain, "isOffloadedPlaybackSupported: true, which says nothing of gaps"),
            (-1, Support::No, "the platform could not be asked"),
        ];
        for (answer, support, words) in cases {
            assert_eq!(offload_support(answer), (support, words.to_string()), "{answer:#x}");
        }
    }
}
