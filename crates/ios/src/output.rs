//! The iPod's sound card: [`IosOutput`] over a [`Sink`].
//!
//! The real sink is AURemoteIO (`ios/Sound/NoriAudio.m`). Tests use a simulated one. The render
//! callback is [`nori_ios_render`]: the audio unit calls a C function, which has no Rust pointer of
//! ours, so the playing feed is a static. That path pulls and stamps; it does not lock or allocate.

use std::ffi::{c_char, CStr};
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering,
};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use nori_engine::{AudioOutput, Device, DeviceWatch, Feed, OutputFormat, OutputKind};

/// Deep I/O buffer: the longest iOS grants in practice, about 11 wakes a second.
pub const DEEP_IO_MS: u32 = 93;
/// Equalizer in sight: a band moved is heard within a frame or two.
pub const SHALLOW_IO_MS: u32 = 10;

/// Port codes the shim sends. Not [`OutputKind`]'s discriminants: those are the engine's.
pub const PORT_SPEAKER: i32 = 1;
pub const PORT_WIRED: i32 = 2;
pub const PORT_BLUETOOTH: i32 = 3;

/// What the session granted, and the route it is playing through.
pub struct Grant {
    pub rate: u32,
    pub io_ms: u32,
    pub latency_us: u64,
    pub kind: OutputKind,
    pub name: String,
}

/// The audio unit, as the output drives it. The render callback is the other direction.
pub trait Sink: Send {
    fn open(&mut self, rate: u32, channels: usize, io_ms: u32) -> Result<Grant, String>;
    fn start(&mut self) -> Result<(), String>;
    fn stop(&mut self);
    /// Asks for an I/O buffer of `io_ms` and returns what the session granted.
    fn set_io_ms(&mut self, io_ms: u32) -> Result<u32, String>;
    fn route(&mut self) -> (OutputKind, String, u64);
    fn close(&mut self);
}

pub fn kind_of(port: i32) -> OutputKind {
    match port {
        PORT_SPEAKER => OutputKind::Speaker,
        PORT_WIRED => OutputKind::Wired,
        PORT_BLUETOOTH => OutputKind::Bluetooth,
        _ => OutputKind::Other,
    }
}

fn port_of(kind: OutputKind) -> i32 {
    match kind {
        OutputKind::Speaker => PORT_SPEAKER,
        OutputKind::Wired => PORT_WIRED,
        OutputKind::Bluetooth => PORT_BLUETOOTH,
        _ => 0,
    }
}

/// When the last frame rendered will have been heard.
struct Heard {
    until_us: AtomicU64,
}

impl Default for Heard {
    fn default() -> Self {
        Heard {
            until_us: AtomicU64::new(0),
        }
    }
}

impl Heard {
    /// A callback at `now_us` rendered `frames` at `rate`, heard `delay_us` after it starts.
    fn pulled(&self, now_us: u64, delay_us: u64, frames: usize, rate: u32) {
        let play = frames as u64 * 1_000_000 / rate.max(1) as u64;
        self.until_us.store(
            now_us.saturating_add(delay_us).saturating_add(play),
            Ordering::Relaxed,
        );
    }

    fn left_us(&self, now_us: u64) -> u64 {
        self.until_us.load(Ordering::Relaxed).saturating_sub(now_us)
    }
}

/// The playing feed, for [`nori_ios_render`]. Written while the unit is stopped.
static FEED: AtomicPtr<Feed> = AtomicPtr::new(std::ptr::null_mut());
static RATE: AtomicU32 = AtomicU32::new(44_100);
static CHANNELS: AtomicU32 = AtomicU32::new(2);
static LATENCY: AtomicU64 = AtomicU64::new(0);
static KIND: AtomicI32 = AtomicI32::new(0);
/// A media-services reset the engine has not reopened for yet.
static RESET: AtomicBool = AtomicBool::new(false);
static WATCH: Mutex<Option<std::sync::Arc<DeviceWatch>>> = Mutex::new(None);
static LOST: AtomicBool = AtomicBool::new(false);
static INTERRUPT: AtomicU8 = AtomicU8::new(0);

fn render_heard() -> &'static Heard {
    static HEARD: OnceLock<Heard> = OnceLock::new();
    HEARD.get_or_init(Heard::default)
}

/// Fills `out` from the feed (silence when there is none) and stamps when it will be heard.
fn paint(
    feed: *mut Feed,
    heard: &Heard,
    out: &mut [f32],
    channels: usize,
    rate: u32,
    delay_us: u64,
    now_us: u64,
) {
    if feed.is_null() {
        out.fill(0.0);
    } else {
        // SAFETY: published while the unit is stopped, and the unit is stopped before it is cleared.
        unsafe { (*feed).pull(out) };
    }
    let frames = out.len() / channels.max(1);
    heard.pulled(now_us, delay_us, frames, rate);
}

/// The audio unit's render callback.
///
/// # Safety
/// `out` is `frames` × the channel count set at open, interleaved float, for this call only.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_render(frames: u32, out: *mut f32) {
    if out.is_null() || frames == 0 {
        return;
    }
    let channels = CHANNELS.load(Ordering::Relaxed).max(1) as usize;
    // SAFETY: the caller's promise above.
    let buf = unsafe { std::slice::from_raw_parts_mut(out, frames as usize * channels) };
    paint(
        FEED.load(Ordering::Acquire),
        render_heard(),
        buf,
        channels,
        RATE.load(Ordering::Relaxed),
        LATENCY.load(Ordering::Relaxed),
        HostClock.now_us(),
    );
}

/// The shim's route notification. `unavailable`: headphones pulled, or a Bluetooth device gone.
///
/// # Safety
/// `name` is a NUL-terminated UTF-8 string, or null.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_route(
    port: i32,
    name: *const c_char,
    latency_us: u64,
    unavailable: i32,
) {
    LATENCY.store(latency_us, Ordering::Relaxed);
    KIND.store(port, Ordering::Relaxed);
    if unavailable != 0 {
        LOST.store(true, Ordering::Release);
    }
    let name = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    };
    if let Some(w) = WATCH.lock().unwrap_or_else(|p| p.into_inner()).clone() {
        w(Device {
            kind: kind_of(port),
            name,
        });
    }
    #[cfg(target_os = "ios")]
    crate::session::audio_changed();
}

/// A line from the shim for the core's log (the audio unit's builds and the hardware's rate).
///
/// # Safety
/// `line` is a NUL-terminated UTF-8 string, or null.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_audio_log(line: *const c_char) {
    if !line.is_null() {
        nori_core::alog::info(&unsafe { CStr::from_ptr(line) }.to_string_lossy());
    }
}

/// The shim's media-services reset. The engine reopens on its next turn ([`AudioOutput::failed`]).
#[no_mangle]
pub extern "C" fn nori_ios_media_reset() {
    RESET.store(true, Ordering::Release);
    let feed = FEED.load(Ordering::Acquire);
    if !feed.is_null() {
        // SAFETY: the same promise as [`nori_ios_render`]: published only while the feed is alive.
        unsafe { (*feed).wake_engine() };
    }
}

/// The shim's interruption. `began` non-zero starts it; `resume` is the session's should-resume.
#[no_mangle]
pub extern "C" fn nori_ios_interruption(began: i32, resume: i32) {
    let code = if began != 0 {
        1
    } else if resume != 0 {
        2
    } else {
        3
    };
    INTERRUPT.store(code, Ordering::Release);
    #[cfg(target_os = "ios")]
    crate::session::audio_changed();
}

/// An audio interruption, as the session reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    Began,
    Ended { resume: bool },
}

/// The interruption stored by [`nori_ios_interruption`], once.
pub fn take_interruption() -> Option<Interrupt> {
    match INTERRUPT.swap(0, Ordering::AcqRel) {
        1 => Some(Interrupt::Began),
        2 => Some(Interrupt::Ended { resume: true }),
        3 => Some(Interrupt::Ended { resume: false }),
        _ => None,
    }
}

/// Headphones or a Bluetooth device went away since last asked.
pub fn take_route_lost() -> bool {
    LOST.swap(false, Ordering::AcqRel)
}

/// Host time in µs from an arbitrary base. The render path reads it; tests bring their own.
pub trait Clock: Send {
    fn now_us(&self) -> u64;
}

pub struct HostClock;

impl Clock for HostClock {
    fn now_us(&self) -> u64 {
        static BASE: OnceLock<Instant> = OnceLock::new();
        BASE.get_or_init(Instant::now).elapsed().as_micros() as u64
    }
}

/// The engine's output. `publish` is the process's one card: its feed is what [`nori_ios_render`] pulls.
pub struct IosOutput<C: Clock = HostClock> {
    sink: Box<dyn Sink>,
    clock: C,
    heard: Heard,
    /// Use the process-wide render state. One output does, the tests' do not.
    publish: bool,
    format: Option<OutputFormat>,
    channels: usize,
    rate: u32,
    io_ms: u32,
    shallow: bool,
    opened: bool,
    playing: bool,
    route_latency_us: u64,
    kind: OutputKind,
    watch: Option<std::sync::Arc<DeviceWatch>>,
    failed: Option<String>,
    /// [`IosOutput::note_reset`]: a reset the next [`AudioOutput::failed`] reopens for.
    reset: bool,
    feed: Option<Box<Feed>>,
}

impl<C: Clock> IosOutput<C> {
    pub fn new(sink: Box<dyn Sink>, clock: C) -> IosOutput<C> {
        IosOutput {
            sink,
            clock,
            heard: Heard::default(),
            publish: false,
            format: None,
            channels: 2,
            rate: 44_100,
            io_ms: DEEP_IO_MS,
            shallow: false,
            opened: false,
            playing: false,
            route_latency_us: 0,
            kind: OutputKind::Speaker,
            watch: None,
            failed: None,
            reset: false,
            feed: None,
        }
    }

    /// The route moved. Tells the watcher the sink's current route.
    pub fn route_changed(&mut self) {
        let (kind, name, latency) = self.sink.route();
        self.note_route(kind, name, latency);
    }

    /// A media-services reset, for a test. The device sets the same flag from [`nori_ios_media_reset`].
    pub fn note_reset(&mut self) {
        self.reset = true;
    }

    /// One period, for tests. The device's callback is [`nori_ios_render`].
    pub fn render(&mut self, out: &mut [f32]) {
        let feed = self
            .feed
            .as_mut()
            .map(|f| &mut **f as *mut Feed)
            .unwrap_or(std::ptr::null_mut());
        let heard = if self.publish {
            render_heard()
        } else {
            &self.heard
        };
        paint(
            feed,
            heard,
            out,
            self.channels.max(1),
            self.rate,
            self.delay_us(),
            self.clock.now_us(),
        );
    }

    fn delay_us(&self) -> u64 {
        if self.publish {
            LATENCY.load(Ordering::Relaxed)
        } else {
            self.route_latency_us
        }
    }

    fn note_route(&mut self, kind: OutputKind, name: String, latency_us: u64) {
        self.kind = kind;
        self.route_latency_us = latency_us;
        if self.publish {
            KIND.store(port_of(kind), Ordering::Relaxed);
            LATENCY.store(latency_us, Ordering::Relaxed);
        }
        if let Some(w) = &self.watch {
            w(Device { kind, name });
        }
    }

    fn publish_format(&self) {
        if !self.publish {
            return;
        }
        RATE.store(self.rate, Ordering::Relaxed);
        CHANNELS.store(self.channels.max(1) as u32, Ordering::Relaxed);
        LATENCY.store(self.route_latency_us, Ordering::Relaxed);
        KIND.store(port_of(self.kind), Ordering::Relaxed);
    }

    fn publish_feed(&self) {
        if !self.publish {
            return;
        }
        let p = self
            .feed
            .as_ref()
            .map(|f| &**f as *const Feed as *mut Feed)
            .unwrap_or(std::ptr::null_mut());
        FEED.store(p, Ordering::Release);
    }

    fn clear_feed(&mut self) {
        if self.publish {
            FEED.store(std::ptr::null_mut(), Ordering::Release);
        }
        self.feed = None;
    }

    /// Opens again after a reset. One attempt: the engine reads the error from [`AudioOutput::failed`].
    fn reopen(&mut self) -> Result<(), String> {
        let Some(fmt) = self.format else {
            return Ok(());
        };
        self.sink.stop();
        let g = self.sink.open(fmt.rate, fmt.channels, self.io_ms)?;
        self.took(g);
        if self.playing {
            self.sink.start()?;
        }
        Ok(())
    }

    fn took(&mut self, g: Grant) {
        self.rate = g.rate;
        self.io_ms = g.io_ms;
        self.opened = true;
        self.format = Some(OutputFormat {
            rate: g.rate,
            channels: self.channels,
            bits: 0,
        });
        self.note_route(g.kind, g.name, g.latency_us);
        self.publish_format();
    }

    fn apply_io(&mut self) -> Result<(), String> {
        let ms = self.sink.set_io_ms(self.io_ms)?;
        self.io_ms = ms;
        let (kind, name, latency) = self.sink.route();
        self.note_route(kind, name, latency);
        Ok(())
    }
}

#[cfg(target_os = "ios")]
impl IosOutput<HostClock> {
    /// The process's output, over the audio unit in `ios/Sound/NoriAudio.m`.
    pub fn device() -> IosOutput<HostClock> {
        let mut out = IosOutput::new(Box::new(DeviceSink), HostClock);
        out.publish = true;
        out
    }
}

impl<C: Clock> Drop for IosOutput<C> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl<C: Clock> IosOutput<C> {
    fn shutdown(&mut self) {
        self.sink.stop();
        self.clear_feed();
        if self.opened {
            self.sink.close();
            self.opened = false;
        }
        self.playing = false;
        if self.publish {
            *WATCH.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }
}

impl<C: Clock> AudioOutput for IosOutput<C> {
    fn watch(&mut self, changed: DeviceWatch) {
        let w = std::sync::Arc::new(changed);
        if self.publish {
            *WATCH.lock().unwrap_or_else(|p| p.into_inner()) = Some(w.clone());
        }
        self.watch = Some(w);
    }

    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        self.channels = want.channels.max(1);
        let g = self.sink.open(want.rate, self.channels, self.io_ms)?;
        self.took(g);
        Ok(self.format.expect("just opened"))
    }

    fn start(&mut self, feed: Feed) -> Result<(), String> {
        // Starts paused: the engine resumes when it wants sound.
        self.sink.stop();
        self.clear_feed();
        self.feed = Some(Box::new(feed));
        self.publish_feed();
        self.playing = false;
        Ok(())
    }

    fn pause(&mut self) {
        self.playing = false;
        self.sink.stop();
    }

    fn resume(&mut self) {
        self.playing = true;
        if let Err(e) = self.sink.start() {
            self.failed = Some(e);
        }
    }

    fn latency_us(&self) -> u64 {
        let heard = if self.publish {
            render_heard()
        } else {
            &self.heard
        };
        heard.left_us(self.clock.now_us())
    }

    fn mixed_us(&self) -> u64 {
        let (kind, latency) = if self.publish {
            (
                kind_of(KIND.load(Ordering::Relaxed)),
                LATENCY.load(Ordering::Relaxed),
            )
        } else {
            (self.kind, self.route_latency_us)
        };
        if kind == OutputKind::Bluetooth {
            latency
        } else {
            0
        }
    }

    fn takes_float(&mut self) -> bool {
        true
    }

    fn holding(&self) -> bool {
        self.latency_us() > 0
    }

    fn shallow(&mut self, on: bool) {
        if on == self.shallow {
            return;
        }
        self.shallow = on;
        self.io_ms = if on { SHALLOW_IO_MS } else { DEEP_IO_MS };
        if self.opened {
            if let Err(e) = self.apply_io() {
                self.failed = Some(e);
            }
        }
    }

    fn failed(&mut self) -> Option<String> {
        let reset = self.reset || (self.publish && RESET.swap(false, Ordering::AcqRel));
        self.reset = false;
        if reset {
            if let Err(e) = self.reopen() {
                return Some(e);
            }
        }
        self.failed.take()
    }

    fn close(&mut self) {
        self.shutdown();
    }
}

/// The output music plays to now, keyed as the engine keys outputs (`nori_player::outputs::key`), read
/// from the audio session itself, so it holds whether or not the engine's output is open. Empty off the
/// device.
pub(crate) fn route_key() -> String {
    #[cfg(target_os = "ios")]
    {
        let mut g = AudioGrant { rate: 0, io_ms: 0, latency_us: 0, port: 0, name: [0; 128] };
        // SAFETY: the shim fills the struct it is given and keeps nothing.
        unsafe { nori_audio_route(&mut g) };
        nori_player::outputs::key(kind_of(g.port), &name_of(&g.name))
    }
    #[cfg(not(target_os = "ios"))]
    {
        String::new()
    }
}

/// What the ObjC shim returns. Kept in step with `NoriGrant` in `ios/Sound/NoriAudio.h`.
#[cfg(target_os = "ios")]
#[repr(C)]
struct AudioGrant {
    rate: u32,
    io_ms: u32,
    latency_us: u64,
    port: i32,
    name: [u8; 128],
}

#[cfg(target_os = "ios")]
extern "C" {
    fn nori_audio_open(
        rate: u32,
        channels: u32,
        io_ms: u32,
        out: *mut AudioGrant,
        err: *mut u8,
        err_len: u32,
    ) -> i32;
    fn nori_audio_start(err: *mut u8, err_len: u32) -> i32;
    fn nori_audio_stop();
    fn nori_audio_set_io_ms(io_ms: u32, granted_ms: *mut u32, err: *mut u8, err_len: u32) -> i32;
    fn nori_audio_route(out: *mut AudioGrant);
    fn nori_audio_close();
}

#[cfg(target_os = "ios")]
struct DeviceSink;

#[cfg(target_os = "ios")]
fn err_buf() -> [u8; 256] {
    [0; 256]
}

#[cfg(target_os = "ios")]
fn take_err(buf: &[u8], fallback: &str) -> String {
    let n = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let s = String::from_utf8_lossy(&buf[..n]);
    if s.is_empty() {
        fallback.to_string()
    } else {
        s.into_owned()
    }
}

#[cfg(target_os = "ios")]
fn name_of(bytes: &[u8]) -> String {
    let n = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..n]).into_owned()
}

#[cfg(target_os = "ios")]
impl DeviceSink {
    fn grant(g: AudioGrant) -> Grant {
        Grant {
            rate: g.rate,
            io_ms: g.io_ms,
            latency_us: g.latency_us,
            kind: kind_of(g.port),
            name: name_of(&g.name),
        }
    }
}

#[cfg(target_os = "ios")]
impl Sink for DeviceSink {
    fn open(&mut self, rate: u32, channels: usize, io_ms: u32) -> Result<Grant, String> {
        let mut g = AudioGrant {
            rate: 0,
            io_ms: 0,
            latency_us: 0,
            port: 0,
            name: [0; 128],
        };
        let mut err = err_buf();
        let rc = unsafe {
            nori_audio_open(
                rate,
                channels as u32,
                io_ms,
                &mut g,
                err.as_mut_ptr(),
                err.len() as u32,
            )
        };
        if rc != 0 {
            return Err(take_err(&err, "the audio unit would not open"));
        }
        Ok(Self::grant(g))
    }

    fn start(&mut self) -> Result<(), String> {
        let mut err = err_buf();
        let rc = unsafe { nori_audio_start(err.as_mut_ptr(), err.len() as u32) };
        if rc != 0 {
            Err(take_err(&err, "the output would not start"))
        } else {
            Ok(())
        }
    }

    fn stop(&mut self) {
        unsafe { nori_audio_stop() };
    }

    fn set_io_ms(&mut self, io_ms: u32) -> Result<u32, String> {
        let mut granted = 0u32;
        let mut err = err_buf();
        let rc = unsafe {
            nori_audio_set_io_ms(io_ms, &mut granted, err.as_mut_ptr(), err.len() as u32)
        };
        if rc != 0 {
            Err(take_err(&err, "the buffer would not change"))
        } else {
            Ok(granted)
        }
    }

    fn route(&mut self) -> (OutputKind, String, u64) {
        let mut g = AudioGrant {
            rate: 0,
            io_ms: 0,
            latency_us: 0,
            port: 0,
            name: [0; 128],
        };
        unsafe { nori_audio_route(&mut g) };
        (kind_of(g.port), name_of(&g.name), g.latency_us)
    }

    fn close(&mut self) {
        unsafe { nori_audio_close() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    use nori_engine::AudioOutput;

    struct State {
        grant_rate: Option<u32>,
        latency_us: u64,
        kind: OutputKind,
        name: String,
        opens: Vec<(u32, usize, u32)>,
        io_sets: Vec<u32>,
        starts: u32,
        fail_start: bool,
    }

    #[derive(Clone)]
    struct Sim(Arc<Mutex<State>>);

    impl Sim {
        fn new() -> Sim {
            Sim(Arc::new(Mutex::new(State {
                grant_rate: None,
                latency_us: 12_000,
                kind: OutputKind::Wired,
                name: "Headphones".into(),
                opens: Vec::new(),
                io_sets: Vec::new(),
                starts: 0,
                fail_start: false,
            })))
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, State> {
            self.0.lock().unwrap_or_else(|p| p.into_inner())
        }
    }

    impl Sink for Sim {
        fn open(&mut self, rate: u32, channels: usize, io_ms: u32) -> Result<Grant, String> {
            let mut s = self.lock();
            s.opens.push((rate, channels, io_ms));
            Ok(Grant {
                rate: s.grant_rate.unwrap_or(rate),
                io_ms,
                latency_us: s.latency_us,
                kind: s.kind,
                name: s.name.clone(),
            })
        }

        fn start(&mut self) -> Result<(), String> {
            let mut s = self.lock();
            if s.fail_start {
                return Err("the output would not start".into());
            }
            s.starts += 1;
            Ok(())
        }

        fn stop(&mut self) {}

        fn set_io_ms(&mut self, io_ms: u32) -> Result<u32, String> {
            self.lock().io_sets.push(io_ms);
            Ok(io_ms)
        }

        fn route(&mut self) -> (OutputKind, String, u64) {
            let s = self.lock();
            (s.kind, s.name.clone(), s.latency_us)
        }

        fn close(&mut self) {}
    }

    #[derive(Clone)]
    struct Manual(Arc<AtomicU64>);

    impl Clock for Manual {
        fn now_us(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }

    fn stereo(rate: u32) -> OutputFormat {
        OutputFormat {
            rate,
            channels: 2,
            bits: 0,
        }
    }

    fn opened(sim: &Sim, clock: &Manual) -> IosOutput<Manual> {
        let mut out = IosOutput::new(Box::new(sim.clone()), clock.clone());
        out.open(stereo(44_100)).unwrap();
        out
    }

    #[test]
    fn the_session_grants_the_rate() {
        let sim = Sim::new();
        sim.lock().grant_rate = Some(48_000);
        let clock = Manual(Arc::new(AtomicU64::new(0)));
        let mut out = IosOutput::new(Box::new(sim.clone()), clock);
        let got = out.open(stereo(44_100)).unwrap();
        assert_eq!(
            got.rate, 48_000,
            "the engine resamples to what the session gave"
        );
        assert_eq!(sim.lock().opens, vec![(44_100, 2, DEEP_IO_MS)]);
        assert!(out.takes_float());
        assert!(!out.bursts());
        assert!(!out.ramp(None, 0.0, 20));
    }

    #[test]
    fn latency_is_the_route_plus_what_the_callback_took() {
        let sim = Sim::new();
        let clock = Manual(Arc::new(AtomicU64::new(1_000_000)));
        let mut out = opened(&sim, &clock);
        let mut buf = [1.0f32; 256];
        out.render(&mut buf);
        assert!(
            buf.iter().all(|s| *s == 0.0),
            "nothing is playing yet: silence"
        );
        // 128 frames at 44.1 kHz is 2902 µs, on top of the route's 12 ms.
        assert_eq!(out.latency_us(), 12_000 + 128 * 1_000_000 / 44_100);
        assert!(out.holding());
        clock
            .0
            .store(1_000_000 + out.latency_us(), Ordering::Relaxed);
        assert_eq!(out.latency_us(), 0, "heard");
        assert_eq!(out.mixed_us(), 0, "a wired route has nothing mixed ahead");
    }

    #[test]
    fn shallow_asks_for_the_short_buffer() {
        let sim = Sim::new();
        let clock = Manual(Arc::new(AtomicU64::new(0)));
        let mut out = IosOutput::new(Box::new(sim.clone()), clock);
        out.shallow(true);
        out.open(stereo(44_100)).unwrap();
        assert_eq!(sim.lock().opens[0].2, SHALLOW_IO_MS);
        out.shallow(false);
        assert_eq!(sim.lock().io_sets, vec![DEEP_IO_MS]);
        out.shallow(false);
        assert_eq!(sim.lock().io_sets.len(), 1, "the same depth asks once");
    }

    #[test]
    fn a_route_change_reaches_the_watcher() {
        let sim = Sim::new();
        let clock = Manual(Arc::new(AtomicU64::new(0)));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut out = IosOutput::new(Box::new(sim.clone()), clock);
        let got = seen.clone();
        out.watch(Box::new(move |d| {
            got.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((d.kind, d.name))
        }));
        out.open(stereo(44_100)).unwrap();
        {
            let mut s = sim.lock();
            s.kind = OutputKind::Bluetooth;
            s.name = "Pods".into();
            s.latency_us = 180_000;
        }
        out.route_changed();
        let seen = seen.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(seen.len(), 2, "open, then the change: {seen:?}");
        assert_eq!(seen[1], (OutputKind::Bluetooth, "Pods".into()));
        assert_eq!(
            out.mixed_us(),
            180_000,
            "bluetooth latency is already on its way to the ear"
        );
    }

    #[test]
    fn a_failed_reopen_is_what_the_engine_reads() {
        let sim = Sim::new();
        let clock = Manual(Arc::new(AtomicU64::new(0)));
        let mut out = opened(&sim, &clock);
        out.resume();
        assert_eq!(sim.lock().starts, 1);
        sim.lock().fail_start = true;
        out.note_reset();
        let err = out.failed().expect("the engine asks after the wake");
        assert_eq!(err, "the output would not start");
        assert_eq!(sim.lock().opens.len(), 2, "one reopen");
        assert!(out.failed().is_none(), "told once");
    }

    #[test]
    fn render_does_not_allocate() {
        let heard = Heard::default();
        let mut out = [0.5f32; 256];
        let before = crate::counting::n();
        paint(
            std::ptr::null_mut(),
            &heard,
            &mut out,
            2,
            44_100,
            12_000,
            1_000_000,
        );
        assert_eq!(crate::counting::n(), before);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn the_process_callbacks_keep_an_interruption_and_a_pulled_jack() {
        let name = std::ffi::CString::new("Pods").unwrap();
        unsafe { nori_ios_route(PORT_BLUETOOTH, name.as_ptr(), 150_000, 1) };
        assert!(take_route_lost());
        assert!(!take_route_lost());
        nori_ios_interruption(1, 0);
        assert_eq!(take_interruption(), Some(Interrupt::Began));
        nori_ios_interruption(0, 1);
        assert_eq!(take_interruption(), Some(Interrupt::Ended { resume: true }));
        assert_eq!(take_interruption(), None);
        // The route callback also tells a watcher, when the device output has set one.
        let seen = Arc::new(Mutex::new(None));
        let got = seen.clone();
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                *WATCH.lock().unwrap_or_else(|p| p.into_inner()) = None;
            }
        }
        let _clear = Clear;
        *WATCH.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(Box::new(move |d| {
            *got.lock().unwrap_or_else(|p| p.into_inner()) = Some((d.kind, d.name));
        })));
        unsafe { nori_ios_route(PORT_WIRED, std::ptr::null(), 8_000, 0) };
        assert_eq!(
            seen.lock().unwrap_or_else(|p| p.into_inner()).clone(),
            Some((OutputKind::Wired, String::new()))
        );
    }
}
