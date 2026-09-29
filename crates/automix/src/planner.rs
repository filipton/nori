//! The transition planner the audio path asks: the play window and output state are handed in when they change;
//! the engine asks for plans, whether a song wants measuring, and hands over finished measurements.
//! The engine re-asks every couple of seconds while nothing is planned, so a repeated "no" is a cached lookup.

use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

use nori_model::alog;
use nori_player::automix::analysis::Analyzer;
use nori_player::engine::Plan;
use nori_player::transitions::{engine_plan, pick, whole_song, Skip, TransitionPrefs, WindowSong};
use parking_lot::Mutex;

use super::store::{get, missing, put};

/// Ids of radio streams and of files from outside the library, which are never analysed.
const RADIO_PREFIX: &str = "radio:";
const EXTERNAL_PREFIX: &str = "ext-";
/// How many recent plans are kept for screens.
const NOTES_KEPT: usize = 4;

type ReadSettings = fn() -> Option<TransitionPrefs>;

struct Planner {
    /// Reads the user's transition settings from the settings store; set once at startup.
    read_settings: Option<ReadSettings>,
    prefs: Option<TransitionPrefs>,
    transitions_off: bool,
    window: Vec<WindowSong>,
    shuffling: bool,
    /// Bumped whenever anything a plan depends on changes: the window, the settings, a stored analysis.
    generation: u64,
    /// The last "no transition" answer: song, generation, and why (`None`: the planner chose gapless).
    none: Option<(String, u64, Option<Skip>)>,
    /// The last few plans, newest last.
    notes: Vec<TransitionNote>,
}

impl Planner {
    const fn new() -> Self {
        Planner { read_settings: None, prefs: None, transitions_off: false, window: Vec::new(), shuffling: false, generation: 0, none: None, notes: Vec::new() }
    }

    fn take_prefs(&mut self, prefs: TransitionPrefs) {
        if self.prefs != Some(prefs) {
            self.prefs = Some(prefs);
            self.generation += 1;
        }
    }

    fn duration_of(&self, song_id: &str) -> i64 {
        self.window.iter().find(|s| s.id == song_id).map_or(0, |s| s.duration_ms)
    }

    fn note(&mut self, n: TransitionNote) {
        self.notes.retain(|k| k.outgoing_id != n.outgoing_id);
        if self.notes.len() >= NOTES_KEPT {
            self.notes.remove(0);
        }
        self.notes.push(n);
    }
}

/// The process's one planner: the settings store, the queue, the engine and the clients all reach it with no
/// shared handle between them.
static PLANNER: Mutex<Planner> = Mutex::new(Planner::new());

/// The settings as the store has them now. Read without the planner locked: the store may call into the planner
/// under its own lock.
fn read_settings() -> Option<TransitionPrefs> {
    let read = PLANNER.lock().read_settings;
    read.and_then(|read| read())
}

/// Whether the output forbids touching samples (`AudioPolicy::transitions_off`); set whenever the policy changes.
pub fn transition_setup(transitions_off: bool) {
    let mut p = PLANNER.lock();
    p.transitions_off = transitions_off;
    p.generation += 1;
}

pub fn transitions_off() -> bool {
    PLANNER.lock().transitions_off
}

/// Where the planner reads the transition settings from: read at every plan, so plans always use current settings.
pub fn settings_from(read: ReadSettings) {
    PLANNER.lock().read_settings.get_or_insert(read);
}

/// An analysis was stored outside the engine's tap (the measurer ahead, the beat model): a cached "no transition"
/// may no longer hold.
pub fn analyses_changed() {
    PLANNER.lock().generation += 1;
}

/// The songs in play order: the one before the current one, the current one, and those after it.
pub fn transition_window(window: Vec<WindowSong>, shuffling: bool) {
    let mut p = PLANNER.lock();
    p.window = window;
    p.shuffling = shuffling;
    p.generation += 1;
}

/// The engine's question: how to mix out of `outgoing_id`, if at all.
pub fn plan_for(outgoing_id: &str) -> Option<Plan> {
    let kept = read_settings();
    let mut p = PLANNER.lock();
    if let Some(kept) = kept {
        p.take_prefs(kept);
    }
    let prefs = p.prefs?;
    let generation = p.generation;
    if p.none.as_ref().is_some_and(|(id, g, _)| *g == generation && id == outgoing_id) {
        return None;
    }
    let chosen = match pick(&prefs, p.transitions_off, &p.window, outgoing_id, p.shuffling) {
        Ok(chosen) => chosen,
        Err(skip) => {
            if !p.none.as_ref().is_some_and(|(id, _, s)| *s == Some(skip) && id == outgoing_id) {
                alog::info(&format!("planFor: {}", skip.describe(outgoing_id)));
            }
            p.none = Some((outgoing_id.to_string(), generation, Some(skip)));
            return None;
        }
    };
    let (o, n) = (p.window[chosen.out].clone(), p.window[chosen.next].clone());
    drop(p);
    // Database reads happen without the planner locked.
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
    let note = TransitionNote {
        outgoing_id: o.id.clone(),
        incoming_id: n.id.clone(),
        kind: if plan.is_some() { format!("{:?}", t.kind) } else { "Gapless".into() },
        start_ms: t.out_start_ms,
        duration_ms: if plan.is_some() { t.duration_ms } else { 0 },
        tempo_ratio: t.tempo_ratio as f32,
        reason: t.reason.clone(),
    };
    {
        let mut p = PLANNER.lock();
        p.none = plan.is_none().then(|| (outgoing_id.to_string(), generation, None));
        p.note(note);
    }
    match &plan {
        None => alog::info(&format!("planFor: gapless ({})", t.reason)),
        Some(_) => alog::info(&format!(
            "transition {} -> {}: {} {} ms at {}, tempo x{:.3} ({})",
            o.title,
            n.title,
            screaming_snake(&format!("{:?}", t.kind)),
            t.duration_ms,
            t.out_start_ms,
            t.tempo_ratio,
            t.reason
        )),
    }
    plan
}

/// A transition as planned, for screens: the kind as the planner names it (`BeatMatched`, `EchoOut`, `Gapless`,
/// ...), where in the outgoing song it starts, its length (0 for gapless), the incoming speed, and the reason.
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

/// The last plan out of `outgoing_id`.
pub fn transition_note(outgoing_id: &str) -> Option<TransitionNote> {
    PLANNER.lock().notes.iter().rev().find(|n| n.outgoing_id == outgoing_id).cloned()
}

/// The last plan into `incoming_id`.
pub fn transition_into(incoming_id: &str) -> Option<TransitionNote> {
    PLANNER.lock().notes.iter().rev().find(|n| n.incoming_id == incoming_id).cloned()
}

/// `BeatMatched` -> `BEAT_MATCHED`, as the logs name kinds.
fn screaming_snake(camel: &str) -> String {
    let mut s = String::with_capacity(camel.len() + 4);
    for (i, c) in camel.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            s.push('_');
        }
        s.push(c.to_ascii_uppercase());
    }
    s
}

/// Whether `song_id` should be measured as it plays, and its length in ms (0 unknown) to size the measurement.
pub fn wants_analysis(song_id: &str) -> Option<u64> {
    let kept = read_settings();
    let (auto_mix, duration) = {
        let p = PLANNER.lock();
        (kept.or(p.prefs).is_some_and(|x| x.auto_mix), p.duration_of(song_id).max(0) as u64)
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

/// The thread measurements are finished and stored on, never the audio thread. Started on first use.
fn worker() -> &'static Mutex<Sender<Finished>> {
    static WORKER: OnceLock<Mutex<Sender<Finished>>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = channel::<Finished>();
        std::thread::Builder::new()
            .name("nori-analysis".into())
            .spawn(move || rx.into_iter().for_each(finish))
            .expect("the analysis thread starts");
        Mutex::new(tx)
    })
}

/// The engine heard `song_id` from its first sample to its last.
pub fn analysed(song_id: &str, analyzer: Analyzer, frames: u64, rate: u32) {
    let _ = worker().lock().send(Finished { song_id: song_id.to_string(), analyzer, frames, rate });
}

fn finish(mut f: Finished) {
    let expected_ms = PLANNER.lock().duration_of(&f.song_id);
    let heard_ms = (f.frames * 1000 / f.rate.max(1) as u64) as i64;
    // Only a song heard whole, and at least 30 s long, is stored.
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
mod tests {
    use super::*;

    fn n(out: &str, inc: &str) -> TransitionNote {
        TransitionNote { outgoing_id: out.into(), incoming_id: inc.into(), kind: "BeatMatched".into(), start_ms: 1, duration_ms: 8000, tempo_ratio: 1.0, reason: String::new() }
    }

    #[test]
    fn notes_keep_last_plan_per_song() {
        let mut p = Planner::new();
        p.note(n("a", "b"));
        p.note(TransitionNote { kind: "EchoOut".into(), ..n("a", "b") });
        assert_eq!(p.notes.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>(), ["EchoOut"], "a replan replaces the note");
        for i in 0..8 {
            p.note(n(&format!("x{i}"), "y"));
        }
        assert_eq!(p.notes.len(), NOTES_KEPT);
        assert_eq!(p.notes.last().unwrap().outgoing_id, "x7");
    }

    #[test]
    fn kind_names_log_as_screaming_snake() {
        assert_eq!(screaming_snake("BeatMatched"), "BEAT_MATCHED");
        assert_eq!(screaming_snake("Gapless"), "GAPLESS");
    }
}
