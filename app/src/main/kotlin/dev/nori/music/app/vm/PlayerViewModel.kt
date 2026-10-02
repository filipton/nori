package dev.nori.music.app.vm

import android.app.Application
import android.media.AudioManager
import dev.nori.music.ffi.model.Lyrics
import dev.nori.music.data.FoundLyrics
import dev.nori.music.data.followSong
import dev.nori.music.ffi.settings.LyricsOrigin
import dev.nori.music.net.said
import dev.nori.music.playback.PlayerState
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.stateIn
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.launch

class PlayerViewModel(app: Application) : NoriViewModel(app) {
    private val player = nori.player
    val state: StateFlow<PlayerState> = player.state
    /** Where a seek asked to go, while it is still being watched into place; the seek bar holds this. */
    val pendingSeek: StateFlow<Long?> = player.pendingSeek

    /** A mix is being heard (see [dev.nori.music.playback.PlayerConnection.mixing]): the seek row says so. */
    val mixing: StateFlow<Boolean> = player.mixing

    /** Just the id, so a list can highlight its playing row without observing the whole player. */
    val currentId: StateFlow<String?> = state.map { it.current?.id }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), null)

    /**
     * Whether the queue is the one [page] started (nori-queue `playlist_from`): its Play or Shuffle, or a
     * row of its list, and any edit since. Asked when [PlayerState.origin] moves, not on every event.
     */
    fun playsFrom(page: dev.nori.music.ffi.library.PageQueue): Boolean = dev.nori.music.ffi.queue.playlistFrom(page)

    /** Just the play/pause flag, for the same reason: the marked row's bars move only while it sounds. */
    val sounding: StateFlow<Boolean> = state.map { it.playing }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), false)

    private val lyricsKept = dev.nori.music.data.SongAnswers<Load<FoundLyrics>>()

    /**
     * Lyrics of whatever is heard (the page's song, [PlayerState.current]); fetched only while a lyrics
     * view is collecting. Each song starts from Loading, so the view shows its loader and then the new
     * words, instead of holding the last song's lyrics on screen while the next ones are fetched.
     *
     * Every answer names the song it is for, and the view shows it only under that song
     * ([dev.nori.music.data.ForSong.of]): the last answer outlives the collecting, and the panel opened
     * again after the song had changed used to be handed the old song's words first - under the new
     * title, with the new song's playhead, so nothing lit and nothing scrolled. Because of that tag the
     * last answer is kept when the panel stops watching: under the same song it is the right one, and
     * the panel opened again shows it at once instead of the loader and a lookup all over again.
     *
     * The answers of the last few songs are kept too ([lyricsKept]): a song come back to - the panel
     * reopened after the upstream stopped, or the song heard for a moment again around a skip - starts
     * from its words, and is not looked up again once its lookup had finished. The same words read again
     * are equal and not handed on as new ones, which faded them out and in and reset their clock.
     */
    val lyrics: StateFlow<dev.nori.music.data.ForSong<Load<FoundLyrics>>> = state.map { it.current }
        .followSong(
            id = { it.id },
            loading = Load.Loading,
            none = { Load.Ready(FoundLyrics(dev.nori.music.ffi.library.lyricsNone(), LyricsOrigin.SERVER)) },
            failed = { Load.Failed(it.said ?: it.javaClass.simpleName) },
            answers = lyricsKept,
            keep = { it is Load.Ready && it.data.lyrics.lines.isNotEmpty() },
        ) { song -> (dev.nori.music.app.testLyrics(song) ?: nori.library.lyricsFor(song)).map<FoundLyrics, Load<FoundLyrics>> { Load.Ready(it) } }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), dev.nori.music.data.ForSong(null, Load.Loading))

    /**
     * The moving cover of the album playing (Settings, Look): an HLS address, or null when it has none,
     * when the switch is off, or on mobile data while it is kept to Wi-Fi. Found by the core
     * (`motion_video`, which remembers every answer) only while the player collects this, which is while
     * it is open, and once per album: the next song of the same record carries on with the video it has.
     * A new album starts from null, so the last one's video steps back at once rather than playing on
     * under the new cover. Switched off, nothing is asked at all.
     */
    @OptIn(ExperimentalCoroutinesApi::class)
    val motionVideo: StateFlow<String?> = kotlinx.coroutines.flow.combine(
        state.map { it.current }.distinctUntilChanged { a, b -> a?.albumId == b?.albumId && a?.album == b?.album && a?.artist == b?.artist },
        nori.settings.prefs.map { (it.thirdPartyLookups && it.motionArtwork) to it.motionArtworkWifiOnly }.distinctUntilChanged(),
    ) { song, rule -> song to rule.first }
        .flatMapLatest { (song, on) ->
            if (song == null || !on) flowOf<String?>(null)
            else kotlinx.coroutines.flow.flow<String?> {
                emit(null)
                emit(kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { nori.client.motionVideo(song, nori.http.metered) })
            }
        }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), null)

    /**
     * The player behind the moving cover. The object is only made when the screen first hands it a
     * surface, and its ExoPlayer only when it first plays; with the switch off neither ever is.
     */
    private val motionLazy = lazy {
        nori.motionPlayer { gone -> viewModelScope.launch(kotlinx.coroutines.Dispatchers.IO) { nori.client.motionForget(gone) } }
    }
    private val motion by motionLazy

    /** The moving cover whose first frame is on its surface; the screen fades it in after this. */
    val motionReady: StateFlow<String?> get() = motion.ready

    /** The moving cover's surface arrived on screen ([shown]) or left it. */
    fun motionView(view: android.view.TextureView, shown: Boolean) {
        if (shown) motion.show(view) else if (motionLazy.isInitialized()) motion.hide(view)
    }

    fun motionPlay(url: String) = motion.play(url)

    fun motionPause() {
        if (motionLazy.isInitialized()) motion.pause()
    }

    fun motionRelease() {
        if (motionLazy.isInitialized()) motion.release()
    }

    override fun onCleared() {
        motionRelease()
        super.onCleared()
    }

    private val _coversNear = kotlinx.coroutines.flow.MutableStateFlow<List<String>>(emptyList())
    /** The playing song's cover and those a skip either way lands on, for the bar to work their colours out ahead. */
    val coversNear: StateFlow<List<String>> = _coversNear

    init {
        // The artwork either side of what is playing, fetched before it is asked for. A skip used to
        // show an empty sleeve for as long as the server took to render the next cover - on a slow one,
        // seconds. Same sizes and requests as the player and the rows, so a warmed cover is a cache hit.
        // How far ahead is the user's (Settings, "Covers fetched ahead"); already-cached ones cost a
        // memory lookup and nothing else.
        // Which positions, in which order, is the core's (`covers_around`, over the queue it keeps, which
        // also names the covers either side whose colours the bar works out ahead); it is asked once, only
        // when the queue, the playing song or a skip's target moves, not on every play/pause or buffering change.
        viewModelScope.launch {
            kotlinx.coroutines.flow.combine(state, nori.settings.prefs.map { it.coversAhead }.distinctUntilChanged()) { s, ahead -> s to ahead }
                .distinctUntilChanged { (a, x), (b, y) ->
                    a.queue === b.queue && a.index == b.index && a.previousIndex == b.previousIndex && a.nextIndex == b.nextIndex && x == y
                }
                .map { (s, ahead) -> dev.nori.music.ffi.coversAround(s.index, s.previousIndex, s.nextIndex, ahead) }
                .distinctUntilChanged()
                .collect { around ->
                    _coversNear.value = around.near
                    // The covers a skip lands on (either side of the song playing) into memory, decoded: a
                    // skip lands on a picture that is already there. The ones further out onto the disk
                    // only, as the server sent them: decoded they held a megabyte or two each, ten either
                    // way with "Covers fetched ahead" at 10, and a skip that far is a disk read away.
                    val loader = dev.nori.music.data.CoverLoader.get(getApplication<Application>())
                    for (want in around.wants) {
                        val url = nori.library.coverUrl(want.id, want.size.toInt()) ?: continue
                        if (want.id in around.near) loader.prefetch(url) else loader.warm(url)
                    }
                }
        }
    }


    /** Pull, do not push: the UI reads this on its own clock while the seek bar is on screen. */
    val positionMs: Long get() = player.positionMs
    /** The player's own place, unshaped: for the test bridge's traces. */
    val playerPositionMs: Long get() = player.playerPositionMs

    /** [positionMs] while the song playing is still [songId]; null once the player has left it. */
    fun positionIn(songId: String?): Long? = dev.nori.music.data.playheadFor(songId, state.value.current?.id) { player.positionMs }

    fun connect() = player.connect()
    fun toggle() = player.toggle()
    fun next() = player.next()
    fun previous() = player.previous()
    fun previousItem() = player.previousItem()
    fun seekTo(ms: Long) = player.seekTo(ms)
    fun skipTo(index: Int) = player.skipTo(index)
    fun remove(index: Int) = player.remove(index)
    fun restore(song: dev.nori.music.ffi.model.Song, index: Int) = player.restore(song, index)
    fun move(from: Int, to: Int) = player.move(from, to)
    fun toggleShuffle() = player.setShuffle(!state.value.shuffle)
    fun cycleRepeat() = player.cycleRepeat()
    fun sleep(minutes: Int, endOfTrack: Boolean = false, songs: Int = 0) = player.sleep(minutes, endOfTrack, songs)

    /**
     * The phone's music-stream volume, for the player's volume slider. Read live (hardware keys can
     * move it under us) and written without flags, so dragging it never pops a system UI over the art.
     */
    private val audio = app.getSystemService(AudioManager::class.java)
    /**
     * The music stream's volume, pushed the moment it changes. It used to be read once a second by the
     * player screen, so a press of the volume keys took up to a second to show on the slider, and the
     * screen ticked for as long as it was open. Android announces every change with a broadcast - the
     * one ExoPlayer's own volume tracking listens to - and this listens only while the screen collects
     * it: nothing registered, and nothing running, once the player is closed.
     */
    val volume: StateFlow<Float> = kotlinx.coroutines.flow.callbackFlow {
        val context = getApplication<Application>()
        trySend(volumeFraction())
        val receiver = object : android.content.BroadcastReceiver() {
            override fun onReceive(c: android.content.Context, intent: android.content.Intent) {
                val stream = intent.getIntExtra("android.media.EXTRA_VOLUME_STREAM_TYPE", AudioManager.STREAM_MUSIC)
                if (stream == AudioManager.STREAM_MUSIC) trySend(volumeFraction())
            }
        }
        androidx.core.content.ContextCompat.registerReceiver(
            context, receiver, android.content.IntentFilter("android.media.VOLUME_CHANGED_ACTION"),
            androidx.core.content.ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        // That broadcast is not public API, and a hardened or future Android may stop delivering it. The
        // system also writes every volume to its settings store half a second or so later, which any app
        // may watch; so if the broadcast never comes, the slider is late rather than frozen.
        val observer = object : android.database.ContentObserver(android.os.Handler(android.os.Looper.getMainLooper())) {
            override fun onChange(selfChange: Boolean) { trySend(volumeFraction()) }
        }
        context.contentResolver.registerContentObserver(android.provider.Settings.System.CONTENT_URI, true, observer)
        awaitClose {
            context.unregisterReceiver(receiver)
            context.contentResolver.unregisterContentObserver(observer)
        }
    }.distinctUntilChanged().stateIn(viewModelScope, SharingStarted.WhileSubscribed(1_000), volumeFraction())

    fun volumeFraction(): Float {
        val max = audio.getStreamMaxVolume(AudioManager.STREAM_MUSIC).takeIf { it > 0 } ?: return 0f
        return audio.getStreamVolume(AudioManager.STREAM_MUSIC) / max.toFloat()
    }
    fun setVolumeFraction(f: Float) {
        val max = audio.getStreamMaxVolume(AudioManager.STREAM_MUSIC).takeIf { it > 0 } ?: return
        // Nearest step, not the one below: truncating made the bar jump back a notch every time it was let go.
        audio.setStreamVolume(AudioManager.STREAM_MUSIC, kotlin.math.round(f.coerceIn(0f, 1f) * max).toInt().coerceIn(0, max), 0)
    }
}
