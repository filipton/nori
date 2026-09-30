//! How Nori plays music, platform-free: decoding, the sound chain, AutoMix, transitions, the queue and
//! the player pipeline. Platforms hand in packets and settings and get PCM and decisions back.

pub mod automix;
pub mod burst;
pub mod chain;
pub mod compressor;
pub mod contour;
pub mod dac;
pub mod device;
pub mod decode;
pub mod dither;
pub mod dsp;
pub mod engine;
pub mod eqfit;
pub mod gain;
pub mod graphic;
pub mod outputs;
pub mod heard;
pub mod pcm;
pub mod seek;
pub mod pipeline;
pub mod playlist;
pub mod policy;
pub mod queue;
pub mod silence;
pub mod sink;
#[cfg(any(test, feature = "synth"))]
pub mod sim;
pub mod sound;
pub mod sonic;
pub mod spatial;
pub mod speed;
pub mod transitions;
pub mod transport;
pub mod types;

/// Allocation checks for the audio path.
#[cfg(test)]
mod no_alloc;
