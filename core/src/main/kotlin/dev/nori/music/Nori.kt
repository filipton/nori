package dev.nori.music

import android.content.Context
import androidx.annotation.OptIn
import androidx.media3.common.util.UnstableApi
import dev.nori.music.data.Library
import dev.nori.music.downloads.Downloads
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.Core
import dev.nori.music.ffi.CoverNet
import dev.nori.music.ffi.net.NetProfile
import dev.nori.music.ffi.ServerConfig
import dev.nori.music.net.Http
import dev.nori.music.net.lifted
import dev.nori.music.playback.BitPerfect
import dev.nori.music.playback.Outputs
import dev.nori.music.playback.MediaSources
import dev.nori.music.playback.PlayerConnection
import dev.nori.music.ffi.settings.SavedServer
import dev.nori.music.settings.Settings
import dev.nori.music.settings.serverList
import dev.nori.music.settings.withServers
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import dev.nori.music.settings.server

/**
 * The object graph, built by hand: there are a dozen long-lived objects and a DI
 * framework would cost more at startup than it saves. The UI reaches everything
 * through here; nothing in this module knows a UI exists.
 */
@OptIn(UnstableApi::class)
class Nori private constructor(private val context: Context) {
    val settings = Settings(context)

    // Everything below is built on first use. The application warms it up from a background thread, so by the
    // time anything needs the core it is normally there; the UI thread itself only ever needs [settings] and
    // the cheap shells ([library], [downloads], [player]) to draw its first frame.
    private val lock = Any()
    /**
     * The active profile's core and the client over it, opened together. [key] is the active server id as
     * the settings hold it, [id] whose rows those are (the core's `server_db`: "default" before any).
     */
    private class Opened(val key: String, val id: String, val core: Core, val client: Client)
    @Volatile private var opened: Opened? = null
    private val lazyHttp = lazy { Http(context).also { it.configure(settings.value.server) } }
    private val lazySources = lazy { MediaSources(context, ::client, http, settings) }

    /**
     * What the core's cover loader ([dev.nori.music.data.CoverLoader]) fetches through, and keys covers by:
     * [transport] once it is built, so covers ride the API's connection, and each profile's addresses.
     */
    val coverNet by lazy { CoverNet() }

    /** The core's one door to the network; built with [http]. */
    private val transport by lazy { http.transport { library.onServerChanged() }.also { coverNet.setTransport(it) } }

    private fun active(): Opened {
        // Compared as the settings hold it, so the core is only asked whose rows those are on a switch.
        val key = settings.value.activeServerId
        opened?.takeIf { it.key == key }?.let { return it }
        return synchronized(lock) {
            opened?.takeIf { it.key == key } ?: settings.value.server.let { p ->
                val id = dev.nori.music.ffi.settings.serverDb(key)
                val core = open(id, p)
                Opened(key, id, core, Client(core, transport, coverNet).also { c -> p?.let { c.setProfile(it.net()) } })
            }.also { opened = it }
        }
    }

    /** The index of the active server profile; every profile has its own rows in the app's database. */
    val core: Core get() = active().core

    /** The Subsonic client of the active server profile: addresses, stored reads, offline writes. */
    val client: Client get() = active().client

    val http: Http by lazyHttp
    val sources: MediaSources by lazySources

    /**
     * Applies "Space for streamed music" now; the player otherwise picks it up at the next track.
     * Off the main thread: shrinking the cache touches the disk. Never builds the graph itself.
     */
    fun applyCacheLimit() {
        if (lazySources.isInitialized()) sources.setStreamLimitMb(settings.value.cacheMb)
    }

    val library = Library(::core, ::client)
    val downloads = Downloads(context, ::core, ::client, lazySources, settings)
    val dac = BitPerfect(context)
    val outputs = Outputs(context)
    /** A player for the moving cover; the screen's view model makes one when it first shows one. */
    fun motionPlayer(onGone: (String) -> Unit) = dev.nori.music.playback.MotionPlayer(context, http, sources, onGone)
    /** Each output device's own sound; built when the playback service first sees a device. */
    val deviceSound by lazy { dev.nori.music.playback.DeviceSound(settings, { core }, { client }, { http.metered }) }
    val player = PlayerConnection(context, this)
    /** The app's own updates from its GitHub releases; nothing is asked until the app starts it. */
    val updates = dev.nori.music.update.Updates(context, { http }, { client })

    /** True while requests go to the profile's second address; stream quality is capped then. */
    val onSecondAddress: Boolean get() = opened?.client?.onSecondAddress() ?: false

    private fun open(id: String, profile: SavedServer?): Core =
        Core(File(context.filesDir, dev.nori.music.ffi.db.dbFileName()).path, id).also { c -> profile?.let { c.configure(it.config()) } }

    private fun SavedServer.config() = ServerConfig(url, user, password, apiKey.ifEmpty { null }, legacyAuth)
    private fun SavedServer.net() = NetProfile(url, altUrl, musicFolderId, altMaxBitRate.coerceAtLeast(0).toUInt())

    /** Called off the main thread at process start. */
    fun warmUp() {
        val t = android.os.SystemClock.elapsedRealtime()
        core; http; sources
        dev.nori.music.NoriLog.i("core ready in ${android.os.SystemClock.elapsedRealtime() - t} ms")
    }

    /**
     * The AutoEQ headphone list kept on the device. The core decides whether it is due (switched on, an
     * unmetered network, missing or a month old) and does nothing otherwise, so this is called when the app
     * starts and when the network turns unmetered, never on a timer. How many headphones it now offers,
     * or null when nothing was fetched.
     */
    suspend fun keepAutoEqList(): UInt? = withContext(Dispatchers.IO) {
        runCatching { client.autoeqUpdate(false, http.metered) }
            .onFailure { dev.nori.music.NoriLog.w("autoeq list: ${it.message}") }
            .getOrNull()
    }

    /**
     * A profile with two addresses: the core asks the first one briefly and uses the second if it does not
     * answer. Runs when the app comes to the foreground and after a request failed, never on a timer.
     * Returns true when the address in use changed.
     */
    suspend fun chooseAddress(): Boolean = withContext(Dispatchers.IO) {
        if (settings.value.server == null) return@withContext false
        client.chooseAddress()
    }

    /**
     * Checks the profile against the server before keeping it; the core tries the second address and, for
     * servers without token auth (error 41), legacy auth, which is then remembered.
     */
    suspend fun login(form: SavedServer): SavedServer = withContext(Dispatchers.IO) {
        // A server and user already saved is that profile logged in to again, not a copy with an empty
        // library and no downloads (the core's `servers_login`).
        val draft = dev.nori.music.ffi.settings.serverForLogin(settings.value.serverList(), form)
        val old = settings.value.server
        val accepted = try {
            http.configure(draft)
            val legacy = lifted { dev.nori.music.ffi.loginCheck(transport, draft.config(), draft.altUrl) }
            if (legacy) draft.copy(legacyAuth = true) else draft
        } catch (e: Exception) {
            http.configure(old)
            throw e
        }
        activate(accepted)
        accepted
    }

    /** Makes [profile] the active server (adding or replacing it in the saved list). */
    fun activate(profile: SavedServer) {
        player.clear()
        // The old core is dropped, not closed: a request may still be using it, and the cleaner frees it.
        synchronized(lock) { opened = null }
        settings.update { p -> p.withServers(dev.nori.music.ffi.settings.serversActivated(p.serverList(), profile)) }
        http.configure(profile)
        library.onServerChanged()
        library.onProfileChanged()
    }

    /** Settings that do not need the server asked again: headers, Wi-Fi only, music folder, name. */
    fun updateServer(profile: SavedServer) {
        settings.update { p -> p.withServers(dev.nori.music.ffi.settings.serversUpdated(p.serverList(), profile)) }
        if (profile.id == settings.value.activeServerId) {
            http.configure(profile)
            opened?.takeIf { it.key == profile.id }?.client?.setProfile(profile.net())
            library.onServerChanged()
        }
    }

    fun removeServer(id: String) {
        val wasActive = settings.value.activeServerId == id
        if (wasActive) { player.clear(); synchronized(lock) { opened = null } }
        // Which one takes over when the one in use goes is the core's (`settings::servers_remove`).
        settings.update { p -> p.withServers(dev.nori.music.ffi.settings.serversRemoved(p.serverList(), id)) }
        // Its rows in the app's database; a whole library is a lot of rows, so not on this thread.
        val db = File(context.filesDir, dev.nori.music.ffi.db.dbFileName()).path
        Thread({ runCatching { dev.nori.music.ffi.db.dbForgetServer(db, id) } }, "nori-forget").start()
        if (wasActive) { http.configure(settings.value.server); library.onServerChanged() }
    }

    fun logout() = settings.value.server?.let { removeServer(it.id) }

    companion object {
        @Volatile private var instance: Nori? = null
        fun get(context: Context): Nori = instance ?: synchronized(this) { instance ?: Nori(context.applicationContext).also { instance = it } }
    }
}
