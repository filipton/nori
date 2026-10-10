---
title: Desktop
description: The desktop client for Linux and macOS.
sidebar:
  order: 2
---

Applies to nori 0.6 on Linux and macOS.

The desktop client is a window over the same core, drawn with Slint. It plays through the system's sound
(PipeWire or ALSA on Linux, CoreAudio on macOS), shows its controls to the desktop (MPRIS on Linux), and
has the library, search, the queue, lyrics, the equalizer, settings, [remote control](../connected/remote-control.md)
and [jams](../connected/jams.md).

## Install

Build it from source: see [Install](../start/install.md#desktop-and-terminal). It shares its data
directory with the terminal client.

## Limits

- Signing in takes an address, user and password or an API key, and a second address; extra HTTP headers
  and self-signed certificates are Android's only for now.
- HE-AAC radio stations play without their highest frequencies (above 11 to 12 kHz), since no free
  decoder for that part is linked.
