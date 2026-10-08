package dev.nori.music.app.vm

import android.app.Application
import androidx.lifecycle.viewModelScope
import dev.nori.music.ffi.JamView
import dev.nori.music.ffi.RelaySupport
import dev.nori.music.ffi.RemoteDevice
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.remote.Op
import dev.nori.music.ffi.remote.QrCode
import dev.nori.music.settings.server
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The devices sheet and the jam (its header in the queue, the invite, the people, a guest's app) over the
 * core's remote control (Remotes). Everything is the core's: the jam is read again whenever the core says
 * something changed, and the devices only while a sheet is watching.
 */
class RemoteViewModel(app: Application) : NoriViewModel(app) {
    private val remotes = nori.remotes

    private val _devices = MutableStateFlow<List<RemoteDevice>>(emptyList())
    /** The account's other devices with their states, nearby ones first. */
    val devices: StateFlow<List<RemoteDevice>> = _devices.asStateFlow()

    /** The jam this phone hosts or is a guest in. */
    val jam: StateFlow<JamView?> = remotes.jam

    /** The device playing while it is not this phone (Remotes.mirror). */
    val mirror: StateFlow<dev.nori.music.ffi.Mirror?> = remotes.mirror

    /** Whether the server relays: jams and devices elsewhere only then. */
    val relay: StateFlow<RelaySupport> = remotes.relay

    /**
     * Whether a jam can be started here: jams are on, the server is not known to lack the relay, and none
     * is hosted or joined. Song, album and playlist menus offer "Start a jam" only then.
     */
    val canStartJam: StateFlow<Boolean> = combine(nori.settings.prefs.map { it.jam }, remotes.relay, remotes.jam) { on, relay, jam ->
        on && relay != RelaySupport.UNSUPPORTED && jam == null
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), false)

    private val _found = MutableStateFlow<List<Song>>(emptyList())
    /** A jam guest's search results. */
    val found: StateFlow<List<Song>> = _found.asStateFlow()

    private val _jamFailed = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    /** A jam asked for here did not start. */
    val jamFailed: SharedFlow<Unit> = _jamFailed

    private var watchers = 0

    init {
        viewModelScope.launch { remotes.changes.collect { if (watchers > 0) refresh() } }
    }

    private fun refresh() = remotes.ask({ it.devices() }) { _devices.value = it }

    /** A sheet that shows devices is on screen (true) or gone (false): other devices are followed while it is. */
    fun watch(on: Boolean) {
        watchers = (watchers + if (on) 1 else -1).coerceAtLeast(0)
        remotes.watch(watchers > 0)
        if (on) refresh()
    }

    override fun onCleared() {
        if (watchers > 0) remotes.watch(false)
    }

    /** Moves the playback to [device], or to this phone (null). */
    fun pick(device: String?) = remotes.pick(device)

    /** Starts hosting a jam around what plays; a failure comes out of [jamFailed]. */
    fun jamStart() = viewModelScope.launch {
        if (runCatching { remotes.jamOpen() }.isFailure) _jamFailed.tryEmit(Unit)
    }

    fun jamEnd() = remotes.ask({ it.jamClose() })

    /** A jam op from here: the host's own, or a guest's sent to the host. */
    fun jamAct(op: Op) = remotes.ask({ it.jamAct(op) })

    fun accept(request: ULong, yes: Boolean) = jamAct(Op.Decide(request, yes))

    /** Makes member [id] an admin, or a guest again. */
    fun promote(id: String, admin: Boolean) = jamAct(Op.Promote(id, admin))

    /** Sends member [id] out of the jam. */
    fun remove(id: String) = jamAct(Op.Kick(id))

    fun request(song: Song) = jamAct(Op.Request(song))

    fun leave() = viewModelScope.launch { nori.leaveJam() }

    /** A guest's search, through the host's server. */
    fun search(query: String) = viewModelScope.launch {
        if (query.isBlank()) { _found.value = emptyList(); return@launch }
        _found.value = runCatching { nori.library.search(query).songs }.getOrDefault(emptyList())
    }

    /** The server's address when an invite to it works only on a home network (the core's `is_home_only`). */
    fun homeOnly(): String? = nori.settings.value.server?.url?.takeIf { dev.nori.music.ffi.remote.isHomeOnly(it) }

    /** The invite as a QR code, made off the main thread. */
    suspend fun qr(link: String): QrCode? = withContext(Dispatchers.Default) { dev.nori.music.ffi.remote.qrCode(link) }
}
