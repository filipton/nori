//! The app's queue (`nori_player::playlist::Playlist`). Every change is made here first; the platform's
//! player mirrors it from the returned [`QueueChange`] / [`QueueEdit`].

use nori_model::{OriginKind, PageOrigin, Song};
use nori_player::playlist::{Playlist, Splice, REPEAT_ALL, REPEAT_ONE};
use parking_lot::Mutex;

// Public so the uniffi scaffolding can name them.
pub use nori_player::playlist::Hand;
pub use nori_player::queue::Onto;

use crate::queue;

/// The queue plus the state that follows it.
struct Queue {
    list: Playlist,
    /// The planner window last handed over (id, album run) and whether shuffling, to skip unchanged ones.
    window: (Vec<(String, u32)>, bool),
    /// Album runs for a saved queue about to be restored ([`playlist_put_back_runs`]).
    put_back: Option<(Vec<String>, Vec<u32>)>,
    /// The page the queue was started from; kept through edits, replaced by each new queue.
    origin: Option<PageOrigin>,
    /// Bumped by each new queue, so pages re-check [`playlist_from`] only then.
    origin_gen: u32,
}

// Global: the uniffi/JNI entry points have no handle to carry it.
static QUEUE: Mutex<Queue> = Mutex::new(Queue { list: Playlist::new(), window: (Vec::new(), false), put_back: None, origin: None, origin_gen: 0 });

impl Queue {
    fn set_origin(&mut self, origin: Option<PageOrigin>) {
        self.origin = origin;
        self.origin_gen = self.origin_gen.wrapping_add(1);
    }

    fn change(&self, at: Option<usize>) -> QueueChange {
        QueueChange { at: at.map(|a| a as u32), shuffled: self.list.shuffle_order().is_some() }
    }
}

/// The page the queue was started from, if any; saved with the queue (`Core::playlist_save`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_origin() -> Option<PageOrigin> {
    QUEUE.lock().origin.clone()
}

/// Changes with every new queue (`PlaylistJni.origin`).
pub fn playlist_origin_gen() -> u32 {
    QUEUE.lock().origin_gen
}

/// Whether the queue was started from `page` (same kind and id), not merely whether it holds its songs.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_from(page: std::sync::Arc<nori_library::pages::PageQueue>) -> bool {
    QUEUE.lock().origin.as_ref() == Some(page.origin_ref())
}

/// Lends the queue to `f`.
pub fn with<R>(f: impl FnOnce(&Playlist) -> R) -> R {
    f(&QUEUE.lock().list)
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

fn edit(f: impl FnOnce(&mut Playlist) -> Option<usize>) -> QueueChange {
    let mut q = QUEUE.lock();
    let at = f(&mut q.list);
    q.change(at)
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

/// Applies the splice `f` returns, as a [`QueueEdit`] inserting `songs`.
pub fn edit_splice(f: impl FnOnce(&mut Playlist) -> Option<Splice>, songs: Vec<Song>) -> Option<QueueEdit> {
    let mut q = QUEUE.lock();
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
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_unbridge() -> Option<QueueEdit> {
    edit_splice(|p| p.unbridge(), Vec::new())
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_bridge_state() -> BridgeState {
    with(|p| BridgeState { bridging: p.bridging(), next_is_parked: p.next_is_parked(), parked: p.parked_id().map(str::to_string), current: p.current_id().map(str::to_string) })
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

/// Sets a new queue starting at `start` (None: wherever shuffle starts), from page `origin`.
///
/// Album runs (`Playlist::album_run`): an album origin makes the whole queue one run, a "shuffle
/// albums" origin makes each album its own run, anything else (playlists included) has none. A queue
/// restored after [`playlist_put_back_runs`] takes its saved runs.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_set(ids: Vec<String>, start: Option<u32>, shuffle: bool, origin: Option<PageOrigin>) -> QueueChange {
    let kind = origin.as_ref().map(|o| o.kind);
    let spans = (kind == Some(OriginKind::ShuffleAlbums)).then(|| album_spans(&ids));
    let mut q = QUEUE.lock();
    let saved = q.put_back.take().filter(|(put, _)| *put == ids).map(|(_, runs)| runs);
    q.set_origin(origin);
    let at = q.list.set(ids, start.map(|s| s as usize), shuffle, seed());
    match saved {
        Some(runs) => q.list.set_album_runs(&runs),
        None if kind == Some(OriginKind::Album) => {
            let len = q.list.len();
            q.list.as_album(0, len)
        }
        None => spans.into_iter().flatten().for_each(|(from, to)| q.list.as_album(from, to)),
    }
    q.change(at)
}

/// The (from, to) spans of adjacent songs sharing an album, from the song store; songs without an album
/// are in none.
fn album_spans(ids: &[String]) -> Vec<(usize, usize)> {
    let albums: Vec<Option<String>> = queue::with(|s| ids.iter().map(|id| s.songs.get(id).and_then(|(song, _)| song.album_id.clone())).collect());
    let mut spans = Vec::new();
    let mut from = 0;
    for i in 1..=albums.len() {
        if i == albums.len() || albums[i] != albums[from] {
            if albums[from].is_some() {
                spans.push((from, i));
            }
            from = i;
        }
    }
    spans
}

/// Saved album `runs` (one per song) for the saved queue `ids`; the next [`playlist_set`] of exactly
/// those ids takes them.
pub fn playlist_put_back_runs(ids: Vec<String>, runs: Vec<u32>) {
    QUEUE.lock().put_back = (ids.len() == runs.len()).then_some((ids, runs));
}

/// Sets a queue already in play order (a weighted shuffle), shown as shuffled, with no album runs.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_set_ordered(ids: Vec<String>, origin: Option<PageOrigin>) -> QueueChange {
    let mut q = QUEUE.lock();
    q.set_origin(origin);
    let at = q.list.set_ordered(ids);
    q.change(at)
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum Hand {
    No,
    Next,
    Last,
    Bridge,
}

/// Inserts songs near `at`, each marked with how it was added (`hands`); `Playlist::take` picks the spot.
/// `from` is set when they are a whole album (one new album run) or a "shuffle albums" refill (one run
/// per album).
#[cfg_attr(feature = "ffi", uniffi::export(default(from = None)))]
pub fn playlist_take(at: u32, ids: Vec<String>, hands: Vec<Hand>, from: Option<PageOrigin>) -> QueueChange {
    let count = ids.len();
    let kind = from.as_ref().map(|o| o.kind);
    let spans = (kind == Some(OriginKind::ShuffleAlbums)).then(|| album_spans(&ids));
    edit(|p| {
        let at = p.take(at as usize, ids, &hands);
        if kind == Some(OriginKind::Album) && count > 0 {
            p.as_album(at, at + count);
        }
        spans.into_iter().flatten().for_each(|(from, to)| p.as_album(at + from, at + to));
        Some(at)
    })
}

/// Removes `from..to`; a single song is kept for [`playlist_restore`]. Removing the current song plays the next.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_remove(from: u32, to: u32) -> QueueChange {
    edit(|p| {
        p.remove_undoably(from as usize, to as usize);
        p.current()
    })
}

/// Undo: puts back `id` if it is the last song removed on its own (same index, shuffle turn and hand).
/// `at` is None when it is not; the caller then inserts it itself.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_restore(id: String) -> QueueChange {
    edit(|p| p.restore_taken(&id))
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_move(from: u32, to: u32, new_index: u32) -> QueueChange {
    edit(|p| {
        p.move_range(from as usize, to as usize, new_index as usize);
        p.current()
    })
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_shuffle(on: bool) -> QueueChange {
    edit(|p| {
        p.set_shuffle(on, seed());
        p.current()
    })
}

/// Shows shuffle as `on` right away, before the queue change that follows.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_show_shuffle(on: bool) {
    QUEUE.lock().list.show_shuffle(on);
}

/// Whether shuffle is shown as on (`Playlist::lit`).
pub fn playlist_shuffle_shown() -> bool {
    with(|p| p.lit())
}

/// The play order while shuffling (list indexes), lent to `f`; None when not shuffling.
pub fn playlist_shuffle_order<R>(f: impl FnOnce(Option<&[usize]>) -> R) -> R {
    with(|p| f(p.shuffle_order()))
}

/// Sets repeat (media3 numbering: off 0, one 1, all 2).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_repeat(mode: u8) {
    QUEUE.lock().list.set_repeat(mode);
}

/// The player moved to `index` on its own (song ended, seek to another song).
pub fn playlist_moved_to(index: i32) {
    if let Ok(i) = usize::try_from(index) {
        QUEUE.lock().list.moved_to(i);
    }
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum Onto {
    Skip,
    Loop,
    Song,
}

/// The repeat mode for walking neighbours: repeat one walks like repeat all.
fn walk_repeat(p: &Playlist) -> u8 {
    if p.repeat() == REPEAT_ONE { REPEAT_ALL } else { p.repeat() }
}

/// Whether arriving on `index` would skip it ("skip explicit songs"); asked before the song is read.
pub fn playlist_skips(index: usize) -> bool {
    let skip_explicit = crate::rules::prefs(|p| p.skip_explicit);
    if !skip_explicit {
        return false;
    }
    let (id, has_next) = with(|p| (p.ids().get(index).cloned(), p.next_of(index, walk_repeat(p)).is_some()));
    let explicit = id.is_some_and(|id| queue::queue_flags(id) & queue::EXPLICIT != 0);
    nori_player::queue::arrival(true, skip_explicit, explicit, has_next, false) == Onto::Skip
}

/// Up to `n` upcoming ids in play order, the current one first.
pub fn playlist_upcoming(n: u32) -> Vec<String> {
    with(|p| p.upcoming().take(n as usize).map(|i| p.ids()[i].clone()).collect())
}

/// Songs in the planner window from the current one on.
const WINDOW_LEN: usize = 8;

/// Hands the planner its window (previous song, current, then the next ones as walked, repeat included)
/// if it changed. True when it did, so the platform asks for a new plan.
pub fn playlist_window() -> bool {
    let window = {
        let mut q = QUEUE.lock();
        let p = &q.list;
        let mut ids = Vec::with_capacity(WINDOW_LEN + 1);
        let at = |i: usize| (p.ids()[i].clone(), p.album_run(i));
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
    queue::queue_window(&window.0, window.1);
    true
}

/// The current song's ReplayGain volume (`queue::queue_gain`); `bit_perfect`: the output is untouched.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_gain(bit_perfect: bool) -> f32 {
    gain_at(None, bit_perfect)
}

/// [`playlist_gain`] for list index `index`, so a player can set the next song's volume ahead of time.
pub fn playlist_gain_of(index: usize, bit_perfect: bool) -> f32 {
    gain_at(Some(index), bit_perfect)
}

fn gain_at(index: Option<usize>, bit_perfect: bool) -> f32 {
    let Some(s) = nori_settings::settings_store::settings_current() else { return 1.0 };
    let prefs = s.gain_prefs();
    let (before, current, after, shuffling) = with(|p| {
        let id = |i: Option<usize>| i.map(|i| (p.ids()[i].clone(), p.album_run(i)));
        let repeat = walk_repeat(p);
        match index.filter(|&i| i < p.len()) {
            Some(i) => (id(p.previous_of(i, repeat)), id(Some(i)), id(p.next_of(i, repeat)), p.shuffling()),
            None => (id(p.previous()), id(p.current()), id(p.next()), p.shuffling()),
        }
    });
    queue::queue_gain(before, current, after, &prefs, bit_perfect, shuffling)
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

/// The queue view; songs are omitted when `held` equals the current `list_rev`.
pub fn playlist_view(held: u64) -> PlaylistView {
    let (ids, mut view) = with(|p| {
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
        view.songs = queue::queue_songs(ids);
    }
    view
}

/// [`playlist_view`] only if the queue has `len` songs: while the platform player trails the core after
/// a change, its index must not be paired with the core's songs (use [`playlist_view_of`] then).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_view_for(held: u64, len: u32) -> Option<PlaylistView> {
    let v = playlist_view(held);
    (v.len == len).then_some(v)
}

/// A view built from the platform player's own list while it trails the core's: `ids`, `hands` and its
/// play `order` (the list order if `order` is not a permutation). Songs are always sent; `list_rev` 0
/// and `rev` `u64::MAX` keep readers from mistaking it for the core's list.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_view_of(ids: Vec<String>, hands: Vec<Hand>, order: Vec<u32>) -> PlaylistView {
    let len = ids.len();
    let mut seen = vec![false; len];
    let permutation = order.len() == len && order.iter().all(|&i| seen.get_mut(i as usize).is_some_and(|s| !std::mem::replace(s, true)));
    let order = if permutation { order } else { (0..len as u32).collect() };
    let queued = (0..len as u32).filter(|&i| hands.get(i as usize).is_some_and(|h| *h != Hand::No)).collect();
    let (shuffle, repeat, bridging) = with(|p| (p.lit(), p.repeat(), p.bridging()));
    let songs = if ids.is_empty() { Vec::new() } else { queue::queue_songs(ids) };
    PlaylistView { songs, len: len as u32, list_rev: 0, order, queued, index: -1, shuffle, repeat, bridging, rev: u64::MAX }
}

/// Changes whenever the list or its order does.
pub fn playlist_rev() -> u64 {
    with(|p| p.rev())
}

/// The ids to save as the server's play queue: radio streams left out, empty unless scrobbling is on.
pub fn playlist_to_push() -> Vec<String> {
    if !crate::rules::prefs(|p| p.scrobble) {
        return Vec::new();
    }
    with(|p| p.ids().iter().filter(|id| !id.starts_with(queue::RADIO_PREFIX)).cloned().collect())
}

/// The server write saving the play queue at `current`/`position_ms`; None when there is nothing to save.
pub fn push_write(current: Option<String>, position_ms: i64) -> Option<nori_net::requests::Write> {
    let ids = playlist_to_push();
    (!ids.is_empty()).then_some(nori_net::requests::Write::SaveQueue { ids, current, position_ms })
}

/// The current id and all queued ids.
pub fn snapshot() -> (Option<String>, Vec<String>) {
    with(|p| (p.current_id().map(str::to_string), p.ids().to_vec()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nori_model::OriginKind::{Album, Artist, Mix, Playlist as PlaylistKind, Search, ShuffleAlbums};

    /// The queue is process-wide: tests that use it take turns.
    static TURN: Mutex<()> = Mutex::new(());

    pub(crate) fn hold(ids: &[&str], start: u32) -> parking_lot::MutexGuard<'static, ()> {
        let g = TURN.lock();
        playlist_set(ids.iter().map(|s| s.to_string()).collect(), Some(start), false, None);
        playlist_repeat(0);
        g
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn window_ids() -> Vec<String> {
        QUEUE.lock().window.0.iter().map(|w| w.0.clone()).collect()
    }

    fn runs() -> Vec<u32> {
        with(|p| p.album_runs().to_vec())
    }

    fn origin(kind: OriginKind, id: &str) -> Option<PageOrigin> {
        Some(PageOrigin::new(kind, id))
    }

    fn page(kind: OriginKind, id: &str) -> std::sync::Arc<nori_library::pages::PageQueue> {
        nori_library::pages::PageQueue::new(PageOrigin::new(kind, id))
    }

    fn at(c: QueueChange) -> usize {
        c.at.unwrap() as usize
    }

    #[test]
    fn window_is_handed_over_only_on_change() {
        let _g = hold(&["w1", "w2", "w3"], 1);
        playlist_window();
        assert!(!playlist_window());
        playlist_moved_to(2);
        assert!(playlist_window());
        assert_eq!(window_ids(), ids(&["w2", "w3"]));
        playlist_repeat(2);
        assert!(playlist_window());
        assert_eq!(window_ids(), ids(&["w2", "w3", "w1", "w2", "w3", "w1", "w2", "w3", "w1"]));
    }

    #[test]
    fn only_whole_albums_get_an_album_run() {
        let _g = hold(&[], 0);
        let songs = ids(&["ar1", "ar2", "ar3"]);
        playlist_set(songs.clone(), Some(1), false, origin(Album, "AR"));
        let r = runs();
        assert!(r[0] > 0 && r.iter().all(|&x| x == r[0]), "{r:?}");
        // Autofill and a single song get none; the album added whole gets a new run.
        playlist_take(9, ids(&["fill"]), vec![Hand::No], None);
        playlist_take(9, ids(&["ar2"]), vec![Hand::Last], None);
        playlist_take(9, songs.clone(), vec![Hand::Last; 3], origin(Album, "AR"));
        assert_eq!(with(|p| p.ids().to_vec()), ids(&["ar1", "ar2", "ar2", "ar1", "ar2", "ar3", "ar3", "fill"]));
        let added = runs()[3];
        assert!(added > 0 && added != r[0]);
        assert_eq!(runs(), [r[0], r[0], 0, added, added, added, r[0], 0]);
        // The window carries each place's run.
        playlist_window();
        assert!(QUEUE.lock().window.0.iter().any(|(id, run)| id == "ar2" && *run == r[0]));

        for from in [origin(PlaylistKind, "pl"), origin(Search, "ar"), origin(Mix, "m"), None] {
            playlist_set(songs.clone(), Some(0), false, from.clone());
            assert_eq!(runs(), [0, 0, 0], "{from:?}");
        }
        playlist_set_ordered(songs.clone(), origin(Album, "AR"));
        assert_eq!(runs(), [0, 0, 0], "a weighted shuffle has no runs");
    }

    #[test]
    fn shuffle_albums_gives_each_album_its_own_run() {
        let _g = hold(&[], 0);
        let song = |id: &str, album: Option<&str>| Song { album_id: album.map(str::to_string), ..Song::only_id(id.to_string()) };
        queue::queue_register(vec![song("a1", Some("A")), song("a2", Some("A")), song("b1", Some("B")), song("b2", Some("B")), song("x", None), song("c1", Some("C")), song("c2", Some("C"))]);
        let shuffle = origin(ShuffleAlbums, "");
        playlist_set(ids(&["a1", "a2", "b1", "b2"]), Some(0), false, shuffle.clone());
        let r = runs();
        assert!(r[0] > 0 && r[0] == r[1] && r[2] > 0 && r[2] == r[3] && r[0] != r[2], "{r:?}");
        playlist_take(4, ids(&["x", "c1", "c2"]), vec![Hand::No; 3], shuffle);
        let r = runs();
        assert!(r[4] == 0 && r[5] > 0 && r[5] == r[6] && r[5] != r[2], "{r:?}");
        // Songs not marked as the shuffle's refill get none.
        let i = at(playlist_take(9, ids(&["c1"]), vec![Hand::Last], None));
        assert_eq!(runs()[i], 0);
        let i = at(playlist_take(99, ids(&["c1", "c2"]), vec![Hand::No; 2], None));
        assert_eq!(runs()[i..i + 2], [0, 0]);
        playlist_set(ids(&["a1"]), Some(0), false, None);
        playlist_take(1, ids(&["c1", "c2"]), vec![Hand::No; 2], None);
        assert_eq!(runs(), [0, 0, 0]);
    }

    /// A song tapped on an album page while a playlist plays makes the album's page the playing one.
    #[test]
    fn album_page_tap_moves_origin_to_album() {
        use nori_library::pages::PageQueue;
        let _g = hold(&[], 0);
        let (playlist, album) = (PageOrigin::new(PlaylistKind, "pl-t"), PageOrigin::new(Album, "al-t"));
        let lights = |o: &PageOrigin| playlist_from(PageQueue::new(o.clone()));
        playlist_set(ids(&["p1", "t2", "p3"]), Some(1), false, Some(playlist.clone()));
        assert!(lights(&playlist) && !lights(&album));
        let gen = playlist_origin_gen();
        playlist_set(ids(&["t1", "t2", "t3"]), Some(1), false, Some(album.clone()));
        assert_ne!(playlist_origin_gen(), gen);
        assert!(lights(&album) && !lights(&playlist));
        playlist_set(ids(&["t1", "t2", "t3"]), Some(2), false, Some(album.clone()));
        assert_eq!(with(|p| p.current_id().map(str::to_string)), Some("t3".into()));
        // Edits keep both the origin and the album run.
        let run = with(|p| p.album_run(0));
        playlist_take(3, ids(&["auto1", "auto2"]), vec![Hand::No; 2], None);
        playlist_take(9, ids(&["mine"]), vec![Hand::Next], None);
        playlist_remove(0, 1);
        playlist_move(0, 1, 2);
        playlist_shuffle(true);
        playlist_shuffle(false);
        assert!(lights(&album) && !lights(&playlist));
        assert_eq!(with(|p| (0..p.len()).filter(|&i| p.album_run(i) == run).count()), 2, "t2 and t3: {:?}", runs());
    }

    #[test]
    fn restored_queue_keeps_album_runs() {
        let _g = hold(&[], 0);
        playlist_set(ids(&["pb1", "pb2"]), Some(0), false, origin(Album, "PB"));
        playlist_take(9, ids(&["pb3"]), vec![Hand::Last], None);
        let saved = (with(|p| p.ids().to_vec()), runs());
        assert_eq!((saved.1[1], saved.1[0] == saved.1[2]), (0, true), "{saved:?}");
        playlist_set(ids(&["other"]), Some(0), false, None);
        playlist_put_back_runs(saved.0.clone(), saved.1.clone());
        playlist_set(saved.0.clone(), Some(0), false, origin(Album, "PB"));
        assert_eq!(runs(), saved.1);
        // Only the very next set of those ids takes them.
        playlist_put_back_runs(saved.0.clone(), saved.1.clone());
        playlist_set(ids(&["another"]), Some(0), false, None);
        playlist_set(saved.0.clone(), Some(0), false, None);
        assert_eq!(runs(), [0, 0, 0]);
    }

    #[test]
    fn album_gain_only_in_album_run() {
        use nori_player::gain::GainPrefs;
        use nori_player::policy::GainMode;
        let _g = hold(&[], 0);
        let rg = nori_model::ReplayGain { track_gain: Some(-6.0), album_gain: Some(-2.0), track_peak: Some(0.5), album_peak: Some(0.5), ..Default::default() };
        let song = |id: &str, track: u32| Song { duration: 200, album_id: Some("GA".into()), track, disc_number: 1, replay_gain: Some(rg.clone()), ..Song::only_id(id.to_string()) };
        queue::queue_register(vec![song("ga1", 1), song("ga2", 2)]);
        let prefs = GainPrefs::attenuating(GainMode::Auto, 0.0, 0.0);
        let at = |id: &str, run: u32| Some((id.to_string(), run));
        let db = |g: f32| (20.0 * g.log10() * 10.0).round() / 10.0;
        assert_eq!(db(queue::queue_gain(at("ga1", 3), at("ga2", 3), None, &prefs, false, false)), -2.0);
        assert_eq!(db(queue::queue_gain(at("ga1", 0), at("ga2", 0), None, &prefs, false, false)), -6.0);
    }

    #[test]
    fn edits_report_where_songs_went() {
        let _g = hold(&["e1", "e2", "e3"], 0);
        assert_eq!(playlist_take(9, ids(&["n"]), vec![Hand::Next], None), QueueChange { at: Some(1), shuffled: false });
        assert_eq!(playlist_take(9, ids(&["i"]), vec![Hand::No], None).at, Some(4), "clamped to the end");
        assert!(playlist_shuffle(true).shuffled);
        assert_eq!(with(|p| p.shuffle_order().unwrap()[..2].to_vec()), [0, 1], "current, then the one added by hand");
        let v = playlist_view(0);
        assert_eq!((v.index, v.queued.as_slice(), v.shuffle), (0, &[1u32][..], true));
        assert_eq!(v.songs[1].id, "n");
    }

    #[test]
    fn view_resends_songs_only_when_list_changed() {
        let _g = hold(&["v1", "v2", "v3"], 0);
        let first = playlist_view(0);
        assert_eq!((first.songs.len(), first.len), (3, 3));
        playlist_shuffle(true);
        let shuffled = playlist_view(first.list_rev);
        assert!(shuffled.songs.is_empty());
        assert_eq!((shuffled.len, shuffled.list_rev), (3, first.list_rev));
        assert_ne!(shuffled.rev, first.rev);
        playlist_take(9, ids(&["v4"]), vec![Hand::Next], None);
        let added = playlist_view(first.list_rev);
        assert_eq!((added.songs.len(), added.len), (4, 4));
        assert_ne!(added.list_rev, first.list_rev);
        assert!(playlist_view_for(0, 3).is_none(), "the player trails the core's list");
        assert_eq!(playlist_view_for(0, 4).map(|v| v.songs.len()), Some(4));
    }

    #[test]
    fn view_of_player_list() {
        let _g = hold(&["p1"], 0);
        let v = playlist_view_of(ids(&["p1", "p2", "p3"]), vec![Hand::No, Hand::Next, Hand::Last], vec![2, 0, 1]);
        assert_eq!((v.songs.len(), v.len, v.order.as_slice(), v.queued.as_slice()), (3, 3, &[2u32, 0, 1][..], &[1u32, 2][..]));
        assert_eq!((v.list_rev, v.rev), (0, u64::MAX));
        for bad in [vec![], vec![0, 0, 1], vec![0, 1, 3], vec![0, 1]] {
            assert_eq!(playlist_view_of(ids(&["p1", "p2", "p3"]), vec![], bad.clone()).order, [0, 1, 2], "{bad:?}");
        }
        assert!(playlist_view_of(ids(&["p1", "p2"]), vec![], vec![]).queued.is_empty());
    }

    #[test]
    fn shuffle_shown_until_turned_off() {
        let _g = hold(&["s1", "s2"], 0);
        assert!(!playlist_shuffle_shown());
        playlist_show_shuffle(true);
        assert!(playlist_shuffle_shown());
        playlist_set_ordered(ids(&["s2", "s1"]), None);
        assert!(playlist_shuffle_shown(), "a weighted shuffle stays lit");
        playlist_take(0, ids(&["s3"]), vec![Hand::Last], None);
        assert!(playlist_shuffle_shown());
        playlist_show_shuffle(false);
        playlist_shuffle(false);
        assert!(!playlist_shuffle_shown());
    }

    #[test]
    fn origin_survives_edits() {
        let _g = hold(&["o0"], 0);
        let (a, b, album, artist) = (page(PlaylistKind, "A"), page(PlaylistKind, "B"), page(Album, "al"), page(Artist, "ar"));
        let gen = playlist_origin_gen();
        playlist_set(ids(&["a1", "shared", "a3"]), Some(1), false, Some(a.origin()));
        assert_ne!(playlist_origin_gen(), gen);
        assert!(playlist_from(a.clone()));
        assert!(!playlist_from(b.clone()), "shares the song but did not start the queue");
        assert!(!playlist_from(album.clone()) && !playlist_from(artist.clone()));
        assert!(!playlist_from(page(Album, "A")), "same id, other kind");

        playlist_set(ids(&["shared", "al2"]), Some(0), false, Some(album.origin()));
        assert!(playlist_from(album.clone()) && !playlist_from(a.clone()));

        let gen = playlist_origin_gen();
        playlist_take(9, ids(&["x"]), vec![Hand::Next], None);
        playlist_take(9, ids(&["fill1", "fill2"]), vec![Hand::No; 2], None);
        playlist_move(0, 1, 2);
        playlist_remove(1, 2);
        playlist_shuffle(true);
        playlist_moved_to(1);
        assert!(playlist_from(album.clone()));
        assert_eq!(playlist_origin_gen(), gen);

        playlist_set_ordered(ids(&["r1", "r2"]), Some(artist.origin()));
        assert!(playlist_from(artist.clone()) && !playlist_from(album.clone()));
        playlist_set(ids(&["radio:1"]), Some(0), false, None);
        assert_eq!(playlist_origin(), None);
    }

    #[test]
    fn removed_song_restores_once() {
        let _g = hold(&["u1", "u2", "u3", "u4"], 1);
        playlist_take(9, ids(&["mine"]), vec![Hand::Next], None);
        assert_eq!(with(|p| p.ids().to_vec()), ids(&["u1", "u2", "mine", "u3", "u4"]));
        playlist_remove(2, 3);
        assert_eq!(playlist_restore("other".into()).at, None);
        assert_eq!(playlist_restore("mine".into()), QueueChange { at: Some(2), shuffled: false });
        assert_eq!(with(|p| (p.ids().to_vec(), p.current(), p.hand(2))), (ids(&["u1", "u2", "mine", "u3", "u4"]), Some(1), Hand::Next));
        assert_eq!(playlist_restore("mine".into()).at, None, "only once");

        // Only the last single removal.
        playlist_remove(0, 1);
        playlist_remove(1, 2);
        assert_eq!(playlist_restore("u1".into()).at, None);
        assert_eq!(playlist_restore("mine".into()).at, Some(1));
        playlist_remove(0, 2);
        assert_eq!(playlist_restore("u2".into()).at, None, "a range is not undoable");

        // Removing the current song plays the next; restoring keeps that one current.
        playlist_set(ids(&["p1", "p2", "p3"]), Some(1), false, None);
        playlist_remove(1, 2);
        assert_eq!(with(|p| p.current_id().map(str::to_string)).as_deref(), Some("p3"));
        assert_eq!(playlist_restore("p2".into()).at, Some(1));
        let v = playlist_view(0);
        assert_eq!((v.index, v.len), (2, 3));

        playlist_remove(0, 1);
        playlist_set(ids(&["n1", "p1"]), Some(0), false, None);
        assert_eq!(playlist_restore("p1".into()).at, None, "a new queue forgets it");
    }

    #[test]
    fn restore_keeps_origin_and_shuffle_turn() {
        let _g = hold(&["o0"], 0);
        let album = page(Album, "al");
        playlist_set(ids(&["s1", "s2", "s3", "s4", "s5"]), Some(0), true, Some(album.origin()));
        let gen = playlist_origin_gen();
        let order = with(|p| p.play_order().collect::<Vec<_>>());
        let i = order[2];
        let id = with(|p| p.ids()[i].clone());
        playlist_remove(i as u32, i as u32 + 1);
        assert_eq!(playlist_restore(id), QueueChange { at: Some(i as u32), shuffled: true });
        assert_eq!(with(|p| p.play_order().collect::<Vec<_>>()), order);
        assert!(playlist_from(album));
        assert_eq!(playlist_origin_gen(), gen);
    }

    #[test]
    fn push_leaves_out_radio() {
        let _g = hold(&["t1", "t2", "radio:9"], 0);
        match push_write(Some("t2".into()), 7) {
            Some(nori_net::requests::Write::SaveQueue { ids: pushed, current, position_ms }) => {
                assert_eq!((pushed, current.as_deref(), position_ms), (ids(&["t1", "t2"]), Some("t2"), 7));
            }
            w => panic!("{w:?}"),
        }
    }
}
