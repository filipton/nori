package dev.nori.music.playback

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRouting
import android.media.AudioTrack
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.Player
import androidx.media3.common.SimpleBasePlayer
import androidx.media3.common.Timeline
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSource
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import dalvik.annotation.optimization.CriticalNative
import dalvik.annotation.optimization.FastNative
import dev.nori.music.Nori

/**
 * nori-engine, the Rust player (crates/android/src/player.rs): the core's queue played into an AudioTrack
 * the engine writes itself (crates/android/src/track.rs). Commands go in as primitives; the engine's
 * events come out through [RustBridge.signal] and [event], one wake of the main thread per batch.
 */
internal object RustPlayerJni {
    init { System.loadLibrary("norimusic") }
    /** [float] is the high quality output setting; [memoryMb] the app's memory class. 0 when it could not start. */
    @JvmStatic external fun create(sdk: Int, float: Boolean, memoryMb: Int): Long
    @JvmStatic external fun destroy(h: Long)
    /** Answers the jump's number, which the song events it leads to carry ([eventJumps]). */
    @JvmStatic @CriticalNative external fun goTo(h: Long, index: Int, ms: Long): Long
    @JvmStatic @CriticalNative external fun pauseAtEnd(h: Long, on: Boolean)
    @JvmStatic @CriticalNative external fun play(h: Long)
    @JvmStatic @CriticalNative external fun pause(h: Long)
    /** Pause at once, no fade (headphones pulled out). */
    @JvmStatic @CriticalNative external fun pauseNow(h: Long)
    /** The core's queue was edited (or reordered): the engine follows it. */
    @JvmStatic @CriticalNative external fun queueChanged(h: Long)
    @JvmStatic @CriticalNative external fun setRepeat(h: Long, mode: Int)
    @JvmStatic @CriticalNative external fun replan(h: Long)
    @JvmStatic @CriticalNative external fun gainChanged(h: Long)
    @JvmStatic @CriticalNative external fun setForeground(h: Long, on: Boolean)
    /** The sound and the controls' fades as the core's settings are now. */
    @JvmStatic @CriticalNative external fun applySettings(h: Long)
    /** Where the ear is in it now (the engine's `status().position_now()`), read when asked, never ticked. */
    @JvmStatic @CriticalNative external fun positionMs(h: Long): Long
    /** [positionMs] for the seek bar on screen in queue index [index] (-1: the engine is on another song): see `EnginePlayer.shownMs`. */
    @JvmStatic @CriticalNative external fun shownMs(h: Long, index: Int): Long
    /** The engine reads its output once, now. */
    @JvmStatic @CriticalNative external fun look(h: Long)
    @JvmStatic @CriticalNative external fun mixing(h: Long): Boolean
    @JvmStatic @CriticalNative external fun chainIn(h: Long): Boolean
    @JvmStatic @CriticalNative external fun onCpu(h: Long): Boolean
    @JvmStatic @CriticalNative external fun gainReductionDb(h: Long): Float
    @JvmStatic @CriticalNative external fun compressionDb(h: Long): Float
    @JvmStatic @CriticalNative external fun setVolume(h: Long, index: Int, max: Int, db: Float)
    @JvmStatic @CriticalNative external fun bytesWritten(h: Long): Long
    /** The next event, `kind shl 32 or index` (kind: state 0, song 1, error 2, output 3); -1 when there are no more. */
    @JvmStatic @CriticalNative external fun event(h: Long): Long
    /**
     * The words of the event [event] last gave: the error, the stream's title, the output's name. Short and
     * calling nothing back (the words sit behind a lock only the main thread takes, and one string is
     * made of them), so a fast door.
     */
    @JvmStatic @FastNative external fun eventText(h: Long): String?
    /** The jumps the engine had made when it said the song or loop [event] last gave (see `EnginePlayer.onSong`). */
    @JvmStatic @CriticalNative external fun eventJumps(h: Long): Long
    /** The track's route changed: [type] is `AudioDeviceInfo.TYPE_*`. */
    @JvmStatic external fun device(h: Long, type: Int, name: String?)
    /** What the engine cannot see of the output: something USB attached, a DAC playing bit-perfect. */
    @JvmStatic @CriticalNative external fun setOutput(h: Long, usb: Boolean, bitPerfect: Boolean)
    /** The offloaded track's stream event: 0 it wants more, 1 it played to the end of stream, 2 it was torn down. */
    @JvmStatic @CriticalNative external fun offloadEvent(h: Long, kind: Int)
    /** The songs go to the audio chip now. */
    @JvmStatic @CriticalNative external fun offloaded(h: Long): Boolean
    /** The settings and the output let the songs go to the audio chip. */
    @JvmStatic @CriticalNative external fun offloadWanted(h: Long): Boolean
    /**
     * Why the music plays on the CPU and not on the audio chip, in words; while it is offloaded, null, or
     * what the chip leaves in (a song's encoder delay and padding, where it does not do gapless offload).
     */
    @JvmStatic @FastNative external fun pcmWhy(h: Long): String?
    /** A radio station queued as [id], streaming at [url]. */
    @JvmStatic external fun radio(h: Long, id: String, url: String)
}

/**
 * What the Rust player asks of the platform, from its own threads: an AudioTrack, a song's bytes, the cache
 * key a song resolves to, and a wake for its events. Each is rare: a track per start, a body per burst of a
 * song, a key per song, a signal per batch of events.
 */
@UnstableApi
internal object RustBridge {
    @Volatile var player: EnginePlayer? = null

    /** [encoding] is `AudioFormat.ENCODING_*`: 16-bit, 24-bit packed (a song played as it is) or float. */
    @JvmStatic fun openTrack(rate: Int, channels: Int, encoding: Int, frames: Int): AudioTrack? = player?.openTrack(rate, channels, encoding, frames)
    /** [ticket]: the request's number, by which [cancel] calls it off (see [Tickets]). */
    @JvmStatic fun open(url: String, key: String, from: Long, ticket: Long): RustBody? =
        player?.open(url, key, from, ticket) ?: null.also { Tickets.end(ticket) }
    /** The Rust side lets a request go that has not answered or sends nothing: its call is cancelled. From any of its threads. */
    @JvmStatic fun cancel(ticket: Long) = Tickets.cancel(ticket)
    @JvmStatic fun openLive(url: String): RustBody? = player?.openLive(url)
    /**
     * For the songs fetched ahead (nori-engine's one fetcher, through [open]): whether all of [key] is in the
     * stream cache, and whether the player is writing it now. Asked once per song; with no player, as there
     * and busy, so nothing is fetched.
     */
    @JvmStatic fun kept(key: String): Boolean = player?.kept(key) ?: true
    @JvmStatic fun busy(key: String): Boolean = player?.busy(key) ?: true
    /** What the stream cache keeps of [key], in words: for the perf build's break of a player that plays nothing. */
    @JvmStatic fun disk(key: String): String = player?.disk(key) ?: "$key: no player"
    /**
     * [key] played nothing and is fetched anew (crates/engine `Library::forget`): its stream cache entry goes.
     * What it kept is answered, in words, for the log.
     */
    @JvmStatic fun forget(key: String): String = player?.forget(key) ?: "$key: no player"
    /**
     * Whether the audio chip decodes [encoding] where the music goes now, as the platform answers media3:
     * the call made in the high byte (3 `getDirectPlaybackSupport`, 2 `getPlaybackOffloadSupport`, 1
     * `isOffloadedPlaybackSupported`) and its answer in the low one; -1 when it could not be asked. The
     * Rust side reads it (crates/android/src/player.rs `offload_support`).
     */
    @JvmStatic fun offloadSupport(encoding: Int, rate: Int, channels: Int): Int = player?.offloadSupport(encoding, rate, channels) ?: -1
    @JvmStatic fun openOffload(encoding: Int, rate: Int, channels: Int, bytes: Int): AudioTrack? = player?.openOffload(encoding, rate, channels, bytes)
    /** Whether a player took it: none registered yet, the engine signals again with its next event. */
    @JvmStatic fun signal(): Boolean = player?.let { it.signal(); true } ?: false
    /**
     * The engine needs the CPU kept awake ([awake]), or can let it sleep while the audio chip plays the
     * songs (nori-engine's `Event::Awake`). Called on the engine's thread as it changes, before the
     * engine goes on.
     */
    @JvmStatic fun cpu(awake: Boolean) { player?.engineAwake(awake) }
}

/**
 * A song's bytes from [from] on, read by the Rust player's loader a buffer at a time. [length] is -1 when
 * unknown; [icy] is a station's stream's bytes of music between two announcements, 0 when it sends none.
 * [past]: asked for from past the song's end, so there are no bytes, and [length] is the whole song's
 * when the server said it (-1 when not): the Rust side takes it as the song's real end
 * (crates/engine/src/source.rs `OpenError::PastEnd`). [status]: the server answered with this error
 * status instead of bytes; it was reached, so the song's failure is not the network's (`OpenError::Status`).
 */
class RustBody internal constructor(
    private val source: DataSource?,
    @JvmField val length: Long,
    @JvmField val icy: Int = 0,
    @JvmField val past: Boolean = false,
    @JvmField val status: Int = 0,
    private val done: () -> Unit,
) {
    /** As large as the loader's reads (crates/engine/src/source.rs CHUNK); none for an answer without bytes. */
    @JvmField val buffer = ByteArray(if (source == null) 0 else 256 * 1024)
    private var broke = false

    /**
     * Bytes read into [buffer], at most [max]: as many as come until it is full or the song ends, so the
     * loader crosses over once per quarter megabyte rather than once per network packet (a read from the
     * network gives at most what one packet brought). -1 at the end, -2 when the connection broke; what
     * came before it broke is handed over first.
     */
    fun read(max: Int): Int {
        if (broke) return -2
        val source = source ?: return -1
        val want = minOf(max, buffer.size)
        var got = 0
        try {
            while (got < want) {
                val n = source.read(buffer, got, want - got)
                if (n == C.RESULT_END_OF_INPUT) break
                got += n
            }
        } catch (e: Exception) {
            dev.nori.music.NoriLog.w("rust player: the song's bytes stopped coming: $e")
            broke = true
            if (got == 0) return -2
        }
        return if (got == 0) -1 else got
    }

    fun close() {
        runCatching { source?.close() }
        done()
    }
}

/**
 * The Rust player as media3 sees it, so the session, the notification, the lock screen, headset buttons,
 * Android Auto and the widget follow it as they would any media3 player, through [PlaybackService]'s `Controls`.
 * The queue is the core's: `Controls` edits it there first, and this keeps the items the session shows
 * and tells the engine. The song shown is the one heard (the engine's song events, which come through a
 * mix at the moment the next song is audible); the position is the engine's, read when asked.
 *
 * What ExoPlayer does around the player and the engine does not, this does as ExoPlayer would: audio
 * focus, pausing when headphones are pulled out, the CPU wake lock while music plays. The player's own
 * rules - a seek or a skip while paused is held until play, so nothing is fetched until the music is
 * wanted; the sleep timer's pause at the end of a song - are nori-engine's (`Engine::go_to`,
 * `Engine::pause_at_end`), as a desktop client gets them.
 */
@UnstableApi
class EnginePlayer(private val context: Context, private val nori: Nori) : SimpleBasePlayer(Looper.getMainLooper()) {
    private val main = Handler(Looper.getMainLooper())
    /**
     * The engine's handle; 0 once released. Every door takes it as it is at the call, and the Rust side
     * answers 0 - or a handle that is gone - with nothing, so a routing callback, a test's read or a
     * posted settings change that arrives after the release does no harm.
     */
    @Volatile private var h: Long = RustPlayerJni.create(
        Build.VERSION.SDK_INT, nori.settings.value.hiRes,
        context.getSystemService(android.app.ActivityManager::class.java).memoryClass,
    )

    private val items = ArrayList<MediaItem>()
    private val uids = ArrayList<Long>()
    private var nextUid = 0L
    /** Changes with every edit, for the timeline kept below. */
    private var edits = 0

    /** The song shown: the one heard, or the one a seek asked for while paused (which the engine holds). */
    private var current = 0
    /** A song the engine was sent to, whose song event is that jump, not a song ending. */
    private var expecting = -1
    /**
     * The number of the last jump the engine was sent. A song event said before the engine made it is
     * from the place already left: pressed quickly, next went 1, 2 and the engine's word that 1 was heard
     * (said as it got there, taken here after the second press) put the page back on 1, and then on 2
     * again as a song ending by itself - two changes more than were asked for.
     */
    private var sent = 0L
    /** The number of the last jump the engine said it landed (its position event). */
    private var landed = 0L
    private var prepared = false
    private var playWhenReady = false
    private var whyPlayWhenReady = Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST
    private var suppressed = Player.PLAYBACK_SUPPRESSION_REASON_NONE
    private var engineState = ENGINE_IDLE
    private var repeat = Player.REPEAT_MODE_OFF
    private var shuffle = false
    private var error: PlaybackException? = null
    /** The engine's words for the last song that would not play, for the error a stop after it shows. */
    private var lastError: String? = null
    /** The engine waits for a song's bytes with nothing left to play (its buffering event). */
    private var buffering = false
    /** The engine moved on by itself (a song ended into the next): said once, in the next state. */
    private var moved = false
    /**
     * The engine moved the music to another path (the audio chip to the CPU or back) without a jump asked
     * for: the next state says the place again as a discontinuity, so the session and its controllers
     * anchor their clocks there, as they do after a pause or a seek, rather than running on from an
     * older word.
     */
    private var placed = false
    @Volatile private var loading = 0
    /** Pause when the song playing ends (the sleep timer's "end of this song"): the engine does it. */
    var pauseAtEndOfItem = false
        set(on) { field = on; RustPlayerJni.pauseAtEnd(h, on) }
    /** Times the song playing started again by itself (repeat one); see [position]. */
    private var loops = 0
    /** What the radio station playing announced (ICY), shown as the song's title as ExoPlayer shows it. */
    private var announced: String? = null
    /**
     * A song the network would not bring, with the offline bridge's setting on: the service hands it to
     * the bridge, and says whether the music goes on (it jumped somewhere to play) or stops here.
     */
    var onBridge: (() -> Boolean)? = null
    /** The speed and pitch the engine plays at, as media3 is told them. */
    private var parameters = nori.settings.value.let { PlaybackParameters(it.speed, it.pitch) }

    // ---- what the service asks of it directly ----

    /**
     * The settings changed: the engine takes them from the core. The speed and pitch are said to media3
     * as well, which runs the session's and the controllers' position on at that pace between readings.
     */
    fun applySettings() {
        RustPlayerJni.applySettings(h)
        val p = nori.settings.value.let { PlaybackParameters(it.speed, it.pitch) }
        if (p != parameters) { parameters = p; invalidateState() }
    }
    fun gainChanged() = RustPlayerJni.gainChanged(h)
    /** The equalizer's screen is open: the engine trades its deep buffer for a shallow one until the next boundary after it closes. */
    fun setForeground(on: Boolean) = RustPlayerJni.setForeground(h, on)
    fun replan() = RustPlayerJni.replan(h)
    val mixing: Boolean get() = RustPlayerJni.mixing(h)
    /** The sound chain is in the samples' path, and what its limiter takes off, dB: see [Equalizer.inChain]. */
    val chainIn: Boolean get() = RustPlayerJni.chainIn(h)
    /** The ear is on music the CPU made, through the engine's own output: where [chainIn] says anything. */
    val onCpu: Boolean get() = RustPlayerJni.onCpu(h)
    val gainReductionDb: Float get() = RustPlayerJni.gainReductionDb(h)
    /** The compressor's meter: what it takes off, dB. */
    val compressionDb: Float get() = RustPlayerJni.compressionDb(h)
    /** The music volume, for loudness compensation: the core applies it only when it moves the sound. */
    fun setVolume(index: Int, max: Int, db: Float) = RustPlayerJni.setVolume(h, index, max, db)
    val bytesWritten: Long get() = RustPlayerJni.bytesWritten(h)
    /** The songs go to the audio chip now, and whether the settings and the output let them. */
    val offloaded: Boolean get() = RustPlayerJni.offloaded(h)
    val offloadWanted: Boolean get() = RustPlayerJni.offloadWanted(h)
    /** Why the music is on the CPU rather than the audio chip, for the perf report. */
    val pcmWhy: String? get() = RustPlayerJni.pcmWhy(h)
    /** What the engine cannot see of the output: something USB attached (the chip cannot reach it), a DAC playing bit-perfect. */
    fun setOutput(usb: Boolean, bitPerfect: Boolean) = RustPlayerJni.setOutput(h, usb, bitPerfect)

    /**
     * The engine's own place in the song at queue index [index], for the seek bar on screen; -1 when the
     * engine is on another song. Its last reading run on for two seconds at most, the engine asked to read
     * its output again whenever that reading is a second old (nori_player::heard::screen_place).
     */
    fun shownMs(index: Int): Long = RustPlayerJni.shownMs(h, index)

    /** Jumps sent to the engine so far: a seek asked after this reading is sent as a later one. */
    val jumpsSent: Long get() = sent
    /** Whether the engine has landed every jump sent to it. */
    val landedAll: Boolean get() = landed >= sent

    /** The engine for a door that reads it itself (PlayheadJni.position); 0 once released. */
    internal val handle: Long get() = h

    /** The engine reads its output once, now: the screen is coming back (its last wake may be minutes old). */
    fun look() = RustPlayerJni.look(h)
    /**
     * The session and its controllers run their place on from its last word, and media3 lets them run on
     * until a play, a pause or a seek says it again (periodic updates are off). One that drifted from the
     * engine's own place is put right here: the place said again as a discontinuity, as after a change of path.
     */
    fun reanchor() {
        placed = true
        invalidateState()
    }

    // ---- the state media3 reads ----

    /**
     * The position, read when asked. Each state has its own, so that the one before a repeat-one loop
     * reads as the end of the song and the one after as its start: media3 tells a loop (a transition
     * with the reason REPEAT) from the song's own place going back.
     */
    private fun position(round: Int): PositionSupplier {
        // The song's end, as the state before a loop last heard it.
        val end = items.getOrNull(current)?.mediaMetadata?.durationMs?.takeIf { it > 0 } ?: (Long.MAX_VALUE / 4)
        return PositionSupplier { if (round == loops) RustPlayerJni.positionMs(h) else end }
    }
    private var timeline: QueueTimeline? = null
    private var timelineAt = Pair(-1, -1L)

    /** The list with the core's play order under shuffle, built again only when either changed. */
    private fun timeline(): QueueTimeline {
        val at = Pair(edits, if (shuffle) PlaylistJni.rev() else -1L)
        timeline?.takeIf { timelineAt == at }?.let { return it }
        val order = if (shuffle && items.isNotEmpty()) IntArray(items.size).takeIf { PlaylistJni.order(it) == items.size } else null
        return QueueTimeline(ArrayList(items), uids.toLongArray(), order).also { timeline = it; timelineAt = at }
    }

    private fun playbackState(): Int = when {
        !prepared || error != null -> Player.STATE_IDLE
        items.isEmpty() || engineState == ENGINE_ENDED -> Player.STATE_ENDED
        // The music ran out waiting for the network: what ExoPlayer says then, and what a screen shows as loading.
        buffering && playWhenReady -> Player.STATE_BUFFERING
        else -> Player.STATE_READY
    }

    override fun getState(): State {
        val b = State.Builder()
            .setAvailableCommands(COMMANDS)
            .setPlayWhenReady(playWhenReady, whyPlayWhenReady)
            .setPlaybackState(playbackState())
            .setPlaybackSuppressionReason(suppressed)
            .setRepeatMode(repeat)
            .setShuffleModeEnabled(shuffle)
            .setIsLoading(loading > 0)
            .setAudioAttributes(ATTRIBUTES)
            .setPlaylist(timeline(), Tracks.EMPTY, announcement())
            .setCurrentMediaItemIndex(if (items.isEmpty()) C.INDEX_UNSET else current.coerceIn(0, items.size - 1))
            .setContentPositionMs(position(loops))
            .setPlaybackParameters(parameters)
        error?.let { b.setPlayerError(it) }
        if (moved) {
            moved = false
            placed = false
            b.setPositionDiscontinuity(Player.DISCONTINUITY_REASON_AUTO_TRANSITION, 0)
        } else if (placed) {
            placed = false
            b.setPositionDiscontinuity(Player.DISCONTINUITY_REASON_INTERNAL, RustPlayerJni.positionMs(h))
        }
        return b.build()
    }

    /** The radio station playing, titled with what it announced, as ExoPlayer merges ICY into the metadata; null otherwise. */
    private fun announcement(): androidx.media3.common.MediaMetadata? {
        val said = announced ?: return null
        val item = items.getOrNull(current)?.takeIf { it.isRadio } ?: return null
        return item.mediaMetadata.buildUpon().setTitle(said).build()
    }

    // ---- commands ----

    private fun done(): ListenableFuture<*> = Futures.immediateVoidFuture()

    /**
     * Whether music may sound now: prepared, wanted, and no call (a transient loss of focus) under way.
     * Everything that would start the engine asks this first; while a call holds the focus the engine
     * stays paused, and holds any place asked for until the focus comes back.
     */
    private fun audible(): Boolean = prepared && playWhenReady && suppressed == Player.PLAYBACK_SUPPRESSION_REASON_NONE

    /** The engine plays on from where it is, or from where the list or a seek left it (which it held). */
    private fun start() {
        if (items.isEmpty() || h == 0L) return
        RustPlayerJni.play(h)
    }

    /** The engine goes to [index] at [ms], playing or paused as it is: paused, it holds the place until play. */
    private fun goTo(index: Int, ms: Long) {
        if (index != current) expecting = index
        current = index
        jumped(RustPlayerJni.goTo(h, index, ms))
    }

    /** The engine was sent jump [n] (0: nothing was sent). */
    private fun jumped(n: Long) {
        if (n > 0) sent = n
    }

    override fun handleSetPlayWhenReady(playWhenReady: Boolean): ListenableFuture<*> {
        // Asked again on every play, not only when it was never held: play pressed during a call must
        // not sound over it, and the system refuses the focus until the call is over.
        if (playWhenReady && !focus(again = true)) return done().also { dev.nori.music.NoriLog.w("rust player: play refused, the audio focus was not granted") }
        // Every change of "wants to play" is in the log with its cause: a page that reads paused over music
        // playing is told apart from the engine's word, a call, or headphones.
        if (playWhenReady != this.playWhenReady) dev.nori.music.NoriLog.i("rust player: ${if (playWhenReady) "play" else "pause"} asked for")
        this.playWhenReady = playWhenReady
        whyPlayWhenReady = Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST
        suppressed = Player.PLAYBACK_SUPPRESSION_REASON_NONE
        if (playWhenReady) {
            if (prepared) start()
        } else {
            RustPlayerJni.pause(h)
            unfocus()
        }
        follow()
        return done()
    }

    override fun handlePrepare(): ListenableFuture<*> {
        prepared = true
        if (h != 0L) clearError()
        if (audible() && focus()) start()
        follow()
        return done()
    }

    /** Stopped: the engine pauses and keeps its place, to start from after the next prepare. */
    override fun handleStop(): ListenableFuture<*> {
        RustPlayerJni.pause(h)
        prepared = false
        unfocus()
        follow()
        return done()
    }

    override fun handleRelease(): ListenableFuture<*> {
        unwatchNetwork()
        unfocus()
        follow(released = true)
        if (RustBridge.player === this) RustBridge.player = null
        main.removeCallbacks(drain)
        val handle = h
        h = 0L
        RustPlayerJni.destroy(handle)
        return done()
    }

    override fun handleSetRepeatMode(repeatMode: Int): ListenableFuture<*> {
        repeat = repeatMode
        RustPlayerJni.setRepeat(h, repeatMode)
        return done()
    }

    /** The core already put the songs in their new order (`Controls`); the engine and the timeline follow it. */
    override fun handleSetShuffleModeEnabled(shuffleModeEnabled: Boolean): ListenableFuture<*> {
        shuffle = shuffleModeEnabled
        edited()
        return done()
    }

    override fun handleSetMediaItems(mediaItems: MutableList<MediaItem>, startIndex: Int, startPositionMs: Long): ListenableFuture<*> {
        val was = items.getOrNull(current)?.mediaId
        items.clear()
        uids.clear()
        for (item in mediaItems) { items += item; uids += nextUid++ }
        stations(mediaItems)
        edited()
        val at = if (startIndex == C.INDEX_UNSET || items.isEmpty()) timeline().getFirstWindowIndex(shuffle).coerceAtLeast(0) else startIndex.coerceIn(0, items.size - 1)
        if (items.isEmpty()) { current = 0; RustPlayerJni.pause(h); return done() }
        // The song playing, kept in a queue made around it: its entry stays (`Playlist::set`), so the
        // engine carries on with it, playing or paused, with no jump.
        if (items[at].isKept() && items[at].mediaId == was) { current = at; return done() }
        // Playing, the new list plays at once, as ExoPlayer's does; paused, the engine holds its start.
        expecting = at
        current = at
        jumped(RustPlayerJni.goTo(h, at, if (startPositionMs == C.TIME_UNSET) 0 else startPositionMs))
        if (audible()) start()
        return done()
    }

    override fun handleAddMediaItems(index: Int, mediaItems: MutableList<MediaItem>): ListenableFuture<*> {
        val at = index.coerceIn(0, items.size)
        val had = items.isNotEmpty()
        items.addAll(at, mediaItems)
        uids.addAll(at, List(mediaItems.size) { nextUid++ })
        if (had && at <= current) current += mediaItems.size
        stations(mediaItems)
        edited()
        return done()
    }

    /** A radio station's address is its item's own (the core's queue keeps ids only): handed to the engine as it is queued. */
    private fun stations(added: List<MediaItem>) {
        for (item in added) {
            if (!item.isRadio) continue
            val url = (item.localConfiguration?.uri ?: item.requestMetadata.mediaUri)?.toString() ?: continue
            RustPlayerJni.radio(h, item.mediaId, url)
        }
    }

    override fun handleRemoveMediaItems(fromIndex: Int, toIndex: Int): ListenableFuture<*> {
        val to = toIndex.coerceAtMost(items.size)
        if (fromIndex >= to) return done()
        val gone = current in fromIndex until to
        items.subList(fromIndex, to).clear()
        uids.subList(fromIndex, to).clear()
        when {
            current >= to -> current -= to - fromIndex
            gone -> current = fromIndex.coerceAtMost(items.size - 1).coerceAtLeast(0)
        }
        edited()
        if (items.isEmpty()) {
            RustPlayerJni.pause(h)
        } else if (gone) {
            // The song playing went: the one after it plays (or waits, paused), as media3 moves on.
            expecting = current
            jumped(RustPlayerJni.goTo(h, current, 0))
        }
        return done()
    }

    override fun handleMoveMediaItems(fromIndex: Int, toIndex: Int, newIndex: Int): ListenableFuture<*> {
        val on = uids.getOrNull(current)
        val moving = items.subList(fromIndex, toIndex).toList()
        val movingUids = uids.subList(fromIndex, toIndex).toList()
        items.subList(fromIndex, toIndex).clear()
        uids.subList(fromIndex, toIndex).clear()
        val at = newIndex.coerceIn(0, items.size)
        items.addAll(at, moving)
        uids.addAll(at, movingUids)
        on?.let { u -> current = uids.indexOf(u).coerceAtLeast(0) }
        edited()
        return done()
    }

    /**
     * A seek, a skip or a tap on a song of the queue. Playing, the engine goes there at once (with its own
     * dip); paused, it holds the place until play, as a paused ExoPlayer fetches nothing either
     * (`Engine::go_to`). A seek in the song heard is a jump to it as well: in the last seconds of a song
     * the engine is already reading the next one, and its own seek would land there.
     */
    override fun handleSeek(mediaItemIndex: Int, positionMs: Long, seekCommand: Int): ListenableFuture<*> {
        if (items.isEmpty()) return done()
        val target = if (mediaItemIndex == C.INDEX_UNSET) current else mediaItemIndex.coerceIn(0, items.size - 1)
        goTo(target, if (positionMs == C.TIME_UNSET) 0 else positionMs.coerceAtLeast(0))
        return done()
    }

    private fun edited() {
        edits++
        RustPlayerJni.queueChanged(h)
    }

    // ---- the engine's events ----

    /** Called on one of the engine's threads: its events are taken on the main thread. */
    internal fun signal() { main.post(drain) }

    private val drain = Runnable {
        while (true) {
            val e = RustPlayerJni.event(h)
            if (e < 0) break
            val arg = e.toInt()
            when ((e ushr 32).toInt()) {
                EVENT_STATE -> onState(arg)
                EVENT_SONG -> onSong(arg, RustPlayerJni.eventJumps(h))
                EVENT_ERROR -> RustPlayerJni.eventText(h).let { lastError = it; "rust player error: $it".let { t -> dev.nori.music.NoriLog.w(t); PlaybackService.observer?.error(t) } }
                EVENT_STOPPED -> stoppedByItself()
                EVENT_BUFFERING -> buffering = arg != 0
                EVENT_LOOPED -> onLoop(arg, RustPlayerJni.eventJumps(h))
                EVENT_TITLE -> announced = RustPlayerJni.eventText(h)
                // A mix began or ended being heard: the page is nudged, and reads [mixing] then. Nothing
                // else changes, so nothing else is said.
                EVENT_MIXING -> PlaybackService.onMixingChanged?.invoke()
                EVENT_PLACED -> placed = true
                // The engine is where a jump or seek asked: the session says that place, not its own guess.
                EVENT_LANDED -> { landed = RustPlayerJni.eventJumps(h); placed = true; PlaybackService.onLanded?.invoke() }
                // Handed on after the batch: the bridge edits and seeks this player itself.
                EVENT_BRIDGE -> main.post { if (onBridge?.invoke() != true) { stoppedByItself(); follow(); invalidateState() } }
                // The output device's own sound is DeviceSound's, from Outputs: its name is not asked for,
                // which would only make a string to throw away.
                else -> {}
            }
        }
        follow()
        invalidateState()
    }

    private fun onState(state: Int) {
        engineState = state
        when {
            // Played to the end of the queue: ExoPlayer keeps wanting to play, and says it ended.
            state == ENGINE_ENDED -> {}
            // Idle after it was started is the output failing: it would not open, or died and would not open again.
            state == ENGINE_IDLE -> {
                error = PlaybackException("the audio output would not open", null, PlaybackException.ERROR_CODE_AUDIO_TRACK_INIT_FAILED)
            }
            // Music again: whatever stopped it before is over.
            state == ENGINE_PLAYING -> clearError()
        }
    }

    /**
     * Watches for the network while the error shown is that the server could not be reached, and only
     * then: the network coming back clears it, so the page does not keep saying so over a player that
     * can play again. Registered with the error, let go with it.
     */
    private var networkWatch: android.net.ConnectivityManager.NetworkCallback? = null

    private fun clearError() {
        error = null
        unwatchNetwork()
    }

    private fun watchNetwork() {
        if (networkWatch != null) return
        val connectivity = context.getSystemService(android.net.ConnectivityManager::class.java)
        // The network the failure happened on is told at once as the callback registers; only another
        // one, or this one after it was lost, is the network coming back.
        val failedOn = runCatching { connectivity.activeNetwork }.getOrNull()
        val cb = object : android.net.ConnectivityManager.NetworkCallback() {
            @Volatile private var lost = failedOn == null
            override fun onLost(network: android.net.Network) {
                lost = true
            }
            override fun onAvailable(network: android.net.Network) {
                if (!lost && network == failedOn) return
                main.post {
                    if (error?.errorCode != PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED) return@post
                    dev.nori.music.NoriLog.i("rust player: the network is back, the failure it left goes")
                    clearError()
                    invalidateState()
                }
            }
        }
        runCatching { connectivity.registerDefaultNetworkCallback(cb) }
            .onSuccess { networkWatch = cb }
    }

    private fun unwatchNetwork() {
        val cb = networkWatch ?: return
        networkWatch = null
        runCatching { context.getSystemService(android.net.ConnectivityManager::class.java).unregisterNetworkCallback(cb) }
    }

    /**
     * The engine stopped by itself (the queue's rules after songs that would not play), which it says
     * apart from a pause it was asked for: guessed from the pause alone, every pause asked for - a stop
     * before a prepare, a call - turned wanting to play off as well.
     */
    private fun stoppedByItself() {
        pauseAtEndOfItem = false
        // After a run of songs that would not play (not the sleep timer's end of a song, which leaves no
        // failure behind): the player's error, as ExoPlayer's is, so the page says why the music stopped.
        // The words are the core's (PlayerConnection reads the kind from the code); play prepares again.
        dev.nori.music.ffi.queue.queueLastError()?.let { kind ->
            val code = when (kind) {
                dev.nori.music.ffi.model.PlaybackError.NETWORK -> PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED
                dev.nori.music.ffi.model.PlaybackError.OUTPUT -> PlaybackException.ERROR_CODE_AUDIO_TRACK_INIT_FAILED
                dev.nori.music.ffi.model.PlaybackError.OTHER -> PlaybackException.ERROR_CODE_DECODING_FAILED
            }
            error = PlaybackException(lastError ?: "a song would not play", null, code)
            if (kind == dev.nori.music.ffi.model.PlaybackError.NETWORK) watchNetwork()
        }
        if (!playWhenReady) return
        dev.nori.music.NoriLog.i("rust player: the engine stopped by itself: no longer wants to play")
        playWhenReady = false
        whyPlayWhenReady = Player.PLAY_WHEN_READY_CHANGE_REASON_END_OF_MEDIA_ITEM
    }

    /** [i]: where the song's entry is in the queue now (see `event`); -1 when an edit took it out. */
    private fun onSong(i: Int, jumps: Long) {
        // Said before the engine made the last jump sent: the page is already where that jump goes.
        if (jumps < sent || i !in items.indices) return
        val asked = i == expecting
        expecting = -1
        if (i != current) announced = null
        current = i
        if (asked) return
        // A song ending into the next one.
        moved = true
    }

    /** The song playing started again by itself (repeat one): a transition media3 reports as a repeat. */
    private fun onLoop(index: Int, jumps: Long) {
        if (jumps < sent || index !in items.indices) return
        current = index
        loops++
        moved = true
    }

    // ---- the platform around the player, as ExoPlayer runs it ----

    private val audio = context.getSystemService(AudioManager::class.java)
    private val focusRequest = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
        .setAudioAttributes(PLATFORM_ATTRIBUTES)
        .setOnAudioFocusChangeListener({ change -> onFocus(change) }, main)
        .build()
    private var focused = false

    private fun focus(again: Boolean = false): Boolean {
        if (!focused || again) focused = audio.requestAudioFocus(focusRequest) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        return focused
    }

    private fun unfocus() {
        if (focused) audio.abandonAudioFocusRequest(focusRequest)
        focused = false
    }

    /** A call or another player: paused for good, or until it is over. Ducking the system does itself. */
    private fun onFocus(change: Int) {
        dev.nori.music.NoriLog.i("rust player: audio focus $change, wants to play $playWhenReady")
        when (change) {
            AudioManager.AUDIOFOCUS_LOSS -> {
                focused = false
                if (playWhenReady) {
                    RustPlayerJni.pause(h)
                    playWhenReady = false
                    whyPlayWhenReady = Player.PLAY_WHEN_READY_CHANGE_REASON_AUDIO_FOCUS_LOSS
                }
            }
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> if (playWhenReady) {
                RustPlayerJni.pause(h)
                suppressed = Player.PLAYBACK_SUPPRESSION_REASON_TRANSIENT_AUDIO_FOCUS_LOSS
            }
            AudioManager.AUDIOFOCUS_GAIN -> if (suppressed != Player.PLAYBACK_SUPPRESSION_REASON_NONE) {
                suppressed = Player.PLAYBACK_SUPPRESSION_REASON_NONE
                if (playWhenReady) start()
            }
        }
        follow()
        invalidateState()
    }

    /**
     * Headphones pulled out: paused, as ExoPlayer's `setHandleAudioBecomingNoisy` does, and at once
     * whatever the fade on pause is, as a fade would play on out of the speaker.
     */
    private val noisy = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (!playWhenReady) return
            dev.nori.music.NoriLog.i("rust player: the audio became noisy: paused")
            RustPlayerJni.pauseNow(h)
            playWhenReady = false
            whyPlayWhenReady = Player.PLAY_WHEN_READY_CHANGE_REASON_AUDIO_BECOMING_NOISY
            follow()
            invalidateState()
        }
    }
    private var listening = false

    @Suppress("DEPRECATION")
    private val wakeLock = context.getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nori:engine").apply { setReferenceCounted(false) }

    /** Music is wanted and can come ([follow]): the CPU lock is held for it, unless the engine lets the CPU sleep. */
    @Volatile private var wantsCpu = false
    /**
     * The engine said it can let the CPU sleep (nori-engine's `Event::Awake(false)`): the songs are on the
     * audio chip, fed, and nothing but the platform's word is due. The chip plays from its own buffer
     * without the CPU; when it wants more, the platform's `onDataRequest` wakes the engine's thread
     * (audioserver holds its own wake lock while it runs the offloaded track and calls back). It is the
     * rule media3 plays offload by: `ExoPlayerImpl` lets its wake lock go while it sleeps for offload and
     * waits for the same callback. The engine takes the lock back ([engineAwake] true) before any work of
     * its own: a control, a song starting or its bytes awaited, a fade, a song or the end of the music
     * coming up (its volume and its event come on time), a count to look at again, the CPU path.
     */
    @Volatile private var engineAsleep = false

    /** The engine's word ([RustBridge.cpu]), on its thread. */
    internal fun engineAwake(awake: Boolean) {
        engineAsleep = !awake
        holdCpu()
    }

    /**
     * The receiver and the CPU lock are held exactly while music is wanted, as ExoPlayer's WAKE_MODE_LOCAL -
     * and can come: an output that would not open leaves the player wanting music it cannot play, and
     * held on through that, the lock kept the phone awake for nothing.
     */
    private fun follow(released: Boolean = false) {
        val playing = !released && playWhenReady && prepared && error == null && engineState != ENGINE_ENDED
        if (playing && !listening) {
            val filter = IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY)
            if (Build.VERSION.SDK_INT >= 33) context.registerReceiver(noisy, filter, Context.RECEIVER_NOT_EXPORTED) else context.registerReceiver(noisy, filter)
            listening = true
        } else if (!playing && listening) {
            context.unregisterReceiver(noisy)
            listening = false
        }
        wantsCpu = playing
        holdCpu()
    }

    /**
     * The CPU lock, held while music is wanted, but for while the engine lets the CPU sleep with no song's
     * bytes being fetched (a fetch runs on the loader's threads, which the engine's word does not cover).
     * From the main thread, the engine's and the loaders'.
     */
    @Synchronized private fun holdCpu() {
        val hold = wantsCpu && (!engineAsleep || loading > 0)
        if (hold == wakeLock.isHeld) return
        if (hold) wakeLock.acquire() else wakeLock.release()
        PlaybackService.observer?.wakeLock(hold)
    }

    // ---- what the Rust side asks for (RustBridge) ----

    /**
     * The engine's AudioTrack: music attributes, a deep buffer in the power-saving mode (the framework's deep-buffer output, which wakes least), pinned to a DAC
     * when bit-perfect is on, and the route told to the engine whenever it changes. Called on the engine's
     * thread.
     */
    internal fun openTrack(rate: Int, channels: Int, encoding: Int, frames: Int): AudioTrack? = runCatching {
        val width = when (encoding) { AudioFormat.ENCODING_PCM_FLOAT -> 4; AudioFormat.ENCODING_PCM_24BIT_PACKED -> 3; else -> 2 }
        // The DAC's mixer attributes are read by the framework when the track is built: set for this format first.
        nori.dac.onFormat(rate, encoding)
        val bitPerfect = nori.dac.state.value.bitPerfect
        // Always opened deep, in power saving mode: the shallow buffer while the app is in sight is the same
        // track made smaller in place (setBufferSizeInFrames, crates/android/src/track.rs), not another one.
        val mode = if (bitPerfect) AudioTrack.PERFORMANCE_MODE_NONE else AudioTrack.PERFORMANCE_MODE_POWER_SAVING
        val track = AudioTrack.Builder()
            .setAudioAttributes(PLATFORM_ATTRIBUTES)
            .setAudioFormat(AudioFormat.Builder().setSampleRate(rate).setEncoding(encoding).setChannelMask(mask(channels)).build())
            .setTransferMode(AudioTrack.MODE_STREAM)
            .setBufferSizeInBytes(frames * channels * width)
            .setPerformanceMode(mode)
            .build()
        // Started with a quarter of a second in it rather than once full: a seek is heard as soon as its
        // first burst is decoded. Before Android 12 a track waits to be full, and the engine fills it.
        if (Build.VERSION.SDK_INT >= 31) runCatching { track.setStartThresholdInFrames(minOf(rate / 4, track.bufferSizeInFrames)) }
        nori.dac.preferredDevice()?.let { runCatching { track.setPreferredDevice(it) } }
        nori.dac.onTrack(rate, encoding, false)
        track.addOnRoutingChangedListener(AudioRouting.OnRoutingChangedListener { r ->
            r.routedDevice?.let { d -> RustPlayerJni.device(h, d.type, d.productName?.toString()) }
        }, main)
        // The perf build's self test plays quietly; the engine's own volumes are scaled from its next one on.
        if (Quiet.level < 1f) track.setVolume(Quiet.level)
        PlaybackService.track = OpenedTrack(track, frames * channels * width, mode)
        val given = if (track.performanceMode == AudioTrack.PERFORMANCE_MODE_POWER_SAVING) "power saving" else "normal"
        dev.nori.music.NoriLog.i("rust AudioTrack: $rate Hz x$channels enc=$encoding, ${track.bufferSizeInFrames} of $frames frames (${track.bufferSizeInFrames * 1000L / rate} ms), $given, bitPerfect=$bitPerfect")
        track
    }.onFailure { dev.nori.music.NoriLog.w("rust AudioTrack would not open", it); PlaybackService.observer?.error("rust AudioTrack would not open: $it") }.getOrNull()

    /**
     * Whether the phone's audio chip decodes [encoding] (`AudioFormat.ENCODING_MP3`, `_AAC_LC`, `_OPUS`)
     * where the music goes now: 2 without gaps between songs too, 1 only by itself, 0 not at all (or
     * before Android 10). Asked by the engine once per format until the output moves.
     */
    internal fun offloadSupport(encoding: Int, rate: Int, channels: Int): Int {
        if (Build.VERSION.SDK_INT < 29) return -1
        return runCatching {
            val format = AudioFormat.Builder().setEncoding(encoding).setSampleRate(rate).setChannelMask(mask(channels)).build()
            // The call media3 1.11 makes on each Android (DefaultAudioOffloadSupportProvider): from 13 on it asks
            // getDirectPlaybackSupport, whose answer a phone may give where getPlaybackOffloadSupport says no.
            when {
                Build.VERSION.SDK_INT >= 33 -> (3 shl 8) or (AudioManager.getDirectPlaybackSupport(format, PLATFORM_ATTRIBUTES) and 0xFF)
                Build.VERSION.SDK_INT >= 31 -> (2 shl 8) or (AudioManager.getPlaybackOffloadSupport(format, PLATFORM_ATTRIBUTES) and 0xFF)
                else -> (1 shl 8) or (if (AudioManager.isOffloadedPlaybackSupported(format, PLATFORM_ATTRIBUTES)) 1 else 0)
            }
        }.getOrDefault(-1)
    }

    /**
     * An AudioTrack the audio chip decodes into: the song's packets as they are, [bytes] of them held
     * (minutes of music, so the engine writes rarely). Its stream events go to the engine, which acts on
     * them on its own thread; the route is told to it as for any track. Called on the engine's thread.
     */
    internal fun openOffload(encoding: Int, rate: Int, channels: Int, bytes: Int): AudioTrack? {
        if (Build.VERSION.SDK_INT < 29) return null
        return runCatching {
            val track = AudioTrack.Builder()
                .setAudioAttributes(PLATFORM_ATTRIBUTES)
                .setAudioFormat(AudioFormat.Builder().setSampleRate(rate).setEncoding(encoding).setChannelMask(mask(channels)).build())
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(bytes)
                .setOffloadedPlayback(true)
                .build()
            val events = offloadEvents ?: OffloadEvents { kind -> RustPlayerJni.offloadEvent(h, kind) }.also { offloadEvents = it }
            track.registerStreamEventCallback(Runnable::run, events)
            track.addOnRoutingChangedListener(AudioRouting.OnRoutingChangedListener { r ->
                r.routedDevice?.let { d -> RustPlayerJni.device(h, d.type, d.productName?.toString()) }
            }, main)
            nori.dac.onTrack(rate, encoding, track.isOffloadedPlayback)
            if (Quiet.level < 1f) track.setVolume(Quiet.level)
            PlaybackService.track = OpenedTrack(track, bytes, AudioTrack.PERFORMANCE_MODE_NONE)
            dev.nori.music.NoriLog.i("rust offloaded AudioTrack: $rate Hz x$channels enc=$encoding, ${track.bufferSizeInFrames} of $bytes bytes, offloaded=${track.isOffloadedPlayback}")
            track
        }.onFailure { dev.nori.music.NoriLog.w("rust offloaded AudioTrack would not open", it); PlaybackService.observer?.error("rust offloaded AudioTrack would not open: $it") }.getOrNull()
    }

    /** The offloaded track's stream events, made once, the first time a track is offloaded (Android 10's). */
    private var offloadEvents: OffloadEvents? = null

    /** A radio station's stream, straight from the network, its announcements asked for. Called on a loader thread. */
    internal fun openLive(url: String): RustBody? {
        val (source, every) = try {
            nori.sources.openLive(url)
        } catch (e: Exception) {
            dev.nori.music.NoriLog.w("rust player: the station would not open: $e")
            return null
        }
        loaded(+1)
        return RustBody(source, -1, every) { loaded(-1) }
    }

    /**
     * A song's bytes from [from] on, at the URL and under the cache key the core resolved (over the network
     * state told it here), through media3's data sources (MediaSources): a download, then the stream
     * cache, then the network on the app's one OkHttp client (its TLS, certificates and headers). Called
     * on a loader thread; while one is open the service holds the Wi-Fi lock.
     */
    internal fun open(url: String, key: String, from: Long, ticket: Long = 0): RustBody? {
        val (source, length) = try {
            nori.sources.openResolved(url, key, from, ticket)
        } catch (e: MediaSources.PastEnd) {
            // Not a failure: the song ends before [from] (a transcode's estimated length was longer).
            dev.nori.music.NoriLog.i("rust player: $key from byte $from: past its end (${if (e.whole >= 0) "at ${e.whole}" else "unknown"})")
            return RustBody(null, e.whole, past = true) {}
        } catch (e: Exception) {
            dev.nori.music.NoriLog.w("rust player: $key would not open: $e")
            // The server answered, with an error: said as such, since it was reached.
            val status = MediaSources.httpStatus(e)
            return if (status > 0) RustBody(null, -1, status = status) {} else null
        }
        loaded(+1)
        return RustBody(source, if (length == C.LENGTH_UNSET.toLong()) -1 else length) { Tickets.end(ticket); loaded(-1) }
    }

    internal fun kept(key: String): Boolean = runCatching { MediaSources.isWhole(nori.sources.streamCache, key) }.getOrDefault(true)
    internal fun busy(key: String): Boolean = nori.sources.beingWritten(key)
    internal fun disk(key: String): String = nori.sources.cacheWords(key)
    internal fun forget(key: String): String = nori.sources.forgetStream(key)

    /** A song's bytes started or stopped coming, on a loader thread: several load at once, so both ends are read under the one lock. */
    private fun loaded(by: Int) {
        val changed = synchronized(this) {
            val was = loading > 0
            loading += by
            was != loading > 0
        }
        if (changed) holdCpu()
        if (changed) main.post { invalidateState() }
    }

    // Last, once everything above exists: from here on the engine's threads may call in.
    init {
        RustBridge.player = this
        // Whatever the engine said before this was registered (its first state) is taken now.
        main.post(drain)
        if (h == 0L) error = PlaybackException("the Rust player would not start", null, PlaybackException.ERROR_CODE_FAILED_RUNTIME_CHECK)
    }

    private companion object {
        const val ENGINE_IDLE = 0
        const val ENGINE_PLAYING = 1
        const val ENGINE_ENDED = 3
        const val EVENT_STATE = 0
        const val EVENT_SONG = 1
        const val EVENT_ERROR = 2
        const val EVENT_STOPPED = 4
        const val EVENT_BUFFERING = 5
        const val EVENT_LOOPED = 6
        const val EVENT_TITLE = 7
        const val EVENT_BRIDGE = 8
        const val EVENT_MIXING = 9
        const val EVENT_PLACED = 10
        const val EVENT_LANDED = 11

        val ATTRIBUTES: AudioAttributes = AudioAttributes.Builder().setUsage(C.USAGE_MEDIA).setContentType(C.AUDIO_CONTENT_TYPE_MUSIC).build()
        val PLATFORM_ATTRIBUTES: android.media.AudioAttributes = android.media.AudioAttributes.Builder()
            .setUsage(android.media.AudioAttributes.USAGE_MEDIA).setContentType(android.media.AudioAttributes.CONTENT_TYPE_MUSIC).build()

        fun mask(channels: Int) = if (channels == 1) AudioFormat.CHANNEL_OUT_MONO else AudioFormat.CHANNEL_OUT_STEREO

        /** Volume and speed are the engine's (fades, ReplayGain, the settings), so they are not offered. */
        val COMMANDS: Player.Commands = Player.Commands.Builder().addAll(
            Player.COMMAND_PLAY_PAUSE, Player.COMMAND_PREPARE, Player.COMMAND_STOP, Player.COMMAND_RELEASE,
            Player.COMMAND_SEEK_TO_DEFAULT_POSITION, Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM,
            Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM, Player.COMMAND_SEEK_TO_PREVIOUS,
            Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM, Player.COMMAND_SEEK_TO_NEXT, Player.COMMAND_SEEK_TO_MEDIA_ITEM,
            Player.COMMAND_SEEK_BACK, Player.COMMAND_SEEK_FORWARD,
            Player.COMMAND_SET_REPEAT_MODE, Player.COMMAND_SET_SHUFFLE_MODE,
            Player.COMMAND_GET_CURRENT_MEDIA_ITEM, Player.COMMAND_GET_TIMELINE, Player.COMMAND_GET_METADATA,
            Player.COMMAND_SET_MEDIA_ITEM, Player.COMMAND_CHANGE_MEDIA_ITEMS, Player.COMMAND_GET_AUDIO_ATTRIBUTES,
            Player.COMMAND_GET_VOLUME,
        ).build()
    }
}

/**
 * An offloaded track's stream events, handed to the engine as they come, on the platform's own callback
 * thread: 0 it wants more, 1 it played everything up to the end of stream, 2 it was torn down.
 */
@androidx.annotation.RequiresApi(29)
private class OffloadEvents(private val tell: (Int) -> Unit) : AudioTrack.StreamEventCallback() {
    override fun onDataRequest(track: AudioTrack, sizeInFrames: Int) { OffloadCalls.dataRequests++; tell(0) }
    override fun onPresentationEnded(track: AudioTrack) { OffloadCalls.presented++; tell(1) }
    override fun onTearDown(track: AudioTrack) { OffloadCalls.tornDown++; tell(2) }
}

/**
 * The queue as media3's timeline: one window per song, walked in the core's play order under shuffle
 * ([PlaylistJni.order]) so that next and previous, the notification and the controllers' queue land where
 * the engine goes.
 */
@UnstableApi
private class QueueTimeline(private val items: List<MediaItem>, private val uids: LongArray, private val order: IntArray?) : Timeline() {
    private val byUid = HashMap<Any, Int>(uids.size * 2).apply { uids.forEachIndexed { i, u -> put(u, i) } }
    /** Where each song is in [order]. */
    private val place = order?.let { o -> IntArray(o.size).also { p -> o.forEachIndexed { k, i -> if (i in p.indices) p[i] = k } } }

    override fun getWindowCount() = items.size
    override fun getPeriodCount() = items.size

    private fun durationUs(i: Int): Long = items[i].mediaMetadata.durationMs?.takeIf { it > 0 }?.let { it * 1000 } ?: C.TIME_UNSET

    override fun getWindow(windowIndex: Int, window: Window, defaultPositionProjectionUs: Long): Window =
        window.set(uids[windowIndex], items[windowIndex], null, C.TIME_UNSET, C.TIME_UNSET, C.TIME_UNSET, true, false, null, 0, durationUs(windowIndex), windowIndex, windowIndex, 0)

    override fun getPeriod(periodIndex: Int, period: Period, setIds: Boolean): Period =
        period.set(if (setIds) uids[periodIndex] else null, if (setIds) uids[periodIndex] else null, periodIndex, durationUs(periodIndex), 0)

    override fun getIndexOfPeriod(uid: Any): Int = byUid[uid] ?: C.INDEX_UNSET
    override fun getUidOfPeriod(periodIndex: Int): Any = uids[periodIndex]

    private fun shuffled(shuffle: Boolean) = shuffle && order != null && place != null

    override fun getFirstWindowIndex(shuffle: Boolean): Int = when {
        isEmpty -> C.INDEX_UNSET
        shuffled(shuffle) -> order!![0]
        else -> 0
    }

    override fun getLastWindowIndex(shuffle: Boolean): Int = when {
        isEmpty -> C.INDEX_UNSET
        shuffled(shuffle) -> order!![order.size - 1]
        else -> items.size - 1
    }

    override fun getNextWindowIndex(windowIndex: Int, repeatMode: Int, shuffle: Boolean): Int {
        if (repeatMode == Player.REPEAT_MODE_ONE) return windowIndex
        if (windowIndex == getLastWindowIndex(shuffle)) return if (repeatMode == Player.REPEAT_MODE_ALL) getFirstWindowIndex(shuffle) else C.INDEX_UNSET
        return if (shuffled(shuffle)) order!![place!![windowIndex] + 1] else windowIndex + 1
    }

    override fun getPreviousWindowIndex(windowIndex: Int, repeatMode: Int, shuffle: Boolean): Int {
        if (repeatMode == Player.REPEAT_MODE_ONE) return windowIndex
        if (windowIndex == getFirstWindowIndex(shuffle)) return if (repeatMode == Player.REPEAT_MODE_ALL) getLastWindowIndex(shuffle) else C.INDEX_UNSET
        return if (shuffled(shuffle)) order!![place!![windowIndex] - 1] else windowIndex - 1
    }
}
