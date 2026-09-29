//! Lyrics: the server's and the lyrics services' answers read into one shape (lyrics.rs, formats.rs,
//! json.rs, html.rs), the services (services.rs, lrclib.rs), asked together and ranked (race.rs,
//! trust.rs, fit.rs, credits.rs, sync.rs), and the lyrics page's clock handles (look.rs).

pub mod credits;
pub mod fit;
pub mod formats;
pub mod html;
pub mod json;
pub mod look;
pub mod lrclib;
pub mod lyrics;
pub mod race;
pub mod services;
pub mod sync;
pub mod trust;
