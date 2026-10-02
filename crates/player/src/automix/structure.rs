//! Structure on top of the beat grid, and the key.
//! Metre: 3/4 when the beats repeat every three clearly better than every two or four, else 4/4. Downbeats: votes
//! over the bar's phases from low-band hits (the kick) and chroma change. Sections: novelty of bar features
//! across candidate boundaries. Key: the tuned pitch profile against key profiles.

use super::analysis::Features;
use super::loudness;
use super::tempo::Tempo;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Downbeat {
    pub phase: i32,
    pub confidence: f32,
    /// 3 or 4; 0 when nothing was measured.
    pub beats_per_bar: i64,
}

/// A section boundary: the 4 bars after differ from the 4 before by this much (RMS over the bar features in units
/// of their spread). Tuned on `eval.rs`; blocks (no grid) need more.
const NOVELTY_MIN: f64 = 0.6;
const NOVELTY_MIN_BLOCKS: f64 = 0.8;
/// The smallest noticeable change per bar feature (level dB, low dB, tonal dB, ln onset, octaves of brightness,
/// voice share). Half of it floors each feature's spread, so a song that never changes is not judged on noise.
const SECTION_SCALE: [f64; 6] = [3.0, 4.0, 4.0, 0.3, 0.25, 0.08];
/// Units (bars, or blocks without a grid) compared either side of a boundary.
const SECTION_SPAN: usize = 4;
/// How far a boundary may thin the music and still end an intro (or fill it and still start an outro).
const FULLER_EPS: f64 = 0.25;
/// How much better the beats must repeat every three than every two or four to call the bar 3/4.
const TRIPLE_MARGIN: f64 = 0.3;

fn frame_of(f: &Features, t: f64) -> isize {
    ((t - f.t0) * f.fps).round() as isize
}

fn z(v: &mut [f64]) {
    let n = v.len().max(1) as f64;
    let mean = v.iter().sum::<f64>() / n;
    let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n).sqrt();
    v.iter_mut().for_each(|x| *x = if sd > 1e-12 { (*x - mean) / sd } else { 0.0 });
}

/// Mean chroma, unit length, of the chroma frames centred in `[a, b)`.
fn chroma_between(f: &Features, a: f64, b: f64) -> [f64; 12] {
    let mut c = [0f64; 12];
    if f.chroma_step <= 0.0 {
        return c;
    }
    let j0 = ((a - f.chroma_t0) / f.chroma_step).ceil().max(0.0) as usize;
    let j1 = (((b - f.chroma_t0) / f.chroma_step).ceil().max(0.0) as usize).min(f.chroma.len());
    for frame in f.chroma.get(j0..j1.max(j0)).unwrap_or(&[]) {
        for (a, v) in c.iter_mut().zip(frame) {
            *a += *v as f64;
        }
    }
    let norm = c.iter().map(|v| v * v).sum::<f64>().sqrt();
    if norm > 0.0 {
        c.iter_mut().for_each(|v| *v /= norm);
    }
    c
}

/// Rise of the linear low-band level over two frames: how hard a kick or bass note lands (log flux would weigh a
/// snare's faint low leakage like a kick).
fn low_rise(f: &Features) -> Vec<f32> {
    let rms: Vec<f32> = f.low_power.iter().map(|p| p.max(0.0).sqrt()).collect();
    (0..rms.len()).map(|k| if k < 2 { 0.0 } else { (rms[k] - rms[k - 2]).max(0.0) }).collect()
}

fn peak_near(v: &[f32], k: isize, reach: isize) -> f64 {
    let lo = (k - reach).max(0) as usize;
    let hi = ((k + reach + 1).max(0) as usize).min(v.len());
    v.get(lo..hi.max(lo)).unwrap_or(&[]).iter().fold(0f32, |m, x| m.max(*x)) as f64
}

/// Mean cosine between rows `n` and `n + lag`, rows centred on the column means first.
fn lag_similarity(rows: &[Vec<f64>], lag: usize) -> f64 {
    if rows.len() <= lag + 4 {
        return 0.0;
    }
    let dims = rows[0].len();
    let mut mean = vec![0f64; dims];
    for r in rows {
        for (m, v) in mean.iter_mut().zip(r) {
            *m += v / rows.len() as f64;
        }
    }
    let centred: Vec<Vec<f64>> = rows.iter().map(|r| r.iter().zip(&mean).map(|(v, m)| v - m).collect()).collect();
    let mut total = 0.0;
    for i in 0..centred.len() - lag {
        let (a, b) = (&centred[i], &centred[i + lag]);
        let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na = a.iter().map(|x| x * x).sum::<f64>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f64>().sqrt();
        if na > 1e-9 && nb > 1e-9 {
            total += dot / (na * nb);
        }
    }
    total / (centred.len() - lag) as f64
}

/// Downbeat phase and metre over `music` (seconds). `meter` is the bar length when already known.
pub fn downbeat(t: &Tempo, f: &Features, music: (f64, f64), meter: Option<i64>) -> Downbeat {
    if t.period_s <= 0.0 || t.bpm <= 0.0 {
        return Downbeat::default();
    }
    let first = t.grid_pos(music.0).ceil() as i64;
    let last = t.grid_pos(music.1).floor() as i64 - 1;
    if last - first < 16 {
        return Downbeat::default();
    }
    let rise = low_rise(f);
    let reach = ((t.period_s / 8.0) * f.fps).round().max(1.0) as isize;
    let quarter = t.period_s / 4.0;
    let mut kick = Vec::with_capacity((last - first + 1) as usize);
    let mut change = Vec::with_capacity(kick.capacity());
    let mut rhythm: Vec<Vec<f64>> = Vec::with_capacity(kick.capacity());
    let mut prev = chroma_between(f, t.offset_s + (first - 1) as f64 * t.period_s, t.offset_s + first as f64 * t.period_s);
    for n in first..=last {
        let tn = t.offset_s + n as f64 * t.period_s;
        kick.push(peak_near(&rise, frame_of(f, tn), reach));
        let c = chroma_between(f, tn, tn + t.period_s);
        change.push(1.0 - c.iter().zip(&prev).map(|(a, b)| a * b).sum::<f64>());
        // The beat's rhythm: onsets on it and its quarters, low hits on it and half way.
        let mut r: Vec<f64> = (0..4).map(|q| peak_near(&f.onset, frame_of(f, tn + q as f64 * quarter), (reach / 2).max(1))).collect();
        r.push(peak_near(&rise, frame_of(f, tn), reach));
        r.push(peak_near(&rise, frame_of(f, tn + 2.0 * quarter), reach));
        rhythm.push(r);
        prev = c;
    }
    z(&mut kick);
    z(&mut change);
    for d in 0..rhythm[0].len() {
        let mut col: Vec<f64> = rhythm.iter().map(|r| r[d]).collect();
        z(&mut col);
        for (r, v) in rhythm.iter_mut().zip(col) {
            r[d] = v;
        }
    }
    let evidence: Vec<f64> = kick.iter().zip(&change).map(|(k, c)| k + c).collect();
    let fold = |m: i64| -> Vec<f64> {
        let mut score = vec![0f64; m as usize];
        let mut count = vec![0usize; m as usize];
        for (i, n) in (first..=last).enumerate() {
            let p = n.rem_euclid(m) as usize;
            score[p] += evidence[i];
            count[p] += 1;
        }
        score.iter().zip(&count).map(|(s, c)| s / (*c).max(1) as f64).collect()
    };
    let change_rows: Vec<Vec<f64>> = change.iter().map(|c| vec![*c]).collect();
    let sim = |lag: usize| lag_similarity(&rhythm, lag) + 0.5 * lag_similarity(&change_rows, lag);
    let (s2, s3, s4) = (sim(2), sim(3), sim(4));
    let beats_per_bar = match meter {
        Some(m @ (3 | 4)) => m,
        _ if s3 - s2.max(s4) > TRIPLE_MARGIN => 3,
        _ => 4,
    };
    let score = fold(beats_per_bar);
    let mut order: Vec<usize> = (0..score.len()).collect();
    order.sort_by(|a, b| score[*b].total_cmp(&score[*a]));
    let margin = score[order[0]] - score[order[1]];
    Downbeat { phase: order[0] as i32, confidence: ((margin - 0.15) / 0.6).clamp(0.0, 1.0) as f32, beats_per_bar }
}

fn mean_db(v: &[f32]) -> f64 {
    if v.is_empty() {
        return -120.0;
    }
    loudness::db(v.iter().map(|x| *x as f64).sum::<f64>() / v.len() as f64)
}

/// Intro end and outro start, seconds: section changes on 4-bar lines with a usable grid, else on 2 s blocks,
/// falling back to the energy envelope.
pub fn cues(t: &Tempo, db: &Downbeat, f: &Features, music: (f64, f64), grid_ok: bool) -> (f64, f64) {
    let len = music.1 - music.0;
    if len <= 0.0 {
        return (music.0, music.1);
    }
    if grid_ok && t.period_s > 0.0 {
        if let Some(c) = phrase_cues(t, db, f, music) {
            return c;
        }
    }
    // 0.5 s blocks, dB; "loud" is within 3 dB of the median of the audible part.
    let bl: Vec<f64> = f.blocks_raw.chunks(5).map(|c| mean_db(c)).collect();
    let (a, b) = ((music.0 / 0.5) as usize, ((music.1 / 0.5).ceil() as usize).min(bl.len()));
    if b <= a {
        return (music.0, music.1);
    }
    let mut sorted = bl[a..b].to_vec();
    sorted.sort_unstable_by(|x, y| x.total_cmp(y));
    let loud = sorted[sorted.len() / 2] - 3.0;
    let intro = bl[a..b].iter().position(|v| *v >= loud).map_or(music.0, |i| (a + i) as f64 * 0.5).max(music.0);
    let outro = bl[a..b].iter().rposition(|v| *v >= loud).map_or(music.1, |i| (a + i + 1) as f64 * 0.5).min(music.1);
    let units: Vec<f64> = (0..).map(|k| music.0 + BLOCK_S * k as f64).take_while(|u| *u <= music.1).collect();
    let (i, o) = section_changes(f, &units, 1, &t.beats, music);
    let intro = i.unwrap_or(intro);
    let outro = o.unwrap_or(outro);
    (intro, outro.max(intro))
}

fn frames_between<'a>(f: &Features, v: &'a [f32], a: f64, b: f64) -> &'a [f32] {
    let i = frame_of(f, a).max(0) as usize;
    let j = (frame_of(f, b).max(0) as usize).min(v.len());
    v.get(i..j.max(i)).unwrap_or(&[])
}

/// What `[a, b)` sounds like: level, low end, tonal energy, onset density, brightness and voice share.
fn bar_vector(f: &Features, a: f64, b: f64) -> [f64; 6] {
    let mean = |v: &[f32]| if v.is_empty() { 0.0 } else { v.iter().map(|x| *x as f64).sum::<f64>() / v.len() as f64 };
    let tonal = {
        let j0 = ((a - f.chroma_t0) / f.chroma_step).ceil().max(0.0) as usize;
        let j1 = (((b - f.chroma_t0) / f.chroma_step).ceil().max(0.0) as usize).min(f.chroma.len());
        let frames = f.chroma.get(j0..j1.max(j0)).unwrap_or(&[]);
        let sum: f64 = frames.iter().map(|c| c.iter().map(|v| *v as f64).sum::<f64>()).sum();
        loudness::db(sum * sum / frames.len().max(1) as f64 + 1e-12)
    };
    [
        mean_db(frames_between(f, &f.power, a, b)),
        mean_db(frames_between(f, &f.low_power, a, b)),
        tonal,
        (mean(frames_between(f, &f.onset, a, b)) + 1e-3).ln(),
        (mean(frames_between(f, &f.centroid, a, b)) + 1.0).log2(),
        mean(frames_between(f, &f.vocal, a, b)),
    ]
}

/// Unit length of the section search without a beat grid, seconds.
const BLOCK_S: f64 = 2.0;

/// Intro end and outro start among `units` (unit start times, the last one the end): Foote novelty between the 4
/// units before and after each `step`-th boundary. The intro ends at the first early change that does not thin
/// the music; the outro starts at the last late one that does not fill it. Block boundaries snap to `snap` beats.
fn section_changes(f: &Features, units: &[f64], step: usize, snap: &[f64], music: (f64, f64)) -> (Option<f64>, Option<f64>) {
    let n = units.len().saturating_sub(1);
    if n < 12 {
        return (None, None);
    }
    let mut v: Vec<[f64; 6]> = units.windows(2).map(|w| bar_vector(f, w[0], w[1])).collect();
    for d in 0..6 {
        let mean = v.iter().map(|r| r[d]).sum::<f64>() / n as f64;
        let sd = (v.iter().map(|r| (r[d] - mean).powi(2)).sum::<f64>() / n as f64).sqrt().max(0.5 * SECTION_SCALE[d]);
        v.iter_mut().for_each(|r| r[d] = (r[d] - mean) / sd);
    }
    let span = |m0: usize, m1: usize| -> [f64; 6] {
        let mut s = [0f64; 6];
        for r in &v[m0..m1] {
            for (a, x) in s.iter_mut().zip(r) {
                *a += x / (m1 - m0) as f64;
            }
        }
        s
    };
    // (novelty, how much fuller it gets) at unit m: 4 units either side, down to 2 at the end of the song.
    let change = |m: usize| -> (f64, f64) {
        let w = SECTION_SPAN.min(n - m);
        let (before, after) = (span(m - SECTION_SPAN, m), span(m, m + w));
        let novelty = (before.iter().zip(&after).map(|(x, y)| (y - x).powi(2)).sum::<f64>() / 6.0).sqrt();
        let fuller = (after[0] + after[1] + after[2] + after[3] - before[0] - before[1] - before[2] - before[3]) / 4.0;
        (novelty, fuller)
    };
    let novelty_min = if snap.is_empty() { NOVELTY_MIN } else { NOVELTY_MIN_BLOCKS };
    let to_beat = |t: f64| -> f64 {
        snap.iter().copied().filter(|b| (b - t).abs() <= BLOCK_S).min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())).unwrap_or(t)
    };
    let len = music.1 - music.0;
    let candidates: Vec<usize> = (SECTION_SPAN..=n - 2).filter(|m| m % step == 0).collect();
    let intro = candidates
        .iter()
        .take_while(|&&m| units[m] <= music.0 + (0.4 * len).min(90.0) && m + SECTION_SPAN <= n)
        .find(|&&m| {
            let (nov, fuller) = change(m);
            nov >= novelty_min && fuller > -FULLER_EPS
        })
        .map(|&m| to_beat(units[m]));
    let outro_from = (music.0 + 0.5 * len).max(music.1 - 90.0);
    let outro = candidates
        .iter()
        .rev()
        .take_while(|&&m| units[m] >= outro_from)
        .find(|&&m| {
            let (nov, fuller) = change(m);
            nov >= novelty_min && fuller < FULLER_EPS
        })
        .map(|&m| to_beat(units[m]));
    (intro, outro)
}

/// The song's bar lines from the first downbeat of the beat (of the music when the beat starts under two bars in),
/// and whether the opening before them is beatless.
struct Bars {
    start: f64,
    bar: f64,
    beatless: bool,
    /// Bar starts; the last one is the end of the last whole bar.
    units: Vec<f64>,
}

fn bar_lines(t: &Tempo, db: &Downbeat, music: (f64, f64)) -> Option<Bars> {
    if !(t.period_s > 0.0) {
        return None;
    }
    let bpb = if db.beats_per_bar == 3 { 3 } else { 4 };
    let bar = bpb as f64 * t.period_s;
    let beat_from = t.beats.first().copied().unwrap_or(music.0).max(music.0);
    let mut n0 = (t.grid_pos(beat_from) - 0.5).ceil() as i64;
    while n0.rem_euclid(bpb) != db.phase as i64 {
        n0 += 1;
    }
    let mut start = t.offset_s + n0 as f64 * t.period_s;
    // Under two bars before it is a pickup, not an intro.
    let beatless = start - music.0 >= 2.0 * bar;
    if !beatless {
        while start - bar >= music.0 - 0.5 * t.period_s {
            start -= bar;
        }
    }
    let bars = ((music.1 - start) / bar).floor() as i64;
    if bars < 16 {
        return None;
    }
    Some(Bars { start, bar, beatless, units: (0..=bars).map(|m| start + m as f64 * bar).collect() })
}

/// How far into the song a drop may be (further is a chorus, or beyond any mix).
const DROP_REACH_S: f64 = 75.0;
/// A drop reaches the song's median bar within `DROP_BODY_DB` (level, low end, tonal energy) from bars that lacked
/// `DROP_LACK_DB` of one of them.
const DROP_BODY_DB: [f64; 3] = [2.0, 3.0, 3.0];
const DROP_LACK_DB: [f64; 3] = [3.0, 3.0, 3.0];

/// The drop: the first early four-bar line where the song reaches its body (level, low end and chords) from bars
/// that lacked one. A beatless opening into the full band drops on the first downbeat.
pub fn drop_point(t: &Tempo, db: &Downbeat, f: &Features, music: (f64, f64)) -> Option<Drop> {
    let b = bar_lines(t, db, music)?;
    let n = b.units.len() - 1;
    let v: Vec<[f64; 6]> = b.units.windows(2).map(|w| bar_vector(f, w[0], w[1])).collect();
    let median = |d: usize| {
        let mut x: Vec<f64> = v.iter().map(|r| r[d]).collect();
        x.sort_by(|a, b| a.total_cmp(b));
        x[x.len() / 2]
    };
    let body = [median(0), median(1), median(2)];
    let mean = |rows: &[[f64; 6]]| -> [f64; 3] {
        let k = rows.len().max(1) as f64;
        [0, 1, 2].map(|d| rows.iter().map(|r| r[d]).sum::<f64>() / k)
    };
    let arrives = |after: [f64; 3]| (0..3).all(|d| after[d] >= body[d] - DROP_BODY_DB[d]);
    let lacked = |before: [f64; 3]| (0..3).any(|d| before[d] < body[d] - DROP_LACK_DB[d]);
    let reach = music.0 + (0.4 * (music.1 - music.0)).min(DROP_REACH_S);
    if b.beatless && arrives(mean(&v[..SECTION_SPAN.min(n)])) {
        let opening = bar_vector(f, music.0, b.start);
        if lacked([opening[0], opening[1], opening[2]]) {
            return Some(Drop { at: b.start, runup_tonal_db: (opening[2] - body[2]) as f32 });
        }
    }
    (SECTION_SPAN..=n.saturating_sub(SECTION_SPAN))
        .step_by(SECTION_SPAN)
        .take_while(|&m| b.units[m] <= reach)
        .find(|&m| arrives(mean(&v[m..m + SECTION_SPAN])) && lacked(mean(&v[m - SECTION_SPAN..m])))
        .map(|m| {
            // The most chordal four bars of the run-up, beatless opening included.
            let opening = if b.beatless { bar_vector(f, music.0, b.start)[2] } else { f64::MIN };
            let most = (0..m).step_by(SECTION_SPAN).map(|i| mean(&v[i..(i + SECTION_SPAN).min(m)])[2]).fold(opening, f64::max);
            Drop { at: b.units[m], runup_tonal_db: (most - body[2]) as f32 }
        })
}

/// A drop, and the tonal energy of the most chordal four bars before it relative to the body, dB.
#[derive(Clone, Copy, Debug)]
pub struct Drop {
    pub at: f64,
    pub runup_tonal_db: f32,
}

/// A closing breakdown: the level falls this far from the four bars before,
const BREAK_DB: f64 = 6.0;
/// no bar after it comes back within this much,
const BREAK_STAY_DB: f64 = 3.0;
/// and the low end falls this far or the onsets thin by this much (ln).
const BREAK_LOW_DB: f64 = 6.0;
const BREAK_ONSET: f64 = 0.5;
/// Only a short ending counts; a longer quiet stretch is part of the song.
pub const BREAK_MAX_S: f64 = 24.0;

/// Where a closing breakdown (coda, fade-out) begins before `end`: the earliest unit in the last `BREAK_MAX_S`
/// where level and beat drop and never come back. Bars with a usable grid, else 2 s blocks snapped to beats.
pub fn breakdown(t: &Tempo, db: &Downbeat, f: &Features, music: (f64, f64), end: f64, grid_ok: bool) -> Option<f64> {
    let (units, snap): (Vec<f64>, &[f64]) = match bar_lines(t, db, (music.0, end)).filter(|_| grid_ok && t.period_s > 0.0) {
        Some(b) => (b.units, &[]),
        None => ((0..).map(|k| music.0 + BLOCK_S * k as f64).take_while(|u| *u <= end).collect(), &t.beats),
    };
    let n = units.len().checked_sub(1)?;
    if n < SECTION_SPAN + 1 {
        return None;
    }
    let v: Vec<[f64; 6]> = units.windows(2).map(|w| bar_vector(f, w[0], w[1])).collect();
    let mean = |rows: &[[f64; 6]]| -> [f64; 6] {
        let k = rows.len().max(1) as f64;
        [0, 1, 2, 3, 4, 5].map(|d| rows.iter().map(|r| r[d]).sum::<f64>() / k)
    };
    let m = (SECTION_SPAN..n).filter(|&m| end - units[m] <= BREAK_MAX_S).find(|&m| {
        let (before, after) = (mean(&v[m - SECTION_SPAN..m]), mean(&v[m..n]));
        after[0] <= before[0] - BREAK_DB
            && (after[1] <= before[1] - BREAK_LOW_DB || after[3] <= before[3] - BREAK_ONSET)
            && v[m..n].iter().all(|r| r[0] <= before[0] - BREAK_STAY_DB)
    })?;
    let at = units[m];
    Some(snap.iter().copied().filter(|b| (b - at).abs() <= BLOCK_S).min_by(|a, b| (a - at).abs().total_cmp(&(b - at).abs())).unwrap_or(at))
}

/// `cues` on the bar grid; a beatless opening is the intro.
fn phrase_cues(t: &Tempo, db: &Downbeat, f: &Features, music: (f64, f64)) -> Option<(f64, f64)> {
    let Bars { start, bar, beatless, units } = bar_lines(t, db, music)?;
    let bars = units.len() as i64 - 1;
    let at = |m: i64| start + m as f64 * bar;
    let (intro, outro) = section_changes(f, &units, 4, &[], music);
    let intro = if beatless { start } else { intro.unwrap_or(music.0) };
    // No change found: the last 8-bar line at least 16 bars before the end.
    let outro = outro.unwrap_or_else(|| (1..).map(|k| 8 * k).take_while(|&m| m + 16 <= bars).last().map_or(music.0, at));
    Some((intro.max(music.0), outro.clamp(intro.max(music.0), music.1)))
}

/// Temperley's major profile (keeps fifth partials from reading as the dominant) and Krumhansl-Kessler's minor
/// (favours the Aeolian flat seventh common in pop), chosen on `eval.rs` and `keys_of_simple_progressions`.
const MAJOR: [f64; 12] = [5.0, 2.0, 3.5, 2.0, 4.5, 4.0, 2.0, 4.5, 2.0, 3.5, 1.5, 4.0];
const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

fn pearson(a: &[f64; 12], b: &[f64; 12], shift: usize) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut dbb) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let x = a[(i + shift) % 12] - ma;
        let y = b[i] - mb;
        num += x * y;
        da += x * x;
        dbb += y * y;
    }
    if da <= 0.0 || dbb <= 0.0 {
        0.0
    } else {
        num / (da * dbb).sqrt()
    }
}

/// Camelot code of a key: 1..12 minor (A), 13..24 major (B).
pub fn camelot(tonic: usize, minor: bool) -> i32 {
    let major_pc = if minor { (tonic + 3) % 12 } else { tonic % 12 };
    let num = (7 * major_pc + 7) % 12 + 1;
    num as i32 + if minor { 0 } else { 12 }
}

/// The tuning, semitones from A = 440 Hz (-0.5..0.5), from the peaks' circular mean.
pub fn tuning(f: &Features) -> f64 {
    let (c, s) = f.tuning_cs;
    if c.hypot(s) <= 1e-9 {
        0.0
    } else {
        s.atan2(c) / (2.0 * std::f64::consts::PI)
    }
}

/// The song's pitch-class profile with its tuning removed: each 10-cent slot goes to its nearest tuned pitch class,
/// weighted down towards the half-semitone.
pub fn tuned_profile(f: &Features) -> [f64; 12] {
    let tune = tuning(f);
    let slots = f.pitch.len().max(1) as f64;
    let mut c = [0f64; 12];
    for (s, v) in f.pitch.iter().enumerate() {
        let x = s as f64 * 12.0 / slots - tune;
        let d = x - x.round();
        let w = (1.0 - 2.0 * d.abs()).max(0.0);
        c[(x.round() as i64).rem_euclid(12) as usize] += w * v;
    }
    c
}

/// `(camelot code, confidence)` of a pitch-class profile; `(0, 0)` without tonal content.
pub fn key_of(c: [f64; 12]) -> (i32, f32) {
    let mean = c.iter().sum::<f64>() / 12.0;
    if mean <= 1e-9 {
        return (0, 0.0);
    }
    let contrast = (c.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / 12.0).sqrt() / mean;
    let mut scores: Vec<(f64, i32)> = Vec::with_capacity(24);
    for tonic in 0..12 {
        scores.push((pearson(&c, &MAJOR, tonic), camelot(tonic, false)));
        scores.push((pearson(&c, &MINOR, tonic), camelot(tonic, true)));
    }
    scores.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (best, code) = scores[0];
    let conf = ((best - 0.5) / 0.35).clamp(0.0, 1.0) * ((contrast - 0.1) / 0.3).clamp(0.0, 1.0);
    (code, conf as f32)
}

/// "8B", "11A"; empty for 0.
pub fn camelot_name(code: i32) -> String {
    match code {
        1..=12 => format!("{code}A"),
        13..=24 => format!("{}B", code - 12),
        _ => String::new(),
    }
}

/// Steps on the Camelot wheel: 0 same key, 1 neighbour or relative major/minor; None when either key is unknown.
pub fn key_distance(a: i32, b: i32) -> Option<i32> {
    if !(1..=24).contains(&a) || !(1..=24).contains(&b) {
        return None;
    }
    let d = ((a - 1) % 12 - (b - 1) % 12).rem_euclid(12);
    Some(d.min(12 - d) + ((a > 12) != (b > 12)) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelot_codes_match_wheel() {
        assert_eq!(camelot_name(camelot(0, false)), "8B"); // C major
        assert_eq!(camelot_name(camelot(9, true)), "8A"); // A minor
        assert_eq!(camelot_name(camelot(7, false)), "9B"); // G major
        assert_eq!(camelot_name(camelot(11, false)), "1B"); // B major
        assert_eq!(camelot_name(camelot(0, true)), "5A"); // C minor
        assert_eq!(camelot_name(camelot(1, true)), "12A"); // C# minor
        assert_eq!(camelot_name(0), "");
        assert_eq!(key_distance(camelot(0, false), camelot(9, true)), Some(1), "relative minor");
        assert_eq!(key_distance(camelot(0, false), camelot(7, false)), Some(1), "a fifth up");
        assert_eq!(key_distance(camelot(0, false), camelot(6, false)), Some(6), "tritone");
        assert_eq!(key_distance(camelot(0, false), camelot(0, false)), Some(0));
        assert_eq!(key_distance(0, 5), None);
    }

    #[test]
    fn key_profiles_know_own_shape() {
        let rot = |p: &[f64; 12], t: usize| -> [f64; 12] { std::array::from_fn(|i| p[(i + 12 - t) % 12]) };
        assert_eq!(key_of(rot(&MAJOR, 2)).0, camelot(2, false));
        assert_eq!(key_of(rot(&MINOR, 4)).0, camelot(4, true));
        assert_eq!(key_of([1.0; 12]).1, 0.0, "a flat profile is not trusted");
        assert_eq!(key_of([0.0; 12]), (0, 0.0));
    }
}
