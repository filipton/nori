//! What a controlled device does with a command, by who sent it and how its queue stands now.

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
    if from != Sender::Owner {
        return Err(Refusal::NotAllowed);
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
        _ => Ok(()),
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
            (Op::Pause, Sender::Member(Role::Admin), Err(Refusal::NotAllowed)),
            (Op::Next, Sender::Member(Role::Guest), Err(Refusal::NotAllowed)),
        ];
        for (op, from, want) in cases {
            assert_eq!(admit(op, *from, 7, 3), *want, "{op:?} from {from:?}");
        }
    }
}
