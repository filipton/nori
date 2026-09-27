package dev.nori.music.update

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Android's answers about an update's install session, handed to [Updates]. Not exported: only our own PendingIntent reaches it. */
class UpdateStatusReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Updates.ACTION_STATUS) return
        dev.nori.music.Nori.get(context).updates.onStatus(intent)
    }
}
