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

/** Launch requests that play rather than open a page (a launcher shortcut's); App carries them out. */
const val SHUFFLE_SONGS = "shuffle:songs"
const val SHUFFLE_ALBUMS = "shuffle:albums"

class MainActivity : ComponentActivity() {
    private var started = false
    private val askNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}
    /** What was asked for from outside (the download notification, a launcher shortcut); App does it and clears this. */
    private val launchRoute = androidx.compose.runtime.mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        // A turn of the phone dissolves the old layout into the new one rather than spinning a picture of the
        // whole screen round, bar and all; the tab glyphs then turn upright in place (App). Seamless, as this
        // was, is not honoured with the status bar shown, and the system cut to black instead.
        window.attributes = window.attributes.apply { rotationAnimation = android.view.WindowManager.LayoutParams.ROTATION_ANIMATION_CROSSFADE }
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

    /** What an intent from outside asks for: the download notification's screen, or a launcher shortcut's (res/xml/shortcuts.xml). */
    private fun routeOf(intent: Intent?): String? = when (intent?.action) {
        ACTION_OPEN_DOWNLOADS -> "downloads"
        "dev.nori.music.SHORTCUT_SEARCH" -> "search"
        "dev.nori.music.SHORTCUT_SHUFFLE_SONGS" -> SHUFFLE_SONGS
        "dev.nori.music.SHORTCUT_SHUFFLE_ALBUMS" -> SHUFFLE_ALBUMS
        else -> null
    }

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
