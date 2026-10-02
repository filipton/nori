//! The engine's thread and the handle a client drives it with. Playback is `nori_player::pipeline`;
//! this adds the real clock, the controls' fades, audio offload and the events.
//!
//! The thread sleeps unless something is due ([`Worker::wake_in`]): a command, the ring's low mark (once
//! per burst), a song's bytes arriving, or a timed moment. Paused for [`Config::idle_release_ms`] it
//! lets the output and the song's bytes go.
//!
//! With an [`OffloadOutput`] and nothing that touches samples, songs go to the output's decoder as
//! packets and the thread sleeps minutes between top-ups. A song moves between the CPU and that decoder
//! behind a short dip where the ear is, or at a song boundary; leaving it, the CPU opens the song
//! [`REMAKE_LEAD_MS`] ahead first so the handover is never a gap.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

use nori_player::pcm::Encoding;
use nori_player::pipeline::{App, ChainSettings, Player, Queue, Reading, Songs, Sound};
use nori_player::playlist::Playlist;
use nori_player::policy::{audio_policy, offload_blocked, AudioPrefs, OutputState};
use nori_player::queue::previous_restarts;
use nori_player::transport::{load_control, switch_dip, Switch, IDLE_RELEASE_MS};
use parking_lot::Mutex;

use crate::clock::{Clock, Monotonic};
use crate::demux::Demuxed;
use crate::library::{Library, Sources};
use crate::offload::{Offload, OffloadOutput, OnCpu, Step, Tail};
use crate::output::{AudioOutput, Device, RingTrack, WAKE_LOW_US};
use crate::panic_words;
use crate::source::{Fetching, Held};

/// The sound and controls the settings ask for.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub sound: Sound,
    pub speed: f32,
    pub pitch: f32,
    pub skip_silence: bool,
    /// Fade on play, pause and switches, ms (0 off).
    pub fade_ms: i32,
    /// Float decoding and chain, into a device that plays float (`nori_player::policy`).
    pub hi_res: bool,
    /// Highest device rate, Hz (0: the song's own), within the song's rate family.
    pub max_rate: u32,
    /// Let an [`OffloadOutput`] decode songs when nothing needs the samples.
    pub offload: bool,
    pub crossfade_s: i32,
    pub auto_mix: bool,
    /// Most ReplayGain may turn a song up, dB (`nori_player::gain`): above 0 songs are read as floats
    /// with the limiter behind them, and a song turned up stays off offload.
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

/// The settings as the audio policy let them through; `gain_max` linear.
#[derive(Clone, PartialEq, Default)]
struct Applied {
    untouched: bool,
    bit_perfect: bool,
    float: bool,
    gain_max: f32,
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

/// What the engine tells a client, each once as it changes, after writing the status.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    State(State),
    /// The audible song changed. `jumps`: the jumps asked for by then ([`Engine::play_at`] and the
    /// like), so a client that asked for a later one knows this predates it. `seq`: the entry
    /// (`Playlist::seqs`), for a client whose queue was edited since `index`.
    Song { index: usize, seq: Option<u64>, id: String, jumps: u64 },
    /// The song restarted by itself (repeat one).
    Looped { index: usize, seq: Option<u64>, id: String, jumps: u64 },
    /// The position: at the pace of [`Engine::position_updates`], and once when a seek or jump lands.
    /// `jumps`: as [`Event::Song`]'s, so a client knows which of its jumps landed.
    Position { index: usize, ms: i64, jumps: u64 },
    /// A song would not play, or the output would not open (`id` empty).
    Error { id: String, message: String },
    /// The music goes to another output device, by the core's device name.
    Output { name: String },
    /// Playback ran dry waiting for a song's bytes (`true`), or resumed.
    Buffering(bool),
    /// Playback stopped by itself (the queue's rules). `plays`: see [`Engine::superseded`].
    Stopped { plays: u64 },
    /// A live stream's announced title, when playback reaches it.
    Title(String),
    /// A mix became audible (`true`) or ended.
    Mixing(bool),
    /// A song failed for want of network and the offline bridge is to take over; playback waits paused.
    Bridge { plays: u64 },
    /// The place moved without a jump (between the CPU and the output's decoder, or a mix's tempo
    /// ended): a client extrapolating the position re-anchors here.
    Placed { index: usize, ms: i64 },
    /// Whether the CPU must be kept awake while playing; said before the work it is for.
    Awake(bool),
}

/// The engine's last reading, for a screen to read without waking it.
#[derive(Debug, Clone)]
pub struct Status {
    pub state: State,
    /// The audible song: queue index and id, and the position in it at `at`, ms.
    pub index: Option<usize>,
    pub id: Option<String>,
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
    /// The sound chain is in the samples' path; means something only while `on_cpu`.
    pub chain: bool,
    pub on_cpu: bool,
    /// What the limiter and the compressor took off the last buffer, dB.
    pub gain_reduction_db: f32,
    pub compression_db: f32,
    pub offloaded: bool,
    /// The settings allow offload, as last applied.
    pub offload_wanted: bool,
    /// Why the music is on the CPU rather than offloaded, for a report.
    pub pcm_why: Option<String>,
    /// As last said by [`Event::Awake`].
    pub awake: bool,
}

impl Default for Status {
    fn default() -> Self {
        Status {
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
        }
    }
}

impl Status {
    /// The position now: the last reading moved on at its pace while playing.
    pub fn position_now(&self) -> i64 {
        match self.state {
            State::Playing => self.position_ms + (self.at.elapsed().as_secs_f64() * 1000.0 * self.pace as f64) as i64,
            _ => self.position_ms,
        }
    }

    /// The position for a seek bar, and whether the reading is old enough to [`Engine::look`] again.
    pub fn screen_now(&self) -> (i64, bool) {
        nori_player::heard::screen_place(self.position_ms, self.at.elapsed().as_millis() as i64, self.pace, self.state == State::Playing)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Memory the platform gives the app, MB (`nori_player::transport::load_control`).
    pub memory_mb: u32,
    pub settings: Settings,
    /// Paused this long, the output and the song's bytes are let go, ms.
    pub idle_release_ms: i64,
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
    /// With this fade (`None`: the settings').
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
    Shallow(bool),
    Positions(Option<Duration>),
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
    /// Hand the song between the CPU and the output's decoder.
    Hand,
}

impl Switched {
    fn jumps(&self) -> bool {
        matches!(self, Switched::To(..) | Switched::Next | Switched::Previous)
    }
}

/// A fade down to silence (down at `at`, engine ms), the switches made at its bottom, and the fade up.
struct Dip {
    at: i64,
    up_ms: i64,
    then: Vec<Switched>,
}

/// The handle: every call sends a command, wakes the engine's thread and returns at once.
pub struct Engine {
    tx: Sender<Command>,
    thread: Thread,
    join: Mutex<Option<JoinHandle<()>>>,
    status: Arc<Mutex<Status>>,
    /// Jumps and plays asked for so far ([`Event::Song`], [`Event::Stopped`]).
    jumps: AtomicU64,
    plays: AtomicU64,
    /// How a command wakes the thread on a test's clock ([`Engine::start_on`]).
    wake: Option<Box<dyn Fn() + Send + Sync>>,
    /// Its songs' fetches.
    fetching: Arc<Fetching>,
}

impl Engine {
    /// Starts the engine's thread over `library`, `app` (transition planner and log) and `queue`,
    /// playing through `output`, or `offload` when the settings allow. `events` runs on the engine's
    /// thread and should only hand the event on.
    pub fn start<L, A, Q, E>(library: L, app: A, queue: Q, output: Box<dyn AudioOutput>, offload: Option<Box<dyn OffloadOutput>>, config: Config, events: E) -> Engine
    where
        L: Library,
        A: App + Send + 'static,
        Q: Queue + Send + 'static,
        E: FnMut(Event) + Send + 'static,
    {
        Engine::launch(library, app, queue, output, offload, config, Monotonic::new(), false, events)
    }

    /// [`Engine::start`] on a test's clock: every command goes through [`Clock::wake`].
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
    fn launch<L, A, Q, E, C>(library: L, app: A, queue: Q, mut output: Box<dyn AudioOutput>, offload: Option<Box<dyn OffloadOutput>>, config: Config, clock: C, hooked: bool, events: E) -> Engine
    where
        L: Library,
        A: App + Send + 'static,
        Q: Queue + Send + 'static,
        E: FnMut(Event) + Send + 'static,
        C: Clock + Sync,
    {
        let (tx, rx) = channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let fetching = Arc::new(Fetching::default());
        let loading = fetching.clone();
        let (shared, devices, hook, own) = (status.clone(), tx.clone(), clock.clone(), clock.clone());
        let join = std::thread::Builder::new()
            .name("nori-engine".into())
            .spawn(move || {
                let me = std::thread::current();
                let wake = me.clone();
                // Device changes arrive on the output's own thread.
                output.watch(Box::new(move |d| {
                    if devices.send(Command::Device(d)).is_ok() {
                        own.wake(&wake);
                    }
                }));
                let songs = Sources::new(library, load_control(config.memory_mb), clock.waits(), me, loading);
                let player = Player::build(songs, queue, app, RingTrack::new(output));
                Worker::new(player, offload.map(Offload::new), rx, events, shared, config, clock).run();
            })
            .expect("a thread for the engine");
        let thread = join.thread().clone();
        let wake = hooked.then(|| {
            let t = thread.clone();
            Box::new(move || hook.wake(&t)) as Box<dyn Fn() + Send + Sync>
        });
        Engine { tx, thread, join: Mutex::new(Some(join)), status, jumps: AtomicU64::new(0), plays: AtomicU64::new(0), wake, fetching }
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

    /// Plays queue index `index` from `ms`.
    pub fn play_at(&self, index: usize, ms: i64) -> u64 {
        self.jump(Command::PlayAt(index, ms))
    }

    /// Goes to queue index `index` at `ms`, playing or paused as before; paused, nothing is fetched
    /// until play.
    pub fn go_to(&self, index: usize, ms: i64) -> u64 {
        self.jump(Command::GoTo(index, ms))
    }

    /// Pauses at the end of the current song, on the next one at its start (the sleep timer); nothing
    /// after it is read or mixed into. `false` cancels.
    pub fn pause_at_end(&self, on: bool) {
        self.send(Command::PauseAtEnd(on));
    }

    /// Returns the play's number ([`Event::Stopped`]).
    pub fn play(&self) -> u64 {
        let n = self.plays.fetch_add(1, Ordering::AcqRel) + 1;
        self.send(Command::Play);
        n
    }

    /// Whether `event` is a stop from before a later [`Engine::play`]: a client should ignore it.
    pub fn superseded(&self, event: &Event) -> bool {
        match event {
            Event::Stopped { plays } | Event::Bridge { plays } => *plays < self.plays.load(Ordering::Acquire),
            _ => false,
        }
    }

    pub fn pause(&self) {
        self.send(Command::Pause(None));
    }

    /// Pauses at once whatever the fade setting (headphones pulled out), cutting a fade short.
    pub fn pause_now(&self) {
        self.send(Command::Pause(Some(0)));
    }

    pub fn toggle(&self) {
        self.send(Command::Toggle);
    }

    /// Paused, a skip also starts playback.
    pub fn next(&self) -> u64 {
        self.jump(Command::Next)
    }

    /// Restarts the song a few seconds in, else the previous one; paused, it also starts playback.
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

    /// The transition out of the current song is planned again (settings or an analysis changed).
    pub fn replan(&self) {
        self.send(Command::Replan);
    }

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

    /// The device holds only a fraction of a second (`true`: the app in sight on a phone, the
    /// equalizer page on the desktop), so a sound change is heard soon, or its deep buffer again.
    /// Nothing it holds is dropped: going shallow it plays out first.
    pub fn set_shallow(&self, on: bool) {
        self.send(Command::Shallow(on));
    }

    /// Position events this often while playing, or none (the default).
    pub fn position_updates(&self, every: Option<Duration>) {
        self.send(Command::Positions(every));
    }

    /// Reads the output once now and updates the status (offloaded, the engine may sleep for minutes).
    pub fn look(&self) {
        self.send(Command::Look);
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    /// Reads the status in place, for a question asked every frame.
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

    /// What its songs' loaders hold now, for the memory report.
    pub fn held(&self) -> Held {
        self.fetching.held()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A place nothing is loaded for, played from on play: one chosen while paused (`shown`: its id, the
/// status shows it at once), or where the player was when the output was let go.
struct Parked {
    at: usize,
    ms: i64,
    shown: Option<String>,
}

/// The offloaded song opened on the CPU ahead of the output's decoder, taken over at `from_ms` behind a
/// dip starting at `dip_at` (engine ms) while the output plays on.
struct Takeover {
    id: String,
    from_ms: i64,
    r: Demuxed,
    ready: bool,
    dip_at: i64,
}

/// Moving songs between the CPU and the output's decoder.
#[derive(Default)]
struct Handover {
    /// The next song opened as packets and, once known, whether the output decodes it: if so the CPU
    /// plays the current song to its end (`at_end`) and the output takes the next.
    probe: Option<(usize, Result<Demuxed, String>, Option<bool>)>,
    at_end: Option<usize>,
    /// The current song opened as packets, to hand it over where the ear is (`now` once it may).
    entering: Option<(usize, Result<Demuxed, String>)>,
    now: bool,
    takeover: Option<Takeover>,
    /// Torn tracks; at [`TEAR_DOWNS`] offload is given up (`refused`) for the engine's life.
    tear_downs: u32,
    refused: bool,
    /// Music was heard since the offload path started (ends a run of failing songs).
    heard: bool,
    /// The offloaded song's placing last reported: another is a repeat-one loop.
    seq: u64,
}

/// What the client was last told, and what it is still to be told.
#[derive(Default)]
struct Told {
    /// The song, by index and id (a new queue can put another song at the same index).
    heard: Option<(usize, String)>,
    loops: u32,
    stalled: bool,
    placed: bool,
    seek_landed: bool,
    /// The jumps made by the last [`Event::Position`].
    landed_jumps: u64,
    /// The song plays at a mix's tempo: [`Event::Placed`] once it is back at its own.
    stretched: bool,
    asleep: bool,
    /// Position event interval and the next one due, ms.
    positions: Option<i64>,
    next_position: i64,
    /// A live stream's title, and when playback reaches it.
    title: Option<(String, i64)>,
}

/// Where the position stood still since `since` (engine ms), and whether it did at the last look.
#[derive(Debug, Clone, PartialEq)]
struct Stall {
    since: i64,
    place: (Option<usize>, i64),
    standing: bool,
}

const PANICS_WITHIN_MS: i64 = 60_000;
/// This many panics within [`PANICS_WITHIN_MS`] stop playback until a command.
const PANICS_KEPT_ON: usize = 3;
/// Playing with the position still this long and no bytes on their way, the song is opened again from
/// scratch: an output that stopped taking music without saying so.
const STALL_RESTART_MS: i64 = 10_000;
/// A standing position is looked at once more at this point, so a watching client sees it.
const STALL_SAY_MS: i64 = 5_000;
const STANDING_MS: i64 = 250;
/// Playing on the CPU with nothing else due, the position is still looked at this often (longer than a
/// burst, so never while music plays).
const STALL_GUARD_MS: i64 = 30_000;
/// Less than this to play while a song's bytes are on their way is [`Event::Buffering`].
const STALL_US: i64 = 200_000;
const TEAR_DOWNS: u32 = 2;
/// The dip a song is handed between the CPU and the output's decoder behind, down and up, ms.
const HAND_DIP_MS: i64 = 30;
/// Chain changes closer together than this (a slider dragged) are made together.
const CHAIN_EVERY_MS: i64 = 100;
/// How far ahead of the output's decoder the CPU opens an offloaded song it takes over.
pub const REMAKE_LEAD_MS: i64 = 120;

struct Worker<L: Library, A: App, Q: Queue, E: FnMut(Event), C: Clock> {
    p: Player<Sources<L>, RingTrack, A, Q>,
    off: Option<Offload>,
    rx: Receiver<Command>,
    events: E,
    status: Arc<Mutex<Status>>,
    clock: C,
    settings: Settings,
    applied: Option<Applied>,
    facts: OutputFacts,
    idle_release_ms: i64,
    watch: Option<crate::watch::Watcher>,
    state: State,
    /// When a pause's fade ends.
    pause_at: Option<i64>,
    dip: Option<Dip>,
    parked: Option<Parked>,
    /// Paused: when the output is let go.
    idle_at: Option<i64>,
    releases: u64,
    /// Jumps and plays taken off the channel.
    jumps: u64,
    plays: u64,
    gain_changed: bool,
    /// The settings and output allow offload; if not, why.
    offload: bool,
    blocked: Option<&'static str>,
    h: Handover,
    told: Told,
    /// The queue's entries as last seen, to find the parked place and the offloaded songs again.
    seqs: Vec<u64>,
    /// Chain settings waiting for [`CHAIN_EVERY_MS`] since the last change at `chain_at`.
    chain_wanted: Option<ChainSettings>,
    chain_at: i64,
    panics: VecDeque<i64>,
    stall: Option<Stall>,
    /// The song last opened again from scratch: the same with nothing heard since has failed.
    restarted: Option<String>,
    /// A jump under way: a panic during it restarts there.
    jumping: Option<(usize, i64)>,
}

impl<L: Library, A: App, Q: Queue, E: FnMut(Event), C: Clock> Worker<L, A, Q, E, C> {
    fn new(p: Player<Sources<L>, RingTrack, A, Q>, off: Option<Offload>, rx: Receiver<Command>, events: E, status: Arc<Mutex<Status>>, config: Config, clock: C) -> Self {
        let seqs = p.queue.read(|q| q.seqs().to_vec());
        let (idle_release_ms, watch) = (config.idle_release_ms, config.watch);
        let mut w = Worker {
            p,
            off,
            rx,
            events,
            status,
            clock,
            settings: Settings::default(),
            applied: None,
            facts: OutputFacts::default(),
            idle_release_ms,
            watch,
            state: State::Idle,
            pause_at: None,
            dip: None,
            parked: None,
            idle_at: None,
            releases: 0,
            jumps: 0,
            plays: 0,
            gain_changed: false,
            offload: false,
            blocked: None,
            h: Handover::default(),
            told: Told::default(),
            seqs,
            chain_wanted: None,
            chain_at: i64::MIN / 2,
            panics: VecDeque::new(),
            stall: None,
            restarted: None,
            jumping: None,
        };
        w.apply(config.settings);
        w
    }

    /// Engine time, ms.
    fn now(&self) -> i64 {
        self.clock.now_ms() + 1_000
    }

    /// A turn per wake. A panicking turn is reported and the song opened again ([`Worker::recover`]):
    /// a dead thread once left the app claiming to play in silence.
    fn run(mut self) {
        loop {
            let wake = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.turn())) {
                Ok(None) => return,
                Ok(Some(w)) => w,
                Err(p) => self.panicked(panic_words(&*p)),
            };
            match wake {
                Some(0) => continue,
                w => self.clock.sleep(w.map(|ms| ms as u64), || self.waiting_for_bytes()),
            }
        }
    }

    /// After a panicking turn: recovers (itself guarded), or waits for a command.
    fn panicked(&mut self, mut why: String) -> Option<i64> {
        let now = self.now();
        for _ in 0..=PANICS_KEPT_ON {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.recover(now, &why))) {
                Ok(true) => return Some(1),
                Ok(false) => break,
                Err(p) => why = panic_words(&*p),
            }
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.p.app.log("the engine's thread panicked again and again: it waits for a command");
            if self.state == State::Playing {
                (self.events)(Event::Stopped { plays: self.plays });
                self.set_state(State::Paused);
            }
        }));
        None
    }

    /// One wake: the commands, the music, the status and events. Returns how long to sleep, or `None`
    /// to stop.
    fn turn(&mut self) -> Option<Option<i64>> {
        self.clock.woke();
        loop {
            match self.rx.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return None,
                Ok(c) => {
                    // Kept awake for the command's work; `follow_awake` may let go at the end.
                    self.say_awake(true);
                    self.command(c)
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        let now = self.now();
        self.due(now);
        self.follow_gain();
        if self.p.app.measured() {
            self.replan();
        }
        if self.offloading() {
            self.turn_offload(now);
        } else {
            // The burst's estimate of what the device holds drifts; the ring's fill is exact.
            if self.p.playing() && !self.p.source_ended() && self.p.sink.track.filled_us() <= WAKE_LOW_US {
                self.p.burst.restart();
            }
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
        Some(self.wake_in(now))
    }

    /// The CPU may sleep only while offloaded, fed, and nothing else is under way.
    fn follow_awake(&mut self) {
        let h = &self.h;
        let quiet = self.pause_at.is_none() && self.dip.is_none() && h.takeover.is_none() && h.entering.is_none() && h.probe.is_none() && h.at_end.is_none() && self.parked.is_none();
        let sleeps = self.state == State::Playing && quiet && self.chip().is_some_and(Offload::lets_cpu_sleep);
        self.say_awake(!sleeps);
    }

    fn say_awake(&mut self, awake: bool) {
        if awake == self.told.asleep {
            self.told.asleep = !awake;
            self.status.lock().awake = awake;
            (self.events)(Event::Awake(awake));
        }
    }

    /// Some song's bytes are awaited (a test's clock stands still meanwhile).
    fn waiting_for_bytes(&self) -> bool {
        let h = &self.h;
        self.p.waiting_for_bytes() || h.entering.is_some() || h.takeover.as_ref().is_some_and(|m| !m.ready) || h.probe.as_ref().is_some_and(|p| p.2.is_none()) || self.off.as_ref().is_some_and(Offload::waiting_for_bytes)
    }

    // ---- the CPU and offload paths as one player ----

    /// The offload path, when it holds the song (or the song starting there is still opening).
    fn chip(&self) -> Option<&Offload> {
        self.off.as_ref().filter(|o| o.active())
    }

    fn chip_mut(&mut self) -> Option<&mut Offload> {
        self.off.as_mut().filter(|o| o.active())
    }

    fn offloading(&self) -> bool {
        self.chip().is_some()
    }

    fn playing(&self) -> bool {
        self.chip().map_or(self.p.playing(), Offload::playing)
    }

    /// Playing and no pause fading.
    fn going(&self) -> bool {
        self.state == State::Playing && self.pause_at.is_none()
    }

    fn current(&self) -> Option<usize> {
        self.chip().map_or(self.p.current(), Offload::current)
    }

    /// The song and place the seek bar shows: in a mix, the outgoing song until the takeover.
    fn shown(&mut self) -> Option<(usize, i64)> {
        if !self.offloading() {
            let seen = self.p.bar();
            if let Some(i) = seen.index {
                return Some((i, seen.ms));
            }
        }
        let i = self.current()?;
        Some((i, self.position_ms()))
    }

    fn position_ms(&mut self) -> i64 {
        match self.chip_mut() {
            Some(o) => o.heard().map_or(0, |h| h.1),
            None => self.p.position_ms(),
        }
    }

    fn go_on(&mut self) {
        match self.chip_mut() {
            Some(o) => o.play(),
            None => self.p.resume(),
        }
    }

    fn halt(&mut self) {
        match self.chip_mut() {
            Some(o) => o.pause(),
            None => self.p.pause(),
        }
    }

    /// Fades the path that holds the song from `from` (or where it is) to `to` over `ms`.
    fn ramp(&mut self, from: Option<f32>, to: f32, ms: i64) {
        let now = self.now();
        match self.chip_mut() {
            Some(o) => o.ramp(from, to, ms, now),
            None => self.p.sink.track.ramp(from, to, ms),
        }
    }

    /// Queue index `i` from `ms`: offloaded when allowed (the song decides once open), else on the CPU.
    fn jump(&mut self, i: usize, ms: i64) {
        self.jumping = Some((i, ms));
        (self.h.probe, self.h.at_end, self.h.takeover) = (None, None, None);
        if self.offload && self.off.is_some() {
            if !self.offloading() {
                // Never both outputs open at once.
                self.p.pause();
                self.p.release();
                self.p.sink.track.release();
            }
            let going = self.going();
            let Some(off) = self.off.as_mut() else { return };
            let playing = off.playing() || going;
            let i = off.start(i, ms, &mut self.p.tracks, &self.p.queue);
            if playing {
                off.play();
            }
            // A song placed again for a jump is not a repeat-one loop.
            (self.h.heard, self.h.seq) = (false, 0);
            self.p.queue.moved_to(i);
        } else {
            self.leave_offload();
            self.p.jump(i, ms);
        }
        self.jumping = None;
    }

    /// Releases the offload path; returns where it was.
    fn leave_offload(&mut self) -> Option<(usize, i64)> {
        self.off.as_mut()?.release()
    }

    /// The CPU plays song `i` from `ms` (opened ahead as `opened` if given), the offload path let go.
    /// Returns the level to come back up from when playing.
    fn onto_cpu(&mut self, i: usize, ms: i64, opened: Option<(String, Demuxed, i64)>) -> Option<f32> {
        let playing = self.going();
        self.leave_offload();
        match opened {
            Some(o) => self.p.jump_from(i, ms, o),
            None => self.p.jump(i, ms),
        }
        self.told.placed = true;
        playing.then(|| {
            self.p.resume();
            0.0
        })
    }

    /// Runs the offload path's turn; the CPU takes over where it hands the song back.
    fn turn_offload(&mut self, now: i64) {
        let Worker { p, off, .. } = self;
        let Some(off) = off.as_mut() else { return };
        let (app, max) = (&mut p.app, p.gain_max);
        let Step::ToPcm { index, ms, refused } = off.turn(now, &mut p.tracks, &p.queue, &mut |i, id| app.gain(i, id).min(max)) else { return };
        if refused {
            self.h.tear_downs += 1;
            self.p.app.log(&format!("the offloaded track failed ({} times): the CPU plays on", self.h.tear_downs));
            if self.h.tear_downs >= TEAR_DOWNS && !self.h.refused {
                self.h.refused = true;
                self.apply(self.settings.clone());
            }
        }
        self.onto_cpu(index, ms, None);
    }

    /// Offload wanted while the CPU plays: when the output decodes the next song, the CPU plays the
    /// current one to its end and hands over there (as media3 reconfigures its sink at a boundary).
    fn follow_offload_ahead(&mut self) {
        if !self.offload || self.off.is_none() || self.h.at_end.is_some() || !self.p.playing() {
            return;
        }
        let Some(cur) = self.p.current() else { return };
        let Some(next) = self.p.queue.read(|q| q.next_of(cur, q.repeat())) else { return };
        if self.h.probe.as_ref().is_none_or(|p| p.0 != next) {
            self.h.probe = Some((next, self.p.tracks.open_packets(&self.p.id_at(next), 0, true), None));
        }
        let (_, opened, known) = self.h.probe.as_mut().expect("set above");
        if known.is_none() {
            if opened.as_mut().is_ok_and(|r| !r.ready()) {
                return;
            }
            // The offload path opens the song anew.
            let opened = std::mem::replace(opened, Err(String::new()));
            let why = self.judge(next, opened);
            self.h.probe.as_mut().expect("set above").2 = Some(why.is_none());
        }
        // Only while the current song is still being read; past it, the handover waits a song.
        if self.h.probe.as_ref().is_some_and(|p| p.2 == Some(true)) && self.p.reading_index() == Some(cur) && self.p.stopping_after().is_none() {
            self.p.app.log("offload takes over at the next song");
            self.p.pause_at_end(true);
            self.h.at_end = Some(cur);
        }
    }

    /// Offload wanted while the CPU plays: once the current song is known to be decodable there, the
    /// output takes it over where the ear is, behind a dip.
    fn follow_offload_now(&mut self) {
        let Some(i) = self.h.entering.as_ref().map(|e| e.0) else { return };
        if !self.offload || self.p.current() != Some(i) || !self.p.playing() || self.offloading() {
            self.h.entering = None;
            return;
        }
        if self.h.entering.as_mut().is_some_and(|e| e.1.as_mut().is_ok_and(|r| !r.ready())) {
            return;
        }
        let (i, opened) = self.h.entering.take().expect("checked");
        if self.off.is_some() && self.judge(i, opened).is_none() {
            self.p.app.log("offload takes over where the ear is");
            self.h.now = true;
            let now = self.now();
            self.dip_down(now, HAND_DIP_MS, HAND_DIP_MS).then.push(Switched::Hand);
        }
    }

    /// Why the output would not decode song `i` as `opened`, noted as why it plays on the CPU.
    fn judge(&mut self, i: usize, opened: Result<Demuxed, String>) -> Option<OnCpu> {
        let level = self.gain_of(i);
        let Worker { p, off, .. } = self;
        let off = off.as_mut().expect("an offload output");
        let why = match &opened {
            Ok(r) => {
                let album = off.in_album(i, &p.tracks, &p.queue);
                off.refuses(r, album, level)
            }
            Err(_) => Some(OnCpu::Unread),
        };
        if why.is_some() {
            off.on_cpu = why.clone();
        }
        why
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
        if self.playing() || self.parked.as_ref().is_some_and(|p| p.shown.is_none()) {
            return;
        }
        let at = self.leave_offload().or_else(|| self.p.release());
        if self.parked.is_none() {
            self.parked = at.map(|(at, ms)| Parked { at, ms, shown: None });
        }
        self.p.sink.track.release();
        self.p.tracks.let_go();
        self.releases += 1;
    }

    /// Lets the device go (it failed, or offload is wanted while paused), keeping the place.
    fn let_go(&mut self) {
        if self.parked.as_ref().is_some_and(|p| p.shown.is_none()) {
            return;
        }
        let at = self.p.release();
        if self.parked.is_none() {
            self.parked = at.map(|(at, ms)| Parked { at, ms, shown: None });
        }
        self.p.sink.track.release();
    }

    /// After the output was let go, opens the song again where it was.
    fn reopen(&mut self) {
        if let Some(p) = self.parked.take_if(|p| p.shown.is_none()) {
            self.jump(p.at, p.ms);
        }
    }

    /// A turn panicked: reported, and the song opened again from scratch ([`Worker::start_again`]);
    /// after [`PANICS_KEPT_ON`] within [`PANICS_WITHIN_MS`] playback stops instead. Returns whether the
    /// music goes on.
    fn recover(&mut self, now: i64, why: &str) -> bool {
        while self.panics.front().is_some_and(|t| now - t > PANICS_WITHIN_MS) {
            self.panics.pop_front();
        }
        self.panics.push_back(now);
        let on = self.jumping.map(|j| j.0).or_else(|| self.current()).or_else(|| self.p.queue.read(|q| q.current())).map(|i| self.p.id_at(i));
        let song = on.as_deref().map(|id| format!(" on {id}")).unwrap_or_default();
        let id = on.unwrap_or_default();
        if self.panics.len() >= PANICS_KEPT_ON {
            self.p.app.log(&format!("the engine's thread panicked{song} ({why}), {} times within a minute: playback stops", self.panics.len()));
            self.jumping = None;
            self.let_go_of_everything();
            self.p.tracks.let_go();
            (self.events)(Event::Error { id, message: format!("the player failed: {why}") });
            (self.events)(Event::Stopped { plays: self.plays });
            self.set_state(State::Paused);
            return false;
        }
        self.p.app.log(&format!("the engine's thread panicked{song} ({why}): the music is made again from scratch"));
        (self.events)(Event::Error { id, message: format!("the player panicked ({why}): the song is opened again from scratch") });
        self.start_again(&format!("a panic: {why}"));
        true
    }

    /// Drops everything held for the music, keeping the place in the queue.
    fn let_go_of_everything(&mut self) {
        let h = std::mem::take(&mut self.h);
        (self.h.tear_downs, self.h.refused, self.h.heard, self.h.seq) = (h.tear_downs, h.refused, h.heard, h.seq);
        if h.at_end.is_some() {
            self.p.pause_at_end(false);
        }
        (self.dip, self.pause_at, self.parked, self.stall) = (None, None, None, None);
        self.unstall();
        self.leave_offload();
        self.p.release();
        self.p.sink.track.release();
    }

    /// Opens the song again from scratch where the ear is, its bytes and cache entry dropped
    /// ([`Library::forget`]). The same song again with nothing heard since fails as a song that would
    /// not play.
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
            self.ramp(Some(0.0), 1.0, HAND_DIP_MS);
            self.set_state(State::Playing);
        }
    }

    /// Whether the bytes the music waits for are still being fetched (the song being read, or the next
    /// once that is read to its end).
    fn bytes_coming(&self) -> bool {
        if let Some(o) = self.chip() {
            return o.waiting_for_bytes();
        }
        let Some(r) = self.p.reading_index().filter(|_| self.p.waiting_for_bytes()) else { return false };
        let next = self.p.queue.read(|q| q.next_of(r, q.repeat()));
        [Some(r), next].into_iter().flatten().any(|i| self.p.tracks.loading(&self.p.id_at(i)).is_some_and(|l| l.fetching()))
    }

    /// While music should move, opens the song again from scratch ([`Worker::start_again`]) once the
    /// position stood still [`STALL_RESTART_MS`] with no bytes on their way.
    fn restart_if_stalled(&mut self, now: i64) {
        if !(self.going() && self.dip.is_none() && self.parked.is_none() && self.p.queue.read(|q| !q.is_empty())) {
            self.stall = None;
            return;
        }
        let place = (self.current(), self.position_ms());
        let mut q = match self.stall.take() {
            // Turns close together see a moving place unmoved.
            Some(q) if q.place == place => Stall { standing: now - q.since >= STANDING_MS, ..q },
            Some(_) => {
                // Moving: a song opened again plays.
                self.restarted = None;
                Stall { since: now, place, standing: false }
            }
            None => Stall { since: now, place, standing: false },
        };
        // Bytes on their way fail through their request's own stall; an offloaded track has its own
        // watchdog, which knows how long a platform's count may stand still.
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

    /// The output device changed: the app may give it its own sound.
    fn device(&mut self, d: Device) {
        if let Some(off) = self.off.as_mut() {
            off.output_moved();
        }
        let Some((name, sound)) = self.p.app.output_changed(d.kind, &d.name) else { return };
        if let Some(sound) = sound {
            self.apply(Settings { sound, ..self.settings.clone() });
        }
        (self.events)(Event::Output { name });
    }

    /// Jumps made or parked, not counting those waiting in the dip ([`Event::Song`]'s `jumps`).
    fn made(&self) -> u64 {
        self.jumps - self.dip.as_ref().map_or(0, |d| d.then.iter().filter(|s| s.jumps()).count()) as u64
    }

    fn command(&mut self, c: Command) {
        let now = self.now();
        match c {
            Command::PlayAt(i, ms) => {
                self.jumps += 1;
                self.parked.take_if(|p| p.shown.is_some());
                self.switch(Switched::To(i, ms), Switch::ToSong, now)
            }
            Command::GoTo(i, ms) => {
                self.jumps += 1;
                self.go(Switched::To(i, ms), Switch::ToSong, now)
            }
            Command::Next | Command::Previous => {
                self.jumps += 1;
                let s = if matches!(c, Command::Next) { Switched::Next } else { Switched::Previous };
                // Paused, a skip also plays.
                if self.playing() && self.pause_at.is_none() {
                    self.switch(s, Switch::Skip, now);
                } else {
                    self.hold(s);
                    self.play();
                }
            }
            Command::PauseAtEnd(on) => self.pause_at_end(on),
            Command::Play => {
                self.plays += 1;
                self.play()
            }
            Command::Pause(fade) => self.pause(now, fade.unwrap_or(self.settings.fade_ms)),
            Command::Toggle if self.state == State::Playing => self.pause(now, self.settings.fade_ms),
            Command::Toggle => self.play(),
            Command::Seek(ms) => self.go(Switched::Seek(ms), Switch::Seek, now),
            Command::Settings(s) => self.apply(*s),
            Command::Output(facts) => {
                self.facts = facts;
                self.apply(self.settings.clone());
            }
            Command::Replan => self.replan(),
            Command::QueueChanged => {
                self.p.queue_changed();
                self.follow_parked();
                self.follow_queue();
                // Another song may follow now.
                self.replan();
            }
            Command::Repeat(m) => {
                self.p.set_repeat(m);
                self.follow_queue();
            }
            Command::Gain => self.gain_changed = true,
            Command::Shallow(on) => self.p.sink.track.shallow(on),
            Command::Positions(every) => {
                self.told.positions = every.map(|d| d.as_millis().max(1) as i64);
                self.told.next_position = now;
            }
            Command::Device(d) => self.device(d),
            Command::Look | Command::Stop => {}
        }
    }

    /// The sleep timer's end of this song.
    fn pause_at_end(&mut self, on: bool) {
        // It replaces a handover waiting at the same end.
        if self.h.at_end.take().is_some() && !on {
            self.p.pause_at_end(false);
        }
        let Worker { p, off, .. } = self;
        let Some(o) = off.as_mut().filter(|o| o.active()) else { return p.pause_at_end(on) };
        // The next song is written already: restart here without it.
        if o.pause_at_end(on, &mut p.tracks, &p.queue) {
            if let Some((i, ms, _)) = o.heard() {
                self.jump(i, ms);
                if let Some(o) = self.off.as_mut() {
                    o.stop_after = Some(i);
                }
            }
        }
    }

    /// The queue changed: the song heard follows its entry, and the offload path finds its songs again,
    /// restarting where the ear is when a song it already wrote no longer follows.
    fn follow_queue(&mut self) {
        let old = std::mem::replace(&mut self.seqs, self.p.queue.read(|q| q.seqs().to_vec()));
        if let Some((i, _)) = self.told.heard.as_mut() {
            if let Some(k) = old.get(*i).and_then(|s| self.seqs.iter().position(|n| n == s)) {
                *i = k;
            }
        }
        self.h.probe = None;
        let Worker { p, off, .. } = self;
        let Some(off) = off.as_mut().filter(|o| o.active()) else { return };
        let heard = off.heard().map(|(i, ms, _)| (old.get(i).copied(), i, ms));
        if !off.queue_changed(&old, &mut p.tracks, &p.queue) {
            return;
        }
        // The song heard goes on where it is; taken out, the one now in its place plays from its start.
        let n = self.seqs.len();
        match heard.map(|(seq, i, ms)| (seq.and_then(|s| self.seqs.iter().position(|&n| n == s)), i, ms)) {
            Some((Some(k), _, ms)) => self.jump(k, ms),
            Some((None, i, _)) if n > 0 => self.jump(i.min(n - 1), 0),
            _ => {}
        }
    }

    /// Applies the settings through `nori_player::policy::audio_policy`: bit-perfect leaves samples
    /// untouched, high quality output runs it all in float, and anything touching samples keeps songs
    /// off offload.
    fn apply(&mut self, s: Settings) {
        let hi_res = s.hi_res && self.p.sink.track.takes_float();
        let bit_perfect = self.facts.bit_perfect;
        let prefs = AudioPrefs { dsp: s.sound.on(), skip_silence: s.skip_silence, offload: s.offload && self.off.is_some(), crossfade_s: s.crossfade_s, auto_mix: s.auto_mix, speed: s.speed, pitch: s.pitch };
        let state = OutputState { hi_res, bit_perfect, usb: self.facts.usb, offload_refused: self.h.refused };
        let policy = audio_policy(&prefs, &state);
        self.blocked = if self.off.is_some() { offload_blocked(&prefs, &state) } else { Some("the output does not decode songs itself") };
        // Turning up needs float samples and the limiter; the limiter alone does not block offload.
        let boost_db = if s.gain_boost_db > 0.0 { s.gain_boost_db.min(nori_player::gain::BOOST_MAX_DB) } else { 0.0 };
        let gain_max = if policy.untouched || boost_db == 0.0 { 1.0 } else { 10f32.powf(boost_db / 20.0) };
        let mut sound = if policy.untouched { Sound::default() } else { s.sound.clone() };
        sound.limiter |= gain_max > 1.0;
        let now = Applied { untouched: policy.untouched, bit_perfect, float: hi_res, gain_max };
        self.p.sink.track.set_float(hi_res);
        // Takes effect when the output is made again.
        self.p.sink.track.max_rate = s.max_rate;
        let first = self.applied.is_none();
        let was = self.applied.take().unwrap_or(Applied { gain_max: 1.0, ..Applied::default() });
        let encoding = if policy.float || gain_max > 1.0 { Encoding::Float } else { Encoding::Pcm16 };
        if first || was.gain_max != now.gain_max {
            self.p.tracks.encoding = encoding;
            self.p.gain_max = gain_max;
            self.gain_changed |= !first;
        }
        if first || (was.untouched, was.bit_perfect, was.float) != (now.untouched, now.bit_perfect, now.float) {
            self.p.tracks.encoding = encoding;
            self.p.sink.track.exact = policy.untouched;
            self.p.gain_off = bit_perfect;
            self.p.engine.lock_rate = policy.lock_rate;
            self.p.app.transitions_off(policy.transitions_off);
            self.gain_changed |= was.bit_perfect != now.bit_perfect;
        }
        // The equalizer stays in (flat) but for bit-perfect output.
        self.chain_wanted = Some(ChainSettings { sound, speed: s.speed, pitch: s.pitch, skip_silence: policy.skip_silence, keep_eq: !policy.untouched });
        // The plan out of the current song was made under the old transition settings.
        let replan = first || was.untouched != now.untouched || (self.settings.crossfade_s, self.settings.auto_mix) != (s.crossfade_s, s.auto_mix);
        self.applied = Some(now);
        self.settings = s;
        self.follow_chain(self.now());
        self.follow_offload(policy.offload);
        if replan {
            self.replan();
        }
    }

    /// Applies the chain settings asked for, while music plays at most every [`CHAIN_EVERY_MS`].
    fn follow_chain(&mut self, now: i64) {
        if self.chain_wanted.is_none() || (self.p.playing() && now < self.chain_at + CHAIN_EVERY_MS) {
            return;
        }
        let c = self.chain_wanted.take().expect("checked");
        if *self.p.sink.settings() != c {
            self.p.set_chain(c);
            self.chain_at = now;
        }
    }

    /// Plans the transition out of the current song again; on the CPU an ending already made otherwise
    /// is made again ([`Player::replan_ending`]).
    fn replan(&mut self) {
        if self.offloading() {
            return self.p.engine.replan();
        }
        self.p.now_ms = self.now();
        self.p.replan_ending();
    }

    /// Offload became allowed or not. Leaving is where the ear is; entering too, once the current song
    /// is known to be decodable there (else at the next song that is).
    fn follow_offload(&mut self, wanted: bool) {
        let was = std::mem::replace(&mut self.offload, wanted);
        self.status.lock().offload_wanted = wanted;
        if was == wanted {
            return;
        }
        (self.h.probe, self.h.entering, self.h.takeover) = (None, None, None);
        if !wanted {
            if self.h.at_end.take().is_some() {
                self.p.pause_at_end(false);
            }
            if self.offloading() {
                self.leave_chip();
            }
        } else if !self.p.playing() && self.p.current().is_some() && self.parked.as_ref().is_none_or(|p| p.shown.is_none()) {
            // Paused on the CPU: let go, so play opens the song where the output decodes it.
            self.let_go();
        } else if let Some(i) = self.p.current().filter(|_| self.p.playing() && !self.offloading()) {
            self.h.entering = Some((i, self.p.tracks.open_packets(&self.p.id_at(i), 0, true)));
        }
    }

    /// Hands the offloaded song to the CPU. While the track can play on (playing, no USB, not refused),
    /// the CPU opens the song ahead first so the handover is a dip, not a gap.
    fn leave_chip(&mut self) {
        if !(self.going() && !self.facts.usb && !self.h.refused && self.leave_ahead()) {
            self.leave_now();
        }
    }

    /// Opens the offloaded song on the CPU [`REMAKE_LEAD_MS`] ahead of the track; the CPU takes over
    /// there once it is open ([`Worker::follow_takeover`]).
    fn leave_ahead(&mut self) -> bool {
        let Some((i, ms, _)) = self.off.as_mut().and_then(Offload::heard) else { return false };
        let id = self.p.id_at(i);
        let length = self.p.tracks.about(&id).duration_ms;
        let from_ms = ms + REMAKE_LEAD_MS;
        if length <= 0 || from_ms + REMAKE_LEAD_MS >= length {
            return false;
        }
        let Ok(r) = self.p.tracks.open(&id, from_ms) else { return false };
        let dip_at = self.now() + REMAKE_LEAD_MS - HAND_DIP_MS;
        self.h.takeover = Some(Takeover { id, from_ms, r, ready: false, dip_at });
        self.p.app.log("offload given up: the CPU takes over once the song is open");
        // Looked at now: this turn may be past its own look.
        self.follow_takeover(self.now());
        true
    }

    /// The dip under way, or a new one fading down now over `down_ms`.
    fn dip_down(&mut self, now: i64, down_ms: i64, up_ms: i64) -> &mut Dip {
        if self.dip.is_none() {
            self.ramp(None, 0.0, down_ms);
            self.dip = Some(Dip { at: now + down_ms, up_ms, then: Vec::new() });
        }
        self.dip.as_mut().expect("set above")
    }

    /// Once the song opened ahead is open and the track a dip away from its start, the dip goes down and
    /// the CPU takes over at its bottom ([`Worker::handed`]). Paused, the handover is made at once.
    fn follow_takeover(&mut self, now: i64) {
        if self.h.takeover.is_none() || self.dip.as_ref().is_some_and(|d| d.then.iter().any(|s| matches!(s, Switched::Hand))) || self.pause_at.is_some() {
            return;
        }
        if !self.offloading() {
            self.h.takeover = None;
            return;
        }
        let playing = self.playing();
        let t = self.h.takeover.as_mut().expect("checked");
        let failed = playing && !t.ready && {
            t.ready = t.r.ready();
            if !t.ready {
                // Its loader wakes the thread.
                return;
            }
            t.r.error().is_some()
        };
        if failed || !playing {
            self.h.takeover = None;
            self.leave_now();
        } else if self.dip.is_none() && now >= t.dip_at {
            self.dip_down(now, HAND_DIP_MS, HAND_DIP_MS).then.push(Switched::Hand);
        }
    }

    /// Plays from where the player is, fading in if the settings say so.
    fn resume(&mut self) {
        self.go_on();
        match self.settings.fade_ms {
            ms if ms > 0 => self.ramp(Some(0.0), 1.0, ms as i64),
            _ => self.ramp(None, 1.0, 0),
        }
        self.set_state(State::Playing);
    }

    fn play(&mut self) {
        let chosen = self.parked.as_ref().is_some_and(|p| p.shown.is_some());
        if self.pause_at.take().is_some() && !chosen {
            // Play during the pause's fade: back up from where the fade got to.
            self.ramp(None, 1.0, self.settings.fade_ms.max(0) as i64);
            return self.set_state(State::Playing);
        }
        // A place chosen during the fade (a skip, a new queue) is played below.
        if self.playing() && self.state == State::Playing {
            return;
        }
        if let Some(p) = self.parked.take_if(|p| p.shown.is_some()) {
            self.jump(p.at, p.ms);
            return self.resume();
        }
        self.reopen();
        if self.offloading() {
            if self.state == State::Ended {
                self.jump(self.p.queue.read(|q| q.current()).unwrap_or(0), 0);
            }
        } else if let Some(i) = self.p.stopped_at() {
            // Stopped at a failing song: try it again.
            self.jump(i, 0);
        } else if self.p.current().is_none() || self.state == State::Ended {
            if self.p.queue.read(|q| q.is_empty()) {
                return;
            }
            self.jump(self.p.queue.read(|q| q.current()).or(self.p.current()).unwrap_or(0), 0);
        }
        self.resume();
    }

    fn pause(&mut self, now: i64, fade_ms: i32) {
        if self.pause_at.is_some() && fade_ms <= 0 {
            // At once, during a fade.
            self.pause_at = None;
            return self.halt();
        }
        if !self.playing() || self.pause_at.is_some() {
            return;
        }
        // A pause never swallows the switch it interrupts.
        self.end_dip();
        if fade_ms > 0 {
            self.ramp(None, 0.0, fade_ms as i64);
            self.pause_at = Some(now + fade_ms as i64);
        } else {
            self.halt();
        }
        self.set_state(State::Paused);
    }

    /// A jump or seek that keeps playing or paused: made behind its dip while playing, parked while
    /// paused. After the end of the queue it plays, as the music did before it ended.
    fn go(&mut self, s: Switched, kind: Switch, now: i64) {
        if self.playing() && self.pause_at.is_none() {
            self.switch(s, kind, now);
        } else {
            self.hold(s);
            if self.state == State::Ended {
                self.play();
            }
        }
    }

    /// Parks the place `s` leads to, from the place chosen before or the player's.
    fn hold(&mut self, s: Switched) {
        let len = self.p.queue.read(|q| q.len());
        if len == 0 {
            return;
        }
        let (at, ms) = match self.parked.as_ref().filter(|p| p.shown.is_some()) {
            Some(p) => (p.at, p.ms),
            None => match self.shown() {
                Some(place) => place,
                None => (self.p.queue.read(|q| q.current()).unwrap_or(0), self.position_ms()),
            },
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
                (if previous_restarts(ms, before.is_some(), false) { at } else { before.unwrap_or(at) }, 0)
            }
            // Paused, the music is made again on play.
            Switched::Hand => return,
        };
        self.parked = Some(Parked { at: i, ms, shown: Some(self.p.id_at(i)) });
        self.told.seek_landed |= matches!(s, Switched::Seek(_));
        // The queue moves now, so a queue saved while paused restores this song.
        if !matches!(s, Switched::Seek(_)) {
            self.p.queue.moved_to(i);
        }
    }

    /// The queue was edited: a parked place follows its entry, or goes with it; a jump waiting in the
    /// dip follows its entry, or goes to what took its place.
    fn follow_parked(&mut self) {
        let now_at = |seqs: &[u64], q: &Q, i: usize| seqs.get(i).and_then(|&s| q.read(|q| q.index_of(s)));
        if let Some(d) = self.dip.as_mut() {
            for s in &mut d.then {
                if let Switched::To(i, _) = s {
                    *i = now_at(&self.seqs, &self.p.queue, *i).unwrap_or(*i);
                }
            }
        }
        let Some(p) = self.parked.as_mut() else { return };
        match now_at(&self.seqs, &self.p.queue, p.at) {
            Some(k) => p.at = k,
            None => self.parked = None,
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

    /// Makes one switch. Returns the level the music comes back up from when not where the fade left it.
    fn run_switch(&mut self, s: Switched) -> Option<f32> {
        let mut from = None;
        match s {
            Switched::To(i, ms) if i < self.p.queue.read(|q| q.len()) => self.jump(i, ms),
            Switched::Next => {
                if let Some(n) = self.p.queue.read(Playlist::next) {
                    self.jump(n, 0);
                }
            }
            Switched::Previous => {
                let has_previous = self.p.queue.read(|q| q.previous().is_some());
                if previous_restarts(self.position_ms(), has_previous, false) {
                    self.seek(0);
                } else if let Some(n) = self.p.queue.read(Playlist::previous) {
                    self.jump(n, 0);
                }
            }
            Switched::Seek(ms) => {
                self.seek(ms);
                self.told.seek_landed = true;
            }
            Switched::Hand => from = self.handed(),
            Switched::To(..) => {}
        }
        // Only a play_at comes here paused (a skip or a go_to is parked instead): music is wanted.
        if s.jumps() && !self.playing() && self.current().is_some() {
            self.resume();
        }
        from
    }

    /// At the dip's bottom: the CPU takes the song over from the output's decoder, or that takes it over
    /// where the ear is. Returns the level to come back up from, if not the fade's.
    fn handed(&mut self) -> Option<f32> {
        if let Some(t) = self.h.takeover.take() {
            // The track's fade ends at silence, whatever tick it last took.
            self.ramp(None, 0.0, 0);
            let now = self.now();
            let (i, ms) = self.off.as_mut().and_then(|o| o.leave(now))?;
            self.p.app.log("offload given up: the CPU plays on from here");
            let opened = t.ready.then_some((t.id, t.r, t.from_ms));
            return self.onto_cpu(i, ms, opened);
        }
        let i = self.p.current().filter(|_| !self.offloading())?;
        if !(std::mem::take(&mut self.h.now) && self.offload && self.off.is_some() && self.p.playing()) {
            return None;
        }
        // Where the ear is, read as the CPU stops.
        self.p.pause();
        let ms = self.p.position_ms();
        self.jump(i, ms);
        self.told.placed = true;
        Some(0.0)
    }

    /// Leaves the output's decoder at once, where the ear is.
    fn leave_now(&mut self) {
        let now = self.now();
        if let Some((i, ms)) = self.off.as_mut().and_then(|o| o.leave(now)) {
            self.p.app.log("offload given up: the CPU plays on from here");
            self.onto_cpu(i, ms, None);
        }
    }

    /// A seek in the current song; offloaded, the track restarts at the packet it lands in.
    fn seek(&mut self, ms: i64) {
        self.h.takeover = None;
        match self.chip().and_then(Offload::current) {
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
        self.follow_chain(now);
        self.follow_takeover(now);
    }

    /// A ReplayGain settings change: heard from what the output can still replace on the CPU, or on the
    /// offloaded track's volume.
    fn follow_gain(&mut self) {
        if !std::mem::take(&mut self.gain_changed) {
            return;
        }
        if !self.offloading() {
            return self.p.gain_changed();
        }
        let level = self.current().map_or(1.0, |i| self.gain_of(i));
        let off = self.off.as_mut().expect("offloading");
        if nori_player::gain::offload_allows(level) {
            return off.set_level(level);
        }
        // Turned up: that needs the samples.
        off.on_cpu = Some(OnCpu::TurnedUp);
        self.leave_chip();
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
            // Let go as after a long pause: the next play opens a new device.
            self.let_go();
        }
        for (id, message) in std::mem::take(&mut self.p.failures) {
            (self.events)(Event::Error { id, message });
        }
        self.p.changes.clear();
        if self.p.source_ended() {
            self.p.sink.track.set_ended(true);
        }
        let ended = self.p.playing() && self.p.ended();
        if ended && self.h.at_end.is_some() && self.p.stopping_after() == self.h.at_end {
            // The output's decoder takes the next song from its start.
            let i = self.h.at_end.take().expect("checked");
            self.p.pause_at_end(false);
            match self.p.queue.read(|q| q.next_of(i, q.repeat())) {
                Some(n) => self.jump(n, 0),
                None => {
                    self.p.pause();
                    self.set_state(State::Ended);
                }
            }
        } else if let Some(i) = self.p.stopping_after().filter(|_| ended) {
            self.p.pause_at_end(false);
            self.p.pause();
            self.stopped_after(i);
        } else if ended {
            self.p.pause();
            self.set_state(State::Ended);
        } else if !self.p.playing() && self.state == State::Playing && self.pause_at.is_none() && self.dip.is_none() {
            // The queue's rules stopped playback (a run of songs that would not play), or the bridge takes over.
            if std::mem::take(&mut self.p.bridge) {
                (self.events)(Event::Bridge { plays: self.plays });
            } else {
                (self.events)(Event::Stopped { plays: self.plays });
            }
            self.set_state(State::Paused);
        }
    }

    /// Played to the end of song `i`, where the sleep timer stops: paused on the next song's start
    /// (fetched on play), or ended.
    fn stopped_after(&mut self, i: usize) {
        let next = self.p.queue.read(|q| q.next_of(i, q.repeat()));
        if let Some(n) = next {
            self.parked = Some(Parked { at: n, ms: 0, shown: Some(self.p.id_at(n)) });
        }
        (self.events)(Event::Stopped { plays: self.plays });
        self.set_state(if next.is_some() { State::Paused } else { State::Ended });
    }

    /// Everything written to the offload track was heard: the end of the queue (or of the sleep timer's
    /// song), or a next song that needs another track or the CPU.
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
        if self.told.title.as_ref().is_some_and(|t| now >= t.1) {
            let (t, _) = self.told.title.take().expect("checked");
            (self.events)(Event::Title(t));
        }
        let Some(i) = self.p.reading_index().filter(|_| !self.offloading()) else { return };
        let fresh = self.p.queue.read(|q| q.ids().get(i).and_then(|id| self.p.tracks.loading(id)).and_then(|l| l.announced()));
        if let Some(t) = fresh {
            let track = &self.p.sink.track;
            self.told.title = Some((t, now + (track.filled_us() + track.latency_us()) / 1000));
        }
    }

    /// Tells a watching client what this wake saw ([`crate::watch`]).
    fn watch(&mut self, now: i64) {
        crate::watch::look(self.watch.as_ref(), || {
            let offloaded = self.offloading();
            let in_output_ms = match self.chip() {
                Some(o) => o.in_track_us() / 1000,
                None => (self.p.sink.track.filled_us() + self.p.sink.track.latency_us()) / 1000,
            };
            let waiting = self.told.stalled || self.waiting_for_bytes();
            let state = self.words(in_output_ms);
            let quiet_ms = self.stall.as_ref().filter(|q| q.standing).map_or(0, |q| now - q.since);
            let bytes_coming = self.bytes_coming();
            let output_open = if offloaded { self.off.as_ref().is_some_and(|o| o.track().is_some()) } else { self.p.sink.track.opened() };
            let s = self.status.lock();
            let playing = s.state == State::Playing && !s.switching && !waiting;
            crate::watch::Seen { now_ms: now, playing, offloaded, index: s.index, id: s.id.clone(), position_ms: s.position_ms, in_output_ms, quiet_ms, bytes_coming, output_open, state }
        });
    }

    /// The engine's state in words, for a stall report.
    fn words(&self, in_output_ms: i64) -> String {
        let parked = self.parked.as_ref().map(|p| if p.shown.is_some() { ", a place chosen" } else { ", output let go" });
        let marks = [(self.dip.is_some(), ", switching"), (self.pause_at.is_some(), ", pausing"), (self.offloading(), ", offloaded")];
        let mut w = format!("{:?}", self.state);
        marks.iter().filter(|m| m.0).for_each(|m| w.push_str(m.1));
        w.push_str(parked.unwrap_or_default());
        w.push_str(&format!("; {}; ring {} ms, output {in_output_ms} ms", self.p.words(), self.p.sink.track.filled_us() / 1000));
        if self.waiting_for_bytes() {
            w.push_str(", waiting for a song's bytes");
        }
        if self.told.stalled {
            w.push_str(", said to be buffering");
        }
        w.push_str(&format!("; loaders: {}", self.p.tracks.words()));
        w
    }

    /// Keeps [`Status::pcm_why`] current, logging each change.
    fn follow_why(&mut self) {
        let offloaded = self.offloading();
        let why = match self.chip() {
            Some(o) => o.gapped.clone(),
            None => self.current().is_some().then(|| self.why_on_cpu()),
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
            _ if self.h.at_end.is_some() => "offload takes over at the next song".into(),
            _ => self.off.as_ref().and_then(|o| o.on_cpu.as_ref()).map_or_else(|| "the song began on the CPU before offload was wanted".into(), OnCpu::words),
        }
    }

    /// Ends a [`Event::Buffering`]: the CPU path no longer waits for bytes.
    fn unstall(&mut self) {
        if std::mem::take(&mut self.told.stalled) {
            (self.events)(Event::Buffering(false));
        }
    }

    /// Whether song `i` (with `id`) is another than the one last reported. With no jump pending, a new
    /// queue can put another song at the same index.
    fn other(&self, i: usize, id: Option<&str>) -> bool {
        match &self.told.heard {
            Some((h, heard_id)) if *h == i => match id {
                Some(id) => id != heard_id,
                None => self.dip.as_ref().is_none_or(|d| !d.then.iter().any(Switched::jumps)) && self.p.queue.read(|q| q.ids().get(i).is_none_or(|s| s != heard_id)),
            },
            _ => true,
        }
    }

    /// Writes the status, then says what changed: a client reading the status on an event finds the
    /// event's song and state there.
    fn report(&mut self, now: i64) {
        if let Some(p) = self.parked.as_ref().map(|p| (p.at, p.ms, p.shown.clone())) {
            self.unstall();
            let (i, ms, Some(id)) = p else {
                // Let go: the place last reported stands.
                let mut s = self.status.lock();
                s.releases = self.releases;
                s.offloaded = false;
                s.on_cpu = false;
                return;
            };
            // A place chosen while paused is where the player is, to the screen.
            let other = self.other(i, Some(&id));
            if other {
                self.told.heard = Some((i, id));
            }
            self.write_status(i, ms, |s| {
                s.switching = false;
                s.on_cpu = false;
            });
            if other {
                self.say_song(i, false);
            }
            return self.say_position(now, i, ms);
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
        let stall_changed = std::mem::replace(&mut self.told.stalled, stalled) != stalled;
        let seen = self.p.bar();
        let Some(i) = seen.index.or(self.p.current()) else { return };
        let ms = if seen.index.is_some() { seen.ms } else { self.p.position_ms() };
        let other = self.other(i, None);
        if other {
            self.told.heard = Some((i, self.p.id_at(i)));
        }
        let looped = !other && self.p.loops != self.told.loops;
        self.told.loops = self.p.loops;
        let mixing = self.p.mixing();
        // The place moves at the speed times the tempo a mix brings the song in at.
        let tempo = self.p.sink.pace_heard();
        let stretched = (tempo - 1.0).abs() > 1e-3;
        self.told.placed |= self.told.stretched && !stretched;
        self.told.stretched = stretched;
        let (speed, underruns, switching) = (self.p.speed().0, self.p.sink.track.underruns(), self.dip.is_some());
        let (chain, meter, compression) = (self.p.sink.chain_in(), self.p.sink.meter_db(), self.p.sink.compression_db());
        let on_cpu = self.state == State::Playing && !stalled && !switching;
        let mixing_was = self.write_status(i, ms, |s| {
            s.speed = speed;
            s.pace = speed * tempo as f32;
            s.mixing = mixing;
            s.underruns = underruns;
            s.switching = switching;
            s.chain = chain;
            s.on_cpu = on_cpu;
            s.gain_reduction_db = meter;
            s.compression_db = compression;
            s.offloaded = false;
        });
        if stall_changed {
            (self.events)(Event::Buffering(stalled));
        }
        self.say_changes(now, i, ms, other, looped, mixing_was, mixing);
    }

    /// [`Worker::report`] while the songs go to the output's decoder: the queue follows the song heard,
    /// the one after it is fetched, and a song placed again (repeat one) is a loop.
    fn report_offload(&mut self, now: i64) {
        let Some((i, ms, seq)) = self.off.as_mut().and_then(Offload::heard) else { return };
        let other = self.other(i, None);
        let looped = !other && seq != self.h.seq && self.h.seq != 0;
        self.h.seq = seq;
        if other {
            self.told.heard = Some((i, self.p.id_at(i)));
            self.p.queue.moved_to(i);
            if let Some(n) = self.p.queue.read(|q| q.next_of(i, q.repeat())) {
                let next = self.p.id_at(n);
                self.p.tracks.upcoming(&next);
            }
        }
        if !self.h.heard && ms > 0 && self.state == State::Playing {
            // Music is heard: a run of songs that would not play is broken.
            self.h.heard = true;
            self.p.errors.played();
            self.p.app.playing();
        }
        let switching = self.dip.is_some();
        let mixing_was = self.write_status(i, ms, |s| {
            s.speed = 1.0;
            s.pace = 1.0;
            s.mixing = false;
            s.switching = switching;
            s.chain = false;
            s.on_cpu = false;
            s.gain_reduction_db = 0.0;
            s.compression_db = 0.0;
            s.offloaded = true;
        });
        self.say_changes(now, i, ms, other, looped, mixing_was, false);
    }

    /// Writes the song, place, state and releases into the status, then `more`; returns whether a mix
    /// was audible before.
    fn write_status(&mut self, i: usize, ms: i64, more: impl FnOnce(&mut Status)) -> bool {
        let id = self.told.heard.as_ref().map(|h| h.1.clone());
        let mut s = self.status.lock();
        let was = s.mixing;
        s.state = self.state;
        if s.index != Some(i) || s.id != id {
            s.index = Some(i);
            s.id = id;
        }
        s.position_ms = ms;
        s.at = Instant::now();
        s.releases = self.releases;
        more(&mut s);
        was
    }

    fn say_song(&mut self, i: usize, looped: bool) {
        let (id, jumps, seq) = (self.told.heard.as_ref().map(|h| h.1.clone()).unwrap_or_default(), self.made(), self.seqs.get(i).copied());
        (self.events)(if looped { Event::Looped { index: i, seq, id, jumps } } else { Event::Song { index: i, seq, id, jumps } });
    }

    #[allow(clippy::too_many_arguments)]
    fn say_changes(&mut self, now: i64, i: usize, ms: i64, other: bool, looped: bool, mixing_was: bool, mixing: bool) {
        if other || looped {
            self.say_song(i, looped);
        }
        if mixing != mixing_was {
            (self.events)(Event::Mixing(mixing));
        }
        if std::mem::take(&mut self.told.placed) {
            (self.events)(Event::Placed { index: i, ms });
        }
        self.say_position(now, i, ms);
    }

    /// [`Event::Position`]: at the pace asked for while playing, and once when a seek or jump lands.
    fn say_position(&mut self, now: i64, index: usize, ms: i64) {
        let jumps = self.made();
        let landed = std::mem::take(&mut self.told.seek_landed) || jumps != self.told.landed_jumps;
        let due = self.told.positions.is_some() && self.state == State::Playing && now >= self.told.next_position;
        if due {
            self.told.next_position = now + self.told.positions.expect("checked");
        }
        if landed || due {
            self.told.landed_jumps = jumps;
            (self.events)(Event::Position { index, ms, jumps });
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
        [d, look].into_iter().flatten().min()
    }

    fn wake_for_music(&self, now: i64) -> Option<i64> {
        let mut d: Option<i64> = None;
        let mut at = |ms: i64| d = Some(d.map_or(ms, |x| x.min(ms)));
        for t in [self.pause_at, self.dip.as_ref().map(|d| d.at), self.idle_at, self.told.title.as_ref().map(|t| t.1)].into_iter().flatten() {
            at(t - now);
        }
        if self.chain_wanted.is_some() {
            at(self.chain_at + CHAIN_EVERY_MS - now);
        }
        // Loaders wake the thread for openings; these are fallbacks.
        if self.h.entering.is_some() {
            at(1_000);
        }
        if let Some(m) = self.h.takeover.as_ref().filter(|_| self.dip.is_none() && self.pause_at.is_none()) {
            at(if m.ready { m.dip_at - now } else { 1_000 });
        }
        let positions = self.told.positions.is_some() && self.state == State::Playing;
        if let Some(off) = self.chip() {
            if off.unlooked() {
                return Some(0);
            }
            if let Some(ms) = off.wake_in() {
                at(ms);
            }
            if positions {
                at(self.told.next_position - now);
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
        // With no screen watching, only a change of song needs saying on time (the notification, a car).
        for u in [self.p.until_next_song_us().map(|u| (u, 5)), self.p.until_heard_changes_us(!positions).map(|u| (u, 1))].into_iter().flatten() {
            at((u.0 as f64 / speed / 1000.0) as i64 + u.1);
        }
        if self.h.probe.as_ref().is_some_and(|p| p.2.is_none()) {
            at(1_000);
        }
        if positions {
            at(self.told.next_position - now);
        }
        d.map(|x| x.max(1))
    }
}
