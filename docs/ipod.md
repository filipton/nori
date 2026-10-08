# nori on the iPod touch 6: implementation plan

A client for a jailbroken iPod touch (6th generation), built from the ground up on the nori engine.
Three things come first, in this order when they collide: the sound, the feel of the interface, and what
it costs the 1080 mAh battery.

Written 2026-10-03 from a device (SSH over USB) and the repo at `a0b72b4`.

## 1. The device, measured

Read over SSH (`iproxy <port> 44` to checkra1n's dropbear, `root` with checkra1n's default password).

| | |
|---|---|
| Model | `iPod7,1` (`N102AP`), iPod touch 6th generation, 32 GB (30 GiB; 13 GiB free) |
| OS | iOS 12.5.8 (`16H88`), the last for this device. Jailbroken with checkra1n (semi-tethered, `libhooker` 1.6.9, `libsubstitute`) |
| SoC | Apple A8 (`T7000`): 2 × Typhoon cores at 1.1 GHz, ARMv8-A with NEON (`neon_hpfp`, no `fp16`), 64 KB L1D, 1 MB L2; PowerVR GX6450 GPU (Metal, GPU family 2) |
| Memory | 1 GB LPDDR3 (`hw.memsize` 1 044 381 696), 16 KB pages. Idle after a reboot the system already holds most of it (2763 free pages) |
| Audio | Cirrus Logic CS42L81 codec (`audio-control,cs42l81`): 3.5 mm jack, one bottom speaker (mono), Lightning digital out, Bluetooth 4.1 (A2DP SBC/AAC) |
| Screen | 4.0" IPS, 1136 × 640 at 326 ppi, @2x: **320 × 568 pt**, 20 pt status bar, no safe-area insets beyond it, home button |
| Not there | Taptic engine (no haptics), 3D Touch, Touch ID, cellular, GPS, NFC |
| Battery | 1080 mAh design, `CycleCount` 0 (a new cell) |
| Jailbreak tooling | `AppSync Unified` 116.2 (unsigned/fake-signed IPAs install), `ldid` at `/binpack/usr/bin/ldid`, `dpkg`/`apt`, `uicache`, Zebra, Filza, Activator, PreferenceLoader. No `sftp-server` (copy files with `ssh ... 'cat > file'` or `tar` over ssh) |
| Swift runtime | In the OS from iOS 12.2: an app whose minimum is 12.2 ships no Swift dylibs (one built for 12.1 carries ~140 MB of them) |

What iOS 12 gives and withholds, for the interface (section 6): UIKit with large titles, `UIViewPropertyAnimator`
with springs, `UIFontMetrics`, drag and drop, `AVRoutePickerView`; **no** SwiftUI, SF Symbols, diffable data
sources, compositional layouts, context menus, system dark mode, `UISheetPresentationController`, haptics.

What iOS gives and withholds, for the sound (section 5): a float mixer at the device rate through
AVAudioSession, RemoteIO with an I/O buffer of at most ~93 ms, no deep hardware buffer like Android's
AudioTrack, no app-level offload API, no exclusive/bit-perfect mode on the jack. Background audio is a
declared mode; a playing session keeps the process alive, a paused one gets suspended.

## 2. What the engine is, in one page

`nori-engine` is the whole player for a platform that has none: one thread that sleeps unless something is
due. It loads each song in bursts (`load_control`: 60 s to 10 min ahead, byte cap a quarter of the memory
budget, 16 to 48 MB), demuxes with symphonia's readers, decodes with `nori-player::decode` (MP3, FLAC, AAC-LC,
Vorbis, ALAC, Opus; gapless with the encoder delay and padding cut), runs the sound chain (EQ, pre-amp,
limiter, crossfeed, balance, ReplayGain, speed and pitch, silence skip), holds and mixes endings (crossfade,
AutoMix), walks the core's queue, and writes float frames into a lock-free ring (`crates/engine/src/output.rs`)
sized `BUFFER_US` 10 s + 2 s slack (4.2 MB at 44.1 kHz stereo float). The device's thread calls `Feed::pull`
for every buffer it plays - no lock, no allocation; when the ring falls under the low mark the pull unparks
the engine for the next burst. A sound change made while music plays is heard seamlessly: the sink keeps the
chain's state every 8192 frames, goes back to the first frame the device cannot have taken, runs the chain
again with the new settings, blended over 5 ms.

With the `core` feature the engine plays the core's queue, planner, settings, stream addresses and
ReplayGain (`nori_engine::core`: `CoreApp`, `CoreLibrary`, `CoreQueue`, `Store`, `Downloader`, `Measurer`).
`nori-host` wraps all of that into one `Session` per server profile: core, client, engine, store, downloader,
cover loader, search, queue saves (`Keeper`), the song-arrived rules, the offline bridge, scrobbling, lyrics.
The terminal and desktop clients are interfaces over `Session` and nothing more; **the iPod client should be
the same**.

What a client has to write itself (docs/clients.md, in short): the `AudioOutput`, the HTTP `Transport` and
`ByteSource` (or link `nori-http`), the OS glue (now playing, remote commands, interruptions, routes,
background), the pictures (a `Paint` for the platform), every screen and every word.

Every dependency under `nori-host` builds for `aarch64-apple-ios`: bundled SQLite and Signalsmith through
`cc`, `ring` (has iOS arm64 assembly), rustls with `webpki-roots`, cpal/coreaudio-rs (has an iOS backend,
which this plan does not use), `objc2`. Nothing of tokio, OpenSSL or D-Bus beyond `nori-mpris`, which is a
no-op off Linux already.

## 3. Architecture

```
┌──────────────────────── nori.app (Swift, UIKit, iOS 12.2+) ─────────────────────────┐
│ screens, gestures, words (Localizable.strings), MPNowPlayingInfoCenter,            │
│ MPRemoteCommandCenter, AVAudioSession policy, UIApplication lifecycle              │
├───────────── uniffi Swift bindings (user actions, one page per call) ──────────────┤
├───────────── C doors (@_cdecl-free: plain `extern "C"`, per frame / row / buffer) ─┤
│ crates/ios  (staticlib `libnori_ios.a`)                                             │
│   session glue over nori-host::Session · IosOutput (AudioOutput) · CgPaint (covers)  │
│   doors: seek bar step, lyric clock, cover colours blend, playhead read             │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ ios/Sound/NoriAudio.m  (C ABI): AVAudioSession, AURemoteIO, route + interruption    │
│ notifications; calls back into Rust for every I/O cycle (`nori_ios_pull`)           │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ nori-host → nori-engine(core) → nori-core → every domain crate; nori-http; nori-covers; nori-look │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

Decisions, with the reasons:

- **Swift for the interface, not Objective-C, with the deployment target at 12.2.** From iOS 12.2 the Swift
  runtime is in the dyld shared cache, so nothing of it is embedded (the fork's 140 MB go away). SwiftUI is
  out (iOS 13); this is a UIKit app. Swift gets uniffi's Swift generator for the coarse API, which is the
  single biggest saving of hand labour (the settings model alone has 125 settings with specs and states).
  Objective-C remains only in the audio shim, where the APIs are C and ObjC anyway. If uniffi's pinned
  revision (`aea52cb`, needed for the Kotlin JNI generator) turns out not to build Swift bindings cleanly,
  the fallback is a hand-written C ABI (cbindgen) for everything, as Android's doors are - more typing, same
  shape.
- **One Rust static library, `crates/ios` (`nori-ios`)**, built like `crates/android`: it links `nori-host`
  (so the whole session logic is shared with the terminal and desktop clients) and holds the iOS-only Rust:
  the `AudioOutput`, the cover `Paint` into CGImage-ready buffers, the doors. No uniffi `ffi` features on the
  domain crates beyond what the Swift bindings need. `neural-beats` **off** (15 MB and a transformer on two
  A8 cores with 1 GB: not for this device).
- **`nori-host` grows one seam**: `Open` takes the `Box<dyn AudioOutput>` (and its `Volume` as an
  `OutputVolume`) instead of building `CpalOutput` itself, with cpal behind a cargo feature the cli and
  desktop keep on. That is the only change the core needs for a third platform, and it removes a platform
  decision from a shared crate.
- **HTTP in Rust (`nori-http`, ureq + rustls), not NSURLSession**, for v1: one pool for API, covers and
  audio, the engine's cancellable `ByteSource` already written, identical behaviour to the desktop. Roots
  are `webpki-roots`; a self-signed Navidrome goes through the core's own certificate settings (already a
  feature), not the iOS trust store. NSURLSession comes back only if background downloads are wanted
  (section 7).
- **Data under the app's container**: `Library/Application Support/nori/` for `nori.db`, `music/` (stream
  cache + downloads) and `covers/`, with `NSURLIsExcludedFromBackupKey` set (there is no iCloud here, but
  iTunes/Finder backups would copy gigabytes otherwise). Same layout as the desktop's data dir, so
  `nori_host::db_path` applies unchanged.

## 4. Toolchain and the build pipeline

Everything builds in one Docker image (`tools/ios-build/Dockerfile`), on Linux or a Mac alike, with no Xcode:
swift.org's Linux Swift and clang, lld's Mach-O linker, Rust with the `aarch64-apple-ios` target, and
Procursus' ldid. What Apple's toolchain would otherwise bring is fetched once into `~/.cache/nori-ios` with
pinned checksums: theos' copy of the iPhoneOS 16.4 SDK (headers, `.tbd` stubs and Swift interfaces, 221
MB), and from swift.org's macOS toolchain the `libswiftCompatibility*.a` that a deployment target below iOS
13 links, Darwin's `Dispatch`/`os` API notes and clang's `libclang_rt.ios.a`. Linux's Swift resource
directory carries its own `Dispatch` module, which clashes with the SDK's, so the build uses a resource
directory of only `shims`, `clang` and those iOS parts. A first build takes minutes (the image, 1.5 GB of
toolchain, Swift's module cache); an edit rebuilds in about 15 s.

The linker matters: Xcode 26's produces binaries that crash on iOS 12.5.x (reported on the developer
forums), which is why the app first linked with Xcode 15's classic ld64. lld writes the same load
commands as that one (dyld info, no chained fixups). No Xcode attaches a debugger to an iOS 12 device
(device support starts at iOS 15 in 15.2); debugging is logs over SSH, which the jailbreak makes easy.

Rust: `aarch64-apple-ios` supports iOS 10+, set `IPHONEOS_DEPLOYMENT_TARGET=12.2`. The static library needs
no linker at all; `cc`-built C/C++ (SQLite, Signalsmith) needs the SDK headers.

The pipeline is `tools/ipod.sh` (like `tools/apk.sh`): `build` runs `tools/ios-build/build.sh` in the image,
then `install` and `run` reach the iPod; plain `tools/ipod.sh` builds and, with an iPod on USB, installs
and runs.

1. **Rust**: `SDKROOT=<iPhoneOS SDK> IPHONEOS_DEPLOYMENT_TARGET=12.2 cargo build --release --target
   aarch64-apple-ios -p nori-ios`, every object `platform IOS minos 12.2`. Release profile as the workspace
   has it (fat LTO, one codegen unit, panic = unwind so a core panic surfaces as an error, not a crash).
2. **The app**: **there is no Xcode project**: one `swiftc` call (`-target arm64-apple-ios12.2 -O -wmo`, the
   bridging header `ios/Sources/nori_ios.h`, `-lnori_ios -lc++`, the frameworks, lld, `-dead_strip`) is the
   whole build, so it runs from a shell and nothing in a `.pbxproj` drifts. `nori.app` lands in
   `build/ios/`. The launch screen is a plain 640 × 1136
   image (`UILaunchImages`, still honoured by iOS 12), because `ibtool` can crash on a CoreSimulator
   mismatch and a storyboard would be the only thing needing it. The home-screen icon is the same mark as
   Android's, on the rice-white plate, as square PNGs named in `CFBundleIcons` (`AppIcon60x60@2x.png` is
   the one the iPod touch 6 shows); SpringBoard rounds them. `uicache --path --respring` reloads the icon.
3. **Signing and the .ipa**: `ldid -S entitlements.plist` fake-signs it, and `Payload/nori.app` zipped is
   `build/ios/nori-ipod-<version>.ipa`. An app in `/Applications` is a system app to launchd: without
   `platform-application` and `com.apple.private.security.no-container` it is simply never spawned (no
   crash report, `uiopen` returns 0 regardless), so `ios/entitlements.plist` carries them, as Zebra and
   Filza do, plus `skip-library-validation` and `get-task-allow`. The background audio mode is Info.plist's.
4. **`install`**: into `/Applications/nori.app` and `uicache --path`. It runs as `mobile`, unsandboxed, with
   its data under `/var/mobile/Library/Application Support/nori`; that is fine for this device and needs
   no lockdown pairing. The sandboxed install (`ideviceinstaller -i`, entitlements back to the identifier
   alone) needs the trust dialog tapped once and is the alternative, not a requirement.
5. **`run`**: `uiopen -b dev.nori.music`, then the process line and the data dir (the device has no awk or
   pgrep; `ps`, `grep`, `cut` and `sed` only).
6. **Logs**: the core's `alog` file (`nori.log` in the data dir, kept from launch) over SSH, `os_log` from Swift, crash reports in
   `/var/mobile/Library/Logs/CrashReporter/`, and symbolication with `atos` against the unstripped
   executable the app step built.

Traps met and answered: `cc`, `ring` and `libsqlite3-sys` take `SDKROOT` and the deployment target from
the env; `sccache` is the workspace's rustc wrapper, so the image carries it;
`iproxy` must be detached (`nohup … & disown`) or it dies with the shell that started it; macOS's `tar`
adds AppleDouble `._` files unless `COPYFILE_DISABLE=1`; checkra1n's dropbear accepts a public key and then
stalls for good, so the script authenticates with the password through `sshpass` (`NORI_IPOD_PASSWORD`,
checkra1n's default unless set).

## 5. The sound

This is where the plan earns its keep. The goals: the engine's samples reach the codec with nothing in the
way, a sound change is heard at once on the equalizer screen, and the process wakes as little as iOS lets it.

### 5.1 The output: `IosOutput` over AURemoteIO, shaped like Android's `track.rs`

iOS has no equivalent of a deep AudioTrack. Every path (AudioQueue, AVAudioEngine, RemoteIO) ends in the
same in-process I/O thread (`com.apple.audio.IOThread.client`) that mediaserverd wakes once per I/O buffer;
AudioQueue only adds a client-side buffer list on top of it. So the honest design is RemoteIO directly,
with the I/O buffer as long as the session allows:

- `AVAudioSession`: category `playback`, mode `default`, `setPreferredIOBufferDuration(0.093)` (4096 frames
  at 44.1 kHz, the ceiling in practice: expect `ioBufferDuration` ≈ 0.0929). That is ~11 wakes a second on
  the I/O thread, each a 32 KB copy from the ring (`Feed::pull` into the unit's float buffers). The desktop
  client runs at exactly this cadence (`PERIOD_MS` 100 in nori-output-cpal) at 0.03 % CPU.
- `setPreferredSampleRate` to the song's rate family (44.1 or 48 kHz, `OutputFormat.rate` as the engine asks
  through `open`). The CS42L81 path takes both. The unit's input is always the asked rate and Rust is
  granted exactly it; RemoteIO converts if the hardware is elsewhere. The hardware moves to the preferred
  rate *after* activation, and a unit built before the move played 44.1 kHz songs 9 % fast or went silent
  until the engine's stall check reopened it, so the shim builds the unit again (same input rate) whenever
  its output side changes. With the rate matched, the system mixer's SRC is out of the path - the closest
  thing to bit-perfect this hardware offers.
- `takes_float` = true, always: RemoteIO's canonical format is Float32, the mixer is float, the codec is fed
  24-bit. So "high quality output" on means float from the decoder to the DAC with no dither of ours; off
  means the engine's 16-bit chain with its own dither and the mixer converting (cheaper on memory, half the
  ring). Default on for this client: the A8 does not notice, and it is the better sound.
- `latency_us` = session `outputLatency` + `ioBufferDuration` + frames pulled but not yet rendered (from the
  render callback's `AudioTimeStamp` against the host clock, as cpal's `Heard` does). `mixed_us` = the
  session's `outputLatency` on a Bluetooth route (hundreds of ms of A2DP: a sound change comes no sooner for
  dropping it). `holding` while `latency_us` > 0.
- `ramp` returns false: the ring runs the fades per sample (`Feed::pull` already does), since the device
  holds under 100 ms. `flush` is a no-op for the same reason. `bursts` false: the ring's low mark wakes the
  engine from the pull itself (`WAKE_LOW_US`), no timer either way.
- `shallow(on)` (equalizer screen in sight): `setPreferredIOBufferDuration(0.010)` → ~100 wakes/s while the
  screen is open, a band moved heard in ~30 ms. The `rules::equalizer_tuning` in the core already says when;
  the client only reports in-sight/touched. Back to 93 ms when the screen goes.
- `watch`: `AVAudioSessionRouteChangeNotification` → `Device { kind, name }` from the current route's output
  port type (`BuiltInSpeaker` → Speaker, `Headphones` → Wired, `BluetoothA2DP` → Bluetooth with the port
  name; anything else → Other). This feeds `nori-devices`: a sound profile per output and the AutoEQ
  offer for a named headphone, exactly as on Android. `OutputFacts` stays at its default: no USB, never
  bit-perfect (the jack is the output, section 11).
- `failed`: the render callback stopped and the unit would not restart after a media server reset
  (`AVAudioSessionMediaServicesWereResetNotification`): rebuild the unit once; if that fails, tell the engine.
- `pause`/`resume`: `AudioOutputUnitStop/Start` and `setActive(false)` on pause after the engine's idle
  release (5 min, `IDLE_RELEASE_MS`), so the session, and with it the process, can sleep.

The Rust side is a `Sink` trait like `crates/android/src/track.rs` has: the ObjC shim implements it through
a C ABI (`nori_audio_open(rate, channels, io_ms)`, `nori_audio_start/stop`, `nori_audio_latency_us`,
`nori_audio_set_io_ms`, a route callback), and the Rust `IosOutput` holds the state machine. The tests run
on a simulated sink and the virtual clock, as track.rs's do; the device check is the ObjC shim alone.

### 5.2 The volume

iOS keeps the volume to itself: the hardware buttons and `MPVolumeView` set the system volume; an app
cannot read a slider's value into its own chain without `MPVolumeView`. So the player's volume slider is an
`MPVolumeView` (styled, iOS 12 still lets its thumb and track be tinted), the chain runs at full scale,
and `OutputVolume` (the loudness compensation the chain reads) is fed from
`AVAudioSession.outputVolume` KVO, which also moves the slider. Nothing of nori's own multiplies samples
for volume - one fewer pass, and the system's ramp on the buttons is the one iPod users already know.

### 5.3 Offload: measured later, not promised

Playing compressed audio "outside the process" is what Apple's Music app does for files (`AVAudioPlayer`
hands the file to mediaserverd and the app's I/O thread stays quiet). The engine's `OffloadOutput` trait
expects packets fed gaplessly on one track; iOS offers no packet API to mediaserverd, only whole files or
`AVPlayerItem`s. An experiment for later: an `OffloadOutput` over `AVQueuePlayer` playing the stream
cache's whole files for MP3/AAC songs when nothing touches the samples (the engine already decides when,
`nori_player::policy`), with gapless by `AVPlayerItem` pre-roll. Expected win: the ~11 wakes/s go to
mediaserverd, our process sleeps between songs. Expected losses: gapless is at the mercy of AVFoundation's
priming, ReplayGain becomes `AVPlayer.volume`, and the fades leave the ring. Measure on the device with
the battery gauge before deciding (section 8); v1 ships without it.

### 5.4 Interruptions, routes, remote controls

- `AVAudioSessionInterruptionNotification`: began → `engine.pause_now()`; ended with `shouldResume` →
  `engine.play()`. Siri, alarms, a FaceTime call on the iPod.
- Route change with `OldDeviceUnavailable` (headphones pulled, Bluetooth gone) → pause, the core's rule
  (`rules.rs`) says the fade; a new route → `DeviceWatch` (above) → the device's sound profile.
- `MPRemoteCommandCenter`: play, pause, toggle, next, previous, `changePlaybackPosition`, seek
  forward/backward, like/dislike mapped to star. These are the EarPods clicker, the lock screen, Control
  Center, and Bluetooth AVRCP; they also wake a suspended app that was the last now-playing app.
- `MPNowPlayingInfoCenter`: title, artist, album, duration, elapsed, rate, artwork (from the cover loader at
  600 px, once per song), updated on song change, seek, play/pause and mix takeover (`Event::Song` fires
  when the next song is the louder in a mix - the lock screen flips then, as the Android notification
  does). Never on a timer: iOS extrapolates elapsed from rate.
- Remote control (Settings → Remote control, the `remoteControl` setting; off, nothing runs): the
  session's `Remote` serves the LAN door and the relay as on the desktop, and Bonjour (`NetService`,
  `ios/Sources/Devices.swift`) announces the door and, while "Play on" is open, finds the account's other
  devices. The place published is the one heard: the render callback passes how far ahead its buffer
  leaves the unit (the timestamp's host time less now), and `latency_us` adds the route's
  `outputLatency`. The remote clock is CLOCK_MONOTONIC_RAW (`mach_continuous_time`), which runs on while
  the iPod sleeps. A volume set from another device moves the system volume through an `MPVolumeView`
  slider; the system's own keys reach the other devices through `nori_ios_volume`. While another device
  plays, the player, the queue sheet and the lock screen are its music and control it. The iPod is not a
  jam host or guest. Paused in the background, iOS suspends the app and its door with it: another device
  reaches it again once it plays or is opened.

### 5.5 What the engine already gives this device

Gapless, crossfade, AutoMix (the analysis FFT and Signalsmith stretch are fine on A8 for one song at a time;
"Better beat detection" is off, see section 3), the 10-band EQ with its limiter, crossfeed (headphones are
this device's life), ReplayGain with the boost cap, speed and pitch, silence skip, sleep timer, the offline
bridge, songs fetched ahead over Wi-Fi in one burst then the radio left alone. All tested in Rust on the
virtual clock; nothing of it is re-implemented here.

## 6. Interface and feel

The reference is Apple's own Music app of iOS 12 on an iPhone SE (same 320 × 568 pt screen): it is the
proof that the Apple Music look fits four inches. The Android app's screens map one to one; the words come
from `strings.xml` into `Localizable.strings` (about 1050 strings today, mechanically carried over, and
numbers written by a Swift `Fmt` mirroring `core/text/Fmt.kt` with the same vectors).

### 6.1 Structure

- `UITabBarController` with four tabs: **Home** (shelves: recently played, recently added, mixes, starred,
  pinned playlists - the core's home rows), **Library** (the list Apple Music has: Playlists, Artists,
  Albums, Songs, Genres, Downloaded, Smart lists; recently added grid underneath), **Search** (the index at
  every key, the server once typing pauses, `SearchSession`), **Settings** (a tab, not a gear: the app has
  125 settings and a 4" screen has no room for a gear in a large-title bar). Each tab a
  `UINavigationController`; the back swipe is the system's.
- **The mini player** sits over the tab bar (cover, title, play/pause, next; a progress hairline). Tapping
  or dragging it up brings **the player card**: a custom `UIPresentationController` and an interactive
  transition driven by one pan gesture, settled by a `UISpringTimingParameters` animator the finger scrubs.
  No LNPopupController: the feel is the client's own and the dependency is 5000 lines for one gesture.
- **Pages**: album, artist, playlist, genre, smart list, downloads, queue, lyrics, equalizer, per-device
  sound, about/credits. Each takes its data from the core's page layouts (`nori-library::pages`, `rows`,
  `menus`) through one call and draws it.

### 6.2 The player

Top to bottom on 568 pt: status bar 20, grabber 14, cover **272 × 272** (24 pt margins; scales to 0.82
when paused, as Apple's does, a spring the user's tap drives), title/artist/album with star and menu (56),
seek bar with times (36), transport row (72), `MPVolumeView` (36), bottom row: lyrics, route picker
(`AVRoutePickerView`), queue (44). The background is black whatever the cover (the owner's decision,
2026-10-04): the cover's colours tint nothing, so pale covers never wash out the light text and nothing
is derived or composed per song.

### 6.3 Lyrics

Full black card. Lines in SF Pro Semibold 24 pt, the lit line at full colour, the others at
`UNSUNG`; word-by-word fill drawn with CoreText in a custom view's `draw(_:)` from the record's word times.
Redraw is paced by the core's `LyricClock` (`Step::wait`, `still`): a one-shot `DispatchSourceTimer` set
to the clock's next moment, and a `CADisplayLink` only while a word fill is moving on screen. Off screen or
paused, nothing runs. A line tapped seeks.

### 6.4 Lists and covers

`UITableView` and `UICollectionViewFlowLayout` with cell reuse and `prefetchDataSource` (the next
screenful). Covers come from `nori-covers` at the view's pixel size: the iOS `Paint` decodes straight into
a buffer that becomes a `CGImage` (`CGDataProvider` over the Rust allocation, released by a Rust callback,
no copy), and an `UIImage` memory cache keyed like Android's `CoverLoader` (per address, largest kept,
trimmed on `didReceiveMemoryWarning`). The rule from clients.md holds: nothing touches the disk or network
on the main thread, the request posts its answer to main.

### 6.5 Type, icons, theme

System font (San Francisco: the exact Apple Music typeface), Dynamic Type through `UIFontMetrics`. No SF
Symbols on iOS 12: the glyphs are Android's own Material icons (`Icons.Filled.*`), their path data
carried into `ios/Sources/Glyphs.swift` and drawn as templates at the size each place needs; the
Licenses page credits them. Themes: iOS 12 has no system dark mode, so the app's own `theme` setting (dark by
default, light as the option; the AMOLED-black one is pointless on an LCD and is hidden).

### 6.6 Motion

The rules from `docs/motion.md` carry over: nothing animates unless the user touched it; `UIAccessibility
.isReduceMotionEnabled` turns each into a snap or a short fade. Push/pop is UIKit's. The card, the paused
cover, the mini player's progress and the lyrics fill are the animations the app has. All through
`CALayer` properties (transform, opacity, position) so Core Animation runs them on the render server, not
on our CPU per frame.

## 7. Memory, storage, the network

- **1 GB with iOS 12 on it**: a background audio app that grows past ~100 MB is the first jetsam victim.
  Budget: < 60 MB resident while playing in the background, < 120 MB in the foreground with covers.
  `Config.memory_mb` = 96 → `load_control` caps a song's in-memory window at 24 MB; the ring is 4.2 MB
  (float) or shrinks to the 16-bit chain when high quality output is off; the cover memory cache 16 MB;
  nothing of the library is held in Swift beyond the page on screen (the core's pages are read, drawn,
  dropped).
- **Storage**: 13 GB free. Stream cache (`cache_mb`) default 2 GB, downloads unlimited, both the core's
  eviction orders; `NSURLIsExcludedFromBackupKey` on `music/` and `covers/`.
- **Network**: Wi-Fi only (no cellular), so the "unmetered" network is the only one and the metered settings
  are hidden. The engine's fetching ahead (whole songs in one burst, the radio left alone) is the battery
  rule that matters most on Wi-Fi.
- **Suspension**: when paused and the session goes inactive, iOS suspends the process within seconds. Before
  that (`applicationDidEnterBackground`), save the queue (`queue_keep(QueueMoment::Closing)`) and finish
  what the `Keeper` holds; downloads and AutoMix measuring stop until the app is open or playing again -
  stated in the Downloads page, not hidden. Background `NSURLSession` downloads are a phase-2 item (they
  would need a second `ByteSource` in Swift and the core's progress fed from the session's delegate).

## 8. Measuring on the device

The jailbreak gives a better lab than a stock iPhone: everything the perf build reads on Android is
readable over SSH here.

- **Battery**: `ioreg -rc AppleARMPMUCharger` → `InstantAmperage`, `Voltage`, `AppleRawCurrentCapacity`
  every 30 s, screen off, Wi-Fi on, a fixed playlist: mA over 30 minutes, like `tools/bench.sh`. Compare
  against Apple's Music app on the same songs.
- **Wakeups and CPU**: `ps -o pid,%cpu,rss,wq,threads -p <pid>` and `top -l` over SSH; per-thread CPU and
  context switches through the app's own perf doors (the `nori-perf` recorder, as Android's perf build:
  `task_threads` + `thread_info` from inside the process, written to the log).
- **The sound**: the engine's `WavOutput` renders any queue on the host for a bit-exact listen; on the
  device, `Status.underruns` on the player's debug line and the I/O thread's render deadline misses.
- **Frames**: `CADisplayLink` timestamps in a debug overlay while scrolling the album grid; the GX6450 shows
  overdraw quickly, so each cell is one opaque layer.

Targets (to be confirmed with the first numbers): playing FLAC over Wi-Fi, screen off: the app's own CPU
under 1.5 % of one core, ~11 wakes/s (the I/O thread) + 1 every ~8 s (the engine), Wi-Fi active only in
bursts; paused 5 min: 0 wakes, suspended.

## 9. Milestones

Each ends with something that runs on the iPod; nothing moves on until the one before is measured.

0. **Hello, iPod** (a day): empty Swift UIKit app, deployment 12.2, built with Xcode 15, fake-signed, installed
   over AppSync, launches. Proves the pipeline and the linker flags; `tools/ipod.sh` written here.
1. **A Rust library on the device** (a day): `nori-ios` staticlib with one uniffi call (`core_credits`)
   shown in a label; `SDKROOT` build of the library, the link with Xcode. Sizes noted (expect ~12 MB for the
   library).
2. **Sound** (a week): the audio shim, `IosOutput` with its Rust tests, `nori-host::Open` taking the output;
   a hard-coded profile plays a queue from a real server through the engine: play, pause, seek,
   gapless, crossfade, the EQ screen's shallow mode heard. Headphones pulled pauses; the lock screen shows
   and controls it; a Bluetooth speaker gets its own profile. First battery numbers.
3. **The app** (three to four weeks): login and profiles, Home, Library, Search, album/artist/playlist pages,
   queue, downloads, the player card and mini player, settings (the curated pages, iOS words), lyrics,
   equalizer, per-device sound. Covers through `nori-covers` with the CGImage paint. Words carried over
   from `strings.xml`.
4. **Polish and cost** (two weeks): every animation looked at as `docs/motion.md` does, VoiceOver, Dynamic
   Type, the 30-minute battery runs against Apple's Music app, memory under the budget with 2000 albums in
   the grid, the offload experiment (5.3) measured.
5. **Release**: a `tools/release.sh` step that produces `nori-ipod.ipa` beside the APK; install over
   `ideviceinstaller`, a Zebra repo later if wanted.

## 10. Risks and how each is met

| Risk | Answer |
|---|---|
| Xcode 15's linker makes a binary iOS 12 will not load | `-ld_classic` from milestone 0, lld since; the app already on the device proves an Xcode 12.5-linked arm64 app runs, so the SDK 17.2 headers with a 12.2 minimum is the only new variable |
| uniffi's pinned revision will not generate Swift | Hand-written C ABI (cbindgen) for everything; the doors are planned that way regardless |
| RemoteIO will not grant 93 ms on some route (Bluetooth often caps at ~40 ms) | Read `ioBufferDuration` after activation and size `latency_us` from it; the engine's ring and bursts do not depend on the buffer length, only the wake count does |
| 1 GB: jetsam kills the app in the background | The budget in section 7; `didReceiveMemoryWarning` trims the cover cache and the core's memory (`trim_memory`); the engine's loaders are bounded by `load_control`'s byte cap and reported by `Engine::held` for the perf log; measured in milestone 4 |
| A8 too slow for AutoMix's analysis of two songs around a mix | The measurer already runs at the lowest priority on one thread; if a 5-minute FLAC takes more than its own length to analyse, AutoMix measures only as songs come (`measure_as_it_comes`) and never from the disk |
| No debugger | The core's log over SSH, `os_log` from Swift, crash reports in `/var/mobile/Library/Logs/CrashReporter`, and `atos` against the unstripped build |
| checkra1n's semi-tethered boot: a reboot without a computer loses the jailbreak | The app does not depend on the jailbreak at runtime; only AppSync's signature bypass does, and an unsigned app will not launch after an unjailbroken boot. A free developer certificate (7-day) from Xcode is the fallback for a trip |

## 11. The owner's decisions (2026-10-03)

- **Dark theme by default.** The light theme stays as a setting; the AMOLED-black one is hidden (LCD).
- **The headphone jack is the output.** No Lightning DAC: the `Usb` route, bit-perfect reopening per song
  and the DAC pages are not built for this client; Bluetooth and the speaker are the other two routes.
- **Downloads run while the app is open or playing.** No `NSURLSession` background sessions; the Downloads
  page says so in one line.
- **The player is black.** (2026-10-04) Whatever the cover; its colours tint nothing.
- **The icons are Android's.** (2026-10-04) Its Material icons, drawn from their path data.
- **Other music apps on the device are not a reference.** Nothing of their behaviour is copied; the
  reference is Android's app and Apple's own Music app (section 6).

## 12. Where the skeleton stands (2026-10-03)

In the tree, uncommitted:

- `crates/ios` (`nori-ios`): a staticlib over `nori-host`, in the workspace. Its C ABI today is
  `nori_ios_version`, `nori_ios_probe(data_dir)` (opens the settings database; proves SQLite and the
  filesystem on the device) and `nori_ios_free`. Two tests (`cargo test -p nori-ios`) cover the ABI's
  string handling and the probe.
- `ios/`: `Sources/main.swift`, `AppDelegate.swift` (the data directory under Application Support,
  excluded from backup), `ProbeViewController.swift` (one label: the version and the probe's note, the
  core called off the main thread), `Sources/nori_ios.h` (the bridging header, kept in step with lib.rs by
  hand), `Info.plist` (`dev.nori.music`, iOS 12.2, portrait, `audio` background mode, launch image),
  `Launch-568h@2x.png`, `entitlements.plist`, `tools/ios-build/build.sh` (what builds it).
- `tools/ipod.sh` with the steps `build install run` (section 4).
- `docs/ipod.md`, this file.

**Milestone 1 is done**: `rust` (48 s, 9.2 MB), `app` (Swift 5.9.2, 2.7 MB executable,
`minos 12.2`), `sign`, `install`, and the app runs on the iPod as `mobile` at 11 MB RSS, having created
`nori.db` in its data dir (SQLite and the filesystem through the whole stack). Two things were learned on
the way and are in section 4: the system-app entitlements, and dropbear's key auth. The only part not
seen with eyes is the label's text (no screenshot tool on the device); the database's existence is the
probe's answer.

Past the skeleton: nori-host takes the client's output (W2), the ABI stays C (W3), `IosOutput` plus the
RemoteIO shim are in the tree with their Rust tests (W4), and one session plays a queue of files through
the C controls onto `WavOutput` (W5). The iPod shows the shell (W7): four tabs and the mini player.
Settings → Server checks an address and saves it (W9a); a saved server opens the session at launch.

The pages are drawn (W6–W12 in a first form, each section below says what is left). One door carries
every page: `nori_ios_read(token, kind, arg)` answers on the page callback as JSON
(`crates/ios/src/pages.rs`), song lists stay in Rust under the token so play / enqueue / download name a
token and an index, never ids. Covers come decoded as RGBA on the cover callback. On the device the app
runs at about 60 MB RSS against a real server, Home fetches its covers, and no crash log has come since.
What has not been seen with eyes: every screen (no screenshot tool on the device), the sound through the
jack, and the lock screen.

## 13. Work packages, for delegation

Each package is one agent's piece of work: self-contained, with what it reads from the core, what it
writes, and how it is checked. The order is the dependency order; packages on one line can run side by
side. Every package follows AGENTS.md (Rust decides, Swift draws; nothing on the main thread waits on the
core; a bug fix starts with a failing test; no dead code) and ends with `cargo test -j4 --workspace`,
`cargo clippy -j4 --workspace --all-targets`, and `tools/ipod.sh` run to the device for anything that
reaches it.

### W1 - done: milestone 1 on the device (section 12)

What remains of it for the next package: `run` should tail the core's log file once W5 opens a session
that writes one.

### W2 - done: the host seam ‖ W3 - done: the bindings

**W2.** `Open` takes `output: Box<dyn AudioOutput>`, `volume: Arc<OutputVolume>` (listener volume in dB;
the client keeps the device volume and calls `Session::volume_changed`) and `memory_mb`. cpal left
nori-host: the cli and desktop build `CpalOutput` themselves and `set_volume` lives on their sessions.
`nori-mpris` is the `desktop` feature (on by default); nori-ios depends with `default-features = false`,
so it links neither cpal nor MPRIS (`cargo tree -p nori-ios` names neither).

**W3.** The ABI stays C. `crates/uniffi-bindgen` generates Kotlin only, and the render callback cannot
cross a generated binding, so there is no Swift backend to try and no cbindgen step: `ios/Sources/nori_ios.h`
is kept in step with the `extern "C"` functions by hand. The note is at the top of `crates/ios/src/lib.rs`.

### W4 - done on the Rust side: `IosOutput` and the audio shim

`crates/ios/src/output.rs`: `Sink`, `IosOutput` (float, 93 ms / 10 ms, latency from the callback's stamp,
`mixed_us` on Bluetooth, one reopen after a media-services reset). Tests cover the grant, the latency
sum, shallow, a route change, a failed reopen, and no allocation on the render path.
`ios/Sound/NoriAudio.m` is the AURemoteIO unit (syntax-checked against the iOS 17.2 SDK); `tools/ios-build/build.sh`
compiles it. The device listen (a sine, both buffer sizes, a Bluetooth change) still waits: W5 plays in
a Rust test, and that build is not on the iPod. The line in docs/testing.md says why that part cannot
be Rust.

Rust (`crates/ios/src/output.rs`, tested on the virtual clock with a simulated sink, as
`crates/android/src/track.rs` is):

- A `Sink` trait the shim implements: `open(rate, channels, io_ms) -> granted (rate, io_ms, latency_us)`,
  `start`, `stop`, `set_io_ms`, `latency_us`, `route() -> (kind, name)`, and the render callback's
  entry `nori_ios_render(frames, out: *mut f32)` that calls `Feed::pull` - no lock, no allocation, nothing
  but the copy and the host-time stamp.
- `IosOutput: AudioOutput` over it: `open` asks the sink for the song's rate family; `takes_float` true;
  `latency_us` = granted latency + io buffer + unrendered frames since the last callback (as
  `nori-output-cpal`'s `Heard`); `mixed_us` = the route's output latency on Bluetooth; `shallow(on)` →
  `set_io_ms(10 | 93)`; `ramp` false, `flush` no-op, `bursts` false; `failed` after a media-services reset
  the shim could not recover; `watch` wired to the route callback with the `OutputKind` mapping of 5.1.
- Tests: the device format asked and granted, latency arithmetic, shallow switching, a route change
  reaching the watcher, a failed reopen reaching the engine, and the `no_alloc` check on the render path.

ObjC/C (`ios/Sound/NoriAudio.m`, `NoriAudio.h`, added to `tools/ios-build/build.sh` with `-framework
AVFoundation -framework AudioToolbox`): AVAudioSession (category playback; preferred rate and I/O
duration; activation; `outputLatency`, the current route's port type and name), an AURemoteIO unit in
Float32 interleaved at the granted rate, the render callback forwarding to `nori_ios_render`,
interruption / route-change / media-reset notifications forwarded as C callbacks. No logic: every decision
the shim seems to make (pause on headphones out, resume after an interruption) is the Rust side's answer.

Device check (a line in docs/testing.md saying why it cannot be Rust): a sine from the engine's test tone
through the jack with no underrun for a minute at 93 ms, at 10 ms, and across a Bluetooth route change.

### W5 - done on the Rust side: the session behind the ABI

`crates/ios/src/session.rs` holds one `nori_host::Session` in a `OnceLock` (the C entry points have no
handle). `nori_ios_open(data_dir, server_id)` loads the saved server — an empty id is the active one —
with `IosOutput` on device, `nori_http::Http`, covers on and `memory_mb` 96. No saved server returns an
error and leaves the slot empty. Every `Said` is queued and delivered on `nori-ios-out`, which calls
`Session::followed` and then the Swift callback; engine events are a `repr(C)` `NoriReport` (state,
index, ms, jumps, and a kind for the rest). The controls (`play_at`, `toggle`, `next`, `previous`,
`seek`, `go_to`, `set_repeat`, `shuffle`, `remove`, `put_back`, `move`, `clear_upcoming`) are thin
wrappers. `nori_ios_background` saves now (`QueueMoment::Closing`, no push). `nori_ios_memory_warning`
calls `Loader::trim`, which drops the decoded-cover cache and rests the workers.

The Rust check opens a temp dir on `WavOutput` and walks two downloaded songs through those controls,
including the saved queue and a memory warning that leaves playback usable. The header is
`ios/Sources/nori_ios.h`. A profile that plays a real server is milestone 2's end, and is not
installed.

### W6 - the OS around the player (Swift)

In `ios/Sources/Player.swift` as `NowPlaying`: the info centre (600 px artwork once per song), the
remote commands, like → star, and `AVAudioSession.outputVolume` → `nori_ios_volume` for loudness. Route
lost pauses and an interruption pauses / resumes, decided in `session.rs::audio_changed`. Left: the
device check below.

`ios/Sources/NowPlaying.swift`: `MPNowPlayingInfoCenter` fed on `Event::Song`, `State`, a seek's
`Position`, with the artwork from W8's loader at 600 px once per song; `MPRemoteCommandCenter` for play,
pause, toggle, next, previous, `changePlaybackPosition`, like → star; the interruption and route events
from W4's callbacks turned into the core's answers (`rules.rs` for the fade); `applicationDidEnterBackground`
→ `nori_ios_background`. Check on the device: lock screen controls, the EarPods clicker, headphones pulled
pauses within the fade, Bluetooth speaker gets its own profile row in the device's sound page later (W12).

### W7 - the app shell and design system (Swift) ‖ W8 - covers

The shell is on the device: `ShellController` (Home, Library, Search, Settings), large titles, the dark
theme, drawn glyphs, and the mini player (cover, song, play/pause, next, a progress hairline ticking once a
second only while playing and in front). `Say.swift` holds the words in English, `Fmt` the times, sizes
and quality line. Every screen word goes through `Say`, in `strings.xml`'s wording where Android has the
phrase; text styles follow Dynamic Type live (the card keeps its fixed sizes, fitted to 568 pt);
VoiceOver labels on every glyph button and on the drawn lyric lines. Left: the `fmt-check` vectors.

W8 is in `pages.rs` (`nori_ios_cover` / `_cancel`, RGBA on the cover callback, copied once into a
`CGImage`) and `Covers.swift` (`CoverCache` 16 MB, trimmed on memory warning; `CoverView` fading in).
Covers reach Swift without a copy: the callback carries an owner that `CGDataProvider` releases
(`nori_ios_cover_release`). Only the core's two renditions are fetched (`cover_rendition`: 320 px for
rows and cards, 800 px for the album page, the player and the lock screen), each decoded to the view's
size, so every view of a cover shares one download; at each song change its covers and its neighbours'
are warmed into the disk cache (`covers_around`), and the lock screen's artwork is decoded at the card's
size into the cache the card reads. A size not decoded yet shows the cover's sharpest decoded picture
until it comes. While one loads a sheen crosses the plate (three passes at most) and the
picture fades in over 260 ms; a cached picture shows at once.

**W7.** `UITabBarController` (Home, Library, Search, Settings), each in a `UINavigationController` with
large titles on the roots; the mini player over the tab bar; `Design.swift` (the Apple-Music-like shapes:
list rows, grid cells, section headers, the pill buttons, the play/shuffle pair; SF system font, Dynamic
Type through `UIFontMetrics`); `Theme.swift` (dark default, light; the colours from the core's theme
model); the glyph set as PDF assets rendered to @2x by a script step (`actool` is in Xcode but, like
`ibtool`, may crash: fall back to pre-rendered PNGs in `ios/Assets/`); `Say.swift` over
`Localizable.strings` carried from `app/src/main/res/values/strings*.xml` by a one-off script
(`tools/ios-strings.py`, kept, since the Android words move), and `Fmt.swift` mirroring `core/text/Fmt.kt`
with its vectors as a test (`swift test` is not available on this toolchain: a tiny `fmt-check` executable
in `tools/ios-build/build.sh`). Check: every tab opens with a placeholder page; the mini player shows
the session's state.

**W8.** `crates/ios/src/covers.rs`: a `nori_covers::Paint` writing straight RGBA into a buffer that
Swift turns into a `CGImage` through `CGDataProvider` with a Rust release callback (no copy); the request /
cancel / done doors as Android's `CoverPixels` has them; `CoverLoader.swift` as the `UIImage` cache keyed
per address keeping the largest, trimmed on memory warning; `CoverView` (plate, sheen, fade in over 260 ms,
the note glyph when nothing comes). Check: a Rust test decodes a JPEG into the paint's buffer at a
padded stride; on the device, the album grid scrolls at 60 fps with the GX6450's overdraw at one layer per
cell.

### W9 - the pages (Swift, each its own package once W7 and W8 are in)

Each reads one core page record through the coarse path and draws it; words through `Say`; no logic.

Drawn in `Pages.swift` / `Shell.swift`: Home shelves, Library (Albums grid with its orders and the A-Z
index, Artists, Songs paged, Playlists, Genres, Favorites, Smart playlists, Downloaded with progress),
album / artist / playlist / genre / smart pages with Play and Shuffle, swipe actions, the song menu on a
long press (`menus::song_menu` through `crates/ios/src/menu.rs`: favorite, play next, queue, add to a
playlist or a new one, download, go to album / artist, radio, instant mix, exclude from mixes, share,
details, and from the player the sleep timer), removing from and deleting a playlist, Search (debounced
by `liveSearchDelayMs`, recent searches, provider items marked), Downloads with the open-or-playing
line. A headed page's Play and Shuffle are the core's `hero_buttons` (`nori_ios_hero`): while the queue
playing was started from that page, Play is Pause and Shuffle, lit, turns its shuffle off. The queue is
a sheet from the player (`Queue.swift`): play order from `queue_rows`, the song
playing on top, History above it, Playing next with Clear, a row held and dragged to move it, a swipe to
remove with Undo, shuffle and repeat pinned (the card has only previous, play and next, as on Android).
The card, the queue and the lyrics close the same way (`DragToClose`): a drag down on the sheet (on
the queue and lyrics, on the strip above the list) or a pull past the list's top.

- **W9a Login and profiles** — the form is on the device: Settings → Server asks for an address, user
  and password, `nori_ios_login` pings through `login_check` and saves the profile (`servers_activated`),
  and a saved server opens the session at the next launch (`nori_ios_open`). A second server saved while
  one session is already open waits until the next launch. Settings → Servers switches between saved
  profiles and forgets one. The form's Advanced fields are a name, the second address and an API key
  (which stands in for the user and password). An unreachable server opens on what is stored, as every
  session does. Out: custom headers and accepting a self-signed certificate - `nori-http` (the
  desktop and iPod transport) applies neither yet; only Android's OkHttp does.
- **W9b Home**: the core's home rows (`read_cached` with `PageShown`), shelves as horizontal collections.
- **W9c Library**: the root list, then Albums (grid, `AlbumSort`, A-Z scroller), Artists, Songs (orders),
  Playlists, Genres, Downloaded, Smart lists.
- **W9d Album / Artist / Playlist / Genre pages**: `AlbumDetail`, `ArtistDetail`, `PlaylistDetail` with
  their captions and big buttons from `pages`, the row swipe (`RowSwipeAct`), the song menu (`SongAction`s
  as a `UIAlertController` sheet), the download entry (`DownloadAct`).
- **W9e Search**: `SearchSession` at every key, the server once typing pauses (`live_search_delay_ms`),
  the scopes, recent searches, provider items marked and never queued.
- **W9f Queue**: `Session::view`, reorder with the table's drag handles, remove, clear upcoming, put back,
  shuffle and repeat.
- **W9g Downloads**: `download_sections`, progress and phases from `transfers`, the one line that says
  downloads run while the app is open or playing.

### W10 - the player card and mini player (Swift)

`PlayerCard` in `Player.swift`: the 6.2 layout, the paused cover scale (spring, snaps with reduce
motion), seek with times, transport, shuffle / repeat, `MPVolumeView`, `AVRoutePickerView`, star, go to
album / artist, the song menu. It opens and closes through `CardTransition`: UIKit's own presentation
with a `UIPercentDrivenInteractiveTransition` the finger drives (up from the mini player, down on the
card); the card's transform is never touched, since UIKit sets its frame while presenting. Its page is
black. The seek timer runs at 2 Hz only while the card is shown and
playing.

The custom presentation controller and interactive transition (one pan, `UISpringTimingParameters`,
scrubbed), the layout of 6.2, the paused cover scale, the seek bar drawn once per pixel with
`nori_look::motion`'s step through a door, times from `Fmt` cached per second, `MPVolumeView`,
`AVRoutePickerView`, star and menu, the wash and colours from `nori_look` (`dress`, `cover::derive`) through
doors with the per-frame `mix` only while a change runs. Check: the card opens and settles with the finger,
reduce-motion snaps; the player draws nothing while paused and nothing per second with the screen off
(`CADisplayLink` count in the debug overlay).

### W11 - lyrics (Swift + one door)

`LyricsCard`: `nori_ios_lyrics` asks, `Said::Lyrics` lands in `pages.rs`, `nori_ios_lyrics_page` reads the
lines; the sung line is lit by a one-shot timer set to the next line's start (no display link), a tap
seeks. Now paced by the core's `LyricClock` behind a handle (`crates/ios/src/lyrics.rs`): lines lit
by `line_strength`, the sung line filled word by word with CoreText when the words are timed, a display
link only while a word fills and a one-shot timer between, nothing while paused, buffering or hidden.

`LyricsView` drawing lines with CoreText from the record's words, lit by `line_strength`/`UNSUNG`, paced by
the core's `LyricClock` (`Step::wait`, `still`) through a door, the display link only while a word fill
moves; `Session::lyrics` → `Said::Lyrics` with `lyrics_replaces` and `matching_line` on a replacement; tap
to seek. Check: against the Android screenshot `docs/screenshots/lyrics.png` for the look; no wake with the
page hidden.

### W12 - settings, equalizer, per-device sound (Swift)

Settings shows a curated set (playback, sound, lyrics, server) from `settings_model` through
`nori_ios_settings` / `nori_ios_set`, the index refresh, and About. The equalizer page moves the graphic
bands (±12 dB, 0.5 steps) and calls `nori_ios_tuning` for the shallow buffer, with the built-in presets.
Settings → Sound lists the outputs and the sound each gets (`crates/ios/src/sound.rs`, as Android's
`DeviceSound`: automatic, flat, no processing, leave as is, a profile, an AutoEQ curve; forget). The
output playing now is read from the audio session's route when asked (`output::route_key`, keyed as the
engine keys outputs), not remembered from events, and the list reads it again on a route change; and
Headphone presets is the AutoEQ list (download, search, apply to the current sound). Settings → About →
Licenses lists `core_credits`, the Material icons from `android_credits` and `data_credits`, with the
texts from Android's assets. Settings has Android's Queue section (skip explicit songs, keep playing when
the queue ends and, while it does, carry on with songs or albums, chosen by, remote songs) and When
something goes wrong (skip songs that won't play, play downloads when offline).

The curated settings pages for this client (what a phone-only setting is, is hidden: metered network,
offload, bit-perfect, USB, AMOLED), each row from `settings_model::specs` and `state`, changes through
`setting_set` → `SettingChange::effect` → `Session::apply`; the equalizer screen with the bands as
sliders, `edit_band`, presets, the shallow mode through `rules::equalizer_tuning` (in sight, touched,
eq on) → `set_shallow`; the per-output sound page over `nori-devices` (speaker, wired, each Bluetooth
device by name) and the AutoEQ offer for a named headphone; the about and credits pages (`core_credits`,
a new `ios_credits` list for the Swift side's few third parties - expected none). Check: a band moved is
heard within 50 ms on the jack with the screen open; the settings' `tests/stored_format.rs` untouched.

### W13 - measuring (tooling)

Done: `tools/ipod-bench.sh` (over Wi-Fi, since the USB cable charges the battery; the current, the
battery counter's mAh, the app's CPU time and RSS). Not built: `perf.rs` - iOS has no public per-thread
count of voluntary context switches (Android's recorder ranks threads by them; Mach gives per-thread
CPU and only task-wide switches), and nothing reaches into the app on request over SSH; the bench reads
the app's CPU and memory from outside instead. The plan:
`tools/ipod-bench.sh`: 30 minutes of a fixed playlist, screen off, Wi-Fi on; every 30 s over SSH
`ioreg -rc AppleARMPMUCharger` (`InstantAmperage`, `Voltage`, capacity) and `ps` for the app's CPU and
RSS; a table at the end, and the same run for Apple's Music app with the same songs downloaded. The
perf recorder from `nori-perf` fed by a `crates/ios/src/perf.rs` reading `task_threads` / `thread_info`
(per-thread CPU and context switches) on request, written to the log. Targets in section 8.

### W14 - polish and release

The motion pass (every animation listed as `docs/motion.md` does, with its status), VoiceOver labels,
Dynamic Type at the largest size on 320 pt, the memory budget with a 2000-album grid, the offload
experiment of 5.3 if W13's numbers say the I/O thread is what costs. Done: `tools/release.sh` produces
the .ipa (`tools/ipod.sh build`, Docker only); `--no-ipod` makes a release of the APK alone. The plan: `tools/release.sh` producing
`build/nori-ipod-<version>.ipa` (a `Payload/` zip of the signed `nori.app`) beside the APK, and the
changelog's `feat`/`fix`/`perf` subjects covering the iPod as they cover Android.
