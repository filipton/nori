//! The queue: songs in list order, play order under shuffle, hand-added marks, current song, repeat,
//! and the offline bridge. Every change is made here first and mirrored to the platform's player.

use crate::queue::{place, shuffle, shuffle_around};

/// How a song came into the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hand {
    /// Part of the list it was played from.
    #[default]
    No,
    /// Play next.
    Next,
    /// Add to queue.
    Last,
    /// A download played by the offline bridge while the server is unreachable.
    Bridge,
}

impl Hand {
    /// Added by the user (Play next, Add to queue).
    pub fn by_user(self) -> bool {
        matches!(self, Hand::Next | Hand::Last)
    }
}

/// A list change for the platform's player: remove the ranges (last first), insert `count` at `at`,
/// then seek to `seek` if set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    pub remove: Vec<(usize, usize)>,
    pub at: usize,
    pub count: usize,
    pub seek: Option<usize>,
}

/// A removed song with what is needed to restore it (undo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    pub id: String,
    /// List index.
    pub at: usize,
    pub hand: Hand,
    /// Play order position while shuffling.
    pub turn: Option<usize>,
    /// [`Playlist::album_run`].
    pub run: u32,
}

/// media3's repeat modes, same numbers.
pub const REPEAT_OFF: u8 = 0;
pub const REPEAT_ONE: u8 = 1;
pub const REPEAT_ALL: u8 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Playlist {
    ids: Vec<String>,
    hand: Vec<Hand>,
    /// Per-song album run ([`Playlist::album_run`]), 0 for none.
    runs: Vec<u32>,
    last_run: u32,
    /// Play order while shuffling (list indexes); empty otherwise.
    order: Vec<usize>,
    shuffling: bool,
    /// Shuffle shown as on: shuffling, or a list pre-shuffled before queueing (weighted shuffle).
    lit: bool,
    cur: Option<usize>,
    /// While bridging: the song to resume when the server is back.
    parked: Option<usize>,
    repeat: u8,
    /// Bumped on every change.
    rev: u64,
    /// Bumped only when the list's songs change.
    list_rev: u64,
    /// The last single song the user removed, for undo; cleared by a new list.
    taken: Option<Taken>,
}

impl Playlist {
    pub const fn new() -> Self {
        Playlist {
            ids: Vec::new(),
            hand: Vec::new(),
            runs: Vec::new(),
            last_run: 0,
            order: Vec::new(),
            shuffling: false,
            lit: false,
            cur: None,
            parked: None,
            repeat: REPEAT_OFF,
            rev: 0,
            list_rev: 0,
            taken: None,
        }
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
    pub fn current(&self) -> Option<usize> {
        self.cur
    }
    pub fn current_id(&self) -> Option<&str> {
        self.cur.and_then(|i| self.ids.get(i)).map(String::as_str)
    }
    pub fn shuffling(&self) -> bool {
        self.shuffling
    }
    pub fn lit(&self) -> bool {
        self.lit || self.shuffling
    }
    pub fn repeat(&self) -> u8 {
        self.repeat
    }
    pub fn rev(&self) -> u64 {
        self.rev
    }
    pub fn list_rev(&self) -> u64 {
        self.list_rev
    }
    pub fn hand(&self, i: usize) -> Hand {
        self.hand.get(i).copied().unwrap_or_default()
    }
    /// The album run of list index `i`: songs of an album queued whole share a unique number
    /// ([`Playlist::as_album`]); 0 otherwise. Keeps albums gapless (`transitions::follows_on_album`).
    pub fn album_run(&self, i: usize) -> u32 {
        self.runs.get(i).copied().unwrap_or(0)
    }
    /// All album runs in list order (saved with the queue).
    pub fn album_runs(&self) -> &[u32] {
        &self.runs
    }

    /// Marks `from..to` as an album queued whole, with a new album run.
    pub fn as_album(&mut self, from: usize, to: usize) {
        let to = to.min(self.ids.len());
        if from >= to {
            return;
        }
        self.last_run += 1;
        self.runs[from..to].fill(self.last_run);
        self.rev += 1;
    }

    /// Restores saved album runs; ignored if the length differs.
    pub fn set_album_runs(&mut self, runs: &[u32]) {
        if runs.len() != self.ids.len() {
            return;
        }
        self.runs.copy_from_slice(runs);
        self.last_run = self.last_run.max(runs.iter().copied().max().unwrap_or(0));
        self.rev += 1;
    }

    pub fn parked_id(&self) -> Option<&str> {
        self.parked.and_then(|i| self.ids.get(i)).map(String::as_str)
    }
    pub fn by_hand(&self) -> impl Iterator<Item = usize> + '_ {
        self.hand.iter().enumerate().filter(|(_, h)| h.by_user()).map(|(i, _)| i)
    }

    /// The play order when shuffling.
    pub fn shuffle_order(&self) -> Option<&[usize]> {
        self.shuffling.then_some(self.order.as_slice())
    }

    /// The list indexes in the order they play.
    pub fn play_order(&self) -> impl Iterator<Item = usize> + '_ {
        let (walk, n) = if self.shuffling { (Some(&self.order), self.order.len()) } else { (None, self.ids.len()) };
        (0..n).map(move |k| walk.map_or(k, |o| o[k]))
    }

    fn position(&self, i: usize) -> Option<usize> {
        if self.shuffling {
            self.order.iter().position(|&o| o == i)
        } else {
            (i < self.ids.len()).then_some(i)
        }
    }

    fn at_position(&self, p: usize) -> Option<usize> {
        if self.shuffling {
            self.order.get(p).copied()
        } else {
            (p < self.ids.len()).then_some(p)
        }
    }

    /// The song after `i` in play order, repeat as given (media3's `getNextWindowIndex`).
    pub fn next_of(&self, i: usize, repeat: u8) -> Option<usize> {
        if repeat == REPEAT_ONE {
            return Some(i);
        }
        let p = self.position(i)?;
        match self.at_position(p + 1) {
            Some(n) => Some(n),
            None if repeat == REPEAT_ALL => self.at_position(0),
            None => None,
        }
    }

    /// The song before `i` in play order.
    pub fn previous_of(&self, i: usize, repeat: u8) -> Option<usize> {
        if repeat == REPEAT_ONE {
            return Some(i);
        }
        let p = self.position(i)?;
        match p.checked_sub(1).and_then(|q| self.at_position(q)) {
            Some(n) => Some(n),
            None if repeat == REPEAT_ALL => self.at_position(self.ids.len().checked_sub(1)?),
            None => None,
        }
    }

    /// Next/previous button targets: repeat-one skips like repeat-all.
    pub fn next(&self) -> Option<usize> {
        self.next_of(self.cur?, if self.repeat == REPEAT_ONE { REPEAT_ALL } else { self.repeat })
    }
    pub fn previous(&self) -> Option<usize> {
        self.previous_of(self.cur?, if self.repeat == REPEAT_ONE { REPEAT_ALL } else { self.repeat })
    }

    /// Songs after the current one in play order, ignoring repeat.
    pub fn songs_after(&self) -> usize {
        match self.cur.and_then(|c| self.position(c)) {
            Some(p) => self.len() - 1 - p,
            None => 0,
        }
    }

    /// The current song and those after it in play order, ignoring repeat.
    pub fn upcoming(&self) -> impl Iterator<Item = usize> + '_ {
        let from = self.cur.and_then(|c| self.position(c)).unwrap_or(self.len());
        (from..self.len()).filter_map(move |p| self.at_position(p))
    }

    /// Replaces the queue, starting at `start` (`None`: a random song when shuffling, else 0). Returns
    /// the start.
    pub fn set(&mut self, ids: Vec<String>, start: Option<usize>, shuffling: bool, seed: u64) -> Option<usize> {
        let n = ids.len();
        self.ids = ids;
        self.list_rev += 1;
        self.taken = None;
        self.hand = vec![Hand::No; n];
        self.runs = vec![0; n];
        self.parked = None;
        self.shuffling = shuffling && n > 0;
        self.lit = shuffling;
        self.cur = match (n, start) {
            (0, _) => None,
            (_, Some(s)) => Some(s.min(n - 1)),
            (_, None) if shuffling => {
                let mut all: Vec<usize> = (0..n).collect();
                shuffle(&mut all, seed);
                Some(all[0])
            }
            (_, None) => Some(0),
        };
        self.order = match (self.shuffling, self.cur) {
            (true, Some(c)) => shuffle_around(n, c, &vec![false; n], seed),
            _ => Vec::new(),
        };
        self.rev += 1;
        self.cur
    }

    /// A list already in the order it should play, shown as shuffled.
    pub fn set_ordered(&mut self, ids: Vec<String>) -> Option<usize> {
        let at = self.set(ids, Some(0), false, 0);
        self.lit = true;
        at
    }

    /// Play next / Add to queue; returns the list index. An empty queue takes them as its list.
    pub fn add(&mut self, ids: Vec<String>, hand: Hand) -> usize {
        let count = ids.len();
        let Some(cur) = self.cur.filter(|_| !self.ids.is_empty()) else {
            let at = self.ids.len();
            self.insert(at, ids, hand);
            if self.cur.is_none() && !self.ids.is_empty() {
                self.cur = Some(0);
            }
            return at;
        };
        let flags: Vec<bool> = self.hand.iter().map(|h| h.by_user()).collect();
        let p = place(self.ids.len(), cur, &flags, self.shuffling.then_some(self.order.as_slice()), hand == Hand::Last, count);
        self.splice(p.at, ids, hand);
        if let Some(o) = p.order {
            self.order = o;
        }
        self.rev += 1;
        p.at
    }

    /// Songs from a controller at `at`, with a mark per song. If all are hand-added (and the queue is not
    /// empty) they go where [`Playlist::add`] puts them (the first mark decides); otherwise at `at`.
    pub fn take(&mut self, at: usize, ids: Vec<String>, hands: &[Hand]) -> usize {
        if !self.ids.is_empty() && !hands.is_empty() && hands.iter().all(|h| h.by_user()) {
            return self.add(ids, hands[0]);
        }
        let at = at.min(self.ids.len());
        self.insert(at, ids, Hand::No);
        at
    }

    /// Shows shuffle on/off at once, before the change itself arrives.
    pub fn show_shuffle(&mut self, on: bool) {
        self.lit = on;
    }

    /// A plain insert at `at`; under shuffle the songs play after the rest.
    pub fn insert(&mut self, at: usize, ids: Vec<String>, hand: Hand) {
        let at = at.min(self.ids.len());
        let count = ids.len();
        self.splice(at, ids, hand);
        if self.shuffling {
            for o in self.order.iter_mut() {
                if *o >= at {
                    *o += count;
                }
            }
            self.order.extend(at..at + count);
        }
        self.rev += 1;
    }

    fn splice(&mut self, at: usize, ids: Vec<String>, hand: Hand) {
        let count = ids.len();
        self.ids.splice(at..at, ids);
        self.list_rev += 1;
        self.hand.splice(at..at, std::iter::repeat_n(hand, count));
        self.runs.splice(at..at, std::iter::repeat_n(0, count));
        for i in [self.cur.as_mut(), self.parked.as_mut()].into_iter().flatten() {
            if *i >= at {
                *i += count;
            }
        }
    }

    /// Removes `from..to`. If the current song goes, the next remaining in play order becomes current
    /// (else the previous), as media3 does.
    pub fn remove(&mut self, from: usize, to: usize) {
        let to = to.min(self.ids.len());
        if from >= to {
            return;
        }
        let gone = |i: usize| (from..to).contains(&i);
        let shift = |i: usize| if i >= to { i - (to - from) } else { i };
        if let Some(c) = self.cur {
            if gone(c) {
                let order: Vec<usize> = self.play_order().collect();
                let p = order.iter().position(|&o| o == c).unwrap_or(0);
                let after = order[p..].iter().find(|&&o| !gone(o)).or_else(|| order[..p].iter().rev().find(|&&o| !gone(o)));
                self.cur = after.map(|&i| shift(i));
            } else {
                self.cur = Some(shift(c));
            }
        }
        self.parked = self.parked.filter(|&h| !gone(h)).map(shift);
        self.ids.drain(from..to);
        self.list_rev += 1;
        self.hand.drain(from..to);
        self.runs.drain(from..to);
        self.order.retain(|&o| !gone(o));
        for o in self.order.iter_mut() {
            *o = shift(*o);
        }
        if self.ids.is_empty() {
            self.cur = None;
            self.shuffling = false;
            self.order.clear();
        }
        self.rev += 1;
    }

    /// [`Playlist::remove`] by the user; a single song is remembered for [`Playlist::restore_taken`].
    pub fn remove_undoably(&mut self, from: usize, to: usize) {
        self.taken = if to == from + 1 { self.taken(from) } else { None };
        self.remove(from, to);
    }

    /// Undoes the last [`Playlist::remove_undoably`] if it removed `id`; returns the list index.
    pub fn restore_taken(&mut self, id: &str) -> Option<usize> {
        let t = self.taken.take_if(|t| t.id == id)?;
        Some(self.restore(&t))
    }

    /// Snapshot of the song at `at` for [`Playlist::restore`].
    pub fn taken(&self, at: usize) -> Option<Taken> {
        let id = self.ids.get(at)?.clone();
        Some(Taken { id, at, hand: self.hand(at), turn: if self.shuffling { self.position(at) } else { None }, run: self.album_run(at) })
    }

    /// Puts a removed song back at its list index (clamped) and play order position, with its marks. The
    /// current song stays current. Returns the list index.
    pub fn restore(&mut self, t: &Taken) -> usize {
        let at = t.at.min(self.ids.len());
        let was_empty = self.ids.is_empty();
        self.splice(at, vec![t.id.clone()], t.hand);
        self.runs[at] = t.run;
        if self.shuffling {
            for o in self.order.iter_mut() {
                if *o >= at {
                    *o += 1;
                }
            }
            let turn = t.turn.unwrap_or(self.order.len()).min(self.order.len());
            self.order.insert(turn, at);
        }
        if was_empty {
            self.cur = Some(0);
        }
        self.rev += 1;
        at
    }

    /// Moves `from..to` so the first lands at `new_index` (media3's `moveMediaItems`); the shuffle order
    /// keeps each song's position.
    pub fn move_range(&mut self, from: usize, to: usize, new_index: usize) {
        let n = self.ids.len();
        let to = to.min(n);
        if from >= to || from == new_index {
            return;
        }
        let count = to - from;
        let new_index = new_index.min(n - count);
        // Old index -> new index.
        let mut map: Vec<usize> = (0..n).collect();
        let mut list: Vec<usize> = (0..n).collect();
        let moved: Vec<usize> = list.drain(from..to).collect();
        list.splice(new_index..new_index, moved);
        for (new, &old) in list.iter().enumerate() {
            map[old] = new;
        }
        let ids = std::mem::take(&mut self.ids);
        let hand = std::mem::take(&mut self.hand);
        let runs = std::mem::take(&mut self.runs);
        let mut slots: Vec<Option<((String, Hand), u32)>> = ids.into_iter().zip(hand).zip(runs).map(Some).collect();
        for &old in &list {
            let ((id, h), r) = slots[old].take().expect("each index moves once");
            self.ids.push(id);
            self.hand.push(h);
            self.runs.push(r);
        }
        self.list_rev += 1;
        self.cur = self.cur.map(|c| map[c]);
        self.parked = self.parked.map(|h| map[h]);
        for o in self.order.iter_mut() {
            *o = map[*o];
        }
        self.rev += 1;
    }

    /// Turns shuffle on ([`shuffle_around`]) or off (list order).
    pub fn set_shuffle(&mut self, on: bool, seed: u64) {
        self.lit = on;
        if on == self.shuffling {
            return;
        }
        self.shuffling = on && !self.ids.is_empty();
        let flags: Vec<bool> = self.hand.iter().map(|h| h.by_user()).collect();
        self.order = match (self.shuffling, self.cur) {
            (true, Some(c)) => shuffle_around(self.ids.len(), c, &flags, seed),
            (true, None) => {
                let mut all: Vec<usize> = (0..self.ids.len()).collect();
                shuffle(&mut all, seed);
                all
            }
            _ => Vec::new(),
        };
        self.rev += 1;
    }

    /// The offline bridge's downloads are in the queue.
    pub fn bridging(&self) -> bool {
        self.hand.contains(&Hand::Bridge)
    }

    /// The next song is the parked one (the bridge ran out of downloads).
    pub fn next_is_parked(&self) -> bool {
        self.parked.is_some() && self.cur.and_then(|c| self.next_of(c, REPEAT_OFF)) == self.parked
    }

    /// Inserts downloads to play while offline: the first time before the current (failed) song, which is
    /// parked, and playing the first; later just before the parked song. Given order, even under shuffle.
    pub fn bridge(&mut self, ids: Vec<String>) -> Option<Splice> {
        if ids.is_empty() {
            return None;
        }
        let (before, starting) = match (self.parked, self.cur) {
            (Some(h), _) => (h, false),
            (None, Some(c)) => (c, true),
            (None, None) => return None,
        };
        let count = ids.len();
        let pos = self.position(before);
        self.splice(before, ids, Hand::Bridge);
        if self.shuffling {
            for o in self.order.iter_mut() {
                if *o >= before {
                    *o += count;
                }
            }
            let pos = pos.unwrap_or(self.order.len());
            self.order.splice(pos..pos, before..before + count);
        }
        if starting {
            self.parked = Some(before + count);
            self.cur = Some(before);
        }
        self.rev += 1;
        Some(Splice { remove: Vec::new(), at: before, count, seek: starting.then_some(before) })
    }

    /// Server back: removes the bridge's songs and plays the parked song.
    pub fn unbridge(&mut self) -> Option<Splice> {
        if !self.bridging() {
            return None;
        }
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for (i, h) in self.hand.iter().enumerate() {
            if *h == Hand::Bridge {
                match runs.last_mut() {
                    Some((_, to)) if *to == i => *to = i + 1,
                    _ => runs.push((i, i + 1)),
                }
            }
        }
        runs.reverse();
        let head = self.parked;
        // Make the parked song current first so removing the bridge does not move it.
        if let Some(h) = head {
            self.cur = Some(h);
        }
        for &(from, to) in &runs {
            self.remove(from, to);
        }
        let seek = self.parked.take().or(if self.ids.is_empty() { None } else { Some(0) });
        self.cur = seek;
        self.rev += 1;
        Some(Splice { remove: runs, at: 0, count: 0, seek })
    }

    pub fn set_repeat(&mut self, mode: u8) {
        self.repeat = mode;
    }

    /// The player moved to `index` by itself.
    pub fn moved_to(&mut self, index: usize) {
        if index < self.ids.len() && self.cur != Some(index) {
            self.cur = Some(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn list(p: &Playlist) -> Vec<&str> {
        p.ids().iter().map(String::as_str).collect()
    }

    fn played(p: &Playlist) -> Vec<&str> {
        p.play_order().map(|i| p.ids()[i].as_str()).collect()
    }

    #[test]
    fn restore_puts_song_back() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d"]), Some(0), false, 0);
        p.add(ids(&["x"]), Hand::Next);
        assert_eq!(list(&p), ["a", "x", "b", "c", "d"]);
        let t = p.taken(1).unwrap();
        assert_eq!(t, Taken { id: "x".into(), at: 1, hand: Hand::Next, turn: None, run: 0 });
        p.remove(1, 2);
        assert_eq!(list(&p), ["a", "b", "c", "d"]);
        assert_eq!(p.restore(&t), 1);
        assert_eq!(list(&p), ["a", "x", "b", "c", "d"]);
        assert_eq!(p.by_hand().collect::<Vec<_>>(), [1], "still one added by hand");
        assert_eq!(p.current(), Some(0));
        assert_eq!(p.next(), Some(1), "and it plays next again");

        // Current song after the removed one moves with the list.
        p.moved_to(3);
        let t = p.taken(1).unwrap();
        p.remove(1, 2);
        assert_eq!(p.current_id(), Some("c"));
        p.restore(&t);
        assert_eq!((p.current(), p.current_id()), (Some(3), Some("c")));
        assert!(p.taken(9).is_none());
    }

    fn runs(p: &Playlist) -> Vec<u32> {
        p.album_runs().to_vec()
    }

    #[test]
    fn album_runs_survive_edits() {
        let mut p = Playlist::default();
        p.set(ids(&["a1", "a2", "a3"]), Some(0), false, 0);
        assert_eq!(runs(&p), [0, 0, 0], "a queue is no album until it is said to be");
        p.as_album(0, 3);
        let a = p.album_run(0);
        assert!(a > 0);
        assert_eq!(runs(&p), [a, a, a]);
        // Other songs get no run and split the album's.
        p.add(ids(&["x"]), Hand::Next);
        p.insert(4, ids(&["fill"]), Hand::No);
        assert_eq!(list(&p), ["a1", "x", "a2", "a3", "fill"]);
        assert_eq!(runs(&p), [a, 0, a, a, 0]);
        // The same album again gets a new run.
        let at = p.add(ids(&["a1", "a2"]), Hand::Last);
        p.as_album(at, at + 2);
        let b = p.album_run(at);
        assert!(b != a && b > 0);
        assert_eq!(list(&p), ["a1", "x", "a1", "a2", "a2", "a3", "fill"]);
        assert_eq!(runs(&p), [a, 0, b, b, a, a, 0]);
        let t = p.taken(1).unwrap();
        p.remove(1, 2);
        assert_eq!(runs(&p), [a, b, b, a, a, 0]);
        let t2 = p.taken(1).unwrap();
        p.remove(1, 2);
        assert_eq!(runs(&p), [a, b, a, a, 0]);
        p.restore(&t2);
        p.restore(&t);
        assert_eq!(runs(&p), [a, 0, b, b, a, a, 0]);
        p.move_range(3, 4, 6);
        assert_eq!(list(&p), ["a1", "x", "a1", "a2", "a3", "fill", "a2"]);
        assert_eq!(runs(&p), [a, 0, b, a, a, 0, b]);
        let saved = runs(&p);
        p.set(ids(&["a1", "x", "a1", "a2", "a3", "fill", "a2"]), Some(0), false, 0);
        assert_eq!(runs(&p), [0; 7]);
        p.set_album_runs(&saved);
        assert_eq!(runs(&p), saved);
        p.as_album(5, 7);
        assert!(p.album_run(5) > b, "a run handed out after a queue put back is still a new one");
        p.set_album_runs(&[1, 1]);
        assert_eq!(p.album_run(0), a, "runs for another list are not put on this one");
    }

    #[test]
    fn restore_under_shuffle_keeps_turn() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d", "e", "f"]), Some(2), true, 11);
        let before: Vec<String> = played(&p).iter().map(|s| s.to_string()).collect();
        let at = p.play_order().nth(3).unwrap();
        let t = p.taken(at).unwrap();
        assert_eq!(t.turn, Some(3));
        p.remove(at, at + 1);
        assert_eq!(p.len(), 5);
        assert_eq!(p.restore(&t), at);
        assert_eq!(played(&p), before, "the same play order as before it went");
        assert_eq!(p.current_id(), Some("c"));
    }

    #[test]
    fn restored_current_song_does_not_play() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c"]), Some(1), false, 0);
        let t = p.taken(1).unwrap();
        p.remove(1, 2);
        assert_eq!(p.current_id(), Some("c"), "the next one plays");
        p.restore(&t);
        assert_eq!(list(&p), ["a", "b", "c"]);
        assert_eq!(p.current_id(), Some("c"), "the music does not go back to it");

        let mut p = Playlist::default();
        p.set(ids(&["only"]), Some(0), false, 0);
        let t = p.taken(0).unwrap();
        p.remove(0, 1);
        assert!(p.is_empty() && p.current().is_none());
        p.restore(&t);
        assert_eq!((list(&p), p.current()), (vec!["only"], Some(0)));

        // Clamped to a shrunken queue.
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d"]), Some(0), false, 0);
        let t = p.taken(3).unwrap();
        p.remove(1, 4);
        assert_eq!(p.restore(&t), 1);
        assert_eq!(list(&p), ["a", "d"]);
    }

    #[test]
    fn plain_queue_order_and_repeat() {
        let mut p = Playlist::default();
        assert_eq!(p.set(ids(&["a", "b", "c"]), Some(1), false, 0), Some(1));
        assert_eq!((p.next(), p.previous(), p.songs_after()), (Some(2), Some(0), 1));
        assert_eq!(p.upcoming().collect::<Vec<_>>(), [1, 2]);
        p.set_repeat(REPEAT_ALL);
        p.moved_to(2);
        assert_eq!(p.next(), Some(0));
        p.set_repeat(REPEAT_ONE);
        assert_eq!(p.next_of(2, REPEAT_ONE), Some(2), "a song ending under repeat-one plays again");
        assert_eq!(p.next(), Some(0), "but next still skips");
    }

    #[test]
    fn shuffle_keeps_current_first() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d", "e"]), Some(2), false, 0);
        p.set_shuffle(true, 7);
        assert_eq!(played(&p)[0], "c");
        assert_eq!(p.songs_after(), 4);
        p.set_shuffle(false, 0);
        assert_eq!(played(&p), ["a", "b", "c", "d", "e"]);
        assert!(!p.lit());
    }

    #[test]
    fn shuffled_start_picks_first_song() {
        let mut p = Playlist::default();
        let start = p.set(ids(&["a", "b", "c", "d"]), None, true, 99).unwrap();
        assert_eq!(p.play_order().next(), Some(start));
        assert!(p.lit() && p.shuffling());
    }

    #[test]
    fn play_next_and_add_to_queue_under_shuffle() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d"]), Some(1), true, 3);
        assert_eq!(played(&p)[0], "b");
        p.add(ids(&["x"]), Hand::Last);
        p.add(ids(&["y"]), Hand::Last);
        p.add(ids(&["z"]), Hand::Next);
        assert_eq!(&played(&p)[..4], ["b", "z", "x", "y"]);
        assert_eq!(list(&p)[..5], ["a", "b", "z", "x", "y"]);
        assert_eq!(p.by_hand().collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(p.current_id(), Some("b"));
    }

    #[test]
    fn take_places_by_mark() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c"]), Some(0), false, 0);
        assert_eq!(p.take(99, ids(&["x", "y"]), &[Hand::Last, Hand::Last]), 1, "added by hand: after the playing song");
        assert_eq!(p.take(99, ids(&["n"]), &[Hand::Next]), 1, "play next: right after it, ahead of the others");
        assert_eq!(p.take(99, ids(&["l"]), &[Hand::Last]), 4, "add to queue: after the ones added before");
        assert_eq!(p.take(1, ids(&["m"]), &[Hand::Next, Hand::No]), 1, "not all by hand: the controller's own insert");
        assert_eq!(p.take(99, ids(&["e"]), &[Hand::No]), 8, "an insert past the end lands at the end");
        let mut empty = Playlist::default();
        assert_eq!(empty.take(5, ids(&["q"]), &[Hand::Next]), 0, "an empty queue takes them as its list");
        assert_eq!(empty.hand(0), Hand::No);
    }

    #[test]
    fn shuffle_shown_state() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b"]), Some(0), false, 0);
        p.show_shuffle(true);
        assert!(p.lit() && !p.shuffling(), "asked for before the queue changed");
        p.set_ordered(ids(&["b", "a"]));
        assert!(p.lit() && !p.shuffling(), "a weighted shuffle stays lit with the player's shuffle off");
        p.add(ids(&["c"]), Hand::Next);
        assert!(p.lit(), "editing the queue keeps it lit");
        p.show_shuffle(false);
        p.set_shuffle(false, 0);
        assert!(!p.lit(), "turned off");
        p.set(ids(&["a"]), Some(0), false, 0);
        assert!(!p.lit(), "a plain play");
    }

    #[test]
    fn removing_current_moves_on() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c"]), Some(1), false, 0);
        p.remove(1, 2);
        assert_eq!((list(&p), p.current_id()), (vec!["a", "c"], Some("c")));
        p.remove(1, 2);
        assert_eq!(p.current_id(), Some("a"), "the last one gone: the one before");
        p.remove(0, 1);
        assert_eq!((p.current(), p.shuffling()), (None, false));
    }

    #[test]
    fn removing_under_shuffle_keeps_order() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d", "e"]), Some(0), true, 11);
        let before: Vec<String> = played(&p).iter().map(|s| s.to_string()).collect();
        p.remove(2, 3);
        let expect: Vec<&str> = before.iter().map(String::as_str).filter(|s| *s != "c").collect();
        assert_eq!(played(&p), expect);
    }

    #[test]
    fn move_range_like_media3() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d"]), Some(0), false, 0);
        p.move_range(0, 1, 2);
        assert_eq!((list(&p), p.current_id()), (vec!["b", "c", "a", "d"], Some("a")));
        p.move_range(3, 4, 0);
        assert_eq!(list(&p), ["d", "b", "c", "a"]);
        assert_eq!(p.current(), Some(3));
    }

    #[test]
    fn empty_queue_takes_added_songs() {
        let mut p = Playlist::default();
        assert_eq!(p.add(ids(&["a", "b"]), Hand::Last), 0);
        assert_eq!((list(&p), p.current()), (vec!["a", "b"], Some(0)));
    }

    #[test]
    fn bridge_parks_and_restores() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d"]), Some(1), false, 0);
        // "b" failed: two downloads play before it.
        assert_eq!(p.bridge(ids(&["x", "y"])), Some(Splice { remove: vec![], at: 1, count: 2, seek: Some(1) }));
        assert_eq!((list(&p), p.current_id()), (vec!["a", "x", "y", "b", "c", "d"], Some("x")));
        assert!(p.bridging() && !p.next_is_parked());
        assert!(p.by_hand().next().is_none(), "bridge songs are not the user's");
        p.moved_to(2);
        assert!(p.next_is_parked());
        assert_eq!(p.bridge(ids(&["z"])), Some(Splice { remove: vec![], at: 3, count: 1, seek: None }));
        assert_eq!((list(&p), p.current_id()), (vec!["a", "x", "y", "z", "b", "c", "d"], Some("y")));
        assert_eq!(p.unbridge(), Some(Splice { remove: vec![(1, 4)], at: 0, count: 0, seek: Some(1) }));
        assert_eq!((list(&p), p.current_id(), p.bridging()), (vec!["a", "b", "c", "d"], Some("b"), false));
        assert_eq!(p.unbridge(), None);
    }

    #[test]
    fn bridge_under_shuffle() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c", "d"]), Some(2), true, 9);
        let parked = played(&p).iter().map(|s| s.to_string()).collect::<Vec<_>>();
        p.bridge(ids(&["x", "y"]));
        let now = played(&p);
        assert_eq!(&now[..3], ["x", "y", "c"], "the bridge, then the parked song, then the rest as before");
        assert_eq!(now[3..].iter().map(|s| s.to_string()).collect::<Vec<_>>(), parked[1..]);
    }

    #[test]
    fn insert_under_shuffle_plays_last() {
        let mut p = Playlist::default();
        p.set(ids(&["a", "b", "c"]), Some(0), true, 5);
        p.insert(1, ids(&["x"]), Hand::No);
        assert_eq!(list(&p), ["a", "x", "b", "c"]);
        assert_eq!(*played(&p).last().unwrap(), "x");
        assert_eq!(p.current_id(), Some("a"));
    }
}
