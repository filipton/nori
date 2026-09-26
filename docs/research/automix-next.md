# AutoMix, the next step: iOS 27 smoothness and the plan to get there

This follows `automix.md`, which covers the earlier research and the design as built, and `analysis.md`, which covers the beat, key and structure analysis. Read those first. This file records what changed in Apple's AutoMix with iOS 27 (2026), what our own logs show about why nori's mixes rarely sound like it, and the build plan in order.

Labels: **[verified]** is stated by a cited source; **[inferred]** is our reasoning; **[measured]** comes from nori's logs or tests.

## 1. What iOS 27 AutoMix changed

- **It re-cuts intros and outros so the tempos line up.** It goes beyond speeding one song up: Apple "remix[es] the outro and intro of songs to perfectly align the tempo of the transitions" **[verified]** ([9to5Mac](https://9to5mac.com/2026/07/16/ios-27-makes-one-of-my-favorite-apple-music-features-even-better/)).
- **It repeats sections to bridge songs.** It will "repeat parts of the outros and intros to bridge the transition", and it extends sections of tracks to make smoother blends **[verified]** ([9to5Mac](https://9to5mac.com/2026/07/16/ios-27-makes-one-of-my-favorite-apple-music-features-even-better/), [RouteNote](https://routenote.com/blog/apple-music-automix-upgrade/), [MacRumors](https://www.macrumors.com/2026/06/09/apple-music-gains-automix-upgrades-and-more-in-ios-27/)). **[inferred]** These are bar-aligned loops on a beat grid, used when an intro or outro is shorter than the overlap the planner wants.
- **The "underwater" sound is mostly gone.** The iOS 26 sound "has largely been replaced by more natural transitions" **[verified]** (9to5Mac). **[inferred]** Heavy low-pass sweeps on the outgoing song gave way to natural blends: EQ/band crossfades and level.
- **Transitions are more varied, matched to each pair's energy and tempo,** rather than one repeated effect **[verified]** (RouteNote).
- **The basis is the same as before:** time-stretching with pitch kept, plus beat matching **[verified]** ([MusicTech](https://musictech.com/news/gear/apple-music-automix-ai/), [Apple Newsroom](https://www.apple.com/newsroom/2025/06/apple-services-deliver-powerful-features-and-intelligent-updates-to-users-this-fall/)).
- **It now also runs on HomePod and Apple TV** **[verified]** (MacRumors).
- **Not published:** transition lengths, how loops are chosen, vocal handling, EQ details. Treat everything beyond the list above as ours to design.

## 2. Where nori stands (as of 2026-09-26)

What the planner (`crates/player/src/automix/plan.rs`) can already do:
- **Transition kinds** (`TransitionKind`, crates/model and crates/player types):
  - Gapless;
  - EqualPowerFade: nothing is known about either song;
  - MixRampFade: overlap from loudness ramps and trimmed silence, with an optional filter sweep;
  - BeatMatched: tempo-locked and bar-aligned, with an optional bass swap;
  - EchoOut: the outgoing song exits into a beat-synced echo.
- **Tempo:** stretch with pitch kept (`stretch.rs`, nori-player's Sonic), one fixed ratio per transition.
- **Key:** Camelot distance chooses a soft or classic filter shape (`apply_fade_filter`).
- **Structure:** phrase lines and downbeats (`structure.rs`), "on a phrase line" and "4-bar run-up" choices.
- **Loudness:** LUFS matching with a gentle release.
- **Analysis:** `analysis.rs`, `beats.rs`, `tempo.rs`; the optional neural beat tracker `neural.rs` ("Better beat detection", Beat This! small0, weights fetched from the authors on first use); measured as the bytes arrive (`crates/engine/src/arriving.rs`).

**The main gap [measured]: the beat-matched path almost never runs on real music.**
- In the user's perf reports from the S22 (a 2 h battery run on 2026-09-25 and perf1.txt on 2026-09-26), most transitions were `MIX_RAMP_FADE` with the reason `no reliable beat grid`.
- Example: Pennyroyal Tea read as 84 BPM, conf 0.24, stab **0.00**. The incoming 168 BPM song had conf 0.80, stab 0.92.
- The gate is `plan.rs`: `MIN_BPM_CONFIDENCE = 0.5`, `MIN_STABILITY = 0.6` (lines about 29–30 and 104). The cue picker in `automix/mod.rs` has its own gate: `CUE_MIN_CONFIDENCE = 0.4`, `CUE_MIN_STABILITY = 0.5`.
- A steady rock song with stability 0.00 points to the measurement, not the music. Possible causes:
  - too little audio analysed at the ends;
  - the stability metric's definition;
  - octave errors (84 vs 168);
  - windows that include intros or outros with no drums.
- Tempo ratios in the logs were almost always `x1.000`.

So the beat-matched and effect paths exist but are starved of reliable grids. Fix that first; everything below depends on it.

## 3. Build plan, in order

Each step needs Rust tests on the virtual clock and the synthetic set (`automix/synth.rs`, `automix/eval.rs`; see docs/testing.md), plus a listening check on the phone.

### Step 0: a way to listen fast (tooling)
- A debug or perf "mix preview": for the current queue, play only the last ~20 s of song N into the first ~20 s of N+1, then jump to the next pair. This lets a person judge many transitions in minutes.
- A perf-report section per transition: kind, length, reason, both grids (bpm/conf/stab), key distance, and whether loops or tempo ramps were used. The logs have most of this; make it a table.
- A library survey command (CLI or perf build): the share of songs with a reliable grid, the distribution of conf/stab, and octave disagreements, classical tracker vs neural.

### Step 1: reliable beat grids (the blocker)
- Find why steady songs get stab 0.00. Check:
  - how many seconds each end is analysed with;
  - how stability is computed;
  - octave handling (84/168);
  - whether drumless intros and outros poison the window.
  Fix the measurement.
- Compare against "Better beat detection" (neural, `neural.rs`) on the user's real songs. Decide whether it should be the default when the device is fast enough. Measure its time per song on the S22 through the perf report.
- Octave: allow 2x and ½ matches when checking tempo compatibility (84 ↔ 168 is a perfect match).
- Target: most songs of a normal rock/pop library get a grid that passes the gate, without loosening the gate blindly.

### Step 2: bar-aligned loops (Apple's main iOS 27 trick)
- When the outgoing outro or incoming intro is shorter than the planned overlap, loop 1–2 bars (on downbeats, on the grid) to extend it so a full phrase overlaps: 8 or 16 bars when the tempo allows.
- Loop points must be click-free: zero-crossing or a short crossfade at each seam, and matched loudness.
- Never loop vocals: use the vocal detection (step 5) to pick instrumental bars.
- The planner decides: loop or not, which bars, how many repeats. The mixer (`mixer.rs`) plays it. The old unused `in_loop_ms` field was removed in the 2026-09-26 cleanup; design this fresh.
- **No time-skip problems:** Apple drew complaints for skipping "as much as a full minute" (see `automix.md` §1). Keep what's skipped short and never cut a song's final chord.

### Step 3: tempo ramps instead of one fixed ratio
- During the overlap, glide the outgoing tempo to the incoming one. After the handover, ease the incoming song back to its native tempo over a few bars.
- Limits: about ±6–8% (the current max-tempo setting), with pitch kept.
- The stretch must handle a changing ratio without artifacts; check `stretch.rs`'s ratio updates per block.

### Step 4: natural blends instead of heavy filter sweeps
- Replace the default low-pass sweep on fades with a three-band crossfade: the bass swap (already there), plus a gentle mid dip on the outgoing song while both are busy, plus highs crossfaded.
- Keep sweeps for EchoOut and for clashing keys only, and make them subtle.

### Step 5: vocal-aware overlaps
- Detect vocal activity near both ends. `structure.rs`/`analysis.rs` may have a start; the synthetic set has sung lines.
- Pick overlap windows where at most one song has vocals. Otherwise end the outgoing vocal before the incoming vocal starts, or use EchoOut.

### Step 6: variety from energy, key and structure
- Choose the kind and length per pair:
  - energy (loudness and onset density) rising or falling;
  - key distance (a compatible key allows a long blend, a clash calls for a short one or EchoOut);
  - phrase positions;
  - tempo gap.
- Avoid the same effect on every transition.

## 4. Open research questions (for later)
- How DJ software picks loop lengths and mix-in points: Rekordbox, Traktor, Mixxx (open source: its AutoDJ and beat grid code are worth reading), and djay's Automix AI.
- Open papers on automatic DJ mixing and transition generation. Look for "automatic DJ mix generation", "DJ transition", and the Beat This! authors' related work.
- Phone-cheap vocal activity detection (spectral-flux or small-model options) and its cost on the S22.
- Loudness and energy curves: what makes a transition feel smooth, as opposed to merely aligned.
- Whether a server-side pass (the server has the files) could precompute grids for the whole library. This fits the "Rust backend" architecture if the server ever runs nori code; today it's Navidrome.

## 5. Sources
- [MacRumors: Apple Music Gains AutoMix Upgrades and More in iOS 27](https://www.macrumors.com/2026/06/09/apple-music-gains-automix-upgrades-and-more-in-ios-27/)
- [9to5Mac: iOS 27 makes one of my favorite Apple Music features even better](https://9to5mac.com/2026/07/16/ios-27-makes-one-of-my-favorite-apple-music-features-even-better/)
- [9to5Mac: iOS 27, all the new Apple Music features](https://9to5mac.com/2026/06/10/ios-27-heres-all-the-new-apple-music-features/)
- [RouteNote: Apple Music makes AutoMix even smarter](https://routenote.com/blog/apple-music-automix-upgrade/)
- [RouteNote Radar: AutoMix gets a major upgrade in iOS 27](https://routenote.com/radar/apple-music-automix-gets-a-major-upgrade-in-ios-27/)
- [MusicTech: AutoMix uses AI to time-stretch and beat-match](https://musictech.com/news/gear/apple-music-automix-ai/)
- [Apple Newsroom, June 2025 (AutoMix introduced)](https://www.apple.com/newsroom/2025/06/apple-services-deliver-powerful-features-and-intelligent-updates-to-users-this-fall/)
- [MacRumors: enabling AutoMix in iOS 26](https://www.macrumors.com/how-to/ios-enable-automix-feature-apple-music/)
- Earlier sources and complaints: `automix.md` §1.
