//! Interleaved PCM bytes (16-bit or float) and byte-level adapters for the sample-based mixer and
//! stretcher.

use crate::dither::Dither;
use crate::automix::stretch::{Stretcher, BLOCK};

/// Sample encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Pcm16,
    Float,
}

impl Encoding {
    /// media3's `C.ENCODING_PCM_16BIT` / `C.ENCODING_PCM_FLOAT`.
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

/// Rate, channel count and encoding of a stream.
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
    b.as_chunks::<2>().0.iter().map(|&c| i16::from_le_bytes(c) as f32 / 32768.0)
}

fn f32s(b: &[u8]) -> impl Iterator<Item = f32> + '_ {
    b.as_chunks::<4>().0.iter().map(|&c| f32::from_le_bytes(c))
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
            for (d, v) in out.as_chunks_mut::<2>().0.iter_mut().zip(samples) {
                *d = ((v * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes();
            }
        }
        Encoding::Float => {
            for (d, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(samples) {
                *d = v.to_le_bytes();
            }
        }
    }
}

/// Scales samples in place by `gain` (0..1, so nothing clips), 16-bit rounded to nearest.
pub fn scale(bytes: &mut [u8], enc: Encoding, gain: f32) {
    match enc {
        Encoding::Pcm16 => {
            for d in bytes.as_chunks_mut::<2>().0 {
                let v = i16::from_le_bytes(*d) as f32 * gain;
                *d = (v.round().clamp(-32768.0, 32767.0) as i16).to_le_bytes();
            }
        }
        Encoding::Float => {
            for d in bytes.as_chunks_mut::<4>().0 {
                *d = (f32::from_le_bytes(*d) * gain).to_le_bytes();
            }
        }
    }
}

/// [`scale`] with 16-bit samples dithered instead of rounded (ReplayGain on the 16-bit path).
pub fn scale_dithered(bytes: &mut [u8], enc: Encoding, gain: f32, channels: usize, dither: &mut Dither) {
    match enc {
        Encoding::Pcm16 => {
            let ch = channels.max(1);
            for (i, d) in bytes.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let v = i16::from_le_bytes(*d) as f64 * gain as f64 / 32768.0;
                *d = dither.to_i16(i % ch, v).to_le_bytes();
            }
        }
        Encoding::Float => scale(bytes, enc, gain),
    }
}

/// A [`Stretcher`] over bytes, staged through fixed float blocks so it never allocates after creation.
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

    /// Speed `ratio` (>1 faster) for `hold_frames` output frames, then ramped to 1 over `ramp_frames`.
    pub fn configure(&mut self, ratio: f64, hold_frames: u64, ramp_frames: u64) {
        self.s.configure(ratio, hold_frames, ramp_frames);
    }

    pub fn bypassed(&self) -> bool {
        self.s.bypassed()
    }

    pub fn latency_frames(&self) -> usize {
        self.s.latency_frames()
    }

    /// Input frames consumed since the last call ([`Stretcher::take_content`]).
    pub fn take_content(&mut self) -> f64 {
        self.s.take_content()
    }

    /// Returns (bytes consumed, bytes produced).
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
                Encoding::Pcm16 => self.fin[..n_in].iter_mut().zip(i16s(src)).for_each(|(d, v)| *d = v),
                Encoding::Float => self.fin[..n_in].iter_mut().zip(f32s(src)).for_each(|(d, v)| *d = v),
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

    /// Drains the stretcher's delay line into `output` block by block, as much as fits; returns bytes
    /// written.
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
