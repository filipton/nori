---
title: Install
description: Getting nori onto an Android phone, a desktop, a terminal or an iPod touch.
sidebar:
  order: 2
---

Applies to nori 0.6. Releases are on [GitHub](https://github.com/norifm/nori/releases).

## Android

Android 8.0 or later. Download `nori-music-<version>.apk` from the latest release and open it on the
phone; Android asks once to allow installing from the app you opened it with. The app checks GitHub for
newer releases at most once a day, unless you turn off Check automatically in the Updates section of
Settings.

**Android Auto** hides apps that were not installed from the Play Store. To see nori in the car, open
the Android Auto settings on the phone, tap Version ten times to unlock developer settings, then in the ⋮
menu open Developer settings and turn on Unknown sources.

## Desktop and terminal

The desktop and terminal clients are built from source for now. With [Rust](https://rustup.rs) installed:

```sh
git clone https://github.com/norifm/nori
cd nori
cargo run --release -p nori-desktop     # the desktop client
cargo run --release -p nori-cli         # the terminal client
```

Both keep their data in `$XDG_DATA_HOME/nori` (or `~/.local/share/nori`) and share it, so a server added
in one is there in the other.

## iPod touch

The iPod touch 6th generation, on iOS 12.2 or later, jailbroken. The release carries
`nori-ipod-<version>.ipa`, fake-signed for AppSync Unified. See [iPod touch](../clients/ipod.md) for the
requirements and how to install it.
