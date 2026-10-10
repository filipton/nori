---
title: Privacy and network
description: Every request nori makes, to whom, and the setting that controls it.
sidebar:
  order: 1
---

Applies to nori 0.6 on every client, except where a row names one.

nori has no accounts of its own and no analytics. Your music stays on your server: nori streams it from
there and keeps what you download on your device. Everything else it asks for online is listed below,
each with the switch that stops it.

## Your server

Everything about your library, your playlists, your favourites, your plays and the music itself goes to
the server you signed in to, and nowhere else. Scrobbles go to your server too; whether it passes them on
(to Last.fm or ListenBrainz, for example) is the server's setting.

## Look things up online

One switch, **Look things up online** (Settings, on by default), is over the first three rows. Off, none
of them asks anything.

| Request | Sent to | What is sent | When | Controlled by |
|---|---|---|---|---|
| Missing lyrics | the [lyrics sources](lyrics/sources.md): PaxSenix (and Apple's public iTunes Search to find the song), BiniLyrics, Unison, BetterLyrics, KuGou, NetEase, LyricsPlus mirrors, SimpMusic (and YouTube Music to find the song's video), LRCLIB, YouTube, Megalobiz, Genius | artist, song and album name, the song's length; your key, for a service you gave one | when you open the lyrics of a song your server has no timed lyrics for; the answer is kept | **Find missing lyrics online** (on), and each source's own switch |
| The AutoEQ headphone list | raw.githubusercontent.com (the AutoEQ project) | nothing about you | on Wi-Fi when the list is missing or a month old; a curve when you choose it | **Keep the AutoEQ list** (on) |
| Moving covers (Android) | Apple: itunes.apple.com, music.apple.com and its catalogue API | artist and album name | when the player opens on an album, by default only on Wi-Fi; the answer is kept | **Moving covers** (off) |

## Other requests

| Request | Sent to | What is sent | When | Controlled by |
|---|---|---|---|---|
| Update check (Android) | api.github.com (this project's releases) | nothing about you | when the app starts, at most once a day | **Check automatically** (on) |
| Better beat detection model | cloud.cp.jku.at (the Beat This! authors) | nothing about you | once, about 8 MB, by default only on Wi-Fi | **Better beat detection** (off) |
| Remote control | your own devices on the network (mDNS), or your server's octo-fiesta relay | what plays and the controls, signed with your server credentials | while music plays or another device is shown | **Control from other devices** (off) |
| Jams | your server's octo-fiesta relay | the jam's queue and requests, each member's name | while a jam runs | **Jams** (off) |

Lookups made for provider songs (octo-fiesta's streaming providers) are never made: no lyrics, no
analysis.

## On your device

What nori keeps (your settings, the library index, covers, the stream cache, downloads, lyrics found
online, AutoMix's measurements and the taste model) stays in its own storage on the device. Settings,
Storage shows the sizes and clears each.
