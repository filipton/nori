//! Interleaved PCM as a platform hands it over: bytes in one of two encodings. The mixer, stretcher
//! and analyser work on samples; these are the byte-level doors into them, so no caller has to know
//! how 16-bit audio is staged through float.

use crate::automix::mixer::Mixer;
use crate::dither::Dither;
use crate::automix::stretch::{Stretcher, BLOCK};

/// Sample encodings, numbered as media3 numbers them (`C.ENCODING_PCM_16BIT`, `C.ENCODING_PCM_FLOAT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Pcm16,
    Float,
}

impl Encoding {
    pub const PCM_16: i32 = 2;
    pub const FLOAT: i32 = 4;

    pub fn media3(self) -> i32 {
        match self {
            Encoding::Pcm16 => Self::PCM_16,
            Encoding::Float => Self::FLOAT,
        }
    }

    pub fn width(self) -> usize {
        match self {
            Encoding::Pcm16 => 2,
            Encoding::Float => 4,
        }
    }
}

/// A stream's shape: rate, channel count and encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: usize,
    pub encoding: Encoding,
}

impl Format {
    pub fn frame_bytes(&self) -> usize {
        self.channels * self.encoding.width()
    }

    /// Duration of `bytes` bytes of this format, µs.
    pub fn us(&self, bytes: usize) -> i64 {
        (bytes / self.frame_bytes().max(1)) as i64 * 1_000_000 / self.rate.max(1) as i64
    }

    /// Bytes in `us` µs of this format, whole frames.
    pub fn bytes(&self, us: i64) -> usize {
        (us.max(0) * self.rate as i64 / 1_000_000) as usize * self.frame_bytes()
    }
}

fn i16s(b: &[u8]) -> impl Iterator<Item = f32> + '_ {
    b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
}

fn f32s(b: &[u8]) -> impl Iterator<Item = f32> + '_ {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
}

/// Decodes interleaved bytes into float samples, appending to `out`.
pub fn to_f32(bytes: &[u8], enc: Encoding, out: &mut Vec<f32>) {
    match enc {
        Encoding::Pcm16 => out.extend(i16s(bytes)),
        Encoding::Float => out.extend(f32s(bytes)),
    }
}

/// Encodes float samples into `out` (which must be exactly the right length).
pub fn from_f32(samples: &[f32], enc: Encoding, out: &mut [u8]) {
    match enc {
        Encoding::Pcm16 => {
            for (d, v) in out.chunks_exact_mut(2).zip(samples) {
                d.copy_from_slice(&((v * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
            }
        }
        Encoding::Float => {
            for (d, v) in out.chunks_exact_mut(4).zip(samples) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// Scales interleaved samples in place by `gain` (0..1: nothing clips), rounding 16-bit ones to the
/// nearest step.
pub fn scale(bytes: &mut [u8], enc: Encoding, gain: f32) {
    match enc {
        Encoding::Pcm16 => {
            for d in bytes.chunks_exact_mut(2) {
                let v = i16::from_le_bytes([d[0], d[1]]) as f32 * gain;
                d.copy_from_slice(&(v.round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
            }
        }
        Encoding::Float => {
            for d in bytes.chunks_exact_mut(4) {
                let v = f32::from_le_bytes([d[0], d[1], d[2], d[3]]) * gain;
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// [`scale`], with 16-bit samples rounded back through TPDF dither ([`crate::dither`]) rather than to the
/// nearest step: ReplayGain on the 16-bit path. `channels` keeps each channel's noise its own.
pub fn scale_dithered(bytes: &mut [u8], enc: Encoding, gain: f32, channels: usize, dither: &mut Dither) {
    match enc {
        Encoding::Pcm16 => {
            let ch = channels.max(1);
            for (i, d) in bytes.chunks_exact_mut(2).enumerate() {
                let v = i16::from_le_bytes([d[0], d[1]]) as f64 * gain as f64 / 32768.0;
                d.copy_from_slice(&dither.to_i16(i % ch, v).to_le_bytes());
            }
        }
        Encoding::Float => scale(bytes, enc, gain),
    }
}

/// Mixes `frames` frames of `outgoing` and `incoming` into `dest` (all the mixer's channel count and
/// `enc`). `dest` may be the same memory as `outgoing`: the mix is written in place over what was held.
///
/// # Safety
/// Each pointer must be valid for `frames * channels` samples of `enc`.
pub unsafe fn mix_raw(m: &mut Mixer, outgoing: *const u8, incoming: *const u8, dest: *mut u8, frames: usize, enc: Encoding) {
    match enc {
        Encoding::Pcm16 => {
            m.run(outgoing as *const i16, incoming as *const i16, dest as *mut i16, frames, |x| x as f64, |y| y.round().clamp(-32768.0, 32767.0) as i16)
        }
        Encoding::Float => m.run(outgoing as *const f32, incoming as *const f32, dest as *mut f32, frames, |x| x as f64, |y| y as f32),
    }
}

/// A stretcher that takes and gives bytes. 16-bit audio is staged through float in fixed blocks, so
/// the steady state allocates nothing.
pub struct ByteStretcher {
    s: Stretcher,
    ch: usize,
    fin: Vec<f32>,
    fout: Vec<f32>,
}

impl ByteStretcher {
    pub fn new(rate: u32, channels: usize, keep_pitch: bool) -> ByteStretcher {
        let ch = channels.clamp(1, crate::automix::stretch::MAX_CHANNELS);
        ByteStretcher { s: Stretcher::new(rate.max(1), ch, keep_pitch), ch, fin: vec![0f32; BLOCK * 4 * ch], fout: vec![0f32; BLOCK * 8 * ch] }
    }

    /// `ratio` playback speed (>1 faster), held for `hold_frames` output frames, then ramped to 1 over `ramp_frames`.
    pub fn configure(&mut self, ratio: f64, hold_frames: u64, ramp_frames: u64) {
        self.s.configure(ratio, hold_frames, ramp_frames);
    }

    pub fn bypassed(&self) -> bool {
        self.s.bypassed()
    }

    pub fn latency_frames(&self) -> usize {
        self.s.latency_frames()
    }

    /// The song time handed out since this was last asked, in input frames ([`Stretcher::take_content`]).
    pub fn take_content(&mut self) -> f64 {
        self.s.take_content()
    }

    /// Runs `input` through into `output`; returns (bytes consumed, bytes produced). Staged through
    /// float in the blocks reserved at creation, for either encoding: nothing is allocated here.
    pub fn process(&mut self, input: &[u8], output: &mut [u8], enc: Encoding) -> (usize, usize) {
        let ch = self.ch;
        let w = enc.width();
        let si = input.len() / w / ch * ch;
        let so = output.len() / w / ch * ch;
        let (mut used, mut made) = (0usize, 0usize);
        loop {
            let n_in = (si - used).min(self.fin.len());
            let n_out = (so - made).min(self.fout.len());
            if n_out == 0 {
                break;
            }
            let src = &input[used * w..(used + n_in) * w];
            match enc {
                Encoding::Pcm16 => {
                    for (d, c) in self.fin[..n_in].iter_mut().zip(src.chunks_exact(2)) {
                        *d = i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0;
                    }
                }
                Encoding::Float => {
                    for (d, c) in self.fin[..n_in].iter_mut().zip(src.chunks_exact(4)) {
                        *d = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                    }
                }
            }
            let (u, m) = self.s.process(&self.fin[..n_in], &mut self.fout[..n_out]);
            from_f32(&self.fout[..m * ch], enc, &mut output[made * w..(made + m * ch) * w]);
            used += u * ch;
            made += m * ch;
            if u == 0 && m == 0 {
                break;
            }
            if used == si && m * ch < n_out {
                break;
            }
        }
        (used * w, made * w)
    }

    /// Writes what is still inside the stretcher to `output`, as much as fits; returns bytes written.
    /// The stretcher hands it over a block at a time: taking only the first block dropped the rest of
    /// its delay line, and the song jumped ahead by that much where the stretch handed back to it.
    pub fn drain(&mut self, output: &mut [u8], enc: Encoding) -> usize {
        let (ch, w) = (self.ch, enc.width());
        let cap = output.len() / w / ch * ch;
        let mut done = 0;
        loop {
            let n = (cap - done).min(self.fout.len());
            if n == 0 {
                break;
            }
            let m = self.s.drain(&mut self.fout[..n]) * ch;
            from_f32(&self.fout[..m], enc, &mut output[done * w..(done + m) * w]);
            done += m;
            if m < n {
                break;
            }
        }
        done * w
    }
}
