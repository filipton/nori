package dev.nori.music.app.ui

import android.content.res.Resources
import dev.nori.music.app.R
import dev.nori.music.ffi.library.AlbumDetail
import dev.nori.music.ffi.library.DiscGroup
import dev.nori.music.ffi.library.LibraryOffer
import dev.nori.music.ffi.library.StatsPage
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.devices.OutputPort
import dev.nori.music.ffi.library.AlbumSort
import dev.nori.music.ffi.library.DownloadAct
import dev.nori.music.ffi.library.LibrarySection
import dev.nori.music.ffi.library.MixName
import dev.nori.music.ffi.library.ReleaseGroup
import dev.nori.music.ffi.library.ReleaseKind
import dev.nori.music.ffi.library.RowSwipeAct
import dev.nori.music.ffi.library.SearchScope
import dev.nori.music.ffi.library.SleepChoice
import dev.nori.music.ffi.library.SongAction
import dev.nori.music.ffi.model.AutoEqEntry
import dev.nori.music.ffi.model.PresetKind
import dev.nori.music.ffi.model.SmartBuiltin
import dev.nori.music.ffi.settings.BandMark
import dev.nori.music.ffi.settings.EqBypass
import dev.nori.music.ffi.settings.LyricsOrigin
import dev.nori.music.ffi.settings.BandChannel
import dev.nori.music.ffi.model.EqKind
import dev.nori.music.ffi.settings.HomeRow
import dev.nori.music.text.Fmt

/**
 * Every word the screens say, from the app's string resources (res/values/strings.xml), so the app can
 * be translated. The fixed words are read once per locale into fields ([say] is rebuilt when the locale
 * changes); a word with a number or a name in it is a function. Numbers themselves are [Fmt]'s. The core
 * says none of these: it hands over data and kinds, and this words them.
 */
class Say(private val r: Resources) {
    val home: String = r.getString(R.string.home)
    val search: String = r.getString(R.string.search)
    val library: String = r.getString(R.string.library)
    val settings: String = r.getString(R.string.settings)
    val back: String = r.getString(R.string.back)
    val more: String = r.getString(R.string.more)
    val next: String = r.getString(R.string.next)
    val previous: String = r.getString(R.string.previous)
    val play: String = r.getString(R.string.play)
    val pause: String = r.getString(R.string.pause)
    val shuffle: String = r.getString(R.string.shuffle)
    val repeat: String = r.getString(R.string.repeat)
    val queue: String = r.getString(R.string.queue)
    val lyrics: String = r.getString(R.string.lyrics)
    val favourite: String = r.getString(R.string.favourite)
    val addToFavourites: String = r.getString(R.string.add_to_favourites)
    val removeFromFavourites: String = r.getString(R.string.remove_from_favourites)
    val remove: String = r.getString(R.string.remove)
    val undo: String = r.getString(R.string.undo)
    fun queueRemoved(title: String): String = r.getString(R.string.queue_removed, title)
    val cancel: String = r.getString(R.string.cancel)
    val done: String = r.getString(R.string.done)
    val close: String = r.getString(R.string.close)
    val create: String = r.getString(R.string.create)
    val delete: String = r.getString(R.string.delete)
    val edit: String = r.getString(R.string.edit)
    val save: String = r.getString(R.string.save)
    val add: String = r.getString(R.string.add)
    val copyIt: String = r.getString(R.string.copy_it)
    val clear: String = r.getString(R.string.clear)
    val reset: String = r.getString(R.string.reset)
    val open: String = r.getString(R.string.open)
    val import: String = r.getString(R.string.import_it)
    val name: String = r.getString(R.string.name)
    val sort: String = r.getString(R.string.sort)
    val filter: String = r.getString(R.string.filter)
    val get: String = r.getString(R.string.get)
    val retry: String = r.getString(R.string.retry)
    val automatic: String = r.getString(R.string.automatic)
    val chosen: String = r.getString(R.string.chosen)
    val nowPlayingBar: String = r.getString(R.string.now_playing_bar)
    val notInLibraryYet: String = r.getString(R.string.not_in_library_yet)
    val nothingMatches: String = r.getString(R.string.nothing_matches)
    val listenNow: String = r.getString(R.string.listen_now)
    val shuffleEverything: String = r.getString(R.string.shuffle_everything)
    val resumeFromServer: String = r.getString(R.string.resume_from_server)
    val rearrangeRows: String = r.getString(R.string.rearrange_rows)
    val forYou: String = r.getString(R.string.for_you)
    val rows: String = r.getString(R.string.rows)
    val holdARowToMoveIt: String = r.getString(R.string.hold_a_row_to_move_it)
    val notShown: String = r.getString(R.string.not_shown)
    val searchHint: String = r.getString(R.string.search_hint)
    val recentSearches: String = r.getString(R.string.recent_searches)
    val albums: String = r.getString(R.string.albums)
    val artists: String = r.getString(R.string.artists)
    val songs: String = r.getString(R.string.songs)
    val playlists: String = r.getString(R.string.playlists)
    val smart: String = r.getString(R.string.smart)
    val history: String = r.getString(R.string.history)
    val favourites: String = r.getString(R.string.favourites)
    val genres: String = r.getString(R.string.genres)
    val decades: String = r.getString(R.string.decades)
    val folders: String = r.getString(R.string.folders)
    val radio: String = r.getString(R.string.radio)
    val downloads: String = r.getString(R.string.downloads)
    val filterArtists: String = r.getString(R.string.filter_artists)
    val newPlaylist: String = r.getString(R.string.new_playlist)
    val importPlaylistFile: String = r.getString(R.string.import_playlist_file)
    val exportPlaylistFile: String = r.getString(R.string.export_playlist_file)
    val favouriteSongs: String = r.getString(R.string.favourite_songs)
    val starredFavourites: String = r.getString(R.string.starred_favourites)
    val newStation: String = r.getString(R.string.new_station)
    val streamUrl: String = r.getString(R.string.stream_url)
    val downloadQueue: String = r.getString(R.string.download_queue)
    val addAllToQueue: String = r.getString(R.string.add_all_to_queue)
    val downloadAll: String = r.getString(R.string.download_all)
    val downloadEverything: String = r.getString(R.string.download_everything)
    val addToQueue: String = r.getString(R.string.add_to_queue)
    val openInBrowser: String = r.getString(R.string.open_in_browser)
    val topSongs: String = r.getString(R.string.top_songs)
    val similarArtists: String = r.getString(R.string.similar_artists)
    val sleepTimer: String = r.getString(R.string.sleep_timer)
    val addToPlaylist: String = r.getString(R.string.add_to_playlist)
    val details: String = r.getString(R.string.details)
    val playlist: String = r.getString(R.string.playlist)
    val clearSelection: String = r.getString(R.string.clear_selection)
    val addedByYou: String = r.getString(R.string.added_by_you)
    val reorder: String = r.getString(R.string.reorder)
    val later: String = r.getString(R.string.later)
    val sooner: String = r.getString(R.string.sooner)
    val mixing: String = r.getString(R.string.mixing)
    val newMix: String = r.getString(R.string.new_mix)
    val newSmartPlaylist: String = r.getString(R.string.new_smart_playlist)
    val readyMade: String = r.getString(R.string.ready_made)
    val smartPlaylist: String = r.getString(R.string.smart_playlist)
    val matchAll: String = r.getString(R.string.match_all)
    val matchAny: String = r.getString(R.string.match_any)
    val removeRule: String = r.getString(R.string.remove_rule)
    val addRule: String = r.getString(R.string.add_rule)
    val sortBy: String = r.getString(R.string.sort_by)
    val descending: String = r.getString(R.string.descending)
    val limit: String = r.getString(R.string.limit)
    val noLimit: String = r.getString(R.string.no_limit)
    val listeningStats: String = r.getString(R.string.listening_stats)
    val clearHistory: String = r.getString(R.string.clear_history)
    val listening: String = r.getString(R.string.listening)
    val whenYouListen: String = r.getString(R.string.when_you_listen)
    val topArtists: String = r.getString(R.string.top_artists)
    val topAlbums: String = r.getString(R.string.top_albums)
    val topGenres: String = r.getString(R.string.top_genres)
    val downloaded: String = r.getString(R.string.downloaded)
    val downloadFailed: String = r.getString(R.string.download_failed)
    val stopAll: String = r.getString(R.string.stop_all)
    val downloading: String = r.getString(R.string.downloading)
    val waiting: String = r.getString(R.string.waiting)
    val failed: String = r.getString(R.string.failed)
    val retryAll: String = r.getString(R.string.retry_all)
    val finished: String = r.getString(R.string.finished)
    val stopDownload: String = r.getString(R.string.stop_download)
    val findingLyrics: String = r.getString(R.string.finding_lyrics)
    val analysing: String = r.getString(R.string.analysing)
    val equalizer: String = r.getString(R.string.equalizer)
    val eqHint: String = r.getString(R.string.eq_hint)
    val addBand: String = r.getString(R.string.add_band)
    val eqGraphic: String = r.getString(R.string.eq_graphic)
    val eqParametric: String = r.getString(R.string.eq_parametric)
    val eqGraphicHint: String = r.getString(R.string.eq_graphic_hint)
    fun eqBandCount(n: Int): String = r.getString(R.string.eq_band_count, n)
    /**
     * How closely the graphic equalizer follows a headphone correction, by its largest difference; past
     * 1.5 dB it says what follows it more closely (more bands, or the parametric equalizer).
     */
    fun eqFollows(maxDb: Float, bands: Int): String {
        val db = Fmt.fixed(maxDb.toDouble(), 1)
        return when {
            maxDb <= 1.5f -> r.getString(R.string.eq_follows, db)
            bands < 31 -> r.getString(R.string.eq_follows_loose, db)
            else -> r.getString(R.string.eq_follows_loose_31, db)
        }
    }
    val pastePreset: String = r.getString(R.string.paste_preset)
    val presets: String = r.getString(R.string.presets)
    val autoPreampHint: String = r.getString(R.string.auto_preamp_hint)
    val output: String = r.getString(R.string.output)
    val balance: String = r.getString(R.string.balance)
    val mono: String = r.getString(R.string.mono)
    val monoDetail: String = r.getString(R.string.mono_detail)
    val limiter: String = r.getString(R.string.limiter)
    val limiterDetail: String = r.getString(R.string.limiter_detail)
    val profiles: String = r.getString(R.string.profiles)
    val saveTheseSettings: String = r.getString(R.string.save_these_settings)
    val saveAsProfile: String = r.getString(R.string.save_as_profile)
    val crossfeed: String = r.getString(R.string.crossfeed)
    val crossfeedOff: String = r.getString(R.string.crossfeed_off)
    val crossfeedDefault: String = r.getString(R.string.crossfeed_default)
    val crossfeedChuMoy: String = r.getString(R.string.crossfeed_chu_moy)
    val crossfeedJanMeier: String = r.getString(R.string.crossfeed_jan_meier)
    val importPreset: String = r.getString(R.string.import_preset)
    val importPresetHint: String = r.getString(R.string.import_preset_hint)
    val importPresetExample: String = r.getString(R.string.import_preset_example)
    val noFiltersFound: String = r.getString(R.string.no_filters_found)
    val frequency: String = r.getString(R.string.frequency)
    val removeBand: String = r.getString(R.string.remove_band)
    val headphonePresets: String = r.getString(R.string.headphone_presets)
    val autoeqAbout: String = r.getString(R.string.autoeq_about)
    val downloadTheList: String = r.getString(R.string.download_the_list)
    val refreshList: String = r.getString(R.string.refresh_list)
    val autoeqNoCurve: String = r.getString(R.string.autoeq_no_curve)
    val autoeqCredit: String = r.getString(R.string.autoeq_credit)
    val autoeqCreditGraphic: String = r.getString(R.string.autoeq_credit_graphic)
    val devices: String = r.getString(R.string.devices)
    val autoeqAuto: String = r.getString(R.string.autoeq_auto)
    val autoeqAutoDetail: String = r.getString(R.string.autoeq_auto_detail)
    val devicesNote: String = r.getString(R.string.devices_note)
    val flat: String = r.getString(R.string.flat)
    val flatDetail: String = r.getString(R.string.flat_detail)
    val noProcessing: String = r.getString(R.string.no_processing)
    val noProcessingDetail: String = r.getString(R.string.no_processing_detail)
    val leaveAsIs: String = r.getString(R.string.leave_as_is)
    val leaveAsIsDetail: String = r.getString(R.string.leave_as_is_detail)
    val savedProfile: String = r.getString(R.string.saved_profile)
    val autoeqCurves: String = r.getString(R.string.autoeq_curves)
    val autoeqDownloadHint: String = r.getString(R.string.autoeq_download_hint)
    val forgetDevice: String = r.getString(R.string.forget_device)
    val searchSettings: String = r.getString(R.string.search_settings)
    val performance: String = r.getString(R.string.performance)
    val performanceDetail: String = r.getString(R.string.performance_detail)
    val server: String = r.getString(R.string.server)
    val serverHint: String = r.getString(R.string.server_hint)
    val serverUrl: String = r.getString(R.string.server_url)
    val user: String = r.getString(R.string.user)
    val password: String = r.getString(R.string.password)
    val connecting: String = r.getString(R.string.connecting)
    val connect: String = r.getString(R.string.connect)
    val hideAdvanced: String = r.getString(R.string.hide_advanced)
    val advanced: String = r.getString(R.string.advanced)
    val nameOptional: String = r.getString(R.string.name_optional)
    val secondAddress: String = r.getString(R.string.second_address)
    val secondAddressHint: String = r.getString(R.string.second_address_hint)
    val apiKey: String = r.getString(R.string.api_key)
    val extraHeaders: String = r.getString(R.string.extra_headers)
    val extraHeadersHint: String = r.getString(R.string.extra_headers_hint)
    val legacyAuth: String = r.getString(R.string.legacy_auth)
    val legacyAuthHint: String = r.getString(R.string.legacy_auth_hint)
    val selfSigned: String = r.getString(R.string.self_signed)
    val selfSignedHint: String = r.getString(R.string.self_signed_hint)
    val wifiOnly: String = r.getString(R.string.wifi_only)
    val wifiOnlyHint: String = r.getString(R.string.wifi_only_hint)
    val clientCertPassword: String = r.getString(R.string.client_cert_password)
    val importClientCert: String = r.getString(R.string.import_client_cert)
    val replaceClientCert: String = r.getString(R.string.replace_client_cert)
    val underTheHood: String = r.getString(R.string.under_the_hood)
    val playback: String = r.getString(R.string.playback)
    val libraryAndSearch: String = r.getString(R.string.library_and_search)
    val automix: String = r.getString(R.string.automix)
    val interfaceTitle: String = r.getString(R.string.interface_title)
    val openSource: String = r.getString(R.string.open_source)
    val freeSoftware: String = r.getString(R.string.free_software)
    val licenceLine: String = r.getString(R.string.licence_line)
    val licences: String = r.getString(R.string.licences)
    val licencesDetail: String = r.getString(R.string.licences_detail)
    val rustCore: String = r.getString(R.string.rust_core)
    val fontsAndData: String = r.getString(R.string.fonts_and_data)
    val noLicenceText: String = r.getString(R.string.no_licence_text)
    val licenceUnreadable: String = r.getString(R.string.licence_unreadable)

    // ---- counts: kept per number, since list rows ask for them while they scroll ----

    private val songsMade = arrayOfNulls<String>(COUNTS)
    private val albumsMade = arrayOfNulls<String>(COUNTS)

    private fun counted(made: Array<String?>, id: Int, n: Int): String {
        if (n !in 0 until COUNTS) return r.getQuantityString(id, n, n)
        return made[n] ?: r.getQuantityString(id, n, n).also { made[n] = it }
    }

    /** "1 song", "12 songs". */
    fun songs(n: Int): String = counted(songsMade, R.plurals.songs, n)
    /** "12 songs" whatever the count: how the album and playlist pages have always put it. */
    fun songsAlwaysPlural(n: Int): String = r.getString(R.string.songs_always_plural, n)
    /** "1 album", "12 albums": under an artist's name. */
    fun albums(n: Int): String = counted(albumsMade, R.plurals.albums, n)
    fun releases(n: Int): String = r.getQuantityString(R.plurals.releases, n, n)
    fun selected(n: Int): String = r.getString(R.string.selected_count, n)
    /** The count alone, where "12 selected" does not fit. */
    fun selectedShort(n: Int): String = r.getString(R.string.selected_count_short, n)

    /** A folder's caption: "2 folders · 14 songs". */
    fun folderCaption(folders: Int, songs: Int): String = r.getQuantityString(R.plurals.folders, folders, folders) + " · " + songs(songs)
    /** A folder's title: its name, or "Folder" for one the server did not name. */
    fun folderTitle(name: String): String = name.ifEmpty { r.getString(R.string.folder_untitled) }
    /** A folder's row in a folder: a folder mark, then its name. */
    fun folderRow(name: String): String = "📁  $name"
    /** A decade by its first year: "1990s". */
    fun decade(start: Int): String = r.getString(R.string.decade_name, start)

    /** The download queue's row in the library: "4 to go · 1 failed", or that nothing is downloading. */
    fun downloadQueue(waiting: Int, failed: Int): String {
        val toGo = if (waiting > 0) r.getString(R.string.download_queue_to_go, waiting) else null
        val lost = if (failed > 0) r.getString(dev.nori.music.core.R.string.downloads_failed, failed) else null
        return listOfNotNull(toGo, lost).joinToString(" · ").ifEmpty { r.getString(dev.nori.music.core.R.string.downloads_nothing) }
    }

    // ---- captions ----

    /** A list's caption: "12 songs · 48:10", every count plural when [alwaysPlural]. */
    fun listCaption(count: Int, seconds: Long, alwaysPlural: Boolean): String =
        (if (alwaysPlural) songsAlwaysPlural(count) else songs(count)) + " · " + Fmt.duration(seconds)

    /** What is known of an album before its songs are: "2019 · 12 songs · 48:10", each part only if known. */
    fun albumHintCaption(year: Int, songCount: Int, seconds: Long): String = listOfNotNull(
        year.takeIf { it > 0 }?.toString(),
        songCount.takeIf { it > 0 }?.let(::songsAlwaysPlural),
        seconds.takeIf { it > 0 }?.let(Fmt::duration),
    ).joinToString(" · ")

    /** An album page's caption once its songs are in: "2019 · 12 songs · 48:10 · FLAC 16/44.1 · explicit". */
    fun albumCaption(d: AlbumDetail): String = listOfNotNull(
        d.album.year.toInt().takeIf { it > 0 }?.toString(),
        songsAlwaysPlural(d.songs.size),
        Fmt.duration(d.seconds.toLong()),
        d.songs.firstOrNull()?.let(::quality),
        if (d.album.explicitStatus == "explicit") r.getString(R.string.caption_explicit) else null,
    ).joinToString(" · ")

    /** The format of a record, from its first song: "FLAC 24/96.0", "MP3 320 kbps". */
    fun quality(s: Song): String? {
        val lossless = s.suffix.lowercase() in LOSSLESS
        val name = s.suffix.uppercase()
        val detail = if (lossless && s.bitDepth > 0u) "${s.bitDepth}/${Fmt.khz(s.samplingRate.toInt())}"
        else if (s.bitRate > 0u) "${s.bitRate} kbps" else null
        return listOfNotNull(name.ifEmpty { null }, detail).takeIf { it.isNotEmpty() }?.joinToString(" ")
    }

    /** A song's rate, sample rate, depth and (with [channels]) channels as the details say them, each only if known. */
    private fun qualityParts(s: Song, channels: Boolean): List<String> = listOfNotNull(
        if (s.bitRate > 0u) "${s.bitRate} kbps" else null,
        if (s.samplingRate > 0u) "${Fmt.khz(s.samplingRate.toInt())} kHz" else null,
        if (s.bitDepth > 0u) "${s.bitDepth} bit" else null,
        if (channels && s.channelCount > 0u) "${s.channelCount} ch" else null,
    )

    /** The line under a song at the top of its menu: "FLAC · 1411 kbps · 44.1 kHz · 16 bit". */
    fun songFormat(s: Song): String = (listOf(s.suffix.uppercase()) + qualityParts(s, false)).joinToString(" · ")

    /** A song's second line where it is shown on its own (its menu's head): "Artist · Album". */
    fun songLine(artist: String, album: String): String = if (album.isEmpty()) artist else "$artist · $album"

    /** Everything the server said about one file, in the order the details sheet shows it, blanks left out. */
    fun trackInfo(s: Song): List<Pair<String, String>> {
        val artists = s.artists.joinToString(", ") { it.name }
        val gain = s.replayGain?.let { g ->
            listOfNotNull(
                g.trackGain?.let { r.getString(R.string.info_track_gain, Fmt.fixed(it.toDouble(), 2, true)) },
                g.albumGain?.let { r.getString(R.string.info_album_gain, Fmt.fixed(it.toDouble(), 2, true)) },
                g.trackPeak?.let { r.getString(R.string.info_peak, Fmt.fixed(it.toDouble(), 3)) },
            ).joinToString(" · ")
        }
        val rows = listOf(
            R.string.info_title to s.title,
            R.string.info_artist to artists.ifEmpty { s.artist },
            R.string.info_album to s.album,
            R.string.info_track to listOfNotNull(
                if (s.discNumber > 0u) r.getString(R.string.info_disc_number, s.discNumber.toInt()) else null,
                if (s.track > 0u) r.getString(R.string.info_track_number, s.track.toInt()) else null,
            ).joinToString(", "),
            R.string.info_year to (if (s.year > 0u) s.year.toString() else null),
            R.string.info_genre to s.genre,
            R.string.info_duration to Fmt.duration(s.duration.toLong()),
            R.string.info_format to listOfNotNull(s.suffix.ifEmpty { null }?.uppercase(), s.contentType.ifEmpty { null }).joinToString(" · "),
            R.string.info_quality to qualityParts(s, true).joinToString(" · "),
            R.string.info_size to (if (s.size > 0u) Fmt.megabytes(s.size.toLong()) else null),
            R.string.info_replay_gain to gain,
            R.string.info_bpm to (if (s.bpm > 0u) s.bpm.toString() else null),
            R.string.info_plays to (if (s.playCount > 0u) s.playCount.toString() else null),
            R.string.info_last_played to s.played?.take(16)?.replace('T', ' '),
            R.string.info_added to s.created?.take(10),
            R.string.info_path to s.path,
            R.string.info_musicbrainz to s.musicBrainzId,
            R.string.info_comment to s.comment,
            R.string.info_id to s.id,
        )
        return rows.mapNotNull { (label, v) -> v?.takeIf { it.isNotBlank() }?.let { r.getString(label) to it } }
    }

    /** An album's disc heading, "Disc 2 · Bonus"; empty for the one disc of an album that has only one. */
    fun discHeading(d: DiscGroup): String = when {
        !d.headed -> ""
        d.title.isEmpty() -> r.getString(R.string.disc_heading, d.disc.toInt())
        else -> r.getString(R.string.disc_heading_titled, d.disc.toInt(), d.title)
    }

    /** The offer on a provider's page: "Add the whole album to the library". */
    fun libraryOffer(o: LibraryOffer): String = r.getString(if (o.playlist) R.string.library_offer_playlist else R.string.library_offer_album)

    // ---- the player ----

    /** What the player's title says with no song: the station playing, or that nothing is. */
    fun playerIdle(radio: String?): String = radio ?: r.getString(R.string.nothing_playing)
    val bridging: String get() = r.getString(R.string.bridging)
    val upNext: String get() = r.getString(R.string.up_next)
    /** The now playing bar's second line: what went wrong, else the artist, else (a station) "Radio". */
    fun barLine(error: String?, artist: String?): String = error ?: artist ?: radio
    fun playingThrough(output: String): String = r.getString(R.string.said_playing_through, output)

    /** The sleep timer under the seek bar: "Sleep · end of track", or the minutes left rounded up, never fewer than one. */
    fun sleep(endOfTrack: Boolean, leftMs: Long): String =
        if (endOfTrack) r.getString(R.string.sleep_end_of_track)
        else r.getString(R.string.sleep_minutes, ((leftMs + 59_999) / 60_000).coerceAtLeast(1).toInt())

    // ---- lyrics ----

    /** A lyrics source's name as the credit gives it: "your server", "LRCLIB". */
    fun lyricsOrigin(o: LyricsOrigin): String = when (o) {
        LyricsOrigin.SERVER -> r.getString(R.string.lyrics_your_server)
        LyricsOrigin.BINILYRICS -> "BiniLyrics"
        LyricsOrigin.BETTER_LYRICS, LyricsOrigin.PORTATO -> "BetterLyrics"
        LyricsOrigin.PAXSENIX, LyricsOrigin.PAXSENIX_MUSIXMATCH, LyricsOrigin.PAXSENIX_SPOTIFY -> "PaxSenix"
        LyricsOrigin.LYRICS_PLUS -> "LyricsPlus"
        LyricsOrigin.SIMPMUSIC -> "SimpMusic"
        LyricsOrigin.UNISON -> "Unison"
        LyricsOrigin.NETEASE -> "NetEase"
        LyricsOrigin.KUGOU -> "KuGou"
        LyricsOrigin.LRCLIB -> "LRCLIB"
        LyricsOrigin.YOUTUBE_CAPTIONS -> "YouTube"
        LyricsOrigin.MEGALOBIZ -> "Megalobiz"
        LyricsOrigin.YOUTUBE_MUSIC -> "YouTube Music"
        LyricsOrigin.GENIUS -> "Genius"
    }

    /**
     * The corner under the lyrics, closed: whose words these are when they are not the server's, and when
     * they came without timings, that they did. The server's timed words say "Timing" (the corner opens
     * the nudge buttons); the server's untimed words have no corner at all (null).
     */
    fun lyricsCredit(o: LyricsOrigin, synced: Boolean): String? {
        val source = if (o != LyricsOrigin.SERVER) lyricsOrigin(o) else null
        if (source == null && !synced) return null
        val first = source ?: r.getString(R.string.lyrics_timing)
        return if (synced) first else first + " · " + r.getString(R.string.lyrics_not_timed)
    }

    /** How far the lyrics are nudged: "+0.5 s". */
    fun nudge(ms: Long): String = r.getString(R.string.lyrics_nudge, Fmt.nudge(ms))

    // ---- the core's kinds, named: the fixed words read once per locale, so a row or a frame allocates nothing ----

    val menuPlayNext: String = r.getString(R.string.menu_play_next)
    private val menuAddToPlaylist: String = r.getString(R.string.menu_add_to_playlist)
    private val menuRemoveDownload: String = r.getString(R.string.menu_remove_download)
    val menuDownload: String = r.getString(R.string.menu_download)
    private val menuGoToAlbum: String = r.getString(R.string.menu_go_to_album)
    private val menuGoToArtist: String = r.getString(R.string.menu_go_to_artist)
    private val menuSleepTimer: String = r.getString(R.string.menu_sleep_timer)
    private val menuStartRadio: String = r.getString(R.string.menu_start_radio)
    private val menuInstantMix: String = r.getString(R.string.menu_instant_mix)
    private val menuExclude: String = r.getString(R.string.menu_exclude_from_mixes)
    private val menuShare: String = r.getString(R.string.menu_share_link)

    /** A line of the song menu, by what it does (`menus::song_menu`). */
    fun songAction(a: SongAction): String = when (a) {
        is SongAction.Favourite -> if (a.on) addToFavourites else removeFromFavourites
        SongAction.PlayNext -> menuPlayNext
        SongAction.AddToQueue -> addToQueue
        SongAction.AddToPlaylist -> menuAddToPlaylist
        SongAction.RemoveDownload -> menuRemoveDownload
        SongAction.StopDownload -> stopDownload
        SongAction.Download -> menuDownload
        is SongAction.GoToAlbum -> menuGoToAlbum
        is SongAction.GoToArtist -> if (a.named) r.getString(R.string.menu_go_to_named, a.name) else menuGoToArtist
        SongAction.AddToLibrary -> r.getString(R.string.menu_add_to_library)
        SongAction.SleepTimer -> menuSleepTimer
        SongAction.StartRadio -> menuStartRadio
        SongAction.InstantMix -> menuInstantMix
        SongAction.ExcludeFromMixes -> menuExclude
        SongAction.Share -> menuShare
        SongAction.Details -> details
    }

    /** One of the sleep timer's choices: "30 minutes", "End of track", "After 3 songs", or "Off" (all zeros). */
    fun sleepChoice(c: SleepChoice): String = when {
        c.endOfTrack -> r.getString(R.string.sleep_choice_end_of_track)
        c.songs > 0u -> r.getString(R.string.sleep_choice_after_songs, c.songs.toInt())
        c.minutes > 0u -> r.getString(R.string.sleep_choice_minutes, c.minutes.toInt())
        else -> r.getString(R.string.sleep_choice_off)
    }

    /** What letting go of a swiped row does, under the row: one of the words read above, nothing made. */
    fun rowSwipe(a: RowSwipeAct): String = when (a) {
        RowSwipeAct.Queue -> addToQueue
        RowSwipeAct.PlayNext -> menuPlayNext
        is RowSwipeAct.Favourite -> if (a.on) favourite else remove
        RowSwipeAct.Download -> menuDownload
    }

    /** A page's download entry: "Download", "Remove downloads", or "Download the other 3" for the [missing]. */
    fun downloadEntry(act: DownloadAct, missing: Int): String = when (act) {
        DownloadAct.ALL -> menuDownload
        DownloadAct.REMOVE -> r.getString(R.string.entry_remove_downloads)
        DownloadAct.MISSING -> r.getString(R.string.entry_download_other, missing)
    }

    /** A "For you" tile's or page's name. */
    fun mixName(n: MixName): String = when (n) {
        MixName.FAVOURITES -> favourites
        MixName.QUICK_PICKS -> r.getString(R.string.mix_quick_picks)
        MixName.DISCOVER -> r.getString(R.string.mix_discover)
        MixName.DISCOVER_WEEKLY -> r.getString(R.string.mix_discover_weekly)
        MixName.LISTEN_AGAIN -> r.getString(R.string.mix_listen_again)
        MixName.TOP -> r.getString(R.string.mix_top)
    }

    fun mixUnknown(id: String): String = r.getString(R.string.mix_unknown, id)

    /** A ready-made smart playlist's name. */
    fun smartBuiltin(b: SmartBuiltin): String = r.getString(
        when (b) {
            SmartBuiltin.MOST_PLAYED -> R.string.smart_most_played
            SmartBuiltin.RECENTLY_PLAYED -> R.string.smart_recently_played
            SmartBuiltin.RECENTLY_ADDED -> R.string.smart_recently_added
            SmartBuiltin.NEVER_PLAYED -> R.string.smart_never_played
            SmartBuiltin.TOP_RATED -> R.string.smart_top_rated
            SmartBuiltin.FORGOTTEN_FAVOURITES -> R.string.smart_forgotten_favourites
            SmartBuiltin.LONG_TRACKS -> R.string.smart_long_tracks
        },
    )

    /** A smart playlist's name: the one the user gave it, a ready-made one's, or "Smart playlist" for one left blank. */
    fun smartName(p: dev.nori.music.ffi.model.SmartPlaylist): String = p.builtin?.let(::smartBuiltin) ?: p.name.ifEmpty { smartPlaylist }

    /** An artist's shelf heading: "Albums", "EPs", or a type the app does not know by its tag ("Mixtapes"). */
    fun releaseShelf(g: ReleaseGroup): String = when (g.kind) {
        ReleaseKind.ALBUM -> albums
        ReleaseKind.EP -> r.getString(R.string.shelf_eps)
        ReleaseKind.SINGLE -> r.getString(R.string.shelf_singles)
        ReleaseKind.LIVE -> r.getString(R.string.shelf_live)
        ReleaseKind.COMPILATION -> r.getString(R.string.shelf_compilations)
        ReleaseKind.SOUNDTRACK -> r.getString(R.string.shelf_soundtracks)
        ReleaseKind.REMIX -> r.getString(R.string.shelf_remixes)
        ReleaseKind.OTHER -> r.getString(R.string.shelf_other)
        ReleaseKind.TAGGED -> if (g.tag.endsWith('s')) g.tag else r.getString(R.string.shelf_tagged, g.tag)
    }

    /** One of the album grid's orders. */
    fun albumSort(s: AlbumSort): String = when (s) {
        AlbumSort.BY_NAME -> r.getString(R.string.sort_az)
        AlbumSort.BY_ARTIST -> r.getString(R.string.sort_artist)
        AlbumSort.NEWEST -> r.getString(R.string.sort_added)
        AlbumSort.RECENT -> r.getString(R.string.sort_played)
        AlbumSort.FREQUENT -> r.getString(R.string.sort_most_played)
        AlbumSort.STARRED -> favourites
        AlbumSort.BY_YEAR -> r.getString(R.string.sort_year)
        AlbumSort.RANDOM -> r.getString(R.string.sort_random)
        AlbumSort.BY_GENRE -> genres
    }

    /** One of the songs list's orders, by the name the core keeps it under (`browse::song_sorts`). */
    fun songSort(name: String): String = when (name) {
        "TITLE" -> r.getString(R.string.sort_title)
        "ARTIST" -> r.getString(R.string.sort_artist)
        "ALBUM" -> r.getString(R.string.sort_album)
        "YEAR" -> r.getString(R.string.sort_year)
        "ADDED" -> r.getString(R.string.sort_added)
        "PLAYS" -> r.getString(R.string.sort_most_played)
        "LONGEST" -> r.getString(R.string.sort_longest)
        else -> name
    }

    /** A pill of the library. */
    fun librarySection(s: LibrarySection): String = when (s) {
        LibrarySection.ALBUMS -> albums
        LibrarySection.FAVOURITES -> favourites
        LibrarySection.ARTISTS -> artists
        LibrarySection.SONGS -> songs
        LibrarySection.PLAYLISTS -> playlists
        LibrarySection.SMART -> smart
        LibrarySection.HISTORY -> history
        LibrarySection.GENRES -> genres
        LibrarySection.DECADES -> decades
        LibrarySection.FOLDERS -> folders
        LibrarySection.RADIO -> radio
        LibrarySection.DOWNLOADS -> downloads
    }

    /** A search scope's chip. */
    fun searchScope(s: SearchScope): String = when (s) {
        SearchScope.EVERYTHING -> r.getString(R.string.scope_everything)
        SearchScope.LIBRARY -> r.getString(R.string.scope_in_library)
        SearchScope.PROVIDERS -> notInLibraryYet
    }

    /** The server search failed and the offline answer stays; [reason] is what the failure said. */
    fun searchFallback(reason: String?): String = r.getString(R.string.search_fallback, reason ?: "null")

    private val homeRows: Array<String> = HomeRow.entries.map { row ->
        when (row) {
            HomeRow.PINNED -> r.getString(R.string.home_row_pinned)
            HomeRow.PLAYLISTS -> playlists
            HomeRow.RECENT -> r.getString(R.string.home_row_recent)
            HomeRow.NEWEST -> r.getString(R.string.home_row_newest)
            HomeRow.FREQUENT -> r.getString(R.string.home_row_frequent)
            HomeRow.TOP_SONGS -> r.getString(R.string.home_row_top_songs)
            HomeRow.RANDOM -> r.getString(R.string.home_row_random)
            HomeRow.STARRED -> r.getString(R.string.home_row_starred)
        }
    }.toTypedArray()

    /** A home shelf's title: read once per locale, so a shelf composed again allocates nothing. */
    fun homeRow(row: HomeRow): String = homeRows[row.ordinal]

    private val bandKinds: Array<String> = arrayOf(
        R.string.band_peak, R.string.band_low_shelf, R.string.band_high_shelf, R.string.band_low_pass, R.string.band_high_pass,
        R.string.band_band_pass, R.string.band_notch, R.string.band_all_pass, R.string.band_low_shelf_slope, R.string.band_high_shelf_slope,
    ).map(r::getString).toTypedArray()
    private val bandChannels: Array<String> = arrayOf(R.string.channel_both, R.string.channel_left, R.string.channel_right).map(r::getString).toTypedArray()

    /** A kind of band as the band editor names it: read once, so the list's rows allocate nothing. */
    fun bandKind(k: EqKind): String = bandKinds[k.ordinal]
    fun bandChannel(c: BandChannel): String = bandChannels[c.ordinal]

    /** A built-in equalizer curve's name. */
    fun preset(k: PresetKind): String = when (k) {
        PresetKind.FLAT -> flat
        PresetKind.BASS_BOOST -> r.getString(R.string.preset_bass_boost)
        PresetKind.BASS_CUT -> r.getString(R.string.preset_bass_cut)
        PresetKind.TREBLE_BOOST -> r.getString(R.string.preset_treble_boost)
        PresetKind.TREBLE_CUT -> r.getString(R.string.preset_treble_cut)
        PresetKind.VOCAL_BOOST -> r.getString(R.string.preset_vocal_boost)
        PresetKind.LOUDNESS -> r.getString(R.string.preset_loudness)
        PresetKind.SMALL_SPEAKERS -> r.getString(R.string.preset_small_speakers)
    }

    /** Why nothing on the equalizer screen reaches the sound. */
    fun eqBypass(b: EqBypass): String = r.getString(
        when (b) {
            EqBypass.BIT_PERFECT -> R.string.eq_bypass_bit_perfect
            EqBypass.HI_RES -> R.string.eq_bypass_hi_res
            EqBypass.OUTPUT -> R.string.eq_bypass_output
        },
    )

    /** Under a saved profile: which devices use it, or that choosing it loads it. */
    fun profileUse(devices: List<String>): String =
        if (devices.isEmpty()) r.getString(R.string.profile_choose_to_load) else r.getString(R.string.profile_used_for, devices.joinToString(", "))

    // ---- output devices: a key's parts (`outputs::parts`) in words ----

    private val outputSpeaker: String = r.getString(R.string.output_speaker)
    private val outputWired: String = r.getString(R.string.output_wired)
    private val outputUsb: String = r.getString(R.string.output_usb)
    private val outputBluetooth: String = r.getString(R.string.output_bluetooth)

    /** Where a device is plugged in, when that is worth saying: "USB", "Bluetooth". */
    fun outputKind(port: OutputPort): String? = when (port) {
        OutputPort.USB -> outputUsb
        OutputPort.BLUETOOTH -> outputBluetooth
        else -> null
    }

    /** A device's name: its own, or the app's for one without ("Phone speaker", "DAC"). */
    fun outputName(port: OutputPort, name: String?): String = when (port) {
        OutputPort.SPEAKER -> outputSpeaker
        OutputPort.WIRED -> outputWired
        OutputPort.USB -> name ?: r.getString(R.string.output_usb_nameless)
        OutputPort.BLUETOOTH -> name ?: r.getString(R.string.output_bluetooth_nameless)
        OutputPort.OTHER -> name ?: r.getString(R.string.output_other)
    }

    /** A device with where it is plugged in: "USB: K3", "Bluetooth: buds", "Phone speaker". */
    fun outputLabel(port: OutputPort, name: String?): String =
        outputKind(port)?.let { r.getString(R.string.output_named, it, outputName(port, name)) } ?: outputName(port, name)

    /** What the player's output button says to a screen reader. */
    fun outputDescription(port: OutputPort, name: String?): String = r.getString(R.string.output_description, outputLabel(port, name))

    /** The mark on the device playing now, after its kind when it has one. */
    fun devicePlayingNow(afterKind: Boolean): String = r.getString(if (afterKind) R.string.device_playing_now_after else R.string.device_playing_now)

    /** The words under a device's name in its sheet. */
    fun deviceIntro(kind: String?): String =
        r.getString(R.string.device_sheet_intro, if (kind == null) r.getString(R.string.device_this) else r.getString(R.string.device_this_kind, kind))

    /** The line under "Automatic": whether a known curve is used or offered. */
    fun deviceAutomatic(autoApply: Boolean): String = r.getString(if (autoApply) R.string.device_auto_uses else R.string.device_auto_offers)

    /** The snackbar about the device that just connected: its words and its one action. */
    fun deviceNotice(offer: Boolean, curve: String): Pair<String, String> =
        if (offer) r.getString(R.string.device_notice_offer, curve) to r.getString(R.string.device_notice_apply)
        else r.getString(R.string.device_notice_applied, curve) to r.getString(R.string.device_notice_undo)

    /** The size of the AutoEQ list, and the search field's hint. */
    fun autoeqCount(n: Int): String = r.getString(R.string.autoeq_count, n)
    fun autoeqSearch(n: Int): String = r.getString(R.string.autoeq_search_count, n)

    /** An AutoEQ curve's line in the browser: who measured it, the form and the target, whichever are known. */
    fun autoeqCaption(e: AutoEqEntry): String = listOf(e.source, e.form, e.target).filter { it.isNotEmpty() }.joinToString(" · ")
    /** Its line in a device's sheet: who measured it and the form. */
    fun autoeqShort(e: AutoEqEntry): String = "${e.source} · ${e.form}"

    // ---- confirmations ----

    fun addedToPlaylist(name: String): String = r.getString(R.string.said_added_to_playlist, name)
    fun playlistCreated(name: String): String = r.getString(R.string.said_playlist_created, name)
    val playingNext: String get() = r.getString(R.string.said_playing_next)
    val addedToQueue: String get() = r.getString(R.string.said_added_to_queue)
    val excludedFromMixes: String get() = r.getString(R.string.said_excluded_from_mixes)
    val noServerQueue: String get() = r.getString(R.string.said_no_server_queue)
    val serverDownloading: String get() = r.getString(R.string.said_server_downloading)
    val saidFailed: String get() = r.getString(R.string.said_failed)
    fun favourite(on: Boolean): String = r.getString(if (on) R.string.said_favourite_added else R.string.said_favourite_removed)
    fun downloadingSongs(n: Int): String = r.getQuantityString(R.plurals.said_downloading, n, n)
    fun downloadsRemoved(n: Int): String = r.getQuantityString(R.plurals.said_downloads_removed, n, n)
    /** After an M3U import: how many of its entries were found and went into [playlist]. */
    fun m3uImported(found: Int, entries: Int, playlist: String): String =
        if (found == 0) r.getString(R.string.m3u_none_found, entries) else r.getString(R.string.m3u_imported, found, entries, playlist)
    fun autoeqApplied(name: String): String = r.getString(R.string.autoeq_applied, name)
    fun deleteNamed(name: String): String = r.getString(R.string.delete_named, name)

    /** Stopping every download: its title, and what it does to [unfinished] songs. */
    val stopAllTitle: String get() = r.getString(R.string.stop_all_title)
    fun stopAllText(unfinished: Int): String = r.getQuantityString(R.plurals.stop_all_text, unfinished, unfinished)

    fun note(n: Note): String = r.getString(n.id)

    // ---- the equalizer ----

    /** "Pre-amp -3.5 dB (automatic)". */
    fun preamp(db: Float, automatic: Boolean): String =
        r.getString(if (automatic) R.string.eq_preamp_automatic else R.string.eq_preamp, Fmt.signedDb(db))
    /** "centre", "L 30%", "R 5%". */
    fun balance(balance: Float): String = when {
        balance == 0f -> r.getString(R.string.eq_balance_centre)
        else -> r.getString(if (balance < 0f) R.string.eq_balance_left else R.string.eq_balance_right, Fmt.fixed((kotlin.math.abs(balance) * 100f).toDouble(), 0))
    }
    /** "Ceiling -1.0 dB". */
    fun ceiling(db: Float): String = r.getString(R.string.eq_ceiling, Fmt.fixed(db.toDouble(), 1))
    /** What the limiter is pulling back right now, or that it is not: "−2.3 dB", "not clipping". */
    /** What the compressor is taking off right now, or that it is resting: "Compressing 3.2 dB". */
    fun compression(db: Float): String =
        if (db > 0.05f) r.getString(R.string.eq_compressing, Fmt.fixed(db.toDouble(), 1)) else r.getString(R.string.eq_not_compressing)
    fun reduction(db: Float): String =
        if (db > 0.05f) r.getString(R.string.eq_reduction, Fmt.fixed(db.toDouble(), 1)) else r.getString(R.string.eq_not_clipping)
    fun crossfeed(db: Float, custom: Boolean = false): String = when {
        db <= 0f -> r.getString(R.string.eq_off)
        custom -> r.getString(R.string.eq_crossfeed_custom, Fmt.fixed(db.toDouble(), 1))
        else -> r.getString(R.string.eq_crossfeed_on, Fmt.fixed(db.toDouble(), 1))
    }
    /** "Cutoff 700 Hz: how high up the other ear hears". */
    fun crossfeedCut(hz: Float): String = r.getString(R.string.eq_crossfeed_cut, Fmt.hz(hz))
    /** A graphic band's ISO label: "31.5", "63", "1k", "12.5k". */
    fun isoBand(hz: Float): String = if (hz < 100f && hz != kotlin.math.floor(hz)) Fmt.fixed(hz.toDouble(), 1) else Fmt.hz(hz)
    /** The band dialog's title: "63 Hz", "1k Hz". */
    fun hzTitle(freq: Float): String = r.getString(R.string.eq_hz_title, Fmt.hz(freq))
    /** "Slope 0.71" for a shelf given by its slope, "Q 1.41" for the rest. */
    fun shape(slope: Boolean, q: Float): String = r.getString(if (slope) R.string.eq_slope else R.string.eq_q, Fmt.fixed(q.toDouble(), 2))
    /** A band's label: its frequency and a mark for its channel or kind (the core's `band_mark`). */
    fun band(freq: Float, mark: BandMark): String {
        val hz = Fmt.hz(freq)
        return when (mark) {
            BandMark.NONE -> hz
            BandMark.LEFT -> r.getString(R.string.eq_band_left, hz)
            BandMark.RIGHT -> r.getString(R.string.eq_band_right, hz)
            BandMark.LOW_SHELF -> "$hz ↙"
            BandMark.HIGH_SHELF -> "$hz ↗"
            BandMark.NO_GAIN -> "$hz ∿"
        }
    }

    // ---- listening stats ----

    /** The periods the stats can cover, in the order the chips run: days back from now (0 for all time) and a name. */
    val statsPeriods: List<Pair<Int, String>> get() = listOf(
        7 to r.getString(R.string.stats_week), 30 to r.getString(R.string.stats_month),
        365 to r.getString(R.string.stats_year), 0 to r.getString(R.string.stats_all_time),
    )
    /** Under the play count: "plays · 12:34:56 listened". */
    fun statsHeadline(listenedMs: Long): String = r.getString(R.string.stats_headline, Fmt.duration(listenedMs / 1000))
    /** "Most around 21:00, mostly on Fridays"; null (and no chart) when nothing was played. */
    fun statsHabit(p: StatsPage): String? {
        val hour = p.busiestHour?.toInt() ?: return null
        val day = p.busiestWeekday?.toInt() ?: return r.getString(R.string.stats_habit, hour)
        return r.getString(R.string.stats_habit_day, hour, r.getStringArray(R.array.stats_weekdays)[day])
    }
    /** The six tiles under the headline, in two rows of three: a number and what it counts. */
    fun statsTiles(p: StatsPage): List<Pair<String, String>> = with(p.stats) {
        listOf(
            distinctSongs.toString() to r.getString(R.string.stat_songs),
            distinctArtists.toString() to r.getString(R.string.stat_artists),
            distinctAlbums.toString() to r.getString(R.string.stat_albums),
            skips.toString() to r.getString(R.string.stat_skips),
            activeDays.toString() to r.getString(R.string.stat_active_days),
            longestStreakDays.toString() to r.getString(R.string.stat_day_streak),
        )
    }

    // ---- about ----

    /** What About says this build is made of, one line each, and all of it together for a bug report. */
    class AboutFacts(val title: String, val build: String, val playback: String, val library: String, val automix: String, val ui: String, val report: String)

    /**
     * About's facts for the app [version], from [versions] as the build writes them ("media3=1.8.0;
     * okhttp=5.1.0;...", an empty value for one it could not read), a [debug] build or not, its commit
     * [sha] (blank outside a checkout), the first [abi] the phone runs and its Android [release] and [sdk].
     */
    fun about(version: String, versions: String, debug: Boolean, sha: String, abi: String?, release: String, sdk: Int): AboutFacts {
        val known = versions.split(';').mapNotNull { p -> p.indexOf('=').takeIf { it >= 0 }?.let { p.substring(0, it) to p.substring(it + 1) } }
            .filter { (_, v) -> '=' !in v && v.isNotBlank() }
        fun v(name: String) = known.firstOrNull { it.first == name }?.let { " ${it.second}" }.orEmpty()
        val build = r.getString(
            R.string.about_build, r.getString(if (debug) R.string.about_debug else R.string.about_release),
            sha.ifBlank { r.getString(R.string.about_no_commit) }, abi ?: r.getString(R.string.about_unknown_abi), release, sdk,
        )
        val playback = r.getString(R.string.about_playback, v("media3"), v("okhttp"))
        val library = r.getString(R.string.about_library, v("rusqlite"), v("uniffi"))
        val automix = r.getString(R.string.about_automix, v("rustfft"), v("signalsmith-stretch"))
        val ui = r.getString(R.string.about_ui, v("composeBom"))
        val title = r.getString(R.string.about_title, version)
        return AboutFacts(title, build, playback, library, automix, ui, r.getString(R.string.about_report, title, build, playback, library, automix, ui))
    }


    companion object {
        /** Counts up to this are made once and kept. */
        private const val COUNTS = 512
        private val LOSSLESS = setOf("flac", "alac", "wav", "aiff", "ape", "wv", "dsf", "dff")

        @Volatile private var res: Resources? = null
        @Volatile private var made: Say? = null

        /** The app's resources, once at start and again when the locale changes: the words are read anew. */
        fun use(r: Resources) {
            res = r
            made = null
        }

        /** The words in the app's locale now. */
        val current: Say get() = made ?: Say(checkNotNull(res) { "Say.use was not called" }).also { made = it }
    }
}

/** The empty-list, failure and help lines around the app, by where they are shown. */
enum class Note(val id: Int) {
    /** No index yet: the songs and decades lists. */
    NO_INDEX(R.string.note_no_index),
    NO_PLAYLISTS(R.string.note_no_playlists),
    NO_STATIONS(R.string.note_no_stations),
    NOTHING_DOWNLOADED(R.string.note_nothing_downloaded),
    NO_DOWNLOADS(R.string.note_no_downloads),
    NO_DOWNLOADS_HELP(R.string.note_no_downloads_help),
    DOWNLOAD_FAILED(R.string.note_download_failed),
    NO_FAVOURITE_SONGS(R.string.note_no_favourite_songs),
    NOTHING_TO_MIX(R.string.note_nothing_to_mix),
    SMART_HELP(R.string.note_smart_help),
    NO_HISTORY(R.string.note_no_history),
    NOTHING_FOUND(R.string.note_nothing_found),
    NO_LYRICS(R.string.note_no_lyrics),
    /** Over the reason, where a page could not be read. */
    COULD_NOT_LOAD(R.string.note_could_not_load),
    COULD_NOT_EVALUATE(R.string.note_could_not_evaluate),
}
