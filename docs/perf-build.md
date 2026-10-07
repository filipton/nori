# The perf build

A release build with a recorder in it, for measuring what the app costs on a real phone in real use:
battery, CPU, wakeups, allocations, memory and frames, with no adb and no computer attached. The
results are on a Performance page in settings, and a plain-text report can be shared from there.

It is minified and not debuggable, like a release, so what it measures is what a release costs. It
installs beside the normal app (id `dev.nori.music.perf`, named "nori dev"), with its own sign-in,
settings and downloads.

## Building and installing

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

## What it records

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

## What it costs

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

## Where the memory goes

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

## What the open player costs

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

## A fair battery test

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

## Sharing

"Share report" opens Android's share sheet with the report as plain text: the device, the Android
version, the build (version and commit), the table by state and how much of each state was really
offloaded, the last stretches each with its output and its timeline, the benchmark results if they were
run this session, and at the end the app's own log: the crash buffer (this and earlier runs of the
app) or the copy of it kept at the last start, the last uncaught exception (kept in the app's database
by the recorder's handler as the process died, then handed on to the platform's), and the process's
last 400 log lines (`logcat --pid`), at most 60 000 characters, after the app's own lines of the last
ten minutes (alog's, so a moment the logcat tail has lost is still there). Send it anywhere text goes; the table
is in columns for a monospaced font. The page shows the same log folded at its end.

## Benchmarks

"Run call benchmark" and "Run cover benchmark" run the same code as the debug build's `bench` and
`coverbench` test commands (`app/src/bench/.../Bench.kt`): what a crossing into the core costs by kind
against the same work in Kotlin, and what the core's covers cost: the decode alone, and the whole way to a
software or hardware Bitmap (docs/clients.md). The cover benchmark needs covers on the disk and in memory,
so browse some albums first.
