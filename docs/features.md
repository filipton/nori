# Feature checklist

Living inventory for **nori 0.5.0**. Every distinct feature found in Symfonium (S), Musly (M) and
Navic (N), merged. The raw, sourced inventories are in `docs/research/` (Symfonium: all 67 release
posts, the docs site, the Play listing and the APK's strings; Musly: changelog, 892 l10n keys,
source at v2.0.2; Navic: releases alpha19-55, 836 commits, source). Provider-specific items that
cannot apply to a Subsonic server (Plex auth, Jellyfin Quick Connect, ...) are left in the raw files.

nori column: `yes` have it, `part` partly. Plan column: **add** = clean win, will be built;
**ask** = costs battery/CPU, a dependency, privacy or a lot of scope, the owner decides;
**skip** = see reason. "cost" notes say what a feature costs *while music plays with the screen off*,
because that is the budget this app protects.

## Decisions (2026-09-17)

The owner answered the **ask** rows:

- Sources: Subsonic only. No Jellyfin/Emby, local files or yt-dlp.
- Audio engine: bundled FFmpeg decoder, USB exclusive driver + DSD output, resampler / fixed output
  rate (+ compressor), smart fades + waveform bar: **all yes, each behind a switch that is off by default.**
- Network: UPnP/DLNA, Chromecast, third-party lookups (LRCLIB, AutoEQ database, update check), mTLS:
  **all yes, each can be disabled.**
- Extras: on-device taste model + mixes + Wrapped: yes. Word-by-word lyrics that scroll smoothly: yes,
  explicitly wanted. Wear OS / Android TV, audiobook mode, Bluetooth lyrics: no.

The rule that follows: **an optional subsystem that is switched off costs nothing** - it is not
initialised, holds no listener, opens no socket and adds no audio processor. Settings has one
"Features" page listing them all.

Build order: 1 connection and servers, 2 library and browsing, 3 queue / playlists / smart playlists /
mixes / taste model, 4 playback behaviour, 5 downloads and storage, 6 lyrics, 7 DSP extras and
per-output profiles, 8 casting, 9 FFmpeg + waveform + smart fades, 10 USB exclusive + DSD (needs the
owner's DAC), 11 backup, automation API, shortcuts, widgets, Auto nodes, logs, Wrapped.

## Servers and connection

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Subsonic token auth | x | x | x | yes | |
| Legacy (plaintext/enc) auth for old servers, auto-detected | x | x | | yes | done |
| OpenSubsonic API-key auth | x | | | yes | done |
| Stable salt so URLs stay cacheable | | x | | yes | |
| Custom HTTP headers (reverse proxy, Cloudflare Access) | x | | x | yes | done |
| Basic-auth for reverse proxies | x | | | yes | done |
| Accept self-signed certificate / custom CA / mTLS client cert | x | x | x(user CA) | yes | done |
| Two addresses per server (LAN first, WAN fallback), bitrate cap on the second | x | x | | yes | done |
| "Wi-Fi only" per server | x | | | yes | done |
| Multiple saved servers / profiles, switcher | x | x | | yes | done |
| Music-folder (library) selection | x | x | | yes | done |
| URL help: prepend https, http/https chips, reverse-proxy subpath | x | | x | yes | done |
| Server type/version shown, octo-fiesta detected | | x | | part | add |
| Categorised login errors, retry, "open offline" | | x | | yes | done |
| Zstandard / gzip response compression | x | | | gzip (OkHttp) | skip: server side does not offer zstd |
| Other sources: Jellyfin/Emby, Plex, Kodi, Audiobookshelf | x | x(J/E) | | | ask |
| Local files on the device | x | x | | | ask |
| SMB / WebDAV / cloud drives | x | | | | skip: different product |
| YouTube Music through yt-dlp (embedded Python) | | x | | | ask (leaning no: +60 MB, legal grey) |

## Sync and offline index

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Whole library mirrored locally | x | x | x | optional "Sync all" | keep optional: live search is the point |
| Automatic / scheduled re-sync, differential | x | 6 h | 1 h | | add: on app open when older than N hours, Wi-Fi only option; never from the background |
| Sync status screen with counters | x | | x | part | add |
| Compatibility mode for servers without empty-query search3 | x | | | | add (walk getAlbumList2) |
| Fetch extra metadata (artist bios/images) during sync | x | | | on demand | skip: on demand is cheaper |

## Library and browsing

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Albums, artists, playlists, genres, favourites, radio, downloads | x | x | x | yes | |
| All-songs list with sorts | x | x | x | yes | done |
| Browse by year / decade | x | | | yes | done |
| Browse by folder (getIndexes / getMusicDirectory) | x | | | yes | done |
| Album sorts: name, artist, added, played, most played, starred | x | x | x | yes | |
| More sorts: year, random, release date; asc/desc | x | | x | yes | done |
| Sort + view remembered per list | x | | x | part | part: sort |
| Grid / list toggle, grid size | x | x | x | | add (UI-light) |
| Filters: starred only, downloaded only, quick text filter | x | x | x | part | part: artists, songs, album/playlist tracks |
| A-Z fast scroller | x | x | x | part | part: artists |
| Tracks grouped by disc, disc subtitles | x | | x | yes | done |
| Album: "more by artist", quality badge, in-album filter | x | x | x | yes | done |
| Artist: albums grouped album/EP/single, "appears on", top songs, similar, bio | x | x | x | part | part: no appears-on |
| Artist: play all, shuffle, queue artist, download all albums | x | x | x | yes | done |
| last.fm / MusicBrainz links (behind a confirmation) | | | x | yes | done |
| Multiple artists per track with artist picker | x | x | x | yes | done |
| Track info sheet (path, codec, rate, bits, channels, ReplayGain, MBID) | x | | x | yes | done |
| Fullscreen cover with save / share | | | x | part | part: view only |
| Swipe a row: queue / play next / favourite (configurable) | x | x | x | yes | done |
| Default tap action configurable (play list / play one / queue) | x | | | yes | done |
| Multi-select with batch actions | x | x | | yes | done |
| Drag and drop onto play/queue targets | x | | | | skip: UI-heavy, no function gained |
| Playing row highlighted | x | x | x | yes | |
| Explicit badge; explicit content: allow / skip | x | | x | yes | done |
| Listening history screen | x | x | | yes | done |
| Home: fixed shelves | | | x | yes | |
| Home: configurable rows / order / pinned playlists | x | | | yes | done |
| Tablet two-pane layout, landscape layouts | x | x | x | | skip for now: UI pass belongs to the UI rewrite |
| Open audio files sent from other apps | x | | | | skip: no local-file source |

## Search

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Live server search merged with instant offline index | | live opt. | local+server | yes | |
| Provider (octo-fiesta) results marked, never auto-queued | | detect only | badge | yes | |
| Recent searches | 15 | x | 10 | 20 | |
| Filter chips (all / songs / albums / artists / playlists), top result | x | x | x | | add |
| Playlists in results | x | x | x | | add |
| Accent-insensitive; transliteration | x | | | accent yes | skip transliteration (ICU tables, niche) |
| Favourites-only search; search inside queue / album / playlist | x | x | | | add |
| Play from search starts a similar-songs queue | | x | | | ask via setting "default tap action" (off for ext- items, always) |
| Voice search (Assistant, Android Auto) | x | x | | yes | |

## Playback

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Gapless | x | x | x | yes | |
| Hardware offload | x | | x | yes | |
| Burst playback from a deep buffer (CPU asleep ~80 %) | | | | yes | |
| Crossfade with real overlap | x | fade-out only | | yes | |
| Crossfade: separate in/out length, curves, "mix only", off for albums in order | x | | | part | part: off inside albums |
| A scrub into the mix stays on the song to hear the ending, and the mix still fires | | | | yes | |
| AutoMix "Better beat detection": a neural beat tracker (Beat This!, MIT) reads each song's first and last half minute | | | | yes | done in the core: off by default, runs in the measurer on the song playing and the next; in every build (`neural-beats`, release included; `-PrustFeatures=` leaves it out); the weights are fetched from the authors' server and converted once on the device, none are shipped (docs/research/analysis.md) |
| Smart fades (waveform-analysed fade points) | x | | | | ask (cost: decode-ahead analysis per track) |
| Fade on play / pause / seek / skip | x | x | | yes | done |
| Speed | x | x | x | yes | |
| Pitch control / preserve pitch | x | stub | | yes | done |
| Skip silence; only for audiobooks | x | | | yes | |
| ReplayGain track / album | x | x | x | yes | |
| ReplayGain automatic (album when queue is one album), fallback gain for untagged, clipping guard | x | x | x | yes | done |
| ReplayGain with positive gain (needs DSP path) | x | | x | yes | done: "Turn quiet songs up" (0 to +12 dB, off by default: attenuation only as before); a song turned up is read as floats and turned up before the mix, the limiter behind it, and keeps off the audio chip (`nori_player::gain::offload_allows`); songs turned down stay a volume and keep offload |
| Loudness normalisation to LUFS target | x | | | yes | done: −14, −16, −18 (ReplayGain's own, default), −23 LUFS; R128 tags reach nori as ReplayGain through the server (Navidrome converts R128_* with +5 dB); untagged songs use AutoMix's measured loudness when there is one |
| Repeat, shuffle, shuffle order restored, previous follows history | x | x | x | part | add history-aware previous |
| Weighted shuffle (spread artists/albums) | x | | | always | done: every shuffle of more than two songs spreads artists and albums apart; no longer a switch |
| Queue + position survive process death | x | x | x | yes | |
| Server play-queue sync (other devices) | bookmarks | | | yes | |
| Bookmarks / resume points for long tracks | x | | | | add |
| Audiobook mode: chapters, rollback, mark played | x | | | | ask (scope) |
| Play / skip counts, thresholds | x | | x | yes | done (local history) |
| Audio focus: duck / pause / ignore; resume after call | x | x | | default | add |
| Auto-play on headset/Bluetooth connect; pause at volume 0 | x | | | | add (broadcast-driven, no polling) |
| Headset single/double/triple click mapping, long-press = next album | x | | | | add |
| "Previous" rewinds first toggle; seek step sizes | x | | | part | part: previous rule |
| Pre-cache next N tracks, separate Wi-Fi / mobile | x | x | | yes | done |
| Bitrate by network; metered Wi-Fi/VPN counts as mobile; re-transcode when Wi-Fi drops | x | x | x | part | add |
| Prefer original over cached transcode on Wi-Fi | x | | | | add |
| Keep skipping on server errors / faster skip offline | x | | | yes | done |
| Buffered position in seek bar | x | | | | add |
| Waveform seek bar | x | | | | ask (cost: decode whole track once, or server call) |
| Notification: shuffle / repeat / favourite / custom buttons | x | | x | | add |
| Media session options (queue exposure, explicit marks) | x | | | default | skip: niche |
| Sleep timer: minutes, end of track | x | x | x | yes | |
| Sleep timer: after N songs, end of queue, fade out, custom duration | x | x | x | after 2, 3, 5 or 10 songs | add end of queue, fade out, custom duration |
| Internet radio playback, ICY now-playing titles | x | x | x | yes | done |
| Formats Android cannot decode (DSD, APE, WavPack, WMA, MPC, TTA) via bundled FFmpeg | x | | | | ask (cost: +5-8 MB, those formats decode on CPU) |
| CUE sheets | x | | | | skip: file providers only |

## Output and DAC

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Android 14+ bit-perfect USB | x | | | yes (untested on hardware) | |
| Hi-res float output | x | | | yes | |
| 24/32-bit integer output to the DAC | x | | | | add (custom AudioOutputProvider) - needs your DAC to verify |
| Settings per output device (speaker / wired / each BT device / each DAC): EQ, RG, offload | x | | | yes | done as sound profiles bound to outputs |
| Bypass all processing per output | x | | | bit-perfect only | add (falls out of per-output settings) |
| Max output sample rate / fixed output format / high-quality resampler | x | | | yes | done: "Highest sample rate" per output (each song's own by default, or 48/96/192 kHz, halved within the family); a polyphase Kaiser-sinc resampler (>100 dB) that runs only for a mix across rates or a device that will not take the rate |
| USB exclusive mode (own USB stack), DAC volume, warm-up delay | x | | | | ask (large; needs your DAC) |
| DSD native / DoP / PCM-to-DSD | x | | | | ask (tied to FFmpeg + USB stack) |
| Vendor DAP routes (HiBy, FiiO, iBasso, Shanling) | x | | | | skip unless you own one |
| Show current output, rate, depth in player | x | | x | output kind and name (the player's output button) | add rate and depth |

## DSP

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Parametric EQ, peaking + shelves, AutoEQ/APO import, auto pre-amp | x | | | yes | |
| More filter types: low/high pass, band pass, notch; per-channel filters | x | | | yes | done |
| Graphic EQ 5/10/15/31 bands | x | gone | system 5-band | part | part: 10 bands plus any number of parametric ones |
| Built-in presets (bass/treble/vocal, loudness) and named profiles, per-output auto-apply | x | gone | | yes | done |
| AutoEQ database browser (download headphone list) | x | | | yes | done: 8850 headphones, index kept on the device; see "AutoEQ list" below |
| Crossfeed (levels, cutoff) | x | | | yes (level) | add cutoff |
| L/R balance, mono | x | | | yes | done |
| Limiter (so boosts and positive ReplayGain cannot clip) | x | | | yes | done: look-ahead, soft knee, transparent below the ceiling |
| Compressor / expander / noise gate | x | | | | ask (niche; cost small) |
| Bass boost, virtualizer, volume boost | x | | | | add bass boost + volume boost as EQ/limiter presets; virtualizer: skip |

### AutoEQ list (2026-09-25)

The AutoEQ index (`results/INDEX.md`, 850 kB, about 110 kB gzipped on the wire) is kept on the device by
itself: "Keep the AutoEQ list" (Settings, Sound; on by default, under "Look things up online") has the core
fetch it on an unmetered network when it is missing or 30 days old. There is no timer and no listener of its
own: the app asks the core when it starts, when the playback service sees the network turn unmetered, and
when a device arrives with no curve found because the list is not there yet, so new headphones find their
curve without a trip to the list first. The core decides (`nori_devices::autoeq::index_due`) and does nothing
otherwise. The fetch is not conditional: the platform transport hands back no response headers, and
raw.githubusercontent.com's ETag cannot be worked out from the body, so an unchanged index is recognised by
its fingerprint instead and only has its time moved on (no rewrite of the table). The list's own "Download
the list" / "Refresh list" button fetches on any network.

Every measurement has "ParametricEQ.txt" and "GraphicEQ.txt" beside it. A curve is read from the parametric
preset; where that is missing or has no filter, the graphic curve is fitted once, when it is chosen, with the
same ten filters AutoEQ's presets have (a low shelf, eight peaks, a high shelf; `nori_player::eqfit`,
Levenberg-Marquardt on a log-frequency grid 20 Hz - 20 kHz, with a pre-amp that leaves no boost), so it
plays at exactly the cost of any parametric preset. Pasting a GraphicEQ.txt into the equalizer's import
works the same way. Measured against the GraphicEQ curve itself, after the overall level: Sony WH-1000XM6
(analog cable) rms 0.42 dB / max 2.4 dB, Sennheiser HD 600 0.15 / 0.54 dB, 64 Audio U12t 0.35 / 1.5 dB
(AutoEQ's own ParametricEQ.txt for the same three: 1.24 / 6.2, 1.12 / 7.3, 1.65 / 11.2 dB, since it weighs
the treble above 10 kHz down on purpose). An entry with neither file usable is remembered
(`autoeq_missing`) and left out of the list, the search and the device matching from then on.

"That preset had no filters in it" for "Sony WH-1000XM6 (analog cable)" was not a missing curve: the index
parser cut every link at its first ")", so the 2560 entries (of 8850) whose folder name has parentheses
pointed at a folder that is not there, GitHub answered 404 with a body, and that body was read as a preset.
The link now ends at the parenthesis that balances its opening one. An index kept before this is fetched
again on the next unmetered network, since it has no fetch time.
| System equalizer / external EQ session broadcast | x | | x | panel only | add session broadcast |

## Queue

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Play next / add / remove / clear / jump | x | x | x | yes | |
| Drag to reorder | x | x | x | yes | done: the queue panel's drag handle, offered while the play order is the list's own |
| Swipe to remove / play next; scroll to current | x | x | x | | add |
| Save queue as playlist; clear remaining | x | | | | add |
| Total / remaining time header | | | x | | add |
| Duplicate-in-queue warning | | x(playlist) | x | | add |
| Multiple saved queues (last 15) | x | | | | add |
| Auto-continue with similar songs | x | x | 1 random | yes | |
| Auto-continue modes: random, same genre, same artist, similar; how many | x | x | | songs or albums; similar, same artist, same genre, same era | add random and how many |
| Mixes: instant mix from track/artist, decade, genre; "exclude from mixes" | x | x | | yes | done: Discover + Discover Weekly (on-device taste, daily/weekly seed) |
| On-device taste model (plays, skips, completion, hour of day) feeding mixes | x | x | | yes | done |
| Live queue reshaping ("Smart Flow") | x | | | | skip: opaque behaviour, little demand |

## Playlists

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Create, delete, add, remove song | x | x | x | yes | |
| Rename, comment, public flag | x | | | | add |
| Reorder tracks, multi-select remove | x | x | | | add |
| Add to several playlists at once; "already in playlist" hint | x | x | x | | add |
| Remove duplicates / missing | x | | | | add |
| M3U/M3U8 import and export | x | | | yes | done |
| Pin playlist to home; launcher shortcuts (play / shuffle) | x | x | | favourite playlists | done as favourite playlists (kept on this device, a home row); launcher shortcuts: add |
| Smart playlists: rule groups (AND/OR, nested) over index fields, limit, sort, stable random | x | | | yes | done (editor: one group; nesting via the core's JSON) |
| Default smart playlists (most played, recently played, never played, ...) | x | | | yes | done |
| Composite 2x2 covers | | x | | server-made | skip: Navidrome already serves them |

## Downloads and caches

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Download song / album / playlist / whole library | x | x | x | yes | |
| Download all albums of an artist / all favourites | x | x | x | artist | done for an artist ("Download everything" on its page); all favourites: add |
| Auto-download rules (playlist, smart playlist, artist, genre, favourites) | x | | | | add (evaluated on sync, Wi-Fi only option) |
| Download quality, Wi-Fi vs mobile; per-item "original" | x | | x | one quality | add |
| Parallel downloads 1-8, Wi-Fi only, pause/resume/cancel, low-space stop | x | x | x | 1-10 (5 by default), cancel one or all | add Wi-Fi only, pause/resume, low-space stop |
| Download manager screen with per-item state, retry | x | x | x | yes | done: the downloads page (active, waiting, failed, finished; speed and time left; retry and stop) |
| Covers and lyrics saved with downloads | x | x | x | covers via image cache | add |
| Storage location incl. SD card; export to Music/Downloads | x | x | | app-external | add |
| Rolling stream cache with size cap | x | x | | yes | |
| Promote played cache to permanent | x | | | | add |
| Storage screen: sizes, clear image cache / stream cache / downloads / index / pending | x | x | x | sizes, clears stream + covers | clear downloads and the index from here |
| Image cache: Wi-Fi-only, size, cover quality setting | x | x | x | fixed | add |
| Offline mode: auto / forced / "metered counts as offline"; hide unavailable | x | x | x | implicit | add |
| Bridge with downloads while offline (park online queue, resume when back) | | | | yes | done: StoredPrefs `bridgeOffline` (off by default); network errors jump to a download still in the queue or park the rest and play smart picks from full downloads; network callback only while bridging |
| Offline write queue (stars, ratings, plays, playlist edits) | x | scrobbles | x | yes | |

## Lyrics

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Server synced + plain lyrics, tap to seek | x | x | x | yes | |
| Word-by-word (karaoke) cues from OpenSubsonic enhanced lyrics | x | stub | x | yes | done: server cues, inline LRC tags, or estimated per line; swept in the draw phase at 30 fps, only while visible; switchable |
| Translations | x | | | yes | done |
| Lyrics services for songs the server has none for, ranked, each switchable | | x | x | yes | done: sixteen services under "Look things up online" (on by default since 2026-09-25); all on under it since 2026-09-26 (six asked first: PaxSenix, BiniLyrics, Unison, KuGou, SimpMusic, LRCLIB; the rest only when those miss, score low, or have no word timing while words are preferred; the two that need a PaxSenix key stay silent until it is given); every answer scored and the best shown, the choice kept with its score; see "Lyrics services" below |
| Offset adjust; keep screen on; text size / alignment | x | x | x | yes | done (alignment: no) |
| Lyrics cached with downloads | x | x | x | | add |
| Share lyrics as image / text | | | x | | add (text); image: skip |
| Lyric line under artwork / over cover | x | x | | | skip: UI rewrite |
| Lyrics over Bluetooth (AVRCP title field) | x | gone | | | ask (cost: a metadata update per line while playing) |

### Lyrics services (2026-09-24, defaults 2026-09-25, trust score 2026-09-25)

Sixteen services, ranked out of the box from the best down (Apple Music's lyrics timed syllable by
syllable, then the others that time words, then LRCLIB and the others that time lines, then untimed
words), in one list on Settings → Lyrics → Lyrics sources: each with a switch that turns it on or off
where it stands, and moved by holding the row and dragging it (as the home page's rows are). The order
settles near ties between answers; which answer is shown is decided by its score (below).
The server's own lyrics (OpenSubsonic structured lyrics) are not in the list: they always come first,
and no service is asked when the server has timed lyrics. All sit under "Find missing lyrics online",
which sits under "Look things up online"; both are on out of the box since 2026-09-25 (an install that
stored the switch off keeps it off: new defaults reach new installs only). Under it, nine are on, in two waves: PaxSenix, BiniLyrics,
Unison and LRCLIB are asked first, together (one or two requests each); BetterLyrics, KuGou, NetEase,
LyricsPlus and SimpMusic only when the first wave found nothing or its best answer scored below 0.82 (a
lone line-timed answer, or one whose service names nothing). A song the first wave has costs four to
seven requests once, and none again (the choice is kept). The rest are off until switched on (the two PaxSenix keyed routes also need the user's own key
and are skipped quietly without one). They are asked only when the lyrics are opened, never for provider
(`ext-`) songs. Each answer the service reports a length for must be within four seconds of the song's.
Word timings are used only where the format really carries them. The defaults apply to new installs; a
ranking already stored keeps its order and switches (a service new since is put in where it ranks out of
the box).

Why each is where it is (probed from this machine on 2026-09-25 with a handful of well-known songs):

| # | Service | Default | Why |
|---|---|---|---|
| 1 | PaxSenix | on, first wave | Apple Music's syllable-timed lyrics, the best timing there is. The song is found in Apple's own catalogue first (the public iTunes Search API: title, artist and length must match), so it rarely names the wrong song; two quick requests. Unofficial: someone else serves Apple's lyrics. iTunes Search allows about 20 requests a minute, far more than one per song opened. Trust 0.95 |
| 2 | BiniLyrics | on, first wave | Apple Music's lyrics again, indexed by ISRC on a volunteer's host; found songs the others missed. One search plus the document; results are checked on title, artist and length, and name the song they found. Unofficial, one host. Trust 0.9 |
| 3 | Unison | on, first wave | Open data (ODbL) with a documented API, word-timed where listeners timed it; matches the length itself. Small catalogue so far, but a miss is one fast request. Trust 0.85 |
| 4 | BetterLyrics | on, second wave | Apple's lyrics too; without a key it answers only for songs already stored (a 401 is a miss, one request). It was caught returning another song: its answers name nothing, so a wrong one scores low on agreement with the others and is outvoted. Trust 0.9 |
| 5 | KuGou | on, second wave | Word-timed KRC, deep Chinese, Japanese and Korean catalogue; the candidate must match title, singer and length, and its name counts in the score. An unofficial desktop-player API with a shared key; two requests. Trust 0.75 |
| 6 | NetEase Cloud Music | on, second wave | Word-timed YRC and a broad catalogue; the hit must match title, artist and length and names them. An unofficial web API that refuses some countries ("-460", a failure: rested, not hammered). Trust 0.75 |
| 7 | LyricsPlus (YouLy+) | on, second wave | Aggregates Apple, Musixmatch and others; every answer's `metadata` is checked with `names_this` and scored. Six volunteer mirrors: the last good one is asked alone, the rest only when it misses, so usually one request. Trust 0.8 |
| 8 | SimpMusic | on, second wave | Community lyrics, often word-timed, keyed on the song's YouTube video (one YouTube Music search, matched on title, artist and length) and the entry's length. Two requests. Trust 0.75 |
| 9 | BetterLyrics Portato | off | QQ Music's word timing through BetterLyrics; needs a key for most songs and was caught returning the wrong song |
| 10 | PaxSenix: Musixmatch | off | Needs the user's PaxSenix key |
| 11 | LRCLIB | on, first wave | The reputable open one: volunteer-run, documented, no key, line-timed (word-timed where a `lyricsfile` is published), fast, and names the track it found. Trust 0.85 |
| 12 | PaxSenix: Spotify | off | Needs the user's PaxSenix key; line timing |
| 13 | YouTube captions | off | Scraped from YouTube's private API; often a speech recogniser's; line timing only |
| 14 | Megalobiz | off | Scraped from web pages never meant for an app |
| 15 | YouTube Music | off | Untimed; YouTube Music's private API |
| 16 | Genius | off | Untimed; a web search and page scrape; asked last, only when no service has timed lyrics |

**Choosing the best answer** (nori-lyrics `trust.rs`, `race.rs`). Every answer gets a score from 0 to 1:
the metadata match (0.2: the title, artist, album and length the service named against the song's,
normalised, a part it did not name counted as 0.7, and an answer with the same words as one that names
the song borrows its match), the timing (0.2: word 1, line 0.7, plain 0.35; without "prefer words" line
0.95), the plausibility of the times (0.15: in order, within the song, no silence over a minute, starting
in the first 60 %, the last line within 45 s of the end), agreement with the other answers (0.3: half the
words in common, `fit::agree`; alone 0.5, all agreeing towards 1), the service's trust (0.15, the table),
and a small tie-break for the user's order (up to 0.03); less for junk: lines repeated over and over, few
distinct lines, credits or placeholders left in the middle, a named title that is not the song's (0.15),
another script than every other answer (0.2), and disagreeing with every other answer while they agree
among themselves (0.2). Answers below 0.4 are never shown. The best is shown at once when it scores 0.75
or more and something backs it (it names the song, or another answer agrees), otherwise when the first
wave is in, or at the end. What is on screen is replaced only by an answer that scores more than 0.03
higher and agrees with it (or more than 0.2 higher when it does not: what was shown was another song's).
The choice is logged and put on the perf timeline, e.g. `lyrics: chose BiniLyrics (0.91, word-timed),
runner-up LRCLIB (0.84)`.

**Kept.** The lyrics shown are kept in the app's database with their source and score
(`lyrics|BEST|artist|title|length`) and served from there next time without any request. A choice that
scored under 0.7 is looked up again after three days, and replaced if something better turns up. Each
service's own answer is kept too (hits for good, misses for a week), with what it named. Settings →
Storage → Lyrics shows the size and clears all of it after asking.

**Credits stripped.** Every answer loses, at its start and end only, the credits and furniture services
put around the words: "role: name" lines in any script ("Lyrics by: …", "作词 : …", "작곡: …"), "Written
by …"-style lines whose next word is a name, watermarks and ads ("Lyrics from …", "Paroles de la chanson
…", "…Embed", "You might also like", "See … Live"), a header naming the song ("Title Lyrics", "Artist -
Title"), "[Instrumental]" placeholders, and empty or time-only lines (`credits.rs`). Done once as an answer
is read, before it is scored and kept; the lines left keep their times.

All of it is Rust (docs/clients.md): the service table and the user's ranking in nori-settings
(`lyrics_sources.rs`), the requests, matching and answers in nori-lyrics (`services.rs`, through the
core's `Transport`, which now carries a third party's headers and a JSON body), the formats with their
tests and fixtures (`formats.rs`, `json.rs`, `html.rs`, `testdata/`), the ranking and racing
(`race.rs`), the answers kept in the app's database (the core's response cache, per service and song).
Kotlin shows what the core hands it (`Library.lyricsFor`) and draws the settings rows.

**None of the requests could be tried from the machine this was written on.** The request and answer
shapes come from BitChord's code of 2026-09-20, Better Lyrics' own OpenAPI description of 2026-09-24 and
other open-source clients. `tools/feature-e2e.sh` asks each service on its own and prints what it
answered: that run on a phone is the real check.

How each is asked (the ranking and the reasons are in the table above):

| # | Service | Timing | Default | Needs | Notes |
|---|---|---|---|---|---|
| 1 | BiniLyrics | syllable (Apple Music's TTML) | on | - | Apple's lyrics from a volunteer's copy |
| 2 | BetterLyrics | syllable (Apple Music's TTML) | off | a key only for songs it has not stored | asked without a key, where a 401 counts as a miss; a key (`X-API-Key`, field in Settings) lets it fetch the rest, and a miss is asked again once one is given. Tries `api.betterlyrics.org`, then the older `lyrics-api.boidu.dev` if the first cannot be reached |
| 3 | PaxSenix | syllable (Apple Music's) | on | - | the Apple Music id comes from the public iTunes Search API, then `lyrics.paxsenix.org` is asked for it |
| 4 | LyricsPlus (YouLy+) | syllable, word or line, as its answer says | off | - | six volunteer servers: the one that answered last is asked alone, the rest together only when it has nothing |
| 5 | BetterLyrics Portato | word (QQ Music's QRC) | off | as BetterLyrics | how QQ Music is reached: Portato hands the QRC over decrypted |
| 6 | PaxSenix: Musixmatch | word where Musixmatch has it, else line | off | PaxSenix key | skipped until a key is set |
| 7 | SimpMusic | word (rich sync), line, plain | off | the song's YouTube video | the video is found with one YouTube Music search (Songs filter, matched on title, artist and length), shared with the two YouTube services and kept for the last few songs |
| 8 | Unison | word (TTML or tagged LRC), line, plain | on | - | open data (ODbL), credited as asked |
| 9 | NetEase Cloud Music | word (YRC), line | off | - | can refuse addresses outside China ("-460"), which is a failure, not a miss |
| 10 | KuGou | word (KRC) | off | - | KRC decrypted and inflated in Rust |
| 11 | LRCLIB | line; word where a `lyricsfile` is published | on | - | |
| 12 | PaxSenix: Spotify | line | off | PaxSenix key | skipped until a key is set |
| 13 | YouTube captions | line | off | the song's YouTube video | speech-recognition word offsets are not used as word timing |
| 14 | Megalobiz | line (LRC shown on its pages) | off | - | only result links that name the song are followed, the artist's first, and the last line must come before the song ends |
| 15 | YouTube Music | plain (the Lyrics tab) | off | the song's YouTube video | |
| 16 | Genius | plain | off | - | asked last, only when no service has timed lyrics |
| - | Musixmatch directly | word, line | - | - | not added: it needs Musixmatch's own private signing key, taken from their web player; use PaxSenix: Musixmatch with your own key instead |
| - | Spotify directly | word, line | - | - | not added: needs the user's Spotify login; its lyrics come through PaxSenix instead |
| - | BetterLyrics' KuGou route | line | - | - | not added: KuGou is asked directly with word timing |

How a lookup runs (nori-lyrics `race.rs`): what each service answered before is read first (a hit is
kept for good, a miss is asked again after a week; the cache is per service and song, in the app's one
database), so a song played again asks nobody. The rest are asked together, best-ranked first, six at a
time, on no thread of their own (one future, polled by the caller); each request may take 6 s (PaxSenix's
keyed routes 15 s, as its own clients allow) and each service 12 s in all (30 s for those two). A finer
answer goes on screen as soon as it arrives; a swap between two answers timed alike waits until the end.
The lookup stops, dropping (and so cancelling) whatever is still out, as soon as no service still to
answer could beat the answer in hand, and leaving the lyrics cancels it all. A failed or too slow service
is never remembered as a miss, but it is not asked about the same song again for half an hour, and a
service that fails three songs in a row rests for ten minutes, so a service that is down is not hammered.
With "Prefer word-by-word lyrics" on (the default) a word-timed answer beats a line-timed one whoever has
it, so a line-timed answer from high up is shown at once but the lookup keeps going for words; off, the
best-ranked timed answer wins as soon as everything above it has answered. Untimed words (YouTube Music,
Genius) are asked only once every service that could time something has answered without.

Translations that NetEase and KuGou carry are Chinese translations and are not used; the server's own
translations still are.

## Casting and remote

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Chromecast | x | x | | | ask (needs Google Play services Cast SDK) |
| UPnP / DLNA renderers, gapless, volume | x | x | | | ask (own SSDP+SOAP code, no dependency; discovery runs only while the picker is open) |
| Sonos groups, Kodi, Plex clients | x | | | | skip unless you own them |
| Subsonic jukebox (play on the server's sound card) | | x | | | add |
| Android output switcher integration | x | | | yes | done: the player's output button opens Android's output switcher |
| LAN remote control between two phones | | disabled | | | skip |

## Car, watch, TV, widgets

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Android Auto browse tree + voice search | x | x | session only | yes | |
| Android Auto: tabs (Home, Library, Favorites, Downloads), mixes, artists, genres, Play/Shuffle rows, a picked song plays its folder, voice by artist/album/playlist/genre/mix, long-press actions, repeat and radio buttons, covers through a provider | x | x | | done | configurable tabs: add |
| Widget 4x1 | x | | x | yes | |
| More widgets (2x2 with cover, resizable) | x | | x | | add, still event-driven |
| Launcher shortcuts (search, shuffle all, playlists) | x | | | | add |
| Wear OS app (browse, control, download to watch) | x | | | | ask |
| Android TV build (leanback, D-pad) | x | x | | | ask |
| Quick-settings tile / Assistant actions | | | | | skip |

## Ratings, scrobbling, stats

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Star songs/albums/artists | x | x | x | yes | |
| 1-5 star ratings | x | x | x | no | dropped: nobody used them, favourites cover it |
| Scrobble + now playing, threshold %, offline queue | x | x | x | yes | |
| Minimum duration to scrobble | | | x | 10 s fixed | add |
| OpenSubsonic playbackReport | x | | | | add (octo-fiesta handles it) |
| Year-in-review ("Wrapped") | | x | | yes | done as listening stats for any period |
| Direct Last.fm / ListenBrainz | | | | | skip: Navidrome does this server-side |

## Shares, radio, misc

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Create share link | x | | x | yes | |
| Share with expiry + description; list / delete shares; hide sharing | | | x | | add |
| Radio: list, play, add, delete | x | x | add only | yes | |
| Radio: edit; homepage link; copy URL | | x | | | add |
| Settings backup / restore (encrypted zip) | x | | | | add |
| Automation API (broadcast intents: play/pause/next, play playlist, load EQ profile, set bitrate, sync) | x | | | | add |
| Settings PIN | x | | | | skip |
| In-app log viewer / send logs, crash screen | x | | x | | add log viewer |
| Update checker (GitHub releases) | | x | x | | ask (a network call to GitHub on open) |
| Discord presence, donation nags, easter eggs, emulator block, analytics | | x | | | skip |
| Translations | 20 | 27 | 28 | yes | done |

## Owner's list, 2026-09-18

The owner asked for these explicitly.

- USB DAC handling like Symfonium, and earphone-model auto EQ -> AutoEQ browser done; when a Bluetooth or USB device
  connects whose name matches a measured headphone, the app offers its curve and remembers it for that device;
  integer/exclusive USB output needs the owner's DAC.
- Octo-fiesta-aware search, telling library and provider items apart, and the stale-cover bug (provider covers
  cached with their "not downloaded" badge) -> done: provider label, library/provider filter, provider covers never
  disk-cached, "add to library" (star makes octo-fiesta download it).
- Automatic synced lyrics for songs without an LRC -> done: server first, then LRCLIB (synced preferred, duration-matched,
  hits kept, misses retried weekly, failures never cached). LyricsPlus mirrors (Navic's karaoke source) were all
  down on 2026-09-18, so not shipped. Since 2026-09-24 sixteen services, word-timed where they have it, ranked,
  asked together and switchable in Settings → Lyrics → Lyrics sources (see "Lyrics services" above).
- Apple Music AutoMix-style transitions with BPM/beat matching -> **done** (on-device analysis, Camelot-aware length/filters, outro loop remix, bass swap, echo-out, LUFS match; see docs/research/automix.md). Optional neural beats ("Better beat detection", Beat This! small through tract, off by default, in builds with the `neural-beats` feature) -> **done** (docs/research/analysis.md).
- Material You, AMOLED, adjustable -> done: wallpaper colours, accent colours, theme mode, true-black mode.
- The owner's friend: "take it from Apple, the album cover spills into the page; but no forced liquid glass" -> done:
  album, artist and playlist pages open with the cover edge to edge under the status bar, melting into a colour taken
  from it; buttons use the cover's accent; the player gets the same wash. Static tint (0 % CPU idle), switchable,
  and with AMOLED on it melts into black instead.
- Gestures -> done: swipe the mini player to skip, up to open; swipe the player header or artwork down to close;
  swipe the artwork to skip.

## Player artwork

| Feature | S | M | N | nori | Plan |
|---|---|---|---|---|---|
| Moving album covers in the player (Apple Music's motion artwork) | | | | yes | done: Settings, Look; behind the lookups switch, and off by default itself though the lookups switch is on (heavier, a few megabytes each); Wi-Fi only by default; see below |
| Spotify Canvas (a looping clip per song) | | | | | skip: needs the listener's Spotify login; see below |

**Moving covers.** Apple Music has a short looping video of the cover for some albums, mostly recent
ones from the larger labels, and plays it in the full-bleed sleeve. nori plays the same video in the
same place, over the still cover and cropped the same way, only while the player is open and at rest
and the screen is on, and never with reduced motion. It is found in three requests to Apple, all made by
the core (`crates/core/src/motion.rs`, through the client's transport): the public iTunes Search API
for the album's catalogue id, then the catalogue itself for that album's video, read with the token
Apple's own web player gives every visitor, which is taken from the player's scripts and looked for again
when it is refused. The artist and the album name are what leave the phone. A video is a few megabytes,
hence Wi-Fi only unless allowed, and each is kept in a 64 MB cache so that the loop is fetched once. The
answer is remembered per album in the app's database: a video for good, "none" asked again after a week,
a failed request not at all. Playing it is Android's (`MotionPlayer`, its own muted ExoPlayer with the
HLS module). **Unverified**: none of these requests could be tried where this was written, and Apple
documents none of them for this use; their shapes are what other clients were seen sending in 2026. If
Apple changes them, nothing breaks: no cover moves. Costs nothing switched off (no lookup, no player, no
listener, no surface); switched on, a hardware video decode and a redraw of the sleeve per video frame
while it plays, and nothing at all with the player closed or the screen off.

**Why Spotify Canvas is not there.** Spotify has no public API for Canvas. The clips come from an
internal endpoint of Spotify's own apps (`spclient.wg.spotify.com/canvaz-cache`, protobuf), which
answers only an access token made from a signed-in listener's session cookie (`sp_dc`). Since 2025
making that token also needs a one-time code worked out from a secret hidden in Spotify's web player,
and the clients that still manage it load the web player in a hidden WebView with the listener's cookie
and pose as Spotify's iOS app. That is asking people for their login to another service and working
round a protection Spotify put there on purpose, which this app does not do. Apple's route needs
neither: the search API is public, and the catalogue token is the one Apple's web player hands to anyone
who opens it, signed in or not.

## Look and feel

The interface is a priority alongside performance: Apple Music-like, the cover colour melting into
every page, smooth motion, nothing blocky. Symfonium's style builders, Navic's five themes and Musly's
artwork editor are reference points, not goals in themselves; options here are theme mode
(system/light/dark), dynamic colour, cover-based page and player colour, keep-screen-on for lyrics,
configurable mini-player buttons, tab order/hide.
