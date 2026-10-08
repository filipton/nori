//! A jam as its host keeps it: who is in it with which role, the songs asked for and waiting, and who
//! asked for the songs that went in. Only the host's device plays.

use std::collections::HashMap;

use nori_model::{is_provider_id, Song};

use crate::wire::{Entry, JamMember, JamState, Member, Op, Pending, Refusal, Role};

/// Requests one member may have waiting at once: a guest cannot fill the list (each provider song
/// accepted is a download).
pub const MAX_WAITING: usize = 5;

/// Who applies a jam op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By<'a> {
    /// The host's own account: its device or another of its devices.
    Host,
    /// A member, by the id the relay stamped.
    Member(&'a str),
}

struct Waiting {
    request: u64,
    from: String,
    from_name: String,
    song: Song,
}

pub struct Jam {
    /// The relay's room.
    pub room: String,
    /// What a guest joins with: the link and QR code carry it.
    pub invite: String,
    host: JamMember,
    /// The members present now, the host not among them.
    members: Vec<JamMember>,
    /// Roles given, kept while a member is away.
    roles: HashMap<String, Role>,
    waiting: Vec<Waiting>,
    next_request: u64,
    /// Who asked for each song that went in, by song id, for "added by".
    added: HashMap<String, String>,
}

impl Jam {
    pub fn new(room: String, invite: String, host_id: String, host_name: String) -> Jam {
        Jam { room, invite, host: JamMember { id: host_id, name: host_name, role: Role::Host }, members: Vec::new(), roles: HashMap::new(), waiting: Vec::new(), next_request: 1, added: HashMap::new() }
    }

    /// Takes the members the relay lists in the jam's room now; true when that changed who is in.
    /// Requests of a member who left stay.
    pub fn present(&mut self, listed: &[Member]) -> bool {
        let now: Vec<JamMember> = listed
            .iter()
            .filter(|m| m.id != self.host.id)
            .map(|m| JamMember { id: m.id.clone(), name: m.name.clone(), role: self.roles.get(&m.id).copied().unwrap_or(Role::Guest) })
            .collect();
        let changed = now != self.members;
        self.members = now;
        changed
    }

    fn role(&self, by: By) -> Option<Role> {
        match by {
            By::Host => Some(Role::Host),
            By::Member(id) => self.members.iter().find(|m| m.id == id).map(|m| m.role),
        }
    }

    fn name(&self, by: By) -> String {
        match by {
            By::Host => self.host.name.clone(),
            By::Member(id) => self.members.iter().find(|m| m.id == id).map_or_else(String::new, |m| m.name.clone()),
        }
    }

    /// Applies a jam op; the song to add to the queue when one goes in.
    pub fn apply(&mut self, by: By, op: Op) -> Result<Option<Song>, Refusal> {
        let role = self.role(by).ok_or(Refusal::NotAllowed)?;
        let decides = matches!(role, Role::Host | Role::Admin);
        match op {
            Op::Request { song } if decides => {
                self.added.insert(song.id.clone(), self.name(by));
                Ok(Some(song))
            }
            Op::Request { song } => {
                let By::Member(from) = by else { return Err(Refusal::NotAllowed) };
                if self.waiting.iter().any(|w| w.song.id == song.id) {
                    return Ok(None);
                }
                if self.waiting.iter().filter(|w| w.from == from).count() >= MAX_WAITING {
                    return Err(Refusal::TooMany);
                }
                let request = self.next_request;
                self.next_request += 1;
                self.waiting.push(Waiting { request, from: from.to_string(), from_name: self.name(by), song });
                Ok(None)
            }
            Op::Decide { request, accept } if decides => {
                let at = self.waiting.iter().position(|w| w.request == request).ok_or(Refusal::Unknown)?;
                let w = self.waiting.remove(at);
                Ok(accept.then(|| {
                    self.added.insert(w.song.id.clone(), w.from_name);
                    w.song
                }))
            }
            Op::Promote { member, admin } if role == Role::Host => {
                let m = self.members.iter_mut().find(|m| m.id == member).ok_or(Refusal::Unknown)?;
                m.role = if admin { Role::Admin } else { Role::Guest };
                self.roles.insert(member, m.role);
                Ok(None)
            }
            Op::Kick { member } if role == Role::Host => {
                let at = self.members.iter().position(|m| m.id == member).ok_or(Refusal::Unknown)?;
                self.members.remove(at);
                self.roles.remove(&member);
                self.waiting.retain(|w| w.from != member);
                Ok(None)
            }
            _ => Err(Refusal::NotAllowed),
        }
    }

    /// Who asked for song `id`, if it came in through the jam.
    pub fn added_by(&self, id: &str) -> Option<String> {
        self.added.get(id).cloned()
    }

    pub fn state(&self) -> JamState {
        JamState {
            members: std::iter::once(&self.host).chain(&self.members).cloned().collect(),
            pending: self
                .waiting
                .iter()
                .map(|w| Pending { request: w.request, from: w.from.clone(), from_name: w.from_name.clone(), song: Entry::of(0, 0, &w.song, None), provider: is_provider_id(&w.song.id) })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str) -> Song {
        Song { id: id.into(), title: id.into(), ..Default::default() }
    }

    fn member(id: &str) -> Member {
        Member { id: id.into(), name: id.to_uppercase(), ..Default::default() }
    }

    fn jam() -> Jam {
        let mut j = Jam::new("j".into(), "k".into(), "host".into(), "Host".into());
        assert!(j.present(&[member("host"), member("ann"), member("bob")]));
        j
    }

    fn request(j: &mut Jam, by: By, id: &str) -> Result<Option<Song>, Refusal> {
        j.apply(by, Op::Request { song: song(id) })
    }

    #[test]
    fn guests_ask_admins_and_the_host_decide() {
        let mut j = jam();
        assert_eq!(request(&mut j, By::Member("ann"), "s1"), Ok(None), "a guest's request waits");
        assert_eq!(request(&mut j, By::Member("bob"), "s1"), Ok(None), "the same song asked twice waits once");
        assert_eq!(j.state().pending.len(), 1);
        let req = j.state().pending[0].request;
        assert_eq!(j.apply(By::Member("bob"), Op::Decide { request: req, accept: true }), Err(Refusal::NotAllowed), "a guest cannot accept");
        assert_eq!(j.apply(By::Member("ann"), Op::Promote { member: "bob".into(), admin: true }), Err(Refusal::NotAllowed));
        assert_eq!(j.apply(By::Host, Op::Promote { member: "bob".into(), admin: true }), Ok(None));
        assert_eq!(j.apply(By::Member("bob"), Op::Decide { request: req, accept: true }), Ok(Some(song("s1"))), "an admin accepts");
        assert_eq!(j.added_by("s1").as_deref(), Some("ANN"), "added by whoever asked");
        assert_eq!(j.apply(By::Member("bob"), Op::Decide { request: req, accept: true }), Err(Refusal::Unknown), "decided once");
        assert_eq!(request(&mut j, By::Member("bob"), "s2"), Ok(Some(song("s2"))), "an admin's request goes straight in");
        assert_eq!(request(&mut j, By::Host, "s3"), Ok(Some(song("s3"))));
        assert_eq!(request(&mut j, By::Member("eve"), "s4"), Err(Refusal::NotAllowed), "not a member");

        request(&mut j, By::Member("ann"), "s5").unwrap();
        let req = j.state().pending[0].request;
        assert_eq!(j.apply(By::Host, Op::Decide { request: req, accept: false }), Ok(None), "declined");
        assert!(j.state().pending.is_empty() && j.added_by("s5").is_none());
    }

    #[test]
    fn waiting_requests_are_capped_and_follow_their_member() {
        let mut j = jam();
        for i in 0..MAX_WAITING {
            request(&mut j, By::Member("ann"), &format!("a{i}")).unwrap();
        }
        assert_eq!(request(&mut j, By::Member("ann"), "more"), Err(Refusal::TooMany));
        request(&mut j, By::Member("bob"), "b").unwrap();

        // Away for a while: requests and role stay.
        j.apply(By::Host, Op::Promote { member: "bob".into(), admin: true }).unwrap();
        assert!(j.present(&[member("host"), member("ann")]));
        assert_eq!(j.state().pending.len(), MAX_WAITING + 1);
        j.present(&[member("host"), member("ann"), member("bob")]);
        assert_eq!(j.state().members.iter().find(|m| m.id == "bob").map(|m| m.role), Some(Role::Admin));

        assert_eq!(j.apply(By::Host, Op::Kick { member: "ann".into() }), Ok(None));
        let st = j.state();
        assert_eq!(st.pending.iter().map(|p| p.from.as_str()).collect::<Vec<_>>(), ["bob"], "a kicked member's requests go too");
        assert_eq!(st.members.iter().map(|m| (m.id.as_str(), m.role)).collect::<Vec<_>>(), [("host", Role::Host), ("bob", Role::Admin)]);
    }

    #[test]
    fn provider_requests_are_marked() {
        let mut j = jam();
        request(&mut j, By::Member("ann"), "ext-deezer-song-9").unwrap();
        request(&mut j, By::Member("ann"), "s1").unwrap();
        let provider: Vec<bool> = j.state().pending.iter().map(|p| p.provider).collect();
        assert_eq!(provider, [true, false]);
    }
}
