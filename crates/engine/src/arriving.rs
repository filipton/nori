//! A song decoded as its bytes arrive (AutoMix's measuring, `core::measure_as_it_comes`): the fetcher
//! (fetching ahead, a download, the next song's loader) hands each piece to a [`Listening`], read by a
//! lowest-priority decoder thread, so the network and CPU wake together and nothing is read back.
//!
//! At most [`PIPE`] bytes wait: a fetch that may wait blocks for room; one that must not (the player's
//! loader) abandons the decoding. The decoder wakes per [`WAKE`] bytes. Only a song fetched whole from its
//! first byte counts as measured.

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use symphonia::core::io::MediaSource;

/// What hears the samples a [`Listening`] decodes.
pub trait Heard: Send {
    fn samples(&mut self, rate: u32, channels: usize, samples: &[f32]);
    /// Decoding ended: `whole` when decoded to the end and the fetch got every byte.
    fn done(self: Box<Self>, whole: bool);
}

/// Bytes that may wait for the decoder.
pub const PIPE: usize = 2 << 20;
/// A waiting decoder is woken once this many bytes wait (or the song ended).
pub const WAKE: usize = 256 << 10;

#[derive(Default)]
struct State {
    bytes: VecDeque<u8>,
    /// The fetch ended, and whether whole.
    ended: Option<bool>,
    /// The decoder stopped reading: incoming bytes are dropped.
    quit: bool,
    /// A non-waiting fetch found no room: decoding abandoned.
    overflowed: bool,
    reader_waits: bool,
    writer_waits: bool,
}

#[derive(Default)]
struct Pipe {
    s: Mutex<State>,
    cv: Condvar,
}

/// The fetch's side of a song decoded as it arrives.
pub struct Listening {
    pipe: Arc<Pipe>,
    /// The fetch may block on the decoder; otherwise decoding is abandoned when behind.
    wait: bool,
    ended: bool,
}

impl Listening {
    /// Starts a decoder thread for a song (`hint`: extension or MIME type) feeding `heard`.
    pub fn start(hint: Option<String>, wait: bool, heard: Box<dyn Heard>) -> Option<Listening> {
        let pipe = Arc::new(Pipe::default());
        pipe.s.lock().bytes.reserve_exact(PIPE);
        let p = pipe.clone();
        std::thread::Builder::new().name("nori-measure".into()).spawn(move || decode(p, hint, heard)).ok()?;
        Some(Listening { pipe, wait, ended: false })
    }

    fn finish(&mut self, whole: bool) {
        if std::mem::replace(&mut self.ended, true) {
            return;
        }
        let mut s = self.pipe.s.lock();
        s.ended = Some(whole && !s.overflowed);
        self.pipe.cv.notify_all();
    }
}

impl Listening {
    /// The song's next bytes, in order.
    pub fn take(&mut self, mut bytes: &[u8]) {
        let mut s = self.pipe.s.lock();
        while !bytes.is_empty() {
            if s.quit || s.overflowed {
                return;
            }
            let room = PIPE - s.bytes.len();
            if room == 0 {
                if !self.wait {
                    s.overflowed = true;
                    s.bytes.clear();
                    self.pipe.cv.notify_all();
                    return;
                }
                s.writer_waits = true;
                self.pipe.cv.wait(&mut s);
                continue;
            }
            let n = room.min(bytes.len());
            s.bytes.extend(&bytes[..n]);
            bytes = &bytes[n..];
            if s.reader_waits && s.bytes.len() >= WAKE {
                self.pipe.cv.notify_all();
            }
        }
    }

    /// The bytes ended; `whole` when every byte came in order. Dropping is `end(false)`.
    pub fn end(mut self, whole: bool) {
        self.finish(whole);
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.finish(false);
    }
}

/// The decoder's side. The first [`HEAD`] bytes are kept so a reader can seek back to the start.
struct Reader {
    pipe: Arc<Pipe>,
    /// Read position, and bytes taken from the pipe.
    at: u64,
    taken: u64,
    head: Vec<u8>,
}

/// First bytes kept for seeking back.
const HEAD: usize = 64 << 10;

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.at < self.taken {
            let from = self.at as usize;
            let n = buf.len().min(self.head.len() - from);
            buf[..n].copy_from_slice(&self.head[from..from + n]);
            self.at += n as u64;
            return Ok(n);
        }
        let n = self.pull(buf)?;
        if (self.taken as usize) < HEAD {
            let keep = n.min(HEAD - self.taken as usize);
            self.head.extend_from_slice(&buf[..keep]);
        }
        self.taken += n as u64;
        self.at = self.taken;
        Ok(n)
    }
}

impl Reader {
    fn pull(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut s = self.pipe.s.lock();
        loop {
            if s.overflowed {
                return Err(io::Error::other("the fetch went on without the decoder"));
            }
            if !s.bytes.is_empty() {
                break;
            }
            if s.ended.is_some() {
                return Ok(0);
            }
            s.reader_waits = true;
            while s.bytes.len() < WAKE && s.ended.is_none() && !s.overflowed {
                self.pipe.cv.wait(&mut s);
            }
            s.reader_waits = false;
        }
        let (a, b) = s.bytes.as_slices();
        let n = buf.len().min(a.len() + b.len());
        let from_a = n.min(a.len());
        buf[..from_a].copy_from_slice(&a[..from_a]);
        buf[from_a..n].copy_from_slice(&b[..n - from_a]);
        s.bytes.drain(..n);
        if s.writer_waits && s.bytes.len() <= PIPE / 2 {
            s.writer_waits = false;
            self.pipe.cv.notify_all();
        }
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => self.at.checked_add_signed(d).ok_or_else(|| io::Error::other("seek before the start"))?,
            SeekFrom::End(_) => return Err(io::Error::new(io::ErrorKind::Unsupported, "a song still coming has no end yet")),
        };
        // Only to where it is, or back into the kept head.
        if p == self.taken || (p < self.taken && self.head.len() as u64 == self.taken) {
            self.at = p;
            return Ok(p);
        }
        Err(io::Error::new(io::ErrorKind::Unsupported, "a song still coming is read in order"))
    }
}

impl MediaSource for Reader {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

fn decode(pipe: Arc<Pipe>, hint: Option<String>, mut heard: Box<dyn Heard>) {
    lower_priority();
    let reader = Reader { pipe: pipe.clone(), at: 0, taken: 0, head: Vec::with_capacity(HEAD) };
    let decoded = crate::demux::decode_as_it_comes(Box::new(reader), hint.as_deref(), |rate, channels, samples| {
        heard.samples(rate, channels, samples);
        true
    });
    // Drop the rest (tags after the music) and wait for the fetch's verdict.
    let whole = {
        let mut s = pipe.s.lock();
        s.quit = true;
        s.bytes.clear();
        pipe.cv.notify_all();
        while s.ended.is_none() {
            pipe.cv.wait(&mut s);
        }
        s.ended == Some(true)
    };
    heard.done(whole && matches!(decoded, Ok(true)));
}

/// Lowers the calling thread to the lowest priority.
pub(crate) fn lower_priority() {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    // SAFETY: a plain syscall; on Linux PRIO_PROCESS with 0 is the calling thread.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
}

/// Returns freed memory to the system after a burst of large frees (a beat model run): Android's
/// allocator otherwise kept ~150 MB. A few ms per run.
pub fn give_memory_back() {
    #[cfg(target_os = "android")]
    {
        // bionic's M_PURGE_ALL (API 34+), else M_PURGE.
        const M_PURGE: libc::c_int = -101;
        const M_PURGE_ALL: libc::c_int = -104;
        extern "C" {
            fn mallopt(param: libc::c_int, value: libc::c_int) -> libc::c_int;
        }
        // SAFETY: plain integers; an unknown option returns 0 and does nothing.
        unsafe {
            if mallopt(M_PURGE_ALL, 0) == 0 {
                mallopt(M_PURGE, 0);
            }
        }
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only releases free memory.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// The calling thread's CPU time, ms, where available.
pub fn thread_cpu_ms() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime writes the timespec it is given.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) } == 0 {
            return Some(t.tv_sec as u64 * 1000 + t.tv_nsec as u64 / 1_000_000);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Sender};

    /// Counts samples and reports how decoding ended.
    struct Count(u64, Sender<(u64, bool)>);

    impl Heard for Count {
        fn samples(&mut self, _rate: u32, _channels: usize, samples: &[f32]) {
            self.0 += samples.len() as u64;
        }
        fn done(self: Box<Self>, whole: bool) {
            let _ = self.1.send((self.0, whole));
        }
    }

    fn wav(frames: usize) -> Vec<u8> {
        let data = (frames * 4) as u32;
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&2u16.to_le_bytes());
        w.extend_from_slice(&44_100u32.to_le_bytes());
        w.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
        w.extend_from_slice(&4u16.to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data.to_le_bytes());
        w.extend((0..frames * 2).flat_map(|i| ((i as i16).wrapping_mul(7)).to_le_bytes()));
        w
    }

    fn fed(bytes: &[u8], piece: usize, whole: bool, wait: bool) -> (u64, bool) {
        let (tx, rx) = channel();
        let mut l = Box::new(Listening::start(Some("wav".into()), wait, Box::new(Count(0, tx))).unwrap());
        for p in bytes.chunks(piece) {
            l.take(p);
        }
        l.end(whole);
        rx.recv().unwrap()
    }

    #[test]
    fn decoded_as_it_arrives() {
        let frames = 44_100 * 40;
        let song = wav(frames);
        assert!(song.len() > 3 * PIPE, "more than the pipe holds: the fetch waits for the decoder");
        assert_eq!(fed(&song, 64 << 10, true, true), (frames as u64 * 2, true), "every sample, and whole");
        let (_, whole) = fed(&song[..song.len() / 2], 64 << 10, false, true);
        assert!(!whole, "a fetch that broke off is not a measured song");
        // Dropped half way: not whole.
        let (tx, rx) = channel();
        let mut l = Listening::start(Some("wav".into()), true, Box::new(Count(0, tx))).unwrap();
        l.take(&song[..PIPE / 2]);
        drop(l);
        assert!(!rx.recv().unwrap().1);
    }

    #[test]
    fn non_waiting_fetch_abandons_decoding() {
        let song = wav(44_100 * 20);
        let (tx, rx) = channel();
        let mut l = Box::new(Listening::start(Some("wav".into()), false, Box::new(Count(0, tx))).unwrap());
        let started = std::time::Instant::now();
        l.take(&song);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "never waited on the decoder");
        l.end(true);
        assert!(!rx.recv().unwrap().1, "given up: not kept as measured");
    }
}
