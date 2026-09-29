//! Cover art: fetched through the core's [`Transport`](nori_core::transport::Transport), cached on disk
//! ([`DiskCache`]), decoded to the drawn size ([`Decoder`]) and cached in memory ([`MemoryCache`]).
//! [`Loader`] serves shared, cancellable requests on worker threads; [`Paint`] picks the output (RGBA
//! rows, or Android Bitmaps in crates/android covers.rs).

pub mod decode;
pub mod disk;
pub mod loader;
pub mod memory;
pub mod scale;

pub use decode::{format, header, Decoder, Error as DecodeError, Format, Header};
pub use disk::{DiskCache, Key};
pub use loader::{Config, Error, Loader, Paint, Rgba, Ticket};
pub use memory::{Image, MemoryCache, Sized};
pub use nori_core::covers::{cover_url_into, is_provider_cover};
pub use scale::{Alpha, Target};
