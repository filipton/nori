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
use nori_model::{OriginKind, PageOrigin, Song};
use std::sync::atomic::{AtomicU32, Ordering};

static LIST: Mutex<Playlist> = Mutex::new(Playlist::new());
/// The planner's window as it was last handed over (each song with its album run), so it is handed over
/// only when it changes.
static WINDOW: Mutex<(Vec<(String, u32)>, bool)> = Mutex::new((Vec::new(), false));
/// The album runs of a saved queue about to be put back, with its songs ([`playlist_put_back_runs`]):
/// taken by the next new queue if it is those songs.
static PUT_BACK: Mutex<Option<(Vec<String>, Vec<u32>)>> = Mutex::new(None);
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
///
/// A queue started from an album ([`OriginKind::Album`]: its page, its Play or a row of it) is the album
/// played as an album: its songs are one album run (`Playlist::album_run`), kept gapless with "keep
/// albums gapless" on. So is each album of a "shuffle albums" queue ([`OriginKind::ShuffleAlbums`]):
/// whole albums one after another, each its own run. Any other origin is not, a playlist's included: a
/// playlist is a list somebody made, and songs of one album in it mix like any others (the album's own
/// page plays it whole). A queue put back as it was saved takes the runs it was saved with
/// ([`playlist_put_back_runs`]).
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn playlist_set(ids: Vec<String>, start: i32, shuffle: bool, origin: Option<PageOrigin>) -> QueueChange {
    let album = is_album(origin.as_ref());
    let whole = is_shuffle_albums(origin.as_ref()).then(|| album_spans(&ids));
    let saved = PUT_BACK.lock().take().filter(|(put, _)| *put == ids).map(|(_, runs)| runs);
    set_origin(origin);
    edit(|p| {
        let at = p.set(ids, usize::try_from(start).ok(), shuffle, seed());
        match saved {
            Some(runs) => p.set_album_runs(&runs),
            None if album => p.as_album(0, p.len()),
            None => whole.into_iter().flatten().for_each(|(from, to)| p.as_album(from, to)),
        }
        at
    })
}

/// Songs from `origin` are an album played (or added) as an album.
fn is_album(origin: Option<&PageOrigin>) -> bool {
    origin.is_some_and(|o| o.kind == OriginKind::Album)
}

fn is_shuffle_albums(origin: Option<&PageOrigin>) -> bool {
    origin.is_some_and(|o| o.kind == OriginKind::ShuffleAlbums)
}

/// Where each album of `ids` starts and ends: the stretches of songs of one album next to each other,
/// from the songs the queue knows (a song with no album is in none).
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

/// The album runs `runs` of the saved queue `ids` (one per song, as `Playlist::album_runs` gave them), for
/// the queue about to be put back from it: the next [`playlist_set`] of exactly those songs takes them.
pub fn playlist_put_back_runs(ids: Vec<String>, runs: Vec<u32>) {
    *PUT_BACK.lock() = (ids.len() == runs.len()).then_some((ids, runs));
}

/// A new queue already in the order it plays, shown as shuffled (a weighted shuffle), from `origin`. A
/// shuffle: no album runs, whatever the origin.
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
/// to queue, or neither). Where they go is `nori_player::playlist::Playlist::take`'s call. `from` is the
/// page they are all the songs of, when they are: an album's ([`OriginKind::Album`], its Play next or Add
/// to queue) is the album added whole, one album run of its own; a "shuffle albums" one
/// ([`OriginKind::ShuffleAlbums`], that queue's refill) is whole albums, each a run of its own as the
/// queue's first albums are. Songs added any other way (one at a time, a selection, autofill's songs) have
/// none.
#[cfg_attr(feature = "ffi", uniffi::export(default(from = None)))]
pub fn playlist_take(at: u32, ids: Vec<String>, hands: Vec<Hand>, from: Option<PageOrigin>) -> QueueChange {
    let count = ids.len();
    let album = is_album(from.as_ref()) && count > 0;
    let whole = is_shuffle_albums(from.as_ref()).then(|| album_spans(&ids));
    edit(|p| {
        let at = p.take(at as usize, ids, &hands);
        if album {
            p.as_album(at, at + count);
        }
        whole.into_iter().flatten().for_each(|(from, to)| p.as_album(at + from, at + to));
        Some(at)
    })
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

/// Whether arriving on list index `index` now would skip it (`nori_player::queue::arrival` over this queue
/// and the user's "skip explicit songs"): a player that walks the queue itself asks before it reads the
/// song, so none of it is heard.
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
        let at = |i: usize| (p.ids()[i].clone(), p.album_run(i));
        if let Some(c) = p.current() {
            if let Some(b) = p.previous_of(c, p.repeat()) {
                ids.push(at(b));
            }
            let mut i = Some(c);
            let mut n = 0;
            while let (Some(x), true) = (i, n < WINDOW_LEN) {
                ids.push(at(x));
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
    queue::queue_window(&ids, shuffling);
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
        let id = |i: Option<usize>| i.map(|i| (p.ids()[i].clone(), p.album_run(i)));
        // As `Playlist::previous` and `next` walk from the current song: repeat one counts as all.
        let repeat = if p.repeat() == nori_player::playlist::REPEAT_ONE { nori_player::playlist::REPEAT_ALL } else { p.repeat() };
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
        assert_eq!(WINDOW.lock().0.iter().map(|w| w.0.clone()).collect::<Vec<_>>(), ids(&["w2", "w3"]));
        playlist_repeat(2);
        assert!(playlist_window());
        assert_eq!(WINDOW.lock().0.iter().map(|w| w.0.clone()).collect::<Vec<_>>(), ids(&["w2", "w3", "w1", "w2", "w3", "w1", "w2", "w3", "w1"]));
    }

    /// Each queued song's album run, in list order.
    fn runs() -> Vec<u32> {
        with(|p| p.album_runs().to_vec())
    }

    fn album(id: &str) -> Option<PageOrigin> {
        Some(PageOrigin::new(OriginKind::Album, id))
    }

    #[test]
    fn only_an_album_played_or_added_whole_is_an_album_run() {
        let _g = hold(&[], 0);
        let songs = ids(&["ar1", "ar2", "ar3"]);
        // From the album's page (its Play, a row of it): one run.
        playlist_set(songs.clone(), 1, false, album("AR"));
        let r = runs();
        assert!(r[0] > 0 && r.iter().all(|&x| x == r[0]), "{r:?}");
        // Autofill's songs, a song added on its own, the album's songs picked one at a time: none. The
        // album added whole: a run of its own.
        playlist_take(9, ids(&["fill"]), vec![Hand::No], None);
        playlist_take(9, ids(&["ar2"]), vec![Hand::Last], None);
        playlist_take(9, songs.clone(), vec![Hand::Last; 3], album("AR"));
        // Each goes after the song playing, after those added by hand before it; the album parts the first
        // run where it lands, and the songs either side of it no longer meet.
        assert_eq!(with(|p| p.ids().to_vec()), ids(&["ar1", "ar2", "ar2", "ar1", "ar2", "ar3", "ar3", "fill"]));
        let added = runs()[3];
        assert!(added > 0 && added != r[0]);
        assert_eq!(runs(), [r[0], r[0], 0, added, added, added, r[0], 0]);
        // The window carries each place's run, so the planner tells the same song queued twice apart.
        playlist_window();
        assert!(WINDOW.lock().0.iter().any(|(id, run)| id == "ar2" && *run == r[0]));

        // A playlist's, a search's, a mix's, no page's: none, even if it holds an album in order.
        for from in [Some(PageOrigin::new(OriginKind::Playlist, "pl")), Some(PageOrigin::new(OriginKind::Search, "ar")), Some(PageOrigin::new(OriginKind::Mix, "m")), None] {
            playlist_set(songs.clone(), 0, false, from.clone());
            assert_eq!(runs(), [0, 0, 0], "{from:?}");
        }
        // The album page's Shuffle, spread by the core: a shuffle, no run.
        playlist_set_ordered(songs.clone(), album("AR"));
        assert_eq!(runs(), [0, 0, 0]);
    }

    /// "Shuffle albums": whole albums one after another, the first ones and each refill's, every album a
    /// run of its own, so "keep albums gapless" keeps each gapless and the change of album mixes.
    #[test]
    fn each_album_of_a_shuffle_albums_queue_is_a_run_of_its_own() {
        let _g = hold(&[], 0);
        let song = |id: &str, album: Option<&str>| Song { album_id: album.map(str::to_string), ..Song::only_id(id.to_string()) };
        queue::queue_register(vec![song("a1", Some("A")), song("a2", Some("A")), song("b1", Some("B")), song("b2", Some("B")), song("x", None), song("c1", Some("C")), song("c2", Some("C"))]);
        playlist_set(ids(&["a1", "a2", "b1", "b2"]), 0, false, Some(PageOrigin::new(OriginKind::ShuffleAlbums, "")));
        let r = runs();
        assert!(r[0] > 0 && r[0] == r[1] && r[2] > 0 && r[2] == r[3] && r[0] != r[2], "{r:?}");
        // The refill, from the shuffle: its album a run of its own, a song of no album in none.
        let shuffle = Some(PageOrigin::new(OriginKind::ShuffleAlbums, ""));
        playlist_take(4, ids(&["x", "c1", "c2"]), vec![Hand::No; 3], shuffle.clone());
        let r = runs();
        assert!(r[4] == 0 && r[5] > 0 && r[5] == r[6] && r[5] != r[2], "{r:?}");
        // A song added by hand to it: none. Nor songs added by no hand from nowhere (a controller's): only
        // what says it comes from the shuffle is split into its albums.
        let at = playlist_take(9, ids(&["c1"]), vec![Hand::Last], None).at;
        assert_eq!(runs()[at as usize], 0);
        let at = playlist_take(99, ids(&["c1", "c2"]), vec![Hand::No; 2], None).at as usize;
        assert_eq!(runs()[at..at + 2], [0, 0]);
        // Any other queue's refill: none.
        playlist_set(ids(&["a1"]), 0, false, None);
        playlist_take(1, ids(&["c1", "c2"]), vec![Hand::No; 2], None);
        assert_eq!(runs(), [0, 0, 0]);
    }

    /// A playlist playing, its song's album opened, a song of the album tapped: the queue is the album's,
    /// from the song tapped, and its page is the one playing (its Play reads Pause), the playlist's no
    /// longer. A tap on another song there, or on the one playing, plays the album from that song: a tap on
    /// a row is never the page's pause.
    #[test]
    fn a_song_tapped_on_an_album_page_makes_the_album_the_one_playing() {
        use nori_library::pages::{hero_buttons, HeroPress, PageQueue};
        let _g = hold(&[], 0);
        let (playlist, album) = (PageOrigin::new(OriginKind::Playlist, "pl-t"), PageOrigin::new(OriginKind::Album, "al-t"));
        let lights = |o: &PageOrigin| playlist_from(PageQueue::new(o.clone()));
        playlist_set(ids(&["p1", "t2", "p3"]), 1, false, Some(playlist.clone()));
        assert!(lights(&playlist) && !lights(&album));
        let gen = playlist_origin_gen();
        // What a tap does is the settings' (a whole list from the row by default), never a toggle.
        assert_eq!(crate::actions::tap_plan(false), crate::actions::TapPlan::PlayList);
        playlist_set(ids(&["t1", "t2", "t3"]), 1, false, Some(album.clone()));
        assert_ne!(playlist_origin_gen(), gen, "a page asks again whether it is the one playing");
        assert!(lights(&album) && !lights(&playlist));
        let b = hero_buttons(lights(&album), false, true, false, true, true);
        assert!(b.pausing && b.play_press == HeroPress::Toggle, "the album's Play reads Pause and pauses its queue");
        assert!(!hero_buttons(lights(&playlist), false, true, false, true, true).pausing, "the playlist's reads Play and starts it");
        // Another song of the page tapped, then the one playing: the album from there, still the album's.
        playlist_set(ids(&["t1", "t2", "t3"]), 2, false, Some(album.clone()));
        assert_eq!(with(|p| p.current_id().map(str::to_string)), Some("t3".into()));
        playlist_set(ids(&["t1", "t2", "t3"]), 2, false, Some(album.clone()));
        assert!(lights(&album));
        // Autofill appending, a song added, removed and moved, shuffle: still the album's queue, and its
        // songs still its album run.
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
    fn a_queue_put_back_keeps_its_album_runs() {
        let _g = hold(&[], 0);
        playlist_set(ids(&["pb1", "pb2"]), 0, false, album("PB"));
        playlist_take(9, ids(&["pb3"]), vec![Hand::Last], None);
        let saved = (with(|p| p.ids().to_vec()), runs());
        assert_eq!((saved.1[1], saved.1[0] == saved.1[2]), (0, true), "{saved:?}");
        playlist_set(ids(&["other"]), 0, false, None);
        // Put back as the platform does, with the page it came from: the saved runs, not one for all.
        playlist_put_back_runs(saved.0.clone(), saved.1.clone());
        playlist_set(saved.0.clone(), 0, false, album("PB"));
        assert_eq!(runs(), saved.1);
        // Taken by that one queue only.
        playlist_put_back_runs(saved.0.clone(), saved.1.clone());
        playlist_set(ids(&["another"]), 0, false, None);
        playlist_set(saved.0.clone(), 0, false, None);
        assert_eq!(runs(), [0, 0, 0]);
    }

    #[test]
    fn album_gain_in_auto_mode_is_for_an_album_played_as_one() {
        use nori_player::gain::GainPrefs;
        use nori_player::policy::GainMode;
        let _g = hold(&[], 0);
        let rg = nori_model::ReplayGain { track_gain: Some(-6.0), album_gain: Some(-2.0), track_peak: Some(0.5), album_peak: Some(0.5), ..Default::default() };
        let song = |id: &str, track: u32| Song { duration: 200, album_id: Some("GA".into()), track, disc_number: 1, replay_gain: Some(rg.clone()), ..Song::only_id(id.to_string()) };
        queue::queue_register(vec![song("ga1", 1), song("ga2", 2)]);
        let prefs = GainPrefs::attenuating(GainMode::Auto, 0.0, 0.0);
        let at = |id: &str, run: u32| Some((id.to_string(), run));
        let db = |g: f32| (20.0 * g.log10() * 10.0).round() / 10.0;
        assert_eq!(db(queue::queue_gain(at("ga1", 3), at("ga2", 3), None, &prefs, false, false)), -2.0, "the album played as one: album gain");
        assert_eq!(db(queue::queue_gain(at("ga1", 0), at("ga2", 0), None, &prefs, false, false)), -6.0, "two of its songs queued one at a time: track gain");
    }

    #[test]
    fn edits_say_where_the_songs_went() {
        let _g = hold(&["e1", "e2", "e3"], 0);
        assert_eq!(playlist_take(9, ids(&["n"]), vec![Hand::Next], None), QueueChange { at: 1, shuffled: false });
        assert_eq!(playlist_take(9, ids(&["i"]), vec![Hand::No], None).at, 4, "a controller's own insert, clamped to the end");
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
        playlist_take(9, ids(&["v4"]), vec![Hand::Next], None);
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
        playlist_take(0, ids(&["s3"]), vec![Hand::Last], None);
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
        playlist_take(9, ids(&["x"]), vec![Hand::Next], None);
        playlist_take(9, ids(&["y"]), vec![Hand::Last], None);
        playlist_take(9, ids(&["fill1", "fill2"]), vec![Hand::No; 2], None);
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
        playlist_take(9, ids(&["mine"]), vec![Hand::Next], None);
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
        assert!(!playlist_skips(1));
        playlist_moved_to(1);
        assert_eq!(playlist_bridge_state().current.as_deref(), Some("t2"));
        assert_eq!(playlist_to_push(), ids(&["t1", "t2"]), "radio streams are not handed to the server");
        match push_write(Some("t2".into()), 7) {
            Some(nori_net::requests::Write::SaveQueue { ids: pushed, current, position_ms }) => {
                assert_eq!((pushed, current.as_deref(), position_ms), (ids(&["t1", "t2"]), Some("t2"), 7));
            }
            w => panic!("{w:?}"),
        }
    }
}
