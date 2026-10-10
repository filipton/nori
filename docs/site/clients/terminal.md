---
title: Terminal
description: The terminal client, with covers drawn in the terminal.
sidebar:
  order: 3
---

Applies to nori 0.6 on Linux and macOS terminals.

The terminal client is a whole music player in a terminal, drawn with ratatui. Start it with
`cargo run --release -p nori-cli` (see [Install](../start/install.md#desktop-and-terminal)).

## Covers

Covers are drawn in whichever picture protocol the terminal answers to: kitty graphics, sixel or
iTerm2's, and in half blocks anywhere else. `I` turns them off.

Under **tmux**, a tmux that draws sixel keeps the picture in the pane. Otherwise pictures pass through to
the outer terminal, and nori draws them again when the pane has focus back; it turns tmux's
`focus-events` on for that.

## Keys

`?` shows every key. The main ones:

| Key | Does |
|---|---|
| `space` | play or pause |
| `n` / `p` | next song, previous (or the start of this one) |
| `←` `→` or `,` `.` | back or forward 5 seconds; with shift, 30 seconds |
| `+` / `-` | volume |
| `s` / `r` | shuffle on or off; repeat off, all, one |
| `/` | search |
| `1` … `7` | Home, Albums, Artists, Songs, Downloads, Equalizer, Settings |
| `tab` | sidebar, page, panel |
| `N` / `Q` / `L` / `C` | the right panel: now playing, queue, lyrics, devices |
| `F` | the player over the whole window |
| `enter` / `l` | open, or play from here |
| `a` / `A` | add to the queue, or play next |
| `x` / `X` | play the whole page, or shuffle it |
| `D` / `f` | download; favourite |
| `i` / `o` | start a jam with the selected item; join one from its link |
| `m` | mouse on or off (off: the terminal selects text) |
| `q` | quit |

## Cost

Idle or paused, the client does not wake at all. Playing, its screen wakes about once a second for the
clock, and a few times a second with word-by-word lyrics on screen.
