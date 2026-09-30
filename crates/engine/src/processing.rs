//! Post-download work that reads a song back from disk: its analysis when it was not measured as it
//! arrived (an MP4, a resumed download, an outdated analysis), and the beat model over its ends when
//! wanted, so AutoMix never runs it during playback.
//!
//! What a song needs is the core's (`nori_core::transfers::needs`). Songs queue in one line worked by one
//! lowest-priority thread, one decode per song, living only while a song is ready. A song still being
//! measured as it arrives waits until that ends ([`kick`]). Progress phases are the core's (`transfers`).

use std::collections::VecDeque;
use std::sync::Arc;

use nori_core::transfers::{self, Needs, Saved, Work};
use nori_core::Core;
use parking_lot::{Condvar, Mutex};

use crate::core::{decode, listen, Decoded, Model, Shelf, ARRIVALS};

/// Saved songs waiting to be read back, and whether a thread works through them.
#[derive(Debug, Default)]
struct Line {
    ids: VecDeque<String>,
    running: bool,
}

impl Line {
    /// Appends new `ids`; returns whether a thread should start.
    fn add(&mut self, ids: impl IntoIterator<Item = String>) -> bool {
        for id in ids {
            if !self.ids.contains(&id) {
                self.ids.push_back(id);
            }
        }
        self.start()
    }

    /// Whether a thread should start (none runs and something waits).
    fn start(&mut self) -> bool {
        if self.running || self.ids.is_empty() {
            return false;
        }
        self.running = true;
        true
    }

    /// The first song not `busy` (still measured as it arrives). None ends the thread; [`kick`] restarts it.
    fn next(&mut self, busy: impl Fn(&str) -> bool) -> Option<String> {
        match self.ids.iter().position(|id| !busy(id)) {
            Some(i) => self.ids.remove(i),
            None => {
                self.running = false;
                None
            }
        }
    }

    fn idle(&self) -> bool {
        self.ids.is_empty() && !self.running
    }
}

/// The post-download work over a client's disk.
pub struct Processor {
    shelf: Box<dyn Shelf>,
    line: Mutex<Line>,
    /// Signalled when the thread ends, for [`wait`].
    ended: Condvar,
}

/// Process-wide: [`kick`] is called from decoders that hold no handle ([`install`]).
static PROCESSOR: Mutex<Option<Arc<Processor>>> = Mutex::new(None);

fn processor() -> Option<Arc<Processor>> {
    PROCESSOR.lock().clone()
}

/// Sets where downloads are read back from; the first call wins.
pub fn install(shelf: Box<dyn Shelf>) {
    let mut p = PROCESSOR.lock();
    if p.is_none() {
        *p = Some(Arc::new(Processor { shelf, line: Mutex::new(Line::default()), ended: Condvar::new() }));
    }
}

/// The build has the beat model and AutoMix with "Better beat detection" is on.
fn model_on() -> bool {
    nori_player::automix::beats::AVAILABLE && nori_core::settings_store::with_prefs(|p| p.auto_mix && p.auto_mix_better_beats).unwrap_or(false)
}

/// What `id` needs once saved.
fn needs_of(core: &Core, id: &str) -> (Needs, Saved) {
    let one = vec![id.to_string()];
    let analysable = nori_core::queue::analysable(id);
    let saved = Saved {
        analysable,
        measuring: ARRIVALS.lock().has(id),
        analysed: analysable && core.analysis_missing(one.clone()).is_ok_and(|m| m.is_empty()),
        model_on: model_on(),
        beats_wanted: transfers::wants_beats(id) && nori_core::settings_store::with_prefs(|p| p.download_beats != nori_core::settings::DownloadBeats::Never).unwrap_or(false),
        beats_done: analysable && core.analysis_neural_missing(one).is_ok_and(|m| m.is_empty()),
    };
    (transfers::needs(saved), saved)
}

/// `ids` just finished downloading: marks what each needs and queues those needing a read-back.
pub fn saved(ids: Vec<String>) {
    plan(ids, true);
}

/// "Analyse downloaded songs" (`Core::download_unanalysed`): as [`saved`] without lyrics. Returns how many
/// were queued.
pub fn analyse(ids: Vec<String>) -> u32 {
    plan(ids, false)
}

fn plan(ids: Vec<String>, download: bool) -> u32 {
    // Without a shelf nothing is marked, so nothing waits forever.
    let (Some(p), Some(core)) = (processor(), nori_core::active()) else { return 0 };
    let mut line = Vec::new();
    for id in ids {
        let (needs, s) = needs_of(&core, &id);
        if transfers::plan(&id, needs, download.then_some(needs.analysis && !s.measuring)) {
            line.push(id);
        } else if !needs.beats && transfers::wants_beats(&id) && s.beats_done {
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

/// A song's measuring as it arrived ended: resume the line.
pub fn kick() {
    if let Some(p) = processor() {
        if p.line.lock().start() {
            p.spawn();
        }
    }
}

/// Blocks until the line is empty (scripts and tests).
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
        crate::arriving::lower_priority();
        // Loaded by the first song that needs it, dropped with the thread.
        let mut model = Model::default();
        loop {
            let next = self.line.lock().next(|id| ARRIVALS.lock().has(id));
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

    /// Reads `id` back for its pending analysis and beat model run.
    fn process(&self, model: &mut Model, id: &str) {
        let (analysis, beats) = (transfers::waits(id, Work::Analysis), transfers::waits(id, Work::Beats));
        if !analysis && !beats {
            return;
        }
        let Some(core) = nori_core::active() else { return finish(id) };
        let one = vec![id.to_string()];
        // It may have been measured elsewhere since it was saved.
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
        // Abandoned once it no longer waits (cancelled or timed out).
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
                crate::arriving::give_memory_back();
            }
            transfers::work_done(id, Work::Beats);
            let _ = core.download_beats_forget(&[id.to_string()]);
        }
        if let (Some(a), Some(b)) = (cpu, crate::arriving::thread_cpu_ms()) {
            nori_core::alog::info(&format!("reading {id} back took {} ms of CPU", b.saturating_sub(a)));
        }
        if stored {
            // Replan what was planned without it.
            let measurers = ARRIVALS.lock().measurers();
            for m in measurers {
                m.stored_elsewhere();
            }
        }
    }
}

/// Marks `id`'s post-download work done.
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

    #[test]
    fn line_drains_in_order_skipping_busy_songs() {
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
