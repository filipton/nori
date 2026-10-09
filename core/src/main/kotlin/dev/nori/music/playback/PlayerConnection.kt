package dev.nori.music.playback

import android.content.ComponentName
import android.content.Context
import android.os.Bundle
import android.os.SystemClock
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.common.util.Util
import androidx.media3.session.MediaController
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionToken
import com.google.common.util.concurrent.MoreExecutors
import dalvik.annotation.optimization.CriticalNative
import dalvik.annotation.optimization.FastNative
import dev.nori.music.Nori
import dev.nori.music.core.R
import dev.nori.music.ffi.queue.Hand
import dev.nori.music.ffi.queue.NextAction
import dev.nori.music.ffi.model.PageOrigin
import dev.nori.music.ffi.model.RadioStation
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.Mirror
import dev.nori.music.ffi.remote.Op
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

enum class Repeat { OFF, ALL, ONE }

/** Everything a UI needs to draw the player. Position is deliberately absent: see [PlayerConnection.positionMs]. */
data class PlayerState(
    val connected: Boolean = false,
    val queue: List<Song> = emptyList(),
    val index: Int = -1,
    /** What a skip either way lands on, shuffle and repeat included; -1 at an end. Not [index] ± 1 under shuffle. */
    val nextIndex: Int = -1,
    val previousIndex: Int = -1,
    /** [queue]'s indices in the order they play: the shuffle order under shuffle, otherwise 0 until size. */
    val order: List<Int> = emptyList(),
    /** Indices of the songs added by hand (Play next, Add to queue). */
    val queued: Set<Int> = emptySet(),
    val radio: String? = null,
    val playing: Boolean = false,
    val buffering: Boolean = false,
    val shuffle: Boolean = false,
    val repeat: Repeat = Repeat.OFF,
    val durationMs: Long = 0,
    val error: String? = null,
    /** elapsedRealtime at which playback will pause, or 0. */
    val sleepAt: Long = 0,
    val sleepAtEndOfTrack: Boolean = false,
    /**
     * Playing from downloads while the server is unreachable; the original queue is parked after the
     * bridge block and comes back when the network does.
     */
    val bridging: Boolean = false,
    /**
     * Moves whenever a new queue is set (nori-queue `playlist_origin_gen`), so a page asks whether the
     * queue is its own (`playlist_from`) only then.
     */
    val origin: Int = 0,
    /** The device playing while it is not this phone: the page shows and controls that one. */
    val playingOn: String? = null,
    /** [playingOn] is the host of a jam this phone is a guest in: the page shows it and controls nothing. */
    val jamGuest: Boolean = false,
) {
    val current: Song? get() = queue.getOrNull(index)
}

/**
 * The UI's only handle on playback. It talks to [PlaybackService] through a
 * MediaController, so the UI holds no player and the service can outlive it.
 * State is pushed on change; the playhead is pulled ([positionMs]) so that a
 * hidden player screen costs nothing.
 */
@UnstableApi
class PlayerConnection(private val context: Context, private val nori: Nori) {
    private val _state = MutableStateFlow(PlayerState())
    val state: StateFlow<PlayerState> = _state
    private var controller: MediaController? = null
    /** The connection under way; forgotten by [disconnect], so one landing after it is let go at once. */
    private var connecting: com.google.common.util.concurrent.ListenableFuture<MediaController>? = null
    private val pending = ArrayList<(MediaController) -> Unit>()

    /** Moves whenever a new queue is set (nori-queue `playlist_origin_gen`): when to ask again which page it came from. */
    val queueOrigin: Int get() = PlaylistJni.origin(nori.sessionHandle)

    /**
     * The device playing elsewhere (Remotes.mirror), which the page shows and every control here acts on
     * while there is one, or the host of a jam this phone is a guest in (Remotes.jamPlaying), shown only;
     * null while this phone plays.
     */
    private var mirror: Mirror? = null
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    init {
        scope.launch {
            kotlinx.coroutines.flow.combine(nori.remotes.mirror, nori.remotes.jamPlaying) { m, host -> m?.let { it to false } ?: host?.let { it to true } }
                .collect { mirrored(it?.first, it?.second == true) }
        }
    }

    private fun mirrored(m: Mirror?, guest: Boolean) {
        val was = mirror
        mirror = m
        when {
            m != null -> showMirror(m, guest)
            // Back here: the page shows this phone's own player again.
            was != null -> controller?.let { publish(it, queueChanged = true) } ?: run { _state.value = PlayerState() }
        }
    }

    /**
     * The mirrored device's queue and playback as the page's state; its rows are in play order already. A
     * jam [guest] has no song either side to skip to.
     */
    private fun showMirror(m: Mirror, guest: Boolean) {
        val old = _state.value
        val songs = m.rows.map { it.song }
        val queue = if (songs == old.queue) old.queue else songs
        val at = m.at?.toInt() ?: -1
        val all = m.repeat.toInt() == Player.REPEAT_MODE_ALL && m.rows.size.toUInt() == m.len
        _state.value = old.copy(
            connected = true, queue = queue, index = at,
            nextIndex = if (at < 0 || guest) -1 else (at + 1).takeIf { it < queue.size } ?: (if (all) 0 else -1),
            previousIndex = if (at < 0 || guest) -1 else (at - 1).takeIf { it >= 0 } ?: (if (all) queue.lastIndex else -1),
            order = if (old.order.size == queue.size && old.order.withIndex().all { (k, v) -> k == v }) old.order else queue.indices.toList(),
            queued = emptySet(), radio = null, playing = m.playing, buffering = m.buffering && m.playing, shuffle = m.shuffle,
            repeat = when (m.repeat.toInt()) { Player.REPEAT_MODE_ALL -> Repeat.ALL; Player.REPEAT_MODE_ONE -> Repeat.ONE; else -> Repeat.OFF },
            durationMs = (queue.getOrNull(at)?.duration?.toLong() ?: 0L) * 1000, error = null, bridging = false, playingOn = m.name,
            jamGuest = guest,
        )
    }

    /** The mirrored device's place now: where its listener is, within the song (the core's clock is elapsedRealtime's). */
    private fun mirrorPosition(m: Mirror): Long {
        val ran = if (m.playing) ((android.os.SystemClock.elapsedRealtimeNanos() / 1_000 - m.atUs) / 1_000 * m.rate).toLong() else 0L
        val end = _state.value.durationMs.takeIf { it > 0 } ?: Long.MAX_VALUE
        return (m.positionMs + ran).coerceIn(0, end)
    }

    /** [op] for the mirrored device; shown at once, as it is expected to come out. */
    private fun remote(op: Op) = nori.remotes.command(op)

    /** The mirrored device's list index for the page's row [row]. */
    private fun remoteIndex(m: Mirror, row: Int): UInt? = m.rows.getOrNull(row)?.index

    /**
     * Where the seek bar is. Through a transition the player runs ahead of the ear (the held ending is
     * counted as played so the next track arrives in time to be mixed in); the sink says what is really
     * heard, and the bar shows that, in the song it belongs to (see publish) - held while the ear has
     * changed song and the page has not followed yet, and run on from where it was while the controller
     * is being built again and can answer nothing (reporting zero then makes the bar snap to 0:00 and
     * jump back a heartbeat later). Those rules are nori-player's (heard.rs Playhead); this is one JNI
     * call with primitives in and out per frame.
     */
    val positionMs: Long get() {
        mirror?.let { return mirrorPosition(it) }
        val c = controller ?: local() ?: return PlayheadJni.runOn(clock, android.os.SystemClock.elapsedRealtime(), _state.value.playing)
        return heard(c, _state.value.index)
    }

    /**
     * The service's own player, while the controller is not connected and this is the main thread: it
     * lives in this process, on this thread, and answers at once. The controller is let go of while the
     * app is hidden (MainActivity.onStop) and connected again only after the first frame is out, and
     * connecting takes a moment more: the screen come back used to show the song from before it went,
     * for half a second, when the music had moved on while it was off.
     */
    private fun local(): Player? =
        PlaybackService.sessionPlayer?.takeIf { android.os.Looper.myLooper() == android.os.Looper.getMainLooper() }

    /**
     * The page brought up to date from the service's player in one read, for a screen coming back: called
     * before its first frame (MainActivity.onStart), so that frame shows the song playing now. Nothing is
     * read while the app is hidden; this is one look as it returns, and the controller's own state takes
     * over when it connects.
     */
    fun catchUp() {
        // The engine may have slept for minutes (the songs offloaded): it reads its output now, so that the
        // first frame's seek bar starts from a fresh reading, not one run on from its last wake.
        engine()?.look()
        if (controller != null || mirror != null) return
        val p = local() ?: return
        if (p.mediaItemCount == 0) return
        publish(p, queueChanged = true)
    }

    /**
     * The song being heard and the place in it, while that is not what the player says. Through a
     * transition the player runs ahead of the ear: the held ending is counted as played the moment it
     * is decoded, so that the next track arrives in time to be mixed in, and the player is on the next
     * song while this one's ending still plays alone. The sink says what is really heard; the UI shows
     * that, in the song it belongs to; the song is left in [heardIndex]. False when the
     * player's own word is the truth.
     *
     * The sink's reading is taken when the player asks for its position, which with a deep buffer is
     * seconds apart, so it is run on from there at one times - and past the point where the mix takes
     * over the ear has left the song, whether or not the sink has been asked since. The rules live in
     * nori-player (crates/player/src/heard.rs), so any app on it shows the same.
     */
    private fun heard(c: MediaController): Boolean {
        // One call, primitives only. The queue is the core's own.
        read(HeardJni.at(clock, android.os.SystemClock.elapsedRealtime(), c.isPlaying, c.currentPosition))
        return heardIndex >= 0
    }

    /**
     * [heard] for the seek bar, whose page shows queue index [shown]: the place the bar shows. It goes by
     * the engine's own place, read in this process ([EnginePlayer.shownMs]), not by the controller's: a
     * controller's place is the session's last word run on at one times and held at the song's length,
     * put right only by a play, a pause or a seek, so a word taken off (the S22's first reading as the
     * phone was unlocked) kept the bar at the song's end with 14 s left. A controller that drifted like
     * that is put right as well, for the notification and the lock screen that go by the same word. The
     * controller's own place stands while a seek is on its way (it has the seek; the engine not yet) and
     * on another song than the engine's.
     */
    private fun heard(c: Player, shown: Int): Long {
        val raw = c.currentPosition
        val now = android.os.SystemClock.elapsedRealtime()
        val on = c.currentMediaItemIndex
        val engine = engine()?.takeIf { _pendingSeek.value == null }
        val r = PlayheadJni.position(clock, now, c.isPlaying, on, raw, shown, engine?.handle ?: 0L)
        val out = read(r)
        if (c === controller && c.isPlaying && r < 0 && now - reanchored > REANCHOR_GAP_MS) {
            reanchored = now
            dev.nori.music.NoriLog.i("seek bar: the controller ran on to $raw ms, the engine is at ${engine?.shownMs(on)} ms: the session says its place again")
            engine?.reanchor()
        }
        if (tracePositions) android.util.Log.d("noripos", "raw=$raw engine=${engine?.shownMs(on)} out=$out on=$on shown=$shown heard=$heardIndex c=${c.javaClass.simpleName} t=${Thread.currentThread().name}")
        return out
    }

    /** The service's engine, on the main thread only (where it lives): null with no service. */
    private fun engine(): EnginePlayer? =
        PlaybackService.rustPlayer?.takeIf { android.os.Looper.myLooper() == android.os.Looper.getMainLooper() }

    /** When a drifted controller was last put right ([heard]), elapsed realtime ms. */
    private var reanchored = Long.MIN_VALUE / 2

    /** Unpacks an answer into [heardIndex]; returns the place in it. */
    private fun read(r: Long): Long {
        heardIndex = ((r ushr 44) and 0x7FFFF).toInt() - 1
        // The ear changed song between two readings: the page changes with it now, not at the next one.
        if ((r ushr 43) and 1L != 0L) main.post { controller?.let { publish(it, queueChanged = false) } }
        return r and ((1L shl 43) - 1)
    }

    /** What [heard] last found: the queue index the ear is on (-1: the player's own). */
    private var heardIndex = -1
    /** nori-player's reading of the transition engine: see crates/player/src/heard.rs. */
    private val clock = HeardJni.create(nori.sessionHandle)

    private val _mixing = MutableStateFlow(false)
    /**
     * A mix (AutoMix, a crossfade) is being heard right now, while the music plays. Pushed, not polled:
     * the player says when a mix starts and stops being heard (nori-engine nudges [publish], as for a
     * change of song), so between mixes nothing runs for it.
     * Apart from [state], so that a mix starting recomposes only what says so.
     */
    val mixing: StateFlow<Boolean> = _mixing

    /** The player's own place, as the controller runs it on: for the test bridge's traces only. */
    val playerPositionMs: Long get() = controller?.currentPosition ?: -1L

    fun connect() {
        if (controller != null || connecting != null) return
        val future = MediaController.Builder(context, SessionToken(context, ComponentName(context, PlaybackService::class.java))).buildAsync()
        connecting = future
        future.addListener({
            val c = runCatching { future.get() }.getOrNull()
            if (connecting !== future) {
                // Disconnected meanwhile: what was asked of it is done, and it is let go.
                c?.let { pending.forEach { action -> action(it) }; it.release() }
                pending.clear()
                return@addListener
            }
            connecting = null
            c ?: return@addListener
            controller = c
            c.addListener(listener)
            // A mix starting or ending is a change to the UI, and the player itself fires no event for it.
            PlaybackService.onMixingChanged = { main.post { controller?.let { publish(it, queueChanged = false) } } }
            // A new queue made in the core: its origin read again once the service has set it.
            PlaybackService.onQueueSet = { main.post { controller?.let { publish(it, queueChanged = true) } } }
            PlaybackService.onLanded = { followSeek() }
            publish(c, queueChanged = true)
            pending.forEach { it(c) }
            pending.clear()
        }, MoreExecutors.directExecutor())
    }

    fun disconnect() {
        connecting = null
        _pendingSeek.value = null
        controller?.let { it.removeListener(listener); it.release() }
        controller = null
        _state.value = _state.value.copy(connected = false)
    }

    /**
     * Every controller call goes through here, and a MediaController may only be touched from the main
     * thread - it throws otherwise. Callers are not all on it (switching servers happens on an IO
     * thread while the network is being probed), so anything arriving from elsewhere is posted rather
     * than left to blow up in the caller's face.
     */
    private fun with(action: (MediaController) -> Unit) {
        if (android.os.Looper.myLooper() != android.os.Looper.getMainLooper()) {
            android.os.Handler(android.os.Looper.getMainLooper()).post { with(action) }
            return
        }
        controller?.let(action) ?: run { pending += action; connect() }
    }

    private val listener = object : Player.Listener {
        override fun onEvents(player: Player, events: Player.Events) {
            followSeek()
            publish(player, events.contains(Player.EVENT_TIMELINE_CHANGED))
        }

        override fun onPlayerError(error: PlaybackException) {
            // The platform only sorts its error into the core's kinds (media3's audio output codes are the
            // 5000s), and says it in its own words. It used to show the exception's own text, or a
            // constant's name when the controller had none.
            val kind = when {
                error.errorCode in 5000 until 6000 -> dev.nori.music.ffi.model.PlaybackError.OUTPUT
                error.isNetworkish() -> dev.nori.music.ffi.model.PlaybackError.NETWORK
                else -> dev.nori.music.ffi.model.PlaybackError.OTHER
            }
            val said = when (kind) {
                dev.nori.music.ffi.model.PlaybackError.OUTPUT -> R.string.playback_error_output
                dev.nori.music.ffi.model.PlaybackError.NETWORK -> R.string.playback_error_network
                else -> R.string.playback_error_other
            }
            _state.value = _state.value.copy(error = context.getString(said))
        }
    }

    /** The core queue's revision the page's queue was read at; -1 when it was not read from the core. */
    private var viewRev = -1L

    /** The core list's revision the page's songs were read at (0: none held), so they are not sent again. */
    private var heldList = 0uL

    /** The last radio title worked out, and what it was worked out from: asked again only when those change. */
    private var radioAnnounced: String? = null
    private var radioStation: String? = null
    private var radioShown: String? = null

    private fun publish(p: Player, queueChanged: Boolean) {
        // The session follows the mirrored device then; the page reads it from the core instead.
        if (mirror != null) return
        val old = _state.value
        val item = p.currentMediaItem
        val fresh = queueChanged || !old.connected
        val look = fresh || p.shuffleModeEnabled != old.shuffle
        // The queue is the core's (crates/queue/src/playlist.rs), read in one call; the controller's copy
        // of it trails the service a little, and while the two differ in length the core reads the
        // player's own list instead (playlist_view_of). A timeline change is not always a queue change (a
        // song's source opening is one too), so the core's revision is asked first and a queue the page
        // already holds is not copied over again; nor are its songs when only the order changed.
        val same = look && viewRev >= 0 && PlaylistJni.rev(nori.sessionHandle) == viewRev && old.queue.size == p.mediaItemCount
        val view = if (look && !same) nori.session.playlistViewFor(heldList, p.mediaItemCount.toUInt()) ?: playerView(p) else null
        if (view != null) { viewRev = view.rev.toLong(); heldList = view.listRev }
        val queue = view?.songs?.takeIf { it.size == view.len.toInt() } ?: old.queue
        val order = view?.order?.map { it.toInt() } ?: old.order
        val queued = view?.queued?.mapTo(HashSet()) { it.toInt() } ?: old.queued
        // The song on the page is the one being heard. Into a transition the player has moved on to
        // the next song while the ending of this one still plays alone (see heard); the page stays
        // on this song until the mix is heard, and moves to the next one the moment it is, even while
        // the player is still on the old one. Which row of the page's list that is, the core decides
        // (crates/queue/src/heard.rs shown_row).
        val heardIndex = (p as? MediaController)?.takeIf(::heard)?.let {
            nori.session.heardShownRow(this.heardIndex.takeIf { it >= 0 }?.toUInt(), queue.map { it.id }, item?.mediaId)?.toInt()
        }
        _mixing.value = p.isPlaying && PlaybackService.rustPlayer?.mixing == true
        _state.value = old.copy(
            connected = true, queue = queue, order = order, queued = queued,
            index = if (p.mediaItemCount == 0) -1 else heardIndex ?: p.currentMediaItemIndex,
            nextIndex = if (p.mediaItemCount == 0) -1 else p.nextMediaItemIndex,
            previousIndex = if (p.mediaItemCount == 0) -1 else p.previousMediaItemIndex,
            // For a stream the live metadata carries what the station announces (ICY title); which of that and
            // the station's name shows is the core's (words.rs radio_title).
            radio = item?.takeIf { it.isRadio }?.let { radioTitle(p.mediaMetadata.title?.toString(), it.mediaMetadata.title?.toString()) },
            playing = p.isPlaying, buffering = p.playbackState == Player.STATE_BUFFERING && p.playWhenReady,
            // A weighted shuffle plays a pre-spread list with the player's shuffle off so the order sticks;
            // the core keeps the control lit until the user turns it off or starts a plain Play.
            shuffle = p.shuffleModeEnabled || PlaylistJni.shuffleShown(nori.sessionHandle),
            repeat = when (p.repeatMode) { Player.REPEAT_MODE_ALL -> Repeat.ALL; Player.REPEAT_MODE_ONE -> Repeat.ONE; else -> Repeat.OFF },
            // The heard song's length while the ear is a song behind the player, else the player's, else
            // the tags' (nori_player::heard::shown_duration_ms).
            durationMs = PlayheadJni.durationMs(heardIndex?.let { queue[it].duration.toLong() } ?: -1, p.duration, item?.mediaMetadata?.durationMs ?: 0),
            error = if (p.playerError == null) null else old.error,
            bridging = view?.bridging ?: old.bridging,
            origin = PlaylistJni.origin(nori.sessionHandle), playingOn = null,
        )
    }

    /**
     * What a stream shows as its title (words.rs radio_title). Every player event of a radio stream
     * lands here, and nearly all of them leave the announcement as it was, so the core is asked only
     * when it changed.
     */
    private fun radioTitle(announced: String?, station: String?): String? {
        if (radioShown == null || announced != radioAnnounced || station != radioStation) {
            radioAnnounced = announced
            radioStation = station
            radioShown = dev.nori.music.ffi.radioTitle(announced, station)
        }
        return radioShown
    }

    /** The player's own list as the core reads it while it trails the core's (playlist_view_of). */
    private fun playerView(p: Player): dev.nori.music.ffi.queue.PlaylistView {
        val items = List(p.mediaItemCount) { p.getMediaItemAt(it) }
        val t = p.currentTimeline
        val order = ArrayList<UInt>(t.windowCount)
        var i = if (t.isEmpty) C.INDEX_UNSET else t.getFirstWindowIndex(p.shuffleModeEnabled)
        while (i != C.INDEX_UNSET) { order += i.toUInt(); i = t.getNextWindowIndex(i, Player.REPEAT_MODE_OFF, p.shuffleModeEnabled) }
        return nori.session.playlistViewOf(items.map { it.mediaId }, items.map { it.queuedAs() ?: dev.nori.music.ffi.queue.Hand.NO }, order)
    }

    private fun items(songs: List<Song>): List<MediaItem> = songs.toMediaItems(nori.session) { CarArt.cover(context, it.coverArt, NOTIFICATION_ART)?.toString() }

    // ---- queue ----

    /**
     * A new queue of [songs]. [from]: the page they are the songs of (its Play, Shuffle or a row of its
     * list), which that page then answers for; null for a queue from no page (one song, a selection, a
     * radio or a mix drawn from a song, a queue picked up from the server).
     */
    fun play(songs: List<Song>, startIndex: Int = 0, shuffle: Boolean = false, from: PageOrigin? = null) = with { c ->
        if (songs.isEmpty()) return@with
        mirror?.let { m ->
            remote(Op.Replace(songs, startIndex.coerceIn(0, songs.lastIndex).toUInt(), 0, true, null, shuffle, m.repeat))
            return@with
        }
        // Shuffle lit when this start asked for shuffle; cleared on a plain Play, so the album
        // control does not stay on after the row's Play starts some other queue. Pause and resume on
        // the page's own queue do not come through here, and leave the light as it was. Said to the
        // core at once, so the page does not flicker while the queue's own change is on its way.
        nori.session.playlistShowShuffle(shuffle)
        c.shuffleModeEnabled = shuffle
        c.setMediaItems(startedFrom(items(songs), from), if (shuffle) C.INDEX_UNSET else startIndex.coerceIn(0, songs.lastIndex), 0)
        c.prepare()
        c.play()
    }

    /**
     * [songs] as the new queue around the song playing, which is [songs][at]: it goes on where it is,
     * playing or paused, rather than starting again (a tap on it in its own album's list). Any other
     * song plays as [play] would.
     */
    fun keepPlaying(songs: List<Song>, at: Int, from: PageOrigin? = null) = with { c ->
        if (mirror != null || c.currentMediaItem?.mediaId != songs.getOrNull(at)?.id) return@with play(songs, at, from = from)
        nori.session.playlistShowShuffle(false)
        c.shuffleModeEnabled = false
        val made = items(songs).toMutableList()
        made[at] = made[at].kept()
        c.setMediaItems(startedFrom(made, from), at, c.currentPosition)
    }

    /**
     * Play [songs] in [order] (positions in it, the core's weighted shuffle) while keeping the Shuffle
     * control lit. Used for weighted artist-spread shuffles: media3's own shuffle would undo the spread.
     */
    fun playShuffledOrder(songs: List<Song>, order: List<UInt>, from: PageOrigin? = null) = with { c ->
        if (songs.isEmpty() || order.isEmpty()) return@with
        mirror?.let { m ->
            remote(Op.Replace(songs, order.first(), 0, true, order, true, m.repeat))
            return@with
        }
        nori.session.playlistShowShuffle(true)
        // Marked as already in order: the service takes it as it is and turns the player's own shuffle off.
        val made = items(songs)
        val items = order.map { made[it.toInt()] }
        c.setMediaItems(startedFrom(listOf(items.first().ordered()) + items.drop(1), from), 0, 0)
        c.prepare()
        c.play()
        _state.value = _state.value.copy(shuffle = true)
    }

    // Where these land is the core's (nori_player::playlist::Playlist::take): after the playing song, and
    // for "last" after the songs added by hand before them, whatever the shuffle order says.
    fun playNext(songs: List<Song>) = with { c ->
        if (mirror != null) return@with run { remote(Op.Add(songs, true)) }
        c.addMediaItems(items(songs).map { it.queued(Hand.NEXT) })
        if (c.playbackState == Player.STATE_IDLE) c.prepare()
    }

    fun enqueue(songs: List<Song>) = with { c ->
        if (mirror != null) return@with run { remote(Op.Add(songs, false)) }
        c.addMediaItems(items(songs).map { it.queued(Hand.LAST) })
        if (c.playbackState == Player.STATE_IDLE) c.prepare()
    }

    fun playRadio(station: RadioStation) = with { c ->
        c.setMediaItem(station.toMediaItem(context.getString(R.string.radio_artist)))
        c.prepare()
        c.play()
    }

    /** Runs [action] on the main thread once the service is up (connecting to it starts it). */
    fun connected(action: () -> Unit) = with { action() }

    fun skipTo(index: Int) = with { c ->
        mirror?.let { m -> remoteIndex(m, index)?.let { remote(Op.Jump(it, m.rev)) }; return@with }
        c.seekToDefaultPosition(index); if (c.playbackState == Player.STATE_IDLE) c.prepare(); c.play()
    }
    fun remove(index: Int) = with { c ->
        mirror?.let { m -> remoteIndex(m, index)?.let { remote(Op.Remove(it, m.rev)) }; return@with }
        c.removeMediaItem(index)
    }
    /** Undo of [remove]: [song] back where it was (the core's `playlist_restore`, or the mirrored device's), or at [index] if the core no longer has it. */
    fun restore(song: Song, index: Int) = with { c ->
        if (mirror != null) return@with run { nori.remotes.putBack(song.id) }
        c.addMediaItem(index.coerceIn(0, c.mediaItemCount), items(listOf(song)).single().restored())
        if (c.playbackState == Player.STATE_IDLE) c.prepare()
    }
    fun move(from: Int, to: Int) = with { c ->
        mirror?.let { m ->
            val a = remoteIndex(m, from)
            val b = remoteIndex(m, to)
            if (a != null && b != null) remote(Op.Move(a, b, m.rev))
            return@with
        }
        c.moveMediaItem(from, to)
    }
    fun clear() = with { it.clearMediaItems() }

    // ---- transport ----

    /** Also prepares a queue that was restored but never loaded. */
    fun toggle() = with { c ->
        mirror?.let { m -> remote(if (m.playing) Op.Pause else Op.Play); return@with }
        Util.handlePlayPauseButtonAction(c)
    }
    fun next() = with { c ->
        if (mirror != null) return@with run { remote(Op.Next) }
        _pendingSeek.value = null
        // With nothing after, the service refills the queue and takes the skip when songs land
        // (nori_player::transport::next_action).
        when (dev.nori.music.ffi.queue.nextAction(c.hasNextMediaItem())) {
            NextAction.SKIP -> c.seekToNextMediaItem()
            NextAction.FILL_THEN_SKIP -> c.sendCustomCommand(SessionCommand(PlaybackService.CMD_FILL_NEXT, Bundle.EMPTY), Bundle.EMPTY)
        }
    }
    /** A rewind is a seek to the top, not a skip; the rule mirrors the service's (media3 rewinds past three seconds). */
    fun previous() = with { c ->
        if (mirror != null) return@with run { remote(Op.Previous) }
        // Restart here, or let the player's own previous decide: nori_player::queue::previous_restarts.
        if (nori.session.queuePreviousRestarts(c.currentPosition, c.hasPreviousMediaItem())) { seekTo(0); if (!c.playWhenReady) c.play() }
        else { _pendingSeek.value = null; c.seekToPrevious() }
    }
    /** The song before, even well into this one - a swipe is a request for the other record, not a restart. */
    fun previousItem() = with { c ->
        mirror?.let { m -> _state.value.previousIndex.takeIf { it >= 0 }?.let { remoteIndex(m, it) }?.let { remote(Op.Jump(it, m.rev)) }; return@with }
        _pendingSeek.value = null
        c.seekToPreviousMediaItem()
    }

    /** A seek. The bar holds its place ([pendingSeek]) until the engine says it landed there ([followSeek]). */
    fun seekTo(ms: Long) = with { c ->
        if (mirror != null) return@with run { remote(Op.Seek(ms.coerceAtLeast(0))) }
        // Asked for: the bar and the lyrics go there as they are, even a moment back (heard.rs Playhead).
        PlayheadJni.jumped(clock)
        seekAfter = engine()?.let { it to it.jumpsSent }
        _pendingSeek.value = ms
        // A tap is a place in the song on the page. While the ear is still on the song the player
        // has left (see publish), that is the earlier song: the seek goes to it, not to the one the
        // player is already counting.
        val heardIndex = _state.value.index.takeIf { heard(c) && it >= 0 && it != c.currentMediaItemIndex }
        if (heardIndex != null) c.seekTo(heardIndex, ms) else c.seekTo(ms)
    }

    /** The engine and its jumps sent before the pending seek; null when there is no engine to wait for. */
    private var seekAfter: Pair<EnginePlayer, Long>? = null
    /**
     * Where a seek asked to go, until the engine has landed it. The seek bar holds this instead of its
     * own timer, so a slow seek reads as one held place rather than a jump, a snap-back and a glide.
     */
    private val _pendingSeek = MutableStateFlow<Long?>(null)
    val pendingSeek: StateFlow<Long?> = _pendingSeek
    private val main by lazy { android.os.Handler(android.os.Looper.getMainLooper()) }

    /**
     * Lets the bar go once the engine has landed a jump sent after the seek (its position event), or the
     * engine it was sent to is gone.
     */
    private fun followSeek() {
        if (_pendingSeek.value == null) return
        val e = engine()
        val (sentTo, after) = seekAfter ?: (null to 0L)
        if (e == null || e !== sentTo || (e.jumpsSent > after && e.landedAll)) _pendingSeek.value = null
    }

    fun setShuffle(on: Boolean) = with {
        if (mirror != null) return@with run { remote(Op.Shuffle(on)) }
        nori.session.playlistShowShuffle(on)
        it.shuffleModeEnabled = on
        _state.value = _state.value.copy(shuffle = on || it.shuffleModeEnabled)
    }

    fun cycleRepeat() = with { c ->
        mirror?.let { m -> remote(Op.Repeat(dev.nori.music.ffi.queue.queueNextRepeat(m.repeat))); return@with }
        c.repeatMode = dev.nori.music.ffi.queue.queueNextRepeat(c.repeatMode.toUByte()).toInt()
    }

    /** The app is in sight until [disconnect]: the service trades its deep audio buffer for a sound change heard at once. */
    fun inSight() = with { c ->
        c.sendCustomCommand(SessionCommand(PlaybackService.CMD_IN_SIGHT, Bundle.EMPTY), Bundle().apply { putBoolean(PlaybackService.ARG_ON, true) })
    }

    /** Presses one of the session's own buttons (the notification's heart or shuffle) the way the notification does; for the test bridge. */
    fun pressSessionButton(action: String) = with { it.sendCustomCommand(SessionCommand(action, Bundle.EMPTY), Bundle.EMPTY) }

    /** The notification's extra buttons as the session last published them, e.g. "heart_filled shuffle_off"; for the test bridge. */
    val sessionButtons: String get() = controller?.mediaButtonPreferences.orEmpty().joinToString(" ") {
        when (it.icon) {
            androidx.media3.session.CommandButton.ICON_HEART_FILLED -> "heart_filled"
            androidx.media3.session.CommandButton.ICON_HEART_UNFILLED -> "heart"
            androidx.media3.session.CommandButton.ICON_SHUFFLE_ON -> "shuffle_on"
            androidx.media3.session.CommandButton.ICON_SHUFFLE_OFF -> "shuffle_off"
            else -> it.sessionCommand?.customAction ?: "?"
        }
    }

    /** [minutes] 0 and [endOfTrack] false cancels. */
    fun sleep(minutes: Int, endOfTrack: Boolean = false, songs: Int = 0) = with { c ->
        c.sendCustomCommand(SessionCommand(PlaybackService.CMD_SLEEP, Bundle.EMPTY), Bundle().apply {
            putInt(PlaybackService.ARG_MINUTES, minutes); putBoolean(PlaybackService.ARG_END_OF_TRACK, endOfTrack); putInt(PlaybackService.ARG_SONGS, songs)
        })
        val shown = dev.nori.music.ffi.queue.sleepShown(minutes.coerceAtLeast(0).toUInt(), endOfTrack, songs.coerceAtLeast(0).toUInt(), SystemClock.elapsedRealtime())
        _state.value = _state.value.copy(sleepAt = shown.atMs, sleepAtEndOfTrack = shown.atEndOfTrack)
    }
}

/** Every seek bar reading logged (tag noripos), for a check frame by frame: the test bridge's "tracelyrics". */
@Volatile var tracePositions = false

/** A drifted controller is put right at most this often (PlayerConnection.heard). */
private const val REANCHOR_GAP_MS = 2_000L

/**
 * The seek bar's place over a [HeardJni] clock (crates/queue/src/heard.rs over nori_player::heard::Playhead):
 * asked every frame the bar is drawn, so primitives only.
 */
internal object PlayheadJni {
    init { System.loadLibrary("norimusic") }

    /**
     * As [HeardJni.at], with the place the bar shows while the page shows queue index [shown] (-1: nothing).
     * [positionMs] is the player's word; [player] the engine ([EnginePlayer.handle], 0: not asked), whose own
     * place in song [on] the bar goes by when it has one. Negative when the player's word has drifted from
     * the engine's place (nori_player::heard::drifted): the session must say its place again.
     */
    @JvmStatic @CriticalNative external fun position(h: Long, nowMs: Long, playing: Boolean, on: Int, positionMs: Long, shown: Int, player: Long): Long
    /** The last place shown, run on from then if [playing]: for while the controller cannot be asked. */
    @JvmStatic @CriticalNative external fun runOn(h: Long, nowMs: Long, playing: Boolean): Long
    /** The listener asked for a place (a seek): the next reading is shown as it is, even a moment back in the song. */
    @JvmStatic @CriticalNative external fun jumped(h: Long)
    /** The length the page shows: the heard song's ([heardS] seconds, -1 none), else the player's, else the tags'. */
    @JvmStatic @CriticalNative external fun durationMs(heardS: Long, playerMs: Long, taggedMs: Long): Long
}

/**
 * The core's queue (crates/queue/src/playlist.rs) where it is asked on every player event or edit:
 * primitives in and out, nothing copied.
 */
internal object PlaylistJni {
    init { System.loadLibrary("norimusic") }

    // [session] is the queue session's handle (`Nori.sessionHandle`).

    /** Changes whenever the list or its order does. */
    @JvmStatic @CriticalNative external fun rev(session: Long): Long
    /** Moves whenever a new queue is set, and with it perhaps the page it came from. */
    @JvmStatic @CriticalNative external fun origin(session: Long): Int
    /** Shuffle shown as on (the player's own, or a weighted shuffle's). */
    @JvmStatic @CriticalNative external fun shuffleShown(session: Long): Boolean
    /** The play order while shuffling, written into [out] when it is exactly that long; its length, -1 when not shuffling. */
    @JvmStatic @FastNative external fun order(session: Long, out: IntArray): Int
}
