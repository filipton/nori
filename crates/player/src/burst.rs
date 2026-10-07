//! Feeding the output in bursts: [`Fed`] lets the deep output buffer ([`BUFFER_US`]) drain to
//! [`LOW_US`], then fills it to the top, so the audio path sleeps between bursts instead of waking for
//! every decoder buffer. Refused offers never reach the output.

use crate::engine::Downstream;
use crate::pcm::Format;

/// Output buffer depth for bursts.
pub const BUFFER_US: i64 = 10_000_000;
/// Offer audio again once the output holds less than this.
pub const LOW_US: i64 = 2_000_000;
/// Allowed overshoot of the clock beyond what was queued between two readings.
const JUMP_US: i64 = 250_000;

/// Burst state kept between calls.
#[derive(Debug, Clone)]
pub struct Burst {
    filling: bool,
    /// Audio in the output = written - played, counted from bytes written and clock movement, never from
    /// timestamps (a mix's timestamps jump ahead by the mix length).
    written_us: i64,
    played_us: i64,
    last_position: Option<i64>,
    last_read_ms: i64,
    format: Option<Format>,
    /// Bytes handed to the output in total.
    pub bytes_written: u64,
}

impl Default for Burst {
    fn default() -> Self {
        Burst { filling: true, written_us: 0, played_us: 0, last_position: None, last_read_ms: 0, format: None, bytes_written: 0 }
    }
}

impl Burst {
    /// Resets the count (play, pause, flush, new output). Can only make feeding start early, never late.
    pub fn restart(&mut self) {
        self.filling = true;
        self.written_us = 0;
        self.played_us = 0;
        self.last_position = None;
    }

    /// Audio in the output at clock `position`. A clock jump beyond what was queued counts as at most
    /// the wall time that passed.
    fn queued_us(&mut self, position: Option<i64>, now_ms: i64) -> Option<i64> {
        let position = position?;
        let wall = (now_ms - self.last_read_ms) * 1000;
        let last = self.last_position.replace(position);
        self.last_read_ms = now_ms;
        if let Some(last) = last {
            let queued = self.written_us - self.played_us;
            self.played_us += match position - last {
                m if m <= 0 => 0,
                m if m <= queued + JUMP_US => m,
                _ => wall.min(queued),
            };
        }
        Some((self.written_us - self.played_us).max(0))
    }
}

/// The output, fed in bursts. Made per call into the engine; `now_ms` is that call's clock.
pub struct Fed<'a, D> {
    pub down: &'a mut D,
    pub burst: &'a mut Burst,
    pub now_ms: i64,
    /// The output's clock, read at most once per call (outer `None`: not read yet).
    position: Option<Option<i64>>,
    /// A discontinuity was passed down: the output may resync its clock on the next offer, so it is
    /// re-read after each offer until audio has gone in (the engine measures that jump).
    resynced: bool,
    /// Song frames per frame ([`Downstream::media_pace`]); written audio is counted in song time.
    pace: f64,
}

impl<'a, D: Downstream> Fed<'a, D> {
    pub fn new(down: &'a mut D, burst: &'a mut Burst, now_ms: i64) -> Self {
        Fed { down, burst, now_ms, position: None, resynced: false, pace: 1.0 }
    }

    fn clock(&mut self) -> Option<i64> {
        *self.position.get_or_insert_with(|| self.down.position_us(false))
    }

    fn after_offer(&mut self, used: usize) {
        if self.resynced {
            self.position = None;
            if used > 0 {
                self.resynced = false;
            }
        }
    }
}

impl<D: Downstream> Downstream for Fed<'_, D> {
    fn configure(&mut self, format: Format) {
        self.burst.restart();
        self.burst.format = Some(format);
        self.position = None;
        self.down.configure(format);
    }

    fn handle_buffer(&mut self, data: &[u8], from: usize, pts_us: i64) -> (bool, usize) {
        if !self.burst.filling {
            // No clock (stopped, never played, restarted): feed rather than wait forever.
            let (position, now) = (self.clock(), self.now_ms);
            if self.burst.queued_us(position, now).is_some_and(|q| q > LOW_US) {
                return (false, 0);
            }
            self.burst.filling = true;
        }
        let (taken, used) = self.down.handle_buffer(data, from, pts_us);
        self.after_offer(used);
        self.burst.bytes_written += used as u64;
        if let Some(f) = self.burst.format.filter(|f| f.frame_bytes() > 0 && f.rate > 0) {
            self.burst.written_us += ((used / f.frame_bytes()) as f64 * self.pace * 1_000_000.0 / f.rate as f64) as i64;
        }
        if !taken {
            // Full: wait until it drains to the low mark.
            self.burst.filling = false;
        }
        (taken, used)
    }

    fn media_pace(&mut self, pace: f64) {
        self.pace = pace;
        self.down.media_pace(pace);
    }

    fn applies_gain(&self) -> bool {
        self.down.applies_gain()
    }

    fn song_gain(&mut self, gain: f32) {
        self.down.song_gain(gain);
    }

    fn handle_discontinuity(&mut self) {
        // Written audio stays in the output, so the count stands; `queued_us` ignores the clock's jump.
        self.burst.filling = true;
        self.resynced = true;
        self.down.handle_discontinuity();
    }

    fn position_us(&mut self, source_ended: bool) -> Option<i64> {
        if source_ended {
            self.down.position_us(true)
        } else {
            self.clock()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm::Encoding;

    const FMT: Format = Format { rate: 44_100, channels: 2, encoding: Encoding::Pcm16 };

    /// An output with a `cap_us` buffer; the test moves its playhead.
    struct Track {
        cap_us: i64,
        written_us: i64,
        played_us: i64,
        offers: usize,
    }

    impl Downstream for Track {
        fn configure(&mut self, _: Format) {}
        fn handle_buffer(&mut self, data: &[u8], from: usize, _: i64) -> (bool, usize) {
            self.offers += 1;
            let room = FMT.bytes(self.cap_us - (self.written_us - self.played_us)).min(data.len() - from);
            let room = room / FMT.frame_bytes() * FMT.frame_bytes();
            self.written_us += FMT.us(room);
            (room == data.len() - from, room)
        }
        fn handle_discontinuity(&mut self) {}
        fn position_us(&mut self, _: bool) -> Option<i64> {
            Some(self.played_us)
        }
    }

    fn offer(t: &mut Track, b: &mut Burst, now_ms: i64) -> bool {
        let chunk = vec![0u8; FMT.bytes(26_000)];
        let mut f = Fed::new(t, b, now_ms);
        f.handle_buffer(&chunk, 0, 0).0
    }

    #[test]
    fn fills_then_waits_for_low_mark() {
        let (mut t, mut b) = (Track { cap_us: BUFFER_US, written_us: 0, played_us: 0, offers: 0 }, Burst::default());
        Fed::new(&mut t, &mut b, 0).configure(FMT);
        let mut now = 0;
        while offer(&mut t, &mut b, now) {}
        assert!(t.written_us >= BUFFER_US - 30_000, "filled: {}", t.written_us);
        let offers = t.offers;
        // 7 s of playback, offered every 10 ms: no offer reaches the output.
        for _ in 0..700 {
            now += 10;
            t.played_us += 10_000;
            assert!(!offer(&mut t, &mut b, now));
        }
        assert_eq!(t.offers, offers, "refused without touching the output");
        // Below the low mark it is fed and filled again.
        while t.offers == offers {
            now += 10;
            t.played_us += 10_000;
            offer(&mut t, &mut b, now);
        }
        assert!(t.written_us - t.played_us <= LOW_US + 30_000, "fed only once it ran low: {}", t.written_us - t.played_us);
        while offer(&mut t, &mut b, now) {}
        assert!(t.written_us - t.played_us >= BUFFER_US - 60_000, "and filled again");
    }

    #[test]
    fn clock_jump_is_not_counted_as_played() {
        let (mut t, mut b) = (Track { cap_us: BUFFER_US, written_us: 0, played_us: 0, offers: 0 }, Burst::default());
        Fed::new(&mut t, &mut b, 0).configure(FMT);
        while offer(&mut t, &mut b, 0) {}
        offer(&mut t, &mut b, 10);
        // The clock leaps 20 s in 100 ms (a mix in the next song's time): only 100 ms counts as played.
        t.played_us += 20_000_000;
        let before = t.offers;
        assert!(!offer(&mut t, &mut b, 110));
        assert_eq!(t.offers, before);
    }
}
