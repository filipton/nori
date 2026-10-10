//! `nori-engine`'s desktop output over cpal (PipeWire/ALSA, CoreAudio, WASAPI).
//!
//! The device is asked for the stream's rate and channels first so nothing is resampled; otherwise it
//! plays at its own and the engine converts. Paused, the stream is stopped so the device can sleep.
//! The engine is told the current device on open and when the system moves the default stream
//! (not reported on ALSA).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, DeviceType, ErrorKind, InterfaceType, SampleFormat, SizedSample, Stream, StreamConfig};
use nori_engine::{AudioOutput, DeviceWatch, Feed, OutputFormat, OutputKind};

#[derive(Default)]
pub struct CpalOutput {
    /// Device name; None is the system default.
    wanted: Option<String>,
    device: Option<Device>,
    config: Option<(StreamConfig, SampleFormat)>,
    /// Period range the device supports, frames.
    periods: Option<(u32, u32)>,
    /// Kept so the stream can be rebuilt with another period. Only the playing stream's callback locks it.
    feed: Option<Arc<Mutex<Feed>>>,
    stream: Option<Stream>,
    playing: bool,
    /// Equalizer tuning: the period is [`SHALLOW_PERIOD_MS`].
    shallow: bool,
    /// When what the device took has been heard.
    heard: Arc<Heard>,
    /// Why the stream stopped for good (its device went away), for [`AudioOutput::failed`].
    failed: Arc<Mutex<Option<String>>>,
    watch: Option<Arc<DeviceWatch>>,
    volume: Volume,
}

/// When the last frame the device pulled will have been heard: the device's own delay from its last
/// callback, plus that callback's frames, plus what the ring gave since. Written by the callback, read by
/// the engine. What was pulled is the ring's own count: the engine reads the place heard as what the ring
/// gave less what the device holds, and a pull wakes it before the callback has said when that is heard.
struct Heard {
    base: Instant,
    last: Mutex<Last>,
    /// The ring's count of frames pulled.
    ring: Mutex<Option<Pulled>>,
}

type Pulled = Box<dyn Fn() -> u64 + Send + Sync>;

/// The last callback's music: heard until `until_us` after `base`, the ring's count `taken` after it.
#[derive(Default, Clone, Copy)]
struct Last {
    until_us: u64,
    taken: u64,
    rate: u32,
}

impl Default for Heard {
    fn default() -> Self {
        Heard { base: Instant::now(), last: Mutex::default(), ring: Mutex::default() }
    }
}

impl Heard {
    fn follow(&self, ring: Pulled) {
        *self.ring.lock().unwrap_or_else(|p| p.into_inner()) = Some(ring);
    }

    fn taken(&self) -> u64 {
        self.ring.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map_or(0, |r| r())
    }

    /// A callback at `now` took `frames` at `rate`, heard `delay_us` after it starts.
    fn pulled(&self, now: Instant, delay_us: u64, frames: usize, rate: u32) {
        let at = now.saturating_duration_since(self.base).as_micros() as u64;
        let taken = self.taken();
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Last { until_us: at + delay_us + frames as u64 * 1_000_000 / rate.max(1) as u64, taken, rate };
    }

    /// How long until what was pulled has been heard.
    fn left_us(&self, now: Instant) -> u64 {
        // The callback's word first: a pull after it counts as held.
        let last = *self.last.lock().unwrap_or_else(|p| p.into_inner());
        let since = self.taken().saturating_sub(last.taken) * 1_000_000 / last.rate.max(1) as u64;
        last.until_us.saturating_sub(now.saturating_duration_since(self.base).as_micros() as u64) + since
    }
}

/// Listener volume, 0 to 1, applied after the engine's chain so ReplayGain, limiter and fades see full
/// scale. Settable from any thread.
#[derive(Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    pub fn set(&self, v: f32) {
        self.0.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

impl Default for Volume {
    fn default() -> Self {
        Volume(Arc::new(AtomicU32::new(1.0f32.to_bits())))
    }
}

fn describe(device: &Device) -> Option<nori_engine::Device> {
    let d = device.description().ok()?;
    let kind = match (d.interface_type(), d.device_type()) {
        (InterfaceType::Usb, _) => OutputKind::Usb,
        (InterfaceType::Bluetooth, _) => OutputKind::Bluetooth,
        (InterfaceType::Hdmi | InterfaceType::DisplayPort | InterfaceType::Line | InterfaceType::Spdif, _) => OutputKind::Line,
        (_, DeviceType::Headphones | DeviceType::Headset) => OutputKind::Wired,
        (_, DeviceType::Speaker) | (InterfaceType::BuiltIn, _) => OutputKind::Speaker,
        (_, DeviceType::Dock) => OutputKind::Line,
        _ => OutputKind::Other,
    };
    Some(nori_engine::Device { kind, name: d.name().to_string() })
}

impl CpalOutput {
    /// The system's default output device.
    pub fn new() -> CpalOutput {
        CpalOutput::default()
    }

    /// The output device called `name`, as [`CpalOutput::devices`] lists it.
    pub fn with_device(name: &str) -> CpalOutput {
        CpalOutput { wanted: Some(name.to_string()), ..CpalOutput::default() }
    }

    pub fn volume(&self) -> Volume {
        self.volume.clone()
    }

    pub fn devices() -> Vec<String> {
        let host = cpal::default_host();
        host.output_devices().map(|ds| ds.filter_map(|d| d.description().ok().map(|n| n.to_string())).collect()).unwrap_or_default()
    }

    fn pick(&self) -> Result<Device, String> {
        let host = cpal::default_host();
        match &self.wanted {
            None => host.default_output_device().ok_or_else(|| "no output device".to_string()),
            Some(name) => host
                .output_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.description().is_ok_and(|n| n.to_string() == *name))
                .ok_or_else(|| format!("no output device called {name}")),
        }
    }
}

/// Preference among supported sample formats; None is unsupported.
fn rank(f: SampleFormat) -> Option<u8> {
    match f {
        SampleFormat::F32 => Some(0),
        SampleFormat::I16 => Some(1),
        _ => None,
    }
}

/// Default period. The sound server's own default is a few ms, which wakes the callback hundreds of
/// times a second for music buffered seconds ahead.
const PERIOD_MS: u32 = 100;
/// Period while the equalizer is tuned: a band moved is heard this much sooner.
const SHALLOW_PERIOD_MS: u32 = 10;

fn buffer_size(rate: u32, ms: u32, periods: Option<(u32, u32)>) -> BufferSize {
    match periods {
        Some((min, max)) => BufferSize::Fixed((rate * ms / 1000).clamp(min, max)),
        None => BufferSize::Default,
    }
}

/// Builds a paused stream that pulls `feed`, scales by `volume` and records when it is heard. A contended
/// feed lock (only possible while a replaced stream is still stopping) plays silence.
#[allow(clippy::too_many_arguments)]
fn build_stream<T: SizedSample + Default + Send + 'static>(
    device: &Device,
    config: StreamConfig,
    feed: Arc<Mutex<Feed>>,
    volume: Volume,
    heard: Arc<Heard>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
    pull: fn(&mut Feed, &mut [T]) -> usize,
    scale: fn(T, f32) -> T,
) -> Result<Stream, String> {
    let rate = config.sample_rate;
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let music = match feed.try_lock() {
                    Ok(mut f) => pull(&mut f, data),
                    Err(_) => {
                        data.fill(T::default());
                        0
                    }
                };
                let v = volume.get();
                if v != 1.0 {
                    data.iter_mut().for_each(|s| *s = scale(*s, v));
                }
                let t = info.timestamp();
                let delay = t.playback.saturating_duration_since(t.callback).as_micros() as u64;
                // Only music counts: playing silence holds nothing, so a reopen waiting to drain goes on.
                if music > 0 {
                    heard.pulled(Instant::now(), delay, music, rate);
                }
            },
            on_error,
            None,
        )
        .map_err(|e| e.to_string())?;
    let _ = stream.pause();
    Ok(stream)
}

/// A stream error other than a device change: one whose device is gone stops the stream for good, so the
/// engine is told ([`AudioOutput::failed`]) and woken to hear it; others are only logged.
fn stream_failed(kind: ErrorKind, detail: String, failed: &Mutex<Option<String>>, wake: impl FnOnce()) {
    eprintln!("nori: the output stream failed: {detail}");
    if matches!(kind, ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable | ErrorKind::StreamInvalidated) {
        *failed.lock().unwrap_or_else(|p| p.into_inner()) = Some(detail);
        wake();
    }
}

impl CpalOutput {
    fn period_ms(&self) -> u32 {
        if self.shallow {
            SHALLOW_PERIOD_MS
        } else {
            PERIOD_MS
        }
    }

    /// Drops the current stream, then builds a new one from the current config and feed, playing if
    /// the engine wants it playing.
    fn rebuild(&mut self) -> Result<(), String> {
        self.stream = None;
        let (Some(device), Some((config, format)), Some(feed)) = (&self.device, &self.config, &self.feed) else { return Err("not open".into()) };
        // Only follows the default device; a named device stays put.
        let watch = self.watch.clone().filter(|_| self.wanted.is_none());
        let (failed, stopped) = (self.failed.clone(), feed.clone());
        let on_error = move |e: cpal::Error| match e.kind() {
            ErrorKind::DeviceChanged => {
                if let (Some(w), Some(d)) = (&watch, cpal::default_host().default_output_device().as_ref().and_then(describe)) {
                    w(d);
                }
            }
            kind => stream_failed(kind, e.to_string(), &failed, || stopped.lock().unwrap_or_else(|p| p.into_inner()).wake_engine()),
        };
        let (feed, volume, heard) = (feed.clone(), self.volume.clone(), self.heard.clone());
        let stream = match format {
            SampleFormat::F32 => build_stream(device, *config, feed, volume, heard, on_error, Feed::pull, |s, v| s * v),
            _ => build_stream(device, *config, feed, volume, heard, on_error, Feed::pull_i16, |s, v| (s as f32 * v) as i16),
        }?;
        if self.playing {
            stream.play().map_err(|e| e.to_string())?;
        }
        self.stream = Some(stream);
        Ok(())
    }
}

impl AudioOutput for CpalOutput {
    fn open(&mut self, want: OutputFormat) -> Result<OutputFormat, String> {
        let device = self.pick()?;
        let exact = device
            .supported_output_configs()
            .map_err(|e| e.to_string())?
            .filter(|r| r.channels() as usize == want.channels && rank(r.sample_format()).is_some())
            .filter_map(|r| r.try_with_sample_rate(want.rate))
            .min_by_key(|c| rank(c.sample_format()));
        let chosen = match exact {
            Some(c) => c,
            None => device.default_output_config().map_err(|e| e.to_string())?,
        };
        let format = chosen.sample_format();
        if rank(format).is_none() {
            return Err(format!("the device only takes {format:?} samples"));
        }
        self.periods = match *chosen.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } if max >= min => Some((min, max)),
            _ => None,
        };
        let buffer_size = buffer_size(chosen.sample_rate(), self.period_ms(), self.periods);
        let config = StreamConfig { channels: chosen.channels(), sample_rate: chosen.sample_rate(), buffer_size };
        let got = OutputFormat { rate: config.sample_rate, channels: config.channels as usize, bits: 0 };
        if let (Some(w), Some(d)) = (&self.watch, describe(&device)) {
            w(d);
        }
        self.device = Some(device);
        self.config = Some((config, format));
        Ok(got)
    }

    fn start(&mut self, feed: Feed) -> Result<(), String> {
        self.heard.follow(Box::new(feed.pulled()));
        self.feed = Some(Arc::new(Mutex::new(feed)));
        self.rebuild()
    }

    fn pause(&mut self) {
        self.playing = false;
        if let Some(s) = &self.stream {
            let _ = s.pause();
        }
    }

    fn resume(&mut self) {
        self.playing = true;
        if let Some(s) = &self.stream {
            if let Err(e) = s.play() {
                *self.failed.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
                if let Some(feed) = &self.feed {
                    feed.lock().unwrap_or_else(|p| p.into_inner()).wake_engine();
                }
            }
        }
    }

    fn watch(&mut self, changed: DeviceWatch) {
        self.watch = Some(Arc::new(changed));
    }

    fn latency_us(&self) -> u64 {
        self.heard.left_us(Instant::now())
    }

    fn holding(&self) -> bool {
        self.latency_us() > 0
    }

    fn failed(&mut self) -> Option<String> {
        self.failed.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    fn takes_float(&mut self) -> bool {
        let Ok(device) = self.pick() else { return false };
        device.supported_output_configs().is_ok_and(|mut c| c.any(|r| r.sample_format() == SampleFormat::F32))
    }

    /// Rebuilds the stream with the shorter period, so a sound change is heard sooner.
    fn shallow(&mut self, on: bool) {
        if on == self.shallow {
            return;
        }
        self.shallow = on;
        let period = self.period_ms();
        let Some((config, _)) = &mut self.config else { return };
        config.buffer_size = buffer_size(config.sample_rate, period, self.periods);
        if self.stream.is_none() {
            return;
        }
        if let Err(e) = self.rebuild() {
            *self.failed.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
            if let Some(feed) = &self.feed {
                feed.lock().unwrap_or_else(|p| p.into_inner()).wake_engine();
            }
        }
    }

    fn close(&mut self) {
        self.stream = None;
        self.feed = None;
        self.device = None;
        self.playing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    #[test]
    fn pulled_period_counts_till_heard() {
        let heard = Heard::default();
        let t = heard.base + Duration::from_secs(1);
        heard.pulled(t, 20_000, 4_410, 44_100);
        assert_eq!(heard.left_us(t), 120_000, "the device's delay and the period it took");
        assert_eq!(heard.left_us(t + Duration::from_millis(70)), 50_000);
        assert_eq!(heard.left_us(t + Duration::from_millis(130)), 0, "all heard");
    }

    /// The engine, woken by a pull, reads before the callback says when that pull is heard: the frames
    /// the ring gave since count as held.
    #[test]
    fn a_pull_not_yet_said_counts_as_held() {
        let heard = Heard::default();
        let taken = Arc::new(AtomicU64::new(0));
        let ring = taken.clone();
        heard.follow(Box::new(move || ring.load(Ordering::Relaxed)));
        let t = heard.base + Duration::from_secs(1);
        taken.store(4_410, Ordering::Relaxed);
        heard.pulled(t, 20_000, 4_410, 44_100);
        let next = t + Duration::from_millis(100);
        taken.store(8_820, Ordering::Relaxed);
        assert_eq!(heard.left_us(next), 120_000, "the delay left and the period pulled");
        heard.pulled(next, 20_000, 4_410, 44_100);
        assert_eq!(heard.left_us(next), 120_000, "as the callback then says");
    }

    #[test]
    fn a_device_gone_stops_the_engine() {
        let failed = Mutex::new(None);
        for (kind, stops) in [(ErrorKind::DeviceNotAvailable, true), (ErrorKind::HostUnavailable, true), (ErrorKind::DeviceBusy, false), (ErrorKind::StreamInvalidated, true)] {
            let mut woke = false;
            stream_failed(kind, "gone".into(), &failed, || woke = true);
            assert_eq!((failed.lock().unwrap().take().is_some(), woke), (stops, stops), "{kind:?}");
        }
    }
}
