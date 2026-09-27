//! Which songs are mixed into which, and under what settings: everything between "the output reached
//! the ending of a song" and the planner (`automix::plan`). The platform describes the window of songs
//! the player will play and the user's preferences; this decides whether there is a transition at all,
//! picks the pair, and turns the planner's answer into what the engine runs.

use crate::automix::mixer;
use crate::engine::Plan;
use crate::types::{AutoMixSettings, TransitionKind, TransitionPlan};

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
}

/// Two songs follow on the same album, in order: `b` is played right after `a` (the caller's window is in
/// play order), both are of the same album, and the queue is not shuffled, which puts songs next to each
/// other by chance. Such songs are never mixed into each other.
///
/// The track and disc numbers do not have to count on by one. Real libraries leave numbers out (a song with
/// no track number, one tagged by another tool without a disc number), number a double album on across its
/// discs, or miss a file (1, 2, 4); read as "not in order", each such album was mixed song into song with
/// AutoMix, every next song coming in part-way where the mix entered it, with "keep albums gapless" on. The
/// one thing the numbers still say is going back: where both are known, a lower disc, or a lower track on
/// the same disc, is a song picked out of the album on its own (track 5 queued, then track 2), not the album
/// played on, and it mixes. A song with a disc number and one without have no place on the album to
/// compare: Navidrome lists a song with no disc number before its album's others, so the album's own page
/// plays track 2 (no disc) and then track 1 (disc 1), and that is the album played on.
pub fn follows_on_album(a: &WindowSong, b: &WindowSong, shuffling: bool) -> bool {
    if shuffling || a.album_id.is_none() || a.album_id != b.album_id {
        return false;
    }
    !goes_back(a, b)
}

/// `b` comes before `a` on their album, as far as the numbers both have say.
fn goes_back(a: &WindowSong, b: &WindowSong) -> bool {
    match (a.disc > 0, b.disc > 0) {
        (true, true) if b.disc != a.disc => b.disc < a.disc,
        (true, false) | (false, true) => false,
        _ => a.track > 0 && b.track > 0 && b.track < a.track,
    }
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
        // A track missing from the queue (or the library) between them: still the album played on.
        let gap = [song("a", Some("x"), 2), song("b", Some("x"), 4)];
        assert!(pick(&prefs(), false, &gap, "a", false).unwrap().settings.same_album_in_order, "a track left out");
        // Going back is a song picked out of the album on its own.
        let back_track = [song("a", Some("x"), 5), song("b", Some("x"), 2)];
        assert!(!pick(&prefs(), false, &back_track, "a", false).unwrap().settings.same_album_in_order, "back to an earlier track");
        let disc = |id: &str, disc: i32, track: i32| WindowSong { disc, ..song(id, Some("x"), track) };
        let turn = [disc("a", 1, 12), disc("b", 2, 1)];
        assert!(pick(&prefs(), false, &turn, "a", false).unwrap().settings.same_album_in_order, "on to the next disc");
        let back = [disc("a", 2, 1), disc("b", 1, 2)];
        assert!(!pick(&prefs(), false, &back, "a", false).unwrap().settings.same_album_in_order, "back to the disc before");
        let past = [disc("a", 1, 12), disc("b", 2, 2)];
        assert!(pick(&prefs(), false, &past, "a", false).unwrap().settings.same_album_in_order, "the next disc's first track left out");
        let on = [disc("a", 1, 10), disc("b", 2, 11)];
        assert!(pick(&prefs(), false, &on, "a", false).unwrap().settings.same_album_in_order, "numbered on across the discs");
        assert!(in_album_run(None, &w[0], Some(&w[1]), false) && in_album_run(Some(&w[0]), &w[1], None, false));
        assert!(!in_album_run(None, &song("c", None, 1), None, false));
    }

    #[test]
    fn an_album_is_in_order_as_the_queue_plays_it_whatever_its_numbers_leave_out() {
        let in_order = |a: WindowSong, b: WindowSong| pick(&prefs(), false, &[a, b], "a", false).unwrap().settings.same_album_in_order;
        let tagged = |id: &str, disc: i32, track: i32| WindowSong { disc, ..song(id, Some("x"), track) };
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
    fn a_plain_crossfade_uses_none_of_automix_extras() {
        let w = [song("a", None, 1), song("b", None, 2)];
        let s = pick(&TransitionPrefs { auto_mix: false, crossfade_s: 5, ..prefs() }, false, &w, "a", false).unwrap().settings;
        assert_eq!(s.max_transition_s, 5.0);
        assert!(!s.beat_match && !s.bass_swap && !s.filter_effects && !s.echo_out && !s.match_loudness);
        let s = pick(&TransitionPrefs { replay_gain: true, ..prefs() }, false, &w, "a", false).unwrap().settings;
        assert!(!s.match_loudness, "replaygain already levels them");
    }

    #[test]
    fn only_a_whole_song_is_an_analysis_of_it() {
        assert!(whole_song(241_000, 240_000) && whole_song(10_000, 0));
        assert!(!whole_song(90_000, 240_000));
    }
}
