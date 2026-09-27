package dev.nori.music.playback

import dev.nori.music.ffi.model.AutoEqEntry
import dev.nori.music.ffi.devices.ChoiceKind
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.Core
import dev.nori.music.ffi.model.CurveStep
import dev.nori.music.ffi.devices.DeviceEffect
import dev.nori.music.ffi.model.SoundProfile
import dev.nori.music.net.said
import dev.nori.music.settings.Settings
import dev.nori.music.settings.Sound
import dev.nori.music.settings.sound
import dev.nori.music.settings.withSound
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * Which sound each output device gets. The decisions are nori-player's (crates/player/src/device.rs) and
 * the steps the core's (crates/devices/src/profiles.rs): each step is one call that reads the settings,
 * looks up, saves and binds profiles, keeps the sound from before a device took over and the devices
 * never to be offered a curve, and answers with a [DeviceEffect]. This fetches AutoEQ presets, applies
 * the effects and raises the notices. A device can be given a saved profile, a flat sound, an AutoEQ
 * curve, or nothing; when it becomes the active output its sound is loaded, and when music goes back to
 * a device with nothing chosen the sound from before comes back. Headphones with nothing chosen and a
 * curve in the AutoEQ list get it offered, or applied straight away when [autoEqAuto] is on. When no
 * curve is found because the list is not on the device yet, it is fetched then if the core says it is
 * due (on Wi-Fi), so new headphones find their curve without a trip to the list first.
 *
 * Driven by [Outputs.current] from the playback service, so it works with the app's screens closed.
 * It adds no listener of its own: it runs once per device change, never while music plays.
 */
class DeviceSound(private val settings: Settings, private val core: () -> Core, private val client: () -> Client, private val metered: () -> Boolean) {
    private val lock = Mutex()

    /** Something to tell the user about the device that just connected. */
    sealed interface Notice { val output: String }
    /** Nothing is chosen for [output]; [entry] looks like it. Asking is the default. */
    data class Offer(override val output: String, val entry: AutoEqEntry) : Notice
    /** [curve] was applied and remembered for [output] without asking; [before] is what undo puts back. */
    data class Applied(override val output: String, val curve: String, val before: Sound, val created: Boolean) : Notice

    private val _notice = MutableStateFlow<Notice?>(null)
    val notice: StateFlow<Notice?> = _notice
    fun consume(n: Notice) { _notice.compareAndSet(n, null) }
    /** The last notice raised, seen or not: the test bridge answers it after the snackbar is gone. */
    @Volatile var lastNotice: Notice? = null
        private set
    private fun post(n: Notice) { lastNotice = n; _notice.value = n }

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
        val a = io { core().deviceArrive(output) }
        perform(output, a.effect)
        if (a.curve == CurveStep.NONE) return
        var entry = a.entry ?: listArrived(output) ?: return
        // An entry AutoEQ turns out to have no curve for is hidden by the core; the next best is tried.
        repeat(3) {
            if (a.curve == CurveStep.OFFER) {
                post(Offer(output, entry))
                return
            }
            val before = settings.value.sound()
            val created = try {
                adopt(output, entry, live = true)
            } catch (_: NoCurve) {
                entry = curvesFor(output, 1).firstOrNull() ?: return
                return@repeat
            } catch (e: Exception) {
                // No network, or GitHub not answering: asking later is better than silently doing nothing.
                dev.nori.music.NoriLog.w("autoeq for $output: ${e.said}")
                post(Offer(output, entry))
                return
            }
            post(Applied(output, entry.name, before, created))
            return
        }
    }

    /** No curve matched: the AutoEQ list is fetched if the core says it is due, then asked again. */
    private suspend fun listArrived(output: String): AutoEqEntry? {
        val fetched = io { runCatching { client().autoeqUpdate(false, metered()) }.getOrNull() } ?: return null
        return if (fetched == 0u) null else curvesFor(output, 1).firstOrNull()
    }

    /** The AutoEQ curves this output's own name points at, best first. Empty for the speaker, a nameless DAC, or no index. */
    suspend fun curvesFor(output: String, limit: Int = 5): List<AutoEqEntry> = io { runCatching { core().autoeqForOutput(output, limit.toUInt()) }.getOrDefault(emptyList()) }

    /** Yes to an [Offer]. When AutoEQ turns out to have no curve for it, the next best one is offered. */
    suspend fun accept(offer: Offer) {
        lock.withLock {
            try {
                adopt(offer.output, offer.entry, live = true)
            } catch (_: NoCurve) {
                curvesFor(offer.output, 1).firstOrNull()?.let { post(Offer(offer.output, it)) }
            }
        }
    }

    /** Undoes an [Applied]: the sound from before, nothing bound, and this device is not offered a curve again. */
    suspend fun undo(n: Applied): Unit = lock.withLock {
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
            is Choice.Curve -> {
                adopt(output, choice.entry, live)
                return@withLock
            }
        }
        perform(output, io { core().deviceAssign(output, kind, name, live) })
    }

    /** AutoEQ has no curve for this entry; the core has taken it out of the list. */
    class NoCurve(val entry: AutoEqEntry) : Exception(entry.name)

    /**
     * Fetches [entry]'s curve (its parametric preset, or its graphic curve fitted by the core) and has the
     * core save it as a profile named after it, bound to [output] alone, and load it when [live]. Returns
     * whether the profile is new. Throws [NoCurve] when AutoEQ has none, or why the request failed.
     */
    private suspend fun adopt(output: String, entry: AutoEqEntry, live: Boolean): Boolean {
        val c = io { core() }
        val text = io { client().autoeqCurve(entry) } ?: throw NoCurve(entry)
        val effect = io { c.deviceAdopt(output, entry.name, text, live) }
        perform(output, effect)
        return effect.created
    }

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
