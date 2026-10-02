package dev.nori.music.playback

import android.content.Context
import dalvik.annotation.optimization.FastNative
import android.net.Uri
import androidx.media3.common.C
import androidx.media3.common.PlaybackException
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.StandaloneDatabaseProvider
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSourceException
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.HttpDataSource
import androidx.media3.datasource.cache.Cache
import androidx.media3.datasource.cache.CacheDataSource
import androidx.media3.datasource.cache.CacheEvictor
import androidx.media3.datasource.cache.CacheSpan
import androidx.media3.datasource.cache.ContentMetadata
import androidx.media3.datasource.cache.ContentMetadataMutations
import androidx.media3.datasource.cache.NoOpCacheEvictor
import androidx.media3.datasource.cache.SimpleCache
import androidx.media3.datasource.okhttp.OkHttpDataSource
import dalvik.annotation.optimization.CriticalNative
import dev.nori.music.downloads.DownloadsJni
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.net.StreamQuality
import dev.nori.music.net.Http
import dev.nori.music.settings.Settings
import dev.nori.music.ffi.settings.SavedQuality
import java.io.File
import java.io.IOException

/** The stream cache's keys and their order of use, kept in the core (crates/transfers/src/stream_cache.rs). */
internal object StreamCacheJni {
    init { System.loadLibrary("norimusic") }
    /** A new order, for one cache's evictor to hold for the cache's life. */
    @JvmStatic external fun create(): Long
    @JvmStatic @FastNative external fun touch(h: Long, key: String)
    /** What the cache held when this process first looked; told once. */
    @JvmStatic external fun seed(h: Long, keys: Array<String>)
    /** The next key to drop, forgotten by the core as it is handed out; null when there is none. */
    @JvmStatic @FastNative external fun next(h: Long): String?
    /** A song's streamed copies, forgotten by the core as they are handed out. */
    @JvmStatic external fun copies(h: Long, id: String): Array<String>
    @JvmStatic @CriticalNative external fun clear(h: Long)
}

/**
 * Eviction with a limit that follows the setting. media3's own evictor takes its maximum once, in the
 * constructor, so changing "Space for streamed music" would otherwise wait for a restart to mean
 * anything. What goes first is the core's (never used by this run, then least recently used); this
 * reports each use and, only when the cache is over its limit, drops whole resources in the order the
 * core names them. The core knows the keys, so they are handed over once rather than on every trim.
 */
class ResizableEvictor(
    @Volatile var maxBytes: Long,
    /** A piece of a song was written: [MediaSources] looks whether the song is whole now. */
    private val added: (Cache, String) -> Unit = { _, _ -> },
) : CacheEvictor {
    /** The core's order of the cache's keys. */
    private val order = StreamCacheJni.create()

    override fun onCacheInitialized() {}
    override fun onStartFile(cache: Cache, key: String, position: Long, length: Long) = touch(cache, key)
    override fun onSpanAdded(cache: Cache, span: CacheSpan) {
        touch(cache, span.key!!)
        added(cache, span.key!!)
    }
    override fun onSpanRemoved(cache: Cache, span: CacheSpan) {}
    override fun onSpanTouched(cache: Cache, oldSpan: CacheSpan, newSpan: CacheSpan) = touch(cache, newSpan.key!!)
    override fun requiresCacheSpanTouches() = true

    private fun touch(cache: Cache, key: String) = synchronized(this) {
        StreamCacheJni.touch(order, key)
        trimLocked(cache)
    }

    /** Throws out whole resources until the cache fits. Runs wherever the caller is. */
    fun trim(cache: Cache) = synchronized(this) { trimLocked(cache) }

    private fun trimLocked(cache: Cache) {
        if (cache.cacheSpace <= maxBytes) return
        seed(cache)
        while (cache.cacheSpace > maxBytes) {
            val key = StreamCacheJni.next(order) ?: return
            runCatching { cache.removeResource(key) }
        }
    }

    @Volatile private var seeded = false

    /**
     * Tells the core, once, what an earlier run left in the cache; from then on it hears of every key
     * through the callbacks above. Only asked when something is to be dropped, so a run that never
     * trims never lists the cache. Told twice in a race it is the same: the core only adds keys it
     * does not know.
     */
    fun seed(cache: Cache) {
        if (seeded) return
        seeded = true
        StreamCacheJni.seed(order, cache.keys.toTypedArray())
    }

    /** A song's streamed copies, forgotten as they are handed out. */
    fun copies(id: String): Array<String> = StreamCacheJni.copies(order, id)

    /** The core forgets every key: the cache was emptied, and whatever it still holds is told again. */
    fun forget() {
        StreamCacheJni.clear(order)
        seeded = false
    }
}

/**
 * Where audio bytes come from, in order: a finished download, the rolling
 * stream cache, the network. Both caches are keyed by song id and quality, never
 * by URL, so a replayed track costs no radio time at all.
 */
@UnstableApi
class MediaSources(context: Context, private val clientOf: () -> Client, private val http: Http, private val settings: Settings) {
    private val client get() = clientOf()
    val database = StandaloneDatabaseProvider(context)
    val streamEvictor = ResizableEvictor(settings.value.cacheMb * 1024L * 1024L, ::added)
    val streamCache = SimpleCache(File(context.cacheDir, "stream"), streamEvictor, database)
    val downloadCache = SimpleCache(File(context.getExternalFilesDir(null) ?: context.filesDir, "downloads"), Arrivals(::added), database)

    private val motionDir = File(context.cacheDir, "motion")

    /**
     * Moving covers (MotionPlayer): each loop is played from here after its first pass, so the video is
     * fetched once rather than once a loop. Built the first time one plays - never while they are
     * switched off - and least recently played first out past 64 MB, a dozen or so albums.
     */
    val motionCache: Cache by lazy { SimpleCache(motionDir, androidx.media3.datasource.cache.LeastRecentlyUsedCacheEvictor(64L * 1024 * 1024), database) }

    /**
     * Told when a song has become whole in either cache, on the thread that wrote its last piece (AutoMix's
     * measuring ahead, which only measures a song once it is all on the device). Null while nobody asks.
     */
    @Volatile var onWhole: (() -> Unit)? = null

    private fun added(cache: Cache, key: String) {
        val tell = onWhole ?: return
        if (isWhole(cache, key)) tell()
    }

    /** A download cache keeps everything, and says when a piece of a song was written. */
    private class Arrivals(private val added: (Cache, String) -> Unit) : CacheEvictor by NoOpCacheEvictor() {
        override fun onSpanAdded(cache: Cache, span: CacheSpan) = added(cache, span.key!!)
    }

    /** Whether [id]'s download is complete, from the core's memory of its downloads table. */
    fun isDownloaded(id: String): Boolean = DownloadsJni.held(DownloadsJni.h, id) == DownloadsJni.DONE

    /**
     * The stream cache's copy of [id] (a whole one first) by its key, and whether it is whole; none. For the
     * perf build's timeline, which says where a song played from; asked once a song.
     */
    fun streamCopy(id: String): Pair<String, Boolean>? {
        val keys = streamCache.keys.filter { it.startsWith("$id:") }
        return keys.firstOrNull { isWhole(streamCache, it) }?.let { it to true } ?: keys.firstOrNull()?.let { it to false }
    }

    val network: DataSource.Factory = OkHttpDataSource.Factory(http.streamFactory)

    /** The rolling cache over the network. */
    val streamCached: CacheDataSource.Factory = CacheDataSource.Factory().setCache(streamCache).setUpstreamDataSourceFactory(network)
        .setFlags(CacheDataSource.FLAG_IGNORE_CACHE_ON_ERROR)

    private val cached: DataSource.Factory = CacheDataSource.Factory()
        .setCache(downloadCache)
        .setCacheWriteDataSinkFactory(null)
        .setUpstreamDataSourceFactory(streamCached)

    /**
     * Whether someone holds the first stretch of [key] the stream cache is missing: the player, loading it.
     * Asked once per song fetched ahead, without waiting: the fetching ahead leaves such a song to the player,
     * whose reads would otherwise go past a locked cache to the network a second time.
     */
    fun beingWritten(key: String): Boolean = runCatching {
        val length = ContentMetadata.getContentLength(streamCache.getContentMetadata(key))
        val from = if (length > 0) streamCache.getCachedLength(key, 0, length).coerceAtLeast(0) else 0L
        if (length > 0 && from >= length) return@runCatching false
        val span = streamCache.startReadWriteNonBlocking(key, from, if (length > 0) length - from else androidx.media3.common.C.LENGTH_UNSET.toLong()) ?: return@runCatching true
        if (!span.isCached) streamCache.releaseHoleSpan(span)
        false
    }.getOrDefault(false)

    /**
     * What the stream cache keeps of [key], in words, for the log and the perf build's report: whether it has
     * an entry, the length its metadata gives, the bytes cached and in which spans, whether that counts as
     * whole and whether someone writes it now.
     */
    fun cacheWords(key: String): String = runCatching {
        val length = ContentMetadata.getContentLength(streamCache.getContentMetadata(key))
        val spans = streamCache.getCachedSpans(key).toList()
        if (spans.isEmpty() && length == C.LENGTH_UNSET.toLong()) return@runCatching "$key: no entry"
        val shown = spans.take(8).joinToString(", ") { "${it.position}+${it.length}" } + if (spans.size > 8) ", and ${spans.size - 8} more" else ""
        val said = if (length == C.LENGTH_UNSET.toLong()) "unset" else length.toString()
        "$key: metadata length $said, ${spans.sumOf { it.length }} bytes cached in ${spans.size} spans ($shown), " +
            (if (isWhole(streamCache, key)) "whole" else "not whole") + ", " +
            (if (beingWritten(key)) "being written" else "nobody writing it")
    }.getOrElse { "$key: could not be read: $it" }

    /**
     * [key]'s stream cache entry goes (the Rust player makes a song that played nothing again from scratch,
     * and what the cache kept of it may be why): what it kept is answered first, in words. A download stays.
     */
    fun forgetStream(key: String): String {
        val kept = cacheWords(key)
        runCatching { streamCache.removeResource(key) }.onFailure { return "$kept; it would not go: $it" }
        return kept
    }

    /**
     * The same sources as [cached], over a network whose every OkHttp call [ticket] can cancel: made per
     * request the Rust player may call off, a few builders and nothing else.
     */
    private fun cancellable(ticket: Ticket): DataSource.Factory {
        val net = OkHttpDataSource.Factory(okhttp3.Call.Factory { http.stream.newCall(it).also(ticket::track) })
        val stream = CacheDataSource.Factory().setCache(streamCache).setUpstreamDataSourceFactory(net)
            .setFlags(CacheDataSource.FLAG_IGNORE_CACHE_ON_ERROR)
        return CacheDataSource.Factory().setCache(downloadCache).setCacheWriteDataSinkFactory(null).setUpstreamDataSourceFactory(stream)
    }

    /**
     * A song's bytes from [from] on, at [url] under the cache key [key] as the core resolved them (the Rust
     * player opens its songs so): a download, then the stream cache, then the network. The source and how
     * many bytes are left (C.LENGTH_UNSET unknown). [ticket] is the request's number when the Rust side may
     * call it off ([Tickets]); it is let go here when the open fails, and by the body's close otherwise.
     */
    fun openResolved(url: String, key: String, from: Long, ticket: Long = 0): Pair<DataSource, Long> {
        applyStreamLimit()
        val t = Tickets.start(ticket)
        val source = (if (t == null) cached else cancellable(t)).createDataSource()
        try {
            t?.opening()
            if (t?.cancelled == true) throw java.io.InterruptedIOException("called off")
            return source.open(DataSpec.Builder().setUri(Uri.parse(url)).setKey(key).setPosition(from).build()).let { source to it }
        } catch (e: IOException) {
            // Let go at once: the stream cache's lock on the song goes with it.
            runCatching { source.close() }
            Tickets.end(ticket)
            if (t?.cancelled == true) throw e
            val said = (if (from > 0) pastEnd(e) else null) ?: throw e
            // A 416 that does not say the length (a proxy drops Content-Range): the first byte asked for
            // alone, whose ranged answer does. Past the end, the server holds the finished transcode and
            // serves ranges.
            val whole = if (said >= 0) said else wholeLength(url).takeIf { it in 1..from } ?: -1
            forgetEstimate(key, from, whole)
            throw PastEnd(whole)
        } finally {
            t?.opened()
        }
    }

    /** [url]'s whole length as the server says it in a ranged answer for its first byte; -1 when it does not. */
    private fun wholeLength(url: String): Long = runCatching {
        val source = network.createDataSource()
        try {
            source.open(DataSpec.Builder().setUri(Uri.parse(url)).setPosition(0).setLength(1).build())
            val range = source.responseHeaders.entries.firstOrNull { it.key.equals("Content-Range", ignoreCase = true) }?.value?.firstOrNull()
            range?.substringAfterLast('/')?.trim()?.toLongOrNull() ?: -1L
        } finally {
            source.close()
        }
    }.getOrDefault(-1L)

    /**
     * The stream cache's length for [key] made the real one, [whole] (-1 not known), once a range from
     * [from] on turned out to start past the end. A transcode's first answer promises an estimated
     * length, the cache keeps it, and a song kept with it never counts as whole and sends readers past
     * its real end again; an unknown real length is dropped rather than kept wrong.
     */
    private fun forgetEstimate(key: String, from: Long, whole: Long) = runCatching {
        val had = ContentMetadata.getContentLength(streamCache.getContentMetadata(key))
        if (had == C.LENGTH_UNSET.toLong() || had == whole) return@runCatching
        val change = ContentMetadataMutations()
        when {
            whole >= 0 -> ContentMetadataMutations.setContentLength(change, whole)
            had > from -> change.remove(ContentMetadata.KEY_CONTENT_LENGTH)
            else -> return@runCatching
        }
        streamCache.applyContentMetadataMutations(key, change)
        dev.nori.music.NoriLog.i("stream cache: $key ends at ${if (whole >= 0) whole else "an unknown byte"}, not the $had first promised")
    }

    /**
     * A radio station's stream at [url], straight from the network (a live stream is never cached), with
     * the station's announcements asked for: the source, and the bytes of music between two announcements
     * as the station answers (`icy-metaint`; 0 when it sends none).
     */
    fun openLive(url: String): Pair<DataSource, Int> {
        val source = network.createDataSource()
        source.open(DataSpec.Builder().setUri(Uri.parse(url)).setHttpRequestHeaders(mapOf("Icy-MetaData" to "1")).build())
        val every = source.responseHeaders.entries.firstOrNull { it.key.equals("icy-metaint", ignoreCase = true) }?.value?.firstOrNull()?.trim()?.toIntOrNull()
        return source to (every ?: 0)
    }

    /** The key [resolve] would give a song that is not downloaded, without building its URL. */
    fun streamKey(id: String): String = settings.value.let { client.streamKey(id, http.metered, it.wifi.ffi(), it.mobile.ffi()) }

    private fun SavedQuality.ffi() = StreamQuality(bitRate.coerceAtLeast(0).toUInt(), format)

    fun downloadKey(id: String) = dev.nori.music.ffi.net.downloadKey(id)

    fun downloadUrl(id: String): String = client.downloadTarget(id, settings.value.download.ffi()).url

    /**
     * The limit follows the setting without a restart; checked whenever a track is opened, so the
     * player picks a change up at the next song at the latest. A settings change applies it at once
     * through [setStreamLimitMb].
     */
    private fun applyStreamLimit() {
        val want = settings.value.cacheMb * 1024L * 1024L
        if (streamEvictor.maxBytes != want) setStreamLimitMb(settings.value.cacheMb)
    }

    /** Sets the streamed-music limit now; call off the main thread, it touches the disk. */
    fun setStreamLimitMb(mb: Int) {
        streamEvictor.maxBytes = mb * 1024L * 1024L
        streamEvictor.trim(streamCache)
    }

    fun streamBytes(): Long = runCatching { streamCache.cacheSpace }.getOrDefault(0L)
    fun downloadBytes(): Long = runCatching { downloadCache.cacheSpace }.getOrDefault(0L)

    /**
     * Forgets every streamed copy of [id], whatever quality it was fetched at. A finished download is
     * the permanent copy; the streamed one is the same bytes twice. Call off the main thread.
     */
    fun dropStreamCopies(id: String) {
        runCatching { streamEvictor.seed(streamCache) }
        // The key grammar is the core's, and so is the list of keys: it names this song's copies.
        for (key in streamEvictor.copies(id)) runCatching { streamCache.removeResource(key) }
    }

    /** Empties the streamed-music cache; downloads, covers and the index stay. Call off the main thread. */
    fun clearStream() {
        for (key in runCatching { streamCache.keys }.getOrDefault(emptySet())) runCatching { streamCache.removeResource(key) }
        streamEvictor.forget()
    }

    /**
     * A song asked for from past its end: the server's first answer promised an estimated length (a
     * transcode) beyond the real one. [whole] is the real length when the server said it, -1 when not.
     */
    class PastEnd(val whole: Long) : IOException("past the end of the song")

    companion object {
        /**
         * Whether [e] says a range started past the resource's end: the whole length the server gave
         * (`Content-Range: bytes * /N`), -1 when it gave none; null when [e] is some other failure (a
         * network that dropped is asked again, never taken for an end).
         */
        fun pastEnd(e: Throwable): Long? {
            var c: Throwable? = e
            while (c != null) {
                if (c is HttpDataSource.InvalidResponseCodeException) {
                    if (c.responseCode != 416) return null
                    val range = c.headerFields.entries.firstOrNull { it.key.equals("Content-Range", ignoreCase = true) }?.value?.firstOrNull()
                    return range?.let(::unsatisfiedRange) ?: -1L
                }
                if (c is DataSourceException && c.reason == PlaybackException.ERROR_CODE_IO_READ_POSITION_OUT_OF_RANGE) return -1L
                c = c.cause
            }
            return null
        }

        /** The HTTP status the server answered [e] with, 0 when it did not answer (the network, a timeout). */
        fun httpStatus(e: Throwable): Int =
            generateSequence(e) { it.cause }.filterIsInstance<HttpDataSource.InvalidResponseCodeException>().firstOrNull()?.responseCode ?: 0

        /** The whole length in an unsatisfiable range's `Content-Range` (`bytes * /1000`); null when it names none. */
        fun unsatisfiedRange(v: String): Long? =
            v.trim().removePrefix("bytes").trim().takeIf { it.startsWith("*/") }?.substring(2)?.trim()?.toLongOrNull()

        /**
         * How much may be missing at the end of a song for it to count as whole: tags after the audio. The
         * player stops reading an MP3 where its frames end, so the ID3v1 tag after them (128 bytes) is never
         * fetched, and a song streamed through the player was never whole in the cache.
         */
        const val TAIL = 16 * 1024L

        /** Whether [cache] holds [key] from its first byte to its end, but perhaps a [TAIL]. */
        fun isWhole(cache: Cache, key: String): Boolean {
            val length = ContentMetadata.getContentLength(cache.getContentMetadata(key))
            return length > 0 && cache.getCachedLength(key, 0, length) >= length - TAIL
        }
    }
}
