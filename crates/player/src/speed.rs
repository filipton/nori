//! Speed and pitch processor: [`crate::sonic::Sonic`] plus media3 `SonicAudioProcessor`'s byte
//! counts, which map played output back to media time.

use crate::pcm::Encoding;
use crate::sonic::Sonic;

/// Below this much output the measured ratio is too noisy; the nominal speed is used.
#[cfg(any(test, feature = "synth"))]
const MIN_BYTES_FOR_DURATION_SCALING: u64 = 1024;
const CLOSE_THRESHOLD: f32 = 0.0001;

/// Whether speed or pitch differs from 1 beyond float noise (otherwise the stage is bypassed).
pub fn speed_active(speed: f32, pitch: f32) -> bool {
    (speed - 1.0).abs() >= CLOSE_THRESHOLD || (pitch - 1.0).abs() >= CLOSE_THRESHOLD
}

/// Media time covered by `playout_us` of output at the nominal `speed`.
pub fn nominal_media_us(speed: f32, playout_us: i64) -> i64 {
    (speed as f64 * playout_us as f64) as i64
}

/// Output time `media_us` of the song takes at the nominal `speed`.
#[cfg(any(test, feature = "synth"))]
pub fn nominal_playout_us(speed: f32, media_us: i64) -> i64 {
    (media_us as f64 / speed as f64) as i64
}

#[derive(Clone)]
enum Engine {
    Short(Sonic<i16>),
    Float(Sonic<f32>),
}

pub struct SpeedPitch {
    rate: u32,
    ch: usize,
    enc: Encoding,
    speed: f32,
    pitch: f32,
    engine: Engine,
    input_bytes: u64,
    output_bytes: u64,
    staged_i16: Vec<i16>,
    staged_f32: Vec<f32>,
}

/// `clone_from` keeps the buffers' memory (the sink copies the chain's state without allocating).
impl Clone for SpeedPitch {
    fn clone(&self) -> Self {
        SpeedPitch { engine: self.engine.clone(), staged_i16: Vec::new(), staged_f32: Vec::new(), ..*self }
    }

    fn clone_from(&mut self, o: &Self) {
        match (&mut self.engine, &o.engine) {
            (Engine::Short(a), Engine::Short(b)) => a.clone_from(b),
            (Engine::Float(a), Engine::Float(b)) => a.clone_from(b),
            (a, b) => *a = b.clone(),
        }
        let SpeedPitch { rate, ch, enc, speed, pitch, engine: _, input_bytes, output_bytes, staged_i16: _, staged_f32: _ } = *o;
        (self.rate, self.ch, self.enc, self.speed, self.pitch, self.input_bytes, self.output_bytes) = (rate, ch, enc, speed, pitch, input_bytes, output_bytes);
    }
}

/// Appends `samples` to `out` as little-endian bytes, in one pass.
fn append<const W: usize, T: Copy>(out: &mut Vec<u8>, samples: &[T], bytes: impl Fn(T) -> [u8; W]) {
    let at = out.len();
    out.resize(at + samples.len() * W, 0);
    out[at..].as_chunks_mut::<W>().0.iter_mut().zip(samples).for_each(|(o, &v)| *o = bytes(v));
}

impl SpeedPitch {
    pub fn new(rate: u32, channels: usize, enc: Encoding) -> SpeedPitch {
        let ch = channels.clamp(1, 8);
        let mut p = SpeedPitch {
            rate,
            ch,
            enc,
            speed: 1.0,
            pitch: 1.0,
            engine: Engine::Short(Sonic::new(rate, ch, 1.0, 1.0, rate)),
            input_bytes: 0,
            output_bytes: 0,
            staged_i16: Vec::new(),
            staged_f32: Vec::new(),
        };
        p.flush();
        p
    }

    /// From the next input on; what is queued plays on. Invalid values mean 1.
    pub fn set(&mut self, speed: f32, pitch: f32) {
        self.speed = if speed > 0.0 && speed.is_finite() { speed } else { 1.0 };
        self.pitch = if pitch > 0.0 && pitch.is_finite() { pitch } else { 1.0 };
        match &mut self.engine {
            Engine::Short(s) => s.set(self.speed, self.pitch),
            Engine::Float(s) => s.set(self.speed, self.pitch),
        }
    }

    pub fn active(&self) -> bool {
        speed_active(self.speed, self.pitch)
    }

    /// Starts a new stream (seek or new parameters) at the current settings.
    pub fn flush(&mut self) {
        self.engine = match self.enc {
            Encoding::Pcm16 => Engine::Short(Sonic::new(self.rate, self.ch, self.speed, self.pitch, self.rate)),
            Encoding::Float => Engine::Float(Sonic::new(self.rate, self.ch, self.speed, self.pitch, self.rate)),
        };
        self.input_bytes = 0;
        self.output_bytes = 0;
    }

    /// Takes interleaved bytes in the configured encoding; appends ready output to `out`.
    pub fn process(&mut self, input: &[u8], out: &mut Vec<u8>) {
        self.input_bytes += input.len() as u64;
        match &mut self.engine {
            Engine::Short(s) => {
                self.staged_i16.clear();
                self.staged_i16.extend(input.as_chunks::<2>().0.iter().map(|&c| i16::from_le_bytes(c)));
                s.queue_input(&self.staged_i16);
            }
            Engine::Float(s) => {
                self.staged_f32.clear();
                self.staged_f32.extend(input.as_chunks::<4>().0.iter().map(|&c| f32::from_le_bytes(c)));
                s.queue_input(&self.staged_f32);
            }
        }
        self.drain(out);
    }

    fn drain(&mut self, out: &mut Vec<u8>) {
        let before = out.len();
        match &mut self.engine {
            Engine::Short(s) => {
                let n = s.output_frames() * self.ch;
                self.staged_i16.resize(n, 0);
                let got = s.get_output(&mut self.staged_i16) * self.ch;
                append(out, &self.staged_i16[..got], i16::to_le_bytes);
            }
            Engine::Float(s) => {
                let n = s.output_frames() * self.ch;
                self.staged_f32.resize(n, 0.0);
                let got = s.get_output(&mut self.staged_f32) * self.ch;
                append(out, &self.staged_f32[..got], f32::to_le_bytes);
            }
        }
        self.output_bytes += (out.len() - before) as u64;
    }

    /// Flushes the remaining output at end of input.
    pub fn end_of_stream(&mut self, out: &mut Vec<u8>) {
        match &mut self.engine {
            Engine::Short(s) => s.queue_end_of_stream(),
            Engine::Float(s) => s.queue_end_of_stream(),
        }
        self.drain(out);
    }

    #[cfg(any(test, feature = "synth"))]
    fn processed_input_bytes(&self) -> u64 {
        let pending = match &self.engine {
            Engine::Short(s) => s.pending_input_frames() * self.ch * 2,
            Engine::Float(s) => s.pending_input_frames() * self.ch * 4,
        } as u64;
        self.input_bytes.saturating_sub(pending)
    }

    /// Media time covered by `playout_us` of played output, computed as media3 does.
    #[cfg(any(test, feature = "synth"))]
    pub fn media_duration_us(&self, playout_us: i64) -> i64 {
        if self.output_bytes >= MIN_BYTES_FOR_DURATION_SCALING {
            (playout_us as i128 * self.processed_input_bytes() as i128 / self.output_bytes as i128) as i64
        } else {
            nominal_media_us(self.speed, playout_us)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_only_off_unity() {

        assert!(!speed_active(1.0, 1.0));
        assert!(!speed_active(1.00005, 0.99995), "float noise");
        assert!(speed_active(1.25, 1.0));
        assert!(speed_active(1.0, 0.9));
        assert!(speed_active(1.0001, 1.0), "the threshold itself counts");
        assert_eq!(nominal_media_us(1.5, 1_000_000), 1_500_000);
        assert_eq!(nominal_playout_us(2.0, 1_000_000), 500_000);
        assert_eq!(nominal_playout_us(1.0, 7), 7);
    }

    fn sine(secs: f64, hz: f64) -> Vec<u8> {
        (0..(44100.0 * secs) as usize)
            .flat_map(|i| {
                let v = ((i as f64 / 44100.0 * hz * std::f64::consts::TAU).sin() * 12000.0) as i16;
                [v, v]
            })
            .flat_map(|v| v.to_le_bytes())
            .collect()
    }

    #[test]
    fn faster_is_shorter() {
        let mut p = SpeedPitch::new(44100, 2, Encoding::Pcm16);
        p.set(1.5, 1.0);
        p.flush();
        let mut out = Vec::new();
        for c in sine(3.0, 220.0).chunks(8192) {
            p.process(c, &mut out);
        }
        p.end_of_stream(&mut out);
        let secs = out.len() as f64 / 4.0 / 44100.0;
        assert!((secs - 2.0).abs() < 0.01, "{secs}");
    }

    #[test]
    fn media_time_runs_at_the_speed() {
        let mut p = SpeedPitch::new(44100, 2, Encoding::Pcm16);
        p.set(1.5, 1.0);
        p.flush();
        let mut out = Vec::new();
        p.process(&sine(3.0, 220.0), &mut out);
        let m = p.media_duration_us(1_000_000);
        assert!((m - 1_500_000).abs() < 20_000, "{m}");
    }
}
