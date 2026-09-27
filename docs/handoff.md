# Handoff: where the work stopped

Read `AGENTS.md` first — it holds the house rules; this file only covers what is in flight and the
traps that make this app easy to test wrongly.

## The one-paragraph summary

The app works and is tested end to end against the owner's real server (`tools/audio-e2e.sh`,
`tools/feature-e2e.sh`). The Apple Music comparison that the previous session started has been
carried out: the full-screen player, the lyrics view and the album page were measured against
Apple's own App Store screenshots and the differences closed. What is left is listed below.

## Recently closed

- **Sound extras, on top of the graphic equalizer and effects (branch `feat/sound-extras`).** All in
  nori-player, tested there; settings are `StoredPrefs` fields kept in sound profiles.
  - **Crossfeed presets and cutoff**: bs2b's Default (700 Hz, 4.5 dB), Chu Moy (700, 6) and Jan Meier (650,
    9.5) as chips on the equalizer screen, a cutoff slider (300 to 2000 Hz, `crossfeedHz`) under the level
    (now up to 12 dB). `crossfeedPreset` is a special name (OFF, DEFAULT, CHU_MOY, JAN_MEIER; "" is custom).
    bs2b's own design leaves centred sound up to 1.8 dB darker above the cutoff; that is bs2b, not a bug.
  - **No processing on an output** (`soundBypass`, the profile's `bypass`): a switch on the sound page and a
    "No processing" choice in each device's sheet (a reserved profile, like "Flat"). The chain is the
    identity (`nori-engine core::settings`), `sound_chain_on` is false, so offload comes back. ReplayGain and
    transitions are not the chain's and stay.
  - **Crossfade curves** (`crossfadeCurve`: equal power, the default and what plain crossfades always
    used; linear; S-curve = sin²) and separate fade-in and fade-out lengths within the crossfade
    (`crossfadeInSec`, `crossfadeOutSec`, 0 = all of it), in `transitions::shape_crossfade`, applied to
    plain (blind) crossfades only; AutoMix keeps its own curves. "Mix only" was not added: crossfading
    across albums and never inside an album played in order is exactly "Keep albums gapless"
    (`crossfadeKeepAlbums`), and a skip already has its own dip (`fadeMs`).
  - **Compressor meter** on the Effects section (the core's `compression_db` through the engine's status
    and a `compressionDb` door), read every `stage.meterMs` only while the settings page is resumed. Like
    the limiter's meter it is what the last buffer through the chain took off, so with bursts it moves
    once per burst. **Noise gate**: a downward expander (`expander`, threshold, ratio 1:1 to 1:20, attack,
    release) sharing the compressor's gain computer, before it in the chain; off by default.
  - **Five-band graphic layout** on the ISO centres 63, 250, 1k, 4k, 16k (every other band of the ten),
    bells 1.25 spacings wide: 0.2 dB at the centres, 0.7 between, a ±12 zigzag 0.34 off; about 2 dB rms
    against real AutoEQ corrections (the ten: 1 to 1.3).
  - **Loudness compensation** (`loudness`, `loudnessRefPhon`, off by default): ISO 226:2003 contours
    (`contour.rs`), the difference between the reference level (80 phon at full volume) and the level
    the volume leaves, drawn by a low and a high shelf fitted by least squares (within 0.3 to 2 dB down to
    40 phon), with a pre-gain that pays the boost back so it never clips. Kotlin's `VolumeWatch` listens
    (a ContentObserver on the system settings, only while it is on) and hands the step, the maximum and
    `getStreamVolumeDb` to the `setVolume` door; the core applies it only when it moves the sound by a
    quarter dB. The terminal client follows its own volume. **Not checked on a phone**: whether
    `getStreamVolumeDb` answers sensibly over Bluetooth absolute volume (0 dB below the top step is read
    by the core's own curve instead), and how the lower middle feels at low volume.

- **The lyrics sync check hears the middle of the stereo image.** The vocal curve (nori-player automix/vocal.rs)
  of a stereo song is measured on its centre: the side rides in the analysis FFT's imaginary part, and each
  voice-band bin counts by how alike the channels are there (nothing under 0.6), so panned guitars drop out. On
  a synthetic metal band sung/unsung AUC went from 0.56 (downmix) to 0.95; about 6 % more analysis CPU. Mono
  songs, and stereo ones with the same channels, give the same curve to the bit; the rest of the analysis reads
  the downmix as before. `ANALYSIS_VERSION` is 12, so songs are measured again as they come; their old curves
  are read until then. **Not measured on real songs**: `sync_tune.rs` needs the library it was tuned on
  (`~/.music.pass`); run `sync_gather` then `sync_real` in a fresh `NORI_TUNE_DIR` and compare with the numbers
  in vocal.rs's header (metal about 0.5, pop/rock/rap about 0.7).

- **Ported onto the Rust core: the lyrics, moving-cover and player work of `claude/lyrics-motion-artwork`.**
  That branch was written on master before the rewrite, with the lyrics services and Apple's catalogue
  asked from Kotlin. Here everything that computes is Rust, and Kotlin only draws and carries bytes:
  - **Lyrics services, asked together.** Sixteen services, ranked in Settings → Lyrics (held and dragged;
    only the switch turns one off), with "Prefer word-by-word lyrics" and fields for a PaxSenix and a
    BetterLyrics key. Out of the box nothing is asked ("Look things up online" is off); under it only the
    open LRCLIB and Unison are on, and the fourteen others stay off until switched on. The service table
    and the ranking are nori-settings' (`lyrics_sources.rs`, stored as `lyricsOrder` and `lyricsOn`); the
    requests, matching and answers nori-lyrics' (`services.rs`); the race `race.rs`: what each service
    answered before is read from the response cache first (a hit for good, a miss for a week), the rest
    are asked six at a time, each request cut off at 6 s and each service at 12 s (30 s for the keyed
    PaxSenix routes), a finer answer shown as soon as it comes, and everything dropped once nobody still
    out could beat the answer in hand. A service that failed is not asked about the same song again for
    half an hour, and one that fails three songs in a row rests ten minutes: nothing is hammered. It is
    one future on no thread of its own, polled by the caller (`Client::lyrics_lookup`, handing each pick
    to Kotlin's `LyricsShown`); leaving the lyrics cancels it. The core's `Transport` gained `send`, a
    third party's headers and a JSON body (NetEase's Referer, the keys, YouTube Music's player API);
    OkHttp still only carries the bytes. The cache is JSON now, every word time kept (`formats::to_cache`);
    the old `lrclib2|` rows are dropped once per run.
  - **Every format read in Rust, with fixtures.** LRCLIB's `lyricsfile` (YAML, word timing where a
    contributor timed words; it beats the LRC beside it), Apple-style TTML, NetEase's YRC, KuGou's KRC
    (decrypted and inflated), QQ's QRC, LyricsPlus, PaxSenix's Apple JSON, Spotify's and Musixmatch's
    JSON through any envelope, YouTube's search, lyrics tab and captions, Genius and Megalobiz pages and
    escaped LRC: nori-lyrics `formats.rs`, `json.rs`, `html.rs`, each tested against a sample in
    `crates/lyrics/testdata/` (invented words, real shapes). Backing vocals are kept apart from their line
    (`LyricLine.backing`, `backing_words`) and duets get sides (`LyricLine.voice`).
  - **Apple-style lyric fill** (docs/motion.md item 20): a soft edge, words that rise and settle, held
    notes that glow, backing vocals under their line, duet sides. The clock's timing is nori-look's
    (`backing_sung`, `moving`, the `lively` flag and the timings in `stage`); the drawing is Kotlin's.
  - **Moving covers** (docs/features.md, "Player artwork"; motion.md item 19): found by the core
    (`crates/core/src/motion.rs`: the iTunes Search API, the web player's token, the catalogue's
    `editorialVideo`, kept per album in the response cache), played by `MotionPlayer` (its own muted
    ExoPlayer with media3's HLS module, a 64 MB cache) in a TextureView inside the showing record
    (`SleeveMotion.kt`). Off by default, behind the lookups switch, Wi-Fi only unless allowed; switched
    off nothing of it is built, and switched on it plays only while the player is open, at rest and on
    screen, and is released when the player is put away, the app is left or the screen goes off.
  - **"Keep playing" takes turns** (nori-queue `autofill.rs`, `autofill_picks` in the app's database,
    per server): candidates neither picked nor played in the last two weeks first, the first of them drawn
    from the top three, then the rest least lately used first; the core's refill ranks and remembers
    (`Client::autofill`), so no platform code changed.
  - **Shuffle always spreads artists and albums**: `weighted_shuffle` is gone from the settings, their
    page and `shuffle_plan`.
  - **The player**: the blur at the sleeve's foot moves with each move (`SoftSleeve`, motion.md item 18),
    no hairline half way up the opening player (the sleeve measured against the player's own root, and
    the wash's plain bands run a row under the sleeve's), and the tab stays lit on the pages opened inside
    it (`tabRoute` in App.kt).
  **Checked**: `cargo test --workspace` and both Android builds. **Not checked**: none of the services'
  or Apple's hosts could be reached from where this was written, so every request shape is what their
  documentation or other clients of September 2026 show; `tools/feature-e2e.sh` now asks each lyrics
  service on its own and reports it, checks that nothing is asked with lookups off, and checks that the
  moving cover builds no player switched off and lets it go when the player is put away. Run it on a
  phone, and `tools/bench.sh dev.nori.music 90 off` with moving covers off (must match the numbers
  below) and on with the player closed. On the phone, also judge: the lyrics settings' drag on the
  longer list and the key dialogs; a word-timed song, a duet and backing vocals; a slow swipe held half
  way (no haze on the lifted card), a slow pull on the sheet; and, with an album that has a moving
  cover, the fade in and out and `dumpsys gfxinfo` while it plays.
- **"Better beat detection", an opt-in neural beat tracker for AutoMix, all in the core.** Settings, Playback,
  under AutoMix; off by default, and only in a build with the `neural-beats` cargo feature (nori-engine's, which
  pulls in tract through nori-core and nori-player). On, the measurer (nori-engine `Measurer`, lowest priority,
  songs whole on the disk only) makes Beat This!'s small model once: the core carries only its graph
  (`crates/player/models/beat-this-small0.graph.onnx`, 186 kB, no weights), and fetches the authors' own
  checkpoint through the client's transport (`crates/core/src/beat_download.rs`: their URL, Wi-Fi unless
  allowed, SHA-256 checked), reads it with a restricted unpickler (`automix/checkpoint.rs`), converts it into the
  weights file the graph reads (`automix/weights.rs`, 4.2 MB, its own SHA-256 pinned), keeps that beside the
  database and deletes it when switched off (`crates/automix/src/beat_model.rs` holds the pins and where it
  is). On the desktop the whole of it took 0.48 s (0.44 s of it the download) and 31 MB at the peak. It then keeps the first and last 35 s of the song playing and the next one as it decodes them
  (`automix::beats::Ends`, mono at about 22 kHz, 3.4 MB each), and reads each end once; the grid replaces the
  classical one at an end when it is sure (`automix/beats.rs`). The model is loaded when a look needs it and let
  go when the measuring thread ends. Synthetic mix windows right and trusted 24 to 27 of 32, none wrong (the
  branch's measurement; see the research). The debug and perf builds carry the feature, a release build leaves it
  out unless `-PrustFeatures=neural-beats` (it makes the arm64 perf APK 15.7 MB bigger: the library 9.6 to 25.3 MB, the graph 0.19 MB of it; the APK was 34.7 MB with the bundled weights, 29.8 MB now), and the row is not shown
  without it. No copy of the weights is shipped or hosted (the old bundled export and its dead download URL are
  gone): the weights made on the device give the old export's logits within 4e-5 and the same beats (research
  7.1, `automix::weights` test). On *Kid A* it changes no mix
  window as shipped (research 7.1). Open: the branch's charging backlog (`BeatBacklog`, a JobScheduler job over downloaded
  songs) was not carried over, so only songs about to play are read; and nothing was timed on a phone: 6.7 s of one
  desktop core per new song, expect 25-35 s of a phone's big core. Research and numbers: `docs/research/analysis.md`, sections 5 to 7.
- **AutoMix enters the next song on its drop, leaves before a dead ending, and keeps two singers apart.** The
  analysis finds where the incoming song's arrangement arrives (the drop), a closing breakdown and the silence
  before a hidden track; the planner searches for the window where the drop lands on a downbeat (and phrase line)
  of the outgoing song, the bass changing hands there, with a run-up of whole phrases under it. The 15 s cap now
  counts music only: the silence at either end of a file and a hidden track's gap are free, so a mix is never laid
  over silence. When both songs sing, the incoming voice band is held 18 dB down until the swap and the outgoing
  voice is thinned by a rising high-pass after it, instead of the echo-out. A drum intro is exempt from the key
  caps. All of it is in nori-player's `automix` (analysis, plan and mixer); the rows gained their columns in
  nori-automix's store, and an older database gets them added when it opens (nori-db). `ANALYSIS_VERSION` is 8. The transition harness (`transition_eval`, `landmark_eval`) and the numbers are
  in section 7 of `docs/research/automix.md`.

- **AutoMix reads songs better, and it is measured.** `crates/player/src/automix/eval.rs` renders 16 synthetic songs with known
  beats, bars, key and sections (swing, drum and bass, a one-drop, a waltz, drifting bands, a detuned band, a
  beatless intro) and scores the analysis the way the literature does, plus what the mix needs: over the half
  minute at each end, is the grid right, and was it trusted. Against it: the tempo is a comb over the bar, so
  syncopated grooves no longer read at a beat and a half; a rival tempo lowers confidence; 3/4 is measured
  (`beats_per_bar`, and a waltz never locks to a four-beat song); half-time bars start on the kick; the key
  takes the band's tuning out and uses a minor profile that suits pop; intros and outros are where the music's
  sound changes (drum-only intros were missed entirely). Right-and-trusted mix windows 18 to 24 of 32, cues 11
  to 26 of 32, 6 % more CPU. Old rows are measured again as songs play (the analysis version moved). The
  research, numbers and the neural beat tracker's plan are in `docs/research/analysis.md`.

- **The bar shows what is heard.** With a crossfade or AutoMix on, `TransitionSink` holds the outgoing
  song's ending and mixes the next one into it, and the player counts that held ending as played the
  moment it is decoded (it has to: ExoPlayer only starts reading the next item within ten seconds of
  the end of the current one, so an honest position starved the mix). The bar used to show that lead:
  a jump of the whole crossfade as the hold began, then the next song at 0:00 while the old one was
  still playing alone. The sink now also keeps what the ear is at (`heardId`, `heardUs`, `heardAtMs`),
  `PlayerConnection` publishes that instead of the player's own position, and the title, cover and
  bar change on the frame the mix becomes audible. A seek is sent to the heard item, so scrubbing
  during the held ending stays on the song being heard.
- **A scrub into the transition hears the transition.** Seeking to -0:02 of a song whose mix started at
  -0:04 used to jump to where the mix would have been about to begin. The hold now measures how late it
  began (`lateUs`), the Rust mixer is seeked to that point in its curves (`Mixer::seek`) and the
  incoming track skips the same amount, so what is heard is what would have been heard had the song
  played on.
- **The seek bar has one clock.** `SeekBar` is one per-frame loop while the player screen is resumed:
  it reads the position, moves the bar towards it at a steady rate (a short exponential approach), and
  only teleports when the gap is a whole song. A song change and an outside seek glide; a finger holds
  the bar exactly where it is and seeks once, on release.
- **The seek bar reads the engine, not the controller.** A MediaController's place is the session's
  last word run on at one times and held at the song's length, and with the session's periodic updates
  off nothing puts it right until a play, a pause or a seek: a word taken off as the phone was unlocked
  kept the S22's bar at the end of a song with 14 s left. `PlayerConnection.heard` now goes by the
  engine's own place (`EnginePlayer.shownMs`: the last reading run on for 2 s at most, the engine asked
  to look again when it is a second old, `nori_player::heard::screen_place`), asks the engine to look as
  the app comes back (`catchUp`), and has the session say its place again when a controller drifts more
  than 2 s from it (`drifted`, `EnginePlayer.reanchor`). The controller's place stands for half a
  second after a seek and on another song than the engine's. The perf build's "place" invariant says
  when the bar or a controller is more than 2 s from the engine for over a second.
- **A tempo-stretched mix keeps the song's own time.** The incoming song of a beat-matched mix is
  played at the mix's tempo (x1.071 on the S22: 84 to 90 BPM) through it and eased back after. Its
  mixed audio was stamped with a clock that ran at one times, so the place fell behind the music by
  ~1.7 s over a 22 s mix and leapt ahead where the song's own timestamps came back - or, from the ear's
  reading (which did run at the tempo), fell back 2 s as the player's clock took over: "0:32 came back
  to 0:30". Now every chunk the transition engine hands down says the song time a frame of it stands
  for (`Downstream::media_pace`, from `Stretcher::take_content`), the sink counts played music and its
  clock in that time, the running clock moves on by it, and `Status::pace` (the speed times the tempo
  heard) is what a place is run on at between readings. The session is told the place again once the
  song is back at its own tempo (`Event::Placed`). crates/engine tests/stretch.rs reads the true place
  off the sound heard, through the mix, a seek into it and at 1.25x.
- **Buffering is not starving.** The perf build's "starved" watch on the CPU's track waits while the
  engine says it plays nothing (a provider's song still coming, a jump's dip): the report's false alarm
  on a qobuz song buffering (`Watch::track`).
- **One soft bottom, the sleeve's.** Every record - flat, lifted, sliding or flying in from the now
  playing bar or the lyrics thumbnail - used to carry its own blurred bottom, which travelled and
  changed size with it, and two records side by side met at a seam between two blurs. Now the records
  are whole squares and the sleeve's last rows are rubbed out of the whole layer once (`rubOutBottom`,
  `MELT`), so the band stays at the bottom of the sleeve whatever the records do, and a record picked
  up is clean above it and soft inside it. `SoftCover` and its per-record `BACKDROP` are gone.
- **A swipe within an album left the record half off the screen.** Every song of an album has the
  album's cover, and `land` read "the next record has the same picture" as "the player never caught
  up with the last change" and changed the song without moving the record. It now only takes that
  way out when a committed change really has not arrived (the 500 ms wait ran out).
- **The page's colours travel with the record.** The song only changes when the record has finished
  sliding, so the page's colours could not start before that either - the whole change happened after
  the move. `PageShift` carries how far the record has gone and which cover it is going to, the page
  draws the arriving cover's blur over its own at that strength, and when the song finally changes the
  page takes those colours over underneath instead of fading to them a second time (`CoverTint` keeps
  a palette with the cover it came from, so the hand-over cannot happen a frame early, which showed as
  a flick back to the old colour).
- **The page follows the record as it is picked up.** The blur behind is the record at the record's own
  size, so when the record shrinks the blur shrinks with it; left at the resting size, it carried on
  below a card that had shrunk away from it as a band at the wrong scale. (The soft bottom no longer
  belongs to the record at all; see "One soft bottom" above.)
- **The page is more of a blurred mirror, less of a tint.** Each pixel of the wash was pulled two
  thirds of the way back to a flat colour; it is pulled about a third now, with a wider lightness band
  and more of the record's own saturation, and two more blur passes so that more colour does not
  become patches.
- **The three buttons under the volume read as centred.** They were evenly spaced already, but the ink
  inside the glyphs is not centred in its own square and the queue's marks came out a sixth narrower
  than the others, so the row leaned. `PanelButton` takes an optical size and nudge; measured on
  screen, the outer two now sit within a pixel of each other's mirror.

- **The page's colour changes with the song.** The covers either side of what is playing were already
  fetched ahead, but their colours were only worked out once the song had changed, so the page wore
  the last song's colour for a beat and then caught up. `warmCoverPalette` does that work in advance,
  from the now playing bar (which is on screen whenever music is, so a skip from the notification
  counts too), and one cover is measured only once however many places ask for it at the same moment.
  The cross-fade is 420 ms, about as long as the record takes to slide across.

- **Settings, regrouped and in plain words.** Seven pages now, each about one thing: Playing, Sound,
  Downloads and data, Look, Lyrics, Library and lists, Servers. Every title and description was
  rewritten short and plain, with no dashes or semicolons, and the dead "Home shelves" note in Lists
  (which only said the rows are arranged on the home page) is gone. The search index was rewritten
  with the rows, since a row is found by its own title.
- **The queue's last row fades.** It was cut off dead straight a few pixels above the song's title,
  and those pixels flickered as a panel came or went. The list goes soft over its last stretch now,
  masked rather than painted over, the way the lyrics are.

- **The cover travels into the lyrics, and back out.** Entering the lyrics used to dissolve the sleeve
  into the blurred page, which read as a block of blur appearing at the top of the screen; the sleeve
  now shrinks into the lyrics header's thumbnail and grows back out of it, one picture the whole way
  (`PanelFlight`, with `PlayerSheet.panelFlight` standing both ends' own copies down). The queue has no
  cover of its own, so that change is still a dissolve - a real one now: the panel cross-fade was
  driven by a child animation inside `AnimatedContent`, which the arriving content reads as already
  settled, so it never ran.
- **The hairline of letters above the seek bar.** The lyrics' fade is a mask, and the mask was drawn to
  the panel's exact height while the layer it erases is clipped to whole pixels: the last fractional
  row came through unmasked. The mask is drawn a pixel beyond every edge now, with the gradient still
  anchored to the panel's own height.

- **What plays when the queue runs out is a choice.** Keep playing now has two settings beside it
  (Settings, then Playing): songs or a whole album at a time, and what that is chosen by - what the
  server calls similar, the same artist, the same genre or the same decade. Albums queue the record
  in its own order, skipping one the queue has already played. `PlaybackService.autoFill`.
- **Tracks are measured before they are played.** nori-engine's `Measurer` (crates/android/src/measure.rs,
  with `AutoMixPrefetch` saying where the files are) decodes the track playing and the two after it on
  a thread of the lowest priority and feeds the streaming analyser, each once, so a
  transition has both halves' tempo, beats and cues the first time those two songs meet - until now
  the tap only finished a track as it ended, which is one boundary too late. It never fetches
  anything: a track is measured only once its bytes are on the device (downloaded, or brought in by
  the fetching ahead, which measures a song as it comes), and the whole thing is off unless AutoMix is on.
- **The mix no longer allocates as it starts.** The tail buffer, the mixer, the time stretcher and
  the chunk pool are built when the plan is made, at the start of the outgoing track, instead of
  between two buffers on the audio thread at the moment the mix begins (`TransitionSink.prepare`).
- The word "Mixing" is gone from the middle of the seek row.
- **The sleeve's swipe waits for a sideways finger.** The full-screen cover claimed any drag that
  passed sideways touch slop, so putting the player away with the slightest slant changed the song;
  it now takes the gesture only when it has gone nearly twice as far across as down.
- The player's ⋯ has a heart again, at the top of the sheet, reading this session's marks.
- **Favourite albums refresh at once.** Starring evicted every cached read but the starred album
  list, which is the home shelf; that list is dropped now, the shelf re-queries on a star, and this
  session's marks are applied on top so an album leaves it the moment its heart goes out.
- **The home page pulls to refresh.** A thin ring, no plate: it drops the stored answers behind the
  shelves, re-queries them and walks the offline index.
- **The chrome stands off the page.** The floating slab was a ninth of the text colour over the
  background, which over a dark page and the AMOLED black one was nothing at all; the lift now
  follows the page's own luminance and the slab carries a hairline and a deeper shadow.

- **The wash without stairs.** The page's blur was a 32 px texture stretched over the screen, and the
  sleeve's melt read it a row at a time in 28 slices, so both came down in visible steps. The wash is
  still worked out at 32 px but handed to the GPU at 128 (`CoverColors.smooth`: bilinear, two light
  box passes and a one-level dither against eight-bit banding), and the melt is cut into 72 slices.
  One 64 kB texture per cover, still one quad per frame.
- **Black is a colour.** The page-colour histogram has a bucket for black and near-black at full
  weight, and the dark page may go all the way down to black, so a mostly black sleeve gives a black
  page instead of a muddy one. Pale greys and whites are still discounted, because a page that pale
  would take the controls with it.
- **The sleeve always melts.** With the cover's colours off there is no wash, and the sleeve used to
  stop dead; it now goes soft into the plain page (black on AMOLED), in place and in flight.
- The format caption is gone from the foot of the player; the ⋯ menu's header shows it.
- The downloads suite check now tries up to twelve albums for one with work left: the emulator keeps
  what earlier runs downloaded, and four ran out.
- **The now playing bar's heart updates.** The bar sat outside the `LocalStarMarks` provider, so it
  only ever saw the server's answer and a tap changed nothing on screen until the song came round
  again. It is inside it now.
- **A song row's marks are columns.** Heart, download mark, time and menu each have a fixed-width
  slot on every row, the time last before the ⋯ and right-aligned in a 36 dp box, so hearts line up
  in a list of favourites and the time sits against the menu instead of across an empty download
  slot from it.
- **Home rows, owned by the home page.** ⋯ → Rearrange rows lists every shelf: the shown ones in
  order, held anywhere to move (a long press, since the list scrolls), each with a switch, and the
  hidden ones under "Not shown". Settings only points there now. Two new shelves: all Playlists, and
  Most played songs (from the offline index, no request). The favourite playlists shelf stands where
  the order puts it; it used to be drawn above everything whatever the order said.
- **Playlist favourites.** What was "pin to home" is a favourite, drawn as a heart that fills when it
  is one, and the shelf is "Favourite playlists". Still kept on the phone - Subsonic cannot star a
  playlist.
- **A tab that is already showing does nothing** when tapped, instead of running the page's arrival
  again.
- **Page colour from what there is most of.** CoverColors picks the page's colour from a coarse hue
  histogram weighted by area, not Palette's dominant swatch (which filters out near-whites and some
  warm hues before it counts, and on a pale pink sleeve with dark hair voted for the hair). The
  bottom-row average no longer enters the page colour - it still starts the wash, for the seam - and
  the dark-mode band is 0.10-0.20 lightness, which keeps a pink cover a deep rose instead of brown.
- **The player fades up through the bar** over the first tenth of the rise instead of appearing at
  full strength on the first frame of a drag, and **the flying cover is really centred**: a square
  wider than the screen is centred by layout before any translation, which the flight had left out,
  so it flew about forty pixels left of centre and jumped right when the sleeve took over.
- **The album page, on a friend's reading.** The library's album grid is two per row everywhere
  (`GridCells.Fixed(2)`); it was adaptive, which gave two on an ordinary phone and three from about
  460 dp of width - large phones, landscape, split screen - which is the cramped case. The hero's
  cover is no longer tappable and the full-screen artwork dialog it opened is gone (HeroPage is
  shared, so that also goes for the artist, playlist and mix pages). The album page's filter field is
  gone with it: an album is not long enough for searching to beat scrolling. `FilterField` itself
  stays, because the playlist page still wants one.
- **The player's own furniture.** The close chevron and the drag bar are gone - a pull anywhere on
  the artwork already puts the player away, and the lyrics and queue keep a plain invisible strip at
  the top for the same drag. The title block now carries the artist and the album on their own
  lines, each a tap to that page; the format caption moved to the very foot of the page, centred
  under everything; and the favourite here is a heart, as it is on albums, artists and playlists. The
  now playing bar has a heart of its own.
- **The panel's text fades with the panel.** The title, artist, album and the buttons beside them are
  part of the panel, so they take the same fade as the artwork and the queue; before, they were left
  behind and cut.
- **The cover's flight, again.** It grows about its own middle and is under the middle of the screen
  by the time the sheet is half way, instead of carrying its left edge the whole climb and spending
  it in the corner with the page showing beside it. Its last frame now matches the sleeve exactly
  (it used to arrive about forty pixels off). The melt at the sleeve's bottom is drawn where the
  sleeve will be rather than on the record that is still travelling, so it stays in one place instead
  of sliding up the screen with the picture.
- **The song menu, ⋯.** One stage, so the back gesture closes the whole sheet rather than collapsing
  it. The favourite row is gone (the player has a heart and a swipe does it on any row). Seven
  everyday actions carry icons at the top - play next, queue, playlist, download, album, artist,
  sleep timer - and radio, instant mix, exclude, share and details are behind a "More" row that opens
  in place.
- **Page transitions.** Pages arrive from a little above and settle down into place with a decelerate
  ease, and leave the same way; the home page's sections follow one another down the screen over
  about a third of a second, once per visit. The owner read the old horizontal slide as things flying
  out of the top left corner.
- **A record left standing up.** A button press that turned out to have nothing to slide to (the
  player had not caught up, so the record waiting off the edge was the one already showing) changed
  the song and returned without putting the record back down, and the cover stayed at its small
  lifted size. That path settles it now, and a guard effect makes it general: once nothing is moving
  and no finger is on the record, a record that is still up goes back into its sleeve. The button
  queue also refuses a fifth press rather than dropping one from the middle, so the count of what is
  waiting - which decides whether the record stays up between presses - cannot drift.
- **Panels cross-fade properly.** The artwork used to stay fully drawn under the incoming lyrics or
  queue and then vanish in one frame, because the outgoing panel was held at full strength (the
  transport is shared across the change and fading the content it sits in dimmed it half-way). The
  fade is now on the panel itself - one number from the AnimatedContent's own transition, applied to
  the artwork, the queue and the lyrics - so the two panels pass through each other and the transport
  stays solid.
- **The cover flies from the lyrics too.** Put away from the lyrics, the cover travels from the
  header's thumbnail to the one in the now playing bar (`FlyingThumb`, `PlayerSheet.panelCover`,
  measured by the header only while the sheet is fully open so the rectangle is in sheet
  coordinates). Before, the lyrics simply sank behind the bar and a cover appeared there out of
  nothing.
- **Reading the lyrics.** A finger on the words stops the list following the song, and it stays where
  it was put for four seconds after the finger lifts; then it glides back to the line being sung
  rather than jumping (a jump is still right for a seek, which is a different thing). Lyrics that
  came without timings say so in the corner - "LRCLIB · not timed" - because unsung words are all one
  brightness and a tap on one goes nowhere, which otherwise looks like the lyrics are broken.
- **The scrub no longer snaps back.** Letting go of the seek bar showed the old position for the
  moment between the finger leaving and the player answering. The bar holds the place it was dragged
  to until the player is really there (or a second and a half has passed).
- **A title that fits is left alone.** The soft right edge is only drawn when the line really is too
  long: the title measures the room it has and the width it needs (an `onSizeChanged` either side of
  the marquee, which lays text out unbounded), so a title that merely comes close is not dimmed at
  its last letters. Same in the now playing bar.
- **The record landed off centre from a button.** `land` measured the gap between records from the
  lift the record *had*, and a button press starts the lift in a coroutine of its own and comes
  straight on, so the lift had not begun: the record was sent a full unlifted span, shrank on the way
  and arrived about a fifth of the screen too far over - its edge in the middle instead of its
  middle. It measures the lift it is going to have instead. A swipe was right all along only because
  the lift had already started under the finger. Checked on the emulator: `incomingX=0` exactly, for
  next, for previous and for a swipe.
- **The record change, second pass.** Four things the owner saw on a real phone, all in
  SleeveCarousel. The record overshot the middle on a button press: the lift sprang past its mark
  (damping 0.9), and since the gap the arriving record waits in is measured from the lift, a lift
  that overshot pulled the incoming record past centre and back. Every spring here is critically
  damped now, and `liftedScale` clamps the lift to 0..1 - a spring settling back used to dip below
  nought, which made the record a shade bigger than the sleeve and left a line at the bottom where
  the page's wash, drawn to the sleeve's own size, stopped short. A press while the change before it
  was still settling slid in a copy of the cover already showing ("it changes the cover first and
  then animates from it to itself"): each change now waits for the player to be on the song the last
  one asked for (`committed`, which is set whether or not there was a picture to hold over - waiting
  on the picture let a cover that failed to load release the next change against a queue that had not
  moved). And a swipe that catches a record mid-flight commits the change only once the record has
  really gone (half a span), rebasing the offset onto the arriving record so the drag carries on from
  the cover it can see; caught earlier, nothing changes and the record stays under the finger.
- **Covers ahead, both ways.** The prefetch walked forwards from the playing song and only one song
  back, so the second swipe backwards always waited on the server. It now steps outwards in both
  directions. A neighbour whose picture has not arrived is drawn as a record - same square, same
  corners - with the app's loading sheen rather than a flat grey card.
- **The record change, properly.** Three faults sat on top of each other in SleeveCarousel, all from
  the same root: a `pointerInput` keyed on `Unit` is created once and never replaced, so the gesture
  closed over the first composition's addresses and painters - back then there was no queue at all.
  The landing was filed under an address the sleeve could never match, so the record sat in the
  middle at its lifted size, over the whole change, until the timeout let go of it (that is the
  "static smaller cover covering the animation"); a painter caught that early has no picture in it,
  so a record could land with nothing to draw and the cover being left stayed put for a few frames
  (the blink). Everything a gesture or the button queue reads now goes through `rememberUpdatedState`.
  The landed record also travels with the drag instead of sitting in the middle, so a swipe during a
  change no longer has a second cover pinned over it, and a landing that is cancelled leaves the
  offset alone if a finger has taken the record over - putting it back wiped the new drag's first
  half, which is why a swipe straight after a swipe went nowhere.
- **Buttons make the same move.** Next and previous lift the record, send it out one side and settle
  the new one into the sleeve, exactly as a thumb does, with a stiffer spring (`BUTTON_STIFFNESS`)
  and a quicker settle. Presses queue rather than interrupt (a small channel), and the record stays
  up between them, so four quick presses are four songs and four changes. Springs animate to within a
  pixel now, not a hundredth of one: the default threshold made a quarter-second move take half a
  second.
- **A suite flake.** The downloads test picked a random album and expected songs to start
  downloading; an album an earlier run had already fetched has nothing to do and failed it. It now
  tries up to four candidates until one has work left.
- **The transport and the record.** A skip asked for while the music is paused starts it playing
  (`Controls.andPlay` in PlaybackService, so the notification and a headset do it too); the service's
  own skips - an explicit track, a track that will not play - go to the player underneath and leave a
  paused queue paused. The player's times refresh on a track change even while paused (`position`
  takes the song as a key), instead of leaving the last song's 2:50 under the new song's title. The
  transport's skip buttons now send the record across exactly as a swipe does, a little quicker
  (`SleeveSlide`, `BUTTON_STIFFNESS`); a previous press that only rewinds the current song - media3's
  three-second rule, which the button repeats so the sleeve and the sound agree - does not, because
  there is no other record to show.
- **Flicked records.** Both carousels kept their offset in an `Animatable` and snapped to it from a
  coroutine per pointer event. On a flick several of those were still queued when the finger left and
  landed on top of the settle that had already started, dragging the record back mid-change - the
  jerk you could see when a swipe was let go early with momentum. The offset is now plain state
  written straight from the drag, with one cancellable job for the settle or the landing, and a
  landing that is cancelled still changes the song so a quick second press is not dropped.
- **Back gesture on the player.** `PlayerSheet.isOpen` is what the sheet was last asked to do, not
  where it is: it came from the Animatable's target, so the first pixel of a drag or a back gesture
  read as "closed" and switched the back handler off underneath the finger - the player never sank
  and then vanished in one frame instead of settling onto the now playing bar. The back gesture's
  close is a little quicker than the close button's (spring stiffness 700 against 420), and the page
  back gesture lets go of the page sooner too (`PredictiveBack.MS`, 320 -> 240).
- **Scrubbing.** The seek bar owns the pointer from touch-down and consumes every move, so the player
  sheet's vertical drag can no longer take a scrub that runs a few degrees off level - that was the
  bug where the bar followed the finger, the time changed, and the song never moved, because the
  gesture ended in `onDragCancel`. The strip is 34 dp tall (26 dp was easy to miss with a thumb), the
  bar thickens and grows a dot while held, and the seek happens once, on release. `build/seek.sh
  <slant px per step> <end x>` scrubs it on the emulator and prints where it landed.
- **Long titles read themselves out.** `Modifier.readable()` in PlayerScreen (`basicMarquee`) walks a
  title that does not fit, after a 2.6 s pause, in the full player, the lyrics header and the now
  playing bar. The last one passes `iterations = 2` - it reads itself out when the song comes on and
  then settles back - because that bar is on screen for as long as the app is, which is the same
  reason it has no progress bar. On the full player it runs while you are looking at it and not
  otherwise (`LocalPlayerShown`): the player stays composed behind the rest of the app, and a title
  scrolling down there would hold the frame clock awake. Measured on the emulator: 0 frames in 5 s
  with the player closed, ~37 fps while it is open and a title walks, and the now playing bar goes
  quiet (0 frames per 10 s) about 40 s after a long title starts.
  A marquee lays its text out unbounded, so there is no ellipsis; the line goes soft over its last
  20 dp instead (an offscreen layer and a `DstIn` gradient in `readable`).
- **Up Next.** Play next and Add to queue (the default right swipe) work as in Apple Music: the songs
  go right after the playing one, "last" ones after the songs added by hand before them, in order,
  and then the queue carries on. Items carry `queued` = "next"/"last" in their extras
  (`MediaItem.queued`); the service's `Controls.addMediaItems` sends marked items to `upNext`, which
  places them in the list and, under shuffle, rebuilds the `DefaultShuffleOrder` so they are not
  scattered. Turning shuffle on puts the playing song first, keeps the hand-added run after it and
  shuffles only the rest (the core's `playlist_shuffle`, over nori-player's `queue::shuffle_around`). The queue panel lists songs in play order
  (`PlayerState.order`), marks hand-added ones, and hides reordering under shuffle. Checked with
  `build/upnext.sh`-style runs via `do enqueue|playnext|shuffle` and the `upNext` state field.
- **Swipes.** A song row moves only in a direction that has an action. Right adds to the queue, left
  favourites (or unfavourites) by default; the left one is stored under a new key, `swipeLeft3`, so
  old installs get the new default too. The drag uncovers the
  action's icon and words; past 30% of the width the strip turns accent, the phone ticks and the row
  goes heavier, and letting go acts. 5-star ratings are gone (nobody used them).
- **The silent USB DAC.** Audio offload hands the compressed stream to the phone's audio chip, and
  that chip has no path to a USB device: the track opened, reported itself playing, and the DAC sat
  in silence. Offload now stands down whenever anything USB is attached (`Outputs.usb`), and a sink
  that refuses the stream once gives up offload for the life of the service instead of skipping
  through the queue. Bit-perfect also read the wrong format — the *decoder's* input rather than what
  the sink writes — so no mode ever matched; it is now applied from the audio track provider, which
  is the last moment the framework still reads preferred mixer attributes.
- **Offload on the Rust engine: a second of music, then silence (S22, Android 16).** Not confirmed on
  the phone yet. In the report the songs changed on the user's own taps (`ViewPostIme` before each
  "to song N"), so the engine was not ending songs by itself; what it did was write each whole song
  and its end of stream within a quarter second, and every skip then paused and flushed a track that
  `setOffloadEndOfStream` had stopped. Android's `AudioTrack` keeps such a track "stopping": a write
  that does not wait takes nothing (`blockUntilOffloadDrain`) and a late `onPresentationEnded` can
  follow. `offload.rs` now opens a new track instead, as media3 does, says an end of stream only
  while the track plays, writes four minutes ahead at most, reads a failed `getPlaybackHeadPosition`
  as no reading (it was nought, which skipped to the next song on a gapless track), takes a lower
  count for a join only where the clock says the ear can be, and hands the song to the CPU when the
  head keeps making no sense. Its reasons reach the perf report as `offload:` events ("… ended by
  the play head …", "the play head read …"). The next report says which of these it was.
- **Offload on a small track, with the screen off (S21 FE and S22, Android 16).** Not confirmed on the
  phones yet. The S21 FE grants an offloaded track 32 KB of the 8 MB asked (819 ms of a 320 kbps song),
  the S22 64 KB; the engine topped them up on a timer worked out from those bytes (every ~409 ms), and
  its watchdog gave the chip up when the count stood still for what the track holds and two seconds
  (2.8 s): with the screen off the S21 FE's timestamp and play head stood that long while it played, so
  the CPU took over and offload was given up for the song. Now: a full track waits for the platform's
  `onDataRequest` once the platform has asked once (media3 sleeps for offload the same way), each wake
  writes all the track takes, and the next song is written once less than 30 s is left however small
  the track. On the simulated chips (crates/engine tests/paths.rs `on_a_small_grant_…`) the engine wakes
  exactly as often as the platform asks: 2.40/s on 32 KB and 1.20/s on 64 KB when the chip buffers
  nothing of its own (the platform's own pace: no fewer is possible without risking a gap), 0.27/s and
  0.22/s with a DSP buffering 256 KB (before: 0.55/s and 0.47/s). The watchdog gives a chip up only when
  its count has not moved and the platform has asked for nothing for longer than the music written past
  the count and a slack of 10 s (twice the longest the count was seen standing, up to a minute), and the
  CPU then plays on from where the chip's count last put the ear, never from the clock. A count that
  stands while the platform keeps asking is one it does not keep: the play head is tried, then the
  clock (the platform played what it asked for). Note on the report: "left at 245306 ms (chip said 94130
  ms …)" was most likely no 150 s skip: the first is the place in the song, the others the track's own
  count, which starts where offload took the song over (part way in, at ~150 s, if the two agree); what
  the old watchdog put the ear past was the 867 ms written beyond the count.
  The `nori:engine` wake lock is let go while the songs are offloaded, the track fed, the platform has
  shown it asks for more, and nothing else is due (nori-engine's `Event::Awake(false)`, RustPlayer.kt
  `holdCpu`); it is taken again (on the engine's thread, before it goes on) for a control, a song
  starting, its bytes awaited or fetched (any open body), a fade, a count to look at again, the CPU path,
  and from a few seconds before the ear reaches the next song or the end of the music (half again the
  longest the platform went between two asks, and a second: the song's event and its ReplayGain volume
  come on time). What that relies on, as media3 does (`ExoPlayerImpl` lets its lock go while it sleeps
  for offload): the audio HAL and audioserver keep the offloaded track playing from the DSP with the CPU
  asleep, and wake it (with their own wake lock) to pull more, which is when `StreamEventCallback.
  onDataRequest` reaches the app's callback thread, which unparks the engine. Nothing in the app polls
  meanwhile: the engine's own timers do not run while the phone sleeps, which is why the lock comes back
  before a join. The media session needs no lock: it changes only on the engine's events, which come
  with it held. To check on the phones: a perf report of a playlist with the screen off for 5+ minutes.
  Its offloaded stretch should say the wake lock was held a few % of the time, nori-engine's wakeups/s
  about the chip's asks/s, no "offload given up", and every song's "ended by the play head".
- **The player against Apple's.** The sleeve runs to all three edges — **including up under the
  status bar**, which is the point: Apple's artwork has no top edge, and stopping ours below the
  handle drew a line across the screen. The handle and the close button float over it, with the same
  shade under the status bar the album page uses. Three plain transport glyphs (double triangles, as
  Apple draws them), a volume slider with no knob, a favourite and a ⋯ on the title row, and the
  column's spare height split three ways as Apple's is: about 7 % of the screen under the sleeve,
  10 % over the volume slider, 11 % under the bottom icons. Every row of the control stack now lands
  within a percent or two of `w4`; measure a change against `/tmp/crops/w4_screen.png` the same way.
- **The lyrics view.** One title block, not two: the artwork shrinks to a thumbnail in a header row
  and the words take the whole middle of the screen.
- **The album page.** Sentence case under the title, as Apple writes it, and a track by the album's
  own artist no longer repeats that artist on every row.
- **The seek row's centre label.** Apple puts a word between the elapsed and remaining times while a
  transition is running; ours now reads "Mixing" for exactly as long as `TransitionSink` is out of
  `Phase.PASS`. Nothing new watches it - the seek bar is the only thing that ticks while the player is
  open, so it asks on the same beat. Note while testing this: the media session's position pins at the
  outgoing track's duration while the mixed tail plays, which looks like a stall and is not one.
- **The output switcher.** The middle glyph at the bottom of the player is where the sound is going,
  as Apple's AirPlay mark is: a cast glyph on the phone's speaker, headphones or Bluetooth in the
  accent colour when something else carries it. It opens Android's own output picker: on 14 and
  later through the public `MediaRouter2.showSystemOutputSwitcher()`; on 11-13 through SystemUI's
  `LAUNCH_MEDIA_OUTPUT_DIALOG`, which is a **broadcast** — the first version started it as an
  activity, which cannot resolve, so on a real phone the button only named the output; on 10 through
  the Settings panel. Cast speakers will not appear in it until the app implements casting
  (`docs/features.md`, build step 8); USB, Bluetooth and wired outputs do. The sleep timer
  that used to sit there is on the player's ⋯ instead — `LocalPlayerMenu`, the same song menu with
  the playback-wide entry added, so a row's menu in a list does not grow one.
- **Favourites, search and the mini player.** A favourite flips under the finger instead of after a
  round trip to the server; tapping Search raises the keyboard even when the screen is already open;
  the mini player rises with the finger and hands over to the full player part-way through the drag.

- **Downloads.** Up to "Downloads at once" (Settings, then Downloads and data, 1-10, default 5) run in parallel, in
  the order asked for; that is media3's own queue, `maxParallelDownloads` kept in step from the
  foreground-notification tick. Progress comes from wrapping media3's downloaders
  (`TrackedDownloaders`), not from polling, and passes a `ProgressGate` (4 a second, whole percents)
  into one flow per song. `Downloads.marks` changes only on a phase change, so song rows
  (`DownloadSlot`) recompose on those and one ring per downloading song recomposes on progress.
  `downloads` is a route; the notification opens it with `ACTION_OPEN_DOWNLOADS` (onNewIntent when
  running). Traps found the hard way:
  - `DefaultDownloaderFactory`'s executor runs the byte copying. A small pool there caps the
    downloads that move at the pool size while media3 still reports all of them as downloading (5
    rings, 2 filling). It is `Runnable::run`: each download copies on media3's own task thread. The
    stream `Dispatcher` (media3's OkHttp source enqueues on it) must also have room for 10 + playback.
  - Nothing about a download may live only in memory. media3's `DefaultDownloadIndex` survives a
    force stop; the manager only resumes it once something starts `DownloadWorker`. `Downloads.reconcile`
    (at launch, and again from `ActionsViewModel`) squares the Rust index with media3's and starts the
    service; the batch the notification counts (`DownloadBatch`) is fed only from the manager's
    callbacks, `onInitialized` included, so it is rebuilt the same way.
  - The notification's total is the batch's: everything queued since the queue was last empty. The
    result goes in its own id (1002), because the service takes 1001 away when it stops.

## The page colour

Apple's player is not painted one flat colour. Sample across their screenshot and it varies both
ways — at `y=1100`: `118,27,25  99,29,25  84,16,34  88,28,27  78,19,18`; at `y=2000` it is still
red but darker. The background *is* the artwork, enormously enlarged and blurred, which is why it
matches the sleeve so exactly.

One average of the cover's bottom rows cannot do that. *In Rainbows* is vivid everywhere and near
black along its bottom edge, so the page came out brown mud next to a rainbow. nori-look's `wash_of`
(crates/look/src/cover.rs) now shrinks the cover to 16 px a side, smooths it once off the main thread, pulls every pixel to
within 0.05 of the page colour's own lightness (0.035 in light mode) and holds its saturation back —
then `sleeveWash` (Design.kt) draws that stretched over the page and lets the GPU's bilinear filter do
the enlarging. The hues vary the way the record's do; the contrast text needs does not move.

It is still static: one 4 kB texture per cover, uploaded once, drawn as one quad. Neither fill covers
the whole page — the seam gradient is opaque down to 42 % and gone by 68 %, so each is clipped to
where it shows. Sixteen pixels a side held the colour but no shape, and the sleeve read as stopping
dead where the sharp artwork ended; thirty-two keeps enough of the record's forms that the picture
seems to carry on behind the words, for the same one quad.

**The player only.** It was tried on the album page and taken out again. That page has a list
scrolling over it and an artwork that fades under the parallax, and every edge those give the wash is
one more thing for it to disagree with: stretched over the header alone it squashed the whole cover
into a few hundred pixels (visible as bands of the record's colours behind the title) and ended on
the sleeve's dark bottom rows with a line across the page; drawn on the page instead it stayed put
while the list scrolled into it, so the top rows always sat on colour. The album page keeps
`pageBrush`, which has none of those problems.

## Animation

There is one, and it is the exception to "nothing animates unless the user touched it": the four bars
where a playing track's number would be (`Components.PlayingBars`). The owner asked for it. They move
only while the music sounds, freeze into a fixed shape when it is paused, and stop entirely when the
screen goes off or the row leaves the composition. The phase is read in the draw phase, so a frame
invalidates that 16 dp box and nothing else. The screen-off benchmark below is unchanged by it.

## The sleeve is not square

Album art is square, Apple's included — so how does their player's artwork touch the top edge of the
screen *and* reach down behind the title, which a full-width square cannot do? It is the square
scaled up and cropped at the left and right edges to fill a taller box. Crop `w4` across the row
where a full-width square would have ended (y = 977 of a 977-wide screen) and the flowers below that
line are exactly as sharp as the ones above it, with a strip of red tape crossing it unbroken. It is
the picture, not the blur behind it.

`PlayerScreen.SLEEVE` is that ratio and `Cover` already crops. Everything else follows from it: no top
edge because the picture starts at y = 0, and the picture's tail reaching the title because it ends
past half the screen. It is 0.80 rather than the 0.88 measured off `w4`, because this screen is 20:9
against the 19.5:9 that was measured; check it by sharpness rather than by the number. Row by row,
`w4` against ours: 44 % 9.9/9.8, 46 % 9.9/10.0, 48 % 8.2/9.1, 50 % 5.2/4.4, 54 % 1.1/3.2. It costs
about an eighth of the cover off each side, which is the price of the sleeve reaching both ends.

**The wash has to be quieter than Apple's, not equal to it.** Measured across the page below the
sleeve, Apple's colour varies *more* than ours ever did - channel spreads of 36/21/17 against our
8/5/5 now - but all of their variation is inside one red, because that cover is one hue. A cover that
is teal down one side and warm down the other gives the page teal and warm patches at the same
spread, and a patch reads as a fault where a glow does not. `MUTE` in nori-look's cover.rs pulls every pixel most
of the way back to the flat page colour after the lightness clamp; that is what makes it subtle
without making it grey.

Below that, the sleeve's last rows are rubbed out of the records' layer (`rubOutBottom` in
PlayerScreen, a `DstOut` gradient over the last `MELT` of the sleeve) and `sleeveWash` draws the
cover's blur behind and below at the sleeve's own scale, so what shows through the rubbed-out rows is
the same picture gone soft and the join cannot be seen. (`drawSleeveMelt`, which painted the blurred
rows over each record, is gone: a band painted on a record travelled with it.) **Nowhere in either is
there a flat colour**, which is the whole
point — every earlier version faded the picture onto some computed colour, and that colour met the
page along a dead straight line every time.

Two traps at that edge: the rub-out must reach full strength *before* the sleeve's last row, not on
it, or a hairline of raw cover is left along the bottom (plain to see the moment a lifted record grows
back); and `sleeveWash`'s three bands must round their *edges*, not their heights, or a row of page
colour shows between them.

## Sizes, measured

Everything on the player was measured against `w4` as a share of the screen's *width*, so a 977 px
iPhone and a 1080 px Android compare directly. Apple / ours after the change:

| | Apple | ours |
|---|---|---|
| title top, artist top | 56.5 %, 59.3 % of height | 56.3 %, 59.3 % |
| cover detail at 53 / 55 / 57 % of height | 5.0 / 1.9 / 0.8 | 4.5 / 2.4 / 0.7 |
| side margin (title, seek bar, discs) | 8.2 % | 8.3 % (`PLAYER_GUTTER`, 33 dp) |
| pause glyph height | 9.8 % | 10.0 % |
| skip glyph width | 9.7 % | 9.6 % |
| seek bar thickness | 1.64 % | 1.67 % |
| volume bar thickness | 1.84 % | 1.76 % |
| title-row disc | 7.9 %, glyph 60 % of it | 7.9 %, glyph 60 % |
| bottom icons | 5.4 × 5.1 % | 5.5 × 5.0 % |

The title sits over the sleeve's blurred tail, as Apple's does: the sleeve is laid out shorter than it
is drawn (`SLEEVE_UNDER_TEXT`), and its melt is quick-then-long (`1 - (1-t)³`) so a faint trace of
the cover is still there behind the title, which is what the numbers above show on theirs.

Settings and menus had two Material shapes left in them. The switch is now UISwitch's 51 × 31 pt
track with a 27 pt white thumb (`NoriSwitch`), and `NoriSlider` is UISlider's 4 pt track with a
28 pt white knob on a soft shadow. Neither of those was measured off a screenshot — there is no
settings screen in the App Store set — they are UIKit's own defaults.

## Gestures and the equalizer

- **The player is a sheet, not a route** (`PlayerSheet`). One number, `progress` 0..1, drives the
  whole transition: the sheet's top edge goes from the mini player's top to the screen's top, the
  page behind darkens, and the cover flies from the mini player's thumbnail into the sleeve
  (`FlyingCover`). A drag sets the number directly, so it follows the finger and holds where it is
  held; a release goes the way of a flick, else finishes once it has come 15 % of the way, else goes
  back. Up on the mini player opens it; down anywhere on the artwork (or on the handle, in lyrics
  and queue) closes it. Modelled on the reference recording of Apple's (owner's `otherappanim.mp4`).
- **The flying cover is laid out once** as the full square at the sleeve's height and moved only by
  a layer transform (scale plus a clip from square to the sleeve's window). Growing it by layout gave
  the image a new size per frame, and every size was a new decode and texture: 300 ms stalls. The
  sleeve and the flight share one painter; two requests for the same picture in one frame decoded
  two bitmaps and uploaded the second at the landing.
- **The player stays composed** once the app has been up 1.5 s, parked a screen below the bottom
  edge (off-screen, so it draws nothing and catches no touch meant for the mini player). Building it
  on the first frame of the drag stalled that frame. Anything in it that ticks or reaches outside
  (seek bar, lyric timing, status-bar icons, keep-screen-on) checks `LocalPlayerShown`.
- **Panels dissolve** (artwork, lyrics, queue): `AnimatedContent`, the old panel held opaque under
  the new one (`ExitTransition.KeepUntilTransitionsFinished`; a zero-length delayed fade-out was not
  held), and the seek bar, transport, volume and icons are shared elements so one copy moves rather
  than two showing.
- **Measuring on the emulator:** it renders with SwiftShader (CPU), so GPU time per frame is 30-60 ms
  even idle and first draws take hundreds. Judge smoothness on a phone; on the emulator, check that
  nothing recomposes per frame (log from the composables) and read the UI-thread columns of
  `dumpsys gfxinfo framestats`. Raw gestures: `adb shell input motionevent DOWN/MOVE/UP x y`, which
  can hold a drag mid-way for a screenshot.
- **The equalizer no longer drops the sound on entry.** Opening it sent `CMD_TUNING`, and the
  service rebuilt the sink (stop, prepare) to swap the 10 s buffer for a shallow one - an audible
  break, on a DAC or anywhere, and again on leaving. Now: nothing on opening; one rebuild on the
  first change to a band, only if the equalizer is in the chain; nothing on leaving; the deep
  buffer comes back at the next pause, where a rebuild is silent. Counted from the `AudioTrack` log
  line: 0 / 1 / 0 / 0 / 1 for open, first change, second change, leave, pause.

## Loading, and nothing appearing in one frame

The owner's rule: with animations on, nothing may appear or change in one frame, and waiting must
look like waiting. The pieces:

- `LoadingDots` (Design.kt): three breathing dots, Apple's lyric-interlude mark. Invisible for the
  first 250 ms, then fades in, so fast loads never show a loader. Used by `LoadBox` (every page's
  loading state, whose content now fades in over it) and by the lyrics, placed where the first line
  will be.
- `Modifier.loadingSheen`: a faint band crossing a cover's plate while it loads, same 250 ms grace.
  Every `Cover` uses it, fades its picture in (260 ms, skipped for a picture already in memory) and
  fades in the note glyph when there is no picture.
- The player's sleeve (`SleeveArt`) keeps the old cover while the next one loads and cross-fades;
  after 600 ms without it, the old one fades out to the sheen so it never stands under the wrong
  title. The page colours cross-fade with it and hold the old palette while the new one is worked out.
  `PlayerViewModel` warms the covers of the previous track and `Prefs.coversAhead` (default 3)
  upcoming ones, taking shuffle into account via `PlayerState.nextIndex` / `previousIndex`.
- A sideways swipe on the sleeve is a carousel (`SleeveCarousel`). Each record is the cover's whole
  square (wider than the screen, so at rest the screen crops it to the sleeve); held, it lifts - shrinks
  to 86 % of the width, rounds, casts a shadow - so its cropped sides come into view. The neighbour's picture waits off
  the edge and follows the finger in; on commit the old record goes all the way off, and the incoming
  picture stays drawn over the sleeve until `SleeveArt.shownUrl` matches it (`snapNext` makes that
  load skip its cross-fade), so the change has no second step. A swipe back is `previousItem()`
  (always the previous song), not `previous()` (which restarts the song past 3 s).
- Lyrics go back through the loader on every song (`PlayerViewModel.lyrics` starts each song from
  `Loading`), and an empty server answer is not emitted while a lyrics service may still answer. The lyrics
  loader sits centred, where "No lyrics" would be.
- `PlayPauseGlyph`: play, pause and the buffering spinner cross-fade, the spinner only after 300 ms.

## Animation and Android's animation setting

Compose scales every animation by Android's animator duration scale. Plenty of people switch that off
for speed (and GrapheneOS users often do), and at 0 every tween finishes on its first frame - the
owner's lyrics jumped from line to line on the phone while gliding on the emulator. `reduceMotion()`
also followed that switch. `Prefs.ignoreSystemMotion` ("Animate even when Android's are off") makes
`reduceMotion()` ignore it. It is on by default (stored as "animateAnyway"): the owner's phone has
Android's animations off, and the app looked broken there; Reduce motion in the app still turns them off. The speed itself is app-wide: `MainActivity` builds the window's
recomposer with `AppMotion` (a `MotionDurationScale`) in its context, and every animation in the
composition, ours and the libraries' (page transitions, sheets, fades, the lyrics), reads its scale
from there - the system's normally, 1 when the switch is on. It replaced a per-animation override
that only the few animations wrapped in it obeyed. Measured with the system scale at 0: the sheet
opens through intermediate frames with the switch on and in one frame with it off; earlier, off, a
lyric line change was over in 66 ms with 60 % of the movement in one frame, on, 600-730 ms with no
frame over 20 %.

Measure motion with `glide3.py`-style frame differencing (share of a change in its biggest frame),
not by matching vertical shifts: lyric lines are evenly spaced, so "moved one line" and "did not
move" look the same to a shift search.

## The back gesture

Predictive back is on (`enableOnBackInvokedCallback`), so Navigation Compose scrubs a page transition
with the finger. Unless `NavHost` is given `predictivePopEnterTransition` / `predictivePopExitTransition`
it uses its own defaults - the page being left scales to 70 % with no fade over the page underneath,
already fully drawn - which is what the owner saw as the animation "breaking" on a back swipe.
`PredictiveBack` in App.kt is a linear fade-through: the page leaving is gone by 60 % of the way and
slides a fifth of the width towards the edge the finger moves to; the page underneath fades in over the
second half. Cancelled, it runs back. The player sheet takes the gesture itself (`SheetBack`,
`PredictiveBackHandler`): it sinks up to a fifth with the finger and closes on a stiffer spring than a
tap-close (`PlayerSheet.backClose`).

## Interface size

Every size above was measured on a phone 411 dp wide. The owner's phone is about 358 dp wide
(measured off a screenshot: the 64 dp lyrics thumbnail is 17.9 % of its width against 15.6 % on the
emulator), which is Android's display-size setting, and at that width every dp-sized thing is a
seventh larger - the whole app looked zoomed in. `Prefs.uiScale` defaults to automatic, which scales
the app's density so it lays out as if the screen were at least 411 dp wide (`Theme.uiScale`). It
only ever shrinks, and it leaves the system font scale alone, since that one is the reader's.
Checked with `adb shell wm density 483`: pause glyph and title margin come out at the same share of
the width as at 411 dp. `wm density reset` afterwards.

## The album page's seam

An album page has no wash (see above) and no separate gradient under its artwork either. It used to:
the picture faded onto the cover's edge colour and a second gradient below carried that on to the
page colour. But the parallax slides the picture *down* over that gradient as the page scrolls —
`translationY = 0.4 * scroll` — squeezing it into a few dozen pixels, and a colour ramp that steep
across the full width is a line. The dissolve now happens entirely inside the artwork, which has its
own height to do it in, and finishes on the page colour, so there is nothing left to hand over to.
Because the artwork's layer is alpha-faded by the same parallax, its last row is the page colour at
any scroll and at any fade.

Measure this, do not eyeball it. `edges.py` in a scratch directory is twenty lines: sample only the
far left and right margins, where no text or control ever sits, average 14 rows either side of each
candidate, and report a step of 10 or more. Averaging is what makes it useful — it ignores row
dividers and glyph edges, which are one or two pixels tall, and finds the things that are not. Run it
on the player and on the album page at three or four scroll positions; anything it reports inside the
artwork's own rows is the cover's own contrast, not a fault.

## What the audio path costs

`tools/bench.sh dev.nori.music 90 off`, same album, fresh install, on an x86_64 emulator, before
this work and after it:

| | `2670679` (before) | `c9c6910` (after) |
|---|---|---|
| CPU | 3.04 % of one core | 2.82 % of one core |
| wakeups | 411 /s | 425 /s |
| quiet seconds | 75 of 90 | 72 of 90 |
| PSS | 239 MB | 225 MB |

Within the noise of a debug build on an emulator, and "quiet" stays around the 80 % the house rules
ask for. The work added nothing per buffer or per frame: the audio track provider runs once when a
track is opened, and `Outputs.usb` only emits when something is plugged in. With the page wash and
the playing bars on top of it the numbers are the same again — 72 of 90 quiet seconds, 392 wakeups a
second, and no UI thread anywhere in the busiest list, because neither runs while the screen is off.

## What the page costs to scroll

`tools/scroll.sh dev.nori.music 12` on an album page, music playing. The emulator renders in
software, so the absolute numbers are dreadful and only the comparison means anything.

| | 50th | 90th |
|---|---|---|
| flat page colour | 73 ms | 93 ms |
| page wash on the album page | 77 ms | 97 ms |
| as shipped (wash on the player only) | 69 ms | 93 ms |

Measured on the same build by making `derive` hand back a null wash, which is the only honest way to
compare. While the album page carried the wash it cost about 4 ms a frame to scroll, roughly 5 % —
the emulator's software rasterizer, where a full-screen textured fill is expensive and a gradient is
not; on a GPU a second full-screen quad is nothing. It was taken off that page for how it looked
rather than what it cost, and scrolling is back at the baseline. The player never scrolls.

Two traps this measurement fell into, both worth knowing:

- **Wake the device first.** `tools/scroll.sh` straight after `tools/bench.sh` swipes at a black
  screen and reports `Total frames rendered: 0`.
- **Do not compare runs with different frame counts.** A run with the playing bars animating rendered
  647 frames against 366, and the extra cheap animation frames pulled the percentiles down to 65/85 —
  which read as "the wash made scrolling faster" and was nothing of the sort. Scroll an album whose
  track is *not* the one playing, so the frame population is the same on both sides.

## Not done

- **The vocal gate cannot tell a voice from a pad.** It reads the voice band's share of the power; a beatless pad
  intro reads as sung. The candidates (a separation mask, a singing classifier) and their licences are in
  `docs/research/analysis.md`. The new vocal separation only acts on what this gate calls sung, so on real songs
  it will miss most pairs of singers until the gate is better; the harness measures it with the voices taken from
  the truth.
- **AutoMix's new transitions have not been heard on a phone.** Listen for: the drop landing on the swap with the
  outgoing song let go a beat later (a DJ move, or a jump?); the vocal duck and high-pass ride between two sung
  songs; up to 15 s of an instrumental intro skipped to reach a drop; leaving on a closing breakdown or before a
  short hidden track. One thing to watch in the logs: the transition engine drops the skipped remainder of the
  outgoing song by decoding through it and the skipped start of the incoming one the same way, so leaving before
  a hidden track after minutes of silence needs the whole rest of the file decoded (and, when streaming, fetched)
  within the hold's runway; if it is not, the hold lets go and the held ending plays unmixed, as it does when the
  next song is late.
- **Bit-perfect at 24 bit.** media3's sink writes 16-bit or float and nothing else, so a DAC that
  only offers bit-perfect modes at 24 or 32 bit is told so rather than driven. Feeding one needs an
  integer output path: an audio processor at the end of the chain that widens to
  `ENCODING_PCM_24BIT_PACKED`, and the sink opening the track at that encoding. `DacState.blockedBy`
  already says this to the user.
- **Apple's Human Interface Guidelines were never read.** The numbers in `Design.kt` were measured
  off screenshots, not taken from the type scale, standard margins, separator insets and row heights
  under `https://developer.apple.com/design/human-interface-guidelines/`. Do not invent numbers: a
  wrong one is worse than a missing one, because it gets implemented literally.
- **The album page's Play pill** is a solid fill of the page accent. Apple's is a translucent
  capsule with the accent as its content colour. Unverified against a real screenshot of the album
  page — the App Store set below does not include one.

## Reference material

Apple's own App Store assets (iOS 26 era). Fetch with a browser User-Agent; each base URL takes a
trailing size segment, so request them large.

```
https://is1-ssl.mzstatic.com/image/thumb/PurpleSource221/v4/09/d2/62/09d262c5-7fb2-0e48-6545-4a3e16dffb14/iPhone6p9-iOS26-USEN-Music-Wrapper1.png/1290x2796bb.png
.../PurpleSource211/v4/62/61/29/626129da-b55a-c00a-7629-da095e947ba9/iPhone6p9-iOS26-USEN-Music-Wrapper2.png/1290x2796bb.png
.../PurpleSource221/v4/66/f9/e7/66f9e733-0174-ee61-1963-0ce4ceb518a6/iPhone6p9-iOS26-USEN-Music-Wrapper3.png/1290x2796bb.png
.../PurpleSource221/v4/c5/f0/46/c5f046ff-4e33-2395-2c4f-9ae04149801b/iPhone6p9-iOS26-USEN-Music-Wrapper4.png/1290x2796bb.png
.../PurpleSource221/v4/c7/aa/89/c7aa89f7-b2c0-58de-bc20-8a710dc90671/iPhone6p9-iOS26-USEN-Music-Wrapper5.png/1290x2796bb.png
.../PurpleSource221/v4/67/04/a6/6704a634-d243-e850-2b93-fd38c1915e62/iPhone6p9-iOS26-USEN-Music-Wrapper6.png/1290x2796bb.png
```

Wrapper 3 is the lyrics view, 4 is the player, 2 shows a playlist, 5 and 6 the mini player and tab
bar. They are listed on `https://apps.apple.com/us/app/apple-music/id1108187390`, so the list can be
rebuilt if those URLs rot. The images are not committed: ~29 MB of someone else's copyrighted
marketing material, and one curl away.

## How to work on this app

`tools/app.sh` drives a **debug** build over adb without touching the screen, which is the only
reliable way to test this app — see the warnings below. `open <route>`, `play "search:…"`,
`do "download album:<id>"`, `do "dac <spec>"`, `set limiter true`, `state` (one JSON line: route,
playback, DSP, downloads, lyrics, DAC). `tools/audio-e2e.sh` and `tools/feature-e2e.sh` build on it.
`tools/apk.sh` builds a release APK for a phone (arm64 by default; it lands in `build/`, and
**never** copy it into the user's home directory — they asked for that explicitly).

### Testing a USB DAC without a USB DAC

One cannot be attached to an emulator, so `app.sh do "dac Topping E30@44100/16,96000/24"` points the
app at a fake one offering exactly those bit-perfect modes, and tells `Outputs` a USB device is
attached. `dac off` hands it back to the audio system. The state dump then answers `dac`,
`bitPerfect`, `dacModes`, `dacBlocked`, `dacTrack` and `offloadWanted`, which is enough to check the
whole decision — including the part that was actually broken, offload standing down. The checks at
the end of `tools/feature-e2e.sh` do this.

What a mock cannot prove is that a real DAC makes a sound. When the hardware is to hand, plug it in,
play something, and read Settings, then Sound: the line under the toggle now says what the AudioTrack
was opened with and whether it was offloaded.

### Testing this app is full of traps

Every one of these produced a wrong conclusion in an earlier session:

- **The media session's position does not move while music plays.** Periodic updates are switched
  off deliberately to save wakeups. A test that watches it passes in silence.
- **A `MediaController` in the background reports a stale position**, so the app's own numbers lie
  too while it is not on screen.
- **`TransitionSink.bytesWritten` (the burst count) is honest but bursty** — ten seconds of audio are written at once, then
  nothing for about eight. A three-second sampling window sees zero and calls it silence.
- What can be trusted: `adb shell dumpsys audio` showing our `AudioTrack` as `state:started`, plus
  sink bytes measured over a full buffer cycle. `tools/audio-e2e.sh` does exactly this.
- **A sleeping device answers `uiautomator dump` and `screencap` with stale content.** Taps go
  nowhere and the screenshots look plausible. Check `dumpsys power | grep mWakefulness` first.
- **The on-screen keyboard eats automation.** A fling across it is glide typing. `tools/ui.sh kb off`
  disables every IME for a run; `input text` does not need one.
- **The offline check needs a long song.** `feature-e2e.sh` downloads a random song and samples the
  AudioTrack a dozen seconds after starting it. It once drew a thirteen-second interlude, found the
  track legitimately stopped, and reported that downloads do not play offline. It now picks one of at
  least 90 s. Before believing a failure there, look at the duration it printed.
- **The DAC mock names the output too.** `do "dac Topping E30@..."` makes `Outputs.current` read
  `USB: Topping E30` as well as setting the USB flag, so the player's output glyph and anything bound
  to that output can be checked on an emulator.
- **`do star` with no argument does nothing** — the verb needs a song, as in `do "star song:<id>"`.
- **The emulator's own settings are not the defaults.** Crossfade and crossfeed left switched on
  there mean offload can never be asked for, so a check of the offload rules measures nothing until
  they are turned off.
- **`tools/perf-suite.sh` installs `app-release.apk`** — it now builds it first, but if you change
  that, remember two of its runs once measured a day-old binary.

### Bug classes this codebase keeps producing

Check for these before believing a screen is fine — each has bitten more than once:

1. **`Surface` with a computed colour and no `contentColor`.** Material resolves it to
   `Color.Unspecified` and text inside renders almost black, so a title ends up dimmer than its own
   subtitle. Always pass `contentColor`.
2. **Gestures keyed on something that changes while dragging.** `pointerInput(list)` restarts when
   the list reorders and the drag dies; tracking a dragged item by index has the same effect. Key on
   a stable identity, or do not reorder until the finger lifts (the queue does the latter).
3. **State read at composition time inside a `pointerInput(Unit)` block.** The block is created once
   and keeps the values it captured. Read `MutableState` or a view model at event time, or keep the
   flag inside the gesture block itself (`dragsSheet` keeps its velocity tracker there).
4. **Sentinels used in arithmetic.** `AudioSink.getCurrentPositionUs` returns
   `CURRENT_POSITION_NOT_SET` (`Long.MIN_VALUE`) when stopped; subtracting it produced an enormous
   "already buffered" figure and the sink refused audio for ever, which is what made playback silent
   after a background pause.
5. **Player or `DownloadManager` touched off the main thread.** Both throw. `PlayerConnection.with {}`
   posts to the main looper; new code that reaches a media3 object directly must do the same. The
   audio track provider runs on the playback thread, so anything it triggers posts to `main` first.
6. **Scrolling screens under the floating chrome** need `LocalChromeInset.current` as bottom content
   padding, or their last row cannot be reached.
7. **Two clocks in one subtraction.** The sleep label subtracted `System.currentTimeMillis()` from a
   deadline set with `SystemClock.elapsedRealtime()`, got a number about fifty years wide, and the
   `coerceAtLeast(1)` after it turned that into "1 min" for every timer ever set. Anything stored as
   a deadline in this codebase is elapsedRealtime; read it back the same way.
8. **Ranking with a catch-all that beats the default.** `Outputs.rank` gave unlisted device types 5
   and the built-in speaker 9, so a phone's telephony output — every phone has one — was reported as
   where the music was going. Unlisted types now rank below the speaker.
9. **Implementation detail leaking into user-visible text.** The settings copy has been cleaned once;
   new strings keep reintroducing threads, buffers and "Rust core".

## Standing instructions from the owner

- Commit messages: one line, no attribution lines, no `Co-Authored-By`.
- One emulator only: `battery-perf`. No extra AVDs, not even for subagents; they take turns on it.
- Do not run the long performance suite for small or UI-only changes — build, install, screenshot.
  Measure only when something can plausibly move CPU or battery.
- Build artefacts stay in `build/`. Never copy them to `~`.
- The look is Apple Music: artwork bleeding into the page, floating chrome, no Material defaults. No
  liquid glass, and nothing that reads as an Android navigation bar — a patch behind the selected tab
  was rejected three times, in circle and rectangle form. It is colour, weight and size only.
- Do not fake word-by-word lyric timing. The sweep runs only when the lyrics genuinely carry per-word
  timings; line-timed lyrics simply light up.
- Test features end to end rather than declaring them done. A screenshot proves a screen renders, not
  that a feature works.
