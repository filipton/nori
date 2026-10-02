//! Whole-pipeline tests on synthetic tracks with a known tempo, downbeat, key and structure.

use super::structure::camelot;
use super::*;
use super::eval::{grid, precision, Song, Style, DRUMS, FULL, KEYS};
use super::synth::*;

fn octave_ok(got: f64, want: f64, tol: f64) -> bool {
    [0.5, 1.0, 2.0].iter().any(|k| (got / (want * k) - 1.0).abs() < tol)
}

/// `f` over every case, one thread each, results in case order (rendering and analysis dominate test time).
fn each<C: Sync, R: Send>(cases: &[C], f: impl Fn(&C) -> R + Sync) -> Vec<R> {
    std::thread::scope(|s| cases.iter().map(|c| s.spawn(|| f(c))).collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap()).collect())
}

/// Click tracks at 90/120/128/174 BPM, straight, swung and noisy.
#[test]
fn tempo_accuracy() {
    let mut cases = Vec::new();
    for bpm in [90.0, 120.0, 128.0, 174.0] {
        cases.push((bpm, "plain", Synth::new(bpm)));
        cases.push((bpm, "swing", Synth { offbeat: 0.67, ..Synth::new(bpm) }));
        cases.push((bpm, "noise", Synth { noise: 0.15, ..Synth::new(bpm) }));
        cases.push((bpm, "swing+noise+chords", Synth { offbeat: 0.67, noise: 0.1, chords: vec![(0, false), (5, false)], ..Synth::new(bpm) }));
    }
    let got = each(&cases, |(_, _, synth)| analyse("t", &synth.render(), synth.rate));
    let (mut acc1, mut acc2, mut wrong) = (0, 0, Vec::new());
    for ((bpm, name, _), a) in cases.iter().zip(&got) {
        let t = &a.track;
        let err = (t.bpm / bpm - 1.0) * 100.0;
        let row = format!("{bpm:>5} {name:<20} bpm {:>7.2} ({err:+.2} %) raw {:>7.2} conf {:.2} stab {:.2} downbeat {} ({:.2})", t.bpm, a.tempo.raw_bpm, t.bpm_confidence, t.stability, t.downbeat_phase, t.downbeat_confidence);
        println!("{row}");
        acc1 += usize::from((t.bpm / bpm - 1.0).abs() < 0.04);
        acc2 += usize::from(octave_ok(t.bpm, *bpm, 0.04));
        if !octave_ok(t.bpm, *bpm, 0.01) || t.bpm_confidence < 0.5 || t.stability < 0.6 {
            wrong.push(row);
        }
    }
    let n = cases.len();
    println!("Acc1 {acc1}/{n}, Acc2 {acc2}/{n}");
    assert!(wrong.is_empty(), "off by more than 1 % (octaves allowed), confidence under 0.5 or stability under 0.6:\n{}", wrong.join("\n"));
    assert!(acc1 >= n - 2, "Acc1 {acc1}/{n}");
}

#[test]
fn grid_is_precise() {
    let bpms = [90.0, 123.0, 128.0, 140.5];
    let got = each(&bpms, |&bpm| {
        let s = Synth { secs: 90.0, ..Synth::new(bpm) };
        analyse("t", &s.render(), s.rate).track.bpm
    });
    for (bpm, got) in bpms.iter().zip(got) {
        assert!((got - bpm).abs() < 0.05, "{bpm} BPM read as {got}");
    }

    // `offset + n * period` within a few ms of every true beat.
    let cases: Vec<(f64, f64)> = [90.0, 128.0, 174.0].into_iter().flat_map(|bpm| [(bpm, 0.1), (bpm, 0.33)]).collect();
    let synth = |&(bpm, first): &(f64, f64)| Synth { first_beat: first, lead_silence: 1.5, ..Synth::new(bpm) };
    let got = each(&cases, |c| {
        let s = synth(c);
        analyse("t", &s.render(), s.rate).track
    });
    for (c, t) in cases.iter().zip(got) {
        let (bpm, first) = *c;
        let period = 60_000.0 / t.bpm;
        let mut worst = 0f64;
        let mut mean = 0f64;
        let truth = synth(c).beats();
        for b in &truth {
            let ms = b * 1000.0;
            let n = ((ms - t.beat_offset_ms) / period).round();
            let d = ms - (t.beat_offset_ms + n * period);
            worst = worst.max(d.abs());
            mean += d;
        }
        mean /= truth.len() as f64;
        println!("{bpm} first {first}: grid error mean {mean:+.2} ms, worst {worst:.2} ms");
        assert!(worst < 8.0, "{bpm}/{first}: grid is {worst} ms off (mean {mean})");
        assert!(t.beat_offset_ms >= 0.0 && t.beat_offset_ms < period, "{bpm}/{first}: offset {} ms outside one beat of {period} ms", t.beat_offset_ms);
    }
}

#[test]
fn downbeats_follow_the_kick() {
    let phases = [0, 1, 2, 3];
    let synth = |phase: usize| Synth { downbeat: phase, chords: vec![(0, false), (7, false), (9, true), (5, false)], ..Synth::new(124.0) };
    let got = each(&phases, |&phase| {
        let s = synth(phase);
        analyse("t", &s.render(), s.rate).track
    });
    for (phase, t) in phases.into_iter().zip(got) {
        let period = 60_000.0 / t.bpm;
        let first_grid = ((synth(phase).first_beat * 1000.0 - t.beat_offset_ms) / period).round() as i64;
        let want = (phase as i64 + first_grid).rem_euclid(4) as i32;
        assert_eq!(t.downbeat_phase, want, "kick on beat {phase}");
        assert!(t.downbeat_confidence >= 0.5, "kick on beat {phase}: confidence {}", t.downbeat_confidence);
    }
}

#[test]
fn non_music_is_measured() {
    let rate = 44100;
    let mut rng = Rng(12345);
    let x: Vec<f32> = (0..rate * 60).map(|_| (0.3 * rng.next()) as f32).collect();
    let t = analyse("n", &x, rate as u32).track;
    println!("noise: bpm {:.1} conf {:.2} stab {:.2} key {} ({:.2}) downbeat conf {:.2}", t.bpm, t.bpm_confidence, t.stability, t.key, t.key_confidence, t.downbeat_confidence);
    assert!(t.bpm_confidence < 0.3, "noise got a confident tempo: {}", t.bpm_confidence);
    assert!(t.key_confidence < 0.2, "noise got a confident key: {}", t.key_confidence);
    let p = plan::plan(Some(&t), Some(&t), 60_000, 60_000, &crate::types::AutoMixSettings::default());
    assert_ne!(p.kind, crate::types::TransitionKind::BeatMatched);

    // Brown noise with slow swells.
    let mut y = 0f64;
    let x: Vec<f32> = (0..rate * 60)
        .map(|i| {
            y = 0.995 * y + 0.05 * rng.next();
            (y * (0.6 + 0.4 * (i as f64 / rate as f64 * 0.37).sin())) as f32
        })
        .collect();
    let t = analyse("n", &x, rate as u32).track;
    assert!(t.bpm_confidence < 0.3, "brown noise: {}", t.bpm_confidence);

    // Beatless tone is measured.
    let x: Vec<f32> = (0..44100 * 5).map(|i| (2.0 * std::f64::consts::PI * 997.0 * i as f64 / 44100.0).sin() as f32).collect();
    let t = analyse("tone", &x, 44100).track;
    assert!((t.lufs + 3.01).abs() < 0.1, "lufs {}", t.lufs);

    // Silence and trims.
    let t = analyse("s", &vec![0f32; 44100 * 20], 44100).track;
    assert_eq!((t.bpm, t.bpm_confidence, t.lufs, t.key), (0.0, 0.0, -70.0, 0));
    assert_eq!((t.silence_start_ms, t.silence_end_ms), (0, 0));
    assert_eq!(t.duration_ms, 20_000);

    let s = Synth { lead_silence: 3.0, tail_silence: 5.0, ..Synth::new(120.0) };
    let t = analyse("t", &s.render(), s.rate).track;
    assert!((t.silence_start_ms - 3200).abs() <= 100, "lead {}", t.silence_start_ms); // first hat at 3.25 s
    assert!((t.silence_end_ms - 63_000).abs() <= 200, "tail {}", t.silence_end_ms);
    assert!(t.mixramp_start_ms >= t.silence_start_ms && t.mixramp_start_ms < t.silence_start_ms + 1000, "{t:?}");
    assert!(t.mixramp_end_ms <= t.silence_end_ms && t.mixramp_end_ms > t.silence_end_ms - 1000, "{t:?}");
    assert!(t.lufs > -30.0 && t.lufs < -5.0, "lufs {}", t.lufs);
    assert!((t.bpm - 120.0).abs() < 0.1);
}

#[test]
fn windows_measure_voice_and_tone() {
    let sung = analyse("t", &Synth { chords: vec![(0, false), (5, false)], ..Synth::new(128.0) }.render(), 44100).track;
    let drums = analyse("t", &Synth::new(128.0).render(), 44100).track;
    assert!(sung.outro_vocal > drums.outro_vocal, "{} vs {}", sung.outro_vocal, drums.outro_vocal);
    assert!(sung.intro_vocal > drums.intro_vocal, "{} vs {}", sung.intro_vocal, drums.intro_vocal);
    assert!((sung.outro_centroid / drums.outro_centroid).log2().abs() > 0.5, "{} vs {}", sung.outro_centroid, drums.outro_centroid);
    for t in [&sung, &drums] {
        assert!((0.0..=1.0).contains(&t.outro_vocal) && (0.0..=1.0).contains(&t.intro_vocal));
        assert!(t.outro_centroid > 0.0 && t.intro_centroid > 0.0);
    }
    let silent = analyse("s", &vec![0f32; 44100 * 20], 44100).track;
    assert_eq!((silent.outro_vocal, silent.intro_vocal, silent.outro_centroid, silent.intro_centroid), (0.0, 0.0, 0.0, 0.0));
}

#[test]
fn input_form_changes_nothing() {
    let got = each(&[22050, 32000, 44100, 48000, 96000], |&rate| {
        let s = Synth { rate, chords: vec![(9, true), (2, true)], ..Synth::new(126.0) };
        let t = analyse("t", &s.render(), rate).track;
        (rate, t.bpm, t.beat_offset_ms, t.key)
    });
    for (rate, bpm, offset, key) in &got {
        assert!((bpm - 126.0).abs() < 0.1, "{rate}: {bpm}");
        assert!((offset - got[2].2).abs() < 6.0, "{rate}: offset {offset} vs {}", got[2].2);
        assert_eq!(*key, got[2].3, "{rate}");
    }

    // Streaming matches whole.
    let s = Synth { chords: vec![(0, false), (5, false)], ..Synth::new(128.0) };
    let x = s.render();
    let whole = analyse("t", &x, s.rate).track;
    let mut a = analysis::Analyzer::new(s.rate, 0);
    let mut i = 0;
    let mut step = 1;
    while i < x.len() {
        let n = step.min(x.len() - i);
        a.feed(&x[i..i + n]);
        i += n;
        step = step * 7 % 4093 + 1;
    }
    let streamed = finish("t", &a.take_features()).track;
    assert_eq!(TrackAnalysis { analysed_ms: 0, ..whole }, TrackAnalysis { analysed_ms: 0, ..streamed });

    // Interleaved stereo 16-bit.
    let pcm: Vec<i16> = x.iter().flat_map(|v| {
        let s = (v * 32767.0) as i16;
        [s, s]
    }).collect();
    a.feed_interleaved(&pcm, 2, |v| v as f32 / 32768.0);
    let t = finish("t", &a.take_features()).track;
    assert!((t.bpm - 128.0).abs() < 0.05);

    // Decoder bytes, 16-bit stereo or float mono, analyse like f32 samples.
    let s = Synth { rate: 48000, chords: vec![(9, true), (4, false)], ..Synth::new(96.0) };
    let x = s.render();
    let reference = analyse("t", &x, s.rate).track;
    let stereo16: Vec<u8> = x.iter().flat_map(|v| {
        let b = ((v * 32767.0).round() as i16).to_le_bytes();
        [b[0], b[1], b[0], b[1]]
    }).collect();
    let t = analyse_bytes("t", &stereo16, 48000, 2, PCM_16).track;
    assert!((t.bpm - reference.bpm).abs() < 0.01 && t.key == reference.key, "{t:?}");
    assert!((t.beat_offset_ms - reference.beat_offset_ms).abs() < 1.0);
    assert_eq!(t.duration_ms, reference.duration_ms);
    let mono: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    let t = analyse_bytes("t", &mono, 48000, 1, PCM_FLOAT).track;
    assert_eq!(TrackAnalysis { analysed_ms: 0, ..t }, TrackAnalysis { analysed_ms: 0, ..reference });
    // Garbage in: no panic.
    let t = analyse_bytes("t", &vec![1, 2, 3], 0, 0, 99).track;
    assert!(t.bpm == 0.0 && t.duration_ms <= 1, "{t:?}");
}

#[test]
fn keys_are_read() {
    let cases = [
        (vec![(0, false), (5, false), (7, false), (0, false)], Some(camelot(0, false))), // C F G C
        (vec![(9, true), (2, true), (4, false), (9, true)], Some(camelot(9, true))),     // Am Dm E Am
        (vec![(7, false), (0, false), (2, false), (7, false)], Some(camelot(7, false))), // G C D G
        (vec![(2, true), (7, true), (9, false), (2, true)], Some(camelot(2, true))),     // Dm Gm A Dm
        (vec![], None),                                                                  // drums only
    ];
    let got = each(&cases, |(chords, _)| {
        let s = Synth { chords: chords.clone(), ..Synth::new(110.0) };
        analyse("t", &s.render(), s.rate).track
    });
    for ((_, want), t) in cases.iter().zip(got) {
        match want {
            Some(want) => {
                println!("key want {} got {} ({:.2})", structure::camelot_name(*want), structure::camelot_name(t.key), t.key_confidence);
                assert_eq!(t.key, *want, "want {} got {}", structure::camelot_name(*want), structure::camelot_name(t.key));
                assert!(t.key_confidence >= 0.3, "{}: confidence {}", structure::camelot_name(*want), t.key_confidence);
            }
            None => assert!(t.key_confidence < 0.3, "drums got key confidence {}", t.key_confidence),
        }
    }

    // A band tuned ±40 cents keeps its key: the tuning is measured and removed first.
    for (cents, tonic, minor) in [(40.0, 7, true), (-40.0, 2, false), (0.0, 9, true)] {
        let song = Song { cents, progression: 1, sections: vec![(4, KEYS), (16, FULL)], ..Song::new("detuned", Style::Backbeat, 120.0, tonic, minor) };
        let (x, truth) = song.render();
        let mut an = analysis::Analyzer::new(song.rate, 0);
        an.feed(&x);
        let f = an.take_features();
        let tune = structure::tuning(&f) * 100.0;
        let t = finish("t", &f).track;
        println!("{cents:+} cents: measured {tune:+.1}, key {} (want {})", structure::camelot_name(t.key), structure::camelot_name(truth.key));
        assert!((tune - cents).abs() < 8.0, "{cents}: tuning {tune}");
        assert_eq!(t.key, truth.key, "{cents}: {}", structure::camelot_name(t.key));
        assert!(t.key_confidence >= 0.4, "{cents}: confidence {}", t.key_confidence);
    }
}

#[test]
fn drifting_tempo_is_unstable() {
    let s = Synth { secs: 120.0, end_bpm: 132.0, ..Synth::new(120.0) };
    let t = analyse("t", &s.render(), s.rate).track;
    println!("drift: bpm {:.2} conf {:.2} stability {:.2}", t.bpm, t.bpm_confidence, t.stability);
    assert!(t.stability < 0.6, "stability {}", t.stability);
}

#[test]
fn sections_are_found() {
    // 16 bars of hats, the full groove, 16 bars of hats.
    let s = Synth { secs: 150.0, intro_bars: 16, outro_bars: 16, chords: vec![(0, false), (5, false)], ..Synth::new(128.0) };
    let t = analyse("t", &s.render(), s.rate).track;
    let bar = 4.0 * 60_000.0 / 128.0;
    let intro_want = (s.first_beat * 1000.0) + 16.0 * bar;
    let bars = s.beats().len() / 4;
    let outro_want = (s.first_beat * 1000.0) + (bars - 16) as f64 * bar;
    println!("intro {} (want {intro_want:.0}), outro {} (want {outro_want:.0})", t.intro_end_ms, t.outro_start_ms);
    assert!((t.intro_end_ms as f64 - intro_want).abs() < 60.0, "intro {}", t.intro_end_ms);
    assert!((t.outro_start_ms as f64 - outro_want).abs() < 60.0, "outro {}", t.outro_start_ms);

    // A steady track: no intro; the outro cue on an 8-bar line at least 16 bars before the end.
    let s = Synth { secs: 120.0, ..Synth::new(128.0) };
    let t = analyse("t", &s.render(), s.rate).track;
    assert!(t.intro_end_ms <= t.silence_start_ms + 1, "{t:?}");
    let from_first = (t.outro_start_ms as f64 - s.first_beat * 1000.0) / bar;
    assert!((from_first / 8.0 - (from_first / 8.0).round()).abs() < 0.02, "on an 8-bar line: {from_first}");
    assert!(t.silence_end_ms as f64 - t.outro_start_ms as f64 >= 16.0 * bar - 100.0);

    // House with 16-bar drum intro and outro, marked by tonal energy, not level. A beatless pad intro ends where the
    // beat starts.
    let song = Song { sections: vec![(16, DRUMS), (32, FULL), (16, DRUMS)], ..Song::new("house", Style::House, 124.0, 9, true) };
    let (x, truth) = song.render();
    let t = analyse("t", &x, song.rate).track;
    let beat = 60.0 / 124.0;
    let (intro, outro) = (t.intro_end_ms as f64 / 1000.0, t.outro_start_ms as f64 / 1000.0);
    println!("intro {intro:.2} (want {:.2}), outro {outro:.2} (want {:.2})", truth.intro_end, truth.outro_start.unwrap());
    assert!((intro - truth.intro_end).abs() <= beat, "intro {intro}");
    assert!((outro - truth.outro_start.unwrap()).abs() <= beat, "outro {outro}");

    let song = Song { ambient_s: 30.0, sections: vec![(32, FULL)], ..Song::new("pad", Style::House, 122.0, 3, false) };
    let (x, truth) = song.render();
    let t = analyse("t", &x, song.rate).track;
    let intro = t.intro_end_ms as f64 / 1000.0;
    assert!((intro - truth.intro_end).abs() <= 60.0 / 122.0, "intro {intro}, the beat starts at {}", truth.intro_end);
}

#[test]
fn syncopation_reads_the_beat() {
    // Grooves whose strongest lag is not the beat (drum and bass, dotted-eighth funk): autocorrelation alone read
    // 116 and 139 BPM.
    for (song, want) in [
        (Song { sections: vec![(28, FULL)], ..Song::new("dnb", Style::DnB, 174.0, 0, true) }, 174.0),
        (Song { sections: vec![(20, FULL)], ..Song::new("funk", Style::Funk, 104.0, 10, false) }, 104.0),
    ] {
        let (x, _) = song.render();
        let t = analyse("t", &x, song.rate).track;
        println!("{}: {:.2} BPM (conf {:.2})", song.name, t.bpm, t.bpm_confidence);
        assert!(octave_ok(t.bpm, want, 0.01), "{}: {}", song.name, t.bpm);
    }

    // A steady syncopated groove is trusted at both ends. Regression: its five-sixteenths lag counted as a rival
    // tempo and dropped confidence to 0.44.
    use super::plan::{MIN_BPM_CONFIDENCE, MIN_STABILITY};
    for (bpm, swing) in [(125.0, 0.66), (128.0, 0.5), (130.0, 0.58)] {
        let song = Song { swing, sections: vec![(8, DRUMS), (24, FULL), (8, FULL)], ..Song::new("broken", Style::Broken, bpm, 4, false) };
        let (x, _) = song.render();
        let t = analyse("t", &x, song.rate).track;
        for (end, got, conf, stab) in [
            ("whole", t.bpm, t.bpm_confidence, t.stability),
            ("intro", t.intro_bpm, t.intro_bpm_confidence, t.intro_stability),
            ("outro", t.outro_bpm, t.outro_bpm_confidence, t.outro_stability),
        ] {
            println!("{bpm} swing {swing} {end}: {got:.2} BPM (conf {conf:.2}, stab {stab:.2})");
            assert!(octave_ok(got, bpm, 0.01), "{bpm} {end}: {got}");
            assert!(conf >= MIN_BPM_CONFIDENCE && stab >= MIN_STABILITY, "{bpm} {end}: conf {conf} stab {stab}");
        }
    }
}

#[test]
fn waltz_has_three_beats() {
    let song = Song { sections: vec![(24, FULL)], ..Song::new("waltz", Style::Waltz, 150.0, 5, false) };
    let (x, truth) = song.render();
    let t = analyse("t", &x, song.rate).track;
    assert_eq!(t.beats_per_bar, 3, "{t:?}");
    let (_, downbeats) = grid(&t, truth.beats[4], truth.music.1 - 1.0);
    assert!(precision(&truth.downbeats, &downbeats) >= 0.9, "downbeats {downbeats:?}");
    let four = analyse("t", &Synth { chords: vec![(0, false), (5, false)], ..Synth::new(126.0) }.render(), 44100).track;
    assert_eq!(four.beats_per_bar, 4);
}

/// Half-time: bars start on the kick, not on the snare's beat three.
#[test]
fn half_time_bars_start_on_the_kick() {
    let song = Song { sections: vec![(24, FULL)], ..Song::new("halftime", Style::HalfTime, 140.0, 5, true) };
    let (x, truth) = song.render();
    let t = analyse("t", &x, song.rate).track;
    let (_, downbeats) = grid(&t, truth.beats[4], truth.music.1 - 1.0);
    println!("{:.2} BPM, downbeats {:?}", t.bpm, &downbeats[..4.min(downbeats.len())]);
    assert!(precision(&truth.downbeats, &downbeats) >= 0.9, "{:.2} BPM phase {}", t.bpm, t.downbeat_phase);
}

#[test]
fn plan_from_real_analyses() {
    let a = analyse("a", &Synth { secs: 120.0, ..Synth::new(128.0) }.render(), 44100).track;
    let b = analyse("b", &Synth { secs: 120.0, ..Synth::new(125.0) }.render(), 44100).track;
    let p = plan::plan(Some(&a), Some(&b), a.duration_ms, b.duration_ms, &crate::types::AutoMixSettings::default());
    assert_eq!(p.kind, crate::types::TransitionKind::BeatMatched, "{}", p.reason);
    assert!((p.tempo_ratio - a.bpm / b.bpm).abs() < 1e-6);
}

