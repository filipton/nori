package dev.nori.music.app.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.util.VelocityTracker
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * The full player as a sheet over the app rather than a page navigated to. It is one number,
 * [progress], from 0 (only the mini player showing) to 1 (the player filling the screen), and every
 * part of the transition is drawn from it: the sheet's top edge travels from the mini player's top to
 * the top of the screen, and the cover grows from the mini player's thumbnail into the sleeve. A drag
 * sets the number directly, so the whole thing follows the finger and holds wherever it is held; a
 * release settles it with the finger's own speed. Closing is the same path backwards.
 *
 * Before this the player was a route: a drag on the mini player had to cross a threshold, and then a
 * separate slide played on its own - you pulled, and then something else happened.
 */
@Stable
class PlayerSheet(private val scope: CoroutineScope) {
    val progress = Animatable(0f).also { it.updateBounds(0f, 1f) }

    /** The mini player's artwork, in root coordinates: where the cover flies from and back to. */
    var miniCover by mutableStateOf(Rect.Zero)

    /**
     * The cover a panel of its own shows - the lyrics header's thumbnail - in the sheet's coordinates,
     * or [Rect.Zero] when the panel on screen has no cover. What the cover flies to and from when the
     * player is put away from that panel, in place of the sleeve.
     */
    var panelCover by mutableStateOf(Rect.Zero)

    /**
     * One cover is travelling between the sleeve and a panel's own thumbnail right now, so neither end
     * draws its own copy: the flight is the only one on screen. See PanelFlight in PlayerScreen.
     */
    var panelFlight by mutableStateOf(false)

    /** The top of the mini player, in root coordinates: where the sheet's top edge starts. */
    var miniTop by mutableFloatStateOf(0f)

    /** The window's height, for when there is no mini player to start from. */
    var rootHeight by mutableFloatStateOf(0f)

    /** Settle quickly and without a spring: the user's reduce-motion switch. */
    var plain = false

    /**
     * Open, or on the way there. What the back button, the test bridge and the chrome go by.
     *
     * It is the last thing the sheet was *asked* to do, not where it happens to be: a drag or a back
     * gesture moves [progress] (and with it the Animatable's target) without meaning anything by it,
     * and reading that turned the sheet "closed" on the first pixel of a gesture - which switched the
     * back handler off underneath the finger, so the player never sank and then vanished in one frame
     * instead of settling back onto the now playing bar.
     */
    var isOpen by mutableStateOf(false)
        private set

    /** How far the sheet's top edge travels, in pixels. */
    val travel: Float get() = if (miniTop > 0f) miniTop else rootHeight.coerceAtLeast(1f)

    /** The sheet's top edge, relative to the top of the screen. */
    fun offset(): Float = (1f - progress.value) * travel

    fun open() = settle(1f, 0f)

    /** Open at once, no travel: the sheet as it was before the app was put back together. */
    suspend fun openAtOnce() {
        isOpen = true
        progress.snapTo(1f)
    }

    /**
     * The back gesture at [progress] (0..1): the sheet sinks with it, a fifth of the way at most - a hint
     * of where it is going, not the whole trip, which is the release's to make.
     */
    suspend fun backBy(progress: Float) {
        val eased = 1f - (1f - progress.coerceIn(0f, 1f)).let { it * it }
        this.progress.snapTo(1f - BACK_TRAVEL * eased)
    }
    fun close() = settle(0f, 0f)

    /**
     * Put away by the back gesture: quicker than a close from a tap. The gesture has already carried the
     * sheet part of the way, and the rest at the drag's pace read as the app lagging behind the thumb.
     */
    fun backClose() = settle(0f, 0f, stiffness = 700f)

    /** Which end the sheet was nearer when the finger went down. */
    private var from = 0f

    fun dragStart() { from = if (progress.value > 0.5f) 1f else 0f }

    /** Moves the sheet with a finger that moved [dy] pixels (down is positive). */
    fun dragBy(dy: Float) {
        scope.launch { progress.snapTo((progress.value - dy / travel).coerceIn(0f, 1f)) }
    }

    /** The finger let go at [velocityY] pixels a second: the sheet settles where [sheetTarget] says. */
    fun release(velocityY: Float) {
        settle(sheetTarget(from, progress.value, velocityY), -velocityY / travel)
    }

    private fun settle(target: Float, velocity: Float, stiffness: Float = 420f) {
        isOpen = target == 1f
        scope.launch {
            // Critically damped: it arrives with the finger's speed and does not bounce past either end.
            if (plain) progress.animateTo(target, tween(120))
            else progress.animateTo(target, spring(dampingRatio = 1f, stiffness = stiffness), initialVelocity = velocity)
        }
    }

}

val LocalPlayerSheet = staticCompositionLocalOf<PlayerSheet> { error("no player sheet") }

/**
 * Whether the player is on screen at all. It stays composed while it is put away, so that opening it
 * only has to move it - building it from nothing on the first frame of the drag stalled that frame -
 * and everything in it that ticks, or reaches outside the player, checks this first.
 */
val LocalPlayerShown = androidx.compose.runtime.compositionLocalOf { true }

/**
 * Vertical drags on this element move the sheet. The velocity is taken from the summed deltas, not
 * from the pointer's position: inside the sheet the element moves with the finger, so its local
 * position barely changes and would report a flick as standing still.
 */
internal fun Modifier.dragsSheet(sheet: PlayerSheet, enabled: Boolean = true): Modifier =
    if (!enabled) this else pointerInput(sheet) {
        val tracker = VelocityTracker()
        var y = 0f
        detectVerticalDragGestures(
            onDragStart = { tracker.resetTracking(); y = 0f; sheet.dragStart() },
            onDragEnd = { sheet.release(tracker.calculateVelocity().y) },
            onDragCancel = { sheet.release(0f) },
        ) { change, dy ->
            y += dy
            tracker.addPosition(change.uptimeMillis, Offset(0f, y))
            sheet.dragBy(dy)
        }
    }

/** How far the back gesture takes the player down before it is let go. */
private const val BACK_TRAVEL = 0.2f
/** The player sheet: a release faster than this, in pixels a second, goes the way it was flicked... */
private const val SHEET_FLICK_PX_S = 900f
/** ...and a slow drag that has come this share of the way finishes the move it started. */
private const val SHEET_COMMIT = 0.15f

/**
 * Where the player sheet settles when the finger lifts: 1 open, 0 put away. [from] is the end it was
 * nearer when the drag began, [progress] where it is now and [velocity] the finger's (down positive). A
 * flick goes the way it was flicked; a drag that has come a little way finishes the move it started, and
 * a smaller one goes back. Deciding by the halfway point instead meant a pull down from the player had to
 * cover half the screen before it would close.
 */
private fun sheetTarget(from: Float, progress: Float, velocity: Float): Float {
    val moved = progress - from
    return when {
        velocity < -SHEET_FLICK_PX_S -> 1f
        velocity > SHEET_FLICK_PX_S -> 0f
        moved > SHEET_COMMIT -> 1f
        moved < -SHEET_COMMIT -> 0f
        else -> from
    }
}
