//! A live MP3 stream's frames, found here rather than by symphonia's reader, which can walk from one
//! false header to the next inside the music forever and scans noise without bound on the engine's
//! thread. A station may start mid-frame, carry noise between songs, and change rate or channels.
//!
//! - A frame right after the last, of the same shape, is taken.
//! - Any other frame only when the next header follows it with the same shape.
//! - At most [`SCAN`] bytes are scanned per call; then `WouldBlock`, and the engine asks again later.
//! - A frame cut short by the end of the stream is dropped.
//!
//! Layer III only. Each frame is a symphonia packet (one allocation, as symphonia's readers do).

use std::io::{self, Read};

use symphonia::core::errors::{Error, Result};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

/// Most bytes scanned per call: under `source::LIVE_READY` (32 KiB), so a call never waits for bytes.
pub(crate) const SCAN: usize = 16 * 1024;
/// Bytes read from the stream at a time.
const CHUNK: usize = 4096;

/// A layer III frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub rate: u32,
    pub channels: usize,
    /// Frame length, header included.
    pub len: usize,
    /// Audio frames it decodes to.
    pub samples: u64,
}

impl Header {
    /// Parses a layer III header at the start of `b`; None for other layers, free-format or reserved values.
    pub fn parse(b: &[u8]) -> Option<Header> {
        let h = u32::from_be_bytes(b.get(..4)?.try_into().ok()?);
        let (version, layer, bitrate, rate_index, padding) = ((h >> 19) & 3, (h >> 17) & 3, (h >> 12) & 0xf, (h >> 10) & 3, (h >> 9) & 1);
        if h >> 21 != 0x7ff || version == 1 || layer != 1 || bitrate == 0 || bitrate == 0xf || rate_index == 3 {
            return None;
        }
        let mpeg1 = version == 3;
        let kbps: u32 = if mpeg1 {
            [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320][bitrate as usize]
        } else {
            [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160][bitrate as usize]
        };
        let base = [44_100, 48_000, 32_000][rate_index as usize];
        let rate = match version {
            3 => base,
            2 => base / 2,
            _ => base / 4,
        };
        let per = if mpeg1 { 144 } else { 72 };
        let len = (per * kbps * 1000 / rate + padding) as usize;
        let channels = if (h >> 6) & 3 == 3 { 1 } else { 2 };
        Some(Header { rate, channels, len, samples: if mpeg1 { 1152 } else { 576 } })
    }

    fn shape(&self) -> (u32, usize) {
        (self.rate, self.channels)
    }
}

/// A live MP3 stream read frame by frame.
pub(crate) struct Frames {
    source: MediaSourceStream<'static>,
    /// Bytes read and not yet handed out, from `at`.
    buf: Vec<u8>,
    at: usize,
    eof: bool,
    /// Rate and channels of the last frame handed out.
    shape: Option<(u32, usize)>,
    /// The next byte directly follows the last frame handed out.
    synced: bool,
    pts: u64,
    track: u32,
}

impl Frames {
    pub fn new(source: MediaSourceStream<'static>, track: u32) -> Frames {
        Frames { source, buf: Vec::with_capacity(4 * CHUNK), at: 0, eof: false, shape: None, synced: false, pts: 0, track }
    }

    /// Reads until `need` bytes are buffered from `at` or the stream ends; returns whether they are.
    fn fill(&mut self, need: usize) -> io::Result<bool> {
        while self.buf.len() - self.at < need && !self.eof {
            if self.at > 0 {
                self.buf.drain(..self.at);
                self.at = 0;
            }
            let len = self.buf.len();
            self.buf.resize(len + CHUNK, 0);
            let n = loop {
                match self.source.read(&mut self.buf[len..]) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    other => break other,
                }
            };
            self.buf.truncate(len + *n.as_ref().unwrap_or(&0));
            match n {
                Ok(0) => self.eof = true,
                Ok(_) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(self.buf.len() - self.at >= need)
    }

    /// The next frame; None at the end. `WouldBlock` after [`SCAN`] bytes without one: ask again.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        let mut skipped = 0usize;
        loop {
            if !self.fill(4)? {
                return Ok(None);
            }
            let found = Header::parse(&self.buf[self.at..]);
            if let Some(f) = found {
                if !self.fill(f.len)? {
                    self.at += 1;
                    self.synced = false;
                    skipped += 1;
                    continue;
                }
                let next = if self.fill(f.len + 4)? { Header::parse(&self.buf[self.at + f.len..]) } else { None };
                let known = self.shape == Some(f.shape());
                if (self.synced && known) || next.is_some_and(|n| n.shape() == f.shape()) {
                    let data: Box<[u8]> = self.buf[self.at..self.at + f.len].into();
                    self.at += f.len;
                    self.synced = true;
                    self.shape = Some(f.shape());
                    let pts = self.pts;
                    self.pts += f.samples;
                    return Ok(Some(Packet::new(self.track, Timestamp::new(pts as i64), Duration::new(f.samples), data)));
                }
            }
            // Not a frame: skip to the next 0xff.
            self.synced = false;
            let step = self.buf[self.at + 1..].iter().position(|&b| b == 0xff).map_or(self.buf.len() - self.at, |p| p + 1);
            self.at += step;
            skipped += step;
            if skipped >= SCAN {
                return Err(Error::IoError(io::Error::new(io::ErrorKind::WouldBlock, "no MPEG frame found yet")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia::core::io::MediaSourceStreamOptions;

    /// A 44.1 kHz stereo MPEG-1 frame (417 bytes) or a 22.05 kHz mono MPEG-2 one (104), body `fill`.
    fn frame(mpeg1: bool, fill: u8) -> Vec<u8> {
        let head: [u8; 4] = if mpeg1 { [0xff, 0xfb, 0x90, 0x00] } else { [0xff, 0xf3, 0x40, 0xc0] };
        let len = Header::parse(&head).unwrap().len;
        let mut f = head.to_vec();
        f.resize(len, fill);
        f
    }

    fn frames_of(bytes: Vec<u8>) -> Frames {
        let mss = MediaSourceStream::new(Box::new(io::Cursor::new(bytes)), MediaSourceStreamOptions::default());
        Frames::new(mss, 0)
    }

    /// Every frame as `(rate, channels, first body byte)`, and the `WouldBlock` count.
    fn read_all(mut f: Frames) -> (Vec<(u32, usize, u8)>, usize) {
        let (mut got, mut yields) = (Vec::new(), 0);
        loop {
            match f.next_packet() {
                Ok(Some(p)) => {
                    let h = Header::parse(&p.data).unwrap();
                    got.push((h.rate, h.channels, p.data[4]));
                }
                Ok(None) => return (got, yields),
                Err(Error::IoError(e)) if e.kind() == io::ErrorKind::WouldBlock => yields += 1,
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn headers() {
        assert_eq!(Header::parse(&[0xff, 0xfb, 0x90, 0x00]), Some(Header { rate: 44_100, channels: 2, len: 417, samples: 1152 }));
        assert_eq!(Header::parse(&[0xff, 0xfb, 0x92, 0x00]).map(|h| h.len), Some(418), "padded");
        assert_eq!(Header::parse(&[0xff, 0xf3, 0x40, 0xc0]), Some(Header { rate: 22_050, channels: 1, len: 104, samples: 576 }));
        assert_eq!(Header::parse(&[0xff, 0xe3, 0x40, 0x00]).map(|h| (h.rate, h.len)), Some((11_025, 208)), "MPEG-2.5");
        assert_eq!(Header::parse(&[0xff, 0xfd, 0x90, 0x00]), None, "layer II");
        assert_eq!(Header::parse(&[0xff, 0xfb, 0x00, 0x00]), None, "free format");
        assert_eq!(Header::parse(&[0xff, 0xfb, 0x9c, 0x00]), None, "reserved rate");
        assert_eq!(Header::parse(&[0xff, 0xeb, 0x90, 0x00]), None, "reserved version");

        // Shape change needs two agreeing frames.
        let mut bytes = Vec::new();
        for i in 1..4 {
            bytes.extend_from_slice(&frame(true, i));
        }
        // A lone header of another shape: noise.
        bytes.extend_from_slice(&[0xff, 0xf3, 0x40, 0xc0, 9, 9, 9]);
        for i in 4..6 {
            bytes.extend_from_slice(&frame(true, i));
        }
        for i in 6..9 {
            bytes.extend_from_slice(&frame(false, i));
        }
        let (got, _) = read_all(frames_of(bytes));
        assert_eq!(
            got,
            [(44_100, 2, 1), (44_100, 2, 2), (44_100, 2, 3), (44_100, 2, 4), (44_100, 2, 5), (22_050, 1, 6), (22_050, 1, 7), (22_050, 1, 8)]
        );
    }

    #[test]
    fn mid_frame_starts_at_whole_frame() {
        let mut bytes = frame(true, 1)[200..].to_vec();
        for i in 2..6 {
            bytes.extend_from_slice(&frame(true, i));
        }
        let (got, _) = read_all(frames_of(bytes));
        assert_eq!(got.iter().map(|g| g.2).collect::<Vec<_>>(), [2, 3, 4, 5]);
    }

    #[test]
    fn noise_scan_bounded() {
        let mut bytes = frame(true, 1);
        bytes.extend_from_slice(&frame(true, 2));
        // 100 kB of noise, two frames, and a cut one.
        bytes.extend((0..100_000u32).map(|i| if i % 7 == 0 { 0xff } else { (i * 31 % 251) as u8 }));
        bytes.extend_from_slice(&frame(false, 3));
        bytes.extend_from_slice(&frame(false, 4));
        bytes.extend_from_slice(&frame(false, 5)[..50]);
        let (got, yields) = read_all(frames_of(bytes));
        assert_eq!(got.iter().map(|g| g.2).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert!(yields >= 100_000 / SCAN, "{yields} calls gave up for now");
    }
}
