package dev.nori.music.app.vm

import android.app.Application
import androidx.lifecycle.viewModelScope
import dev.nori.music.data.StarKind
import dev.nori.music.downloads.DownloadState
import dev.nori.music.downloads.DownloadMark
import dev.nori.music.ffi.transfers.DownloadSections
import dev.nori.music.net.said
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.stateIn
import dev.nori.music.ffi.model.Album
import dev.nori.music.ffi.model.OriginKind
import dev.nori.music.ffi.model.PageOrigin
import dev.nori.music.ffi.model.Playlist
import dev.nori.music.ffi.model.Song
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import dev.nori.music.ffi.settings.SwipeAction
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import dev.nori.music.ffi.queue.ShufflePlan
import dev.nori.music.ffi.queue.TapPlan
import dev.nori.music.ffi.queue.shufflePlan
import dev.nori.music.ffi.queue.tapPlan
import dev.nori.music.app.ui.say

/** Everything that can be done to a song, album or playlist from any screen. One instance per activity. */
class ActionsViewModel(app: Application) : NoriViewModel(app) {
    private val _messages = Channel<String>(Channel.CONFLATED)
    /** One-line confirmations and failures, for a snackbar or whatever the UI uses. */
    val messages = _messages.receiveAsFlow()
    /** This session's star changes, so every heart on screen can prefer them over its snapshot. */
    val starMarks: StateFlow<dev.nori.music.ffi.library.StarMarks> = nori.library.starMarks
    val downloads: StateFlow<DownloadState> = nori.downloads.state

    // A process started in the background could not restart the download service; with a screen up it can.
    init { nori.downloads.resume() }

    internal fun attempt(done: String?, block: suspend () -> Unit) = viewModelScope.launch {
        try { block(); done?.let { _messages.send(it) } } catch (e: Exception) { _messages.send(e.said ?: say.saidFailed) }
    }

    // ---- selection mode: long-press a song anywhere, then act on the whole selection ----

    private val picked = Selection<Song> { it.id }
    val selection: StateFlow<List<Song>> = picked.items
    fun toggleSelected(song: Song) = picked.toggle(song)
    fun clearSelection() = picked.clear()
    /** Back while songs are selected lets go of them and goes no further (see [Selection.back]). */
    fun backFromSelection(): Boolean = picked.back()
    /** The page on screen is now [key] (a back stack entry): a selection does not outlive its page. */
    fun onPage(key: String) = picked.onPage(key)

    /**
     * What a plain tap on row [index] of [songs] does: the core's answer from the settings (`tap_plan`).
     * [from] is the page whose list [songs] is: playing the list from the row is that page's queue, and
     * its Play reads Pause; one song on its own is no page's.
     */
    fun tap(songs: List<Song>, index: Int, from: PageOrigin? = null) {
        when (tapPlan(picked.items.value.isNotEmpty())) {
            TapPlan.SELECT -> toggleSelected(songs[index])
            TapPlan.PLAY_LIST -> nori.player.play(songs, index, from = from)
            TapPlan.PLAY_ONE -> nori.player.play(listOf(songs[index]))
            TapPlan.QUEUE -> enqueue(listOf(songs[index]))
            TapPlan.PLAY_NEXT -> playNext(listOf(songs[index]))
        }
    }

    /** What swiping a song row right and left does, as set in Settings. */
    val swipes: Pair<SwipeAction, SwipeAction> get() = nori.settings.value.let { it.swipeRight to it.swipeLeft }

    // By id: the core has the artist's albums from reading the page, so they need not be handed back.
    fun playArtist(artistId: String, shuffle: Boolean = false) = attempt(null) {
        nori.player.play(nori.library.artistSongs(artistId), shuffle = shuffle, from = PageOrigin(OriginKind.ARTIST, artistId))
    }
    fun queueArtist(artistId: String) = attempt(null) { enqueue(nori.library.artistSongs(artistId)) }
    fun downloadArtist(artistId: String) = attempt(null) { download(nori.library.artistSongs(artistId)) }

    /** [songs] from [index]; [from] the page they are the list of, if they are one page's (see [tap]). */
    fun play(songs: List<Song>, index: Int = 0, from: PageOrigin? = null) = nori.player.play(songs, index, from = from)

    /** Spreads artists and albums apart (in the core) unless the user prefers a plain random order. */
    fun shuffle(songs: List<Song>, from: PageOrigin? = null) {
        when (val plan = shufflePlan(songs)) {
            ShufflePlan.Empty -> {}
            ShufflePlan.PlayerShuffle -> nori.player.play(songs, shuffle = true, from = from)
            is ShufflePlan.Order -> nori.player.playShuffledOrder(songs, plan.order, from)
        }
    }

    fun instantMix(song: Song) = attempt(null) { nori.player.play(nori.library.instantMix(song)) }

    fun excludeFromMixes(song: Song) = attempt(say.excludedFromMixes) { nori.library.excludeFromMixes(song.id, true) }

    fun exportM3u(name: String, songs: List<Song>): String = nori.library.m3uExport(name, songs)

    /** Creates a server playlist from an M3U file; tracks that are not in the index are reported, not guessed. */
    fun importM3u(name: String, text: String) = attempt(null) {
        val imported = withContext(Dispatchers.IO) { nori.core.m3uImport(text) }
        if (imported.songIds.isNotEmpty()) nori.library.createPlaylist(name, imported.songIds)
        _messages.send(say.m3uImported(imported.songIds.size, imported.entries.toInt(), name))
    }
    /** [songs] added by hand; [from] the page they are all the songs of (an album's: added whole, kept gapless). */
    fun playNext(songs: List<Song>, from: PageOrigin? = null) { nori.player.playNext(songs, from); _messages.trySend(say.playingNext) }
    fun enqueue(songs: List<Song>, from: PageOrigin? = null) { nori.player.enqueue(songs, from); _messages.trySend(say.addedToQueue) }

    // Each shuffle marks its queue, so its refills go on the same way whatever the autoplay setting says.
    fun shuffleAll() = attempt(null) { nori.player.play(nori.library.shuffleAll(), from = PageOrigin(OriginKind.SHUFFLE_SONGS, "")) }
    /** Random albums, each whole and in its own order. */
    fun shuffleAlbums() = attempt(null) { nori.player.play(nori.library.shuffleAlbums(), from = PageOrigin(OriginKind.SHUFFLE_ALBUMS, "")) }

    /** An endless-ish mix seeded from one song. */
    fun startRadio(song: Song) = attempt(null) { nori.player.play(nori.library.radio(song)) }

    /** Picks up the queue another device (or the web player) left on the server. */
    fun resumeFromServer() = attempt(null) {
        // What to do with what the server kept is the core's (`resume_from_server`).
        when (val plan = nori.library.resumeFromServer()) {
            is dev.nori.music.ffi.library.ResumePlan.Nothing -> _messages.send(say.noServerQueue)
            is dev.nori.music.ffi.library.ResumePlan.Play -> { nori.player.play(plan.songs, plan.index.toInt()); nori.player.seekTo(plan.positionMs.toLong()) }
        }
    }

    /**
     * octo-fiesta downloads a provider item into the library when it is starred: a song on its own, an album
     * or playlist in full, on the server, without streaming it to the phone.
     */
    fun addToLibrary(id: String, isAlbum: Boolean) = attempt(say.serverDownloading) {
        nori.library.star(if (isAlbum) StarKind.ALBUM else StarKind.SONG, id, true)
    }

    fun star(song: Song, on: Boolean) = favourite(on) { nori.library.star(StarKind.SONG, song.id, on) }
    fun starAlbum(id: String, on: Boolean) = favourite(on) { nori.library.star(StarKind.ALBUM, id, on) }
    fun starArtist(id: String, on: Boolean) = favourite(on) { nori.library.star(StarKind.ARTIST, id, on) }

    /**
     * The heart changes the moment it is pressed (starMarks), so its message does too: it used to wait
     * for the server, and a quick favourite-unfavourite showed "Added" long after the heart was empty
     * again. Only a failure comes back afterwards. The message can be switched off in Settings (the core
     * says nothing then).
     */
    private fun favourite(on: Boolean, block: suspend () -> Unit) {
        if (dev.nori.music.ffi.favouriteNotice()) _messages.trySend(say.favourite(on))
        attempt(null, block)
    }

    private val _shares = Channel<String>(Channel.BUFFERED)
    /** Links ready to hand to the system share sheet. */
    val shares = _shares.receiveAsFlow()
    fun share(id: String) = attempt(null) { _shares.send(nori.library.share(id)) }

    fun download(songs: List<Song>) = askingBeats { beats ->
        nori.downloads.download(songs, beats)
        warmCovers(songs)
        _messages.trySend(say.downloadingSongs(songs.size))
    }

    /**
     * Settings' "Analyse downloaded songs": the downloads with no current analysis, and those the beat model has
     * not read if it is on and wanted, are read back one at a time under the download service.
     */
    fun analyseDownloads() = askingBeats { beats ->
        nori.downloads.analyse(beats) { n -> _messages.trySend(if (n > 0) say.analysingDownloads(n) else say.nothingToAnalyse) }
    }

    /**
     * A question waiting for its answer: whether the beat model also reads the songs being downloaded. [go] does
     * what was asked for, with the answer.
     */
    class BeatsAsk(internal val go: (Boolean) -> Unit)

    private val _beatsAsk = MutableStateFlow<BeatsAsk?>(null)
    /** Download pressed with "Better beat detection" on and "ML beats for downloads" at Ask: the question on screen. */
    val beatsAsk: StateFlow<BeatsAsk?> = _beatsAsk

    /** Runs [go] with whether the beat model reads the songs: asked first when the settings say to (the core's `download_beats_offer`). */
    private fun askingBeats(go: (Boolean) -> Unit) {
        val offer = runCatching { dev.nori.music.ffi.downloadBeatsOffer() }.getOrDefault(dev.nori.music.ffi.transfers.BeatsOffer.OFF)
        if (offer == dev.nori.music.ffi.transfers.BeatsOffer.ASK) _beatsAsk.value = BeatsAsk(go)
        else go(offer == dev.nori.music.ffi.transfers.BeatsOffer.YES)
    }

    /** The question answered: [yes] or no, and with [remember] the setting takes the answer and it is not asked again. */
    fun answerBeats(yes: Boolean, remember: Boolean) {
        val ask = _beatsAsk.value ?: return
        _beatsAsk.value = null
        if (remember) {
            dev.nori.music.ffi.settings.settingSet("downloadBeats", dev.nori.music.ffi.downloadBeatsRemembered(yes).name)?.let(nori.settings::took)
        }
        ask.go(yes)
    }

    /** The question put away unanswered: nothing is downloaded. */
    fun dismissBeats() { _beatsAsk.value = null }

    /**
     * Fetches the artwork of songs being downloaded onto the disk; which covers, at which addresses, is
     * the core's (`Core::download_cover_urls`). Not decoded: nothing is drawing them now, and hundreds of
     * them would push the covers on screen out of memory.
     */
    private fun warmCovers(songs: List<Song>) = viewModelScope.launch(kotlinx.coroutines.Dispatchers.IO) {
        val loader = dev.nori.music.data.CoverLoader.get(getApplication<Application>())
        for (url in nori.core.downloadCoverUrls(songs.mapNotNull { it.coverArt })) loader.warm(url)
    }

    /** Gives the downloads back: the same menu entry that offered them should be able to take them away. */
    fun undownload(songs: List<Song>) {
        nori.downloads.remove(songs.map { it.id })
        _messages.trySend(say.downloadsRemoved(songs.size))
    }
    fun removeDownloads(ids: List<String>) = nori.downloads.remove(ids)

    /** What each download this session touched is doing; see [dev.nori.music.downloads.Downloads.marks]. */
    val downloadMarks: StateFlow<Map<String, DownloadMark>> = nori.downloads.marks

    /**
     * The downloads screen's lists: downloading, waiting (in the order they will run), failed, and
     * finished this session - split in the core, asked again when the index or a phase changes, and
     * only while the screen is watching. Null until the first answer, which is not the same as empty.
     */
    val downloadSections: StateFlow<DownloadSections?> =
        // A song finishing changes the table and its mark together: the changes are only the signal, and
        // the ones that arrive while the lists are being worked out are asked for once, not once each.
        combine(nori.downloads.state, nori.downloads.marks) { _, _ -> }
            .conflate()
            .map { runCatching { nori.core.downloadSections() }.getOrNull() }
            .flowOn(kotlinx.coroutines.Dispatchers.IO)
            .stateIn<DownloadSections?>(viewModelScope, SharingStarted.WhileSubscribed(5_000), null)

    /**
     * Every downloaded song, newest first, for the library's downloads page: read again when the table
     * changes, and only while the page is watching. Null until the first answer.
     */
    val downloadedSongs: StateFlow<List<Song>?> =
        nori.downloads.state.map { it.done }
            .flowOn(kotlinx.coroutines.Dispatchers.IO)
            .stateIn<List<Song>?>(viewModelScope, SharingStarted.WhileSubscribed(5_000), null)

    fun retryDownloads(songs: List<Song>) = nori.downloads.retry(songs)
    fun cancelDownloads(songs: List<Song>) = nori.downloads.cancel(songs.map { it.id })
    fun cancelAllDownloads() = nori.downloads.cancelAll()

    suspend fun playlists(): List<Playlist> = runCatching { nori.library.playlists().first() }.getOrDefault(emptyList())
    fun addToPlaylist(p: Playlist, songs: List<Song>) = attempt(say.addedToPlaylist(p.name)) { nori.library.addToPlaylist(p.id, songs.map { it.id }) }
    fun addToNewPlaylist(name: String, songs: List<Song>) = attempt(say.playlistCreated(name)) { nori.library.createPlaylist(name, songs.map { it.id }) }
}
