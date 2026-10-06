package dev.nori.music.app.vm

import android.app.Application
import androidx.lifecycle.viewModelScope
import dev.nori.music.ffi.JamView
import dev.nori.music.ffi.RemoteDevice
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.remote.Op
import dev.nori.music.ffi.remote.QrCode
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The devices sheet and the jam screen over the core's remote control (Remotes). Everything is the core's;
 * this reads it again whenever the core says something changed, and only while a screen is watching.
 */
class RemoteViewModel(app: Application) : NoriViewModel(app) {
    private val remotes = nori.remotes

    private val _devices = MutableStateFlow<List<RemoteDevice>>(emptyList())
    /** The account's other devices with their states, nearby ones first. */
    val devices: StateFlow<List<RemoteDevice>> = _devices.asStateFlow()

    private val _jam = MutableStateFlow<JamView?>(null)
    /** The jam this phone hosts or is a guest in. */
    val jam: StateFlow<JamView?> = _jam.asStateFlow()

    private val _found = MutableStateFlow<List<Song>>(emptyList())
    /** A jam guest's search results. */
    val found: StateFlow<List<Song>> = _found.asStateFlow()

    /** Something that did not work, as a kind the screen words. */
    enum class Failure { JAM_START, JAM_JOIN }
    private val _failures = MutableSharedFlow<Failure>(extraBufferCapacity = 1)
    val failures: SharedFlow<Failure> = _failures

    private var watchers = 0

    init {
        viewModelScope.launch { remotes.changes.collect { if (watchers > 0) refresh() } }
    }

    private fun refresh() = remotes.ask({ it.devices() to it.jamView() }) { (d, j) ->
        _devices.value = d
        _jam.value = j
    }

    /** A sheet or screen that shows devices or the jam is on screen (true) or gone (false). */
    fun watch(on: Boolean) {
        watchers = (watchers + if (on) 1 else -1).coerceAtLeast(0)
        remotes.watch(watchers > 0)
        if (on) refresh()
    }

    override fun onCleared() {
        if (watchers > 0) remotes.watch(false)
    }

    fun send(device: String, op: Op) = remotes.ask({ it.send(device, op) })

    /** What [device] plays, played on here from where it is. */
    fun playHere(device: String) = remotes.ask({ it.send(device, Op.Transfer(it.id())) })

    /** What plays here, played on [device] from where it is. */
    fun playThere(device: String) = remotes.ask({ it.handOver(device) })

    fun jamStart() = viewModelScope.launch {
        if (runCatching { remotes.jamOpen() }.isFailure) _failures.tryEmit(Failure.JAM_START)
        refresh()
    }

    fun jamEnd() = remotes.ask({ it.jamClose() }) { refresh() }

    /** A jam op from here: the host's own, or a guest's sent to the host. */
    fun jamAct(op: Op) = remotes.ask({ it.jamAct(op) })

    fun request(song: Song) = jamAct(Op.Request(song))

    fun join(link: String) = viewModelScope.launch {
        if (runCatching { nori.joinJam(link) }.isFailure) _failures.tryEmit(Failure.JAM_JOIN)
    }

    fun leave() = viewModelScope.launch { nori.leaveJam() }

    /** A guest's search, through the host's server. */
    fun search(query: String) = viewModelScope.launch {
        if (query.isBlank()) { _found.value = emptyList(); return@launch }
        _found.value = runCatching { nori.library.search(query).songs }.getOrDefault(emptyList())
    }

    /** The invite as a QR code, made off the main thread. */
    suspend fun qr(link: String): QrCode? = withContext(Dispatchers.Default) { dev.nori.music.ffi.remote.qrCode(link) }
}
