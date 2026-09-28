package dev.nori.music.app.ui

import dev.nori.music.look.CoverLook
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.calculateStartPadding
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.ui.layout.layout
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Shuffle
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.ffi.settings.ThemeMode

/**
 * An album, artist or playlist page, built the way Apple Music builds one: the artwork runs edge to
 * edge under the status bar and dissolves into the page, the page wears the colour the artwork ends
 * on, and the title, the artist and the two big buttons sit centred underneath it.
 *
 * The dissolve is the point. The wash starts from the average colour of the cover's own bottom rows,
 * so there is no line where the picture stops - it simply runs out of picture. Everything is static:
 * one gradient, no blur, no animation, so an open page costs nothing between frames. Scrolling moves
 * the artwork in the draw phase only, so it never recomposes.
 */
@Composable
fun HeroPage(
    coverUrl: String?,
    title: String,
    /** The artist line under the title, in the cover's accent colour; tapping it opens [onSubtitle]. */
    subtitle: String? = null,
    /** A quiet line under that, in sentence case: year, song count, length, quality. */
    caption: String = "",
    onSubtitle: (() -> Unit)? = null,
    onPlay: (() -> Unit)? = null,
    onShuffle: (() -> Unit)? = null,
    /**
     * Keep the Shuffle + Play row even while [onPlay] / [onShuffle] are still null (hinted album /
     * artist / playlist pages waiting on the server). Without this the row jumps from heart-only to
     * transport when the detail lands - a one-frame pop.
     */
    awaitingPlay: Boolean = false,
    /**
     * This page's own queue (`pages::PageQueue`): the origin a queue started from the page carries. The
     * two big buttons answer for that queue rather than for the player in general
     * (`pages::hero_buttons`), and only for it: another page's queue playing a song this page also has
     * leaves them Play and Shuffle. Null: the page has no queue of its own.
     */
    queue: dev.nori.music.ffi.library.PageQueue? = null,
    /** Icon buttons on the line with the pills: favourite, queue, download. */
    actions: @Composable RowScope.() -> Unit = {},
    /**
     * Artwork of the page's own making, for a page with no cover to bleed (a mix): drawn as a tile in the
     * middle, below the status bar, the way Apple shows a made-for-you mix. Used only when [coverUrl] is null.
     */
    art: (@Composable () -> Unit)? = null,
    /**
     * Whether this kind of page keeps its cover's colours with a black background (album and artist
     * pages each have a setting); the others go black.
     */
    keepsColours: (dev.nori.music.ffi.settings.StoredPrefs) -> Boolean = { false },
    content: LazyListScope.() -> Unit,
) {
    val settings: SettingsViewModel = viewModel()
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    val dark = when (prefs.theme) { ThemeMode.SYSTEM -> isSystemInDarkTheme(); ThemeMode.DARK -> true; ThemeMode.LIGHT -> false }
    val keeps = keepsColours(prefs)
    val black = remember(prefs.amoled, keeps) { dev.nori.music.ffi.pageBlack(prefs.amoled, keeps) }
    // A provider's page too: its cover is on screen already, so measuring it asks the provider for
    // nothing more, and the cover loader never keeps it (only the colours stay, in memory).
    val palette = if (prefs.coverColors) rememberCoverPalette(coverUrl, dark, black) else null
    // Shuffle stays labelled Shuffle (never Pause); it lights while this page's queue is shuffling.
    val player: PlayerViewModel = viewModel()
    val playerState by player.state.collectAsStateWithLifecycle()
    // The queue this page started is what is on - whichever song of it happens to be sounding. Asked
    // when a new queue is set (the core's origin generation), not on every change of the player's state.
    val here = remember(queue, playerState.origin) { queue != null && player.playsFrom(queue) }
    // The core's answer (`pages::hero_buttons`) over JNI, one int, on every play and pause.
    val bits = remember(here, playerState.shuffle, playerState.playing, playerState.buffering, onPlay != null, onShuffle != null) {
        CoverLook.heroButtons(here, playerState.shuffle, playerState.playing, playerState.buffering, onPlay != null, onShuffle != null)
    }
    val pausing = bits and 4 != 0
    val buttons = remember(bits) {
        dev.nori.music.ffi.library.HeroButtons(
            shuffleLit = bits and 1 != 0, shuffleEnabled = bits and 2 != 0, shufflePress = heroPress(bits shr 4),
            pausing = pausing, playEnabled = bits and 8 != 0, playPress = heroPress(bits shr 6),
        )
    }

    TintedTheme(palette) {
        val scheme = MaterialTheme.colorScheme
        SystemBarIcons(LocalLook.current)
        PageTint(palette, waiting = palette == null && prefs.coverColors && coverUrl != null)
        val list = rememberLazyListState()
        // How far the hero has scrolled off, for its parallax: the list's own scroll upright; on its side the
        // hero stands still in its half and does not move with the songs (see below).
        val wide = LocalWide.current
        val heroScroll: (Float) -> Float = if (wide) { _ -> 0f } else { h -> if (list.firstVisibleItemIndex == 0) list.firstVisibleItemScrollOffset.toFloat() else h }
        // One block: artwork, then the wash it melts into, carrying the title and the buttons.
        // [coverSide]: on its side, the cover (or the page's own artwork) at this size, centred in the half,
        // so the half keeps the width its buttons need however short the screen is. Upright it is null and
        // the cover takes the full width, as ever.
        // [showArt] false: the name, caption and buttons alone, for laying over a cover drawn apart (below).
        val hero: @Composable (coverSide: androidx.compose.ui.unit.Dp?, showArt: Boolean) -> Unit = { coverSide, showArt ->
        Column(Modifier.fillMaxWidth()) {
            if (showArt) {
            // On its side the cover is a card standing in its half, as on a shelf, not a sleeve bleeding to the
            // screen's edges: nothing for it to dissolve into above or beside it.
            if (coverUrl != null && coverSide != null) Box(
                Modifier.fillMaxWidth().statusBarsPadding().padding(top = WIDE_TOP, bottom = 14.dp),
                Alignment.Center,
            ) { Cover(coverUrl, coverSide, radius = Radius.card) }
            else if (coverUrl != null) Box(
                Modifier.fillMaxWidth().aspectRatio(1f)
                    // Parallax and fade, read in the draw phase: scrolling never recomposes the hero.
                    .graphicsLayer {
                        val scrolled = heroScroll(size.height)
                        translationY = scrolled * 0.4f
                        alpha = 1f - (scrolled / size.height).coerceIn(0f, 1f) * 0.5f
                    },
            ) {
                Cover(coverUrl, 0.dp, Modifier.fillMaxSize())
                val look = LocalLook.current
                Box(
                    Modifier.fillMaxSize().drawWithCache {
                        // The whole dissolve happens inside the artwork, and finishes on the
                        // page colour rather than on the cover's edge colour. It used to stop
                        // on the edge colour and leave a second gradient below to carry on -
                        // but the parallax slides the picture down over that gradient as the
                        // page scrolls, squeezing it into a few dozen pixels, and a colour
                        // ramp that steep across the full width is a line. The picture has
                        // its own height to do this in, and ending on the page colour means
                        // there is nothing left to hand over to. The stops are the look's
                        // (nori_look::dress), made into brushes once per size.
                        val at = stage.heroStops
                        val dissolve = Brush.verticalGradient(
                            at[0] to Color.Transparent,
                            at[1] to look.color(CoverLook.HERO_EDGE),
                            at[2] to look.color(CoverLook.HERO_MID),
                            at[3] to look.color(CoverLook.BACKGROUND),
                        )
                        // Just enough shade under the status bar for white icons on a pale cover.
                        val shade = Brush.verticalGradient(0f to Color.Black.copy(alpha = stage.statusShade), stage.statusShadeTo to Color.Transparent)
                        onDrawWithContent {
                            drawContent()
                            drawRect(dissolve)
                            drawRect(shade)
                        }
                    },
                )
            } else if (art != null) Box(
                Modifier.fillMaxWidth().statusBarsPadding().padding(top = if (coverSide != null) WIDE_TOP else 64.dp, bottom = if (coverSide != null) 14.dp else 18.dp)
                    // The page's own artwork is drawn at one size; on its side it is scaled to fit its place.
                    .then(if (coverSide != null) Modifier.height(coverSide) else Modifier),
                Alignment.Center,
            ) {
                if (coverSide == null) art()
                else Box(Modifier.size(coverSide).wrapContentSize(unbounded = true).graphicsLayer {
                    val k = coverSide.toPx() / MIX_ART.toPx()
                    scaleX = k; scaleY = k
                }) { art() }
            } else Spacer(Modifier.statusBarsPadding().height(72.dp))
            }

            // Nothing is painted here: the artwork above has already dissolved onto the page
            // colour, and the page colour is what the root is painted with.
            Column(Modifier.fillMaxWidth()) {
            Column(
                Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 4.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    title, style = MaterialTheme.typography.headlineSmall, textAlign = TextAlign.Center,
                    maxLines = 2, overflow = TextOverflow.Ellipsis,
                )
                if (!subtitle.isNullOrEmpty()) Text(
                    subtitle,
                    Modifier.padding(top = 2.dp).then(if (onSubtitle != null) Modifier.clickable(onClick = onSubtitle) else Modifier),
                    style = MaterialTheme.typography.titleMedium, color = scheme.primary,
                    textAlign = TextAlign.Center, maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                // Sentence case, as Apple writes it ("25 songs, 1 hour 42 minutes"). Small
                // capitals here made the line shout louder than the artist above it.
                Caption(caption, Modifier.padding(top = 6.dp), align = TextAlign.Center, caps = false)
            }

            // Apple's arrangement: shuffle in a circle on the left, one wide Play pill in the
            // middle, and the page's other action in a circle on the right. Two equal pills
            // side by side give the page two things to look at instead of one. The row is
            // reserved while [awaitingPlay] so the layout does not jump when the taps land.
            if (awaitingPlay || onPlay != null || onShuffle != null) Row(
                Modifier.fillMaxWidth().padding(start = Space.gutter, end = Space.gutter, top = 16.dp),
                Arrangement.spacedBy(12.dp), Alignment.CenterVertically,
            ) {
                val press: (dev.nori.music.ffi.library.HeroPress, (() -> Unit)?) -> Unit = { p, start ->
                    when (p) {
                        dev.nori.music.ffi.library.HeroPress.START -> start?.invoke()
                        dev.nori.music.ffi.library.HeroPress.TOGGLE -> player.toggle()
                        dev.nori.music.ffi.library.HeroPress.SHUFFLE_OFF -> player.toggleShuffle()
                    }
                }
                CircleButton(
                    Icons.Filled.Shuffle, say.shuffle,
                    enabled = buttons.shuffleEnabled, lit = buttons.shuffleLit,
                    onClick = { press(buttons.shufflePress, onShuffle) },
                )
                PillButton(
                    if (buttons.pausing) say.pause else say.play, if (buttons.pausing) Icons.Filled.Pause else Icons.Filled.PlayArrow,
                    { press(buttons.playPress, onPlay) }, Modifier.weight(1f),
                    prominent = true, enabled = buttons.playEnabled,
                )
                actions()
            } else Row(
                Modifier.fillMaxWidth().padding(start = Space.tight, end = Space.tight, top = 2.dp),
                Arrangement.Center, Alignment.CenterVertically,
            ) { actions() }
            Spacer(Modifier.height(10.dp))
            }
        }
        }
        // On its side the page's colour runs out under the strips beside it (the camera's, the rail's), which it
        // leaves as it goes, rather than the app's own colour there showing the page's edges.
        val outStart = LocalPageStart.current
        val outEnd = LocalPageEnd.current
        Box(Modifier.fillMaxSize().drawBehind {
            val l = outStart.toPx()
            drawRect(scheme.background, topLeft = Offset(-l, 0f), size = Size(size.width + l + outEnd.toPx(), size.height))
        }) {
            if (wide) androidx.compose.foundation.layout.BoxWithConstraints(Modifier.fillMaxSize()) {
                // On its side the page stands in two halves, as Apple's does on a wide screen: the cover,
                // the name and the buttons on the left, still, and the songs scrolling down the right.
                // Upright, the cover alone was the whole screen and the songs began a screen further down.
                // The half is wide enough for the buttons; the cover in it is as large as the height left once
                // the status bar, the now playing bar and the name, caption and buttons (about 170 dp) are
                // counted out, so all of it fits without scrolling.
                // At least room for the three round buttons and a Play pill that still fits "Pause".
                // The player's own share (PlayerHalves), so a cover here is the same band, cropped above and below,
                // its soft edge where the page's words begin.
                val half = maxOf(maxWidth * 0.55f, 360.dp)
                // Inside the half's gutters, and short enough to leave the top margin, the name, caption and
                // buttons (about 190 dp) and the now playing bar their room.
                val top = with(androidx.compose.ui.platform.LocalDensity.current) {
                    androidx.compose.foundation.layout.WindowInsets.statusBars.getTop(this).toDp()
                }
                val side = minOf(half - Space.gutter * 2, maxHeight - top - WIDE_TOP - LocalChromeInset.current - 190.dp).coerceAtLeast(88.dp)
                Row(Modifier.fillMaxSize()) {
                    // A cover fills its half as the player's sleeve fills its own: from the screen's very left
                    // edge - under the camera's punch hole, which the app otherwise keeps pages clear of - and top,
                    // going soft at its right edge, blurred as the sleeve does, into the page. The name, caption and
                    // buttons stand on the right above the songs, as the player's controls stand beside its sleeve,
                    // so nothing is written over the picture. A page with no cover of its own (a mix) keeps its
                    // artwork as a tile beside its buttons.
                    if (coverUrl != null) {
                        val cutout = LocalPageStart.current
                        Box(
                            Modifier.width(half).fillMaxHeight().layout { measurable, constraints ->
                                // Out over the strip the page is kept off (the camera's, or the rail's), to the screen's edge.
                                val extra = cutout.roundToPx()
                                val placeable = measurable.measure(constraints.copy(minWidth = constraints.maxWidth + extra, maxWidth = constraints.maxWidth + extra))
                                layout(constraints.maxWidth, placeable.height) { placeable.place(-extra, 0) }
                            },
                        ) {
                            SoftSleeve(Modifier.fillMaxSize()) { Cover(coverUrl, 0.dp, Modifier.fillMaxSize(), radius = 0.dp) }
                            // The shade under the status bar, faded out with the soft right edge so it does not end on a line.
                            Box(
                                Modifier.fillMaxSize().graphicsLayer { compositingStrategy = androidx.compose.ui.graphics.CompositingStrategy.Offscreen }.drawWithCache {
                                    val shade = Brush.verticalGradient(0f to Color.Black.copy(alpha = stage.statusShade), stage.statusShadeTo to Color.Transparent)
                                    val right = alphaGradient(stage.rubOut, Color.Black, size.width * (1f - MELT), size.width, across = true)
                                    onDrawBehind {
                                        drawRect(shade)
                                        drawRect(right, blendMode = androidx.compose.ui.graphics.BlendMode.DstOut)
                                    }
                                },
                            )
                        }
                                        } else Column(Modifier.width(half).fillMaxHeight().verticalScroll(androidx.compose.foundation.rememberScrollState())) {
                        hero(side, true)
                        Spacer(Modifier.height(LocalChromeInset.current))
                    }
                    LazyColumn(Modifier.weight(1f).fillMaxHeight(), state = list) {
                        // The first song level with the top of the cover beside it.
                        item(key = "hero-wide-top") { Spacer(Modifier.statusBarsPadding().height(WIDE_TOP)) }
                        if (coverUrl != null) item(key = "hero-wide-head", contentType = "hero") { Column(Modifier.padding(bottom = 8.dp)) { hero(null, false) } }
                        content()
                        item(key = "tail") { Spacer(Modifier.height(Space.section + LocalChromeInset.current)) }
                    }
                }
            } else LazyColumn(state = list) {
                item(key = "hero", contentType = "hero") {
                    // One block: artwork, then the wash it melts into, carrying the title and the buttons.
                    //
                    hero(null, true)
                }
                content()
                item(key = "tail") { Spacer(Modifier.height(Space.section + LocalChromeInset.current)) }
            }
            // No back button over the artwork: the back gesture is how these pages are left, and a dimmed disc
            // on the cover only covered part of it.
        }
    }
}

/** On its side: the space above the cover and above the first song, under the status bar. */
private val WIDE_TOP = 12.dp

/** The size a page's own artwork (a mix's) is drawn at: MixScreen hands [HeroPage] its art at this size. */
private val MIX_ART = 236.dp

/** Two bits of `HeroButtons::pack`: what a button presses. */
private fun heroPress(bits: Int) = when (bits and 3) {
    1 -> dev.nori.music.ffi.library.HeroPress.TOGGLE
    2 -> dev.nori.music.ffi.library.HeroPress.SHUFFLE_OFF
    else -> dev.nori.music.ffi.library.HeroPress.START
}
