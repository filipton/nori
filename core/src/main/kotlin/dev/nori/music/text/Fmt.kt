package dev.nori.music.text

import dev.nori.music.ffi.model.bytes as coreBytes
import dev.nori.music.ffi.model.clock as coreClock
import dev.nori.music.ffi.model.fixed as coreFixed
import dev.nori.music.ffi.model.hz as coreHz
import dev.nori.music.ffi.model.isoBand as coreIsoBand
import dev.nori.music.ffi.model.khz as coreKhz
import dev.nori.music.ffi.model.kilohertz as coreKilohertz
import dev.nori.music.ffi.model.megabytes as coreMegabytes
import dev.nori.music.ffi.model.nudge as coreNudge
import dev.nori.music.ffi.model.signedDb as coreSignedDb
import dev.nori.music.ffi.model.speed as coreSpeed

/**
 * How the app writes numbers on screen: times, sizes, speeds, decibels and frequencies, as every client
 * does (crates/model/src/numbers.rs). The words around the numbers are string resources ([Words], the
 * app's `Say`); what is here is the phone's decimal separator ("12,4 MB" on a Polish phone) and a cache
 * of the clock's times.
 */
object Fmt {
    /** Times up to two hours are made once per second and kept; a longer mix is formatted as it goes. */
    const val CACHED_SECONDS = 7200

    private val made = arrayOfNulls<String>(CACHED_SECONDS)
    private val left = arrayOfNulls<String>(CACHED_SECONDS)

    /**
     * "3:07", or "1:02:03" from an hour. Each second's text is made once for the life of the process: the
     * seek bar asks for two of these every second a song plays, and every list row for its song's length,
     * so after the first time through they cost an array read and allocate nothing.
     */
    fun duration(seconds: Long): String {
        if (seconds < 0 || seconds >= CACHED_SECONDS) return clock(seconds, false)
        val i = seconds.toInt()
        return made[i] ?: clock(seconds, false).also { made[i] = it }
    }

    /** The time left in a song, under the seek bar: "-3:07". Kept the same way as [duration]. */
    fun durationLeft(seconds: Long): String {
        if (seconds < 0 || seconds >= CACHED_SECONDS) return clock(seconds, true)
        val i = seconds.toInt()
        return left[i] ?: clock(seconds, true).also { left[i] = it }
    }

    /** [duration], or with [minus] [durationLeft], made anew. */
    fun clock(seconds: Long, minus: Boolean): String = coreClock(seconds, minus)

    private fun point(): String = java.text.DecimalFormatSymbols.getInstance().decimalSeparator.toString()

    /** [v] with [places] decimals, halves rounded up, with a '+' in front of a non-negative number when [plus]. */
    fun fixed(v: Double, places: Int, plus: Boolean = false): String = coreFixed(v, places.toUInt(), plus, point())

    /** A decibel figure with its sign, one decimal: "+3.5", "-1.0", and "+0.0" for nothing at all. */
    fun signedDb(db: Float): String = coreSignedDb(db, point())

    /** How far the lyrics are nudged, "+0.5" (the unit is the caller's). */
    fun nudge(ms: Long): String = coreNudge(ms, point())

    /** A band's frequency as its label says it: "63", "1k", "2.5k", "12.5k". */
    fun hz(f: Float): String = coreHz(f, point())

    /** A graphic equalizer band's nominal frequency: "31.5", "63", "1k". */
    fun isoBand(f: Float): String = coreIsoBand(f, point())

    /** "850 B", "38 KB", "2.1 MB", "38 MB", "2.1 GB". */
    fun bytes(bytes: Long): String = coreBytes(bytes, point())

    /** "12.4 MB". */
    fun megabytes(bytes: Long): String = coreMegabytes(bytes, point())

    /** "850 KB/s", "3.2 MB/s"; empty when nothing is measurable. */
    fun speed(bps: Long): String = coreSpeed(bps, point())

    /** A sample rate as a DAC's mode is written, "44.1 kHz", in the phone's number style ("44,1 kHz"). */
    fun kiloHertz(rate: Int): String = coreKilohertz(rate, point())

    /** A sample rate in kHz as the records write it, "44.1", "96.0": never localised. */
    fun khz(rate: Int): String = coreKhz(rate)
}
