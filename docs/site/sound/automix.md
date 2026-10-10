---
title: AutoMix and crossfade
description: How nori blends one song into the next, and when it does not.
sidebar:
  order: 4
---

Applies to nori 0.6 on every client.

## AutoMix

**AutoMix** (off by default) blends songs like a DJ, matching the beat. nori measures each song on the
device as it arrives: its beats, bars, tempo, key and loudness. With both songs of a transition
measured, it plans a mix:

| Setting | Default | What it does |
|---|---|---|
| Longest mix | 12 s | how long a mix may last (6 to 24 s) |
| Match the beat | on | nudges the next song's speed so the beats line up |
| Biggest speed change | 6 % | how far a song may be sped up or slowed down to match (2 to 8 %) |
| Keep the pitch | on | off lets the pitch follow the speed, up to 2 % |
| Swap the bass | on | the new song's bass replaces the old one's |
| Muffle the ending | on | the outgoing song fades out muffled |
| Echo out clashes | on | overlapping vocals end in an echo instead |

**Better beat detection** (off by default) runs a small neural beat tracker (Beat This!) over the start
and end of each song, for mixes that land on the beat more often. It needs a one-time download of about
8 MB from the model's authors, on Wi-Fi unless you allow mobile data.

### When AutoMix does not mix

- **The songs are not measured yet.** A song is measured as it streams or downloads. Until both songs
  are, the transition is a plain fade.
- **An album plays in order.** With **Keep albums gapless** on (the default), songs of one album that
  follow each other in order join without a gap, as the album was made.
- **The tempos are too far apart.** Beyond the biggest speed change, nori does not force a beat match.
- **Provider songs** from octo-fiesta are never measured.

## Crossfade

Without AutoMix, **Crossfade** overlaps the end of one song with the start of the next for 2 to 12
seconds (off by default). The curve, and how long the fade in and fade out each take within it, can be
set. **Keep albums gapless** applies here too. **Fade on play and pause** softens starts and stops
(off, or 150 ms to 1 s).

Gapless playback needs no setting: songs that run into each other join sample for sample.
