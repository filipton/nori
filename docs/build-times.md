# Development build and test times

Baseline for [issue #36](https://github.com/norifm/nori/issues/36), measured on
2026-10-10 in checkout `2664eca3`: Apple M4 Pro, 14 cores, 24 GB RAM, arm64 macOS,
Cargo 1.99.0-nightly. Existing dependency and build caches were retained; these
are not clean-build measurements. Heavy commands ran sequentially, with Cargo
limited to four jobs, including the Cargo tasks started by Gradle. Each scenario
was measured once, so these are observations rather than benchmark averages.

## Wall time

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

## Where the time goes

### Android

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

### Rust tests

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

## Experiments selected from the baseline

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

## CPU and emulator e2e follow-up

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

## Research: faster iteration and parallel agents

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

### Parallel agents

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

### Bazel

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

## Local measurement artifacts

Logs, Cargo timing reports and Gradle profiles are retained in the ignored
`build/development-timings/2026-10-10/` directory. Open the Cargo HTML reports for
compilation concurrency and the `gradle/profile-*.html` reports for task durations.
The `cpu-e2e/` and `cpu-kotlin/` directories hold the CPU follow-up measurements;
`cpu-e2e/sections.json` records each suite's section timing and emulator CPU.

The Gradle native builds were measured with `CARGO_BUILD_JOBS=4`. To reproduce,
use the commands above with the same caches and temporary edits; restore source
files afterward. An unchanged build alone does not measure the editing loop.


## Implemented improvements

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
concurrent jobs; they do not set an OS CPU quota. See [development.md](development.md)
for commands and [testing.md](testing.md) for the final device-suite results.

Updated logs are under `build/development-timings/2026-10-10/optimized/`,
`build/development-timings/nextest-warm/` and
`build/development-timings/cargo-final/`. CPU samples are observations of all
matching compiler/JVM/emulator processes and may include concurrent host work.


### Final hardware-rendered device costs

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
