//! The platform-free player: walks the queue one song at a time, feeds the transition engine in
//! bursts, and outputs through [`Sink`] (media3's AudioSink with nori's processors) into a [`Track`].
//! `sim` and `nori-engine` both run this with their own [`Songs`], [`Track`], [`App`] and clock.

use crate::burst::{Burst, Fed, BUFFER_US};
use crate::engine::{Heard, Host, StreamFormat, StreamId, TransitionEngine};
use crate::heard::{HeardTracker, PlayerNow, Seen, StreamAt};
use crate::pcm::Format;
use crate::playlist::Playlist;
use crate::queue::{measure_ahead, ErrorRun, OnError, PlaybackError};
use crate::transitions::WindowSong;

pub use crate::sink::{blended, ChainSettings, Remake, Sink, Sound, Track, BLEND_US};

/// Start of the renderer's timeline, as in media3.
pub const BASE_OFFSET_US: i64 = 1_000_000_000_000;
/// The next song is read once the current one's end is this close.
pub const READ_AHEAD_US: i64 = 10_000_000;
/// Most buffers offered per turn.
const BUFFERS_PER_TURN: usize = 256;

/// One song opened for reading, as decoded interleaved buffers (16-bit, or float for high quality).
pub trait Reading {
    fn format(&self) -> Format;
    /// Length, µs, as far as known (exact once read to the end).
    fn duration_us(&self) -> i64;
    /// Open and the next buffer's bytes have arrived. Asked again next turn if not; nothing else is
    /// called before the first `true`.
    fn ready(&mut self) -> bool {
        true
    }
    /// Why the song cannot play on (failed to open, or its bytes stopped for good). Asked once ready
    /// and at the end.
    fn error(&self) -> Option<(PlaybackError, String)> {
        None
    }
    /// Decodes the next buffer; false at the end.
    fn fill(&mut self) -> bool;
    /// The buffer [`Reading::fill`] made.
    fn buffer(&self) -> &[u8];
    /// Song time of the buffer's start, µs.
    fn at_us(&self) -> i64;
    /// Bits per sample stored in the file (0 unknown).
    fn bits(&self) -> u32 {
        0
    }
    /// Decodes and drops everything before `ms`, before the first buffer is handed out. False if it
    /// cannot (the song is then reopened there).
    fn skip_to_ms(&mut self, _ms: i64) -> bool {
        false
    }
}

/// Opens the songs a queue names.
pub trait Songs {
    type Reading: Reading;
    /// Opens `id` from `from_ms`. An error is a song that will not play.
    fn open(&mut self, id: &str, from_ms: i64) -> Result<Self::Reading, String>;
    /// Length as tagged, album and track number, for the planner and seek bar.
    fn about(&self, id: &str) -> WindowSong;
    /// `id` plays next; a platform may prefetch it.
    fn upcoming(&mut self, _id: &str) {}
}

/// The app around the player: planner, analysis store, log.
pub trait App: Host {
    /// The clock for the next engine calls ([`Host::now_ms`] returns it).
    fn clock(&mut self, now_ms: i64);
    /// AutoMix is on (upcoming songs are measured).
    fn auto_mix(&self) -> bool;
    /// The planner's window: the previous song, then the current and following in play order.
    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool);
    /// Measures the unmeasured songs among `ids`.
    fn measure_ahead<S: Songs>(&mut self, _songs: &mut S, _ids: &[String]) {}
    /// Output moved to a device: its key and its bound sound, if any. `None` when the app does not
    /// track devices.
    fn output_changed(&mut self, _kind: crate::outputs::OutputKind, _name: &str) -> Option<(String, Option<Sound>)> {
        None
    }
    /// Songs were measured in the background since last asked: replan.
    fn measured(&mut self) -> bool {
        false
    }
    /// The output forbids touching samples, so the planner disables transitions.
    fn transitions_off(&mut self, _off: bool) {}
    /// What to do about a failed song when the app tracks the failure run itself; `None` uses the
    /// player's [`ErrorRun`].
    fn on_error(&mut self, _kind: PlaybackError, _has_next: bool) -> Option<OnError> {
        None
    }
    /// Audio is coming out: resets the failure run.
    fn playing(&mut self) {}
    /// ReplayGain for index `index` of `list`, the queue as the player holds it (capped by
    /// [`Player::gain_max`]), applied per song by [`TransitionEngine::set_gain`].
    fn gain(&mut self, _list: &Playlist, _index: usize) -> f32 {
        1.0
    }
    /// Sing: `song_id`'s vocal mask, once made.
    fn vocal_mask(&mut self, _song_id: &str) -> Option<std::sync::Arc<crate::sing::VocalMask>> {
        None
    }
    /// Vocal masks were made since last asked.
    fn masks_made(&mut self) -> bool {
        false
    }
    /// The sound changed (`what`: the chain, the gain or the vocal masks) from output frame `at.output`, made
    /// from input frame `at.input` (frames since the last flush).
    fn spliced(&mut self, what: &str, at: Splice) {
        self.log(&format!("the {what} changes from output frame {} (input frame {})", at.output, at.input));
    }
}

/// Where a sound change starts: frames since the last flush, of the chain's input and of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Splice {
    pub input: u64,
    pub output: u64,
}

/// Where the playlist is kept: the player's own, or the core's.
pub trait Queue {
    fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R;
    /// The player moved to `index` by itself.
    fn moved_to(&mut self, index: usize);
    fn set_repeat(&mut self, mode: u8);
    /// Arriving on index `index` of `list` would skip it (explicit song, skip setting on).
    fn skips(&self, _list: &Playlist, _index: usize) -> bool {
        false
    }
}

/// The queue as the player last took it ([`Player::queue_changed`]): another thread edits `live`, and
/// the indexes the player holds keep naming songs of the list it reads until it is told.
pub struct Known<Q> {
    pub live: Q,
    list: Playlist,
}

impl<Q: Queue> Known<Q> {
    fn new(live: Q) -> Self {
        let list = live.read(Playlist::clone);
        Known { live, list }
    }

    fn take_edits(&mut self) {
        self.list = self.live.read(Playlist::clone);
    }

    pub fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R {
        f(&self.list)
    }

    pub fn moved_to(&mut self, index: usize) {
        self.list.moved_to(index);
        self.live.moved_to(index);
    }

    fn set_repeat(&mut self, mode: u8) {
        self.list.set_repeat(mode);
        self.live.set_repeat(mode);
    }

    pub fn skips(&self, index: usize) -> bool {
        self.live.skips(&self.list, index)
    }
}

impl Queue for Playlist {
    fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R {
        f(self)
    }

    fn moved_to(&mut self, index: usize) {
        Playlist::moved_to(self, index);
    }

    fn set_repeat(&mut self, mode: u8) {
        Playlist::set_repeat(self, mode);
    }
}

/// A stream handed to the output: song index, start in renderer time, length, applied gain, and the
/// serial that tells it from another stream of the same song ([`StreamId`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Period {
    index: usize,
    offset_us: i64,
    duration_us: i64,
    gain: f32,
    serial: u64,
}

/// The song being read and its start in renderer time.
struct Reader<R> {
    index: usize,
    offset_us: i64,
    r: R,
    pos: usize,
    ended: bool,
    /// The last turn found the next buffer's bytes still missing.
    waiting: bool,
}

/// A song opened from `from_ms`, not ready yet.
struct Opening<R> {
    index: usize,
    from_ms: i64,
    offset_us: i64,
    r: R,
}

impl<R: Reading> Reader<R> {
    fn new(index: usize, offset_us: i64, r: R) -> Reader<R> {
        Reader { index, offset_us, r, pos: 0, ended: false, waiting: false }
    }

    fn fill(&mut self) -> bool {
        self.pos = 0;
        let got = self.r.fill();
        self.ended = !got;
        got
    }

    fn left(&self) -> bool {
        self.pos < self.r.buffer().len()
    }
}

/// The player: a queue, one song read at a time, and the transition engine in front of the output.
/// Every engine call goes through [`Fed`] with the player's clock.
pub struct Player<S: Songs, T: Track, A: App, Q: Queue> {
    pub now_ms: i64,
    pub engine: TransitionEngine,
    pub burst: Burst,
    pub sink: Sink<T>,
    pub app: A,
    pub queue: Known<Q>,
    pub tracks: S,
    pub tracker: HeardTracker,
    /// Measure upcoming songs whenever the queue moves (with AutoMix on).
    pub measure_on_move: bool,
    /// ReplayGain off (bit-perfect output). Call [`Player::gain_changed`] after changing it.
    pub gain_off: bool,
    /// Maximum gain: 1 unless float output with the limiter allows boosting (`nori_player::gain`).
    pub gain_max: f32,
    reading: Option<Reader<S::Reading>>,
    /// The next song last prefetched ([`Songs::upcoming`]).
    upcoming: Option<String>,
    /// A song to read after a jump or seek, still opening.
    opening: Option<Opening<S::Reading>>,
    /// The audible song opened again to make its ending anew ([`Player::replan_ending`]).
    remaking: Option<Opening<S::Reading>>,
    /// Where the song being read started; cleared once the clock passes it (audio is audible).
    heard_from: Option<i64>,
    /// The song after the one being read, opened when that one is read to its end.
    next: Option<(usize, Result<S::Reading, String>)>,
    periods: Vec<Period>,
    playing: bool,
    /// Renderer position: from a seek or start, then from the engine once the output has a clock.
    position_us: Option<i64>,
    current: Option<usize>,
    source_ended: bool,
    /// Song changes with their time; the platform drains them.
    pub changes: Vec<(i64, usize)>,
    /// Failed songs (id, reason); the platform drains them.
    pub failures: Vec<(String, String)>,
    pub errors: ErrorRun,
    pub skip_on_error: bool,
    /// A failure found while reading ahead (or mid-song), raised when playback reaches it.
    failed: Option<(usize, PlaybackError, String)>,
    /// Playback stopped at this failed song; nothing is read until a jump.
    stopped: Option<usize>,
    /// Stop at the end of this song ([`Player::pause_at_end`]).
    stop_after: Option<usize>,
    /// The last turn hit [`BUFFERS_PER_TURN`] with the output still taking.
    hungry: bool,
    /// Start of the stream the clock is in; a new stream of the same song is a repeat-one loop.
    heard_period: Option<i64>,
    /// The serial of the stream the clock is in, and the last serial given.
    on_serial: Option<u64>,
    serials: u64,
    /// Repeat-one loops heard.
    pub loops: u32,
    /// A song failed for lack of network and the offline bridge takes over.
    pub bridge: bool,
    /// Queue entries as last seen, to map indexes across edits.
    seqs: Vec<u64>,
}

impl<S: Songs, T: Track, A: App, Q: Queue> Player<S, T, A, Q> {
    /// A player over `queue`, idle, writing into `track` through the deep buffer.
    pub fn build(tracks: S, queue: Q, app: A, track: T) -> Self {
        let mut p = Player {
            now_ms: 1_000,
            engine: TransitionEngine::new(),
            burst: Burst::default(),
            sink: Sink::new(BUFFER_US, ChainSettings::default(), track),
            app,
            queue: Known::new(queue),
            tracks,
            tracker: HeardTracker::new(),
            measure_on_move: true,
            gain_off: false,
            gain_max: 1.0,
            reading: None,
            upcoming: None,
            opening: None,
            remaking: None,
            heard_from: None,
            next: None,
            periods: Vec::new(),
            playing: false,
            position_us: None,
            current: None,
            source_ended: false,
            changes: Vec::new(),
            failures: Vec::new(),
            errors: ErrorRun::new(),
            skip_on_error: true,
            failed: None,
            stopped: None,
            stop_after: None,
            hungry: false,
            heard_period: None,
            on_serial: None,
            serials: 0,
            loops: 0,
            bridge: false,
            seqs: Vec::new(),
        };
        p.engine.follow_rate = true;
        p.seqs = p.queue.read(|q| q.seqs().to_vec());
        p.sync_queue();
        p
    }

    /// The id at list index `i`.
    pub fn id_at(&self, i: usize) -> String {
        self.queue.read(|q| q.ids()[i].clone())
    }

    fn next_of(&self, i: usize) -> Option<usize> {
        if self.stop_after == Some(i) {
            return None;
        }
        self.queue.read(|q| q.next_of(i, q.repeat())).map(|n| self.playable(n))
    }

    /// Stops at the end of the current song (sleep timer), as at the end of the queue; [`Player::ended`]
    /// reports it. Cleared by `false` or the next jump.
    pub fn pause_at_end(&mut self, on: bool) {
        let Some(c) = self.current.filter(|_| on) else {
            self.stop_after = None;
            return;
        };
        // Already reading the next song: re-read the rest of this one so nothing follows it.
        if self.reading.as_ref().is_some_and(|r| r.index != c) {
            let at = self.position_ms();
            self.jump(c, at);
        }
        self.stop_after = Some(c);
    }

    /// The song [`Player::pause_at_end`] stops after.
    pub fn stopping_after(&self) -> Option<usize> {
        self.stop_after
    }

    /// `i`, or the first song after it that is not skipped on arrival.
    fn playable(&self, i: usize) -> usize {
        let mut at = i;
        for _ in 0..self.queue.read(|q| q.len()) {
            if !self.queue.skips(at) {
                return at;
            }
            match self.queue.read(|q| q.next_of(at, q.repeat())) {
                Some(n) if n != i => at = n,
                _ => return at,
            }
        }
        at
    }

    /// Calls into the engine with the output fed in bursts, on the player's clock.
    fn call<R>(&mut self, f: impl FnOnce(&mut TransitionEngine, &mut Fed<'_, Sink<T>>, &mut A) -> R) -> R {
        self.app.clock(self.now_ms);
        let mut fed = Fed::new(&mut self.sink, &mut self.burst, self.now_ms);
        f(&mut self.engine, &mut fed, &mut self.app)
    }

    fn configure(&mut self, i: usize, serial: u64, format: Format) {
        let s = StreamFormat { id: StreamId { song: self.id_at(i), serial }, format };
        self.call(|e, d, a| e.configure(d, a, s));
    }

    fn new_serial(&mut self) -> u64 {
        self.serials += 1;
        self.serials
    }

    /// The serial of song `i`'s latest stream, or a new one.
    fn serial_for(&mut self, i: usize) -> u64 {
        match self.periods.iter().rev().find(|p| p.index == i) {
            Some(p) => p.serial,
            None => self.new_serial(),
        }
    }

    /// The serial of song `i`'s latest stream.
    fn serial_at(&self, i: usize) -> Option<u64> {
        self.periods.iter().rev().find(|p| p.index == i).map(|p| p.serial)
    }

    /// Starts reading song `i` (opened as `r`) from `from_ms` at `offset_us`, after a flush: now if
    /// ready, else once it is. False when it failed at once.
    fn begin(&mut self, i: usize, from_ms: i64, offset_us: i64, mut r: S::Reading) -> bool {
        self.reading = None;
        self.next = None;
        self.opening = None;
        self.remaking = None;
        // A seek within the same song keeps its stream's identity.
        let serial = self.serial_for(i);
        self.periods = vec![Period { index: i, offset_us, duration_us: 0, gain: 1.0, serial }];
        self.on_serial = Some(serial);
        self.position_us = Some(offset_us + from_ms * 1000);
        self.source_ended = false;
        self.heard_period = None;
        self.place_masks();
        if r.ready() {
            return self.start_reading(i, from_ms, offset_us, r);
        }
        self.opening = Some(Opening { index: i, from_ms, offset_us, r });
        true
    }

    /// Hands Sing's masker the vocal masks of the songs on the timeline (none while Sing is off).
    fn place_masks(&mut self) {
        if self.sink.settings().sing.is_none() {
            return;
        }
        let placed = self.masks_placed();
        if let Some((input, output)) = self.sink.set_masks(&placed) {
            self.app.spliced("vocal masks", Splice { input, output });
            self.burst.restart();
            self.sink.fill();
        }
    }

    /// The vocal masks of the songs on the timeline, where each song is.
    fn masks_placed(&mut self) -> Vec<crate::sing::Placed> {
        let mut placed = Vec::new();
        for (k, p) in self.periods.iter().enumerate() {
            let id = self.queue.read(|q| q.ids().get(p.index).cloned());
            if let Some(mask) = id.and_then(|id| self.app.vocal_mask(&id)) {
                let to = self.periods.get(k + 1).map_or(i64::MAX, |n| n.offset_us);
                placed.push(crate::sing::Placed { at: p.offset_us..to, mask });
            }
        }
        placed
    }

    /// Starts reading the opening song once it is ready.
    fn opened(&mut self) {
        let Some(o) = self.opening.as_mut() else { return };
        if !o.r.ready() {
            return;
        }
        let o = self.opening.take().expect("checked");
        self.start_reading(o.index, o.from_ms, o.offset_us, o.r);
    }

    /// The plan out of the audible song is asked for again (settings, an analysis or the queue changed).
    /// If its ending was made otherwise and can still change, it is made again from where the old and
    /// new endings part. A mix already audible plays out as it began.
    pub fn replan_ending(&mut self) {
        self.engine.replan();
        if self.mixing() {
            return;
        }
        let Some((cur, _)) = self.ear() else { return };
        let Some(period) = self.periods.iter().rev().find(|p| p.index == cur).copied() else { return };
        let id = self.id_at(cur);
        // Read on gaplessly into a song that no longer follows.
        let astray = self.read_astray(cur);
        self.app.clock(self.now_ms);
        let plan = self.app.plan_for(&id);
        let made = match self.ending_made(cur, plan.as_ref().map(|p| p.out_start_us)) {
            Some(made) => made,
            None if astray => None,
            None => return,
        };
        if made == plan && !astray {
            return;
        }
        let starts = [&made, &plan].map(|p| p.as_ref().map(|p| p.out_start_us));
        let part_us = starts.into_iter().flatten().min().unwrap_or(period.duration_us).min(period.duration_us);
        let said = |p: &Option<crate::engine::Plan>| p.as_ref().map_or("gapless".to_string(), |p| format!("a mix from {} ms", p.out_start_us / 1000));
        let why = if astray { "another song follows it now".to_string() } else { format!("{} now, {} as it was made", said(&plan), said(&made)) };
        self.app.log(&format!("the ending of {id} is made again from {} ms: {why}", part_us / 1000));
        self.remake_from(cur, part_us / 1000);
    }

    /// Opens song `i` again at `ms`, to read on from there once it is open ([`Player::remade`]).
    fn remake_from(&mut self, i: usize, ms: i64) {
        let Some(offset_us) = self.periods.iter().rev().find(|p| p.index == i).map(|p| p.offset_us) else { return };
        match self.tracks.open(&self.id_at(i), ms) {
            Ok(r) => self.remaking = Some(Opening { index: i, from_ms: ms, offset_us, r }),
            Err(why) => self.app.log(&format!("{} would not open again ({why}): its ending plays as it was made", self.id_at(i))),
        }
        self.remade();
    }

    /// The song opened again is open: the output from there on is dropped, if the output can still
    /// replace it, and the song read on from there for the transition engine to make what follows.
    fn remade(&mut self) {
        let Some(o) = self.remaking.as_mut() else { return };
        if !o.r.ready() {
            return;
        }
        let o = self.remaking.take().expect("checked");
        let Some(period) = self.periods.iter().rev().find(|p| p.offset_us == o.offset_us).copied() else { return };
        let format = o.r.format();
        let frame = o.from_ms * format.rate as i64 / 1000;
        let pts = o.offset_us + frame * 1_000_000 / format.rate as i64;
        if o.r.error().is_some() || self.sink.format != Some(format) || !self.sink.cut_at(pts) {
            self.app.log(&format!("{} is not made again from {} ms: it plays as it was made", self.id_at(o.index), o.from_ms));
            return;
        }
        self.call(|e, _, a| e.flush(a));
        self.burst.restart();
        self.periods.retain(|p| p.offset_us <= o.offset_us);
        self.place_masks();
        self.opening = None;
        self.next = None;
        self.failed = None;
        self.source_ended = false;
        self.sink.track.source_bits(o.r.bits());
        self.reading = Some(Reader::new(o.index, o.offset_us, o.r));
        self.configure(o.index, period.serial, format);
        self.engine.set_output_stream_offset_us(o.offset_us);
        self.engine.set_gain(period.gain);
    }

    /// Starts reading ready song `i`; false when it reports an error.
    fn start_reading(&mut self, i: usize, from_ms: i64, offset_us: i64, r: S::Reading) -> bool {
        if let Some((kind, why)) = r.error() {
            self.fail(i, kind, why);
            return false;
        }
        let (format, duration_us) = (r.format(), r.duration_us());
        self.sink.track.source_bits(r.bits());
        self.reading = Some(Reader::new(i, offset_us, r));
        self.next = None;
        let gain = self.song_gain(i);
        let serial = self.serial_for(i);
        self.periods = vec![Period { index: i, offset_us, duration_us, gain, serial }];
        self.place_masks();
        self.position_us = Some(offset_us + from_ms * 1000);
        self.heard_from = self.position_us;
        self.source_ended = false;
        self.configure(i, serial, format);
        self.engine.set_output_stream_offset_us(offset_us);
        self.engine.set_gain(gain);
        true
    }

    /// Song `i`'s ReplayGain, or 1 when gain is off.
    fn song_gain(&mut self, i: usize) -> f32 {
        if self.gain_off {
            return 1.0;
        }
        let Player { queue, app, .. } = self;
        queue.read(|q| app.gain(q, i)).min(self.gain_max)
    }

    /// ReplayGain settings changed: the output is made again at the new levels from the first frame the
    /// track can still replace, and the song being read from its next buffer.
    pub fn gain_changed(&mut self) {
        let mut ranges = Vec::new();
        for k in 0..self.periods.len() {
            let p = self.periods[k];
            let gain = self.song_gain(p.index);
            if gain != p.gain && p.gain > 0.0 {
                let to = self.periods.get(k + 1).map_or(i64::MAX, |n| n.offset_us);
                ranges.push((p.offset_us..to, gain / p.gain));
                self.engine.rescale(p.offset_us, gain / p.gain);
            }
            self.periods[k].gain = gain;
        }
        if !ranges.is_empty() {
            if let Some((input, output)) = self.sink.rescale(&ranges) {
                self.app.spliced("gain", Splice { input, output });
            }
            self.burst.restart();
            self.sink.fill();
        }
        if let Some(i) = self.reading.as_ref().map(|r| r.index) {
            let gain = match self.periods.iter().rev().find(|p| p.index == i) {
                Some(p) => p.gain,
                None => self.song_gain(i),
            };
            self.engine.set_gain(gain);
        }
    }

    /// A timeline offset past everything handed out so far.
    fn fresh_offset(&self) -> i64 {
        self.periods.iter().map(|p| p.offset_us + p.duration_us).max().unwrap_or(BASE_OFFSET_US - 1_000_000) + 1_000_000
    }

    /// Plays queue index `i` from its start.
    #[cfg(any(test, feature = "synth"))]
    pub fn play_from(&mut self, i: usize) {
        self.jump(i, 0);
        self.resume();
    }

    /// Moves to list index `i` (or the first playable after it) at `from_ms`; play state unchanged.
    pub fn jump(&mut self, i: usize, from_ms: i64) {
        self.jump_opened(i, from_ms, None);
    }

    /// [`Player::jump`] reusing a reading opened ahead (id, reading, opened-at ms) if it is that song
    /// and can reach `from_ms` ([`taken_from`]); otherwise the song is reopened.
    pub fn jump_from(&mut self, i: usize, from_ms: i64, opened: (String, S::Reading, i64)) {
        self.jump_opened(i, from_ms, Some(opened));
    }

    fn jump_opened(&mut self, i: usize, from_ms: i64, opened: Option<(String, S::Reading, i64)>) {
        self.stopped = None;
        self.stop_after = None;
        let i = self.playable(i);
        let id = self.id_at(i);
        let r = match opened.filter(|o| o.0 == id).and_then(|(_, r, at)| taken_from(r, at, from_ms)) {
            Some((r, from)) => Ok((r, from)),
            None => self.tracks.open(&id, from_ms).map(|r| (r, from_ms)),
        };
        let (r, from_ms) = match r {
            Ok(r) => r,
            Err(why) => return self.fail(i, PlaybackError::Other, why),
        };
        let offset = self.fresh_offset();
        self.call(|e, _, a| e.flush(a));
        self.burst.restart();
        self.sink.flush();
        self.queue.moved_to(i);
        if self.begin(i, from_ms, offset, r) {
            self.set_current(i);
        }
    }

    pub fn resume(&mut self) {
        self.burst.restart();
        self.sink.play();
        self.playing = true;
    }

    pub fn pause(&mut self) {
        // The last turn may have been a burst ago.
        if self.playing {
            self.follow_clock();
        }
        self.burst.restart();
        self.sink.pause();
        self.playing = false;
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    /// Frees everything a long pause does not need and returns (queue index, ms) to [`Player::jump`]
    /// back to.
    pub fn release(&mut self) -> Option<(usize, i64)> {
        let ended = self.source_ended;
        if let Some(now) = self.call(|e, d, a| e.position_us(d, a, ended)) {
            self.position_us = Some(now);
        }
        let at = self.current.map(|i| (i, self.position_ms()));
        self.call(|e, _, a| e.reset(a));
        self.burst.restart();
        self.sink.reset();
        self.reading = None;
        self.opening = None;
        self.remaking = None;
        self.next = None;
        self.failed = None;
        self.source_ended = false;
        at
    }

    /// Next, as the button does it.
    pub fn next(&mut self) -> bool {
        match self.queue.read(Playlist::next) {
            Some(n) => {
                self.jump(n, 0);
                true
            }
            None => false,
        }
    }

    /// Previous, as the button does it.
    pub fn previous(&mut self) -> bool {
        match self.queue.read(Playlist::previous) {
            Some(n) => {
                self.jump(n, 0);
                true
            }
            None => false,
        }
    }

    /// Seeks in the current song: flushes and rereads on the same timeline (announcing the song again).
    pub fn seek(&mut self, ms: i64) {
        // The song the seek bar shows: in a mix, the outgoing one until the takeover.
        let Some(i) = self.bar().index.or(self.current) else { return };
        let offset = self.periods.iter().find(|p| p.index == i).map_or_else(|| self.fresh_offset(), |p| p.offset_us);
        let r = match self.tracks.open(&self.id_at(i), ms) {
            Ok(r) => r,
            Err(why) => return self.fail(i, PlaybackError::Other, why),
        };
        self.call(|e, _, a| e.flush(a));
        self.burst.restart();
        self.sink.flush();
        if self.begin(i, ms, offset, r) {
            self.set_current(i);
        }
    }

    /// New chain settings, heard from the first frame the output can still replace.
    pub fn set_chain(&mut self, settings: ChainSettings) {
        if *self.sink.settings() == settings {
            return;
        }
        // The masks are in place before the masker joins, so what it makes again has them.
        if self.sink.settings().sing.is_none() && settings.sing.is_some() {
            let placed = self.masks_placed();
            self.sink.set_masks(&placed);
        }
        let ear = self.sink.ear();
        if let Some((input, output)) = self.sink.change(settings) {
            self.app.spliced("chain", Splice { input, output });
            // In place; a device that drops what it holds says when its change is heard itself.
            if let Some((heard, rate)) = ear.filter(|&(heard, _)| output > heard) {
                self.app.log(&format!("the change is heard after {} ms", (output - heard) * 1000 / rate.max(1) as u64));
            }
        }
        self.burst.restart();
        self.sink.fill();
    }

    /// Speed and pitch as set.
    pub fn speed(&self) -> (f32, f32) {
        let s = self.sink.settings();
        (s.speed, s.pitch)
    }

    /// Has the app measure upcoming songs, then replans.
    pub fn measure_ahead(&mut self) {
        let n = measure_ahead(self.app.auto_mix());
        let ids: Vec<String> = self.queue.read(|q| q.upcoming().take(n).map(|i| q.ids()[i].clone()).collect());
        if ids.is_empty() {
            return;
        }
        self.app.measure_ahead(&mut self.tracks, &ids);
        self.engine.replan();
    }

    /// Treats song `i` as failed (skipped or stopped at, per the failure rules).
    pub fn give_up(&mut self, i: usize, why: String) {
        self.fail(i, PlaybackError::Other, why);
    }

    fn fail(&mut self, i: usize, kind: PlaybackError, why: String) {
        let id = self.id_at(i);
        let next = self.next_of(i);
        self.failures.push((id.clone(), why));
        let decided = match self.app.on_error(kind, next.is_some()) {
            Some(d) => d,
            None => self.errors.failed(kind, false, false, self.skip_on_error, next.is_some()),
        };
        match (decided, next) {
            (OnError::Skip, Some(n)) => {
                self.app.log(&format!("{id} will not play: skipped"));
                self.jump(n, 0);
            }
            _ => {
                self.app.log(&format!("{id} will not play: stopped"));
                self.bridge = decided == OnError::Bridge;
                self.stopped = Some(i);
                self.call(|e, _, a| e.reset(a));
                self.sink.flush();
                self.sink.pause();
                self.playing = false;
                self.reading = None;
                self.opening = None;
                self.remaking = None;
                self.next = None;
            }
        }
    }

    /// The queue was edited. Indexes the player holds follow their entries; a song opened ahead that is
    /// no longer next is dropped.
    pub fn queue_changed(&mut self) {
        self.queue.take_edits();
        let seqs = self.queue.read(|q| q.seqs().to_vec());
        let old = std::mem::replace(&mut self.seqs, seqs);
        let edited = old != self.seqs;
        // Emptied (another server's profile taken up): nothing is left to play or to point at.
        if self.seqs.is_empty() && !old.is_empty() {
            self.pause();
            self.release();
            self.current = None;
            self.periods.clear();
            self.stop_after = None;
            self.upcoming = None;
            self.sync_queue();
            self.engine.replan();
            return;
        }
        if !old.is_empty() && edited {
            let new = &self.seqs;
            let moved = |i: usize| old.get(i).and_then(|s| new.iter().position(|n| n == s));
            let at = |i: usize| moved(i).unwrap_or_else(|| i.min(new.len().saturating_sub(1)));
            self.current = self.current.map(at);
            if let Some(r) = self.reading.as_mut() {
                r.index = at(r.index);
            }
            for o in [self.opening.as_mut(), self.remaking.as_mut()].into_iter().flatten() {
                o.index = at(o.index);
            }
            for p in self.periods.iter_mut() {
                p.index = at(p.index);
            }
            self.stop_after = self.stop_after.map(at);
            let after = self.reading.as_ref().and_then(|r| self.next_of(r.index));
            // A pending failure only stands while its song is still the one read or the next (by id,
            // not by position).
            let reading = self.reading.as_ref().map(|r| r.index);
            self.failed = self.failed.take().and_then(|(i, kind, why)| {
                moved(i).filter(|k| Some(*k) == reading || Some(*k) == after).map(|k| (k, kind, why))
            });
            match self.next.as_mut() {
                Some(n) if moved(n.0).is_some_and(|i| Some(i) == after) => n.0 = after.expect("checked"),
                _ => self.next = None,
            }
        }
        self.sync_queue();
        // The next song may have changed.
        self.engine.replan();
        let Some(cur) = self.current else { return };
        // Prefetch and measure the new next song now, so its mix can be planned in time.
        let next = self.next_of(cur).map(|n| self.id_at(n));
        let other_next = next != self.upcoming;
        if other_next || edited {
            self.upcoming = next;
            if let Some(id) = &self.upcoming {
                self.tracks.upcoming(id);
            }
        }
        if (edited || other_next) && self.app.auto_mix() && self.measure_on_move {
            self.measure_ahead();
        }
    }

    /// The reader moved past `cur` into a song that no longer follows it (queue edited after the
    /// ending was made).
    fn read_astray(&self, cur: usize) -> bool {
        let Some(r) = self.reading.as_ref().map(|r| r.index).or(self.opening.as_ref().map(|o| o.index)) else { return false };
        r != cur && Some(r) != self.next_of(cur)
    }

    /// Sets the repeat mode (`playlist::REPEAT_*`) and replans.
    pub fn set_repeat(&mut self, mode: u8) {
        self.queue.set_repeat(mode);
        self.sync_queue();
        self.engine.replan();
    }

    /// Sends the planner its window (previous song, then eight in play order, as the core's
    /// `Session::window`).
    fn sync_queue(&mut self) {
        let current = self.current;
        let (window, shuffling) = self.queue.read(|q| {
            let repeat = q.repeat();
            let mut window = Vec::new();
            if let Some(c) = current.or(q.current()) {
                window.extend(q.previous_of(c, repeat));
                let mut at = Some(c);
                for _ in 0..8 {
                    let Some(i) = at else { break };
                    window.push(i);
                    at = q.next_of(i, repeat);
                }
            }
            let ids: Vec<String> = window.into_iter().map(|i| q.ids()[i].clone()).collect();
            (ids, q.shuffling())
        });
        let window = window.iter().map(|id| self.tracks.about(id)).collect();
        self.app.window(window, shuffling);
    }

    fn set_current(&mut self, i: usize) {
        if self.current == Some(i) {
            return;
        }
        self.current = Some(i);
        self.queue.moved_to(i);
        self.changes.push((self.now_ms, i));
        self.sync_queue();
        self.upcoming = self.next_of(i).map(|n| self.id_at(n));
        if let Some(id) = &self.upcoming {
            self.tracks.upcoming(id);
        }
        if self.app.auto_mix() && self.measure_on_move {
            self.measure_ahead();
        }
    }

    /// The song whose stream the output's clock has reached.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// The failed song playback stopped at, until the next jump.
    pub fn stopped_at(&self) -> Option<usize> {
        self.stopped
    }

    pub fn current_id(&self) -> Option<String> {
        self.current.map(|i| self.id_at(i))
    }

    /// Position in the current song, ms.
    pub fn position_ms(&self) -> i64 {
        let Some(pos) = self.position_us else { return 0 };
        let offset = self.current.and_then(|i| self.periods.iter().rev().find(|p| p.index == i)).map_or(0, |p| p.offset_us);
        (pos - offset) / 1000
    }

    /// Song time until the clock reaches the next stream already handed out.
    pub fn until_next_song_us(&self) -> Option<i64> {
        let pos = self.position_us?;
        self.periods.iter().map(|p| p.offset_us).filter(|&o| o > pos).min().map(|o| o - pos)
    }

    /// Time until what is heard changes within a mix or held ending, µs of song time: the takeover as
    /// the seek bar reckons it ([`crate::heard`]), or the transition engine's next change (with
    /// `song_only`, only a change of song).
    pub fn until_heard_changes_us(&self, song_only: bool) -> Option<i64> {
        let h = self.engine.heard();
        let takeover = match h.id {
            Some(_) => Some(h.until_us - h.us - (self.now_ms - h.at_ms) * 1000),
            None => h.from.map(|_| h.audible_us - self.position_ms() * 1000),
        };
        [self.engine.until_heard_changes_us(song_only), takeover.filter(|&u| u > 0)].into_iter().flatten().min()
    }

    /// The seek bar's song and position.
    pub fn bar(&mut self) -> Seen {
        let pos = self.position_ms();
        let now = PlayerNow { now_ms: self.now_ms, playing: self.playing, on: self.on_serial, position_ms: pos };
        let periods = &self.periods;
        let after = self.reading.as_ref().and_then(|r| self.next_of(r.index));
        let placed = |p: &Period| (p.index, if p.duration_us > 0 { p.duration_us / 1000 } else { i64::MAX });
        let find = |s: StreamAt| match s {
            StreamAt::Serial(s) => periods.iter().find(|p| p.serial == s).map(placed),
            // A stream not handed out yet is the song after the one being read.
            StreamAt::After(s) => periods.iter().filter(|p| p.serial > s).min_by_key(|p| p.serial).map(placed).or(after.map(|n| (n, i64::MAX))),
        };
        self.tracker.at(self.engine.heard(), now, &find)
    }

    /// What is audible now, per the engine.
    pub fn heard(&self) -> &Heard {
        self.engine.heard()
    }

    /// A mix is audible now.
    pub fn mixing(&self) -> bool {
        self.engine.heard().mixing
    }

    /// The audible song and position, ms, as of the last turn. During a held ending that is the
    /// previous song, although `current` has moved on.
    fn ear(&mut self) -> Option<(usize, i64)> {
        let current = self.current?;
        if self.engine.heard().id.is_some() {
            if let Some(i) = self.bar().index {
                let heard = self.engine.heard();
                let since = if self.playing { (self.now_ms - heard.at_ms).max(0) } else { 0 };
                let ms = (heard.us + since * 1000) / 1000;
                return Some((i, ms));
            }
        }
        Some((current, self.position_ms()))
    }

    /// How song `cur`'s ending was already made: `Some(plan)` once a hold began or its last buffer went
    /// out (`Some(None)`: gapless), also `Some(None)` while still reading it past `start_us`. `None`
    /// while a plan starting at `start_us` can still be taken up.
    fn ending_made(&self, cur: usize, start_us: Option<i64>) -> Option<Option<crate::engine::Plan>> {
        let reading = self.reading.as_ref().filter(|r| r.index == cur);
        if let Some(made) = self.serial_at(cur).and_then(|s| self.engine.made(s)) {
            if self.engine.holding() || reading.is_none() {
                return Some(made.cloned());
            }
        }
        let r = reading?;
        (start_us? < r.r.at_us()).then_some(None)
    }

    /// The last song has been read to its end and handed out.
    pub fn source_ended(&self) -> bool {
        self.source_ended
    }

    /// Everything has played.
    pub fn ended(&self) -> bool {
        self.source_ended && self.sink.drained()
    }

    /// The last turn hit its buffer budget with the output still taking: turn again at once.
    pub fn hungry(&self) -> bool {
        self.hungry
    }

    /// State summary for diagnostics.
    pub fn words(&self) -> String {
        let id = |i: usize| self.queue.read(|q| q.ids().get(i).cloned()).unwrap_or_else(|| "?".into());
        let mut w = format!("{} on {}", if self.playing { "playing" } else { "paused" }, self.current.map_or("nothing".into(), |i| format!("{i} ({})", id(i))));
        if self.position_us.is_some() {
            w.push_str(&format!(" at {} ms", self.position_ms()));
        }
        match &self.reading {
            Some(r) => {
                w.push_str(&format!("; reading {} ({}) at {} ms", r.index, id(r.index), r.r.at_us() / 1000));
                if r.ended {
                    w.push_str(", read to its end");
                }
                if r.waiting {
                    w.push_str(", waiting for its bytes");
                }
                if r.left() {
                    w.push_str(", a buffer in hand");
                }
            }
            None => w.push_str("; reading nothing"),
        }
        if let Some(o) = &self.opening {
            w.push_str(&format!("; opening {} ({}) from {} ms", o.index, id(o.index), o.from_ms));
        }
        match &self.next {
            Some((n, Ok(_))) => w.push_str(&format!("; next {n} ({}) opened", id(*n))),
            Some((n, Err(why))) => w.push_str(&format!("; next {n} ({}) would not open: {why}", id(*n))),
            None => {}
        }
        if let Some((n, _, why)) = &self.failed {
            w.push_str(&format!("; {n} ({}) failed: {why}", id(*n)));
        }
        if let Some(n) = self.stopped {
            w.push_str(&format!("; stopped at {n}"));
        }
        if let Some(n) = self.stop_after {
            w.push_str(&format!("; stopping after {n}"));
        }
        if self.source_ended {
            w.push_str("; the queue read to its end");
        }
        if self.hungry {
            w.push_str("; hungry");
        }
        w.push_str(&format!("; transition engine {}", self.engine.words()));
        w
    }

    /// The song being read or opening.
    pub fn reading_index(&self) -> Option<usize> {
        self.reading.as_ref().map(|r| r.index).or(self.opening.as_ref().map(|o| o.index))
    }

    /// The song being read is waiting for bytes or still opening.
    pub fn starved(&self) -> bool {
        self.opening.is_some() || self.reading.as_ref().is_some_and(|r| !r.left() && !r.ended && r.waiting)
    }

    /// Reading is blocked on bytes: [`Player::starved`], a song opened again for its ending, or the next song's first bytes.
    pub fn waiting_for_bytes(&self) -> bool {
        self.starved() || self.remaking.is_some() || (self.failed.is_none() && self.next.as_ref().is_some_and(|(_, n)| n.is_ok()) && self.reading.as_ref().is_some_and(|r| r.ended && r.waiting && !r.left()))
    }

    /// One render turn at `now_ms`: reads the clock and offers audio until the output refuses.
    pub fn turn(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
        if self.app.masks_made() {
            self.place_masks();
        }
        self.opened();
        self.remade();
        if !self.playing {
            return;
        }
        self.follow_clock();
        self.render();
        // A pending failure is raised once everything before it has played.
        if self.failed.is_some() && self.ended() {
            let (n, kind, why) = self.failed.take().expect("checked");
            self.fail(n, kind, why);
        }
    }

    /// Updates the position and current song from the output's clock.
    fn follow_clock(&mut self) {
        let ended = self.source_ended;
        if let Some(at) = self.call(|e, d, a| e.position_us(d, a, ended)) {
            self.position_us = Some(at);
            if let Some(p) = self.periods.iter().rev().find(|p| at >= p.offset_us).copied() {
                // A new stream of the same song: repeat one looped.
                if self.heard_period.is_some_and(|o| o != p.offset_us) && self.current == Some(p.index) {
                    self.loops += 1;
                }
                if self.heard_period != Some(p.offset_us) {
                    self.on_serial = Some(p.serial);
                }
                self.heard_period = Some(p.offset_us);
                self.set_current(p.index);
            }
            if self.heard_from.is_some_and(|from| at > from) {
                self.heard_from = None;
                self.errors.played();
                self.app.playing();
            }
        }
    }

    fn render(&mut self) {
        self.hungry = false;
        // What a splice left to run again goes first.
        if !self.sink.fill() {
            return;
        }
        for k in 0..BUFFERS_PER_TURN {
            if !self.ensure_buffer() {
                break;
            }
            self.hungry = k + 1 == BUFFERS_PER_TURN;
            let Player { engine, sink, burst, app, reading, now_ms, .. } = self;
            let r = reading.as_mut().expect("a buffer is ready");
            app.clock(*now_ms);
            let mut fed = Fed::new(sink, burst, *now_ms);
            let pts = r.offset_us + r.r.at_us();
            let (taken, used) = engine.handle_buffer(&mut fed, app, &r.r.buffer()[r.pos..], pts);
            r.pos += used;
            if !taken {
                self.hungry = false;
                return;
            }
        }
        if self.reading.as_ref().is_some_and(|r| r.ended) && (self.at_queue_end() || self.failed.is_some()) && !self.source_ended {
            if self.call(|e, d, a| e.play_to_end_of_stream(d, a)) {
                self.source_ended = true;
                self.sink.end_of_stream();
                self.sink.source_ended = true;
            }
        } else if self.source_ended {
            self.call(|e, d, _| e.queue_empty(d));
            self.sink.fill();
        }
    }

    fn at_queue_end(&self) -> bool {
        self.reading.as_ref().is_some_and(|r| self.next_of(r.index).is_none())
    }

    /// Ensures a buffer is in hand, moving on to the next song once this one is read and its end is
    /// within [`READ_AHEAD_US`].
    fn ensure_buffer(&mut self) -> bool {
        loop {
            let Some(r) = self.reading.as_mut() else { return false };
            if r.left() {
                return true;
            }
            if !r.ended {
                r.waiting = !r.r.ready();
                if r.waiting {
                    return false;
                }
                if r.fill() {
                    continue;
                }
                // Bytes stopped for good: fail once what was read has played.
                if let Some((kind, why)) = r.r.error() {
                    if self.failed.is_none() {
                        self.failed = Some((r.index, kind, why));
                    }
                }
            }
            let (i, end) = (r.index, r.offset_us + r.r.duration_us());
            if self.failed.is_some() {
                return false;
            }
            let Some(n) = self.next_of(i) else { return false };
            // Open the next song now, so failures show early and fetching gets the most time.
            if self.next.as_ref().is_none_or(|(at, _)| *at != n) {
                let opened = self.tracks.open(&self.id_at(n), 0);
                self.next = Some((n, opened));
            }
            let failed = match self.next.as_mut() {
                Some((_, Err(why))) => Some((PlaybackError::Other, why.clone())),
                Some((_, Ok(r))) => r.ready().then(|| r.error()).flatten(),
                None => None,
            };
            if let Some((kind, why)) = failed {
                self.failed = Some((n, kind, why));
                self.next = None;
                return false;
            }
            if self.position_us.is_none_or(|pos| pos < end - READ_AHEAD_US) {
                return false;
            }
            if self.next.as_mut().is_some_and(|(_, r)| r.as_mut().is_ok_and(|r| !r.ready())) {
                if let Some(r) = self.reading.as_mut() {
                    r.waiting = true;
                }
                return false;
            }
            let Some((_, Ok(next))) = self.next.take() else { unreachable!("an opened song is waiting") };
            let (format, duration_us) = (next.format(), next.duration_us());
            self.sink.track.source_bits(next.bits());
            let gain = self.song_gain(n);
            let serial = self.new_serial();
            self.configure(n, serial, format);
            self.call(|e, d, a| e.handle_discontinuity(d, a));
            self.engine.set_output_stream_offset_us(end);
            self.engine.set_gain(gain);
            self.reading = Some(Reader::new(n, end, next));
            self.periods.push(Period { index: n, offset_us: end, duration_us, gain, serial });
            self.place_masks();
        }
    }
}

/// How far before a pre-opened reading's start the audible position may be for it to be reused (that
/// much audio is skipped).
const OPENED_EARLY_MS: i64 = 40;

/// Reuses reading `r` opened at `at` ms for playback from `from_ms`: read from `at` if `from_ms` is at
/// most [`OPENED_EARLY_MS`] before it, or skipped forward to `from_ms`. `None` if unusable.
fn taken_from<R: Reading>(mut r: R, at: i64, from_ms: i64) -> Option<(R, i64)> {
    if from_ms < at - OPENED_EARLY_MS {
        return None;
    }
    if from_ms <= at {
        return Some((r, at));
    }
    r.skip_to_ms(from_ms).then_some((r, from_ms))
}
