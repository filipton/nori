---
title: Remote control
description: Playing on one of your devices and controlling it from the others.
sidebar:
  order: 1
---

Applies to nori 0.6 on Android, the desktop, the terminal and the iPod touch.

With **Control from other devices** on (Settings, Library, Other devices; off by default), your devices
with nori, signed in to the same server account, see each other. One plays at a time; the others control it.

- **Moving the music.** The output button lists this device, your other devices and this device's own
  outputs. Choosing another device moves the playback there: the queue, its order, shuffle, repeat and
  the place in the song.
- **Controlling it.** While another device plays, the player, the notification, the lock screen, the car
  and the volume keys show it and control it. The device you hold stops its own playback.
- **The terminal** lists your devices in its right panel (`C`).

## How devices reach each other

On the same network, devices talk straight to each other, found by mDNS and signed with your Subsonic
credentials. Elsewhere they go through octo-fiesta's relay on your server. Nothing goes through any other
service.

Switched on, it costs one held request a minute while music plays or another device is shown; off,
nothing at all.
