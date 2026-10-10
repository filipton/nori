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

## Direct GPU cover uploads

A further comparison against `2b4f8c95` used the same private library on Xvfb, one test process at a
time, and thirty alternating play/pause actions spaced 650 ms apart. These numbers are from separate
normal-process and jemalloc-profiled runs; profiling RSS is not used to measure the saving.

| Measurement | Before | After |
|---|---:|---:|
| Sampled total live heap | 103.1 MiB | 59.7 MiB |
| Anonymous resident memory after the loop | 114.3 MiB | 81.5 MiB |
| Normal-process RSS after the loop | 276.7 MiB | 223.4 MiB |
| Normal-process PSS after the loop | 218.0 MiB | 164.6 MiB |
| GPU process memory | 103 MiB | 123 MiB |
| Animation-loop CPU, percent of one core | 89.6% | 89.1% |

Decoded covers upload directly to Slint's GPU images on the compositor's device. This removes the
app's retained CPU pixel copies and Skia's raster copies. Top Picks copy the four equal square card
textures into one texture, without rasterizing or reading pixels back to the CPU. Cover regions in
the matched Home screenshots were pixel-identical. GPU cache entries keep the existing byte limits;
the GPU memory increase reflects covers now retained there instead of in the CPU heap.

The compositor also omits wgpu's unused indirect-command validation pipelines: every compositor draw
uses a fixed vertex range. Ordinary validation stays enabled. The NVIDIA compiler's large allocation
is still present when Skia builds its rendering pipelines; disabling the unused indirect pipelines
does not eliminate that driver allocation.

About 28 MiB of the remaining sampled heap belongs to NVIDIA driver allocations, and about 13 MiB
to the playback engine. RSS additionally includes code, shared libraries, driver mappings and
allocator slack. Library sharing makes RSS comparisons less stable than the anonymous and live-heap
measurements. This batch establishes a RAM reduction, not an animation CPU or frame-rate improvement.

## Resize rendering and repeated skips

Fresh Slint layer textures were drawn through Skia without being initialized in wgpu's tracker.
The first wgpu read therefore cleared the rendered pixels. The compositor forced another draw to
recover, leaving a black frame on each resize. It now initializes fresh attachments through wgpu
before Skia draws and transitions existing attachments back from sampling to rendering state.
The forced second redraw is removed.

A GPU regression failed with transparent black pixels before the change and passes after it.
In Xvfb recordings of repeated resizing on Home and fullscreen, the unchanged central region
contained 473 mostly black frames out of 542 before, and zero out of 572 after. Different frame
counts reflect recording duration; this checks flashing rather than frame pacing.

Six batches of thirty next/previous actions over already-requested tracks completed without
playback stalls. Anonymous resident memory was 90.1, 90.3, 85.0, 85.0, 85.1 and 85.2 MiB,
respectively. After warm-up it varied by less than 0.3 MiB across the last four batches.
This demonstrates a plateau for that workload, not proof that every path is leak-free.

## Mouse scrolling and border dragging

The custom backend now forwards wheel phases as Slint's native winit backend does. The public
`PointerScrolled` event marks wheels as cancelled, which makes Flickable jump immediately instead
of using its wheel deceleration. A virtual-clock regression failed before the phase correction;
it checks motion between notches and the final accumulated distance. Precise touchpad phases
remain intact. Shelves support mouse dragging, Shift+wheel and arrows.
Their Show all links open vertically scrollable, virtualized album or Top Picks grids.

Border-drag comparisons used a private xfwm4 session with compositing disabled on a 1920 × 1080
Xvfb screen. Each run dragged the corner and right border inward and outward on Home and fullscreen.
The observed client widths ranged from 1000 to 1280 pixels. Recordings contained no black flashes;
the final browsing build had zero mostly black frames out of 601.
With the same instrumented optimized build, reducing Linux's maximum queued frames from two to
one changed median whole-frame CPU time from 42.1 to 38.2 ms. Painting remained about 3.4 ms;
presentation dominated at 29.6 versus 28.2 ms. Vsync remains enabled. This modest virtual-display
improvement does not establish smooth physical-display resizing or a CPU/memory reduction.

## Linux GPU fault follow-up

A physical-display back-navigation failure was followed by a reboot. The previous boot's kernel
log records NVIDIA Xid 13 and Xid 31 (GPU memory read fault), attributed to the same nori-desktop
process as Slint's failed Vulkan texture import. This is a GPU fault, not just a UI exception.
Slint's Vulkan import wraps a borrowed image handle; its Skia image does not retain the wgpu texture
that owns it. Linux now supplies raster cover images and mosaics so Skia owns their GPU uploads.
The direct-upload memory figures above describe the earlier implementation; Linux restores CPU
cover copies in this fix. Other platforms retain their existing upload path. The one-frame latency
trial is reverted, and the horizontal shelf scrollbar is removed.

The compositor now keeps the physical window dimensions from resize events. Winit's X11
`inner_size()` makes a synchronous XGetGeometry request; drawing and pointer routing no longer
repeat that request. Resize events update the latest dimensions, and layer layout and surface
reconfiguration run once before drawing rather than on every intermediate resize event.
Software Vulkan navigation checks isolate the NVIDIA GPU, but cannot prove hardware-specific
fault recovery or 60 fps physical-display resizing.

Ten overview/back cycles passed with software Vulkan. One bounded Xvfb check of both collection
views on NVIDIA also passed, with no new kernel Xids during that run. Software-Vulkan corner and
side dragging produced zero mostly black frames out of 661, and the GPU pixel regression passed.
These are bounded checks, not a guarantee that every NVIDIA rendering path is fault-free.
