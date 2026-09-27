//! Skipping silence, ported line for line from media3's `SilenceSkippingAudioProcessor` (Apache-2.0,
//! Copyright The Android Open Source Project), so it sounds exactly as it did through media3: a quiet
//! stretch longer than [`MIN_SILENCE_US`] is shortened to a fifth of its length (and never more than
//! [`MAX_SILENCE_TO_KEEP_US`]), faded down to a tenth of its volume and back up rather than cut, so it
//! reads as a studio's hush and not as playback stopping. What was dropped is counted, so a player
//! keeps its position honest. media3's took 16-bit PCM only; this one takes float too (the high quality
//! chain), judged against the same level on the 16-bit scale, so a song is skipped through alike either way.
//!
//! media3's processor stops after each piece of output and takes the rest of its input on the next
//! call; this one takes all of it and appends every piece in the same order, which is the same bytes.

/// Below this absolute 16-bit level a sample is silent.
pub const THRESHOLD: i32 = 1024;
/// Silence shorter than this is left alone.
pub const MIN_SILENCE_US: i64 = 100_000;
/// Fraction of a silence kept.
pub const RETENTION_RATIO: f32 = 0.2;
/// Most silence kept, after the ratio.
pub const MAX_SILENCE_TO_KEEP_US: i64 = 2_000_000;
/// Volume a shortened silence is taken down to, percent.
pub const MIN_VOLUME_PERCENT: i32 = 10;
const AVOID_TRUNCATION_FACTOR: i32 = 1000;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Noisy,
    Shortening,
}

#[derive(Clone, Copy, PartialEq)]
enum Volume {
    FadeOut,
    Mute,
    FadeIn,
    Keep,
}

pub struct SilenceSkipper {
    rate: u32,
    /// Bytes per sample: 2 for 16-bit, 4 for float.
    width: usize,
    bytes_per_frame: usize,
    state: State,
    skipped: u64,
    output_silence_frames_since_noise: i64,
    maybe: Vec<u8>,
    maybe_start: usize,
    maybe_size: usize,
    contiguous: Vec<u8>,
}

impl SilenceSkipper {
    /// For 16-bit samples.
    pub fn new(rate: u32, channels: usize) -> SilenceSkipper {
        SilenceSkipper::of(rate, channels, false)
    }

    /// For 16-bit samples, or float ones.
    pub fn of(rate: u32, channels: usize, float: bool) -> SilenceSkipper {
        let width = if float { 4 } else { 2 };
        let bytes_per_frame = channels.clamp(1, 8) * width;
        let mut s = SilenceSkipper {
            rate,
            width,
            bytes_per_frame,
            state: State::Noisy,
            skipped: 0,
            output_silence_frames_since_noise: 0,
            maybe: Vec::new(),
            maybe_start: 0,
            maybe_size: 0,
            contiguous: Vec::new(),
        };
        // Divide by 2 to allow the buffer to be split into two frame-aligned halves.
        let size = s.align((s.frames(MIN_SILENCE_US) as usize * bytes_per_frame / 2) as i32) as usize * 2;
        s.maybe = vec![0; size];
        s.contiguous = vec![0; size];
        s
    }

    /// Frames dropped since the last flush.
    #[cfg(any(test, feature = "synth"))]
    pub fn skipped_frames(&self) -> u64 {
        self.skipped
    }

    pub fn flush(&mut self) {
        self.state = State::Noisy;
        self.skipped = 0;
        self.output_silence_frames_since_noise = 0;
        self.maybe_start = 0;
        self.maybe_size = 0;
    }

    fn frames(&self, us: i64) -> i64 {
        us * self.rate as i64 / 1_000_000
    }

    fn align(&self, v: i32) -> i32 {
        (v / self.bytes_per_frame as i32) * self.bytes_per_frame as i32
    }

    /// The sample at byte `i`, on the 16-bit scale for both widths.
    fn sample(&self, b: &[u8], i: usize) -> i32 {
        if self.width == 4 {
            (f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) * 32768.0) as i32
        } else {
            i16::from_le_bytes([b[i], b[i + 1]]) as i32
        }
    }

    fn is_noise(&self, b: &[u8], i: usize) -> bool {
        self.sample(b, i).saturating_abs() > THRESHOLD
    }

    /// The first frame boundary at or after `from` holding a noisy sample, or `to`.
    fn find_noise_position(&self, b: &[u8], from: usize, to: usize) -> usize {
        let mut i = from;
        while i + self.width <= to {
            if self.is_noise(b, i) {
                return self.bytes_per_frame * (i / self.bytes_per_frame);
            }
            i += self.width;
        }
        to
    }

    /// The earliest position in [from, to) from which every frame to `to` is silent.
    fn find_noise_limit(&self, b: &[u8], from: usize, to: usize) -> usize {
        let mut i = to as isize - self.width as isize;
        while i >= from as isize {
            if self.is_noise(b, i as usize) {
                return self.bytes_per_frame * (i as usize / self.bytes_per_frame) + self.bytes_per_frame;
            }
            i -= self.width as isize;
        }
        from
    }

    /// Little-endian input in, 16-bit or float as made; what survives is appended to `out`.
    pub fn process(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let mut pos = 0;
        while pos < input.len() {
            pos = match self.state {
                State::Noisy => self.process_noisy(input, pos, out),
                State::Shortening => self.shorten_until_noise(input, pos, out),
            };
        }
    }

    fn process_noisy(&mut self, b: &[u8], pos: usize, out: &mut Vec<u8>) -> usize {
        let limit = b.len().min(pos + self.maybe.len());
        let noise_limit = self.find_noise_limit(b, pos, limit);
        if noise_limit == pos {
            self.state = State::Shortening;
            pos
        } else {
            out.extend_from_slice(&b[pos..noise_limit]);
            noise_limit
        }
    }

    fn shorten_until_noise(&mut self, b: &[u8], pos: usize, out: &mut Vec<u8>) -> usize {
        let len = self.maybe.len();
        let noise_position = self.find_noise_position(b, pos, b.len());
        let silence_input = noise_position - pos;
        let (index, contiguous_remaining) = if self.maybe_start + self.maybe_size < len {
            (self.maybe_start + self.maybe_size, len - (self.maybe_size + self.maybe_start))
        } else {
            let upper = len - self.maybe_start;
            let index = self.maybe_size - upper;
            (index, self.maybe_start - index)
        };
        let noise_found = noise_position < b.len();
        let n = silence_input.min(contiguous_remaining);
        self.maybe[index..index + n].copy_from_slice(&b[pos..pos + n]);
        self.maybe_size += n;
        let to_noisy = noise_found && silence_input < contiguous_remaining;
        self.output_shortened(to_noisy, out);
        if to_noisy {
            self.state = State::Noisy;
            self.output_silence_frames_since_noise = 0;
        }
        pos + n
    }

    fn output_shortened(&mut self, to_noisy: bool, out: &mut Vec<u8>) {
        let len = self.maybe.len();
        if !(self.maybe_size == len || to_noisy) {
            return;
        }
        let (bytes_to_output, consumed);
        if self.output_silence_frames_since_noise == 0 {
            if to_noisy {
                bytes_to_output = self.maybe_size;
                self.output_silence(bytes_to_output, Volume::Keep, out);
                consumed = bytes_to_output;
            } else {
                bytes_to_output = len / 2;
                self.output_silence(bytes_to_output, Volume::FadeOut, out);
                consumed = bytes_to_output;
            }
        } else if to_noisy {
            let remaining_after_half = self.maybe_size - len / 2;
            consumed = remaining_after_half + len / 2;
            let shortened = self.shortened_length(remaining_after_half as i32) as usize;
            bytes_to_output = len / 2 + shortened;
            self.output_silence(bytes_to_output, Volume::FadeIn, out);
        } else {
            consumed = self.maybe_size - len / 2;
            bytes_to_output = self.shortened_length(consumed as i32) as usize;
            self.output_silence(bytes_to_output, Volume::Mute, out);
        }
        self.maybe_size -= consumed;
        self.maybe_start = (self.maybe_start + consumed) % len;
        self.output_silence_frames_since_noise += (bytes_to_output / self.bytes_per_frame) as i64;
        self.skipped += ((consumed - bytes_to_output) / self.bytes_per_frame) as u64;
    }

    fn shortened_length(&self, to_shorten: i32) -> i32 {
        let needed = (self.frames(MAX_SILENCE_TO_KEEP_US) - self.output_silence_frames_since_noise) as i32 * self.bytes_per_frame as i32
            - self.maybe.len() as i32 / 2;
        let v = (to_shorten as f32 * RETENTION_RATIO + 0.5).min(needed as f32);
        self.align(v as i32).max(0)
    }

    fn output_silence(&mut self, size: usize, volume: Volume, out: &mut Vec<u8>) {
        if size == 0 {
            return;
        }
        let len = self.maybe.len();
        if volume == Volume::FadeIn {
            // Keeps the end of the contents: it pads the start of the next noise.
            if self.maybe_start + self.maybe_size <= len {
                let from = self.maybe_start + self.maybe_size - size;
                self.contiguous[..size].copy_from_slice(&self.maybe[from..from + size]);
            } else {
                let upper = len - self.maybe_start;
                let lower = self.maybe_size - upper;
                if lower >= size {
                    self.contiguous[..size].copy_from_slice(&self.maybe[lower - size..lower]);
                } else {
                    let in_upper = size - lower;
                    self.contiguous[..in_upper].copy_from_slice(&self.maybe[len - in_upper..len]);
                    self.contiguous[in_upper..in_upper + lower].copy_from_slice(&self.maybe[..lower]);
                }
            }
        } else if self.maybe_start + size <= len {
            self.contiguous[..size].copy_from_slice(&self.maybe[self.maybe_start..self.maybe_start + size]);
        } else {
            let upper = len - self.maybe_start;
            self.contiguous[..upper].copy_from_slice(&self.maybe[self.maybe_start..len]);
            self.contiguous[upper..size].copy_from_slice(&self.maybe[..size - upper]);
        }
        self.modify_volume(size, volume);
        out.extend_from_slice(&self.contiguous[..size]);
    }

    fn modify_volume(&mut self, size: usize, volume: Volume) {
        if volume == Volume::Keep {
            return;
        }
        let last = (size / self.bytes_per_frame) as i32 - 1;
        let mut idx = 0;
        while idx < size {
            let frame = (idx / self.bytes_per_frame) as i32;
            let pct = match volume {
                Volume::FadeOut => Self::fade_out(frame, last),
                Volume::FadeIn => Self::fade_in(frame, last),
                _ => MIN_VOLUME_PERCENT,
            };
            if self.width == 4 {
                let b = &self.contiguous[idx..idx + 4];
                let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]) * pct as f32 / 100.0;
                self.contiguous[idx..idx + 4].copy_from_slice(&v.to_le_bytes());
            } else {
                let s = self.sample(&self.contiguous, idx);
                let v = (s * pct / 100).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                self.contiguous[idx..idx + 2].copy_from_slice(&v.to_le_bytes());
            }
            idx += self.width;
        }
    }

    fn fade_out(value: i32, max: i32) -> i32 {
        if max == 0 {
            return MIN_VOLUME_PERCENT;
        }
        ((MIN_VOLUME_PERCENT - 100) * ((AVOID_TRUNCATION_FACTOR * value) / max)) / AVOID_TRUNCATION_FACTOR + 100
    }

    fn fade_in(value: i32, max: i32) -> i32 {
        if max == 0 {
            return 100;
        }
        MIN_VOLUME_PERCENT + ((100 - MIN_VOLUME_PERCENT) * (AVOID_TRUNCATION_FACTOR * value) / max) / AVOID_TRUNCATION_FACTOR
    }

    /// The input has ended: a silence still held goes out as the end of a pause.
    pub fn end_of_stream(&mut self, out: &mut Vec<u8>) {
        if self.maybe_size > 0 {
            self.output_shortened(true, out);
            self.output_silence_frames_since_noise = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44100;

    fn frames(v: i16, secs: f64) -> Vec<u8> {
        (0..(RATE as f64 * secs) as usize * 2).flat_map(|_| v.to_le_bytes()).collect()
    }

    fn run(s: &mut SilenceSkipper, x: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in x.chunks(4096) {
            s.process(c, &mut out);
        }
        s.end_of_stream(&mut out);
        out
    }

    #[test]
    fn a_long_pause_is_shortened_and_counted() {
        let mut s = SilenceSkipper::new(RATE, 2);
        let x = [frames(8000, 1.0), frames(0, 3.0), frames(8000, 1.0)].concat();
        let y = run(&mut s, &x);
        let secs = y.len() as f64 / 4.0 / RATE as f64;
        // Two seconds of music, and the pause cut to about a fifth of itself.
        assert!(secs > 2.4 && secs < 2.8, "{secs}");
        assert_eq!(s.skipped_frames() as usize + y.len() / 4, x.len() / 4, "every frame is either played or counted as skipped");
    }

    #[test]
    fn a_short_rest_is_music() {
        let mut s = SilenceSkipper::new(RATE, 2);
        let x = [frames(8000, 1.0), frames(0, 0.05), frames(8000, 1.0)].concat();
        assert_eq!(run(&mut s, &x), x, "50 ms of quiet is left exactly as it was");
    }

    #[test]
    fn quiet_music_is_not_silence() {
        let mut s = SilenceSkipper::new(RATE, 2);
        let x = frames(1500, 2.0);
        assert_eq!(run(&mut s, &x), x);
    }

    /// The same music in float is skipped through exactly as in 16 bits: the same frames kept, faded alike.
    #[test]
    fn float_is_skipped_as_16_bit_is() {
        let x = [frames(8000, 1.0), frames(300, 3.0), frames(-8000, 0.5), frames(0, 0.05), frames(8000, 1.0)].concat();
        let f: Vec<u8> = x.chunks_exact(2).flat_map(|c| (i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).to_le_bytes()).collect();
        let (mut a, mut b) = (SilenceSkipper::new(RATE, 2), SilenceSkipper::of(RATE, 2, true));
        let (y16, yf) = (run(&mut a, &x), run(&mut b, &f));
        assert_eq!(a.skipped_frames(), b.skipped_frames());
        assert!(a.skipped_frames() > RATE as u64, "the pause was shortened");
        let back: Vec<i16> = yf.chunks_exact(4).map(|c| (f32::from_le_bytes([c[0], c[1], c[2], c[3]]) * 32768.0) as i16).collect();
        let want: Vec<i16> = y16.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(back.len(), want.len());
        assert!(back.iter().zip(&want).all(|(p, q)| (p - q).abs() <= 1), "the same fades, to the 16-bit step");
    }
}
