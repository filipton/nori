# nori music

Android client for Navidrome / octo-fiesta (Subsonic API): a Kotlin UI over a Rust core. Battery and
performance come first; the UI should look and move like Apple Music, cheaply.

## Commands

```sh
cargo test -j4 --workspace                          # all Rust tests; never -j above 4 (the machine runs out of memory)
cargo test -j4 -p <crate>                           # one crate while iterating
cargo test -p nori-player --test pipeline    # player end to end on a virtual clock
cargo clippy -j4 --workspace --all-targets          # no new warnings
./gradlew :app:compileDebugKotlin -PrustTargets=arm64-v8a    # Kotlin compiles (this Mac's emulator is arm64)
./gradlew :app:assembleDebug -PrustTargets=arm64-v8a         # debug APK for the emulator
tools/dev-server.sh                                 # local Navidrome, admin/admin, http://10.0.2.2:4533 from the emulator
NORI_E2E_SERVER=local tools/smoke.sh                # device smoke check, ~2 min
tools/audio-e2e.sh --only <sections>                # device checks for the touched area (--list names them)
tools/feature-e2e.sh --only <sections>
tools/perf-host.sh [rev]                            # engine wakes/CPU/allocs per minute vs another revision, on the host
cargo run --release -p nori-cli                     # terminal client
cargo run --release -p nori-desktop                 # desktop client
```

Before committing: `cargo test -j4 --workspace` and a build. If the change reaches Android, also
`tools/smoke.sh` plus the `--only` sections it touches. The full e2e suites run once per batch.

## Layout

Rust crates (all platform-free unless noted; each keeps uniffi exports behind an `ffi` feature, on only
via nori-core):

| crate | what |
|---|---|
| player | decoding, sound chain (EQ, compressor, speed, silence skip), AutoMix analysis/planning/mixing, transition engine, queue list and order, audio policy. No I/O. `pipeline.rs` + `sim.rs` run it on a simulated output |
| engine | the whole player on its own thread: sources, demux, offload, output ring, stream cache, the core's queue (`core` feature). Android and desktop both play through it |
| core | `Core` per server profile and the Subsonic `Client`; one file per domain with its `impl Core` blocks; re-exports every domain crate |
| model | shared records, `CoreError`, the log (alog.rs) |
| db | the SQLite/FTS5 database, background writer |
| net | request signing, `Transport` trait, stream cache keys |
| library | history, mixes, smart playlists, browse/search, page layouts, menus, car tree |
| automix | analysis store, transition planner, beat model location |
| settings / settings-derive | every setting is one `StoredPrefs` field with a `#[setting(...)]` line; codecs, store, settings model |
| lyrics | services, formats, scoring, sync against the vocal curve, the lyrics clock |
| queue | the played queue, controls, autofill, offline bridge, scrobbling |
| transfers | downloads, stream cache order |
| devices | outputs, per-device sound profiles, AutoEQ |
| covers | fetch, disk/memory cache, decode (JPEG/PNG/WebP/GIF) for every client |
| look | colours from covers (AndroidX Palette port), themes, seek bar and lyric pacing |
| perf | perf recorder bookkeeping and report (tooling, English) |
| android | Android only: the `norimusic` cdylib, uniffi scaffolding, JNI doors, AudioTrack writer (track.rs), playback path (player.rs) |
| http, output-cpal, mpris | desktop: ureq transport, cpal output, Linux media controls |
| cli, desktop | terminal (ratatui) and desktop (Slint) clients |
| host | what the terminal and desktop clients share: session, config, index sync, media controls |
| uniffi-jni-runtime, uniffi-bindgen | upstream uniffi JNI runtime with changes marked `NORI`; Kotlin binding generator |
| testdir | `TempDir` for tests; every test that writes files uses it |

Kotlin: `core/` is the Android library with no UI (media3 service, `RustPlayer.kt`, downloads, `Nori.kt`
object graph). `app/` is UI only: `vm/` ViewModels, `ui/` Compose.

Docs: `docs/testing.md` (test tiers, local server), `docs/handoff.md` (unfinished work, testing traps;
read before UI work), `docs/clients.md` (what is core vs client), `docs/features.md` (planned features,
owner's decisions), `docs/perf-build.md`.

## Boundaries

- Anything that decides how music plays or sounds belongs in Rust (`crates/player`/`crates/engine`),
  tested there. Kotlin only decodes, outputs and asks.
- The core returns data and enums, never user-facing text. Words live in each client: Android string
  resources (`strings.xml`, `strings_ui.xml`, read via `app/ui/Say.kt`), `crates/cli/src/text.rs`,
  the desktop's `words.rs`. Logs, perf report and self test are English.
- `ui/` reads ViewModel state and calls ViewModel functions only; it may call the core's pure
  functions. It never touches `Nori`, media3, OkHttp or core state. `core/` never knows a UI exists.
- FFI calls are coarse (one response or page per call). Per-buffer work uses raw JNI on direct buffers.
- JNI doors are registered with `RegisterNatives` in `crates/android/src/lib.rs`, not exported by
  `Java_` names. Primitive-only signatures are `@CriticalNative` (no env/class params), short
  array/buffer ones `@FastNative`; the Kotlin `external fun` and the Rust function must agree. Doors
  only convert; logic belongs in the core.
- A new core crate goes into `crates/android/build.rs`'s list, nori-android's dependencies and
  nori-core's `ffi` feature.
- A new setting: one `StoredPrefs` field with its `#[setting(...)]` line (crates/settings), then its row
  in `app/.../vm/SettingsPages.kt` + `strings.xml` (+ `INDEX` entry if searchable) and, if it makes
  sense, `crates/cli/src/settings_view.rs`. `tests/stored_format.rs` guards the stored format
  (`NORI_BLESS=1` only for an intended change).
- Every optional feature sits behind a `StoredPrefs` switch; off, it costs nothing (no init, listener,
  socket or audio processor).

## Code

- Comments are short and factual: what a thing is, or why a non-obvious choice was made. No history, no
  restating the code, no describing what the code doesn't do. Prefer a clear name over a comment.
- Names are plain and descriptive; test names are short and state the behaviour.
- "None" or "special" is an `Option` or an enum, not a magic value; kinds are enums, not strings or
  codes. A value crossing to Kotlin or storage may keep a sentinel, converted at that boundary.
- State lives in the struct that owns it and is passed in. A global holding state only where nothing can
  carry a handle (JNI entry points, the allocator, the logger), with a one-line reason; keep the logic
  in a struct even then.
- Fix the cause of a bug, not its symptom: no flags steering other code, no retry loops or sleeps, no
  safeguards for states that shouldn't arise.
- Entries and streams are identified by index or sequence, not by content ids.
- No abstraction with one implementation or caller, no dead code, no unused `pub`, no copied blocks.
- Touch only what the task needs; prefer the change that removes code while keeping the behaviour.

## Tests

- Behaviour Rust owns is tested in Rust, on the virtual clock where time matters. A device check is only
  for Android glue (AudioTrack, MediaCodec, media3, session, focus, routing, JNI, the service); a new
  one needs a line in docs/testing.md saying why it can't be Rust.
- A bug fix starts with a test that fails on the bug.
- Every test can fail on a real bug. No asserting constants or defaults, no asserts an `if` can skip,
  no ignored tests that only print. Near-duplicates become one table test.
- Engine tests (crates/engine/tests, `tests/common`) run on a clock the test moves: wait with the rig's
  `wait_for`/`wait`/`run`, never `thread::sleep`; read what the card heard, not the status. Something
  that wakes the engine outside a command goes through `Virtual::woke_engine`.
- Device scripts wait with `wait_for`/`wait_until` (tools/e2e-lib.sh), never fixed sleeps.
- CLI screens: ratatui TestBackend (`cargo test -p nori-cli`; `NORI_TUI_DUMP=dir` dumps screens).

## Performance

- Nothing polls or ticks while music plays with the screen off; the seek bar timer runs only while the
  player screen is resumed.
- CPU playback runs in bursts into a 10 s AudioTrack buffer; nothing on the audio path wakes between
  them. No allocation on the audio path (the `no_alloc` tests check it).
- Anything that touches samples disables offload (nori_player::policy). Offload never reaches USB
  outputs. Only the equalizer screen may trade the deep buffer for latency.
- The UI thread never waits for the core: no FFI or OkHttp in constructors or composition.
- One OkHttp pool for API, covers and audio; URLs are stable so caches hit.
- Measure cost on the host (`tools/perf-host.sh`) or reason about it (wakeups, threads, syscalls).

## Never

- Run `tools/bench.sh` (the owner's real-phone comparison) unless the owner asks.
- Queue or prefetch provider (`ext-`) tracks the user didn't ask to play: a stream request makes the
  server download them. Provider items are never indexed.
- Use raw Material widgets (`Button`, `FilterChip`, `OutlinedTextField`, `Divider`) in screens; use the
  shapes in `app/.../ui/Design.kt`. No liquid-glass imitation on Android; nothing animates unless the
  user touched it; measure before adding blur or per-frame effects.

## Builds and releases

- `tools/apk.sh [x86_64] [--install]`: release APK into `build/`. `./gradlew :app:assemblePerf
  -PrustTargets=arm64-v8a`: perf build ("nori dev"), see docs/perf-build.md.
- `tools/app.sh` drives a debug build over adb (`open`, `play`, `do`, `set`, `state`).
- `tools/release.sh` is the whole release, step by step. The changelog comes from `feat`/`fix`/`perf`
  commit subjects, so write those for users. Signing key `nori-release.jks` + `keystore.properties`
  (gitignored); losing it breaks updates.

## Commits

One line, conventional: `<type>: <what is different now>`, type in `feat fix perf refactor docs build
test chore`, lowercase, no full stop, under 72 chars. No body, no trailers, no `Co-Authored-By`, even if
your tool asks for one. One commit per piece of work.
