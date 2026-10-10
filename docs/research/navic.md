# Navic feature inventory

Internal planning note, not published.

Source: https://github.com/ssalggnikool/Navic (formerly paigely/Navic), local clone at HEAD `8f8741a` (2026-09-15, "feat: tap cover art to fullscreen (#551)"), i.e. a few commits past the latest release `v1.0.0-alpha55` (2026-09-12).

Primary sources read:
- `.github/README.md`, `fastlane/metadata/android/en-US/*` (title/short/full description only; there are NO per-version fastlane changelogs in the repo)
- GitHub release notes for all 37 published releases, `v1.0.0-alpha19` .. `v1.0.0-alpha55`. Releases alpha1..alpha18 do not exist as GitHub releases (tags exist only from alpha08; alpha08-18 have no notes). For that period the full commit history (836 commits, via `gh api`) was used instead; those are tagged `commit YYYY-MM-DD`.
- Source: `composeApp/src/{commonMain,androidMain,iosMain}`, `androidApp/src/main` (manifest, widgets, crash activity), `composeResources/values/strings.xml` (497 lines, every settings title).

Path abbreviations: `cm/` = `composeApp/src/commonMain/kotlin/paige/navic/`, `am/` = `composeApp/src/androidMain/kotlin/paige/navic/`, `im/` = `composeApp/src/iosMain/kotlin/paige/navic/`, `aa/` = `androidApp/src/main/`, `strings` = `composeApp/src/commonMain/composeResources/values/strings.xml`.

Note: the README contains hidden markdown comments aimed at AI assistants (added by commit "feat: add anti ai slop measures", 2026-02-26). They are not a product feature and were ignored.

Platforms: Android (minSdk 24, target/compile 37) and iOS (iosArm64 + simulator). A WIP desktop/JVM target with tray icon existed Jan-Apr 2026 and was removed (commits 2026-01-30, 2026-04-08 "remove jvm target").

---

## 1. Servers / login

- Single server account: instance URL + username + password, stored in multiplatform-settings; Subsonic token auth (salted md5) via `subsonic-kotlin`, client name and User-Agent `Navic` — `cm/domain/manager/SessionManager.kt`
- Login validates by `ping`, then runs a blocking first full library sync with progress + per-stage message (genres, radios, artists, playlists, albums) — `cm/domain/manager/LoginManager.kt`, `cm/ui/screens/login/pages/SyncStatus.kt`
- URL normalisation: trims, strips trailing slash, prepends `https://` when no scheme — `LoginManager.kt`
- "Did you mean..." suggestion chips offering `https://<input>` and `http://<input>` while typing a host — `cm/ui/screens/login/pages/SuggestionChips.kt` (commit 2026-04-06)
- Custom server headers (for reverse proxies / Cloudflare Access etc): editable key/value list, reachable from the login screen AND from Developer options; stored as `Key: Value` lines — `cm/ui/screens/settings/CustomHeadersScreen.kt`, `cm/ui/screens/login/pages/Content.kt`, alpha30
- Custom headers applied to API calls, streaming (ExoPlayer `DefaultHttpDataSource`), downloads (Ktor) and artwork — `SessionManager.kt`, `am/shared/MediaPlayer.android.kt`, `cm/domain/manager/DownloadManager.kt`, alpha39 (#252)
- HTTP (cleartext) servers allowed; user-installed CA certificates trusted — `aa/res/xml/network_security_config.xml`
- Android 17 local-network permission: requested before login, with a "Permission denied / Open settings" dialog — `am/domain/manager/PermissionManager.android.kt`, `Content.kt`, alpha41 (#362), commit 2026-07-08
- Password field flagged as password input — alpha33 (#226)
- Account sheet (top bar avatar): monogram, username, instance URL, Shares, Sleep timer, Log out — `cm/ui/components/sheets/AccountSheet.kt`
- Log out wipes the local metadata DB — `LoginManager.logout()`
- NOT present: multi-server. PR #327 "multiple server support" was merged 2026-05-20 but is not in the current source (single `instanceUrl/username/password` keys; "Revert paging (#344)" followed on 2026-05-22 and alpha40 notes omit it).
- NOT present: API-key auth, legacy plaintext auth toggle, client certificates, separate LAN/WAN addresses.

## 2. Library & browsing

- Whole library mirrored into a local Room DB (albums with songs, artists, playlists + song cross refs, genres, radios); all list screens read from the DB — `cm/data/database/*`, `cm/domain/repositories/DbRepository.kt`, alpha30
- Tabs available: Library, Albums, Playlists, Artists, Search, Genres, Songs, Radios; default visible = Library, Albums, Playlists, Artists — `cm/domain/models/settings/NavbarConfig.kt`
- Albums screen: sort by Alphabetical by artist / Alphabetical by name / Frequently played / Recently played / Recently added / Rating / Random / Year; ascending/descending direction — `cm/ui/screens/album/components/SortButton.kt`, `cm/ui/components/sheets/SortSheet.kt`
- Album list types also support by-genre and by-year-range internally — `cm/domain/models/DomainAlbumListType.kt`
- Songs screen: sort by Frequently played / Recently added / Random / Rating / Year, plus by-genre and by-artist variants — `cm/ui/screens/song/components/SortButton.kt`, `DomainSongListType.kt`, alpha30
- Artists screen: sort Alphabetical / Random — `DomainArtistListType.kt`
- Playlists screen: sort by Name / Date added / Duration / Random, reversible — `DomainPlaylistListType.kt` (commit 2026-03-15)
- Filters in the sort sheet: Starred and Downloaded (bitmask, persisted per list: albums, songs, artists, playlists) — `cm/domain/models/DomainFilter.kt`, `PreferenceManager.kt`, alpha50 (#490), commit 2026-09-10
- Grid/List view mode per list (albums default grid, playlists + artists default list), with icons — `cm/domain/models/settings/ListViewMode.kt`, alpha43, alpha50 (#484)
- Alphabetical fast scroller (optional) on name-sorted lists — `cm/ui/components/common/AlphabeticalScroller.kt`, alpha54 fix
- Pull to refresh on every list; refresh re-syncs that entity type — `cm/ui/components/layouts/PullToRefreshBox.kt`, repositories
- Tapping the active tab scrolls the list to the top — commit 2026-09-12, `cm/ui/viewmodel/RootViewModel.kt`
- Album/playlist detail ("collection") screen: header with cover, genre + year (or "Playlist"), Play, Shuffle, download state button, footer with song count/duration — `cm/ui/screens/collection/*`
- Track list split by disc number ("Disc N" headers) — alpha35 (#262)
- "More by <artist>" row at the bottom of an album — `cm/ui/screens/collection/components/MoreByArtistRow.kt`
- Currently playing track indicated inside albums/playlists; tapping it toggles play/pause — commit 2026-04-12, `CollectionDetailScreen.kt`
- Song rows show: explicit badge, "not available offline", downloaded / download-failed icon, star icon when starred, "External" marker for `isExternal` songs — `cm/ui/screens/collection/components/SongRow.kt`, alpha41 (#375), alpha43
- Swipe a song row right = add to queue, left = play next — `SongRow.kt` (commit 2026-04-06, alpha41 #390)
- Song long-press/more sheet: rate, share, star, download/cancel/delete/retry, play next, add to queue, add to (another) playlist, remove from playlist, view album, view artist, sleep timer, playback speed, track info — `cm/ui/components/sheets/SongSheet.kt`
- Album/playlist sheet: rate, view on last.fm, view on MusicBrainz, share, play next, add to queue, add all to playlist, view artist, star, download, delete (playlist) — `cm/ui/components/sheets/CollectionSheet.kt`
- Artist sheet: last.fm / MusicBrainz links, play next, add to queue, add to playlist, star, download all — `cm/ui/components/sheets/ArtistSheet.kt`
- Artist detail: image, biography (expandable "More"), Play artist, download all albums (with storage warning), top songs ("Frequently played" + See all), albums, similar artists — `cm/ui/screens/artist/ArtistDetailScreen.kt`, `getArtistInfo`, commits 2026-01-25, 2026-03-18, alpha35 (#263)
- Multiple artists per song, each clickable — alpha43, `DomainSongArtist.kt`
- Genres list (cards) and genre detail (songs + albums sections with See all) — `cm/ui/screens/genre/*`, commit 2026-07-03
- Starred screen: starred songs, albums, artists in one page — `cm/ui/screens/starred/*`, alpha40 (#322)
- Track info screen/sheet: name, artists, album, year, genre, duration, track/disc number, format, bitrate, bit depth, sampling rate, channel count, file size, path, track/album ReplayGain and effective value — `cm/ui/screens/song/viewmodels/SongDetailViewModel.kt`, `strings` `info_track_*`
- Fullscreen cover viewer from album/playlist header: shared-element transition, swipe vertically to dismiss, Share image, Save image — `cm/ui/screens/imageView/*`, HEAD commit #551 (originally commit 2026-01-23)
- Animated cover art (GIF/animated WebP through coil-gif) — alpha30 (#216), `composeApp/build.gradle.kts`
- Confirmation dialog before opening any external link, opened in an isolated browser tab — `cm/ui/components/dialogs/LinkConfirmationDialog.kt`, commit 2026-08-10
- Tablet/iPad adaptive list-detail layout (navigation3 `ListDetailSceneStrategy`), settings back button hidden on medium+ widths — `cm/App.kt`
- NOT present: folder/directory browsing, music-folder (library) selection, year/decade browser UI, composer/contributor browsing, album versions grouping.

## 3. Home ("Library" tab)

- Four shortcut buttons: Recently added, Random, Starred, Frequently played — `cm/ui/screens/library/components/Content.kt`
- Horizontal carousels with "See all": Recently played albums, Playlists, Artists, Genres — same file
- Long-press sheets work from the carousels (star, rate, share, queue, delete playlist) — `cm/ui/screens/library/LibraryScreen.kt`
- Top bar: Search (when the Search tab is hidden), Settings, Account — `cm/ui/components/layouts/RootTopBar.kt`
- Pull to refresh refreshes all four sections — `LibraryScreen.kt`
- Errors shown as an expandable error snackbar with stack trace — `cm/ui/components/snackbars/ErrorSnackBar.kt`
- NOT present: configurable/reorderable home sections, mixes, "continue listening", stats.

## 4. Search

- `search3` online, merged with local playlist matches; results inserted into the local DB — `cm/domain/repositories/SearchRepository.kt`
- Automatic fallback to local DB search when offline or when the request fails — same
- 300 ms debounce — `cm/ui/screens/search/viewmodels/SearchViewModel.kt`
- Filter chips: All / Songs / Albums / Artists — `cm/ui/screens/search/SearchScreen.kt`
- Recent searches (last 10), tap to reuse, remove individually — alpha24 (#157)
- Song results: tap to play, swipe to add to queue, play next, add to queue, track info; duplicate-in-queue confirmation — `SearchScreen.kt`
- "No results" indicator — alpha41 (#369)
- Search can be a bottom tab or a top-bar button — commit 2026-03-04

## 5. Playback engine

- Android: Media3 ExoPlayer 1.11 in a `MediaSessionService` foreground service; iOS: `AVPlayer` with `MPRemoteCommandCenter` + `MPNowPlayingInfoCenter` — `am/shared/MediaPlayer.android.kt`, `im/shared/MediaPlayer.ios.kt`
- System integration: media notification, lock screen, quick settings media player, Bluetooth/headset buttons (`MediaButtonReceiver`) — README, `aa/AndroidManifest.xml`
- Custom notification/session buttons: Shuffle and Repeat (off/all/one) — `PlaybackService.makeButtons`, commit 2026-07-03
- Buffering: 32-64 s forward buffer, 10 s back buffer, 2.5 s to start, 5 s after rebuffer; `WAKE_MODE_NETWORK`; handles audio-becoming-noisy (unplug pauses); audio focus handled — `PlaybackService.onCreate`
- Restricted extractor set (FLAC, WAV, MP4/fMP4, Ogg, Matroska, ADTS, MP3 with constant-bitrate seeking) — same, alpha54 (#531)
- `estimateContentLength=true` on stream URLs so transcoded streams are seekable — `getStreamUrl`
- Shuffle, repeat off/all/one, shuffle-play a collection, previous restarts the song when >1 s in — `MediaPlayer.android.kt` (commit 2026-02-27)
- Playback speed 0.5x-2.0x slider plus presets 1.0/1.25/1.5/1.75/2.0 — `cm/ui/screens/nowPlaying/PlaybackScreen.kt`, alpha38 (#282)
- Sleep timer: by time (5/10/15/30/45 min, 1 h), after N songs (1/2/3/5/10), or at end of queue; countdown shown in account sheet and song sheet — `cm/domain/manager/SleepTimerManager.kt`, `cm/ui/components/sheets/SleepTimerSheet.kt`, alpha30/34/50 (#475)
- Player state persisted (queue, index, position, shuffle, repeat, speed) and restored paused on next launch — `cm/domain/repositories/PlayerStateRepository.kt`, `cm/shared/MediaPlayer.kt` (commit 2026-03-04 #136)
- Plays the downloaded file when one exists, otherwise streams — `DomainSong.toMediaItem()`
- Explicit content playback: Allowed / Skip explicit songs / Skip for this session; queue auto-skips flagged songs — `cm/domain/models/settings/ExplicitContentPlayback.kt`, `skipUnavailableSong()`, commit 2026-07-02
- Stopping: service stops when the task is swiped away — `onTaskRemoved`
- Artwork passed to the session as bytes from the Coil disk cache (falls back to URL) — alpha41 (#347)
- Notification shows track artist rather than album artist — alpha52 (#534)
- NOT present: crossfade, skip silence, Chromecast/UPnP/jukebox, bookmarks/resume for long tracks, server play-queue sync (`savePlayQueue`).

## 6. Audio (EQ, ReplayGain, offload, gapless)

- Audio effects screen is Android-only; iOS gain manager is a no-op stub — `cm/ui/screens/settings/PlaybackScreen.kt`, `im/domain/manager/AudioGainManager.ios.kt`
- ReplayGain modes: Off / Track / Album / Dynamic (album gain if the whole queue is one album, else track gain) — `cm/domain/models/settings/ReplayGainMode.kt`, `applyAudioGain()`, alpha26, alpha39 (#305), alpha50 (#480)
- Gain fallback chain: track -> album -> fallbackGain -> baseGain -> 0 (mirrored for album mode), values from OpenSubsonic `replayGain` — `cm/util/ReplayGainUtils.kt`
- Preamp: two sliders -12..+12 dB in 0.1 dB steps, one "with ReplayGain tags", one "without ReplayGain tags" — `cm/ui/screens/settings/AudioEffectsScreen.kt`, alpha52 (#519)
- Implemented as a custom ExoPlayer `AudioProcessor` on 16-bit PCM with hard clipping (no peak-based limiter), flushing only when gain changes — `am/exoplayer/AudioGainProcessor.kt`
- Equaliser source: Disabled / Built-in / External — `cm/domain/models/settings/EqualiserMode.kt`, alpha49 (#476)
- Built-in EQ: Android platform `Equalizer` effect, device-provided band count and level range, vertical sliders, reset button; "device does not support" message — `cm/ui/screens/settings/EqualiserScreen.kt`, `PlaybackService.makeEqualiser`, alpha43
- External EQ: broadcasts `ACTION_OPEN/CLOSE_AUDIO_EFFECT_CONTROL_SESSION` so Wavelet/Poweramp EQ etc can attach — `openAudioEffectSession`, alpha49
- EQ disabled while audio offload is on — `AudioEffectsScreen.kt`
- Gapless playback toggle (default on, "experimental", restart required) via offload preference `setIsGaplessSupportRequired` — `PlaybackService`, alpha26
- Audio offload toggle (default off, experimental, restart required) — alpha26
- NOT present: EQ presets, AutoEq import, parametric EQ, bass boost/virtualiser, crossfeed, bit-perfect/USB DAC output.

## 7. Queue

- Queue screen: tap to jump, drag handle reorder, swipe to remove, Clear queue — `cm/ui/screens/queue/QueueScreen.kt`, alpha24 (#157)
- Queue info header: song count + total duration, tap to toggle to songs/time remaining (persisted) — `QueueInfoType.kt`, alpha41 (#350), commit 2026-09-15 (#566)
- Play next / Add to queue for songs, albums, playlists, artists (all albums) — sheets, alpha34 (#255), alpha35 (#259)
- Duplicate-in-queue confirmation with "Don't show again" (resettable in Developer options) — `cm/ui/components/dialogs/QueueDuplicateDialog.kt`, commit 2026-09-10
- Auto-fill queue: appends one random library song when only one song is left — `checkAndAutoFillQueue()`, alpha43 (#466)
- Albums queued in disc/track order — `addToQueue`, alpha36 (#266)
- Queue items show explicit and offline-unavailable markers — `cm/ui/screens/queue/components/Item.kt`
- Snackbars for "Added to queue" / "Playing next" — alpha41 (#373)

## 8. Playlists

- Create playlist (name) — `cm/ui/screens/playlist/dialogs/PlaylistCreateDialog.kt`
- Delete playlist with confirmation; deletion is queued when offline — `cm/ui/components/dialogs/DeletionDialog.kt`
- Add song / all songs of album / artist to a playlist; multi-select several playlists at once; "New" shortcut and refresh in the dialog — `PlaylistUpdateDialog.kt`, alpha30
- Remove a song from a playlist — `CollectionDetailViewModel.removeFromPlaylist`
- Playlist sort + Downloaded filter + grid/list — see Library
- Playlists included in search (local match), shares, downloads, play next / queue — various
- NOT present: rename, edit comment/public flag, reorder tracks inside a playlist, smart playlists, import/export.

## 9. Offline / downloads / sync

- Offline mode setting: Auto (follow connectivity), Forced, No Wi-Fi (treat cellular/metered as offline) — `cm/domain/models/settings/OfflineMode.kt`, `am/domain/manager/ConnectivityManager.android.kt`, alpha34 (#256)
- Online = network has INTERNET + VALIDATED; "cellular" = cellular transport OR metered — same
- Periodic sync: every 15 min pending actions are flushed; full library pull when the last one is older than 1 h; immediate full sync on first run — `cm/domain/manager/SyncManager.kt`
- Full sync pulls genres, radios, artists (via paged `search3`), playlists + songs, all albums alphabetically then `getAlbum` for each — `DbRepository.kt`
- Offline write queue (persisted `SyncActionEntity`): star, unstar, set rating 0-5, scrobble, delete playlist; replayed in order when back online, stops at first failure — `SyncManager.processQueue`, alpha32 (#221)
- Download song / album / playlist / all albums of an artist / entire library — `cm/domain/manager/DownloadManager.kt`, `cm/ui/screens/settings/DataStorageScreen.kt`
- 10 parallel downloads, per-song progress, cancel, delete, failed state with "click to retry" — same
- Each download also caches cover art (song + album), and lyrics — `executeDownloadProcess`
- Entire-library download with progress bar and cancel, skips already downloaded — `downloadEntireLibrary`
- Downloads stored in app-private `filesDir/downloads` (not user-visible, no SD card choice) — `am/domain/manager/StorageManager.android.kt`
- Download state button on album/playlist/artist headers and carousels — `HeadingRowButtons.kt`, `ArtistActionButtons.kt`
- Songs not downloaded are greyed/marked "Not available offline" when offline — `SongRow.kt`
- Snackbars for download started / deleted — alpha41 (#373)
- NOT present: auto-download of starred/playlists, download only on Wi-Fi rule, storage location picker, cache size limit for audio, streaming cache (pre-cache next tracks beyond ExoPlayer buffer).

## 10. Transcoding / streaming quality

- Separate Streaming quality and Download quality screens, each with independent Wi-Fi and Cellular choice, "(in use)" marker on the active network — `SettingsStreamingQualityScreen.kt`, `SettingsDownloadQualityScreen.kt`, alpha36 (#268), alpha42 (#437)
- Presets: Low (Android 80 kbps Opus / iOS 96 kbps AAC), Medium (128 Opus / 160 AAC), High (192 Opus / 256 AAC), Lossless (no transcoding) — `cm/domain/models/settings/StreamingQuality.kt`
- Advanced transcoding: custom max bitrate and custom format string per network; empty values leave the decision to the server — same screens, alpha39 (#311), commit 2026-07-03
- Changing quality applies without restart — alpha41 (#372)
- Technical info row in Now Playing shows actual format/bitrate/sample rate and requested bitrate — `cm/ui/screens/nowPlaying/components/rows/TechnicalInfoRow.kt`, alpha36 (#267)
- Cover art quality: Low 512 / Medium 1024 / High 4096 px — `CoverArtQuality.kt`, commit 2026-05-22

## 11. Lyrics

- Providers: Subsonic/OpenSubsonic `getLyricsBySongId` (default on), LRCLIB (off), LyricsPlus/"Youly+" (off); each toggleable and drag-reorderable for priority; disclaimer about third-party requests — `cm/domain/models/lyrics/LyricsConfig.kt`, `cm/ui/screens/settings/dialogs/LyricProvidersSheet.kt`, alpha43, commit 2026-08-10
- LyricsPlus queried on 6 mirrors in parallel, first success wins — `cm/domain/repositories/LyricsRepository.kt`
- LRCLIB loose `q=` search with parentheticals stripped, prefers synced over plain — same, alpha42 (#441)
- Beat-by-beat (word/syllable timed, karaoke fill) lyrics from LyricsPlus — `cm/ui/screens/lyrics/components/KaraokeText.kt`, alpha22 (#151)
- Synced LRC, unsynced/plain lyrics, RTL support — `cm/domain/parser/LyricsContentParser.kt`, alpha36 (#267, #272), alpha50 (#491)
- Lyrics cached in DB on fetch and with downloads (offline lyrics) — `LyricDao`, alpha38 (#286)
- Tap a line to seek (and resume) — `cm/ui/screens/lyrics/components/Content.kt`
- Settings: auto-scroll, beat-by-beat on/off, keep screen on, blur the edges (inactive lines), brighter inactive lines — `cm/ui/screens/settings/NowPlayingScreen.kt`
- "Lyrics provided by X" footer; refresh/retry button; loading notice — `Content.kt`, commits 2026-02-23, 2026-07-03
- Share lyrics: select adjacent lines (max 150 chars, playback pauses while selecting), renders a 4:5 card with cover, title, artist, Navic branding, themed colours; shared as PNG — `cm/ui/screens/lyrics/dialogs/LyricsShareSheet.kt`, commits 2026-02-14, 2026-04-30
- Lyrics opened from Now Playing toolbar or by tapping the cover (configurable) — `CoverArtTapAction.kt`
- NOT present: lyrics offset adjustment, translation/romanisation, manual search/edit, embedded-tag lyrics beyond what the server returns.

## 12. Radio

- Internet radio stations synced from the server, shown as cards with homepage — `cm/ui/screens/radio/*`, alpha34 (#228)
- Play a station as a live stream (single-item queue, "Live Radio" metadata) — `playRadio`
- Add a station (name, stream URL, homepage URL) via `createInternetRadioStation` — `RadioCreateDialog.kt`
- Optional Radios bottom tab — `NavbarConfig.kt`
- NOT present: edit/delete station, ICY now-playing metadata, artist/track "instant mix"/similar-songs radio.

## 13. Shares

- Create a share for album/playlist/song with optional expiry (duration picker), link copied to clipboard — `cm/ui/screens/share/dialogs/ShareDialog.kt`, `cm/ui/components/common/DurationPicker.kt`
- Shares list: cover, "Shared by", expires in / never / expired; share link via system sheet, delete — `cm/ui/screens/share/*`, commit 2026-02-20
- Setting to hide all sharing UI — `enableSharing`, commit 2026-09-12
- NOT present: edit share description/expiry, handling of incoming share links.

## 14. Android Auto / widgets / platforms

- Android Auto: declared as a media app (`automotive_app_desc`, `MediaBrowserService` intent filter, tintable attribution icon). The service is a plain `MediaSessionService`, so it offers playback controls + shuffle/repeat buttons but no browsable library tree; README calls it "Basic Android Auto support", PR title "(Kinda) Android Auto support" — `aa/AndroidManifest.xml`, `aa/res/xml/automotive_app_desc.xml`, alpha42 (#447)
- Glance home-screen widgets: "Mini Player" (4x1, cover, title, artist, prev/play-pause/next, horizontally resizable) and "Turn Table" (2x2 round cover with play/pause); updated by a `NOW_PLAYING_UPDATED` broadcast; tap opens the app; previews; system corner radius — `aa/kotlin/.../widgets/*`, commits 2026-02-11, alpha39 (#309)
- iOS app: AVPlayer, Control Centre/lock screen controls with seek, scrobbling, downloads, AltStore source (`app-repo.json`), TestFlight CI — `im/`, alpha39/41
- Distribution: GitHub releases (APK + IPA), F-Droid, IzzyOnDroid, Obtainium, AltSource, Codeberg mirror — README
- In-app update checker (GitHub latest release) with release-notes sheet rendered from markdown, "Don't show again"; hidden in F-Droid builds — `cm/ui/components/sheets/ChangelogSheet.kt`, `AboutScreen.kt` (`BuildInfo.FDROID`), alpha54 (#547)
- Per-app language support, 28 translation locales via Weblate, RTL — `aa/res/xml/locales_config.xml`, composeResources
- NOT present: Wear OS, Android TV, desktop, Chromecast, Google Assistant voice search (`onSearch`/play-from-search).

## 15. UI customisation

- Themes: Dynamic (Material You, Android), Tinted (seed hue picker), Cupertino, Music.app (pink), Spotlight (green) — `cm/domain/models/settings/Theme.kt`
- Theme mode System / Dark / Light — `ThemeMode.kt` (commit 2026-03-10)
- Tinted options: palette accent hue picker, palette style (all MaterialKolor styles: TonalSpot, Vibrant, Expressive, Monochrome, ...), palette specification (2021/2025) — `cm/ui/screens/settings/ThemesScreen.kt`
- Dynamic theming of album and artist screens from cover art colours — `dynamicTheming`, commit 2026-07-03
- App icon variants: Default, Inverted (Android activity-alias, restarts app) — `AppIconVariant.kt`, alpha42 (#430)
- Application font: System or Google Sans (external fonts stubbed out) — `cm/ui/screens/settings/FontsScreen.kt`
- Artwork shape for albums/songs and separately for artist images: Square, Soft, Curved, Circle — `CoverArtShape.kt`, commit 2026-06-30
- Grid size 2x2/3x3/4x4 (phones) or cover art size slider 50-500 (larger screens) — `GridSize.kt`, `AppearanceScreen.kt`
- Text auto-scroll (marquee) speed: Disabled/Slow/Medium/Fast — `MarqueeSpeed.kt`
- Animation style Expressive / Standard (M3 Expressive motion scheme); predictive back animations toggle; shared element transitions — `AnimationStyle.kt`, commit 2026-09-15
- Now Playing: background Static / Dynamic (animated blended cover colours), slider style Flat / Squiggly / Slim / Yoyo, toolbar position Top / Bottom, show technical song info, cover tap action (Disabled / Show lyrics), swipe cover to change songs, landscape layout, star button, haptic play button, clickable title (album) and artists, sheet uses device screen corner radius — `cm/ui/screens/settings/NowPlayingScreen.kt`, `cm/ui/screens/nowPlaying/*`, alpha21 (#148)
- Bottom bar: collapse mode Never / On scroll; visibility Default / All screens; navigation bar style Normal / Short; label visibility Always / Only selected / Never; configure and re-order tabs — `cm/ui/screens/settings/BottomBarScreen.kt`, `NavtabsDialog.kt`
- Mini player: Unified / Detached style; progress bar Hidden / Visible / Seekable (drag with haptics); swipe left/right to skip; hide if idle — `cm/ui/components/layouts/MiniPlayer.kt`, alpha54 (#543)
- Toggle visibility of ratings and of sharing — commits 2026-09-12
- Accessibility pass (content descriptions, tooltips, swipe a11y labels) — commit 2026-07-02

## 16. Scrobbling / ratings / favourites

- Scrobbling to the Navidrome server (toggle), "now playing" notification + submission — `cm/domain/manager/ScrobbleManager.kt`
- Scrobble percentage slider 0-100% (default 50%) measured on accumulated real play time, and minimum duration to scrobble (default 30 s) — `PlaybackScreen.kt`, commit 2026-01-24. Note: the min-duration slider is declared with `valueRange = 0f..1f` while being displayed as seconds, which looks like a bug.
- Repeat/seek-to-start re-arms the scrobble; resumes only count after resume — `am/domain/manager/ScrobbleManager.android.kt`, alpha28 (#191), alpha41 (#395), alpha55 (#553)
- Offline scrobbles queued and submitted when online — alpha32 (#221)
- Star/unstar songs, albums, artists (optimistic local write + queued sync) — repositories
- 1-5 star ratings for songs and albums from sheets, Now Playing and collection screen; rating sort — `cm/ui/components/common/RatingRow.kt`, alpha35 (#264)
- NOT present: direct Last.fm/ListenBrainz scrobbling (relies on the server), rating artists, local play history screen.

## 17. Data & storage (settings page)

- Network: Download quality, Offline mode, Cover art quality — `cm/ui/screens/settings/DataStorageScreen.kt`
- Synchronisation control: live status with progress/message, Trigger manual sync ("forces a full library pull"), Last full sync time — same
- Cache & storage: pending sync actions count, downloaded songs count + size, image cache size, Download entire library — same
- Danger zone: Clear image cache, Clear pending sync actions, Clear downloads, Rebuild database (wipe metadata + fresh sync) — same
- Image cache: Coil, 2 GiB disk cache in temp dir keyed by cover id, 15% memory cache, crossfade — `cm/di/SingletonImageLoaderInit.kt`
- Android auto-backup enabled — manifest
- NOT present: settings import/export/backup file.

## 18. Developer options

- Custom server headers editor — `cm/ui/screens/settings/DeveloperScreen.kt`
- Reset "don't show again" dialogs — same
- In-app log viewer (Android logcat): coloured levels, clear, tap line to copy, scroll-to-bottom button, autoscroll only at bottom — `cm/ui/screens/settings/LogsScreen.kt`, commits 2026-06-07, 2026-09-04
- Test exception handler (deliberate crash) — same
- Crash handler activity in a separate process showing the stack trace — `aa/kotlin/.../CrashActivity.kt`, commit 2026-04-09
- Build info (F-Droid flag etc) — alpha54 (#547)

## 19. Misc

- About screen: platform + version (tap to copy), GitHub, Codeberg, Discord links, check-for-updates toggle — `cm/ui/screens/settings/AboutScreen.kt`
- Zero telemetry/analytics, minimal permissions (INTERNET, NETWORK_STATE, LOCAL_NETWORK, MODIFY_AUDIO_SETTINGS, foreground service, widgets) — README, manifest
- Haptic feedback on play button and mini-player seek — `ButtonsRow.kt`, `MiniPlayer.kt`
- Monogram placeholder avatar, "content unavailable" empty states for every list — `cm/ui/components/common/*`
- Bottom bar force-hidden in landscape on non-root screens — alpha43
- Stack: Compose Multiplatform (Material 3 Expressive), navigation3, Room 3, Koin, Ktor, Coil 3, Media3 1.11, Glance, subsonic-kotlin 1.0.0-beta07 — `gradle/libs.versions.toml`

## Subsonic API endpoints actually used

`ping`, `getAlbumList2`, `getAlbum`, `getAlbumInfo2`, `getArtistInfo2`, `search3` (search + artist enumeration), `getGenres`, `getPlaylists`, `getPlaylist`, `createPlaylist`, `updatePlaylist`, `deletePlaylist`, `star`, `unstar`, `setRating`, `scrobble`, `getLyricsBySongId`, `getInternetRadioStations`, `createInternetRadioStation`, `getShares`, `createShare`, `deleteShare`, `stream`, `getCoverArt` — grep of `api.*` over the source.
