//! What a controlled device does with a command, by who sent it and how its queue stands now, and what a
//! jam member's player controls reach.

use crate::wire::{Op, Refusal, Role};

/// Who sent a command, as the link it came over says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    /// Another device of the same account (the account's room, or the LAN with the account's proof).
    Owner,
    /// A member of the jam this device hosts.
    Member(Role),
}

/// Whether the platform's player may carry out `op` from `from`, the queue at revision `rev` with `len`
/// songs. Jam ops are the jam's ([`crate::jam::Jam::apply`]).
pub fn admit(op: &Op, from: Sender, rev: u64, len: u32) -> Result<(), Refusal> {
    if let Sender::Member(role) = from {
        if Controls::of(role, false).reach(op) != Reach::Jam {
            return Err(Refusal::NotAllowed);
        }
    }
    let at = |index: u32, of: u64| {
        if of != rev {
            Err(Refusal::Stale)
        } else if index >= len {
            Err(Refusal::Unknown)
        } else {
            Ok(())
        }
    };
    match *op {
        Op::Jump { index, rev } | Op::Remove { index, rev } => at(index, rev),
        Op::Move { from, to, rev } => at(from, rev).and(at(to, rev)),
        Op::Restore { index, .. } if index > len => Err(Refusal::Unknown),
        _ => Ok(()),
    }
}

/// Where a jam member's player control acts, as in Spotify's Jam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Reach {
    /// Not offered.
    Nowhere,
    /// This device's own listening.
    Here,
    /// The host's playback, which every listener follows.
    Jam,
}

/// What each of a jam member's player controls reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Controls {
    pub play_pause: Reach,
    /// Next, previous, and a song of the queue picked.
    pub skip: Reach,
    pub seek: Reach,
    /// Moving songs in the queue.
    pub reorder: Reach,
    pub volume: Reach,
}

impl Controls {
    /// The controls of a member with `role`, `listening` along (the jam's music plays on its device) or
    /// not: the host's and admins' control the jam; a guest's play and pause, and everyone's volume, act
    /// only where the music plays.
    pub fn of(role: Role, listening: bool) -> Controls {
        let here = if listening { Reach::Here } else { Reach::Nowhere };
        let (jam, play_pause) = match role {
            Role::Host => (Reach::Here, Reach::Here),
            Role::Admin => (Reach::Jam, Reach::Jam),
            Role::Guest => (Reach::Nowhere, here),
        };
        Controls { play_pause, skip: jam, seek: jam, reorder: jam, volume: here }
    }

    /// What `op`, one of the player's controls, reaches.
    pub fn reach(&self, op: &Op) -> Reach {
        match op {
            Op::Play | Op::Pause => self.play_pause,
            Op::Next | Op::Previous | Op::Jump { .. } => self.skip,
            Op::Seek { .. } => self.seek,
            Op::Move { .. } => self.reorder,
            Op::Volume { .. } => self.volume,
            _ => Reach::Nowhere,
        }
    }
}

/// Whether `op` is a jam's to apply rather than the player's.
pub fn is_jam(op: &Op) -> bool {
    matches!(op, Op::Request { .. } | Op::Decide { .. } | Op::Promote { .. } | Op::Kick { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_admitted() {
        let jump = |index, rev| Op::Jump { index, rev };
        let cases: &[(Op, Sender, Result<(), Refusal>)] = &[
            (Op::Play, Sender::Owner, Ok(())),
            (jump(2, 7), Sender::Owner, Ok(())),
            (jump(2, 6), Sender::Owner, Err(Refusal::Stale)),
            (jump(3, 7), Sender::Owner, Err(Refusal::Unknown)),
            (Op::Move { from: 0, to: 3, rev: 7 }, Sender::Owner, Err(Refusal::Unknown)),
            (Op::Remove { index: 1, rev: 8 }, Sender::Owner, Err(Refusal::Stale)),
            (Op::Restore { song: Default::default(), index: 3 }, Sender::Owner, Ok(())),
            (Op::Restore { song: Default::default(), index: 4 }, Sender::Owner, Err(Refusal::Unknown)),
            (Op::Clear, Sender::Member(Role::Guest), Err(Refusal::NotAllowed)),
            (Op::Pause, Sender::Member(Role::Guest), Err(Refusal::NotAllowed)),
            (Op::Next, Sender::Member(Role::Guest), Err(Refusal::NotAllowed)),
            (Op::Pause, Sender::Member(Role::Admin), Ok(())),
            (Op::Seek { ms: 5 }, Sender::Member(Role::Admin), Ok(())),
            (Op::Move { from: 0, to: 2, rev: 7 }, Sender::Member(Role::Admin), Ok(())),
            (Op::Move { from: 0, to: 2, rev: 6 }, Sender::Member(Role::Admin), Err(Refusal::Stale)),
            (jump(2, 7), Sender::Member(Role::Admin), Ok(())),
            (Op::Clear, Sender::Member(Role::Admin), Err(Refusal::NotAllowed)),
            (Op::Volume { percent: 5 }, Sender::Member(Role::Admin), Err(Refusal::NotAllowed)),
        ];
        for (op, from, want) in cases {
            assert_eq!(admit(op, *from, 7, 3), *want, "{op:?} from {from:?}");
        }
    }

    #[test]
    fn controls_reach_by_role() {
        use Reach::*;
        // (role, listening along, play and pause, skip, seek, reorder, volume)
        let cases = [
            (Role::Admin, true, Jam, Jam, Jam, Jam, Here),
            (Role::Admin, false, Jam, Jam, Jam, Jam, Nowhere),
            (Role::Guest, true, Here, Nowhere, Nowhere, Nowhere, Here),
            (Role::Guest, false, Nowhere, Nowhere, Nowhere, Nowhere, Nowhere),
        ];
        for (role, listening, play_pause, skip, seek, reorder, volume) in cases {
            let c = Controls::of(role, listening);
            let reached = [Op::Pause, Op::Previous, Op::Seek { ms: 0 }, Op::Move { from: 0, to: 1, rev: 0 }, Op::Volume { percent: 1 }].map(|op| c.reach(&op));
            assert_eq!(reached, [play_pause, skip, seek, reorder, volume], "{role:?}, listening {listening}");
        }
    }
}
