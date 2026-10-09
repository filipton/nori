# Faster development checks

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

## Devices

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
real server as documented in [testing.md](testing.md).

Accessibility checks use the standalone shell helper `tools/UiDump.java`, built
on demand by `tools/ui-dump.sh`. It reads visible nodes without waiting for an
animating player to become idle. Conditions still wait for the expected state.
Failures save XML and screenshots under `build/e2e/SERIAL/failures/`.

## Bazel pilot

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

## Measurements

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
one core. See [build-times.md](build-times.md) for measured edits and limitations.
