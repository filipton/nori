//! Speed, pitch and silence skipping through the output clock.

use nori_player::sim::Player;

use crate::common::*;

/// The player's position `ms` of the virtual clock apart, after `settle_ms` for the change to reach
/// the ear through what the output already holds.
fn pace(p: &mut Player, settle_ms: i64, ms: i64) -> i64 {
    p.run_for(settle_ms);
    let a = p.position_ms();
    p.run_for(ms);
    p.position_ms() - a
}

#[test]
fn speed_and_pitch() {
    let song = centred(440.0, 0.4, 60.0);
    let mut p = Player::new(vec![track("a", &song)]);
    p.play_from(0);
    p.run_for(2_000);
    let heard_before = p.sink.heard_frames as usize;
    p.set_speed(1.5, 1.0);
    let moved = pace(&mut p, 12_000, 6_000);
    assert!((8_900..=9_100).contains(&moved), "6 s of the clock moved the song {moved} ms");
    assert!(p.run_to_end(60_000));
    assert!(p.sink.gaps.is_empty(), "no hole when the speed changed: {:?}", p.sink.gaps);
    // Everything after the change played in two thirds of its time, give or take the stretcher's edges.
    let rest = p.sink.heard_frames as usize - heard_before;
    let want = (frames(60.0) - heard_before) as f64;
    assert!((rest as f64 - want).abs() < want / 3.0 + frames(0.5) as f64, "{rest} frames for {want}");
    let heard = left(&p.sink.heard_samples());
    let hz = pitch_hz(&heard[heard.len() - frames(6.0)..heard.len() - frames(1.0)], RATE as f64);
    assert!((hz - 440.0).abs() < 2.0, "the pitch stays: {hz:.1} Hz");

    // Pitch keeps pace.
    let song = centred(440.0, 0.4, 40.0);
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_speed(1.0, 1.1);
    p.play_from(0);
    let moved = pace(&mut p, 2_000, 6_000);
    assert!((5_950..=6_050).contains(&moved), "6 s of the clock moved the song {moved} ms");
    assert!(p.run_to_end(60_000));
    assert!((p.sink.heard_frames as i64 - frames(40.0) as i64).abs() < frames(0.1) as i64, "as long as the song: {}", p.sink.heard_frames);
    let heard = left(&p.sink.heard_samples());
    let hz = pitch_hz(&heard[frames(5.0)..frames(30.0)], RATE as f64);
    assert!((hz - 484.0).abs() < 2.0, "a tenth higher: {hz:.1} Hz");
}

#[test]
fn silence_skipping() {
    // Three seconds of music, four of silence, three of music.
    let (a, b) = (music(3.0, 31), music(3.0, 32));
    let song: Vec<i16> = [a.clone(), vec![0; frames(4.0) * 2], b.clone()].concat();
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_skip_silence(true);
    p.play_from(0);
    assert!(p.run_to_end(30_000));
    let heard = p.sink.heard_samples();
    let secs = heard.len() as f64 / 2.0 / RATE as f64;
    // The pause is cut to a fifth of itself: 0.8 s instead of 4.
    assert!((6.7..7.0).contains(&secs), "{secs:.2} s heard of 10");
    assert!(heard[..a.len()] == a[..], "the music before the pause is untouched");
    // After it, from its first note above the silence threshold: the quiet samples before that are the
    // end of the pause, and fade in with it.
    let loud = b.iter().position(|v| v.unsigned_abs() > 1024).unwrap() / 2 * 2;
    assert!(heard[heard.len() - (b.len() - loud)..] == b[loud..], "and so is the music after it");
    assert!(p.sink.gaps.is_empty());
    // What was skipped still counts as played: the player ends at the end of the song.
    assert!((p.position_ms() - 10_000).abs() <= 25, "the position is the song's: {}", p.position_ms());

    // Silence skipping keeps short rest.
    let song: Vec<i16> = [music(2.0, 33), vec![0; frames(0.05) * 2], music(2.0, 34)].concat();
    let mut p = Player::new(vec![track("a", &song)]);
    p.set_skip_silence(true);
    p.play_from(0);
    assert!(p.run_to_end(30_000));
    assert!(p.sink.heard_samples() == song, "50 ms of quiet is music");
}

