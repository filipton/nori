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
- **Tempo:** stretch with pitch kept (`stretch.rs`; Signalsmith Stretch, MIT, through the `signalsmith-stretch` crate, with varispeed as the other engine; an earlier version of this line said Sonic, which was out of date). Only the incoming song is stretched: it holds a ratio for the overlap, then ramps back to native tempo over a few bars. Users report Apple moves the outgoing side too (`automix.md` §1a).
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
- Second pass, 2026-09-29 (see `automix.md` §1a): [Mac guide](https://support.apple.com/guide/music/muse5e9ec085/mac), [9to5Mac 2025-06-16](https://9to5mac.com/2025/06/16/automix-apple-music-ios-26/), [Yahoo: tried Apple Music's DJ feature](https://tech.yahoo.com/audio/articles/tried-apple-musics-dj-feature-153000036.html), [Yahoo/TechRadar: users loving AutoMix](https://tech.yahoo.com/audio/articles/apple-music-users-loving-automix-140000012.html), [James Cridland's examples](https://james.cridland.net/blog/2025/apple-music-auto-mix-examples/), [AppleInsider](https://appleinsider.com/articles/25/06/10/apples-automix-in-macos-26-isnt-a-house-dj-but-is-a-good-fm-radio-simulator), [How-To Geek](https://www.howtogeek.com/i-disabled-apple-music-automix/), [BGR](https://www.bgr.com/2059450/how-to-turn-off-automix-apple-music-worst-feature/), [Apple Community thread](https://discussions.apple.com/thread/256143899), [mystats.music](https://mystats.music/blog/apple-music-automix-2026) (weak), [digdis.de](https://digdis.de/en/blog/post/apple-music-automix-smooth-dj-transitions-directly-in-the-app) (weak), [AppleMagazine](https://applemagazine.com/everyones-going-crazy-about-apple-automix-heres-why/) and [TechPulse](https://techpulse.press/faq/ios-27-new-apple-music-features/) (the unsourced on-device claim), [US 7,518,053](https://patents.google.com/patent/US7518053B1/en) (prior art, not Apple).
- Third pass, iOS 27 focus (2026-09-29, see `automix.md` §1a): [MacRumors tvOS 27 release notes](https://www.macrumors.com/2026/09/25/tvos-27-release-notes/), [MacRumors HomePod software 27](https://www.macrumors.com/2026/09/14/apple-releases-homepod-software-27/), [Apple Community iOS 27 beta thread](https://discussions.apple.com/thread/256311704), [Is iOS Stable on iOS 27 AutoMix](https://x.com/isiosstable/status/2079892915929649661), [Max Weinbach on X](https://x.com/mweinbach/status/2068158525524570417) (unread), [TechRadar hands-on](https://www.techradar.com/audio/apple-music/automix-is-the-apple-music-feature-that-made-me-love-listening-to-music-on-my-iphone-again), [MacRumors forum threads](https://forums.macrumors.com/threads/automix.2458553/), [Apple ML downbeat tracking](https://machinelearning.apple.com/research/downbeat-tracking-with-tempo), [US 8,553,504](https://patents.google.com/patent/US8553504B2/en), [exploringmusickit crossfade](https://exploringmusickit.com/musickit-crossfade).
- [Ben Aqua: AutoMix in iOS 26 is (kinda) magical](https://www.youtube.com/watch?v=7IbPywte4Ko) (a DJ naming high-pass and low-pass filters and a tempo ramp), plus unread: [Pulse Tech R](https://www.youtube.com/watch?v=vN1UzY5oPBQ), [SoundGuys vs Spotify Mix](https://www.youtube.com/watch?v=0fT0jgQItp4), [bbdtv](https://www.youtube.com/watch?v=s9kaRYzPhiM), [ViWizard iOS 27](https://www.youtube.com/watch?v=YjuuLL8AGpY).
- Reddit, read from the owner's printed PDFs (2026-09-29): [iOS 27 DB1 AutoMix repeats beats (r/iOSBeta)](https://www.reddit.com/r/iOSBeta/comments/1u0qk7c/ios_27_db1_automix_updated_to_repeat_beats/), [same, r/AppleMusic](https://www.reddit.com/r/AppleMusic/comments/1u0qfog/ios_27_updated_automix_to_repeat_beats/), [New AutoMix transition](https://www.reddit.com/r/AppleMusic/comments/1u1zcn0/ios_27_new_automix_transition/), [AutoMix iOS 27?](https://www.reddit.com/r/AppleMusic/comments/1wkm7d1/automix_ios_27/), [iOS 27 redeems AutoMix](https://www.reddit.com/r/AppleMusic/comments/1ue3oyp/ios_27_redeems_automix/), [AutoMix is so back](https://www.reddit.com/r/AppleMusic/comments/1wc9phz/automix_is_so_back/), [ios 27 automix is underwhelming](https://www.reddit.com/r/AppleMusic/comments/1wh6el1/ios_27_automix_is_underwhelming/), [If your AutoMix doesn't work after iOS 27 beta](https://www.reddit.com/r/AppleMusic/comments/1u3h2h7/if_your_automix_doesnt_work_after_ios_27_beta/), [I noticed this with AutoMix in iOS 27](https://www.reddit.com/r/AppleMusic/comments/1vfhju9/i_noticed_this_with_automix_in_ios_27/), [Auto mix improvement](https://www.reddit.com/r/AppleMusic/comments/1w5euk0/auto_mix_improvement/), [best AutoMix transition on iOS 27](https://www.reddit.com/r/AppleMusic/comments/1wpemls/i_present_you_the_best_automix_transition_on_ios/), [drop your best AutoMix transitions](https://www.reddit.com/r/AppleMusic/comments/1vlep3g/ios_27_users_drop_your_best_automix_transitions/), [What's up with AutoMix iOS 27?](https://www.reddit.com/r/AppleMusic/comments/1wl9d38/whats_up_with_automix_ios_27/), [IOS 27 AUTOMIX](https://www.reddit.com/r/AppleMusic/comments/1u0kcuc/ios_27_automix/), [RC tips bug (r/iOSBeta)](https://www.reddit.com/r/iOSBeta/comments/1wd0za6/ios_27_rc_apple_music_shows_tips_for_automix/), [r/Beatmatch: app that auto mixes tracks](https://www.reddit.com/r/Beatmatch/comments/1nocxhy/app_that_auto_mixes_tracks_adjusted_bpms_etc/).

## 6. Beyond iOS 27: the heavy-compute direction (2026-09-29)

The owner lifted the battery limit for AutoMix: heavy is fine if it sounds excellent. Four research passes (ML for mixing, bigger analysis models, stem separation on phones, the effects palette) were read and merged here. Labels: **[verified]** read on a page (URL given); **[estimate]** our reasoning; **[unknown]** not found. Two caveats up front. The effects recipes and rules of thumb below come from DJ practice as the researcher knew it, not from pages read (marked). No phone speed for any separation model was found anywhere.

### 6.1 What the field looks like

- **Nothing published is an end-to-end neural AutoMix.** The closest commercial thing is djay's Neural Mix: real-time stem separation on the device (Core ML on the Neural Engine, AudioShake's model, quality tiers of 100, 80 and 70 % by chip, 64-bit Android supported) with named transitions (Dissolve, Riser, Echo) **[verified]** ([AudioShake](https://www.audioshake.ai/post/algoriddim-djaypro-neural-mix), [djay help](https://help.algoriddim.com/topic/using-djay/neuralmix-compatibility)). Apple's iOS 27 is not known to use stems; its new mechanics are loop and repeat **[verified]**, see `automix.md` §1a.
- **The published ML is mostly a controller, not a sound generator.** DJtransGAN learns EQ and fader curves from real DJ mixes with a differentiable fader and EQ (MIT code, weights licence unstated, EDM-only training data that cannot be shared) **[verified]** ([GitHub](https://github.com/ChenPaulYu/DJtransGAN)); Diff-MST predicts a differentiable mixing console's parameters ([arXiv 2407.08889](https://arxiv.org/abs/2407.08889)); a 2024 detection-transformer finds DJ cue points (21k expert cues on 4.7k tracks, code and weights said to be public, licence unstated) ([arXiv 2407.06823](https://arxiv.org/abs/2407.06823)); Raveform gives beats, downbeats and intro, buildup, drop, breakdown and outro labels for 1,423 EDM tracks (CC BY 4.0) ([TISMIR](https://transactions.ismir.net/articles/10.5334/tismir.288)). All EDM-heavy; pop and rock are **[unknown]**.
- **Generative bridges are not usable.** MusicGen's weights are CC BY-NC; Stable Audio Open is under the Stability community licence, 1B parameters, made for GPUs **[verified]**. Generated audio would not match the user's real songs, and vocals are unrealistic. Teacher or research only.
- **Strong music embeddings are non-commercial** (MERT and MuQ weights CC BY-NC, Essentia models CC BY-NC-SA). OpenL3 (MIT code, CC BY 4.0 weights) is the only permissive one confirmed. No paper shows an embedding predicts transition quality **[verified absent]**.

### 6.2 Direction, in order of audible gain per effort

1. **Grids first, with the big model (low effort).** Beat This! `final0` is MIT, about 78 MB (an 82 MB community ONNX exists at [huggingface.co/aaatmy/beat-this-onnx](https://huggingface.co/aaatmy/beat-this-onnx), its input is 22.05 kHz log-mel) **[verified]**. `analysis.md` measured that the small model agrees with the full one only at F 0.867 on real clips (0.977 on synthetic), and 7.2 s per window here for the full one, so the full model is now affordable and likely worth it **[measured]**. Then add tempo octave correction, time-signature and phrase length from downbeat spacing, and per-bar energy curves (bass energy, flux, kick density, loudness per bar) for in and out points; no model found for drop detection, so keep heuristics on energy curves **[verified absent]**.
2. **Render the transition ahead, several ways, and pick the best (medium effort, no new model).** While song A plays, render 3 to 8 candidate transitions (preset, start, length, pitch or tempo option) offline, score each (loudness continuity across the seam, true-peak, bass-band and vocal-band overlap, vocal overlap, grid error), play the winner, and choose among near-equals for variety. Cache per pair. This uses compute the owner now allows and makes every other effect choose musically **[estimate; the scoring metrics are the researcher's proposal]**.
3. **A wider palette of effects (medium effort each, recipes from DJ practice, not read on a page).** Ranked, smallest implementation each:
   - reverb-tail hard cut (a small reverb on the last beat, dry cut on the downbeat; ports of Freeverb or Dattorro are about 100 lines; Rust crates `synfx-dsp`, `freeverb`, `fundsp` exist, licences unchecked);
   - reverse reverb or reverse cymbal riser (needs the lookahead render above, which nori has);
   - noise riser and sweep (filtered noise, rising cutoff and gain over 2 to 8 bars);
   - beat-repeat roll (1/4, 1/8, 1/16 slices, an extension of the existing bar loop; instrumental only);
   - tape stop or brake (a playback-rate ramp to zero over 0.4 to 1 s, for hard endings or big tempo gaps);
   - quick cut on the one with a 10 ms micro-fade (rock, hip-hop, big gaps);
   - phaser or flanger on the outgoing tail (house-style blends);
   - later: half-time or double-time for near 2:1 tempos, a plus-or-minus 1 to 2 semitone key shift with Signalsmith, sidechain duck.
   Products show the shape of a good palette: djay (Automatic, Fade, Filter, EQ, Echo, Dissolve, Riser, Neural Mix), Spotify Mix (Fade, Rise, Blend, Wave, Melt, Slam, editable curves) **[verified]** ([MusicTech](https://musictech.com/news/gear/algoriddim-free-dj-software-djay-pro-ai-automix-and-neural-mix/), [Spotify](https://newsroom.spotify.com/2025-08-19/mix-your-favorite-playlists-seamlessly-by-adding-your-own-transitions/), [DJ.Studio](https://dj.studio/blog/basic-transition-techniques)). Avoid Mixxx (GPL) and Rubber Band (GPL) code.
4. **A precompute path on the server or desktop (medium effort, unlocks the heavy models).** A sidecar next to Navidrome that writes one analysis JSON per song and, optionally, the stems of the transition windows. OpenSubsonic has no field for arbitrary analysis data (Sonic similarity and Transcoding are extensions, not this) and Navidrome's custom tags may not reach the API; the simplest route is a sidecar HTTP service keyed by song id, with the phone falling back to its own analysis **[verified in part]** ([extensions](https://opensubsonic.netlify.app/docs/extensions/), [Navidrome custom tags](https://www.navidrome.org/docs/usage/configuration/custom-tags/)). Heavy pieces that only fit here: All-In-One structure (MIT code, weights licence unstated, needs Demucs stems, 10 songs in 73 s on an RTX 4090) **[verified]**, the 3-model `final` ensemble, and stems.
5. **Stem-aware transitions (large effort, biggest new capability).** Vocal duck by stem instead of a band-pass, drum-first swap, acapella over instrumental, per-stem echo-out, and exact vocal activity from the vocal stem. Only about 90 s of audio per transition needs separating (45 s of each song), ahead of time. What is known: htdemucs is MIT code (repo archived 2025-01-01, forked at adefossez/demucs), 9.0 dB SDR, about 1.5x track length on a GPU, about 0.2 real-time factor on an M4 Pro CPU (community ONNX export, four-stem bag 316 MB) **[verified]** ([Demucs](https://github.com/facebookresearch/demucs), [ONNX export](https://huggingface.co/StemSplitio/htdemucs-ft-onnx)). **Phone speed is [unknown]:** no Demucs or RoFormer figure on a Snapdragon, Exynos or Tensor was found. A Mel-Band RoFormer vocals model took 40.9 s per pass (about 11 s of audio) on an Adreno 660 through WebGPU, about 0.27x real time **[verified]**, so RoFormer is server-only. The best size-to-speed candidate is SCNet (about 10M parameters, 9.0 dB, 22.6 MB fp16 ONNX, MIT code, 2.83x real time on a desktop browser with 16 WASM threads) **[verified]** ([scnet-web-wasm](https://github.com/elicwhite/scnet-web-wasm)); its phone speed is **[estimate]**. Mix the stems back only inside the overlap, keep the residual (mix minus stems) as a bus, and crossfade to and from the original, because stem sums have bleed (Mixxx's port measured about 7.4 dB SI-SDR) **[estimate]** ([Mixxx](https://mixxx.org/news/2025-10-27-gsoc2025-demucs-to-onnx-dhunstack/)).
6. **Learned controller (later).** Distil real DJ-mix statistics (fade, EQ and cue parameters mined by aligning mixes to tracks, as in Kim et al., ISMIR 2020, [arXiv 2008.10267](https://arxiv.org/abs/2008.10267)) into a small model that outputs EQ, fader and filter curves, applied by the normal Rust chain. Data is EDM-heavy and scraped (redistribution unclear); UnmixDB gives synthetic ground truth. Ship only after items 1 to 3 give a benchmark to beat.
7. **Skip for now:** generative bridges, on-phone stem separation before a speed test, drop-detection models (none exist usably), embedding-based "what follows what" (queue order is the user's).

### 6.3 The smallest experiments that decide things

- **Phone stem speed:** a day's harness on the S22 (both Snapdragon and Exynos if possible): htdemucs ONNX and SCNet fp16 on ONNX Runtime Mobile (CPU with XNNPACK first, NNAPI only for comparison), one 45 s excerpt, three runs back to back to catch throttling, log wall time, real-time factor, peak memory and temperature. Under 1.0 means on-device background separation works; 1 to 3 means server or desktop stems cached; above 3 means server only. Then listen to a 30 s A/B (crossfade against stem-aware) with stems used only in the overlap.
- **Grid gain:** run `final0` against `small` on the owner's own library and compare against the classical grid, scored on how many transitions leave the "no reliable beat grid" fallback. This needs the real-music benchmark from `automix.md` §1a (record Apple's mixes of the same pairs through AirPlay to a Mac with Audio Hijack).

### 6.4 Licences that must be settled before any of this ships

- **Demucs weights:** the primary page states only the code licence (MIT); a third-party export says "All MIT-licensed"; `analysis.md` earlier recorded the weights as scientific-use-only. These disagree, so read the upstream terms.
- **MUSDB18-HQ is academic use only** **[verified]** ([sigsep](https://sigsep.github.io/datasets/musdb.html)); checkpoints trained on it may inherit that. No specific checkpoint was verified.
- **All-In-One weights, DJtransGAN weights and the cue-point model:** licence unstated. Ask the authors.
- **madmom and the Essentia models are non-commercial**; Open-Unmix `umxl` is CC BY-NC-SA. `demucs-android` is GPL-3.0, so read it, do not copy it.
- Whether nori is ever commercial decides how strict this is; today it is a personal client.

## 7. Plan and owner decisions (2026-09-29): nothing is being built yet

This section records what the owner decided in conversation so it is not lost. **No code is being written now.** The research above is saved for the day the work starts; more research is expected during the work.

### 7.1 Decisions

- **Battery does not matter for AutoMix** if the result sounds excellent. Heavy analysis, lookahead rendering and a charging-only pass are all acceptable.
- **Pick the best model per task, not a mediocre one.** nori is an MIT-licensed, open-source, non-commercial project (`LICENSE`, `README.md`), so non-commercial model weights are acceptable in principle. Keep the existing pattern of the device fetching weights from the model authors (as `beat_download.rs` does for Beat This!) so the repo and APK never redistribute them.
- **Order of work.** First improve the whole code side of AutoMix using the research (grids, the transition chooser, effects, structure), researching further as each piece is built. Later, choose which models run on the phone and which on a server or desktop.
- **Save everything in this research file** (`automix.md` §1a for what Apple does, this file §6 and §7 for the direction).

### 7.2 A quality ladder in settings instead of stacked switches (proposal, not decided)

- **Standard:** the classical analysis, on the ends of each song. Cheap; what runs today.
- **Better:** the existing "Better beat detection" (Beat This! small, a 5 MB download).
- **Best:** everything heavy, in stages under one name: the full `final0` beat model (about 78 MB, MIT), possibly the three-checkpoint average; tempo-octave correction and per-bar energy; the ahead-of-time transition chooser with the heavier candidates; and later stems ("Studio"). Runs while charging on Wi-Fi.
- Each step shows its download size and time per song. A song without stems still gets the better matching and chooser, so the tier degrades per song and per transition.
- Stored analysis records which model heard each song. Moving up a tier re-measures; moving down keeps the better result.
- **Do not offer Best before it is measured to help.** `analysis.md` found the small model moved no mix window from wrong to right on the real album tested and that it agrees with the full model at only F 0.867 on real clips; whether `final0` helps on the owner's library is unmeasured, and so is its time per song on the S22.

### 7.3 What can be prepared at download time

Nori's fetch path already decodes a song once, as its bytes arrive, on a lowest-priority thread (`crates/engine/src/arriving.rs`: fetching ahead, a download and the loader all hand their pieces over), so the decoded audio is there at download time. Only a song fetched whole from its first byte is kept as measured.

- **Per-song, so can run at or after download:** beats, downbeats, tempo (the big beat model, octave correction, time signature from downbeat spacing); structure and per-bar energy, drops, breakdowns, intro and outro points, dead gaps and hidden tracks; loudness, key, silence; loop candidates (clean instrumental bars); vocal activity (from a vocal stem, or from synced lyric timings when the song has lyrics); stems of the transition windows (the last and first 45 s; whole-song stems are 100 MB or more, the windows an estimated 30 to 40 MB per song); server-only extras (All-In-One structure, the beat ensemble). All keyed to song and analysis version, in the existing analysis store.
- **Needs both songs, so cannot run at download:** which transition, its length, and the candidate renders. Run them as lookahead when the next song is known (fetched ahead, or at the start of the current song), cache per pair and drop the cache when the queue changes.
- **Playback only:** applying the chosen render; reacting to setting changes.
- **Limits:** a bulk library download would keep a phone busy for hours, so do two passes: the cheap ends-only measurement at download, and the heavy full-song pass later on the charger. A server transcode is a different file from the original, so key the cache to what was heard. Songs streamed without a download only get the fetch-ahead window.

### 7.4 "Studio": stems around the transition (proposal)

Separate each song into vocals, drums, bass and the rest, for only the overlap (about 45 s of the outgoing song's end and of the incoming song's start), ahead of time. Then: a vocal duck by stem instead of a band-pass; drums first; a vocal echo-out; an acapella over an instrumental (only for compatible tempo and key); exact vocal activity. Render on the layers only inside the overlap, keep the original mix underneath as a residual bus and crossfade to and from it, because stem sums have bleed. If the layers are not ready in time, fall back to the current transition. Where it runs (phone or server or desktop) is decided by the S22 speed test in §6.3. Largest effort on the list; do it after the grids and the chooser.

### 7.5 Apple's repeat of a beat or a vocal (opinion, agreed direction)

The Reddit threads (`automix.md` §1a) are split: a repeated beat or bar is liked, a looped vocal is the loudest complaint ("repeats lyrics", "a disrespect to the artists"). So: repeat the instrumental (with stems, loop the drums and instrumental of the last bar or two while the vocal fades or echoes out); offer a deliberate vocal stutter as an optional style only, off by default, on the beat grid, 2 to 3 repeats at most with an echo tail, at a phrase end with a clean vocal; never loop a whole lyric line abruptly. Without stems keep today's rule: loop only instrumental bars. How good stem-based repeats sound (bleed shows up in loops) needs listening on the phone.

### 7.6 Best model per task (a shortlist to test, not a verdict)

Rankings come from each model's own reported numbers on different test sets. Confirm on the owner's library before locking anything in.

| Task | Best pick | Runs | Notes |
|---|---|---|---|
| Beats, downbeats, tempo | Beat This! `final`, average of `final0/1/2` (the average is untested) | phone on charge, or server | reported state of the art without post-processing; nothing read beats it |
| Structure, sections | All-In-One | server or desktop | needs Demucs stems; weights licence unstated; a 2025 foundation-model structure paper ([arXiv 2507.13572](https://arxiv.org/abs/2507.13572)) reports larger gains but no code was found |
| Mix-in and mix-out points | the 2024 cue-point detector ([arXiv 2407.06823](https://arxiv.org/abs/2407.06823)) | server | 21k DJ cue points, EDM-heavy; test on pop and rock |
| Stems, best quality | Mel-Band RoFormer or BS-RoFormer | server or desktop | vocals stem about 11 dB SDR; 0.27x real time on a phone GPU, so not for the phone |
| Stems, on the phone | SCNet XL, else htdemucs_ft | phone if the S22 test passes | phone speed unmeasured |
| Vocal activity | a vocal stem, plus lyric timings when the song has them | server or desktop | replaces the heuristic |
| Transition controller | nothing ready; DJtransGAN and Diff-MST as templates | later | train on mined DJ-mix data |
| Key | keep our own, improve the profiles | phone | no chord model adds audible value for mixing |
| Drops, builds | per-bar energy curves | phone | no usable model exists |

Skip even at best quality: generative bridge audio, and the strong music embeddings as a "what follows what" predictor (nothing shows they predict transition quality, and the queue order is the user's).

### 7.7 Licences (personal open-source project, MIT, non-commercial)

Confirmed MIT code and weights: Beat This! (`final`, `small`), Signalsmith Stretch. Non-commercial weights (CC BY-NC, CC BY-NC-SA: MERT, MuQ, the Essentia models, madmom, Open-Unmix `umxl`) are acceptable for a non-commercial open-source app, but ShareAlike ones clash with an MIT release if bundled, so fetch them from the authors and do not ship them. Unstated weights licence (All-In-One, DJtransGAN, the cue-point model, SCNet): no stated permission to redistribute, so fetch from the authors; a GitHub issue asking the authors settles it. GPL code (`demucs-android`, Mixxx's effects, Rubber Band) cannot be copied into an MIT project; reading it for ideas is fine. **Still to verify:** the Demucs weights (the repo states only the code licence, a third-party export says all MIT, and `analysis.md` earlier recorded them as scientific-use-only) and whether checkpoints trained on the academic-only MUSDB18-HQ inherit that. This is a reading of licence pages, not legal advice.

### 7.8 First experiments when the work starts

1. The real-music benchmark: record Apple's mixes of the same pairs through AirPlay to a Mac with Audio Hijack (`automix.md` §1a), and score nori on the same songs.
2. `final0` (and the three-checkpoint average) against `small` and the classical grid on the owner's library, scored by how many transitions leave the "no reliable beat grid" fallback.
3. The S22 stem-speed harness (§6.3).
4. All-In-One and the cue-point model over a sample of the library, compared with the current drop and breakdown heuristics.

