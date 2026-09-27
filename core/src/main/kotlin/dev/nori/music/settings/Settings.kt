package dev.nori.music.settings

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import dev.nori.music.ffi.model.EqKind
import dev.nori.music.ffi.settings.BandChannel
import dev.nori.music.ffi.settings.EqLevel
import dev.nori.music.ffi.settings.SavedQuality
import dev.nori.music.ffi.settings.SavedServer
import dev.nori.music.ffi.settings.SettingChange
import dev.nori.music.ffi.settings.SoundTool
import dev.nori.music.ffi.settings.ServerList
import dev.nori.music.ffi.settings.SoundBand
import dev.nori.music.ffi.settings.SoundSettings
import dev.nori.music.ffi.settings.StoredPrefs
import dev.nori.music.ffi.settings.serverLabel
import dev.nori.music.ffi.db.dbFileName
import dev.nori.music.ffi.settings.prefsSound
import dev.nori.music.ffi.settings.prefsWithSound
import dev.nori.music.ffi.settings.settingsOpen
import dev.nori.music.ffi.settings.settingsPut
import dev.nori.music.ffi.settings.settingsSoundTool
import dev.nori.music.ffi.settings.eqModelGet
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update

// The settings are the core's `StoredPrefs` (settings.rs), its enums included: what each setting is,
// its default and its range are said there once. Here are only a few conveniences over them.

/** Whether each band kind has a gain and a slope, and the equalizer's ranges: the core's (`settings::eq_model`), asked once. */
val EQ by lazy { eqModelGet() }

/** Whether the band kind has a gain to set (the core's `EqModel`, in `EqKind`'s order). */
val EqKind.usesGain: Boolean get() = EQ.bandKinds[ordinal].usesGain

/** Whether the band kind's width is a slope rather than a Q. */
val EqKind.slope: Boolean get() = EQ.bandKinds[ordinal].slope

/** A new server profile: only its id, everything else empty. */
fun newServer(id: String) = SavedServer(
    id = id, name = "", url = "", altUrl = "", user = "", password = "", apiKey = "", legacyAuth = false, headers = emptyMap(),
    allowSelfSigned = false, clientCert = "", clientCertPassword = "", wifiOnly = false, musicFolderId = "", altMaxBitRate = 0,
)

/** Its name, or else the host of its address (the core's `settings::label`). */
val SavedServer.label: String get() = serverLabel(name, url)

/** The server in use. */
val StoredPrefs.server: SavedServer? get() = servers.firstOrNull { it.id == activeServerId }
val StoredPrefs.loggedIn get() = server != null
val StoredPrefs.serverUrl get() = server?.url.orEmpty()
val StoredPrefs.user get() = server?.user.orEmpty()

private object Preamp {
    var of: StoredPrefs? = null
    var db = 0f
}

/**
 * The pre-amp in effect: the one set, or the automatic one (the core's `SoundSettings::effective_preamp_db`),
 * worked out once per settings record. Over plain JNI: a band's drag makes new settings on every step, and
 * the screen shows this for each.
 */
val StoredPrefs.effectivePreampDb: Float
    get() = synchronized(Preamp) {
        if (Preamp.of !== this) {
            // The graphic equalizer's sliders are peaks (kind 0) for the automatic pre-amp.
            val graphic = eqMode == dev.nori.music.ffi.settings.EqMode.GRAPHIC
            Preamp.db = dev.nori.music.playback.Dsp.effectivePreampDb(
                eqEnabled, eqPreampDb ?: 0f, eqPreampDb == null,
                if (graphic) IntArray(eqGraphic.size) else IntArray(eqBands.size) { eqBands[it].kind.ordinal },
                if (graphic) eqGraphic.toFloatArray() else FloatArray(eqBands.size) { eqBands[it].gainDb },
            )
            Preamp.of = this
        }
        Preamp.db
    }

/** The part of the settings a sound profile remembers; its JSON is read and written by the core (`settings.rs`). */
typealias Sound = SoundSettings

fun StoredPrefs.sound(): Sound = prefsSound(this)

fun StoredPrefs.withSound(s: Sound): StoredPrefs = prefsWithSound(this, s)

/** The saved servers and the one in use, as the core edits them (`settings::servers_*`). */
fun StoredPrefs.serverList() = ServerList(servers, activeServerId)

fun StoredPrefs.withServers(list: ServerList) = copy(servers = list.servers, activeServerId = list.activeServerId)

/**
 * The settings are the core's (`settings_store.rs`): it reads them once, keeps them and writes them to
 * the app's database whenever they change. The playback service needs them synchronously on start,
 * and this is a few rows read once.
 */
class Settings(private val context: Context) {
    private val state = MutableStateFlow(load())
    val prefs: StateFlow<StoredPrefs> = state
    val value get() = state.value

    /**
     * What each change asks of the player, as the core says (settings_store.rs: APPLY_AUDIO 1,
     * APPLY_GAIN 2, REPLAN 4, SOUND 8); nothing for a change only screens care about.
     */
    private val _effects = kotlinx.coroutines.flow.MutableSharedFlow<Int>(extraBufferCapacity = 16)
    val effects: kotlinx.coroutines.flow.SharedFlow<Int> = _effects

    fun update(change: (StoredPrefs) -> StoredPrefs) = put(change(state.value))

    /** Where a band crosses to the core and back; one for the settings, taken in turn. */
    private val band = FloatArray(5)

    /**
     * One equalizer band changed, on every step of a slider: edited in the core where the settings are
     * kept, which holds it in range, and only that band replaced here. Building and comparing the whole
     * settings record for each step was most of what a drag cost.
     */
    fun setBand(index: Int, b: SoundBand) {
        val effect: Int
        val kept: SoundBand
        synchronized(band) {
            band[0] = b.kind.ordinal.toFloat(); band[1] = b.freq; band[2] = b.gainDb; band[3] = b.q; band[4] = b.channel.ordinal.toFloat()
            effect = SoundEdit.setBand(index, band)
            if (effect < 0) return
            kept = SoundBand(EqKind.entries[band[0].toInt()], band[1], band[2], band[3], BandChannel.entries[band[4].toInt()])
        }
        state.update { p -> if (index in p.eqBands.indices) p.copy(eqBands = p.eqBands.toMutableList().also { it[index] = kept }) else p }
        if (effect != 0) _effects.tryEmit(effect)
    }

    /** Pre-amp, balance, limiter ceiling or crossfeed moved; edited in the core like a band, which holds and snaps it. */
    fun setLevel(level: EqLevel, value: Float) {
        val r = SoundEdit.setLevel(level.ordinal, value)
        if (r == -1L) return
        val kept = java.lang.Float.intBitsToFloat((r ushr 32).toInt())
        state.update { p ->
            when (level) {
                EqLevel.PREAMP -> p.copy(eqPreampDb = kept)
                EqLevel.BALANCE -> p.copy(balance = kept)
                EqLevel.LIMITER -> p.copy(limiterThresholdDb = kept)
                EqLevel.CROSSFEED -> p.copy(crossfeedDb = kept)
                EqLevel.REPLAY_GAIN_PREAMP -> p.copy(preampDb = kept)
                EqLevel.BASS_BOOST -> p.copy(bassBoostDb = kept)
                EqLevel.VIRTUALIZER -> p.copy(virtualizer = kept)
                EqLevel.VOLUME_BOOST -> p.copy(volumeBoostDb = kept)
                EqLevel.COMP_THRESHOLD -> p.copy(compThresholdDb = kept)
                EqLevel.COMP_RATIO -> p.copy(compRatio = kept)
                EqLevel.COMP_ATTACK -> p.copy(compAttackMs = kept)
                EqLevel.COMP_RELEASE -> p.copy(compReleaseMs = kept)
                EqLevel.COMP_MAKEUP -> p.copy(compMakeupDb = kept)
                EqLevel.COMP_KNEE -> p.copy(compKneeDb = kept)
                EqLevel.CROSSFEED_CUT -> p.copy(crossfeedHz = kept)
            }
        }
        val effect = r.toInt()
        if (effect != 0) _effects.tryEmit(effect)
    }

    /** One graphic equalizer slider moved; edited in the core like a band, which holds it in range. */
    fun setGraphic(index: Int, value: Float) {
        val r = SoundEdit.setGraphic(index, value)
        if (r == -1L) return
        val kept = java.lang.Float.intBitsToFloat((r ushr 32).toInt())
        // Moved by hand, the sliders are no longer a headphone correction (the core forgets it too).
        state.update { p -> if (index in p.eqGraphic.indices) p.copy(eqGraphic = p.eqGraphic.toMutableList().also { it[index] = kept }, eqGraphicTarget = emptyList()) else p }
        val effect = r.toInt()
        if (effect != 0) _effects.tryEmit(effect)
    }

    /**
     * A change by name the core has already kept (`setting_set`): taken in here, and nothing sent back.
     * A settings row's every step used to send the whole record back to be compared and kept again.
     */
    fun took(change: SettingChange) {
        state.value = change.prefs
        if (change.effect != 0u) _effects.tryEmit(change.effect.toInt())
    }

    /**
     * One of the equalizer screen's tools, used where the core keeps the settings; only the sound part
     * comes back. Returns how many bands there are now. Throws, saying why, for an import with no filters.
     */
    fun soundTool(tool: SoundTool): Int {
        val c = settingsSoundTool(tool) ?: return state.value.eqBands.size
        state.update { it.withSound(c.sound) }
        if (c.effect != 0u) _effects.tryEmit(c.effect.toInt())
        return c.sound.eqBands.size
    }

    /** Settings changed here or worked out by the core (a device's sound, a server's list), kept as they are. */
    fun put(next: StoredPrefs) {
        if (next == state.value) return
        // The core first: whatever reacts to the new value (on any thread) reads it from there.
        val effect = settingsPut(next)
        state.value = next
        if (effect != 0u) _effects.tryEmit(effect.toInt())
    }

    private fun load(): StoredPrefs = settingsOpen(java.io.File(context.filesDir, dbFileName()).path)
}
