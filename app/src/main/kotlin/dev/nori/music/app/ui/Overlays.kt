package dev.nori.music.app.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.sizeIn
import androidx.compose.material3.AlertDialogDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ProvideTextStyle
import androidx.compose.material3.Surface
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SheetValue
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.layout.layout
import kotlin.math.roundToInt
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.withContext

/*
 * Every dialog, sheet and menu the app puts up, moving in and out on the app's own clock.
 *
 * A Compose Dialog is a window of its own, and Android animates windows by the "Window animation scale"
 * and dims behind them by the same clock: with that scale at 0 a dialog appeared and vanished on one
 * frame, whatever the app's "Animate anyway" said. The app's theme (res/values/themes.xml) gives every
 * dialog window no animation and no dim, and the dialogs here draw their own scrim and move their own
 * content, under AppMotion like the rest of the app.
 *
 * Leaving is the other half: a dialog composed as `if (open) AlertDialog(...)` is gone the frame `open`
 * turns false, so there is nothing left to animate. [NoriDialog] and [NoriSheet] take what they show
 * (or whether) and keep the last of it on screen while it leaves; only then do they drop the window.
 * Closed, each is one remembered state and an early return: no window, no effect, no frame.
 */

/** The app's settle: most of the travel early, no overshoot (PageMotion's). */
private val Settle = CubicBezierEasing(0.2f, 0f, 0f, 1f)
private const val ENTER_MS = 240
private const val EXIT_MS = 170
/** With motion reduced: a short fade, never a cut. */
private const val PLAIN_MS = 120

/** How a dialog arrives: a card in the middle over a scrim, or a page over the whole screen. */
enum class DialogStyle { Card, Page }

/** [NoriDialog] with nothing to hand its content: shown while [visible]. */
@Composable
fun NoriDialog(
    visible: Boolean,
    onDismissRequest: () -> Unit,
    style: DialogStyle = DialogStyle.Card,
    scrim: Float = 0.4f,
    content: @Composable () -> Unit,
) = NoriDialog(if (visible) Unit else null, onDismissRequest, style, scrim) { content() }

/**
 * A dialog showing [value] while it is not null, animating in when it becomes one and out when it goes
 * back to null - showing the last value until it has left. [content] is what sits on the scrim: an
 * [AlertCard], a picture, or (as a [DialogStyle.Page]) a whole screen.
 */
@Composable
fun <T : Any> NoriDialog(
    value: T?,
    onDismissRequest: () -> Unit,
    style: DialogStyle = DialogStyle.Card,
    scrim: Float = 0.4f,
    content: @Composable (T) -> Unit,
) {
    // Set when the value arrives, cleared once the exit has played: the only state kept while closed.
    var held by remember { mutableStateOf<T?>(null) }
    SideEffect { if (value != null && held !== value) held = value }
    val shown = value ?: held ?: return
    val open = value != null
    val dismiss by rememberUpdatedState(onDismissRequest)
    val live by rememberUpdatedState(open)
    Dialog(
        onDismissRequest = { if (live) dismiss() },
        // Our own window over the whole screen, so the scrim is ours and fades with the card; taps on it
        // are read below rather than by the window.
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false, dismissOnClickOutside = false),
    ) {
        val shownAt = remember { Animatable(0f) }
        LaunchedEffect(open) {
            val plain = AppMotion.reduce
            val ms = if (plain) PLAIN_MS else if (open) ENTER_MS else EXIT_MS
            // The dialog's composition is the window's own; run on the app's clock whatever it inherited.
            withContext(AppMotion) {
                shownAt.animateTo(if (open) 1f else 0f, tween(ms, easing = if (plain) LinearEasing else Settle))
            }
            if (!open) held = null
        }
        val plain = AppMotion.reduce
        Box(
            Modifier
                .fillMaxSize()
                .drawBehind { drawRect(Color.Black, alpha = scrim * shownAt.value) }
                .then(if (style == DialogStyle.Card) Modifier.pointerInput(Unit) { detectTapGestures { if (live) dismiss() } } else Modifier)
                // A leaving dialog takes no more taps: a second press on "Create" would create twice.
                .pointerInput(open) {
                    if (!open) awaitPointerEventScope {
                        while (true) awaitPointerEvent(PointerEventPass.Initial).changes.forEach { it.consume() }
                    }
                },
            contentAlignment = Alignment.Center,
        ) {
            val card = when (style) {
                DialogStyle.Card -> Modifier.systemBarsPadding().imePadding().padding(horizontal = 24.dp, vertical = 24.dp).widthIn(max = 560.dp)
                    // The card's own taps stay the card's.
                    .pointerInput(Unit) { detectTapGestures { } }
                DialogStyle.Page -> Modifier.fillMaxSize()
            }
            Box(
                card.graphicsLayer {
                    val p = shownAt.value
                    alpha = p
                    if (!plain) when (style) {
                        DialogStyle.Card -> { val s = 0.92f + 0.08f * p; scaleX = s; scaleY = s }
                        DialogStyle.Page -> translationY = (1f - p) * size.height / 24f
                    }
                },
                propagateMinConstraints = style == DialogStyle.Page,
            ) {
                content(shown)
            }
        }
    }
}

/**
 * The card of a Material alert dialog, drawn in place inside a [NoriDialog] rather than in a window of
 * its own (Material 1.4 keeps its own card behind its window). Same measures as Material's: 280 to 560
 * dp wide, 24 dp in, the title over the text over the buttons at the end.
 */
@Composable
fun AlertCard(
    confirmButton: @Composable () -> Unit,
    modifier: Modifier = Modifier,
    dismissButton: (@Composable () -> Unit)? = null,
    title: (@Composable () -> Unit)? = null,
    text: (@Composable () -> Unit)? = null,
) {
    Surface(
        modifier.sizeIn(minWidth = 280.dp, maxWidth = 560.dp),
        shape = AlertDialogDefaults.shape,
        color = AlertDialogDefaults.containerColor,
        tonalElevation = AlertDialogDefaults.TonalElevation,
    ) {
        Column(Modifier.padding(24.dp)) {
            title?.let {
                CompositionLocalProvider(LocalContentColor provides AlertDialogDefaults.titleContentColor) {
                    ProvideTextStyle(MaterialTheme.typography.headlineSmall) { Box(Modifier.padding(bottom = 16.dp)) { it() } }
                }
            }
            text?.let {
                CompositionLocalProvider(LocalContentColor provides AlertDialogDefaults.textContentColor) {
                    ProvideTextStyle(MaterialTheme.typography.bodyMedium) { Box(Modifier.weight(1f, fill = false).padding(bottom = 24.dp)) { it() } }
                }
            }
            CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.primary) {
                ProvideTextStyle(MaterialTheme.typography.labelLarge) {
                    Row(Modifier.align(Alignment.End), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        dismissButton?.invoke()
                        confirmButton()
                    }
                }
            }
        }
    }
}

/**
 * A bottom sheet showing [value] while it is not null. Material's sheet slides itself in, and out when
 * it is swiped or its scrim tapped; a sheet whose caller simply stops composing it (a menu item that
 * closes the menu) vanished instead. Here the value going null slides the sheet down first.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun <T : Any> NoriSheet(
    value: T?,
    onDismissRequest: () -> Unit,
    skipPartiallyExpanded: Boolean = true,
    content: @Composable ColumnScope.(T) -> Unit,
) {
    var held by remember { mutableStateOf<T?>(null) }
    SideEffect { if (value != null && held !== value) held = value }
    val shown = value ?: held ?: return
    val state = rememberModalBottomSheetState(skipPartiallyExpanded)
    val open = value != null
    LaunchedEffect(open) {
        withContext(AppMotion) {
            if (!open) { state.hide(); held = null }
            // Opened again while it was on its way down.
            else if (state.targetValue == SheetValue.Hidden && state.currentValue != SheetValue.Hidden) state.show()
        }
    }
    val dismiss by rememberUpdatedState(onDismissRequest)
    val live by rememberUpdatedState(open)
    // In the app's own colours, its plate and its text alike, also when opened from a page dressed in a
    // cover's (the plate is Material's, and a cover's text on it may not read).
    val scheme = LocalAppScheme.current ?: MaterialTheme.colorScheme
    val look = remember(scheme) { FixedLook(plainLook(scheme)) }
    MaterialTheme(colorScheme = scheme) {
        CompositionLocalProvider(LocalLook provides look) {
            ModalBottomSheet(onDismissRequest = { if (live) dismiss() }, sheetState = state) { content(shown) }
        }
    }
}

/** [NoriSheet] shown while [visible]. */
@Composable
fun NoriSheet(visible: Boolean, onDismissRequest: () -> Unit, skipPartiallyExpanded: Boolean = true, content: @Composable ColumnScope.() -> Unit) =
    NoriSheet(if (visible) Unit else null, onDismissRequest, skipPartiallyExpanded) { content() }

/**
 * A bar that takes its place in the layout while [value] is not null - the selection bar over the mini
 * player. It opens its own height and fades in on the app's clock (in: 240 ms, out: 170 ms, a 120 ms
 * fade with motion reduced, like [NoriDialog]), rising from behind what is under it, and closes the
 * same way, showing the last value while it leaves; what is under it moves with the room it takes
 * rather than jumping. A leaving bar takes no taps. Closed, it is one remembered state and an early return.
 */
@Composable
fun <T : Any> NoriBar(value: T?, modifier: Modifier = Modifier, content: @Composable (T) -> Unit) {
    var held by remember { mutableStateOf<T?>(null) }
    SideEffect { if (value != null && held !== value) held = value }
    val shown = value ?: held ?: return
    val open = value != null
    val shownAt = remember { Animatable(0f) }
    LaunchedEffect(open) {
        val plain = AppMotion.reduce
        val ms = if (plain) PLAIN_MS else if (open) ENTER_MS else EXIT_MS
        withContext(AppMotion) {
            shownAt.animateTo(if (open) 1f else 0f, tween(ms, easing = if (plain) LinearEasing else Settle))
        }
        if (!open) held = null
    }
    Box(
        modifier
            .clipToBounds()
            // As much of its height as it has opened, read at layout: the bar's top edge rises with it.
            .layout { measurable, constraints ->
                val p = measurable.measure(constraints)
                layout(p.width, (p.height * shownAt.value).roundToInt()) { p.placeRelative(0, 0) }
            }
            .graphicsLayer { alpha = shownAt.value }
            .pointerInput(open) {
                if (!open) awaitPointerEventScope {
                    while (true) awaitPointerEvent(PointerEventPass.Initial).changes.forEach { it.consume() }
                }
            },
    ) { content(shown) }
}
