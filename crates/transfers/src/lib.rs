//! On-device storage bookkeeping: downloads (transfers.rs) and the stream cache's eviction order
//! (stream_cache.rs). The platform moves the bytes and words the messages.

pub mod stream_cache;
pub mod transfers;
