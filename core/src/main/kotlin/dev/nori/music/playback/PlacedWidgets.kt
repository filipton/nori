package dev.nori.music.playback

import android.appwidget.AppWidgetManager
import android.content.Context

/**
 * Whether any of the app's home-screen widgets is placed. The service's announcement
 * ([PlaybackService.ACTION_STATE]) is for those alone, and each one starts every widget receiver in turn.
 * Asked of the system the first time it matters in a process, then kept by the widgets' own enabling
 * and disabling. Main thread.
 */
class PlacedWidgets(private val context: Context) {
    private var known: Boolean? = null

    /** The service's announcement while it runs, so a widget just placed is told the song at once. */
    var onPlaced: (() -> Unit)? = null

    val any: Boolean get() = known ?: count().also { known = it }

    /** A kind of widget was placed for the first time. */
    fun enabled() {
        known = true
        onPlaced?.invoke()
    }

    /** A kind of widget lost its last one; one of another kind may still be placed. */
    fun disabled() { known = null }

    private fun count(): Boolean {
        // Null where the device has no home-screen widgets (a car's own screen).
        val m = AppWidgetManager.getInstance(context) ?: return false
        return m.getInstalledProvidersForPackage(context.packageName, null).any { m.getAppWidgetIds(it.provider).isNotEmpty() }
    }
}
