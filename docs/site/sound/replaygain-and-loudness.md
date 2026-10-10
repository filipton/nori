---
title: ReplayGain and loudness
description: Evening out the volume between songs and albums.
sidebar:
  order: 5
---

Applies to nori 0.6 on every client.

**Even out volume** reads the ReplayGain tags your files carry (off by default):

| Choice | What it does |
|---|---|
| Per song | every song at the same loudness |
| Per album | each album at one level, keeping its quiet and loud songs as they are |
| Automatic | per album when the queue is one album, per song otherwise |

With it on:

- **Loudness target**: −14, −16, −18 (ReplayGain's own, the default) or −23 LUFS. Navidrome hands R128
  tags over as ReplayGain, so those work too.
- **Turn quiet songs up** (off by default, up to +12 dB): songs that need it are turned up, with the
  limiter behind them so they cannot clip.
- **Volume for songs without tags**: −6 dB by default (0 to −12 dB).
- **Measure songs without tags**: songs without tags play at the loudness AutoMix measured, once it has.

Turning a song down is only a volume change, so [offload](offload.md) stays on. Turning one up needs the
samples, so that song plays on the CPU.
