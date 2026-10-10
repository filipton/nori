# Motion: every animation in the app, and the state of each

The owner asked for every animation to be looked at and polished, one at a time, starting with the
page transitions ("all of them just animate from the top of the page, it doesn't seem natural; back
needs to be gesture-natural"). This file is the list, so the work can be picked up by anyone: what
each animation is, where it lives, what it does now, what it should do, how to check it, and whether
it is done. Update the status line when an item changes.

The rules these sit under (from `AGENTS.md`): nothing animates unless the user touched it; everything
is read in the draw phase (`graphicsLayer`, `drawBehind`), so a running animation redraws a layer and
recomposes nothing; `reduceMotion()` / `AppMotion.reduce` must turn every one of these into a snap or
a short fade. The feel is Apple Music's, not Material's.

## How to look at one

The emulator drops frames and `screencap` lands wherever it lands, so record and tile:

```sh
(adb shell screenrecord --time-limit 5 --bit-rate 8000000 /sdcard/m.mp4 &); sleep 1.2
adb shell input tap X Y            # or a swipe: adb shell input swipe x0 y0 x1 y1 ms
sleep 3.5; adb pull /sdcard/m.mp4 /tmp/m.mp4
ffmpeg -y -i /tmp/m.mp4 -vf "fps=30,scale=216:-1,select='between(n\,30\,53)',tile=8x3" -frames:v 1 /tmp/tile.png
```

`tools/app.sh open <route>` opens a page without touching the screen; `tools/app.sh open player`
opens the sheet. The back *gesture* is `adb shell input swipe 5 1200 500 1200 350` (from the left
edge); the back *button* is `adb shell input keyevent BACK`.

## The list

Status is one of: **todo**, **doing**, **done (commit)**, **leave** (looked at, fine as it is).

### 1. Page push and pop — `App.kt` `PageMotion`, `NavHost` enter/exit/popEnter/popExit

- Now: every page change, either direction, is the same: the new page fades in while dropping from a
  twelfth of the screen *above* its place (220 ms, decelerate); the old one fades out lifting up. Both
  pages fade, so for a few frames neither is opaque and the window's black shows through - a
  fade-through-black, and the "from the top of the page" the owner means.
- Should: a stack, the way iOS pushes. Push: the new page slides in from the right edge, opaque, the
  whole width, over the old page, which moves a third of the way left underneath and darkens under a
  scrim. Pop: the reverse. No fade on the pages themselves (the pages are opaque; a fade is what made
  the earlier sideways slide read as "flying out of the top left corner"). About 350 ms, decelerating hard (most of the travel in the first half).
- Check: home → tap an album → back button. Both pages must stay opaque throughout; the album must
  arrive from the right, not from above; the home page must be visibly *under* it, shifted, and come
  back to place on pop.
- Done as: `PageMotion` (push/pop slides, 200 ms tap / 140 ms gesture finish, sharp `Settle` ease; tab roots cross-fade) and `Page`
  (paints the page's background while it moves, and the scrim on the page underneath, as a child of
  the page's own transition so the gesture scrubs it). Every route goes through `page(route)`.
  Two things the old fades had been hiding had to go with it: the home page replayed its sections'
  staggered arrival on every return (now once per process, `HomeScreen.arrived`), and `LoadBox` faded
  a page's content in from the page's background even when the data came within a quarter second
  (now a snap inside `QUICK_LOAD_MS`; the fade is only for a page that really showed its loader).
- Known: the first ~100 ms of a pop are lost to composing the page that is coming back (Compose
  Navigation does not keep it); on a debug build on the emulator that is most of the slide. Judge
  the pop on a release build (`tools/apk.sh x86_64 --install`). The push shows the page's bare
  background for as long as its data takes to arrive - see item 16.
- Status: **done**

### 2. The back gesture — `App.kt` `PredictiveBack`, `NavHost` predictivePop*

- Now: a linear fade-through scrubbed by the finger: the page leaving fades out and lifts a tenth of
  the height; the one underneath fades in and drops from above over the second half.
- Should: the pop of item 1, scrubbed: the page follows the finger to the right, the page underneath
  slides from a third left back to place and its scrim lifts. Let go, it finishes; cancelled, it runs
  back. Nothing fades.
- Check: album → swipe from the left edge slowly and hold: the album should sit part way off to the
  right with home visible and shifted under it. Release: it completes. Swipe and swipe back: cancels.
- Done as: the pop of item 1 with `LinearEasing` over 140 ms (`PageMotion.popEnter/popExit(scrubbed = true)`);
  `PageMotion.scrubbed` tells `Page` to run the scrim linear too. Finger scrub seeks; 140 ms is only
  the leftover after release / cancel.
- Status: **done**

### 3. Tab switches — `Nav.tab`, same `NavHost` transitions

- Now: Home → Library etc. use the same page transition as a push. Tabs are siblings, not a stack, so
  a direction is wrong here (and Apple Music does not animate a tab change at all).
- Should: no direction: a short cross-fade (about 150 ms) or nothing. Told apart from a push by the
  route: the four tab roots are `home`, `search`, `library`, `settings`, and a transition between two
  of them is a tab change; anything else is a push/pop.
- Check: tap Library then Home: nothing slides.
- Done as: `PageMotion.tab()` - both routes in `roots` - returns a 100 ms fade and sets `pop = null`
  so neither page is dimmed.
- Status: **done**

### 4. Previous / next buttons pressed quickly — `PlayerScreen.kt` `SleeveCarousel`, the `asks` channel

- Now: presses queue (a channel of four); each waits for the record before it to finish sliding,
  hurried up to three times as quick. Four fast presses are four full slides, so the records go on
  scrolling after the finger has stopped - it reads as lag, and the owner said so.
- Should: spammable with no backlog. A press while a record is in flight commits that change at once
  (the song changes now) and the record coming in carries straight on from where it is to become the
  one going out - one continuous scroll, the motion never more than one slide behind the thumb, and
  it stops when the presses stop. A press on a record at rest is the same full slide as now.
- Check: on the player, tap next five times in about a second. Five songs later, and the records
  stop moving within about a third of a second of the last tap. `tools/app.sh state` shows the index.
- Done as: the channel and `queued` are gone. `slide.run` cancels the slide in flight
  (`moving.cancelAndJoin`) and starts a new `land`; `land`'s cancel path sees `presses` has moved on
  and commits the change with the record left where it is (`arrive(offset - go * span)`), and the new
  slide starts from that offset with the speed the record had (`speed`, written by the animate
  callback) at 1.5x the button stiffness. The wait for the player to catch up before a slide
  (`stale`) is 250 ms, down from 500. `PlayPauseGlyph` shows pause while buffering with play-when-ready
  (it flashed the play arrow after every skip). Checked with adb: 5 taps 180 ms apart → 5 songs, last
  slide over ~400 ms after the last tap; swipe, tap-then-swipe, prev-then-next all count once each.
- Known: between two records with different covers the slide waits for the player to answer the
  change before it (the neighbour's cover is read from `state.index`); on the debug emulator that is
  100-250 ms of the record sitting lifted, on a phone much less. Removing it means the carousel
  looking further down the queue than one song.
- Status: **done**

### 5. Player sheet open / close — `PlayerSheet.kt`, `App.kt` `PlayerLayer`, `FlyingCover`

- Now: a critically damped spring (stiffness 420, 700 on a back-gesture close) carrying the finger's
  velocity; the page behind darkens to 45 %; the sheet's corners round in flight; the cover flies from
  the now playing bar's thumbnail into the sleeve. Recently tuned with the owner.
- Should: as is. Look only if something else changes under it.
- Status: **leave**

### 6. Player panel change (artwork ↔ lyrics ↔ queue) — `PlayerScreen.kt` `arrival`, `PanelFlight`, `PANEL_MS`

- Now: a 360 ms dissolve driven by one `Animatable` for the whole screen; the cover flies between the
  sleeve and the lyrics header's thumbnail; the transport is shared (not faded). Recently tuned.
- Should: as is.
- Status: **leave**

### 7. Record swipe on the sleeve — `SleeveCarousel` (finger), `land`

- Now: follows the finger, decides at a third of the width or a 1000 px/s flick, springs at 520 (a
  finger) or 950 (a button). Recently tuned; a same-album swipe bug was fixed in the last commit.
- Should: as is.
- Status: **leave**

### 8. Now playing bar swipe — `Chrome.kt` `SwipeCarousel`

- Now: the same as 7 at the bar's size, spring 560.
- Should: as is.
- Status: **leave**

### 9. Home page sections arriving — `HomeScreen.kt` `rememberArrival`, `Arrival`

- Now: on the first composition of the page the sections fade up and rise into place one after
  another over about a third of a second. Runs once per visit to the route.
- Should: with item 1 the page itself now slides in, and a second movement inside a moving page is
  busy. Keep it for the app's first page only (a cold start, when there is no push), and skip it when
  the page arrives by a push or a pop. Decide after seeing 1 on the device.
- Check: cold start → home: the sections stagger in. Album → back → home: they do not.
- Done as: `HomeScreen.arrived`, a process-wide flag; the run happens the first time only.
- Status: **done**

### 10. Album / artist / playlist page — `HeroPage.kt`

- Now: nothing animates on arrival; the cover parallaxes and fades with the scroll (draw phase).
- Should: as is; the push (item 1) is the arrival.
- Status: **leave**

### 11. Loaders — `Components.kt` `LoadBox`, `Design.kt` `LoadingDots`, `loadingSheen`

- Now: content fades in over the loader (260 ms after 60 ms); the dots stay invisible for the first
  quarter-second and never show for a fast load; the cover sheen only runs for a picture that is
  really on the way. A cover that does not come (offline at once, otherwise after its one try again)
  fades its sheen out over 300 ms as the plate's note fades in; one that comes later (its try again,
  or the network back) fades in over the note.
- Should: as is.
- Status: **leave**

### 12. Song menu — `SongMenu.kt` `NoriSheet`, the "More" row

- Now: Material's sheet spring; "More" expands in place (`expandVertically`), its chevron turns. A
  menu item that closes the menu slides the sheet down (it used to vanish), and the sleep choices,
  details and playlist picker come up as the sheet goes down (item 22).
- Should: as is unless the owner says otherwise.
- Status: **leave**

### 13. Lyrics — `LyricsView.kt` (line change, scroll)

- Now: the line arriving fades and rises 1/40 of its height over 420 ms after an 80 ms wait; the list
  scrolls with a tuned spring. Tuned with the owner on the phone.
- Should: as is.
- Status: **leave**

### 14. Seek bar, volume, switches, swipe-row actions, play/pause glyph, downloads screen

- `SeekBar` (one per-frame loop, eases to the position, done in the last two commits); `VolumeRow`
  (180 ms ease on an outside change); `NoriSwitch` (180 ms); `swipeActions` (spring back, colour and
  pop when armed); `PlayPauseGlyph` (fade + scale, 180/140 ms); `DownloadsScreen` (phase icons,
  progress ring, rows moving with `animateItem`). A song saved but still processing (lyrics, AutoMix
  analysis; at most 30 s) stays with the downloading ones: its second line cross-fades to "Finding
  lyrics…"/"Analysing…", its ring turns and the stop square fades; every section's rows are one call, so
  the line and the trailing mark cross-fade again as it moves to the finished ones.
- Should: as is; each answers a touch or a state change and does nothing otherwise.
- Status: **leave**

### 16. A pushed page shows its bare background until its data arrives — `AlbumScreen` and the other `LoadBox` pages

- Now: the page slides in at once (item 1) but everything in it is behind `LoadBox`, and until the
  server answers (100-300 ms on the emulator, longer on a slow connection) the card is the page's
  background colour with nothing on it. The old fade-through hid this; the slide shows it.
- Should: the page arrives with something on it: the hero's plate, the title if the row that was
  tapped knew it, the wash from a palette already measured for that cover (`CoverTint` cache). A
  skeleton, not a spinner.
- Check: home → tap an album with the network throttled (or a cold server): the card that slides in
  should not be a blank rectangle.
- Done as: `Nav.album(id, hint)` keeps the last few tapped `Album`s; `AlbumScreen` draws `HeroPage`
  from `nav.albumHint(id)` (cover, title, artist, year/count caption) as soon as it is composed, and
  fills in Play/shuffle/songs when `Load.Ready` lands. Deep links and "Go to album" from a song still
  have no hint and wait behind `LoadBox`. Call sites that have an `Album` pass it (home, search,
  library, artist). The same pattern is on artists and playlists (`Nav.artist` / `Nav.playlist`,
  `ArtistScreen` / `PlaylistScreen`); song-menu "Go to artist" passes a stub with the name it already
  knows.
- Status: **done**

### 15. Login → app

- Now: `App` composes `LoginScreen` or the app; the change is a cut.
- Should: a fade would do; once, on sign-in. Low priority.
- Done as: `Crossfade(prefs.loggedIn, …)` around the login and the signed-in tree, 280 ms (snap when
  reduce-motion). No direction - the two screens are unrelated.
- Status: **done**

### 17. Audit: nothing appears on one frame — hinted detail pages + player title

- Found (record tiles of home → album): with item 16 the hero is there from the slide, but
  Play / Shuffle / ⋯ still landed by replacing the heart-only row, and the tracklist appeared fully
  opaque on one frame once the server answered. Player title / artist / album also swapped hard
  when the song changed while the sleeve was still moving.
- Fixed: `HeroPage.awaitingPlay` keeps the transport row (disabled) and a 46 dp slot for ⋯ while
  the detail is on the wire; album / artist / playlist pass it from the hint path. Body content
  under the hero fades and rises once via `Arrive` (300 ms). Player title block cross-fades on
  song id (220 / 160 ms).
- Later (a thousand-song playlist stuttered as it opened): the body was one lazy item holding every row,
  so opening a long page composed and measured all of them inside the slide. Each row is now an item
  of its own (`songRows`), and the rise is one clock per page (`rememberArrival`) that each item reads in
  its draw phase (`Modifier.arriving`): the rows on screen still fade and rise as one block (320 ms,
  36 dp), a row scrolled to mid-run joins where the block is, and none is animated after. An artist's
  biography, links and top songs, which come after the page, fade in where they join (`animateItem`).
  `tools/open-bench.sh` counts the frames of opening a page ten times (docs/testing.md).
- Left: page content that is composed late mid-slide (Compose Navigation), player sheet contents
  that are already on the rising surface, Cover picture fade-in, loaders, lyrics, downloads,
  switches, seek / volume - already eased or intentional snaps under `reduceMotion`.
- Status: **done**

### 18. The sleeve's soft bottom through every move — `PlayerScreen.kt` `SoftSleeve`, `SleeveShade`

- Found: the owner saw the blur at the cover's foot "appear only after the animation ends" and
  "hide and appear" on a slow drag. The blurred band existed only on the resting sleeve, so it
  vanished on the first frame of a pull down or a lyrics change and came back on the last; on a
  record in the hand it stayed at full strength and smeared the lifted card's foot. The status bar's
  shade vanished and came back with the sheet flight too.
- Done as: one `SoftSleeve` (blurred copy plus rub-out, the band's tint still nori-look's through
  `BandEffect`) used by `SleeveCarousel`, `FlyingCover` and `PanelFlight`, each passing a blur strength
  from its own motion: `1 - lift` on a swipe, the second half of `sheet.progress` on the flight, the
  first 45 % of the lyrics flight. `SleeveShade` fades the status bar's shade the same way. The flights'
  layers are the sleeve's height rather than the screen's.
- Check: slow swipe held half way (no haze on the lifted card); slow sheet pull down and back up (the
  blur thins as the cover leaves and is whole when it lands, with no step at either end); artwork →
  lyrics → artwork. Checked on the emulator frame by frame (animations slowed five times): the blur
  goes as a held record lifts and returns as it settles, thins over a slow pull and comes back with the
  flight into the sleeve, and follows artwork → lyrics → artwork; no step at either end. Not yet on a phone.
- Status: **doing**

### 19. The moving cover — `SleeveMotion.kt` `MotionDirector`, `MotionCover`, `SleeveMotion`

- Now: with Settings, Look, "Moving covers" on, an album's Apple Music motion artwork plays in the
  sleeve over the still cover (a TextureView inside the showing record, inside `SoftSleeve`). It is the
  one thing that moves without being touched, like the playing bars, and only while the player is fully
  open on the artwork, the app is in front with the screen on, the record is at rest, no finger is on
  the player and motion is not reduced.
- Should: never appear or leave in one frame. In: 480 ms, only after its first frame is on the surface,
  and 350 ms after a move has ended (at once after a tap that moved nothing). Out: 160 ms, at a
  finger's first touch (ahead of a swipe, a skip, a pull on the sheet or a panel change), when the
  sheet leaves fully open, and on a new album before its video is loaded. Flights draw the still cover.
- Check: an album with motion artwork: open the player (the still cover comes alive after a beat);
  touch and hold the sleeve (the video steps back before the record lifts); swipe, skip with a button,
  pull the sheet slowly, go to the lyrics and back; turn the screen off and on. With reduced motion on,
  the cover stays still.
- A song that changes by itself (item 21) takes the video out with the record it played on, fading as
  it slides; it used to vanish on the slide's first frame, the still cover showing in its place. The
  surface is composed beside the records rather than inside the showing one, so it is never made again
  mid-move. With the moving covers on, a song change crashed the app: the lookup of an album without a
  video answers none, and the generated Kotlin asserted every async answer non-null (fixed in
  crates/uniffi-bindgen).
- Known: a very quick pull on the sheet or the back gesture can catch the video part faded, since the
  sleeve hands over to the flight's still cover on the first frame of a move; and the blurred band at
  the sleeve's foot is the still cover's (a surface is drawn once).
- Status: **doing**

### 20. Lyrics sung the way Apple's are — `LyricsView.kt` `SungText`, `drawSung`, nori-look `lyrics.rs`

- Now: the lit line's fill has a soft edge 24 dp wide instead of a cut through the letter; each timed
  word or syllable rises 2 dp as it is sung and settles over 420 ms once done; a note held 0.9 s or more
  swells 4 % with a soft light under it. Backing vocals are drawn smaller under their line and fill on
  their own times; a duet's other voice sings from the right, each side leaving a lane clear.
- The timings are nori-look's (`RISE_MIN_MS`, `SETTLE_MS`, `HELD_MS`, `GLOW_FADE_MS`, handed to Kotlin
  in `stage`), and so is when to draw: every display frame while a word of the sung line moves
  (`LyricTiming::moving`, the clock's `lively`), every second frame otherwise. The rise and glow
  themselves are per-frame animation maths, kept in Kotlin (docs/clients.md). Only the moving pieces are
  drawn on their own; the brushes are made once and moved, so a frame allocates nothing new. With
  movement reduced there is only the fill, at every second frame.
- Colours, as Apple's: every timed line is drawn by `SungText` with its own strength (the core's, moving
  over the change): sung words at that strength with a soft glow under them (the text's own shape
  blurred, following the fill's edge), unsung words of the line being sung at nori-look's `UNSUNG` (0.55),
  the other lines at 0.35 ahead and 0.19 behind. A line that stops being sung keeps its fill as it was
  last drawn and dims as a whole while the next brightens and the list glides: before, the finished line
  was swapped for a plain fully lit copy (a one-frame snap to white) and then faded. Lines timed by the
  line only are lit whole, with the same glow.
- Idle: while sweeping, the clock sleeps (in ms, `Step::still`) between words and after a line is sung
  instead of waking every second display frame. Measured on the emulator (60 Hz): about 60 frames/s
  while a word-timed line is sung with the rise on, nothing rendered while the lyrics are still or paused.
- Check: a word-timed song (English and CJK), a duet, a song with backing vocals, reduced motion on,
  and `dumpsys gfxinfo` while a line sweeps. Continuity is easiest to see with the app's "Animate
  anyway" off and Android's animator scale at 5, the app started after setting it.
- Status: **doing**

### 21. A song that changes by itself — `PlayerScreen.kt` `SleeveCarousel` (`natural`), `Chrome.kt` `SwipeCarousel`

- Was: the end of a song, gapless, a crossfade or an AutoMix switch dissolved the sleeve in place
  (480 ms) and cut the now playing bar, where a skip slides the records.
- Now: a step to the song either side slides the way a skip does - the record left goes out one side,
  the new one comes in lifted from the other and settles - on the sleeve and on the bar. It starts
  where the page changes song, which for a mix is where the incoming song becomes the louder
  (nori-player's heard.rs), and the page's colour fade (420 ms) runs alongside. Decided while
  composing, so the new song's first frame already has its record off the edge; counted in frames
  (`settleByFrames`), so the slow frame that composes a new song slows the slide rather than skipping
  it. Anything else (a new queue, a song tapped far down the list, covers not in hand, the player or
  the bar out of sight) changes as before; `reduceMotion()` keeps the snap.
- Check: AutoMix on, the player open, `tools/app.sh do "seek <ms>"` to 25 s before the end, record
  through the switch: the old record stays whole until the switch, then slides out as the new one
  slides in, once.
- Status: **done**

### 22. Dialogs, sheets and menus — `Overlays.kt` `NoriDialog`, `NoriSheet`, `AlertCard`; `res/values/themes.xml`

- Was: every dialog appeared and vanished on one frame with Android's window animation scale at 0,
  "Animate anyway" or not. A Compose Dialog is a window, and the window's enter/exit animation and its
  dim run on the window clock; and `if (open) AlertDialog(...)` drops the window the frame `open`
  turns false, so there was never an exit to play. Sheets closed from a menu item vanished the same way.
- Now: the app theme's `dialogTheme` gives every dialog window no animation and no dim. `NoriDialog`
  opens its own full-screen window, draws the scrim and moves the card on AppMotion (in: 240 ms, fade
  and 0.92 → 1 scale on the `Settle` ease; out: 170 ms; a page-style dialog rises 1/24 of the height
  instead of scaling; reduced motion: a 120 ms fade), keeps the last value on screen while it leaves
  and only then drops the window. `NoriSheet` does the same for Material's bottom sheet (`hide()` before
  it leaves). `AlertCard` is Material's alert card drawn in place (Material 1.4 keeps its own behind its
  window). A leaving dialog takes no taps. Closed, each is one remembered state and an early return.
  Dropdown menus are Material's own popup with its own transition, which already runs on AppMotion.
- Rule: no screen puts up `AlertDialog`, `ModalBottomSheet` or a raw `Dialog`; `OverlaysTest` fails
  the build's tests if one does.
- Check: all three scales at 0 (`adb shell settings put global animator_duration_scale 0`, and
  `window_animation_scale`, `transition_animation_scale`), record opening and closing: no frame where a
  dialog, scrim or sheet is fully there after nothing, or gone after fully there.
- Status: **done**

### 23. The selection bar — `SongMenu.kt` `SelectionBar`, `Overlays.kt` `NoriBar`; `App.kt` `SelectionBack`

- Was: `if (selection.isEmpty()) return` - the bar over the mini player appeared and vanished on one
  frame, and the mini player under it jumped by its height. Back while selecting popped the page and left
  the bar up on the page underneath; its "1 selected" was squeezed into a column one letter wide.
- Now: `NoriBar` opens the bar's height and fades it in on AppMotion (240 ms in, 170 ms out on the
  `Settle` ease; reduced motion: 120 ms linear), the bar rising from behind the mini player, which moves up
  with it; leaving, it shows the selection it had and takes no taps. Back with a selection clears it and
  stays on the page (not while the player is up); any change of page ends the selection
  (`Selection.onPage`, SelectionTest). The count is one line: "3 selected", or "3" where that does not fit.
- Check: all three animation scales at 0; long-press a row on an album page, record: the bar rises over
  several frames. Tap ✕: it sinks. Long-press again, press back: the bar sinks and the album stays. Long-
  press again, tap the Home tab: Home arrives and the bar sinks. On a 360 dp wide screen the count is one line.
- Status: **done** (not yet checked on the device)

### 24. A skip to a song whose cover is not loaded — `PlayerScreen.kt` `SleeveArt`, `rememberSleeveArt`, the page's colours; `CoverTurn.kt`

- Was: the owner saw "the same cover twice in a row". A record for a song whose cover was not in memory
  slid in as a plate (in the page's colour, the last song's), and on landing the sleeve under it went back
  to the last song's picture, which stayed for 600 ms (`sleeve_hold_ms`) before fading to the plate; the
  new one then snapped in. The page kept the last song's colours for 1.2 s (`colour_wait_ms`), snapped its
  text and buttons to the plain look while only the wash faded, and snapped from the plain page to the new
  colours when they came (a fade from "no colours" was no fade).
- Now: `CoverTurn` decides per cover address: a picture at hand shows with the change; one not at hand
  keeps what is on screen for a grace of 180 ms (`sleeve_hold_ms`, `colour_wait_ms` in nori-core's
  `stage`), so a cover read from the disk arrives without a placeholder blink, and then the last picture
  fades out (360 ms) to the placeholder: a plate in the theme's own surface colour with the loading sheen
  in the theme's ink, no song's colour. The picture fades in over it (320 ms), or over the last picture if
  it came inside the grace (480 ms). A picture for a song skipped past is dropped. A record that slid in
  without its whole picture leaves the sleeve at the plate at once (`SleeveArt.clearFor`; the old picture
  has slid away, there is nothing to hold), and the sleeve carries on the record's own fade when the
  picture comes. Neighbour records fade their picture in over the plate (320 ms) instead of swapping it.
  The page: after the same grace it fades (420 ms, by frames) to the plain page, and from there to the new
  colours, text and buttons with the wash; a fade cut short by the next change finishes from where it is.
  Colours measured while a record is already on its way are not brought up mid-slide; the page fades to
  them once the record is in.
- Found (2026-09-26, the owner's phone): after fast skips the sleeve stayed on the plate for the whole
  song. A load that came back with nothing was final (`CoverImage` went to MISSING and never asked
  again), and the loader could answer with nothing a view that asked for a cover just as a fetch of it
  that nobody waited for any more was ending. Now the loader ends such a fetch under the same lock as its
  look (nori-covers loader.rs), a failed load is asked again once after 2 s with the sheen still on
  (`CoverFetch`, CoverFetchTest), the perf build logs every failure with its reason ("cover: … did not
  load: …"), and a picture whose fade was cut short finishes it rather than stay faint over the plate.
- Check: with the network slow or a cold cover cache, on the player: tap next; swipe; tap next five times
  quickly. No frame shows the last song's cover in the middle after a record has landed; the plate is
  grey, not the last song's colour; the picture and the colours come in as fades. With the cover on the
  disk (played before, app restarted): no plate shows at all.
- Status: **doing** (not yet checked frame by frame on a device)

### 25. The queue's rows: swipe to remove, undo, reorder — `PlayerScreen.kt` `Queue`, `UndoPill`; `QueueEdits.kt`; `Components.kt` `swipeable`, `SwipeBackdrop`

- Was: only the × and the drag handle. Rows were keyed by their index, so a song taken out changed the key
  (and so the content) of every row under it in place, on one frame. Every row read the drag in
  composition, so each frame of a reorder recomposed every visible row; and on the drop the rows snapped
  back to their places and then swapped content.
- Now: a row swiped left, past the same third of the width as a library row (the same strip, words,
  tick and pop, in the page's colours: the veil, then the accent), slides off that side (200 ms) and is
  taken out; the rows under it close up (`animateItem`, 260 ms) while it fades where it was (160 ms,
  already off screen). Only leftwards: rightwards from the edge is the back gesture's, and the drag handle
  takes its own drags first. The song playing (as shown, and the queue's current while a mix hands over:
  the core's `QueueRows.kept`) is not swiped away: it gives a little ([GIVE], at most [GIVE_LIMIT] of the
  width), shows no strip and comes back. The × still takes any row. After either, "Removed “song” · Undo"
  rises over the foot of the queue for 4 s (240 ms in, 170 ms out, by frames; it keeps its words as it
  goes and takes no taps); Undo puts the song back through the core (`playlist_restore`: its index, its
  turn under shuffle, its hand mark) and the row slides back in from the side it left while the rows
  under it open up. Rows are keyed by song id and occurrence (`queueKeys`), so they keep their place and
  content through every change. The drag is read in each row's layer (`QueueDrag.shift`); the held row
  lifts and settles (150 ms), and on letting go settles into its slot (140 ms), the move is sent, and the
  rows stay where they are drawn until the queue has changed - that frame they are laid out where they
  were drawn, with no placement animation.
- Reduced motion: the swipe still follows the finger; a row taken out goes at once, the rows close up
  and the undo fades (120 ms) with no slide.
- Check: all three animation scales at 0 and at 5 on the app's clock (item 22's scales), the queue open:
  swipe a row left slowly past the third (tick, accent), let go; swipe one flicked; swipe the playing row;
  Undo; take two in a row and undo the second; reorder a row by three places and let go; turn shuffle on
  and take one out, undo. Frame by frame: no row appears or leaves on one frame, nothing jumps on a drop.
- Status: **done** (not yet checked on the device)

## Decisions and traps

- The owner rejected a sideways slide once because the pages faded while they slid, which read as
  the page flying diagonally out of the top-left corner. The push in item 1 must keep both pages
  opaque: slide and scrim only, no `fadeIn`/`fadeOut` on the page.
- `NavHost`'s default predictive-back transition scales the leaving page to 70 % over a page that is
  already fully drawn; always give `predictivePopEnterTransition`/`predictivePopExitTransition`.
- Compose scales every animation by Android's animator duration scale; `Prefs.ignoreSystemMotion`
  (on by default) makes the app ignore that. Test with the emulator's scale at 1 (Developer options).
- Nothing here may tick while music plays with the screen off (`tools/bench.sh`); a transition that
  is over is over, and `PlayerLayer` skips a fully covered page.
