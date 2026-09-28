package dev.nori.music.app

import android.app.Application
import dev.nori.music.Nori

class NoriApp : Application() {
    override fun onCreate() {
        super.onCreate()
        dev.nori.music.app.ui.Say.use(resources)
        dev.nori.music.net.Failures.use(resources)
        // Loading the native core and opening SQLite overlaps with the activity being created instead of preceding it.
        val nori = Nori.get(this)
        // Only a release build installs a release over itself: a debug build is the same app signed and
        // built differently, and the perf build another app, so both only say a newer version is out.
        nori.updates.configure(BuildConfig.VERSION_NAME, installs = BuildConfig.BUILD_TYPE == "release")
        // Then the AutoEQ list, if the core says it is due (one request on Wi-Fi, once a month at most), and
        // whether there is a newer release (once a day at most, unless switched off).
        // The app's log is kept on the disk for a report of a problem (Settings > About), from here on.
        Thread { dev.nori.music.Report.start(this); nori.warmUp(); forgetCoil(); nori.updates.checkIfDue(); kotlinx.coroutines.runBlocking { nori.keepAutoEqList() } }.start()
    }

    /** A new locale changes the words, read again from the resources (fractions follow it by themselves: "12,4 MB"). */
    override fun onConfigurationChanged(newConfig: android.content.res.Configuration) {
        super.onConfigurationChanged(newConfig)
        dev.nori.music.app.ui.Say.use(resources)
        dev.nori.music.net.Failures.use(resources)
    }

    /**
     * Covers were kept by Coil, in its own format, before the core fetched and kept them
     * (dev.nori.music.data.CoverLoader, in a directory of its own): nothing reads that directory now, so
     * it goes, once.
     */
    private fun forgetCoil() {
        val old = cacheDir.resolve("covers")
        if (old.exists()) old.deleteRecursively()
    }
}
