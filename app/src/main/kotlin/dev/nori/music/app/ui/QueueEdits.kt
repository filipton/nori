package dev.nori.music.app.ui

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlin.math.roundToInt

/**
 * A key for each of the queue's rows that stays with its song when songs before it come and go, so a row
 * taken out leaves and the rows under it close up (rather than every row below changing key, and so
 * content, in place). A song in the queue twice is told apart by how many times it came before.
 */
internal fun queueKeys(ids: List<String>): List<String> {
    val seen = HashMap<String, Int>()
    return ids.map { id -> "$id#${seen.merge(id, 1, Int::plus)}" }
}

/**
 * Where the row held at [from] would land [offset] pixels away, rows [rowHeight] tall, in a list of [size],
 * no higher than row [first] (the songs already played and the one playing stay where they are).
 */
internal fun reorderTarget(from: Int, offset: Float, rowHeight: Float, size: Int, first: Int = 0): Int {
    if (from < 0 || rowHeight <= 0f) return from
    val last = (size - 1).coerceAtLeast(0)
    return (from + (offset / rowHeight).roundToInt()).coerceIn(first.coerceIn(0, last).coerceAtMost(from), last)
}

/**
 * Whether a queue come to rest is resting on the song playing, and so follows it to its next row: the row
 * at its top ([top], a row key) is that song's ([playing]), or the list is at its end ([atEnd]), where the
 * song could not be brought any higher. A list the user left anywhere else stays where they put it.
 */
internal fun queueFollows(top: Any?, playing: Any?, atEnd: Boolean): Boolean = playing != null && (top == playing || atEnd)

/**
 * How far row [at] is drawn from its place while the row at [from] is held [offset] pixels away: the held
 * row follows the finger, and the rows it has passed step out of its way, a row each.
 */
internal fun reorderShift(at: Int, from: Int, target: Int, offset: Float, rowHeight: Float): Float = when {
    from < 0 -> 0f
    at == from -> offset
    at in (from + 1)..target -> -rowHeight
    at in target until from -> rowHeight
    else -> 0f
}

/**
 * The queue panel's drag to reorder. Written by the gesture and read where the rows are drawn
 * ([shift], from a `graphicsLayer`), so a frame of the drag moves layers and recomposes no row.
 * Nothing is reordered until the finger lifts: the drop is animated to its slot, sent, and the rows keep
 * their drawn places until the queue has changed from [landing] - on that frame they are laid out where
 * they were drawn, so nothing jumps.
 */
@Stable
internal class QueueDrag {
    /** The row held, by its place in the list as drawn; -1 none. */
    var from by mutableIntStateOf(-1)
    /** The key of the row lifted by the drag, as long as it is ([lift] above 0: it settles after the drop). */
    var liftKey by mutableStateOf<String?>(null)
    val offset = mutableFloatStateOf(0f)
    val lift = mutableFloatStateOf(0f)
    var rowHeight by mutableFloatStateOf(0f)
    var size = 0
    /** The highest row a drop may land on: the one after the song playing. */
    var first = 0
    /** The queue a drop was sent against: set, the rows hold their places until the queue is another. */
    var landing by mutableStateOf<Any?>(null)

    fun target(): Int = reorderTarget(from, offset.floatValue, rowHeight, size, first)

    fun shift(at: Int): Float = reorderShift(at, from, target(), offset.floatValue, rowHeight)

    /** Whether a drop has landed: the queue on the page is no longer the one it was sent against. */
    fun landed(queue: Any): Boolean = landing != null && landing !== queue

    fun clear() {
        from = -1
        offset.floatValue = 0f
        landing = null
    }
}

/**
 * The undo for a song taken out of the queue: the last one only, offered for a few seconds. [T] is the
 * song. [shown] is what the undo offers; [returning] is the row coming back, which slides in from the
 * side it left by ([Taken.side]: -1 left, 1 right, 0 it fades in, as after the × button).
 */
@Stable
internal class QueueUndo<T> {
    class Taken<T>(val item: T, val index: Int, val key: String, val side: Float)

    var shown by mutableStateOf<Taken<T>?>(null)
        private set
    var returning by mutableStateOf<Taken<T>?>(null)
        private set

    /** [item], at list [index] and with row [key], was taken out; it replaces any undo still offered. */
    fun took(item: T, index: Int, key: String, side: Float): Taken<T> = Taken(item, index, key, side).also { shown = it; returning = null }

    /** The undo pressed: what to put back, once; its row is then [returning] until it has [arrived]. */
    fun undo(): Taken<T>? {
        val t = shown ?: return null
        shown = null
        returning = t
        return t
    }

    /** The time for [t] is up; a later removal's undo stays. */
    fun expire(t: Taken<T>) {
        if (shown === t) shown = null
    }

    /** The row [key] is back on screen (or will not slide in): nothing is returning any more. */
    fun arrived(key: String) {
        if (returning?.key == key) returning = null
    }
}
