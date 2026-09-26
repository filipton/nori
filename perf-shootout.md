# Battery/CPU shootout: nori vs Symfonium vs musly vs Navic

Date: 2026-09-21 (UTC). Device: sdk_gphone64_x86_64 (Google), Android 14 (SDK 34), ABI x86_64
(abilist x86_64,arm64-v8a). Server: local Navidrome at http://10.0.2.2:4533, user admin.
Workload everywhere: track "Noise 1", album "Bench" (~10-min MP3 320 kbps) — no window crosses
a track boundary. No substitutions: all four apps played the exact track. Media volume 0,
screen OFF, no touches during every window. App versions: nori 0.2.0 (**debug** build —
see caveat), Symfonium 15.0.1, musly 2.0.2, Navic v1.0.0-alpha55.

## Headline table (screen-off playback 90 s, paused-in-background 30 s)

| App × scenario | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session |
|---|---|---|---|---|---|
| nori · MP3 screen-off 90 s | 2.48 | 340.5 | 74 / 90 | 193 | PLAYING |
| nori · FLAC screen-off 90 s | 2.07 | 202.8 | 73 / 90 | 210 | PLAYING |
| nori · paused in background 30 s | 0.06 | 1.7 | 29 / 30 | 173 | PAUSED |
| Symfonium · MP3 screen-off 90 s | 6.66 | 1746.7 | 1 / 90 | 117 | PLAYING |
| Symfonium · paused in background 30 s | 0.00 | 0.0 | 29 / 30 | 105 | PAUSED |
| musly · MP3 screen-off 90 s | 2.78 | 533.4 | 0 / 90 | 152 | PLAYING |
| musly · paused in background 30 s | 0.10 | 22.0 | 0 / 30 | 150 | PAUSED, holds `AudioService` wakelock |
| Navic · MP3 screen-off 90 s | 2.78 | 433.9 | 1 / 90 | 140 | PLAYING |
| Navic · paused in background 30 s | 0.00 | 1.4 | 29 / 30 | 127 | PAUSED, no wakelocks |

## Cold start (am force-stop, sleep 1, am start -W, TotalTime ms, ×3)

| App (launcher component) | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| nori (`dev.nori.music/.app.MainActivity`) | 1148 | 1137 | 1165 |
| Symfonium (`app.symfonik.music.player/app.symfonik.ui.MainActivity`) | 424 | 392 | 350 |
| musly (`com.devid.musly/.MainActivity`) | 804 | 730 | 760 |
| Navic (`paige.navic/.MainActivityDefault`) | 555 | 579 | 571 |

## simpleperf — nori playing Noise 1, screen off, 45 s, 10820 samples (top 40 by comm,dso,symbol)

```
Overhead  Command          Shared Object          Symbol
7.22%     ExoPlayer:Playb  libart.so              art::interpreter::ExecuteSwitchImplCpp<false>
3.50%     ExoPlayer:Media  libart.so              art::interpreter::ExecuteSwitchImplCpp<false>
2.21%     MediaCodec_loop  [kernel.kallsyms]      smp_call_function_many_cond
2.12%     ExoPlayer:Playb  libart.so              artQuickToInterpreterBridge
1.39%     ExoPlayer:Playb  libart.so              art::interpreter::DoCall<false>
1.29%     ExoPlayer:Playb  libart.so              NterpGetMethod
1.26%     ExoPlayer:Playb  libart.so              art::RuntimeCallbacks::HaveLocalsChanged()
1.01%     ExoPlayer:Playb  libart.so              art::ResolveFieldWithAccessChecks
0.97%     ExoPlayer:Media  libart.so              art::RuntimeCallbacks::HaveLocalsChanged()
0.88%     ExoPlayer:Playb  libart.so              art::interpreter::Execute
0.78%     MediaCodec_loop  libart.so              art::interpreter::ExecuteSwitchImplCpp<false>
0.75%     ExoPlayer:Media  libart.so              art::interpreter::DoCall<false>
0.74%     ExoPlayer:Media  libart.so              art::ResolveFieldWithAccessChecks
0.64%     ExoPlayer:Media  [kernel.kallsyms]      x86_pmu_disable_all
0.64%     MediaCodec_loop  libc.so                scudo::Allocator::allocate
0.59%     ExoPlayer:Media  libart.so              NterpGetMethod
0.59%     MediaCodec_loop  [kernel.kallsyms]      x86_pmu_disable_all
0.58%     ExoPlayer:Playb  libart.so              art::ArtMethod* ArtInterpreterToCompiledCodeBridge
0.54%     ExoPlayer:Playb  libart.so              art_quick_to_interpreter_bridge
0.53%     ExoPlayer:Playb  libart.so              art::FindMethodToCall<(InvokeType)2>
0.51%     ExoPlayer:Playb  libart.so              NterpGetInstanceFieldOffset
0.51%     ExoPlayer:Media  libart.so              art::FindMethodToCall<(InvokeType)2>
0.50%     MediaCodec_loop  [kernel.kallsyms]      x2apic_send_IPI
0.49%     ExoPlayer:Media  libart.so              ArtInterpreterToCompiledCodeBridge
0.46%     ExoPlayer:Playb  libc.so                memcpy
0.45%     MediaCodec_loop  libc.so                scudo::HybridMutex::unlock()
0.43%     ExoPlayer:Playb  libart.so              art::jit::Jit::MaybeDoOnStackReplacement
0.42%     ExoPlayer:Playb  libart.so              art::Thread::ObserveAsyncException()
0.41%     ExoPlayer:Playb  libart.so              artQuickGenericJniTrampoline
0.40%     ExoPlayer:Playb  libart.so              art::instrumentation::Instrumentation::NeedsSlowInterpreterForMethod
0.39%     MediaCodec_loop  [kernel.kallsyms]      queued_spin_lock_slowpath
0.36%     ExoPlayer:Playb  libart.so              art::Runtime::UseJitCompilation() const
0.35%     ExoPlayer:Playb  libart.so              art::EnsureInitialized
0.34%     MediaCodec_loop  libc.so                scudo::Allocator::quarantineOrDeallocateChunk
0.33%     MediaCodec_loop  libc.so                scudo::HybridMutex::tryLock()
0.33%     MediaCodec_loop  libutils.so            android::RefBase::incStrong(void const*) const
0.33%     ExoPlayer:Playb  [kernel.kallsyms]      x86_pmu_disable_all
0.33%     ExoPlayer:Playb  libart.so              art::jit::JitCodeCache::ContainsPc
0.32%     ExoPlayer:Playb  libc.so                memset_generic
0.32%     ExoPlayer:Playb  libart.so              ScopedCheck::CheckPossibleHeapValue (JNI check)
```

No Rust/DSP symbols anywhere near the top: EQ/limiter off, offload wanted, decoder is MediaCodec.
The profile is dominated by ART interpreter/JIT machinery on the ExoPlayer playback threads —
expected for a debug build with no AOT compilation (see caveat).

## VERDICT

**Where nori wins.** Screen-off playback sleep quality, by a mile: 74/90 quiet seconds (FLAC:
73/90) vs 1/90 (Symfonium, Navic) and 0/90 (musly), and the lowest wakeup rate (340/s MP3,
203/s FLAC vs 434–1747/s). That is the BurstSink + 10 s AudioTrack buffer design doing its
job: decode in bursts, then let the CPU sit in deep idle. Total CPU is jointly lowest
(2.48% MP3 / 2.07% FLAC, same band as musly/Navic 2.78%, far below Symfonium 6.66%). Paused
in background is fully clean (0.06%, no wakelocks — same as Symfonium/Navic at 0.00%).

**Where nori loses.** Cold start is the slowest: ~1.15 s vs 350–580 ms (Symfonium/Navic) and
~760 ms (musly) — roughly 2–3× the fastest. Memory (PSS) is the highest in every scenario:
193–210 MB playing, 173 MB paused, vs 105–152 MB elsewhere. Both are at least partly the
debug-build handicap (below), but startup time is worth profiling on a release build.

**Suspected causes, per app.**
- Symfonium 6.66% / 1746 wakeups/s: busiest threads are `ExoPlayer:Playb` (1400 ms) plus four
  `BG-1-T-*` workers at ~350 ms each, `ExoPlayer:Simpl` (470 ms), `AudioEngine/1` (310 ms) —
  i.e. a pool of background workers + an audio engine doing continuous work during playback.
  Paused it winds down slowly (two bench runs went negative-CPU from threads exiting
  mid-window) but reaches a true 0.00% steady state, so the playback cost is active
  processing, not a leak.
- musly never sleeps: quiet 0/90 playing AND 0/30 paused, ~22 wakeups/s paused forever, and it
  keeps a `com.ryanheise.audioservice.AudioService` wakelock while paused. Busiest threads are
  plain ExoPlayer + MediaCodec (Flutter/just_audio stack), plus `dart:io_EventHa`. Suspect:
  the audio-service plugin keeps the foreground service and its event loop alive regardless
  of state — cheap in CPU (0.10%) but it never lets the device go quiet.
- Navic 2.78% / 434 wakeups/s: its single busiest thread is the app main thread
  (`paige.navic`, 980 ms) ahead of `ExoPlayer:Playb` (840 ms) and `MediaCodec_loop`
  (640 ms) — UI/main-thread work (progress/state updates?) runs continuously during
  playback. Paused behavior is clean (0.00%).
- nori 2.48%: cost is `ExoPlayer:Playb` (1080–1200 ms) + `MediaCodec_loop` (640–700 ms) with
  everything else negligible; simpleperf shows that playback-thread time is overwhelmingly
  ART interpreter/JIT transitions rather than native decode or app code.

**Caveat — uneven builds.** nori was measured as a debug build (`dev.nori.music`, TestBridge
present): no AOT, JIT/interpreter overhead (visible in simpleperf), slower startup, larger
PSS. The other three are release builds. nori still leads on sleep quality despite the
handicap; the cold-start and PSS gaps should be re-run against a release build with baseline
profiles before drawing conclusions.

## Deviations from the procedure (all recorded)

1. nori was logged out (fresh reinstall). Logged in via UI taps (Server URL → admin/admin →
   Connect), the same flow as tools/perf-suite.sh — the TestBridge `login` hook only exists
   once the main UI is composed, so `app.sh login` cannot work while logged out.
2. Paused runs: added a settle wait after MEDIA_PAUSE before bench (20 s everywhere; 60 s
   re-run for Symfonium) because ExoPlayer/codec threads exiting mid-window produce negative
   CPU/wakeup deltas. Discarded artifacts: nori −1.83% (first run), Symfonium −6.33%;
   reported numbers are steady-state re-runs. musly's 0/30-quiet paused result reproduced
   after a 60 s settle — genuine, not transient.
3. `adb root` was required for simpleperf (perf_event permission); reverted with `adb unroot`
   afterwards. No code or device software was installed.
4. Symfonium's `.MainActivity.Note` launcher alias does not exist; cold start used the
   resolve-activity result `app.symfonik.ui.MainActivity` (TotalTime recorded the same way).
5. musly's transport controls are unlabeled Flutter nodes; play was tapped by coordinates
   (540,1860, center of the transport row). Playback verified via media_session PLAYING +
   description=Noise 1.
6. Symfonium resumed a previous queue at 04:00 into the 10:00 track — still Noise 1/Bench,
   no boundary crossed during the window.

## Appendix — full bench outputs

<details><summary>nori · MP3 320 screen-off 90 s</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      2240 ms  = 2.48% of one core
wakeups:  340.5 per second
quiet:    74 of 90 seconds with (almost) no wakeups
memory:   193 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  1200 ExoPlayer:Playb
  660 MediaCodec_loop
  260 ExoPlayer:Media
  260 ExoPlayer:Media
  120 HwBinder:3415_1
  40 Jit_thread_pool
  30 Profile_Saver
  10 OkHttp_Connecti
session after: state=PLAYING
```

</details>

<details><summary>nori · FLAC screen-off 90 s</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      1870 ms  = 2.07% of one core
wakeups:  202.8 per second
quiet:    73 of 90 seconds with (almost) no wakeups
memory:   210 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  1080 ExoPlayer:Playb
  700 MediaCodec_loop
  270 ExoPlayer:Media
  260 ExoPlayer:Media
  130 HwBinder:3415_1
  20 Profile_Saver
session after: state=PLAYING
```

</details>

<details><summary>nori · paused in background 30 s (steady-state re-run)</summary>

```
package: dev.nori.music   session: state=PAUSED   window: 30s   screen: off
cpu:      20 ms  = .06% of one core
wakeups:  1.7 per second
quiet:    29 of 30 seconds with (almost) no wakeups
memory:   173 MB PSS
wakelocks:
busiest threads (ms):
  20 ExoPlayer:Playb
session after: state=PAUSED
```

First run (discarded wind-down artifact): cpu −550 ms = −1.83%, wakeups −11.1/s.

</details>

<details><summary>Symfonium · MP3 screen-off 90 s</summary>

```
package: app.symfonik.music.player   session: state=PLAYING   window: 90s   screen: off
cpu:      6000 ms  = 6.66% of one core
wakeups:  1746.7 per second
quiet:    1 of 90 seconds with (almost) no wakeups
memory:   117 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  1400 ExoPlayer:Playb
  470 ExoPlayer:Simpl
  380 ik.music.player
  350 BG-1-T-4
  350 BG-1-T-3
  350 BG-1-T-2
  350 BG-1-T-1
  310 AudioEngine/1
session after: state=PLAYING
```

</details>

<details><summary>Symfonium · paused in background 30 s (steady-state re-run)</summary>

```
package: app.symfonik.music.player   session: state=PAUSED   window: 30s   screen: off
cpu:      0 ms  = 0% of one core
wakeups:  0 per second
quiet:    29 of 30 seconds with (almost) no wakeups
memory:   105 MB PSS
wakelocks:
busiest threads (ms):
session after: state=PAUSED
```

First run (discarded wind-down artifact): cpu −1900 ms = −6.33%, wakeups −2523.2/s.

</details>

<details><summary>musly · MP3 screen-off 90 s</summary>

```
package: com.devid.musly   session: state=PLAYING   window: 90s   screen: off
cpu:      2510 ms  = 2.78% of one core
wakeups:  533.4 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   152 MB PSS
wakelocks: 'AudioMix' 'com.ryanheise.audioservice.AudioService'
busiest threads (ms):
  1060 ExoPlayer:Playb
  870 MediaCodec_loop
  140 HwBinder:16592_
  140 ExoPlayer:Media
  130 ExoPlayer:Media
  80 com.devid.musly
  30 Jit_thread_pool
  30 dart:io_EventHa
session after: state=PLAYING
```

</details>

<details><summary>musly · paused in background 30 s</summary>

```
package: com.devid.musly   session: state=PAUSED   window: 30s   screen: off
cpu:      30 ms  = .10% of one core
wakeups:  22.0 per second
quiet:    0 of 30 seconds with (almost) no wakeups
memory:   150 MB PSS
wakelocks: 'com.ryanheise.audioservice.AudioService'
busiest threads (ms):
  20 com.devid.musly
  10 dart:io_EventHa
session after: state=PAUSED
```

(60 s-settle re-run; 20 s-settle run was nearly identical: 0.13%, 22.6/s, 0/30 quiet.)

</details>

<details><summary>Navic · MP3 screen-off 90 s</summary>

```
package: paige.navic   session: state=PLAYING   window: 90s   screen: off
cpu:      2510 ms  = 2.78% of one core
wakeups:  433.9 per second
quiet:    1 of 90 seconds with (almost) no wakeups
memory:   140 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  980 paige.navic
  840 ExoPlayer:Playb
  640 MediaCodec_loop
  130 Jit_thread_pool
  120 HwBinder:9329_1
  100 ExoPlayer:Media
  90 ExoPlayer:Media
  30 ExoPlayer:Loade
session after: state=PLAYING
```

</details>

<details><summary>Navic · paused in background 30 s</summary>

```
package: paige.navic   session: state=PAUSED   window: 30s   screen: off
cpu:      0 ms  = 0% of one core
wakeups:  1.4 per second
quiet:    29 of 30 seconds with (almost) no wakeups
memory:   127 MB PSS
wakelocks:
session after: state=PAUSED
```

</details>

## Release re-check (2026-09-21)

Same device (sdk_gphone64_x86_64, Android 14), same server, same workload
("Noise 1", album "Bench", 10-min MP3 320 kbps; "Noise flac"; media volume 0,
screen OFF, no touches). nori 0.2.0 as a **release** build
(`./gradlew :app:assembleRelease -PrustTargets="x86_64"`, the apk.sh pattern for
an emulator), installed with `adb install -r` over the debug build — same
debug-key signature, so data and login survived and no fresh login was needed.
`adb shell cmd package compile -m speed-profile -f` was NOT re-run; profiles
came from the install itself. Paused run used a 30 s settle after MEDIA_PAUSE.
Release builds have no TestBridge/app.sh hooks; all driving was via tools/ui.sh
taps (+ `input text` with `kb off`, restored to `kb on` at the end).

### Headline table — release vs debug vs competitors (debug/competitor rows repeated from above)

| App × scenario | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session |
|---|---|---|---|---|---|
| nori **release** · MP3 screen-off 90 s | 1.18 | 374.9 | 76 / 90 | 108 | PLAYING |
| nori **release** · FLAC screen-off 90 s | 1.24 | 408.3 | 75 / 90 | 125 | PLAYING |
| nori **release** · paused in background 30 s | 0.03 | 0.6 | 29 / 30 | 118 | PAUSED |
| nori debug · MP3 screen-off 90 s | 2.48 | 340.5 | 74 / 90 | 193 | PLAYING |
| nori debug · FLAC screen-off 90 s | 2.07 | 202.8 | 73 / 90 | 210 | PLAYING |
| nori debug · paused in background 30 s | 0.06 | 1.7 | 29 / 30 | 173 | PAUSED |
| Symfonium · MP3 screen-off 90 s | 6.66 | 1746.7 | 1 / 90 | 117 | PLAYING |
| musly · MP3 screen-off 90 s | 2.78 | 533.4 | 0 / 90 | 152 | PLAYING |
| Navic · MP3 screen-off 90 s | 2.78 | 433.9 | 1 / 90 | 140 | PLAYING |

### Cold start, release (am force-stop, sleep 1, am start -W, TotalTime ms, ×3)

| App (launcher component) | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| nori **release** (`dev.nori.music/.app.MainActivity`) | 289 | 268 | 285 |
| nori debug | 1148 | 1137 | 1165 |
| Symfonium | 424 | 392 | 350 |
| musly | 804 | 730 | 760 |
| Navic | 555 | 579 | 571 |

### simpleperf — nori release playing Noise 1, screen off, 45 s, 8560 samples (top 30 by comm,dso,symbol)

```
Overhead  Command          Shared Object          Symbol
4.51%     MediaCodec_loop  [kernel.kallsyms]      smp_call_function_many_cond
2.43%     ExoPlayer:Playb  libart.so              ExecuteNterpImpl
1.56%     ExoPlayer:Playb  libart.so              NterpGetShorty
1.26%     MediaCodec_loop  [kernel.kallsyms]      x2apic_send_IPI
1.06%     MediaCodec_loop  libc.so                scudo::Allocator::allocate
0.91%     MediaCodec_loop  [kernel.kallsyms]      queued_spin_lock_slowpath
0.85%     ExoPlayer:Media  [kernel.kallsyms]      x86_pmu_disable_all
0.83%     ExoPlayer:Playb  libart.so              art::ResolveFieldWithAccessChecks
0.80%     MediaCodec_loop  [kernel.kallsyms]      x86_pmu_disable_all
0.78%     MediaCodec_loop  libc.so                scudo::HybridMutex::tryLock
0.72%     ExoPlayer:Playb  libart.so              NterpGetMethod
0.68%     MediaCodec_loop  libutils.so            android::RefBase::incStrong
0.55%     MediaCodec_loop  libc.so                scudo::HybridMutex::unlock
0.53%     ExoPlayer:Playb  [kernel.kallsyms]      x86_pmu_disable_all
0.52%     MediaCodec_loop  libutils.so            android::RefBase::decStrong
0.52%     HwBinder:1732_1  libc.so                scudo::Allocator::allocate
0.48%     MediaCodec_loop  libc.so                memcpy
0.47%     ExoPlayer:Playb  libc.so                memcpy
0.47%     MediaCodec_loop  libc.so                pthread_mutex_lock
0.43%     MediaCodec_loop  libc.so                quarantineOrDeallocateChunk
0.42%     MediaCodec_loop  libc.so                deallocate
0.40%     Profile Saver    libprofile.so          FindOrAddHotMethod
0.38%     MediaCodec_loop  libsfplugin_ccodec.so  android::CCodec::onMessageReceived
0.38%     MediaCodec_loop  libdl.so               __cfi_slowpath
0.33%     MediaCodec_loop  libstagefright.so      __cfi_check
0.33%     HwBinder:1732_1  libc.so                scudo::HybridMutex::unlock
0.33%     Profile Saver    libart.so              ProfileSaver::GetClassesAndMethodsHelper::CollectInternal
0.32%     ExoPlayer:Playb  libart.so              NterpGetInstanceFieldOffset
0.31%     ExoPlayer:Playb  libart.so              nterp_op_invoke_virtual
0.30%     MediaCodec_loop  [vdso]                 clock_gettime
0.30%     ExoPlayer:Playb  libart.so              InvokeVirtualOrInterfaceWithVarArgs
```

(`adb root` required for simpleperf, reverted with `adb unroot` afterwards.
Session still PLAYING after the 45 s window; the static media-session position
is the known reporting trap, not a stall.)

### Comparison

**Vs the debug numbers: everything the caveat predicted.** CPU falls by roughly
half (MP3 2.48% → 1.18%, FLAC 2.07% → 1.24%) — the AOT-compiled playback path
replaces interpreter/JIT churn: debug's top symbols were
`ExecuteSwitchImplCpp` (7.22% + 3.50%) and `artQuickToInterpreterBridge`
(2.12%), while release tops out at `ExecuteNterpImpl` (2.43%) with kernel IPI
noise (`smp_call_function_many_cond`, 4.51%) above it. PSS drops ~40–45%
(193 → 108 MP3, 210 → 125 FLAC, 173 → 118 paused). Cold start drops ~4×
(~1150 ms → 268–289 ms). Sleep quality is unchanged (74 → 76 MP3, 73 → 75
FLAC quiet seconds) — it comes from the BurstSink design, not the build type.
One number moved the wrong way: FLAC wakeups/s doubled (202.8 → 408.3) while
CPU still fell; MP3 wakeups are flat-ish (340.5 → 374.9). Still the lowest
wakeup rate in the field, but worth a look if FLAC becomes the reference
workload.

**Vs competitors: nori release now leads every playback column.** Lowest CPU
(1.18% vs 2.78/2.78/6.66%), best sleep (76/90 quiet vs 1/0/1), lowest playing
PSS (108 MB vs 117/152/140 — the debug-build memory deficit is gone), and now
the fastest cold start (268–289 ms vs 350–804 ms). Paused is a three-way tie at
the floor (0.03%, 0.6/s, 29/30, no wakelocks; Symfonium holds the lowest
paused PSS at 105 MB vs 118 MB). No Rust/DSP symbols in the release top-30
either — EQ/limiter off, decoder MediaCodec, same as debug.

## Appendix — full bench outputs (release)

<details><summary>nori release · MP3 320 screen-off 90 s</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      1070 ms  = 1.18% of one core
wakeups:  374.9 per second
quiet:    76 of 90 seconds with (almost) no wakeups
memory:   108 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  450 MediaCodec_loop
  270 ExoPlayer:Playb
  100 HwBinder:30363_
  80 ExoPlayer:Media
  70 ExoPlayer:Media
  20 Jit_thread_pool
  10 Profile_Saver
session after: state=PLAYING
```

</details>

<details><summary>nori release · FLAC screen-off 90 s</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      1120 ms  = 1.24% of one core
wakeups:  408.3 per second
quiet:    75 of 90 seconds with (almost) no wakeups
memory:   125 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  560 MediaCodec_loop
  310 ExoPlayer:Playb
  130 HwBinder:30363_
  110 ExoPlayer:Media
  80 ExoPlayer:Media
  10 Profile_Saver
session after: state=PLAYING
```

</details>

<details><summary>nori release · paused in background 30 s</summary>

```
package: dev.nori.music   session: state=PAUSED   window: 30s   screen: off
cpu:      10 ms  = .03% of one core
wakeups:  .6 per second
quiet:    29 of 30 seconds with (almost) no wakeups
memory:   118 MB PSS
wakelocks:
busiest threads (ms):
  10 ExoPlayer:Playb
session after: state=PAUSED
```

(30 s settle after MEDIA_PAUSE; steady state reached immediately, no wind-down
artifact this time.)

</details>

## Paused waker hunt (2026-09-21)

Release build 0.2.0 (no TestBridge, driven via UI taps + media keys), Noise 1 /
Bench verified PLAYING via dumpsys, then MEDIA_PAUSE, 60 s settle, Home,
screen off, `tools/bench.sh dev.nori.music 300 off` with no other adb traffic
during the window. Session stayed PAUSED for the whole 300 s.

```
package: dev.nori.music   session: state=PAUSED   window: 300s   screen: off
cpu:      580 ms  = .19% of one core
wakeups:  26.9 per second
quiet:    0 of 300 seconds with (almost) no wakeups
memory:   100 MB PSS
wakelocks:
busiest threads (ms):
  360 dev.nori.music
  160 DefaultExecutor
  50 ExoPlayer:Playb
  10 DefaultDispatch
session after: state=PAUSED
```

Per-thread voluntary_ctxt deltas over 20 s (service alive): main
`dev.nori.music:1732` +208 (10.4/s, 20 ms CPU), `DefaultExecutor:2195` +208
(10.4/s, 10 ms CPU), `ExoPlayer:Playb` +20 (1/s, 0 ms CPU) — total 21.9/s,
matching the bench's 26.9/s within method noise. A separate 5 s × 24 sampler
showed deltas of 104–115 every single interval: a metronome-steady trickle,
no bursts, and not a single quiet second in 300.

Which thread/timer wakes: (1) the main/UI thread, woken by the Looper
(epoll) — simpleperf shows it doing `binder_transaction` /
`IPCThreadState::transact` plus obfuscated JIT frames (R8-minified release,
e.g. `xn0.run`, `sn1.r`); (2) `kotlinx.coroutines.DefaultExecutor`, parked in
`LockSupport.parkNanos`, in exact 1:1 lockstep with main. That pair is one
~100 ms recurring coroutine-delay task (`delay(~100)` → dispatch onto Main →
re-arm) firing ~10×/s while paused, backgrounded, screen off. (3)
`ExoPlayer:Playback` ticks at 1/s (50 ms / 300 s, at the profiling
threshold, no symbols of note). A SIGQUIT stack dump caught all three parked
(main/Playback in `Looper.pollOnce`, DefaultExecutor in `parkNanos`) — expected
for timer-driven wakeups; the 60 s simpleperf top-20 is diffuse message-passing
with no hot compute symbol (top lines: `NterpGetShorty` 1.99%,
`DefaultExecutor/art::Thread::Park` 1.10%, `binder_transaction` 0.90% on main).

Two surprises. First, this contradicts the 30 s Appendix run above (0.6/s,
29/30 quiet): the ~10 Hz timer is the steady state minutes after pausing, so
the 30 s run must have caught a quiescent phase (or the timer starts later) —
the 300 s number supersedes it for "paused in background". Second, the timer
is NOT the PlaybackService: ~11 min after pause ActivityManager logs
`Stopping service due to app idle: ... dev.nori.music/.playback.PlaybackService`,
the session is destroyed and `ExoPlayer:Playb` goes silent — but the
main+DefaultExecutor 10 Hz pair continues unchanged in the cached process
(20.6/s after the session is gone). No wakelocks held at any point (uid 10200,
checked twice); no AlarmManager/JobScheduler entries for the app.

Worth fixing? The absolute cost is tiny — 0.19% of one core, ~97k
wakeups/hour, each a few µs of CPU plus a binder hop; single-digit mW, a
couple percent of overnight idle drain at most. But it is pure waste (a paused
app has nothing to update 10×/s) and it costs the one thing nori otherwise
wins: deep-idle residency is 0/300 quiet seconds vs 29/30 for
Symfonium/Navic at the paused floor. If the ~100 ms polling flow (position /
seek-bar updater still armed while paused and backgrounded) is gated on
(playing || player screen visible), paused should drop to ~0 wakeups/s and
300/300 quiet — free parity with the field, and it keeps working past the
~11 min app-idle service stop, which does not kill the timer today.

## EQ-on shootout (2026-09-21)

Same device (sdk_gphone64_x86_64, Android 14), same server, same workload
("Noise 1", album "Bench", 10-min MP3 320 kbps; no window crosses a track
boundary except possibly Symfonium, see deviation 5), media volume 0
(STREAM_MUSIC muted, verified before each window), screen OFF, no touches,
`tools/bench.sh <pkg> 90 off` per app — with each app's equalizer/DSP turned
ON to a mild, comparable setting: EQ enabled + one treble band raised ~4–5 dB.
nori ran as the installed **debug** build (DEBUGGABLE flag set; the release
build was not on the device, so the task's TestBridge fallback applied —
`tools/app.sh` hooks for enable/play/verify, UI taps only for the EQ slider).
EQ-off reference rows are repeated from above; the apples-to-apples nori
comparison is debug-vs-debug.

### Per-app EQ setting used

| App | Setting (all others in the DSP page left off) |
|---|---|
| nori | Settings → Equalizer: enabled, 10-band graphic default, 8 kHz peaking Q1.41 **+5.1 dB**, pre-amp automatic. Verified in chain by logcat tag `nori` `equalizer in chain: 44100 Hz x2` on track start. |
| Symfonium | Output settings → Phone → Equalizer (Hi-Res DSP): **Graphic equalizer ON** (10-band), 8 kHz **+3.8 dB**, 4 kHz −0.4 dB, rest flat, pre-gain 0. Parametric/volume-boost/bass-boost/compressor/limiter/virtualizer/crossfeed all OFF, ReplayGain Off. (A stray tap briefly enabled Parametric with all 9 filters OFF = no processing; switched back OFF, verified.) |
| musly | **No equalizer exists**: full scroll of Settings → Playback (Auto DJ, crossfade, gapless, fade, lyrics, ReplayGain Off, transcoding) plus the player ••• sheet (Sleep Timer, Playback Speed, Preserve pitch) shows no EQ/bass-boost anywhere — expected for the just_audio stack. Measured stock, nothing changed. |
| Navic | Settings → Playback → Audio effects → Equaliser: source **Built-in** (Android effect, 5 bands ±15 dB), bands flat except band 5 (treble) ≈ **+4 dB**. Found state was Built-in with a non-flat curve (see deviation 4); restored afterwards. ReplayGain Off. |

### Headline table — EQ-on vs EQ-off (screen-off playback 90 s, Noise 1 MP3)

| App × EQ | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session |
|---|---|---|---|---|---|
| nori debug · **EQ on** (8k +5.1) | 2.94 | 432.9 | **0 / 90** | 179 | PLAYING |
| nori debug · EQ off | 2.48 | 340.5 | 74 / 90 | 193 | PLAYING |
| nori release · EQ off | 1.18 | 374.9 | 76 / 90 | 108 | PLAYING |
| Symfonium · **EQ on** (graph 8k +3.8) | 6.73 | 1805.3 | 1 / 90 | 128 | PLAYING |
| Symfonium · EQ off | 6.66 | 1746.7 | 1 / 90 | 117 | PLAYING |
| musly · **no EQ** (stock re-run) | 2.61 | 541.1 | 0 / 90 | 146 | PLAYING |
| musly · stock | 2.78 | 533.4 | 0 / 90 | 152 | PLAYING |
| Navic · **EQ on** (band5 +4 dB) | 2.81 | 475.5 | 0 / 90 | 148 | PLAYING |
| Navic · "off" (Built-in + found curve!) | 2.78 | 433.9 | 1 / 90 | 140 | PLAYING |

### Verdict — whose DSP costs what

**nori pays the most for EQ, relatively — and loses its signature advantage.**
Debug-vs-debug: CPU +0.46 pp (+19%), wakeups +27%, and deep sleep collapses
completely: quiet 74/90 → **0/90**. The busiest threads grow in place —
`ExoPlayer:Playb` 1200 → 1500 ms, `MediaCodec_loop` 660 → 800 ms — and a 45 s
simpleperf (adb root, reverted after; 11213 samples) shows the new cost centre:
`libnorimusic.so` contributes **2.97% total over 50 stripped entries**
(`libnorimusic.so[+offset]`, no Rust symbols — release-optimised .so) on the
playback threads, sitting under `artQuickGenericJniTrampoline`/CheckJNI on
`ExoPlayer:Playb`. The top of the profile is otherwise unchanged ART
interpreter/JIT churn (debug build). Reading: the EQ forces the sample-domain
path — an audio processor in the sink chain stands offload down (the state line
still says `offload:true`, i.e. wanted, but volume-0 PCM rendering runs
continuously) so the BurstSink burst-then-sleep pattern stops working and nori
sleeps exactly like the field (0 quiet seconds, same as musly/Navic). The Rust
DSP itself is cheap (~3% of samples); the sleep loss is the bigger bill. The
release-build EQ-on number was not measured (no TestBridge there); expect the
same shape on top of the 1.18% floor.

**Symfonium's EQ costs nothing measurable: 6.66% → 6.73%, 1746.7 → 1805.3/s**
(both within run-to-run noise), thread shape identical (`ExoPlayer:Playb`
1390, `Simpl` 460, four `BG-1-T-*` ~340 each; only change is `DefaultDispatch`
320 in place of `AudioEngine/1` 310). Its 6.7% base is dominated by background
workers + audio engine; a 10-band graphic EQ is a rounding error on top.

**Navic's EQ costs nothing measurable either: 2.78% → 2.81%** — unsurprising
twice over: it uses Android's built-in (AudioFlinger-side) Equalizer effect,
which doesn't even run in the app's threads (`paige.navic` main still busiest
at 1030 ms vs 980, `Playb` actually down 840 → 530, i.e. noise). Plus the
punchline: Navic was found with source **Built-in and a non-flat curve**
(pixel-measured ticks ≈ +12/+10/+1/−5/−3.5 dB across bands 1–5), so its
"EQ-off" baseline row was already EQ-on. DSP cost there: ~zero either way.

**musly has no EQ to turn on** (checked both settings tabs and player sheet);
the stock re-run (2.61%, 541.1/s) reproduces its baseline (2.78%, 533.4/s).

Net ranking with DSP on: Navic ≈ musly ≈ 2.6–2.8% < nori-debug-EQ 2.94% <<
Symfonium 6.73%. nori's EQ-on still sleeps no worse than anyone else — it just
no longer sleeps better.

### Deviations / notes (all recorded)

1. Installed nori was the debug build, not release as assumed — used the
   task's TestBridge fallback (`set eq true`, `play "search:noise 1"`, `state`;
   UI tap only for the 8 kHz slider, verified +5.1 dB via screen texts).
2. Symfonium's media-session position is unreliable (the known trap): its own
   UI showed 06:43 with 3:17 left before the window (105 s needed), but after
   pausing post-bench the session read Noise 2 at 1.1 s. If the track crossed
   into Noise 2 mid-window, both are 10-min Bench noise tracks; no silence
   crossed. Session stayed PLAYING throughout (bench header + after).
3. Navic as-found EQ curve (Built-in source, ticks at y 749/799/1073/1299/1249
   px ≈ bands boosted/1–2, cut/4–5) was pixel-recorded, Reset to flat for the
   run, then drag-restored to within ~20 px (≈ one 100 mB slider step) of found
   and verified by screenshot analysis. Navic's baseline row is therefore EQ-on.
4. nori's `offload:true` in `app.sh state` with EQ on means offload *wanted*;
   the quiet-0/90 shape says the render path fell back to PCM (processor in
   chain), as designed.
5. Navic's bench wakelock line lists musly's
   `com.ryanheise.audioservice.AudioService` alongside `AudioMix` — cross-talk
   from the uid grep (musly was paused, holding its characteristic paused
   wakelock per the baseline), not a Navic wakelock.

### Cleanup (all apps left as found, stock)

nori `set eq false`, 8 kHz band back to +0.1 dB (slider snaps; EQ off so the
chain is empty — functionally stock); Symfonium Graphic equalizer OFF, 8k/4k
bands back to ≈0 dB (EQ off, profile Custom/Not saved as found), Parametric
still OFF, ReplayGain still Off; Navic Built-in curve restored (3); musly
untouched then force-stopped (no session, as found); Symfonium force-stopped
after pausing (no session, as found); Navic/nori left paused/not-playing.

## Appendix — full bench outputs (EQ-on)

<details><summary>nori debug · EQ on (8 kHz +5.1 dB) screen-off 90 s</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      2650 ms  = 2.94% of one core
wakeups:  432.9 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   179 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  1500 ExoPlayer:Playb
  800 MediaCodec_loop
  270 ExoPlayer:Media
  260 ExoPlayer:Media
  140 HwBinder:20245_
  20 Profile_Saver
  20 Jit_thread_pool
session after: state=PLAYING
```

simpleperf, same session, 45 s, 11213 samples (top 40 by comm,dso,symbol;
`adb root`, reverted with `adb unroot` after):

```
Overhead  Command          Shared Object          Symbol
6.76%     ExoPlayer:Playb  libart.so              art::interpreter::ExecuteSwitchImplCpp<false>
3.31%     ExoPlayer:Media  libart.so              art::interpreter::ExecuteSwitchImplCpp<false>
1.70%     ExoPlayer:Playb  libart.so              artQuickToInterpreterBridge
1.67%     MediaCodec_loop  [kernel.kallsyms]      smp_call_function_many_cond
1.50%     ExoPlayer:Playb  libart.so              art::interpreter::DoCall<false>
1.28%     ExoPlayer:Playb  libart.so              art::RuntimeCallbacks::HaveLocalsChanged()
1.17%     ExoPlayer:Playb  libart.so              NterpGetMethod
0.90%     ExoPlayer:Playb  libart.so              art::ResolveFieldWithAccessChecks
0.77%     ExoPlayer:Playb  libart.so              art::interpreter::Execute
0.74%     ExoPlayer:Media  libart.so              art::interpreter::DoCall<false>
0.73%     MediaCodec_loop  libart.so              art::interpreter::ExecuteSwitchImplCpp<false>
0.70%     MediaCodec_loop  libc.so                scudo::Allocator::allocate
0.66%     ExoPlayer:Media  libart.so              art::RuntimeCallbacks::HaveLocalsChanged()
0.58%     ExoPlayer:Playb  libart.so              art::ArtInterpreterToCompiledCodeBridge
0.57%     ExoPlayer:Playb  libart.so              artQuickGenericJniTrampoline
0.55%     MediaCodec_loop  [kernel.kallsyms]      x2apic_send_IPI
0.54%     ExoPlayer:Playb  libart.so              art_quick_to_interpreter_bridge
0.51%     MediaCodec_loop  [kernel.kallsyms]      x86_pmu_disable_all
0.49%     ExoPlayer:Media  libart.so              NterpGetMethod
0.49%     MediaCodec_loop  [kernel.kallsyms]      x86_pmu_disable_all
0.46%     ExoPlayer:Media  libart.so              art::ResolveFieldWithAccessChecks
0.45%     ExoPlayer:Media  libart.so              ArtInterpreterToCompiledCodeBridge
0.45%     ExoPlayer:Media  libart.so              artQuickToInterpreterBridge
0.44%     ExoPlayer:Playb  libart.so              art::jit::Jit::MaybeDoOnStackReplacement
0.43%     ExoPlayer:Playb  libart.so              NterpGetInstanceFieldOffset
0.43%     MediaCodec_loop  libdl.so               __cfi_slowpath
0.42%     ExoPlayer:Playb  libc.so                memcpy
0.39%     MediaCodec_loop  libc.so                pthread_mutex_lock
0.39%     ExoPlayer:Playb  libc.so                memset_generic
0.37%     ExoPlayer:Playb  libart.so              art::QuickArgumentVisitor::VisitArguments
0.37%     ExoPlayer:Media  libart.so              art::FindMethodToCall<(InvokeType)2>
0.36%     ExoPlayer:Playb  libart.so              art::instrumentation::Instrumentation::NeedsDexPcEvents
0.35%     MediaCodec_loop  libc.so                scudo::HybridMutex::tryLock
0.35%     ExoPlayer:Playb  libart.so              art::Thread::ObserveAsyncException
0.35%     ExoPlayer:Playb  libart.so              art::ScopedCheck::CheckPossibleHeapValue (JNI check)
0.33%     ExoPlayer:Media  libart.so              art::GenericJniMethodEnd
0.33%     MediaCodec_loop  libutils.so            android::RefBase::incStrong
```

`libnorimusic.so` (stripped, offsets only) totals 2.97% over 50 entries on the
playback threads — the only new symbol group vs the EQ-off debug profile.

</details>

<details><summary>Symfonium · Graphic EQ on (8 kHz +3.8 dB) screen-off 90 s</summary>

```
package: app.symfonik.music.player   session: state=PLAYING   window: 90s   screen: off
cpu:      6060 ms  = 6.73% of one core
wakeups:  1805.3 per second
quiet:    1 of 90 seconds with (almost) no wakeups
memory:   128 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  1390 ExoPlayer:Playb
  460 ExoPlayer:Simpl
  380 ik.music.player
  340 BG-1-T-4
  340 BG-1-T-3
  340 BG-1-T-2
  340 BG-1-T-1
  320 DefaultDispatch
session after: state=PLAYING
```

</details>

<details><summary>musly · no EQ (stock) screen-off 90 s</summary>

```
package: com.devid.musly   session: state=PLAYING   window: 90s   screen: off
cpu:      2350 ms  = 2.61% of one core
wakeups:  541.1 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   146 MB PSS
wakelocks: 'AudioMix' 'com.ryanheise.audioservice.AudioService'
busiest threads (ms):
  980 ExoPlayer:Playb
  850 MediaCodec_loop
  150 HwBinder:6840_1
  130 ExoPlayer:Media
  120 ExoPlayer:Media
  70 com.devid.musly
  30 Jit_thread_pool
  30 dart:io_EventHa
session after: state=PLAYING
```

</details>

<details><summary>Navic · Built-in EQ on (band 5 +4 dB) screen-off 90 s</summary>

```
package: paige.navic   session: state=PLAYING   window: 90s   screen: off
cpu:      2530 ms  = 2.81% of one core
wakeups:  475.5 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   148 MB PSS
wakelocks: 'com.ryanheise.audioservice.AudioService' 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  1030 paige.navic
  650 MediaCodec_loop
  530 ExoPlayer:Playb
  130 HwBinder:9329_1
  100 ExoPlayer:Media
  90 ExoPlayer:Media
  40 Jit_thread_pool
  40 ExoPlayer:Loade
session after: state=PLAYING
```

(The `AudioService` wakelock is musly's, held while paused — see note 5 above.)

</details>

## Crossfade / AutoMix shootout (2026-09-21)

Same device (sdk_gphone64_x86_64, Android 14), same server
(http://10.0.2.2:4533, admin), same workload everywhere: album "First Light"
by **Alpha Waves** — Track 1 (00:43), Track 2 (00:46), Track 3 (00:49),
Track 4 (00:52), total 190 s (03:10) — tracks 1–4 back-to-back, so one
`tools/bench.sh <pkg> 200 off` window (15 s settle + 200 s) covers all 3
transitions. Media volume 0, screen OFF, no touches. nori ran as the
installed **debug** build (TestBridge `app.sh` hooks); the other three are
release builds. Each app's transition feature ON, exact setting recorded
below; features turned OFF (or restored) afterwards.

### Per-app transition setting used

| App | Setting (everything else as found) |
|---|---|
| nori | **AutoMix ON** (all sub-options at defaults: longest mix 12 s, match-the-beat on, keep-pitch on, bass swap on, muffle-ending on, echo-out on; plain crossfade 0 s) + **"Keep albums gapless" OFF** (default ON would suppress all mixing inside one album, which is exactly this workload). Verified via `app.sh state` (`autoMix:true`). |
| Symfonium | Playback → Transitions → **Crossfade enabled** (expanded state; Fade in/out Disabled, Smart fades OFF, Mix only shown, Fade curves Disabled — no duration value is displayed anywhere, so the default duration applies). CAUTION, read deviation 1. |
| musly | Settings → Playback → **Track Crossfade slider at ~10 s** (found already ON at 10 s, kept; Gapless ON as found, Fade In/Out OFF as found). Turned to Off afterwards. |
| Navic | **No transition feature exists**: Playback settings offer only Streaming quality, Explicit, Audio effects (= equalizer), Auto-fill queue (as found) and scrobbling. Measured stock, nothing changed. |

Queueing: nori `play album:308NdbnGJmq0qLrgA6LHUJ`; musly album screen → Track 1
tap; Navic search → Albums → First Light/Alpha Waves → Play. Symfonium is the
exception (deviation 1): it states "Crossfade is disabled when playing albums
in sequential order", so the 4 tracks were multi-selected and played as a
manual queue ("1 of 4", 03:10) instead of an album play.

### Headline table — transitions ON, screen-off 200 s over 3 track boundaries

| App × transition | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session after |
|---|---|---|---|---|---|
| nori debug · **AutoMix on** | 1.04 | 177.6 | **151 / 200** | 178 | PLAYING (auto-fill carried on past the album) |
| Symfonium · **Crossfade on** (manual queue) | 1.83 | 382.2 | 117 / 200 | 124 | NONE (queue of 4 exhausted) |
| musly · **Crossfade 10 s** | 1.22 | 208.9 | 3 / 200 | 135 | PLAYING (Track 4, pos 52 s) |
| Navic · **stock** (no such feature) | 0.60 | 62.7 | 108 / 200 | 133 | STOPPED (queue of 4 exhausted) |

Plain-playback baselines for reference (Noise 1 MP3, 90 s, no boundary):
nori-debug 2.48% / 340.5/s / 74/90 · Symfonium 6.66% / 1746.7/s / 1/90 ·
musly 2.78% / 533.4/s / 0/90 · Navic 2.78% / 433.9/s / 1/90.

### Verdict — transitions vs plain playback

**Every app measures cheaper here than on its Noise 1 baseline** (CPU roughly
halved or better across the board), so the First Light material itself decodes
cheaper than the 320 kbps Noise track — the within-shootout ranking below is
apples-to-apples, but do not compare these rows against the baseline rows as
feature-cost deltas. With that caveat, ranked by CPU with transitions on:
**Navic-stock 0.60% < nori-AutoMix 1.04% < musly-xfade 1.22% < Symfonium-xfade
1.83%.**

- **nori keeps its sleep crown even while DJ-mixing: 151/200 quiet seconds.**
  AutoMix stands offload down (`offloadWanted:false` with the feature on — it
  is a sample-domain mix, like EQ), yet unlike EQ-on (0/90 quiet) the deep
  sleep survives: a 12 s-max mix inside ~45 s tracks leaves ~30 s of straight
  playback per track where the burst-then-sleep pattern still works; only the
  transitions cost extra. Thread shape: `ExoPlayer:Playb` 2000 ms and, notably,
  `nori-analyse-ah` 1260 ms — these tracks had never been analysed, so the
  one-time tempo/beat analysis ran inside the window (a repeat run would shed
  that). No `MediaCodec_loop` in the top threads. Debug-build caveat stands.
- **Symfonium is unrecognisable vs its baseline: 6.66% → 1.83%,
  1746.7 → 382.2/s, quiet 1/90 → 117/200.** The baseline's busy signature
  (Simpl 470, four BG workers ~350 each, AudioEngine) is still visible in
  miniature, but totals are ~3.5× lower. Either the baseline's 10-minute Noise
  queue triggered something pathological (waveform work? long-track buffering?)
  or the manual short-track queue behaves differently — unexplained, needs a
  re-check before claiming crossfade is "cheap" for Symfonium. Crossfade
  engagement itself is assumed (manual queue source), with no direct
  observable at volume 0.
- **musly's 10 s crossfade on ~45 s tracks (≈22% overlap, the heaviest mix in
  the shootout) costs little CPU (1.22%) but it still never sleeps: 3/200
  quiet.** Same story as baseline — `com.devid.musly` main (490 ms) +
  `dart:io_EventHa` (250 ms) keep the event loop churning under the
  `AudioService` wakelock regardless of transitions. No `MediaCodec_loop` in
  its top threads either.
- **Navic wins CPU at 0.60% by doing nothing: no transition feature, stock
  gapless-ish album play**, and its main thread is still its busiest (840 ms
  vs `Playb` 640 ms — the main-thread-during-playback signature from the
  baseline, now at a fraction of the absolute cost). Sleep is good but not
  nori-good (108/200 vs 151/200); wakeups lowest in the field (62.7/s).

Net: nobody pays a dramatic battery price for transitions on this material —
the feature-cost ordering is lost in the material-effect noise. The durable
findings are nori-AutoMix sleeping through a mix-heavy workload (151/200),
musly never sleeping with or without crossfade, and Symfonium's baseline
busyness not reproducing here (open question, not a conclusion).

### Deviations / notes (all recorded)

1. Symfonium crossfade on/off was inferred from child-option visibility
   (expanded = enabled with Smart fades/curves shown; collapsed = disabled) —
   no duration or on/off summary is displayed. Because of its
   no-crossfade-on-sequential-albums rule, its queue was a manual 4-track
   selection play, while the other three apps played the album normally.
2. musly's Track Crossfade was found already at ~10 s (set up by the previous
   interrupted attempt, or a default) and kept for the run; slider set to Off
   afterwards (screenshot-verified "Off (Instant transition)").
3. The 190 s album is shorter than the 215 s bench span: nori's auto-fill
   extended the queue (4 → 19 tracks, ended on a Beta Band track, PLAYING);
   Symfonium/Navic exhausted their queues (NONE/STOPPED); musly was still on
   Track 4. Every window covers all 3 transitions; tails differ, as recorded.
4. nori transition evidence: queue advanced Track 1 idx0/4 → idx5/19 plus one
   logcat line `nori: mixing: the next track arrived 5 ms into the hold with
   8375 ms of sound left` (later TestBridge calls clear logcat, so only one
   line was captured). The media-session metadata went stale mid-run (kept
   reporting an old track) — the known reporting trap, not a stall.
5. Typing used `input text` with `kb off`, restored to `kb on` at the end.

### Cleanup (all apps left stock / at rest)

nori `autoMix false`, `crossfadeKeepAlbums true` (both defaults), paused;
Symfonium Crossfade OFF (collapsed state, verified) then force-stopped (no
session); musly Track Crossfade Off (verified) then force-stopped (no
session); Navic queue stopped by itself, settings untouched.

## Appendix — full bench outputs (crossfade / AutoMix)

<details><summary>nori debug · AutoMix on (defaults, gapless-albums OFF) screen-off 200 s</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 200s   screen: off
cpu:      2080 ms  = 1.04% of one core
wakeups:  177.6 per second
quiet:    151 of 200 seconds with (almost) no wakeups
memory:   178 MB PSS
wakelocks: 'ExoPlayer:WakeLockManager' 'AudioMix'
busiest threads (ms):
  2000 ExoPlayer:Playb
  1260 nori-analyse-ah
  470 HwBinder:8227_1
  320 dev.nori.music
  70 HeapTaskDaemon
  60 Profile_Saver
  50 Jit_thread_pool
  50 GrallocUploadTh
session after: state=PLAYING
```

</details>

<details><summary>Symfonium · Crossfade on (manual 4-track queue) screen-off 200 s</summary>

```
package: app.symfonik.music.player   session: state=PLAYING   window: 200s   screen: off
cpu:      3670 ms  = 1.83% of one core
wakeups:  382.2 per second
quiet:    117 of 200 seconds with (almost) no wakeups
memory:   124 MB PSS
wakelocks:
busiest threads (ms):
  1380 ExoPlayer:Playb
  460 ik.music.player
  400 BG-1-T-2
  390 BG-1-T-4
  390 BG-1-T-1
  380 BG-1-T-3
  350 AudioEngine/1
  260 SessionHandler
session after: state=NONE
```

</details>

<details><summary>musly · Track Crossfade 10 s (Gapless on, Fade off) screen-off 200 s</summary>

```
package: com.devid.musly   session: state=PLAYING   window: 200s   screen: off
cpu:      2450 ms  = 1.22% of one core
wakeups:  208.9 per second
quiet:    3 of 200 seconds with (almost) no wakeups
memory:   135 MB PSS
wakelocks: 'com.ryanheise.audioservice.AudioService'
busiest threads (ms):
  1410 ExoPlayer:Playb
  490 com.devid.musly
  250 dart:io_EventHa
  200 HwBinder:17939_
  20 Jit_thread_pool
  20 HeapTaskDaemon
session after: state=PLAYING
```

</details>

<details><summary>Navic · stock (no transition feature) screen-off 200 s</summary>

```
package: paige.navic   session: state=PLAYING   window: 200s   screen: off
cpu:      1210 ms  = .60% of one core
wakeups:  62.7 per second
quiet:    108 of 200 seconds with (almost) no wakeups
memory:   133 MB PSS
wakelocks:
busiest threads (ms):
  840 paige.navic
  640 ExoPlayer:Playb
  130 HwBinder:18638_
  50 Jit_thread_pool
  40 Profile_Saver
  10 HeapTaskDaemon
session after: state=STOPPED
```

</details>

## FLAC shootout (2026-09-21)

Same device (sdk_gphone64_x86_64, Android 14), same server
(http://10.0.2.2:4533, admin), same shape of workload as the MP3 baseline:
10-minute noise track, media volume 0 (STREAM_MUSIC muted, streamVolume 0 —
verified via dumpsys audio), screen OFF, no touches,
`tools/bench.sh <pkg> 90 off` per app. Track everywhere: "Noise flac",
album "Long Play" by Bench (4 tracks, 40 min; FLAC 44.1 kHz, 882 kbps per
Navic's now-playing readout) — no window crosses a track boundary. nori
rows are the installed release build, repeated from above; competitors are
release builds (Symfonium 15.0.1, musly 2.0.2, Navic v1.0.0-alpha55).

### Headline table — FLAC vs MP3 (screen-off playback 90 s, FLAC rows new)

| App × codec | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session |
|---|---|---|---|---|---|
| Symfonium · **FLAC** | 7.41 | 1954.8 | 0 / 90 | 128 | PLAYING |
| Symfonium · MP3 | 6.66 | 1746.7 | 1 / 90 | 117 | PLAYING |
| musly · **FLAC** | 3.76 | 581.3 | 0 / 90 | 156 | PLAYING |
| musly · MP3 | 2.78 | 533.4 | 0 / 90 | 152 | PLAYING |
| Navic · **FLAC** (flat EQ, see deviation 1) | 4.44 | 625.2 | 0 / 90 | 142 | PLAYING |
| Navic · MP3 (found non-flat curve) | 2.78 | 433.9 | 1 / 90 | 140 | PLAYING |
| nori release · FLAC | 1.24 | 408.3 | 75 / 90 | 125 | PLAYING |
| nori release · MP3 | 1.18 | 374.9 | 76 / 90 | 108 | PLAYING |

### Verdict — heavier decode, same ranking, wider gap

**Nobody keeps up under FLAC, and the gap widens.** nori release plays the
FLAC at 1.24% / 408 wakeups/s / 75/90 quiet — essentially its MP3 numbers
(+0.06 pp CPU, +33/s wakeups, sleep unchanged). The cheapest competitor on
FLAC is musly at 3.76% (3× nori), then Navic 4.44%, then Symfonium 7.41%
(6×). Sleep is the same story as MP3, untouched by codec: nori 75/90 quiet
seconds vs 0/90 everywhere else.

**Vs each app's MP3 baseline, the heavier decode shows up in CPU but not in
sleep — because nobody slept to begin with.** There is no nori-style sleep
collapse anywhere (nori's EQ-on collapse was 74/90 → 0/90; here the field
sits at 0–1/90 on both codecs). Per app:
- musly +35% CPU (2.78% → 3.76%), wakeups +9%: the decode cost is directly
  visible — `MediaCodec_loop` 870 → 1250 ms, now clearly its #2 thread
  behind `ExoPlayer:Playb` (1360 ms). Shape otherwise identical
  (AudioService wakelock, `dart:io_EventHa` churning, 0 quiet seconds).
- Navic +60% CPU (2.78% → 4.44%), wakeups +44%: the steepest relative
  climb, and it lands on the app main thread — `paige.navic` 980 →
  1750 ms, still busier than `Playb` (590 ms) and `MediaCodec_loop`
  (750 ms). Whatever the main thread does during playback, it scales with
  decode cost. (Caveat: FLAC ran flat-EQ vs the MP3 baseline's found
  curve — deviation 1 — but the EQ round measured Navic's EQ cost at
  ~zero, so this delta reads as decode.)
- Symfonium +11% CPU (6.66% → 7.41%), wakeups +12%: the smallest relative
  move — same lesson as its EQ round (DSP cost ~zero there too). Its base
  is dominated by background workers + audio engine (`Playb` 1490, four
  `BG-1-T-*` ~400 each, `AudioEngine/1` 440, `Simpl` 360 — the exact MP3
  signature, scaled up), so decode is a rounding error on top of whatever
  those workers do.

**The asymmetry that matters:** nori's BurstSink burst-then-sleep pattern
survives heavier decode (73–76 quiet seconds on every nori row in this
file, MP3 or FLAC, debug or release) and only broke under sample-domain
DSP (EQ-on: 0/90). The field never sleeps in any configuration measured
so far — codec, EQ, crossfade/AutoMix all read 0–3 quiet seconds outside
of paused states and the anomalous Symfonium crossfade run.

### Deviations / notes (all recorded)

1. Navic FLAC ran with a flat Built-in EQ per the task's "EQ off
everywhere"; its MP3 baseline row was (unknowingly) the found non-flat
curve. Screenshots before/after: found thumb centres
(771/819/1073/1278/1228 px) were drag-restored to within ~22 px (~70 mB,
under one 100 mB slider step) of the photographed positions; source stayed
Built-in, ReplayGain stayed Off, no transition feature exists (stock).
Setup for Navic exceeded the 10 min budget because of this flatten/restore
detour — recorded, not skipped.
2. musly's search field dropped the typed space (`input text` %s handling),
so it played via Library → Long Play album → track 4 "Noise flac" — same
track, verified via session description (Noise flac, Bench, Long Play).
Stock verified, untouched: Track Crossfade Off (Instant transition), Auto
DJ Off, no EQ exists.
3. Symfonium stock verified, untouched: Graphic/Parametric/Volume/Bass EQ
toggles all off, ReplayGain Off, Crossfade collapsed (= off, as left by the
crossfade round). Volume-0 verified via dumpsys audio (Muted,
streamVolume 0) before its window; same `volume --set 0` for the others.
4. Cleanup: Symfonium paused (keyevent 85 — `dispatch MEDIA_PAUSE` is not a
valid subcommand) then force-stopped, no session; musly paused (PAUSED
verified) then force-stopped, no session — its paused AudioService
wakelock is not left held; Navic EQ restored (1) and left paused on Noise
flac. Keyboard IME re-enabled (`ui.sh kb on`) after the musly/Navic runs.

## Appendix — full bench outputs (FLAC)

<details><summary>Symfonium · FLAC screen-off 90 s</summary>

```
package: app.symfonik.music.player   session: state=PLAYING   window: 90s   screen: off
cpu:      6670 ms  = 7.41% of one core
wakeups:  1954.8 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   128 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  1490 ExoPlayer:Playb
  440 AudioEngine/1
  420 ik.music.player
  410 BG-1-T-3
  400 BG-1-T-4
  400 BG-1-T-2
  400 BG-1-T-1
  360 ExoPlayer:Simpl
session after: state=PLAYING
```

</details>

<details><summary>musly · FLAC screen-off 90 s</summary>

```
package: com.devid.musly   session: state=PLAYING   window: 90s   screen: off
cpu:      3390 ms  = 3.76% of one core
wakeups:  581.3 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   156 MB PSS
wakelocks: 'com.ryanheise.audioservice.AudioService' 'AudioMix' 
busiest threads (ms):
  1360 ExoPlayer:Playb
  1250 MediaCodec_loop
  190 HwBinder:11700_
  190 ExoPlayer:Media
  160 ExoPlayer:Media
  100 com.devid.musly
  50 ExoPlayer:Loade
  40 dart:io_EventHa
session after: state=PLAYING
```

</details>

<details><summary>Navic · FLAC screen-off 90 s (flat EQ)</summary>

```
package: paige.navic   session: state=PLAYING   window: 90s   screen: off
cpu:      4000 ms  = 4.44% of one core
wakeups:  625.2 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   142 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  1750 paige.navic
  750 MediaCodec_loop
  590 ExoPlayer:Playb
  240 DefaultDispatch
  180 Jit_thread_pool
  170 ExoPlayer:Loade
  150 HwBinder:26152_
  120 ExoPlayer:Media
session after: state=PLAYING
```

</details>

## nori release EQ-on (2026-09-21)

Same device (sdk_gphone64_x86_64, Android 14), same server
(http://10.0.2.2:4533, admin), same workload ("Noise 1", album "Bench",
10-min MP3 320 kbps; no window crosses a track boundary), media volume 0,
screen OFF, no touches. nori 0.2.0 as a **release** build
(`./gradlew :app:assembleRelease -PrustTargets="x86_64"`, installed with
`adb install -r` over the debug build — same debug-key signature, so login
survived). No TestBridge in release: login state, EQ setup, EQ teardown and
playback all driven via tools/ui.sh taps (+ `input text` with `kb off`,
restored to `kb on` at the end), exactly like the competitor runs. EQ
setting replicated from the EQ-on shootout: Settings → Equalizer and
crossfeed → enabled, 10-band graphic default, **8 kHz +5.1 dB**, everything
else flat/off (pre-amp automatic −5.1 dB, crossfeed Off, limiter/mono
untouched). Verified in chain by logcat `nori: equalizer in chain: 44100 Hz
x2` on track start; playback verified PLAYING via dumpsys media_session
(description=Noise 1).

### Headline table — release EQ-on vs release EQ-off vs debug EQ-on (screen-off playback 90 s, Noise 1 MP3)

| nori × EQ | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session |
|---|---|---|---|---|---|
| nori **release · EQ on** (8k +5.1) | 1.33 | 389.5 | 73 / 90 | 107 | PLAYING |
| nori release · EQ off | 1.18 | 374.9 | 76 / 90 | 108 | PLAYING |
| nori debug · EQ on (8k +5.1) | 2.94 | 432.9 | 0 / 90 | 179 | PLAYING |

### Verdict — on release, EQ is cheap and sleep survives

Vs release EQ-off (measured): CPU +0.15 pp (1.18% → 1.33%, +13%),
wakeups +15/s (374.9 → 389.5), quiet 76/90 → 73/90, PSS flat (108 →
107 MB). Thread shape grows in place like the debug run but at release
scale: `MediaCodec_loop` 450 → 510 ms, `ExoPlayer:Playb` 270 → 390 ms.

Vs debug EQ-on (measured): CPU −1.61 pp (2.94% → 1.33%), and — the big
one — deep sleep survives: quiet 0/90 → **73/90**. The debug EQ-on
conclusion ("the BurstSink burst-then-sleep pattern stops working") does
not reproduce on release: with AOT-compiled playback the burst pattern
keeps working through the sample-domain path, and EQ-on sleeps like
EQ-off. A 45 s simpleperf (`adb root`, reverted with `adb unroot` after;
8739 samples) shows `libnorimusic.so` at **5.89% total over 60 stripped
entries** (offsets only, all on `ExoPlayer:Playb`, top entry +1a9c19 at
0.48%) vs 2.97% over 50 on debug — a larger *share* of a much smaller
profile (debug's ART interpreter/JIT churn is gone; release top symbol is
kernel `smp_call_function_many_cond` at 4.10%). The Rust DSP is audibly
working (in chain per logcat, 60 hot entries) yet costs ~0.15 pp — the
sleep loss was the debug bill, not the DSP bill.

Net ranking with DSP on, release terms: nori-EQ 1.33% vs musly-stock
2.61% / Navic-EQ 2.81% / Symfonium-EQ 6.73% — nori keeps the lead even
with the equalizer engaged, and keeps 73/90 quiet seconds against 0–1/90
for the field.

### Cleanup

EQ toggled OFF (Sound row back to "Equalizer and crossfeed: Off"); 8 kHz
band reset via the Flat preset to +0.0 dB (note: applying the preset
re-enabled EQ, so the switch was toggled off *after* the reset and the
Off state re-verified). Pre-amp back to +0.0 dB automatic, crossfeed still
Off, keyboard restored, screen off. Release build left installed.

## Appendix — full bench output (nori release EQ-on)

<details><summary>nori release · EQ on (8 kHz +5.1 dB) screen-off 90 s</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      1200 ms  = 1.33% of one core
wakeups:  389.5 per second
quiet:    73 of 90 seconds with (almost) no wakeups
memory:   107 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  510 MediaCodec_loop
  390 ExoPlayer:Playb
  110 HwBinder:10594_
  100 ExoPlayer:Media
  70 ExoPlayer:Media
  10 Profile_Saver
  10 Jit_thread_pool
session after: state=PLAYING
```

simpleperf, same session, 45 s, 8739 samples (top 20 by comm,dso,symbol;
`adb root`, reverted with `adb unroot` after):

```
Overhead  Command          Shared Object          Symbol
4.10%     MediaCodec_loop  [kernel.kallsyms]      smp_call_function_many_cond
1.17%     MediaCodec_loop  [kernel.kallsyms]      x2apic_send_IPI
1.01%     ExoPlayer:Media  [kernel.kallsyms]      x86_pmu_disable_all
0.98%     MediaCodec_loop  [kernel.kallsyms]      queued_spin_lock_slowpath
0.97%     ExoPlayer:Playb  libart.so              ExecuteNterpImpl
0.88%     ExoPlayer:Playb  libart.so              NterpGetShorty
0.88%     MediaCodec_loop  libc.so                scudo::Allocator::quarantineOrDeallocateChunk
0.87%     MediaCodec_loop  libc.so                scudo::Allocator::allocate
0.83%     MediaCodec_loop  [kernel.kallsyms]      x86_pmu_disable_all
0.79%     ExoPlayer:Playb  [kernel.kallsyms]      x86_pmu_disable_all
0.76%     MediaCodec_loop  libc.so                scudo::HybridMutex::unlock
0.66%     MediaCodec_loop  libutils.so            android::RefBase::incStrong
0.64%     MediaCodec_loop  libc.so                scudo::HybridMutex::tryLock
0.63%     MediaCodec_loop  libc.so                pthread_mutex_lock
0.60%     MediaCodec_loop  libutils.so            android::RefBase::decStrong
0.51%     MediaCodec_loop  libc.so                scudo::Allocator::deallocate
0.49%     HwBinder:10594_  libc.so                scudo::Allocator::allocate
0.48%     ExoPlayer:Playb  libnorimusic.so        libnorimusic.so[+1a9c19]
0.46%     MediaCodec_loop  libdl.so               __cfi_slowpath
0.44%     HwBinder:10594_  [kernel.kallsyms]      x86_pmu_disable_all
```

`libnorimusic.so` (stripped, offsets only) totals 5.89% over 60 entries,
all on `ExoPlayer:Playb` (next entries: +1a9c73 0.38%, +1a9c22 0.29%,
+1a9b08 0.27%, +1a9b02 0.24%, +1a9bfd 0.24%, +1a9bf5 0.22%, +1a9a6c 0.18%,
…).

</details>

## nori release final gaps (2026-09-21)

Two remaining release-build measurements (debug numbers existed; owner
asked for release-only data). Same device (sdk_gphone64_x86_64,
Android 14), same server (http://10.0.2.2:4533, admin, already logged
in), media volume 0 (STREAM_MUSIC muted), screen OFF, no touches.
Release build verified before the runs (`dumpsys package`:
flags=0x0, no DEBUGGABLE, no TestBridge) — everything driven via
tools/ui.sh taps (+ `input text` with `kb off`, restored to `kb on` at
the end). Workload: album "First Light" by **Alpha Waves** — Track 1
(00:43), Track 2 (00:46), Track 3 (00:49), Track 4 (00:52), total
190 s — same as the debug crossfade/AutoMix round, so the comparison
below is apples-to-apples on material.

RUN 1 setup: Settings → Playing showed AutoMix already ON with
defaults (longest mix 12 s, match-the-beat/biggest-change/pitch/bass/
muffle/echo all on, "Keep albums gapless" already OFF) — the exact
requested state except crossfade ("Fade in and out"), which was set to
Off. Album played from Track 1 via search; verified PLAYING
(description=Track 1 of First Light) before screen-off. `bench 200`
covers all 3 transitions (plus auto-fill tail, as in the debug run).

RUN 2 setup: keyevent 127 pause after RUN 1 (PAUSED verified), 60 s
settle counting the post-RUN-1 pause plus extra sleep, Home, screen
off, `bench 90`.

### Headline table — release vs debug (transitions 200 s over 3 boundaries; paused 90 s)

| nori × scenario | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session |
|---|---|---|---|---|---|
| **release · AutoMix on** (gapless-albums OFF) 200 s | 0.37 | 189.2 | 159 / 200 | 103 | PLAYING |
| debug · AutoMix on (gapless-albums OFF) 200 s | 1.04 | 177.6 | 151 / 200 | 178 | PLAYING |
| **release · paused in background** 90 s | 0.01 | 1.4 | 89 / 90 | 97 | PAUSED |
| debug-fix · paused in background 90 s | 0.06 | 1.3 | 89 / 90 | 151 | PAUSED |

(Debug reference rows: transitions 1.04%/177.6/s/151/200 from the
crossfade/AutoMix round; paused-fix 0.06%/1.3/s/89/90.)

### Comparison

**Transitions: CPU nearly 3× lower on release (1.04% → 0.37%), sleep a
touch better (151 → 159 quiet seconds), PSS −42% (178 → 103 MB).**
Wakeups flat (177.6 → 189.2/s, within material noise). The biggest
visible change is thread shape: debug paid 1260 ms of one-time
`nori-analyse-ah` tempo/beat analysis inside its window (those tracks
had never been analysed); the release window shows no analyse thread at
all (analysis cached from the debug round) and no `MediaCodec_loop` in
the top threads — just `ExoPlayer:Playb` 590 ms + HwBinder 260 ms. So
part of the CPU delta is one-time analysis, part is the AOT playback
path. Like debug, release keeps its sleep crown while DJ-mixing
(159/200 vs the field's 3–117/200), and the queue again AutoMixed past
the album (post-run: PAUSED on a Second Wind track after auto-fill
carried through Beta Band → Gamma Ray Trio versions — transitions
occurred across album boundaries too).

**Paused 90 s: at the floor on both builds (0.01% vs 0.06%, 1.4 vs
1.3/s, 89/90 quiet both), PSS −36% (151 → 97 MB).**
No wakelocks, busiest thread a vestigial `ExoPlayer:Playb` 10 ms.
Caveat: the 300 s "paused waker hunt" in this file showed a ~10 Hz
main+DefaultExecutor timer appearing minutes after pausing (0/300 quiet
on a 300 s window) — this 90 s run settled only ~75 s post-pause and
caught the quiescent phase (89/90), same as the 30 s release run
(29/30). The 90 s number stands for short-paused; the long-paused timer
question from the waker hunt is unchanged by it.

### Cleanup

AutoMix settings verified identical to as-found after RUN 1 (ON,
defaults, gapless OFF — nothing needed restoring); crossfade left Off
(as set for the run); nori left PAUSED in background, no session
playing; keyboard restored (`kb on`, Gboard enabled + selected);
screen off.

## Appendix — full bench outputs (release final gaps)

<details><summary>nori release · AutoMix on (defaults, gapless-albums OFF) screen-off 200 s</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 200s   screen: off
cpu:      740 ms  = .37% of one core
wakeups:  189.2 per second
quiet:    159 of 200 seconds with (almost) no wakeups
memory:   103 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager'
busiest threads (ms):
  590 ExoPlayer:Playb
  260 HwBinder:10594_
  20 dev.nori.music
  10 MediaCodec_loop
  10 DefaultDispatch
  10 DefaultDispatch
session after: state=PLAYING
```

</details>

<details><summary>nori release · paused in background 90 s</summary>

```
package: dev.nori.music   session: state=PAUSED   window: 90s   screen: off
cpu:      10 ms  = .01% of one core
wakeups:  1.4 per second
quiet:    89 of 90 seconds with (almost) no wakeups
memory:   97 MB PSS
wakelocks:
busiest threads (ms):
  10 ExoPlayer:Playb
session after: state=PAUSED
```

</details>


## MP3 re-verify (2026-09-22)

Same device, same server, same track ("Noise 1", Bench, 10-min MP3 320),
media volume 0, screen OFF, `tools/bench.sh <pkg> 90 off`. nori is the
installed **release** APK (EQ off, AutoMix irrelevant for a single long
track). Rivals force-stopped while each window ran. No FLAC / EQ / mix /
paused windows in this pass — those stay as the 2026-09-21 numbers above.

### Headline — MP3 screen-off 90 s

| App | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session |
|---|---|---|---|---|---|
| nori release · MP3 | **1.26** | 372.1 | **75 / 90** | 119 | PLAYING |
| Symfonium · MP3 (run 1) | 10.20 | 1968.1 | 2 / 90 | 122 | PLAYING |
| Symfonium · MP3 (run 2, back-to-back) | 11.25 | 2132.5 | 0 / 90 | 125 | PLAYING |
| musly · MP3 | 4.01 | 568.5 | 0 / 90 | 142 | PLAYING |
| Navic · MP3 | 3.10 | 464.6 | 1 / 90 | 124 | PLAYING |

**Verdict.** Ranking unchanged: nori still the only app that sleeps (~83 %
quiet seconds) and the cheapest CPU by ~2.5× vs the next (Navic 3.10%).
Absolute numbers moved a little vs 2026-09-21 (nori 1.18→1.26, musly
2.78→4.01, Navic 2.78→3.10). Symfonium is the outlier: two consecutive
windows at 10.20% and 11.25% against yesterday's 6.66%, with the same
thread cast (ExoPlayer:Playb, AudioEngine, four BG workers) — hotter
steady state on this install, not a measurement glitch. PSS for nori is
up (108→119 MB); still competitive with Symfonium/Navic and below musly.

No hot-path fix shipped from this pass: screen-off decode threads are the
expected MediaCodec + ExoPlayer pair, quiet seconds hold, and there is no
new allocator or timer visible in the busy list.

<details><summary>nori release · MP3 320 screen-off 90 s (2026-09-22)</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      1140 ms  = 1.26% of one core
wakeups:  372.1 per second
quiet:    75 of 90 seconds with (almost) no wakeups
memory:   119 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  550 MediaCodec_loop
  320 ExoPlayer:Playb
  130 HwBinder:18059_
  100 ExoPlayer:Media
  70 ExoPlayer:Media
  10 Profile_Saver
  10 Jit_thread_pool
session after: state=PLAYING
```

</details>

<details><summary>Symfonium · MP3 screen-off 90 s run 1 (2026-09-22)</summary>

```
package: app.symfonik.music.player   session: state=PLAYING   window: 90s   screen: off
cpu:      9180 ms  = 10.20% of one core
wakeups:  1968.1 per second
quiet:    2 of 90 seconds with (almost) no wakeups
memory:   122 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  2520 ExoPlayer:Playb
  690 AudioEngine/1
  660 ExoPlayer:Simpl
  550 ik.music.player
  550 BG-1-T-3
  540 BG-1-T-2
  530 BG-1-T-4
  530 BG-1-T-1
session after: state=PLAYING
```

</details>

<details><summary>Symfonium · MP3 screen-off 90 s run 2 (2026-09-22)</summary>

```
package: app.symfonik.music.player   session: state=PLAYING   window: 90s   screen: off
cpu:      10130 ms  = 11.25% of one core
wakeups:  2132.5 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   125 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  2610 ExoPlayer:Playb
  750 AudioEngine/1
  710 ExoPlayer:Simpl
  580 ik.music.player
  580 BG-1-T-2
  570 BG-1-T-3
  570 BG-1-T-1
  550 BG-1-T-4
session after: state=PLAYING
```

</details>

<details><summary>musly · MP3 screen-off 90 s (2026-09-22)</summary>

```
package: com.devid.musly   session: state=PLAYING   window: 90s   screen: off
cpu:      3610 ms  = 4.01% of one core
wakeups:  568.5 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   142 MB PSS
wakelocks: 'AudioMix' 'com.ryanheise.audioservice.AudioService' 
busiest threads (ms):
  1660 ExoPlayer:Playb
  1180 MediaCodec_loop
  180 ExoPlayer:Media
  170 HwBinder:23734_
  160 ExoPlayer:Media
  120 com.devid.musly
  60 dart:io_EventHa
  30 Jit_thread_pool
session after: state=PLAYING
```

</details>

<details><summary>Navic · MP3 screen-off 90 s (2026-09-22)</summary>

```
package: paige.navic   session: state=PLAYING   window: 90s   screen: off
cpu:      2790 ms  = 3.10% of one core
wakeups:  464.6 per second
quiet:    1 of 90 seconds with (almost) no wakeups
memory:   124 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  1150 paige.navic
  750 MediaCodec_loop
  750 ExoPlayer:Playb
  170 Jit_thread_pool
  140 HwBinder:6034_1
  120 ExoPlayer:Media
  100 ExoPlayer:Media
  40 HeapTaskDaemon
session after: state=PLAYING
```

</details>


## MP3 re-verify (2026-09-22)

Same device, same server, same track ("Noise 1", Bench, 10-min MP3 320),
media volume 0, screen OFF, `tools/bench.sh <pkg> 90 off`. nori is the
installed **release** APK (EQ off, AutoMix irrelevant for a single long
track). Rivals force-stopped while each window ran. No FLAC / EQ / mix /
paused windows in this pass — those stay as the 2026-09-21 numbers above.

### Headline — MP3 screen-off 90 s

| App | CPU (% of one core) | Wakeups/s | Quiet s | PSS MB | Session |
|---|---|---|---|---|---|
| nori release · MP3 | **1.26** | 372.1 | **75 / 90** | 119 | PLAYING |
| Symfonium · MP3 (run 1) | 10.20 | 1968.1 | 2 / 90 | 122 | PLAYING |
| Symfonium · MP3 (run 2, back-to-back) | 11.25 | 2132.5 | 0 / 90 | 125 | PLAYING |
| musly · MP3 | 4.01 | 568.5 | 0 / 90 | 142 | PLAYING |
| Navic · MP3 | 3.10 | 464.6 | 1 / 90 | 124 | PLAYING |

**Verdict.** Ranking unchanged: nori still the only app that sleeps (~83 %
quiet seconds) and the cheapest CPU by ~2.5× vs the next (Navic 3.10%).
Absolute numbers moved a little vs 2026-09-21 (nori 1.18→1.26, musly
2.78→4.01, Navic 2.78→3.10). Symfonium is the outlier: two consecutive
windows at 10.20% and 11.25% against yesterday's 6.66%, with the same
thread cast (ExoPlayer:Playb, AudioEngine, four BG workers) — hotter
steady state on this install, not a measurement glitch. PSS for nori is
up (108→119 MB); still competitive with Symfonium/Navic and below musly.

No hot-path fix shipped from this pass: screen-off decode threads are the
expected MediaCodec + ExoPlayer pair, quiet seconds hold, and there is no
new allocator or timer visible in the busy list.

<details><summary>nori release · MP3 320 screen-off 90 s (2026-09-22)</summary>

```
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      1140 ms  = 1.26% of one core
wakeups:  372.1 per second
quiet:    75 of 90 seconds with (almost) no wakeups
memory:   119 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  550 MediaCodec_loop
  320 ExoPlayer:Playb
  130 HwBinder:18059_
  100 ExoPlayer:Media
  70 ExoPlayer:Media
  10 Profile_Saver
  10 Jit_thread_pool
session after: state=PLAYING
```

</details>

<details><summary>Symfonium · MP3 screen-off 90 s run 1 (2026-09-22)</summary>

```
package: app.symfonik.music.player   session: state=PLAYING   window: 90s   screen: off
cpu:      9180 ms  = 10.20% of one core
wakeups:  1968.1 per second
quiet:    2 of 90 seconds with (almost) no wakeups
memory:   122 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  2520 ExoPlayer:Playb
  690 AudioEngine/1
  660 ExoPlayer:Simpl
  550 ik.music.player
  550 BG-1-T-3
  540 BG-1-T-2
  530 BG-1-T-4
  530 BG-1-T-1
session after: state=PLAYING
```

</details>

<details><summary>Symfonium · MP3 screen-off 90 s run 2 (2026-09-22)</summary>

```
package: app.symfonik.music.player   session: state=PLAYING   window: 90s   screen: off
cpu:      10130 ms  = 11.25% of one core
wakeups:  2132.5 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   125 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  2610 ExoPlayer:Playb
  750 AudioEngine/1
  710 ExoPlayer:Simpl
  580 ik.music.player
  580 BG-1-T-2
  570 BG-1-T-3
  570 BG-1-T-1
  550 BG-1-T-4
session after: state=PLAYING
```

</details>

<details><summary>musly · MP3 screen-off 90 s (2026-09-22)</summary>

```
package: com.devid.musly   session: state=PLAYING   window: 90s   screen: off
cpu:      3610 ms  = 4.01% of one core
wakeups:  568.5 per second
quiet:    0 of 90 seconds with (almost) no wakeups
memory:   142 MB PSS
wakelocks: 'AudioMix' 'com.ryanheise.audioservice.AudioService' 
busiest threads (ms):
  1660 ExoPlayer:Playb
  1180 MediaCodec_loop
  180 ExoPlayer:Media
  170 HwBinder:23734_
  160 ExoPlayer:Media
  120 com.devid.musly
  60 dart:io_EventHa
  30 Jit_thread_pool
session after: state=PLAYING
```

</details>

<details><summary>Navic · MP3 screen-off 90 s (2026-09-22)</summary>

```
package: paige.navic   session: state=PLAYING   window: 90s   screen: off
cpu:      2790 ms  = 3.10% of one core
wakeups:  464.6 per second
quiet:    1 of 90 seconds with (almost) no wakeups
memory:   124 MB PSS
wakelocks: 'AudioMix' 'ExoPlayer:WakeLockManager' 
busiest threads (ms):
  1150 paige.navic
  750 MediaCodec_loop
  750 ExoPlayer:Playb
  170 Jit_thread_pool
  140 HwBinder:6034_1
  120 ExoPlayer:Media
  100 ExoPlayer:Media
  40 HeapTaskDaemon
session after: state=PLAYING
```

</details>


## FLAC + EQ + cold re-verify (2026-09-22, nori 0.3.0 release)

Same device/server/volume-0/screen-off protocol as the morning MP3 pass.
nori release rebuilt after the 0.3.0 bump; EQ off + offload on for stock
FLAC; nori EQ row uses 8 kHz **+8.4 dB** (equalizer confirmed in chain).
Symfonium EQ row is Noise 1 as-found (Graphic EQ left on). musly has no EQ.
Navic EQ row is Noise 1 as-found (Built-in effects). Mix/paused not re-run.

### FLAC — Noise flac, 90 s

| App | CPU | Wakeups/s | Quiet | PSS MB |
|---|---|---|---|---|
| nori release | **1.30%** | 469.1 | **69/90** | 126 |
| Symfonium | 8.74% | 1937.7 | 0/90 | 118 |
| musly | 4.70% | 610.8 | 0/90 | 148 |
| Navic | 3.72% | 589.1 | 1/90 | 118 |

### EQ on — Noise 1 MP3, 90 s

| App | CPU | Wakeups/s | Quiet | PSS MB |
|---|---|---|---|---|
| nori release (8k +8.4) | **1.35%** | 395.0 | **73/90** | 104 |
| Symfonium (as-found) | 13.37% | 2136.0 | 1/90 | 119 |
| Navic (as-found) | 2.92% | 458.1 | 1/90 | 119 |

### Cold start — `am start -W` TotalTime ms, ×3

| App | Run 1 | Run 2 | Run 3 | ~median |
|---|---|---|---|---|
| nori release | 576 | 632 | 567 | **~590** |
| Symfonium | 946 | 849 | 909 | ~900 |
| musly | 1226 | 1083 | 832 | ~1050 |
| Navic | 799 | 659 | 635 | ~700 |

<details><summary>Raw dumps</summary>

```
# nori FLAC
package: dev.nori.music   session: state=PLAYING   window: 90s   screen: off
cpu:      1170 ms  = 1.30% of one core
wakeups:  469.1 per second
quiet:    69 of 90 seconds with (almost) no wakeups
memory:   126 MB PSS

# nori EQ
cpu:      1220 ms  = 1.35% of one core
wakeups:  395.0 per second
quiet:    73 of 90 seconds with (almost) no wakeups
memory:   104 MB PSS

# sym FLAC
cpu:      7870 ms  = 8.74% of one core
quiet:    0 of 90

# sym EQ as-found
cpu:      12040 ms  = 13.37% of one core
quiet:    1 of 90

# musly FLAC
cpu:      4230 ms  = 4.70% of one core
quiet:    0 of 90

# navic FLAC
cpu:      3350 ms  = 3.72% of one core
quiet:    1 of 90

# navic EQ as-found
cpu:      2630 ms  = 2.92% of one core
quiet:    1 of 90
```

</details>
