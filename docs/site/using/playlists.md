---
title: Playlists and smart playlists
description: Making playlists, building smart playlists from rules, and importing and exporting M3U.
sidebar:
  order: 3
---

Applies to nori 0.6. Smart playlists are edited on Android; every client plays them.

## Playlists

Create a playlist from a song's menu or from several selected songs, add and remove songs, and delete
it. Playlists live on your server, so every client and every other app sees them. Pin the ones you play
most to Home as favourite playlists (kept on that device). M3U and M3U8 files can be imported and
exported.

## Smart playlists

A smart playlist is a set of rules over your library, worked out on the device from its index. Rules
compare a field with a value, and a group joins its rules with *all* (and) or *any* (or). The fields
include title, album, artist, genre, format, year, length, track and disc number, bitrate, sample rate,
bit depth, size, play and skip counts, last played, date added, favourite, downloaded and excluded from
mixes. A smart playlist can be limited to a number of songs and sorted, including a random order that
stays the same until you ask for a new one.

The editor makes one group of rules; groups inside groups can be written in the playlist's JSON.

Ready-made smart playlists: Most played, Recently played, Recently added, Never played, Top rated,
Forgotten favorites and Long tracks.
