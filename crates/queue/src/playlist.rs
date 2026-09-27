//! The queue the app plays, owned here (nori_player::playlist). The platform's player mirrors it: a
//! change to the queue is made here first and the answer says where it lands and, while shuffling,
//! the play order to write. Everything that asks what the queue is - what comes next, the transition
//! planner's window, ReplayGain, the queue as the app lists it, the queue saved for next time - reads
//! it here, without the player's list crossing over.

use nori_player::playlist::{Playlist, Splice, Taken};
use parking_lot::Mutex;

// Public, like model.rs's, since the uniffi scaffolding in crates/android names them by a public path.
pub use nori_player::playlist::Hand;
pub use nori_player::queue::Onto;

use crate::queue;
use nori_model::{PageOrigin, Song};
use std::sync::atomic::{AtomicU32, Ordering};

static LIST: Mutex<Playlist> = Mutex::new(Playlist::new());
/// The planner's window as it was last handed over, so it is handed over only when it changes.
static WINDOW: Mutex<(Vec<String>, bool)> = Mutex::new((Vec::new(), false));
/// The page the queue was started from ([`PageOrigin`]); none for a queue started anywhere else.
/// Set with each new queue and kept through every edit of it (added, put next, removed, moved,
/// refilled, bridged): the queue still came from that page.
static ORIGIN: Mutex<Option<PageOrigin>> = Mutex::new(None);
/// Moves each time a new queue is set, so a page asks again whether it is the one playing only then.
static ORIGIN_GEN: AtomicU32 = AtomicU32::new(0);
/// The last song taken out on its own, as it was, for an undo to put back ([`playlist_restore`]). Gone
/// with a new queue: an undo never reaches into another one.
static TAKEN: Mutex<Option<Taken>> = Mutex::new(None);

fn set_origin(origin: Option<PageOrigin>) {
    *ORIGIN.lock() = origin;
    *TAKEN.lock() = None;
    ORIGIN_GEN.fetch_add(1, Ordering::Release);
}

/// The page the queue was started from, if it was; saved with the queue (`Core::playlist_save`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_origin() -> Option<PageOrigin> {
    ORIGIN.lock().clone()
}

/// Moves whenever a new queue is set, and with it perhaps its origin: a page asks [`playlist_from`]
/// again only when this has moved (`PlaylistJni.origin`, one int on each player event).
pub fn playlist_origin_gen() -> u32 {
    ORIGIN_GEN.load(Ordering::Acquire)
}

/// Whether the queue is the one `page` started: its origin is the page's own, kind and id. Not
/// whether the song playing is one of the page's: a song of playlist A playing from A leaves playlist
/// B, which has it too, alone, and an album's song played from anywhere else leaves the album's page
/// alone. Asked when the queue changes ([`playlist_origin_gen`]), never per frame.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_from(page: std::sync::Arc<nori_library::pages::PageQueue>) -> bool {
    ORIGIN.lock().as_ref() == Some(page.origin_ref())
}

/// The queue, lent to `f` to read.
pub fn with<R>(f: impl FnOnce(&Playlist) -> R) -> R {
    f(&LIST.lock())
}

/// The queue as it is now, lent to `f`, for a player that walks it itself (`nori-engine`): what comes
/// next, repeat and the play order are read here rather than copied out.
pub fn playlist_read<R>(f: impl FnOnce(&Playlist) -> R) -> R {
    with(f)
}

fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64)
}

/// Where a change landed, for the player to make the same change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QueueChange {
    /// The list index: the song to start at for a new queue, where the songs went for an insert; -1 none.
    pub at: i32,
    /// Shuffling: the player is given the play order, read with `PlaylistJni.order` straight into the
    /// array it takes rather than crossing here as a list of boxed numbers.
    pub shuffled: bool,
}

fn change(p: &Playlist, at: Option<usize>) -> QueueChange {
    QueueChange { at: at.map_or(-1, |a| a as i32), shuffled: p.shuffle_order().is_some() }
}

fn edit(f: impl FnOnce(&mut Playlist) -> Option<usize>) -> QueueChange {
    let mut p = LIST.lock();
    let at = f(&mut p);
    change(&p, at)
}

/// A change the player makes as given: `remove` holds (from, to) pairs, last first, flattened; then
/// `songs` go in at `at`; then, when not -1, a jump to `seek`; and while `shuffled` the play order.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct QueueEdit {
    pub remove: Vec<u32>,
    pub at: u32,
    pub songs: Vec<Song>,
    pub seek: i32,
    pub shuffled: bool,
}

/// An edit of the queue that splices `songs` in where `f` says, as the platform is told of it.
pub fn edit_splice(f: impl FnOnce(&mut Playlist) -> Option<Splice>, songs: Vec<Song>) -> Option<QueueEdit> {
    let mut p = LIST.lock();
    let s = f(&mut p)?;
    Some(QueueEdit {
        remove: s.remove.iter().flat_map(|&(a, b)| [a as u32, b as u32]).collect(),
        at: s.at as u32,
        songs,
        seek: s.seek.map_or(-1, |i| i as i32),
        shuffled: p.shuffle_order().is_some(),
    })
}

/// The server is back: the offline bridge's songs go and the parked song plays. None when not bridging.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_unbridge() -> Option<QueueEdit> {
    edit_splice(|p| p.unbridge(), Vec::new())
}

/// Whether the offline bridge is playing, and whether it has run out before the parked song.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_bridge_state() -> BridgeState {
    with(|p| BridgeState { bridging: p.bridging(), next_is_parked: p.next_is_parked(), parked: p.parked_id().map(str::to_string), current: p.current_id().map(str::to_string) })
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BridgeState {
    pub bridging: bool,
    pub next_is_parked: bool,
    /// The song the queue picks up at once the server is back, and the one playing.
    pub parked: Option<String>,
    pub current: Option<String>,
}

/// A new queue (`start` -1: wherever shuffle starts), started from the page `origin` (none: from
/// anywhere that is not a page's songs, such as one song, a selection, a radio or a mix drawn from a song).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_set(ids: Vec<String>, start: i32, shuffle: bool, origin: Option<PageOrigin>) -> QueueChange {
    set_origin(origin);
    edit(|p| p.set(ids, usize::try_from(start).ok(), shuffle, seed()))
}

/// A new queue already in the order it plays, shown as shuffled (a weighted shuffle), from `origin`.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_set_ordered(ids: Vec<String>, origin: Option<PageOrigin>) -> QueueChange {
    set_origin(origin);
    edit(|p| p.set_ordered(ids))
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum Hand {
    No,
    Next,
    Last,
    Bridge,
}

/// Songs a controller adds at `at`, each marked with how it came (`hands`, one per song: Play next, Add
/// to queue, or neither). Where they go is `nori_player::playlist::Playlist::take`'s call.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_take(at: u32, ids: Vec<String>, hands: Vec<Hand>) -> QueueChange {
    edit(|p| Some(p.take(at as usize, ids, &hands)))
}

/// Songs `from..to` taken out. One song on its own is remembered as it was, so an undo can put it back
/// ([`playlist_restore`]); the song playing going, the one after it plays (`Playlist::remove`).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_remove(from: u32, to: u32) -> QueueChange {
    edit(|p| {
        *TAKEN.lock() = if to == from + 1 { p.taken(from as usize) } else { None };
        p.remove(from as usize, to as usize);
        p.current()
    })
}

/// Undo: the song `id` last taken out on its own put back where it was - its list index, its turn under
/// shuffle and its mark as added by hand - in the queue as it is now. The song playing stays the one
/// playing, and the queue's origin stays. `at` is where it went, or -1 when that song is not the one to
/// put back (nothing taken out, another song since, a new queue); the caller then inserts it itself.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_restore(id: String) -> QueueChange {
    let mut p = LIST.lock();
    let t = TAKEN.lock().take_if(|t| t.id == id);
    let at = t.map(|t| p.restore(&t));
    change(&p, at)
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

/// The user asked for shuffle on or off (a plain Play, Shuffle, the shuffle button): shown so at once,
/// before the queue's own change arrives, which then says the same.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_show_shuffle(on: bool) {
    LIST.lock().show_shuffle(on);
}

/// Whether shuffle is shown as on: the queue is shuffled, or was put in a shuffled order before it was
/// queued (a weighted shuffle), and the user has not turned it off since.
pub fn playlist_shuffle_shown() -> bool {
    with(|p| p.lit())
}

/// The play order while shuffling (list indexes, in the order they play), lent to `f`; none when not
/// shuffling. The platform copies it straight into the order its player takes.
pub fn playlist_shuffle_order<R>(f: impl FnOnce(Option<&[usize]>) -> R) -> R {
    with(|p| f(p.shuffle_order()))
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_repeat(mode: u8) {
    LIST.lock().set_repeat(mode);
}

/// The player moved to `index` by itself: a song ended, a seek to another song.
pub fn playlist_moved_to(index: i32) {
    if let Ok(i) = usize::try_from(index) {
        LIST.lock().moved_to(i);
    }
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum Onto {
    Skip,
    Loop,
    Song,
}

/// The player moved onto `index` (-1: onto nothing), `looped` by its own repeat. What that means is
/// `nori_player::queue::arrival`'s call over this queue and the user's "skip explicit songs"; a new song
/// also breaks a run of songs that would not play.
#[cfg(test)]
pub fn playlist_transition(index: i32, looped: bool) -> Onto {
    playlist_moved_to(index);
    let skip_explicit = crate::rules::prefs(|p| p.skip_explicit);
    let (current, has_next) = with(|p| (p.current_id().map(str::to_string), p.next().is_some()));
    let explicit = current.clone().is_some_and(|id| queue::queue_flags(id) & queue::EXPLICIT != 0);
    let a = nori_player::queue::arrival(current.is_some(), skip_explicit, explicit, has_next, looped);
    a
}

/// Whether arriving on list index `index` now would skip it, as [`playlist_transition`] would decide
/// there: a player that walks the queue itself asks before it reads the song, so none of it is heard.
pub fn playlist_skips(index: usize) -> bool {
    let skip_explicit = crate::rules::prefs(|p| p.skip_explicit);
    if !skip_explicit {
        return false;
    }
    let (id, has_next) = with(|p| {
        let repeat = if p.repeat() == nori_player::playlist::REPEAT_ONE { nori_player::playlist::REPEAT_ALL } else { p.repeat() };
        (p.ids().get(index).cloned(), p.next_of(index, repeat).is_some())
    });
    let explicit = id.is_some_and(|id| queue::queue_flags(id) & queue::EXPLICIT != 0);
    nori_player::queue::arrival(true, skip_explicit, explicit, has_next, false) == Onto::Skip
}

/// How many songs still follow the current one in play order, repeat left out.
pub fn playlist_after() -> u32 {
    with(|p| p.songs_after() as u32)
}

/// The songs coming up, the current one first, at most `n`, in play order.
pub fn playlist_upcoming(n: u32) -> Vec<String> {
    with(|p| p.upcoming().take(n as usize).map(|i| p.ids()[i].clone()).collect())
}

/// How many songs the planner's window holds after the one before the current one.
const WINDOW_LEN: usize = 8;

/// Hands the transition planner its window - the song before the current one, then the current one
/// and those after it as the player will walk them, repeat included - when it changed. True when it
/// did, so the platform asks for a new plan.
pub fn playlist_window() -> bool {
    let (ids, shuffling) = with(|p| {
        let mut ids = Vec::with_capacity(WINDOW_LEN + 1);
        if let Some(c) = p.current() {
            if let Some(b) = p.previous_of(c, p.repeat()) {
                ids.push(p.ids()[b].clone());
            }
            let mut i = Some(c);
            let mut n = 0;
            while let (Some(x), true) = (i, n < WINDOW_LEN) {
                ids.push(p.ids()[x].clone());
                i = p.next_of(x, p.repeat());
                n += 1;
            }
        }
        (ids, p.shuffling())
    });
    let mut w = WINDOW.lock();
    if w.0 == ids && w.1 == shuffling {
        return false;
    }
    queue::queue_window(ids.clone(), shuffling);
    *w = (ids, shuffling);
    true
}

/// The volume the current song plays at under ReplayGain (see `queue::queue_gain`), as the settings
/// say; `bit_perfect` whether the output takes the samples untouched.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_gain(bit_perfect: bool) -> f32 {
    gain_at(None, bit_perfect)
}

/// The volume the song at list index `index` will play at once the player gets there, as
/// [`playlist_gain`] works it out for the current one: a player that knows where the next song starts
/// in its output sets its volume for that moment ahead of time.
pub fn playlist_gain_of(index: usize, bit_perfect: bool) -> f32 {
    gain_at(Some(index), bit_perfect)
}

fn gain_at(index: Option<usize>, bit_perfect: bool) -> f32 {
    let Some(s) = nori_settings::settings_store::current() else { return 1.0 };
    let prefs = s.gain_prefs();
    let (before, current, after, shuffling) = with(|p| {
        let id = |i: Option<usize>| i.map(|i| p.ids()[i].clone());
        // As `Playlist::previous` and `next` walk from the current song: repeat one counts as all.
        let repeat = if p.repeat() == nori_player::playlist::REPEAT_ONE { nori_player::playlist::REPEAT_ALL } else { p.repeat() };
        match index.filter(|&i| i < p.len()) {
            Some(i) => (id(p.previous_of(i, repeat)), Some(p.ids()[i].clone()), id(p.next_of(i, repeat)), p.shuffling()),
            None => (id(p.previous()), p.current_id().map(str::to_string), id(p.next()), p.shuffling()),
        }
    });
    queue::queue_gain(before, current, after, &prefs, bit_perfect, shuffling)
}

/// The queue as the app lists it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PlaylistView {
    /// The songs, or none when the reader holds them already (`list_rev` is the one it passed).
    pub songs: Vec<Song>,
    /// How many songs the list has, sent or not.
    pub len: u32,
    /// Changes only when the songs listed do, not their order or the current one.
    pub list_rev: u64,
    /// The list indexes in the order they play.
    pub order: Vec<u32>,
    /// The songs added by hand.
    pub queued: Vec<u32>,
    pub index: i32,
    /// Shuffle shown as on.
    pub shuffle: bool,
    pub repeat: u8,
    /// The offline bridge is playing.
    pub bridging: bool,
    /// Changes whenever the list or its order does.
    pub rev: u64,
}

/// The queue as the app lists it. `held` is the `list_rev` of the songs the reader already holds: when
/// the list is still those songs (a shuffle, a song added by hand marked, the current one moved), they
/// are not copied again. The queue's every change used to send every song of it across.
pub fn playlist_view(held: u64) -> PlaylistView {
    let (ids, len, list_rev, order, queued, index, shuffle, repeat, bridging, rev) = with(|p| {
        (
            if p.list_rev() == held { Vec::new() } else { p.ids().to_vec() },
            p.len() as u32,
            p.list_rev(),
            p.play_order().map(|i| i as u32).collect(),
            p.by_hand().map(|i| i as u32).collect(),
            p.current().map_or(-1, |c| c as i32),
            p.lit(),
            p.repeat(),
            p.bridging(),
            p.rev(),
        )
    });
    let songs = if ids.is_empty() { Vec::new() } else { queue::queue_songs(ids) };
    PlaylistView { songs, len, list_rev, order, queued, index, shuffle, repeat, bridging, rev }
}

/// [`playlist_view`] for a page whose player lists `len` songs, or none when the core's list is not that
/// long: after a change the platform's player trails the core's list for a moment, and a page must not
/// pair the player's current index with the core's songs then. [`playlist_view_of`] reads the player's
/// own list in that moment.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_view_for(held: u64, len: u32) -> Option<PlaylistView> {
    let v = playlist_view(held);
    (v.len == len).then_some(v)
}

/// The queue as the app lists it, read from the platform player's own list while that trails the core's
/// ([`playlist_view_for`] gave none): `ids` and how each was added (`hands`) as the player's items carry
/// them, and `order`, the player's list indexes in the order they play. An order that is not one of
/// every index once (a player with no timeline yet) is the list's own. The songs are always sent, and
/// `list_rev` is 0 and `rev` `u64::MAX`, so no reader takes them for the core's list.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_view_of(ids: Vec<String>, hands: Vec<Hand>, order: Vec<u32>) -> PlaylistView {
    let len = ids.len();
    let mut seen = vec![false; len];
    let whole = order.len() == len && order.iter().all(|&i| seen.get_mut(i as usize).is_some_and(|s| !std::mem::replace(s, true)));
    let order = if whole { order } else { (0..len as u32).collect() };
    let queued = (0..len as u32).filter(|&i| hands.get(i as usize).is_some_and(|h| *h != Hand::No)).collect();
    let (shuffle, repeat, bridging) = with(|p| (p.lit(), p.repeat(), p.bridging()));
    let songs = if ids.is_empty() { Vec::new() } else { queue::queue_songs(ids) };
    PlaylistView { songs, len: len as u32, list_rev: 0, order, queued, index: -1, shuffle, repeat, bridging, rev: u64::MAX }
}

/// What the list looks like now, cheaply: changes whenever the list or its order does.
pub fn playlist_rev() -> u64 {
    with(|p| p.rev())
}

/// The queue to hand the server (its "play queue", for picking up on another device): the songs, radio
/// streams left out, and only while the user lets plays be sent to the server at all.
pub fn playlist_to_push() -> Vec<String> {
    if !crate::rules::prefs(|p| p.scrobble) {
        return Vec::new();
    }
    with(|p| p.ids().iter().filter(|id| !id.starts_with(queue::RADIO_PREFIX)).cloned().collect())
}

/// The server's copy of the queue (its "play queue") for [`playlist_to_push`]'s songs, with `current`
/// and the place in it; none when there is nothing to hand over.
pub fn push_write(current: Option<String>, position_ms: i64) -> Option<nori_net::requests::Write> {
    let ids = playlist_to_push();
    (!ids.is_empty()).then_some(nori_net::requests::Write::SaveQueue { ids, current, position_ms })
}

/// The ids queued, and the current one, for autofill.
pub fn snapshot() -> (Option<String>, Vec<String>) {
    with(|p| (p.current_id().map(str::to_string), p.ids().to_vec()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The one queue is the process's: tests that use it take turns.
    static TURN: Mutex<()> = Mutex::new(());

    pub(crate) fn hold(ids: &[&str], start: i32) -> parking_lot::MutexGuard<'static, ()> {
        let g = TURN.lock();
        playlist_set(ids.iter().map(|s| s.to_string()).collect(), start, false, None);
        playlist_repeat(0);
        g
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_window_is_handed_over_only_when_it_changes() {
        let _g = hold(&["w1", "w2", "w3"], 1);
        playlist_window();
        assert!(!playlist_window(), "nothing changed");
        playlist_moved_to(2);
        assert!(playlist_window());
        assert_eq!(WINDOW.lock().0, ids(&["w2", "w3"]));
        playlist_repeat(2);
        assert!(playlist_window());
        assert_eq!(WINDOW.lock().0, ids(&["w2", "w3", "w1", "w2", "w3", "w1", "w2", "w3", "w1"]));
    }

    #[test]
    fn edits_say_where_the_songs_went() {
        let _g = hold(&["e1", "e2", "e3"], 0);
        assert_eq!(playlist_take(9, ids(&["n"]), vec![Hand::Next]), QueueChange { at: 1, shuffled: false });
        assert_eq!(playlist_take(9, ids(&["i"]), vec![Hand::No]).at, 4, "a controller's own insert, clamped to the end");
        assert!(playlist_shuffle(true).shuffled);
        assert_eq!(with(|p| p.shuffle_order().unwrap()[..2].to_vec()), [0, 1], "the current song, then the one added by hand");
        let v = playlist_view(0);
        assert_eq!((v.index, v.queued.as_slice(), v.shuffle), (0, &[1u32][..], true));
        assert_eq!(v.songs[1].id, "n");
    }

    #[test]
    fn the_songs_are_sent_again_only_when_the_list_changed() {
        let _g = hold(&["v1", "v2", "v3"], 0);
        let first = playlist_view(0);
        assert_eq!((first.songs.len(), first.len), (3, 3));
        playlist_shuffle(true);
        let shuffled = playlist_view(first.list_rev);
        assert!(shuffled.songs.is_empty(), "only the order changed: the reader holds the songs");
        assert_eq!((shuffled.len, shuffled.list_rev), (3, first.list_rev));
        assert_ne!(shuffled.rev, first.rev);
        playlist_take(9, ids(&["v4"]), vec![Hand::Next]);
        let added = playlist_view(first.list_rev);
        assert_eq!((added.songs.len(), added.len), (4, 4));
        assert_ne!(added.list_rev, first.list_rev);
        assert_eq!(playlist_view(0).songs.len(), 4, "a reader holding nothing gets them all");
    }

    #[test]
    fn a_player_of_another_length_gets_no_core_view() {
        let _g = hold(&["l1", "l2", "l3"], 0);
        assert_eq!(playlist_view_for(0, 3).map(|v| v.songs.len()), Some(3));
        assert!(playlist_view_for(0, 2).is_none(), "the player trails the core's list");
    }

    #[test]
    fn the_players_own_list_is_read_while_it_trails() {
        let _g = hold(&["p1"], 0);
        let v = playlist_view_of(ids(&["p1", "p2", "p3"]), vec![Hand::No, Hand::Next, Hand::Last], vec![2, 0, 1]);
        assert_eq!((v.songs.len(), v.len, v.order.as_slice(), v.queued.as_slice()), (3, 3, &[2u32, 0, 1][..], &[1u32, 2][..]));
        assert_eq!((v.list_rev, v.rev as i64), (0, -1), "never taken for the core's list");
        // An order that is not every index once is the list's own.
        for bad in [vec![], vec![0, 0, 1], vec![0, 1, 3], vec![0, 1]] {
            assert_eq!(playlist_view_of(ids(&["p1", "p2", "p3"]), vec![], bad.clone()).order, [0, 1, 2], "{bad:?}");
        }
        assert!(playlist_view_of(ids(&["p1", "p2"]), vec![], vec![]).queued.is_empty(), "no marks: none added by hand");
        assert_eq!(playlist_view_of(vec![], vec![], vec![]).len, 0);
    }

    #[test]
    fn shuffle_is_shown_as_asked_until_turned_off() {
        let _g = hold(&["s1", "s2"], 0);
        assert!(!playlist_shuffle_shown());
        playlist_show_shuffle(true);
        assert!(playlist_shuffle_shown(), "at once, before the queue changes");
        playlist_set_ordered(ids(&["s2", "s1"]), None);
        assert!(playlist_shuffle_shown(), "a weighted shuffle stays lit");
        playlist_take(0, ids(&["s3"]), vec![Hand::Last]);
        assert!(playlist_shuffle_shown());
        playlist_show_shuffle(false);
        playlist_shuffle(false);
        assert!(!playlist_shuffle_shown());
    }

    fn page(kind: nori_model::OriginKind, id: &str) -> std::sync::Arc<nori_library::pages::PageQueue> {
        nori_library::pages::PageQueue::new(PageOrigin::new(kind, id))
    }

    #[test]
    fn a_page_is_the_one_playing_only_when_the_queue_came_from_it() {
        use nori_model::OriginKind::{Album, Artist, Playlist, Search};
        let _g = hold(&["o0"], 0);
        let (a, b, album, artist) = (page(Playlist, "A"), page(Playlist, "B"), page(Album, "al"), page(Artist, "ar"));
        // Playlist A plays; its song "shared" is in playlist B as well, and is on album "al" by "ar".
        let gen = playlist_origin_gen();
        playlist_set(ids(&["a1", "shared", "a3"]), 1, false, Some(a.origin()));
        assert_ne!(playlist_origin_gen(), gen, "a new queue moves the generation");
        assert!(playlist_from(a.clone()));
        assert!(!playlist_from(b.clone()), "B shares the song playing but did not start the queue");
        assert!(!playlist_from(album.clone()), "the album's song, played from a playlist");
        assert!(!playlist_from(artist.clone()), "the artist's song, played from a playlist");
        assert!(!playlist_from(page(Album, "A")), "the same id of another kind");

        // The album page lights only for a queue its own start made.
        playlist_set(ids(&["shared", "al2"]), 0, false, Some(album.origin()));
        assert!(playlist_from(album.clone()) && !playlist_from(a.clone()) && !playlist_from(artist.clone()));

        // Every edit keeps the origin: the queue still came from the album.
        let gen = playlist_origin_gen();
        playlist_take(9, ids(&["x"]), vec![Hand::Next]);
        playlist_take(9, ids(&["y"]), vec![Hand::Last]);
        playlist_take(9, ids(&["fill1", "fill2"]), vec![Hand::No; 2]);
        playlist_move(0, 1, 2);
        playlist_remove(1, 2);
        playlist_shuffle(true);
        playlist_shuffle(false);
        playlist_moved_to(1);
        assert!(playlist_from(album.clone()), "added, put next, refilled, moved, removed, shuffled");
        assert_eq!(playlist_origin_gen(), gen, "an edit does not make the pages ask again");

        // A new queue replaces it: from another page, or from no page at all.
        playlist_set_ordered(ids(&["r1", "r2"]), Some(artist.origin()));
        assert!(playlist_from(artist.clone()) && !playlist_from(album.clone()));
        playlist_set(ids(&["one"]), 0, false, Some(PageOrigin::new(Search, "q")));
        assert!(!playlist_from(artist.clone()));
        playlist_set(ids(&["radio:1"]), 0, false, None);
        assert_eq!(playlist_origin(), None);
        assert!(!playlist_from(a) && !playlist_from(b) && !playlist_from(album) && !playlist_from(artist));
    }

    #[test]
    fn a_song_taken_out_is_put_back_once() {
        let _g = hold(&["u1", "u2", "u3", "u4"], 1);
        playlist_take(9, ids(&["mine"]), vec![Hand::Next]);
        assert_eq!(with(|p| p.ids().to_vec()), ids(&["u1", "u2", "mine", "u3", "u4"]));
        playlist_remove(2, 3);
        assert_eq!(playlist_restore("other".into()).at, -1, "not the song taken out");
        assert_eq!(playlist_restore("mine".into()), QueueChange { at: 2, shuffled: false });
        assert_eq!(with(|p| (p.ids().to_vec(), p.current(), p.hand(2))), (ids(&["u1", "u2", "mine", "u3", "u4"]), Some(1), Hand::Next));
        assert_eq!(playlist_restore("mine".into()).at, -1, "put back once");

        // Only the last one, and not several taken out at once.
        playlist_remove(0, 1);
        playlist_remove(1, 2);
        assert_eq!(playlist_restore("u1".into()).at, -1);
        assert_eq!(playlist_restore("mine".into()).at, 1, "where it was in the queue as it is now");
        playlist_remove(0, 2);
        assert_eq!(playlist_restore("u2".into()).at, -1);

        // The song playing: the next one plays, and the undo does not go back to it.
        playlist_set(ids(&["p1", "p2", "p3"]), 1, false, None);
        playlist_remove(1, 2);
        assert_eq!(with(|p| p.current_id().map(str::to_string)).as_deref(), Some("p3"));
        assert_eq!(playlist_restore("p2".into()).at, 1);
        let v = playlist_view(0);
        assert_eq!((v.index, v.len), (2, 3), "p3 still playing, p2 back in its place");

        // A new queue forgets it.
        playlist_remove(0, 1);
        playlist_set(ids(&["n1", "p1"]), 0, false, None);
        assert_eq!(playlist_restore("p1".into()).at, -1);
    }

    #[test]
    fn an_undo_keeps_the_origin_and_the_shuffle() {
        use nori_model::OriginKind::Album;
        let _g = hold(&["o0"], 0);
        let album = page(Album, "al");
        playlist_set(ids(&["s1", "s2", "s3", "s4", "s5"]), 0, true, Some(album.origin()));
        let gen = playlist_origin_gen();
        let order = with(|p| p.play_order().collect::<Vec<_>>());
        let at = order[2];
        let id = with(|p| p.ids()[at].clone());
        playlist_remove(at as u32, at as u32 + 1);
        let back = playlist_restore(id);
        assert_eq!((back.at, back.shuffled), (at as i32, true));
        assert_eq!(with(|p| p.play_order().collect::<Vec<_>>()), order, "back in its turn");
        assert!(playlist_from(album), "removed and put back: still the album's queue");
        assert_eq!(playlist_origin_gen(), gen, "and the pages are not asked again");
    }

    #[test]
    fn arriving_on_a_song() {
        let _g = hold(&["t1", "t2", "radio:9"], 0);
        assert_eq!(playlist_transition(1, false), Onto::Song);
        assert_eq!(playlist_bridge_state().current.as_deref(), Some("t2"));
        assert_eq!(playlist_transition(1, true), Onto::Loop);
        assert_eq!(playlist_to_push(), ids(&["t1", "t2"]), "radio streams are not handed to the server");
        match push_write(Some("t2".into()), 7) {
            Some(nori_net::requests::Write::SaveQueue { ids: pushed, current, position_ms }) => {
                assert_eq!((pushed, current.as_deref(), position_ms), (ids(&["t1", "t2"]), Some("t2"), 7));
            }
            w => panic!("{w:?}"),
        }
    }
}
