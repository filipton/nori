package dev.nori.music.app.ui

import androidx.compose.runtime.setValue
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.material3.IconButton
import androidx.compose.runtime.mutableStateOf
import androidx.compose.animation.togetherWith
import androidx.compose.animation.core.animateFloat
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.composed
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.Typography
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.isSpecified
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.semantics.toggleableState
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.role
import dev.nori.music.app.R
import dev.nori.music.look.CoverLook
import dev.nori.music.data.of

/**
 * The look of the app in one file: radii, spacing, type and the handful of shapes every screen is
 * built from. Apple Music is the reference for the feel - artwork that bleeds into the page, round
 * corners everywhere, hairlines instead of boxes, floating chrome - but the widgets underneath stay
 * ordinary Material 3, because a phone that looks like iOS on purpose ends up looking wrong.
 *
 * Everything here is static: colours and brushes are values, not animations, so a page that sits on
 * screen costs nothing between frames.
 */
object Radius {
    val cover = 8.dp
    val card = 12.dp
    val tile = 18.dp
    val sheet = 26.dp
    val pill = 100.dp
}

object Space {
    /** The side margin every screen shares. Text, rows and section titles all start here. */
    val gutter = 20.dp
    val tight = 8.dp
    val row = 12.dp
    val section = 26.dp
}

val CardShape = RoundedCornerShape(Radius.card)
val TileShape = RoundedCornerShape(Radius.tile)
val PillShape = RoundedCornerShape(Radius.pill)

/**
 * Inter, as a variable font, standing in for the system face an iPhone would use. It is the single
 * biggest reason a screenshot reads as "a modern music app" rather than "an Android app": Roboto's
 * wide, loose letterforms are what makes stock Compose look like a settings screen. One 880 kB file
 * carries every weight, and each weight is a real instance of the variable font, never a synthetic
 * bold - synthesised weights are what make text look smeared.
 */
private fun inter(weight: FontWeight) = Font(
    R.font.inter, weight,
    variationSettings = FontVariation.Settings(FontVariation.weight(weight.weight)),
)

val Inter = FontFamily(inter(FontWeight.Normal), inter(FontWeight.Medium), inter(FontWeight.SemiBold), inter(FontWeight.Bold))

/**
 * Tighter and heavier than stock Material, which is what makes a music app read as a music app:
 * screen and album titles are display-weight with negative tracking, list rows are plain text, and
 * captions are small grey capitals.
 */
val NoriTypography = Typography().run {
    fun TextStyle.f(weight: FontWeight? = null, tracking: Float? = null) = copy(
        fontFamily = Inter,
        fontWeight = weight ?: fontWeight,
        letterSpacing = tracking?.sp ?: letterSpacing,
    )
    copy(
        displayLarge = displayLarge.f(FontWeight.Bold, -1.5f),
        displayMedium = displayMedium.f(FontWeight.Bold, -1.2f),
        displaySmall = displaySmall.f(FontWeight.Bold, -1f),
        headlineLarge = headlineLarge.f(FontWeight.Bold, -0.9f),
        headlineMedium = headlineMedium.f(FontWeight.Bold, -0.7f),
        headlineSmall = headlineSmall.f(FontWeight.Bold, -0.5f),
        titleLarge = titleLarge.f(FontWeight.Bold, -0.4f),
        titleMedium = titleMedium.f(FontWeight.SemiBold, -0.2f),
        titleSmall = titleSmall.f(FontWeight.SemiBold, -0.1f),
        bodyLarge = bodyLarge.f(FontWeight.Normal, -0.1f).copy(fontSize = 16.sp, lineHeight = 21.sp),
        bodyMedium = bodyMedium.f().copy(fontSize = 15.sp, lineHeight = 20.sp),
        bodySmall = bodySmall.f().copy(fontSize = 13.sp, lineHeight = 17.sp),
        labelLarge = labelLarge.f(FontWeight.SemiBold),
        labelMedium = labelMedium.f(FontWeight.Medium),
        labelSmall = labelSmall.f(FontWeight.SemiBold, 0.5f),
    )
}

/**
 * The colours a page wears, taken from its artwork: its whole [look] as nori-look dressed it (see
 * [Look]), and the wash - the cover blurred and pulled towards the page, drawn stretched over the page so
 * the colours vary the way the artwork's do instead of settling into one average.
 *
 * Two palettes are the same when their looks are and they share a wash.
 */
@androidx.compose.runtime.Immutable
class PagePalette(val look: IntArray, val wash: androidx.compose.ui.graphics.ImageBitmap? = null) {
    val fixed = FixedLook(look)
    /** What the bottom of the cover is, so the picture can dissolve into the page without a seam. */
    val edge: Color get() = Color(look[CoverLook.EDGE])
    /** The page under it. */
    val background: Color get() = Color(look[CoverLook.BACKGROUND])
    override fun equals(other: Any?): Boolean = other is PagePalette && other.wash == wash && other.look.contentEquals(look)
    override fun hashCode(): Int = look.contentHashCode() * 31 + (wash?.hashCode() ?: 0)
}

val LocalPalette = staticCompositionLocalOf<PagePalette?> { null }

/** Star changes made this session, per kind (see Library.starMarks). */
val LocalStarMarks = staticCompositionLocalOf { dev.nori.music.ffi.library.StarMarks(emptyMap(), emptyMap(), emptyMap()) }

/**
 * Star state as the screen should show it: this session's change wins over the snapshot the list was
 * painted with. Every toggle must also act on this, not on the snapshot, or the second tap undoes
 * the first one's server call instead of flipping what is on screen.
 */
fun dev.nori.music.ffi.library.StarMarks.effectiveStar(kind: dev.nori.music.data.StarKind, id: String, snapshot: Boolean): Boolean =
    of(kind, id) ?: snapshot

/**
 * The player's page, aligned to its sleeve. The cover is drawn at the sleeve's own scale behind it, so
 * the blurred copy and the sharp one are the same picture at the same size; above and below, the
 * texture's first and last rows carry on. That last part is the whole point: the sleeve's bottom edge
 * then meets a wash made of the sleeve's own bottom rows, in the same colours, and the picture runs out
 * into the page instead of ending on one. Stretched over the screen instead, the wash showed the middle
 * of the cover where the sleeve ended, and the hue jumped across a line.
 */
fun androidx.compose.ui.draw.CacheDrawScope.sleeveWash(
    palette: PagePalette, sleeveBottom: Float, sleeveHeight: Float,
): androidx.compose.ui.draw.DrawResult {
    val endY = size.height
    val wash = palette.wash
    val page = palette.background
    if (wash == null) return onDrawBehind { drawRect(page, size = Size(size.width, endY)) }
    val w = size.width.toInt().coerceAtLeast(1)
    val bottom = sleeveBottom.coerceIn(1f, endY)
    val top = (bottom - sleeveHeight).coerceAtLeast(0f).toInt()
    // The last row, carried down, is the right colour where it meets the sleeve and wrong everywhere
    // below it: the same stripes at the same strength all the way to the bottom edge. Apple's page
    // darkens and calms as it goes down - at y 2000 of `w4` it is still their red, but deeper and more
    // even than under the artwork. So the stripes give way, slowly at first, to a deeper page colour,
    // and arrive at it exactly at the bottom edge of the screen, where there is nothing to meet.
    //
    // The page ends on its own colour, not on a darkened version of it. Taking a third of the way
    // to black off the bottom was barely visible while every page was dark; now that a bright
    // record gets a bright page it split the screen in two - the record's colour across the top
    // and something close to black under the controls. The gradient below still calms the stripes;
    // it no longer changes how light the page is. Made once per page and sleeve, not per frame.
    val look = palette.look
    val floor = if (endY - bottom > 1f) Brush.verticalGradient(
        // The colours stay through the controls and settle into one only towards the bottom:
        // the owner liked them under the transport and did not want them gone, just ended.
        stage.floorStops[0] to Color(look[CoverLook.FLOOR_0]),
        stage.floorStops[1] to Color(look[CoverLook.FLOOR_22]),
        stage.floorStops[2] to Color(look[CoverLook.FLOOR_75]),
        stage.floorStops[3] to page,
        startY = bottom, endY = endY,
    ) else null
    val floorTop = Offset(0f, bottom)
    val floorSize = Size(size.width, endY - bottom)
    return onDrawBehind {
        // Rounded edges rather than rounded heights, so the three bands abut exactly with no row of page
        // colour showing between them.
        //
        // And the two plain bands each run one row on under the sleeve's, which is drawn last. Abutting is
        // exact only on whole pixels: while the sheet rises it sits at a fraction of a pixel, each band's
        // edge is anti-aliased, and two part-covered edges in the same row let the page under the sheet
        // show through - a hairline across the screen at the sleeve's bottom that came and went with the
        // fraction, half way up. The extra rows are one source row stretched, so they cost the picture
        // nothing, and on whole pixels the sleeve's band covers them.
        fun band(srcY: Int, srcH: Int, y0: Int, y1: Int) {
            if (y1 <= y0) return
            drawImage(
                wash,
                srcOffset = IntOffset(0, srcY), srcSize = IntSize(WASH_ROWS, srcH),
                dstOffset = IntOffset(0, y0), dstSize = IntSize(w, y1 - y0),
                filterQuality = FilterQuality.Low,
            )
        }
        val bottomRow = bottom.toInt()
        band(0, 1, 0, if (top > 0) top + 1 else 0)
        band(WASH_ROWS - 1, 1, if (bottomRow > top) bottomRow - 1 else bottomRow, endY.toInt())
        band(0, WASH_ROWS, top, bottomRow)
        if (floor != null) drawRect(floor, topLeft = floorTop, size = floorSize)
    }
}

/**
 * [sleeveWash] on its side: the sleeve is the left part of the screen, up to x [sleeveRight], [sleeveWidth]
 * wide and the screen's height. The blurred cover is drawn there at the sleeve's own size, and its last
 * column carries on to the right edge under the controls, calming into the page as it goes - the bottom
 * rows' job, turned, so the sleeve's soft right edge runs out into its own colours there too.
 */
fun androidx.compose.ui.draw.CacheDrawScope.sleeveWashAcross(
    palette: PagePalette, sleeveRight: Float, sleeveWidth: Float,
): androidx.compose.ui.draw.DrawResult {
    val endX = size.width
    val wash = palette.wash
    val page = palette.background
    if (wash == null) return onDrawBehind { drawRect(page) }
    val h = size.height.toInt().coerceAtLeast(1)
    val right = sleeveRight.coerceIn(1f, endX)
    val left = (right - sleeveWidth).coerceAtLeast(0f).toInt()
    val look = palette.look
    val floor = if (endX - right > 1f) Brush.horizontalGradient(
        stage.floorStops[0] to Color(look[CoverLook.FLOOR_0]),
        stage.floorStops[1] to Color(look[CoverLook.FLOOR_22]),
        stage.floorStops[2] to Color(look[CoverLook.FLOOR_75]),
        stage.floorStops[3] to page,
        startX = right, endX = endX,
    ) else null
    val floorLeft = Offset(right, 0f)
    val floorSize = Size(endX - right, size.height)
    return onDrawBehind {
        fun band(srcX: Int, srcW: Int, x0: Int, x1: Int) {
            if (x1 <= x0) return
            drawImage(
                wash,
                srcOffset = IntOffset(srcX, 0), srcSize = IntSize(srcW, WASH_ROWS),
                dstOffset = IntOffset(x0, 0), dstSize = IntSize(x1 - x0, h),
                filterQuality = FilterQuality.Low,
            )
        }
        val rightCol = right.toInt()
        band(0, 1, 0, if (left > 0) left + 1 else 0)
        band(WASH_ROWS - 1, 1, if (rightCol > left) rightCol - 1 else rightCol, endX.toInt())
        band(0, WASH_ROWS, left, rightCol)
        if (floor != null) drawRect(floor, topLeft = floorLeft, size = floorSize)
    }
}

/**
 * How the app draws and times its pages - every gradient's stops, the waits, the fades and the meter's
 * pace - as nori-core says (`stage.rs`, `nori_look::sleeve`). Read once, the first time a page draws.
 */
val stage: dev.nori.music.ffi.Stage by lazy { dev.nori.music.ffi.stage() }

/**
 * Every word the screens say, from the app's string resources ([Say]): the fixed ones read once per
 * locale, the ones with a number or a name in them made when asked.
 */
val say: Say get() = Say.current

/**
 * How much of the sleeve's height goes soft at the bottom: the same share the colour of those rows is
 * averaged out of the wash at (`nori_look::cover::MELT`).
 */
val MELT: Float get() = stage.melt

/**
 * A gradient of [color] at the core's [stops], from [startY] to [endY] - or, [across], from x [startY] to
 * [endY]. Made once per size, never per frame.
 */
fun alphaGradient(stops: List<dev.nori.music.ffi.GradientStop>, color: Color, startY: Float, endY: Float, across: Boolean = false): Brush {
    val at = Array(stops.size) { stops[it].at to color.copy(alpha = stops[it].alpha) }
    return if (across) Brush.horizontalGradient(*at, startX = startY, endX = endY) else Brush.verticalGradient(*at, startY = startY, endY = endY)
}

/** The wash texture is this many pixels a side (nori_look's `WASH_OUT`). */
private const val WASH_ROWS = CoverLook.WASH

/**
 * A hairline in the Apple sense: a dim line that starts where the text starts and never reaches the
 * right edge of the screen. Drawn, not laid out, so a long list does not pay for a divider composable.
 */
@Composable
fun Hairline(startIndent: Dp = Space.gutter) {
    val color = LocalContentColor.current.copy(alpha = 0.10f)
    Box(Modifier.fillMaxWidth().padding(start = startIndent).height(1.dp).background(color))
}

/** A section heading: big, bold, sitting on the gutter, with an optional action on the right. */
@Composable
fun SectionHeader(title: String, modifier: Modifier = Modifier, action: @Composable (RowScope.() -> Unit)? = null) {
    Row(
        modifier.fillMaxWidth().padding(start = Space.gutter, end = Space.tight, top = Space.section - 12.dp, bottom = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            title, Modifier.weight(1f),
            style = MaterialTheme.typography.titleLarge.copy(fontSize = 20.sp, fontWeight = FontWeight.SemiBold),
            maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
        action?.invoke(this)
    }
}

/** The name of a whole screen, the size Apple sets it: big, bold, sitting on the gutter. */
@Composable
fun LargeTitle(text: String, modifier: Modifier = Modifier, trailing: @Composable (RowScope.() -> Unit)? = null) {
    Row(
        modifier.fillMaxWidth().padding(start = Space.gutter, end = Space.tight, top = 8.dp, bottom = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text, Modifier.weight(1f),
            style = MaterialTheme.typography.headlineMedium.copy(fontSize = 30.sp),
            maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
        trailing?.invoke(this)
    }
}

/** Small grey capitals under a title: "2019 · ALTERNATIVE · LOSSLESS". */
@Composable
fun Caption(text: String, modifier: Modifier = Modifier, align: TextAlign = TextAlign.Start, caps: Boolean = true) {
    if (text.isEmpty()) return
    // The page's quieter text colour, read while drawing (the player's page moves under it).
    val look = LocalLook.current
    LookText(
        if (caps) remember(text) { text.uppercase() } else text, { look.color(CoverLook.ON_VARIANT) }, modifier,
        style = MaterialTheme.typography.labelSmall, textAlign = align, maxLines = 2, overflow = TextOverflow.Ellipsis,
    )
}

/**
 * The button the page is built around: a full pill, tinted with the page's accent. [prominent] is the
 * one the eye should land on (Play); the others sit on a translucent version of the same colour.
 * On a paper-white page the plates go darker so they do not wash out into the background - and the
 * darkening is continuous in the page's lightness, so a cross-fade onto white does not flip the
 * Play button black in one frame.
 */
@Composable
fun PillButton(
    text: String, icon: ImageVector?, onClick: () -> Unit, modifier: Modifier = Modifier,
    prominent: Boolean = false, enabled: Boolean = true,
) {
    // The plates lean darker as the page gets paler, continuously (nori_look::dress): looked up.
    val look = LocalLook.current
    val container = look.color(if (prominent) CoverLook.PILL else CoverLook.PILL_PLATE)
    val content = look.color(if (prominent) CoverLook.PILL_INK else CoverLook.TINT_INK)
    Surface(
        onClick = onClick, enabled = enabled, shape = PillShape, color = container, contentColor = content,
        modifier = modifier.heightIn(min = 42.dp),
    ) {
        Row(Modifier.padding(horizontal = 16.dp), Arrangement.Center, Alignment.CenterVertically) {
            if (icon != null) Icon(icon, null, Modifier.size(18.dp))
            Text(
                text, Modifier.padding(start = if (icon != null) 7.dp else 0.dp),
                style = MaterialTheme.typography.titleSmall.copy(fontSize = 15.sp), maxLines = 1,
            )
        }
    }
}


/**
 * Dresses everything inside in the colours of one cover: the page colour becomes the surface, the
 * cover's accent becomes the primary, and text colours are chosen to read on it. Screens keep using
 * `MaterialTheme.colorScheme`, so nothing below needs to know where the colours came from.
 */
@Composable
fun TintedTheme(palette: PagePalette?, content: @Composable () -> Unit) {
    val base = MaterialTheme.colorScheme
    val scheme = androidx.compose.runtime.remember(palette, base) { palette?.let { base.dressedIn(it.fixed) } ?: base }
    MaterialTheme(colorScheme = scheme) {
        androidx.compose.runtime.CompositionLocalProvider(
            LocalContentColor provides scheme.onSurface,
            LocalPalette provides palette,
            LocalLook provides (palette?.fixed ?: LocalLook.current),
        ) { content() }
    }
}

/** This scheme with the page's roles put in from [l]. */
private fun androidx.compose.material3.ColorScheme.dressedIn(l: Look): androidx.compose.material3.ColorScheme {
    val on = l.color(CoverLook.ON)
    return copy(
        background = l.color(CoverLook.BACKGROUND), surface = l.color(CoverLook.BACKGROUND),
        onBackground = on, onSurface = on, onSurfaceVariant = l.color(CoverLook.ON_VARIANT),
        surfaceVariant = l.color(CoverLook.SURFACE_VARIANT),
        surfaceContainer = l.color(CoverLook.SURFACE_CONTAINER),
        surfaceContainerHigh = l.color(CoverLook.SURFACE_CONTAINER_HIGH),
        primary = l.color(CoverLook.ACCENT), onPrimary = l.color(CoverLook.ON_PRIMARY),
        secondaryContainer = l.color(CoverLook.SECONDARY_CONTAINER), onSecondaryContainer = on,
        outlineVariant = l.color(CoverLook.OUTLINE_VARIANT),
    )
}

/**
 * Status bar icons follow the colour of the page they sit on, and go back to the app's when it leaves.
 * The look says which (a light page gets dark icons); a page that is changing colour is followed as it
 * goes, without recomposing anything.
 */
@Composable
fun SystemBarIcons(look: Look) {
    val view = androidx.compose.ui.platform.LocalView.current
    val controller = remember(view) {
        (view.context as? android.app.Activity)?.window?.let { androidx.core.view.WindowCompat.getInsetsController(it, view) }
    }
    androidx.compose.runtime.DisposableEffect(controller) {
        val before = controller?.isAppearanceLightStatusBars
        onDispose { if (before != null) controller.isAppearanceLightStatusBars = before }
    }
    LaunchedEffect(controller, look) {
        if (controller == null) return@LaunchedEffect
        androidx.compose.runtime.snapshotFlow { look.argb(CoverLook.STATUS_LIGHT) != 0 }
            .collect { controller.isAppearanceLightStatusBars = it }
    }
}

/**
 * A search or filter field as a soft rounded capsule rather than an outlined box: one tinted surface,
 * an icon, and a clear button that only exists when there is something to clear.
 */
@Composable
fun SearchField(
    value: String, onValue: (String) -> Unit, placeholder: String, modifier: Modifier = Modifier,
    testTag: String? = null, autofocus: Boolean = false, focusKey: Any = Unit,
) {
    val scheme = MaterialTheme.colorScheme
    // Asked for by hand: opening search focuses the field so the keyboard is already there.
    val focus = remember(autofocus) { if (autofocus) FocusRequester() else null }
    val keyboard = androidx.compose.ui.platform.LocalSoftwareKeyboardController.current
    LaunchedEffect(focus, focusKey) {
        if (focus == null) return@LaunchedEffect
        // The field's node is not attached on the frame this first runs, and requestFocus on an
        // unattached one throws and leaves the screen with no keyboard at all - which is what tapping
        // Search used to do about half the time. Ask again for a few frames until it takes.
        repeat(12) {
            androidx.compose.runtime.withFrameNanos {}
            if (runCatching { focus.requestFocus() }.isSuccess) {
                // Focus alone does not always raise the keyboard when it lands before the window is
                // ready; ask for it explicitly, which is the whole point of autofocus.
                keyboard?.show()
                return@LaunchedEffect
            }
        }
    }
    Surface(
        shape = PillShape, color = LocalLook.current.color(CoverLook.FIELD),
        contentColor = scheme.onSurface, modifier = modifier.fillMaxWidth(),
    ) {
        Row(Modifier.padding(horizontal = 14.dp, vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.Search, null, Modifier.size(19.dp), tint = scheme.onSurfaceVariant)
            // The padding belongs to the box, not to the field: with it on the field the placeholder sat
            // at the top of the capsule while the typed text sat in the middle of it.
            Box(Modifier.weight(1f).padding(horizontal = 8.dp, vertical = 11.dp)) {
                if (value.isEmpty()) Text(placeholder, style = MaterialTheme.typography.bodyLarge, color = scheme.onSurfaceVariant)
                androidx.compose.foundation.text.BasicTextField(
                    value, onValue, singleLine = true,
                    textStyle = MaterialTheme.typography.bodyLarge.copy(color = scheme.onSurface),
                    cursorBrush = androidx.compose.ui.graphics.SolidColor(scheme.primary),
                    modifier = Modifier.fillMaxWidth()
                        .then(if (testTag != null) Modifier.testTag(testTag) else Modifier)
                        .then(if (focus != null) Modifier.focusRequester(focus) else Modifier),
                )
            }
            if (value.isNotEmpty()) androidx.compose.material3.IconButton({ onValue("") }, Modifier.size(28.dp)) {
                Icon(Icons.Filled.Clear, say.clear, Modifier.size(17.dp), tint = scheme.onSurfaceVariant)
            }
        }
    }
}

/** A choice chip: a pill that fills with the accent when it is the one selected. */
@Composable
fun Chip(label: String, selected: Boolean, modifier: Modifier = Modifier, onClick: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Surface(
        onClick = onClick, shape = PillShape,
        color = if (selected) scheme.primary else LocalLook.current.color(CoverLook.FIELD),
        contentColor = if (selected) scheme.onPrimary else scheme.onSurface,
        modifier = modifier,
    ) {
        Text(
            label, Modifier.padding(horizontal = 14.dp, vertical = 8.dp),
            style = MaterialTheme.typography.labelLarge.copy(fontSize = 13.5f.sp), maxLines = 1,
        )
    }
}

/**
 * A slider drawn rather than assembled: a rounded track, a fill and a dot. Material's Slider brings a
 * ripple, a state layer and a value label, which on a screen of ten equalizer bands reads as ten
 * widgets instead of one curve - and costs a layer each. [centred] fills outwards from zero, which is
 * what a gain control should look like.
 */
@Composable
fun NoriSlider(
    value: Float,
    range: ClosedFloatingPointRange<Float>,
    onChange: (Float) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    centred: Boolean = false,
) {
    val scheme = MaterialTheme.colorScheme
    val track = scheme.onSurface.copy(alpha = if (enabled) 0.16f else 0.07f)
    val fill = if (enabled) scheme.primary else scheme.onSurface.copy(alpha = 0.25f)
    val knob = if (enabled) Color.White else Color(0xFFBDBDBD)
    val span = (range.endInclusive - range.start).takeIf { it > 0f } ?: 1f
    val fraction = ((value - range.start) / span).coerceIn(0f, 1f)
    val pick: (Float, Float) -> Unit = { x, w -> onChange(range.start + (x / w).coerceIn(0f, 1f) * span) }
    Box(
        modifier.fillMaxWidth().height(40.dp)
            .pointerInput(enabled, range) {
                if (!enabled) return@pointerInput
                detectHorizontalDragGestures(
                    onDragStart = { pick(it.x, size.width.toFloat()) },
                ) { change, _ -> pick(change.position.x, size.width.toFloat()) }
            }
            .pointerInput(enabled, range) {
                if (!enabled) return@pointerInput
                detectTapGestures { pick(it.x, size.width.toFloat()) }
            }
            .drawBehind {
                // UISlider's proportions: a 4 pt track and a 28 pt white knob sitting on a soft shadow.
                // The old 6 dp track with a 17 dp grey knob was Material's shape in Apple's colours.
                val h = 4.dp.toPx()
                val y = (size.height - h) / 2f
                val radius = androidx.compose.ui.geometry.CornerRadius(h / 2f, h / 2f)
                drawRoundRect(track, androidx.compose.ui.geometry.Offset(0f, y), androidx.compose.ui.geometry.Size(size.width, h), radius)
                val from = if (centred) size.width * ((0f - range.start) / span).coerceIn(0f, 1f) else 0f
                val to = size.width * fraction
                drawRoundRect(
                    fill, androidx.compose.ui.geometry.Offset(minOf(from, to), y),
                    androidx.compose.ui.geometry.Size(kotlin.math.abs(to - from), h), radius,
                )
                val r = 14.dp.toPx()
                val cx = to.coerceIn(r, size.width - r)
                drawCircle(Color.Black.copy(alpha = if (enabled) 0.18f else 0.08f), r + 1.dp.toPx(), androidx.compose.ui.geometry.Offset(cx, size.height / 2f + 1.5f.dp.toPx()))
                drawCircle(knob, r, androidx.compose.ui.geometry.Offset(cx, size.height / 2f))
            },
    )
}

/**
 * A form field with the app's corners and no hard outline: a soft filled capsule-ish box, the way a
 * settings form looks on iOS. It keeps Material's text field underneath, so labels, password masking
 * and keyboard options all behave exactly as before - only the frame changes.
 */
@Composable
fun FormField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    label: @Composable (() -> Unit)? = null,
    placeholder: @Composable (() -> Unit)? = null,
    supportingText: @Composable (() -> Unit)? = null,
    singleLine: Boolean = false,
    minLines: Int = 1,
    visualTransformation: androidx.compose.ui.text.input.VisualTransformation = androidx.compose.ui.text.input.VisualTransformation.None,
    keyboardOptions: androidx.compose.foundation.text.KeyboardOptions = androidx.compose.foundation.text.KeyboardOptions.Default,
) {
    val scheme = MaterialTheme.colorScheme
    val filled = LocalLook.current.color(CoverLook.FORM)
    androidx.compose.material3.OutlinedTextField(
        value, onValueChange, modifier, label = label, placeholder = placeholder, supportingText = supportingText,
        singleLine = singleLine, minLines = minLines,
        visualTransformation = visualTransformation, keyboardOptions = keyboardOptions,
        shape = CardShape,
        colors = androidx.compose.material3.OutlinedTextFieldDefaults.colors(
            focusedContainerColor = filled, unfocusedContainerColor = filled,
            focusedBorderColor = scheme.primary.copy(alpha = 0.6f),
            unfocusedBorderColor = Color.Transparent,
        ),
    )
}

/**
 * The list row every browsing screen shares: something on the left, a title (and maybe a second line),
 * a value or a chevron on the right, and a hairline that starts where the text does. Genres, decades,
 * folders, playlists and stations all used to draw their own row, each with its own padding, which is
 * what made the library feel like several apps stitched together.
 */
@Composable
fun NavRow(
    title: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    trailing: String? = null,
    leading: (@Composable () -> Unit)? = null,
    chevron: Boolean = false,
    divider: Boolean = true,
    action: (@Composable RowScope.() -> Unit)? = null,
) {
    val scheme = MaterialTheme.colorScheme
    Column(modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().clickable(onClick = onClick)
                .padding(start = Space.gutter, end = if (action != null) 4.dp else Space.gutter, top = 11.dp, bottom = 11.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (leading != null) { leading(); Spacer(Modifier.size(12.dp)) }
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                if (!subtitle.isNullOrEmpty()) Text(
                    subtitle, style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
            }
            if (!trailing.isNullOrEmpty()) Text(trailing, style = MaterialTheme.typography.bodyMedium, color = scheme.onSurfaceVariant)
            action?.invoke(this)
            if (chevron) Icon(
                Icons.AutoMirrored.Filled.KeyboardArrowRight, null,
                Modifier.padding(start = 6.dp).size(19.dp), tint = scheme.onSurfaceVariant.copy(alpha = 0.7f),
            )
        }
        if (divider) Hairline(startIndent = Space.gutter + (if (leading != null) 60.dp else 0.dp))
    }
}

/** A row that does something rather than going somewhere: "New playlist", "Import M3U…". */
@Composable
fun ActionRow(title: String, icon: ImageVector, onClick: () -> Unit, divider: Boolean = true) {
    Column {
        Row(
            Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = Space.gutter, vertical = 13.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(icon, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.primary)
            Text(
                title, Modifier.padding(start = 12.dp),
                style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.primary,
            )
        }
        if (divider) Hairline(startIndent = Space.gutter + 32.dp)
    }
}

/**
 * A round, softly filled button: the small actions either side of a page's Play pill.
 *
 * [lit] fills the whole disc with the page's accent and its glyph with the accent's own ink - that is
 * Shuffle while shuffle is on, and it has to read as pressed at a glance. A stronger plate was not
 * enough: on a paper-white page it came out as a few percent more grey. [selected] is the quieter
 * plate, under a heart that is on, where the filled glyph already says it. Plate strength tracks page
 * lightness continuously (same reason as [PillButton]) so a swipe onto white does not snap.
 *
 * [iconModifier] reaches the glyph alone, for an animation that must leave the disc it sits in still.
 */
@Composable
fun CircleButton(
    icon: ImageVector, description: String, modifier: Modifier = Modifier,
    enabled: Boolean = true, selected: Boolean = false, lit: Boolean = false,
    iconModifier: Modifier = Modifier, onClick: () -> Unit,
) {
    // Keep the plate and icon at full strength while disabled: washing them out made Shuffle look
    // absent on a dark page, so the row still "popped" when Play became tappable. The plates are the
    // look's (nori_look::dress): lighter on paper than on a dark page, not heavier - at 16 % black the
    // heart and "..." discs read as grey slabs on a white page.
    val look = LocalLook.current
    val plate = look.color(when { lit -> CoverLook.ACCENT; selected -> CoverLook.CIRCLE_SELECTED; else -> CoverLook.CIRCLE_PLATE })
    Surface(
        onClick = onClick, enabled = enabled, shape = androidx.compose.foundation.shape.CircleShape,
        color = plate,
        contentColor = look.color(if (lit) CoverLook.ON_PRIMARY else CoverLook.TINT_INK),
        modifier = modifier.size(46.dp),
    ) { Box(Modifier.fillMaxSize(), Alignment.Center) { Icon(icon, description, Modifier.size(20.dp).then(iconModifier)) } }
}

/**
 * Heart that fills and gives a short jump when favourited. [starred] is what is shown; the jump runs
 * on a change, not on every recomposition of an already-hearted song.
 */
@Composable
fun FavoriteHeart(
    starred: Boolean,
    modifier: Modifier = Modifier,
    size: Dp = 22.dp,
    tint: Color = MaterialTheme.colorScheme.primary,
    muted: Color = tint.copy(alpha = 0.75f),
    onClick: () -> Unit,
) {
    val plain = reduceMotion()
    val scale = remember { androidx.compose.animation.core.Animatable(1f) }
    var ready by remember { mutableStateOf(false) }
    LaunchedEffect(starred) {
        if (!ready) { ready = true; return@LaunchedEffect }
        if (plain) return@LaunchedEffect
        scale.snapTo(1f)
        // Stiffer springs than they were: same jump, a beat quicker - the old one hung at the top.
        scale.animateTo(1.28f, androidx.compose.animation.core.spring(dampingRatio = 0.42f, stiffness = 1350f))
        scale.animateTo(1f, androidx.compose.animation.core.spring(dampingRatio = 0.55f, stiffness = 900f))
    }
    IconButton(onClick, modifier) {
        Icon(
            if (starred) Icons.Filled.Favorite else Icons.Filled.FavoriteBorder,
            if (starred) say.removeFromFavourites else say.addToFavourites,
            Modifier.size(size).graphicsLayer { scaleX = scale.value; scaleY = scale.value },
            tint = if (starred) tint else muted,
        )
    }
}

/** Favourite as a [CircleButton] with the same fill/jump as [FavoriteHeart]. */
@Composable
fun FavoriteCircle(starred: Boolean, modifier: Modifier = Modifier, onClick: () -> Unit) {
    val plain = reduceMotion()
    val scale = remember { androidx.compose.animation.core.Animatable(1f) }
    var ready by remember { mutableStateOf(false) }
    LaunchedEffect(starred) {
        if (!ready) { ready = true; return@LaunchedEffect }
        if (plain) return@LaunchedEffect
        scale.snapTo(1f)
        // Stiffer springs than they were: same jump, a beat quicker - the old one hung at the top.
        scale.animateTo(1.22f, androidx.compose.animation.core.spring(dampingRatio = 0.42f, stiffness = 1350f))
        scale.animateTo(1f, androidx.compose.animation.core.spring(dampingRatio = 0.55f, stiffness = 900f))
    }
    // The jump is the heart's alone. It used to scale the whole Box, disc included, so the circle
    // around the heart bulged with it; only the glyph moves now, and the plate holds its place.
    CircleButton(
        if (starred) Icons.Filled.Favorite else Icons.Filled.FavoriteBorder,
        if (starred) say.removeFromFavourites else say.favourite,
        modifier, selected = starred,
        iconModifier = Modifier.graphicsLayer { scaleX = scale.value; scaleY = scale.value },
        onClick = onClick,
    )
}

/** The circle that holds whatever did not fit beside a page's Play button. */
@Composable
fun MoreCircle(items: List<Pair<String, () -> Unit>>, modifier: Modifier = Modifier) {
    val open = androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }
    Box(modifier) {
        CircleButton(Icons.Filled.MoreHoriz, say.more) { open.value = true }
        androidx.compose.material3.DropdownMenu(open.value, { open.value = false }) {
            items.forEach { (label, action) ->
                androidx.compose.material3.DropdownMenuItem({ Text(label) }, { action(); open.value = false })
            }
        }
    }
}


/**
 * Whether movement should be kept to a minimum: the app's own switch, or the system's animations being
 * turned off (Developer options, or the accessibility setting some people rely on). Read it rather than
 * hard-coding durations, so "reduce motion" means the same thing everywhere.
 */
@Composable
fun reduceMotion(): Boolean {
    val prefs by (androidx.lifecycle.viewmodel.compose.viewModel<dev.nori.music.app.vm.SettingsViewModel>()).prefs.collectAsStateWithLifecycle()
    val context = androidx.compose.ui.platform.LocalContext.current
    val systemOff = androidx.compose.runtime.remember {
        android.provider.Settings.Global.getFloat(context.contentResolver, android.provider.Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
    }
    // The rule is the core's; asked only when one of its three answers changes.
    return androidx.compose.runtime.remember(prefs.reduceMotion, prefs.ignoreSystemMotion, systemOff) {
        dev.nori.music.ffi.motionReduced(prefs.reduceMotion, prefs.ignoreSystemMotion, systemOff)
    }
}

/**
 * How fast every animation in the app runs. Compose scales each one by whatever MotionDurationScale is
 * in its coroutine's context, and by default that is Android's animator duration scale - so with system
 * animations off, every tween and spring in the app finished on its first frame. The activity starts
 * the whole composition with this object in its context instead (see MainActivity), so every animation
 * there is, ours and the libraries', reads it: the system's scale normally, full speed when the user
 * asked this app to animate regardless of the rest of the phone.
 */
object AppMotion : androidx.compose.ui.MotionDurationScale {
    // On until the settings say otherwise, as they do by default: the first frames, composed before App
    // has read them, would otherwise run at Android's scale and finish on the spot.
    @Volatile var force = true

    /**
     * The user's reduce-motion answer, for code that must not look it up per call - a cover in a grid
     * of a hundred cannot each collect the settings. Kept current by App; [reduceMotion] is the same
     * answer for everything else.
     */
    @Volatile var reduce = false
    // A static read, and the process is told when the setting changes: nothing to observe.
    override val scaleFactor: Float get() = if (force) 1f else android.animation.ValueAnimator.getDurationScale()
}


/**
 * A switch drawn the way iOS draws one: UISwitch's 51 by 31 pt track and a 27 pt white thumb on a
 * soft shadow, the track filling with the page's accent when on. Material's switch - a thin outlined
 * pill whose thumb grows when it is on - was the most Android-looking thing left in settings.
 *
 * The thumb slides when it is tapped, which is the one kind of motion this app allows: something the
 * user touched, answering. Nothing moves otherwise.
 */
@Composable
fun NoriSwitch(checked: Boolean, onCheckedChange: ((Boolean) -> Unit)?, modifier: Modifier = Modifier, enabled: Boolean = true) {
    val scheme = MaterialTheme.colorScheme
    val t by androidx.compose.animation.core.animateFloatAsState(
        if (checked) 1f else 0f, androidx.compose.animation.core.tween(if (reduceMotion()) 0 else 180), label = "switch",
    )
    val on = scheme.primary
    val off = LocalLook.current.color(CoverLook.SWITCH_OFF)
    val track = androidx.compose.ui.graphics.lerp(off, on, t).let { if (enabled) it else it.copy(alpha = 0.4f) }
    Box(
        modifier.size(width = 51.dp, height = 31.dp)
            .then(
                if (onCheckedChange != null) Modifier.clickable(
                    enabled = enabled,
                    interactionSource = remember { androidx.compose.foundation.interaction.MutableInteractionSource() },
                    indication = null,
                ) { onCheckedChange(!checked) } else Modifier,
            )
            .semantics { role = androidx.compose.ui.semantics.Role.Switch; toggleableState = androidx.compose.ui.state.ToggleableState(checked) }
            .drawBehind {
                val h = size.height
                drawRoundRect(track, cornerRadius = androidx.compose.ui.geometry.CornerRadius(h / 2f, h / 2f))
                val r = 13.5f.dp.toPx()
                val pad = 2.dp.toPx()
                val cx = pad + r + (size.width - 2 * (pad + r)) * t
                drawCircle(Color.Black.copy(alpha = 0.16f), r + 0.5f.dp.toPx(), androidx.compose.ui.geometry.Offset(cx, h / 2f + 1.dp.toPx()))
                drawCircle(if (enabled) Color.White else Color(0xFFE0E0E0), r, androidx.compose.ui.geometry.Offset(cx, h / 2f))
            },
    )
}

/** A decibel figure with its sign, one decimal, never "-0.0" (nori-core's `fmt::signed_db`). */
fun signedDb(db: Float): String = dev.nori.music.text.Fmt.signedDb(db)


/**
 * Something is on its way: three dots breathing one after another, the mark Apple's lyrics show for an
 * instrumental break. Quiet on purpose - it says "coming", not "look at me". It stays invisible for the
 * first quarter-second, so anything that arrives quickly never shows a loader at all, and then fades
 * in rather than appearing. Only animates while it is on screen; with reduce motion it holds still.
 */
@Composable
fun LoadingDots(modifier: Modifier = Modifier, dot: androidx.compose.ui.unit.Dp = 7.dp, color: Color = Color.Unspecified) {
    // The page's text colour, read while drawing: a page changing colour under it only redraws the dots.
    val look = LocalLook.current
    val plain = AppMotion.reduce
    val appear = remember { androidx.compose.animation.core.Animatable(0f) }
    androidx.compose.runtime.LaunchedEffect(Unit) {
        kotlinx.coroutines.delay(250)
        appear.animateTo(1f, androidx.compose.animation.core.tween(350))
    }
    val phase = if (plain) null else androidx.compose.animation.core.rememberInfiniteTransition(label = "dots").animateFloat(
        0f, 1f,
        androidx.compose.animation.core.infiniteRepeatable(androidx.compose.animation.core.tween(1300, easing = androidx.compose.animation.core.LinearEasing)),
        label = "dots",
    )
    androidx.compose.foundation.Canvas(modifier.size(dot * 4.4f, dot).graphicsLayer { alpha = appear.value }) {
        val r = size.height / 2f
        val gap = (size.width - size.height * 3f) / 2f
        val ink = if (color.isSpecified) color else look.color(dev.nori.music.look.CoverLook.ON)
        for (i in 0..2) {
            // Each dot swells and brightens in turn, a third of a cycle behind the one before it.
            val t = phase?.value?.let { ((it - i / 3f) % 1f + 1f) % 1f } ?: 0.5f
            val pulse = 0.5f - 0.5f * kotlin.math.cos(t * 2f * Math.PI.toFloat())
            drawCircle(
                ink.copy(alpha = 0.22f + 0.5f * pulse),
                radius = r * (0.78f + 0.22f * pulse),
                center = androidx.compose.ui.geometry.Offset(r + i * (size.height + gap), r),
            )
        }
    }
}

/**
 * A soft sheen gliding across a placeholder while its picture is on the way, so a slow cover reads as
 * loading rather than missing. Like [LoadingDots] it waits a quarter-second before showing and fades
 * in, so a cover that comes from the cache never shimmers. Draw-phase only: a running sheen redraws
 * one layer and recomposes nothing, and when [active] goes false it stops entirely. [leaving]: the load
 * is over with nothing to cover the sheen, so it fades out (in [SHEEN_LEAVE_MS]) rather than vanishing;
 * the caller drops it after that.
 */
/** How long a sheen whose load is over takes to fade out. */
const val SHEEN_LEAVE_MS = 300

fun Modifier.loadingSheen(active: Boolean, leaving: Boolean = false): Modifier = if (!active) this else composed {
    val appear = remember { androidx.compose.animation.core.Animatable(0f) }
    androidx.compose.runtime.LaunchedEffect(leaving) {
        if (leaving) appear.animateTo(0f, androidx.compose.animation.core.tween(SHEEN_LEAVE_MS))
        else {
            kotlinx.coroutines.delay(250)
            appear.animateTo(1f, androidx.compose.animation.core.tween(400))
        }
    }
    val plain = AppMotion.reduce
    val sweep = if (plain) null else androidx.compose.animation.core.rememberInfiniteTransition(label = "sheen").animateFloat(
        0f, 1f,
        androidx.compose.animation.core.infiniteRepeatable(
            androidx.compose.animation.core.tween(1500, easing = androidx.compose.animation.core.FastOutSlowInEasing),
            initialStartOffset = androidx.compose.animation.core.StartOffset(0),
        ),
        label = "sheen",
    )
    val look = LocalLook.current
    drawWithCache {
        // One band, made once per size and colour and slid across by translation: the band used to be a
        // new gradient on every frame of every loading cover.
        val color = look.color(dev.nori.music.look.CoverLook.ON)
        val band = size.width * 0.9f
        val sheen = Brush.linearGradient(
            0f to Color.Transparent, 0.5f to color.copy(alpha = 0.10f), 1f to Color.Transparent,
            start = androidx.compose.ui.geometry.Offset(-band / 2f, 0f),
            end = androidx.compose.ui.geometry.Offset(band / 2f, size.height),
        )
        val still = color.copy(alpha = 0.05f)
        onDrawWithContent {
            drawContent()
            val a = appear.value
            if (a <= 0f) return@onDrawWithContent
            if (sweep == null) { drawRect(still, alpha = a); return@onDrawWithContent }
            val x = -band + sweep.value * (size.width + band * 2f)
            translate(left = x) {
                drawRect(sheen, topLeft = androidx.compose.ui.geometry.Offset(-x, 0f), size = size, alpha = a)
            }
        }
    }
}

/**
 * Play, pause, or "starting": the glyph changes by cross-fading with a slight scale, never by swapping
 * in one frame. The spinner only comes in when the wait is long enough to notice - past 300 ms - since
 * most skips start playing within that, and a spinner flicking in and out of the pause button for a
 * frame was one of the things that made skipping feel rough.
 *
 * While it waits for that, the wait is "playing": the player is not playing yet, but it is going to
 * (that is what [buffering] means - waiting *with* play-when-ready), and showing the play arrow in the
 * meantime said "paused" for a fraction of a second after every skip. A run of quick skips flashed it
 * on and off with every press.
 */
@Composable
fun PlayPauseGlyph(
    playing: Boolean, buffering: Boolean, size: androidx.compose.ui.unit.Dp, spinner: androidx.compose.ui.unit.Dp,
    /** The glyph's colour read while drawing; the content colour when null. */
    tint: androidx.compose.ui.graphics.ColorProducer? = null,
) {
    var busy by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }
    androidx.compose.runtime.LaunchedEffect(buffering) {
        if (buffering) kotlinx.coroutines.delay(stage.spinnerAfterMs)
        busy = buffering
    }
    // Which glyph is the core's (`transport_glyph`), asked over JNI when one of the three changes.
    val glyph = androidx.compose.runtime.remember(playing, buffering, busy) {
        dev.nori.music.ffi.TransportGlyph.entries[dev.nori.music.look.CoverLook.transportGlyph(playing, buffering, busy)]
    }
    androidx.compose.animation.AnimatedContent(
        glyph,
        transitionSpec = {
            (androidx.compose.animation.fadeIn(androidx.compose.animation.core.tween(180)) +
                androidx.compose.animation.scaleIn(androidx.compose.animation.core.tween(180), initialScale = 0.8f)) togetherWith
                (androidx.compose.animation.fadeOut(androidx.compose.animation.core.tween(140)) +
                    androidx.compose.animation.scaleOut(androidx.compose.animation.core.tween(140), targetScale = 0.8f))
        },
        contentAlignment = Alignment.Center,
        label = "playPause",
    ) { g ->
        Box(Modifier.size(size), Alignment.Center) {
            when (g) {
                dev.nori.music.ffi.TransportGlyph.SPINNER -> androidx.compose.material3.CircularProgressIndicator(Modifier.size(spinner), color = tint?.invoke() ?: androidx.compose.material3.LocalContentColor.current, strokeWidth = 2.dp)
                dev.nori.music.ffi.TransportGlyph.PAUSE -> if (tint != null) LookIcon(Icons.Filled.Pause, say.pause, Modifier.fillMaxSize(), tint)
                    else androidx.compose.material3.Icon(Icons.Filled.Pause, say.pause, Modifier.fillMaxSize())
                else -> if (tint != null) LookIcon(Icons.Filled.PlayArrow, say.play, Modifier.fillMaxSize(), tint)
                    else androidx.compose.material3.Icon(Icons.Filled.PlayArrow, say.play, Modifier.fillMaxSize())
            }
        }
    }
}
