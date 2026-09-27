//! The whole player for a platform that has none of its own (a desktop app, a terminal client): songs
//! loaded in bursts, demuxed, decoded, run through the sound chain and the transition engine, and
//! pulled by a sound card, all from the core's decisions. A client writes an [`AudioOutput`] (or uses
//! `nori-output-cpal`), says where songs are ([`Library`], with a [`ByteSource`] for HTTP), and drives
//! an [`Engine`]; the screen follows its [`Event`]s.
//!
//! No JNI, no uniffi, no platform code: what plays is `nori_player::pipeline`, the same code the
//! Android app's behaviour is tested against.

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

#[cfg(test)]
mod no_alloc;

pub use clock::{Clock, Monotonic};
pub use engine::{Config, Engine, Event, OutputFacts, Settings, State, Status, REMAKE_LEAD_MS};
pub use offload::{Coded, Coding, OffloadOutput, OnCpu, Support};
pub use library::{Library, Located, Source, Sources};
pub use nori_player::pipeline::{App, Queue, Sound};
pub use output::{AudioOutput, Device, DeviceWatch, Feed, OutputFormat, OutputKind, ShallowDepth};
pub use source::{Body, ByteSource, Cancel, Loader, OpenError, Window};
pub use store::{Order, Recent, Store};
pub use wav::WavOutput;

/// A panic's own words: its message, when it gave one.
pub(crate) fn panic_words(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "a panic with no message".into())
}

/// A playlist kept by the client and shared with the engine: edit it, then [`Engine::queue_changed`].
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
