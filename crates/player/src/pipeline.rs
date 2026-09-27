//! The player around the transition engine, with the platform left out: a queue walked song by song,
//! one reading at a time, the engine fed in bursts, and below it an output shaped like media3's
//! AudioSink with nori's processors in it (equalizer, silence skipping, speed and pitch) over whatever
//! plays the samples. The simulated player (`sim`, a virtual AudioTrack on a virtual clock) and the
//! desktop player (`nori-engine`, a ring buffer a sound card pulls from) are both this, with their own
//! songs, their own device below the sink and their own clock.
//!
//! What a platform brings: [`Songs`] (a song id opened for reading, as decoded buffers), [`Track`]
//! (the device buffer the sink writes into and whose playhead it reads), [`App`] (the transition
//! planner and the log) and [`Queue`] (the playlist, wherever it is kept).

use std::collections::VecDeque;

use crate::burst::{Burst, Fed, BUFFER_US};
use crate::dsp::{Band, Effects, Equalizer};
use crate::engine::{Downstream, Heard, Host, StreamFormat, TransitionEngine, POSITION_NOT_SET};
use crate::heard::{HeardTracker, Seen};
use crate::pcm::{Encoding, Format};
use crate::playlist::Playlist;
use crate::queue::{measure_ahead, ErrorRun, OnError, PlaybackError};
use crate::silence::SilenceSkipper;
use crate::sound::sound_on;
use crate::speed::{speed_active, SpeedPitch};
use crate::transitions::WindowSong;
use crate::transport::{rebuild, Chain, ChainAct, ChainChange, Rebuild};

/// media3 starts the renderer's timeline here, so timestamps are never small numbers.
pub const BASE_OFFSET_US: i64 = 1_000_000_000_000;
/// The player reads the next song only once the one it is reading is this close to its end.
pub const READ_AHEAD_US: i64 = 10_000_000;
/// The equalizer screen's output buffer.
pub const SHALLOW_US: i64 = 500_000;
/// media3 resyncs its clock when a buffer's timestamp is this far from where the count says it should be.
const PTS_TOLERANCE_US: i64 = 200_000;
/// The limiter as the settings run it.
const LIMITER_RELEASE_MS: f64 = 120.0;
const LIMITER_LOOKAHEAD_MS: f64 = 5.0;
/// How many buffers one turn offers at most before it lets the thread do something else.
const BUFFERS_PER_TURN: usize = 256;
/// Changes of pace a sink keeps track of while they are in flight: a ramp after a mix changes it on
/// every buffer, and a deep buffer holds some ten seconds of them.
const PACES: usize = 512;

/// The sound settings, as the settings store hands them to the chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    /// The equalizer's bands; empty is the parametric equalizer off.
    pub bands: Vec<Band>,
    /// The graphic equalizer's sliders (dB, one per band of a `graphic::LAYOUTS` layout), played in place
    /// of `bands`; empty is the graphic equalizer off.
    pub graphic: Vec<f64>,
    /// Bass boost, compressor, virtualizer and volume boost.
    pub effects: Effects,
    pub preamp_db: f64,
    pub crossfeed_db: f64,
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
            balance: 0.0,
            mono: false,
            limiter: false,
            threshold_db: -1.0,
        }
    }
}

impl Sound {
    /// Whether anything here touches the samples: the equalizer processor then sits in the chain.
    pub fn on(&self) -> bool {
        let eq = !self.bands.is_empty() || !self.graphic.is_empty() || self.preamp_db != 0.0;
        sound_on(eq, self.crossfeed_db as f32, self.balance as f32, self.mono, self.limiter, self.effects.on())
    }

    /// The chain set up the way `follow_chain` in the core sets it up. Anything that boosts the level
    /// (the volume boost, bass boost, a compressor's make-up) brings the limiter with it.
    pub fn apply(&self, eq: &mut Equalizer) {
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

/// What the sink writes into and reads its playhead from: an AudioTrack's buffer on a phone, a ring a
/// sound card pulls from on a desktop, a list of pieces on a virtual clock in the tests. Everything is
/// in the sink's format (interleaved at the stream's rate, 16-bit or float as the stream comes); the
/// track converts if its device wants something else.
pub trait Track {
    /// The sink's format from now on (a new stream shape, or the sink built again).
    fn open(&mut self, format: Format);
    /// Bytes handed over and not played yet, in the sink's format.
    fn queued_bytes(&self) -> usize;
    /// Takes `data` whole (the sink never offers more than there is room for), standing for `media`
    /// frames of the song: speed and silence skipping make the two differ.
    fn write(&mut self, data: &[u8], media: f64);
    /// Frames of the song played out since the last flush, as the ear has them.
    fn played_media(&mut self) -> f64;
    /// Nothing handed over is left to play.
    fn is_empty(&self) -> bool;
    /// Everything handed over is dropped, and the playhead starts again at nought.
    fn flush(&mut self);
    fn play(&mut self);
    fn pause(&mut self);
    /// The music handed over and not played yet between `from` and `to` media frames past the last flush
    /// is to be heard `ratio` times as loud: the ReplayGain settings changed, and it was made at the old
    /// volume. A track whose buffer is long enough to matter scales what it still holds; the rest is
    /// at the new volume already.
    fn rescale(&mut self, _from: f64, _to: f64, _ratio: f32) {}
    /// The bits per sample of the song whose stream is configured next, as its file stores them (0
    /// unknown): for a track that plays each song in its own format.
    fn source_bits(&mut self, _bits: u32) {}
    /// Whether the track must be opened again for a stream in `format` (and the bits last told), which it
    /// does only once everything handed over before has played: a track that plays each song as it is
    /// (bit-perfect, high quality output) rather than converting it to the first one's format.
    fn must_reopen(&mut self, _format: Format) -> bool {
        false
    }
    /// How much audio the sink keeps in the track from now on, told as the sink is built (before the
    /// flush that goes with it): a track with a buffer of its own behind the one counted here (a device
    /// behind a ring) keeps its own as shallow when this is.
    fn depth(&mut self, _capacity_us: i64) {}
    /// Whether [`Track::depth`] takes effect at once, over the same track, keeping what it holds and
    /// playing on: the sink's depth then changes in place when the equalizer screen opens or closes,
    /// with nothing made again ([`Player::set_tuning`]).
    fn resizes(&self) -> bool {
        false
    }
}

/// media3's AudioSink with nori's processors in it, over a [`Track`]. Its clock is media3's: the
/// timestamp of the first buffer after a flush, moved by the difference whenever a buffer arrives more
/// than 200 ms from where the count of submitted frames says it should be (or after a discontinuity),
/// plus the song time of what the track has played. The transition engine relies on exactly that: a
/// mix is stamped in the next song's time, and the clock follows the moment it is offered.
///
/// Once its buffers have grown to the stream's buffer size, nothing here allocates.
pub struct Sink<T: Track> {
    pub format: Option<Format>,
    /// Every token the engine configured this output with, in order.
    pub configs: Vec<u32>,
    /// Times the output was opened for another format after the first.
    pub rebuilds: usize,
    /// How much audio the track may hold.
    pub capacity_us: i64,
    /// Whether the equalizer processor sits in the chain; set when the sink is built.
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
    /// The song's frames handed in since the clock's reference (a frame of a stretched mix is more or
    /// less than one: [`Downstream::media_pace`]).
    submitted_frames: f64,
    /// The song time each frame offered stands for, as the transition engine last said.
    pace: f64,
    /// Where (in the song frames handed in since the last flush) each pace began, for the one under the
    /// play head ([`Sink::pace_heard`]). Room for a ramp's worth is made once.
    paces: VecDeque<(f64, f64)>,
    /// Buffers whose timestamp was more than 200 ms off: each is a stutter on a phone.
    pub timestamp_jumps: usize,
    /// A buffer taken only in part: the next offer must be the rest of it, or media3 throws.
    owed: Option<(usize, usize)>,
    /// A stream the track opens again for, once what it holds of the one before has played out.
    reopen: Option<Format>,
    /// Processed audio still to go into the track (from `pending_pos` on), the song time it stands
    /// for, and song time not yet attached to any output.
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
    /// The source has ended: running dry now is the end of the music, not a gap.
    pub source_ended: bool,
    /// The largest reduction the limiter reported, dB.
    pub gain_reduction_db: f32,
    /// What the limiter took off the last buffer through it, dB: the meter a screen shows.
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

    /// The sink as a stop and a prepare make it again, over the same track: a new chain for the
    /// settings as they are now, everything in flight dropped, the stages' settings kept.
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

    /// The song time per frame of what the track plays now ([`Downstream::media_pace`]): 1, but for a song
    /// brought into a mix at another tempo. Times the speed, it is how fast the place moves.
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

    /// A pace begins with the song frames handed in so far.
    fn note_pace(&mut self) {
        if self.paces.back().is_some_and(|p| p.1 == self.pace) {
            return;
        }
        if self.paces.len() == PACES {
            // More changes of pace in flight than a ramp makes: the oldest is past hearing anyway.
            self.paces.pop_front();
        }
        self.paces.push_back((self.submitted_frames, self.pace));
    }

    /// Frames of the song the track will have played when the clock reads `pts_us`, counted from the
    /// last flush as [`Track::played_media`] counts them; none before the first buffer.
    pub fn media_frames(&self, pts_us: i64) -> Option<f64> {
        let f = self.format.filter(|_| !self.needs_init)?;
        Some((pts_us - self.start_media_us) as f64 * f.rate as f64 / 1_000_000.0)
    }

    pub fn queued_us(&self) -> i64 {
        self.format.map_or(0, |f| f.us(self.track.queued_bytes()))
    }

    /// The buffer size the track is opened with, bytes.
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
        // media3's silence skipping took 16-bit audio only and stood aside for float; this one takes both.
        self.silence = self.skip_silence.then(|| SilenceSkipper::of(f.rate, f.channels, f.encoding == Encoding::Float));
        self.speed = speed_active(self.speed_pitch.0, self.speed_pitch.1).then(|| {
            let mut s = SpeedPitch::new(f.rate, f.channels, f.encoding);
            s.set(self.speed_pitch.0, self.speed_pitch.1);
            s.flush();
            s
        });
    }

    /// Whether the sound chain (equalizer, pre-amp, limiter, ...) is in the path of the samples.
    pub fn chain_in(&self) -> bool {
        self.eq.is_some()
    }

    /// The format the silence skipper runs at, while it is in the path of the samples.
    pub fn skipping_silence(&self) -> Option<Format> {
        self.format.filter(|_| self.silence.is_some())
    }

    /// New sound settings: the equalizer picks them up on its next buffer, live.
    pub fn set_sound(&mut self, sound: Sound) {
        self.sound = sound;
        self.sound_dirty = true;
    }

    /// Speed and pitch, and silence skipping: what is inside the stages now plays out first, then
    /// they start again with the new settings, as media3 applies new parameters after draining.
    pub fn set_stages(&mut self, speed: f32, pitch: f32, skip_silence: bool) {
        self.drain_stages();
        self.speed_pitch = (speed, pitch);
        self.skip_silence = skip_silence;
        self.build_stages();
    }

    /// The stages' held audio out into the track.
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

    /// Moves processed audio into the track as far as it has room; true when none is left over.
    fn write_pending(&mut self) -> bool {
        if !self.pending_left() {
            return true;
        }
        let room = self.room_bytes();
        let left = self.pending.len() - self.pending_pos;
        let n = room.min(left);
        if n > 0 {
            // The song time goes with the bytes in proportion, so what is left keeps its share.
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

    /// Runs `input` through the processors in media3's order (equalizer, silence skipping, speed) into
    /// the pending output. Every buffer here is kept between calls.
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

    /// The equalizer set up for the latest settings, on the buffer after they changed.
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

    /// Everything queued and processed is dropped, and the clock starts again at the next buffer.
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

    /// The end of the queue: what the chain still holds comes out, as media3 drains its processors.
    /// The limiter keeps its look-ahead (5 ms) and nothing more would push it out, so that much
    /// silence goes through it; the silence skipper and the speed stage give up what they hold.
    pub fn end_of_stream(&mut self) {
        let Some(f) = self.format else { return };
        let held = self.eq.as_ref().filter(|e| !e.is_identity()).map_or(0, Equalizer::delay_frames);
        if held > 0 {
            // Once per queue, so the silence is made here rather than kept.
            self.process(&vec![0u8; held * f.frame_bytes()], 0.0);
        }
        self.drain_stages();
    }

    /// Processed audio waiting for room goes into the track as far as it fits: once the source has
    /// ended nothing else offers it.
    pub fn write_out(&mut self) {
        self.write_pending();
    }

    /// Processed audio is waiting for room in the track.
    pub fn pending(&self) -> bool {
        self.pending_left()
    }

    /// Whether everything handed over has been played.
    pub fn drained(&self) -> bool {
        self.track.is_empty() && !self.pending_left()
    }

    /// The track waits to play out the stream before it opens again for the next one.
    pub fn reopening(&self) -> bool {
        self.reopen.is_some()
    }

    /// The stream waiting for the track to open again, once the one before has played out: opened now,
    /// and its clock starts again at its first buffer, as media3's sink starts a new AudioTrack. False
    /// while the stream before still plays.
    fn reopened(&mut self) -> bool {
        let Some(f) = self.reopen else { return true };
        self.write_pending();
        if !self.drained() {
            return false;
        }
        self.reopen = None;
        self.rebuilds += 1;
        // Nothing is left to drop: the track opens anew, its playhead from nought.
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
            // A track that plays every song in its own format: what it holds of the stream before plays
            // out first, as media3's sink drains its AudioTrack before it makes one for another format.
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

    fn position_us(&mut self, _source_ended: bool) -> i64 {
        match self.format {
            Some(f) if !self.needs_init => self.start_media_us + (self.track.played_media() * 1_000_000.0 / f.rate as f64) as i64,
            _ => POSITION_NOT_SET,
        }
    }
}

/// One song opened for reading: decoded buffers of interleaved samples (16-bit, or float for high
/// quality output), one at a time.
pub trait Reading {
    fn format(&self) -> Format;
    /// The song's length, µs, as far as it is known (exactly, once it has been read to its end).
    fn duration_us(&self) -> i64;
    /// Whether the reading can go on without waiting: it is open (a platform may open a song whose
    /// bytes are still on their way off its own thread) and the next buffer's bytes have arrived. A
    /// reading that is not ready is asked again on the next turn, and nothing else is asked of it
    /// before this has said yes once.
    fn ready(&mut self) -> bool {
        true
    }
    /// Why the song will not play on: it could not be opened, or its bytes stopped coming for good
    /// (it was read to its "end" early). Asked once it is ready, and once it is read to its end.
    fn error(&self) -> Option<(PlaybackError, String)> {
        None
    }
    /// The next buffer; false at the end of the song.
    fn fill(&mut self) -> bool;
    /// The buffer [`Reading::fill`] made.
    fn buffer(&self) -> &[u8];
    /// Where in the song the buffer begins, µs.
    fn at_us(&self) -> i64;
    /// Bits per sample as the file stores them (0 unknown or not stored that way): for a track that
    /// plays each song as it is.
    fn bits(&self) -> u32 {
        0
    }
    /// A reading opened ahead of the ear (the music made again with a new sound) is started later than
    /// planned: what comes before `ms` of the song is decoded and dropped, as a seek drops it. Only
    /// before its first buffer is handed out; false when it cannot, and the song is opened again there.
    fn skip_to_ms(&mut self, _ms: i64) -> bool {
        false
    }
}

/// The songs a queue names, as a platform opens them.
pub trait Songs {
    type Reading: Reading;
    /// Song `id` read from `from_ms` on (a seek lands where the format lets it, and what comes before
    /// the place asked for is decoded and dropped). An error is a song that will not play.
    fn open(&mut self, id: &str, from_ms: i64) -> Result<Self::Reading, String>;
    /// What the planner and the seek bar know of `id`: its length as tagged, its album and number.
    fn about(&self, id: &str) -> WindowSong;
    /// `id` plays next: a platform may start fetching it now, in the same burst as the song before.
    fn upcoming(&mut self, _id: &str) {}
}

/// The app around the player: the transition planner, the analysis store and the log.
pub trait App: Host {
    /// The player's clock as the next calls into the engine are made: [`Host::now_ms`] answers it.
    fn clock(&mut self, now_ms: i64);
    /// Whether AutoMix is on: the songs coming up are measured then.
    fn auto_mix(&self) -> bool;
    /// The planner's window: the song before the current one, then it and those after it in play
    /// order, repeat included.
    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool);
    /// Measures what it has not measured of `ids`, the songs coming up.
    fn measure_ahead<S: Songs>(&mut self, _songs: &mut S, _ids: &[String]) {}
    /// The music goes to another output device (`kind`, called `name` by the system): the name it is
    /// known by, and the sound to play it with when the device has one of its own (a profile bound to
    /// it). `None` when the app does not follow devices.
    fn output_changed(&mut self, _kind: crate::outputs::OutputKind, _name: &str) -> Option<(String, Option<Sound>)> {
        None
    }
    /// Songs were measured since this was last asked (by a job of the app's own, in the background):
    /// a plan made without them is asked for again.
    fn measured(&mut self) -> bool {
        false
    }
    /// Whether the output forbids touching the samples (bit-perfect or high quality output): the
    /// planner stands every transition down (`AudioPolicy::transitions_off`).
    fn transitions_off(&mut self, _off: bool) {}
    /// A song would not play: what to do, when the app keeps the run of failures itself (the core does,
    /// `rules::queue_error`). `None` leaves it to the player's own count.
    fn on_error(&mut self, _kind: PlaybackError, _has_next: bool) -> Option<OnError> {
        None
    }
    /// Music is coming out of the output: a run of songs that would not play is broken. Not merely a
    /// new song, since the skip a failure makes is one too.
    fn playing(&mut self) {}
    /// The volume song `id` (queue index `index`) plays at under ReplayGain, 0..1: the player scales
    /// the song's samples by it before the transition engine holds or mixes them
    /// ([`TransitionEngine::set_gain`]).
    fn gain(&mut self, _index: usize, _id: &str) -> f32 {
        1.0
    }
}

/// Where the playlist is kept: the player's own, or the core's.
pub trait Queue {
    fn read<R>(&self, f: impl FnOnce(&Playlist) -> R) -> R;
    /// The player moved to `index` by itself (a song ended, a jump).
    fn moved_to(&mut self, index: usize);
    fn set_repeat(&mut self, mode: u8);
    /// Arriving on queue (list) index `index` now would skip straight past it: an explicit song, with
    /// the user's setting to skip them and somewhere to go (the core's `playlist_transition`).
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

/// A stream handed to the output: which song, where it starts in the renderer's time, how long it is,
/// and the volume its samples were scaled to.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Period {
    index: usize,
    offset_us: i64,
    duration_us: i64,
    gain: f32,
}

/// The song being read, and where it starts in the renderer's timeline.
struct Reader<R> {
    index: usize,
    offset_us: i64,
    r: R,
    pos: usize,
    ended: bool,
    /// The last turn found its next buffer's bytes still on their way.
    waiting: bool,
}

/// A song opened to be read from `from_ms` that is not ready yet: it starts reading once it is.
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

/// ExoPlayer, as far as the audio cares: a queue, one renderer reading one song at a time, and the
/// transition engine in front of the output. Every call into the engine goes through [`Fed`] with the
/// player's clock, as the Android glue makes it.
pub struct Player<S: Songs, T: Track, A: App, Q: Queue> {
    pub now_ms: i64,
    pub engine: TransitionEngine<u32>,
    pub burst: Burst,
    pub sink: Sink<T>,
    pub app: A,
    pub queue: Q,
    pub tracks: S,
    /// Transport decisions for the sound chain: deferred rebuilds, the equalizer screen's buffer.
    pub chain: Chain,
    pub sound: Sound,
    pub tracker: HeardTracker,
    /// Whether the songs coming up are measured whenever the queue moves (with AutoMix on).
    pub measure_on_move: bool,
    /// ReplayGain stands down: the output takes the samples as they are (bit-perfect). Changed through
    /// [`Player::gain_changed`]'s caller, which then calls it.
    pub gain_off: bool,
    /// The equalizer processor stays in the chain whatever the sound ([`Player::keep_chain`]).
    chain_kept: bool,
    /// How much the sink holds while the equalizer is tuned: [`SHALLOW_US`], or less for an output
    /// with a buffer of its own behind the sink's (nori-engine's ring before a device).
    pub shallow_us: i64,
    /// The sound changed while paused: what the output holds is made again when the music comes back.
    resound: bool,
    reading: Option<Reader<S::Reading>>,
    /// The song after the current one as last fetched ahead ([`Songs::upcoming`]): a queue edit that
    /// puts another there fetches and measures that one at once.
    upcoming: Option<String>,
    /// A song to read after a jump, a seek or a rebuild, still opening.
    opening: Option<Opening<S::Reading>>,
    /// Where the song being read started, until the output's clock has moved past it: music is heard.
    heard_from: Option<i64>,
    /// The song after the one being read, opened as soon as that one is read to its end: which queue
    /// index, and what opening it gave.
    next: Option<(usize, Result<S::Reading, String>)>,
    periods: Vec<Period>,
    playing: bool,
    /// The renderer's position: where a seek or a start put it, then what the engine reports once
    /// the output has a clock.
    position_us: i64,
    current: Option<usize>,
    source_ended: bool,
    token: u32,
    speed: (f32, f32),
    skip_silence: bool,
    /// Every song change the player made, with the time it happened. A platform takes them as it
    /// reports them.
    pub changes: Vec<(i64, usize)>,
    /// Songs that would not play and why, as the player met them; a platform takes them as it reports them.
    pub failures: Vec<(String, String)>,
    /// The run of songs that would not play, and whether the user lets the player skip them.
    pub errors: ErrorRun,
    pub skip_on_error: bool,
    /// A song that would not play, found while reading ahead (or one that stopped half way); its error
    /// is raised when playback gets there.
    failed: Option<(usize, PlaybackError, String)>,
    /// Playback stopped at this song because it would not play: nothing is read until a jump, and a
    /// play tries the song again, as a platform's player does after an error.
    stopped: Option<usize>,
    /// The song the music stops at the end of ([`Player::pause_at_end`]).
    stop_after: Option<usize>,
    /// The last turn stopped at its budget of buffers with the output still taking them.
    hungry: bool,
    /// Nothing is read on from the song being read while set: the same song is being opened elsewhere
    /// in it (the music about to be made again), and two readers of one song's bytes pull its fetch back
    /// and forth. The output plays what it holds meanwhile; the one setting it keeps that enough.
    pub read_held: bool,
    /// Where the stream the output's clock is in starts: a new one of the same song is a repeat loop.
    heard_period: Option<i64>,
    /// Times the song playing started again by itself (repeat one), counted as the ear reaches it.
    pub loops: u32,
    /// A song would not play for want of the network and the app's offline bridge is to take over: playback
    /// stopped there, and the platform hands it on.
    pub bridge: bool,
    /// The queue's ids as the player last followed it: an edit is read against them.
    ids: Vec<String>,
}

impl<S: Songs, T: Track, A: App, Q: Queue> Player<S, T, A, Q> {
    /// A player over `queue`, nothing playing yet, writing into `track` through a deep buffer.
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
            position_us: POSITION_NOT_SET,
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
            loops: 0,
            bridge: false,
            ids: Vec::new(),
        };
        p.ids = p.queue.read(|q| q.ids().to_vec());
        p.sync_queue();
        p
    }

    /// The id at queue (list) index `i`.
    pub fn id_at(&self, i: usize) -> String {
        self.queue.read(|q| q.ids()[i].clone())
    }

    fn next_of(&self, i: usize) -> Option<usize> {
        if self.stop_after == Some(i) {
            return None;
        }
        self.queue.read(|q| q.next_of(i, q.repeat())).map(|n| self.playable(n))
    }

    /// The music stops at the end of the song playing (the sleep timer's "end of this song"): nothing
    /// after it is read or mixed into, as at the end of the queue, and [`Player::ended`] says when it has
    /// been heard to its end. Off again with `false`, or by the next jump.
    pub fn pause_at_end(&mut self, on: bool) {
        let Some(c) = self.current.filter(|_| on) else {
            self.stop_after = None;
            return;
        };
        // Read on into the next song already (one shorter than what is read ahead): the rest of this one
        // is read again, so its end is where the music stops.
        if self.reading.as_ref().is_some_and(|r| r.index != c) {
            let at = self.position_ms();
            self.jump(c, at);
        }
        self.stop_after = Some(c);
    }

    /// The song the music stops at the end of, while [`Player::pause_at_end`] is on.
    pub fn stopping_after(&self) -> Option<usize> {
        self.stop_after
    }

    /// `i`, or the first song after it that arriving on would not skip.
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

    /// Calls into the engine as the JNI glue does: the output below fed in bursts, on this clock.
    fn call<R>(&mut self, f: impl FnOnce(&mut TransitionEngine<u32>, &mut Fed<'_, Sink<T>>, &mut A) -> R) -> R {
        self.app.clock(self.now_ms);
        let mut fed = Fed::new(&mut self.sink, &mut self.burst, self.now_ms);
        f(&mut self.engine, &mut fed, &mut self.app)
    }

    fn configure(&mut self, i: usize, format: Format) {
        self.token += 1;
        let (s, t) = (StreamFormat { id: Some(self.id_at(i)), format: Some(format) }, self.token);
        self.call(|e, d, a| e.configure(d, a, s, t));
    }

    /// Song `i` (opened as `r`) is read from `from_ms` on a fresh timeline, after a flush: at once
    /// when it is ready, or once it is. False when it failed at once (and the player has moved on).
    fn begin(&mut self, i: usize, from_ms: i64, offset_us: i64, mut r: S::Reading) -> bool {
        self.reading = None;
        self.next = None;
        self.opening = None;
        // Where the player stands while the song opens; its length comes with it.
        self.periods = vec![Period { index: i, offset_us, duration_us: 0, gain: 1.0 }];
        self.position_us = offset_us + from_ms * 1000;
        self.source_ended = false;
        self.heard_period = None;
        if r.ready() {
            return self.start_reading(i, from_ms, offset_us, r);
        }
        self.opening = Some(Opening { index: i, from_ms, offset_us, r });
        true
    }

    /// The song waiting to open, if it has: read from here on, or failed.
    fn opened(&mut self) {
        let Some(o) = self.opening.as_mut() else { return };
        if !o.r.ready() {
            return;
        }
        let o = self.opening.take().expect("checked");
        self.start_reading(o.index, o.from_ms, o.offset_us, o.r);
    }

    /// Starts reading song `i` (opened as `r`, and ready) from `from_ms` on a fresh timeline; false
    /// when it would not open after all.
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
        self.periods = vec![Period { index: i, offset_us, duration_us, gain }];
        self.position_us = offset_us + from_ms * 1000;
        self.heard_from = Some(self.position_us);
        self.source_ended = false;
        self.configure(i, format);
        self.engine.set_output_stream_offset_us(offset_us);
        self.engine.set_gain(gain);
        true
    }

    /// The volume song `i` is heard at: its ReplayGain, or as it is when the output takes the samples
    /// untouched.
    fn song_gain(&mut self, i: usize) -> f32 {
        if self.gain_off {
            return 1.0;
        }
        let id = self.id_at(i);
        self.app.gain(i, &id)
    }

    /// The ReplayGain settings changed (or whether they may apply): every song handed to the output is
    /// heard at its new volume from now on. The music the output still holds is scaled there (as far as
    /// the track can), so is what the transition engine holds on its way to it, and the song being read
    /// is scaled from its next buffer.
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
            // And what the engine took in of it and the sink has not taken yet: the rest of a buffer the
            // sink took only part of goes on at the new level from where the track's music ends.
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

    /// A timeline for a new start, past everything played so far.
    fn fresh_offset(&self) -> i64 {
        self.periods.iter().map(|p| p.offset_us + p.duration_us).max().unwrap_or(BASE_OFFSET_US - 1_000_000) + 1_000_000
    }

    /// Plays queue index `i` from its start.
    #[cfg(any(test, feature = "synth"))]
    pub fn play_from(&mut self, i: usize) {
        self.jump(i, 0);
        self.resume();
    }

    /// Queue index `i` from `from_ms`, without touching whether it plays. A song that arriving on skips
    /// (an explicit one) gives way to the first after it that does not.
    pub fn jump(&mut self, i: usize, from_ms: i64) {
        self.jump_opened(i, from_ms, None);
    }

    /// [`Player::jump`], with song `i` opened already from `opened.2` ms (`opened.1`, ahead of time, so
    /// that nothing waits for it here): taken when it is that song, and read from `from_ms`, dropping what
    /// comes before, when the ear got further meanwhile; opened again otherwise.
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
        // A jump empties the output anyway, so a swap waiting for a boundary is made here, where it costs
        // nothing. Left for the next song's start, which with a crossfade on is inside the mix, it cut the
        // mix off where it was heard - and a jump back to the song on the page is not a change of song.
        let capacity = self.depth();
        // A buffer of another depth than the tuning wants (the equalizer screen opened with nothing
        // loaded) is made here too.
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
        // The place is taken from the clock as it stops: the last turn may have been a burst ago.
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

    /// Lets everything go that a long pause does not need - the engine's held audio, the output's
    /// buffer, the song being read - and says where the player was (queue index, ms), for a
    /// [`Player::jump`] there when music is asked for again. The output is opened again then.
    pub fn release(&mut self) -> Option<(usize, i64)> {
        self.resound = false;
        // Where the ear is now: the last turn may have been a while before the pause.
        let ended = self.source_ended;
        let now = self.call(|e, d, a| e.position_us(d, a, ended));
        if now != POSITION_NOT_SET {
            self.position_us = now;
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

    /// A seek in the song the player is on. The engine and the output are flushed, and the renderer
    /// starts reading there on the same timeline - announcing the song again, as media3 does when the
    /// stream it reads changes.
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

    /// New sound settings. The equalizer follows them live; a processor joining or leaving the chain
    /// needs a rebuild, which waits for the next song while music plays.
    pub fn set_sound(&mut self, sound: Sound) {
        let was = self.chain_in();
        let changed = sound != self.sound;
        self.sound = sound.clone();
        self.sink.set_sound(sound);
        // Paused, nothing is heard of the change until play: what the output holds, made with the old
        // sound, is made again then (a band dragged while paused costs nothing until the music is back).
        self.resound |= changed && was && !self.playing && self.current.is_some();
        self.follow_chain(was);
    }

    /// Whether the equalizer processor sits in the chain whatever the sound, flat and skipped when
    /// nothing in it is on, as Android keeps it on every path where the samples may be touched: the
    /// sound switched on or off is then heard from the next buffer, with no rebuild waiting for the
    /// next song. Off, the processor joins and leaves with the sound.
    pub fn keep_chain(&mut self, on: bool) {
        let was = self.chain_in();
        self.chain_kept = on;
        self.follow_chain(was);
    }

    fn chain_in(&self) -> bool {
        self.chain_kept || self.sound.on()
    }

    /// The processor joins or leaves the chain (it was in it: `was`): the output is built again for
    /// it, at the next song while music plays.
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

    /// The equalizer screen opened or closed: its shallow buffer comes and goes at the next boundary; at
    /// once, and in place, over a track that [`Track::resizes`].
    pub fn set_tuning(&mut self, on: bool) {
        let was = self.chain;
        let act = self.chain.tuning(on, self.sound.on(), self.current.is_none(), self.playing);
        self.burst.enabled = self.chain.bursting(false);
        if self.sink.track.resizes() {
            // Nothing waits for a boundary or a pause, and nothing is made again: the sink takes no more
            // than the new depth from here on, and the track follows.
            self.chain.swap_pending = was.swap_pending;
            self.chain.deep_at_next_pause = was.deep_at_next_pause;
            self.resize_sink();
            return;
        }
        if act == ChainAct::Rebuild {
            self.rebuild_sink();
        }
    }

    /// The shallow buffer is `us` from now on (a device kept shallow found it needs a deeper ring to be
    /// fed from): at once, in place, while tuned over a track that [`Track::resizes`], else from the
    /// next time the sink is made shallow.
    pub fn set_shallow_us(&mut self, us: i64) {
        if us == self.shallow_us {
            return;
        }
        self.shallow_us = us;
        if self.chain.tuning && self.sink.track.resizes() {
            self.resize_sink();
        }
    }

    /// The sink's depth as the tuning wants it, in place: for a track that [`Track::resizes`].
    fn resize_sink(&mut self) {
        let capacity = self.depth();
        if self.sink.capacity_us != capacity {
            self.sink.capacity_us = capacity;
            self.sink.track.depth(capacity);
            self.app.log(&format!("output depth in place: {} ms", capacity / 1000));
        }
    }

    /// How much the sink holds: the shallow buffer while the equalizer is tuned, the deep one otherwise.
    fn depth(&self) -> i64 {
        if self.chain.tuning {
            self.shallow_us
        } else {
            BUFFER_US
        }
    }

    /// What the output holds is made again from where the ear is, with the sound, the stages and the
    /// buffer as they are now: for an output that can drop what it holds without a gap worth hearing
    /// (nori-engine's, behind a dip of its volume), so a change is heard at once rather than after the
    /// seconds the deep buffer holds. The rebuild carries whatever was waiting for a boundary or a pause.
    /// Paused, it is made again when the music comes back.
    pub fn resound(&mut self) {
        if self.current.is_none() {
            return;
        }
        if !self.playing {
            self.resound = true;
            return;
        }
        // The place is taken from the clock now: the last turn may have been a burst ago.
        self.follow_clock();
        self.chain.swap_pending = false;
        self.chain.deep_at_next_pause = false;
        self.rebuild_sink();
    }

    /// [`Player::resound`], with the song `id` opened ahead of the ear from `from_ms` (`r`), while the
    /// output played on: nothing waits for the song to open here, the moment the output is emptied. It
    /// is read from where the ear has got to, which the one making it again timed to be `from_ms` or a
    /// little past it; the song is opened again there instead when the ear is on another song, or short
    /// of `from_ms` by more than a moment.
    pub fn resound_from(&mut self, id: String, r: S::Reading, from_ms: i64) {
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
        self.rebuild_sink_from(Some((id, r, from_ms)));
    }

    /// Speed and pitch, which the sink's stage hears live.
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

    /// The output as a stop and a prepare make it again: the engine reset, a new chain for the
    /// settings as they are now, and the song read again from where it is.
    fn rebuild_sink(&mut self) {
        self.rebuild_sink_from(None);
    }

    /// [`Player::rebuild_sink`], the song the ear is on read from `opened` when it was opened ahead for
    /// it ([`Player::resound_from`]).
    fn rebuild_sink_from(&mut self, opened: Option<(String, S::Reading, i64)>) {
        self.resound = false;
        // From where the ear is: inside a held ending, the song before the one the player moved on to.
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

    /// Has the app measure the songs coming up that it has not measured, and asks the engine for its
    /// plan again.
    pub fn measure_ahead(&mut self) {
        let n = measure_ahead(self.app.auto_mix());
        let ids: Vec<String> = self.queue.read(|q| q.upcoming().take(n).map(|i| q.ids()[i].clone()).collect());
        if ids.is_empty() {
            return;
        }
        self.app.measure_ahead(&mut self.tracks, &ids);
        self.engine.replan();
    }

    /// Song `i` would not play (`why`): skipped as the platform's error handler does (the app's run of
    /// failures, or `queue::ErrorRun`), or playback stops there.
    /// Song `i` will not play, whatever its reader says: the queue's rules take it as a song that failed
    /// (skipped, or playback stopped there), as one that would not open.
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

    /// The queue changed here (songs added, removed or moved, shuffle): the planner's window and the
    /// seek bar follow. What the player holds by queue index - the song it is on, the one being read,
    /// the one opening, the streams handed to the output - is found again by its id, nearest to where
    /// it was, since an edit before it moves it. A song opened ahead that is no longer the one after is
    /// let go, and the right one opened when it is due, as media3 drops a period the edit replaced.
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
            if let Some(f) = self.failed.as_mut() {
                f.0 = at(f.0);
            }
            self.stop_after = self.stop_after.map(at);
            let after = self.reading.as_ref().and_then(|r| self.next_of(r.index));
            match self.next.as_mut() {
                Some(n) if moved(&old, &self.ids, n.0).is_some_and(|i| Some(i) == after) => n.0 = after.expect("checked"),
                _ => self.next = None,
            }
        }
        self.sync_queue();
        // What follows the song playing may be another song now, and its ending was planned into the
        // old one: asked again on the next buffer, as the platform player does on a timeline change.
        self.engine.replan();
        let Some(cur) = self.current else { return };
        // A song queued to follow the one playing (Play next, Add to queue onto its end) is fetched and
        // measured now, not when the song playing ends: its mix is planned before then, and with nothing
        // measured of it AutoMix had nothing to mix it by.
        let next = self.next_of(cur).map(|n| self.id_at(n));
        let other_next = next != self.upcoming;
        // Asked again after any edit, the same next song or not: the songs after it, fetched ahead, may
        // be others now. A platform asked for the song it is fetching already goes on with it.
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

    /// The reader has gone on from `cur`, the song the ear is on, into a song that no longer follows it:
    /// the queue was edited after the ending was made.
    pub fn read_astray(&self, cur: usize) -> bool {
        let Some(r) = self.reading.as_ref().map(|r| r.index).or(self.opening.as_ref().map(|o| o.index)) else { return false };
        r != cur && Some(r) != self.next_of(cur)
    }

    /// Repeat off, one or all (`playlist::REPEAT_*`): the player walks the queue that way from now on,
    /// and the plan out of the song playing is asked for again.
    pub fn set_repeat(&mut self, mode: u8) {
        self.queue.set_repeat(mode);
        self.sync_queue();
        self.engine.replan();
    }

    /// The queue as the planner and the seek bar see it: the window (the song before the current one,
    /// then it and those after it, in play order) and every song's length.
    fn sync_queue(&mut self) {
        // As the core hands it over (`playlist_window`): the song before, then eight as the player walks
        // them, repeat included.
        let current = self.current;
        let (window, shuffling, all) = self.queue.read(|q| {
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
            (ids, q.shuffling(), q.ids().to_vec())
        });
        let window = window.iter().map(|id| self.tracks.about(id)).collect();
        self.app.window(window, shuffling);
        let songs: Vec<(String, i64)> = all.into_iter().map(|id| (id.clone(), self.tracks.about(&id).duration_ms)).collect();
        self.tracker.set_queue(songs);
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

    /// The song the player is on: the stream the output's clock has reached.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// The song playback stopped at because it would not play; none once anything was jumped to.
    pub fn stopped_at(&self) -> Option<usize> {
        self.stopped
    }

    pub fn current_id(&self) -> Option<String> {
        self.current.map(|i| self.id_at(i))
    }

    /// The player's position in the song it is on, ms.
    pub fn position_ms(&self) -> i64 {
        if self.position_us == POSITION_NOT_SET {
            return 0;
        }
        let offset = self.current.and_then(|i| self.periods.iter().rev().find(|p| p.index == i)).map_or(0, |p| p.offset_us);
        (self.position_us - offset) / 1000
    }

    /// How long until the output's clock reaches the next song already handed to it, in song time;
    /// `None` when none is.
    pub fn until_next_song_us(&self) -> Option<i64> {
        if self.position_us == POSITION_NOT_SET {
            return None;
        }
        self.periods.iter().map(|p| p.offset_us).filter(|&o| o > self.position_us).min().map(|o| o - self.position_us)
    }

    /// What the seek bar shows: the song heard and the place in it.
    pub fn bar(&mut self) -> Seen {
        let (on, next, pos) = (self.current, self.queue.read(Playlist::next), self.position_ms());
        let heard = self.engine.heard().clone();
        self.tracker.at_index(&heard, self.now_ms, self.playing, on, next, pos)
    }

    /// What the engine says the ear has now.
    pub fn heard(&self) -> &Heard {
        self.engine.heard()
    }

    /// A mix is being heard right now.
    pub fn mixing(&self) -> bool {
        self.engine.heard().mixing
    }

    /// The song the ear is on and the place in it, ms. While an ending is held for a mix (or the mix is
    /// made and not heard yet) the player has been told that ending has played, and is on the next song
    /// already: the ear is still in the ending.
    /// [`Player::ear`], the place read from the output's clock now.
    pub fn ear_now(&mut self) -> Option<(usize, i64)> {
        if self.playing && self.current.is_some() {
            // The last turn may have been a burst ago.
            self.follow_clock();
        }
        self.ear()
    }

    /// [`Player::ear`] as of the last turn.
    pub fn ear(&mut self) -> Option<(usize, i64)> {
        let current = self.current?;
        if self.engine.heard().id.is_some() {
            if let Some(i) = self.bar().index {
                let heard = self.engine.heard();
                // Read at `at_ms`; the sound has moved on since, if it plays.
                let since = if self.playing { (self.now_ms - heard.at_ms).max(0) } else { 0 };
                let ms = (heard.us + since * 1000) / 1000;
                return Some((i, ms));
            }
        }
        Some((current, self.position_ms()))
    }

    /// How much of the ending of song `cur` (the one the ear is on) the output already holds, and what
    /// it was made with: the plan whose hold has begun or that its last buffer went out with
    /// (`Some(None)`: gapless), and `Some(None)` too while it is still being read but past `start_us`,
    /// where a plan starting there would have begun - everything read up to here went out as it is.
    /// `None` while nothing a plan starting at `start_us` would change has been made: the plan is then
    /// simply taken up there.
    pub fn ending_made(&self, cur: usize, start_us: Option<i64>) -> Option<Option<crate::engine::Plan>> {
        let id = self.id_at(cur);
        let reading = self.reading.as_ref().filter(|r| r.index == cur);
        if let Some(made) = self.engine.made(&id) {
            if self.engine.holding() || reading.is_none() {
                return Some(made.cloned());
            }
        }
        let r = reading?;
        (start_us? < r.r.at_us()).then_some(None)
    }

    /// The last song has been read to its end and handed over.
    pub fn source_ended(&self) -> bool {
        self.source_ended
    }

    /// Whether everything has been played to the end.
    pub fn ended(&self) -> bool {
        self.source_ended && self.sink.drained()
    }

    /// The last turn ran out of its budget with the output still taking audio: another turn is due at
    /// once rather than when the output runs low.
    pub fn hungry(&self) -> bool {
        self.hungry
    }

    /// Where the player stands, in words, for a perf report's invariant break: the song it is on and where,
    /// the song being read and whether it waits for its bytes, the one opening, the next one opened, a
    /// failure waiting to be raised, and the transition engine's own account.
    pub fn words(&self) -> String {
        let id = |i: usize| self.queue.read(|q| q.ids().get(i).cloned()).unwrap_or_else(|| "?".into());
        let mut w = format!("{} on {}", if self.playing { "playing" } else { "paused" }, self.current.map_or("nothing".into(), |i| format!("{i} ({})", id(i))));
        if self.position_us != POSITION_NOT_SET {
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

    /// The queue index of the song being read (or opening): past the song playing near its end.
    pub fn reading_index(&self) -> Option<usize> {
        self.reading.as_ref().map(|r| r.index).or(self.opening.as_ref().map(|o| o.index))
    }

    /// The song being read is waiting for its bytes, or still opening.
    pub fn starved(&self) -> bool {
        self.opening.is_some() || self.reading.as_ref().is_some_and(|r| !r.left() && !r.ended && r.waiting)
    }

    /// Nothing can be read on until a song's bytes come: the song being read is waiting for them (or
    /// opening), or it is read to its end and the next one's first bytes are on their way.
    pub fn waiting_for_bytes(&self) -> bool {
        self.starved() || (self.failed.is_none() && self.next.as_ref().is_some_and(|(_, n)| n.is_ok()) && self.reading.as_ref().is_some_and(|r| r.ended && r.waiting && !r.left()))
    }

    /// One turn of the renderer at `now_ms`: the position is read, and the output is offered audio
    /// until it refuses.
    pub fn turn(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
        self.opened();
        if !self.playing {
            return;
        }
        self.follow_clock();
        self.render();
        // The error of a song that would not play surfaces once the song before it has played out, as
        // media3 raises it when playback reaches it.
        if self.failed.is_some() && self.ended() {
            let (n, kind, why) = self.failed.take().expect("checked");
            self.fail(n, kind, why);
        }
    }

    /// The position from the output's clock, and the song it is in.
    fn follow_clock(&mut self) {
        let ended = self.source_ended;
        let at = self.call(|e, d, a| e.position_us(d, a, ended));
        if at != POSITION_NOT_SET {
            self.position_us = at;
            if let Some(p) = self.periods.iter().rev().find(|p| self.position_us >= p.offset_us).copied() {
                // The same song again in a stream of its own: repeat one went round.
                if self.heard_period.is_some_and(|o| o != p.offset_us) && self.current == Some(p.index) {
                    self.loops += 1;
                }
                self.heard_period = Some(p.offset_us);
                self.set_current(p.index);
            }
            // Music is heard: that breaks a run of songs that would not play.
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
            self.sink.write_out();
        }
    }

    fn at_queue_end(&self) -> bool {
        self.reading.as_ref().is_some_and(|r| self.next_of(r.index).is_none())
    }

    /// A buffer to offer, reading on into the next song when this one is read to its end and the player
    /// is close enough to that end.
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
                // Its bytes stopped coming for good: the song fails where it stopped, once what was
                // read of it has played.
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
            // The next song is opened as soon as this one is read to its end: a song that will not play
            // is known then, and a platform's fetch has the most time.
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
            if self.position_us == POSITION_NOT_SET || self.position_us < end - READ_AHEAD_US {
                return false;
            }
            // Still opening, or its first bytes on their way: the song before plays out meanwhile, and
            // the player is woken when they come.
            if self.next.as_mut().is_some_and(|(_, r)| r.as_mut().is_ok_and(|r| !r.ready())) {
                if let Some(r) = self.reading.as_mut() {
                    r.waiting = true;
                }
                return false;
            }
            let Some((_, Ok(next))) = self.next.take() else { unreachable!("an opened song is waiting") };
            // The next song: its format announced, then its first buffer is a new stream, scaled to its
            // own volume from its first sample.
            let (format, duration_us) = (next.format(), next.duration_us());
            self.sink.track.source_bits(next.bits());
            let gain = self.song_gain(n);
            self.configure(n, format);
            self.call(|e, d, a| e.handle_discontinuity(d, a));
            self.engine.set_output_stream_offset_us(end);
            self.engine.set_gain(gain);
            self.reading = Some(Reader::new(n, end, next));
            self.periods.push(Period { index: n, offset_us: end, duration_us, gain });
        }
    }
}

/// How far short of the place a song was opened at ahead of time the ear may be when it is taken, ms:
/// that much of the song is not heard. The one opening it times the switch for the ear to be there.
const OPENED_EARLY_MS: i64 = 40;

/// A song opened ahead from `at` ms, taken to be read from `from_ms` (where the ear is): from `at` when
/// the ear is at most a moment short of it, from `from_ms` when it is past it and the reading can drop what
/// comes before. None when it cannot be taken.
fn taken_from<R: Reading>(mut r: R, at: i64, from_ms: i64) -> Option<(R, i64)> {
    if from_ms < at - OPENED_EARLY_MS {
        return None;
    }
    if from_ms <= at {
        return Some((r, at));
    }
    r.skip_to_ms(from_ms).then_some((r, from_ms))
}

/// Where the song at index `i` of `old` is in `new`: the same id, nearest to where it was (a song can
/// be in the queue twice). None when it was taken out.
fn moved(old: &[String], new: &[String], i: usize) -> Option<usize> {
    let id = old.get(i)?;
    new.iter().enumerate().filter(|(_, n)| *n == id).map(|(j, _)| j).min_by_key(|&j| j.abs_diff(i))
}
