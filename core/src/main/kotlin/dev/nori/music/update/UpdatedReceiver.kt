package dev.nori.music.update

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Android's word that this app was just replaced by a newer version; see [Updates.replaced]. */
class UpdatedReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == Intent.ACTION_MY_PACKAGE_REPLACED) Updates.replaced(context)
    }
}
