---
title: AutoEQ
description: Correction curves for 8,850 headphones, kept on the device and offered when they connect.
sidebar:
  order: 2
---

Applies to nori 0.6 on every client.

AutoEQ is an open project that measures headphones and publishes a correction curve for each. nori keeps
its list of 8,850 headphones on the device, so you can search it and apply a curve offline.

- **Keep the AutoEQ list** (on by default): downloads the headphone list (850 kB, from github.com) on
  Wi-Fi, and again once a month. It sits under **Look things up online**; see
  [Privacy and network](../privacy.md).
- **Picking a curve**: search the list by name and apply it to the sound of the output you are on. The
  curve itself is fetched once, when you choose it.
- **When headphones connect**: if a Bluetooth or USB device's name matches a measured headphone, nori
  offers its curve and remembers your answer for that device. **Apply AutoEQ automatically** (off
  by default) applies it as soon as they connect, instead of asking.

A curve becomes ten filters of the parametric equalizer, so it costs no more than any preset.
