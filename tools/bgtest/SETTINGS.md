# What each player offers, and where

Mapped by hand on the S21 FE (SM-G990B, Android 16) on 2026-09-26, apps in English
(`cmd locale set-app-locales <pkg> --locales en-US`). Versions: Nori perf 0.3.3 (b2994591), Musly and
Navic as installed from the S22, Symfonium 15.1.0 from repo.symfonium.app. "Default" is a fresh install
(`pm clear`). Paths are what to tap from the app's home screen.

| Feature | Nori | Musly | Navic | Symfonium |
|---|---|---|---|---|
| **Crossfade** | Settings > Playback > Crossfade (default Off) | Settings > Playback > Smart Crossfade slider, Off–12 s in 1 s steps (default Off) | none | Settings > Playback > Transitions > Crossfade (default off; "disabled when playing albums in order") |
| **Beat-aware / smart mixing** | Playback > AutoMix (default off): beat matching, longest mix 12 s, speed change up to 6 %, bass swap, muffled ending, echo | none ("Auto DJ" only picks what plays next) | none | Transitions > Smart fades (default off): "best crossfade from the waveform, may increase battery usage" |
| **Neural beat detection** | Playback > AutoMix > Better beat detection (default off, 8 MB model download) | Storage > BPM analysis: caches BPMs, "Cache All BPMs" (on demand) | none | none |
| **Queue auto-fill** | Playback > Keep playing when the queue ends (default on, similar music) | Playback > Auto DJ Mode: Off / Shuffle Library / Similar Songs / Same Genre / Same Artist / Smart Mix (default Off) | Playback > Auto-fill queue | Smart flow (up to 12 tracks) |
| **Gapless** | Playback > Keep albums gapless (default on); gapless is always on | Playback > Gapless Playback (default on) | Playback > Audio effects > Gapless playback (default on, experimental, needs restart) | built in |
| **Fade on play/pause** | Playback > Fade on play and pause (default Off) | Playback > Enable Fade In/Out | none | Transitions > Fade in / Fade out (default disabled) |
| **Equalizer** | Settings > Sound > Equalizer and crossfeed: 10-band parametric, presets, pre-amp, limiter, balance, mono, crossfeed, AutoEQ (default off) | **none** (the player menu has only sleep timer, speed, pitch) | Playback > Audio effects > Equaliser > source: Disabled / Built-in / External (default Disabled) | Settings > Playback > Output settings > Telefon > Equalizer: parametric and graphic EQ, bass boost, volume boost, compressor, limiter, virtualizer, crossfeed, skip silence (default off) |
| **ReplayGain** | Settings > Sound > Even out volume (default Off) | Playback > Volume Normalization (ReplayGain) > Mode (default Off) | Audio effects > ReplayGain mode: Off / Track / Album / Dynamic (default Off) | Output settings > Telefon > Equalizer > Replay gain (default Off) |
| **Audio offload** (chip decodes, CPU sleeps) | Settings > Sound > Save battery while playing (default on; blocked by EQ, crossfade, AutoMix) | none | Audio effects > Audio offload (default **off**, experimental, needs restart) | none found |
| **Decoder** | Rust engine (Symphonia) | Android MediaCodec via ExoPlayer | Android MediaCodec via ExoPlayer | its own; Output > "Prefer hardware-accelerated codecs" (default off) |
| **Streaming quality** | Downloads and storage > Quality on Wi-Fi: Original / mobile: Opus 192 | Playback > Enable Transcoding (default off = original) | Playback > Streaming quality: Wi-Fi and cellular Low / Medium / High / Lossless (default Lossless) | Playback > Decoding and transcoding: Wi-Fi / mobile maximum bitrate (default Original) |
| **Load ahead / cache** | Downloads and storage > Load ahead on Wi-Fi 2 songs, mobile next song; stream cache 1 GB | Storage > Songs & Streaming Cache | ExoPlayer default buffer | Offline, cache, and download > Playback > Playback cache size (default **Disabled**) |
| **Download for offline** | Downloads and storage > Download (per list, whole library) | Storage > Offline downloads; playlist page "Download playlist" | Data & Storage > Download entire library | Manage offline files |
| **Lyrics from the internet** | Settings > Lyrics > Find missing lyrics online (default **on**, 9 sources) | Playback > Fetch lyrics from LRCLIB (default **on**) | Now Playing > Configure lyric providers: Subsonic on, LRCLIB off, LyricsPlus off | not found in settings (Manage media providers may add lyrics sources; to check) |
| **Keep screen on with lyrics** | Lyrics > Keep the screen on (default on) | Storage > "Keep Screen On" is for downloads only | Now Playing > Keep screen on in lyrics view | — |
| **Scrobbling** | Library > Tell the server what you play (default on) | — | Playback > Enable scrobbling | Playback > play percentages |

## Comparable test variants (proposal)

- **default**: every app as installed.
- **crossfade 6 s**: Nori, Musly, Symfonium (Navic has none).
- **smart mixing**: Nori AutoMix (and with Better beat detection); Symfonium Smart fades; Musly smart crossfade.
- **EQ on** (one preset, e.g. bass boost): Nori, Navic (Built-in), Symfonium. Musly has no EQ.
- **ReplayGain track**: all four.
- **offload**: Nori default (offload allowed) vs Navic with Audio offload on. Musly and Symfonium cannot.
- **lyrics on screen**: screen on, lyrics view open, internet lyrics on: Nori, Musly (LRCLIB), Navic (LRCLIB on).
- **full**: everything each app has from the above at once (Nori AutoMix + beats + EQ + ReplayGain; Musly crossfade + ReplayGain; Navic EQ + ReplayGain; Symfonium smart fades + EQ + ReplayGain).

## Found while testing

- **Nori offload** (b0dffb6e): the platform grants a 32 KB offload track of the 8 MB asked, so the engine
  tops up every ~0.4 s; with the screen off the watchdog calls the track stuck after ~2.8 s of a still
  timestamp, gives offload up and plays on from where the clock (not the chip) says, skipping the rest of
  the song; and the `nori:engine` wakelock is held throughout. Left out of the matrix until fixed.
- **Navic offload** needs gapless off (the S21 FE offloads without gapless support), then plays offloaded,
  but after "next" stays in BUFFERING for good. Its matrix run starts at the first song, no skips.
- **Musly and Navic** decode in Android's mediacodec service (UID 1046), charged to that service, not to
  the app: the harness adds it ("app + decoder + audio").

## Driving notes

- Navic's switches don't report their state to uiautomator: read the switch colour from a screenshot.
- Musly's settings are one Flutter page with tabs ("Playback\nTab 1 of 6"…); the Auto DJ and ReplayGain
  choices are drop-downs opened by tapping the arrow at the row's right; crossfade is a slider.
- Symfonium's pages scroll far: go back to the top before looking for an entry.
