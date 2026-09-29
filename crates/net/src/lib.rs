//! The Subsonic API below the client: request signing (api.rs), the platform's HTTP transport and its
//! failures (transport.rs), profiles and writes (requests.rs), and stream cache keys (stream.rs).

pub mod api;
pub mod requests;
pub mod stream;
pub mod transport;
