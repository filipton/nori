//! Download bookkeeping in the core's database. Progress tracking and reporting are nori-transfers'.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use crate::Core;

pub use nori_transfers::transfers::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Queues `songs` for download; see [`DownloadQueued`].
    pub fn download_queue(&self, songs: Vec<crate::Song>) -> crate::Result<DownloadQueued> {
        follow_quality();
        know(&songs);
        let rows = songs.into_iter().map(|s| {
            let json = serde_json::to_string(&s).unwrap_or_default();
            (s.id, json)
        });
        let mut c = self.db.lock();
        let q = queue_rows(&mut c, rows)?;
        self.held_queued(&q);
        Ok(q)
    }

    /// Queues every indexed song, in index order ([`Core::download_queue`]).
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

    /// Marks `ids` for the beat model once downloaded; stored, so it survives a process restart.
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

    /// Downloaded songs lacking a current analysis or (with `beats`) a beat model read, newest first; with
    /// `beats` they are also marked via [`Core::download_want_beats`].
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

impl Core {
    /// Unmarks `ids` for the beat model (read, or no longer wanted).
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

    fn held_queued(&self, q: &DownloadQueued) {
        let mut held = self.held.lock();
        for id in &q.fresh {
            held.queued(id);
        }
    }
}

/// Whether Download asks about the beat model ([`beats_offer`]).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_beats_offer() -> BeatsOffer {
    let (on, choice) = crate::settings_store::prefs(|p| (p.auto_mix && p.auto_mix_better_beats, p.download_beats));
    beats_offer(on && nori_player::automix::beats::AVAILABLE, choice)
}

/// The "ML beats for downloads" setting for a remembered answer.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_beats_remembered(yes: bool) -> nori_settings::settings::DownloadBeats {
    beats_remembered(yes)
}

/// What downloaded songs still wait for, for the notification; `now` is the platform clock.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_processing(now: i64) -> Option<Processing> {
    processing(now)
}

/// The ids marked for the beat model.
pub(crate) fn beats_wanted_rows(c: &rusqlite::Connection) -> crate::Result<Vec<String>> {
    let mut st = c.prepare("SELECT id FROM download_beats WHERE server=sid()")?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Records settled downloads in order: finished, or removed (`finished` false). One transaction.
    pub fn download_settle(&self, ids: Vec<String>, finished: Vec<bool>) -> crate::Result<()> {
        let gone_ids: Vec<String> = ids.iter().zip(&finished).filter(|(_, f)| !**f).map(|(id, _)| id.clone()).collect();
        // Read first: the in-memory ids are asked per list row and must not wait for the statements.
        let mut state: HashMap<&str, HeldState> = {
            let held = self.held.lock();
            ids.iter().map(|id| (id.as_str(), held.state(id))).collect()
        };
        let mut c = self.db.lock();
        let tx = c.transaction()?;
        {
            let mut done = tx.prepare_cached("UPDATE downloads SET done=1 WHERE server=sid() AND id=?1")?;
            let mut gone = tx.prepare_cached("DELETE FROM downloads WHERE server=sid() AND id=?1")?;
            let mut unwanted = tx.prepare_cached("DELETE FROM download_beats WHERE server=sid() AND id=?1")?;
            for id in &gone_ids {
                unwanted.execute([id])?;
            }
            for (id, f) in ids.iter().zip(&finished) {
                let now = state.get_mut(id.as_str()).expect("read above");
                match (*now, *f) {
                    (HeldState::Absent, _) | (HeldState::Done, true) => {}
                    (_, true) => {
                        done.execute([id])?;
                        *now = HeldState::Done;
                    }
                    (_, false) => {
                        gone.execute([id])?;
                        *now = HeldState::Absent;
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
        // After releasing the database: the tracker reads it under its own lock.
        want_beats(&gone_ids, false);
        Ok(())
    }

    /// Removes all unfinished downloads and their marks; returns their ids for the platform to stop.
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
        // After releasing the database: the tracker reads it under its own lock.
        with(|t| {
            for id in &ids {
                t.close(id);
                t.unmark(id);
            }
        });
        want_beats(&ids, false);
        Ok(ids)
    }

    /// Download counts, from memory.
    pub fn download_counts(&self) -> DownloadCounts {
        let held = self.held.lock();
        let done = held.done;
        DownloadCounts { done, pending: held.ids.len() as u32 - done, version: HELD_VERSION.load(Ordering::Relaxed) }
    }

    /// Reconciles the table with the platform's queue (`known`) after a restart: records finished songs,
    /// marks failed ones and reports what is left.
    pub fn download_recover(&self, known: Vec<DownloadKnown>) -> crate::Result<DownloadRecovery> {
        follow_quality();
        let pending = self.downloads(false)?;
        know(&pending);
        let pending: Vec<String> = pending.into_iter().map(|s| s.id).collect();
        let (mut r, failed) = recovery(&pending, &known);
        self.download_settle(r.finished.clone(), vec![true; r.finished.len()])?;
        r.failed = with(|t| {
            failed
                .into_iter()
                .map(|(id, length, bytes)| {
                    let estimate = t.info(&id).map_or(0, |i| i.estimate);
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

    /// The downloads screen: pending songs by state, oldest first, and this session's finished, newest first.
    pub fn download_sections(&self) -> crate::Result<DownloadSections> {
        let pending = self.downloads(false)?;
        // Only this session's finished songs (few), looked up one by one.
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

    /// The download tracker is process-wide: tests that use it take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn song(id: &str) -> Song {
        Song { id: id.into(), ..Default::default() }
    }

    #[test]
    fn a_busy_database_does_not_hold_up_download_reports() {
        let _turn = TURN.lock();
        let core = Core::new(String::new(), "t".into()).unwrap();
        core.download_queue(vec![Song { id: "busy".into(), size: 4_000_000, ..Default::default() }]).unwrap();
        let busy = core.db.lock();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            followed("busy", QUEUED, 0);
            tx.send(start_fraction("busy")).unwrap();
        });
        let fraction = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("answered while the database was busy");
        drop(busy);
        assert_eq!(fraction, 0.0, "its size known from the queue");
        removed("busy");
    }

    #[test]
    fn queue_adds_new_and_retries_unfinished() {
        let _turn = TURN.lock();
        let core = Core::new(String::new(), "t".into()).unwrap();
        let q = core.download_queue(vec![song("a"), song("b"), song("a")]).unwrap();
        assert_eq!((q.fresh, q.again), (vec!["a".to_string(), "b".into()], vec![]));
        core.download_done("a".into()).unwrap();
        let q = core.download_queue(vec![song("a"), song("b"), song("c")]).unwrap();
        assert_eq!(q.fresh, ["c"]);
        assert_eq!(q.again, ["b"]);
        // Listed newest first.
        let pending: Vec<String> = core.downloads(false).unwrap().into_iter().rev().map(|s| s.id).collect();
        assert_eq!(pending, ["b", "c"]);
    }

    #[test]
    fn beat_marks_last_until_read_or_removed() {
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
        assert!(!wants_beats("wb-b"));
        core.download_cancel_all().unwrap();
        assert!(!wants_beats("wb-a") && !wants_beats("wb-c"));
        assert!(rows(&core).is_empty());
        core.download_queue(vec![song("wb-d")]).unwrap();
        core.download_want_beats(vec!["wb-d".into()]).unwrap();
        core.download_settle(vec!["wb-d".into()], vec![true]).unwrap();
        assert!(wants_beats("wb-d"), "downloaded, not read yet");
        core.download_beats_forget(&["wb-d".to_string()]).unwrap();
        assert!(!wants_beats("wb-d") && rows(&core).is_empty(), "read");
    }

    #[test]
    fn library_queue_follows_index_order() {
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
    fn recovery_after_restart() {
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
        assert_eq!(core.downloads(true).unwrap().len(), 1);
        assert_eq!(download_phase("rc-b".into()), Some(Phase::Failed.shown()));
    }

    #[test]
    fn held_state_tracks_settle_and_cancel() {
        let _turn = TURN.lock();
        let core = Core::new(String::new(), "t".into()).unwrap();
        let state = |id: &str| core.held.lock().state(id);
        core.download_queue(vec![song("h-a"), song("h-b"), song("h-c"), song("h-d")]).unwrap();
        let v0 = core.download_counts();
        assert_eq!((v0.done, v0.pending, state("h-a"), state("h-x")), (0, 4, HeldState::Pending, HeldState::Absent));
        // "h-b" finishes then goes; "h-x" never existed.
        let ids = ["h-a", "h-b", "h-b", "h-x"].map(String::from).to_vec();
        core.download_settle(ids, vec![true, true, false, false]).unwrap();
        let v1 = core.download_counts();
        assert_eq!((v1.done, v1.pending, state("h-a"), state("h-b")), (1, 2, HeldState::Done, HeldState::Absent));
        assert!(v1.version > v0.version);
        assert_eq!(core.downloads(true).unwrap().len(), 1);

        let mut gone = core.download_cancel_all().unwrap();
        gone.sort();
        assert_eq!(gone, ["h-c", "h-d"]);
        assert_eq!((core.download_counts().pending, core.downloads(false).unwrap().len(), state("h-a")), (0, 0, HeldState::Done), "finished songs stay");
        // Reloading gives the same state.
        let c = core.db.lock();
        let again = Held::load(&c).unwrap();
        assert_eq!((again.done, again.ids.len()), (1, 1));
    }
}
