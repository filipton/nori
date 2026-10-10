---
title: Performance
description: How nori keeps the CPU asleep while music plays, how it is measured, and the results so far.
sidebar:
  order: 1
---

Applies to nori 0.6. The model is the same on every client; the measurements are Android's.

## Burst playback

Most players decode a little audio, hand it to the speaker, and wake again a few milliseconds later.
nori decodes a stretch of music ahead instead, then lets the CPU sleep until that stretch is nearly
played.

- On Android the player writes into a deep AudioTrack buffer, about ten seconds of music. The player's
  thread and the track's writer both wake about once every ten seconds while music plays, and nothing
  on the audio path wakes in between.
- With nothing that needs the samples (no equalizer, no speed change, no AutoMix), songs can go to the
  phone's audio chip as they are: **offload**. The CPU then sleeps for minutes between top-ups. See
  [Offload and processing](sound/offload.md).
- Nothing polls or ticks while music plays with the screen off. The seek bar's timer runs only while
  the player is on screen.
- An optional feature that is switched off costs nothing: it is not started, holds no listener and
  opens no connection.

## How it is measured

The comparisons below were made on one Android emulator (`sdk_gphone64_x86_64`, Android 14) against one
local Navidrome server, with the same tracks for every app, media volume at 0, the screen off and no
touches. nori was the release build; the other apps were their store releases.

- **CPU** is the share of one core the app used.
- **Quiet seconds** count the seconds in which every thread of the app stayed asleep: the figure that
  decides battery life with the screen off.
- Playback rows are 90-second windows over a 10-minute MP3 at 320 kbps and a FLAC; the equalizer row
  adds a treble boost in each app that has one.
- An emulator has no real battery and decodes in software, so the absolute numbers do not carry over to
  a phone. The order between apps does.

The perf build measures a real phone in real use, without a computer attached: see
[Building](developers/building.md#the-perf-build).

## Results

:::caution[Historical]
These numbers were measured on nori 0.3.x in September 2026 and are being re-measured for 0.6. The raw
runs, with every thread and profile, are in the repository under
[`docs/performance/raw`](../performance/raw/benchmarks.md).
:::

| Measured on nori 0.3.1 | nori | Symfonium 15.0.1 | Musly 2.0.2 | Navic alpha55 |
|---|---|---|---|---|
| CPU playing MP3 | 1.26 % | 10.7 % | 4.01 % | 3.10 % |
| CPU playing FLAC | 1.30 % | 8.74 % | 4.70 % | 3.72 % |
| CPU playing with the equalizer | 1.35 % | 13.4 % | no equalizer | 2.92 % |
| Quiet seconds, MP3 | 75 of 90 | 1 of 90 | 0 of 90 | 1 of 90 |
| Cold start | about 590 ms | about 900 ms | about 1050 ms | about 700 ms |
| Wake lock while paused | none | none | held | none |

Musly has no equalizer, and Navic has no transitions, so those rows compare features as much as cost.
