//! The JSON both the relay (octo-fiesta's `noriRemote.*`) and a device's LAN door speak. A poll answers
//! with the rooms the caller is in (their members, each with the state it last published) and the
//! events addressed to it; a send publishes the sender's state or posts an event.

use nori_model::Song;
use serde::{Deserialize, Serialize};

/// How long the relay and the door hold a poll with nothing new. Under the 60 s read timeout of common
/// reverse proxies.
pub const HOLD_MS: u32 = 50_000;

/// What a device is, for its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    #[default]
    Phone,
    Desktop,
    Terminal,
    /// A jam guest without an account.
    Guest,
}

/// One member of a room as the relay lists it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Member {
    pub id: String,
    pub name: String,
    #[serde(deserialize_with = "lenient")]
    pub kind: DeviceKind,
    /// The state it last published; None for a controller or a jam guest.
    #[serde(deserialize_with = "lenient")]
    pub state: Option<DeviceState>,
}

/// A value another version of nori wrote differently reads as its default rather than failing the
/// whole answer.
fn lenient<'de, D: serde::Deserializer<'de>, T: Deserialize<'de> + Default>(d: D) -> Result<T, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(T::deserialize(v).unwrap_or_default())
}

/// A room: the caller's account (its devices) or a jam.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Room {
    pub room: String,
    /// Whether this is a jam rather than the account's own room.
    pub jam: bool,
    pub members: Vec<Member>,
}

/// An event addressed to the caller (or to everyone in its room). `from` is stamped by the relay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub seq: u64,
    pub room: String,
    pub from: String,
    pub body: Body,
}

/// A poll's answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Answer {
    /// The newest sequence; the next poll asks for what comes after it.
    pub seq: u64,
    /// The caller's member id, as events from it are stamped.
    pub you: String,
    pub rooms: Vec<Room>,
    #[serde(deserialize_with = "readable")]
    pub events: Vec<Event>,
}

/// The events this version reads; one from a newer client (an op it does not know) is passed over
/// rather than failing the whole answer.
fn readable<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Event>, D::Error> {
    let all = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(all.into_iter().filter_map(|e| Event::deserialize(e).ok()).collect())
}

/// What a send carries: the sender's state, an event, or both.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Outgoing {
    /// None: the account's room.
    pub room: Option<String>,
    /// None: everyone in the room.
    pub to: Option<String>,
    pub state: Option<DeviceState>,
    pub body: Option<Body>,
}

/// An event's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Body {
    /// `id` is the sender's own sequence, echoed by the ack.
    Command { id: u64, op: Box<Op> },
    /// None: done.
    Ack { id: u64, refusal: Option<Refusal> },
    /// The answer to [`Op::Page`]: the queue at revision `rev`, in play order from turn `from`.
    Page { id: u64, rev: u64, from: u32, entries: Vec<Entry> },
    /// The answer to [`Op::Clock`]: the device received it at `t2` and answered at `t3`, on its clock
    /// ([`crate::clock`]). A door answers its time endpoint with it too.
    Clock { t1: i64, t2: i64, t3: i64 },
}

/// What a controller asks of a device. Index-based edits carry the queue revision they were made
/// against (`DeviceState::rev`); a device whose queue changed since refuses them as [`Refusal::Stale`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[serde(tag = "op", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant, reason = "uniffi cannot pass a boxed field; ops are few and short-lived")]
pub enum Op {
    Play,
    Pause,
    Seek { ms: i64 },
    Next,
    Previous,
    /// Plays list index `index`.
    Jump { index: u32, rev: u64 },
    Remove { index: u32, rev: u64 },
    Move { from: u32, to: u32, rev: u64 },
    /// Removes every song after the current one in play order.
    Clear,
    /// Undoes a [`Op::Remove`]: `song` goes back where it was in the device's queue (its turn under
    /// shuffle too), or at list index `index` if the device no longer has it as taken out.
    Restore { song: Song, index: u32 },
    /// After the current song (`next`) or at the end.
    Add { songs: Vec<Song>, next: bool },
    /// A new queue, from list index `index` at `position_ms`, playing or paused. Under `shuffle` it plays
    /// in `order` (list indexes), or in an order of the device's own when that is None.
    Replace {
        songs: Vec<Song>,
        index: u32,
        position_ms: i64,
        play: bool,
        #[serde(default)]
        order: Option<Vec<u32>>,
        #[serde(default)]
        shuffle: bool,
        /// Media3 numbering, as [`Op::Repeat`].
        #[serde(default)]
        repeat: u8,
    },
    /// The device's media volume, 0 to 100.
    Volume { percent: u8 },
    Shuffle { on: bool },
    /// Media3 numbering: off 0, one 1, all 2.
    Repeat { mode: u8 },
    /// Hands the queue and position to device `to`, which plays on; this one pauses.
    Transfer { to: String },
    /// Favourites song `id` (or not) where the device shows it.
    Star { id: String, on: bool },
    /// Asks for `count` songs of the queue from turn `from` of its play order; answered with [`Body::Page`].
    Page { from: u32, count: u32 },
    /// A time exchange sent at `t1` on the sender's clock; answered with [`Body::Clock`].
    Clock { t1: i64 },
    /// A jam member asks for a song.
    Request { song: Song },
    /// The host or an admin accepts or declines request `request`.
    Decide { request: u64, accept: bool },
    /// The host makes `member` an admin or a guest again.
    Promote { member: String, admin: bool },
    /// The host sends `member` out of the jam.
    Kick { member: String },
}

/// Why a device did not do a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[serde(rename_all = "camelCase")]
pub enum Refusal {
    /// The queue changed since the controller read it.
    Stale,
    /// The sender's role does not allow it.
    NotAllowed,
    /// No such request, member or list index.
    Unknown,
    /// The sender already has as many requests waiting as a jam allows.
    TooMany,
}

/// A song as a controller lists it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default, rename_all = "camelCase")]
pub struct Entry {
    /// The list index commands name it by.
    pub index: u32,
    /// Its place in the play order.
    pub turn: u32,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
    pub cover_art: Option<String>,
    /// Seconds.
    pub duration: u32,
    pub starred: bool,
    /// A provider's song, not in the library yet.
    pub external: bool,
    /// The jam member who asked for it.
    pub by: Option<String>,
}

impl Entry {
    pub fn of(index: u32, turn: u32, song: &Song, by: Option<String>) -> Entry {
        Entry {
            index,
            turn,
            id: song.id.clone(),
            title: song.title.clone(),
            artist: song.artist.clone(),
            album: song.album.clone(),
            album_id: song.album_id.clone(),
            artist_id: song.artist_id.clone(),
            cover_art: song.cover_art.clone(),
            duration: song.duration,
            starred: song.starred,
            external: song.is_external,
            by,
        }
    }

    /// The song as a controller's player shows it.
    pub fn song(&self) -> Song {
        Song {
            id: self.id.clone(),
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            album_id: self.album_id.clone(),
            artist_id: self.artist_id.clone(),
            cover_art: self.cover_art.clone(),
            duration: self.duration,
            starred: self.starred,
            is_external: self.external,
            ..Default::default()
        }
        .dressed()
    }
}

/// What a device publishes whenever its playback or queue changes (not as time passes: a reader
/// carries the position on from `at_us`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceState {
    /// Bumped by every publish.
    pub seq: u64,
    pub playing: bool,
    /// Waiting for the song's bytes while it should play.
    pub buffering: bool,
    /// Where the song was at `at_us`, or as this was published.
    pub position_ms: i64,
    /// When the listener heard `position_ms`, on the device's clock ([`crate::clock::now_us`]); None from
    /// an older device, whose reader takes the position as of when the state arrived.
    pub at_us: Option<i64>,
    /// Song ms per real ms at `at_us`: the speed times a mix's tempo; None from an older device (1).
    pub rate: Option<f32>,
    /// The current list index.
    pub index: Option<u32>,
    /// The queue's revision; index-based commands name it.
    pub rev: u64,
    /// Songs in the queue.
    pub len: u32,
    /// The songs around the current one, in play order; the rest by [`Op::Page`].
    pub entries: Vec<Entry>,
    /// The device's media volume, 0 to 100, when it can be set.
    pub volume: Option<u8>,
    pub shuffle: bool,
    pub repeat: u8,
    /// The jam this device hosts.
    pub jam: Option<JamState>,
    /// The device this one last handed its playback to, until it plays again: its controllers follow it there.
    pub handed_to: Option<String>,
}

/// A jam member's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// The playing device; one per jam.
    Host,
    /// Its requests go straight in; it accepts and declines others'.
    Admin,
    /// Only requests songs.
    Guest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct JamMember {
    pub id: String,
    pub name: String,
    pub role: Role,
}

/// A song asked for and not yet accepted or declined. A provider song stays an id here: nothing streams,
/// prefetches or looks it up until it is accepted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub request: u64,
    pub from: String,
    pub from_name: String,
    pub song: Entry,
    /// A provider song: accepting it makes the server download it.
    pub provider: bool,
}

/// A hosted jam as its members see it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default, rename_all = "camelCase")]
pub struct JamState {
    pub members: Vec<JamMember>,
    pub pending: Vec<Pending>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_read_back() {
        let song = Song { id: "ext-deezer-song-9".into(), title: "Wish".into(), is_external: true, ..Default::default() };
        let out = Outgoing {
            room: Some("j1".into()),
            to: Some("phone".into()),
            state: None,
            body: Some(Body::Command { id: 3, op: Box::new(Op::Request { song: song.clone() }) }),
        };
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains(r#""t":"command""#) && json.contains(r#""op":"request""#), "{json}");
        assert_eq!(serde_json::from_str::<Outgoing>(&json).unwrap(), out);

        // A member published by a newer client still reads.
        let answer: Answer = serde_json::from_str(r#"{"seq":4,"rooms":[{"room":"u","members":[{"id":"a","kind":"tablet","name":"A","state":{"rev":"x"}}]}]}"#).unwrap();
        let a = &answer.rooms[0].members[0];
        assert_eq!((a.id.as_str(), a.kind, a.state.is_none()), ("a", DeviceKind::Phone, true));
        let answer: Answer = serde_json::from_str(r#"{"seq":5,"events":[{"seq":4,"room":"u","from":"a","body":{"t":"command","id":1,"op":{"op":"dance"}}},{"seq":5,"room":"u","from":"a","body":{"t":"command","id":2,"op":{"op":"pause"}}}]}"#).unwrap();
        assert_eq!(answer.events.iter().map(|e| e.seq).collect::<Vec<_>>(), [5], "an op this version does not know is passed over");

        // An older client's transfer and state read with the newer fields at their defaults.
        let old: Op = serde_json::from_str(r#"{"op":"replace","songs":[{"id":"s1"}],"index":0,"position_ms":5,"play":true}"#).unwrap();
        assert!(matches!(old, Op::Replace { order: None, shuffle: false, repeat: 0, .. }), "{old:?}");
        let st: DeviceState = serde_json::from_str(r#"{"playing":true,"index":1,"entries":[{"index":1,"id":"s2","title":"Two"}],"later":7}"#).unwrap();
        assert_eq!((st.seq, st.len, st.entries[0].turn, st.entries[0].starred), (0, 0, 0, false));

        let page = Body::Page { id: 2, rev: 9, from: 40, entries: vec![Entry::of(3, 40, &song, None)] };
        let json = serde_json::to_string(&page).unwrap();
        assert_eq!(serde_json::from_str::<Body>(&json).unwrap(), page);
        assert!(Entry::of(3, 40, &song, None).song().is_external, "the provider flag goes both ways");
    }
}
