---
title: iPod touch
description: nori on a jailbroken iPod touch 6th generation, on iOS 12.
sidebar:
  order: 4
---

Applies to nori 0.6 on the iPod touch 6th generation, iOS 12.2 or later, jailbroken.

The iPod client is a UIKit app over the same core: Home, Library, Search and Settings tabs, the player
and mini player, the queue, lyrics, the equalizer and per-device sound, downloads,
[remote control](../connected/remote-control.md) and [jams](../connected/jams.md), and the lock screen's
controls. It has a dark theme by default, and a light one.

## What you need

- An iPod touch 6th generation (`iPod7,1`) on iOS 12.2 or later (12.5.8 is the last).
- A jailbreak, such as checkra1n, with **AppSync Unified** installed, since the app is fake-signed.

## Install

The release carries `nori-ipod-<version>.ipa`, fake-signed for AppSync Unified: install it with a
package manager or file manager on the iPod that installs IPAs.

From the source, with Docker on Linux or a Mac and the iPod on USB:

```sh
tools/ipod.sh          # builds the app in Docker, then installs and runs it on the iPod
```

It needs `sshpass` and libimobiledevice's `iproxy`, and reaches the iPod over USB with checkra1n's
default password unless `NORI_IPOD_PASSWORD` says otherwise. See
[iPod internals](../developers/ipod-internals.md) for the build.

## Limits

- The headphone jack, the speaker and Bluetooth are the outputs; there are no USB DAC pages.
- Downloads run while the app is open or playing.
- Signing in takes an address, user and password or an API key, and a second address; extra HTTP headers
  and self-signed certificates are not applied yet.
- After a reboot without a computer, checkra1n's jailbreak is gone and the app will not open until the
  device is jailbroken again.

## Battery

The app is built to sleep between bursts of music, as on Android. A 30-minute battery run against
Apple's Music app on the same songs (`tools/ipod-bench.sh`) is the measure; results will be on the
[performance page](../performance.md) once they are in.
