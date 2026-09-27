package dev.nori.music.app

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Looper
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.ui.platform.createLifecycleAwareWindowRecomposer
import androidx.activity.result.contract.ActivityResultContracts
import dev.nori.music.Nori
import dev.nori.music.downloads.ACTION_OPEN_DOWNLOADS
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.launch
import dev.nori.music.app.ui.App
import dev.nori.music.settings.loggedIn

class MainActivity : ComponentActivity() {
    private var started = false
    private val askNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}
    /** A screen asked for from outside (the download notification); App opens it and clears this. */
    private val launchRoute = androidx.compose.runtime.mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (Build.VERSION.SDK_INT >= 33 && savedInstanceState == null) askNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        // The composition runs under the app's own animation speed, not Android's: see AppMotion.
        @OptIn(androidx.compose.ui.InternalComposeUiApi::class)
        val recomposer = window.decorView.createLifecycleAwareWindowRecomposer(dev.nori.music.app.ui.AppMotion, lifecycle)
        // Only a fresh launch: a recreated activity (rotation) already went where its intent asked.
        if (savedInstanceState == null) launchRoute.value = routeOf(intent)
        setContent(recomposer) { App(launchRoute) }
    }

    /** The app already running: singleTop hands the notification's intent here instead of to a new activity. */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        routeOf(intent)?.let { launchRoute.value = it }
    }

    private fun routeOf(intent: Intent?): String? = if (intent?.action == ACTION_OPEN_DOWNLOADS) "downloads" else null

    override fun onStart() {
        // Before the screen draws again: the song may have changed while it was away (see catchUp), and the
        // first frame is to show the song playing now. Until that frame is out, Android shows the last one
        // drawn before the app went; from it, the page cross-fades to the new song as it would on screen.
        Nori.get(this).player.catchUp()
        // Back from Android's "install unknown apps" page: an update waiting for it goes on.
        Nori.get(this).updates.resumed()
        super.onStart()
        // Binding starts the playback service, which builds a player on this thread. Let the first frame out first.
        window.decorView.post { Looper.myQueue().addIdleHandler { if (started && Nori.get(this).settings.value.loggedIn) { Nori.get(this).player.connect(); pickAddress() }; false } }
        started = true
    }

    /** Coming to the foreground is when the network may have changed (home Wi-Fi vs. outside). */
    private fun pickAddress() = lifecycleScope.launch { runCatching { Nori.get(this@MainActivity).chooseAddress() } }

    override fun onStop() {
        super.onStop()
        started = false
        // The service keeps playing on its own; holding a controller while hidden would only keep callbacks flowing.
        Nori.get(this).player.disconnect()
    }
}
