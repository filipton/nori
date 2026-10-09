package dev.nori.music.app.vm

import android.app.Application
import androidx.lifecycle.viewModelScope
import dev.nori.music.ffi.JamView
import dev.nori.music.ffi.RelaySupport
import dev.nori.music.ffi.RemoteDevice
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
 * The devices sheet and the jam (its header in the queue, the invite, the people, a guest's requests) over the
 * core's remote control (Remotes). Everything is the core's: the jam is read again whenever the core says
 * something changed, and the devices only while a sheet is watching.
 */
class RemoteViewModel(app: Application) : NoriViewModel(app) {
    private val remotes = nori.remotes

    private val _devices = MutableStateFlow<List<RemoteDevice>>(emptyList())
    /** The account's other devices with their states, nearby ones first. */
    val devices: StateFlow<List<RemoteDevice>> = _devices.asStateFlow()

    private val _names = MutableStateFlow<List<String>>(emptyList())
    /** [devices]' names as the picker lists them, told apart where two share one (the core's `device_names`). */
    val names: StateFlow<List<String>> = _names.asStateFlow()

    private val me = dev.nori.music.ffi.RemoteMe(dev.nori.music.remote.Remotes.deviceName(app), dev.nori.music.ffi.remote.DeviceKind.PHONE)
    private val kinds = app.resources.let { r ->
        dev.nori.music.ffi.KindWords(
            r.getString(dev.nori.music.app.R.string.devices_kind_phone), r.getString(dev.nori.music.app.R.string.devices_kind_desktop),
            r.getString(dev.nori.music.app.R.string.devices_kind_terminal), r.getString(dev.nori.music.app.R.string.devices_kind_guest),
        )
    }

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

    /** The songs this guest asked for that the host has not decided on yet, by id. */
    val asked: StateFlow<Set<String>> = remotes.jam.map { j -> j?.takeIf { !it.hosting }?.let { v -> v.pending.filter { it.from == v.you }.map { it.song.id }.toSet() }.orEmpty() }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptySet())

    private val _jamFailed = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    /** A jam asked for here did not start. */
    val jamFailed: SharedFlow<Unit> = _jamFailed

    private var watchers = 0

    init {
        viewModelScope.launch { remotes.changes.collect { if (watchers > 0) refresh() } }
    }

    private fun refresh() = remotes.ask({ r -> r.devices().let { d -> d to dev.nori.music.ffi.deviceNames(d, me, kinds) } }) { (d, n) ->
        _devices.value = d
        _names.value = n
    }

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

    /** Plays the jam this phone is a guest in here, in step with its host, or only shows it. */
    fun listen(on: Boolean) = remotes.listen(on)

    /** Lets the hosted jam's guests listen along, or not. */
    fun jamAlong(on: Boolean) = remotes.jamAlong(on)

    /** A jam op from here: the host's own, or a guest's sent to the host. */
    fun jamAct(op: Op) = remotes.ask({ it.jamAct(op) })

    fun accept(request: ULong, yes: Boolean) = jamAct(Op.Decide(request, yes))

    /** Makes member [id] an admin, or a guest again. */
    fun promote(id: String, admin: Boolean) = jamAct(Op.Promote(id, admin))

    /** Sends member [id] out of the jam. */
    fun remove(id: String) = jamAct(Op.Kick(id))

    fun leave() = viewModelScope.launch { nori.leaveJam() }

    /** The server's address when an invite to it works only on a home network (the core's `is_home_only`). */
    fun homeOnly(): String? = nori.settings.value.server?.url?.takeIf { dev.nori.music.ffi.remote.isHomeOnly(it) }

    /** The invite as a QR code, made off the main thread. */
    suspend fun qr(link: String): QrCode? = withContext(Dispatchers.Default) { dev.nori.music.ffi.remote.qrCode(link) }
}
