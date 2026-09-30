//! `nori-cli --script`: a non-interactive player for scripts and renders. Logs in, searches, queues
//! and plays through the core and nori-engine.
//!
//! ```text
//! nori-cli --script --url http://localhost:4533 --user admin --password admin --play "noise" --crossfade 6
//! nori-cli --script ... --search "noise" --songs 2 --start 570 --crossfade 6 --mix-albums --wav out.wav
//! ```
//!
//! Without `--wav` it plays on the sound card and reads commands from stdin (see `HELP`). With `--wav`
//! it renders the queue to a file and exits at the end.

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nori_core::client::{Client, NetProfile};
use nori_core::settings::{GainMode, StoredPrefs};
use nori_core::transport::Transport;
use nori_core::settings_store::{settings_open, settings_put, APPLY_AUDIO, APPLY_GAIN, REPLAN, SOUND};
use nori_core::{Core, Param, ServerConfig, Song};
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue, Downloader, Measurer};
use nori_engine::{AudioOutput, Config, Engine, Event, State, Store, WavOutput};
use nori_http::Http;
use nori_output_cpal::CpalOutput;

use crate::backend::{block_on, Audio};
use crate::text::clock;

const HELP: &str = "play, pause, next, prev, seek <s>, crossfade <s>|off, automix on|off, gain off|track|album|auto, pos, positions on|off, set <name> <value>, tuning on|off, band <i> <dB>, search <q>, queue, quit";

struct Args {
    url: String,
    user: String,
    password: String,
    data: PathBuf,
    search: Option<String>,
    songs: usize,
    start_s: f64,
    crossfade: Option<i32>,
    automix: bool,
    /// Crossfade within an album played in order too (by default albums stay gapless).
    mix_albums: bool,
    replay_gain: Option<GainMode>,
    /// Float output to the device or file.
    hi_res: bool,
    /// Download the songs found before playing them.
    download: bool,
    /// No login and no audio requests; the queue is the downloaded songs.
    offline: bool,
    mpris: bool,
    wav: Option<PathBuf>,
    pace: f64,
    device: Option<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: nori-cli --script --url URL --user USER --password PASSWORD [--search|--play QUERY] [--songs N] [--start SECONDS]\n\
         \x20               [--crossfade SECONDS] [--automix] [--mix-albums] [--replay-gain off|track|album|auto] [--hi-res]\n\
         \x20               [--download] [--offline] [--mpris]\n\
         \x20               [--wav OUT.wav [--pace X]] [--device NAME] [--devices]\n\
         \x20               [--data DIR]\n\
         The url, user and password may also come from NORI_URL, NORI_USER and NORI_PASSWORD."
    );
    std::process::exit(2)
}

fn gain_mode(v: &str) -> Option<GainMode> {
    [("off", GainMode::Off), ("track", GainMode::Track), ("album", GainMode::Album), ("auto", GainMode::Auto)].into_iter().find(|(n, _)| *n == v).map(|(_, m)| m)
}

fn args(argv: Vec<String>) -> Args {
    let env = |k: &str| std::env::var(k).ok();
    let mut a = Args {
        url: env("NORI_URL").unwrap_or_default(),
        user: env("NORI_USER").unwrap_or_default(),
        password: env("NORI_PASSWORD").unwrap_or_default(),
        data: std::env::temp_dir().join("nori-cli"),
        search: None,
        songs: usize::MAX,
        start_s: 0.0,
        crossfade: None,
        automix: false,
        mix_albums: false,
        replay_gain: None,
        hi_res: false,
        download: false,
        offline: false,
        mpris: false,
        wav: None,
        pace: 16.0,
        device: None,
    };
    let mut it = argv.into_iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().unwrap_or_else(|| usage());
        match k.as_str() {
            "--url" => a.url = v(),
            "--user" => a.user = v(),
            "--password" => a.password = v(),
            "--data" => a.data = v().into(),
            "--search" | "--play" => a.search = Some(v()),
            "--songs" => a.songs = v().parse().unwrap_or_else(|_| usage()),
            "--start" => a.start_s = v().parse().unwrap_or_else(|_| usage()),
            "--crossfade" => a.crossfade = Some(v().parse().unwrap_or_else(|_| usage())),
            "--automix" => a.automix = true,
            "--mix-albums" => a.mix_albums = true,
            "--replay-gain" => a.replay_gain = Some(gain_mode(&v()).unwrap_or_else(|| usage())),
            "--hi-res" => a.hi_res = true,
            "--download" => a.download = true,
            "--offline" => a.offline = true,
            "--mpris" => a.mpris = true,
            "--wav" => a.wav = Some(v().into()),
            "--pace" => a.pace = v().parse().unwrap_or_else(|_| usage()),
            "--device" => a.device = Some(v()),
            "--devices" => {
                for d in CpalOutput::devices() {
                    println!("{d}");
                }
                std::process::exit(0)
            }
            _ => usage(),
        }
    }
    if a.url.is_empty() || a.user.is_empty() {
        usage();
    }
    a
}

struct Cli {
    core: Arc<Core>,
    http: Arc<Http>,
    prefs: StoredPrefs,
    engine: Arc<Engine>,
    /// The queue as last set, shared with the event printer and MPRIS for titles.
    songs: Arc<Mutex<Vec<Song>>>,
    downloader: Arc<Downloader>,
}

impl Cli {
    fn search(&self, query: &str) -> Result<Vec<Song>, String> {
        let p = |k: &str, v: &str| Param { key: k.into(), value: v.into() };
        let url = self.core.url("search3".into(), vec![p("query", query), p("songCount", "50"), p("albumCount", "0"), p("artistCount", "0")]);
        let r = block_on(self.http.get(url, 0)).map_err(|e| e.to_string())?;
        let found = self.core.parse_search(r.body).map_err(|e| e.to_string())?;
        // Provider songs (octo-fiesta) are downloaded by the server when requested: never queued from a search.
        Ok(found.songs.into_iter().filter(|s| !s.is_provider()).collect())
    }

    fn queue(&mut self, songs: Vec<Song>, start_ms: i64) {
        nori_core::queue::queue_register(songs.clone());
        nori_core::playlist::playlist_set(songs.iter().map(|s| s.id.clone()).collect(), Some(0), false, None);
        *self.songs.lock().unwrap() = songs;
        self.engine.queue_changed();
        self.engine.play_at(0, start_ms);
    }

    fn download(&self, songs: &[Song]) {
        match self.core.download_queue(songs.to_vec()) {
            Ok(q) => println!("downloading {} ({} asked again)", q.fresh.len(), q.again.len()),
            Err(e) => println!("download failed: {e}"),
        }
        self.downloader.start(self.prefs.parallel_downloads.max(1) as usize);
    }

    /// The stored settings; a device's own sound may have been loaded since the last put.
    fn kept(&self) -> StoredPrefs {
        nori_core::settings_store::settings_current().unwrap_or_else(|| self.prefs.clone())
    }

    fn put(&mut self, prefs: StoredPrefs) {
        self.apply(settings_put(prefs));
    }

    /// Reloads the stored settings and applies `effect` to the engine.
    fn apply(&mut self, effect: u32) {
        self.prefs = self.kept();
        if effect & (APPLY_AUDIO | SOUND) != 0 {
            self.engine.set_settings(settings(&self.prefs, 0.0));
        }
        if effect & APPLY_GAIN != 0 {
            self.engine.gain_changed();
        }
        if effect & REPLAN != 0 {
            self.engine.replan();
        }
    }
}

fn title(songs: &[Song], index: usize) -> String {
    songs.get(index).map_or_else(|| format!("#{index}"), |s| format!("{} - {} ({})", s.artist, s.title, crate::text::duration(s.duration as i64)))
}

fn print_songs(songs: &[Song]) {
    for i in 0..songs.len() {
        println!("  {i}: {}", title(songs, i));
    }
}

pub fn main(argv: Vec<String>) {
    let a = args(argv);
    std::fs::create_dir_all(&a.data).unwrap_or_else(|e| panic!("{}: {e}", a.data.display()));
    let db = a.data.join("nori.db").to_string_lossy().into_owned();
    let core = Core::new(db.clone(), "cli".into()).unwrap_or_else(|e| panic!("the database: {e}"));
    let config = ServerConfig { url: a.url.clone(), user: a.user.clone(), password: a.password.clone(), api_key: None, legacy_auth: false };
    core.configure(config.clone()).unwrap_or_else(|e| panic!("the server: {e}"));
    let http = Http::new();
    let client = Client::new(core.clone(), http.clone());
    client.set_profile(NetProfile { url: a.url.clone(), ..Default::default() });
    if !a.offline {
        if let Err(e) = block_on(nori_core::client::login_check(http.clone(), config, String::new())) {
            eprintln!("login failed: {e}");
            std::process::exit(1);
        }
    }
    let mut prefs = settings_open(db).unwrap_or_else(|e| panic!("the settings: {e}"));
    prefs.crossfade_sec = a.crossfade.unwrap_or(prefs.crossfade_sec);
    prefs.auto_mix = a.automix;
    prefs.crossfade_keep_albums = !a.mix_albums;
    prefs.replay_gain = a.replay_gain.unwrap_or(prefs.replay_gain);
    prefs.hi_res = a.hi_res;
    settings_put(prefs.clone());

    let output: Box<dyn AudioOutput> = match (&a.wav, &a.device) {
        (Some(path), _) if a.hi_res => Box::new(WavOutput::new(path, a.pace).in_float()),
        (Some(path), _) => Box::new(WavOutput::new(path, a.pace)),
        (None, Some(name)) => Box::new(CpalOutput::with_device(name)),
        (None, None) => Box::new(CpalOutput::new()),
    };
    let (tx, events): (_, Receiver<Event>) = channel();
    let store = Store::open(a.data.join("music"), prefs.cache_mb.max(0) as u64 * 1024 * 1024, Box::new(CoreOrder)).unwrap_or_else(|e| panic!("the music directory: {e}"));
    let audio = Arc::new(Audio::new(http.clone(), a.offline));
    let downloader = Downloader::new(core.clone(), client.clone(), audio.clone(), store.clone());
    let app = CoreApp::new().measuring(Measurer::new(core.clone(), client.clone(), store.clone())).per_device(core.clone());
    let library = CoreLibrary { client: client.clone(), bytes: audio.clone(), metered: false, store: Some(store) };
    let engine = Engine::start(library, app, CoreQueue, output, None, Config { memory_mb: 256, settings: settings(&prefs, 0.0), ..Config::default() }, move |e| {
        let _ = tx.send(e);
    });
    let mut cli = Cli { core, http, prefs, engine: Arc::new(engine), songs: Arc::default(), downloader };
    let how = if a.offline { "offline" } else { "logged in" };
    println!("{how} to {} as {}; crossfade {} s, AutoMix {}", a.url, a.user, cli.prefs.crossfade_sec, if cli.prefs.auto_mix { "on" } else { "off" });

    let found = match (&a.search, a.offline) {
        (_, true) => Ok(cli.core.downloads(true).unwrap_or_default()),
        (Some(q), false) => cli.search(q),
        (None, false) => Ok(Vec::new()),
    };
    match found {
        Ok(found) if !found.is_empty() => {
            let found: Vec<Song> = found.into_iter().take(a.songs).collect();
            print_songs(&found);
            if a.download {
                cli.download(&found);
                cli.downloader.wait();
            }
            cli.queue(found, (a.start_s * 1000.0) as i64);
        }
        Ok(_) if a.search.is_some() || a.offline => println!("nothing found"),
        Ok(_) => {}
        Err(e) => println!("search failed: {e}"),
    }

    let songs = cli.songs.clone();
    let title_at = move |i: usize| title(&songs.lock().unwrap(), i);
    if a.wav.is_some() {
        // Render: follow the engine to the end of the queue.
        loop {
            match events.recv_timeout(Duration::from_secs(600)) {
                Ok(Event::Song { index, .. }) => println!("now: {}", title_at(index)),
                Ok(Event::Error { id, message }) => println!("error: {id} {message}"),
                Ok(Event::State(State::Ended)) => break,
                Ok(Event::State(State::Idle)) if cli.songs.lock().unwrap().is_empty() => break,
                Ok(_) => {}
                Err(_) => {
                    println!("timed out");
                    break;
                }
            }
        }
        let s = cli.engine.status();
        println!("ended; underruns {}; audio requests {}", s.underruns, audio.requests.load(Ordering::Relaxed));
        cli.engine.stop();
        return;
    }

    let mpris = a.mpris.then(|| {
        let songs = cli.songs.clone();
        let controls = Arc::new(crate::backend::Controls { engine: cli.engine.clone(), song: Box::new(move |s| s.index.and_then(|i| songs.lock().unwrap().get(i).cloned())) });
        let m = nori_mpris::Mpris::start("nori").map_err(|e| println!("no media controls: {e}")).ok()?;
        m.serve(Some(controls));
        Some(m)
    });
    let mpris = mpris.flatten();
    let shown = title_at.clone();
    std::thread::spawn(move || {
        for e in events {
            if matches!(e, Event::Song { .. } | Event::State(_)) {
                if let Some(m) = &mpris {
                    m.changed();
                }
            }
            match e {
                Event::Song { index, .. } => println!("now: {}", shown(index)),
                Event::State(s) => println!("{s:?}"),
                Event::Position { index, ms } => println!("  {} at {}", shown(index), clock(ms)),
                Event::Error { id, message } => println!("error: {id} {message}"),
                Event::Output { name } => println!("output: {name}"),
                Event::Stopped { .. } => println!("stopped"),
                Event::Buffering(on) => println!("{}", if on { "buffering" } else { "playing again" }),
                Event::Looped { index, .. } => println!("again: {}", shown(index)),
                Event::Title(t) => println!("on air: {t}"),
                // No offline bridge here: the error rules stop playback.
                Event::Bridge { .. } => println!("stopped: the network is gone"),
                Event::Mixing(on) => println!("{}", if on { "mixing" } else { "mixed" }),
                Event::Placed { index, ms } => println!("  {} at {} (another path)", shown(index), clock(ms)),
                Event::Awake(_) => {}
            }
        }
    });
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let (cmd, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        match cmd {
            "play" => {
                cli.engine.play();
            }
            "pause" => cli.engine.pause(),
            "p" | "toggle" => cli.engine.toggle(),
            "next" | "n" => {
                cli.engine.next();
            }
            "prev" | "previous" => {
                cli.engine.previous();
            }
            "seek" => match rest.parse::<f64>() {
                Ok(s) => cli.engine.seek((s * 1000.0) as i64),
                Err(_) => println!("seek <seconds>"),
            },
            "crossfade" => {
                let secs = if rest == "off" { 0 } else { rest.parse().unwrap_or(6) };
                cli.put(StoredPrefs { crossfade_sec: secs, ..cli.kept() });
                println!("crossfade {secs} s");
            }
            "gain" => match gain_mode(rest) {
                Some(m) => {
                    cli.put(StoredPrefs { replay_gain: m, ..cli.kept() });
                    println!("ReplayGain {rest}");
                }
                None => println!("gain off|track|album|auto"),
            },
            "automix" => {
                cli.put(StoredPrefs { auto_mix: rest != "off", ..cli.kept() });
                println!("AutoMix {}", if cli.prefs.auto_mix { "on" } else { "off" });
            }
            "pos" => {
                let s = cli.engine.status();
                let name = s.index.map_or_else(|| "nothing".into(), &title_at);
                println!("{:?}: {} at {}{}; underruns {}", s.state, name, clock(s.position_now()), if s.mixing { " (mixing)" } else { "" }, s.underruns);
            }
            // Any setting by name, as the settings screen sets it: `set eq true`.
            "set" => {
                let (name, value) = rest.split_once(' ').unwrap_or((rest, ""));
                match nori_core::settings_model::setting_set(name.to_string(), value.to_string()) {
                    Some(c) => {
                        cli.apply(c.effect);
                        println!("{name} = {value}");
                    }
                    None => println!("{name}: not a setting"),
                }
            }
            // The equalizer screen's low-latency buffer.
            "tuning" => cli.engine.set_tuning(rest != "off"),
            "band" => {
                let mut it = rest.split_whitespace();
                let (Some(Ok(i)), Some(Ok(db))) = (it.next().map(str::parse::<u32>), it.next().map(str::parse::<f32>)) else {
                    println!("band <index> <dB>");
                    continue;
                };
                let Some(b) = cli.kept().eq_bands.get(i as usize).copied() else {
                    println!("no band {i}");
                    continue;
                };
                if let Some((effect, _)) = nori_core::settings_store::edit_band(i, nori_core::settings::SoundBand { gain_db: db, ..b }) {
                    cli.apply(effect);
                }
            }
            "positions" => cli.engine.position_updates((rest != "off").then(|| Duration::from_secs(1))),
            "search" => match cli.search(rest) {
                Ok(found) if !found.is_empty() => {
                    print_songs(&found);
                    cli.queue(found, 0);
                }
                Ok(_) => println!("nothing found"),
                Err(e) => println!("search failed: {e}"),
            },
            "download" => {
                let songs = cli.songs.lock().unwrap().clone();
                cli.download(&songs);
            }
            "queue" => print_songs(&cli.songs.lock().unwrap()),
            "q" | "quit" | "exit" => break,
            "" => {}
            _ => println!("{HELP}"),
        }
    }
    cli.engine.stop();
}
