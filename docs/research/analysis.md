# Song analysis for AutoMix: what a phone can afford

Internal planning note, not published.

How AutoMix reads a song (beats, bars, tempo, key, sections, voices, loudness), what the best current methods
are, which of them an MIT-licensed app may ship, what they cost on a phone, and what was changed on the back of
this research. The design of AutoMix itself is in `automix.md`; this is the analysis underneath it.

Labels: **[verified]** = stated by a cited source; **[measured]** = measured for this document, with the command
given; **[inferred]** = my reasoning or estimate, not confirmed.

## Summary

- **Build first: the classical upgrades. Done.** A metrical comb for the tempo, a confidence that drops when a
  second, unrelated tempo scores as well, bars of three, downbeats read from how heavily the low end lands,
  the band's own tuning taken out before the key is read, a minor-key profile that suits pop, and section changes
  (Foote novelty on bars) for intros and outros. On the synthetic evaluation set the mix windows that are right
  and trusted went from 18 to 24 of 32, windows starting on the wrong beat of the bar from 6 to 2, keys from 11 to
  13 of 16 (every miss now a Camelot neighbour), intro and outro cues from 11 to 26 of 32. Nothing was trusted and
  wrong before or after. The cost went from 177 to 188 ms per four-minute track on one desktop core. **[measured]**
- **Shipped as an opt-in: Beat This! (small) for beats and downbeats, "Better beat detection".** It is the one
  state-of-the-art beat tracker whose code *and* weights are MIT; madmom's models are CC BY-NC-SA and Demucs's
  weights are for scientific use only. On GTZAN it scores 88.8 % beat F1 and 79.4 % downbeat F1 (the full model
  89.1 % and 78.3 %), where classical trackers of the kind nori uses score 55-66 %. **[verified]** The switch is
  off by default; on, it runs the small model (4.2 MB of fp16 weights) through tract (pure Rust, no second native
  runtime) over the first and last 30 s of the song playing and the next one, in nori-engine's measurer. The
  Android builds, release included, carry the feature and the model's graph (`-PrustFeatures=` leaves it out; section 5). The weights are made on the device from the authors' own checkpoint, fetched from their
  server when the switch goes on (7.1). Its grid replaces the classical one at an end when it is confident and its bar is settled
  (section 7). On the synthetic set the mix windows that are right and trusted go from 24 to 27 of 32 and none is
  trusted and wrong. **[measured]**
- **The small model, not the full one, and not quantised to int8.** Over the same windows its beats agree with the
  full fp32 model's at F 0.977 (downbeats 0.968) on synthetic songs and 0.867 (0.785) on the real clips, it runs a
  window in 4.3 s here against 7.2 s, and it is a 5 MB download against 42-83 MB. **[measured]**
- **The memory needed a change to the model file.** As the ONNX exporter writes it, the model's attention holds
  every frame-by-frame score matrix of a 30 s window, for 32 frequency rows at once: 700 MB at the peak. With each
  attention fused into one `Attention` node, which tract runs as flash attention, the same logits (within 1e-5)
  need about 100 MB. **[measured]**
- **It is not free.** tract makes the arm64 core library 15.5 MB bigger (9.8 to 25.3 MB; 6.2 MB more compressed),
  and with the model the perf APK grows by 20.6 MB (14.0 to 34.6 MB), which a build with the feature makes every
  install pay whether the switch is on or not. Each new song costs two windows of one core: 6.7 s on a Ryzen 5
  3600, perhaps 25-35 s on a mid-range phone's big core, and about 150 MB while it runs **[measured/inferred]**.
  Worth it only as an opt-in, and only in a build meant for it.
- **Not shown to be better on real records yet.** On ten songs of one real album (Radiohead's *Kid A*, scored
  against the full model's beats) the grid the app adopts moves no mix window from wrong to right (section 7.1).
- **Do not trust someone else's int8 export.** The quantised "small" Beat This! file BitChord ships gives beats only
  for the first seconds of some windows; the full model quantised by us does not do this. The shipped file was
  checked against the full model (above). **[measured]**
- **Vocals are where classical analysis is weakest**, and no cheap, clearly licensed model was verified. Open-Unmix
  UMX-HQ (vocals mask, 8.9 M parameters, 139 ms per 22 s window here) is the candidate if its weights' licence is
  confirmed; UMX-L is non-commercial.
- **Loudness needs nothing new.** BS.1770 and MixRamp are already exact.

## 1. What AutoMix reads, and where

`crates/player/src/automix/`: one streaming front end (`analysis.rs`: two FFTs per hop, K-weighted blocks) fed either
by the transition engine's tap on what is playing (only for songs that still need it) or by nori-engine's
`Measurer`, which decodes the songs coming up whole on a thread of the lowest priority, and only once their bytes
are on the device. The whole-song steps (`finish`) run once at the end; the rows are nori-automix's (`store.rs`).
The planner (`plan.rs`) reads:

| Field | Used for |
|---|---|
| intro and outro grids (tempo, first beat, stability, confidence, downbeat) | beat matching, measured over the first and last 40 s |
| whole-song grid | fallback when a window has too little music |
| `beats_per_bar` (new) | bar length; a 3/4 song never locks to a 4/4 one |
| `intro_end_ms`, `outro_start_ms` | where the exit starts (on a phrase), where the bass swap happens |
| key and its confidence | Camelot distance: blend length, filters, clash |
| intro and outro vocal share and centroid | vocal-on-vocal and timbre gates |
| LUFS, silence, MixRamp | loudness match, fallback fades |

The planner only beat-matches when both grids say they are confident and stable, so an analysis that doubts
itself costs a plainer fade, while one that is confidently wrong costs a train wreck. That asymmetry drives
everything below.

## 2. How it was measured

`crates/player/src/automix/eval.rs` renders 16 synthetic songs whose beats, downbeats, metre, key, sung notes and
sections are known exactly: kick, snare and hats, a bass line, chords and a sung line with vibrato and vowel
formants, in the styles that trip trackers up. Four on the floor, swing (0.62 and 0.66), a band drifting ±2 % with
12 ms timing spread, a band slowing from 96 to 88 BPM, an accelerando from 100 to 132, half-time, drum and bass, a
syncopated funk groove built on dotted eighths, a reggae one-drop, a waltz, a 45 s beatless pad intro, silences, a
ballad, and bands tuned 38 cents sharp and 30 flat. It scores them as the literature does (beat F-measure at ±70 ms,
tempo Acc1 within 4 % and Acc2 also allowing ×2, ×½, ×3, ×⅓, downbeat F-measure, key accuracy and the MIREX weighted
key score) and adds what the mix depends on: over the 30 s at each end, is the grid the planner would use right
(beats and downbeats, half or double tempo allowed since the planner folds octaves), and was it trusted?

```sh
cargo test --release -p nori-player analysis_eval -- --ignored --nocapture
NORI_EVAL_VERBOSE=1 NORI_EVAL_ONLY=waltz,funk cargo test ...   # a line of detail per song; a subset
NORI_REAL=<dir> cargo test ...                                 # also real recordings with reference beats
NORI_EVAL_DUMP=<dir> cargo test ...                            # write the songs as WAV for other trackers
```

**Real recordings.** Hosts with free music (Jamendo, Free Music Archive, archive.org, Wikimedia) were unreachable
from here; GitHub was not. librosa's example recordings are on GitHub with a licence note each, and five are CC BY
4.0 or public domain: Kevin MacLeod's *Vibe Ace* (jazz-electronic, 61 s) and *Dance of the Sugar Plum Fairy* (120 s),
Brahms' *Hungarian Dance No. 5* by a string orchestra (46 s, rubato), a trumpet playing with tempo variation (26 s)
and an accelerating snare (30 s). Four more are CC BY-NC and were left out. None has beat annotations, so the
reference beats are Beat This!'s own (the full model, fp32, run with a numpy copy of its front end): these numbers
are agreement with the state of the art, not accuracy, and at the end of *Vibe Ace* the reference itself goes
irregular.

**Beat This! on the synthetic songs.** The WAVs were run through the full model and scored the same way, as a
ceiling. It also runs inside the harness through tract (`neural_eval`, section 7).

## 3. Results

Synthetic songs: before this branch, after it, and with Beat This! (full model) instead of the classical tracker:

| | Before | After | Beat This! |
|---|---|---|---|
| Tracked beats, F (±70 ms) | 0.896 | 0.956 | 0.980 |
| Tempo Acc1 / Acc2 | 12 / 15 of 16 | 14 / 16 | 15 / 16 |
| Metre | 15 of 16 | 16 | 16 |
| Mix windows: grid F | 0.830 | 0.882 | 0.942 |
| Mix windows: downbeat F | 0.722 | 0.879 | 0.938 |
| Right and trusted (bar-locked) | 18 of 32 | 24 | 25 |
| Trusted, beats right, wrong beat of the bar | 6 | 2 | 2 |
| Trusted and wrong | 0 | 0 | 0 |
| Refused (a plain fade) | 8 | 6 | 5 |
| Key exact / MIREX / within one Camelot step | 11 / 0.756 / 14 of 16 | 13 / 0.869 / 16 | - |
| Intro and outro cues within a beat | 11 of 32 | 26 | - |
| Vocal gate right | 29 of 32 | 29 | - |

**[measured]** Beat This!'s "tracked beats" and "tempo" rows are its raw beats; its mix-window rows are its beats
fitted to AutoMix's constant grid inside the harness. The two wrong bars left are the one-drop, which both nori and
Beat This! read at 150 BPM, so every bar of theirs is half a real one: the conventional tempo of a one-drop is itself
a matter of taste. The refusals are the drifting band, the accelerando and (for nori) the band slowing down: one
constant grid cannot follow them, and saying so is correct (section 4.2).

Real recordings, against Beat This!'s beats:

| | Before | After |
|---|---|---|
| Tracked beats, F | 0.675 | 0.791 |
| Whole-song grid, F | 0.302 | 0.415 |
| Tempo Acc1 | 2 of 5 | 3 of 5 |
| Mix windows right and trusted / trusted and wrong / refused | 1 / 0 / 9 of 10 | 1 / 1 / 8 |

**[measured]** The gain is *Vibe Ace*: 101.6 BPM before (a beat and a quarter, which no octave folding repairs),
130.0 after, its tracked beats going from F 0.30 to 0.88. The one trusted-and-wrong window is its last 30 s, where
the grid holds 129.9 BPM while the reference beats turn irregular; which of the two is right there was not
settled. The orchestra, the trumpet and the accelerating snare are refused before and after, which is right.

**Cost.** Four minutes of 44.1 kHz mono: 177 ms before (173 front end, 6 finish), 188 ms after (177 and 11), on one
core of the Xeon (2.1 GHz, AVX-512) this was measured on (`cargo test --release -p nori-player analysis_cost --
--ignored --nocapture`). On a phone, decoding costs more than this. **[measured]**

## 4. Task by task

### 4.1 Beats and downbeats

**Neural, state of the art.**
- **Beat This!** (Foscarin, Schlüter, Widmer, ISMIR 2024): convolutions and transformers over a log-mel spectrogram,
  no DBN. GTZAN (unseen in training): 89.1 % beat F1 and 78.3 % downbeat F1; the small model 88.8 % and 79.4 %.
  Code and weights MIT; about 78 MB per full checkpoint and 8.1 MB per small one ([paper][bt-arxiv],
  [repository][bt-repo]). **[verified]** The full model as ONNX has 20.3 M parameters and uses Einsum, ScatterND,
  Erf, ReduceL2 and Sin/Cos (rotary embeddings) **[measured]**; an MIT ONNX export of it is in
  [beat_this_cpp][bt-cpp].
- **Böck and Davies' TCN** (ISMIR 2020), the madmom lineage: GTZAN 0.885 beat and 0.672 downbeat F-measure
  ([as tabulated in WaveBeat][wavebeat]). madmom's code is BSD, but **all its models are CC BY-NC-SA 4.0**
  ([madmom][madmom]): not shippable. **[verified]**
- **BeatNet** (online, particle filtering): 75.4 % beat and 46.7 % downbeat F1 on GTZAN; repository CC BY 4.0
  ([repo][beatnet], [BeatNet+][beatnetplus]). **[verified]** Built for zero latency, which AutoMix does not need.
- **All-In-One** (Kim and Nam 2023): beats, downbeats and labelled sections, but run on Demucs stems ([repo][allinone],
  code MIT), and Demucs's weights are "provided only for scientific purposes" ([issue #327][demucs-327]). Too heavy
  and not shippable. **[verified]**

**Classical.** In a comparison on GTZAN, Klapuri's tracker scored 65.5 % F, Degara's 65.3 %, Davies's 62.8 % and
Ellis's dynamic programme, which nori uses, 55.1 % ([Holzapfel et al.][selective]). **[verified]** That gap is real
music's, and a synthetic set cannot show it: on our songs the classical tracker already reaches 0.96.

**DBN or HMM decoding over a classical activation.** madmom's DBNs follow tempo changes and, in the bar-pointer
version, decode metre and downbeats jointly. That is worth having when beats are consumed one by one. AutoMix
consumes one constant grid per 40 s window, and refuses a window whose tempo moves (section 4.2), so a Viterbi path
with no change of metre or phase reduces to choosing one metre and one phase by summed evidence. The new downbeat
step does exactly that, over beats rather than frames, without a per-frame state space (thousands of states times
20,000 frames). The dynamic programme already follows a band's drift beat by beat (tracked-beat F 1.00 on the
drifting songs). **[inferred]** So the classical work went where the harness showed failures:

- **Metrical comb** (`tempo.rs`). Each candidate beat period is scored with the autocorrelation at twice and four
  times it (or three and six times), each read with its own tolerance. The strongest single lag used to win, and in
  drum and bass or a syncopated or swung groove that was a beat and a half or a beat and a quarter: 116 BPM for 174,
  139 for 104, 101.6 for 130. Not an octave, so the planner's folding could not repair it.
- **A tie lowers confidence.** When a tempo unrelated to the winner (not ×2, ×3, ×4 or their inverses) scores 95 to
  100 % as well, the confidence falls to nothing, so a coin flip gets a fade rather than a guess. It first counted
  from 80 %, which was wrong: in any groove with eighths, sixteenths or triplets the lags of 3/4, 5/4, 3/2 or 2/3 of
  a beat are multiples of the subdivision and score 85 to 95 % of the beat, so right, steady grids of syncopated
  songs lost their trust (two Radiohead songs on a phone from 0.88 and 0.61 to 0.12 and 0.04; *Optimistic*,
  *Idioteque* and *Morning Bell* from Kid A likewise), and four of five pairs of such songs in the harness got a
  fade instead of a beat-matched mix or a timed echo-out. It never refused a wrong grid: over 1,700 synthetic
  windows in nine styles (with a kick that moves from bar to bar, swing, and 3-12 ms of timing spread) a grid at
  such a ratio to the music never held still, and stability refused every one, the rival counted or not. With the
  syncopated songs in the set, the mix windows right and trusted go from 25 to 28 of 36, none trusted and wrong.
  **[measured]**
- **Metre** (`structure.rs`). 3/4 when each beat's rhythm (onsets on its quarters, how the low band rises on and
  between beats) and the chord changes repeat every three beats clearly better than every two or four. Stored as
  `beats_per_bar`; the planner builds bars from it and never locks a waltz to a four-beat song.
- **Downbeats from how heavily the low band lands.** The vote used the log-compressed low-band flux, which answers
  "how much did the band change relative to itself": a snare's faint low leakage into a band that had gone quiet
  read louder than a kick landing on a sustained bass note, so half-time bars started on the snare. The rise of the
  linear low-band level is the kick.

**BitChord.** It runs a quantised Beat This! "small" (2.1 M parameters, onnxruntime dynamic int8, 4.5 MB) over the
first and last 30 s. **[measured]** Fed the same spectrogram passage starting at different frames, that file finds
its beats in one placement and almost none in another: on *Vibe Ace*, 1.4 % of frames positive instead of about
20 %, and beats only in the first few seconds of most windows. The full model quantised the same way by us tracks
the song cleanly, so the fault is that export or the small model under that quantisation; the fp32 small checkpoint
was not reachable from here to tell which. Whatever ships must be compared against the full fp32 model on real
music first.

### 4.2 Tempo, and tempo that moves

Tempo is the beat period above; octave errors are harmless because the planner folds ×2 and ×½. Drift is the real
problem, and it is the mixer's as much as the analysis's: the stretcher holds one ratio across the overlap. The
analysis measures a grid over the first and last 40 s and reports it unstable when its two halves disagree by more
than about 1 %. On the harness that refuses the drifting band, the accelerando and the band slowing down; the last
one's grids were in fact right over 30 s (F 0.99), but its tempo moves 2 % across the 40 s window. **[measured]**
Following drift would need a tempo map in the stretcher (a ratio that moves beat by beat) and a grid made of the
tracked beats instead of a straight line. Beat This! would help there, since it assumes no constant tempo at all.
Not done here.

### 4.3 Key

**Neural.** CNN key classifiers ([Korzeniowski and Widmer 2018][keycnn]) beat template matching, most of all on dance
music, but the one shipped with madmom is CC BY-NC-SA, and no permissively licensed one was found. **[verified]**

**Classical, now** (`analysis.rs`, `structure.rs`).
- **Tuning.** Every chroma frame's spectral peaks between 250 Hz and 2.5 kHz, their frequency refined by a parabola
  through the log magnitudes, vote for how far they sit from the equal-tempered grid; the circular mean is the
  tuning, and a 10-cent pitch-class profile summed over the song is folded to 12 around it. On synthetic bands at
  +40, -40 and 0 cents it measures +39.8, -40.0 and -0.2. Before, a band 38 cents sharp read a semitone high (G minor
  as G sharp minor, five steps round the Camelot wheel, which the planner treats as a clash). **[measured]**
- **Profiles.** Temperley's major (it keeps third partials from reading as the dominant) and Krumhansl-Kessler's
  minor. Temperley's minor is the harmonic minor, leading tone 4 and flat seventh 1.5, while minor-key pop and dance
  music is mostly Aeolian ([Faraldo et al.][faraldo]); it read three of five natural-minor songs as their relative
  major. Krumhansl-Kessler's major on its own heard the dominant in bare triads, and taking the third and fifth
  partials out of the spectrum cost more than it fixed. **[measured]**
- For the mix, a relative key or a fifth is a Camelot neighbour, which the planner treats as compatible. All 16
  synthetic keys now land within one step (14 before).

### 4.4 Phrases, intros and outros

The planner starts the exit on the outgoing song's outro phrase and swaps the bass where the incoming intro ends.
The old rule looked for a 3 dB level jump (6 dB in the low band) on 8-bar lines, and missed the most common DJ
structure outright: in a drums-only house intro the kick carries nearly all the level, and the bass and chords
arriving change it by a fraction of a decibel. **[measured]**

Now (`section_changes`): six features per bar (level, low band, tonal energy from the chroma, onset strength,
brightness, voice-band share), each on its own spread across the song but never finer than half a change one would
notice, compared over the 4 bars either side of every 4-bar line: Foote's novelty, on bars rather than frames. The
intro ends at the first clear change in the first 40 % that does not thin the music out; the outro starts at the
last one in the second half that does not fill it up; a beatless opening ends where the beat starts. Without a
stable grid the same runs on 2 s blocks and snaps to the nearest tracked beat. Cues within a beat: 11 of 32 before,
26 after. **[measured]** Labelled sections (verse, chorus) would need a model like All-In-One; AutoMix only needs
where the intro ends and the outro starts.

### 4.5 Vocal activity

AutoMix's vocal gate reads the share of power between 300 Hz and 3.4 kHz. It cannot tell a voice from a pad: the
beatless pad intro reads 0.74, the sung sections 0.25 to 0.40. **[measured]** Unchanged on this branch: nothing
classical and cheap was convincing without real songs to tune it on.

- **Classical.** Lehner, Widmer and Sonnleitner's fluctogram (vibrato-like pitch movement per band), spectral
  flatness, contraction and vocal variance reached 82.2 % on Jamendo, 88.2 % with a random forest
  ([paper][lehner]). **[verified]** It needs a trained classifier and annotated songs.
- **Open-Unmix** vocals masks: UMX-L's weights are CC BY-NC-SA ([README][umx]); UMX-HQ's are on Zenodo and BitChord
  states they are MIT, **not verified here**. BitChord's int8 UMX-HQ vocals model (8.9 M parameters, an LSTM over a
  2049-bin stereo STFT) takes 139 ms for a 22 s window on one core here **[measured]**, but uses onnxruntime's own
  DynamicQuantizeLSTM operator, so it needs onnxruntime or a clean fp32 export.
- **Spleeter**: code MIT, a 2-stem vocals model in TensorFlow ([README][spleeter]); heavier. **Demucs**: weights for
  scientific use only. **Essentia**'s voice classifiers: non-commercial models, and the library is AGPL.
- **YAMNet** (MobileNet v1, 16 kHz, 0.96 s frames, Apache-2.0) has AudioSet classes "Singing" and "Vocal music". It is
  small and made for phones, but its balanced AudioSet mAP over all classes is 0.306, and how well "Singing" works
  over a full mix is unknown ([YAMNet][yamnet]). **[verified]**

A vocals-to-mix energy ratio from a separation mask is the most direct signal for "two voices will overlap". So:
confirm UMX-HQ's licence, export it to fp32 ONNX, check it runs in tract (LSTM), and evaluate it against sung and
instrumental labels on real songs before it replaces the band share.

### 4.6 Energy and loudness

BS.1770 integrated loudness with gating, silence trims and MixRamp points are computed exactly while the audio
streams past (`loudness.rs`). Nothing here needs a model. The per-bar section features of 4.4 are the "energy level"
a DJ tool shows.

## 5. Running a model from Rust on Android

| Runtime | What it is | Measured or known here |
|---|---|---|
| **tract** | Pure-Rust ONNX and NNEF inference (Sonos), MIT/Apache-2.0 | Runs Beat This! (full, fp32) with outputs within 2.4e-5 of onnxruntime; 5.3 s per 30 s window against onnxruntime's 1.9 s, one thread, x86 with AVX-512; loads and optimises in 1.2 s. Built with `neural-beats`, the core library grows from 4.0 to 18.4 MB for arm64 (1.97 to 7.22 MB compressed; the text from 3.0 to 14.1 MB, the relocated read-only data from 46 to 568 kB) and from 4.9 to 23.9 MB for x86_64 (2.3 to 8.6 MB compressed), all in the release profile (fat LTO, stripped). Loading NNEF instead of ONNX would save 3.7 MB of that (a stand-alone arm64 library: 14.4 MB with the ONNX front end, 10.6 MB with only NNEF), but tract 0.23 cannot yet write the model's exact GELU to NNEF. Does not load onnxruntime's dynamic-int8 export (its ConvInteger mixes u8 and i8). The 58 crates the feature adds to the Android build are all MIT, Apache-2.0 or both (a few also Unlicense, BSD-2-Clause or BlueOak). **[measured]** |
| **ort** (onnxruntime) | C++ runtime with Java and Rust bindings | Fastest here. The Android AAR 1.30.0 carries a 33.0 MB `libonnxruntime.so` for arm64 (12.4 MB compressed) **[measured]**; a reduced build needs building ONNX Runtime ourselves. XNNPACK and QNN execution providers for ARM CPUs and Qualcomm NPUs. |
| **candle** | Hugging Face's Rust framework | `candle-onnx` evaluates a subset of ONNX ops; not tested on this graph. |
| **burn** | Rust framework; `burn-import` turns ONNX into Rust code at build time | Op coverage for this graph not tested. |

NNAPI, which onnxruntime and TensorFlow Lite used to reach accelerators, is deprecated from Android 15
([Android][nnapi]). **[verified]** For a 2 to 20 M parameter model run a few seconds per song, one CPU core is the
honest target.

**Chosen: tract.** One Rust dependency in the core that already holds the DSP, no second native library to package
for every ABI, and the model sees the same PCM the analyser sees. It is 2 to 3 times slower than onnxruntime on x86
**[measured]**; on ARM, where tract has hand-written NEON kernels, it may differ **[inferred]**. If device timing
says tract is too slow, onnxruntime through `ort` is the fallback.

**The size is the price.** Built in, every install carries the library's 15.7 MB more, and Android stores native
libraries uncompressed in the APK, so the arm64 APK grows by that much; the weights are not in it (7.1). Measured on
the NDK build of September 2026 (`./gradlew :app:assemblePerf -PrustTargets=arm64-v8a`, sizes read from the APK):
`libnorimusic.so` 9,615,584 bytes without the feature and 25,314,080 with it (the graph's 186 kB and the checkpoint
reader included), the APK 14,150,663 and 29,849,159 bytes. While the weights were bundled, the same build was
34,705,148 bytes (the library 25,099,776 and the 5,069,707-byte model stored beside it). **[measured]** So the
every Android build has the feature, release included (the owner's call, 2026-09-26; `core/build.gradle.kts`: an empty
`-PrustFeatures=` takes it out of any build). The setting is only shown by a build that has it: without it nothing of tract is compiled or linked. In a build with it, nothing else is paid while the
switch is off: the code is mapped, not run, and the model is never loaded; the dynamic linker does relocate 0.5 MB
more of read-only data when the library opens. Keeping tract in a second library loaded only when the switch is on
would not shrink the APK, which is where the cost is; NNEF would save a quarter of it, once tract can write the
model's GELU (above); storing the libraries compressed (`useLegacyPackaging`) would halve the download but keep an
extracted copy on the phone as well. None of these was done. **[measured/inferred]**

**Phone cost [inferred].** A mid-range phone's big core (Cortex-A78 class, 128-bit NEON) has roughly a quarter of the
fp32 throughput of the core measured here. The small model as shipped takes 4.3 s per 30 s window here, so about
17 s a window and 35 s a song on such a core, more on a little core, which is where a thread of the lowest priority
often runs. A big core at 1 to 1.5 W for 35 s is 35-50 J, 0.06-0.09 % of a 4,500 mAh battery per song, once per
song; an hour of songs never heard before (15 of them) about 1 %. Decoding a song again for the model, when its
classical analysis was already stored, is about a second more. All of this has to be measured on a device.

## 6. When it can run without hurting battery

What was built follows these rules:

- **Only two windows per song, once.** AutoMix mixes over the first and last half minute; the model never reads the
  middle. The measurer keeps the first and last 35 s of a song while it decodes it (`beats::Ends`: mono, averaged to
  about 22 kHz as the model's front end would, 3.4 MB each, sized once, only with the switch on), cuts each end to
  the music's first or last 30 s using the silence trims already stored (`beats::window`), runs the model and stores
  the end (`Core::analysis_neural_store`). Each end records that the model has looked at it (`intro_grid_source`,
  `outro_grid_source`), so a song is never read twice, and a later classical measurement of the same file keeps
  what the model found (`beats::carry`).
- **Reuse the decode when there is one.** A song not yet measured is decoded once for both the classical analysis
  and the model. A song measured before the switch went on is decoded again, whole, from the device: about a second
  of CPU, cheap next to the model, and the only way its ends are counted in the same frames as the analysis.
- **Only the song playing and the next one.** The songs coming up are measured ahead as before, the model only for
  the first two. (The branch this was first written on also read downloaded songs while the phone charged, a
  JobScheduler job; that was not carried into the Rust measurer.)
- **Never on the audio thread, one song at a time, at the lowest priority, and off means off.** The measurer's
  thread runs at the lowest priority and only while there is something new to look at; flash attention stays on
  that thread (no rayon pool). The model is fetched and loaded when a look first needs it and let go when the
  thread ends. With the switch off nothing of this exists: no model, no download, no copies of the ends; and a
  build without the `neural-beats` feature has no tract in it at all.
- **Classical first, model second.** The classical grid is still measured; the model replaces an end's grid only
  when it is confident, and a failed or missing model leaves the classical answer.

## 7. Recommendation and plan

1. **Done** (the analysis version moved, so stored rows are measured again): the classical upgrades of section 4,
   with the evaluation harness and tests. Stored rows gained `beats_per_bar`. No Kotlin changes.
2. **Done: Beat This! small, behind a setting** ("Better beat detection" under AutoMix, off by default, a 5 MB
   download). See 7.1 for how the model was chosen, 6 for when it runs, 5 for what it costs.
   - nori-player (`automix/beats.rs`, always built; `automix/neural.rs`, feature `neural-beats`): the model over
     one end's samples gives beats and downbeats (`beats::read`); the grid fitted to them replaces the stored one
     at that end when the planner would trust it and its bar is settled (`beats::merge`).
     Beat This! sometimes marks every other beat as a downbeat (drum-only passages, where the bar is not in the
     sound); its bar is then decided by the classical grid's when both have the same tempo and metre and the
     classical bar starts on one of the two candidates, and otherwise the classical grid stays. Each end keeps its
     own metre (`intro_beats_per_bar`, `outro_beats_per_bar`). No new analysis version: stored rows read as not yet
     heard by the model.
   - nori-engine (`Measurer`, feature `neural-beats`): the ends of the song playing and the next, the model loaded
     for the measuring thread's life. nori-core (`model_download.rs`): the authors' checkpoint fetched through the
     platform's transport from their server, Wi-Fi unless mobile data is allowed, its SHA-256 checked, converted
     into the weights file and that checked against its own pin, kept beside the app's database, tried again at
     the next song when it failed. nori-player (`automix/checkpoint.rs`, `automix/weights.rs`): the checkpoint's
     zip and pickle read by a restricted unpickler (values, `OrderedDict`, tensor storages and
     `_rebuild_tensor_v2`; any other opcode or global is refused), and the graph's recipes followed. nori-automix
     (`beat_model.rs`): the pins and where the file is, deleted when the switch goes off. nori-settings: the two
     switches and their rows, shown only in a build with the model. No Kotlin besides the two fields its copy of
     the settings carries.
   - Every Android build has `neural-beats`, release included (section 5). `cargo build -p nori-cli --features neural-beats` for the desktop. Every client takes the same path, and
     no one ships or hosts a copy of the weights.
   - The graph is made by `tools/beat-this/export.py` from the MIT code and checkpoint (7.1):
     `crates/player/models/beat-this-small0.graph.onnx`, 186,116 bytes, every initializer external data with the
     recipe that makes it from the state_dict. The weights file made from the checkpoint is 4,229,216 bytes,
     SHA-256 `e9349da04b9da4ad41c5e416c71a9471af3a416249e7addef0101b3d569df5a7`, the same from the Rust
     conversion and from export.py's numpy copy of it.
   - Measured: on the synthetic set, mix windows right and trusted 24 to 27 of 32, trusted with the bar on the
     wrong beat 2 to 0, refused 6 to 5, trusted and wrong still none. On the five real clips the windows come out as before (1 right,
     1 trusted and wrong at the end of *Vibe Ace* where the reference itself turns irregular, 8 refused). Expected
     on real music at large: beat F1 in the high 80s and downbeat F1 near 80 % against the 55 to 66 % of classical
     trackers on GTZAN **[verified figures, from other datasets and settings]**.

### 7.1 Choosing the model

Every variant was run where the app runs it, in tract over the ends of the harness's songs and the five real clips,
and compared with the full fp32 model (final0, beat_this_cpp's export) over the same windows. `NORI_BEAT_THIS=<file>
NORI_BEAT_THIS_REF=<full model> NORI_REAL=<dir> cargo test --release -p nori-player --features neural-beats
neural_eval -- --ignored --nocapture`. Times are one thread of the Xeon above, on a machine that was busy with other
builds (±10 %). **[measured]**

| | Classical | Full (final0) | Small (small0), as shipped |
|---|---|---|---|
| Parameters, file | - | 20.3 M, 83 MB fp32 (42.6 MB fp16) | 2.1 M, 9.3 MB fp32, 5.07 MB fp16 |
| Synthetic mix windows: right and trusted / wrong bar / wrong / refused | 24 / 2 / 0 / 6 | 25 / 2 / 0 / 5 | 27 / 0 / 0 / 5 |
| Synthetic windows, agreement with the full model: beats F / downbeats F | - | - | 0.977 / 0.968 (31 windows with beats) |
| Real clips: right and trusted / trusted and wrong / refused | 1 / 1 / 8 | 1 / 1 / 8 | 1 / 1 / 8 |
| Real windows, agreement with the full model | - | - | 0.867 / 0.785 (10 windows) |
| One 30 s window, attention spelled out | - | 5.9 s, 773 MB peak | 3.3 s, 655 MB peak |
| One 30 s window, attention fused (flash, one thread) | - | 7.2 s, 235 MB | 4.3 s, 120 MB |
| Load and optimise | - | 0.6 s | 0.3 s |

- **Small, not full.** It agrees with the full model where the full model is sure of itself (synthetic windows:
  0.977), and where the music is hard (rubato strings, a trumpet, an accelerating snare) they both wander and the
  planner refuses both. On the one-drop the small model reads 75 BPM, the full one 150, so its bars are a real
  bar's where the full model's are half of one. It is 1.7 times faster, and its file a sixteenth of the full
  model's (an eighth of it in fp16).
- **Not erratic.** BitChord's int8 small export found beats only in the first seconds of a window. Ours finds beats
  through every window: as many as the full model on the synthetic songs (within one, bar the one-drop it reads at
  half the tempo), and more or fewer only where the music is hard (the orchestra's intro 102 against 76, the
  accelerating snare 74 against 60, the trumpet 35 against 39).
- **fp16 weights, not int8.** fp16 storage (a Cast back to fp32 in front of each weight; tract computes in fp32)
  moves the logits by at most 0.01 on a real spectrogram and no frame changes side of zero. Weight-only int8 with a
  scale per output channel would save 2 MB more (3.1 MB) but flips 11 frames of 3000 on the same input (4 with the
  full model); onnxruntime's dynamic int8 does not load in tract at all.
- **Fused attention, not shorter chunks.** The spelled-out attention's 700 MB is not something to ask of a phone.
  Running each window in shorter chunks, as the authors' inference splits a song, also cuts it, but costs context:
  over 512-frame chunks (149 MB, 1.9 s) agreement with the full model fell to 0.966 / 0.930 on the synthetic
  windows and 0.789 / 0.742 on the real ones, and the Nutcracker's outro was trusted with its bar on the wrong
  beat; 762 and 1012 frames fell less (0.812 / 0.692 and 0.855 / 0.758 on real windows). The fused file keeps the
  whole window in one pass with the same logits (within 1e-5) for 30 % more time.
- **Where the file comes from (September 2026).** `tools/beat-this/export.py`, run as its header says: the code of
  CPJKU/beat_this at b95c8ab0c58c (`LICENSE`: MIT, Copyright (c) 2024 Institute of Computational Perception, JKU
  Linz; the README: "The code and the published model weights are released under the MIT license", with a note
  that some of the training data is not, which does not bind the weights), the `small0` checkpoint fetched from
  the authors' server (cloud.cp.jku.at), SHA-256 `6074be2c4d490c5f6101fcc374a1ec72ae93456e23bb6019783b849f5dc7d47b`
  as the script pins it, 2,099,960 parameters; PyTorch 2.8.0 (CPU), onnx 1.19.0, rotary-embedding-torch 0.6.4 and
  einops 0.8.0 on Python 3.13. The fp16 export is within 0.0012 of PyTorch's logits on the test input, no frame on
  the other side of zero, and 12 attentions are fused. The output, `beat-this-small0-v1.onnx`, is 5,069,707 bytes,
  SHA-256 `847b51aaef519a60a47c815fa58440782de73bff7000210396673b0353e2cc8c`, the same bytes on a second run. It
  is 8 bytes short of the file pinned before (5,069,715 bytes, built from two public exports of the same weights
  without PyTorch), which was never published and is not kept, so the two could not be compared. Two things in
  the script had to change: rotary-embedding-torch is pinned to the version beat_this's `requirements.txt` names
  (0.9.1 traces another graph, 1.2 MB bigger), and the PyTorch reference is taken from a freshly loaded model
  (tracing leaves its sizes in the rotary embedding's cache, and the traced model then answers with logits off by
  up to 7, which made the check fail although the file is right). **[measured]**
- **Weights from the authors, not from us (September 2026).** That file was shipped inside the debug and perf APKs,
  and the address the core would have downloaded it from was never filled (a release asset that answered 404). Now
  no copy of the weights is shipped or hosted: export.py splits the same export into the graph and a recipe per
  weight (copy, transpose, a Conv2d with its BatchNorm2d folded in, the bias that fold leaves; fp16 where the file
  had it; `--full` still writes the whole file, the same 847b51aa... bytes, run again to check), and the core makes
  the weights on the device from the checkpoint it fetches from the authors' server. PyTorch's fold uses a square
  root that is not correctly rounded for 8 of the stem's 32 channels, so the conversion (IEEE float32 step for
  step, the same bytes on every platform and in export.py's numpy) differs from the shipped file in 130 of
  2,099,736 values, each by one unit in the last place. Run over the same windows in tract
  (`NORI_BEAT_THIS_CKPT=small0.ckpt NORI_BEAT_THIS=<the shipped file> cargo test --release -p nori-player --features
  neural-beats official_weights`): on export.py's kind of spectrogram-like input the largest logit difference is
  4.5e-6, on a synthetic drum loop through the app's front end 3.6e-5, no frame on the other side of zero, and the
  same 63 beats and 29 downbeats. The whole path on the Ryzen: the checkpoint (8,451,101 bytes) fetched in 0.44 s,
  converted in 37-42 ms, peak RSS 31 MB above the process's before it (the checkpoint, its tensors and the weights
  at once), and the model assembled and loaded in 0.23 s, 65 MB peak for the process. The measurer test
  (`NORI_BEAT_THIS_CKPT=small0.ckpt cargo test --release -p nori-engine --features neural-beats --test core`)
  serves the checkpoint at the authors' address and passes; `cargo test --release -p nori-cli
  --features neural-beats --test beat_model -- --ignored` fetches it from their server. **[measured]**
- **Checked again with the exported file (September 2026, a Ryzen 5 3600, one thread).** The full model's
  reference is final0 exported the same way (fp32, attention fused). The synthetic set has 18 songs now (36 mix
  windows); the real songs are the ten of Radiohead's *Kid A* (FLAC), with the full model's own beats and
  downbeats (PyTorch, no DBN, over the whole song) as the reference, so they measure agreement with the big model
  more than truth, on an album that is hard for any tracker (*Treefingers* has no beat, *Motion Picture
  Soundtrack* barely one). `analysis_eval` for the classical rows, `neural_eval` for the others, with `NORI_REAL`.
  **[measured]**

| Mix windows: bar-locked / wrong bar / wrong / refused (grid F, downbeat F) | Synthetic, 36 windows | *Kid A*, 16 windows |
|---|---|---|
| Classical | 28 / 2 / 0 / 6 (0.895, 0.892) | 6 / 2 / 3 / 5 (0.730, 0.477) |
| Beat This! small, as shipped (adopted where sure) | 31 / 0 / 0 / 5 (0.931, 0.924) | 6 / 2 / 3 / 5 (0.709, 0.456) |
| Beat This! small wherever it has a grid | 31 / 0 / 0 / 5 (0.972, 0.969) | 7 / 3 / 2 / 4 (0.785, 0.555) |
| Agreement of small with full, beats F / downbeats F | 0.953 / 0.943 | 0.662 / 0.673 (20 windows) |

  On the synthetic songs the model does what it did before: three more windows right, the two with the bar on the
  wrong beat fixed, nothing trusted and wrong. On *Kid A* the gate that decides when its grid replaces the
  classical one lets almost nothing through: one window changes (*Optimistic*'s intro, still bar-locked, F 1.00 to
  0.67), and the counts stay those of the classical tracker. Its grid everywhere would be better on F (beats 0.73 to
  0.79, downbeats 0.48 to 0.56) and turn one trusted-and-wrong window into a right one, but put one more bar on the
  wrong beat. The small model agrees with the full one far less on these records than on the synthetic songs. So on
  this album it is not better as shipped; whether it is on music with a steady beat has to be measured on a
  labelled set (GTZAN, Ballroom, or the user's own library with hand-tapped beats).
- **Time and memory per song (September 2026, Ryzen 5 3600, release, one thread).** `neural_song_cost`: loading
  and optimising the model from its bytes 0.20-0.27 s, each 30 s window 3.2 s, the whole song 6.6-6.7 s (three songs
  of *Kid A*); the process's peak RSS grows by about 150 MB over it (72 to 220 MB, the song's own PCM included in
  the first number). `neural_eval` over its 56 windows: 3.27 s a window. **[measured]**
3. **Vocals**: verify UMX-HQ's weights licence; if it is MIT, the same path (fp32 ONNX, tract, two windows) gives a
   vocals-to-mix ratio for the gate. Measure it against labelled songs first.
4. **Not recommended**: madmom's models, Essentia, UMX-L, Demucs (licences); NNAPI (deprecated); separating whole
   songs into stems (cost); shipping a quantised model nobody compared with the original.

## 8. What remains unverified

- Accuracy on real music at scale. Five short CC clips and one album with a model's beats as the reference are a
  sanity check, not a measurement, and the synthetic songs are cleaner than records.
- Anything on a phone: the classical analysis after this branch (+6 % on the desktop), tract's and onnxruntime's
  speed on ARM, the model's time and energy per song, its memory under Android. The arm64 size was measured on a
  Linux arm64 build of the same code, not an NDK one (no Android NDK here).
- "Better beat detection" on a phone: the debug and perf builds ship the model, but its time, energy and memory
  have not been measured on a phone. The measurer's path is tested on the desktop without the model
  (`crates/engine/tests/core.rs`) and with one when `NORI_BEAT_THIS` names it, including reading it from inside
  a package.
- Whether the model beats the classical tracker on real music with a steady beat (7.1: on *Kid A* it does not, as
  shipped); and UMX-HQ's licence.
- The key profiles were chosen on synthetic chords. Of the real clips only the *Sugar Plum Fairy* had a key worth
  checking against (E minor, if the arrangement keeps Tchaikovsky's); it reads B minor, a fifth away, before and
  after. The Brahms is "in F sharp minor" by its file name, but string-orchestra arrangements are not always in the
  original key, so it was left out.

## Sources

- Foscarin, Schlüter, Widmer, "Beat this! Accurate beat tracking without DBN postprocessing", ISMIR 2024: [arXiv][bt-arxiv]; code, models and licence: [CPJKU/beat_this][bt-repo]; ONNX export: [beat_this_cpp][bt-cpp]
- Böck and Davies' TCN results as tabulated in Steinmetz and Reiss, "WaveBeat": [arXiv][wavebeat]
- madmom, licence of models: [CPJKU/madmom][madmom]
- BeatNet: [mjhydri/BeatNet][beatnet]; BeatNet+: [TISMIR][beatnetplus]
- All-In-One: [mir-aidj/all-in-one][allinone]; Demucs weights: [issue #327][demucs-327]
- Holzapfel et al., "Selective Sampling for Beat Tracking Evaluation": [ResearchGate][selective]
- Korzeniowski and Widmer, "Genre-agnostic key classification with convolutional neural networks": [arXiv][keycnn]
- Faraldo, Gómez, Jordà, Herrera, "Key estimation in electronic dance music", ECIR 2016: [Springer][faraldo]
- Lehner, Widmer, Sonnleitner, "On the reduction of false positives in singing voice detection", ICASSP 2014: [ResearchGate][lehner]
- Open-Unmix model licences: [README][umx]; Spleeter: [README][spleeter]; YAMNet: [tensorflow/models][yamnet]
- NNAPI deprecation: [Android NDK][nnapi]
- librosa's example recordings and their licences: [librosa/data][librosa-data]

[bt-arxiv]: https://arxiv.org/abs/2407.21658
[bt-repo]: https://github.com/CPJKU/beat_this
[bt-cpp]: https://github.com/mosynthkey/beat_this_cpp
[wavebeat]: https://arxiv.org/pdf/2110.01436
[madmom]: https://github.com/CPJKU/madmom
[beatnet]: https://github.com/mjhydri/BeatNet
[beatnetplus]: https://transactions.ismir.net/articles/10.5334/tismir.198
[allinone]: https://github.com/mir-aidj/all-in-one
[demucs-327]: https://github.com/facebookresearch/demucs/issues/327
[selective]: https://www.researchgate.net/publication/260691355_Selective_Sampling_for_Beat_Tracking_Evaluation
[keycnn]: https://arxiv.org/abs/1808.05340
[faraldo]: https://link.springer.com/chapter/10.1007/978-3-319-30671-1_25
[lehner]: https://www.researchgate.net/publication/262259403_On_the_reduction_of_false_positives_in_singing_voice_detection
[umx]: https://github.com/sigsep/open-unmix-pytorch
[spleeter]: https://github.com/deezer/spleeter
[yamnet]: https://github.com/tensorflow/models/tree/master/research/audioset/yamnet
[nnapi]: https://developer.android.com/ndk/guides/neuralnetworks/migration-guide
[librosa-data]: https://github.com/librosa/data
