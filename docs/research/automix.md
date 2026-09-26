# AutoMix-style transitions for an Android player: research and design

Labels used: **[verified]** = stated by a cited source; **[inferred]** = my reasoning or estimate, not confirmed by a source.

The song analysis underneath (beats, bars, tempo, key, sections, vocals: methods, licences, what runs on a phone,
and how nori's analysis measures against a synthetic test set) has its own document, `analysis.md`. Where the
two disagree on the analysis, `analysis.md` is the later one.

## 1. What Apple's AutoMix does

**Apple's own wording.** Apple's WWDC25 services release says AutoMix uses AI "to analyze audio features" and "crafts unique transitions between songs with time stretching and beat matching" ([Apple Newsroom](https://www.apple.com/newsroom/2025/06/apple-services-deliver-powerful-features-and-intelligent-updates-to-users-this-fall/), [MusicTech](https://musictech.com/news/gear/apple-music-automix-ai/)). Press coverage adds that it looks at tempo and key ([MacRumors](https://www.macrumors.com/how-to/ios-enable-automix-feature-apple-music/)). No WWDC technical session covers the DSP.

**Limits and behaviour, from Apple Support** ([support.apple.com/105067](https://support.apple.com/en-us/105067)) **[verified]:**
- It only works with Apple Music subscription content. It does not work with uploaded or matched library files or iTunes purchases, even when the same song is in the catalogue.
- It is not available with hi-res lossless. On Mac it needs Apple silicon.
- It may not transition when an album plays in order, or when "the genres or tempos are incompatible". In those cases it falls back to a plain transition.
- Crossfade uses a fixed 1–12 s. AutoMix picks its own transition points and length.

**Server-side or on-device? [inferred]** The catalogue-only rule, with a clean copy of the same song being refused, strongly suggests the transition metadata is computed by Apple on the server for each catalogue track: beat grid, cue points and compatibility. The rendering (time-stretch, filters, mixing) happens on the device, which fits the Apple-silicon and no-hi-res requirements. Apple has not confirmed this. A third-party developer notes that MusicKit never exposes decoded protected audio to apps ([primuse #117](https://github.com/chenqi92/primuse/issues/117)), so a client could not analyse protected audio itself anyway.

**What it sounds like.** In iOS 26 the transitions were described as predictable, with a characteristic "underwater" sound ([9to5Mac](https://9to5mac.com/2026/07/16/ios-27-makes-one-of-my-favorite-apple-music-features-even-better/)). **[inferred]** That is almost certainly a low-pass filter sweep on the outgoing track, so the effect is DJ-style filtering and not only volume. In iOS 27 Apple reworked the intro and outro sections so the tempos line up, and it loops or repeats parts of intros and outros to bridge the two songs ([MacRumors](https://www.macrumors.com/2026/06/09/apple-music-gains-automix-upgrades-and-more-in-ios-27/), [RouteNote](https://routenote.com/radar/apple-music-automix-gets-a-major-upgrade-in-ios-27/)). Apple has not published how long transitions are or whether it uses EQ swaps.

**Complaints** ([How-To Geek](https://www.howtogeek.com/i-disabled-apple-music-automix/), [BGR](https://www.bgr.com/2059450/how-to-turn-off-automix-apple-music-worst-feature/)):
- The transition does not always happen.
- It sometimes cuts off final chords.
- It sometimes skipped "as much as a full minute" to line up the tempos.
- Volume spikes and "broken record" stutters.
- It made odd choices, such as jumping into the middle of the next song.
- Album listeners dislike it, and it suits house, techno and pop better than other genres.

**Lesson for us:** never throw away a lot of the song, keep the transition window bounded, and make fallbacks conservative.

## 2. Comparable features

- **Spotify Automix.** It looks at tempo, key, energy and rhythmic structure. Spotify, not the user, chooses the start and end points, and the user cannot change the overlap length ([Spotify Community](https://community.spotify.com/t5/FAQs/Automix-Overview/ta-p/5257278)). Its newer Mix feature for playlists shows waveform, key and BPM and offers transition presets such as "Fade" and "Rise", with EQ, effects and cue-point editing ([MusicRadar](https://www.musicradar.com/music-tech/spotify-responds-to-apple-musics-new-automix-feature-by-letting-you-turn-your-playlists-into-ready-made-dj-sets-with-seamless-transitions)). Spotify computed bars, beats, sections, key and loudness on its servers for years. Its Web API exposed this as `/audio-analysis` until access was cut on 27 Nov 2024 ([Music Ally](https://musically.com/2024/11/28/spotify-removes-features-from-web-api-citing-security-issues/)). This supports the idea that server-side analysis is standard in the industry.
- **Symfonium Smart Fades.** Experimental, and it "requires waveform extraction" ([Symfonium 12.3.0](https://symfonium.app/news/version-1230/)). The developer says that "smart fades with settings are crossfade" ([forum](https://support.symfonium.app/t/smart-fade-tuning/12114)). Users complain that songs with long tails still get faded, and that tracks with a loud start still get a fade-in ([feedback thread](https://support.symfonium.app/t/smart-fades-feedback-thread/7900)). **[inferred]** It seems to pick fade points from where the amplitude envelope crosses thresholds. It does not beat-match.
- **Plexamp Sweet Fades.** Based on MPD's MixRamp. The server measures EBU R128 loudness and works out how far two songs should overlap from the loudness ramps at the end of one and the start of the next ([Plex Labs](https://medium.com/plexlabs/plexamp-v3-9af3b10063b4)). In MPD, MixRamp tags store how loudness changes over time at each end of the song. Overlap is set where both songs sit at `mixrampdb` (e.g. −17 dB), and MPD can also analyse songs on the fly ([MPD docs](https://mpd.readthedocs.io/en/stable/user.html)). This is the cheapest "smart" transition and makes an ideal fallback.
- **Poweramp.** A fixed-length crossfade in milliseconds, with separate settings for automatic and manual track changes, plus fades on seek, play and pause ([guide](https://caninfotech.com/poweramp-music-player/poweramp-music-player-how-to-crossfade-between-two-tracks/)). No analysis.
- **djay Automix AI.** It "identifies rhythmic patterns and the best intro and outro sections", "calculates optimal fade durations and automatically applies parameter changes to EQs and filters". Neural Mix adds stem separation ([Algoriddim](https://help.algoriddim.com/user-manual/djay-pro-windows/mixing-basics/automix)). Rekordbox and Serato work from a beat grid computed offline, with cue points and phrase analysis (Rekordbox). **[not re-verified here]**
- **Open-source reference.** kumone PR #51 is a full AutoMix design built on vDSP ([kumone #51](https://github.com/missuo/kumone/pull/51)):
  - Analysis: spectral-flux onsets, BPM by autocorrelation with a log-normal prior, Ellis DP beat tracking, downbeat voting, phrase boundaries, RMS-based intro and outro landmarks, BS.1770 loudness, Krumhansl key, and how much vocals are present over time.
  - Five checks decide whether a pair may be mixed: loudness gap, timbre distance, tempo stability, key distance and vocal clash. Each pair then gets a transition type, from a short fade up to a beat-matched mix with EQ hand-over or a beat-synced echo-out.
  - It checks alignment at bar level with a 3 % tolerance, because beat-level checks failed: onset timing jitters by 5–13 %.
  - Offline rendering runs at 100–300× realtime.
  - It also falls back to plain whole-mix blending when rendering fails or is late.

  This is the closest public blueprint for what we want.

## 3. Algorithms

### Tempo and beat tracking
Standard pipeline, which suits mobile:
1. Downmix to mono and resample to about 22 kHz.
2. STFT (window 1024–2048, hop 512), then mel or log-magnitude bands.
3. Half-wave-rectified spectral flux gives the onset-strength envelope (about 43 frames/s).
4. Autocorrelate the envelope, or run a comb filterbank, over 60–200 BPM. Weight by a log-normal prior centred near 120 BPM.
5. Ellis (2007) dynamic programming finds the beat sequence. Each beat's score is its onset strength plus the best earlier score, minus a penalty for straying from the target beat period. Backtracking gives the path.

Ellis reported just under 60 % beat accuracy on MIREX-06 development data ([paper](https://www.ee.columbia.edu/~dpwe/pubs/Ellis07-beattrack.pdf)). Tempo is usually scored as Acc1 (within 4 % of the true tempo) and Acc2 (also counting ×2, ×3, ½ and ⅓ as correct). **Half and double tempo errors are the main failure mode** ([Hörschläger et al.](https://www.ifs.tuwien.ac.at/~knees/publications/hoerschlaeger_etal_smc_2015.pdf)).

For mixing, most octave errors do no harm. When comparing two tracks, compare BPM after folding ×½ and ×2, and beat-match at whichever level gives the smallest ratio.

Rust crates, both MIT/Apache:
- **`beat-track-rs`** is exactly Ellis 2007: mel spectral flux, autocorrelation with a log-normal prior, then DP. It uses rustfft and ndarray ([docs.rs](https://docs.rs/beat-track-rs)).
- **`stratum-dsp`** covers BPM, key and HMM beat grids, with optional ONNX ([docs.rs](https://docs.rs/stratum-dsp)).

aubio is GPL and Essentia is AGPL, so avoid both. Recommendation: implement it in our own Rust core (about 500 lines), using `beat-track-rs` as a reference or dependency. We already have FFT/DSP code.

**CPU [inferred estimate]:** a 4-min track at 22 kHz gives about 10k frames. rustfft on one Cortex-A7x core takes roughly 0.1 s. Onset detection, autocorrelation and DP add less than 50 ms. **Decoding dominates:** about 0.2–1 s per track for MP3, AAC or Opus, less for FLAC. Total: about 0.3–1.5 CPU-seconds per track, roughly 0.2–0.5 % of the track's duration.

### Downbeats and phrases (lightweight)
- **Downbeat phase:** test the 4 possible bar starts (assume 4/4). For each one, add up bass-band onset strength (kick) and chroma change (chords tend to change on the "1") at every 4th beat, then take the phase with the highest total. This is the "downbeat voting" approach.
- **Phrases:** use beat-synchronous features (RMS, bass energy, chroma). Compute a Foote novelty curve from the self-similarity matrix, or simply look for energy jumps. Keep only candidates that fall on multiples of 8 or 16 bars from the first downbeat.
- **Checking:** check the grid at bar level, not beat level (see kumone). Flag tracks with drifting tempo (live recordings, older music). The DP beat intervals show this through their variance, and such tracks should not be beat-matched.

### Intro and outro regions
- Compute the RMS and loudness envelope at about 10 Hz, per beat. Trim silence where the level stays below −50 to −60 dBFS.
- The outro candidate is the last phrase boundary before the energy drops, or the last 16–32 bars when the ending is steady. The intro candidate is the region before the first large energy or bass jump.
- Also store MixRamp-style points: when the end of the track falls below −17 dB relative to track loudness, and when the start rises above it. These are the fallback.
- **Vocal activity heuristic [inferred, rough]:** high energy in the 300 Hz–3.4 kHz band relative to the whole spectrum, together with the spectral flatness and centroid patterns vocals produce, smoothed per beat. It is enough to avoid overlapping two vocal sections, but not to detect lyrics. Stem separation is too heavy for a battery-first design.

### Time-stretching

| Library | Licence | Quality at ±2–8 % | Notes |
|---|---|---|---|
| **Signalsmith Stretch** | MIT | Very good; rated alongside Rubber Band R3 ([KVR](https://www.kvraudio.com/forum/viewtopic.php?t=623537)) | C++11, header-only. Rust crates `signalsmith-stretch` ([lib.rs](https://lib.rs/crates/signalsmith-stretch)) and `ssstretch`. It has a cheaper preset. Build it with optimisation on, because it is about 10× slower without ([docs](https://signalsmith-audio.co.uk/code/stretch/)). |
| **Bungee** | MPL-2.0 | Good (adaptive phase vocoder) | Supports Android. Rust bindings `bungee-rs` ([GitHub](https://github.com/bungee-audio-stretch/bungee)). |
| Rubber Band | GPL, or paid commercial licence | R3 is excellent, R2 is fine | R3 uses a lot of CPU ([licence](https://breakfastquay.com/rubberband/license.html)). |
| SoundTouch | LGPL-2.1 | OK for small changes (WSOLA), tuned for pop/rock | About 100 ms latency. `soundtouch` crate ([lib.rs](https://lib.rs/crates/soundtouch)). |
| media3 Sonic | Apache-2.0 | Poor for music | Based on PICOLA and aimed at speech; its author says music quality is "pretty poor" ([Sonic docs](https://github.com/waywardgeek/sonic/blob/master/doc/index.md)). Fine as a last resort for ≤2 %. |
| Resampling ("vinyl") | – | Pitch moves 0.34 semitone per 2 % | The cheapest option. Many DJs accept it at ±2 %. |

Recommendation: **Signalsmith Stretch** (MIT) inside the Rust core, linked statically through its crate. Use varispeed resampling for changes of 2 % or less when the user enables that mode.

**CPU [inferred]:** Signalsmith at the default preset on 44.1/48 kHz stereo probably uses a single-digit percentage of one big mobile core, and only during the 10–30 s window. Measure it on device.

### Key detection
Take chroma from the STFT, averaged over the track (better: weighted towards the intro and outro windows that will actually overlap). Correlate it with Krumhansl or Temperley profiles for all 24 keys. Expect about 70–85 % accuracy on tonal Western pop, with relative-key and fifth errors being common ([summary](https://github.com/Corentin-Lcs/music-key-finder)).

**Verdict:** do not reorder the queue by key, because users of a library player expect the queue to be respected. Use key distance as one input to the pair score: Camelot distance ≤1 allows a long harmonic overlap, a clashing pair gets a short overlap, drums only, or an echo-out. The cost is minimal because the chroma comes from the same STFT.

### Transition shaping
- **Equal-power curve** (cos/sin) for uncorrelated material. Linear or sine-squared for beat-matched, phase-locked content, where the two tracks add coherently.
- **Bass swap:** the incoming track starts with its lows cut (high-pass or low-shelf at about 150–200 Hz, −20 to −inf dB). On a downbeat at a phrase boundary, swap in one move, over about 1 beat: cut the outgoing lows and restore the incoming lows. Only one track ever carries the bass ([vibesdj](https://vibesdj.io/learn/techniques/eq-swapping), [Club Ready DJ School](https://www.clubreadydjschool.com/tribe-talk/getting-started/bass-swapping-dont-make-this-common-mistake)). Use a gradual swap when the incoming intro is sparse.
- **Filter sweep:** a low-pass on the outgoing track, from 20 kHz down to about 300 Hz over 4–8 bars (the "underwater" sound). Or a high-pass on the outgoing track as the incoming track comes in.
- **Echo-out:** feedback delay synced to the beat on the outgoing track, then cut. Use it for clashing pairs.
- All of this is biquads plus gains. Per-sample cost is negligible.

## 4. Recommended design (battery-first)

**When to analyse:**
1. **During normal playback, for free:** tap the PCM already passing through our audio chain. Decoding is already paid for, so feed a streaming analyser in the Rust core (STFT, onset envelope, chroma and RMS accumulators). At track end, run tempo, DP, downbeat, phrase and key, which takes tens of ms, and store the record.
2. **When a track finishes downloading or caching:** analyse it on a low-priority thread, or queue it.
3. **Backfill the library** with WorkManager, constrained to charging + unmetered + battery-not-low (+ device idle). The work is batched and can resume.
4. **Just in time (first play, no record):** about 20 s before the outro window, decode only the last ~45 s of the current track and the first ~45 s of the next. That is about 10 % of a full analysis. Grid confidence is lower, so use a more conservative transition.

**Storage:** one SQLite row per track, about 200–500 bytes. Key it on server id + file hash or duration, and store an `analysis_version`.

```
bpm REAL, bpm_confidence REAL, beat_offset_ms INT, tempo_stable BOOL,
downbeat_phase INT(0-3), first_downbeat_ms INT,
intro_end_ms INT, outro_start_ms INT,                -- phrase-aligned cues
cue_candidates BLOB  -- few (ms, bars, energy, vocal) tuples
lead_silence_ms INT, trail_silence_ms INT,
mixramp_start_ms INT, mixramp_end_ms INT,
loudness_lufs REAL, key INT(0-23), key_confidence REAL,
vocal_end_ms INT, vocal_start_ms INT
```

Beat times are not stored. The grid is rebuilt as `offset + n·60/bpm`. For tracks with drifting tempo, beat matching is simply turned off.

**Planning at playback (Kotlin):**
1. Take the records for the current track A and next track B, and compute the tempo ratio after folding ×½ and ×2.
2. Beat-match only if both grids are confident and stable and the ratio is within the user's max (default ±6 %).
3. Choose A's outro cue and B's intro cue on phrase boundaries, avoiding vocal-on-vocal overlap. Never skip more than about 15 s of either track. This directly addresses Apple's "skipped a minute" complaint.

**Rendering (Rust):**
- Run both decks through the existing chain. Only B is stretched during the window, locked to A's tempo.
- After the swap, ramp B back to its native tempo over 4–8 bars, then bypass the stretcher.
- Apply the filters and bass swap, with loudness matching from LUFS or ReplayGain.
- **Outside the window the extra cost is zero.** During it, one stretcher plus about 6 biquads run for 10–30 s.

**Navidrome:** OpenSubsonic `Child` exposes `bpm` and `replayGain` ([OpenSubsonic Child](https://opensubsonic.netlify.app/docs/responses/child/), [Navidrome PR #2597](https://github.com/navidrome/navidrome/pull/2597)). Use `bpm` as a prior to settle half/double tempo, and as a prefilter to skip pairs that cannot be matched without decoding anything. It has no beat phase, so it cannot drive beat matching alone. Tag quality varies.

**Fallback ladder:**
1. Full analysis: beat-matched mix with bass swap.
2. Grid missing or unreliable: phrase-less crossfade at MixRamp or silence-trimmed points, with a filter sweep.
3. Nothing is known: a fixed equal-power crossfade.
4. Same album, played in order (or gapless-tagged): gapless, with no mixing.

## 5. Settings

- AutoMix on/off (separate from Crossfade).
- Style: Smart fade only / DJ mix.
- Transition length: auto, or a maximum in bars/seconds (e.g. 4–32 bars).
- Beat matching on/off.
- Allow tempo change on/off, with a maximum change of 2/4/6/8 %.
- Keep pitch (time-stretch) vs varispeed.
- Bass swap on/off. Filter effects on/off.
- Skip transitions within albums (default on), and respect gapless.
- Also transition on manual skip.
- Loudness matching.
- Analyse library only on charger + Wi-Fi (default on).
- Per-track exclusion ("never mix this track").

## 6. Competitive audit (2026) and what we ship

| Capability | Apple Music AutoMix | DJ.Studio Harmonize | Symfonium / Plexamp | **nori** |
|---|---|---|---|---|
| Beat match + time-stretch | yes (catalogue AI) | yes (offline edit) | no / MixRamp only | **yes, on-device** |
| Bass swap | not documented | yes | no | **yes** |
| LPF / filter sweep | yes (iOS 26 “underwater”; iOS 27 softer) | yes + HPF presets | no | **yes; Camelot-softened** |
| Echo-out for clashes | simple fade fallback | yes | no | **yes** |
| Camelot-aware length | inferred (key+tempo) | yes (bars 4–32) | no | **yes (≤1 long, 2 short, ≥4 echo)** |
| Loudness match | yes (catalogue) | yes | MixRamp / RG | **LUFS when RG off** |
| Max skip bound | criticised (≤1 min) | n/a (edit) | n/a | **15 s of music, hard cap; silence free** |
| Enter the next song on its drop | not documented | yes (manual cues) | no | **yes** (section 7) |
| Leave before a dead ending or hidden track | criticised for false endings | manual | no | **yes, within the cap** |
| Two singers kept apart | unknown | EQ lanes | no | **vocal duck + high-pass ride** |
| Phrase-matched start and swap | phrase-aligned (inferred) | yes | no | **yes** |
| Album-in-order gapless | yes | n/a | yes | **yes** |
| Works on self-hosted library | **no** (catalogue only) | yes (files) | yes | **yes** |
| Hi-res / USB DAC path | blocked on hi-res | n/a | varies | **offload-aware** |
| Intro/outro loop remix | **iOS 27** | loop effects | no | **outro loop remix** (intro live-loop deferred) |
| Reorder playlist by key | no (queue respected) | **yes (Harmonize)** | no | **no** (by design: library player) |
| Stem separation | no | optional | no | **no** (battery) |
| Tag BPM half/double prior | inferred | yes | n/a | **yes** |
| DJ filter-open (HPF) | soft in iOS 27 | yes | no | **yes** (Camelot stretch pairs) |

**Verdict.** For a library player that respects queue order, we match or beat Apple on self-hosted music: on-device analysis, bass swap, clash echo-out, MixRamp fallback, hard skip cap, Camelot-scaled length/filters, LUFS match, tag-BPM octave correction, and outro loop remix when the ending is too short for the target overlap. Still behind Apple’s catalogue intro looping (needs a second decode source) and DJ.Studio’s playlist reordering — deliberate non-goals for a queue-respecting library client.

## 7. Entering on the drop, leaving before a dead ending, two singers (2026-09)

The owner compared AutoMix with BitChord's and asked for three things: enter the next song on its drop, let the
outgoing song's exit be an interior point when its ending is not worth playing, and keep two singers apart with
filters rather than rerouting to an echo-out. Beside them, this section surveys how the other automatic mixers and
DJ practice handle transitions, and says which of those ideas were built.

Hosts other than GitHub could not be opened from here (Spotify, Mixxx's site, Algoriddim, DJ.Studio, most blogs).
Where a page could not be read, the fact below comes from the search engine's excerpt of it and is marked
**[excerpt]**; **[read]** means the page or file itself was read. BitChord (GPL-3.0) and Orchard (AGPL-3.0 from
4.0; releases up to 3.x were MIT) were read for facts only; no code was taken.

### 7.1 What others do

- **Apple Music AutoMix.** iOS 27 remixes intros and outros so tempos align, and repeats parts of them to bridge
  two songs ([MacRumors](https://www.macrumors.com/2026/06/09/apple-music-gains-automix-upgrades-and-more-in-ios-27/),
  [RouteNote](https://routenote.com/blog/apple-music-automix-upgrade/)) **[excerpt]**. The complaints are about
  where it cuts: starting the mix early enough to chop the last 30 s of a song, starting the next one 49 s in (a
  Taylor Swift example that skipped a first verse), and being caught out by false endings
  ([TechRadar](https://www.techradar.com/audio/apple-music/apple-music-fans-are-obsessed-with-automix-in-ios-26-but-one-big-flaw-could-be-its-downfall))
  **[excerpt]**.
- **Spotify Automix and Mix.** Automix trims intros and outros and aligns tempo within limits; Mix (beta, 2025)
  shows waveform, BPM and key and offers presets: **Fade** (a crossfade with the bass swapped around the midpoint),
  **Rise** (an overlap with the bass swap at the end, low-pass in and high-pass out) and **Blend** (a smooth
  three-band EQ fade), each editable as volume, EQ and effect curves
  ([Spotify](https://newsroom.spotify.com/2025-08-19/mix-your-favorite-playlists-seamlessly-by-adding-your-own-transitions/),
  [Yahoo Tech](https://tech.yahoo.com/audio/articles/spotifys-mixing-feature-lets-dj-093000572.html)) **[excerpt]**.
- **djay Automix AI.** Finds "the best intro and outro sections" and rhythmic patterns, automates EQs and filters,
  and with Neural Mix splits the songs into stems during a transition (a reverb on the outgoing vocal, say);
  transition types include Dissolve, Riser and Echo
  ([MusicTech](https://musictech.com/news/gear/algoriddim-free-dj-software-djay-pro-ai-automix-and-neural-mix/),
  [DJ Mag](https://djmag.com/news/new-djay-ai-ios-adds-improved-ai-mixing)) **[excerpt]**.
- **Mixxx Auto DJ.** Uses intro and outro cues (set by silence detection, editable). *Full Intro + Outro*, the
  default, starts the next track during the outro so that **the end of the intro lines up with the end of the
  outro**; *Fade At Outro Start* lines up their starts and cuts the rest of a longer outro; the crossfade is the
  shorter of the two sections
  ([Mixxx manual source](https://github.com/mixxxdj/manual/blob/2.4/source/chapters/djing_with_mixxx.rst),
  [wiki](https://github.com/mixxxdj/mixxx/wiki/Auto%20DJ%20Cues)) **[read]**.
- **rekordbox.** Phrase analysis labels Intro, Up, Down, Chorus, Verse, Bridge and Outro according to a track's
  "mood"; its Automix uses beat position, BPM and key
  ([Phrase Edit guide](https://cdn.rekordbox.com/files/20200312172204/rekordbox5.1.0_Phrase_Edit_operation_guide_EN.pdf))
  **[excerpt]**. Serato's Autoplay plays tracks back to back with no crossfade
  ([Serato](https://support.serato.com/hc/en-us/articles/202304934-Can-Serato-DJ-auto-mix-my-songs)) and Engine DJ
  users are still asking for an auto-mix
  ([Engine DJ community](https://community.enginedj.com/t/auto-mix-needed-for-engine-dj-stand-alone-controllers/55157))
  **[excerpt]**.
- **DJ.Studio.** Transition presets are volume, EQ and effect curves (slow crossfades, mid-band blends, filter
  sweeps, instant bass swaps); lengths are set in bars; Harmonize uses the Camelot wheel and lets the user choose
  how tempo is carried across ([help](https://help.dj.studio/en/articles/7878402-harmonize-previously-automix))
  **[excerpt]**.
- **Mixed In Key.** Scores energy 1 to 10 from the content (hi-hat patterns, noise risers), not the tempo, and
  advises mixing within one level for a steady set
  ([Mixed In Key](https://mixedinkey.com/harmonic-mixing-guide/sorting-playlists-by-energy-level/)) **[excerpt]**.
- **Plexamp Sweet Fades.** MPD's MixRamp on EBU R128 loudness (section 2)
  ([music-assistant discussion](https://github.com/orgs/music-assistant/discussions/3929)) **[read]**.
- **Orchard.** "Beat-matched, phrase-aligned AutoMix transitions with 3-phase volume curves, progressive filter
  sweeps, downbeat quantization, and bass swaps", on-device beat analysis on mobile, BPM from GetSongBPM
  ([README](https://github.com/SFG5453/Orchard)) **[read]**.
- **BitChord** (read-only clone, `playback/smart/*`, `native/analyzer/*`) **[read]**. Entry candidates are scored:
  a "main drop" (weight 0.5), an "intro drop" (0.4), the audible start (0.15) and phrase lines (0.1), plus 0.1 on a
  downbeat, minus 0.2 for a cold open (under four beats of run-up) and plus up to 0.2 for an instrumental run-up
  over the 16 beats before. Its "main drop" is simply 32 beats after the first downbeat when that is inside the
  first 40 % of the song, its intro drop the first 8-bar line capped at 36 s. Exits are an "energy cliff" (a late
  silence, backtracked to where the level fell), the outro start or the end of the content, under a 12 s budget of
  skipped music in which silence (below a tenth of the loud level) is free. Two voices are handled by filter rides
  scaled by how much they overlap: the outgoing song low-passed towards 1.6 kHz, the incoming one high-passed from
  700 Hz (520 to 950 Hz in a beat-matched blend) and opened by 45 to 70 % of the mix.
- **DJ practice.** A phrase is eight bars (32 beats) and sections change on phrase lines; the incoming track starts
  on beat one of a phrase and its intro is laid over the outgoing outro so that both turn together
  ([Native Instruments](https://blog.native-instruments.com/phrase-mixing/),
  [Wikipedia](https://en.wikipedia.org/wiki/Phrasing_(DJ))) **[excerpt]**. Voices sit between about 200 Hz and
  4 kHz; to stop two clashing, cut the incoming track's mids during the blend and swap them over as the tracks
  change hands ([Digital DJ Pool](https://digitaldjpool.com/blog/dj-eq-mixing-for-beginners/),
  [Home DJ Studio](https://homedjstudio.com/dj-eqing/)) **[excerpt]**. Key clashes matter only where melodic parts
  overlap; percussive intros and outros are key-neutral
  ([Pioneer DJ](https://blog.pioneerdj.com/djtips/how-do-djs-approach-harmonic-mixing/),
  [Digital DJ Pool](https://digitaldjpool.com/blog/harmonic-mixing-camelot-wheel/),
  [OpenKeyScan](https://www.openkeyscan.com/harmonic-mixing-for-house-music)) **[excerpt]**. And for energy: do not
  mix from a high-intensity section into an intro, which drops the floor
  ([DJ.Studio](https://dj.studio/blog/anatomy-great-dj-mix-structure-energy-flow-transition-logic)) **[excerpt]**.

### 7.2 What was built

All of it is in nori-player's `automix` (`plan.rs`, `structure.rs`, `loudness.rs`, `mixer.rs`), which every
platform shares; nothing outside it changed but the stored rows' columns. `ANALYSIS_VERSION` is 8, so rows are
measured again as songs play.

1. **Enter on the drop.** The analysis finds where the arrangement arrives (`drop_point`): the first four-bar line
   in the opening (40 % of the song, 75 s at most) where the four bars after it reach the body of the song - within
   2 to 3 dB of its median bar in level, low end and chord energy - and the bars before lacked one of them by 3 dB.
   A drum intro has the level but not the chords, a pad the chords but not the low end, so neither is the drop; the
   full band arriving is. It stores the voice share of the eight bars before and after. The planner then searches
   (`drop_aligned`): every landing of the incoming song (the drop, the end of its intro, its four-bar lines, its
   first downbeat) against every downbeat of the outgoing song's last sixteen bars, with run-ups of sixteen bars
   down to none and a tail of a beat to four bars after the swap. The swap is where the landing meets the downbeat,
   so the drop, the bass swap (now over the sixteenth before the line, so the drop's first kick has all its low
   end) and both songs' section change coincide - Mixxx's *Full Intro + Outro*, Spotify's *Rise*. Windows that
   skip more than 15 s of music of either song are never considered; the rest are scored on the landing (drop 4,
   intro end 3, phrase line 1, straight in 0), skipped music (0.06 per second of an instrumental end, 0.15 of a
   sung one), the incoming intro left on its own after the mix (0.1 per second: the energy hole), the run-up laid
   under the outgoing song, a phrase line of the outgoing song, and a sung run-up under a sung ending. When the
   outgoing song has too few bars for the run-up and they are not sung, its last four or eight are read round (the
   iOS 27 outro remix, now a candidate in the search rather than a separate path; a sung loop is Apple's "broken
   record"). *Limits:* a drop further in than 15 s plus the longest run-up the mix length allows is out of reach (a
   16-bar drum intro under a 16 s mix); the drop is only as good as the grid (none without one, one bar off on a
   half-time grid).
2. **Leave before a dead ending.** The analysis finds the last silence of 6 s or more inside the music
   (`last_gap`) and a closing breakdown (`breakdown`): the earliest bar line in the last 24 s where the level falls
   6 dB below the four bars before, the beat goes with it (6 dB of low end or a third of the onsets) and no bar
   comes back within 3 dB - a pad coda, a breakdown the song never returns from, a fade-out. The exit is that
   breakdown, or the gap when the music after it is 15 s or less (a short hidden track). The planner's `Ending`
   charges only music against the cap - the gap and the silence at either end of a file are free - and aims the
   swap at the exit, so the incoming drop lands where the outgoing energy leaves, the breakdown falls away under
   it for up to four bars and the rest (within the cap) is not played. The same rule applies to echo-outs,
   one-grid fades and MixRamp fades. *Limits:* a hidden track longer than 15 s is music, so it is played, and the
   silence before it with it; a false ending with more than 15 s of song after it is not an exit; the sink drops
   the skipped remainder by decoding through it (see `docs/handoff.md`).
3. **Two singers.** The mixer has a vocal duck on the incoming deck: a band-pass around 1 kHz (Q 0.35, 3 dB down
   near 300 Hz and 3.3 kHz) subtracted in proportion, which is a peaking cut whose depth can move sample by sample
   without touching the filter's state, so it releases without a click and leaves the deck untouched, bit for bit,
   when off. The plan holds the incoming voice band 18 dB down at 1 kHz (about 6 dB at 500 Hz and 2 kHz) while
   the outgoing song still leads, releasing it over the beat before the swap, and after the swap rides a high-pass
   on the outgoing deck from 200 Hz to 2 kHz over the first half of what is left, so its voice thins to breath
   while it falls away - the DJ's mid swap, with the song taking over always the clear one. Each half only where
   both sing then. A pair whose voices overlap now gets this beat-matched mix; it echoes out only when no mix fits
   or the filters are off. A plain fade holds the incoming voice down through its first half. One band-pass per
   channel on the incoming deck while the duck is on: the mixer with every effect on costs 2.3 ms of CPU per second
   of audio (0.23 % of one core) on the desktop measured. *Limit:* it triggers on what the analysis calls sung, and
   the voice-band share cannot tell the synthetic singing (0.28 to 0.37) from an unsung band (0.20 to 0.34) or pads
   (0.8) - `analysis.md`'s open problem. The harness therefore also runs with the voices taken from the truth.

Chosen from the survey:

4. **A drum intro is laid under any key.** Key clashes need two melodic parts; the analysis stores the chord energy
   of the run-up to the drop (its most chordal four bars, a beatless opening included), and a run-up at least 3 dB
   below the song's chords is exempt from the Camelot and timbre caps on length, which then hold only after the
   swap, where both songs are whole.
5. **Phrase-matched starts.** Run-ups of whole four-bar phrases are preferred, so the mix starts on a phrase line
   of both songs as well as swapping on one.
6. **Loudness through the mix, measured.** Apple is criticised for volume spikes; the harness now renders each
   transition through the real mixer and compares its loudest 3 s with either song's own. It stays within +0.8 LU
   (+1.5 LU with 32 s mixes), so the gain law was left as it is.

Left out: reordering the queue by key or energy (a library player keeps the queue; DJ.Studio's Harmonize does
this), stem separation (djay's Neural Mix; battery and model licences, `analysis.md`), a riser (Spotify's Rise
low-passes the incoming song into the drop; a matter of taste the harness cannot score, and the drop landing on
the swap already makes the arrival the event), an intro loop (needs a second decoder on the incoming song), a
separate energy score in Mixed In Key's manner (with a fixed order it could only change the transition, and the
drop landing already avoids mixing a full song into a bare intro), and keeping a vocal pickup before the incoming
song's first downbeat (the synthetic songs have none to test it on).

### 7.3 How it was measured

`eval.rs` gained a transition harness. Fifteen pairs of synthetic songs (twelve new songs: drum intros longer than
a mix, a pad-drums-drop build, codas, a sung coda over keys, hidden tracks after 37 s of silence with 4 and 16 bars
of music after it, silence at the ends of the files, singing to the very end and from the first bar; plus pairs
from the analysis corpus) are rendered, analysed as the app would, planned with the app's settings and scored
against the truth: where the incoming drop lands against the swap, the swap on the outgoing song's bars and phrase
lines, music skipped, silence heard, the run-up laid over a dead ending, two voices competing (both sounding and
neither 10 dB under the other across 500 Hz to 2 kHz, measured by running the real mixer with tones on one deck at
a time), and loudness. `landmark_eval` scores the drop and exit detection on all 29 songs.

```sh
cargo test --release -p nori-player transition_eval -- --ignored --nocapture
cargo test --release -p nori-player landmark_eval -- --ignored --nocapture
NORI_EVAL_VERBOSE=1 NORI_EVAL_ONLY=hidden,coda cargo test ...   # the plan's reason per pair; a subset
```

Before is the planner and mixer as they were, scored by the same harness **[measured]**:

| Default settings (16 s at most), 15 pairs | Before | After |
|---|---|---|
| Incoming drops the swap lands on | 4 of 14 | 8 (10 with 32 s mixes, against 5) |
| Incoming intro left on its own after the mix | 90.2 s | 54.6 s (32 s mixes: 88.7 to 42.0) |
| Swaps on a downbeat / a phrase line of the outgoing song | 10 / 4 of 11 | 12 / 12 of 12 |
| Mixes starting on a phrase line | 4 of 11 | 10 of 12 (12 with 32 s mixes) |
| Music skipped, outgoing / incoming | 48.7 / 1.8 s | 58.8 / 39.9 s (none over the cap) |
| Silence heard before the mix | 58.5 s | 37.5 s (all of it the hidden track too long to leave) |
| Run-up laid over a dead ending (a coda, a gap, silence) | 18.4 s | 0 |
| Voices competing, the analysis's own vocal gate | 14.5 of 23.1 s sung together | 12.1 of 21.1 s |
| Voices competing, voices from the truth | 2.1 of 3.6 s (three echo-outs) | 5.3 of 21.1 s (12.7 without the separation; one echo-out) |
| Louder than either song | +0.3 LU mean, +1.1 at most | +0.4, +0.8 |

Landmarks over the 29 songs: drops 14 of 16 found within a beat, one false alarm (the one-drop, read at double
tempo); exits 3 of 3, no false alarms. The analysis corpus's own numbers (section 3 of `analysis.md`) are
unchanged, and the analysis costs 57.9 ms per minute of audio against 57.3.

What the numbers do not show, and what must be listened to on a phone: whether landing the drop on the swap and
letting the outgoing song go a beat after it sounds like a DJ or like a jump on real records; whether the 18 dB
vocal duck and the high-pass ride sound like two singers handing over or like a filter; whether skipping up to
15 s of an instrumental intro is noticed; whether leaving on a closing breakdown or before a short hidden track is
welcome or feels like a song cut short.
