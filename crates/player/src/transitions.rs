//! Whether a song's ending gets a transition, with which next song and settings, and converting the
//! planner's answer (`automix::plan`) into the engine's [`Plan`].

use crate::engine::Plan;
use crate::types::{AutoMixSettings, FadeCurve, TransitionKind, TransitionPlan};

/// A song in the planner's window.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowSong {
    pub id: String,
    /// For the log only.
    pub title: String,
    pub duration_ms: i64,
    pub album_id: Option<String>,
    pub disc: i32,
    pub track: i32,
    /// Server BPM tag, 0 when none.
    pub tag_bpm: f32,
    /// A radio stream: never mixed.
    pub radio: bool,
    /// The queue's album run for this entry (`playlist::Playlist::album_run`): shared by the songs of an
    /// album queued whole; 0 otherwise.
    pub album_run: u32,
}

/// The user's transition settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionPrefs {
    pub auto_mix: bool,
    /// Plain crossfade length with AutoMix off; 0 for none.
    pub crossfade_s: i32,
    pub auto_mix_max_s: i32,
    pub beat_match: bool,
    pub max_tempo_change_pct: f32,
    pub bass_swap: bool,
    pub filter_effects: bool,
    pub echo_out: bool,
    pub keep_pitch: bool,
    /// An album queued whole stays gapless.
    pub keep_albums: bool,
    /// ReplayGain is on, so AutoMix does not match loudness.
    pub replay_gain: bool,
    /// Plain crossfade curve (AutoMix picks its own).
    pub fade_curve: FadeCurve,
    /// Plain crossfade: incoming rise (from the start) and outgoing fall (to the end), ms; 0 is the
    /// whole crossfade.
    pub fade_in_ms: i32,
    pub fade_out_ms: i32,
}

/// `b` plays right after `a` as part of an album queued whole: same non-zero album run, same album, not
/// shuffled, and not going back ([`goes_back`]). Such songs are never mixed.
///
/// Track and disc numbers need not be consecutive (real tags skip and omit them); only a known lower
/// disc or a lower track on the same disc means the album is not playing on.
pub fn follows_on_album(a: &WindowSong, b: &WindowSong, shuffling: bool) -> bool {
    if shuffling || a.album_run == 0 || a.album_run != b.album_run || a.album_id.is_none() || a.album_id != b.album_id {
        return false;
    }
    !goes_back(a, b)
}

/// `b` comes before `a` on their album, as far as both numbers are known. A song without a disc number
/// is not comparable with one that has it (Navidrome lists it first).
fn goes_back(a: &WindowSong, b: &WindowSong) -> bool {
    match (a.disc > 0, b.disc > 0) {
        (true, true) if b.disc != a.disc => b.disc < a.disc,
        (true, false) | (false, true) => false,
        _ => a.track > 0 && b.track > 0 && b.track < a.track,
    }
}

/// `current` is inside an album played in order ([`follows_on_album`] with a neighbour), for album gain
/// and gapless offload.
pub fn in_album_run(before: Option<&WindowSong>, current: &WindowSong, after: Option<&WindowSong>, shuffling: bool) -> bool {
    after.is_some_and(|n| follows_on_album(current, n, shuffling)) || before.is_some_and(|p| follows_on_album(p, current, shuffling))
}

/// The pair a transition would join, and the planner's settings for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub out: usize,
    pub next: usize,
    pub settings: AutoMixSettings,
}

/// Why there is no transition; logged when it changes. Data rather than text since it is re-asked often.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Off { transitions_off: bool, auto_mix: bool, crossfade_s: i32 },
    NotInWindow,
    NothingAfter,
    Radio,
    Durations(i64, i64),
}

impl Skip {
    pub fn describe(&self, outgoing_id: &str) -> String {
        match *self {
            Skip::Off { transitions_off, auto_mix, crossfade_s } => format!("off (transitionsOff={transitions_off} autoMix={auto_mix} crossfadeSec={crossfade_s})"),
            Skip::NotInWindow => format!("{outgoing_id} not in the upcoming window"),
            Skip::NothingAfter => format!("nothing after {outgoing_id}"),
            Skip::Radio => "radio".into(),
            Skip::Durations(a, b) => format!("durations {a}/{b}"),
        }
    }
}

/// The pair and planner settings for mixing out of `outgoing_id`, given the play-order `window`
/// (previous song first).
pub fn pick(prefs: &TransitionPrefs, transitions_off: bool, window: &[WindowSong], outgoing_id: &str, shuffling: bool) -> Result<Pick, Skip> {
    if transitions_off || (!prefs.auto_mix && prefs.crossfade_s == 0) {
        return Err(Skip::Off { transitions_off, auto_mix: prefs.auto_mix, crossfade_s: prefs.crossfade_s });
    }
    // The previous song is in the window because decoding runs ahead: its ending may still be flowing.
    let out = window.iter().position(|s| s.id == outgoing_id).ok_or(Skip::NotInWindow)?;
    let next = out + 1;
    let (o, n) = (&window[out], window.get(next).ok_or(Skip::NothingAfter)?);
    if o.radio || n.radio {
        return Err(Skip::Radio);
    }
    if o.duration_ms <= 0 || n.duration_ms <= 0 {
        return Err(Skip::Durations(o.duration_ms, n.duration_ms));
    }
    let m = prefs.auto_mix;
    let settings = AutoMixSettings {
        max_transition_s: (if m { prefs.auto_mix_max_s } else { prefs.crossfade_s }) as f32,
        beat_match: m && prefs.beat_match,
        max_tempo_change_pct: prefs.max_tempo_change_pct,
        bass_swap: m && prefs.bass_swap,
        filter_effects: m && prefs.filter_effects,
        echo_out: m && prefs.echo_out,
        keep_pitch: prefs.keep_pitch,
        same_album_in_order: prefs.keep_albums && follows_on_album(o, n, shuffling),
        match_loudness: m && !prefs.replay_gain,
        out_tag_bpm: o.tag_bpm,
        in_tag_bpm: n.tag_bpm,
    };
    Ok(Pick { out, next, settings })
}

/// Applies the user's curve and side lengths to a plain crossfade (AutoMix off); other plans are left
/// alone. Equal power keeps uncorrelated songs' level; linear and the S-curve dip ~3 dB mid-fade.
pub fn shape_crossfade(prefs: &TransitionPrefs, t: &mut TransitionPlan) {
    if prefs.auto_mix || t.kind != TransitionKind::EqualPowerFade || t.duration_ms <= 0 {
        return;
    }
    let dur = t.duration_ms;
    let part = |ms: i32| if ms > 0 { (ms as i64).min(dur) } else { dur };
    t.fade_curve = prefs.fade_curve;
    (t.in_fade_start_ms, t.in_fade_end_ms) = (0, part(prefs.fade_in_ms));
    (t.out_fade_start_ms, t.out_fade_end_ms) = (dur - part(prefs.fade_out_ms), dur);
    if t.fade_curve != FadeCurve::EqualPower || t.in_fade_end_ms != dur || t.out_fade_start_ms != 0 {
        t.reason = format!("{}; {:?} curve, in over {} ms, out over {} ms", t.reason, t.fade_curve, t.in_fade_end_ms, dur - t.out_fade_start_ms);
    }
}

/// The engine plan for a planner answer; `None` for gapless.
pub fn engine_plan(p: &TransitionPlan, incoming_id: &str) -> Option<Plan> {
    if p.kind == TransitionKind::Gapless {
        return None;
    }
    Some(Plan {
        incoming_id: incoming_id.to_string(),
        out_start_us: p.out_start_ms * 1000,
        duration_us: p.duration_ms * 1000,
        in_skip_us: p.in_start_ms * 1000,
        mixer: p.clone(),
        tempo_ratio: p.tempo_ratio as f32,
        keep_pitch: p.keep_pitch,
        ramp_us: p.tempo_ramp_ms * 1000,
        out_loop_us: p.out_loop_ms.unwrap_or(0) * 1000,
    })
}

/// An analysis of `heard_ms` covers the whole `expected_ms` song (0: unknown). A cut-short analysis must
/// not be stored.
pub fn whole_song(heard_ms: i64, expected_ms: i64) -> bool {
    expected_ms <= 0 || (heard_ms - expected_ms).abs() <= WHOLE_SLACK_MS
}

/// Allowed decoded-vs-server length difference (encoder padding, rounding).
const WHOLE_SLACK_MS: i64 = 3_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> TransitionPrefs {
        TransitionPrefs {
            auto_mix: true,
            crossfade_s: 0,
            auto_mix_max_s: 12,
            beat_match: true,
            max_tempo_change_pct: 6.0,
            bass_swap: true,
            filter_effects: true,
            echo_out: true,
            keep_pitch: true,
            keep_albums: true,
            replay_gain: false,
            fade_curve: FadeCurve::EqualPower,
            fade_in_ms: 0,
            fade_out_ms: 0,
        }
    }

    fn song(id: &str, album: Option<&str>, track: i32) -> WindowSong {
        WindowSong { id: id.into(), title: id.into(), duration_ms: 200_000, album_id: album.map(Into::into), disc: 1, track, tag_bpm: 0.0, radio: false, album_run: album.map_or(0, |_| 1) }
    }

    #[test]
    fn off_plans_nothing() {
        let w = [song("a", None, 1), song("b", None, 2)];
        assert!(pick(&TransitionPrefs { auto_mix: false, ..prefs() }, false, &w, "a", false).is_err());
        assert!(pick(&prefs(), true, &w, "a", false).is_err(), "the output forbids touching samples");
        assert!(pick(&TransitionPrefs { auto_mix: false, crossfade_s: 4, ..prefs() }, false, &w, "a", false).is_ok());
    }

    #[test]
    fn pair_is_outgoing_and_next() {
        let w = [song("before", None, 1), song("a", None, 1), song("b", None, 1)];
        let p = pick(&prefs(), false, &w, "a", false).unwrap();
        assert_eq!((p.out, p.next), (1, 2));
        let p = pick(&prefs(), false, &w, "before", false).unwrap();
        assert_eq!((p.out, p.next), (0, 1));
        assert_eq!(pick(&prefs(), false, &w, "b", false).unwrap_err(), Skip::NothingAfter);
        assert_eq!(pick(&prefs(), false, &w, "x", false).unwrap_err(), Skip::NotInWindow);
    }

    #[test]
    fn radio_and_unknown_lengths_not_mixed() {
        let mut w = [song("a", None, 1), song("b", None, 2)];
        w[1].radio = true;
        assert_eq!(pick(&prefs(), false, &w, "a", false).unwrap_err(), Skip::Radio);
        w[1].radio = false;
        w[0].duration_ms = 0;
        assert_eq!(pick(&prefs(), false, &w, "a", false).unwrap_err(), Skip::Durations(0, 200_000));
    }

    #[test]
    fn album_order_despite_missing_numbers() {
        let w = [song("a", Some("x"), 3), song("b", Some("x"), 4)];
        assert!(!pick(&TransitionPrefs { keep_albums: false, ..prefs() }, false, &w, "a", false).unwrap().settings.same_album_in_order);
        assert!(in_album_run(None, &w[0], Some(&w[1]), false) && in_album_run(Some(&w[0]), &w[1], None, false));
        assert!(!in_album_run(None, &song("c", None, 1), None, false));
        let in_order = |a: WindowSong, b: WindowSong| pick(&prefs(), false, &[a, b], "a", false).unwrap().settings.same_album_in_order;
        let tagged = |id: &str, disc: i32, track: i32| WindowSong { disc, ..song(id, Some("x"), track) };
        assert!(in_order(tagged("a", 1, 3), tagged("b", 1, 4)));
        assert!(in_order(tagged("a", 1, 2), tagged("b", 1, 4)), "a track left out");
        assert!(in_order(tagged("a", 1, 12), tagged("b", 2, 1)), "on to the next disc");
        assert!(in_order(tagged("a", 1, 12), tagged("b", 2, 2)), "the next disc's first track left out");
        assert!(in_order(tagged("a", 1, 10), tagged("b", 2, 11)), "numbered on across the discs");
        assert!(!in_order(tagged("a", 2, 1), tagged("b", 1, 2)), "back to the disc before");
        assert!(in_order(tagged("a", 1, 0), tagged("b", 1, 0)), "no track numbers at all");
        assert!(in_order(tagged("a", 1, 3), tagged("b", 1, 0)), "the next one's track number left out");
        assert!(in_order(tagged("a", 1, 0), tagged("b", 1, 7)), "this one's track number left out");
        assert!(in_order(tagged("a", 1, 3), tagged("b", 0, 4)), "the next one's disc left out");
        assert!(in_order(tagged("a", 0, 3), tagged("b", 0, 4)), "no disc numbers at all");
        assert!(in_order(tagged("a", 1, 0), tagged("b", 2, 0)), "on to the next disc, no track numbers");
        assert!(in_order(tagged("a", 1, 0), tagged("b", 3, 0)), "a disc left out");
        assert!(in_order(tagged("a", 1, 9), tagged("b", 0, 2)), "a lower track, one disc not known: nothing to compare");
        assert!(in_order(tagged("a", 0, 2), tagged("b", 1, 1)), "the song with no disc listed first, as Navidrome lists it");
        // Going back, where both numbers say so, still counts.
        assert!(!in_order(tagged("a", 2, 0), tagged("b", 1, 0)), "back to the disc before");
        assert!(!in_order(tagged("a", 1, 7), tagged("b", 1, 3)), "back to an earlier track");
        assert!(!in_order(tagged("a", 0, 7), tagged("b", 0, 3)), "back to an earlier track, no discs");
        assert!(in_order(tagged("a", 1, 7), tagged("b", 1, 7)), "the same number twice is not going back");
        assert!(!in_order(tagged("a", 1, 0), WindowSong { album_id: Some("y".into()), ..tagged("b", 1, 0) }), "another album");
        assert!(!pick(&prefs(), false, &[tagged("a", 1, 0), tagged("b", 1, 0)], "a", true).unwrap().settings.same_album_in_order, "shuffled");
    }

    #[test]
    fn only_album_runs_are_in_order() {
        let in_order = |a: WindowSong, b: WindowSong| pick(&prefs(), false, &[a, b], "a", false).unwrap().settings.same_album_in_order;
        let run = |id: &str, track: i32, run: u32| WindowSong { album_run: run, ..song(id, Some("x"), track) };
        assert!(in_order(run("a", 1, 7), run("b", 2, 7)), "one run: the album played from its page, or added whole");
        assert!(!in_order(run("a", 1, 0), run("b", 2, 0)), "two songs of the album queued one at a time, or by autofill");
        assert!(!in_order(run("a", 1, 7), run("b", 2, 0)), "a song of the album queued after the album");
        assert!(!in_order(run("a", 12, 7), run("b", 1, 8)), "the album added twice: its end and its start again");
        assert!(!in_order(run("a", 3, 0), run("a", 3, 0)), "the same song queued again");
        assert!(in_order(run("a", 3, 4), run("a", 3, 4)), "the same place: repeat one on an album's song");
        assert!(!in_album_run(Some(&run("p", 1, 0)), &run("c", 2, 0), Some(&run("n", 3, 0)), false), "ReplayGain's album gain likewise");
        assert!(in_album_run(Some(&run("p", 1, 2)), &run("c", 2, 2), Some(&run("n", 3, 0)), false));
    }

    #[test]
    fn plain_crossfade_has_no_automix_extras() {
        let w = [song("a", None, 1), song("b", None, 2)];
        let s = pick(&TransitionPrefs { auto_mix: false, crossfade_s: 5, ..prefs() }, false, &w, "a", false).unwrap().settings;
        assert_eq!(s.max_transition_s, 5.0);
        assert!(!s.beat_match && !s.bass_swap && !s.filter_effects && !s.echo_out && !s.match_loudness);
        let s = pick(&TransitionPrefs { replay_gain: true, ..prefs() }, false, &w, "a", false).unwrap().settings;
        assert!(!s.match_loudness, "replaygain already levels them");
    }

    #[test]
    fn plain_crossfade_takes_curve_and_lengths() {
        let plain = TransitionPrefs { auto_mix: false, crossfade_s: 6, ..prefs() };
        let blind = |s: &AutoMixSettings| crate::automix::plan::plan(None, None, 200_000, 200_000, s);
        let w = [song("a", None, 1), song("b", None, 2)];
        let s = pick(&plain, false, &w, "a", false).unwrap().settings;
        let mut t = blind(&s);
        let before = t.clone();
        shape_crossfade(&plain, &mut t);
        assert_eq!(t, before, "equal power over the whole crossfade: what a crossfade always was");
        let shaped = TransitionPrefs { fade_curve: FadeCurve::SineSquared, fade_in_ms: 2_000, fade_out_ms: 9_000, ..plain };
        shape_crossfade(&shaped, &mut t);
        assert_eq!(t.fade_curve, FadeCurve::SineSquared);
        assert_eq!((t.in_fade_start_ms, t.in_fade_end_ms), (0, 2_000), "in over its first two seconds");
        assert_eq!((t.out_fade_start_ms, t.out_fade_end_ms), (0, 6_000), "out over all of it: no longer than the crossfade");
        // AutoMix's own transitions, and gapless ones, are the planner's.
        let mut g = crate::automix::plan::plan(None, None, 200_000, 200_000, &AutoMixSettings { same_album_in_order: true, ..s });
        let gapless = g.clone();
        shape_crossfade(&shaped, &mut g);
        assert_eq!(g, gapless);
        let mut m = blind(&s);
        let automix = m.clone();
        shape_crossfade(&TransitionPrefs { auto_mix: true, ..shaped }, &mut m);
        assert_eq!(m, automix);
    }

    /// Mid-crossfade level of two uncorrelated signals relative to one alone, per curve.
    #[test]
    fn curve_levels_mid_fade() {
        use crate::automix::mixer::Mixer;
        let n = 48_000usize;
        let mut rng = crate::automix::synth::Rng(0x9E37_79B9_7F4A_7C15);
        let a: Vec<f32> = (0..n).map(|_| rng.next() as f32 * 0.25).collect();
        let b: Vec<f32> = (0..n).map(|_| rng.next() as f32 * 0.25).collect();
        let rms = |x: &[f32]| (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
        let middle = |curve: FadeCurve| {
            let plain = TransitionPrefs { auto_mix: false, crossfade_s: 1, fade_curve: curve, ..prefs() };
            let s = pick(&plain, false, &[song("a", None, 1), song("b", None, 2)], "a", false).unwrap().settings;
            let mut t = crate::automix::plan::plan(None, None, 200_000, 200_000, &s);
            shape_crossfade(&plain, &mut t);
            let mut m = Mixer::new(48_000, 1);
            m.configure(&t);
            let mut out = a.clone();
            m.process(&mut out, &b);
            let mid = n / 2 - 2_400..n / 2 + 2_400;
            20.0 * (rms(&out[mid.clone()]) / rms(&a[mid])).log10()
        };
        let (equal, linear, s) = (middle(FadeCurve::EqualPower), middle(FadeCurve::Linear), middle(FadeCurve::SineSquared));
        assert!(equal.abs() < 0.5, "equal power keeps the level: {equal} dB");
        assert!((linear + 3.0).abs() < 0.6, "linear dips 3 dB: {linear}");
        assert!((s + 3.0).abs() < 0.6, "the S-curve too, at its centre: {s}");
    }

    #[test]
    fn whole_song_tolerance() {
        assert!(whole_song(241_000, 240_000) && whole_song(10_000, 0));
        assert!(!whole_song(90_000, 240_000));
    }
}
