//! How Nori plays music, independent of any platform: the sound chain (equalizer, crossfeed,
//! limiter), AutoMix (analysis, planning, per-sample mixing, time-stretch, resampling) and the
//! values they share. Nothing here decodes, fetches, stores or talks to an OS: a platform hands in
//! PCM and settings and gets PCM and decisions back, so Android and a desktop app sound the same.

pub mod automix;
pub mod burst;
pub mod compressor;
pub mod dac;
pub mod device;
pub mod decode;
pub mod dsp;
pub mod engine;
pub mod eqfit;
pub mod graphic;
pub mod outputs;
pub mod headphones;
pub mod heard;
pub mod pcm;
pub mod seek;
pub mod pipeline;
pub mod playlist;
pub mod policy;
pub mod queue;
pub mod silence;
#[cfg(any(test, feature = "synth"))]
pub mod sim;
pub mod sound;
pub mod sonic;
pub mod spatial;
pub mod speed;
pub mod transitions;
pub mod transport;
pub mod types;

#[cfg(test)]
mod no_alloc;
