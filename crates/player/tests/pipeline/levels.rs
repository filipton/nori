//! Measured transition loudness (100 ms windows): per-song gains, no steps, gentle loudness-match release.

use nori_player::automix::ANALYSIS_VERSION;
use nori_player::sim::{prefs_off, Player};
use nori_player::transitions::TransitionPrefs;
use nori_player::types::TrackAnalysis;

use crate::common::*;

const SONG_S: f64 = 60.0;
/// The level window: 100 ms.
const WIN_S: f64 = 0.1;

/// Partials and a little noise at a fixed level, so any change in measured level is the player's.
fn steady(secs: f64, seed: u64, amp: f64) -> Vec<i16> {
    cached((1, secs.to_bits(), seed, amp.to_bits()), || make_steady(secs, seed, amp))
}

fn make_steady(secs: f64, seed: u64, amp: f64) -> Vec<i16> {
    let detune = 1.0 + (seed % 7) as f64 * 0.13;
    let parts = [(110.0, 0.5), (220.0, 0.3), (330.0, 0.25), (523.0, 0.2), (1250.0, 0.12), (2900.0, 0.08)];
    let mut r = nori_player::automix::synth::Rng(seed);
    let norm: f64 = parts.iter().map(|(_, a)| a).sum::<f64>() + 0.1;
    (0..frames(secs))
        .flat_map(|i| {
            let t = i as f64 / RATE as f64;
            let tone: f64 = parts.iter().enumerate().map(|(k, (hz, a))| a * (std::f64::consts::TAU * hz * detune * t + k as f64).sin()).sum();
            let l = amp * (tone + 0.1 * r.next()) / norm;
            let rr = amp * (0.9 * tone + 0.1 * r.next()) / norm;
            [(l * 32767.0).round() as i16, (rr * 32767.0).round() as i16]
        })
        .collect()
}

/// Silence as long as a song.
fn silence(secs: f64) -> Vec<i16> {
    vec![0; frames(secs) * 2]
}

fn at(s: &[i16], gain: f32) -> Vec<i16> {
    s.iter().map(|&v| (v as f32 * gain).round() as i16).collect()
}

fn gain_db(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A song measured as steady music at `bpm` from end to end, at `lufs`.
fn measured(id: &str, bpm: f64, lufs: f32) -> TrackAnalysis {
    let ms = (SONG_S * 1000.0) as i64;
    TrackAnalysis {
        song_id: id.into(),
        analysis_version: ANALYSIS_VERSION,
        duration_ms: ms,
        bpm,
        bpm_confidence: 1.0,
        beat_offset_ms: 250.0,
        stability: 1.0,
        downbeat_confidence: 1.0,
        beats_per_bar: 4,
        lufs,
        silence_end_ms: ms,
        mixramp_end_ms: ms,
        intro_end_ms: 250,
        outro_start_ms: ms - 16_000,
        outro_bpm: bpm,
        outro_bpm_confidence: 1.0,
        outro_beat_offset_ms: 250.0,
        outro_stability: 1.0,
        intro_bpm: bpm,
        intro_bpm_confidence: 1.0,
        intro_beat_offset_ms: 250.0,
        intro_stability: 1.0,
        ..Default::default()
    }
}

/// One way through a transition: the settings, what the two songs measured as, and the words the
/// planner's log line carries for it.
struct Kind {
    name: &'static str,
    prefs: TransitionPrefs,
    a: TrackAnalysis,
    b: TrackAnalysis,
    logged: &'static [&'static str],
}

fn automix_prefs() -> TransitionPrefs {
    TransitionPrefs { auto_mix: true, auto_mix_max_s: 16, echo_out: false, ..prefs_off() }
}

/// Every kind of transition, the songs at `lufs_a` and `lufs_b` as AutoMix measured them.
fn kinds(lufs_a: f32, lufs_b: f32) -> Vec<Kind> {
    let (a, b) = (measured("a", 120.0, lufs_a), measured("b", 123.0, lufs_b));
    let same_tempo = measured("b", 120.0, lufs_b);
    // Far-apart keys: the pair echoes out instead of blending.
    let keyed = |t: &TrackAnalysis, key: i32| TrackAnalysis { key, key_confidence: 0.9, ..t.clone() };
    // Five bars of music at the end of the outgoing song after a long quiet stretch, and the incoming
    // song's intro twelve bars long: the run-up reads the outgoing song's last four bars round.
    let bar = 2_000;
    let music_from = (SONG_S * 1000.0) as i64 - 5 * bar;
    let short_outro = TrackAnalysis {
        silence_start_ms: music_from - 1_500,
        silence_end_ms: (SONG_S * 1000.0) as i64 - 1_500,
        outro_start_ms: music_from,
        mixramp_end_ms: music_from + 5 * bar / 2,
        ..a.clone()
    };
    let long_intro = TrackAnalysis { intro_end_ms: 250 + 12 * bar, silence_start_ms: 100, ..same_tempo.clone() };
    vec![
        Kind { name: "equal-power crossfade", prefs: crossfade(6), a: a.clone(), b: b.clone(), logged: &["EqualPowerFade"] },
        Kind {
            name: "beat-matched, stretched, bass swap",
            prefs: automix_prefs(),
            a: a.clone(),
            b: b.clone(),
            logged: &["BeatMatched", "tempo x0.976"],
        },
        Kind {
            name: "beat-matched, no bass swap, no filters",
            prefs: TransitionPrefs { bass_swap: false, filter_effects: false, ..automix_prefs() },
            a: a.clone(),
            b: same_tempo.clone(),
            logged: &["BeatMatched", "tempo x1.000"],
        },
        Kind {
            name: "beat-matched, the outro looped",
            prefs: automix_prefs(),
            a: short_outro,
            b: long_intro,
            logged: &["BeatMatched", "remix"],
        },
        Kind {
            name: "echo-out",
            prefs: TransitionPrefs { echo_out: true, ..automix_prefs() },
            a: keyed(&a, 1),
            b: keyed(&same_tempo, 7),
            logged: &["EchoOut"],
        },
        Kind {
            name: "MixRamp fade",
            prefs: TransitionPrefs { beat_match: false, ..automix_prefs() },
            a: a.clone(),
            b: b.clone(),
            logged: &["MixRampFade", "mixramp fade"],
        },
        Kind {
            name: "downbeat-aligned fade",
            prefs: automix_prefs(),
            a: a.clone(),
            b: TrackAnalysis { bpm_confidence: 0.1, intro_bpm_confidence: 0.1, ..b.clone() },
            logged: &["MixRampFade", "downbeat-aligned"],
        },
    ]
}

/// A run's output and where its mix is heard, from the planner's log line: (start, length), frames.
struct Run {
    out: Vec<i16>,
    mix: (usize, usize),
    line: String,
}

fn run(kind: &Kind, a: &[i16], b: &[i16], gains: &[(&str, f32)], replay_gain: bool) -> Run {
    run_listed(kind, track("a", a), b, gains, replay_gain)
}

fn run_listed(kind: &Kind, a: nori_player::sim::Track, b: &[i16], gains: &[(&str, f32)], replay_gain: bool) -> Run {
    let prefs = TransitionPrefs { replay_gain, ..kind.prefs };
    let mut p = Player::with_prefs(vec![a, track("b", b)], prefs);
    p.measure_on_move = false;
    for t in [&kind.a, &kind.b] {
        if !t.song_id.is_empty() {
            p.app.analyses.insert(t.song_id.clone(), t.clone());
        }
    }
    for (id, g) in gains {
        p.app.gains.insert(id.to_string(), *g);
    }
    p.play_from(0);
    assert!(p.run_to_end(200_000), "{}: {:?}", kind.name, p.app.log);
    assert!(p.sink.gaps.is_empty(), "{}: {:?}", kind.name, p.sink.gaps);
    let log = p.app.log.clone();
    let line = log.iter().find(|l| l.starts_with("transition a -> b")).unwrap_or_else(|| panic!("{}: no transition: {log:?}", kind.name)).clone();
    for w in kind.logged {
        assert!(line.contains(w), "{}: planned as {line}", kind.name);
    }
    assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{}: {log:?}", kind.name);
    // "transition a -> b: Kind <len> ms at <start>, ..."
    let words: Vec<&str> = line.split_whitespace().collect();
    let len_ms: f64 = words[5].parse().unwrap();
    let start_ms: f64 = words[8].trim_end_matches(',').parse().unwrap();
    Run { out: p.sink.heard_samples(), mix: (frames(start_ms / 1000.0), frames(len_ms / 1000.0)), line }
}

/// The level of `x` (left channel, full scale 1) in dB over each 100 ms from frame `from` to `to`.
fn levels(x: &[f64], from: usize, to: usize) -> Vec<f64> {
    let w = frames(WIN_S);
    (from..to.min(x.len()).saturating_sub(w)).step_by(w).map(|i| db(rms(&x[i..i + w]))).collect()
}

/// The largest move of the level from one 100 ms to the next, and where (the window's index).
fn largest_move(l: &[f64]) -> (f64, usize) {
    l.windows(2).enumerate().map(|(i, w)| ((w[1] - w[0]).abs(), i)).fold((0.0, 0), |m, v| if v.0 > m.0 { v } else { m })
}

/// Loud raw song a at −6 dB of ReplayGain, quiet raw song b at −2 dB: a's level after its gain is
/// 4 dB above b's raw, and b is heard 2 dB under its own.
fn songs() -> (Vec<i16>, Vec<i16>, f32, f32) {
    (steady(SONG_S, 31, 0.5), steady(SONG_S, 32, 0.25), gain_db(-6.0), gain_db(-2.0))
}

#[test]
fn each_mix_kind_uses_song_gains() {
    let (a, b, ga, gb) = songs();
    for kind in kinds(-14.0, -14.0) {
        // The ideal: each song turned to its own volume first, then played with no ReplayGain at all.
        let ideal = run(&kind, &at(&a, ga), &at(&b, gb), &[], true);
        let got = run(&kind, &a, &b, &[("a", ga), ("b", gb)], true);
        assert_eq!(got.line, ideal.line, "{}: the volumes leave the plan alone", kind.name);
        assert_eq!(got.out.len(), ideal.out.len(), "{}: and the timing", kind.name);
        let off = got.out.iter().zip(&ideal.out).map(|(g, i)| (*g as i32 - *i as i32).abs()).max().unwrap_or(0);
        let (x, y) = (left(&got.out), left(&ideal.out));
        let (s, n) = got.mix;
        let (lg, li) = (levels(&x, s, s + n + frames(2.0)), levels(&y, s, s + n + frames(2.0)));
        let worst = lg.iter().zip(&li).map(|(g, i)| (g - i).abs()).fold(0.0, f64::max);
        println!("{}: {}; largest sample difference {off}, largest level difference {worst:.3} dB", kind.name, got.line);
        // A step of dither where the gain meets the 16-bit grid, and one for each rounding of the mix (a looped outro mixes twice). A stretch
        // picks where it splices by the samples themselves, so a step of dither may move a splice: there the
        // level says it (below), not each sample.
        assert!(off <= 3 || kind.name.contains("stretched"), "{}: every sample as the gain-then-mix one (off by {off})", kind.name);
        assert!(worst < 0.01, "{}: {worst:.3} dB from the gain-then-mix level", kind.name);
        // Either side of the mix, each song alone at its own level.
        let own = |s: &[i16], g: f32| db(rms(&left(&at(s, g))[frames(10.0)..frames(20.0)]));
        let before = db(rms(&x[s - frames(3.0)..s - frames(1.0)]));
        let after = db(rms(&x[s + n + frames(8.0)..s + n + frames(10.0)]));
        assert!((before - own(&a, ga)).abs() < 0.1, "{}: a at {before:.2} dB, its own {:.2}", kind.name, own(&a, ga));
        assert!((after - own(&b, gb)).abs() < 0.1, "{}: b at {after:.2} dB, its own {:.2}", kind.name, own(&b, gb));
    }
}

#[test]
fn incoming_song_at_own_level() {
    // The outgoing song silent: all that is heard during the mix is the incoming one, at its own
    // ReplayGain times the mix curve. Measured against the same with no ReplayGain on either song,
    // the difference is b's own gain all the way - never a's, never none.
    let (_, b, ga, gb) = songs();
    let quiet = silence(SONG_S);
    for kind in kinds(-14.0, -14.0) {
        let flat = run(&kind, &quiet, &b, &[], true);
        let got = run(&kind, &quiet, &b, &[("a", ga), ("b", gb)], true);
        let (s, n) = got.mix;
        let (lg, lf) = (levels(&left(&got.out), s, s + n), levels(&left(&flat.out), s, s + n));
        // Where the incoming song is heard at all (the curve above −40 dB of it).
        let peak = lf.iter().cloned().fold(f64::MIN, f64::max);
        let diffs: Vec<f64> = lg.iter().zip(&lf).filter(|(_, f)| **f > peak - 40.0).map(|(g, f)| g - f).collect();
        let (lo, hi) = diffs.iter().fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
        println!("{}: b under its gain through the mix: {lo:.3}..{hi:.3} dB (its gain {:.2} dB)", kind.name, 20.0 * gb.log10());
        assert!(!diffs.is_empty(), "{}", kind.name);
        assert!((lo - 20.0 * gb.log10() as f64).abs() < 0.05 && (hi - 20.0 * gb.log10() as f64).abs() < 0.05, "{}: {lo:.3}..{hi:.3} dB", kind.name);
    }
}

#[test]
fn no_level_step() {
    let (a, b, ga, gb) = songs();
    for kind in kinds(-14.0, -14.0) {
        let got = run(&kind, &a, &b, &[("a", ga), ("b", gb)], true);
        let (s, n) = got.mix;
        // From the last 200 ms of the mix to 25 seconds past it: a stretch ramps back and hands
        // the song back to its own clock in there.
        let l = levels(&left(&got.out), s + n - frames(0.2), s + n + frames(25.0));
        let (step, i) = largest_move(&l);
        println!("{}: largest 100 ms move from the end of the mix on: {step:.3} dB, {:.1} s after it", kind.name, i as f64 * WIN_S - 0.2);
        assert!(step <= 1.0, "{}: the level moves {step:.2} dB in 100 ms, {:.1} s after the mix ends ({})", kind.name, i as f64 * WIN_S - 0.2, got.line);
    }

    // Short song mix ends without step.
    // The server lists the outgoing song a second and a half longer than its audio (a length rounded up,
    // a VBR estimate): a plan made with that length runs past the end of what is held, and the mix must
    // still finish its curves - the incoming song fading up on its own - rather than drop them where the
    // held audio ends. The outgoing song 6 dB louder than the incoming one; with ReplayGain off, AutoMix
    // matches the two, and that match must be let go gently too.
    let (a, b) = (steady(SONG_S, 31, 0.5), steady(SONG_S, 32, 0.25));
    // Its music dies away over its last three seconds, as a song's does: what is measured is the mix.
    let end = frames(SONG_S - 1.5);
    let fade = frames(3.0);
    let short: Vec<i16> = a[..end * 2].iter().enumerate().map(|(k, &v)| (v as f64 * ((end - k / 2) as f64 / fade as f64).min(1.0)).round() as i16).collect();
    let short = &short[..];
    let mut all = kinds(-8.0, -14.0);
    all.push(Kind {
        name: "AutoMix, nothing measured",
        prefs: TransitionPrefs { auto_mix_max_s: 12, ..automix_prefs() },
        a: TrackAnalysis::default(),
        b: TrackAnalysis::default(),
        logged: &["EqualPowerFade", "not analysed"],
    });
    let mut bad = Vec::new();
    let mut cut = 0;
    for kind in all {
        for replay_gain in [true, false] {
            let listed = track("a", short).listed_as((SONG_S * 1000.0) as i64);
            let got = run_listed(&kind, listed, &b, &[], replay_gain);
            let (s, n) = got.mix;
            if s + n <= end {
                continue;
            }
            cut += 1;
            let l = levels(&left(&got.out), end - frames(0.2), end + frames(1.0));
            let (step, i) = largest_move(&l);
            let what = format!("{} (loudness match {})", kind.name, if replay_gain { "off" } else { "on" });
            println!("{what}: largest 100 ms move where the held audio ends: {step:.3} dB, {:.1} s after it ({})", i as f64 * WIN_S - 0.2, got.line);
            if step > 1.0 {
                bad.push(format!("{what}: {step:.2} dB in 100 ms, {:.1} s after the held audio ends ({})", i as f64 * WIN_S - 0.2, got.line));
            }
        }
    }
    assert!(cut >= 4, "the plans that run past the audio: {cut}");
    assert!(bad.is_empty(), "{bad:#?}");
}

/// The loudness match's own curve for `kind`, dB per 100 ms of the mix: the incoming song alone (the
/// outgoing one silent), with the match on (ReplayGain off) against the same with it off.
fn trim_curve(kind: &Kind) -> (Vec<f64>, Run) {
    let b = steady(SONG_S, 52, 0.2);
    let quiet = silence(SONG_S);
    let matched = run(kind, &quiet, &b, &[], false);
    let plain = run(kind, &quiet, &b, &[], true);
    assert_eq!(matched.mix, plain.mix, "{}: {} against {}", kind.name, matched.line, plain.line);
    let (s, n) = matched.mix;
    let (lm, lp) = (levels(&left(&matched.out), s, s + n + frames(3.0)), levels(&left(&plain.out), s, s + n + frames(3.0)));
    let peak = lp.iter().cloned().fold(f64::MIN, f64::max);
    (lm.iter().zip(&lp).map(|(m, p)| if *p > peak - 40.0 { m - p } else { f64::NAN }).collect(), matched)
}

/// The incoming song measured louder or quieter than the song before it, by 6 dB (the most that keeps
/// the mix long) and by 9 (the most the match turns, over a mix a loudness gap has shortened): one test
/// each, so the four run side by side.
/// Into songs 6 and 9 dB quieter and louder.
#[test]
fn loudness_match_releases_gently() {
    for (la, lb) in [(-8.0, -14.0), (-14.0, -8.0), (-5.0, -14.0), (-14.0, -5.0)] {
        loudness_match_lets_go_gently(la, lb);
    }
}

fn loudness_match_lets_go_gently(la: f32, lb: f32) {
    let mut bad = Vec::new();
    {
        for kind in kinds(la, lb).into_iter().skip(1) {
            let (curve, r) = trim_curve(&kind);
            let known: Vec<(usize, f64)> = curve.iter().cloned().enumerate().filter(|(_, v)| v.is_finite()).collect();
            let first = known.first().map_or(0.0, |v| v.1);
            let last = known.last().map_or(0.0, |v| v.1);
            let step = known.windows(2).filter(|w| w[1].0 == w[0].0 + 1).map(|w| (w[1].1 - w[0].1).abs()).fold(0.0, f64::max);
            println!("{} ({la} -> {lb} LUFS): matched by {first:+.2} dB, {last:+.2} dB after, largest 100 ms move {step:.2} dB; {}", kind.name, r.line);
            assert!((first - (la - lb) as f64).abs() < 0.3, "{}: the incoming song matched by {first:.2} dB, not {}", kind.name, la - lb);
            assert!(last.abs() < 0.05, "{}: {last:.2} dB still turned after the mix", kind.name);
            if step > 1.0 {
                bad.push(format!("{} ({la} -> {lb} LUFS): the match lets go by {step:.2} dB in 100 ms ({})", kind.name, r.line));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}



#[test]
fn tiny_stretch_keeps_level() {
    // Stretched by 0.075 %, the incoming song came out of the pitch-keeping stretcher up to 2.6 dB quiet (on
    // noise-like music: cymbals, a crowd, a pad) for the whole mix and the ramp after it, and came back with a
    // step where the stretch handed over. The outgoing song silent, so what is heard is the incoming one.
    let b = noise(0.3, SONG_S, 77);
    let quiet = silence(SONG_S);
    let kind = Kind {
        name: "beat-matched, 120.09 into 120 BPM",
        prefs: TransitionPrefs { bass_swap: false, filter_effects: false, ..automix_prefs() },
        a: measured("a", 120.0, -14.0),
        b: measured("b", 120.09, -14.0),
        logged: &["BeatMatched", "tempo x0.999"],
    };
    let got = run(&kind, &quiet, &b, &[], true);
    let (s, n) = got.mix;
    let x = left(&got.out);
    let own = db(rms(&left(&b)[frames(30.0)..frames(40.0)]));
    // From where the incoming song is fully in (the second half of the mix) to twenty seconds past it.
    let l = levels(&x, s + n / 2, s + n + frames(20.0));
    let (lo, hi) = l.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v - own), hi.max(*v - own)));
    let (step, i) = largest_move(&l);
    println!("{}: {}; the incoming song at {lo:.2}..{hi:.2} dB of its own, largest 100 ms move {step:.2} dB", kind.name, got.line);
    assert!(lo > -0.6 && hi < 0.6, "{}: the incoming song at {lo:.2}..{hi:.2} dB of its own level", kind.name);
    assert!(step <= 1.0, "{}: {step:.2} dB in 100 ms, {:.1} s into the second half of the mix", kind.name, i as f64 * WIN_S);
}

