---
title: Troubleshooting
description: Fixes for the problems people meet most.
sidebar:
  order: 2
---

Applies to nori 0.6.

## Signing in

| When nori says | What to do |
|---|---|
| That address answered, but not like a Subsonic server | Check the URL, including any path your reverse proxy adds before `/rest`. |
| Cleartext HTTP was refused | Use `https://`. |
| This server is set to Wi-Fi only | Connect to Wi-Fi, or turn off Wi-Fi only for the server. |
| The server did not answer | Check that the server runs and is reachable from this device. A second address helps away from home. |

Behind Cloudflare Access or basic auth, add the headers under Advanced on Android. The desktop, terminal
and iPod do not send extra headers yet.

## nori is not in Android Auto

Android Auto hides apps not installed from the Play Store. Turn on Unknown sources in Android Auto's
developer settings: see [Install](../start/install.md#android).

## No lyrics

- Check that **Look things up online** and **Find missing lyrics online** are on.
- Provider songs from octo-fiesta are never looked up.
- A song nobody had is asked again after a week. Settings, Storage, Lyrics clears what is kept.

## AutoMix does not mix

A song has to be measured before it can be mixed, which happens as it streams or downloads; albums in
order stay gapless with **Keep albums gapless** on. See
[When AutoMix does not mix](../sound/automix.md#when-automix-does-not-mix).

## Battery use is higher than expected

The equalizer, effects, speed, skipping silence, AutoMix and crossfade all need the CPU to process the
music, so the audio chip cannot play it on its own. Settings shows when offload is paused and why. See
[Offload and processing](../sound/offload.md).

## The iPod app will not open

After a reboot without a computer, checkra1n's jailbreak is gone and fake-signed apps do not launch.
Jailbreak the device again.
