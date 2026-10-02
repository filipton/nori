package dev.nori.music.playback

import android.os.SystemClock
import dev.nori.music.Nori
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

/**
 * Counting plays is the core's (crates/queue/src/scrobble.rs): it sums listening time from play and
 * pause edges, judges a song when it is left, and records it in the local history. This hands it the
 * edges and sends the server what the core says to.
 */
class Scrobbler(private val nori: Nori, private val scope: CoroutineScope) {
    fun onPlaying(playing: Boolean) = nori.session.scrobblePlaying(playing, SystemClock.elapsedRealtime())

    /**
     * The player's song changed to [id] ([why]). What that song is followed as (a radio stream moved onto
     * is not), whether to record and send, and when a play counts, the core decides over its settings.
     */
    fun onTrack(id: String?, why: dev.nori.music.ffi.queue.TrackChange, playing: Boolean) {
        val wall = System.currentTimeMillis()
        val send = nori.session.scrobbleTrack(id, why, playing, SystemClock.elapsedRealtime(), wall, java.util.TimeZone.getDefault().getOffset(wall))
        if (send.submitId == null && send.nowPlayingId == null) return
        scope.launch(Dispatchers.IO) {
            // Both are writes: made offline, they wait in the pending queue and keep their original time.
            send.submitId?.let { runCatching { nori.library.scrobble(it, submission = true, timeMs = send.submitAt) } }
            send.nowPlayingId?.let { runCatching { nori.library.nowPlaying(it) } }
        }
    }
}
