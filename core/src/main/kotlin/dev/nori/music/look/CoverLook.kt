package dev.nori.music.look

import dalvik.annotation.optimization.CriticalNative
import dalvik.annotation.optimization.FastNative

/**
 * How a page looks, worked out in Rust (crates/look) so every app built on it dresses the same record
 * the same way: a cover's colours and everything dressed in them - the theme's roles, the plates behind
 * the buttons, the chrome, the status bar, the gradients' stops - as one table of ints, looked up and
 * never recomputed. See crates/lyrics/src/look.rs and crates/look/src/dress.rs, whose indices these are.
 */
object CoverLook {
    init { System.loadLibrary("norimusic") }

    /** Pixels a side of the wash handed back. */
    const val WASH = 128

    // A look's entries (nori_look::dress). Colours are ARGB; PAPER, BAND_TINT and the BAND_K* are Float
    // bits, and STATUS_LIGHT is 0 or 1.
    const val EDGE = 0
    const val BACKGROUND = 1
    const val ON = 2
    const val ON_VARIANT = 3
    const val ACCENT = 4
    const val MELT = 5
    const val ON_PRIMARY = 6
    const val SURFACE_VARIANT = 7
    const val SURFACE_CONTAINER = 8
    const val SURFACE_CONTAINER_HIGH = 9
    const val SECONDARY_CONTAINER = 10
    const val OUTLINE_VARIANT = 11
    const val PAPER = 12
    const val PILL = 13
    const val PILL_INK = 14
    const val PILL_PLATE = 15
    const val TINT_INK = 16
    const val CIRCLE_SELECTED = 17
    const val CIRCLE_PLATE = 18
    const val DISC = 19
    const val DISC_INK_SELECTED = 20
    const val FIELD = 21
    const val FORM = 22
    const val SWITCH_OFF = 23
    const val VEIL_13 = 24
    const val VEIL_6 = 25
    const val VEIL_10 = 26
    const val ON_60 = 27
    const val ON_45 = 28
    const val ON_55 = 29
    const val ON_22 = 30
    const val ON_85 = 31
    const val ON_35 = 32
    const val ON_80 = 33
    const val ON_VARIANT_70 = 34
    const val CHROME_SLAB = 35
    const val CHROME_CONTENT = 36
    const val CHROME_PAGE = 37
    const val CHROME_EDGE = 38
    const val CHROME_CONTENT_65 = 39
    const val CHROME_CONTENT_75 = 40
    const val CHROME_FADE = 41
    const val STATUS_LIGHT = 42
    const val BAND_TINT = 43
    const val BAND_KR = 44
    const val BAND_KG = 45
    const val BAND_KB = 46
    const val HERO_EDGE = 47
    const val HERO_MID = 48
    const val FLOOR_0 = 49
    const val FLOOR_22 = 50
    const val FLOOR_75 = 51
    const val LEN = 52

    /**
     * Looks [from] and [to] mixed at [t] into [out], all [LEN] entries (`nori_look::dress::mix`): one frame
     * of a page cross-fading. Primitives only, so a frame crosses once and allocates nothing.
     */
    @JvmStatic @FastNative external fun mix(from: IntArray, to: IntArray, t: Float, out: IntArray)

    /**
     * A page's look ([LEN] entries) and its wash, [WASH] x [WASH] pixels, or null on AMOLED black. The
     * cover loader works them out from the cover's own pixels ([dev.nori.music.data.CoverLoader.colours]).
     */
    class Colours(val look: IntArray, val wash: android.graphics.Bitmap?)

    /**
     * The look of a page in the theme's own colours, from its roles: background, on surface, on surface
     * variant, primary, on primary, surface variant, surface container, surface container high,
     * secondary container, outline variant. Once per theme.
     */
    fun plain(roles: IntArray): IntArray = IntArray(LEN).also { plain(roles, it) }

    /** Primary, on primary, primary container, on primary container, secondary, secondary container,
     *  on secondary container, surface, background, surface variant, on surface variant. */
    fun tones(seed: Int, dark: Boolean): IntArray = IntArray(11).also { tones(seed, dark, it) }

    /** What AMOLED black puts in a dark scheme: background, surface, surface dim, container lowest,
     *  low, container, high, highest. */
    fun amoled(): IntArray = IntArray(8).also { amoled(it) }

    /**
     * One step of the seek bar towards where the song is (`nori_look::motion::seek_step`): the new place,
     * and the milliseconds to wait before the next step (0 the next frame, -1 stop). Primitives only.
     */
    /**
     * The seek bar's two times (`nori-core stage::seek_times`) as `atS shl 32 or leftS`, whole seconds:
     * the finger's place while [dragging], else a held seek ([heldMs] -1 for none), else the music's.
     * Primitives only: it is asked on every frame a finger moves the bar.
     */
    @JvmStatic @CriticalNative external fun seekTimes(dragging: Boolean, drag: Float, heldMs: Long, positionMs: Long, durationMs: Long): Long

    @JvmStatic @CriticalNative external fun seekStep(bar: Float, target: Float, dtS: Float, widthPx: Float, speed: Float): Long

    // The seek bar's pace (nori_look::motion::SeekPace), through [SeekPace]: a handle and primitives.
    @JvmStatic @CriticalNative external fun seekPaceNew(): Long
    @JvmStatic @CriticalNative external fun seekPaceFree(h: Long)
    @JvmStatic @CriticalNative external fun seekPaceSync(h: Long, positionMs: Long, durationMs: Long)
    @JvmStatic @CriticalNative external fun seekPaceHold(h: Long, bar: Float, positionMs: Long, durationMs: Long)
    @JvmStatic @CriticalNative external fun seekPaceStep(h: Long, positionMs: Long, durationMs: Long, dtS: Float, widthPx: Float, rate: Float): Int
    @JvmStatic @CriticalNative external fun seekPaceBar(h: Long): Float
    @JvmStatic @CriticalNative external fun seekPaceTimes(h: Long): Long
    @JvmStatic @CriticalNative external fun seekPaceFrom(h: Long): Long
    @JvmStatic @CriticalNative external fun seekPaceFade(h: Long): Float

    /**
     * [color] moved lighter or darker in its own hue until it reads on [background] (`nori_look::cover::readable`),
     * else [fallback]. ARGB ints.
     */
    @JvmStatic @CriticalNative external fun readable(color: Int, background: Int, fallback: Int): Int

    /** Which glyph the play button shows (`nori-core stage::transport_glyph`): 0 play, 1 pause, 2 spinner. */
    @JvmStatic @CriticalNative external fun transportGlyph(playing: Boolean, buffering: Boolean, waited: Boolean): Int

    /**
     * A page's Shuffle and Play (`nori-core pages::hero_buttons`), packed by `HeroButtons::pack`: bit 0
     * Shuffle lit, 1 Shuffle enabled, 2 pausing, 3 Play enabled, bits 4-5 and 6-7 what Shuffle and Play
     * press (0 start, 1 toggle, 2 shuffle off). Primitives only: it is asked on every play and pause.
     */
    @JvmStatic @CriticalNative external fun heroButtons(here: Boolean, shuffle: Boolean, playing: Boolean, buffering: Boolean, canPlay: Boolean, canShuffle: Boolean): Int

    @JvmStatic @FastNative private external fun plain(roles: IntArray, out: IntArray)
    @JvmStatic @FastNative private external fun tones(seed: Int, dark: Boolean, out: IntArray)
    @JvmStatic @FastNative private external fun amoled(out: IntArray)
}
