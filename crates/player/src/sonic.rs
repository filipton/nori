//! Sonic speed/pitch (pitch-synchronous overlap-add), a line-for-line port of media3's `Sonic.java`
//! (Apache-2.0, Copyright 2017 The Android Open Source Project, Copyright 2010 Bill Cox, Sonic Library;
//! https://github.com/waywardgeek/sonic), 16-bit and float variants, sample-exact with media3.

const MINIMUM_PITCH: i32 = 65;
const MAXIMUM_PITCH: i32 = 400;
const AMDF_FREQUENCY: i32 = 4000;
const MINIMUM_SPEEDUP_RATE: f32 = 1.00001;
const MINIMUM_SLOWDOWN_RATE: f32 = 0.99999;

/// Per-format arithmetic, as in media3's two implementations.
pub trait Sample: Copy + Default + Send + 'static {
    fn overlap_add(frame_count: usize, ch: usize, out: &mut [Self], out_pos: usize, input: &[Self], down: usize, up: usize);
    fn interpolate(input: &[Self], pos: usize, ch: usize, old_rate_pos: i64, new_rate_pos: i64, old_rate: i64, new_rate: i64) -> Self;
    /// Returns (period, min_diff, max_diff) as media3 stores them.
    fn find_period(samples: &[Self], pos_frames: usize, ch: usize, min_period: i32, max_period: i32) -> (i32, f64, f64);
    fn down_sample(input: &[Self], pos_frames: usize, skip: usize, frame_count: usize, ch: usize, out: &mut [Self]);
}

impl Sample for i16 {
    fn overlap_add(frame_count: usize, ch: usize, out: &mut [i16], out_pos: usize, input: &[i16], down: usize, up: usize) {
        let n = frame_count as i32;
        for i in 0..ch {
            let (mut o, mut u, mut d) = (out_pos * ch + i, up * ch + i, down * ch + i);
            for t in 0..n {
                out[o] = ((input[d] as i32 * (n - t) + input[u] as i32 * t) / n) as i16;
                o += ch;
                d += ch;
                u += ch;
            }
        }
    }

    fn interpolate(input: &[i16], pos: usize, ch: usize, old_rate_pos: i64, new_rate_pos: i64, old_rate: i64, new_rate: i64) -> i16 {
        let left = input[pos] as i64;
        let right = input[pos + ch] as i64;
        let position = new_rate_pos * old_rate;
        let left_position = old_rate_pos * new_rate;
        let right_position = (old_rate_pos + 1) * new_rate;
        let ratio = right_position - position;
        let width = right_position - left_position;
        ((ratio * left + (width - ratio) * right) / width) as i16
    }

    fn find_period(samples: &[i16], pos_frames: usize, ch: usize, min_period: i32, max_period: i32) -> (i32, f64, f64) {
        let (mut best, mut worst, mut min_diff, mut max_diff) = (0i32, 255i32, 1i32, 0i32);
        let p = pos_frames * ch;
        for period in min_period..=max_period {
            let mut diff = 0i32;
            for i in 0..period as usize {
                diff = diff.wrapping_add((samples[p + i] as i32 - samples[p + period as usize + i] as i32).abs());
            }
            if diff.wrapping_mul(best) < min_diff.wrapping_mul(period) {
                min_diff = diff;
                best = period;
            }
            if diff.wrapping_mul(worst) > max_diff.wrapping_mul(period) {
                max_diff = diff;
                worst = period;
            }
        }
        (best, (min_diff / best.max(1)) as f64, (max_diff / worst.max(1)) as f64)
    }

    fn down_sample(input: &[i16], pos_frames: usize, skip: usize, frame_count: usize, ch: usize, out: &mut [i16]) {
        let per = ch * skip;
        let p = pos_frames * ch;
        for i in 0..frame_count {
            let mut v = 0i32;
            for j in 0..per {
                v += input[p + i * per + j] as i32;
            }
            out[i] = (v / per as i32) as i16;
        }
    }
}

impl Sample for f32 {
    fn overlap_add(frame_count: usize, ch: usize, out: &mut [f32], out_pos: usize, input: &[f32], down: usize, up: usize) {
        let n = frame_count as i32;
        for i in 0..ch {
            let (mut o, mut u, mut d) = (out_pos * ch + i, up * ch + i, down * ch + i);
            for t in 0..n {
                out[o] = (input[d] * (n - t) as f32 + input[u] * t as f32) / n as f32;
                o += ch;
                d += ch;
                u += ch;
            }
        }
    }

    fn interpolate(input: &[f32], pos: usize, ch: usize, old_rate_pos: i64, new_rate_pos: i64, old_rate: i64, new_rate: i64) -> f32 {
        let left = input[pos];
        let right = input[pos + ch];
        let position = new_rate_pos * old_rate;
        let left_position = old_rate_pos * new_rate;
        let right_position = (old_rate_pos + 1) * new_rate;
        let ratio = right_position - position;
        let width = right_position - left_position;
        (ratio as f32 * left + (width - ratio) as f32 * right) / width as f32
    }

    fn find_period(samples: &[f32], pos_frames: usize, ch: usize, min_period: i32, max_period: i32) -> (i32, f64, f64) {
        let (mut best, mut worst, mut min_diff, mut max_diff) = (0i32, 255i32, 1f64, 0f64);
        let p = pos_frames * ch;
        for period in min_period..=max_period {
            let mut diff = 0f64;
            for i in 0..period as usize {
                diff += (samples[p + i] - samples[p + period as usize + i]).abs() as f64;
            }
            if diff * (best as f64) < min_diff * period as f64 {
                min_diff = diff;
                best = period;
            }
            if diff * (worst as f64) > max_diff * period as f64 {
                max_diff = diff;
                worst = period;
            }
        }
        (best, min_diff / best.max(1) as f64, max_diff / worst.max(1) as f64)
    }

    fn down_sample(input: &[f32], pos_frames: usize, skip: usize, frame_count: usize, ch: usize, out: &mut [f32]) {
        let per = ch * skip;
        let p = pos_frames * ch;
        for i in 0..frame_count {
            let mut v = 0f64;
            for j in 0..per {
                v += input[p + i * per + j] as f64;
            }
            out[i] = (v / per as f64) as f32;
        }
    }
}

pub struct Sonic<T: Sample> {
    input_rate: i32,
    ch: usize,
    speed: f32,
    pitch: f32,
    rate: f32,
    min_period: i32,
    max_period: i32,
    max_required: usize,

    input: Vec<T>,
    output: Vec<T>,
    pitch_buf: Vec<T>,
    down: Vec<T>,
    input_frames: usize,
    output_frames: usize,
    pitch_frames: usize,
    old_rate_pos: i64,
    new_rate_pos: i64,
    remaining_copy: usize,
    prev_period: i32,
    accumulated_error: f64,
    min_diff: f64,
    max_diff: f64,
    prev_min_diff: f64,
}

impl<T: Sample> Sonic<T> {
    pub fn new(input_rate: u32, channels: usize, speed: f32, pitch: f32, output_rate: u32) -> Sonic<T> {
        let input_rate = input_rate as i32;
        let ch = channels.max(1);
        let min_period = input_rate / MAXIMUM_PITCH;
        let max_period = input_rate / MINIMUM_PITCH;
        let max_required = 2 * max_period as usize;
        Sonic {
            input_rate,
            ch,
            speed,
            pitch,
            rate: input_rate as f32 / output_rate as f32,
            min_period,
            max_period,
            max_required,
            input: vec![T::default(); max_required * ch],
            output: vec![T::default(); max_required * ch],
            pitch_buf: vec![T::default(); max_required * ch],
            down: vec![T::default(); max_required],
            input_frames: 0,
            output_frames: 0,
            pitch_frames: 0,
            old_rate_pos: 0,
            new_rate_pos: 0,
            remaining_copy: 0,
            prev_period: 0,
            accumulated_error: 0.0,
            min_diff: 0.0,
            max_diff: 0.0,
            prev_min_diff: 0.0,
        }
    }

    /// Input that is queued but not processed yet, in frames.
    pub fn pending_input_frames(&self) -> usize {
        self.input_frames
    }

    fn ensure(buf: &mut Vec<T>, ch: usize, frames: usize, additional: usize) {
        let cap = buf.len() / ch;
        if frames + additional > cap {
            buf.resize((3 * cap / 2 + additional) * ch, T::default());
        }
    }

    pub fn queue_input(&mut self, samples: &[T]) {
        let frames = samples.len() / self.ch;
        Self::ensure(&mut self.input, self.ch, self.input_frames, frames);
        let at = self.input_frames * self.ch;
        self.input[at..at + frames * self.ch].copy_from_slice(&samples[..frames * self.ch]);
        self.input_frames += frames;
        self.process_stream_input();
    }

    /// Output ready now, in frames.
    pub fn output_frames(&self) -> usize {
        self.output_frames
    }

    /// Moves up to `out.len()` samples of output into `out`; returns frames moved.
    pub fn get_output(&mut self, out: &mut [T]) -> usize {
        let n = (out.len() / self.ch).min(self.output_frames);
        out[..n * self.ch].copy_from_slice(&self.output[..n * self.ch]);
        self.output_frames -= n;
        self.output.copy_within(n * self.ch..(n + self.output_frames) * self.ch, 0);
        n
    }

    pub fn queue_end_of_stream(&mut self) {
        let remaining = self.input_frames;
        let s = self.speed as f64 / self.pitch as f64;
        let r = self.rate as f64 * self.pitch as f64;
        let adjusted = remaining as i64 - self.remaining_copy as i64;
        let expected = self.output_frames as i64
            + ((adjusted as f64 / s + self.remaining_copy as f64 + self.accumulated_error + self.pitch_frames as f64) / r + 0.5) as i64;
        self.accumulated_error = 0.0;
        Self::ensure(&mut self.input, self.ch, 0, remaining + 2 * self.max_required);
        let from = remaining * self.ch;
        for v in &mut self.input[from..from + 2 * self.max_required * self.ch] {
            *v = T::default();
        }
        self.input_frames += 2 * self.max_required;
        self.process_stream_input();
        if self.output_frames as i64 > expected {
            self.output_frames = expected.max(0) as usize;
        }
        self.input_frames = 0;
        self.remaining_copy = 0;
        self.pitch_frames = 0;
    }

    pub fn flush(&mut self) {
        self.input_frames = 0;
        self.output_frames = 0;
        self.pitch_frames = 0;
        self.old_rate_pos = 0;
        self.new_rate_pos = 0;
        self.remaining_copy = 0;
        self.prev_period = 0;
        self.accumulated_error = 0.0;
        self.prev_min_diff = 0.0;
        self.min_diff = 0.0;
        self.max_diff = 0.0;
    }

    fn copy_to_output(&mut self, pos: usize, frames: usize) {
        Self::ensure(&mut self.output, self.ch, self.output_frames, frames);
        let (ch, o) = (self.ch, self.output_frames * self.ch);
        self.output[o..o + frames * ch].copy_from_slice(&self.input[pos * ch..(pos + frames) * ch]);
        self.output_frames += frames;
    }

    fn copy_input_to_output(&mut self, pos: usize) -> usize {
        let n = self.max_required.min(self.remaining_copy);
        self.copy_to_output(pos, n);
        self.remaining_copy -= n;
        n
    }

    fn find_pitch_period(&mut self, pos: usize) -> i32 {
        let skip = if self.input_rate > AMDF_FREQUENCY { self.input_rate / AMDF_FREQUENCY } else { 1 };
        let mut period;
        if self.ch == 1 && skip == 1 {
            period = self.period_in(pos, self.min_period, self.max_period, false);
        } else {
            T::down_sample(&self.input, pos, skip as usize, self.max_required / skip as usize, self.ch, &mut self.down);
            period = self.period_in(0, self.min_period / skip, self.max_period / skip, true);
            if skip != 1 {
                period *= skip;
                let min_p = (period - skip * 4).max(self.min_period);
                let max_p = (period + skip * 4).min(self.max_period);
                if self.ch == 1 {
                    period = self.period_in(pos, min_p, max_p, false);
                } else {
                    T::down_sample(&self.input, pos, 1, self.max_required, self.ch, &mut self.down);
                    period = self.period_in(0, min_p, max_p, true);
                }
            }
        }
        let ret = if self.previous_period_better() { self.prev_period } else { period };
        self.prev_min_diff = self.min_diff;
        self.prev_period = period;
        ret
    }

    fn period_in(&mut self, pos: usize, min_p: i32, max_p: i32, downsampled: bool) -> i32 {
        let (p, mn, mx) = if downsampled { T::find_period(&self.down, pos, 1, min_p, max_p) } else { T::find_period(&self.input, pos, self.ch, min_p, max_p) };
        self.min_diff = mn;
        self.max_diff = mx;
        p
    }

    fn previous_period_better(&self) -> bool {
        if self.min_diff == 0.0 || self.prev_period == 0 {
            return false;
        }
        if self.max_diff > self.min_diff * 3.0 {
            return false;
        }
        if self.min_diff * 2.0 <= self.prev_min_diff * 3.0 {
            return false;
        }
        true
    }

    fn adjust_rate(&mut self, rate: f32, original_output_frames: usize) {
        if self.output_frames == original_output_frames {
            return;
        }
        let mut new_rate = (self.input_rate as f32 / rate) as i64;
        let mut old_rate = self.input_rate as i64;
        while new_rate != 0 && old_rate != 0 && new_rate % 2 == 0 && old_rate % 2 == 0 {
            new_rate /= 2;
            old_rate /= 2;
        }
        self.move_new_samples_to_pitch_buffer(original_output_frames);
        let ch = self.ch;
        let mut position = 0;
        while position + 1 < self.pitch_frames {
            while (self.old_rate_pos + 1) * new_rate > self.new_rate_pos * old_rate {
                Self::ensure(&mut self.output, ch, self.output_frames, 1);
                for i in 0..ch {
                    let v = T::interpolate(&self.pitch_buf, position * ch + i, ch, self.old_rate_pos, self.new_rate_pos, old_rate, new_rate);
                    self.output[self.output_frames * ch + i] = v;
                }
                self.new_rate_pos += 1;
                self.output_frames += 1;
            }
            self.old_rate_pos += 1;
            if self.old_rate_pos == old_rate {
                self.old_rate_pos = 0;
                self.new_rate_pos = 0;
            }
            position += 1;
        }
        self.remove_pitch_frames(self.pitch_frames.saturating_sub(1));
    }

    fn move_new_samples_to_pitch_buffer(&mut self, original_output_frames: usize) {
        let n = self.output_frames - original_output_frames;
        let ch = self.ch;
        Self::ensure(&mut self.pitch_buf, ch, self.pitch_frames, n);
        let (src, dst) = (original_output_frames * ch, self.pitch_frames * ch);
        self.pitch_buf[dst..dst + n * ch].copy_from_slice(&self.output[src..src + n * ch]);
        self.output_frames = original_output_frames;
        self.pitch_frames += n;
    }

    fn remove_pitch_frames(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let ch = self.ch;
        self.pitch_buf.copy_within(n * ch..self.pitch_frames * ch, 0);
        self.pitch_frames -= n;
    }

    fn skip_pitch_period(&mut self, pos: usize, speed: f64, period: i32) -> usize {
        let n;
        if speed >= 2.0 {
            let expected = period as f64 / (speed - 1.0) + self.accumulated_error;
            n = expected.round() as i64;
            self.accumulated_error = expected - n as f64;
        } else {
            n = period as i64;
            let expected = period as f64 * (2.0 - speed) / (speed - 1.0) + self.accumulated_error;
            self.remaining_copy = expected.round() as usize;
            self.accumulated_error = expected - self.remaining_copy as f64;
        }
        let n = n.max(0) as usize;
        Self::ensure(&mut self.output, self.ch, self.output_frames, n);
        T::overlap_add(n, self.ch, &mut self.output, self.output_frames, &self.input, pos, pos + period as usize);
        self.output_frames += n;
        n
    }

    fn insert_pitch_period(&mut self, pos: usize, speed: f64, period: i32) -> usize {
        let n;
        if speed < 0.5 {
            let expected = period as f64 * speed / (1.0 - speed) + self.accumulated_error;
            n = expected.round() as i64;
            self.accumulated_error = expected - n as f64;
        } else {
            n = period as i64;
            let expected = period as f64 * (2.0 * speed - 1.0) / (1.0 - speed) + self.accumulated_error;
            self.remaining_copy = expected.round() as usize;
            self.accumulated_error = expected - self.remaining_copy as f64;
        }
        let n = n.max(0) as usize;
        let (ch, p) = (self.ch, period as usize);
        Self::ensure(&mut self.output, ch, self.output_frames, p + n);
        let o = self.output_frames * ch;
        self.output[o..o + p * ch].copy_from_slice(&self.input[pos * ch..(pos + p) * ch]);
        T::overlap_add(n, ch, &mut self.output, self.output_frames + p, &self.input, pos + p, pos);
        self.output_frames += p + n;
        n
    }

    fn change_speed(&mut self, speed: f64) {
        if self.input_frames < self.max_required {
            return;
        }
        let frames = self.input_frames;
        let mut pos = 0;
        loop {
            if self.remaining_copy > 0 {
                pos += self.copy_input_to_output(pos);
            } else {
                let period = self.find_pitch_period(pos);
                if speed > 1.0 {
                    pos += period as usize + self.skip_pitch_period(pos, speed, period);
                } else {
                    pos += self.insert_pitch_period(pos, speed, period);
                }
            }
            if pos + self.max_required > frames {
                break;
            }
        }
        let ch = self.ch;
        let remaining = self.input_frames - pos;
        self.input.copy_within(pos * ch..(pos + remaining) * ch, 0);
        self.input_frames = remaining;
    }

    fn process_stream_input(&mut self) {
        let original = self.output_frames;
        let s = self.speed as f64 / self.pitch as f64;
        let r = self.rate * self.pitch;
        if s > MINIMUM_SPEEDUP_RATE as f64 || s < MINIMUM_SLOWDOWN_RATE as f64 {
            self.change_speed(s);
        } else {
            let n = self.input_frames;
            self.copy_to_output(0, n);
            self.input_frames = 0;
        }
        if r != 1.0 {
            self.adjust_rate(r, original);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, secs: f64, hz: f64) -> Vec<i16> {
        (0..(rate as f64 * secs) as usize)
            .flat_map(|i| {
                let v = ((i as f64 / rate as f64 * hz * std::f64::consts::TAU).sin() * 12000.0) as i16;
                [v, v]
            })
            .collect()
    }

    fn run(s: &mut Sonic<i16>, x: &[i16]) -> Vec<i16> {
        let mut out = Vec::new();
        let mut buf = vec![0i16; 1 << 16];
        for c in x.chunks(4096) {
            s.queue_input(c);
            let n = s.get_output(&mut buf);
            out.extend_from_slice(&buf[..n * 2]);
        }
        s.queue_end_of_stream();
        loop {
            let n = s.get_output(&mut buf);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n * 2]);
        }
        out
    }

    fn hz(x: &[i16], rate: u32) -> f64 {
        let l: Vec<i16> = x.chunks_exact(2).map(|f| f[0]).collect();
        let m = &l[l.len() / 4..l.len() * 3 / 4];
        m.windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count() as f64 / (m.len() as f64 / rate as f64)
    }

    #[test]
    fn speed_keeps_pitch_and_shortens() {
        let x = sine(44100, 4.0, 220.0);
        let y = run(&mut Sonic::new(44100, 2, 1.25, 1.0, 44100), &x);
        let secs = y.len() as f64 / 2.0 / 44100.0;
        assert!((secs - 3.2).abs() < 0.01, "{secs}");
        assert!((hz(&y, 44100) - 220.0).abs() < 3.0, "{}", hz(&y, 44100));
    }

    #[test]
    fn pitch_keeps_length_and_moves_up() {
        let x = sine(44100, 4.0, 200.0);
        let y = run(&mut Sonic::new(44100, 2, 1.0, 1.1, 44100), &x);
        let secs = y.len() as f64 / 2.0 / 44100.0;
        assert!((secs - 4.0).abs() < 0.01, "{secs}");
        assert!((hz(&y, 44100) - 220.0).abs() < 3.0, "{}", hz(&y, 44100));
    }

    #[test]
    fn unchanged_is_a_copy() {
        let x = sine(48000, 1.0, 330.0);
        assert_eq!(run(&mut Sonic::new(48000, 2, 1.0, 1.0, 48000), &x), x);
    }
}
