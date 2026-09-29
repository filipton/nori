//! Transitions between tracks inside one output stream: crossfades and AutoMix (beat-matched, with
//! bass swap, filter sweep and tempo stretch). Sits between the decoder and the output ([`Downstream`]).
//!
//! Audio passes through until the outgoing track reaches the plan's start, then is held (up to the
//! transition's length; the rest of the ending is dropped). When the next track begins, its opening is
//! mixed into the hold.
//!
//! The output format is latched by the first PCM stream and other streams are resampled to it. With
//! [`TransitionEngine::follow_rate`], a stream nothing is mixed into reopens the output at its own
//! format instead, unless the converter can carry on seamlessly from the previous stream. With
//! [`TransitionEngine::lock_rate`] off (bit-perfect) every stream passes through native.
//!
//! Decoded audio of unanalysed tracks is fed to the streaming analyser. During a transition the
//! reported position runs ahead of what is audible, which is tracked in [`Heard`].

use std::collections::VecDeque;

use crate::automix::analysis::Analyzer;
use crate::automix::mixer::Mixer;
use crate::automix::resample::Resampler;
use crate::pcm::{mix_raw, ByteStretcher, Format};

/// Output left below this lets a held ending go unmixed.
const DRY_US: i64 = 1_500_000;
/// A hold younger than this is exempt from [`DRY_US`] (a seek into a transition starts with no runway).
const HOLD_GRACE_MS: i64 = 10_000;
/// Maximum gain applied to a song's buffers ([`TransitionEngine::set_gain`]): +24 dB.
const MAX_GAIN: f32 = 16.0;
/// How long a `None` plan is trusted before asking again.
const NULL_PLAN_RETRY_MS: i64 = 2_000;

/// Song time covered by `frames` of output at `pace` song frames each, µs.
fn span_us(frames: usize, pace: f64, out: Format) -> i64 {
    (frames as f64 * pace * 1_000_000.0 / out.rate as f64).round() as i64
}

/// A transition from one track into the next, from the planner.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub incoming_id: String,
    pub out_start_us: i64,
    pub duration_us: i64,
    pub in_skip_us: i64,
    /// `automix::mixer::params(plan)`.
    pub mixer: Vec<f32>,
    pub tempo_ratio: f32,
    pub keep_pitch: bool,
    pub ramp_us: i64,
    /// Capture this much outgoing audio and loop it for `duration_us`; 0 captures the full duration.
    pub out_loop_us: i64,
}

impl Plan {
    fn stretching(&self) -> bool {
        (self.tempo_ratio - 1.0).abs() > 1e-4
    }
}

/// A stream's format as the decoder announces it. `format` is `None` for non-PCM (offload,
/// passthrough), which cannot be mixed or converted.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamFormat {
    pub id: Option<String>,
    pub format: Option<Format>,
}

/// The real output below. Called only from the engine's thread.
pub trait Downstream {
    /// A platform token for a format, handed back to [`Downstream::configure`].
    type Config: Clone;
    /// Opens the output for `config`; `format` is `None` for non-PCM.
    fn configure(&mut self, config: &Self::Config, format: Option<Format>);
    /// Offers `data[from..]` at `pts_us`; returns (all taken, bytes taken). A partly taken buffer is
    /// offered again as the same memory with `from` advanced, so an output can check it.
    fn handle_buffer(&mut self, data: &[u8], from: usize, pts_us: i64) -> (bool, usize);
    fn handle_discontinuity(&mut self);
    /// Song frames per output frame from now on (not 1 while the incoming song is stretched), so the
    /// output's clock counts song time.
    fn media_pace(&mut self, _pace: f64) {}
    /// Playback position, µs; `None` before there is one.
    fn position_us(&mut self, source_ended: bool) -> Option<i64>;
}

/// What the engine asks of the platform. Called on the engine's thread; must be quick.
pub trait Host {
    /// The transition out of `outgoing_id`, or `None` for gapless.
    fn plan_for(&mut self, outgoing_id: &str) -> Option<Plan>;
    /// `Some(length in ms, 0 if unknown)` if `song_id` still needs analysing; sizes the analyser up front.
    fn wants_analysis(&mut self, song_id: &str) -> Option<u64>;
    /// The analyser finished `song_id`: `frames` at `rate`, `channels` wide.
    fn analysed(&mut self, song_id: &str, analyzer: Analyzer, channels: usize, frames: u64, rate: u32);
    /// [`Heard`] started or stopped differing from the player's position, or its mixing flag changed.
    fn heard_changed(&mut self) {}
    fn log(&mut self, _message: &str) {}
    /// Monotonic clock, ms.
    fn now_ms(&self) -> i64;
}

/// What is audible while the reported position runs ahead of it: the song whose held ending plays and
/// the position in it (song time). The seek bar and title show this.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Heard {
    /// `None` whenever the player's position is what is audible.
    pub id: Option<String>,
    pub us: i64,
    /// [`Host::now_ms`] when `us` was read.
    pub at_ms: i64,
    /// Position in the held song where the next song becomes the louder.
    pub until_us: i64,
    /// A mix is audible now.
    pub mixing: bool,
    /// The song being mixed in, where playback lands in it at takeover, and its tempo ratio in the mix.
    pub next_id: Option<String>,
    pub next_from_us: i64,
    pub next_rate: f32,
    /// The song being mixed out of, and where in it the next song takes over.
    pub from_id: Option<String>,
    pub audible_us: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pass,
    Hold,
    Mix,
}

/// A queued piece of output.
struct Chunk {
    data: Vec<u8>,
    pos: usize,
    pts_us: i64,
    resync: bool,
    /// First chunk of a mix: measure the output clock's jump when it is offered ([`TransitionEngine::drain`]).
    measure: bool,
    /// Stream offset of the single song it holds (`None` for a mix), for [`TransitionEngine::rescale`].
    stream_us: Option<i64>,
    /// Song frames per frame ([`Downstream::media_pace`]).
    pace: f64,
}

/// A format announced by decode-ahead, armed once its buffers flow.
struct Staged<C> {
    id: Option<String>,
    format: Option<Format>,
    config: C,
}

pub struct TransitionEngine<C: Clone> {
    /// The output format; `None` before the first stream or while it is not PCM.
    out: Option<Format>,
    /// Applied to the output once what is queued has drained.
    pending_config: Option<(Option<Format>, C)>,

    /// The song of the last configure (may be decode-ahead).
    current_id: Option<String>,
    /// The song flowing now, from the last discontinuity; never from decode-ahead.
    playing_id: Option<String>,
    /// Flushed and nothing has flowed since: the next configure is the stream about to play.
    fresh: bool,
    /// A discontinuity without a mix and nothing flowed since: the next new stream is the one about to
    /// flow (media3 announces a stream with its first buffer, after the discontinuity).
    awaiting_stream: bool,
    /// A mix began before its incoming stream was announced (media3's order): (plan, late µs), entered
    /// once the incoming format is known.
    awaiting_incoming: Option<(Plan, i64)>,
    offset_us: i64,

    phase: Phase,
    plan: Option<Plan>,
    plan_for: Option<String>,
    replan_wanted: bool,
    last_null_at: i64,
    /// The last song whose ending went out and its plan (`None`: gapless).
    made: Option<(String, Option<Plan>)>,
    tail: Vec<u8>,
    /// Hold capacity for this transition, bytes.
    tail_limit: usize,
    tail_len: usize,
    /// Bytes of the hold that are the song's own audio; the rest is silence padding a song that ended early.
    tail_heard: usize,
    tail_read: usize,
    /// Output timestamp the hold began at.
    held_from_us: Option<i64>,
    held_at: i64,
    /// The held song and its stream offset.
    held_id: Option<String>,
    held_offset_us: i64,
    /// How far into the transition the hold began (non-zero after a seek into it).
    late_us: i64,
    /// Time from the mix becoming audible to the incoming song becoming the louder (`mixer::crossover_ms`
    /// less `late_us`).
    takeover_us: i64,
    /// Outgoing audio swallowed into the hold, µs; reported as played.
    held_us: i64,
    /// The last position reported; never goes backwards. `i64::MIN` before any.
    reported: i64,
    /// The output's clock jumps to the incoming song's time when the first mixed chunk is offered. Until
    /// the clock reaches `shift_until_us`, audible = clock - `shift_us`.
    shift_us: i64,
    shift_until_us: Option<i64>,
    /// Output timestamp the queued mix runs to.
    mixed_end_us: Option<i64>,
    /// Output timestamp the mix is audible from.
    mix_from_us: Option<i64>,
    skip_left: usize,
    resync_next: bool,
    measure_next: bool,
    /// The engine's own clock for mixed and stretched audio, in the incoming song's time.
    synthetic_pts_us: Option<i64>,
    /// Stream time of the first incoming sample of a stretched mix (after the skip).
    mix_in_pts_us: Option<i64>,
    /// Song frames per frame of the stretcher's last output, and the song time the last frames of a
    /// just-finished stretcher carried.
    stretch_pace: f64,
    last_content: f64,
    /// Frame cursor when a short captured outgoing loop is repeated for the mix.
    mix_out_frame: usize,
    mix_out_frames: usize,
    out_loop_frames: usize,

    /// The mixer with the rate and channel count it was built for.
    mixer: Option<(Mixer, u32, usize)>,
    stretch: Option<ByteStretcher>,
    /// The live stretcher's format (the incoming one while converting).
    stretch_format: Option<Format>,
    /// Built with the plan (keep_pitch), used when the next track begins; see `prepare`.
    pending_stretch: Option<(ByteStretcher, bool)>,
    loop_buf: Vec<u8>,

    analyzer: Option<(Analyzer, usize)>,
    analyzer_for: Option<String>,
    analyzer_rate: u32,
    /// A seek broke the analysed stream's continuity.
    analysis_tainted: bool,
    analysis_buf: Vec<f32>,

    /// The incoming format while it is converted to the output's.
    conv_in: Option<Format>,
    /// The stream the converter is armed for.
    conv_id: Option<String>,
    resampler: Option<Resampler>,
    staged: VecDeque<Staged<C>>,
    /// The stream the mix is consuming.
    mix_source_id: Option<String>,
    /// Keep the latched output format. False while the output is bit-perfect: streams pass native.
    pub lock_rate: bool,
    /// Reopen the output at the format of a stream that nothing is mixed into instead of converting it.
    /// Off in a bare engine; the player turns it on.
    pub follow_rate: bool,

    /// Gain of the song whose buffers arrive now (ReplayGain); see [`TransitionEngine::set_gain`].
    gain: f32,
    dither: crate::dither::Dither,

    queue: VecDeque<Chunk>,
    pool: Vec<Vec<u8>>,

    heard: Heard,
}

impl<C: Clone> Default for TransitionEngine<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Clone> TransitionEngine<C> {
    pub fn new() -> Self {
        TransitionEngine {
            out: None,
            pending_config: None,
            current_id: None,
            playing_id: None,
            fresh: false,
            awaiting_stream: false,
            awaiting_incoming: None,
            offset_us: 0,
            phase: Phase::Pass,
            plan: None,
            plan_for: None,
            replan_wanted: false,
            last_null_at: 0,
            made: None,
            tail: Vec::new(),
            tail_limit: 0,
            tail_len: 0,
            tail_heard: 0,
            tail_read: 0,
            held_from_us: None,
            held_at: 0,
            held_id: None,
            held_offset_us: 0,
            late_us: 0,
            takeover_us: 0,
            held_us: 0,
            reported: i64::MIN,
            shift_us: 0,
            shift_until_us: None,
            mixed_end_us: None,
            mix_from_us: None,
            skip_left: 0,
            resync_next: false,
            measure_next: false,
            synthetic_pts_us: None,
            mix_in_pts_us: None,
            stretch_pace: 1.0,
            last_content: 0.0,
            mix_out_frame: 0,
            mix_out_frames: 0,
            out_loop_frames: 0,
            mixer: None,
            stretch: None,
            stretch_format: None,
            pending_stretch: None,
            loop_buf: Vec::new(),
            analyzer: None,
            analyzer_for: None,
            analyzer_rate: 0,
            analysis_tainted: false,
            analysis_buf: Vec::new(),
            conv_in: None,
            conv_id: None,
            resampler: None,
            staged: VecDeque::new(),
            mix_source_id: None,
            lock_rate: true,
            follow_rate: false,
            gain: 1.0,
            dither: crate::dither::Dither::new(),
            queue: VecDeque::new(),
            pool: Vec::new(),
            heard: Heard { until_us: i64::MAX, audible_us: i64::MAX, next_rate: 1.0, ..Default::default() },
        }
    }

    pub fn heard(&self) -> &Heard {
        &self.heard
    }

    /// Asks for the playing track's plan again at the next buffer (e.g. a new analysis arrived).
    pub fn replan(&mut self) {
        self.replan_wanted = true;
    }

    /// How the ending of `id` was made: `Some(plan)` once its hold began or its last buffer went out
    /// (inner `None`: gapless); `None` while its ending is still to come.
    pub fn made(&self, id: &str) -> Option<Option<&Plan>> {
        if self.holding() && self.plan_for.as_deref() == Some(id) {
            return Some(self.plan.as_ref());
        }
        self.made.as_ref().filter(|(m, _)| m == id).map(|(_, p)| p.as_ref())
    }

    /// The playing song's ending is held, waiting for the next song.
    pub fn holding(&self) -> bool {
        matches!(self.phase, Phase::Hold)
    }

    /// State summary for diagnostics.
    pub fn words(&self) -> String {
        let phase = match self.phase {
            Phase::Pass => "passing",
            Phase::Hold => "holding an ending",
            Phase::Mix => "mixing",
        };
        let mut w = format!("{phase}, flowing {}, arriving {}", self.playing_id.as_deref().unwrap_or("-"), self.current_id.as_deref().unwrap_or("-"));
        if let (Some(p), Some(of)) = (&self.plan, &self.plan_for) {
            w.push_str(&format!(", plan {of} -> {} from {} ms for {} ms", p.incoming_id, p.out_start_us / 1000, p.duration_us / 1000));
        }
        if self.holding() {
            w.push_str(&format!(", held {} of {} of {} ({} ms)", self.tail_len, self.tail_limit, self.held_id.as_deref().unwrap_or("-"), self.held_us / 1000));
        }
        if let Some((p, _)) = &self.awaiting_incoming {
            w.push_str(&format!(", awaiting {}", p.incoming_id));
        }
        if self.fresh {
            w.push_str(", fresh");
        }
        let queued: usize = self.queue.iter().map(|c| c.data.len() - c.pos).sum();
        w.push_str(&format!(", {} chunks ({queued} bytes) for the output", self.queue.len()));
        w
    }

    // ---- configuration ----

    /// The decoder announces a stream. `config` goes to [`Downstream::configure`] if this format is
    /// applied to the output.
    pub fn configure<D: Downstream<Config = C>, H: Host>(&mut self, down: &mut D, host: &mut H, stream: StreamFormat, config: C) {
        let id = stream.id.clone();
        let f = stream.format;
        match f {
            Some(f) => host.log(&format!("sink: {} {} Hz x{} enc={}", id.as_deref().unwrap_or("?"), f.rate, f.channels, f.encoding.media3())),
            None => host.log(&format!("sink: {} - not PCM, no transitions", id.as_deref().unwrap_or("?"))),
        }
        // After a flush, or after a boundary nothing has flowed across yet, the announced stream is the
        // one about to flow, not decode-ahead: it must be armed now (staged, it would never be armed and
        // play unconverted at the wrong rate).
        let new_stream = id.is_some() && id != self.current_id;
        let awaited = self.awaiting_stream && new_stream;
        let for_current = id.is_none() || id == self.current_id || self.fresh || awaited;
        if self.fresh || awaited {
            self.fresh = false;
            self.awaiting_stream = false;
            if id.is_some() {
                self.playing_id = id.clone();
            }
        }
        if let Some(new) = id.as_ref().filter(|i| Some(*i) != self.current_id.as_ref()) {
            let new = new.clone();
            self.on_new_stream(host, new);
        }
        let Some(f) = f else {
            // Not PCM: the output must follow, after anything held has played out.
            self.drop_converter();
            if self.queue.is_empty() && self.phase == Phase::Pass {
                self.apply(down, None, &config);
                return;
            }
            self.abandon_transition(host);
            self.pending_config = Some((None, config));
            return;
        };
        if !self.lock_rate {
            // Bit-perfect: the output follows the native format.
            self.drop_converter();
            if self.out == Some(f) {
                return;
            }
            self.abandon_transition(host);
            self.pending_config = Some((Some(f), config));
            return;
        }
        let Some(out) = self.out else {
            self.apply(down, Some(f), &config);
            host.log(&format!("sink pins {} Hz x{} for the queue", f.rate, f.channels));
            return;
        };
        if self.phase == Phase::Mix && new_stream {
            if let Some((p, late)) = self.awaiting_incoming.take() {
                // The incoming side of a mix that began before it was announced.
                if f == out {
                    self.drop_converter();
                } else if !self.arm_conversion(host, id.clone(), f) {
                    self.abandon_transition(host);
                    self.pending_config = Some((Some(f), config));
                    return;
                }
                self.mix_source_id = id;
                self.enter_incoming(&p, late);
                return;
            }
        }
        if f == out {
            // Already the output's format: never reconfigure the output for it.
            if self.phase == Phase::Pass && for_current {
                self.drop_converter();
                return;
            }
            self.staged.push_back(Staged { id, format: Some(f), config });
            return;
        }
        if self.phase == Phase::Pass && for_current {
            if self.carries_on(f) {
                self.conv_id = id;
                return;
            }
            if self.follow_rate {
                self.follow(host, f, config);
                return;
            }
            if !self.arm_conversion(host, id, f) {
                self.abandon_transition(host);
                self.pending_config = Some((Some(f), config));
            }
            return;
        }
        // Decode-ahead: never interrupts a mix; armed once its buffers flow.
        self.staged.push_back(Staged { id, format: Some(f), config });
    }

    /// Converts stream `id` (format `src`) to the output format. Drops a stretcher built in `prepare`,
    /// since the stretcher now works in `src`.
    fn arm_conversion<H: Host>(&mut self, host: &mut H, id: Option<String>, src: Format) -> bool {
        let Some(out) = self.out else { return false };
        self.resampler = Resampler::new(src.rate as i32, src.channels as i32, out.rate as i32, out.channels as i32);
        if self.resampler.is_none() {
            self.conv_in = None;
            self.conv_id = None;
            return false;
        }
        self.conv_in = Some(src);
        self.conv_id = id;
        self.pending_stretch = None;
        host.log(&format!("converting {} Hz x{} -> {} Hz x{}", src.rate, src.channels, out.rate, out.channels));
        true
    }

    fn drop_converter(&mut self) {
        self.resampler = None;
        self.conv_in = None;
        self.conv_id = None;
    }

    /// After a seek: same formats, fresh converter state.
    fn reset_converter(&mut self) {
        if let (Some(src), Some(out)) = (self.conv_in, self.out) {
            self.resampler = Resampler::new(src.rate as i32, src.channels as i32, out.rate as i32, out.channels as i32);
            if self.resampler.is_none() {
                self.conv_in = None;
            }
        }
    }

    fn converting(&self) -> bool {
        self.resampler.is_some()
    }

    /// The running converter already converts `src`, so a gapless successor can reuse its history.
    fn carries_on(&self, src: Format) -> bool {
        self.resampler.is_some() && self.conv_in == Some(src)
    }

    /// Reopens the output at `f` once the queue has drained, instead of converting.
    fn follow<H: Host>(&mut self, host: &mut H, f: Format, config: C) {
        self.drop_converter();
        self.pending_config = Some((Some(f), config));
        host.log(&format!("sink follows {} Hz x{}: nothing overlaps, so the output opens again rather than convert", f.rate, f.channels));
    }

    /// Arms the staged format of `id` now that its buffers flow; other staged formats wait. `gapless`:
    /// nothing is mixed into it, so the output may follow its rate.
    fn arm_staged_for<H: Host>(&mut self, host: &mut H, id: Option<String>, gapless: bool) {
        let mut found: Option<Staged<C>> = None;
        let mut keep = VecDeque::new();
        while let Some(s) = self.staged.pop_front() {
            if s.id == id || (id.is_none() && found.is_none()) {
                found = Some(s);
            } else {
                keep.push_back(s);
            }
        }
        self.staged = keep;
        let Some(s) = found else { return };
        let Some(f) = s.format else {
            self.drop_converter();
            return;
        };
        if Some(f) == self.out {
            if self.conv_id.is_some() {
                self.drop_converter();
            }
            return;
        }
        // Re-arming would lose the filter history and click.
        if self.carries_on(f) && (self.conv_id == id || gapless) {
            self.conv_id = id;
            return;
        }
        if gapless && self.follow_rate {
            self.follow(host, f, s.config);
            return;
        }
        if !self.arm_conversion(host, id, f) {
            self.abandon_transition(host);
            self.pending_config = Some((Some(f), s.config));
        }
    }

    fn apply<D: Downstream<Config = C>>(&mut self, down: &mut D, f: Option<Format>, config: &C) {
        self.out = f;
        down.configure(config, f);
    }

    pub fn set_output_stream_offset_us(&mut self, offset_us: i64) {
        self.offset_us = offset_us;
    }

    /// Sets the gain (ReplayGain) of the song whose buffers come next, from its first sample. Applied per
    /// song before holding or mixing, so each side of a mix keeps its own level; the analyser gets the
    /// unscaled audio. Above 1 only float samples are boosted (16-bit would clip here).
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = if gain.is_finite() { gain.clamp(0.0, MAX_GAIN) } else { 1.0 };
    }

    /// Scales by `ratio` everything still queued or held of the song at `stream_offset_us` (settings
    /// changed), including the untaken rest of a partly taken chunk. Mixed chunks are left alone.
    /// Allocates nothing.
    pub fn rescale(&mut self, stream_offset_us: i64, ratio: f32) {
        let Some(out) = self.out else { return };
        if !ratio.is_finite() || ratio < 0.0 || ratio == 1.0 {
            return;
        }
        for c in self.queue.iter_mut().filter(|c| c.stream_us == Some(stream_offset_us)) {
            crate::pcm::scale(&mut c.data[c.pos..], out.encoding, ratio);
        }
        if self.tail_len > 0 && matches!(self.phase, Phase::Hold | Phase::Mix) && self.held_offset_us == stream_offset_us {
            // All of it: a looped hold is read again.
            crate::pcm::scale(&mut self.tail[..self.tail_len], out.encoding, ratio);
        }
    }

    // ---- the audio path ----

    /// One decoded buffer at `pts_us`. Returns (all taken, bytes taken); the rest is offered again.
    pub fn handle_buffer<D: Downstream<Config = C>, H: Host>(&mut self, down: &mut D, host: &mut H, buffer: &[u8], pts_us: i64) -> (bool, usize) {
        self.fresh = false;
        self.awaiting_stream = false;
        if let Some((p, late)) = self.awaiting_incoming.take() {
            // The incoming side flows without a new format: it is the current stream.
            if self.phase == Phase::Mix {
                self.mix_source_id = self.current_id.clone();
                self.enter_incoming(&p, late);
            }
        }
        if let Some((f, config)) = self.pending_config.take() {
            if !self.drain(down) {
                self.pending_config = Some((f, config));
                return (false, 0);
            }
            self.apply(down, f, &config);
        }
        let Some(out) = self.out else { return down.handle_buffer(buffer, 0, pts_us) };
        if !self.drain(down) {
            return (false, 0);
        }
        let native = self.conv_in.unwrap_or(out);
        let gain = if native.encoding == crate::pcm::Encoding::Float { self.gain } else { self.gain.min(1.0) };
        let scaled = (gain != 1.0).then(|| {
            let mut b = self.copy_of(buffer);
            crate::pcm::scale_dithered(&mut b, native.encoding, gain, native.channels, &mut self.dither);
            b
        });
        let taken = self.route(down, host, buffer, scaled.as_deref().unwrap_or(buffer), pts_us, out, native);
        if let Some(b) = scaled {
            self.recycle(b);
        }
        taken
    }

    /// Routes a buffer by phase. `raw` is unscaled (for the analyser), `buffer` is at the song's gain.
    #[allow(clippy::too_many_arguments)]
    fn route<D: Downstream<Config = C>, H: Host>(&mut self, down: &mut D, host: &mut H, raw: &[u8], buffer: &[u8], pts_us: i64, out: Format, native: Format) -> (bool, usize) {
        self.feed_analysis(host, raw, native);
        match self.phase {
            Phase::Pass => {
                if self.stretch.is_some() {
                    self.stretch_out(host, buffer, pts_us);
                    self.drain(down);
                    return (true, buffer.len());
                }
                let converted = if self.converting() {
                    match self.converted(host, buffer) {
                        Some(b) => Some(b),
                        None => {
                            self.drain(down);
                            return (true, buffer.len());
                        }
                    }
                } else {
                    None
                };
                let result = self.pass_or_hold(down, host, converted.as_deref().unwrap_or(buffer), buffer.len(), pts_us, out);
                if let Some(b) = converted {
                    self.recycle(b);
                }
                if let Some(r) = result {
                    return r;
                }
            }
            Phase::Hold => {
                if self.converting() {
                    if let Some(b) = self.converted(host, buffer) {
                        self.hold(&b, out);
                        self.recycle(b);
                    }
                } else {
                    self.hold(buffer, out);
                }
            }
            Phase::Mix => self.mix(host, buffer, pts_us, out),
        }
        self.drain(down);
        (true, buffer.len())
    }

    /// Passes `buf` (already converted) through, or at the plan's start passes the head and holds the
    /// rest. `Some` is the result to return now; `None` means it was taken whole.
    fn pass_or_hold<D: Downstream<Config = C>, H: Host>(&mut self, down: &mut D, host: &mut H, buf: &[u8], whole: usize, pts_us: i64, out: Format) -> Option<(bool, usize)> {
        if self.playing_id.is_none() {
            self.playing_id = self.current_id.clone();
        }
        let mut p = self.refresh_plan(host);
        if p.is_none() && self.playing_id != self.current_id {
            // A stale playing id (rapid skips): trust the decoder's id; costs one replan.
            self.playing_id = self.current_id.clone();
            p = self.refresh_plan(host);
        }
        let track_pos = pts_us - self.offset_us;
        let fb = out.frame_bytes();
        let frames = (buf.len() / fb) as i64;
        let start_frame = p.map_or(i64::MAX, |(start_us, _)| (start_us - track_pos) * out.rate as i64 / 1_000_000);
        // Past the start but inside the transition (a seek): hold what is left. Past its end: nothing to hold.
        let skip_transition = p.is_some_and(|(_, duration_us)| start_frame < -duration_us * out.rate as i64 / 1_000_000);
        if start_frame >= frames || skip_transition {
            return Some(self.pass(down, buf, whole, pts_us));
        }
        let p = self.plan.clone().expect("a plan exists past this point");
        let late = start_frame < 0;
        let before = start_frame.max(0) as usize * fb;
        if before > 0 {
            let head = self.copy_of(&buf[..before]);
            self.enqueue(head, pts_us, Some(self.offset_us));
        }
        self.late_us = if late { -start_frame * 1_000_000 / out.rate as i64 } else { 0 };
        self.begin_hold(&p, out);
        if late {
            host.log(&format!("transition: late hold, {} ms in", self.late_us / 1000));
        }
        let held_from = pts_us + (before / fb) as i64 * 1_000_000 / out.rate as i64;
        self.held_from_us = Some(held_from);
        self.heard.audible_us = held_from - self.held_offset_us + self.takeover_us;
        self.held_at = host.now_ms();
        let runway = down.position_us(false).map(|at| held_from - at);
        host.log(&match runway {
            None => "holding the ending, no sound still in the sink".to_string(),
            Some(r) => format!("holding the ending, {} ms of sound still in the sink", r / 1000),
        });
        if let Some(runway) = runway.filter(|&r| r < DRY_US && !late) {
            // Decode never got ahead (a seek near the boundary, next track still fetching): don't hold.
            host.log(&format!("transition: no runway ({} ms), letting the ending play", runway / 1000));
            self.abandon_transition(host);
            return Some(self.pass(down, &buf[before..], whole, pts_us));
        }
        self.hold(&buf[before..], out);
        None
    }

    /// Queues a copy of `buffer` and drains; reports `whole` bytes taken. Copied so a later partial take
    /// and [`TransitionEngine::rescale`] work on the engine's own memory.
    fn pass<D: Downstream<Config = C>>(&mut self, down: &mut D, buffer: &[u8], whole: usize, pts_us: i64) -> (bool, usize) {
        let c = self.copy_of(buffer);
        self.enqueue(c, pts_us, Some(self.offset_us));
        self.drain(down);
        (true, whole)
    }

    /// The playing song's plan as (start, duration) µs, asked for when unknown. A `None` answer is
    /// often transient (queue edits, analyses landing), so it is re-asked after [`NULL_PLAN_RETRY_MS`].
    fn refresh_plan<H: Host>(&mut self, host: &mut H) -> Option<(i64, i64)> {
        let span = |plan: &Option<Plan>| plan.as_ref().map(|p| (p.out_start_us, p.duration_us));
        let id = self.playing_id.as_deref()?;
        let now = host.now_ms();
        if self.plan_for.as_deref() == Some(id) && !self.replan_wanted && (self.plan.is_some() || now - self.last_null_at <= NULL_PLAN_RETRY_MS) {
            return span(&self.plan);
        }
        let id = id.to_string();
        self.replan_wanted = false;
        self.plan = host.plan_for(&id);
        self.plan_for = Some(id);
        if self.plan.is_none() {
            self.last_null_at = now;
        }
        if let Some(p) = self.plan.clone() {
            self.prepare(&p);
        }
        span(&self.plan)
    }

    /// Allocates what the transition needs when the plan arrives, not when the mix starts (allocating
    /// between two buffers can starve the output).
    fn prepare(&mut self, p: &Plan) {
        let Some(out) = self.out else { return };
        let bytes = out.bytes(p.duration_us);
        if bytes > 0 && self.tail.len() < bytes {
            self.tail.resize(bytes, 0);
        }
        self.ensure_mixer(out);
        if p.stretching() {
            if self.pending_stretch.as_ref().is_some_and(|(_, k)| *k != p.keep_pitch) {
                self.pending_stretch = None;
            }
            if self.pending_stretch.is_none() {
                self.pending_stretch = Some((ByteStretcher::new(out.rate, out.channels, p.keep_pitch), p.keep_pitch));
            }
        }
        while self.pool.len() < 8 {
            self.pool.push(Vec::with_capacity(16384));
        }
    }

    /// The mixer for `out`, rebuilt only when rate or channels changed.
    fn ensure_mixer(&mut self, out: Format) -> &mut Mixer {
        if !matches!(self.mixer, Some((_, rate, channels)) if rate == out.rate && channels == out.channels) {
            self.mixer = Some((Mixer::new(out.rate, out.channels), out.rate, out.channels));
        }
        &mut self.mixer.as_mut().expect("just ensured").0
    }

    fn begin_hold(&mut self, p: &Plan, out: Format) {
        self.held_id = self.playing_id.clone().or_else(|| self.current_id.clone());
        self.held_offset_us = self.offset_us;
        self.heard.next_rate = if p.stretching() { p.tempo_ratio } else { 1.0 };
        // The next song takes over where it becomes the louder, not where the fade begins.
        let late = self.late_us.clamp(0, p.duration_us);
        self.takeover_us = (crate::automix::mixer::crossover_ms(&p.mixer) * 1000 - late).max(0);
        self.heard.next_from_us = p.in_skip_us + ((late + self.takeover_us) as f64 * self.heard.next_rate as f64) as i64;
        self.heard.next_id = Some(p.incoming_id.clone());
        self.heard.from_id = self.held_id.clone();
        self.heard.audible_us = i64::MAX;
        // Outro loop: only the loop slice is captured.
        let hold_us = if p.out_loop_us > 0 { p.out_loop_us } else { p.duration_us };
        let bytes = out.bytes(hold_us);
        if self.tail.len() < bytes {
            self.tail.resize(bytes, 0);
        }
        // A late hold only holds what is left of the overlap.
        let late_hold = if p.out_loop_us > 0 { 0 } else { late };
        self.tail_limit = out.bytes((hold_us - late_hold).max(0));
        self.tail_len = 0;
        self.phase = Phase::Hold;
    }

    /// Appends outgoing audio to the hold; what does not fit is skipped by the plan.
    fn hold(&mut self, buffer: &[u8], out: Format) {
        self.held_us += out.us(buffer.len());
        let n = buffer.len().min(self.tail_limit - self.tail_len);
        if n > 0 {
            self.tail[self.tail_len..self.tail_len + n].copy_from_slice(&buffer[..n]);
            self.tail_len += n;
        }
    }

    /// The next track begins (or a seek landed): start mixing into the hold, or let the hold go unmixed.
    pub fn handle_discontinuity<D: Downstream<Config = C>, H: Host>(&mut self, down: &mut D, host: &mut H) {
        let p = self.plan.clone();
        let ending = self.plan_for.clone().or_else(|| self.playing_id.clone());
        // Our pipeline announces the next stream before this; media3 announces it with its first buffer.
        let announced = self.current_id.is_some() && self.current_id != ending;
        let mixed = self.holding() && self.tail_len > 0 && self.out.is_some();
        self.made = ending.map(|id| (id, if mixed { p.clone() } else { None }));
        match (self.phase, p, self.out) {
            (Phase::Hold, Some(p), Some(out)) if self.tail_len > 0 => {
                // Arm the incoming format so stretch and skip are measured in its own domain.
                if announced {
                    self.arm_staged_for(host, self.current_id.clone(), false);
                    self.mix_source_id = self.current_id.clone();
                }
                self.playing_id = Some(p.incoming_id.clone());
                let late = self.late_us.clamp(0, p.duration_us);
                let m = self.ensure_mixer(out);
                m.configure(&p.mixer);
                // A late hold starts the mix curves that far in.
                if late > 0 {
                    m.seek((late * out.rate as i64 / 1_000_000) as u64);
                }
                self.skip_left = 0;
                if announced {
                    self.enter_incoming(&p, late);
                } else {
                    self.awaiting_incoming = Some((p.clone(), late));
                }
                let left = self.held_from_us.zip(down.position_us(false)).map(|(from, at)| from - at);
                host.log(&format!(
                    "mixing: the next track arrived {} ms into the hold with {} of sound left",
                    host.now_ms() - self.held_at,
                    left.map_or("no".to_string(), |l| format!("{} ms", l / 1000)),
                ));
                // The hold goes out as the mix now; the position already reported stands.
                self.held_us = 0;
                // A song shorter than planned: pad the hold with silence so the mix curves run to the end.
                self.tail_heard = self.tail_len;
                if p.out_loop_us <= 0 && self.tail_len < self.tail_limit {
                    self.tail[self.tail_len..self.tail_limit].fill(0);
                    self.tail_len = self.tail_limit;
                }
                self.tail_read = 0;
                self.mix_out_frame = 0;
                self.mix_out_frames = ((p.duration_us - late) * out.rate as i64 / 1_000_000).max(0) as usize;
                self.out_loop_frames = if p.out_loop_us > 0 { ((p.out_loop_us * out.rate as i64 / 1_000_000) as usize).max(1) } else { 0 };
                self.mixed_end_us = None;
                self.mix_from_us = None;
                self.resync_next = true;
                self.measure_next = true;
                self.phase = Phase::Mix;
            }
            _ => {
                self.abandon_transition(host);
                self.playing_id = self.current_id.clone();
                self.arm_staged_for(host, self.current_id.clone(), true);
                self.mix_source_id = None;
                self.awaiting_stream = true;
                down.handle_discontinuity();
            }
        }
        self.plan = None;
        self.plan_for = None;
    }

    /// Sets up the mix's incoming side once its format is known: the stretcher and the skip, in the
    /// incoming format when converting.
    fn enter_incoming(&mut self, p: &Plan, late: i64) {
        let Some(out) = self.out else { return };
        let stretching = p.stretching();
        let in_late_us = if stretching { (late as f64 * p.tempo_ratio as f64) as i64 } else { late };
        let s_fmt = self.conv_in.unwrap_or(out);
        if stretching {
            let taken = if !self.converting() { self.pending_stretch.take().filter(|(_, k)| *k == p.keep_pitch).map(|(s, _)| s) } else { None };
            self.pending_stretch = None;
            let mut s = taken.unwrap_or_else(|| ByteStretcher::new(s_fmt.rate, s_fmt.channels, p.keep_pitch));
            s.configure(
                p.tempo_ratio as f64,
                ((p.duration_us - late) * s_fmt.rate as i64 / 1_000_000).max(0) as u64,
                (p.ramp_us * s_fmt.rate as i64 / 1_000_000).max(0) as u64,
            );
            self.stretch = Some(s);
            self.stretch_format = Some(s_fmt);
            self.mix_in_pts_us = None;
            self.stretch_pace = p.tempo_ratio as f64;
            self.last_content = 0.0;
        }
        self.skip_left = s_fmt.bytes(p.in_skip_us + in_late_us);
    }

    /// Mixes the incoming track into the held ending.
    fn mix<H: Host>(&mut self, host: &mut H, buffer: &[u8], pts_us: i64, out: Format) {
        let mut buffer = buffer;
        let mut first_us = pts_us;
        if self.skip_left > 0 {
            let n = self.skip_left.min(buffer.len());
            buffer = &buffer[n..];
            self.skip_left -= n;
            first_us += self.conv_in.unwrap_or(out).us(n);
            if buffer.is_empty() {
                return;
            }
        }
        if self.stretch.is_some() && self.synthetic_pts_us.is_none() && self.mix_in_pts_us.is_none() {
            self.mix_in_pts_us = Some(first_us);
        }
        // Stretch in the incoming format, then convert to the output's.
        let stretched = if self.stretch.is_some() {
            match self.stretched(buffer) {
                Some(b) => Some(b),
                None => return,
            }
        } else {
            None
        };
        let buffer: &[u8] = stretched.as_deref().unwrap_or(buffer);
        let converted = if self.converting() {
            match self.converted(host, buffer) {
                Some(b) => Some(b),
                None => {
                    if let Some(b) = stretched {
                        self.recycle(b);
                    }
                    return;
                }
            }
        } else {
            None
        };
        let src: &[u8] = converted.as_deref().unwrap_or(buffer);
        let pace = if stretched.is_some() { self.stretch_pace } else { 1.0 };
        let fb = out.frame_bytes();
        let looping = self.out_loop_frames > 0;
        let remaining = if looping { self.mix_out_frames.saturating_sub(self.mix_out_frame) } else { usize::MAX };
        let tail_frames = if looping { remaining } else { (self.tail_len - self.tail_read) / fb };
        let frames = (src.len() / fb).min(remaining).min(tail_frames);
        let mut used = 0;
        if frames > 0 {
            let bytes = frames * fb;
            if looping {
                let hold_frames = (self.tail_len / fb).max(1);
                let mut chunk = std::mem::take(&mut self.loop_buf);
                chunk.resize(bytes, 0);
                self.wrap_out(hold_frames, self.out_loop_frames, self.mix_out_frame, &mut chunk, frames, fb);
                if let Some((m, ..)) = self.mixer.as_mut() {
                    // SAFETY: `chunk` and `src` each hold at least `frames` frames.
                    unsafe { mix_raw(m, chunk.as_ptr(), src.as_ptr(), chunk.as_mut_ptr(), frames, out.encoding) };
                }
                let at = self.stamp(pts_us, frames, pace, out);
                let c = self.copy_of(&chunk);
                self.loop_buf = chunk;
                self.enqueue_paced(c, at, None, pace);
                self.mixed_end_us = Some(at + span_us(frames, pace, out));
                self.mix_out_frame += frames;
            } else {
                let r = self.tail_read;
                if let Some((m, ..)) = self.mixer.as_mut() {
                    let t = self.tail[r..r + bytes].as_mut_ptr();
                    // SAFETY: the tail slice and `src` each hold at least `frames` frames.
                    unsafe { mix_raw(m, t, src.as_ptr(), t, frames, out.encoding) };
                }
                let at = self.stamp(pts_us, frames, pace, out);
                let mut c = self.take_pooled(bytes);
                c.extend_from_slice(&self.tail[r..r + bytes]);
                self.enqueue_paced(c, at, None, pace);
                self.mixed_end_us = Some(at + span_us(frames, pace, out));
                self.tail_read += bytes;
            }
            used = bytes;
        }
        if used < src.len() && !looping {
            let rest = &src[used..];
            let at = self.stamp(pts_us, rest.len() / fb, pace, out);
            let c = self.copy_of(rest);
            self.enqueue_paced(c, at, Some(self.offset_us), pace);
            self.mixed_end_us = Some(at + span_us(rest.len() / fb, pace, out));
        }
        if let Some(b) = converted {
            self.recycle(b);
        }
        if let Some(b) = stretched {
            self.recycle(b);
        }
        if (looping && self.mix_out_frame >= self.mix_out_frames) || (!looping && self.tail_read >= self.tail_len) {
            self.phase = Phase::Pass;
            self.finish_conversion(host);
        }
    }

    /// Copies looped outgoing audio: frames before the last `loop_frames` of the hold play once, the
    /// last `loop_frames` repeat.
    fn wrap_out(&self, hold_frames: usize, loop_frames: usize, from_frame: usize, dst: &mut [u8], frames: usize, fb: usize) {
        let lp = loop_frames.clamp(1, hold_frames);
        let prefix = hold_frames.saturating_sub(lp);
        let mut i = 0;
        while i < frames {
            let f = from_frame + i;
            let src_frame = if f < prefix { f } else { prefix + (f - prefix) % lp };
            let run = if f < prefix { (frames - i).min(prefix - f) } else { (frames - i).min(lp - (f - prefix) % lp) };
            let (s, n) = (src_frame * fb, run * fb);
            dst[i * fb..i * fb + n].copy_from_slice(&self.tail[s..s + n]);
            i += run;
        }
    }

    /// Converts incoming-format audio to the output format. `None` when nothing is ready yet. If
    /// conversion fails, drops the converter and lets the ending play unmixed.
    fn converted<H: Host>(&mut self, host: &mut H, input: &[u8]) -> Option<Vec<u8>> {
        let (Some(src), Some(out)) = (self.conv_in, self.out) else { return None };
        let in_frames = input.len() / src.frame_bytes().max(1);
        let need = (((in_frames as u64 * out.rate as u64 / src.rate.max(1) as u64) as usize + 4) * out.frame_bytes()).max(16384);
        let mut b = self.take_pooled(need);
        b.resize(need, 0);
        let mut r = self.resampler.as_mut().and_then(|rs| rs.process(input, src.encoding.media3(), &mut b, out.encoding.media3()));
        if r.is_none() && self.resampler.is_some() {
            b.resize(b.len() * 2 + 16384, 0);
            r = self.resampler.as_mut().and_then(|rs| rs.process(input, src.encoding.media3(), &mut b, out.encoding.media3()));
        }
        let Some((_, made)) = r else {
            host.log(&format!("conversion {} Hz x{} -> {} Hz x{} failed, letting the ending play", src.rate, src.channels, out.rate, out.channels));
            self.recycle(b);
            self.drop_converter();
            self.abandon_transition(host);
            return None;
        };
        b.truncate(made);
        if made == 0 {
            self.recycle(b);
            None
        } else {
            Some(b)
        }
    }

    /// The mix is done: arm the mixed-in stream's own staged format.
    fn finish_conversion<H: Host>(&mut self, host: &mut H) {
        self.mix_source_id = None;
        self.arm_staged_for(host, self.current_id.clone(), false);
    }

    /// Timestamp for mixed or stretched output: the engine's own clock in the incoming song's time,
    /// advanced by the song time each frame carries, so a stretch never looks like a jump.
    fn stamp(&mut self, pts_us: i64, frames: usize, pace: f64, out: Format) -> i64 {
        if self.stretch.is_none() && self.synthetic_pts_us.is_none() {
            return pts_us;
        }
        let at = self.synthetic_pts_us.or_else(|| self.mix_in_pts_us.take()).unwrap_or(pts_us);
        self.synthetic_pts_us = Some(at + span_us(frames, pace, out));
        at
    }

    fn stretched(&mut self, input: &[u8]) -> Option<Vec<u8>> {
        let fmt = self.stretch_format.or(self.out)?;
        let need = input.len() * 3 + self.stretch.as_ref()?.latency_frames() * fmt.frame_bytes() + 8192;
        let mut buf = self.take_pooled(need);
        buf.resize(need, 0);
        let s = self.stretch.as_mut()?;
        let mut produced = 0;
        let mut at = 0;
        let fb = fmt.frame_bytes();
        while at < input.len() {
            let (used, made) = s.process(&input[at..], &mut buf[produced..], fmt.encoding);
            at += used;
            produced += made;
            if used == 0 && made == 0 {
                break;
            }
        }
        if s.bypassed() {
            produced = self.finish_stretch(&mut buf, produced, fmt);
        }
        // Song time per output frame, including a finished stretcher's last frames.
        let content = match self.stretch.as_mut() {
            Some(s) => s.take_content(),
            None => std::mem::take(&mut self.last_content),
        };
        if produced >= fb {
            self.stretch_pace = content / (produced / fb) as f64;
        }
        buf.truncate(produced);
        if produced == 0 {
            self.recycle(buf);
            None
        } else {
            Some(buf)
        }
    }

    /// After a mix, the incoming track stays in the stretcher until its tempo ramp ends.
    fn stretch_out<H: Host>(&mut self, host: &mut H, buffer: &[u8], pts_us: i64) {
        let Some(out) = self.out else { return };
        // Stamped on the running clock, including the stretcher's final output (stamped 0 it would pull
        // the output's clock back and a later hold would never be released).
        let at = self.synthetic_pts_us.unwrap_or(pts_us);
        let Some(s) = self.stretched(buffer) else { return };
        let o = if self.converting() {
            let c = self.converted(host, &s);
            self.recycle(s);
            match c {
                Some(o) => o,
                None => return,
            }
        } else {
            s
        };
        let frames = o.len() / out.frame_bytes();
        let pace = self.stretch_pace;
        // Finished: the resync belongs to the buffer after this one, back on real timestamps.
        let resync = self.stretch.is_none() && std::mem::take(&mut self.resync_next);
        self.enqueue_paced(o, at, Some(self.offset_us), pace);
        self.resync_next |= resync;
        if let Some(at) = self.synthetic_pts_us.as_mut() {
            *at += span_us(frames, pace, out);
        }
    }

    fn finish_stretch(&mut self, buf: &mut Vec<u8>, produced: usize, fmt: Format) -> usize {
        let more = match self.stretch.as_mut() {
            Some(s) => {
                let room = (s.latency_frames() + 4 * crate::automix::stretch::BLOCK) * fmt.frame_bytes();
                if buf.len() < produced + room {
                    buf.resize(produced + room, 0);
                }
                s.drain(&mut buf[produced..], fmt.encoding)
            }
            None => 0,
        };
        self.last_content = self.stretch.as_mut().map_or(0.0, |s| s.take_content());
        self.stretch = None;
        self.stretch_format = None;
        // Back on the track's own timestamps: the output takes the next one as a new reference.
        self.resync_next = true;
        self.synthetic_pts_us = None;
        produced + more
    }

    /// Lets the held audio go unmixed (the next track never came, or the mix was cut short). A cut
    /// mix queues only the song's audio not yet mixed out. The converter is kept.
    fn abandon_transition<H: Host>(&mut self, host: &mut H) {
        self.measure_next = false;
        if self.tail_len > 0 && matches!(self.phase, Phase::Hold | Phase::Mix) {
            // In a mix, never the silence padding a short song's hold.
            let end = if self.phase == Phase::Mix { self.tail_heard.min(self.tail_len) } else { self.tail_len };
            let from = if self.phase == Phase::Mix { self.tail_read.min(end) } else { 0 };
            if from < end {
                // At its own timestamps (the output keeps its clock from them), after any queued mix.
                let at = match (self.phase, self.mixed_end_us) {
                    (Phase::Mix, Some(end)) => end,
                    _ => self.held_from_us.unwrap_or(0),
                };
                let mut c = self.take_pooled(end - from);
                c.extend_from_slice(&self.tail[from..end]);
                self.enqueue(c, at, Some(self.held_offset_us));
            }
        }
        if self.phase != Phase::Pass {
            host.log(&format!("transition abandoned in {:?}", self.phase));
        }
        self.heard.next_id = None;
        self.heard.from_id = None;
        self.phase = Phase::Pass;
        self.awaiting_incoming = None;
        self.tail_len = 0;
        self.held_from_us = None;
        self.held_us = 0;
        self.mix_source_id = None;
        // Mark the plan as used so the next buffer of the same track does not hold again.
        self.plan = None;
        self.plan_for = self.current_id.clone();
    }

    // ---- analysis tap ----

    fn on_new_stream<H: Host>(&mut self, host: &mut H, id: String) {
        self.finish_analysis(host);
        self.current_id = Some(id);
        self.analysis_tainted = false;
        self.analyzer_for = None;
    }

    fn feed_analysis<H: Host>(&mut self, host: &mut H, bytes: &[u8], f: Format) {
        if self.current_id.is_none() || self.analysis_tainted || bytes.is_empty() {
            return;
        }
        if self.analyzer_for != self.current_id {
            let id = self.current_id.clone().expect("checked above");
            self.analyzer_for = Some(id.clone());
            self.analyzer = None;
            if let Some(expected_ms) = host.wants_analysis(&id) {
                self.analyzer = Some((Analyzer::new(f.rate, expected_ms), f.channels));
                self.analyzer_rate = f.rate;
            }
        }
        if let Some((a, ch)) = self.analyzer.as_mut() {
            self.analysis_buf.clear();
            crate::pcm::to_f32(bytes, f.encoding, &mut self.analysis_buf);
            a.feed_interleaved(&self.analysis_buf, *ch, |v| v);
        }
    }

    fn finish_analysis<H: Host>(&mut self, host: &mut H) {
        let taken = self.analyzer.take();
        let id = self.analyzer_for.clone();
        let (Some((a, ch)), Some(id)) = (taken, id) else { return };
        if self.analysis_tainted {
            return;
        }
        let frames = a.samples();
        host.analysed(&id, a, ch, frames, self.analyzer_rate);
    }

    // ---- output queue ----

    /// Queues `data` at `pts_us`; `stream_us` is its song's stream offset (`None` for a mix).
    fn enqueue(&mut self, data: Vec<u8>, pts_us: i64, stream_us: Option<i64>) {
        self.enqueue_paced(data, pts_us, stream_us, 1.0);
    }

    fn enqueue_paced(&mut self, data: Vec<u8>, pts_us: i64, stream_us: Option<i64>, pace: f64) {
        if data.is_empty() {
            self.recycle(data);
            return;
        }
        self.queue.push_back(Chunk { data, pos: 0, pts_us, resync: self.resync_next, measure: self.measure_next, stream_us, pace });
        self.resync_next = false;
        self.measure_next = false;
    }

    /// An empty buffer with capacity for `n` bytes, from the pool when one fits.
    fn take_pooled(&mut self, n: usize) -> Vec<u8> {
        let mut b = match self.pool.iter().position(|b| b.capacity() >= n) {
            Some(i) => self.pool.swap_remove(i),
            None => Vec::with_capacity(n.max(16384)),
        };
        b.clear();
        b
    }

    fn copy_of(&mut self, src: &[u8]) -> Vec<u8> {
        let mut b = self.take_pooled(src.len());
        b.extend_from_slice(src);
        b
    }

    fn recycle(&mut self, b: Vec<u8>) {
        if self.pool.len() < 32 {
            self.pool.push(b);
        }
    }

    /// Sends queued chunks. False when the output would not take everything yet.
    fn drain<D: Downstream<Config = C>>(&mut self, down: &mut D) -> bool {
        loop {
            let Some(c) = self.queue.front_mut() else { return true };
            if c.resync && c.pos == 0 {
                down.handle_discontinuity();
            }
            let before = if c.measure { down.position_us(false) } else { None };
            down.media_pace(c.pace);
            let (taken, used) = down.handle_buffer(&c.data, c.pos, c.pts_us);
            c.pos += used;
            if c.measure {
                // The output jumps its clock to this chunk's time on taking it; if it refused before
                // looking, the next offer is measured again.
                let jump = before.zip(down.position_us(false)).map(|(b, a)| a - b).filter(|j| j.abs() > 50_000);
                if let Some(j) = jump {
                    self.shift_us = j;
                    self.shift_until_us = Some(c.pts_us);
                }
                if jump.is_some() || taken {
                    self.mix_from_us = Some(c.pts_us);
                    c.measure = false;
                }
            }
            if !taken {
                return false;
            }
            let c = self.queue.pop_front().expect("front exists");
            self.recycle(c.data);
        }
    }

    /// The position to report to the player, called every few ms. Also the one place that notices a
    /// held ending about to starve the output (the next track is late) and lets it go unmixed.
    pub fn position_us<D: Downstream<Config = C>, H: Host>(&mut self, down: &mut D, host: &mut H, source_ended: bool) -> Option<i64> {
        let Some(at) = down.position_us(source_ended) else { return self.held_from_nothing(host) };
        if let (Phase::Hold, Some(from)) = (self.phase, self.held_from_us) {
            if from - at < DRY_US && host.now_ms() - self.held_at > HOLD_GRACE_MS {
                host.log(&format!("transition: nothing to mix in yet with {} ms of sound left, letting the ending play", (from - at) / 1000));
                self.abandon_transition(host);
                self.drain(down);
            }
        }
        // Held audio counts as played, so the player reads the next track (which it does only near the
        // end) in time to mix it in. Never goes backwards; once the mix starts it stands until the audible
        // position catches up.
        self.reported = self.reported.max(at + self.held_us);
        // The first mixed sample is audible: the clock is now the incoming song's.
        if self.shift_us != 0 && self.shift_until_us.is_none_or(|until| at >= until) {
            self.shift_us = 0;
        }
        let ear = at - self.shift_us;
        let was_heard = self.heard.id.is_some();
        // While the mix plays but before takeover, the audible song is still the outgoing one: mix
        // start plus time since, where the clock runs at the incoming song's tempo (`pace`).
        let pace = self.heard.next_rate.max(0.01) as f64;
        let incoming_from = self.mix_from_us.filter(|&from| self.shift_us == 0 && self.held_from_us.is_some() && at >= from);
        let taking_over = incoming_from.is_some_and(|from| at < from + (self.takeover_us as f64 * pace) as i64);
        match &self.held_id {
            Some(id) if self.reported > ear + 20_000 || taking_over => {
                let start = self.held_from_us.map(|from| from - self.held_offset_us);
                // Never read the incoming song's clock as the outgoing song's position.
                self.heard.us = match (start, incoming_from) {
                    (Some(start), Some(from)) => start + ((at - from) as f64 / pace) as i64,
                    _ => ear - self.held_offset_us,
                };
                self.heard.until_us = start.map_or(i64::MAX, |start| start + self.takeover_us);
                self.heard.at_ms = host.now_ms();
                if self.heard.id.as_ref() != Some(id) {
                    self.heard.id = Some(id.clone());
                }
            }
            _ => self.heard.id = None,
        }
        if self.heard.id.is_some() != was_heard {
            host.heard_changed();
        }
        // Caught up: later clock wobbles must not revive the held song.
        if self.heard.id.is_none() && self.phase == Phase::Pass && self.shift_us == 0 {
            self.held_id = None;
        }
        if self.mix_from_us.is_some() && self.mixed_end_us.is_some_and(|end| at >= end) {
            self.mix_from_us = None;
            // The whole mix was heard; clearing these also stops nori-engine waking for a mix.
            if self.phase == Phase::Pass {
                self.heard.next_id = None;
                self.heard.from_id = None;
            }
        }
        let mixing = self.mix_from_us.is_some_and(|from| at >= from);
        if mixing != self.heard.mixing {
            self.heard.mixing = mixing;
            host.heard_changed();
        }
        Some(self.reported)
    }

    /// The output has no position because it was given nothing: everything since a seek into a
    /// transition is being held. Held audio still counts as played (or the next song is never read and
    /// nothing plays), and the audible position is the hold's start.
    fn held_from_nothing<H: Host>(&mut self, host: &mut H) -> Option<i64> {
        let from = self.held_from_us.filter(|_| self.holding() && self.held_us > 0)?;
        self.reported = self.reported.max(from + self.held_us);
        if let Some(id) = self.held_id.as_ref() {
            let start = from - self.held_offset_us;
            self.heard.us = start;
            self.heard.until_us = start + self.takeover_us;
            self.heard.at_ms = host.now_ms();
            if self.heard.id.as_ref() != Some(id) {
                self.heard.id = Some(id.clone());
                host.heard_changed();
            }
        }
        Some(self.reported)
    }

    /// The source ended: flush the hold and finish the last analysis. Returns whether everything
    /// queued went to the output.
    pub fn play_to_end_of_stream<D: Downstream<Config = C>, H: Host>(&mut self, down: &mut D, host: &mut H) -> bool {
        self.abandon_transition(host);
        self.finish_analysis(host);
        self.drain(down)
    }

    #[cfg(any(test, feature = "synth"))]
    pub fn has_pending_data(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Drains, then whether the queue is empty.
    pub fn queue_empty<D: Downstream<Config = C>>(&mut self, down: &mut D) -> bool {
        if !self.queue.is_empty() {
            self.drain(down);
        }
        self.queue.is_empty()
    }

    fn clear<H: Host>(&mut self, host: &mut H) {
        while let Some(c) = self.queue.pop_front() {
            self.recycle(c.data);
        }
        self.phase = Phase::Pass;
        self.awaiting_stream = false;
        self.awaiting_incoming = None;
        self.tail_len = 0;
        self.tail_read = 0;
        self.skip_left = 0;
        self.mixed_end_us = None;
        self.mix_from_us = None;
        if self.heard.mixing {
            self.heard.mixing = false;
            host.heard_changed();
        }
        self.held_from_us = None;
        self.held_us = 0;
        self.reported = i64::MIN;
        self.held_id = None;
        self.late_us = 0;
        self.takeover_us = 0;
        self.heard.next_id = None;
        self.heard.from_id = None;
        self.shift_us = 0;
        self.shift_until_us = None;
        if self.heard.id.take().is_some() {
            host.heard_changed();
        }
        self.plan = None;
        self.plan_for = None;
        self.made = None;
        self.mix_source_id = None;
        self.resync_next = false;
        self.measure_next = false;
        self.synthetic_pts_us = None;
        self.mix_in_pts_us = None;
        self.stretch_pace = 1.0;
        self.last_content = 0.0;
        self.stretch = None;
        self.stretch_format = None;
        self.pending_stretch = None;
        // The output and formats stay; the converter restarts and staged formats are stale.
        self.reset_converter();
        self.staged.clear();
        self.pending_config = None;
        // The analyser no longer hears the track continuously.
        self.analyzer = None;
        self.analysis_tainted = true;
    }

    /// A seek or a jump in the queue: drops everything in flight.
    pub fn flush<H: Host>(&mut self, host: &mut H) {
        self.clear(host);
        self.fresh = true;
    }

    /// Stopped: also forgets the output format and frees buffers.
    pub fn reset<H: Host>(&mut self, host: &mut H) {
        self.clear(host);
        self.drop_converter();
        self.out = None;
        self.mixer = None;
        self.pool.clear();
        self.tail = Vec::new();
        self.loop_buf = Vec::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automix::{mixer, plan};
    use crate::pcm::Encoding;
    use crate::types::AutoMixSettings;

    const RATE: u32 = 44_100;
    const FMT: Format = Format { rate: RATE, channels: 2, encoding: Encoding::Pcm16 };

    /// A fake output: records what it takes; the test sets its position.
    #[derive(Default)]
    struct Down {
        taken: Vec<(Vec<u8>, i64)>,
        configured: Vec<u32>,
        discontinuities: usize,
        position: Option<i64>,
        /// Take at most this many bytes of the next offer.
        take_only: Option<usize>,
        /// (address, length) of a partly taken buffer's rest, which must be offered next (as `pipeline::Sink` requires).
        owed: Option<(usize, usize)>,
    }

    impl Downstream for Down {
        type Config = u32;
        fn configure(&mut self, config: &u32, _: Option<Format>) {
            self.configured.push(*config);
        }
        fn handle_buffer(&mut self, data: &[u8], from: usize, pts_us: i64) -> (bool, usize) {
            let key = (data.as_ptr() as usize + from, data.len() - from);
            if let Some(owed) = self.owed {
                assert_eq!(owed, key, "offered another buffer while one was only partly taken (pipeline::Sink panics here)");
            }
            let n = self.take_only.take().unwrap_or(usize::MAX).min(data.len() - from);
            self.taken.push((data[from..from + n].to_vec(), pts_us));
            let all = from + n == data.len();
            self.owed = if all { None } else { Some((key.0 + n, key.1 - n)) };
            (all, n)
        }
        fn handle_discontinuity(&mut self) {
            self.discontinuities += 1;
        }
        fn position_us(&mut self, _: bool) -> Option<i64> {
            self.position
        }
    }

    impl Down {
        fn samples(&self) -> Vec<i16> {
            self.taken.iter().flat_map(|(d, _)| d.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]))).collect()
        }
    }

    #[derive(Default)]
    struct Host_ {
        plans: std::collections::HashMap<String, Plan>,
        log: Vec<String>,
        now: i64,
        analysed: Vec<String>,
    }

    impl Host for Host_ {
        fn plan_for(&mut self, id: &str) -> Option<Plan> {
            self.plans.get(id).cloned()
        }
        fn wants_analysis(&mut self, _: &str) -> Option<u64> {
            Some(0)
        }
        fn analysed(&mut self, id: &str, _: Analyzer, _: usize, _: u64, _: u32) {
            self.analysed.push(id.to_string());
        }
        fn log(&mut self, m: &str) {
            self.log.push(m.to_string());
        }
        fn now_ms(&self) -> i64 {
            self.now
        }
    }

    fn stream(id: &str, f: Format) -> StreamFormat {
        StreamFormat { id: Some(id.into()), format: Some(f) }
    }

    /// `secs` of a constant stereo 16-bit value at 44.1 kHz.
    fn tone(v: i16, secs: f64) -> Vec<u8> {
        tone_at(v, RATE, secs)
    }

    /// `secs` of a constant stereo 16-bit value at `rate`.
    fn tone_at(v: i16, rate: u32, secs: f64) -> Vec<u8> {
        let frames = (rate as f64 * secs) as usize;
        (0..frames * 2).flat_map(|_| v.to_le_bytes()).collect()
    }

    /// Feeds 44.1 kHz `data` in 4096-byte buffers stamped from `from_us`.
    fn feed(e: &mut TransitionEngine<u32>, d: &mut Down, h: &mut Host_, data: &[u8], from_us: i64) {
        feed_in(e, d, h, data, FMT, from_us);
    }

    /// Feeds `data` (at `f`) in 4096-byte buffers stamped from `from_us`.
    fn feed_in(e: &mut TransitionEngine<u32>, d: &mut Down, h: &mut Host_, data: &[u8], f: Format, from_us: i64) {
        let mut at = 0;
        while at < data.len() {
            let n = 4096.min(data.len() - at);
            let (all, used) = e.handle_buffer(d, h, &data[at..at + n], from_us + f.us(at));
            assert!(all && used == n, "the fake output takes everything");
            at += n;
        }
    }

    /// A 2 s equal-power fade into `to`, starting `start_us` into the outgoing song.
    fn fade(to: &str, start_us: i64) -> Plan {
        let s = AutoMixSettings { max_transition_s: 2.0, ..Default::default() };
        let t = plan::plan(None, None, 60_000, 60_000, &s);
        assert_eq!(t.duration_ms, 2000);
        Plan {
            incoming_id: to.into(),
            out_start_us: start_us,
            duration_us: 2_000_000,
            in_skip_us: 0,
            mixer: mixer::params(&t),
            tempo_ratio: 1.0,
            keep_pitch: true,
            ramp_us: 0,
            out_loop_us: 0,
        }
    }

    #[test]
    fn passes_through_without_plan() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.configure(&mut d, &mut h, stream("a", FMT), 7);
        let a = tone(1000, 1.0);
        feed(&mut e, &mut d, &mut h, &a, 0);
        assert_eq!(d.configured, vec![7], "the first PCM stream pins the output");
        assert_eq!(d.taken.iter().map(|(b, _)| b.len()).sum::<usize>(), a.len());
        assert!(d.samples().iter().all(|&v| v == 1000));
        assert_eq!(d.taken[1].1, FMT.us(4096), "timestamps are the decoder's");
    }

    #[test]
    fn other_rate_is_converted() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(1000, 0.5), 0);
        // Our pipeline's order: announce, then the boundary.
        e.configure(&mut d, &mut h, stream("b", F48), 2);
        e.handle_discontinuity(&mut d, &mut h);
        let before = taken(&d);
        feed_in(&mut e, &mut d, &mut h, &tone_at(500, 48_000, 1.0), F48, 1_000_000);
        assert_eq!(d.configured, vec![1], "the output stays as it was opened");
        let secs = secs_after(&d, before, FMT);
        assert!((secs - 1.0).abs() < 0.01, "one second of 48 kHz comes out as one second at 44.1: {secs}");
    }

    #[test]
    fn first_format_after_flush_is_armed() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(1000, 0.3), 0);
        e.flush(&mut h);
        e.configure(&mut d, &mut h, stream("b", F48), 2);
        assert!(h.log.iter().any(|l| l.starts_with("converting 48000 Hz")), "{:?}", h.log);
    }

    #[test]
    fn fade_mixes_next_track_into_hold() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        h.plans.insert("a".into(), fade("b", 1_000_000));
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(8000, 3.0), 0);
        assert!(h.log.iter().any(|l| l.starts_with("holding the ending")), "{:?}", h.log);
        let passed = d.samples().len();
        assert_eq!(passed, FMT.bytes(1_000_000) / 2, "only the part before the transition went out");
        e.configure(&mut d, &mut h, stream("b", FMT), 2);
        e.handle_discontinuity(&mut d, &mut h);
        feed(&mut e, &mut d, &mut h, &tone(-8000, 3.0), 3_000_000);
        let s = d.samples();
        let frames = s.len() / 2;
        // 1 s alone, 2 s mixed, then the rest of b: the held second of a past the fade is skipped.
        assert_eq!(frames, (RATE as f64 * (1.0 + 3.0)) as usize);
        let mid = (RATE as usize * 2) * 2;
        assert!(s[mid].abs() < 8000, "the middle of the fade is a mix, not either track: {}", s[mid]);
        assert_eq!(s[..passed].iter().copied().collect::<std::collections::HashSet<_>>().len(), 1);
        assert!(s[s.len() - 10..].iter().all(|&v| v == -8000), "b plays alone after the fade");
        assert_eq!(d.discontinuities, 1, "one resync, for the mix's timestamps");
        assert_eq!(e.heard().next_id.as_deref(), Some("b"));
    }

    /// Feeds `data` in 4096-byte buffers, re-offering the rest of partly taken ones. Buffer `at_buffer`
    /// is only partly taken (1024 bytes); `between` runs between re-offers and after that buffer.
    fn offer(e: &mut TransitionEngine<u32>, d: &mut Down, h: &mut Host_, data: &[u8], at_buffer: usize, mut between: impl FnMut(&mut TransitionEngine<u32>)) {
        for (i, slice) in data.chunks(4096).enumerate() {
            if i == at_buffer {
                d.take_only = Some(1024);
            }
            let mut from = 0;
            loop {
                let (all, used) = e.handle_buffer(d, h, &slice[from..], FMT.us(i * 4096 + from));
                from += used;
                if all {
                    break;
                }
                between(e);
            }
            if i == at_buffer {
                between(e);
            }
        }
    }

    #[test]
    fn rescale_reaches_rest_of_partly_taken_buffer() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        e.set_gain(0.5);
        let mut changed = false;
        offer(&mut e, &mut d, &mut h, &tone(1000, 1.0), 10, |e| {
            if !std::mem::replace(&mut changed, true) {
                e.rescale(0, 0.25 / 0.5);
                e.set_gain(0.25);
            }
        });
        let s = d.samples();
        let cut = (10 * 4096 + 1024) / 2;
        assert_eq!(s.len(), tone(1000, 1.0).len() / 2, "every sample, once");
        assert!(s[..cut].iter().all(|&v| v == 500), "what was taken before the change at the old level");
        assert!(s[cut..].iter().all(|&v| v == 250), "and everything after it at the new one, the rest of that buffer too");
    }

    #[test]
    fn rescale_reaches_held_ending() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        d.position = Some(0);
        h.plans.insert("a".into(), fade("b", 2_000_000));
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        e.set_gain(0.5);
        feed(&mut e, &mut d, &mut h, &tone(1000, 3.0), 0);
        assert!(e.holding(), "the ending is held");
        e.rescale(0, 0.25 / 0.5);
        e.abandon_transition(&mut h);
        e.drain(&mut d);
        let s = d.samples();
        let held = FMT.bytes(2_000_000) / 2;
        assert!(s[..held].iter().all(|&v| v == 500) && s[held..].iter().all(|&v| v == 250) && s.len() > held, "held at 0.5, let go at 0.25");
    }

    #[test]
    fn no_plan_is_gapless() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(100, 1.0), 0);
        e.configure(&mut d, &mut h, stream("b", FMT), 2);
        e.handle_discontinuity(&mut d, &mut h);
        feed(&mut e, &mut d, &mut h, &tone(200, 1.0), 1_000_000);
        let s = d.samples();
        assert_eq!(s.len(), (RATE as usize * 2) * 2);
        assert_eq!(d.discontinuities, 1);
        assert_eq!(d.configured, vec![1], "same format: the output stays open");
    }

    #[test]
    fn unmixed_ending_is_released_before_output_runs_dry() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        h.plans.insert("a".into(), fade("b", 1_000_000));
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(1000, 3.0), 0);
        let before = d.samples().len();
        h.now += HOLD_GRACE_MS + 1;
        d.position = Some(900_000);
        e.position_us(&mut d, &mut h, false);
        assert!(h.log.iter().any(|l| l.contains("letting the ending play")), "{:?}", h.log);
        assert_eq!(d.samples().len() - before, FMT.bytes(2_000_000) / 2, "the held ending went out unmixed");
    }

    #[test]
    fn no_runway_means_no_hold() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        h.plans.insert("a".into(), fade("b", 1_000_000));
        d.position = Some(999_000);
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(1000, 3.0), 0);
        assert!(h.log.iter().any(|l| l.contains("no runway")), "{:?}", h.log);
        assert_eq!(d.samples().len(), (RATE as usize * 3) * 2, "everything played straight through");
    }

    #[test]
    fn seek_into_transition_mixes_from_there() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        h.plans.insert("a".into(), fade("b", 1_000_000));
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        // From 1.5 s: half a second into the fade.
        feed(&mut e, &mut d, &mut h, &tone(1000, 1.5), 1_500_000);
        assert!(h.log.iter().any(|l| l.contains("late hold, 500 ms in")), "{:?}", h.log);
        e.configure(&mut d, &mut h, stream("b", FMT), 2);
        e.handle_discontinuity(&mut d, &mut h);
        // Takeover is 1 s into the fade, so 1 s into b.
        assert_eq!(e.heard().next_from_us, 1_000_000, "the next song is entered where the fade hands it over");
    }

    #[test]
    fn heard_lags_reported_position_while_holding() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        h.plans.insert("a".into(), fade("b", 1_000_000));
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(1000, 3.0), 0);
        d.position = Some(500_000);
        let reported = e.position_us(&mut d, &mut h, false).unwrap();
        assert!(reported > 500_000, "the held ending counts as played so the next track is read in time");
        let heard = e.heard();
        assert_eq!(heard.id.as_deref(), Some("a"));
        assert_eq!(heard.us, 500_000);
        // The fade starts at 1 s and crosses over at its middle.
        assert!((heard.until_us - 2_000_000).abs() <= 23, "the ear leaves a where b becomes the louder, to the frame: {}", heard.until_us);
    }

    #[test]
    fn stretched_mix_returns_to_track_clock() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        let mut p = fade("b", 1_000_000);
        p.tempo_ratio = 1.03;
        p.ramp_us = 500_000;
        h.plans.insert("a".into(), p);
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(4000, 3.0), 0);
        e.configure(&mut d, &mut h, stream("b", FMT), 2);
        e.handle_discontinuity(&mut d, &mut h);
        feed(&mut e, &mut d, &mut h, &tone(-4000, 6.0), 3_000_000);
        assert!(d.discontinuities >= 2, "a resync into the mix and one back onto real timestamps: {}", d.discontinuities);
        let s = d.samples();
        assert!(s[s.len() - 10..].iter().all(|&v| (v + 4000).abs() <= 1), "after the stretch the track plays as it is");
        // Regression: the stretcher's last audio was stamped 0, pulling the clock back so the next
        // hold was never released. Timestamps must only move forward from the mix on.
        let from_mix: Vec<i64> = d.taken.iter().map(|(_, p)| *p).skip_while(|&p| p < 1_000_000).collect();
        assert!(from_mix.windows(2).all(|w| w[1] >= w[0]), "{from_mix:?}");
    }

    #[test]
    fn analysis_handed_over_on_next_stream() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(1000, 1.0), 0);
        e.configure(&mut d, &mut h, stream("b", FMT), 2);
        assert_eq!(h.analysed, vec!["a".to_string()]);
    }

    /// Seconds of output at `f` taken after the first `from` bytes.
    fn secs_after(d: &Down, from: usize, f: Format) -> f64 {
        let got = d.taken.iter().map(|(b, _)| b.len()).sum::<usize>() - from;
        got as f64 / f.frame_bytes() as f64 / f.rate as f64
    }

    fn taken(d: &Down) -> usize {
        d.taken.iter().map(|(b, _)| b.len()).sum()
    }

    #[test]
    fn stream_announced_after_boundary_is_converted() {
        // media3's order: discontinuity first, then the next format with its first buffer.
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(1000, 0.5), 0);
        e.handle_discontinuity(&mut d, &mut h);
        e.configure(&mut d, &mut h, stream("b", F48), 2);
        let before = taken(&d);
        feed_in(&mut e, &mut d, &mut h, &tone_at(500, 48_000, 1.0), F48, 1_000_000);
        assert_eq!(d.configured, vec![1], "the output stays as it was opened");
        let secs = secs_after(&d, before, FMT);
        assert!((secs - 1.0).abs() < 0.01, "one second of 48 kHz comes out as one second at 44.1: {secs}");
        assert!(h.log.iter().any(|l| l.starts_with("converting 48000 Hz")), "{:?}", h.log);
    }

    #[test]
    fn pinned_format_after_converted_stream_is_not_converted() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.configure(&mut d, &mut h, stream("a", F48), 1);
        feed_in(&mut e, &mut d, &mut h, &tone_at(1000, 48_000, 0.5), F48, 0);
        e.handle_discontinuity(&mut d, &mut h);
        e.configure(&mut d, &mut h, stream("b", FMT), 2);
        let before = taken(&d);
        feed(&mut e, &mut d, &mut h, &tone(700, 1.0), 1_000_000);
        let secs = secs_after(&d, before, F48);
        assert!((secs - 1.0).abs() < 0.01, "the 44.1 kHz song is converted: {secs}");
        e.handle_discontinuity(&mut d, &mut h);
        e.configure(&mut d, &mut h, stream("c", F48), 3);
        let before = taken(&d);
        let c = tone_at(300, 48_000, 1.0);
        feed_in(&mut e, &mut d, &mut h, &c, F48, 2_000_000);
        // The converter's leftover of b may come first; then c untouched.
        let got = taken(&d) - before;
        assert!(got <= c.len() + 64 && got + 64 >= c.len(), "c goes down sample for sample: {got} of {} bytes", c.len());
        let s = d.samples();
        assert!(s[s.len() - 1000..].iter().all(|&v| v == 300));
        assert_eq!(d.configured, vec![1]);
    }

    #[test]
    fn mix_into_late_announced_stream_converts_it() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        h.plans.insert("a".into(), fade("b", 1_000_000));
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(8000, 3.0), 0);
        e.handle_discontinuity(&mut d, &mut h);
        e.configure(&mut d, &mut h, stream("b", F48), 2);
        feed_in(&mut e, &mut d, &mut h, &tone_at(-8000, 48_000, 3.0), F48, 3_000_000);
        let frames = d.samples().len() / 2;
        // 1 s alone + b's 3 s, less the converter's lookahead (< 2 ms).
        let want = RATE as usize * 4;
        assert!(frames.abs_diff(want) < 160, "{frames} frames, not {want}");
        let s = d.samples();
        // Dithered after conversion: within a step.
        assert!(s[s.len() - 1000..].iter().all(|&v| (v + 8000).abs() <= 1), "b plays alone after the fade");
        assert_eq!(d.configured, vec![1]);
    }

    #[test]
    fn stretched_mix_into_late_announced_stream() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        let mut p = fade("b", 1_000_000);
        p.tempo_ratio = 1.03;
        p.ramp_us = 500_000;
        h.plans.insert("a".into(), p);
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(4000, 3.0), 0);
        e.handle_discontinuity(&mut d, &mut h);
        e.configure(&mut d, &mut h, stream("b", F48), 2);
        feed_in(&mut e, &mut d, &mut h, &tone_at(-4000, 48_000, 6.0), F48, 3_000_000);
        let frames = d.samples().len() / 2;
        // 1 s alone, then 6 s of b with the first 2.5 s up to 3 % fast.
        let secs = frames as f64 / RATE as f64;
        assert!((6.8..7.05).contains(&secs), "{secs} s");
        let s = d.samples();
        assert!(s[s.len() - 10..].iter().all(|&v| (v + 4000).abs() <= 1), "after the stretch the track plays as it is");
    }

    #[test]
    fn gapless_follows_next_rate() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.follow_rate = true;
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(1000, 1.0), 0);
        e.configure(&mut d, &mut h, stream("b", F48), 2);
        e.handle_discontinuity(&mut d, &mut h);
        let before = taken(&d);
        let b = sine(HZ, 48_000, 1.0, 0);
        feed_in(&mut e, &mut d, &mut h, &b, F48, 1_000_000);
        assert_eq!(d.configured, vec![1, 2], "opened again for b");
        let got: Vec<u8> = d.taken.iter().flat_map(|t| t.0.iter().copied()).skip(before).collect();
        assert!(got == b, "b sample for sample");
        assert!(!h.log.iter().any(|l| l.contains("converting")), "{:?}", h.log);
        // Mixed into, it is still converted.
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.follow_rate = true;
        h.plans.insert("a".into(), fade("b", 1_000_000));
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(8000, 3.0), 0);
        e.handle_discontinuity(&mut d, &mut h);
        e.configure(&mut d, &mut h, stream("b", F48), 2);
        feed_in(&mut e, &mut d, &mut h, &tone_at(-8000, 48_000, 3.0), F48, 3_000_000);
        assert_eq!(d.configured, vec![1], "the mix at a's rate");
        assert!(h.log.iter().any(|l| l.contains("converting 48000 Hz x2 -> 44100 Hz x2")), "{:?}", h.log);
    }

    #[test]
    fn converter_carries_on_across_gapless_join() {
        // a (44.1) mixes into b (48, converted); c (48) follows b gaplessly. The converter must carry
        // on (no reopen, no reset click): a tone through b into c stays one tone.
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        e.follow_rate = true;
        h.plans.insert("a".into(), fade("b", 1_000_000));
        e.configure(&mut d, &mut h, stream("a", FMT), 1);
        feed(&mut e, &mut d, &mut h, &tone(0, 3.0), 0);
        e.handle_discontinuity(&mut d, &mut h);
        e.configure(&mut d, &mut h, stream("b", F48), 2);
        let b = sine(HZ, 48_000, 4.0, 0);
        feed_in(&mut e, &mut d, &mut h, &b, F48, 3_000_000);
        e.configure(&mut d, &mut h, stream("c", F48), 3);
        e.handle_discontinuity(&mut d, &mut h);
        let from = d.samples().len() / 2;
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 1.0, 48_000 * 4), F48, 7_000_000);
        assert_eq!(d.configured, vec![1], "no opening again between b and c");
        assert_eq!(h.log.iter().filter(|l| l.contains("converting 48000")).count(), 1, "one converter, b's: {:?}", h.log);
        let left: Vec<f64> = d.samples().chunks_exact(2).map(|c| c[0] as f64).collect();
        let own = 8000.0 * std::f64::consts::TAU * HZ / RATE as f64;
        let near = &left[from - 2_000..from + 2_000];
        let step = near.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        assert!(step <= own * 1.01 + 2.0, "a step of {step:.0} at the join, the tone's own {own:.0}");
        let hz = last_second_hz(&d, FMT);
        assert!((hz - HZ).abs() < 1.0, "c at its own pitch, converted: {hz}");
    }

    // ---- tempo after a beat-matched mix, a seek, a skip or a pause ----

    const F48: Format = Format { rate: 48_000, ..FMT };
    const HZ: f64 = 1000.0;

    /// `secs` of a stereo 16-bit sine of `hz` at `rate`, starting at frame `from`.
    fn sine(hz: f64, rate: u32, secs: f64, from: usize) -> Vec<u8> {
        let frames = (rate as f64 * secs) as usize;
        (from..from + frames)
            .flat_map(|k| {
                let v = ((k as f64 * hz * std::f64::consts::TAU / rate as f64).sin() * 8000.0).round() as i16;
                [v, v]
            })
            .flat_map(i16::to_le_bytes)
            .collect()
    }

    /// Pitch of the last second taken, from zero crossings.
    fn last_second_hz(d: &Down, f: Format) -> f64 {
        let s = d.samples();
        let left: Vec<i16> = s.chunks_exact(2).map(|c| c[0]).collect();
        let w = &left[left.len() - f.rate as usize..];
        w.windows(2).filter(|p| (p[0] < 0) != (p[1] < 0)).count() as f64 / 2.0
    }

    /// A 2 s beat-matched mix into `c` 1 s in, `c` 4 % fast (pitch too), ramped back over 1 s.
    fn beat_matched() -> Plan {
        Plan { tempo_ratio: 1.04, keep_pitch: false, ramp_us: 1_000_000, ..fade("c", 1_000_000) }
    }

    /// In media3's order: `a` (48 kHz) pins the output, `b` (44.1) is converted, `b` mixes into `c` (48);
    /// returns with `secs` of `c` fed.
    fn into_the_mix(e: &mut TransitionEngine<u32>, d: &mut Down, h: &mut Host_, secs: f64) {
        h.plans.insert("b".into(), beat_matched());
        e.configure(d, h, stream("a", F48), 1);
        feed_in(e, d, h, &sine(HZ, 48_000, 1.0, 0), F48, 0);
        e.handle_discontinuity(d, h);
        e.configure(d, h, stream("b", FMT), 2);
        e.set_output_stream_offset_us(1_000_000);
        feed_in(e, d, h, &sine(HZ, RATE, 3.0, 0), FMT, 1_000_000);
        assert!(h.log.iter().any(|l| l.starts_with("holding the ending")), "{:?}", h.log);
        e.handle_discontinuity(d, h);
        e.configure(d, h, stream("c", F48), 3);
        e.set_output_stream_offset_us(4_000_000);
        feed_in(e, d, h, &sine(HZ, 48_000, secs, 0), F48, 4_000_000);
    }

    fn assert_own_pitch(d: &Down, what: &str) {
        let hz = last_second_hz(d, F48);
        assert!((hz - HZ).abs() < 3.0, "{what}: the song plays at {hz} Hz, not {HZ} (x{:.4})", hz / HZ);
    }

    #[test]
    fn tempo_restored_after_beat_matched_mix() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        into_the_mix(&mut e, &mut d, &mut h, 6.0);
        assert_own_pitch(&d, "after the mix and the ramp");
    }

    #[test]
    fn tempo_restored_after_seek_in_mix() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        into_the_mix(&mut e, &mut d, &mut h, 0.5);
        e.flush(&mut h);
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 3.0, 48_000 * 20), F48, 24_000_000);
        assert_own_pitch(&d, "after a seek in the mix");
    }

    #[test]
    fn tempo_restored_after_seek_in_ramp() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        // 0.5 s after the 2 s mix: inside the ramp.
        into_the_mix(&mut e, &mut d, &mut h, 2.5);
        e.flush(&mut h);
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 3.0, 48_000 * 20), F48, 24_000_000);
        assert_own_pitch(&d, "after a seek in the ramp");
    }

    #[test]
    fn tempo_restored_after_skip_in_mix() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        into_the_mix(&mut e, &mut d, &mut h, 0.5);
        e.flush(&mut h);
        e.configure(&mut d, &mut h, stream("d", F48), 4);
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 3.0, 0), F48, 30_000_000);
        assert_own_pitch(&d, "the song skipped to");
        e.flush(&mut h);
        e.configure(&mut d, &mut h, stream("c", F48), 5);
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 3.0, 0), F48, 40_000_000);
        assert_own_pitch(&d, "the song skipped back to");
    }

    #[test]
    fn tempo_restored_after_pause_in_mix() {
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        into_the_mix(&mut e, &mut d, &mut h, 0.5);
        // Paused: only position queries, nothing flowing.
        for _ in 0..50 {
            h.now += 100;
            e.position_us(&mut d, &mut h, false);
        }
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 5.0, 24_000), F48, 4_500_000);
        assert_own_pitch(&d, "after a pause in the mix");
    }

    #[test]
    fn tempo_restored_after_seek_in_mix_announced_first() {
        // Our pipeline's order: the next song announced before the boundary.
        let (mut e, mut d, mut h) = (TransitionEngine::<u32>::new(), Down::default(), Host_::default());
        h.plans.insert("b".into(), beat_matched());
        e.configure(&mut d, &mut h, stream("a", F48), 1);
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 1.0, 0), F48, 0);
        e.configure(&mut d, &mut h, stream("b", FMT), 2);
        e.handle_discontinuity(&mut d, &mut h);
        e.set_output_stream_offset_us(1_000_000);
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, RATE, 3.0, 0), FMT, 1_000_000);
        e.configure(&mut d, &mut h, stream("c", F48), 3);
        e.handle_discontinuity(&mut d, &mut h);
        e.set_output_stream_offset_us(4_000_000);
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 0.5, 0), F48, 4_000_000);
        e.flush(&mut h);
        feed_in(&mut e, &mut d, &mut h, &sine(HZ, 48_000, 3.0, 48_000 * 20), F48, 24_000_000);
        assert_own_pitch(&d, "after a seek in the mix");
    }
}
