---
title: Building
description: Building nori's apps and checks, keeping them fast across parallel work, and the perf build.
sidebar:
  order: 3
---

Applies to the nori repository at 0.6. Commands run from the repository's root.

## Quick start

```sh
cargo test -j4 --workspace                                   # every Rust test
./gradlew :app:assembleDebug -PrustTargets=arm64-v8a         # a debug APK for an arm64 emulator or phone
cargo run --release -p nori-desktop                          # the desktop client
```

Android builds need the Android SDK and NDK, the Rust Android targets and `cargo-ndk`. Never run cargo
with more than four jobs: the build machine runs out of memory. The full list of commands is in
[AGENTS.md](../../../AGENTS.md#commands).

## Faster development checks

Debug Android builds use the optimized `android-dev` Rust profile: incremental
compilation, sixteen codegen units and no LTO. Release, perf and preview builds
keep the shipping release profile. The default debug ABI follows the host's
architecture; override with `-PrustTargets=...`. Keep debug APKs out of runtime
performance comparisons; use the perf profile for those.

Install the optional test/build runners on this Mac:

```sh
brew install cargo-nextest bazelisk
```

Use the shared heavy-work queue across worktrees:

```sh
tools/check.sh rust --workspace                 # nextest, four build jobs and four test workers
tools/check.sh rust -p nori-player               # iteration on one crate
tools/check.sh rust-doc --workspace              # nextest does not run doctests
tools/check.sh android                          # one debug APK
python3 tools/with-resource.py build cargo test -j4 --workspace
```

The last command remains the complete pre-commit Rust check. The four-job and
Gradle four-worker limits also apply when commands are called directly, but the
machine-wide queue only applies through these wrappers. These are scheduling
limits, not CPU quotas. Native libraries and generated bindings have Gradle
cacheable outputs, keyed by sources, features, profiles, compiler and NDK
versions and build flags. Binding source inputs list exported crates in
`core/build.gradle.kts`; extend that list when adding an FFI crate.

### Devices

Start existing arm64 AVDs with hardware graphics and four guest CPUs:

```sh
tools/emulator.sh a16 5554
tools/emulator.sh a16b 5556
export NORI_E2E_SERVER=local
export NORI_E2E_DEVICES=emulator-5554,emulator-5556
tools/device-check.sh --install app/build/outputs/apk/debug/app-debug.apk smoke
tools/device-check.sh audio --only tuning,restart
```

Create the second AVD in Android Studio first. A job leases the first available
device through setup, installation, checks and cleanup. Direct smoke/audio/feature
scripts lease their `ANDROID_SERIAL` too. Agent count can exceed device count;
waiting jobs queue. Do not run direct app.sh/adb mutations on a leased device.
Keep only as many emulators running as the workload needs.

Each local device uses its own Navidrome test account, stream-proxy port, CLI
peer, guest logs and failure artifacts. Port 5554 uses proxy 4534; port 5556 uses
4536. The library fixture is shared. An explicit `NORI_E2E_PROXY_PORT` supports
other serials. Jam checks need an up-to-date octo-fiesta relay on localhost:5274
whose `/nori/jam` page offers “Open in nori”. `tools/jam-server.sh` starts one
from the neighbouring octo-fiesta checkout; override with `NORI_OCTO_ROOT`. A missing or outdated prerequisite
fails promptly. Generated fixture songs have no lyrics; test those against the
real server as documented in [Testing](testing.md).

Accessibility checks use the standalone shell helper `tools/UiDump.java`, built
on demand by `tools/ui-dump.sh`. It reads visible nodes without waiting for an
animating player to become idle. Conditions still wait for the expected state.
Failures save XML and screenshots under `build/e2e/SERIAL/failures/`.

### Bazel pilot

Cargo manifests and Cargo.lock remain the dependency source. The pinned Bazel
and rules_rust versions currently cover the player unit and pipeline tests:

```sh
tools/check.sh bazel test //crates/player:unit_tests //crates/player:pipeline
```

The shared disk action cache is `~/.cache/nori/bazel`. Separate worktrees use
separate Bazel output bases and reuse identical compilation and test actions.
Cached passing tests do not execute again; use `--nocache_test_results` when you
need an actual repeat. This pilot does not replace Android Gradle builds, the
workspace check or device suites. The first dependency fetch and analysis are
more expensive than an unchanged cached invocation. A remote cache can be added
with Bazel's `--remote_cache` option when infrastructure is available; remote
execution requires a separate service and is not configured here.

### Measurements

```sh
python3 tools/measure-dev.py --name rust-edit tools/check.sh android
python3 tools/measure-dev.py --name smoke env NORI_E2E_SERVER=local tools/device-check.sh smoke
python3 tools/test-resources.py
```

Pass an executable command, for example `env NORI_E2E_SERVER=local
tools/device-check.sh smoke`. The macOS recorder writes wall time, process-tree
CPU, sampled compiler/JVM/emulator CPU and memory, and whole-machine activity to
`build/development-timings/NAME/`. Emulator and JVM counters include all matching
host processes; unrelated concurrent activity can affect them. 100% CPU means
one core. See [build and test times](#development-build-and-test-times) for measured edits and limitations.

## The perf build

A release build with a recorder in it, for measuring what the app costs on a real phone in real use:
battery, CPU, wakeups, allocations, memory and frames, with no adb and no computer attached. The
results are on a Performance page in settings, and a plain-text report can be shared from there.

It is minified and not debuggable, like a release, so what it measures is what a release costs. It
installs beside the normal app (id `dev.nori.music.perf`, named "nori dev"), with its own sign-in,
settings and downloads.

### Building and installing

```sh
./gradlew :app:assemblePerf -PrustTargets=arm64-v8a            # a phone
./gradlew :app:assemblePerf -PrustTargets=x86_64               # the emulator
```

The APK is `app/build/outputs/apk/perf/app-perf.apk`, signed with the Android debug key. Put it on the
phone either way:

- `adb install -r app/build/outputs/apk/perf/app-perf.apk`, or
- send the file to the phone (a chat to yourself, a cloud drive, a USB cable) and open it there; Android
  asks once to allow installing from that app.

Open nori dev, sign in, and the recorder is already running: it starts with the app's process.
Settings > Performance is the page.

### What it records

The app's life is cut into **stretches**, each spent in one state:

| State | |
|---|---|
| Screen off, playing | the number that matters most: music in the pocket |
| Screen off, paused | should be close to nothing |
| Screen on, playing, player open | the player sheet up |
| Screen on, playing, other page | the app on screen, anywhere but the player |
| Screen on, playing, another app | the phone in use, the music in the background |
| Screen on, paused | |
| Charging | kept apart; left out of every battery figure |

A stretch also ends when a setting that changes the cost changes (equalizer, AutoMix, crossfade,
offload, hi-res, bit-perfect), so every stretch was spent with one set of them; the stretch lists them
in brackets, ending with whether offload was **wanted**: "offload wanted", or "offload not wanted: <the
setting that keeps it off>". Wanted is not given: whether the audio chip really played the music is the
output line's "offload given" or "PCM (<why>)", and the time it did is "offloaded 45 min 00 s of 1 h
00 min (75 %)" on the stretch and, added up, on each state.

A stretch the chip played in also says what it cost the CPU: "wake lock held 12 s of 10 min 00 s
(2 %), nori-engine 0.30 wakeups/s, the chip asked for more 0.25 times/s". The wake lock is the player's
`nori:engine` one, which it lets go while the songs are offloaded and nothing but the platform's word is
due (nori-engine's `Event::Awake`); the engine's wakeups are its thread's own; the chip's asks are the
offloaded track's `onDataRequest`s, the platform's own pace, which the engine's wakes should match.
Summed per state in "Really offloaded" over the stretches that were offloaded at all.

For each stretch:

| Figure | What it is |
|---|---|
| time | how long the stretch lasted (the elapsed-realtime clock, which counts deep sleep) |
| CPU % | the app process's CPU time (utime + stime, `/proc/self/stat`) over the stretch, in % of one core |
| wakeups/s | voluntary context switches of the app's threads per second (`/proc/self/task/*/status`): each is a thread going to sleep and being woken again, which is what keeps the CPU out of deep idle. Summed over the threads alive at both ends, as `tools/bench.sh` does |
| KB/min | Java heap allocated per minute (`art.gc.bytes-allocated`); GCs is the collections run |
| PSS MB | the process's proportional memory at the end of the stretch (`Debug.MemoryInfo`) |
| mAh, mAh/h | charge used by the whole phone, from the battery's own counter (`BATTERY_PROPERTY_CHARGE_COUNTER`); mAh/h is the average current in mA. Phones without the counter show the battery level's drop in % and %/h instead |
| gauge mA | what the fuel gauge said at the two ends (its own average, `CURRENT_AVERAGE`, where it keeps one, else `CURRENT_NOW`): a cross-check, not a measurement |
| °C | battery temperature at the two ends |
| frames, janky | frames the app drew while on screen, and those that missed their deadline (Android 12 on) or took longer than one refresh of the display (before); the slowest frame's time |
| network | bytes the app received and sent over the stretch (`TrafficStats`), where the phone counts them |

Under each stretch, on the page and in the report, two lines say why it cost what it did:

| Line | What it is |
|---|---|
| threads by wakeups | the six threads that woke most over the stretch (`/proc/self/task/*/stat` and `status`): each one's name, wakeups per second and CPU time. A thread that started during the stretch is marked "(new)". The Rust player's threads carry their own names (`nori-engine`, `nori-track`, `nori-load`, `nori-open`, `nori-covers`, `nori-analysis`); Java's are the platform's (`binder:…`, `RenderThread`, `main` is the app's package name) |
| output | the AudioTrack the player last opened, as the platform describes it when the stretch ended, against what was asked of it: the engine, rate, channels and encoding, the buffer in ms (given, of what was asked, and the most it could grow to), the performance mode asked for and given (the player asks for power saving, the platform's deep-buffer output), whether it is offloaded to the audio chip, where it is routed, its underruns since it was made, and whether it was playing |

Under those, the stretch's **timeline**: what happened during it, one timestamped line each, folded on
the page ("12 events") and printed in full in the report. At most 150 per stretch; past that the oldest
go, and the stretch says how many.

| Event | What it says |
|---|---|
| song | the song the ear arrived on: the file as the server has it (suffix, rate, bit depth, bit rate) and where its bytes come from (the download, the stream cache at which quality, or the network) |
| settings | which settings changed, from what to what (the servers and keys only as "changed"); a slider dragged is one event |
| engine | the player service started with an engine, or ended |
| output | an AudioTrack opened or reopened, as the output line says it (format, buffer, mode, offload, route), or let go |
| offload | entered, or left and why (the player's own reason, or the setting that keeps it off) |
| underruns | the output's underrun count grew, with the reading before, between which and this one they first appeared |
| error | a song or the output failed, in the player's words |
| shallow | the output's shallow buffer (the app in sight) on or off; on the Rust engine also the size the track took for the output it plays on and why (`shallow 550 ms for Bluetooth: its latency is 200 ms, its pulls are 200 ms`), and any growth after it ran dry |

**Invariant breaks** head the report (crates/perf/src/invariants.rs says what each one holds to). Two of
them came of the S22's silent classical playlist (2026-09-26), where the player said it played, no output
was open, no song was being fetched and nothing was said anywhere:

| Break | When |
|---|---|
| silent | the engine plays, the place heard has stood still for 5 s with no output open and none of the song's bytes on their way. Said with the engine's own account of where it stood and what media3's stream cache keeps of the song (its metadata length, the spans cached, whole or not, being written or not) |
| panic | a thread of the Rust library panicked, caught or not: its name, the message and where. Every build says it in the log (Android sends a Rust thread's standard error nowhere) |

With every break the app's own latest lines (under the `nori` tag, the core's and the Kotlin's, kept in
memory by nori-model's alog: the last ten minutes', at least 500 and at most 5000) are copied into the database as it happens, the latest three
breaks' worth, and printed before the log tail: logcat's buffer is the whole system's and a codec's chatter
turns it over in minutes, so the tail rarely reached back to the moment something broke.

Whatever the build, the engine does not stay silent: a panic on its thread is said and the song opened again
from scratch, a song that makes it panic again is skipped as one that would not play, and playing with the
place standing still for 10 s and nothing on its way makes the music again from scratch too (the song's
bytes and its stream cache entry let go, a new output), said as an error (crates/engine tests/silent.rs).

A buffer much smaller than asked, or power saving asked and not given, is what makes a writer wake more
often than the design says: the Rust player's writer then tops the track up once per half of what it
holds (and says so in the log), which the thread line shows as `nori-track`'s wakeups.

"By state" adds the stretches of each state up. Battery is the whole phone's, not the app's alone:
the screen, the radio and every other app are in it, which is why a fair test holds them still (below).
CPU, wakeups, allocations and PSS are the app's own.

Stretches are kept in the app's database (`perf_stretches`, crates/perf/src/perf_log.rs) for 14 days,
each with its timeline; the last crash of each kind in `perf_crashes`. "Start fresh" forgets both.
Stretches under 3 seconds are the blinks between two states (the screen going off also stops the
activity) and are dropped. Android reads the counters and counts the frames (`app/src/perf`); which
state a stretch is filed under, what two readings make, the sums by state and every word on the page
and in the report are the core's (`perf_state`, `perf_stretch`, `perf_page`, `perf_report`), so a
desktop recorder files and reports the same way. The stretch under way when the process dies is lost; the page shows it as
"Now" while it lasts.

### What it costs

Nothing ticks. The counters are read only when something happens: the screen goes on or off, the
power is connected or not, playback starts or stops (the service's own broadcast), the app comes to
the front or leaves it, the player opens or closes, a setting above changes, or the Performance page
opens. Each reading takes a few milliseconds (the PSS is most of it), on a thread of its own that
sleeps in its looper in between. With the screen off and music playing the recorder runs only when a
song changes, a moment the player and the service's broadcast already wake for, so it adds **no
wakeups of its own** to the numbers it records. The thread list, the output and the
network bytes are read at the same moments, not in between.

The timeline is told, not looked for: the player service hands the recorder each song, output, error
and tuning change as it happens (`PlaybackObserver`, null in every other build), and the settings flow
each change. A song change wakes the recorder's thread once, as the service's broadcast on every song
already did; it reads the output's underrun count then (a getter) and the song's record, and hands the
core one note. The app's log is read from logcat only when the report is shared or the page's log is
unfolded; the crash buffer once when the app starts, so a crash outlives logcat's buffer.

While the app is on screen, Android hands every frame's timings to that thread
(`Window.addOnFrameMetricsAvailableListener`); the listener is removed when the app leaves the
screen. That is a small cost per frame drawn, in the screen-on stretches only.

The benchmark buttons are real work: their cost lands in the stretch under way.

### Where the memory goes

The phone reports (a Galaxy S22, motion artwork on) show 210-240 MB PSS with the screen off and
280-440 MB with the player open. On the emulator, with the screen off, nori reads 197 MB against 114-138 MB
for the other clients. What holds it, read from the code, and what was done:

| What | Size | Engine | Done |
|---|---|---|---|
| The moving cover's ExoPlayer: media3's default `DefaultLoadControl`, 50 s ahead, looping, up to 1080 px HLS in the Java heap | tens of MB with the player open (50 s at 5-10 Mbit/s is 30-60 MB) | both | `motion_load_control`: 4-8 s, at most 4 MB. The loop comes from the disk cache, so this costs no network |
| Decoded covers (`CoverLoader`, hardware Bitmaps, counted under Graphics) | up to 15 % of the memory class: 29 MB on the emulator (192 MB), 38 MB on a 256 MB phone | both | a quarter stays while no screen is in sight (`cover_rules().hidden_share`). Covers still on the page are held by their views anyway |
| Page colours (`CoverPalette`): 128 entries, each with a 128 px ARGB wash | up to 8 MB (native heap) | both | an LRU capped at 2 MB, about 30 washes |
| The music's buffer (`load_control`): a quarter of the memory class, at most 48 MB, the whole song in one burst | the whole song, played part included | both | kept: one network wake per song is the battery design. The Rust loader could drop what it has played (it needs chunked storage, since `Vec::drain` keeps the allocation), but a seek back then needs the network |
| The Rust engine's ring: 12 s of f32 | 4.2 MB at 44.1 kHz stereo, 9.2 MB at 96 kHz | Rust | kept: storing i16 for a 16-bit device would halve it, but the fades run on the samples as they are pulled |
| The AudioTrack: 11.5 s | 2 MB i16, 4 MB float (shared memory) | Rust | kept (the burst design) |
| SQLite: two connections at the default 2 MB page cache, no mmap | at most 4 MB, filled only by reads | both | kept |
| Lyrics timing (4 sets), the covers' native memory cache (0 on Android), the loader's threads (rest after 20 s) | < 1 MB | both | nothing to do |
| Thread stacks | virtual; only the pages a thread touches count | both | nothing to gain |
| libnorimusic.so (8.0 MB arm64: fat LTO, one codegen unit, stripped, opt-level 3) | file-backed; only the touched pages count, and they are reclaimable | both | kept: `opt-level = "s"` would slow the decoders and the DSP, and `panic = "abort"` would end the app on a panic uniffi now turns into an exception |

`tools/meminfo.sh <label>` prints one line of `dumpsys meminfo` (total PSS, Java heap, native heap, code,
stack, graphics, other), plus the PSS of libnorimusic.so and the thread count. Use it for the
before/after table in each state (cold start, library, a scrolled album grid, player open, lyrics, screen
off). That table is still to be measured.

### What the open player costs

The phone reports (a Galaxy S22 at 120 Hz, Android 16) had the open player at 60-86 % of a core, 400-500
wakeups/s and 190-310 mAh/h, and "other page" at 20 % and 241 wakeups/s. What drew, found on the emulator
with `tools/cost.sh` (below), the test bridge's `states` and simpleperf:

- **Word-synced lyrics** redrew on every display frame while a word moved (the core asks for one frame,
  meaning a sixtieth of a second, and the loop counted display frames: 120 a second on the S22), and every
  one of those frames also re-ran the composition: `active` and `glideMs` were `derivedStateOf` over the
  frame state, so the composition had to check them each frame, found nothing, and cost the main thread
  a recomposition pass per frame anyway. The fill also asked the text layout for the same horizontal
  positions on every frame. Now the loop waits for the time the core asked for, not the number of display
  frames (60 redraws a second at most, the same on a 60 Hz screen); the line lit and its glide are states
  written only when they change; the positions are worked out once per laid-out line.
- **The playing bars** beside the song on an album or playlist page ticked on every display frame for as
  long as the page was open: that is the "other page" cost. They now step about twenty times a second.
- **The moving cover** plays at its own 24 frames a second (Apple's HLS says `FRAME-RATE=24.000`); nothing
  else redraws with it. Its playback thread no longer wakes every 10 ms (`experimentalSetDynamicScheduling`).
  With the music paused it goes on looping (the owner's call, 2026-09-26: held, it stopped mid-motion on an odd
  frame). It costs its 24 frames a second while the player is open and the screen on; the screen's timeout, the
  app going behind or the player closing stops it.
- **The equalizer's shallow buffer** stayed on for as long as the equalizer page stayed composed, and the
  page stays composed under the player: opening the player after moving a band kept a 160 ms AudioTrack
  (and the CPU that feeds it) for the whole time the player was open, and each later switch back reopened
  the output, a gap in the sound. That is the user's stutter while opening and closing the player
  (perf11, 10:10-10:11). Now the page asks for it only while it is in sight (resumed and not covered by
  the player) and after a band has moved, and the service owns the switch: it passes on only real
  changes, and drops it when the app's controller goes.
- Checked and left as they were: the static sleeve, the soft sleeve's blur and the page wash (static, 6 fps
  from the seek bar alone), line-synced lyrics (a glide when a line changes, nothing between), the title
  marquee (two passes, then it rests), MIXING (a fade in and out, nothing between) and the mini player
  (nothing ticks: 0 frames on Home).

Emulator (x86_64, 60 Hz, debug build: interpreted, so the main thread's share is higher than a release
build's), 30 s per state, before and after:

| State | CPU % before | after | wakeups/s before | after | fps before | after | GCs/min before | after |
|---|---|---|---|---|---|---|---|---|
| Player open, static cover | 7.2 | 4.0 | 91 | 249 (a precache running) | 6.2 | 5.6 | 0 | 4 |
| Player open, moving cover, music playing | 33-41 | 41-46 | 1026-1073 | 1020-1069 | 31-33 | 28-33 | 2-4 | 0 |
| Player open, moving cover, music paused | 6.5 | 0 | 123 | 0 | 0 | 0 | 0 | 0 |
| Lyrics, line-synced | 20.9 | 7.6 | 296 | 164 | 15 | 15 | 0 | 0 |
| Lyrics, word-synced | 52.0 | 38-41 | 681 | 596-624 | 64 | 61-63 | 2 | 0-2 |
| Album page, playing row (other page) | 19.2 | 12.6 | 366 | 260 | 36 | 22.5 | 2 | 0 |
| Home, mini player | 0.4 | - | 0 | - | 0 | - | 0 | - |

The emulator's display is 60 Hz, so the lyrics' biggest win is not in the table: on a 120 Hz phone the
word-synced page drew twice as many frames as here, and now draws the same number. The moving cover's cost
on the emulator is its software video path (the goldfish decoder's HwBinder and MediaCodec threads) and
varies by ±10 % from run to run; the phone decodes in hardware. With the player toggled open and shut ten
times on each engine, with AutoMix and the equalizer on (and with the shallow buffer forced on), the
emulator counted no underruns: the gap on the phone was the output reopened, not a starved track.

The shallow buffer's switches on the Rust engine are in place, both ways: the AudioTrack is opened deep
once, in power saving mode, and the app coming in sight only moves the part
of it that may be filled (`AudioTrack.setBufferSizeInFrames`, crates/android track.rs `Writer::resize`); the
engine's ring stays deep (a band moved replaces the ring's music ahead of the track, `nori_player::sink`).
A second track carrying the music meanwhile was tried and dropped: lined up to the frame by play heads and
timestamps, it was still heard as a cut on a Galaxy S22 over Bluetooth at every switch. The trade-off:
- **Made shallow** as the app comes in sight, the track still holds the seconds taken before (up to the
  11.5 s buffer): it takes nothing more until it has played down to its fraction of a second, and a change
  meanwhile is heard where it runs out. After that every band moved is heard ahead of the track's fraction
  of a second, in place. Out of sight a change waits for what the deep track holds to play.
- **Made deep**, the track is filled up from the engine's next burst: the same buffer, mode and ten-second
  wakes as before the app came in sight, so the battery is what it was.
- **Latency while shallow**: the track stays on the output power saving chose when it was built (the deep
  buffer mixer, on a phone that has one; the emulator has only the primary output). A smaller size does not
  move it or change that output's periods, so its own latency (tens of ms on most phones) is added to the
  ring's 40-80 ms and the track's 80-160 ms, where the old shallow track went to the normal mixer. The
  sound server reads no more per period than before; the writer only tops the track up more often. The
  start threshold is kept inside the size (Android 12 on), or a flush while shallow would wait for more
  than the track may take.
- **How shallow is the output's to say.** The first cut shrank the track to 160 ms wherever it played, and
  on a Galaxy S22 with Sony WH-1000XM6 headphones it ran dry about every 300 ms (62 underruns in a few
  minutes, against one on the speaker). The platform lets `setBufferSizeInFrames` go down to 16 frames
  whatever the output needs, where a track opened anew is given at least `getMinBufferSize` for it; and the
  writer's clock counts music from the play head the device presents, so a Bluetooth link's own couple of
  hundred milliseconds counted as the track's: a 160 ms track on it held nothing. The shallow size is now
  topped up while it still holds the output's latency (`getLatency` less the buffer), the least a new track
  there is given and a wake's lateness, with a quarter of that again on top (`shallow_marks`; the speaker
  keeps its 80/160 ms). While shallow the writer also watches the latency it sees (play head against what
  was presented) and `getUnderrunCount`, and grows for either, never shrinking again on that output. On Bluetooth a band
  moved is heard about half a second later, most of it the headphones' own latency.
- Tested on the simulated track (crates/android track.rs, air.rs: every frame heard in order over a jittery
  mixer and a late writer, never reopened or flushed). Over a Bluetooth-like output (bursts of 100-200 ms
  taken at once, 200 ms of latency) the track never runs dry when the output says what it is
  (`tuned_over_bluetooth_...`), and stops within a few underruns when it says nothing.

`tools/cost.sh LABEL [SECONDS]` measures a state as it is on screen: CPU of a core and wakeups from
`/proc/<pid>/task/*` (context switches), frames from `dumpsys gfxinfo`, GCs from the test bridge's `gc`.

### A fair battery test

Battery figures need long stretches: many phones move the charge counter in steps of a few mAh, and
the level in whole percents. For one setting or one build against another:

1. Unplugged, and not charged during the run (a charging stretch is not counted).
2. The same playlist, the same volume, the same output (speaker, the same headphones), the same
   network (Wi-Fi or mobile, the same place), downloaded or streamed the same way.
3. Screen off, 1 to 2 hours per stretch. Don't touch the phone: waking the screen ends the stretch.
4. Three runs of each, and compare the mAh/h of "Screen off, playing". One run can be off by a lot:
   the phone's own background work comes and goes.
5. The normal nori app should not be playing at the same time.

Press "Start fresh" before a series so the table holds only that series, and share the report after
each (the report lists every stretch, with its settings).

### Sharing

"Share report" opens Android's share sheet with the report as plain text: the device, the Android
version, the build (version and commit), the table by state and how much of each state was really
offloaded, the last stretches each with its output and its timeline, the benchmark results if they were
run this session, and at the end the app's own log: the crash buffer (this and earlier runs of the
app) or the copy of it kept at the last start, the last uncaught exception (kept in the app's database
by the recorder's handler as the process died, then handed on to the platform's), and the process's
last 400 log lines (`logcat --pid`), at most 60 000 characters, after the app's own lines of the last
ten minutes (alog's, so a moment the logcat tail has lost is still there). Send it anywhere text goes; the table
is in columns for a monospaced font. The page shows the same log folded at its end.

### Benchmarks

"Run call benchmark" and "Run cover benchmark" run the same code as the debug build's `bench` and
`coverbench` test commands (`app/src/bench/.../Bench.kt`): what a crossing into the core costs by kind
against the same work in Kotlin, and what the core's covers cost: the decode alone, and the whole way to a
software or hardware Bitmap (docs/clients.md). The cover benchmark needs covers on the disk and in memory,
so browse some albums first.

## Development build and test times

Baseline for [issue #36](https://github.com/norifm/nori/issues/36), measured on
2026-10-10 in checkout `2664eca3`: Apple M4 Pro, 14 cores, 24 GB RAM, arm64 macOS,
Cargo 1.99.0-nightly. Existing dependency and build caches were retained; these
are not clean-build measurements. Heavy commands ran sequentially, with Cargo
limited to four jobs, including the Cargo tasks started by Gradle. Each scenario
was measured once, so these are observations rather than benchmark averages.

### Wall time

| Scenario | Command | Time |
| --- | --- | ---: |
| Workspace tests, first run with existing caches | `cargo test -j4 --workspace --timings` | 132.50 s |
| Workspace tests, unchanged repeat | same | 71.96 s |
| Player pipeline, first package-only run | `cargo test -j4 -p nori-player --test pipeline --timings` | 25.56 s |
| Debug APK, first run with existing caches | `./gradlew :app:assembleDebug -PrustTargets=arm64-v8a --profile --console=plain` | 184.47 s |
| Debug APK, unchanged repeat | same | 0.52 s |
| Debug APK after one Kotlin expression edit | same | 2.95 s |
| Player pipeline compilation after one Rust constant edit | `cargo test -j4 -p nori-player --test pipeline --no-run --timings` | 2.03 s |
| Workspace test compilation after that Rust edit | `cargo test -j4 --workspace --no-run --timings` | 15.73 s |
| Debug APK after that Rust edit | Gradle command above | 195.08 s |
| Player pipeline after restoring the Rust source, compilation and tests | `cargo test -j4 -p nori-player --test pipeline` | 4.90 s |
| Player pipeline, unchanged repeat | same | 2.35 s |

The Kotlin edit changed `Say.home` from `r.getString(R.string.home)` to
`r.getString(R.string.home).toString()`. The Rust edit changed the private
`LIMITER_RELEASE_MS` constant in `crates/player/src/sink.rs` from `120.0` to
`121.0`. Both edits were restored byte-for-byte. Rust tests were compiled, not
executed, with the dummy constant. No APK was installed during the dummy-edit
experiment. The experimental APK was generated before the Rust source was
restored; the CPU/e2e follow-up rebuilt the restored source and installed that APK.

The package-only pipeline baseline compiled some dependencies despite the earlier
workspace run: its feature combination differs from the workspace's. Of its
25.56 s, Cargo reported 22.82 s compilation and the tests reported 2.22 s execution.
The workspace Rust-edit compilation followed the targeted compilation, so it
measures the additional workspace command after that step.

### Where the time goes

#### Android

| Gradle task | First APK | Kotlin edit | Rust edit |
| --- | ---: | ---: | ---: |
| `:core:cargoNdkBuild` | 177.13 s | up to date | 194.11 s |
| `:core:uniffiBindgen` | 4.01 s | up to date | 3.74 s |
| `:core:compileDebugKotlin` | 0.58 s, from cache | up to date | up to date |
| `:app:compileDebugKotlin` | 16.11 s | 1.46 s | up to date |
| `:app:packageDebug` | 0.55 s | 0.13 s | 0.23 s |

Tasks overlap; their durations must not be added together. The one-line Rust
change puts almost the entire Android build on the native compilation path.
`core/build.gradle.kts` defaults to the Rust release profile even for debug APKs:
`opt-level = 3`, fat LTO, one codegen unit, and the `neural-beats` feature. The
measurement covers the whole Cargo task, not LTO separately.

Both native compilation and binding generation declare all of `crates/` as an
input. An implementation-only Rust change therefore regenerates bindings even
when the generated Kotlin stays unchanged. In this experiment that added 3.74 s
of overlapping work; it did not cause Kotlin compilation.

#### Rust tests

The first workspace run spent 47.20 s compiling; the unchanged repeat spent
0.92 s compiling. Both passed 1,129 tests, with three existing ignored tests.

On the unchanged repeat, test suites reported 54.87 s of execution in total.
The remaining approximately 16.17 s covers Cargo, process startup, doctest
compilation/execution and other overhead; it was not separately profiled.
Cargo launched doctests for 24 crates.

| Suite | Unchanged repeat |
| --- | ---: |
| Android Rust output tests (`norimusic`) | 14.06 s |
| Engine integration tests (`tests/main.rs`) | 11.19 s |
| iPod Rust tests (`nori_ios`) | 10.45 s |
| Remote integration tests (`tests/remote.rs`) | 5.10 s |
| Covers unit tests | 3.93 s |
| Player unit tests | 3.19 s |
| Player pipeline tests | 2.24 s |

The first workspace compilation's longest units were the desktop test binary
(30.06 s), engine integration binary (21.84 s), and player unit-test binary
(13.19 s). After the dummy edit, the desktop test binary still took 8.73 s of the
15.73 s workspace compilation. These units also overlap.

### Experiments selected from the baseline

1. Correct the feature suite's remote discovery and invite navigation. Its failed
   run spent 709 s in remote/Jam checks, including repeated UI timeouts in Chrome.
2. Compare a supported hardware graphics renderer with the emulator's current
   SwiftShader renderer, keeping the same suites and display settings.
3. Measure an Android development Rust profile with incremental compilation and
   cheaper linking, retaining optimization for the audio path. Native rebuilds
   took 154–195 s. Keep shipping profiles unchanged.
4. Profile individual Android, engine and iPod Rust tests. These three suites
   account for about 36 s of the 55 s reported warm test execution.
5. Compare the workspace command with `cargo test -j4 --workspace --lib --tests`
   to isolate doctest overhead. Keep the full required checks before committing.
6. Narrow binding generation inputs after measuring invalidation from changes to
   unrelated crates and test files.

Clippy, clean builds, physical-device checks, release APKs and iPod builds were
not timed in this baseline.

### CPU and emulator e2e follow-up

Measured sequentially on the same machine with the restored source. The emulator
was `emulator-5554`, AVD `a16`: arm64, four guest CPUs, 2 GB guest RAM, and a
1080 × 2400 display. `SurfaceFlinger` identified its renderer as ANGLE over
SwiftShader: graphics are rendered on the host CPU. Its existing configuration
was retained.

All CPU percentages below use **100% = one host CPU core**, not the whole
14-core machine. CPU was sampled once per second from cumulative process CPU
times; command CPU was also recorded with `/usr/bin/time -l`. Emulator peaks are
one-second observations. Short-lived command processes can be missed by the
sampler, so the Rust command averages use the process-tree counters. Native
build CPU uses sampled compiler counters because Cargo runs under an existing
Gradle daemon and is absent from the Gradle client's process-tree counters. The
Kotlin build uses sampled JVM counters for the same reason; its short duration
makes the one-second averages and peaks coarser than those of the longer runs.

| Workload | Wall time | Command/compiler average CPU | Emulator average CPU | Emulator peak CPU |
| --- | ---: | ---: | ---: | ---: |
| Android native rebuild of restored source | 154.46 s | 97% | 1% | 5% |
| Kotlin expression edit, through APK packaging | 4.74 s | 413% (JVM) | 11% | 28% |
| Workspace test compilation after restoring source | 13.42 s | 389% | 24% | 261% |
| Workspace tests, warm | 79.32 s | 267% | 2% | 23% |
| Smoke, local server | 80.01 s | 16% | 272% | 822% |
| Full audio e2e, local server | 98.97 s | 20% | 138% | 766% |
| Full feature e2e, failed remote/Jam checks | 785.57 s | 9% | 103% | 902% |

Smoke passed 34 checks; audio passed 33. The warm Rust suite passed again. The
local Navidrome, stream proxy, octo-fiesta Jam relay, and release CLI peer already
existed, so generating fixtures and compiling the CLI were not part of these
device-suite measurements. Hardware offload was unavailable on this emulator;
smoke exercised its fallback path. Notification shade controls were unavailable,
so smoke used the script's media-session fallback.

The CPU-measured Kotlin edit appended `.substring(0)` to `Say.home`; it was
restored and the original APK rebuilt afterward. Rust/native tasks remained up
to date. The JVM briefly peaked at 868% CPU during that edit build. The edited
APK was not installed.

The native rebuild spent most of its time near one core. The warm Rust tests
consumed 211.38 CPU-seconds, or 2.67 cores on average. Smoke consumed about
216 emulator CPU-seconds, with brief spikes above eight cores: the emulator
includes the app, Android system processes, UI automation, and software graphics.
This is host development cost, not the app's CPU usage on a physical phone.

The first 20-second screen-off sample averaged 29% emulator CPU, dominated by a
513% spike during the screen-off transition. The settled emulator subsequently
averaged 1% during the native build. Unrelated macOS processes were also busy
throughout the experiment; whole-machine `top` readings include them and are
retained separately. Measurement overhead was not subtracted.

The full feature suite reported 63 passed checks and 47 failures. Its first 76 s
covered 37 passing checks for UI, downloads, offline playback, DAC and device
sound; lyrics were skipped because the local generated songs have none. During
that observed segment the emulator averaged 417% CPU and peaked at 902%.

Remote playback then took 77 s and reported no discovered Mac endpoint, with four
failures. The script reused the existing release CLI executable, dated
2026-10-08; it only builds that peer when the executable is missing. Jam checks
took another 632 s and had 43 failures. A saved accessibility snapshot during
those checks showed `com.android.chrome` in the foreground with the invite URL,
so subsequent checks were looking for nori controls on a browser screen. Guest
logs showed that requests reached the host's pending list. The failed full-run
time is not a healthy passing-suite baseline. Discovery and browser/app handoff
need diagnosis before repeating it as a benchmark; no test or product logic was
changed during this investigation.

The lower 103% emulator average over the full failed feature run is diluted by
long timeout waits. The passing initial segment's 417% average better describes
the CPU cost of active feature checks on this emulator.

| Section | Elapsed time |
| --- | ---: |
| Smoke: AutoMix transition | 29 s |
| Smoke: offload check and unavailable-hardware fallback | 16 s |
| Smoke: notification controls and media-session fallback | 12 s |
| Audio: transport, including the required 20 s background pause | 45 s |
| Audio: crossfade | 27 s |
| Audio: overstated transcode | 11 s |
| Feature: checks before remote/Jam, including initialization | 76 s |
| Feature: remote playback, four failures | 77 s |
| Feature: Jam, 43 failures | 632 s |

Section times come from the scripts' rounded-second markers and include any
setup before the next marker. All per-second samples, section estimates,
process-tree CPU counters and whole-machine `top` logs are in the local artifacts.

### Research: faster iteration and parallel agents

The following research preceded the implementation reported below. Keep Cargo/Gradle as
the baseline while measuring changed-source builds, total CPU-seconds, peak CPU,
memory and passing test results for each alternative.

1. **Use hardware graphics.** This AVD uses SwiftShader, a CPU renderer. Compare
   `-gpu host` with the current renderer using identical smoke/audio checks and
   display settings, and inspect screenshots for rendering errors. Android's
   [acceleration documentation](https://developer.android.com/studio/run/emulator-acceleration)
   describes the supported hardware and software modes. This targets the observed
   emulator spikes directly; it does not establish physical-device app cost.
2. **Give Android iteration its own Rust profile.** Debug APKs currently build
   the native library with shipping fat LTO and one codegen unit. Try an optimized
   development profile with incremental compilation, more codegen units and
   `lto = "off"`, then compare ThinLTO if runtime performance needs it. Preserve
   audio optimization and existing shipping profiles. The Gradle task must learn
   to pass `--profile <name>`; its present release/dev switch is insufficient.
   [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)
   document these settings. The measured 154–195 s task includes compilation
   and linking; LTO's individual contribution has not been isolated.
3. **Repair expensive failed device prerequisites.** Remote/Jam spent 709 s in
   the failing run. Diagnose discovery and invite navigation; once a prerequisite
   fails, report dependent checks as blocked rather than repeating impossible UI
   waits. Keep independent checks and cleanup. This improves failure turnaround;
   it is not evidence that a healthy full suite can save the same 709 s.
4. **Improve whole-task reuse.** Native and binding tasks currently declare all
   crates as inputs and are not cacheable custom Gradle tasks. Model their actual
   inputs, including dependencies, manifests, lockfile, generators, features and
   toolchains, before enabling output caching. Narrow binding invalidation only
   where it preserves correct API regeneration. Gradle's
   [build cache](https://docs.gradle.org/current/userguide/build_cache.html)
   can reuse complete outputs across worktrees. Existing configuration/build
   caches and parallel execution are already enabled.
5. **Bound test scheduling.** Try nextest with a small global concurrency budget
   and heavier tests assigned more slots, comparing it with today's serial
   binary execution. Its
   [test groups](https://github.com/nextest-rs/nextest/blob/main/site/src/docs/configuration/test-groups.md)
   support resource limits. Per-test process startup may offset gains; retain
   doctests separately if using a runner that does not execute them. Profile
   Android/iPod tests before changing optimization or clock handling.

#### Parallel agents

Separate worktrees protect source/output state, but do not create CPU capacity.
Give each worktree its own Gradle build outputs and Cargo target directory; reuse
eligible compiler/task outputs through caches. Sharing one writable Cargo target
directory can serialize builds through Cargo's build-directory lock. Separate
directories can instead duplicate compilation, so measure cache reuse and memory.

The four-job Cargo cap is per command, not per machine: four agents can launch
sixteen compiler jobs. Enforce `build.jobs = 4` for Gradle's Cargo invocations too,
then coordinate heavy commands through a machine-wide queue/budget. Start with
one heavy build or workspace suite at a time; allow cheap targeted checks within
the measured resource budget. `cargo test -j4` limits build jobs, while
`-- --test-threads=N` limits libtest concurrency; these are distinct controls in
[Cargo's test documentation](https://doc.rust-lang.org/cargo/commands/cargo-test.html).
Gradle's [worker limit](https://docs.gradle.org/current/userguide/build_environment.html)
also defaults to processor count. Lower limits can reduce contention/peaks but
increase elapsed time; they are not strict CPU quotas and do not automatically
reduce total work.

Give each emulator an exclusive lease covering installation, setup, the suite
and cleanup. Current scripts force-stop apps, toggle connectivity, clear logcat
and manipulate the foreground UI; they cannot share one emulator concurrently.
They already select a device through `ANDROID_SERIAL`. Separate emulators allow
device parallelism only after isolating host fixtures, ports, accounts, CLI peers
and report paths too. Build a particular APK once, install it on each allocated
device, and queue targeted checks for each revision. Full suites still run once
per integrated batch as required by AGENTS.md. With this 24 GB Mac and the current
software renderer, begin with one device worker; benchmark two after graphics
and failure handling improve. Agent count need not equal emulator count.

#### Bazel

Bazel is a credible option for sharing complete native build and test outputs
across agents and CI. Its
[remote cache](https://bazel.build/remote/caching) reuses outputs for identical
actions; [remote execution](https://bazel.build/remote/rbe) can move cache-miss
work to other machines. These are separate benefits: a cache does not offload a
new compilation, and local Bazel does not remove emulator CPU cost.

This can help beyond the current compiler cache: sccache's
[Rust limitations](https://github.com/mozilla/sccache/blob/main/docs/Rust.md)
exclude incremental compilations and linked artifacts such as the Android
`cdylib`. Do not disable incremental development builds merely to increase cache
eligibility; compare both strategies on actual edits.

My recommendation is a Rust-only Bazel pilot if cross-worktree reuse remains
expensive after the preceding experiments. Use
[rules_rust crate_universe](https://bazelbuild.github.io/rules_rust/crate_universe_bzlmod.html)
to source dependencies from Cargo manifests and lockfile. Measure two worktrees
building identical code, then different edits, with and without a shared cache.
Kotlin rules provide [compiler plugins and workers](https://github.com/bazel-contrib/rules_kotlin),
but moving the whole project would also require Android resources/Compose, NDK,
generated JNI bindings and existing client build scripts to be modeled correctly.
That integration cost is inferred from this repository, not a migration estimate.
The completed Rust pilot and cache measurements are reported below.

### Local measurement artifacts

Logs, Cargo timing reports and Gradle profiles are retained in the ignored
`build/development-timings/2026-10-10/` directory. Open the Cargo HTML reports for
compilation concurrency and the `gradle/profile-*.html` reports for task durations.
The `cpu-e2e/` and `cpu-kotlin/` directories hold the CPU follow-up measurements;
`cpu-e2e/sections.json` records each suite's section timing and emulator CPU.

The Gradle native builds were measured with `CARGO_BUILD_JOBS=4`. To reproduce,
use the commands above with the same caches and temporary edits; restore source
files afterward. An unchanged build alone does not measure the editing loop.


### Implemented improvements

Measured after pulling `73368b10`. Caches remain warm and each result is one
observation; the updated workspace has 1,130 passing tests, three ignored.

| Scenario | Earlier measurement | Updated measurement |
| --- | ---: | ---: |
| Same private Rust constant edit, debug APK | 195.08 s | 7.04 s |
| Kotlin expression edit, APK packaging | 2.95–4.74 s | 2.75 s, no build-cache restoration |
| Complete Cargo workspace tests, unchanged | 79.32 s, 211.38 CPU-seconds | 55.88 s, 169.42 CPU-seconds |
| Nextest workspace tests, unchanged | not measured | 42.59 s, 149.36 CPU-seconds |
| Gradle native and bindings in another worktree | rebuilt before caching | both FROM-CACHE, APK 1 s reported by Gradle |
| Bazel player tests, unchanged | not measured | 0.64 s including invocation; passing tests cached |
| Bazel player tests in a fresh worktree | not measured | 29.41 s, 292 disk-cache hits; passing tests cached |
| Bazel player library and both test binaries after the same Rust edit | not measured | 8.66 s, compilation only |
| Remote section | 80 s, four failures | 27 s, nine passing checks |
| Smoke, one emulator | 80.01 s, 34 passing | 56.38 s, 34 passing |
| Full audio | 98.97 s, 33 passing | 95.21 s, 33 passing |
| Full features | 785.57 s, 47 failures | 216.76 s, 123 passing |

The Kotlin edit used the original `.toString()` expression and disabled the
Gradle build cache for that measurement. Only app Kotlin compiled; native and
binding tasks remained up to date. Sampled JVM CPU averaged 304%, peaking at
308%, versus the earlier 413% average and 868% peak for a different equivalent
expression. The sample covers only two one-second intervals and can miss peaks;
compiler/JIT warmup and cache state also differ. The original source and APK
were restored before installation.

The APK Rust edit is about 28 times faster. The experiment used the same
`LIMITER_RELEASE_MS` edit, restored byte-for-byte before installation. The first
build with the new native profile took 89 s including dependency compilation;
that is a different workload from a warm edit. Debug now retains optimization
but drops shipping fat LTO, increases codegen units and enables incremental
compilation. Its native library is about 31 MB versus 25 MB for release. Runtime
performance comparisons must still use perf/release APKs.

The longest individual tests in the warm nextest run were:

| Test | Execution |
| --- | ---: |
| Engine `mixramp_fades_out_after_seek` | 10.14 s |
| iOS `a_phone_controls_the_ipod_and_mirrors_the_place_heard` | 9.88 s |
| Engine `seek_ahead_of_a_transcode_lands` | 8.13 s |
| Core `a_mirrored_device_is_timed_only_while_it_plays_on_screen` | 5.02 s |
| Engine `mixes_keep_their_length` | 4.69 s |

These run concurrently; their durations cannot be added to infer suite time.

The complete Cargo run includes doctests; nextest does not. Run the separate
`rust-doc` check or the required complete Cargo command before committing.
Optimizing the Android/iOS workspace packages' test code removed expensive
unoptimized audio loops. Test cases and assertions remain unchanged. Nextest
uses four workers, with allocation/neural-memory tests requiring two slots;
weighting every engine/platform test heavily was slower and was discarded.
The warm nextest run overlapped a device section; its process-tree CPU excludes
that emulator. The new complete Cargo run did too. Engine transition/network
and iOS playback tests remain among the slowest; their timing checks remain.

Gradle input declarations distinguish exported binding sources from player
implementation changes and include toolchain, NDK, profile and flag inputs.
A second worktree reused both outputs with no native or generator work. Bazel
uses a pinned Rust toolchain and a shared disk action cache. Its 29-second fresh
worktree result is primarily startup/repository analysis, not Rust compilation.
Cached test results mean **zero test executions** on these unchanged calls;
they are not measurements of faster test execution. A final invocation with
`--nocache_test_results` executed both tests successfully in 6.5 s including the
Bazel command, with two libtest threads per binary. The edited binaries were only
compiled; the edit was restored before that final execution. The pilot covers player
unit and pipeline tests only. No remote build infrastructure was configured.

Device scripts now resolve the current CLI's exact advertised identity, wait for
actual UI conditions with a standalone accessibility helper, handle current Jam
navigation and Chrome app handoff, and fail early on missing prerequisites.
Local accounts, stream proxies, peer sessions and artifacts are isolated per
emulator. Host build and device leases are shared across worktrees. Limits bound
concurrent jobs; they do not set an OS CPU quota. See [faster development checks](#faster-development-checks)
for commands and [Testing](testing.md) for the final device-suite results.

Updated logs are under `build/development-timings/2026-10-10/optimized/`,
`build/development-timings/nextest-warm/` and
`build/development-timings/cargo-final/`. CPU samples are observations of all
matching compiler/JVM/emulator processes and may include concurrent host work.


#### Final hardware-rendered device costs

The final single-emulator smoke run retained the same arm64 AVD, four guest
cores, resolution and local music, with host graphics replacing SwiftShader.
It passed all 34 checks in 56.38 s. Sampled emulator CPU averaged 62.2%, versus
272.5% in the earlier software-rendered smoke. This compares the combined
improvements (hardware graphics, accessibility snapshots, repaired setup and
warm state), not graphics alone. Peak CPU was 242.3%; sampled emulator work was
34.78 CPU-seconds, versus about 216 previously. The second emulator was shut down
before this run. CPU percentages count 100% per host core.

The full audio run passed 33 checks in 95.21 s, averaging 86.6% aggregate emulator
CPU. The full feature run passed 123 checks in 216.76 s and averaged 108.5%.
These included short overlap with other device/host checks; emulator counters
include both devices when both ran. The prior failed feature run spent much of
its time waiting, so its average CPU is not a useful passing-workload comparison.
The new feature duration includes a 6.85 s debug CLI rebuild after the dummy
source experiment. Genuine background/audio durations remain in the suites.

Final artifacts: `build/development-timings/smoke-hardware/`, `audio-final/`,
`feature-passing/`, `kotlin-edit-final/` and `bazel-rust-edit/`. The Bazel command's
process-tree CPU excludes its persistent server; its near-zero driver CPU must
not be used as the compilation cost. Sampled compiler CPU includes that work.
