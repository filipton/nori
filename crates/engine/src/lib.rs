//! The player for platforms without one (desktop, terminal): songs loaded in bursts, demuxed, decoded,
//! run through the sound chain and transition engine, and pulled by a sound card. A client provides an
//! [`AudioOutput`] (or `nori-output-cpal`) and a [`Library`] (with a [`ByteSource`] for HTTP), drives
//! an [`Engine`] and follows its [`Event`]s. Playback is `nori_player::pipeline`, shared with Android.

pub mod ahead;
pub mod arriving;
pub mod clock;
pub mod demux;
mod engine;
pub mod library;
mod mpeg;
mod mp4;
pub mod offload;
pub mod output;
pub mod pieces;
pub mod source;
pub mod store;
pub mod wav;
pub mod watch;

#[cfg(feature = "core")]
pub mod core;
#[cfg(feature = "core")]
pub mod processing;
#[cfg(feature = "core")]
pub mod sing;

#[cfg(test)]
mod no_alloc;
#[cfg(feature = "testing")]
pub mod testing;

pub use clock::{Clock, Monotonic};
pub use engine::{Config, Engine, Event, OutputFacts, Settings, State, Status, REMAKE_LEAD_MS};
pub use offload::{Coded, Coding, OffloadOutput, OnCpu, Support};
pub use library::{Library, Located, Source, Sources};
pub use nori_player::pipeline::{App, Queue, Sound};
pub use output::{AudioOutput, Device, DeviceWatch, Feed, OutputFormat, OutputKind};
pub use source::{Body, ByteSource, Cancel, Fetching, Held, Loader, OpenError, Waits, Window};
pub use store::{CacheOrder, Store};
pub use wav::WavOutput;

/// A panic's message.
pub(crate) fn panic_words(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "a panic with no message".into())
}

/// A client-owned playlist shared with the engine: edit it, then call [`Engine::queue_changed`].
#[derive(Clone, Default)]
pub struct SharedQueue(pub std::sync::Arc<parking_lot::Mutex<nori_player::playlist::Playlist>>);

impl Queue for SharedQueue {
    fn read<R>(&self, f: impl FnOnce(&nori_player::playlist::Playlist) -> R) -> R {
        f(&self.0.lock())
    }

    fn moved_to(&mut self, index: usize) {
        self.0.lock().moved_to(index);
    }

    fn set_repeat(&mut self, mode: u8) {
        self.0.lock().set_repeat(mode);
    }
}
