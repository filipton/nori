//! The engine's thread and the handle a client drives it with. Playback itself is
//! `nori_player::pipeline`; this adds the real clock, the controls' fades, audio offload and the events.
//!
//! The thread sleeps unless something is due. It wakes for a command, the ring reaching its low mark
//! (once per 10 s burst; half the shallow ring while the equalizer is tuned), a song's bytes arriving,
//! and timed moments computed each turn (a fade's end, the next song becoming audible, every 250 ms
//! through a mix, the end of the queue, position events when asked for). Paused, it sleeps until a
//! command; paused for [`Config::idle_release_ms`] it lets the output and the song's bytes go.
//!
//! With an [`OffloadOutput`] and nothing that touches samples, songs go to the output's decoder as
//! packets and the thread sleeps minutes between top-ups. Offload starts at the playback position behind a dip,
//! or at the next song when the current one is not decodable there, and stops at once when something
//! needs the samples.
//!
//! A change to the sound while the CPU plays (equalizer, limiter, speed, silence skipping, high quality
//! output, tuning's shallow buffer) remakes what the ring and device hold from the playback position, behind
//! a 30 ms dip, at most every 150 ms. The song is opened [`REMAKE_LEAD_MS`] ahead of playback first,
//! while the output plays on, so reopening it is never heard as a gap; leaving offload works the same way.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

use nori_player::pcm::Encoding;
use nori_player::pipeline::{App, Player, Queue, Reading, Songs, Sound, Track};
use nori_player::playlist::Playlist;
use nori_player::policy::{audio_policy, offload_blocked, AudioPrefs, OutputState};
use nori_player::queue::previous_restarts;
use nori_player::transport::{load_control, pause_fade, play_fade, skip_plays, switch_dip, Switch, IDLE_RELEASE_MS};
use parking_lot::Mutex;

use crate::clock::{Clock, Monotonic};
use crate::demux::Demuxed;
use crate::library::{Library, Sources};
use crate::offload::{Offload, OffloadOutput, OnCpu, Step, Tail};
use crate::output::{AudioOutput, Device, RingTrack, SHALLOW_US, WAKE_LOW_US};
use crate::panic_words;

/// The sound and controls the settings ask for.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub sound: Sound,
    pub speed: f32,
    pub pitch: f32,
    pub skip_silence: bool,
    /// Fade on play, pause and switches, ms (0 off).
    pub fade_ms: i32,
    /// High quality output: float decoding and chain, into a device that plays float
    /// (`nori_player::policy`); a 16-bit device gets the dithered 16-bit chain.
    pub hi_res: bool,
    /// Highest device rate, Hz (0: the song's own); higher songs are converted down within their rate
    /// family (`nori_player::policy::capped_rate`). Not applied to bit-perfect output.
    pub max_rate: u32,
    /// Let an [`OffloadOutput`] decode songs when nothing needs the samples.
    pub offload: bool,
    /// Crossfade (s, 0 off) and AutoMix; both touch samples, so offload stands down.
    pub crossfade_s: i32,
    pub auto_mix: bool,
    /// Most ReplayGain may turn a song up, dB (`nori_player::gain`); 0 when it only turns down. Above 0,
    /// songs are read as floats with the limiter behind them, and a song turned up stays off offload.
    pub gain_boost_db: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { sound: Sound::default(), speed: 1.0, pitch: 1.0, skip_silence: false, fade_ms: 0, hi_res: false, max_rate: 0, offload: false, crossfade_s: 0, auto_mix: false, gain_boost_db: 0.0 }
    }
}

/// What the platform knows of the output: a USB device (offload cannot reach it) and a bit-perfect DAC
/// (no chain, ReplayGain or conversion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputFacts {
    pub usb: bool,
    pub bit_perfect: bool,
}

/// The settings as the audio policy lets them through.
#[derive(Clone, PartialEq)]
struct Applied {
    sound: Sound,
    speed: (f32, f32),
    skip_silence: bool,
    untouched: bool,
    bit_perfect: bool,
    float: bool,
    /// Most a song is turned up, linear (1: never).
    gain_max: f32,
    max_rate: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Nothing loaded.
    Idle,
    Playing,
    Paused,
    /// Played to the end of the queue.
    Ended,
}

/// What the engine tells a client, each once as it changes. The status ([`Engine::status`]) is written
/// before an event is said.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    State(State),
    /// The audible song changed (through a mix, when the next song becomes audible). `jumps` is how
    /// many jumps ([`Engine::play_at`], [`Engine::go_to`], [`Engine::next`], [`Engine::previous`]) had
    /// been made: a client that asked for a later jump knows this event predates it.
    Song { index: usize, id: String, jumps: u64 },
    /// The song restarted by itself (repeat one).
    Looped { index: usize, id: String, jumps: u64 },
    /// The playback position: at the pace set by [`Engine::position_updates`], and once when a seek lands.
    Position { index: usize, ms: i64 },
    /// A song would not play (skipped or stopped per the queue's rules), or the output would not open
    /// (`id` empty).
    Error { id: String, message: String },
    /// The music goes to another output device, by the core's device name.
    Output { name: String },
    /// Playback ran dry waiting for a song's bytes (`true`), or resumed (`false`).
    Buffering(bool),
    /// Playback stopped by itself (the queue's rules after songs that would not play), before the
    /// `Paused` state. `plays` is how many [`Engine::play`] calls the engine had taken: see
    /// [`Engine::superseded`].
    Stopped { plays: u64 },
    /// A live stream's announced title (ICY), when playback reaches it.
    Title(String),
    /// A mix (AutoMix, crossfade) became audible (`true`) or ended.
    Mixing(bool),
    /// A song failed for want of network and the offline bridge is to take over (the queue's rules):
    /// playback waits paused for its jump. `plays` as for [`Event::Stopped`].
    Bridge { plays: u64 },
    /// The song moved between the output's decoder and the CPU, or a mix's tempo ended, without a jump:
    /// the position is `ms` in `index`. A client that extrapolates the position (media3) re-anchors here.
    Placed { index: usize, ms: i64 },
    /// Whether the CPU must be kept awake while playing (`true`) or may sleep until the platform wakes
    /// the engine (offloaded and fed, `Offload::lets_cpu_sleep`). Said on the engine's thread before the
    /// work it is for, so a wake lock can follow it.
    Awake(bool),
}

/// The engine's last reading, for a screen to read without waking it.
#[derive(Debug, Clone)]
pub struct Status {
    pub state: State,
    /// The audible song: queue index and id.
    pub index: Option<usize>,
    pub id: Option<String>,
    /// Position in it at `at`, ms.
    pub position_ms: i64,
    pub at: Instant,
    pub speed: f32,
    /// How fast the position moves: speed times the tempo a mix plays the song at.
    pub pace: f32,
    pub mixing: bool,
    /// Pulls that found the ring short while music was due.
    pub underruns: u64,
    /// Times the output was let go after a long pause.
    pub releases: u64,
    /// A switch is waiting out its dip: the position is still the old one.
    pub switching: bool,
    /// The sound chain is in the samples' path.
    pub chain: bool,
    /// Audible music comes from the CPU path (not offloaded, released, paused or buffering): the only
    /// time `chain` means anything.
    pub on_cpu: bool,
    /// What the limiter took off the last buffer, dB.
    pub gain_reduction_db: f32,
    /// What the compressor took off the last buffer, dB (`Sink::compression_db`).
    pub compression_db: f32,
    /// Songs go to the output's decoder.
    pub offloaded: bool,
    /// The settings allow offload, as last applied.
    pub offload_wanted: bool,
    /// Why the music is on the CPU rather than offloaded, for a report. None with nothing loaded and while
    /// offloaded, except for a song offloaded with its encoder gap left in.
    pub pcm_why: Option<String>,
    /// As last said by [`Event::Awake`].
    pub awake: bool,
}

impl Status {
    /// The position now: the last reading moved on at its pace while playing.
    pub fn position_now(&self) -> i64 {
        match self.state {
            State::Playing => self.position_ms + (self.at.elapsed().as_secs_f64() * 1000.0 * self.pace as f64) as i64,
            _ => self.position_ms,
        }
    }

    /// The position for a seek bar, and whether the reading is old enough to ask for a new one
    /// ([`Engine::look`]); see `nori_player::heard::screen_place`.
    pub fn screen_now(&self) -> (i64, bool) {
        nori_player::heard::screen_place(self.position_ms, self.at.elapsed().as_millis() as i64, self.pace, self.state == State::Playing)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Memory the platform gives the app, MB: sizes how much of a song is kept loaded
    /// (`nori_player::transport::load_control`).
    pub memory_mb: u32,
    pub settings: Settings,
    /// Paused this long, the output and the song's bytes are let go, ms.
    pub idle_release_ms: i64,
    /// Told what each wake saw ([`crate::watch`]).
    pub watch: Option<crate::watch::Watcher>,
}

impl Default for Config {
    fn default() -> Self {
        Config { memory_mb: 256, settings: Settings::default(), idle_release_ms: IDLE_RELEASE_MS, watch: None }
    }
}

enum Command {
    PlayAt(usize, i64),
    GoTo(usize, i64),
    PauseAtEnd(bool),
    Play,
    /// Pause with this fade (`None`: the settings' fade).
    Pause(Option<i32>),
    Toggle,
    Next,
    Previous,
    Seek(i64),
    Settings(Box<Settings>),
    Output(OutputFacts),
    Replan,
    QueueChanged,
    Repeat(u8),
    Gain,
    Tuning(bool),
    Positions(Option<Duration>),
    /// Only wake: the turn reads the output and writes the status.
    Look,
    Device(Device),
    Stop,
}

/// What a switch does at the bottom of its dip.
#[derive(Debug, Clone, Copy)]
enum Switched {
    To(usize, i64),
    Next,
    Previous,
    Seek(i64),
    /// Remake the music from the playback position ([`Worker::resound_soon`]), or hand it to the output's decoder.
    Resound,
}

/// A fade down to silence, the switches made at its bottom, and the fade back up.
struct Dip {
    /// When it is down (engine ms).
    at: i64,
    up_ms: i64,
    then: Vec<Switched>,
}

impl Dip {
    /// Jumps waiting in it.
    fn jumps(&self) -> usize {
        self.then.iter().filter(|s| matches!(s, Switched::To(..) | Switched::Next | Switched::Previous)).count()
    }

    fn resounds(&self) -> bool {
        self.then.iter().any(|s| matches!(s, Switched::Resound))
    }
}

/// The handle: every call sends a command, wakes the engine's thread and returns at once.
pub struct Engine {
    tx: Sender<Command>,
    thread: Thread,
    join: Mutex<Option<JoinHandle<()>>>,
    status: Arc<Mutex<Status>>,
    /// Jumps asked for so far ([`Event::Song`]).
    jumps: AtomicU64,
    /// Plays asked for so far ([`Event::Stopped`]).
    plays: AtomicU64,
    /// How a command wakes the thread on a test's clock ([`Engine::start_on`]).
    wake: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Engine {
    /// Starts the engine's thread over `library`, `app` (transition planner and log) and `queue`,
    /// playing through `output`, or through `offload` when the settings allow. `events` runs on the
    /// engine's thread and should only hand the event on.
    pub fn start<L, A, Q, E>(library: L, app: A, queue: Q, output: Box<dyn AudioOutput>, offload: Option<Box<dyn OffloadOutput>>, config: Config, events: E) -> Engine
    where
        L: Library,
        A: App + Send + 'static,
        Q: Queue + Send + 'static,
        E: FnMut(Event) + Send + 'static,
    {
        Engine::launch(library, app, queue, output, offload, config, Monotonic::new(), false, events)
    }

    /// [`Engine::start`] on `clock`, a test's clock moved by hand: every command goes through
    /// [`Clock::wake`].
    #[allow(clippy::too_many_arguments)]
    pub fn start_on<L, A, Q, E, C>(library: L, app: A, queue: Q, output: Box<dyn AudioOutput>, offload: Option<Box<dyn OffloadOutput>>, config: Config, clock: C, events: E) -> Engine
    where
        L: Library,
        A: App + Send + 'static,
        Q: Queue + Send + 'static,
        E: FnMut(Event) + Send + 'static,
        C: Clock + Sync,
    {
        Engine::launch(library, app, queue, output, offload, config, clock, true, events)
    }

    #[allow(clippy::too_many_arguments)]
    fn launch<L, A, Q, E, C>(library: L, app: A, queue: Q, output: Box<dyn AudioOutput>, offload: Option<Box<dyn OffloadOutput>>, config: Config, clock: C, hooked: bool, events: E) -> Engine
    where
        L: Library,
        A: App + Send + 'static,
        Q: Queue + Send + 'static,
        E: FnMut(Event) + Send + 'static,
        C: Clock + Sync,
    {
        let (tx, rx) = channel();
        let status = Arc::new(Mutex::new(Status {
            state: State::Idle,
            index: None,
            id: None,
            position_ms: 0,
            at: Instant::now(),
            speed: 1.0,
            pace: 1.0,
            mixing: false,
            underruns: 0,
            releases: 0,
            switching: false,
            chain: false,
            on_cpu: false,
            gain_reduction_db: 0.0,
            compression_db: 0.0,
            offloaded: false,
            offload_wanted: false,
            pcm_why: None,
            awake: true,
        }));
        let shared = status.clone();
        let devices = tx.clone();
        let hook = clock.clone();
        let own = clock.clone();
        let join = std::thread::Builder::new()
            .name("nori-engine".into())
            .spawn(move || {
                let me = std::thread::current();
                let mut output = output;
                // Device changes arrive on the output's own thread.
                let wake = me.clone();
                output.watch(Box::new(move |d| {
                    if devices.send(Command::Device(d)).is_ok() {
                        own.wake(&wake);
                    }
                }));
                let songs = Sources::new(library, load_control(config.memory_mb), clock.waits(), me);
                let mut player = Player::build(songs, queue, app, RingTrack::new(output));
                player.shallow_us = SHALLOW_US;
                Worker::new(player, offload.map(Offload::new), rx, events, shared, config.settings, config.idle_release_ms, config.watch, clock).run();
            })
            .expect("a thread for the engine");
        let thread = join.thread().clone();
        let wake = hooked.then(|| {
            let t = thread.clone();
            Box::new(move || hook.wake(&t)) as Box<dyn Fn() + Send + Sync>
        });
        Engine { tx, thread, join: Mutex::new(Some(join)), status, jumps: AtomicU64::new(0), plays: AtomicU64::new(0), wake }
    }

    fn send(&self, c: Command) {
        if self.tx.send(c).is_ok() {
            match &self.wake {
                Some(w) => w(),
                None => self.thread.unpark(),
            }
        }
    }

    /// Sends a jump; returns its number ([`Event::Song`]'s `jumps`).
    fn jump(&self, c: Command) -> u64 {
        let n = self.jumps.fetch_add(1, Ordering::AcqRel) + 1;
        self.send(c);
        n
    }

    /// Plays queue index `index` from `ms`. Returns the jump's number.
    pub fn play_at(&self, index: usize, ms: i64) -> u64 {
        self.jump(Command::PlayAt(index, ms))
    }

    /// Goes to queue index `index` at `ms`, playing or paused as before. Paused, the place is held and
    /// nothing is fetched until play. Returns the jump's number.
    pub fn go_to(&self, index: usize, ms: i64) -> u64 {
        self.jump(Command::GoTo(index, ms))
    }

    /// Pauses at the end of the current song, on the next one at its start (the sleep timer's "end of
    /// this song"); nothing after it is read or mixed into. `false` cancels.
    pub fn pause_at_end(&self, on: bool) {
        self.send(Command::PauseAtEnd(on));
    }

    /// Plays from the current place (or where a jump or stop left it). Returns the play's number
    /// ([`Event::Stopped`]).
    pub fn play(&self) -> u64 {
        let n = self.plays.fetch_add(1, Ordering::AcqRel) + 1;
        self.send(Command::Play);
        n
    }

    /// Whether `event` is a stop ([`Event::Stopped`], [`Event::Bridge`]) from before a later
    /// [`Engine::play`], which restarts the music: a client should ignore it.
    pub fn superseded(&self, event: &Event) -> bool {
        match event {
            Event::Stopped { plays } | Event::Bridge { plays } => *plays < self.plays.load(Ordering::Acquire),
            _ => false,
        }
    }

    pub fn pause(&self) {
        self.send(Command::Pause(None));
    }

    /// Pauses at once, whatever the fade setting (headphones pulled out), cutting a fade short.
    pub fn pause_now(&self) {
        self.send(Command::Pause(Some(0)));
    }

    pub fn toggle(&self) {
        self.send(Command::Toggle);
    }

    /// The next song; paused, it also starts playback (`nori_player::transport::skip_plays`).
    pub fn next(&self) -> u64 {
        self.jump(Command::Next)
    }

    /// Previous: restarts the song a few seconds in; paused, it also starts playback.
    pub fn previous(&self) -> u64 {
        self.jump(Command::Previous)
    }

    pub fn seek(&self, ms: i64) {
        self.send(Command::Seek(ms));
    }

    pub fn set_settings(&self, settings: Settings) {
        self.send(Command::Settings(Box::new(settings)));
    }

    pub fn set_output(&self, facts: OutputFacts) {
        self.send(Command::Output(facts));
    }

    /// The transition plan out of the current song is asked for again (settings or an analysis changed).
    pub fn replan(&self) {
        self.send(Command::Replan);
    }

    /// The queue was edited: what plays next and the planner's window follow.
    pub fn queue_changed(&self) {
        self.send(Command::QueueChanged);
    }

    /// Repeat off, one or all (`nori_player::playlist::REPEAT_*`).
    pub fn set_repeat(&self, mode: u8) {
        self.send(Command::Repeat(mode));
    }

    /// The ReplayGain settings changed.
    pub fn gain_changed(&self) {
        self.send(Command::Gain);
    }

    /// The equalizer screen opened (`true`) or closed: the output is kept shallow while it is open, so a
    /// band moved is heard at once (`nori_player::transport::Chain::tuning`).
    pub fn set_tuning(&self, on: bool) {
        self.send(Command::Tuning(on));
    }

    /// Position events this often while playing, or none (the default).
    pub fn position_updates(&self, every: Option<Duration>) {
        self.send(Command::Positions(every));
    }

    /// Reads the output once now and updates the status: for a screen coming back, or a seek bar whose
    /// reading is old ([`Status::screen_now`]). Offloaded, the engine may sleep for minutes.
    pub fn look(&self) {
        self.send(Command::Look);
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    /// Reads the status in place, without cloning it: for a question asked every frame.
    pub fn status_with<R>(&self, f: impl FnOnce(&Status) -> R) -> R {
        f(&self.status.lock())
    }

    /// Stops the thread and lets the output go.
    pub fn stop(&self) {
        self.send(Command::Stop);
        let join = self.join.lock().take();
        if let Some(j) = join {
            let _ = j.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker<L: Library, A: App, Q: Queue, E: FnMut(Event), C: Clock> {
    p: Player<Sources<L>, RingTrack, A, Q>,
    /// The offload path, when the platform has an output that decodes songs itself.
    off: Option<Offload>,
    rx: Receiver<Command>,
    events: E,
    status: Arc<Mutex<Status>>,
    clock: C,
    settings: Settings,
    applied: Option<Applied>,
    state: State,
    /// When a pause's fade ends.
    pause_at: Option<i64>,
    dip: Option<Dip>,
    /// The song last reported, and its id (a new queue can put another song at the same index).
    heard: Option<usize>,
    heard_id: Option<String>,
    /// Jumps and plays taken off the channel.
    jumps: u64,
    plays: u64,
    /// The ReplayGain settings changed.
    gain_changed: bool,
    /// Position event interval and the next one due, ms.
    positions: Option<i64>,
    next_position: i64,
    idle_release_ms: i64,
    watch: Option<crate::watch::Watcher>,
    /// Paused: when the output is let go; once it is, where the player was.
    idle_at: Option<i64>,
    released: Option<(usize, i64)>,
    releases: u64,
    /// [`Event::Buffering`] as last said.
    stalled: bool,
    /// A jump or seek made while paused (queue index, ms, id): held until play, so nothing is fetched
    /// for a place that may change again. The status shows it at once.
    held: Option<(usize, i64, String)>,
    facts: OutputFacts,
    /// The settings and output allow offload; if not, why.
    offload: bool,
    blocked: Option<&'static str>,
    /// Offload given up for the engine's life: the track was torn down [`TEAR_DOWNS`] times.
    offload_refused: bool,
    tear_downs: u32,
    /// The next song opened as packets to see whether the output decodes it (queue index, the opening,
    /// the answer once known). If it does, the CPU plays the current song to its end and hands over.
    probe: Option<(usize, Result<Demuxed, String>, Option<bool>)>,
    /// The CPU plays this song to its end, then the output's decoder takes the next.
    handing_over: Option<usize>,
    /// Repeat-one loops, and the offloaded song's placing, as last reported.
    loops: u32,
    heard_seq: u64,
    /// A live stream's title, and when playback reaches it.
    title: Option<(String, i64)>,
    /// The queue's ids as last seen, for the offload path to find its songs again.
    ids: Vec<String>,
    /// Music was heard since the offload path started (ends a run of failing songs).
    offload_heard: bool,
    /// The current CPU song opened as packets now that offload is wanted (queue index, the opening,
    /// whether the settings change also changed the sound).
    entering: Option<(usize, Result<Demuxed, String>, bool)>,
    /// The output decodes the current song: the next `Switched::Resound` hands it over.
    offload_now: bool,
    /// When the music is to be remade ([`Worker::resound_soon`]), and when it last was.
    resound_due: Option<i64>,
    resounded_at: i64,
    /// Say [`Event::Placed`] at the next report.
    placed_due: bool,
    /// Say [`Event::Position`] at the next report.
    seek_landed: bool,
    /// The song plays at a mix's tempo: [`Event::Placed`] is said when it is back at its own.
    stretched: bool,
    remake: Option<Remake>,
    /// Panicked turns within the last [`PANICS_WITHIN_MS`].
    panics: VecDeque<i64>,
    /// Where the position stands still while music should move ([`Worker::restart_if_stalled`]).
    stall: Option<Stall>,
    /// The song last restarted from scratch: the same song again with nothing heard since has failed.
    restarted: Option<String>,
    /// A jump under way (queue index, ms): a panic during it restarts there.
    jumping: Option<(usize, i64)>,
    /// As last said by [`Event::Awake`].
    awake: bool,
}

/// The current song opened ahead of playback, to remake the music from there ([`Worker::remake`]) while
/// the output plays what it holds.
struct Remake {
    id: String,
    from_ms: i64,
    r: Demuxed,
    /// Open, with its first bytes here.
    ready: bool,
    /// When the dip starts, so playback is at `from_ms` at its bottom (engine ms).
    dip_at: i64,
    /// Takes the song over from the output's decoder.
    leaving: bool,
}

enum Turn {
    Stop,
    /// Sleep this long (ms), or until woken.
    Sleep(Option<i64>),
}

const PANICS_WITHIN_MS: i64 = 60_000;
/// This many panics within [`PANICS_WITHIN_MS`] stop playback: the thread waits for a command instead
/// of panicking every turn.
const PANICS_KEPT_ON: usize = 3;

/// Playing, with the position still this long and no bytes on their way, the music is restarted from
/// scratch ([`Worker::start_again`]): a net under outputs that stop taking music without saying so.
const STALL_RESTART_MS: i64 = 10_000;
/// A standing position is looked at once more at this point, so a watching client sees it.
const STALL_SAY_MS: i64 = 5_000;
/// Playing on the CPU with nothing else due, the thread still wakes this often to check the position.
/// Longer than any burst cycle, so it never fires while music plays.
const STALL_GUARD_MS: i64 = 30_000;

/// Where the position stood still, and since when.
#[derive(Debug, Clone, PartialEq)]
struct Stall {
    since: i64,
    /// Queue index and ms.
    place: (Option<usize>, i64),
    /// The position did not move at the last look.
    standing: bool,
}

/// Less than this to play while a song's bytes are on their way is [`Event::Buffering`].
const STALL_US: i64 = 200_000;
/// A track torn down this many times gives offload up for the engine's life.
const TEAR_DOWNS: u32 = 2;
/// The dip the music is remade behind, down and up, ms: no click, and too short to notice.
const RESOUND_DIP_MS: i64 = 30;
/// Sound changes closer together than this (a slider dragged) are remade together.
const RESOUND_EVERY_MS: i64 = 150;
/// Most music the ring and device may hold while tuned for a band moved to be heard as it is; more is
/// left from the deep buffer and is remade.
const TUNED_HELD_US: i64 = 400_000;
/// Room for the device's own latency beyond the shallow ring and shallow device.
const TUNED_SLACK_US: i64 = 160_000;
/// The equalizer screen turns tuning on with its first change, which may reach the engine first and be
/// remade into the deep buffer: tuning within this of a remake remakes once more, into the shallow one.
const TUNED_AFTER_RESOUND_MS: i64 = 1_000;
/// A device holding more than this keeps enough of the old ReplayGain level to hear: remade.
const HELD_US: i64 = 250_000;
/// How far ahead of playback the song is opened for a remake. The output plays on meanwhile and is
/// emptied behind the dip once playback gets there, so reopening the song is never a gap.
pub const REMAKE_LEAD_MS: i64 = 120;
/// While a song is opened ahead, the current one is not read on while the output holds this much: the
/// two would pull the same song's fetch back and forth.
const REMAKE_HOLD_US: i64 = 1_000_000;

impl<L: Library, A: App, Q: Queue, E: FnMut(Event), C: Clock> Worker<L, A, Q, E, C> {
    #[allow(clippy::too_many_arguments)]
    fn new(p: Player<Sources<L>, RingTrack, A, Q>, off: Option<Offload>, rx: Receiver<Command>, events: E, status: Arc<Mutex<Status>>, settings: Settings, idle_release_ms: i64, watch: Option<crate::watch::Watcher>, clock: C) -> Self {
        let ids = p.queue.read(|q| q.ids().to_vec());
        let mut w = Worker {
            p,
            off,
            rx,
            events,
            status,
            clock,
            settings: Settings::default(),
            applied: None,
            state: State::Idle,
            pause_at: None,
            dip: None,
            heard: None,
            heard_id: None,
            jumps: 0,
            plays: 0,
            gain_changed: false,
            positions: None,
            next_position: 0,
            idle_release_ms,
            watch,
            idle_at: None,
            released: None,
            releases: 0,
            stalled: false,
            held: None,
            facts: OutputFacts::default(),
            offload: false,
            blocked: None,
            offload_refused: false,
            tear_downs: 0,
            probe: None,
            handing_over: None,
            loops: 0,
            heard_seq: 0,
            title: None,
            ids,
            offload_heard: false,
            entering: None,
            offload_now: false,
            resound_due: None,
            placed_due: false,
            seek_landed: false,
            stretched: false,
            resounded_at: i64::MIN / 2,
            remake: None,
            panics: VecDeque::new(),
            stall: None,
            restarted: None,
            jumping: None,
            awake: true,
        };
        w.apply(settings);
        w
    }

    /// Engine time, ms: the clock plus 1 s.
    fn now(&self) -> i64 {
        self.clock.now_ms() + 1_000
    }

    /// The engine's thread: a turn per wake. A panicking turn does not end the thread (a dead engine
    /// once left the app claiming to play in silence): it is reported and the song restarted
    /// ([`Worker::recover`]).
    fn run(mut self) {
        loop {
            let turned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.turn()));
            let wake = match turned {
                Ok(Turn::Stop) => return,
                Ok(Turn::Sleep(w)) => w,
                Err(p) => {
                    let mut why = panic_words(&*p);
                    let now = self.now();
                    // Recovering may panic too: counted, and a second restart of the song skips it.
                    let mut recovered = false;
                    for _ in 0..=PANICS_KEPT_ON {
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.recover(now, &why))) {
                            Ok(go_on) => {
                                recovered = go_on;
                                break;
                            }
                            Err(p) => why = panic_words(&*p),
                        }
                    }
                    if recovered {
                        Some(1)
                    } else {
                        // Wait for a command; a client that thinks it plays is told it stopped.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            self.p.app.log("the engine's thread panicked again and again: it waits for a command");
                            if self.state == State::Playing {
                                (self.events)(Event::Stopped { plays: self.plays });
                                self.set_state(State::Paused);
                            }
                        }));
                        None
                    }
                }
            };
            match wake {
                Some(0) => continue,
                w => self.clock.sleep(w.map(|ms| ms as u64), || self.waiting_for_bytes()),
            }
        }
    }

    /// One wake: the commands, the music, the status and events. Returns how long to sleep.
    fn turn(&mut self) -> Turn {
        self.clock.woke();
        loop {
            match self.rx.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return Turn::Stop,
                Ok(c) => {
                    // Kept awake for the command's work; `follow_awake` may let go at the turn's end.
                    self.stay_awake();
                    self.command(c)
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        let now = self.now();
        self.due(now);
        self.follow_gain();
        self.follow_depth();
        if self.p.app.measured() {
            self.replan();
        }
        if self.offloading() {
            self.turn_offload(now);
        } else {
            // The burst's estimate of what the device holds drifts; the ring's fill is exact, so the
            // count restarts at the ring's low mark.
            if self.p.playing() && !self.p.source_ended() && self.p.sink.track.filled_us() <= WAKE_LOW_US {
                self.p.burst.restart();
            }
            // A song opened ahead for a remake reads alone while the output holds enough.
            self.p.read_held = self.remake.as_ref().is_some_and(|m| !m.leaving) && self.held_us() > REMAKE_HOLD_US;
            self.p.turn(now);
            self.follow_offload_now();
            self.follow_offload_ahead();
        }
        self.check();
        self.announce(now);
        self.report(now);
        self.follow_why();
        self.restart_if_stalled(now);
        self.watch(now);
        self.follow_awake();
        // A flush made this turn reaches the device now, with the music after it in the ring.
        self.p.sink.track.told();
        Turn::Sleep(self.wake_in(now))
    }

    /// [`Event::Awake`]: the CPU may sleep only while offloaded, fed, and nothing else is under way.
    fn follow_awake(&mut self) {
        let sleeps = self.state == State::Playing
            && self.pause_at.is_none()
            && self.dip.is_none()
            && self.remake.is_none()
            && self.entering.is_none()
            && self.probe.is_none()
            && self.handing_over.is_none()
            && self.held.is_none()
            && self.off.as_ref().is_some_and(|o| o.active() && o.lets_cpu_sleep());
        self.say_awake(!sleeps);
    }

    fn stay_awake(&mut self) {
        self.say_awake(true);
    }

    fn say_awake(&mut self, awake: bool) {
        if awake == self.awake {
            return;
        }
        self.awake = awake;
        self.status.lock().awake = awake;
        (self.events)(Event::Awake(awake));
    }

    /// Some song's bytes are awaited (for a test clock, which stands still meanwhile).
    fn waiting_for_bytes(&self) -> bool {
        self.p.waiting_for_bytes() || self.entering.is_some() || self.remake.as_ref().is_some_and(|m| !m.ready) || self.probe.as_ref().is_some_and(|p| p.2.is_none()) || self.off.as_ref().is_some_and(Offload::waiting_for_bytes)
    }

    // ---- the CPU and offload paths as one player ----

    /// The offload path holds the song (or the song starting there is still opening).
    fn offloading(&self) -> bool {
        self.off.as_ref().is_some_and(Offload::active)
    }

    fn playing(&self) -> bool {
        match &self.off {
            Some(o) if o.active() => o.playing(),
            _ => self.p.playing(),
        }
    }

    fn current(&self) -> Option<usize> {
        match &self.off {
            Some(o) if o.active() => o.current(),
            _ => self.p.current(),
        }
    }

    fn position_ms(&mut self) -> i64 {
        match self.off.as_mut() {
            Some(o) if o.active() => o.heard().map_or(0, |h| h.1),
            _ => self.p.position_ms(),
        }
    }

    /// Queue index `i` from `ms`: offloaded when allowed (the song decides once open), else on the CPU.
    /// Playing or paused as before.
    fn jump(&mut self, i: usize, ms: i64) {
        // A panic while opening restarts here.
        self.jumping = Some((i, ms));
        self.jump_now(i, ms);
        self.jumping = None;
    }

    fn jump_now(&mut self, i: usize, ms: i64) {
        self.probe = None;
        self.handing_over = None;
        self.remake = None;
        if self.offload && self.off.is_some() {
            if !self.offloading() {
                // Never both outputs open at once.
                self.p.pause();
                self.p.release();
                self.p.sink.track.release();
            }
            if let Some(off) = self.off.as_mut() {
                let playing = off.playing() || self.state == State::Playing && self.pause_at.is_none();
                let i = off.start(i, ms, &mut self.p.tracks, &self.p.queue);
                if playing {
                    off.play();
                }
                self.offload_heard = false;
                // A song placed again for a jump is not a repeat-one loop.
                self.heard_seq = 0;
                self.p.queue.moved_to(i);
                return;
            }
        }
        self.leave_offload();
        self.p.jump(i, ms);
    }

    /// Releases the offload path; returns where it was.
    fn leave_offload(&mut self) -> Option<(usize, i64)> {
        self.off.as_mut()?.release()
    }

    /// Resumes whichever path holds the song.
    fn go_on(&mut self) {
        match self.off.as_mut() {
            Some(o) if o.active() => o.play(),
            _ => self.p.resume(),
        }
    }

    /// Pauses whichever path holds the song.
    fn halt(&mut self) {
        match self.off.as_mut() {
            Some(o) if o.active() => o.pause(),
            _ => self.p.pause(),
        }
    }

    /// Fades the path that holds the song from `from` (or where it is) to `to` over `ms`.
    fn ramp(&mut self, from: Option<f32>, to: f32, ms: i64) {
        let now = self.now();
        match self.off.as_mut() {
            Some(o) if o.active() => o.ramp(from, to, ms, now),
            _ => self.p.sink.track.ramp(from, to, ms),
        }
    }

    /// Releases the CPU's device, keeping the place for the next play.
    fn park(&mut self) {
        self.remake = None;
        if self.released.is_none() {
            self.released = self.p.release();
            self.p.sink.track.release();
        }
    }

    /// Runs the offload path's turn; the CPU takes over where it hands the song back.
    fn turn_offload(&mut self, now: i64) {
        let Worker { p, off, .. } = self;
        let Some(off) = off.as_mut() else { return };
        let app = &mut p.app;
        let max = p.gain_max;
        let step = off.turn(now, &mut p.tracks, &p.queue, &mut |i, id| app.gain(i, id).min(max));
        let Step::ToPcm { index, ms, refused } = step else { return };
        if refused {
            self.tear_downs += 1;
            self.p.app.log(&format!("the offloaded track failed ({} times): the CPU plays on", self.tear_downs));
            if self.tear_downs >= TEAR_DOWNS && !self.offload_refused {
                self.offload_refused = true;
                let s = self.settings.clone();
                self.apply(s);
            }
        }
        let playing = self.state == State::Playing && self.pause_at.is_none();
        self.leave_offload();
        self.p.jump(index, ms);
        self.placed_due = true;
        if playing {
            self.p.resume();
        }
    }

    /// Offload wanted while the CPU plays: when the output decodes the next song, the CPU plays the
    /// current one to its end and hands over there (as media3 reconfigures its sink at a boundary).
    fn follow_offload_ahead(&mut self) {
        if !self.offload || self.off.is_none() || self.handing_over.is_some() || !self.p.playing() {
            return;
        }
        let Some(cur) = self.p.current() else { return };
        let Some(next) = self.p.queue.read(|q| q.next_of(cur, q.repeat())) else { return };
        if self.probe.as_ref().is_none_or(|(i, _, _)| *i != next) {
            let id = self.p.id_at(next);
            self.probe = Some((next, self.p.tracks.open_packets(&id, 0, true), None));
        }
        if self.probe.as_mut().is_some_and(|(_, o, k)| k.is_none() && o.as_mut().is_ok_and(|r| !r.ready())) {
            return;
        }
        // Asked once, when the song is open.
        let level = if self.probe.as_ref().is_some_and(|p| p.2.is_none()) { self.gain_of(next) } else { 1.0 };
        let (_, opened, known) = self.probe.as_mut().expect("set above");
        if known.is_none() {
            let off = self.off.as_mut().expect("checked");
            let why = match opened {
                Ok(r) => {
                    let album = off.in_album(next, &self.p.tracks, &self.p.queue);
                    off.refuses(r, album, level)
                }
                Err(_) => Some(OnCpu::Unread),
            };
            *known = Some(why.is_none());
            if why.is_some() {
                off.on_cpu = why;
            }
            // The offload path opens the song anew.
            *opened = Err(String::new());
        }
        // Only while the current song is still being read; past it, the handover waits a song.
        if *known == Some(true) && self.p.reading_index() == Some(cur) && self.p.stopping_after().is_none() {
            self.p.app.log("offload takes over at the next song");
            self.p.pause_at_end(true);
            self.handing_over = Some(cur);
        }
    }

    fn set_state(&mut self, s: State) {
        if self.state != s {
            self.state = s;
            self.idle_at = (s == State::Paused).then(|| self.now() + self.idle_release_ms);
            self.status.lock().state = s;
            (self.events)(Event::State(s));
        }
    }

    /// Paused long enough: lets the device and the song's bytes go, keeping the place.
    fn release(&mut self) {
        self.idle_at = None;
        if self.playing() || self.released.is_some() {
            return;
        }
        self.released = match self.leave_offload() {
            Some(at) => Some(at),
            None => self.p.release(),
        };
        self.p.sink.track.release();
        self.p.tracks.let_go();
        self.releases += 1;
    }

    /// After a release, opens the song again where it was.
    fn reopen(&mut self) {
        if let Some((i, ms)) = self.released.take() {
            self.jump(i, ms);
        }
    }

    /// A turn panicked: reported, and the song restarted from scratch ([`Worker::start_again`]). After
    /// [`PANICS_KEPT_ON`] panics within [`PANICS_WITHIN_MS`] playback stops instead. Returns whether the
    /// music goes on; false means wait for a command (a panic every turn would spin).
    fn recover(&mut self, now: i64, why: &str) -> bool {
        while self.panics.front().is_some_and(|t| now - t > PANICS_WITHIN_MS) {
            self.panics.pop_front();
        }
        self.panics.push_back(now);
        let on = self.jumping.map(|j| j.0).or_else(|| self.current()).or_else(|| self.p.queue.read(|q| q.current())).map(|i| self.p.id_at(i));
        let song = on.as_deref().map(|id| format!(" on {id}")).unwrap_or_default();
        if self.panics.len() >= PANICS_KEPT_ON {
            self.p.app.log(&format!("the engine's thread panicked{song} ({why}), {} times within a minute: playback stops", self.panics.len()));
            self.jumping = None;
            self.let_go_of_everything();
            self.p.tracks.let_go();
            (self.events)(Event::Error { id: on.unwrap_or_default(), message: format!("the player failed: {why}") });
            (self.events)(Event::Stopped { plays: self.plays });
            self.set_state(State::Paused);
            return false;
        }
        self.p.app.log(&format!("the engine's thread panicked{song} ({why}): the music is made again from scratch"));
        (self.events)(Event::Error { id: on.unwrap_or_default(), message: format!("the player panicked ({why}): the song is opened again from scratch") });
        self.start_again(&format!("a panic: {why}"));
        true
    }

    /// Drops everything held for the music (openings, pending switches, readers, outputs), keeping the
    /// place in the queue.
    fn let_go_of_everything(&mut self) {
        self.remake = None;
        self.probe = None;
        self.entering = None;
        self.offload_now = false;
        if self.handing_over.take().is_some() {
            self.p.pause_at_end(false);
        }
        self.dip = None;
        self.resound_due = None;
        self.pause_at = None;
        self.held = None;
        self.stall = None;
        self.unstall();
        let _ = self.leave_offload();
        self.p.release();
        self.p.sink.track.release();
        self.released = None;
    }

    /// Restarts the song from scratch at the playback position: everything held goes, with the song's bytes and
    /// cache entry ([`Library::forget`]), and it is fetched anew. The same song again with nothing heard
    /// since fails as a song that would not play.
    fn start_again(&mut self, why: &str) {
        let playing = self.state == State::Playing;
        let (at, ms) = match self.jumping.take() {
            Some((i, ms)) => (Some(i), ms),
            None => (self.current().or_else(|| self.p.queue.read(|q| q.current())), self.position_ms().max(0)),
        };
        self.let_go_of_everything();
        let Some(i) = at.filter(|&i| i < self.p.queue.read(|q| q.len())) else {
            self.p.tracks.let_go();
            return;
        };
        let id = self.p.id_at(i);
        self.p.tracks.forget(&id);
        if self.restarted.as_deref() == Some(id.as_str()) {
            self.restarted = None;
            self.p.app.log(&format!("{id} made no music again ({why}): it counts as a song that would not play"));
            self.p.give_up(i, format!("no music came of it ({why})"));
        } else {
            self.restarted = Some(id.clone());
            self.p.app.log(&format!("{id} is opened again from scratch at {ms} ms ({why})"));
            self.jump(i, ms);
        }
        if playing && self.p.stopped_at().is_none() && self.current().is_some() {
            self.go_on();
            self.ramp(Some(0.0), 1.0, RESOUND_DIP_MS);
            self.set_state(State::Playing);
        }
    }

    /// Whether the bytes the music waits for are still being fetched (the song being read, or the next
    /// one once that is read to its end).
    fn bytes_coming(&self) -> bool {
        if self.offloading() {
            return self.off.as_ref().is_some_and(Offload::waiting_for_bytes);
        }
        if !self.p.waiting_for_bytes() {
            return false;
        }
        let Some(r) = self.p.reading_index() else { return false };
        let next = self.p.queue.read(|q| q.next_of(r, q.repeat()));
        [Some(r), next].into_iter().flatten().any(|i| self.p.tracks.loading(&self.p.id_at(i)).is_some_and(|l| l.fetching()))
    }

    /// While music should move, restarts the song from scratch ([`Worker::start_again`]) once the
    /// position has stood still [`STALL_RESTART_MS`] with no bytes on their way: an output that stopped
    /// taking music without saying so.
    fn restart_if_stalled(&mut self, now: i64) {
        let wanted = self.state == State::Playing && self.pause_at.is_none() && self.dip.is_none() && self.held.is_none() && self.p.queue.read(|q| !q.is_empty());
        if !wanted {
            self.stall = None;
            return;
        }
        let place = (self.current(), self.position_ms());
        let mut q = match self.stall.take() {
            Some(q) if q.place == place => Stall { standing: true, ..q },
            Some(_) => {
                // Moving: a restarted song plays.
                self.restarted = None;
                Stall { since: now, place, standing: false }
            }
            None => Stall { since: now, place, standing: false },
        };
        // Bytes on their way fail through their request's own stall; an offloaded track has its own
        // watchdog that knows how long a platform's count may stand still.
        if self.bytes_coming() || self.off.as_ref().is_some_and(Offload::watching) {
            q.since = now;
            q.standing = false;
        }
        let long = now - q.since;
        self.stall = Some(q);
        if long >= STALL_RESTART_MS {
            let words = self.words((self.p.sink.track.filled_us() + self.p.sink.track.latency_us()) / 1000);
            self.p.app.log(&format!("playing, and the music stood still for {long} ms with nothing on its way: {words}"));
            let id = self.current().map(|i| self.p.id_at(i)).unwrap_or_default();
            (self.events)(Event::Error { id, message: format!("no music for {} s while playing: the song is opened again from scratch", long / 1000) });
            self.start_again(&format!("the music stood still for {long} ms"));
        }
    }

    /// How long the position has stood still while music should move, ms.
    fn stalled_ms(&self, now: i64) -> i64 {
        self.stall.as_ref().filter(|q| q.standing).map_or(0, |q| now - q.since)
    }

    /// The output device changed: the app may give it its own sound.
    fn device(&mut self, d: Device) {
        if let Some(off) = self.off.as_mut() {
            off.output_moved();
        }
        let Some((name, sound)) = self.p.app.output_changed(d.kind, &d.name) else { return };
        if let Some(sound) = sound {
            let s = Settings { sound, ..self.settings.clone() };
            self.apply(s);
        }
        (self.events)(Event::Output { name });
    }

    /// Jumps made or held, not counting those waiting in the dip ([`Event::Song`]'s `jumps`).
    fn made(&self) -> u64 {
        self.jumps - self.dip.as_ref().map_or(0, Dip::jumps) as u64
    }

    fn command(&mut self, c: Command) {
        let now = self.now();
        if matches!(c, Command::PlayAt(..) | Command::GoTo(..) | Command::Next | Command::Previous) {
            self.jumps += 1;
        }
        match c {
            Command::PlayAt(i, ms) => {
                self.held = None;
                self.switch(Switched::To(i, ms), Switch::ToSong, now)
            }
            Command::GoTo(i, ms) => self.go(Switched::To(i, ms), Switch::ToSong, now),
            Command::PauseAtEnd(on) => {
                // The sleep timer's stop replaces a handover waiting at the same end.
                if self.handing_over.take().is_some() && !on {
                    self.p.pause_at_end(false);
                }
                let Worker { p, off, .. } = self;
                match off.as_mut() {
                    Some(o) if o.active() => {
                        if o.pause_at_end(on, &mut p.tracks, &p.queue) {
                            // The next song is written already: restart here without it.
                            if let Some((i, ms, _)) = o.heard() {
                                self.jump(i, ms);
                                if let Some(o) = self.off.as_mut() {
                                    o.stop_after = Some(i);
                                }
                            }
                        }
                    }
                    _ => self.p.pause_at_end(on),
                }
            }
            Command::Play => {
                self.plays += 1;
                self.play()
            }
            Command::Pause(fade) => self.pause(now, fade.unwrap_or(self.settings.fade_ms)),
            Command::Toggle => {
                if self.state == State::Playing {
                    self.pause(now, self.settings.fade_ms)
                } else {
                    self.play()
                }
            }
            // Paused, a skip also plays (nori_player::transport::skip_plays).
            Command::Next => self.skip(Switched::Next, now),
            Command::Previous => self.skip(Switched::Previous, now),
            Command::Seek(ms) => self.go(Switched::Seek(ms), Switch::Seek, now),
            Command::Settings(s) => self.apply(*s),
            Command::Output(facts) => {
                self.facts = facts;
                let s = self.settings.clone();
                self.apply(s);
            }
            Command::Replan => self.replan(),
            Command::QueueChanged => {
                self.p.queue_changed();
                self.follow_held();
                self.follow_queue();
                // Another song may follow now: its transition is planned again.
                self.replan();
            }
            Command::Repeat(m) => {
                self.p.set_repeat(m);
                self.follow_queue();
            }
            Command::Gain => self.gain_changed = true,
            Command::Tuning(on) => self.tune(on),
            Command::Positions(every) => {
                self.positions = every.map(|d| d.as_millis().max(1) as i64);
                self.next_position = now;
            }
            Command::Device(d) => self.device(d),
            Command::Look | Command::Stop => {}
        }
    }

    /// The equalizer screen opened or closed: the output goes shallow (or deep) now, behind a dip,
    /// rather than at the next song. An output that [`AudioOutput::resizes`] changes depth in place and
    /// nothing is remade.
    fn tune(&mut self, on: bool) {
        self.p.set_tuning(on);
        if self.p.sink.track.resizes() {
            self.follow_depth();
            // The change that turned tuning on was just remade into the deep buffer: again, shallow.
            if self.p.chain.tuning && self.now() - self.resounded_at <= TUNED_AFTER_RESOUND_MS && self.held_us() > self.tuned_held_us() {
                self.resound_soon();
            }
            return;
        }
        let wanted = if self.p.chain.tuning { self.p.shallow_us } else { nori_player::burst::BUFFER_US };
        if self.p.sink.capacity_us != wanted {
            self.resound_soon();
        }
    }

    /// The queue changed: the offload path finds its songs again, and restarts at the playback position when a
    /// song it already wrote no longer follows.
    fn follow_queue(&mut self) {
        let ids = self.p.queue.read(|q| q.ids().to_vec());
        let old = std::mem::replace(&mut self.ids, ids);
        self.probe = None;
        let Worker { p, off, .. } = self;
        let Some(off) = off.as_mut().filter(|o| o.active()) else { return };
        if off.queue_changed(&old, &mut p.tracks, &p.queue) {
            if let Some((i, ms, _)) = off.heard() {
                self.jump(i, ms);
            }
        }
    }

    /// Applies the settings through `nori_player::policy::audio_policy`: bit-perfect leaves samples
    /// untouched (no chain, silence skipping, transitions or ReplayGain), high quality output runs it all
    /// in float, and anything touching samples keeps songs off offload.
    fn apply(&mut self, s: Settings) {
        let hi_res = s.hi_res && self.p.sink.track.takes_float();
        let bit_perfect = self.facts.bit_perfect;
        let prefs = AudioPrefs {
            dsp: s.sound.on(),
            skip_silence: s.skip_silence,
            offload: s.offload && self.off.is_some(),
            crossfade_s: s.crossfade_s,
            auto_mix: s.auto_mix,
            speed: s.speed,
            pitch: s.pitch,
        };
        let state = OutputState { hi_res, bit_perfect, usb: self.facts.usb, offload_refused: self.offload_refused };
        let policy = audio_policy(&prefs, &state);
        self.blocked = if self.off.is_some() { offload_blocked(&prefs, &state) } else { Some("the output does not decode songs itself") };
        // Turning up needs float samples and the limiter; the limiter alone does not block offload
        // (only a song actually turned up does).
        let boost_db = if s.gain_boost_db > 0.0 { s.gain_boost_db.min(nori_player::gain::BOOST_MAX_DB) } else { 0.0 };
        let gain_max = if policy.untouched || boost_db == 0.0 { 1.0 } else { 10f32.powf(boost_db / 20.0) };
        let mut sound = if policy.untouched { Sound::default() } else { s.sound.clone() };
        sound.limiter |= gain_max > 1.0;
        let now = Applied {
            sound,
            speed: (s.speed, s.pitch),
            skip_silence: policy.skip_silence,
            untouched: policy.untouched,
            bit_perfect,
            float: hi_res,
            gain_max,
            max_rate: s.max_rate,
        };
        self.p.sink.track.set_float(hi_res);
        // Takes effect when the output is remade below.
        self.p.sink.track.max_rate = s.max_rate;
        let first = self.applied.is_none();
        let was = self.applied.take().unwrap_or(Applied { sound: Sound::default(), speed: (1.0, 1.0), skip_silence: false, untouched: false, bit_perfect: false, float: false, gain_max: 1.0, max_rate: 0 });
        // Float for high quality, bit-perfect, and turned-up songs (over full scale until the limiter).
        let encoding = if policy.float || gain_max > 1.0 { Encoding::Float } else { Encoding::Pcm16 };
        if first || was.gain_max != now.gain_max {
            self.p.tracks.encoding = encoding;
            self.p.gain_max = gain_max;
            self.gain_changed |= !first;
        }
        if first || was.untouched != now.untouched || was.bit_perfect != now.bit_perfect || was.float != now.float {
            self.p.tracks.encoding = encoding;
            self.p.sink.track.exact = policy.untouched;
            self.p.gain_off = bit_perfect;
            // The chain stays in (skipped while flat) so switching it on is heard at once.
            self.p.keep_chain(!policy.untouched);
            self.p.engine.lock_rate = policy.lock_rate;
            self.p.app.transitions_off(policy.transitions_off);
            if was.bit_perfect != now.bit_perfect {
                self.gain_changed = true;
            }
        }
        if was.sound != now.sound {
            self.p.set_sound(now.sound.clone());
        }
        if was.speed != now.speed {
            self.p.set_speed(s.speed, s.pitch);
        }
        if was.skip_silence != now.skip_silence {
            self.p.set_skip_silence(now.skip_silence);
        }
        // What the output holds was made under the old settings.
        let heard_differently = !first && was != now;
        let sound_only = was.sound != now.sound && Applied { sound: now.sound.clone(), ..was.clone() } == now;
        // The plan out of the current song was made under the old transition settings.
        let replan = first || was.untouched != now.untouched || (self.settings.crossfade_s, self.settings.auto_mix) != (s.crossfade_s, s.auto_mix);
        self.applied = Some(now);
        self.settings = s;
        let restarted = self.follow_offload(policy.offload, heard_differently);
        if replan {
            self.replan();
        }
        if heard_differently && !restarted {
            // Tuned, the output is shallow and a band moved is heard as it is, unless it still holds
            // seconds from before it was made shallow in place.
            let tuned = self.p.chain.tuning && self.p.sink.capacity_us == self.p.shallow_us && self.held_us() <= self.tuned_held_us();
            match self.entering.as_mut() {
                // Offloaded at the playback position, or remade there if it stays on the CPU.
                Some(e) => e.2 = true,
                None if sound_only && tuned => {}
                None => self.resound_soon(),
            }
        }
    }

    /// Asks for the transition plan out of the current song again. The output runs seconds ahead, so
    /// when the song's ending is already made under the old plan it is remade from the playback position. A
    /// mix already audible plays out as it began.
    fn replan(&mut self) {
        self.p.engine.replan();
        if self.offloading() || self.p.mixing() {
            return;
        }
        let Some((cur, ear_ms)) = self.p.ear() else { return };
        let id = self.p.id_at(cur);
        if self.p.read_astray(cur) {
            // Read on gaplessly into a song that no longer follows.
            self.p.app.log(&format!("the ending of {id} is made again: another song follows it now"));
            self.resound_soon();
            return;
        }
        let now = self.now();
        self.p.app.clock(now);
        let plan = self.p.app.plan_for(&id);
        let Some(made) = self.p.ending_made(cur, plan.as_ref().map(|p| p.out_start_us)) else { return };
        if made == plan {
            return;
        }
        // Gapless so far, and playback past where the new mix would have ended: nothing to remake.
        let ear_us = ear_ms * 1000;
        if made.is_none() && plan.as_ref().is_some_and(|p| ear_us >= p.out_start_us + p.duration_us) {
            return;
        }
        self.p.app.log(&format!(
            "the ending of {id} is made again: {} now, {} as it was made",
            plan.as_ref().map_or("gapless".to_string(), |p| format!("a mix from {} ms", p.out_start_us / 1000)),
            made.as_ref().map_or("gapless".to_string(), |p| format!("a mix from {} ms", p.out_start_us / 1000)),
        ));
        self.resound_soon();
    }

    /// Offload became allowed or not. Leaving is immediate, at the playback position. Entering happens where the
    /// ear is behind a dip once the current song is known to be decodable (else at the next song that
    /// is). `resound`: the sound changed too, so a song staying on the CPU is remade. Returns whether the
    /// CPU took the song over (already made with the new settings).
    fn follow_offload(&mut self, wanted: bool, resound: bool) -> bool {
        let was = std::mem::replace(&mut self.offload, wanted);
        self.status.lock().offload_wanted = wanted;
        if was == wanted {
            return false;
        }
        self.probe = None;
        self.entering = None;
        self.remake = None;
        if !wanted {
            if self.handing_over.take().is_some() {
                self.p.pause_at_end(false);
            }
            if self.offloading() {
                return self.leave_chip();
            }
        } else if !self.p.playing() && self.p.current().is_some() && self.held.is_none() {
            self.park();
        } else if let Some(i) = self.p.current().filter(|_| self.p.playing() && !self.offloading()) {
            // Opened as packets to see whether the output decodes it.
            let id = self.p.id_at(i);
            self.entering = Some((i, self.p.tracks.open_packets(&id, 0, true), resound));
        }
        false
    }

    /// Hands the offloaded song to the CPU. While the offload track can play on (playing, no USB, track not
    /// refused), the CPU opens the song ahead first so the handover is a dip, not a gap.
    fn leave_chip(&mut self) -> bool {
        let playing = self.state == State::Playing && self.pause_at.is_none();
        if playing && !self.facts.usb && !self.offload_refused && self.leave_ahead() {
            return true;
        }
        self.leave_now()
    }

    /// Opens the offloaded song on the CPU [`REMAKE_LEAD_MS`] ahead of the offload track; the CPU takes over there
    /// once it is open ([`Worker::remake`]).
    fn leave_ahead(&mut self) -> bool {
        let Some((i, ms, _)) = self.off.as_mut().and_then(Offload::heard) else { return false };
        let id = self.p.id_at(i);
        let length = self.p.tracks.about(&id).duration_ms;
        let from_ms = ms + REMAKE_LEAD_MS;
        if length <= 0 || from_ms + REMAKE_LEAD_MS >= length {
            return false;
        }
        let now = self.now();
        if !self.open_remake(id, from_ms, now + REMAKE_LEAD_MS - RESOUND_DIP_MS, true) {
            return false;
        }
        self.p.app.log("offload given up: the CPU takes over once the song is open");
        true
    }

    /// Offload wanted while the CPU plays: once the current song is known to be decodable there, the
    /// output takes it over at the playback position, behind a dip.
    fn follow_offload_now(&mut self) {
        let Some(i) = self.entering.as_ref().map(|e| e.0) else { return };
        if !self.offload || self.p.current() != Some(i) || !self.p.playing() || self.offloading() {
            self.entering = None;
            return;
        }
        if self.entering.as_mut().is_some_and(|e| e.1.as_mut().is_ok_and(|r| !r.ready())) {
            return;
        }
        let (i, opened, resound) = self.entering.take().expect("checked");
        let level = self.gain_of(i);
        let Worker { p, off, .. } = self;
        let Some(off) = off.as_mut() else { return };
        let why = match &opened {
            Ok(r) => {
                let joins = off.in_album(i, &p.tracks, &p.queue);
                off.refuses(r, joins, level)
            }
            Err(_) => Some(OnCpu::Unread),
        };
        let taken = why.is_none();
        if let Some(why) = why {
            off.on_cpu = Some(why);
        }
        if taken {
            self.p.app.log("offload takes over where the ear is");
            self.offload_now = true;
            self.resound_due = Some(self.now());
        } else if resound {
            self.resound_soon();
        }
    }

    /// Most the ring and device may hold while tuned for a band moved to be heard as it is: the shallow
    /// ring, the shallow device as it reports its needs (a Bluetooth latency) and slack.
    fn tuned_held_us(&self) -> i64 {
        let device = self.p.sink.track.shallow_depth().map_or(0, |d| d.device_us);
        TUNED_HELD_US.max(device + self.p.shallow_us + TUNED_SLACK_US)
    }

    /// While tuned over a device that resizes in place, keeps the ring as deep as the device reports it
    /// needs ([`AudioOutput::shallow_depth`]), never under [`SHALLOW_US`].
    fn follow_depth(&mut self) {
        if !self.p.chain.tuning || !self.p.sink.track.resizes() {
            return;
        }
        let ring = self.p.sink.track.shallow_depth().map_or(SHALLOW_US, |d| d.ring_us.max(SHALLOW_US));
        if ring != self.p.shallow_us {
            self.p.app.log(&format!("the shallow ring follows the device: {} ms", ring / 1000));
            self.p.set_shallow_us(ring);
        }
    }

    /// Music made and not yet heard (ring and device), µs.
    fn held_us(&self) -> i64 {
        let track = &self.p.sink.track;
        track.filled_us() + track.latency_us()
    }

    /// The sound changed while the CPU plays: what the output holds is remade from the playback position,
    /// behind a short dip, at most every [`RESOUND_EVERY_MS`]. Paused, it is remade on resume.
    fn resound_soon(&mut self) {
        if self.offloading() || self.p.current().is_none() {
            return;
        }
        if !self.p.playing() {
            self.p.resound();
            return;
        }
        if self.dip.as_ref().is_some_and(Dip::resounds) {
            // The remake at the dip's bottom includes this change.
            return;
        }
        let at = self.now().max(self.resounded_at + RESOUND_EVERY_MS);
        self.resound_due = Some(self.resound_due.map_or(at, |t| t.min(at)));
    }

    /// Starts a remake now: the song opened ahead, or the dip going down (`Switched::Resound`).
    fn resound(&mut self, now: i64) {
        if let Some(t) = self.pause_at {
            // Paused at the fade's end, then remade on resume.
            self.resound_due = Some(t);
            return;
        }
        self.resound_due = None;
        if self.offloading() || self.p.current().is_none() {
            return;
        }
        if !self.p.playing() {
            self.p.resound();
            return;
        }
        if self.p.mixing() {
            // Not cutting an audible mix off: after it.
            self.resound_due = Some(now + 250);
            return;
        }
        if self.dip.as_ref().is_some_and(Dip::resounds) || self.remake.is_some() {
            // A remake under way includes this change.
            return;
        }
        if self.dip.is_none() && !self.offload_now && self.open_ahead(now) {
            return;
        }
        self.dip_for_resound(now);
    }

    /// Fades down now and remakes the music at the bottom ([`Worker::resounded`]).
    fn dip_for_resound(&mut self, now: i64) {
        self.dip_down(now, RESOUND_DIP_MS, RESOUND_DIP_MS).then.push(Switched::Resound);
    }

    /// The dip under way, or a new one fading down now over `down_ms`.
    fn dip_down(&mut self, now: i64, down_ms: i64, up_ms: i64) -> &mut Dip {
        if self.dip.is_none() {
            self.ramp(None, 0.0, down_ms);
            self.dip = Some(Dip { at: now + down_ms, up_ms, then: Vec::new() });
        }
        self.dip.as_mut().expect("set above")
    }

    /// Opens the current song [`REMAKE_LEAD_MS`] ahead of playback to remake from there
    /// ([`Worker::remake`]). False for a song of unknown length (a live stream) or at its very end.
    fn open_ahead(&mut self, now: i64) -> bool {
        let Some((i, ms)) = self.p.ear_now() else { return false };
        let speed = self.p.speed().0.clamp(0.1, 8.0) as f64;
        let from_ms = ms + (REMAKE_LEAD_MS as f64 * speed) as i64;
        let id = self.p.id_at(i);
        let length = self.p.tracks.about(&id).duration_ms;
        if length <= 0 || from_ms + REMAKE_LEAD_MS >= length {
            return false;
        }
        self.open_remake(id, from_ms, now + REMAKE_LEAD_MS - RESOUND_DIP_MS, false)
    }

    fn open_remake(&mut self, id: String, from_ms: i64, dip_at: i64, leaving: bool) -> bool {
        match self.p.tracks.open(&id, from_ms) {
            Ok(r) => {
                self.remake = Some(Remake { id, from_ms, r, ready: false, dip_at, leaving });
                true
            }
            Err(_) => false,
        }
    }

    /// Follows the song opened ahead: once open and playback is a dip away from its start, the dip goes
    /// down and the output is refilled from it ([`Worker::resounded`]). Dropped when playback moved to
    /// another song or it would not open; paused, the music is remade on resume.
    fn remake(&mut self, now: i64) {
        let Some(m) = self.remake.as_ref() else { return };
        if self.dip.as_ref().is_some_and(Dip::resounds) {
            return;
        }
        if m.leaving {
            if !self.offloading() {
                self.remake = None;
                return;
            }
            if self.pause_at.is_some() {
                return;
            }
            if !self.playing() {
                // Paused offloaded: nothing to hear of the handover.
                self.remake = None;
                self.leave_now();
                return;
            }
        } else if self.offloading() || self.pause_at.is_some() {
            if self.pause_at.is_none() {
                self.remake = None;
            }
            return;
        } else if !self.p.playing() {
            self.remake = None;
            self.p.resound();
            return;
        }
        let m = self.remake.as_mut().expect("checked");
        if !m.ready {
            m.ready = m.r.ready();
            if !m.ready {
                // Its loader wakes the thread.
                return;
            }
            if m.r.error().is_some() {
                // Remade at the playback position instead.
                let leaving = m.leaving;
                self.remake = None;
                if leaving {
                    self.leave_now();
                } else {
                    self.dip_for_resound(now);
                }
                return;
            }
        }
        if self.dip.is_some() || now < m.dip_at {
            return;
        }
        if !m.leaving {
            let (from_ms, id) = (m.from_ms, m.id.clone());
            match self.p.ear_now() {
                Some((i, _)) if self.p.id_at(i) != id => {
                    // Playback moved into the next song: open again there.
                    self.remake = None;
                    self.resound_soon();
                    return;
                }
                Some((_, ms)) => {
                    let speed = self.p.speed().0.clamp(0.1, 8.0) as f64;
                    let short = ((from_ms - ms) as f64 / speed) as i64 - RESOUND_DIP_MS;
                    if short > 1 {
                        // Playback is behind the estimate (the output's clock settling): wait a moment.
                        if let Some(m) = self.remake.as_mut() {
                            m.dip_at = now + short;
                        }
                        return;
                    }
                }
                None => {
                    self.remake = None;
                    return;
                }
            }
            if self.p.mixing() {
                // Not cutting an audible mix off: after it.
                self.remake = None;
                self.resound_due = Some(now + 250);
                return;
            }
        }
        self.dip_for_resound(now);
    }

    /// Plays from where the player is, fading in if the settings say so.
    fn resume(&mut self) {
        self.go_on();
        match play_fade(self.settings.fade_ms, false) {
            Some(ms) => self.ramp(Some(0.0), 1.0, ms as i64),
            None => self.ramp(None, 1.0, 0),
        }
        self.set_state(State::Playing);
    }

    fn play(&mut self) {
        if self.pause_at.take().is_some() && self.held.is_none() {
            // Play during the pause's fade: back up from where the fade got to.
            self.ramp(None, 1.0, self.settings.fade_ms.max(0) as i64);
            self.set_state(State::Playing);
            return;
        }
        // A place held during the fade (a skip, a new queue) is played below.
        if self.playing() && self.state == State::Playing {
            return;
        }
        if let Some((i, ms, _)) = self.held.take() {
            self.released = None;
            self.jump(i, ms);
            self.resume();
            return;
        }
        self.reopen();
        if self.offloading() {
            if self.state == State::Ended {
                let at = self.p.queue.read(|q| q.current()).unwrap_or(0);
                self.jump(at, 0);
            }
        } else if let Some(i) = self.p.stopped_at() {
            // Stopped at a failing song, nothing is read: try it again.
            self.jump(i, 0);
        } else if self.p.current().is_none() || self.state == State::Ended {
            let at = self.p.queue.read(|q| q.current()).or(self.p.current()).unwrap_or(0);
            if self.p.queue.read(|q| q.is_empty()) {
                return;
            }
            self.jump(at, 0);
        }
        self.resume();
    }

    fn pause(&mut self, now: i64, fade_ms: i32) {
        if self.pause_at.is_some() && fade_ms <= 0 {
            // Pause at once during a fade.
            self.pause_at = None;
            self.halt();
            return;
        }
        if !self.playing() || self.pause_at.is_some() {
            return;
        }
        // A pause never swallows the switch it interrupts.
        self.end_dip();
        match pause_fade(fade_ms, true) {
            Some(ms) => {
                self.ramp(None, 0.0, ms as i64);
                self.pause_at = Some(now + ms as i64);
            }
            None => self.halt(),
        }
        self.set_state(State::Paused);
    }

    /// A skip button: from the held place if any; paused, it also plays.
    fn skip(&mut self, s: Switched, now: i64) {
        if !(self.playing() && self.pause_at.is_none()) {
            self.hold(s);
            if skip_plays(false) {
                self.play();
            }
            return;
        }
        self.switch(s, Switch::Skip, now);
    }

    /// A jump or seek that keeps playing or paused: made behind its dip while playing, held while paused.
    fn go(&mut self, s: Switched, kind: Switch, now: i64) {
        if self.playing() && self.pause_at.is_none() {
            self.switch(s, kind, now);
        } else {
            self.hold(s);
        }
    }

    /// Holds the place `s` leads to, from the held place or the player's.
    fn hold(&mut self, s: Switched) {
        let len = self.p.queue.read(|q| q.len());
        if len == 0 {
            return;
        }
        let (at, ms) = match &self.held {
            Some((i, ms, _)) => (*i, *ms),
            None => {
                let ms = self.position_ms();
                (self.current().or(self.p.queue.read(|q| q.current())).unwrap_or(0), ms)
            }
        };
        let (i, ms) = match s {
            Switched::To(i, ms) => (i.min(len - 1), ms.max(0)),
            Switched::Seek(ms) => (at, ms.max(0)),
            Switched::Next => match self.p.queue.read(|q| q.next_of(at, q.repeat())) {
                Some(n) => (n, 0),
                None => return,
            },
            Switched::Previous => {
                let before = self.p.queue.read(|q| q.previous_of(at, q.repeat()));
                if previous_restarts(ms, before.is_some(), false) {
                    (at, 0)
                } else {
                    (before.unwrap_or(at), 0)
                }
            }
            // Paused, the music is remade on resume.
            Switched::Resound => return,
        };
        self.held = Some((i, ms, self.p.id_at(i)));
        self.seek_landed |= matches!(s, Switched::Seek(_));
        // The queue moves now, so a queue saved while paused restores this song.
        if !matches!(s, Switched::Seek(_)) {
            self.p.queue.moved_to(i);
        }
    }

    /// The queue was edited: a held place follows its song.
    fn follow_held(&mut self) {
        let Some((i, _, id)) = self.held.as_mut() else { return };
        let found = self.p.queue.read(|q| q.ids().iter().enumerate().filter(|(_, s)| **s == *id).map(|(k, _)| k).min_by_key(|k| k.abs_diff(*i)));
        match found {
            Some(k) => *i = k,
            None => self.held = None,
        }
    }

    fn switch(&mut self, s: Switched, kind: Switch, now: i64) {
        let playing = self.playing() && self.pause_at.is_none();
        match switch_dip(self.settings.fade_ms, kind, playing) {
            Some(dip) => {
                let d = self.dip_down(now, dip.down_ms as i64, dip.up_ms as i64);
                d.up_ms = dip.up_ms as i64;
                d.then.push(s);
            }
            None => {
                self.reopen();
                self.run_switch(s);
                if self.playing() {
                    self.set_state(State::Playing);
                }
            }
        }
    }

    /// The dip is down, or a pause cuts it short: its switches are made and the music comes back up.
    fn end_dip(&mut self) {
        let Some(dip) = self.dip.take() else { return };
        if !dip.then.is_empty() {
            self.reopen();
        }
        let mut from = None;
        for s in dip.then {
            from = self.run_switch(s).or(from);
        }
        self.ramp(from, 1.0, dip.up_ms);
        if self.playing() {
            self.set_state(State::Playing);
        }
    }

    /// Makes one switch. Returns the level the music comes back up from when it is not where the fade
    /// left it (a new output, silent).
    fn run_switch(&mut self, s: Switched) -> Option<f32> {
        // Only a play_at comes here paused (a skip or a go_to is held instead): music is wanted.
        let wants_music = !matches!(s, Switched::Seek(_) | Switched::Resound);
        let mut from = None;
        match s {
            Switched::To(i, ms) => {
                if i < self.p.queue.read(|q| q.len()) {
                    self.jump(i, ms);
                }
            }
            Switched::Next => {
                if let Some(n) = self.p.queue.read(Playlist::next) {
                    self.jump(n, 0);
                }
            }
            Switched::Previous => {
                let has_previous = self.p.queue.read(|q| q.previous().is_some());
                let at = self.position_ms();
                if previous_restarts(at, has_previous, false) {
                    self.seek(0);
                } else if let Some(n) = self.p.queue.read(Playlist::previous) {
                    self.jump(n, 0);
                }
            }
            Switched::Seek(ms) => {
                self.seek(ms);
                self.seek_landed = true;
            }
            Switched::Resound => from = self.resounded(),
        }
        if wants_music && !self.playing() && self.current().is_some() {
            self.resume();
        }
        from
    }

    /// At the dip's bottom: the output's decoder takes the song over at the playback position, or the CPU remakes
    /// the music from there. Returns the level to come back up from, if not the fade's.
    fn resounded(&mut self) -> Option<f32> {
        self.resounded_at = self.now();
        // This remake includes every change so far.
        self.resound_due = None;
        let mut remake = self.remake.take();
        if remake.as_ref().is_some_and(|m| m.leaving) {
            return self.take_over(remake.take().expect("checked"));
        }
        if self.offloading() {
            return None;
        }
        let i = self.p.current()?;
        if std::mem::take(&mut self.offload_now) && self.offload && self.off.is_some() && self.p.playing() {
            // The playback position, read as the CPU stops.
            self.p.pause();
            let ms = self.p.position_ms();
            self.jump(i, ms);
            self.placed_due = true;
            return Some(0.0);
        }
        match remake {
            Some(m) if m.ready => self.p.resound_from(m.id, m.r, m.from_ms),
            _ => self.p.resound(),
        }
        None
    }

    /// The CPU takes the song over from the offload track where it got to, with the song opened ahead
    /// ([`Worker::leave_ahead`]); the music comes up from silence.
    fn take_over(&mut self, m: Remake) -> Option<f32> {
        let playing = self.state == State::Playing && self.pause_at.is_none();
        let now = self.now();
        // The offload track's fade ends at silence, whatever tick it last took.
        self.ramp(None, 0.0, 0);
        let (i, ms) = self.off.as_mut().and_then(|o| o.leave(now))?;
        self.p.app.log("offload given up: the CPU plays on from here");
        if m.ready {
            self.p.jump_from(i, ms, (m.id, m.r, m.from_ms));
        } else {
            self.p.jump(i, ms);
        }
        self.placed_due = true;
        if playing {
            self.p.resume();
            return Some(0.0);
        }
        None
    }

    /// Leaves the output's decoder at once, at the playback position.
    fn leave_now(&mut self) -> bool {
        let playing = self.state == State::Playing && self.pause_at.is_none();
        let now = self.now();
        let Some((i, ms)) = self.off.as_mut().and_then(|o| o.leave(now)) else { return false };
        self.p.app.log("offload given up: the CPU plays on from here");
        self.p.jump(i, ms);
        self.placed_due = true;
        if playing {
            self.p.resume();
        }
        true
    }

    /// A seek in the current song; offloaded, the track restarts at the packet it lands in.
    fn seek(&mut self, ms: i64) {
        self.remake = None;
        match self.off.as_ref().filter(|o| o.active()).and_then(Offload::current) {
            Some(i) => self.jump(i, ms),
            None => self.p.seek(ms),
        }
    }

    fn due(&mut self, now: i64) {
        if self.idle_at.is_some_and(|t| now >= t) {
            self.release();
        }
        if self.pause_at.is_some_and(|t| now >= t) {
            self.pause_at = None;
            self.halt();
        }
        if self.dip.as_ref().is_some_and(|d| now >= d.at) {
            self.end_dip();
        }
        if self.resound_due.is_some_and(|t| now >= t) {
            self.resound(now);
        }
        self.remake(now);
    }

    /// Applies a ReplayGain settings change: to the ring and what is read next on the CPU (the player
    /// applies each song's gain before any mix), or to the offloaded track's volume.
    fn follow_gain(&mut self) {
        if !std::mem::take(&mut self.gain_changed) {
            return;
        }
        match self.off.as_ref() {
            Some(o) if o.active() => {
                let level = self.current().map_or(1.0, |i| self.gain_of(i));
                if !nori_player::gain::offload_allows(level) {
                    // Turned up: needs its samples, so the CPU takes over.
                    if let Some(o) = self.off.as_mut() {
                        o.on_cpu = Some(OnCpu::TurnedUp);
                    }
                    self.leave_chip();
                    return;
                }
                if let Some(o) = self.off.as_mut() {
                    o.set_level(level);
                }
            }
            _ => {
                self.p.gain_changed();
                // The ring was rescaled in place; what the device holds, or music already limited,
                // cannot be: remade.
                if self.p.sink.track.latency_us() > HELD_US || self.p.gain_max > 1.0 {
                    self.resound_soon();
                }
            }
        }
    }

    /// Song `i`'s ReplayGain, capped at what may be turned up now.
    fn gain_of(&mut self, i: usize) -> f32 {
        let id = self.p.id_at(i);
        self.p.app.gain(i, &id).min(self.p.gain_max)
    }

    fn check(&mut self) {
        if self.offloading() {
            return self.check_offload();
        }
        if let Some(message) = self.p.sink.track.take_failure() {
            (self.events)(Event::Error { id: String::new(), message });
            self.p.pause();
            self.set_state(State::Idle);
            // Released as after a long pause: the next play opens a new device.
            if self.released.is_none() {
                self.released = self.p.release();
                self.p.sink.track.release();
            }
        }
        for (id, message) in std::mem::take(&mut self.p.failures) {
            (self.events)(Event::Error { id, message });
        }
        self.p.changes.clear();
        if self.p.source_ended() {
            self.p.sink.track.set_ended(true);
        }
        if self.p.playing() && self.p.ended() && self.handing_over.is_some() && self.p.stopping_after() == self.handing_over {
            // Handover: the output's decoder takes the next song from its start.
            let i = self.handing_over.take().expect("checked");
            self.p.pause_at_end(false);
            let next = self.p.queue.read(|q| q.next_of(i, q.repeat()));
            match next {
                Some(n) => self.jump(n, 0),
                None => {
                    self.p.pause();
                    self.set_state(State::Ended);
                }
            }
        } else if let Some(i) = self.p.stopping_after().filter(|_| self.p.playing() && self.p.ended()) {
            self.p.pause_at_end(false);
            self.p.pause();
            self.stopped_after(i);
        } else if self.p.playing() && self.p.ended() {
            self.p.pause();
            self.set_state(State::Ended);
        } else if !self.p.playing() && self.state == State::Playing && self.pause_at.is_none() && self.dip.is_none() {
            if std::mem::take(&mut self.p.bridge) {
                (self.events)(Event::Bridge { plays: self.plays });
                self.set_state(State::Paused);
                return;
            }
            // The queue's rules stopped playback (a run of songs that would not play).
            (self.events)(Event::Stopped { plays: self.plays });
            self.set_state(State::Paused);
        }
    }

    /// Played to the end of song `i`, where the sleep timer stops: paused on the next song's start
    /// (fetched on play), or ended.
    fn stopped_after(&mut self, i: usize) {
        let next = self.p.queue.read(|q| q.next_of(i, q.repeat()));
        if let Some(n) = next {
            self.held = Some((n, 0, self.p.id_at(n)));
        }
        (self.events)(Event::Stopped { plays: self.plays });
        self.set_state(if next.is_some() { State::Paused } else { State::Ended });
    }

    /// Everything written to the offload track was heard: the end of the queue (or of the sleep
    /// timer's song), or a next song that needs another track or the CPU.
    fn check_offload(&mut self) {
        let Some(off) = self.off.as_mut() else { return };
        let Some(tail) = off.done() else { return };
        let stop_after = off.stop_after;
        match tail {
            Tail::Then(n) => {
                self.jump(n, 0);
                if self.state == State::Playing && !self.playing() {
                    self.go_on();
                }
            }
            Tail::End => {
                let at = off.current();
                off.pause();
                off.stop_after = None;
                match stop_after.filter(|&s| Some(s) == at) {
                    Some(i) => self.stopped_after(i),
                    None => self.set_state(State::Ended),
                }
            }
        }
    }

    /// Says a live stream's title once playback reaches it (after what the output holds).
    fn announce(&mut self, now: i64) {
        if let Some((_, due)) = &self.title {
            if now >= *due {
                let (t, _) = self.title.take().expect("checked");
                (self.events)(Event::Title(t));
            }
        }
        if self.offloading() {
            return;
        }
        let Some(i) = self.p.reading_index() else { return };
        let fresh = self.p.queue.read(|q| q.ids().get(i).and_then(|id| self.p.tracks.loading(id)).and_then(|l| l.announced()));
        if let Some(t) = fresh {
            let track = &self.p.sink.track;
            self.title = Some((t, now + (track.filled_us() + track.latency_us()) / 1000));
        }
    }

    /// Tells a watching client what this wake saw ([`crate::watch`]).
    fn watch(&mut self, now: i64) {
        crate::watch::look(self.watch.as_ref(), || {
            let offloaded = self.offloading();
            let in_output_ms = match self.off.as_ref() {
                Some(o) if offloaded => o.in_track_us() / 1000,
                _ => (self.p.sink.track.filled_us() + self.p.sink.track.latency_us()) / 1000,
            };
            let waiting = self.stalled || self.waiting_for_bytes();
            let state = self.words(in_output_ms);
            let quiet_ms = self.stalled_ms(now);
            let bytes_coming = self.bytes_coming();
            let output_open = if offloaded { self.off.as_ref().is_some_and(|o| o.track().is_some()) } else { self.p.sink.track.opened() };
            let s = self.status.lock();
            crate::watch::Seen {
                now_ms: now,
                playing: s.state == State::Playing && !s.switching && !waiting,
                offloaded,
                index: s.index,
                id: s.id.clone(),
                position_ms: s.position_ms,
                in_output_ms,
                quiet_ms,
                bytes_coming,
                output_open,
                state,
            }
        });
    }

    /// The engine's state in words, for a stall report.
    fn words(&self, in_output_ms: i64) -> String {
        let mut w = format!("{:?}", self.state);
        if self.dip.is_some() {
            w.push_str(", switching");
        }
        if self.pause_at.is_some() {
            w.push_str(", pausing");
        }
        if self.offloading() {
            w.push_str(", offloaded");
        }
        if self.released.is_some() {
            w.push_str(", output let go");
        }
        if self.held.is_some() {
            w.push_str(", a place held");
        }
        w.push_str(&format!("; {}; ring {} ms, output {in_output_ms} ms", self.p.words(), self.p.sink.track.filled_us() / 1000));
        if self.waiting_for_bytes() {
            w.push_str(", waiting for a song's bytes");
        }
        if self.stalled {
            w.push_str(", said to be buffering");
        }
        w.push_str(&format!("; loaders: {}", self.p.tracks.words()));
        w
    }

    /// Keeps [`Status::pcm_why`] current, logging each change.
    fn follow_why(&mut self) {
        let offloaded = self.offloading();
        let why = match self.off.as_ref() {
            Some(o) if offloaded => o.gapped.clone(),
            _ => self.current().is_some().then(|| self.why_on_cpu()),
        };
        let mut s = self.status.lock();
        if s.pcm_why == why {
            return;
        }
        s.pcm_why = why.clone();
        drop(s);
        if let Some(w) = why {
            self.p.app.log(&format!("{}: {w}", if offloaded { "offloaded" } else { "playing on the CPU" }));
        }
    }

    fn why_on_cpu(&self) -> String {
        match self.blocked {
            Some(b) if !self.offload => b.to_string(),
            _ if self.handing_over.is_some() => "offload takes over at the next song".into(),
            _ => self.off.as_ref().and_then(|o| o.on_cpu.as_ref()).map_or_else(|| "the song began on the CPU before offload was wanted".into(), OnCpu::words),
        }
    }

    /// A new queue put another song at index `i` than the one last reported, and no jump is pending.
    fn other_song_at(&self, i: usize) -> bool {
        self.dip.as_ref().is_none_or(|d| d.jumps() == 0) && self.p.queue.read(|q| q.ids().get(i).map(String::as_str) != self.heard_id.as_deref())
    }

    /// The CPU path no longer waits for bytes (released, held, or offloaded): ends a [`Event::Buffering`].
    fn unstall(&mut self) {
        if std::mem::take(&mut self.stalled) {
            (self.events)(Event::Buffering(false));
        }
    }

    /// Writes the status, then says what changed: a client reading the status on an event finds the
    /// event's song and state there.
    fn report(&mut self, now: i64) {
        if let Some((i, ms)) = self.held.as_ref().map(|h| (h.0, h.1)) {
            // A place held while paused is where the player is, to the screen.
            self.unstall();
            let other = self.heard != Some(i) || self.held.as_ref().map(|h| h.2.as_str()) != self.heard_id.as_deref();
            if other {
                self.heard = Some(i);
                self.heard_id = self.held.as_ref().map(|h| h.2.clone());
            }
            {
                let mut s = self.status.lock();
                s.state = self.state;
                if s.index != Some(i) || other {
                    s.index = Some(i);
                    s.id = self.heard_id.clone();
                }
                s.position_ms = ms;
                s.at = Instant::now();
                s.switching = false;
                s.on_cpu = false;
            }
            if other {
                let jumps = self.made();
                (self.events)(Event::Song { index: i, id: self.heard_id.clone().unwrap_or_default(), jumps });
            }
            self.say_position(now, i, ms);
            return;
        }
        if self.released.is_some() {
            self.unstall();
            // Let go: the place last reported stands.
            let mut s = self.status.lock();
            s.releases = self.releases;
            s.offloaded = false;
            s.on_cpu = false;
            return;
        }
        if self.offloading() {
            // The output's decoder says its own waits.
            self.unstall();
            return self.report_offload(now);
        }
        if self.p.current().is_none() {
            self.unstall();
            self.status.lock().on_cpu = false;
            return;
        }
        let track = &self.p.sink.track;
        let stalled = self.state == State::Playing && self.p.starved() && track.filled_us() < STALL_US && track.latency_us() < STALL_US;
        let stall_changed = stalled != self.stalled;
        self.stalled = stalled;
        let seen = self.p.bar();
        let index = seen.index.or(self.p.current());
        let ms = if seen.index.is_some() { seen.ms } else { self.p.position_ms() };
        // The id is copied only when the song changes: this runs on every wake.
        let other = index != self.heard || index.is_some_and(|i| self.other_song_at(i));
        if other {
            self.heard = index;
            self.heard_id = index.map(|i| self.p.id_at(i));
        }
        let looped = !other && self.p.loops != self.loops;
        self.loops = self.p.loops;
        let mixing = self.p.mixing();
        // The place moves at the speed times the tempo a mix brings the song in at.
        let tempo = self.p.sink.pace_heard();
        let stretched = (tempo - 1.0).abs() > 1e-3;
        if self.stretched && !stretched {
            self.placed_due = true;
        }
        self.stretched = stretched;
        let mixing_was = {
            let mut s = self.status.lock();
            let was = s.mixing;
            s.state = self.state;
            if s.index != index || other {
                s.index = index;
                s.id = self.heard_id.clone();
            }
            s.position_ms = ms;
            s.at = Instant::now();
            s.speed = self.p.speed().0;
            s.pace = s.speed * tempo as f32;
            s.mixing = mixing;
            s.underruns = self.p.sink.track.underruns();
            s.releases = self.releases;
            s.switching = self.dip.is_some();
            s.chain = self.p.sink.chain_in();
            s.on_cpu = self.state == State::Playing && !stalled && !s.switching;
            s.gain_reduction_db = self.p.sink.meter_db;
            s.compression_db = self.p.sink.compression_db();
            s.offloaded = false;
            was
        };
        if stall_changed {
            (self.events)(Event::Buffering(stalled));
        }
        let Some(i) = index else { return };
        let jumps = self.made();
        if other {
            (self.events)(Event::Song { index: i, id: self.p.id_at(i), jumps });
        } else if looped {
            (self.events)(Event::Looped { index: i, id: self.p.id_at(i), jumps });
        }
        if mixing != mixing_was {
            (self.events)(Event::Mixing(mixing));
        }
        if std::mem::take(&mut self.placed_due) {
            (self.events)(Event::Placed { index: i, ms });
        }
        self.say_position(now, i, ms);
    }

    /// [`report`](Worker::report) while the songs go to the output's decoder: the queue follows the song
    /// heard, the one after it is fetched, and a song placed again (repeat one) is a loop.
    fn report_offload(&mut self, now: i64) {
        let Some((i, ms, seq)) = self.off.as_mut().and_then(Offload::heard) else { return };
        let other = self.heard != Some(i) || self.other_song_at(i);
        let looped = !other && seq != self.heard_seq && self.heard_seq != 0;
        self.heard_seq = seq;
        if other {
            self.heard = Some(i);
            self.heard_id = Some(self.p.id_at(i));
            self.p.queue.moved_to(i);
            if let Some(n) = self.p.queue.read(|q| q.next_of(i, q.repeat())) {
                let next = self.p.id_at(n);
                self.p.tracks.upcoming(&next);
            }
        }
        if !self.offload_heard && ms > 0 && self.state == State::Playing {
            // Music is heard: a run of songs that would not play is broken.
            self.offload_heard = true;
            self.p.errors.played();
            self.p.app.playing();
        }
        let mixing_was = {
            let mut s = self.status.lock();
            let was = s.mixing;
            s.state = self.state;
            if s.index != Some(i) || s.id != self.heard_id {
                s.index = Some(i);
                s.id = self.heard_id.clone();
            }
            s.position_ms = ms;
            s.at = Instant::now();
            s.speed = 1.0;
            s.pace = 1.0;
            s.mixing = false;
            s.releases = self.releases;
            s.switching = self.dip.is_some();
            s.chain = false;
            s.on_cpu = false;
            s.gain_reduction_db = 0.0;
            s.compression_db = 0.0;
            s.offloaded = true;
            was
        };
        let jumps = self.made();
        if other {
            (self.events)(Event::Song { index: i, id: self.p.id_at(i), jumps });
        } else if looped {
            (self.events)(Event::Looped { index: i, id: self.p.id_at(i), jumps });
        }
        if mixing_was {
            (self.events)(Event::Mixing(false));
        }
        if std::mem::take(&mut self.placed_due) {
            (self.events)(Event::Placed { index: i, ms });
        }
        self.say_position(now, i, ms);
    }

    /// [`Event::Position`]: at the pace asked for while playing, and once when a seek lands.
    fn say_position(&mut self, now: i64, index: usize, ms: i64) {
        let landed = std::mem::take(&mut self.seek_landed);
        let due = match self.positions {
            Some(every) if self.state == State::Playing && now >= self.next_position => {
                self.next_position = now + every;
                true
            }
            _ => false,
        };
        if landed || due {
            (self.events)(Event::Position { index, ms });
        }
    }


    /// How long the thread may sleep: `None` until a command, `Some(0)` not at all. The music's timers,
    /// and while music should move, the stall checks ([`Worker::restart_if_stalled`]).
    fn wake_in(&self, now: i64) -> Option<i64> {
        let d = self.wake_for_music(now);
        let look = match &self.stall {
            Some(q) if q.standing && now - q.since < STALL_SAY_MS => Some((q.since + STALL_SAY_MS - now).max(1)),
            Some(q) if q.standing => Some((q.since + STALL_RESTART_MS - now).max(1)),
            Some(_) if d.is_none() && !self.offloading() => Some(STALL_GUARD_MS),
            _ => None,
        };
        match (d, look) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn wake_for_music(&self, now: i64) -> Option<i64> {
        let mut d: Option<i64> = None;
        let mut at = |ms: i64| d = Some(d.map_or(ms, |x| x.min(ms)));
        if let Some(t) = self.pause_at {
            at(t - now);
        }
        if let Some(d) = &self.dip {
            at(d.at - now);
        }
        if let Some(t) = self.idle_at {
            at(t - now);
        }
        if let Some((_, t)) = &self.title {
            at(t - now);
        }
        if let Some(t) = self.resound_due {
            at(t - now);
        }
        if self.entering.is_some() {
            // Its loader wakes the thread; this is a fallback.
            at(1_000);
        }
        if let Some(m) = self.remake.as_ref().filter(|_| self.dip.is_none() && self.pause_at.is_none()) {
            // The dip is due when playback gets near; still opening, its loader wakes the thread.
            at(if m.ready { m.dip_at - now } else { 1_000 });
        }
        if let Some(off) = self.off.as_ref().filter(|o| o.active()) {
            if let Some(ms) = off.wake_in() {
                at(ms);
            }
            if let (Some(_), State::Playing) = (self.positions, self.state) {
                at(self.next_position - now);
            }
            return d.map(|x| x.max(1));
        }
        if !self.p.playing() {
            return d.map(|x| x.max(1));
        }
        if self.p.hungry() {
            return Some(0);
        }
        let track = &self.p.sink.track;
        let speed = self.p.speed().0.max(0.1) as f64;
        let fill = track.filled_us();
        // A shallow ring (tuning) is topped up at half.
        let low = WAKE_LOW_US.min(self.p.sink.capacity_us / 2);
        if self.p.source_ended() || self.p.sink.reopening() {
            // The end of the queue, or a format change the device reopens for: the pull says when the
            // last of it has played.
            track.wake_at(0);
            at((fill + track.latency_us()) / 1000 + 5);
        } else if self.p.starved() {
            // The loader wakes the thread; this guards against one that never answers.
            at(1_000);
        } else if fill > low {
            track.wake_at(low);
            // The pull wakes the thread at the low mark; the timer covers a slow device clock. A device
            // that pulls in bursts leaves the ring standing between them, so it gets no timer.
            if !track.bursts() {
                at((fill - low) / 1000 + 250);
            }
        } else {
            // The burst's own count (which includes the device) does not agree yet.
            at(200);
        }
        if let Some(u) = self.p.until_next_song_us() {
            at((u as f64 / speed / 1000.0) as i64 + 5);
        }
        let h = self.p.heard();
        if h.id.is_some() || h.mixing || h.next_id.is_some() {
            at(250);
        }
        if self.probe.as_ref().is_some_and(|p| p.2.is_none()) {
            // Its loader wakes the thread; this is a fallback.
            at(1_000);
        }
        if let (Some(_), State::Playing) = (self.positions, self.state) {
            at(self.next_position - now);
        }
        d.map(|x| x.max(1))
    }
}
