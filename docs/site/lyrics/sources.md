---
title: Lyrics sources
description: Where nori finds lyrics, in what order, and what it sends to find them.
sidebar:
  order: 1
---

Applies to nori 0.6 on every client. The list of sources and its order are edited on Android, the
desktop and the terminal.

## Your server first

Your server's own lyrics always come first. Navidrome and other OpenSubsonic servers can serve them
timed by line or by word. When the server has timed lyrics, no other source is asked.

## Sources online

When the server has no timed lyrics, **Find missing lyrics online** asks sixteen lyrics services. It is on
by default and sits under **Look things up online**; turning either off means nothing is asked. A lookup
sends the artist, song and album name, and the song's length, and happens only when you open the lyrics.
Provider songs from octo-fiesta are never looked up.

:::caution[Unofficial sources]
Only Unison and LRCLIB are open services with a documented API. The others are unofficial: they serve
lyrics taken from other apps and sites, which did not agree to it. They are on by default; switch off any
you would rather not use, or turn off Find missing lyrics online altogether.
:::

The sources, in their default order:

| # | Source | Finest timing | Asked first |
|---|---|---|---|
| 1 | PaxSenix | words | yes |
| 2 | BiniLyrics | words | yes |
| 3 | Unison | words | yes |
| 4 | BetterLyrics | words | |
| 5 | KuGou | words | yes |
| 6 | NetEase Cloud Music | words | |
| 7 | LyricsPlus | words | |
| 8 | SimpMusic | words | yes |
| 9 | BetterLyrics Portato | words | |
| 10 | PaxSenix: Musixmatch | words | needs your PaxSenix key |
| 11 | LRCLIB | words where published, else lines | yes |
| 12 | PaxSenix: Spotify | lines | needs your PaxSenix key |
| 13 | YouTube captions | lines | |
| 14 | Megalobiz | lines | |
| 15 | YouTube Music | untimed | |
| 16 | Genius | untimed | |

## How the best answer is chosen

The sources marked "asked first" are asked together; the others only when those find nothing good. Every
answer is scored on how well it matches the song (title, artist, album and length), how finely it is
timed, whether its times make sense, and whether the other sources agree with it. The best one is shown,
and an answer that scores too low is never shown. With **Prefer word-by-word lyrics** on (the default),
nori keeps looking past lyrics timed line by line.

The lyrics chosen are kept on the device with their source, so a song played again asks nobody. Settings,
Storage, Lyrics shows their size and clears them.

## Changing the order

In Settings, Lyrics, Lyrics sources, each source has a switch, and you can hold a row and drag it to
reorder. The order settles near ties between answers; the score decides the rest.
