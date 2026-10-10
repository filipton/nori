---
title: What nori is
description: A music client for your own server, on Android, desktop, the terminal and the iPod touch.
sidebar:
  order: 1
---

Applies to nori 0.6 on every client.

nori plays the music on your own server. It talks to Navidrome and any other Subsonic or OpenSubsonic
server, and to octo-fiesta, which adds streaming providers and a relay for remote control and jams.

## One core, four clients

Everything that decides how music plays, what the queue does, how lyrics are found and what a setting
means is written once, in Rust. Each client draws its own screens over it:

| Client | Built with | Notes |
|---|---|---|
| [Android](../clients/android.md) | Kotlin and Compose | the main app; widgets and Android Auto |
| [Desktop](../clients/desktop.md) | Slint | Linux and macOS; media keys, MPRIS on Linux |
| [Terminal](../clients/terminal.md) | ratatui | covers drawn in the terminal |
| [iPod touch](../clients/ipod.md) | Swift and UIKit | the 6th generation on iOS 12, jailbroken |

## What it cares about

- **Battery.** Music is decoded in bursts and the CPU sleeps in between. See
  [Performance](../performance.md).
- **Sound.** Gapless albums, an equalizer with AutoEQ curves, per-output profiles and AutoMix
  transitions. See [Equalizer](../sound/equalizer.md) and [AutoMix](../sound/automix.md).
- **Your data.** No accounts, no analytics. Your music stays on your server. See
  [Privacy and network](../privacy.md).

nori is open source under the MIT licence.
