package dev.nori.music

import android.content.Context
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Build
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter

/**
 * What a report of a problem says, for someone to paste into an issue: the app and the phone, the time now
 * (the log's lines carry theirs, so "it stuttered ten minutes ago" can be found), what was playing and to
 * which output, the settings changed from their defaults, and the app's log as kept on the disk for the last
 * day or so (nori-model's alog, [start]). Tooling, so in English. Never the server's address, the user or a
 * typed key: the settings leave text out, and the log says none of them.
 */
object Report {
    /** From now on the app's log is kept on the disk too, under the app's own files. Off the main thread. */
    fun start(context: Context) {
        val offset = ZonedDateTime.now().offset.totalSeconds / 60
        runCatching { dev.nori.music.ffi.model.alogPersist(context.filesDir.resolve("logs").path, offset) }
        // A crash is said in the log too, with where it happened, before the platform's handler ends the process.
        val platform = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, e ->
            runCatching { NoriLog.w("crashed on ${thread.name}: ${e.stackTraceToString()}") }
            platform?.uncaughtException(thread, e)
        }
    }

    /**
     * The whole report, under [header] (the client's own facts of its build). [positionMs] is read by the
     * caller on the main thread, where the player answers. Reads files and asks the core: off the main thread.
     */
    fun write(context: Context, header: String, positionMs: Long): String {
        val nori = Nori.get(context)
        val now = ZonedDateTime.now()
        val p = nori.player.state.value
        val song = p.queue.getOrNull(p.index)
        return buildString {
            appendLine(header.trimEnd())
            appendLine("${Build.MANUFACTURER} ${Build.MODEL}, Android ${Build.VERSION.RELEASE} (SDK ${Build.VERSION.SDK_INT})")
            appendLine("now ${now.format(DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm:ss"))} ${now.zone} (UTC${now.offset})")
            appendLine()
            appendLine("## Playing")
            appendLine(
                if (song == null) "nothing" else
                    "${if (p.playing) "playing" else "paused"} ${song.id} \"${song.title}\" - ${song.artist}, ${song.suffix} ${song.bitRate} kbps " +
                        "${song.samplingRate} Hz ${song.bitDepth} bit, at ${positionMs / 1000} s of ${song.duration} s" +
                        (if (p.buffering) ", buffering" else "") + (if (p.bridging) ", from downloads" else "") + (p.error?.let { ", error: $it" } ?: ""),
            )
            appendLine("queue ${p.queue.size} songs, at ${p.index + 1}, shuffle ${p.shuffle}, repeat ${p.repeat}")
            appendLine("outputs: ${outputs(context)}")
            appendLine()
            appendLine("## Settings changed")
            append(runCatching { nori.settings.core.settingsChanged() }.getOrDefault("(could not be read)\n").ifEmpty { "none\n" })
            appendLine()
            appendLine("## Log")
            append(runCatching { dev.nori.music.ffi.model.alogJournal() }.getOrDefault("").ifEmpty { "(empty)\n" })
        }
    }

    /** The outputs the phone has now, by kind and name: whether it played over Bluetooth, a DAC, the speaker. */
    private fun outputs(context: Context): String {
        val am = context.getSystemService(AudioManager::class.java) ?: return "?"
        return am.getDevices(AudioManager.GET_DEVICES_OUTPUTS).filter { it.type != AudioDeviceInfo.TYPE_BUILTIN_EARPIECE && it.type != AudioDeviceInfo.TYPE_TELEPHONY }
            .joinToString { d ->
                val kind = when (d.type) {
                    AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> "speaker"
                    AudioDeviceInfo.TYPE_BLUETOOTH_A2DP -> "bluetooth"
                    AudioDeviceInfo.TYPE_BLUETOOTH_SCO -> "bluetooth call"
                    AudioDeviceInfo.TYPE_WIRED_HEADPHONES, AudioDeviceInfo.TYPE_WIRED_HEADSET -> "wired"
                    AudioDeviceInfo.TYPE_USB_DEVICE, AudioDeviceInfo.TYPE_USB_HEADSET -> "usb"
                    else -> "type ${d.type}"
                }
                "$kind ${d.productName}"
            }
    }
}
