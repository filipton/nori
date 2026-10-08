package dev.nori.music.remote

import android.content.Context
import android.database.ContentObserver
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import com.google.common.util.concurrent.ListenableFuture
import com.google.common.util.concurrent.SettableFuture
import dev.nori.music.Nori
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.JamView
import dev.nori.music.ffi.Mirror
import dev.nori.music.ffi.Playing
import dev.nori.music.ffi.RelaySupport
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

    private val _mirror = MutableStateFlow<Mirror?>(null)

    /**
     * The account's active device while it is another one (the core's `Remote::active`): the player, the
     * session and the notification show and control it instead of this phone. Null while this phone plays.
     */
    val mirror: StateFlow<Mirror?> = _mirror.asStateFlow()

    private val _jam = MutableStateFlow<JamView?>(null)

    /** The jam this phone hosts or is a guest in, as of the core's last change; null without one. */
    val jam: StateFlow<JamView?> = _jam.asStateFlow()

    private val _jamAdded = MutableStateFlow<Map<String, String>>(emptyMap())

    /** Who asked for each song of the hosted jam's queue, by song id (the core's `jam_added`). */
    val jamAdded: StateFlow<Map<String, String>> = _jamAdded.asStateFlow()

    private val _relay = MutableStateFlow(RelaySupport.UNKNOWN)

    /** Whether the server relays: jams and devices elsewhere only then. */
    val relay: StateFlow<RelaySupport> = _relay.asStateFlow()

    /** The playback service's player while it runs; ops go to it, starting the service when it is not. */
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
            main.post { service?.apply(op) ?: nori.player.connected { service?.apply(op) } }
        }
    }

    private val shown = object : RemoteShown {
        override fun changed() {
            _changes.update { it + 1 }
            work { mirrorNow(); jamNow() }
        }
    }

    /** Reads the mirrored device again (on the worker) and shows it; [then] once it is shown. */
    private fun mirrorNow(then: () -> Unit = {}) {
        val m = remote?.active()
        main.post { _mirror.value = m; then() }
    }

    /** Reads the jam and the relay again (on the worker) and shows them. */
    private fun jamNow() {
        val r = remote
        val j = r?.jamView()
        val added = if (j?.hosting == true) r.jamAdded() else emptyMap()
        val relay = r?.relay() ?: RelaySupport.UNKNOWN
        main.post { _jam.value = j; _jamAdded.value = added; _relay.value = relay }
    }

    /**
     * The volume keys while this phone is controllable: another device sees them move. Listened to only
     * while serving, through the system settings the volume steps are kept in.
     */
    private val volumeKeys = object : ContentObserver(main) {
        override fun onChange(selfChange: Boolean) {
            val r = remote ?: return
            val now = volumePercent(context)?.toUByte()
            // The core passes on only a volume that moved.
            work { r.volumeChanged(now) }
        }
    }
    private var keysWatched = false

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
        remote?.stop()
        remote = null
        client = null
        main.post { _mirror.value = null; _jam.value = null; _jamAdded.value = emptyMap(); _relay.value = RelaySupport.UNKNOWN }
    }

    /** Whether the playback service is up: the device is controllable then, while remote control is on. */
    fun serve(on: Boolean) {
        // Nothing built and nothing wanted: the worker thread is not started.
        if (remote == null && !wanted()) return synchronized(this) { serving = on }
        work {
            serving = on
            val serves = on && nori.settings.value.remoteControl
            current()?.serve(serves)
            main.post { watchKeys(serves) }
        }
    }

    private fun watchKeys(on: Boolean) {
        if (on == keysWatched) return
        keysWatched = on
        if (on) context.contentResolver.registerContentObserver(android.provider.Settings.System.CONTENT_URI, true, volumeKeys)
        else context.contentResolver.unregisterContentObserver(volumeKeys)
    }

    /** A device picker or jam screen is open: other devices are followed while it is. */
    fun watch(on: Boolean) = work {
        watching = on
        current()?.watch(on)
    }

    /** The player's state changed; nothing happens unless a remote exists. */
    fun played(playing: Boolean, buffering: Boolean, positionMs: Long, index: Int) {
        val r = remote ?: return
        val read = SystemClock.elapsedRealtimeNanos()
        work {
            // The place ran on while the worker was busy: the core takes it as of the call.
            val ran = if (playing) (SystemClock.elapsedRealtimeNanos() - read) / 1_000_000 else 0
            r.played(Playing(playing, buffering, positionMs + ran, index.takeIf { it >= 0 }?.toUInt(), volumePercent(context)?.toUByte()))
        }
    }

    /** Moves the playback to [device], or to this phone (null). */
    fun pick(device: String?) = work { current()?.pick(device) }

    /**
     * [op] for the device this phone mirrors. The answer is shown at once, as it is expected to come out
     * (the core's foresight), and the future completes once it is; the device's next state corrects it.
     */
    fun command(op: Op): ListenableFuture<*> {
        val done = SettableFuture.create<Unit>()
        worker.execute {
            runCatching {
                val r = remote
                val id = _mirror.value?.id
                if (r != null && id != null) r.send(id, op)
                mirrorNow { done.set(Unit) }
            }.onFailure { done.set(Unit) }
        }
        return done
    }

    /** A song's heart changed here: the mirrored device marks it too. */
    fun starred(id: String, on: Boolean) {
        if (_mirror.value != null) command(Op.Star(id, on))
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
