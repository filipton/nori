# Musly feature inventory (dddevid/musly, tag v2.0.2, pubspec 2.0.2+7)

Internal planning note, not published.

Sources read: README.md, CHANGELOG.md (all entries 1.0.1 → 2.0.2), GitHub release notes v1.0.0–v2.0.2 (`gh release view`), fastlane/metadata/android/en-US (full_description + changelogs 5, 7), pubspec.yaml, lib/l10n/app_en.arb (892 keys), lib/services/*, lib/providers/*, lib/screens/*, lib/widgets/*, android MainActivity.kt + AndroidManifest, ios/Runner/*, react-website/src/components/Features.jsx.

Source tags: `CL x.y.z` = CHANGELOG entry, `REL x.y.z` = GitHub release notes, `arb:key` = lib/l10n/app_en.arb, paths relative to repo root (`lib/` omitted where obvious).

**Status legend**: no marker = verified present in v2.0.2 source. **[GONE]** = advertised in changelog/website/l10n but NOT in v2.0.2 source (2.0 was a rewrite that dropped it). **[DISABLED]** = code present but unreachable/commented-out in UI. **[STUB]** = UI/plumbing exists, implementation is fake or non-functional. **[CLAIM]** = only README/store text, no code found.

---

## 1. Servers / sources

- Subsonic API client (Navidrome, Subsonic, Airsonic, Gonic advertised); API via `/rest/*?f=json` — services/subsonic_service.dart, README
- Token+salt auth (md5(password+salt)); "Legacy Authentication" toggle sends plaintext `p=` for old servers — subsonic_service.dart:290, arb:legacyAuthentication
- Stable per-install salt for cover-art/stream URLs so URLs stay cacheable — subsonic_service.dart:316 (`_ensureStableAuthParams`), CL 2.0.0 (#199)
- Jellyfin / Emby native client (own REST service, auth by username/password → token, userId) — services/jellyfin_service.dart, models/server_config.dart (apiToken,userId)
- Jellyfin/Emby music-library selection with ParentId filtering (keeps audiobooks out) — jellyfin_service.dart `getMusicLibraries`, CL 2.0.0 (#188)
- "Web Stream" source = YouTube Music without login: yt-dlp (Android: embedded CPython via Chaquopy + `ytdlp_helper.py`; desktop: host `yt-dlp`/python subprocess; fallback `youtube_explode_dart`) — services/ytdlp_service.dart, services/youtube_service.dart, android/app/build.gradle.kts (chaquopy), MainActivity.kt channel `com.devid.musly/ytdlp`
- Web Stream: search songs/albums/artists, playlists, radio tracks ("similar"), hi-res thumbnails, proxying StreamAudioSource with matching headers + on-disk part/cached files — youtube_service.dart:17-200, CL 2.0.0
- Web Stream hidden entirely on iOS (App Store compliance) — add_server_screen.dart:51,334; CL 2.0.0
- Local files as first-class source ("Local Files" provider tile; no server needed) — screens/auth/login_screen.dart:595, add_server_screen.dart:351, services/local_music_service.dart
- Local scan: recursive scan of default dirs (Android Music/Download(s); `~/Music`,`~/Downloads` desktop; app Music dir on iOS) + user-added custom folders + excluded folders; formats mp3/flac/m4a/ogg/opus/wav/aac/wma; tag reading via `audio_metadata_reader`; embedded art cache; folder cover fallback; cached library; pick individual files — local_music_service.dart:35-400, CL 1.0.12
- "Merge with Server Library" toggle: show local music alongside server library — arb:mergeLocalLibrary, settings_storage_tab.dart:275, providers/library_provider.dart:93
- Multi-server profiles: save many Subsonic/Jellyfin/WebStream profiles, switch, rename, remove; "ACTIVE" badge; server switcher bottom sheet — widgets/modals/server_switcher_sheet.dart, services/storage_service.dart:155-310, settings_server_tab.dart, CL 2.0.0 (#185,#196)
- Credentials in `flutter_secure_storage` (hardware-backed) — storage_service.dart:35-75, pubspec, fastlane description
- Dual LAN/WAN URL per profile: probes LAN `/rest/ping` with 3 s timeout at configure, falls back to WAN — subsonic_service.dart:113-135, CL 2.0.0 (#187)
- Self-signed certificate allow toggle — arb:allowSelfSignedCerts, subsonic_service.dart:161
- Custom CA certificate file (.crt/.pem/.cer) — arb:customTlsCertificate, certificateFileSubtitle
- mTLS client certificate (.p12/.pfx + optional password) — arb:clientCertificate, models/server_config.dart:12-13, CL 1.0.5
- Music-folder selection dialog (Subsonic `getMusicFolders`; note: only first selected id is sent as `musicFolderId`) — settings_server_tab.dart:603, subsonic_service.dart:349
- Server type/version detection & display (Navidrome/Subsonic/Airsonic…); auto-detects Octo-Fiesta proxy on Subsonic and Jellyfin — subsonic_service.dart:439, jellyfin_service.dart:135, CL 2.0.0 (#216)
- Connection status card (CONNECTED / CONNECTING / OFFLINE) — settings_server_tab.dart:169
- Ping retry (3×, 2 s backoff, 10 s timeout), "Cannot reach server" screen with Retry and "Open in offline mode" — providers/auth_provider.dart, arb:serverUnreachableTitle, CL 1.0.8
- Categorized login error cards with troubleshooting hints + "Copy error" — arb:copyError, CL 2.0.0
- URL validation (must start with http/https) — arb:invalidUrlFormat
- Logout (clears cached data, cancels downloads, deletes downloads) — auth_provider.dart:377-394
- Internet radio "via Radio Browser API" **[CLAIM]** — README only; no radio-browser code in lib (radio = Subsonic `getInternetRadioStations` only)
- Roadmap (not done): Custom API server, Tizen/WebOS ports, CarPlay — README

## 2. Library & browsing

- Bottom nav: Home / Library / Search (+ Settings from header); nested navigator keeps mini-player & nav bar persistent — screens/main/main_screen.dart, utils/navigation_helper.dart, REL 1.0.3
- Android back handling: pop nested → go Home tab → exit — REL 1.0.3
- Route de-duplication (won't open identical screen twice) — CL 2.0.0
- "Your Library" screen: filter chips Playlists / Albums / Artists / Downloaded / Songs; sort Recents / Recently added / Alphabetical / Creator; list ↔ grid toggle; pinned tiles (Liked Songs, Downloaded Songs, Radio Stations, Liked Albums); "+" → create playlist or add music source — screens/main/library_screen.dart
- Virtualized library lists/grids (SliverList/SliverGrid.builder, RepaintBoundary, cacheExtent 600) — CL 2.0.2
- Library search delegate across playlists, albums, artists — screens/main/library_search_delegate.dart, CL 1.0.1 (#25)
- All Songs screen with sort (Title A-Z/Z-A, Artist, Album, Duration longest, Recently added) + Play All / Shuffle — screens/media/song_collection_screen.dart
- "Get all songs" strategy: Subsonic has no endpoint → iterated; Jellyfin single call — subsonic_service.dart:624, jellyfin_service.dart:360
- Album collection screens: All Albums, New Releases (`newest`), Top Rated (`highest`), Liked Albums; album list types used: recent, frequent, newest, random, highest, alphabeticalByName, byGenre — screens/media/album_collection_screen.dart, subsonic_service.dart:653
- A–Z alphabet fast-scroll sidebar with haptics on album grids — album_collection_screen.dart `_buildAlphabetSidebar`, CL 2.0.0 (#204)
- Album screen: collapsing pinned header art, play/shuffle pills, like album, download album, "Search in album"/filter tracks, add all to queue, total duration, tappable artist — screens/detail/album_screen.dart, CL 2.0.1
- Artist screen: top songs, sections Albums / EPs / Singles, Play (top songs + rest), Shuffle, Add artist to queue, Download all albums, biography (`getArtistInfo`), multi-layer fallback lookup when artist id missing — screens/detail/artist_screen.dart, subsonic_service.dart:635, CL 2.0.0 (#186,#221,#225), CL 1.0.12
- Multi-artist support (Navidrome `artists[]`; "/"-separated legacy) with artist picker sheet — models/artist_ref.dart, widgets/common/multi_artist_widget.dart, CL 1.0.9
- Genres screen (song/album counts tooltip) and Genre screen with Songs + Albums tabs — screens/media/genres_screen.dart, screens/detail/genre_screen.dart, CL 1.0.6
- Favorites screen: Songs + Albums tabs, Play all / Shuffle / Download all, remove-favorite confirm — screens/media/favorites_screen.dart
- Downloads screen: Songs + Albums tabs — screens/media/downloads_screen.dart, CL 2.0.0 (#226)
- Listening History screen (locally tracked songs) — song_collection_screen.dart (`HistoryScreen`), home_screen.dart:127, REL 1.0.2
- Internet Radio screen (server stations): play, Play All, LIVE badge, open homepage, copy stream URL, refresh — screens/media/radio_screen.dart, REL 1.0.3
- Radio station create/update/delete API methods exist but have NO UI **[STUB]** — subsonic_service.dart:1238-1267 (no callers)
- Downloaded / Dolby Atmos badges on song tiles (`hasDolbyAtmos` field from server JSON) — widgets/common/dolby_atmos_badge.dart, models/song.dart:29, CL 2.0.0 (#188,#224)
- Now-playing animated equalizer indicator on the active row — widgets/common/animated_equalizer.dart, song_tile.dart
- Shimmer skeleton loaders; no-artwork placeholder; artwork keeps aspect ratio (non-square art gets rounded corners + shadow) — widgets/common/shimmer_loading.dart, album_artwork.dart, CL 1.0.6/1.0.7/2.0.0 (#206)
- Library cache: SQLite (`sqflite`) library DB replacing JSON; 6-hour background refresh cooldown; manual Refresh forces full re-sync; 5 s server init timeout then local mode — services/library_database_service.dart, library_provider.dart:361-374, CL 1.0.13/1.0.12/1.0.4
- NOT present: folder/directory browsing (`getIndexes`/`getMusicDirectory`), browse by year/decade (arb key `years` unused), podcasts, bookmarks, shares, play-queue sync (`savePlayQueue`) — grep of lib/

## 3. Home / recommendations

- Time-of-day greeting; History + Settings header buttons; category chips Music / Made For You / Playlists — screens/main/home_screen.dart:38-160
- Spotify-style top quick-access grid (incl. Liked Songs) — home_screen.dart `_buildSpotifyTopGrid`, widgets/cards/quick_access_tile.dart
- Sections: Jump back in (recent albums), Made For You, Listen Again, Your Top Hits, Your Playlists, Artists You Love — home_screen.dart:270-379
- On-device taste engine: per-song profiles (plays, skips, completion rate, ratings, stars, hour-of-day preference), exponential recency decay, skip-rate "disliked" flag (>60% & ≥3 skips), artist/genre affinity — services/recommendation_service.dart:97-345,774-830
- Generated mixes: personalized feed, Quick Picks, Discover Mix (unheard songs), Listen Again, Top Hits, Artist Mixes, Genre Mixes — recommendation_service.dart:345-505
- "Enable Recommendations" toggle; listening data counter ("N total plays"); "Clear Listening History" — arb:enableRecommendations, clearListeningHistory, settings_display_tab.dart:587-600
- History always recorded even if recommendations disabled — CL 1.0.11 (#146)
- Made For You screen with "Shuffle New Selection" — song_collection_screen.dart, arb:shuffleNewSelection
- Web Stream mixes built from web listening habits, cached in SQLite — CL 2.0.0
- Empty-state with refresh when no content — CL 1.0.1 (#22)
- Seasonal Musly Wrapped hero banner on Home (see §15) — home_screen.dart:634
- Desktop home: denser layout, compact table rows — CL 1.0.8
- 50-songs milestone celebration dialog (pauses playback, deferred if backgrounded) — widgets/dialogs/milestone_celebration_dialog.dart, player_provider.dart `_check50SongsMilestone`

## 4. Search

- Search across Songs / Artists / Albums / Playlists with filter chips and "Top result" card (song/artist/album variants) — screens/main/search_screen.dart, widgets/cards/top_result_card.dart
- Live Search setting: update results as you type (250 ms debounce) vs. suggestion dropdown — arb:liveSearch, search_screen.dart:74
- Recent searches list with Clear — arb:recentSearches
- "Browse all" category cards: Made For You, New Releases, Top Rated, Radio Stations, Genres & Moods, Liked Songs + quick genre cards (Pop & Hits, Hip-Hop & Rap, Rock & Alt, Chill & Relax) — search_screen.dart `_buildBrowseAllSection`
- Playing a song from search starts a "radio queue" of similar songs instead of the search result list — player_provider.dart:1718 `playSongWithRadio`, CL 2.0.2
- Search in album / search in playlist track filters — arb:searchInAlbum, searchInPlaylist
- Android Auto voice/keyboard search + play-from-search — player_provider.dart:571-744
- Easter egg: tapping the Search tab 11× opens bouncing "Fantasy" screen — main_screen.dart:677-690, screens/media/fantasy_screen.dart

## 5. Playback engine

- `just_audio` core; `just_audio_windows` on Windows; `just_audio_media_kit` + libmpv on Linux/macOS; `audio_service` background handler on Android/iOS — pubspec.yaml, services/audio_handler.dart
- Gapless playback via `ConcatenatingAudioSource` (toggle, persisted) — arb:gaplessPlayback, player_provider.dart:3087, CL 1.0.13
- Next-track preloading at ≤25 s remaining or ≥75% played: pre-resolves stream URL (yt-dlp/Subsonic), pre-fetches LRCLIB lyrics, pre-caches 800 px cover — player_provider.dart:1083-1185
- Shuffle (persisted, incl. shuffled order) and Repeat off/all/one (persisted) — player_provider.dart:2734-2819, CL 1.0.12
- Shuffle history: Previous walks actual playback history while shuffling — CL 1.0.9
- Playback speed 0.5×–2.0× slider (7 steps; provider clamps 0.25–4.0) — widgets/now_playing/now_playing_more_menu.dart:122-140, player_provider.dart:1207
- Pitch control / "Preserve pitch" toggle **[STUB in 2.0.2]**: Dart calls channel `com.devid.musly/pitch`, but no native handler exists in MainActivity.kt → always falls back to plain `setSpeed` — audio_handler.dart:588, android MainActivity.kt (only tv_mode + ytdlp channels), CL 1.0.11
- Volume: in-app slider (optional), persisted volume, mute/unmute restoring last non-zero level, hardware volume via `volume_controller` — widgets/now_playing/volume_slider.dart, player_provider.dart:2986-3017
- Resume after restart: queue, index, song id and position restored without auto-play — player_provider.dart:216-290, CL 1.0.12/1.0.13
- Audio focus (Android): explicit AudioSession config; fade-out + auto-pause when another app takes focus, fade-in + auto-resume on return; "another app has audio focus" snackbar — player_provider.dart:1496-1590, arb:audioFocusDenied, CL 2.0.0
- Android 16 / Media3 playback workaround — CL 1.0.5
- Windows position-polling fallback timer (500 ms) — player_provider.dart:1378
- Dolby Atmos stream detection badge — CL 2.0.0 (#188)
- Internet radio stream playback with dedicated radio player UI, "Streaming Live", Stop Radio — player_provider.dart:2133-2200, arb:streamingLive
- Seek forward/backward remote commands (iOS Control Center) — CL 1.0.8
- NOT present: skip silence, A-B repeat, bit-perfect/exclusive output, ReplayGain via DSP, visualizer — grep of lib/

## 6. Audio / DSP

- ReplayGain: Off / Track / Album; preamp −12…+12 dB (service clamps ±15); Prevent Clipping (uses peak); Fallback gain −12…0 dB (default −6) for untagged tracks. Implemented as player volume multiplier, clamped to ≤1.0 (so can only attenuate) — services/replay_gain_service.dart, settings_playback_tab.dart:196-280, player_provider.dart:3244
- "Smart Crossfade" 0–12 s slider **[partial]**: implementation only ramps the *outgoing* track's volume down over the last N seconds (no overlapping second player) — services/crossfade_service.dart, player_provider.dart:3268-3295, CL 2.0.0
- Fade In/Out on play/pause: toggle + duration 100–1000 ms — services/fade_settings_service.dart, settings_playback_tab.dart:536-600, arb:fadeInOutEnable
- Audio-focus duck/fade (see §5)
- BPM "analysis" **[STUB]**: BPM is estimated from genre string, cached in SharedPreferences; "Cache All BPMs"/"Clear BPM Cache" UI; feeds AutoDJ Smart Mix energy model — services/bpm_analyzer_service.dart:27-60, settings_storage_tab.dart:185, auto_dj_service.dart:482
- 10-band Equalizer with presets (Rock, Pop, Jazz, Classical, Bass/Treble Boost, Vocal, Electronic, Hip Hop) + custom presets **[GONE]** — CL 1.0.1 and website Features.jsx only; no equalizer code in lib/ at v2.0.2

## 7. Queue

- Queue view inside Now Playing: drag-and-drop reorder (`ReorderableListView`), remove, per-item ⋮ / long-press → song options — widgets/now_playing/queue_view.dart, CL 2.0.2 (#236)
- Play Next / Add to Queue / Add all to queue (album, playlist) / Add artist to queue — widgets/modals/song_options_modal.dart, player_provider.dart:2825-2866
- Spotify-style swipe-right on any song row to queue, with haptic + confirmation pill — widgets/common/swipeable_song_tile.dart, CL 2.0.0
- Persistent queue across restarts (200 ms debounced save; validates local paths) — CL 1.0.12 (#156)
- Clear queue — player_provider.dart:2898
- Auto DJ modes: Off / Shuffle Library / Similar Songs (`getSimilarSongs`) / Same Genre / Same Artist (`getTopSongs`) / Smart Mix (tempo+energy+genre+habits); "Songs to add" 1–20; trigger when ≤2 songs remain; de-dupes last 100 added — services/auto_dj_service.dart, settings_playback_tab.dart:79-175, REL 1.0.3
- Continuous auto-refill for radio/AutoDJ queues when ≤3 songs remain; fallback chain similar → genre random → artist top songs → artist search → random — player_provider.dart:1659-1785, CL 2.0.2
- Desktop right-sidebar queue panel (Now Playing card + Next Up, hover play/remove, closeable) — widgets/navigation/right_sidebar.dart
- "Playing Next" collapsible section — CL 1.0.10

## 8. Playlists

- List, open, create, delete server playlists (Subsonic + Jellyfin) — screens/media/playlists_screen.dart, subsonic_service.dart:713-850, jellyfin_service.dart:780-830
- Add to playlist from song menu / Now Playing "Add to" sheet; create new playlist with the song; duplicate warning ("Already in playlist → Add anyway") — widgets/now_playing/add_to_menu.dart, song_options_modal.dart:632-750, CL 2.0.0 (#211)
- Playlist screen: collapsing cover, Play/Shuffle, reorder mode (drag), multi-select + remove selected, select/deselect all, filter songs, add all to queue, download/remove downloads, total duration, favorite playlist — screens/detail/playlist_screen.dart, CL 1.0.12/1.0.4
- Local "Favorite playlists" pinning (SharedPreferences) with Home/sidebar section — services/favorite_playlists_service.dart, widgets/navigation/favorite_playlists_section.dart
- Composite 2×2 playlist cover generated & cached on disk — services/playlist_cover_service.dart, widgets/common/playlist_artwork.dart
- Offline: playlists restored from cache when server unreachable — CL 1.0.6 (#29)
- Web Stream: local playlists (create/update/delete stored locally) — youtube_service.dart:506-620
- Desktop sidebar playlist list + create-playlist modal — widgets/navigation/desktop_navigation_sidebar.dart
- "Local playlists independent of server" (README roadmap ticked) — only evidenced for Web Stream mode
- NOT present: smart playlists (website claims "Smart Playlists" **[CLAIM]**), M3U import/export, public/comment editing

## 9. Offline / downloads / cache

- Download song / album / playlist / all favorites / all albums of an artist / entire library ("Download All Library" with confirm) — song_options_modal.dart:271-380, album_screen.dart, playlist_screen.dart, favorites_screen.dart, artist_screen.dart, settings_storage_tab.dart:1004
- Uses Subsonic `/download` (original file, never transcoded) and validates file size vs `song.size` (64 KB floor) — services/offline_service.dart:299-450
- Each download also stores cover art (600 px, by song id and coverArt id) and lyrics (synced + plain) for offline — offline_service.dart:458-490
- Parallel downloads 1–5 ("slower but stable" ↔ "faster") — offline_service.dart:96,522-532, arb:parallelDownloads
- Keep Screen On during download (wakelock) — offline_service.dart:561, arb:keepScreenOnDuringDownload
- Durable playlist download state machine: sequential queue, persisted, auto-resume at startup, cancel, outline-check (queued) vs solid-check (complete) badges — offline_service.dart:100-300,681
- Active Downloads detail screen with per-song log (queued/downloading/done/failed) and Playlist Downloads status panel — screens/media/download_playlist_status_screen.dart, settings_storage_tab.dart:808-900
- Custom download folder incl. SD card — offline_service.dart:118-137, arb:downloadFolder, CL 2.0.0 (#193)
- Downloaded stats (count • size), Delete All Downloads, tap-to-remove album/playlist downloads — settings_storage_tab.dart, arb:deleteDownloads
- Offline mode: automatic fallback to downloaded music when server unreachable; orange "Offline Mode – Playing downloaded music only" banner; "Local Files Mode" banner — auth_provider.dart:42,111, arb:offlineModeBanner, CL 1.0.1
- Offline scrobble queue: failed scrobbles persisted and flushed on next login — offline_service.dart:796-850, auth_provider.dart:95
- Playback prefers local/downloaded file over stream — player_provider.dart:3049-3056
- Cache toggles: Image cache, Music (metadata) cache, BPM cache — services/cache_settings_service.dart, settings_storage_tab.dart:117-145
- Cache sizes shown (songs/stream cache, artwork cache, total) with per-cache clear + Clear All — cache_settings_service.dart:80-185
- Stream cache: `LockCachingAudioSource` temp file when transcoding; YouTube stream disk cache — player_provider.dart:3069-3080, youtube_service.dart:36-60
- Normalized cover-art cache keys (no duplicate fetches); reduced image cache for low-memory devices — CL 2.0.0 (#199), CL 1.0.5
- GrapheneOS background download fixes — CL 1.0.10

## 10. Transcoding / streaming quality

- Enable Transcoding toggle; WiFi bitrate and Mobile bitrate: Original / 64 / 128 / 192 / 256 / 320 kbps (defaults WiFi original, mobile 192) — services/transcoding_service.dart:6-70
- Format: Original(raw) / MP3 / Opus / AAC — transcoding_service.dart:29-36
- Smart Transcoding: auto-picks WiFi vs mobile bitrate from `connectivity_plus`, live network pill + "Active bitrate" — transcoding_service.dart:106-130, arb:smartTranscoding, CL 1.0.8
- Manual connection-type selector when smart mode off — transcoding_service.dart:175
- Sends `maxBitRate` + `format` on `/rest/stream` (Jellyfin equivalent) — subsonic_service.dart:531, jellyfin_service.dart:178
- Seeking while transcoding fixed via caching source — CL 1.0.13 (#170)
- Downloads are never transcoded — offline_service.dart:431

## 11. Lyrics

- Sources in order: downloaded local lyrics → OpenSubsonic `getLyricsBySongId` (structured/synced) → `getLyrics` (plain) → LRCLIB fallback; Jellyfin lyrics endpoint; Web Stream uses LRCLIB — now_playing_screen.dart:95-150, subsonic_service.dart:1010-1090, jellyfin_service.dart:830
- LRCLIB fallback toggle; multi-tier `/get` then fuzzy `/search` with title/artist candidate cleanup, in-memory cache — services/lrclib_service.dart, arb:enableLrcLibFallback, CL 1.0.13
- LRC parser `[mm:ss.xx]` — services/lrc_ttml_parser.dart (file named TTML but only LRC parsing implemented)
- Synced lyrics view: auto-scroll & centering, tap line to seek, "Back to current", interlude animated dots, slide-up/fade transition — widgets/lyrics/lyrics_list_view.dart, interlude_dots_widget.dart
- Word-by-word (karaoke) rendering path exists in the widget, but no parser populates word timings **[STUB]** — widgets/lyrics/lyrics_line.dart:136, models/lyric_word.dart
- Lyrics display settings: Blur unfocused lines, alignment Left/Center, Active line glow — settings_display_tab.dart:1022-1060, CL 2.0.0 (#184)
- "Live Lyrics Under Artwork" pill (frosted, multiline, tap opens lyrics; hidden if no synced lyrics) — now_playing_screen.dart:336, arb:lyricsUnderArtwork, CL 2.0.0/2.0.2
- Tap cover to show lyrics; lyrics button in bottom actions — CL 1.0.9, now_playing_bottom_actions.dart
- Landscape: lyrics on right 60%, art left 40% — CL 1.0.4
- Desktop lyrics slide-over panel (380 px, blurred art backdrop, SYNC badge, seek-on-tap) and fullscreen lyrics mode — widgets/navigation/desktop_lyrics_panel.dart, arb:fullscreen, REL 1.0.2
- Lyrics preloaded for next song — player_provider.dart:1150
- Lock-screen lyrics / iOS Live Activity / Dynamic Island / Android RemoteViews lyrics **[GONE]** — CL 1.0.10; removed 1.0.13 (iOS) and no `live_activities` dep or service in 2.0.2
- Windows lyric-line toast notifications (`local_notifier`) **[DISABLED]**: service methods exist, no callers — services/windows_system_service.dart:145-205
- Bluetooth (AVRCP) lyrics **[GONE/orphaned]**: ios/Runner/iOSBluetoothPlugin.swift remains but no Dart channel uses it — CL 1.0.10
- Lyrics wake lock **[GONE]** — CL 1.0.12; WakelockPlus only used for downloads in 2.0.2

## 12. Casting / UPnP / jukebox / remote

- Unified "Connect to a Device" modal (mobile sheet / desktop dialog) listing This device + Cast + DLNA — screens/connect/connect_devices_modal.dart, widgets/navigation/cast_button.dart
- Google Cast / Chromecast (bundled fork `packages/flutter_chrome_cast`): discovery, connect, load media with art (generic metadata, 1280×720 art), play/pause/seek/volume, position sync, auto-advance on finish, handover at current position, MIME resolution from URL — services/cast_service.dart, CL 1.0.5/2.0.2 (#235)
- UPnP/DLNA renderer support (own SSDP discovery + SOAP AVTransport/RenderingControl): SetAVTransportURI with DIDL-Lite metadata, play/pause (Stop fallback)/seek/next/prev, volume get/set, `SetNextAVTransportURI` pre-queue, 1 s polling, renderer-initiated skip detection, auto-disconnect after 30 failed polls — services/upnp_service.dart, CL 1.0.9/2.0.2
- Hardware/Android Auto volume keys routed to remote renderer (remote-volume media session) — audio_handler.dart:438-490
- AirPlay route-picker button on iOS (native `AVRoutePickerView`) — widgets/navigation/airplay_button.dart, ios/Runner/AirPlayButtonFactory.swift
- Subsonic Jukebox mode: toggle in Server settings; controller screen (now playing art, transport, gain slider, queue list, shuffle/clear/remove, 5 s polling); "Play on Jukebox"/"Add to Jukebox Queue" in song menu; 501 not-supported help text; main transport controls drive jukebox when enabled — services/jukebox_service.dart, screens/media/jukebox_screen.dart, player_provider.dart:301-365, CL 1.0.6/1.0.13
- Musly Connect (Spotify-Connect-like LAN remote: UDP beacon discovery, embedded HTTP/WebSocket server, remote play/pause/seek/volume, 1-tap queue transfer) **[DISABLED]**: service + settings toggle exist, provider registration and device list commented out; changelog says "Temporarily Unavailable" — services/musly_connect_service.dart, main.dart:276,381, connect_devices_modal.dart:205, CL 2.0.0
- Musly BeatSync (multi-device synced party audio: NTP offset, scheduled start, drift correction, ±50 ms calibration nudge) **[DISABLED]** — services/beatsync_service.dart, screens/connect/beatsync_party_screen.dart (no route to it), CL 2.0.0 (commented out)

## 13. Android Auto / CarPlay / system media / widgets / desktop / TV

- Android Auto via audio_service MediaBrowserService: root Recent / Albums / Artists / Playlists (Web Stream mode: Recent + Playlists), artist→albums→songs, voice & keyboard search, play-from-search, works with app closed (headless engine), loading spinner state, artwork, downloaded-first data, remote playback state — services/audio_handler.dart:177-420, library_provider.dart:489-625, android res/xml/automotive_app_desc.xml
- Media notification / lock-screen controls, headset media button click handling — audio_handler.dart:132-176
- iOS Control Center / lock screen Now Playing with 1200 px art, prev/next instead of ±15 s — CL 1.0.8/1.0.10
- CarPlay **[DISABLED]**: `CarPlaySceneDelegate.swift` (Now Playing template + basic actions list) exists, scene manifest commented out pending entitlement — ios/Runner/Info.plist:66, README roadmap
- Windows SMTC (`smtc_windows`): metadata, artwork, timeline, hardware media keys, AppUserModelID "Musly" — services/windows_system_service.dart, CL 2.0.0
- Windows taskbar progress indicator (`windows_taskbar`) — windows_system_service.dart:127-136
- Discord Rich Presence (desktop only) — see §16
- Desktop layout: 240 px left sidebar (Home/Search/Library/Settings, Your Library: Playlists, Liked Songs, Radio, create playlist, Disconnect), 90 px bottom player bar (art, heart, shuffle/prev/play/next/repeat, progress, volume+mute, connect, queue & lyrics toggles), right queue panel, lyrics panel, hover micro-interactions — widgets/navigation/desktop_*.dart, right_sidebar.dart, CL 2.0.2
- Desktop: min window 800×560; global Space = play/pause (ignored in text fields); "Hide Window Titlebar / Decorations" (Linux Wayland/tiling WMs); borderless fullscreen lyrics — main.dart:173-178, main_screen.dart:446, arb:hideWindowTitlebar
- Android TV: native TV detection (leanback/UI mode/no-touch/model keywords), D-pad focus traversal, media-key shortcuts (play/pause/next/prev/ff/rew) — MainActivity.kt:26-64, services/tv_detection_service.dart, widgets/navigation/tv_remote_scope.dart, CL 2.0.0 (#183)
- High refresh rate request on Android (`flutter_displaymode`) — main.dart:156
- Android 13 themed (monochrome) icon — CL 2.0.0
- Home-screen widget: NOT present (no AppWidget/home_widget) — grep
- Linux build uses libmpv; Windows NSIS installer; macOS DMG; universal APK; CI release for Android/iOS/Windows/Linux/macOS — installer.nsi, packaging/macos, .github/workflows, CL 1.0.9/1.0.11

## 14. Now Playing UI / customization / themes

- Full-screen player as draggable sheet: swipe down to dismiss (morph animation), horizontal swipe on artwork to skip with carousel + haptics, PageView for player / lyrics / queue — screens/player/now_playing_screen.dart:183-230,692, REL 1.0.3, CL 1.0.9/2.0.0 (#201)
- Ambient background from 3-color album palette (dominant/vibrant/deep) with blurred gradient blobs — services/palette_service.dart, widgets/common/blurred_gradient_background.dart, CL 2.0.0
- Responsive 2-column landscape layout; small-screen scaling (iPhone SE) — CL 2.0.1 (#234), utils/screen_helper.dart
- "PLAYING FROM" header with tappable album/artist; marquee title — arb:playingFrom, widgets/now_playing/marquee_text.dart
- 5-star rating bar on Now Playing (respects "Show Star Ratings") + 1-tap heart + add-to-playlist button — CL 2.0.2
- Bottom actions: Cast/Connect, Lyrics, Queue; More menu: Sleep Timer, Playback Speed, Preserve Pitch — now_playing_bottom_actions.dart, now_playing_more_menu.dart
- Custom thin sliders with grow-on-drag thumb — widgets/now_playing/playback_progress_slider.dart, CL 1.0.12
- Mini player with optional Heart / Repeat / Shuffle buttons (3 toggles), LIVE badge for radio — widgets/navigation/mini_player.dart, arb:showMiniPlayerHeart/Repeat/Shuffle
- Player Interface toggles: Show Volume Slider, Show Star Ratings, Live Lyrics Under Artwork — services/player_ui_settings_service.dart
- Theme: System / Light / Dark — settings_display_tab.dart:1084
- Accent colors: red, pink, orange, yellow, green, blue, purple + Material You dynamic color (Android 12+, fallback elsewhere) applied app-wide — services/theme_service.dart:4-36, CL 2.0.2/1.0.8
- "Circular Design" (liquid-glass floating rounded nav bar & player with blur; mobile only) — arb:circularDesignLabel, theme_service.dart:94, main_screen.dart `_buildGlassBottomNav`
- Artwork Style editor: shape Rounded/Circle/Square, corner radius 0–24 px, shadow None/Soft/Medium/Strong, shadow color Black/Accent, live preview — settings_display_tab.dart, player_ui_settings_service.dart:125-163, CL 1.0.7
- Now Playing custom theme system (theme manager, 5-tab editor, mesh/gradient/blur/custom-code backgrounds, rotating/pulsing artwork, export/import) **[GONE]** — CL 1.0.13 and orphan arb keys (themeSaved, themeSafeMode…); no theme editor code in 2.0.2
- Language selector: 27 bundled locales, "System Default (<language>)", native names + flags + Crowdin completion %, runtime switch — services/locale_service.dart, CL 2.0.1/2.0.2
- OTA translation updates: "Check for Translation Updates" pulls ARB files from GitHub raw and discovers new languages via GitHub API — services/translation_ota_service.dart:86,125
- Onboarding welcome tour (3 slides, keyboard nav on desktop, replayable from Settings → About) — screens/onboarding/onboarding_screen.dart
- Settings organised in 6 tabs: Playback, Storage, Server, Display, Support, About — screens/settings/settings_screen.dart:57-62

## 15. Scrobbling / ratings / favorites / stats

- Server scrobble (`scrobble` submission=true) only after ≥50% or ≥240 s played (30 s if duration unknown); "now playing" (`submission=false`) on start and on gapless auto-advance; Jellyfin playback reporting; offline queueing — player_provider.dart:95-107,1592-1602, subsonic_service.dart:998, jellyfin_service.dart:628, CL 2.0.0 (#207,#210)
- No direct Last.fm/ListenBrainz integration (server-side only) — grep
- Star/unstar songs, albums, artists (Subsonic star/unstar; Jellyfin favorites) — subsonic_service.dart:935-965; heart in mini player, Now Playing, desktop bar, album screen, song menu
- 1–5 star song ratings via `setRating` with star picker dialog, remove rating; feeds recommendation engine — song_options_modal.dart:403-490, player_provider.dart:3339, CL Unreleased (#27)
- Liked Songs / Liked Albums collections — favorites_screen.dart, album_collection_screen.dart
- Listening history + total plays stat + clear — §3
- Musly Wrapped / "Musly Playback" year-in-review: 100% on-device; seasonal unlock Nov 24 → Jan 15 (mobile only, excluded on desktop); story slides Intro, Minutes listened (total hours, unique tracks), Musical Chronotype (4 types), Genre Galaxy, Top 5 Songs with countdown, Top 5 Artists, Listening Personality archetype (5 archetypes + traits), percentile/superfan badges, recap card, "Play Your Top Songs"; long-press to pause story; glassmorphic/particle visuals; dev preview unlocked by tapping version 8× / debug mode — services/wrapped_service.dart:117-165, screens/wrapped/wrapped_screen.dart, settings_about_tab.dart:131,243, CL 2.0.0/2.0.2
- "Add to Library" action for Web Stream songs (shows "already in server library" otherwise) — add_to_menu.dart:106

## 16. Integrations

- Discord Rich Presence (Windows/Linux/macOS, `dart_discord_rpc`, app id 1465763539246645252): title, artist, album, elapsed/remaining timestamps; toggle; second-line style Artist / Song title / App name; debounce — services/discord_rpc_service.dart, settings_display_tab.dart:650-706
- yt-dlp / YouTube Music (§1) — services/ytdlp_service.dart
- LRCLIB (§11)
- GitHub Releases update checker: "Update Available" dialog with current/latest, What's New, Download, Later — services/update_service.dart:51, main_screen.dart
- Crowdin community translations + OTA sync — crowdin.yml, translation_ota_service.dart
- Octo-Fiesta auto-detection (§1)
- Analytics: Countly added in 1.0.9, fully removed in 2.0.0 (AnalyticsService now only tracks local app-rating state; orphan arb keys for "Anonymous Analytics") — services/analytics_service.dart:4-5, CL 2.0.0
- NOT present: Last.fm, ListenBrainz, Sonos, Kodi, share links — grep

## 17. Sleep timer / misc

- Sleep timer presets Off / 5 / 10 / 15 / 30 min / End of Song in Now Playing menu; provider also supports custom duration, "finish current song", and fade-out over last N s (default 30 s, 5–300) — arb strings for custom duration & fade-out exist but the 2.0.2 menu only exposes presets — now_playing_more_menu.dart:15-105, player_provider.dart:1255-1337, arb:customSleepTimer, fadeOut
- Privacy policy acceptance dialog on first run (Accept / Decline & Exit) — widgets/dialogs/privacy_policy_dialog.dart, main.dart:46
- Support/donation dialog after ~8 min usage with "Don't show again"; Support tab (Discord, donation); "Rate Musly" in-app rating dialog with feedback — services/usage_time_service.dart, widgets/dialogs/support_dialog.dart, settings_support_tab.dart, settings_about_tab.dart:353
- About tab: version, platform, developer, GitHub, changelog, report issue, Discord, Welcome Tour — settings_about_tab.dart
- Emulator block on release mobile builds (`safe_device`) — main.dart:69-75 (HEAD), arb:emulatorNotAllowed
- Haptic feedback on swipes/alphabet jump; snackbar UI feedback helper — utils/ui_feedback.dart
- Fantasy easter egg (§4)

## 18. Backup / restore

- None. No settings/data export, import or backup in source (theme export/import existed only in 1.0.13 theme system, now gone) — grep of lib/ for backup/export/import

## 19. Platform-specific summary

- Android: Android Auto, Material You dynamic color + themed icon, Chaquopy yt-dlp, Google Cast, audio focus fade, SD-card download folder, READ_MEDIA_AUDIO perms, high refresh rate, TV mode, universal APK; no boot receiver — AndroidManifest.xml, CL 1.0.5
- iOS (min 15.0): AirPlay picker, Control Center integration, no Web Stream, CarPlay code present but disabled; orphan native iOSSystemPlugin/iOSBluetoothPlugin — ios/Runner/*
- Windows: SMTC, taskbar progress, NSIS installer, position polling workaround — windows_system_service.dart, installer.nsi
- Linux: libmpv backend, hide titlebar option, ALSA dep — pubspec, CL 2.0.0 (#208)
- macOS: media_kit backend, DMG packaging — packaging/macos
- Desktop common: Spotify-like 3-pane UI, Discord RPC, keyboard space shortcut, fullscreen lyrics, Wrapped excluded
- Web: `web/` folder exists; code guards `kIsWeb` but not an advertised target
- License CC BY-NC-SA 4.0; "do not redistribute to Play Store" — README
- 100% telemetry-free claim — CL 2.0.0, PRIVACY_POLICY.md

---

## Appendix: advertised-but-missing at v2.0.2 (quick list)

| Feature | Where advertised | State in v2.0.2 source |
|---|---|---|
| 10-band equalizer + presets | CL 1.0.1, website | no code |
| Now Playing theme editor | CL 1.0.13, arb keys | no code |
| Lock-screen / Live Activity / Bluetooth / Windows-toast lyrics | CL 1.0.10 | no Dart code (Windows method unused; iOS swift orphaned) |
| Pitch control / preserve pitch | CL 1.0.11, menu toggle | no native handler → speed only |
| Lyrics wake lock | CL 1.0.12 | not present |
| Musly Connect, BeatSync | CL 2.0.0, fastlane, arb | services exist, UI/provider commented out |
| CarPlay | README roadmap | swift present, plist disabled |
| True crossfade | CL 2.0.0 | fade-out of outgoing track only |
| BPM analysis | settings UI | genre-based estimate |
| Radio Browser API | README | not found |
| Smart playlists | website | not found |
| Radio station management | REL 1.0.3 (API) | API methods only, no UI |
| Custom sleep timer duration / fade-out option | arb keys, provider | provider supports; menu exposes presets only |
