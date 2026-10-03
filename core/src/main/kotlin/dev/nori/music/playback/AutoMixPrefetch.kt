package dev.nori.music.playback

import androidx.media3.common.C
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSink
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.cache.Cache
import androidx.media3.datasource.cache.ContentMetadata
import dalvik.annotation.optimization.CriticalNative

/** The measurer's doors (crates/android/src/measure.rs, over nori-engine's `Measurer`). */
internal object MeasureJni {
    init { System.loadLibrary("norimusic") }
    /**
     * AutoMix's analyses over [current]'s client (a `CurrentClient.uniffiCloneHandle()`, taken over) and
     * the queue session [session] (`Nori.sessionHandle`): a handle for the process's life, which the
     * doors below take.
     */
    @JvmStatic external fun analyses(current: Long, session: Long): Long
    /** Makes the measurer, idle until it is asked, asking [bridge], as a handle [stop] takes back; 0 when it could not. */
    @JvmStatic external fun start(bridge: MeasureBridge, analyses: Long): Long
    /** The songs coming up may have changed: the core names them, and the same songs change nothing. */
    @JvmStatic @CriticalNative external fun update(measurer: Long)
    /** A song has become whole in one of the caches. */
    @JvmStatic @CriticalNative external fun arrived(measurer: Long)
    @JvmStatic @CriticalNative external fun stop(measurer: Long)
    /** A download measured as it comes: a handle, 0 when nothing measures it (measured already, an MP4). */
    @JvmStatic external fun downloadOpen(analyses: Long, key: String): Long
    @JvmStatic external fun downloadTake(h: Long, bytes: ByteArray, len: Int)
    @JvmStatic external fun downloadEnd(h: Long, whole: Boolean)
    /** Downloads can be read back from the disk from now on (nori-engine's `processing`), found through [bridge]. */
    @JvmStatic external fun processStart(bridge: MeasureBridge, analyses: Long)
    /** Downloads just saved and settled: what each needs besides its lyrics is decided, marked and started. Off the main thread. */
    @JvmStatic external fun processSaved(analyses: Long, ids: Array<String>)
    /** Downloads asked for again ("Analyse downloaded songs"): how many are to be read back. Off the main thread. */
    @JvmStatic external fun processAnalyse(analyses: Long, ids: Array<String>): Int
}

/**
 * A download's bytes, as media3 fetches them from the network, handed to the core's measuring
 * (crates/android/src/measure.rs `download_*`, over nori-engine's `measure_download_as_it_comes`): a song
 * downloaded for offline listening is measured as it downloads, whatever AutoMix says, on the same bytes, and
 * neither a later mix nor the lyrics' sync needs a pass of its own. A quarter megabyte crosses at a time. Only
 * a download fetched from its first byte to its known end counts as measured; one taken up half way is
 * measured from the disk once it is saved (the core's `processing`).
 */
@UnstableApi
internal class MeasuringSink(private val analyses: Long) : DataSink {
    private var h = 0L
    private var length = C.LENGTH_UNSET.toLong()
    private var written = 0L
    private var buffer: ByteArray? = null
    private var filled = 0

    override fun open(dataSpec: DataSpec) {
        close()
        length = dataSpec.length
        written = 0
        filled = 0
        h = if (dataSpec.position == 0L) MeasureJni.downloadOpen(analyses, dataSpec.key ?: "") else 0L
        if (h != 0L && buffer == null) buffer = ByteArray(PIECE)
    }

    override fun write(bytes: ByteArray, offset: Int, count: Int) {
        if (h == 0L) return
        val buf = buffer ?: return
        var at = offset
        var left = count
        while (left > 0) {
            val n = minOf(left, buf.size - filled)
            System.arraycopy(bytes, at, buf, filled, n)
            filled += n
            at += n
            left -= n
            if (filled == buf.size) flush()
        }
        written += count
    }

    private fun flush() {
        val buf = buffer ?: return
        if (filled > 0) MeasureJni.downloadTake(h, buf, filled)
        filled = 0
    }

    override fun close() {
        if (h == 0L) return
        flush()
        MeasureJni.downloadEnd(h, length > 0 && written == length)
        h = 0L
    }

    private companion object {
        const val PIECE = 256 * 1024
    }
}

/**
 * What one measuring shelf asks of the platform, from its own thread: where a song's bytes are ([find]), and
 * that one was measured ([told]). Handed to the core with the shelf, so its calls reach their own owner.
 */
internal class MeasureBridge(private val find: (String) -> Array<String>?, private val told: () -> Unit = {}) {
    fun whole(id: String): Array<String>? = find(id)
    fun measured() = told()
}

/**
 * Measures tracks before they are played, so a transition has both halves' tempo, beats and cue points
 * the first time those two songs meet. The player's streaming analysis only finishes a track as it
 * ends, which is one boundary too late: the mix out of a song the phone has never heard had nothing to
 * plan from and fell back to a plain fade.
 *
 * The measuring is the core's: nori-engine's measurer (crates/android/src/measure.rs)
 * decodes each song once, whole, on a thread of the lowest priority, reading its files straight. This
 * only says where a song's bytes are in media3's caches, and when one has become whole: the caches'
 * own callbacks say so as the fetching ahead or the player writes the last of it, so the song after the one
 * playing is measured as soon as it is on the device, and nothing is ever measured while its bytes are
 * still coming. Nothing here touches the network.
 */
@UnstableApi
class AutoMixPrefetch(
    private val sources: MediaSources,
    analyses: Long,
    /** A track has been measured: whatever was planned without it can be planned again. On the measuring thread. */
    onMeasured: () -> Unit = {},
) {
    /** The measurer's handle; 0 once released. */
    @Volatile private var measurer = 0L

    init {
        measurer = MeasureJni.start(MeasureBridge(::whole, onMeasured), analyses)
        sources.onWhole = { MeasureJni.arrived(measurer) }
    }

    /** The queue moved or was edited: the measurer asks the core which songs come up now. */
    fun update() = MeasureJni.update(measurer)

    fun release() {
        val m = measurer
        measurer = 0L
        MeasureJni.stop(m)
        sources.onWhole = null
    }

    /**
     * [id]'s bytes if they are all on the device: its cache key, then the files that hold them in order -
     * a download's, or else a streamed copy's. Null while any of it is missing.
     */
    private fun whole(id: String): Array<String>? =
        files(sources.downloadCache, sources.downloadKey(id)) ?: runCatching { sources.streamKey(id) }.getOrNull()?.let { files(sources.streamCache, it) }

    internal companion object {
        /**
         * The files of [key] in [cache] from its first byte on, when they hold all of it but perhaps a short
         * tail (see [MediaSources.isWhole]); the key first.
         */
        fun files(cache: Cache, key: String): Array<String>? {
            if (!MediaSources.isWhole(cache, key)) return null
            val spans = cache.getCachedSpans(key)
            val out = ArrayList<String>(spans.size + 1)
            out += key
            var at = 0L
            for (span in spans) {
                val file = span.file ?: break
                if (span.position != at) break
                out += file.path
                at += span.length
            }
            return if (at >= ContentMetadata.getContentLength(cache.getContentMetadata(key)) - MediaSources.TAIL) out.toTypedArray() else null
        }
    }
}
