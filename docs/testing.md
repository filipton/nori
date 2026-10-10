# Testing

Three tiers, from fastest to slowest. What decides how music plays, the queue, the library, lyrics and
settings is Rust and is tested by `cargo test`, on a virtual clock where time matters. The device checks
cover only the Android glue: what needs an AudioTrack, MediaCodec, media3, the media session and its
notification, audio focus, routing, JNI, the service and a force stop.

| Tier | What | Time | Who runs it |
| --- | --- | --- | --- |
| `cargo test -j4 --workspace` | every Rust test | 56 s (build warm, M4 Pro; [measurements](build-times.md)) | every agent, every change |
| `tools/smoke.sh` | launch, login, play, pause, seek, next, a queue edit, one AutoMix transition, the equalizer tuned in place, offload on and off, the notification's pause and play, a download played offline, no crash or ANR | 56 s on the hardware-rendered arm64 emulator against the local server; 34 checks passed | every agent that touched Android, on its emulator turn |
| `tools/audio-e2e.sh --only …`, `tools/feature-e2e.sh --only …` | the sections of the area a change touched | a section is 10-90 s | the agent that touched it |
| `tools/audio-e2e.sh` in full | every audio device check | 95 s on the arm64 emulator against the local server; 33 checks passed | the coordinator, once per batch, before a perf APK build |
| `tools/feature-e2e.sh` in full | every feature device check | 216 s, 123 passing checks, hardware graphics and isolated local fixtures ([details](build-times.md)) | the coordinator, once per batch, before a perf APK build |

Never `-j` above 4: the machine runs out of memory. For queued host checks, nextest,
Bazel player targets and a pool of hardware-rendered emulators, see [development.md](development.md).

## Desktop UI controls

On Linux and macOS, build with `--features test-control` and pass `--control-socket PATH` to opt in.
Normal builds contain no control server; a test build without the flag starts no listener or thread.
Each instance needs its own socket and data directory. Existing socket paths are never replaced.

```sh
cargo build -j4 -p nori-desktop --features test-control
export NORI_DESKTOP_SOCKET=/path/to/private/test-directory/control.sock
target/debug/nori-desktop --data /path/to/test-data --control-socket "$NORI_DESKTOP_SOCKET"
# From another terminal:
python3 tools/desktop-app.py state
python3 tools/desktop-app.py open fullscreen
python3 tools/desktop-app.py panel queue
python3 tools/desktop-app.py panel none
python3 tools/desktop-app.py do pause
python3 tools/desktop-app.py do play
python3 tools/desktop-app.py do next
```

`open` takes `home`, `search`, `albums`, `artists`, `playlists`, `songs`, `settings`, `equalizer` or
`fullscreen`. `panel` takes `none`, `queue`, `lyrics` or `devices`. `do` takes `play`, `pause`, `toggle`,
`next`, `previous` or `close_fullscreen`. Actions run through existing UI callbacks on Slint's event
loop. The JSON reply reads actual UI state, including playback position, buffering, active animations and error notes;
playback requests complete asynchronously, so wait for subsequent state changes before asserting.
The socket is local and owner-only (0600), and is removed when the app exits normally.

These checks cover desktop presentation and the real audio output. Playback and queue decisions stay
in the Rust virtual-clock tests. Use Xvfb and a private audio sink for unattended checks. Physical
display clicks and captures require the owner's authorization. See [desktop profiling](desktop-performance.md)
for CPU, live heap and GPU measurements.

The desktop GPU regression reads rendered pixels before and after a redraw, checking that wgpu
does not clear Skia's first frame. It needs a real GPU backend, so it runs explicitly rather than
on headless CI: `cargo test -j4 -p nori-desktop layer_pixels_survive_first_sampling_and_redraw -- --ignored`.
This checks the renderer handoff and texture layouts, which the playback virtual clock cannot exercise.

## Running the device checks

```sh
tools/smoke.sh                                  # the real server in ~/.music.pass
NORI_E2E_SERVER=local tools/smoke.sh            # tools/dev-server.sh's Navidrome, through tools/lying-proxy.py
tools/audio-e2e.sh --list                       # the sections
tools/audio-e2e.sh --only tuning,restart        # just those
tools/feature-e2e.sh --only lyrics-services     # an opt-in section: never part of a full run
```

All three take `--only <section>[,<section>]` and `--list`, exit non-zero on a failure, print the time
each section starts at, and use `ANDROID_SERIAL` (emulator-5554 unless set). They share
`tools/e2e-lib.sh`: `wait_for <field> <value> <timeout>` polls `tools/app.sh state` until a field reads a
value (`!v` for anything but, `>n` / `<n` for numbers), `wait_until <timeout> <command>` until a command
succeeds, `sounds` / `silent` until the app's own AudioTrack is started or not. There is no fixed sleep
where a condition can be awaited. The two that stay are real durations being measured: 20 s paused in
the background (the system has to treat the app as idle), and the ten-second burst cycle
(`bursts_continue` waits for the next top-up, up to 15 s).

### The local server

`NORI_E2E_SERVER=local` runs everything against `tools/dev-server.sh`: a Navidrome on port 4533 (Docker or
Podman; one already answering on 4533 is used as it is), its music and database in the main checkout's
`.dev/`, shared by every worktree. It seeds what the checks need beside its generated albums:

- `Nori E2E / Long Album`: twelve songs of 80-135 s, every third one FLAC (the bridge, album pages, AutoMix).
- `Nori E2E Two / Far Side`: three songs of 150-190 s off another album (a crossfade between two albums,
  since an album's own songs join gaplessly).
- `Nori Bench 1000`: a playlist of a thousand songs (the server's own, over again if it has fewer), for
  opening and scrolling a long page: `tools/open-bench.sh <package> "Nori Bench 1000" 10 3` from Library >
  Playlists, on a perf or release build, prints the frames and janky frames of each open and of the flings.

Each emulator logs in with its own `nori-e2e-SERIAL` fixture account. The app talks to
the server through `tools/lying-proxy.py` on port 4534 (`http://10.0.2.2:4534` from the
emulator; port 5556 uses proxy 4536): a transcoded stream is stated a quarter longer than its bytes, the connection closes short, and
a range past the real end is answered 416, as a server answering `estimateContentLength=true` does when
the transcode comes out smaller than estimated. audio-e2e's `transcode` section plays such a song past its
real end. The generated songs have no lyrics anywhere, so the `lyrics` section only runs against the real
server. Other songs come through the proxy at about 4 MB/s, so a download can be seen running beside
another. Navidrome itself states the estimate only on a transcode it has not cached yet (a cached one comes with
its exact length), and nori no longer asks for it (Navidrome cuts a transcode larger than its estimate), so
the proxy makes every transcode overstated, every time. The real server stays the default; never play or stream an `ext-` item on it.

The proxy also stands in for octo-fiesta asked for a provider's song it cannot fetch: `NORI_HANG=id1,id2`
(`NORI_HANG_MODE=headers|body`) at its start, or `curl 'localhost:4534/_hang?ids=id1,id2&mode=body'` at any
time, makes those songs' streams never answer (or answer and never send), and `curl localhost:4534/_hang`
says how many hang now and how many the app closed. What the engine does with such songs is tested in Rust
(crates/engine tests/hung.rs); the device check is only that media3 and OkHttp let go of a request the engine
calls off (the count hanging now falls as the songs are skipped), which only the real stack shows.

## What moved to Rust and what stays on the device

Every check the two device suites had on 2026-09-25 (110 call sites: 41 in audio-e2e, 69 in
feature-e2e). **(a)** is behaviour Rust owns, removed from the device once a Rust test covers it; **(b)**
is Android glue and stays on the device. 50 moved, 60 stay.

### tools/audio-e2e.sh (21 moved, 20 kept)

| Check | Kind | Now |
| --- | --- | --- |
| plays a song | b | audio-e2e `play`, smoke `play` |
| media key pause / resume | b | audio-e2e `transport` |
| resume after 20 s paused in the background | b | audio-e2e `transport` |
| pause and resume with the screen off | b | audio-e2e `transport` |
| still playing after eq / offload on and off | b | audio-e2e `processing`, smoke `eq`, `offload` |
| still playing after limiter / mono / autoMix switched | a | engine.rs, chain.rs |
| the limiter only catches peaks | a | chain.rs, engine.rs |
| a crossfade is planned for the next boundary | a | crossfade.rs |
| the bar walks steadily through the held ending | a | crossfade.rs, paths.rs |
| the sink reaches the mix | b | audio-e2e `crossfade`, smoke `automix` (the mix heard on a real track) |
| the next track arrives in time to be mixed | a | crossfade.rs, engine.rs |
| the ending is not let go for want of it | a | crossfade.rs, gapless.rs |
| the next track plays out of the mix | b | audio-e2e `crossfade`, smoke `automix` |
| a crossfade is planned past the new song too | a | crossfade.rs |
| a scrub into the mix stays to hear the ending; the mix still fires; the next plays out of it (3) | a | crossfade.rs, engine.rs |
| with it off, the planner says so | a | crossfade.rs |
| taking the EQ out is heard at once, no swap deferred | a | engine.rs |
| still playing after the EQ leaves | b | audio-e2e `eq`, smoke `eq` |
| still playing across the boundary | a | engine.rs |
| out of sight, deep buffer back in place; in sight, shallow buffer in place; a change made in place; track never reopened or emptied; still playing (5) | b | audio-e2e `tuning`, smoke `eq` (crates/android track.rs resizes the real AudioTrack) |
| the deep buffer came back without waiting for a boundary | a | engine.rs |
| measuring starts when AutoMix is switched on; the songs coming up are measured (2) | b | audio-e2e `automix` (read out of the media3 cache through measure.rs's JNI) |
| the mix is planned from what was measured | a | automix.rs, core.rs |
| speed runs through the engine's stage; still playing at 1.5x (2) | a | engine.rs |
| 1.5x plays 6 s as ~9 s of song | a | stages.rs |
| pitch alone keeps the pace | a | stages.rs |
| silence skipping runs; still playing while skipping (2) | a | stages.rs, engine.rs |
| next track plays | b | smoke `controls` |
| previous track plays | a | controls.rs, queue |
| a seek while paused after a restart sticks; play resumes from it (2) | b | audio-e2e `restart` (a new service restoring the saved queue) |
| no playback errors | b | audio-e2e `errors`, smoke `crashes` (over the whole run's log now) |
| (new) a transcode stated longer than it is plays past its real end | b | audio-e2e `transcode`, local server only |

### tools/feature-e2e.sh (29 moved, 40 kept)

| Check | Kind | Now |
| --- | --- | --- |
| lyrics arrive for a well-known song | b | feature-e2e `lyrics` (real server only) |
| sweeping only claimed for real word timing | a | crates/lyrics formats.rs, every format's test asserts `synced`/`word_timed` (`lyricsfile`, `ttml_lines_and_voices`, ...) |
| lookups off: only the server's lyrics | a | lyrics_sources.rs (no service under the lookups switch) |
| each lyrics service answers (a report, no check) | - | feature-e2e `lyrics-services`, opt-in: 16 × up to 12 s, the services' health rather than the app's |
| no video player while off; let go when put away (2) | b | feature-e2e `motion` (ExoPlayer, Kotlin) |
| starring reaches the server | a | client.rs |
| the server is told what is playing | a | client.rs, scrobble.rs |
| the notification has a heart and a shuffle; its heart stars the song; it redraws; its shuffle toggles (4) | b | feature-e2e `notification` |
| the notification's star reaches the server | a | client.rs, shown.rs |
| the second tap puts it back | a | stars.rs `star_marks`, shown.rs (the section still puts it back) |
| the pill reads Play / Pause; shuffle lights; stays Pause; a second press turns it off; pill pauses; reads Play; Play picks up (8) | b | feature-e2e `album-page` (taps on the real screen) |
| without drawing a new queue; without restarting it (2) | a | pages.rs, controls.rs |
| (new) only the page a queue was started from answers for it, through edits and a restore; another page sharing the song reads Play | a | queue playlist.rs, core playlist.rs, cli tests.rs |
| a downloaded song plays with the network off | b | smoke `offline` |
| downloads stand in while the server is out of reach; they play; the album comes back with the network (3) | b | feature-e2e `bridge` (the network really going and coming) |
| at the song that could not play | a | core bridge.rs, queue, paths.rs |
| tapping the download notification opens the queue | b | feature-e2e `download-notification` |
| several songs download at once; the notification's own id; picks up after a force stop; the album finishes (4) | b | feature-e2e `downloads` (media3's DownloadManager) |
| the batch reports a speed; an ETA (2) | a | transfers.rs |
| a new playlist reaches the server; the song went in; the check cleans up (3) | a | client.rs |
| adding to the queue grows it; play next grows it (2) | a | queue, controls.rs; smoke `queue` keeps one on the device |
| the favourites tile opens; lists what the server starred; starring adds; unstarring removes; tapping a row plays it; with the mix around it; the mix has songs (7) | b | feature-e2e `foryou` (the screen) |
| a mix stays the same when opened again | a | board.rs |
| a transition is planned at a track boundary (real album) | a | crossfade.rs, automix.rs, engine.rs; smoke `automix` hears one |
| the songs coming up are measured before they are played | b | audio-e2e `automix` (it was checked twice) |
| the queue is carried on past its last song; a whole album when albums are chosen (2) | a | core autofill.rs, queue |
| a next pressed at the queue's end skips once the songs land, only if they land within 2 s of the last press; mashed, it is one skip; songs for an end that moved stay out | a | queue autofill.rs; player queue.rs |
| the playing song's cover comes after fast skips, loads answered out of order, a failed load asked again once | a | covers loader.rs; app CoverFetchTest |
| offload asked on the phone; stands down for USB; the DAC is seen; audio flows to it; bit-perfect engages; says what the track was opened with (6) | b | feature-e2e `dac` |
| a DAC this app cannot feed says why; is not claimed bit-perfect (2) | a | dac.rs |
| a device with a profile gets it on connect; the sound from before comes back without it (2) | b | feature-e2e `device-sound` (the platform's output events) |
| flat turns the equalizer off; on again on the speaker (2) | a | profiles.rs |
| AutoEQ: asking first leaves the sound; yes applies; automatic applies; undo; not switched again (5) | a | device.rs arrival tests (`CurveStep::Offer` / `Apply`), profiles.rs |

### What could not move

- The AudioTrack itself: whether it is started, its bursts, its in-place resize in and out of sight, offload onto
  the chip, a DAC's format. The engine is tested against a simulated track (crates/android track.rs,
  crates/engine paths.rs), which is what the Rust side decides; the platform's answer only a device gives.
- Media keys, the notification's buttons, the media session, the screen off, the background: Android's.
- media3: downloads, its cache (AutoMix measures songs out of it), and a transcode's stated length on a real
  HTTP stack.
- The screens: taps on the album page and the For you pages go through Compose and the ViewModels.
- A force stop: the service restoring the queue, downloads picking up.
- The network going and coming (the bridge) and a notification's intent opening a screen.
- The app updating itself: which release, which APK and whether to say so are nori-core's (update.rs, tested
  there); the download, PackageInstaller's session, its confirmation and "install unknown apps" are Android's.
  A debug build checks by hand with `app.sh do "update 0.3.0"` (it pretends to be 0.3.0, finds the latest
  release newer and shows the banner; a debug build never installs, its button opens the release's page). An
  install in place needs a release build that is older: `./gradlew :app:assembleRelease -PpretendVersion=0.3.9
  -PrustTargets=x86_64`, signed with the release key, installed, then About, Check for updates, Update.
- Remote control and jams: the protocol, a device admitting commands, the jam's roles and requests, the LAN
  door and its proof are nori-remote's, and two cores controlling each other through a relay (and a door), and
  a jam with a host, an admin and a guest, are nori-core's tests/remote.rs, as is the active device: who plays,
  following it with the picker closed and stopping when it goes, its whole queue in pages, a command shown at
  once and corrected by the next state, a transfer's play order, shuffle and repeat, the volume both ways and
  the star. The desktop's and terminal's volume over remote control is nori-host's (remote.rs). What stays on a
  device is Android's: NsdManager announcing and finding doors, the system volume a command sets, an invite (the
  relay's `/nori/jam` page in the browser, or a `nori://jam` link) opening the app as a guest, and the media
  session handed to another device (RemoteDevicePlayer: the notification, the volume keys as a remote volume, the phone's own engine let go), which only media3 and the
  system can show. `tools/feature-e2e.sh --only remote` checks that with the terminal client on this Mac as the
  other device (local server only): the emulator cannot hear the Mac's mDNS, so the door is handed to the app
  (`app.sh remote "found" "<host>|<port>|<txt>"`, read with `dns-sd`), then `remote pick`, `remote mirror`.
  `--only jam` hosts a jam on the emulator against octo-fiesta's real relay (the local one on 5274,
  `NORI_E2E_JAM`) with two guests on this Mac (`tools/jam-guest.py`, the relay's frames in Python): what only
  the device shows is the jam in the player, its queue and the devices sheet, requests arriving live and
  decided by tapping (the accessibility tree's Accept and Refuse), and the accepted song playing; once ended,
  its own old invite opened through the system's intent saying the jam ended and changing nothing, and a new
  one starting at once (which invites are its own and what an ended one does are tests/remote.rs'). Then the
  emulator is a guest of a jam on the relay's server hosted on this Mac (`tools/jam-host.py`) while its own
  profile is the home server's: the invite opening the guest's player, a tap asking, Leave returning home.
  Listening along there is the playback service's own (media3's foreground service and notification, the
  AudioTrack's deep buffer with the screen off, the session's state): the notification saying whose jam it
  is, the music going on in the background with the screen off, a plain guest's pause holding only its
  own listening and play joining again, and an admin's pause, play and skip reaching the host
  (`NORI_JAM_ADMINS=1`). What a guest's engine does with them (holding, joining where the host is, its
  output let go and back) is nori-engine's along.rs; which control reaches where is nori-remote's and
  nori-core's tests/remote.rs.
- The car: the tree, the rows a pick plays, search and spoken requests are nori-core's (car.rs, tested there);
  media3's session, the items Android Auto reads and the pictures the car opens through CarArtProvider are
  Android's. A debug build walks it as a car connects, through a media browser: `app.sh do "car tree home"`,
  `"car search <words>"`, `"car play <row id>"`, `"car voice <words>[|artist|album|playlist|genre|song]"`, each
  row logged under the tag noricar; `adb shell content read --uri content://dev.nori.music.carart/c/<cover id>`
  reads a picture. Android Auto itself (the head unit) is checked on a phone with the Desktop Head Unit.
  The song's cover is opened by the notification, the lock screen and the headphones, each on its own: the
  provider draws it once and serves the file after that (feature-e2e `notification`), since the readers and
  the file descriptor handed to them are Android's.


## cargo test

`cargo test -j4 --workspace` passed 1,129 tests in 72–79 s once built on 2026-10-10;
the first warm measurement reported 54.87 s in test binaries. The older 2026-09-26
baseline was about 1,120 tests in 30 s; its per-binary figures below are historical,
not the current baseline. See [build-times.md](build-times.md) for current timings.
`[profile.test.package.…]` in Cargo.toml
builds nori-player, nori-engine and every dependency at opt-level 2 for `cargo test` only; debug
assertions and overflow checks stay on. The workspace's own crates that are not listed there (nori-core,
the android crate) are unoptimised, so their tests keep their data small.

Test binaries run one after another and the tests inside a binary side by side, so a binary takes as long
as its slowest test or its total over the cores, whichever is more. The binaries that matter:

| Binary | Tests | Time | Its slowest |
| --- | --- | --- | --- |
| crates/engine `--test engine` (engine.rs, paths.rs, radio.rs, tempo.rs, stretch.rs, estimated.rs, hung.rs, silent.rs, transcode.rs) | 144 | 10 s | offload tests of a few ffmpeg songs, 4-7 s each, and next pressed fast through four queues, 6 s |
| crates/player `--test pipeline` | 67 | 4.6 s | levels.rs, 3-4 s each: every kind of transition through two 60 s songs |
| crates/player lib | 262 | 3.6 s | automix/tests.rs, the synthetic songs analysed side by side |
| crates/android lib | 40 | 8 s | track.rs's two tests of the real engine thread on the wall clock, the rapid skips over a phone-like track 6 s |
| crates/engine `--test one_fetch`, `--test core` | 1 each | 1-2 s | one core and one queue per process, so a binary each |
| everything else | about 600 | under 1.3 s a binary | |

### What the output should have played

`crates/engine/tests/common/reference.rs` renders offline what a card should hear when the sound changes
mid-play: the chain's input (a song, or the transition engine's output as a plain run hears it) through
the equalizer, silence skipping and speed as first set, switched at the input frame the engine logged for
each change, spliced in where it logged it and blended as the ring blends. engine.rs's
`*_changes_seamlessly*` tests compare the card with it sample for sample (EQ, compressor, speed, silence
skipping, in a crossfade, in a stretched AutoMix, right after a seek, paused, on a device holding seconds),
so a repeated, lost or clicking stretch fails them; `reference::clicks` finds jumps where no exact
rendering exists.

On Android the handover between two AudioTracks is heard the same way in `crates/android/src/track/air.rs`:
tracks mixed a period at a time with the mixer's volume ramps, presented after the output's latency,
their timestamps read as the output reports its periods (Bluetooth's: none for 400 ms after a start, then
one per packet with milliseconds of jitter, stall-corrected between), fed by a ring whose frames carry
their numbers. It checks that every frame is heard once, with no silence, level or click, the tracks'
alignment and how soon the change is heard, for outputs from 5 to 40 ms periods and Bluetooth, play heads
that don't say what the mixer took (a new track's first mix never counted, counted per Bluetooth packet,
a resampler's look-ahead: the second track is then not lined up, the track emptied instead, a gap and
never a jump), a slider drag, a pause or a jump at any moment of a handover, and a second track that
won't open.

### The host perf report

`tools/perf-host.sh [rev] [runs]` plays minutes of music through the engine on the test's clock, for this
checkout and for another revision (the latest release tag unless named), and prints per minute of music:
the engine's wakes (what keeps a phone's CPU from deep idle), the process's CPU time and its allocations,
for plain playback, the equalizer on, and crossfades. The bench is `crates/engine/tests/perf_bench.rs`, an
ignored test; the script gives an older revision its files. No device: it answers "did a change make the
player wake, work or allocate more", on the machine it runs on, and only a comparison made there means
anything (the allocated bytes include the test card's own record of every sample, alike on both sides). The
battery and deep idle of a real phone stay the owner's `tools/bench.sh`.

### What each crate's tests cover

| Crate | What is checked |
| --- | --- |
| player | the sound chain sample by sample (decoders, ReplayGain, equalizer, limiter, speed, silence skipping), AutoMix's analysis on synthetic songs with a known tempo, key and structure, the planner (never panicking over any stored row: automix/plan_fuzz.rs), the mixer, and the whole player on a simulated output and virtual clock (`sim`, tests/pipeline): gapless joins, crossfades, levels through a mix, controls, the output |
| engine | the player for platforms without one, on the virtual clock of tests/common: loading and the loader (source.rs), fetching ahead (ahead.rs), the stream cache, offload onto a simulated chip (paths.rs), radio, tempo, the place said through a tempo-stretched mix measured against the song heard (stretch.rs), a transcode's estimated length and its 416 (estimated.rs), Navidrome's transcodes played to their end and a cached copy cut short fetched anew (transcode.rs), a player that never plays silent (silent.rs: a panic on its thread, a loader that dies, an output that stops taking music), one fetch per song with AutoMix measuring (one_fetch.rs), transition settings changed while playing (replan.rs), an album kept gapless under AutoMix or a crossfade heard to every sample, the core's planner and measurer included (album.rs), downloads and the core (core.rs) |
| core | the FFI surface over the real SQLite: the index and search, smart playlists, mixes, history, lyrics' race, covers, AutoEQ and device profiles, the Subsonic client against a fake transport (offline writes and their replay, the address in use, login), stream addresses, transfers, the car's tree, the Kotlin twins (tests/twins.rs), a listen kept by its profile (tests/scrobble.rs), whole flows against a Subsonic server kept in memory: log in, index, search, offline changes replayed in order, a radio (tests/scenario.rs); remote control and jams against a relay kept in memory (tests/remote.rs) |
| lyrics | every lyrics format, the services' answers, trust and fitting, the race between services, synced times checked against synthetic sung songs (sync.rs) |
| covers | decoders against Pillow's references, the scaler, the disk and memory caches, the loader's workers |
| look | colours from a cover (the AndroidX palette port), Compose's colour maths, the lyrics and motion layout |
| library, queue, settings, transfers, devices, perf | pages, menus, the queue's rules, settings and their store (tests/stored_format.rs: the stored keys and encodings against a golden copy), the download table, AutoEQ, the perf log's report |
| cli, android, mpris, net, http, db | each client's own glue: the terminal's drawing, the AudioTrack model (track.rs), every JNI door's signature against its Kotlin `external fun` (lib.rs), media controls, requests |
| testdir | the tests' temp directories, and tests/registered.rs: every file under a crate's `tests/` is in a test binary |

The Kotlin unit tests (`./gradlew :core:testDebugUnitTest :app:testDebugUnitTest`, a minute, no device)
cover what the Kotlin keeps: formatting, the media3 error reading, the frame fades, the queue panel's keys, drag and undo (QueueEditsTest), the player's cover across
fast skips (CoverTurnTest, CoverFetchTest: loads answered out of order, cancelled, failed and asked again), and InitOrderTest, which
reads Nori.kt and Downloads.kt and fails when a field a constructor's thread reaches is declared after the
line that starts the thread (the start-up NPE of 2026-09-25).

### Reading a failure

- An assert says what was wrong and with what: expected and actual, the song ids, positions in ms or
  frames, and for the engine the event log (`{:?}` of `rig.events`) or the planner's log (`app.log()`).
  Read the last event before the failure first: `Error`, `Stopped` or `Bridge` there is the cause, the
  assert is the symptom.
- `the engine did not go to sleep` (tests/common) means the engine thread is stuck or spinning, not slow:
  the limit is 120 s of real time.
- A test on the virtual clock that fails only under load has a real thread in it that the clock does not
  see. The clock stands still while the engine waits for bytes (`BYTES_WAIT`, 2 s of real time; 20 ms when
  a server is itself waiting for the clock through `Virtual::wait_until`). Make the thread wait for a
  condition, not for time.
- A test file that is not in any binary never runs: testdir's `every_test_file_is_registered`
  names it. crates/engine has `autotests = false`, so a new file there needs a `[[test]]` or a `#[path]`
  line in tests/main.rs; a new file in crates/player/tests/pipeline needs a `mod` in main.rs.

### Writing one

- No `thread::sleep` as a wait or as proof that something did not happen. Wait for a condition with a
  deadline that panics saying what did not come; prove "nothing more" with a barrier (the next job on a
  single worker, a later warm-up) and `try_recv`.
- On the engine's clock, time moves only when the test moves it (`Rig::run`, `wait_for`, `until`). The
  loader's waits (stalls, tries again after a dropped connection) come from the clock's
  `nori_engine::Waits`, which the virtual clock shortens.
- Songs are made once per binary: `music()` in engine.rs and pipeline/common.rs, `steady()` in levels.rs
  and the ffmpeg encodes in paths.rs and estimated.rs are cached by length and seed.
- Anything process-wide (the core's queue, the active client, the AutoEQ fetch, the watch hook) is either
  its own test binary or held to what the test itself made (the hook's records filtered by the engine's
  thread id).
- A table of cases that each take seconds runs its cases on threads of their own (`each` in
  automix/tests.rs) or is split into one test per row, so they run side by side.

## The iPod

`IosOutput` (crates/ios/src/output.rs) is tested in Rust on a simulated sink: the format the session
grants, the latency sum, shallow switching, a route change, a failed reopen. The session behind the C
controls (crates/ios/src/session.rs) is tested in Rust on `WavOutput`: two downloaded songs through
pause, seek, next, previous, play_at, go_to, repeat, shuffle, the queue edits, the save on background,
and a memory warning. What Rust cannot see is AURemoteIO itself, so the device check — once a session
plays on the iPod — is a sine through the jack with no underrun for a minute at 93 ms, at 10 ms, and
across a Bluetooth route change. `tools/ipod.sh run` shows the process; the core log is the rest.

The doors the screens read are tested in Rust too (`cargo test -p nori-ios`): the queue in play order
split at the song playing, the song menu's lines and codes, each output's sound and the device sheet,
the AutoEQ search, the lyric clock lighting and filling a line and landing a tap, the icons' credit,
a cover handed over without a copy living until the app lets go, and the login form's
advanced fields. What only the device shows is UIKit's part, looked at by eye: the card and queue
sliding under a finger, the word fill drawn with CoreText, the drawn icons, a queue row dragged.

Remote control on the iPod is tested in Rust as well: behind the deep I/O buffer the engine's place is
the one the simulated unit's listener hears (virtual clock), and the session as a device on that unit
called back in real time: a phone's core finds its door, starts a song on it, mirrors a playhead within
20 ms of what was heard, sets its volume and sees the iPod's own. What only the device shows: Bonjour
announcing and finding, the render timestamps' host time, the volume view moving the system volume, and
the lock screen while another device plays.
Battery and CPU are `tools/ipod-bench.sh` (unplugged, SSH over Wi-Fi), never on the host.


## Issue 36 validation

On the M4 Pro arm64 emulator, the updated full feature suite passed 123 checks in
216 s; the full audio suite passed 33 in 95 s. These are local-fixture runs, so
lyrics and physical offload remain unavailable here. Remote uses a freshly built
CLI peer; Jam covers browser links and direct app links, requests, guest/admin
playback and recovery when the host disappears. Missing prerequisites fail rather
than passing through skipped dependent checks. The prior 785 s feature run had
47 failures and is not a healthy performance baseline.

Parallel smoke and feature checks exercised both emulators with separate
accounts and ports. The resource harness tests exclusion, nested calls,
cancellation/release and selecting an available pool device. It does not replace
the device checks. Clippy completed with existing warnings in unchanged Rust
code. The complete Cargo workspace check, Android APK build and actual Bazel
player unit/pipeline execution also passed.
