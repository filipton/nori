package dev.nori.music.app.car

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.graphics.Bitmap
import android.net.Uri
import android.os.ParcelFileDescriptor
import dev.nori.music.Nori
import dev.nori.music.app.widget.Painter
import dev.nori.music.app.widget.Widgets
import dev.nori.music.ffi.coverRendition
import dev.nori.music.playback.CarArt
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import java.io.ByteArrayOutputStream
import java.io.File
import java.security.MessageDigest
import java.util.concurrent.Executors

/**
 * The car's pictures (dev.nori.music.playback.CarArt makes their addresses). Android Auto draws a browse
 * item's picture only from a `content://` address it opens itself, never from the server's signed one, so
 * this draws them, from the covers the app keeps: `c/<cover id>[?s=<side>]`, a cover (the queue's songs
 * carry these too, for the car's now playing and the lock screen); `m/<mix id>?c=..` a mix's tile
 * as Home draws it (the widgets' [Painter]).
 * Only ever read, and only what the car asks for as it lists a folder.
 */
class CarArtProvider : ContentProvider() {
    override fun onCreate() = true

    override fun getType(uri: Uri): String = "image/jpeg"

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        require(mode == "r") { "the car's pictures are only read" }
        val app = checkNotNull(context).applicationContext
        // The song's cover is opened by the notification, the lock screen, the always-on display and the
        // headphones, each on its own and again as it is shown: drawn once, then read from the disk, with
        // nothing in the app woken for it.
        val kept = coverAddress(app, uri)?.let { File(File(app.cacheDir, KEPT_DIR), name("$it#${side(uri)}")) }
        if (kept?.isFile == true) {
            // Read now: the last to go.
            kept.setLastModified(System.currentTimeMillis())
            return ParcelFileDescriptor.open(kept, ParcelFileDescriptor.MODE_READ_ONLY)
        }
        val (read, write) = ParcelFileDescriptor.createReliablePipe()
        // Drawn and written on a thread of its own: the car reads the pipe as it fills.
        drawing.execute {
            runCatching {
                val lasting = ParcelFileDescriptor.AutoCloseOutputStream(write).use { out ->
                    val drawn = draw(app, uri) ?: return@use null
                    val bytes = ByteArrayOutputStream().also { drawn.bitmap.compress(Bitmap.CompressFormat.JPEG, 90, it) }.toByteArray()
                    out.write(bytes)
                    bytes.takeIf { drawn.lasting }
                }
                if (kept != null && lasting != null) keep(kept, lasting)
            }
        }
        return read
    }

    /** A picture, and whether it may be kept: a cover the server had, not the plate drawn for one it had not. */
    private class Drawn(val bitmap: Bitmap, val lasting: Boolean)

    private fun draw(context: Context, uri: Uri): Drawn? = runBlocking {
        withTimeoutOrNull(10_000) {
            val parts = uri.pathSegments
            when (parts.firstOrNull()) {
                "c" -> {
                    val address = coverAddress(context, uri) ?: return@withTimeoutOrNull null
                    val side = side(uri)
                    coversDrawn++
                    val picture = Widgets.picture(context, address, side)
                    Drawn(Painter.cover(context, picture, side, side, 0f, Widgets.theme(context).look), picture != null)
                }
                "m" -> {
                    val mix = parts.getOrNull(1) ?: return@withTimeoutOrNull null
                    val library = Nori.get(context).library
                    val tile = CarArt.ART / 2
                    val rendition = coverRendition(tile.toUInt()).toInt()
                    val covers = coroutineScope { uri.getQueryParameters("c").map { id -> async { Widgets.picture(context, library.coverUrl(id, rendition), tile) } }.awaitAll() }
                    Drawn(Painter.mix(covers.filterNotNull(), dev.nori.music.ffi.library.mixTileColours(mix).map { it.toInt() }, CarArt.ART, CarArt.ART, 0f), false)
                }
                else -> null
            }
        }
    }

    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? = null
    override fun insert(uri: Uri, values: ContentValues?): Uri? = null
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?) = 0
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?) = 0

    internal companion object {
        /** The covers drawn, for the test bridge: one asked for again is read from the disk instead. */
        @Volatile var coversDrawn = 0
            private set

        /** A few at once, as a grid of covers is asked for together. */
        private val drawing = Executors.newFixedThreadPool(3)

        /** Where the covers drawn are kept, and how many. */
        private const val KEPT_DIR = "car-art"
        private const val KEPT = 100

        /** The size asked for (the queue's covers are the lock screen's 800), else a car row's. */
        private fun side(uri: Uri) = uri.getQueryParameter("s")?.toIntOrNull()?.coerceIn(64, 1024) ?: CarArt.ART

        /**
         * The server's address of a `c/` picture's cover, at the rendition the phone's screens fetch too
         * (the core's `cover_rendition`): the same picture and cached file, whoever asks.
         */
        private fun coverAddress(context: Context, uri: Uri): String? {
            if (uri.pathSegments.firstOrNull() != "c") return null
            return Nori.get(context).library.coverUrl(uri.pathSegments.getOrNull(1) ?: return null, coverRendition(side(uri).toUInt()).toInt())
        }

        private fun name(address: String) = MessageDigest.getInstance("SHA-1").digest(address.toByteArray()).joinToString("") { "%02x".format(it) } + ".jpg"

        /** Writes [bytes] as [file] whole (another reader never sees half of it), and lets the least recently read go. */
        private fun keep(file: File, bytes: ByteArray) {
            val dir = file.parentFile ?: return
            dir.mkdirs()
            val part = File.createTempFile("cover", null, dir)
            part.writeBytes(bytes)
            if (!part.renameTo(file)) part.delete()
            dir.listFiles()?.sortedByDescending { it.lastModified() }?.drop(KEPT)?.forEach { it.delete() }
        }
    }
}
