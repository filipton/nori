//! What the engine tests share: the virtual clock (`nori_engine::testing`), songs, servers and waits.

#![allow(dead_code, unused_imports)]

pub mod card;
pub mod reference;

use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

pub use nori_engine::testing::{Device, Stepper, Virtual};

/// A server's answer held until the test opens the gate.
#[derive(Default)]
pub struct Gate(Mutex<bool>, Condvar);

impl Gate {
    pub fn open(&self) {
        *self.0.lock() = true;
        self.1.notify_all();
    }

    pub fn wait(&self) {
        let mut open = self.0.lock();
        while !*open {
            self.1.wait(&mut open);
        }
    }
}

/// A 16-bit stereo WAV file of interleaved `samples`.
pub fn wav(rate: u32, samples: &[i16]) -> Vec<u8> {
    let samples: Vec<i32> = samples.iter().map(|&v| v as i32).collect();
    wav_bits(rate, 16, &samples)
}

/// A stereo PCM WAV file with `bits` (16 or 24) per sample.
pub fn wav_bits(rate: u32, bits: u16, samples: &[i32]) -> Vec<u8> {
    let width = bits as u32 / 8;
    let data = samples.len() as u32 * width;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2 * width).to_le_bytes());
    w.extend_from_slice(&(2 * width as u16).to_le_bytes());
    w.extend_from_slice(&bits.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for v in samples {
        w.extend_from_slice(&v.to_le_bytes()[..width as usize]);
    }
    w
}

/// Interleaved stereo 16-bit samples of a sine at `hz`, `secs` long, peaking at `peak`.
pub fn sine(rate: u32, hz: f64, secs: f64, peak: f64) -> Vec<i16> {
    let frames = (rate as f64 * secs) as usize;
    (0..frames).flat_map(|i| {
        let v = ((std::f64::consts::TAU * hz * i as f64 / rate as f64).sin() * peak) as i16;
        [v, v]
    }).collect()
}

/// Forty seconds of a 120 bpm click over a quiet tone at `hz`, stereo 16-bit at 44.1 kHz.
pub fn beat(hz: f64) -> Vec<i16> {
    let rate = 44_100usize;
    (0..rate * 40).flat_map(|i| {
        let in_beat = i % (rate / 2);
        let click = if in_beat < 2000 { (1.0 - in_beat as f64 / 2000.0) * 0.8 } else { 0.0 };
        let tone = (i as f64 * hz * std::f64::consts::TAU / rate as f64).sin() * 0.1;
        let v = (((click * ((i * 7919) % 97) as f64 / 97.0) + tone) * 32767.0) as i16;
        [v, v]
    }).collect()
}

/// Whether ffmpeg is installed (tests that need encoded songs pass without it).
pub fn ffmpeg() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A core transport that answers every API call with a 500: resolving songs needs none.
#[cfg(feature = "core")]
#[derive(Default)]
pub struct NoApi {
    pub metered: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "core")]
#[async_trait::async_trait]
impl nori_core::transport::Transport for NoApi {
    async fn get(&self, _url: String, _timeout_ms: u32) -> Result<nori_core::transport::TransportResponse, nori_core::transport::TransportError> {
        Ok(nori_core::transport::TransportResponse { status: 500, body: Vec::new() })
    }

    async fn send(&self, _request: nori_core::transport::Exchange) -> Result<nori_core::transport::TransportResponse, nori_core::transport::TransportError> {
        Ok(nori_core::transport::TransportResponse { status: 500, body: Vec::new() })
    }

    fn address_changed(&self) {}

    fn network(&self) -> nori_core::transport::Network {
        if self.metered.load(std::sync::atomic::Ordering::Relaxed) { nori_core::transport::Network::Metered } else { nori_core::transport::Network::Unmetered }
    }
}

/// A count of events a test waits on.
#[derive(Default)]
pub struct Signal(Mutex<u64>, Condvar);

impl Signal {
    pub fn bump(&self) {
        *self.0.lock() += 1;
        self.1.notify_all();
    }

    /// Waits until `n` events came.
    pub fn reach(&self, n: u64) {
        self.until(|seen| seen >= n);
    }

    /// Waits until `done` holds, looked at again after each event; `done` gets the count.
    pub fn until(&self, mut done: impl FnMut(u64) -> bool) {
        let mut c = self.0.lock();
        while !done(*c) {
            self.1.wait(&mut c);
        }
    }
}

/// Blocks until `store` fetches nothing ahead and `analyses` and `measurer` measure nothing: on a device
/// that work finishes long before the next song, but a test's clock would overtake it.
#[cfg(feature = "core")]
pub fn settle(store: &nori_engine::Store, analyses: &nori_engine::core::Analyses, measurer: &nori_engine::core::Measurer) {
    while store.fetching_ahead() || analyses.measuring_as_they_come() || measurer.busy() {
        store.wait_ahead();
        analyses.wait_arrivals();
        measurer.wait();
    }
}

/// A core of the test's own in `dir`: its settings opened (and changed by `prefs`), its queue, and a
/// client for it that answers no API call.
#[cfg(feature = "core")]
pub fn own_core(dir: &std::path::Path, prefs: impl FnOnce(&mut nori_core::settings::StoredPrefs)) -> (Arc<nori_core::Core>, Arc<nori_core::client::Client>) {
    let settings = Arc::new(nori_core::settings_store::Settings::default());
    let mut p = settings.open(&dir.join("app.db").to_string_lossy()).unwrap();
    prefs(&mut p);
    settings.put(p);
    let session = Arc::new(nori_core::queue::Session::new(settings));
    let core = nori_core::Core::open(dir.join("nori.db").to_string_lossy().into_owned(), "test".into(), session).unwrap();
    core.configure(nori_core::ServerConfig { url: "http://music.test".into(), user: "u".into(), password: "p".into(), api_key: None, legacy_auth: false }).unwrap();
    let client = nori_core::client::Client::new(core.clone(), Arc::new(NoApi::default()));
    client.set_profile(nori_core::client::NetProfile { url: "http://music.test".into(), ..Default::default() });
    (core, client)
}

