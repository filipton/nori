package dev.nori.music.app.widget

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Bitmap
import android.os.SystemClock
import android.util.LruCache
import android.view.KeyEvent
import android.widget.RemoteViews
import androidx.media3.session.MediaButtonReceiver
import dev.nori.music.Nori
import dev.nori.music.app.MainActivity
import dev.nori.music.app.R
import dev.nori.music.app.ui.CoverSize
import dev.nori.music.data.CoverLoader
import dev.nori.music.ffi.library.PageQueue
import dev.nori.music.ffi.model.OriginKind
import dev.nori.music.ffi.model.PageOrigin
import dev.nori.music.playback.PlaybackService
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlin.coroutines.resume

/** What the playback service last announced (PlaybackService.announce): the song, and where it was when. */
data class Playing(
    val title: String? = null,
    val artist: String? = null,
    val id: String? = null,
    val cover: String? = null,
    val playing: Boolean = false,
    val positionMs: Long = 0,
    /** elapsedRealtime when [positionMs] was true. */
    val atMs: Long = 0,
    val speed: Float = 1f,
) {
    /** Where the song is now, carried on from the announcement while it plays. */
    fun positionNow(): Long = if (!playing) positionMs else positionMs + ((SystemClock.elapsedRealtime() - atMs) * speed).toLong()
}

/**
 * What every home-screen widget shares: the song last announced, the app's theme and a cover's own look
 * (the colours its page wears in the app, from nori-look), the pictures, and the intents a tap sends.
 * Nothing here polls: widgets are drawn when placed or resized, when the service announces a change, and
 * when a widget asks (the random albums' shuffle, the lyrics' next line).
 */
object Widgets {
    /** The last announcement. Main thread. */
    var playing = Playing()
        private set

    /** Takes in [intent], an announcement. Main thread. */
    fun heard(intent: Intent) {
        playing = Playing(
            intent.getStringExtra(PlaybackService.EXTRA_TITLE), intent.getStringExtra(PlaybackService.EXTRA_ARTIST),
            intent.getStringExtra(PlaybackService.EXTRA_ID), intent.getStringExtra(PlaybackService.EXTRA_COVER),
            intent.getBooleanExtra(PlaybackService.EXTRA_PLAYING, false), intent.getLongExtra(PlaybackService.EXTRA_POSITION, 0),
            intent.getLongExtra(PlaybackService.EXTRA_AT, SystemClock.elapsedRealtime()), intent.getFloatExtra(PlaybackService.EXTRA_SPEED, 1f),
        )
    }

    /**
     * The song for the widgets: the last announcement or, before there has been one (the app just started),
     * the song the app's queue was put back on. Main thread.
     */
    fun now(context: Context): Playing {
        if (playing.id != null) return playing
        val nori = Nori.get(context)
        val s = runCatching { nori.player.state.value.current }.getOrNull() ?: return playing
        return Playing(
            s.title, s.artist, s.id, runCatching { nori.library.coverUrl(s.coverArt, dev.nori.music.playback.NOTIFICATION_ART) }.getOrNull(),
            false, runCatching { nori.player.positionMs }.getOrDefault(0L), SystemClock.elapsedRealtime(),
        )
    }

    /** Where widgets are drawn: the main thread, with the reading and decoding sent off it. */
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val drawing = HashMap<String, Job>()

    /** Draws with [block] for [key], dropping a drawing of the same widget still under way. Main thread. */
    fun draw(key: String, block: suspend () -> Unit) {
        drawing.remove(key)?.cancel()
        drawing[key] = scope.launch {
            runCatching { block() }.onFailure { if (it !is kotlinx.coroutines.CancellationException) android.util.Log.w("nori", "widget $key", it) }
        }
    }

    fun ids(context: Context, provider: Class<*>): IntArray =
        AppWidgetManager.getInstance(context).getAppWidgetIds(ComponentName(context, provider))

    /** The app's theme as its screens wear it: dark or not, AMOLED black, and the look dressed from its scheme. */
    class Theme(val dark: Boolean, val amoled: Boolean, val look: IntArray)

    fun theme(context: Context): Theme {
        val prefs = Nori.get(context).settings.value
        val system = context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        val dark = dev.nori.music.ffi.settings.themeIsDark(prefs.theme, system)
        return Theme(dark, dark && prefs.amoled, dev.nori.music.app.ui.plainLook(dev.nori.music.app.ui.schemeOf(context, prefs, dark)))
    }

    /** A cover's look and its wash, as its page in the app has them. */
    class Dressed(val look: IntArray, val wash: Bitmap?)

    private val looks = LruCache<String, Dressed>(8)

    /** The look of the page [url]'s cover heads in [theme]; null when there is no cover to take it from. */
    suspend fun coverLook(context: Context, url: String?, theme: Theme): Dressed? {
        if (url == null) return null
        val key = "$url|${theme.dark}|${theme.amoled}"
        looks.get(key)?.let { return it }
        val pages = withContext(Dispatchers.IO) {
            runCatching { CoverLoader.get(context).colours(url, CoverSize.ROW, theme.dark, !theme.amoled, theme.amoled) }.getOrNull()
        } ?: return null
        val c = (if (theme.amoled) pages.black else pages.plain) ?: return null
        return Dressed(c.look, c.wash).also { looks.put(key, it) }
    }

    /**
     * The cover at [url], [px] a side, or null when there is none. The loader's own are in the GPU's memory
     * for the app's screens; a widget's picture is painted on the CPU, so it gets a copy it can read.
     */
    suspend fun picture(context: Context, url: String?, px: Int): Bitmap? {
        if (url == null) return null
        val b: Bitmap = withContext(Dispatchers.Main) {
            suspendCancellableCoroutine<Bitmap?> { c ->
                val request = CoverLoader.get(context).load(url, px, px) { c.resume(it) }
                c.invokeOnCancellation { scope.launch { request.cancel() } }
            }
        } ?: return null
        return if (b.config == Bitmap.Config.HARDWARE) withContext(Dispatchers.Default) { b.copy(Bitmap.Config.ARGB_8888, false) } else b
    }

    /** A widget's size in dp: upright, its narrowest width and tallest height, which is how the launcher lays it out. */
    fun sizeDp(context: Context, id: Int): Pair<Int, Int> {
        val o = AppWidgetManager.getInstance(context).getAppWidgetOptions(id)
        val upright = context.resources.configuration.orientation != Configuration.ORIENTATION_LANDSCAPE
        val w = o.getInt(if (upright) AppWidgetManager.OPTION_APPWIDGET_MIN_WIDTH else AppWidgetManager.OPTION_APPWIDGET_MAX_WIDTH)
        val h = o.getInt(if (upright) AppWidgetManager.OPTION_APPWIDGET_MAX_HEIGHT else AppWidgetManager.OPTION_APPWIDGET_MIN_HEIGHT)
        return (if (w > 0) w else 250) to (if (h > 0) h else 110)
    }

    /** Pixels for [dp], at most [cap]: a picture larger than that is scaled up by the launcher instead. */
    fun px(context: Context, dp: Float, cap: Int = 900): Int = (dp * context.resources.displayMetrics.density).toInt().coerceIn(1, cap)

    /** The widget's corners, in pixels of a picture drawn [scale] times the screen's. */
    fun radiusPx(context: Context, scale: Float = 1f): Float = context.resources.getDimension(R.dimen.widget_radius) * scale

    /** Opens the app at [route] (App's launch routes: a page's, "player", "lyrics"), or as it was when null. */
    fun open(context: Context, route: String?): PendingIntent = PendingIntent.getActivity(
        context, route.hashCode(),
        Intent(context, MainActivity::class.java).setAction(MainActivity.ACTION_WIDGET).putExtra(MainActivity.EXTRA_ROUTE, route)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** A media button, as a headset's: it works with the app and the service both gone. */
    fun key(context: Context, code: Int): PendingIntent = PendingIntent.getBroadcast(
        context, code,
        Intent(Intent.ACTION_MEDIA_BUTTON).setComponent(ComponentName(context, MediaButtonReceiver::class.java)).putExtra(Intent.EXTRA_KEY_EVENT, KeyEvent(KeyEvent.ACTION_DOWN, code)),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** Sends [action] to [provider]: a widget's own button. */
    fun self(context: Context, provider: Class<*>, action: String): PendingIntent = PendingIntent.getBroadcast(
        context, action.hashCode(), Intent(context, provider).setAction(action), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** A shelf tile's own tap: start the page [EXTRA_KIND] [EXTRA_ID]'s queue. */
    const val ACTION_PLAY = "dev.nori.music.widget.PLAY"
    private const val EXTRA_KIND = "kind"
    private const val EXTRA_ID = "id"

    /** Whether the queue is the one page [origin] started (nori-queue `playlist_from`), playing or paused. */
    fun playsFrom(context: Context, origin: PageOrigin): Boolean = runCatching { Nori.get(context).session.playlistFrom(PageQueue(origin)) }.getOrDefault(false)

    /**
     * A shelf tile's tap: the page's queue started from the home screen, or when it is the queue already, the
     * player opened on it, as a tap on the song playing does on its page. Decided as the tile is drawn: a
     * broadcast from a widget may not open the app, so the choice cannot wait for the tap. The shelves are
     * drawn again when the queue changes ([queueMoved]).
     */
    fun playOrOpen(context: Context, provider: Class<*>, origin: PageOrigin): PendingIntent =
        if (playsFrom(context, origin)) open(context, "player")
        else PendingIntent.getBroadcast(
            context, "${origin.kind}:${origin.id}".hashCode(),
            Intent(context, provider).setAction(ACTION_PLAY).putExtra(EXTRA_KIND, origin.kind.name).putExtra(EXTRA_ID, origin.id),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )

    /** The page an [ACTION_PLAY] is for. */
    fun originOf(intent: Intent): PageOrigin? {
        val kind = intent.getStringExtra(EXTRA_KIND)?.let { k -> runCatching { OriginKind.valueOf(k) }.getOrNull() } ?: return null
        return PageOrigin(kind, intent.getStringExtra(EXTRA_ID) ?: return null)
    }

    /**
     * Plays page [origin] from its first song, as its Play does: an album's songs, or a mix's draw. Nothing
     * when its queue is already the one playing. The player connects to the service for it if it has to.
     */
    suspend fun play(context: Context, origin: PageOrigin) {
        if (playsFrom(context, origin)) return
        val nori = Nori.get(context)
        val songs = when (origin.kind) {
            OriginKind.ALBUM -> nori.library.album(origin.id).first().songs
            OriginKind.MIX -> withContext(Dispatchers.IO) {
                // As the mix's page: favourites once the starred songs are handed over, a mix once drawn for today.
                if (origin.id == dev.nori.music.app.vm.FAVOURITES_MIX) runCatching { nori.library.handFavourites().firstOrNull() }
                else dev.nori.music.app.vm.MixStore.ensure(nori, origin.id)
                (nori.core.mixPage(origin.id) as? dev.nori.music.ffi.library.MixLookup.Ready)?.sheet?.songs.orEmpty()
            }
            else -> emptyList()
        }
        if (songs.isNotEmpty()) nori.player.play(songs, 0, from = origin)
    }

    /** Carries out [intent], an [ACTION_PLAY], while the broadcast is held open for it. */
    fun played(receiver: android.content.BroadcastReceiver, context: Context, intent: Intent) {
        val origin = originOf(intent) ?: return
        val pending = receiver.goAsync()
        scope.launch {
            try { kotlinx.coroutines.withTimeoutOrNull(9_000) { runCatching { play(context.applicationContext, origin) } } } finally { pending.finish() }
        }
    }

    private val origins = HashMap<String, Int>()

    /** Whether a new queue was set since shelf [key] last asked: its tiles' taps are then decided again. */
    fun queueMoved(context: Context, key: String): Boolean {
        val now = Nori.get(context).player.queueOrigin
        return origins.put(key, now) != now
    }

    /** Paints the glyph [id] in [colour]. */
    fun RemoteViews.tint(id: Int, colour: Int) = setInt(id, "setColorFilter", colour)
}
