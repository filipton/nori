# Desktop profiling

Measure an optimized binary with symbols. Debug builds and Xvfb presentation timings do not represent
the owner's desktop. Keep automated clicks on Xvfb with a private audio sink; a physical-display trace
uses only passive sampling while the owner operates the window.

```sh
cargo rustc -j4 --profile android-dev -p nori-desktop -- \
  -C debuginfo=1 -C strip=none -C force-frame-pointers=yes
perf record -e cycles:u -F 99 -g --call-graph dwarf,16384 -p "$NORI_PID" \
  -o build/desktop-cpu.data -- sleep 60
perf report -i build/desktop-cpu.data --stdio --no-children --sort comm,dso,symbol
perf script -i build/desktop-cpu.data | stackcollapse-perf.pl | flamegraph.pl \
  > build/desktop-cpu.svg
```

The last two commands use Brendan Gregg's FlameGraph scripts. Dependency frame pointers require a
separate `RUSTFLAGS` build; the command above adds them only to the desktop crate. NVIDIA's proprietary
libraries have incomplete symbols/unwinding, so inspect flat samples alongside the call graph.

For memory, read `/proc/$NORI_PID/smaps_rollup` and group `smaps` RSS by mapping. RSS includes resident
code, shared libraries, driver mappings and allocator slack. It is not the live Rust heap. Sample a
separate test process with jemalloc's `LD_PRELOAD` and
`MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:17`; dump its live heap with `mallctl("prof.dump")`
while it is still running. `jeprof --collapsed --inuse_space BINARY DUMP` produces stacks for
`flamegraph.pl`. An exit dump misses allocations released during shutdown. Sampling at 128 KiB gives
estimates; stack branches overlap and must not be added together.

Use a private SQLite backup and copied cover/music caches when profiling the owner's library. Keep
the copy private: its database contains credentials. Test processes need their own display and audio
sink, and must not receive provider songs that the owner did not request.

For GPU work, RenderDoc's Vulkan capture and `EventGPUDuration` counters measure individual draw/copy
events. Their sum excludes presentation and some synchronization. Record image acquisition, painting,
encoding, submission and presentation CPU times separately when investigating frame delays.

## October 2026 measurements

Linux X11, NVIDIA GTX 960, 1280 × 820/900 virtual window. The physical-display run used the owner's
window and library. These are workload observations, not a fixed memory guarantee or frame-rate test.

| Measurement | Before | After |
|---|---:|---:|
| GPU process memory, fixture and repeated pause animation | 258–262 MiB | 95–97 MiB |
| Captured fullscreen pause frame, summed GPU draw/copy events | 0.474 ms | 0.375 ms |
| Sampled decoded-cover allocations, copied owner library | 45.4 MiB | 21.3 MiB |
| Sampled total live heap, copied owner library | 127.6 MiB | 102.6 MiB |
| Desktop decoded-cover cache budget | 90 MiB | 56 MiB |
| Normal-process RSS after pause loop, copied owner library | 320 MiB | 285 MiB |

The GPU memory change uses wgpu's `MemoryUsage` allocation hint. Grid and Home shelf virtualization
release offscreen image components. Desktop disables the cover loader's duplicate decoded cache and
keeps its own LRU. Hidden sidebar/player layers and a page covered by an opaque fullscreen player do
not render. Top Picks combine four covers into one image to avoid independent GPU pixel rounding.

In a 60-second physical-display interaction trace, the updated app averaged 13.2% of one CPU core and
312–359 MiB RSS. Roughly 75% of sampled user CPU cycles were on the UI thread, 14% on analysis and 10%
on the playback engine. This includes pauses between interactions and has no matched physical-display
baseline. It does not establish animation frame percentiles or solve the remaining RAM cost.

Xvfb pause loops used about 76–79% of one CPU core. Instrumentation measured median painting at
0.76 ms and presentation at 27.08 ms. Refresh pacing did not improve that test and was discarded;
the virtual presentation bottleneck must not be attributed to the physical monitor.

The physical trace also exposed a playback stall: a silent pull after a skip advanced the ring past
discarded audio without updating the output's accounting. This created about ten seconds of phantom
device latency. A failing clock-based output test reproduces it; thirty rapid skips on the private
copy completed without the watchdog restarting playback after the correction.
