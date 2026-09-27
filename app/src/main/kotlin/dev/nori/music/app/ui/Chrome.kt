package dev.nori.music.app.ui

import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.background
import androidx.compose.ui.draw.drawBehind
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.Column
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.graphics.toArgb
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.draw.drawWithCache
import dev.nori.music.look.CoverLook
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.ui.composed
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.PointerInputChange
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.graphics.vector.ImageVector
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.first
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.foundation.layout.Spacer
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.FastForward
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.material.icons.filled.Favorite
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.PlayerViewModel

/**
 * The chrome that never leaves: what is playing, and where to go. Apple Music stacks them into one
 * floating slab with a rounded top and a hairline between the two halves, and the page scrolls
 * underneath it. That is what this is - one surface, two rows, no boxes and no Material indicator pill.
 *
 * It is drawn in two layers. This one, under the player sheet, is the mini player (the sheet grows out
 * of it, so it is simply covered as the sheet rises) with room left below it for the tab bar. The tab
 * bar is [TabBar], over the sheet, so that it can slide down out of the way as the player opens rather
 * than vanish under it in one frame. The two are split because only the tab bar can move: the mini
 * player holds the drag that opens the player, and moving the element a drag started on corrupts it.
 */
@Composable
fun BottomChrome(player: PlayerViewModel, actions: ActionsViewModel, onOpenPlayer: () -> Unit, tabsHeight: androidx.compose.ui.unit.Dp, look: Look) {
    // A soft wash under the chrome so the list fades out as it passes behind it. Apple gets this from
    // blurring what is behind the bars; one vertical gradient costs nothing and reads much the same.
    // Made once per size and page colour, not on every draw.
    Column(
        Modifier.drawWithCache {
            val fade = androidx.compose.ui.graphics.Brush.verticalGradient(
                0f to Color.Transparent, 0.45f to look.color(CoverLook.CHROME_FADE), 1f to look.color(CoverLook.CHROME_PAGE),
            )
            onDrawBehind { drawRect(fade) }
        },
    ) {
        SelectionBar(actions)
        Box(Modifier.padding(horizontal = 10.dp)) { MiniPlayer(player, actions, onOpenPlayer, look) }
        Spacer(Modifier.height(tabsHeight))
        Spacer(Modifier.navigationBarsPadding())
    }
}

/**
 * The tabs, over the player sheet. As the sheet rises they slide down off the screen - gone by the
 * time it is 70 % open - and come back up as it closes, read in the draw phase from the sheet's
 * progress so nothing recomposes while it moves. Slid away, they are out of reach as well as sight.
 */
@Composable
fun TabBar(route: String?, tabs: List<Tab>, onTab: (String) -> Unit, look: Look, player: PlayerViewModel, onHeight: (androidx.compose.ui.unit.Dp) -> Unit) {
    val slab = look.color(CoverLook.CHROME_SLAB)
    val content = look.color(CoverLook.CHROME_CONTENT)
    val edge = look.color(CoverLook.CHROME_EDGE)
    val accent = rememberTabAccent(player, slab, content)
    val search = tabs.firstOrNull { it.route == "search" }
    val rest = tabs.filter { it.route != "search" }
    val sheet = LocalPlayerSheet.current
    val density = androidx.compose.ui.platform.LocalDensity.current
    Column(
        Modifier.graphicsLayer {
            val t = (sheet.progress.value / 0.7f).coerceIn(0f, 1f)
            translationY = t * (size.height + 12.dp.toPx())
        },
    ) {
        Row(
            Modifier.onGloballyPositioned { onHeight(with(density) { it.size.height.toDp() }) }
                .padding(start = 10.dp, end = 10.dp, top = 8.dp, bottom = 4.dp),
            Arrangement.spacedBy(8.dp), Alignment.CenterVertically,
        ) {
            Surface(
                shape = PillShape, color = slab, contentColor = content, shadowElevation = 12.dp,
                border = androidx.compose.foundation.BorderStroke(androidx.compose.ui.unit.Dp.Hairline, edge),
                modifier = Modifier.weight(1f),
            ) {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 5.dp),
                    Arrangement.SpaceEvenly, Alignment.CenterVertically,
                ) { rest.forEach { t -> TabButton(t, selected = route == t.route, content = content, accent = accent) { onTab(t.route) } } }
            }
            if (search != null) SearchCircle(search, route == search.route, slab, content, edge, accent) { onTab(search.route) }
        }
        Spacer(Modifier.navigationBarsPadding())
    }
}

/**
 * A window wider than it is tall and short with it - a phone on its side - where the app lays itself out
 * across rather than down: the tabs on a rail, the player and the cover pages in two halves. Decided by
 * the window's size, not by which way the phone is held, so a tablet or an unfolded phone gets what fits.
 */
val LocalWide = androidx.compose.runtime.compositionLocalOf { false }

/** Whether a window of this size is laid out across ([LocalWide]). */
fun isWide(widthDp: Int, heightDp: Int): Boolean = widthDp > heightDp && heightDp < 600

/** Search, on its own round slab beside the tabs (or under them, on the rail): the same in both. */
@Composable
private fun SearchCircle(search: Tab, selected: Boolean, slab: Color, content: Color, edge: Color, accent: Color, onClick: () -> Unit) {
    Surface(
        onClick = onClick, shape = CircleShape, color = slab, shadowElevation = 12.dp,
        border = androidx.compose.foundation.BorderStroke(androidx.compose.ui.unit.Dp.Hairline, edge),
        modifier = Modifier.size(58.dp).semantics { contentDescription = search.label },
    ) {
        Box(Modifier.fillMaxSize(), Alignment.Center) {
            Icon(search.icon, null, Modifier.size(25.dp), tint = if (selected) accent else content)
        }
    }
}

/**
 * The tabs on a wide, short window (a phone on its side): [TabBar] stood on end at the left edge - the same
 * floating pill of tabs, and Search on its own circle under it - so the page keeps its height, which a bar
 * across the bottom took almost half of. It slides out to the left as the player opens, as the bar slides
 * down. [onWidth]: how much of the left edge it takes, which the page leaves to it.
 */
@Composable
fun TabRail(route: String?, tabs: List<Tab>, onTab: (String) -> Unit, look: Look, player: PlayerViewModel, onWidth: (androidx.compose.ui.unit.Dp) -> Unit) {
    val slab = look.color(CoverLook.CHROME_SLAB)
    val content = look.color(CoverLook.CHROME_CONTENT)
    val edge = look.color(CoverLook.CHROME_EDGE)
    val accent = rememberTabAccent(player, slab, content)
    val search = tabs.firstOrNull { it.route == "search" }
    val rest = tabs.filter { it.route != "search" }
    val sheet = LocalPlayerSheet.current
    val density = androidx.compose.ui.platform.LocalDensity.current
    Column(
        Modifier
            .windowInsetsPadding(androidx.compose.foundation.layout.WindowInsets.safeDrawing.only(androidx.compose.foundation.layout.WindowInsetsSides.Start + androidx.compose.foundation.layout.WindowInsetsSides.Vertical))
            .onGloballyPositioned { onWidth(with(density) { (it.boundsInRoot().right).toDp() }) }
            .graphicsLayer {
                val t = (sheet.progress.value / 0.7f).coerceIn(0f, 1f)
                translationX = -t * (size.width + 12.dp.toPx())
            }
            .padding(start = 10.dp, end = 4.dp),
        Arrangement.spacedBy(10.dp, Alignment.CenterVertically), Alignment.CenterHorizontally,
    ) {
        Surface(
            shape = PillShape, color = slab, contentColor = content, shadowElevation = 12.dp,
            border = androidx.compose.foundation.BorderStroke(androidx.compose.ui.unit.Dp.Hairline, edge),
        ) {
            Column(Modifier.padding(horizontal = 5.dp, vertical = 6.dp), Arrangement.spacedBy(2.dp), Alignment.CenterHorizontally) {
                rest.forEach { t -> TabButton(t, selected = route == t.route, content = content, accent = accent, narrow = true) { onTab(t.route) } }
            }
        }
        if (search != null) SearchCircle(search, route == search.route, slab, content, edge, accent) { onTab(search.route) }
    }
}

/**
 * The chrome's look: the slab, what is written on it, and the page it fades into. Neutral, like Apple's.
 * Only a page that is *about* one cover - an album, an artist - wears that cover's colour; a bar tinted
 * by whatever happens to be playing turns the whole app red on screens that have nothing to do with the
 * record. The colours themselves are the page's look (nori_look::dress): how far the slab is lifted off
 * the page - more on a dark page, where AMOLED black left it nothing to lift off - and the cover's edge
 * lent to it first, so it still belongs to the record.
 *
 * The bar changes with the page rather than switching over in the frame the page does: one cross-fade
 * over the span the player's own colours travel in, shared by the bar and the tabs.
 */
@Composable
fun rememberChromeLook(): Look {
    val base = LocalLook.current
    val made = pagePalette.value?.look ?: (base as? FixedLook)?.table ?: IntArray(CoverLook.LEN) { base.argb(it) }
    // Keyed on the colours, not the array: a look read out afresh is a new array with the same colours
    // every time this composes, and keying the fade on that restarted it every frame - the fade's own
    // state recomposing this, which made another array, sixty times a second on every screen.
    val kept = remember { arrayOf(made) }
    if (!kept[0].contentEquals(made) && pagesWaiting.intValue == 0) kept[0] = made
    val target = kept[0]
    val t = remember { androidx.compose.animation.core.Animatable(1f) }
    val live = remember { LiveLook { t.value }.also { it.set(null, target, 0) } }
    var first by remember { mutableStateOf(true) }
    LaunchedEffect(target) {
        if (first) { first = false; return@LaunchedEffect }
        // From wherever it has got to, so a page change mid-fade turns round instead of jumping.
        val now = IntArray(CoverLook.LEN) { live.argb(it) }
        t.snapTo(0f)
        live.set(now, target, 0)
        t.animateTo(1f, androidx.compose.animation.core.tween(420))
    }
    return live
}

/**
 * The current tab's colour. With the cover's colours on, it is the accent of the page open when that page
 * wears a cover (an album, an artist, a playlist: [PageTint]), else of what is playing - the one the
 * player's own buttons wear - moved until it reads on the bar ([CoverLook.readable]). The bar itself
 * stays neutral away from those pages (see [rememberChromeLook]), so only the one lit tab follows the
 * music. The theme's own accent with neither and with the setting off. It changes in the span the chrome's own colours take.
 */
@Composable
private fun rememberTabAccent(player: PlayerViewModel, slab: Color, content: Color): Color {
    val theme = MaterialTheme.colorScheme.primary
    val settings: dev.nori.music.app.vm.SettingsViewModel = androidx.lifecycle.viewmodel.compose.viewModel()
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    val state by player.state.collectAsStateWithLifecycle()
    val dark = when (prefs.theme) {
        dev.nori.music.ffi.settings.ThemeMode.SYSTEM -> androidx.compose.foundation.isSystemInDarkTheme()
        dev.nori.music.ffi.settings.ThemeMode.DARK -> true
        dev.nori.music.ffi.settings.ThemeMode.LIGHT -> false
    }
    // The same cover and key the mini player warms, so the colours are already worked out by the time a
    // song starts.
    val url = if (prefs.coverColors) player.cover(state.current?.coverArt, CoverSize.ROW) else null
    val playing = rememberCoverPalette(url, dark, prefs.amoled)?.look?.get(CoverLook.ACCENT)
    val seed = (if (prefs.coverColors) pagePalette.value?.look?.get(CoverLook.ACCENT) else null) ?: playing
    // A cover's accent that no shade of reads on the bar (a pink on an artist page's lifted brown) gives
    // way to the bar's own ink, which always does; the tab is still marked by its weight and size.
    val worked = remember(seed, slab, theme, content) {
        if (seed == null) theme else Color(CoverLook.readable(seed, slab.toArgb(), content.toArgb()))
    }
    // Held while a page's colours are on their way (see pagesWaiting), so it changes once, with the page.
    val held = remember { arrayOf(worked) }
    if (pagesWaiting.intValue == 0) held[0] = worked
    val target = held[0]
    return androidx.compose.animation.animateColorAsState(target, androidx.compose.animation.core.tween(420), label = "tab accent").value
}

data class Tab(val route: String, val label: String, val icon: ImageVector)

@Composable
private fun TabButton(tab: Tab, selected: Boolean, content: Color, accent: Color, narrow: Boolean = false, onClick: () -> Unit) {
    // Apple marks the current tab twice over: the accent colour on the glyph, and a plain lighter patch
    // behind it - light grey on their white bar, so the equivalent here is a little of the bar's own
    // text colour. Tinting that patch with the accent is what made it read as a Material pill.
    val colour = if (selected) accent else content
    val press = remember { androidx.compose.foundation.interaction.MutableInteractionSource() }
    // No patch behind anything. Where you are is the accent colour, a bold label and a slightly larger
    // glyph - every shape drawn behind the current tab, circle or rectangle, ended up reading as
    // Material's active indicator no matter how faint it was made.
    Column(
        Modifier.clip(RoundedCornerShape(14.dp))
            .clickable(interactionSource = press, indication = null, onClick = onClick)
            .padding(horizontal = if (narrow) 8.dp else 18.dp, vertical = 7.dp)
            .semantics { contentDescription = tab.label },
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(tab.icon, null, Modifier.size(if (selected) 26.dp else 23.dp), tint = colour)
        Text(
            tab.label, Modifier.padding(top = 2.dp),
            style = MaterialTheme.typography.labelSmall.copy(fontSize = 10.5f.sp, letterSpacing = 0.sp),
            color = if (selected) colour else colour.copy(alpha = 0.7f),
            fontWeight = if (selected) FontWeight.Bold else FontWeight.Medium,
        )
    }
}

/**
 * The bar above the tabs: artwork, what is playing, and the two controls a thumb wants. Sideways
 * flings skip, an upward fling opens the player - the same gestures the full screen answers to.
 * No progress bar on purpose: it would tick for as long as the app is open.
 */
@Composable
fun MiniPlayer(vm: PlayerViewModel, actions: ActionsViewModel, onOpen: () -> Unit, look: Look) {
    val slab = look.color(CoverLook.CHROME_SLAB)
    val content = look.color(CoverLook.CHROME_CONTENT)
    val edge = look.color(CoverLook.CHROME_EDGE)
    val state by vm.state.collectAsStateWithLifecycle()
    val title = state.current?.title ?: state.radio ?: return
    val settings: dev.nori.music.app.vm.SettingsViewModel = androidx.lifecycle.viewmodel.compose.viewModel()
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    val dark = when (prefs.theme) {
        dev.nori.music.ffi.settings.ThemeMode.SYSTEM -> androidx.compose.foundation.isSystemInDarkTheme()
        dev.nori.music.ffi.settings.ThemeMode.DARK -> true
        dev.nori.music.ffi.settings.ThemeMode.LIGHT -> false
    }
    // The colours of what is playing and of the songs either side, worked out before they are reached. Their covers are
    // already fetched ahead (PlayerViewModel); this is the other half of that, and it is what stops the
    // page wearing the last song's colour for a moment after a skip. It happens here rather than in the
    // player because the bar is on screen whenever something is playing, so a skip from the
    // notification or the lock screen is covered too.
    val context = androidx.compose.ui.platform.LocalContext.current
    // Which songs either side is the core's (`covers_around`, the skips' own targets, shuffle included),
    // worked out once per skip with the covers fetched ahead.
    val near by vm.coversNear.collectAsStateWithLifecycle()
    val around = remember(near) { near.mapNotNull { vm.cover(it, CoverSize.ROW) }.filterNot(::isProviderCover) }
    LaunchedEffect(around, dark, prefs.coverColors, prefs.amoled, prefs.playerColours) {
        if (!prefs.coverColors) return@LaunchedEffect
        for (url in around) {
            // The full-screen player keeps the record's colours where the bar goes black, so that is a
            // second set of colours for the same cover, worked out from the same decode.
            warmCoverPalette(context, url, dark, prefs.amoled, andPlain = prefs.playerColours)
        }
    }
    val scheme = MaterialTheme.colorScheme
    val sheet = LocalPlayerSheet.current
    // Under the open player the bar is still composed and drawn, only covered: a title walking there
    // redrew the whole screen every frame for nobody. Read through derivedStateOf so a drag of the sheet
    // recomposes nothing here until it lands.
    val covered by remember(sheet) { androidx.compose.runtime.derivedStateOf { sheet.progress.value >= 1f } }
    Surface(
        shape = CardShape, color = slab, contentColor = content,
        shadowElevation = 10.dp,
        border = androidx.compose.foundation.BorderStroke(androidx.compose.ui.unit.Dp.Hairline, edge),
        modifier = Modifier.fillMaxWidth()
            .semantics { contentDescription = say.nowPlayingBar }
            .onGloballyPositioned { sheet.miniTop = it.positionInRoot().y }
            // Up opens the player, following the finger the whole way; see PlayerSheet.
            .dragsSheet(sheet),
    ) {
        // The tap has to be a child of the drag detectors, not a sibling behind them: a pointerInput
        // waiting for drag slop swallows a tap offered to a clickable further up the same chain.
        Surface(onClick = onOpen, color = Color.Transparent, contentColor = content) {
        Row(Modifier.fillMaxWidth().padding(end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            // What is playing slides aside for the next (or last) song, which comes in from the other
            // edge already showing; the buttons stay where they are. Radio has no neighbours.
            val song = state.current
            val track: @Composable (dev.nori.music.ffi.model.Song?, Boolean) -> Unit = { s, real ->
                Row(Modifier.fillMaxWidth().padding(start = 8.dp, top = 7.dp, bottom = 7.dp), verticalAlignment = Alignment.CenterVertically) {
                    Cover(
                        vm.cover(s?.coverArt, CoverSize.ROW), 42.dp,
                        if (real) Modifier.onGloballyPositioned {
                            // Only the whole square: slid part-way out of the bar it is clipped, and a
                            // thumbnail with no width is not something to grow the cover from.
                            val b = it.boundsInRoot()
                            if (b.height > 0f && kotlin.math.abs(b.width - b.height) < 1f) sheet.miniCover = b
                        } else Modifier, radius = 7.dp,
                    )
                    Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
                        // A long title here reads itself out twice when the song comes on and then
                        // settles, as the full player's does (see readable). Only the row really
                        // showing: the neighbours wait off either edge at alpha zero, and a long title
                        // there walked after every change of song, a frame each time, on every page.
                        Text(
                            s?.title ?: title, Modifier.readable(iterations = if (real && !covered) READ_OUT else 0),
                            maxLines = 1, softWrap = false, overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodyLarge,
                        )
                        val error = if (real) state.error else null
                        Text(
                            remember(error, s?.artist) { say.barLine(error, s?.artist) }, maxLines = 1, overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodySmall,
                            color = if (real && state.error != null) scheme.error else look.color(CoverLook.CHROME_CONTENT_65),
                        )
                    }
                }
            }
            SwipeCarousel(
                current = song,
                previous = state.queue.getOrNull(state.previousIndex)?.takeIf { song != null },
                next = state.queue.getOrNull(state.nextIndex)?.takeIf { song != null },
                same = { a, b -> a?.id == b?.id },
                onPrevious = vm::previousItem, onNext = vm::next,
                still = covered,
                modifier = Modifier.weight(1f),
                item = track,
            )
            // The one judgement worth making without opening the player: whether this is a song to keep.
            // Apple has only the transport here; the owner asked for the heart, and the bar has the room
            // for it because the title beside it is already allowed to run out of space gracefully.
            song?.let { s ->
                val starred = LocalStarMarks.current.effectiveStar(dev.nori.music.data.StarKind.SONG, s.id, s.starred)
                FavoriteHeart(starred, tint = content, muted = look.color(CoverLook.CHROME_CONTENT_75)) { actions.star(s, !starred) }
            }
            IconButton(vm::toggle) { PlayPauseGlyph(state.playing, state.buffering, 26.dp, 20.dp) }
            IconButton(vm::next) { Icon(Icons.Filled.FastForward, say.next, Modifier.size(25.dp)) }
        }
        }
    }
}

private val pagePalette = androidx.compose.runtime.mutableStateOf<PagePalette?>(null)

/**
 * How much room the floating chrome takes at the bottom. Screens add it to the bottom of their own
 * scrolling content, so a list can run underneath the mini player - the page's colour reaches the
 * bottom edge of the screen, and the last row is still reachable.
 */
val LocalChromeInset = androidx.compose.runtime.compositionLocalOf { 0.dp }

/**
 * Pages whose cover's colours are still being worked out. While there are any, the chrome and the lit
 * tab keep the colours they have: from one album to the next they went to the playing song's (or the
 * theme's) for the moment the new cover took, and then to the new page's, flashing twice where the page
 * changed once.
 */
private val pagesWaiting = androidx.compose.runtime.mutableIntStateOf(0)

/**
 * A tinted page (an album, an artist, the player) lends its colours to the chrome while it is open.
 * [waiting]: it will have colours, but they are not worked out yet.
 */
@Composable
fun PageTint(palette: PagePalette?, waiting: Boolean = false) {
    androidx.compose.runtime.DisposableEffect(palette, waiting) {
        if (waiting) pagesWaiting.intValue++ else pagePalette.value = palette
        onDispose {
            if (waiting) pagesWaiting.intValue--
            else if (pagePalette.value === palette) pagePalette.value = null
        }
    }
}

/**
 * A row of records, one showing: a sideways drag slides the showing one aside and brings its neighbour
 * in from the other edge, already drawn. Let go past a third of the way, or flicked, the neighbour
 * lands and [onNext] / [onPrevious] runs; it stays drawn in place of [current] until [current] is that
 * song too (the player answers a few frames later), so the old one never comes back for a frame.
 * [item]'s second argument is true for the one really showing. Nothing runs until a finger is down, or
 * until [current] steps to a neighbour by itself, which slides the same way unless the row is [still]
 * (out of sight).
 */
@Composable
internal fun <T> SwipeCarousel(
    current: T, previous: T?, next: T?, same: (T?, T?) -> Boolean,
    onPrevious: () -> Unit, onNext: () -> Unit,
    modifier: Modifier = Modifier,
    still: Boolean = false,
    item: @Composable (T?, Boolean) -> Unit,
) {
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val haptics = androidx.compose.ui.platform.LocalHapticFeedback.current
    // Straight from the finger, not through a coroutine per pointer event: those queued up on a flick
    // and landed after the settle had started, which pulled the row back mid-change. Same story as the
    // sleeve's; see SleeveCarousel.
    var offset by remember { androidx.compose.runtime.mutableFloatStateOf(0f) }
    var moving by remember { mutableStateOf<kotlinx.coroutines.Job?>(null) }
    val hasBefore by androidx.compose.runtime.rememberUpdatedState(previous != null)
    val hasAfter by androidx.compose.runtime.rememberUpdatedState(next != null)
    val nextNow by androidx.compose.runtime.rememberUpdatedState(next)
    val previousNow by androidx.compose.runtime.rememberUpdatedState(previous)
    // The neighbour that has been slid in, shown until the player has caught up with it.
    var landed by remember { mutableStateOf<Any?>(NONE) }
    val currentNow by androidx.compose.runtime.rememberUpdatedState(current)
    androidx.compose.runtime.LaunchedEffect(landed) {
        if (landed === NONE) return@LaunchedEffect
        @Suppress("UNCHECKED_CAST")
        kotlinx.coroutines.withTimeoutOrNull(2_000) {
            androidx.compose.runtime.snapshotFlow { same(currentNow, landed as T?) }.first { it }
        }
        landed = NONE
    }
    // A song that changes by itself (its end, a mix, the bar's own next button, the notification) comes
    // in as a swipe brings it: the one it left goes out one side, the new one in from the other. As on
    // the player's sleeve, decided while composing, so the new song is never drawn in place first; only
    // for a step to a neighbour (the one left is then the neighbour, drawn where the slide starts).
    val last = remember { Last<T>() }
    val go = when {
        !last.set || same(current, last.item) || current == null -> 0
        AppMotion.reduce || still || landed !== NONE || moving?.isActive == true -> 0
        same(previous, last.item) -> 1
        same(next, last.item) -> -1
        else -> 0
    }
    /** Where the row is while it slides by itself, in rows: 1 a whole row to the right, 0 in place. */
    val natural = remember(current) { androidx.compose.animation.core.Animatable(go.toFloat()) }
    val naturalNow by androidx.compose.runtime.rememberUpdatedState(natural)
    androidx.compose.runtime.SideEffect {
        if (!last.set || !same(current, last.item)) { last.item = current; last.set = true }
    }
    androidx.compose.runtime.LaunchedEffect(natural) {
        // Counted in frames, as the sleeve's is: the new song's first frames can be slow ones.
        if (natural.value != 0f) settleByFrames(natural, 560f)
    }
    Box(
        modifier.clipToBounds().pointerInput(Unit) {
            val tracker = androidx.compose.ui.input.pointer.util.VelocityTracker()
            var x = 0f
            val release: (Float) -> Unit = { v ->
                val o = offset
                val w = size.width.toFloat()
                // The same gesture as the sleeve's, by the same rule (swipeTurn), with the bar's slower flick.
                val go = swipeTurn(o, v, w, hasBefore, hasAfter, bar = true)
                val running = moving
                moving = scope.launch {
                    running?.cancelAndJoin()
                    val settle = androidx.compose.animation.core.spring<Float>(dampingRatio = 1f, stiffness = 560f)
                    if (go == 0) {
                        androidx.compose.animation.core.animate(offset, 0f, v, settle) { value, _ -> offset = value }
                        return@launch
                    }
                    haptics.performHapticFeedback(androidx.compose.ui.hapticfeedback.HapticFeedbackType.LongPress)
                    val arriving = if (go < 0) nextNow else previousNow
                    var changed = false
                    try {
                        if (AppMotion.reduce) offset = go * w
                        else androidx.compose.animation.core.animate(offset, go * w, v, settle) { value, _ -> offset = value }
                        landed = arriving
                        offset = 0f
                        changed = true
                        if (go < 0) onNext() else onPrevious()
                    } finally {
                        if (!changed) { offset = 0f; if (go < 0) onNext() else onPrevious() }
                    }
                }
            }
            // The bar answers an upward drag by opening the player (dragsSheet, on the surface around
            // this), so the sideways drag has to be plainly sideways or the two fight over every
            // diagonal - which is the bar changing the song when it was asked to open.
            sidewaysDrag(
                slop = 1.5f, ratio = 1.8f,
                onDragStart = {
                    tracker.resetTracking(); x = 0f; moving?.cancel()
                    // A row still sliding in by itself is taken over where it is.
                    val n = naturalNow.value
                    if (n != 0f) { offset += n * size.width; scope.launch { naturalNow.snapTo(0f) } }
                },
                onDragEnd = { release(tracker.calculateVelocity().x) },
                onDragCancel = { release(0f) },
            ) { change, d ->
                x += d
                tracker.addPosition(change.uptimeMillis, androidx.compose.ui.geometry.Offset(x, 0f))
                val w = size.width.toFloat()
                val moved = offset + d
                val allowed = (moved > 0f && hasBefore) || (moved < 0f && hasAfter)
                offset = if (allowed) moved.coerceIn(-w, w) else (offset + d * GIVE).coerceIn(-w * GIVE_LIMIT, w * GIVE_LIMIT)
            }
        },
    ) {
        @Suppress("UNCHECKED_CAST")
        val showing = if (landed === NONE) current else landed as T?
        Box(Modifier.graphicsLayer { translationX = offset + natural.value * size.width }) { item(showing, landed === NONE) }
        Box(Modifier.graphicsLayer { val o = offset + natural.value * size.width; alpha = if (o < 0f) 1f else 0f; translationX = o + size.width }) { item(next, false) }
        Box(Modifier.graphicsLayer { val o = offset + natural.value * size.width; alpha = if (o > 0f) 1f else 0f; translationX = o - size.width }) { item(previous, false) }
    }
}

private val NONE = Any()

/** What a row of records was last composed with; written after each composition, read by the next. */
private class Last<T> {
    var item: T? = null
    var set = false
}
