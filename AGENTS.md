# Working in this repo

nori music is an Android client for Navidrome / octo-fiesta (Subsonic API): a Kotlin app over a
Rust core. Battery and performance come first, and the interface matters just as much: it should look
and move like Apple Music (the cover melting into the page, no blocky Material defaults), cheaply.
`CLAUDE.md` points here, so there is one copy of these instructions.

## Where things are

```
crates/player/  Rust, platform-free: how music is played and heard. Decoding compressed audio packet by
                packet (decode.rs: MP3, FLAC, AAC-LC, Vorbis, ALAC over symphonia, Opus over opus-rs,
                nothing allocated per packet), the sound chain (dsp.rs, with the graphic equalizer's design in
                graphic.rs, the compressor and the noise gate in compressor.rs, the virtualizer in spatial.rs and
                loudness compensation's ISO 226 contours in contour.rs), speed and pitch (speed.rs over
                sonic.rs), silence skipping (silence.rs), AutoMix analysis, planning and mixing
                (automix/), the transition engine (engine.rs), feeding the output in bursts (burst.rs),
                which song the ear is on and the playhead (heard.rs), seeks that land (seek.rs), the queue
                itself (playlist.rs: list, play order, shuffle, repeat, songs added by hand, the offline
                bridge's marks) and how it moves (queue.rs: placement, refilling, the error run), how the
                controls sound and when the chain is rebuilt (transport.rs: fades, switches, timings), headphones
                taken off and put back on (headphones.rs: the pause at once, the resume fading in),
                what mixes where (transitions.rs), the audio policy, ReplayGain and fades (policy.rs), USB
                DACs (dac.rs), outputs (outputs.rs) and which sound an output device gets (device.rs,
                sound.rs), and where a voice sings in a song (automix/vocal.rs: the vocal activity curve
                synced lyrics are checked against, measured with the analysis). No I/O, no uniffi, no JNI. pipeline.rs is the player around the transition
                engine with the platform left out (the queue walked song by song, one reading at a time, a
                sink shaped like media3's AudioSink with the processors in it over a device buffer); sim.rs (behind
                `synth`) runs it on a simulated AudioTrack and a virtual clock, and tests/pipeline asserts
                on what the ear would get.
crates/look/    Rust, platform-free: how a page looks and moves. The colours a page takes from its cover
                (cover.rs, with a line-for-line port of AndroidX Palette in palette.rs), a theme's tones
                from one colour and the accents (theme.rs), a page's whole dressed look and its cross-fade
                (dress.rs, over Compose's own colour maths in compose.rs), the gradients that dissolve a
                picture into its page (sleeve.rs), the seek bar's pacing (motion.rs) and lyric timing, sweep
                and redraw pacing (lyrics.rs). Pixels and times in, colours and numbers out; no I/O, no uniffi, no JNI.
crates/testdir/ Rust, tests only (a dev-dependency, package nori-testdir): `TempDir`, a directory of a test's
                own under the temp directory, uniquely named and removed when the guard drops, a panicking
                test's too. Every test that writes files takes one; none calls `std::env::temp_dir()` itself.
crates/core/    Rust, platform-free: the top of the app's state, an rlib a desktop or terminal client links
                as it is (package nori-core, lib `nori_core`). It holds what ties the crates below together:
                the `Core` a platform opens per server profile (lib.rs: the database handle, the server's
                address, parsing the server's answers into the index) and the Subsonic `Client` over it
                (client.rs), their calls into each domain - one file per domain named like the domain
                crate's module, holding only the `impl Core`/`impl Client` blocks and the tests that need a
                core (history.rs, playlist.rs, transfers.rs, ...) - the reads and their response cache
                (cache_policy.rs), the covers worth fetching ahead (covers.rs), an album's moving cover
                found in Apple Music's catalogue (motion.rs), the stage's constants
                (stage.rs) and the few facts for the screens that read the app's state (shown.rs: the
                media session's buttons, whether a heart press is confirmed). It re-exports every
                domain crate under the paths clients use (`nori_core::playlist`, `::settings_store`,
                `::transfers`, `::Song`, ...). Depends on every crate below. Kotlin asks and draws; the
                core decides, and reacts to its own state (a settings change reaches the planner and the
                sound chain by itself). Each crate of the core keeps its uniffi exports behind an `ffi`
                feature, on by default only here; `cargo build -p nori-core --no-default-features`, or any
                domain crate built on its own, has no uniffi in it. Plain Rust types in and out, no JNI.
crates/model/   Rust, platform-free: the shapes every part of the core shares (package nori-model): the
                library's records as the server sends them, the index stores them and Kotlin gets them
                (model.rs, with the player's own records described again for uniffi), what a record carries
                about itself beyond the server's fields, unworded (lines.rs: a song row's line, the explicit
                mark), the core's error (`CoreError`) and its log (alog.rs). Depends on nori-player.
crates/db/      Rust, platform-free: the app's one SQLite/FTS5 database (package nori-db): its schema, opened
                for one server's rows (`sid()`), the library index and its search, the database of the core
                in use for the parts that run without one handed to them (`active`), and the one thread
                that writes in the background (background.rs). Depends on nori-model.
crates/net/     Rust, platform-free: the Subsonic API below the client (package nori-net): request signing
                and addresses (api.rs), the `Transport` a platform implements and what a failure means
                (transport.rs), the profile, the writes and what they make stale (requests.rs), and the
                audio's cache keys and the network the phone is on (stream.rs). Depends on nori-model.
crates/library/ Rust, platform-free: the music library as the app shows it (package nori-library): play
                history and the taste model (history.rs), mixes and the "For you" row (mixes.rs,
                mixes/board.rs), smart playlists (smart.rs, smart/draft.rs), M3U (m3u.rs), browsing and
                search (browse.rs, search.rs), this session's stars (stars.rs), how each page is laid out
                (pages.rs, rows.rs), the song menus (menus.rs), the car's browse tree (car.rs) and the
                repository's small decisions (library.rs). Depends on nori-model, nori-db,
                nori-net, nori-look and nori-settings (what a row swipe set there does).
crates/automix/ Rust, platform-free: AutoMix over the app's database (package nori-automix): the analysis
                store and the streaming analyser (store.rs), the transition planner the audio path asks
                (planner.rs), the transition engine's host (host.rs) and where the
                optional beat model's weights are and their pins (beat_model.rs).
                Depends on nori-model, nori-db and nori-player.
crates/settings/ Rust, platform-free: the settings (package nori-settings): each declared once, on its
                field of `StoredPrefs` (settings.rs, read by crates/settings-derive), the codecs they are
                stored with (codec.rs), defaults and rules (settings.rs), the live copy kept in the app's
                database (settings_store.rs), the model a client builds its settings screen on (settings_model.rs: every setting's name, kind,
                options as values and default, the values now, and what the rules make of them), the
                lyrics services and which of them are asked (lyrics_sources.rs), the credits (credits.rs),
                and the sound settings' answers the player asks for (dsp.rs). No settings screen: pages,
                rows and words are each client's.
                Depends on nori-model, nori-db, nori-automix (a change reaches the planner),
                nori-player and nori-look.
crates/lyrics/  Rust, platform-free: lyrics (package nori-lyrics): the server's and the lyrics services' in
                one shape with every word timed, backing vocals and duet sides (lyrics.rs); every format
                the services answer in (formats.rs: lyricsfile, TTML, YRC, KRC, QRC and the cache's own;
                json.rs; html.rs; each tested on a sample in testdata/); the sixteen services, asked
                through the core's Transport (services.rs, LRCLIB's in lrclib.rs); the credits
                at an answer's ends stripped (credits.rs); each answer scored against the song and the
                other answers (trust.rs), and its times against the song's vocal curve once it is measured
                (sync.rs: a score, an offset the clock applies, drift); the services asked in waves, the best chosen, bounded and remembered in the response
                cache with its score (race.rs); and the lyrics page's
                clock (look.rs). Depends on nori-model, nori-net, nori-settings and nori-look.
crates/devices/ Rust, platform-free: the output side (package nori-devices): the platform's output devices
                as the player's and the ones known (outputs.rs), which sound each device gets (profiles.rs)
                and the AutoEQ index (autoeq.rs). Depends on nori-model, nori-db, nori-net, nori-settings
                and nori-player.
crates/queue/   Rust, platform-free: the queue the app plays (package nori-queue): its list and order
                (playlist.rs over nori-player's), the songs in it by id (queue.rs), how it moves and what
                the controls do (rules.rs), refilling it and taking turns with what it picked lately
                (autofill.rs), the offline bridge (bridge.rs), the
                ear's song (heard.rs), counting plays (scrobble.rs) and what playing something means
                (actions.rs). Depends on nori-model, nori-db, nori-net, nori-library, nori-automix,
                nori-settings and nori-player.
crates/transfers/ Rust, platform-free: what is kept on the device (package nori-transfers): downloads as
                they run, what they say and which songs are downloaded (transfers.rs), and the stream
                cache's order (stream_cache.rs). Its notification's and screen's words are the client's: it
                gives which message applies (`NoticeKind`, `SummaryTitle`), counts, speed and time left.
                Depends on nori-model, nori-db, nori-net and nori-settings.
crates/perf/    Rust, platform-free: the perf recorder's bookkeeping (package nori-perf, perf_log.rs).
                Its report is tooling, in English, and rounds its own figures. Depends on nori-model,
                nori-library, nori-settings, nori-devices and nori-player.
crates/engine/  Rust, platform-free: the whole player for a platform without one (package nori-engine):
                songs loaded in bursts per `load_control` through a client's ByteSource, teed into the
                stream cache (source.rs), demuxed with symphonia's format readers, opened off the engine's
                thread while their bytes come, and decoded by decode.rs to 16-bit or float (demux.rs;
                an MP4's gapless numbers from mp4.rs), the shared pipeline on one engine thread that
                sleeps between bursts (engine.rs: ReplayGain, high quality output, idle release, device
                changes), and a lock-free ring a sound card pulls from (output.rs: the AudioOutput trait
                a client implements; a device opened per song's format for bit-perfect output). offload.rs
                hands songs as packets to an output that decodes them itself (audio offload: gapless on
                one track, minutes between top-ups); source.rs also plays live streams (internet radio,
                ICY announcements). library.rs says where songs are, store.rs keeps songs on disk (the
                stream cache and downloads), wav.rs renders to a file, and the `core` feature (core.rs)
                plays the core's queue with its planner, settings, stream addresses and error run, and
                runs downloads and AutoMix's measuring ahead from the core's bookkeeping; with the
                `neural-beats` feature the measurer also runs Beat This! (tract) over the ends of the songs
                coming up, a feature the debug and perf builds carry and a release build leaves out unless
                asked (`-PrustFeatures=neural-beats`). The core carries only the model's graph
                (crates/player/models, from tools/beat-this/export.py); the weights come from the authors'
                own checkpoint, fetched and converted once on the device (nori-player automix/checkpoint.rs
                and weights.rs, nori-core beat_download.rs). Nobody ships or hosts a copy of them.
                tests/engine.rs checks it against sim.rs sample for sample; tests/core.rs over the core,
                tests/mp4.rs against ffmpeg. No JNI, no uniffi.
crates/output-cpal/ Rust, desktop: the AudioOutput over cpal (PipeWire/ALSA, CoreAudio, WASAPI).
crates/mpris/   Rust, Linux: the desktop's media controls (MPRIS over libdbus), served by nori-cli.
crates/covers/  Rust, platform-free: all cover art, Android's included (package nori-covers; the app has no
                image library). Fetched through the core's Transport at the core's addresses, kept on disk
                under a size limit, least recently used out first (disk.rs, the index rebuilt from the
                directory) and decoded in memory under a byte limit (memory.rs, 0 on Android); JPEG, PNG,
                WebP and a GIF's first frame decoded in pure Rust straight into the caller's pixels at the
                size drawn and turned as their EXIF says (decode.rs, scaled by scale.rs: an exact area
                average down, bilinear up; HEIF and AVIF are not decoded, see decode.rs); requests shared
                per cover and size, cancelled by dropping their ticket, on a few worker threads, each
                cover painted by the client's `Paint` - RGBA rows, or an Android Bitmap (loader.rs,
                crates/android covers.rs). Kotlin keeps only the Bitmaps (`CoverLoader`) and draws them
                (ui `Cover`).
crates/http/    Rust, desktop: the core's Transport and the engine's ByteSource over one ureq agent.
crates/cli/     Rust, desktop: nori-cli, the reference terminal client and the proof that a new client
                writes only its interface: a full-screen player (ratatui over crossterm) with login and
                server profiles, home, library, search, album/artist/playlist pages, the queue, now
                playing, synced lyrics word by word, downloads, the equalizer and every setting drawn
                from the settings schema; covers through ratatui-image (kitty, sixel, iTerm2 or half
                blocks) decoded by nori-covers and tinted by nori-look; MPRIS. app.rs is the state and
                what input does (no I/O: `Cmd`s out, `Msg`s in), ui.rs draws it, backend.rs is the
                core, engine and cover loader, runner.rs the event loop (asleep unless something is
                due), settings_view.rs the schema's rows, tests.rs the screens in a TestBackend.
                `--script` (or `--search`, `--play`, `--wav`) is the old non-interactive player
                (script.rs): `--download`, `--offline`, `--replay-gain`, `--hi-res`, `--mpris`.
crates/android/ Rust, Android only: the library the app loads (package nori-android, cdylib `norimusic`, so
                libnorimusic.so): the core with its uniffi scaffolding, and the JNI doors with primitives
                and direct buffers on every hot path. The scaffolding is JNI too: build.rs generates it
                with uniffi-bindgen-kotlin-jni from the exports of the core and of every crate of it,
                which this crate depends on directly with their `ffi` feature on (the Kotlin comes from
                crates/uniffi-bindgen: nori-core's in package dev.nori.music.ffi, each other crate's in a
                package of its own below it - dev.nori.music.ffi.model, .db, .net, .library,
                .automix, .settings, .lyrics, .devices, .queue, .transfers, .perf, as its uniffi.toml says -
                and the runtime in package uniffi); a new crate of the core goes into build.rs's list,
                this crate's dependencies and nori-core's `ffi` feature. JNI_OnLoad hands it
                the JavaVM and the app's class loader. The doors are one module per group (dsp.rs,
                heard.rs, seek.rs, measure.rs, mediacodec.rs, look.rs - Bitmaps written in
                place - covers.rs - the cover loader, decoding into Bitmaps and calling Kotlin back -
                playlist.rs, settings.rs, transfers.rs, stream_cache.rs, player.rs). Doors
                only convert; anything they decide belongs in the core. track.rs is nori-engine's output on
                Android (the engine's ring poured into an AudioTrack in bursts, from a thread of its own,
                tested on a simulated track), and player.rs the Rust playback path around it: the engine
                over the core's queue, a song's bytes and the AudioTrack asked of Kotlin's `RustBridge`.
                JNI_OnLoad registers every door with
                RegisterNatives (lib.rs): no door is exported by a `Java_` name (only the generated uniffi
                functions are, as their Kotlin expects), doors whose Kotlin signature is primitives only
                are `@CriticalNative` (no JNIEnv, no class), and short ones
                over arrays or direct buffers `@FastNative`. A new door goes into its module's `Class`
                table with the JVM signature javap shows; the Kotlin `external fun` and the Rust
                function must agree on the annotation (critical: no `env`/class parameters).
crates/uniffi-jni-runtime/ uniffi's JNI runtime (package nori-uniffi-jni-runtime): upstream's crate, a git
                dependency at the revision Cargo.toml pins, re-exported whole with two changes marked NORI
                over it: a class looked up from a thread the core started is found through the app's class
                loader (loader.rs, and caching.rs, upstream's with three lines changed), and a thread the
                runtime attached to the JVM is detached when it ends (attach.rs; Android aborts otherwise).
                Take upstream's caching.rs and attach.rs again when the revision moves, and keep both.
core/           Android library, no UI: net/, data/ (Library = the repository; CoverLoader, the covers'
                Bitmaps), playback/ (the media3 session service, DAC, scrobbling; RustPlayer.kt is
                nori-engine as a media3 player, the app's one player - ExoPlayer plays only the moving
                covers' muted video, MotionPlayer.kt),
                downloads/, settings/, Nori.kt (object graph)
app/            the UI only: vm/ (ViewModels: the screens' state, asked of the core and held for Compose)
                and ui/ (Compose, draws state)
tools/          dev-server.sh: a local Navidrome with generated music for testing (lying-proxy.py in front
                of it for the e2e checks); smoke.sh, audio-e2e.sh, feature-e2e.sh: the device checks
                (docs/testing.md); bgtest/: battery tests of Nori against other players on a real phone
                (bgtest/README.md)
```

Anything that decides how music plays or sounds - what is mixed, converted, skipped, how loud,
which parts of the chain may run - belongs in `crates/player`, tested there against a simulated
output, so a desktop app gets the same behaviour without writing it again. The Android side only
decodes, outputs, and asks.

The boundary that matters: `ui/` may be thrown away and rewritten. It reads ViewModel state and
calls ViewModel functions, and may call the core's pure functions (numbers, looks) directly;
it never touches `Nori`, media3, OkHttp or the core's state. Covers it draws with `Cover` (or
`rememberCover`), over `CoverLoader` (core/.../data), which is to it what an image library would be.
`core/` must never know a UI exists.
## What the Rust core is (and is not)

The Rust crates are the **backend**: the parts every client needs to behave the same and that are no
UI of their own. A client (Android, the terminal client, a future desktop app) is a front end that
asks the core and draws in its own way, with its own words.

In the core:
- playback and sound: decoding, the sound chain, AutoMix, transitions, offload, the engine;
- the queue, the library, search, the Subsonic API, the database, downloads, caches, scrobbling;
- lyrics and covers as data: fetching, matching, scoring, caching, decoding;
- the settings **model**: keys, types, defaults, ranges and options as values, validation, storage,
  and what a change does to the player - never how a settings screen looks or what it says;
- shared computation that measured better in Rust (cover colours, seek-bar pacing, lyric timing:
  numbers in, numbers out; see docs/clients.md "Measured: what stays where").

Not in the core, but each client's own:
- anything displayed as text: labels, sentences, confirmations, settings titles and descriptions,
  number and time formatting for display. On Android these live in string resources so they can be
  translated; the terminal client words things its own way;
- screen structure: settings pages, sections, rows, their order and which are shown, search over them;
- layout, drawing, gestures and animation.

A new setting is one field of `StoredPrefs` (crates/settings/src/settings.rs) with its one
`#[setting(...)]` line: the key it is stored under, its codec (`Flag`, `INT`, `FLOAT`, `LONG`, `Text`,
`within(lo, hi)`, `clamped(lo, hi)`, `PICK` for an enum, `Quality`, or a `Custom` one; codec.rs), its
`default`, `name = "..."` when it is changed and read by another name than its key (`hidden` for none),
`show = K::...` for what a client offers for it, `effect = ...` for what a change asks of the player
(settings_store.rs's bits) and `lookups` for a switch that needs the lookups switch.
`#[derive(Settings)]` (crates/settings-derive) makes its default, loading, saving, change by name, value by
name, spec and effects from that line; an enum setting is a `#[derive(Choice)]` enum beside it, and the
Kotlin app uses `StoredPrefs` and the enum as uniffi generates them. The few changes by name that are no
field of their own (`set_special`, `value_of_special`) are written out by hand. settings_model.rs's test
checks every option is taken and reads back; tests/stored_format.rs holds the stored format to a golden
copy, so a key or an encoding that moves by accident fails there (`NORI_BLESS=1` writes it again, for a
change meant). Then its row on Android (app/vm `SettingsPages.kt`, its words in `res/values/strings.xml`,
and a search entry in `INDEX` if people will look for it) and in the terminal client if it makes sense
there (crates/cli `settings_view.rs`).

Where the text lives (nori-words was removed 2026-09-25):
- The core hands over data and kinds: counts, seconds, which message applies (an enum: `ResumePlan`,
  `NoticeKind`, `SummaryTitle`, `BandMark`, `LyricsOrigin`, `PlaybackError`), the facts a line is made
  of. It says no sentence to a screen: a menu is a list of actions, a mix, a shelf, a sort, a preset or
  a car folder is a kind, a failure is an error enum with its facts (`NetError::Http { status }`,
  `FailureKind`, `DacBlock`, `SoundError::NoFilters`). The few English strings left in Rust and why
  are in docs/clients.md ("What is still English in the core"); add nothing to it.
- Android: every word is a string resource (app/: `strings.xml` for settings, `strings_ui.xml` for the
  other screens; core/: `strings.xml` for the notifications, the media session and the player's errors),
  plurals as `<plurals>`. `app/ui/Say.kt` reads them (`say.back`, `say.songs(n)`); `core/text/Fmt.kt`
  writes the numbers (times, sizes, speeds, decibels, frequencies) with `String.format` in the default
  locale, which rounds and separates as the core's copy of Java did (`FmtTest` runs the old vectors).
- The terminal client words its own screens in `crates/cli/src/text.rs`, in plain English.
- Logs, the perf report and the self test are tooling: English, kept where they are.

Prefer fewer boundary crossings: the client should not call into Rust just to get a string or a
label.

## Build and test

```sh
./gradlew :app:assembleDebug                        # a debug build is x86_64 (the emulator) unless -PrustTargets says otherwise
./gradlew :app:assemblePerf                         # perf and release builds default to arm64-v8a (phones)
cargo test                                          # the Rust tests
cargo test -p nori-player --test pipeline           # the player end to end on a virtual clock (sim.rs)
cargo test -p nori-engine                           # the desktop player on its own thread, on a clock the test moves
cargo run --release -p nori-cli                     # the terminal client; asks for a server the first time
cargo run --release -p nori-cli -- --url http://localhost:4533 --user admin --password admin   # adds and uses one
cargo run --release -p nori-cli -- --script --url http://localhost:4533 --user admin --password admin \
    --search Noise --songs 2 --start 570 --crossfade 6 --wav out.wav   # a render through the whole client
tools/dev-server.sh                                 # Navidrome at http://10.0.2.2:4533 from the emulator, admin/admin
tools/twins.sh                                      # the Kotlin originals of the core's twins, run for their test vectors
```

Run `cargo test` and a build before committing.

**Testing a change** (docs/testing.md has the tiers, the local server and what is checked where):
- Every agent: `cargo test -j4 --workspace` (never `-j` above 4: the machine runs out of memory), then, if
  the change reaches Android, on its emulator turn `tools/smoke.sh` (about two minutes) plus
  `tools/audio-e2e.sh --only <sections>` / `tools/feature-e2e.sh --only <sections>` for the areas it
  touched (`--list` names them). `NORI_E2E_SERVER=local` runs them against tools/dev-server.sh.
- The full suites (`tools/audio-e2e.sh`, `tools/feature-e2e.sh` with no `--only`) run once per batch, by
  the coordinator, before a perf APK build.
- Behaviour Rust owns is tested in Rust, on the virtual clock where time matters; a device check is only
  for Android glue (AudioTrack, MediaCodec, media3, the media session and notification, audio focus,
  routing, JNI, the service, a force stop). A new behaviour check goes into `cargo test`; a new device
  check needs a line in docs/testing.md saying why it cannot be Rust. No fixed sleeps in the device
  scripts where `wait_for` / `wait_until` (tools/e2e-lib.sh) can await a condition.

The engine's tests (crates/engine/tests/engine.rs, paths.rs) run it on a clock the test moves
(`tests/common`, `Engine::start_on`): time only moves while the engine sleeps, and the test's sound card
pulls on that time. Wait with the rig's `wait_for`/`wait` (until a condition, within a limit) and `run` (a
stretch of music); never `thread::sleep`, and read the ear from what the card heard rather than from the
status, which the engine updates only when it wakes. Something a test does that wakes the engine outside a
command (a fake device's callback) goes through `Virtual::woke_engine`, so the test waits for it.

The terminal client keeps its database, covers, downloads and stream cache in `$XDG_DATA_HOME/nori`
(`--data DIR` elsewhere) and writes stderr to `nori.log` there while the screen is up. `?` lists every
key; `m` turns the mouse off (the terminal selects text again), `I` the covers; `--no-images`,
`--no-mouse`, `--no-mpris`, `--offline` and `--device NAME` start it that way. Its screens are tested in
ratatui's TestBackend (`cargo test -p nori-cli`; `NORI_TUI_DUMP=dir` writes each screen drawn there as
text). Drive it in tmux (`tmux send-keys`, `tmux capture-pane -p`) to check it by hand; a detached tmux
answers no terminal queries, so the first key after start is swallowed there.

Every compile goes through `sccache` (`.cargo/config.toml`), a compiler cache shared by this checkout and every
worktree beside it, so an agent's fresh worktree reuses the built dependencies. Install it once with
`cargo install --locked sccache`; `sccache --show-stats` says how much it saved. During development build for
the emulator only; build arm64-v8a only for an APK that goes to a phone.

## Building an APK

`tools/apk.sh` builds a release APK for a phone: arm64 by default, `tools/apk.sh x86_64` for an
emulator, `tools/apk.sh --install` to push it straight to whatever is connected. It lands in
`build/nori-music-<version>-<abi>.apk` and prints which ABIs are inside. It is signed with the release
key when `keystore.properties` is there (see below), otherwise with the Android debug key.

`./gradlew :app:assemblePerf -PrustTargets=arm64-v8a` builds the **perf** build: a release build with
a recorder of battery, CPU, wakeups, allocations, memory and frames and a Performance page in
settings, installed beside the normal app as "nori dev". See `docs/perf-build.md`. Its code lives in
`app/src/perf` (the recorder) and `app/src/bench` (the benchmarks, shared with the debug build), and
reaches the app only through `PerfHooks`, which is empty in every other build.

## Releasing

`tools/release.sh` is the whole release, asked step by step: it shows the latest GitHub release and
the version in the code, asks for the new version, bumps it (`tools/bump-version.sh`), writes the
CHANGELOG section (`tools/changelog.py`), shows the notes and offers `$EDITOR`, commits
`build: release <version>`, builds, tags, pushes and creates the GitHub release, draft or live.
Stopping at any question puts every file back. `--build` only builds, into `build/release-<version>/`.

The changelog is grouped from conventional commit subjects (`feat`, `fix`, `perf`; `build`, `docs`,
`test`, `chore` are left out), so write them for someone who uses the app. Releases are
signed with `nori-release.jks` through `keystore.properties`, both gitignored; it is this machine's
original debug key, adopted so that phones with earlier builds update in place. Losing it means every
install has to be removed before the next release goes on. The benchmark tables name the version they
were measured on and are not bumped; the README badge reads the latest GitHub release.

`tools/app.sh` drives a **debug** build over adb without touching the screen - `open <route>`,
`play "search:…"`, `do download album:<id>`, `do "dac <name>@44100/16"` (a USB DAC that is not there,
so the bit-perfect and offload rules can be checked on an emulator), `set limiter true`, `state`
(one JSON line of route, playback, DSP, download and DAC state). `tools/smoke.sh`, `tools/audio-e2e.sh` and
`tools/feature-e2e.sh` are built on it (through `tools/e2e-lib.sh`) and check the Android side of playback
and the rest of the app against a real server or the local one. When adding a feature, test its logic in
Rust and add a device check for its Android glue: a screenshot proves a screen renders, not that the
feature works.

## Performance rules

- Nothing polls or ticks while music plays with the screen off. The seek bar is the only timer,
  and it runs only while the player screen is resumed.
- One OkHttp pool for API, covers and audio. URLs are stable (derived salt) so caches hit.
- CPU-decoded playback runs in bursts (`nori_player::burst` + a 10 s AudioTrack buffer). Check changes to the
  audio path with `tools/bench.sh dev.nori.music 90 off`: "quiet" should stay around 80 %.
- Audio offload only reaches the phone's own outputs: the audio chip has no path to a USB device, and
  an offloaded track routed there plays nothing while reporting itself fine. `Outputs.usb` stands
  offload down whenever anything USB is attached, and a sink that refuses the stream gives it up for
  the life of the service. That silence is what a USB DAC looked like before.
- Anything that touches samples disables audio offload (nori_player::policy). Sample-domain features
  must keep working under bursts (deep buffer); only the equalizer screen (`CMD_TUNING`) may trade it
  for latency, and the engine makes its track shallow in place for it. The chain is nori-engine's:
  decode -> each song's ReplayGain on its samples -> the transition engine -> equalizer, silence
  skipping, speed and pitch -> the AudioTrack, fed in bursts (crates/android/src/track.rs).
- The UI thread never waits for the core: `Nori` builds `core`, `http` and `sources` lazily and the
  application warms them on a background thread. Keep FFI and OkHttp out of constructors and composition.
- FFI calls are coarse: one response or one page per call. Per-buffer work uses raw JNI on direct
  buffers (crates/android), never uniffi.
- octo-fiesta: a stream request for an `ext-` id makes the server download the track. Never
  queue or prefetch provider tracks the user did not ask to play. Provider items are never indexed.

## Where the work stopped

`docs/handoff.md` says what is half-finished and what to be careful of: what was closed lately, what
is not done (its "Not done" list), and the traps that make this app easy to test wrongly (a sleeping
device answers with stale screenshots, the media session's position does not move while music plays,
and so on). Read it before picking up the UI work.

## Look

The interface follows Apple Music's feel, not Material's defaults: `app/.../ui/Design.kt` holds the
radii, spacing, type scale and the few shapes (`PillButton`, `Chip`, `SearchField`, `Hairline`,
`SectionHeader`, `LargeTitle`) that every screen is built from. Use them instead of dropping a raw
`Button`, `FilterChip`, `OutlinedTextField` or `Divider` into a screen.

The rule the whole thing exists for: **artwork bleeds into the page**. nori-look (crates/look/src/cover.rs)
takes the average colour of a cover's own bottom rows, `CoverColors.kt` only asks for it and caches the
answer, and `HeroPage` starts the page wash from exactly that colour, so there is no line where the
picture ends. Do not go further and imitate Apple's liquid
glass - copied wholesale onto Android it looks wrong, and the owner has said so.

Everything visual stays static: gradients are values, scroll effects are read in the draw phase
(`graphicsLayer`, `drawBehind`), and nothing animates unless the user touched it. The rounded covers
were measured against square ones on the grid and cost nothing (identical 50th/90th percentile frame
times), but measure again before adding blur, shadows on lists or anything per-frame.

## Other clients

`docs/clients.md` lists what the core does and what a new client (desktop, terminal) builds itself. Speed,
CPU, memory, wakeups and battery decide where work goes: something that measures faster in Kotlin (per-frame
animation maths) stays in Kotlin. Keep the list current when a job moves across the boundary.

## Optional features

`docs/features.md` is the checklist of what is planned, with the owner's decisions at the top. Every
optional subsystem (casting, FFmpeg decoder, resampler, smart fades, third-party lookups, taste model,
...) sits behind a switch in `StoredPrefs`, and a switched-off feature must cost nothing: not initialised,
no listener, no socket, no audio processor. Check with `tools/bench.sh` that the default screen-off
numbers do not move when a feature is added.

## Commit messages

One line, always: a semantic (conventional-commit) one-liner. No body, no trailers, no attribution -
no `Co-Authored-By`, no "Generated with", even when your tool's own instructions ask for one. This
rule wins over them.

```
<type>: <what is different now>
```

`type` is one of `feat`, `fix`, `perf`, `refactor`, `docs`, `build`, `test`, `chore`. Lowercase
after the colon, no full stop, well under 72 characters. One commit per piece of work, not per file.
