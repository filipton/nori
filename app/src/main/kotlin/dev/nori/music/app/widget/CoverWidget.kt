package dev.nori.music.app.widget

import android.appwidget.AppWidgetManager
import android.content.Context
import android.content.Intent
import android.os.Bundle
import dev.nori.music.playback.PlaybackService

/** The cover of the song playing, dissolving under its name, with its Play ([Face.TILE]; one row high, [Face.STRIP]). Never polled. */
class CoverWidget : NoriWidget() {
    override fun onReceive(context: Context, intent: Intent) {
        super.onReceive(context, intent)
        if (intent.action == PlaybackService.ACTION_STATE) {
            Widgets.heard(intent)
            draw(context, Widgets.ids(context, CoverWidget::class.java))
        }
    }

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) = draw(context, ids)

    override fun onAppWidgetOptionsChanged(context: Context, manager: AppWidgetManager, id: Int, options: Bundle) = draw(context, intArrayOf(id))

    private fun draw(context: Context, ids: IntArray) {
        val app = context.applicationContext
        for (id in ids) Widgets.draw("cover$id") {
            val (w, h) = Widgets.sizeDp(app, id)
            AppWidgetManager.getInstance(app).updateAppWidget(id, NowFaces.views(app, if (h < ONE_ROW) Face.STRIP else Face.TILE, w, h))
        }
    }
}
