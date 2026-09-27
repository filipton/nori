//! ReplayGain through a mix: each song is heard at its own volume the whole way, so a crossfade or an
//! AutoMix between a quiet song and a loud one sounds exactly as the two songs turned to their volumes
//! first and mixed after - no step anywhere, the end of the mix included - and the end of a mix joins
//! the song that plays on without a click.

use nori_player::automix::ANALYSIS_VERSION;
use nori_player::sim::{prefs_off, Audio, Player, Sound, Track};
use nori_player::transitions::TransitionPrefs;
use nori_player::types::TrackAnalysis;

use crate::common::*;

const SONG_S: f64 = 45.0;

/// −6 dB, as ReplayGain works it out.
fn minus_6db() -> f32 {
    10f32.powf(-6.0 / 20.0)
}

/// `s` at volume `gain`, rounded to 16 bits as the player rounds it.
fn at(s: &[i16], gain: f32) -> Vec<i16> {
    s.iter().map(|&v| (v as f32 * gain).round() as i16).collect()
}

/// A song measured as steady music at `bpm` from end to end, so AutoMix beat-matches into and out of it.
fn measured(id: &str, bpm: f64) -> TrackAnalysis {
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
        lufs: -14.0,
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

fn automix() -> TransitionPrefs {
    TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, echo_out: false, ..prefs_off() }
}

/// What the player hears of `songs` under `prefs`, each at the volume `gains` gives it (1 unlisted),
/// with AutoMix's measurements of them known up front.
fn heard(songs: &[(&str, &[i16], f64)], prefs: &TransitionPrefs, gains: &[(&str, f32)]) -> (Vec<i16>, Vec<String>) {
    let mut p = Player::with_prefs(songs.iter().map(|(id, s, _)| track(id, s)).collect(), prefs.clone());
    p.measure_on_move = false;
    for (id, _, bpm) in songs {
        p.app.analyses.insert(id.to_string(), measured(id, *bpm));
    }
    for (id, g) in gains {
        p.app.gains.insert(id.to_string(), *g);
    }
    p.play_from(0);
    assert!(p.run_to_end(200_000), "{:?}", p.app.log);
    assert!(p.sink.gaps.is_empty(), "{:?}", p.sink.gaps);
    (p.sink.heard_samples(), p.app.log.clone())
}

/// The first sample where `heard` differs from `ideal`, and by how much the level is off there.
fn first_off(heard: &[i16], ideal: &[i16]) -> Option<(usize, i32)> {
    heard.iter().zip(ideal).position(|(h, i)| h != i).map(|k| (k / 2, (heard[k] as i32 - ideal[k] as i32).abs()))
}

fn check_gain_then_mix(prefs: TransitionPrefs, what: &str) {
    let (a, b) = (music(SONG_S, 21), music(SONG_S, 22));
    let g = minus_6db();
    let quiet = at(&a, g);
    // The ideal: the songs turned to their volumes first, then played with no ReplayGain at all.
    let (ideal, log) = heard(&[("a", &quiet, 120.0), ("b", &b, 123.0)], &prefs, &[]);
    let (got, _) = heard(&[("a", &a, 120.0), ("b", &b, 123.0)], &prefs, &[("a", g), ("b", 1.0)]);
    assert!(log.iter().any(|l| l.contains("mixing: the next track arrived")), "{what}: a mix was heard: {log:?}");
    assert_eq!(got.len(), ideal.len(), "{what}: the volumes leave the timing alone");
    assert_eq!(first_off(&got, &ideal), None, "{what}: every sample as the gain-then-mix one, the end of the mix included");
    // And so the level never steps: the largest sample-to-sample move is the ideal's own.
    assert_eq!(max_step(&left(&got)), max_step(&left(&ideal)), "{what}");
}

#[test]
fn a_crossfade_mixes_each_song_at_its_own_replay_gain() {
    check_gain_then_mix(crossfade(6), "crossfade");
}

#[test]
fn automix_mixes_each_song_at_its_own_replay_gain() {
    check_gain_then_mix(automix(), "automix");
}

/// The largest sample-to-sample step in `x` within `ms` either side of frame `at`.
fn step_near(x: &[f64], at: usize, ms: usize) -> f64 {
    let w = ms * RATE as usize / 1000;
    max_step(&x[at.saturating_sub(w)..(at + w).min(x.len())])
}

#[test]
fn a_crossfade_ends_without_a_click() {
    let (a, b) = (music(SONG_S, 23), music(SONG_S, 24));
    let (got, log) = heard(&[("a", &a, 120.0), ("b", &b, 120.0)], &crossfade(6), &[]);
    assert!(log.iter().any(|l| l.contains("transition a -> b: EqualPowerFade 6000 ms at 39000")), "{log:?}");
    let x = left(&got);
    let end = frames(39.0 + 6.0);
    // The songs' own largest step where they meet: the end of a's hold and b six seconds in.
    let own = step_near(&left(&a), end, 100).max(step_near(&left(&b), frames(6.0), 100));
    let step = step_near(&x, end, 20);
    assert!(step <= 2.0 * own, "the end of the mix steps {step:.4} against the songs' own {own:.4}");
}

#[test]
fn an_automix_ends_and_its_stretch_hands_back_without_a_click() {
    let (a, b) = (music(SONG_S, 25), music(SONG_S, 26));
    let (got, log) = heard(&[("a", &a, 120.0), ("b", &b, 123.0)], &automix(), &[]);
    let plan = log.iter().find(|l| l.contains("transition a -> b: BeatMatched")).unwrap_or_else(|| panic!("{log:?}")).clone();
    assert!(plan.contains("tempo x0.976"), "{plan}");
    let x = left(&got);
    // The songs' own largest step anywhere: steady music, so the same all the way through.
    let own = max_step(&left(&a)).max(max_step(&left(&b)));
    let step = max_step(&x);
    let worst = x.windows(2).position(|w| (w[1] - w[0]).abs() == step).unwrap_or(0);
    assert!(step <= 2.0 * own, "a step of {step:.4} at {:.3} s against the songs' own {own:.4}: {plan}", worst as f64 / RATE as f64);
}

// ---- a song turned up ----

/// The limiter's look-ahead at [`RATE`], frames: everything through it comes out this much later.
const LOOKAHEAD: usize = 220;

/// `song` as floats, the way a decoder asked for floats hands it out.
fn floats(song: &[i16]) -> Vec<f32> {
    song.iter().map(|&v| v as f32 / 32768.0).collect()
}

fn float_track(id: &str, song: &[i16]) -> Track {
    Track::new(id, Audio::pcm_float(RATE, 2, &floats(song)))
}

/// What the ear hears of `song` (floats or 16 bits) played at `gain`, songs allowed up to `max`, the
/// limiter on as nori-engine puts it behind songs that may be turned up.
fn turned_up(song: Track, gain: f32, max: f32) -> Player {
    let mut p = Player::new(vec![song]);
    p.app.gains.insert("a".into(), gain);
    p.gain_max = max;
    p.set_sound(Sound { limiter: true, ..Default::default() });
    p.play_from(0);
    assert!(p.run_to_end(60_000), "{:?}", p.app.log);
    assert!(p.sink.gaps.is_empty(), "{:?}", p.sink.gaps);
    p
}

#[test]
fn a_song_turned_up_is_louder_and_the_limiter_holds_the_ceiling() {
    // Music peaking at 0.45 of full scale, turned up 9 dB: peaks at 1.27, over full scale.
    let song = music(20.0, 41);
    let x = floats(&song);
    let gain = 10f32.powf(9.0 / 20.0);
    let p = turned_up(float_track("a", &song), gain, gain);
    let heard = p.sink.heard_floats();
    assert_eq!(heard.len(), x.len() + LOOKAHEAD * 2, "the look-ahead delays the song, and brings out its end");
    let peak = heard.iter().fold(0f32, |m, v| m.max(v.abs()));
    let ceiling = 10f32.powf(-1.0 / 20.0);
    assert!(peak <= ceiling * (1.0 + 1e-5), "nothing past the -1 dB ceiling: {peak}");
    assert!(p.sink.gain_reduction_db > 0.0, "the limiter worked");
    // Where the song is quiet enough, every sample is it turned up 9 dB: the limiter's knee (from -3 dB)
    // and its release after a peak left out.
    let knee = 10f32.powf(-3.0 / 20.0);
    let second = RATE as usize * 2;
    let (mut same, mut quiet) = (0, 0);
    for (k, (&y, &v)) in heard[LOOKAHEAD * 2..].iter().zip(&x).enumerate() {
        let near = x[k.saturating_sub(second)..(k + second / 10).min(x.len())].iter().fold(0f32, |m, s| m.max(s.abs())) * gain;
        if near < knee {
            quiet += 1;
            same += ((y - v * gain).abs() < 1e-6) as usize;
        }
    }
    assert!(quiet > x.len() / 10, "enough of the song is quiet to look at: {quiet}");
    assert_eq!(same, quiet, "the quiet parts at exactly +9 dB");
    // And the song as a whole is up by close to the 9 dB asked for.
    let rms = |s: &[f32]| (s.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
    let up = 20.0 * (rms(&heard) / rms(&x)).log10();
    assert!(up > 8.0 && up < 9.1, "{up:.2} dB louder");
}

#[test]
fn a_song_in_16_bits_is_never_turned_up_where_it_would_clip() {
    // 16-bit samples cannot go over full scale on their way to the limiter: the player leaves such a song
    // at its own level (nori-engine reads songs as floats whenever they may be turned up).
    let song = music(10.0, 42);
    let p = turned_up(track("a", &song), 2.0, 2.0);
    let heard = p.sink.heard_samples();
    assert!(heard[LOOKAHEAD * 2..] == song[..heard.len() - LOOKAHEAD * 2], "every sample as it is");
    // Songs may not be turned up at all: a float song asked at 2 plays at 1.
    let p = turned_up(float_track("a", &song), 2.0, 1.0);
    let heard = p.sink.heard_floats();
    assert!(heard[LOOKAHEAD * 2..].iter().zip(floats(&song)).all(|(h, s)| *h == s), "held to the most allowed");
}
