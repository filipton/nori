package dev.nori.music.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class QueueEditsTest {
    @Test fun `a row keeps its key when a song before it goes`() {
        val before = queueKeys(listOf("a", "b", "c", "b"))
        assertEquals(listOf("a#1", "b#1", "c#1", "b#2"), before)
        val after = queueKeys(listOf("b", "c", "b"))
        assertEquals(before.drop(1), after)
        // A song twice: taking out the first makes the second the first, which is the one that closes up.
        assertEquals(listOf("a#1", "c#1", "b#1"), queueKeys(listOf("a", "c", "b")))
    }

    @Test fun `the held row follows the finger and the rows it passes step aside`() {
        val h = 100f
        assertEquals(-1, reorderTarget(-1, 500f, h, 5))
        assertEquals(1, reorderTarget(1, 40f, h, 5))
        assertEquals(3, reorderTarget(1, 160f, h, 5))
        assertEquals(4, reorderTarget(1, 10_000f, h, 5))
        assertEquals(0, reorderTarget(1, -10_000f, h, 5))
        assertEquals("no row measured yet", 1, reorderTarget(1, 160f, 0f, 5))

        // Row 1 held and dragged down past two rows: 2 and 3 move up a row each, 0 and 4 stay.
        val shift = { at: Int -> reorderShift(at, 1, 3, 160f, h) }
        assertEquals(listOf(0f, 160f, -h, -h, 0f), (0..4).map(shift))
        // Dragged up past one row: that row moves down.
        assertEquals(listOf(0f, h, -60f, 0f), (0..3).map { reorderShift(it, 2, 1, -60f, h) })
        assertEquals(listOf(0f, 0f, 0f), (0..2).map { reorderShift(it, -1, -1, 0f, h) })
    }

    @Test fun `a song is not dropped among the ones played or above the one playing`() {
        val h = 100f
        // Row 2 plays: a song held at row 4 goes no higher than row 3.
        assertEquals(3, reorderTarget(4, -10_000f, h, 6, first = 3))
        assertEquals(5, reorderTarget(4, 10_000f, h, 6, first = 3))
        // A row above the floor (never lifted) is not pushed down by it.
        assertEquals(1, reorderTarget(1, 0f, h, 6, first = 3))
        assertEquals(4, reorderTarget(4, -10_000f, h, 5, first = 9))
    }

    @Test fun `the queue follows the song playing unless scrolled away`() {
        assertTrue(queueFollows("a#1", "a#1", atEnd = false))
        assertTrue("at the end, where the song could go no higher", queueFollows("x#1", "a#1", atEnd = true))
        assertFalse("scrolled elsewhere", queueFollows("x#1", "a#1", atEnd = false))
        assertFalse("nothing playing", queueFollows("a#1", null, atEnd = true))
    }

    @Test fun `a drop holds the rows until the queue has changed`() {
        val d = QueueDrag()
        d.rowHeight = 100f
        d.size = 4
        d.from = 0
        d.offset.floatValue = 200f
        assertEquals(2, d.target())
        val queue = listOf("a", "b", "c", "d")
        d.landing = queue
        assertFalse(d.landed(queue))
        assertEquals(200f, d.shift(0), 0f)
        assertTrue(d.landed(listOf("b", "c", "a", "d")))
        d.clear()
        assertEquals(-1, d.from)
        assertEquals(0f, d.shift(0), 0f)
        assertFalse(d.landed(queue))
    }

    @Test fun `the undo offers the last song taken out, once`() {
        val u = QueueUndo<String>()
        assertNull(u.undo())
        val first = u.took("one", 3, "one#1", -1f)
        assertSame(first, u.shown)
        val second = u.took("two", 5, "two#1", 0f)
        assertSame(second, u.shown)
        u.expire(first)
        assertSame("an older undo running out leaves the newer", second, u.shown)

        val back = u.undo()!!
        assertEquals("two", back.item)
        assertEquals(5, back.index)
        assertNull(u.shown)
        assertNull("once", u.undo())
        assertSame(back, u.returning)
        u.arrived("one#1")
        assertSame(back, u.returning)
        u.arrived("two#1")
        assertNull(u.returning)
    }

    @Test fun `an undo not taken runs out`() {
        val u = QueueUndo<String>()
        val t = u.took("one", 0, "one#1", -1f)
        u.expire(t)
        assertNull(u.shown)
        assertNull(u.undo())
        assertNull(u.returning)
    }
}
