package dev.nori.music.app.widget

import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.view.View
import android.widget.RemoteViews
import dev.nori.music.Nori
import dev.nori.music.app.R
import dev.nori.music.app.ui.CoverSize
import dev.nori.music.app.ui.Say
import dev.nori.music.app.vm.MixStore
import dev.nori.music.app.widget.Widgets.tint
import dev.nori.music.ffi.library.AlbumSort
import dev.nori.music.ffi.model.Album
import dev.nori.music.ffi.model.OriginKind
import dev.nori.music.ffi.model.PageOrigin
import dev.nori.music.look.CoverLook
import dev.nori.music.settings.loggedIn
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.math.roundToInt

/** A shelf's measures, in dp: its padding at the sides, between tiles, and the header over them. */
private const val SIDE = 14
private const val GAP = 10
private const val HEAD = 40
/** Below this height the header is left out and the tiles take it all. */
private const val HEADED = 150

/** The ids of a shelf's tiles, by slot. */
private val MIX_SLOTS = intArrayOf(R.id.widget_slot0, R.id.widget_slot1, R.id.widget_slot2, R.id.widget_slot3, R.id.widget_slot4, R.id.widget_slot5)
private val MIX_ART = intArrayOf(R.id.widget_art0, R.id.widget_art1, R.id.widget_art2, R.id.widget_art3, R.id.widget_art4, R.id.widget_art5)
private val MIX_MARK = intArrayOf(R.id.widget_mark0, R.id.widget_mark1, R.id.widget_mark2, R.id.widget_mark3, R.id.widget_mark4, R.id.widget_mark5)
private val MIX_NAME = intArrayOf(R.id.widget_name0, R.id.widget_name1, R.id.widget_name2, R.id.widget_name3, R.id.widget_name4, R.id.widget_name5)
private val ROWS = intArrayOf(R.id.widget_row0, R.id.widget_row1, R.id.widget_row2)
private val ALBUMS = arrayOf(
    intArrayOf(R.id.widget_album00, R.id.widget_album01, R.id.widget_album02, R.id.widget_album03, R.id.widget_album04, R.id.widget_album05),
    intArrayOf(R.id.widget_album10, R.id.widget_album11, R.id.widget_album12, R.id.widget_album13, R.id.widget_album14, R.id.widget_album15),
    intArrayOf(R.id.widget_album20, R.id.widget_album21, R.id.widget_album22, R.id.widget_album23, R.id.widget_album24, R.id.widget_album25),
)

/** A shelf on the page's own colour, its header in the Home page's section title, shown when there is room for it. */
private fun shelf(context: Context, layout: Int, look: IntArray, hDp: Int): RemoteViews = RemoteViews(context.packageName, layout).apply {
    tint(R.id.widget_back, look[CoverLook.BACKGROUND])
    setTextColor(R.id.widget_header, look[CoverLook.ON])
    setViewVisibility(R.id.widget_head, if (hDp >= HEADED) View.VISIBLE else View.GONE)
}

/** The height left for the tiles of a shelf [hDp] high. */
private fun tilesDp(hDp: Int): Int = hDp - 24 - (if (hDp >= HEADED) HEAD else 0)

/**
 * "For you" on the home screen: favourites and the day's mixes, each tile as the Home page draws it, as
 * many as fit; a tap plays the mix (see [Widgets.playOrOpen]). Drawn when placed or resized, and when the app is left ([refresh]),
 * since the mixes are drawn anew each day while it is used. Never polled.
 */
class MixesWidget : AppWidgetProvider() {
    override fun onReceive(context: Context, intent: Intent) {
        super.onReceive(context, intent)
        when (intent.action) {
            Widgets.ACTION_PLAY -> Widgets.played(this, context, intent)
            dev.nori.music.playback.PlaybackService.ACTION_STATE -> { Widgets.heard(intent); if (Widgets.queueMoved(context, "mixes")) refresh(context) }
        }
    }

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) = draw(context.applicationContext, ids)

    override fun onAppWidgetOptionsChanged(context: Context, manager: AppWidgetManager, id: Int, options: Bundle) = draw(context.applicationContext, intArrayOf(id))

    companion object {
        /** Draws every placed one again. */
        fun refresh(context: Context) {
            val ids = Widgets.ids(context, MixesWidget::class.java)
            if (ids.isNotEmpty()) draw(context.applicationContext, ids)
        }

        private fun draw(context: Context, ids: IntArray) {
            for (id in ids) Widgets.draw("mixes$id") {
                val nori = Nori.get(context)
                val look = Widgets.theme(context).look
                val (wDp, hDp) = Widgets.sizeDp(context, id)
                val v = shelf(context, R.layout.widget_mixes, look, hDp)
                val tiles = if (!nori.settings.value.loggedIn) emptyList() else withContext(Dispatchers.IO) {
                    val taste = nori.settings.value.tasteModel
                    // As the Home row has them: the mixes drawn for today, the starred songs handed over first.
                    if (taste) runCatching { MixStore.warm(nori) }
                    // As MixesViewModel: a failed hand-over leaves the favourites empty rather than unknown.
                    withTimeoutOrNull(10_000) { runCatching { nori.library.handFavourites().firstOrNull() }.onFailure { runCatching { nori.core.mixFavourites(emptyList()) } } }
                    runCatching { nori.core.mixCards(taste) }.getOrDefault(emptyList())
                }
                val tileH = tilesDp(hDp)
                val n = ((wDp - 2 * SIDE + GAP).toFloat() / (tileH + GAP)).roundToInt().coerceIn(1, MIX_SLOTS.size).coerceAtMost(tiles.size.coerceAtLeast(1))
                val tileW = (wDp - 2 * SIDE - GAP * (n - 1)) / n
                val say = Say.current
                val pw = Widgets.px(context, tileW.toFloat(), 400)
                val ph = (pw * tileH / tileW.toFloat()).toInt().coerceAtLeast(1)
                val shown = tiles.take(n)
                coroutineScope {
                    shown.mapIndexed { i, t ->
                        async {
                            val covers = t.covers.take(4).map { c -> async { Widgets.picture(context, nori.library.coverUrl(c, CoverSize.ROW), pw / 2) } }.awaitAll()
                            val colours = dev.nori.music.ffi.library.mixTileColours(t.id).map { it.toInt() }
                            v.setImageViewBitmap(MIX_ART[i], Painter.mix(covers.filterNotNull(), colours, pw, ph, 12f * pw / tileW))
                        }
                    }.awaitAll()
                }
                MIX_SLOTS.forEachIndexed { i, slot -> v.setViewVisibility(slot, if (i < shown.size) View.VISIBLE else View.GONE) }
                shown.forEachIndexed { i, t ->
                    v.setTextViewText(MIX_NAME[i], say.mixName(t.name))
                    v.setImageViewResource(MIX_MARK[i], if (t.favourites) R.drawable.widget_heart else R.drawable.widget_sparkle)
                    v.setOnClickPendingIntent(MIX_SLOTS[i], Widgets.playOrOpen(context, MixesWidget::class.java, PageOrigin(OriginKind.MIX, t.id)))
                }
                AppWidgetManager.getInstance(context).updateAppWidget(id, v)
            }
        }
    }
}

/**
 * Random albums, as the Home page's Random row: as many covers as fit, a tap playing the album (see [Widgets.playOrOpen]), and the
 * shuffle drawing others. The same albums are kept through a resize; drawn anew when placed and on the
 * shuffle only. Never polled.
 */
class AlbumsWidget : AppWidgetProvider() {
    override fun onReceive(context: Context, intent: Intent) {
        super.onReceive(context, intent)
        when (intent.action) {
            SHUFFLE -> {
                drawn = emptyList()
                draw(context.applicationContext, Widgets.ids(context, AlbumsWidget::class.java))
            }
            Widgets.ACTION_PLAY -> Widgets.played(this, context, intent)
            dev.nori.music.playback.PlaybackService.ACTION_STATE -> {
                Widgets.heard(intent)
                if (Widgets.queueMoved(context, "albums")) draw(context.applicationContext, Widgets.ids(context, AlbumsWidget::class.java))
            }
        }
    }

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) = draw(context.applicationContext, ids)

    override fun onAppWidgetOptionsChanged(context: Context, manager: AppWidgetManager, id: Int, options: Bundle) = draw(context.applicationContext, intArrayOf(id))

    private companion object {
        const val SHUFFLE = "dev.nori.music.widget.SHUFFLE_ALBUMS"

        /** The albums drawn last, shared by every placed one. Main thread. */
        var drawn: List<Album> = emptyList()

        /** The side of a cover the grid aims for, dp. */
        const val AIM = 84

        fun draw(context: Context, ids: IntArray) {
            for (id in ids) Widgets.draw("albums$id") {
                val nori = Nori.get(context)
                val theme = Widgets.theme(context)
                val look = theme.look
                val (wDp, hDp) = Widgets.sizeDp(context, id)
                val v = shelf(context, R.layout.widget_albums, look, hDp)
                v.tint(R.id.widget_shuffle_glyph, look[CoverLook.ACCENT])
                v.setOnClickPendingIntent(R.id.widget_shuffle, Widgets.self(context, AlbumsWidget::class.java, SHUFFLE))
                val cols = ((wDp - 2 * SIDE + GAP).toFloat() / (AIM + GAP)).roundToInt().coerceIn(1, ALBUMS[0].size)
                val side = (wDp - 2 * SIDE - GAP * (cols - 1)) / cols
                val rows = ((tilesDp(hDp) + GAP).toFloat() / (side + GAP)).roundToInt().coerceIn(1, ROWS.size)
                val want = rows * cols
                if (drawn.size < want && nori.settings.value.loggedIn) {
                    val more = withTimeoutOrNull(15_000) { runCatching { nori.library.albums(AlbumSort.RANDOM, want).first() }.getOrNull() }
                    if (more != null) drawn = (drawn + more.filter { a -> drawn.none { it.id == a.id } }).take(maxOf(want, drawn.size))
                }
                val albums = drawn.take(want)
                val px = Widgets.px(context, side.toFloat(), 300)
                val r = 12 * px / side.toFloat()
                val pictures = coroutineScope { albums.map { a -> async { Widgets.picture(context, nori.library.coverUrl(a.coverArt, CoverSize.CARD), px) } }.awaitAll() }
                ROWS.forEachIndexed { row, rowId -> v.setViewVisibility(rowId, if (row < rows) View.VISIBLE else View.GONE) }
                for (row in 0 until ROWS.size) for (col in ALBUMS[row].indices) {
                    val cell = ALBUMS[row][col]
                    val i = row * cols + col
                    if (row >= rows || col >= cols) { v.setViewVisibility(cell, View.GONE); continue }
                    v.setViewVisibility(cell, View.VISIBLE)
                    val a = albums.getOrNull(i)
                    v.setImageViewBitmap(cell, Painter.cover(context, pictures.getOrNull(i), px, px, r, theme.look))
                    if (a != null) {
                        v.setContentDescription(cell, a.name)
                        v.setOnClickPendingIntent(cell, Widgets.playOrOpen(context, AlbumsWidget::class.java, PageOrigin(OriginKind.ALBUM, a.id)))
                    }
                }
                AppWidgetManager.getInstance(context).updateAppWidget(id, v)
            }
        }
    }
}
