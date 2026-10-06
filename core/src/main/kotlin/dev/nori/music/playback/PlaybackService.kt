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
import dev.nori.music.settings.loggedIn
import dev.nori.music.data.StarKind
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.settings.StoredPrefs
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
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
        const val CMD_IN_SIGHT = "nori.inSight"
        /** The notification's and lock screen's heart: favourite or unfavourite the current song. */
        const val CMD_FAVOURITE = "nori.favourite"
        /** The notification's and lock screen's shuffle toggle. */
        const val CMD_SHUFFLE = "nori.shuffle"
        /**
         * Next was pressed with nothing after the current song. Autofill may still be fetching similar
         * songs: remember the skip and take it when they land, instead of the press dying as a no-op.
         */
        const val CMD_FILL_NEXT = "nori.fillNext"
        /** The car's own now-playing buttons: repeat, turned on round (off, all, one), and a radio from the song playing. */
        const val CMD_REPEAT = "nori.repeat"
        const val CMD_RADIO = "nori.radio"
        /** Broadcast inside the package on every track or play-state change while a home-screen widget is placed (PlacedWidgets). */
        const val ACTION_STATE = "dev.nori.music.STATE"
        const val EXTRA_TITLE = "title"
        const val EXTRA_ARTIST = "artist"
        const val EXTRA_PLAYING = "playing"
        const val EXTRA_ID = "id"
        const val EXTRA_COVER = "cover"
        /** Where the song was when this was sent, ms, at [EXTRA_AT] (elapsedRealtime), moving at [EXTRA_SPEED]. */
        const val EXTRA_POSITION = "position"
        const val EXTRA_AT = "at"
        const val EXTRA_SPEED = "speed"
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
        /**
         * The engine said it landed a jump (its position event, on the main thread). A seek held until then
         * is let go at once rather than at media3's next event: the landing changes nothing the controller
         * has not already shown, so none may come until the song changes. Set by PlayerConnection.
         */
        @Volatile var onLanded: (() -> Unit)? = null
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
    /** The car's tree as media3 items. */
    private val car by lazy { CarTree(this) }
    /** How many tabs the car shows at its root, as it last said (Android Auto: four). */
    @Volatile private var rootLimit = 4
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
     * The network turning unmetered, for the AutoEQ list. One registration for the service's life; a change
     * that leaves the answer as it was is not passed on.
     */
    private val connectivity by lazy { getSystemService(android.net.ConnectivityManager::class.java) }
    private var metered: Boolean? = null
    private val network = object : android.net.ConnectivityManager.NetworkCallback() {
        override fun onCapabilitiesChanged(network: android.net.Network, caps: android.net.NetworkCapabilities) =
            meteredNow(!caps.hasCapability(android.net.NetworkCapabilities.NET_CAPABILITY_NOT_METERED))
    }
    private fun meteredNow(now: Boolean) {
        if (now == metered) return
        metered = now
        // Onto Wi-Fi: the AutoEQ list is fetched if the core says it is due, and nothing happens otherwise.
        if (!now) scope.launch { nori.keepAutoEqList() }
    }

    override fun onCreate() {
        super.onCreate()
        nori = Nori.get(this)
        scrobbler = Scrobbler(nori, scope)
        meteredNow(nori.http.metered)
        runCatching { connectivity.registerDefaultNetworkCallback(network, main) }
        player = EnginePlayer(this, nori).also { rustPlayer = it }
        engine = "rust"
        observer?.engine(engine)
        // A song that has become whole on the device is measured then, whoever fetched it (see AutoMixPrefetch).
        analyser = AutoMixPrefetch(nori.sources, nori.analyses) { player.replan() }
        // Nothing is watched until a bridge starts (see OfflineBridge). The player says itself when the
        // core handed a failure to the bridge.
        offlineBridge = OfflineBridge(this, player, { nori.core }, nori.session, main, ::applyEdit, ::skipAfterError)
        player.onBridge = ::bridge
        player.addListener(listener)
        nori.widgets.onPlaced = ::announce

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
            // The queue's covers are the app's own content addresses (CarArt), which a car can open too; the
            // server's are fetched by the provider through the same pool.
            .setBitmapLoader(CacheBitmapLoader(DataSourceBitmapLoader.Builder(this).setDataSourceFactory(androidx.media3.datasource.DefaultDataSource.Factory(this, nori.sources.network)).setMaximumOutputDimension(512).build()))
            // Controllers extrapolate the playhead themselves; a broadcast every few seconds is a wake-up for nothing.
            .setPeriodicPositionUpdateEnabled(false)
            // What a long press on a row offers in the car (CarTree).
            .setCommandButtonsForMediaItems(car.itemButtons())
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
        // Controllable from the account's other devices while this runs and remote control is on (Remotes).
        nori.remotes.service = remotePlayer
        scope.launch { nori.settings.prefs.map { it.remoteControl }.distinctUntilChanged().collect { nori.remotes.serve(true) } }
        restoreQueue()
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo) = session

    override fun onTaskRemoved(rootIntent: android.content.Intent?) {
        if (!player.playWhenReady || player.mediaItemCount == 0) stopSelf()
    }

    override fun onDestroy() {
        nori.remotes.serve(false)
        nori.remotes.service = null
        nori.widgets.onPlaced = null
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
            val steps = nori.session.songArrived()
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

        // A seek moves the lyrics widget's line; the song's own playing on is worked out from the last announce.
        override fun onPositionDiscontinuity(oldPosition: Player.PositionInfo, newPosition: Player.PositionInfo, reason: Int) {
            if (reason == Player.DISCONTINUITY_REASON_SEEK) announce()
        }

        override fun onPlaybackParametersChanged(playbackParameters: androidx.media3.common.PlaybackParameters) = announce()

        override fun onIsLoadingChanged(isLoading: Boolean) {
            if (isLoading && !wifiLock.isHeld) wifiLock.acquire() else if (!isLoading && wifiLock.isHeld) wifiLock.release()
        }

        override fun onIsPlayingChanged(isPlaying: Boolean) {
            // Sound is coming out: whatever failed before it is no longer a run (rules.rs queue_playing).
            if (isPlaying) nori.session.queuePlaying()
            observer?.playing(isPlaying)
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

        override fun onShuffleModeEnabledChanged(on: Boolean) { refreshButtons(); remoteState() }

        override fun onRepeatModeChanged(repeatMode: Int) { refreshButtons(); remoteState() }

        override fun onTimelineChanged(timeline: androidx.media3.common.Timeline, reason: Int) {
            if (reason == Player.TIMELINE_CHANGE_REASON_PLAYLIST_CHANGED) {
                keepQueue(dev.nori.music.ffi.queue.QueueMoment.EDITED)
                remoteState()
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
        offlineBridge?.take() ?: nori.session.queueBridgeFailed().also { if (it) skipAfterError() }

    /** The core said to skip a song that will not play (and counted it). */
    private fun skipAfterError() {
        player.seekToNextMediaItem()
        player.prepare()
        player.play()
    }

    /** What the heart, shuffle and repeat buttons last showed, so an unrelated change does not rebuild the notification. */
    private var buttonsShown: dev.nori.music.ffi.SessionButtons? = null
    private var repeatShown = -1

    private fun currentStarred(item: MediaItem): Boolean =
        nori.library.isStarred(StarKind.SONG, item.mediaId, nori.session.queueStarred(item.mediaId))

    /**
     * Heart and shuffle beside previous / play / next, in the secondary slots the way other players put
     * them. Called when the track, the shuffle flag or a star changes - never on a timer. Whether the
     * heart shows and what each does are the core's (shown.rs session_buttons_now); what they say, ours.
     */
    private fun refreshButtons() {
        if (!::session.isInitialized) return
        val item = player.currentMediaItem
        val b = nori.core.sessionButtonsNow(item != null && currentStarred(item), player.shuffleModeEnabled)
        if (b == buttonsShown && player.repeatMode == repeatShown) return
        buttonsShown = b
        repeatShown = player.repeatMode
        val buttons = ArrayList<CommandButton>(4)
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
        // Then repeat and a radio from a song of the library, in the overflow: a car's now playing lists them
        // after the heart and shuffle; the phone's media controls, which show two, keep those.
        val (icon, said) = when (player.repeatMode) {
            Player.REPEAT_MODE_ALL -> CommandButton.ICON_REPEAT_ALL to R.string.car_repeat_all
            Player.REPEAT_MODE_ONE -> CommandButton.ICON_REPEAT_ONE to R.string.car_repeat_one
            else -> CommandButton.ICON_REPEAT_OFF to R.string.car_repeat_off
        }
        buttons += CommandButton.Builder(icon).setDisplayName(getString(said)).setSessionCommand(SessionCommand(CMD_REPEAT, Bundle.EMPTY))
            .setSlots(CommandButton.SLOT_OVERFLOW).build()
        if (b.heart) buttons += CommandButton.Builder(CommandButton.ICON_RADIO).setDisplayName(getString(R.string.car_start_radio))
            .setSessionCommand(SessionCommand(CMD_RADIO, Bundle.EMPTY)).setSlots(CommandButton.SLOT_OVERFLOW).build()
        session.setMediaButtonPreferences(buttons)
    }

    /** Pause once the song playing ends (the sleep timer's "end of this song"). */
    private fun pauseAtEnd(on: Boolean) {
        player.pauseAtEndOfItem = on
    }

    /** Tells the remote control (if it is on) the player changed; a null check otherwise. */
    private fun remoteState() = nori.remotes.played(player.playWhenReady, player.currentPosition, player.currentMediaItemIndex)

    /** What another device asks of this one through the remote control, done to the player as the session's controllers would. */
    private val remotePlayer = object : dev.nori.music.ffi.RemotePlayer {
        override fun apply(op: dev.nori.music.ffi.remote.Op) {
            when (op) {
                dev.nori.music.ffi.remote.Op.Play -> { if (controls.playbackState == Player.STATE_IDLE) controls.prepare(); controls.play() }
                dev.nori.music.ffi.remote.Op.Pause -> controls.pause()
                is dev.nori.music.ffi.remote.Op.Seek -> controls.seekTo(op.ms)
                dev.nori.music.ffi.remote.Op.Next -> controls.seekToNext()
                dev.nori.music.ffi.remote.Op.Previous -> controls.seekToPrevious()
                is dev.nori.music.ffi.remote.Op.Jump -> { controls.seekToDefaultPosition(op.index.toInt()); if (controls.playbackState == Player.STATE_IDLE) controls.prepare(); controls.play() }
                is dev.nori.music.ffi.remote.Op.Remove -> controls.removeMediaItem(op.index.toInt())
                is dev.nori.music.ffi.remote.Op.Move -> controls.moveMediaItem(op.from.toInt(), op.to.toInt())
                is dev.nori.music.ffi.remote.Op.Add -> {
                    controls.addMediaItems(items(op.songs).map { it.queued(if (op.next) dev.nori.music.ffi.queue.Hand.NEXT else dev.nori.music.ffi.queue.Hand.LAST) })
                    if (controls.playbackState == Player.STATE_IDLE) controls.prepare()
                }
                is dev.nori.music.ffi.remote.Op.Replace -> {
                    controls.shuffleModeEnabled = false
                    controls.setMediaItems(items(op.songs), op.index.toInt(), op.positionMs)
                    controls.prepare()
                    controls.playWhenReady = op.play
                }
                is dev.nori.music.ffi.remote.Op.Volume -> dev.nori.music.remote.Remotes.setVolume(this@PlaybackService, op.percent.toInt())
                is dev.nori.music.ffi.remote.Op.Shuffle -> controls.shuffleModeEnabled = op.on
                is dev.nori.music.ffi.remote.Op.Repeat -> controls.repeatMode = op.mode.toInt()
                // The core keeps transfers and jam ops to itself.
                is dev.nori.music.ffi.remote.Op.Transfer, is dev.nori.music.ffi.remote.Op.Request, is dev.nori.music.ffi.remote.Op.Decide,
                is dev.nori.music.ffi.remote.Op.Promote, is dev.nori.music.ffi.remote.Op.Kick -> {}
            }
            remoteState()
        }
    }

    private fun announce() {
        remoteState()
        if (!nori.widgets.any) return
        val m = player.currentMediaItem?.mediaMetadata
        sendBroadcast(android.content.Intent(ACTION_STATE).setPackage(packageName)
            .putExtra(EXTRA_TITLE, m?.title?.toString()).putExtra(EXTRA_ARTIST, m?.artist?.toString()).putExtra(EXTRA_PLAYING, player.isPlaying)
            .putExtra(EXTRA_ID, player.currentMediaItem?.mediaId).putExtra(EXTRA_COVER, m?.artworkUri?.let { a -> CarArt.coverOf(a)?.let { (id, size) -> nori.library.coverUrl(id, size) } ?: a.toString() })
            .putExtra(EXTRA_POSITION, player.currentPosition).putExtra(EXTRA_AT, android.os.SystemClock.elapsedRealtime())
            .putExtra(EXTRA_SPEED, player.playbackParameters.speed))
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
            val back = mediaItems.singleOrNull()?.takeIf { it.isRestored() }?.let { nori.session.playlistRestore(it.mediaId) }?.takeIf { it.at != null }
            val c = back ?: nori.session.playlistTake(at, ids(mediaItems), mediaItems.map { it.queuedAs() ?: Hand.NO })
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
            val c = if (ordered) nori.session.playlistSetOrdered(ids(mediaItems), origin)
            else nori.session.playlistSet(ids(mediaItems), startIndex.coerceAtMost(mediaItems.size - 1).takeIf { it >= 0 }?.toUInt(), wrappedPlayer.shuffleModeEnabled, origin)
            if (ordered && wrappedPlayer.shuffleModeEnabled) super.setShuffleModeEnabled(false)
            super.setMediaItems(mediaItems, c.at?.toInt() ?: 0, if (startIndex == C.INDEX_UNSET) C.TIME_UNSET else startPositionMs)
            onQueueSet?.invoke()
        }
        override fun clearMediaItems() {
            offlineBridge?.abandon()
            nori.session.playlistSet(emptyList(), null, false, null)
            super.clearMediaItems()
            onQueueSet?.invoke()
        }
        override fun removeMediaItem(index: Int) = removeMediaItems(index, index + 1)
        override fun removeMediaItems(fromIndex: Int, toIndex: Int) {
            // The player drops removed songs from its play order and keeps the rest as it was, as the core does.
            nori.session.playlistRemove(fromIndex.toUInt(), toIndex.toUInt())
            super.removeMediaItems(fromIndex, toIndex)
        }
        override fun moveMediaItem(currentIndex: Int, newIndex: Int) = moveMediaItems(currentIndex, currentIndex + 1, newIndex)
        override fun moveMediaItems(fromIndex: Int, toIndex: Int, newIndex: Int) {
            nori.session.playlistMove(fromIndex.toUInt(), toIndex.toUInt(), newIndex.toUInt())
            super.moveMediaItems(fromIndex, toIndex, newIndex)
        }
        /** Shuffle on: the playing song first, the songs added by hand after it, the rest shuffled. */
        override fun setShuffleModeEnabled(shuffleModeEnabled: Boolean) {
            nori.session.playlistShuffle(shuffleModeEnabled)
            super.setShuffleModeEnabled(shuffleModeEnabled)
        }
        override fun setRepeatMode(repeatMode: Int) {
            nori.session.playlistRepeat(repeatMode.toUByte())
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
            if (!nori.session.queuePreviousRestarts(currentPosition, hasPreviousMediaItem()) && hasPreviousMediaItem()) super.seekToPreviousMediaItem()
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
        val fresh = runCatching { nori.library.autofill() }.getOrNull() ?: dev.nori.music.ffi.Refill(emptyList(), null, null)
        // Player work stays on this scope's main dispatcher.
        if (nori.library.autofillArrived(fresh)) {
            controls.addMediaItems(held(fresh.songs))
        }
        // A next pressed at the end while these were on the way is taken now, if the user is still there
        // and pressed it moments ago; a press the user has long since settled after is not.
        if (nori.session.autofillLanded()) player.seekToNextMediaItem()
    }

    /** Next with nothing after: fetch, and take the skip when the songs land (the core remembers the press). */
    private fun fillThenNext() {
        when (nori.session.autofillNext()) {
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
    private fun items(songs: List<Song>): List<MediaItem> = songs.toMediaItems(nori.session) { CarArt.cover(this, it.coverArt, NOTIFICATION_ART)?.toString() }
    private fun item(s: Song): MediaItem = items(listOf(s)).first()
    /** Songs the core made for the queue and already keeps (MediaItems.heldMediaItems): nothing handed back. */
    private fun held(songs: List<Song>): List<MediaItem> = songs.heldMediaItems { CarArt.cover(this, it.coverArt, NOTIFICATION_ART)?.toString() }

    // ---- session: custom commands, Android Auto browsing, voice search ----

    /** Controllers whose screen is in sight: the app's while it is started, a car's while it is connected. */
    private val inSight = mutableSetOf<MediaSession.ControllerInfo>()

    /**
     * [controller]'s screen came in sight ([on]) or left it; the player hears only when whether any is
     * changes. While one is, the track holds a fraction of a second, so a sound change is heard at once
     * (crates/android/src/track.rs); with none, the deep buffer that lets the phone sleep.
     */
    private fun inSight(controller: MediaSession.ControllerInfo, on: Boolean) {
        val was = inSight.isNotEmpty()
        if (on) inSight += controller else inSight -= controller
        if (inSight.isNotEmpty() == was) return
        player.setForeground(!was)
        observer?.shallow(!was)
    }

    private inner class Callback : MediaLibrarySession.Callback {
        override fun onConnect(session: MediaSession, controller: MediaSession.ControllerInfo): MediaSession.ConnectionResult {
            val commands = MediaSession.ConnectionResult.DEFAULT_SESSION_AND_LIBRARY_COMMANDS.buildUpon().add(SessionCommand(CMD_SLEEP, Bundle.EMPTY)).add(SessionCommand(CMD_IN_SIGHT, Bundle.EMPTY))
                .add(SessionCommand(CMD_FAVOURITE, Bundle.EMPTY)).add(SessionCommand(CMD_SHUFFLE, Bundle.EMPTY))
                .add(SessionCommand(CMD_FILL_NEXT, Bundle.EMPTY)).add(SessionCommand(CMD_REPEAT, Bundle.EMPTY)).add(SessionCommand(CMD_RADIO, Bundle.EMPTY))
                .add(SessionCommand(CarTree.CMD_ITEM_NEXT, Bundle.EMPTY)).add(SessionCommand(CarTree.CMD_ITEM_QUEUE, Bundle.EMPTY))
                .add(SessionCommand(CarTree.CMD_ITEM_FAVOURITE, Bundle.EMPTY)).add(SessionCommand(CarTree.CMD_ITEM_DOWNLOAD, Bundle.EMPTY)).build()
            if (session.isAutoCompanionController(controller) || session.isAutomotiveController(controller)) inSight(controller, true)
            return MediaSession.ConnectionResult.AcceptedResultBuilder(session).setAvailableSessionCommands(commands).build()
        }

        override fun onCustomCommand(session: MediaSession, controller: MediaSession.ControllerInfo, command: SessionCommand, args: Bundle): ListenableFuture<SessionResult> {
            if (command.customAction == CMD_SLEEP) {
                val alarms = getSystemService(AlarmManager::class.java)
                alarms.cancel(sleepAlarm)
                // The songs still to go are counted in the core (rules.rs sleep_set, sleep_song_changed).
                pauseAtEnd(nori.session.sleepSet(args.getInt(ARG_SONGS).coerceAtLeast(0).toUInt(), args.getBoolean(ARG_END_OF_TRACK)))
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
            if (command.customAction == CMD_IN_SIGHT) inSight(controller, args.getBoolean(ARG_ON))
            if (command.customAction == CMD_REPEAT) controls.repeatMode = when (player.repeatMode) {
                Player.REPEAT_MODE_OFF -> Player.REPEAT_MODE_ALL
                Player.REPEAT_MODE_ALL -> Player.REPEAT_MODE_ONE
                else -> Player.REPEAT_MODE_OFF
            }
            if (command.customAction == CMD_RADIO) player.currentMediaItem?.mediaId?.let(::radioFrom)
            args.getString(androidx.media3.session.MediaConstants.EXTRA_KEY_MEDIA_ID)?.let { id -> carItemCommand(command.customAction, id) }
            return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
        }

        // A screen in sight goes with its controller: the app stopped (it lets go of the service), its
        // process died, or the car disconnected.
        override fun onDisconnected(session: MediaSession, controller: MediaSession.ControllerInfo) {
            inSight(controller, false)
        }

        override fun onAddMediaItems(session: MediaSession, controller: MediaSession.ControllerInfo, items: MutableList<MediaItem>): ListenableFuture<MutableList<MediaItem>> {
            val query = items.singleOrNull()?.requestMetadata?.searchQuery
            if (query != null) return scope.future { items(nori.library.search(query).songs).toMutableList() }
            // Our own UI sends complete items. Android Auto sends bare ids of things it was shown earlier.
            if (items.all { it.mediaMetadata.title != null }) return Futures.immediateFuture(items.map { it.playable() }.toMutableList())
            return scope.future {
                items.mapNotNull { i ->
                    // A row of the car's tree carries its folder with the song's id (crates/library/src/car.rs).
                    val id = dev.nori.music.ffi.library.carRow(i.mediaId)?.song ?: i.mediaId
                    served.get(id) ?: withContext(Dispatchers.IO) { runCatching { nori.library.song(id) }.getOrNull() }?.let(::item)
                }.toMutableList()
            }
        }

        override fun onPlaybackResumption(session: MediaSession, controller: MediaSession.ControllerInfo): ListenableFuture<MediaSession.MediaItemsWithStartPosition> = scope.future {
            val q = withContext(Dispatchers.IO) { nori.core.loadQueue() }
            MediaSession.MediaItemsWithStartPosition(startedFrom(held(q.songs), q.origin), q.index.toInt(), q.positionMs.toLong())
        }

        override fun onGetLibraryRoot(session: MediaLibrarySession, browser: MediaSession.ControllerInfo, params: LibraryParams?): ListenableFuture<LibraryResult<MediaItem>> {
            params?.extras?.getInt(androidx.media3.session.MediaConstants.EXTRAS_KEY_ROOT_CHILDREN_LIMIT, 0)?.takeIf { it > 0 }?.let { rootLimit = it }
            // Signed out, the car says so and offers to open the app on the phone, rather than an empty tree.
            if (!nori.settings.value.loggedIn) return Futures.immediateFuture(failure(signedOut(), params))
            return Futures.immediateFuture(LibraryResult.ofItem(car.folder(nori.client.browseRoot()), params))
        }

        // The car's folders are read and made into rows on an IO thread: the core's reads, parses and the
        // songs' items are work the main thread, which carries the session and the player, does not wait on.
        override fun onGetChildren(session: MediaLibrarySession, browser: MediaSession.ControllerInfo, parentId: String, page: Int, pageSize: Int, params: LibraryParams?): ListenableFuture<LibraryResult<ImmutableList<MediaItem>>> =
            scope.future(Dispatchers.IO) {
                val found = runCatching {
                    if (parentId == nori.client.browseRoot().id) nori.client.carRoot(rootLimit.toUInt(), offline(), page.toUInt(), pageSize.toUInt())
                    else nori.client.browseChildren(parentId, page.toUInt(), pageSize.toUInt())
                }.getOrNull()
                if (found == null || found.failed) return@future failure(unreachable(), params)
                listed(parentId, found, params)
            }

        override fun onSearch(session: MediaLibrarySession, browser: MediaSession.ControllerInfo, query: String, params: LibraryParams?): ListenableFuture<LibraryResult<Void>> {
            scope.launch {
                val found = withContext(Dispatchers.IO) { runCatching { nori.client.carSearch(query) }.getOrNull() }
                session.notifySearchResultChanged(browser, query, found?.let { it.folders.size + it.songs.size } ?: 0, params)
            }
            return Futures.immediateFuture(LibraryResult.ofVoid())
        }

        override fun onGetSearchResult(session: MediaLibrarySession, browser: MediaSession.ControllerInfo, query: String, page: Int, pageSize: Int, params: LibraryParams?): ListenableFuture<LibraryResult<ImmutableList<MediaItem>>> =
            scope.future(Dispatchers.IO) {
                val found = runCatching { nori.client.browseChildren("search:$query", page.toUInt(), pageSize.toUInt()) }.getOrNull()
                if (found == null || found.failed) return@future failure(unreachable(), params)
                listed("search:$query", found, params)
            }

        /**
         * The car's picks: a row of the tree plays its folder from it (or the whole folder, for Play and
         * Shuffle and a folder played as it is listed), a spoken request what it names. Anything else is
         * media3's own way, through [onAddMediaItems].
         */
        override fun onSetMediaItems(session: MediaSession, controller: MediaSession.ControllerInfo, items: MutableList<MediaItem>, startIndex: Int, startPositionMs: Long): ListenableFuture<MediaSession.MediaItemsWithStartPosition> {
            val one = items.singleOrNull()
            val query = one?.requestMetadata?.searchQuery
            if (query != null) return scope.future { spoken(query, one.requestMetadata.extras) }
            val row = one?.mediaId?.let(::rowOf)
            if (row != null) return scope.future { queued(withContext(Dispatchers.IO) { runCatching { nori.client.carQueue(row) }.getOrNull() }) }
            return super.onSetMediaItems(session, controller, items, startIndex, startPositionMs)
        }
    }

    /** Row [id] of the car's tree, or a folder played as it is listed as its Play row; null for anything else. */
    private fun rowOf(id: String): String? = when {
        dev.nori.music.ffi.library.carRow(id) != null -> id
        dev.nori.music.ffi.library.carPlaysWhole(id) -> dev.nori.music.ffi.library.carActionRow(id, dev.nori.music.ffi.library.CarAction.PLAY)
        else -> null
    }

    /** A page of folder [parent], [found], as the car's rows; the songs kept for a later pick by bare id. */
    private fun listed(parent: String, found: dev.nori.music.ffi.library.BrowsePage, params: LibraryParams?): LibraryResult<ImmutableList<MediaItem>> {
        val made = items(found.songs)
        found.songs.forEachIndexed { i, s -> served.put(s.id, made[i]) }
        return LibraryResult.ofItemList(car.items(parent, found, made), params)
    }

    /** A queue for the car: [q]'s songs as the player's items, from its song, shuffled when it asks. */
    private fun queued(q: dev.nori.music.ffi.library.CarQueue?): MediaSession.MediaItemsWithStartPosition {
        // Nothing to play fails the request: the car says so, rather than holding an empty item as the queue.
        check(q != null && q.songs.isNotEmpty()) { "nothing to play" }
        // As the app's Play and Shuffle do (PlayerConnection.play): the shuffle light said to the core first.
        nori.session.playlistShowShuffle(q.shuffle)
        controls.shuffleModeEnabled = q.shuffle
        return MediaSession.MediaItemsWithStartPosition(startedFrom(items(q.songs), q.origin), if (q.shuffle) C.INDEX_UNSET else q.index.toInt(), C.TIME_UNSET)
    }

    /**
     * What a spoken request plays ("play ... on nori"): nothing named, the queue as it was left, or else
     * Quick picks; a mix by its name, which only the app words; anything else the core's `car_voice`.
     */
    private suspend fun spoken(query: String, extras: Bundle?): MediaSession.MediaItemsWithStartPosition {
        if (query.isBlank()) {
            val q = withContext(Dispatchers.IO) { runCatching { nori.core.loadQueue() }.getOrNull() }
            if (q != null && q.songs.isNotEmpty()) return MediaSession.MediaItemsWithStartPosition(startedFrom(held(q.songs), q.origin), q.index.toInt(), q.positionMs.toLong())
        }
        val tiles = withContext(Dispatchers.IO) { runCatching { nori.core.mixCards(nori.settings.value.tasteModel) }.getOrDefault(emptyList()) }
        val mix = if (query.isBlank()) tiles.firstOrNull { it.name == dev.nori.music.ffi.library.MixName.QUICK_PICKS } ?: tiles.firstOrNull()
        else tiles.firstOrNull { CarWords.mix(it.name).equals(query.trim(), ignoreCase = true) }
        if (mix != null) return queued(withContext(Dispatchers.IO) { runCatching { nori.client.carQueue(dev.nori.music.ffi.library.carActionRow("mix:${mix.id}", dev.nori.music.ffi.library.CarAction.PLAY)) }.getOrNull() })
        val focus = when (extras?.getString(android.provider.MediaStore.EXTRA_MEDIA_FOCUS)) {
            android.provider.MediaStore.Audio.Artists.ENTRY_CONTENT_TYPE -> dev.nori.music.ffi.library.VoiceFocus.ARTIST
            android.provider.MediaStore.Audio.Albums.ENTRY_CONTENT_TYPE -> dev.nori.music.ffi.library.VoiceFocus.ALBUM
            android.provider.MediaStore.Audio.Playlists.ENTRY_CONTENT_TYPE -> dev.nori.music.ffi.library.VoiceFocus.PLAYLIST
            android.provider.MediaStore.Audio.Genres.ENTRY_CONTENT_TYPE -> dev.nori.music.ffi.library.VoiceFocus.GENRE
            android.provider.MediaStore.Audio.Media.ENTRY_CONTENT_TYPE -> dev.nori.music.ffi.library.VoiceFocus.SONG
            else -> dev.nori.music.ffi.library.VoiceFocus.ANY
        }
        val ask = dev.nori.music.ffi.library.VoiceAsk(
            query, focus, extras?.getString(android.provider.MediaStore.EXTRA_MEDIA_ARTIST), extras?.getString(android.provider.MediaStore.EXTRA_MEDIA_ALBUM),
            extras?.getString(android.provider.MediaStore.EXTRA_MEDIA_TITLE), extras?.getString(android.provider.MediaStore.EXTRA_MEDIA_GENRE),
            extras?.getString(android.provider.MediaStore.EXTRA_MEDIA_PLAYLIST),
        )
        return queued(withContext(Dispatchers.IO) { runCatching { nori.client.carVoice(ask) }.getOrNull() })
    }

    /** The car's long press on row or folder [id]: queue it next or last, heart it, download it. */
    private fun carItemCommand(action: String, id: String) = scope.launch {
        val song = dev.nori.music.ffi.library.carRow(id)?.song
        val whole = rowOf(id)
        val songs: List<Song> = runCatching {
            when {
                // A song's row is that song alone; Play, Shuffle or a folder, the whole of it.
                song != null -> listOfNotNull(withContext(Dispatchers.IO) { nori.library.song(song) })
                whole != null -> withContext(Dispatchers.IO) { nori.client.carQueue(whole) }?.songs.orEmpty()
                else -> listOfNotNull(withContext(Dispatchers.IO) { nori.library.song(id) })
            }
        }.getOrDefault(emptyList())
        if (songs.isEmpty()) return@launch
        when (action) {
            CarTree.CMD_ITEM_NEXT, CarTree.CMD_ITEM_QUEUE -> {
                val how = if (action == CarTree.CMD_ITEM_NEXT) Hand.NEXT else Hand.LAST
                controls.addMediaItems(items(songs).map { it.queued(how) })
                if (player.playbackState == Player.STATE_IDLE) controls.prepare()
            }
            CarTree.CMD_ITEM_FAVOURITE -> songs.first().let { s ->
                runCatching { nori.library.star(StarKind.SONG, s.id, !nori.library.isStarred(StarKind.SONG, s.id, s.starred)) }
            }
            CarTree.CMD_ITEM_DOWNLOAD -> nori.downloads.download(songs)
        }
    }

    /** The songs after the one playing become a radio from it (the server's similar songs, as the app's "Start radio"). */
    private fun radioFrom(id: String) = scope.launch {
        val songs = runCatching { withContext(Dispatchers.IO) { dev.nori.music.net.lifted { nori.client.radio(id) } } }.getOrNull()?.filter { it.id != id }
        if (songs.isNullOrEmpty()) return@launch
        nori.session.playlistShowShuffle(false)
        controls.shuffleModeEnabled = false
        val at = controls.currentMediaItemIndex
        if (at + 1 < controls.mediaItemCount) controls.removeMediaItems(at + 1, controls.mediaItemCount)
        controls.addMediaItems(items(songs))
    }

    private fun offline(): Boolean = getSystemService(android.net.ConnectivityManager::class.java)?.activeNetwork == null

    /** Signed out: the car's message, and a button that opens the app on the phone. */
    private fun signedOut(): androidx.media3.session.SessionError {
        val open = packageManager.getLaunchIntentForPackage(packageName)?.let { PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE) }
        val extras = Bundle().apply {
            putString(androidx.media3.session.MediaConstants.EXTRAS_KEY_ERROR_RESOLUTION_ACTION_LABEL_COMPAT, getString(R.string.car_open_nori))
            open?.let { putParcelable(androidx.media3.session.MediaConstants.EXTRAS_KEY_ERROR_RESOLUTION_ACTION_INTENT_COMPAT, it) }
        }
        return androidx.media3.session.SessionError(androidx.media3.session.SessionError.ERROR_SESSION_AUTHENTICATION_EXPIRED, getString(R.string.car_signed_out), extras)
    }

    private fun <T : Any> failure(e: androidx.media3.session.SessionError, params: LibraryParams?): LibraryResult<T> =
        if (params != null) LibraryResult.ofError<T>(e, params) else LibraryResult.ofError<T>(e)

    private fun unreachable() = androidx.media3.session.SessionError(androidx.media3.session.SessionError.ERROR_IO, getString(R.string.car_unreachable))

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

    /** The output's shallow buffer (the app in sight) came on or off. */
    fun shallow(on: Boolean)

    /** The player started or stopped sounding. */
    fun playing(on: Boolean) {}

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
