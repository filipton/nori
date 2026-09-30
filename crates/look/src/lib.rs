//! Platform-independent visuals: colours derived from a cover, seeded theme tones, and lyrics timing
//! (lyrics.rs). Colours are `u32` ARGB throughout.

pub mod color;
pub mod compose;
pub mod cover;
pub mod dress;
pub mod lyrics;
pub mod motion;
#[cfg(test)]
mod no_alloc;
pub mod palette;
pub mod sleeve;
mod random;
pub mod theme;
