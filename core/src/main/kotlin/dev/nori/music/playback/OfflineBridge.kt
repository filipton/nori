package dev.nori.music.playback

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.os.Handler
import android.util.Log
import androidx.media3.common.C
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import dev.nori.music.ffi.BridgeTake
import dev.nori.music.ffi.queue.BridgeStep
import dev.nori.music.ffi.queue.QueueEdit
import dev.nori.music.ffi.net.Failure
import dev.nori.music.ffi.net.failureNetworkish
import dev.nori.music.net.failureKind

/**
 * When the server is gone mid-evening and the next queued song is not on the phone, keep playing from
 * full downloads until the network is back, then return to the parked queue. Off by default; when off
 * this object never registers a network callback and costs nothing.
 *
 * Which downloads, where they go and what comes back is the core's (crates/queue/src/bridge.rs over its
 * queue, nori_player::playlist): this watches the network, and the service makes each change the core
 * made to its player ([apply]).
 */
@androidx.media3.common.util.UnstableApi
class OfflineBridge(
    context: Context,
    private val player: Player,
    private val core: () -> dev.nori.music.ffi.Core,
    private val main: Handler,
    private val apply: (QueueEdit) -> Unit,
    private val skip: () -> Unit,
) {
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    /**
     * A song the network would not bring, the core having said the failure is the bridge's: what happens
     * is the core's (`Core::bridge_take`: a download still queued plays, else a bridge starts, else the
     * song is skipped or the music stops), done here to the player. Whether the music goes on.
     */
    fun take(): Boolean = when (val t = runCatching { core().bridgeTake() }.getOrNull()) {
        is BridgeTake.Jump -> {
            player.seekTo(t.index.toInt(), C.TIME_UNSET)
            player.prepare()
            player.play()
            true
        }
        is BridgeTake.Bridged -> {
            apply(t.edit)
            watchNetwork()
            Log.i(TAG, "bridging with ${t.edit.songs.size} downloads")
            true
        }
        BridgeTake.Skip -> { skip(); true }
        BridgeTake.Stop, null -> false
    }

    /**
     * A song arrived, and the core said what it means for the bridge (rules.rs song_arrived): with none
     * playing nothing is watched; once the bridge has played up to the parked song, the queue comes back
     * if the network has, or more downloads go in before it if it has not (`Core::bridge_parked`).
     */
    fun onSong(step: BridgeStep) {
        when (step) {
            BridgeStep.OFF, BridgeStep.BRIDGING -> {}
            BridgeStep.IDLE -> stopWatching()
            BridgeStep.PARKED -> {
                val up = networkUp()
                runCatching { core().bridgeParked(up) }.getOrNull()?.let(apply)
                if (up) stopWatching()
            }
        }
    }

    /** Network came back: the bridge's songs go and the parked song plays. */
    fun resume() {
        dev.nori.music.ffi.queue.playlistUnbridge()?.let { apply(it); Log.i(TAG, "resumed the parked queue") }
        stopWatching()
    }

    /** A new queue replaced the bridged one (the core dropped the bridge with it): stop watching. */
    fun abandon() = stopWatching()

    private fun watchNetwork() {
        if (networkCallback != null) return
        // The callback is told of the network already up at once: that one did not bring the song (the
        // server may be what is gone), so only another network ends the bridge; the parked song tries again.
        val failedOn = connectivity.activeNetwork
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                if (network != failedOn) main.post { resume() }
            }
        }
        runCatching { connectivity.registerDefaultNetworkCallback(cb) }
            .onSuccess { networkCallback = cb }
            .onFailure { Log.w(TAG, "could not watch network for bridge resume", it) }
    }

    private fun stopWatching() {
        networkCallback?.let { runCatching { connectivity.unregisterNetworkCallback(it) } }
        networkCallback = null
    }

    private fun networkUp(): Boolean =
        connectivity.activeNetwork != null &&
            connectivity.getNetworkCapabilities(connectivity.activeNetwork) != null

    private companion object {
        const val TAG = "nori.bridge"
    }
}

/**
 * Network / unreachable server, not a bad file or a refused audio sink. The platform only reads its own
 * error codes and exceptions into kinds; which of them mean the network is the core's
 * (crates/net/src/transport.rs failure_networkish).
 */
fun PlaybackException.isNetworkish(): Boolean {
    val status = when (errorCode) {
        PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
        PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT,
        PlaybackException.ERROR_CODE_TIMEOUT,
        PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS,
        -> true
        else -> false
    }
    val causes = generateSequence(cause) { it.cause }.map { Failure(failureKind(it), it.message) }.toList()
    return failureNetworkish(status, causes)
}
