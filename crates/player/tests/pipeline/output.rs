//! Output feeding: bursts, and one format per queue with conversion or reopening.

use std::f64::consts::TAU;

use nori_player::burst::{BUFFER_US, LOW_US};
use nori_player::sim::{Audio, Player, Track, STEP_MS};

use crate::common::*;

#[test]
fn output_fed_in_bursts() {
    let mut p = Player::new(vec![track("a", &music(60.0, 40))]);
    p.play_from(0);
    p.run_for(3_000);
    let mut levels = Vec::new();
    for _ in 0..500 {
        p.run_for(100);
        levels.push(p.sink.queued_us());
    }
    let (lo, hi) = levels.iter().fold((i64::MAX, 0), |(l, h), &q| (l.min(q), h.max(q)));
    assert!(lo >= LOW_US - STEP_MS * 1000 && hi <= BUFFER_US, "the buffer stays between {LOW_US} and {BUFFER_US}: {lo}..{hi}");
    let refills = levels.windows(2).filter(|w| w[1] > w[0]).count();
    // Fifty seconds of playing, eight seconds drained between refills: six or seven wake-ups, not thousands.
    assert!((6..=7).contains(&refills), "{refills} refills");
    assert!(p.run_to_end(20_000));
    assert_eq!(p.burst.bytes_written as usize, frames(60.0) * 4, "every byte went down once");
}

/// A tone at `rate`, both sides the same.
fn tone_at(rate: u32, hz: f64, secs: f64) -> Vec<i16> {
    (0..(secs * rate as f64) as usize).flat_map(|i| [((0.4 * (TAU * hz * i as f64 / rate as f64).sin()) * 32767.0).round() as i16; 2]).collect()
}

#[test]
fn gapless_rate_change_reopens_output() {
    let a = Track::new("a", Audio::pcm(RATE, 2, &tone_at(RATE, 440.0, 5.0)));
    let b_pcm = tone_at(48_000, 440.0, 5.0);
    let b = Track::new("b", Audio::pcm(48_000, 2, &b_pcm));
    let mut p = Player::new(vec![a, b]);
    p.play_from(0);
    assert!(p.run_to_end(20_000));
    // Once a has played out, the output opens again at 48 kHz: b is heard as it is, not resampled.
    assert_eq!((p.sink.configs.len(), p.sink.rebuilds), (2, 1), "opened again for b");
    assert_eq!(p.sink.format.map(|f| f.rate), Some(48_000));
    assert!(!p.app.logged("converting"), "{:?}", p.app.log);
    assert!(p.app.logged("sink follows 48000 Hz x2"), "{:?}", p.app.log);
    let heard = p.sink.heard_samples();
    let tail = &heard[heard.len() - b_pcm.len()..];
    assert!(tail == &b_pcm[..], "b sample for sample");
    assert_eq!(p.sink.timestamp_jumps, 0);
}

#[test]
fn crossfade_across_rates() {
    let a = Track::new("a", Audio::pcm(RATE, 2, &tone_at(RATE, 440.0, 30.0)));
    let b = Track::new("b", Audio::pcm(48_000, 2, &tone_at(48_000, 660.0, 30.0)));
    let mut p = Player::with_prefs(vec![a, b], crossfade(6));
    p.play_from(0);
    assert!(p.run_to_end(80_000));
    assert!(p.app.logged("mixing: the next track arrived"), "{:?}", p.app.log);
    assert_eq!(p.sink.rebuilds, 0);
    // What the converter holds back to see ahead (under 2 ms) is still in it when the queue ends.
    assert!((p.sink.heard_frames as i64 - frames(54.0) as i64).abs() <= 80, "{} frames", p.sink.heard_frames);
    let heard = left(&p.sink.heard_samples());
    let (tail_a, mid, b_alone) = (&heard[frames(20.0)..frames(23.0)], &heard[frames(26.5)..frames(27.5)], &heard[frames(32.0)..frames(35.0)]);
    assert!((pitch_hz(tail_a, RATE as f64) - 440.0).abs() < 0.5);
    assert!(level_at(mid, 440.0, RATE as f64) > 0.1 && level_at(mid, 660.0, RATE as f64) > 0.1, "both songs in the middle of the mix");
    assert!((pitch_hz(b_alone, RATE as f64) - 660.0).abs() < 0.5, "b at its own pitch after the mix");
    assert!(p.sink.gaps.is_empty() && p.sink.timestamp_jumps == 0);
}
