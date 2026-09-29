//! Gapless joins: every sample of one song, then every sample of the next.

use nori_player::sim::{Audio, Player, Track};

use crate::common::*;

/// Plays the queue to its end and checks the output never ran dry or stuttered on the way.
fn play_all(p: &mut Player, max_ms: i64) {
    p.play_from(0);
    assert!(p.run_to_end(max_ms), "the queue played to its end");
    assert!(p.sink.gaps.is_empty(), "the output never ran dry: {:?}", p.sink.gaps);
    assert_eq!(p.sink.timestamp_jumps, 0, "no timestamp jumped (a stutter on a phone)");
    assert_eq!(p.sink.rebuilds, 0, "the output was never opened again");
}

#[test]
fn split_signal_plays_back_whole() {
    // Cut at a frame no buffer size divides, so the join falls inside the output's buffers.
    let whole = music(20.0, 7);
    let cut = frames(9.0) * 2 + 2 * 377;
    let mut p = Player::new(vec![track("a", &whole[..cut]), track("b", &whole[cut..])]);
    play_all(&mut p, 60_000);
    let heard = p.sink.heard_samples();
    assert_eq!(heard.len(), whole.len(), "no sample added or dropped");
    assert!(heard == whole, "sample for sample the same, first difference at {:?}", heard.iter().zip(&whole).position(|(a, b)| a != b));
    assert_eq!(p.changes.iter().map(|c| c.1).collect::<Vec<_>>(), vec![0, 1], "the player moved on to the second song");
}

#[test]
fn gapless_join_exact_through_limiter() {
    let whole = music(12.0, 17);
    let cut = frames(5.0) * 2 + 2 * 91;
    let mut p = Player::new(vec![track("a", &whole[..cut]), track("b", &whole[cut..])]);
    p.set_sound(nori_player::sim::Sound { limiter: true, ..Default::default() });
    play_all(&mut p, 30_000);
    // The look-ahead is carried across the boundary: the join is where it was, 220 frames later, and
    // the end of the queue brings out the last 220 the limiter held back.
    let heard = p.sink.heard_samples();
    assert!(heard[..440].iter().all(|&v| v == 0) && heard[440..] == whole[..]);
}

#[test]
fn mp3_gapless_join() {
    let audio = Audio::mp3(&testdata("tone440.mp3"));
    let once = audio.decode_all();
    let mut p = Player::new(vec![Track::new("a", audio.clone()), Track::new("b", audio.clone()), Track::new("c", audio)]);
    play_all(&mut p, 30_000);
    let heard = p.sink.heard_samples();
    assert_eq!(heard.len(), once.len() * 3);
    for (k, part) in heard.chunks(once.len()).enumerate() {
        assert!(part == once, "song {k} is the decoder's output exactly");
    }
}

#[test]
fn opus_gapless_join() {
    let audio = Audio::opus(&testdata("tone440.opus"));
    let once = audio.decode_all();
    assert!(once.len() / 2 >= 48_000, "a second of tone, pre-skip gone: {}", once.len() / 2);
    let mut p = Player::new(vec![Track::new("a", audio.clone()), Track::new("b", audio)]);
    play_all(&mut p, 30_000);
    let heard = p.sink.heard_samples();
    assert!(heard.len() == once.len() * 2 && heard[..once.len()] == once[..] && heard[once.len()..] == once[..]);
}

#[test]
fn album_in_order_stays_gapless() {
    let whole = music(40.0, 11);
    let cut = frames(21.0) * 2;
    let a = track("a", &whole[..cut]).on_album("x", 1);
    let b = track("b", &whole[cut..]).on_album("x", 2);
    let mut p = Player::with_prefs(vec![a, b], crossfade(6)).as_album();
    play_all(&mut p, 90_000);
    assert!(p.app.logged("planFor: gapless (same album in order"), "{:?}", p.app.log);
    assert!(p.sink.heard_samples() == whole, "the album plays straight on, sample for sample");
}

/// The transitions the log says were planned, "a -> b" each.
fn mixed(p: &Player) -> Vec<String> {
    p.app.log.iter().filter_map(|l| l.strip_prefix("transition ")).filter(|l| l.contains(" -> ")).map(|l| l.split(':').next().unwrap_or("").to_string()).collect()
}

#[test]
fn album_songs_queued_separately_mix() {
    let whole = music(40.0, 11);
    let cut = frames(21.0) * 2;
    // Scar Tissue, then Californication, each queued on its own: one album, in order, never played as it.
    let a = track("a", &whole[..cut]).on_album("x", 1);
    let b = track("b", &whole[cut..]).on_album("x", 2);
    let mut p = Player::with_prefs(vec![a, b], crossfade(6));
    play_all(&mut p, 90_000);
    assert_eq!(mixed(&p), ["a -> b"], "{:?}", p.app.log);
    assert!(!p.app.logged("same album in order"), "{:?}", p.app.log);
    assert!(p.sink.heard_samples().len() < whole.len(), "six seconds overlapped");
}

#[test]
fn same_song_thrice_mixes() {
    let song = music(20.0, 5);
    let mut p = Player::with_prefs((0..3).map(|_| track("a", &song).on_album("x", 3)).collect(), crossfade(4));
    play_all(&mut p, 90_000);
    assert_eq!(p.app.log.iter().filter(|l| l.starts_with("mixing:")).count(), 2, "{:?}", p.app.log);
    assert!(!p.app.logged("same album in order"), "{:?}", p.app.log);
}

#[test]
fn album_added_whole_gapless_inside() {
    // A song of the album queued on its own, the album added whole after it, then autofill's song of the
    // same album: only the album's own run is kept whole.
    let whole = music(60.0, 13);
    let (c1, c2) = (frames(20.0) * 2, frames(40.0) * 2);
    let before = track("s", &music(20.0, 3)).on_album("x", 1);
    let a1 = track("a1", &whole[..c1]).on_album("x", 1);
    let a2 = track("a2", &whole[c1..c2]).on_album("x", 2);
    let a3 = track("a3", &whole[c2..]).on_album("x", 3);
    let after = track("t", &music(20.0, 4)).on_album("x", 4);
    let mut p = Player::with_prefs(vec![before, a1, a2, a3, after], crossfade(4));
    p.queue.as_album(1, 4);
    p.queue_changed();
    play_all(&mut p, 200_000);
    assert_eq!(mixed(&p), ["s -> a1", "a3 -> t"], "{:?}", p.app.log);
    // Between the mix into its first song and the mix out of its last, the album sample for sample.
    let heard = p.sink.heard_samples();
    let inner = &whole[frames(5.0) * 2..whole.len() - frames(5.0) * 2];
    let from = heard.windows(64).position(|w| w == &inner[..64]).expect("the album is heard");
    assert!(heard.len() >= from + inner.len() && heard[from..from + inner.len()] == *inner, "the album's songs one straight into the next");
}

#[test]
fn shuffled_album_mixes() {
    let whole = music(40.0, 11);
    let cut = frames(21.0) * 2;
    let a = track("a", &whole[..cut]).on_album("x", 1);
    let b = track("b", &whole[cut..]).on_album("x", 2);
    let mut p = Player::shuffled(vec![a, b], 1).as_album();
    p.app.prefs = crossfade(6);
    let first = p.queue.current().unwrap();
    p.play_from(first);
    assert!(p.run_to_end(90_000));
    assert_eq!(mixed(&p).len(), 1, "{:?}", p.app.log);
    assert!(!p.app.logged("same album in order"), "{:?}", p.app.log);
}

#[test]
fn long_song_simulates_fast() {
    // Two ten-minute songs, a twelve-second crossfade: twenty minutes of music on the virtual clock.
    let started = std::time::Instant::now();
    let (a, b) = (sine(441.0, 0.5, 1.0), sine(551.25, 0.5, 1.0));
    let ten = frames(600.0) as u64;
    let tracks = vec![Track::new("a", Audio::looped(RATE, 2, &a, ten)), Track::new("b", Audio::looped(RATE, 2, &b, ten))];
    let mut p = Player::with_prefs(tracks, crossfade(12));
    // Only the join is kept: 30 s before the crossfade to 30 s after it.
    let join = ten - frames(12.0) as u64;
    p.sink.capture = join - frames(30.0) as u64..join + frames(42.0) as u64;
    p.play_from(0);
    assert!(p.run_to_end(1_300_000), "twenty minutes played");
    assert!(p.sink.gaps.is_empty(), "no hole anywhere: {:?}", p.sink.gaps);
    assert!(p.app.logged("transition a -> b: EqualPowerFade 12000 ms at 588000"), "{:?}", p.app.log);
    assert!(!p.app.logged("letting the ending play"), "the ending was held for the mix: {:?}", p.app.log);
    assert_eq!(p.sink.heard_frames, 2 * ten - frames(12.0) as u64, "the overlap is heard once, nothing else is lost");
    let heard = p.sink.heard_samples();
    let (a_run, b_run) = (a.repeat(30), b.repeat(42));
    let (at, overlap) = (frames(30.0) * 2, frames(12.0) * 2);
    assert!(heard[..at] == a_run[..at], "the ending of a alone, untouched, up to the planned sample");
    let mix = reference_mix(&a_run[..overlap], &b_run[..overlap], &blind_plan(600_000, 600_000, 12.0));
    assert!(heard[at..at + overlap] == mix[..], "the mix is the mixer's, starting on the planned sample");
    assert!(heard[at + overlap..] == b_run[overlap..frames(42.0) * 2], "then b alone, exactly where the overlap left it");
    // The whole point of a virtual clock: twenty minutes of music on a real one would take twenty minutes.
    // Far above what it takes on a busy machine, far below what a clock that waited would.
    assert!(started.elapsed().as_secs_f64() < 60.0, "twenty minutes of music took {:?} of real time", started.elapsed());
}
