package dev.nori.music.app.widget

import android.content.Context
import android.graphics.RectF
import android.view.KeyEvent
import android.widget.RemoteViews
import dev.nori.music.app.R
import dev.nori.music.app.widget.Widgets.tint
import dev.nori.music.look.CoverLook

/** A launcher's one row is up to about 130 dp high, two about 200: anything lower than this is one row. */
const val ONE_ROW = 140

/** How the song playing is laid out, by the widget's size. */
enum class Face {
    /** One row: the app's mini player. */
    MINI,
    /** Wider than tall: the cover at the start melting into the page, as the app on its side. */
    BAND,
    /** Taller than wide: the cover on top dissolving into the page, as an album's page opens. */
    TALL,
    /** Small and square: the cover alone, dissolving under the song's name, with its Play. */
    TILE,
    /** The cover widget one row high: the cover at the start melting into the page, the song and its Play beside it. */
    STRIP;

    companion object {
        fun of(wDp: Int, hDp: Int): Face = when {
            hDp < ONE_ROW -> MINI
            wDp < 180 -> TILE
            hDp < wDp * 0.8f -> BAND
            else -> TALL
        }
    }
}

/** The song playing as a widget shows it, in each [Face]: the player widget's and the cover widget's. */
object NowFaces {
    /** Around the mini player's cover, on every side; its corners are the widget's, less this, so the two run together. */
    private const val MINI_INSET = 8f
    /** The mini player's cover's corners at the least (Chrome's MiniPlayer). */
    private const val MINI_RADIUS = 7f

    /** Past the band's cover, as a share of it: where the words start, over its melted end. */
    private const val BAND_TEXT = 0.88f

    suspend fun views(context: Context, face: Face, wDp: Int, hDp: Int): RemoteViews {
        val p = Widgets.now(context)
        val theme = Widgets.theme(context)
        val density = context.resources.displayMetrics.density
        val layout = when (face) {
            Face.MINI -> R.layout.widget_player
            Face.BAND -> R.layout.widget_player_band
            Face.TALL -> R.layout.widget_player_tall
            Face.TILE -> R.layout.widget_cover
            Face.STRIP -> R.layout.widget_cover_row
        }
        val v = RemoteViews(context.packageName, layout)
        // The mini player is chrome, in the theme's own colours; the others are the song's page, in its cover's.
        val look = if (face == Face.MINI) theme.look else Widgets.coverLook(context, p.cover, theme)?.look ?: theme.look
        val ink: Int
        val quiet: Int
        if (face == Face.MINI) {
            ink = look[CoverLook.CHROME_CONTENT]
            quiet = look[CoverLook.CHROME_CONTENT_65]
            v.tint(R.id.widget_back, look[CoverLook.CHROME_SLAB])
            val side = Widgets.px(context, hDp - 2 * MINI_INSET)
            val radius = maxOf(MINI_RADIUS * density, Widgets.radiusPx(context) - MINI_INSET * density)
            v.setImageViewBitmap(R.id.widget_cover, Painter.cover(context, Widgets.picture(context, p.cover, side), side, side, radius, look))
        } else {
            ink = look[CoverLook.ON]
            quiet = look[CoverLook.ON_VARIANT]
            // Drawn at most 900 px a side and scaled up by the launcher: a cover is softer than that anyway once melted.
            val scale = minOf(1f, 900f / (maxOf(wDp, hDp) * density))
            val w = (wDp * density * scale).toInt().coerceAtLeast(1)
            val h = (hDp * density * scale).toInt().coerceAtLeast(1)
            val art = when (face) {
                Face.BAND, Face.STRIP -> {
                    val coverDp = minOf(hDp * 1.12f, wDp * 0.55f)
                    val start = (coverDp * BAND_TEXT * density).toInt()
                    v.setViewPadding(R.id.widget_open, start, 0, if (face == Face.BAND) (14 * density).toInt() else 0, 0)
                    RectF(0f, 0f, coverDp * density * scale, h.toFloat())
                }
                // Dissolved far enough (the hero's 0.8) by where the song's name starts, about 140 dp from the bottom.
                Face.TALL -> RectF(0f, 0f, w.toFloat(), minOf(wDp.toFloat(), (hDp - 140f) / 0.8f) * density * scale)
                else -> RectF(0f, 0f, w.toFloat(), h.toFloat())
            }
            val picture = Widgets.picture(context, p.cover, maxOf(art.width(), art.height()).toInt())
            v.setImageViewBitmap(R.id.widget_back, Painter.melt(context, picture, w, h, Widgets.radiusPx(context, scale), look, art, across = face == Face.BAND || face == Face.STRIP))
        }
        // Too narrow for the words beside it, a strip is the cover and its Play.
        if (face == Face.STRIP) v.setViewVisibility(R.id.widget_open, if (wDp < 220) android.view.View.INVISIBLE else android.view.View.VISIBLE)
        v.setTextViewText(R.id.widget_title, p.title ?: context.getString(R.string.nothing_playing))
        v.setTextViewText(R.id.widget_artist, p.artist.orEmpty())
        v.setTextColor(R.id.widget_title, ink)
        v.setTextColor(R.id.widget_artist, quiet)
        v.setImageViewResource(R.id.widget_toggle_glyph, if (p.playing) R.drawable.widget_pause else R.drawable.widget_play)
        v.setContentDescription(R.id.widget_toggle_glyph, context.getString(if (p.playing) R.string.pause else R.string.play))
        if (face == Face.TILE || face == Face.STRIP) {
            // The page's own Play (the hero's prominent pill), on the dissolved corner of the cover.
            v.tint(R.id.widget_plate, look[CoverLook.PILL])
            v.tint(R.id.widget_toggle_glyph, look[CoverLook.PILL_INK])
        } else {
            v.tint(R.id.widget_toggle_glyph, ink)
            v.tint(R.id.widget_next_glyph, ink)
            v.setOnClickPendingIntent(R.id.widget_next, Widgets.key(context, KeyEvent.KEYCODE_MEDIA_NEXT))
            if (face != Face.MINI) {
                v.tint(R.id.widget_previous_glyph, ink)
                v.setOnClickPendingIntent(R.id.widget_previous, Widgets.key(context, KeyEvent.KEYCODE_MEDIA_PREVIOUS))
            }
        }
        v.setOnClickPendingIntent(R.id.widget_toggle, Widgets.key(context, KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE))
        // The picture as well as the words: a narrow strip hides its words.
        val open = Widgets.open(context, "player")
        v.setOnClickPendingIntent(R.id.widget_open, open)
        v.setOnClickPendingIntent(R.id.widget_back, open)
        return v
    }
}
