package dev.nori.music.app.ui

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material.icons.filled.PlaylistRemove
import dev.nori.music.ffi.model.Song
import androidx.compose.foundation.MarqueeSpacing
import androidx.compose.foundation.background
import androidx.compose.foundation.basicMarquee
import kotlin.math.roundToInt
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.zIndex
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.material.icons.filled.DragHandle
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.QueueMusic
import androidx.compose.material.icons.filled.Bedtime
import androidx.compose.foundation.layout.Spacer
import androidx.compose.material.icons.filled.Speaker
import androidx.compose.material.icons.outlined.Speaker
import dev.nori.music.app.R
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.FastForward
import androidx.compose.material.icons.filled.FastRewind
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Lyrics
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Repeat
import androidx.compose.material.icons.filled.RepeatOne
import androidx.compose.material.icons.filled.Shuffle
import androidx.compose.material.icons.filled.SkipNext
import androidx.compose.material.icons.filled.SkipPrevious
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.material.icons.automirrored.filled.VolumeDown
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.composed
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.clip
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.asComposeRenderEffect
import androidx.compose.ui.graphics.isSpecified
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.geometry.Rect
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.look.CoverLook
import androidx.compose.ui.draw.drawWithCache
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.playback.Repeat
import dev.nori.music.ffi.settings.ThemeMode
import kotlinx.coroutines.delay
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.first

enum class Panel { ART, QUEUE, LYRICS }

/**
 * Now playing, the way a full-screen player should feel: the page is a wash of the artwork's own
 * colours, the artwork is a large rounded card that shrinks when the music stops, and the controls
 * sit in one column under it. Lyrics and the queue take the artwork's place rather than opening a
 * second screen, so the transport never moves.
 *
 * The only thing that ticks is the seek bar, and only while this screen is resumed and playing.
 */
@OptIn(androidx.compose.animation.ExperimentalSharedTransitionApi::class)
@Composable
fun PlayerScreen(vm: PlayerViewModel, actions: ActionsViewModel) {
    val state by vm.state.collectAsStateWithLifecycle()
    val marks = LocalStarMarks.current
    val sheet = LocalPlayerSheet.current
    val menu = LocalSongMenu.current
    val playerMenu = LocalPlayerMenu.current
    // The artist and album lines under the title lead somewhere; going there puts the player away, which
    // Nav does for every route while the sheet is up.
    val nav = LocalNav.current
    var panel by rememberSaveable { mutableStateOf(Panel.ART) }
    // A panel's button opens it, and pressed again goes back to the artwork.
    // Each change's arriving panel goes under every panel before it (see the panels' AnimatedContent).
    var changes by remember { mutableIntStateOf(0) }
    val choose: (Panel) -> Unit = { panel = if (panel == it) Panel.ART else it; changes++ }
    // Where the sleeve ends, so the page behind it can be drawn at the same scale. Written on layout,
    // read in the draw phase; it only moves when the window does.
    var sleeveBottom by remember { mutableFloatStateOf(0f) }
    // The player's own coordinates, which the sleeve is measured in. Not the window's: the sheet moves the
    // whole player with a layer, and a layer moving counts as the sleeve moving, so measured against the
    // window the sleeve's bottom rode up the screen with the sheet (a frame late, too). The page's blur,
    // drawn inside the sheet, then sat a whole sheet's travel below the cover all the way up.
    val player = remember { arrayOfNulls<androidx.compose.ui.layout.LayoutCoordinates>(1) }
    var sleeveHeight by remember { mutableFloatStateOf(0f) }
    // A panel asked for from outside (the lyrics widget's tap, a jam strip's): that panel once the player is
    // up and its sleeve measured, as its button there would have it. Asked for while the app was still
    // opening, the change ran before either, and its fade stopped with the cover still whole and the
    // lyrics not shown.
    LaunchedEffect(sheet.panelAsked) {
        val asked = sheet.panelAsked ?: return@LaunchedEffect
        androidx.compose.runtime.snapshotFlow { sheet.progress.value >= 1f && sleeveHeight > 0f }.first { it }
        sheet.panelAsked = null
        if (panel != asked) choose(asked)
    }
    // The transport's way of asking the sleeve to change record; see SleeveSlide.
    val slide = remember { SleeveSlide() }
    // Where the page's colours are between records while one is moving; see PageShift.
    val shift = remember { PageShift() }
    // How far the panel that is arriving has arrived. One number for the whole screen, driven here
    // rather than inside AnimatedContent: a child animation started by the content that is entering
    // reads its own state as already settled and never runs, which is why the sleeve used to be
    // replaced by the page behind it in a single frame instead of dissolving into it.
    val arrival = remember { Animatable(1f) }
    var showing by remember { mutableStateOf(panel) }
    /** The panel being left, for as long as the change lasts. */
    var leaving by remember { mutableStateOf(panel) }
    LaunchedEffect(panel) {
        if (panel == showing) return@LaunchedEffect
        leaving = showing
        // The panel that has just been asked for is composed before this runs, and an arrival still
        // sitting at 1 from the last change drew it once at full strength before it started fading up:
        // the panel blinked, whole, and then eased in from nothing.
        if (AppMotion.reduce) { arrival.snapTo(1f); showing = panel } else {
            arrival.snapTo(0f)
            showing = panel
            arrival.animateTo(1f, androidx.compose.animation.core.tween(PANEL_MS))
        }
    }
    // Read in the draw phase: nothing until the change above has begun, so the frame a panel first
    // appears on is the first frame of its fade rather than one at full strength.
    val arrived = FloatReader { if (panel != showing) 0f else arrival.value }
    // The lyrics keep a small copy of the cover in their header, so between the artwork and the lyrics
    // there is one cover and it travels, the way it does between the now playing bar and the sleeve.
    // Dissolving the sleeve into the blurred page instead is what read as a block of blur appearing at
    // the top of the screen out of nothing. The queue has no cover of its own, so that change stays a
    // plain dissolve.
    var thumb by remember { mutableStateOf(Rect.Zero) }
    LaunchedEffect(sheet) {
        androidx.compose.runtime.snapshotFlow { sheet.panelCover }.collect { if (it != Rect.Zero) thumb = it }
    }
    // Derived, so the change's own frames do not recompose the player: only the moment the flight
    // starts and the moment it ends do.
    val flying by remember {
        androidx.compose.runtime.derivedStateOf {
            arrival.value < 1f && thumb != Rect.Zero && sleeveHeight > 0f &&
                (panel == Panel.LYRICS && leaving == Panel.ART || panel == Panel.ART && leaving == Panel.LYRICS)
        }
    }
    androidx.compose.runtime.DisposableEffect(flying) {
        sheet.panelFlight = flying
        onDispose { sheet.panelFlight = false }
    }

    val settingsVm: SettingsViewModel = viewModel()
    val prefs by settingsVm.prefs.collectAsStateWithLifecycle()
    val dark = when (prefs.theme) { ThemeMode.SYSTEM -> isSystemInDarkTheme(); ThemeMode.DARK -> true; ThemeMode.LIGHT -> false }
    val coverUrl = vm.cover(state.current?.coverArt, CoverSize.FULL)
    // One picture for the sleeve and for the cover in flight (see SleeveArt).
    val sleeveArt = rememberSleeveArt(coverUrl)
    // The moving cover (Settings, Look), over the still one while the sleeve is at rest (SleeveMotion.kt).
    // Nothing of it exists while it is switched off: no lookup, no player, no surface, no touch watcher.
    val motion = if (prefs.motionArtwork && prefs.thirdPartyLookups) remember { SleeveMotion(vm::motionView) } else null
    if (motion != null) MotionDirector(vm, motion, sheet, onArtwork = panel == Panel.ART && showing == Panel.ART && !flying)
    // AMOLED black everywhere else, but the player keeps the cover's colours unless asked not to: in
    // black, the page under the sleeve was pure black and the picture looked cut off, where Apple's
    // carries the record's colour down the whole screen.
    val black = remember(prefs.amoled, prefs.playerColours) { dev.nori.music.ffi.pageBlack(prefs.amoled, prefs.playerColours) }
    // A provider's song wears its cover too (it is on screen); only its neighbours' are never measured ahead.
    val rowUrl = vm.cover(state.current?.coverArt, CoverSize.ROW)
    val tint = if (prefs.coverColors) rememberCoverTint(rowUrl, dark, black) else CoverTint(rowUrl, null)
    val found = tint.palette
    // The colours of the record on its way in, already worked out by the time it is asked for (the now
    // playing bar measures both neighbours ahead; see warmCoverPalette). Only as they were when the record
    // set off: colours worked out while it was already on its way would be brought up at however far it
    // had got, in one frame. Those come after it instead, the page cross-fading to them once it is in.
    val arrivingNowMeasured = if (prefs.coverColors) rememberCoverPalette(shift.towards, dark, black) else null
    val arriving = remember(shift.towards, prefs.coverColors, dark, black) { arrivingNowMeasured }
    val previousTintUrl = state.queue.getOrNull(state.previousIndex)?.let { vm.cover(it.coverArt, CoverSize.ROW)?.takeUnless(::isProviderCover) }
    val nextTintUrl = state.queue.getOrNull(state.nextIndex)?.let { vm.cover(it.coverArt, CoverSize.ROW)?.takeUnless(::isProviderCover) }
    // Both neighbours' colours are worked out while nothing is happening, so that a record swiped to has
    // them the moment it starts moving. The now playing bar does this too, but the bar is not on screen
    // while the player is, and without it the page had nothing to cross-fade to: it wore the last
    // record's colours for the whole swipe and changed to the new one in a single frame at the end,
    // which is the old colour sitting under a cover that had already changed.
    val warmContext = LocalContext.current
    LaunchedEffect(previousTintUrl, nextTintUrl, dark, black) {
        if (!prefs.coverColors) return@LaunchedEffect
        warmCoverPalette(warmContext, nextTintUrl, dark, black)
        warmCoverPalette(warmContext, previousTintUrl, dark, black)
    }
    // The page's colours change with the song by cross-fading, not in one frame. A song whose colours are
    // not worked out yet keeps the last song's for a moment (the same grace the sleeve's picture has, so
    // colours measured from the disk go straight from the one song's to the other's), and then the page
    // fades to the theme's own plain page, no song's colour, under the sleeve's placeholder; when its
    // colours come it fades from there to them. A song with no artwork stays on the plain page.
    var palette by remember { mutableStateOf(found) }
    var fadingFrom by remember { mutableStateOf<PagePalette?>(null) }
    val washFade = remember { androidx.compose.animation.core.Animatable(1f) }
    // The plain page as a page's colours, to fade from: the theme's own look with no picture in its wash.
    val baseTable = LocalLook.current.let { l -> remember(l) { (l as? FixedLook)?.table ?: IntArray(dev.nori.music.look.CoverLook.LEN) { l.argb(it) } } }
    val neutral = remember(baseTable) { PagePalette(baseTable) }
    val colourTurn = remember { CoverTurn(stage.colourWaitMs) }
    // See below, where it is latched.
    var held by remember { mutableStateOf<PagePalette?>(null) }
    val arrivingNow by androidx.compose.runtime.rememberUpdatedState(arriving)
    LaunchedEffect(tint) {
        // Still the last song's colours, which the song after it must not be given: colours worked out
        // for a song skipped past are dropped.
        if (tint.url != rowUrl) return@LaunchedEffect
        val now = android.os.SystemClock.uptimeMillis()
        colourTurn.song(rowUrl, now, ready = found != null)
        // A fade this change cut short is finished first, from where it had got to: starting the next
        // one from its end would put the page there in one frame.
        if (fadingFrom != null) {
            if (!AppMotion.reduce && washFade.value < 1f) washFade.fadeByFrames(1f, stage.colourFadeMs * (1f - washFade.value))
            fadingFrom = null
        }
        if (found == palette) { shift.adopted = rowUrl; return@LaunchedEffect }
        // The grace, counted from the change of song.
        if (found == null) delay(colourTurn.holdLeft(now))
        // A record that carried its colours in with it has them on screen already, so the page takes
        // them over underneath rather than fading to them a second time; anything else - a song tapped
        // in the queue, the notification, the queue running on by itself, colours that came after the
        // record - cross-fades. Or the page has this record's colours already, because the record handed
        // them over when it landed (see PageShift.arrived). Fading from the record before it then would
        // be the page going back to the old colour and coming forward again, the blink at the end of a
        // change. Only colours really on screen count: a record that came in with none carried none, and
        // taking its colours over "underneath" then was the page changing in one frame.
        val carried = found != null && (held == found ||
            shift.amount > 0.9f && shift.towards == rowUrl && arrivingNow == found)
        val from = if (carried) null else (palette ?: neutral)
        val fades = from != null && !AppMotion.reduce
        // Back to its start before the colours change under it, not after. Taking the fade over from one
        // still running can wait a frame for it to let go, and on that frame the new colours were drawn
        // at the old fade's strength over the old ones: the page changed, went back, and changed again.
        if (fades) washFade.snapTo(0f)
        fadingFrom = from
        palette = found
        // In the same breath as the colours themselves, so the sleeve lets go of them on a frame where
        // the page is already drawing them.
        shift.adopted = rowUrl
        // As long as the record takes to slide across, so the page and the sleeve arrive together: a
        // song that changes by itself slides its record in over the same stretch (see SleeveCarousel).
        // Theme colours (text, Play, heart) travel with the wash via mixPalette - snapping the theme
        // while only the wash faded left the controls jumping a frame ahead of the page.
        // Counted in frames: this runs on the frames that compose the new song, and timed by the clock
        // a slow one of those carried the page most of the way to its new colour in one step.
        if (fades) washFade.fadeByFrames(1f, stage.colourFadeMs.toFloat())
        fadingFrom = null
    }

    // The colours of a record that has fully arrived, kept until the page itself is wearing them (`held`,
    // declared above). The page used to stop drawing them the moment the sleeve let go of the record,
    // which is one or two frames before it took them on: for those frames the page went back to the
    // record before, and the record growing back into place swept its own soft bottom down over that -
    // the frame of the previous cover that shows as the sleeve zooms in. Latched on a boolean, so it
    // cannot be missed when two changes land in the same frame.
    LaunchedEffect(shift) {
        androidx.compose.runtime.snapshotFlow { shift.amount >= 0.999f }.collect { full ->
            if (full) arrivingNow?.let { held = it }
        }
    }
    // And let go of once the page is wearing them. Held on to, it went on being painted over the page
    // for every song after it - which is a song that ends by itself leaving the page in the colours of
    // whatever was last swiped to.
    LaunchedEffect(palette) { if (held == palette) held = null }

    // A record that has arrived hands its colours over there and then. The page is already drawing them
    // - they came across with the record - so nothing changes on screen; what it prevents is the page
    // ever having to go back to the record before while the song catches up.
    val landedColours by androidx.compose.runtime.rememberUpdatedState(arriving)
    LaunchedEffect(shift) {
        androidx.compose.runtime.snapshotFlow { shift.arrived }.collect { url ->
            if (url == null) return@collect
            val p = landedColours ?: return@collect
            if (p != palette) { fadingFrom = null; palette = p }
            shift.adopted = url
        }
    }

    // Theme follows the same progress as the wash: with the sleeve while it scrolls, then with the
    // post-skip fade when the song changes without a swipe. A hard light/dark cut on the buttons used
    // to fire near the end of a white↔colour cross-fade and look like a snap.
    //
    // Two looks worked out by nori-look, and how far between them the page is, read where each colour
    // is drawn: a fade or a drag redraws what shows the colours and recomposes nothing. It used to mix
    // a palette and build a whole new colour scheme here on every frame, which recomposed the player.
    val live = remember {
        LiveLook { mode ->
            when (mode) {
                MIX_FADE -> washFade.value
                MIX_SLIDE -> shift.amount.let { if (it > 0.001f) it else 0f }
                else -> 1f
            }
        }
    }
    val baseLook = LocalLook.current
    // The sleeve's placeholder and its sheen are in the theme's own colours, never the song's.
    androidx.compose.runtime.SideEffect { if (sleeveArt.neutral !== baseLook) sleeveArt.neutral = baseLook }
    val heldWash = held?.takeIf { it != palette }
    // The plain page (no palette) is the theme's own look, and the page fades to it and from it like to
    // and from any song's colours: the text, the buttons and the wash all at once.
    val settledLook = palette?.look ?: baseTable
    val fromLook = fadingFrom?.look
    val arrivingLook = arriving?.takeIf { it != palette }?.look
    androidx.compose.runtime.SideEffect {
        when {
            heldWash != null -> live.set(null, heldWash.look, MIX_NONE)
            fromLook != null -> live.set(fromLook, settledLook, MIX_FADE)
            arrivingLook != null -> live.set(settledLook, arrivingLook, MIX_SLIDE)
            else -> live.set(null, settledLook, MIX_NONE)
        }
    }

    TintedTheme(palette) {
      androidx.compose.runtime.CompositionLocalProvider(LocalLook provides live) {
        val scheme = MaterialTheme.colorScheme
        if (LocalPlayerShown.current) SystemBarIcons(live)
        Box(
            // Pull down from anywhere on the artwork page and the whole player follows the finger down;
            // the lyrics and queue need a vertical drag to scroll, so there only the handle does.
            Modifier.fillMaxSize().onGloballyPositioned { player[0] = it }
                .then(if (motion != null) Modifier.watchTouches(motion) else Modifier)
                .dragsSheet(sheet, enabled = panel == Panel.ART),
        ) {
            // The page is the cover itself, enlarged and smoothed, lined up with the sleeve. No seam
            // gradient over it: the sleeve carries its own dissolve at its bottom edge, and a gradient
            // anchored to the top of the screen only laid a flat slab over the wash above the sleeve.
            // Built once per page and sleeve place (drawWithCache), then drawn as it is.
            val page = scheme.background
            // On its side (LocalWide): the sleeve is the left half, and its wash and soft edge run across.
            val across = LocalWide.current
            // The cover at the end: the wash is drawn turned round, from the sleeve's edge measured from the right.
            val washTurned = across && LocalCoverAtEnd.current
            fun Modifier.wash(p: PagePalette?): Modifier = (if (washTurned) graphicsLayer { scaleX = -1f } else this).drawWithCache {
                if (p == null) onDrawBehind { drawRect(page) } else {
                    // Lyrics and queue have no sleeve on screen, and a player opened straight into
                    // one of them has never measured it: use where it would be, so those panels get
                    // the same picture behind them rather than one stretched row from the very top.
                    val resting = if (sleeveHeight > 0f) sleeveHeight else if (across) minOf(size.height, size.width / 2f) else size.width / SLEEVE
                    val bottom = if (sleeveBottom > 0f) sleeveBottom else resting
                    if (across) return@drawWithCache sleeveWashAcross(p, bottom, resting)
                    // At the sleeve's size and place whatever the record is doing. A record picked up is
                    // whole above the sleeve's soft band; only in that band does it give way to this,
                    // and the band does not move (see rubOutBottom). The copy used to shrink with the
                    // record, and the page's colours moving about was what caught the eye.
                    sleeveWash(p, bottom, resting)
                }
            }
            // While the colours change, the old page stays underneath and the new one fades in over it.
            // The page does not travel with the record: it is where the record is going, not a second
            // thing sliding about behind it.
            Box(Modifier.matchParentSize().wash(fadingFrom ?: palette))
            if (fadingFrom != null) Box(Modifier.matchParentSize().graphicsLayer { alpha = washFade.value }.wash(palette))
            // The arriving record's page, brought up as the record itself crosses. Only while there is
            // something to bring up: with no colours worked out yet this would be the plain page sliding
            // in, which is worse than the page simply waiting.
            val over = held?.takeIf { it != palette }
            if (over != null) Box(Modifier.matchParentSize().wash(over))
            else if (arriving != null && arriving != palette) Box(
                Modifier.matchParentSize().graphicsLayer { alpha = shift.amount }.wash(arriving),
            )
            // On its side the sleeve is the left half and not the screen's width that these flights grow to
            // and from; there the sheet brings the sleeve up with it instead.
            if (panel == Panel.ART && !across) FlyingCover(sheet, vm.cover(state.current?.coverArt, CoverSize.ROW), sleeveArt, sleeveHeight > 0f)
            // Put away from the lyrics, the cover still travels - from the header's thumbnail to the one
            // in the now playing bar. Without it the lyrics simply went down behind the bar and a cover
            // appeared there out of nothing.
            else if (panel == Panel.LYRICS && !across) FlyingThumb(sheet, vm.cover(state.current?.coverArt, CoverSize.ROW))
            if (flying && !across) PanelFlight(sleeveArt, thumb, sleeveBottom, sleeveHeight, toThumb = panel == Panel.LYRICS) { arrival.value }
            // Artwork, lyrics and queue dissolve into each other rather than cutting. The fade is on the
            // panel itself and not on the whole screen: the transport is the same in all three and is
            // shared across the change, and fading the content it sits in dimmed it half-way. Fading only
            // the incoming panel, with the outgoing one held at full strength until the end, is what left
            // the cover sitting there under the lyrics and then vanishing in a single frame.
            androidx.compose.animation.SharedTransitionLayout {
            androidx.compose.animation.AnimatedContent(
                targetState = panel,
                // The one that is leaving fades out where it stands, which is also what keeps it on
                // screen while it does; the one arriving is brought up by [arrival] instead, so the
                // controls they share are not faded twice over.
                // The one leaving is drawn over the one arriving. AnimatedContent draws the arriving one on
                // top unless told: with the artwork arriving on top, its sleeve covered the queue at once,
                // and closing the queue was a cut instead of a dissolve. Each arrival lower than the last,
                // since a panel keeps the depth it arrived at and the one leaving came in a change before.
                transitionSpec = {
                    (androidx.compose.animation.EnterTransition.None togetherWith
                        androidx.compose.animation.fadeOut(androidx.compose.animation.core.tween(PANEL_MS)))
                        .apply { targetContentZIndex = -changes.toFloat() }
                },
                label = "panel",
            ) { page ->
            // 1 for the panel that is leaving - it has the transition's own fade on top of it - and the
            // arrival for the one coming in, read in the draw phase so a dissolve recomposes nothing.
            val panelFade = FloatReader { if (page == panel) arrived.read() else 1f }
            // The seek bar, the transport, the volume and the icons are in every panel but not at the same
            // height. Shared, only one copy of each is drawn during the dissolve, and it moves from where it
            // was to where it goes; dissolved like the rest, both copies showed and the controls doubled.
            @Composable fun kept(key: String) = Modifier.sharedElement(rememberSharedContentState(key), this@AnimatedContent)
            // The page's text colour, read while the transport draws.
            val ink = androidx.compose.ui.graphics.ColorProducer { live.color(CoverLook.ON) }
            // On its side the controls' half has nothing of its own to scroll, so a pull down anywhere on it puts
            // the player away whatever the panel - as the artwork does - while the lyrics and the queue beside it
            // keep their vertical drag for scrolling.
            PlayerHalves(LocalWide.current, Modifier.dragsSheet(sheet, enabled = panel != Panel.ART), panel = {
                // The artwork bleeds to all three edges like the sleeve it is - up under the status bar
                // as well, which is the whole point: Apple's has no top edge, and giving it one drew a
                // line across the screen. The handle and the close button float over it instead.
                // Queue keeps the screen's side margin, lyrics lay out their own.
                //
                // The sleeve takes exactly its square and no more. Giving it the column's spare height
                // instead left a band of empty wash under it twice as deep as Apple's, because the
                // controls below are shorter than the space that was left over; the spare height now
                // sits above the volume slider, which is where Apple's is.
                // The sleeve draws further down than it takes up: the title and artist are laid out over
                // its last stretch, which is already going soft, the way Apple's are. Measured on `w4`,
                // their cover is sharp to about 53 % of the screen and still leaves a faint trace behind
                // the title at 56.5 % and the artist at 59-61 %. A sleeve that ended above the text left
                // the text sitting on bare page, which is what read as the cover being out of place.
                if (page == Panel.ART) Box(
                    // There or not there, never half there. The panel being left is drawn over this one
                    // and fades out, which is the dissolve; bringing the sleeve up from nothing
                    // underneath it as well means a half-there cover over a page that is already a
                    // blurred copy of the same cover, and a cover that is half there has no soft bottom -
                    // its last rows are rubbed out, so what is left there is the page. But it must stay
                    // away until the change has actually begun: the panel is composed a frame before the
                    // flight starts, and at full strength that frame is the whole sleeve appearing for an
                    // instant before it flies.
                    Modifier.fillMaxWidth()
                        // Away for the whole of a flight as well. `sheet.panelFlight` is written by an
                        // effect, which runs after the frame that started the flight: on that frame the
                        // sleeve was drawn at full size while the cover in flight was drawn too, which
                        // is the two covers that show up together for an instant - one the whole square,
                        // one the square cropped to the sleeve.
                        // The panel being left keeps its sleeve until the flight is really under way:
                        // held back from the frame the panel changed, it went out on that frame while
                        // the flight, which is started by an effect, had not begun - one frame with no
                        // cover on screen at all.
                        // Back from the queue, which covers nothing (rows over the page), the sleeve comes up
                        // under it as the rows fade: at full strength at once it was a cut under a fade.
                        .graphicsLayer {
                            alpha = when {
                                (page == panel && panel != showing) || flying -> 0f
                                page == panel && leaving == Panel.QUEUE -> arrived.read()
                                else -> 1f
                            }
                        }
                        .then(if (across) Modifier.fillMaxHeight() else Modifier.layout { measurable, constraints ->
                            val placeable = measurable.measure(constraints)
                            val takes = (placeable.height * (1f - SLEEVE_UNDER_TEXT)).toInt()
                            layout(placeable.width, takes) { placeable.place(0, 0) }
                        })
                        .onGloballyPositioned {
                            // On its side the sleeve's soft edge is its right one, and the wash carries on
                            // from there under the controls: its right edge and width stand in for the
                            // bottom and height.
                            if (across) {
                                val owner = player[0]?.takeIf { p -> p.isAttached }
                                val left = owner?.localPositionOf(it, Offset.Zero)?.x ?: 0f
                                // Turned round (the cover at the end), from the right: the wash is drawn turned too.
                                sleeveBottom = if (washTurned) (owner?.size?.width ?: 0).toFloat() - left else left + it.size.width
                                sleeveHeight = it.size.width.toFloat()
                                return@onGloballyPositioned
                            }
                            // What is drawn, not what the column was told: the wash lines up with the
                            // picture, and the picture runs on under the title.
                            val drawn = it.size.width / SLEEVE
                            val top = player[0]?.takeIf { p -> p.isAttached }?.localPositionOf(it, Offset.Zero)?.y
                                ?: it.positionInRoot().y
                            sleeveBottom = top + drawn
                            sleeveHeight = drawn
                        },
                ) {
                    // While the sheet moves, the cover on screen is FlyingCover's; this one takes over
                    // the moment the sheet arrives, in exactly the same place.
                    Box(Modifier.graphicsLayer { alpha = if (across || !sheet.panelFlight && (sheet.progress.value >= 1f || sheet.miniCover == Rect.Zero)) 1f else 0f }) {
                        val previousSong = state.queue.getOrNull(state.previousIndex)
                        val nextSong = state.queue.getOrNull(state.nextIndex)
                        Artwork(
                            vm, sleeveArt, coverUrl,
                            previousSong?.let { vm.cover(it.coverArt, CoverSize.FULL) },
                            nextSong?.let { vm.cover(it.coverArt, CoverSize.FULL) },
                            previousTintUrl, nextTintUrl,
                            Songs(state.current?.id, previousSong?.id, nextSong?.id),
                            slide, shift, motion,
                        )
                    }
                } else {
                    // Where the handle used to be. The bar and the close button below it were two more
                    // things to look at for something the page already does - a pull anywhere on the
                    // artwork puts the player away - so only the drag is left, over the strip the lyrics
                    // and the queue cannot have (they need their own vertical drag to scroll).
                    Spacer(Modifier.fillMaxWidth().statusBarsPadding().height(22.dp).dragsSheet(sheet))
                    Box(
                        Modifier.weight(1f).graphicsLayer { alpha = panelFade.read() }
                            // On its side the lyrics and the queue start clear of the camera's punch hole,
                            // as the pages do; only the cover runs under it.
                            .then(
                                if (!across) Modifier
                                else if (LocalCoverAtEnd.current) Modifier.windowInsetsPadding(androidx.compose.foundation.layout.WindowInsets.displayCutout.only(androidx.compose.foundation.layout.WindowInsetsSides.End)).padding(start = LocalUnderControls.current)
                                else Modifier.windowInsetsPadding(androidx.compose.foundation.layout.WindowInsets.displayCutout.only(androidx.compose.foundation.layout.WindowInsetsSides.Start)).padding(end = LocalUnderControls.current),
                            )
                            .then(if (page == Panel.QUEUE) Modifier.padding(horizontal = 26.dp) else Modifier),
                    ) {
                        if (page == Panel.QUEUE) Queue(vm) else LyricsView(vm, actions, state.playing)
                    }
                }

            }, controls = {
                // The column's spare height. Measured off `w4` by row profile, Apple put the transport
                // 10.6 % of the screen above the volume slider and the bottom icons 7.7 % clear of the
                // home indicator. The three controls at the bottom are deliberately closer together than
                // that here - the owner found Apple's own spacing too loose on a 20:9 screen, which is
                // taller than the 19.5:9 those percentages were taken from - and the space that frees
                // up goes underneath them rather than between them.
                if (page == Panel.ART && !across) Spacer(Modifier.weight(0.02f))
                // The lyrics view carries its own header - a thumbnail with the title, the favourite and
                // the menu beside it, the way Apple's does - so this block would be the second copy of it.
                // Shared between the artwork and the queue, where it stands at another height: one copy moves
                // from one place to the other, as the transport does, instead of one blinking out where it
                // was and another fading in where it goes. The lyrics have none (their header is their own),
                // so between those it fades with the panel. On its side the title stays beside the cover for
                // lyrics too, since the lyrics view's own header is portrait-only.
                val titleRow = rememberSharedContentState("title")
                // On its side the controls are one column beside every panel, laid out the same whichever it is:
                // the art's spacing is left out (the compact one lyrics and the queue have reads better there), so
                // nothing in the column moves as the panel changes.
                if (page != Panel.LYRICS || across) Row(
                    Modifier.fillMaxWidth().sharedElement(titleRow, this@AnimatedContent)
                        .graphicsLayer { alpha = if (titleRow.isMatchFound) 1f else panelFade.read() }
                        .padding(start = PLAYER_GUTTER, end = PLAYER_GUTTER, top = 2.dp),
                    Arrangement.spacedBy(10.dp), Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        // Title and artist change with the record; a hard swap while the sleeve is still
                        // sliding reads as a pop. Cross-fade the block on the song id.
                        val meta = remember(state.current, state.radio) { playerTitleMeta(state.current, state.radio) }
                        androidx.compose.animation.AnimatedContent(
                            targetState = meta,
                            transitionSpec = {
                                if (AppMotion.reduce) {
                                    androidx.compose.animation.fadeIn(androidx.compose.animation.core.snap()) togetherWith
                                        androidx.compose.animation.fadeOut(androidx.compose.animation.core.snap())
                                } else {
                                    androidx.compose.animation.fadeIn(androidx.compose.animation.core.tween(220)) togetherWith
                                        androidx.compose.animation.fadeOut(androidx.compose.animation.core.tween(160))
                                }
                            },
                            contentKey = { it.key },
                            label = "player title",
                        ) { m ->
                            Column {
                                LookText(
                                    m.title, { live.color(CoverLook.ON) },
                                    Modifier.readable(key = m.key), style = MaterialTheme.typography.titleLarge,
                                    maxLines = 1, softWrap = false, overflow = TextOverflow.Ellipsis,
                                )
                                // Artist and album, each on its own line and each a way there. On one line they
                                // ran two ellipses into each other as soon as the album had a long name, which
                                // is why the album used to be on the ⋯ menu and nowhere else.
                                //
                                // Apple holds these lines back from the title rather than colouring them: a
                                // saturated accent here is the one thing that made the screen read as Material.
                                LookText(
                                    m.artist, { live.color(CoverLook.ON_60) },
                                    Modifier.clickable(enabled = m.artistId != null) {
                                        m.artistId?.let(nav::artist)
                                    },
                                    style = MaterialTheme.typography.titleMedium,
                                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                                )
                                m.album.takeIf { it.isNotEmpty() }?.let { album ->
                                    LookText(
                                        album, { live.color(CoverLook.ON_45) },
                                        Modifier.clickable(enabled = m.albumId != null) {
                                            m.albumId?.let(nav::album)
                                        },
                                        style = MaterialTheme.typography.bodyMedium,
                                        maxLines = 1, overflow = TextOverflow.Ellipsis,
                                    )
                                }
                            }
                        }
                    }
                    state.current?.let { s ->
                        val starred = marks.effectiveStar(dev.nori.music.data.StarKind.SONG, s.id, s.starred)
                        Row(Modifier, Arrangement.spacedBy(16.dp), Alignment.CenterVertically) {
                            // A heart, as on an album, an artist and a playlist. The star here was the
                            // odd one out, and a song being "starred" while everything else is
                            // "favourited" is a distinction the server makes and nobody else does.
                            // A jam guest's would be the host's.
                            if (!state.jamGuest) TitleCircle(
                                if (starred) Icons.Filled.Favorite else Icons.Filled.FavoriteBorder,
                                say.favourite, starred,
                            ) { actions.star(s, !starred) }
                            TitleCircle(Icons.Filled.MoreHoriz, say.more, false) { playerMenu(s) }
                        }
                    }
                }
                state.error?.let { Text(it, Modifier.padding(horizontal = PLAYER_GUTTER), color = scheme.error, style = MaterialTheme.typography.bodySmall) }
                if (state.bridging) LookText(
                    remember { say.bridging }, { live.color(CoverLook.ON_55) },
                    Modifier.padding(horizontal = PLAYER_GUTTER, vertical = 2.dp),
                    style = MaterialTheme.typography.bodySmall,
                )

                Box(kept("seek")) { SeekBar(vm, state.playing, state.durationMs, seekable = !state.jamGuest) }

                // Three controls, plain glyphs with no containers. Shuffle and repeat live in the queue header.
                // Sized off `w4` as a share of the screen's width: Apple's pause glyph stands 9.8 % of the
                // width tall and the skip glyphs are 9.7 % wide; these were about a fifth smaller. The
                // seek bar, volume bar and bottom icons below were scaled by their own measured ratios.
                // Apple leaves a clear gap between the times and these, rather than letting them follow on.
                // A jam guest has none: the host plays.
                if (!state.jamGuest) Row(kept("transport").fillMaxWidth().padding(top = 24.dp), Arrangement.spacedBy(34.dp, Alignment.CenterHorizontally), Alignment.CenterVertically) {
                    // The buttons send the record across exactly as a swipe does, so the two ways of
                    // changing song look like the same thing happening. A previous that only rewinds
                    // this song is not a record change and gets no slide - there is nothing to slide
                    // to. The rule for which one it is has to match the player's (media3 rewinds
                    // within the first three seconds), so the sleeve and the sound agree.
                    IconButton(
                        {
                            // The player's own rule (`queue_previous_restarts`), so the sleeve and the sound agree.
                            val rewinds = vm.previousRestarts(vm.positionMs, state.previousIndex >= 0)
                            if (rewinds || !slide.ask(1)) vm.previous()
                        },
                        Modifier.size(72.dp),
                    ) { LookIcon(Icons.Filled.FastRewind, say.previous, Modifier.size(55.dp), ink) }
                    IconButton(vm::toggle, Modifier.size(84.dp)) {
                        PlayPauseGlyph(state.playing, state.buffering, 70.dp, 28.dp, ink)
                    }
                    IconButton({ if (!slide.ask(-1)) vm.next() }, Modifier.size(72.dp)) { LookIcon(Icons.Filled.FastForward, say.next, Modifier.size(55.dp), ink) }
                }

                if (page == Panel.ART && !across) Spacer(Modifier.weight(0.17f))
                // Nothing sounds on a jam guest's phone to set the volume of.
                if (!state.jamGuest) Box(kept("volume")) { VolumeRow(vm) }
                val jam by vm.jamStrip.collectAsStateWithLifecycle()
                if (jam != null) Box(Modifier.fillMaxWidth().padding(top = if (state.jamGuest) 16.dp else 0.dp), contentAlignment = Alignment.Center) {
                    JamStrip(jam, live.color(CoverLook.ACCENT))
                }

                // Three slots of fixed shares, so the middle one's words, however long a device's name, never
                // move the lyrics and queue buttons; each keeps the line under its glyph whether it has words or not.
                Row(kept("icons").fillMaxWidth().padding(start = 24.dp, end = 24.dp, top = 2.dp, bottom = 4.dp), verticalAlignment = Alignment.Top) {
                    Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                        PanelButton(Icons.Filled.Lyrics, say.lyrics, page == Panel.LYRICS, nudge = (-1.5).dp) { choose(Panel.LYRICS) }
                        Spacer(Modifier.height(OUTPUT_LINE))
                    }
                    // Apple's middle glyph is AirPlay, not a sleep timer: on this screen the thing worth
                    // one tap is where the sound is going. The sleep timer moved to the ⋯ on the title row,
                    // which is where a setting for the evening belongs.
                    Column(Modifier.weight(1.6f), horizontalAlignment = Alignment.CenterHorizontally) {
                        if (!state.jamGuest) {
                            val panel = LocalLook.current
                            OutputButton(
                                state.playingOn != null, 27.dp,
                                idle = { panel.color(CoverLook.ON_VARIANT) }, lit = { panel.color(CoverLook.ACCENT) },
                            )
                            Text(
                                state.playingOn?.let { words(R.string.devices_playing_on, it) }.orEmpty(), Modifier.height(OUTPUT_LINE),
                                style = MaterialTheme.typography.labelSmall, color = live.color(CoverLook.ACCENT), maxLines = 1, overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                    Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                        PanelButton(Icons.AutoMirrored.Filled.QueueMusic, say.queue, page == Panel.QUEUE, size = 30.dp, nudge = 0.5.dp) { choose(Panel.QUEUE) }
                        Spacer(Modifier.height(OUTPUT_LINE))
                    }
                }
                if (page == Panel.ART && !across) Spacer(Modifier.weight(0.19f))
            })
            }
            }
        }
      }
    }
}

/** The line under the lyrics, output and queue glyphs: the output's "Playing on", or nothing, the same height. */
private val OUTPUT_LINE = 14.dp

/** The larger of the status bar's and the gesture bar's heights, for room kept alike above and below. */
@Composable
private fun barRoom(): androidx.compose.ui.unit.Dp {
    val d = androidx.compose.ui.platform.LocalDensity.current
    val top = androidx.compose.foundation.layout.WindowInsets.statusBars.getTop(d)
    val bottom = androidx.compose.foundation.layout.WindowInsets.navigationBars.getBottom(d)
    return with(d) { maxOf(top, bottom).toDp() }
}

/**
 * On its side: how far a cover reaches in under the words and controls beside it (the player's, a page's), and
 * how much wider its soft edge is than upright, so it goes soft over that stretch rather than ending at the
 * controls' start.
 */
internal val UNDER_TEXT = 96.dp
internal const val ACROSS_MELT = 1.5f

/** How much of the sleeve's panel lies under the controls on its side ([PlayerHalves]): the lyrics and queue keep off it. */
private val LocalUnderControls = androidx.compose.runtime.compositionLocalOf { 0.dp }

/**
 * The player's two parts: [panel] - the sleeve, the lyrics or the queue - and [controls] - the title, the seek
 * bar, the transport, the volume and the icons. One above the other on a phone held upright, as they always
 * were. On its side ([LocalWide]) the screen has no height for that - the sleeve alone filled it and pushed
 * everything else off - so they stand side by side: the panel on the left, as wide as the screen is tall
 * (the sleeve its whole square), the controls down the rest.
 */
@Composable
private fun PlayerHalves(wide: Boolean, controlsDrag: Modifier, panel: @Composable ColumnScope.() -> Unit, controls: @Composable ColumnScope.() -> Unit) {
    if (!wide) Column(Modifier.fillMaxSize().navigationBarsPadding()) { panel(); controls() }
    else androidx.compose.foundation.layout.BoxWithConstraints(Modifier.fillMaxSize()) {
        // Wider than it is tall: the sleeve is a band across the cover, cropped above and below rather than at
        // the sides. The controls keep their place (the square's edge) and stand over the sleeve's soft edge,
        // which runs on under them towards the middle; the lyrics and the queue stop where the controls start.
        val controlsAt = minOf(maxHeight, maxWidth * 0.5f)
        // Clearly wider than tall, reaching [UNDER_TEXT] in under the controls, which keep their place.
        val side = minOf(controlsAt + UNDER_TEXT, maxWidth * 0.7f)
        // The cover at the end (LocalCoverAtEnd): the same two halves, the other way round.
        val atEnd = LocalCoverAtEnd.current
        Box(Modifier.fillMaxSize()) {
            androidx.compose.runtime.CompositionLocalProvider(LocalUnderControls provides (side - controlsAt).coerceAtLeast(0.dp)) {
                Column(Modifier.width(side).fillMaxHeight().align(if (atEnd) Alignment.TopEnd else Alignment.TopStart)) { panel() }
            }
            Column(
                // Clear of the camera's punch hole too, which is on this side when the phone is turned the other way.
                // The whole half, edge to edge, takes the pull down ([controlsDrag]) before the insets are kept off.
                // The same room kept above as below (the gesture bar's, or the status bar's if it is shown), so the
                // controls stand in the middle of the screen's height rather than of what is above the gesture bar.
                Modifier.padding(start = if (atEnd) 0.dp else controlsAt, end = if (atEnd) controlsAt else 0.dp).fillMaxSize().then(controlsDrag).padding(vertical = barRoom())
                    .windowInsetsPadding(androidx.compose.foundation.layout.WindowInsets.displayCutout.only(if (atEnd) androidx.compose.foundation.layout.WindowInsetsSides.Start else androidx.compose.foundation.layout.WindowInsetsSides.End))
                    .padding(start = if (atEnd) 0.dp else 8.dp, end = if (atEnd) 8.dp else 0.dp),
                verticalArrangement = Arrangement.Center,
            ) { controls() }
        }
    }
}

private data class PlayerTitleMeta(
    val key: String?, val title: String, val artist: String, val album: String,
    val artistId: String?, val albumId: String?,
)

private fun playerTitleMeta(song: dev.nori.music.ffi.model.Song?, radio: String?) = song?.let {
    PlayerTitleMeta(it.id, it.title, it.artist, it.album.orEmpty(), it.artistId, it.albumId)
} ?: PlayerTitleMeta(null, say.playerIdle(radio), "", "", null, null)

/**
 * Where the sound is going, and one tap to change it. The glyph says which kind of output is carrying
 * the music, the way Apple's AirPlay mark fills in when something is connected.
 *
 * The picker itself is Android's own: `Settings.Panel.ACTION_MEDIA_OUTPUT` is the documented way in
 * and lists Bluetooth, wired, USB and any Cast target the system knows about - far more than this app
 * could offer on its own, and the same sheet the media notification opens. Some builds do not carry
 * that panel; they get SystemUI's dialog, and a device with neither is simply told what it is playing
 * through rather than being left with a button that does nothing.
 */
@Composable
internal fun OutputButton(
    otherDevice: Boolean, size: androidx.compose.ui.unit.Dp,
    idle: androidx.compose.ui.graphics.ColorProducer, lit: androidx.compose.ui.graphics.ColorProducer,
) {
    val settings: SettingsViewModel = viewModel()
    val output by settings.currentOutput.collectAsStateWithLifecycle()
    val context = LocalContext.current
    // Whether the sound has gone elsewhere is the core's (`output_look`).
    val o = remember(output) { dev.nori.music.ffi.devices.outputLook(output) }
    // One speaker for every output: filled and lit while the sound is elsewhere, an outline while it is here.
    val elsewhere = o.elsewhere || otherDevice
    val icon = if (elsewhere) Icons.Filled.Speaker else Icons.Outlined.Speaker
    val description = remember(o) { say.outputDescription(o.port, o.name) }
    // With remote control or jams on, the button opens nori's own devices first (RemoteScreens); this
    // phone's outputs are one row of it.
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    val devices = prefs.remoteControl || prefs.jam
    val openDevices = LocalDevices.current
    val tell = LocalMessages.current
    IconButton({ if (devices) openDevices() else openOutputPicker(context, output, tell) }) {
        LookIcon(icon, description, Modifier.size(size)) { if (elsewhere) lit() else idle() }
    }
}

internal fun openOutputPicker(context: android.content.Context, output: String, tell: (String) -> Unit) {
    // Android 14 and later have a public call for exactly this, and it is the one that works on a
    // current phone: the same output switcher the media controls open, listing Bluetooth, wired, USB
    // and any Cast target the system knows about.
    if (android.os.Build.VERSION.SDK_INT >= 34 &&
        runCatching { android.media.MediaRouter2.getInstance(context).showSystemOutputSwitcher() }.getOrDefault(false)
    ) return
    // Android 11 to 13: SystemUI opens the same dialog on a *broadcast*, not an activity. The first
    // version of this started it as an activity, which can never resolve - on a phone the button did
    // nothing but name the output.
    if (android.os.Build.VERSION.SDK_INT >= 30) {
        val sent = runCatching {
            context.sendBroadcast(
                android.content.Intent("com.android.systemui.action.LAUNCH_MEDIA_OUTPUT_DIALOG")
                    .setPackage("com.android.systemui")
                    .putExtra("package_name", context.packageName),
            )
        }.isSuccess
        if (sent && android.os.Build.VERSION.SDK_INT < 34) return
    }
    // Android 10: the Settings panel.
    if (android.os.Build.VERSION.SDK_INT >= 29 && runCatching {
            context.startActivity(
                android.content.Intent("android.settings.panel.action.MEDIA_OUTPUT")
                    .putExtra("com.android.settings.panel.extra.PACKAGE_NAME", context.packageName)
                    .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        }.isSuccess
    ) return
    tell(dev.nori.music.ffi.devices.outputLook(output).let { say.playingThrough(say.outputLabel(it.port, it.name)) })
}

/**
 * The player's side margin. Apple keeps its title, seek bar and title-row buttons 8.2 % of the
 * screen's width in from each edge - measured on `w4` - which on a 411 dp wide phone is 33 dp; ours sat
 * at 26 and 16, so everything read as pushed against the sides.
 */
private val PLAYER_GUTTER = 33.dp

/** How long the artwork, the lyrics and the queue take to dissolve into one another (the core's). */
private val PANEL_MS: Int get() = stage.panelMs

// Which clock the player's [LiveLook] is on: none (one look), the post-skip fade, or the sleeve's slide.
private const val MIX_NONE = 0
private const val MIX_FADE = 1
private const val MIX_SLIDE = 2

/**
 * Width over height of the player's sleeve. Album art is square - Apple's too - so theirs is the
 * square scaled up and cropped at the left and right edges to fill a taller box. Crop the region of
 * `w4` that spans where a full-width square would have ended and the flowers below that line are as
 * sharp as the ones above it, with a strip of red tape running across it unbroken: it is the picture,
 * not the blur behind it. That is the whole trick, and it is why their sleeve can touch the top edge
 * and still reach down behind the title, which no square can do. A phone's box: a desktop window
 * lays its player out otherwise, so the ratio is the phone's layout and not the core's.
 */
private const val SLEEVE = 0.74f

/**
 * How much of the sleeve's height runs on underneath the title block instead of above it. With the
 * sleeve at [SLEEVE] this puts the title where `w4` has it, 56.5 % of the screen, with the picture's
 * blurred tail behind it.
 */
private const val SLEEVE_UNDER_TEXT = 0.095f

/** A soft sleeve's own box as the sleeve: its bottom (its right edge, on its side) and its whole height (width). */
private val SLEEVE_ALL: androidx.compose.ui.draw.CacheDrawScope.() -> Pair<Float, Float> = { size.height to size.height }

/**
 * The artwork full-bleed: edge to edge, square corners, no shadow - the sleeve it is. Its bottom
 * third melts into the page wash (transparent to the wash colour at that height), so there is no
 * line where the picture ends - the same dissolve the album page uses.
 */
@Composable
private fun Artwork(
    vm: PlayerViewModel, art: SleeveArt, currentUrl: String?,
    previousUrl: String?, nextUrl: String?,
    /** The same two records at the size the colours are worked out from; see PageShift. */
    previousTint: String?, nextTint: String?,
    songs: Songs,
    slide: SleeveSlide, shift: PageShift,
    /** The moving cover, or null while it is switched off. */
    motion: SleeveMotion?,
) {
    Box(Modifier.fillMaxWidth(), Alignment.TopCenter) {
        Box(
            // Not square. Measure `w4` and Apple's sleeve runs from the very top edge of the screen down
            // to about half of it - 977 wide by roughly 1050 tall - so it is the cover scaled to fill and
            // cropped a little at the sides. That is how it manages to have no top edge *and* reach down
            // behind the title; a full-width square can only do one or the other. Cover crops already.
            if (LocalWide.current) Modifier.fillMaxSize() else Modifier.fillMaxWidth().aspectRatio(SLEEVE),
        ) {
            SleeveCarousel(
                art, currentUrl, previousUrl, nextUrl, previousTint, nextTint, songs,
                onPrevious = vm::previousItem, onNext = vm::next, slide = slide, shift = shift, motion = motion,
            )
            // Just enough shade under the status bar for its icons to read on a pale cover; the same
            // amount the album page uses, and invisible against anything darker.
            // On its side the sleeve's right edge goes soft (SoftSleeve), and the shade goes with it: stopping
            // where the sleeve does, it was a darker block with a hard edge across the top of the soft band.
            val across = LocalWide.current
            val turned = across && LocalCoverAtEnd.current
            Box(
                Modifier.fillMaxWidth().fillMaxHeight(stage.statusShadeTo)
                    .then(if (across) Modifier.graphicsLayer { compositingStrategy = androidx.compose.ui.graphics.CompositingStrategy.Offscreen; if (turned) scaleX = -1f }
                        .rubOutBottom(across = true) { (size.width - size.width * MELT * ACROSS_MELT) to size.width } else Modifier)
                    .background(remember { Brush.verticalGradient(0f to Color.Black.copy(alpha = stage.statusShade), 1f to Color.Transparent) }),
            )
            // Nothing is drawn here to soften the sleeve's bottom. There is one blurred copy of the
            // cover on this screen - the page's - and it sits still behind everything at the sleeve's
            // own size. The records are rubbed out over the sleeve's last rows (see SleeveCarousel), a
            // band that stays put whatever the records do, so the page's blur shows through it.
        }
    }
}

/**
 * The sleeve's soft bottom: a band of rows at the sleeve's bottom edge rubbed out of whatever is drawn
 * over them, so that a record ends by going soft into the page instead of on a line. What shows
 * through is the page's own blurred copy of the cover, drawn at the sleeve's size and place.
 *
 * The band belongs to the sleeve, not to the records: it is the same rows of the screen whether a
 * record is lying flat, lifted and small, sliding past or flying in from the now playing bar. A record
 * picked up is a whole square above the band; put down, it grows back into it and goes soft where it
 * always did. Every version of this that gave each record its own soft bottom had that softness
 * travel and change size with the record - a blur moving about the screen - and, with two records
 * side by side, a seam between two blurs.
 */
private fun rubOutBrush(top: Float, bottom: Float, across: Boolean = false): Brush =
    // The melt's own easing, in stops, and why its tail finishes by 70 %: nori_look::sleeve::RUB_OUT.
    alphaGradient(stage.rubOut, Color.Black, top, bottom, across)

/**
 * Rubs the band from [top] to [bottom] out of what [content] drew; the brush is made once per place.
 * [across]: the band runs down the right edge instead, from x [top] to [bottom].
 */
private fun Modifier.rubOutBottom(across: Boolean = false, band: androidx.compose.ui.draw.CacheDrawScope.() -> Pair<Float, Float>): Modifier = drawWithCache {
    val (top, bottom) = band()
    val brush = if (bottom > top) rubOutBrush(top, bottom, across) else null
    val at = if (across) Offset(top, 0f) else Offset(0f, top)
    val area = if (across) Size(bottom - top, size.height) else Size(size.width, bottom - top)
    onDrawWithContent {
        drawContent()
        if (brush != null) drawRect(brush, topLeft = at, size = area, blendMode = androidx.compose.ui.graphics.BlendMode.DstOut)
    }
}

/**
 * The same flight as [FlyingCover] between two thumbnails: the lyrics header's and the now playing
 * bar's. One picture moving and changing size, never one swapped for another, and the corners round
 * off from the one to the other on the way.
 */
@Composable
private fun FlyingThumb(sheet: PlayerSheet, url: String?) {
    val flying by remember { androidx.compose.runtime.derivedStateOf { sheet.progress.value < 1f } }
    val to = sheet.panelCover
    if (!flying || sheet.miniCover == Rect.Zero || to == Rect.Zero) return
    val density = androidx.compose.ui.platform.LocalDensity.current
    val side = with(density) { to.height.toDp() }
    val fromRadius = with(density) { 7.dp.toPx() }
    val toRadius = with(density) { 9.dp.toPx() }
    val shapes = remember { CornerShapes() }
    Box(Modifier.fillMaxSize()) {
        Box(
            Modifier.requiredSize(side).align(Alignment.TopStart)
                .graphicsLayer {
                    val t = sheet.progress.value.coerceIn(0f, 1f)
                    val from = sheet.miniCover.translate(0f, -sheet.travel)
                    fun mix(a: Float, b: Float) = a + (b - a) * t
                    val k = (mix(from.height, to.height) / to.height).coerceAtLeast(0.01f)
                    transformOrigin = androidx.compose.ui.graphics.TransformOrigin(0f, 0f)
                    scaleX = k; scaleY = k
                    translationX = mix(from.left, to.left)
                    translationY = mix(from.top, to.top)
                    shape = shapes.of(mix(fromRadius, toRadius) / k)
                    clip = true
                },
        ) { Cover(url, side, radius = 0.dp) }
    }
}

/**
 * The cover between the artwork and the lyrics: the sleeve shrinks into the lyrics header's thumbnail
 * and grows back out of it, one picture the whole way. Both ends stand their own copy down while this
 * runs (PlayerSheet.panelFlight), so there is never a second cover on screen.
 *
 * It is the same trick as the flight out of the now playing bar below: the square is laid out once at
 * the sleeve's size and only moved and scaled by a layer, so nothing is measured or decoded again
 * while it travels. [progress] is the panel change's own 0..1, read in the draw phase.
 */
@Composable
private fun PanelFlight(
    art: SleeveArt, thumb: Rect, sleeveBottom: Float, sleeveHeight: Float, toThumb: Boolean,
    progress: FloatReader,
) {
    val density = androidx.compose.ui.platform.LocalDensity.current
    val side = with(density) { sleeveHeight.toDp() }
    val thumbRadius = with(density) { 9.dp.toPx() }
    val sleeveRadius = with(density) { 2.dp.toPx() }
    val shapes = remember { CornerShapes() }
    // 0 at the sleeve, 1 at the thumbnail, whichever way the change is going.
    val away = FloatReader { progress.read().coerceIn(0f, 1f).let { if (toThumb) it else 1f - it } }
    // How much of the sleeve's own dressing the cover still has: all of it in the sleeve, none by the
    // time it is under half way to the thumbnail.
    val dressed = FloatReader { smooth(1f - away.read(), 0.55f, 1f) }
    androidx.compose.foundation.layout.BoxWithConstraints(Modifier.fillMaxSize()) {
        val w = constraints.maxWidth.toFloat()
        // The sleeve's soft bottom stays where the sleeve's is (see SoftSleeve): the record flying through
        // those rows goes soft there and is whole everywhere else, and its blur leaves with the record and
        // comes back as it lands. The cover never goes below the sleeve's bottom on the way (the thumbnail
        // is in the header above it), so the layer stops there rather than at the screen's foot.
        SoftSleeve(
            Modifier.fillMaxWidth().height(with(density) { sleeveBottom.toDp() }),
            sleeve = { sleeveBottom to sleeveHeight }, blur = dressed,
        ) {
            Box(
                Modifier.requiredSize(side).align(Alignment.TopStart)
                    .graphicsLayer {
                        val t = away.read()
                        val eased = t * t * (3f - 2f * t)
                        fun mix(a: Float, b: Float) = a + (b - a) * eased
                        val k = (mix(sleeveHeight, thumb.height) / sleeveHeight).coerceAtLeast(0.01f)
                        transformOrigin = androidx.compose.ui.graphics.TransformOrigin(0f, 0f)
                        scaleX = k; scaleY = k
                        // The sleeve is the square cropped by the screen's edges, so it starts wider than
                        // the screen and centred on it; layout has already put it there, which `overhang`
                        // takes back out before the travel is applied.
                        val overhang = (w - size.width) / 2f
                        translationX = mix((w - sleeveHeight) / 2f, thumb.left) - overhang
                        translationY = mix(sleeveBottom - sleeveHeight, thumb.top)
                        shape = shapes.of(mix(sleeveRadius, thumbRadius) / k)
                        clip = true
                    },
            ) {
                SleeveImage(art, Modifier.fillMaxSize())
                SleeveShade(dressed)
            }
        }
    }
}

/**
 * The sleeve's soft bottom round whatever [content] draws in it - the records at rest or in the hand, the
 * cover flying up from the now playing bar, the cover shrinking into the lyrics. There is one of these
 * wherever the sleeve is drawn, so its bottom is the same thing in all of them and nothing about it
 * changes on the frame one hands over to the next.
 *
 * Two parts, both in [content]'s own layer, or the erase would take the page behind it too. Rubbing out
 * only fades: a sharp line in the picture inside the band - a frame, a black strip - stays a sharp line,
 * fainter. So a blurred copy of the content is faded in over it just above the band, and the band is
 * rubbed out of both (see rubOutBottom): sharp, then soft, then the page's own blur, with no step
 * between. The blurred copy is Android 12 and later; a switch in Appearance turns it off. [content] is
 * composed a second time as that copy and told so, for anything that can only be drawn once.
 *
 * [sleeve] is the sleeve's bottom and height in this box's own pixels. [blur] is how much of the blurred
 * copy is drawn, read in the draw phase: 1 for a sleeve at rest, less as the picture leaves it. Only the
 * resting sleeve had the blur at first, so it vanished on the first frame of every move away from it and
 * came back on the last frame of every move into it - when a record was put down, when the player
 * opened, when the lyrics closed. Now it leaves and comes back with the move itself.
 */
@Composable
internal fun SoftSleeve(
    modifier: Modifier,
    sleeve: androidx.compose.ui.draw.CacheDrawScope.() -> Pair<Float, Float> = SLEEVE_ALL,
    blur: FloatReader = FloatReader { 1f },
    content: @Composable androidx.compose.foundation.layout.BoxScope.(blurred: Boolean) -> Unit,
) {
    val soft = android.os.Build.VERSION.SDK_INT >= 31 &&
        androidx.lifecycle.viewmodel.compose.viewModel<dev.nori.music.app.vm.SettingsViewModel>().prefs.collectAsStateWithLifecycle().value.softSleeve
    val blurPx = with(androidx.compose.ui.platform.LocalDensity.current) { 22.dp.toPx() }
    // A white, cream or black page has a wash in its own tint only (nori_look's wash), and the blurred band
    // has to arrive at the same thing: blurred as it is, A Beautiful Lie's red lettering spread across its
    // white bottom as a pink haze over a grey page. So on such a page the band keeps its light and dark
    // but takes the page's tint - grey on white, cream on cream, whatever the page is, nothing fixed. A
    // coloured page keeps the band's colours, as its wash does. Which it is, and the tint, are the look's
    // (nori_look::dress); read in the draw phase.
    val band = if (android.os.Build.VERSION.SDK_INT >= 31) remember(blurPx) { BandEffect(blurPx) } else null
    val look = LocalLook.current
    // On its side the sleeve's soft edge is its right one, towards the controls: the same band and blur,
    // turned. [sleeve] is then its right edge and width.
    val across = LocalWide.current
    val edge: androidx.compose.ui.draw.CacheDrawScope.() -> Pair<Float, Float> = if (across && sleeve === SLEEVE_ALL) { { size.width to size.width } } else sleeve
    // With the cover at the end (a car's driver on the left) the sleeve is turned round whole, soft edge and
    // blur with it, and the picture turned back inside it: the soft edge faces the controls, never the picture.
    val mirror = across && LocalCoverAtEnd.current
    Box(
        modifier
            .graphicsLayer { compositingStrategy = androidx.compose.ui.graphics.CompositingStrategy.Offscreen; if (mirror) scaleX = -1f }
            .rubOutBottom(across) { val (bottom, height) = edge(); (bottom - height * MELT * (if (across) ACROSS_MELT else 1f)) to bottom },
    ) {
        Unturned(mirror) { content(false) }
        if (soft && band != null) Box(
            Modifier.fillMaxSize()
                // At nought the layer is skipped whole, blur and all.
                .graphicsLayer {
                    compositingStrategy = androidx.compose.ui.graphics.CompositingStrategy.Offscreen
                    alpha = blur.read().coerceIn(0f, 1f)
                }
                .drawWithCache {
                    // Where the blurred copy shows: nowhere above the band's upper reach, all of it by the
                    // time the rub-out is under way. Eased, so its own start is no line. One brush per
                    // size and place, not one per frame.
                    val (bottom, height) = edge()
                    val top = bottom - height
                    val mask = alphaGradient(stage.soft, Color.Black, top + height * stage.softFrom, top + height * stage.softTo, across)
                    onDrawWithContent {
                        drawContent()
                        drawRect(mask, blendMode = androidx.compose.ui.graphics.BlendMode.DstIn)
                    }
                },
        ) {
            Box(Modifier.fillMaxSize().graphicsLayer { renderEffect = band.of(look) }) { Unturned(mirror) { content(true) } }
        }
    }
}

/** [content] turned back the right way round inside something turned round ([turned]), or as it is. */
@Composable
private fun Unturned(turned: Boolean, content: @Composable androidx.compose.foundation.layout.BoxScope.() -> Unit) {
    if (!turned) Box(Modifier.fillMaxSize(), content = content)
    else Box(Modifier.fillMaxSize().graphicsLayer { scaleX = -1f }, content = content)
}

/**
 * The shade under the status bar, on a picture travelling into or out of the sleeve: just enough for the
 * status bar's icons to read on a pale cover, as the sleeve has. It belongs to the sleeve, so it arrives
 * with the picture - at [strength] - rather than being there from the thumbnail on, or missing until the
 * sleeve takes over and then there in one frame.
 */
@Composable
private fun SleeveShade(strength: FloatReader) {
    Box(
        Modifier.fillMaxSize().graphicsLayer { alpha = strength.read() }.drawWithCache {
            val shade = Brush.verticalGradient(
                0f to Color.Black.copy(alpha = stage.statusShade), 1f to Color.Transparent,
                startY = 0f, endY = size.height * stage.statusShadeTo,
            )
            val area = androidx.compose.ui.geometry.Size(size.width, size.height * stage.statusShadeTo)
            onDrawBehind { drawRect(shade, size = area) }
        },
    )
}

/** 0 up to [from], 1 from [to] on, and a smooth step in between. */
private fun smooth(x: Float, from: Float, to: Float): Float {
    val t = ((x - from) / (to - from)).coerceIn(0f, 1f)
    return t * t * (3f - 2f * t)
}

/**
 * The cover in flight: from the mini player's thumbnail to the sleeve, one picture changing size and
 * place with the sheet's progress, never a thumbnail swapped for a sleeve. It rides in the sheet's own
 * coordinates - the thumbnail's place relative to the mini player's top at 0, the sleeve's at 1 - so it
 * moves with the sheet and only has to grow.
 *
 * It is the whole square the entire way, never a cropped window on it: as it grows it simply runs off
 * both sides of the screen, and at the end the screen's own edges crop it to exactly what the sleeve
 * shows (the sleeve is the square cropped at the sides), so the last frame is the real sleeve pixel
 * for pixel and the hand-over cannot be seen. A window narrowing from square to the sleeve's shape
 * read as the picture being cut while it moved.
 *
 * The picture is laid out once, as the full square at the sleeve's height, and moved and grown only
 * by a layer transform. Growing it by layout
 * instead gave the image a new size every frame, and each new size was a new decode and a new texture:
 * the flight stalled for a third of a second at a time.
 *
 * The small rendition the mini player already has sits under the large one, so the first frame of a
 * flight shows the picture even before the large one has come out of the cache.
 */
@Composable
private fun FlyingCover(sheet: PlayerSheet, rowUrl: String?, art: SleeveArt, measured: Boolean) {
    val flying by remember { androidx.compose.runtime.derivedStateOf { sheet.progress.value < 1f } }
    if (!flying || !measured || sheet.miniCover == Rect.Zero) return
    val density = androidx.compose.ui.platform.LocalDensity.current
    val thumbRadius = with(density) { 7.dp.toPx() }
    val shapes = remember { CornerShapes() }
    // How much of the sleeve's own dressing - the blurred bottom, the shade under the status bar - the
    // cover has on its way: none for the first half of the climb, all of it on arrival, where the sleeve
    // takes over with its own. Without it both were missing for the whole flight and appeared on the frame
    // the sleeve took over, and went on the first frame of a pull down.
    val dressed = FloatReader { smooth(sheet.progress.value, 0.5f, 1f) }
    androidx.compose.foundation.layout.BoxWithConstraints(Modifier.fillMaxSize()) {
        val w = constraints.maxWidth.toFloat()
        val h = w / SLEEVE
        val side = with(density) { h.toDp() }
        // The sleeve's soft bottom is there before the record arrives and stays when it has gone: the same
        // rows of the sheet, rubbed out of whatever flies through them (see SoftSleeve). The cover never
        // leaves the sleeve's rows on the way - it grows from the thumbnail near the sheet's top edge into
        // the sleeve that starts there - so the layer is the sleeve's size, not the screen's.
        SoftSleeve(Modifier.fillMaxWidth().height(side), blur = dressed) {
            Box(
                Modifier.requiredSize(side).align(Alignment.TopStart)
                    .graphicsLayer {
                        val t = sheet.progress.value.coerceIn(0f, 1f)
                        val from = sheet.miniCover.translate(0f, -sheet.travel)
                        fun mix(a: Float, b: Float) = a + (b - a) * t
                        val k = (mix(from.height, h) / h).coerceAtLeast(0.01f)
                        transformOrigin = androidx.compose.ui.graphics.TransformOrigin(0f, 0f)
                        scaleX = k; scaleY = k
                        // The square grows about its own middle and travels from the middle of the
                        // thumbnail to the middle of the screen, which is where the sleeve's middle is.
                        // Carrying its left edge instead - what this did - left it hanging in the top left
                        // corner of the sheet for the whole climb, with the page showing down the right
                        // hand side, and it arrived a little way off the sleeve it was handing over to.
                        val drawn = size.width * k
                        // Sideways it is most of the way over before it is half way up. The thumbnail sits
                        // at the very left of the bar and the sheet under it is already the full width, so
                        // a square that crosses at the same rate as it climbs spends the whole climb in the
                        // corner with the page showing beside it. Easing only this axis keeps both ends
                        // exact - the thumbnail at the start, the sleeve at the end - and has the record
                        // under the middle of the screen by the time the sheet is half way, after which it
                        // only grows.
                        val across = (t / 0.45f).coerceAtMost(1f).let { 1f - (1f - it) * (1f - it) }
                        // The square is wider than the screen, and layout centres anything wider than the
                        // room it was given: it is already sitting `overhang` to the left (a negative
                        // number) before any of this moves it. Leaving that out put the record that far off
                        // centre for the whole flight - its left side already cropped by the screen while
                        // its right had a gap - and it jumped right by the same amount when the sleeve
                        // took over at the end.
                        val overhang = (w - size.width) / 2f
                        translationX = from.center.x + (w / 2f - from.center.x) * across - drawn / 2f - overhang
                        translationY = mix(from.center.y, size.height / 2f) - size.height * k / 2f
                        shape = shapes.of(thumbRadius * (1f - t) / k)
                        clip = true
                    },
            ) {
                // The sleeve's placeholder, then the bar's small picture of this song over it while the
                // large one is not there, then the sleeve's own picture.
                Box(Modifier.fillMaxSize().drawBehind { drawRect(art.plate) })
                if (art.current == null) Cover(rowUrl, 0.dp, Modifier.fillMaxSize(), radius = 0.dp, plate = false)
                SleeveImage(art, Modifier.fillMaxSize(), plate = false)
                SleeveShade(dressed)
            }
        }
    }
}

/**
 * The sleeve's picture across songs. A picture at hand changes with the song, cross-fading over the
 * last one. One that still has to be read or fetched gets a moment ([HOLD_MS], so a cover on the disk
 * comes without the placeholder blinking in first); after that the last song's picture fades out to the
 * placeholder - a plate in the theme's own surface colour, no song's colour, with the loading sheen over
 * it - and the new one fades in from there when it comes. Never the last song's cover under the new
 * song's title for longer than that moment, and never a picture that came back for a song skipped past
 * (see [CoverTurn]). The very first picture fades in from the plate too, unless it came straight from
 * memory, where a fade would only be a delay.
 *
 * One of these feeds both the sleeve and the cover in flight: two requests for the same picture in the
 * same frame each decoded their own bitmap, and the second was uploaded to the GPU on the frame the
 * sleeve took over - a stall exactly at the landing.
 */
@androidx.compose.runtime.Stable
private class SleeveArt {
    /** What is showing, fading in over [previous] at [fade]. */
    var current by mutableStateOf<androidx.compose.ui.graphics.painter.Painter?>(null)
    val fade = androidx.compose.animation.core.Animatable(1f)
    /** The picture being left, at [previousAlpha]. */
    var previous by mutableStateOf<androidx.compose.ui.graphics.painter.Painter?>(null)
    val previousAlpha = androidx.compose.animation.core.Animatable(1f)
    var loading by mutableStateOf(true)
    /** The address of [current]: what a swipe waits for before it hands the sleeve back. */
    var shownUrl by mutableStateOf<String?>(null)
    /** The next picture goes straight in: a swipe has already slid it into place. */
    var snapNext = false
    /**
     * The cover of the song on the page, as the sleeve was last composed with it: what a record that
     * slides in by itself shows while the sleeve underneath is still changing (see SleeveCarousel).
     */
    var cover: CoverImage? = null
    /**
     * The theme's own look, no song's colours in it: the placeholder under the picture is its surface and
     * the sheen over the placeholder its ink, whatever song is playing. Read while drawing.
     */
    var neutral by mutableStateOf<Look>(FixedLook(IntArray(CoverLook.LEN)))
    /** The placeholder's colour. */
    val plate: Color get() = neutral.color(CoverLook.SURFACE_VARIANT)
    /** Which song's picture is wanted, and when the placeholder is due. */
    val turn = CoverTurn(HOLD_MS)
    /**
     * A record slid in for this address with no picture on it, or with its picture only part way in: the
     * sleeve is at the plate for it already, and the old picture is not to come back while the player
     * catches up with the change.
     */
    var clearedFor by mutableStateOf<String?>(null)
    /** Where the next picture's fade starts: how far in the record that slid in had it; below 0 for no such record. */
    var fadeFrom = -1f

    /**
     * A record has slid in over the sleeve for [url] without its whole picture ([level] of it, 0 for none):
     * the sleeve under it becomes the plate at once, since the picture that was on it has slid away, and the
     * new one carries on from [level] when the sleeve has it.
     */
    fun clearFor(url: String?, level: Float) {
        current = null
        previous = null
        shownUrl = null
        loading = level <= 0f
        fadeFrom = level.coerceIn(0f, 1f)
        clearedFor = url
    }

    /** Lets the current picture go, fading it out to the plate. */
    suspend fun letGo() {
        val leaving = current ?: return
        previous = leaving; current = null
        if (AppMotion.reduce) previousAlpha.snapTo(0f)
        else { previousAlpha.snapTo(1f); previousAlpha.animateTo(0f, androidx.compose.animation.core.tween(PLATE_OUT_MS)) }
        previous = null
    }
}

/** How long a song whose cover is not at hand keeps the last picture before the placeholder: nori-core's `stage`. */
private val HOLD_MS: Long get() = stage.sleeveHoldMs

/** The last picture fading out to the placeholder. */
private const val PLATE_OUT_MS = 360

/** A picture fading in over the placeholder. */
private const val PICTURE_IN_MS = 320

/** A picture fading in over the last song's. */
private const val PICTURE_OVER_MS = 480

/** How long the sleeve waits on a record slid in without a picture for the player to take the change. */
private const val CLEARED_WAIT_MS = 1_500L

/**
 * The sleeve as one record in a row of them: a sideways drag slides it and brings the next (or the
 * last) record in from the other edge, already drawn, the way Apple's does. Let go past a third of the
 * way, or flicked, the old one goes all the way off and the new one all the way in, and only then does
 * the song change. The new record then stays drawn over the sleeve until the sleeve has the same
 * picture, so there is no second change: no fade, no plate, no old cover coming back for a frame.
 *
 * The neighbours' pictures come from the cache the player keeps warm (PlayerViewModel's covers ahead),
 * so they are normally there before the finger is. Nothing here runs until a finger is down.
 */
@Composable
private fun SleeveCarousel(
    art: SleeveArt, currentUrl: String?, previousUrl: String?, nextUrl: String?,
    previousTint: String?, nextTint: String?, songs: Songs,
    onPrevious: () -> Unit, onNext: () -> Unit, slide: SleeveSlide, shift: PageShift,
    motion: SleeveMotion?,
) {
    val scope = rememberCoroutineScope()
    val haptics = LocalHapticFeedback.current
    // Where the record is, in pixels, written straight from the finger. It was an Animatable, snapped
    // to from a coroutine per pointer event; on a flick several of those were still queued when the
    // finger left, and they landed on top of the animation that had already started and dragged the
    // record back - the change that jerked instead of running through once.
    //
    // Every write also moves the page's colours with it (see `sync` below), there and then: the page used
    // to follow through a flow that made a list and boxed the offset on every frame of a drag.
    val offsetBox = remember { OffsetBox() }
    var offset by offsetBox
    // The one animation allowed to be running: a settle, or a record landing. A new gesture or a
    // button press takes it over.
    var moving by remember { mutableStateOf<kotlinx.coroutines.Job?>(null) }
    // Bumped whenever a finger takes the record over. A landing that is cancelled half way still changes
    // the song, but it must not put the record back in the middle if a new drag is already moving it -
    // doing that wiped the new drag's first half, and the swipe that followed a swipe went nowhere.
    var gesture by remember { mutableIntStateOf(0) }
    // Bumped by every press on the transport's buttons, so a slide can tell whether it was cut short
    // by the next press (then its change still happens, and the record stays where it is for the next
    // slide to carry on) or by something else. See land.
    var presses by remember { mutableIntStateOf(0) }
    // How fast the record was moving, in pixels a second, the last time a slide moved it: the slide
    // that interrupts one starts from this rather than from rest.
    var speed by remember { mutableFloatStateOf(0f) }
    /** A finger is on the record. While it is, the record stays lifted whatever else finishes. */
    var holding by remember { mutableStateOf(false) }
    // 0 at rest, 1 while a finger holds the record: it lifts off the page - a little smaller, rounded,
    // with a shadow - and the cover's own blur shows round it. It goes back down once the song is in.
    val lift = remember { Animatable(0f) }
    val density = androidx.compose.ui.platform.LocalDensity.current
    val gap = with(density) { 18.dp.toPx() }
    val radius = with(density) { 22.dp.toPx() }
    val before = rememberCover(previousUrl, CoverSize.FULL)
    val after = rememberCover(nextUrl, CoverSize.FULL)
    val beforeFade = rememberPictureFade(before)
    val afterFade = rememberPictureFade(after)
    // A pointerInput block keyed on Unit is created once and never replaced, so anything it closes over
    // is whatever it was on the first composition - back then there was no queue, so the addresses were
    // null. That is what left a record landing under a name the sleeve could never match: the picture
    // of the song just left sat in the middle, at its lifted size, over the whole change until the
    // four-second timeout let go of it. Everything the gesture and the button queue read goes through
    // these instead, which are read at the moment they are used.
    val hasBefore by androidx.compose.runtime.rememberUpdatedState(previousUrl != null)
    val hasAfter by androidx.compose.runtime.rememberUpdatedState(nextUrl != null)
    // The covers too: one is made per address, so the one a gesture caught on the first composition is
    // a cover of nothing, with no picture in it - which is why a record could land with nothing to draw,
    // and the cover of the song being left stayed in the middle until the sleeve caught up.
    val afterNow by androidx.compose.runtime.rememberUpdatedState(after)
    val beforeNow by androidx.compose.runtime.rememberUpdatedState(before)
    val afterFadeNow by androidx.compose.runtime.rememberUpdatedState(afterFade)
    val beforeFadeNow by androidx.compose.runtime.rememberUpdatedState(beforeFade)
    val nextUrlNow by androidx.compose.runtime.rememberUpdatedState(nextUrl)
    val previousUrlNow by androidx.compose.runtime.rememberUpdatedState(previousUrl)
    val currentUrlNow by androidx.compose.runtime.rememberUpdatedState(currentUrl)
    val nextTintNow by androidx.compose.runtime.rememberUpdatedState(nextTint)
    val previousTintNow by androidx.compose.runtime.rememberUpdatedState(previousTint)
    /** The colours the last change asked for, held until the page is showing them. */
    var committedTint by remember { mutableStateOf<String?>(null) }
    val onNextNow by androidx.compose.runtime.rememberUpdatedState(onNext)
    val onPreviousNow by androidx.compose.runtime.rememberUpdatedState(onPrevious)
    // The record that has been slid in, drawn over the sleeve until the sleeve shows it too. The drawn
    // picture itself, not the painter: the painter is handed the following song's address next.
    var landed by remember { mutableStateOf<androidx.compose.ui.graphics.painter.Painter?>(null) }
    var landedUrl by remember { mutableStateOf<String?>(null) }
    /** The address of the song the last change asked the player for; see land. */
    var committed by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(landedUrl) {
        val url = landedUrl ?: return@LaunchedEffect
        // A second and a half, not four: if the sleeve has not arrived by then something is wrong with
        // the picture, and a record held over the page is worse than the sleeve's own cross-fade.
        kotlinx.coroutines.withTimeoutOrNull(1_500) { androidx.compose.runtime.snapshotFlow { art.shownUrl }.first { it == url } }
        landed = null; landedUrl = null
    }
    // A song that changes by itself - the end of one, gapless, a crossfade, an AutoMix switch, a skip
    // from the notification - changes record the way a skip does: the record it left goes out one side
    // and the new one comes in from the other, lifted, and settles into the sleeve. It used to dissolve
    // in place, which next to a skip read as the player snapping. The song on the page only moves when
    // the ear does (for a mix, where the incoming song becomes the louder), so the slide starts there.
    //
    // Decided while composing, so the first frame the new song is drawn on already has its record off
    // the edge and the one it left in the middle: an effect would run after that frame, and the new
    // cover would sit in the middle for it, then jump out and slide back in. Only a step to the song
    // either side (the one left is now the neighbour, so its picture is already there) and only with
    // both pictures in hand; anything else - a new queue, a song tapped far down the list, a cover not
    // loaded yet, reduced motion, the player out of sight - changes as it did.
    val passing = remember { Passing() }
    var askedId by remember { mutableStateOf<String?>(null) }
    val onScreen = LocalPlayerShown.current
    val incoming = art.cover?.takeIf { it.url == currentUrl }?.painter
    val go = when {
        songs.current == null || passing.id == null || songs.current == passing.id -> 0
        AppMotion.reduce || !onScreen || holding || moving?.isActive == true || landedUrl != null || songs.current == askedId -> 0
        incoming == null -> 0
        songs.previous == passing.id && before.image != null -> 1
        songs.next == passing.id && after.image != null -> -1
        else -> 0
    }
    /** Where a record sliding in by itself is, in records: 1 a whole record to the right, 0 in place. */
    val natural = remember(songs.current) { Animatable(go.toFloat()) }
    val naturalUrl = remember(songs.current) { currentUrl.takeIf { go != 0 } }
    val naturalPicture = remember(songs.current) { incoming.takeIf { go != 0 } }
    /** Which side the record left goes out to: the `go` this song's slide started with. */
    val naturalGo = remember(songs.current) { go }
    androidx.compose.runtime.SideEffect {
        if (passing.id != songs.current) { passing.id = songs.current; askedId = null }
    }
    // The record that came in is drawn from its own picture until the sleeve underneath has finished
    // changing to it (its dissolve runs out of sight meanwhile), then the sleeve takes over in the same
    // place with the same picture.
    val naturalHere by remember(natural, naturalUrl) {
        androidx.compose.runtime.derivedStateOf { naturalUrl != null && (natural.value != 0f || art.shownUrl != naturalUrl) }
    }
    val naturalNow by androidx.compose.runtime.rememberUpdatedState(natural)
    val nextIdNow by androidx.compose.runtime.rememberUpdatedState(songs.next)
    val previousIdNow by androidx.compose.runtime.rememberUpdatedState(songs.previous)
    LaunchedEffect(natural) {
        if (natural.value == 0f) return@LaunchedEffect
        // The same lift, slide and settle as a press on the transport (see the buttons' `run` below).
        launch { lift.animateTo(1f, spring(dampingRatio = 1f, stiffness = 1200f, visibilityThreshold = 0.001f)) }
        settleByFrames(natural, BUTTON_STIFFNESS)
        lift.animateTo(0f, spring(dampingRatio = 1f, stiffness = 600f, visibilityThreshold = 0.001f))
    }

    // The moving cover plays only on a record at rest: none held, lifted, sliding (a skip or a song changing by itself) or waiting to land.
    if (motion != null) LaunchedEffect(motion) {
        androidx.compose.runtime.snapshotFlow { !holding && offset == 0f && lift.value == 0f && landedUrl == null && !naturalHere }
            .collect { motion.resting = it }
    }
    // The plate a record waits on: the same placeholder as the sleeve's, the theme's own surface. It
    // used to be the page's, which is the colour of the song being left - a record for the next song
    // arriving in the last one's colour.
    val plate = Modifier.drawBehind { drawRect(art.plate) }
    val shapes = remember { CornerShapes() }
    androidx.compose.foundation.layout.BoxWithConstraints(Modifier.fillMaxSize()) {
    val widthPx = constraints.maxWidth.toFloat()
    // Each record is the cover's square, as tall as the sleeve - or, on a sleeve wider than it is tall (a phone
    // on its side), as wide, so the picture fills it and is cropped above and below instead of at the sides.
    val heightPx = maxOf(constraints.maxHeight, constraints.maxWidth).toFloat()
    val sideDp = with(density) { heightPx.toDp() }
    val down = spring<Float>(dampingRatio = 1f, stiffness = 300f, visibilityThreshold = 0.001f)
    // The page's colours follow the record across (see PageShift). Once a record has arrived the song
    // takes a frame or two to catch up, and the offset is back at nought by then, so the colours are
    // held at the arriving record until the player has it - otherwise the page fell back to the old
    // song's colour for those frames and then changed again.
    val travel = heightPx * liftedScale(if (AppMotion.reduce) 0f else 1f, widthPx, heightPx) + gap
    val travelNow by androidx.compose.runtime.rememberUpdatedState(travel)
    fun sync(at: Float) {
        val waiting = committed
        val showing = currentUrlNow
        val taken = shift.adopted
        // Held from the moment a record is sent until the page is drawing its colours, and taken
        // from the record's own position the rest of the time.
        // Let go of the record a swipe sent the moment the player is on it and the page is wearing
        // its colours. Kept, it made every later change look like that record arriving again: a song
        // that ends by itself moves the player off what was committed, which read as a record still
        // on its way in, and the page went back to its colours and stayed there.
        if (committedTint != null && waiting == showing && taken == committedTint) {
            committed = null
            committedTint = null
        }
        if (committedTint != null && (waiting != showing || taken != committedTint)) {
            shift.towards = committedTint
            shift.amount = 1f
            shift.arrived = committedTint
        } else {
            shift.arrived = null
            shift.towards = if (at < 0f) nextTintNow else if (at > 0f) previousTintNow else null
            shift.amount = (kotlin.math.abs(at) / travelNow).coerceIn(0f, 1f)
        }
    }
    offsetBox.onSet = FloatSink { sync(it) }
    LaunchedEffect(shift, travel) {
        // The rest of what the page's colours depend on changes once a song, not once a frame.
        androidx.compose.runtime.snapshotFlow { Triple(committed, currentUrlNow, shift.adopted) }.collect { sync(offset) }
    }
    androidx.compose.runtime.DisposableEffect(shift) {
        onDispose { shift.towards = null; shift.amount = 0f; shift.arrived = null }
    }

    /**
     * A record still sliding in by itself is taken over where it is, by a finger or a press: its place
     * goes into [offset], so what moves it next starts from where it can be seen.
     */
    suspend fun takeOver() {
        val n = naturalNow.value
        if (n == 0f) return
        naturalNow.snapTo(0f)
        offset += n * (heightPx * liftedScale(lift.value, widthPx, heightPx) + gap)
    }

    /**
     * The record goes [go] (-1 for the next one, 1 for the one before), the song changes as it arrives,
     * and the new record settles into the sleeve. Cancelled half way - a second button press, a new
     * gesture - it still changes the song, so nothing asked for is quietly dropped.
     */
    suspend fun land(go: Int, velocity: Float, stiffness: Float, liftDown: Float = 240f) {
        val turn = gesture
        val press = presses
        // One change at a time. The song a record has just landed on is only the song the player is
        // playing a frame or two later, and until it is, the record waiting off the edge is still the
        // one that is showing: starting now would slide in a copy of the cover already in the middle,
        // which is the press that seems to change the cover first and then animate from it to itself.
        //
        // A quarter of a second is long enough for a player that is going to answer at all, and short
        // enough that a run of presses (each of which waits here for the change before it) does not
        // read as the records pausing between slides.
        val waitingFor = committed
        val stale = waitingFor != null && currentUrlNow != waitingFor && kotlinx.coroutines.withTimeoutOrNull(250) {
            androidx.compose.runtime.snapshotFlow { currentUrlNow }.first { it == waitingFor }
        } == null
        val painter = if (go < 0) afterNow else beforeNow
        val url = if (go < 0) nextUrlNow else previousUrlNow
        // The player never caught up with the last change, so the record waiting off the edge is the one
        // already in the middle and there is nothing to slide. Change the song plainly rather than send
        // the same record across the screen - and put the record back down, since it was picked up for a
        // move that is not going to happen. Leaving it up here is what left the cover sitting at its
        // small size after a button press.
        //
        // Only that, though - not "the next record has the same picture as this one". Every song of an
        // album has the album's cover, so that test was true for every swipe within an album, and the
        // record was left wherever the finger dropped it: half off the screen, with the empty plate of
        // the record after it showing beside it. Two records with the same picture still slide.
        if (stale) {
            committed = null
            askedId = if (go < 0) nextIdNow else previousIdNow
            if (go < 0) onNextNow() else onPreviousNow()
            lift.animateTo(0f, spring(dampingRatio = 1f, stiffness = liftDown, visibilityThreshold = 0.001f))
            return
        }
        // Where the neighbour sits once the record is lifted, which is where it will be when it arrives.
        // A record is the whole square, as tall as the sleeve.
        //
        // The lift it is going to have, not the one it has: a button press starts the lift in a
        // coroutine of its own and comes straight here, so the lift had not begun yet and the record
        // was sent a full unlifted span - the gap between two records that have not shrunk. It shrank
        // on the way, and arrived that much too far over, which is a record ending up with its edge in
        // the middle of the screen instead of its middle. A swipe was right only because the lift had
        // already started under the finger.
        val span = heightPx * liftedScale(if (AppMotion.reduce) 0f else 1f, widthPx, heightPx) + gap

        /**
         * The record has arrived, wherever it got to: the song changes, and the picture that came in is
         * held over the sleeve until the sleeve has it too. [rest] is where the record is left - the
         * middle when nothing else has hold of it, and the arriving record's own place when a finger
         * has, so the drag carries on from the record it can see instead of jumping.
         */
        fun arrive(rest: Float) {
            // Only hold the picture over if it is really there, whole: as a bare plate it is a grey square,
            // and a picture still fading in over its plate would come up to full strength in one frame.
            val picture = painter.painter
            val level = if (picture == null) 0f else (if (go < 0) afterFadeNow else beforeFadeNow).value
            val whole = picture != null && level >= 1f
            landed = picture.takeIf { whole }
            landedUrl = url.takeIf { whole }
            // Otherwise the record that came in is a plate, or a picture part way in, and the sleeve going
            // back under it has to be the same: not the picture of the song just left, which has slid off
            // the other side. That was the same cover twice in a row - out one side, back in the middle
            // for the length of the load. The sleeve goes to the plate at once and fades this song's
            // picture in from where the record had it when it comes (SleeveArt.clearFor).
            if (!whole && url != art.shownUrl) art.clearFor(url, level)
            // What the player has been asked for, whether or not there was a picture to hold over. The
            // next change waits for this, not for the picture: a cover that failed to load used to let
            // the one after it start against a queue that had not moved yet.
            committed = url
            committedTint = if (go < 0) nextTintNow else previousTintNow
            // Unless the sleeve already shows this picture (the next song of the same album): then no
            // picture is coming, and a snap left waiting here would cut the fade of the next real change.
            art.snapNext = whole && url != art.shownUrl
            offset = rest
            askedId = if (go < 0) nextIdNow else previousIdNow
            if (go < 0) onNextNow() else onPreviousNow()
        }

        var changed = false
        try {
            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
            // A tenth of a pixel is not worth animating to: the default threshold kept the spring
            // running long after the record had arrived, which is what made a button press take half a
            // second to do a quarter of a second's work.
            val settle = spring(dampingRatio = 1f, stiffness = stiffness, visibilityThreshold = 1f)
            if (AppMotion.reduce) offset = go * span
            else androidx.compose.animation.core.animate(offset, go * span, velocity, settle) { v, vel -> offset = v; speed = vel }
            speed = 0f
            changed = true
            // Same frame: the incoming record takes the middle, the sleeve goes back under it.
            arrive(0f)
            // The new record settles back into the sleeve. The settle is let go of rather than waited
            // for: the record is back in the sleeve as far as this change is concerned, so a press that
            // comes in while it is still growing takes it over and lifts it again from wherever it has
            // got to, instead of queueing behind the rest of an animation that is already finished with.
            scope.launch { lift.animateTo(0f, spring(dampingRatio = 1f, stiffness = liftDown, visibilityThreshold = 0.001f)) }
        } finally {
            if (!changed) {
                val caught = gesture != turn
                when {
                    // Another press on the buttons. The change happens now, however far the record has
                    // got, and the record that was coming in keeps the place it is in: the press that
                    // cancelled this one slides on from there, so a run of presses is one continuous
                    // scroll of records rather than a queue of full slides (see the transport below).
                    presses != press -> arrive(offset - go * span)
                    // Cancelled by something that is not a finger - the screen going away. Honour it.
                    !caught -> arrive(0f)
                    // A finger caught the record after it had all but gone: the change has happened as
                    // far as the eye is concerned, so it counts, and the record that was coming in keeps
                    // the place it is already in - a span along - so the drag carries straight on from
                    // the cover it can see. Putting the offset back to nought instead dropped the old
                    // cover into the middle for a frame, which is the jump with no slide.
                    kotlin.math.abs(offset) > span / 2f -> arrive(offset - go * span)
                    // Caught early, before the record had really left: the song does not change at all,
                    // and the record stays under the finger where it was. Committing it here would have
                    // left the finger dragging a record that is no longer the one playing.
                    else -> Unit
                }
            }
        }
    }

    // The transport's own skips make the same move a thumb does - the record lifts off the page, goes
    // out one side and the next one settles into the sleeve - only quicker, since there is no finger to
    // follow. A previous press that only rewinds the song never gets here: there is no other record to
    // show. See the buttons in PlayerScreen.
    //
    // One slide in flight, never a queue. A press while a record is still on its way commits that
    // change at once (the song moves on now, see land's cancel path) and the record that was coming
    // in carries straight on from wherever it is to become the one going out - so five quick presses
    // are five songs and one continuous scroll of records, and the motion stops within a slide of the
    // last press. They used to queue, four deep, each waiting for the slide before it: the records
    // went on scrolling for a second after the thumb had stopped, which read as the player lagging.
    // The interrupting slide starts with the speed the record already had, so nothing jolts.
    LaunchedEffect(moving, holding) {
        // Whatever happened - a move that turned out to have nothing to move to, a landing cancelled
        // by a finger that then went nowhere - a record with nobody holding it and nothing to do
        // belongs flat in its sleeve. This is the one place that is guaranteed to run after every move.
        moving?.join()
        if (!holding && lift.value != 0f) lift.animateTo(0f, down)
    }
    androidx.compose.runtime.DisposableEffect(slide) {
        val run: (Int) -> Boolean = { go ->
            if ((go < 0 && hasAfter) || (go > 0 && hasBefore)) {
                val running = moving?.takeIf { it.isActive }
                presses++
                moving = scope.launch {
                    running?.cancelAndJoin()
                    takeOver()
                    // No bounce in the lift, and quicker than the slide. A record that is still being
                    // picked up is still shrinking, and the gap the next one waits in shrinks with it;
                    // a lift that sprang past its mark pulled the arriving record past the middle and
                    // back, which is the overshoot you see when a button sends it across.
                    if (!AppMotion.reduce) launch { lift.animateTo(1f, spring(dampingRatio = 1f, stiffness = 1200f, visibilityThreshold = 0.001f)) }
                    // A press on top of a slide has further to go (the record it moves is part way in)
                    // and a thumb that is in a hurry: half again as stiff.
                    land(go, speed, if (running != null) BUTTON_STIFFNESS * 1.5f else BUTTON_STIFFNESS, liftDown = 600f)
                }
                true
            } else false
        }
        slide.run = run
        onDispose { if (slide.run === run) slide.run = null }
    }
    Box(
        Modifier.fillMaxSize().pointerInput(Unit) {
            val tracker = androidx.compose.ui.input.pointer.util.VelocityTracker()
            var x = 0f
            val release: (Float) -> Unit = { v ->
                holding = false
                val o = offset
                val w = size.width.toFloat()
                // Past a third of the way or flicked (swipeTurn, shared with the bar).
                val go = swipeTurn(o, v, w, hasBefore, hasAfter, bar = false)
                val running = moving
                moving = scope.launch {
                    running?.cancelAndJoin()
                    takeOver()
                    if (go == 0) {
                        launch { lift.animateTo(0f, down) }
                        androidx.compose.animation.core.animate(offset, 0f, v, spring(dampingRatio = 1f, stiffness = 520f, visibilityThreshold = 1f)) { value, _ -> offset = value }
                    } else land(go, v, 520f)
                }
            }
            // Sideways only, and plainly so: the sleeve sits inside the sheet that is pulled down to
            // put the player away, and a dismissal with any slant at all used to change the song.
            sidewaysDrag(
                slop = 1.5f, ratio = 1.8f,
                onDragStart = {
                    tracker.resetTracking(); x = 0f
                    holding = true
                    // A finger beats the buttons: a record still on its way is cancelled - it changes
                    // the song on its way out if it had all but arrived (see land).
                    gesture++
                    moving?.cancel()
                    scope.launch { takeOver() }
                    if (!AppMotion.reduce) scope.launch { lift.animateTo(1f, spring(dampingRatio = 1f, stiffness = 420f, visibilityThreshold = 0.001f)) }
                },
                onDragEnd = { release(tracker.calculateVelocity().x) },
                onDragCancel = { release(0f) },
            ) { change, d ->
                x += d
                tracker.addPosition(change.uptimeMillis, Offset(x, 0f))
                val w = size.width.toFloat()
                val next = offset + d
                // Towards a record that is not there it gives a little and no more.
                val allowed = (next > 0f && hasBefore) || (next < 0f && hasAfter)
                offset = if (allowed) next.coerceIn(-w, w) else (offset + d * GIVE).coerceIn(-w * GIVE_LIMIT, w * GIVE_LIMIT)
            }
        },
    ) {
        // One record, lifted by [lift] and moved by [dx]; [fade] is its brightness against the page. All
        // of it read in the draw phase: a drag moves layers and recomposes nothing.
        // Each record is the cover's whole square, as tall as the sleeve and so wider than the screen: at
        // rest the screen's edges crop it to exactly the sleeve, and lifted it shrinks until all of it is
        // on screen - the sides the sleeve hides come into view as the record is picked up.
        fun Modifier.record(dx: RecordDx, fade: RecordFade) = align(Alignment.Center).requiredSize(sideDp).graphicsLayer {
            val l = lift.value
            val s = liftedScale(l, widthPx, size.height)
            scaleX = s; scaleY = s
            val span = size.width * s + gap
            val o = offset + natural.value * span
            translationX = dx.at(o, span)
            alpha = fade.at(o, (kotlin.math.abs(o) / span).coerceIn(0f, 1f))
            if (l > 0f) {
                shape = shapes.of(radius * l / s)
                clip = true
                // No shadow. A shadow is drawn from the layer's outline, which is the whole record -
                // including the last rows, which are rubbed out so that the record can dissolve into
                // its own blur. So the shadow showed straight through that transparency: a dark band
                // sitting at the record's bottom, travelling with it while it was lifted and gone the
                // moment it settled, which is the line that runs ahead of a cover as it grows and is
                // nowhere to be found once it has. A record that is picked up is already smaller and
                // rounded; it does not need one.
            }
        }
        // Each neighbour waits just off its edge and is drawn only while it is being pulled in, coming up
        // from a little dimmer as it arrives. One whose picture has not arrived is still a record - the
        // same square, the same corners - with the sheen the rest of the app uses while it waits, rather
        // than a flat grey card: the covers are fetched ahead (PlayerViewModel) but a cold queue, or a
        // slow server, can still be reached before they land.
        // Until the picture has faded all the way in over it, so the sheen goes away under a whole picture.
        val afterHere by remember(afterFade) { androidx.compose.runtime.derivedStateOf { afterFade.value >= 1f } }
        val beforeHere by remember(beforeFade) { androidx.compose.runtime.derivedStateOf { beforeFade.value >= 1f } }
        // A neighbour only shows while a finger pulls it in; waiting for its picture it shimmers then and
        // only then. With nothing either side, or a picture that never comes, a sheen on the unseen
        // record ran for ever - and redrew the whole app every frame, on every screen, the player being
        // composed underneath them all.
        val afterShown by remember { androidx.compose.runtime.derivedStateOf { offsetBox.state.floatValue < 0f } }
        val beforeShown by remember { androidx.compose.runtime.derivedStateOf { offsetBox.state.floatValue > 0f } }

        /** The records themselves. */
        @Composable
        fun androidx.compose.foundation.layout.BoxScope.records(blurred: Boolean) {
            // Not while a record that has landed is held over it: the two sit in the same place, and
            // the one on top is the whole record.
            if (landedUrl == null && !naturalHere) Box(Modifier.fillMaxSize().record({ o, _ -> o }, { _, f -> 1f - 0.35f * f })) {
                SleeveImage(art, Modifier.fillMaxSize())
            }
            if (landedUrl == null && naturalHere) Box(Modifier.fillMaxSize().record({ o, _ -> o }, { _, f -> 1f - 0.35f * f }).then(plate)) {
                naturalPicture?.let { androidx.compose.foundation.Image(it, null, Modifier.fillMaxSize(), contentScale = androidx.compose.ui.layout.ContentScale.Crop) }
            }
            // The sheen under the picture, which fades in over it when it comes (rememberPictureFade).
            Box(Modifier.fillMaxSize().record({ o, span -> o + span }, { o, f -> if (o < 0f) 0.55f + 0.45f * f else 0f }).then(plate)) {
                PlateSheen(art, !afterHere && afterShown)
                after.painter?.let { androidx.compose.foundation.Image(it, null, Modifier.fillMaxSize().graphicsLayer { alpha = afterFade.value }, contentScale = androidx.compose.ui.layout.ContentScale.Crop) }
            }
            Box(Modifier.fillMaxSize().record({ o, span -> o - span }, { o, f -> if (o > 0f) 0.55f + 0.45f * f else 0f }).then(plate)) {
                PlateSheen(art, !beforeHere && beforeShown)
                before.painter?.let { androidx.compose.foundation.Image(it, null, Modifier.fillMaxSize().graphicsLayer { alpha = beforeFade.value }, contentScale = androidx.compose.ui.layout.ContentScale.Crop) }
            }
            // The moving cover, in the same square as the still one and cropped by the same edges, so the
            // fade between them shows no shift; it moves with the record, too. The sharp copy only: a
            // surface is drawn once, so the blurred band at the sleeve's foot stays the still cover's, which
            // is also what the page's wash is made of. When the song changes by itself it goes out with the
            // record it was playing on, fading as it goes: that record is the neighbour by then, and the
            // video used to vanish on the slide's first frame, leaving the still cover in its place.
            // Composed here, after the records, whatever happens to them, so its surface is never made
            // again in the middle of a move.
            if (!blurred && motion != null && motion.present && landedUrl == null) Box(
                Modifier.fillMaxSize().record(
                    { o, span -> if (naturalHere) o - naturalGo * span else o },
                    { o, f ->
                        when {
                            !naturalHere -> 1f - 0.35f * f
                            naturalGo > 0 -> if (o > 0f) 0.55f + 0.45f * f else 0f
                            else -> if (o < 0f) 0.55f + 0.45f * f else 0f
                        }
                    },
                ),
            ) { MotionCover(motion) }
            // It is the record that is showing, so it moves with the record: held still in the middle it
            // covered the next change from on top, which is the "cover stuck over the animation".
            if (landedUrl != null) Box(Modifier.fillMaxSize().record({ o, _ -> o }, { _, f -> 1f - 0.35f * f }).then(plate)) {
                landed?.let { androidx.compose.foundation.Image(it, null, Modifier.fillMaxSize(), contentScale = androidx.compose.ui.layout.ContentScale.Crop) }
            }
        }

        // All the records in one layer, and the sleeve's soft bottom made of that layer once (SoftSleeve):
        // the band is the sleeve's, and a record lifted, sliding or growing back is whole above it and soft
        // inside it.
        //
        // The blurred half of that bottom goes as the record is picked up and comes back as it settles, on
        // the lift's own spring. A record in the hand is a card, and a card has no blur at its foot: at full
        // strength the blurred copy, which starts well above the band, smeared the lower part of the lifted
        // record and hazed round its rounded corners - a blur that came and went as the record shrank and
        // grew under it. Now the zoom out takes the blur with it and the zoom in brings it back, one move.
        SoftSleeve(Modifier.fillMaxSize(), blur = FloatReader { smooth(1f - lift.value, 0f, 1f) }) { blurred -> records(blurred) }
    }
}
}

/**
 * The soft band's blur, and on a white, cream or black page the page's tint over it: the band's
 * luminance scaled per channel by the page's colour relative to its brightest channel (grey for white,
 * the same shading warmed for cream). Made once per tint and kept; a page cross-fading between a tinted
 * page and a coloured one mixes the two matrices rather than switching in one frame.
 */
@androidx.annotation.RequiresApi(31)
private class BandEffect(blurPx: Float) {
    private val blur = android.graphics.RenderEffect.createBlurEffect(blurPx, blurPx, android.graphics.Shader.TileMode.CLAMP)
    private val plain = blur.asComposeRenderEffect()
    private val made = androidx.collection.MutableLongObjectMap<androidx.compose.ui.graphics.RenderEffect>()

    fun of(look: Look): androidx.compose.ui.graphics.RenderEffect {
        val s = Float.fromBits(look.argb(CoverLook.BAND_TINT))
        if (s <= 0f) return plain
        val kr = Float.fromBits(look.argb(CoverLook.BAND_KR))
        val kg = Float.fromBits(look.argb(CoverLook.BAND_KG))
        val kb = Float.fromBits(look.argb(CoverLook.BAND_KB))
        // Kept by the tint to a 1/256th, which is finer than the blurred band can show.
        fun q(x: Float) = (x * 256f).toLong().coerceIn(0, 1023)
        val key = (q(s) shl 30) or (q(kr) shl 20) or (q(kg) shl 10) or q(kb)
        return made[key] ?: run {
            // The matrix is nori-look's (`sleeve::band_matrix`), asked once per tint.
            val m = android.graphics.ColorMatrix(dev.nori.music.ffi.bandMatrix(s, kr, kg, kb).toFloatArray())
            android.graphics.RenderEffect.createColorFilterEffect(android.graphics.ColorMatrixColorFilter(m), blur).asComposeRenderEffect()
        }.also { made[key] = it }
    }
}

/** Takes a Float where it is written, without boxing it. */
internal fun interface FloatSink { fun put(v: Float) }

/** The sleeve's offset: a float state that also tells [onSet] each time it is written. */
@Stable
internal class OffsetBox {
    val state = androidx.compose.runtime.mutableFloatStateOf(0f)
    var onSet: FloatSink? = null
    @Suppress("NOTHING_TO_INLINE")
    inline operator fun getValue(thisRef: Any?, property: kotlin.reflect.KProperty<*>): Float = state.floatValue
    @Suppress("NOTHING_TO_INLINE")
    inline operator fun setValue(thisRef: Any?, property: kotlin.reflect.KProperty<*>, value: Float) {
        state.floatValue = value
        onSet?.put(value)
    }
}

/** A record's sideways place for an offset and a span, and its brightness at an offset for how far across it is. */
private fun interface RecordDx { fun at(offset: Float, span: Float): Float }
private fun interface RecordFade { fun at(offset: Float, across: Float): Float }

/** The songs on the page and either side of it, by id: the sleeve tells a song that changed by itself from its own skips by them. */
@androidx.compose.runtime.Immutable
internal data class Songs(val current: String?, val previous: String?, val next: String?)

/** The song the sleeve was last composed with; written after each composition, read by the next. */
private class Passing { var id: String? = null }

/**
 * How far the page's colour has travelled towards the record coming in, and which record that is.
 * Written by the sleeve as it moves and read in the draw phase, so the page's colours cross over with
 * the record rather than waiting for it to land: the song itself only changes when the record arrives,
 * and until this existed so did its colour, a whole slide late.
 */
@Stable
internal class PageShift {
    /** The cover coming in, or null when nothing is on its way. */
    var towards by mutableStateOf<String?>(null)
    /** 0 at the record showing, 1 at the one arriving. */
    var amount by mutableFloatStateOf(0f)

    /**
     * The cover of a record that has arrived and is waiting for the song to catch up, or null. The page
     * takes its colours on as soon as this says so, rather than waiting for the song: while it waited,
     * anything that let go of [towards] first - a second swipe, a queue that moved underneath - dropped
     * the page back to the record before for the frames in between, which is the old colour flashing up
     * as the animation ended.
     */
    var arrived by mutableStateOf<String?>(null)

    /**
     * The cover whose colours the page itself is now drawing. The sleeve holds a landed record's
     * colours up until this says the page has them: the song changes a frame or two before its colours
     * are looked up, and letting go in between dropped the page back to the last song for those frames.
     */
    var adopted by mutableStateOf<String?>(null)
}

/**
 * The transport's way of asking the sleeve to change record the way a swipe does. It answers false
 * when there is no record that way, or when there is no sleeve on screen at all (the queue or the
 * lyrics are showing), and the caller just changes the song.
 */
@Stable
internal class SleeveSlide {
    internal var run: ((Int) -> Boolean)? = null

    /** [go] is -1 for the next record, 1 for the one before. */
    fun ask(go: Int): Boolean = run?.invoke(go) ?: false
}

/**
 * Brings [a] to rest at 0 as a critically damped spring of [stiffness] would, a frame at a time, with no
 * frame counted as longer than [MAX_STEP_S]. A song that changes by itself starts its slide on the
 * frames that compose the whole new song, and the first of those can take a tenth of a second or more:
 * a spring timed by the clock had done most of its travel by the next frame, and the record jumped most
 * of the way in. Counted like this, a slow frame slows the slide down instead of skipping it.
 */
internal suspend fun settleByFrames(a: Animatable<Float, androidx.compose.animation.core.AnimationVector1D>, stiffness: Float) {
    val from = a.value
    val w = kotlin.math.sqrt(stiffness)
    var t = 0f
    var last = androidx.compose.runtime.withFrameNanos { it }
    while (true) {
        val now = androidx.compose.runtime.withFrameNanos { it }
        t += ((now - last) / 1e9f).coerceAtMost(MAX_STEP_S)
        last = now
        val x = from * (1f + w * t) * kotlin.math.exp(-w * t)
        if (kotlin.math.abs(x) < 0.001f) break
        a.snapTo(x)
    }
    a.snapTo(0f)
}

/** The longest a frame counts for in [settleByFrames]: two frames at sixty a second. */
private const val MAX_STEP_S = 0.033f

/** A record sent across by a button rather than a thumb: the same move, a little quicker. */
private const val BUTTON_STIFFNESS = 950f

/** How much of the screen's width a record held by a finger takes: it lifts off the page, smaller. */
private const val LIFTED_WIDTH = 0.86f

/** A slow drag changes the record past this share of the width; the same share arms a row's swipe. */
internal const val TURN = 0.3f
/** A release faster than this, in pixels a second, changes the record whatever the distance: on the player's sleeve... */
private const val FLICK_PX_S = 1_000f
/** ...and on the now playing bar, a small strip under the thumb, where a flick is shorter and slower. */
private const val BAR_FLICK_PX_S = 900f
/** Towards a record that is not there a drag gives this much of the finger's travel, and no more than [GIVE_LIMIT] of the width. */
internal const val GIVE = 0.2f
internal const val GIVE_LIMIT = 0.06f

/**
 * Where a sideways drag on a row of records goes when the finger lifts at [offset] pixels (negative:
 * towards the next) with [velocity] pixels a second, on a row [width] wide: -1 to the next record, 1 to
 * the one before, 0 back where it was. Past [TURN] of the width, or flicked; never towards a record that
 * is not there. [bar] is the now playing bar, which takes a slower flick than the sleeve. How a touch
 * feels is the platform's own, and this is a few comparisons Compose makes on the release itself.
 */
internal fun swipeTurn(offset: Float, velocity: Float, width: Float, hasBefore: Boolean, hasAfter: Boolean, bar: Boolean): Int {
    val flick = if (bar) BAR_FLICK_PX_S else FLICK_PX_S
    return if (offset < 0f && hasAfter && (velocity < -flick || offset < -width * TURN)) -1
    else if (offset > 0f && hasBefore && (velocity > flick || offset > width * TURN)) 1
    else 0
}

/** The scale of a record [side] tall at lift [l], on a sleeve [width] wide: 1 at rest, the whole square at 86 % of the width held. */
private fun liftedScale(l: Float, width: Float, side: Float): Float {
    val held = if (side > 0f) (LIFTED_WIDTH * width / side).coerceAtMost(1f) else 1f
    // Never past either end. A spring settling back to nought used to dip below it for a few frames,
    // which made the record a shade bigger than the sleeve - enough for the page's wash, which is drawn
    // to the sleeve's own size, to stop short of the bottom edge and leave a line there.
    return 1f - (1f - held) * l.coerceIn(0f, 1f)
}


@Composable
private fun rememberSleeveArt(url: String?): SleeveArt {
    val art = remember { SleeveArt() }
    val cover = rememberCover(url, CoverSize.FULL)
    art.cover = cover
    val picture = cover.painter
    // Run again when a record slides in for another song without its picture (see clearFor), and not when
    // the song it slid in for arrives and takes that over: this song's own fade is not to be cut short.
    val elsewhere = art.clearedFor?.takeIf { it != url }
    LaunchedEffect(cover, cover.state, picture, elsewhere) {
        val now = android.os.SystemClock.uptimeMillis()
        val cleared = art.clearedFor
        if (cleared != null && cleared != url) {
            // A record slid in for a song the player has not moved to yet: the plate stays for it. Should
            // the player never get there, the picture of the song it is still on comes back, faded in.
            delay(CLEARED_WAIT_MS)
            art.fadeFrom = 0f
            art.clearedFor = null
            return@LaunchedEffect
        }
        // The song it slid in for is here: taken once, so a later change of song is not held up by it.
        if (cleared != null) art.clearedFor = null
        art.turn.song(url, now, ready = picture != null, cleared = cleared != null)
        when {
            picture != null -> if (picture !== art.current) {
                // Only this song's picture: one for a song skipped past never gets here (each cover is
                // its own request, let go of with its song), and the turn says so as well.
                if (!art.turn.arrived(url)) return@LaunchedEffect
                art.loading = false
                val swiped = art.snapNext.also { art.snapNext = false }
                val from = art.fadeFrom.also { art.fadeFrom = -1f }
                val instant = swiped || from < 0f && art.current == null && art.previous == null && cover.fromMemory
                // The picture on screen stays underneath at full strength while the new one covers it; one
                // already fading out to the plate carries on from where it is.
                art.current?.let { art.previous = it; art.previousAlpha.snapTo(1f) }
                art.current = picture
                if (instant || AppMotion.reduce) art.fade.snapTo(1f)
                else {
                    // A record that slid in with the picture part way in hands the sleeve that much of it.
                    val start = from.coerceAtLeast(0f)
                    art.fade.snapTo(start)
                    val ms = if (art.previous == null) PICTURE_IN_MS else PICTURE_OVER_MS
                    art.fade.animateTo(1f, androidx.compose.animation.core.tween((ms * (1f - start)).toInt().coerceAtLeast(1)))
                }
                art.previous = null
                // Last, once the picture is really the one on screen. Said before the swap - and there
                // is a suspension between the two - this let the record held over the sleeve be taken
                // away while the sleeve underneath was still showing the cover before it, which is the
                // frame of the previous cover that appeared as the record grew back.
                art.shownUrl = url
            } else if (art.shownUrl != url) {
                // This picture's fade was cut short (the effect ran again for the same song: a record slid
                // in for another one and was not taken up, the cover's state moved): it finishes from where
                // it got to. Left there, the picture stayed faint over the plate for the rest of the song.
                art.loading = false
                if (AppMotion.reduce) art.fade.snapTo(1f)
                else art.fade.animateTo(1f, androidx.compose.animation.core.tween(((1f - art.fade.value) * PICTURE_IN_MS).toInt().coerceAtLeast(1)))
                art.previous = null
                art.shownUrl = url
            }
            cover.state == CoverImage.LOADING -> {
                if (art.current == null) art.loading = true
                else {
                    // The grace, counted from the change of song rather than from this run of the effect.
                    delay(art.turn.holdLeft(now))
                    art.loading = true
                    art.letGo()
                }
            }
            // Nothing to show for this song: back to the plate rather than keep the last cover.
            else -> { art.loading = false; art.letGo() }
        }
    }
    return art
}

/** The sleeve's picture over its placeholder; [plate] false leaves the placeholder to the caller. */
@Composable
private fun SleeveImage(art: SleeveArt, modifier: Modifier, plate: Boolean = true) {
    // The sheen outlives the load by the picture's fade and is drawn under it, so it goes away behind a
    // picture that already covers it instead of vanishing from on top of one that has only begun to come.
    var sheen by remember { mutableStateOf(art.loading) }
    LaunchedEffect(art.loading) { if (!art.loading) delay(PICTURE_IN_MS.toLong()); sheen = art.loading }
    Box(if (plate) modifier.drawBehind { drawRect(art.plate) } else modifier) {
        PlateSheen(art, sheen)
        art.previous?.let { androidx.compose.foundation.Image(it, null, Modifier.fillMaxSize().graphicsLayer { alpha = art.previousAlpha.value }, contentScale = androidx.compose.ui.layout.ContentScale.Crop) }
        art.current?.let { androidx.compose.foundation.Image(it, null, Modifier.fillMaxSize().graphicsLayer { alpha = art.fade.value }, contentScale = androidx.compose.ui.layout.ContentScale.Crop) }
    }
}

/** The loading sheen over a placeholder, in the theme's own ink rather than the song's. */
@Composable
private fun androidx.compose.foundation.layout.BoxScope.PlateSheen(art: SleeveArt, active: Boolean) {
    if (!active) return
    androidx.compose.runtime.CompositionLocalProvider(LocalLook provides art.neutral) {
        Box(Modifier.matchParentSize().loadingSheen(true))
    }
}

/**
 * How far a neighbour record's picture has faded in over its plate: whole at once for one that was in
 * memory when the record was made, over [PICTURE_IN_MS] for one that came while it waited - a record
 * being pulled in never swaps its plate for its picture in one frame.
 */
@Composable
private fun rememberPictureFade(cover: CoverImage): Animatable<Float, androidx.compose.animation.core.AnimationVector1D> {
    val a = remember(cover) { Animatable(if (cover.image != null) 1f else 0f) }
    val here = cover.image != null
    LaunchedEffect(cover, here) {
        if (!here || a.value >= 1f) return@LaunchedEffect
        if (AppMotion.reduce) a.snapTo(1f) else a.animateTo(1f, androidx.compose.animation.core.tween(PICTURE_IN_MS))
    }
    return a
}

/** A title-row circle: translucent fill, light glyph, 48 dp across with a 44 dp hit region or better. */
@Composable
internal fun TitleCircle(icon: ImageVector, label: String, selected: Boolean, onClick: () -> Unit) {
    // These two sit on the sleeve's own melting bottom, not on the page: whatever the page colour is,
    // what is behind them is a piece of the record, and it can be any brightness at all. A disc tinted
    // from the page came out lighter than the page on a bright record and carried a white glyph on top
    // of it - on The Bends, a pale orange disc with a white heart. The disc brings its own contrast.
    //
    // Disc strength tracks page lightness continuously (same as Play): a boolean onSurface cut flipped
    // black↔white midway through a swipe onto paper, and a white disc on a white wash had no contrast.
    // Both are the look's (nori_look::dress), read while drawing: a page changing colour redraws them.
    val look = LocalLook.current
    val plain = reduceMotion()
    val scale = remember { androidx.compose.animation.core.Animatable(1f) }
    var ready by remember { mutableStateOf(false) }
    // Jump only for the heart, when its selected state flips.
    val isHeart = icon == Icons.Filled.Favorite || icon == Icons.Filled.FavoriteBorder
    LaunchedEffect(selected, isHeart) {
        if (!isHeart) return@LaunchedEffect
        if (!ready) { ready = true; return@LaunchedEffect }
        if (plain) return@LaunchedEffect
        scale.snapTo(1f)
        scale.animateTo(1.22f, androidx.compose.animation.core.spring(dampingRatio = 0.42f, stiffness = 900f))
        scale.animateTo(1f, androidx.compose.animation.core.spring(dampingRatio = 0.55f, stiffness = 600f))
    }
    // Material's Surface, laid out the same way, with its plate drawn rather than composed.
    androidx.compose.runtime.CompositionLocalProvider(LocalContentColor provides Color.White) {
        Box(
            Modifier.size(42.dp).graphicsLayer { scaleX = scale.value; scaleY = scale.value }
                .minimumInteractiveComponentSize()
                .drawBehind { drawCircle(look.color(CoverLook.DISC)) }
                .clip(CircleShape)
                .clickable(role = androidx.compose.ui.semantics.Role.Button, onClick = onClick),
            Alignment.Center,
        ) {
            LookIcon(icon, label, Modifier.size(25.dp)) { if (selected) look.color(CoverLook.DISC_INK_SELECTED) else Color.White }
        }
    }
}

/**
 * The phone's music-stream volume, read live so the hardware keys never leave it stale. Ticks only
 * while this screen is resumed; a drag writes straight through and updates the thumb itself.
 */
@Composable
private fun VolumeRow(vm: PlayerViewModel) {
    val look = LocalLook.current
    // Pushed by the system the moment it changes - no polling, nothing ticking while the screen is open.
    val system by vm.volume.collectAsStateWithLifecycle()
    var dragging by remember { mutableStateOf(false) }
    // What is drawn. A change from outside - the volume keys, another app - eases over; a drag is
    // followed exactly, written straight from the finger rather than snapped to from a coroutine per
    // pointer event.
    val shown = remember { mutableFloatStateOf(vm.volumeFraction()) }
    val plain = reduceMotion()
    LaunchedEffect(system, dragging) {
        if (dragging) return@LaunchedEffect
        if (plain) shown.floatValue = system
        else androidx.compose.animation.core.animate(shown.floatValue, system, animationSpec = androidx.compose.animation.core.tween(180)) { v, _ -> shown.floatValue = v }
    }
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 52.dp, vertical = 2.dp),
        Arrangement.spacedBy(12.dp), Alignment.CenterVertically,
    ) {
        LookIcon(Icons.AutoMirrored.Filled.VolumeDown, null, Modifier.size(16.dp)) { look.color(CoverLook.ON_VARIANT) }
        val pick: (Float, Float) -> Unit = { x, w ->
            val f = (x / w).coerceIn(0f, 1f)
            vm.setVolumeFraction(f)
            shown.floatValue = f
        }
        Box(
            Modifier.weight(1f).height(34.dp)
                .pointerInput(Unit) {
                    detectHorizontalDragGestures(
                        onDragStart = { dragging = true; pick(it.x, size.width.toFloat()) },
                        onDragEnd = { dragging = false },
                        onDragCancel = { dragging = false },
                    ) { change, _ -> pick(change.position.x, size.width.toFloat()) }
                }
                .pointerInput(Unit) { detectTapGestures { pick(it.x, size.width.toFloat()) } }
                .drawBehind {
                    val track = look.color(CoverLook.ON_22)
                    val filled = look.color(CoverLook.ON_85)
                    val h = 7.dp.toPx()
                    val y = (size.height - h) / 2f
                    val r = CornerRadius(h / 2f, h / 2f)
                    val at = shown.floatValue
                    drawRoundRect(track, Offset(0f, y), Size(size.width, h), r)
                    drawRoundRect(filled, Offset(0f, y), Size(size.width * at, h), r)
                    // No knob unless a finger is on it: Apple's volume slider is a filled bar and
                    // nothing else, and a permanent white circle is the most Material thing on the screen.
                    if (dragging) drawCircle(filled, h * 1.15f, Offset(size.width * at, size.height / 2f))
                },
        )
        LookIcon(Icons.AutoMirrored.Filled.VolumeUp, null, Modifier.size(20.dp)) { look.color(CoverLook.ON_VARIANT) }
    }
}

/**
 * A line too long for its width reads itself out: it sits still for a moment, so the start can be
 * read, then walks slowly sideways and comes back round, the way the title does in Apple's player.
 * A line that fits is left alone - the modifier only animates while the text overflows.
 *
 * It reads itself out [iterations] times when the song comes on or the player comes into view, then
 * settles at its start with the soft edge. A walking line is a new frame of the whole screen every
 * vsync - the blurred sleeve and wash behind it included - and one left walking for as long as the
 * player was open held a 120 Hz phone at fifty frames a second and two thirds of a core. Only while
 * the player is on screen: it stays composed behind the rest of the app (see LocalPlayerShown). 0
 * holds the line still, soft edge and all, so a row can stop walking without changing how it looks;
 * so does [reduceMotion] (e-ink screens pay for every redrawn frame).
 */
@Composable
internal fun Modifier.readable(iterations: Int = READ_OUT, key: String? = null): Modifier {
    // Put away, the player forgets: it reads the title out again the next time it comes into view.
    if (!LocalPlayerShown.current) { if (key != null) ReadOut.forget(); return this }
    // [key]: the line is the same one wherever it is drawn (the player's title, in each panel). It remembers
    // what it measured and that it has begun reading itself out, so a panel change - which draws the title
    // anew - does not lay it out once without its soft edge and then again with it, and does not walk it from
    // the start again: that was the title flashing and jumping back each time the lyrics or queue opened.
    val seen = key?.let(ReadOut::of)
    // What the line needs and what it has. The first size is this element's own - the width the row
    // gives the title - and the second is the text's, measured inside the marquee, which lays it out
    // with no width limit at all. A line that fits is left alone entirely: no walk, and no soft edge
    // either, which would otherwise dim the last letters of a title that merely came close.
    var room by remember { mutableIntStateOf(seen?.room ?: 0) }
    var needs by remember { mutableIntStateOf(seen?.needs ?: 0) }
    val over = needs > room + 1
    // Only the first copy of the line reads it out; one drawn after it (another panel) holds still at the start,
    // as the line settles, rather than walking it from the beginning again.
    val still = reduceMotion()
    val walks = remember(seen, still) { if (seen?.started == true || still) 0 else iterations }
    if (seen != null && over && walks > 0) LaunchedEffect(seen) { seen.started = true }
    return onSizeChanged { room = it.width; seen?.room = it.width }
        .then(
            // A marquee lays its text out unbounded, so there is no ellipsis to fall back on and the
            // line would otherwise end on a half-drawn letter at the edge. It goes soft over the last
            // 20 dp instead, both while it walks and once it has settled back at the start.
            if (!over) Modifier else Modifier
                .graphicsLayer(compositingStrategy = CompositingStrategy.Offscreen)
                .drawWithCache {
                    // Made once per size: the marquee redraws this every frame it walks.
                    val fade = 20.dp.toPx()
                    val edge = Brush.horizontalGradient(
                        listOf(Color.Black, Color.Transparent),
                        startX = size.width - fade, endX = size.width,
                    )
                    onDrawWithContent {
                        drawContent()
                        drawRect(edge, blendMode = BlendMode.DstIn)
                    }
                },
        )
        .basicMarquee(
            iterations = walks,
            repeatDelayMillis = 2600,
            initialDelayMillis = 2600,
            spacing = MarqueeSpacing(46.dp),
            velocity = 26.dp,
        )
        .onSizeChanged { needs = it.width; seen?.needs = it.width }
}

/** What a [readable] line with a key has measured and whether it has begun reading itself out, for its next copy. */
private class ReadOut(val key: String) {
    var room = 0
    var needs = 0
    var started = false

    companion object {
        // One line at a time: the song's title. A new song is a new key and starts over.
        private var last: ReadOut? = null
        fun of(key: String): ReadOut = last?.takeIf { it.key == key } ?: ReadOut(key).also { last = it }
        fun forget() { last = null }
    }
}

/** How many times a line too long for its width reads itself out before it settles; see [readable]. */
internal const val READ_OUT = 2

@Composable
private fun PanelButton(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    on: Boolean,
    /**
     * Optical alignment. The three boxes are spaced evenly and the glyphs are centred in them, but the
     * ink inside a Material glyph is not centred in its own square and no two of these three fill it
     * the same way: measured on screen, the queue's marks came out a sixth narrower than the other two
     * and the row read as leaning. [size] evens out how much of the box each one covers and [nudge]
     * moves its ink, not its touch target, so the three sit symmetrically about the middle one.
     */
    size: androidx.compose.ui.unit.Dp = 27.dp,
    nudge: androidx.compose.ui.unit.Dp = 0.dp,
    onClick: () -> Unit,
) {
    val look = LocalLook.current
    IconButton(onClick) {
        LookIcon(icon, label, Modifier.size(size).offset(x = nudge)) { look.color(if (on) CoverLook.ACCENT else CoverLook.ON_VARIANT) }
    }
}

/**
 * A hairline seek bar, drawn rather than assembled: two rounded rectangles and a dot, which is both
 * what it should look like and cheaper than a Slider with its own layers and ripples.
 *
 * The bar shows one of three places. The finger, while it is down. The place a released scrub asked
 * for, until the player is really there (the connection watches the seek and says so through
 * [PlayerViewModel.pendingSeek]; a slow seek thus reads as one held place, never a snap-back and a
 * glide). Otherwise the music, paced by nori-look ([SeekPace]): drawn where the song is while it plays,
 * a pixel at a time, and gliding over a third of a second when the song's place jumps - a new song, a
 * mix handing over, a seek from the notification, a queue replaced - while the times under it cross-fade.
 * Nothing teleports.
 *
 * The only ticking thing in the app, and only while this screen is on screen and resumed and the music
 * plays; a paused bar settles and stops. Put away, it does nothing at all, and the song moves on without
 * it: so the moment it is on screen again (the player opened, the app come back) it is set straight to
 * the song's real place, on the first frame drawn, rather than gliding there from wherever it was left -
 * which, opened in the middle of a song change, was the middle of the last song.
 */
@Composable
private fun SeekBar(vm: PlayerViewModel, playing: Boolean, durationMs: Long, seekable: Boolean) {
    val state by vm.state.collectAsStateWithLifecycle()
    val look = LocalLook.current
    // Read through the gesture rather than keyed: a track that learns its real length mid-scrub
    // would otherwise restart the pointer detector under the finger.
    val d by rememberUpdatedState(durationMs.coerceAtLeast(1).toFloat())
    val length by rememberUpdatedState(durationMs)
    val dragging = remember { mutableStateOf(false) }
    val drag = remember { mutableFloatStateOf(0f) }
    val held = remember { mutableStateOf<Long?>(null) }
    // The watch clears the flow when the seek has landed or been given up. The collected state lags
    // the flow by a frame, so the flow's own value is what is checked: right after a release the
    // collected value is still the old null while the flow already holds the seek. Keyed on the hold
    // too: a seek the connection applies outright (one sent to the song still being heard through a
    // crossfade) never enters the flow at all, and a hold waiting for that flow to change would wait
    // for ever - the bar stuck where the finger left it, whatever the song did.
    val watched by vm.pendingSeek.collectAsStateWithLifecycle()
    LaunchedEffect(watched, held.value) { if (held.value != null && vm.pendingSeek.value == null) held.value = null }

    val pace = remember { dev.nori.music.look.SeekPace() }
    androidx.compose.runtime.DisposableEffect(pace) { onDispose { pace.close() } }
    // What the pace says, as the draw phase reads it: written only when it changes.
    val bar = remember { mutableFloatStateOf(0f) }
    val times = remember { mutableLongStateOf(0L) }
    val fadingFrom = remember { mutableLongStateOf(-1L) }
    val fade = remember { mutableFloatStateOf(1f) }
    val publish = remember(pace) {
        {
            val b = pace.bar
            if (b != bar.floatValue) bar.floatValue = b
            val t = pace.times
            if (t != times.longValue) times.longValue = t
            val from = pace.fadingFrom
            if (from != fadingFrom.longValue) fadingFrom.longValue = from
            val f = pace.fade
            if (f != fade.floatValue) fade.floatValue = f
        }
    }
    /** The bar's length on screen, in pixels, for how often it needs drawing. */
    val barWidth = remember { mutableFloatStateOf(1000f) }
    val free = !dragging.value && held.value == null
    var resumed by remember { mutableStateOf(false) }
    LifecycleResumeEffect(Unit) { resumed = true; onPauseOrDispose { resumed = false } }
    val live = resumed && LocalPlayerShown.current
    // Coming on screen: straight to the song, in the frame being made - a side effect runs after this
    // composition and before its frame is drawn, where an effect would start a frame later.
    val wasLive = remember { booleanArrayOf(false) }
    androidx.compose.runtime.SideEffect {
        if (live && !wasLive[0]) { pace.sync(vm.positionMs, length); publish() }
        wasLive[0] = live
    }
    // Paused, the loop settles the bar and stops - nothing ticks over a paused song - so anything that
    // can move a paused player restarts it: play, a skip, a seek. While the music plays it keeps going,
    // a step every frame through a glide and one a pixel or a second otherwise. A seek starts it again
    // twice, as it is asked and as it lands, mid-glide: a loop stepped a moment ago steps on from that
    // step in its first frame (waiting a frame for a time to count from held the bar still for it).
    val stepped = remember { longArrayOf(0L) }
    // An e-ink screen sets the bar and times where the song is on each of those, and nothing runs between.
    val eink = LocalEinkScreen.current
    LaunchedEffect(free, live, playing, state.current?.id, state.index, durationMs, watched, eink) {
        if (!free || !live) return@LaunchedEffect
        if (eink) { pace.sync(vm.positionMs, length); publish(); return@LaunchedEffect }
        var last = stepped[0]
        var now = androidx.compose.runtime.withFrameNanos { it }
        if (now - last >= 100_000_000L) { last = now; now = androidx.compose.runtime.withFrameNanos { it } }
        while (isActive) {
            val wait = pace.step(vm.positionMs, length, (now - last) / 1e9f, barWidth.floatValue, if (playing) 1f else 0f)
            last = now
            stepped[0] = now
            publish()
            if (wait < 0) break
            if (wait > 0) kotlinx.coroutines.delay(wait.toLong())
            now = androidx.compose.runtime.withFrameNanos { it }
        }
    }

    // Held, the bar thickens and the dot grows, the way Apple's does, so the scrub is felt as well as
    // seen. Animated both ways: nothing here changes size in one frame.
    val thickness = animateFloatAsState(if (dragging.value) 11f else 7.3f, spring(0.9f, 420f), label = "seek")
    val knob = animateFloatAsState(if (dragging.value) 1.5f else 0f, spring(0.9f, 420f), label = "knob")
    Column(Modifier.padding(horizontal = PLAYER_GUTTER, vertical = 4.dp)) {
        Box(
            // The strip is wider than the hairline it draws: a thumb is not a mouse.
            Modifier.fillMaxWidth().height(34.dp)
                .onSizeChanged { barWidth.floatValue = it.width.toFloat() }
                .pointerInput(seekable) {
                    if (!seekable) return@pointerInput
                    // Written out rather than assembled from the drag and tap detectors, because both
                    // let the gesture go: the pointer is claimed on touch-down and every move is
                    // consumed, so the sheet's own vertical drag cannot take a scrub that runs a few
                    // degrees off level and leave the finger lifting on a cancelled gesture.
                    awaitEachGesture {
                        val down = awaitFirstDown(requireUnconsumed = false)
                        down.consume()
                        drag.floatValue = (down.position.x / size.width).coerceIn(0f, 1f)
                        dragging.value = true
                        var seek = true
                        while (true) {
                            val change = awaitPointerEvent().changes.firstOrNull { it.id == down.id }
                            // The pointer vanished from the event: the window took it (a call, a
                            // system gesture). Leave the song where it was.
                            if (change == null) { seek = false; break }
                            drag.floatValue = (change.position.x / size.width).coerceIn(0f, 1f)
                            change.consume()
                            if (!change.pressed) break
                        }
                        // Apple seeks on release, not while the finger moves: one seek, at the end,
                        // and the sound carries on undisturbed until then. The pace is set down at the
                        // finger so that when the hold drops there is nothing stale to glide from.
                        if (seek) {
                            val target = (drag.floatValue * d).toLong()
                            pace.hold(drag.floatValue, target, length)
                            publish()
                            held.value = target
                            vm.seekTo(target)
                        }
                        dragging.value = false
                    }
                }
                .drawBehind {
                    // Where the bar is, worked out here in the draw phase from primitives: no lambda
                    // returning a boxed Float on every frame.
                    val hold = held.value
                    val f = if (dragging.value) drag.floatValue else if (hold != null) (hold / d).coerceIn(0f, 1f) else bar.floatValue
                    val track = look.color(CoverLook.ON_22)
                    val filled = look.color(CoverLook.ON_85)
                    val h = thickness.value.dp.toPx()
                    val y = (size.height - h) / 2f
                    val r = CornerRadius(h / 2f, h / 2f)
                    drawRoundRect(track, Offset(0f, y), Size(size.width, h), r)
                    drawRoundRect(filled, Offset(0f, y), Size(size.width * f, h), r)
                    val k = knob.value
                    if (k > 0.01f) drawCircle(filled, h * k, Offset(size.width * f, size.height / 2f))
                },
        )
        val mixing = vm.mixing.collectAsStateWithLifecycle()
        SeekTimes(times, fadingFrom, fade, dragging, drag, held, durationMs, state.error, state.sleepAtEndOfTrack, state.sleepAt, mixing)
    }
}

/**
 * The two times under the seek bar and whatever needs saying between them. Nothing here recomposes as
 * the song plays: the times are read in the draw phase, each second's text made once (see [duration]).
 * When the bar glides over a jump the times cross-fade, the old ones out where they stand and the new
 * ones in, rather than changing in one frame.
 */
@Composable
private fun SeekTimes(
    times: androidx.compose.runtime.LongState, fadingFrom: androidx.compose.runtime.LongState, fade: androidx.compose.runtime.FloatState,
    dragging: androidx.compose.runtime.State<Boolean>, drag: androidx.compose.runtime.FloatState, held: androidx.compose.runtime.State<Long?>,
    durationMs: Long, error: String?, sleepAtEndOfTrack: Boolean, sleepAt: Long, mixing: androidx.compose.runtime.State<Boolean>,
) {
    val look = LocalLook.current
    val quiet = androidx.compose.ui.graphics.ColorProducer { look.color(CoverLook.ON_VARIANT) }
    // Which place the times count from - the finger, a held seek, the music - is the core's (`seek_times`)
    // and the pace's. Asked over JNI with primitives, in the draw phase: a second, or a scrub on every
    // frame the finger moves, redraws the two times and recomposes nothing.
    val now = {
        if (dragging.value || held.value != null) CoverLook.seekTimes(dragging.value, drag.floatValue, held.value ?: -1L, 0L, durationMs)
        else times.longValue
    }
    val gone = { if (dragging.value || held.value != null) -1L else fadingFrom.longValue }
    val style = MaterialTheme.typography.labelSmall
    val longest = (durationMs / 1000).coerceAtLeast(0)
    // The new times come in as the old ones go; with nothing fading this is 1 and the old layer draws nothing.
    val inAlpha = Modifier.graphicsLayer { alpha = if (gone() < 0) 1f else fade.floatValue }
    val outAlpha = Modifier.graphicsLayer { alpha = 1f - fade.floatValue }
    Row(Modifier.fillMaxWidth(), Arrangement.SpaceBetween, Alignment.CenterVertically) {
        Box {
            LookTime({ gone().let { if (it < 0) "" else duration(it ushr 32) } }, duration(longest), quiet, style, end = false, modifier = outAlpha)
            LookTime({ duration(now() ushr 32) }, duration(longest), quiet, style, end = false, modifier = inAlpha)
        }
        // The centre slot carries whatever needs saying: an error, or the sleep timer. Empty the
        // rest of the time, holding its space so the two times either side never move.
        // While a timer is set the times are read here, so it recomposes this once a second and
        // keeps the minutes current; otherwise nothing here reads them. How the timer reads is
        // nori-core's (`words_sleep`), asked only while one is set.
        //
        // elapsedRealtime, not wall clock: sleepAt is set from SystemClock (PlayerConnection),
        // and subtracting one from the other gives a number about fifty years wide, which the
        // rounding then turned into a cheerful "1 min" for every timer ever set.
        if (sleepAt > 0) times.longValue
        val centre = error ?: if (sleepAtEndOfTrack || sleepAt > 0) say.sleep(sleepAtEndOfTrack, if (sleepAt > 0) sleepAt - android.os.SystemClock.elapsedRealtime() else 0) else ""
        val errorColour = MaterialTheme.colorScheme.error
        Box(Modifier.weight(1f).padding(horizontal = 8.dp), contentAlignment = Alignment.Center) {
            LookText(
                centre, if (error != null) androidx.compose.ui.graphics.ColorProducer { errorColour } else quiet,
                Modifier.fillMaxWidth(),
                style = MaterialTheme.typography.labelSmall,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            // An error or the sleep timer has the slot to itself.
            MixingLabel(mixing, free = centre.isEmpty(), quiet)
        }
        Box {
            LookTime({ gone().let { if (it < 0) "" else durationLeft(it and 0xFFFF_FFFFL) } }, durationLeft(longest), quiet, style, end = true, modifier = outAlpha)
            LookTime({ durationLeft(now() and 0xFFFF_FFFFL) }, durationLeft(longest), quiet, style, end = true, modifier = inAlpha)
        }
    }
}

/**
 * "MIXING", between the times while an AutoMix or a crossfade is being heard, the way Apple's player says
 * it. It fades in and out (counted in frames, like every fade on this page) where it stands, in a slot
 * that is there either way, so the times beside it never move. Nothing of it exists between mixes: the
 * word is only composed while it is shown or fading, and the flag it follows is pushed by the player when
 * a mix starts and ends, so a song playing on its own costs no recomposition, no frame and no wakeup.
 */
@Composable
private fun MixingLabel(mixing: androidx.compose.runtime.State<Boolean>, free: Boolean, colour: androidx.compose.ui.graphics.ColorProducer) {
    val on = mixing.value && free
    val shown = remember { Animatable(if (on) 1f else 0f) }
    LaunchedEffect(on) {
        val to = if (on) 1f else 0f
        if (shown.value == to) return@LaunchedEffect
        if (AppMotion.reduce) shown.snapTo(to) else shown.fadeByFrames(to, MIXING_FADE_MS)
    }
    // Read through a derived state, so the fade's frames redraw the word and recompose nothing.
    val present by remember { androidx.compose.runtime.derivedStateOf { shown.value > 0f } }
    if (!on && !present) return
    val word = remember { say.mixing.uppercase() }
    LookText(
        word, colour, Modifier.graphicsLayer { alpha = fadeEase(shown.value) },
        style = MaterialTheme.typography.labelSmall.copy(letterSpacing = androidx.compose.ui.unit.TextUnit(1.4f, androidx.compose.ui.unit.TextUnitType.Sp), fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold),
        maxLines = 1,
    )
}

/** How long "MIXING" takes to come and go. */
private const val MIXING_FADE_MS = 360f

@Composable
private fun Queue(vm: PlayerViewModel) {
    val state by vm.state.collectAsStateWithLifecycle()
    // In the order the songs will play, which under shuffle is not the order of the list itself. A drag
    // moves a song within the list, so reordering is offered only when the two are the same.
    // Which order, whether a drag may reorder it, which rows a swipe leaves and which have played are the
    // core's (`queue_rows`).
    val rows = remember(state.order, state.queue.size, state.shuffle, state.index) {
        vm.queueRows(state.queue.size, state.shuffle, state.index)
    }
    val order = remember(rows) { rows.order.map { it.toInt() } }
    val kept = remember(rows) { rows.kept.map { it.toInt() }.toSet() }
    val keys = remember(state.queue) { queueKeys(state.queue.map { it.id }) }
    // The song playing is the list's first row as it opens, the way Apple's queue is. The songs already
    // played sit above it, dimmed under "History", and are only seen by scrolling up; "Playing next"
    // heads what comes after it. An entry is a place in [order], or one of the two captions.
    val now = rows.now
    val entries = remember(order, now) { queueEntries(order.size, now) }
    val nowEntry = if (now >= 0) entries.indexOf(now) else 0
    val list = rememberLazyListState(initialFirstVisibleItemIndex = nowEntry.coerceAtLeast(0))
    // Nothing is reordered until the finger lifts. The held row follows it, the rows it passes step out
    // of the way, and the gap travels with it - reordering live would change the keys under the gesture
    // and cancel it, which is why a row could only ever be moved one place at a time. All of it is read
    // where the rows are drawn (QueueDrag), so a frame of the drag recomposes no row.
    val drag = remember { QueueDrag() }
    val undo = remember { QueueUndo<Song>() }
    val haptics = LocalHapticFeedback.current
    val scope = rememberCoroutineScope()
    val plain = reduceMotion()
    val look = LocalLook.current
    val quiet = androidx.compose.ui.graphics.ColorProducer { look.color(CoverLook.ON_VARIANT) }
    val accent = androidx.compose.ui.graphics.ColorProducer { look.color(CoverLook.ACCENT) }
    val ink = androidx.compose.ui.graphics.ColorProducer { look.color(CoverLook.ON) }
    // A swipe's strip in the page's own colours: the player is dressed in the cover's, not the theme's.
    val swipeColours = remember(look) {
        SwipeColours({ look.color(CoverLook.VEIL_13) }, { look.color(CoverLook.ACCENT) }, { look.color(CoverLook.ON_VARIANT) }, { look.color(CoverLook.ON_PRIMARY) })
    }
    var listWidth by remember { mutableFloatStateOf(0f) }

    // The jam this phone hosts heads the queue; its guests' songs carry who asked for them.
    val remote: dev.nori.music.app.vm.RemoteViewModel = androidx.lifecycle.viewmodel.compose.viewModel()
    val jam by remote.jam.collectAsStateWithLifecycle()
    val added by vm.jamAdded.collectAsStateWithLifecycle()
    val plate = androidx.compose.ui.graphics.ColorProducer { look.color(CoverLook.VEIL_13) }

  Box(Modifier.fillMaxSize()) {
    // Shuffle and repeat live here, pinned above the list - not in the transport, and never scrolled
    // away (the list opens at the playing row, which used to hide them).
    Column(Modifier.fillMaxSize()) {
        jam?.let { j ->
            if (j.hosting) JamHeader(j) { vm.cover(it, CoverSize.ROW) }
            else if (state.jamGuest) GuestJamHeader(j) { vm.cover(it, CoverSize.ROW) }
        }
        // A jam guest's queue is the host's to change: it only shows it.
        val edits = !state.jamGuest
        Row(Modifier.fillMaxWidth(), Arrangement.SpaceBetween, Alignment.CenterVertically) {
            Caption(remember { say.queue }, Modifier.padding(top = 4.dp, bottom = 8.dp))
            if (edits) Row(Modifier, Arrangement.spacedBy(4.dp), Alignment.CenterVertically) {
                val shuffleOn = state.shuffle
                val repeatOn = state.repeat != Repeat.OFF
                IconButton(vm::toggleShuffle, Modifier.size(44.dp)) {
                    LookIcon(Icons.Filled.Shuffle, say.shuffle, Modifier.size(22.dp)) { look.color(if (shuffleOn) CoverLook.ACCENT else CoverLook.ON_VARIANT) }
                }
                IconButton(vm::cycleRepeat, Modifier.size(44.dp)) {
                    LookIcon(
                        if (state.repeat == Repeat.ONE) Icons.Filled.RepeatOne else Icons.Filled.Repeat, say.repeat,
                        Modifier.size(22.dp),
                    ) { look.color(if (repeatOn) CoverLook.ACCENT else CoverLook.ON_VARIANT) }
                }
            }
        }
    val reorderable = rows.reorderable && edits
    val queueNow by rememberUpdatedState(state.queue)
    drag.size = state.queue.size
    // A song is dropped only among those still to come: not above the song playing, nor into what has played.
    drag.first = now + 1
    // When the song playing changes, the list goes with it, the new song to the top, as long as the list
    // was resting on the song that had been playing (or at its end, where it could go no further); a
    // list scrolled somewhere else stays where it was put. Where it rests is decided when a scroll ends,
    // not when the song changes: by then a reordered list (shuffle switched) is laid out again and its
    // top says nothing about where the user had left it. Both are asked once per event, not per frame.
    val nowKey = order.getOrNull(now)?.let { keys[it] }
    var followed by remember { mutableStateOf(nowKey) }
    // Opened at the song playing, so resting on it until a scroll says otherwise.
    val pinned = remember { booleanArrayOf(true) }
    // Whether the user has dragged the list since its last rest. Only their scrolls say where they left
    // it: one of the list's own, cut short by the next song (skips in quick succession), stops half way.
    val dragged = remember { booleanArrayOf(false) }
    // A song tapped in the list is followed to the top wherever the list was: the tap asked for it. So is
    // the first song of a new queue (a page's Play while the panel is open; `origin` moves with each): the
    // place the old one was scrolled to means nothing in it.
    var tapped by remember { mutableStateOf<String?>(null) }
    val origin = remember { intArrayOf(state.origin) }
    LaunchedEffect(list) {
        list.interactionSource.interactions.collect { if (it is androidx.compose.foundation.interaction.DragInteraction.Start) dragged[0] = true }
    }
    LaunchedEffect(list) {
        androidx.compose.runtime.snapshotFlow { list.isScrollInProgress }.collect { moving ->
            if (moving || !dragged[0]) return@collect
            dragged[0] = false
            val info = list.layoutInfo
            val top = info.visibleItemsInfo.firstOrNull { it.offset + it.size / 2 > info.viewportStartOffset }?.key
            pinned[0] = queueFollows(top, followed, !list.canScrollForward)
        }
    }
    LaunchedEffect(nowKey, nowEntry, state.origin) {
        followed = nowKey
        if (nowKey == null || drag.from >= 0 || dragged[0]) return@LaunchedEffect
        val asked = tapped == nowKey || origin[0] != state.origin
        tapped = null
        origin[0] = state.origin
        if (!asked && !pinned[0]) return@LaunchedEffect
        if (list.firstVisibleItemIndex == nowEntry && list.firstVisibleItemScrollOffset == 0) return@LaunchedEffect
        pinned[0] = true
        if (plain) list.scrollToItem(nowEntry) else list.animateScrollToItem(nowEntry)
    }
    // A drop has landed when the queue is no longer the one it was sent against. On that frame the rows
    // are laid out where they were already drawn: their shifts go (before this frame is laid out) and
    // they do not also slide there.
    val landed = drag.landed(state.queue)
    SideEffect { if (landed) drag.clear() }
    // The last row is cut off dead straight where the list ends, a few pixels above the song's title,
    // and those few pixels are the ones that flickered as a panel came or went: a row half drawn, over
    // a title arriving in the same place. It goes soft over the last stretch instead, the way the
    // lyrics do, and a mask is used rather than a colour laid on top because the page behind is the
    // cover's blur and any flat colour meeting it draws a line of its own.
    val fadeOut = with(androidx.compose.ui.platform.LocalDensity.current) { 28.dp.toPx() }
    LazyColumn(
        Modifier.fillMaxSize().weight(1f)
            .onSizeChanged { listWidth = it.width.toFloat() }
            .graphicsLayer { compositingStrategy = androidx.compose.ui.graphics.CompositingStrategy.Offscreen }
            .drawWithCache {
                // One brush per size, not one per frame of a scroll.
                val mask = androidx.compose.ui.graphics.Brush.verticalGradient(
                    listOf(androidx.compose.ui.graphics.Color.Black, androidx.compose.ui.graphics.Color.Transparent),
                    startY = size.height - fadeOut, endY = size.height,
                )
                // A pixel past each edge: the layer is clipped to whole pixels and a mask drawn to
                // the exact height leaves the last fractional row of it untouched.
                val at = Offset(-1f, size.height - fadeOut)
                val area = androidx.compose.ui.geometry.Size(size.width + 2f, fadeOut + 2f)
                onDrawWithContent {
                    drawContent()
                    drawRect(mask, topLeft = at, size = area, blendMode = androidx.compose.ui.graphics.BlendMode.DstIn)
                }
            },
        state = list,
    ) {
        items(
            entries.size,
            key = { e -> when (val at = entries[e]) { QUEUE_HISTORY -> "history"; QUEUE_NEXT -> "next"; else -> keys[order[at]] } },
            contentType = { e -> if (entries[e] < 0) "caption" else "song" },
        ) { e ->
            val at = entries[e]
            if (at < 0) {
                Caption(
                    if (at == QUEUE_HISTORY) say.history else say.upNext,
                    Modifier.animateItem(fadeInSpec = null, placementSpec = if (plain) null else tween(QUEUE_MOVE_MS, easing = androidx.compose.animation.core.FastOutSlowInEasing), fadeOutSpec = null)
                        .padding(top = if (at == QUEUE_HISTORY) 4.dp else 14.dp, bottom = 6.dp),
                )
                return@items
            }
            val i = order[at]
            val s = state.queue[i]
            // Already played: drawn quieter, above the song playing.
            val played = at < now
            val key = keys[i]
            // Only the row picked up and put down recomposes; the drag itself is read in the layer below.
            val held by remember(key) { derivedStateOf { drag.liftKey == key } }
            val place by rememberUpdatedState(at)
            val back = undo.returning?.takeIf { it.key == key }
            // A row put back by the undo comes in from the side it left by, and does not fade as well.
            val swipe = remember { SwipeState().apply { if (back != null && !plain && back.side != 0f) offset.floatValue = back.side * listWidth } }
            LaunchedEffect(Unit) {
                if (undo.returning?.key != key) return@LaunchedEffect
                undo.arrived(key)
                if (swipe.offset.floatValue != 0f) androidx.compose.animation.core.animate(
                    swipe.offset.floatValue, 0f, animationSpec = spring(dampingRatio = 0.9f, stiffness = 420f),
                ) { v, _ -> swipe.offset.floatValue = v }
            }
            val take = RowSwipe(Icons.Filled.PlaylistRemove, say.remove) {
                // Where the song is now: the list may have moved under a slow swipe.
                val now = vm.state.value.queue
                val index = queueKeys(now.map { it.id }).indexOf(key)
                if (index >= 0) {
                    undo.took(now[index], index, key, -1f)
                    vm.remove(index)
                }
            }
            Box(
                Modifier.fillMaxWidth()
                    .animateItem(
                        fadeInSpec = if (plain || back != null) null else tween(QUEUE_IN_MS),
                        placementSpec = if (plain || landed) null else tween(QUEUE_MOVE_MS, easing = androidx.compose.animation.core.FastOutSlowInEasing),
                        // A song that left the queue goes at once. Faded out, it stayed behind where it
                        // was when the fade never finished (a queue replaced or refilled under the panel):
                        // a whole old list drawn still over the real one, which scrolled under it.
                        fadeOutSpec = null,
                    )
                    .zIndex(if (held) 1f else 0f)
                    .graphicsLayer {
                        translationY = drag.shift(place)
                        if (drag.liftKey == key) {
                            val l = drag.lift.floatValue
                            shadowElevation = 14f * l; scaleX = 1f + 0.02f * l; scaleY = 1f + 0.02f * l
                        }
                    }
                    .onGloballyPositioned { if (drag.rowHeight == 0f) drag.rowHeight = it.size.height.toFloat() },
            ) {
                // The song playing only gives a little and comes back: a swipe does not stop the music
                // (the × does, on purpose). Leftwards only: rightwards from the edge is the back gesture's.
                val taken = if (i in kept || !edits) null else take
                SwipeBackdrop(swipe, null, taken, Modifier.matchParentSize(), swipeColours, reveal = true, inset = 12.dp)
                Row(
                    Modifier.fillMaxWidth()
                        .then(if (played) Modifier.graphicsLayer { alpha = QUEUE_PLAYED_ALPHA } else Modifier)
                        .then(if (edits) Modifier.swipeable(swipe, null, taken, null, gone = true, resist = i in kept).clickable { tapped = key; vm.skipTo(i) } else Modifier)
                        .padding(vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        Cover(vm.cover(s.coverArt, CoverSize.ROW), 44.dp, radius = 6.dp)
                        // The song playing: its cover darkened under the playing bars, as in Apple's queue.
                        if (i == state.index) {
                            Box(Modifier.matchParentSize().drawBehind { drawRoundRect(QUEUE_NOW_VEIL, cornerRadius = androidx.compose.ui.geometry.CornerRadius(6.dp.toPx())) })
                            PlayingBars(Color.White, Modifier.size(16.dp))
                        }
                    }
                    Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
                        LookText(
                            s.title, if (i == state.index) accent else ink,
                            maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyLarge,
                        )
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            // Added by hand: plays before the rest of the queue carries on. A jam guest's song says who.
                            val by = added[s.id]
                            if (by != null) AddedBy(by, ink, plate)
                            else if (i in state.queued) LookIcon(Icons.AutoMirrored.Filled.QueueMusic, say.addedByYou, Modifier.padding(end = 4.dp).size(14.dp), accent)
                            LookText(s.artist, quiet, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
                        }
                    }
                    if (edits) IconButton({ undo.took(s, i, key, 0f); vm.remove(i) }, Modifier.size(38.dp)) {
                        LookIcon(Icons.Filled.Close, say.remove, Modifier.size(19.dp), quiet)
                    }
                    // The handle's room is kept on every row while the list can be reordered, so every ×
                    // stands in one column; only the rows still to come show a handle in it.
                    androidx.compose.animation.AnimatedVisibility(
                        reorderable,
                        enter = androidx.compose.animation.fadeIn() + androidx.compose.animation.expandHorizontally(),
                        exit = androidx.compose.animation.fadeOut() + androidx.compose.animation.shrinkHorizontally(),
                    ) { Box(Modifier.size(44.dp)) { androidx.compose.animation.AnimatedVisibility(
                        at > now,
                        enter = androidx.compose.animation.fadeIn(),
                        exit = androidx.compose.animation.fadeOut(),
                    ) { LookIcon(
                        Icons.Filled.DragHandle, say.reorder,
                        tint = { if (drag.liftKey == key) androidx.compose.ui.graphics.lerp(quiet(), accent(), drag.lift.floatValue) else quiet() },
                        modifier = Modifier.size(44.dp).padding(11.dp).pointerInput(Unit) {
                            var lifting: kotlinx.coroutines.Job? = null
                            fun lift(to: Float) {
                                lifting?.cancel()
                                lifting = scope.launch {
                                    if (plain) drag.lift.floatValue = to
                                    else androidx.compose.animation.core.animate(drag.lift.floatValue, to, animationSpec = tween(QUEUE_LIFT_MS)) { v, _ -> drag.lift.floatValue = v }
                                    if (to == 0f && drag.liftKey == key) drag.liftKey = null
                                }
                            }
                            // Let go: the row settles into the slot it is over, and only then is the move
                            // sent; the rows stay drawn where they are until the queue has changed.
                            fun drop(send: Boolean) {
                                val from = drag.from
                                if (from < 0) return
                                val to = if (send) drag.target() else from
                                val h = drag.rowHeight
                                scope.launch {
                                    val rest = (to - from) * h
                                    if (plain) drag.offset.floatValue = rest
                                    else androidx.compose.animation.core.animate(drag.offset.floatValue, rest, animationSpec = tween(QUEUE_DROP_MS, easing = androidx.compose.animation.core.FastOutSlowInEasing)) { v, _ -> drag.offset.floatValue = v }
                                    lift(0f)
                                    if (to == from || drag.from != from) { if (drag.from == from) drag.clear(); return@launch }
                                    drag.landing = queueNow
                                    vm.move(from, to)
                                    // A move that never arrives does not hold the rows up for good.
                                    kotlinx.coroutines.delay(QUEUE_LANDING_MS)
                                    if (drag.landing != null && drag.from == from) drag.clear()
                                }
                            }
                            detectDragGestures(
                                onDragStart = {
                                    drag.clear()
                                    drag.from = place
                                    drag.liftKey = key
                                    lift(1f)
                                    haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                                },
                                onDragEnd = { drop(send = true) },
                                onDragCancel = { drop(send = false) },
                            ) { change, d -> change.consume(); drag.offset.floatValue += d.y }
                        },
                    ) } } }
                }
            }
        }
    }
    }
    UndoPill(undo, plain, Modifier.align(Alignment.BottomCenter).padding(bottom = 10.dp)) { t ->
        vm.restore(t.item, t.index)
    }
  }
}

/** The queue's two captions among its [queueEntries]: above the songs played, and above those to come. */
private const val QUEUE_HISTORY = -1
private const val QUEUE_NEXT = -2

/**
 * The queue panel's entries for [size] rows with the song playing at row [now] (-1 none): the rows played
 * under "History", the song playing, then "Playing next" and the rest. Each is a row, or a caption.
 */
private fun queueEntries(size: Int, now: Int): List<Int> = buildList(size + 2) {
    if (now > 0) add(QUEUE_HISTORY)
    for (at in 0 until size) {
        add(at)
        if (at == now && at < size - 1) add(QUEUE_NEXT)
    }
}

/** How much a song already played shows through. */
private const val QUEUE_PLAYED_ALPHA = 0.45f
/** The shade over the playing song's cover, under its bars. */
private val QUEUE_NOW_VEIL = Color.Black.copy(alpha = 0.4f)

/** How the queue's rows come and move: a row appearing, the rest closing up. */
private const val QUEUE_IN_MS = 220
private const val QUEUE_MOVE_MS = 260
/** A held row lifting and settling, and a dropped row going into its slot. */
private const val QUEUE_LIFT_MS = 150
private const val QUEUE_DROP_MS = 140
/** How long the rows wait for a move to reach the queue before they give it up. */
private const val QUEUE_LANDING_MS = 1_000L
/** How long the undo is offered for. */
private const val UNDO_MS = 5_000L
private const val UNDO_IN_MS = 240f
private const val UNDO_OUT_MS = 170f

/**
 * "Removed “song” · Undo", a small pill over the foot of the queue for a few seconds after a song is taken
 * out. It rises and fades in, and sinks and fades out showing what it said, taking no taps as it goes. A
 * second song taken out while it is up replaces the words in place.
 */
@Composable
private fun UndoPill(undo: QueueUndo<Song>, plain: Boolean, modifier: Modifier, restore: (QueueUndo.Taken<Song>) -> Unit) {
    val now = undo.shown
    val shown = remember { Animatable(0f) }
    var last by remember { mutableStateOf(now) }
    if (now != null) last = now
    LaunchedEffect(now) {
        val t = now ?: return@LaunchedEffect
        kotlinx.coroutines.delay(UNDO_MS)
        undo.expire(t)
    }
    LaunchedEffect(now != null) {
        val to = if (now != null) 1f else 0f
        if (shown.value == to) return@LaunchedEffect
        if (plain) shown.animateTo(to, tween(120, easing = androidx.compose.animation.core.LinearEasing))
        else shown.fadeByFrames(to, if (to == 1f) UNDO_IN_MS else UNDO_OUT_MS)
    }
    val present by remember { derivedStateOf { shown.value > 0f } }
    val t = last ?: return
    if (now == null && !present) return
    val rise = with(androidx.compose.ui.platform.LocalDensity.current) { 12.dp.toPx() }
    MessagePill(
        say.queueRemoved(t.item.title), LocalLook.current,
        modifier
            .widthIn(max = 320.dp)
            .graphicsLayer {
                val v = fadeEase(shown.value)
                alpha = v
                if (!plain) translationY = (1f - v) * rise
            },
        // Taps only while it is there to be read, not as it goes.
        action = say.undo, actionEnabled = now != null, onAction = { if (undo.shown === t) undo.undo()?.let(restore) },
    ) {
        // The pill takes every touch on it, as it fades out too: a tap meant for Undo a moment late must not
        // fall through to the × of the row under it and take another song out. It is taken by a layer
        // beside the button, under it, never by a parent of it: a parent consuming the touch cancels the
        // button's tap on the first move of the finger (Compose's tap checks for exactly that), and a
        // finger on a phone always moves a little, so Undo never did anything there.
        Spacer(Modifier.matchParentSize().pointerInput(Unit) { awaitPointerEventScope { while (true) awaitPointerEvent().changes.forEach { it.consume() } } })
    }
}
