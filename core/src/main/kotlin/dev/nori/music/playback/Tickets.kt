package dev.nori.music.playback

import okhttp3.Call
import java.util.concurrent.ConcurrentHashMap

/**
 * One request of a song's bytes that the Rust player may call off (crates/engine/src/source.rs `Cancel`):
 * the song let go while its server has not answered or sends nothing, its answer or next byte too late,
 * newer requests crowding it out. Left running, it held a slot of the stream dispatcher and a connection's
 * stream, and the stream cache's lock on the song, until the server gave up - which a server that never
 * answers (octo-fiesta fetching a provider's song it cannot have) never does, so after a few skips nothing
 * from that server could start. Called off, its OkHttp call is cancelled (the open or the body's read
 * fails at once, and the data source let go releases the cache entry) and a wait for the cache entry's
 * lock is interrupted.
 */
internal class Ticket {
    @Volatile var cancelled = false
        private set
    private var call: Call? = null
    /** The thread opening it, while it opens: the only time an interrupt is meant for this request. */
    private var opening: Thread? = null

    /** The OkHttp call made for this request (a new one for each piece the cache asks the network for). */
    @Synchronized fun track(c: Call) {
        call = c
        if (cancelled) c.cancel()
    }

    @Synchronized fun opening() {
        opening = Thread.currentThread()
    }

    /** The open returned: an interrupt meant for it and come too late is not left for the thread's next wait. */
    fun opened() {
        synchronized(this) { opening = null }
        Thread.interrupted()
    }

    @Synchronized fun cancel() {
        cancelled = true
        call?.cancel()
        opening?.interrupt()
    }
}

/** One player's requests running, by the number its Rust side gave each (crates/android/src/player.rs `open_java`). */
internal class Tickets {
    private val running = ConcurrentHashMap<Long, Ticket>()

    /** [id]'s ticket, as the request starts: already called off when the Rust side was quicker. */
    fun start(id: Long): Ticket? {
        if (id <= 0) return null
        // A request called off after its ticket was let go leaves one behind: the old ones go now and then.
        if (running.size > 64) running.keys.removeIf { it < id - 64 }
        return running.getOrPut(id) { Ticket() }
    }

    fun end(id: Long) {
        if (id > 0) running.remove(id)
    }

    /** Calls [id]'s request off: now if it runs, as soon as it starts if it has not yet. */
    fun cancel(id: Long) {
        if (id <= 0) return
        running.getOrPut(id) { Ticket() }.cancel()
    }
}
