//! The iPod's sound card: [`IosOutput`] over a [`Sink`].
//!
//! The real sink is AURemoteIO (`ios/Sound/NoriAudio.m`). Tests use a simulated one. The render
//! callback is [`nori_ios_render`]: the audio unit calls a C function, which has no Rust pointer of
//! ours, so the device's render state is a static. That path pulls and stamps; it does not lock or
//! allocate.

use std::ffi::{c_char, CStr};
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering,
};
use std::sync::{Arc, Mutex, OnceLock};
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

/// When the last music rendered will have been heard.
struct Heard {
    until_us: AtomicU64,
}

impl Heard {
    const fn new() -> Heard {
        Heard {
            until_us: AtomicU64::new(0),
        }
    }

    /// `frames` of music at `rate` leave the unit at `out_us` and reach the ear `delay_us` later.
    fn pulled(&self, out_us: u64, delay_us: u64, frames: usize, rate: u32) {
        let play = frames as u64 * 1_000_000 / rate.max(1) as u64;
        self.until_us.store(
            out_us.saturating_add(delay_us).saturating_add(play),
            Ordering::Relaxed,
        );
    }

    fn left_us(&self, now_us: u64) -> u64 {
        self.until_us.load(Ordering::Relaxed).saturating_sub(now_us)
    }
}

/// What the render callback reads: the playing feed, its format, the route and when the music rendered
/// will have been heard. The device's is [`device_render`], as the audio unit's C callback carries no
/// pointer of ours; a test's output has its own.
pub(crate) struct Render {
    /// Written while the unit is stopped.
    feed: AtomicPtr<Feed>,
    rate: AtomicU32,
    channels: AtomicU32,
    /// The route's own latency (the session's `outputLatency`): from leaving the unit to the ear.
    latency_us: AtomicU64,
    /// A `PORT_` code.
    port: AtomicI32,
    heard: Heard,
}

impl Render {
    const fn new() -> Render {
        Render {
            feed: AtomicPtr::new(std::ptr::null_mut()),
            rate: AtomicU32::new(44_100),
            channels: AtomicU32::new(2),
            latency_us: AtomicU64::new(0),
            port: AtomicI32::new(0),
            heard: Heard::new(),
        }
    }

    /// Fills `out` from the feed (silence when there is none) at `now_us`, for a buffer that leaves the
    /// unit `ahead_us` later (its render timestamp), and stamps when its music will have been heard. Only
    /// music counts: a running unit playing silence holds nothing, so a reopen waiting for it to drain goes
    /// on. Returns the frames of music.
    pub(crate) fn paint(&self, out: &mut [f32], now_us: u64, ahead_us: u64) -> usize {
        let feed = self.feed.load(Ordering::Acquire);
        let music = if feed.is_null() {
            out.fill(0.0);
            0
        } else {
            // SAFETY: published while the unit is stopped, and the unit is stopped before it is cleared.
            unsafe { (*feed).pull(out) }
        };
        if music > 0 {
            self.heard.pulled(
                now_us.saturating_add(ahead_us),
                self.latency_us.load(Ordering::Relaxed),
                music,
                self.rate.load(Ordering::Relaxed),
            );
        }
        music
    }

    fn channels(&self) -> usize {
        self.channels.load(Ordering::Relaxed).max(1) as usize
    }
}

fn device_render() -> &'static Arc<Render> {
    static DEVICE: OnceLock<Arc<Render>> = OnceLock::new();
    DEVICE.get_or_init(|| Arc::new(Render::new()))
}

/// A media-services reset the engine has not reopened for yet.
static RESET: AtomicBool = AtomicBool::new(false);
static WATCH: Mutex<Option<Arc<DeviceWatch>>> = Mutex::new(None);
static LOST: AtomicBool = AtomicBool::new(false);
static INTERRUPT: AtomicU8 = AtomicU8::new(0);

/// The audio unit's render callback. `ahead_us`: how long after this call the buffer's first frame
/// leaves the unit (its timestamp's host time less now).
///
/// # Safety
/// `out` is `frames` × the channel count set at open, interleaved float, for this call only.
#[no_mangle]
pub unsafe extern "C" fn nori_ios_render(frames: u32, out: *mut f32, ahead_us: u64) {
    if out.is_null() || frames == 0 {
        return;
    }
    let render = device_render();
    // SAFETY: the caller's promise above.
    let buf = unsafe { std::slice::from_raw_parts_mut(out, frames as usize * render.channels()) };
    render.paint(buf, HostClock.now_us(), ahead_us);
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
    let render = device_render();
    render.latency_us.store(latency_us, Ordering::Relaxed);
    render.port.store(port, Ordering::Relaxed);
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
    let feed = device_render().feed.load(Ordering::Acquire);
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

/// The engine's output. The device's ([`IosOutput::device`]) renders what [`nori_ios_render`] pulls and
/// hears the shim's notifications.
pub struct IosOutput<C: Clock = HostClock> {
    sink: Box<dyn Sink>,
    clock: C,
    pub(crate) render: Arc<Render>,
    /// The process's one card: its render state is [`device_render`]'s and the shim's notifications reach it.
    device: bool,
    format: Option<OutputFormat>,
    channels: usize,
    io_ms: u32,
    shallow: bool,
    opened: bool,
    playing: bool,
    watch: Option<Arc<DeviceWatch>>,
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
            render: Arc::new(Render::new()),
            device: false,
            format: None,
            channels: 2,
            io_ms: DEEP_IO_MS,
            shallow: false,
            opened: false,
            playing: false,
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

    fn note_route(&mut self, kind: OutputKind, name: String, latency_us: u64) {
        self.render.port.store(port_of(kind), Ordering::Relaxed);
        self.render.latency_us.store(latency_us, Ordering::Relaxed);
        if let Some(w) = &self.watch {
            w(Device { kind, name });
        }
    }

    fn clear_feed(&mut self) {
        self.render.feed.store(std::ptr::null_mut(), Ordering::Release);
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
        self.io_ms = g.io_ms;
        self.opened = true;
        self.format = Some(OutputFormat {
            rate: g.rate,
            channels: self.channels,
            bits: 0,
        });
        self.render.rate.store(g.rate, Ordering::Relaxed);
        self.render
            .channels
            .store(self.channels.max(1) as u32, Ordering::Relaxed);
        self.note_route(g.kind, g.name, g.latency_us);
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
        out.render = device_render().clone();
        out.device = true;
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
        if self.device {
            *WATCH.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }
}

impl<C: Clock> AudioOutput for IosOutput<C> {
    fn watch(&mut self, changed: DeviceWatch) {
        let w = Arc::new(changed);
        if self.device {
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
        let mut feed = Box::new(feed);
        self.render
            .feed
            .store(&mut *feed as *mut Feed, Ordering::Release);
        self.feed = Some(feed);
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
        self.render.heard.left_us(self.clock.now_us())
    }

    fn mixed_us(&self) -> u64 {
        if kind_of(self.render.port.load(Ordering::Relaxed)) == OutputKind::Bluetooth {
            self.render.latency_us.load(Ordering::Relaxed)
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
        let reset = self.reset || (self.device && RESET.swap(false, Ordering::AcqRel));
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

/// A simulated audio unit: what the session grants, the route, and the unit rendering whenever a test's
/// clock says a callback is due.
#[cfg(test)]
pub(crate) mod sim {
    use super::*;

    pub(crate) struct State {
        pub grant_rate: Option<u32>,
        pub latency_us: u64,
        pub kind: OutputKind,
        pub name: String,
        pub opens: Vec<(u32, usize, u32)>,
        pub io_sets: Vec<u32>,
        pub starts: u32,
        pub fail_start: bool,
        running: bool,
        io_ms: u32,
        /// Each buffer of music rendered: when it left the unit, and its frames.
        rendered: Vec<(u64, u64)>,
    }

    #[derive(Clone)]
    pub(crate) struct Sim(Arc<Mutex<State>>);

    impl Sim {
        pub(crate) fn new() -> Sim {
            Sim(Arc::new(Mutex::new(State {
                grant_rate: None,
                latency_us: 12_000,
                kind: OutputKind::Wired,
                name: "Headphones".into(),
                opens: Vec::new(),
                io_sets: Vec::new(),
                starts: 0,
                fail_start: false,
                running: false,
                io_ms: DEEP_IO_MS,
                rendered: Vec::new(),
            })))
        }

        pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, State> {
            self.0.lock().unwrap_or_else(|p| p.into_inner())
        }

        /// The I/O buffer, frames at `rate`, and how long it plays.
        fn buffer(&self, rate: u32) -> (usize, u64) {
            let frames = (self.lock().io_ms as u64 * rate as u64 / 1000).max(1);
            (frames as usize, frames * 1_000_000 / rate.max(1) as u64)
        }

        /// How often the unit calls back, µs.
        pub(crate) fn period_us(&self, render: &Render) -> u64 {
            self.buffer(render.rate.load(Ordering::Relaxed)).1
        }

        /// One callback at `now_us`, if the unit runs: an I/O buffer rendered from `render`, leaving the unit
        /// one buffer later, as RemoteIO's do. True when the pull woke the engine. The unit's lock is held
        /// throughout, so a stop waits for a callback under way, as `AudioOutputUnitStop` does.
        pub(crate) fn callback(&self, render: &Render, now_us: u64) -> bool {
            let rate = render.rate.load(Ordering::Relaxed);
            let (frames, play_us) = self.buffer(rate);
            let mut s = self.lock();
            if !s.running {
                return false;
            }
            let feed = render.feed.load(Ordering::Acquire);
            // SAFETY: the feed changes only while the unit is stopped, which waits for this lock.
            let waits = || !feed.is_null() && unsafe { (*feed).engine_waits() };
            let waited = waits();
            let mut out = vec![0.0f32; frames * render.channels()];
            let music = render.paint(&mut out, now_us, play_us);
            if music > 0 {
                s.rendered.push((now_us + play_us, music as u64));
            }
            waited && !waits()
        }

        /// Frames of music the listener has heard by `now_us`.
        pub(crate) fn heard(&self, render: &Render, now_us: u64) -> u64 {
            let rate = render.rate.load(Ordering::Relaxed) as u64;
            let s = self.lock();
            s.rendered
                .iter()
                .map(|&(out_us, frames)| {
                    let at = out_us + s.latency_us;
                    (now_us.saturating_sub(at) * rate / 1_000_000).min(frames)
                })
                .sum()
        }
    }

    impl Sink for Sim {
        fn open(&mut self, rate: u32, channels: usize, io_ms: u32) -> Result<Grant, String> {
            let mut s = self.lock();
            s.opens.push((rate, channels, io_ms));
            s.io_ms = io_ms;
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
            s.running = true;
            Ok(())
        }

        fn stop(&mut self) {
            self.lock().running = false;
        }

        fn set_io_ms(&mut self, io_ms: u32) -> Result<u32, String> {
            let mut s = self.lock();
            s.io_sets.push(io_ms);
            s.io_ms = io_ms;
            Ok(io_ms)
        }

        fn route(&mut self) -> (OutputKind, String, u64) {
            let s = self.lock();
            (s.kind, s.name.clone(), s.latency_us)
        }

        fn close(&mut self) {}
    }

    /// 16-bit stereo PCM at 44.1 kHz, a 440 Hz tone, `seconds` long.
    pub(crate) fn wav(seconds: u32) -> Vec<u8> {
        let rate = 44_100u32;
        let frames = rate * seconds;
        let data = frames * 4;
        let mut w = Vec::with_capacity(44 + data as usize);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&2u16.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * 4).to_le_bytes());
        w.extend_from_slice(&4u16.to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data.to_le_bytes());
        for i in 0..frames {
            let s =
                ((i as f64 * 440.0 * std::f64::consts::TAU / rate as f64).sin() * 8000.0) as i16;
            w.extend_from_slice(&s.to_le_bytes());
            w.extend_from_slice(&s.to_le_bytes());
        }
        w
    }
}

#[cfg(test)]
mod tests {
    use super::sim::Sim;
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use nori_engine::AudioOutput;

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
    fn latency_is_the_route_plus_the_music_pulled() {
        let heard = Heard::new();
        // Music that leaves the unit at 1.093 s: 128 frames at 44.1 kHz is 2902 µs, heard 12 ms after.
        heard.pulled(1_093_000, 12_000, 128, 44_100);
        assert_eq!(heard.left_us(1_000_000), 93_000 + 12_000 + 128 * 1_000_000 / 44_100);
        assert_eq!(heard.left_us(1_093_000 + 12_000 + 2_902), 0, "heard");
    }

    #[test]
    fn a_running_unit_playing_silence_holds_nothing() {
        // Regression: every callback stamped its period, so a running unit never drained and a reopen
        // for another rate waited for ever (the first tap on a song of another rate was silent).
        let sim = Sim::new();
        let clock = Manual(Arc::new(AtomicU64::new(1_000_000)));
        let out = opened(&sim, &clock);
        let mut buf = [1.0f32; 256];
        for period in 0..3 {
            clock.0.store(1_000_000 + period * 2_902, Ordering::Relaxed);
            out.render.paint(&mut buf, clock.now_us(), 0);
            assert!(buf.iter().all(|s| *s == 0.0), "nothing is playing: silence");
            assert!(!out.holding(), "period {period}");
        }
        assert_eq!(out.latency_us(), 0);
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
        let render = Render::new();
        let mut out = [0.5f32; 256];
        let before = crate::counting::n();
        render.paint(&mut out, 1_000_000, 93_000);
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

    /// Songs as WAV files in memory.
    struct Wavs(Vec<(String, Arc<Vec<u8>>)>);

    impl nori_engine::ByteSource for Wavs {
        fn open(&self, url: &str, from: u64) -> Result<nori_engine::Body, nori_engine::OpenError> {
            let f = self.0.iter().find(|(id, _)| id == url).ok_or("no such song")?.1.clone();
            Ok(nori_engine::Body {
                start: from,
                len: Some(f.len() as u64),
                reader: Box::new(std::io::Cursor::new(f[from as usize..].to_vec())),
            })
        }
    }

    struct Songs(Arc<Wavs>);

    impl nori_engine::Library for Songs {
        fn locate(&mut self, id: &str) -> Result<nori_engine::Located, String> {
            Ok(nori_engine::Located {
                source: nori_engine::Source::Url { url: id.to_string(), bytes: self.0.clone() },
                hint: Some("wav".into()),
                duration_ms: Some(60_000),
                estimated: false,
            })
        }

        fn about(&self, id: &str) -> nori_player::transitions::WindowSong {
            nori_player::transitions::WindowSong { id: id.to_string(), duration_ms: 60_000, ..Default::default() }
        }
    }

    /// The test's clock as the output reads it.
    struct Ticks(nori_engine::testing::Virtual);

    impl Clock for Ticks {
        fn now_us(&self) -> u64 {
            (self.0.now_ns() / 1_000) as u64
        }
    }

    /// The audio unit calling back on the test's clock.
    struct Unit {
        sim: Sim,
        render: Arc<Render>,
        next_ns: i64,
    }

    impl nori_engine::testing::Device for Unit {
        fn due_ns(&self) -> i64 {
            self.next_ns
        }

        fn tick(&mut self, now_ns: i64) -> bool {
            self.next_ns = now_ns + self.sim.period_us(&self.render) as i64 * 1_000;
            self.sim.callback(&self.render, (now_ns / 1_000) as u64)
        }
    }

    /// Behind the deep I/O buffer (each buffer leaves the unit 93 ms after its callback, heard 12 ms after
    /// that), the engine's place is the one the listener hears, which devices mirroring the iPod show.
    #[test]
    fn a_deep_output_says_the_place_heard() {
        let clock = nori_engine::testing::Virtual::default();
        let sim = Sim::new();
        let out = IosOutput::new(Box::new(sim.clone()), Ticks(clock.clone()));
        let render = out.render.clone();
        let wavs = Arc::new(Wavs(vec![("a".into(), Arc::new(sim::wav(60)))]));
        let queue = nori_engine::SharedQueue::default();
        queue.0.lock().set(vec!["a".into()], Some(0), false, 0);
        let mut app = nori_player::sim::App::new();
        app.prefs = nori_player::sim::prefs_off();
        let engine = nori_engine::Engine::start_on(Songs(wavs), app, queue, Box::new(out), None, nori_engine::Config::default(), clock.clone(), |_| {});
        let unit = Unit { sim: sim.clone(), render: render.clone(), next_ns: 0 };
        let time = nori_engine::testing::Stepper::new(clock.clone(), Arc::new(parking_lot::Mutex::new(unit)));
        let heard_ms = || sim.heard(&render, (clock.now_ns() / 1_000) as u64) as i64 * 1_000 / 44_100;
        engine.queue_changed();
        engine.play_at(0, 0);
        assert!(time.until(Duration::from_secs(20), || heard_ms() > 10_000), "it plays");
        // Read at moments through a buffer, not only as one starts.
        for _ in 0..5 {
            time.run(Duration::from_millis(37));
            engine.look();
            clock.settle();
            let off = engine.status().position_ms - heard_ms();
            assert!(off.abs() <= 5, "the engine says {off} ms off what is heard");
        }
        engine.stop();
    }
}
