//! The transition planner the audio path asks: the play window and output state are handed in when they change;
//! the engine asks for plans, whether a song wants measuring, and hands over finished measurements.
//! The engine re-asks every couple of seconds while nothing is planned, so a repeated "no" is a cached lookup.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, OnceLock, Weak};

use nori_model::alog;
use nori_player::automix::analysis::Analyzer;
use nori_player::engine::Plan;
use nori_player::transitions::{engine_plan, pick, whole_song, Skip, TransitionPrefs, WindowSong};
pub use nori_player::types::TransitionKind;
use parking_lot::Mutex;

use super::store::{get, missing};

/// How many recent plans are kept for screens.
const NOTES_KEPT: usize = 4;

/// Reads the user's transition settings; None before they are open.
pub type ReadSettings = Box<dyn Fn() -> Option<TransitionPrefs> + Send + Sync>;

/// The planner's memory between questions.
struct State {
    /// The settings last read.
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

impl State {
    const fn new() -> Self {
        State { prefs: None, transitions_off: false, window: Vec::new(), shuffling: false, generation: 0, none: None, notes: Vec::new() }
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

/// One app's planner: the window and output state handed in, the plans made, over the profile's
/// analyses and the settings `read_settings` gives.
pub struct Planner {
    state: Mutex<State>,
    db: Arc<nori_db::Profile>,
    read_settings: ReadSettings,
    /// The thread measurements are finished and stored on, never the audio thread. Started on first use.
    worker: OnceLock<Mutex<Sender<Finished>>>,
}

impl Planner {
    pub fn new(db: Arc<nori_db::Profile>, read_settings: ReadSettings) -> Arc<Planner> {
        Arc::new(Planner { state: Mutex::new(State::new()), db, read_settings, worker: OnceLock::new() })
    }

    /// Whether the output forbids touching samples (`AudioPolicy::transitions_off`); set whenever the
    /// policy changes.
    pub fn transition_setup(&self, transitions_off: bool) {
        let mut p = self.state.lock();
        p.transitions_off = transitions_off;
        p.generation += 1;
    }

    pub fn transitions_off(&self) -> bool {
        self.state.lock().transitions_off
    }

    /// An analysis was stored outside the engine's tap (the measurer ahead, the beat model): a cached
    /// "no transition" may no longer hold.
    pub fn analyses_changed(&self) {
        self.state.lock().generation += 1;
    }

    /// The songs in play order: the one before the current one, the current one, and those after it.
    pub fn transition_window(&self, window: Vec<WindowSong>, shuffling: bool) {
        let mut p = self.state.lock();
        p.window = window;
        p.shuffling = shuffling;
        p.generation += 1;
    }

    /// The engine's question: how to mix out of `outgoing_id`, if at all.
    pub fn plan_for(&self, outgoing_id: &str) -> Option<Plan> {
        // Read without the planner locked: the settings may call into the planner under their own lock.
        let kept = (self.read_settings)();
        let mut p = self.state.lock();
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
            self.db.get().map_or((None, None), |db| {
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
            kind: if plan.is_some() { t.kind } else { TransitionKind::Gapless },
            start_ms: t.out_start_ms,
            duration_ms: if plan.is_some() { t.duration_ms } else { 0 },
            tempo_ratio: t.tempo_ratio as f32,
            reason: t.reason.clone(),
        };
        {
            let mut p = self.state.lock();
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

    /// The last plan out of `outgoing_id`.
    pub fn transition_note(&self, outgoing_id: &str) -> Option<TransitionNote> {
        self.state.lock().notes.iter().rev().find(|n| n.outgoing_id == outgoing_id).cloned()
    }

    /// The last plan into `incoming_id`.
    pub fn transition_into(&self, incoming_id: &str) -> Option<TransitionNote> {
        self.state.lock().notes.iter().rev().find(|n| n.incoming_id == incoming_id).cloned()
    }

    /// Whether `song_id` should be measured as it plays, and its length in ms (0 unknown) to size the measurement.
    pub fn wants_analysis(&self, song_id: &str) -> Option<u64> {
        let kept = (self.read_settings)();
        let (auto_mix, duration) = {
            let p = self.state.lock();
            (kept.or(p.prefs).is_some_and(|x| x.auto_mix), p.duration_of(song_id).max(0) as u64)
        };
        if !auto_mix || !nori_model::analysable(song_id) {
            return None;
        }
        let db = self.db.get()?;
        let wanted = missing(&db.lock(), &[song_id.to_string()]).is_ok_and(|m| !m.is_empty());
        wanted.then_some(duration)
    }

    /// The engine heard `song_id` from its first sample to its last. Finished on the planner's thread,
    /// which ends with the planner.
    pub fn analysed(self: &Arc<Self>, song_id: &str, analyzer: Analyzer, frames: u64, rate: u32) {
        let worker = self.worker.get_or_init(|| {
            let (tx, rx) = channel::<Finished>();
            let me: Weak<Planner> = Arc::downgrade(self);
            std::thread::Builder::new()
                .name("nori-analysis".into())
                .spawn(move || rx.into_iter().for_each(|f| if let Some(p) = me.upgrade() { p.finish(f) }))
                .expect("the analysis thread starts");
            Mutex::new(tx)
        });
        let _ = worker.lock().send(Finished { song_id: song_id.to_string(), analyzer, frames, rate });
    }

    fn finish(&self, mut f: Finished) {
        let expected_ms = self.state.lock().duration_of(&f.song_id);
        let heard_ms = (f.frames * 1000 / f.rate.max(1) as u64) as i64;
        // Only a song heard whole, and at least 30 s long, is stored.
        if !whole_song(heard_ms, expected_ms) || f.analyzer.samples() < (f.analyzer.rate() * 30.0) as u64 {
            alog::info(&format!("analysed {}: not stored: heard {heard_ms} ms of {expected_ms} ms", f.song_id));
            return;
        }
        let features = f.analyzer.take_features();
        let Some(db) = self.db.get() else {
            alog::info(&format!("analysed {}: not stored: no database", f.song_id));
            return;
        };
        let stored = super::store::put_finished(&db.lock(), &f.song_id, &features);
        let a = match stored {
            Ok(a) => a,
            Err(e) => {
                alog::info(&format!("analysed {}: not stored: {e}", f.song_id));
                return;
            }
        };
        self.state.lock().generation += 1;
        alog::info(&format!(
            "analysed {}: {:.2} bpm (conf {:.2}, stab {:.2}), key {}, heard {} ms of {} ms, {} frames at {} Hz",
            f.song_id,
            a.bpm,
            a.bpm_confidence,
            a.stability,
            nori_player::automix::structure::camelot_name(a.key),
            a.duration_ms,
            expected_ms,
            f.frames,
            f.rate,
        ));
    }
}

/// A transition as planned, for screens: its kind, where in the outgoing song it starts, its length (0 for
/// gapless), the incoming speed, and the reason.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionNote {
    pub outgoing_id: String,
    pub incoming_id: String,
    pub kind: TransitionKind,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub tempo_ratio: f32,
    pub reason: String,
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

struct Finished {
    song_id: String,
    analyzer: Analyzer,
    frames: u64,
    rate: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(out: &str, inc: &str) -> TransitionNote {
        TransitionNote { outgoing_id: out.into(), incoming_id: inc.into(), kind: TransitionKind::BeatMatched, start_ms: 1, duration_ms: 8000, tempo_ratio: 1.0, reason: String::new() }
    }

    #[test]
    fn heard_measure_keeps_model_grid() {
        use nori_player::automix::beats::GRID_NEURAL;
        use nori_player::automix::synth::Synth;
        let db = std::sync::Arc::new(Mutex::new(nori_db::open("", "t").unwrap()));
        let profile = Arc::new(nori_db::Profile::default());
        profile.set(&db);
        let planner = Planner::new(profile, Box::new(|| None));
        let s = Synth::new(120.0);
        let modelled = nori_model::TrackAnalysis { song_id: "s".into(), analysis_version: crate::ANALYSIS_VERSION, duration_ms: (s.secs * 1000.0) as i64, intro_bpm: 90.0, intro_grid_source: GRID_NEURAL, ..Default::default() };
        super::super::store::put(&db.lock(), &modelled).unwrap();
        let mut analyzer = Analyzer::new(s.rate, 60_000);
        let pcm = s.render();
        analyzer.feed(&pcm);
        planner.finish(Finished { song_id: "s".into(), analyzer, frames: pcm.len() as u64, rate: s.rate });
        let kept = get(&db.lock(), "s").unwrap().unwrap();
        assert_eq!((kept.intro_grid_source, kept.intro_bpm), (GRID_NEURAL, 90.0));
        assert!(super::super::store::get_voice(&db.lock(), "s").unwrap().is_some());
    }

    #[test]
    fn songs_never_analysed() {
        let db = std::sync::Arc::new(Mutex::new(nori_db::open("", "t").unwrap()));
        let profile = Arc::new(nori_db::Profile::default());
        profile.set(&db);
        let planner = Planner::new(profile, Box::new(|| Some(TransitionPrefs { auto_mix: true, ..nori_player::sim::prefs_off() })));
        assert_eq!(planner.wants_analysis("s"), Some(0), "a library song, its length unknown");
        for id in ["radio:1", "ext-2", "pl-deezer-3"] {
            assert_eq!(planner.wants_analysis(id), None, "{id}: a station's or a provider's");
        }
    }

    fn song(id: &str, duration_ms: i64) -> WindowSong {
        WindowSong { id: id.into(), duration_ms, ..WindowSong::default() }
    }

    /// A planner over an empty database whose crossfade length the test sets (0: off).
    fn crossfading() -> (Arc<Planner>, Arc<Mutex<i32>>, Arc<Mutex<rusqlite::Connection>>) {
        let db = Arc::new(Mutex::new(nori_db::open("", "t").unwrap()));
        let profile = Arc::new(nori_db::Profile::default());
        profile.set(&db);
        let crossfade = Arc::new(Mutex::new(6));
        let read = crossfade.clone();
        let planner = Planner::new(profile, Box::new(move || Some(TransitionPrefs { crossfade_s: *read.lock(), ..nori_player::sim::prefs_off() })));
        (planner, crossfade, db)
    }

    #[test]
    fn a_cached_no_follows_what_it_depends_on() {
        let (planner, crossfade, _db) = crossfading();
        assert!(planner.plan_for("a").is_none(), "an empty window");
        planner.transition_window(vec![song("a", 200_000), song("b", 200_000)], false);
        assert!(planner.plan_for("a").is_some(), "a new window");
        let note = planner.transition_note("a").unwrap();
        assert_eq!((note.incoming_id.as_str(), note.kind, note.duration_ms), ("b", TransitionKind::EqualPowerFade, 6000));
        assert_eq!(planner.transition_into("b"), Some(note));
        assert_eq!(planner.transition_into("a"), None);

        planner.transition_setup(true);
        assert!(planner.transitions_off());
        assert!(planner.plan_for("a").is_none(), "the output forbids it");
        planner.transition_setup(false);
        assert!(!planner.transitions_off());
        assert!(planner.plan_for("a").is_some(), "the output allows it again");

        *crossfade.lock() = 0;
        assert!(planner.plan_for("a").is_none(), "crossfade off");
        *crossfade.lock() = 4;
        assert!(planner.plan_for("a").is_some(), "crossfade on again");
        assert_eq!(planner.transition_note("a").unwrap().duration_ms, 4000);
    }

    #[test]
    fn measures_sized_by_the_window() {
        let (planner, _, _db) = crossfading();
        let auto_mix = Planner::new(planner.db.clone(), Box::new(|| Some(TransitionPrefs { auto_mix: true, ..nori_player::sim::prefs_off() })));
        auto_mix.transition_window(vec![song("a", 200_000), song("b", 180_000)], false);
        assert_eq!(auto_mix.wants_analysis("b"), Some(180_000));
        assert_eq!(planner.wants_analysis("b"), None, "AutoMix off");
    }

    /// Only a song heard whole, and at least 30 s of it, is stored.
    #[test]
    fn stores_only_whole_songs() {
        use nori_player::automix::synth::Synth;
        for (secs, window_ms, stored) in [(60.0, 60_000, true), (60.0, 0, true), (60.0, 200_000, false), (20.0, 20_000, false)] {
            let (planner, _, db) = crossfading();
            planner.transition_window(vec![song("s", window_ms)], false);
            let s = Synth { secs, ..Synth::new(120.0) };
            let mut analyzer = Analyzer::new(s.rate, 60_000);
            let pcm = s.render();
            analyzer.feed(&pcm);
            planner.finish(Finished { song_id: "s".into(), analyzer, frames: pcm.len() as u64, rate: s.rate });
            assert_eq!(get(&db.lock(), "s").unwrap().is_some(), stored, "{secs} s heard of {window_ms} ms");
        }
    }

    #[test]
    fn notes() {
        let mut p = State::new();
        p.note(n("a", "b"));
        p.note(TransitionNote { kind: TransitionKind::EchoOut, ..n("a", "b") });
        assert_eq!(p.notes.iter().map(|n| n.kind).collect::<Vec<_>>(), [TransitionKind::EchoOut], "a replan replaces the note");
        for i in 0..8 {
            p.note(n(&format!("x{i}"), "y"));
        }
        assert_eq!(p.notes.len(), NOTES_KEPT);
        assert_eq!(p.notes.last().unwrap().outgoing_id, "x7");

        // Kind names log as screaming snake.
        assert_eq!(screaming_snake("BeatMatched"), "BEAT_MATCHED");
        assert_eq!(screaming_snake("Gapless"), "GAPLESS");
    }

}
