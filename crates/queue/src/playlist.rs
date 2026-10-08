//! The app's queue (`nori_player::playlist::Playlist`). Every change is made here first; the platform's
//! player mirrors it from the returned [`QueueChange`] / [`QueueEdit`].

use nori_model::{PageOrigin, Song};
use nori_player::playlist::{Playlist, Splice, REPEAT_ALL, REPEAT_ONE};

// Public so the uniffi scaffolding can name them.
pub use nori_player::playlist::Hand;
pub use nori_player::queue::Onto;

use crate::{queue, Session};

/// The queue plus the state that follows it.
#[derive(Default)]
pub(crate) struct Queue {
    list: Playlist,
    /// The planner window last handed over and whether shuffling, to skip unchanged ones.
    window: (Vec<String>, bool),
    /// The page the queue was started from; kept through edits, replaced by each new queue.
    origin: Option<PageOrigin>,
    /// Bumped by each new queue, so pages re-check [`playlist_from`] only then.
    origin_gen: u32,
}

impl Queue {
    fn set_origin(&mut self, origin: Option<PageOrigin>) {
        self.origin = origin;
        self.origin_gen = self.origin_gen.wrapping_add(1);
    }

    fn change(&self, at: Option<usize>) -> QueueChange {
        QueueChange { at: at.map(|a| a as u32), shuffled: self.list.shuffle_order().is_some() }
    }
}

fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64)
}

/// Where a change landed, for the player to make the same change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QueueChange {
    /// The list index: the start song of a new queue, or where inserted songs went.
    pub at: Option<u32>,
    /// Shuffling: the player reads the play order with `PlaylistJni.order`.
    pub shuffled: bool,
}

/// A change for the player: remove the flattened (from, to) pairs, last first; insert `songs` at `at`;
/// then jump to `seek`; while `shuffled`, read the play order.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QueueEdit {
    pub remove: Vec<u32>,
    pub at: u32,
    pub songs: Vec<Song>,
    pub seek: Option<u32>,
    pub shuffled: bool,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BridgeState {
    pub bridging: bool,
    pub next_is_parked: bool,
    /// The song resumed once the server is back.
    pub parked: Option<String>,
    pub current: Option<String>,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum Hand {
    No,
    Next,
    Last,
    Bridge,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum Onto {
    Skip,
    Loop,
    Song,
}

/// The queue as the app lists it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PlaylistView {
    /// Empty when the reader already holds them (its `list_rev` matched).
    pub songs: Vec<Song>,
    /// Song count, whether sent or not.
    pub len: u32,
    /// Changes only when the listed songs change.
    pub list_rev: u64,
    /// List indexes in play order.
    pub order: Vec<u32>,
    /// Indexes of songs added by hand.
    pub queued: Vec<u32>,
    /// Current list index; -1 for none.
    pub index: i32,
    /// Shuffle shown as on.
    pub shuffle: bool,
    pub repeat: u8,
    pub bridging: bool,
    /// Changes whenever the list or its order does.
    pub rev: u64,
}

/// The repeat mode for walking neighbours: repeat one walks like repeat all.
fn walk_repeat(p: &Playlist) -> u8 {
    if p.repeat() == REPEAT_ONE { REPEAT_ALL } else { p.repeat() }
}

/// A song of a list and its neighbours in play order, as ReplayGain reads them.
struct Around {
    before: Option<String>,
    song: String,
    after: Option<String>,
    shuffling: bool,
}

impl Around {
    fn of(p: &Playlist, i: usize) -> Around {
        let id = |i: usize| p.ids()[i].clone();
        let repeat = walk_repeat(p);
        Around { before: p.previous_of(i, repeat).map(id), song: id(i), after: p.next_of(i, repeat).map(id), shuffling: p.shuffling() }
    }
}

/// Songs in the planner window from the current one on.
const WINDOW_LEN: usize = 8;

impl Session {
    /// Lends the queue to `f`.
    pub fn playlist<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R {
        f(&self.queue.lock().list)
    }

    /// The page the queue was started from, if any; saved with the queue (`Core::playlist_save`).
    pub fn origin(&self) -> Option<PageOrigin> {
        self.queue.lock().origin.clone()
    }

    /// Changes with every new queue (`PlaylistJni.origin`).
    pub fn origin_gen(&self) -> u32 {
        self.queue.lock().origin_gen
    }

    /// Whether the queue was started from `page` (same kind and id), not merely whether it holds its songs.
    pub fn from_page(&self, page: &nori_library::pages::PageQueue) -> bool {
        self.queue.lock().origin.as_ref() == Some(page.origin_ref())
    }

    fn edit(&self, f: impl FnOnce(&mut Playlist) -> Option<usize>) -> QueueChange {
        let mut q = self.queue.lock();
        let at = f(&mut q.list);
        q.change(at)
    }

    /// Applies the splice `f` returns, as a [`QueueEdit`] inserting `songs`.
    pub fn edit_splice(&self, f: impl FnOnce(&mut Playlist) -> Option<Splice>, songs: Vec<Song>) -> Option<QueueEdit> {
        let mut q = self.queue.lock();
        let s = f(&mut q.list)?;
        Some(QueueEdit {
            remove: s.remove.iter().flat_map(|&(a, b)| [a as u32, b as u32]).collect(),
            at: s.at as u32,
            songs,
            seek: s.seek.map(|i| i as u32),
            shuffled: q.list.shuffle_order().is_some(),
        })
    }

    /// The server is back: drops the offline bridge's songs and resumes the parked song. None when not bridging.
    pub fn unbridge(&self) -> Option<QueueEdit> {
        self.edit_splice(|p| p.unbridge(), Vec::new())
    }

    pub fn bridge_state(&self) -> BridgeState {
        self.playlist(|p| BridgeState { bridging: p.bridging(), next_is_parked: p.next_is_parked(), parked: p.parked_id().map(str::to_string), current: p.current_id().map(str::to_string) })
    }

    /// Sets a new queue starting at `start` (None: wherever shuffle starts), from page `origin`.
    pub fn set(&self, ids: Vec<String>, start: Option<u32>, shuffle: bool, origin: Option<PageOrigin>) -> QueueChange {
        let mut q = self.queue.lock();
        q.set_origin(origin);
        let at = q.list.set(ids, start.map(|s| s as usize), shuffle, seed());
        q.change(at)
    }

    /// Sets a queue already in play order (a weighted shuffle), shown as shuffled.
    pub fn set_ordered(&self, ids: Vec<String>, origin: Option<PageOrigin>) -> QueueChange {
        let mut q = self.queue.lock();
        q.set_origin(origin);
        let at = q.list.set_ordered(ids);
        q.change(at)
    }

    /// A queue handed over from another device: from list index `index`, under `shuffle` in `order`
    /// (shuffled here, from wherever that starts, when there is none), repeating as `repeat` says.
    pub fn handed(&self, ids: Vec<String>, index: u32, order: Option<Vec<u32>>, shuffle: bool, repeat: u8) -> QueueChange {
        let mut q = self.queue.lock();
        q.set_origin(None);
        let at = match (shuffle, order) {
            (true, Some(order)) => q.list.set_in_order(ids, index as usize, order.into_iter().map(|i| i as usize).collect(), seed()),
            (true, None) => q.list.set(ids, None, true, seed()),
            (false, _) => q.list.set(ids, Some(index as usize), false, seed()),
        };
        q.list.set_repeat(repeat);
        q.change(at)
    }

    /// Inserts songs near `at`, each marked with how it was added (`hands`); `Playlist::take` picks the
    /// spot.
    pub fn take(&self, at: u32, ids: Vec<String>, hands: Vec<Hand>) -> QueueChange {
        self.edit(|p| Some(p.take(at as usize, ids, &hands)))
    }

    /// Removes `from..to`; a single song is kept for [`Session::restore`]. Removing the current song
    /// plays the next.
    pub fn remove(&self, from: u32, to: u32) -> QueueChange {
        self.edit(|p| {
            p.remove_undoably(from as usize, to as usize);
            p.current()
        })
    }

    /// Undo: puts back `id` if it is the last song removed on its own (same index, shuffle turn and
    /// hand). `at` is None when it is not; the caller then inserts it itself.
    pub fn restore(&self, id: String) -> QueueChange {
        self.edit(|p| p.restore_taken(&id))
    }

    pub fn move_range(&self, from: u32, to: u32, new_index: u32) -> QueueChange {
        self.edit(|p| {
            p.move_range(from as usize, to as usize, new_index as usize);
            p.current()
        })
    }

    pub fn shuffle(&self, on: bool) -> QueueChange {
        self.edit(|p| {
            p.set_shuffle(on, seed());
            p.current()
        })
    }

    /// Shows shuffle as `on` right away, before the queue change that follows.
    pub fn show_shuffle(&self, on: bool) {
        self.queue.lock().list.show_shuffle(on);
    }

    /// Whether shuffle is shown as on (`Playlist::lit`).
    pub fn shuffle_shown(&self) -> bool {
        self.playlist(|p| p.lit())
    }

    /// The play order while shuffling (list indexes), lent to `f`; None when not shuffling.
    pub fn shuffle_order<R>(&self, f: impl FnOnce(Option<&[usize]>) -> R) -> R {
        self.playlist(|p| f(p.shuffle_order()))
    }

    /// Sets repeat (media3 numbering: off 0, one 1, all 2).
    pub fn repeat(&self, mode: u8) {
        self.queue.lock().list.set_repeat(mode);
    }

    /// The player moved to `index` on its own (song ended, seek to another song).
    pub fn moved_to(&self, index: i32) {
        if let Ok(i) = usize::try_from(index) {
            self.queue.lock().list.moved_to(i);
        }
    }

    /// Whether arriving on `index` of `list`, the queue as the player knows it, would skip it ("skip
    /// explicit songs"); asked before the song is read.
    pub fn skips(&self, list: &Playlist, index: usize) -> bool {
        let skip_explicit = self.settings.prefs(|p| p.skip_explicit);
        if !skip_explicit {
            return false;
        }
        let has_next = list.next_of(index, walk_repeat(list)).is_some();
        let explicit = list.ids().get(index).is_some_and(|id| self.explicit(id));
        nori_player::queue::arrival(true, skip_explicit, explicit, has_next, false) == Onto::Skip
    }

    /// Up to `n` upcoming ids in play order, the current one first.
    pub fn upcoming(&self, n: u32) -> Vec<String> {
        self.playlist(|p| p.upcoming().take(n as usize).map(|i| p.ids()[i].clone()).collect())
    }

    /// Hands the planner its window (previous song, current, then the next ones as walked, repeat
    /// included) if it changed. True when it did, so the platform asks for a new plan.
    pub fn window(&self) -> bool {
        let window = {
            let mut q = self.queue.lock();
            let p = &q.list;
            let mut ids = Vec::with_capacity(WINDOW_LEN + 1);
            let at = |i: usize| p.ids()[i].clone();
            if let Some(c) = p.current() {
                ids.extend(p.previous_of(c, p.repeat()).map(at));
                ids.extend(std::iter::successors(Some(c), |&x| p.next_of(x, p.repeat())).take(WINDOW_LEN).map(at));
            }
            let window = (ids, p.shuffling());
            if q.window == window {
                return false;
            }
            q.window = window.clone();
            window
        };
        // Outside the queue's lock: the planner may take the database's.
        self.hand_window(&window.0, window.1);
        true
    }

    /// The current song's ReplayGain volume (`Session::queue_gain`); `bit_perfect`: the output is untouched.
    pub fn gain(&self, bit_perfect: bool) -> f32 {
        let around = self.playlist(|p| p.current().map(|c| Around::of(p, c)));
        self.gain_around(around, bit_perfect)
    }

    /// [`Session::gain`] for index `index` of `list`, the queue as a player last took it (its indexes name
    /// that list's songs while the live one is edited), so it can set the next song's volume ahead of time.
    pub fn gain_of(&self, list: &Playlist, index: usize, bit_perfect: bool) -> f32 {
        self.gain_around((index < list.len()).then(|| Around::of(list, index)), bit_perfect)
    }

    fn gain_around(&self, around: Option<Around>, bit_perfect: bool) -> f32 {
        let (Some(s), Some(a)) = (self.settings.current(), around) else { return 1.0 };
        self.queue_gain(a.before, Some(a.song), a.after, &s.gain_prefs(), bit_perfect, a.shuffling)
    }

    /// The queue view; songs are omitted when `held` equals the current `list_rev`.
    pub fn view(&self, held: u64) -> PlaylistView {
        let (ids, mut view) = self.playlist(|p| {
            let ids = if p.list_rev() == held { Vec::new() } else { p.ids().to_vec() };
            let view = PlaylistView {
                songs: Vec::new(),
                len: p.len() as u32,
                list_rev: p.list_rev(),
                order: p.play_order().map(|i| i as u32).collect(),
                queued: p.by_hand().map(|i| i as u32).collect(),
                index: p.current().map_or(-1, |c| c as i32),
                shuffle: p.lit(),
                repeat: p.repeat(),
                bridging: p.bridging(),
                rev: p.rev(),
            };
            (ids, view)
        });
        if !ids.is_empty() {
            view.songs = self.songs(ids);
        }
        view
    }

    /// [`Session::view`] only if the queue has `len` songs: while the platform player trails the core
    /// after a change, its index must not be paired with the core's songs (use [`Session::view_of`] then).
    pub fn view_for(&self, held: u64, len: u32) -> Option<PlaylistView> {
        let v = self.view(held);
        (v.len == len).then_some(v)
    }

    /// A view built from the platform player's own list while it trails the core's: `ids`, `hands` and
    /// its play `order` (the list order if `order` is not a permutation). Songs are always sent;
    /// `list_rev` 0 and `rev` `u64::MAX` keep readers from mistaking it for the core's list.
    pub fn view_of(&self, ids: Vec<String>, hands: Vec<Hand>, order: Vec<u32>) -> PlaylistView {
        let len = ids.len();
        let mut seen = vec![false; len];
        let permutation = order.len() == len && order.iter().all(|&i| seen.get_mut(i as usize).is_some_and(|s| !std::mem::replace(s, true)));
        let order = if permutation { order } else { (0..len as u32).collect() };
        let queued = (0..len as u32).filter(|&i| hands.get(i as usize).is_some_and(|h| *h != Hand::No)).collect();
        let (shuffle, repeat, bridging) = self.playlist(|p| (p.lit(), p.repeat(), p.bridging()));
        let songs = if ids.is_empty() { Vec::new() } else { self.songs(ids) };
        PlaylistView { songs, len: len as u32, list_rev: 0, order, queued, index: -1, shuffle, repeat, bridging, rev: u64::MAX }
    }

    /// Changes whenever the list or its order does.
    pub fn rev(&self) -> u64 {
        self.playlist(|p| p.rev())
    }

    /// The ids to save as the server's play queue: radio streams left out, empty unless scrobbling is on.
    pub fn to_push(&self) -> Vec<String> {
        if !self.settings.prefs(|p| p.scrobble) {
            return Vec::new();
        }
        self.playlist(|p| p.ids().iter().filter(|id| !queue::is_radio(id)).cloned().collect())
    }

    /// The server write saving the play queue at `current`/`position_ms`; None when there is nothing to save.
    pub fn push_write(&self, current: Option<String>, position_ms: i64) -> Option<nori_net::requests::Write> {
        let ids = self.to_push();
        (!ids.is_empty()).then_some(nori_net::requests::Write::SaveQueue { ids, current, position_ms })
    }

    /// The current id and all queued ids.
    pub fn snapshot(&self) -> (Option<String>, Vec<String>) {
        self.playlist(|p| (p.current_id().map(str::to_string), p.ids().to_vec()))
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Session {
    /// [`Session::from_page`].
    pub fn playlist_from(&self, page: std::sync::Arc<nori_library::pages::PageQueue>) -> bool {
        self.from_page(&page)
    }

    /// [`Session::unbridge`].
    pub fn playlist_unbridge(&self) -> Option<QueueEdit> {
        self.unbridge()
    }

    /// [`Session::bridge_state`].
    pub fn playlist_bridge_state(&self) -> BridgeState {
        self.bridge_state()
    }

    /// [`Session::set`].
    pub fn playlist_set(&self, ids: Vec<String>, start: Option<u32>, shuffle: bool, origin: Option<PageOrigin>) -> QueueChange {
        self.set(ids, start, shuffle, origin)
    }

    /// [`Session::set_ordered`].
    pub fn playlist_set_ordered(&self, ids: Vec<String>, origin: Option<PageOrigin>) -> QueueChange {
        self.set_ordered(ids, origin)
    }

    /// [`Session::handed`].
    pub fn playlist_handed(&self, ids: Vec<String>, index: u32, order: Option<Vec<u32>>, shuffle: bool, repeat: u8) -> QueueChange {
        self.handed(ids, index, order, shuffle, repeat)
    }

    /// [`Session::take`].
    pub fn playlist_take(&self, at: u32, ids: Vec<String>, hands: Vec<Hand>) -> QueueChange {
        self.take(at, ids, hands)
    }

    /// [`Session::remove`].
    pub fn playlist_remove(&self, from: u32, to: u32) -> QueueChange {
        self.remove(from, to)
    }

    /// [`Session::restore`].
    pub fn playlist_restore(&self, id: String) -> QueueChange {
        self.restore(id)
    }

    /// [`Session::move_range`].
    pub fn playlist_move(&self, from: u32, to: u32, new_index: u32) -> QueueChange {
        self.move_range(from, to, new_index)
    }

    /// [`Session::shuffle`].
    pub fn playlist_shuffle(&self, on: bool) -> QueueChange {
        self.shuffle(on)
    }

    /// [`Session::show_shuffle`].
    pub fn playlist_show_shuffle(&self, on: bool) {
        self.show_shuffle(on)
    }

    /// [`Session::repeat`].
    pub fn playlist_repeat(&self, mode: u8) {
        self.repeat(mode)
    }

    /// [`Session::gain`].
    pub fn playlist_gain(&self, bit_perfect: bool) -> f32 {
        self.gain(bit_perfect)
    }

    /// [`Session::view_for`].
    pub fn playlist_view_for(&self, held: u64, len: u32) -> Option<PlaylistView> {
        self.view_for(held, len)
    }

    /// [`Session::view_of`].
    pub fn playlist_view_of(&self, ids: Vec<String>, hands: Vec<Hand>, order: Vec<u32>) -> PlaylistView {
        self.view_of(ids, hands, order)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nori_model::OriginKind::{self, Album, Artist, Playlist as PlaylistKind};

    /// A session playing `ids` from `start`.
    pub(crate) fn session(ids: &[&str], start: u32) -> Session {
        let s = Session::default();
        s.set(ids.iter().map(|s| s.to_string()).collect(), Some(start), false, None);
        s
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn window_ids(s: &Session) -> Vec<String> {
        s.queue.lock().window.0.clone()
    }

    fn page(kind: OriginKind, id: &str) -> std::sync::Arc<nori_library::pages::PageQueue> {
        nori_library::pages::PageQueue::new(PageOrigin::new(kind, id))
    }

    #[test]
    fn views() {
        let s = session(&["w1", "w2", "w3"], 1);
        s.window();
        assert!(!s.window());
        s.moved_to(2);
        assert!(s.window());
        assert_eq!(window_ids(&s), ids(&["w2", "w3"]));
        s.repeat(2);
        assert!(s.window());
        assert_eq!(window_ids(&s), ids(&["w2", "w3", "w1", "w2", "w3", "w1", "w2", "w3", "w1"]));

        // View resends songs only when list changed.
        let s = session(&["v1", "v2", "v3"], 0);
        let first = s.view(0);
        assert_eq!((first.songs.len(), first.len), (3, 3));
        s.shuffle(true);
        let shuffled = s.view(first.list_rev);
        assert!(shuffled.songs.is_empty());
        assert_eq!((shuffled.len, shuffled.list_rev), (3, first.list_rev));
        assert_ne!(shuffled.rev, first.rev);
        s.take(9, ids(&["v4"]), vec![Hand::Next]);
        let added = s.view(first.list_rev);
        assert_eq!((added.songs.len(), added.len), (4, 4));
        assert_ne!(added.list_rev, first.list_rev);
        assert!(s.view_for(0, 3).is_none(), "the player trails the core's list");
        assert_eq!(s.view_for(0, 4).map(|v| v.songs.len()), Some(4));

        // View of player list.
        let s = session(&["p1"], 0);
        let v = s.view_of(ids(&["p1", "p2", "p3"]), vec![Hand::No, Hand::Next, Hand::Last], vec![2, 0, 1]);
        assert_eq!((v.songs.len(), v.len, v.order.as_slice(), v.queued.as_slice()), (3, 3, &[2u32, 0, 1][..], &[1u32, 2][..]));
        assert_eq!((v.list_rev, v.rev), (0, u64::MAX));
        for bad in [vec![], vec![0, 0, 1], vec![0, 1, 3], vec![0, 1]] {
            assert_eq!(s.view_of(ids(&["p1", "p2", "p3"]), vec![], bad.clone()).order, [0, 1, 2], "{bad:?}");
        }
        assert!(s.view_of(ids(&["p1", "p2"]), vec![], vec![]).queued.is_empty());
    }

    /// A session over settings opened in a directory of its own, changed by `change`.
    fn opened(change: impl FnOnce(&mut nori_settings::settings::StoredPrefs)) -> (nori_testdir::TempDir, Session) {
        let dir = nori_testdir::TempDir::new("session");
        let settings = nori_settings::settings_store::Settings::new();
        let mut p = settings.open(&dir.join("app.db").to_string_lossy()).unwrap();
        change(&mut p);
        settings.put(p);
        (dir, Session::new(settings))
    }

    #[test]
    fn explicit_songs_skipped_when_asked() {
        let explicit = |id: &str| Song { explicit_status: "explicit".into(), ..Song::only_id(id.to_string()) };
        let (_dir, s) = opened(|p| p.skip_explicit = true);
        s.register(vec![explicit("e1"), Song::only_id("c".into()), explicit("e2")]);
        s.set(ids(&["e1", "c", "e2"]), Some(1), false, None);
        let skips = |s: &Session, i: usize| s.skips(&s.playlist(nori_player::playlist::Playlist::clone), i);
        assert_eq!((skips(&s, 0), skips(&s, 1), skips(&s, 2)), (true, false, false), "the last explicit song plays: nothing comes after it");
        s.repeat(REPEAT_ONE);
        assert!(skips(&s, 2), "repeating, the queue goes on past its end");
        let (_dir, s) = opened(|_| {});
        s.register(vec![explicit("e1"), Song::only_id("c".into())]);
        s.set(ids(&["e1", "c"]), Some(1), false, None);
        assert!(!skips(&s, 0), "the setting off");
    }

    #[test]
    fn replay_gain_by_album_order() {
        use nori_player::policy::GainMode;
        let rg = nori_model::ReplayGain { track_gain: Some(-6.0), album_gain: Some(-2.0), track_peak: Some(0.5), album_peak: Some(0.5), ..Default::default() };
        let song = |id: &str, track: u32| Song { duration: 200, album_id: Some("GA".into()), track, disc_number: 1, replay_gain: Some(rg.clone()), ..Song::only_id(id.to_string()) };
        let db = |g: f32| (20.0 * g.log10() * 10.0).round() / 10.0;
        let (_dir, s) = opened(|p| {
            p.replay_gain = GainMode::Auto;
            p.preamp_db = 0.0;
        });
        s.register(vec![song("ga1", 1), song("ga2", 2), song("ga3", 3)]);
        s.set(ids(&["ga1", "ga2", "ga3"]), Some(0), false, None);
        let list = s.playlist(nori_player::playlist::Playlist::clone);
        assert_eq!([0, 1, 2].map(|i| db(s.gain_of(&list, i, false))), [-2.0; 3], "the album played in order");
        assert_eq!(db(s.gain(false)), -2.0, "the current song's");
        assert_eq!(s.gain(true), 1.0, "bit-perfect: untouched");
        s.set(ids(&["ga3", "ga1"]), Some(0), false, None);
        let list = s.playlist(nori_player::playlist::Playlist::clone);
        assert_eq!([0, 1].map(|i| db(s.gain_of(&list, i, false))), [-6.0; 2], "out of order: each song's own");
        s.set(ids(&["radio:1"]), Some(0), false, None);
        assert_eq!(s.gain(false), 1.0, "a station");
        let (_dir, s) = opened(|p| p.replay_gain = GainMode::Off);
        s.register(vec![song("ga1", 1)]);
        s.set(ids(&["ga1"]), Some(0), false, None);
        assert_eq!(s.gain(false), 1.0, "off");
    }

    #[test]
    fn replay_gain_of_the_list_the_player_holds() {
        use nori_player::policy::GainMode;
        let song = |id: &str, track_gain: f32| Song { duration: 200, replay_gain: Some(nori_model::ReplayGain { track_gain: Some(track_gain), ..Default::default() }), ..Song::only_id(id.to_string()) };
        let db = |g: f32| (20.0 * g.log10() * 10.0).round() / 10.0;
        let (_dir, s) = opened(|p| {
            p.replay_gain = GainMode::Track;
            p.preamp_db = 0.0;
        });
        s.register(vec![song("a", -2.0), song("b", -6.0), song("c", -4.0)]);
        s.set(ids(&["a", "b", "c"]), Some(0), false, None);
        let held = s.playlist(nori_player::playlist::Playlist::clone);
        // Edited before the player takes it: index 1 of the live queue is "c" now.
        s.remove(0, 1);
        assert_eq!(db(s.gain_of(&held, 1, false)), -6.0, "index 1 of the list the player holds is still \"b\"");
    }

    #[test]
    fn origin_follows_queue() {
        // A song tapped on an album page while a playlist plays makes the album's page the playing one.
        use nori_library::pages::PageQueue;
        let s = session(&[], 0);
        let (playlist, album) = (PageOrigin::new(PlaylistKind, "pl-t"), PageOrigin::new(Album, "al-t"));
        let lights = |o: &PageOrigin| s.from_page(&PageQueue::new(o.clone()));
        s.set(ids(&["p1", "t2", "p3"]), Some(1), false, Some(playlist.clone()));
        assert!(lights(&playlist) && !lights(&album));
        let gen = s.origin_gen();
        s.set(ids(&["t1", "t2", "t3"]), Some(1), false, Some(album.clone()));
        assert_ne!(s.origin_gen(), gen);
        assert!(lights(&album) && !lights(&playlist));
        s.set(ids(&["t1", "t2", "t3"]), Some(2), false, Some(album.clone()));
        assert_eq!(s.playlist(|p| p.current_id().map(str::to_string)), Some("t3".into()));
        // Edits keep the origin.
        s.take(3, ids(&["auto1", "auto2"]), vec![Hand::No; 2]);
        s.take(9, ids(&["mine"]), vec![Hand::Next]);
        s.remove(0, 1);
        s.move_range(0, 1, 2);
        s.shuffle(true);
        s.shuffle(false);
        assert!(lights(&album) && !lights(&playlist));

        // Origin survives edits.
        let s = session(&["o0"], 0);
        let (a, b, album, artist) = (page(PlaylistKind, "A"), page(PlaylistKind, "B"), page(Album, "al"), page(Artist, "ar"));
        let gen = s.origin_gen();
        s.set(ids(&["a1", "shared", "a3"]), Some(1), false, Some(a.origin()));
        assert_ne!(s.origin_gen(), gen);
        assert!(s.from_page(&a));
        assert!(!s.from_page(&b), "shares the song but did not start the queue");
        assert!(!s.from_page(&album) && !s.from_page(&artist));
        assert!(!s.from_page(&page(Album, "A")), "same id, other kind");

        s.set(ids(&["shared", "al2"]), Some(0), false, Some(album.origin()));
        assert!(s.from_page(&album) && !s.from_page(&a));

        let gen = s.origin_gen();
        s.take(9, ids(&["x"]), vec![Hand::Next]);
        s.take(9, ids(&["fill1", "fill2"]), vec![Hand::No; 2]);
        s.move_range(0, 1, 2);
        s.remove(1, 2);
        s.shuffle(true);
        s.moved_to(1);
        assert!(s.from_page(&album));
        assert_eq!(s.origin_gen(), gen);

        s.set_ordered(ids(&["r1", "r2"]), Some(artist.origin()));
        assert!(s.from_page(&artist) && !s.from_page(&album));
        s.set(ids(&["radio:1"]), Some(0), false, None);
        assert_eq!(s.origin(), None);

        // Restore keeps origin and shuffle turn.
        let s = session(&["o0"], 0);
        let album = page(Album, "al");
        s.set(ids(&["s1", "s2", "s3", "s4", "s5"]), Some(0), true, Some(album.origin()));
        let gen = s.origin_gen();
        let order = s.playlist(|p| p.play_order().collect::<Vec<_>>());
        let i = order[2];
        let id = s.playlist(|p| p.ids()[i].clone());
        s.remove(i as u32, i as u32 + 1);
        assert_eq!(s.restore(id), QueueChange { at: Some(i as u32), shuffled: true });
        assert_eq!(s.playlist(|p| p.play_order().collect::<Vec<_>>()), order);
        assert!(s.from_page(&album));
        assert_eq!(s.origin_gen(), gen);
    }

    #[test]
    fn edits_and_restores() {
        let s = session(&["e1", "e2", "e3"], 0);
        assert_eq!(s.take(9, ids(&["n"]), vec![Hand::Next]), QueueChange { at: Some(1), shuffled: false });
        assert_eq!(s.take(9, ids(&["i"]), vec![Hand::No]).at, Some(4), "clamped to the end");
        assert!(s.shuffle(true).shuffled);
        assert_eq!(s.playlist(|p| p.shuffle_order().unwrap()[..2].to_vec()), [0, 1], "current, then the one added by hand");
        let v = s.view(0);
        assert_eq!((v.index, v.queued.as_slice(), v.shuffle), (0, &[1u32][..], true));
        assert_eq!(v.songs[1].id, "n");

        // Removed song restores once.
        let s = session(&["u1", "u2", "u3", "u4"], 1);
        s.take(9, ids(&["mine"]), vec![Hand::Next]);
        assert_eq!(s.playlist(|p| p.ids().to_vec()), ids(&["u1", "u2", "mine", "u3", "u4"]));
        s.remove(2, 3);
        assert_eq!(s.restore("other".into()).at, None);
        assert_eq!(s.restore("mine".into()), QueueChange { at: Some(2), shuffled: false });
        assert_eq!(s.playlist(|p| (p.ids().to_vec(), p.current(), p.hand(2))), (ids(&["u1", "u2", "mine", "u3", "u4"]), Some(1), Hand::Next));
        assert_eq!(s.restore("mine".into()).at, None, "only once");

        // Only the last single removal.
        s.remove(0, 1);
        s.remove(1, 2);
        assert_eq!(s.restore("u1".into()).at, None);
        assert_eq!(s.restore("mine".into()).at, Some(1));
        s.remove(0, 2);
        assert_eq!(s.restore("u2".into()).at, None, "a range is not undoable");

        // Removing the current song plays the next; restoring keeps that one current.
        s.set(ids(&["p1", "p2", "p3"]), Some(1), false, None);
        s.remove(1, 2);
        assert_eq!(s.playlist(|p| p.current_id().map(str::to_string)).as_deref(), Some("p3"));
        assert_eq!(s.restore("p2".into()).at, Some(1));
        let v = s.view(0);
        assert_eq!((v.index, v.len), (2, 3));

        s.remove(0, 1);
        s.set(ids(&["n1", "p1"]), Some(0), false, None);
        assert_eq!(s.restore("p1".into()).at, None, "a new queue forgets it");
    }

    #[test]
    fn shuffle_shown_until_turned_off() {
        let s = session(&["s1", "s2"], 0);
        assert!(!s.shuffle_shown());
        s.show_shuffle(true);
        assert!(s.shuffle_shown());
        s.set_ordered(ids(&["s2", "s1"]), None);
        assert!(s.shuffle_shown(), "a weighted shuffle stays lit");
        s.take(0, ids(&["s3"]), vec![Hand::Last]);
        assert!(s.shuffle_shown());
        s.show_shuffle(false);
        s.shuffle(false);
        assert!(!s.shuffle_shown());
    }

    #[test]
    fn push_leaves_out_radio() {
        let s = session(&["t1", "t2", "radio:9"], 0);
        match s.push_write(Some("t2".into()), 7) {
            Some(nori_net::requests::Write::SaveQueue { ids: pushed, current, position_ms }) => {
                assert_eq!((pushed, current.as_deref(), position_ms), (ids(&["t1", "t2"]), Some("t2"), 7));
            }
            w => panic!("{w:?}"),
        }
    }
}
