//! Controls and queue: next/previous, seeks, failing songs, repeat and shuffle.

use nori_player::playlist::{Hand, REPEAT_ALL, REPEAT_ONE};
use nori_player::sim::{Audio, Player, Track};

use crate::common::*;

fn songs(n: usize, secs: f64) -> Vec<Vec<i16>> {
    (0..n).map(|k| music(secs, 100 + k as u64)).collect()
}

fn queue(songs: &[Vec<i16>]) -> Vec<Track> {
    songs.iter().enumerate().map(|(k, s)| track(&format!("s{k}"), s)).collect()
}

/// What was heard from frame `from` on.
fn heard_from(p: &Player, from: u64) -> Vec<i16> {
    p.sink.heard_samples()[from as usize * 2..].to_vec()
}

#[test]
fn next_previous_start_at_first_sample() {
    let s = songs(3, 10.0);
    let mut p = Player::new(queue(&s));
    p.play_from(0);
    p.run_for(2_000);
    let at = p.sink.heard_frames;
    assert!(p.next());
    assert_eq!(p.current(), Some(1));
    p.run_for(2_000);
    let heard = heard_from(&p, at);
    assert!(heard[..frames(1.9) * 2] == s[1][..frames(1.9) * 2], "the next song from its first sample, nothing of the old one");
    let at = p.sink.heard_frames;
    assert!(p.previous());
    assert_eq!(p.current(), Some(0));
    p.run_for(2_000);
    assert!(heard_from(&p, at)[..frames(1.9) * 2] == s[0][..frames(1.9) * 2], "previous goes back to the start of the song before");
    assert!(p.next() && p.next());
    assert!(!p.next(), "nothing after the last song with repeat off");
    assert!(p.run_to_end(15_000));
    assert!(p.sink.gaps.is_empty() && p.sink.timestamp_jumps == 0);
}

#[test]
fn next_mid_crossfade_cuts_cleanly() {
    let s = songs(3, 40.0);
    let mut p = Player::with_prefs(queue(&s), crossfade(10));
    p.play_from(0);
    assert!(p.run_until(60_000, |p| p.mixing()));
    p.run_for(3_000);
    let at = p.sink.heard_frames;
    assert!(p.next());
    p.run_for(2_000);
    assert_eq!(p.current(), Some(2));
    assert!(heard_from(&p, at)[..frames(1.9) * 2] == s[2][..frames(1.9) * 2], "the song after, from its start, with no mix left over");
    assert!(!p.mixing());
}

#[test]
fn seeks_land_on_sample() {
    let song = music(30.0, 7);
    let mut p = Player::new(vec![track("a", &song)]);
    p.play_from(0);
    p.run_for(2_000);
    let at = p.sink.heard_frames;
    p.seek(12_345);
    assert_eq!(p.position_ms(), 12_345, "the player is where the seek asked at once");
    p.run_for(1_000);
    let target = 12_345 * RATE as usize / 1000;
    let heard = heard_from(&p, at);
    assert!(heard == song[target * 2..target * 2 + heard.len()], "from exactly that sample on");
    // The position is the seek's place plus exactly what has been heard since, to the millisecond.
    let since_ms = heard.len() as i64 / 2 * 1000 / RATE as i64;
    assert!((p.position_ms() - (12_345 + since_ms)).abs() <= 1, "{} after {since_ms} ms heard", p.position_ms());

    // Mp3 seek is sample exact.
    let audio = Audio::mp3(&testdata("tone440.mp3"));
    let whole = audio.decode_all();
    let mut p = Player::new(vec![Track::new("a", audio)]);
    p.play_from(0);
    p.run_for(100);
    let at = p.sink.heard_frames;
    p.seek(500);
    p.run_for(600);
    let heard = heard_from(&p, at);
    // 500 ms is frame 22050; the MPEG frame holding it starts at 19 * 1152 = 21888. That is where the
    // seek lands (within one frame, as every MP3 player's), and what comes out is that place in the song.
    let landed = 19 * 1152;
    // After a reset the decoder's filterbank takes a frame to fill; from the third frame on the samples
    // are the ones a decode from the start gives, to the bit.
    let settled = 2 * 1152 * 2;
    assert!(heard.len() > settled + 4608);
    assert!(heard[settled..heard.len().min(whole.len() - landed * 2)] == whole[landed * 2 + settled..landed * 2 + heard.len().min(whole.len() - landed * 2)], "the seek landed on frame {landed}");

    // Opus seek is sample exact.
    let audio = Audio::opus(&testdata("tone440.opus"));
    let whole = left(&audio.decode_all());
    let mut p = Player::new(vec![Track::new("a", audio)]);
    p.play_from(0);
    p.run_for(100);
    let at = p.sink.heard_frames;
    p.seek(500);
    p.run_for(600);
    let heard = left(&heard_from(&p, at));
    // 500 ms is sample 24000. The packet a seek lands on starts 80 ms earlier, and the decoder plays
    // those 80 ms in again and drops them, so the first sample out is the packet boundary at or before
    // the place asked for: 21 * 960 - 312 (the pre-skip) + 3840 = 23688.
    let landed = 23_688;
    // The file is a pure tone, which repeats every 1200 samples (11 cycles of 440 Hz at 48 kHz), so the
    // alignment is checked within half of that either way.
    let best = (landed - 500..=landed + 500)
        .map(|lag| (lag, heard[..9_600].iter().zip(&whole[lag..]).map(|(a, b)| (a - b).powi(2)).sum::<f64>()))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert_eq!(best.0, landed, "the samples heard are the song's from {landed} on");
    // The decoder starts over at the seek, so its predictions converge on the song's as it goes: about
    // -20 dB off in the first 20 ms after the pre-roll, and gone into the noise within a tenth of a
    // second. That is the pre-roll being too short for this decoder, not the seek landing wrong.
    let off = |from: usize| {
        let (a, b) = (from, from + 960);
        let e = heard[a..b].iter().zip(&whole[landed + a..landed + b]).map(|(x, y)| (x - y).powi(2)).sum::<f64>() / 960.0;
        db(e.sqrt() / rms(&whole[landed + a..landed + b]))
    };
    assert!(off(0) < -18.0, "first 20 ms {:.1} dB off", off(0));
    assert!(off(4_800) < -45.0, "100 ms on {:.1} dB off", off(4_800));
    assert!(off(9_600) < -70.0, "200 ms on {:.1} dB off", off(9_600));
}

/// How far apart two positions may be read: one turn of the renderer.
const STEP: i64 = 10;

#[test]
fn seek_while_paused_sticks() {
    let song = music(40.0, 8);
    let mut p = Player::new(vec![track("a", &song)]);
    p.play_from(0);
    p.run_for(3_000);
    p.pause();
    p.seek(20_000);
    let at = p.sink.heard_frames;
    p.run_for(3_000);
    assert_eq!(p.position_ms(), 20_000, "paused on the place asked for");
    assert_eq!(p.sink.heard_frames, at, "and nothing plays while paused");
    p.resume();
    p.run_for(1_000);
    let heard = heard_from(&p, at);
    let target = frames(20.0);
    assert!(heard == song[target * 2..target * 2 + heard.len()], "play resumes from the seek");
    assert!((p.position_ms() - 21_000).abs() <= STEP, "{}", p.position_ms());
}

#[test]
fn failing_songs() {
    let s = songs(6, 4.0);
    let mut p = Player::new(queue(&s));
    p.tracks.broken = ["s1", "s2", "s3", "s4"].map(String::from).to_vec();
    p.play_from(0);
    p.run_for(15_000);
    let skipped: Vec<&str> = p.app.log.iter().filter(|l| l.contains("will not play")).map(String::as_str).collect();
    assert_eq!(skipped, ["s1 will not play: skipped", "s2 will not play: skipped", "s3 will not play: skipped", "s4 will not play: stopped"]);
    assert!(!p.playing(), "stopped after three in a row");
    assert_eq!(p.sink.heard_frames as usize, frames(4.0), "only the first song was heard; s5 never played");

    // Playing song resets error run.
    let s = songs(7, 3.0);
    let mut p = Player::new(queue(&s));
    p.tracks.broken = ["s1", "s3", "s4", "s5"].map(String::from).to_vec();
    p.play_from(0);
    p.run_for(20_000);
    let skipped = p.app.log.iter().filter(|l| l.ends_with("will not play: skipped")).count();
    assert_eq!(skipped, 4, "{:?}", p.app.log);
    assert!(p.changes.iter().any(|c| c.1 == 6), "the last song played: {:?}", p.changes);
    assert_eq!(p.sink.heard_frames as usize, frames(9.0), "s0, s2 and s6");
}

#[test]
fn repeat_all_and_one_gapless() {
    let s = songs(2, 3.0);
    let mut p = Player::new(queue(&s));
    p.set_repeat(REPEAT_ALL);
    p.play_from(0);
    p.run_for(14_000);
    let heard = p.sink.heard_samples();
    let round: Vec<i16> = s.concat();
    assert!(heard[..round.len() * 2] == [round.clone(), round.clone()].concat()[..], "a, b, a, b, sample for sample");
    let mut p = Player::new(queue(&s));
    p.set_repeat(REPEAT_ONE);
    p.play_from(1);
    p.run_for(8_000);
    let heard = p.sink.heard_samples();
    assert!(heard[..s[1].len() * 2] == [s[1].clone(), s[1].clone()].concat()[..], "b, b, sample for sample");
    assert!(p.sink.gaps.is_empty() && p.sink.timestamp_jumps == 0);
}

#[test]
fn shuffle_plays_each_song_once() {
    let s = songs(6, 2.0);
    let mut p = Player::shuffled(queue(&s), 42);
    let order: Vec<usize> = p.queue.live.play_order().collect();
    // The same seed is the same order, on every run and every platform.
    assert_eq!(order, vec![2, 5, 1, 4, 3, 0]);
    p.play_from(order[0]);
    assert!(p.run_to_end(30_000));
    assert_eq!(p.changes.iter().map(|c| c.1).collect::<Vec<_>>(), order, "played in the shuffled order");
    let heard = p.sink.heard_samples();
    let joined: Vec<i16> = order.iter().flat_map(|&i| s[i].iter().copied()).collect();
    assert!(heard == joined, "each song once, whole, back to back");
}

#[test]
fn edits_near_current() {
    // An edit while the playing song is opened again to remake its ending: the remake follows the song.
    let s = songs(4, 12.0);
    let mut p = Player::new(queue(&s));
    p.tracks.slow.push("s1".into());
    p.play_from(1);
    // The reader is into s2: s1's ending was made gapless into it.
    assert!(p.run_until(10_000, |p| p.app.log.iter().any(|l| l.contains("sink: s2"))), "{:?}", p.app.log);
    let extra = music(2.0, 200);
    p.tracks.push(track("x", &extra));
    p.tracks.push(track("y", &extra));
    p.queue.live.add(vec!["x".into()], Hand::Next);
    p.queue_changed();
    p.replan_ending();
    assert!(p.app.logged("the ending of s1 is made again"), "{:?}", p.app.log);
    p.queue.live.insert(0, vec!["y".into()], Hand::No);
    p.queue_changed();
    assert!(p.run_to_end(60_000));
    let heard = p.sink.heard_samples();
    let joined: Vec<i16> = [&s[1], &extra, &s[2], &s[3]].iter().flat_map(|v| v.iter().copied()).collect();
    // Where x replaces the s2 already written, it is blended in.
    let (cut, blend) = (s[1].len(), frames(nori_player::pipeline::BLEND_US as f64 / 1e6) * 2);
    assert_eq!(heard.len(), joined.len());
    assert!(heard[..cut] == joined[..cut] && heard[cut + blend..] == joined[cut + blend..], "s1 whole, then the song added to play next, then the rest");

    // Edit before current keeps player on it.
    let s = songs(4, 12.0);
    let mut p = Player::new(queue(&s));
    p.play_from(1);
    // A second in: the song after it is not being read yet (that starts ten seconds before the end).
    p.run_for(1_500);
    let extra = music(2.0, 200);
    p.tracks.push(track("x", &extra));
    // One song before the one playing, and one straight after it: every index the player holds moves.
    p.queue.live.insert(0, vec!["y".into()], Hand::No);
    p.tracks.push(track("y", &extra));
    p.queue.live.add(vec!["x".into()], Hand::Next);
    p.queue_changed();
    assert_eq!(p.current_id().as_deref(), Some("s1"), "still on the song playing");
    assert!(p.run_to_end(60_000));
    let heard = p.sink.heard_samples();
    let joined: Vec<i16> = [&s[1], &extra, &s[2], &s[3]].iter().flat_map(|v| v.iter().copied()).collect();
    assert!(heard == joined, "the song playing whole, then the one added to play next, then the rest");
}

#[test]
fn shuffle_keeps_current_and_next() {
    let s = songs(6, 2.0);
    let mut p = Player::new(queue(&s));
    p.play_from(2);
    p.run_for(500);
    let extra = music(2.0, 200);
    p.tracks.push(track("x", &extra));
    p.queue.live.add(vec!["x".into()], Hand::Next);
    p.queue.live.set_shuffle(true, 7);
    p.queue_changed();
    let order: Vec<&str> = p.queue.live.play_order().map(|i| p.queue.live.ids()[i].as_str()).collect();
    assert_eq!(&order[..2], &["s2", "x"], "{order:?}");
    assert!(p.next());
    assert_eq!(p.current_id().as_deref(), Some("x"), "play next is next, shuffle or not");
}

#[test]
fn queue_emptied_while_playing() {
    // Another server's profile taken up empties the queue under the song playing: nothing indexes it.
    let s = songs(3, 12.0);
    let mut p = Player::new(queue(&s));
    p.play_from(0);
    p.run_for(1_500);
    p.queue.live.set(Vec::new(), None, false, 0);
    p.queue_changed();
    p.run_for(2_000);
    assert_eq!(p.current_id(), None, "no song of an empty queue");
}
