package dev.nori.music.app.ui

import dev.nori.music.app.R

import androidx.compose.runtime.mutableStateOf
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.runtime.Stable
import androidx.compose.runtime.State
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.animation.togetherWith
import androidx.compose.animation.core.Animatable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.horizontalDrag
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CloudDownload
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.automirrored.filled.PlaylistPlay
import androidx.compose.material.icons.automirrored.filled.QueueMusic
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.HeartBroken
import dev.nori.music.ffi.settings.SwipeAction
import androidx.compose.material.icons.filled.GraphicEq
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.MusicNote
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import kotlinx.coroutines.isActive
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.Load
import dev.nori.music.ffi.model.Album
import dev.nori.music.ffi.model.Song
import dev.nori.music.look.CoverLook
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.drawscope.clipRect
import kotlinx.coroutines.launch
import kotlin.math.roundToInt

/**
 * A cover of an octo-fiesta provider item (external song, album, artist or playlist), which is never
 * kept (`Covers.isProvider`).
 */
fun isProviderCover(url: String): Boolean = dev.nori.music.data.Covers.isProvider(url)

/**
 * The sizes covers are drawn at, nori-core's (`cover_rules`): two, not four, because a Subsonic server
 * renders each size it is asked for on demand. A list thumbnail and a grid card share one rendition, and
 * the full-screen artwork shares its rendition with the notification and the lock screen.
 */
object CoverSize {
    private val rules get() = dev.nori.music.data.Covers.rules
    val ROW: Int = rules.row.toInt()
    val CARD: Int = rules.card.toInt()
    val FULL: Int = rules.full.toInt()
}

/**
 * Artwork with the app's corner radius. The picture is asked for at the view's own size, decoded by the
 * core straight into a Bitmap that size (rememberCover), and let go if the view leaves first; the
 * rounded clip is a plain render-node clip, which the GPU does for free and which a grid of covers needs
 * to not look like a spreadsheet. Pass `radius = 0.dp` for the full-bleed artwork at the top of a page;
 * `size = 0.dp` sizes it by its layout, and it asks once it has been measured.
 *
 * Nothing about it appears in one frame. A picture that has to be fetched fades in over its plate
 * (one kept in memory is simply there - fading those in made every scroll shimmer); one that takes a
 * while shows a soft sheen crossing the plate, so a slow server reads as loading rather than as a
 * missing cover; and one that never comes settles into the plate's note glyph, faded in too.
 *
 * [plate] false draws nothing of its own - no plate, sheen or note - so the picture fades in over
 * whatever is behind it (a mix tile's colour), and a missing one simply leaves that showing.
 */
@Composable
fun Cover(url: String?, size: Dp, modifier: Modifier = Modifier, radius: Dp = Radius.cover, plate: Boolean = true) {
    val px = with(LocalDensity.current) { size.roundToPx() }
    val cover = rememberCover(url, px, px)
    // Sized by its layout: asked for from layout, where the size is known, rather than through a state
    // that would compose every such cover a second time. The last size is kept for the next address.
    val measured = remember { IntArray(2) }
    if (px == 0) androidx.compose.runtime.DisposableEffect(cover) {
        if (measured[0] > 0 && measured[1] > 0) cover.want(measured[0], measured[1])
        onDispose {}
    }
    val shape = remember(radius) { androidx.compose.foundation.shape.RoundedCornerShape(radius) }
    // A flat grey square is what makes a library of half-loaded covers look broken. Underneath every
    // cover sits a soft two-tone plate with a note on it, which is what shows while the picture loads
    // and what stays when a track simply has no artwork. One gradient in the page's look, drawn - and
    // read while drawing, so the player's page changing colour under a cover only redraws it.
    val look = LocalLook.current
    val loading = cover.state == CoverImage.LOADING
    val missing = cover.state == CoverImage.MISSING
    // The sheen outlives the load by the length of the picture's fade, so it goes away underneath a
    // picture that is already covering it instead of vanishing from on top of the plate.
    var sheen by remember(cover) { mutableStateOf(loading) }
    androidx.compose.runtime.LaunchedEffect(loading) { if (!loading) kotlinx.coroutines.delay(SHEEN_LEAVE_MS.toLong()); sheen = loading }
    // Read while drawing, so the fade redraws the picture and recomposes nothing.
    val fade = remember(cover) { androidx.compose.animation.core.Animatable(if (cover.image != null) 1f else 0f) }
    val here = cover.image != null
    androidx.compose.runtime.LaunchedEffect(cover, here) {
        if (!here || fade.value == 1f) return@LaunchedEffect
        if (AppMotion.reduce) fade.snapTo(1f) else fade.animateTo(1f, tween(260, easing = androidx.compose.animation.core.LinearEasing))
    }
    Box(
        // Without a size it fills what it is given, as the picture it used to hold did.
        (if (px > 0) modifier.size(size) else modifier.fillMaxSize().onSizeChanged {
            measured[0] = it.width; measured[1] = it.height
            if (it.width > 0 && it.height > 0) cover.want(it.width, it.height)
        })
            .then(if (radius > 0.dp) Modifier.clip(shape) else Modifier)
            .drawWithCache {
                val brush = if (plate) Brush.linearGradient(listOf(look.color(CoverLook.VEIL_13), look.color(CoverLook.VEIL_6))) else null
                onDrawWithContent {
                    brush?.let { drawRect(it) }
                    drawContent()
                    cover.image?.let { drawCover(it, fade.value) }
                }
            },
    ) {
        // A load that ends with no picture (offline, a failure) fades the sheen out as the note fades in.
        if (sheen && plate) Box(Modifier.matchParentSize().loadingSheen(true, leaving = !loading))
        androidx.compose.animation.AnimatedVisibility(
            missing && plate, Modifier.align(Alignment.Center),
            enter = androidx.compose.animation.fadeIn(androidx.compose.animation.core.tween(300)), exit = androidx.compose.animation.fadeOut(),
        ) {
            LookIcon(
                Icons.Filled.MusicNote, null,
                Modifier.size(if (size > 0.dp) size * 0.34f else 40.dp),
            ) { look.color(CoverLook.ON_22) }
        }
    }
}

/**
 * "3:07", or "1:02:03" from an hour ([dev.nori.music.text.Fmt.duration]). Each second's text is made once
 * for the life of the process and kept: the seek bar asks for two of these every second a song plays, and
 * every list row for its song's length, so after the first time through they cost an array read.
 */
fun duration(seconds: Long): String = dev.nori.music.text.Fmt.duration(seconds)

/** The time left, "-3:07": kept the same way. */
fun durationLeft(seconds: Long): String = dev.nori.music.text.Fmt.durationLeft(seconds)

/** What a sideways drag on a song row does: what it uncovers under the row, and what letting go past [SWIPE_ARM] does. */
class RowSwipe(val icon: ImageVector, val label: String, val action: () -> Unit)

/** One per row: the Animatable the row slides on, and whether letting go now would act. */
@Stable
class SwipeState {
    /** Where the row is, written straight from the finger; see [swipeable]. */
    val offset = androidx.compose.runtime.mutableFloatStateOf(0f)
    var armed by mutableStateOf(false)
    /** The spring back, while it runs: a new drag takes the row from wherever it has got to. */
    var settling: kotlinx.coroutines.Job? = null
}

/** How far across the row a drag has to go before letting go acts: the share that turns a record. */
private const val SWIPE_ARM = TURN

/**
 * Sideways drag on a row. Only a direction with an action moves at all. Past [SWIPE_ARM] of the width
 * the row goes heavier (it follows at 40%), the strip underneath takes the accent colour and the phone
 * ticks, so the finger knows before it lifts; letting go then acts and the row springs back. The row
 * paints [fill] under itself only while it is off its place, so the strip never shows through it.
 */
internal fun Modifier.swipeable(
    s: SwipeState, right: RowSwipe?, left: RowSwipe?, fill: Color?,
    /** The action takes the row away (a song out of the queue): armed and let go, the row slides off first. */
    gone: Boolean = false,
    /** A side with no action still gives a little ([GIVE] of the finger, at most [GIVE_LIMIT] of the width) and comes back: "not this one". */
    resist: Boolean = false,
): Modifier = composed {
    val scope = rememberCoroutineScope()
    val haptics = LocalHapticFeedback.current
    val back = remember { spring<Float>(dampingRatio = 0.8f, stiffness = 520f) }
    // The gesture outlives recompositions (it is keyed on which sides act, not on the actions), so it
    // reads the actions as they are now: a favourite swiped once must offer "Remove" the second time.
    val acts by rememberUpdatedState(right to left)
    pointerInput(right != null, left != null, resist) {
        var x = 0f
        // The finger's own travel, for a side that only gives.
        var travel = 0f
        fun settle() {
            s.armed = false
            s.settling = scope.launch { androidx.compose.animation.core.animate(s.offset.floatValue, 0f, animationSpec = back) { v, _ -> s.offset.floatValue = v } }
        }
        // A row that went but is still here long after (the change did not happen): it comes back rather
        // than leave a hole. A row that really went has left the composition by then, and this with it.
        suspend fun comeBackIfStill() {
            kotlinx.coroutines.delay(SWIPE_GONE_WAIT_MS)
            s.armed = false
            androidx.compose.animation.core.animate(s.offset.floatValue, 0f, animationSpec = back) { v, _ -> s.offset.floatValue = v }
        }
        sidewaysDrag(
            onDragStart = { s.settling?.cancel(); x = s.offset.floatValue; travel = x },
            onDragEnd = {
                val act = if (s.armed) (if (x > 0f) acts.first else acts.second) else null
                when {
                    act == null -> settle()
                    gone && !AppMotion.reduce -> s.settling = scope.launch {
                        // Off the side it was going, at the speed of a short slide, and only then acted on:
                        // the rows under it close up once the queue has changed.
                        val to = kotlin.math.sign(x) * size.width.toFloat()
                        androidx.compose.animation.core.animate(s.offset.floatValue, to, animationSpec = tween(SWIPE_GONE_MS, easing = androidx.compose.animation.core.FastOutLinearInEasing)) { v, _ -> s.offset.floatValue = v }
                        act.action()
                        comeBackIfStill()
                    }
                    gone -> s.settling = scope.launch { s.offset.floatValue = kotlin.math.sign(x) * size.width.toFloat(); act.action(); comeBackIfStill() }
                    else -> { act.action(); settle() }
                }
            },
            onDragCancel = { settle() },
        ) { change, delta ->
            val w = size.width.toFloat()
            val arm = w * SWIPE_ARM
            travel += delta
            change.consume()
            val acting = if (travel > 0f) right != null else left != null
            if (resist && !acting) {
                x = kotlin.math.sign(travel) * kotlin.math.min(kotlin.math.abs(travel) * GIVE, w * GIVE_LIMIT)
                s.offset.floatValue = x
                return@sidewaysDrag
            }
            val heavy = kotlin.math.abs(x) > arm && (delta > 0f) == (x > 0f)
            x = (x + if (heavy) delta * 0.4f else delta).coerceIn(if (left != null) -w * 0.6f else 0f, if (right != null) w * 0.6f else 0f)
            travel = x
            val armed = kotlin.math.abs(x) > arm
            if (armed != s.armed) { s.armed = armed; haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove) }
            // Written, not snapped to from a coroutine: one launch per pointer event was a job per frame.
            s.offset.floatValue = x
        }
    }
        .graphicsLayer { translationX = s.offset.floatValue }
        .then(if (fill != null) Modifier.drawBehind { if (s.offset.floatValue != 0f) drawRect(fill) } else Modifier)
}

/** How long a row taken away by its swipe takes to leave from where the finger let go. */
private const val SWIPE_GONE_MS = 200
private const val SWIPE_GONE_WAIT_MS = 1_500L

/**
 * A drag taken only when it is plainly sideways: once the finger has gone [slop] times the touch slop,
 * it has to have moved at least [ratio] times as far across as down. Anything steeper, or anything
 * something else has already taken, is left alone, so a scroll that is a little off vertical scrolls
 * instead of swiping a song, and a diagonal pull upwards opens the player instead of changing it.
 * ([detectHorizontalDragGestures] claims a drag on the sideways distance alone, which a slanted drag
 * easily reaches first - which is what every one of those was.)
 *
 * Everywhere a sideways drag shares its space with an up-and-down one uses this: the rows of a list,
 * the now playing bar and the full-screen sleeve, so the three feel like one gesture.
 */
internal suspend fun androidx.compose.ui.input.pointer.PointerInputScope.sidewaysDrag(
    onDragStart: () -> Unit, onDragEnd: () -> Unit, onDragCancel: () -> Unit,
    slop: Float = 1f, ratio: Float = 2f,
    onDrag: (androidx.compose.ui.input.pointer.PointerInputChange, Float) -> Unit,
) = awaitEachGesture {
    val down = awaitFirstDown(requireUnconsumed = false)
    val decide = viewConfiguration.touchSlop * slop
    var dx = 0f
    var dy = 0f
    while (true) {
        val c = awaitPointerEvent().changes.firstOrNull { it.id == down.id } ?: return@awaitEachGesture
        if (!c.pressed || c.isConsumed) return@awaitEachGesture
        val d = c.positionChange()
        dx += d.x; dy += d.y
        if (dx * dx + dy * dy < decide * decide) continue
        if (kotlin.math.abs(dx) < ratio * kotlin.math.abs(dy)) return@awaitEachGesture
        c.consume()
        break
    }
    onDragStart()
    val finished = horizontalDrag(down.id) { onDrag(it, it.positionChange().x) }
    if (finished) onDragEnd() else onDragCancel()
}

/**
 * The strip a swiped row uncovers: the action's icon and words at the edge the row left, fading in over
 * the first 64 dp. Neutral until the drag is far enough to act, then the accent, with the icon giving
 * a small pop. Composed only while the row is off its place, so a list at rest carries none of it.
 */
@Composable
internal fun SwipeBackdrop(
    s: SwipeState, right: RowSwipe?, left: RowSwipe?, modifier: Modifier,
    /** Its colours: the theme's (a list on a page) unless given (the player's queue, on the cover's colours). */
    colours: SwipeColours? = null,
    /** Drawn only where the row has moved off, for a row with no fill of its own to cover the rest. */
    reveal: Boolean = false,
    /** The words' and icon's distance from the edge. */
    inset: Dp = Space.gutter,
) {
    val side by remember { derivedStateOf { kotlin.math.sign(s.offset.floatValue) } }
    if (side == 0f) return
    val face = (if (side > 0f) right else left) ?: return
    val scheme = MaterialTheme.colorScheme
    val c = colours ?: remember(scheme) {
        SwipeColours({ scheme.surfaceContainerHighest }, { scheme.primary }, { scheme.onSurfaceVariant }, { scheme.onPrimary })
    }
    val armed by animateFloatAsState(if (s.armed) 1f else 0f, tween(140), label = "swipe armed")
    val fill = { androidx.compose.ui.graphics.lerp(c.fill(), c.armedFill(), armed) }
    val ink = androidx.compose.ui.graphics.ColorProducer { androidx.compose.ui.graphics.lerp(c.ink(), c.armedInk(), armed) }
    val pop by animateFloatAsState(if (s.armed) 1.15f else 1f, spring(dampingRatio = 0.45f, stiffness = 700f), label = "swipe pop")
    Box(
        modifier.drawWithContent {
            val x = s.offset.floatValue
            // The strip the row has uncovered, or the whole row under a row that paints over the rest itself.
            val from = if (!reveal) 0f else if (x > 0f) 0f else size.width + x
            val to = if (!reveal) size.width else if (x > 0f) x else size.width
            if (to <= from) return@drawWithContent
            clipRect(left = from, right = to) {
                drawRect(fill())
                this@drawWithContent.drawContent()
            }
        },
    ) {
        Row(
            Modifier.align(if (side > 0f) Alignment.CenterStart else Alignment.CenterEnd).padding(horizontal = inset)
                .graphicsLayer { alpha = (kotlin.math.abs(s.offset.floatValue) / 64.dp.toPx()).coerceIn(0f, 1f) },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            LookIcon(face.icon, null, Modifier.size(22.dp).graphicsLayer { scaleX = pop; scaleY = pop }, ink)
            LookText(face.label, ink, Modifier.padding(start = 10.dp), style = MaterialTheme.typography.labelLarge, maxLines = 1)
        }
    }
}

/** A swipe's strip: plain, and once far enough to act; its words and icon the same. Read in the draw phase. */
internal class SwipeColours(val fill: () -> Color, val armedFill: () -> Color, val ink: () -> Color, val armedInk: () -> Color)

/**
 * One track. Numbered rows (an album) carry no artwork; everywhere else the cover leads. The row ends
 * in a hairline that starts where the text starts, which is what keeps a long list from reading as a
 * stack of boxes.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun SongRow(
    song: Song, coverUrl: String?, onClick: () -> Unit, onMenu: () -> Unit, modifier: Modifier = Modifier,
    number: Int? = null, playing: Boolean = false, downloaded: Boolean = false,
    selected: Boolean = false, onLongClick: (() -> Unit)? = null, swipeRight: RowSwipe? = null, swipeLeft: RowSwipe? = null,
    divider: Boolean = true,
    /** The second line: the song's own (its explicit mark and artist), or what the page makes of it (an album's, without its own artist). */
    line: String = song.line,
) {
    val scheme = MaterialTheme.colorScheme
    val swipe = if (swipeRight != null || swipeLeft != null) remember { SwipeState() } else null
    Column(modifier.fillMaxWidth().background(if (selected) scheme.secondaryContainer else Color.Transparent)) {
      Box(Modifier.fillMaxWidth()) {
        if (swipe != null) SwipeBackdrop(swipe, swipeRight, swipeLeft, Modifier.matchParentSize())
        Row(
            Modifier.fillMaxWidth()
                .then(if (swipe != null) Modifier.swipeable(swipe, swipeRight, swipeLeft, if (selected) scheme.secondaryContainer else scheme.background) else Modifier)
                .combinedClickable(onClick = onClick, onLongClick = onLongClick)
                .padding(start = Space.gutter, top = 9.dp, bottom = 9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // The track playing shows a waveform where its number would be - the same cue Apple uses, and
            // clearer at a glance than the title merely changing colour.
            if (number != null) Box(Modifier.width(26.dp), Alignment.Center) {
                if (playing) PlayingBars(scheme.primary, Modifier.size(16.dp))
                else Text(
                    if (number > 0) "$number" else "", textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodyMedium, color = scheme.onSurfaceVariant,
                )
            } else Cover(coverUrl, 46.dp, radius = 6.dp)
            Column(Modifier.weight(1f).padding(start = if (number != null) 14.dp else 12.dp, end = 8.dp)) {
                // With no second line (an album's own tracks) the cloud leads the title rather than standing alone under it.
                RemoteLine(
                    song.title, song.isExternal && line.isEmpty(), MaterialTheme.typography.bodyLarge,
                    color = if (playing) scheme.primary else scheme.onSurface,
                )
                // Worked out by the core when the song was read (`fmt::song_line`), not per row.
                if (line.isNotEmpty()) RemoteLine(line, song.isExternal, MaterialTheme.typography.bodySmall)
            }
            val tint = scheme.onSurfaceVariant
            // Every row's marks sit in columns of their own, the same width on every row: the heart, then
            // the download ring or tick, then the time, then the menu. A list of favourites then puts all
            // its hearts one above another - placed after the title, the heart moved with the width of
            // the time beside it, "3:44" against "12:05" - and a mark arriving or leaving moves nothing
            // else. The time is last before the menu and in a box of one fixed width, its digits held to
            // the right edge, so it sits against the ⋯ rather than with an empty download slot between.
            // A jam guest's hearts would be the host's: none are shown.
            val rules = LocalRules.current
            Box(Modifier.padding(start = 4.dp).width(15.dp), Alignment.Center) {
                if (rules.account && LocalStarMarks.current.effectiveStar(dev.nori.music.data.StarKind.SONG, song.id, song.starred)) Icon(Icons.Filled.Favorite, say.favourite, Modifier.size(15.dp), tint)
            }
            Box(Modifier.width(MARK_SLOT), Alignment.Center) { DownloadSlot(song.id, downloaded, tint) }
            // A song a jam guest asked for says so in its time's place until the host decides.
            val asked = rules.asks && song.id in LocalAsked.current.value
            Text(
                if (asked) words(R.string.jam_asked_mark) else if (song.duration > 0u) duration(song.duration.toLong()) else "",
                Modifier.widthIn(min = TIME_SLOT), textAlign = TextAlign.End,
                style = MaterialTheme.typography.bodySmall, color = if (asked) scheme.primary else tint, maxLines = 1, softWrap = false,
            )
            IconButton(onMenu, Modifier.size(40.dp)) { Icon(Icons.Filled.MoreHoriz, say.more, Modifier.size(20.dp), tint) }
        }
      }
        if (divider) Hairline(startIndent = if (number != null) Space.gutter + 40.dp else Space.gutter + 58.dp)
    }
}

/** How long the playing bars wait between steps, ms: with the frame after it, about twenty steps a second. */
private const val BARS_STEP_MS = 34L

/**
 * The bars Apple draws where a playing track's number would be. They move while the music does and
 * stand still when it is paused, which is the cue that matters: a frozen glyph beside the marked row
 * says "this one, but stopped" without a second icon.
 *
 * The phase is read in the draw phase, so a frame invalidates this 16 dp box and nothing else - no
 * recomposition and no relayout anywhere in the list. Nothing runs at all while the music is paused,
 * while this row is not composed or not [seen] (screen off, or the player over the page).
 */
@Composable
fun PlayingBars(tint: Color, modifier: Modifier = Modifier) {
    // Read here rather than threaded through every list: these bars exist on exactly one row, so this
    // is one collector on one boolean, not one per song.
    val player: dev.nori.music.app.vm.PlayerViewModel = androidx.lifecycle.viewmodel.compose.viewModel()
    val moving by player.sounding.collectAsStateWithLifecycle()
    val phase = remember { androidx.compose.runtime.mutableFloatStateOf(0f) }
    val seen = seen()
    androidx.compose.runtime.LaunchedEffect(moving, seen) {
        if (!moving || !seen) return@LaunchedEffect
        // One callback object for every frame, handed to the frame clock as it is: the millisecond
        // variants wrap it in a new lambda each frame, which is garbage for as long as the music plays.
        val tick: (Long) -> Unit = { phase.floatValue = it / 1e9f }
        // Not every display frame: each one is a frame of the whole screen, and every display frame of a
        // 120 Hz phone kept a page with this row on it at a fifth of a core. A score or so of steps a second
        // still reads as the bars moving with the music; see BARS_STEP_MS.
        while (coroutineContext.isActive) {
            androidx.compose.runtime.withFrameNanos(tick)
            kotlinx.coroutines.delay(BARS_STEP_MS)
        }
    }
    androidx.compose.foundation.Canvas(modifier) {
        val t = phase.floatValue
        val bars = 4
        val w = size.width / (bars * 2 - 1)
        for (i in 0 until bars) {
            // Four speeds that do not share a period, so the bars never fall into step and read as a meter.
            val level = 0.5f + 0.5f * kotlin.math.sin(t * (5.1f + i * 1.3f) + i * 1.7f)
            val h = size.height * (0.28f + 0.72f * if (moving) level else RESTING[i])
            drawRoundRect(
                tint,
                androidx.compose.ui.geometry.Offset(i * w * 2f, size.height - h),
                androidx.compose.ui.geometry.Size(w, h),
                androidx.compose.ui.geometry.CornerRadius(w / 2f, w / 2f),
            )
        }
    }
}

/** What the bars stand at while the music is paused: a shape, not a flat line. */
private val RESTING = floatArrayOf(0.35f, 0.8f, 0.5f, 0.65f)

/**
 * A song list wired to the configured tap, swipe and selection behaviour; every screen that lists songs uses
 * this. Each row is an item of its own, so only the rows on screen are composed, however long the list:
 * a thousand-song playlist costs a page the same few rows a ten-song album does.
 */
fun LazyListScope.songRows(
    songs: List<Song>, actions: ActionsViewModel, playingId: String?, downloaded: Set<String>, selected: Set<String>, menu: (Song) -> Unit,
    numbered: Boolean = false, cover: (Song) -> String? = { null }, keyPrefix: String = "",
    /**
     * Which of [songs] are listed, in order, as places in it: one disc of an album, a filtered view. All of
     * them when null. A tap plays the whole of [songs] from the row's own place.
     */
    rows: List<UInt>? = null,
    /**
     * Each listed row's second line where the page words them itself: an album's, which leaves the album's
     * own artist off its tracks (the core's `DiscGroup.lines`), by the row's place in [rows]. Otherwise each
     * song's own.
     */
    lines: List<String>? = null,
    /**
     * Rows keyed by song alone (the ids must be unique) that fade and slide when the list changes under
     * them - a favourite unstarred, a mix drawn again - instead of the rows below jumping up in one frame.
     */
    animated: Boolean = false,
    /** Rows that fade in when they join a list already drawn (an artist's top songs, read after the page), keyed by place. */
    appear: Boolean = false,
    /** The page's arrival ([rememberArrival]), read in each row's draw phase: the rows on screen rise in as one block. */
    arrival: State<Float>? = null,
    /** The page [songs] are the list of: a tap that plays them from the row starts that page's queue. */
    from: dev.nori.music.ffi.model.PageOrigin? = null,
) {
    val (onRight, onLeft) = actions.swipes
    val count = rows?.size ?: songs.size
    items(
        count,
        key = { k ->
            val at = rows?.get(k)?.toInt() ?: k
            if (animated) keyPrefix + songs[at].id else "$keyPrefix$at-${songs[at].id}"
        },
        contentType = { "song" },
    ) { k ->
        val at = rows?.get(k)?.toInt() ?: k
        val s = songs[at]
        val moving = animated || appear
        val sheet = LocalPlayerSheet.current
        val modifier = when {
            !moving -> Modifier
            AppMotion.reduce -> Modifier.animateItem(null, null, null)
            else -> Modifier.animateItem()
        }
        SongRow(
            s, if (numbered) null else remember(s) { cover(s) },
            onClick = { if (actions.tap(songs, at, from, playing = s.id == playingId)) sheet.open() }, onMenu = { menu(s) },
            modifier = if (arrival != null) modifier.arriving(arrival) else modifier,
            number = if (numbered) s.track.toInt() else null, playing = s.id == playingId, downloaded = s.id in downloaded,
            selected = s.id in selected, onLongClick = { actions.toggleSelected(s) },
            swipeRight = rowSwipe(onRight, s, actions), swipeLeft = rowSwipe(onLeft, s, actions),
            divider = k < count - 1,
            line = lines?.getOrNull(k) ?: s.line,
        )
    }
}

/** The icon and words a swipe setting uncovers under [song]'s row, and the action; null when that side does nothing. */
@Composable
internal fun rowSwipe(action: SwipeAction, song: Song, actions: ActionsViewModel): RowSwipe? {
    val starred = action == SwipeAction.FAVOURITE && LocalStarMarks.current.effectiveStar(dev.nori.music.data.StarKind.SONG, song.id, song.starred)
    // What it does is nori-core's (`row_swipe`); there are twenty answers in all, so each is asked once.
    // What it says is one of Say's words, read once per locale: a row allocates no text.
    val act = SwipeActs.of(action, starred, LocalRules.current.account) ?: return null
    // Made once per row and answer: a new one on every pass would make the row compose again with it.
    return remember(act, song, actions) {
        val label = say.rowSwipe(act)
        when (act) {
            dev.nori.music.ffi.library.RowSwipeAct.Queue -> RowSwipe(Icons.AutoMirrored.Filled.QueueMusic, label) { actions.enqueue(listOf(song)) }
            dev.nori.music.ffi.library.RowSwipeAct.PlayNext -> RowSwipe(Icons.AutoMirrored.Filled.PlaylistPlay, label) { actions.playNext(listOf(song)) }
            dev.nori.music.ffi.library.RowSwipeAct.Download -> RowSwipe(Icons.Filled.Download, label) { actions.download(listOf(song)) }
            is dev.nori.music.ffi.library.RowSwipeAct.Favourite -> RowSwipe(if (act.on) Icons.Filled.Favorite else Icons.Filled.HeartBroken, label) { actions.star(song, act.on) }
        }
    }
}

/** The core's answer for each swipe setting, hearted or not, asked once each. */
private object SwipeActs {
    private val made = arrayOfNulls<Any>(SwipeAction.entries.size * 4)
    private val NONE = Any()
    fun of(setting: SwipeAction, starred: Boolean, account: Boolean): dev.nori.music.ffi.library.RowSwipeAct? {
        // A slot per setting, heart and profile kind: a place to remember the answer in, nothing more.
        val i = setting.ordinal * 4 + (if (starred) 1 else 0) + (if (account) 2 else 0)
        val got = made[i] ?: (dev.nori.music.ffi.library.rowSwipe(setting, starred, account) ?: NONE).also { made[i] = it }
        return got as? dev.nori.music.ffi.library.RowSwipeAct
    }
}

/** A cover with its title under it: the tile every shelf and grid is made of. */
/**
 * [fill] is for a grid, where the cell decides the width and the artwork has to take all of it: given a
 * fixed width inside a wider cell the card hugs the left edge of it and the grid looks ragged.
 */
@Composable
fun CoverCard(title: String, subtitle: String, coverUrl: String?, size: Dp, onClick: () -> Unit, modifier: Modifier = Modifier, fill: Boolean = false, remote: Boolean = false) {
    Column((if (fill) modifier else modifier.width(size)).clickable(onClick = onClick)) {
        if (fill) Cover(coverUrl, 0.dp, Modifier.fillMaxWidth().aspectRatio(1f), radius = Radius.card)
        else Cover(coverUrl, size, radius = Radius.card)
        Text(
            title, maxLines = 1, overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.bodyMedium.copy(fontSize = 14.sp, fontWeight = FontWeight.Medium),
            modifier = Modifier.padding(top = 8.dp),
        )
        if (subtitle.isNotEmpty() || remote) RemoteLine(subtitle, remote, MaterialTheme.typography.bodySmall.copy(fontSize = 12.5f.sp))
    }
}

/**
 * A line of a row or card, led by the cloud when the item is a provider's (not in the library yet). Which provider
 * is not said: to the listener remote is remote, and a name there only pushed the line out of its width.
 * The cloud comes first so the text, not the mark, is what gets cut short.
 */
@Composable
fun RemoteLine(
    text: String, remote: Boolean, style: androidx.compose.ui.text.TextStyle, modifier: Modifier = Modifier,
    color: Color = MaterialTheme.colorScheme.onSurfaceVariant,
) {
    val tint = MaterialTheme.colorScheme.onSurfaceVariant
    Row(modifier, verticalAlignment = Alignment.CenterVertically) {
        if (remote) Icon(
            Icons.Filled.CloudDownload, say.notInLibraryYet,
            Modifier.padding(end = if (text.isEmpty()) 0.dp else 4.dp).size(with(LocalDensity.current) { style.fontSize.toDp() } + 1.dp), tint,
        )
        if (text.isNotEmpty()) Text(text, maxLines = 1, overflow = TextOverflow.Ellipsis, style = style, color = color)
    }
}

/** A round portrait, for artists. */
@Composable
fun ArtistCard(name: String, subtitle: String, coverUrl: String?, size: Dp, onClick: () -> Unit, modifier: Modifier = Modifier) {
    Column(modifier.width(size).clickable(onClick = onClick), horizontalAlignment = Alignment.CenterHorizontally) {
        Cover(coverUrl, size, radius = size / 2)
        Text(
            name, maxLines = 1, overflow = TextOverflow.Ellipsis, textAlign = TextAlign.Center,
            style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(top = 7.dp),
        )
        if (subtitle.isNotEmpty()) Text(
            subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis, textAlign = TextAlign.Center,
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
fun AlbumCard(album: Album, coverUrl: String?, size: Dp, onClick: () -> Unit, modifier: Modifier = Modifier, fill: Boolean = false) =
    CoverCard(
        album.name,
        album.subtitle,
        coverUrl, size, onClick, modifier, fill, remote = album.isExternal,
    )

@Composable
fun SectionTitle(text: String, modifier: Modifier = Modifier) = SectionHeader(text, modifier)

/**
 * Content that was not on the page at first (the songs under a hero hint) fades and rises once, as one
 * block. The body of a page is many lazy items - a row each, so a long list composes only what is on
 * screen - and they share this one clock, started when [ready] turns true: each item reads it in its draw
 * phase ([arriving]), so the run redraws the rows on screen and recomposes none of them, and a row
 * composed later (scrolled to mid-run) joins wherever the block is. Once run it stays at 1: rows scrolled
 * to afterwards are simply there.
 */
@Composable
fun rememberArrival(ready: Boolean): State<Float> {
    // With movement reduced there is no run: the body is simply there on its first frame.
    val progress = remember { Animatable(if (AppMotion.reduce) 1f else 0f) }
    LaunchedEffect(ready) {
        if (!ready || progress.value >= 1f) return@LaunchedEffect
        if (AppMotion.reduce) progress.snapTo(1f)
        else progress.animateTo(1f, tween(ARRIVE_MS, easing = androidx.compose.animation.core.FastOutSlowInEasing))
    }
    return progress.asState()
}

/** How long a page's body takes to arrive, ms. */
private const val ARRIVE_MS = 320

/** How far a page's body rises as it arrives: what the old whole-body slide (6 % of its height) was for an album. */
private val ARRIVE_RISE = 36.dp

/**
 * One item of a page's arriving body ([rememberArrival]), read in the draw phase. The alpha is applied to
 * each thing drawn rather than through a layer of its own, so a row costs no offscreen buffer while it fades.
 */
fun Modifier.arriving(arrival: State<Float>): Modifier = graphicsLayer {
    val t = arrival.value
    alpha = t
    translationY = (1f - t) * ARRIVE_RISE.toPx()
    compositingStrategy = androidx.compose.ui.graphics.CompositingStrategy.ModulateAlpha
}

@Composable
fun <T> LoadBox(load: Load<T>, modifier: Modifier = Modifier, content: @Composable (T) -> Unit) {
    // A page's content fades in over its loader instead of replacing it in one frame. Keyed on the kind
    // of state only: fresh data for a page already showing just recomposes it, with no fade.
    //
    // But only once a loader has really been seen. The dots stay invisible for their first quarter
    // second (LoadingDots), so data that arrives inside that window replaces nothing anyone saw - and
    // a fade there is not a fade over a loader, it is the page's content fading up out of the page's
    // own background while the page is still sliding in (PageMotion): a black card arriving, and then
    // the album appearing on it. Quick data snaps in; only a page that was waiting gets the fade.
    val opened = remember { android.os.SystemClock.uptimeMillis() }
    androidx.compose.animation.AnimatedContent(
        load, contentKey = { it::class },
        transitionSpec = {
            if (android.os.SystemClock.uptimeMillis() - opened < stage.quickLoadMs) {
                androidx.compose.animation.fadeIn(androidx.compose.animation.core.snap()) togetherWith
                    androidx.compose.animation.fadeOut(androidx.compose.animation.core.snap())
            } else {
                androidx.compose.animation.fadeIn(androidx.compose.animation.core.tween(260, delayMillis = 60)) togetherWith
                    androidx.compose.animation.fadeOut(androidx.compose.animation.core.tween(160))
            }
        },
        label = "load",
    ) { state ->
        when (state) {
            is Load.Ready -> content(state.data)
            is Load.Loading -> Box(modifier.fillMaxSize(), Alignment.Center) { LoadingDots() }
            is Load.Failed -> Column(modifier.fillMaxSize().padding(Space.gutter), Arrangement.Center, Alignment.CenterHorizontally) {
                Text(noteText(Note.COULD_NOT_LOAD), style = MaterialTheme.typography.titleLarge)
                Text(state.message, Modifier.padding(top = 4.dp), color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
            }
        }
    }
}


/** One of the app's notes ([Note]), read once where it is shown. */
@Composable
fun noteText(note: Note): String = remember(note) { say.note(note) }

/** Big, quiet type for an empty list. */
@Composable
fun EmptyNote(note: Note, modifier: Modifier = Modifier) = EmptyNote(noteText(note), modifier)

/** Big, quiet type for an empty list: "Nothing here yet". */
@Composable
fun EmptyNote(text: String, modifier: Modifier = Modifier) = Text(
    text, modifier.fillMaxWidth().padding(Space.gutter), textAlign = TextAlign.Center,
    style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.Medium), color = MaterialTheme.colorScheme.onSurfaceVariant,
)

/**
 * Warms artwork that is about to be needed. A server renders each thumbnail the first time it is
 * asked for, which on a real library is the better part of a second per cover; asking for the next
 * screenful while the current one is being read turns that wait into something already done. They go
 * through the same loader, decoded at the rendition's own size into memory, so a prefetched cover is
 * simply there when its view appears, whatever size that is. For a caller that works out [urls] outside
 * composition, from a scroll observer. Main thread.
 */
fun prefetchCovers(context: android.content.Context, urls: List<String?>) {
    val loader = dev.nori.music.data.CoverLoader.get(context)
    urls.forEach { url -> if (url != null) loader.prefetch(url) }
}

/**
 * The time's column: wide enough for "59:59" in the row's small type, so every ordinary track's time
 * ends in the same place. A track over an hour is wider and pushes left, which is rare enough to allow.
 */
private val TIME_SLOT = 36.dp
