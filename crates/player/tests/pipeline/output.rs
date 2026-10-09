//! Output feeding: bursts, and one format per queue with conversion or reopening.

use std::f64::consts::TAU;

use nori_player::burst::{BUFFER_US, LOW_US};
use nori_player::playlist::Hand;
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
    assert_eq!((p.sink.opens, p.sink.rebuilds), (2, 1), "opened again for b");
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

/// What moves the music while the next song, at another rate, is read ahead.
#[derive(Debug, Clone, Copy)]
enum Move {
    /// Previous restarting the song (the engine jumps to it again).
    Restart,
    SeekStart,
    SeekMid,
    /// Previous to the song before.
    Previous,
    JumpToNext,
    /// Let go as after a long pause, and back where it was.
    Release,
    /// A song at the playing one's rate added to play next: the ending is made again.
    NextAdded,
}

/// Regression: with the output waiting to reopen in the next song's format, a jump, a seek, a release
/// or an ending made again left the wait in place, and the music from there went through the output in
/// the other format (too slow or too fast, or its channels taken as frames), and stayed so after a
/// pause.
#[test]
fn moves_while_the_next_format_waits_play_in_the_songs_format() {
    const SECS: u64 = 30;
    let song = |id: &str, (rate, channels): (u32, usize), hz: f64| {
        let tone: Vec<i16> = tone_at(rate, hz, 1.0).into_iter().step_by(3 - channels).collect();
        Track::new(id, Audio::looped(rate, channels, &tone, SECS * rate as u64))
    };
    // z, a (played), b (read ahead), and x (added after a, in its format).
    let hz = [330.0, 440.0, 660.0, 550.0];
    let moves = [Move::Restart, Move::SeekStart, Move::SeekMid, Move::Previous, Move::JumpToNext, Move::Release, Move::NextAdded];
    let mut wrong = Vec::new();
    let (s48, s44, s32, m48, m44) = ((48_000, 2), (44_100, 2), (32_000, 2), (48_000, 1), (44_100, 1));
    for [z, a, b] in [[s48, s48, s44], [s44, s48, s44], [s32, s48, s44], [s44, s44, s48], [s48, s48, m48], [m44, m44, s44], [s44, m44, m48]] {
        let formats = [z, a, b, a];
        // A mix plays out as it began: nothing is made again.
        for (mv, mix) in moves.into_iter().flat_map(|m| [(m, false), (m, true)]).filter(|&(m, mix)| !(mix && matches!(m, Move::NextAdded))) {
            let case = format!("{formats:?} {mv:?}{}", if mix { " crossfading" } else { "" });
            let songs = (0..3).map(|k| song(["z", "a", "b"][k], formats[k], hz[k])).collect();
            let mut p = if mix { Player::with_prefs(songs, crossfade(6)) } else { Player::new(songs) };
            p.play_from(1);
            let ahead = |p: &mut Player| if mix { p.mixing() } else { p.sink.reopening() };
            assert!(p.run_until(SECS as i64 * 1000, ahead), "{case}: b is read ahead");
            // The songs heard 2 s after the move and 16 s after it (a pause between).
            let (soon, later) = match mv {
                Move::Restart => {
                    p.jump(1, 0);
                    (1, 1)
                }
                Move::SeekStart => {
                    p.seek(0);
                    (1, 1)
                }
                Move::SeekMid => {
                    p.seek(10_000);
                    (1, 1)
                }
                Move::Previous => {
                    // In a mix the queue is on b already.
                    let to = p.queue.read(|q| q.previous()).expect("a song before");
                    assert!(p.previous());
                    (to, to)
                }
                Move::JumpToNext => {
                    p.jump(2, 0);
                    (2, 2)
                }
                Move::Release => {
                    // Some 20 s into a: b by then.
                    let (i, ms) = p.release().expect("a place");
                    p.jump(i, ms);
                    (i, 2)
                }
                Move::NextAdded => {
                    p.tracks.push(song("x", formats[3], hz[3]));
                    p.queue.live.add(vec!["x".into()], Hand::Next);
                    p.queue_changed();
                    p.replan_ending();
                    assert!(p.app.logged("the ending of a is made again"), "{case}: {:?}", p.app.log);
                    (1, 3)
                }
            };
            // The output's rate and channels, and the pitch heard in them over the last half second.
            let heard = |p: &Player| {
                let f = p.sink.format.expect("open");
                let heard: Vec<f64> = p.sink.heard_samples().into_iter().step_by(f.channels).map(f64::from).collect();
                ((f.rate, f.channels), pitch_hz(&heard[heard.len() - f.rate as usize / 2..], f.rate as f64))
            };
            p.run_for(2_000);
            let first = heard(&p);
            p.pause();
            p.run_for(1_000);
            p.resume();
            p.run_for(14_000);
            for (when, to, (format, pitch)) in [("", soon, first), (" after a pause", later, heard(&p))] {
                // A converter running for the mix may go on converting the song: right pitch, other format.
                if (format != formats[to] && !mix) || (pitch - hz[to]).abs() > 1.0 {
                    wrong.push(format!("{case}{when}: {pitch:.1} Hz in {format:?}, the song is {} Hz in {:?}", hz[to], formats[to]));
                }
            }
            if !p.sink.gaps.is_empty() {
                wrong.push(format!("{case}: gaps {:?}", p.sink.gaps));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
