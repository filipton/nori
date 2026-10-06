package dev.nori.music.app.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class SingLevelTest {
    /** How many settings writes a drag through [picks] makes, the level following each write. */
    private fun writes(start: Float, picks: List<Float>): Int {
        var level = start
        var n = 0
        for (p in picks) singLevelStep(level, p)?.let { level = it; n++ }
        return n
    }

    @Test fun `a drag writes once per percent it crosses`() {
        // A slow drag across the whole slider: a pointer event every pixel of a 1000 px track.
        assertEquals(100, writes(0f, List(1001) { it / 1000f }))
    }

    @Test fun `a finger resting inside one percent writes nothing`() {
        assertEquals(0, writes(0.25f, listOf(0.2501f, 0.2499f, 0.252f, 0.248f)))
    }
}
