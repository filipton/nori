package dev.nori.music.playback

import android.content.Context
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Handler
import android.os.Looper
import dev.nori.music.ffi.devices.outputsForget
import dev.nori.music.ffi.devices.outputsKnown
import dev.nori.music.ffi.devices.outputsRefresh
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * Which output the music is going to, as a stable key a sound profile can be bound to: the speaker,
 * wired headphones, each Bluetooth device by name, each USB DAC by name. The naming, the ranking and
 * the list are nori-player's (crates/player/src/outputs.rs), and the core keeps the list in the app's
 * database with the settings; this lists what the audio system has. The callback only fires when something is plugged in or paired, so this costs
 * nothing while music plays.
 */
class Outputs(context: Context, private val settings: dev.nori.music.ffi.settings.Settings) {
    private val audio = context.getSystemService(AudioManager::class.java)
    private val _current = MutableStateFlow(SPEAKER)
    val current: StateFlow<String> = _current
    /**
     * Every output ever seen, so a device can be given its own sound while it is unplugged. Kept across
     * restarts by the core; read on first use, not at startup.
     */
    private val _known by lazy { MutableStateFlow(outputsKnown(settings)) }
    val known: StateFlow<List<String>> get() = _known

    /** Drops a device from [known]; it comes back by itself the next time it is connected. */
    fun forget(output: String) {
        outputsForget(settings, _known.value, _current.value, output)?.let { _known.value = it }
    }

    /**
     * A USB audio device is attached. Audio offload targets the phone's own DSP: with the stream handed
     * to the chip, a track routed to USB opens without complaint and then plays nothing, which is the
     * "silent DAC" this flag exists to prevent. [PlaybackService] decodes on the CPU while it is true.
     */
    private val _usb = MutableStateFlow(false)
    val usb: StateFlow<Boolean> = _usb

    private val callback = object : AudioDeviceCallback() {
        override fun onAudioDevicesAdded(added: Array<out AudioDeviceInfo>) = refresh()
        override fun onAudioDevicesRemoved(removed: Array<out AudioDeviceInfo>) = refresh()
    }

    fun start() {
        // registerAudioDeviceCallback reports every device already attached, so this also fills the
        // initial state: a DAC plugged in before the service started would otherwise go unnoticed.
        audio.registerAudioDeviceCallback(callback, Handler(Looper.getMainLooper()))
        // That report arrives a moment later. Until then [current] would say "speaker" with a DAC plugged
        // in, and whatever follows the output would switch the sound away and straight back.
        refresh()
    }

    /**
     * Pretend a USB device of this name is attached, so everything that hangs off it - offload standing
     * down, the player's output button, a sound profile bound to that output - can be checked without
     * the hardware. Null hands it back to the audio system.
     */
    fun testUsb(name: String?) {
        override = name
        refresh()
    }

    private var override: String? = null

    fun stop() = audio.unregisterAudioDeviceCallback(callback)

    /** Hands the core every output the audio system lists (type and product name) and keeps what it says. */
    private fun refresh() {
        val devices = audio.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
        val seen = outputsRefresh(settings, devices.map { it.type }, devices.map { it.productName?.toString().orEmpty() }, _known.value, override)
        _current.value = seen.current
        seen.known?.let { _known.value = it }
        _usb.value = seen.usb
    }

    /** The name nori_player::outputs::SPEAKER gives the phone's own speaker, read once. */
    companion object { val SPEAKER: String by lazy { dev.nori.music.ffi.devices.outputsSpeaker() } }
}
