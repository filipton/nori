# Symfonium (Android) - exhaustive feature inventory

Internal planning note, not published.

Compiled 2026-09-17 from primary sources only. Current stable: 15.0.1 (2026-08); beta: 15.1.0 b6.

## Source tags

- `[home]` https://symfonium.app/
- `[uc/servers]` https://symfonium.app/android-music-player-plex-jellyfin-subsonic-navidrome/ ; `[uc/cloud]` /android-cloud-music-player/ ; `[uc/hires]` /android-hi-res-dsd-music-player/ ; `[uc/peq]` /android-music-player-peq-autoeq/ ; `[uc/cast]` /android-music-player-sonos-chromecast-dlna/
- `[n/X]` release notes https://symfonium.app/news/version-X/ (e.g. `[n/1500]` = 15.0.0, `[n/5-8-0]` = 5.8.0). All 67 posts (1.2.0 .. 15.0.0) were read in full.
- `[forum/15.1b]` https://support.symfonium.app/t/15101 (15.1.0 beta 6 changelog)
- `[docs/<slug>]` https://docs.symfonium.app/wiki/.../<slug>/ (settings, providers, other)
- `[play]` Google Play listing text (id app.symfonik.music.player)
- `[strings]` English string resources of the official APK (aapt2 dump, 2413 strings). Setting titles are quoted verbatim.

Provider marks: **(non-Subsonic)** = does not apply to a Subsonic/OpenSubsonic/Navidrome connection; **(Subsonic)** = Subsonic-specific; **(OpenSubsonic)** = needs server-side OpenSubsonic extension; **(file providers)** = Local device (folder/SAF mode), SMB, WebDAV, cloud drives, which share Symfonium's own tag parser.

---

## 1. Providers / servers

### Supported sources
- Subsonic API servers: Navidrome, LMS, Gonic, Funkwhale, Ampache, Astiga, Airsonic(-Advanced), original Subsonic, Jpsonic, Nextcloud Music (Subsonic) `[docs/subsonic-opensubsonic-media-provider-configuration]` `[n/1-2-0]` `[n/1-3-0]` `[n/3-2-0]`
- OpenSubsonic officially embraced as the target API (Subsonic) `[n/5-1-0]` `[n/6-0-0]`
- Plex (non-Subsonic) `[home]`
- Emby (non-Subsonic) `[home]`
- Jellyfin incl. Jellyfin 12 (non-Subsonic) `[home]` `[n/1500]`
- Kodi as library source (non-Subsonic) `[home]`
- Audiobookshelf (experimental; authors -> artists, books -> audiobook albums) (non-Subsonic) `[n/1410]` `[docs/supported-features-by-media-providers]`
- Local device: "By folders / SD Card" (SAF, Symfonium parser) or "By Android" (Media Store list + Symfonium parser); pure Media Store mode removed for new users (non-Subsonic) `[docs/local-device-media-provider-configuration]` `[n/1200]`
- Samba SMB v2/v3 with custom port; no anonymous login (non-Subsonic) `[n/6-0-0]` `[n/1000]` `[strings]`
- WebDAV, incl. invalid HTTPS certs accepted (non-Subsonic) `[n/6-0-0]` `[n/1100]`
- Google Drive (incl. folder shortcuts, shared folders, trashed files ignored), OneDrive, Dropbox, Box, pCloud (non-Subsonic) `[n/7-0-0]` `[n/9-0-0]` `[n/9-1-0]` `[n/1010]`
- Subfolder selection for cloud providers (non-Subsonic) `[n/1340]`
- Multiple providers at once in one merged library; provider order configurable `[docs/settings-manage-media-providers]`
- Internet radios as a pseudo-source (see section 17) `[n/5-9-0]`

### Connection / auth
- Add-provider wizard: full URL or IP+port, auto-detect of servers on the LAN, "Select your primary music source" first-run wizard `[docs/subsonic-...]` `[strings]`
- Connection type HTTP / HTTPS / "HTTPS (Ignore cert)" `[strings]`
- Subdirectory (reverse-proxy path) field, editable `[n/1-5-0]` `[strings]`
- Subsonic token auth with auto-detection of unsupported token auth; "Legacy authentication" option (Subsonic) `[n/2-0-0]` `[docs/subsonic-...]`
- OpenSubsonic API-key authentication ("Use an API key" / "Use an account") (OpenSubsonic) `[n/1160]` `[strings]`
- "Send basic authentication headers" / proxy login+password for reverse proxies; "Send default login and password" (Subsonic, Emby/Jellyfin) `[n/5-9-0]` `[strings]`
- Custom HTTP headers per provider (Subsonic, Emby/Jellyfin) `[n/9-0-0]`
- Secondary connection (home vs outside) with automatic switch; API to force primary/secondary; "Secondary connection maximum bitrate" (Subsonic, Emby/Jellyfin) `[n/9-0-0]` `[n/1340]` `[n/1500]`
- Client certificate mTLS: PKCS#12 import with password or Android KeyChain (Subsonic, Emby, Jellyfin, Audiobookshelf) `[n/1410]` `[strings]`
- Zstandard compression for server communication `[n/1410]`
- "Wi-Fi only" per provider (no communication on mobile) `[n/2-0-0]`
- Plex: plex.tv web auth, link/QR-code auth (Android TV), managed/home users with PIN, shared servers, 2FA, relay detection + retry for direct connection (non-Subsonic) `[docs/plex-media-provider-configuration]` `[n/1310]` `[strings]`
- Jellyfin Quick Connect (non-Subsonic) `[n/1350]`
- Debug logging toggle inside the add-provider wizard with "send logs" `[strings]`

### Sync model
- Offline-first: full library synced into a local DB; automatic sync when the server reports changes; differential sync where supported; long-press Sync forces full sync `[docs/sync-media-provider-data]`
- Sync manager screen (per-provider last status; can be a home shortcut); "Sync all" shortcut `[n/1370]` `[n/1400]`
- Detailed sync status with counters; first-sync status on home page; last-sync details from the filter sheet `[n/5-6-0]` `[n/6-1-0]` `[n/1130]`
- "Disable library auto sync" `[n/1-9-0]`
- Subsonic "Compatibility mode (Very slow sync)" for servers without empty-query search3; "Do not use download endpoint" (Subsonic) `[docs/subsonic-...]`
- Subsonic "Fetch additional metadata" (getArtistInfo2/getAlbumInfo2 bios, images) + "Clear scraped data" (Subsonic) `[n/4-0-0]` `[n/1310]`
- Navidrome: lastPlayed sync, server-side favourite removal, offline playcount sync optimisations, ID migration after upgrades (Subsonic) `[n/2-0-0]` `[n/5-5-0]` `[n/4-3-0]` `[forum/15.1b]`
- Multiple music folders exposed as separate libraries (Subsonic) `[n/6-0-0]`; genre-per-library filtering workaround (Subsonic) `[n/8-0-0]`
- Artist genres emulated from song+album genres (Subsonic) `[n/1-7-0]` `[n/9-0-0]`
- Artists with only unsupported roles ignored (Subsonic) `[n/1220]`
- Plex "Full sync" (moods, styles, album types); "Favorites as rating" (5 stars = favourite); per-track genres (non-Subsonic) `[docs/plex-...]` `[n/4-3-0]` `[n/1140]`
- Emby/Jellyfin "Sync collections" toggle (collections become tags) (non-Subsonic) `[n/1130]`
- "Exclude libraries" per provider; per-library media type Music/Audiobook `[strings]` `[n/1410]`
- "Automatic image cache" and "Automatic media offline cache" per provider/library `[docs/subsonic-...]`
- "Automatic playlist import" per provider (servers: online-first; files: M3U/PLS read-only) `[n/1500]` `[n/9-0-0]`
- "Scrobble as advertisement" per provider (lets external scrobblers ignore it) `[n/1000]`
- "Clear playback history" per provider `[n/1210]`

### OpenSubsonic extensions consumed (OpenSubsonic)
- Multiple artists / album artists, multiple genres, composers, moods, record label, BPM, comments `[n/6-0-0]`
- Seek during transcoding (LMS, Gonic) `[n/6-0-0]`
- getLyricsBySongId; enhanced songLyrics with word cues and translations `[n/7-2-0]` `[n/1500]`
- Disc subtitles, album releaseDate, album version, explicitStatus `[n/1000]` `[n/1300]` `[n/1160]`
- channelCount / samplingRate / bitDepth fields `[n/1010]`
- formPost extension for playlists `[n/1000]`
- New transcoding API `[n/1365]`
- sonicSimilarity, grouping tags, playbackReport (incl. playback speed), work/movement metadata `[n/1500]`
- Public share links from "Share file" (Subsonic/Navidrome) `[n/1500]`
- Bookmarks API used for resume points (Subsonic) `[n/4-2-0]`
- Internet radio station import (Subsonic) `[n/5-9-0]`

---

## 2. Library & browsing

- Home page fully configurable: icon, up to 4 shortcut rows, overview rows, playlist rows, order, per-row style `[n/8-0-0]` `[n/1300]` `[n/1400]` `[docs/settings-interface-home-page]`
- Overview rows available: recently added albums/tracks/artists/album artists, recent (by release date) albums, random albums/tracks/artists/album artists, rediscover albums/tracks, most played & last played albums/tracks/artists/album artists, favourite albums/artists/tracks/playlists/radios, 5-star tracks, continue album / continue track listening `[strings]` `[n/4-1-0]` `[n/5-8-0]` `[n/1350]`
- Home row options: max items (0 = header link only), columns portrait/landscape, rows, card/outlined card, overline/underline, custom header text, clickable header, hide if empty, show play button, per-row sort for favourites, "Copy from" / "Apply to all", live preview `[n/1140]` `[n/1100]` `[n/1400]` `[strings]`
- Home styles: grid, row, multiline row, carousel, landscape, overlay grid, vertical overlay `[n/1400]`
- Swipe-to-refresh on home reloads random rows `[n/1300]`
- Home shortcuts: mixes, library, search, queues, settings, sync all, sync manager, source filter, "Play on", change style, change profile `[n/7-2-0]` `[n/1300]` `[n/1360]`
- Library page: configurable shortcut buttons, order, columns, spacing, horizontal/list styles `[docs/settings-interface-library-page]` `[n/1200]`
- Browse nodes: albums, artists, album artists, composers, compilation artists, genres, years, decades, moods, styles, tags, track tags, languages, record labels, countries, instruments, album types, album grouping, occasion, media type, playlists, files, internet radios `[strings]` `[n/4-0-0]` `[n/1330]` `[n/1140]` `[n/1150]`
- File/folder mode browsing for providers exposing files, incl. Subsonic `[n/1-8-0]`; sorts, filter, play/shuffle/queue bar, 2-level recursive folder play, favourite files, add-to-playlist from files `[n/5-2-0]` `[n/4-1-0]` `[n/1120]` `[n/1365]`
- Global source filter: toggle providers/libraries; "Hide media not available offline"; "Hide non playable media"; "Hide unavailable providers" `[docs/globally-filter-...]` `[n/9-0-0]`
- "Automatic offline mode": offline filter tied to Wi-Fi state `[n/4-0-0]`
- Active-filters hint when filters hide everything `[n/1500]`
- Profiles: bundle of app style + smart queue settings + global filters (music vs audiobook mode) `[n/1360]`
- Contextual list filters: only favourites, album artists only, hide compilations, hide composers only, hide small albums/artists `[strings]` `[n/5-1-0]`
- Quick text filter in most lists; optional always-visible filter bar `[n/5-6-0]` `[n/1200]`
- Smart filters shown as chips above lists `[n/1160]`
- Sort saved per screen and sub-category; tap again to invert; "Inverted sort arrows" option `[n/3-2-0]` `[n/1400]`
- Sorts: name, sort name, display artist, display artist+year, display album artist, display composer, year, release date, original release date, date added, last played, play count, favourite date, rating, filename, track number, related (artists), random, random (stable), random albums (stable) `[strings]` `[n/6-0-0]` `[n/1160]` `[n/1350]`
- Display modes per list: list, small list, single-line list, text-only list, grid, small grid, card grid, round grid, image grid, landscape grid, fanart grid (artists), overlay grids; columns (min 1) separately for portrait/landscape; item spacing `[n/1400]` `[n/1200]` `[n/1120]` `[n/8-0-0]`
- Fast-scroll bar with value indicator; jump loading; "Larger page size" `[n/1110]` `[strings]`
- Collapsible letter headers in artist/genre views (long press) `[n/4-1-0]`
- Multi-select with drag-select; actions respect selection order; personal mix from selected artists/genres `[n/5-8-0]` `[n/5-9-0]` `[n/1130]`
- Long-press drag and drop to play / shuffle / queue / play next / add to playlist targets `[docs/long-press-drag-and-drop]` `[n/1350]`
- Configurable track swipe left/right actions: queue, queue next, add to playlist, toggle favourite, rate, add to permanent offline cache `[n/1350]` `[n/1410]`
- Default track action (play, play without auto queue, queue, resume ...); separate audiobook track action; album click action in playlists; search-page track action `[docs/settings-interface-navigation-and-media-start]` `[n/1500]`
- Clicking a track in a filtered list queues the whole list `[n/4-0-0]`
- Artist page: albums grouped by release type, "Appears on", top tracks (source: default/favourites/most played; count), about rows reorderable, biography with links, play-button action (albums in order / tracks chronological), rating bar `[n/8-0-0]` `[n/1350]` `[n/1100]` `[n/1330]`
- Album page: "More from artist", quality badge in header, composer / track artist rows, group by work + movement names (classical), track thumbnails, multiline titles, disc subtitles, album version, explicit/clean badges, hide track numbers `[docs/settings-interface-album-page]` `[n/1330]` `[n/1150]`
- Genre page: albums / artists / album artists / top tracks sections `[docs/settings-interface-genre-page]`
- "Skip genre/artist details page" straight to artists/albums/tracks `[n/8-0-0]`
- Source provider shown on album/artist pages `[n/9-1-0]`
- Active track highlighted in every list `[n/5-8-0]`
- Additional + second additional track info in lists: favourite, rating, duration, offline status, play count, codec, bitrate, quality, BPM, year `[n/1-5-0]` `[n/3-1-0]` `[n/4-1-0]`
- Favourite / offline / playlist-type overlay icons on artwork `[n/1400]`
- Item counts in lists `[strings]`
- Hide individual tracks from library ("Hide from library", restorable) (file providers) `[n/5-9-0]`
- Delete file from device (local SAF) `[n/1200]`
- Share menu for albums/artists/tracks; "Share file" to other apps; web search; Genius search `[n/4-0-0]` `[n/1160]` `[n/9-1-0]` `[n/1120]`
- Track information sheet (path, MIME, codec, sample rate, bits, channels, MBID ...) `[strings]`
- Artist play count / last played derived from track plays; album play count mode avg/sum/min `[n/1350]` `[n/1370]`
- Registered as a handler for audio files opened from other apps `[n/2-0-0]`

## 3. Search

- Global search across artists, albums, tracks, playlists with filter chips (single-line option) `[docs/settings-interface-search-page]`
- Multi-token search in any order across multiple fields `[n/7-2-0]`
- Transliteration search: accent-insensitive, ASCII search of Cyrillic/Kanji/Chinese, Simplified/Traditional cross search (Android 10+) `[n/5-5-0]`
- Searches sort titles and disc subtitles too `[n/4-1-0]` `[n/1100]`
- Last 15 searches saved `[n/9-0-0]`
- Only-favourites filter in search `[n/4-3-0]`
- Search sort option and per-search-page track action `[docs/settings-interface-search-page]`
- Search inside the now playing queue `[n/1140]`
- Voice search: MEDIA_PLAY_FROM_SEARCH incl. incomplete queries, Google Assistant, playlists included `[n/4-3-0]` `[n/7-2-0]` `[n/1140]`
- Search button placement: nav entry, top bar, library shortcut; second tap clears field `[n/1200]` `[n/1320]`

## 4. Playback engine

- Formats: FLAC, ALAC, Opus, AAC, xHE-AAC, MP3, Vorbis, WAV, AIFF, WMA/ASF, Musepack, APE, TTA, WavPack, AMR, AU, MKA/WebM, M4B, DSD (DSF/DFF/IFF/DSDIFF, DST-compressed) `[home]` `[n/1500]` `[n/5-0-0]` `[n/5-4-0]`
- Own FFmpeg 8.1 based engine; native WMA/AIFF/WavPack extractors; internal Vorbis/AAC/FLAC decoders to dodge device bugs `[n/1410]` `[n/1500]` `[n/7-0-0]`
- Gapless playback (local; MP3 gapless fixes) `[home]` `[n/1500]`
- Crossfade: separate fade-in/out durations and curves (linear, smooth, bungee, flat); disabled for albums played in order `[docs/settings-playback-transitions]` `[strings]`
- "Mix only" crossfade (overlap without volume change) `[n/1400]`
- Smart Fades: waveform-analysed automatic crossfade points `[n/1230]` `[n/1300]`
- USB exclusive crossfade policy (same format only / lock to first track / lock to DAC max) `[strings]`
- Fade in / fade out on play, pause, seek and manual skip `[n/1140]`
- Playback speed (fine-grained, editable shortcut values) with optional pitch control, "Lock to speed" `[n/5-2-0]` `[n/9-1-0]` `[n/1120]`
- Skip silence (per output, optional "Only for audiobooks") `[n/1500]`
- ReplayGain: track / album / automatic / prefer-with-fallback; R128 tags; normalisation target -18 or -23 LUFS; Plex loudness data `[docs/replay-gain-support]` `[n/9-1-0]` `[n/1410]` `[strings]`
- Repeat off/all/one; shuffle mode restores original order (shuffle indices saved with queue) `[n/1410]`
- Weighted shuffle (spreads artists/albums), can be disabled `[docs/mixes-radio-shuffle-...]`
- "Retain player state" (shuffle, repeat, speed, pitch between plays) `[n/5-4-5]`
- Full playback state persistence across app kill (always on) `[n/2-0-0]` `[n/9-1-0]`
- Resume points per track with min play time; "Never set resume point"; Subsonic bookmarks / Plex / Jellyfin sync `[n/4-1-0]` `[n/4-2-0]`
- Audiobooks: chapters (many tag types, ID3v2 CHAP, server chapters), previous/next chapter buttons, resume rollback, auto-rewind after focus loss, mark as played, per-library audiobook type `[n/1-6-0]` `[n/1210]` `[n/1410]` `[n/1500]` `[n/1130]`
- CUE sheets: external and FLAC-embedded (file providers) `[n/4-1-0]` `[n/1310]`
- Played threshold %, skip-count threshold %, "Reduce last played updates", "Never update skip count" `[docs/settings-playback-playback]` `[strings]`
- Skip count and last-skipped tracked per track `[n/1150]`
- Detailed playback history stored (and backed up) `[n/1320]`
- Audio focus: short-loss action duck / pause / none; "Permanent audio focus loss" `[n/1-9-0]` `[n/6-1-0]`
- Auto play on wired headset / Bluetooth connect; auto pause/resume when volume hits zero; pause on task removal; ignore remote stop; keep paused state on skips; stop repeat-one on skips `[docs/settings-playback-automatic-actions]`
- Previous rewinds first (toggle); long-press next/previous = next/previous album; long-press play = stop `[n/1110]` `[n/5-4-5]` `[n/1350]`
- Circular queue navigation `[n/5-8-0]`
- Headset button mapping: single/double/triple click actions, remap next/previous (incl. force next/previous, toggle favourite), slower click detection `[docs/settings-playback-headset-buttons]` `[n/1370]` `[n/1340]`
- Rewind / fast-forward external commands; configurable skip amounts `[n/1010]` `[n/1400]`
- Playback cache (disk) with pre-cache of N queue tracks, separate Wi-Fi/mobile counts, force first-track pre-cache `[docs/settings-offline-cache-and-download]`
- Bitrate limits: Wi-Fi max, mobile max, "Force instant transcoding" on Wi-Fi loss, metered Wi-Fi / VPN treated as mobile `[docs/settings-playback-decoding-and-transcoding]` `[n/1100]` `[n/1340]`
- "Prefer server version on Wi-Fi" / "Always prefer server version" when the cached copy is transcoded `[n/1-7-0]` `[n/1120]`
- "Ignore offline server errors" (keep skipping to end of queue); faster skipping when server offline `[n/1350]` `[n/1130]`
- Force HTTP/1.1 for playback `[n/1130]`
- Buffering indicator and buffered position in progress bars `[n/5-3-0]` `[n/5-7-0]`
- Waveform extraction (local, server on Wi-Fi, server always) feeding seek bar and Smart Fades `[n/1100]` `[n/1160]`
- Offload mode (DSP coprocessor), "Enabled if device supports gapless" `[n/2-0-0]` `[strings]`
- Media session: configurable notification / Android 13 media controls / Android Auto buttons (skip prev/next toggles + 3 actions incl. favourite, rating toggle, shuffle, repeat, seek) `[n/7-0-0]` `[n/1150]`
- Media-session options: expose / never expose queue, track number in metadata, explicit symbols, delay registration, disable session, art as ALBUM_ART for third parties `[docs/settings-playback-advanced-settings]` `[n/4-3-0]`
- Android 13 output switcher integration (can be disabled) `[n/4-2-0]` `[n/1200]`
- Playback messages / slow-preparation messages toggles `[strings]`

## 5. Audio output / DAC

- Settings stored per output device (speaker, wired, each Bluetooth device, each USB DAC, remote playback); saved outputs listed and removable `[docs/settings-playback]` `[n/1500]`
- USB playback mode per DAC: Android default / Android direct USB (Android 14+ bit-perfect) / USB exclusive (own USB stack) `[strings]` `[n/1500]`
- Native DSD and DoP output; "Force PCM for DSD"; "All to DSD" conversion `[n/1500]` `[strings]`
- Hi-Res PCM above 192 kHz; high-quality resampler; "Maximum output sample rate" per device; "Fixed PCM output format" (rate + depth); sample-rate family preserved `[n/1500]` `[n/6-1-0]` `[forum/15.1b]`
- "Upsample PCM to DAC maximum" in exclusive mode `[n/1500]`
- "Bypass processing" per output (no RG/EQ/DSP/system EQ) `[strings]`
- "Prefer hardware-accelerated codecs" per output `[strings]`
- Vendor routes: native HiBy (SmartAudio), FiiO, iBasso, Shanling DAP DSD paths; "Disable vendor-specific playback routes" `[n/1500]` `[forum/15.1b]` `[strings]`
- USB extras: volume safety check, software-volume fallback, standard USB volume control, DAC warm-up delay, DAC media buttons support, USB permission prompts, handle USB attach `[strings]` `[docs/settings-playback-advanced-settings]`
- Output debug details (supported depths/rates) `[strings]`
- "Restart player on device change" workaround `[n/1100]`
- Now-playing can show current output / renderer / bit depth / sample rate `[n/1400]` `[n/1-10-0]`
- Android 8+ support for older DAPs `[n/1500]`

## 6. DSP / EQ

- 64-bit internal DSP chain for local playback: ReplayGain, PEQ/GEQ, crossfeed, mono, skip silence, speed/pitch `[n/1500]`
- Parametric EQ: 10 bands default, up to 64 in expert mode; filter types peaking, low/high shelf (Q and slope), low/high pass, band pass, notch; per-filter channel All/L/R; min 5 Hz `[n/1370]` `[n/1400]` `[strings]` `[forum/15.1b]`
- Graphic EQ independent of OS libs: 5/10/15/31 bands; expert mode custom band count and frequencies `[n/1370]` `[docs/advanced-equalizer-autoeq]`
- AutoEQ database (4000+ headphones) downloadable; custom GraphicEQ.txt import; EqualizerAPO profile import incl. channel config `[n/1-8-0]` `[n/1410]` `[docs/use-a-custom-autoeq-profile]`
- Built-in presets: flat, bass/treble/vocal boost and attenuation, loudness `[strings]`
- Preamp with linked or per-channel gain (L/R balance) `[n/1010]`
- Compressor and limiter with attack/release/ratio/threshold/knee/noise gate/expander/post-gain (expert) `[strings]`
- Volume boost, bass boost, virtualizer (binaural/transaural) `[n/5-3-0]`
- Crossfeed with light/medium/strong profiles, cutoff and feed level `[n/1500]` `[strings]`
- Stereo-to-mono converter `[n/1400]`
- Named EQ profiles (save as / load), automatic per-output application, API call to load a profile by name, included in backups `[docs/advanced-equalizer-autoeq]` `[n/1500]` `[n/5-3-0]`
- Legacy "Android DSP" engine and system equalizer integration ("Start system equalizer") `[strings]` `[n/2-0-0]`
- Equalizer block size setting `[strings]`
- Casting ReplayGain processor (RG applied via transcoding proxy for Chromecast etc.) `[n/5-4-0]` `[docs/replay-gain-support]`

## 7. Queue

- Multiple media queues: new queue per play action, last 15 kept, each stores position, shuffle, repeat, speed; rename/delete/delete others; load via shortcut, widget or API `[docs/multiple-media-queues]` `[n/4-2-0]` `[n/1200]`
- Queue screen: drag reorder, swipe right = play next, swipe left = remove, scroll to current, save queue as playlist, clear remaining queue, "More actions" `[docs/now-playing-current-queue]` `[n/1100]`
- Play next / queue last / play / shuffle on every item; "Preserve Play Next order" `[n/1370]`
- Track additional-info columns applied to queue list; scrollbar; search `[n/5-6-0]` `[n/1140]`
- Smart Queue (auto-extend at end): random, genre, artist, mood, style, sonic-analysis based; options to queue albums or restart playlist `[n/1210]` `[n/1220]` `[n/1100]`
- Smart Flow (live queue reshaping): Shuffle specialist, Transition maestro (N inserted tracks), Double shot, Artist fan, Echo match, Era enthusiast, Steady vibes; sonic modes need Plex sonic analysis, Jellyfin AudioMuse AI or OpenSubsonic sonicSimilarity `[docs/smart-queue-and-smart-flow]` `[n/1330]` `[n/1500]`
- "Remember Smart flow mode" for new queues `[n/1500]`
- Personal mixes (track mix, album mix, decade mix, per-genre/artist): favourites + forgotten + discovery balancing, excludes 1-2 star, recently skipped and "excluded from mix" tracks; size configurable `[docs/mixes-radio-shuffle-...]` `[n/1230]`
- Instant mix from a track or artist (genre based) `[n/1-5-0]`
- Radio mix from server similar artists / similar tracks (Subsonic getSimilarSongs option "Use similar tracks for Radio mix") `[n/5-7-0]` `[strings]`
- Starting a mix from the current track does not restart it `[n/1010]`
- Queue exposed to Bluetooth / media session `[n/7-2-0]`

## 8. Playlists & smart playlists / filters

- Local playlists; create from any selection; add to several playlists at once; "already in playlist" indicator; add dialog remembers filter per provider `[n/1140]` `[n/5-6-0]` `[n/1500]`
- Provider playlist import with three modes: Offline first (manual push/pull), Online first (auto two-way, needs connection), Read-only (auto pull); mode switchable later; skip duplicates; import all `[docs/import-sync-media-providers-playlists]` `[n/5-4-0]` `[n/1500]`
- Push playlist edits to Plex, Emby, Jellyfin, Subsonic, Audiobookshelf `[n/1-9-0]` `[docs/supported-features-by-media-providers]`
- Lock playlist to one provider, or multi-provider playlists `[strings]` `[n/1-9-0]`
- M3U / M3U8 / PLS import from files (relative and some absolute paths, non-UTF-8), read-only re-sync, strict path mapping option (file providers) `[n/6-0-0]` `[n/7-1-0]` `[n/1325]`
- Export normal playlists to m3u8; export playlist media to Downloads `[n/1200]` `[n/9-1-0]`
- Android Media Store playlist import `[n/1-6-0]`
- Playlist maintenance: remove duplicates, remove missing, remove read-only protection, irreversible sort content (by album, year, artist, random albums ...), per-playlist sort override and display mode `[n/2-0-0]` `[n/1360]` `[n/1110]`
- Playlist tags (hashtags in names auto-converted; Emby tags imported), hide playlists, favourite playlists, playlist thumbnails generated or custom `[n/5-6-0]` `[n/5-8-0]` `[n/1325]`
- Resume / shuffle / play buttons; resuming restores shuffle order `[n/1-10-0]`
- Pin playlist to home as a row; launcher shortcuts (play / shuffle / resume) `[n/8-0-0]` `[n/4-3-0]` `[n/1140]`
- Unavailable tracks greyed out in offline mode `[n/6-0-0]`
- Smart filters: rule groups with AND/OR ("Match all/any"), nested groups, saved/loaded, save as smart playlist `[docs/smart-filters]`
- Smart playlists for tracks, albums, artists: sort, limit, stable random with reseed, duplicate, export/import to file (with thumbnail), copy as normal playlist, inherit global filters, provider/library scope `[n/9-1-0]` `[n/1010]` `[n/1320]` `[n/1360]`
- Default smart playlists import (most played, last played ...) `[n/1-8-0]`
- Smart filter fields: title, album, artist(s), display artist, composer, genre, mood, style, tag, language, label, country, year, dates (added, last played, last skipped, favourite, modified, release, original), play count, skip count, played %, rating, user rating, favourite, album/artist is favourite, is single, album type, compilation, explicit, BPM, comment, duration, codec, bitrate, sample rate, bits, channels, path/filename, offline status (not/partial/full), thumbnail present, resume point, in playlist / not in playlist, in smart playlist, excluded from mix, work/movement/grouping/occasion/media type `[strings]` `[n/4-1-0]` `[n/1150]` `[n/1330]`
- Operators: is / is not / contains / starts / ends / greater / less / before / after / within N days / is present / is missing `[strings]`

## 9. Offline / cache / sync

- Permanent offline cache for tracks, albums, artists, genres, playlists (manual) `[docs/offline-media-cache-downloads-...]`
- Rolling offline cache with size cap and oldest-first eviction; "Move to permanent cache" `[n/9-0-0]`
- Playback cache can feed the rolling cache `[n/9-0-0]`
- Auto offline rules per playlist / smart playlist / artist / genre, with per-rule original-quality override; media dropped when no rule covers it `[n/9-0-0]`
- Automatic offline caching of favourites (tracks, albums, artists) `[n/3-0-0]` `[n/5-5-0]`
- Whole provider / selected libraries automatic cache `[docs/...]`
- Offline cache quality (transcode bitrate) with per-item "Original" override `[n/4-1-0]`
- Download manager: queue, pause/resume/cancel, max simultaneous downloads (1-8), Wi-Fi-only downloads, low-space stop, Force HTTP/1.1 `[n/1130]` `[n/1500]`
- Manage offline files screen: sizes per cache, filter, cleanup of orphaned files, remove all `[docs/settings-manage-offline-files]`
- Export to Downloads folder (tracks, albums, artists, playlists, folders) for other apps `[n/6-1-0]`
- Cache storage location: internal or SD card `[strings]`
- Images and lyrics cached along with offline media `[n/5-4-5]` `[n/1-9-0]`
- Image cache: Wi-Fi-only image downloads, persistent image cache, lossless "High quality images", clear / clean unused `[n/5-2-0]` `[n/4-2-0]`
- Offline write queue: ratings, favourites, play counts and scrobbles recorded offline and pushed later `[n/5-3-0]` `[n/9-1-0]`
- Offline status overlays and smart-filter field `[n/1400]`

## 10. Transcoding

- Server-side transcoding requested from Plex, Emby, Jellyfin, Subsonic, Audiobookshelf with Wi-Fi/mobile bitrate caps (64-320 kbps) `[docs/supported-features-by-media-providers]`
- Subsonic: Opus by default, "Transcode to MP3" option, "Ignore server transcoding settings", automatic transcode of unsupported formats, OpenSubsonic transcoding API (Subsonic) `[docs/subsonic-...]` `[n/4-3-0]` `[n/1365]`
- On-device FFmpeg transcoding engine for unsupported formats and for casting (AAC/ALAC to Chromecast; more cases for UPnP) (mostly relevant to non-Subsonic file providers) `[docs/transcoding-engine]` `[n/1410]`
- Device max sample rate detected; transcodes when unsupported `[n/6-0-0]`
- Per-renderer "Maximum bitrate" override `[n/1410]`
- UPnP renderer capability detection drives direct play vs transcode `[n/1500]`
- API to change Wi-Fi/mobile transcode bitrate `[n/1-10-0]`

## 11. Lyrics

- Plain and synced lyrics: embedded tags (USLT, SYLT, LYRICS, many non-standard), external .lrc next to files, provider lyrics (Plex, Emby, Jellyfin, OpenSubsonic) `[n/1-5-0]` `[n/5-0-0]` `[n/1140]` `[n/7-2-0]`
- TTML and enhanced LRC; karaoke word/syllable highlight; voice colour indicators; background voices hide; translations with colour and word sync `[n/1360]` `[n/1365]` `[n/1400]`
- Lyrics screen settings with live preview: font size/weight, spacing, centring, inactive scale/alpha, drop shadows, timestamps before/after, mini player, progress bar, sync buttons, close button, thumbnail `[docs/lyrics-interface]` `[strings]`
- Tap a line to seek; auto-scroll toggle; temporary +/- offset buttons `[n/9-0-0]` `[n/1100]`
- Instrumental markers, empty-line pauses, identical timestamps merged, hour timestamps `[n/1220]` `[n/1210]`
- Lyrics over cover art, always-visible lyrics panel in landscape, toggle cover/lyrics gesture `[n/1200]` `[n/1300]`
- Bluetooth lyrics (synced line sent as metadata to car/headunit) `[n/4-2-0]`
- Lyrics stored with offline cache; shown when casting to UPnP `[n/1-9-0]` `[n/1015]`
- Keep screen on while lyrics visible `[strings]`

## 12. Casting / remote outputs

- Chromecast (CAF): speed, Assistant skip, transcoding, next-track preload, notification output switcher `[n/3-0-0]` `[n/4-2-0]`
- UPnP/DLNA: gapless, alternative flags, seek-mode fallbacks, stop on external changes, capability detection, loopback casting to local apps (e.g. UAPP) `[docs/settings-renderer-settings]` `[n/3-2-0]` `[n/1360]`
- Sonos: groups view, join/leave, per-speaker volume, sync volume on join, bonded speakers `[n/1350]` `[n/1340]`
- Kodi as renderer with gapless `[n/1-4-0]`
- Plex / Plexamp clients as renderers `[n/1210]`
- Per-renderer settings: volume step (0 disables volume keys), proxy via Symfonium, prefer offline cached version, max bitrate, stop casting on stop `[n/8-0-0]` `[n/1200]` `[n/1410]`
- Remember last renderer; automatic renderer reset when offline; playback migration between renderers `[strings]` `[n/1230]`
- Renderer list sort toggle; renderer IDs shown for API use `[n/9-1-0]`
- Internal web server / proxy serves local, cloud and offline files to renderers `[n/2-0-0]`
- Wear OS streaming proxied (and transcoded) through the phone `[docs/wear-os-application]`

## 13. Android Auto / Wear / TV / widgets

- Android Auto: configurable tabs, home/library/favourites rows, display styles, genre navigation target, letter splits, file browsing, favourites tab, years/decade mixes, resume lists, playlist play/shuffle/resume submenu, search order, buffering state `[docs/settings-android-auto]` `[n/1500]` `[n/1330]`
- Wear OS companion: browse, stream via phone, download to watch for phone-free playback, tile, rotary volume, quality settings, watch-speaker mode `[docs/wear-os-application]`
- Android TV build: overscan setting, Plex link auth, separate F-Droid repo `[n/1235]` `[n/1300]`
- Widgets: standard, resizable (Material You), circle, shortcut widget (mixes, playlist, queues); per-widget opacity, theme override, tint, hide logo, track number, skip-next secondary action, no image margins `[n/2-0-0]` `[n/3-1-0]` `[n/3-2-0]` `[strings]`
- Launcher app shortcuts and pinned playlist shortcuts `[n/4-3-0]`
- Android 13 themed icon, per-app language, predictive back `[n/1-5-0]` `[n/2-0-0]`
- Alternative launcher icons `[n/2-0-0]`

## 14. UI customisation

- Application Styles (whole-UI presets, import/export incl. custom home icon) and Now Playing Styles; online catalogue styles.symfonium.app `[n/1200]` `[n/1500]`
- Built-in styles: Modern, Universal, Floating, Classic navigation, Single page, Fruit Music, Green Music, Adventurous, Basic old school; tablet presets `[strings]`
- Theme modes light/dark/black/system/system-black; Material You; seed colour with palette style, contrast, 2021/2025 colour spec; full custom theme editor with JSON import/export `[docs/settings-interface-theme-font-and-colors]`
- Dynamic colours from now-playing art (dominant/vibrant/themed) for the whole app and per detail page; instant colour updates; ignore surfaces `[n/1000]` `[n/1400]`
- Typography presets, any Google Font by name, all-caps fonts `[n/1300]` `[n/1150]`
- Navigation styles: bottom bar, compact tabs, tabs, docked toolbar, floating toolbar (+FAB), left rail, drawer; label modes; separate portrait/landscape; right-handed flip; transparency; entries reorderable, now playing as nav entry `[strings]` `[n/1400]` `[n/1370]`
- Now playing (expanded) builder with preview, portrait and landscape: row order, cover style (classic, circle, hidden, fill), rounding, rotation, shadow, no crop, animated artwork (WebP/GIF), backgrounds (blur, album art, gradient, solid, animated glow), overlays, up to N template strings with icons and click/long-click actions, button bars with sizes/spacers, rating bar styles, volume bar, queue/lyrics side panels `[n/1200]` `[n/1400]` `[docs/settings-now-playing-expanded-player]`
- Progress bar styles: basic, wave (Android 13), waveform, waveform bars with reflection; scale factors; inline times; remaining time toggle; tap position for time picker `[n/1100]` `[n/1400]` `[n/5-5-0]`
- Cover gestures: tap, double tap, long press, horizontal swipe, swipe up - each assignable (seek, lyrics, queue, go to album/artist, track info ...) `[n/1210]` `[n/1220]`
- String template language: placeholders, conditionals, &&/||, ranges, formatting markup, localized labels, bool toggles, next.* fields, format/renderer/sleep-timer/volume fields `[docs/symfonium-custom-string-template-v12]` `[n/1400]`
- Compact player: styles, floating, left-handed, swipe to skip, round progress, configurable buttons (incl. seek +/-, favourite, quick rate, chapters), thumbnail rotation `[n/1300]` `[n/1200]`
- Detail page header builder (album/artist/genre/playlist) with preview, compact header, fanart preference, hidden buttons `[n/1400]` `[n/1410]`
- Rounded corner size, hide status bar, ignore camera cutout, orientation lock, keep screen on modes, tooltips `[n/1100]` `[n/1300]`
- Top bar toggles: hide filter / cast / home / back buttons, search button `[docs/settings-interface-advanced-settings]`
- Custom local thumbnails for playlists, genres, artists, albums, tracks (file picker, photo picker, URL, generated from tracks) `[n/5-5-0]` `[n/1310]`
- Crowd-sourced translations: 20 locales besides English in the APK (da, ja, de, nl, pl, sl, ko, ro, fr, tr, cs, es, et, it, pt, pt-BR, hu, ru, zh-CN, zh-TW) `[strings]`

## 15. Scrobbling / ratings / favourites

- Play count, last played and scrobble reporting to the server (Subsonic scrobble, Plex/Emby/Jellyfin progress), incl. offline plays with correct timestamps `[n/1-6-0]` `[n/5-6-0]`
- OpenSubsonic playbackReport (OpenSubsonic) `[n/1500]`
- No built-in Last.fm/ListenBrainz scrobbler found in any source; only hooks for external scrobbler apps via the media session ("Scrobble as advertisement", track-number-in-session warning "Might break external scrobblers") `[n/1000]` `[strings]`
- Favourites for tracks, albums, artists, playlists, radios, files; favourite date tracked `[n/1200]`
- 5-star user ratings with optional half stars for tracks, albums, artists; rating from now playing, lists, swipe, notification; haptics `[n/1100]` `[n/1160]` `[n/1150]`
- Offline rating changes queued `[n/5-3-0]`
- Tag ratings imported (POPM, RATING, rate, MusicBee Love) optionally as user ratings (file providers) `[n/5-1-0]` `[n/1400]`
- "Exclude from personal mixes" flag `[n/1230]`
- Reset playback history / skip history per track `[n/7-1-0]` `[strings]`

## 16. Metadata / tags

- Multi-value artists, album artists, composers, genres with configurable separators (file providers; OpenSubsonic) `[n/1000]`
- TagLib-based parser covering ID3v2, Vorbis, APE, ASF, MP4 mappings for 60+ fields incl. classical (work, movement), sort names, MBIDs, disc subtitle, label, country, explicit, language, custom TAGS (file providers) `[docs/symfonium-custom-tag-parser]`
- Album split rules: folder, MBID, album artist, year, composer, album version; ignore MBIDs option (file providers) `[n/5-2-0]` `[n/1500]`
- Release date vs original release date, "Prefer Year tag" `[n/1-6-0]` `[n/1200]`
- Album types / release types (normalised, clickable) `[n/1140]`
- Artist info folder: artist.nfo, thumb, fanart, animated art (file providers) `[docs/artist-information-folder]`
- Online artist scraping (images, biography; language preference incl. Spanish) (file providers) `[n/1-7-0]` `[n/1010]`
- External cover files (folder/cover/album .jpg/.png) (file providers) `[n/9-1-0]`
- Generate missing artist/genre images from track art `[n/1220]`
- Prefer track art over album art (Kodi, local, Subsonic option) `[n/1-6-0]` `[n/1500]`
- Ignore articles for sort names; ASCII sort `[docs/settings-database]`
- Remove empty albums/artists/genres after sync `[docs/settings-database]`
- ICY metadata and art for radios `[n/9-0-0]`
- No tag editing (explicitly stated) `[play]`

## 17. Sleep timer / automation / intents

- Sleep timer: duration, "Finish last track", "Stop casting", remembers last value, shown in now playing `[n/5-6-0]` `[n/1210]`
- Broadcast intent API (Tasker etc.): select renderer, force sync, play/pause/stop/next/previous/shuffle/repeat/mute/seek/volume, sleep timer, start playlist/artist/album/genre/track/mix/radio with shuffle/resume/queue, change transcode bitrates and offline filter, import playlists, load queue, load EQ profile, load style/profile, start backup, cleanup cache, regenerate thumbnails, switch provider connection `[docs/symfonium-api-allow-control-from-other-apps-like-tasker]` `[n/1500]`
- Internet radios: manual URL or Subsonic / Jellyfin Live TV import, mobile URL, thumbnail URL, Shoutcast/HLS/DASH, PLS/M3U resolution, favourites, all stations queued for skipping `[n/5-9-0]` `[n/1200]` `[n/1500]`

## 18. Backup / restore

- Encrypted (password) zip backup of settings, providers, EQ profiles, radios, playlists, smart filters, auto offline rules, user data (ratings, play counts, history), custom images, file favourites, tag cache `[n/1160]` `[docs/settings-backup-restore]`
- Backup to a chosen folder; restore from first-run screen; scheduled via API `[n/1130]` `[n/3-2-0]`
- Restore defaults per settings page `[n/1-3-0]`

## 19. Accessibility / misc

- Settings PIN code `[n/5-3-0]`
- Debug mode, secure log upload, generated-files manager `[n/7-1-0]`
- Anonymous analytics and crash reporting opt-out `[docs/settings-advanced]`
- Database tools: compact, clear media-info cache, cleanup internal states `[docs/settings-advanced]`
- Battery-optimisation hint card / dontkillmyapp link `[n/1140]`
- Help buttons on settings pages linking to docs `[n/5-2-0]`
- Paid app with trial; web licence without Google services; tip programme; official F-Droid repos `[strings]` `[docs/f-droid-repository]`
- Changelog on update `[strings]`

---

## Not accessed / limits

- Forum (support.symfonium.app): only category list and the 15.0.1 / 15.1.0-beta changelog topics were read; the ~270 beta changelog topics, 2200 feature-request threads and wiki mirror were not crawled (stable notes on symfonium.app cover the same content).
- styles.symfonium.app, translation.symfonium.app and purchase.symfonium.app were not opened.
- docs pages read in text form only (screenshots not viewed); `theme-colors-documentation`, `privacy-policy`, licence FAQ and the five cloud-provider setup pages were skimmed, not itemised.
- APK: string resources only; no code, layouts or non-English strings examined. The beta APK was not dumped.
- Version 1.0/1.1 release notes do not exist on the site (oldest post is 1.2.0).
