package dev.nori.music.remote

import android.content.Context
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import dev.nori.music.Nori
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.Playing
import dev.nori.music.ffi.Remote
import dev.nori.music.ffi.RemoteMe
import dev.nori.music.ffi.RemotePlayer
import dev.nori.music.ffi.RemoteShown
import dev.nori.music.ffi.remote.DeviceKind
import dev.nori.music.ffi.remote.Op
import dev.nori.music.ffi.remote.isGuestKey
import dev.nori.music.settings.server
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.util.concurrent.Executors

/**
 * The core's remote control and jams (nori-core remote.rs) for the profile in use. A [Remote] exists only
 * while remote control or jams are switched on, or the profile is a jam guest's; otherwise every call here
 * is a null check. The core decides everything; this hands it the player, the system's mDNS and a thread.
 */
class Remotes(private val context: Context, private val nori: Nori) {
    private val _changes = MutableStateFlow(0L)

    /** Bumped whenever the devices, their states or the jam changed; screens read them again. */
    val changes: StateFlow<Long> = _changes.asStateFlow()

    /** The playback service's player while it runs; ops go to it, or else through the app's controller. */
    @Volatile var service: RemotePlayer? = null

    private val main = Handler(Looper.getMainLooper())
    /** Calls into the core leave the main thread here, one at a time. */
    private val worker by lazy { Executors.newSingleThreadExecutor { Thread(it, "nori-remote-calls") } }
    private val discovery by lazy { NsdDiscovery(context) { remote } }
    @Volatile private var remote: Remote? = null
    private var client: Client? = null
    private var serving = false
    private var watching = false

    private val player = object : RemotePlayer {
        override fun apply(op: Op) {
            main.post { (service ?: connection).apply(op) }
        }
    }

    private val shown = object : RemoteShown {
        override fun changed() = _changes.update { it + 1 }
    }

    /** When the service is not running (a transfer here): through the app's controller, which starts it. */
    private val connection = object : RemotePlayer {
        override fun apply(op: Op) {
            val p = nori.player
            when (op) {
                is Op.Replace -> p.playAt(op.songs, op.index.toInt(), op.positionMs, op.play)
                is Op.Add -> if (op.next) p.playNext(op.songs) else p.enqueue(op.songs)
                is Op.Volume -> setVolume(context, op.percent.toInt())
                // Only a controllable device is sent the rest, and it is one only while its service runs.
                else -> {}
            }
        }
    }

    private fun work(f: () -> Unit) = worker.execute { runCatching(f).onFailure { dev.nori.music.NoriLog.w("remote: ${it.message}") } }

    /** The remote for the profile in use, built when asked for and switched on; null otherwise. Off the main thread. */
    private fun current(): Remote? = synchronized(this) {
        if (!wanted()) {
            drop()
            return null
        }
        val c = nori.client
        if (remote == null || client !== c) {
            drop()
            val p = nori.settings.value
            val kind = if (isGuest()) DeviceKind.GUEST else DeviceKind.PHONE
            remote = Remote(c, RemoteMe(deviceName(context), kind), player, shown, discovery)
            client = c
            if (serving) remote?.serve(p.remoteControl)
            if (watching) remote?.watch(true)
        }
        remote
    }

    private fun isGuest() = nori.settings.value.server?.apiKey?.let { isGuestKey(it) } == true

    /** Whether the profile in use asks for a remote at all. */
    private fun wanted(): Boolean {
        val p = nori.settings.value
        return p.server != null && (p.remoteControl || p.jam || isGuest())
    }

    private fun drop() {
        remote?.let { r -> r.serve(false); r.watch(false); r.jamClose() }
        remote = null
        client = null
    }

    /** Whether the playback service is up: the device is controllable then, while remote control is on. */
    fun serve(on: Boolean) {
        // Nothing built and nothing wanted: the worker thread is not started.
        if (remote == null && !wanted()) return synchronized(this) { serving = on }
        work {
            serving = on
            current()?.serve(on && nori.settings.value.remoteControl)
        }
    }

    /** A device picker or jam screen is open: other devices are followed while it is. */
    fun watch(on: Boolean) = work {
        watching = on
        current()?.watch(on)
    }

    /** The player's state changed; nothing happens unless a remote exists. */
    fun played(playing: Boolean, positionMs: Long, index: Int) {
        val r = remote ?: return
        work { r.played(Playing(playing, positionMs, index.takeIf { it >= 0 }?.toUInt(), volumePercent(context)?.toUByte())) }
    }

    /** Everything else a screen asks, on the worker; [then] gets the answer back on the main thread. */
    fun <T> ask(f: (Remote) -> T, then: (T) -> Unit = {}) = work {
        val r = current() ?: return@work
        val v = f(r)
        main.post { then(v) }
    }

    /** What the screens read: the remote if there is one, without building it. */
    fun peek(): Remote? = remote

    /** Opens a jam; its invite link. */
    suspend fun jamOpen(): String = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
        val r = synchronized(this@Remotes) { current() } ?: error("jams are off")
        dev.nori.music.net.lifted { r.jamOpen() }
    }

    /** Leaves the jam this guest profile is in. */
    suspend fun leave() = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
        runCatching { remote?.jamLeave() }
    }

    companion object {
        fun deviceName(context: Context): String =
            android.provider.Settings.Global.getString(context.contentResolver, android.provider.Settings.Global.DEVICE_NAME)
                ?.takeIf { it.isNotBlank() } ?: Build.MODEL

        private fun audio(context: Context) = context.getSystemService(AudioManager::class.java)

        fun volumePercent(context: Context): Int? = audio(context)?.let { a ->
            val max = a.getStreamMaxVolume(AudioManager.STREAM_MUSIC)
            if (max <= 0) null else a.getStreamVolume(AudioManager.STREAM_MUSIC) * 100 / max
        }

        fun setVolume(context: Context, percent: Int) {
            val a = audio(context) ?: return
            val max = a.getStreamMaxVolume(AudioManager.STREAM_MUSIC)
            a.setStreamVolume(AudioManager.STREAM_MUSIC, (percent.coerceIn(0, 100) * max + 50) / 100, 0)
        }
    }
}
