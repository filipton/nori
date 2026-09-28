//! The player's one thread, and the handle a client drives it with. Everything that decides how music
//! plays is the shared pipeline (`nori_player::pipeline`) the tests run on a virtual clock; this adds
//! the real clock, the controls' fades and the events a screen follows.
//!
//! The thread sleeps whenever it has nothing to do, and wakes only for:
//! - a command (a control, a settings change, the queue changed): the handle unparks it;
//! - the ring running down to its low mark (about 1.75 s left of a 10 s burst): the device's pull
//!   unparks it, once per burst, so while music plays it wakes every eight seconds or so (and at the
//!   end of the queue, when the ring has run empty); while the equalizer is tuned the ring is kept at a
//!   fraction of a second and the pull wakes it at half of that;
//! - a song's bytes arriving while it waited for them: the loader unparks it;
//! - a moment it has to act at: a fade's end (pause, a seek's dip, the music made again), the next song
//!   reaching the ear (so the song event comes on time), every 250 ms through a held ending or a mix
//!   (the ear moves into the next song there), the music running out at the end of the queue, and
//!   position events when a screen asked for them. Each is one timed sleep, computed, never a ticking
//!   timer.
//!
//! Paused, stopped or at the end of the queue, it sleeps until a command comes, and the device is
//! paused so it can sleep too. Paused long enough (the core's idle release, five minutes on Android),
//! it wakes once more to let the device and the song's bytes go; play opens them again where it was.
//!
//! With an output that decodes compressed songs itself (audio offload, `offload.rs`) and nothing in the
//! chain that would change a sample, the songs go there as packets instead, and the thread sleeps
//! minutes between top-ups. Offload is taken up where the ear is as soon as nothing needs the samples,
//! behind a dip (a song the output does not decode plays to its end on the CPU, and the next one that it
//! does is handed over at its start), and given up at once when something needs the samples, as media3
//! rebuilds then.
//!
//! A change to the sound while the CPU plays (the equalizer, the limiter, speed, silence skipping, high
//! quality output, the equalizer screen's shallow buffer) would otherwise be heard only once the seconds
//! the ring and the device hold have played: what they hold is made again from where the ear is, behind
//! a 30 ms dip, changes that come quickly taken together, one every 150 ms at most. The song is opened
//! for it a moment ahead of the ear first ([`REMAKE_LEAD_MS`]), while the output plays on, and the output
//! is emptied only once it is open: opening a song again (its bytes fetched again, a seek through a long
//! file) takes its time, and done after the output was emptied it was a silence as long. Coming off the
//! output's decoder is the same: the chip plays on until the CPU has the song open where it will be.

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

/// What the settings ask of the sound, as the engine applies it.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub sound: Sound,
    pub speed: f32,
    pub pitch: f32,
    pub skip_silence: bool,
    /// The fade on play, pause and switches, ms (0 off).
    pub fade_ms: i32,
    /// High quality output: songs decoded to float, 24 bits kept, the sound chain run on the floats and
    /// what it makes taken to a device that plays float (`nori_player::policy`). A device that takes
    /// 16-bit only gets the 16-bit chain, dithered.
    pub hi_res: bool,
    /// The highest rate the device is opened at, Hz (0: a song's own): a song above it is converted down
    /// within its family (`nori_player::policy::capped_rate`). Bit-perfect output is not held to it.
    pub max_rate: u32,
    /// Let an output that decodes songs itself have them, when nothing needs the samples.
    pub offload: bool,
    /// The crossfade (s, 0 off) and AutoMix: transitions touch the samples, so offload stands down.
    pub crossfade_s: i32,
    pub auto_mix: bool,
    /// The most ReplayGain turns a song up, dB (`nori_player::gain`); 0 while it only turns songs down
    /// or is off. Over 0, wherever the samples may be touched, songs are read as floats and the limiter
    /// runs behind them, so a song turned up cannot clip; such a song keeps off the output's decoder.
    pub gain_boost_db: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { sound: Sound::default(), speed: 1.0, pitch: 1.0, skip_silence: false, fade_ms: 0, hi_res: false, max_rate: 0, offload: false, crossfade_s: 0, auto_mix: false, gain_boost_db: 0.0 }
    }
}

/// What the platform knows of the output that the engine cannot see: a USB device attached (the chip
/// has no path to it, so offload stands down), and a DAC playing bit-perfect (nothing may touch the
/// samples: no chain, no ReplayGain, no conversion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputFacts {
    pub usb: bool,
    pub bit_perfect: bool,
}

/// The settings as the policy let them through to the player.
#[derive(Clone, PartialEq)]
struct Applied {
    sound: Sound,
    speed: (f32, f32),
    skip_silence: bool,
    untouched: bool,
    bit_perfect: bool,
    float: bool,
    /// The most a song is turned up, linear (1: not at all).
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

/// What a screen hears from the engine. Each comes once, when it changes; positions only when asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    State(State),
    /// The ear moved to another song: through a mix, the moment the next song is audible. `jumps` is
    /// how many of the jumps asked for ([`Engine::play_at`], [`Engine::go_to`], [`Engine::next`],
    /// [`Engine::previous`], each of which answers with its own number) the engine had made when it
    /// said this: a client that has asked for a later one since, and already shows it, knows the event
    /// is from before that jump and not a song ending by itself.
    Song { index: usize, id: String, jumps: u64 },
    /// The song playing started again by itself (repeat one): a play of its own for a scrobbler.
    Looped { index: usize, id: String, jumps: u64 },
    /// Where the ear is, at the pace asked for with [`Engine::position_updates`].
    Position { index: usize, ms: i64 },
    /// A song would not play (it is skipped, or playback stops, as the queue's rules say), or the
    /// output would not open (`id` empty).
    Error { id: String, message: String },
    /// The music goes to another output device now, by the name the core keeps devices under.
    Output { name: String },
    /// The music ran out while a song's bytes are on their way (a slow network): `true` when the ear
    /// starts hearing nothing for it, `false` when the music goes on. Nothing is said while the output
    /// still holds music to play through the wait.
    Buffering(bool),
    /// Playback stopped by itself rather than because it was asked to (the queue's rules after a run
    /// of songs that would not play): the `Paused` state that follows is the engine's, and a client that
    /// keeps its own "wants to play" lets it go. A pause that was asked for comes without this. `plays`
    /// is how many plays asked for ([`Engine::play`]) the engine had taken when it stopped: a client
    /// that has asked for one since reads the stop as over ([`Engine::superseded`]), since that play
    /// starts the music again (the song that stopped it tried once more).
    Stopped { plays: u64 },
    /// A live stream's station announced what it plays now (ICY), as the ear reaches it.
    Title(String),
    /// A mix (AutoMix, a crossfade) began to be heard (`true`), or is over (`false`): said as the engine
    /// wakes to feed the output, which it does four times a second through a mix, so a screen can say so
    /// without asking.
    Mixing(bool),
    /// A song would not play for want of the network, and the app's offline bridge is to take over (the
    /// queue's rules said so): playback waits there, paused, for the bridge's jump. `plays` as for
    /// [`Event::Stopped`]: a play asked for since has tried the song again, and the bridge is not wanted.
    Bridge { plays: u64 },
    /// The song heard went from the output's decoder to the CPU or back (the settings changed, or the
    /// chip's track failed), nobody having asked for a jump: the place is `ms` in queue index `index` now,
    /// said once the status has it. A client that runs its own clock on from the engine's last word
    /// (media3's controllers do, until a play, pause or seek says the place again) takes it from here.
    Placed { index: usize, ms: i64 },
    /// Whether the engine needs the CPU kept awake while music plays (`true`, as it starts), or can let
    /// it sleep (`false`): the songs are offloaded, the track is fed, the platform says when it wants
    /// more, and nothing else is due before its next word (`Offload::lets_cpu_sleep`). Said on the
    /// engine's thread as it changes, before the engine goes on: a platform that keeps the CPU awake
    /// with a lock (Android's wake lock) takes it at `true` before the work it is for, and lets it go at
    /// `false`. A platform's wake of the engine (the offloaded track asking for more) is its own.
    Awake(bool),
}

/// The engine as it last looked, for a screen to read at any time without waking it.
#[derive(Debug, Clone)]
pub struct Status {
    pub state: State,
    /// The song heard, as a queue (list) index, and its id.
    pub index: Option<usize>,
    pub id: Option<String>,
    /// Where the ear was in it when `at` was taken, ms.
    pub position_ms: i64,
    pub at: Instant,
    pub speed: f32,
    /// How fast the place moves, in the song's time per the listener's: the playing speed, times the
    /// tempo a mix brings the song heard in at (1.07 through a beat-matched mix into a faster song,
    /// easing back to 1 after it). A place run on between readings runs on at this.
    pub pace: f32,
    pub mixing: bool,
    /// Pulls that found the output's buffer short while music was due: a gap each.
    pub underruns: u64,
    /// Times the output was let go after a long pause.
    pub releases: u64,
    /// A jump, skip or seek is waiting out its dip: the place is still the one before it.
    pub switching: bool,
    /// The sound chain is in the samples' path, and what its limiter took off the last buffer, dB.
    pub chain: bool,
    /// The ear is on music the CPU made, playing through the engine's own output: the only time `chain`
    /// says anything. Not while offloaded, let go, paused, or waiting for a song's bytes.
    pub on_cpu: bool,
    pub gain_reduction_db: f32,
    /// What the compressor took off the last buffer, dB (`Sink::compression_db`).
    pub compression_db: f32,
    /// The songs go to the output's own decoder (audio offload).
    pub offloaded: bool,
    /// The settings let offload be used, as last applied: what the output is asked for.
    pub offload_wanted: bool,
    /// Why the music is played on the CPU and not by the output's own decoder, in words for a report:
    /// the setting that keeps offload off, or what came of offering the song to the output. None with
    /// nothing loaded, and while offloaded, but for a song offloaded with the encoder's delay and padding
    /// left in (an output without gapless offload, and no song of its album joining it), which says so.
    pub pcm_why: Option<String>,
    /// The engine needs the CPU kept awake, as last said ([`Event::Awake`]).
    pub awake: bool,
}

impl Status {
    /// The place in the song now: the last reading, moved on at the pace it moves at.
    pub fn position_now(&self) -> i64 {
        match self.state {
            State::Playing => self.position_ms + (self.at.elapsed().as_secs_f64() * 1000.0 * self.pace as f64) as i64,
            _ => self.position_ms,
        }
    }

    /// The place for a seek bar on screen: the last reading run on at its pace for at most
    /// `nori_player::heard::RUN_ON_MS`, and whether it is old enough that the engine is to be asked to
    /// look again ([`Engine::look`]); see `nori_player::heard::screen_place`.
    pub fn screen_now(&self) -> (i64, bool) {
        nori_player::heard::screen_place(self.position_ms, self.at.elapsed().as_millis() as i64, self.pace, self.state == State::Playing)
    }
}

/// How the engine is set up.
#[derive(Debug, Clone)]
pub struct Config {
    /// The memory the platform gives the app, MB: it sizes how much of a song is kept loaded
    /// (`nori_player::transport::load_control`).
    pub memory_mb: u32,
    pub settings: Settings,
    /// Paused this long, the output and the song's bytes are let go, ms
    /// (`nori_player::transport::IDLE_RELEASE_MS`).
    pub idle_release_ms: i64,
    /// Told what each wake of the engine's thread saw, for a client keeping watch ([`crate::watch`]).
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
    /// Pause, fading out over this long (`None`: the user's fade on pause).
    Pause(Option<i32>),
    Toggle,
    Next,
    Previous,
    Seek(i64),
    Settings(Settings),
    Output(OutputFacts),
    Replan,
    QueueChanged,
    Repeat(u8),
    Gain,
    Tuning(bool),
    Positions(Option<Duration>),
    /// Nothing to do but wake: the turn reads the output and the status is said again.
    Look,
    Device(Device),
    Stop,
}

/// What a switch does once its dip is down.
#[derive(Debug, Clone, Copy)]
enum Switched {
    To(usize, i64),
    Next,
    Previous,
    Seek(i64),
    /// The music made again from where the ear is ([`Worker::resound_soon`]), or handed to the output's
    /// decoder there.
    Resound,
}

/// The handle: every call sends a command and wakes the engine's thread, and returns at once.
pub struct Engine {
    tx: Sender<Command>,
    thread: Thread,
    join: Mutex<Option<JoinHandle<()>>>,
    status: Arc<Mutex<Status>>,
    /// The jumps asked for so far (see [`Event::Song`]).
    jumps: AtomicU64,
    /// The plays asked for so far (see [`Event::Stopped`]).
    plays: AtomicU64,
    /// How a command wakes the thread, when the clock has a say in it (a test's, [`Engine::start_on`]).
    wake: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Engine {
    /// Starts the engine's thread over `library`'s songs, `queue` and `app` (the transition planner
    /// and log), playing through `output`, and through `offload` (an output that decodes compressed songs
    /// itself) whenever the settings and the output let them (`Settings::offload`). `events` is called on
    /// the engine's thread; it should only hand the event on.
    pub fn start<L, A, Q, E>(library: L, app: A, queue: Q, output: Box<dyn AudioOutput>, offload: Option<Box<dyn OffloadOutput>>, config: Config, events: E) -> Engine
    where
        L: Library,
        A: App + Send + 'static,
        Q: Queue + Send + 'static,
        E: FnMut(Event) + Send + 'static,
    {
        Engine::launch(library, app, queue, output, offload, config, Monotonic::new(), false, events)
    }

    /// [`Engine::start`] on `clock` rather than the machine's: for a test that moves the time by
    /// hand, and has every command go through [`Clock::wake`].
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
                // The output says where the music goes whenever that changes, on a thread of its own.
                let wake = me.clone();
                output.watch(Box::new(move |d| {
                    if devices.send(Command::Device(d)).is_ok() {
                        own.wake(&wake);
                    }
                }));
                let songs = Sources::new(library, load_control(config.memory_mb), me);
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

    /// A jump sent: its number, which the song events it leads to carry ([`Event::Song`]'s `jumps`).
    fn jump(&self, c: Command) -> u64 {
        let n = self.jumps.fetch_add(1, Ordering::AcqRel) + 1;
        self.send(c);
        n
    }

    /// Plays queue (list) index `index` from `ms`. Answers the jump's number (see [`Event::Song`]).
    pub fn play_at(&self, index: usize, ms: i64) -> u64 {
        self.jump(Command::PlayAt(index, ms))
    }

    /// Goes to queue (list) index `index` at `ms` and leaves playing or paused as it was: paused, the
    /// place is held and nothing is fetched for it until play, as a skip, a previous or a seek is.
    /// Answers the jump's number (see [`Event::Song`]).
    pub fn go_to(&self, index: usize, ms: i64) -> u64 {
        self.jump(Command::GoTo(index, ms))
    }

    /// Pauses when the song playing ends (the sleep timer's "end of this song"), on the next one, from
    /// its start: nothing after this song is read or mixed into. `false` takes it back.
    pub fn pause_at_end(&self, on: bool) {
        self.send(Command::PauseAtEnd(on));
    }

    /// Plays from where the music is, or from where a jump or a stop left it. Answers the play's number
    /// (see [`Event::Stopped`]).
    pub fn play(&self) -> u64 {
        let n = self.plays.fetch_add(1, Ordering::AcqRel) + 1;
        self.send(Command::Play);
        n
    }

    /// Whether `event`, read now, is a stop ([`Event::Stopped`], [`Event::Bridge`]) from before a play
    /// asked for since. That play reaches the engine after the stop and starts the music again, so a
    /// client that let its "wants to play" go on such a stop would say paused over music playing: it
    /// passes over it instead, as over a song event from before its last jump ([`Event::Song`]).
    pub fn superseded(&self, event: &Event) -> bool {
        match event {
            Event::Stopped { plays } | Event::Bridge { plays } => *plays < self.plays.load(Ordering::Acquire),
            _ => false,
        }
    }

    pub fn pause(&self) {
        self.send(Command::Pause(None));
    }

    /// Pause at once, whatever the user's fade on pause is (headphones pulled out: the audio became
    /// noisy), cutting short a fade out already under way.
    pub fn pause_now(&self) {
        self.send(Command::Pause(Some(0)));
    }

    pub fn toggle(&self) {
        self.send(Command::Toggle);
    }

    /// The next song, as the button does it: paused, a skip starts the music
    /// (`nori_player::transport::skip_plays`).
    pub fn next(&self) -> u64 {
        self.jump(Command::Next)
    }

    /// Previous, as the button does it: back to the start of the song a few seconds in; paused, it
    /// starts the music, as the next button does.
    pub fn previous(&self) -> u64 {
        self.jump(Command::Previous)
    }

    pub fn seek(&self, ms: i64) {
        self.send(Command::Seek(ms));
    }

    /// The sound and the controls' fades, as the settings are now.
    pub fn set_settings(&self, settings: Settings) {
        self.send(Command::Settings(settings));
    }

    /// What the platform knows of the output now (a USB device, a bit-perfect DAC).
    pub fn set_output(&self, facts: OutputFacts) {
        self.send(Command::Output(facts));
    }

    /// Something a transition plan depends on changed (the transition settings, an analysis): the plan
    /// out of the song playing is asked for again.
    pub fn replan(&self) {
        self.send(Command::Replan);
    }

    /// The queue was edited where it is kept: the planner's window and what plays next follow.
    pub fn queue_changed(&self) {
        self.send(Command::QueueChanged);
    }

    /// Repeat off, one or all (`nori_player::playlist::REPEAT_*`).
    pub fn set_repeat(&self, mode: u8) {
        self.send(Command::Repeat(mode));
    }

    /// The ReplayGain settings changed: the volume of the song playing is asked for again.
    pub fn gain_changed(&self) {
        self.send(Command::Gain);
    }

    /// The equalizer's screen is open (`true`) or closed: a band moved is heard at once while it is,
    /// the sink trading its deep buffer for a shallow one (`nori_player::transport::Chain::tuning`),
    /// and the deep buffer is back at the next boundary after.
    pub fn set_tuning(&self, on: bool) {
        self.send(Command::Tuning(on));
    }

    /// Position events this often while music plays, or none (the default: an idle screen is not woken).
    pub fn position_updates(&self, every: Option<Duration>) {
        self.send(Command::Positions(every));
    }

    /// Reads the output once, now, and says the status again: for a screen coming back, or a seek bar
    /// whose reading is old ([`Status::screen_now`]). Between its own wakes the engine sleeps - minutes
    /// with the songs offloaded - and a place run on from a reading that old is only as good as that
    /// reading was.
    pub fn look(&self) {
        self.send(Command::Look);
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    /// Reads the status in place, without copying it (and the song's id with it): for a question asked
    /// every frame.
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
    /// A pause waiting for its fade to end.
    pause_at: Option<i64>,
    /// Switches waiting out their dip, when the dip is down, and how fast the music comes back.
    switches: VecDeque<Switched>,
    switch_at: Option<i64>,
    up_ms: i64,
    /// The song last reported, and its id: a new queue can put another song at the same index.
    heard: Option<usize>,
    heard_id: Option<String>,
    /// The jumps asked for (play_at, go_to, next, previous) taken off the channel.
    jumps: u64,
    /// The plays asked for ([`Engine::play`]) taken off the channel.
    plays: u64,
    /// The ReplayGain settings changed: the song playing's volume is asked for again.
    gain_changed: bool,
    positions: Option<i64>,
    next_position: i64,
    /// Paused: when the output is let go, and after that, where the player was.
    idle_release_ms: i64,
    /// The client keeping watch, if one does ([`crate::watch`]).
    watch: Option<crate::watch::Watcher>,
    idle_at: Option<i64>,
    released: Option<(usize, i64)>,
    releases: u64,
    /// The music ran out waiting for a song's bytes, as last said.
    stalled: bool,
    /// Where a jump, a skip or a seek made while paused goes (the queue index, the place, the song's
    /// id): held until play, so nothing is fetched for a place that may move again before anyone
    /// listens. The screen is told it at once.
    held: Option<(usize, i64, String)>,
    /// What the platform says of the output.
    facts: OutputFacts,
    /// The settings and the output let the songs go to the output's decoder, and if not, why not.
    offload: bool,
    blocked: Option<&'static str>,
    /// The output tore its offloaded track down, or would not open one: offload is given up for the
    /// engine's life.
    offload_refused: bool,
    tear_downs: u32,
    /// The next song, opened as packets to see whether the output decodes it: the CPU then plays the
    /// song playing to its end and hands over (the queue index, what came of it, and once known whether
    /// it is the output's).
    probe: Option<(usize, Result<Demuxed, String>, Option<bool>)>,
    /// The CPU plays this song to its end, and the output's decoder takes the next one.
    handing_over: Option<usize>,
    /// Repeat-one loops, and the placing of the song on the offload path, as last reported.
    loops: u32,
    heard_seq: u64,
    /// A live stream's announcement, and when the ear reaches it.
    title: Option<(String, i64)>,
    /// The queue's ids as last followed, for the offload path to find its songs again by.
    ids: Vec<String>,
    /// Music has been heard since the offload path started: the run of songs that would not play is broken.
    offload_heard: bool,
    /// The song playing on the CPU, opened as packets to see whether the output decodes it, now that
    /// offload is wanted (the queue index, what came of it, whether the change also changed the sound).
    entering: Option<(usize, Result<Demuxed, String>, bool)>,
    /// The output decodes the song playing: the next `Switched::Resound` hands it over.
    offload_now: bool,
    /// When the music is to be made again ([`Worker::resound_soon`]), and when it last was.
    resound_due: Option<i64>,
    resounded_at: i64,
    /// The music changed path without a jump asked for: [`Event::Placed`] is said at the next report.
    placed_due: bool,
    /// The song heard is played at a tempo a mix brought it in at: its place moves at another pace than
    /// the speed. A client running its place on at the speed alone (media3's controllers) has drifted by
    /// the time the song is back at its own, so the place is said again then ([`Event::Placed`]).
    stretched: bool,
    /// The song opened ahead of the ear for the music to be made again there ([`Worker::remake`]).
    remake: Option<Remake>,
    /// Where the volume comes back up from after a dip: from silence on a device just opened.
    up_from: Option<f32>,
    /// Where the last fade asked for goes ([`Worker::ramp`]): the volume the music is meant to be at once it
    /// is over.
    volume_to: f32,
    /// When the turns that panicked did, within the last [`PANICS_WITHIN_MS`].
    panics: VecDeque<i64>,
    /// The ear standing still while music should be moving, and since when.
    quiet: Option<Quiet>,
    /// The song last made again from scratch ([`Worker::start_again`]): the same song again, with no
    /// music heard in between, is the song's own failure.
    healed: Option<String>,
    /// A jump being made (the queue index and place): a turn that panicked while it was is made again
    /// there, where the music was asked to go.
    jumping: Option<(usize, i64)>,
    /// The engine needs the CPU kept awake, as last said ([`Event::Awake`]).
    awake: bool,
}

/// The song the ear is on, opened ahead of it to make the music again from there with a changed sound
/// ([`Worker::remake`]), while the output plays what it holds.
struct Remake {
    id: String,
    /// Opened from this place in it, ms.
    from_ms: i64,
    r: Demuxed,
    /// It opened, and its first bytes are here.
    ready: bool,
    /// When the dip is due to start, for the ear to be at `from_ms` at its bottom (engine ms).
    dip_at: i64,
    /// It takes the song over from the output's decoder.
    leaving: bool,
}

/// What a turn of the engine's thread ends in.
enum Turn {
    /// A command said so, or the engine was let go.
    Stop,
    /// Asleep for this long (ms), or until something wakes it.
    Sleep(Option<i64>),
}

/// Panics are counted over this long, ms.
const PANICS_WITHIN_MS: i64 = 60_000;
/// This many panics within [`PANICS_WITHIN_MS`] and the engine stops trying: playback stops, said, and the
/// thread waits for a command rather than panicking turn after turn.
const PANICS_KEPT_ON: usize = 3;

/// Playing, with the place heard standing still for this long and no song's bytes on their way that
/// could move it: the music is made again from scratch ([`Worker::start_again`]). A safety net under whatever
/// left the engine so; a song still coming over a slow network is not it (its request's own stall says
/// when that fails).
pub const QUIET_HEAL_MS: i64 = 10_000;
/// The place heard standing still this long is looked at once more before it is made again: a watching
/// client (the perf build's invariant watch) sees it at that wake.
const QUIET_SAY_MS: i64 = 5_000;
/// Playing on the CPU, the thread wakes at least this often to see that the music moves, should nothing
/// else wake it: longer than any burst cycle (a deep buffer's ten seconds at half speed), so while music
/// plays it never fires.
const QUIET_GUARD_MS: i64 = 30_000;

/// Where the ear stood still since when, while music should have been moving ([`Worker::follow_quiet`]).
#[derive(Debug, Clone, PartialEq)]
struct Quiet {
    since: i64,
    /// The song (queue index) and the place in it, ms, as it stands.
    place: (Option<usize>, i64),
    /// The place did not move at the last look.
    standing: bool,
}

/// Less than this left to play while a song's bytes are on their way is a stall a screen shows.
const STALL_US: i64 = 200_000;
/// A track torn down this many times gives offload up for the engine's life.
const TEAR_DOWNS: u32 = 2;
/// The dip the music is made again behind ([`Worker::resound_soon`]), down and up again, ms: long enough
/// that the cut is not a click, short enough to pass for nothing.
const RESOUND_DIP_MS: i64 = 30;
/// Changes to the sound that come quicker than this (a slider dragged) are made heard together.
const RESOUND_EVERY_MS: i64 = 150;
/// The most music the ring and the device may hold while the equalizer is tuned for a band moved to be
/// heard as it is: the shallow ring's and the shallow track's, with room for the device's own latency.
/// More is left from the deep buffer, and is made again.
const TUNED_HELD_US: i64 = 400_000;
/// Beyond the shallow ring and the shallow device, what [`TUNED_HELD_US`] leaves for the device's own
/// latency: the measure for a device that found it needs more than a phone speaker's shallow track.
const TUNED_SLACK_US: i64 = 160_000;
/// The equalizer screen turns tuning on at the first change made on it, which reaches the engine a
/// moment before the tuning does and is made again into the deep buffer. Tuning that comes within this
/// of the music made again makes it again once more, so that first change lands in the shallow buffer as
/// every later one does, whichever of the two came first.
const TUNED_AFTER_RESOUND_MS: i64 = 1_000;
/// A device holding more music than this (a phone's track holds seconds) holds enough of the old
/// ReplayGain level to be heard: the music is made again, as for any other change of the sound.
const HELD_US: i64 = 250_000;
/// How far ahead of the ear the song is opened when the music is made again with a changed sound: the
/// output plays on meanwhile, and is emptied behind the dip once the ear reaches that place, so opening
/// the song again is never a silence. The change is heard this long after it is made (with the dip), or
/// once the song is open if that takes longer.
pub const REMAKE_LEAD_MS: i64 = 120;
/// While the song is opened ahead, the song being read is not read on from as long as the output holds
/// this much: the two would pull one song's fetch back and forth.
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
            switches: VecDeque::new(),
            switch_at: None,
            up_ms: 0,
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
            stretched: false,
            resounded_at: i64::MIN / 2,
            remake: None,
            up_from: None,
            panics: VecDeque::new(),
            quiet: None,
            volume_to: 1.0,
            healed: None,
            jumping: None,
            awake: true,
        };
        w.apply(settings);
        w
    }

    fn now(&self) -> i64 {
        self.clock.now_ms() + 1_000
    }

    /// The engine's thread: a turn at every wake, and asleep in between. A turn that panics does not end
    /// the thread: an engine gone with nobody told was a player that said it played and made no sound,
    /// song after song, until the app was started again (the S22's classical playlist, 2026-09-26). The
    /// panic is said, the song is opened again from scratch ([`Worker::recover`]), and the music goes on.
    fn run(mut self) {
        loop {
            let turned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.turn()));
            let wake = match turned {
                Ok(Turn::Stop) => return,
                Ok(Turn::Sleep(w)) => w,
                Err(p) => {
                    let mut why = panic_words(&*p);
                    let now = self.clock.now_ms() + 1_000;
                    // Made again, the song may panic again as it opens: that is counted too, and the song
                    // made again a second time is skipped as one that would not play.
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
                        // Nothing more is tried until someone asks for something; a screen that still says
                        // it plays is told it stopped.
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

    /// One wake's work: the commands sent, then the music made and the screen told. What the thread may
    /// sleep for after it, or that it is to stop.
    fn turn(&mut self) -> Turn {
        self.clock.woke();
        loop {
            match self.rx.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return Turn::Stop,
                Ok(c) => {
                    // A command is work: the CPU is kept awake for it first, and let sleep again at the
                    // end of the turn if nothing came of it.
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
            // The burst's count of what the device holds is kept from the clock it reads, and misses
            // whatever played before its first reading; the ring knows exactly. At the low mark the
            // count starts again, so a burst always begins when the ring says it is time.
            if self.p.playing() && !self.p.source_ended() && self.p.sink.track.filled_us() <= WAKE_LOW_US {
                self.p.burst.restart();
            }
            // The song opened ahead to make the music again reads its bytes alone while the output
            // holds enough to play on.
            self.p.read_held = self.remake.as_ref().is_some_and(|m| !m.leaving) && self.held_us() > REMAKE_HOLD_US;
            self.p.turn(now);
            self.follow_offload_now();
            self.follow_offload_ahead();
        }
        self.check();
        self.announce(now);
        self.report(now);
        self.follow_why();
        self.follow_volume();
        self.follow_quiet(now);
        self.watch(now);
        self.follow_awake();
        // A flush this turn made is told to the device by now, with the music after it in the ring.
        self.p.sink.track.told();
        Turn::Sleep(self.wake_in(now))
    }

    /// Whether the CPU may sleep until the platform wakes the engine ([`Event::Awake`]), said as it
    /// changes: only while the songs are offloaded and the offload path needs nothing but the platform's
    /// word, with no control, switch, fade or song opened ahead under way here.
    fn follow_awake(&mut self) {
        let sleeps = self.state == State::Playing
            && self.pause_at.is_none()
            && self.switches.is_empty()
            && self.switch_at.is_none()
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

    /// The thread sleeps until a song's bytes come: the one being read or read on into, one opened to be
    /// looked at, or the one the output's decoder is given. Asked only by a clock moved by hand.
    fn waiting_for_bytes(&self) -> bool {
        self.p.waiting_for_bytes() || self.entering.is_some() || self.remake.as_ref().is_some_and(|m| !m.ready) || self.probe.as_ref().is_some_and(|p| p.2.is_none()) || self.off.as_ref().is_some_and(Offload::waiting_for_bytes)
    }

    // ---- the two paths, as one player ----

    /// The songs go to the output's decoder now (or the song starting there is still opening).
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

    /// Queue index `i` from `ms`, on the output's decoder when the settings and the output let it (the
    /// song itself decides, once it is open), on the CPU otherwise. Playing or not stays as it was.
    fn jump(&mut self, i: usize, ms: i64) {
        // Where it was going, should opening the song there panic: made again from scratch there.
        self.jumping = Some((i, ms));
        self.jump_now(i, ms);
        self.jumping = None;
    }

    fn jump_now(&mut self, i: usize, ms: i64) {
        self.probe = None;
        self.handing_over = None;
        // The music is made anew there, with the sound as it is.
        self.remake = None;
        if self.offload && self.off.is_some() {
            if !self.offloading() {
                // The CPU's device is let go: the two are never open at once.
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
                // The song placed on the track again for a jump (a seek, the chip taking it over from
                // the CPU) is not repeat one starting it again: no loop is said for it.
                self.heard_seq = 0;
                self.p.queue.moved_to(i);
                return;
            }
        }
        self.leave_offload();
        self.p.jump(i, ms);
    }

    /// The offload path lets its track go; where it was, if anywhere.
    fn leave_offload(&mut self) -> Option<(usize, i64)> {
        let off = self.off.as_mut()?;
        if !off.active() && off.track().is_none() {
            return None;
        }
        off.release()
    }

    /// The music goes on: on the path that holds the song.
    fn go_on(&mut self) {
        match self.off.as_mut() {
            Some(o) if o.active() => o.play(),
            _ => self.p.resume(),
        }
    }

    /// The music stops where it is.
    fn halt(&mut self) {
        match self.off.as_mut() {
            Some(o) if o.active() => o.pause(),
            _ => self.p.pause(),
        }
    }

    /// A fade of the volume from `from` (or where it is) to `to` over `ms`, where the music is.
    fn ramp(&mut self, from: Option<f32>, to: f32, ms: i64) {
        let now = self.now();
        self.volume_to = to;
        match self.off.as_mut() {
            Some(o) if o.active() => o.ramp(from, to, ms, now),
            _ => {
                self.p.sink.track.ramp(from, to, ms);
                // The output's decoder follows it too while it plays nothing, so a song it takes up later
                // starts at the volume the music is at, never at the silence a pause or a take-over by the
                // CPU left it at.
                if let Some(o) = self.off.as_mut() {
                    o.ramp(from, to, ms, now);
                }
            }
        }
    }

    /// Playing, with no pause, jump or dip under way, the music is meant to be at its full volume. When the
    /// last fade asked for left it lower (silent, to the ear, while the place moves on), it is faded back
    /// up at once, and the log says what it was left at and where the engine stood: a fade down that
    /// nothing brought back up is a bug to find.
    fn follow_volume(&mut self) {
        let settled = self.state == State::Playing && self.pause_at.is_none() && self.switch_at.is_none() && self.switches.is_empty() && self.held.is_none();
        if !settled || self.volume_to >= 1.0 {
            return;
        }
        let words = self.words((self.p.sink.track.filled_us() + self.p.sink.track.latency_us()) / 1000);
        self.p.app.log(&format!("playing, with the volume left at {:.2} and nothing fading it: brought back up ({words})", self.volume_to));
        self.ramp(None, 1.0, RESOUND_DIP_MS);
    }

    /// The CPU's path lets its device go, keeping the place for the next play.
    fn park(&mut self) {
        self.remake = None;
        if self.released.is_none() {
            self.released = self.p.release();
            self.p.sink.track.release();
        }
    }

    /// A turn of the offload path: its track topped up, the song starting placed, the CPU taking over
    /// where it must.
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
            self.p.sink.track.ramp(None, 1.0, 0);
        }
    }

    /// Offload wanted while the CPU plays: the next song is looked at, and when the output decodes it,
    /// the CPU plays the song playing to its end and hands over there, as media3 changes its sink's
    /// configuration at a song boundary.
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
        // Asked once, when the song is open and is to be looked at.
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
            // The next song stays on the CPU too, and why is the report's while it does.
            if why.is_some() {
                off.on_cpu = why;
            }
            // Its packets are not needed again: the offload path opens the song anew.
            *opened = Err(String::new());
        }
        // Only while the song playing is still being read: once the reader is into the next song, the
        // handover waits for the boundary after.
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
            (self.events)(Event::State(s));
        }
    }

    /// Paused a long while: the device and the song's bytes go, so nothing holds the sound card or the
    /// memory; the place in the queue and the song stays.
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

    /// After a release, the song is opened again where it was.
    fn reopen(&mut self) {
        if let Some((i, ms)) = self.released.take() {
            self.jump(i, ms);
        }
    }

    /// A turn panicked (`why`): it is said, and the music made again from scratch where the ear was
    /// ([`Worker::start_again`]). Panicking again and again ([`PANICS_KEPT_ON`] within
    /// [`PANICS_WITHIN_MS`]), playback stops instead, said as a failure, rather than going round.
    /// True when the music goes on; false when playback stopped for it, and the thread is to wait for a
    /// command rather than turn again (a panic in every turn would otherwise spin).
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

    /// Everything the player holds of the music it was making goes: what was opened ahead or waits to
    /// be switched to, the songs being read, the transition engine's audio, and the output (opened again
    /// by the next song). The place stays in the queue.
    fn let_go_of_everything(&mut self) {
        self.remake = None;
        self.probe = None;
        self.entering = None;
        self.offload_now = false;
        if self.handing_over.take().is_some() {
            self.p.pause_at_end(false);
        }
        self.switches.clear();
        self.switch_at = None;
        self.resound_due = None;
        self.pause_at = None;
        self.held = None;
        self.quiet = None;
        self.unstall();
        let _ = self.leave_offload();
        self.p.release();
        self.p.sink.track.release();
        self.released = None;
    }

    /// The music made again from scratch where the ear is (`why` it is): everything the player holds of
    /// it goes ([`Worker::let_go_of_everything`]), and with it the song's bytes and what the stream cache
    /// keeps of it ([`Library::forget`]); the song is fetched and opened anew there, on a new output,
    /// playing as it was. The same song made again with no music heard since is the song's own failure:
    /// the queue's rules take it as one that would not play (skipped, or playback stopped there).
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
        if self.healed.as_deref() == Some(id.as_str()) {
            self.healed = None;
            self.p.app.log(&format!("{id} made no music again ({why}): it counts as a song that would not play"));
            self.p.give_up(i, format!("no music came of it ({why})"));
        } else {
            self.healed = Some(id.clone());
            self.p.app.log(&format!("{id} is opened again from scratch at {ms} ms ({why})"));
            self.jump(i, ms);
        }
        if playing && self.p.stopped_at().is_none() && self.current().is_some() {
            self.go_on();
            self.ramp(Some(0.0), 1.0, RESOUND_DIP_MS);
            self.set_state(State::Playing);
        }
    }

    /// Whether the song the music waits for has its bytes on their way: the one being read (or opening),
    /// or the one after it once that is read to its end. Waiting with nothing on its way is waiting for
    /// nothing. On the output's decoder, what it waits for is its own to say.
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

    /// Music should be moving (playing, no pause or jump under way): the place heard is looked at, and
    /// when it has stood still for [`QUIET_HEAL_MS`] with no song's bytes on their way that could move
    /// it, the music is made again from scratch ([`Worker::start_again`]). What left the engine so is
    /// said in the log with the engine's own account of where it stood.
    fn follow_quiet(&mut self, now: i64) {
        let wanted = self.state == State::Playing && self.pause_at.is_none() && self.switch_at.is_none() && self.switches.is_empty() && self.held.is_none() && self.p.queue.read(|q| !q.is_empty());
        if !wanted {
            self.quiet = None;
            return;
        }
        let place = (self.current(), self.position_ms());
        let mut q = match self.quiet.take() {
            Some(q) if q.place == place => Quiet { standing: true, ..q },
            Some(_) => {
                // The music moves: a song made again before is playing.
                self.healed = None;
                Quiet { since: now, place, standing: false }
            }
            None => Quiet { since: now, place, standing: false },
        };
        // A playing offloaded track has its own watchdog, which knows how long the platform's counts may
        // stand still (seconds, with the screen off, while the chip plays from its own buffer) and hands
        // the song to the CPU where the chip was: made again from scratch here, a count standing ten
        // seconds would lose the song.
        if self.bytes_coming() || self.off.as_ref().is_some_and(Offload::watching) {
            // Its bytes are on their way (a slow network): their request says when that fails.
            q.since = now;
            q.standing = false;
        }
        let long = now - q.since;
        self.quiet = Some(q);
        if long >= QUIET_HEAL_MS {
            let words = self.words((self.p.sink.track.filled_us() + self.p.sink.track.latency_us()) / 1000);
            self.p.app.log(&format!("playing, and the music stood still for {long} ms with nothing on its way: {words}"));
            let id = self.current().map(|i| self.p.id_at(i)).unwrap_or_default();
            (self.events)(Event::Error { id, message: format!("no music for {} s while playing: the song is opened again from scratch", long / 1000) });
            self.start_again(&format!("the music stood still for {long} ms"));
        }
    }

    /// How long the place heard has stood still while music should be moving, ms (0 when it moves, or
    /// when it waits for bytes on their way).
    fn quiet_ms(&self, now: i64) -> i64 {
        self.quiet.as_ref().filter(|q| q.standing).map_or(0, |q| now - q.since)
    }

    /// The output device changed: the core picks its sound (a profile bound to it), which is applied.
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

    /// The jumps asked for that have been made (or held, paused): those taken less those still waiting
    /// out their dip. What the ear is on is said with this ([`Event::Song`]'s `jumps`).
    fn made(&self) -> u64 {
        let waiting = self.switches.iter().filter(|s| matches!(s, Switched::To(..) | Switched::Next | Switched::Previous)).count();
        self.jumps - waiting as u64
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
                // The sleep timer's stop comes before a handover waiting at the same end.
                if self.handing_over.take().is_some() && !on {
                    self.p.pause_at_end(false);
                }
                let Worker { p, off, .. } = self;
                match off.as_mut() {
                    Some(o) if o.active() => {
                        if o.pause_at_end(on, &mut p.tracks, &p.queue) {
                            // The next song is in the track already: it starts again here, without it.
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
            // The skip buttons: paused, a skip is a request for music (nori_player::transport::skip_plays).
            Command::Next => self.skip(Switched::Next, now),
            Command::Previous => self.skip(Switched::Previous, now),
            Command::Seek(ms) => self.go(Switched::Seek(ms), Switch::Seek, now),
            Command::Settings(s) => self.apply(s),
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
                // Another song may follow the one playing now: its ending is planned again, and made
                // again where the output holds it made for the song that followed before.
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

    /// The equalizer screen opened or closed. The pipeline's rule waits for the next song to change the
    /// output's depth, which on a phone's AudioTrack is a rebuild and a gap; here it is a dip, so the
    /// shallow buffer comes at once (a band moved is heard at once from the first touch) and the deep one
    /// comes back as the screen closes.
    ///
    /// Over an output that [`AudioOutput::resizes`] (a phone's AudioTrack) neither is heard: the ring and
    /// the device change their depth in place, nothing is dropped or made again. Made shallow, what they
    /// hold plays out; a band moved before it has is made again behind the dip every change of the sound
    /// takes outside the screen ([`Worker::apply`]), and from then on is heard as it is.
    fn tune(&mut self, on: bool) {
        self.p.set_tuning(on);
        if self.p.sink.track.resizes() {
            self.follow_depth();
            // The change that turned tuning on was made again into the deep buffer a moment ago: again,
            // into the shallow one, as a change made once tuned is. Otherwise which buffer it landed in
            // was down to whether the change or the tuning reached the engine first.
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

    /// The queue changed: the offload path finds its songs again, and starts again where the ear is when
    /// a song it already wrote no longer follows.
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

    /// The settings, through the audio policy Android applies: a DAC playing bit-perfect keeps the samples
    /// untouched, which stands the sound chain, silence skipping, the pinned output format, every
    /// transition and ReplayGain down; high quality output on a device that plays float runs all of them
    /// on float samples, into a float device; nothing that needs the samples lets them go to the output's
    /// decoder. Songs opened from now on are decoded for it.
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
        // A song ReplayGain turns up is put on its samples as floats, the limiter behind it: only where
        // the samples may be touched. The limiter does not count as the sound chain being on, which
        // would keep every song off the output's decoder: only a song turned up keeps off it.
        let gain_max = if policy.untouched || !(s.gain_boost_db > 0.0) { 1.0 } else { 10f32.powf(s.gain_boost_db.min(nori_player::gain::BOOST_MAX_DB) / 20.0) };
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
        // A new maximum is heard as the output is made again (below): the device opens again at it.
        self.p.sink.track.max_rate = s.max_rate;
        // The player starts out with the defaults' sound; the output's say is given once at least.
        let first = self.applied.is_none();
        let was = self.applied.take().unwrap_or(Applied { sound: Sound::default(), speed: (1.0, 1.0), skip_silence: false, untouched: false, bit_perfect: false, float: false, gain_max: 1.0, max_rate: 0 });
        // Songs are read as floats with high quality output or a bit-perfect DAC (the chain run on them,
        // the device fed float), and when ReplayGain turns songs up (they go over full scale until the
        // limiter holds them).
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
            // Wherever the samples may be touched the equalizer stays in, flat and skipped while nothing
            // in it is on, as Android's does: switching it on is heard at once, not at the next song.
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
        // Anything that changes what the samples become: what the output holds of them was made before.
        let heard_differently = !first && was != now;
        let sound_only = was.sound != now.sound && Applied { sound: now.sound.clone(), ..was.clone() } == now;
        // The transitions: the planner reads its own settings, but the plan out of the song playing was
        // made under the old ones (and the output may hold its ending already).
        let replan = first || was.untouched != now.untouched || (self.settings.crossfade_s, self.settings.auto_mix) != (s.crossfade_s, s.auto_mix);
        self.applied = Some(now);
        self.settings = s;
        let restarted = self.follow_offload(policy.offload, heard_differently);
        if replan {
            self.replan();
        }
        if heard_differently && !restarted {
            // While the equalizer is tuned the output is shallow already: a band moved is heard as it is.
            // Made shallow in place, it may still hold seconds from before: those are made again once.
            let tuned = self.p.chain.tuning && self.p.sink.capacity_us == self.p.shallow_us && self.held_us() <= self.tuned_held_us();
            match self.entering.as_mut() {
                // Handed to the output's decoder where the ear is, or made again there if it stays here.
                Some(e) => e.2 = true,
                None if sound_only && tuned => {}
                None => self.resound_soon(),
            }
        }
    }

    /// The plan out of the song playing is asked for again: the transition settings changed, or something
    /// the planner reads (a song measured, the ReplayGain mode, the queue). The engine takes it up at its
    /// next buffer - but the output runs ten seconds and more ahead of the ear, and when the song's ending
    /// is in it already (made gapless, or held for the old plan's mix) the new plan would first be heard
    /// a song later: a crossfade or AutoMix switched on near the end of a song did nothing, and one
    /// switched off still mixed. Then the music is made again from where the ear is, behind the short dip
    /// any change of the sound takes, and the ending is made the new way. A mix already being heard plays
    /// out as it began.
    fn replan(&mut self) {
        self.p.engine.replan();
        if self.offloading() || self.p.mixing() {
            return;
        }
        let Some((cur, ear_ms)) = self.p.ear() else { return };
        let id = self.p.id_at(cur);
        if self.p.read_astray(cur) {
            // Read on gaplessly into a song the queue no longer has next (one was queued before it).
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
        // Gapless so far, and the ear past where the new mix would have ended: nothing to make again.
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

    /// Offload came or went with the settings or the output. Coming off it is at once, where the ear is:
    /// the chip's path cannot take what now needs the samples. Going onto it is at once too, where the
    /// ear is and behind a dip, once the song playing turns out to be one the output decodes (else at
    /// the next song that is); while paused, the swap costs nothing. `resound`: the change also made
    /// the music sound different, so a song that stays on the CPU is made again where the ear is. True
    /// when the CPU took the music over from the output's decoder, made with the settings as they are.
    fn follow_offload(&mut self, wanted: bool, resound: bool) -> bool {
        let was = std::mem::replace(&mut self.offload, wanted);
        self.status.lock().offload_wanted = wanted;
        if was == wanted {
            return false;
        }
        self.probe = None;
        self.entering = None;
        // A song opened ahead for the path now changing again: the path it was for is not the one wanted.
        self.remake = None;
        if !wanted {
            if self.handing_over.take().is_some() {
                self.p.pause_at_end(false);
            }
            if self.offloading() {
                // Where the chip plays on (nothing USB attached, the track not refused), it does until
                // the CPU has the song open where the ear will be: the handover is then a dip, not a
                // silence as long as the song takes to open.
                let playing = self.state == State::Playing && self.pause_at.is_none();
                if playing && !self.facts.usb && !self.offload_refused && self.leave_ahead() {
                    return true;
                }
                return self.leave_now();
            }
        } else if !self.p.playing() && self.p.current().is_some() && self.held.is_none() {
            self.park();
        } else if let Some(i) = self.p.current().filter(|_| self.p.playing() && !self.offloading()) {
            // The song playing is opened as packets to see whether the output decodes it.
            let id = self.p.id_at(i);
            self.entering = Some((i, self.p.tracks.open_packets(&id, 0, true), resound));
        }
        false
    }

    /// The song the chip plays, opened on the CPU [`REMAKE_LEAD_MS`] ahead of where the chip is: the CPU
    /// takes it over there once it is open ([`Worker::remake`]).
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

    /// Offload wanted while the CPU plays a song: once it is known whether the output decodes it, the
    /// output takes it over where the ear is (behind a dip, as a jump), or the CPU plays on.
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

    /// The most music the ring and the device hold once tuned for a band moved to be heard as it is:
    /// [`TUNED_HELD_US`], or the shallow ring's, the shallow device's as it found it needs (a Bluetooth
    /// output's latency in it) and some slack.
    fn tuned_held_us(&self) -> i64 {
        let device = self.p.sink.track.shallow_depth().map_or(0, |d| d.device_us);
        TUNED_HELD_US.max(device + self.p.shallow_us + TUNED_SLACK_US)
    }

    /// While tuned over a device that resizes in place, the ring is kept as deep as the device found it
    /// needs to be fed from ([`AudioOutput::shallow_depth`]): never shallower than [`SHALLOW_US`]. The
    /// device says so from its own thread once it has looked at where it plays, or grown after running
    /// dry; one atomic read a wake, and only while tuned.
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

    /// Music made and not yet heard: what the ring holds and what the device does, µs.
    fn held_us(&self) -> i64 {
        let track = &self.p.sink.track;
        track.filled_us() + track.latency_us()
    }

    /// Something that changes what the music sounds like changed while the CPU plays it: what the output
    /// holds (the ring's seconds, and a phone's track's), made with the old settings, is made again from
    /// where the ear is, behind a short dip, so the change is heard at once instead of seconds later.
    /// Changes that come quickly one after the other (a slider dragged) are taken together, one
    /// [`RESOUND_EVERY_MS`] at most. Paused, the music is made again when it comes back.
    fn resound_soon(&mut self) {
        if self.offloading() || self.p.current().is_none() {
            return;
        }
        if !self.p.playing() {
            self.p.resound();
            return;
        }
        if self.switches.iter().any(|s| matches!(s, Switched::Resound)) {
            // The dip is going down: the music made again at its bottom is made with this change too.
            return;
        }
        let at = self.now().max(self.resounded_at + RESOUND_EVERY_MS);
        self.resound_due = Some(self.resound_due.map_or(at, |t| t.min(at)));
    }

    /// The music made again now: the dip goes down, and at its bottom the output is emptied and filled
    /// again from where the ear is (`Switched::Resound`).
    fn resound(&mut self, now: i64) {
        if let Some(t) = self.pause_at {
            // The pause's fade is running: looked at again once it is over, when the music is made again
            // as it comes back.
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
            // A mix is heard: made again once it is over, rather than cutting it off.
            self.resound_due = Some(now + 250);
            return;
        }
        if self.switches.iter().any(|s| matches!(s, Switched::Resound)) || self.remake.is_some() {
            // Made again already, or about to be: with this change too.
            return;
        }
        if self.switch_at.is_none() && !self.offload_now && self.open_ahead(now) {
            return;
        }
        self.dip_for_resound(now);
    }

    /// The dip the music is made again behind: down now, and made again at its bottom
    /// ([`Worker::resounded`]).
    fn dip_for_resound(&mut self, now: i64) {
        if self.switch_at.is_none() {
            self.ramp(None, 0.0, RESOUND_DIP_MS);
            self.switch_at = Some(now + RESOUND_DIP_MS);
            self.up_ms = RESOUND_DIP_MS;
        }
        self.switches.push_back(Switched::Resound);
    }

    /// The song the ear is on is opened [`REMAKE_LEAD_MS`] ahead of it, to make the music again from
    /// there once it is open ([`Worker::remake`]); what the output holds plays on meanwhile. False where
    /// that cannot be: a station (a live stream is where the station is now, not ahead of the ear) or a
    /// song of no known length, and the last moment of a song.
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

    /// The song opened ahead of the ear ([`Worker::open_ahead`]): once it is open and the ear is a dip
    /// away from where it was opened, the dip goes down, and at its bottom the output is emptied and
    /// filled from it ([`Worker::resounded`]). Given up (and the music made again as before) when the
    /// ear moved to another song meanwhile, or it would not open; paused, the music is made again as it
    /// comes back.
    fn remake(&mut self, now: i64) {
        let Some(m) = self.remake.as_ref() else { return };
        if self.switches.iter().any(|s| matches!(s, Switched::Resound)) {
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
                // Paused on the chip: nothing is heard of the handover.
                self.remake = None;
                self.leave_now();
                return;
            }
        } else if self.offloading() || self.pause_at.is_some() {
            // Paused (its fade still running) or gone to the output's decoder: nothing to make ahead.
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
                // Its loader wakes the thread when it is.
                return;
            }
            if m.r.error().is_some() {
                // It will not open ahead: the music is made again where the ear is, as the song opens.
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
        if self.switch_at.is_some() || now < m.dip_at {
            return;
        }
        if !m.leaving {
            let (from_ms, id) = (m.from_ms, m.id.clone());
            match self.p.ear_now() {
                Some((i, _)) if self.p.id_at(i) != id => {
                    // The ear went on into the next song: opened again there.
                    self.remake = None;
                    self.resound_soon();
                    return;
                }
                Some((_, ms)) => {
                    let speed = self.p.speed().0.clamp(0.1, 8.0) as f64;
                    let short = ((from_ms - ms) as f64 / speed) as i64 - RESOUND_DIP_MS;
                    if short > 1 {
                        // The ear is slower than the clock said (the output's clock settling): a moment more.
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
                // A mix is heard: made again once it is over, rather than cutting it off.
                self.remake = None;
                self.resound_due = Some(now + 250);
                return;
            }
        }
        self.ramp(None, 0.0, RESOUND_DIP_MS);
        self.switch_at = Some(now + RESOUND_DIP_MS);
        self.up_ms = RESOUND_DIP_MS;
        self.switches.push_back(Switched::Resound);
    }

    /// Music from where the player is: fading in from silence when the settings say so.
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
            // Pressed again inside the fade out: the music comes back from where the fade got to.
            self.ramp(None, 1.0, self.settings.fade_ms.max(0) as i64);
            self.set_state(State::Playing);
            return;
        }
        // A place asked for inside the fade out (a new queue, a skip) was held, as paused: it is where
        // the music is wanted now, not the song the fade was taking away.
        if self.playing() && self.state == State::Playing {
            return;
        }
        if let Some((i, ms, _)) = self.held.take() {
            // The place a skip or a seek left while paused: fetched now that the music is wanted.
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
            // Stopped at a song that would not play, nothing is being read: resuming would run the
            // clock over silence. It is tried again.
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
            // Asked to stop at once inside a fade out: it stops now.
            self.pause_at = None;
            self.halt();
            return;
        }
        if !self.playing() || self.pause_at.is_some() {
            return;
        }
        // A pause never swallows the switch it interrupts.
        self.run_switches();
        match pause_fade(fade_ms, true) {
            Some(ms) => {
                self.ramp(None, 0.0, ms as i64);
                self.pause_at = Some(now + ms as i64);
            }
            None => self.halt(),
        }
        self.set_state(State::Paused);
    }

    /// A skip button: made from the place held, if one is, and a request for music when paused.
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

    /// A jump or a seek that leaves playing or paused as it was: while music plays it is made (with its
    /// dip), paused it is held until play.
    fn go(&mut self, s: Switched, kind: Switch, now: i64) {
        if self.playing() && self.pause_at.is_none() {
            self.switch(s, kind, now);
        } else {
            self.hold(s);
        }
    }

    /// Works out where `s` goes from the place held or the player's, and holds it there.
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
            // Paused, the music is made again as it comes back: nothing to hold.
            Switched::Resound => return,
        };
        self.held = Some((i, ms, self.p.id_at(i)));
        // The queue is where the user put it, heard or not: a queue saved now (and a program started
        // again from it) comes back on this song, not the one before the skip.
        if !matches!(s, Switched::Seek(_)) {
            self.p.queue.moved_to(i);
        }
    }

    /// The queue was edited: a held place stays on its song, wherever that is now.
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
                if self.switch_at.is_none() {
                    self.ramp(None, 0.0, dip.down_ms as i64);
                    self.switch_at = Some(now + dip.down_ms as i64);
                }
                self.switches.push_back(s);
                self.up_ms = dip.up_ms as i64;
            }
            None => {
                self.switches.push_back(s);
                self.run_switches();
            }
        }
    }

    fn run_switches(&mut self) {
        let dipped = self.switch_at.take().is_some();
        if !self.switches.is_empty() {
            self.reopen();
        }
        while let Some(s) = self.switches.pop_front() {
            // Only a play_at comes through here paused (a skip or a go_to is held instead): music is wanted.
            let wants_music = !matches!(s, Switched::Seek(_) | Switched::Resound);
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
                Switched::Seek(ms) => self.seek(ms),
                Switched::Resound => self.resounded(),
            }
            // A skip while paused is a request for music.
            if wants_music && !self.playing() && self.current().is_some() {
                self.resume();
            }
        }
        if dipped {
            let (from, up) = (self.up_from.take(), self.up_ms);
            self.ramp(from, 1.0, up);
        }
        if self.playing() {
            self.set_state(State::Playing);
        }
    }

    /// The dip is down: the output's decoder takes the song over where the ear is, or the CPU makes the
    /// music again from there.
    fn resounded(&mut self) {
        self.resounded_at = self.now();
        // Every change made so far is in what is made now.
        self.resound_due = None;
        let mut remake = self.remake.take();
        if remake.as_ref().is_some_and(|m| m.leaving) {
            return self.take_over(remake.take().expect("checked"));
        }
        if self.offloading() {
            return;
        }
        let Some(i) = self.p.current() else { return };
        if std::mem::take(&mut self.offload_now) && self.offload && self.off.is_some() && self.p.playing() {
            // Where the ear is, read from the clock as it stops.
            self.p.pause();
            let ms = self.p.position_ms();
            self.jump(i, ms);
            self.placed_due = true;
            // The chip's track comes up from silence with the dip.
            self.ramp(Some(0.0), 0.0, 0);
            return;
        }
        match remake {
            Some(m) if m.ready => self.p.resound_from(m.id, m.r, m.from_ms),
            _ => self.p.resound(),
        }
    }

    /// The CPU takes the song over from the output's decoder, where the chip got to: with the song opened
    /// for it ahead of time ([`Worker::leave_ahead`]), from silence up with the dip.
    fn take_over(&mut self, m: Remake) {
        let playing = self.state == State::Playing && self.pause_at.is_none();
        let now = self.now();
        // The chip's fade ends in silence, whichever tick it last took.
        self.ramp(None, 0.0, 0);
        let Some((i, ms)) = self.off.as_mut().and_then(|o| o.leave(now)) else { return };
        self.p.app.log("offload given up: the CPU plays on from here");
        if m.ready {
            self.p.jump_from(i, ms, (m.id, m.r, m.from_ms));
        } else {
            self.p.jump(i, ms);
        }
        self.placed_due = true;
        if playing {
            self.p.resume();
            self.up_from = Some(0.0);
        }
    }

    /// Off the output's decoder at once, where the ear is.
    fn leave_now(&mut self) -> bool {
        let playing = self.state == State::Playing && self.pause_at.is_none();
        let now = self.now();
        let Some((i, ms)) = self.off.as_mut().and_then(|o| o.leave(now)) else { return false };
        self.p.app.log("offload given up: the CPU plays on from here");
        self.p.jump(i, ms);
        self.placed_due = true;
        if playing {
            self.p.resume();
            self.p.sink.track.ramp(None, 1.0, 0);
        }
        true
    }

    /// A seek in the song playing: the offload path starts again there, at the packet it lands in.
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
        if self.switch_at.is_some_and(|t| now >= t) {
            self.run_switches();
        }
        if self.resound_due.is_some_and(|t| now >= t) {
            self.resound(now);
        }
        self.remake(now);
    }

    /// Each song's ReplayGain volume is put on its samples by the player itself, before any mix
    /// (`TransitionEngine::set_gain`). Only a change of the settings is applied here: to what the ring
    /// still holds and what is read from now on, or on the offload path to the track's volume, at once.
    fn follow_gain(&mut self) {
        if !std::mem::take(&mut self.gain_changed) {
            return;
        }
        match self.off.as_ref() {
            Some(o) if o.active() => {
                let level = self.current().map_or(1.0, |i| self.gain_of(i));
                if !nori_player::gain::offload_allows(level) {
                    // Turned up now: the song needs its samples, and the CPU takes it over where the ear is.
                    if let Some(o) = self.off.as_mut() {
                        o.on_cpu = Some(OnCpu::TurnedUp);
                    }
                    let playing = self.state == State::Playing && self.pause_at.is_none();
                    if !(playing && !self.facts.usb && !self.offload_refused && self.leave_ahead()) {
                        self.leave_now();
                    }
                    return;
                }
                if let Some(o) = self.off.as_mut() {
                    o.set_level(level);
                }
            }
            _ => {
                self.p.gain_changed();
                // The ring's music was turned where it lies; what the device took of it cannot be. Nor
                // may music already through the limiter be turned up where it lies: made again.
                if self.p.sink.track.latency_us() > HELD_US || self.p.gain_max > 1.0 {
                    self.resound_soon();
                }
            }
        }
    }

    /// Song `i`'s gain, as far as songs may be turned up now.
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
            // The device is let go as a long pause lets it go: the next play opens a new one where the
            // music was, rather than resuming into one that takes nothing.
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
            // The CPU played the song to its end: the output's decoder takes the next one from its start.
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
        } else if self.p.playing() && self.p.ended() && self.p.stopping_after().is_some() {
            // Heard to the end of the song the sleep timer stops at: paused on the next one, from its
            // start, which is fetched when play is pressed.
            let i = self.p.stopping_after().expect("checked");
            self.p.pause_at_end(false);
            self.p.pause();
            let next = self.p.queue.read(|q| q.next_of(i, q.repeat()));
            if let Some(n) = next {
                self.held = Some((n, 0, self.p.id_at(n)));
            }
            (self.events)(Event::Stopped { plays: self.plays });
            self.set_state(if next.is_some() { State::Paused } else { State::Ended });
        } else if self.p.playing() && self.p.ended() {
            self.p.pause();
            self.set_state(State::Ended);
        } else if !self.p.playing() && self.state == State::Playing && self.pause_at.is_none() && self.switch_at.is_none() {
            if std::mem::take(&mut self.p.bridge) {
                // A song the network would not bring: the app's offline bridge takes over from here.
                (self.events)(Event::Bridge { plays: self.plays });
                self.set_state(State::Paused);
                return;
            }
            // The queue's rules stopped playback (a run of songs that would not play).
            (self.events)(Event::Stopped { plays: self.plays });
            self.set_state(State::Paused);
        }
    }

    /// The offload path heard everything it was given: the end of the queue (or of the sleep timer's
    /// song), or the next song, which needs a track of its own or the CPU.
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
                match stop_after.zip(at).filter(|(s, a)| s == a) {
                    Some((i, _)) => {
                        let next = self.p.queue.read(|q| q.next_of(i, q.repeat()));
                        if let Some(n) = next {
                            self.held = Some((n, 0, self.p.id_at(n)));
                        }
                        (self.events)(Event::Stopped { plays: self.plays });
                        self.set_state(if next.is_some() { State::Paused } else { State::Ended });
                    }
                    None => self.set_state(State::Ended),
                }
            }
        }
    }

    /// A live stream's announcement, once the song being read has passed it, is said when the ear gets
    /// there: after what the output holds.
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

    /// What this wake saw, for a client keeping watch ([`crate::watch`]); nothing unless one wants it.
    fn watch(&mut self, now: i64) {
        crate::watch::look(self.watch.as_ref(), || {
            let offloaded = self.offloading();
            let in_output_ms = match self.off.as_ref() {
                Some(o) if offloaded => o.in_track_us() / 1000,
                _ => (self.p.sink.track.filled_us() + self.p.sink.track.latency_us()) / 1000,
            };
            let waiting = self.stalled || self.waiting_for_bytes();
            let state = self.words(in_output_ms);
            let quiet_ms = self.quiet_ms(now);
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

    /// Where the engine stands, in words, for a watching client's report of a stall: its state, the
    /// player's own account (the song, what is read and opened, the transition engine), what the ring and
    /// the output hold, what it waits for, and every song's loader.
    fn words(&self, in_output_ms: i64) -> String {
        let mut w = format!("{:?}", self.state);
        if self.switch_at.is_some() {
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

    /// Why the music is on the CPU, said whenever it changes: in the status for the perf report's output
    /// line, and once in the log.
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

    /// Another song than the one last said is at queue index `i` now (a new queue put it there), and the
    /// ear is on it: no jump is still waiting out its dip, so what is heard is the queue's song. Compared
    /// in place, as this runs on every wake.
    fn other_song_at(&self, i: usize) -> bool {
        let waiting = self.switches.iter().any(|s| matches!(s, Switched::To(..) | Switched::Next | Switched::Previous));
        !waiting && self.p.queue.read(|q| q.ids().get(i).map(String::as_str) != self.heard_id.as_deref())
    }

    /// The music is no longer waiting for a song's bytes on the CPU path (it was let go, paused on a
    /// place held, or went to the output's decoder): a wait said is said to be over.
    fn unstall(&mut self) {
        if std::mem::take(&mut self.stalled) {
            (self.events)(Event::Buffering(false));
        }
    }

    fn report(&mut self, now: i64) {
        if let Some((i, ms)) = self.held.as_ref().map(|h| (h.0, h.1)) {
            // A place held while paused is where the player is, to the screen. Its id is copied only
            // when it changes.
            self.unstall();
            let id = || self.held.as_ref().map(|h| h.2.clone()).unwrap_or_default();
            let other = self.heard != Some(i) || self.held.as_ref().map(|h| h.2.as_str()) != self.heard_id.as_deref();
            if other {
                self.heard = Some(i);
                self.heard_id = Some(id());
                let jumps = self.made();
                (self.events)(Event::Song { index: i, id: id(), jumps });
            }
            let mut s = self.status.lock();
            s.state = self.state;
            if s.index != Some(i) || other {
                s.index = Some(i);
                s.id = Some(id());
            }
            s.position_ms = ms;
            s.at = Instant::now();
            s.switching = false;
            s.on_cpu = false;
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
            // The output's decoder says its own waits: a wait said on the CPU path is over.
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
        if stalled != self.stalled {
            self.stalled = stalled;
            (self.events)(Event::Buffering(stalled));
        }
        let seen = self.p.bar();
        let index = seen.index.or(self.p.current());
        let ms = if seen.index.is_some() { seen.ms } else { self.p.position_ms() };
        // The song's id is copied only when the song changes: this runs on every wake.
        let jumps = self.made();
        let other = index != self.heard || index.is_some_and(|i| self.other_song_at(i));
        if other {
            self.heard = index;
            self.heard_id = index.map(|i| self.p.id_at(i));
            if let Some(i) = index {
                (self.events)(Event::Song { index: i, id: self.p.id_at(i), jumps });
            }
        } else if self.p.loops != self.loops {
            if let Some(i) = index {
                (self.events)(Event::Looped { index: i, id: self.p.id_at(i), jumps });
            }
        }
        self.loops = self.p.loops;
        let mixing = self.p.mixing();
        // The pace the place moves at: the speed, and the tempo a mix brings the song heard in at.
        let tempo = self.p.sink.pace_heard();
        let pace = self.p.speed().0 * tempo as f32;
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
                s.id = index.map(|i| self.p.id_at(i));
            }
            s.position_ms = ms;
            s.at = Instant::now();
            s.speed = self.p.speed().0;
            s.pace = pace;
            s.mixing = mixing;
            s.underruns = self.p.sink.track.underruns();
            s.releases = self.releases;
            s.switching = self.switch_at.is_some();
            s.chain = self.p.sink.chain_in();
            s.on_cpu = self.state == State::Playing && !self.stalled && !s.switching;
            s.gain_reduction_db = self.p.sink.meter_db;
            s.compression_db = self.p.sink.compression_db();
            s.offloaded = false;
            was
        };
        if mixing != mixing_was {
            (self.events)(Event::Mixing(mixing));
        }
        if let Some(i) = index.filter(|_| self.placed_due) {
            self.placed_due = false;
            (self.events)(Event::Placed { index: i, ms });
        }
        if let (Some(every), Some(i), State::Playing) = (self.positions, index, self.state) {
            if now >= self.next_position {
                self.next_position = now + every;
                (self.events)(Event::Position { index: i, ms });
            }
        }
    }

    /// What the ear is on while the songs go to the output's decoder: the queue follows it, the song
    /// after it is fetched, and a song placed again (repeat one) is a loop.
    fn report_offload(&mut self, now: i64) {
        let Some((i, ms, seq)) = self.off.as_mut().and_then(Offload::heard) else { return };
        if self.heard != Some(i) || self.other_song_at(i) {
            self.heard = Some(i);
            let id = self.p.id_at(i);
            self.heard_id = Some(id.clone());
            self.p.queue.moved_to(i);
            if let Some(n) = self.p.queue.read(|q| q.next_of(i, q.repeat())) {
                let next = self.p.id_at(n);
                self.p.tracks.upcoming(&next);
            }
            let jumps = self.made();
            (self.events)(Event::Song { index: i, id, jumps });
        } else if seq != self.heard_seq && self.heard_seq != 0 {
            let jumps = self.made();
            (self.events)(Event::Looped { index: i, id: self.p.id_at(i), jumps });
        }
        self.heard_seq = seq;
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
                s.id = Some(self.p.id_at(i));
            }
            s.position_ms = ms;
            s.at = Instant::now();
            s.speed = 1.0;
            s.pace = 1.0;
            s.mixing = false;
            s.releases = self.releases;
            s.switching = self.switch_at.is_some();
            s.chain = false;
            s.on_cpu = false;
            s.gain_reduction_db = 0.0;
            s.compression_db = 0.0;
            s.offloaded = true;
            was
        };
        if mixing_was {
            (self.events)(Event::Mixing(false));
        }
        if std::mem::take(&mut self.placed_due) {
            (self.events)(Event::Placed { index: i, ms });
        }
        if let (Some(every), State::Playing) = (self.positions, self.state) {
            if now >= self.next_position {
                self.next_position = now + every;
                (self.events)(Event::Position { index: i, ms });
            }
        }
    }

    /// How long the thread may sleep: `None` until a command, `Some(0)` not at all. The music's own
    /// timers, and while it should be moving, a look at whether it does ([`Worker::follow_quiet`]): when
    /// the place stood still at this wake, once it has for [`QUIET_SAY_MS`] and for [`QUIET_HEAL_MS`]; on the CPU with nothing
    /// else to wake the thread, [`QUIET_GUARD_MS`] on, which a burst's wake always comes before.
    fn wake_in(&self, now: i64) -> Option<i64> {
        let d = self.wake_for_music(now);
        let look = match &self.quiet {
            // Looked at once it has stood still for QUIET_SAY_MS (a watching client's break), and again
            // when it is made again.
            Some(q) if q.standing && now - q.since < QUIET_SAY_MS => Some((q.since + QUIET_SAY_MS - now).max(1)),
            Some(q) if q.standing => Some((q.since + QUIET_HEAL_MS - now).max(1)),
            Some(_) if d.is_none() && !self.offloading() => Some(QUIET_GUARD_MS),
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
        if let Some(t) = self.switch_at {
            at(t - now);
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
            // The song playing opening as packets: its loader wakes the thread, this only in case.
            at(1_000);
        }
        if let Some(m) = self.remake.as_ref().filter(|_| self.switch_at.is_none() && self.pause_at.is_none()) {
            // Opened ahead: the dip is due when the ear gets near; still opening, its loader wakes the
            // thread, this only in case.
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
        // A shallow ring (the equalizer tuned) is topped up when half of it is left.
        let low = WAKE_LOW_US.min(self.p.sink.capacity_us / 2);
        if self.p.source_ended() || self.p.sink.reopening() {
            // The end of the queue, or a song in another format the device opens again for: the
            // device's pull says when the last of it has gone.
            track.wake_at(0);
            at((fill + track.latency_us()) / 1000 + 5);
        } else if self.p.starved() {
            // The loader wakes the thread as soon as the bytes are there; this only guards against a
            // loader that never answers.
            at(1_000);
        } else if fill > low {
            track.wake_at(low);
            // The device's pull wakes the thread at the low mark; this is in case its clock runs slow. A
            // device that pulls in bursts leaves the ring standing between them, so a timer from the
            // ring's fill would only wake the thread for nothing once a burst.
            if !track.bursts() {
                at((fill - low) / 1000 + 250);
            }
        } else {
            // The burst's own count (which sees what the device holds too) did not agree yet.
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
            // The next song opening to be looked at: its loader wakes the thread, this only in case.
            at(1_000);
        }
        if let (Some(_), State::Playing) = (self.positions, self.state) {
            at(self.next_position - now);
        }
        d.map(|x| x.max(1))
    }
}
