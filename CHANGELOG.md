# Changelog

What changed in each release, newest first. Sections are grouped from the commit log by
`tools/changelog.py` — `feat:` becomes Added, `fix:` becomes Fixed, and so on — then read over by
hand. Versions follow [semantic versioning](https://semver.org).

## [Unreleased]

## [0.4.1] - 2026-09-27

### Breaking

- To update from 0.3.x, install 0.4.0 first; later versions no longer bring over its servers, settings and downloads

### Added

- Remote items show only the cloud, not the provider's name
- Nori tells you when a newer release is out, and updates itself from inside the app
- The queue opens on the song playing, with the songs already played dimmed above it
- Lyrics sync listens to the centre of the stereo image, so panned guitars no longer read as singing
- Headphones taken off pause at once, and can resume fading in when put back on
- A graphic equalizer beside the parametric one, and a compressor, virtualizer, bass boost and volume boost
- ReplayGain turns quiet songs up, levels to a loudness target and measures untagged songs
- Crossfeed presets, no processing per output, crossfade curves, a noise gate, a 5-band EQ and loudness compensation
- High quality output keeps the effects in float, 16-bit output is dithered, and a real resampler with a highest rate per output
- The tab bar takes the cover's colours, and black background can keep them per page

### Fixed

- Undo after taking a song out of the queue puts it back again
- Downloads say when they are still finding lyrics or analysing, and the time left counts down steadily
- **build:** The Rust tests build and pass on macOS
- The last lyric line dims once it is sung, and better-timed lyrics arriving mid-song no longer step back a line
- A new album started on its first song shows that song as playing
- **build:** The dev server keeps its database in a docker volume, as SQLite on a mac's shared folder corrupts it
- Songs of one album played one after another never mix with AutoMix, whatever their tags say, each next song from its start
- **build:** Tools/app.sh launches the app on emulators without hardware keys, where monkey refuses to run
- Headphone play and pause keys follow the fade setting, no resume switch
- A long beat-matched AutoMix no longer shows the next song early and replays its first lyric line
- Keep albums gapless only for an album played or added whole, other songs of one album mix
- A page started from a song row reads Pause at once, its origin is republished when the service sets the queue

## [0.4.0] - 2026-09-26

The player is rewritten: playback, the queue, the library, lyrics and downloads now run in a Rust core
shared with a new terminal client, and the Android app draws the screens.

### Added

- A new player engine in Rust replaces ExoPlayer's playback, with offload to the audio chip that lets the CPU sleep
- Better beat detection (the Beat This! model, downloaded from its authors on first use) for AutoMix
- A terminal client (nori-cli) that plays through the same core
- Swipe a song out of the queue, with undo
- Every lyrics service is on by default, word-timed lyrics are sought first, and synced lyrics are checked against the song's voice and shifted when early or late
- Downloaded songs keep their lyrics offline, and a finished download shows while its lyrics and analysis are found
- Updating from 0.3.4 keeps your servers, settings, downloads and queue

### Fixed

- Playback no longer goes silent after fast skipping, a hung remote song or an engine error, and a song never ends stuck at its last second
- Transcoded songs play whole and start at once on mobile data
- Albums stay gapless with AutoMix on, across discs too
- The seek bar stays right after the app comes back and through tempo-matched mixes
- The first equalizer change plays on with no gap, and dialogs, sheets and the selection bar animate even with system animations off
- Covers never show the previous song's picture, playlist covers show offline, and a cover that cannot load settles on a placeholder

### Performance

- Far fewer wakeups while playing, and offload tops up only when the chip asks
- Less memory: songs already cached are read from disk, covers ahead wait on disk, and the beat model hands its memory back
- Long playlists open smoothly

## [0.3.4] - 2026-09-22

### Added

- Favourite messages replace each other at once and can be switched off
- Automix beat-matches band-played songs on their intro and outro grids
- About page with build details and third-party licences

### Fixed

- No dropout at the end of an AutoMix, and the bar moves to the next song as soon as it is heard
- Light and dark covers keep their own colour, and a big light strip at the bottom gets a light page
- The page moves to the next song when its mix is heard and never flips back
- Light pages keep the cover's own lightness instead of fading to white
- A song skipped to at another sample rate is converted instead of stuttering
- Taps on the full-screen player no longer reach the page underneath

## [0.3.3] - 2026-09-22

### Added

- Album and playlist pages pause their own queue, and Shuffle shows when it is on
- The player's cover blurs as it melts into the page
- Settings pages are split into short named sections, with plainer descriptions

### Fixed

- Page colours follow what the cover really is: a big field of colour beats a black or white border, and a dark strip at the bottom no longer leaves a band
- White covers keep a white page
- Controls stay readable on white pages, and the colours change smoothly between songs
- The player's controls follow the cover's colour while you swipe, with no jump halfway

### Performance

- Snappier page push and pop, and a quicker finish to the back gesture
