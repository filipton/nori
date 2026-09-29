//! Crossfades between songs, from the setting to the samples: planned for the right boundary, the
//! ending held until the next song arrives, the mix exactly the mixer's and starting on the planned
//! sample, equal power through the overlap, and the player and the seek bar following the ear.

use nori_player::automix::mixer::Mixer;
use nori_player::playlist::Hand;
use nori_player::sim::{prefs_off, Player};

use crate::common::*;

const SONG_S: f64 = 45.0;
const SONG_MS: i64 = 45_000;

fn three() -> (Vec<i16>, Vec<i16>, Vec<i16>) {
    (music(SONG_S, 1), music(SONG_S, 2), music(SONG_S, 3))
}

fn player(prefs_secs: i32) -> (Player, Vec<i16>, Vec<i16>, Vec<i16>) {
    let (a, b, c) = three();
    let p = Player::with_prefs(vec![track("a", &a), track("b", &b), track("c", &c)], crossfade(prefs_secs));
    (p, a, b, c)
}

/// What a seek bar shows: the song (queue index) and the place in it.
fn shown(p: &mut Player) -> (usize, i64) {
    let seen = p.bar();
    (seen.index.or(p.current()).unwrap_or(usize::MAX), seen.ms)
}

#[test]
fn a_seek_just_before_a_crossfade_keeps_the_bar_with_the_ear() {
    // Seeking to nine seconds before a four-second mix: the ending is held at once, the next song
    // arrives at once, and the mix goes to the output seconds before it is heard. The output's clock
    // jumps to the next song's time as the mix is offered; the bar must not jump with it.
    let (a, b) = (music(43.0, 6), music(60.0, 7));
    let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b)], crossfade(4));
    p.play_from(0);
    p.run_for(1_000);
    p.seek(30_000);
    for k in 1..=12 {
        p.run_for(250);
        let (song, ms) = shown(&mut p);
        assert_eq!(song, 0, "still a");
        assert!((ms - (30_000 + k * 250)).abs() < 100, "{} ms after the seek the bar is at {ms}", k * 250);
    }
}

#[test]
fn a_seek_just_before_a_crossfade_keeps_the_bar_with_the_ear_while_tuning() {
    // The same, with the equalizer screen open (bursting is off from the next boundary). The simulated
    // output reads its clock fresh on every call, so the Android-only case - a clock read cached from
    // before the mix was offered, fixed in burst.rs's `after_offer` - does not show here; this keeps the
    // tuning path itself covered.
    let (z, a, b) = (music(12.0, 5), music(43.0, 6), music(60.0, 7));
    let mut p = Player::with_prefs(vec![track("z", &z), track("a", &a), track("b", &b)], crossfade(4));
    p.set_tuning(true);
    p.play_from(0);
    p.run_for(14_000);
    assert_eq!(shown(&mut p).0, 1, "past the first boundary, into a, with bursting off");
    p.seek(30_000);
    for k in 1..=12 {
        p.run_for(250);
        let (song, ms) = shown(&mut p);
        assert_eq!(song, 1, "still a");
        assert!((ms - (30_000 + k * 250)).abs() < 100, "{} ms after the seek the bar is at {ms}", k * 250);
    }
}

#[test]
fn a_crossfade_is_planned_for_the_next_boundary_and_heard_exactly_there() {
    let (mut p, a, b, _) = player(12);
    p.play_from(0);
    assert!(p.run_until(10_000, |p| p.app.logged("transition a -> b: EqualPowerFade 12000 ms at 33000")), "{:?}", p.app.log);
    assert!(p.run_until(60_000, |p| p.mixing()), "the sink reaches the mix");
    assert!(p.app.logged("mixing: the next track arrived"), "the next track arrived in time to be mixed: {:?}", p.app.log);
    assert!(!p.app.logged("letting the ending play"), "the ending was not let go for want of it");
    assert!(p.run_until(20_000, |p| p.current() == Some(1)), "the next track plays out of the mix");
    p.run_for(15_000);
    let heard = p.sink.heard_samples();
    let (start, overlap) = (frames(33.0) * 2, frames(12.0) * 2);
    assert!(heard[..start] == a[..start], "a alone up to the planned sample");
    let mix = reference_mix(&a[start..], &b[..overlap], &blind_plan(SONG_MS, SONG_MS, 12.0));
    assert!(heard[start..start + overlap] == mix[..], "the mix, sample for sample, from the planned sample on");
    assert!(heard[start + overlap..] == b[overlap..heard.len() - start], "then b alone");
    assert!(p.sink.gaps.is_empty() && p.sink.timestamp_jumps == 0, "no hole, no stutter: {:?}", p.sink.gaps);
}

#[test]
fn switched_on_mid_song_it_is_planned_for_the_song_already_playing() {
    let (a, b, _) = three();
    let mut p = Player::new(vec![track("a", &a), track("b", &b)]);
    p.play_from(0);
    p.run_for(5_000);
    assert!(p.app.logged("planFor: off"), "{:?}", p.app.log);
    // Turning the setting on and waiting for this song to end is how anyone tries the feature out.
    p.set_prefs(crossfade(12));
    assert!(p.run_until(10_000, |p| p.app.logged("transition a -> b")), "{:?}", p.app.log);
    assert!(p.run_until(60_000, |p| p.mixing()));
    assert!(p.run_to_end(60_000));
    assert_eq!(p.sink.heard_frames as usize, frames(2.0 * SONG_S - 12.0), "twelve seconds of overlap");
}

#[test]
fn a_song_put_next_is_planned_into_at_once() {
    // "Play next" on a song already under way: the plan out of it was made for the song that followed
    // before, and it is made again for the new one without waiting for anything else to change.
    let (mut p, ..) = player(4);
    p.play_from(0);
    assert!(p.run_until(10_000, |p| p.app.logged("transition a -> b")), "{:?}", p.app.log);
    p.tracks.push(track("x", &music(SONG_S, 9)));
    p.queue.add(vec!["x".into()], Hand::Next);
    p.queue_changed();
    assert!(p.run_until(12_000, |p| p.app.logged("transition a -> x")), "{:?}", p.app.log);
    assert!(p.run_until(60_000, |p| p.current_id().as_deref() == Some("x")), "x plays next: {:?}", p.app.log);
}

#[test]
fn the_bar_walks_steadily_through_the_held_ending() {
    let (mut p, ..) = player(12);
    p.play_from(0);
    p.run_for(3_000);
    p.seek(SONG_MS - 24_000);
    let mut seen = Vec::new();
    for _ in 0..8 {
        p.run_for(1_000);
        let (song, ms) = shown(&mut p);
        if song != 0 {
            break;
        }
        seen.push(ms);
    }
    // The player counts the held ending as played the moment it is decoded; the bar must not.
    assert!(seen.windows(2).all(|w| w[1] >= w[0] && w[1] - w[0] <= 1_100), "steady, one second a second: {seen:?}");
    assert!(*seen.last().unwrap() > SONG_MS - 22_000 + 5_000, "{seen:?}");
    // Up to the mix it stays on a, and on into the fade while a is the louder of the two: an
    // equal-power fade hands over in its middle, six seconds into twelve. There the bar is on b, six
    // seconds into it, never on b while b could not be heard yet.
    assert!(p.run_until(20_000, |p| p.mixing()));
    let mut last = shown(&mut p).1;
    let mut on_a = 0;
    while shown(&mut p).0 == 0 {
        p.run_for(100);
        let ms = shown(&mut p).1;
        if shown(&mut p).0 == 0 {
            assert!(ms >= last && ms - last <= 200, "a walks on through the first half of the fade: {last} -> {ms}");
            last = ms;
        }
        on_a += 100;
        assert!(on_a <= 7_000, "a handed over by the middle of the fade");
    }
    assert!((5_800..=6_200).contains(&on_a), "a is shown until the fade's middle, not its start: {on_a} ms");
    let (song, ms) = shown(&mut p);
    assert_eq!(song, 1, "the bar follows the ear into b once b is the louder");
    assert!((5_800..6_300).contains(&ms), "six seconds into b: {ms}");
    let mut last = ms;
    for _ in 0..20 {
        p.run_for(500);
        let (song, ms) = shown(&mut p);
        assert_eq!(song, 1, "and never falls back to a");
        assert!(ms >= last && ms - last <= 600, "{last} -> {ms}");
        last = ms;
    }
}

#[test]
fn a_scrub_into_the_mix_stays_to_hear_the_ending_and_the_mix_still_fires() {
    let (mut p, _, b, c) = player(12);
    p.play_from(1);
    p.run_for(3_000);
    assert!(p.app.logged("transition b -> c: EqualPowerFade 12000 ms at 33000"), "{:?}", p.app.log);
    // Eight seconds from the end is four seconds into a twelve-second crossfade.
    p.seek(SONG_MS - 8_000);
    let from = p.sink.heard_frames as usize;
    assert!(p.run_until(2_000, |p| p.app.logged("late hold, 4000 ms in")), "{:?}", p.app.log);
    assert_eq!(p.current(), Some(1), "the scrub stays on the song whose ending it asked for");
    assert!(p.run_until(10_000, |p| p.app.logged("mixing: the next track arrived")), "the mix still fires after the late seek: {:?}", p.app.log);
    assert!(p.run_until(10_000, |p| p.current() == Some(2)), "and c plays out of it");
    p.run_for(12_000);
    // What is heard from the seek on: the mix from four seconds in, curves and c both as far along as
    // they would have been had b played into it.
    let heard = &p.sink.heard_samples()[from * 2..];
    let late = frames(4.0);
    let mut m = Mixer::new(RATE, 2);
    m.configure(&blind_plan(SONG_MS, SONG_MS, 12.0));
    m.seek(late as u64);
    let rest = frames(8.0) * 2;
    let mut mix = b[b.len() - rest..].to_vec();
    m.process(&mut mix, &c[late * 2..late * 2 + rest]);
    let start = heard.iter().position(|&v| v != 0).unwrap_or(0) / 2 * 2;
    assert!(heard[start..start + rest] == mix[..], "the mix from where the scrub landed, sample for sample");
    assert!(heard[start + rest..start + rest + frames(2.0) * 2] == c[late * 2 + rest..late * 2 + rest + frames(2.0) * 2], "then c alone");
}

#[test]
fn pausing_anywhere_changes_nothing_that_is_heard() {
    let (a, b, _) = three();
    let whole = {
        let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b)], crossfade(12));
        p.play_from(0);
        assert!(p.run_to_end(120_000));
        p.sink.heard_samples()
    };
    let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b)], crossfade(12));
    p.play_from(0);
    // Paused while a plays alone, while its ending is held, in the middle of the mix and after it.
    for (play, pause) in [(5_000, 3_000), (20_000, 7_000), (8_000, 60_000), (4_000, 1_000), (10_000, 2_000)] {
        p.run_for(play);
        p.pause();
        p.run_for(pause);
        p.resume();
    }
    assert!(p.run_to_end(120_000));
    assert!(p.sink.gaps.is_empty(), "{:?}", p.sink.gaps);
    assert!(p.sink.heard_samples() == whole, "the same samples, however often it was paused");
}

#[test]
fn a_seek_back_out_of_the_held_ending_plays_the_song_on_and_mixes_again() {
    let (a, b, _) = three();
    let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b)], crossfade(12));
    p.play_from(0);
    // Decode runs seconds ahead: the ending is held long before it is heard.
    assert!(p.run_until(40_000, |p| p.app.logged("holding the ending")));
    assert!(p.position_ms() < 33_000);
    p.seek(10_000);
    let from = p.sink.heard_frames as usize;
    assert!(p.run_to_end(80_000));
    let heard = &p.sink.heard_samples()[from * 2..];
    let (start, overlap) = (frames(23.0) * 2, frames(12.0) * 2);
    assert!(heard[..start] == a[frames(10.0) * 2..frames(33.0) * 2], "a from the seek up to the planned start");
    let mix = reference_mix(&a[frames(33.0) * 2..], &b[..overlap], &blind_plan(SONG_MS, SONG_MS, 12.0));
    assert!(heard[start..start + overlap] == mix[..], "the same mix, sample for sample");
    assert!(heard[start + overlap..] == b[overlap..], "then b to its end");
}

#[test]
fn an_ending_held_for_a_song_that_will_not_play_is_let_go_whole() {
    let (a, b, c) = three();
    let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b), track("c", &c)], crossfade(12));
    p.tracks.broken = vec!["b".into()];
    p.play_from(0);
    assert!(p.run_until(120_000, |p| p.current() == Some(2)), "{:?}", p.app.log);
    p.run_for(5_000);
    assert!(p.app.logged("b will not play: skipped"), "{:?}", p.app.log);
    let heard = p.sink.heard_samples();
    assert!(heard[..a.len()] == a[..], "a to its very end, its held ending played out unmixed");
    assert!(heard[a.len()..] == c[..heard.len() - a.len()], "then c from its start");
}

#[test]
fn a_crossfade_at_one_and_a_half_times_still_meets_the_next_song() {
    let (a, b, _) = three();
    let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b)], crossfade(12));
    p.set_speed(1.5, 1.0);
    p.play_from(0);
    assert!(p.run_to_end(120_000));
    assert!(p.app.logged("mixing: the next track arrived"), "{:?}", p.app.log);
    assert!(!p.app.logged("letting the ending play"), "{:?}", p.app.log);
    assert!(p.sink.gaps.is_empty(), "{:?}", p.sink.gaps);
    // 78 s of music (two songs less the overlap) in two thirds of the time.
    let secs = p.sink.heard_frames as f64 / RATE as f64;
    assert!((secs - 52.0).abs() < 0.5, "{secs:.2} s");
}

#[test]
fn with_it_off_the_planner_says_so_rather_than_going_quiet() {
    let (mut p, ..) = player(12);
    p.play_from(0);
    p.run_for(2_000);
    p.set_prefs(prefs_off());
    assert!(p.run_until(10_000, |p| p.app.logged("planFor: off (transitionsOff=false autoMix=false crossfadeSec=0)")), "{:?}", p.app.log);
    assert!(p.run_to_end(150_000));
    assert!(!p.app.logged("holding the ending"), "nothing held");
    assert_eq!(p.sink.heard_frames as usize, frames(3.0 * SONG_S), "three whole songs, back to back");
}

#[test]
fn an_equal_power_fade_keeps_the_level_of_uncorrelated_songs() {
    // Two independent noises at the same level: through an equal-power fade their sum keeps that level.
    let (a, b) = (noise(0.3, 40.0, 11), noise(0.3, 40.0, 12));
    let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b)], crossfade(10));
    p.play_from(0);
    assert!(p.run_to_end(120_000));
    let heard = left(&p.sink.heard_samples());
    let steady = rms(&heard[frames(5.0)..frames(25.0)]);
    let window = frames(0.1);
    let (from, to) = (frames(29.0), frames(41.0));
    let levels: Vec<f64> = heard[from..to].chunks(window).map(|w| db(rms(w) / steady)).collect();
    let (lo, hi) = levels.iter().fold((f64::MAX, f64::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    assert!(lo > -0.5 && hi < 0.5, "no dip or bump through the overlap: {lo:.2} .. {hi:.2} dB");
}

#[test]
fn a_crossfade_plays_the_curve_and_the_fade_in_asked_for() {
    use nori_player::transitions::TransitionPrefs;
    use nori_player::types::FadeCurve;
    // The outgoing song at 440 Hz, the incoming one at 1 kHz: each side's gain read by its own tone.
    let (a, b) = (sine(440.0, 0.3, 40.0), sine(1000.0, 0.3, 40.0));
    let heard = |prefs: TransitionPrefs| {
        let mut p = Player::with_prefs(vec![track("a", &a), track("b", &b)], prefs);
        p.play_from(0);
        assert!(p.run_to_end(120_000));
        left(&p.sink.heard_samples())
    };
    // A ten-second crossfade from 30 s: where each side is at a moment, as a share of its own level.
    let at = |x: &[f64], hz: f64, secs: f64| level_at(&x[frames(30.0 + secs - 0.25)..frames(30.0 + secs + 0.25)], hz, RATE as f64) / 0.3;
    let plain = heard(crossfade(10));
    let rise = (0.3 * std::f64::consts::FRAC_PI_2).sin();
    assert!((at(&plain, 1000.0, 3.0) - rise).abs() < 0.05, "equal power, by default: {}", at(&plain, 1000.0, 3.0));
    let shaped = heard(TransitionPrefs { fade_curve: FadeCurve::Linear, fade_in_ms: 2_000, ..crossfade(10) });
    assert!(at(&shaped, 1000.0, 3.0) > 0.97, "the incoming song is up after two seconds: {}", at(&shaped, 1000.0, 3.0));
    assert!((at(&shaped, 440.0, 5.0) - 0.5).abs() < 0.05, "the outgoing one falls in a straight line, half way at the middle: {}", at(&shaped, 440.0, 5.0));
}
