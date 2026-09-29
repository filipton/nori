package dev.nori.music.app.widget

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Rect
import android.graphics.RectF
import android.graphics.Shader
import androidx.core.content.ContextCompat
import dev.nori.music.app.R
import dev.nori.music.look.CoverLook

/**
 * The widgets' pictures, painted once per change into a Bitmap the launcher shows: the cover dissolving
 * into its page with the album page's own stops, in the look's colours, the card's corners rounded to the
 * launcher's. RemoteViews has no gradients or clips of its own to do it with.
 */
object Painter {
    /** Where an album page's cover dissolves (nori_look::sleeve::HERO_STOPS): clear, then HERO_EDGE, HERO_MID, the page. */
    private val HERO = floatArrayOf(0.60f, 0.76f, 0.88f, 1f)

    private fun canvas(w: Int, h: Int): Pair<Bitmap, Canvas> = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888).let { it to Canvas(it) }

    /** Clips [c] to a card [w] x [h] with corners [r]. */
    private fun round(c: Canvas, w: Int, h: Int, r: Float) {
        c.clipPath(Path().apply { addRoundRect(RectF(0f, 0f, w.toFloat(), h.toFloat()), r, r, Path.Direction.CW) })
    }

    /** [b] drawn into [dst], cropped from the middle to fill it, as the app's covers are. */
    private fun crop(c: Canvas, b: Bitmap, dst: RectF) {
        val k = maxOf(dst.width() / b.width, dst.height() / b.height)
        val sw = dst.width() / k
        val sh = dst.height() / k
        val sx = (b.width - sw) / 2f
        val sy = (b.height - sh) / 2f
        c.drawBitmap(b, Rect(sx.toInt(), sy.toInt(), (sx + sw).toInt(), (sy + sh).toInt()), dst, Paint(Paint.FILTER_BITMAP_FLAG))
    }

    /** What a missing cover shows (the app's Cover): VEIL_13 running to VEIL_6, the note in ON_22. */
    private fun plate(context: Context, c: Canvas, dst: RectF, look: IntArray) {
        c.drawRect(dst, Paint().apply { shader = LinearGradient(dst.left, dst.top, dst.right, dst.bottom, look[CoverLook.VEIL_13], look[CoverLook.VEIL_6], Shader.TileMode.CLAMP) })
        val note = ContextCompat.getDrawable(context, R.drawable.widget_note)!!.mutate()
        val s = (minOf(dst.width(), dst.height()) * 0.34f).toInt()
        val x = (dst.centerX() - s / 2f).toInt()
        val y = (dst.centerY() - s / 2f).toInt()
        note.setBounds(x, y, x + s, y + s)
        note.setTint(look[CoverLook.ON_22])
        note.draw(c)
    }

    /** A cover [w] x [h] with corners [r], or its plate when there is no picture. */
    fun cover(context: Context, picture: Bitmap?, w: Int, h: Int, r: Float, look: IntArray): Bitmap {
        val (b, c) = canvas(w, h)
        round(c, w, h, r)
        val all = RectF(0f, 0f, w.toFloat(), h.toFloat())
        if (picture != null) crop(c, picture, all) else plate(context, c, all, look)
        return b
    }

    /**
     * A card [w] x [h] in the page's colour with the cover over [art], dissolving [across] (towards the end)
     * or down into the page with the hero's stops, as the album page and its band on its side do.
     */
    fun melt(context: Context, picture: Bitmap?, w: Int, h: Int, r: Float, look: IntArray, art: RectF, across: Boolean): Bitmap {
        val (b, c) = canvas(w, h)
        round(c, w, h, r)
        c.drawColor(look[CoverLook.BACKGROUND])
        if (picture != null) crop(c, picture, art) else plate(context, c, art, look)
        val colours = intArrayOf(Color.TRANSPARENT, look[CoverLook.HERO_EDGE], look[CoverLook.HERO_MID], look[CoverLook.BACKGROUND])
        val shader = if (across) LinearGradient(art.left, 0f, art.right, 0f, colours, HERO, Shader.TileMode.CLAMP)
        else LinearGradient(0f, art.top, 0f, art.bottom, colours, HERO, Shader.TileMode.CLAMP)
        c.drawRect(if (across) RectF(art.left, 0f, art.right, h.toFloat()) else RectF(0f, art.top, w.toFloat(), art.bottom), Paint().apply { this.shader = shader })
        return b
    }

    /** The page behind the lyrics: the cover's wash stretched over it, as behind the player, or its colour on black. */
    fun wash(wash: Bitmap?, w: Int, h: Int, r: Float, look: IntArray): Bitmap {
        val (b, c) = canvas(w, h)
        round(c, w, h, r)
        c.drawColor(look[CoverLook.BACKGROUND])
        if (wash != null) c.drawBitmap(wash, null, RectF(0f, 0f, w.toFloat(), h.toFloat()), Paint(Paint.FILTER_BITMAP_FLAG))
        return b
    }

    /**
     * A "For you" tile as the Home page draws it (SmartScreens' MixArt): the mix's colour running to its
     * deeper one, its covers - four as a square of four - and the band rising under its name. [tile] is
     * nori-core's `mix_tile_colours`.
     */
    fun mix(covers: List<Bitmap>, tile: List<Int>, w: Int, h: Int, r: Float): Bitmap {
        val (b, c) = canvas(w, h)
        round(c, w, h, r)
        c.drawRect(0f, 0f, w.toFloat(), h.toFloat(), Paint().apply { shader = LinearGradient(0f, 0f, w.toFloat(), h.toFloat(), tile[0], tile[1], Shader.TileMode.CLAMP) })
        if (covers.size >= 4) {
            val hw = w / 2f
            val hh = h / 2f
            for (i in 0..3) crop(c, covers[i], RectF((i % 2) * hw, (i / 2) * hh, (i % 2 + 1) * hw, (i / 2 + 1) * hh))
        } else if (covers.isNotEmpty()) crop(c, covers[0], RectF(0f, 0f, w.toFloat(), h.toFloat()))
        c.drawRect(0f, 0f, w.toFloat(), h.toFloat(), Paint().apply {
            shader = LinearGradient(0f, 0f, 0f, h.toFloat(), intArrayOf(tile[2], tile[2], tile[3], tile[4]), floatArrayOf(0f, 0.38f, 0.72f, 1f), Shader.TileMode.CLAMP)
        })
        return b
    }
}
