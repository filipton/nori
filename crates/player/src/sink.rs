//! media3's AudioSink with nori's processors ([`Processors`]) over a [`Track`]. The input the chain was
//! given is kept while the track may still play what came of it ([`Kept`]), so a sound change is made
//! from the first frame the track can still replace: the chain goes back to its kept state before
//! there, runs up to it again (the same output), then goes on with the new settings. Nothing is decoded
//! again and no position is guessed.

use std::collections::VecDeque;
use std::ops::Range;

use crate::chain::{Kept, Processors};
use crate::dsp::{Band, Effects, Equalizer};
use crate::engine::Downstream;
use crate::pcm::{Encoding, Format};
use crate::silence::SilenceSkipper;
use crate::sing::{Masker, Placed};
use crate::sound::sound_on;
use crate::speed::{speed_active, SpeedPitch};

/// A buffer this far from its expected timestamp resyncs the clock (media3).
const PTS_TOLERANCE_US: i64 = 200_000;
const LIMITER_RELEASE_MS: f64 = 120.0;
const LIMITER_LOOKAHEAD_MS: f64 = 5.0;
/// Pace changes tracked in flight (a post-mix ramp changes pace every buffer).
const PACES: usize = 512;
/// Input run at a time to reach a splice: how far past the first replaceable frame a splice may land.
const REPLAY_FRAMES: u64 = 256;
/// Kept input reaches this far behind what has played, so a track that replaces from a little before
/// its play head (one that drops what its device holds) finds a kept state there.
const KEPT_BEHIND_US: i64 = 500_000;

/// How long a splice blends what the track held into what replaces it, µs.
pub const BLEND_US: i64 = 5_000;

/// Frame `k` of an `n`-frame blend from `old` into `new`.
pub fn blended(old: f32, new: f32, k: usize, n: usize) -> f32 {
    old + (new - old) * (k + 1) as f32 / (n + 1) as f32
}

/// Sound settings for the equalizer and effects.
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

/// What the chain does to the samples.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainSettings {
    pub sound: Sound,
    pub speed: f32,
    pub pitch: f32,
    pub skip_silence: bool,
    /// The equalizer is in the chain even while flat (all but bit-perfect output).
    pub keep_eq: bool,
    /// Sing: the vocals' level (0 to 1) where a song has a mask; `None` is off.
    pub sing: Option<f32>,
}

impl Default for ChainSettings {
    fn default() -> Self {
        ChainSettings { sound: Sound::default(), speed: 1.0, pitch: 1.0, skip_silence: false, keep_eq: false, sing: None }
    }
}

impl ChainSettings {
    fn eq_in(&self) -> bool {
        self.keep_eq || self.sound.on()
    }
}

/// Why music is made again from the first frame a track can still replace ([`Track::freeze`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remake {
    /// The sound changed: a device holding seconds plays them first.
    Sound,
    /// The ending changed: another song follows, or another mix.
    Ending,
}

/// The device buffer the sink writes into (an AudioTrack, a desktop ring, a simulated track). Data is
/// in the sink's format; the track converts if its device needs another. Frames count sink frames
/// written since the last flush.
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
    /// Frames played since the last flush.
    fn played(&mut self) -> u64;
    /// The first frame that can still be replaced for `why`; the device takes nothing past it until the
    /// next [`Track::cut`].
    fn freeze(&mut self, why: Remake) -> u64;
    /// Drops what was written from frame `at` (at least the frozen frame) on; returns the song frames
    /// written before it. What is written next replaces it: blended in over [`BLEND_US`] where the
    /// device plays on from what was dropped.
    fn cut(&mut self, at: u64) -> f64;
    /// Nothing written is left to play.
    fn is_empty(&self) -> bool;
    /// Drops everything written; the playhead restarts at 0.
    fn flush(&mut self);
    fn play(&mut self);
    fn pause(&mut self);
    /// Bits per sample of the next configured stream's file (0 unknown).
    fn source_bits(&mut self, _bits: u32) {}
    /// Whether the track must reopen for `format` (and the last bits), after everything written has
    /// played: for tracks that play each song in its own format (bit-perfect).
    fn must_reopen(&mut self, _format: Format) -> bool {
        false
    }
}

/// The processors and the buffers they run in. Allocation-free once the buffers have grown.
#[derive(Default)]
struct Runner {
    chain: Processors,
    /// The songs' vocal masks where they are on the timeline, for Sing's masker.
    masks: Vec<Placed>,
    /// The last run's output.
    out: Vec<u8>,
    scratch: Vec<u8>,
    /// Limiter reduction on the last buffer, dB.
    meter_db: f32,
}

impl Runner {
    fn processing(&self) -> bool {
        self.chain.sing.is_some() || self.chain.eq.as_ref().is_some_and(|e| !e.is_identity()) || self.chain.silence.is_some() || self.chain.speed.is_some()
    }

    /// Runs `input` through Sing's masker, equalizer, silence skipping and speed (media3's order) into
    /// `self.out`, each stage reading the last one's output (or `input`) and writing its own. `at`: the
    /// input's timeline position and pace, for the masker; `None` skips it (what it held is already out).
    fn run(&mut self, input: &[u8], float: bool, at: Option<(i64, f64)>) {
        let (mut out, mut spare) = (std::mem::take(&mut self.out), std::mem::take(&mut self.scratch));
        let mut made = false;
        self.meter_db = 0.0;
        if let (Some(m), Some((pts, pace))) = (self.chain.sing.as_mut(), at) {
            out.clear();
            m.process(input, pts, pace, &self.masks, &mut out);
            made = true;
        }
        if let Some(eq) = self.chain.eq.as_mut().filter(|e| !e.is_identity()) {
            let from = if made { &out[..] } else { input };
            // Every byte is written over: only a longer input grows it.
            spare.resize(from.len(), 0);
            eq.process_bytes(from, &mut spare, float);
            std::mem::swap(&mut out, &mut spare);
            self.meter_db = eq.gain_reduction_db();
            made = true;
        }
        let mut stage = |process: &mut dyn FnMut(&[u8], &mut Vec<u8>)| {
            spare.clear();
            process(if made { &out } else { input }, &mut spare);
            std::mem::swap(&mut out, &mut spare);
            made = true;
        };
        if let Some(s) = self.chain.silence.as_mut() {
            stage(&mut |i, o| s.process(i, o));
        }
        if let Some(s) = self.chain.speed.as_mut() {
            stage(&mut |i, o| s.process(i, o));
        }
        if !made {
            out.clear();
            out.extend_from_slice(input);
        }
        (self.out, self.scratch) = (out, spare);
    }

    /// Runs `data` through speed, if it is in the chain.
    fn speed_up(&mut self, data: &mut Vec<u8>) {
        if let Some(s) = self.chain.speed.as_mut() {
            let mut next = std::mem::take(&mut self.scratch);
            next.clear();
            s.process(data, &mut next);
            std::mem::swap(data, &mut next);
            self.scratch = next;
        }
    }

    /// What silence skipping holds, through speed, into `self.out`; with `end`, what speed holds too.
    fn drain_stages(&mut self, end: bool) {
        let mut out = std::mem::take(&mut self.out);
        out.clear();
        if let Some(s) = self.chain.silence.as_mut() {
            s.end_of_stream(&mut out);
        }
        self.speed_up(&mut out);
        if let Some(sp) = self.chain.speed.as_mut().filter(|_| end) {
            sp.end_of_stream(&mut out);
        }
        self.out = out;
    }
}

/// The end of the queue through the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// Not reached.
    Open,
    /// Reached, and the chain is drained once the kept input has all been run (again, after a splice).
    Due,
    Drained,
}

/// The song time of the track's first frame, as the buffers' timestamps set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    /// The next buffer sets it (a new track, or a flush).
    Unknown,
    /// A buffer more than [`PTS_TOLERANCE_US`] off moves it.
    At(i64),
    /// A discontinuity was announced: the next buffer moves it to its own time.
    Moved(i64),
}

/// media3's AudioSink with nori's processors, over a [`Track`]. Its clock is media3's: the first
/// buffer's timestamp after a flush, shifted when a buffer arrives more than [`PTS_TOLERANCE_US`] off
/// (or after a discontinuity), plus the song time the track has played. The transition engine relies
/// on this: a mix is stamped in the next song's time and the clock jumps when it is offered.
///
/// Allocation-free once its buffers have grown.
pub struct Sink<T: Track> {
    pub format: Option<Format>,
    /// Outputs opened.
    pub opens: u32,
    /// Format changes after the first open.
    pub rebuilds: usize,
    /// Buffer depth.
    pub capacity_us: i64,
    settings: ChainSettings,
    runner: Runner,
    kept: Kept,
    /// Input frames run through the chain since the flush: behind the kept input's end after a splice,
    /// until [`Sink::fill`] has run the rest again.
    run: u64,
    /// Song frames of that input.
    run_media: f64,
    /// Output frames the chain made since the flush (written or pending).
    made: u64,
    end: End,
    start: Start,
    /// Song frames submitted since the clock's reference ([`Downstream::media_pace`] weighted).
    submitted_frames: f64,
    /// Current song frames per frame.
    pace: f64,
    /// (submitted song frame, pace) where each pace began, for [`Sink::pace_heard`].
    paces: VecDeque<(f64, f64)>,
    /// Buffers more than [`PTS_TOLERANCE_US`] off (each a stutter on a phone).
    pub timestamp_jumps: usize,
    /// Bytes left of a partly taken buffer, whose rest must be offered next (media3 throws otherwise).
    /// The rest may come from other memory (the transition engine copies what was not taken).
    owed: Option<usize>,
    /// Format to reopen the track with once it has drained.
    reopen: Option<Format>,
    /// Processed audio waiting for room (from `pending_pos`), its song time, and song time not yet
    /// attached to output.
    pending: Vec<u8>,
    pending_pos: usize,
    pending_media: f64,
    carry: f64,
    /// The source ended: running dry is the end, not a gap.
    pub source_ended: bool,
    /// Largest limiter reduction seen, dB.
    pub gain_reduction_db: f32,
    pub track: T,
}

impl<T: Track> Sink<T> {
    pub fn new(capacity_us: i64, settings: ChainSettings, track: T) -> Sink<T> {
        Sink {
            format: None,
            opens: 0,
            rebuilds: 0,
            capacity_us,
            settings,
            runner: Runner::default(),
            kept: Kept::default(),
            run: 0,
            run_media: 0.0,
            made: 0,
            end: End::Open,
            start: Start::Unknown,
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
            source_ended: false,
            gain_reduction_db: 0.0,
            track,
        }
    }

    /// Back to no format over the same track: the next configure opens it and builds the chain.
    pub fn reset(&mut self) {
        self.format = None;
        self.opens = 0;
        self.rebuilds = 0;
        self.runner.chain = Processors::default();
        self.flush();
        self.timestamp_jumps = 0;
        self.gain_reduction_db = 0.0;
    }

    /// The output frame heard now and the output's rate.
    pub fn ear(&mut self) -> Option<(u64, u32)> {
        Some((self.track.played(), self.format?.rate))
    }

    pub fn settings(&self) -> &ChainSettings {
        &self.settings
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

    pub fn queued_us(&self) -> i64 {
        self.format.map_or(0, |f| f.us(self.track.queued_bytes()))
    }

    /// Builds the chain for the format and settings.
    fn build_chain(&mut self) {
        let Some(f) = self.format else { return };
        let s = &self.settings;
        self.runner.chain = Processors {
            sing: s.sing.map(|level| Masker::new(f.rate, f.channels, f.encoding, level)),
            eq: s.eq_in().then(|| {
                let mut eq = Equalizer::new(f.rate, f.channels);
                s.sound.apply(&mut eq);
                eq
            }),
            silence: s.skip_silence.then(|| SilenceSkipper::new(f.rate, f.channels, f.encoding == Encoding::Float)),
            speed: speed_active(s.speed, s.pitch).then(|| {
                let mut sp = SpeedPitch::new(f.rate, f.channels, f.encoding);
                sp.set(s.speed, s.pitch);
                sp
            }),
        };
    }

    /// Whether the equalizer is in the chain.
    pub fn chain_in(&self) -> bool {
        self.runner.chain.eq.is_some()
    }

    /// Limiter reduction on the last buffer, dB.
    pub fn meter_db(&self) -> f32 {
        self.runner.meter_db
    }

    /// The compressor's largest gain reduction in the last buffer, dB; 0 without one.
    pub fn compression_db(&self) -> f32 {
        self.runner.chain.eq.as_ref().map_or(0.0, |e| e.compression_db())
    }

    /// New settings, heard from the first frame the track can still replace. Returns where they start:
    /// input frame and output frame since the flush.
    pub fn change(&mut self, to: ChainSettings) -> Option<(u64, u64)> {
        if to == self.settings {
            return None;
        }
        let at = self.splice();
        self.apply(to);
        // A later change made again from here runs the new settings, not those before.
        if at.is_some() {
            self.kept.mark_changed(self.run, self.made, self.run_media, &self.runner.chain);
        }
        at
    }

    /// Scales the input kept in each timeline range by its ratio (ReplayGain changed), heard from the
    /// first frame the track can still replace.
    pub fn rescale(&mut self, ranges: &[(Range<i64>, f32)]) -> Option<(u64, u64)> {
        let f = self.format?;
        let at = self.splice();
        for (pts, ratio) in ranges {
            self.kept.rescale(self.run, pts.clone(), *ratio, f.encoding);
        }
        at
    }

    /// The songs' vocal masks where they are on the timeline changed: heard from the first frame the track
    /// can still replace if one comes where input has already been run.
    pub fn set_masks(&mut self, masks: &[Placed]) -> Option<(u64, u64)> {
        let same = |a: &Placed, b: &Placed| a.at.start == b.at.start && std::sync::Arc::ptr_eq(&a.mask, &b.mask);
        let reached = self.kept.last_pts().unwrap_or(i64::MIN);
        let late = masks.iter().any(|m| m.at.start <= reached && !self.runner.masks.iter().any(|o| same(o, m)));
        let gone = self.runner.masks.iter().any(|o| o.at.start <= reached && !masks.iter().any(|m| same(o, m)));
        let at = (self.runner.chain.sing.is_some() && (late || gone)).then(|| self.splice()).flatten();
        self.runner.masks.clear();
        self.runner.masks.extend_from_slice(masks);
        at
    }

    /// The chain goes on with `to` from where it is.
    fn apply(&mut self, to: ChainSettings) {
        let Some(f) = self.format else {
            self.settings = to;
            return;
        };
        let live = self.made > 0;
        let chain = &mut self.runner.chain;
        match (chain.sing.as_mut(), to.sing) {
            (Some(m), level) => m.set_level(level.unwrap_or(1.0)),
            (None, Some(level)) => chain.sing = Some(Masker::new(f.rate, f.channels, f.encoding, level)),
            // At full level it stays: leaving would drop what it holds.
            (None, None) => {}
        }
        match chain.eq.as_mut() {
            // A flat equalizer stays until the next flush. The same sound again changes nothing.
            Some(eq) => {
                // Flat, it may not have run yet: a change fades in all the same.
                if live {
                    eq.continuing();
                }
                to.sound.apply(eq)
            }
            None if to.eq_in() => {
                let mut eq = Equalizer::new(f.rate, f.channels);
                if live {
                    eq.continuing();
                }
                to.sound.apply(&mut eq);
                chain.eq = Some(eq);
            }
            None => {}
        }
        match chain.speed.as_mut() {
            // At 1x it stays too: leaving would pad what it holds with silence.
            Some(sp) => sp.set(to.speed, to.pitch),
            None if speed_active(to.speed, to.pitch) => {
                let mut sp = SpeedPitch::new(f.rate, f.channels, f.encoding);
                sp.set(to.speed, to.pitch);
                chain.speed = Some(sp);
            }
            None => {}
        }
        match (to.skip_silence, chain.silence.is_some()) {
            (true, false) => chain.silence = Some(SilenceSkipper::new(f.rate, f.channels, f.encoding == Encoding::Float)),
            (false, true) => {
                // What it holds goes on through speed.
                self.runner.drain_stages(false);
                self.runner.chain.silence = None;
                self.made_output(0.0);
            }
            _ => {}
        }
        self.settings = to;
    }

    /// Puts the chain back where the track can still change: at the first frame it can replace, run
    /// again from the kept state before it (the same output). The track drops what it holds from
    /// there; [`Sink::fill`] runs the kept input after it again.
    fn splice(&mut self) -> Option<(u64, u64)> {
        let written = self.written()?;
        // Nothing kept of this stream yet (its first buffer is still to come): nothing to make again,
        // and the track is left as it is.
        if !self.kept.has_marks() {
            return None;
        }
        let from = self.track.freeze(Remake::Sound).min(written);
        // Kept input starts later than that only if the track reaches back further than it said.
        let k = self.kept.mark_where(|m| m.out <= from).unwrap_or(0);
        let back = self.run_again(k, u64::MAX, from);
        self.back_at(back, written);
        Some((self.run, self.made))
    }

    /// Drops the output from the input at timeline position `pts` on, if it is kept, unstretched, and
    /// the track can still replace it; the chain is left there and the input after it forgotten, for
    /// other input to follow.
    pub fn cut_at(&mut self, pts: i64) -> bool {
        let (Some(f), Some(written)) = (self.format, self.written()) else { return false };
        let Some(at) = self.kept.frame_at(pts, f.rate) else { return false };
        // In input still to be run (after a splice) nothing was made of it yet.
        if at < self.run {
            let Some(k) = self.kept.mark_where(|m| m.frame <= at) else { return false };
            let from = self.track.freeze(Remake::Ending).min(written);
            let keep = self.runner.chain.clone();
            // No further than what was written: input before the cut not written yet is run by `fill`.
            let back = self.run_again(k, at, written);
            if back.1 < from {
                // Already played, or about to be.
                self.runner.chain = keep;
                self.track.cut(written);
                return false;
            }
            self.back_at(back, written);
            // The kept state may be from before the settings last changed: they are applied from here.
            self.apply(self.settings.clone());
        }
        self.submitted_frames -= self.kept.media_from(at);
        while self.paces.back().is_some_and(|p| p.0 > self.submitted_frames) {
            self.paces.pop_back();
        }
        self.kept.forget_from(at);
        // The offer partly taken is dropped with the input after the cut, and the clock is as `pts` is
        // stamped there (a mix dropped here had moved it to the next song's time).
        self.owed = None;
        self.start = Start::At(pts - (self.submitted_frames * 1_000_000.0 / f.rate as f64) as i64);
        self.end = End::Open;
        self.source_ended = false;
        true
    }

    /// Output frames given to the track since the flush.
    fn written(&self) -> Option<u64> {
        let fb = self.format?.frame_bytes();
        Some(self.made - ((self.pending.len() - self.pending_pos) / fb) as u64)
    }

    /// Runs the chain again from kept state `k` up to input frame `until` or output frame `reach`,
    /// whichever comes first; returns where it stopped: input frame, output frame, song frames.
    fn run_again(&mut self, k: usize, until: u64, reach: u64) -> (u64, u64, f64) {
        let Some(f) = self.format else { return (self.run, self.made, self.run_media) };
        let (fb, float) = (f.frame_bytes(), f.encoding == Encoding::Float);
        let m = self.kept.mark_at(k);
        let (mut frame, mut out, mut media) = (m.frame, m.out, m.media);
        self.runner.chain.clone_from(&self.kept.mark_at(k).chain);
        self.runner.out.clear();
        while frame < until && out < reach {
            let Some((piece, end)) = self.kept.piece(frame) else { break };
            let to = end.min(until).min(frame + REPLAY_FRAMES);
            self.runner.run(self.kept.frames(frame, to), float, Some(piece.at(frame, f.rate)));
            out += (self.runner.out.len() / fb) as u64;
            media += (to - frame) as f64 * piece.pace;
            frame = to;
        }
        (frame, out, media)
    }

    /// The chain is at input frame `frame`, having made `out` output frames from `media` song frames:
    /// the track drops what was written from there, and output made again past `written` (it was
    /// pending) stays.
    fn back_at(&mut self, (frame, out, media): (u64, u64, f64), written: u64) {
        let fb = self.format.map_or(1, |f| f.frame_bytes());
        self.kept.forget_after(frame);
        let over = out.saturating_sub(written) as usize;
        let written_media = self.track.cut(out - over as u64);
        self.pending.clear();
        self.pending_pos = 0;
        self.pending_media = 0.0;
        self.carry = media - written_media;
        if over > 0 {
            let mut last = std::mem::take(&mut self.runner.out);
            last.drain(..last.len() - over * fb);
            self.push_pending(&last);
            self.runner.out = last;
        }
        self.run = frame;
        self.run_media = media;
        self.made = out;
        if self.end == End::Drained {
            self.end = End::Due;
        }
    }

    /// Runs kept input through the chain that has not been run since the last splice, writing as
    /// the track takes it. True once everything is written.
    pub fn fill(&mut self) -> bool {
        let Some(f) = self.format else { return true };
        let float = f.encoding == Encoding::Float;
        loop {
            if !self.write_pending() {
                return false;
            }
            let Some((piece, end)) = self.kept.piece(self.run) else {
                if self.end == End::Due {
                    self.end = End::Drained;
                    self.drain();
                    continue;
                }
                return true;
            };
            self.mark();
            let media = (end - self.run) as f64 * piece.pace;
            self.carry += media;
            self.runner.run(self.kept.frames(self.run, end), float, Some(piece.at(self.run, f.rate)));
            self.run_media += media;
            self.run = end;
            self.made_output(0.0);
        }
    }

    /// Keeps the chain's state before the next input, now and then, and lets go of what has played.
    fn mark(&mut self) {
        let Some(f) = self.format else { return };
        if self.kept.mark(self.run, self.made, self.run_media, &self.runner.chain) {
            let behind = (f.rate as i64 * KEPT_BEHIND_US / 1_000_000) as u64;
            self.kept.trim(self.track.played().saturating_sub(behind));
        }
    }

    /// Counts the chain's last output and writes it with `media` more song frames: straight from the
    /// chain as far as the track has room, the rest queued.
    fn made_output(&mut self, media: f64) {
        let fb = self.format.map_or(1, |f| f.frame_bytes());
        self.carry += media;
        let out = std::mem::take(&mut self.runner.out);
        self.made += (out.len() / fb) as u64;
        let mut from = 0;
        if !self.pending_left() && !out.is_empty() {
            from = self.room_bytes().min(out.len());
            if from > 0 {
                let media = self.carry * from as f64 / out.len() as f64;
                self.carry -= media;
                self.track.write(&out[..from], media);
            }
        }
        if from < out.len() {
            self.push_pending(&out[from..]);
        }
        self.runner.out = out;
        self.gain_reduction_db = self.gain_reduction_db.max(self.runner.meter_db);
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

    pub fn play(&mut self) {
        self.track.play();
    }

    pub fn pause(&mut self) {
        self.track.pause();
    }

    /// Drops everything queued, processed and kept; the clock restarts at the next buffer.
    pub fn flush(&mut self) {
        // A reopen waiting for the old format to play out stays: there is nothing left to play, so the next
        // buffer reopens at once (the transition engine already counts the output as in that format).
        self.track.flush();
        self.pending.clear();
        self.pending_pos = 0;
        self.pending_media = 0.0;
        self.carry = 0.0;
        self.owed = None;
        self.start = Start::Unknown;
        self.submitted_frames = 0.0;
        self.paces.clear();
        self.source_ended = false;
        self.restart_counts();
        // Processors that stayed in after being turned off go now.
        let s = &self.settings;
        let chain = &mut self.runner.chain;
        if (chain.sing.is_some(), chain.eq.is_some(), chain.silence.is_some(), chain.speed.is_some()) != (s.sing.is_some(), s.eq_in(), s.skip_silence, speed_active(s.speed, s.pitch)) {
            self.build_chain();
            return;
        }
        if let Some(m) = chain.sing.as_mut() {
            m.reset();
        }
        if let Some(eq) = chain.eq.as_mut() {
            eq.reset();
        }
        if let Some(s) = chain.silence.as_mut() {
            s.flush();
        }
        if let Some(s) = chain.speed.as_mut() {
            s.flush();
        }
    }

    /// The track counts from zero again.
    fn restart_counts(&mut self) {
        self.kept.restart(0, self.format.map_or(1, |f| f.frame_bytes()));
        self.run = 0;
        self.run_media = 0.0;
        self.made = 0;
        self.end = End::Open;
    }

    /// End of the queue: drains the chain once the kept input has all been run.
    pub fn end_of_stream(&mut self) {
        self.end = End::Due;
        self.fill();
    }

    /// What Sing's masker holds, through the rest of the chain; the limiter's look-ahead pushed out with
    /// silence; then what silence skipping and speed hold.
    fn drain(&mut self) {
        let Some(f) = self.format else { return };
        let float = f.encoding == Encoding::Float;
        if let Some(m) = self.runner.chain.sing.as_mut() {
            let mut tail = Vec::new();
            m.end(&self.runner.masks, &mut tail);
            self.runner.run(&tail, float, None);
            self.made_output(0.0);
        }
        let held = self.runner.chain.eq.as_ref().filter(|e| !e.is_identity()).map_or(0, Equalizer::delay_frames);
        if held > 0 {
            self.runner.run(&vec![0u8; held * f.frame_bytes()], float, None);
            self.made_output(0.0);
        }
        self.runner.drain_stages(true);
        self.made_output(0.0);
    }

    /// Output is waiting for room in the track, or kept input to be run again.
    pub fn pending(&self) -> bool {
        self.pending_left() || self.run < self.kept.end() || self.end == End::Due
    }

    /// Everything written has played.
    pub fn drained(&self) -> bool {
        self.track.is_empty() && !self.pending()
    }

    /// Waiting for the track to drain before reopening for the next stream.
    pub fn reopening(&self) -> bool {
        self.reopen.is_some()
    }

    /// Reopens the track for a waiting stream once the previous one has played out (the clock restarts,
    /// as with a new AudioTrack). False while it still plays.
    fn reopened(&mut self) -> bool {
        let Some(f) = self.reopen else { return true };
        self.fill();
        if !self.drained() {
            return false;
        }
        self.reopen = None;
        self.rebuilds += 1;
        self.format = Some(f);
        self.track.open(f);
        self.build_chain();
        self.start = Start::Unknown;
        self.submitted_frames = 0.0;
        self.paces.clear();
        self.restart_counts();
        true
    }
}

impl<T: Track> Downstream for Sink<T> {
    fn configure(&mut self, f: Format) {
        self.opens += 1;
        // Input of the last format still to be run again (after a sound change) is run first.
        if self.format.is_some() && (self.run < self.kept.end() || self.track.must_reopen(f)) {
            self.reopen = Some(f);
            return;
        }
        if self.format != Some(f) {
            if self.format.is_some() {
                self.rebuilds += 1;
            }
            self.format = Some(f);
            self.track.open(f);
            self.build_chain();
            // Frames of another format are not run again: the kept input starts here.
            self.kept.restart(self.run, f.frame_bytes());
        }
    }

    fn handle_buffer(&mut self, data: &[u8], from: usize, pts_us: i64) -> (bool, usize) {
        if !self.reopened() {
            return (false, 0);
        }
        let f = self.format.expect("configured before the first buffer");
        let fb = f.frame_bytes();
        let key = data.len() - from;
        let continuing = self.owed.take();
        if let Some(owed) = continuing {
            assert_eq!(owed, key, "offered another buffer while one was only partly taken (media3 throws here)");
        }
        if continuing.is_none() {
            self.start = match self.start {
                Start::Unknown => Start::At(pts_us.max(0)),
                Start::At(s) | Start::Moved(s) => {
                    let expected = s + (self.submitted_frames * 1_000_000.0 / f.rate as f64) as i64;
                    let moved = matches!(self.start, Start::Moved(_));
                    let jumped = !moved && (expected - pts_us).abs() > PTS_TOLERANCE_US;
                    self.timestamp_jumps += jumped as usize;
                    Start::At(if moved || jumped { s + pts_us - expected } else { s })
                }
            };
        }
        if !self.fill() {
            self.owed = Some(key);
            return (false, 0);
        }
        let input = &data[from..];
        // Timeline position of the input's first frame.
        let pts = pts_us + ((from / fb) as f64 * self.pace * 1_000_000.0 / f.rate as f64) as i64;
        if self.runner.processing() {
            self.note_pace();
            let media = (input.len() / fb) as f64 * self.pace;
            self.submitted_frames += media;
            self.mark();
            self.kept.keep(input, self.pace, pts);
            self.runner.run(input, f.encoding == Encoding::Float, Some((pts, self.pace)));
            self.run += (input.len() / fb) as u64;
            self.run_media += media;
            self.made_output(media);
            return (true, input.len());
        }
        let n = self.room_bytes().min(input.len()) / fb * fb;
        if n > 0 {
            self.note_pace();
            let media = (n / fb) as f64 * self.pace;
            self.mark();
            self.kept.keep(&input[..n], self.pace, pts);
            self.track.write(&input[..n], media);
            self.submitted_frames += media;
            self.run += (n / fb) as u64;
            self.run_media += media;
            self.made += (n / fb) as u64;
        }
        if n < input.len() {
            self.owed = Some(key - n);
            return (false, n);
        }
        (true, n)
    }

    fn handle_discontinuity(&mut self) {
        if let Start::At(s) = self.start {
            self.start = Start::Moved(s);
        }
    }

    fn media_pace(&mut self, pace: f64) {
        self.pace = if pace.is_finite() && pace > 0.0 { pace } else { 1.0 };
    }

    fn position_us(&mut self, _source_ended: bool) -> Option<i64> {
        let (Some(f), Start::At(s) | Start::Moved(s)) = (self.format, self.start) else { return None };
        Some(s + (self.track.played_media() * 1_000_000.0 / f.rate as f64) as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::AudioTrack;

    const F: Format = Format { rate: 1000, channels: 1, encoding: Encoding::Pcm16 };

    fn ramp(from: i16, n: i16) -> Vec<u8> {
        (from..from + n).flat_map(|v| v.to_le_bytes()).collect()
    }

    /// Made again from where the output held music of an earlier change still to be run: the part
    /// before the cut is run and written, the rest dropped.
    #[test]
    fn cut_within_input_still_to_run_again() {
        let mut sink = Sink::new(10_000_000, ChainSettings { keep_eq: true, ..ChainSettings::default() }, AudioTrack::new());
        sink.configure(F);
        let (a, b, c) = (ramp(0, 3000), ramp(3000, 3000), ramp(6000, 3000));
        for (k, buf) in [&a, &b, &c].into_iter().enumerate() {
            assert!(sink.handle_buffer(buf, 0, k as i64 * 3_000_000).0);
        }
        // All of it is to be run again, then all after 4.5 s dropped for other music.
        sink.change(ChainSettings { speed: 2.0, keep_eq: true, ..ChainSettings::default() });
        assert!(sink.cut_at(4_500_000));
        assert!(sink.fill());
        let written = sink.track.queued_bytes() / 2;
        assert!((2_200..=2_300).contains(&written), "4.5 s at twice the speed: {written} frames");
    }

    /// Regression: a flush dropped the wait to reopen for another rate, and that stream then played into
    /// the old format (too slow or too fast). Flushed, the track has nothing left to play out: it reopens.
    #[test]
    fn flush_reopens_for_the_waiting_format() {
        let mut sink = Sink::new(10_000_000, ChainSettings::default(), AudioTrack::new());
        sink.configure(F);
        assert!(sink.handle_buffer(&ramp(0, 3000), 0, 0).0);
        let other = Format { rate: 2000, ..F };
        sink.configure(other);
        assert!(sink.reopening(), "waits for the old rate to play out");
        sink.flush();
        assert!(sink.handle_buffer(&ramp(0, 100), 0, 0).0);
        assert_eq!(sink.format, Some(other), "the track is open at the new rate");
    }

    #[test]
    fn second_change_goes_on() {
        use crate::pipeline::Track;
        let mut sink = Sink::new(10_000_000, ChainSettings { keep_eq: true, ..ChainSettings::default() }, AudioTrack::new());
        sink.configure(F);
        for k in 0..3 {
            assert!(sink.handle_buffer(&ramp(k * 3000, 3000), 0, k as i64 * 3_000_000).0);
        }
        sink.track.play();
        sink.advance(2_000_000);
        sink.change(ChainSettings { speed: 2.0, keep_eq: true, ..ChainSettings::default() });
        assert!(sink.fill());
        // 1000 frames of input played in 500: 6000 left at 1x.
        sink.advance(500_000);
        sink.change(ChainSettings { keep_eq: true, ..ChainSettings::default() });
        assert!(sink.fill());
        let total = sink.track.played() + sink.track.queued_bytes() as u64 / 2;
        assert!((8_400..=8_600).contains(&total), "{total} frames made");
    }
}
