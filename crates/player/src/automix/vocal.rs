//! Where a voice sings in a song: a compact vocal activity curve, measured in the same pass as the rest of the
//! analysis from the FFT it already takes, so that synced lyrics can be checked against the audio (nori-lyrics
//! sync.rs).
//!
//! **What it measures.** A singing voice is a pitched sound in the voice band (300 Hz to 3 kHz) that never holds
//! still: vibrato, glides and a new vowel on every syllable move its harmonics from frame to frame. Drums are in
//! the band too, but they are noise, not peaks; pads and held chords are peaks, but they sit still. So each frame
//! the band's clear spectral peaks are found (a bin above its neighbours and five times the bins three away: a
//! Hann window's main lobe over its skirts), and what is added up is how far the log level moved since the frame
//! before in the three bins around each peak. On the synthetic songs of `eval.rs` this "pitched movement" tells
//! sung from unsung frames with an area under the ROC curve of 0.986 (`vocal_curve_eval`), where the band's
//! share of the power (the `vocal` curve AutoMix's gates read) gets 0.71 and the band's plain spectral flux 0.85.
//!
//! **On real songs** (nori-lyrics sync_tune.rs: 59 songs of a real library, the lines of lyrics the services
//! agree on taken as where the voice is) it tells much less: an AUC of about 0.7 in pop, rock and rap, and none
//! at all in metal (about 0.5): distorted guitars are pitched and restless too, and a solo reads as singing. What
//! still works is timing: the lines' starts fall where the curve rises, so offsets are found (within 250 ms for
//! 95 % of timings shifted by hand) even where the level itself says little. The band and the peak test were
//! chosen there: 300 Hz to 3 kHz and five times (from 250 Hz to 4 kHz and ten times) put the offsets' 90th
//! percentile error from 136 to 38 ms and told good timings from another song's better (AUC 0.88 to 0.94 on how
//! much better they fit where they are than slid far off). A lower bar still (three times) left too few songs
//! with a curve that moves; subtracting the movement of the bins that are no peak's did worse everywhere.
//!
//! **Cost.** Per analysis frame (86 a second) the band's power (about 125 bins) from the spectrum the analysis
//! has already taken, the peak test, and two square roots and a logarithm per bin around a peak; nothing
//! allocated. 15 ms for a four-minute song on the host, a twelfth of the analysis front end (`vocal_cost`). What
//! is kept is one number per [`CURVE_EVERY`] frames (about 17 a second), stored as one byte each: about a
//! kilobyte a minute. (A byte of 16 levels, or 11 frames a second, measured worse on `sync_eval`.)
//!
//! Mono only: the analyser is fed the downmix, so the voice's usual place in the middle of the stereo image is
//! not used.

use rustfft::num_complex::Complex32;

/// The voice band, Hz.
pub const LO_HZ: f64 = 300.0;
pub const HI_HZ: f64 = 3000.0;
/// Log compression of the band's amplitudes, as the onset curves have it: linear below -60 dB.
const GAMMA: f32 = 1000.0;
/// A peak is this many times the power of the bins three away from it.
const PEAK_OVER: f32 = 5.0;
/// How long a bin's mid and side power are averaged over to say how centred it is, seconds: long enough that
/// two takes of a riff panned apart, whose phases wander against each other, read as steadily off centre (an
/// instant's reading flickers, and the flicker would read as movement), short enough to follow a voice
/// coming in over them.
const CENTRE_SMOOTH_S: f64 = 0.1;
/// A bin whose channels are less alike than this is not in the middle ([`centre`]). On `metal_band`, sung told
/// from unsung with an AUC of 0.94 at 0.5, 0.95 at 0.6, 0.97 at 0.7 and 0.95 at 0.8 (the voice's own room
/// starts to go); 0.6 leaves room for a voice mixed wider than the synthetic one. Averaging over 50 ms made it
/// 0.93, 150 ms 0.97 but a voice under a loud band is then heard later; the weight to the first or fourth power in
/// place of squared 0.94 and 0.97.
const CENTRE_FLOOR: f32 = 0.6;
/// Analysis frames per curve frame: about 17 a second, 58 ms each.
pub const CURVE_EVERY: usize = 5;
/// The stored form's version: a curve in another is measured again.
pub const CURVE_VERSION: u8 = 2;
/// Bytes of the stored form before the levels: the version, the frame rate and the time of frame 0.
const HEADER: usize = 9;
/// A curve frame's movement `m` is stored as `ln(1 + m) * LEVEL_SCALE`, clamped to a byte.
const LEVEL_SCALE: f32 = 40.0;

/// The per-frame side of the curve, fed each analysis frame's spectrum.
pub struct Tracker {
    lo: usize,
    hi: usize,
    amp_norm: f32,
    /// The band's power, with three bins of margin each side for the peak test.
    pow: Vec<f32>,
    /// The band's power the frame before.
    prev: Vec<f32>,
    /// The mid's and the side's power per bin of `pow`, averaged over about [`CENTRE_SMOOTH_S`].
    mid_pow: Vec<f32>,
    side_pow: Vec<f32>,
    /// The average's weight on what it was the frame before.
    keep: f32,
    acc: f32,
    n: usize,
    curve: Vec<f32>,
}

impl Tracker {
    /// For an FFT of `n` points at `sr` Hz whose |X| times `amp_norm` is a sine's amplitude; `frames` analysis
    /// frames `hop` samples apart expected (sizes the curve, so nothing grows while it plays).
    pub fn new(n: usize, hop: usize, sr: f64, amp_norm: f32, frames: usize) -> Self {
        let bin_hz = sr / n as f64;
        let lo = ((LO_HZ / bin_hz).ceil() as usize).max(4);
        let hi = ((HI_HZ.min(sr / 2.0 * 0.9) / bin_hz) as usize).min(n / 2 - 4).max(lo + 8);
        let w = hi - lo + 1;
        Tracker {
            lo,
            hi,
            amp_norm,
            pow: vec![0.0; w + 6],
            prev: vec![0.0; w],
            mid_pow: vec![0.0; w + 6],
            side_pow: vec![0.0; w + 6],
            keep: (-(hop as f64) / sr / CENTRE_SMOOTH_S).exp() as f32,
            acc: 0.0,
            n: 0,
            curve: Vec::with_capacity(frames / CURVE_EVERY + 2),
        }
    }

    /// One analysis frame's spectrum (at least `n / 2` bins) of a mono source, or of a stereo one whose
    /// channels are the same: the curve as it was before stereo was read.
    pub fn frame(&mut self, spec: &[Complex32]) {
        let (lo, hi, keep) = (self.lo, self.hi, self.keep);
        for (((p, c), mp), sp) in self.pow.iter_mut().zip(&spec[lo - 3..=hi + 3]).zip(&mut self.mid_pow).zip(&mut self.side_pow) {
            *p = c.norm_sqr();
            // Kept up, so that a stereo stretch after a mono one starts from what was heard.
            *mp = keep * *mp + (1.0 - keep) * *p;
            *sp *= keep;
        }
        self.step();
    }

    /// One analysis frame of a stereo source: the spectra of its mid, (L + R) / 2, and its side, (L - R) / 2
    /// (at least `n / 2` bins each). Only the middle of the stereo image is measured: each bin's mid is
    /// weighted by how centred the bin has sounded lately ([`centre`]), so that a guitar panned to one side,
    /// or two takes of it panned apart, drop out and a voice in the middle stays.
    pub fn frame_stereo(&mut self, mid: &[Complex32], side: &[Complex32]) {
        let (lo, hi, keep) = (self.lo, self.hi, self.keep);
        let bins = mid[lo - 3..=hi + 3].iter().zip(&side[lo - 3..=hi + 3]);
        for (((p, (m, s)), mp), sp) in self.pow.iter_mut().zip(bins).zip(&mut self.mid_pow).zip(&mut self.side_pow) {
            let m = m.norm_sqr();
            *mp = keep * *mp + (1.0 - keep) * m;
            *sp = keep * *sp + (1.0 - keep) * s.norm_sqr();
            *p = m * centre(*mp, *sp);
        }
        self.step();
    }

    /// The movement around this frame's peaks in `pow`, added to the curve.
    fn step(&mut self) {
        // Amplitudes only where they are read: the bins around a peak (and every bin for the next frame to
        // compare with, which is the last frame's `pow`, kept as it was).
        let w = self.prev.len();
        let g = GAMMA * self.amp_norm;
        let mut moved = 0f32;
        let mut done = 0usize;
        for j in 0..w {
            let k = j + 3;
            let p = self.pow[k];
            if p >= self.pow[k - 1] && p >= self.pow[k + 1] && p > PEAK_OVER * 0.5 * (self.pow[k - 3] + self.pow[k + 3]) {
                for i in j.saturating_sub(1).max(done)..(j + 2).min(w) {
                    let now = 1.0 + g * self.pow[i + 3].sqrt();
                    let before = 1.0 + g * self.prev[i].sqrt();
                    moved += (now / before).ln().abs();
                }
                done = (j + 2).min(w);
            }
        }
        self.prev.copy_from_slice(&self.pow[3..3 + w]);
        self.acc += moved;
        self.n += 1;
        if self.n == CURVE_EVERY {
            self.curve.push(self.acc / CURVE_EVERY as f32);
            (self.acc, self.n) = (0.0, 0);
        }
    }

    /// The curve so far, one value per [`CURVE_EVERY`] analysis frames; the tracker starts again.
    pub fn take(&mut self) -> Vec<f32> {
        let out = std::mem::take(&mut self.curve);
        self.reset();
        out
    }

    pub fn reset(&mut self) {
        self.prev.fill(0.0);
        self.mid_pow.fill(0.0);
        self.side_pow.fill(0.0);
        self.curve.clear();
        (self.acc, self.n) = (0.0, 0);
    }
}

/// How much of a bin's power is counted, from its mid's and side's power averaged over time. With L and R the
/// channels, `(mid - side) / (mid + side)` is `2 Re<L R*> / (<|L|²> + <|R|²>)`, how alike the two channels are
/// (Avendano and Jot's similarity, as upmixers use it): 1 dead centre, 0.94 for a sound panned 3 dB to one side,
/// 0.8 for 6 dB, 0 for one channel alone, for two unrelated sounds one each side, or for the channels out of
/// phase. Below [`CENTRE_FLOOR`] a bin counts for nothing, and from there to 1 its amplitude is weighted up to
/// full, squared as power.
///
/// A gate, not a gentle weight, because the curve reads the log level's movement, which a sound turned down
/// but still well over the -60 dB knee keeps: a guitar has to be taken out, not quietened. And two takes of a
/// riff panned apart are not unrelated: where their harmonics meet they are alike in about half the bins, as
/// their timing and tuning put them in phase there, so a plain `r²` weight (no floor) left them (`metal_band`:
/// AUC 0.65, against 0.95 with the floor).
#[inline]
fn centre(mid: f32, side: f32) -> f32 {
    let sum = mid + side;
    if sum <= 0.0 {
        return 0.0;
    }
    let w = ((mid - side) / sum - CENTRE_FLOOR) / (1.0 - CENTRE_FLOOR);
    w.max(0.0) * w.max(0.0)
}

/// A song's vocal activity: one byte per frame, higher where a voice (or something as restless and pitched)
/// sounds. Frame `k` is at `t0 + k / fps` seconds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VocalCurve {
    pub fps: f32,
    pub t0: f32,
    pub level: Vec<u8>,
}

impl VocalCurve {
    /// From the raw curve of an analysis at `fps` analysis frames a second whose frame 0 is at `t0`.
    pub fn from_raw(raw: &[f32], fps: f64, t0: f64) -> Self {
        let e = CURVE_EVERY as f64;
        VocalCurve {
            fps: (fps / e) as f32,
            // A curve frame is the mean of its analysis frames: it sits at their middle.
            t0: (t0 + (e - 1.0) / 2.0 / fps) as f32,
            level: raw.iter().map(|m| ((1.0 + m.max(0.0)).ln() * LEVEL_SCALE).round().clamp(0.0, 255.0) as u8).collect(),
        }
    }

    /// Seconds covered.
    pub fn seconds(&self) -> f64 {
        if self.fps > 0.0 {
            self.level.len() as f64 / self.fps as f64
        } else {
            0.0
        }
    }

    /// The stored form: [`CURVE_VERSION`], the frame rate and frame 0's time (f32, little-endian), the levels.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + self.level.len());
        out.push(CURVE_VERSION);
        out.extend_from_slice(&self.fps.to_le_bytes());
        out.extend_from_slice(&self.t0.to_le_bytes());
        out.extend_from_slice(&self.level);
        out
    }

    /// None for another version or a form that does not read.
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < HEADER || b[0] != CURVE_VERSION {
            return None;
        }
        let fps = f32::from_le_bytes(b[1..5].try_into().ok()?);
        let t0 = f32::from_le_bytes(b[5..9].try_into().ok()?);
        (fps.is_finite() && fps > 0.0 && t0.is_finite()).then(|| VocalCurve { fps, t0, level: b[HEADER..].to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::super::analysis::Analyzer;
    use super::super::eval::{corpus_all, Song, Style, FULL, SUNG};
    use super::*;

    fn curve_of(song: &Song) -> (VocalCurve, super::super::eval::Truth, f64) {
        let (x, truth) = song.render();
        let mut a = Analyzer::new(song.rate, 0);
        a.feed(&x);
        let secs = x.len() as f64 / song.rate as f64;
        (a.take_features().voice_curve(), truth, secs)
    }

    fn auc(pos: &[f32], neg: &[f32]) -> f64 {
        let mut all: Vec<(f32, bool)> = pos.iter().map(|v| (*v, true)).chain(neg.iter().map(|v| (*v, false))).collect();
        all.sort_by(|a, b| a.0.total_cmp(&b.0));
        let rank_sum: f64 = all.iter().enumerate().filter(|(_, (_, p))| *p).map(|(r, _)| (r + 1) as f64).sum();
        let (np, nn) = (pos.len() as f64, neg.len() as f64);
        (rank_sum - np * (np + 1.0) / 2.0) / (np * nn)
    }

    /// Sung frames against unsung ones, per sung song of the corpus, the curve smoothed over half a second as
    /// the sync check reads it.
    fn separation(song: &Song) -> f64 {
        let (c, truth, _) = curve_of(song);
        let r = (0.25 * c.fps) as usize;
        let (mut pos, mut neg) = (Vec::new(), Vec::new());
        for k in 0..c.level.len() {
            let t = c.t0 as f64 + k as f64 / c.fps as f64;
            if t < truth.music.0 + 1.0 || t > truth.music.1 - 1.0 {
                continue;
            }
            let (a, b) = (k.saturating_sub(r), (k + r + 1).min(c.level.len()));
            let v = c.level[a..b].iter().map(|v| *v as f32).sum::<f32>() / (b - a) as f32;
            if truth.sung_at(t) {
                pos.push(v)
            } else {
                neg.push(v)
            }
        }
        auc(&pos, &neg)
    }

    #[test]
    fn the_curve_rises_where_the_voice_sings() {
        let song = Song { sections: vec![(4, FULL), (8, SUNG), (4, FULL), (8, SUNG)], ..Song::new("sung", Style::Backbeat, 110.0, 2, false) };
        let a = separation(&song);
        assert!(a > 0.95, "sung told from unsung frames with an AUC of {a:.3}");
    }

    #[test]
    fn it_is_about_a_kilobyte_a_minute_and_reads_back() {
        let song = Song { sections: vec![(24, SUNG)], ..Song::new("sung", Style::House, 120.0, 2, false) };
        let (c, _, secs) = curve_of(&song);
        let per_min = c.level.len() as f64 / secs * 60.0;
        assert!((1000.0..1100.0).contains(&per_min), "{per_min} bytes a minute");
        assert!((c.fps - 17.2).abs() < 0.1, "{}", c.fps);
        assert_eq!(VocalCurve::decode(&c.encode()), Some(c.clone()));
        assert_eq!(VocalCurve::decode(&[2, 0, 0]), None);
        let mut other = c.encode();
        other[0] = CURVE_VERSION + 1;
        assert_eq!(VocalCurve::decode(&other), None, "another version is measured again");
    }

    /// A metal band in stereo: two takes of a distorted sawtooth riff with bends and a held, wobbling note in
    /// every bar, one hard left and one hard right (the second 12 ms late and 7 cents sharp, as a second take
    /// is), a kick and snare in the middle, and a voice in the middle every other four seconds: vibrato tone
    /// bursts a syllable long, gliding from note to note, their vowel changing, in a room (echoes that differ
    /// left and right, 6 dB under it). Interleaved at 44.1 kHz, and whether the voice sings at a time.
    fn metal_band(secs: f64) -> (Vec<f32>, impl Fn(f64) -> Option<bool>) {
        use std::f64::consts::TAU;
        let rate = 44_100.0;
        let len = (secs * rate) as usize;
        let (mut l, mut r) = (vec![0f64; len], vec![0f64; len]);
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let semis = |f: f64, s: f64| f * 2f64.powf(s / 12.0);
        // The riff: eighths at 150 BPM, E2 power chords moving about, every other note bent up into from a
        // semitone under, the bar's last quarter held with a 6 Hz vibrato.
        let eighth = 0.2;
        let riff = [0.0, 0.0, 3.0, 0.0, 5.0, 3.0, 7.0];
        // A player never hits a note the same way twice: each take's notes land up to 8 ms apart and 5 cents
        // either way of where the other take's do.
        let hash = |k: u64| {
            let mut z = k.wrapping_add(0x9e37_79b9_7f4a_7c15);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        };
        for (take, (out, late, cents)) in [(&mut l, 0.0, 0.0), (&mut r, 0.012, 7.0)].into_iter().enumerate() {
            let mut phase = [0f64; 3];
            for i in 0..len {
                let note_at = (i as f64 / rate / eighth) as u64 * 2 + take as u64;
                let t = i as f64 / rate - late - 0.008 * hash(note_at);
                let cents = cents + 10.0 * hash(note_at + 1_000_000) - 5.0;
                if t < 0.0 {
                    continue;
                }
                let bar_t = t % (8.0 * eighth);
                let step = ((bar_t / eighth) as usize).min(7);
                let (note, since) = if step < 6 { (riff[step], bar_t - step as f64 * eighth) } else { (riff[6], bar_t - 6.0 * eighth) };
                let bend = if step % 2 == 1 && step < 6 { -(1.0 - since / 0.08).max(0.0) } else { 0.0 };
                let vib = if step >= 6 { 0.4 * (TAU * 6.0 * since + take as f64).sin() * (since / 0.1).min(1.0) } else { 0.0 };
                let root = semis(82.41, note + bend + vib + cents / 100.0);
                let mut v = 0.0;
                for (h, ratio) in [1.0, 1.5, 2.0].iter().enumerate() {
                    phase[h] = (phase[h] + root * ratio / rate).fract();
                    v += 2.0 * phase[h] - 1.0;
                }
                let pick = (since / 0.005).min(1.0) * (0.75 + 0.25 * (-since / 0.3).exp());
                out[i] += 0.3 * (3.0 * v * pick).tanh();
            }
        }
        // Drums in the middle: a kick on every beat, a snare on two and four.
        for beat in 0..(secs / 0.4) as usize {
            let at = (beat as f64 * 0.4 * rate) as usize;
            for i in 0..(0.3 * rate) as usize {
                let tau = i as f64 / rate;
                let kick = (TAU * (50.0 * tau + 100.0 * 0.03 * (1.0 - (-tau / 0.03).exp()))).sin() * (-tau / 0.12).exp() * 0.5;
                let snare = if beat % 2 == 1 { (rnd() * 2.0 - 1.0) * (-tau / 0.08).exp() * 0.35 } else { 0.0 };
                if at + i < len {
                    l[at + i] += kick + snare;
                    r[at + i] += kick + snare;
                }
            }
        }
        // The voice: every other four seconds from the fourth, syllables of 180 to 350 ms with a breath between.
        let sings = |t: f64| (t / 4.0) as usize % 2 == 1;
        let scale = [0.0, 2.0, 3.0, 5.0, 7.0, 8.0, 10.0, 12.0];
        let (mut t, mut f_was, mut phase) = (0.0, 220.0, 0f64);
        let mut dry = vec![0f64; len];
        while t < secs {
            if !sings(t) {
                t = (t / 4.0).floor() * 4.0 + 4.0;
                continue;
            }
            let dur = 0.18 + 0.17 * rnd();
            let f = semis(196.0, scale[(rnd() * 8.0) as usize]);
            let (f1, f2) = ([500.0, 700.0, 350.0, 400.0][(rnd() * 4.0) as usize], [1000.0, 1200.0, 2200.0, 1800.0][(rnd() * 4.0) as usize]);
            let (a, n) = ((t * rate) as usize, (dur * rate) as usize);
            for i in 0..n.min(len.saturating_sub(a)) {
                let tau = i as f64 / rate;
                let glide = f_was + (f - f_was) * (tau / 0.04).min(1.0);
                let fv = glide * 2f64.powf(0.25 / 12.0 * (TAU * 5.5 * tau).sin() * ((tau - 0.1) / 0.1).clamp(0.0, 1.0));
                phase = (phase + fv / rate).fract();
                let mut v = 0.0;
                for h in 1..=14 {
                    let hf = fv * h as f64;
                    let formant = (-((hf - f1) / 200.0).powi(2)).exp() + 0.6 * (-((hf - f2) / 300.0).powi(2)).exp() + 0.1;
                    v += formant / h as f64 * (TAU * phase * h as f64).sin();
                }
                let env = (tau / 0.015).min(1.0) * ((dur - tau) / 0.03).min(1.0);
                dry[a + i] = 0.22 * v * env;
            }
            f_was = f;
            t += dur + 0.04;
        }
        // The voice dry in the middle, and a room around it: a dozen echoes 15 to 150 ms late, dying away,
        // different in each channel, together 6 dB under the voice.
        let wet = 0.5;
        for out in [&mut l, &mut r] {
            let taps: Vec<(usize, f64)> = (0..12).map(|_| { let d = 0.015 + 0.135 * rnd(); ((d * rate) as usize, (-d / 0.08).exp() * if rnd() < 0.5 { -1.0 } else { 1.0 }) }).collect();
            let norm = wet / taps.iter().map(|t| t.1 * t.1).sum::<f64>().sqrt();
            for i in 0..len {
                let mut v = dry[i];
                for &(d, g) in &taps {
                    if i >= d {
                        v += norm * g * dry[i - d];
                    }
                }
                out[i] += v;
            }
        }
        let x = l.iter().zip(&r).flat_map(|(a, b)| [*a as f32 * 0.5, *b as f32 * 0.5]).collect();
        // Half a second either side of a change is neither.
        let truth = move |t: f64| {
            let edge = (t - (t / 4.0).round() * 4.0).abs() < 0.5;
            (!edge && t > 4.0).then(|| sings(t))
        };
        (x, truth)
    }

    /// How well a curve's level, smoothed over half a second as the sync check reads it, tells sung from unsung.
    fn separation_of(c: &VocalCurve, truth: &dyn Fn(f64) -> Option<bool>) -> f64 {
        let r = (0.25 * c.fps) as usize;
        let (mut pos, mut neg) = (Vec::new(), Vec::new());
        for k in 0..c.level.len() {
            let t = c.t0 as f64 + k as f64 / c.fps as f64;
            let (a, b) = (k.saturating_sub(r), (k + r + 1).min(c.level.len()));
            let v = c.level[a..b].iter().map(|v| *v as f32).sum::<f32>() / (b - a) as f32;
            match truth(t) {
                Some(true) => pos.push(v),
                Some(false) => neg.push(v),
                None => {}
            }
        }
        auc(&pos, &neg)
    }

    /// Distorted, bending guitars are as pitched and restless as a voice, so the downmix's curve hardly tells
    /// when the voice sings; they are panned and the voice is not, so the middle of the stereo image does.
    #[test]
    fn the_middle_of_the_stereo_image_hears_the_voice_over_panned_guitars() {
        let (x, truth) = metal_band(40.0);
        let mid: Vec<f32> = x.as_chunks::<2>().0.iter().map(|p| (p[0] + p[1]) * 0.5).collect();
        let mut a = Analyzer::new(44_100, 40_000);
        a.feed(&mid);
        let mono = separation_of(&a.take_features().voice_curve(), &truth);
        a.feed_interleaved(&x, 2, |v| v);
        let stereo = separation_of(&a.take_features().voice_curve(), &truth);
        println!("sung told from unsung: downmix AUC {mono:.3}, the middle {stereo:.3}");
        assert!(mono < 0.7, "the downmix already told them apart ({mono:.3}): the band is no test");
        assert!(stereo > 0.9 && stereo - mono > 0.25, "downmix {mono:.3}, middle {stereo:.3}");
    }

    /// A mono song in a stereo file (both channels the same) is measured exactly as the mono one, the curve
    /// and everything else; and in real stereo everything but the curve is measured on the same downmix as
    /// before, the one FFT that now carries the side changing it only by rounding.
    #[test]
    fn stereo_changes_only_the_curve_and_same_channels_change_nothing() {
        let song = Song { sections: vec![(4, FULL), (8, SUNG), (4, FULL)], ..Song::new("sung", Style::Backbeat, 110.0, 2, false) };
        let (x, _) = song.render();
        let mut a = Analyzer::new(song.rate, 0);
        a.feed(&x);
        let mono = a.take_features();
        let twice: Vec<f32> = x.iter().flat_map(|v| [*v, *v]).collect();
        a.feed_interleaved(&twice, 2, |v| v);
        let same = a.take_features();
        assert_eq!(same.voice, mono.voice);
        assert_eq!((&same.onset, &same.power, &same.vocal, &same.chroma, &same.blocks_k), (&mono.onset, &mono.power, &mono.vocal, &mono.chroma, &mono.blocks_k));
        a.feed_interleaved(&x, 1, |v| v);
        assert_eq!(a.take_features().voice, mono.voice, "one channel");

        // The same downmix with a side under it: the right channel 6 dB down, the left made up to keep the mid.
        let panned: Vec<f32> = x.iter().flat_map(|v| [*v * 4.0 / 3.0, *v * 2.0 / 3.0]).collect();
        a.feed_interleaved(&panned, 2, |v| v);
        let st = a.take_features();
        let close = |p: &[f32], q: &[f32]| p.len() == q.len() && p.iter().zip(q).all(|(p, q)| (p - q).abs() <= 1e-4 * (1.0 + p.abs().max(q.abs())));
        assert!(close(&st.onset, &mono.onset) && close(&st.power, &mono.power) && close(&st.vocal, &mono.vocal) && close(&st.low_onset, &mono.low_onset));
        assert!(close(&st.blocks_k, &mono.blocks_k), "loudness is measured on the same downmix");
        let (fa, fb) = (super::super::finish("a", &mono).track, super::super::finish("a", &st).track);
        assert!((fa.bpm - fb.bpm).abs() < 1e-6 && (fa.beat_offset_ms - fb.beat_offset_ms).abs() < 1e-3, "{fa:?} {fb:?}");
        assert_eq!((fa.key, fa.intro_end_ms, fa.outro_start_ms, fa.drop_ms), (fb.key, fb.intro_end_ms, fb.outro_start_ms, fb.drop_ms));
        assert_ne!(st.voice, mono.voice, "a sound 6 dB to one side counts less");
    }

    /// `cargo test --release -p nori-player vocal_cost -- --ignored --nocapture`: what the curve adds to the
    /// analysis of a four-minute song, the best of five runs of the analyser's own FFT loop with and without it
    /// (a loaded machine only ever adds time).
    #[test]
    #[ignore]
    fn vocal_cost() {
        use rustfft::FftPlanner;
        let song = Song { sections: vec![(8, FULL), (48, SUNG), (32, FULL), (32, SUNG)], ..Song::new("cost", Style::Backbeat, 120.0, 2, false) };
        let (x, _) = song.render();
        let secs = x.len() as f64 / song.rate as f64;
        let mono: Vec<f32> = x.as_chunks::<2>().0.iter().map(|p| (p[0] + p[1]) / 2.0).collect();
        let (n, hop, sr) = (1024usize, 256usize, song.rate as f64 / 2.0);
        let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
        let win: Vec<f32> = (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect();
        let mut buf = vec![Complex32::default(); n];
        let mut scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        let mut run = |with: bool| {
            let mut t = Tracker::new(n, hop, sr, 2.0 / win.iter().sum::<f32>(), mono.len() / hop);
            let start = std::time::Instant::now();
            let mut k = n;
            while k <= mono.len() {
                for (o, (s, w)) in buf.iter_mut().zip(mono[k - n..k].iter().zip(&win)) {
                    *o = Complex32::new(s * w, 0.0);
                }
                fft.process_with_scratch(&mut buf, &mut scratch);
                if with {
                    t.frame(&buf);
                }
                k += hop;
            }
            std::hint::black_box(t.take());
            start.elapsed().as_secs_f64() * 1000.0
        };
        let best = |r: &mut dyn FnMut() -> f64| (0..5).map(|_| r()).fold(f64::MAX, f64::min);
        let without = best(&mut || run(false));
        let with = best(&mut || run(true));
        let mut whole = || {
            let t = std::time::Instant::now();
            let mut a = Analyzer::new(song.rate, 0);
            a.feed(&x);
            std::hint::black_box(a.take_features());
            t.elapsed().as_secs_f64() * 1000.0
        };
        let analysis = best(&mut whole);
        println!(
            "{secs:.0} s song: FFT loop {without:.1} ms, with the curve {with:.1} ms: the curve costs {:.1} ms ({:.2} ms a minute), the whole front end {analysis:.1} ms",
            with - without,
            (with - without) / secs * 60.0
        );
    }

    /// `cargo test --release -p nori-player vocal_curve_eval -- --ignored --nocapture`: the table the module's
    /// comment quotes.
    #[test]
    #[ignore]
    fn vocal_curve_eval() {
        let songs: Vec<Song> = corpus_all().into_iter().filter(|s| s.sections.iter().any(|x| x.1.voice)).collect();
        let got: Vec<f64> = std::thread::scope(|sc| songs.iter().map(|s| sc.spawn(move || separation(s))).collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap()).collect());
        for (s, a) in songs.iter().zip(&got) {
            println!("{:<20} AUC {a:.3}", s.name);
        }
        println!("mean AUC {:.3}", got.iter().sum::<f64>() / got.len() as f64);
    }
}
