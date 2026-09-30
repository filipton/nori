# Testing

Three tiers, from fastest to slowest. What decides how music plays, the queue, the library, lyrics and
settings is Rust and is tested by `cargo test`, on a virtual clock where time matters. The device checks
cover only the Android glue: what needs an AudioTrack, MediaCodec, media3, the media session and its
notification, audio focus, routing, JNI, the service and a force stop.

| Tier | What | Time | Who runs it |
| --- | --- | --- | --- |
| `cargo test -j4 --workspace` | every Rust test | about 30 s (build warm) | every agent, every change |
| `tools/smoke.sh` | launch, login, play, pause, seek, next, a queue edit, one AutoMix transition, the equalizer tuned in place, offload on and off, the notification's pause and play, a download played offline, no crash or ANR | aimed at 2-3 min (fixed sleeps: 0; not yet timed on a device) | every agent that touched Android, on its emulator turn |
| `tools/audio-e2e.sh --only …`, `tools/feature-e2e.sh --only …` | the sections of the area a change touched | a section is 10-90 s | the agent that touched it |
| `tools/audio-e2e.sh`, `tools/feature-e2e.sh` in full | every device check | not yet timed; the old suites were 11+ and 10+ min with ~590 s of fixed sleeps | the coordinator, once per batch, before a perf APK build |

Never `-j` above 4: the machine runs out of memory.

## Running the device checks

```sh
tools/smoke.sh                                  # the real server in ~/.music.pass
NORI_E2E_SERVER=local tools/smoke.sh            # tools/dev-server.sh's Navidrome, through tools/lying-proxy.py
tools/audio-e2e.sh --list                       # the sections
tools/audio-e2e.sh --only tuning,restart        # just those
tools/feature-e2e.sh --only lyrics-services     # an opt-in section: never part of a full run
```

All three take `--only <section>[,<section>]` and `--list`, exit non-zero on a failure, print the time
each section starts at, and use `ANDROID_SERIAL` (emulator-5554 unless set). They share
`tools/e2e-lib.sh`: `wait_for <field> <value> <timeout>` polls `tools/app.sh state` until a field reads a
value (`!v` for anything but, `>n` / `<n` for numbers), `wait_until <timeout> <command>` until a command
succeeds, `sounds` / `silent` until the app's own AudioTrack is started or not. There is no fixed sleep
where a condition can be awaited. The two that stay are real durations being measured: 20 s paused in
the background (the system has to treat the app as idle), and the ten-second burst cycle
(`bursts_continue` waits for the next top-up, up to 15 s).

### The local server

`NORI_E2E_SERVER=local` runs everything against `tools/dev-server.sh`: a Navidrome on port 4533 (Docker or
Podman; one already answering on 4533 is used as it is), its music and database in the main checkout's
`.dev/`, shared by every worktree. It seeds what the checks need beside its generated albums:

- `Nori E2E / Long Album`: twelve songs of 80-135 s, every third one FLAC (the bridge, album pages, AutoMix).
- `Nori E2E Two / Far Side`: three songs of 150-190 s off another album (a crossfade between two albums,
  since an album's own songs join gaplessly).
- `Nori Bench 1000`: a playlist of a thousand songs (the server's own, over again if it has fewer), for
  opening and scrolling a long page: `tools/open-bench.sh <package> "Nori Bench 1000" 10 3` from Library >
  Playlists, on a perf or release build, prints the frames and janky frames of each open and of the flings.

The app talks to it through `tools/lying-proxy.py` on port 4534 (`http://10.0.2.2:4534` from the
emulator): a transcoded stream is stated a quarter longer than its bytes, the connection closes short, and
a range past the real end is answered 416, as a server answering `estimateContentLength=true` does when
the transcode comes out smaller than estimated. audio-e2e's `transcode` section plays such a song past its
real end. The generated songs have no lyrics anywhere, so the `lyrics` section only runs against the real
server. Other songs come through the proxy at about 4 MB/s, so a download can be seen running beside
another. Navidrome itself states the estimate only on a transcode it has not cached yet (a cached one comes
chunked, with no length), so the proxy makes every transcode overstated, every time. The real server stays the default; never play or stream an `ext-` item on it.

The proxy also stands in for octo-fiesta asked for a provider's song it cannot fetch: `NORI_HANG=id1,id2`
(`NORI_HANG_MODE=headers|body`) at its start, or `curl 'localhost:4534/_hang?ids=id1,id2&mode=body'` at any
time, makes those songs' streams never answer (or answer and never send), and `curl localhost:4534/_hang`
says how many hang now and how many the app closed. What the engine does with such songs is tested in Rust
(crates/engine tests/hung.rs); the device check is only that media3 and OkHttp let go of a request the engine
calls off (the count hanging now falls as the songs are skipped), which only the real stack shows.

## What moved to Rust and what stays on the device

Every check the two device suites had on 2026-09-25 (110 call sites: 41 in audio-e2e, 69 in
feature-e2e). **(a)** is behaviour Rust owns, removed from the device once a Rust test covers it; **(b)**
is Android glue and stays on the device. 50 moved, 60 stay.

### tools/audio-e2e.sh (21 moved, 20 kept)

| Check | Kind | Now |
| --- | --- | --- |
| plays a song | b | audio-e2e `play`, smoke `play` |
| media key pause / resume | b | audio-e2e `transport` |
| resume after 20 s paused in the background | b | audio-e2e `transport` |
| pause and resume with the screen off | b | audio-e2e `transport` |
| still playing after eq / offload on and off | b | audio-e2e `processing`, smoke `eq`, `offload` |
| still playing after limiter / mono / autoMix switched | a | engine.rs `the_limiter_and_mono_switched_while_playing_keep_the_music_going_and_are_heard` (new), `automix_switched_on_while_playing_mixes_out_of_the_song_playing`, chain.rs `settings_changed_while_playing_never_click` |
| the limiter only catches peaks | a | chain.rs `the_limiter_only_catches_peaks`, engine.rs `the_limiter_and_mono_…` (new) |
| a crossfade is planned for the next boundary | a | crossfade.rs `a_crossfade_is_planned_for_the_next_boundary_and_heard_exactly_there`, `a_song_put_next_is_planned_into_at_once` |
| the bar walks steadily through the held ending | a | crossfade.rs `the_bar_walks_steadily_through_the_held_ending`, paths.rs `through_a_mix_the_place_read_belongs_to_the_song_read_with_it` |
| the sink reaches the mix | b | audio-e2e `crossfade`, smoke `automix` (the mix heard on a real track) |
| the next track arrives in time to be mixed | a | crossfade.rs `a_crossfade_is_planned_for_…`, engine.rs `a_crossfade_is_heard_as_the_simulated_player_hears_it` |
| the ending is not let go for want of it | a | crossfade.rs `a_crossfade_is_planned_for_…`, gapless.rs |
| the next track plays out of the mix | b | audio-e2e `crossfade`, smoke `automix` |
| a crossfade is planned past the new song too | a | crossfade.rs `a_song_put_next_is_planned_into_at_once`, `switched_on_mid_song_it_is_planned_for_the_song_already_playing` |
| a scrub into the mix stays to hear the ending; the mix still fires; the next plays out of it (3) | a | crossfade.rs `a_scrub_into_the_mix_stays_to_hear_the_ending_and_the_mix_still_fires`, engine.rs `a_seek_into_a_mix_further_from_the_end_than_the_player_reads_ahead_plays_on_into_it` |
| with it off, the planner says so | a | crossfade.rs `with_it_off_the_planner_says_so_rather_than_going_quiet` |
| taking the EQ out is heard at once, no swap deferred | a | engine.rs `an_equalizer_switched_off_while_playing_is_heard_at_once_where_the_ear_is` |
| still playing after the EQ leaves | b | audio-e2e `eq`, smoke `eq` |
| still playing across the boundary | a | engine.rs `two_songs_join_sample_for_sample_and_each_is_fetched_once`, `an_equalizer_switched_on_while_playing_is_heard` |
| still playing after tuning cuts in; shallow buffer in place; deep buffer back in place; track not reopened; still playing after the deep swap (5) | b | audio-e2e `tuning`, smoke `eq` (crates/android track.rs resizes the real AudioTrack) |
| the deep buffer came back without waiting for a boundary | a | engine.rs `the_equalizer_screen_makes_the_output_shallow_at_once_and_deep_again_as_it_closes` |
| measuring starts when AutoMix is switched on; the songs coming up are measured (2) | b | audio-e2e `automix` (read out of the media3 cache through measure.rs's JNI) |
| the mix is planned from what was measured | a | automix.rs `switched_on_it_measures_what_comes_up_and_mixes_on_the_beat`, core.rs |
| speed runs through the engine's stage; still playing at 1.5x (2) | a | engine.rs `a_speed_set_while_playing_is_heard` |
| 1.5x plays 6 s as ~9 s of song | a | stages.rs `at_one_and_a_half_times_six_seconds_play_nine_of_the_song_at_its_own_pitch` |
| pitch alone keeps the pace | a | stages.rs `pitch_alone_keeps_the_pace` |
| silence skipping runs; still playing while skipping (2) | a | stages.rs `skipping_silence_takes_out_the_pause_and_keeps_every_note`, engine.rs `silence_skipping_switched_on_while_playing_skips_the_silence_ahead` |
| next track plays | b | smoke `controls` |
| previous track plays | a | controls.rs `next_and_previous_start_their_song_at_its_first_sample`, queue `previous_reads_the_setting_itself` |
| a seek while paused after a restart sticks; play resumes from it (2) | b | audio-e2e `restart` (a new service restoring the saved queue) |
| no playback errors | b | audio-e2e `errors`, smoke `crashes` (over the whole run's log now) |
| (new) a transcode stated longer than it is plays past its real end | b | audio-e2e `transcode`, local server only |

### tools/feature-e2e.sh (29 moved, 40 kept)

| Check | Kind | Now |
| --- | --- | --- |
| lyrics arrive for a well-known song | b | feature-e2e `lyrics` (real server only) |
| sweeping only claimed for real word timing | a | crates/lyrics formats.rs, every format's test asserts `synced`/`word_timed` (`lyricsfile_edges`, `ttml_line_timed_plain_laid_out_and_broken`, ...) |
| lookups off: only the server's lyrics | a | lyrics_sources.rs `the_reputable_services_are_on_out_of_the_box_best_first` (no service under the lookups switch) |
| each lyrics service answers (a report, no check) | - | feature-e2e `lyrics-services`, opt-in: 16 × up to 12 s, the services' health rather than the app's |
| no video player while off; let go when put away (2) | b | feature-e2e `motion` (ExoPlayer, Kotlin) |
| starring reaches the server | a | client.rs `a_heart_a_new_playlist_and_now_playing_are_asked_as_subsonic_says` (new) |
| the server is told what is playing | a | the same (new), scrobble.rs `scrobbling_off_sends_nothing` |
| the notification has a heart and a shuffle; its heart stars the song; it redraws; its shuffle toggles (4) | b | feature-e2e `notification` |
| the notification's star reaches the server | a | client.rs `a_heart_…` (new), shown.rs `the_session_buttons_say_what_a_press_does` |
| the second tap puts it back | a | stars.rs `only_an_unstarred_mark_removes`, shown.rs (the section still puts it back) |
| the pill reads Play / Pause; shuffle lights; stays Pause; a second press turns it off; pill pauses; reads Play; Play picks up (8) | b | feature-e2e `album-page` (taps on the real screen) |
| without drawing a new queue; without restarting it (2) | a | pages.rs `the_big_buttons_answer_for_the_pages_own_queue`, controls.rs `shuffle_switched_on_keeps_the_song_playing_first_and_play_next_next` |
| (new) only the page a queue was started from answers for it, through edits and a restore; another page sharing the song reads Play | a | queue playlist.rs `a_page_is_the_one_playing_only_when_the_queue_came_from_it`, core playlist.rs `the_queue_is_saved_and_put_back_with_the_page_it_came_from`, cli tests.rs `a_queue_started_from_a_page_carries_the_page` |
| a downloaded song plays with the network off | b | smoke `offline` |
| downloads stand in while the server is out of reach; they play; the album comes back with the network (3) | b | feature-e2e `bridge` (the network really going and coming) |
| at the song that could not play | a | core bridge.rs `a_bridge_is_started_and_undone_over_the_core_queue`, queue `the_bridge_step_follows_the_setting_and_the_parked_song`, paths.rs `a_song_the_network_will_not_bring_is_handed_to_the_offline_bridge` |
| tapping the download notification opens the queue | b | feature-e2e `download-notification` |
| several songs download at once; the notification's own id; picks up after a force stop; the album finishes (4) | b | feature-e2e `downloads` (media3's DownloadManager) |
| the batch reports a speed; an ETA (2) | a | transfers.rs `two_songs_running_give_the_batch_a_speed_and_a_time_left` (new) |
| a new playlist reaches the server; the song went in; the check cleans up (3) | a | client.rs `a_heart_a_new_playlist_and_now_playing_are_asked_as_subsonic_says` (new) |
| adding to the queue grows it; play next grows it (2) | a | queue `edits_say_where_the_songs_went`, controls.rs `an_edit_ahead_of_the_song_playing_…`; smoke `queue` keeps one on the device |
| the favourites tile opens; lists what the server starred; starring adds; unstarring removes; tapping a row plays it; with the mix around it; the mix has songs (7) | b | feature-e2e `foryou` (the screen) |
| a mix stays the same when opened again | a | board.rs `draws_follow_the_period_and_again_redraws`, `favourites_follow_the_marks_and_each_core_has_its_own_board` |
| a transition is planned at a track boundary (real album) | a | crossfade.rs, automix.rs, engine.rs `automix_switched_on_while_playing_mixes_out_of_the_song_playing`; smoke `automix` hears one |
| the songs coming up are measured before they are played | b | audio-e2e `automix` (it was checked twice) |
| the queue is carried on past its last song; a whole album when albums are chosen (2) | a | core autofill.rs `similar_songs_skip_what_is_queued_and_keep_their_order`, `an_album_prefers_one_with_a_side_to_it_and_skips_the_seeds`, `a_radio_with_nothing_similar_goes_on_with_random_songs`, queue `when_to_refill_is_read_off_the_queue` |
| a next pressed at the queue's end skips once the songs land, only if they land within 2 s of the last press; mashed, it is one skip; songs for an end that moved stay out | a | queue autofill.rs `a_next_at_the_end_skips_only_when_the_songs_come_soon`, `songs_fetched_for_an_end_that_moved_stay_out`; player queue.rs `a_next_at_the_end_expires_and_counts_once` |
| the playing song's cover comes after fast skips, loads answered out of order, a failed load asked again once | a | covers loader.rs `a_cover_let_go_of_while_fetched_and_asked_for_again_comes`; app CoverFetchTest |
| offload asked on the phone; stands down for USB; the DAC is seen; audio flows to it; bit-perfect engages; says what the track was opened with (6) | b | feature-e2e `dac` |
| a DAC this app cannot feed says why; is not claimed bit-perfect (2) | a | dac.rs `a_missing_mode_says_why`, `nothing_usable_releases_and_says_why` |
| a device with a profile gets it on connect; the sound from before comes back without it (2) | b | feature-e2e `device-sound` (the platform's output events) |
| flat turns the equalizer off; on again on the speaker (2) | a | profiles.rs `choices_from_the_device_list`, `an_unbound_device_gets_the_sound_from_before_back` |
| AutoEQ: asking first leaves the sound; yes applies; automatic applies; undo; not switched again (5) | a | device.rs arrival tests (`CurveStep::Offer` / `Apply`), profiles.rs `adopting_a_curve_saves_binds_and_loads_it`, `undo_puts_everything_back`, `a_quiet_device_is_never_offered_a_curve` |

### What could not move

- The AudioTrack itself: whether it is started, its bursts, its in-place resize for tuning, offload onto
  the chip, a DAC's format. The engine is tested against a simulated track (crates/android track.rs,
  crates/engine paths.rs), which is what the Rust side decides; the platform's answer only a device gives.
- Media keys, the notification's buttons, the media session, the screen off, the background: Android's.
- media3: downloads, its cache (AutoMix measures songs out of it), and a transcode's stated length on a real
  HTTP stack.
- The screens: taps on the album page and the For you pages go through Compose and the ViewModels.
- A force stop: the service restoring the queue, downloads picking up.
- The network going and coming (the bridge) and a notification's intent opening a screen.
- The app updating itself: which release, which APK and whether to say so are nori-core's (update.rs, tested
  there); the download, PackageInstaller's session, its confirmation and "install unknown apps" are Android's.
  A debug build checks by hand with `app.sh do "update 0.3.0"` (it pretends to be 0.3.0, finds the latest
  release newer and shows the banner; a debug build never installs, its button opens the release's page). An
  install in place needs a release build that is older: `./gradlew :app:assembleRelease -PpretendVersion=0.3.9
  -PrustTargets=x86_64`, signed with the release key, installed, then About, Check for updates, Update.


## cargo test

`cargo test -j4 --workspace` runs about 1,120 tests in about 30 s once built (the test binaries 28.5 s
of it). It was 163 s before the sound code was built optimised for tests, 57-89 s after that, and 30 s
since the tests stopped waiting on real time (2026-09-26). `[profile.test.package.…]` in Cargo.toml
builds nori-player, nori-engine and every dependency at opt-level 2 for `cargo test` only; debug
assertions and overflow checks stay on. The workspace's own crates that are not listed there (nori-core,
the android crate) are unoptimised, so their tests keep their data small.

Test binaries run one after another and the tests inside a binary side by side, so a binary takes as long
as its slowest test or its total over the cores, whichever is more. The binaries that matter:

| Binary | Tests | Time | Its slowest |
| --- | --- | --- | --- |
| crates/engine `--test engine` (engine.rs, paths.rs, radio.rs, tempo.rs, stretch.rs, estimated.rs, hung.rs, silent.rs) | 141 | 10 s | offload tests of a few ffmpeg songs, 4-7 s each, and next pressed fast through four queues, 6 s |
| crates/player `--test pipeline` | 67 | 4.6 s | levels.rs, 3-4 s each: every kind of transition through two 60 s songs |
| crates/player lib | 262 | 3.6 s | automix/tests.rs, the synthetic songs analysed side by side |
| crates/android lib | 40 | 8 s | track.rs's two tests of the real engine thread on the wall clock, the rapid skips over a phone-like track 6 s |
| crates/engine `--test one_fetch`, `--test core` | 1 each | 1-2 s | one core and one queue per process, so a binary each |
| everything else | about 600 | under 1.3 s a binary | |

### What the output should have played

`crates/engine/tests/common/reference.rs` renders offline what a card should hear when the sound changes
mid-play: the chain's input (a song, or the transition engine's output as a plain run hears it) through
the equalizer, silence skipping and speed as first set, switched at the input frame the engine logged for
each change, spliced in where it logged it and blended as the ring blends. engine.rs's
`*_changes_seamlessly*` tests compare the card with it sample for sample (EQ, compressor, speed, silence
skipping, in a crossfade, in a stretched AutoMix, right after a seek, paused, on a device holding seconds),
so a repeated, lost or clicking stretch fails them; `reference::clicks` finds jumps where no exact
rendering exists.

On Android the handover between two AudioTracks is heard the same way in `crates/android/src/track/air.rs`:
tracks mixed a period at a time with the mixer's volume ramps, presented after the output's latency,
their timestamps read as the output reports its periods, fed by a ring whose frames carry their numbers.
It checks that every frame is heard once, with no silence, level or click, and the tracks' alignment,
for outputs from 5 to 40 ms periods and Bluetooth's latency, a slider drag, a pause or a jump at any
moment of a handover, and a second track that won't open.

### The host perf report

`tools/perf-host.sh [rev] [runs]` plays minutes of music through the engine on the test's clock, for this
checkout and for another revision (the latest release tag unless named), and prints per minute of music:
the engine's wakes (what keeps a phone's CPU from deep idle), the process's CPU time and its allocations,
for plain playback, the equalizer on, and crossfades. The bench is `crates/engine/tests/perf_bench.rs`, an
ignored test; the script gives an older revision its files. No device: it answers "did a change make the
player wake, work or allocate more", on the machine it runs on, and only a comparison made there means
anything (the allocated bytes include the test card's own record of every sample, alike on both sides). The
battery and deep idle of a real phone stay the owner's `tools/bench.sh`.

### What each crate's tests cover

| Crate | What is checked |
| --- | --- |
| player | the sound chain sample by sample (decoders, ReplayGain, equalizer, limiter, speed, silence skipping), AutoMix's analysis on synthetic songs with a known tempo, key and structure, the planner (never panicking over any stored row: automix/plan_fuzz.rs), the mixer, and the whole player on a simulated output and virtual clock (`sim`, tests/pipeline): gapless joins, crossfades, levels through a mix, controls, the output |
| engine | the player for platforms without one, on the virtual clock of tests/common: loading and the loader (source.rs), fetching ahead (ahead.rs), the stream cache, offload onto a simulated chip (paths.rs), radio, tempo, the place said through a tempo-stretched mix measured against the song heard (stretch.rs), a transcode's estimated length and its 416 (estimated.rs), a player that never plays silent (silent.rs: a panic on its thread, a loader that dies, an output that stops taking music), one fetch per song with AutoMix measuring (one_fetch.rs), transition settings changed while playing (replan.rs), an album kept gapless under AutoMix or a crossfade heard to every sample, the core's planner and measurer included (album.rs), downloads and the core (core.rs) |
| core | the FFI surface over the real SQLite: the index and search, smart playlists, mixes, history, lyrics' race, covers, AutoEQ and device profiles, the Subsonic client against a fake transport (offline writes and their replay, the address in use, login), stream addresses, transfers, the car's tree, the Kotlin twins (tests/twins.rs), the active client (tests/active_client.rs), a listen kept by its profile (tests/scrobble.rs), whole flows against a Subsonic server kept in memory: log in, index, search, offline changes replayed in order, a radio (tests/scenario.rs) |
| lyrics | every lyrics format, the services' answers, trust and fitting, the race between services, synced times checked against synthetic sung songs (sync.rs; `sync_eval` prints the table) |
| covers | decoders against Pillow's references, the scaler, the disk and memory caches, the loader's workers |
| look | colours from a cover (the AndroidX palette port), Compose's colour maths, the lyrics and motion layout |
| library, queue, settings, transfers, devices, perf | pages, menus, the queue's rules, settings and their store (tests/stored_format.rs: the stored keys and encodings against a golden copy), the download table, AutoEQ, the perf log's report |
| cli, android, mpris, net, http, db | each client's own glue: the terminal's drawing, the AudioTrack model (track.rs), every JNI door's signature against its Kotlin `external fun` (lib.rs), media controls, requests |
| testdir | the tests' temp directories, and tests/registered.rs: every file under a crate's `tests/` is in a test binary |

The Kotlin unit tests (`./gradlew :core:testDebugUnitTest :app:testDebugUnitTest`, a minute, no device)
cover what the Kotlin keeps: formatting, the media3 error reading, the frame fades, the queue panel's keys, drag and undo (QueueEditsTest), the player's cover across
fast skips (CoverTurnTest, CoverFetchTest: loads answered out of order, cancelled, failed and asked again), and InitOrderTest, which
reads Nori.kt and Downloads.kt and fails when a field a constructor's thread reaches is declared after the
line that starts the thread (the start-up NPE of 2026-09-25).

### Reading a failure

- An assert says what was wrong and with what: expected and actual, the song ids, positions in ms or
  frames, and for the engine the event log (`{:?}` of `rig.events`) or the planner's log (`app.log()`).
  Read the last event before the failure first: `Error`, `Stopped` or `Bridge` there is the cause, the
  assert is the symptom.
- `the engine did not go to sleep` (tests/common) means the engine thread is stuck or spinning, not slow:
  the limit is 120 s of real time.
- A test on the virtual clock that fails only under load has a real thread in it that the clock does not
  see. The clock stands still while the engine waits for bytes (`BYTES_WAIT`, 2 s of real time; 20 ms when
  a server is itself waiting for the clock through `Virtual::wait_until`). Make the thread wait for a
  condition, not for time.
- A test file that is not in any binary never runs: testdir's `every_file_under_a_crates_tests_is_built_into_a_test_binary`
  names it. crates/engine has `autotests = false`, so a new file there needs a `[[test]]` or a `#[path]`
  line in tests/main.rs; a new file in crates/player/tests/pipeline needs a `mod` in main.rs.

### Writing one

- No `thread::sleep` as a wait or as proof that something did not happen. Wait for a condition with a
  deadline that panics saying what did not come; prove "nothing more" with a barrier (the next job on a
  single worker, a later warm-up) and `try_recv`.
- On the engine's clock, time moves only when the test moves it (`Rig::run`, `wait_for`, `until`). The
  loader's tries again after a dropped connection wait 1, 2 and 4 s of real time in the app;
  `Virtual::default()` shortens them through `nori_engine::source::set_retry_wait_ms`.
- Songs are made once per binary: `music()` in engine.rs and pipeline/common.rs, `steady()` in levels.rs
  and the ffmpeg encodes in paths.rs and estimated.rs are cached by length and seed.
- Anything process-wide (the core's queue, the active client, the AutoEQ fetch, the watch hook) is either
  its own test binary or held to what the test itself made (the hook's records filtered by the engine's
  thread id).
- A table of cases that each take seconds runs its cases on threads of their own (`each` in
  automix/tests.rs) or is split into one test per row, so they run side by side.
