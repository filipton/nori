package dev.nori.music.look

/**
 * The seek bar's place and the times under it, stepped towards where the song is by nori-look
 * (`nori_look::motion::SeekPace`): drawn where the song is while it plays, gliding over a jump (a new song,
 * a seek, a mix handing over) while the times cross-fade, and [sync]ed straight to the song when the bar
 * comes back on screen. One per seek bar, used from the main thread only; [close] it when the bar goes.
 */
class SeekPace : AutoCloseable {
    /** What the bar shows, left here by every change (`PaceShown` in crates/android/src/look.rs). */
    private val view = dev.nori.music.NativeView(24)
    private var h = CoverLook.seekPaceNew(view.address)

    /** Everything where the song is, at once, with nothing gliding: a bar coming (back) on screen. */
    fun sync(positionMs: Long, durationMs: Long) = CoverLook.seekPaceSync(h, positionMs, durationMs)

    /** The bar held at [bar] (0..1), nothing moving: where a released scrub left it. */
    fun hold(bar: Float, positionMs: Long, durationMs: Long) = CoverLook.seekPaceHold(h, bar, positionMs, durationMs)

    /** One step; the wait before the next (0: next frame, ms, -1: nothing moves until something changes). */
    fun step(positionMs: Long, durationMs: Long, dtS: Float, widthPx: Float, rate: Float): Int =
        CoverLook.seekPaceStep(h, positionMs, durationMs, dtS, widthPx, rate)

    /** Where the bar is drawn, 0..1. */
    val bar: Float get() = view.buffer.getFloat(0)

    /** How strongly the times now are drawn, 0..1 (1: nothing fading). */
    val fade: Float get() = view.buffer.getFloat(4)

    /** The times shown, `atS shl 32 or leftS`. */
    val times: Long get() = view.buffer.getLong(8)

    /** The times fading out, packed as [times]; -1 with none. */
    val fadingFrom: Long get() = view.buffer.getLong(16)

    override fun close() {
        CoverLook.seekPaceFree(h)
        h = 0
    }
}
