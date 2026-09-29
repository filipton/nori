//! Audio offload: a song's packets handed as they are to an output that decodes them itself (a phone's
//! audio chip), instead of being decoded here. Only while nothing would change a sample
//! (`nori_player::policy::audio_policy`: no equalizer, speed, silence skipping or transitions, an output
//! the chip reaches) and the output says it takes the song's compression; the engine decides that, and
//! plays everything else on the CPU.
//!
//! What media3's sink does in offload, done the same way: songs of one format join without a gap on one
//! track, each told its encoder delay and padding before its first packet
//! ([`OffloadOutput::delay_padding`]) and the one before closed with [`OffloadOutput::end_of_stream`];
//! a song in another format waits for the track to play out and gets a track of its own; the volume is
//! the song's ReplayGain (and the controls' fades), never the samples. An Opus stream goes in Ogg pages,
//! as media3's `OggOpusAudioPacketizer` hands it to the chip.
//!
//! The track is asked to hold minutes of music ([`TRACK_US`]), and the next song is written once less than
//! half a minute is left ([`LOW_US`]). Each write gives the track as much as it takes. Many phones grant
//! far less than asked (32 KB on a Galaxy S21 FE, 64 KB on an S22: under two seconds of a song), and their
//! chip buffers seconds more of its own behind that. So the engine does not work out from the bytes when
//! to top up: once the platform has asked for more (`onDataRequest`, Android 10's
//! `StreamEventCallback`, as media3's sink sleeps for), a full track waits for its next word. The thread
//! then wakes about as often as the platform asks, and no more. Until the platform has asked once, the
//! top-up's time is worked out from what was written and what the track has played.
//!
//! Otherwise the thread wakes only for the ear reaching the next song (to say so, and to set its volume),
//! a fade's steps, the end of what was written, the output tearing the track down (which the platform
//! says at once), and the watchdog. [`Offload::lets_cpu_sleep`] says when nothing but the platform's word
//! is due: on Android the CPU's wake lock is let go then, as media3 lets its own go while it sleeps for
//! offload.
//!
//! The watchdog: a count of what the chip presented that stands still is no stall by itself. With the
//! screen off a phone may not move its timestamp (nor its play head) for seconds while the chip plays from
//! its own buffer, asking for nothing. The CPU takes over only when neither count has moved, and the
//! platform has asked for nothing, for longer than the music written past the count and a slack that
//! grows with how long the platform's counts were seen to stand still ([`STUCK_SLACK_MS`] at least). It
//! then plays on from where the chip's count last put the ear, never from where the clock would put it.
//! A count that stands while the platform keeps asking for more is one it does not keep: the other count
//! is followed, or, with neither moving, the CPU takes over where the clock puts the ear, the platform
//! having played what it asked for.

use std::collections::VecDeque;

use nori_player::pipeline::{Queue, Reading, Songs};
use nori_player::transitions::{in_album_run, WindowSong};

pub use crate::demux::{Coded, CodedSong, Coding};
use crate::demux::Demuxed;
use crate::library::{Library, Sources};

/// Whether an output decodes a compression itself, and whether it joins songs without a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Support {
    No,
    Plain,
    Gapless,
}

/// An output that decodes compressed songs itself: on Android, an AudioTrack opened for offload.
/// Called only from the engine's thread. The platform wakes that thread (it is the one that opened the
/// track) whenever the track wants more or was torn down.
pub trait OffloadOutput: Send {
    /// Whether the output decodes `coded` itself, where the music goes now.
    fn supports(&mut self, coded: Coded) -> Support;
    /// A track for `coded` holding about `bytes`, paused; replaces one that is open. The bytes it holds.
    fn open(&mut self, coded: Coded, bytes: usize) -> Result<usize, String>;
    /// Takes what it has room for of `data` without waiting: the bytes taken, or the error the track
    /// answered with. `data` stands for `frames` frames of music (what a simulated track plays).
    fn write(&mut self, data: &[u8], frames: u64) -> Result<usize, i32>;
    /// The encoder's delay and padding of the song whose packets come next.
    fn delay_padding(&mut self, delay: u32, padding: u32);
    /// The last packet written was the last of its song: the next one follows without a gap. False when
    /// the platform would not take it (Android's `setOffloadEndOfStream` throws unless the track plays).
    /// Android stops the track with it (`native_stop`, `PLAYSTATE_STOPPING`) until the platform has
    /// presented everything written, so a track told it is let go rather than flushed.
    fn end_of_stream(&mut self) -> bool;
    fn play(&mut self);
    fn pause(&mut self);
    /// Drops what was written and not played. Only paused.
    fn flush(&mut self);
    fn set_volume(&mut self, volume: f32);
    /// Frames of music presented since the track was opened or flushed; none when the platform could not
    /// be asked (a failed call is no reading, never nought). A platform may start counting again from
    /// nought at a song joined without a gap.
    fn head(&mut self) -> Option<u64>;
    /// Frames presented now by the platform's timestamp of the track (Android's `getTimestamp`: a frame
    /// and when it was presented), moved on by the time since it was taken while the track plays, as
    /// media3's `AudioTrackPositionTracker` does; counted as [`OffloadOutput::head`] is. None while the
    /// platform has given none since the track was opened or flushed. Preferred to the play head: on
    /// some phones an offloaded track's play head never moves (its `getRenderPosition` fails).
    fn timestamp(&mut self) -> Option<u64> {
        None
    }
    /// The platform asked for more since this was last asked (Android's `onDataRequest`): the track has
    /// room, whatever the play head says.
    fn data_requested(&mut self) -> bool {
        false
    }
    /// Whether the track has played everything written up to the last end of stream.
    fn presented(&mut self) -> bool;
    /// The track was torn down since this was last asked (the output went where the chip cannot follow):
    /// it plays nothing more.
    fn torn_down(&mut self) -> bool;
    fn close(&mut self);
    /// What the platform said when it was last asked whether it decodes `coded`, in its own words (the
    /// call and its answer), for a report of why a song plays on the CPU.
    fn said(&mut self, _coded: Coded) -> Option<String> {
        None
    }
    /// Something the offload path did that a perf report should say (why a song ended, a play head
    /// that made no sense), in words.
    fn note(&mut self, _what: &str) {}
    /// How many times faster than the music's own pace the track may present it: 1 for a real device,
    /// whose play head cannot run ahead of the clock; a simulated one may run faster.
    fn pace(&self) -> f64 {
        1.0
    }
}

/// Why the CPU plays a song although the settings let the output decode songs itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnCpu {
    /// It would not open as packets.
    Unread,
    /// Its compression is not one an output decodes itself here (FLAC, Vorbis, ALAC, HE-AAC, PCM).
    Compression(&'static str),
    /// The output does not decode it where the music goes now; what the platform said.
    Unsupported(Coded, Option<String>),
    /// It has an encoder delay or padding to cut, it joins a song of its album without a gap (the one
    /// before it or the one after it follows it on the album, as "keep albums gapless" has it), and the
    /// output joins songs of its compression only with a gap: offloaded, that join would not be gapless.
    NotGapless { coded: Coded, delay: u32, padding: u32, said: Option<String> },
    /// The offloaded track would not open.
    WouldNotOpen(Coded),
    /// The offloaded track was torn down, or refused a write.
    Failed,
    /// The platform's count of what the track played made no sense (it could not be read, or ran ahead of
    /// the clock), or it would not take a song's end of stream: in words.
    Head(String),
    /// ReplayGain turns it up: a volume cannot, so its samples are turned up, the limiter behind them.
    TurnedUp,
}

impl OnCpu {
    /// In words, for the perf report and the log.
    pub fn words(&self) -> String {
        let said = |s: &Option<String>| s.as_ref().map(|s| format!(" ({s})")).unwrap_or_default();
        match self {
            OnCpu::Unread => "the song would not open as packets".into(),
            OnCpu::Compression(c) => format!("{c} is not a compression the output decodes"),
            OnCpu::Unsupported(c, s) => format!("the output does not decode {} at {} Hz x{}{}", c.coding.name(), c.rate, c.channels, said(s)),
            OnCpu::NotGapless { coded, delay, padding, said: s } => format!(
                "{} with an encoder delay of {delay} and padding of {padding} joins a song of its album without a gap, which needs gapless offload, and the output does not do it{}",
                coded.coding.name(),
                said(s)
            ),
            OnCpu::WouldNotOpen(c) => format!("the offloaded track for {} would not open", c.coding.name()),
            OnCpu::Failed => "the offloaded track failed".into(),
            OnCpu::Head(why) => format!("the offloaded track could not be followed: {why}"),
            OnCpu::TurnedUp => "ReplayGain turns it up, which needs its samples and the limiter".into(),
        }
    }
}

/// How much music a track is asked to hold.
pub const TRACK_US: i64 = 240_000_000;
/// The track is topped up, and the next song written, when it holds less than this.
pub const LOW_US: i64 = 30_000_000;
/// The least and the most bytes a track is asked for.
const MIN_BYTES: usize = 512 * 1024;
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// A song's bitrate when its bytes do not say: the most a song usually has, so the track is not too small.
const GUESS_BPS: u32 = 320_000;
/// Bytes put together for one write.
const STAGE_BYTES: usize = 256 * 1024;
/// The last of what was written is taken as played this close to its end: a chip's count of what it
/// presented can stop a decoder's delay short of what was written.
const END_SLACK_US: i64 = 100_000;
/// How long to look again while the end of the music is due and the track has not said it got there.
const END_LOOK_MS: i64 = 100;
/// The track saying it played everything is taken as the end only this close to it by its own count.
const PRESENTED_NEAR_US: i64 = 3_000_000;
/// How far the play head may be ahead of the clock since the track began playing (a start's latency, the
/// thread's own lateness), ms.
const CLOCK_SLACK_MS: i64 = 500;
/// How far a reading of the count may step back without meaning anything, ms: Android's timestamp,
/// moved on by the clock from one anchor and then taken again from the next, reads a few frames (up to a
/// couple of milliseconds) back every few seconds, and a few frames back and forth as the track starts.
/// Such a step leaves the ear where it was. A count started again at a join or after an end of stream
/// drops a whole song, far more than this.
const JITTER_MS: i64 = 100;
/// How far the count may be ahead of the clock in the first moments after the track began playing,
/// ms: the platform's first timestamps of an offloaded track can read a start's worth (160 ms on a Galaxy
/// S22) ahead of what it presented, which it corrects itself a moment later. The ear is held to the clock
/// until then.
const START_SLACK_MS: i64 = 10;
/// How long after the track began playing the ear is held to the clock that closely, ms.
const START_MS: i64 = 1_000;
/// Notes of a count that made no sense (a step back away from a join, a reading ahead of the clock) come
/// a few at once at most ([`NOTES_AT_ONCE`]), and one more each this long, ms: a phone whose count
/// misbehaves steadily would cost a string and a log write each time, for the battery. The next note says
/// how many were left out.
const NOTE_GAP_MS: i64 = 10_000;
const NOTES_AT_ONCE: i64 = 4;
/// Readings of the play head in a row that made no sense (none, or ahead of the clock), or ends of
/// stream refused while playing, before the CPU takes over.
const STRIKES: u32 = 3;
/// How soon to look again at a play head that made no sense, or an end of stream still to say, ms.
const LOOK_AGAIN_MS: i64 = 300;
/// The least slack of the watchdog, ms: a playing track whose count has not moved, and whose platform
/// has asked for nothing, for longer than the music written past the count and this, is taken for
/// stalled. A count that stands this long while the platform asks for more is taken for dead. The tester's
/// S21 FE stood 2.8 s with the screen off while it played.
const STUCK_SLACK_MS: i64 = 10_000;
/// The slack grows to twice the longest a count was seen standing still before it moved on, up to this.
const STUCK_SLACK_MAX_MS: i64 = 60_000;
/// Requests for more, since the count last moved, that make a count standing still one the platform does
/// not keep: it plays, and says so.
const DEAD_ASKS: u32 = 2;
/// A full track whose platform says when it has room is looked at on the engine's own no sooner than
/// this, ms, in case its word never comes: once what was written could have played.
const BACKSTOP_MS: i64 = 500;
/// The CPU is kept awake from this long before the ear reaches a song placed after the one heard, or the
/// end of what was written, ms, at least: the song's volume and its event come on time. More on a
/// platform that asks for more seldom ([`Offload::awake_before_ms`]).
const AWAKE_BEFORE_MS: i64 = 3_000;
const AWAKE_BEFORE_MAX_MS: i64 = 60_000;

/// One song handed to the track: where it starts in the track's frames, and how long it is once all of it
/// is written.
#[derive(Debug, Clone)]
struct Placed {
    index: usize,
    id: String,
    start: u64,
    frames: Option<u64>,
    /// Where in the song its first frame is (a seek lands in a packet).
    from_ms: i64,
    level: f32,
    /// Counts songs placed, so a song placed again (repeat one) is another.
    seq: u64,
}

/// What comes after the last song written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// The end of the queue, or of the song the sleep timer stops at.
    End,
    /// Queue index `i` from its start, which this track cannot play: another format (another track), or a
    /// song for the CPU.
    Then(usize),
}

/// What a turn found that the engine has to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Fine,
    /// Queue index `i` from `ms` is for the CPU: its compression is not offloaded, or the track failed
    /// (`refused`: torn down, or a write refused with an error).
    ToPcm { index: usize, ms: i64, refused: bool },
}

/// The track's count of frames presented, made one that only grows: a platform that starts counting
/// again at a gapless join counts on from the song that begins there.
#[derive(Debug, Clone, Copy, Default)]
struct Head {
    base: u64,
    last: u64,
    /// A reading below the last one, not yet taken for a count started again (the next reading says).
    lower: Option<u64>,
}

/// What a reading of the play head was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Fine,
    /// Lower than the last by no more than a moment's jitter (a timestamp moved on by the clock from one
    /// anchor, then taken again from the next): the count holds where it was. How far back it read.
    Jitter(u64),
    /// Lower than the last, where the ear may have reached the next song: the count started again there.
    Joined,
    /// Lower than the last, and not at a join: kept aside until the next reading says what it was.
    Dip,
    /// Lower than the last twice, both well towards nought, the second no lower than the first, where a
    /// count started again is plausible (`restart`): the platform counts again from nought (after an end
    /// of stream, or a standby while paused), and the count goes on from where the ear was.
    Restarted,
}

impl Head {
    /// `raw` read now; `join`, where the song after the one the count is in starts, only when the clock
    /// says the ear may have got there; `restart`, whether the platform may have started counting again
    /// from nought away from a join; `jitter`, the frames a reading may step back by without meaning
    /// anything; `most`, the furthest the ear can be by the clock. A reading that would put the ear past
    /// `most` changes nothing, and is Err with where it would have put it.
    fn read(&mut self, raw: u64, join: Option<u64>, restart: bool, jitter: u64, most: u64) -> Result<(u64, Seen), u64> {
        let (base, last, seen) = if raw >= self.last {
            (self.base, raw, Seen::Fine)
        } else if self.last - raw <= jitter {
            // Never a join nor a count started again: those drop far more than a moment.
            self.lower = None;
            return Ok((self.base + self.last, Seen::Jitter(self.last - raw)));
        } else if let Some(start) = join {
            (start, raw, Seen::Joined)
        } else if restart && raw < self.last / 2 && self.lower.is_some_and(|l| raw >= l) {
            (self.base + self.last, raw, Seen::Restarted)
        } else {
            // Only a drop well towards nought may be the start of a count started again.
            self.lower = (raw < self.last / 2).then_some(raw);
            return Ok((self.base + self.last, Seen::Dip));
        };
        if base + last > most {
            return Err(base + last);
        }
        (self.base, self.last, self.lower) = (base, last, None);
        Ok((base + last, seen))
    }
}

/// A volume fade at the track: from, to, when it began (ms), how long.
#[derive(Debug, Clone, Copy)]
struct Fade {
    from: f32,
    to: f32,
    start_ms: i64,
    ms: i32,
}

/// The song being written.
struct Writing {
    r: Demuxed,
    /// Frames of music written of it.
    frames: u64,
    ogg: Option<Ogg>,
}

/// The engine's offload path: the track, the songs handed to it, and the song being written.
pub(crate) struct Offload {
    out: Box<dyn OffloadOutput>,
    /// The format the track is open for, whether it joins songs without a gap, and the bytes it holds.
    open: Option<(Coded, bool, usize)>,
    supported: Vec<(Coded, Support)>,
    placed: VecDeque<Placed>,
    writing: Option<Writing>,
    /// The song to start with: opened, waiting to be ready, then placed or handed to the CPU.
    starting: Option<(usize, i64, Result<Demuxed, String>)>,
    /// The song after the one written last, opened once that one was read to its end.
    next: Option<(usize, Result<Demuxed, String>)>,
    tail: Option<Tail>,
    stage: Vec<u8>,
    staged: usize,
    stage_frames: u64,
    written_bytes: u64,
    written_frames: u64,
    head: Head,
    /// The platform's timestamp, counted the same way.
    stamp: Head,
    /// The last reading came from the timestamp (not the play head).
    by_stamp: bool,
    /// Counts found not to move while the track played: not read any more on this track.
    stamp_dead: bool,
    head_dead: bool,
    /// When the ear last moved on, or the track began playing: none until the next turn says.
    moved_ms: Option<i64>,
    /// Times the platform asked for more since the ear last moved on, and when it last did.
    asks: u32,
    asked_ms: Option<i64>,
    /// The longest the count stood still before it moved on, ms: how the platform keeps its count, which
    /// the watchdog's slack grows with.
    quiet_ms: i64,
    /// The platform has asked for more on this track: it says when a full track has room.
    called_back: bool,
    /// When the platform last asked while the track played, and the longest it went between two asks.
    last_ask_ms: Option<i64>,
    ask_gap_ms: i64,
    /// The bytes asked for when the track was opened, until what the platform granted is noted.
    granted: Option<usize>,
    /// The frames presented as last read.
    heard_at: u64,
    playing: bool,
    /// The song the music stops at the end of (the sleep timer's "end of this song").
    pub stop_after: Option<usize>,
    gain: f32,
    level: f32,
    fade: Option<Fade>,
    seq: u64,
    /// The last turn found the next packet's bytes still on their way.
    waiting: bool,
    /// A write since the last end of stream, so the next song's join has one to close.
    pending_eos: bool,
    /// The last write was refused in part: the track is full.
    full: bool,
    /// Why the last song this path gave up went to the CPU.
    pub(crate) on_cpu: Option<OnCpu>,
    /// The song placed last has an encoder delay or padding the output cannot cut (it does not do
    /// gapless offload), so it is heard with a few milliseconds of near silence at its ends: in words,
    /// for the report.
    pub(crate) gapped: Option<String>,
    /// The time of the turn under way, ms.
    now_ms: i64,
    /// When the play head was last read and made sense (or the track began playing), and the frames heard
    /// then: the head cannot be further on than the clock has run since. None before the track plays.
    clock: Option<(i64, u64)>,
    /// How much less the count moved than the clock ran between the last two readings that moved it, ms:
    /// the reading that set `clock` may be one from that long before it was read (a platform whose count
    /// stood still for seconds with the screen off while the chip played, read long after it stopped), and
    /// the bound from it is loosened by as much. Nought when the count kept up.
    clock_lag_ms: i64,
    /// When the track last began playing, and the frames heard then: the furthest bound of all, which a
    /// count taken over from one found standing still is held to.
    play_clock: Option<(i64, u64)>,
    /// The play head's raw reading, as last read.
    raw: Option<u64>,
    /// Readings of the play head in a row that made no sense, or ends of stream refused, and the last
    /// one's words.
    strikes: u32,
    strike_why: String,
    /// An end of stream is due and was not said: the track was not playing, or refused it.
    eos_due: bool,
    /// Ends of stream the platform refused in a row while its track played.
    eos_refusals: u32,
    /// The track was told to play since it was opened or last paused: Android takes an end of stream
    /// only then.
    started: bool,
    /// The frames written when the platform last took an end of stream on this track: its word that it
    /// presented everything is about that one only while nothing was written since.
    eos_at: Option<u64>,
    /// How what was written ended is noted, once.
    end_noted: bool,
    /// The track was paused and played again since the count last moved on: a platform may have gone
    /// to standby meanwhile, and count again from nought.
    resumed: bool,
    /// Readings that stepped back a moment ([`Seen::Jitter`]) since the track was emptied, and the most
    /// frames one stepped back by: said once, with how what was written ended.
    jitter: (u32, u64),
    /// Notes of a count that made no sense may be made again once the time is past this, ms, one each
    /// [`NOTE_GAP_MS`] ([`NOTES_AT_ONCE`] at once at most), and how many were left out since the last.
    notes_from_ms: i64,
    quieted: u32,
}

impl Offload {
    pub(crate) fn new(out: Box<dyn OffloadOutput>) -> Offload {
        Offload {
            out,
            open: None,
            supported: Vec::new(),
            placed: VecDeque::new(),
            writing: None,
            starting: None,
            next: None,
            tail: None,
            stage: Vec::with_capacity(STAGE_BYTES + 64 * 1024),
            staged: 0,
            stage_frames: 0,
            written_bytes: 0,
            written_frames: 0,
            head: Head::default(),
            stamp: Head::default(),
            by_stamp: false,
            stamp_dead: false,
            head_dead: false,
            moved_ms: None,
            asks: 0,
            asked_ms: None,
            quiet_ms: 0,
            called_back: false,
            last_ask_ms: None,
            ask_gap_ms: 0,
            granted: None,
            heard_at: 0,
            playing: false,
            stop_after: None,
            gain: 1.0,
            level: 1.0,
            fade: None,
            seq: 0,
            waiting: false,
            pending_eos: false,
            full: false,
            on_cpu: None,
            gapped: None,
            now_ms: 0,
            clock: None,
            play_clock: None,
            clock_lag_ms: 0,
            raw: None,
            strikes: 0,
            strike_why: String::new(),
            eos_due: false,
            eos_refusals: 0,
            started: false,
            eos_at: None,
            end_noted: false,
            resumed: false,
            jitter: (0, 0),
            notes_from_ms: i64::MIN / 2,
            quieted: 0,
        }
    }

    /// The offload path holds a song: it is the one playing (or paused).
    pub(crate) fn active(&self) -> bool {
        self.starting.is_some() || !self.placed.is_empty()
    }

    pub(crate) fn playing(&self) -> bool {
        self.playing && self.active()
    }

    /// A track plays songs placed on it, and the watchdog here judges whether it stalled ([`Offload::stuck`]).
    pub(crate) fn watching(&self) -> bool {
        self.playing && self.started && self.open.is_some() && !self.placed.is_empty()
    }

    /// Whether the track is open, and for what: for the perf report and the screen.
    pub(crate) fn track(&self) -> Option<Coded> {
        self.open.map(|o| o.0)
    }

    /// Whether the output decodes `coded`, asked once per format until the output moves.
    fn support(&mut self, coded: Coded) -> Support {
        if let Some((_, s)) = self.supported.iter().find(|(c, _)| *c == coded) {
            return *s;
        }
        let s = self.out.supports(coded);
        self.supported.push((coded, s));
        s
    }

    /// The music goes to another device: what the output decodes there is asked again.
    pub(crate) fn output_moved(&mut self) {
        self.supported.clear();
    }

    /// Why this output would not play song `r` (none: it would): it would not open, its compression is
    /// not one an output decodes, the output does not decode it, or it has a gap to cut at a join that
    /// must be gapless (`album`: [`Offload::in_album`]) and the output joins songs only with one. media3
    /// requires gapless support for every song with a delay or padding; here only an album's songs in
    /// order need it, and between unrelated songs the encoder's few milliseconds (576 samples of delay
    /// and about a thousand of padding in an MP3) are near silence where a gap is expected anyway.
    /// A song ReplayGain turns up (`level` over 1) needs its samples: it is not the output's either.
    pub(crate) fn refuses(&mut self, r: &Demuxed, album: bool, level: f32) -> Option<OnCpu> {
        if r.error().is_some() {
            return Some(OnCpu::Unread);
        }
        if !nori_player::gain::offload_allows(level) {
            return Some(OnCpu::TurnedUp);
        }
        let Some(s) = r.coded() else { return Some(OnCpu::Compression(r.compression().unwrap_or("an unknown compression"))) };
        let (coded, delay, padding) = (s.coded, s.delay, s.padding);
        match self.support(coded) {
            Support::No => Some(OnCpu::Unsupported(coded, self.out.said(coded))),
            Support::Plain if (delay != 0 || padding != 0) && album => Some(OnCpu::NotGapless { coded, delay, padding, said: self.out.said(coded) }),
            Support::Plain | Support::Gapless => None,
        }
    }

    /// Whether song `i` joins a song of its album without a gap in the order they play: the song before it
    /// or the one after it follows it on the same album, the rule that keeps albums gapless
    /// (`nori_player::transitions::in_album_run`; never while shuffling).
    pub(crate) fn in_album<L: Library, Q: Queue>(&self, i: usize, tracks: &Sources<L>, queue: &Q) -> bool {
        let (ids, runs, before, after, shuffling) = queue.read(|q| {
            let repeat = q.repeat();
            (q.ids().to_vec(), q.album_runs().to_vec(), q.previous_of(i, repeat), q.next_of(i, repeat), q.shuffling())
        });
        let after = after.filter(|_| self.stop_after != Some(i));
        let about = |k: usize| ids.get(k).map(|id| WindowSong { album_run: runs.get(k).copied().unwrap_or(0), ..tracks.about(id) });
        let Some(song) = about(i) else { return false };
        in_album_run(before.and_then(about).as_ref(), &song, after.and_then(about).as_ref(), shuffling)
    }

    /// Queue index `i` from `from_ms`: the track is emptied, and the song opened as packets. Whether it
    /// plays here is known once it is open ([`Offload::turn`] says so).
    pub(crate) fn start<L: Library, Q: Queue>(&mut self, i: usize, from_ms: i64, tracks: &mut Sources<L>, queue: &Q) -> usize {
        self.empty();
        self.stop_after = None;
        // A song that arriving on skips (an explicit one) gives way to the first after it that does not.
        let i = if queue.skips(i) { self.next_of(i, queue).unwrap_or(i) } else { i };
        let id = queue.read(|q| q.ids()[i].clone());
        let opened = tracks.open_packets(&id, from_ms, false);
        self.starting = Some((i, from_ms, opened));
        i
    }

    /// Everything handed to the track goes, and what was being written; the track stays open.
    fn empty(&mut self) {
        if self.eos_at.is_some() && !self.out.presented() && self.open.take().is_some() {
            // Told an end of stream, Android's track was stopped until it presents it (then it starts
            // itself again): paused and flushed before that, it starts again as stopping, refuses every
            // write that does not wait (`blockUntilOffloadDrain`), and may say later that it presented
            // the flushed song's end. media3 never flushes an offloaded track; a new one is opened for
            // what comes.
            self.out.close();
            self.started = false;
        } else if self.open.is_some() {
            self.out.pause();
            self.started = false;
            self.out.flush();
        }
        self.placed.clear();
        self.writing = None;
        self.starting = None;
        self.next = None;
        self.tail = None;
        self.stage.clear();
        self.staged = 0;
        self.stage_frames = 0;
        self.written_bytes = 0;
        self.written_frames = 0;
        self.head = Head::default();
        self.stamp = Head::default();
        self.by_stamp = false;
        self.stamp_dead = false;
        self.head_dead = false;
        self.moved_ms = None;
        self.asks = 0;
        self.asked_ms = None;
        self.last_ask_ms = None;
        self.heard_at = 0;
        self.waiting = false;
        self.pending_eos = false;
        self.full = false;
        self.clock = None;
        self.clock_lag_ms = 0;
        self.play_clock = None;
        self.raw = None;
        self.strikes = 0;
        self.eos_due = false;
        self.eos_refusals = 0;
        self.eos_at = None;
        self.end_noted = false;
        self.resumed = false;
        self.jitter = (0, 0);
    }

    /// The CPU takes the music over at `now_ms` (the settings or the output no longer let the chip play
    /// it): the count is read once more, so the CPU starts where the chip really is and not where the
    /// last turn (up to a top-up's time ago) saw it, and the track is let go. Where the ear was, as
    /// (queue index, ms), with a note of it for the perf report.
    pub(crate) fn leave(&mut self, now_ms: i64) -> Option<(usize, i64)> {
        self.now_ms = now_ms;
        if self.playing && self.started && !self.placed.is_empty() {
            self.read_head();
        }
        self.note_left();
        self.release()
    }

    /// Where the CPU takes over, for the perf report: the place in the song, the chip's own count and
    /// what was written to it, all in ms.
    fn note_left(&mut self) {
        let Some((_, ms, _)) = self.heard() else { return };
        let rate = self.rate() as u64;
        let f = |frames: u64| frames * 1000 / rate;
        let count = if self.by_stamp { self.stamp } else { self.head };
        let said = match self.raw {
            Some(_) => format!("{} ms by its {}", f(count.base + count.last), if self.by_stamp { "timestamp" } else { "play head" }),
            None => "nothing".into(),
        };
        let what = format!("offload: left at {ms} ms (chip said {said}, heard {} ms, written {} ms)", f(self.heard_at), f(self.written_frames));
        self.note(what);
    }

    /// Lets the track go (a long pause, or the CPU takes over): where the ear was, as (queue index, ms).
    pub(crate) fn release(&mut self) -> Option<(usize, i64)> {
        let at = self.heard().map(|(i, ms, _)| (i, ms)).or(self.starting.as_ref().map(|s| (s.0, s.1)));
        self.empty();
        if self.open.take().is_some() {
            self.out.close();
            self.started = false;
        }
        self.playing = false;
        self.stop_after = None;
        // A new track starts at full volume, as a new CPU device does: no fade carries over.
        self.fade = None;
        self.gain = 1.0;
        at
    }

    pub(crate) fn play(&mut self) {
        self.playing = true;
        self.moved_ms = None;
        self.last_ask_ms = None;
        self.play_clock = None;
        if self.open.is_some() && !self.placed.is_empty() {
            self.resumed = true;
            self.out.play();
            self.started = true;
            if self.eos_due {
                self.end_stream();
            }
        }
    }

    pub(crate) fn pause(&mut self) {
        self.playing = false;
        self.moved_ms = None;
        self.last_ask_ms = None;
        if let Some(f) = self.fade.take() {
            // The fade the pause waited for ends at its target, not a step short of it.
            self.gain = f.to;
            self.volume();
        }
        if self.open.is_some() {
            self.out.pause();
            self.started = false;
        }
    }

    /// A fade of the track's volume from `from` (or where it is) to `to` over `ms`, from `now_ms`.
    pub(crate) fn ramp(&mut self, from: Option<f32>, to: f32, ms: i64, now_ms: i64) {
        self.fade = Some(Fade { from: from.unwrap_or(self.gain), to, start_ms: now_ms, ms: ms.clamp(0, i32::MAX as i64) as i32 });
        self.follow_fade(now_ms);
    }

    /// The ReplayGain volume of the song heard, set now (the settings changed).
    pub(crate) fn set_level(&mut self, level: f32) {
        self.level = level;
        if let Some(p) = self.placed.front_mut() {
            p.level = level;
        }
        self.volume();
    }

    fn volume(&mut self) {
        if self.open.is_some() {
            self.out.set_volume(self.gain * self.level);
        }
    }

    fn follow_fade(&mut self, now_ms: i64) {
        let Some(f) = self.fade else { return };
        let (v, done) = nori_player::transport::fade_step(f.from, f.to, f.start_ms, now_ms, f.ms);
        self.gain = v;
        if done {
            self.fade = None;
        }
        self.volume();
    }

    fn rate(&self) -> u32 {
        self.open.map_or(48_000, |o| o.0.rate).max(1)
    }

    /// The furthest the ear can be by the clock: where it was at the last reading that made sense (or
    /// when the track began playing), moved on by the time since at the output's pace, and a little.
    fn most(&self) -> u64 {
        let rate = self.rate() as f64 * self.out.pace();
        let run = |ms: i64| (ms.max(0) as f64 * rate / 1000.0) as u64;
        match self.clock {
            Some((t, at)) => at + run(self.now_ms - t + CLOCK_SLACK_MS + self.clock_lag_ms),
            None => self.heard_at + run(CLOCK_SLACK_MS),
        }
    }

    /// Frames presented now, as a count that only grows and never runs ahead of the clock: by the
    /// platform's timestamp where it has one, by the play head otherwise. A reading that makes no sense
    /// leaves the ear where it was, and is a strike.
    fn read_head(&mut self) -> u64 {
        if self.open.is_none() {
            return self.heard_at;
        }
        let stamp = if self.stamp_dead { None } else { self.out.timestamp() };
        let (raw, by_stamp) = match stamp {
            Some(s) => (s, true),
            // A play head that never moved is not asked again: the watchdog decides.
            None if self.head_dead => return self.heard_at,
            None => match self.out.head() {
                Some(h) => (h, false),
                None => {
                    self.strike("the platform's play head could not be read".into());
                    return self.heard_at;
                }
            },
        };
        self.by_stamp = by_stamp;
        self.raw = Some(raw);
        let most = self.most();
        let rate = self.rate() as i64;
        let jitter = (JITTER_MS * rate / 1000) as u64;
        let mut count = if by_stamp { self.stamp } else { self.head };
        let counted = count.base + count.last;
        // A count started again at a join, only where the clock says the ear may be by now.
        let join = self.placed.iter().map(|p| p.start).find(|&s| s > counted).filter(|&s| s <= most);
        // Away from a join, a count starts again from nought only after an end of stream (Android stops
        // the track until it presented it) or a pause (a standby meanwhile).
        let restart = self.eos_at.is_some() || self.resumed;
        let was = self.heard_at;
        let last = count.last;
        let read = count.read(raw, join, restart, jitter, most);
        if by_stamp {
            self.stamp = count;
        } else {
            self.head = count;
        }
        let what = if by_stamp { "timestamp" } else { "play head" };
        match read {
            Ok((at, seen)) => {
                match seen {
                    Seen::Jitter(back) => self.jitter = (self.jitter.0.saturating_add(1), self.jitter.1.max(back)),
                    Seen::Dip => self.note_now_and_then(|| format!("the {what} read {raw} after {last}, not at a join: looked at again")),
                    Seen::Restarted => self.note_now_and_then(|| format!("the {what} counts again from nought ({raw}): the count goes on from {counted} frames")),
                    Seen::Fine | Seen::Joined => {}
                }
                // In the first moments after the track began playing, no further than the clock says:
                // the platform's first timestamps may read ahead of what it presented.
                let start = self.play_clock.filter(|&(t, _)| self.now_ms - t < START_MS);
                let pace = self.out.pace();
                let at = start.map_or(at, |(t, from)| at.min(from + ((self.now_ms - t + START_SLACK_MS).max(0) as f64 * rate as f64 * pace / 1000.0) as u64));
                self.heard_at = at.min(self.written_frames).max(self.heard_at);
                if seen != Seen::Dip {
                    self.strikes = 0;
                    // Only a count that moved says where the ear is by the clock: one that stands still
                    // may be one the platform does not keep (a play head stuck at nought).
                    if self.heard_at > was || self.clock.is_none() {
                        self.clock_lag_ms = self.clock.map_or(0, |(t, at)| ((self.now_ms - t) - (self.heard_at.saturating_sub(at) as i64 * 1000 / rate)).max(0));
                        self.clock = Some((self.now_ms, self.heard_at));
                    }
                }
                if self.heard_at > was {
                    if let Some(m) = self.moved_ms {
                        self.quiet_ms = self.quiet_ms.max(self.now_ms - m);
                    }
                    self.moved_ms = Some(self.now_ms);
                    self.asks = 0;
                    self.asked_ms = None;
                    if seen == Seen::Fine {
                        self.resumed = false;
                    }
                }
            }
            Err(at) => {
                let ms = |f: u64| f as i64 * 1000 / rate;
                self.strike(format!("the {what} read {raw} frames, {} ms in, ahead of the clock's {} ms", ms(at), ms(most)));
            }
        }
        self.heard_at
    }

    /// Music the track can hold, µs, as its bytes and the songs' bitrate so far make it.
    fn holds_us(&self) -> i64 {
        let Some((_, _, bytes)) = self.open else { return TRACK_US };
        if self.written_bytes == 0 || self.written_frames == 0 {
            return (bytes as i128 * 8_000_000 / GUESS_BPS as i128) as i64;
        }
        (bytes as i128 * self.written_frames as i128 / self.written_bytes as i128 * 1_000_000 / self.rate() as i128) as i64
    }

    /// The watchdog's slack, ms: [`STUCK_SLACK_MS`], or twice the longest the platform's count was seen
    /// standing still before it moved on, up to [`STUCK_SLACK_MAX_MS`]. Scaled to how the platform keeps
    /// its count, not to what the track holds.
    fn slack_ms(&self) -> i64 {
        self.quiet_ms.saturating_mul(2).clamp(STUCK_SLACK_MS, STUCK_SLACK_MAX_MS)
    }

    /// When the watchdog is due to look, ms (engine time): the count standing still while the platform
    /// asks for more ([`DEAD_ASKS`] times) for the slack; or the count standing and the platform silent
    /// for as long as the music written past the count, and the slack.
    fn watch_at(&self, since: i64) -> i64 {
        let slack = self.slack_ms();
        let heard_from = self.asked_ms.map_or(since, |a| a.max(since));
        let stall = heard_from + self.in_track_us() / 1000 + slack;
        if self.asks >= DEAD_ASKS { stall.min(since + slack) } else { stall }
    }

    /// The watchdog: a playing track with music still to present whose count has not moved, and whose
    /// platform has said nothing, for longer than the music written past the count and the slack, has
    /// stalled; one whose count stands while the platform keeps asking for more has a count the platform
    /// does not keep. The play head is tried when the timestamp stood still; when neither moves, the words
    /// of why the CPU takes over, and whether the ear was put where the clock says (a platform that asked
    /// for more played what it asked for) or left where the chip's count last put it (a stall: never
    /// further than the chip got). Never a playing engine over a track starved in silence.
    fn stuck(&mut self) -> Option<(String, bool)> {
        if !self.playing || !self.started || self.placed.is_empty() || self.open.is_none() {
            return None;
        }
        let now = self.now_ms;
        let since = *self.moved_ms.get_or_insert(now);
        if self.in_track_us() <= END_SLACK_US {
            // Everything written was heard: nothing to present, nothing to stand still for.
            self.moved_ms = Some(now);
            return None;
        }
        if now < self.watch_at(since) {
            return None;
        }
        let still = now - since;
        let dead = self.asks >= DEAD_ASKS;
        let asked = match self.asks {
            0 => ", and the platform asked for nothing".to_string(),
            n => format!(", though the platform asked for more {n} times"),
        };
        let what = if self.by_stamp { "timestamp" } else { "play head" };
        let raw = self.raw.map_or("nothing".into(), |r| r.to_string());
        let why = format!("the {what} stood at {raw} for {still} ms while the track played, {} ms written past it{asked} (slack {} ms)", self.in_track_us() / 1000, self.slack_ms());
        if self.by_stamp && !self.head_dead {
            self.stamp_dead = true;
            self.note(format!("{why}: the play head is followed instead"));
            // Where the timestamp put the ear by the clock was its word: the play head is held only to
            // what the clock allows since the track began playing.
            self.clock = self.play_clock.or(self.clock);
            self.clock_lag_ms = 0;
            let was = self.heard_at;
            self.read_head();
            if self.heard_at > was {
                return None;
            }
        }
        self.stamp_dead = true;
        self.head_dead = true;
        if dead {
            // The platform played what it asked for: where the clock says the ear is by now, no further
            // than what was written.
            let by_clock = self.heard_at + (still.max(0) as u128 * self.rate() as u128 / 1000) as u64;
            self.heard_at = by_clock.min(self.written_frames);
        }
        Some((why, dead))
    }

    /// A reading that made no sense, or an end of stream refused.
    fn strike(&mut self, why: String) {
        self.strikes += 1;
        let n = self.strikes;
        self.note_now_and_then(|| format!("{why} ({n} of {STRIKES})"));
        self.strike_why = why;
    }

    fn note(&mut self, what: String) {
        self.out.note(&what);
    }

    /// A note of a count that made no sense, a few at once and one each [`NOTE_GAP_MS`] at most: its
    /// words are put together only when it is made, and say how many were left out since the last.
    fn note_now_and_then(&mut self, what: impl FnOnce() -> String) {
        let from = self.notes_from_ms.max(self.now_ms - NOTES_AT_ONCE * NOTE_GAP_MS);
        if from + NOTE_GAP_MS > self.now_ms {
            self.quieted = self.quieted.saturating_add(1);
            return;
        }
        self.notes_from_ms = from + NOTE_GAP_MS;
        let what = match std::mem::take(&mut self.quieted) {
            0 => what(),
            n => format!("{} (and {n} like it before, not noted)", what()),
        };
        self.note(what);
    }

    /// The song the ear is on, where in it (ms), and which placing of it: the songs before it are gone.
    pub(crate) fn heard(&mut self) -> Option<(usize, i64, u64)> {
        let at = self.heard_at;
        while self.placed.len() > 1 && self.placed[1].start <= at {
            self.placed.pop_front();
            let level = self.placed[0].level;
            self.level = level;
            self.volume();
        }
        let p = self.placed.front()?;
        let rate = self.rate() as i64;
        Some((p.index, p.from_ms + (at.saturating_sub(p.start) as i64) * 1000 / rate, p.seq))
    }

    /// The song the ear is on, without reading anything.
    pub(crate) fn current(&self) -> Option<usize> {
        self.placed.front().map(|p| p.index).or(self.starting.as_ref().map(|s| s.0))
    }

    /// Music of what was written still to be heard, µs.
    pub(crate) fn in_track_us(&self) -> i64 {
        (self.written_frames.saturating_sub(self.heard_at) as i128 * 1_000_000 / self.rate() as i128) as i64
    }

    /// Everything written has been heard and nothing more comes: what follows, if anything was set.
    pub(crate) fn done(&mut self) -> Option<Tail> {
        let tail = self.tail?;
        if self.writing.is_some() || self.staged < self.stage.len() {
            return None;
        }
        let end = self.written_frames;
        let us = |us: i64| (us * self.rate() as i64 / 1_000_000) as u64;
        let by_head = self.heard_at + us(END_SLACK_US) >= end;
        // The platform's word that it played to the end of stream counts only near the end (it says so
        // at every join as well, for the song before it), and only for the end of stream said last with
        // nothing written since.
        let by_word = !by_head && self.eos_at == Some(end) && self.heard_at + us(PRESENTED_NEAR_US) >= end && self.out.presented();
        if !by_head && !by_word {
            return None;
        }
        if std::mem::replace(&mut self.end_noted, true) {
            return Some(tail);
        }
        let ms = |f: u64| f as i64 * 1000 / self.rate() as i64;
        let (id, start) = self.placed.front().map_or((String::new(), 0), |p| (p.id.clone(), p.start));
        let raw = self.raw.map_or("nothing".into(), |r| r.to_string());
        let count = if self.by_stamp { "timestamp" } else { "play head" };
        let (steps, most) = self.jitter;
        let jitter = if steps > 0 { format!(", its {count} a moment back {steps} times (by {most} frames at most), held where it was") } else { String::new() };
        let what = format!(
            "{id} ended {}: the {count} read {raw}, {} ms heard of {} ms written for it, its end of stream {}{jitter}",
            if by_head { "by the play head" } else { "by the platform's word that it presented everything" },
            ms(self.heard_at.saturating_sub(start)),
            ms(end.saturating_sub(start)),
            if self.eos_at == Some(end) { "said" } else { "not said" },
        );
        self.note(what);
        Some(tail)
    }

    /// The next song in play order after `i`, the one arriving on would not skip; none after the song the
    /// music stops at.
    fn next_of<Q: Queue>(&self, i: usize, queue: &Q) -> Option<usize> {
        if self.stop_after == Some(i) {
            return None;
        }
        let first = queue.read(|q| q.next_of(i, q.repeat()))?;
        let mut at = first;
        for _ in 0..queue.read(|q| q.len()) {
            if !queue.skips(at) {
                return Some(at);
            }
            match queue.read(|q| q.next_of(at, q.repeat())) {
                Some(n) if n != first => at = n,
                _ => return Some(at),
            }
        }
        Some(at)
    }

    /// One turn: the song starting is placed once it is open, the track topped up when it is due, the
    /// fade moved on. `gain` says each song's ReplayGain volume.
    pub(crate) fn turn<L: Library, Q: Queue>(&mut self, now_ms: i64, tracks: &mut Sources<L>, queue: &Q, gain: &mut dyn FnMut(usize, &str) -> f32) -> Step {
        self.now_ms = now_ms;
        self.follow_fade(now_ms);
        if self.open.is_some() && self.out.torn_down() {
            return self.fallback(true);
        }
        if let Some(step) = self.begin(tracks, queue, gain) {
            return step;
        }
        // The platform's word that it wants more: the track is topped up whatever the count says.
        let asked = self.open.is_some() && self.out.data_requested();
        if self.play_clock.is_none() && self.playing && self.started {
            self.play_clock = Some((now_ms, self.heard_at));
        }
        self.read_head();
        if asked {
            self.asked_for_more(now_ms);
        }
        if self.eos_due && self.playing {
            self.end_stream();
        }
        if self.strikes >= STRIKES || self.eos_refusals >= STRIKES {
            let why = std::mem::take(&mut self.strike_why);
            self.note(format!("offload given up, the CPU plays on: {why}"));
            let step = self.fallback(true);
            self.on_cpu = Some(OnCpu::Head(why));
            return step;
        }
        if let Some((why, by_clock)) = self.stuck() {
            let ms = self.heard().map_or(0, |h| h.1);
            let place = if by_clock { "where the clock puts the ear, the platform having played what it asked for" } else { "where the chip's count last put the ear" };
            self.note(format!("offload given up, the CPU plays on from {ms} ms, {place}: {why}"));
            let step = self.fallback(true);
            self.on_cpu = Some(OnCpu::Head(why));
            return step;
        }
        // A turn after the bytes it waited for came (the loader woke the thread) writes them, and so
        // stops waiting.
        if self.placed.is_empty() || (!asked && !self.waiting && !self.top_up_due()) {
            return Step::Fine;
        }
        match self.fill(asked, tracks, queue, gain) {
            Ok(()) => Step::Fine,
            Err(_) => self.fallback(true),
        }
    }

    /// The platform asked for more (at `now_ms`): its word that it plays, and that it says when the track
    /// has room.
    fn asked_for_more(&mut self, now_ms: i64) {
        self.called_back = true;
        self.asks = self.asks.saturating_add(1);
        self.asked_ms = Some(now_ms);
        if self.playing && self.started {
            if let Some(last) = self.last_ask_ms.replace(now_ms) {
                self.ask_gap_ms = self.ask_gap_ms.max(now_ms - last);
            }
        }
    }

    /// Something is left to write: the song being written, what is staged of it, or the next one.
    fn more(&self) -> bool {
        self.writing.is_some() || self.next.is_some() || self.staged < self.stage.len()
    }

    /// Whether the track is to be written to now without the platform asking: a full one only while the
    /// platform has not shown it says when there is room (then at [`Offload::low_us`], as the bytes make
    /// it), one that took everything it was given once the next song's mark ([`LOW_US`]) is reached.
    fn top_up_due(&self) -> bool {
        if !self.more() {
            return false;
        }
        if self.full {
            !self.called_back && self.in_track_us() < self.low_us()
        } else {
            self.in_track_us() < LOW_US
        }
    }

    /// How long before a boundary (the ear reaching a song placed after the one heard, or the end of what
    /// was written) the CPU is kept awake, ms: the longest the platform went between two asks and half
    /// again and a second, [`AWAKE_BEFORE_MS`] to [`AWAKE_BEFORE_MAX_MS`]. A thread asleep on a phone
    /// asleep wakes only when the platform wakes it, so this is how late it could otherwise be.
    fn awake_before_ms(&self) -> i64 {
        (self.ask_gap_ms * 3 / 2 + 1_000).clamp(AWAKE_BEFORE_MS, AWAKE_BEFORE_MAX_MS)
    }

    /// Whether the offload path needs nothing of the CPU until the platform wakes the thread: the track
    /// plays, fed, and says when it wants more; no song starts, no bytes are awaited, no fade runs, no
    /// reading of the count is to be looked at again, and no boundary comes within
    /// [`Offload::awake_before_ms`]. The CPU may sleep then (Android lets the wake lock go).
    pub(crate) fn lets_cpu_sleep(&self) -> bool {
        if !self.playing || !self.started || self.placed.is_empty() || self.open.is_none() || self.starting.is_some() || self.waiting || self.fade.is_some() {
            return false;
        }
        if self.strikes > 0 || self.eos_due || self.head.lower.is_some() || self.stamp.lower.is_some() {
            return false;
        }
        // A platform that has not said it asks for more is topped up on the engine's own time.
        if self.more() && !self.called_back {
            return false;
        }
        let rate = self.rate() as i64;
        let ms = |frames: u64| frames as i64 * 1000 / rate;
        let near = self.awake_before_ms();
        if self.placed.get(1).is_some_and(|p| ms(p.start.saturating_sub(self.heard_at)) <= near) {
            return false;
        }
        if self.tail.is_some() && !self.more() && ms(self.written_frames.saturating_sub(self.heard_at)) <= near {
            return false;
        }
        true
    }

    /// The track is topped up when it holds less than this, µs, while the platform has not shown that it
    /// asks for more: [`LOW_US`], or half of a track too small for that (as its bytes and the songs'
    /// bitrate make it), so it is never looked at for nothing.
    fn low_us(&self) -> i64 {
        if self.open.is_none() || self.written_bytes == 0 || self.written_frames == 0 {
            return LOW_US;
        }
        LOW_US.min(self.holds_us() / 2)
    }

    /// The CPU takes over where the ear is.
    fn fallback(&mut self, refused: bool) -> Step {
        if refused {
            self.on_cpu = Some(OnCpu::Failed);
        }
        self.note_left();
        let at = self.heard().map(|(i, ms, _)| (i, ms)).or(self.starting.as_ref().map(|s| (s.0, s.1)));
        self.release();
        match at {
            Some((index, ms)) => Step::ToPcm { index, ms, refused },
            None => Step::Fine,
        }
    }

    /// The song starting, once it is open: placed on a track for its format, or handed to the CPU.
    fn begin<L: Library, Q: Queue>(&mut self, tracks: &mut Sources<L>, queue: &Q, gain: &mut dyn FnMut(usize, &str) -> f32) -> Option<Step> {
        let (i, ms) = self.starting.as_ref().map(|s| (s.0, s.1))?;
        let ready = match &mut self.starting.as_mut().expect("checked").2 {
            Ok(r) => r.ready(),
            Err(_) => true,
        };
        if !ready {
            self.waiting = true;
            return Some(Step::Fine);
        }
        self.waiting = false;
        let (_, _, opened) = self.starting.take().expect("checked");
        let to_pcm = Some(Step::ToPcm { index: i, ms, refused: false });
        let Ok(r) = opened else {
            self.on_cpu = Some(OnCpu::Unread);
            return to_pcm;
        };
        let album = self.in_album(i, tracks, queue);
        let id = queue.read(|q| q.ids()[i].clone());
        let level = gain(i, &id);
        if let Some(why) = self.refuses(&r, album, level) {
            self.on_cpu = Some(why);
            return to_pcm;
        }
        let Some(song) = r.coded().cloned() else { return to_pcm };
        let coded = song.coded;
        if self.open.is_none_or(|o| o.0 != coded) {
            if self.open.take().is_some() {
                self.out.close();
                self.started = false;
            }
            let bps = if song.bitrate > 0 { song.bitrate } else { GUESS_BPS };
            let bytes = ((bps as i128 * TRACK_US as i128 / 8_000_000) as usize).clamp(MIN_BYTES, MAX_BYTES);
            match self.out.open(coded, bytes) {
                Ok(held) => {
                    let gapless = self.support(coded) == Support::Gapless;
                    self.open = Some((coded, gapless, held.max(1)));
                    self.granted = Some(bytes);
                }
                Err(_) => {
                    self.on_cpu = Some(OnCpu::WouldNotOpen(coded));
                    return Some(Step::ToPcm { index: i, ms, refused: true });
                }
            }
        }
        let gapless = self.open.is_some_and(|o| o.1);
        self.gapped = (!gapless && (song.delay != 0 || song.padding != 0)).then(|| {
            let said = self.out.said(coded).map(|s| format!(" ({s})")).unwrap_or_default();
            format!("its encoder delay of {} and padding of {} heard as a moment of near silence at its ends: no song of its album joins it, and the output does not do gapless offload{said}", song.delay, song.padding)
        });
        // A song started part way in has no delay left to cut.
        let delay = if song.from_frame > 0 { 0 } else { song.delay };
        self.out.delay_padding(delay, song.padding);
        let from_ms = song.from_frame * 1000 / coded.rate.max(1) as i64;
        self.level = level;
        self.seq += 1;
        self.placed.push_back(Placed { index: i, id, start: 0, frames: None, from_ms, level, seq: self.seq });
        let ogg = (coded.coding == Coding::Opus).then(|| Ogg::new(song.setup.as_deref()));
        self.writing = Some(Writing { r, frames: 0, ogg });
        self.volume();
        if self.fill(false, tracks, queue, gain).is_err() {
            return Some(self.fallback(true));
        }
        if let Some(asked) = self.granted.take() {
            // What the track holds: said, for the battery. The platform's asks set how often the thread
            // wakes to top it up, once it has asked; before that, the bytes do.
            let held = self.open.map_or(0, |o| o.2);
            let (holds, low) = (self.holds_us() / 1000, self.low_us() / 1000);
            self.note(format!(
                "the platform granted a track of {} KB of the {} KB asked: {holds} ms of this song, topped up when the platform asks for more (about every {low} ms if its chip buffers nothing of its own)",
                held / 1024,
                asked / 1024
            ));
        }
        if self.playing {
            self.out.play();
            self.started = true;
            self.clock = Some((self.now_ms, self.heard_at));
            self.clock_lag_ms = 0;
            self.play_clock = self.clock;
            if self.eos_due {
                self.end_stream();
            }
        }
        Some(Step::Fine)
    }

    /// Writes into the track what it has room for: the rest of the song being written, and the songs
    /// after it that join it without a gap. Err when the track refused a write with an error. `asked`:
    /// the platform asked for more, and the next song is written even while the count says the track
    /// holds more than it can (the count lags what the track played).
    fn fill<L: Library, Q: Queue>(&mut self, asked: bool, tracks: &mut Sources<L>, queue: &Q, gain: &mut dyn FnMut(usize, &str) -> f32) -> Result<(), i32> {
        loop {
            // The track holds minutes at most, whatever the platform would take: the rest waits for a
            // top-up.
            if self.in_track_us() >= TRACK_US {
                return Ok(());
            }
            if self.staged < self.stage.len() {
                if !self.write_staged()? {
                    return Ok(());
                }
                continue;
            }
            if let Some(w) = self.writing.as_mut() {
                if !w.r.ready() {
                    self.waiting = true;
                    return Ok(());
                }
                self.waiting = false;
                self.stage.clear();
                self.staged = 0;
                self.stage_frames = 0;
                while self.stage.len() < STAGE_BYTES && w.r.packet() {
                    let frames = w.r.packet_frames();
                    match w.ogg.as_mut() {
                        Some(ogg) => ogg.page(w.r.buffer(), &mut self.stage),
                        None => self.stage.extend_from_slice(w.r.buffer()),
                    }
                    self.stage_frames += frames;
                    w.frames += frames;
                    if !w.r.ready() {
                        break;
                    }
                }
                if !self.stage.is_empty() {
                    continue;
                }
                if !w.r.ready() {
                    continue;
                }
                // Read to its end: its length is known, and what follows is looked at.
                let frames = w.frames;
                if let Some((_, why)) = w.r.error() {
                    let id = self.placed.back().map(|p| p.id.clone()).unwrap_or_default();
                    let ms = frames as i64 * 1000 / self.rate() as i64;
                    self.note(format!("{id} was read to an early end at {ms} ms: {why}"));
                }
                self.writing = None;
                if let Some(p) = self.placed.back_mut() {
                    p.frames = Some(frames);
                }
                let last = self.placed.back().map(|p| p.index).expect("a song is placed");
                match self.next_of(last, queue) {
                    None => self.close(Tail::End),
                    Some(n) => {
                        let id = queue.read(|q| q.ids()[n].clone());
                        self.next = Some((n, tracks.open_packets(&id, 0, false)));
                    }
                }
                continue;
            }
            // The next song is written once less than half a minute is left, however small the track
            // (a chip that buffers on its own may not present the last of a song until more comes), so an
            // edit of the queue before then costs nothing; the whole of it is here by then (fetched as the
            // song before began).
            let lagging = asked && self.in_track_us() > self.holds_us() * 3 / 2;
            if self.next.is_none() || (self.in_track_us() >= LOW_US && !lagging) {
                return Ok(());
            }
            let (n, opened) = self.next.as_mut().expect("checked");
            let n = *n;
            let song = match opened {
                Ok(r) => {
                    if !r.ready() {
                        self.waiting = true;
                        return Ok(());
                    }
                    r.error().is_none().then(|| r.coded().cloned()).flatten()
                }
                Err(_) => None,
            };
            let id = queue.read(|q| q.ids()[n].clone());
            let level = gain(n, &id);
            // A song turned up is not for this output either (`refuses` says so once the track is out).
            let joins = nori_player::gain::offload_allows(level) && self.open.is_some_and(|(c, gapless, _)| gapless && song.as_ref().is_some_and(|s| s.coded == c));
            let Some(song) = song.filter(|_| joins) else {
                // Another format, or not for this output: the track plays out, and then it is decided.
                self.next = None;
                self.close(Tail::Then(n));
                return Ok(());
            };
            if self.pending_eos {
                // The song before is closed first: the platform takes that only while the track plays.
                self.end_stream();
                if self.pending_eos {
                    return Ok(());
                }
            }
            let (_, opened) = self.next.take().expect("checked");
            let r = opened.expect("checked");
            self.out.delay_padding(song.delay, song.padding);
            let start = self.written_frames;
            self.seq += 1;
            let from_ms = song.from_frame * 1000 / song.coded.rate.max(1) as i64;
            self.placed.push_back(Placed { index: n, id, start, frames: None, from_ms, level, seq: self.seq });
            let ogg = (song.coded.coding == Coding::Opus).then(|| Ogg::new(song.setup.as_deref()));
            self.writing = Some(Writing { r, frames: 0, ogg });
        }
    }

    /// Nothing more is written after what is: `tail` follows once it has played.
    fn close(&mut self, tail: Tail) {
        self.end_stream();
        self.tail = Some(tail);
    }

    /// The last packet written closes its song: said to the platform while the track plays, or once it
    /// does (Android refuses it otherwise). A refusal while playing is a strike.
    fn end_stream(&mut self) {
        if !self.pending_eos {
            self.eos_due = false;
            return;
        }
        if !self.playing || !self.started || self.open.is_none() {
            self.eos_due = true;
            return;
        }
        if self.out.end_of_stream() {
            self.pending_eos = false;
            self.eos_due = false;
            self.eos_refusals = 0;
            self.eos_at = Some(self.written_frames);
        } else {
            self.eos_due = true;
            self.eos_refusals += 1;
            let why = "the platform would not take the end of stream while its track played";
            self.note(format!("{why} ({} of {STRIKES})", self.eos_refusals));
            self.strike_why = why.into();
        }
    }

    /// Writes what is staged. False when the track would not take all of it (it is full).
    fn write_staged(&mut self) -> Result<bool, i32> {
        let left = self.stage.len() - self.staged;
        let frames = self.stage_frames;
        let taken = self.out.write(&self.stage[self.staged..], frames)?.min(left);
        self.full = taken < left;
        // The frames go with the bytes in proportion; they are exact once the whole stage is taken.
        let part = if taken == left { frames } else { (frames as u128 * taken as u128 / left as u128) as u64 };
        self.stage_frames -= part;
        self.written_frames += part;
        self.written_bytes += taken as u64;
        self.staged += taken;
        if taken > 0 {
            self.pending_eos = true;
        }
        Ok(taken == left)
    }

    /// The song starting, or the next packet, waits for its bytes.
    pub(crate) fn waiting_for_bytes(&self) -> bool {
        self.starting.is_some() || self.waiting
    }

    /// How long the thread may sleep before this path needs it, ms; none when nothing is due.
    pub(crate) fn wake_in(&self) -> Option<i64> {
        let mut d: Option<i64> = None;
        let mut at = |ms: i64| d = Some(d.map_or(ms, |x| x.min(ms)));
        if self.fade.is_some() {
            at(nori_player::transport::FADE_TICK_MS);
        }
        if self.starting.is_some() || self.waiting {
            // The loader wakes the thread when the bytes are there; this is in case it never does.
            at(1_000);
        }
        if !self.playing || self.placed.is_empty() {
            return d;
        }
        if self.strikes > 0 || self.head.lower.is_some() || self.stamp.lower.is_some() || self.eos_due {
            at(LOOK_AGAIN_MS);
        }
        let rate = self.rate() as i64;
        let ms = |frames: u64| frames as i64 * 1000 / rate;
        // The ear reaching the next song.
        if let Some(p) = self.placed.get(1) {
            at(ms(p.start.saturating_sub(self.heard_at)) + 5);
        }
        if self.more() {
            if self.full && self.called_back {
                // The platform's ask wakes the thread; this only in case it never comes, once what was
                // written could have played.
                at((self.in_track_us() / 1000).max(BACKSTOP_MS));
            } else if self.full {
                // A track that refused a write while it seemed low holds more than its bytes say: a second,
                // or half the time to the low mark in a track too small for that (64 KB on some phones), so
                // it never runs dry waiting.
                let floor = (self.low_us() / 2000).clamp(1, 1_000);
                at(((self.in_track_us() - self.low_us()) / 1000).max(0) + floor);
            } else {
                at(((self.in_track_us() - LOW_US) / 1000).max(0) + 1);
            }
        } else if self.tail.is_some() {
            let end = ms(self.written_frames.saturating_sub(self.heard_at));
            at(if end > 0 { end + 5 } else { END_LOOK_MS });
        }
        // The watchdog.
        if let Some(since) = self.moved_ms.filter(|_| self.started && self.in_track_us() > END_SLACK_US) {
            at((self.watch_at(since) - self.now_ms).max(0) + 1);
        }
        d
    }

    /// The music stops at the end of the song heard (the sleep timer's "end of this song"), or goes on
    /// again (`false`). True when a song after it is written already, which cannot be taken back: the
    /// track then starts again where the ear is, and the stop is set again on it.
    pub(crate) fn pause_at_end<L: Library, Q: Queue>(&mut self, on: bool, tracks: &mut Sources<L>, queue: &Q) -> bool {
        let Some(c) = self.current() else { return false };
        self.stop_after = on.then_some(c);
        if on && self.placed.len() > 1 {
            return true;
        }
        if self.writing.is_some() {
            return false;
        }
        if on {
            self.next = None;
            self.close(Tail::End);
        } else if self.tail == Some(Tail::End) {
            // Taken back before the end: what follows is written after all.
            self.tail = None;
            match self.next_of(c, queue) {
                Some(n) => {
                    let id = queue.read(|q| q.ids()[n].clone());
                    self.next = Some((n, tracks.open_packets(&id, 0, false)));
                }
                None => self.close(Tail::End),
            }
        }
        false
    }

    /// The queue changed (an edit, shuffle, repeat): what the track holds is found again by id, and the
    /// song after the last one written is looked at again. A song already written after the one heard
    /// that no longer follows it cannot be taken back: the track starts again where the ear is (true).
    pub(crate) fn queue_changed<L: Library, Q: Queue>(&mut self, old: &[String], tracks: &mut Sources<L>, queue: &Q) -> bool {
        let new: Vec<String> = queue.read(|q| q.ids().to_vec());
        for p in self.placed.iter_mut() {
            match moved(old, &new, p.index, &p.id) {
                Some(k) => p.index = k,
                None => return true,
            }
        }
        if let Some(s) = self.starting.as_mut() {
            let id = old.get(s.0).cloned().unwrap_or_default();
            match moved(old, &new, s.0, &id) {
                Some(k) => s.0 = k,
                None => return true,
            }
        }
        for k in 1..self.placed.len() {
            if self.next_of(self.placed[k - 1].index, queue) != Some(self.placed[k].index) {
                return true;
            }
        }
        if self.writing.is_some() {
            return false;
        }
        let Some(last) = self.placed.back().map(|p| p.index) else { return false };
        let after = self.next_of(last, queue);
        let was = self.next.as_ref().map(|(n, _)| *n).or(match self.tail {
            Some(Tail::Then(n)) => Some(n),
            _ => None,
        });
        if was == after {
            return false;
        }
        // Another song follows now (or none): it is opened, and written at the join as any other.
        self.next = None;
        self.tail = None;
        match after {
            Some(n) => {
                let id = new[n].clone();
                self.next = Some((n, tracks.open_packets(&id, 0, false)));
            }
            None => self.close(Tail::End),
        }
        false
    }
}

/// Where the song at index `i` of `old` (`id`) is in `new`: the same id, nearest to where it was.
fn moved(old: &[String], new: &[String], i: usize, id: &str) -> Option<usize> {
    let id = old.get(i).map_or(id, String::as_str);
    new.iter().enumerate().filter(|(_, n)| *n == id).map(|(j, _)| j).min_by_key(|&j| j.abs_diff(i))
}

impl Drop for Offload {
    fn drop(&mut self) {
        if self.open.take().is_some() {
            self.out.close();
        }
    }
}

/// Opus packets put in Ogg pages for the chip, as media3's `OggOpusAudioPacketizer` does: the stream's
/// header (`OpusHead`) and an empty comment header first, then each packet in a page of its own, stamped
/// with the samples decoded up to its end.
pub(crate) struct Ogg {
    head: Option<Vec<u8>>,
    sequence: u32,
    granule: u64,
}

/// The stream every page belongs to.
const OGG_SERIAL: u32 = 0;

impl Ogg {
    pub(crate) fn new(opus_head: Option<&[u8]>) -> Ogg {
        let head = opus_head.filter(|h| h.starts_with(b"OpusHead")).map(<[u8]>::to_vec).unwrap_or_else(|| {
            // A plain stereo header: what media3 writes when the stream brought none.
            let mut h = b"OpusHead".to_vec();
            h.extend_from_slice(&[1, 2, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, 0]);
            h
        });
        Ogg { head: Some(head), sequence: 0, granule: 0 }
    }

    /// `packet` as a page (after the two header pages, before the first one) onto `out`.
    pub(crate) fn page(&mut self, packet: &[u8], out: &mut Vec<u8>) {
        if let Some(head) = self.head.take() {
            self.write(&head, 0x02, 0, out);
            let mut tags = b"OpusTags".to_vec();
            tags.extend_from_slice(&[0; 8]);
            self.write(&tags, 0, 0, out);
        }
        self.granule += opus_samples(packet);
        let granule = self.granule;
        self.write(packet, 0, granule, out);
    }

    fn write(&mut self, packet: &[u8], flags: u8, granule: u64, out: &mut Vec<u8>) {
        let from = out.len();
        let lacing = packet.len() / 255 + 1;
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(flags);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&OGG_SERIAL.to_le_bytes());
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.push(lacing as u8);
        for _ in 0..lacing - 1 {
            out.push(255);
        }
        out.push((packet.len() % 255) as u8);
        out.extend_from_slice(packet);
        let crc = ogg_crc(&out[from..]);
        out[from + 22..from + 26].copy_from_slice(&crc.to_le_bytes());
        self.sequence += 1;
    }
}

/// Samples (at 48 kHz) an Opus packet decodes to, from its table of contents (RFC 6716, 3.1).
pub(crate) fn opus_samples(packet: &[u8]) -> u64 {
    let Some(&toc) = packet.first() else { return 0 };
    let config = toc >> 3;
    let frame: u64 = match config {
        0..=11 => [480, 960, 1920, 2880][(config % 4) as usize],
        12..=15 => [480, 960][(config % 2) as usize],
        _ => [120, 240, 480, 960][(config % 4) as usize],
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => packet.get(1).map_or(0, |b| (b & 0x3f) as u64),
    };
    frame * frames
}

/// Ogg's page checksum: CRC-32 with the polynomial 0x04c11db7, unreflected, from nought.
fn ogg_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in page {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04c1_1db7 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_packets_go_in_ogg_pages_with_their_samples_counted() {
        let mut ogg = Ogg::new(Some(b"OpusHead\x01\x02\x38\x01\x80\xbb\0\0\0\0\0"));
        let mut out = Vec::new();
        // A 20 ms packet (config 1: SILK 20 ms), one frame.
        ogg.page(&[0x08, 1, 2, 3], &mut out);
        // Three pages: the header, the comment header, the packet.
        let pages: Vec<usize> = out.windows(4).enumerate().filter(|(_, w)| *w == b"OggS").map(|(i, _)| i).collect();
        assert_eq!(pages.len(), 3);
        assert_eq!(out[pages[0] + 5], 0x02, "the first page begins the stream");
        let granule = u64::from_le_bytes(out[pages[2] + 6..pages[2] + 14].try_into().unwrap());
        assert_eq!(granule, 960, "20 ms at 48 kHz");
        // Each page's checksum is Ogg's own: computed with its checksum field zeroed.
        for (k, &p) in pages.iter().enumerate() {
            let end = pages.get(k + 1).copied().unwrap_or(out.len());
            let mut page = out[p..end].to_vec();
            let said = u32::from_le_bytes(page[22..26].try_into().unwrap());
            page[22..26].fill(0);
            assert_eq!(ogg_crc(&page), said);
        }
        // A packet of 255 bytes or more takes more than one lacing value.
        let mut out = Vec::new();
        ogg.page(&vec![0x08; 300], &mut out);
        assert_eq!(out[26], 2, "two lacing values");
        assert_eq!((out[27], out[28]), (255, 45));
    }

    #[test]
    fn the_samples_of_an_opus_packet_come_from_its_table_of_contents() {
        assert_eq!(opus_samples(&[0x08]), 960, "SILK 20 ms");
        assert_eq!(opus_samples(&[0xfc]), 960, "CELT 20 ms");
        assert_eq!(opus_samples(&[0xf9]), 1920, "two CELT 20 ms frames");
        assert_eq!(opus_samples(&[0xfb, 0x03]), 2880, "three frames, counted in the second byte");
    }

    /// 100 ms at 44.1 kHz, as the engine reads it.
    const JITTER: u64 = 4_410;

    #[test]
    fn the_head_counts_on_through_a_join_that_starts_it_again() {
        let mut h = Head::default();
        let far = u64::MAX;
        assert_eq!(h.read(1_000, None, false, JITTER, far), Ok((1_000, Seen::Fine)));
        assert_eq!(h.read(10_990, None, false, JITTER, far), Ok((10_990, Seen::Fine)));
        // The platform started counting again at the join: the next song began at 11 000.
        assert_eq!(h.read(20, Some(11_000), false, JITTER, far), Ok((11_020, Seen::Joined)));
        assert_eq!(h.read(500, None, false, JITTER, far), Ok((11_500, Seen::Fine)));
    }

    #[test]
    fn a_lower_reading_away_from_a_join_does_not_move_the_ear() {
        let mut h = Head::default();
        let far = u64::MAX;
        assert_eq!(h.read(44_100, None, true, JITTER, far), Ok((44_100, Seen::Fine)));
        // Nought once (a failed call read as nought, a moment's reset): the ear stays, and the count goes
        // on as it was when the next reading is back.
        assert_eq!(h.read(0, None, true, JITTER, far), Ok((44_100, Seen::Dip)));
        assert_eq!(h.read(46_000, None, true, JITTER, far), Ok((46_000, Seen::Fine)));
        // Counting again from nought for good (a standby, or after an end of stream): taken at the second
        // reading, from where the ear was.
        assert_eq!(h.read(10, None, true, JITTER, far), Ok((46_000, Seen::Dip)));
        assert_eq!(h.read(900, None, true, JITTER, far), Ok((46_900, Seen::Restarted)));
        assert_eq!(h.read(1_900, None, true, JITTER, far), Ok((47_900, Seen::Fine)));
    }

    #[test]
    fn a_count_started_again_away_from_a_join_is_taken_only_where_one_is_plausible() {
        let mut h = Head::default();
        let far = u64::MAX;
        assert_eq!(h.read(441_000, None, false, JITTER, far), Ok((441_000, Seen::Fine)));
        // No end of stream said, no pause: two low readings in a row leave the ear where it was.
        assert_eq!(h.read(10, None, false, JITTER, far), Ok((441_000, Seen::Dip)));
        assert_eq!(h.read(900, None, false, JITTER, far), Ok((441_000, Seen::Dip)));
        // A drop to half way is no count from nought, whatever may be.
        assert_eq!(h.read(300_000, None, true, JITTER, far), Ok((441_000, Seen::Dip)));
        assert_eq!(h.read(300_100, None, true, JITTER, far), Ok((441_000, Seen::Dip)));
        assert_eq!(h.read(441_500, None, true, JITTER, far), Ok((441_500, Seen::Fine)));
    }

    #[test]
    fn a_moment_back_holds_the_count_and_is_never_a_count_started_again() {
        let mut h = Head::default();
        let far = u64::MAX;
        // A Galaxy S22's timestamp as its track starts: 10, 6, 5, 4, 6 (each once taken for a count
        // started again, the ear put 10 and then 16 frames on).
        assert_eq!(h.read(10, None, true, JITTER, far), Ok((10, Seen::Fine)));
        for (raw, back) in [(6, 4), (5, 5), (4, 6), (4, 6), (6, 4)] {
            assert_eq!(h.read(raw, None, true, JITTER, far), Ok((10, Seen::Jitter(back))));
        }
        assert_eq!(h.read(7_074, None, true, JITTER, far), Ok((7_074, Seen::Fine)));
        for raw in [7_066, 7_065, 7_067, 7_065, 7_066, 7_064] {
            assert_eq!(h.read(raw, None, true, JITTER, far).map(|r| r.0), Ok(7_074), "{raw}");
        }
        // Steps back of a few frames to 80 ms as it plays on, even where the next song's start is in
        // reach: never a join.
        assert_eq!(h.read(3_021_762, None, true, JITTER, far), Ok((3_021_762, Seen::Fine)));
        assert_eq!(h.read(3_021_759, Some(3_022_000), true, JITTER, far), Ok((3_021_762, Seen::Jitter(3))));
        assert_eq!(h.read(3_018_234, Some(3_022_000), true, JITTER, far), Ok((3_021_762, Seen::Jitter(3_528))));
        assert_eq!(h.read(3_021_800, None, true, JITTER, far), Ok((3_021_800, Seen::Fine)));
    }

    #[test]
    fn a_reading_ahead_of_the_clock_changes_nothing() {
        let mut h = Head::default();
        assert_eq!(h.read(1_000, None, false, JITTER, 50_000), Ok((1_000, Seen::Fine)));
        assert_eq!(h.read(4_000_000, None, false, JITTER, 50_000), Err(4_000_000));
        assert_eq!(h.read(12_000, None, false, JITTER, 50_000), Ok((12_000, Seen::Fine)), "the count as it was");
        // A join the clock says the ear cannot have reached is not one.
        assert_eq!(h.read(10, None, false, JITTER, 50_000), Ok((12_000, Seen::Dip)));
    }
}
