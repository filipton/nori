# Writing a nori client

The Rust core (`crates/`) is meant to be reused whole: a desktop app or a terminal client links it and
writes only what is specific to its platform. This page lists what the core already does, and what every
client has to build itself. The Android app (`app/`, `core/`) is the reference for each item.

One rule decides where a piece of work goes, and it outranks the split below: **speed, CPU, memory,
wakeups and battery come first.** Something that measures faster on the client side (animation maths
called every frame, for example) belongs in the client even when it looks like "logic".

## What the core does

Link these and call them; do not reimplement them. `nori-core` holds the `Core` and the `Client` a
platform opens and re-exports every crate below it by its module name (`nori_core::playlist` is
`nori_queue::playlist`), so a client can link `nori-core` alone, or only the crates it needs.

| Area | Crate / module |
|---|---|
| Server API, sync, the one app database (`nori.db`), the settings model and effects | `nori-net`: `api`, `transport`, `requests`, `stream`; `nori-core`: `Core`, `Client`, `cache_policy`; `nori-db`; `nori-settings`: `settings`, `settings_store`, `settings_model`, `lyrics_sources`, `dsp` |
| The records the server sends and the app shows, the core's error and log | `nori-model` |
| Library, search, browse, mixes, smart lists, stats, history, favourites, playlists, M3U | `nori-library`: `library`, `search`, `browse`, `mixes`, `smart`, `history`, `stars`, `m3u` |
| What each page shows, as data: an album's discs, an artist's releases by kind, a page's queue, its big buttons, the letters down a long list, the offer on a provider's page; the song menus; what the listening stats read out (the busiest hour and day) | `nori-library`: `pages`, `menus`, `rows`, `browse`; `nori-queue::actions` |
| The queue: order, shuffle (artists and albums always spread), autofill (taking turns with what it picked or played lately), radio, the offline bridge, error runs | `nori-player::playlist`; `nori-queue`: `playlist`, `queue`, `autofill`, `bridge`, `rules` |
| Decoding MP3, FLAC, AAC-LC, Vorbis, ALAC and Opus, gapless, without allocating per packet; HE-AAC through a decoder the client lends | `nori-player::decode` |
| The sound: EQ, pre-amp, limiter, balance, mono, crossfeed, ReplayGain, speed and pitch, silence skipping | `nori-player::dsp`, `sonic`, `speed`, `silence` |
| Transitions: crossfades, AutoMix analysis, planning and mixing | `nori-player::engine`, `transitions`, `automix`; the analyses kept and the planner the audio path asks: `nori-automix` |
| The playhead, seeks, fades on play/pause/skip, and when the player may sleep | `nori-player::heard`, `seek`, `transport`, `burst` |
| **The whole player** for a platform without one: loading in bursts, demuxing, decoding, the sound chain, transitions, gapless, seeks, fades, the queue walked, events for a screen | `nori-engine` (over `nori-player::pipeline`); with its `core` feature it plays the core's queue, planner and settings |
| A desktop sound card; HTTP on the desktop; the Linux desktop's media controls | `nori-output-cpal`; `nori-http` (the core's `Transport` and the engine's `ByteSource`); `nori-mpris` |
| The stream cache and downloads on disk, for a client without a platform player; measuring the songs ahead for AutoMix on every client (a `Shelf` says where a whole song's files are: Android's is media3's caches) | `nori-engine::store` (`Store`), `nori-engine::core` (`CoreOrder`, `Downloader`, `Measurer`, `Shelf`) |
| "Better beat detection": Beat This! over the ends of the songs coming up, its graph built in and its weights made from the authors' checkpoint, fetched once (a build with the `neural-beats` feature; the Android debug and perf builds have it) | `nori-engine::core::Measurer`; `nori-player::automix::beats`, `neural`, `checkpoint`, `weights`; `nori-core::beat_download`; `nori-automix::beat_model` |
| Output devices: naming, ranking, per-device sound profiles, AutoEQ curves, bit-perfect decisions | `nori-player::outputs`, `device`, `dac`; `nori-devices`: `outputs`, `profiles`, `autoeq` |
| Downloads and stream cache bookkeeping: what is stored, what to fetch next, what to evict | `nori-transfers`: `transfers`, `stream_cache`; `nori-core::cache_policy` (the server's answers kept) |
| Scrobbling decisions; lyrics: the server's, and sixteen lyrics services asked through the `Transport` (requests, matching, every format they answer in, credits stripped, each answer scored, asking them in waves, remembering the answers and the choice in the app's database, the lyrics cache's size and clearing), the current line, backing vocals and duet sides, and when the page redraws | `nori-queue::scrobble`; `nori-lyrics`: `lyrics`, `formats`, `json`, `html`, `lrclib`, `services`, `credits`, `trust`, `sync`, `race`, `look`; `nori-core::race` (`Client::lyrics_lookup`); `nori-settings::lyrics_sources`; `nori-look::lyrics` |
| Why a song will not play, as a kind (`PlaybackError`); the credits (the core's crates, Android's libraries, the typeface and the third parties' data) | `nori-model`; `nori-settings::credits`: `core_credits`, `android_credits`, `data_credits` |
| A perf recorder's bookkeeping: the state a stretch is filed under, what two readings of the counters make (the threads that woke most among them), the stretches kept, their sums by state, the page's figures, the audio output's line and the shared report | `nori-perf::perf_log` |
| Cover colours: palette, theme, the page's colour scheme, the wash and melt behind the player | `nori-look::cover`, `palette`, `theme`, `dress` |
| Cover art: fetched through the `Transport`, kept on disk and in memory, JPEG, PNG, WebP and a GIF's first frame decoded straight to the size drawn and turned as their EXIF says (Android's Bitmaps included) | `nori-covers` |
| Moving album covers: finding an album's motion artwork in Apple Music's catalogue (the search, the web player's token, the square video's address), remembered per album | `nori-core::motion` (`Client::motion_video`, `motion_forget`) |
| The car browse tree | `nori-library::car` |

## The reference terminal client

`crates/cli` (nori-cli) is a whole music client for a terminal, and the check that the list below is
complete: it links the core, nori-engine, nori-output-cpal, nori-http, nori-covers, nori-look and
nori-mpris and writes only its interface. What it built itself, item by item, is what any new client
builds:

- **Screens** (ratatui over crossterm, ui.rs): login and server profiles (`check_login` is the core's
  `Client::login`, profiles are the settings' `servers`), home (the core's album lists), the library
  (albums, artists, playlists from the stored reads; songs from the offline index), search
  (`SearchSession`: the index at every key, the server once typing pauses, `live_search_delay_ms`),
  album, artist and playlist pages (`AlbumDetail`, `ArtistDetail`, `PlaylistDetail` with their captions),
  the queue (`playlist_view` in play order; `playlist_remove`, `playlist_move`, `playlist_shuffle`,
  `set_repeat`), now playing (the song heard is the engine's `Event::Song`/`Status`; how the next comes in
  is the planner's `planner::transition_note`), lyrics (the server's, then `Client::lyrics_lookup`;
  timed by `nori_look::lyrics::LyricClock`, lit by `line_strength` and `UNSUNG`, filled a character at a
  time), downloads (`download_sections`, `Downloader`), the equalizer (the bands as bars; `edit_band`,
  `edit_level`, `settings_sound_tool`) and settings.
- **Settings, its own**: a curated settings screen (settings_view.rs) with the terminal's own groups,
  rows and wording - sound (the equalizer, ReplayGain and its pre-amp, crossfade, AutoMix, speed and
  pitch, skipping silence, bit-exact output), playback, the library and history, lyrics (online lookups,
  the sources in order, their keys), the server, storage and caches, and the client's own (the mouse,
  covers and colours, the volume, the output device for the next start). What only a phone can do is
  simply not there. Each row's options are the core's values (`settings_model::specs`), the value now
  and the rules' facts are `settings_model::state`, and a row changes its setting by name through
  `setting_set`, `edit_level` or the sound tools. What a change asks of the player
  (`SettingChange::effect`) is applied to the engine as Android applies it: `set_settings` for the sound,
  the fades and high quality output, `gain_changed`, `replan`. The client's own few settings (mouse,
  covers, volume) are kept beside the app's (`settings_store::app_value`, `keep_app_value`) and drawn as
  rows of the same kinds.
- **Plays and the queue's end**: `scrobble_playing` and `scrobble_track` on the engine's events (the
  history and the scrobbles are the core's), and `autofill_start`/`autofill_next` with
  `Client::autofill` when the queue runs out. A next pressed at the end is taken when the songs land
  (`autofill_landed`) only within 2 s of the last press; `autofill_skip_waiting` says whether one is
  waiting, for a client that shows it (Android does not yet).
- **Pictures**: nori-covers decodes the cover (fetched through the same `Transport`), nori-look's
  `cover::derive` gives the page, text and accent colours, and ratatui-image draws the picture in
  whichever protocol the terminal answers to (kitty graphics, sixel, iTerm2) or in half blocks. ratatui
  0.30 takes a cell's text width as the columns it covers, so the cell holding a picture's escape
  sequence is marked one column wide (`CellDiffOption::ForcedWidth`), or the rest of the frame is
  skipped. Under tmux, tmux itself is asked first: a tmux that draws sixel keeps the picture in the pane
  and draws it again when the window comes back. Otherwise pictures pass through to the outer terminal,
  which tmux does not keep (one sent while the window is hidden is lost), so the client turns tmux's
  `focus-events` on and, when the pane has focus again, writes the whole screen and every picture again.
- **Input**: one table of key bindings (keys.rs) that both dispatch and the help (`?`) read; every
  clickable place recorded as it is drawn; mouse capture off on request, so the terminal selects text.
- **Cost**: one thread blocked on the terminal, and the screen's loop asleep on one channel for input,
  the engine's events and the workers' answers. It wakes by itself only for the clock's next second while
  music plays and, with the lyrics on screen, when the lyric clock says (`Step::wait`, `still`); paused
  or idle it does not wake at all. Measured on a release build: idle and paused 0 wakeups and 0.00 % CPU
  of its own over 30 s; playing, the screen's thread wakes 1.0 times a second (0.03 to 0.1 % CPU), 5 to
  9 a second with word-by-word lyrics on screen (0.3 to 0.4 %); the engine's thread 0.1 to 3.4 a second.
  The output asks the device for a 100 ms period (nori-output-cpal), so cpal's ALSA thread wakes 11 to 12
  times a second. What still wakes most is PipeWire's data loop, a thread of the ALSA plugin inside the
  process, which runs once per cycle of PipeWire's graph: the graph's quantum is set by whichever client
  asks for the smallest, not by this one (its node asks for 4410 frames at 44.1 kHz). Measured 2026-09-25
  (release `--script`, stdin held open, a 600 s MP3 over PipeWire 1.6, `/proc/<pid>/task/*/status`
  context switches over 20 s, twice): the engine 2.1 to 2.5 a second, cpal's thread 10.9 to 11.7, the data
  loop 188 (another client held the graph at 256 frames at 48 kHz, 187.5 cycles a second), about 202 in
  all and 1.5 % of a core. On a graph left to its default quantum (1024 here, capped at 2048) the data
  loop would wake about 47 times a second, or 23 at the cap (worked out, not measured).
- **Its own words**: times, sizes, decibels, counts, captions and the lyrics' credit are the terminal's,
  in plain English (`text.rs`), from the core's data. The core's are a failure's words
  (`describe_error`) and the queue saved for next time (`playlist_save`, `load_queue`).

## What each client builds

### 1. Talking to the network
- **HTTP transport:** implement the core's `Transport` trait (`crates/net/src/transport.rs`, nori-net), or on a
  desktop link `nori-http`, which implements it (and the engine's `ByteSource`) over ureq. It covers
  TLS, self-signed servers, client certificates and the headers the profile asks for. Ask
  `request_policy` once per host (cache it) to learn which requests go to the server.
  `send` is the same with a third party's own headers and a JSON body (the lyrics services, Apple's
  catalogue); the platform sends it as it is and hands back whatever came, error statuses included.
  Android: `net/Http.kt` on OkHttp. Keep API calls, covers and audio on one HTTP/2 connection, so the
  radio wakes once. Keep a per-read timeout off the audio: OkHttp arms its watchdog thread around every
  read of a body with one, a network packet at a time; Android's audio client has none and finds a
  stalled song by looking at the calls in flight once a minute (`net/Stalls.kt`).
- **Network state:** tell the core when the network changes (metered or not, gone or back), for the offline
  bridge and the "unmetered only" setting. Android: `playback/OfflineBridge.kt`.

### 2. Playing audio
A platform without a player of its own (a desktop app, a terminal client) links `nori-engine` and writes
only what touches the hardware:
- **Output:** implement `nori_engine::AudioOutput` - open a device, and from its own thread call
  `Feed::pull` for every buffer it plays (lock-free, allocation-free), say whether it plays float
  (`takes_float`, for high quality output) and, where the platform can tell, which device the music goes
  to (`watch`) - or use `nori-output-cpal` (PipeWire/ALSA, CoreAudio, WASAPI; device changes on
  PipeWire, CoreAudio and WASAPI; `CpalOutput::volume` is the listener's volume, one multiplication a
  sample after the chain, nothing at all at 100 %). `WavOutput` renders to a file instead (16-bit, or float with
  `in_float`), on a clock of its own. An output whose device holds seconds of music (Android's
  AudioTrack) also runs the fades at its own volume (`ramp`), empties the device when the ring is
  flushed (`flush`, with `Feed::flushed` saying which pull starts the new music) and says what it still
  holds (`holding`), so the music is not over while it plays out.
- **Bytes:** a `ByteSource` (a GET from a byte offset) for streamed songs; `nori-http` has one. Local
  files need nothing.
- **Disk:** a directory for `nori_engine::Store`: the stream cache (written while a song loads, read
  back when it is whole, held to the "space for streamed music" setting in the core's eviction order
  through `CoreOrder`) and downloads (fetched by `core::Downloader` from the core's download queue, with
  its progress and phases in `transfers`). `CoreLibrary` with the store plays a download or a whole
  cached copy from the disk before the network is asked.
- **Driving it:** `Engine::start(CoreLibrary, CoreApp, CoreQueue, output, ..)` with the `core` feature
  plays the core's queue with the core's planner, analysis store, settings (`core::settings`), stream
  addresses, ReplayGain, error run and "skip explicit songs". `CoreApp::measuring(Measurer)` measures the
  songs ahead for AutoMix, and `per_device(core)` gives each output device its own sound. Edit the queue
  through the core's `playlist_*` calls and tell the engine (`queue_changed`); the controls are
  `play_at`, `play`, `pause`, `next`, `previous`, `seek`, `go_to`, `set_settings`, `replan`,
  `set_repeat`, `gain_changed`, `set_tuning` (the equalizer screen: the device told through
  `AudioOutput::shallow` to hold only a fraction of a second, so a band moved is heard without it
  dropping what it holds) and `pause_at_end`
  (the sleep timer's "end of this song"). The player's own rules come with them: a seek or a `go_to`
  while paused is held until play and fetches nothing, and a skip button while paused is a request for
  music (`nori_player::transport::skip_plays`). A screen follows `Event`s (state, the song heard -
  through a mix, the moment the next song is the louder - errors, the output device, a network stall
  heard as buffering, playback stopping by itself, and positions only when asked for) and reads
  `Status` at any time without waking the engine, the sound chain and the limiter's meter included.

The engine does, from the core's decisions, everything a platform player would do around the Rust: it
loads each song in bursts per `load_control` (the whole song in one request when it fits, the next one
fetched as the one before starts, the network left alone in between), demuxes with symphonia's format
readers, decodes with `nori-player::decode`, cuts the encoder's delay and padding so songs join sample
for sample (an MP4's from its `iTunSMPB` or edit list, as media3 reads them), runs the sound chain, speed
and pitch and silence skipping, holds and mixes endings through the transition engine, fades on play,
pause and switches, plays each song at its ReplayGain volume (on its own samples, before any mix), walks
the queue (next, previous, repeat, explicit songs skipped, the error run, a failed connection reported)
and keeps the ear's playhead. High quality output carries float from the decoder to a device that plays
float, 24-bit songs whole, with the sound chain run on the floats; without it the chain runs on 16-bit
samples and dithers what it changes back to 16 bits. A song still on its way is opened off the
engine's thread, and a long pause lets the output and the song's bytes go (the core's idle release) and
opens them again where it was. A change to the sound while music plays (the equalizer, the limiter, speed,
silence skipping, ReplayGain) is heard at once and seamlessly: the sink keeps what the chain was given and
the chain's state every 8192 frames (`nori_player::chain`), goes back to the first frame the output can
still replace, runs the chain again up to it (the same music) and on with the new settings, blended over
5 ms. Nothing is decoded again and no position guessed. Android's deep track, holding seconds, hands the
music to a second track from the frame it plays when that one's silence ends, read off both tracks'
timestamps, crossfaded, and takes it back the same way (crates/android/src/track.rs); a device that can't
drops what it holds and plays on, after a gap, from exactly where it was (`Feed::rewind`). Changes that come quickly are taken together, one every 100 ms
at most. An ending made under an old plan (the queue or the transition settings changed) is made again the
same way from where the old and new endings part. It is `nori-player::pipeline`, the code the simulated player
runs, on one thread that sleeps between bursts (its wakeups are listed in `crates/engine/src/engine.rs`).

The engine also plays what the Android player plays around the sound chain, each off unless asked for:
- **Audio offload** (`offload.rs`): given an output that decodes compressed songs itself
  (`nori_engine::OffloadOutput`, `Engine::start`) and settings with nothing that needs the samples
  (`nori_player::policy`), songs go there as their packets (MP3, AAC-LC, Opus in Ogg pages), joined
  without a gap on one track with each song's delay and padding, the ReplayGain and the fades as its
  volume; the thread sleeps minutes between top-ups. An output without gapless offload still gets a song
  with a delay or padding to cut when no song of its album joins it in order (the rule that keeps albums
  gapless): the few milliseconds are near silence between unrelated songs; an album in order stays on the
  CPU. Taken up where the ear is as soon as nothing needs the samples (a song the output does not decode
  plays on the CPU, and the next one that it does is handed over at its start), given up at once when
  something needs the samples or the output tears the track down.
- **Bit-perfect output** (`Engine::set_output`, `OutputFacts::bit_perfect`): every song decoded to
  float, which carries its 16 or 24 bits exactly, handed to a device opened at its own rate, channels and
  depth (`OutputFormat::bits`), opened again between songs of different formats; no chain, no
  ReplayGain, no conversion.
- **Internet radio** (`Source::Live`, `Loader::live`): an endless stream on one connection, a window
  of it held, the station's ICY announcements taken out of the bytes and said as `Event::Title` when the
  ear reaches them. The client says where a station is (`ByteSource::open_live` asks for the
  announcements). An MP3 station is read frame by frame and each frame checked against the next
  (`mpeg.rs`), so it plays on through noise, a join in the middle of a frame and a next song of another
  rate or channel count (converted to the rate the device was opened at), and a read never looks through
  more than 16 KiB for a frame: the engine's thread never waits on the network for one.
- **HE-AAC** (AAC+ with SBR, v2 with PS; what most AAC radio sends, as ADTS saying AAC-LC at 22.05 or
  24 kHz): symphonia decodes only its core, so what plays is the right pitch with nothing above
  11-12 kHz. For the whole of it **the client lends a decoder**: `nori_player::decode::lend_platform_aac`
  with a function making a `PlatformDecoder` for an `AacSetup` (the stated rate and channels, and the
  AudioSpecificConfig when the container has one). A stream `decode::he_aac` calls HE-AAC (object type 5
  or 29, the SBR extension in its config, or AAC-LC at 24 kHz or less) is then decoded by it, one access
  unit at a time on the engine's thread, and the device is opened at the rate and channels it says.
  Android lends MediaCodec through the NDK (crates/android `mediacodec.rs`). A desktop client lends none
  by default and plays the core: no decoder of SBR in Rust is both free to link and fast enough (the one
  found, oxideav-aac, MIT, decodes at 1.85 times real time), and FDK-AAC's licence and faad2's GPL do not
  suit an MIT client. A desktop client that wants the treble can lend one over the system's libavcodec
  (LGPL, linked at run time, its `aac` decoder has SBR and PS), feeding it each unit with a 7-byte ADTS
  header put in front when there is no config.
- **The offline bridge**: with `CoreApp::bridging`, a song the network would not bring is handed to the
  client as `Event::Bridge` when the core's rules say so; the client asks `Core::bridge_take` and does
  what it answers (below, "What the core decides at each moment").
- **Repeat one** says each time round as `Event::Looped`, for a scrobbler.

- **The network's metered state**: `core::network_metered(client, metered)` tells the core (and through it
  every `CoreLibrary`) whether the network is metered, whenever the platform says it changed, and answers
  the quality songs stream at from then on (the user's setting for that network, transcoded by the
  server). It applies to the next song fetched: the song playing and the one already on its way keep the
  address they were fetched from. Android's `EnginePlayer` tells it from its network callback; a desktop
  client that never does streams the unmetered quality. `CoreLibrary::metered` forces the metered quality.
- **Fetching ahead**: as each song starts (and at a queue edit) the engine fetches the next one and the
  core's `Client::precache_targets` names the ones after it (how many for this network - none on a metered
  one by default - never a provider's song or a download), which nori-engine's one fetcher of the songs
  coming up (`ahead.rs`) fetches whole onto the disk one after another, each in one go, and then leaves the
  network alone until the next song starts: `Store::fetch_ahead` into the engine's own store, or
  `Ahead::ask` over a client's cache (`Keeping`; Android's is media3's stream cache). A song the queue no
  longer wants is left where it got to; one the player is writing is left to it, and one the player comes
  for while it is fetched is handed over where it got to (`Ahead::take_over`): each song crosses the
  network once. With AutoMix on, each is measured as it comes (`arriving.rs`, `core::measure_as_it_comes`),
  on the same bytes and in the same burst, never read back from the disk to be decoded again; so is the
  next song as the engine's loader fetches it (as long as the decoder keeps up; else it is measured from
  the disk) and a download (`Downloader`; Android's `MeasuringSink`).

Not in the engine yet: AutoEQ curves offered for a new device (the core's `DeviceArrival` names one;
the core fetches its curve, `Client::autoeq_curve`, and keeps the list, `Client::autoeq_update`, but
offering it is the client's, as Android's `DeviceSound` does over `Outputs` ), a desktop client's radio stations (the core keeps no station's address; the client's library
says where each is), and symphonia's readers still allocate a buffer per packet (their API has no way to
read into one kept).

Android plays through the engine, and only through it: the ExoPlayer path it was measured against is
gone (2026-09-25; git history has it, should it be wanted again). media3 stays for the session, the
notification, Android Auto, the stream cache and downloads, and ExoPlayer only for the moving covers'
muted video (`MotionPlayer.kt`).
`crates/android/src/track.rs` is the engine's output there: a thread of its own pours the ring into an
AudioTrack whose buffer holds one of the engine's bursts and a second and a half more, in the
power-saving mode, waking when a second is left and taking everything the ring has, so the engine is
woken in the same moment - both about every ten seconds while music plays; the engine keeps no timer of its own for the ring then (`AudioOutput::bursts`). Fades
run at the track's volume and a flush empties the track, since seconds of music sit in it
(`AudioOutput::ramp`, `flush`, `holding`); a track that dies is opened again, and one that will not
open is the engine's to hear of (`AudioOutput::failed`). A song's address and cache key are the core's
(`stream::resolve_now`, over the network state Kotlin tells it, `network_metered`), and its bytes come
through media3's data sources on the app's OkHttp client (`RustBridge.open`), so the profile's TLS,
certificates and headers apply and the downloads, the stream cache and the songs fetched ahead play from
the disk; the queue is the core's (`CoreQueue`, `CoreApp`), and `EnginePlayer` (`RustPlayer.kt`) is a
media3 player over it for the session, the notification, Android Auto and the widget. Offload there is an
AudioTrack opened for it by Kotlin (`RustBridge.openOffload`, the support asked of `AudioManager`, Android
10 and later), written from Rust (`JavaOffload` in player.rs), its stream events handed back through
`RustPlayerJni.offloadEvent`; bit-perfect output opens 16-bit, 24-bit packed or float tracks pinned to the
DAC as `BitPerfect.kt` says; a radio station's address comes from its queue item (`RustPlayerJni.radio`)
and its stream through `RustBridge.openLive`; the offline bridge is `OfflineBridge.kt`. Around the
player the engine fetches the songs after the next into the same stream cache (nori-engine's `Ahead`
through `RustBridge.open`, asking `RustBridge.kept` and `busy`), and the service runs AutoMix's measuring ahead,
each output's sound and the AutoEQ offer for a new device (`Outputs`, `DeviceSound`), the sleep timer,
scrobbling, the notification, Android Auto's tree and resuming after a reboot (`onPlaybackResumption`).
Audio focus, pausing when headphones are pulled out and the CPU wake lock are `EnginePlayer`'s own; a stop after a run of songs that would not play is its player error, in the core's words
(`queue_last_error`).

What a client around the engine still does itself, Android's as the example:
- **Output:** the device's own sound follows the output device (`playback/Outputs.kt`, `DeviceSound.kt`),
  and, where the platform can, the device is opened bit-perfect (`playback/BitPerfect.kt`).
- **Mirroring the queue:** the core owns the queue. A platform player that shows its own list (media3's,
  for the session) routes every edit through the core first and applies the `QueueEdit` it answers
  (the `Controls` forwarding player in `PlaybackService.kt`, over `EnginePlayer`).

Every client schedules downloads on the platform's threads or jobs, when the core says to (`downloads/`);
the songs fetched ahead are nori-engine's (`ahead.rs`). AutoMix's measuring ahead is nori-engine's
`Measurer` (crates/android/src/measure.rs): `AutoMixPrefetch.kt` only says
where a song's files are in media3's caches, and when one has become whole (the caches' own callbacks),
so a song not measured as it came is decoded once, as soon as it is all on the device.
The optional beat model runs in the same measurer: a client builds with the `neural-beats` feature and does
nothing more. The core carries the model's graph; with the switch on it fetches the authors' checkpoint through
the client's transport from `beat_model::CHECKPOINT_URL`, checks it, converts it into the weights file and keeps
that beside the database, the same on every client (`nori-core::beat_download`).

### 3. The operating system around the player
- Media controls and "now playing": the Android media session and notification, MPRIS on Linux
  (`nori-mpris`: a client says what the controls do and when something changed), System Media Transport
  Controls on Windows, MPNowPlayingInfoCenter on macOS.
- Headset buttons, audio focus and ducking, sleep and idle release. The core gives the timings
  (`rules.rs`); the client wires the events.
- Car integration (Android Auto: `PlaybackService.kt` browse), widgets (`app/PlayerWidget.kt`).
- File storage for downloads and the stream cache. Android uses media3's `DownloadManager` and
  `SimpleCache`; a client over `nori-engine` gives `Store` a directory. The core decides what goes in and
  out.

### 4. Pictures
- Draw cover images. Fetching, caching and decoding are `nori-covers`', on every client, Android
  included: a client asks for a cover at a view's size and draws what comes back.
- Hand decoded pixels to `nori-look::cover::derive`, which returns the page's colours and the wash
  picture. Draw them; never recompute them. Android has the core do both in one call (below).

`nori-covers` (`crates/covers`) is platform-free:
- **`Loader`** (`Loader::new(Config, Arc<dyn Transport>)` for RGBA, `Loader::with_paint(.., paint)` for a
  platform's own pictures): `request(url, width, height, done)` answers on one of a few worker threads
  (or at once, from memory) with the cover decoded to fill `width` x `height` the way a cover is drawn
  (the middle kept, the overhang cut); 0 x 0 is the file's own size, at most 2048 a side (a larger file
  is shrunk in its shape; one past 16 MP is not decoded at all). The `Ticket` it returns cancels the
  request when dropped (a row scrolled away); `detach` lets it run on. Views asking for one cover at one
  size while it is on its way share one fetch and one decode, and the newest request is served first, so
  the covers on screen now come before the rows flung past. `warm(url)` fetches onto the disk only,
  behind every view's request (a download's covers); `read(url, bytes)` hands the file itself over on the
  calling thread (a page's colours). `cached(url, w, h)` is the memory cache alone, for drawing at once;
  `load` waits on the calling thread. Workers start with the first requests, at most `Config::workers`,
  and sleep when idle; nothing touches the disk on the thread that asks, not even opening the cache.
- **`Paint`**: what a cover's file becomes, on the worker that fetched it. `Rgba` is tight RGBA rows at
  exactly the size asked for (an `Arc<Image>`); Android's paints a Bitmap (below).
- Addresses are the core's (`cover_url_into`, or `Core.coverUrl`), so every client asks the server for
  the same renditions. Provider covers (`is_provider_cover`) are never written to disk.
- **`DiskCache`**: the server's bytes, one file per address (its MD5) in the directory the client
  names, under `Config::disk_bytes` (the core's `cover_rules`), least recently used out first. The index
  is rebuilt from the directory when it opens, and a read sets the file's modification time, so the order
  survives a restart without being state in the app's database. A file that does not decode is deleted
  and fetched again next time; one in a format the decoder does not know is kept.
- **`MemoryCache`**: decoded covers by address and size, under a limit in bytes, least recently drawn
  out first. `trim_memory` lets them all go. 0 bytes keeps none, for a client that keeps its own.
- **`Decoder`**: `decode_into(bytes, Target { px, width, height, stride }, alpha)` writes RGBA straight
  into the caller's rows (padded rows allowed, as a Bitmap or a texture upload buffer has them), straight
  or premultiplied, turned or mirrored the way the file's EXIF says (JPEG APP1, PNG eXIf, WebP EXIF,
  found by walking the container, nothing decoded). A picture already that size is decoded into them with
  no copy between; anything else is decoded whole into a buffer the decoder keeps and filtered in: an
  exact area average shrinking, bilinear growing, in fixed point, two passes, nothing allocated once the
  buffers have grown. JPEG is zune-jpeg (the fastest pure-Rust decoder, SIMD on x86 and NEON), except
  that a JPEG at least twice the size drawn may go to jpeg-decoder, whose IDCT decodes at 1/2, 1/4 or 1/8
  of the size (what Android's `inSampleSize` does in libjpeg-turbo) - about a third faster on a desktop,
  with the pixels a step or so from the exact average; `set_idct_scaling(false)` decodes those whole
  instead. PNG is the `png` crate, WebP `image-webp`, GIF the `gif` crate: its first frame, on its screen,
  with what the frame leaves uncovered transparent (a cover is a still picture). Pictures over 16384
  pixels a side are refused. `header(bytes)` reads a file's format, its size as shown and its
  orientation from the headers alone; `Header::fill` is the size to decode to for a view (never grown
  past the file's pixels).
- **Not decoded: HEIF/HEIC and AVIF.** HEVC has no decoder in pure Rust. AV1 has one (rav1d), but
  without its assembly (which would need nasm and the NDK's assemblers in the build) it added about
  1.5 MB to the library and decoded a 320 px cover in 6.3 ms and an 800 px one in 38 ms on a desktop,
  six times a JPEG's time, for a format a Subsonic server does not send when it is asked for a size (it
  resizes to JPEG or PNG). Such a cover is `DecodeError::Unknown`: its view keeps its placeholder.

Android has no image library: Coil is gone, and there is no fallback to Android's decoders. The core
fetches (through the app's own `Transport`, so covers ride the API's HTTP/2 connection), keeps the files
on disk (`cacheDir/art`, `cover_rules`' size) and decodes; Kotlin keeps the Bitmaps it is handed and
draws them.

- **The door** (`dev.nori.music.look.CoverPixels`, crates/android/src/covers.rs): `open` makes a loader
  (cheap: the directory is read by its first thread) whose `Paint` decodes each cover straight into a
  Bitmap made at the size `Header::fill` says, from a loader thread attached to the JVM: RGB_565 for a
  JPEG (the core packs it), ARGB_8888 premultiplied otherwise, `setHasAlpha(false)` for a JPEG so drawing
  skips blending it. From Android 9 the screens get hardware Bitmaps, as they did from Coil: the picture
  is decoded into a software Bitmap the thread keeps for the next cover (reconfigured, not made again;
  let go past 512x512) and copied to the GPU there, so no frame pays the upload. `request(loader, url, w,
  h, waiter)` answers a handle; the core calls `waiter.done(bitmap, status)` once, on the loader thread
  that finished it, and `cancel(handle)` (`@FastNative`) drops the core's ticket, after which it is not
  called for a cover finished later (one finished as it is cancelled may still arrive, and Kotlin drops
  it: `cancel` does not wait for a call back under way, which would be a `@FastNative` door waiting on
  Java). `warm` and `clear` are the loader's; `colours` is the page's colours, below. The transport is
  the one the app hands the core (`set_cover_transport`, where `Nori` builds it on the warm-up thread);
  a cover that reaches the network before that waits for it on its loader thread, never on the main one.
- **`CoverLoader`** (core/.../data): the app's one loader, and the Bitmaps' memory cache. That cache
  has to be Kotlin's: a Bitmap is a Java object, and the core holding a reference to every one would keep
  it from the collector without knowing when a view has let it go. It is an LRU by bytes
  (`allocationByteCount`), per cover address, sized by `cover_rules`' share of the memory class as Coil's
  was, trimmed as Coil's was when the system asks. Each address keeps its largest picture, and whether it
  is the file's whole picture: a row's thumbnail is drawn from the grid's larger one rather than decoded
  again, and a view that needs more is shown what is kept while the larger one comes. A request posts
  its call back to the main thread (`Handler.post` of the request itself: nothing else allocated) and
  keeps the picture there. `prefetch` decodes at the file's own size (at most 2048 a side) into memory
  (the library's next screenful, the player's neighbours); `warm` is disk only (a download's covers, hundreds of them, which
  would push the screen's covers out of memory). Provider covers are never kept.
- **`Cover` and `rememberCover`** (app/.../ui): the one component every screen draws a cover with. It
  asks at the view's own pixel size (a view sized by its layout asks once measured), draws the kept
  picture at once, otherwise the plate, its sheen and then the picture faded in over 260 ms; a picture
  that never comes leaves the note glyph, faded in. The picture is drawn in the draw phase, the middle of
  it in the view's shape as `ContentScale.Crop` did, and the fade is read there too, so it recomposes
  nothing. Leaving composition cancels the request. The player's sleeve and its neighbours take the same
  `CoverImage` as a painter.
- **The page's colours** (`CoverLoader.colours`, the door's `colours`): the core reads the cover's file
  (disk or network), decodes the whole picture to fit 320 px in straight colours and hands the pixels to
  `nori_look::cover::derive`, writing the look into an int array and the wash into a Bitmap: no Bitmap
  of the cover in between, and no round trip through premultiplied pixels. One call works out the page
  in the plain theme and on AMOLED black from the one decode, where the bar is black and the player
  keeps the record's colours.

The debug build measures the core's covers (the perf build runs the same from its Performance page,
docs/perf-build.md):

```sh
adb shell am broadcast -a dev.nori.music.TEST --es cmd coverbench --es arg 40
adb logcat -s noritest   # one line when done
```

`coverbench` takes up to that many covers from the disk cache (`cacheDir/art`) and decodes each to
300x300 and 1080x1080 into one reused Bitmap: ARGB_8888 with the IDCT shrinking and without, and RGB_565.
Then it loads up to that many covers the app has shown (the addresses kept in memory) from the disk
through a loader of its own at 300 px, all asked for at once as a screenful is, once into software
Bitmaps and once into hardware ones, so the GPU copy's cost is there to see. For each: total and
per-cover time, the Java heap allocated per cover (`art.gc.bytes-allocated`), GCs, and the Java and
native heaps before and after.

### 5. The interface
- Every screen, its layout and text rendering. Take the page contents from the core as data.
- **All its text.** Every word on screen and every number written for display is the client's: labels,
  confirmations, empty-list notes, captions ("2019 · 12 songs · 48:10"), counts and their plurals, times,
  sizes, speeds, decibels, frequencies, the download notification. The core gives counts, seconds and
  kinds (which message applies: `ResumePlan`, `NoticeKind`, `SummaryTitle`, `BandMark`, `LyricsOrigin`,
  `PlaybackError`, `SongAction`, `MixName`, `ReleaseKind`, `PresetKind`, `DacBlock`, `NetError`), never a
  sentence, so each client says it its own way and can be translated. Android
  keeps its words in string resources (`app/` `strings.xml` and `strings_ui.xml`; `core/` `strings.xml`
  for the notifications, the media session and the player's errors), read through `app/ui/Say.kt`, and
  writes numbers with `core/text/Fmt.kt` (`String.format` in the default locale: Java's rounding and the
  phone's decimal separator; `FmtTest` runs the vectors the Rust copy was checked against). The terminal
  client's are `crates/cli/src/text.rs`. Logs, the perf report and the self test are tooling, in English.
- **Settings**: the core holds the model only (`nori-settings::settings_model`): every setting's name
  (what `setting_set` takes), its kind (a switch, a choice, a level, text, a colour), its options as
  values ("0.75", "320:mp3", "SYSTEM"), its range and default (`setting_specs`); the values now in the same
  form, and what the core's rules make of them - whether the output is played untouched, whether the
  sound chain is on, whether the battery saver stands down, the lyrics services in the order they are
  asked, how the beat model's download stands (`settings_state`); `setting_set` applies a change and says
  what it asks of the player. Sizes on disk and counts are numbers the client reads itself
  (`lyrics_cache_bytes`, `analysis_count`, `index_size`). Which pages, sections and rows a client has,
  their order, when a row is shown, the search over them and every word on them are the client's own:
  Android's are in `app/vm/SettingsPages.kt` with the words in `res/values/strings.xml`; the terminal's
  are in `crates/cli/src/settings_view.rs`. The two are expected to differ.
- **Moving covers** (optional): play the HLS address `motion_video` gives, muted and looping, only while
  the player is on screen and at rest, and let the player go when it is not. Android: `MotionPlayer`
  (core/playback, its own ExoPlayer with media3's HLS module and a 64 MB cache, so a loop is fetched
  once) in a TextureView in the sleeve (app/ui `SleeveMotion.kt`). A video decoder is the platform's.
- **Where things sit on the screen.** The core gives the gradients' stops, the timings and the colours;
  the boxes are the platform's. Android's player sleeve is 0.74 wide for 1 tall and runs 9.5 % of its
  height under the title (`PlayerScreen.kt`), tuned for a phone held upright; the interface is scaled
  to a phone 411 dp wide when Android's display size is set large (`Theme.uiScale`). A desktop window
  lays its player out and sizes its text otherwise.
- **Animations, gestures and transitions.** Their feel is per platform: easing, springs, flick
  thresholds, swipe maths. Android's are in `app/ui/` (`PlayerScreen.kt`, `Chrome.kt`, `Components.kt`),
  described in `docs/motion.md`.
- **A perf recorder** (optional): read the platform's counters (CPU time, every thread's name, CPU time and
  context switches, heap, memory, the battery's counter and gauge, frames, the network bytes, and the audio
  output as the platform describes it against what was asked of it) at each change of state, and hand them
  to `perf_log` (`perf_state`, `perf_stretch`, `perf_log_add`, `perf_page`, `perf_report`). Android's is
  the perf build (`app/src/perf`, docs/perf-build.md).
- Locale: the core writes no number for display and needs no locale; the DAC's modes and rates come
  as numbers (`DacMode`, `DacTrack`) and the client writes "44.1 kHz" itself.
- Drawing cost: redraw only when something visible changes. The Android player draws the seek bar once
  per pixel and the times from pre-laid-out glyphs; a paused player draws nothing.

### What is still English in the core

nori-words went on 2026-09-25 and nori-text with the DAC's notes the same day; every screen's words are
the clients'. What the core hands over instead: the DAC's decision as modes, rates and a `DacBlock`; the
song menu as `SongAction`s, the sleep timer as numbers, a row swipe as a `RowSwipeAct`, a page's download
entry as a `DownloadAct`; `MixName`, `SmartBuiltin` (a user's own name stays data), `ReleaseKind` (with the
server's tag for a kind the app does not know), `AlbumSort`, `LibrarySection`, `SearchScope`, the songs
list's orders by name, `CarFolder` (Android Auto's labels are core/ string resources), `PresetKind`,
`EqBypass`, the band kinds' facts (`EqModel`), `BeatFailure`; an output as its key's parts (`OutputPort` and
the name the device gave), a device row as a port, a name and a `ChoiceKind`; and failures as kinds with
their facts (`NetError::{Transport, Http, Api, Parse, Db}`, `SearchFallback`, `MixLookup::Unknown`,
`SoundError::NoFilters`). Left in English, on purpose:
- output keys ("Phone speaker", "USB: K3", "Wired headphones"): identifiers kept with the settings and the
  profiles bound to them, never shown as they are (`outputs::parts` reads them back);
- the credits' lines (`nori-settings::credits`): licence data, shown on the licences page as given;
- a smart playlist definition's validation errors (`smart.rs`: "match.rules[1].op: ..."): a developer's
  JSON reader, shown in the editor as the core wrote them;
- the album card's subtitle (`nori-model::lines::album_subtitle`, "Artist · 2019"): data joined
  with a separator, no words;
- `Display` of the error enums, the logs, the perf report, the self test and the engine's notes on offload
  and the output (`nori-engine`): tooling, in English.

### 6. What the core decides at each moment
The client notices the moment (a platform event, a key, a callback) and carries out what the core
answers; the rule itself is never written again in a client. Android and nori-cli both call these:
- **A song arrives** (not a repeat-one loop): `rules::song_arrived` answers `SongSteps` in one call - save
  the queue after `save_after_ms`, fetch songs for its end (`fill`: `Client::autofill`, then
  `autofill_arrived`), the offline bridge's `BridgeStep`, measure what is on the device after `precache_after_ms`
  (the fetching ahead itself is the engine's, as the song starts: `Library::ahead`), and `pause_at_end` for the
  sleep timer's last song. It counts the sleep timer and the refill itself, so it is asked once per song.
  Android: `PlaybackService`'s `arrived`; nori-cli: `Session::arrived`.
- **The queue kept and handed over**: `rules::queue_keep(QueueMoment)` says, for a song, an edit, a pause
  and closing, whether to save now or after a delay (`Core::playlist_save`, the platform's own timer,
  restarted by the next moment) and whether to hand the queue to the server (`Client::playlist_push`,
  which sends only when the settings allow). nori-cli keeps its timer on a thread that sleeps until a save
  is due (`Keeper`).
- **A song the network would not bring**: `Core::bridge_take` answers `BridgeTake`: jump to a download
  still queued, apply the bridge's `QueueEdit` and watch for the network, skip, or stop; the run of
  failures is counted there. When `SongSteps::bridge` says `Parked`, `Core::bridge_parked(network_up)`
  brings the parked queue back or puts more downloads in before it; `Idle` stops watching the network.
  Whether the network is up is the platform's (Android's `ConnectivityManager`; nori-cli asks the server).
  Android: `OfflineBridge.kt`; nori-cli: `Session::bridge`.
- **A screen's read**: `Client::read_cached(read, PageShown)` hands the stored answer at once and the
  server's when it differs; a failure is an error only when nothing was stored (Rust: `read_each` with a
  closure). A client offline on purpose hands the client a transport that refuses (nori-cli's `Offline`),
  so the core's offline rule is the only one. Android: `Library.cached`.
- **Lyrics**: `Client::lyrics_for(id, LyricsShown)` is the whole order - the server's (an empty answer
  held back while a service may still have the song), then the services' race, then "none" at the end -
  and hands nothing on twice. Which answer beats the one shown is the race's (`Race::to_show`, over
  trust.rs's scores); a client holding answers across lookups (Android's `SongAnswers`) asks
  `lyrics_same` / `lyrics_replaces` before showing one again. When finer lyrics replace the ones on
  screen, `nori_look::lyrics::matching_line` says which new line stands for the old one. Lyrics come with
  `offset_ms` (the sync check found their times that much late against the song's voice, 0 otherwise): a
  clock made by `lyrics_clock` / `LyricsJni.kept` applies it, and a client timing lines itself makes its
  `LyricClock` with `with_offset` (nori-cli) or adds it to the playhead.
- **A favourite**: `Client::star(kind, id, on, StarsShown)` puts the mark up before asking the server,
  hands the marks over, and puts the one from before back if the server refuses.
- **A download's covers**: `Core::download_cover_urls(cover ids)` gives the addresses to fetch onto the
  disk (both sizes, no provider's, at most 500), for the client's cover loader to `warm`.
  `Core::cover_address` builds any cover's address the same way.
- **The equalizer screen's shallow buffer**: `rules::equalizer_tuning(in_sight, touched, eq_on)`; a
  change counts as touching it when that is true with `touched` true. Whether the screen is in sight is
  the client's.
- **Sizes**: how much each read asks for is `browse::library_sizes` (the sync's page, local search).

## Calling the core cheaply
- `crates/cli` (nori-cli) is a whole terminal client that is only an interface (above); its `--script`
  mode is the few hundred lines of arguments, commands and printing it started as.
- Rust clients call the crates directly. The core (`crates/core`, package `nori-core`, lib
  `nori_core`, over the domain crates nori-model, nori-db, nori-net, nori-library,
  nori-automix, nori-settings, nori-lyrics, nori-devices, nori-queue, nori-transfers and nori-perf) is a
  plain rlib with no JNI in it, and its uniffi exports sit behind the default `ffi` feature: depend on it
  with `default-features = false` and nothing of uniffi is built. Each domain crate has an `ffi` feature
  of its own, off by default, so linking one of them alone builds no uniffi either. Everything the
  Android doors call is ordinary Rust there - `dsp::SoundChain` over sample slices, `heard::HeardClock`,
  `automix::store::AnalysisStream`, `automix::host::CoreHost` for the transition engine, the download
  tracker's functions in `transfers`, their facts lent to a closure.
- Other languages go through uniffi bindings for calls made on user actions. For anything called per
  frame, per buffer or per list row, use a thin native door with primitives in and out and no allocation.
  Keep those doors in a crate of their own, as Android does: `crates/android` builds libnorimusic.so from
  the core (with its uniffi scaffolding) and the JNI doors, which only convert arguments and call the core.
- Android's uniffi bindings are uniffi's JNI generator (`uniffi-bindgen-kotlin-jni`), not its JNA one: a
  JNA call measured 10-25 µs and 1.5-4 KB of garbage in a release-like build, where a JNI door costs about
  8 ns. `crates/android/build.rs` generates the scaffolding into the library, and
  `cargo run -p uniffi-bindgen -- bindings src:nori-android <dir>` writes the Kotlin: nori-core's in
  package `dev.nori.music.ffi`, each domain crate's in its own package below it (`dev.nori.music.ffi.queue`
  and so on, set by the crate's uniffi.toml), since the generator writes one file per crate. A thread the core
  starts may call into Kotlin (a callback, a future it wakes): the runtime (`crates/uniffi-jni-runtime`)
  attaches it once, under the thread's own name (attached without one, the JVM renames it "Thread-NN"),
  detaches it when it ends, and finds the app's classes from it through the class loader
  `JNI_OnLoad` handed over, since `FindClass` on such a thread only sees the system's. crates/android's
  own doors attach their threads the same way (`attached` in lib.rs).
- On Android every door is registered in `JNI_OnLoad` with `RegisterNatives` (none is looked up by a
  `Java_` symbol). A door whose Kotlin signature is primitives only is `@CriticalNative` - its Rust
  function takes no `JNIEnv` and no class, and Android 8 to 11 only honour that for registered methods - and
  a short door over arrays or direct buffers, or one that answers a short string (the equalizer's
  figures, a row's download line, the Rust player's `eventText`), that calls nothing back and waits on no
  lock another thread holds for long is `@FastNative`. Every raw pointer (a direct buffer's address, array elements, Bitmap pixels) is checked
  for null and against its length before a slice is made of it.
- Measure before moving work across the boundary. The Android debug build's `bench` test command prints
  what each kind of crossing costs next to the same work in Kotlin.

## Measured: what stays where

Where a job lives is decided by measurement, never by where it looks like it belongs. Measured on the
Android emulator, non-debuggable build (a debuggable build turns off the fast JNI paths and reads 5-10x
slower), with the debug bridge's `bench` command (the twins' rows: a debug build with `isDebuggable = false`
and temporary doors over each twin, three runs, 200 000 calls each after a warm-up; 20 000 for uniffi):

| Work | Kotlin | Rust (crossing included) | Where it lives |
|---|---|---|---|
| Seek bar step (easing, once-a-pixel pacing) | 14-20 ns | 8 ns (`@CriticalNative`) | Rust (`nori_look::motion`) |
| One cover-colour blend frame, all 52 colours | 42 µs (Compose `lerp`) | 16 µs | Rust (`nori_look::dress::mix`) |
| A time label ("3:07"), made the first time a second is shown, then kept (debug build, emulator, 2026-09-25) | 1.2 µs, 120 B (a char array and the string); from the cache 15 ns, nothing | 0.3-0.5 µs, one 24-byte string, over JNI | Kotlin (`Fmt.duration`, cached per second for the process; the seek bar reads the cache) |
| A small uniffi call | - | 0.1-0.3 µs, no garbage (uniffi's JNI bindings; JNA was 10-25 µs and 1.5-4 KB) | Rust |
| Decoding a packet | MediaCodec: a hop to the codec process, framework buffer objects per packet | in-process, nothing allocated | Rust (`nori_player::decode`) |
| Decoding a cover to 300 px (40 real covers, Galaxy S22, arm64, Android 16) | BitmapFactory as Coil drove it: 1.71 ms | `nori-covers`: 0.93 ms decoded whole (0.98 ms with the IDCT shrinking) | Rust (`nori-covers` fetches, keeps and decodes; Kotlin keeps the Bitmaps and draws) |
| Decoding a cover to 1080 px (the player), same phone | 9.48 ms, native heap up 14-178 MB during the run | 2.80 ms either way, native heap flat | Rust; the pixels are practically BitmapFactory's (mean difference under 0.25/255) |
| Is a cover a provider's (per cover a list draws) | 0.5 µs, 32 B (`any { contains }`) | 0.2 µs, nothing allocated (`@FastNative`) | Rust (`covers::is_provider_cover`) |
| A row's cover address with `Uri.encode` | 0.28-0.39 µs, 186 B | 0.48-0.59 µs, 186 B (`@FastNative`, the prefix kept in Rust) | Kotlin (`Library.coverUrl`); Rust twin |
| A cover-colour cache key (`"$url|$dark|$amoled"`) | 0.13-0.17 µs, 196 B | 0.40-0.47 µs, 196 B | Kotlin (`paletteKey`); Rust twin |
| The soft band's effect key, once a frame | 16-19 ns | 7 ns (`@CriticalNative`) | Kotlin, 3 lines keying a Kotlin cache of RenderEffects; Rust twin |
| Volume step for a slider position, per drag event (then a binder call of tens of µs) | 8-11 ns | 4-5 ns (`@CriticalNative`), 0.13-0.16 µs (uniffi) | Kotlin (2 lines beside `AudioManager`); Rust twin |
| Live search's wait, per keystroke (`isBlank`) | 9-11 ns | 72-81 ns (`@FastNative`), 0.18-0.27 µs (uniffi) | Kotlin; Rust twin |
| A page's songs still to download (on every change of the downloaded set) | 15 songs: 0.3-0.7 µs, 115 B; 300: 8-11 µs, 3 KB | uniffi: 4.6-11 µs, 640 B; 73 µs, 6 KB | Kotlin (`downloadEntry`); Rust twin |
| A car browser's page (`drop`/`take` over 500 items) | 1.3-1.7 µs, 1 KB | uniffi range + `subList`: 0.3-1.1 µs, 200 B | Kotlin (a one-liner on media3's lists, a few times a drive); Rust twin |

Kept in the client (Kotlin on Android), because crossing would cost more than the work:
- **Per-frame gesture and animation maths**: drag offsets, spring and easing values, flick velocity
  tracking, sleeve lift and scale. Each is a few multiplications Compose already does inline (about
  1 ns); a crossing costs 8 ns at best, and the numbers are consumed by Compose on the same frame. The
  decisions a gesture ends in (which way a swipe turns, where the sheet settles, the flick speeds, how
  far a drag must go, the give towards a record that is not there, how far back takes the player, how
  small a held record gets) stay beside it too (Chrome.kt, PlayerScreen.kt, PlayerSheet.kt,
  Components.kt): gestures are mobile UI, and a desktop or terminal client has other input.
- **Drawing**: layout, text measurement, the glyph-cached seek times, bitmaps. The core says what to draw
  and with which colours; the platform draws it. The lyrics' fill with its soft edge, the words' rise and
  a held note's glow (`LyricsView.kt`: `SungText`, `lift`, `glow`) are drawing and per-frame animation
  maths over the word times the record already carries; how far the singing is, when the page redraws
  and the animation's timings are the core's (`LyricsClock`, `stage`). Phone-only layout lives here too, with no twin: the
  sleeve's box (`PlayerScreen.SLEEVE`, `SLEEVE_UNDER_TEXT`) and the interface's scale for a large display
  size (`Theme.uiScale`), both of which the core carried once.
- **Threading of the player**: the engine decodes on its own thread in bursts and sleeps between them;
  the ExoPlayer path's decoder (`RustAudioDecoder`, gone with it) ran synchronously on media3's playback
  thread, because a decoder thread of its own had cost two wakeups a packet (24 wakeups/s screen-off, 5
  after).

Anything moved across the boundary must come with a measurement showing it is at least as fast, and this
table updated.

## Twins: logic Android keeps in Kotlin, and the core has too

Where Android keeps a piece of logic in Kotlin - because it runs on the platform's own objects, or where a
crossing costs more than the work (string building per list row, keys read while drawing) - the core
carries a twin with the same answers, so a desktop or terminal client gets the rule without porting it.
A twin is never deleted for being slower on Android, and nothing a client calls per row, frame or buffer
allocates (`crates/core/tests/twins.rs` and `crates/look/src/no_alloc.rs` count).

Each twin is held to its Kotlin original by test vectors: `tools/twins.sh` compiles the generators in
`crates/<crate>/testdata/twins/*.kt` - the original functions copied as they are, with the Android or
media3 class they lean on written out (AOSP's `Uri.encode`, `MutableTransitionState` as two booleans) -
with the Kotlin compiler the app builds with, runs them on the JVM and writes the `.tsv` beside them;
`crates/<crate>/tests/twins.rs` must match every row exactly (floats go in as their bits). When an original
changes, copy it into its generator again and run `tools/twins.sh`.

Each was measured against its Kotlin original (the table above): where the Rust call is as fast or
faster and the Kotlin is more than a line, Android calls the core and the Kotlin copy is gone. What stays
is a line or two, cheaper in Kotlin at its call rate, or glue around a platform object (a media3 cache, a
Compose transition state, the JNI door's own packing); two lines kept in Kotlin are not worth a door each.

| Kotlin original | Rust twin |
|---|---|
| `Library.coverUrl` with `Uri.encode` (data/Library.kt): per row, Kotlin faster | `covers::cover_url_into` (into a kept buffer) |
| `SearchViewModel`'s live search debounce, `isBlank` | `search::live_delay_ms`, `search::kotlin_whitespace` |
| AutoEQ fetches: `Http.get(..).decodeToString()`, as the app read the index and presets before the core fetched them (`Client::autoeq_update`, `Client::autoeq_curve`, which Android now calls) | `autoeq::fetch_text`, `autoeq::text` (the JVM's UTF-8 repair) |
| `ResizableEvictor.trimLocked` (playback/MediaSources.kt) | `stream_cache::trim` |
| `PlayerConnection.read` | `heard::HeardAt::unpack` |
| `PlayerViewModel.setVolumeFraction`, `volumeFraction` | `rules::volume_step`, `rules::volume_fraction` |
| The car browser's paging (`PlaybackService.onGetChildren`, `onGetSearchResult`) | `car::page` |
| `downloadEntry`'s missing songs (ui/DetailScreens.kt) | `menus::download_missing` |
| `AnimatedRows` (ui/DevicesSection.kt): rows kept, new, leaving in place | `rows::merge_rows` |
| `paletteKey` (ui/CoverColors.kt) | `nori_look::cover::palette_key` (into a kept buffer) |
| `BandEffect.of`'s key (ui/PlayerScreen.kt) | `nori_look::sleeve::band_key` |

No longer twins, Android calling the core instead: `Covers.isProvider` (`covers::is_provider_cover`, through
`CoverPixels.isProvider`) and the precacher's list (`rules::precache_list`, through `Client::precache_targets`).
Gone with the ExoPlayer path, their twins too: `RustAudioDecoder.setupData`, the AAC codec string and
packet buffers (`packets.rs`) and `TransitionSink.configure` (`sink.rs`); and with the retired
`AutoMixPrefetch.onDevice`/`update` (Android measures with nori-engine's `Measurer`), `automix::ahead`.

What stays in the client with no twin:
- **Touch gestures and motion**: sleeve and carousel drags, row swipes, the sheet and queue drags, the
  springs and easings Compose runs, flight transforms, the playing bars and loading dots. How a platform
  moves is its own; a desktop or terminal client has other input and its own animation.
- **Glue around platform objects**: media3's session player (`EnginePlayer`, `Controls`), OkHttp (the TLS setup, exception kinds, the per-host
  cache of `request_policy` answers, which only saves crossings), `ConnectivityManager`, `AudioManager`,
  notifications, SharedPreferences carry-overs, the covers' Bitmap memory cache (its trim levels and
  "each address keeps its largest picture": Bitmaps are Java objects), `EnginePlayer`'s media3 item list
  (a mirror of the core's queue for the session; a client on nori-engine has the queue itself), the
  sorting of media3's error codes and exceptions into the core's kinds (`failureKind`, `isNetworkish`,
  the 5000s as the output), and the uniffi/JNI wrappers that unpack what the core packed (`LyricsClock`),
  or turn what it hands over into a Flow (`Library.lyricsFor` over `LyricsShown`).
- **Video**: the moving cover's player (`MotionPlayer`, media3's ExoPlayer with its HLS module, muted,
  looping, no audio track, focus, session or wake lock) and when it plays, fades and is let go
  (`SleeveMotion.kt`: only on screen, at rest, and never with the screen off). Finding the video is the
  core's.
- **The perf build's counters**: reading `/proc`, `BatteryManager`, `Debug.MemoryInfo`, `TrafficStats`,
  the AudioTrack the player opened (`PlaybackService.track`) and `FrameMetrics`, and counting frames as they
  are drawn. What a stretch is, which threads it names and how it is said are the core's (`perf_log`).
- **The debug build's tools**: the test bridge's verbs (`app/src/debug`: `TestBridge.kt`, `TestActions.kt`, `TestDriver.kt`; release and perf get the empty `app/src/noTest`) and
  the benchmarks (`app/src/bench`), which measure Android's own crossings and Bitmaps.
- **Drawing**: layout, glyph widths, gradients' brushes, slider widget geometry, the player's sleeve box
  and the interface's scale.
