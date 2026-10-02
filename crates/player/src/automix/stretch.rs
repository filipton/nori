//! Tempo change of the incoming track during a beat-matched transition: hold `ratio`, ramp back to 1, then hand
//! over to the plain signal. Signalsmith Stretch keeps pitch; varispeed (cubic Hermite) moves it and costs little.
//!
//! Output frame `j` carries the input at `∫ ratio` (start latency dropped), so a beat grid on the input holds on
//! the output. Once `bypassed()`, call `drain` to collect what is inside, then pass the track through; the
//! hand-over is sample-exact. `process` never allocates.

/// Input frames per engine call; the tempo is updated this often (about 6 ms).
pub const BLOCK: usize = 256;
/// Ratio bounds; the minimum bounds the output of one block.
const MIN_RATIO: f64 = 0.5;
const MAX_RATIO: f64 = 2.0;
const MAX_OUT: usize = (BLOCK as f64 / MIN_RATIO) as usize + 8;
/// Crossfade from the stretched to the plain signal, frames.
const XFADE: usize = 1024;
/// Signalsmith Stretch's analysis hop in the preset used, seconds (`presetCheaper`: 40 ms).
const SIGNALSMITH_HOP_S: f64 = 0.04;
/// The smallest ratio offset Signalsmith Stretch follows cleanly, in frames per hop: closer to 1 it loses up to
/// 2.6 dB. Such ratios are snapped to 1 or to this offset (0.07 % of tempo at 44.1 kHz).
const SIGNALSMITH_CLEAR_FRAMES: f64 = 2.5;
pub const MAX_CHANNELS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    /// Ratio 1 and no ramp: a plain copy.
    Direct,
    Active,
    Fading(usize),
    Bypass,
}

struct Vari {
    /// Interleaved frames; `pos` is fractional within it.
    buf: Vec<f32>,
    len: usize,
    pos: f64,
    /// Fractional part dropped at the start of the hand-over crossfade.
    phase: f64,
}

enum Engine {
    Signalsmith(signalsmith_stretch::Stretch),
    Vari(Vari),
}

pub struct Stretcher {
    ch: usize,
    engine: Engine,
    ratio0: f64,
    hold: u64,
    ramp: u64,
    out_pos: u64,
    /// Frames the engine has produced, pre-roll included.
    synth: u64,
    drop_total: u64,
    /// How far ahead of what it returns the engine synthesises (its output latency), frames.
    lead: u64,
    frac: f64,
    drop: usize,
    state: State,
    /// Output produced by the last engine call and not yet handed out.
    pend: Vec<f32>,
    pend_read: usize,
    pend_len: usize,
    /// Plain input delayed by the stretcher's latency, for the hand-over (Signalsmith only).
    raw: Vec<f32>,
    raw_pos: usize,
    /// Frames of `raw` still to hand out by `drain`.
    raw_left: usize,
    latency: usize,
    stage: Vec<f32>,
    /// Signalsmith Stretch's analysis hop, frames; 0 for varispeed.
    hop: f64,
    /// Frames handed out since `configure`, and the song time they stand for (in input frames) not yet
    /// taken by [`Stretcher::take_content`].
    given: u64,
    content: f64,
}

#[inline]
fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

impl Vari {
    fn new(ch: usize) -> Self {
        Vari { buf: vec![0.0; (BLOCK * 2 + 8) * ch], len: 0, pos: 1.0, phase: 0.0 }
    }

    /// Appends `input`, writes as many frames at `ratio` as the buffer allows. `fade`: (done, total) of the hand-over.
    fn run(&mut self, ch: usize, input: &[f32], ratio: f64, out: &mut [f32], fade: Option<(usize, usize)>) -> usize {
        let n = input.len() / ch;
        if self.len == 0 {
            // One frame of history so the first output is exactly frame 0.
            self.buf[..ch].fill(0.0);
            self.len = 1;
        }
        self.buf[self.len * ch..(self.len + n) * ch].copy_from_slice(input);
        self.len += n;
        let mut made = 0;
        let max = out.len() / ch;
        while made < max && (self.pos.floor() as usize) + 2 < self.len {
            let i = self.pos.floor() as usize;
            let t = (self.pos - i as f64) as f32;
            let at = |k: usize, c: usize| self.buf[k * ch + c];
            for c in 0..ch {
                let v = hermite(at(i - 1, c), at(i, c), at(i + 1, c), at(i + 2, c), t);
                out[made * ch + c] = match fade {
                    Some((done, total)) => {
                        // The plain signal: the same stream `phase` frames earlier, on whole frames.
                        let exact = self.pos - self.phase;
                        let e = exact.floor() as usize;
                        let te = (exact - e as f64) as f32;
                        let plain = hermite(at(e.max(1) - 1, c), at(e, c), at(e + 1, c), at(e + 2, c), te);
                        let w = ((done + made) as f32 / total as f32).min(1.0);
                        v * (1.0 - w) + plain * w
                    }
                    None => v,
                };
            }
            made += 1;
            self.pos += ratio;
        }
        // Keep one frame of history before the read position.
        let keep_from = (self.pos.floor() as usize).saturating_sub(1);
        if keep_from > 0 {
            self.buf.copy_within(keep_from * ch..self.len * ch, 0);
            self.len -= keep_from;
            self.pos -= keep_from as f64;
        }
        made
    }
}

impl Stretcher {
    /// `keep_pitch` picks Signalsmith Stretch; otherwise varispeed.
    pub fn new(rate: u32, channels: usize, keep_pitch: bool) -> Self {
        let ch = channels.clamp(1, MAX_CHANNELS);
        let engine = if keep_pitch {
            Engine::Signalsmith(signalsmith_stretch::Stretch::preset_cheaper(ch as u32, rate.max(8000)))
        } else {
            Engine::Vari(Vari::new(ch))
        };
        let latency = match &engine {
            Engine::Signalsmith(s) => s.input_latency() + s.output_latency(),
            Engine::Vari(_) => 0,
        };
        // As the library computes it: `(int)(sampleRate * 0.04)`.
        let hop = if keep_pitch { (rate.max(8000) as f64 * SIGNALSMITH_HOP_S).floor() } else { 0.0 };
        Stretcher {
            ch,
            engine,
            ratio0: 1.0,
            hold: 0,
            ramp: 0,
            out_pos: 0,
            synth: 0,
            drop_total: 0,
            lead: 0,
            frac: 0.0,
            drop: 0,
            state: State::Direct,
            pend: vec![0.0; MAX_OUT * ch],
            pend_read: 0,
            pend_len: 0,
            raw: vec![0.0; latency.max(1) * ch],
            raw_pos: 0,
            raw_left: 0,
            latency,
            stage: vec![0.0; MAX_OUT * ch],
            hop,
            given: 0,
            content: 0.0,
        }
    }

    /// Starts a schedule from the next frame: `ratio` (playback speed, >1 faster) for `hold` output frames, then a
    /// linear ramp to 1 over `ramp` output frames, then the hand-over. Resets the engine.
    pub fn configure(&mut self, ratio: f64, hold: u64, ramp: u64) {
        let ratio = if ratio.is_finite() { ratio.clamp(MIN_RATIO, MAX_RATIO) } else { 1.0 };
        (self.ratio0, self.hold, self.ramp, self.out_pos, self.frac) = (ratio, hold, ramp, 0, 0.0);
        (self.pend_read, self.pend_len, self.raw_pos) = (0, 0, 0);
        (self.given, self.content) = (0, 0.0);
        self.raw.fill(0.0);
        if (ratio - 1.0).abs() < 1e-6 && ramp == 0 {
            self.state = State::Direct;
            self.drop = 0;
            return;
        }
        self.state = State::Active;
        match &mut self.engine {
            Engine::Signalsmith(s) => {
                s.reset();
                // Output frame j holds input (j - out_latency) * ratio - in_latency: drop the difference once.
                self.drop = (s.input_latency() as f64 / ratio + s.output_latency() as f64).round() as usize;
                self.lead = s.output_latency() as u64;
            }
            Engine::Vari(v) => {
                *v = Vari::new(self.ch);
                (self.drop, self.lead) = (0, 0);
            }
        }
        (self.synth, self.drop_total) = (0, self.drop as u64);
    }

    pub fn latency_frames(&self) -> usize {
        self.latency
    }

    pub fn bypassed(&self) -> bool {
        matches!(self.state, State::Bypass | State::Direct)
    }

    /// Song time handed out since last asked, in input frames (`∫ ratio`, not the output frame count).
    pub fn take_content(&mut self) -> f64 {
        std::mem::take(&mut self.content)
    }

    /// Adds the song time of `n` more output frames, integrating the schedule in 64-frame steps.
    fn handed(&mut self, n: usize) {
        let mut k = self.given;
        let end = k + n as u64;
        while k < end {
            let step = (end - k).min(64);
            self.content += self.clear(self.schedule(k + step / 2)) * step as f64;
            k += step;
        }
        self.given = end;
    }

    /// The ratio the schedule gives output frame `at`.
    fn schedule(&self, at: u64) -> f64 {
        if at < self.hold {
            self.ratio0
        } else if at < self.hold + self.ramp {
            let x = (at - self.hold) as f64 / self.ramp as f64;
            self.ratio0 + (1.0 - self.ratio0) * x
        } else {
            1.0
        }
    }

    /// The ratio for frames synthesised now, indexed by the output position they will have once they leave the
    /// stretcher (`output_latency` later).
    fn ratio_now(&self) -> f64 {
        let at = (self.synth + self.lead).saturating_sub(self.drop_total);
        self.clear(self.schedule(at))
    }

    /// `r` snapped away from the band around 1 the engine cannot follow ([`SIGNALSMITH_CLEAR_FRAMES`]).
    fn clear(&self, r: f64) -> f64 {
        let off = (r - 1.0) * self.hop;
        if off == 0.0 || off.abs() >= SIGNALSMITH_CLEAR_FRAMES {
            r
        } else if off.abs() < SIGNALSMITH_CLEAR_FRAMES / 2.0 {
            1.0
        } else {
            1.0 + off.signum() * SIGNALSMITH_CLEAR_FRAMES / self.hop
        }
    }

    /// One block of at most `BLOCK` input frames into `pend`.
    fn run_block(&mut self, input: &[f32]) {
        let ch = self.ch;
        let n = input.len() / ch;
        let r = self.ratio_now();
        if r == 1.0 && self.state == State::Active && self.out_pos >= self.hold + self.ramp + (self.latency + BLOCK) as u64 {
            self.state = State::Fading(0);
            if let Engine::Vari(v) = &mut self.engine {
                v.phase = v.pos - v.pos.round();
            }
        }
        let produced = match self.state {
            State::Direct => {
                self.pend[..n * ch].copy_from_slice(input);
                n
            }
            State::Bypass => match &mut self.engine {
                Engine::Signalsmith(_) => {
                    self.raw_pos = delay(&mut self.raw, self.raw_pos, ch, input, &mut self.pend);
                    n
                }
                Engine::Vari(v) => v.run(ch, input, 1.0, &mut self.pend, None),
            },
            State::Active | State::Fading(_) => {
                let fade = if let State::Fading(done) = self.state { Some((done, XFADE)) } else { None };
                let made = match &mut self.engine {
                    Engine::Signalsmith(s) => {
                        let want = n as f64 / r + self.frac;
                        let m = (want.floor() as usize).min(MAX_OUT);
                        self.frac = want - m as f64;
                        s.process(input, &mut self.pend[..m * ch]);
                        // The plain signal, delayed to line up with the stretcher at ratio 1.
                        self.raw_pos = delay(&mut self.raw, self.raw_pos, ch, input, &mut self.stage);
                        if let Some((done, total)) = fade {
                            for f in 0..m.min(n) {
                                let w = ((done + f) as f32 / total as f32).min(1.0);
                                for c in 0..ch {
                                    let i = f * ch + c;
                                    self.pend[i] = self.pend[i] * (1.0 - w) + self.stage[i] * w;
                                }
                            }
                        }
                        m
                    }
                    Engine::Vari(v) => v.run(ch, input, r, &mut self.pend, fade),
                };
                if let State::Fading(done) = self.state {
                    let done = done + made;
                    self.state = if done >= XFADE { State::Bypass } else { State::Fading(done) };
                    if self.state == State::Bypass {
                        self.raw_left = self.latency;
                        if let Engine::Vari(v) = &mut self.engine {
                            v.pos -= v.phase;
                            v.phase = 0.0;
                        }
                    }
                }
                made
            }
        };
        self.synth += produced as u64;
        // The first `drop` frames are the stretcher's pre-roll.
        let skip = self.drop.min(produced);
        self.drop -= skip;
        self.pend_read = skip;
        self.pend_len = produced;
        self.out_pos += (produced - skip) as u64;
    }

    /// Interleaved f32. Returns (input frames consumed, output frames written).
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> (usize, usize) {
        let ch = self.ch;
        let (inf, cap) = (input.len() / ch, output.len() / ch);
        let (mut used, mut made) = (0, 0);
        loop {
            let take = (self.pend_len - self.pend_read).min(cap - made);
            output[made * ch..(made + take) * ch].copy_from_slice(&self.pend[self.pend_read * ch..(self.pend_read + take) * ch]);
            self.pend_read += take;
            made += take;
            if made == cap || used == inf {
                break;
            }
            let n = BLOCK.min(inf - used);
            self.run_block(&input[used * ch..(used + n) * ch]);
            used += n;
        }
        self.handed(made);
        (used, made)
    }

    /// Frames still inside at the end of the stream or after `bypassed()`; call again while it fills `output`.
    pub fn drain(&mut self, output: &mut [f32]) -> usize {
        let made = self.drain_out(output);
        self.handed(made);
        made
    }

    fn drain_out(&mut self, output: &mut [f32]) -> usize {
        let ch = self.ch;
        let cap = output.len() / ch;
        let mut made = 0;
        let pending = (self.pend_len - self.pend_read).min(cap);
        output[..pending * ch].copy_from_slice(&self.pend[self.pend_read * ch..(self.pend_read + pending) * ch]);
        self.pend_read += pending;
        made += pending;
        if made == cap || self.pend_read < self.pend_len {
            return made;
        }
        match (&mut self.engine, self.state) {
            (_, State::Direct) => {}
            (Engine::Signalsmith(_), State::Bypass) => {
                // The delay line, oldest first; it may take several calls.
                let lat = self.latency;
                while made < cap && self.raw_left > 0 {
                    let slot = self.raw_pos * ch;
                    output[made * ch..(made + 1) * ch].copy_from_slice(&self.raw[slot..slot + ch]);
                    self.raw_pos = if self.raw_pos + 1 == lat { 0 } else { self.raw_pos + 1 };
                    self.raw_left -= 1;
                    made += 1;
                }
                if self.raw_left == 0 {
                    self.state = State::Direct;
                }
            }
            (Engine::Signalsmith(s), _) => {
                // The stream ended mid-stretch: flush what the stretcher holds (the input latency is lost).
                let m = s.output_latency().min(cap - made);
                s.flush(&mut output[made * ch..(made + m) * ch]);
                let skip = self.drop.min(m);
                output.copy_within((made + skip) * ch..(made + m) * ch, made * ch);
                made += m - skip;
                self.state = State::Direct;
            }
            (Engine::Vari(v), _) => {
                // What is left after the read position, at whole frames.
                let from = v.pos.round() as usize;
                let n = v.len.saturating_sub(from).min(cap - made);
                output[made * ch..(made + n) * ch].copy_from_slice(&v.buf[from * ch..(from + n) * ch]);
                made += n;
                *v = Vari::new(ch);
                self.state = State::Direct;
            }
        }
        (self.pend_read, self.pend_len) = (0, 0);
        made
    }
}

/// Pushes `input` through the delay line `raw` (read position `pos`), writing what comes out to `out`; returns the
/// new position.
fn delay(raw: &mut [f32], mut pos: usize, ch: usize, input: &[f32], out: &mut [f32]) -> usize {
    let lat = raw.len() / ch;
    for (i, o) in input.chunks_exact(ch).zip(out.chunks_exact_mut(ch)) {
        let slot = &mut raw[pos * ch..(pos + 1) * ch];
        o.copy_from_slice(slot);
        slot.copy_from_slice(i);
        pos = if pos + 1 == lat { 0 } else { pos + 1 };
    }
    pos
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Clicks every `every` frames, stereo.
    fn clicks(frames: usize, every: usize) -> Vec<f32> {
        let mut x = vec![0f32; frames * 2];
        for f in (every / 2..frames).step_by(every) {
            for k in 0..8 {
                let v = if k % 2 == 0 { 0.8 } else { -0.8 } * (1.0 - k as f32 / 8.0);
                if f + k < frames {
                    x[(f + k) * 2] = v;
                    x[(f + k) * 2 + 1] = v;
                }
            }
        }
        x
    }

    fn run_all(s: &mut Stretcher, x: &[f32], chunk: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let mut buf = vec![0f32; 4096 * 2];
        let mut i = 0;
        while i < x.len() {
            let end = (i + chunk * 2).min(x.len());
            let mut pos = i;
            while pos < end {
                let (u, m) = s.process(&x[pos..end], &mut buf);
                out.extend_from_slice(&buf[..m * 2]);
                pos += u * 2;
            }
            i = end;
        }
        loop {
            let m = s.drain(&mut buf);
            out.extend_from_slice(&buf[..m * 2]);
            if m * 2 < buf.len() {
                break;
            }
        }
        out
    }

    /// Frame index of each energy peak, one per `every`-frame window.
    fn peaks(y: &[f32], every: usize) -> Vec<usize> {
        let frames = y.len() / 2;
        let env: Vec<f32> = (0..frames).map(|f| y[f * 2].abs()).collect();
        let mut out = Vec::new();
        let mut f = 0;
        while f + every <= frames {
            let (i, v) = env[f..f + every].iter().enumerate().fold((0, 0f32), |m, (i, v)| if *v > m.1 { (i, *v) } else { m });
            if v > 0.1 {
                out.push(f + i);
            }
            f += every;
        }
        out
    }

    /// Noise through a stretch of `ratio` held for `hold` s and ramped back over `ramp` s, then handed over: its
    /// level every 100 ms against the noise's own, dB.
    fn noise_levels(keep: bool, ratio: f64, hold: f64, ramp: f64) -> Vec<f64> {
        let rate = 44100;
        let mut s = Stretcher::new(rate, 2, keep);
        s.configure(ratio, (hold * rate as f64) as u64, (ramp * rate as f64) as u64);
        let mut r = crate::automix::synth::Rng(5);
        let x: Vec<f32> = (0..((hold + ramp + 3.0) * rate as f64) as usize * 2).map(|_| (r.next() * 0.3) as f32).collect();
        let mut out = Vec::new();
        let mut buf = vec![0f32; 2048 * 2];
        let mut pos = 0;
        while pos < x.len() {
            let end = (pos + 512 * 2).min(x.len());
            if s.bypassed() {
                out.extend_from_slice(&x[pos..end]);
                pos = end;
                continue;
            }
            let (u, m) = s.process(&x[pos..end], &mut buf);
            out.extend_from_slice(&buf[..m * 2]);
            pos += u * 2;
            if s.bypassed() {
                loop {
                    let m = s.drain(&mut buf);
                    out.extend_from_slice(&buf[..m * 2]);
                    if m * 2 < buf.len() {
                        break;
                    }
                }
            }
        }
        let power = |v: &[f32]| v.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / v.len() as f64;
        let own = power(&x);
        out.chunks(4410 * 2).filter(|c| c.len() == 4410 * 2).map(|c| 10.0 * (power(c) / own).log10()).collect()
    }

    #[test]
    fn near_unity_ratio_keeps_level() {
        // Regression: Signalsmith Stretch lost up to 2.6 dB near ratio 1, heard as a step at the hand-over.
        for ratio in [0.9995, 1.0005, 0.9992, 0.9998, 0.9990, 0.9986, 0.976] {
            let l = noise_levels(true, ratio, 6.0, 0.0);
            let held = &l[5..55];
            let (lo, mean) = (held.iter().cloned().fold(f64::MAX, f64::min), held.iter().sum::<f64>() / held.len() as f64);
            assert!(mean > -0.35 && lo > -0.6, "ratio {ratio}: {mean:.2} dB on average, down to {lo:.2}");
        }
        // Ramped back from a real stretch: no dip on the way and no step at the hand-over.
        let l = noise_levels(true, 0.976, 4.0, 4.0);
        let step = l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        let lo = l.iter().cloned().fold(f64::MAX, f64::min);
        assert!(step < 0.8 && lo > -0.85, "a ramp back to 1 moves {step:.2} dB in 100 ms, down to {lo:.2} dB: {l:.2?}");
    }

    #[test]
    fn direct_mode_is_a_copy() {
        for keep in [true, false] {
            let mut s = Stretcher::new(44100, 2, keep);
            s.configure(1.0, 1000, 0);
            let x = clicks(44100, 11025);
            let y = run_all(&mut s, &x, 777);
            assert_eq!(x, y);
            assert!(s.bypassed());
        }
    }

    /// After hold + ramp the output joins the plain input without a jump, shorter by what the ramp gained.
    #[test]
    fn ramp_ends_in_seamless_bypass() {
        for keep in [true, false] {
            let mut s = Stretcher::new(44100, 2, keep);
            s.configure(1.03, 44100, 44100);
            let x: Vec<f32> = (0..44100 * 8).flat_map(|i| {
                let v = (0.3 * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / 44100.0).sin()) as f32;
                [v, v]
            }).collect();
            let mut out = Vec::new();
            let mut buf = vec![0f32; 2048 * 2];
            let mut pos = 0;
            while pos < x.len() && !s.bypassed() {
                let end = (pos + 512 * 2).min(x.len());
                let (u, m) = s.process(&x[pos..end], &mut buf);
                out.extend_from_slice(&buf[..m * 2]);
                pos += u * 2;
            }
            assert!(s.bypassed(), "keep_pitch {keep}: never handed over");
            loop {
                let m = s.drain(&mut buf);
                out.extend_from_slice(&buf[..m * 2]);
                if m * 2 < buf.len() {
                    break;
                }
            }
            out.extend_from_slice(&x[pos..]);
            let frames = out.len() / 2;
            // Consumed = produced + ∫(ratio - 1): 1 s at 3 % plus a 1 s ramp at 1.5 % on average = 1985 frames.
            let gained = x.len() / 2 - frames;
            assert!((gained as i64 - 1985).abs() < 40, "keep_pitch {keep}: gained {gained}");
            let join = (out.len() - (x.len() - pos)) / 2;
            let tail: Vec<f32> = out[(join - 200) * 2..(join + 200) * 2].iter().step_by(2).copied().collect();
            let jump = tail.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
            // A 220 Hz sine at 0.3 moves at most 0.3 * 2π * 220 / 44100 = 0.0094 per sample.
            assert!(jump < 0.02, "keep_pitch {keep}: discontinuity {jump} at the hand-over");
        }
    }

    /// `take_content` sums to `∫ ratio` during the hold and to the input consumed by the hand-over.
    #[test]
    fn content_handed_out_matches_input() {
        for keep in [true, false] {
            let (rate, ratio) = (48_000usize, 1.071);
            let mut s = Stretcher::new(rate as u32, 2, keep);
            s.configure(ratio, 2 * rate as u64, rate as u64);
            let x: Vec<f32> = (0..rate * 8).flat_map(|i| {
                let v = (0.3 * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / rate as f64).sin()) as f32;
                [v, v]
            }).collect();
            let mut buf = vec![0f32; 2048 * 2];
            let (mut pos, mut made, mut content) = (0, 0usize, 0.0);
            let mut checked = false;
            while pos < x.len() && !s.bypassed() {
                let end = (pos + 512 * 2).min(x.len());
                let (u, m) = s.process(&x[pos..end], &mut buf);
                made += m;
                content += s.take_content();
                pos += u * 2;
                if !checked && made >= rate {
                    checked = true;
                    assert!((content - made as f64 * ratio).abs() < 2.0, "keep_pitch {keep}: {content} for {made} frames");
                }
            }
            loop {
                let m = s.drain(&mut buf);
                content += s.take_content();
                if m * 2 < buf.len() {
                    break;
                }
            }
            let taken = (pos / 2) as f64;
            assert!((content - taken).abs() < 24.0, "keep_pitch {keep}: {content:.1} frames of the song handed out, {taken} taken in");
        }
    }

    #[test]
    fn timeline_is_sample_accurate() {
        // Impulses come out within 12 frames of input / ratio (1.0015: the closest to 1 Signalsmith runs at).
        for (keep, ratio) in [(true, 1.0015f64), (true, 1.03), (true, 1.06), (true, 0.95), (false, 1.02), (false, 0.98)] {
            let mut s = Stretcher::new(44100, 2, keep);
            s.configure(ratio, 1 << 40, 0);
            let every = 11025;
            let mut x = vec![0f32; 44100 * 4 * 2];
            for f in (every / 2..44100 * 4).step_by(every) {
                x[f * 2] = 1.0;
                x[f * 2 + 1] = 1.0;
            }
            let mut out = Vec::new();
            let mut buf = vec![0f32; 8192];
            let mut pos = 0;
            while pos < x.len() {
                let (u, m) = s.process(&x[pos..(pos + 2000).min(x.len())], &mut buf);
                out.extend_from_slice(&buf[..m * 2]);
                pos += u * 2;
            }
            let env: Vec<f32> = out.iter().step_by(2).map(|v| v.abs()).collect();
            let mut errs = Vec::new();
            for k in 0..14 {
                let want = (every / 2 + k * every) as f64 / ratio;
                let w = want as usize;
                if w + 600 > env.len() || w < 600 {
                    continue;
                }
                let (i, _) = env[w - 600..w + 600].iter().enumerate().fold((0, 0f32), |m, (i, v)| if *v > m.1 { (i, *v) } else { m });
                errs.push((w - 600 + i) as f64 - want);
            }
            assert!(errs.len() >= 12, "{keep} {ratio}: {errs:?}");
            assert!(errs.iter().all(|e| e.abs() <= 12.0), "keep_pitch {keep} ratio {ratio}: {errs:?}");
        }

        // At a constant ratio clicks come out at input / ratio from the first one, and the length follows.
        for keep in [true, false] {
            for ratio in [1.04, 0.97] {
                let mut s = Stretcher::new(44100, 2, keep);
                s.configure(ratio, 10 * 44100, 0);
                let every = 22050;
                let x = clicks(44100 * 6, every);
                let y = run_all(&mut s, &x, 1000);
                let got = peaks(&y, (every as f64 / ratio) as usize);
                let want: Vec<f64> = (every / 2..44100 * 6).step_by(every).map(|f| f as f64 / ratio).collect();
                assert!(got.len() >= want.len() - 1, "{keep} {ratio}: {got:?}");
                for (g, w) in got.iter().zip(&want) {
                    assert!((*g as f64 - w).abs() < 0.003 * 44100.0, "keep_pitch {keep} ratio {ratio}: click at {g}, expected {w}");
                }
                let expect_len = x.len() as f64 / ratio;
                assert!((y.len() as f64 - expect_len).abs() < 0.01 * expect_len, "{} vs {expect_len}", y.len());
            }
        }
    }

    #[test]
    fn bad_ratios_are_clamped() {
        let mut s = Stretcher::new(48000, 2, false);
        s.configure(f64::NAN, 100, 0);
        assert!(s.bypassed());
        s.configure(100.0, 1000, 0);
        assert_eq!(s.ratio0, MAX_RATIO);
    }

}
