package dev.nori.music.app.car

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.graphics.Bitmap
import android.graphics.Canvas
import android.net.Uri
import android.os.ParcelFileDescriptor
import androidx.core.content.ContextCompat
import dev.nori.music.Nori
import dev.nori.music.app.R
import dev.nori.music.app.widget.Painter
import dev.nori.music.app.widget.Widgets
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import java.util.concurrent.Executors

/**
 * The car's pictures (dev.nori.music.playback.CarArt makes their addresses). Android Auto draws a browse
 * item's picture only from a `content://` address it opens itself, never from the server's signed one, so
 * this draws them, from the covers the app keeps: `c/<cover id>[?s=<side>]`, a cover (the queue's songs
 * carry these too, for the car's now playing and the lock screen); `m/<mix id>?c=..` a mix's tile
 * as Home draws it (the widgets' [Painter]); `g/<name>`, the glyphs of the Play and Shuffle rows and of the
 * long-press actions.
 * Only ever read, and only what the car asks for as it lists a folder.
 */
class CarArtProvider : ContentProvider() {
    override fun onCreate() = true

    override fun getType(uri: Uri): String = if (uri.pathSegments.firstOrNull() == "g") "image/png" else "image/jpeg"

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        require(mode == "r") { "the car's pictures are only read" }
        val app = checkNotNull(context).applicationContext
        val (read, write) = ParcelFileDescriptor.createReliablePipe()
        // Drawn and written on a thread of its own: the car reads the pipe as it fills.
        drawing.execute {
            runCatching {
                ParcelFileDescriptor.AutoCloseOutputStream(write).use { out ->
                    val glyph = uri.pathSegments.firstOrNull() == "g"
                    draw(app, uri)?.compress(if (glyph) Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG, 90, out)
                }
            }
        }
        return read
    }

    private fun draw(context: Context, uri: Uri): Bitmap? = runBlocking {
        withTimeoutOrNull(10_000) {
            val parts = uri.pathSegments
            when (parts.firstOrNull()) {
                "c" -> {
                    val id = parts.getOrNull(1) ?: return@withTimeoutOrNull null
                    val look = Widgets.theme(context).look
                    // The size asked for (the queue's covers are the lock screen's 800), else a car row's.
                    val side = uri.getQueryParameter("s")?.toIntOrNull()?.coerceIn(64, 1024) ?: SIDE
                    Painter.cover(context, Widgets.picture(context, Nori.get(context).library.coverUrl(id, side), side), side, side, 0f, look)
                }
                "m" -> {
                    val mix = parts.getOrNull(1) ?: return@withTimeoutOrNull null
                    val library = Nori.get(context).library
                    val covers = coroutineScope { uri.getQueryParameters("c").map { id -> async { Widgets.picture(context, library.coverUrl(id, SIDE / 2), SIDE / 2) } }.awaitAll() }
                    Painter.mix(covers.filterNotNull(), dev.nori.music.ffi.library.mixTileColours(mix).map { it.toInt() }, SIDE, SIDE, 0f)
                }
                "g" -> glyph(
                    context,
                    when (parts.getOrNull(1)) {
                        "shuffle" -> R.drawable.widget_shuffle
                        "next" -> dev.nori.music.core.R.drawable.car_play_next
                        "queue" -> dev.nori.music.core.R.drawable.car_add_to_queue
                        "heart" -> dev.nori.music.core.R.drawable.car_favourite
                        "download" -> dev.nori.music.core.R.drawable.car_download
                        "home" -> dev.nori.music.core.R.drawable.car_home
                        "library" -> dev.nori.music.core.R.drawable.car_library
                        "recent" -> dev.nori.music.core.R.drawable.car_recent
                        "new" -> dev.nori.music.core.R.drawable.car_new
                        "most" -> dev.nori.music.core.R.drawable.car_most
                        "albums" -> dev.nori.music.core.R.drawable.car_albums
                        "artists" -> dev.nori.music.core.R.drawable.car_artists
                        "playlists" -> dev.nori.music.core.R.drawable.car_playlists
                        "genres" -> dev.nori.music.core.R.drawable.car_genres
                        "random" -> dev.nori.music.core.R.drawable.car_random
                        else -> R.drawable.widget_play
                    },
                )
                else -> null
            }
        }
    }

    /** A white glyph on nothing, a quarter of its side clear around it; the car tints it. */
    private fun glyph(context: Context, res: Int): Bitmap {
        val side = 96
        val b = Bitmap.createBitmap(side, side, Bitmap.Config.ARGB_8888)
        ContextCompat.getDrawable(context, res)!!.mutate().apply { setBounds(side / 4, side / 4, side * 3 / 4, side * 3 / 4) }.draw(Canvas(b))
        return b
    }

    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? = null
    override fun insert(uri: Uri, values: ContentValues?): Uri? = null
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?) = 0
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?) = 0

    private companion object {
        /** A car's cover, pixels a side: the core's `car::ART`, one rendition the server keeps for every car row. */
        const val SIDE = 300

        /** A few at once, as a grid of covers is asked for together. */
        val drawing = Executors.newFixedThreadPool(3)
    }
}
