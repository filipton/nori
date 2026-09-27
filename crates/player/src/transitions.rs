//! Which songs are mixed into which, and under what settings: everything between "the output reached
//! the ending of a song" and the planner (`automix::plan`). The platform describes the window of songs
//! the player will play and the user's preferences; this decides whether there is a transition at all,
//! picks the pair, and turns the planner's answer into what the engine runs.

use crate::automix::mixer;
use crate::engine::Plan;
use crate::types::{AutoMixSettings, FadeCurve, TransitionKind, TransitionPlan};

/// A song in the window, as far as transitions care.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowSong {
    pub id: String,
    /// For the log only.
    pub title: String,
    pub duration_ms: i64,
    pub album_id: Option<String>,
    pub disc: i32,
    pub track: i32,
    /// The server's BPM tag, 0 when there is none.
    pub tag_bpm: f32,
    /// A radio stream: no ending to mix out of, no beginning to mix into.
    pub radio: bool,
}

/// The user's transition settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionPrefs {
    pub auto_mix: bool,
    /// A plain crossfade this long when AutoMix is off; 0 for none.
    pub crossfade_s: i32,
    /// AutoMix's longest transition.
    pub auto_mix_max_s: i32,
    pub beat_match: bool,
    pub max_tempo_change_pct: f32,
    pub bass_swap: bool,
    pub filter_effects: bool,
    pub echo_out: bool,
    pub keep_pitch: bool,
    /// Consecutive songs of an album played in order stay gapless.
    pub keep_albums: bool,
    /// ReplayGain levels songs already, so AutoMix does not trim them to each other.
    pub replay_gain: bool,
    /// The plain crossfade's curve (AutoMix picks its own).
    pub fade_curve: FadeCurve,
    /// How long the incoming song takes to come up, and the outgoing one to go, in a plain crossfade,
    /// ms; 0 is the whole crossfade. The incoming song's rise starts with the crossfade, the outgoing
    /// one's fall ends with it.
    pub fade_in_ms: i32,
    pub fade_out_ms: i32,
}

/// Two songs follow on the same album, in order: `b` is the track after `a` on the same disc, or the first
/// track of the next disc (a double album goes on from one disc to the next as it does within one). Not
/// while shuffling, which plays them next to each other by chance.
pub fn follows_on_album(a: &WindowSong, b: &WindowSong, shuffling: bool) -> bool {
    let next = (a.disc == b.disc && b.track == a.track + 1) || (b.disc == a.disc + 1 && b.track == 1);
    !shuffling && a.album_id.is_some() && a.album_id == b.album_id && next
}

/// `current` sits inside an album played in order: it follows the song before or leads into the one after.
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

/// Why there is no transition. Every reason is deliberate, and a boundary that passes without one is
/// otherwise indistinguishable from a broken feature, so each is logged - once, when it changes. Kept
/// as data, not text: it is asked again every couple of seconds while nothing is planned.
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

/// Whether the ending of `outgoing_id` is mixed into what follows it in `window` (the song played before
/// the current one first, then the current one and those after it, in play order), and with what
/// settings.
pub fn pick(prefs: &TransitionPrefs, transitions_off: bool, window: &[WindowSong], outgoing_id: &str, shuffling: bool) -> Result<Pick, Skip> {
    if transitions_off || (!prefs.auto_mix && prefs.crossfade_s == 0) {
        return Err(Skip::Off { transitions_off, auto_mix: prefs.auto_mix, crossfade_s: prefs.crossfade_s });
    }
    // The song before the current one is in the window on purpose: its ending can still be what flows
    // through the output after the player has moved on (decoding runs seconds ahead of the ear).
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
        // Loudness trim only when ReplayGain is off: otherwise the player's volume already levels songs.
        match_loudness: m && !prefs.replay_gain,
        out_tag_bpm: o.tag_bpm,
        in_tag_bpm: n.tag_bpm,
    };
    Ok(Pick { out, next, settings })
}

/// A plain crossfade (AutoMix off) shaped as the settings say: its curve, and how long each side takes
/// within it. AutoMix's transitions and gapless ones are left as the planner made them.
///
/// The curves, per side, `x` running 0 to 1 over its fade: linear `x` (the two add to full level in
/// amplitude, a 3 dB dip in the middle for two different songs), equal power `sin(x π/2)` (the two add to
/// full power: no dip, the smooth default, and what a plain crossfade has always used) and an S-curve
/// `sin²(x π/2)` (slow at both ends, quick through the middle, 6 dB down each at the centre).
pub fn shape_crossfade(prefs: &TransitionPrefs, t: &mut TransitionPlan) {
    if prefs.auto_mix || t.kind != TransitionKind::EqualPowerFade || t.duration_ms <= 0 {
        return;
    }
    let dur = t.duration_ms;
    let part = |ms: i32| if ms > 0 { (ms as i64).min(dur) } else { dur };
    t.fade_curve = prefs.fade_curve;
    (t.in_fade_start_ms, t.in_fade_end_ms) = (0, part(prefs.fade_in_ms));
    (t.out_fade_start_ms, t.out_fade_end_ms) = (dur - part(prefs.fade_out_ms), dur);
}

/// What the engine runs for the planner's answer; `None` for a gapless one (nothing to run).
pub fn engine_plan(p: &TransitionPlan, incoming_id: &str) -> Option<Plan> {
    if p.kind == TransitionKind::Gapless {
        return None;
    }
    Some(Plan {
        incoming_id: incoming_id.to_string(),
        out_start_us: p.out_start_ms * 1000,
        duration_us: p.duration_ms * 1000,
        in_skip_us: p.in_start_ms * 1000,
        mixer: mixer::params(p),
        tempo_ratio: p.tempo_ratio as f32,
        keep_pitch: p.keep_pitch,
        ramp_us: p.tempo_ramp_ms * 1000,
        out_loop_us: p.out_loop_ms.max(0) * 1000,
    })
}

/// An analysis that heard `heard_ms` of a song the server says is `expected_ms` long is of the whole
/// song. A measurement cut short (the queue moved, a read failed half way and looked like the end of
/// the file) must not be stored as the song: the planner would refuse it for not matching the song's
/// length, and its outro grid would have been measured somewhere in the middle.
pub fn whole_song(heard_ms: i64, expected_ms: i64) -> bool {
    expected_ms <= 0 || (heard_ms - expected_ms).abs() <= WHOLE_SLACK_MS
}

/// How far a decoded length may be from the server's and still be the same song (encoder padding,
/// a server rounding to seconds).
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
        WindowSong { id: id.into(), title: id.into(), duration_ms: 200_000, album_id: album.map(Into::into), disc: 1, track, tag_bpm: 0.0, radio: false }
    }

    #[test]
    fn nothing_is_planned_with_transitions_off() {
        let w = [song("a", None, 1), song("b", None, 2)];
        assert!(pick(&TransitionPrefs { auto_mix: false, ..prefs() }, false, &w, "a", false).is_err());
        assert!(pick(&prefs(), true, &w, "a", false).is_err(), "the output forbids touching samples");
        assert!(pick(&TransitionPrefs { auto_mix: false, crossfade_s: 4, ..prefs() }, false, &w, "a", false).is_ok());
    }

    #[test]
    fn the_pair_is_the_outgoing_song_and_the_one_after_it() {
        let w = [song("before", None, 1), song("a", None, 1), song("b", None, 1)];
        let p = pick(&prefs(), false, &w, "a", false).unwrap();
        assert_eq!((p.out, p.next), (1, 2));
        // The song before the current one: its ending may still be flowing after the player moved on.
        let p = pick(&prefs(), false, &w, "before", false).unwrap();
        assert_eq!((p.out, p.next), (0, 1));
        assert_eq!(pick(&prefs(), false, &w, "b", false).unwrap_err(), Skip::NothingAfter);
        assert_eq!(pick(&prefs(), false, &w, "x", false).unwrap_err(), Skip::NotInWindow);
    }

    #[test]
    fn radio_and_unknown_lengths_are_never_mixed() {
        let mut w = [song("a", None, 1), song("b", None, 2)];
        w[1].radio = true;
        assert_eq!(pick(&prefs(), false, &w, "a", false).unwrap_err(), Skip::Radio);
        w[1].radio = false;
        w[0].duration_ms = 0;
        assert_eq!(pick(&prefs(), false, &w, "a", false).unwrap_err(), Skip::Durations(0, 200_000));
    }

    #[test]
    fn an_album_in_order_stays_gapless_unless_shuffled() {
        let w = [song("a", Some("x"), 3), song("b", Some("x"), 4)];
        assert!(pick(&prefs(), false, &w, "a", false).unwrap().settings.same_album_in_order);
        assert!(!pick(&prefs(), false, &w, "a", true).unwrap().settings.same_album_in_order, "shuffled next to each other");
        assert!(!pick(&TransitionPrefs { keep_albums: false, ..prefs() }, false, &w, "a", false).unwrap().settings.same_album_in_order);
        let skip = [song("a", Some("x"), 3), song("b", Some("x"), 5)];
        assert!(!pick(&prefs(), false, &skip, "a", false).unwrap().settings.same_album_in_order, "a track skipped");
        let disc = |id: &str, disc: i32, track: i32| WindowSong { disc, ..song(id, Some("x"), track) };
        let turn = [disc("a", 1, 12), disc("b", 2, 1)];
        assert!(pick(&prefs(), false, &turn, "a", false).unwrap().settings.same_album_in_order, "on to the next disc");
        let back = [disc("a", 2, 1), disc("b", 1, 2)];
        assert!(!pick(&prefs(), false, &back, "a", false).unwrap().settings.same_album_in_order, "back to the disc before");
        let past = [disc("a", 1, 12), disc("b", 2, 2)];
        assert!(!pick(&prefs(), false, &past, "a", false).unwrap().settings.same_album_in_order, "the next disc's first track skipped");
        assert!(in_album_run(None, &w[0], Some(&w[1]), false) && in_album_run(Some(&w[0]), &w[1], None, false));
        assert!(!in_album_run(None, &song("c", None, 1), None, false));
    }

    #[test]
    fn a_plain_crossfade_uses_none_of_automix_extras() {
        let w = [song("a", None, 1), song("b", None, 2)];
        let s = pick(&TransitionPrefs { auto_mix: false, crossfade_s: 5, ..prefs() }, false, &w, "a", false).unwrap().settings;
        assert_eq!(s.max_transition_s, 5.0);
        assert!(!s.beat_match && !s.bass_swap && !s.filter_effects && !s.echo_out && !s.match_loudness);
        let s = pick(&TransitionPrefs { replay_gain: true, ..prefs() }, false, &w, "a", false).unwrap().settings;
        assert!(!s.match_loudness, "replaygain already levels them");
    }

    #[test]
    fn a_plain_crossfade_takes_the_curve_and_the_lengths_asked_for() {
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

    /// What the ear gets through the mixer for each curve: the level of two different songs (uncorrelated
    /// noise) at the middle of a crossfade, against either song alone.
    #[test]
    fn the_curves_sound_as_they_say() {
        use crate::automix::mixer::{params, Mixer};
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
            m.configure(&params(&t));
            let mut out = vec![0f32; n];
            unsafe { m.run(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), n, |v: f32| v as f64, |v| v as f32) };
            let mid = n / 2 - 2_400..n / 2 + 2_400;
            20.0 * (rms(&out[mid.clone()]) / rms(&a[mid])).log10()
        };
        let (equal, linear, s) = (middle(FadeCurve::EqualPower), middle(FadeCurve::Linear), middle(FadeCurve::SineSquared));
        assert!(equal.abs() < 0.5, "equal power keeps the level: {equal} dB");
        assert!((linear + 3.0).abs() < 0.6, "linear dips 3 dB: {linear}");
        assert!((s + 3.0).abs() < 0.6, "the S-curve too, at its centre: {s}");
    }

    #[test]
    fn only_a_whole_song_is_an_analysis_of_it() {
        assert!(whole_song(241_000, 240_000) && whole_song(10_000, 0));
        assert!(!whole_song(90_000, 240_000));
    }
}
