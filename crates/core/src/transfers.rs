//! Download bookkeeping in the core's database. Progress tracking and reporting are nori-transfers'.

use std::collections::{HashMap, HashSet};

use crate::Core;

pub use nori_transfers::transfers::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Queues `songs` for download; see [`DownloadQueued`].
    pub fn download_queue(&self, songs: Vec<crate::Song>) -> crate::Result<DownloadQueued> {
        self.downloads.with(|t| {
            t.follow_quality(self.download_kbps());
            t.know(&songs);
        });
        let rows = songs.into_iter().map(|s| {
            let json = serde_json::to_string(&s).unwrap_or_default();
            (s.id, json)
        });
        self.queue_downloads(rows)
    }

    /// Queues every indexed song, in index order ([`Core::download_queue`]).
    pub fn download_queue_library(&self) -> crate::Result<DownloadQueued> {
        self.downloads.with(|t| t.follow_quality(self.download_kbps()));
        let rows: Vec<(String, String)> = {
            let c = self.db.lock();
            let mut st = c.prepare("SELECT id, json FROM items WHERE server=sid() AND kind=?1 ORDER BY rowid")?;
            let rows = st.query_map([crate::db::SONG], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        self.queue_downloads(rows)
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
        self.downloads.with(|t| t.want_beats(&ids, true));
        Ok(())
    }

    /// Downloaded songs lacking a current analysis or (with `beats`) a beat model read, newest first; with
    /// `beats` they are also marked via [`Core::download_want_beats`].
    pub fn download_unanalysed(&self, beats: bool) -> crate::Result<Vec<String>> {
        let ids: Vec<String> = self.download_ids(true)?.into_iter().filter(|id| crate::queue::analysable(id)).collect();
        let mut missing: HashSet<String> = self.analysis_missing(ids.clone())?.into_iter().collect();
        if beats {
            missing.extend(self.analysis_neural_missing(ids.clone())?);
        }
        let ids: Vec<String> = ids.into_iter().filter(|id| missing.contains(id)).collect();
        if beats {
            self.download_want_beats(ids.clone())?;
        }
        Ok(ids)
    }

    /// Records settled downloads in order: finished, or removed (`finished` false). One transaction.
    pub fn download_settle(&self, ids: Vec<String>, finished: Vec<bool>) -> crate::Result<()> {
        let gone: Vec<String> = ids.iter().zip(&finished).filter(|(_, f)| !**f).map(|(id, _)| id.clone()).collect();
        // Read first: the in-memory table is asked per list row and must not wait for the statements.
        let mut state: HashMap<&str, HeldState> = {
            let held = self.downloads.held();
            ids.iter().map(|id| (id.as_str(), held.state(id))).collect()
        };
        {
            let mut c = self.db.lock();
            let tx = c.transaction()?;
            {
                let mut done = tx.prepare_cached("UPDATE downloads SET done=1 WHERE server=sid() AND id=?1")?;
                let mut delete = tx.prepare_cached("DELETE FROM downloads WHERE server=sid() AND id=?1")?;
                let mut unwanted = tx.prepare_cached("DELETE FROM download_beats WHERE server=sid() AND id=?1")?;
                for id in &gone {
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
                            delete.execute([id])?;
                            *now = HeldState::Absent;
                        }
                    }
                }
            }
            tx.commit()?;
        }
        let mut held = self.downloads.held();
        for (id, f) in ids.iter().zip(finished) {
            if f {
                held.finished(id);
            } else {
                held.removed(id);
            }
        }
        drop(held);
        self.downloads.with(|t| t.want_beats(&gone, false));
        Ok(())
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
        self.downloads.with(|t| t.want_beats(ids, false));
        Ok(())
    }

    fn download_kbps(&self) -> i32 {
        self.session.settings.prefs(|p| p.download.bit_rate)
    }

    fn queue_downloads(&self, rows: impl IntoIterator<Item = (String, String)>) -> crate::Result<DownloadQueued> {
        let q = queue_rows(&mut self.db.lock(), rows)?;
        let mut held = self.downloads.held();
        q.fresh.iter().for_each(|id| held.queued(id));
        Ok(q)
    }
}


/// The "ML beats for downloads" setting for a remembered answer.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_beats_remembered(yes: bool) -> nori_settings::settings::DownloadBeats {
    beats_remembered(yes)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Whether Download asks about the beat model ([`beats_offer`]).
    pub fn download_beats_offer(&self) -> BeatsOffer {
        let (on, choice) = self.session.settings.prefs(|p| (p.auto_mix && p.auto_mix_better_beats, p.download_beats));
        beats_offer(on && nori_player::automix::beats::AVAILABLE, choice)
    }

    /// What downloaded songs still wait for, for the notification; `now` is the platform clock.
    pub fn download_processing(&self, now: i64) -> Option<Processing> {
        self.downloads.with(|t| t.processing_at(now))
    }

    /// Resolves once a mark changed since the last [`Core::download_marks_changed`], or these downloads
    /// were let go, so the platform follows processing without polling.
    pub async fn download_marks_moved(&self) {
        self.downloads.marks_moved().await
    }

    /// The marks changed since the last call, with their phase now (None: removed).
    pub fn download_marks_changed(&self) -> DownloadMarks {
        self.downloads.with(Tracker::marks_changed)
    }

    /// Gives up stuck processing steps; `now` is the platform clock. Returns ms until the next deadline,
    /// -1 when nothing is processing.
    pub fn download_processing_expire(&self, now: i64) -> i64 {
        self.downloads.with(|t| t.expire_at(now))
    }

    /// The batch's speed (bytes/s) and seconds left, for checks.
    pub fn download_speed_eta(&self) -> Vec<i64> {
        let (speed, eta) = self.downloads.with(Tracker::speed_eta);
        vec![speed, eta]
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
            let mut held = self.downloads.held();
            ids.iter().for_each(|id| held.removed(id));
            ids
        };
        // After releasing the database: the tracker reads it under its own lock.
        self.downloads.with(|t| {
            ids.iter().for_each(|id| _ = t.forget(id));
            t.want_beats(&ids, false);
        });
        Ok(ids)
    }

    /// Download counts, from memory.
    pub fn download_counts(&self) -> DownloadCounts {
        self.downloads.held().counts()
    }

    /// Reconciles the table with the platform's queue (`known`) after a restart: records finished songs,
    /// marks failed ones and reports what is left.
    pub fn download_recover(&self, known: Vec<DownloadKnown>) -> crate::Result<DownloadRecovery> {
        let pending = self.downloads(false)?;
        self.downloads.with(|t| {
            t.follow_quality(self.download_kbps());
            t.know(&pending);
        });
        let pending: Vec<String> = pending.into_iter().map(|s| s.id).collect();
        let (mut r, failed) = recovery(&pending, &known);
        self.download_settle(r.finished.clone(), vec![true; r.finished.len()])?;
        r.failed = self.downloads.with(|t| failed.into_iter().map(|(id, length, bytes)| DownloadFailed { progress: t.failed_before(&id, length, bytes), id }).collect());
        Ok(r)
    }

    /// The downloads screen: pending songs by state, oldest first, and this session's finished, newest first.
    pub fn download_sections(&self) -> crate::Result<DownloadSections> {
        let pending = self.downloads(false)?;
        // Only this session's finished songs (few), looked up one by one.
        let recent = self.downloads.with(|t| t.saved_ids());
        let done: Vec<crate::Song> = {
            let c = self.db.lock();
            let mut st = c.prepare_cached("SELECT json FROM downloads WHERE server=sid() AND id=?1 AND done=1")?;
            recent.iter().filter_map(|id| st.query_row([id], |r| r.get::<_, String>(0)).ok()).filter_map(|j| serde_json::from_str(&j).ok()).collect()
        };
        let [active, queued, failed, finished] = self.downloads.with(|t| t.sections(&pending, &done, |s: &crate::Song| s.id.as_str()));
        Ok(DownloadSections { active, queued, failed, finished })
    }

    /// [`Core::download_sections`]' counts, without reading the songs.
    pub fn download_queue_counts(&self) -> crate::Result<DownloadQueueCounts> {
        let pending = self.download_ids(false)?;
        let recent = self.downloads.with(|t| t.saved_ids());
        let done: Vec<String> = {
            let held = self.downloads.held();
            recent.into_iter().filter(|id| held.state(id) == HeldState::Done).collect()
        };
        let [active, queued, failed, _] = self.downloads.with(|t| t.sections(&pending, &done, |id: &String| id.as_str()));
        Ok(DownloadQueueCounts { waiting: (active.len() + queued.len()) as u32, failed: failed.len() as u32 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Song;

    fn song(id: &str) -> Song {
        Song { id: id.into(), ..Default::default() }
    }

    fn core() -> std::sync::Arc<Core> {
        Core::new(String::new(), "t".into(), Default::default()).unwrap()
    }

    #[test]
    fn reports_skip_busy_database() {
        let core = core();
        core.download_queue(vec![Song { id: "busy".into(), size: 4_000_000, ..Default::default() }]).unwrap();
        let busy = core.db.lock();
        let (tx, rx) = std::sync::mpsc::channel();
        let downloads = core.downloads.clone();
        std::thread::spawn(move || {
            tx.send(downloads.with(|t| {
                t.followed("busy", QUEUED, 0);
                t.start_fraction("busy")
            }))
            .unwrap();
        });
        let fraction = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("answered while the database was busy");
        drop(busy);
        assert_eq!(fraction, 0.0, "its size known from the queue");
    }

    #[test]
    fn queueing() {
        {
            let core = core();
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

        // Library queue follows index order.
        {
            let core = core();
            let songs: Vec<crate::Song> = ["x", "y", "z"].map(song).to_vec();
            crate::db::index(&mut core.db.lock(), &[], &[], &songs).unwrap();
            core.download_queue(vec![songs[1].clone()]).unwrap();
            let q = core.download_queue_library().unwrap();
            assert_eq!((q.fresh, q.again), (vec!["x".to_string(), "z".into()], vec!["y".to_string()]));
            assert_eq!(core.downloads(false).unwrap().len(), 3);
        }
    }

    #[test]
    fn beat_marks_last_until_read() {
        let core = core();
        let wants = |id: &str| core.downloads.with(|t| t.wants_beats(id));
        core.download_queue(vec![song("a"), song("b"), song("c")]).unwrap();
        core.download_want_beats(vec!["a".into(), "b".into(), "c".into()]).unwrap();
        assert!(wants("a") && wants("b"));
        let rows = |core: &Core| {
            let c = core.db.lock();
            let mut st = c.prepare("SELECT id FROM download_beats ORDER BY id").unwrap();
            st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect::<Vec<_>>()
        };
        assert_eq!(rows(&core), ["a", "b", "c"]);
        let reopened = Downloads::load(&core.db).unwrap();
        assert!(reopened.with(|t| t.wants_beats("c")), "loaded with the table");
        core.download_settle(vec!["b".into()], vec![false]).unwrap();
        assert!(!wants("b"));
        core.download_cancel_all().unwrap();
        assert!(!wants("a") && !wants("c"));
        assert!(rows(&core).is_empty());
        core.download_queue(vec![song("d")]).unwrap();
        core.download_want_beats(vec!["d".into()]).unwrap();
        core.download_settle(vec!["d".into()], vec![true]).unwrap();
        assert!(wants("d"), "downloaded, not read yet");
        core.download_beats_forget(&["d".to_string()]).unwrap();
        assert!(!wants("d") && rows(&core).is_empty(), "read");
    }

    #[test]
    fn restart() {
        {
            let known = |id: &str, state| DownloadKnown { id: id.into(), state, length: 100, bytes: 50 };
            let pending = ["lost", "removing", "done", "failed", "queued"].map(String::from);
            let all = [known("removing", REMOVING), known("done", COMPLETED), known("failed", FAILED), known("queued", QUEUED), known("other", COMPLETED)];
            let (r, failed) = recovery(&pending, &all);
            assert_eq!(r.lost, ["lost", "removing"]);
            assert_eq!(r.finished, ["done"]);
            assert_eq!(failed, [("failed".to_string(), 100, 50)]);
            assert!(r.unfinished);
            assert!(!recovery(&pending[..1], &[]).0.unfinished);

            let core = core();
            core.download_queue(vec![song("a"), song("b")]).unwrap();
            let r = core.download_recover(vec![known("a", COMPLETED), known("b", FAILED)]).unwrap();
            assert_eq!(r.finished, ["a"]);
            assert_eq!(r.failed, [DownloadFailed { id: "b".into(), progress: 0.5 }]);
            assert_eq!(core.downloads(true).unwrap().len(), 1);
            assert_eq!(core.downloads.with(|t| t.phase("b")), Some(nori_model::DownloadPhase::Failed));
        }

        // Restart failure wakes downloads.
        {
            use std::future::Future;
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::Arc;
            struct Woke(AtomicBool);
            impl std::task::Wake for Woke {
                fn wake(self: Arc<Self>) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let core = core();
            core.download_queue(vec![song("a")]).unwrap();
            core.downloads.with(|t| t.marks_changed());
            let woke = Arc::new(Woke(AtomicBool::new(false)));
            let waker = std::task::Waker::from(woke.clone());
            let mut moved = std::pin::pin!(core.downloads.marks_moved());
            assert!(moved.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
            core.download_recover(vec![DownloadKnown { id: "a".into(), state: FAILED, length: 100, bytes: 50 }]).unwrap();
            assert!(woke.0.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn queue_counts_are_the_sections_sizes() {
        let core = core();
        core.download_queue(["a", "b", "c", "d", "e"].map(song).to_vec()).unwrap();
        core.downloads.with(|t| {
            t.followed("a", DOWNLOADING, 0);
            t.followed("b", FAILED, 0);
            t.followed("e", COMPLETED, 0);
        });
        core.download_settle(vec!["e".into()], vec![true]).unwrap();
        let s = core.download_sections().unwrap();
        let counts = core.download_queue_counts().unwrap();
        assert_eq!(counts, DownloadQueueCounts { waiting: (s.active.len() + s.queued.len()) as u32, failed: s.failed.len() as u32 });
        // "a" downloading, "e" saved and processed, "c" and "d" queued.
        assert_eq!(counts, DownloadQueueCounts { waiting: 4, failed: 1 });
    }

    #[test]
    fn held_state_tracks_settle_and_cancel() {
        let core = core();
        let state = |id: &str| core.downloads.held().state(id);
        core.download_queue(vec![song("a"), song("b"), song("c"), song("d")]).unwrap();
        let v0 = core.download_counts();
        assert_eq!((v0.done, v0.pending, state("a"), state("x")), (0, 4, HeldState::Pending, HeldState::Absent));
        // "b" finishes then goes; "x" never existed.
        let ids = ["a", "b", "b", "x"].map(String::from).to_vec();
        core.download_settle(ids, vec![true, true, false, false]).unwrap();
        let v1 = core.download_counts();
        assert_eq!((v1.done, v1.pending, state("a"), state("b")), (1, 2, HeldState::Done, HeldState::Absent));
        assert!(v1.version > v0.version);
        assert_eq!(core.downloads(true).unwrap().len(), 1);

        let mut gone = core.download_cancel_all().unwrap();
        gone.sort();
        assert_eq!(gone, ["c", "d"]);
        assert_eq!((core.download_counts().pending, core.downloads(false).unwrap().len(), state("a")), (0, 0, HeldState::Done), "finished songs stay");
        let reloaded = Downloads::load(&core.db).unwrap();
        assert_eq!(reloaded.held().counts().done, 1, "the table agrees");
    }
}
