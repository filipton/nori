//! Level measurements: BS.1770 loudness, silence trims, hidden-track gaps and MixRamp points, all read from
//! 100 ms blocks measured while the audio streams.

use ebur128::{EbuR128, Mode};

/// Block length for every level measurement.
pub const BLOCK_MS: i64 = 100;
/// Silence trim threshold, dBFS RMS.
pub const SILENCE_DB: f64 = -55.0;
/// MixRamp threshold relative to integrated loudness, dB.
pub const MIXRAMP_DB: f64 = -17.0;

/// Streaming 100 ms block meter: each block's mean square, K-weighted (by ebur128) and plain.
///
/// ebur128's `Mode::I` is not used (75 % dearer); [`integrated`] gates the blocks to the same result.
pub struct Meter {
    r128: EbuR128,
    /// Samples per block (100 ms).
    block: usize,
    n: usize,
    acc_raw: f64,
    /// Mean square per block, K-weighted.
    pub blocks_k: Vec<f32>,
    /// Mean square per block, unweighted.
    pub blocks_raw: Vec<f32>,
}

impl Meter {
    pub fn new(rate: f64, expected_blocks: usize) -> Self {
        let rate = (rate.round() as u32).clamp(16, 2_822_400);
        Meter {
            r128: EbuR128::new(1, rate, Mode::M).expect("one channel at a rate ebur128 takes"),
            block: (rate as usize + 5) / 10,
            n: 0,
            acc_raw: 0.0,
            blocks_k: Vec::with_capacity(expected_blocks),
            blocks_raw: Vec::with_capacity(expected_blocks),
        }
    }

    /// Mono samples in [-1, 1]; non-finite samples count as silence.
    pub fn feed(&mut self, mut x: &[f32]) {
        while !x.is_empty() {
            let (part, rest) = x.split_at((self.block - self.n).min(x.len()));
            // Non-finite samples make the sum non-finite; only then are they cleaned one by one.
            let raw = sum_of_squares(part);
            if raw.is_finite() {
                self.add(part, raw);
            } else {
                let mut clean = [0f32; 256];
                for c in part.chunks(clean.len()) {
                    for (d, v) in clean.iter_mut().zip(c) {
                        *d = if v.is_finite() { *v } else { 0.0 };
                    }
                    let clean = &clean[..c.len()];
                    self.add(clean, sum_of_squares(clean));
                }
            }
            if self.n == self.block {
                self.flush_block();
            }
            x = rest;
        }
    }

    fn add(&mut self, x: &[f32], raw: f64) {
        // Fails only for an empty slice.
        let _ = self.r128.add_frames_f32(x);
        self.acc_raw += raw;
        self.n += x.len();
    }

    fn flush_block(&mut self) {
        if self.n == 0 {
            return;
        }
        let window_ms = (self.n as u64 * 1000 / self.r128.rate() as u64).max(1) as u32;
        let lufs = self.r128.loudness_window(window_ms).unwrap_or(f64::NEG_INFINITY);
        let ms = if lufs.is_finite() { 10f64.powf((lufs + 0.691) / 10.0) } else { 0.0 };
        self.blocks_k.push(ms as f32);
        self.blocks_raw.push((self.acc_raw / self.n as f64) as f32);
        (self.acc_raw, self.n) = (0.0, 0);
    }

    /// Closes a partial last block when at least a quarter full (a shorter one would read as a fade).
    pub fn finish(&mut self) {
        if self.n * 4 >= self.block {
            self.flush_block();
        }
        (self.acc_raw, self.n) = (0.0, 0);
    }

    /// Ready for the next song, keeping allocations.
    pub fn reset(&mut self) {
        self.r128.reset();
        (self.acc_raw, self.n) = (0.0, 0);
        self.blocks_k.clear();
        self.blocks_raw.clear();
    }
}

/// Sum of the squares, in eight lanes so it vectorises.
fn sum_of_squares(x: &[f32]) -> f64 {
    let mut lanes = [0f64; 8];
    let mut chunks = x.chunks_exact(8);
    for c in &mut chunks {
        for (l, v) in lanes.iter_mut().zip(c) {
            *l += *v as f64 * *v as f64;
        }
    }
    chunks.remainder().iter().map(|v| *v as f64 * *v as f64).sum::<f64>() + lanes.iter().sum::<f64>()
}

fn lufs_of(mean_square: f64) -> f64 {
    if mean_square <= 1e-12 {
        -120.0
    } else {
        -0.691 + 10.0 * mean_square.log10()
    }
}

pub fn db(mean_square: f64) -> f64 {
    10.0 * mean_square.max(1e-12).log10()
}

/// Momentary (400 ms) mean squares, one per 100 ms step: window `j` covers blocks `j..j+4`.
fn momentary(blocks_k: &[f32]) -> impl Iterator<Item = f64> + '_ {
    blocks_k.windows(4.min(blocks_k.len()).max(1)).map(|w| w.iter().map(|v| *v as f64).sum::<f64>() / w.len() as f64)
}

/// Integrated loudness, BS.1770-4 gating (absolute -70 LUFS, relative -10 LU) over 400 ms windows with 75 % overlap.
pub fn integrated(blocks_k: &[f32]) -> f64 {
    let gated: Vec<f64> = momentary(blocks_k).filter(|ms| lufs_of(*ms) > -70.0).collect();
    if gated.is_empty() {
        return -70.0;
    }
    let rel = lufs_of(gated.iter().sum::<f64>() / gated.len() as f64) - 10.0;
    let (sum, n) = gated.iter().filter(|ms| lufs_of(**ms) > rel).fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    if n == 0 {
        -70.0
    } else {
        lufs_of(sum / n as f64).max(-70.0)
    }
}

/// First and last audible moment, ms; `(0, 0)` for silence.
pub fn silence_trim(blocks_raw: &[f32]) -> (i64, i64) {
    let loud = |v: &f32| db(*v as f64) > SILENCE_DB;
    match (blocks_raw.iter().position(loud), blocks_raw.iter().rposition(loud)) {
        (Some(a), Some(b)) => (a as i64 * BLOCK_MS, (b as i64 + 1) * BLOCK_MS),
        _ => (0, 0),
    }
}

/// The last silence of at least `min_ms` inside the music, (start, end) ms: the gap before a hidden track.
pub fn last_gap(blocks_raw: &[f32], min_ms: i64) -> Option<(i64, i64)> {
    let loud = |v: &f32| db(*v as f64) > SILENCE_DB;
    let (first, last) = (blocks_raw.iter().position(loud)?, blocks_raw.iter().rposition(loud)?);
    let min_blocks = (min_ms / BLOCK_MS).max(1) as usize;
    let mut j = last;
    while j > first {
        if loud(&blocks_raw[j]) {
            j -= 1;
            continue;
        }
        let end = j + 1;
        while j > first && !loud(&blocks_raw[j]) {
            j -= 1;
        }
        if end - (j + 1) >= min_blocks {
            return Some(((j + 1) as i64 * BLOCK_MS, end as i64 * BLOCK_MS));
        }
    }
    None
}

/// MixRamp points, ms: centres of the first and last 400 ms windows at or above `lufs + MIXRAMP_DB`.
pub fn mixramp(blocks_k: &[f32], lufs: f64) -> Option<(i64, i64)> {
    let thresh = lufs + MIXRAMP_DB;
    let (mut first, mut last) = (None, None);
    for (j, ms) in momentary(blocks_k).enumerate() {
        if lufs_of(ms) >= thresh {
            first.get_or_insert(j);
            last = Some(j);
        }
    }
    let centre = |j: usize| j as i64 * BLOCK_MS + 2 * BLOCK_MS;
    Some((centre(first?), centre(last?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meter(x: &[f32], rate: f64) -> Meter {
        let mut m = Meter::new(rate, 0);
        x.chunks(1000).for_each(|c| m.feed(c));
        m.finish();
        m
    }

    fn sine(freq: f64, amp: f64, secs: f64, rate: f64) -> Vec<f32> {
        (0..(secs * rate) as usize).map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin()) as f32).collect()
    }

    #[test]
    fn full_scale_sine_reads_minus_3_lufs() {
        for rate in [44100.0, 48000.0] {
            let m = meter(&sine(997.0, 1.0, 5.0, rate), rate);
            let l = integrated(&m.blocks_k);
            assert!((l + 3.01).abs() < 0.1, "{rate}: {l}");
        }
        let m = meter(&sine(997.0, 0.1, 5.0, 44100.0), 44100.0);
        assert!((integrated(&m.blocks_k) + 23.01).abs() < 0.1);
    }

    #[test]
    fn integrated_matches_ebur128() {
        let rate = 44100.0;
        let mut steps = sine(440.0, 0.5, 10.0, rate);
        steps.extend(sine(440.0, 0.02, 10.0, rate)); // below the relative gate
        steps.extend(vec![0f32; 44100]); // below the absolute one
        steps.extend(sine(3000.0, 0.2, 7.3, rate));
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..44100 * 30)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed as f32 / u32::MAX as f32 - 0.5) * (0.05 + 0.4 * ((i as f32 / 44100.0 * 0.3).sin().abs()))
            })
            .collect();
        for (name, x, rate) in [("997 Hz", sine(997.0, 0.3, 5.0, 48000.0), 48000.0), ("steps", steps, rate), ("noise", noise, rate)] {
            let mut r = EbuR128::new(1, rate as u32, Mode::I).unwrap();
            r.add_frames_f32(&x).unwrap();
            let theirs = r.loudness_global().unwrap();
            let ours = integrated(&meter(&x, rate).blocks_k);
            assert!((ours - theirs).abs() < 0.01, "{name}: {ours} against ebur128's {theirs}");
        }
    }

    #[test]
    fn chunking_and_non_finite_samples_do_not_matter() {
        let rate = 44100.0;
        let x = sine(440.0, 0.5, 3.0, rate);
        let whole = meter(&x, rate);
        let mut m = Meter::new(rate, 0);
        for c in x.chunks(777) {
            m.feed(c);
        }
        m.finish();
        assert_eq!((&m.blocks_k, &m.blocks_raw), (&whole.blocks_k, &whole.blocks_raw));

        let (mut with_nan, mut with_zero) = (x.clone(), x);
        for i in [10_000, 10_001, 50_000] {
            (with_nan[i], with_zero[i]) = (if i == 50_000 { f32::INFINITY } else { f32::NAN }, 0.0);
        }
        let (a, b) = (meter(&with_nan, rate), meter(&with_zero, rate));
        assert_eq!((&a.blocks_k, &a.blocks_raw), (&b.blocks_k, &b.blocks_raw));
    }

    #[test]
    fn silence_is_minus_70_and_has_no_ramp() {
        let m = meter(&vec![0.0; 44100 * 3], 44100.0);
        assert_eq!(integrated(&m.blocks_k), -70.0);
        assert_eq!(silence_trim(&m.blocks_raw), (0, 0));
        assert_eq!(mixramp(&m.blocks_k, -70.0), None);
    }

    #[test]
    fn finds_last_long_gap() {
        let rate = 8000.0;
        let mut x = sine(440.0, 0.5, 10.0, rate);
        x.extend(vec![0f32; (rate * 2.0) as usize]); // a 2 s rest
        x.extend(sine(440.0, 0.5, 5.0, rate));
        x.extend(vec![0f32; (rate * 8.0) as usize]); // an 8 s gap
        x.extend(sine(440.0, 0.5, 3.0, rate));
        x.extend(vec![0f32; (rate * 4.0) as usize]); // trailing silence is not inside the music
        let m = meter(&x, rate);
        assert_eq!(last_gap(&m.blocks_raw, 6000), Some((17_000, 25_000)));
        assert_eq!(last_gap(&m.blocks_raw, 1000), Some((17_000, 25_000)), "the last one, not the first");
        assert_eq!(last_gap(&m.blocks_raw, 9000), None);
        assert_eq!(last_gap(&[], 6000), None);
    }

    #[test]
    fn trims_and_ramps_find_the_edges() {
        let rate = 44100.0;
        let mut x = vec![0f32; (rate * 2.0) as usize]; // 2 s of silence
        let fade_in: Vec<f32> = sine(440.0, 0.5, 4.0, rate).iter().enumerate().map(|(i, v)| v * (i as f32 / (rate as f32 * 4.0))).collect();
        x.extend(fade_in); // a 4 s linear fade-in
        x.extend(sine(440.0, 0.5, 20.0, rate));
        x.extend(vec![0f32; (rate * 3.0) as usize]);
        let m = meter(&x, rate);
        let (start, end) = silence_trim(&m.blocks_raw);
        // The ramp crosses -55 dB after 4 s * 10^(-46/20) = 20 ms.
        assert!((2000..=2100).contains(&start), "start {start}");
        assert_eq!(end, 26000);
        let lufs = integrated(&m.blocks_k);
        let (r_in, r_out) = mixramp(&m.blocks_k, lufs).unwrap();
        // -17 dB is 0.56 s into the ramp.
        assert!((2400..=3000).contains(&r_in), "ramp in {r_in}");
        assert!((25700..=26100).contains(&r_out), "ramp out {r_out}");
    }
}
