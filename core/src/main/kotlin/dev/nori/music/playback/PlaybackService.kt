package dev.nori.music.playback

import dev.nori.music.core.R
import dev.nori.music.ffi.queue.FillNext
import dev.nori.music.ffi.queue.Hand
import android.app.AlarmManager
import android.app.PendingIntent
import android.media.AudioTrack
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.LruCache
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSourceBitmapLoader
import androidx.media3.session.CacheBitmapLoader
import androidx.media3.session.CommandButton
import android.net.wifi.WifiManager
import androidx.media3.session.LibraryResult
import androidx.media3.session.MediaLibraryService
import androidx.media3.session.MediaSession
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionResult
import com.google.common.collect.ImmutableList
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import dev.nori.music.Nori
import dev.nori.music.data.StarKind
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.settings.StoredPrefs
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.guava.future
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The one place audio is played: nori-engine (RustPlayer.kt's [EnginePlayer]) under the media session,
 * so the notification, media buttons and Android Auto follow it. Built for the screen-off case: decoding
 * is offloaded to the audio DSP when the device can, buffers are large so the radio works in bursts,
 * and nothing here polls or ticks while music plays.
 */
@UnstableApi
class PlaybackService : MediaLibraryService() {
    companion object {
        const val CMD_SLEEP = "nori.sleep"
        const val CMD_TUNING = "nori.tuning"
        /** The notification's and lock screen's heart: favourite or unfavourite the current song. */
        const val CMD_FAVOURITE = "nori.favourite"
        /** The notification's and lock screen's shuffle toggle. */
        const val CMD_SHUFFLE = "nori.shuffle"
        /**
         * Next was pressed with nothing after the current song. Autofill may still be fetching similar
         * songs: remember the skip and take it when they land, instead of the press dying as a no-op.
         */
        const val CMD_FILL_NEXT = "nori.fillNext"
        /** Broadcast inside the package on every track or play-state change; what a home-screen widget listens to. */
        const val ACTION_STATE = "dev.nori.music.STATE"
        const val EXTRA_TITLE = "title"
        const val EXTRA_ARTIST = "artist"
        const val EXTRA_PLAYING = "playing"
        const val ARG_ON = "on"
        const val ARG_MINUTES = "minutes"
        const val ARG_END_OF_TRACK = "endOfTrack"
        const val ARG_SONGS = "songs"
        /** Whether the chain is currently asking for offload; read by the test bridge and the perf recorder, which cannot see in here. */
        val offloadWanted: Boolean get() = rustPlayer?.offloadWanted ?: false
        /**
         * The session's own player, for the app's screen to read once, on the main thread, as it comes
         * back before its controller is connected again (PlayerConnection.catchUp). Null with no service.
         */
        @Volatile var sessionPlayer: Player? = null
            private set
        /** The player while the service runs; read by the test bridge and the perf build. */
        @Volatile var rustPlayer: EnginePlayer? = null
            private set
        /** The player the running service built, "rust"; null with no service. For the perf recorder's timeline. */
        @Volatile var engine: String? = null
            private set
        /** The AudioTrack the player last opened, and what was asked of it; null with no service. For the perf recorder. */
        @Volatile var track: OpenedTrack? = null
            internal set(value) {
                field = value
                observer?.track(value)
            }
        /** The perf build's recorder, told what happens as it happens; null in every other build. */
        @Volatile var observer: PlaybackObserver? = null
        /**
         * A mix began or ended being heard (the player's word, on the main thread): the app's page is told,
         * as for a change of song, since the player itself fires no event for it. Set by PlayerConnection.
         */
        @Volatile var onMixingChanged: (() -> Unit)? = null
        /**
         * A new queue was set in the core, and with it perhaps the page it came from (nori-queue
         * `playlist_origin_gen`). The app's controller shows a new queue before the service has made it
         * (media3 masks the change), so the state it published then still carried the last queue's origin;
         * when the player then reports nothing the controller has not already shown (the song sounding on
         * without a buffering state between), no further event came to correct it, and the page the queue
         * now came from kept Play instead of Pause until the next pause or skip. Set by PlayerConnection.
         */
        @Volatile var onQueueSet: (() -> Unit)? = null
    }

    private lateinit var nori: Nori
    private lateinit var player: EnginePlayer
    private lateinit var session: MediaLibrarySession
    /** The player as the session sees it; every change to the queue goes through here, to the core first. */
    private lateinit var controls: Controls
    private lateinit var scrobbler: Scrobbler
    @Suppress("DEPRECATION")
    private val wifiLock by lazy { applicationContext.getSystemService(WifiManager::class.java).createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "nori:loading").apply { setReferenceCounted(false) } }
    private lateinit var analyser: AutoMixPrefetch
    private val measure = Runnable { analyseAhead() }
    /** How long each chore waits (crates/queue/src/rules.rs playback_timings), read once. */
    private val timings by lazy { dev.nori.music.ffi.queue.playbackTimings() }
    private var offlineBridge: OfflineBridge? = null
    /** The music volume for loudness compensation; listening only while that is on. */
    private var volume: VolumeWatch? = null
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val main = Handler(Looper.getMainLooper())
    private val served = LruCache<String, MediaItem>(500)
    private val saveQueue = Runnable { persistQueue(push = false) }
    /**
     * Paused for a long while: the player stops. The output goes, so the phone sleeps; the session goes
     * idle, so nothing holds the service in the foreground and its notification can be swiped away. The
     * queue and the place in the song are saved first; play, a media button or onPlaybackResumption (once
     * the service is gone) picks them up (nori_player::transport::IDLE_RELEASE_MS).
     */
    private val idleRelease = Runnable {
        if (LongPause.releases(player.playWhenReady, player.playbackState)) {
            dev.nori.music.NoriLog.i("paused a long while: output released")
            persistQueue(push = false)
            player.stop()
        }
    }
    private val sleepAlarm = AlarmManager.OnAlarmListener { player.pause() }
    /**
     * The network the phone is on, told to the core whenever it turns metered or not: the core resolves
     * the quality a song streams at (`stream::resolve_now`) and whether a download may use mobile data from
     * it, without asking here. One registration for the service's life; a change that leaves the answer as
     * it was is not passed on.
     */
    private val connectivity by lazy { getSystemService(android.net.ConnectivityManager::class.java) }
    private var metered: Boolean? = null
    private val network = object : android.net.ConnectivityManager.NetworkCallback() {
        override fun onCapabilitiesChanged(network: android.net.Network, caps: android.net.NetworkCapabilities) =
            tellMetered(!caps.hasCapability(android.net.NetworkCapabilities.NET_CAPABILITY_NOT_METERED))
    }
    private fun tellMetered(now: Boolean) {
        if (now == metered) return
        metered = now
        dev.nori.music.ffi.net.networkMetered(now)
        // Onto Wi-Fi: the AutoEQ list is fetched if the core says it is due, and nothing happens otherwise.
        if (!now) scope.launch { nori.keepAutoEqList() }
    }

    override fun onCreate() {
        super.onCreate()
        nori = Nori.get(this)
        scrobbler = Scrobbler(nori, scope)
        // Before the player opens a song.
        tellMetered(nori.http.metered)
        runCatching { connectivity.registerDefaultNetworkCallback(network, main) }
        player = EnginePlayer(this, nori).also { rustPlayer = it }
        engine = "rust"
        observer?.engine(engine)
        // A song that has become whole on the device is measured then, whoever fetched it (see AutoMixPrefetch).
        analyser = AutoMixPrefetch(nori.sources) { player.replan() }
        // Nothing is watched until a bridge starts (see OfflineBridge). The player says itself when the
        // core handed a failure to the bridge.
        offlineBridge = OfflineBridge(this, player, { nori.core }, main, ::applyEdit, ::skipAfterError)
        player.onBridge = ::bridge
        player.addListener(listener)

        // Fired from the audio device callback (main) and from the player's track opening (its own thread);
        // the player may only be touched on the main looper.
        nori.dac.onChanged = { main.post { applyAudio(nori.settings.value); player.gainChanged() } }
        nori.dac.start()
        nori.outputs.start()
        // Plugging in headphones or a DAC swaps the whole sound chain, if a profile is bound to it.
        scope.launch {
            // A device arriving or leaving changes what the audio chain may do (see applyAudio), whether or
            // not the user binds sound profiles to outputs.
            nori.outputs.usb.collect { applyAudio(nori.settings.value) }
        }
        scope.launch {
            // The device's own sound (a bound profile or AutoEQ curve), or the sound from before it came back.
            nori.outputs.current.collect { output ->
                nori.deviceSound.onOutput(output)
                volume?.outputChanged()
            }
        }
        // Loudness compensation follows the volume: while it is on, the volume is watched and told.
        volume = VolumeWatch(this, { nori.outputs.current.value }) { index, max, db -> player.setVolume(index, max, db) }
        scope.launch {
            nori.settings.prefs.collect { p -> volume?.set(p.loudness) }
        }
        applyAudio(nori.settings.value)
        scope.launch {
            // What a change asks of the player is the core's call (settings_store.rs); the sound chain and
            // the planner's settings follow there by themselves, so a screen's setting costs nothing here.
            nori.settings.effects.collect { e ->
                if (e and 1 != 0) applyAudio(nori.settings.value)
                // The sound chain's values alone (8), or the fades and high quality output (16): the player
                // keeps its own and is handed them.
                else if (e and (8 or 16) != 0) player.applySettings()
                // The player puts each song's volume on its own samples; it only needs telling the settings changed.
                if (e and 2 != 0) player.gainChanged()
                // The player hands the planner the output's say itself, and asks again.
                if (e and 4 != 0) player.replan()
            }
        }

        val open = packageManager.getLaunchIntentForPackage(packageName)?.let { PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE) }
        controls = Controls(player)
        sessionPlayer = controls
        session = MediaLibrarySession.Builder(this, controls, Callback())
            // Notification and lock-screen art: same connection pool as everything else, last bitmap kept, decoded no larger than needed.
            .setBitmapLoader(CacheBitmapLoader(DataSourceBitmapLoader.Builder(this).setDataSourceFactory(nori.sources.network).setMaximumOutputDimension(512).build()))
            // Controllers extrapolate the playhead themselves; a broadcast every few seconds is a wake-up for nothing.
            .setPeriodicPositionUpdateEnabled(false)
            .apply { open?.let(::setSessionActivity) }.build()
        // The notification and the lock screen carry the app's own mark, not media3's stock play circle.
        // Its id stays media3's default (1001): the download notification lives on 2001 so the two never replace each other.
        setMediaNotificationProvider(
            androidx.media3.session.DefaultMediaNotificationProvider.Builder(this)
                .setNotificationId(androidx.media3.session.DefaultMediaNotificationProvider.DEFAULT_NOTIFICATION_ID).build()
                .apply { setSmallIcon(dev.nori.music.core.R.drawable.ic_notification) },
        )
        // A heart changed anywhere in the app (or by the notification itself) redraws the notification's heart.
        // A StateFlow: it emits only on a change, so this is idle while music plays untouched.
        scope.launch { nori.library.starMarks.collect { refreshButtons() } }
        restoreQueue()
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo) = session

    override fun onTaskRemoved(rootIntent: android.content.Intent?) {
        if (!player.playWhenReady || player.mediaItemCount == 0) stopSelf()
    }

    override fun onDestroy() {
        sessionPlayer = null
        rustPlayer = null
        engine = null
        track = null
        observer?.engine(null)
        keepQueue(dev.nori.music.ffi.queue.QueueMoment.CLOSING)
        getSystemService(AlarmManager::class.java).cancel(sleepAlarm)
        main.removeCallbacks(measure)
        main.removeCallbacks(idleRelease)
        runCatching { connectivity.unregisterNetworkCallback(network) }
        volume?.set(false)
        volume = null
        analyser.release()
        offlineBridge?.abandon()
        offlineBridge = null
        nori.dac.onChanged = {}
        nori.dac.stop()
        nori.outputs.stop()
        if (wifiLock.isHeld) wifiLock.release()
        session.release()
        player.release()
        scope.cancel()
        super.onDestroy()
    }

    // ---- what changes with the track ----

    private val listener = object : Player.Listener {
        override fun onMediaItemTransition(item: MediaItem?, reason: Int) {
            item?.let { observer?.song(it.mediaId) }
            observer?.arrived(player.currentMediaItemIndex, reason == Player.MEDIA_ITEM_TRANSITION_REASON_AUTO, player.shuffleModeEnabled)
            val looped = reason == Player.MEDIA_ITEM_TRANSITION_REASON_REPEAT
            // The player walked the core's queue itself (explicit songs skipped, each song's volume set):
            // this is only the song the ear arrived on.
            refreshButtons()
            scrobbler.onTrack(item?.mediaId, if (looped) dev.nori.music.ffi.queue.TrackChange.LOOPED else dev.nori.music.ffi.queue.TrackChange.MOVED, player.isPlaying)
            if (looped) return
            // What a new song asks for is the core's, in one answer (rules.rs song_arrived): the queue saved
            // a moment later, songs fetched for its end, the offline bridge's turn, fetching ahead, and the
            // sleep timer's count.
            val steps = dev.nori.music.ffi.queue.songArrived()
            scheduleSave(steps.saveAfterMs)
            if (steps.fill) fetchFill()
            offlineBridge?.onSong(steps.bridge)
            announce()
            // The songs after the next are fetched ahead by the engine as the song starts, in the same wake of
            // the network as the next one (nori-engine's one fetcher, measuring them as they come); a few
            // seconds in, whatever is whole on the device and not measured yet is measured from there.
            main.removeCallbacks(measure)
            main.postDelayed(measure, steps.precacheAfterMs)
            if (steps.pauseAtEnd) pauseAtEnd(true)
        }

        override fun onIsLoadingChanged(isLoading: Boolean) {
            if (isLoading && !wifiLock.isHeld) wifiLock.acquire() else if (!isLoading && wifiLock.isHeld) wifiLock.release()
        }

        override fun onIsPlayingChanged(isPlaying: Boolean) {
            // Sound is coming out: whatever failed before it is no longer a run (rules.rs queue_playing).
            if (isPlaying) dev.nori.music.ffi.queue.queuePlaying()
            announce()
            scrobbler.onPlaying(isPlaying)
            main.removeCallbacks(idleRelease)
            if (LongPause.arms(isPlaying, player.playWhenReady, player.playbackState)) main.postDelayed(idleRelease, timings.idleReleaseMs)
        }

        /**
         * Paused by the listener (or stopped by the queue's rules): the queue is kept at once. Asked here and
         * not on the music stopping, since a player that never sounded (a song whose bytes never came) is
         * paused without ever having played, and a queue skipped through meanwhile was never saved.
         */
        override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
            if (!playWhenReady) keepQueue(dev.nori.music.ffi.queue.QueueMoment.PAUSED)
        }

        override fun onShuffleModeEnabledChanged(on: Boolean) = refreshButtons()

        override fun onTimelineChanged(timeline: androidx.media3.common.Timeline, reason: Int) {
            if (reason == Player.TIMELINE_CHANGE_REASON_PLAYLIST_CHANGED) {
                keepQueue(dev.nori.music.ffi.queue.QueueMoment.EDITED)
                // The queue was edited: what comes next may not be what it was. A song queued to play next
                // is fetched and measured now, while there is time to plan its mix, not when its turn comes:
                // without it AutoMix had nothing to mix it by. The engine fetches it (and the songs after it)
                // as it hears of the edit; this measures what is on the device, once per burst of edits.
                main.removeCallbacks(measure)
                main.postDelayed(measure, timings.measureAfterEditMs)
            }
        }

        override fun onPlayerError(error: androidx.media3.common.PlaybackException) {
            // The player skips or stops by the core's rules itself; what reaches here is its output or the
            // engine failing to start, which trying again would not change.
            observer?.error(generateSequence<Throwable>(error) { it.cause }.joinToString(" <- "))
            dev.nori.music.NoriLog.w("rust player: $error")
        }

        override fun onPlaybackStateChanged(state: Int) {
            if (state == Player.STATE_ENDED) scrobbler.onTrack(null, dev.nori.music.ffi.queue.TrackChange.ENDED, false)
        }
    }

    /**
     * The player stopped at a song the network would not bring, the core having said it is the offline
     * bridge's (rules.rs queue_error): the bridge takes it, or it is skipped like any failure; which, and
     * the run of failures, are the core's (bridge.rs bridge_take). Whether the music goes on.
     */
    private fun bridge(): Boolean =
        offlineBridge?.take() ?: dev.nori.music.ffi.queue.queueBridgeFailed().also { if (it) skipAfterError() }

    /** The core said to skip a song that will not play (and counted it). */
    private fun skipAfterError() {
        player.seekToNextMediaItem()
        player.prepare()
        player.play()
    }

    /** What the heart and shuffle buttons last showed, so an unrelated change does not rebuild the notification. */
    private var buttonsShown: dev.nori.music.ffi.SessionButtons? = null

    private fun currentStarred(item: MediaItem): Boolean =
        nori.library.isStarred(StarKind.SONG, item.mediaId, dev.nori.music.ffi.queue.queueFlags(item.mediaId) and 2u != 0u)

    /**
     * Heart and shuffle beside previous / play / next, in the secondary slots the way other players put
     * them. Called when the track, the shuffle flag or a star changes - never on a timer. Whether the
     * heart shows and what each does are the core's (shown.rs session_buttons_now); what they say, ours.
     */
    private fun refreshButtons() {
        if (!::session.isInitialized) return
        val item = player.currentMediaItem
        val b = dev.nori.music.ffi.sessionButtonsNow(item != null && currentStarred(item), player.shuffleModeEnabled)
        if (b == buttonsShown) return
        buttonsShown = b
        val buttons = ArrayList<CommandButton>(2)
        if (b.heart) {
            buttons += CommandButton.Builder(if (b.starred) CommandButton.ICON_HEART_FILLED else CommandButton.ICON_HEART_UNFILLED)
                .setDisplayName(getString(if (b.starred) R.string.session_remove_favourite else R.string.session_add_favourite))
                .setSessionCommand(SessionCommand(CMD_FAVOURITE, Bundle.EMPTY))
                .setSlots(CommandButton.SLOT_BACK_SECONDARY, CommandButton.SLOT_OVERFLOW).build()
        }
        buttons += CommandButton.Builder(if (b.shuffling) CommandButton.ICON_SHUFFLE_ON else CommandButton.ICON_SHUFFLE_OFF)
            .setDisplayName(getString(if (b.shuffling) R.string.session_shuffle_off else R.string.session_shuffle_on))
            .setSessionCommand(SessionCommand(CMD_SHUFFLE, Bundle.EMPTY))
            .setSlots(CommandButton.SLOT_FORWARD_SECONDARY, CommandButton.SLOT_OVERFLOW).build()
        session.setMediaButtonPreferences(buttons)
    }

    /** Pause once the song playing ends (the sleep timer's "end of this song"). */
    private fun pauseAtEnd(on: Boolean) {
        player.pauseAtEndOfItem = on
    }

    private fun announce() {
        val m = player.currentMediaItem?.mediaMetadata
        sendBroadcast(android.content.Intent(ACTION_STATE).setPackage(packageName)
            .putExtra(EXTRA_TITLE, m?.title?.toString()).putExtra(EXTRA_ARTIST, m?.artist?.toString()).putExtra(EXTRA_PLAYING, player.isPlaying))
    }

    /**
     * DSP, crossfade, speed, offload and bit-perfect constrain each other. The player puts the core's policy
     * over its own chain (nori-engine's `apply`): it is handed the settings and what only the platform sees
     * of the output (something USB, a DAC playing bit-perfect), and takes offload, speed, silence skipping
     * and the transitions from them.
     */
    private fun applyAudio(p: StoredPrefs) {
        nori.dac.setEnabled(p.bitPerfect)
        player.setOutput(nori.outputs.usb.value, nori.dac.state.value.bitPerfect)
        player.applySettings()
        // AutoMix has nothing to plan from until the tracks coming up have been measured, which was only
        // ever started by the queue moving: switched on in the middle of a song, the first mix it could
        // have made was two boundaries away.
        main.removeCallbacks(measure)
        main.postDelayed(measure, timings.measureAfterSettingsMs)
    }

    /**
     * What the session (notification, headset, our UI, Android Auto) actually controls: the player plus the
     * configured manners. How each control sounds (the fades, the dip around a switch) is nori-engine's.
     */
    private inner class Controls(p: Player) : androidx.media3.common.ForwardingPlayer(p) {
        override fun play() {
            // Let go after a long pause (see idleRelease): opened again here.
            if (wrappedPlayer.playbackState == Player.STATE_IDLE && wrappedPlayer.mediaItemCount > 0) wrappedPlayer.prepare()
            super.play()
        }

        /**
         * A skip the user asked for while the music is paused starts it (nori_player::transport::
         * skip_plays). Only the buttons go through here - the service's own skips (an explicit track, a
         * track that will not play) call the player underneath, so a queue that was paused stays paused
         * while it steps over them.
         */
        private fun andPlay(action: () -> Unit) {
            action()
            if (dev.nori.music.ffi.queue.skipPlays(playWhenReady)) play()
        }

        // The queue is the core's (crates/queue/src/playlist.rs): each change is made there first, and
        // the player is told the same change; it reads the core's play order itself.
        override fun addMediaItem(mediaItem: MediaItem) = addMediaItems(Int.MAX_VALUE, listOf(mediaItem))
        override fun addMediaItem(index: Int, mediaItem: MediaItem) = addMediaItems(index, listOf(mediaItem))
        override fun addMediaItems(mediaItems: List<MediaItem>) = addMediaItems(Int.MAX_VALUE, mediaItems)
        override fun addMediaItems(index: Int, mediaItems: List<MediaItem>) {
            if (mediaItems.isEmpty()) return
            // Where they go (after the playing song when added by hand, else where the controller asked)
            // is the core's; each item says how it came.
            val at = index.coerceIn(0, wrappedPlayer.mediaItemCount).toUInt()
            // An undo puts the song back where it was, if the core still has it as the one taken out.
            val back = mediaItems.singleOrNull()?.takeIf { it.isRestored() }?.let { dev.nori.music.ffi.queue.playlistRestore(it.mediaId) }?.takeIf { it.at != null }
            // The page they are all the songs of, if any (an album added whole: MediaItems.origin).
            val c = back ?: dev.nori.music.ffi.queue.playlistTake(at, ids(mediaItems), mediaItems.map { it.queuedAs() ?: Hand.NO }, mediaItems.first().origin())
            super.addMediaItems(c.at?.toInt() ?: at.toInt(), mediaItems)
        }

        // A fresh evening: the parked online queue from a bridge is not part of this request.
        override fun setMediaItem(mediaItem: MediaItem) = setMediaItems(listOf(mediaItem))
        override fun setMediaItem(mediaItem: MediaItem, resetPosition: Boolean) = setMediaItems(listOf(mediaItem), resetPosition)
        override fun setMediaItem(mediaItem: MediaItem, startPositionMs: Long) = setMediaItems(listOf(mediaItem), 0, startPositionMs)
        override fun setMediaItems(mediaItems: List<MediaItem>) = setMediaItems(mediaItems, C.INDEX_UNSET, C.TIME_UNSET)
        override fun setMediaItems(mediaItems: List<MediaItem>, resetPosition: Boolean) =
            if (resetPosition) setMediaItems(mediaItems, C.INDEX_UNSET, C.TIME_UNSET) else setMediaItems(mediaItems, wrappedPlayer.currentMediaItemIndex, wrappedPlayer.currentPosition)
        override fun setMediaItems(mediaItems: List<MediaItem>, startIndex: Int, startPositionMs: Long) {
            offlineBridge?.abandon()
            // A list already in the order it plays (a weighted shuffle) goes in as it is, shown as
            // shuffled; the player's own shuffle, which would undo that order, goes off.
            val ordered = mediaItems.firstOrNull()?.inOrder() == true
            // The page it was started from, if any (MediaItems.origin): a new queue replaces the last one's.
            val origin = mediaItems.firstOrNull()?.origin()
            val c = if (ordered) dev.nori.music.ffi.queue.playlistSetOrdered(ids(mediaItems), origin)
            else dev.nori.music.ffi.queue.playlistSet(ids(mediaItems), startIndex.coerceAtMost(mediaItems.size - 1).takeIf { it >= 0 }?.toUInt(), wrappedPlayer.shuffleModeEnabled, origin)
            if (ordered && wrappedPlayer.shuffleModeEnabled) super.setShuffleModeEnabled(false)
            super.setMediaItems(mediaItems, c.at?.toInt() ?: 0, if (startIndex == C.INDEX_UNSET) C.TIME_UNSET else startPositionMs)
            onQueueSet?.invoke()
        }
        override fun clearMediaItems() {
            offlineBridge?.abandon()
            dev.nori.music.ffi.queue.playlistSet(emptyList(), null, false, null)
            super.clearMediaItems()
            onQueueSet?.invoke()
        }
        override fun removeMediaItem(index: Int) = removeMediaItems(index, index + 1)
        override fun removeMediaItems(fromIndex: Int, toIndex: Int) {
            // The player drops removed songs from its play order and keeps the rest as it was, as the core does.
            dev.nori.music.ffi.queue.playlistRemove(fromIndex.toUInt(), toIndex.toUInt())
            super.removeMediaItems(fromIndex, toIndex)
        }
        override fun moveMediaItem(currentIndex: Int, newIndex: Int) = moveMediaItems(currentIndex, currentIndex + 1, newIndex)
        override fun moveMediaItems(fromIndex: Int, toIndex: Int, newIndex: Int) {
            dev.nori.music.ffi.queue.playlistMove(fromIndex.toUInt(), toIndex.toUInt(), newIndex.toUInt())
            super.moveMediaItems(fromIndex, toIndex, newIndex)
        }
        /** Shuffle on: the playing song first, the songs added by hand after it, the rest shuffled. */
        override fun setShuffleModeEnabled(shuffleModeEnabled: Boolean) {
            dev.nori.music.ffi.queue.playlistShuffle(shuffleModeEnabled)
            super.setShuffleModeEnabled(shuffleModeEnabled)
        }
        override fun setRepeatMode(repeatMode: Int) {
            dev.nori.music.ffi.queue.playlistRepeat(repeatMode.toUByte())
            super.setRepeatMode(repeatMode)
        }

        override fun seekToNext() { observer?.skipped(currentMediaItemIndex); andPlay { super.seekToNext() } }
        override fun seekToNextMediaItem() { observer?.skipped(currentMediaItemIndex); andPlay { super.seekToNextMediaItem() } }
        override fun seekToPreviousMediaItem() { observer?.skipped(currentMediaItemIndex); andPlay { super.seekToPreviousMediaItem() } }
        // Well into a song this goes back to 0:00 rather than to the song before (media3's own rule,
        // three seconds, unless the user has previous always skip: nori_player::queue::previous_restarts),
        // which paused means: start this one again, from the top, playing.
        override fun seekToPrevious() = andPlay {
            observer?.skipped(currentMediaItemIndex)
            if (!dev.nori.music.ffi.queue.queuePreviousRestarts(currentPosition, hasPreviousMediaItem()) && hasPreviousMediaItem()) super.seekToPreviousMediaItem()
            else super.seekToPrevious()
        }
    }

    private fun ids(items: List<MediaItem>): List<String> = items.map { it.mediaId }

    /**
     * A change the core made to its queue on its own (the offline bridge), made to the player the same
     * way: ranges out, songs in, then the jump, and playing.
     */
    private fun applyEdit(e: dev.nori.music.ffi.queue.QueueEdit) {
        for (k in e.remove.indices step 2) player.removeMediaItems(e.remove[k].toInt(), e.remove[k + 1].toInt())
        if (e.songs.isNotEmpty()) player.addMediaItems(e.at.toInt(), held(e.songs))
        e.seek?.let { player.seekTo(it.toInt(), C.TIME_UNSET); player.prepare(); player.play() }
    }

    /**
     * Measures the track playing and the two after it, unless they have been measured before. AutoMix
     * plans a transition from both halves' analyses, and until this existed the only way to get one was
     * to have played the track through: the first time two songs met they were faded rather than mixed,
     * and the plan for the boundary the listener was already in the middle of arrived too late to use.
     * Off entirely when AutoMix is (the core then names no songs), and it never fetches anything (see
     * AutoMixPrefetch). Asked again with the same songs, it does nothing.
     */
    private fun analyseAhead() = analyser.update()

    /**
     * Keeps the music going past the end of the queue. When to fetch (the end in sight, two songs left
     * at most, one fetch at a time; never for a radio stream or a repeating queue), what it carries on
     * from (the queue's last song) and whether a next pressed meanwhile is still wanted (within two
     * seconds of the last press, once) are the core's
     * (crates/queue/src/autofill.rs over nori_player::queue::Refill, asked as a song arrives), and so is
     * what comes - the user's choice twice over, and every route reads the library, so this never makes
     * octo-fiesta download a provider track.
     */
    private fun fetchFill() = scope.launch {
        val fresh = runCatching { nori.library.autofill() }.getOrNull()
        val songs = fresh?.songs.orEmpty()
        // Player work stays on this scope's main dispatcher.
        if (dev.nori.music.ffi.queue.autofillArrived(songs.size.toUInt())) {
            // Where they come from, as the core says: an album from its page (played as an album, as its Add
            // to queue does), a shuffle's albums from the shuffle.
            controls.addMediaItems(startedFrom(held(songs), fresh?.from))
        }
        // A next pressed at the end while these were on the way is taken now, if the user is still there
        // and pressed it moments ago; a press the user has long since settled after is not.
        if (dev.nori.music.ffi.queue.autofillLanded()) player.seekToNextMediaItem()
    }

    /** Next with nothing after: fetch, and take the skip when the songs land (the core remembers the press). */
    private fun fillThenNext() {
        when (dev.nori.music.ffi.queue.autofillNext()) {
            FillNext.SKIP -> player.seekToNextMediaItem()
            FillNext.FETCH -> fetchFill()
            FillNext.WAIT -> {}
        }
    }

    // ---- the queue outlives the process ----

    /** When the queue is saved, and handed to the server, is the core's (rules.rs queue_keep). */
    private fun keepQueue(moment: dev.nori.music.ffi.queue.QueueMoment) {
        val k = dev.nori.music.ffi.queue.queueKeep(moment)
        if (k.saveAfterMs > 0) scheduleSave(k.saveAfterMs) else persistQueue(k.push)
    }

    private fun scheduleSave(afterMs: Long) {
        main.removeCallbacks(saveQueue)
        main.postDelayed(saveQueue, afterMs)
    }

    private fun persistQueue(push: Boolean) {
        main.removeCallbacks(saveQueue)
        // The queue is the core's (crates/queue/src/playlist.rs); only the place in the song is the player's.
        val current = player.currentMediaItem?.mediaId
        val position = player.currentPosition.coerceAtLeast(0)
        // Not a child of the service's scope: the save made as the service closes runs after the scope is
        // cancelled, and was lost with it, leaving the queue where it was saved before.
        scope.launch(Dispatchers.IO + NonCancellable) {
            runCatching { nori.core.playlistSave(position.toULong()) }
            // What the server is handed (only with scrobbling on, radio left out) is the core's too, read there.
            if (push) runCatching { nori.library.pushQueue(current, position) }
        }
    }

    private fun restoreQueue() = scope.launch {
        val q = withContext(Dispatchers.IO) { runCatching { nori.core.loadQueue() }.getOrNull() } ?: return@launch
        if (q.songs.isEmpty() || player.mediaItemCount > 0) return@launch
        // Not prepared: nothing touches the network until the user presses play. The core keeps the index
        // inside the queue it hands back.
        // With the page it was started from, so that page still answers for it.
        controls.setMediaItems(startedFrom(held(q.songs), q.origin), q.index.toInt(), q.positionMs.toLong())
    }

    /** Songs as the player's items, handed to the core in one call (see MediaItems.toMediaItems). */
    private fun items(songs: List<Song>): List<MediaItem> = songs.toMediaItems { nori.library.coverUrl(it.coverArt, NOTIFICATION_ART) }
    private fun item(s: Song): MediaItem = items(listOf(s)).first()
    /** Songs the core made for the queue and already keeps (MediaItems.heldMediaItems): nothing handed back. */
    private fun held(songs: List<Song>): List<MediaItem> = songs.heldMediaItems { nori.library.coverUrl(it.coverArt, NOTIFICATION_ART) }

    // ---- session: custom commands, Android Auto browsing, voice search ----

    /** The controller whose screen has the shallow buffer on, or null: one owner, so it cannot be left on. */
    private var tuner: MediaSession.ControllerInfo? = null

    /** The equalizer screen is being tuned ([on]) or no longer is. Only a change is passed on. */
    private fun tune(on: Boolean, controller: MediaSession.ControllerInfo?) {
        if (on == (tuner != null)) { if (on) tuner = controller; return }
        tuner = if (on) controller else null
        // The player's own pipeline keeps the rule (nori_player::transport::Chain::tuning): the track made
        // shallow in place, so a band's move is heard within half a second, and deep again after.
        player.setTuning(on)
        observer?.tuning(on)
    }

    private inner class Callback : MediaLibrarySession.Callback {
        override fun onConnect(session: MediaSession, controller: MediaSession.ControllerInfo): MediaSession.ConnectionResult {
            val commands = MediaSession.ConnectionResult.DEFAULT_SESSION_AND_LIBRARY_COMMANDS.buildUpon().add(SessionCommand(CMD_SLEEP, Bundle.EMPTY)).add(SessionCommand(CMD_TUNING, Bundle.EMPTY))
                .add(SessionCommand(CMD_FAVOURITE, Bundle.EMPTY)).add(SessionCommand(CMD_SHUFFLE, Bundle.EMPTY))
                .add(SessionCommand(CMD_FILL_NEXT, Bundle.EMPTY)).build()
            return MediaSession.ConnectionResult.AcceptedResultBuilder(session).setAvailableSessionCommands(commands).build()
        }

        override fun onCustomCommand(session: MediaSession, controller: MediaSession.ControllerInfo, command: SessionCommand, args: Bundle): ListenableFuture<SessionResult> {
            if (command.customAction == CMD_SLEEP) {
                val alarms = getSystemService(AlarmManager::class.java)
                alarms.cancel(sleepAlarm)
                // The songs still to go are counted in the core (rules.rs sleep_set, sleep_song_changed).
                pauseAtEnd(dev.nori.music.ffi.queue.sleepSet(args.getInt(ARG_SONGS).coerceAtLeast(0).toUInt(), args.getBoolean(ARG_END_OF_TRACK)))
                val minutes = args.getInt(ARG_MINUTES)
                // An alarm, not a Handler: with offloaded playback the CPU sleeps and uptime stops counting.
                if (minutes > 0) dev.nori.music.ffi.queue.sleepDelay(minutes.toUInt()).let { (delay, slack) ->
                    alarms.setWindow(AlarmManager.ELAPSED_REALTIME_WAKEUP, SystemClock.elapsedRealtime() + delay, slack, "nori.sleep", sleepAlarm, main)
                }
            }
            if (command.customAction == CMD_FAVOURITE) {
                // Only while the heart shows (a song of the library is playing; the core's call).
                val item = player.currentMediaItem?.takeIf { buttonsShown?.heart != null }
                if (item != null) {
                    val on = !currentStarred(item)
                    // The same path as the app's heart: the mark goes up at once (and redraws both hearts),
                    // the request runs on an IO thread inside Library, and a failure puts the mark back.
                    scope.launch { runCatching { nori.library.star(StarKind.SONG, item.mediaId, on) }.onFailure { dev.nori.music.NoriLog.w("star from the notification failed: $it") } }
                }
            }
            if (command.customAction == CMD_SHUFFLE) controls.shuffleModeEnabled = !player.shuffleModeEnabled
            if (command.customAction == CMD_FILL_NEXT) fillThenNext()
            if (command.customAction == CMD_TUNING) tune(args.getBoolean(ARG_ON), controller)
            return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
        }

        // The screen that asked for the shallow buffer has gone with its controller (the app's process
        // died, or it let go of the service): nobody is tuning any more, so the deep buffer comes back
        // rather than staying shallow for as long as the service lives.
        override fun onDisconnected(session: MediaSession, controller: MediaSession.ControllerInfo) {
            if (tuner == controller) tune(false, null)
        }

        override fun onAddMediaItems(session: MediaSession, controller: MediaSession.ControllerInfo, items: MutableList<MediaItem>): ListenableFuture<MutableList<MediaItem>> {
            val query = items.singleOrNull()?.requestMetadata?.searchQuery
            if (query != null) return scope.future { items(nori.library.search(query).songs).toMutableList() }
            // Our own UI sends complete items. Android Auto sends bare ids of things it was shown earlier.
            if (items.all { it.mediaMetadata.title != null }) return Futures.immediateFuture(items.map { it.playable() }.toMutableList())
            return scope.future {
                items.mapNotNull { i -> served.get(i.mediaId) ?: withContext(Dispatchers.IO) { runCatching { nori.library.song(i.mediaId) }.getOrNull() }?.let(::item) }.toMutableList()
            }
        }

        override fun onPlaybackResumption(session: MediaSession, controller: MediaSession.ControllerInfo): ListenableFuture<MediaSession.MediaItemsWithStartPosition> = scope.future {
            val q = withContext(Dispatchers.IO) { nori.core.loadQueue() }
            MediaSession.MediaItemsWithStartPosition(startedFrom(held(q.songs), q.origin), q.index.toInt(), q.positionMs.toLong())
        }

        override fun onGetLibraryRoot(session: MediaLibrarySession, browser: MediaSession.ControllerInfo, params: LibraryParams?) =
            Futures.immediateFuture(LibraryResult.ofItem(folder(nori.client.browseRoot()), params))

        override fun onGetChildren(session: MediaLibrarySession, browser: MediaSession.ControllerInfo, parentId: String, page: Int, pageSize: Int, params: LibraryParams?): ListenableFuture<LibraryResult<ImmutableList<MediaItem>>> =
            scope.future {
                val children = runCatching { children(parentId) }.getOrDefault(emptyList())
                children.forEach { if (it.mediaMetadata.isPlayable == true) served.put(it.mediaId, it) }
                LibraryResult.ofItemList(children.drop(page * pageSize).take(pageSize), params)
            }

        override fun onSearch(session: MediaLibrarySession, browser: MediaSession.ControllerInfo, query: String, params: LibraryParams?): ListenableFuture<LibraryResult<Void>> {
            scope.launch {
                val n = runCatching { nori.library.search(query).songs.size }.getOrDefault(0)
                session.notifySearchResultChanged(browser, query, n, params)
            }
            return Futures.immediateFuture(LibraryResult.ofVoid())
        }

        override fun onGetSearchResult(session: MediaLibrarySession, browser: MediaSession.ControllerInfo, query: String, page: Int, pageSize: Int, params: LibraryParams?): ListenableFuture<LibraryResult<ImmutableList<MediaItem>>> =
            scope.future {
                val songs = runCatching { items(nori.library.search(query).songs) }.getOrDefault(emptyList())
                songs.forEach { served.put(it.mediaId, it) }
                LibraryResult.ofItemList(songs.drop(page * pageSize).take(pageSize), params)
            }
    }

    /** A folder of the car's tree: the tree's own folders named from the resources, an album's or a playlist's by its own name. */
    private fun folder(f: dev.nori.music.ffi.library.BrowseFolder): MediaItem = MediaItem.Builder().setMediaId(f.id).setMediaMetadata(
        MediaMetadata.Builder().setTitle(f.kind?.let { getString(carFolderName(it)) } ?: f.title)
            .setArtist(f.subtitle ?: f.songs?.let { getString(dev.nori.music.core.R.string.car_playlist_songs, it.toInt()) })
            .setArtworkUri(f.art?.let(android.net.Uri::parse))
            .setIsBrowsable(true).setIsPlayable(false).setMediaType(MediaMetadata.MEDIA_TYPE_FOLDER_MIXED).build()
    ).build()

    private fun carFolderName(k: dev.nori.music.ffi.library.CarFolder): Int = when (k) {
        dev.nori.music.ffi.library.CarFolder.ROOT -> dev.nori.music.core.R.string.car_root
        dev.nori.music.ffi.library.CarFolder.RECENTLY_PLAYED -> dev.nori.music.core.R.string.car_recently_played
        dev.nori.music.ffi.library.CarFolder.RECENTLY_ADDED -> dev.nori.music.core.R.string.car_recently_added
        dev.nori.music.ffi.library.CarFolder.MOST_PLAYED -> dev.nori.music.core.R.string.car_most_played
        dev.nori.music.ffi.library.CarFolder.PLAYLISTS -> dev.nori.music.core.R.string.car_playlists
        dev.nori.music.ffi.library.CarFolder.FAVOURITES -> dev.nori.music.core.R.string.car_favourites
        dev.nori.music.ffi.library.CarFolder.RANDOM -> dev.nori.music.core.R.string.car_random
        dev.nori.music.ffi.library.CarFolder.DOWNLOADS -> dev.nori.music.core.R.string.car_downloads
    }

    /** What a folder of the car's tree holds is the core's (crates/library/src/car.rs); this makes the items. */
    private suspend fun children(parent: String): List<MediaItem> {
        val page = nori.client.browseChildren(parent)
        return page.folders.map(::folder) + items(page.songs)
    }
}

/**
 * An AudioTrack a player opened, with what was asked of it: [askedBytes] of buffer and the performance
 * mode [askedMode]. The perf build reads what the platform made of it against that; nothing else does.
 */
class OpenedTrack(val track: AudioTrack, val askedBytes: Int, val askedMode: Int)

/**
 * What the perf build's recorder is told as it happens, for the timeline under each stretch
 * (docs/perf-build.md). Only that build sets [PlaybackService.observer]; in every other build it stays
 * null and each call site is one null check. Called on whatever thread the event is on: the recorder
 * hands everything to its own.
 */
interface PlaybackObserver {
    /** The ear arrived on the song [id] (a media id). */
    fun song(id: String)

    /** The service started with [engine] ("rust"), or ended (null). */
    fun engine(engine: String?)

    /** A player opened an output, or the service let it go (null). */
    fun track(opened: OpenedTrack?)

    /** Playback failed, in the player's words. */
    fun error(message: String)

    /** The equalizer screen's tuning mode came on or off. */
    fun tuning(on: Boolean)

    /** The user pressed next or previous (the session's buttons: the app, the notification, a headset) on queue place [index]. */
    fun skipped(index: Int) {}

    /** The player arrived on queue place [index]: by itself ([auto], a song that ended) or by a jump; [shuffled] under shuffle. */
    fun arrived(index: Int, auto: Boolean, shuffled: Boolean) {}

    /** The player took its CPU wake lock ([held]) or let it go. From any thread. */
    fun wakeLock(held: Boolean) {}
}

/**
 * The long pause's rule: paused (not playing, not wanting to) and not already
 * let go, the release is armed for `IDLE_RELEASE_MS`; when it fires, the player is stopped only if it is
 * still paused and not already idle.
 */
internal object LongPause {
    fun arms(isPlaying: Boolean, playWhenReady: Boolean, state: Int): Boolean = !isPlaying && releases(playWhenReady, state)
    fun releases(playWhenReady: Boolean, state: Int): Boolean = !playWhenReady && state != Player.STATE_IDLE
}
