//! Downloads as the core's calls: the queue of songs to download and what finished, kept in the core's
//! database. How downloads run, the facts they are worded from and how the batch went are nori-transfers'.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use crate::Core;

pub use nori_transfers::transfers::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Queues `songs` for download; see [`DownloadQueued`].
    pub fn download_queue(&self, songs: Vec<crate::Song>) -> crate::Result<DownloadQueued> {
        follow_quality();
        let rows = songs.into_iter().map(|s| {
            let json = serde_json::to_string(&s).unwrap_or_default();
            (s.id, json)
        });
        let mut c = self.db.lock();
        let q = queue_rows(&mut c, rows)?;
        self.held_queued(&q);
        Ok(q)
    }

    /// Queues every song of the offline index, in index order, as [`Core::download_queue`] does. One
    /// call however big the library: the songs go from the index into the queue without leaving the core.
    pub fn download_queue_library(&self) -> crate::Result<DownloadQueued> {
        follow_quality();
        let mut c = self.db.lock();
        let rows: Vec<(String, String)> = {
            let mut st = c.prepare("SELECT id, json FROM items WHERE server=sid() AND kind=?1 ORDER BY rowid")?;
            let rows = st.query_map([crate::db::SONG], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let q = queue_rows(&mut c, rows)?;
        self.held_queued(&q);
        Ok(q)
    }

    /// The beat model is to read `ids` once they are downloaded ("ML beats for downloads": the answer to the
    /// question, or always). Kept with the downloads, so a download an earlier process left half way gets it too.
    pub fn download_want_beats(&self, ids: Vec<String>) -> crate::Result<()> {
        {
            let mut c = self.db.lock();
            let tx = c.transaction()?;
            {
                let mut add = tx.prepare_cached("INSERT OR IGNORE INTO download_beats(server, id) VALUES(sid(), ?1)")?;
                for id in &ids {
                    add.execute([id])?;
                }
            }
            tx.commit()?;
        }
        want_beats(&ids, true);
        Ok(())
    }

    /// Downloaded songs to analyse again: those with no analysis of the current version, and, with `beats`, those
    /// the beat model has not read (the settings' "Analyse downloaded songs"). The songs the model is to read are
    /// written down as [`Core::download_want_beats`] does. Newest first, as the table lists them.
    pub fn download_unanalysed(&self, beats: bool) -> crate::Result<Vec<String>> {
        let ids: Vec<String> = self.download_ids(true)?.into_iter().filter(|id| crate::queue::analysable(id)).collect();
        let missing: HashSet<String> = self.analysis_missing(ids.clone())?.into_iter().collect();
        let unread: HashSet<String> = if beats { self.analysis_neural_missing(ids.clone())?.into_iter().collect() } else { HashSet::new() };
        if beats {
            let want: Vec<String> = ids.iter().filter(|id| missing.contains(*id) || unread.contains(*id)).cloned().collect();
            self.download_want_beats(want)?;
        }
        Ok(ids.into_iter().filter(|id| missing.contains(id) || unread.contains(id)).collect())
    }
}

/// Asked only in Rust, so not exported to Kotlin.
impl Core {

    /// The beat model has read `id` (or will not): it is not wanted for it any more.
    pub fn download_beats_forget(&self, ids: &[String]) -> crate::Result<()> {
        {
            let c = self.db.lock();
            let mut gone = c.prepare_cached("DELETE FROM download_beats WHERE server=sid() AND id=?1")?;
            for id in ids {
                gone.execute([id])?;
            }
        }
        want_beats(ids, false);
        Ok(())
    }
}

/// What pressing Download does about the beat model now ([`beats_offer`]): nothing while it is off, else ask,
/// or not, as "ML beats for downloads" says.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_beats_offer() -> BeatsOffer {
    let (on, choice) = crate::settings_store::with_prefs(|p| (p.auto_mix && p.auto_mix_better_beats, p.download_beats)).unwrap_or((false, nori_settings::settings::DownloadBeats::Ask));
    beats_offer(on && nori_player::automix::beats::AVAILABLE, choice)
}

/// What "ML beats for downloads" becomes when the question's answer is to be remembered.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_beats_remembered(yes: bool) -> nori_settings::settings::DownloadBeats {
    beats_remembered(yes)
}

/// What the saved songs are still waiting for, for the notification; none when nothing is. `now` is the
/// platform's clock.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_processing(now: i64) -> Option<Processing> {
    processing(now)
}

/// The ids the beat model is to read once downloaded, from the downloads' own table.
pub fn beats_wanted_rows(c: &rusqlite::Connection) -> crate::Result<Vec<String>> {
    let mut st = c.prepare("SELECT id FROM download_beats WHERE server=sid()")?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Downloads that settled, in the order they did: each id finished (`finished` true) or left the
    /// queue for good. One transaction however many there are; ids the table no longer holds cost nothing.
    pub fn download_settle(&self, ids: Vec<String>, finished: Vec<bool>) -> crate::Result<()> {
        let gone_ids: Vec<String> = ids.iter().zip(&finished).filter(|(_, f)| !**f).map(|(id, _)| id.clone()).collect();
        let mut c = self.db.lock();
        let tx = c.transaction()?;
        {
            let held = self.held.lock();
            let mut done = tx.prepare_cached("UPDATE downloads SET done=1 WHERE server=sid() AND id=?1")?;
            let mut gone = tx.prepare_cached("DELETE FROM downloads WHERE server=sid() AND id=?1")?;
            let mut unwanted = tx.prepare_cached("DELETE FROM download_beats WHERE server=sid() AND id=?1")?;
            for id in &gone_ids {
                unwanted.execute([id])?;
            }
            let mut state: HashMap<&str, i32> = HashMap::new();
            for (id, f) in ids.iter().zip(&finished) {
                let now = state.entry(id.as_str()).or_insert_with(|| held.state(id));
                match (*now, *f) {
                    (0, _) | (2, true) => {}
                    (_, true) => {
                        done.execute([id])?;
                        *now = 2;
                    }
                    (_, false) => {
                        gone.execute([id])?;
                        *now = 0;
                    }
                }
            }
        }
        tx.commit()?;
        drop(c);
        {
            let mut held = self.held.lock();
            for (id, f) in ids.iter().zip(finished) {
                if f {
                    held.finished(id);
                } else {
                    held.removed(id);
                }
            }
        }
        // The tracker looks songs up in the database while it is locked: taken once the database is let go.
        want_beats(&gone_ids, false);
        Ok(())
    }

    /// Takes every unfinished song out of the queue at once, forgets their marks and figures, and says
    /// which they were, for the platform to stop.
    pub fn download_cancel_all(&self) -> crate::Result<Vec<String>> {
        let ids: Vec<String> = {
            let c = self.db.lock();
            let ids: Vec<String> = {
                let mut st = c.prepare_cached("SELECT id FROM downloads WHERE server=sid() AND done=0")?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.filter_map(|r| r.ok()).collect()
            };
            c.execute("DELETE FROM download_beats WHERE server=sid() AND id IN (SELECT id FROM downloads WHERE server=sid() AND done=0)", [])?;
            c.execute("DELETE FROM downloads WHERE server=sid() AND done=0", [])?;
            let mut held = self.held.lock();
            for id in &ids {
                held.removed(id);
            }
            ids
        };
        // The tracker looks songs up in the database while it is locked, so it is only taken once the
        // database is let go.
        with(|t| {
            for id in &ids {
                t.close(id);
                t.unmark(id);
            }
        });
        want_beats(&ids, false);
        Ok(ids)
    }

    /// The table counted, from memory.
    pub fn download_counts(&self) -> DownloadCounts {
        let held = self.held.lock();
        let done = held.done;
        DownloadCounts { done, pending: held.ids.len() as u32 - done, version: HELD_VERSION.load(Ordering::Relaxed) }
    }

    /// Brings the downloads table and the platform's queue (`known`: what it holds of the songs pending
    /// here) back into agreement after the process died: finished songs are recorded, failed ones marked,
    /// and what is left to do is said.
    pub fn download_recover(&self, known: Vec<DownloadKnown>) -> crate::Result<DownloadRecovery> {
        follow_quality();
        let pending: Vec<String> = self.downloads(false)?.into_iter().map(|s| s.id).collect();
        let (mut r, failed) = recovery(&pending, &known);
        self.download_settle(r.finished.clone(), vec![true; r.finished.len()])?;
        r.failed = with(|t| {
            failed
                .into_iter()
                .map(|(id, length, bytes)| {
                    let estimate = t.info(&id).estimate;
                    if !t.marks.contains_key(&id) {
                        t.marks.insert(id.clone(), (Phase::Failed, 0));
                        t.changed.insert(id.clone());
                    }
                    DownloadFailed { progress: fraction(length, bytes, estimate), id }
                })
                .collect()
        });
        Ok(r)
    }
}

impl Core {
    fn held_queued(&self, q: &DownloadQueued) {
        let mut held = self.held.lock();
        for id in &q.fresh {
            held.queued(id);
        }
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// The downloads screen's lists: pending songs split by what they are doing, oldest first (the order
    /// they run), and this session's finished songs newest first.
    pub fn download_sections(&self) -> crate::Result<DownloadSections> {
        let pending = self.downloads(false)?;
        // Of the finished songs only this session's are listed, and there are at most [`RECENT`] of those:
        // they are looked up one by one rather than the whole table read for them.
        let recent: Vec<String> = with(|t| t.marks.iter().filter(|(_, m)| matches!(m.0, Phase::Done | Phase::Processing { .. })).map(|(id, _)| id.clone()).collect());
        let done: Vec<crate::Song> = {
            let c = self.db.lock();
            let mut st = c.prepare_cached("SELECT json FROM downloads WHERE server=sid() AND id=?1 AND done=1")?;
            recent.iter().filter_map(|id| st.query_row([id], |r| r.get::<_, String>(0)).ok()).filter_map(|j| serde_json::from_str(&j).ok()).collect()
        };
        with(|t| {
            let [active, queued, failed, finished] = sections(&pending, &done, &t.marks, |s: &crate::Song| s.id.as_str());
            Ok(DownloadSections { active, queued, failed, finished })
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Song;
    use parking_lot::Mutex;

    /// The download tracker is the process's: a test that settles or cancels downloads moves what another
    /// running beside it reads (cancelling all sets the songs wanted for the beat model from its own
    /// database), so they take turns, as the queue's tests do.
    static TURN: Mutex<()> = Mutex::new(());

    fn song(id: &str) -> Song {
        Song { id: id.into(), ..Default::default() }
    }

    #[test]
    fn queuing_adds_what_is_new_and_asks_again_for_what_is_stuck() {
        let _turn = TURN.lock();
        let core = Core::new(String::new(), "t".into()).unwrap();
        let q = core.download_queue(vec![song("a"), song("b"), song("a")]).unwrap();
        assert_eq!((q.fresh, q.again), (vec!["a".to_string(), "b".into()], vec![]));
        core.download_done("a".into()).unwrap();
        let q = core.download_queue(vec![song("a"), song("b"), song("c")]).unwrap();
        assert_eq!(q.fresh, ["c"], "a finished song is left alone");
        assert_eq!(q.again, ["b"], "an unfinished one is asked for again");
        // Listed newest first; the queue runs, and the screen shows it, the other way round.
        let pending: Vec<String> = core.downloads(false).unwrap().into_iter().rev().map(|s| s.id).collect();
        assert_eq!(pending, ["b", "c"]);
    }

    /// "ML beats for downloads": the songs the model is to read are kept with the downloads (a process that dies
    /// half way through a batch finds them again) until the model has read them or the download is taken back.
    #[test]
    fn the_beat_model_is_wanted_for_downloads_until_it_has_read_them() {
        let _turn = TURN.lock();
        let core = Core::new(String::new(), "t".into()).unwrap();
        core.download_queue(vec![song("wb-a"), song("wb-b"), song("wb-c")]).unwrap();
        core.download_want_beats(vec!["wb-a".into(), "wb-b".into(), "wb-c".into()]).unwrap();
        assert!(wants_beats("wb-a") && wants_beats("wb-b"));
        let rows = |core: &Core| {
            let mut r = beats_wanted_rows(&core.db.lock()).unwrap();
            r.sort();
            r
        };
        assert_eq!(rows(&core), ["wb-a", "wb-b", "wb-c"]);
        core.download_settle(vec!["wb-b".into()], vec![false]).unwrap();
        assert!(!wants_beats("wb-b"), "taken back");
        core.download_cancel_all().unwrap();
        assert!(!wants_beats("wb-a") && !wants_beats("wb-c"), "stopped");
        assert!(rows(&core).is_empty());
        core.download_queue(vec![song("wb-d")]).unwrap();
        core.download_want_beats(vec!["wb-d".into()]).unwrap();
        core.download_settle(vec!["wb-d".into()], vec![true]).unwrap();
        assert!(wants_beats("wb-d"), "downloaded: still to be read");
        core.download_beats_forget(&["wb-d".to_string()]).unwrap();
        assert!(!wants_beats("wb-d") && rows(&core).is_empty(), "read");
    }

    #[test]
    fn the_whole_library_is_queued_in_index_order() {
        let _turn = TURN.lock();
        let core = Core::new(String::new(), "t".into()).unwrap();
        let songs: Vec<crate::Song> = ["x", "y", "z"].map(song).to_vec();
        crate::db::index(&mut core.db.lock(), &[], &[], &songs).unwrap();
        core.download_queue(vec![songs[1].clone()]).unwrap();
        let q = core.download_queue_library().unwrap();
        assert_eq!((q.fresh, q.again), (vec!["x".to_string(), "z".into()], vec!["y".to_string()]));
        assert_eq!(core.downloads(false).unwrap().len(), 3);
    }

    #[test]
    fn an_earlier_process_queue_is_sorted_out() {
        let _turn = TURN.lock();
        let known = |id: &str, state| DownloadKnown { id: id.into(), state, length: 100, bytes: 50 };
        let pending = ["lost", "removing", "done", "failed", "queued"].map(String::from);
        let all = [known("removing", REMOVING), known("done", COMPLETED), known("failed", FAILED), known("queued", QUEUED), known("other", COMPLETED)];
        let (r, failed) = recovery(&pending, &all);
        assert_eq!(r.lost, ["lost", "removing"]);
        assert_eq!(r.finished, ["done"]);
        assert_eq!(failed, [("failed".to_string(), 100, 50)]);
        assert!(r.unfinished);
        assert!(!recovery(&pending[..1], &[]).0.unfinished);

        let core = Core::new(String::new(), "t".into()).unwrap();
        core.download_queue(vec![song("rc-a"), song("rc-b")]).unwrap();
        let r = core.download_recover(vec![known("rc-a", COMPLETED), known("rc-b", FAILED)]).unwrap();
        assert_eq!(r.finished, ["rc-a"]);
        assert_eq!(r.failed, [DownloadFailed { id: "rc-b".into(), progress: 0.5 }]);
        assert_eq!(core.downloads(true).unwrap().len(), 1, "recorded as finished");
        assert_eq!(download_phase("rc-b".into()), Phase::Failed.code());
    }

    #[test]
    fn the_table_is_known_from_memory_and_settles_in_one_go() {
        let _turn = TURN.lock();
        let core = Core::new(String::new(), "t".into()).unwrap();
        let state = |id: &str| core.held.lock().state(id);
        core.download_queue(vec![song("h-a"), song("h-b"), song("h-c"), song("h-d")]).unwrap();
        let v0 = core.download_counts();
        assert_eq!((v0.done, v0.pending, state("h-a"), state("h-x")), (0, 4, 1, 0));
        // In order: "h-b" finishes and then goes, "h-x" was never there.
        let ids = ["h-a", "h-b", "h-b", "h-x"].map(String::from).to_vec();
        core.download_settle(ids, vec![true, true, false, false]).unwrap();
        let v1 = core.download_counts();
        assert_eq!((v1.done, v1.pending, state("h-a"), state("h-b")), (1, 2, 2, 0));
        assert!(v1.version > v0.version);
        // The version is the process's, moved by any test's downloads running beside this one: the counts only.
        let again = core.download_counts();
        assert_eq!((again.done, again.pending), (v1.done, v1.pending), "asked again, the same answer");
        assert_eq!(core.downloads(true).unwrap().len(), 1);

        let mut gone = core.download_cancel_all().unwrap();
        gone.sort();
        assert_eq!(gone, ["h-c", "h-d"]);
        assert_eq!((core.download_counts().pending, core.downloads(false).unwrap().len(), state("h-a")), (0, 0, 2), "finished songs stay");
        // What an opened core reads is what was written.
        let c = core.db.lock();
        let again = Held::load(&c).unwrap();
        assert_eq!((again.done, again.ids.len()), (1, 1));
    }
}
