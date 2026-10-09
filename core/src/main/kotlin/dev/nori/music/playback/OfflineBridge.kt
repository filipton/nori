package dev.nori.music.playback

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
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
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * When the server is gone mid-evening and the next queued song is not on the phone, keep playing from
 * full downloads until the network is back, then return to the parked queue. Off by default; when off
 * this object never registers a network callback and costs nothing.
 *
 * Which downloads, where they go and what comes back is the core's (crates/queue/src/bridge.rs over its
 * queue, nori_player::playlist), and so is whether the server is back (crates/core/src/bridge.rs: it
 * answers a ping): this reports the phone moving to another network, and the service makes each change
 * the core made to its player ([apply]).
 */
@androidx.media3.common.util.UnstableApi
class OfflineBridge(
    context: Context,
    private val player: Player,
    private val core: () -> dev.nori.music.ffi.Core,
    private val client: () -> dev.nori.music.ffi.Client,
    private val session: dev.nori.music.ffi.queue.Session,
    private val scope: CoroutineScope,
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
     * if the server is, or more downloads go in before it if it is not (`Client::bridge_parked`).
     */
    fun onSong(step: BridgeStep) {
        when (step) {
            BridgeStep.OFF, BridgeStep.BRIDGING -> {}
            BridgeStep.IDLE -> stopWatching()
            BridgeStep.PARKED -> edited { client().bridgeParked() }
        }
    }

    /** The core's edit, asked off the main thread and made here; once the queue is back nothing is watched. */
    private fun edited(ask: suspend () -> QueueEdit?) = scope.launch {
        val edit = withContext(Dispatchers.IO) { runCatching { ask() }.getOrNull() } ?: return@launch
        apply(edit)
        if (!session.playlistBridgeState().bridging) {
            Log.i(TAG, "resumed the parked queue")
            stopWatching()
        }
    }

    /** A new queue replaced the bridged one (the core dropped the bridge with it): stop watching. */
    fun abandon() = stopWatching()

    private fun watchNetwork() {
        if (networkCallback != null) return
        // The callback is told of the network already up at once: that one did not bring the song, so only
        // another network is a change worth asking the server about.
        val failedOn = connectivity.activeNetwork
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                if (network != failedOn) edited { client().bridgeNetworkChanged() }
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
