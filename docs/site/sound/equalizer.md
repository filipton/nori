---
title: Equalizer and effects
description: The graphic and parametric equalizer, presets, and the effects around it.
sidebar:
  order: 1
---

Applies to nori 0.6. Every client has the equalizer; the effects' rows are Android's and the desktop's.

The equalizer runs in nori's own sound chain, so it sounds the same on every client and every output. A
change is heard at once, seamlessly, while the music plays: nothing stops and nothing is decoded again.

## Graphic or parametric

- **Graphic** (the default): 10 sliders, or 5, 15 or 31, from −12 to +12 dB.
- **Parametric**: any number of bands, each a peak, a low or high shelf, a low or high pass, a band pass
  or a notch, on both channels or one. Paste a preset from Equalizer APO, or a `GraphicEQ.txt`, to
  import it.

The pre-amp is set automatically so a boost cannot clip, and a look-ahead limiter catches what is left.
Built-in presets cover bass, treble, vocals and loudness, and your own setups can be saved as profiles.

## Effects

| Effect | What it does |
|---|---|
| Bass boost, volume boost | quick boosts, kept clean by the limiter |
| Compressor | brings quiet passages up and loud ones down, for noisy places or quiet listening (gentle, balanced or strong) |
| Noise gate | turns hiss and hum down further when the music goes quiet |
| Loudness compensation | as you turn the volume down, brings the bass and the highest notes up the way the ear needs (ISO 226) |
| Crossfeed | blends a little of each channel into the other on headphones (default, Chu Moy or Jan Meier) |
| Balance, mono | left and right, or both channels as one |

Any of these turns [offload](offload.md) off while it is on, since it needs the samples.

While the app is on screen, Android keeps the audio buffer short, so a band you move is heard straight
away; in the background the deep buffer that saves battery comes back. The desktop and terminal do the
same while their equalizer page is open.
