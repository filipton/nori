//! The work after a download's bytes that reads the song back: its analysis from the disk when it was not
//! measured as it came (an MP4, a download taken up half way, an analysis of an older version, a measuring that
//! failed), and Beat This! over its intro and outro when the model is on and wanted for the download
//! ("ML beats for downloads"), so AutoMix never runs it for that song while it plays.
//!
//! What a saved song needs is the core's (`nori_core::transfers::needs`); the songs wait in one line, and one
//! thread of the lowest priority works through it one song at a time, each decoded once from the disk for both,
//! the model loaded for the thread's life and let go with it. The thread exists only while the line holds a song
//! that is ready: with nothing downloaded or asked for it costs nothing. A song still being measured as it comes
//! waits in the line until that decode ends, which says so ([`kick`]). The phases the screens show are the
//! core's (`transfers`: "Analysing…", "Detecting beats…"), each step timed there and given up if it hangs.

use std::collections::VecDeque;
use std::sync::Arc;

use nori_core::transfers::{self, Needs, Saved, Work};
use nori_core::Core;
use parking_lot::{Condvar, Mutex};

use crate::core::{decode, listen, lower_priority, Decoded, Model, Shelf, ARRIVING, MEASURERS};

/// The line of saved songs waiting to be read back, and whether a thread is working through it.
#[derive(Debug, Default)]
struct Line {
    ids: VecDeque<String>,
    running: bool,
}

impl Line {
    /// `ids` join the end of the line (one in it already keeps its place). Whether a thread is to start.
    fn add(&mut self, ids: impl IntoIterator<Item = String>) -> bool {
        for id in ids {
            if !self.ids.contains(&id) {
                self.ids.push_back(id);
            }
        }
        self.start()
    }

    /// Whether a thread is to start: none runs, and something waits.
    fn start(&mut self) -> bool {
        if self.running || self.ids.is_empty() {
            return false;
        }
        self.running = true;
        true
    }

    /// The next song to read, in the order they came: the first one not `busy` (still measured as it comes).
    /// None when none is ready, and the thread ends; a song left waiting is started again by [`kick`].
    fn next(&mut self, busy: impl Fn(&str) -> bool) -> Option<String> {
        match self.ids.iter().position(|id| !busy(id)) {
            Some(i) => self.ids.remove(i),
            None => {
                self.running = false;
                None
            }
        }
    }

    /// Nothing waits and nothing runs.
    fn idle(&self) -> bool {
        self.ids.is_empty() && !self.running
    }
}

/// The work after the bytes, over a client's disk.
pub struct Processor {
    shelf: Box<dyn Shelf>,
    line: Mutex<Line>,
    /// Told when the thread ends, for [`wait`].
    ended: Condvar,
}

/// The one processor, once the platform says where its downloads are ([`install`]).
static PROCESSOR: Mutex<Option<Arc<Processor>>> = Mutex::new(None);

fn processor() -> Option<Arc<Processor>> {
    PROCESSOR.lock().clone()
}

/// The platform's downloads are on `shelf`: songs saved from now on are read back from there. The first shelf
/// stays (a platform may say so more than once).
pub fn install(shelf: Box<dyn Shelf>) {
    let mut p = PROCESSOR.lock();
    if p.is_none() {
        *p = Some(Arc::new(Processor { shelf, line: Mutex::new(Line::default()), ended: Condvar::new() }));
    }
}

/// Whether the beat model is on: the build has it, and AutoMix and "Better beat detection" are on.
fn model_on() -> bool {
    nori_player::automix::beats::AVAILABLE && nori_core::settings_store::with_prefs(|p| p.auto_mix && p.auto_mix_better_beats).unwrap_or(false)
}

/// What `id` needs once saved, from what the core knows of it now.
fn needs_of(core: &Core, id: &str) -> (Needs, Saved) {
    let one = vec![id.to_string()];
    let analysable = nori_core::queue::analysable(id);
    let saved = Saved {
        analysable,
        measuring: ARRIVING.lock().iter().any(|i| i == id),
        analysed: analysable && core.analysis_missing(one.clone()).is_ok_and(|m| m.is_empty()),
        model_on: model_on(),
        beats_wanted: transfers::wants_beats(id) && nori_core::settings_store::with_prefs(|p| p.download_beats != nori_core::settings::DownloadBeats::Never).unwrap_or(false),
        beats_done: analysable && core.analysis_neural_missing(one).is_ok_and(|m| m.is_empty()),
    };
    (transfers::needs(saved), saved)
}

/// `ids` were just downloaded and are in the downloads table as finished: what each needs besides its lyrics is
/// decided and marked (it shows as processing until that is over), and those that need reading back join the line.
pub fn saved(ids: Vec<String>) {
    plan(ids, true);
}

/// Downloaded songs asked for again (the settings' "Analyse downloaded songs", `Core::download_unanalysed`):
/// as [`saved`], without lyrics. How many joined the line.
pub fn analyse(ids: Vec<String>) -> u32 {
    plan(ids, false)
}

fn plan(ids: Vec<String>, download: bool) -> u32 {
    // No disk to read from: nothing is marked, so nothing waits for work that would never come.
    let (Some(p), Some(core)) = (processor(), nori_core::active()) else { return 0 };
    let mut line = Vec::new();
    for id in ids {
        let (needs, s) = needs_of(&core, &id);
        if transfers::plan(&id, needs, download.then_some(needs.analysis && !s.measuring)) {
            line.push(id);
        } else if !needs.beats && transfers::wants_beats(&id) && s.beats_done {
            // Read already: not wanted any more.
            let _ = core.download_beats_forget(std::slice::from_ref(&id));
        }
    }
    let n = line.len() as u32;
    if n > 0 {
        nori_core::alog::info(&format!("{n} saved songs to read back"));
        p.add(line);
    }
    n
}

/// A song measured as it came is over: a saved song waiting for it is looked at.
pub fn kick() {
    if let Some(p) = processor() {
        if p.line.lock().start() {
            p.spawn();
        }
    }
}

/// Waits until nothing is left in the line: for a terminal client's script and the tests.
pub fn wait() {
    if let Some(p) = processor() {
        let mut line = p.line.lock();
        while !line.idle() {
            p.ended.wait(&mut line);
        }
    }
}

impl Processor {
    fn add(self: &Arc<Self>, ids: Vec<String>) {
        if self.line.lock().add(ids) {
            self.spawn();
        }
    }

    fn spawn(self: &Arc<Self>) {
        let me = self.clone();
        if std::thread::Builder::new().name("nori-process".into()).spawn(move || me.run()).is_err() {
            self.line.lock().running = false;
        }
    }

    fn run(&self) {
        // The model's runs are heavy: they yield to the music and to everything else.
        lower_priority();
        // The beat model, loaded by the first song of this thread's life that needs it, let go with the thread.
        let mut model = Model::default();
        loop {
            let next = self.line.lock().next(|id| ARRIVING.lock().iter().any(|i| i == id));
            let Some(id) = next else { break };
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.process(&mut model, &id)));
            if let Err(p) = done {
                nori_core::alog::info(&format!("reading {id} back: {}", crate::panic_words(&*p)));
                finish(&id);
            }
        }
        drop(model);
        self.ended.notify_all();
    }

    /// Reads `id` back for what it still waits for: its analysis, then the model over its ends.
    fn process(&self, model: &mut Model, id: &str) {
        let (analysis, beats) = (transfers::waits(id, Work::Analysis), transfers::waits(id, Work::Beats));
        if !analysis && !beats {
            return;
        }
        let Some(core) = nori_core::active() else { return finish(id) };
        let one = vec![id.to_string()];
        // Asked again now: it may have been measured as it came since it was saved, or elsewhere.
        let classical = analysis && !core.analysis_missing(one.clone()).unwrap_or_default().is_empty();
        if analysis && !classical {
            transfers::work_done(id, Work::Analysis);
        }
        let read = beats && (classical || !core.analysis_neural_missing(one).unwrap_or_default().is_empty());
        if beats && !read {
            transfers::work_done(id, Work::Beats);
            let _ = core.download_beats_forget(&[id.to_string()]);
        }
        if !classical && !read {
            return;
        }
        let Some((pieces, hint)) = self.shelf.whole(id).and_then(|w| Some((crate::pieces::Pieces::open(&w.files).ok()?, w.hint))) else {
            nori_core::alog::info(&format!("reading {id} back: not whole on the disk"));
            return finish(id);
        };
        transfers::working(id, if classical { Work::Analysis } else { Work::Beats });
        // Fetched and loaded the first time a song of this thread needs it; one that cannot come is tried once.
        let listen_now = read && model.ready();
        if read && !listen_now {
            nori_core::alog::info(&format!("reading {id} back: the beat model is not here"));
            transfers::work_done(id, Work::Beats);
            if !classical {
                return;
            }
        }
        let song = core.download_song(id);
        let expected_ms = song.as_ref().map_or(0, |s| s.duration as i64 * 1000);
        let hint = hint.or_else(|| song.map(|s| s.suffix).filter(|s| !s.is_empty()));
        let cpu = crate::arriving::thread_cpu_ms();
        // Left half way when it stops waiting (taken back, or given up on).
        let decoded = decode(id, "reading back", pieces, hint.as_deref(), expected_ms, classical, listen_now, || {
            transfers::waits(id, Work::Analysis) || transfers::waits(id, Work::Beats)
        });
        let Some(Decoded { stream, ends }) = decoded else { return };
        let mut stored = false;
        if classical {
            let a = stream.and_then(|s| core.analysis_finish_whole(id, s, expected_ms).ok().flatten());
            nori_core::alog::info(&match &a {
                Some(t) => format!("analysed {id} from the disk: {:.2} bpm (conf {:.2}, stab {:.2})", t.bpm, t.bpm_confidence, t.stability),
                None => format!("analysed {id} from the disk: not stored"),
            });
            stored = a.is_some();
            transfers::work_done(id, Work::Analysis);
        }
        if listen_now {
            if let (Some(model), Some(mut ends)) = (model.get(), ends) {
                if classical {
                    transfers::working(id, Work::Beats);
                }
                stored |= listen(&core, id, model, &mut ends);
                drop(ends);
                // The model's run took tens of megabytes a block at a time, all free again: handed back now.
                crate::arriving::give_memory_back();
            }
            transfers::work_done(id, Work::Beats);
            let _ = core.download_beats_forget(&[id.to_string()]);
        }
        if let (Some(a), Some(b)) = (cpu, crate::arriving::thread_cpu_ms()) {
            nori_core::alog::info(&format!("reading {id} back took {} ms of CPU", b.saturating_sub(a)));
        }
        if stored {
            // What was planned without it is planned again.
            let measurers: Vec<_> = MEASURERS.lock().iter().filter_map(|w| w.upgrade()).collect();
            for m in measurers {
                m.stored_elsewhere();
            }
        }
    }
}

/// `id` is done with the work after its bytes, whatever was left of it.
fn finish(id: &str) {
    transfers::work_done(id, Work::Analysis);
    transfers::work_done(id, Work::Beats);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Songs are read back one at a time, in the order they came; one still measured as it comes waits its turn
    /// without holding the others up, and the thread ends when nothing is ready, to start again when told.
    #[test]
    fn the_line_drains_one_song_at_a_time() {
        let mut line = Line::default();
        assert!(!line.start(), "nothing to do: no thread");
        assert!(line.add(ids(&["a", "b", "c"])), "the first songs start a thread");
        assert!(!line.add(ids(&["d", "b"])), "one is running: the songs join its line, b keeps its place");
        assert_eq!(line.ids, ids(&["a", "b", "c", "d"]));
        let arriving = |id: &str| id == "b";
        let mut order = Vec::new();
        while let Some(id) = line.next(arriving) {
            order.push(id);
            assert!(line.running, "one thread, one song at a time");
        }
        assert_eq!(order, ["a", "c", "d"], "b is still measured as it comes: left for later");
        assert!(!line.running, "nothing ready: the thread ends");
        assert!(!line.idle(), "b still waits");
        // Its measuring ended: the thread starts again for it.
        assert!(line.start());
        assert!(!line.start(), "not twice");
        assert_eq!(line.next(|_| false), Some("b".to_string()));
        assert_eq!(line.next(|_| false), None);
        assert!(line.idle(), "nothing lingers");
    }
}
