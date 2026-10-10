# Battery shootout: nori vs Symfonium vs musly vs Navic

Historical: measured on nori 0.3.x, being re-measured. The summary is [the performance page](../../site/performance.md).

Same emulator (`sdk_gphone64_x86_64`, Android 14), same local Navidrome server,
same tracks, media volume 0, screen off, no touches. nori is the **release**
build throughout; the other three are Play releases. Full per-thread outputs
and profiles: [perf-shootout.md](perf-shootout.md).

| Metric | **nori 0.3.1** | Symfonium 15.0.1 | musly 2.0.2 | Navic alpha55 |
|---|---|---|---|---|
| Cold start | **~590 ms** | ~900 ms | ~1050 ms | ~700 ms |
| MP3 CPU | **1.26%** | 10.7% | 4.01% | 3.10% |
| MP3 wakeups | **372/s** | 2050/s | 569/s | 465/s |
| MP3 quiet | **75/90** | 1/90 | 0/90 | 1/90 |
| MP3 memory | **119 MB** | 124 MB | 142 MB | 124 MB |
| FLAC CPU | **1.30%** | 8.74% | 4.70% | 3.72% |
| FLAC wakeups | **469/s** | 1938/s | 611/s | 589/s |
| FLAC quiet | **69/90** | 0/90 | 0/90 | 1/90 |
| FLAC memory | **126 MB** | 118 MB | 148 MB | 118 MB |
| EQ CPU | **1.35%** | 13.4% | — | 2.92% |
| EQ quiet | **73/90** | 1/90 | 0/90 | 1/90 |
| EQ memory | **104 MB** | 119 MB | 142 MB | 119 MB |
| Mix CPU | **0.37%** | 1.83% | 1.22% | 0.60% |
| Mix quiet | **159/200** | 117/200 | 3/200 | 108/200 |
| Mix memory | **103 MB** | 124 MB | 135 MB | 133 MB |
| Paused CPU | 0.01% | **0.00%** | 0.10% | **0.00%** |
| Paused quiet | **89/90** | 29/30 | 0/30 | 29/30 |
| Paused memory | **97 MB** | 105 MB | 150 MB | 127 MB |
| Wakelock while paused | none | none | held | none |

**Bold** = best in row. Playback rows are 90 s windows over "Noise 1" (MP3 320)
and "Noise flac" from the dev library; EQ rows add a treble bump per app;
mix rows are 200 s over four ~45 s tracks with three transitions;
paused rows are settled background windows.

Notes:

- MP3, FLAC, EQ and cold-start rows re-measured 2026-09-22 on release **0.3.0** (unchanged for 0.3.1 UX)
  (EQ off + offload on for the stock rows; nori EQ at 8 kHz **+8.4 dB**).
  Mix and paused rows are still from 2026-09-21.
- Symfonium is hotter on this install than on 2026-09-21 (MP3 was 6.66% then,
  ~10–13% now) with the same thread cast — not a measurement glitch. Its EQ
  row is Noise 1 as-found (Graphic EQ left enabled from the earlier setup);
  historically its EQ cost was ~0 pp on top of stock.
- musly has no equalizer; the EQ column is its MP3 stock figure. Feature gap,
  not a win.
- Navic has no transition feature; its 0.60% mix row is stock gapless play.
- Symfonium skips crossfade on sequential albums, so its mix row uses a manual
  queue.
- Paused windows: nori 90 s, the rest 30 s — quiet counts are not directly
  comparable, CPU and memory are.
- musly holds an `AudioService` wakelock while paused and never lets the
  device sleep, in any state.
- FLAC-vs-MP3 scaling (this run): nori +3%, Symfonium −18% vs its hot MP3
  mean (noise), musly +17%, Navic +20%.
- Cold starts are medians of three `am start -W` TotalTime readings after
  force-stop; the emulator was warmer than on 2026-09-21 (everyone slower,
  ranking unchanged: nori still first).
- An emulator has no real battery and software-decodes; absolute numbers do
  not transfer to a phone, relative rankings do.
