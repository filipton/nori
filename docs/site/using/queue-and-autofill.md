---
title: Queue and autofill
description: The queue, shuffle and repeat, and how nori keeps playing when the queue runs out.
sidebar:
  order: 2
---

Applies to nori 0.6 on every client.

## The queue

The queue is kept by the core and saved as you go, so it survives the app being closed or the phone
restarting, with its place in the current song. Add a song to the end or to play next, move it by its
handle, swipe it away (with Undo), or clear what is coming.

- **Shuffle** spreads artists and albums apart, so the same artist rarely plays twice in a row.
- **Repeat** goes off, all, one.
- **Skip explicit songs** leaves out songs the server marks explicit.
- **Skip songs that won't play** moves on after an error, and stops after a run of them.

To carry the queue to another of your devices, see [Remote control](../connected/remote-control.md).

## Keep playing when the queue ends

Autofill is on by default. When the last song starts, nori adds more music:

| Setting | Default | Options |
|---|---|---|
| Carry on with | Songs | songs or albums |
| Chosen by | Similar music | similar music, the same artist, the same genre, the same era |
| Include remote songs | off | songs from octo-fiesta's providers; each one played is downloaded into your library |

## Mixes

On Android, the desktop and the iPod, the **taste model** learns from what you play, skip and finish, on the device, and makes daily and weekly
mixes from it. Nothing it learns leaves the device. Any song, album or artist can start an instant mix,
and any song can be excluded from mixes.
