package dev.nori.music.settings

import dalvik.annotation.optimization.CriticalNative
import dalvik.annotation.optimization.FastNative

/**
 * What the equalizer screen asks of the core about its bands. The screen's words and figures are the
 * app's own; the only question for the core is what kind of band a label marks, which follows from the
 * sound chain's filter kinds.
 */
object EqBands {
    init { System.loadLibrary("norimusic") }

    /**
     * What a band's label marks after its frequency (`settings::band_mark`), as its place in
     * `BandMark`: 0 nothing, 1 left, 2 right, 3 low shelf, 4 high shelf, 5 no gain. Primitives only.
     */
    @JvmStatic @CriticalNative external fun mark(kind: Int, channel: Int): Int

    /** The logarithmic frequency slider: its position is the exponent, 20 Hz at 0 to 20 kHz at 1. */
    fun freqToSlider(freq: Float): Float = kotlin.math.log10((freq / 20f).toDouble()).toFloat() / 3f

    fun sliderToFreq(x: Float): Float = 20f * Math.pow(10.0, x.toDouble() * 3.0).toFloat()
}

/**
 * The equalizer's sliders, edited in the core where the settings are kept (settings_store.rs), so a
 * drag builds no settings record and sends none across. -1 means nothing changed.
 */
internal object SoundEdit {
    init { System.loadLibrary("norimusic") }

    // [settings] is the app's settings' handle (`Settings.handle`).

    /** [band] is `[kind, freq, gain, q, channel]` in, and the band as it was kept out; returns the effects. */
    @JvmStatic @FastNative external fun setBand(settings: Long, index: Int, band: FloatArray): Int
    /** [level] an `EqLevel` ordinal; the value as it was kept as float bits in the high 32, the effects in the low. */
    @JvmStatic @CriticalNative external fun setLevel(settings: Long, level: Int, value: Float): Long
    /** One graphic slider: the value as it was kept as float bits in the high 32, the effects in the low; -1 for no change. */
    @JvmStatic @CriticalNative external fun setGraphic(settings: Long, index: Int, value: Float): Long
}
