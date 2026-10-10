---
title: Library and search
description: Browsing your library, searching it instantly, and choosing what a tap and a swipe do.
sidebar:
  order: 1
---

Applies to nori 0.6. Every client browses and searches the same way; the gestures are Android's.

## Browsing

The library lists albums, artists, songs, playlists, genres, favourites, years and decades, folders as
the server keeps them, internet radio and downloads. Album lists sort by name, artist, year, date added,
last played, most played, starred or at random. An artist's page groups albums, EPs, singles, live
albums and compilations, with top songs and similar artists. Each song's info sheet shows its file, codec,
sample rate, bit depth and ReplayGain.

## Search

Search answers at every key from an index kept on the device, so it works offline and is instant. When
you pause typing, it asks the server too and merges the results. Accents do not matter. The last 20
searches are kept.

Provider songs (from octo-fiesta) are marked as such, and choosing one plays only that song: streaming it
makes the server download it, so nori never queues one you did not ask for.

## Taps and swipes

On Android, a song row reacts to a tap and to a swipe in each direction. All three are settings:

| Setting | Default | Options |
|---|---|---|
| Choosing a song | Plays the list from there | plays only that song, adds it to the queue, plays it next |
| Swipe right | Add to queue | nothing, play next, favourite, download |
| Swipe left | Favourite | nothing, add to queue, play next, download |

Hold a row to select several and act on them together.
