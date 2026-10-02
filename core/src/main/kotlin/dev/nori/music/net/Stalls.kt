package dev.nori.music.net

import android.os.SystemClock
import okhttp3.Call
import okhttp3.Interceptor
import okhttp3.MediaType
import okhttp3.Response
import okhttp3.ResponseBody
import okio.Buffer
import okio.BufferedSource
import okio.ForwardingSource
import okio.buffer
import java.io.IOException
import java.net.SocketTimeoutException
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit

/**
 * The audio client's stall timeout, kept without okio's read timeout. With a read timeout OkHttp arms
 * its watchdog (okio's AsyncTimeout, the "Okio Watchdog" thread) around every read of a body, and a
 * body is read a network packet at a time: a song coming in at 10 MB/s woke the watchdog well over a
 * thousand times a second. The stream client has no read timeout, so nothing is armed per read; this
 * looks at the calls in flight once every quarter of [limitMs] instead, and cancels one that has been
 * waiting for its answer or for more of its body for longer than that - what the read timeout did, a
 * little later, for a few wakeups a minute while something streams and none otherwise. A cancelled call
 * fails as a timed-out read did.
 */
internal class Stalls(private val limitMs: Long) : Interceptor {
    /** One call in flight: since when it has been waiting for bytes, 0 while it is not. */
    private class Watched(val call: Call) {
        @Volatile var waitingSince = 0L
        @Volatile var stalled = false
    }

    private val watched = ArrayList<Watched>()
    private var check: ScheduledFuture<*>? = null
    private val timer = ScheduledThreadPoolExecutor(1) { Thread(it, "nori-stall").apply { isDaemon = true } }.apply {
        // The thread goes when nothing is in flight.
        setKeepAliveTime(limitMs, TimeUnit.MILLISECONDS)
        allowCoreThreadTimeOut(true)
        removeOnCancelPolicy = true
    }

    override fun intercept(chain: Interceptor.Chain): Response {
        val w = Watched(chain.call())
        w.waitingSince = SystemClock.elapsedRealtime()
        add(w)
        val response = try {
            chain.proceed(chain.request())
        } catch (e: Throwable) {
            remove(w)
            throw if (e is IOException && w.stalled) SocketTimeoutException("no answer in $limitMs ms") else e
        }
        w.waitingSince = 0L
        return response.newBuilder().body(Body(response.body, w)).build()
    }

    private fun add(w: Watched) = synchronized(watched) {
        watched += w
        if (check == null) check = timer.scheduleWithFixedDelay(::look, limitMs / 4, limitMs / 4, TimeUnit.MILLISECONDS)
    }

    private fun remove(w: Watched) = synchronized(watched) {
        if (!watched.remove(w)) return
        if (watched.isEmpty()) { check?.cancel(false); check = null }
    }

    /** Cancels whatever has waited too long; on the timer's thread. */
    private fun look() {
        val now = SystemClock.elapsedRealtime()
        val late = synchronized(watched) { watched.filter { it.waitingSince != 0L && now - it.waitingSince > limitMs } }
        for (w in late) {
            w.stalled = true
            w.call.cancel()
        }
    }

    /** The body, read as it was, with the time each read waits noted: two writes per read, nothing made. */
    private inner class Body(private val body: ResponseBody, private val w: Watched) : ResponseBody() {
        private val source: BufferedSource = object : ForwardingSource(body.source()) {
            override fun read(sink: Buffer, byteCount: Long): Long {
                w.waitingSince = SystemClock.elapsedRealtime()
                try {
                    val n = super.read(sink, byteCount)
                    if (n == -1L) remove(w)
                    return n
                } catch (e: IOException) {
                    remove(w)
                    throw if (w.stalled) SocketTimeoutException("no bytes in $limitMs ms") else e
                } finally {
                    w.waitingSince = 0L
                }
            }

            override fun close() {
                remove(w)
                super.close()
            }
        }.buffer()

        override fun contentType(): MediaType? = body.contentType()
        override fun contentLength(): Long = body.contentLength()
        override fun source(): BufferedSource = source
    }
}
