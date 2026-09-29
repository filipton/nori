//! Evaluation harness for the track analysis.
//!
//! Synthetic songs whose beats, downbeats, metre, key, vocals and sections are known exactly: drums, bass, chords
//! and a sung line, in the styles that trip beat trackers up (swing, syncopation, a one-drop, half-time, 3/4, a
//! band that drifts, an accelerando, a long beatless intro, silence, a detuned band, a kick that moves from bar to
//! bar). They are scored the way the MIR literature scores these tasks: beat F-measure at ±70 ms, tempo Acc1
//! (within 4 %) and Acc2 (also ×2, ×½, ×3, ×⅓), downbeat F-measure, key accuracy and the MIREX weighted key score.
//!
//! Beside the textbook scores it keeps the ones the mix depends on. Over the half minute at each end that a
//! transition actually plays, is the grid the planner would use right, and did the analysis say it could be
//! trusted? A wrong grid that claims to be right is a train wreck; one that admits it is unsure only costs a
//! plainer fade. So every mix window is one of: bar-locked (beats and downbeats right, and trusted), beat-locked
//! but on the wrong beat of the bar, wrong, or refused.
//!
//! `cargo test --release -p nori-player analysis_eval -- --ignored --nocapture` prints the tables.
//! `NORI_EVAL_DUMP=<dir>` also writes every song as a 16-bit WAV with its reference beats, for other trackers.
//! `NORI_REAL=<dir>` scores real recordings the same way: `<name>.s16` (mono 16-bit little-endian), `<name>.rate`
//! (the sample rate), `<name>.beats` and `<name>.downbeats` (reference times in seconds, one per line).

use std::f64::consts::PI;

use super::plan::{at_end, at_start, grid_ok, VOCAL_MIN};
use super::structure::camelot;
use super::*;

/// Beat and downbeat tolerance, seconds (the standard ±70 ms).
const TOL: f64 = 0.07;
/// How much music at each end a mix plays over: 16 bars at 128 BPM.
const MIX_S: f64 = 30.0;

/// xorshift64: deterministic, so the scores never flake.
pub struct Rng(pub u64);

impl Rng {
    pub fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn signed(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }

    pub fn gauss(&mut self) -> f64 {
        let (u1, u2) = (self.unit().max(1e-12), self.unit());
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style {
    /// Four on the floor, off-beat hats and bass.
    House,
    /// Kick on 1 and 3, snare on 2 and 4, eighth hats.
    Backbeat,
    /// Snare on 3 only: the feel is half the written tempo.
    HalfTime,
    /// Drum and bass: kick on 1 and the and of 3, snare on 2 and 4.
    DnB,
    /// Syncopated kick and bass, ghost notes, sixteenth hats, chord stabs off the beat.
    Funk,
    /// Reggae one-drop: kick and snare on 3 only, the chords skank on every off-beat.
    OneDrop,
    /// 3/4: bass on 1, chords on 2 and 3.
    Waltz,
    /// Piano on every beat, a soft kit.
    Ballad,
    /// Syncopated rock: the kick falls in threes of sixteenths (on 1, the a of 1, the and of 2) and changes
    /// pattern from bar to bar at random, snare on 2 and 4 with ghost notes, bass on the kick.
    Broken,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Layers {
    pub drums: bool,
    pub bass: bool,
    pub chords: bool,
    pub voice: bool,
}

pub const FULL: Layers = Layers { drums: true, bass: true, chords: true, voice: false };
pub const SUNG: Layers = Layers { drums: true, bass: true, chords: true, voice: true };
pub const DRUMS: Layers = Layers { drums: true, bass: false, chords: false, voice: false };
pub const SILENT: Layers = Layers { drums: false, bass: false, chords: false, voice: false };
pub const KEYS: Layers = Layers { drums: false, bass: true, chords: true, voice: false };
pub const KEYS_SUNG: Layers = Layers { drums: false, bass: true, chords: true, voice: true };
/// Chords alone: a breakdown, a coda, the pad before a build.
pub const PADS: Layers = Layers { drums: false, bass: false, chords: true, voice: false };

#[derive(Clone)]
pub struct Song {
    pub name: &'static str,
    pub rate: u32,
    pub style: Style,
    pub meter: usize,
    pub bpm: f64,
    /// Tempo at the last beat, for an accelerando or a band slowing down (0 = steady).
    pub end_bpm: f64,
    /// Slow tempo wander, per cent of the tempo, over a 37 s cycle: a band without a click.
    pub wander_pct: f64,
    /// Per-hit timing spread, ms (standard deviation).
    pub jitter_ms: f64,
    /// Where the off-beat eighth falls, in beats (0.5 straight, 0.62 swung).
    pub swing: f64,
    pub tonic: usize,
    pub minor: bool,
    /// Tuning away from A = 440 Hz, cents.
    pub cents: f64,
    /// Which chord progression (0 or 1).
    pub progression: usize,
    /// (bars, what plays).
    pub sections: Vec<(usize, Layers)>,
    pub lead_silence: f64,
    pub tail_silence: f64,
    /// Seconds of beatless pad before the first bar.
    pub ambient_s: f64,
    pub noise: f64,
    /// Silent sections are true digital silence (no noise floor): a hidden track's gap, which a master leaves
    /// empty. Off for the analysis corpus, whose `silences-128` keeps its noise floor as before.
    pub quiet_gaps: bool,
    pub seed: u64,
}

impl Song {
    pub fn new(name: &'static str, style: Style, bpm: f64, tonic: usize, minor: bool) -> Self {
        Song {
            name,
            rate: 44100,
            style,
            meter: if style == Style::Waltz { 3 } else { 4 },
            bpm,
            end_bpm: 0.0,
            wander_pct: 0.0,
            jitter_ms: 3.0,
            swing: 0.5,
            tonic,
            minor,
            cents: 0.0,
            progression: 0,
            sections: vec![(8, DRUMS), (24, FULL), (16, SUNG), (8, FULL)],
            lead_silence: 0.0,
            tail_silence: 0.0,
            ambient_s: 0.0,
            noise: 0.01,
            quiet_gaps: false,
            seed: 0x2545F4914F6CDD1D,
        }
    }
}

/// What the song really is.
pub struct Truth {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub meter: usize,
    /// Median tempo over the whole song.
    pub bpm: f64,
    /// Camelot code.
    pub key: i32,
    /// Sung notes, (start, end) seconds.
    pub voice: Vec<(f64, f64)>,
    /// First and last moment of sound.
    pub music: (f64, f64),
    /// Where the intro ends: the first section that brings something in, early in the song (the music's start
    /// when nothing does).
    pub intro_end: f64,
    /// Where the outro starts: the last section that takes something away, late in the song, when there is one.
    pub outro_start: Option<f64>,
    /// Where the arrangement arrives: the first section with drums, bass and chords all playing after one that
    /// lacked some of them, in the first 45 % of the song. None for a song that starts full.
    pub drop: Option<f64>,
    /// Where the ending stops being worth playing: the start of a long silence (6 s or more) followed by no more
    /// than 15 s of music (a hidden track), or else of a closing run of sections without drums that lasts 24 s
    /// or less (a breakdown or coda). None when the song should play to its end.
    pub exit: Option<f64>,
    /// Silent sections inside the music, (start, end) seconds.
    pub gaps: Vec<(f64, f64)>,
}

impl Truth {
    fn beats_in(&self, from: f64, to: f64) -> Vec<f64> {
        self.beats.iter().copied().filter(|t| *t >= from && *t < to).collect()
    }

    fn downbeats_in(&self, from: f64, to: f64) -> Vec<f64> {
        self.downbeats.iter().copied().filter(|t| *t >= from && *t < to).collect()
    }

    /// Music (not a gap) inside `[from, to)`, seconds.
    pub fn music_between(&self, from: f64, to: f64) -> f64 {
        let (from, to) = (from.max(self.music.0), to.min(self.music.1));
        if to <= from {
            return 0.0;
        }
        to - from - self.gaps.iter().map(|(a, b)| (b.min(to) - a.max(from)).max(0.0)).sum::<f64>()
    }

    /// Whether a voice sounds at `t`.
    pub fn sung_at(&self, t: f64) -> bool {
        self.voice.iter().any(|(a, b)| t >= *a && t < *b)
    }

    /// Share of `[from, to)` with a voice sounding.
    fn voice_share(&self, from: f64, to: f64) -> f64 {
        if to <= from {
            return 0.0;
        }
        self.voice.iter().map(|(a, b)| (b.min(to) - a.max(from)).max(0.0)).sum::<f64>() / (to - from)
    }
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Median tempo of a run of beat times.
fn median_bpm(beats: &[f64]) -> f64 {
    let mut ibi: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    let m = median(&mut ibi);
    if m > 0.0 {
        60.0 / m
    } else {
        0.0
    }
}

/// One period of a harmonic waveform, for cheap oscillators.
struct Table(Vec<f32>);

const TABLE: usize = 2048;

impl Table {
    fn new(harmonics: &[f64]) -> Self {
        let peak: f64 = harmonics.iter().map(|a| a.abs()).sum::<f64>().max(1e-9);
        Table(
            (0..TABLE)
                .map(|i| {
                    let ph = 2.0 * PI * i as f64 / TABLE as f64;
                    (harmonics.iter().enumerate().map(|(h, a)| a * ((h + 1) as f64 * ph).sin()).sum::<f64>() / peak) as f32
                })
                .collect(),
        )
    }

    #[inline]
    fn at(&self, phase: f64) -> f32 {
        let p = phase.fract() * TABLE as f64;
        let i = p as usize;
        let f = (p - i as f64) as f32;
        let a = self.0[i % TABLE];
        let b = self.0[(i + 1) % TABLE];
        a + (b - a) * f
    }
}

/// The formant envelope of a sung vowel at `f` Hz.
fn formant(f: f64, vowel: usize) -> f64 {
    const VOWELS: [[f64; 3]; 4] = [[730.0, 1090.0, 2440.0], [570.0, 840.0, 2410.0], [530.0, 1840.0, 2480.0], [300.0, 2200.0, 3000.0]];
    let v = VOWELS[vowel % 4];
    [(v[0], 90.0, 1.0), (v[1], 110.0, 0.6), (v[2], 160.0, 0.35)]
        .iter()
        .map(|(c, bw, g)| g / (1.0 + ((f - c) / bw).powi(2)))
        .sum::<f64>()
        + 0.02
}

struct Mix {
    x: Vec<f32>,
    rate: f64,
    /// Nothing sounds past this sample (the music's end).
    end: usize,
}

impl Mix {
    /// A pitched note from `table`: `env(tau)` shapes it, `vib` = (rate Hz, depth cents) wobbles it after 150 ms.
    fn tone(&mut self, at: f64, len: f64, freq: f64, table: &Table, amp: f64, env: impl Fn(f64) -> f64, vib: Option<(f64, f64)>) {
        let s = (at * self.rate).round().max(0.0) as usize;
        let n = (len * self.rate) as usize;
        let mut phase = 0.0;
        for i in 0..n {
            let k = s + i;
            if k >= self.end {
                break;
            }
            let tau = i as f64 / self.rate;
            let f = match vib {
                Some((r, c)) => freq * 2f64.powf(c / 1200.0 * (2.0 * PI * r * tau).sin() * (tau / 0.15).min(1.0)),
                None => freq,
            };
            phase += f / self.rate;
            self.x[k] += (amp * env(tau)) as f32 * table.at(phase);
        }
    }

    fn kick(&mut self, at: f64, vel: f64, rng: &mut Rng) {
        let s = (at * self.rate).round().max(0.0) as usize;
        let mut phase = 0.0;
        for i in 0..(0.45 * self.rate) as usize {
            let k = s + i;
            if k >= self.end {
                break;
            }
            let tau = i as f64 / self.rate;
            phase += (48.0 + 110.0 * (-tau / 0.035).exp()) / self.rate;
            let click = if tau < 0.002 { 0.3 * rng.signed() } else { 0.0 };
            self.x[k] += (vel * (0.9 * (-tau / 0.2).exp() * (2.0 * PI * phase).sin() + click)) as f32;
        }
    }

    fn snare(&mut self, at: f64, vel: f64, rng: &mut Rng) {
        let s = (at * self.rate).round().max(0.0) as usize;
        let mut prev = 0.0;
        for i in 0..(0.3 * self.rate) as usize {
            let k = s + i;
            if k >= self.end {
                break;
            }
            let tau = i as f64 / self.rate;
            let n = rng.signed();
            let hp = n - prev;
            prev = n;
            let v = 0.35 * hp * (-tau / 0.09).exp() + 0.4 * (2.0 * PI * 190.0 * tau).sin() * (-tau / 0.05).exp();
            self.x[k] += (vel * v) as f32;
        }
    }

    fn hat(&mut self, at: f64, vel: f64, rng: &mut Rng) {
        let s = (at * self.rate).round().max(0.0) as usize;
        let (mut p1, mut p2) = (0.0, 0.0);
        for i in 0..(0.1 * self.rate) as usize {
            let k = s + i;
            if k >= self.end {
                break;
            }
            let tau = i as f64 / self.rate;
            let n = rng.signed();
            let d1 = n - p1;
            let d2 = d1 - p2;
            (p1, p2) = (n, d1);
            self.x[k] += (vel * 0.12 * d2 * (-tau / 0.025).exp()) as f32;
        }
    }
}

fn midi_hz(midi: f64, cents: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0 + cents / 100.0) / 12.0)
}

/// Drum hits, bass notes and chord placement of one bar, positions in beats.
struct Pattern {
    kick: Vec<(f64, f64)>,
    snare: Vec<(f64, f64)>,
    hat: Vec<(f64, f64)>,
    /// (position, length in beats)
    bass: Vec<(f64, f64)>,
    /// Chord hits (position, length in beats); a pad is one hit per bar.
    chords: Vec<(f64, f64)>,
    piano: bool,
    /// Kicks some bars take instead of `kick`, picked at random; none when every bar is the same.
    other_kick: Vec<(f64, f64)>,
}

fn eighths(on: f64, off: f64) -> Vec<(f64, f64)> {
    (0..8).map(|i| (i as f64 * 0.5, if i % 2 == 0 { on } else { off })).collect()
}

fn pattern(style: Style) -> Pattern {
    let pad = vec![(0.0, 4.0)];
    match style {
        Style::House => Pattern {
            kick: (0..4).map(|b| (b as f64, 1.0)).collect(),
            snare: vec![(1.0, 0.6), (3.0, 0.6)],
            hat: (0..4).map(|b| (b as f64 + 0.5, 0.9)).collect(),
            bass: (0..4).map(|b| (b as f64 + 0.5, 0.4)).collect(),
            chords: pad,
            piano: false,
            other_kick: Vec::new(),
        },
        Style::Backbeat => Pattern {
            kick: vec![(0.0, 1.0), (2.0, 0.9), (2.5, 0.5)],
            snare: vec![(1.0, 1.0), (3.0, 1.0)],
            hat: eighths(0.6, 0.4),
            bass: vec![(0.0, 1.5), (2.0, 1.0), (3.0, 0.9)],
            chords: pad,
            piano: false,
            other_kick: Vec::new(),
        },
        Style::HalfTime => Pattern {
            kick: vec![(0.0, 1.0), (0.75, 0.5), (3.5, 0.4)],
            snare: vec![(2.0, 1.0)],
            hat: eighths(0.6, 0.4),
            bass: vec![(0.0, 1.5), (3.5, 0.5)],
            chords: pad,
            piano: false,
            other_kick: Vec::new(),
        },
        Style::DnB => Pattern {
            kick: vec![(0.0, 1.0), (2.5, 0.8)],
            snare: vec![(1.0, 1.0), (3.0, 1.0)],
            hat: eighths(0.5, 0.35),
            bass: vec![(0.0, 1.5), (2.5, 1.5)],
            chords: pad,
            piano: false,
            other_kick: Vec::new(),
        },
        Style::Funk => Pattern {
            kick: vec![(0.0, 1.0), (0.75, 0.6), (2.5, 0.8)],
            snare: vec![(1.0, 1.0), (3.0, 1.0), (1.75, 0.2), (3.25, 0.2)],
            hat: (0..16).map(|i| (i as f64 * 0.25, if i % 2 == 0 { 0.45 } else { 0.25 })).collect(),
            bass: vec![(0.0, 0.4), (0.75, 0.25), (1.5, 0.4), (2.5, 0.25), (3.25, 0.5)],
            chords: vec![(0.5, 0.25), (1.5, 0.25), (2.25, 0.25), (3.5, 0.25)],
            piano: false,
            other_kick: Vec::new(),
        },
        Style::OneDrop => Pattern {
            kick: vec![(2.0, 1.0)],
            snare: vec![(2.0, 0.7)],
            hat: (0..4).map(|b| (b as f64 + 0.5, 0.35)).collect(),
            bass: vec![(0.0, 0.5), (0.75, 0.5), (1.5, 1.0), (3.0, 0.5)],
            chords: (0..4).map(|b| (b as f64 + 0.5, 0.3)).collect(),
            piano: false,
            other_kick: Vec::new(),
        },
        Style::Waltz => Pattern {
            kick: vec![(0.0, 1.0)],
            snare: vec![(1.0, 0.3), (2.0, 0.3)],
            hat: Vec::new(),
            bass: vec![(0.0, 1.0)],
            chords: vec![(1.0, 0.6), (2.0, 0.6)],
            piano: false,
            other_kick: Vec::new(),
        },
        Style::Ballad => Pattern {
            kick: vec![(0.0, 0.6), (2.0, 0.4)],
            snare: vec![(1.0, 0.3), (3.0, 0.3)],
            hat: Vec::new(),
            bass: vec![(0.0, 2.0), (2.0, 2.0)],
            chords: (0..4).map(|b| (b as f64, 1.0)).collect(),
            piano: true,
            other_kick: Vec::new(),
        },
        Style::Broken => Pattern {
            kick: vec![(0.0, 1.0), (0.75, 0.7), (1.5, 0.8), (2.75, 0.6)],
            snare: vec![(1.0, 1.0), (3.0, 1.0), (2.25, 0.2), (3.75, 0.25)],
            hat: eighths(0.5, 0.3),
            bass: vec![(0.0, 0.5), (0.75, 0.5), (1.5, 0.75), (2.75, 0.5)],
            chords: pad,
            piano: false,
            other_kick: vec![(0.0, 1.0), (1.5, 0.8), (2.25, 0.6), (3.5, 0.7)],
        },
    }
}

/// Chords of a progression as (semitones above the tonic, minor), one per bar.
fn progression(minor: bool, which: usize) -> [(usize, bool); 4] {
    match (minor, which % 2) {
        (false, 0) => [(0, false), (7, false), (9, true), (5, false)], // I V vi IV
        (false, _) => [(0, false), (5, false), (7, false), (0, false)], // I IV V I
        (true, 0) => [(0, true), (8, false), (10, false), (0, true)],  // i VI VII i
        (true, _) => [(0, true), (5, true), (7, false), (0, true)],    // i iv V i
    }
}

impl Song {
    fn tempo_at(&self, x: f64, t: f64) -> f64 {
        let base = if self.end_bpm > 0.0 { self.bpm + (self.end_bpm - self.bpm) * x } else { self.bpm };
        base * (1.0 + self.wander_pct / 100.0 * (2.0 * PI * t / 37.0).sin())
    }

    pub fn truth_beats(&self) -> Vec<f64> {
        let bars: usize = self.sections.iter().map(|s| s.0).sum();
        let n = bars * self.meter;
        let first = self.lead_silence + self.ambient_s + 0.15;
        let mut out = Vec::with_capacity(n + 1);
        let mut t = first;
        for k in 0..=n {
            out.push(t);
            t += 60.0 / self.tempo_at(k as f64 / n as f64, t - first);
        }
        out
    }

    /// Renders the song and says what it is.
    pub fn render(&self) -> (Vec<f32>, Truth) {
        let rate = self.rate as f64;
        let all = self.truth_beats();
        // The last entry is where the next bar would start: the music ends there.
        let music_end = *all.last().unwrap();
        let beats = &all[..all.len() - 1];
        let total = ((music_end + self.tail_silence) * rate) as usize;
        let mut m = Mix { x: vec![0f32; total], rate, end: (music_end * rate) as usize };
        let mut rng = Rng(self.seed);
        let mut hits = Rng(self.seed ^ 0xA5A5_A5A5);
        let p = pattern(self.style);
        let meter = self.meter as f64;
        let chords = progression(self.minor, self.progression);
        let scale: [usize; 7] = if self.minor { [0, 2, 3, 5, 7, 8, 10] } else { [0, 2, 4, 5, 7, 9, 11] };
        let pad = Table::new(&[1.0, 0.45, 0.3, 0.2, 0.12, 0.08]);
        let piano = Table::new(&[1.0, 0.6, 0.35, 0.22, 0.14, 0.09, 0.06]);
        let bass = Table::new(&[1.0, 0.55, 0.3, 0.18, 0.1]);
        // Time of a position in beats from beat `i`, following the (possibly drifting) grid.
        let at = |i: usize, pos: f64| -> f64 {
            let whole = pos.floor() as usize;
            let frac = pos - pos.floor();
            let k = (i + whole).min(all.len() - 2);
            all[k] + frac * (all[k + 1] - all[k])
        };
        let swing = |pos: f64| -> f64 {
            let f = pos - pos.floor();
            if (f - 0.5).abs() < 1e-9 {
                pos.floor() + self.swing
            } else {
                pos
            }
        };
        let jitter = self.jitter_ms / 1000.0;

        // Ambient intro: a slow swell of the tonic chord, no pulse at all.
        if self.ambient_s > 0.0 {
            let (root, mi) = chords[0];
            let start = self.lead_silence;
            let len = self.ambient_s + 1.0;
            for (j, iv) in [0, if mi { 3 } else { 4 }, 7, 12].iter().enumerate() {
                let f = midi_hz((48 + self.tonic + root + iv + if j > 0 { 12 } else { 0 }) as f64, self.cents);
                let swell = self.ambient_s;
                m.tone(start, len, f, &pad, 0.07, |tau| (tau / (0.4 * swell)).min(1.0) * (0.75 + 0.25 * (2.0 * PI * tau / 7.3).sin()), None);
            }
        }

        let mut voice = Vec::new();
        let mut bar = 0usize;
        let mut melody = 7usize; // scale index into two octaves
        for (bars, layers) in &self.sections {
            for _ in 0..*bars {
                let b0 = bar * self.meter;
                let (root, mi) = chords[bar % 4];
                let root_pc = self.tonic + root;
                let bar_len = beats.get(b0 + self.meter).copied().unwrap_or(music_end) - beats[b0];
                if layers.drums {
                    let kick = if !p.other_kick.is_empty() && rng.unit() < 0.4 { &p.other_kick } else { &p.kick };
                    for &(pos, v) in kick {
                        m.kick(at(b0, pos) + jitter * hits.gauss(), v, &mut rng);
                    }
                    for &(pos, v) in &p.snare {
                        m.snare(at(b0, pos) + jitter * hits.gauss(), 0.8 * v, &mut rng);
                    }
                    for &(pos, v) in &p.hat {
                        m.hat(at(b0, swing(pos)) + jitter * hits.gauss(), v, &mut rng);
                    }
                }
                if layers.bass {
                    for &(pos, len) in &p.bass {
                        let t = at(b0, swing(pos)) + jitter * hits.gauss();
                        let dur = len * bar_len / meter;
                        let f = midi_hz((36 + root_pc) as f64, self.cents);
                        m.tone(t, dur, f, &bass, 0.28, |tau| (tau / 0.005).min(1.0) * (-tau / 0.6).exp() * ((dur - tau) / 0.03).clamp(0.0, 1.0), None);
                    }
                }
                if layers.chords {
                    let third = if mi { 3 } else { 4 };
                    let notes = [48 + root_pc, 60 + root_pc + third, 60 + root_pc + 7, 72 + root_pc];
                    for &(pos, len) in &p.chords {
                        let t = at(b0, swing(pos)) + jitter * hits.gauss();
                        let pad_bar = len >= meter;
                        let dur = if pad_bar { bar_len } else { len * bar_len / meter + if p.piano { 0.8 } else { 0.0 } };
                        for &n in &notes {
                            let f = midi_hz(n as f64, self.cents);
                            if p.piano {
                                m.tone(t, dur, f, &piano, 0.06, |tau| (tau / 0.003).min(1.0) * (-tau / 0.9).exp(), None);
                            } else if pad_bar {
                                m.tone(t, dur, f, &pad, 0.045, |tau| (tau / 0.04).min(1.0) * ((dur - tau) / 0.06).clamp(0.0, 1.0), None);
                            } else {
                                m.tone(t, dur, f, &pad, 0.07, |tau| (tau / 0.004).min(1.0) * (-tau / 0.12).exp(), None);
                            }
                        }
                    }
                }
                // A sung line: three bars of every four, notes of one or two beats wandering the scale.
                if layers.voice && bar % 4 != 3 {
                    let mut pos = 0.0;
                    while pos < meter - 0.01 {
                        let len = if rng.unit() < 0.5 || pos + 2.0 > meter { 1.0 } else { 2.0 };
                        let step = (rng.unit() * 5.0) as i64 - 2;
                        melody = (melody as i64 + step).clamp(3, 12) as usize;
                        let midi = 60 + self.tonic % 12 + scale[melody % 7] + 12 * (melody / 7) - 12;
                        let f0 = midi_hz(midi as f64, self.cents);
                        let vowel = (rng.unit() * 4.0) as usize;
                        let harm: Vec<f64> = (1..=24).take_while(|h| *h as f64 * f0 < 5000.0).map(|h| formant(h as f64 * f0, vowel) / h as f64).collect();
                        let table = Table::new(&harm);
                        let t = at(b0, pos) + 0.02;
                        let dur = len * bar_len / meter - 0.04;
                        m.tone(t, dur, f0, &table, 0.12, |tau| (tau / 0.06).min(1.0) * ((dur - tau) / 0.08).clamp(0.0, 1.0), Some((5.5, 35.0)));
                        voice.push((t, t + dur));
                        pos += len;
                    }
                }
                bar += 1;
            }
        }
        // Where each section starts and ends, and what plays in it.
        let mut spans: Vec<(f64, f64, Layers)> = Vec::new();
        let mut b = 0;
        for (bars, l) in &self.sections {
            spans.push((all[b * self.meter], all[(b + bars) * self.meter], *l));
            b += bars;
        }
        let silent = |l: &Layers| !(l.drums || l.bass || l.chords || l.voice);
        if self.noise > 0.0 {
            let (a, b) = ((self.lead_silence * rate) as usize, m.end.min(total));
            for (k, v) in m.x[a..b].iter_mut().enumerate() {
                let n = self.noise * rng.signed();
                if self.quiet_gaps {
                    let t = (a + k) as f64 / rate;
                    if spans.iter().any(|(s0, s1, l)| silent(l) && t >= *s0 && t < *s1) {
                        continue;
                    }
                }
                *v += n as f32;
            }
        }
        let peak = m.x.iter().fold(0f32, |p, v| p.max(v.abs())).max(1e-9);
        let x: Vec<f32> = m.x.iter().map(|v| v * 0.8 / peak).collect();

        // Structure. The intro ends at the first section that brings something in (drums, bass, chords or a
        // voice) within the first 40 % of the song, or where the beat starts after a beatless opening; the
        // outro starts at the last section that takes something away in the second half. A song that never
        // changes has neither: its intro "ends" where it starts, and any 8-bar line will do as an outro.
        let mask = |l: &Layers| [l.drums, l.bass, l.chords, l.voice];
        let adds = |a: &Layers, b: &Layers| mask(a).iter().zip(mask(b)).any(|(x, y)| !x && y);
        let mut starts = Vec::new();
        let mut b = 0;
        for (bars, l) in &self.sections {
            starts.push((beats[b * self.meter], *l));
            b += bars;
        }
        let music0 = self.lead_silence;
        let len = music_end - music0;
        let intro_end = if self.ambient_s > 0.0 {
            beats[0]
        } else {
            starts.windows(2).find(|w| adds(&w[0].1, &w[1].1) && w[1].0 - music0 <= 0.4 * len).map_or(music0, |w| w[1].0)
        };
        let outro_start = starts.windows(2).rev().find(|w| adds(&w[1].1, &w[0].1) && w[1].0 - music0 >= 0.5 * len).map(|w| w[1].0);
        let full = |l: &Layers| l.drums && l.bass && l.chords;
        let drop = spans
            .iter()
            .enumerate()
            .find(|(i, (s0, _, l))| {
                let before_full = if *i == 0 { self.ambient_s <= 0.0 } else { full(&spans[i - 1].2) };
                full(l) && !before_full && s0 - music0 <= 0.45 * len && (*i > 0 || self.ambient_s > 0.0)
            })
            .map(|(_, (s0, _, _))| *s0);
        let gaps: Vec<(f64, f64)> = spans.iter().filter(|(_, _, l)| silent(l)).map(|(a, b, _)| (*a, *b)).collect();
        let music_after = |t: f64| spans.iter().filter(|(_, _, l)| !silent(l)).map(|(a, b, _)| (b - a.max(t)).max(0.0)).sum::<f64>();
        let gap_exit = gaps.iter().rev().find(|(a, b)| b - a >= 6.0 && music_after(*b) <= 15.0).map(|(a, _)| *a);
        let breakdown = {
            let mut i = spans.len();
            while i > 0 && !spans[i - 1].2.drums && !silent(&spans[i - 1].2) {
                i -= 1;
            }
            (i < spans.len() && i > 0 && spans[i - 1].2.drums && music_end - spans[i].0 <= 24.0).then(|| spans[i].0)
        };
        let exit = gap_exit.or(breakdown);
        let downbeats = beats.iter().step_by(self.meter).copied().collect();
        let truth = Truth {
            beats: beats.to_vec(),
            downbeats,
            meter: self.meter,
            bpm: median_bpm(beats),
            key: camelot(self.tonic % 12, self.minor),
            voice,
            music: (music0, music_end),
            intro_end,
            outro_start,
            drop,
            exit,
            gaps,
        };
        (x, truth)
    }
}

/// The songs the harness scores.
pub fn corpus() -> Vec<Song> {
    let only = std::env::var("NORI_EVAL_ONLY").unwrap_or_default();
    let all = corpus_all();
    all.into_iter().filter(|s| only.is_empty() || only.split(",").any(|o| s.name.starts_with(o))).collect()
}

pub fn corpus_all() -> Vec<Song> {
    let s = Song::new;
    vec![
        Song { sections: vec![(16, DRUMS), (16, FULL), (16, SUNG), (16, FULL), (16, DRUMS)], ..s("house-124", Style::House, 124.0, 9, true) },
        Song { swing: 0.62, sections: vec![(4, FULL), (16, SUNG), (8, FULL), (16, SUNG), (8, SUNG)], ..s("pop-swing-96", Style::Backbeat, 96.0, 2, false) },
        Song { wander_pct: 2.0, jitter_ms: 12.0, progression: 1, sections: vec![(4, FULL), (24, SUNG), (16, FULL), (24, SUNG), (8, FULL)], ..s("band-drift-118", Style::Backbeat, 118.0, 4, false) },
        Song { end_bpm: 88.0, jitter_ms: 15.0, sections: vec![(8, FULL), (24, SUNG), (24, FULL), (8, SUNG)], ..s("band-slows-96", Style::Backbeat, 96.0, 6, true) },
        Song { end_bpm: 132.0, progression: 1, sections: vec![(8, FULL), (48, FULL), (8, FULL)], ..s("accelerando-100", Style::Backbeat, 100.0, 7, false) },
        Song { sections: vec![(8, DRUMS), (32, FULL), (16, FULL), (8, DRUMS)], ..s("halftime-140", Style::HalfTime, 140.0, 5, true) },
        Song { progression: 1, sections: vec![(16, DRUMS), (48, FULL), (16, FULL), (16, DRUMS)], ..s("dnb-174", Style::DnB, 174.0, 0, true) },
        Song { swing: 0.5, sections: vec![(4, FULL), (32, FULL), (16, FULL)], ..s("funk-104", Style::Funk, 104.0, 10, false) },
        Song { sections: vec![(4, FULL), (16, SUNG), (16, FULL), (8, FULL)], ..s("one-drop-75", Style::OneDrop, 75.0, 9, false) },
        Song { sections: vec![(8, FULL), (32, FULL), (16, FULL), (8, FULL)], ..s("waltz-150", Style::Waltz, 150.0, 5, false) },
        Song { ambient_s: 45.0, sections: vec![(32, FULL), (16, SUNG), (16, FULL), (16, DRUMS)], ..s("ambient-intro-122", Style::House, 122.0, 3, false) },
        Song {
            lead_silence: 3.0,
            tail_silence: 6.0,
            sections: vec![(16, FULL), (24, FULL), (2, SILENT), (24, FULL), (8, DRUMS)],
            ..s("silences-128", Style::House, 128.0, 11, true)
        },
        Song { jitter_ms: 8.0, sections: vec![(8, KEYS_SUNG), (16, SUNG), (16, SUNG), (8, KEYS)], ..s("ballad-72", Style::Ballad, 72.0, 0, false) },
        Song { cents: 38.0, progression: 1, sections: vec![(16, DRUMS), (48, FULL), (16, DRUMS)], ..s("techno-detuned-130", Style::House, 130.0, 7, true) },
        Song { cents: -30.0, sections: vec![(4, FULL), (32, FULL), (16, SUNG), (8, FULL)], ..s("rock-detuned-140", Style::Backbeat, 140.0, 9, false) },
        Song { swing: 0.66, jitter_ms: 10.0, progression: 1, sections: vec![(4, KEYS), (24, SUNG), (24, FULL), (8, FULL)], ..s("shuffle-84", Style::Backbeat, 84.0, 2, true) },
        Song { swing: 0.58, sections: vec![(8, DRUMS), (32, FULL), (16, SUNG), (8, FULL)], ..s("broken-128", Style::Broken, 128.0, 4, true) },
        Song { jitter_ms: 8.0, sections: vec![(4, DRUMS), (24, SUNG), (16, FULL), (8, FULL)], ..s("broken-band-126", Style::Broken, 126.0, 9, false) },
    ]
}

/// Beat F-measure at ±70 ms: each reference beat matches at most one estimate. 1 when both are empty.
pub fn f_measure(reference: &[f64], estimate: &[f64]) -> f64 {
    if reference.is_empty() && estimate.is_empty() {
        return 1.0;
    }
    if reference.is_empty() || estimate.is_empty() {
        return 0.0;
    }
    let mut hits = 0usize;
    let mut j = 0usize;
    for r in reference {
        while j < estimate.len() && estimate[j] < r - TOL {
            j += 1;
        }
        if j < estimate.len() && (estimate[j] - r).abs() <= TOL {
            hits += 1;
            j += 1;
        }
    }
    let p = hits as f64 / estimate.len() as f64;
    let r = hits as f64 / reference.len() as f64;
    if p + r == 0.0 {
        0.0
    } else {
        2.0 * p * r / (p + r)
    }
}

pub fn acc1(est: f64, reference: f64) -> bool {
    reference > 0.0 && (est / reference - 1.0).abs() <= 0.04
}

pub fn acc2(est: f64, reference: f64) -> bool {
    [1.0, 2.0, 0.5, 3.0, 1.0 / 3.0].iter().any(|k| acc1(est, reference * k))
}

/// Camelot code back to (tonic pitch class, minor).
fn tonic_of(code: i32) -> Option<(usize, bool)> {
    (0..12).flat_map(|t| [(t, false), (t, true)]).find(|(t, m)| camelot(*t, *m) == code)
}

/// MIREX key score: 1 same key, 0.5 a fifth away in the same mode, 0.3 relative, 0.2 parallel, else 0.
pub fn mirex_key(est: i32, reference: i32) -> f64 {
    let (Some((te, me)), Some((tr, mr))) = (tonic_of(est), tonic_of(reference)) else { return 0.0 };
    if te == tr && me == mr {
        1.0
    } else if me == mr && ((te + 12 - tr) % 12 == 7 || (tr + 12 - te) % 12 == 7) {
        0.5
    } else if me != mr && ((!mr && te == (tr + 9) % 12) || (mr && te == (tr + 3) % 12)) {
        0.3
    } else if te == tr {
        0.2
    } else {
        0.0
    }
}

/// Beats per bar the analysis claims for a track.
pub fn bar_beats(t: &TrackAnalysis) -> i64 {
    super::plan::bar_beats(t)
}

/// The grid of `t` (as the planner would see it) over `[from, to)`: beats and downbeats.
pub fn grid(t: &TrackAnalysis, from: f64, to: f64) -> (Vec<f64>, Vec<f64>) {
    if !(t.bpm > 0.0 && t.bpm.is_finite()) {
        return (Vec::new(), Vec::new());
    }
    let period = 60.0 / t.bpm;
    let offset = t.beat_offset_ms / 1000.0;
    let bpb = bar_beats(t);
    let first = ((from - offset) / period).ceil() as i64;
    let (mut beats, mut downs) = (Vec::new(), Vec::new());
    let mut n = first;
    loop {
        let b = offset + n as f64 * period;
        if b >= to {
            break;
        }
        beats.push(b);
        if n.rem_euclid(bpb) == t.downbeat_phase.rem_euclid(bpb as i32) as i64 {
            downs.push(b);
        }
        n += 1;
    }
    (beats, downs)
}

/// How one mix window came out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// Trusted, and beats and downbeats are right: a clean beat-matched mix.
    BarLocked,
    /// Trusted, beats right, the bar starts on the wrong beat.
    WrongBar,
    /// Trusted and wrong: a train wreck.
    Wrong,
    /// Not trusted: the planner falls back to a fade. Right or wrong, it costs only the mix.
    Refused,
}

#[derive(Clone, Copy, Debug)]
pub struct WindowScore {
    pub beat_f: f64,
    pub downbeat_f: f64,
    pub acc1: bool,
    pub acc2: bool,
    pub outcome: Outcome,
    /// Whether the grid is right, trusted or not.
    pub right: bool,
}

/// Beat F-measure at the annotated metrical level or at half or double of it, on the beat (never off it): a grid
/// at half or double tempo still lines two songs up, because the planner folds tempos by octaves.
pub fn metrical_f(reference: &[f64], estimate: &[f64]) -> f64 {
    let half_a: Vec<f64> = reference.iter().step_by(2).copied().collect();
    let half_b: Vec<f64> = reference.iter().skip(1).step_by(2).copied().collect();
    let mut double: Vec<f64> = reference.windows(2).flat_map(|w| [w[0], 0.5 * (w[0] + w[1])]).collect();
    double.extend(reference.last());
    [reference, &half_a, &half_b, &double].iter().map(|r| f_measure(r, estimate)).fold(0.0, f64::max)
}

/// Share of `estimate` within the tolerance of some reference time: a claimed downbeat must be a real one.
pub fn precision(reference: &[f64], estimate: &[f64]) -> f64 {
    if estimate.is_empty() {
        return 0.0;
    }
    estimate.iter().filter(|e| reference.iter().any(|r| (*e - r).abs() <= TOL)).count() as f64 / estimate.len() as f64
}

pub fn score_window(t: &TrackAnalysis, truth: &Truth, from: f64, to: f64) -> Option<WindowScore> {
    let rb = truth.beats_in(from, to);
    if rb.len() < 8 {
        return None;
    }
    // Only where the song has beats: a grid carried on through a ringing last chord is not wrong.
    let half = 30.0 / median_bpm(&rb).max(1.0);
    let (eb, ed) = grid(t, from.max(rb[0] - half), to.min(rb[rb.len() - 1] + half));
    let rd = truth.downbeats_in(from, to);
    let beat_f = f_measure(&rb, &eb);
    let downbeat_f = f_measure(&rd, &ed);
    let local = median_bpm(&rb);
    let right_beats = metrical_f(&rb, &eb) >= 0.9;
    let right = right_beats && precision(&rd, &ed) >= 0.9;
    let outcome = if !grid_ok(t) {
        Outcome::Refused
    } else if right {
        Outcome::BarLocked
    } else if right_beats {
        Outcome::WrongBar
    } else {
        Outcome::Wrong
    };
    Some(WindowScore { beat_f, downbeat_f, acc1: acc1(t.bpm, local), acc2: acc2(t.bpm, local), outcome, right })
}

/// Everything measured on one song.
pub struct SongScore {
    pub name: String,
    /// F of the tracked beats, and of the whole-song grid, after the first 5 s of music.
    pub tracked_f: f64,
    pub grid_f: f64,
    pub acc1: bool,
    pub acc2: bool,
    pub intro: Option<WindowScore>,
    pub outro: Option<WindowScore>,
    pub key_ok: Option<bool>,
    pub key_mirex: Option<f64>,
    /// At most one step from the true key on the Camelot wheel (relative or a fifth away): what the planner
    /// treats as compatible anyway. A key further off can make it refuse a good pair or blend a clashing one.
    pub key_near: Option<bool>,
    /// (truth, claimed) for the incoming and outgoing overlap windows.
    pub vocal: Vec<(bool, bool)>,
    pub intro_cue_ok: Option<bool>,
    pub outro_cue_ok: Option<bool>,
    pub meter_ok: bool,
    /// A line of what was measured against what is true, for `NORI_EVAL_VERBOSE`.
    pub detail: Option<String>,
}

pub fn score(name: &str, a: &Analysis, truth: &Truth, with_key: bool) -> SongScore {
    let t = &a.track;
    let music = (t.silence_start_ms as f64 / 1000.0, t.silence_end_ms as f64 / 1000.0);
    let from = truth.beats.first().copied().unwrap_or(0.0).max(truth.music.0) + 5.0;
    let rb = truth.beats_in(from, f64::MAX);
    let tracked: Vec<f64> = a.tempo.beats.iter().copied().filter(|b| *b >= from).collect();
    let (gb, _) = grid(t, from, truth.music.1 + 1.0);
    let intro_t = at_start(t);
    let outro_t = at_end(t);
    let first_beat = truth.beats.first().copied().unwrap_or(truth.music.0);
    let intro = score_window(&intro_t, truth, first_beat.max(music.0), first_beat.max(music.0) + MIX_S);
    let outro = score_window(&outro_t, truth, music.1 - MIX_S, music.1);
    let beat = 60.0 / truth.bpm.max(1.0);
    let intro_cue_ok = Some((t.intro_end_ms as f64 / 1000.0 - truth.intro_end).abs() <= beat);
    let outro_cue = t.outro_start_ms as f64 / 1000.0;
    let outro_cue_ok = Some(match truth.outro_start {
        Some(o) => (outro_cue - o).abs() <= beat,
        // A steady ending: any 8-bar line will do.
        None => truth
            .downbeats
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 8 == 0)
            .any(|(_, d)| (outro_cue - d).abs() <= beat),
    });
    let intro_w = (music.0, (t.intro_end_ms as f64 / 1000.0).max(music.0 + 1.0));
    let outro_w = ((t.outro_start_ms as f64 / 1000.0).min(music.1 - 1.0), music.1);
    let vocal = vec![
        (truth.voice_share(intro_w.0, intro_w.1) >= 0.3, t.intro_vocal >= VOCAL_MIN),
        (truth.voice_share(outro_w.0, outro_w.1) >= 0.3, t.outro_vocal >= VOCAL_MIN),
    ];
    SongScore {
        name: name.to_string(),
        tracked_f: f_measure(&rb, &tracked),
        grid_f: f_measure(&rb, &gb),
        acc1: acc1(t.bpm, truth.bpm),
        acc2: acc2(t.bpm, truth.bpm),
        intro,
        outro,
        key_ok: with_key.then_some(t.key == truth.key),
        key_mirex: with_key.then(|| mirex_key(t.key, truth.key)),
        key_near: with_key.then(|| (0..=1).contains(&structure::key_distance(t.key, truth.key))),
        vocal,
        intro_cue_ok,
        outro_cue_ok,
        meter_ok: bar_beats(t) == truth.meter as i64,
        detail: Some(format!(
            "bpm {:.2} (true {:.2}) conf {:.2} stab {:.2} | intro {:.2} c{:.2} s{:.2} ph{} | outro {:.2} c{:.2} s{:.2} ph{} (true {:.2}) | key {} (true {}) {:.2} | cues {:.1}/{} (true {:.1}/{}) | vocal {:.2}/{:.2}",
            t.bpm, truth.bpm, t.bpm_confidence, t.stability,
            intro_t.bpm, intro_t.bpm_confidence, intro_t.stability, intro_t.downbeat_phase,
            outro_t.bpm, outro_t.bpm_confidence, outro_t.stability, outro_t.downbeat_phase,
            median_bpm(&truth.beats_in(music.1 - MIX_S, music.1)),
            structure::camelot_name(t.key), structure::camelot_name(truth.key), t.key_confidence,
            t.intro_end_ms as f64 / 1000.0, outro_cue, truth.intro_end, truth.outro_start.map_or("-".into(), |o| format!("{o:.1}")),
            t.intro_vocal, t.outro_vocal,
        )),
    }
}

fn mark(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn outcome_name(w: &Option<WindowScore>) -> String {
    match w {
        None => "-".into(),
        Some(w) => format!(
            "{:.2}/{:.2} {}",
            w.beat_f,
            w.downbeat_f,
            match w.outcome {
                Outcome::BarLocked => "bar",
                Outcome::WrongBar => "WRONG-BAR",
                Outcome::Wrong => "WRONG",
                Outcome::Refused => if w.right { "refused(ok)" } else { "refused" },
            }
        ),
    }
}

/// The totals the report quotes.
#[derive(Debug, Default)]
pub struct Totals {
    pub songs: usize,
    pub tracked_f: f64,
    pub grid_f: f64,
    pub acc1: usize,
    pub acc2: usize,
    pub windows: usize,
    pub window_f: f64,
    pub window_db_f: f64,
    pub window_acc1: usize,
    pub window_acc2: usize,
    pub bar_locked: usize,
    pub wrong_bar: usize,
    pub wrong: usize,
    pub refused: usize,
    pub keys: usize,
    pub key_ok: usize,
    pub key_mirex: f64,
    pub key_near: usize,
    pub vocal_windows: usize,
    pub vocal_ok: usize,
    pub vocal_false_alarm: usize,
    pub cues: usize,
    pub cue_ok: usize,
    pub meter_ok: usize,
}

pub fn report(scores: &[SongScore]) -> Totals {
    println!(
        "{:<20} {:>6} {:>6} {:>5} {:>5}  {:<22} {:<22} {:>4} {:>5} {:>9} {:>5}",
        "song", "trkF", "gridF", "acc1", "acc2", "intro F/dbF", "outro F/dbF", "key", "mirex", "vocal", "cues"
    );
    let mut tot = Totals::default();
    for s in scores {
        tot.songs += 1;
        tot.tracked_f += s.tracked_f;
        tot.grid_f += s.grid_f;
        tot.acc1 += s.acc1 as usize;
        tot.acc2 += s.acc2 as usize;
        tot.meter_ok += s.meter_ok as usize;
        for w in [&s.intro, &s.outro].into_iter().flatten() {
            tot.windows += 1;
            tot.window_f += w.beat_f;
            tot.window_db_f += w.downbeat_f;
            tot.window_acc1 += w.acc1 as usize;
            tot.window_acc2 += w.acc2 as usize;
            match w.outcome {
                Outcome::BarLocked => tot.bar_locked += 1,
                Outcome::WrongBar => tot.wrong_bar += 1,
                Outcome::Wrong => tot.wrong += 1,
                Outcome::Refused => tot.refused += 1,
            }
        }
        if let (Some(ok), Some(m)) = (s.key_ok, s.key_mirex) {
            tot.keys += 1;
            tot.key_ok += ok as usize;
            tot.key_mirex += m;
            tot.key_near += s.key_near.unwrap_or(false) as usize;
        }
        for (truth, claim) in &s.vocal {
            tot.vocal_windows += 1;
            tot.vocal_ok += (truth == claim) as usize;
            tot.vocal_false_alarm += (!truth && *claim) as usize;
        }
        for c in [s.intro_cue_ok, s.outro_cue_ok].into_iter().flatten() {
            tot.cues += 1;
            tot.cue_ok += c as usize;
        }
        if let Some(d) = &s.detail {
            if std::env::var("NORI_EVAL_VERBOSE").is_ok() {
                println!("    {d}");
            }
        }
        let vocal = s.vocal.iter().map(|(t, c)| if t == c { "ok" } else if *c { "FA" } else { "miss" }).collect::<Vec<_>>().join("/");
        println!(
            "{:<20} {:>6.3} {:>6.3} {:>5} {:>5}  {:<22} {:<22} {:>4} {:>5} {:>9} {:>5}",
            s.name,
            s.tracked_f,
            s.grid_f,
            mark(s.acc1),
            mark(s.acc2),
            outcome_name(&s.intro),
            outcome_name(&s.outro),
            s.key_ok.map_or("-", mark),
            s.key_mirex.map_or("-".into(), |m| format!("{m:.1}")),
            vocal,
            format!("{}/{}", s.intro_cue_ok.map_or("-", mark), s.outro_cue_ok.map_or("-", mark)),
        );
    }
    let n = tot.songs.max(1) as f64;
    let w = tot.windows.max(1) as f64;
    println!(
        "TOTAL {} songs: tracked beat F {:.3}, whole-grid F {:.3}, tempo Acc1 {}/{} Acc2 {}/{}, metre {}/{}",
        tot.songs, tot.tracked_f / n, tot.grid_f / n, tot.acc1, tot.songs, tot.acc2, tot.songs, tot.meter_ok, tot.songs
    );
    println!(
        "      {} mix windows: grid F {:.3}, downbeat F {:.3}, local tempo Acc1 {} Acc2 {}; bar-locked {}, wrong bar {}, wrong {}, refused {}",
        tot.windows, tot.window_f / w, tot.window_db_f / w, tot.window_acc1, tot.window_acc2, tot.bar_locked, tot.wrong_bar, tot.wrong, tot.refused
    );
    println!(
        "      key {}/{} (MIREX {:.3}, within one Camelot step {}/{}), vocal gate {}/{} ({} false alarms), cues {}/{}",
        tot.key_ok,
        tot.keys,
        tot.key_mirex / tot.keys.max(1) as f64,
        tot.key_near,
        tot.keys,
        tot.vocal_ok,
        tot.vocal_windows,
        tot.vocal_false_alarm,
        tot.cue_ok,
        tot.cues
    );
    tot
}

fn write_wav(path: &std::path::Path, x: &[f32], rate: u32) {
    let mut out = Vec::with_capacity(44 + x.len() * 2);
    let data = (x.len() * 2) as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for v in x {
        out.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    std::fs::write(path, out).unwrap();
}

fn write_times(path: &std::path::Path, t: &[f64]) {
    std::fs::write(path, t.iter().map(|v| format!("{v:.4}\n")).collect::<String>()).unwrap();
}

/// Renders, analyses and scores the synthetic corpus. Returns the scores and the analysis time per minute of audio.
pub fn run_corpus(songs: &[Song]) -> (Vec<SongScore>, f64) {
    let dump = std::env::var("NORI_EVAL_DUMP").ok().map(std::path::PathBuf::from);
    let mut scores = Vec::new();
    let (mut spent, mut audio) = (0.0, 0.0);
    for song in songs {
        let (x, truth) = song.render();
        if let Some(dir) = &dump {
            std::fs::create_dir_all(dir).unwrap();
            write_wav(&dir.join(format!("{}.wav", song.name)), &x, song.rate);
            write_times(&dir.join(format!("{}.beats", song.name)), &truth.beats);
            write_times(&dir.join(format!("{}.downbeats", song.name)), &truth.downbeats);
        }
        let t0 = std::time::Instant::now();
        let a = analyse(song.name, &x, song.rate);
        spent += t0.elapsed().as_secs_f64();
        audio += x.len() as f64 / song.rate as f64;
        scores.push(score(song.name, &a, &truth, true));
    }
    (scores, spent / (audio / 60.0))
}

/// One real recording from `NORI_REAL`, with its reference beats.
pub struct RealSong {
    pub name: String,
    pub x: Vec<f32>,
    pub rate: u32,
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    /// Camelot code from an optional `<name>.key` ("9A"), 0 when there is none.
    pub key: i32,
}

impl RealSong {
    /// The reference as the scores read it; where the music starts and ends is taken from the analysis.
    pub fn truth(&self, t: &TrackAnalysis) -> Truth {
        let music = (t.silence_start_ms as f64 / 1000.0, t.silence_end_ms as f64 / 1000.0);
        Truth {
            bpm: median_bpm(&self.beats),
            meter: 4,
            key: self.key,
            voice: Vec::new(),
            music,
            intro_end: music.0,
            outro_start: None,
            drop: None,
            exit: None,
            gaps: Vec::new(),
            beats: self.beats.clone(),
            downbeats: self.downbeats.clone(),
        }
    }
}

/// The recordings in `NORI_REAL`, by name; none when it is not set.
pub fn real_songs() -> Vec<RealSong> {
    let Ok(dir) = std::env::var("NORI_REAL") else { return Vec::new() };
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok()?.strip_suffix(".s16").map(str::to_string))
        .collect();
    names.sort();
    let dir = std::path::PathBuf::from(dir);
    let read_times = |p: std::path::PathBuf| -> Vec<f64> {
        std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| l.split_whitespace().next()?.parse().ok()).collect()
    };
    names
        .into_iter()
        .map(|name| {
            let bytes = std::fs::read(dir.join(format!("{name}.s16"))).unwrap();
            let rate: u32 = std::fs::read_to_string(dir.join(format!("{name}.rate"))).unwrap().trim().parse().unwrap();
            let x: Vec<f32> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
            let key = std::fs::read_to_string(dir.join(format!("{name}.key")))
                .ok()
                .and_then(|k| (1..=24).find(|c| structure::camelot_name(*c) == k.trim()))
                .unwrap_or(0);
            RealSong {
                beats: read_times(dir.join(format!("{name}.beats"))),
                downbeats: read_times(dir.join(format!("{name}.downbeats"))),
                name,
                x,
                rate,
                key,
            }
        })
        .collect()
}

/// `cargo test --release -p nori-player analysis_eval -- --ignored --nocapture`
#[test]
#[ignore]
fn analysis_eval() {
    let (scores, per_min) = run_corpus(&corpus());
    report(&scores);
    println!("analysis time: {:.1} ms per minute of audio (this machine, one core)", per_min * 1000.0);

    // Real recordings with reference beats, when there are any.
    let mut real = Vec::new();
    for r in real_songs() {
        let (name, x, rate) = (&r.name, &r.x, r.rate);
        let a = analyse(name, x, rate);
        let truth = r.truth(&a.track);
        let s = score(name, &a, &truth, r.key > 0);
        println!(
            "{name}: reference {:.1} BPM, analysis {:.2} BPM (conf {:.2}, stab {:.2}); intro {:.1} outro {:.1}",
            truth.bpm, a.track.bpm, a.track.bpm_confidence, a.track.stability, a.track.intro_bpm, a.track.outro_bpm
        );
        real.push(SongScore { intro_cue_ok: None, outro_cue_ok: None, vocal: Vec::new(), ..s });
    }
    if !real.is_empty() {
        println!("real recordings, against the reference beats:");
        report(&real);
    }
}

/// The songs again with Beat This! run over each end as the app runs it (`beats::window`, `track_window`, the grid
/// and `beats::merge`), everything else (silence, cues, key) as analysed: what the model changes in the mix windows.
/// Scored twice: as shipped (its grid only where it is sure, the classical one kept elsewhere) and with its grid
/// wherever it has one. With `NORI_BEAT_THIS_REF` (another model, normally the full fp32 one) it also prints how
/// far the two agree on each window's beats and downbeats, which is how an export or a smaller model is checked.
/// Real recordings come from `NORI_REAL` as for `analysis_eval`; `NORI_BT_CHUNK` runs the model in shorter chunks.
/// `NORI_BEAT_THIS=<model.onnx> [NORI_BEAT_THIS_REF=<full.onnx>] cargo test --release -p nori-player --features
/// neural-beats neural_eval -- --ignored --nocapture`
#[cfg(feature = "neural-beats")]
#[test]
#[ignore]
fn neural_eval() {
    use super::beats::{self, EndGrid};
    use super::neural::{self, BeatThis, Tracked};
    use super::beats::MixEnd;

    let Ok(path) = std::env::var("NORI_BEAT_THIS") else { return };
    let load = |p: &str, chunk: usize| {
        let t0 = std::time::Instant::now();
        let m = BeatThis::load_chunked(std::path::Path::new(p), chunk).expect("model");
        println!("{p}: chunks of {chunk} frames, loaded and optimised in {:.2} s", t0.elapsed().as_secs_f64());
        m
    };
    // The model as the app runs it (or in chunks of NORI_BT_CHUNK frames), and the reference over whole 30 s
    // windows, as it was trained.
    let chunk = std::env::var("NORI_BT_CHUNK").ok().and_then(|c| c.parse().ok()).unwrap_or(neural::CHUNK);
    let model = load(&path, chunk);
    let reference = std::env::var("NORI_BEAT_THIS_REF").ok().map(|p| load(&p, neural::CHUNK));
    let (spent, runs) = (std::cell::Cell::new(0.0), std::cell::Cell::new(0usize));
    let run = |m: &BeatThis, x: &[f32], rate: u32, end: MixEnd, from_ms: i64, timed: bool| -> Tracked {
        let t0 = std::time::Instant::now();
        let mut t = m.track_window(x, rate, end == MixEnd::Intro).expect("inference");
        if timed {
            spent.set(spent.get() + t0.elapsed().as_secs_f64());
            runs.set(runs.get() + 1);
        }
        t.beats.iter_mut().chain(t.downbeats.iter_mut()).for_each(|v| *v += from_ms as f64 / 1000.0);
        t
    };
    let end_grid = |t: &Tracked| {
        neural::grid(t).map(|g| EndGrid {
            bpm: g.bpm,
            offset_ms: g.offset_s * 1000.0,
            confidence: g.confidence,
            stability: g.stability,
            downbeat_phase: g.downbeat_phase,
            beats_per_bar: g.beats_per_bar,
            other_phase: g.other_phase,
            anchor_ms: g.anchor_s * 1000.0,
        })
    };
    // (beat F, downbeat F) of the model's beats against the reference model's, summed over windows, and the count.
    let agree = std::cell::Cell::new((0.0, 0.0, 0usize));
    let scored = |name: &str, x: &[f32], rate: u32, a: &Analysis, truth: &Truth| -> (SongScore, SongScore) {
        let mut shipped = a.track.clone();
        let mut always = a.track.clone();
        let have = (0, x.len() as i64 * 1000 / rate as i64);
        for end in [MixEnd::Intro, MixEnd::Outro] {
            let Some((from, to)) = beats::window(&a.track, end, have) else { continue };
            let at = |ms: i64| ((ms * rate as i64 / 1000) as usize).min(x.len());
            let piece = &x[at(from)..at(to)];
            let t = run(&model, piece, rate, end, from, true);
            let g = end_grid(&t);
            beats::merge(&mut shipped, end, g);
            if let Some(g) = g {
                beats::merge(&mut always, end, Some(EndGrid { confidence: 1.0, stability: 1.0, other_phase: -1, ..g }));
                // Its own confidence stays what it said, so the planner still refuses a grid it doubts.
                match end {
                    MixEnd::Intro => (always.intro_bpm_confidence, always.intro_stability) = (g.confidence, g.stability),
                    MixEnd::Outro => (always.outro_bpm_confidence, always.outro_stability) = (g.confidence, g.stability),
                }
            }
            if let Some(r) = &reference {
                let rt = run(r, piece, rate, end, from, false);
                let (fb, fd) = (f_measure(&rt.beats, &t.beats), f_measure(&rt.downbeats, &t.downbeats));
                if std::env::var("NORI_EVAL_VERBOSE").is_ok() {
                    println!("    {name} {end:?}: {} beats against the reference's {}, F {fb:.3}; downbeats {} against {}, F {fd:.3}", t.beats.len(), rt.beats.len(), t.downbeats.len(), rt.downbeats.len());
                }
                let (b, d, n) = agree.get();
                agree.set((b + fb, d + fd, n + 1));
            }
        }
        (
            score(name, &Analysis { track: shipped, tempo: a.tempo.clone() }, truth, true),
            score(name, &Analysis { track: always, tempo: a.tempo.clone() }, truth, true),
        )
    };

    let (mut shipped, mut always) = (Vec::new(), Vec::new());
    for song in corpus() {
        let (x, truth) = song.render();
        let a = analyse(song.name, &x, song.rate);
        let (s, w) = scored(song.name, &x, song.rate, &a, &truth);
        shipped.push(s);
        always.push(w);
    }
    let agreement = |agree: &std::cell::Cell<(f64, f64, usize)>| {
        let (b, d, n) = agree.replace((0.0, 0.0, 0));
        if n > 0 {
            println!("agreement with the reference model over {n} windows: beats F {:.3}, downbeats F {:.3}", b / n as f64, d / n as f64);
        }
    };
    println!("Beat This! at each end, adopted where it is sure (as shipped):");
    report(&shipped);
    println!("Beat This! at each end wherever it has a grid:");
    report(&always);
    agreement(&agree);

    let (mut shipped, mut always) = (Vec::new(), Vec::new());
    for r in real_songs() {
        let a = analyse(&r.name, &r.x, r.rate);
        let truth = r.truth(&a.track);
        let (s, w) = scored(&r.name, &r.x, r.rate, &a, &truth);
        shipped.push(SongScore { intro_cue_ok: None, outro_cue_ok: None, vocal: Vec::new(), ..s });
        always.push(SongScore { intro_cue_ok: None, outro_cue_ok: None, vocal: Vec::new(), ..w });
    }
    if !shipped.is_empty() {
        println!("real recordings, as shipped:");
        report(&shipped);
        println!("real recordings, its grid wherever it has one:");
        report(&always);
        agreement(&agree);
    }
    println!("{:.2} s per window of at most 30 s on this machine, one thread ({} windows)", spent.get() / runs.get().max(1) as f64, runs.get());
}


// ---- Transitions ------------------------------------------------------------------------------------------------
//
// The analysis above scores what AutoMix reads; this scores what it does with it. Pairs of synthetic songs are
// rendered, analysed as the app would, planned with the app's settings, and the plan is measured against what the
// songs really are:
//
// - **drop**: where the incoming song's arrangement really arrives, against the bass swap: on it (within a beat),
//   or how long the incoming intro then plays alone after the mix (`dip`, an energy hole), or skipped over;
// - **phrase**: whether the swap falls on a downbeat of the outgoing song, and on a four-bar line of its sections;
// - **skipped music** on either side (silence is free), and whether the 15 s cap held;
// - **dead air**: a long silence of the outgoing song heard before the mix (a hidden track's gap), and **coda**:
//   the outgoing song's closing breakdown heard on its own before the mix;
// - **voices**: how long two sung lines compete - both sounding, neither 10 dB under the other across the voice
//   band. The level of each deck there is measured by running the real mixer with tones on that deck alone, so the
//   gains, filters and any separation are the ones the phone would apply.
//
// `cargo test --release -p nori-player transition_eval -- --ignored --nocapture`

/// The songs the transition harness mixes, besides some of the analysis corpus.
pub fn mix_songs() -> Vec<Song> {
    let s = Song::new;
    vec![
        // A DJ-friendly ending: a sung last chorus, the groove, eight bars of drums.
        Song { sections: vec![(8, DRUMS), (24, FULL), (16, SUNG), (8, FULL), (8, DRUMS)], ..s("house-outro-124", Style::House, 124.0, 9, true) },
        // Sixteen bars of drums before the arrangement arrives: longer than a 16 s mix.
        Song { sections: vec![(16, DRUMS), (24, FULL), (16, SUNG), (16, FULL)], ..s("long-intro-124", Style::House, 124.0, 4, true) },
        // A pad, then drums, then the drop: the drums coming in are not yet the arrangement.
        Song { progression: 1, sections: vec![(8, PADS), (8, DRUMS), (24, FULL), (16, SUNG), (8, FULL)], ..s("build-drop-126", Style::House, 126.0, 9, true) },
        Song { sections: vec![(4, DRUMS), (28, FULL), (16, SUNG), (8, FULL)], ..s("short-intro-124", Style::House, 124.0, 2, true) },
        // Sung to the very end, and a song that is sung from its first bar.
        Song { sections: vec![(4, FULL), (24, SUNG), (16, FULL), (16, SUNG)], ..s("sung-end-120", Style::Backbeat, 120.0, 0, false) },
        Song { progression: 1, sections: vec![(16, SUNG), (16, FULL), (24, SUNG)], ..s("sung-cold-120", Style::Backbeat, 120.0, 7, false) },
        // A sung intro over keys, then the band.
        Song { sections: vec![(8, KEYS_SUNG), (24, SUNG), (16, FULL), (8, FULL)], ..s("sung-intro-122", Style::Backbeat, 122.0, 5, false) },
        // The last chorus, then four bars of pads: an ending not worth playing through.
        Song { sections: vec![(8, DRUMS), (32, FULL), (16, SUNG), (8, FULL), (4, PADS)], ..s("coda-126", Style::House, 126.0, 11, true) },
        // The band drops out for a sung coda over keys: the bass stays, the beat goes.
        Song { sections: vec![(4, FULL), (24, SUNG), (16, FULL), (8, SUNG), (4, KEYS_SUNG)], ..s("keys-coda-120", Style::Backbeat, 120.0, 2, false) },
        // A hidden track: the song, 20 bars of silence (37.5 s), four bars of drums.
        Song {
            quiet_gaps: true,
            sections: vec![(8, DRUMS), (32, FULL), (16, SUNG), (8, FULL), (20, SILENT), (4, DRUMS)],
            ..s("hidden-128", Style::House, 128.0, 7, true)
        },
        // The same with 16 bars after the silence: too much music to skip, so it has to be played.
        Song {
            quiet_gaps: true,
            sections: vec![(8, DRUMS), (32, FULL), (16, SUNG), (8, FULL), (20, SILENT), (16, FULL)],
            ..s("hidden-long-128", Style::House, 128.0, 7, true)
        },
        // Silence the file carries at the ends: 24 s after the music, 8 s before the next.
        Song { tail_silence: 24.0, sections: vec![(8, FULL), (32, FULL), (8, DRUMS)], ..s("tail-silence-128", Style::House, 128.0, 0, false) },
        Song { lead_silence: 8.0, sections: vec![(4, DRUMS), (32, FULL), (8, FULL)], ..s("lead-silence-128", Style::House, 128.0, 7, false) },
    ]
}

/// (outgoing, incoming) by name, from `mix_songs` and the analysis corpus.
pub fn mix_pairs() -> Vec<(&'static str, &'static str)> {
    vec![
        ("house-outro-124", "long-intro-124"),
        ("house-outro-124", "build-drop-126"),
        ("house-outro-124", "short-intro-124"),
        ("sung-end-120", "sung-cold-120"),
        ("sung-end-120", "sung-intro-122"),
        ("coda-126", "long-intro-124"),
        ("coda-126", "short-intro-124"),
        ("keys-coda-120", "short-intro-124"),
        ("hidden-128", "short-intro-124"),
        ("hidden-long-128", "short-intro-124"),
        ("tail-silence-128", "lead-silence-128"),
        ("house-124", "techno-detuned-130"),
        ("techno-detuned-130", "dnb-174"),
        ("pop-swing-96", "shuffle-84"),
        ("rock-detuned-140", "halftime-140"),
    ]
}

/// The level of one deck's voice band through a transition, every 50 ms of wall time: the real mixer run with
/// three tones across the band (500 Hz, 1 kHz, 2 kHz, equal power) on that deck and nothing on the other.
fn deck_level(plan: &crate::types::TransitionPlan, outgoing: bool) -> Vec<f64> {
    const RATE: u32 = 16_000;
    let mut m = mixer::Mixer::new(RATE, 1);
    m.configure(plan);
    let n = (plan.duration_ms.max(0) as usize * RATE as usize / 1000).max(1);
    let tone: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f64 / RATE as f64;
            ([500.0, 1000.0, 2000.0].iter().map(|f| (2.0 * PI * f * t + f / 700.0).sin()).sum::<f64>() / 3f64.sqrt()) as f32
        })
        .collect();
    let zero = vec![0f32; n];
    let mut y = if outgoing { tone.clone() } else { zero.clone() };
    m.process(&mut y, if outgoing { &zero } else { &tone });
    y.chunks(RATE as usize / 20).map(|c| (c.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / c.len() as f64 * 2.0).sqrt()).collect()
}

/// Short-term loudness (EBU R128: 3 s windows, K-weighted, LUFS), one window every 100 ms, of mono `x` at `rate`.
fn short_term(x: &[f32], rate: f64) -> Vec<f64> {
    let mut m = loudness::Meter::new(rate, 0);
    m.feed(x);
    m.finish();
    m.blocks_k.windows(30).map(|w| -0.691 + 10.0 * (w.iter().map(|v| *v as f64).sum::<f64>() / 30.0).max(1e-12).log10()).collect()
}

/// How much louder the transition gets than either song on its own, LU: the loudest 3 s inside the mix (and the
/// second and a half after it) against the louder of the outgoing song's 8 s before it and the incoming song's 8 s
/// after it (medians). The mix is rendered through the real mixer; the incoming song is varispeeded by the plan's
/// ratio (pitch moves, loudness does not).
fn loudness_bump(p: &crate::types::TransitionPlan, xa: &[f32], xb: &[f32], rate: f64) -> f64 {
    let at = |ms: i64| (ms.max(0) as f64 * rate / 1000.0) as usize;
    let (o0, i0, n) = (at(p.out_start_ms), at(p.in_start_ms), at(p.duration_ms));
    let ratio = if p.tempo_ratio > 0.0 { p.tempo_ratio } else { 1.0 };
    let loop_n = p.out_loop_ms.map_or(usize::MAX, |l| at(l).max(1));
    let seg_a: Vec<f32> = (0..n).map(|k| xa.get(o0 + k % loop_n).copied().unwrap_or(0.0)).collect();
    let b_at = |k: f64| {
        let t = i0 as f64 + k;
        let j = t.floor() as usize;
        let f = (t - j as f64) as f32;
        let (u, v) = (xb.get(j).copied().unwrap_or(0.0), xb.get(j + 1).copied().unwrap_or(0.0));
        u + (v - u) * f
    };
    let seg_b: Vec<f32> = (0..n).map(|k| b_at(k as f64 * ratio)).collect();
    let mut m = mixer::Mixer::new(rate as u32, 1);
    m.configure(p);
    let mut mixed = seg_a;
    m.process(&mut mixed, &seg_b);
    let pre = at(8_000);
    let before: Vec<f32> = xa[o0.saturating_sub(pre).min(xa.len())..o0.min(xa.len())].to_vec();
    let b_end = i0 + (n as f64 * ratio) as usize;
    let after: Vec<f32> = xb[b_end.min(xb.len())..(b_end + pre).min(xb.len())].to_vec();
    let mut heard = before.clone();
    heard.extend(&mixed);
    heard.extend(&after);
    let st = short_term(&heard, rate);
    let median = |x: &[f32]| {
        let mut v = short_term(x, rate);
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(v.len() / 2).copied().unwrap_or(-70.0)
    };
    let reference = median(&before).max(median(&after));
    // Windows (every 100 ms, 3 s long) that end inside the mix or up to 1.5 s after it.
    let (w0, w1) = ((before.len() as f64 / rate * 10.0) as usize, ((before.len() + n) as f64 / rate * 10.0 + 15.0) as usize);
    let peak = (w0.saturating_sub(30)..w1.saturating_sub(30).min(st.len())).map(|j| st[j + 0]).fold(f64::MIN, f64::max);
    if peak == f64::MIN { 0.0 } else { peak - reference }
}

/// How one planned transition came out.
#[derive(Debug, Clone)]
pub struct MixScore {
    pub name: String,
    pub kind: crate::types::TransitionKind,
    pub reason: String,
    pub duration_s: f64,
    /// The incoming song has a drop.
    pub has_drop: bool,
    /// The bass swap falls on the incoming song's drop, within a beat.
    pub drop_hit: bool,
    /// The drop was skipped over by the entry.
    pub drop_skipped: bool,
    /// The incoming intro heard on its own after the mix, before its drop, seconds.
    pub dip_s: f64,
    /// The swap is on a downbeat of the outgoing song, and on a four-bar line of its sections.
    pub swap_on_bar: bool,
    pub swap_on_phrase: bool,
    /// The mix starts on a four-bar line of the outgoing song.
    pub start_on_phrase: bool,
    pub skip_out_s: f64,
    pub skip_in_s: f64,
    pub dead_air_s: f64,
    pub coda_s: f64,
    /// The run-up laid over the outgoing song's dead ending, seconds.
    pub dead_runup_s: f64,
    /// Two voices competing, seconds; and in the same window without the separation.
    pub clash_s: f64,
    pub clash_bare_s: f64,
    /// Both voices sounding at once, whatever their levels, seconds.
    pub both_sung_s: f64,
    /// How much louder than either song the mix gets, LU.
    pub bump_lu: f64,
}

/// Scores `p`, the plan from `a` (outgoing) into `b`, against both songs' truth.
pub fn score_mix(name: &str, p: &crate::types::TransitionPlan, a: &Truth, b: &Truth, audio: (&[f32], &[f32], f64)) -> MixScore {
    let ms = |v: i64| v as f64 / 1000.0;
    let dur = ms(p.duration_ms);
    let ratio = if p.tempo_ratio > 0.0 { p.tempo_ratio } else { 1.0 };
    let (out_start, in_start) = (ms(p.out_start_ms), ms(p.in_start_ms));
    let heard_end = out_start + p.out_loop_ms.map_or(dur, ms);
    let beat_a = 60.0 / a.bpm.max(1.0);
    // The bass swap, from where the incoming lows start to come in to where they are all in: a swap that starts on
    // a line and one that finishes on it are both on it.
    let swap = p.bass_swap.map(|s| (ms(s.at_ms), ms(s.at_ms + s.len_ms.max(0))));
    // Where the incoming drop lands in wall time from the start of the mix.
    let lands = b.drop.map(|d| (d - in_start) / ratio);
    let has_drop = lands.is_some();
    let drop_skipped = lands.is_some_and(|l| l < -0.5 * beat_a);
    let drop_hit = matches!((lands, swap), (Some(l), Some((s0, s1))) if l >= s0 - 0.5 * beat_a && l <= s1 + 0.5 * beat_a);
    let dip_s = match lands {
        Some(l) if l > dur => l - dur,
        _ => 0.0,
    };
    // The swap in the outgoing song's own time, against its bars and section lines.
    let at: Vec<f64> = swap.map_or(Vec::new(), |(s0, s1)| vec![out_start + s0, out_start + s1]);
    // The bar line where the music ends counts: it is where the last bar's successor would start.
    let bar_lines: Vec<f64> = a.downbeats.iter().copied().chain([a.music.1]).collect();
    let swap_on_bar = at.iter().any(|t| bar_lines.iter().any(|d| (d - t).abs() <= TOL));
    let lines: Vec<f64> = {
        // Every four bars from the first downbeat and from each section start the truth names.
        let bar = a.meter as f64 * beat_a;
        let starts: Vec<f64> = [a.downbeats.first().copied(), Some(a.intro_end), a.outro_start, a.drop, a.exit]
            .into_iter()
            .flatten()
            .filter(|t| bar_lines.iter().any(|d| (d - t).abs() <= TOL))
            .collect();
        let mut v = Vec::new();
        for s in starts {
            let mut t = s;
            while t <= a.music.1 + bar {
                v.push(t);
                t += 4.0 * bar;
            }
        }
        v
    };
    let swap_on_phrase = at.iter().any(|t| lines.iter().any(|l| (l - t).abs() <= TOL));
    let start_on_phrase = lines.iter().any(|l| (l - out_start).abs() <= TOL);
    let skip_out_s = a.music_between(heard_end, f64::MAX);
    let skip_in_s = b.music_between(0.0, in_start);
    // A long silence of the outgoing song heard before the incoming one comes in: a gap near its end, or the
    // silence after its music.
    let dead_air_s: f64 = a
        .gaps
        .iter()
        .filter(|(g0, g1)| g1 - g0 >= 3.0 && a.music_between(*g1, f64::MAX) <= 30.0)
        .map(|(g0, g1)| (g1.min(out_start) - g0).max(0.0))
        .fold((out_start - a.music.1).max(0.0), |a, b| a + b);
    let coda_s = match a.exit {
        Some(e) if !a.gaps.iter().any(|(g0, _)| (g0 - e).abs() < 0.01) => (out_start.min(a.music.1) - e).max(0.0),
        _ => 0.0,
    };
    // The run-up (up to the swap, or the whole mix without one) laid over the outgoing song's dead ending: its
    // closing breakdown, a gap, the silence after its music. The energy hole an exit is for.
    let run_up = swap.map_or(dur, |(s0, _)| s0);
    let breakdown = a.exit.filter(|e| !a.gaps.iter().any(|(g0, _)| (g0 - e).abs() < 0.01));
    let dead_at = |t: f64| t >= a.music.1 || a.gaps.iter().any(|(g0, g1)| t >= *g0 && t < *g1) || breakdown.is_some_and(|e| t >= e);
    let dead_runup_s = (0..(run_up / 0.05) as usize)
        .filter(|k| {
            let t = *k as f64 * 0.05 + 0.025;
            dead_at(out_start + p.out_loop_ms.map_or(t, |l| t % ms(l)))
        })
        .count() as f64
        * 0.05;
    // Voices competing, with the plan as it is and with its separation taken out (the same window, gains and
    // other filters), which is what the separation buys.
    let competing = |p: &crate::types::TransitionPlan| -> (f64, f64) {
        let (la, lb) = (deck_level(p, true), deck_level(p, false));
        let (mut clash_s, mut both_sung_s) = (0.0, 0.0);
        for (k, (ga, gb)) in la.iter().zip(&lb).enumerate() {
            let t = k as f64 * 0.05 + 0.025;
            let ta = out_start + p.out_loop_ms.map_or(t, |l| t % ms(l));
            if a.sung_at(ta) && b.sung_at(in_start + t * ratio) {
                both_sung_s += 0.05;
                let (da, db) = (20.0 * ga.max(1e-9).log10(), 20.0 * gb.max(1e-9).log10());
                if da.min(db) > -20.0 && (da - db).abs() < 10.0 {
                    clash_s += 0.05;
                }
            }
        }
        (clash_s, both_sung_s)
    };
    let (clash_s, both_sung_s) = competing(p);
    let mut bare = p.clone();
    bare.vocal_duck = None;
    if bare.reason.contains("voices kept apart") && bare.high_pass.is_some_and(|h| h.to_hz == plan::VOCAL_HP_TO_HZ) {
        bare.high_pass = None;
    }
    let (clash_bare_s, _) = if bare != *p { competing(&bare) } else { (clash_s, both_sung_s) };
    MixScore {
        name: name.to_string(),
        kind: p.kind,
        reason: p.reason.clone(),
        duration_s: dur,
        has_drop,
        drop_hit,
        drop_skipped,
        dip_s,
        swap_on_bar,
        swap_on_phrase,
        start_on_phrase,
        skip_out_s,
        skip_in_s,
        dead_air_s,
        coda_s,
        dead_runup_s,
        clash_s,
        clash_bare_s,
        both_sung_s,
        bump_lu: loudness_bump(p, audio.0, audio.1, audio.2),
    }
}

#[derive(Debug, Default)]
pub struct MixTotals {
    pub pairs: usize,
    pub beat_matched: usize,
    pub echo: usize,
    pub drops: usize,
    pub drop_hit: usize,
    pub drop_skipped: usize,
    pub dip_s: f64,
    pub swaps: usize,
    pub swap_on_bar: usize,
    pub swap_on_phrase: usize,
    pub start_on_phrase: usize,
    pub skip_out_s: f64,
    pub skip_in_s: f64,
    pub cap_broken: usize,
    pub dead_air_s: f64,
    pub coda_s: f64,
    pub dead_runup_s: f64,
    pub clash_s: f64,
    pub clash_bare_s: f64,
    pub both_sung_s: f64,
    pub bump_lu: f64,
    pub bump_max_lu: f64,
}

pub fn mix_report(scores: &[MixScore]) -> MixTotals {
    use crate::types::TransitionKind as K;
    println!(
        "{:<38} {:<6} {:>5}  {:<8} {:>5} {:>4}/{:<4} {:>5}/{:<5} {:>5} {:>5} {:>5} {:>6} {:>5}",
        "pair", "kind", "len", "drop", "dip", "bar", "phr", "skipO", "skipI", "dead", "coda", "under", "clash", "bump"
    );
    let mut t = MixTotals::default();
    for s in scores {
        t.pairs += 1;
        t.beat_matched += (s.kind == K::BeatMatched) as usize;
        t.echo += (s.kind == K::EchoOut) as usize;
        if s.has_drop {
            t.drops += 1;
            t.drop_hit += s.drop_hit as usize;
            t.drop_skipped += s.drop_skipped as usize;
        }
        t.dip_s += s.dip_s;
        if s.kind == K::BeatMatched {
            t.swaps += 1;
            t.swap_on_bar += s.swap_on_bar as usize;
            t.swap_on_phrase += s.swap_on_phrase as usize;
            t.start_on_phrase += s.start_on_phrase as usize;
        }
        t.skip_out_s += s.skip_out_s;
        t.skip_in_s += s.skip_in_s;
        t.cap_broken += (s.skip_out_s > 15.05 || s.skip_in_s > 15.05) as usize;
        t.dead_air_s += s.dead_air_s;
        t.coda_s += s.coda_s;
        t.dead_runup_s += s.dead_runup_s;
        t.clash_s += s.clash_s;
        t.clash_bare_s += s.clash_bare_s;
        t.both_sung_s += s.both_sung_s;
        t.bump_lu += s.bump_lu;
        t.bump_max_lu = t.bump_max_lu.max(s.bump_lu);
        let kind = match s.kind {
            K::Gapless => "gapls",
            K::EqualPowerFade => "fade",
            K::MixRampFade => "ramp",
            K::BeatMatched => "match",
            K::EchoOut => "echo",
        };
        let drop = if !s.has_drop {
            "-"
        } else if s.drop_hit {
            "hit"
        } else if s.drop_skipped {
            "SKIPPED"
        } else {
            "miss"
        };
        println!(
            "{:<38} {:<6} {:>5.1}  {:<8} {:>5.1} {:>4}/{:<4} {:>5.1}/{:<5.1} {:>5.1} {:>5.1} {:>5.1} {:>6.1} {:>+5.1}",
            s.name,
            kind,
            s.duration_s,
            drop,
            s.dip_s,
            mark(s.swap_on_bar),
            mark(s.swap_on_phrase),
            s.skip_out_s,
            s.skip_in_s,
            s.dead_air_s,
            s.coda_s,
            s.dead_runup_s,
            s.clash_s,
            s.bump_lu,
        );
        if std::env::var("NORI_EVAL_VERBOSE").is_ok() {
            println!("    {}", s.reason);
        }
    }
    println!(
        "TOTAL {} pairs ({} beat-matched, {} echo-outs): drops hit {}/{} (skipped {}), incoming intro alone after the mix {:.1} s, swaps on a bar {}/{} and on a phrase line {}/{}, mixes starting on a phrase line {}/{}",
        t.pairs, t.beat_matched, t.echo, t.drop_hit, t.drops, t.drop_skipped, t.dip_s, t.swap_on_bar, t.swaps, t.swap_on_phrase, t.swaps, t.start_on_phrase, t.swaps
    );
    println!(
        "      music skipped {:.1} s out / {:.1} s in (cap broken {}), dead air {:.1} s, coda alone {:.1} s, run-up over a dead ending {:.1} s, voices competing {:.1} of {:.1} s sung together ({:.1} without the separation)",
        t.skip_out_s, t.skip_in_s, t.cap_broken, t.dead_air_s, t.coda_s, t.dead_runup_s, t.clash_s, t.both_sung_s, t.clash_bare_s
    );
    println!("      louder than either song: {:+.1} LU on average, {:+.1} LU at most", t.bump_lu / t.pairs.max(1) as f64, t.bump_max_lu);
    t
}

/// The analysis with its voice-band shares replaced by what the truth says is sung over the same windows (0.7
/// where a voice sounds over at least 30 % of it, 0.1 elsewhere): the planner and mixer measured on their own,
/// with a vocal detector that is right. The classical voice-band share cannot tell the synthetic voices from an
/// unsung band (0.28-0.37 against 0.20-0.34, pads 0.8), which is `analysis.md`'s open problem, not the mixer's.
pub fn oracle_vocals(t: &TrackAnalysis, truth: &Truth) -> TrackAnalysis {
    let s = |a: f64, b: f64| if truth.voice_share(a, b) >= 0.3 { 0.7 } else { 0.1 };
    let ms = |v: i64| v as f64 / 1000.0;
    let (m0, m1) = (ms(t.silence_start_ms), ms(t.silence_end_ms));
    let whole = |a: f64, b: f64| if b - a < 1.0 { (m0, m1) } else { (a, b) };
    let bar = if t.bpm > 0.0 { 60.0 / t.bpm * bar_beats(t) as f64 } else { 2.0 };
    let last = if t.exit_ms > 0 { ms(t.exit_ms) } else { m1 };
    let drop = ms(t.drop_ms);
    let (i0, i1) = whole(m0, ms(t.intro_end_ms));
    let (o0, o1) = whole(ms(t.outro_start_ms), m1);
    TrackAnalysis {
        intro_vocal: s(i0, i1),
        outro_vocal: s(o0, o1),
        exit_vocal: s(last - 8.0 * bar, last),
        drop_runup_vocal: if t.drop_ms > 0 { s((drop - 8.0 * bar).max(m0), drop) } else { 0.0 },
        drop_vocal: if t.drop_ms > 0 { s(drop, drop + 8.0 * bar) } else { 0.0 },
        ..t.clone()
    }
}

/// Renders and analyses the songs the pairs use, plans every pair with `settings`, and scores the plans. With
/// `oracle`, the voice-band shares come from the truth (`oracle_vocals`).
pub fn run_mixes(settings: &crate::types::AutoMixSettings, oracle: bool) -> Vec<MixScore> {
    let only = std::env::var("NORI_EVAL_ONLY").unwrap_or_default();
    let pairs: Vec<(&str, &str)> =
        mix_pairs().into_iter().filter(|(a, b)| only.is_empty() || only.split(',').any(|o| a.starts_with(o) || b.starts_with(o))).collect();
    let songs: Vec<Song> = mix_songs().into_iter().chain(corpus_all()).collect();
    let mut done: Vec<(&str, TrackAnalysis, Truth, Vec<f32>, f64)> = Vec::new();
    for name in pairs.iter().flat_map(|(a, b)| [*a, *b]) {
        if done.iter().any(|(n, ..)| *n == name) {
            continue;
        }
        let song = songs.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no song {name}"));
        let (x, truth) = song.render();
        let a = analyse(song.name, &x, song.rate).track;
        let a = if oracle { oracle_vocals(&a, &truth) } else { a };
        done.push((song.name, a, truth, x, song.rate as f64));
    }
    let get = |n: &str| done.iter().find(|(m, ..)| *m == n).unwrap();
    pairs
        .iter()
        .map(|(a, b)| {
            let ((_, ta, xa, wa, rate), (_, tb, xb, wb, _)) = (get(a), get(b));
            let p = plan::plan(Some(ta), Some(tb), ta.duration_ms, tb.duration_ms, settings);
            score_mix(&format!("{a} > {b}"), &p, xa, xb, (wa, wb, *rate))
        })
        .collect()
}

/// `cargo test --release -p nori-player transition_eval -- --ignored --nocapture`
#[test]
#[ignore]
fn transition_eval() {
    for (label, s, oracle) in [
        ("default settings (16 s at most)", crate::types::AutoMixSettings::default(), false),
        ("32 s at most", crate::types::AutoMixSettings { max_transition_s: 32.0, ..Default::default() }, false),
        ("default settings, voices from the truth", crate::types::AutoMixSettings::default(), true),
        ("voices from the truth, filters off", crate::types::AutoMixSettings { filter_effects: false, ..Default::default() }, true),
    ] {
        println!("{label}:");
        mix_report(&run_mixes(&s, oracle));
    }
}

/// How well the analysis finds the landmarks the planner enters and leaves on, over every synthetic song (the
/// analysis corpus and the mix songs): the drop, and the exit (a closing breakdown, or the gap before a short hidden
/// track). A landmark within a beat of the truth is found; one claimed where the truth has none is a false alarm.
/// `cargo test --release -p nori-player landmark_eval -- --ignored --nocapture`
#[test]
#[ignore]
fn landmark_eval() {
    let verbose = std::env::var("NORI_EVAL_VERBOSE").is_ok();
    // (found, truths, false alarms) for the drop and the exit.
    let mut tally = [(0, 0, 0); 2];
    for song in mix_songs().into_iter().chain(corpus()) {
        let (x, truth) = song.render();
        let t = analyse(song.name, &x, song.rate).track;
        let beat = 60.0 / truth.bpm.max(1.0);
        let at = |ms: i64| (ms > 0).then(|| ms as f64 / 1000.0);
        let mut line = format!("{:<20}", song.name);
        let mut wrong = false;
        for (k, (name, want, got)) in [("drop", truth.drop, at(t.drop_ms)), ("exit", truth.exit, at(t.exit_ms))].into_iter().enumerate() {
            let ok = match (want, got) {
                (Some(d), Some(g)) => (d - g).abs() <= beat,
                (None, None) => true,
                _ => false,
            };
            tally[k].1 += want.is_some() as usize;
            tally[k].0 += (ok && want.is_some()) as usize;
            tally[k].2 += (want.is_none() && got.is_some()) as usize;
            wrong |= !ok;
            let s = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.1}"));
            line += &format!(" {name} {:>6} (true {:>6})", s(got), s(want));
        }
        if verbose || wrong {
            println!("{line}");
        }
        if std::env::var("NORI_EVAL_VOCAL").is_ok() {
            let last = at(t.exit_ms).unwrap_or(t.silence_end_ms as f64 / 1000.0);
            let bar = 4.0 * beat;
            println!(
                "    vocal: exit {:.2} (sung {:.2}) intro {:.2} (sung {:.2}) outro {:.2} | drop runup {:.2} after {:.2}",
                t.exit_vocal,
                truth.voice_share(last - 8.0 * bar, last),
                t.intro_vocal,
                truth.voice_share(t.silence_start_ms as f64 / 1000.0, t.intro_end_ms as f64 / 1000.0),
                t.outro_vocal,
                t.drop_runup_vocal,
                t.drop_vocal
            );
        }
    }
    for (name, (found, truths, fa)) in ["drops", "exits"].iter().zip(tally) {
        println!("{name} found {found}/{truths}, {fa} false alarms");
    }
}
