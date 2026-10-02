package dev.nori.music.look

import dalvik.annotation.optimization.CriticalNative
import dalvik.annotation.optimization.FastNative
import dev.nori.music.ffi.model.Lyrics

/**
 * How a page of lyrics moves with the song - which line is lit, how long its change takes, how far the
 * singing is into it, and when to look again - worked out in Rust (crates/look/src/lyrics.rs) so every
 * app built on it keeps the words in step the same way. See crates/lyrics/src/look.rs.
 *
 * Made once per set of lyrics, then asked with the playhead every frame it matters: one JNI call with
 * primitives in and one `Long` out, the fields of which are read with the functions in the companion.
 * [close] frees it; a closed clock answers "nothing lit, never ask again".
 */
class LyricsClock(lyrics: Lyrics, positionMs: Long) : AutoCloseable {
    /** The moment on screen and the backing vocals sung there, left by [at] (`LyricsShown` in crates/android/src/look.rs). */
    private val view = dev.nori.music.NativeView(16)
    private var h = dev.nori.music.ffi.lyrics.lyricsClock(lyrics, positionMs)

    /** Whether the lyrics carry per-word times, so the active line can fill in as it is sung. */
    val sweeps: Boolean = LyricsJni.sweeps(h)

    /**
     * Where the lyrics are with the player at [positionMs]; [lively] when the words rise and glow as they
     * are sung (not with movement reduced), which asks for every frame while they move; [force] draws
     * that moment whatever changed.
     */
    fun at(positionMs: Long, sweep: Boolean, lively: Boolean, force: Boolean): Long = LyricsJni.at(h, positionMs, sweep, lively, force, view.address)

    /** [shownMs] as the last [at] left it: read with no call. */
    val atShownMs: Long get() = view.buffer.getLong(0)

    /** How far into the lit line's backing vocals the singing is, in UTF-16 units, at the moment the last [at] showed. */
    val atBackingSung: Float get() = view.buffer.getFloat(8)

    /** What is on screen now, as a frame. */
    fun shown(): Long = LyricsJni.shown(h)

    /** The moment on screen, the nudge in it: what a word's rise and glow are drawn for. */
    fun shownMs(): Long = LyricsJni.shownMs(h)

    /** Shows [line] at once and returns where to seek the player to. */
    fun tap(line: Int): Long = LyricsJni.tap(h, line)

    /**
     * Lyrics taking the place of the ones on screen for the same song start on [line], the one standing
     * for the line that was lit, rather than going back a line while the song catches up with a new timing
     * (`LyricClock::land`). No seek.
     */
    fun land(line: Int) = LyricsJni.land(h, line)

    /** Sooner (> 0), later (< 0) or back to none (0); returns the nudge in ms. */
    fun nudge(dir: Int): Long = LyricsJni.nudge(h, dir)

    override fun close() { val was = h; h = 0; LyricsJni.destroy(was) }

    /** The fields of an answer, as `Step::pack` and `Frame::pack` lay them out. */
    companion object {
        private const val SUNG_BITS = 29
        private const val SUNG_ONE = (1 shl 18).toFloat()
        private const val ACTIVE_AT = SUNG_BITS
        private const val GLIDE_AT = ACTIVE_AT + 13
        private const val WAIT_AT = GLIDE_AT + 10
        private const val STILL_AT = WAIT_AT + 9
        private const val REDRAW_AT = STILL_AT + 1

        /** The part of an answer that is what to draw: store this, so equal frames are equal values. */
        fun frame(step: Long): Long = step and ((1L shl WAIT_AT) - 1)
        /** The line lit and scrolled to, or -1; the number of lines once the last one is over (every line sung). */
        fun active(frame: Long): Int = ((frame ushr ACTIVE_AT) and 0x1FFF).toInt() - 1
        /** How long the change into the active line takes, scroll and fades together. */
        fun glideMs(frame: Long): Int = ((frame ushr GLIDE_AT) and 0x3FF).toInt()
        /** How far into the active line the singing is, in UTF-16 units: 7.5 is half of the character at 7. */
        fun sung(frame: Long): Float = (frame and ((1L shl SUNG_BITS) - 1)).toFloat() / SUNG_ONE
        /** When to ask again: display frames while sweeping, ms otherwise or when [still]; 0 for never. */
        fun wait(step: Long): Int = ((step ushr WAIT_AT) and 0x1FF).toInt()
        /** Sweeping, but nothing changes for [wait] ms: sleep rather than count display frames. */
        fun still(step: Long): Boolean = (step ushr STILL_AT) and 1L == 1L
        /** Whether anything on screen changed. */
        fun redraw(step: Long): Boolean = (step ushr REDRAW_AT) and 1L == 1L

        /** How lit [line] is while [active] is sung (`nori_look::lyrics::line_strength`). Primitives only. */
        fun strength(synced: Boolean, line: Int, active: Int): Float = LyricsJni.strength(synced, line, active)

        /**
         * The line of the new lyrics ([next], each line's start) that stands for line [at] of the old ones
         * ([old]) when finer lyrics replace them (`nori_look::lyrics::matching_line`).
         */
        fun matchingLine(old: LongArray, at: Int, next: LongArray, timed: Boolean): Int = LyricsJni.matchingLine(old, at, next, timed)
    }
}

/** See crates/lyrics/src/look.rs. */
internal object LyricsJni {
    init { System.loadLibrary("norimusic") }

    @JvmStatic @CriticalNative external fun destroy(h: Long)
    @JvmStatic @CriticalNative external fun sweeps(h: Long): Boolean
    /** `Step::pack`: sung (29 bits, 18 of them fraction), active + 1 (13), glide ms (10), wait (9), still (1), redraw (1). */
    @JvmStatic @CriticalNative external fun at(h: Long, positionMs: Long, sweep: Boolean, lively: Boolean, force: Boolean, view: Long): Long
    @JvmStatic @CriticalNative external fun shown(h: Long): Long
    @JvmStatic @CriticalNative external fun shownMs(h: Long): Long
    @JvmStatic @CriticalNative external fun tap(h: Long, line: Int): Long
    @JvmStatic @CriticalNative external fun land(h: Long, line: Int)
    @JvmStatic @CriticalNative external fun nudge(h: Long, dir: Int): Long
    @JvmStatic @CriticalNative external fun strength(synced: Boolean, line: Int, active: Int): Float
    @JvmStatic @FastNative external fun matchingLine(old: LongArray, at: Int, next: LongArray, timed: Boolean): Int
}
