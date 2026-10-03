//! Audio offload: song packets handed undecoded to an output that decodes them itself (a phone's DSP),
//! only while nothing touches the samples (`nori_player::policy::audio_policy`) and the output takes
//! the song's compression. Done as media3's sink does it: songs of one format join gaplessly on one track
//! (encoder delay and padding told per song, [`OffloadOutput::end_of_stream`] between them), another
//! format gets its own track, volume is ReplayGain and fades (never the samples), Opus goes in Ogg pages.
//!
//! The track is asked to hold [`TRACK_US`] and topped up below [`LOW_US`]. Phones grant far less (32 KB on
//! a Galaxy S21 FE) and buffer more on the DSP, so once the platform has asked for more
//! (`onDataRequest`) a full track waits for its next ask instead of estimating from bytes.
//! [`Offload::lets_cpu_sleep`] says when nothing but that ask is due (Android drops its wake lock then).
//!
//! Watchdog: with the screen off a phone's timestamp and play head may stand still for seconds while the
//! DSP plays from its own buffer. The CPU takes over only when neither count moved and the platform asked
//! for nothing for longer than the music written past the count plus a slack that grows with the longest
//! standstill seen ([`STUCK_SLACK_MS`] minimum); it resumes where the count last was. A count that stands
//! while the platform keeps asking is not kept by the platform: the other count is used, or with neither
//! moving the CPU resumes where the clock says.

use std::collections::VecDeque;

use nori_player::pipeline::{Queue, Reading, Known, Songs};
use nori_player::playlist::Playlist;
use nori_player::transitions::{in_album_run, WindowSong};

pub use crate::demux::{Coded, CodedSong, Coding};
use crate::demux::Demuxed;
use crate::library::{Library, Sources};

/// Whether an output decodes a compression, and whether it joins songs gaplessly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Support {
    No,
    Plain,
    Gapless,
}

/// An output that decodes compressed songs (Android: an offload AudioTrack). Called only on the engine's
/// thread; the platform wakes that thread when the track wants more or was torn down.
pub trait OffloadOutput: Send {
    /// Whether the output decodes `coded` where the music goes now.
    fn supports(&mut self, coded: Coded) -> Support;
    /// Opens a paused track for `coded` of about `bytes`, replacing any open one. Returns its size.
    fn open(&mut self, coded: Coded, bytes: usize) -> Result<usize, String>;
    /// Writes what fits of `data` without blocking: bytes taken, or the track's error. `data` is
    /// `frames` frames of music (for a simulated track).
    fn write(&mut self, data: &[u8], frames: u64) -> Result<usize, i32>;
    /// Encoder delay and padding of the song whose packets come next.
    fn delay_padding(&mut self, delay: u32, padding: u32);
    /// The last packet written ended its song; the next joins gaplessly. False when refused (Android
    /// throws unless the track plays). Android stops the track until it has presented everything, so a
    /// track told this is closed rather than flushed.
    fn end_of_stream(&mut self) -> bool;
    fn play(&mut self);
    fn pause(&mut self);
    /// Drops what was written and not played. Only while paused.
    fn flush(&mut self);
    fn set_volume(&mut self, volume: f32);
    /// Frames presented since open or flush; None when the call failed (never 0 for a failure). May
    /// restart from 0 at a gapless join.
    fn head(&mut self) -> Option<u64>;
    /// Frames presented now by the platform's timestamp (Android's `getTimestamp`), extrapolated while
    /// playing as media3 does; counted like [`OffloadOutput::head`]. Preferred to the play head, which
    /// never moves on some phones.
    fn timestamp(&mut self) -> Option<u64> {
        None
    }
    /// The platform asked for more since last asked (`onDataRequest`): the track has room.
    fn data_requested(&mut self) -> bool {
        false
    }
    /// Everything written up to the last end of stream was played.
    fn presented(&mut self) -> bool;
    /// The track was torn down since last asked (the output moved where offload cannot follow).
    fn torn_down(&mut self) -> bool;
    fn close(&mut self);
    /// The platform's own answer to the last `supports(coded)`, for a report.
    fn said(&mut self, _coded: Coded) -> Option<String> {
        None
    }
    /// A note for the perf report (why a song ended, a bad play head).
    fn note(&mut self, _what: &str) {}
    /// How much faster than real time the track may present: 1 for a real device, more for a simulation.
    fn pace(&self) -> f64 {
        1.0
    }
}

/// Why a song plays on the CPU although offload is allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnCpu {
    /// It would not open as packets.
    Unread,
    /// A compression no output decodes here (FLAC, Vorbis, ALAC, HE-AAC, PCM).
    Compression(&'static str),
    /// The output does not decode it where the music goes now; the platform's answer.
    Unsupported(Coded, Option<String>),
    /// Encoder delay or padding, a gapless album join, and an output without gapless offload.
    NotGapless { coded: Coded, delay: u32, padding: u32, said: Option<String> },
    WouldNotOpen(Coded),
    /// The track was torn down or refused a write.
    Failed,
    /// The play head made no sense, or an end of stream was refused.
    Head(String),
    /// ReplayGain turns it up, which needs the samples and the limiter.
    TurnedUp,
}

impl OnCpu {
    /// For the perf report and the log.
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

/// Music a track is asked to hold.
pub const TRACK_US: i64 = 240_000_000;
/// Topped up, and the next song written, below this.
pub const LOW_US: i64 = 30_000_000;
/// Bounds of the track size asked for.
const MIN_BYTES: usize = 512 * 1024;
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Bitrate assumed when unknown: high, so the track is not too small.
const GUESS_BPS: u32 = 320_000;
/// Bytes gathered for one write.
const STAGE_BYTES: usize = 256 * 1024;
/// Written music counts as played this close to its end: a DSP's count can stop a decoder delay short.
const END_SLACK_US: i64 = 100_000;
/// How often to look while the end is due and not yet reached.
const END_LOOK_MS: i64 = 100;
/// The platform's "presented everything" counts as the end only this close to it by the count.
const PRESENTED_NEAR_US: i64 = 3_000_000;
/// How far the play head may run ahead of the clock since playing began (start latency), ms.
const CLOCK_SLACK_MS: i64 = 500;
/// A reading may step back this far without meaning anything, ms: Android's extrapolated timestamp
/// jitters by a few frames between anchors. A restart at a join drops a whole song, far more.
const JITTER_MS: i64 = 100;
/// For [`START_MS`] after playing begins the count is held within this of the clock, ms: the first
/// timestamps can read ~160 ms ahead (Galaxy S22) before correcting.
const START_SLACK_MS: i64 = 10;
const START_MS: i64 = 1_000;
/// Notes of a misbehaving count: [`NOTES_AT_ONCE`] at most, then one per this long, ms (each costs a
/// string and a log write).
const NOTE_GAP_MS: i64 = 10_000;
const NOTES_AT_ONCE: i64 = 4;
/// Bad play head readings (or refused ends of stream) in a row before the CPU takes over.
const STRIKES: u32 = 3;
/// How soon to look again after a bad reading or a pending end of stream, ms.
const LOOK_AGAIN_MS: i64 = 300;
/// The watchdog's least slack, ms (an S21 FE's count stood 2.8 s with the screen off while playing).
const STUCK_SLACK_MS: i64 = 10_000;
/// The slack grows to twice the longest standstill seen, up to this.
const STUCK_SLACK_MAX_MS: i64 = 60_000;
/// Asks for more since the count last moved that mark the count as one the platform does not keep.
const DEAD_ASKS: u32 = 2;
/// A full track waiting for the platform's ask is still looked at after this, ms, in case it never comes.
const BACKSTOP_MS: i64 = 500;
/// The CPU is kept awake from this long before a song boundary or the end of what was written, so the
/// song's volume and event come on time; more on a platform that asks seldom ([`Offload::awake_before_ms`]).
const AWAKE_BEFORE_MS: i64 = 3_000;
const AWAKE_BEFORE_MAX_MS: i64 = 60_000;

/// A song written to the track from its frame `start` in it.
#[derive(Debug, Clone)]
struct Placed {
    index: usize,
    id: String,
    start: u64,
    /// Song time of its first frame (a seek lands in a packet).
    from_ms: i64,
    level: f32,
    /// Placement count, so repeat-one placing it again is distinguishable.
    seq: u64,
}

/// What comes after the last song written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// End of the queue, or of the sleep timer's song.
    End,
    /// This queue index from its start, which needs another track or the CPU.
    Then(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Fine,
    /// Hand this song and place to the CPU (`refused`: the track failed).
    ToPcm { index: usize, ms: i64, refused: bool },
}

/// The platform's frame count made monotonic: a count that restarts at a gapless join continues from
/// the song that starts there.
#[derive(Debug, Clone, Copy, Default)]
struct Head {
    base: u64,
    last: u64,
    /// A reading below the last, pending: the next reading says whether the count restarted.
    lower: Option<u64>,
}

/// How a reading was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Fine,
    /// Back by no more than jitter (this many frames): the count holds.
    Jitter(u64),
    /// Back where the next song may have started: the count restarted at the join.
    Joined,
    /// Back, away from a join: set aside until the next reading.
    Dip,
    /// Back twice near zero where a restart is plausible (after an end of stream or a pause): counted on
    /// from where it was.
    Restarted,
}

impl Head {
    /// Takes `raw`. `join`: where the next song starts, if the clock allows reaching it; `restart`: the
    /// platform may have restarted its count away from a join; `jitter`: frames a reading may step back;
    /// `most`: the furthest the clock allows. A reading past `most` changes nothing: Err with where it
    /// would have put the count.
    fn read(&mut self, raw: u64, join: Option<u64>, restart: bool, jitter: u64, most: u64) -> Result<(u64, Seen), u64> {
        let (base, last, seen) = if raw >= self.last {
            (self.base, raw, Seen::Fine)
        } else if self.last - raw <= jitter {
            self.lower = None;
            return Ok((self.base + self.last, Seen::Jitter(self.last - raw)));
        } else if let Some(start) = join {
            (start, raw, Seen::Joined)
        } else if restart && raw < self.last / 2 && self.lower.is_some_and(|l| raw >= l) {
            (self.base + self.last, raw, Seen::Restarted)
        } else {
            // Only a drop towards zero may be a restart.
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

/// A volume fade on the track: from, to, start (ms), length.
#[derive(Debug, Clone, Copy)]
struct Fade {
    from: f32,
    to: f32,
    start_ms: i64,
    ms: i32,
}

/// The song being written, and its frames written.
struct Writing {
    r: Demuxed,
    frames: u64,
    ogg: Option<Ogg>,
}

/// What is written to the track and how far it played; emptied with the track.
#[derive(Default)]
struct Run {
    placed: VecDeque<Placed>,
    writing: Option<Writing>,
    /// The first song, opening; placed or handed to the CPU once ready.
    starting: Option<(usize, i64, Result<Demuxed, String>)>,
    /// The song after the last written, opened once that one was read to its end.
    next: Option<(usize, Result<Demuxed, String>)>,
    tail: Option<Tail>,
    /// Bytes of the stage written, and the stage's frames not yet written.
    staged: usize,
    stage_frames: u64,
    written_bytes: u64,
    written_frames: u64,
    /// The platform's play head and timestamp made monotonic, which one the last reading came from,
    /// and those found not to move while playing (ignored for the rest of the track).
    head: Head,
    stamp: Head,
    by_stamp: bool,
    stamp_dead: bool,
    head_dead: bool,
    /// When the count last moved (or playing began); None until the next turn.
    moved_ms: Option<i64>,
    /// Platform asks since the count last moved, and when it last asked (and last asked while playing).
    asks: u32,
    asked_ms: Option<i64>,
    last_ask_ms: Option<i64>,
    /// Frames presented, as last read.
    heard_at: u64,
    /// The next packet's bytes were still on their way.
    waiting: bool,
    /// Written since the last end of stream, so the next join has one to close.
    pending_eos: bool,
    /// The last write was partly refused: the track is full.
    full: bool,
    /// When the count last moved sensibly (or playing began) and its frames then: it cannot be further on
    /// than the clock since. `clock_lag_ms`: how far the count lagged the clock between the last two
    /// moving readings, which loosens the bound. `play_clock`: when playing last began, the widest bound.
    clock: Option<(i64, u64)>,
    clock_lag_ms: i64,
    play_clock: Option<(i64, u64)>,
    /// The raw count last read.
    raw: Option<u64>,
    /// Bad readings in a row.
    strikes: u32,
    /// An end of stream is pending (the track was not playing, or refused it), and refusals in a row.
    eos_due: bool,
    eos_refusals: u32,
    /// Frames written when the last end of stream was taken: `presented` refers to it only then.
    eos_at: Option<u64>,
    end_noted: bool,
    /// Paused and played since the count last moved: the platform may restart its count after standby.
    resumed: bool,
    /// Jitter readings, and the largest step back, noted once at the end.
    jitter: (u32, u64),
}

/// The engine's offload path: the track, the songs written to it, and the one being written.
pub(crate) struct Offload {
    out: Box<dyn OffloadOutput>,
    /// The track's format, whether it joins gaplessly, and its size in bytes.
    open: Option<(Coded, bool, usize)>,
    supported: Vec<(Coded, Support)>,
    t: Run,
    /// Bytes gathered for one write (allocated once).
    stage: Vec<u8>,
    /// Longest standstill of the count before it moved, ms (the watchdog's slack grows with it).
    quiet_ms: i64,
    /// The platform has asked for more on this track, so it says when a full track has room.
    called_back: bool,
    /// The longest gap between the platform's asks while playing.
    ask_gap_ms: i64,
    /// Bytes asked for at open, until the grant is noted.
    granted: Option<usize>,
    playing: bool,
    /// Told to play since opened or paused: Android takes an end of stream only then.
    started: bool,
    /// The song playback stops after (the sleep timer).
    pub stop_after: Option<usize>,
    gain: f32,
    level: f32,
    fade: Option<Fade>,
    seq: u64,
    /// Why the last song left for the CPU.
    pub(crate) on_cpu: Option<OnCpu>,
    /// The last song placed keeps an encoder gap the output cannot cut (no gapless offload), for the report.
    pub(crate) gapped: Option<String>,
    /// This turn's time, ms.
    now_ms: i64,
    /// Why the last bad reading was bad.
    strike_why: String,
    /// Rate limiting of notes ([`NOTE_GAP_MS`]), and how many were dropped since the last.
    notes_from_ms: i64,
    quieted: u32,
}

impl Offload {
    pub(crate) fn new(out: Box<dyn OffloadOutput>) -> Offload {
        Offload {
            out,
            open: None,
            supported: Vec::new(),
            t: Run::default(),
            stage: Vec::with_capacity(STAGE_BYTES + 64 * 1024),
            quiet_ms: 0,
            called_back: false,
            ask_gap_ms: 0,
            granted: None,
            playing: false,
            started: false,
            stop_after: None,
            gain: 1.0,
            level: 1.0,
            fade: None,
            seq: 0,
            on_cpu: None,
            gapped: None,
            now_ms: 0,
            strike_why: String::new(),
            notes_from_ms: i64::MIN / 2,
            quieted: 0,
        }
    }

    /// The offload path holds the current song (playing or paused).
    pub(crate) fn active(&self) -> bool {
        self.t.starting.is_some() || !self.t.placed.is_empty()
    }

    pub(crate) fn playing(&self) -> bool {
        self.playing && self.active()
    }

    /// A track is playing, so this path's watchdog judges stalls ([`Offload::stuck`]).
    pub(crate) fn watching(&self) -> bool {
        self.playing && self.started && self.open.is_some() && !self.t.placed.is_empty()
    }

    /// The open track's format.
    pub(crate) fn track(&self) -> Option<Coded> {
        self.open.map(|o| o.0)
    }

    /// Whether the output decodes `coded`, asked once per format per output device.
    fn support(&mut self, coded: Coded) -> Support {
        if let Some((_, s)) = self.supported.iter().find(|(c, _)| *c == coded) {
            return *s;
        }
        let s = self.out.supports(coded);
        self.supported.push((coded, s));
        s
    }

    /// The output device changed: support is asked again.
    pub(crate) fn output_moved(&mut self) {
        self.supported.clear();
    }

    /// Why this output would not play `r`, or None. Unlike media3, gapless support is required only for
    /// a gapless album join (`album`, [`Offload::in_album`]): elsewhere an MP3's few ms of encoder gap
    /// are inaudible. A song turned up by ReplayGain (`level` > 1) needs its samples.
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

    /// Whether song `i` joins a neighbour of its album gaplessly (`nori_player::transitions::in_album_run`).
    pub(crate) fn in_album<L: Library, Q: Queue>(&self, i: usize, tracks: &Sources<L>, queue: &Known<Q>) -> bool {
        let (ids, runs, before, after, shuffling) = queue.read(|q| {
            let repeat = q.repeat();
            (q.ids().to_vec(), q.album_runs().to_vec(), q.previous_of(i, repeat), q.next_of(i, repeat), q.shuffling())
        });
        let after = after.filter(|_| self.stop_after != Some(i));
        let about = |k: usize| ids.get(k).map(|id| WindowSong { album_run: runs.get(k).copied().unwrap_or(0), ..tracks.about(id) });
        let Some(song) = about(i) else { return false };
        in_album_run(before.and_then(about).as_ref(), &song, after.and_then(about).as_ref(), shuffling)
    }

    /// Starts queue index `i` at `from_ms`: empties the track and opens the song as packets. Whether it
    /// plays here is known once open ([`Offload::turn`]).
    pub(crate) fn start<L: Library, Q: Queue>(&mut self, i: usize, from_ms: i64, tracks: &mut Sources<L>, queue: &Known<Q>) -> usize {
        self.empty();
        self.stop_after = None;
        // A skipped song (explicit) gives way to the next that is not.
        let i = if queue.skips(i) { self.next_of(i, queue).unwrap_or(i) } else { i };
        let id = queue.read(|q| q.ids()[i].clone());
        let opened = tracks.open_packets(&id, from_ms, false);
        self.t.starting = Some((i, from_ms, opened));
        i
    }

    /// Drops everything written and being written; the track stays open.
    fn empty(&mut self) {
        if self.t.eos_at.is_some() && !self.out.presented() && self.open.take().is_some() {
            // After an end of stream Android's track is stopping until it presents it; flushed then, it
            // refuses non-blocking writes. Like media3, open a new track instead.
            self.out.close();
            self.started = false;
        } else if self.open.is_some() {
            self.out.pause();
            self.started = false;
            self.out.flush();
        }
        self.t = Run::default();
        self.stage.clear();
    }

    /// The CPU takes over at `now_ms`: reads the count once more (the last turn may be a top-up ago),
    /// notes it and releases the track. Returns the place (queue index, ms).
    pub(crate) fn leave(&mut self, now_ms: i64) -> Option<(usize, i64)> {
        self.now_ms = now_ms;
        if self.playing && self.started && !self.t.placed.is_empty() {
            self.read_head();
        }
        self.note_left();
        self.release()
    }

    /// Notes where the CPU takes over, the platform's count and what was written, in ms.
    fn note_left(&mut self) {
        let Some((_, ms, _)) = self.heard() else { return };
        let rate = self.rate() as u64;
        let f = |frames: u64| frames * 1000 / rate;
        let count = if self.t.by_stamp { self.t.stamp } else { self.t.head };
        let said = match self.t.raw {
            Some(_) => format!("{} ms by its {}", f(count.base + count.last), if self.t.by_stamp { "timestamp" } else { "play head" }),
            None => "nothing".into(),
        };
        let what = format!("offload: left at {ms} ms (chip said {said}, heard {} ms, written {} ms)", f(self.t.heard_at), f(self.t.written_frames));
        self.note(what);
    }

    /// Releases the track; returns the place (queue index, ms).
    pub(crate) fn release(&mut self) -> Option<(usize, i64)> {
        let at = self.heard().map(|(i, ms, _)| (i, ms)).or(self.t.starting.as_ref().map(|s| (s.0, s.1)));
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
        self.t.moved_ms = None;
        self.t.last_ask_ms = None;
        self.t.play_clock = None;
        if self.open.is_some() && !self.t.placed.is_empty() {
            self.t.resumed = true;
            self.out.play();
            self.started = true;
            if self.t.eos_due {
                self.end_stream();
            }
        }
    }

    pub(crate) fn pause(&mut self) {
        self.playing = false;
        self.t.moved_ms = None;
        self.t.last_ask_ms = None;
        if let Some(f) = self.fade.take() {
            // The pause's fade ends at its target, not a tick short.
            self.gain = f.to;
            self.volume();
        }
        if self.open.is_some() {
            self.out.pause();
            self.started = false;
        }
    }

    /// Fades the track from `from` (or where it is) to `to` over `ms`, starting `now_ms`.
    pub(crate) fn ramp(&mut self, from: Option<f32>, to: f32, ms: i64, now_ms: i64) {
        self.fade = Some(Fade { from: from.unwrap_or(self.gain), to, start_ms: now_ms, ms: ms.clamp(0, i32::MAX as i64) as i32 });
        self.follow_fade(now_ms);
    }

    /// Sets the current song's ReplayGain level (the settings changed).
    pub(crate) fn set_level(&mut self, level: f32) {
        self.level = level;
        if let Some(p) = self.t.placed.front_mut() {
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

    /// The furthest the count can be by the clock: the last sensible reading moved on by the time since.
    fn most(&self) -> u64 {
        let rate = self.rate() as f64 * self.out.pace();
        let run = |ms: i64| (ms.max(0) as f64 * rate / 1000.0) as u64;
        match self.t.clock {
            Some((t, at)) => at + run(self.now_ms - t + CLOCK_SLACK_MS + self.t.clock_lag_ms),
            None => self.t.heard_at + run(CLOCK_SLACK_MS),
        }
    }

    /// Frames presented now, monotonic and never ahead of the clock: by timestamp where available, else
    /// by play head. A bad reading changes nothing and is a strike.
    fn read_head(&mut self) -> u64 {
        if self.open.is_none() {
            return self.t.heard_at;
        }
        let stamp = if self.t.stamp_dead { None } else { self.out.timestamp() };
        let (raw, by_stamp) = match stamp {
            Some(s) => (s, true),
            // A dead play head is left to the watchdog.
            None if self.t.head_dead => return self.t.heard_at,
            None => match self.out.head() {
                Some(h) => (h, false),
                None => {
                    self.strike("the platform's play head could not be read".into());
                    return self.t.heard_at;
                }
            },
        };
        self.t.by_stamp = by_stamp;
        self.t.raw = Some(raw);
        let most = self.most();
        let rate = self.rate() as i64;
        let jitter = (JITTER_MS * rate / 1000) as u64;
        let mut count = if by_stamp { self.t.stamp } else { self.t.head };
        let counted = count.base + count.last;
        // A join the clock says may have been reached.
        let join = self.t.placed.iter().map(|p| p.start).find(|&s| s > counted).filter(|&s| s <= most);
        // Away from a join, a restart is plausible only after an end of stream or a pause.
        let restart = self.t.eos_at.is_some() || self.t.resumed;
        let was = self.t.heard_at;
        let last = count.last;
        let read = count.read(raw, join, restart, jitter, most);
        if by_stamp {
            self.t.stamp = count;
        } else {
            self.t.head = count;
        }
        let what = if by_stamp { "timestamp" } else { "play head" };
        match read {
            Ok((at, seen)) => {
                match seen {
                    Seen::Jitter(back) => self.t.jitter = (self.t.jitter.0.saturating_add(1), self.t.jitter.1.max(back)),
                    Seen::Dip => self.note_now_and_then(|| format!("the {what} read {raw} after {last}, not at a join: looked at again")),
                    Seen::Restarted => self.note_now_and_then(|| format!("the {what} counts again from nought ({raw}): the count goes on from {counted} frames")),
                    Seen::Fine | Seen::Joined => {}
                }
                // Held to the clock just after playing began (early timestamps read ahead).
                let start = self.t.play_clock.filter(|&(t, _)| self.now_ms - t < START_MS);
                let pace = self.out.pace();
                let at = start.map_or(at, |(t, from)| at.min(from + ((self.now_ms - t + START_SLACK_MS).max(0) as f64 * rate as f64 * pace / 1000.0) as u64));
                self.t.heard_at = at.min(self.t.written_frames).max(self.t.heard_at);
                if seen != Seen::Dip {
                    self.t.strikes = 0;
                    // Only a moving count anchors the clock: a still one may be dead.
                    if self.t.heard_at > was || self.t.clock.is_none() {
                        self.t.clock_lag_ms = self.t.clock.map_or(0, |(t, at)| ((self.now_ms - t) - (self.t.heard_at.saturating_sub(at) as i64 * 1000 / rate)).max(0));
                        self.t.clock = Some((self.now_ms, self.t.heard_at));
                    }
                }
                if self.t.heard_at > was {
                    if let Some(m) = self.t.moved_ms {
                        self.quiet_ms = self.quiet_ms.max(self.now_ms - m);
                    }
                    self.t.moved_ms = Some(self.now_ms);
                    self.t.asks = 0;
                    self.t.asked_ms = None;
                    if seen == Seen::Fine {
                        self.t.resumed = false;
                    }
                }
            }
            Err(at) => {
                let ms = |f: u64| f as i64 * 1000 / rate;
                self.strike(format!("the {what} read {raw} frames, {} ms in, ahead of the clock's {} ms", ms(at), ms(most)));
            }
        }
        self.t.heard_at
    }

    /// Music the track can hold, µs, from its size and the bitrate so far.
    fn holds_us(&self) -> i64 {
        let Some((_, _, bytes)) = self.open else { return TRACK_US };
        if self.t.written_bytes == 0 || self.t.written_frames == 0 {
            return (bytes as i128 * 8_000_000 / GUESS_BPS as i128) as i64;
        }
        (bytes as i128 * self.t.written_frames as i128 / self.t.written_bytes as i128 * 1_000_000 / self.rate() as i128) as i64
    }

    /// The watchdog's slack: twice the longest standstill seen, within [`STUCK_SLACK_MS`]..[`STUCK_SLACK_MAX_MS`].
    fn slack_ms(&self) -> i64 {
        self.quiet_ms.saturating_mul(2).clamp(STUCK_SLACK_MS, STUCK_SLACK_MAX_MS)
    }

    /// When the watchdog fires (engine ms): the slack after [`DEAD_ASKS`] asks with no movement, else the
    /// music written past the count plus the slack after the platform last spoke.
    fn watch_at(&self, since: i64) -> i64 {
        let slack = self.slack_ms();
        let heard_from = self.t.asked_ms.map_or(since, |a| a.max(since));
        let stall = heard_from + self.in_track_us() / 1000 + slack;
        if self.t.asks >= DEAD_ASKS { stall.min(since + slack) } else { stall }
    }

    /// The watchdog (see the module docs). A stalled timestamp falls back to the play head; with neither
    /// moving, returns why the CPU takes over and whether the place follows the clock (the platform kept
    /// asking, so it played) rather than the last count (a real stall).
    fn stuck(&mut self) -> Option<(String, bool)> {
        if !self.playing || !self.started || self.t.placed.is_empty() || self.open.is_none() {
            return None;
        }
        let now = self.now_ms;
        let since = *self.t.moved_ms.get_or_insert(now);
        if self.in_track_us() <= END_SLACK_US {
            // Everything written was played.
            self.t.moved_ms = Some(now);
            return None;
        }
        if now < self.watch_at(since) {
            return None;
        }
        let still = now - since;
        let dead = self.t.asks >= DEAD_ASKS;
        let asked = match self.t.asks {
            0 => ", and the platform asked for nothing".to_string(),
            n => format!(", though the platform asked for more {n} times"),
        };
        let what = if self.t.by_stamp { "timestamp" } else { "play head" };
        let raw = self.t.raw.map_or("nothing".into(), |r| r.to_string());
        let why = format!("the {what} stood at {raw} for {still} ms while the track played, {} ms written past it{asked} (slack {} ms)", self.in_track_us() / 1000, self.slack_ms());
        if self.t.by_stamp && !self.t.head_dead {
            self.t.stamp_dead = true;
            self.note(format!("{why}: the play head is followed instead"));
            // The timestamp's clock anchor is discarded: bound the play head by the play start.
            self.t.clock = self.t.play_clock.or(self.t.clock);
            self.t.clock_lag_ms = 0;
            let was = self.t.heard_at;
            self.read_head();
            if self.t.heard_at > was {
                return None;
            }
        }
        self.t.stamp_dead = true;
        self.t.head_dead = true;
        if dead {
            // The platform played what it asked for: follow the clock, within what was written.
            let by_clock = self.t.heard_at + (still.max(0) as u128 * self.rate() as u128 / 1000) as u64;
            self.t.heard_at = by_clock.min(self.t.written_frames);
        }
        Some((why, dead))
    }

    /// A bad reading or a refused end of stream.
    fn strike(&mut self, why: String) {
        self.t.strikes += 1;
        let n = self.t.strikes;
        self.note_now_and_then(|| format!("{why} ({n} of {STRIKES})"));
        self.strike_why = why;
    }

    fn note(&mut self, what: String) {
        self.out.note(&what);
    }

    /// A rate-limited note ([`NOTE_GAP_MS`]); `what` is built only when kept.
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

    /// The song being played, its position (ms) and placement seq; drops the songs before it.
    pub(crate) fn heard(&mut self) -> Option<(usize, i64, u64)> {
        let at = self.t.heard_at;
        while self.t.placed.len() > 1 && self.t.placed[1].start <= at {
            self.t.placed.pop_front();
            let level = self.t.placed[0].level;
            self.level = level;
            self.volume();
        }
        let p = self.t.placed.front()?;
        let rate = self.rate() as i64;
        Some((p.index, p.from_ms + (at.saturating_sub(p.start) as i64) * 1000 / rate, p.seq))
    }

    /// The song being played, without reading the count.
    pub(crate) fn current(&self) -> Option<usize> {
        self.t.placed.front().map(|p| p.index).or(self.t.starting.as_ref().map(|s| s.0))
    }

    /// Written music not yet played, µs.
    pub(crate) fn in_track_us(&self) -> i64 {
        (self.t.written_frames.saturating_sub(self.t.heard_at) as i128 * 1_000_000 / self.rate() as i128) as i64
    }

    /// Everything written was played and nothing more comes: what follows.
    pub(crate) fn done(&mut self) -> Option<Tail> {
        let tail = self.t.tail?;
        if self.t.writing.is_some() || self.t.staged < self.stage.len() {
            return None;
        }
        let end = self.t.written_frames;
        let us = |us: i64| (us * self.rate() as i64 / 1_000_000) as u64;
        let by_head = self.t.heard_at + us(END_SLACK_US) >= end;
        // "Presented" also fires at every join, so it counts only near the end and for the last end of
        // stream with nothing written since.
        let by_word = !by_head && self.t.eos_at == Some(end) && self.t.heard_at + us(PRESENTED_NEAR_US) >= end && self.out.presented();
        if !by_head && !by_word {
            return None;
        }
        if std::mem::replace(&mut self.t.end_noted, true) {
            return Some(tail);
        }
        let ms = |f: u64| f as i64 * 1000 / self.rate() as i64;
        let (id, start) = self.t.placed.front().map_or((String::new(), 0), |p| (p.id.clone(), p.start));
        let raw = self.t.raw.map_or("nothing".into(), |r| r.to_string());
        let count = if self.t.by_stamp { "timestamp" } else { "play head" };
        let (steps, most) = self.t.jitter;
        let jitter = if steps > 0 { format!(", its {count} a moment back {steps} times (by {most} frames at most), held where it was") } else { String::new() };
        let what = format!(
            "{id} ended {}: the {count} read {raw}, {} ms heard of {} ms written for it, its end of stream {}{jitter}",
            if by_head { "by the play head" } else { "by the platform's word that it presented everything" },
            ms(self.t.heard_at.saturating_sub(start)),
            ms(end.saturating_sub(start)),
            if self.t.eos_at == Some(end) { "said" } else { "not said" },
        );
        self.note(what);
        Some(tail)
    }

    /// The next song after `i` that is not skipped; None after the sleep timer's song.
    fn next_of<Q: Queue>(&self, i: usize, queue: &Known<Q>) -> Option<usize> {
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

    /// One turn: place the starting song once open, top up the track, step the fade. `gain` gives each
    /// song's ReplayGain.
    pub(crate) fn turn<L: Library, Q: Queue>(&mut self, now_ms: i64, tracks: &mut Sources<L>, queue: &Known<Q>, gain: &mut dyn FnMut(&Playlist, usize) -> f32) -> Step {
        self.now_ms = now_ms;
        self.follow_fade(now_ms);
        if self.open.is_some() && self.out.torn_down() {
            return self.fallback();
        }
        if let Some(step) = self.begin(tracks, queue, gain) {
            return step;
        }
        // Asked for more: top up whatever the count says.
        let asked = self.open.is_some() && self.out.data_requested();
        if self.t.play_clock.is_none() && self.playing && self.started {
            self.t.play_clock = Some((now_ms, self.t.heard_at));
        }
        self.read_head();
        if asked {
            self.asked_for_more(now_ms);
        }
        if self.t.eos_due && self.playing {
            self.end_stream();
        }
        if self.t.strikes >= STRIKES || self.t.eos_refusals >= STRIKES {
            let why = std::mem::take(&mut self.strike_why);
            self.note(format!("offload given up, the CPU plays on: {why}"));
            let step = self.fallback();
            self.on_cpu = Some(OnCpu::Head(why));
            return step;
        }
        if let Some((why, by_clock)) = self.stuck() {
            let ms = self.heard().map_or(0, |h| h.1);
            let place = if by_clock { "where the clock puts the ear, the platform having played what it asked for" } else { "where the chip's count last put the ear" };
            self.note(format!("offload given up, the CPU plays on from {ms} ms, {place}: {why}"));
            let step = self.fallback();
            self.on_cpu = Some(OnCpu::Head(why));
            return step;
        }
        // Also runs when awaited bytes arrived (the loader woke the thread).
        if self.t.placed.is_empty() || (!asked && !self.t.waiting && !self.top_up_due()) {
            return Step::Fine;
        }
        match self.fill(asked, tracks, queue, gain) {
            Ok(()) => Step::Fine,
            Err(_) => self.fallback(),
        }
    }

    /// The platform asked for more at `now_ms`: it plays, and says when the track has room.
    fn asked_for_more(&mut self, now_ms: i64) {
        self.called_back = true;
        self.t.asks = self.t.asks.saturating_add(1);
        self.t.asked_ms = Some(now_ms);
        if self.playing && self.started {
            if let Some(last) = self.t.last_ask_ms.replace(now_ms) {
                self.ask_gap_ms = self.ask_gap_ms.max(now_ms - last);
            }
        }
    }

    /// Something is left to write.
    fn more(&self) -> bool {
        self.t.writing.is_some() || self.t.next.is_some() || self.t.staged < self.stage.len()
    }

    /// Whether to write without an ask: a full track only if the platform never asks (at
    /// [`Offload::low_us`]), otherwise below [`LOW_US`].
    fn top_up_due(&self) -> bool {
        if !self.more() {
            return false;
        }
        if self.t.full {
            !self.called_back && self.in_track_us() < self.low_us()
        } else {
            self.in_track_us() < LOW_US
        }
    }

    /// How long before a boundary the CPU is kept awake, ms: 1.5 times the longest gap between asks plus
    /// a second, within [`AWAKE_BEFORE_MS`]..[`AWAKE_BEFORE_MAX_MS`]. Asleep, the thread wakes only on an ask.
    fn awake_before_ms(&self) -> i64 {
        (self.ask_gap_ms * 3 / 2 + 1_000).clamp(AWAKE_BEFORE_MS, AWAKE_BEFORE_MAX_MS)
    }

    /// Whether nothing is due before the platform's next ask: playing and fed, nothing opening, fading
    /// or pending, and no boundary within [`Offload::awake_before_ms`].
    pub(crate) fn lets_cpu_sleep(&self) -> bool {
        if !self.playing || !self.started || self.t.placed.is_empty() || self.open.is_none() || self.t.starting.is_some() || self.t.waiting || self.fade.is_some() {
            return false;
        }
        if self.t.strikes > 0 || self.t.eos_due || self.t.head.lower.is_some() || self.t.stamp.lower.is_some() {
            return false;
        }
        // A platform that never asked is topped up on the engine's time.
        if self.more() && !self.called_back {
            return false;
        }
        let rate = self.rate() as i64;
        let ms = |frames: u64| frames as i64 * 1000 / rate;
        let near = self.awake_before_ms();
        if self.t.placed.get(1).is_some_and(|p| ms(p.start.saturating_sub(self.t.heard_at)) <= near) {
            return false;
        }
        if self.t.tail.is_some() && !self.more() && ms(self.t.written_frames.saturating_sub(self.t.heard_at)) <= near {
            return false;
        }
        true
    }

    /// Top-up mark without asks, µs: [`LOW_US`], or half a track too small for it.
    fn low_us(&self) -> i64 {
        if self.open.is_none() || self.t.written_bytes == 0 || self.t.written_frames == 0 {
            return LOW_US;
        }
        LOW_US.min(self.holds_us() / 2)
    }

    /// Hands the song to the CPU at the playback position.
    fn fallback(&mut self) -> Step {
        self.on_cpu = Some(OnCpu::Failed);
        self.note_left();
        let at = self.heard().map(|(i, ms, _)| (i, ms)).or(self.t.starting.as_ref().map(|s| (s.0, s.1)));
        self.release();
        match at {
            Some((index, ms)) => Step::ToPcm { index, ms, refused: true },
            None => Step::Fine,
        }
    }

    /// Places the starting song once open on a track for its format, or hands it to the CPU.
    fn begin<L: Library, Q: Queue>(&mut self, tracks: &mut Sources<L>, queue: &Known<Q>, gain: &mut dyn FnMut(&Playlist, usize) -> f32) -> Option<Step> {
        let (i, ms) = self.t.starting.as_ref().map(|s| (s.0, s.1))?;
        let ready = match &mut self.t.starting.as_mut().expect("checked").2 {
            Ok(r) => r.ready(),
            Err(_) => true,
        };
        if !ready {
            self.t.waiting = true;
            return Some(Step::Fine);
        }
        self.t.waiting = false;
        let (_, _, opened) = self.t.starting.take().expect("checked");
        let to_pcm = Some(Step::ToPcm { index: i, ms, refused: false });
        let Ok(r) = opened else {
            self.on_cpu = Some(OnCpu::Unread);
            return to_pcm;
        };
        let album = self.in_album(i, tracks, queue);
        let (id, level) = queue.read(|q| (q.ids()[i].clone(), gain(q, i)));
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
        // Started part way in: no delay to cut.
        let delay = if song.from_frame > 0 { 0 } else { song.delay };
        self.out.delay_padding(delay, song.padding);
        let from_ms = song.from_frame * 1000 / coded.rate.max(1) as i64;
        self.level = level;
        self.seq += 1;
        self.t.placed.push_back(Placed { index: i, id, start: 0, from_ms, level, seq: self.seq });
        let ogg = (coded.coding == Coding::Opus).then(|| Ogg::new(song.setup.as_deref()));
        self.t.writing = Some(Writing { r, frames: 0, ogg });
        self.volume();
        if self.fill(false, tracks, queue, gain).is_err() {
            return Some(self.fallback());
        }
        if let Some(asked) = self.granted.take() {
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
            self.t.clock = Some((self.now_ms, self.t.heard_at));
            self.t.clock_lag_ms = 0;
            self.t.play_clock = self.t.clock;
            if self.t.eos_due {
                self.end_stream();
            }
        }
        Some(Step::Fine)
    }

    /// Writes what fits: the rest of the song being written, then the songs that join it gaplessly. Err
    /// when the track refused a write. `asked`: the platform asked, so the next song is written even when
    /// the (lagging) count says the track is full.
    fn fill<L: Library, Q: Queue>(&mut self, asked: bool, tracks: &mut Sources<L>, queue: &Known<Q>, gain: &mut dyn FnMut(&Playlist, usize) -> f32) -> Result<(), i32> {
        self.t.waiting = false;
        loop {
            if self.in_track_us() >= TRACK_US {
                return Ok(());
            }
            if self.t.staged < self.stage.len() {
                if !self.write_staged()? {
                    return Ok(());
                }
                continue;
            }
            if let Some(w) = self.t.writing.as_mut() {
                if !w.r.ready() {
                    self.t.waiting = true;
                    return Ok(());
                }
                self.stage.clear();
                self.t.staged = 0;
                self.t.stage_frames = 0;
                while self.stage.len() < STAGE_BYTES && w.r.packet() {
                    let frames = w.r.packet_frames();
                    match w.ogg.as_mut() {
                        Some(ogg) => ogg.page(w.r.buffer(), &mut self.stage),
                        None => self.stage.extend_from_slice(w.r.buffer()),
                    }
                    self.t.stage_frames += frames;
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
                // Read to its end.
                if let Some((_, why)) = w.r.error() {
                    let id = self.t.placed.back().map(|p| p.id.clone()).unwrap_or_default();
                    let ms = w.frames as i64 * 1000 / self.rate() as i64;
                    self.note(format!("{id} was read to an early end at {ms} ms: {why}"));
                }
                self.t.writing = None;
                let last = self.t.placed.back().map(|p| p.index).expect("a song is placed");
                match self.next_of(last, queue) {
                    None => self.close(Tail::End),
                    Some(n) => {
                        let id = queue.read(|q| q.ids()[n].clone());
                        self.t.next = Some((n, tracks.open_packets(&id, 0, false)));
                    }
                }
                continue;
            }
            // The next song is written below LOW_US (a queue edit before then costs nothing), or at an ask
            // while the count lags (a DSP may hold a song's end until more comes).
            let lagging = asked && self.in_track_us() > self.holds_us() * 3 / 2;
            if self.t.next.is_none() || (self.in_track_us() >= LOW_US && !lagging) {
                return Ok(());
            }
            let (n, opened) = self.t.next.as_mut().expect("checked");
            let n = *n;
            let song = match opened {
                Ok(r) => {
                    if !r.ready() {
                        self.t.waiting = true;
                        return Ok(());
                    }
                    r.error().is_none().then(|| r.coded().cloned()).flatten()
                }
                Err(_) => None,
            };
            let (id, level) = queue.read(|q| (q.ids()[n].clone(), gain(q, n)));
            let joins = nori_player::gain::offload_allows(level) && self.open.is_some_and(|(c, gapless, _)| gapless && song.as_ref().is_some_and(|s| s.coded == c));
            let Some(song) = song.filter(|_| joins) else {
                // Another format or not offloadable: decided once the track plays out.
                self.t.next = None;
                self.close(Tail::Then(n));
                return Ok(());
            };
            if self.t.pending_eos {
                // Close the song before first (only accepted while playing).
                self.end_stream();
                if self.t.pending_eos {
                    return Ok(());
                }
            }
            let (_, opened) = self.t.next.take().expect("checked");
            let r = opened.expect("checked");
            self.out.delay_padding(song.delay, song.padding);
            let start = self.t.written_frames;
            self.seq += 1;
            let from_ms = song.from_frame * 1000 / song.coded.rate.max(1) as i64;
            self.t.placed.push_back(Placed { index: n, id, start, from_ms, level, seq: self.seq });
            let ogg = (song.coded.coding == Coding::Opus).then(|| Ogg::new(song.setup.as_deref()));
            self.t.writing = Some(Writing { r, frames: 0, ogg });
        }
    }

    /// Nothing more is written; `tail` follows once it played.
    fn close(&mut self, tail: Tail) {
        self.end_stream();
        self.t.tail = Some(tail);
    }

    /// Tells the platform the last packet ended its song, now if playing or once it plays (Android
    /// refuses it otherwise). A refusal while playing is a strike.
    fn end_stream(&mut self) {
        if !self.t.pending_eos {
            self.t.eos_due = false;
            return;
        }
        if !self.playing || !self.started || self.open.is_none() {
            self.t.eos_due = true;
            return;
        }
        if self.out.end_of_stream() {
            self.t.pending_eos = false;
            self.t.eos_due = false;
            self.t.eos_refusals = 0;
            self.t.eos_at = Some(self.t.written_frames);
        } else {
            self.t.eos_due = true;
            self.t.eos_refusals += 1;
            let why = "the platform would not take the end of stream while its track played";
            self.note(format!("{why} ({} of {STRIKES})", self.t.eos_refusals));
            self.strike_why = why.into();
        }
    }

    /// Writes the stage. False when the track is full.
    fn write_staged(&mut self) -> Result<bool, i32> {
        let left = self.stage.len() - self.t.staged;
        let frames = self.t.stage_frames;
        let taken = self.out.write(&self.stage[self.t.staged..], frames)?.min(left);
        self.t.full = taken < left;
        // Frames in proportion to bytes; exact once the whole stage is taken.
        let part = if taken == left { frames } else { (frames as u128 * taken as u128 / left as u128) as u64 };
        self.t.stage_frames -= part;
        self.t.written_frames += part;
        self.t.written_bytes += taken as u64;
        self.t.staged += taken;
        if taken > 0 {
            self.t.pending_eos = true;
        }
        Ok(taken == left)
    }

    /// A song was started after this turn looked at the path.
    pub(crate) fn unlooked(&self) -> bool {
        self.t.starting.is_some() && !self.t.waiting
    }

    /// The starting song or the next packet waits for bytes.
    pub(crate) fn waiting_for_bytes(&self) -> bool {
        self.t.starting.is_some() || self.t.waiting
    }

    /// How long until this path needs the thread, ms; None when nothing is due.
    pub(crate) fn wake_in(&self) -> Option<i64> {
        let mut d: Option<i64> = None;
        let mut at = |ms: i64| d = Some(d.map_or(ms, |x| x.min(ms)));
        if self.fade.is_some() {
            at(nori_player::transport::FADE_TICK_MS);
        }
        if self.t.starting.is_some() || self.t.waiting {
            // The loader wakes the thread; this is a fallback.
            at(1_000);
        }
        if !self.playing || self.t.placed.is_empty() {
            return d;
        }
        if self.t.strikes > 0 || self.t.head.lower.is_some() || self.t.stamp.lower.is_some() || self.t.eos_due {
            at(LOOK_AGAIN_MS);
        }
        let rate = self.rate() as i64;
        let ms = |frames: u64| frames as i64 * 1000 / rate;
        // The next song becoming audible.
        if let Some(p) = self.t.placed.get(1) {
            at(ms(p.start.saturating_sub(self.t.heard_at)) + 5);
        }
        if self.more() {
            if self.t.full && self.called_back {
                // The ask wakes the thread; this is a fallback.
                at((self.in_track_us() / 1000).max(BACKSTOP_MS));
            } else if self.t.full {
                // Full while it seemed low: the DSP buffers more than the bytes say. Look again within a
                // second (or half the low mark for a tiny track) so it never runs dry.
                let floor = (self.low_us() / 2000).clamp(1, 1_000);
                at(((self.in_track_us() - self.low_us()) / 1000).max(0) + floor);
            } else {
                at(((self.in_track_us() - LOW_US) / 1000).max(0) + 1);
            }
        } else if self.t.tail.is_some() {
            let end = ms(self.t.written_frames.saturating_sub(self.t.heard_at));
            at(if end > 0 { end + 5 } else { END_LOOK_MS });
        }
        // The watchdog.
        if let Some(since) = self.t.moved_ms.filter(|_| self.started && self.in_track_us() > END_SLACK_US) {
            at((self.watch_at(since) - self.now_ms).max(0) + 1);
        }
        d
    }

    /// Stops after the current song (the sleep timer), or not (`false`). True when a later song is
    /// already written: the caller restarts the track at the playback position and sets the stop again.
    pub(crate) fn pause_at_end<L: Library, Q: Queue>(&mut self, on: bool, tracks: &mut Sources<L>, queue: &Known<Q>) -> bool {
        let Some(c) = self.current() else { return false };
        self.stop_after = on.then_some(c);
        if on && self.t.placed.len() > 1 {
            return true;
        }
        if self.t.writing.is_some() {
            return false;
        }
        if on {
            self.t.next = None;
            self.close(Tail::End);
        } else if self.t.tail == Some(Tail::End) {
            // Cancelled before the end: write what follows after all.
            self.t.tail = None;
            match self.next_of(c, queue) {
                Some(n) => {
                    let id = queue.read(|q| q.ids()[n].clone());
                    self.t.next = Some((n, tracks.open_packets(&id, 0, false)));
                }
                None => self.close(Tail::End),
            }
        }
        false
    }

    /// The queue changed from the entries `old`: re-finds the written songs and re-picks the song after
    /// the last. True when a written song no longer follows: the caller restarts where the ear is.
    pub(crate) fn queue_changed<L: Library, Q: Queue>(&mut self, old: &[u64], tracks: &mut Sources<L>, queue: &Known<Q>) -> bool {
        let new: Vec<u64> = queue.read(|q| q.seqs().to_vec());
        let moved = |i: usize| old.get(i).and_then(|s| new.iter().position(|n| n == s));
        self.stop_after = self.stop_after.and_then(moved);
        for i in self.t.placed.iter_mut().map(|p| &mut p.index).chain(self.t.starting.as_mut().map(|s| &mut s.0)) {
            match moved(*i) {
                Some(k) => *i = k,
                None => return true,
            }
        }
        for k in 1..self.t.placed.len() {
            if self.next_of(self.t.placed[k - 1].index, queue) != Some(self.t.placed[k].index) {
                return true;
            }
        }
        if self.t.writing.is_some() {
            return false;
        }
        let Some(last) = self.t.placed.back().map(|p| p.index) else { return false };
        let after = self.next_of(last, queue);
        let was = self.t.next.as_ref().map(|(n, _)| *n).or(match self.t.tail {
            Some(Tail::Then(n)) => Some(n),
            _ => None,
        });
        // The same entry follows (its index may have moved).
        if was.map(|w| old.get(w)) == after.map(|a| new.get(a)) {
            if let (Some(n), Some(a)) = (self.t.next.as_mut(), after) {
                n.0 = a;
            }
            if let (Some(Tail::Then(n)), Some(a)) = (self.t.tail.as_mut(), after) {
                *n = a;
            }
            return false;
        }
        self.t.next = None;
        self.t.tail = None;
        match after {
            Some(n) => {
                let id = queue.read(|q| q.ids()[n].clone());
                self.t.next = Some((n, tracks.open_packets(&id, 0, false)));
            }
            None => self.close(Tail::End),
        }
        false
    }
}

impl Drop for Offload {
    fn drop(&mut self) {
        if self.open.take().is_some() {
            self.out.close();
        }
    }
}

/// Opus packets in Ogg pages, as media3's `OggOpusAudioPacketizer` does: `OpusHead` and an empty
/// comment header, then one page per packet, stamped with the samples decoded up to its end.
pub(crate) struct Ogg {
    head: Option<Vec<u8>>,
    sequence: u32,
    granule: u64,
}

const OGG_SERIAL: u32 = 0;

impl Ogg {
    pub(crate) fn new(opus_head: Option<&[u8]>) -> Ogg {
        let head = opus_head.filter(|h| h.starts_with(b"OpusHead")).map(<[u8]>::to_vec).unwrap_or_else(|| {
            // media3's default stereo header.
            let mut h = b"OpusHead".to_vec();
            h.extend_from_slice(&[1, 2, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, 0]);
            h
        });
        Ogg { head: Some(head), sequence: 0, granule: 0 }
    }

    /// Appends `packet` as a page to `out`, preceded by the header pages the first time.
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

/// Samples (48 kHz) an Opus packet decodes to, from its TOC byte (RFC 6716, 3.1).
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

/// Ogg's page CRC-32: polynomial 0x04c11db7, unreflected, initial 0.
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
    fn opus_packets() {
        let mut ogg = Ogg::new(Some(b"OpusHead\x01\x02\x38\x01\x80\xbb\0\0\0\0\0"));
        let mut out = Vec::new();
        // A 20 ms packet (config 1: SILK 20 ms), one frame.
        ogg.page(&[0x08, 1, 2, 3], &mut out);
        // Header, comment header, packet.
        let pages: Vec<usize> = out.windows(4).enumerate().filter(|(_, w)| *w == b"OggS").map(|(i, _)| i).collect();
        assert_eq!(pages.len(), 3);
        assert_eq!(out[pages[0] + 5], 0x02, "the first page begins the stream");
        let granule = u64::from_le_bytes(out[pages[2] + 6..pages[2] + 14].try_into().unwrap());
        assert_eq!(granule, 960, "20 ms at 48 kHz");
        // CRC computed with its field zeroed.
        for (k, &p) in pages.iter().enumerate() {
            let end = pages.get(k + 1).copied().unwrap_or(out.len());
            let mut page = out[p..end].to_vec();
            let said = u32::from_le_bytes(page[22..26].try_into().unwrap());
            page[22..26].fill(0);
            assert_eq!(ogg_crc(&page), said);
        }
        // 255 bytes or more take several lacing values.
        let mut out = Vec::new();
        ogg.page(&vec![0x08; 300], &mut out);
        assert_eq!(out[26], 2, "two lacing values");
        assert_eq!((out[27], out[28]), (255, 45));

        // Opus samples from toc.
        assert_eq!(opus_samples(&[0x08]), 960, "SILK 20 ms");
        assert_eq!(opus_samples(&[0xfc]), 960, "CELT 20 ms");
        assert_eq!(opus_samples(&[0xf9]), 1920, "two CELT 20 ms frames");
        assert_eq!(opus_samples(&[0xfb, 0x03]), 2880, "three frames, counted in the second byte");
    }

    /// 100 ms at 44.1 kHz, as the engine reads it.
    const JITTER: u64 = 4_410;

    #[test]
    fn head_readings() {
        let mut h = Head::default();
        let far = u64::MAX;
        assert_eq!(h.read(1_000, None, false, JITTER, far), Ok((1_000, Seen::Fine)));
        assert_eq!(h.read(10_990, None, false, JITTER, far), Ok((10_990, Seen::Fine)));
        // Restarted at the join at 11 000.
        assert_eq!(h.read(20, Some(11_000), false, JITTER, far), Ok((11_020, Seen::Joined)));
        assert_eq!(h.read(500, None, false, JITTER, far), Ok((11_500, Seen::Fine)));

        // Head dip away from join holds.
        let mut h = Head::default();
        let far = u64::MAX;
        assert_eq!(h.read(44_100, None, true, JITTER, far), Ok((44_100, Seen::Fine)));
        // One zero reading: held, and the count continues when the next is back.
        assert_eq!(h.read(0, None, true, JITTER, far), Ok((44_100, Seen::Dip)));
        assert_eq!(h.read(46_000, None, true, JITTER, far), Ok((46_000, Seen::Fine)));
        // A real restart (standby, end of stream): taken at the second low reading.
        assert_eq!(h.read(10, None, true, JITTER, far), Ok((46_000, Seen::Dip)));
        assert_eq!(h.read(900, None, true, JITTER, far), Ok((46_900, Seen::Restarted)));
        assert_eq!(h.read(1_900, None, true, JITTER, far), Ok((47_900, Seen::Fine)));

        // Head restart only where plausible.
        let mut h = Head::default();
        let far = u64::MAX;
        assert_eq!(h.read(441_000, None, false, JITTER, far), Ok((441_000, Seen::Fine)));
        // No end of stream or pause: low readings hold.
        assert_eq!(h.read(10, None, false, JITTER, far), Ok((441_000, Seen::Dip)));
        assert_eq!(h.read(900, None, false, JITTER, far), Ok((441_000, Seen::Dip)));
        // A drop to half way is never a restart.
        assert_eq!(h.read(300_000, None, true, JITTER, far), Ok((441_000, Seen::Dip)));
        assert_eq!(h.read(300_100, None, true, JITTER, far), Ok((441_000, Seen::Dip)));
        assert_eq!(h.read(441_500, None, true, JITTER, far), Ok((441_500, Seen::Fine)));

        // Head jitter holds count.
        let mut h = Head::default();
        let far = u64::MAX;
        // A Galaxy S22's timestamp as its track starts (once misread as restarts).
        assert_eq!(h.read(10, None, true, JITTER, far), Ok((10, Seen::Fine)));
        for (raw, back) in [(6, 4), (5, 5), (4, 6), (4, 6), (6, 4)] {
            assert_eq!(h.read(raw, None, true, JITTER, far), Ok((10, Seen::Jitter(back))));
        }
        assert_eq!(h.read(7_074, None, true, JITTER, far), Ok((7_074, Seen::Fine)));
        for raw in [7_066, 7_065, 7_067, 7_065, 7_066, 7_064] {
            assert_eq!(h.read(raw, None, true, JITTER, far).map(|r| r.0), Ok(7_074), "{raw}");
        }
        // Steps back up to 80 ms are never a join, even with one in reach.
        assert_eq!(h.read(3_021_762, None, true, JITTER, far), Ok((3_021_762, Seen::Fine)));
        assert_eq!(h.read(3_021_759, Some(3_022_000), true, JITTER, far), Ok((3_021_762, Seen::Jitter(3))));
        assert_eq!(h.read(3_018_234, Some(3_022_000), true, JITTER, far), Ok((3_021_762, Seen::Jitter(3_528))));
        assert_eq!(h.read(3_021_800, None, true, JITTER, far), Ok((3_021_800, Seen::Fine)));

        // Head ahead of clock rejected.
        let mut h = Head::default();
        assert_eq!(h.read(1_000, None, false, JITTER, 50_000), Ok((1_000, Seen::Fine)));
        assert_eq!(h.read(4_000_000, None, false, JITTER, 50_000), Err(4_000_000));
        assert_eq!(h.read(12_000, None, false, JITTER, 50_000), Ok((12_000, Seen::Fine)), "the count as it was");
        // A join the clock says cannot be reached is not one.
        assert_eq!(h.read(10, None, false, JITTER, 50_000), Ok((12_000, Seen::Dip)));
    }

}
