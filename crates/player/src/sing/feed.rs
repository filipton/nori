//! A song's samples on their way from the player to the thread that makes its vocal mask: a ring the player's
//! thread writes without waiting or allocating ([`Feeding`]), and the mask maker's thread reads ([`Feed::take`]).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::Thread;

use super::VocalMask;
use crate::pcm::{Encoding, Format};

/// One read of a song for its mask: stereo frames from song frame `from` at `rate`, each two 16-bit samples in a
/// slot.
pub struct Feed {
    pub id: String,
    pub mask: Arc<VocalMask>,
    pub from: u64,
    pub rate: u32,
    slots: Box<[AtomicU32]>,
    fed: AtomicU64,
    taken: AtomicU64,
    /// The read reached the song's end.
    ended: AtomicBool,
    /// The read stopped (another read, a seek, or the ring was full): nothing more comes.
    closed: AtomicBool,
    /// The mask maker's thread, woken when frames wait.
    maker: Thread,
    /// The player's thread, woken when rows come.
    player: Thread,
}

impl Feed {
    /// Room for `seconds` at `rate`; `maker` reads it, `player` is told of rows.
    pub fn new(id: &str, mask: Arc<VocalMask>, from: u64, rate: u32, seconds: u32, maker: Thread, player: Thread) -> Feed {
        let slots = (0..rate as usize * seconds as usize).map(|_| AtomicU32::new(0)).collect();
        Feed { id: id.to_string(), mask, from, rate, slots, fed: AtomicU64::new(0), taken: AtomicU64::new(0), ended: AtomicBool::new(false), closed: AtomicBool::new(false), maker, player }
    }

    /// Appends the frames fed since the last take to `out` (interleaved stereo); returns how many.
    pub fn take(&self, out: &mut Vec<f32>) -> u64 {
        let (from, to) = (self.taken.load(Ordering::Relaxed), self.fed.load(Ordering::Acquire));
        let n = self.slots.len() as u64;
        for f in from..to {
            let v = self.slots[(f % n) as usize].load(Ordering::Relaxed);
            out.push((v as u16 as i16) as f32 / 32768.0);
            out.push(((v >> 16) as u16 as i16) as f32 / 32768.0);
        }
        self.taken.store(to, Ordering::Release);
        to - from
    }

    /// The read reached the song's end.
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Nothing more comes, and everything fed was taken.
    pub fn done(&self) -> bool {
        (self.closed.load(Ordering::Acquire) || self.ended()) && self.taken.load(Ordering::Relaxed) == self.fed.load(Ordering::Acquire)
    }

    /// Rows came: the player looks again.
    pub fn rows_came(&self) {
        self.player.unpark();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.maker.unpark();
    }
}

/// The player's end of a [`Feed`]; dropping it ends the read.
pub struct Feeding(pub Arc<Feed>);

impl Feeding {
    /// The frames of `data` (in `format`): the first two channels, or mono on both. A full ring ends the read.
    pub fn push(&self, data: &[u8], format: Format) {
        let feed = &self.0;
        if feed.closed.load(Ordering::Relaxed) {
            return;
        }
        let fb = format.frame_bytes();
        let frames = (data.len() / fb) as u64;
        let fed = feed.fed.load(Ordering::Relaxed);
        let n = feed.slots.len() as u64;
        if fed + frames - feed.taken.load(Ordering::Acquire) > n {
            feed.close();
            return;
        }
        let w = format.encoding.width();
        let right = if format.channels > 1 { w } else { 0 };
        let sample = |b: &[u8]| -> u16 {
            match format.encoding {
                Encoding::Pcm16 => u16::from_le_bytes([b[0], b[1]]),
                Encoding::Float => ((f32::from_le_bytes([b[0], b[1], b[2], b[3]]) * 32768.0).round().clamp(-32768.0, 32767.0) as i16) as u16,
            }
        };
        for (i, frame) in data.chunks_exact(fb).enumerate() {
            let v = sample(frame) as u32 | (sample(&frame[right..]) as u32) << 16;
            feed.slots[((fed + i as u64) % n) as usize].store(v, Ordering::Relaxed);
        }
        feed.mask.fed(frames);
        feed.fed.store(fed + frames, Ordering::Release);
    }

    /// The maker takes what waits, or makes the rows the player waits for.
    pub fn flush(&self) {
        let feed = &self.0;
        if feed.fed.load(Ordering::Relaxed) > feed.taken.load(Ordering::Acquire) || feed.mask.awaited.load(Ordering::Acquire) {
            feed.maker.unpark();
        }
    }

    /// The read reached the song's end.
    pub fn end(&self) {
        self.0.ended.store(true, Ordering::Release);
        self.0.maker.unpark();
    }
}

impl Drop for Feeding {
    fn drop(&mut self) {
        self.0.close();
    }
}
