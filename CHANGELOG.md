# Changelog

What changed in each release, newest first. Sections are grouped from the commit log by
`tools/changelog.py` — `feat:` becomes Added, `fix:` becomes Fixed, and so on — then read over by
hand. Versions follow [semantic versioning](https://semver.org).

## [Unreleased]

## [0.4.8] - 2026-09-29

### Added

- A mix's page shows its covers as one picture, blurred into the page like an album's, upright and on its side
- With a black background mix pages follow the albums and playlists choice, as the playlists they are
- On its side a cover is a wider band that reaches in under the words and controls beside it, going soft over a wider edge

### Fixed

- On its side the library's album cards are large again, filling the row
- On its side shelves run to the screen edges under the camera and the tab rail, fading out smoothly on both sides
- On its side the player's controls stand in the middle of the screen's height
- On its side the page leaves the tab rail its strip from the first frame, so shelves never show unfaded as the app opens

## [0.4.7] - 2026-09-29

### Added

- The app keeps a day of its log and a report of a problem shares it, with drop-outs logged (#17)
- Sideways polish, status bar and keep awake settings, original quality on mobile data (#18)

### Changed

- State handed through globals, magic numbers and strings made explicit across the core, the player and the clients

### Fixed

- Downloads and their analysis keep the CPU awake with the screen off (closes #12)
- The keyboard no longer comes up over the player opened from search (closes #16)
- The album autoplay carries on with stays gapless with keep albums gapless on
- A song waiting only for its beats no longer crashes the downloads screen, the phase is one enum
- The desktop app keeps one downloader and an album added to the queue whole stays gapless
- A heart pressed on one server no longer shows on another, the marks are the core's
- A nameless DAC or Bluetooth device is no longer searched in AutoEQ by its placeholder name
- An Android player going no longer stops the fetching ahead of the one that replaced it
- The music is no longer left silent while it plays, the chip follows every fade and a fade left down is brought back

## [0.4.6] - 2026-09-28

### Added

- **desktop:** A native Slint desktop player over the core and nori-engine, laid out as the terminal client
- **desktop:** The window looks and behaves like Apple Music on macOS: Skia on Metal, the system's sidebar blur, SF Pro, a floating player, Now Playing, the native menu bar
- **desktop:** The sidebar runs the window's full height over the system's sidebar material, its rows and the page where they were
- **desktop:** Settings, synced lyrics, long lists that open at once, a smooth seek bar and Apple's animations
- **desktop:** The player floats on the system's Liquid Glass, artist pages open on their picture, the sidebar takes the page's colour
- **desktop:** Music's own colours measured from the Mac, album headers and buttons as Music has them, the player at its size
- **desktop:** Our own Liquid Glass: the window is drawn by our compositor, the page seen through glass shaders under the sidebar and the player
- **desktop:** The sidebar's glass is lit by what lies beside it on the page, softly and well into the pane
- **desktop:** Settings centred in four tabs as the Mac's own, the sidebar's edge soft
- **desktop:** Lyrics and the queue docked on the right as Music's; Now Playing rebuilt, its lyrics sharp only on the line sung; bigger icons
- **desktop:** Every setting the Android app has that a Mac can use, in its words: an equalizer page, lyrics sources in order, accounts
- **desktop:** A Find field in the toolbar, Artists in two columns, the playlist as a table, an account row
- **desktop:** Lyrics fill word by word and a click on a line seeks to it, the queue's rows fold and open, the traffic lights level with the toolbar as Music's
- **desktop:** The line sung as the Android app draws it: a soft-edged fill, each word and syllable rising as it is sung, a held note swelling
- On its side the app puts the tabs on a floating rail at the left, Search on its own below them
- On its side the player puts the cover, lyrics or queue on the left and the controls on the right
- On its side an album, artist or playlist page shows the cover and buttons on the left and the songs on the right
- On its side the tab rail stands at the right edge and pages keep clear of the camera's punch hole
- On its side the player's cover goes soft at its right edge into a wash under the controls, and lyrics and queue keep clear of the punch hole
- Album, artist, playlist and mix pages drop the back button over the cover, the back gesture leaves them
- On its side an album, artist or playlist page shows its cover as the player does, soft to the page from the screen's edge, with the name and buttons beside it
- On its side the tab bar keeps its size and order and only its glyphs turn, and a tinted page leaves the screen's edges with it

### Fixed

- **desktop:** The player's glass shows over a plain page, as a faint lit pane with a rim
- **desktop:** The sidebar's light from the page is faint and fades within a few dozen points of its edge
- **desktop:** The sidebar's light is a soft glow, gathered from the page blurred until no edge is left
- **desktop:** The player's glass is nearly invisible, as Music's: a neutral veil, no colour of its own, a faint rim
- **desktop:** The player's glass is clear as Music's: a light blur, the page's own colours, a thin quiet rim
- **desktop:** Covers on screen are never let go, so they stop flashing, and the ones on screen load first
- **desktop:** A word-timed line fades from white as the next one is sung, rather than dimming at once
- **desktop:** American spelling, as the app and the terminal client have it
- Shuffle albums keeps each album gapless instead of mixing between its songs
- Turning the phone keeps the player, its lyrics and the text size as they were
- On its side a page's cover fits beside the buttons, Play fits Pause, and the library grid keeps its upright card size
- On its side the player's status bar shade fades with the cover's soft edge instead of ending on a line
- On its side a page's cover is a card level with the first song, clear of the back button, and the artist page no longer crashes
- On its side the player keeps its title and controls in place for the lyrics, and clear of a punch hole on the right
- On its side the now playing bar has no wash under it, which ran over the cover and stopped at the camera
- On its side the now playing bar sits level with the foot of the tab rail

## [0.4.5] - 2026-09-27

### Fixed

- A song tapped again in its album no longer leaves the app paused over the music
- Shuffle albums and shuffle songs keep going after the queue ends, autoplay on or off

## [0.4.4] - 2026-09-27

### Fixed

- The app stays upright until it has a landscape layout
- Songs that left the queue no longer stay drawn over it
- Shuffle albums plays at once and never a provider's songs
- Tapping the song playing in its album keeps it going and opens the player
- Closing the queue dissolves and the title row moves with the controls

## [0.4.3] - 2026-09-27

### Added

- **cli:** TUI rewritten as a desktop music player: sidebar, pages, panel, player bar, equalizer console, card covers; late terminal replies over ssh no longer taken as keys
- Playlist descriptions can be turned off, and the server's auto-import notes are hidden
- Album and playlist pages share one keep-the-cover's-colours choice with a black background
- Home's menu shuffles songs or whole albums instead of one shuffle everything
- The app icon's shortcuts open Search or shuffle songs or albums

### Fixed

- The app and the terminal client use American spelling (color, favorite, license, analyze)
- A shuffle keeps going the way it started, random songs or whole albums, whatever autoplay adds

## [0.4.2] - 2026-09-27

### Added

- Every download is analysed after it is saved, and the beat model can read downloads with consent

### Fixed

- An artist's biography opens in full on a tap instead of stopping at four lines
- A stop the engine made before a play already sent no longer leaves the app paused over music
- **build:** Tools/bump-version.sh leaves nori-uniffi-jni-runtime at upstream's version in Cargo.lock
- Remote (octo-fiesta) songs: autoplay after them, library only, album cloud clears

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
