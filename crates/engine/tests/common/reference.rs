//! What the output should have played, rendered offline from the samples the chain was given, and
//! checks of what it did play against that: sample for sample, and free of clicks.
//!
//! The rendering runs the sound chain's parts (equalizer, silence skipping, speed) directly, switching
//! settings at the input frame the engine logged for each change, as a chain that was never spliced
//! would; what the output held from before the change blends into it over `BLEND_US`, as the card
//! hears it. Nothing of the sink, the kept input or the ring is used.

use nori_player::dsp::Equalizer;
use nori_player::pcm::Encoding;
use nori_player::pipeline::{blended, ChainSettings, BLEND_US};
use nori_player::silence::SilenceSkipper;
use nori_player::speed::{speed_active, SpeedPitch};

/// Frames the rendering runs at a time.
const CHUNK: usize = 1000;

/// Where a change started: frames since the last flush, of the chain's input and of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Splice {
    pub input: u64,
    pub output: u64,
}

/// The splices the engine logged, in order.
pub fn splices(log: &[String]) -> Vec<Splice> {
    let number = |l: &str, after: &str| l.split(after).nth(1).and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse().ok());
    log.iter()
        .filter(|l| l.contains("changes from output frame"))
        .filter_map(|l| Some(Splice { output: number(l, "output frame ")?, input: number(l, "input frame ")? }))
        .collect()
}

/// The chain's parts as the sink builds them for `s`.
struct Chain {
    eq: Option<Equalizer>,
    silence: Option<SilenceSkipper>,
    speed: Option<SpeedPitch>,
    rate: u32,
}

impl Chain {
    fn new(s: &ChainSettings, rate: u32) -> Chain {
        let mut c = Chain { eq: None, silence: None, speed: None, rate };
        c.change(s, false);
        c
    }

    /// Goes on with `s` (`live`: music has gone through).
    fn change(&mut self, s: &ChainSettings, live: bool) -> Vec<u8> {
        if self.eq.is_none() && (s.keep_eq || s.sound.on()) {
            self.eq = Some(Equalizer::new(self.rate, 2));
        }
        if let Some(eq) = self.eq.as_mut() {
            if live {
                eq.continuing();
            }
            s.sound.apply(eq);
        }
        match self.speed.as_mut() {
            Some(sp) => sp.set(s.speed, s.pitch),
            None if speed_active(s.speed, s.pitch) => {
                let mut sp = SpeedPitch::new(self.rate, 2, Encoding::Pcm16);
                sp.set(s.speed, s.pitch);
                self.speed = Some(sp);
            }
            None => {}
        }
        let mut held = Vec::new();
        match (s.skip_silence, self.silence.take()) {
            (true, None) => self.silence = Some(SilenceSkipper::new(self.rate, 2, false)),
            (true, kept) => self.silence = kept,
            (false, Some(mut left)) => {
                left.end_of_stream(&mut held);
                held = self.sped(held);
            }
            (false, None) => {}
        }
        held
    }

    fn sped(&mut self, data: Vec<u8>) -> Vec<u8> {
        let Some(sp) = self.speed.as_mut() else { return data };
        let mut out = Vec::new();
        sp.process(&data, &mut out);
        out
    }

    fn run(&mut self, input: &[i16]) -> Vec<u8> {
        let mut data: Vec<u8> = match self.eq.as_mut().filter(|e| !e.is_identity()) {
            Some(eq) => {
                let mut out = vec![0i16; input.len()];
                eq.process_i16(input, &mut out);
                out.iter().flat_map(|v| v.to_le_bytes()).collect()
            }
            None => input.iter().flat_map(|v| v.to_le_bytes()).collect(),
        };
        if let Some(s) = self.silence.as_mut() {
            let mut out = Vec::new();
            s.process(&data, &mut out);
            data = out;
        }
        self.sped(data)
    }

    /// The end of the queue: the limiter's look-ahead and what the stages hold.
    fn end(&mut self) -> Vec<u8> {
        let held = self.eq.as_ref().filter(|e| !e.is_identity()).map_or(0, Equalizer::delay_frames);
        let mut out = if held > 0 { self.run(&vec![0; held * 2]) } else { Vec::new() };
        let mut rest = Vec::new();
        if let Some(s) = self.silence.as_mut() {
            s.end_of_stream(&mut rest);
        }
        rest = self.sped(rest);
        if let Some(sp) = self.speed.as_mut() {
            sp.end_of_stream(&mut rest);
        }
        out.extend(rest);
        out
    }
}

/// `raw` (stereo 16-bit, as the chain was given it) through the chain with each change's settings from
/// its input frame on (the first at 0), to the end of the queue.
pub fn render(raw: &[i16], rate: u32, changes: &[(u64, ChainSettings)]) -> Vec<i16> {
    let mut chain = Chain::new(&changes[0].1, rate);
    let mut out = Vec::new();
    let frames = raw.len() / 2;
    for (k, (from, s)) in changes.iter().enumerate() {
        if k > 0 {
            out.extend(chain.change(s, *from > 0));
        }
        let to = changes.get(k + 1).map_or(frames, |c| c.0 as usize);
        for at in (*from as usize..to).step_by(CHUNK) {
            out.extend(chain.run(&raw[at * 2..(at + CHUNK).min(to) * 2]));
        }
    }
    out.extend(chain.end());
    out.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect()
}

/// What a card hears when the output held `old` and was replaced from output frame `at` on by `new`:
/// the one blended into the other over [`BLEND_US`], each sample as the ring holds it.
pub fn spliced(old: &[i16], new: &[i16], at: usize, rate: u32) -> Vec<i16> {
    let n = (rate as i64 * BLEND_US / 1_000_000) as usize;
    let mut out = new.to_vec();
    out[..at * 2].copy_from_slice(&old[..at * 2]);
    for k in 0..n.min(old.len() / 2 - at).min(new.len() / 2 - at) {
        for c in 0..2 {
            let i = (at + k) * 2 + c;
            let v = blended(old[i] as f32 / 32768.0, new[i] as f32 / 32768.0, k, n);
            out[i] = (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        }
    }
    out
}

/// The first sample of `heard` further than `tolerance` from `want`, or where one ends before the other.
pub fn first_difference(heard: &[i16], want: &[i16], tolerance: i32) -> Option<usize> {
    heard.iter().zip(want).position(|(h, w)| (*h as i32 - *w as i32).abs() > tolerance).or((heard.len() != want.len()).then_some(heard.len().min(want.len())))
}

/// Says where `heard` leaves `want`: the frame, and the samples around it.
pub fn describe(heard: &[i16], want: &[i16], at: usize, rate: u32) -> String {
    let from = at.saturating_sub(6) & !1;
    let to = (at + 6).min(heard.len()).min(want.len());
    format!("at frame {} ({:.4} s) of {} heard, {} wanted: heard {:?}, wanted {:?}", at / 2, at as f64 / 2.0 / rate as f64, heard.len() / 2, want.len() / 2, &heard[from..to.max(from)], &want[from..to.max(from)])
}

/// Frames where a channel jumps: a second difference over `ratio` times the largest of the
/// surrounding 10 ms (a click, a gap's edge, a lost stretch).
pub fn clicks(heard: &[i16], rate: u32, ratio: f64) -> Vec<usize> {
    let w = rate as usize / 100;
    let mut found = Vec::new();
    for c in 0..2 {
        let x: Vec<f64> = heard.iter().skip(c).step_by(2).map(|&v| v as f64).collect();
        let d2: Vec<f64> = x.windows(3).map(|t| (t[2] - 2.0 * t[1] + t[0]).abs()).collect();
        for k in w..d2.len().saturating_sub(w) {
            let around = d2[k - w..k - 1].iter().chain(&d2[k + 2..k + w]).fold(0.0f64, |m, v| m.max(*v));
            if d2[k] > ratio * around.max(1.0) {
                found.push(k + 1);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}
