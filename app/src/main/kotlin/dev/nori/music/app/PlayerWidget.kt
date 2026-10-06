package dev.nori.music.app

import android.appwidget.AppWidgetManager
import android.content.Context
import android.content.Intent
import android.os.Bundle
import dev.nori.music.app.widget.Face
import dev.nori.music.app.widget.NoriWidget
import dev.nori.music.app.widget.NowFaces
import dev.nori.music.app.widget.Widgets
import dev.nori.music.playback.PlaybackService

/**
 * Home-screen player, one widget at any size: the mini player one row high, and larger the cover melting
 * into its page as the app draws it (see [Face]). It never polls (updatePeriodMillis is 0): the playback
 * service announces track and play-state changes, and that broadcast is the only thing that redraws it.
 * Buttons are plain media-button intents, so they work with the app and the service both dead. Kept under
 * this name, which widgets already placed are bound to.
 */
class PlayerWidget : NoriWidget() {
    override fun onReceive(context: Context, intent: Intent) {
        super.onReceive(context, intent)
        if (intent.action == PlaybackService.ACTION_STATE) {
            Widgets.heard(intent)
            draw(context, Widgets.ids(context, PlayerWidget::class.java))
        }
    }

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) = draw(context, ids)

    override fun onAppWidgetOptionsChanged(context: Context, manager: AppWidgetManager, id: Int, options: Bundle) = draw(context, intArrayOf(id))

    private fun draw(context: Context, ids: IntArray) {
        val app = context.applicationContext
        for (id in ids) Widgets.draw("player$id") {
            val (w, h) = Widgets.sizeDp(app, id)
            AppWidgetManager.getInstance(app).updateAppWidget(id, NowFaces.views(app, Face.of(w, h), w, h))
        }
    }
}
