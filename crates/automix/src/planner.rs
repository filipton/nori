//! The transition planner as the audio path uses it: the window of songs coming up and the user's
//! settings are handed in when they change, and the engine asks for its plan, whether a song wants
//! measuring, and hands over a finished measurement - all here, without a call into Kotlin. The engine
//! asks again every couple of seconds while nothing is planned (a null is often momentary), so that
//! question costs a lookup and no allocation unless the answer changes.

use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use nori_model::alog;
use nori_player::automix::analysis::Analyzer;
use nori_player::engine::Plan;
use nori_player::transitions::{engine_plan, pick, whole_song, Skip, TransitionPrefs, WindowSong};
use parking_lot::Mutex;

use super::store::{get, missing, put};

struct Planner {
    prefs: Option<TransitionPrefs>,
    transitions_off: bool,
    window: Vec<WindowSong>,
    shuffling: bool,
    /// Bumped whenever anything a plan depends on changes: the window, the settings, a stored analysis.
    generation: u64,
    /// The last "no transition" answer - the song, the generation it was worked out in, and why
    /// (`None`: the planner chose gapless) - so the engine's retries cost a comparison until something
    /// changes, and the reason is logged once.
    none: Option<(String, u64, Option<Skip>)>,
}

static PLANNER: Mutex<Planner> =
    Mutex::new(Planner { prefs: None, transitions_off: false, window: Vec::new(), shuffling: false, generation: 0, none: None });

/// Whether the output forbids touching samples at all (`AudioPolicy::transitions_off`). Called whenever
/// the audio policy changes; the user's transition settings reach the planner by themselves
/// ([`settings_changed`]).
pub fn transition_setup(transitions_off: bool) {
    let mut p = PLANNER.lock();
    p.transitions_off = transitions_off;
    p.generation += 1;
}

/// Whether the output forbids transitions, as last set up: what is fetched ahead for a mix follows it.
pub fn transitions_off() -> bool {
    PLANNER.lock().transitions_off
}

/// Where the settings are kept (nori-settings' store): the planner reads the transition settings from it
/// each time it plans, so a plan is always made with the settings as they are, whatever order the
/// [`settings_changed`] calls of two changes made at once on two threads arrived in.
static SETTINGS: OnceLock<fn() -> Option<TransitionPrefs>> = OnceLock::new();

/// The settings store says where the planner reads the transition settings from (see [`SETTINGS`]).
pub fn settings_from(read: fn() -> Option<TransitionPrefs>) {
    let _ = SETTINGS.set(read);
}

/// The settings changed: the planner takes the transition settings from them (`StoredPrefs::transition_prefs`
/// in nori-settings). A plan already made is asked for again by the platform when it hears of the change.
pub fn settings_changed(prefs: TransitionPrefs) {
    take(&mut PLANNER.lock(), prefs);
}

fn take(p: &mut Planner, prefs: TransitionPrefs) {
    if p.prefs != Some(prefs) {
        p.prefs = Some(prefs);
        p.generation += 1;
    }
}

/// An analysis was stored by someone other than the engine's own tap (the measurer ahead, the beat
/// model): a pair that was gapless for want of it may mix now, so a "no transition" worked out before is
/// not taken as the answer again.
pub fn analyses_changed() {
    PLANNER.lock().generation += 1;
}

/// The songs the player will play: the one before the current one first, then the current one and those
/// after it, in play order. Handed in whenever that window changes.
pub fn transition_window(window: Vec<nori_model::WindowSong>, shuffling: bool) {
    let mut p = PLANNER.lock();
    p.window = window;
    p.shuffling = shuffling;
    p.generation += 1;
}

/// The engine's question: how to mix out of `outgoing_id`, if at all.
pub fn plan_for(outgoing_id: &str) -> Option<Plan> {
    // Read before the planner is locked: the store tells the planner of a change under its own lock.
    let kept = SETTINGS.get().and_then(|read| read());
    let mut p = PLANNER.lock();
    if let Some(kept) = kept {
        take(&mut p, kept);
    }
    let prefs = p.prefs?;
    let generation = p.generation;
    if p.none.as_ref().is_some_and(|(id, g, _)| *g == generation && id == outgoing_id) {
        return None;
    }
    let chosen = match pick(&prefs, p.transitions_off, &p.window, outgoing_id, p.shuffling) {
        Err(skip) => {
            let repeated = p.none.as_ref().is_some_and(|(id, _, s)| *s == Some(skip) && id == outgoing_id);
            if !repeated {
                alog::info(&format!("planFor: {}", skip.describe(outgoing_id)));
            }
            p.none = Some((outgoing_id.to_string(), generation, Some(skip)));
            return None;
        }
        Ok(pk) => pk,
    };
    let (o, n) = (p.window[chosen.out].clone(), p.window[chosen.next].clone());
    drop(p);
    // Analyses are read from the database; a plan is asked for once per song (and again only when the
    // window or settings change), so this is not on the per-buffer path.
    let (a, b) = if prefs.auto_mix {
        nori_db::active().map_or((None, None), |db| {
            let c = db.lock();
            (get(&c, &o.id).ok().flatten(), get(&c, &n.id).ok().flatten())
        })
    } else {
        (None, None)
    };
    let mut t = nori_player::automix::plan::plan(a.as_ref(), b.as_ref(), o.duration_ms, n.duration_ms, &chosen.settings);
    nori_player::transitions::shape_crossfade(&prefs, &mut t);
    let plan = engine_plan(&t, &n.id);
    PLANNER.lock().none = plan.is_none().then(|| (outgoing_id.to_string(), generation, None));
    note(TransitionNote {
        outgoing_id: o.id.clone(),
        incoming_id: n.id.clone(),
        kind: if plan.is_some() { format!("{:?}", t.kind) } else { "Gapless".into() },
        start_ms: t.out_start_ms,
        duration_ms: if plan.is_some() { t.duration_ms } else { 0 },
        tempo_ratio: t.tempo_ratio as f32,
        reason: t.reason.to_string(),
    });
    match &plan {
        None => alog::info(&format!("planFor: gapless ({})", t.reason)),
        Some(_) => alog::info(&format!(
            "transition {} -> {}: {} {} ms at {}, tempo x{:.3} ({})",
            o.title,
            n.title,
            screaming(&format!("{:?}", t.kind)),
            t.duration_ms,
            t.out_start_ms,
            t.tempo_ratio,
            t.reason
        )),
    }
    plan
}

/// A transition as planned, for a screen that says how the next song comes in: its kind as the planner
/// names it (`BeatMatched`, `EchoOut`, `Gapless`, ...), where in the outgoing song it starts and how long
/// it lasts (0 for gapless), the incoming song's speed during it, and the planner's reason.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionNote {
    pub outgoing_id: String,
    pub incoming_id: String,
    pub kind: String,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub tempo_ratio: f32,
    pub reason: String,
}

/// The last few plans made, newest last: a plan is made once per song, so this is written as rarely.
static NOTES: Mutex<Vec<TransitionNote>> = Mutex::new(Vec::new());

fn note(n: TransitionNote) {
    let mut notes = NOTES.lock();
    notes.retain(|k| k.outgoing_id != n.outgoing_id);
    if notes.len() >= 4 {
        notes.remove(0);
    }
    notes.push(n);
}

/// How the planner last planned to leave `outgoing_id`, if it has: read by a screen when the song
/// playing changes, never per frame.
pub fn transition_note(outgoing_id: &str) -> Option<TransitionNote> {
    NOTES.lock().iter().rev().find(|n| n.outgoing_id == outgoing_id).cloned()
}

/// How the planner last planned to come into `incoming_id`: the mix in progress once the ear is on the
/// incoming song.
pub fn transition_into(incoming_id: &str) -> Option<TransitionNote> {
    NOTES.lock().iter().rev().find(|n| n.incoming_id == incoming_id).cloned()
}

/// `BeatMix` as the app's logs have always named it: `BEAT_MIX`.
fn screaming(camel: &str) -> String {
    let mut s = String::with_capacity(camel.len() + 4);
    for (i, c) in camel.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            s.push('_');
        }
        s.push(c.to_ascii_uppercase());
    }
    s
}

/// Ids of radio streams and of files from outside the library, which are never analysed.
pub const RADIO_PREFIX: &str = "radio:";
pub const EXTERNAL_PREFIX: &str = "ext-";

/// Whether `song_id` should be measured as it plays, and its length in ms (0 unknown) so the measurement
/// is sized up front. Asked once per song, when its first buffer arrives.
pub fn wants_analysis(song_id: &str) -> Option<u64> {
    let kept = SETTINGS.get().and_then(|read| read());
    let (auto_mix, duration) = {
        let p = PLANNER.lock();
        (kept.or(p.prefs).is_some_and(|x| x.auto_mix),p.window.iter().find(|s| s.id == song_id).map_or(0, |s| s.duration_ms.max(0) as u64))
    };
    if !auto_mix || song_id.starts_with(RADIO_PREFIX) || song_id.starts_with(EXTERNAL_PREFIX) {
        return None;
    }
    let db = nori_db::active()?;
    let wanted = missing(&db.lock(), &[song_id.to_string()]).is_ok_and(|m| !m.is_empty());
    wanted.then_some(duration)
}

struct Finished {
    song_id: String,
    analyzer: Analyzer,
    frames: u64,
    rate: u32,
}

/// Measurements are finished (the heavy part: tempo, beat grids, key, loudness) and stored on a thread of
/// their own, never on the audio thread that hands them over.
fn worker() -> &'static Mutex<Sender<Finished>> {
    static WORKER: OnceLock<Mutex<Sender<Finished>>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = channel::<Finished>();
        std::thread::Builder::new()
            .name("nori-analysis".into())
            .spawn(move || {
                for f in rx {
                    finish(f);
                }
            })
            .expect("the analysis thread starts");
        Mutex::new(tx)
    })
}

/// The engine heard `song_id` from its first sample to its last.
pub fn analysed(song_id: &str, analyzer: Analyzer, frames: u64, rate: u32) {
    let _ = worker().lock().send(Finished { song_id: song_id.to_string(), analyzer, frames, rate });
}

fn finish(mut f: Finished) {
    let expected_ms = PLANNER.lock().window.iter().find(|s| s.id == f.song_id).map_or(0, |s| s.duration_ms);
    let heard_ms = (f.frames * 1000 / f.rate.max(1) as u64) as i64;
    // Only a song heard whole is an analysis of it; nor is anything under 30 s worth keeping.
    if !whole_song(heard_ms, expected_ms) || f.analyzer.samples() < (f.analyzer.rate() * 30.0) as u64 {
        alog::info(&format!("analysed {}: not stored: heard {heard_ms} ms of {expected_ms} ms", f.song_id));
        return;
    }
    let features = f.analyzer.take_features();
    let a = super::finish(&f.song_id, &features).track;
    let stored = nori_db::active().is_some_and(|db| {
        let c = db.lock();
        put(&c, &a).is_ok() && super::store::put_voice(&c, &f.song_id, &features.voice_curve()).is_ok()
    });
    // A pair that was gapless for want of this analysis may mix now.
    PLANNER.lock().generation += 1;
    alog::info(&format!(
        "analysed {}: {:.2} bpm (conf {:.2}, stab {:.2}), key {}, heard {} ms of {} ms, {} frames at {} Hz{}",
        f.song_id,
        a.bpm,
        a.bpm_confidence,
        a.stability,
        nori_player::automix::structure::camelot_name(a.key),
        a.duration_ms,
        expected_ms,
        f.frames,
        f.rate,
        if stored { "" } else { " (not stored: no database)" }
    ));
}

#[cfg(test)]
mod note_tests {
    use super::*;

    fn n(out: &str, inc: &str) -> TransitionNote {
        TransitionNote { outgoing_id: out.into(), incoming_id: inc.into(), kind: "BeatMatched".into(), start_ms: 1, duration_ms: 8000, tempo_ratio: 1.0, reason: String::new() }
    }

    #[test]
    fn the_last_plans_are_kept_for_a_screen_by_either_song() {
        note(n("note-a", "note-b"));
        note(n("note-b", "note-c"));
        assert_eq!(transition_note("note-a").map(|x| x.incoming_id), Some("note-b".into()));
        assert_eq!(transition_into("note-c").map(|x| x.outgoing_id), Some("note-b".into()));
        // A plan made again replaces the one before it, and only the last few are kept.
        note(TransitionNote { kind: "EchoOut".into(), ..n("note-a", "note-b") });
        assert_eq!(transition_note("note-a").map(|x| x.kind), Some("EchoOut".into()));
        for i in 0..8 {
            note(n(&format!("note-x{i}"), "note-y"));
        }
        assert_eq!(transition_note("note-a"), None);
    }
}
