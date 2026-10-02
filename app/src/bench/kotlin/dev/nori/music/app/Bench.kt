package dev.nori.music.app

import android.content.Context
import android.graphics.Bitmap
import android.os.Debug
import dev.nori.music.data.CoverLoader
import dev.nori.music.data.Covers
import dev.nori.music.look.CoverPixels
import java.io.File
import java.util.Locale

/**
 * The benchmarks the debug build's TestBridge runs over adb and the perf build's Performance page runs
 * from a button (src/bench is a source set of both builds, and of neither release). Each answers with
 * one line of text.
 */
object Bench {
    /** What a crossing into the core costs, by kind, against the same work done in Kotlin: one line. */
    fun calls(): String {
        val out = StringBuilder()
        var sink = 0L
        run(out, "empty loop", 200_000) { i -> sink += i }
        run(out, "kotlin seekStep", 200_000) { i -> sink += kotlinSeekStep(0.3f + i * 1e-7f, 0.31f, 0.016f, 900f, 0.003f) }
        run(out, "jni seekStep", 200_000) { i -> sink += dev.nori.music.look.CoverLook.seekStep(0.3f + i * 1e-7f, 0.31f, 0.016f, 900f, 0.003f) }
        run(out, "uniffi motionReduced", 20_000) { i -> if (dev.nori.music.ffi.motionReduced(false, i % 3 == 0, i % 2 == 0)) sink++ }
        val a = IntArray(dev.nori.music.look.CoverLook.LEN) { 0xFF102030.toInt() + it * 997 }
        val b = IntArray(dev.nori.music.look.CoverLook.LEN) { 0xFFF0E0D0.toInt() - it * 991 }
        val o = IntArray(a.size)
        run(out, "jni mix (all colours)", 50_000) { i -> dev.nori.music.look.CoverLook.mix(a, b, (i % 100) / 100f, o); sink += o[3] }
        run(out, "kotlin compose lerp (all colours)", 50_000) { i ->
            val t = (i % 100) / 100f
            for (k in a.indices) o[k] = androidx.compose.ui.graphics.lerp(androidx.compose.ui.graphics.Color(a[k]), androidx.compose.ui.graphics.Color(b[k]), t).value.toInt()
            sink += o[3]
        }
        // The seek bar's time label, formatted in Kotlin (it crossed over JNI, and over uniffi before that):
        // a new string each time, and the per-second cache the seek bar reads.
        run(out, "kotlin duration string", 50_000) { i -> sink += dev.nori.music.text.Fmt.clock((i % 7000).toLong(), false).length }
        run(out, "kotlin duration cached", 50_000) { i -> sink += dev.nori.music.text.Fmt.duration((i % 7000).toLong()).length }
        // The words a list row or a shelf asks for, by the core's kind (Say): read once per locale, so asking
        // allocates nothing.
        val say = dev.nori.music.app.ui.Say.current
        val swipes = arrayOf(dev.nori.music.ffi.library.RowSwipeAct.Queue, dev.nori.music.ffi.library.RowSwipeAct.PlayNext, dev.nori.music.ffi.library.RowSwipeAct.Download)
        val rows = dev.nori.music.ffi.settings.HomeRow.entries.toTypedArray()
        val kinds = dev.nori.music.ffi.model.EqKind.entries.toTypedArray()
        run(out, "say rowSwipe", 200_000) { i -> sink += say.rowSwipe(swipes[i % 3]).length }
        run(out, "say homeRow", 200_000) { i -> sink += say.homeRow(rows[i % rows.size]).length }
        run(out, "say bandKind", 200_000) { i -> sink += say.bandKind(kinds[i % kinds.size]).length }
        return out.append("sink $sink").toString()
    }

    private fun allocated() = android.os.Debug.getRuntimeStat("art.gc.bytes-allocated")?.toLongOrNull() ?: 0L

    /** Inline, so the loop itself boxes nothing and costs next to nothing: what is measured is the body. */
    private inline fun run(out: StringBuilder, name: String, n: Int, body: (Int) -> Unit) {
        repeat(3) { for (i in 0 until n / 10) body(i) } // warm up the JIT
        val a0 = allocated(); val t0 = System.nanoTime()
        for (i in 0 until n) body(i)
        val ns = (System.nanoTime() - t0).toDouble() / n; val bytes = (allocated() - a0).toDouble() / n
        out.append(String.format(java.util.Locale.ROOT, "%s: %.0f ns, %.0f B per call; ", name, ns, bytes))
    }

    /**
     * What the core's covers cost on this phone. First the decode alone: the covers kept on disk (at most
     * [limit], the same ones each time) decoded to 300 and 1080 px into one reused Bitmap, ARGB_8888 with
     * the IDCT shrinking big JPEGs and without, and RGB_565 (what packing adds). Then the whole way a
     * screen gets them: covers the app has shown (the addresses kept in memory) loaded from the disk
     * through a loader of the bench's own at 300 px, into software Bitmaps and into hardware ones (decoded
     * in software and copied to the GPU), each call back counted. For each: ms per cover, Java heap bytes
     * allocated per cover, GCs, and the Java and native heaps before and after. Takes seconds: call it off
     * the main thread.
     */
    fun covers(context: Context, limit: Int = 40): String {
        val files = coverFiles(context, limit)
        if (files.isEmpty()) return "coverbench: no covers in ${context.cacheDir}/${CoverLoader.DIR}"
        val out = StringBuilder("coverbench ${files.size} covers")
        for (side in intArrayOf(300, 1080)) {
            val reused = Bitmap.createBitmap(side, side, Bitmap.Config.ARGB_8888)
            val reused565 = Bitmap.createBitmap(side, side, Bitmap.Config.RGB_565)
            val paths = listOf<Pair<String, (File) -> Boolean>>(
                "decode-idct" to { f -> CoverPixels.decodeFile(f.path, reused, true) == CoverPixels.OK },
                "decode-whole" to { f -> CoverPixels.decodeFile(f.path, reused, false) == CoverPixels.OK },
                "decode-565" to { f -> CoverPixels.decodeFile(f.path, reused565, false) == CoverPixels.OK },
            )
            for ((name, decode) in paths) out.append(" | ").append(side).append(' ').append(measure(name, files.size) { files.count { !decode(it) } })
            reused.recycle()
            reused565.recycle()
        }
        val urls = CoverLoader.get(context).keptAddresses().take(limit)
        if (urls.isEmpty()) return out.append(" | loader: no covers shown yet").toString()
        for (hardware in listOf(false, true)) {
            val loader = CoverPixels.open(dev.nori.music.Nori.get(context).coverNet.uniffiCloneHandle(), File(context.cacheDir, CoverLoader.DIR).path, Covers.rules.diskBytes.toLong(), hardware, true)
            try {
                // Once through first, to warm the JIT, the page cache and the loader's threads.
                load(loader, urls.take(5))
                val name = "loader-${if (hardware) "hardware" else "software"}"
                out.append(" | 300 ").append(measure(name, urls.size) { load(loader, urls) })
            } finally {
                CoverPixels.close(loader)
            }
        }
        return out.toString()
    }

    /** Every cover at [urls] through [loader] at 300 px, all asked for at once, as a screenful is; how many failed. */
    private fun load(loader: Long, urls: List<String>): Int {
        val left = java.util.concurrent.CountDownLatch(urls.size)
        val failed = java.util.concurrent.atomic.AtomicInteger()
        val waiter = object : CoverPixels.Waiter {
            override fun done(bitmap: Bitmap?, status: Int) {
                if (bitmap == null) failed.incrementAndGet() else bitmap.recycle()
                left.countDown()
            }
        }
        val tickets = urls.map { CoverPixels.request(loader, it, 300, 300, waiter) }
        left.await(60, java.util.concurrent.TimeUnit.SECONDS)
        tickets.forEach(CoverPixels::cancel)
        return failed.get() + left.count.toInt()
    }

    /** At most [limit] covers from the disk cache, the same ones each time. */
    private fun coverFiles(context: Context, limit: Int): List<File> =
        File(context.cacheDir, CoverLoader.DIR).walkTopDown().filter { it.isFile && isPicture(it) }.sortedBy { it.name }.take(limit).toList()

    /** Whether the file starts as a JPEG, PNG, WebP or GIF does. */
    private fun isPicture(f: File): Boolean {
        val head = ByteArray(12)
        val n = runCatching { f.inputStream().use { it.read(head) } }.getOrDefault(0)
        if (n < 12) return false
        fun at(i: Int) = head[i].toInt() and 0xFF
        return (at(0) == 0xFF && at(1) == 0xD8 && at(2) == 0xFF) ||
            (at(0) == 0x89 && at(1) == 'P'.code && at(2) == 'N'.code && at(3) == 'G'.code) ||
            (String(head, 0, 4, Charsets.US_ASCII) == "RIFF" && String(head, 8, 4, Charsets.US_ASCII) == "WEBP") ||
            String(head, 0, 3, Charsets.US_ASCII) == "GIF"
    }

    private fun gcCount() = Debug.getRuntimeStat("art.gc.gc-count")?.toLongOrNull() ?: 0L

    private fun javaHeap() = Runtime.getRuntime().let { it.totalMemory() - it.freeMemory() }

    /** [n] covers through [run], which answers how many failed, with what they cost. */
    private fun measure(name: String, n: Int, run: () -> Int): String {
        System.gc(); System.runFinalization(); System.gc()
        val java0 = javaHeap(); val native0 = Debug.getNativeHeapAllocatedSize()
        val a0 = allocated(); val gc0 = gcCount()
        val t0 = System.nanoTime()
        val failed = run()
        val ms = (System.nanoTime() - t0) / 1e6
        val bytes = (allocated() - a0).toDouble() / n; val gcs = gcCount() - gc0
        val java1 = javaHeap(); val native1 = Debug.getNativeHeapAllocatedSize()
        val mb = 1024.0 * 1024.0
        return String.format(Locale.ROOT, "%s: %.0f ms, %.2f ms/cover, %.0f B/cover allocated, %d GCs, java %.1f->%.1f MB, native %.1f->%.1f MB%s",
            name, ms, ms / n, bytes, gcs, java0 / mb, java1 / mb, native0 / mb, native1 / mb, if (failed > 0) ", $failed failed" else "")
    }

    /** nori_look::motion::seek_step, written out in Kotlin for the comparison. */
    private fun kotlinSeekStep(bar: Float, target: Float, dt: Float, widthPx: Float, speed: Float): Long {
        val width = widthPx.coerceAtLeast(1f)
        val gap = (target - bar) * width
        val next = if (kotlin.math.abs(gap) < 0.5f) bar else if (kotlin.math.abs(gap) < 2f) target else {
            val eased = bar + (target - bar) * (1f - kotlin.math.exp(-dt / 0.14f))
            if (kotlin.math.abs((target - eased) * width) < 2f) target else return (eased.toRawBits().toLong() shl 32)
        }
        if (speed <= 0f) return (next.toRawBits().toLong() shl 32) or 0xFFFF_FFFFL
        val ahead = ((target - next) * width).coerceIn(0f, 1f)
        val wait = ((1f - ahead) / (width * speed) * 1000f).toInt().coerceIn(16, 1000)
        return (next.toRawBits().toLong() shl 32) or wait.toLong()
    }
}
