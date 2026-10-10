---
title: Architecture
description: "How nori is put together: one Rust core, thin clients, and where each decision lives."
sidebar:
  order: 1
---

Applies to the nori repository at 0.6.

nori is one Rust core with thin clients around it. Every decision lives once, in Rust, and is tested
there; the clients (Kotlin, Slint, ratatui, Swift) draw, hold the words, and wire the operating system.

```
┌──────────── clients: drawing, words, OS hooks ────────────┐
│ Android (app/, core/)  desktop  terminal  iPod (ios/)     │
├──────────── nori-host: the session every client shares ───┤
│ nori-engine: the player on its own thread, in bursts      │
├──────────── nori-core: Core per server, the Client ───────┤
│ player · library · queue · lyrics · settings · devices    │
│ covers · look · automix · transfers · db · net · model    │
└───────────────────────────────────────────────────────────┘
```

## The layers

- **The domain crates** each own one area: decoding and the sound chain (`player`), the library and its
  pages (`library`), the queue and autofill (`queue`), lyrics (`lyrics`), settings (`settings`), outputs
  and AutoEQ (`devices`), covers (`covers`), colours from covers (`look`), and so on.
- **`nori-core`** holds a `Core` per server profile and the Subsonic `Client`, and re-exports every domain
  crate, so a client can link it alone.
- **`nori-engine`** is the whole player for a platform without one: it loads in bursts, decodes, runs the
  sound chain and transitions, and sleeps in between. Android, the desktop, the terminal and the iPod all
  play through it.
- **`nori-host`** is the session the desktop, terminal and iPod share: engine, queue saving, remote
  control and jams, search, covers.

The table of every crate, with what it holds, is in [AGENTS.md](../../../AGENTS.md#layout).

## The rules that shape it

- The core returns data and enums, never text a person reads. Every word is the client's.
- Calls across the boundary are coarse: one page or one answer per call. Anything per frame or per
  buffer goes through a thin native door with no allocation.
- Every optional feature sits behind a setting, and costs nothing when it is off.
- Where a piece of work lives is decided by measurement. See [Writing a client](writing-a-client.md).

## Docs versions

The docs are published from `docs/site/` as they are on `master`, as "latest". Versioned docs can be
added later by building the site from each release tag into its own path.
