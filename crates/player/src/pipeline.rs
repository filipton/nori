//! The platform-free player: walks the queue one song at a time, feeds the transition engine in
//! bursts, and outputs through [`Sink`] (media3's AudioSink with nori's processors) into a [`Track`].
//! `sim` and `nori-engine` both run this with their own [`Songs`], [`Track`], [`App`] and clock.

use std::collections::VecDeque;

use crate::burst::{Burst, Fed, BUFFER_US};
use crate::dsp::{Band, Effects, Equalizer};
use crate::automix::analysis::Analyzer;
use crate::engine::{Downstream, Heard, Host, Plan, StreamFormat, TransitionEngine};
use crate::heard::{HeardTracker, PlayerNow, Seen};
use crate::pcm::{Encoding, Format};
use crate::playlist::Playlist;
use crate::queue::{measure_ahead, ErrorRun, OnError, PlaybackError};
use crate::silence::SilenceSkipper;
use crate::sound::sound_on;
use crate::speed::{speed_active, SpeedPitch};
use crate::transitions::WindowSong;
use crate::transport::{rebuild, Chain, ChainAct, ChainChange, Rebuild};

/// Start of the renderer's timeline, as in media3.
pub const BASE_OFFSET_US: i64 = 1_000_000_000_000;
/// The next song is read once the current one's end is this close.
pub const READ_AHEAD_US: i64 = 10_000_000;
/// Output buffer depth while the equalizer screen is open.
pub const SHALLOW_US: i64 = 500_000;
/// A buffer this far from its expected timestamp resyncs the clock (media3).
const PTS_TOLERANCE_US: i64 = 200_000;
const LIMITER_RELEASE_MS: f64 = 120.0;
const LIMITER_LOOKAHEAD_MS: f64 = 5.0;
/// Most buffers offered per turn.
const BUFFERS_PER_TURN: usize = 256;
/// Pace changes tracked in flight (a post-mix ramp changes pace every buffer).
const PACES: usize = 512;

/// Sound settings for the chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    /// Parametric bands; empty is off.
    pub bands: Vec<Band>,
    /// Graphic equalizer sliders (dB, one per band of a `graphic::LAYOUTS` layout), used instead of
    /// `bands`; empty is off.
    pub graphic: Vec<f64>,
    pub effects: Effects,
    pub preamp_db: f64,
    pub crossfeed_db: f64,
    /// Crossfeed cutoff, Hz.
    pub crossfeed_hz: f64,
    pub balance: f64,
    pub mono: bool,
    pub limiter: bool,
    pub threshold_db: f64,
}

impl Default for Sound {
    fn default() -> Self {
        Sound {
            bands: Vec::new(),
            graphic: Vec::new(),
            effects: Effects::default(),
            preamp_db: 0.0,
            crossfeed_db: 0.0,
            crossfeed_hz: crate::dsp::CROSSFEED_DEFAULT_HZ,
            balance: 0.0,
            mono: false,
            limiter: false,
            threshold_db: -1.0,
        }
    }
}

impl Sound {
    /// Whether anything here touches the samples.
    pub fn on(&self) -> bool {
        let eq = !self.bands.is_empty() || !self.graphic.is_empty() || self.preamp_db != 0.0;
        sound_on(eq, self.crossfeed_db as f32, self.balance as f32, self.mono, self.limiter, self.effects.on())
    }

    /// Configures `eq`. Anything that boosts the level also enables the limiter.
    pub fn apply(&self, eq: &mut Equalizer) {
        eq.set_crossfeed_cut(self.crossfeed_hz);
        if self.graphic.is_empty() {
            eq.configure(&self.bands, self.preamp_db, self.crossfeed_db);
        } else {
            eq.configure_graphic(&self.graphic, self.preamp_db, self.crossfeed_db);
        }
        eq.configure_effects(&self.effects);
        let lookahead = if self.limiter || self.effects.guard() { LIMITER_LOOKAHEAD_MS } else { 0.0 };
        eq.configure_output(self.balance, self.mono, self.threshold_db, LIMITER_RELEASE_MS, lookahead);
    }
}

/// The device buffer the sink writes into (an AudioTrack, a desktop ring, a simulated track). Data is
/// in the sink's format; the track converts if its device needs another.
pub trait Track {
    /// The sink's format from now on.
    fn open(&mut self, format: Format);
    /// Bytes written and not played yet.
    fn queued_bytes(&self) -> usize;
    /// Takes `data` whole (the sink never exceeds the room), covering `media` song frames (speed and
    /// silence skipping make them differ).
    fn write(&mut self, data: &[u8], media: f64);
    /// Song frames played since the last flush.
    fn played_media(&mut self) -> f64;
    /// Nothing written is left to play.
    fn is_empty(&self) -> bool;
    /// Drops everything written; the playhead restarts at 0.
    fn flush(&mut self);
    fn play(&mut self);
    fn pause(&mut self);
    /// Scales unplayed audio between `from` and `to` song frames past the last flush by `ratio`
    /// (ReplayGain settings changed). Optional for tracks with short buffers.
    fn rescale(&mut self, _from: f64, _to: f64, _ratio: f32) {}
    /// Bits per sample of the next configured stream's file (0 unknown).
    fn source_bits(&mut self, _bits: u32) {}
    /// Whether the track must reopen for `format` (and the last bits), after everything written has
    /// played: for tracks that play each song in its own format (bit-perfect).
    fn must_reopen(&mut self, _format: Format) -> bool {
        false
    }
    /// The sink's buffer depth from now on, told on (re)build before the flush; a track with its own
    /// buffer behind this one can follow it.
    fn depth(&mut self, _capacity_us: i64) {}
    /// Whether [`Track::depth`] applies in place without rebuilding ([`Player::set_tuning`]).
    fn resizes(&self) -> bool {
        false
    }
}

/// media3's AudioSink with nori's processors, over a [`Track`]. Its clock is media3's: the first
/// buffer's timestamp after a flush, shifted when a buffer arrives more than [`PTS_TOLERANCE_US`] off
/// (or after a discontinuity), plus the song time the track has played. The transition engine relies
/// on this: a mix is stamped in the next song's time and the clock jumps when it is offered.
///
/// Allocation-free once its buffers have grown.
pub struct Sink<T: Track> {
    pub format: Option<Format>,
    /// Every config token received, in order.
    pub configs: Vec<u32>,
    /// Format changes after the first open.
    pub rebuilds: usize,
    /// Buffer depth.
    pub capacity_us: i64,
    /// Whether the equalizer processor is in the chain.
    pub dsp: bool,
    eq: Option<Equalizer>,
    sound: Sound,
    sound_dirty: bool,
    skip_silence: bool,
    silence: Option<SilenceSkipper>,
    speed_pitch: (f32, f32),
    speed: Option<SpeedPitch>,
    start_media_us: i64,
    needs_init: bool,
    needs_sync: bool,
    /// Song frames submitted since the clock's reference ([`Downstream::media_pace`] weighted).
    submitted_frames: f64,
    /// Current song frames per frame.
    pace: f64,
    /// (submitted song frame, pace) where each pace began, for [`Sink::pace_heard`].
    paces: VecDeque<(f64, f64)>,
    /// Buffers more than [`PTS_TOLERANCE_US`] off (each a stutter on a phone).
    pub timestamp_jumps: usize,
    /// (address, length) of a partly taken buffer's rest, which must be offered next (media3 throws otherwise).
    owed: Option<(usize, usize)>,
    /// Format to reopen the track with once it has drained.
    reopen: Option<Format>,
    /// Processed audio waiting for room (from `pending_pos`), its song time, and song time not yet
    /// attached to output.
    pending: Vec<u8>,
    pending_pos: usize,
    pending_media: f64,
    carry: f64,
    samples_in: Vec<i16>,
    samples_out: Vec<i16>,
    floats_in: Vec<f32>,
    floats_out: Vec<f32>,
    stage: Vec<u8>,
    stage2: Vec<u8>,
    /// The source ended: running dry is the end, not a gap.
    pub source_ended: bool,
    /// Largest limiter reduction seen, dB.
    pub gain_reduction_db: f32,
    /// Limiter reduction on the last buffer, dB.
    pub meter_db: f32,
    pub track: T,
}

impl<T: Track> Sink<T> {
    pub fn new(capacity_us: i64, dsp: bool, sound: Sound, track: T) -> Sink<T> {
        Sink {
            format: None,
            configs: Vec::new(),
            rebuilds: 0,
            capacity_us,
            dsp,
            eq: None,
            sound,
            sound_dirty: true,
            skip_silence: false,
            silence: None,
            speed_pitch: (1.0, 1.0),
            speed: None,
            start_media_us: 0,
            needs_init: true,
            needs_sync: false,
            submitted_frames: 0.0,
            pace: 1.0,
            paces: VecDeque::with_capacity(PACES),
            timestamp_jumps: 0,
            owed: None,
            reopen: None,
            pending: Vec::new(),
            pending_pos: 0,
            pending_media: 0.0,
            carry: 0.0,
            samples_in: Vec::new(),
            samples_out: Vec::new(),
            floats_in: Vec::new(),
            floats_out: Vec::new(),
            stage: Vec::new(),
            stage2: Vec::new(),
            source_ended: false,
            gain_reduction_db: 0.0,
            meter_db: 0.0,
            track,
        }
    }

    /// Rebuilds over the same track (as stop + prepare): new chain, everything in flight dropped,
    /// stage settings kept.
    pub fn rebuild(&mut self, capacity_us: i64, dsp: bool, sound: Sound) {
        self.format = None;
        self.configs.clear();
        self.rebuilds = 0;
        self.capacity_us = capacity_us;
        self.dsp = dsp;
        self.eq = None;
        self.sound = sound;
        self.sound_dirty = true;
        self.silence = None;
        self.speed = None;
        self.start_media_us = 0;
        self.needs_init = true;
        self.needs_sync = false;
        self.submitted_frames = 0.0;
        self.paces.clear();
        self.timestamp_jumps = 0;
        self.owed = None;
        self.reopen = None;
        self.pending.clear();
        self.pending_pos = 0;
        self.pending_media = 0.0;
        self.carry = 0.0;
        self.source_ended = false;
        self.gain_reduction_db = 0.0;
        self.meter_db = 0.0;
        self.track.depth(capacity_us);
        self.track.flush();
    }

    /// Song frames per frame at the playhead ([`Downstream::media_pace`]).
    pub fn pace_heard(&mut self) -> f64 {
        if self.paces.len() < 2 {
            return self.paces.front().map_or(1.0, |p| p.1);
        }
        let played = self.track.played_media();
        while self.paces.len() >= 2 && self.paces[1].0 <= played {
            self.paces.pop_front();
        }
        self.paces[0].1
    }

    fn note_pace(&mut self) {
        if self.paces.back().is_some_and(|p| p.1 == self.pace) {
            return;
        }
        if self.paces.len() == PACES {
            self.paces.pop_front();
        }
        self.paces.push_back((self.submitted_frames, self.pace));
    }

    /// Song frames since the last flush at clock time `pts_us` (as [`Track::played_media`] counts);
    /// `None` before the first buffer.
    pub fn media_frames(&self, pts_us: i64) -> Option<f64> {
        let f = self.format.filter(|_| !self.needs_init)?;
        Some((pts_us - self.start_media_us) as f64 * f.rate as f64 / 1_000_000.0)
    }

    pub fn queued_us(&self) -> i64 {
        self.format.map_or(0, |f| f.us(self.track.queued_bytes()))
    }

    /// Track buffer size, bytes.
    #[cfg(any(test, feature = "synth"))]
    pub fn buffer_bytes(&self) -> usize {
        self.format.map_or(0, |f| f.bytes(self.capacity_us))
    }

    fn build_processors(&mut self) {
        let Some(f) = self.format else { return };
        self.eq = self.dsp.then(|| Equalizer::new(f.rate, f.channels));
        self.sound_dirty = true;
        self.build_stages();
    }

    fn build_stages(&mut self) {
        let Some(f) = self.format else { return };
        self.silence = self.skip_silence.then(|| SilenceSkipper::new(f.rate, f.channels, f.encoding == Encoding::Float));
        self.speed = speed_active(self.speed_pitch.0, self.speed_pitch.1).then(|| {
            let mut s = SpeedPitch::new(f.rate, f.channels, f.encoding);
            s.set(self.speed_pitch.0, self.speed_pitch.1);
            s.flush();
            s
        });
    }

    /// Whether the equalizer processor is in the path.
    pub fn chain_in(&self) -> bool {
        self.eq.is_some()
    }

    /// The compressor's largest gain reduction in the last buffer, dB; 0 without one.
    pub fn compression_db(&self) -> f32 {
        self.eq.as_ref().map_or(0.0, |e| e.compression_db())
    }

    /// The silence skipper's format while it is active.
    pub fn skipping_silence(&self) -> Option<Format> {
        self.format.filter(|_| self.silence.is_some())
    }

    /// Applied on the next buffer.
    pub fn set_sound(&mut self, sound: Sound) {
        self.sound = sound;
        self.sound_dirty = true;
    }

    /// Drains the speed and silence stages, then rebuilds them with new settings (as media3 does).
    pub fn set_stages(&mut self, speed: f32, pitch: f32, skip_silence: bool) {
        self.drain_stages();
        self.speed_pitch = (speed, pitch);
        self.skip_silence = skip_silence;
        self.build_stages();
    }

    /// Flushes what the stages hold into the track.
    fn drain_stages(&mut self) {
        let mut out = std::mem::take(&mut self.stage);
        out.clear();
        if let Some(s) = self.silence.as_mut() {
            s.end_of_stream(&mut out);
        }
        if let Some(sp) = self.speed.as_mut() {
            let mut sped = std::mem::take(&mut self.stage2);
            sped.clear();
            if !out.is_empty() {
                sp.process(&out, &mut sped);
            }
            sp.end_of_stream(&mut sped);
            std::mem::swap(&mut out, &mut sped);
            self.stage2 = sped;
        }
        if !out.is_empty() {
            self.push_pending(&out);
        }
        self.stage = out;
        self.write_pending();
    }

    fn pending_left(&self) -> bool {
        self.pending_pos < self.pending.len()
    }

    fn push_pending(&mut self, data: &[u8]) {
        let media = std::mem::take(&mut self.carry);
        if self.pending_left() {
            self.pending.drain(..self.pending_pos);
            self.pending_pos = 0;
            self.pending.extend_from_slice(data);
            self.pending_media += media;
        } else {
            self.pending.clear();
            self.pending_pos = 0;
            self.pending.extend_from_slice(data);
            self.pending_media = media;
        }
    }

    fn room_bytes(&self) -> usize {
        let Some(f) = self.format else { return 0 };
        f.bytes(self.capacity_us).saturating_sub(self.track.queued_bytes()) / f.frame_bytes() * f.frame_bytes()
    }

    /// Writes pending audio as far as there is room; true when none is left.
    fn write_pending(&mut self) -> bool {
        if !self.pending_left() {
            return true;
        }
        let room = self.room_bytes();
        let left = self.pending.len() - self.pending_pos;
        let n = room.min(left);
        if n > 0 {
            let media = self.pending_media * n as f64 / left as f64;
            self.pending_media -= media;
            self.track.write(&self.pending[self.pending_pos..self.pending_pos + n], media);
            self.pending_pos += n;
        }
        let done = !self.pending_left();
        if done {
            self.pending.clear();
            self.pending_pos = 0;
        }
        done
    }

    /// Runs `input` through equalizer, silence skipping and speed (media3's order) into pending output.
    fn process(&mut self, input: &[u8], media: f64) {
        self.carry += media;
        let mut data = std::mem::take(&mut self.stage);
        data.clear();
        let float = self.format.is_some_and(|f| f.encoding == Encoding::Float);
        match self.eq.as_mut().filter(|e| !e.is_identity()) {
            Some(eq) if float => {
                self.floats_in.clear();
                self.floats_in.extend(input.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
                self.floats_out.resize(self.floats_in.len(), 0.0);
                eq.process_f32(&self.floats_in, &mut self.floats_out);
                self.meter_db = eq.gain_reduction_db();
                self.gain_reduction_db = self.gain_reduction_db.max(self.meter_db);
                data.extend(self.floats_out.iter().flat_map(|v| v.to_le_bytes()));
            }
            Some(eq) => {
                self.samples_in.clear();
                self.samples_in.extend(input.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])));
                self.samples_out.resize(self.samples_in.len(), 0);
                eq.process_i16(&self.samples_in, &mut self.samples_out);
                self.meter_db = eq.gain_reduction_db();
                self.gain_reduction_db = self.gain_reduction_db.max(self.meter_db);
                data.extend(self.samples_out.iter().flat_map(|v| v.to_le_bytes()));
            }
            None => {
                self.meter_db = 0.0;
                data.extend_from_slice(input);
            }
        }
        if let Some(s) = self.silence.as_mut() {
            let mut next = std::mem::take(&mut self.stage2);
            next.clear();
            s.process(&data, &mut next);
            std::mem::swap(&mut data, &mut next);
            self.stage2 = next;
        }
        if let Some(sp) = self.speed.as_mut() {
            let mut next = std::mem::take(&mut self.stage2);
            next.clear();
            sp.process(&data, &mut next);
            std::mem::swap(&mut data, &mut next);
            self.stage2 = next;
        }
        if !data.is_empty() {
            self.push_pending(&data);
        }
        self.stage = data;
    }

    fn processing(&self) -> bool {
        self.eq.as_ref().is_some_and(|e| !e.is_identity()) || self.silence.is_some() || self.speed.is_some()
    }

    /// Applies changed sound settings to the equalizer.
    fn follow_sound(&mut self) {
        if self.sound_dirty {
            if let Some(eq) = self.eq.as_mut() {
                self.sound.apply(eq);
            }
            self.sound_dirty = false;
        }
    }

    pub fn play(&mut self) {
        self.track.play();
    }

    pub fn pause(&mut self) {
        self.track.pause();
    }

    /// Drops everything queued and processed; the clock restarts at the next buffer.
    pub fn flush(&mut self) {
        self.track.flush();
        self.reopen = None;
        self.pending.clear();
        self.pending_pos = 0;
        self.pending_media = 0.0;
        self.carry = 0.0;
        self.owed = None;
        self.needs_init = true;
        self.needs_sync = false;
        self.submitted_frames = 0.0;
        self.paces.clear();
        self.source_ended = false;
        if let Some(eq) = self.eq.as_mut() {
            eq.reset();
        }
        if let Some(s) = self.silence.as_mut() {
            s.flush();
        }
        if let Some(s) = self.speed.as_mut() {
            s.flush();
        }
    }

    /// End of the queue: drains the chain. The limiter's look-ahead is pushed out with silence.
    pub fn end_of_stream(&mut self) {
        let Some(f) = self.format else { return };
        let held = self.eq.as_ref().filter(|e| !e.is_identity()).map_or(0, Equalizer::delay_frames);
        if held > 0 {
            self.process(&vec![0u8; held * f.frame_bytes()], 0.0);
        }
        self.drain_stages();
    }

    /// Processed audio is waiting for room in the track.
    pub fn pending(&self) -> bool {
        self.pending_left()
    }

    /// Everything written has played.
    pub fn drained(&self) -> bool {
        self.track.is_empty() && !self.pending_left()
    }

    /// Waiting for the track to drain before reopening for the next stream.
    pub fn reopening(&self) -> bool {
        self.reopen.is_some()
    }

    /// Reopens the track for a waiting stream once the previous one has played out (the clock restarts,
    /// as with a new AudioTrack). False while it still plays.
    fn reopened(&mut self) -> bool {
        let Some(f) = self.reopen else { return true };
        self.write_pending();
        if !self.drained() {
            return false;
        }
        self.reopen = None;
        self.rebuilds += 1;
        self.format = Some(f);
        self.track.open(f);
        self.build_processors();
        self.needs_init = true;
        self.needs_sync = false;
        self.submitted_frames = 0.0;
        self.paces.clear();
        true
    }
}

impl<T: Track> Downstream for Sink<T> {
    type Config = u32;

    fn configure(&mut self, config: &u32, format: Option<Format>) {
        self.configs.push(*config);
        let f = format.expect("the player plays PCM");
        if self.format.is_some() && self.track.must_reopen(f) {
            self.reopen = Some(f);
            return;
        }
        if self.format != Some(f) {
            if self.format.is_some() {
                self.rebuilds += 1;
            }
            self.format = Some(f);
            self.track.open(f);
            self.build_processors();
        }
    }

    fn handle_buffer(&mut self, data: &[u8], from: usize, pts_us: i64) -> (bool, usize) {
        if !self.reopened() {
            return (false, 0);
        }
        let f = self.format.expect("configured before the first buffer");
        let fb = f.frame_bytes();
        let key = (data.as_ptr() as usize + from, data.len() - from);
        let continuing = self.owed.take();
        if let Some(owed) = continuing {
            assert_eq!(owed, key, "offered another buffer while one was only partly taken (media3 throws here)");
        }
        if continuing.is_none() {
            if self.needs_init {
                self.start_media_us = pts_us.max(0);
                self.needs_init = false;
                self.needs_sync = false;
            } else {
                let expected = self.start_media_us + (self.submitted_frames * 1_000_000.0 / f.rate as f64) as i64;
                if !self.needs_sync && (expected - pts_us).abs() > PTS_TOLERANCE_US {
                    self.timestamp_jumps += 1;
                    self.needs_sync = true;
                }
                if self.needs_sync {
                    self.start_media_us += pts_us - expected;
                    self.needs_sync = false;
                }
            }
        }
        if !self.write_pending() {
            self.owed = Some(key);
            return (false, 0);
        }
        let input = &data[from..];
        self.follow_sound();
        if self.processing() {
            self.note_pace();
            let media = (input.len() / fb) as f64 * self.pace;
            self.submitted_frames += media;
            self.process(input, media);
            self.write_pending();
            return (true, input.len());
        }
        let n = self.room_bytes().min(input.len()) / fb * fb;
        if n > 0 {
            self.note_pace();
            let media = (n / fb) as f64 * self.pace;
            self.track.write(&input[..n], media);
            self.submitted_frames += media;
        }
        if n < input.len() {
            self.owed = Some((key.0 + n, key.1 - n));
            return (false, n);
        }
        (true, n)
    }

    fn handle_discontinuity(&mut self) {
        self.needs_sync = true;
    }

    fn media_pace(&mut self, pace: f64) {
        self.pace = if pace.is_finite() && pace > 0.0 { pace } else { 1.0 };
    }

    fn position_us(&mut self, _source_ended: bool) -> Option<i64> {
        let f = self.format.filter(|_| !self.needs_init)?;
        Some(self.start_media_us + (self.track.played_media() * 1_000_000.0 / f.rate as f64) as i64)
    }
}

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
    /// ReplayGain for song `id` at queue index `index` (capped by [`Player::gain_max`]), applied per
    /// song by [`TransitionEngine::set_gain`].
    fn gain(&mut self, _index: usize, _id: &str) -> f32 {
        1.0
    }
}

/// Where the playlist is kept: the player's own, or the core's.
pub trait Queue {
    fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R;
    /// The player moved to `index` by itself.
    fn moved_to(&mut self, index: usize);
    fn set_repeat(&mut self, mode: u8);
    /// Arriving on list index `index` would skip it (explicit song, skip setting on).
    fn skips(&self, _index: usize) -> bool {
        false
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
/// serial that tells it from another stream of the same song ([`stream_key`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Period {
    index: usize,
    offset_us: i64,
    duration_us: i64,
    gain: f32,
    serial: u64,
}

/// The transition engine's id for a stream: the song id and a per-stream serial, so the same song
/// twice in a row (queued twice, repeat one) is two streams to it.
fn stream_key(id: &str, serial: u64) -> String {
    format!("{id}\u{1}{serial}")
}

/// The song id and serial of a [`stream_key`].
fn split_key(key: &str) -> (&str, Option<u64>) {
    match key.rsplit_once('\u{1}') {
        Some((id, serial)) => (id, serial.parse().ok()),
        None => (key, None),
    }
}

/// The app as the transition engine's host: stream keys become song ids on the way in, and a plan's
/// incoming song becomes the key of the stream that follows the outgoing one.
struct Keyed<'a, A: App> {
    app: &'a mut A,
    /// Serials of the streams handed out, and the last serial given.
    periods: &'a [Period],
    serials: u64,
}

impl<A: App> Host for Keyed<'_, A> {
    fn plan_for(&mut self, outgoing_id: &str) -> Option<Plan> {
        let (id, serial) = split_key(outgoing_id);
        let mut plan = self.app.plan_for(id)?;
        let serial = serial.unwrap_or(self.serials);
        let incoming = self.periods.iter().map(|p| p.serial).filter(|&s| s > serial).min().unwrap_or(self.serials + 1);
        plan.incoming_id = stream_key(&plan.incoming_id, incoming);
        Some(plan)
    }

    fn wants_analysis(&mut self, song_id: &str) -> Option<u64> {
        self.app.wants_analysis(split_key(song_id).0)
    }

    fn analysed(&mut self, song_id: &str, analyzer: Analyzer, channels: usize, frames: u64, rate: u32) {
        self.app.analysed(split_key(song_id).0, analyzer, channels, frames, rate);
    }

    fn heard_changed(&mut self) {
        self.app.heard_changed();
    }

    fn log(&mut self, message: &str) {
        if !message.contains('\u{1}') {
            return self.app.log(message);
        }
        // Keys read as song ids in the log.
        let mut plain = String::with_capacity(message.len());
        let mut rest = message;
        while let Some(k) = rest.find('\u{1}') {
            plain.push_str(&rest[..k]);
            rest = rest[k + 1..].trim_start_matches(|c: char| c.is_ascii_digit());
        }
        plain.push_str(rest);
        self.app.log(&plain);
    }

    fn now_ms(&self) -> i64 {
        self.app.now_ms()
    }
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
    pub engine: TransitionEngine<u32>,
    pub burst: Burst,
    pub sink: Sink<T>,
    pub app: A,
    pub queue: Q,
    pub tracks: S,
    /// Sound chain transport state: deferred rebuilds, the tuning buffer.
    pub chain: Chain,
    pub sound: Sound,
    pub tracker: HeardTracker,
    /// Measure upcoming songs whenever the queue moves (with AutoMix on).
    pub measure_on_move: bool,
    /// ReplayGain off (bit-perfect output). Call [`Player::gain_changed`] after changing it.
    pub gain_off: bool,
    /// Maximum gain: 1 unless float output with the limiter allows boosting (`nori_player::gain`).
    pub gain_max: f32,
    /// Keep the equalizer processor in the chain regardless of the sound ([`Player::keep_chain`]).
    chain_kept: bool,
    /// Sink depth while tuning: [`SHALLOW_US`], or less for an output with its own buffer behind.
    pub shallow_us: i64,
    /// The sound changed while paused: rebuild what the output holds on resume.
    resound: bool,
    reading: Option<Reader<S::Reading>>,
    /// The next song last prefetched ([`Songs::upcoming`]).
    upcoming: Option<String>,
    /// A song to read after a jump, seek or rebuild, still opening.
    opening: Option<Opening<S::Reading>>,
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
    token: u32,
    speed: (f32, f32),
    skip_silence: bool,
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
    /// Pause reading: the same song is being opened elsewhere (for a rebuild) and two readers would
    /// fight over its fetch. The output plays what it holds.
    pub read_held: bool,
    /// Start of the stream the clock is in; a new stream of the same song is a repeat-one loop.
    heard_period: Option<i64>,
    /// The [`stream_key`] of the stream the clock is in, and the last serial given.
    on_key: Option<String>,
    serials: u64,
    /// Repeat-one loops heard.
    pub loops: u32,
    /// A song failed for lack of network and the offline bridge takes over.
    pub bridge: bool,
    /// Queue ids as last seen, to map indexes across edits.
    ids: Vec<String>,
}

impl<S: Songs, T: Track, A: App, Q: Queue> Player<S, T, A, Q> {
    /// A player over `queue`, idle, writing into `track` through the deep buffer.
    pub fn build(tracks: S, queue: Q, app: A, track: T) -> Self {
        let mut p = Player {
            now_ms: 1_000,
            engine: TransitionEngine::new(),
            burst: Burst::default(),
            sink: Sink::new(BUFFER_US, false, Sound::default(), track),
            app,
            queue,
            tracks,
            chain: Chain::default(),
            sound: Sound::default(),
            tracker: HeardTracker::new(),
            measure_on_move: true,
            gain_off: false,
            gain_max: 1.0,
            chain_kept: false,
            shallow_us: SHALLOW_US,
            resound: false,
            reading: None,
            upcoming: None,
            opening: None,
            heard_from: None,
            next: None,
            periods: Vec::new(),
            playing: false,
            position_us: None,
            current: None,
            source_ended: false,
            token: 0,
            speed: (1.0, 1.0),
            skip_silence: false,
            changes: Vec::new(),
            failures: Vec::new(),
            errors: ErrorRun::new(),
            skip_on_error: true,
            failed: None,
            stopped: None,
            stop_after: None,
            hungry: false,
            read_held: false,
            heard_period: None,
            on_key: None,
            serials: 0,
            loops: 0,
            bridge: false,
            ids: Vec::new(),
        };
        p.engine.follow_rate = true;
        p.ids = p.queue.read(|q| q.ids().to_vec());
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
    fn call<R>(&mut self, f: impl FnOnce(&mut TransitionEngine<u32>, &mut Fed<'_, Sink<T>>, &mut Keyed<'_, A>) -> R) -> R {
        self.app.clock(self.now_ms);
        let mut fed = Fed::new(&mut self.sink, &mut self.burst, self.now_ms);
        let mut host = Keyed { app: &mut self.app, periods: &self.periods, serials: self.serials };
        f(&mut self.engine, &mut fed, &mut host)
    }

    fn configure(&mut self, i: usize, serial: u64, format: Format) {
        self.token += 1;
        let (s, t) = (StreamFormat { id: Some(stream_key(&self.id_at(i), serial)), format: Some(format) }, self.token);
        self.call(|e, d, a| e.configure(d, a, s, t));
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

    /// The [`stream_key`] of song `i`'s latest stream.
    fn key_at(&self, i: usize) -> Option<String> {
        self.periods.iter().rev().find(|p| p.index == i).map(|p| stream_key(&self.id_at(i), p.serial))
    }

    /// Starts reading song `i` (opened as `r`) from `from_ms` at `offset_us`, after a flush: now if
    /// ready, else once it is. False when it failed at once.
    fn begin(&mut self, i: usize, from_ms: i64, offset_us: i64, mut r: S::Reading) -> bool {
        self.reading = None;
        self.next = None;
        self.opening = None;
        // A seek or rebuild within the same song keeps its stream's identity.
        let serial = self.serial_for(i);
        self.periods = vec![Period { index: i, offset_us, duration_us: 0, gain: 1.0, serial }];
        self.on_key = Some(stream_key(&self.id_at(i), serial));
        self.position_us = Some(offset_us + from_ms * 1000);
        self.source_ended = false;
        self.heard_period = None;
        if r.ready() {
            return self.start_reading(i, from_ms, offset_us, r);
        }
        self.opening = Some(Opening { index: i, from_ms, offset_us, r });
        true
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
        let id = self.id_at(i);
        self.app.gain(i, &id).min(self.gain_max)
    }

    /// ReplayGain settings changed: rescales unplayed audio in the track and the engine, and the song
    /// being read from its next buffer.
    pub fn gain_changed(&mut self) {
        for k in 0..self.periods.len() {
            let p = self.periods[k];
            let gain = self.song_gain(p.index);
            if gain == p.gain || p.gain <= 0.0 {
                self.periods[k].gain = gain;
                continue;
            }
            let from = self.sink.media_frames(p.offset_us).unwrap_or(0.0);
            let to = self.periods.get(k + 1).and_then(|n| self.sink.media_frames(n.offset_us)).unwrap_or(f64::MAX);
            self.sink.track.rescale(from, to, gain / p.gain);
            self.engine.rescale(p.offset_us, gain / p.gain);
            self.periods[k].gain = gain;
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
        self.resound = false;
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
        // A jump empties the output anyway: do a pending chain swap or depth change now (at the next
        // boundary it would cut a crossfade).
        let capacity = self.depth();
        let swap = self.chain.boundary(false) == ChainAct::Rebuild || self.sink.capacity_us != capacity;
        if swap {
            self.app.log("chain swap at the boundary");
            self.call(|e, _, a| e.reset(a));
            self.burst.restart();
            self.sink.rebuild(capacity, self.chain_in(), self.sound.clone());
            self.sink.set_stages(self.speed.0, self.speed.1, self.skip_silence);
        } else {
            self.call(|e, _, a| e.flush(a));
            self.burst.restart();
            self.sink.flush();
        }
        self.queue.moved_to(i);
        if self.begin(i, from_ms, offset, r) {
            self.set_current(i);
        }
        if let Some(f) = self.reading.as_ref().filter(|_| swap).map(|r| r.r.format()) {
            self.app.log(&format!("AudioTrack {} Hz buffer={}", f.rate, f.bytes(capacity)));
        }
    }

    pub fn resume(&mut self) {
        if std::mem::take(&mut self.resound) {
            self.rebuild_sink();
        }
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
        if self.chain.paused() == ChainAct::Rebuild {
            self.rebuild_sink();
        }
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    /// Frees everything a long pause does not need and returns (queue index, ms) to [`Player::jump`]
    /// back to.
    pub fn release(&mut self) -> Option<(usize, i64)> {
        self.resound = false;
        let ended = self.source_ended;
        if let Some(now) = self.call(|e, d, a| e.position_us(d, a, ended)) {
            self.position_us = Some(now);
        }
        let at = self.current.map(|i| (i, self.position_ms()));
        self.call(|e, _, a| e.reset(a));
        self.burst.restart();
        let (capacity, dsp) = (self.sink.capacity_us, self.sink.dsp);
        self.sink.rebuild(capacity, dsp, self.sound.clone());
        self.sink.set_stages(self.speed.0, self.speed.1, self.skip_silence);
        self.reading = None;
        self.opening = None;
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
        let Some(i) = self.current else { return };
        self.resound = false;
        let offset = self.periods.iter().find(|p| p.index == i).map_or_else(|| self.fresh_offset(), |p| p.offset_us);
        let r = match self.tracks.open(&self.id_at(i), ms) {
            Ok(r) => r,
            Err(why) => return self.fail(i, PlaybackError::Other, why),
        };
        self.call(|e, _, a| e.flush(a));
        self.burst.restart();
        self.sink.flush();
        self.begin(i, ms, offset, r);
    }

    /// New sound settings, applied live; a processor joining or leaving the chain needs a rebuild,
    /// deferred to the next song while playing.
    pub fn set_sound(&mut self, sound: Sound) {
        let was = self.chain_in();
        let changed = sound != self.sound;
        self.sound = sound.clone();
        self.sink.set_sound(sound);
        // Paused: rebuild the output's old-sound audio on resume.
        self.resound |= changed && was && !self.playing && self.current.is_some();
        self.follow_chain(was);
    }

    /// Keeps the equalizer processor in the chain (bypassed when flat), so turning the sound on or off
    /// needs no rebuild.
    pub fn keep_chain(&mut self, on: bool) {
        let was = self.chain_in();
        self.chain_kept = on;
        self.follow_chain(was);
    }

    fn chain_in(&self) -> bool {
        self.chain_kept || self.sound.on()
    }

    /// Rebuilds the output if the processor joined or left the chain (`was`: it was in), at the next
    /// song while playing.
    fn follow_chain(&mut self, was: bool) {
        let change = ChainChange { processor_changed: was != self.chain_in(), ..Default::default() };
        match rebuild(change) {
            Rebuild::None => {}
            Rebuild::Now => self.rebuild_sink(),
            Rebuild::AtBoundary => {
                if !self.playing {
                    self.rebuild_sink();
                } else if self.chain.defer() {
                    self.app.log("chain swap deferred to the next track");
                }
            }
        }
    }

    /// The equalizer screen opened or closed: switches to or from the shallow buffer at the next
    /// boundary, or in place over a track that [`Track::resizes`].
    pub fn set_tuning(&mut self, on: bool) {
        let was = self.chain;
        let act = self.chain.tuning(on, self.sound.on(), self.current.is_none(), self.playing);
        self.burst.enabled = self.chain.bursting(false);
        if self.sink.track.resizes() {
            self.chain.swap_pending = was.swap_pending;
            self.chain.deep_at_next_pause = was.deep_at_next_pause;
            self.resize_sink();
            return;
        }
        if act == ChainAct::Rebuild {
            self.rebuild_sink();
        }
    }

    /// Sets the shallow buffer depth; applied in place if tuning over a resizable track, else the next
    /// time the sink goes shallow.
    pub fn set_shallow_us(&mut self, us: i64) {
        if us == self.shallow_us {
            return;
        }
        self.shallow_us = us;
        if self.chain.tuning && self.sink.track.resizes() {
            self.resize_sink();
        }
    }

    /// Applies [`Player::depth`] in place (for a track that [`Track::resizes`]).
    fn resize_sink(&mut self) {
        let capacity = self.depth();
        if self.sink.capacity_us != capacity {
            self.sink.capacity_us = capacity;
            self.sink.track.depth(capacity);
            self.app.log(&format!("output depth in place: {} ms", capacity / 1000));
        }
    }

    /// Sink depth: shallow while tuning, deep otherwise.
    fn depth(&self) -> i64 {
        if self.chain.tuning {
            self.shallow_us
        } else {
            BUFFER_US
        }
    }

    /// Rebuilds what the output holds from the audible position with the current sound, stages and
    /// depth, so a change is heard at once (for outputs that can drop audio without an audible gap).
    /// Includes any pending boundary or pause rebuild. Paused: done on resume.
    pub fn resound(&mut self) {
        self.resound_with(None);
    }

    /// [`Player::resound`] reusing song `id` opened ahead from `from_ms` as `r` (see [`Player::jump_from`]).
    pub fn resound_from(&mut self, id: String, r: S::Reading, from_ms: i64) {
        self.resound_with(Some((id, r, from_ms)));
    }

    fn resound_with(&mut self, opened: Option<(String, S::Reading, i64)>) {
        if self.current.is_none() {
            return;
        }
        if !self.playing {
            self.resound = true;
            return;
        }
        self.follow_clock();
        self.chain.swap_pending = false;
        self.chain.deep_at_next_pause = false;
        self.rebuild_sink_from(opened);
    }

    /// Speed and pitch, applied after the stages drain.
    pub fn set_speed(&mut self, speed: f32, pitch: f32) {
        self.speed = (speed, pitch);
        self.sink.set_stages(speed, pitch, self.skip_silence);
        self.app.log(&format!("speed in chain: x{speed} pitch x{pitch}"));
    }

    pub fn set_skip_silence(&mut self, on: bool) {
        self.skip_silence = on;
        self.sink.set_stages(self.speed.0, self.speed.1, on);
        if let Some(f) = self.sink.skipping_silence() {
            self.app.log(&format!("silence skipping in chain: {} Hz x{}", f.rate, f.channels));
        }
    }

    /// Speed and pitch as set.
    pub fn speed(&self) -> (f32, f32) {
        self.speed
    }

    /// Resets the engine, rebuilds the sink for current settings and rereads the audible song from the
    /// audible position.
    fn rebuild_sink(&mut self) {
        self.rebuild_sink_from(None);
    }

    /// [`Player::rebuild_sink`], reusing `opened` if it fits ([`taken_from`]).
    fn rebuild_sink_from(&mut self, opened: Option<(String, S::Reading, i64)>) {
        self.resound = false;
        // Inside a held ending, the audible song is the one before `current`.
        let ear = self.ear();
        self.call(|e, _, a| e.reset(a));
        self.burst.restart();
        let capacity = self.depth();
        self.sink.rebuild(capacity, self.chain_in(), self.sound.clone());
        self.sink.set_stages(self.speed.0, self.speed.1, self.skip_silence);
        if let Some((i, at_ms)) = ear {
            if self.current != Some(i) {
                self.current = Some(i);
                self.queue.moved_to(i);
                self.sync_queue();
            }
            let offset = self.fresh_offset();
            let id = self.id_at(i);
            let ready = opened.filter(|o| o.0 == id).and_then(|(_, r, from)| taken_from(r, from, at_ms.max(0)));
            let r = match ready {
                Some(r) => Ok(r),
                None => self.tracks.open(&id, at_ms.max(0)).map(|r| (r, at_ms.max(0))),
            };
            let (r, at_ms) = match r {
                Ok(r) => r,
                Err(why) => return self.fail(i, PlaybackError::Other, why),
            };
            self.begin(i, at_ms, offset, r);
            if let Some(f) = self.reading.as_ref().map(|r| r.r.format()) {
                self.app.log(&format!("AudioTrack {} Hz buffer={}", f.rate, f.bytes(capacity)));
            }
        }
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
                self.next = None;
            }
        }
    }

    /// The queue was edited. Indexes the player holds are remapped by id (nearest match); a song
    /// opened ahead that is no longer next is dropped.
    pub fn queue_changed(&mut self) {
        let ids = self.queue.read(|q| q.ids().to_vec());
        let old = std::mem::replace(&mut self.ids, ids);
        let edited = old != self.ids;
        if !old.is_empty() && old != self.ids {
            let new = &self.ids;
            let at = |i: usize| moved(&old, new, i).unwrap_or_else(|| i.min(new.len().saturating_sub(1)));
            self.current = self.current.map(at);
            if let Some(r) = self.reading.as_mut() {
                r.index = at(r.index);
            }
            if let Some(o) = self.opening.as_mut() {
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
                moved(&old, new, i).filter(|k| Some(*k) == reading || Some(*k) == after).map(|k| (k, kind, why))
            });
            match self.next.as_mut() {
                Some(n) if moved(&old, &self.ids, n.0).is_some_and(|i| Some(i) == after) => n.0 = after.expect("checked"),
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
    pub fn read_astray(&self, cur: usize) -> bool {
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
    /// `playlist_window`).
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
            let ids: Vec<(String, u32)> = window.into_iter().map(|i| (q.ids()[i].clone(), q.album_run(i))).collect();
            (ids, q.shuffling())
        });
        let window = window.iter().map(|(id, run)| WindowSong { album_run: *run, ..self.tracks.about(id) }).collect();
        self.app.window(window, shuffling);
    }

    fn set_current(&mut self, i: usize) {
        if self.current == Some(i) {
            return;
        }
        let first = self.current.is_none();
        self.current = Some(i);
        self.queue.moved_to(i);
        self.changes.push((self.now_ms, i));
        self.sync_queue();
        self.upcoming = self.next_of(i).map(|n| self.id_at(n));
        if let Some(id) = &self.upcoming {
            self.tracks.upcoming(id);
        }
        if !first && self.chain.boundary(false) == ChainAct::Rebuild {
            self.app.log("chain swap at the boundary");
            self.rebuild_sink();
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

    /// The seek bar's song and position.
    pub fn bar(&mut self) -> Seen {
        let pos = self.position_ms();
        let now = PlayerNow { now_ms: self.now_ms, playing: self.playing, on: self.on_key.as_deref(), position_ms: pos };
        let (periods, last) = (&self.periods, self.serials);
        let after = self.reading.as_ref().and_then(|r| self.next_of(r.index));
        // A stream not handed out yet is the one after the song being read.
        let find = |key: &str| match split_key(key).1 {
            Some(s) => periods.iter().find(|p| p.serial == s).map(|p| (p.index, if p.duration_us > 0 { p.duration_us / 1000 } else { i64::MAX })).or_else(|| (s > last).then_some(after).flatten().map(|n| (n, i64::MAX))),
            None => None,
        };
        self.tracker.at_streams(self.engine.heard(), now, &find)
    }

    /// What is audible now, per the engine.
    pub fn heard(&self) -> &Heard {
        self.engine.heard()
    }

    /// A mix is audible now.
    pub fn mixing(&self) -> bool {
        self.engine.heard().mixing
    }

    /// [`Player::ear`] with the clock read now (the last turn may have been a burst ago).
    pub fn ear_now(&mut self) -> Option<(usize, i64)> {
        if self.playing && self.current.is_some() {
            self.follow_clock();
        }
        self.ear()
    }

    /// The audible song and position, ms, as of the last turn. During a held ending that is the
    /// previous song, although `current` has moved on.
    pub fn ear(&mut self) -> Option<(usize, i64)> {
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
    pub fn ending_made(&self, cur: usize, start_us: Option<i64>) -> Option<Option<crate::engine::Plan>> {
        let reading = self.reading.as_ref().filter(|r| r.index == cur);
        if let Some(made) = self.key_at(cur).and_then(|k| self.engine.made(&k)) {
            if self.engine.holding() || reading.is_none() {
                // As the app made it: the incoming song by its id, not its stream key.
                return Some(made.cloned().map(|mut p| {
                    p.incoming_id = split_key(&p.incoming_id).0.to_string();
                    p
                }));
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

    /// Reading is blocked on bytes: [`Player::starved`], or the next song's first bytes.
    pub fn waiting_for_bytes(&self) -> bool {
        self.starved() || (self.failed.is_none() && self.next.as_ref().is_some_and(|(_, n)| n.is_ok()) && self.reading.as_ref().is_some_and(|r| r.ended && r.waiting && !r.left()))
    }

    /// One render turn at `now_ms`: reads the clock and offers audio until the output refuses.
    pub fn turn(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
        self.opened();
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
                    self.on_key = Some(stream_key(&self.id_at(p.index), p.serial));
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
        if self.read_held {
            return;
        }
        for k in 0..BUFFERS_PER_TURN {
            if !self.ensure_buffer() {
                break;
            }
            self.hungry = k + 1 == BUFFERS_PER_TURN;
            let Player { engine, sink, burst, app, reading, now_ms, periods, serials, .. } = self;
            let r = reading.as_mut().expect("a buffer is ready");
            app.clock(*now_ms);
            let mut fed = Fed::new(sink, burst, *now_ms);
            let pts = r.offset_us + r.r.at_us();
            let mut host = Keyed { app, periods, serials: *serials };
            let (taken, used) = engine.handle_buffer(&mut fed, &mut host, &r.r.buffer()[r.pos..], pts);
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
            self.sink.write_pending();
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

/// Index in `new` of `old[i]`'s id, nearest to `i`; `None` if removed.
fn moved(old: &[String], new: &[String], i: usize) -> Option<usize> {
    let id = old.get(i)?;
    new.iter().enumerate().filter(|(_, n)| *n == id).map(|(j, _)| j).min_by_key(|&j| j.abs_diff(i))
}
