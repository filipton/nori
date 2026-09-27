//! The sound chain on the way to the ear: the limiter's ceiling and its transparency, the equalizer's
//! response, settings changed while music plays, and the rebuilds that wait for a song boundary.

use nori_player::automix::synth::Rng;
use nori_player::burst::BUFFER_US;
use nori_player::compressor::CompressorPreset;
use nori_player::dsp::{Band, Effects, Equalizer, HIGH_SHELF, LOW_SHELF, PEAKING};
use nori_player::sim::{Player, Sound, SHALLOW_US};

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
fn the_limiter_only_catches_peaks() {
    // Mastered music peaks just under full scale; at the default -1 dB it needs a few dB at most.
    let song: Vec<i16> = music(20.0, 5).iter().map(|&v| (v as f64 * 2.4).clamp(-32768.0, 32767.0) as i16).collect();
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_sound(limiter());
    p.play_from(0);
    // The meter a screen reads shows the limiter at work while it is.
    p.run_for(5_000);
    assert!(p.sink.chain_in() && p.sink.meter_db > 0.0, "the meter reads {} dB", p.sink.meter_db);
    assert!(p.run_to_end(40_000));
    let reduction = p.sink.gain_reduction_db;
    assert!(reduction > 0.0 && reduction < 6.0, "the limiter took {reduction} dB off mastered music");
    let peak = p.sink.heard_samples().iter().map(|&v| (v as f64 / 32768.0).abs()).fold(0.0, f64::max);
    assert!(peak <= 10f64.powf(-1.0 / 20.0) + 1.0 / 32768.0, "nothing past the -1 dB ceiling: {peak}");
}

#[test]
fn the_compressor_meter_reads_what_it_takes_off() {
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
fn below_its_threshold_the_limiter_changes_no_sample() {
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
fn nothing_passes_the_limiter_ceiling_at_any_setting() {
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
fn the_equalizer_answers_with_the_gains_it_was_given() {
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
fn the_graphic_equalizer_answers_with_its_sliders() {
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
fn a_volume_boost_is_louder_and_never_past_the_ceiling() {
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
fn the_compressor_brings_quiet_and_loud_closer() {
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

/// Plays a steady tone, changes the sound to `change` for a while and back to `base`, and returns the
/// largest step heard against the largest step of the tone itself.
fn step_through(base: Sound, change: Sound, tone: &[i16]) -> (f64, f64) {
    let mut p = Player::new(vec![track("a", tone)]);
    p.set_sound(base.clone());
    // As the equalizer screen plays it, where settings are moved while listening: a shallow buffer
    // topped up as it goes, so each change goes through the chain within the moment.
    p.sink.capacity_us = SHALLOW_US;
    p.burst.enabled = false;
    p.play_from(0);
    p.run_for(1_000);
    p.set_sound(change);
    p.run_for(1_500);
    p.set_sound(base);
    assert!(p.run_to_end(10_000));
    assert!(p.sink.gaps.is_empty(), "the audio kept flowing");
    let heard = p.sink.heard_samples();
    let side = |c: usize| -> Vec<f64> { heard.iter().skip(c).step_by(2).map(|&v| v as f64 / 32768.0).collect() };
    let own = |c: usize| -> Vec<f64> { tone.iter().skip(c).step_by(2).map(|&v| v as f64 / 32768.0).collect() };
    let edge = frames(0.5);
    let worst = (0..2).map(|c| max_step(&side(c)[edge..heard.len() / 2 - edge])).fold(0.0, f64::max);
    let steady = (0..2).map(|c| max_step(&own(c))).fold(0.0, f64::max);
    (worst, steady)
}

#[test]
fn settings_changed_while_playing_never_click() {
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
        let (worst, steady) = step_through(base, change, &tone);
        assert!(worst <= 2.0 * steady, "{what} on and off: a step of {worst:.4} against the tone's own {steady:.4}");
    }
}

#[test]
fn taking_the_equalizer_out_waits_for_the_boundary() {
    let (a, b) = (music(20.0, 8), music(20.0, 9));
    let mut p = Player::new(vec![track("a", &a), track("b", &b)]);
    p.set_sound(Sound { bands: curve(), ..Sound::default() });
    p.play_from(0);
    p.run_for(3_000);
    // Off: the processor leaves the chain, which rebuilds the output - not mid-song.
    p.set_sound(Sound::default());
    assert!(p.app.logged("chain swap deferred to the next track"), "{:?}", p.app.log);
    assert!(p.sink.dsp, "the processor stays until the boundary");
    assert!(p.run_until(30_000, |p| p.current() == Some(1)));
    assert!(p.app.logged("chain swap at the boundary"), "{:?}", p.app.log);
    assert!(!p.sink.dsp, "and leaves there");
    let before = p.sink.heard_frames;
    p.run_for(3_000);
    assert!(p.sink.heard_frames - before >= frames(2.9) as u64, "still playing after the swap");
    assert!(p.sink.gaps.is_empty(), "{:?}", p.sink.gaps);
}

#[test]
fn tuning_borrows_the_shallow_buffer_and_gives_it_back_at_the_next_boundary() {
    let songs: Vec<Vec<i16>> = (0..3).map(|k| music(15.0, 20 + k)).collect();
    let mut p = Player::new(songs.iter().enumerate().map(|(k, s)| track(&format!("s{k}"), s)).collect());
    p.set_sound(Sound { bands: curve(), ..Sound::default() });
    p.play_from(0);
    p.run_for(2_000);
    let deep = p.sink.buffer_bytes();
    assert_eq!(p.sink.capacity_us, BUFFER_US);
    p.set_tuning(true);
    assert!(!p.burst.enabled, "no bursts while tuning, at once");
    assert_eq!(p.sink.capacity_us, BUFFER_US, "the buffer waits for the boundary");
    assert!(p.run_until(30_000, |p| p.current() == Some(1)));
    assert_eq!(p.sink.capacity_us, SHALLOW_US, "tuning takes the shallow buffer at the boundary");
    let shallow = p.sink.buffer_bytes();
    assert!(shallow > 0 && shallow < deep);
    p.run_for(2_000);
    p.set_tuning(false);
    assert_eq!(p.sink.capacity_us, SHALLOW_US);
    assert!(p.run_until(30_000, |p| p.current() == Some(2)));
    assert_eq!(p.sink.capacity_us, BUFFER_US, "the deep buffer is back after the next boundary");
    assert!(p.burst.enabled);
    let before = p.sink.heard_frames;
    p.run_for(3_000);
    assert!(p.sink.heard_frames - before >= frames(2.9) as u64, "still playing after the deep swap");
    assert_eq!(p.app.log.iter().filter(|l| l.contains("chain swap at the boundary")).count(), 2, "{:?}", p.app.log);
}

#[test]
fn a_swap_waiting_for_a_boundary_is_made_at_a_jump_and_leaves_the_next_mix_whole() {
    // The limiter leaves the chain while a song plays: the rebuild waits. Then the same song is played
    // again from near its end, with a crossfade into the next: the jump empties the output anyway, so
    // the rebuild is made there - not at the next song's start, in the middle of the mix it would cut.
    let (a, b) = (music(40.0, 51), music(40.0, 52));
    let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b)], crossfade(4));
    p.set_sound(limiter());
    p.play_from(0);
    p.run_for(3_000);
    p.set_sound(Sound::default());
    assert!(p.app.logged("chain swap deferred"), "{:?}", p.app.log);
    p.jump(0, 30_000);
    assert!(p.app.logged("chain swap at the boundary"), "made at the jump: {:?}", p.app.log);
    assert!(p.run_until(12_000, |p| p.mixing()), "{:?}", p.app.log);
    let mut heard_mixing = 0;
    while p.mixing() {
        p.run_for(100);
        heard_mixing += 100;
    }
    assert!(heard_mixing >= 3_800, "the whole four-second mix is heard: {heard_mixing} ms");
    assert_eq!(p.app.log.iter().filter(|l| l.contains("chain swap at the boundary")).count(), 1, "{:?}", p.app.log);
    assert!(p.sink.gaps.is_empty(), "{:?}", p.sink.gaps);
}
