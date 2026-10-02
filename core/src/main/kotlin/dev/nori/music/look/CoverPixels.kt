package dev.nori.music.look

import android.graphics.Bitmap
import dalvik.annotation.optimization.FastNative

/**
 * The core's cover loader (crates/covers, through crates/android covers.rs): covers fetched through the
 * transport the app handed the core, kept on disk, and decoded in Rust straight into a Bitmap at the
 * size its view needs. [dev.nori.music.data.CoverLoader] is the app's one loader over this; nothing else
 * should need to call it.
 *
 * A request's [Waiter] is called back once, on a loader thread, with the Bitmap or with null and [OK]'s
 * opposite; the handle [request] answers is the request's, and [cancel] takes it back, once, whether the
 * cover came or not. After [cancel] the waiter is not called for a cover finished later, but one finished
 * as it was cancelled may still be handed over, once: the caller drops it (the native side does not wait
 * for a call back under way, which would make the `@FastNative` [cancel] wait on Java code).
 */
object CoverPixels {
    init { System.loadLibrary("norimusic") }

    const val OK = 0
    const val BAD_BITMAP = 1
    const val UNREADABLE = 2
    /** Not a JPEG, PNG, WebP or GIF (a HEIF cover, say): the view keeps its placeholder. */
    const val UNKNOWN = 3
    const val BROKEN = 4
    /** Nobody waited for the cover when its file came, or the loader closed: asked again, it comes. */
    const val CLOSED = 5
    /** The request did not come back: this plus the transport's `FailureKind` ordinal. */
    const val NETWORK = 100
    /** The server answered with an error: this plus the HTTP status. */
    const val HTTP = 1000

    /** Who waits for a cover. Called from native code by name: see consumer-rules.pro. */
    interface Waiter {
        /** On a loader thread: [bitmap] with [status] [OK], or null. */
        fun done(bitmap: Bitmap?, status: Int)
    }

    /**
     * A loader fetching through [net] (a `CoverNet`'s `uniffiCloneHandle()`, which the loader takes over),
     * keeping at most [diskBytes] of covers in [dir], each decoded into a [hardware] Bitmap (a software one
     * decoded into and copied to the GPU) or a software one, a JPEG in RGB_565 where [rgb565]. Cheap: the
     * directory is read by the loader's first thread. 0 when it cannot be opened.
     */
    @JvmStatic external fun open(net: Long, dir: String, diskBytes: Long, hardware: Boolean, rgb565: Boolean): Long

    @JvmStatic external fun close(loader: Long)

    /**
     * The cover at [url] to fill [width] x [height] (0 x 0: at its own size, at most 2048 a side), for
     * [waiter]. 0 when there is no loader.
     */
    @JvmStatic external fun request(loader: Long, url: String, width: Int, height: Int, waiter: Waiter): Long

    /** Lets the request [ticket] go. A lock and a free: `@FastNative`. */
    @JvmStatic @FastNative external fun cancel(ticket: Long)

    /** The cover at [url] onto the disk, not decoded, behind every view's request. */
    @JvmStatic external fun warm(loader: Long, url: String)

    /**
     * Whether [url] is an octo-fiesta provider's cover, which is never kept (nori-core's
     * `is_provider_cover`). Asked for every cover a list draws: a look at the string, nothing allocated.
     */
    @JvmStatic @FastNative external fun isProvider(url: String): Boolean

    /**
     * Lets go of what the loader keeps for covers to come: its threads end once they have nothing to do,
     * with their buffers and the software Bitmaps kept for GPU copies. For memory running short; the next
     * cover starts a thread again.
     */
    @JvmStatic external fun rest(loader: Long)

    /**
     * Whether any of the app's screens is in sight. Out of sight the loader rests (as [rest]) and keeps
     * no thread waiting for the next cover; in sight, it rests after 20 s without one.
     */
    @JvmStatic external fun show(loader: Long, shown: Boolean)

    /** Deletes every cover on the disk. Off the main thread. */
    @JvmStatic external fun clear(loader: Long)

    /**
     * The pages for the cover at [url], worked out from one decode of it to fit [side] with no Bitmap in
     * between: in the plain theme ([CoverLook.Colours]'s look into [out], its wash into [wash]) and on
     * AMOLED black (the look into [black]), each where its array is not null. Answers [PLAIN], [WASH]
     * and [BLACK] for what it wrote, 0 for no cover. Reads the disk and may wait for the network: off
     * the main thread.
     */
    @JvmStatic external fun colours(loader: Long, url: String, side: Int, dark: Boolean, out: IntArray?, wash: Bitmap?, black: IntArray?): Int

    const val PLAIN = 1
    const val WASH = 2
    const val BLACK = 4

    /**
     * The picture in the file at [path] decoded into [bitmap] (mutable, software ARGB_8888 or RGB_565)
     * at its size: the decode alone, for the debug build's `coverbench`. [idct]: a JPEG at least twice the
     * size is shrunk by the decoder's IDCT rather than decoded whole and averaged.
     */
    @JvmStatic external fun decodeFile(path: String, bitmap: Bitmap, idct: Boolean): Int
}
