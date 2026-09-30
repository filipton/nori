//! The sound chain end to end: limiter, equalizer, live setting changes.

use nori_player::automix::synth::Rng;
use nori_player::compressor::CompressorPreset;
use nori_player::dsp::{Band, Effects, Equalizer, HIGH_SHELF, LOW_SHELF, PEAKING};
use nori_player::sim::{Player, Sound};

use crate::common::*;

fn band(kind: i32, freq: f64, gain_db: f64, q: f64) -> Band {
    Band { kind, freq, gain_db, q, channel: 0 }
}

/// A curve that does something audible everywhere: bass cut, a presence bump, air.
fn curve() -> Vec<Band> {
    vec![band(LOW_SHELF, 200.0, -6.0, 0.7), band(PEAKING, 1000.0, 6.0, 1.0), band(HIGH_SHELF, 5000.0, 4.0, 0.7)]
}

fn limiter() -> Sound {
    Sound { limiter: true, ..Sound::default() }
}

/// The limiter's look-ahead at [`RATE`], frames: everything through it comes out this much later.
const LOOKAHEAD: usize = 220;

#[test]
fn limiter_only_catches_peaks() {
    // Mastered music peaks just under full scale; at the default -1 dB it needs a few dB at most.
    let song: Vec<i16> = music(20.0, 5).iter().map(|&v| (v as f64 * 2.4).clamp(-32768.0, 32767.0) as i16).collect();
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_sound(limiter());
    p.play_from(0);
    // The meter a screen reads shows the limiter at work while it is.
    p.run_for(5_000);
    assert!(p.sink.chain_in() && p.sink.meter_db() > 0.0, "the meter reads {} dB", p.sink.meter_db());
    assert!(p.run_to_end(40_000));
    let reduction = p.sink.gain_reduction_db;
    assert!(reduction > 0.0 && reduction < 6.0, "the limiter took {reduction} dB off mastered music");
    let peak = p.sink.heard_samples().iter().map(|&v| (v as f64 / 32768.0).abs()).fold(0.0, f64::max);
    assert!(peak <= 10f64.powf(-1.0 / 20.0) + 1.0 / 32768.0, "nothing past the -1 dB ceiling: {peak}");
}

#[test]
fn compressor_meter() {
    let song = music(20.0, 5);
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_sound(Sound { effects: Effects { compressor: Some(CompressorPreset::Strong.settings()), ..Effects::default() }, ..limiter() });
    p.play_from(0);
    p.run_for(5_000);
    let db = p.sink.compression_db();
    assert!(db > 1.0 && db < 30.0, "a screen reads {db} dB off music through the strong preset");
    // Without one: nothing to read, and the limiter's meter is its own.
    let mut q = Player::new(vec![track("a", &song)]);
    q.set_sound(limiter());
    q.play_from(0);
    q.run_for(5_000);
    assert!(q.sink.chain_in());
    assert_eq!(q.sink.compression_db(), 0.0);
}

#[test]
fn limiter_bit_exact_below_threshold() {
    let song = music(10.0, 6);
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_sound(limiter());
    p.play_from(0);
    assert!(p.run_to_end(30_000));
    let heard = p.sink.heard_samples();
    assert!(heard[..LOOKAHEAD * 2].iter().all(|&v| v == 0), "the look-ahead delays the song");
    assert!(heard[LOOKAHEAD * 2..] == song[..heard.len() - LOOKAHEAD * 2], "and changes nothing else, bit for bit");
    assert_eq!(p.sink.gain_reduction_db, 0.0);
}

#[test]
fn limiter_ceiling_holds_at_any_setting() {
    // Hot random input, up to 24 dB over full scale, through every threshold, release and look-ahead.
    for threshold in [0.0, -1.0, -3.0, -6.0, -12.0] {
        for release in [5.0, 50.0, 120.0, 1000.0] {
            for lookahead in [0.5, 1.0, 5.0, 20.0] {
                for preamp in [0.0, 6.0, 12.0, 24.0] {
                    let mut eq = Equalizer::new(48_000, 2);
                    eq.configure(&[], preamp, 0.0);
                    eq.configure_output(0.0, false, threshold, release, lookahead);
                    let mut r = Rng(42);
                    let x: Vec<f32> = (0..12_000).map(|_| r.next() as f32).collect();
                    let mut y = vec![0f32; x.len()];
                    eq.process_f32(&x, &mut y);
                    let peak = y.iter().fold(0f64, |m, v| m.max(v.abs() as f64));
                    let ceiling = 10f64.powf(threshold / 20.0);
                    assert!(
                        peak <= ceiling * (1.0 + 1e-6),
                        "threshold {threshold} dB, release {release} ms, look-ahead {lookahead} ms, +{preamp} dB: peak {peak} is {:.3} dB over",
                        db(peak / ceiling)
                    );
                }
            }
        }
    }
}

#[test]
fn equalizer_gains() {
    // One tone for each band, where the band has its whole effect, and one where none of them does.
    let tones = [(30.0, -6.0), (1000.0, 6.0), (16000.0, 4.0), (380.0, 0.35)];
    let x: Vec<i16> = (0..frames(6.0))
        .flat_map(|i| {
            let t = i as f64 / RATE as f64;
            let v = tones.iter().map(|(hz, _)| 0.08 * (std::f64::consts::TAU * hz * t).sin()).sum::<f64>();
            [(v * 32767.0).round() as i16; 2]
        })
        .collect();
    let mut p = Player::new(vec![track("a", &x)]);
    p.set_sound(Sound { bands: curve(), ..Sound::default() });
    p.play_from(0);
    assert!(p.run_to_end(20_000));
    let (heard, input) = (left(&p.sink.heard_samples()), left(&x));
    let (from, to) = (frames(1.0), frames(5.0));
    for (hz, want) in tones {
        let got = db(level_at(&heard[from..to], hz, RATE as f64) / level_at(&input[from..to], hz, RATE as f64));
        assert!((got - want).abs() < 0.3, "{hz} Hz: {got:.2} dB, the curve says {want}");
    }
}

#[test]
fn graphic_equalizer_gains() {
    // Tones at four band centres of the ten-band layout, far enough apart to be read one by one.
    let sliders = vec![0.0, 0.0, 6.0, 6.0, 0.0, -6.0, 0.0, 0.0, 3.0, 0.0];
    let centres = nori_player::graphic::centres(10);
    let tones: Vec<(f64, f64)> = [2, 5, 8].iter().map(|&i| (centres[i], sliders[i])).collect();
    let x: Vec<i16> = (0..frames(6.0))
        .flat_map(|i| {
            let t = i as f64 / RATE as f64;
            let v = tones.iter().map(|(hz, _)| 0.08 * (std::f64::consts::TAU * hz * t).sin()).sum::<f64>();
            [(v * 32767.0).round() as i16; 2]
        })
        .collect();
    let mut p = Player::new(vec![track("a", &x)]);
    p.set_sound(Sound { graphic: sliders, ..Sound::default() });
    p.play_from(0);
    assert!(p.run_to_end(20_000));
    let (heard, input) = (left(&p.sink.heard_samples()), left(&x));
    let (from, to) = (frames(1.0), frames(5.0));
    for (hz, want) in tones {
        let got = db(level_at(&heard[from..to], hz, RATE as f64) / level_at(&input[from..to], hz, RATE as f64));
        assert!((got - want).abs() < 0.4, "{hz} Hz: {got:.2} dB, the slider says {want}");
    }
}

#[test]
fn volume_boost_under_ceiling() {
    // Music boosted 6 dB with the limiter switch left off: the boost
    // brings it anyway.
    let song = music(12.0, 5);
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_sound(Sound { effects: Effects { boost_db: 6.0, ..Effects::default() }, ..Sound::default() });
    p.play_from(0);
    assert!(p.run_to_end(40_000));
    let heard: Vec<f64> = p.sink.heard_samples().iter().map(|&v| v as f64 / 32768.0).collect();
    let input: Vec<f64> = song.iter().map(|&v| v as f64 / 32768.0).collect();
    let peak = heard.iter().fold(0f64, |m, v| m.max(v.abs()));
    assert!(peak <= 10f64.powf(-1.0 / 20.0) + 1.0 / 32768.0, "nothing past the -1 dB ceiling: {peak}");
    let louder = db(rms(&heard) / rms(&input));
    assert!(louder > 3.0 && louder < 6.2, "{louder} dB louder");
}

#[test]
fn compressor_narrows_dynamics() {
    // A quiet half and a loud half, 24 dB apart.
    let quiet = sine(220.0, 0.03, 4.0);
    let loud = sine(220.0, 0.5, 4.0);
    let song: Vec<i16> = quiet.iter().chain(&loud).copied().collect();
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_sound(Sound { effects: Effects { compressor: Some(CompressorPreset::Balanced.settings()), ..Effects::default() }, ..Sound::default() });
    p.play_from(0);
    assert!(p.run_to_end(30_000));
    let heard = left(&p.sink.heard_samples());
    let half = heard.len() / 2;
    let (a, b) = (rms(&heard[frames(1.0)..half - frames(0.5)]), rms(&heard[half + frames(1.0)..heard.len() - frames(0.5)]));
    let apart = db(b / a);
    assert!(apart < 24.0 - 6.0, "24 dB apart went in, {apart:.1} came out");
}

/// Plays a 220 Hz tone, changes the sound to `change` for a while and back to `base`, and returns the
/// largest step heard against what a sine at the level heard around it steps.
fn step_through(base: Sound, change: Sound, tone: &[i16]) -> f64 {
    let mut p = Player::new(vec![track("a", tone)]);
    p.set_sound(base.clone());
    p.play_from(0);
    p.run_for(1_000);
    p.set_sound(change);
    p.run_for(1_500);
    p.set_sound(base);
    assert!(p.run_to_end(10_000));
    assert!(p.sink.gaps.is_empty(), "the audio kept flowing");
    let heard = p.sink.heard_samples();
    let turn = std::f64::consts::TAU * 220.0 / RATE as f64;
    let around = frames(0.01);
    let mut worst = 0.0f64;
    for c in 0..2 {
        let x: Vec<f64> = heard.iter().skip(c).step_by(2).map(|&v| v as f64 / 32768.0).collect();
        for k in frames(0.5)..x.len() - frames(0.5) {
            let peak = x[k - around..k + around].iter().fold(0.0f64, |m, v| m.max(v.abs()));
            worst = worst.max((x[k] - x[k - 1]).abs() / (peak * turn).max(1e-4));
        }
    }
    worst
}

#[test]
fn live_settings_changes_do_not_click() {
    let tone = sine(220.0, 0.3, 4.0);
    let eq = Sound { bands: curve(), ..Sound::default() };
    for (what, base, change) in [
        ("the equalizer", limiter(), Sound { bands: curve(), ..limiter() }),
        ("mono", limiter(), Sound { mono: true, ..limiter() }),
        ("crossfeed", limiter(), Sound { crossfeed_db: 4.5, ..limiter() }),
        ("the crossfeed's cutoff", Sound { crossfeed_db: 9.5, crossfeed_hz: 650.0, ..limiter() }, Sound { crossfeed_db: 9.5, crossfeed_hz: 1500.0, ..limiter() }),
        ("balance", eq.clone(), Sound { balance: 0.4, ..eq.clone() }),
        ("the limiter", eq.clone(), Sound { limiter: true, ..eq.clone() }),
        ("the graphic equalizer", limiter(), Sound { graphic: vec![3.0, 6.0, 4.0, 0.0, -3.0, -3.0, 0.0, 2.0, 4.0, 4.0], ..limiter() }),
        ("the bass boost", limiter(), Sound { effects: Effects { bass_boost_db: 8.0, ..Effects::default() }, ..limiter() }),
        ("the expander", limiter(), Sound { effects: Effects { expander: Some(nori_player::compressor::ExpanderSettings { threshold_db: -20.0, ratio: 4.0, ..Default::default() }), ..Effects::default() }, ..limiter() }),
        ("loudness compensation", limiter(), Sound { effects: Effects { loudness: Some(nori_player::contour::Loudness { reference_phon: 80.0, volume_db: -30.0 }), ..Effects::default() }, ..limiter() }),
        ("the volume under loudness compensation", Sound { effects: Effects { loudness: Some(nori_player::contour::Loudness { reference_phon: 80.0, volume_db: -20.0 }), ..Effects::default() }, ..limiter() }, Sound { effects: Effects { loudness: Some(nori_player::contour::Loudness { reference_phon: 80.0, volume_db: -35.0 }), ..Effects::default() }, ..limiter() }),
        ("the compressor", limiter(), Sound { effects: Effects { compressor: Some(CompressorPreset::Strong.settings()), ..Effects::default() }, ..limiter() }),
        ("the virtualizer", limiter(), Sound { effects: Effects { virtualizer: 1.0, ..Effects::default() }, ..limiter() }),
        ("the volume boost", limiter(), Sound { effects: Effects { boost_db: 6.0, ..Effects::default() }, ..limiter() }),
    ] {
        let worst = step_through(base, change, &tone);
        assert!(worst <= 1.5, "{what} on and off: a step {worst:.2} times a sine's at that level");
    }
}
