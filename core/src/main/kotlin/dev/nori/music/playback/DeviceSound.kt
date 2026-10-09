package dev.nori.music.playback

import dev.nori.music.ffi.model.AutoEqEntry
import dev.nori.music.ffi.devices.ChoiceKind
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.Core
import dev.nori.music.ffi.devices.CurveNotice
import dev.nori.music.ffi.devices.DeviceCurve
import dev.nori.music.ffi.devices.DeviceEffect
import dev.nori.music.ffi.model.SoundProfile
import dev.nori.music.settings.Settings
import dev.nori.music.settings.withSound
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * Which sound each output device gets. The decisions are nori-player's (crates/player/src/device.rs) and
 * the steps the core's (crates/devices/src/profiles.rs, crates/core/src/profiles.rs): each step is one
 * call that reads the settings, looks up, fetches, saves and binds profiles, keeps the sound from before
 * a device took over and the devices never to be offered a curve, and answers with a [DeviceEffect]
 * (and, for an arrival's AutoEQ curve, a [CurveNotice]). This applies the effects and raises the notices.
 *
 * Driven by [Outputs.current] from the playback service, so it works with the app's screens closed.
 * It adds no listener of its own: it runs once per device change, never while music plays.
 */
class DeviceSound(private val settings: Settings, private val core: () -> Core, private val client: () -> Client, private val metered: () -> Boolean) {
    private val lock = Mutex()

    /** Something to tell the user about the device that just connected. */
    private val _notice = MutableStateFlow<CurveNotice?>(null)
    val notice: StateFlow<CurveNotice?> = _notice
    fun consume(n: CurveNotice) { _notice.compareAndSet(n, null) }
    /** The last notice raised, seen or not: the test bridge answers it after the snackbar is gone. */
    @Volatile var lastNotice: CurveNotice? = null
        private set

    private val _profiles = MutableStateFlow<List<SoundProfile>>(emptyList())
    /** The saved profiles with the devices each is bound to. Filled by [refresh]. */
    val profiles: StateFlow<List<SoundProfile>> = _profiles

    private val _quiet = MutableStateFlow<List<String>>(emptyList())
    /** Devices the user said should never be offered a curve. Filled by [refresh]. */
    val quiet: StateFlow<List<String>> = _quiet

    /** Reads the profiles and the quiet devices again; every step that changes either asks for this. */
    suspend fun refresh() {
        val c = io { core() }
        _profiles.value = io { runCatching { c.profiles() }.getOrDefault(emptyList()) }
        _quiet.value = io { c.deviceQuiet() }
    }

    /** The output that music now goes to. */
    suspend fun onOutput(output: String): Unit = lock.withLock { arrive(output) }

    private suspend fun arrive(output: String) {
        // A notice is about the device that just arrived; one left unseen for an earlier device is stale.
        _notice.value = null
        perform(output, io { core().deviceArrive(output) })
        curve(output, io { client().deviceCurve(output, metered()) })
    }

    /** Performs an AutoEQ step's effect, then raises its notice. */
    private suspend fun curve(output: String, c: DeviceCurve) {
        perform(output, c.effect)
        c.notice?.let { lastNotice = it; _notice.value = it }
    }

    /** The AutoEQ curves this output's own name points at, best first. Empty for the speaker, a nameless DAC, or no index. */
    suspend fun curvesFor(output: String): List<AutoEqEntry> = io { runCatching { core().autoeqForOutput(output, 5u) }.getOrDefault(emptyList()) }

    /** Yes to an offer (`Client::device_accept`); throws, saying why, when the curve could not be fetched. */
    suspend fun accept(offer: CurveNotice.Offer): Unit = lock.withLock {
        curve(offer.output, io { client().deviceAccept(offer.output, offer.entry) })
    }

    /** Undoes a curve applied without asking: the sound from before, nothing bound, and this device is not offered a curve again. */
    suspend fun undo(n: CurveNotice.Applied): Unit = lock.withLock {
        perform(n.output, io { core().deviceUndo(n.output, n.curve, n.created, n.before) })
    }

    /** What the user picked for a device in the equalizer's device list. */
    sealed interface Choice {
        /** Nothing chosen: a matching AutoEQ curve is offered (or applied, with the setting on). */
        data object Automatic : Choice
        /** Nothing chosen and nothing offered. */
        data object Quiet : Choice
        /** The equalizer off on this device, everything else as it is now. */
        data object Flat : Choice
        /** No processing on this device: no equalizer and no effects, so offload can play it. */
        data object Bypass : Choice
        data class Profile(val name: String) : Choice
        data class Curve(val entry: AutoEqEntry) : Choice
    }

    /** Gives [output] its own sound; if it is the device playing now, that sound is loaded straight away. */
    suspend fun assign(output: String, choice: Choice, current: String): Unit = lock.withLock {
        val live = output == current
        val (kind, name) = when (choice) {
            Choice.Automatic -> ChoiceKind.AUTOMATIC to ""
            Choice.Quiet -> ChoiceKind.QUIET to ""
            Choice.Flat -> ChoiceKind.FLAT to ""
            Choice.Bypass -> ChoiceKind.BYPASS to ""
            is Choice.Profile -> ChoiceKind.PROFILE to choice.name
            // Its parametric preset, or its graphic curve fitted by the core, saved as a profile named after
            // it and bound to this device alone.
            is Choice.Curve -> {
                val text = io { client().autoeqCurve(choice.entry) } ?: throw NoCurve(choice.entry)
                perform(output, io { core().deviceAdopt(output, choice.entry.name, text, live) })
                return@withLock
            }
        }
        perform(output, io { core().deviceAssign(output, kind, name, live) })
    }

    /** AutoEQ has no curve for this entry; the core has taken it out of the list. */
    class NoCurve(val entry: AutoEqEntry) : Exception(entry.name)

    /** Does what the core said, in its order; see [DeviceEffect]. */
    private suspend fun perform(output: String, e: DeviceEffect) {
        if (e.refresh) refresh()
        e.apply?.let { s -> settings.update { it.withSound(s) } }
        if (e.arrive) arrive(output)
    }

    /** A device the list no longer needs to show: its binding and its "never ask" go with it. */
    suspend fun forget(output: String): Unit = lock.withLock {
        perform(output, io { core().deviceForget(output) })
    }

    private suspend fun <T> io(block: suspend () -> T): T = withContext(Dispatchers.IO) { block() }

    companion object {
        /** The profile nori_player::device::FLAT names, read once. */
        val FLAT: String by lazy { dev.nori.music.ffi.devices.deviceFlat() }
    }
}
