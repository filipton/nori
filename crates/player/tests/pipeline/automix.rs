//! AutoMix end to end: measuring ahead, planning from analyses, and beat-matched stretched mixes.

use nori_player::automix::synth::Synth;
use nori_player::automix::ANALYSIS_VERSION;
use nori_player::sim::{prefs_off, Audio, Player, Track};
use nori_player::transitions::TransitionPrefs;
use nori_player::types::TrackAnalysis;

/// Thirty-two seconds of drums at `bpm` (a song needs thirty to be kept as measured), in stereo.
fn song(id: &str, bpm: f64) -> Track {
    let mut s = Synth::new(bpm);
    // At the analyser's own rate, so measuring costs no resampling.
    s.rate = 22_050;
    s.secs = 32.0;
    s.noise = 0.01;
    let stereo: Vec<i16> = s.render().iter().flat_map(|v| [(v * 0.7 * 32767.0).round() as i16; 2]).collect();
    Track::new(id, Audio::pcm(22_050, 2, &stereo))
}

/// What measuring one of [`song`]'s songs finds: its tempo on a steady grid from the first beat, music
/// from end to end.
fn measured(id: &str, bpm: f64) -> TrackAnalysis {
    TrackAnalysis {
        song_id: id.into(),
        analysis_version: ANALYSIS_VERSION,
        duration_ms: 32_000,
        bpm,
        bpm_confidence: 1.0,
        beat_offset_ms: 250.0,
        stability: 1.0,
        downbeat_confidence: 1.0,
        lufs: -14.0,
        silence_end_ms: 32_000,
        mixramp_end_ms: 32_000,
        intro_end_ms: 250,
        outro_start_ms: 16_250,
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
    // Echo-out takes the place of a beat-matched mix where two voices would clash; these tests are
    // about the beat-matched one.
    TransitionPrefs { auto_mix: true, auto_mix_max_s: 16, echo_out: false, ..prefs_off() }
}

#[test]
fn enabling_measures_and_matches() {
    let mut p = Player::new(vec![song("a", 120.0), song("b", 123.0), song("c", 120.0)]);
    p.play_from(0);
    p.run_for(2_000);
    assert!(p.app.logged("planFor: off"), "{:?}", p.app.log);
    p.set_prefs(automix());
    assert!(p.app.logged("measuring ahead: 3 of 3 unmeasured, 0 not on the device yet"), "measuring starts: {:?}", p.app.log);
    for (id, bpm) in [("a", 120.0), ("b", 123.0), ("c", 120.0)] {
        let a = p.app.analyses.get(id).unwrap_or_else(|| panic!("{id} measured: {:?}", p.app.log));
        assert!((a.bpm - bpm).abs() < 0.5, "{id}: {} bpm", a.bpm);
    }
    // Planned from the measurements: bar-aligned, and b slowed to a's tempo for the overlap.
    assert!(p.run_until(12_000, |p| p.app.logged("transition a -> b: BeatMatched")), "{:?}", p.app.log);
    let plan = p.app.log.iter().find(|l| l.contains("transition a -> b")).unwrap().clone();
    assert!(plan.contains("tempo x0.976"), "{plan}");
    assert!(p.run_until(70_000, |p| p.mixing()), "the mix is heard");
    assert!(p.app.logged("mixing: the next track arrived"), "{:?}", p.app.log);
    assert!(p.run_until(30_000, |p| p.current() == Some(1)), "b plays out of the mix");
    // The clock runs in b's own time: slowed to a's tempo through the mix and eased back to its own
    // after (a 2.4 % change eases back over 32 beats, most of what is left of b), so five seconds heard
    // are between 4.88 and 5 of b's - never five seconds of the output's own, which fell behind the music
    // and leapt where b's timestamps came back. It runs on into the next mix, out of b into c.
    p.run_for(9_000);
    let a = p.position_ms();
    p.run_for(5_000);
    let moved = p.position_ms() - a;
    assert!((4_878 - 20..=5_020).contains(&moved) && moved < 4_990, "{moved} ms of b in 5 s");
    assert!(p.run_to_end(120_000), "the queue plays to its end");
    assert_eq!(p.app.log.iter().filter(|l| l.contains("mixing: the next track arrived")).count(), 2, "{:?}", p.app.log);
    assert!(!p.app.logged("letting the ending play"), "{:?}", p.app.log);
    assert!(p.sink.gaps.is_empty(), "no hole through a stretched mix: {:?}", p.sink.gaps);
    assert_eq!(p.sink.timestamp_jumps, 0, "the stretch's own clock never reads as a jump");
}

#[test]
fn stretched_mix_rate_converted() {
    let mut b = Synth::new(123.0);
    (b.rate, b.secs, b.noise) = (24_000, 32.0, 0.01);
    let b: Vec<i16> = b.render().iter().flat_map(|v| [(v * 0.7 * 32767.0).round() as i16; 2]).collect();
    let mut p = Player::new(vec![song("a", 120.0), Track::new("b", Audio::pcm(24_000, 2, &b)), song("c", 120.0)]);
    // Measured as the synth made them, so this test spends its time on the mix and not the analysis.
    p.measure_on_move = false;
    for (id, bpm) in [("a", 120.0), ("b", 123.0), ("c", 120.0)] {
        p.app.analyses.insert(id.into(), measured(id, bpm));
    }
    p.play_from(0);
    p.set_prefs(automix());
    assert!(p.run_until(12_000, |p| p.app.logged("transition a -> b: BeatMatched")), "{:?}", p.app.log);
    assert!(p.run_to_end(120_000), "{:?}", p.app.log);
    assert!(p.app.logged("converting 24000 Hz x2 -> 22050 Hz x2"), "{:?}", p.app.log);
    assert_eq!(p.app.log.iter().filter(|l| l.contains("mixing: the next track arrived")).count(), 2, "{:?}", p.app.log);
    assert_eq!(p.sink.rebuilds, 0);
    assert!(p.sink.gaps.is_empty(), "{:?}", p.sink.gaps);
    assert_eq!(p.sink.timestamp_jumps, 0);
}

#[test]
fn analysis_kept_only_whole() {
    let mut p = Player::with_prefs(vec![song("a", 118.0), song("b", 118.0)], automix());
    p.app.measure_playing = true;
    p.measure_on_move = false;
    p.play_from(0);
    assert!(p.run_until(70_000, |p| p.app.analyses.contains_key("a")), "{:?}", p.app.log);
    assert!((p.app.analyses["a"].bpm - 118.0).abs() < 0.5, "{}", p.app.analyses["a"].bpm);
    // A song cut short by a seek is not an analysis of the song. The seek lands a couple of seconds
    // before the mix out of a, which starts at 20.6 s: the synth stops part way through a bar, an
    // ending the planner leaves before (`exit_ms`).
    p.run_for(3_000);
    p.seek(18_000);
    // Thirty seconds on, well into b: had a partial hearing been kept, it would be there by now.
    p.run_for(30_000);
    assert_eq!(p.current_id().as_deref(), Some("b"), "b plays: {:?}", p.app.log);
    assert!(!p.app.analyses.contains_key("b"), "b was heard in part only: {:?}", p.app.log);
}

#[test]
fn unmeasured_fade_bar_waits() {
    // The first boundary on a phone that has never heard these songs: they were still on their way
    // when measuring ahead looked, so nothing is measured. The planner fades blind, and the page must
    // not put the next title up while the last song plays on at the start of that fade.
    let prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: 12, ..prefs_off() };
    let mut p = Player::with_prefs(vec![song("a", 120.0), song("b", 96.0), song("c", 120.0)], prefs);
    p.measure_on_move = false;
    p.play_from(0);
    assert!(p.run_until(5_000, |p| p.app.logged("transition a -> b: EqualPowerFade 10666 ms at 21334")), "{:?}", p.app.log);
    assert!(p.app.logged("not analysed"), "{:?}", p.app.log);
    assert!(p.app.analyses.is_empty(), "nothing was measured: {:?}", p.app.log);
    assert!(p.run_until(30_000, |p| p.mixing()), "the fade is heard: {:?}", p.app.log);
    // An equal-power fade as long as the songs allow (a third of one): b is the louder from its middle
    // on, and not before.
    let mut on_a = 0;
    // The song the page shows: the one the ear is on, else the player's own.
    let shown = |p: &mut Player| p.bar().index.or(p.current());
    while shown(&mut p) == Some(0) {
        p.run_for(100);
        on_a += 100;
        assert!(on_a <= 6_500, "a handed over by the middle of the fade");
    }
    assert!((5_200..=5_500).contains(&on_a), "a is shown until the fade's middle: {on_a} ms");
    assert_eq!(shown(&mut p), Some(1));
    let seen = p.bar();
    assert!((5_200..5_700).contains(&seen.ms), "b is entered where it is heard, halfway through the fade: {}", seen.ms);
    // Once the whole fade has been heard nothing is left of it: a player that wakes while a mix is
    // named would otherwise wake four times a second until the next ending.
    assert!(p.run_until(12_000, |p| !p.mixing()), "{:?}", p.app.log);
    p.run_for(1_000);
    assert!(p.heard().from.is_none() && p.heard().id.is_none(), "{:?}", p.heard());
    assert!(p.run_to_end(120_000), "the queue plays to its end: {:?}", p.app.log);
    assert_eq!(p.app.log.iter().filter(|l| l.contains("mixing: the next track arrived")).count(), 2, "{:?}", p.app.log);
    assert!(!p.app.logged("letting the ending play"), "{:?}", p.app.log);
    assert!(p.sink.gaps.is_empty(), "{:?}", p.sink.gaps);
}

/// The songs the page shows from the start of the queue to its end, sampled every `step_ms`, each change
/// once: a page that goes back to a song it has left shows up as that song twice.
fn shown_run(p: &mut Player, step_ms: i64) -> Vec<usize> {
    let mut run: Vec<usize> = Vec::new();
    let mut waited = 0;
    while !p.ended() && waited < 200_000 {
        if let Some(i) = p.bar().index.or(p.current()) {
            if run.last() != Some(&i) {
                run.push(i);
            }
        }
        p.run_for(step_ms);
        waited += step_ms;
    }
    run
}

#[test]
fn page_changes_once_per_song() {
    // The engine lets the mix go once the whole of it has been heard, and the player moves on to the
    // next song at about the same moment. Whichever comes first, the page must not go back to the song
    // it has left for the moment in between: that is the old cover flashing up after the new one.
    for (measured_ahead, max_s) in [(true, 16), (false, 12)] {
        let prefs = TransitionPrefs { auto_mix: true, auto_mix_max_s: max_s, echo_out: false, ..prefs_off() };
        let mut p = Player::with_prefs(vec![song("a", 120.0), song("b", 120.0), song("c", 120.0)], prefs);
        p.measure_on_move = false;
        if measured_ahead {
            for id in ["a", "b", "c"] {
                p.app.analyses.insert(id.into(), measured(id, 120.0));
            }
        }
        p.play_from(0);
        let run = shown_run(&mut p, 5);
        assert_eq!(run, vec![0, 1, 2], "measured ahead {measured_ahead}: {:?}", p.app.log);
    }
}

