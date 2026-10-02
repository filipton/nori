package dev.nori.music.data

import android.app.Activity
import android.app.ActivityManager
import android.app.ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND
import android.app.ActivityManager.RunningAppProcessInfo.IMPORTANCE_VISIBLE
import android.app.Application
import android.content.ComponentCallbacks2
import android.content.Context
import android.content.res.Configuration
import android.graphics.Bitmap
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.LruCache
import dev.nori.music.look.CoverLook
import dev.nori.music.look.CoverPixels
import java.io.File

/**
 * The app's covers. The core fetches them (through the app's own transport, so they ride the API's
 * connection), keeps the server's files on disk and decodes each straight into a Bitmap at the size its
 * view draws it at (crates/covers, through [CoverPixels]); this only keeps the Bitmaps and hands them to
 * the screens on the main thread.
 *
 * The Bitmaps are kept here, not in the core, because a Bitmap is a Java object: an LRU by bytes, per
 * cover address, sized as the core says (`cover_rules`' share of the app's memory class). Each address
 * keeps its largest picture: a row's thumbnail is drawn from the grid's larger one rather than decoded
 * again, and a view that needs more than is kept is shown what is kept while the larger one comes.
 *
 * Covers are hardware Bitmaps from Android 9 (decoded in software on a loader thread and copied to the
 * GPU there, so no frame pays the upload), and a JPEG is RGB_565: half the memory, twice the covers.
 */
class CoverLoader private constructor(context: Context) {
    /** A picture kept for an address; [whole] when it is the file's own size, so no view can want it bigger. */
    class Kept(val bitmap: Bitmap, val whole: Boolean) {
        /** Whether it is enough for a view [width] x [height] pixels. */
        fun fits(width: Int, height: Int): Boolean = whole || bitmap.width >= width && bitmap.height >= height
    }

    private val app = context.applicationContext
    private val main = Handler(Looper.getMainLooper())

    /** Cheap to open: the core reads its directory on the loader's first thread, not here. */
    private val loader = CoverPixels.open(dev.nori.music.Nori.get(app).coverNet.uniffiCloneHandle(), File(app.cacheDir, DIR).path, Covers.rules.diskBytes.toLong(), Build.VERSION.SDK_INT >= 28, true)

    private val memory = object : LruCache<String, Kept>(memoryBytes(app)) {
        override fun sizeOf(key: String, value: Kept): Int = value.bitmap.allocationByteCount
    }

    init {
        // What Coil did with its memory cache: everything goes when the app is in the background and
        // the system is short, half of it when the app is running low. The loader's threads and their
        // buffers go too: a cover asked for later starts a thread again.
        app.registerComponentCallbacks(object : ComponentCallbacks2 {
            override fun onTrimMemory(level: Int) {
                @Suppress("DEPRECATION")
                if (level >= ComponentCallbacks2.TRIM_MEMORY_BACKGROUND) memory.evictAll()
                else if (level >= ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW) memory.trimToSize(memory.maxSize() / 2)
                @Suppress("DEPRECATION")
                if (level >= ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW) CoverPixels.rest(loader)
            }

            override fun onConfigurationChanged(newConfig: Configuration) {}

            @Deprecated("Deprecated in Java")
            override fun onLowMemory() {
                memory.evictAll()
                CoverPixels.rest(loader)
            }
        })
        // Whether any screen is in sight, for the loader to rest when none is (the screen off, another
        // app). Not from onTrimMemory: while music plays the service keeps the process important, and
        // Android sends no UI_HIDDEN.
        val state = ActivityManager.RunningAppProcessInfo().also { ActivityManager.getMyMemoryState(it) }
        CoverPixels.show(loader, state.importance == IMPORTANCE_FOREGROUND || state.importance == IMPORTANCE_VISIBLE)
        (app as? Application)?.registerActivityLifecycleCallbacks(object : Application.ActivityLifecycleCallbacks {
            private var started = 0

            override fun onActivityStarted(activity: Activity) {
                if (started++ == 0) CoverPixels.show(loader, true)
            }

            override fun onActivityStopped(activity: Activity) {
                // The loader may have been made after an activity started, which is then not counted.
                started = maxOf(0, started - 1)
                if (started == 0) {
                    CoverPixels.show(loader, false)
                    // Only the most recent covers are kept while nothing is drawn (`cover_rules`' hidden share):
                    // those still on the page left are held by their views anyway; the pages scrolled past go.
                    memory.trimToSize((memory.maxSize() * Covers.rules.hiddenShare).toInt())
                }
            }

            override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
            override fun onActivityResumed(activity: Activity) {}
            override fun onActivityPaused(activity: Activity) {}
            override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
            override fun onActivityDestroyed(activity: Activity) {}
        })
    }

    /** What is kept for [url], at whatever size. */
    fun kept(url: String): Kept? = memory.get(url)

    /** Bytes of the Bitmaps kept in memory, and how many there are: for the perf report's memory line. */
    fun keptBytes(): Long = memory.size().toLong()
    fun keptCount(): Int = memory.snapshot().size

    /** The addresses of the covers kept in memory: covers the app has shown, for the benchmark to load again. */
    fun keptAddresses(): List<String> = memory.snapshot().keys.toList()

    /**
     * Asks for the cover at [url] for a view [width] x [height] pixels (0 x 0: at the file's own size, at
     * most 2048 a side).
     * [done] is called on the main thread with the Bitmap, or null when there is none (no such cover, a
     * format the core does not decode, the network), unless the request is cancelled first. Main thread.
     */
    fun load(url: String, width: Int, height: Int, done: (Bitmap?) -> Unit): Request = request(url, width, height) { b, _ -> done(b) }

    /**
     * [load], told why there is no picture: [done] gets the Bitmap and [CoverPixels.OK], or null and what
     * stopped it ([CoverPixels.UNKNOWN], [CoverPixels.CLOSED], [CoverPixels.NETWORK] plus the failure's
     * kind, [CoverPixels.HTTP] plus the status, ...).
     */
    fun request(url: String, width: Int, height: Int, done: (Bitmap?, Int) -> Unit): Request =
        Request(url, width, height, done).also { it.start() }

    /**
     * Warms covers that are about to be drawn: fetched and decoded at their own size, into memory, so they
     * are simply there when their views appear. Main thread.
     */
    fun prefetch(url: String) {
        if (Covers.isProvider(url) || memory.get(url)?.whole == true || !prefetching.add(url)) return
        load(url, 0, 0) { prefetching.remove(url) }
    }

    /**
     * The covers [prefetch] has asked for and not had yet. A grid scrolled a row at a time asks again
     * for the next screenful while it is still decoding, and each repeat would cross into the core and
     * back for a cover already on its way. Main thread only, as is every call back.
     */
    private val prefetching = HashSet<String>()

    /** Fetches the cover at [url] onto the disk, not decoded, behind every cover a view is waiting for. */
    fun warm(url: String) = CoverPixels.warm(loader, url)

    /** A cover's page in the plain theme and on AMOLED black, each null when it was not asked for. */
    class Pages(val plain: CoverLook.Colours?, val black: CoverLook.Colours?)

    /**
     * The pages for the cover at [url], [plain] and on AMOLED [black], worked out by the core from the
     * cover's own pixels, decoded once to fit [side]: no Bitmap in between but the wash it draws into.
     * Null when there is no cover. Reads the disk and may wait for the network: off the main thread.
     */
    fun colours(url: String, side: Int, dark: Boolean, plain: Boolean, black: Boolean): Pages? {
        val out = if (plain) IntArray(CoverLook.LEN) else null
        val wash = if (plain) Bitmap.createBitmap(CoverLook.WASH, CoverLook.WASH, Bitmap.Config.ARGB_8888) else null
        val onBlack = if (black) IntArray(CoverLook.LEN) else null
        val got = CoverPixels.colours(loader, url, side, dark, out, wash, onBlack)
        if (got == 0) return null
        return Pages(
            plain = if (got and CoverPixels.PLAIN != 0 && out != null) CoverLook.Colours(out, if (got and CoverPixels.WASH != 0) wash else null) else null,
            black = if (got and CoverPixels.BLACK != 0 && onBlack != null) CoverLook.Colours(onBlack, null) else null,
        )
    }

    /** Views that gave up on their cover for want of the network, each asked again once it comes back. Main thread. */
    private val backs = LinkedHashSet<Back>()
    private var watching = false

    private class Back(val run: () -> Unit)

    /**
     * Runs [run] on the main thread the next time the phone's network comes up, for a view that settled
     * on its placeholder for want of the network. Returns what takes it back. The network is watched from
     * the first such view on; a callback only, nothing polls. Main thread.
     */
    fun whenBack(run: () -> Unit): () -> Unit {
        val b = Back(run)
        backs += b
        if (!watching) {
            watching = true
            val connectivity = app.getSystemService(android.net.ConnectivityManager::class.java)
            // Registering calls onAvailable at once for a network already up; views that failed with the
            // network up are then asked again once, which a server's hiccup deserves anyway.
            runCatching {
                connectivity?.registerDefaultNetworkCallback(object : android.net.ConnectivityManager.NetworkCallback() {
                    // On the main thread (the handler below).
                    override fun onAvailable(network: android.net.Network) {
                        val due = backs.toList()
                        backs.clear()
                        due.forEach { it.run() }
                    }
                }, main)
            }
        }
        return { backs.remove(b) }
    }

    /** Deletes the covers on the disk; they are fetched again as they are shown. Off the main thread. */
    fun clearDisk() = CoverPixels.clear(loader)

    /** Keeps [bitmap] for [url] unless a larger picture of it is kept already. */
    private fun keep(url: String, bitmap: Bitmap, whole: Boolean) {
        val old = memory.get(url)
        if (old == null || whole && !old.whole || bitmap.width * bitmap.height > old.bitmap.width * old.bitmap.height) memory.put(url, Kept(bitmap, whole))
    }

    /**
     * One view's request. The core calls it back on a loader thread; it posts itself to the main thread,
     * where it keeps the picture and hands it on. [cancel] (main thread) lets the core's side go at once;
     * a call back already posted is then dropped when it arrives.
     */
    inner class Request internal constructor(
        private val url: String,
        private val width: Int,
        private val height: Int,
        private val then: (Bitmap?, Int) -> Unit,
    ) : CoverPixels.Waiter, Runnable {
        private var ticket = 0L
        private var over = false
        @Volatile private var got: Bitmap? = null
        @Volatile private var status = CoverPixels.OK

        internal fun start() {
            ticket = CoverPixels.request(loader, url, width, height, this)
            if (ticket == 0L) { over = true; then(null, CoverPixels.UNREADABLE) }
        }

        /** On a loader thread. */
        override fun done(bitmap: Bitmap?, status: Int) {
            got = bitmap
            this.status = status
            main.post(this)
        }

        /** Back on the main thread with the call back. */
        override fun run() {
            if (over) return
            over = true
            CoverPixels.cancel(ticket)
            ticket = 0L
            val b = got
            got = null
            // Smaller than asked for, or asked for at its own size: that is the whole picture.
            if (b != null && !Covers.isProvider(url)) keep(url, b, width == 0 || b.width < width || b.height < height)
            then(b, status)
        }

        /** The view has gone: its cover is not wanted, and it is not handed on. */
        fun cancel() {
            if (over) return
            over = true
            CoverPixels.cancel(ticket)
            ticket = 0L
        }
    }

    companion object {
        /** The disk cache's directory in the app's cache dir. */
        const val DIR = "art"

        @Volatile private var one: CoverLoader? = null

        fun get(context: Context): CoverLoader = one ?: synchronized(this) { one ?: CoverLoader(context).also { one = it } }

        /** The core's share (`cover_rules`) of the memory class the app runs in, in bytes. */
        private fun memoryBytes(context: Context): Int {
            val am = context.getSystemService(ActivityManager::class.java)
            val large = context.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_LARGE_HEAP != 0
            val mb = if (large) am.largeMemoryClass else am.memoryClass
            return (mb * 1024.0 * 1024.0 * Covers.rules.memoryShare).toInt()
        }
    }
}
