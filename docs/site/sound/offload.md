---
title: Offload and processing
description: When the audio chip plays the music, and what makes nori decode it on the CPU instead.
sidebar:
  order: 6
---

Applies to nori 0.6 on Android 10 and later.

**Save battery while playing** (on by default) lets the phone's audio chip decode MP3, AAC and Opus
itself. The CPU hands it minutes of music at a time and sleeps in between. This is called offload.

Offload needs the music untouched. It pauses, and nori decodes on the CPU in short bursts, whenever
something needs the samples:

- the equalizer or any effect;
- speed, pitch or skipping silence;
- AutoMix, or a crossfade between songs;
- a song turned up by ReplayGain (turning down is fine);
- a format the chip does not decode.

The settings page says when offload is paused, and why. It is never used for a USB DAC.

## High quality output

**High quality output** (off by default) plays 24-bit files in full and runs the equalizer and effects in
floating point. It is best on a USB DAC or a hi-res output and starts with the next song. **Highest
sample rate** caps the rate sent to the output (each song's own by default, or 48, 96 or 192 kHz); a
high-quality resampler runs only when it has to.

:::note
A **Bit perfect USB DAC** setting (Android 14 and later) sends files to a USB DAC unchanged. It has not
been tested with a real DAC yet.
:::
