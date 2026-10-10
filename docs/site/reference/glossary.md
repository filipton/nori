---
title: Glossary
description: The words these docs use, in one place.
sidebar:
  order: 4
---

Applies to nori 0.6.

- **AutoMix**: nori's DJ-style transition between songs: both songs are measured on the device and mixed on the beat.
  See [AutoMix and crossfade](../sound/automix.md).
- **Burst playback**: decoding a stretch of music ahead and letting the CPU sleep until it has played. See
  [Performance](../performance.md).
- **Gapless**: songs that run into each other play without a gap, sample for sample, as the album was made.
- **Jam**: a session others join with a link or QR code, without an account, to ask for songs and listen along.
  See [Jams](../connected/jams.md).
- **Offload**: the audio chip decodes the music instead of the CPU. It needs the music untouched. See
  [Offload and processing](../sound/offload.md).
- **octo-fiesta**: a server that sits in front of a Subsonic server, adds streaming providers and relays remote control
  and jams.
- **Provider song**: a song from octo-fiesta's streaming providers, not yet in your library. Playing one makes the server
  download it, so nori never queues one you did not ask for.
- **ReplayGain**: tags that say how loud a song or album is, so it can be played at an even level. See
  [ReplayGain and loudness](../sound/replaygain-and-loudness.md).
