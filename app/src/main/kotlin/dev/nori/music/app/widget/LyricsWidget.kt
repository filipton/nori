package dev.nori.music.app.widget

import android.appwidget.AppWidgetManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.widget.RemoteViews
import androidx.core.content.ContextCompat
import dev.nori.music.Nori
import dev.nori.music.app.R
import dev.nori.music.app.widget.Widgets.tint
import dev.nori.music.ffi.model.Lyrics
import dev.nori.music.look.CoverLook
import dev.nori.music.playback.PlaybackService
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/** The line being sung, large, and the next one under it, on the song's own colours. See [LyricsLine]. */
class LyricsWidget : NoriWidget() {
    override fun onReceive(context: Context, intent: Intent) {
        super.onReceive(context, intent)
        if (intent.action == PlaybackService.ACTION_STATE) {
            Widgets.heard(intent)
            LyricsLine.changed(context.applicationContext)
        }
    }

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) = LyricsLine.changed(context.applicationContext)

    override fun onAppWidgetOptionsChanged(context: Context, manager: AppWidgetManager, id: Int, options: Bundle) = LyricsLine.changed(context.applicationContext)

    override fun onDisabled(context: Context) {
        super.onDisabled(context)
        LyricsLine.stop(context.applicationContext)
    }
}

/**
 * Keeps the lyrics widgets on the line being sung. The song's lyrics are looked up as the app's lyrics are
 * (the server's, then the lyrics services' when the settings allow), once per song and only with the screen
 * on. Then one wake per line: the next is posted for when it starts, and only while music plays, the screen
 * is on and a lyrics widget is placed - nothing ticks between lines, and nothing at all with the screen off.
 * A line's change sends only its words; the picture behind them is sent with the song.
 */
internal object LyricsLine {
    private val main = Handler(Looper.getMainLooper())
    private var app: Context? = null
    private val next = Runnable { app?.let(::line) }

    /** The song the lyrics are for, and what was found: null while looking, or when there are none. */
    private var song: String? = null
    private var lyrics: Lyrics? = null
    private var looked = false
    private var looking: Job? = null

    /** The screen's turning on and off, watched while a lyrics widget is placed. */
    private var watching = false
    private val screen = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == Intent.ACTION_SCREEN_ON) changed(context.applicationContext) else main.removeCallbacks(next)
        }
    }

    private fun on(context: Context) = context.getSystemService(PowerManager::class.java).isInteractive

    /** The song, its playing or the widgets changed: everything drawn again. Main thread. */
    fun changed(context: Context) {
        app = context
        if (Widgets.ids(context, LyricsWidget::class.java).isEmpty()) { stop(context); return }
        if (!watching) {
            watching = true
            ContextCompat.registerReceiver(context, screen, IntentFilter(Intent.ACTION_SCREEN_ON).apply { addAction(Intent.ACTION_SCREEN_OFF) }, ContextCompat.RECEIVER_NOT_EXPORTED)
        }
        val id = Widgets.now(context).id
        if (id != song) {
            song = id
            lyrics = null
            looked = id == null
            looking?.cancel()
            looking = null
        }
        if (id != null && !looked && looking == null && on(context)) looking = Widgets.scope.launch { look(context, id) }
        full(context)
    }

    private suspend fun look(context: Context, id: String) {
        val library = Nori.get(context).library
        runCatching {
            val s = library.song(id)
            // Each better answer as it comes, the server's first; a service that never answers is let go.
            if (s != null) withTimeoutOrNull(30_000) { library.lyricsFor(s).collect { if (song == id) { lyrics = it.lyrics; line(context) } } }
        }
        if (song == id) {
            looked = true
            looking = null
            line(context)
        }
    }

    fun stop(context: Context) {
        main.removeCallbacks(next)
        looking?.cancel()
        looking = null
        if (watching) { watching = false; runCatching { context.unregisterReceiver(screen) } }
    }

    /** The words shown now: the line, the next one, whether the line is a note rather than lyrics, and when it changes (ms), if it will. */
    private class Words(val line: String, val next: String, val note: Boolean, val waitMs: Long?)

    private fun words(context: Context): Words {
        val p = Widgets.now(context)
        val l = lyrics
        if (p.id == null) return Words(context.getString(R.string.nothing_playing), "", true, null)
        if (l == null || l.lines.none { it.text.isNotBlank() }) return Words(if (looked) context.getString(R.string.note_no_lyrics) else NOTE, "", looked, null)
        val said = l.lines.filter { it.text.isNotBlank() }
        if (!l.synced) return Words(said[0].text, said.getOrNull(1)?.text.orEmpty(), false, null)
        // The lines are sung by their start times, the lyrics' found offset added to the playhead (as LyricsClock does).
        val t = p.positionNow() + l.offsetMs
        val at = l.lines.indexOfLast { it.startMs <= t }
        val now = l.lines.getOrNull(at)?.text?.takeIf { it.isNotBlank() } ?: NOTE
        val then = l.lines.drop(at + 1).firstOrNull { it.text.isNotBlank() }?.text.orEmpty()
        val wait = l.lines.getOrNull(at + 1)?.let { ((it.startMs - t) / p.speed.coerceAtLeast(0.1f)).toLong().coerceAtLeast(50) }
        return Words(now, then, false, wait)
    }

    /** Between lines, and in a song's instrumental stretches. */
    private const val NOTE = "♪"

    /** The words of [v] and their colours, in [look]. */
    private fun write(context: Context, v: RemoteViews, look: IntArray): Words {
        val w = words(context)
        v.setTextViewText(R.id.widget_line, w.line)
        v.setTextColor(R.id.widget_line, look[if (w.note) CoverLook.ON_VARIANT else CoverLook.ON])
        v.setTextViewText(R.id.widget_next_line, w.next)
        v.setTextColor(R.id.widget_next_line, look[CoverLook.ON_45])
        return w
    }

    private fun schedule(context: Context, w: Words) {
        main.removeCallbacks(next)
        if (w.waitMs != null && Widgets.now(context).playing && on(context)) main.postDelayed(next, w.waitMs)
    }

    /** The look the last full drawing wore, for the line changes between. */
    private var look: IntArray? = null

    /** Only the words, to every lyrics widget, and the next line's wake. */
    private fun line(context: Context) {
        val ids = Widgets.ids(context, LyricsWidget::class.java)
        val l = look
        if (ids.isEmpty() || l == null) { if (ids.isNotEmpty()) full(context); return }
        val v = RemoteViews(context.packageName, R.layout.widget_lyrics)
        val w = write(context, v, l)
        AppWidgetManager.getInstance(context).partiallyUpdateAppWidget(ids, v)
        schedule(context, w)
    }

    /** Everything: the page from the song's cover, the song, and the words. */
    private fun full(context: Context) {
        val manager = AppWidgetManager.getInstance(context)
        for (id in Widgets.ids(context, LyricsWidget::class.java)) Widgets.draw("lyrics$id") {
            val p = Widgets.now(context)
            val theme = Widgets.theme(context)
            val dressed = Widgets.coverLook(context, p.cover, theme)
            val look = dressed?.look ?: theme.look
            this.look = look
            val (wDp, hDp) = Widgets.sizeDp(context, id)
            val density = context.resources.displayMetrics.density
            val scale = minOf(1f, 600f / (maxOf(wDp, hDp) * density))
            val v = RemoteViews(context.packageName, R.layout.widget_lyrics)
            // The wash is a blur 128 px a side: drawn at most 600 px, it loses nothing.
            v.setImageViewBitmap(
                R.id.widget_back,
                Painter.wash(dressed?.wash, (wDp * density * scale).toInt().coerceAtLeast(1), (hDp * density * scale).toInt().coerceAtLeast(1), Widgets.radiusPx(context, scale), look),
            )
            // One row high, the line alone: no song over it, nothing after it.
            val short = hDp < ONE_ROW
            v.setViewVisibility(R.id.widget_head, if (short) android.view.View.GONE else android.view.View.VISIBLE)
            v.setViewVisibility(R.id.widget_next_line, if (short) android.view.View.GONE else android.view.View.VISIBLE)
            if (short) v.setViewPadding(R.id.widget_open, (16 * density).toInt(), (10 * density).toInt(), (16 * density).toInt(), (10 * density).toInt())
            val thumb = Widgets.px(context, 30f)
            v.setImageViewBitmap(R.id.widget_cover, Painter.cover(context, Widgets.picture(context, p.cover, thumb), thumb, thumb, 6 * density, look))
            v.setTextViewText(R.id.widget_title, listOfNotNull(p.title, p.artist?.takeIf { it.isNotEmpty() }).joinToString(" · ").ifEmpty { context.getString(R.string.lyrics) })
            v.setTextColor(R.id.widget_title, look[CoverLook.ON_VARIANT])
            val w = write(context, v, look)
            v.setOnClickPendingIntent(R.id.widget_open, Widgets.open(context, if (p.id == null) null else "lyrics"))
            manager.updateAppWidget(id, v)
            schedule(context, w)
        }
    }
}
