package dev.nori.music.app.ui

import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.material.icons.filled.DragHandle
import androidx.compose.ui.zIndex
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.pulltorefresh.PullToRefreshState
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material3.Text
import androidx.compose.runtime.setValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.mutableStateOf
import androidx.compose.material3.IconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.DropdownMenu
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.Box
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.HomeViewModel
import dev.nori.music.ffi.settings.HomeRow
import dev.nori.music.ffi.model.Album

@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(actions: ActionsViewModel, vm: HomeViewModel = viewModel()) {
    val load by vm.ui.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    val settings: dev.nori.music.app.vm.SettingsViewModel = viewModel()
    var rearranging by remember { mutableStateOf(false) }
    if (rearranging) { RowOrder(settings) { rearranging = false }; return }
    LoadBox(load) { ui ->
        val arrival = rememberArrival()
        val rise = with(LocalDensity.current) { Arrival.RISE.toPx() }
        val refreshing by vm.refreshing.collectAsStateWithLifecycle()
        val pull = rememberPullToRefreshState()
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = vm::refresh,
            state = pull,
            indicator = { RefreshMark(pull, refreshing) },
        ) {
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            // Shuffling the whole library and picking the server's queue back up are things you do
            // occasionally, so they live behind the title's menu rather than as two buttons across the
            // top of the page: what belongs at the top of this screen is music.
            item(key = "title") {
                var menu by remember { mutableStateOf(false) }
                LargeTitle(say.listenNow, Modifier.arriving(arrival, 0, rise)) {
                    Box {
                        IconButton({ menu = true }) { Icon(Icons.Filled.MoreHoriz, say.more, Modifier.size(22.dp)) }
                        DropdownMenu(menu, { menu = false }) {
                            DropdownMenuItem({ Text(say.shuffleSongs) }, { actions.shuffleAll(); menu = false })
                            DropdownMenuItem({ Text(say.shuffleAlbums) }, { actions.shuffleAlbums(); menu = false })
                            DropdownMenuItem({ Text(say.resumeFromServer) }, { actions.resumeFromServer(); menu = false })
                            DropdownMenuItem({ Text(say.rearrangeRows) }, { rearranging = true; menu = false })
                        }
                    }
                }
            }
            // Favourites are always here; the mixes join them when the taste model is on (MixesViewModel).
            item(key = "mixes") { Column(Modifier.arriving(arrival, 1, rise)) { SectionTitle(say.forYou); MixTiles() } }
            // The shelves carry on the count the sections above started, so each one arrives a moment
            // after the one over it; an empty shelf is not drawn and does not take a place in the order.
            var place = 2
            ui.rows.forEach { shelf ->
                // The favourite playlists are a selection of something the page fetches once for all of
                // it, so their shelf is filled from there - but it stands where the user put it in the
                // order. It used to be drawn above every shelf whatever the order said, which is why it
                // could be at the bottom of the list and at the top of the page at the same time.
                val s = if (shelf.row == dev.nori.music.ffi.settings.HomeRow.PINNED) dev.nori.music.app.vm.Shelf.Playlists(shelf.row, ui.pinned) else shelf
                if (s.isEmpty) return@forEach
                when (s) {
                    is dev.nori.music.app.vm.Shelf.Albums -> shelf(say.homeRow(s.row), s.albums, vm, arrival, place++, rise)
                    is dev.nori.music.app.vm.Shelf.Playlists -> playlistShelf(say.homeRow(s.row), s.playlists, vm, arrival, place++, rise)
                    is dev.nori.music.app.vm.Shelf.Songs -> songShelf(say.homeRow(s.row), s.songs, vm, actions, arrival, place++, rise, s.origin)
                }
            }
        }
        }
    }
}

/**
 * What a pull at the top of the page looks like: a thin ring, no container and nothing behind it. The
 * Material indicator is a filled circle on a raised plate, which on this page reads as a button that
 * has landed in the wrong place. While the finger is down the ring fills as far as the pull has come,
 * which is a static value read in the draw phase; only once the refresh is really running does it turn,
 * and it is gone the moment that is over. At rest it is drawn at zero opacity, so an idle page has
 * nothing at its top and nothing animating there either.
 */
@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
private fun androidx.compose.foundation.layout.BoxScope.RefreshMark(state: PullToRefreshState, refreshing: Boolean) {
    val tint = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.5f)
    Box(
        Modifier.align(androidx.compose.ui.Alignment.TopCenter)
            .graphicsLayer {
                val d = state.distanceFraction
                alpha = if (refreshing) 1f else (d * 1.6f - 0.15f).coerceIn(0f, 1f)
                translationY = d.coerceIn(0f, 1.3f) * 52.dp.toPx()
            }
            .padding(top = 12.dp),
    ) {
        if (refreshing) androidx.compose.material3.CircularProgressIndicator(Modifier.size(18.dp), color = tint, strokeWidth = 2.dp)
        else androidx.compose.material3.CircularProgressIndicator(
            progress = { state.distanceFraction.coerceIn(0f, 1f) },
            modifier = Modifier.size(18.dp), color = tint, strokeWidth = 2.dp,
            trackColor = androidx.compose.ui.graphics.Color.Transparent,
            gapSize = 0.dp,
        )
    }
}

private fun androidx.compose.foundation.lazy.LazyListScope.shelf(
    title: String,
    albums: List<Album>,
    vm: HomeViewModel,
    arrival: State<Float>,
    place: Int,
    rise: Float,
) {
    item(key = title, contentType = "shelf") {
        val nav = LocalNav.current
        Column(Modifier.arriving(arrival, place, rise)) {
            SectionTitle(title)
            LazyRow(contentPadding = PaddingValues(horizontal = Space.gutter), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                items(albums, key = { it.id }, contentType = { "album" }) { a -> AlbumCard(a, vm.cover(a.coverArt, CoverSize.CARD), 150.dp, { nav.album(a.id, a) }) }
            }
        }
    }
}

/** Playlists laid out as the albums are, so a shelf of them reads as the same kind of thing. */
private fun androidx.compose.foundation.lazy.LazyListScope.playlistShelf(
    title: String,
    playlists: List<dev.nori.music.ffi.model.Playlist>,
    vm: HomeViewModel,
    arrival: State<Float>,
    place: Int,
    rise: Float,
) {
    item(key = title, contentType = "shelf") {
        val nav = LocalNav.current
        Column(Modifier.arriving(arrival, place, rise)) {
            SectionTitle(title)
            LazyRow(contentPadding = PaddingValues(horizontal = Space.gutter), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                items(playlists, key = { it.id }, contentType = { "playlist" }) { p ->
                    CoverCard(p.name, remember(p.songCount) { say.songs(p.songCount.toInt()) }, vm.cover(p.coverArt, CoverSize.CARD), 150.dp, { nav.playlist(p.id, p) })
                }
            }
        }
    }
}

/** A shelf of songs plays from where it is tapped, the rest of the shelf behind it, as a list would. */
private fun androidx.compose.foundation.lazy.LazyListScope.songShelf(
    title: String,
    songs: List<dev.nori.music.ffi.model.Song>,
    vm: HomeViewModel,
    actions: dev.nori.music.app.vm.ActionsViewModel,
    arrival: State<Float>,
    place: Int,
    rise: Float,
    /** The shelf: the songs played from it are its queue. */
    from: dev.nori.music.ffi.model.PageOrigin,
) {
    item(key = title, contentType = "shelf") {
        Column(Modifier.arriving(arrival, place, rise)) {
            SectionTitle(title)
            LazyRow(contentPadding = PaddingValues(horizontal = Space.gutter), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                itemsIndexed(songs, key = { _, s -> s.id }, contentType = { _, _ -> "song" }) { i, s ->
                    CoverCard(s.title, s.artist, vm.cover(s.coverArt, CoverSize.CARD), 150.dp, { actions.play(songs, i, from) })
                }
            }
        }
    }
}

/**
 * The home page's sections do not appear all at once: each one fades up and rises the last few pixels
 * into place a moment after the one above it, so the page reads top to bottom the way the pages it is
 * part of now arrive (see PageMotion in App.kt). It is one run, started when the page's content first
 * exists and over inside four tenths of a second, and the fourth section is the last to wait - with a
 * dozen shelves the page would otherwise still be assembling itself after the user had begun to scroll.
 */
private object Arrival {
    /** How long the whole run lasts, and how far apart two sections start within it. */
    const val RUN = 380
    private const val STEP = 40
    /** Each section's own move, and the last place that still waits its turn. */
    private const val MOVE = 220
    private const val LAST = 4

    /** The same curve the pages use: quick to appear, unhurried about coming to rest. */
    private val Settle = CubicBezierEasing(0.05f, 0.7f, 0.1f, 1f)

    val RISE = 14.dp

    /** Where one section is in its own move, given how far the single run driving all of them has got. */
    fun at(run: Float, place: Int): Float {
        val ms = run * RUN - minOf(place, LAST) * STEP
        return Settle.transform((ms / MOVE).coerceIn(0f, 1f))
    }
}

/**
 * The one animation behind the whole page. It runs once, the first time the page is on screen after
 * the app starts: a section scrolled away and back reads a run that finished long ago, and fresh data
 * for a page already showing does not start it again, so nothing here moves while the page is sitting
 * still.
 *
 * Once per process, not once per visit. Coming back to the page - a pop from an album, a tab tap -
 * composes it again from nothing, and the run used to start again with it: the page slid back into
 * place (PageMotion) with every section at nought, so it arrived as a black card and its sections
 * only appeared once the slide was over. The page is what is coming back; it should look as it did.
 */
@Composable
private fun rememberArrival(): State<Float> {
    val plain = reduceMotion()
    val progress = remember { Animatable(if (arrived) 1f else 0f) }
    // Linear, because the curve belongs to each section's own move and not to the clock they share.
    LaunchedEffect(plain) {
        arrived = true
        if (plain) progress.snapTo(1f)
        else if (progress.value < 1f) progress.animateTo(1f, tween(Arrival.RUN, easing = LinearEasing))
    }
    return progress.asState()
}

/** Whether the home page has arrived once already since the app started; see rememberArrival. */
private var arrived = false

/** Read in the draw phase: the run redraws the sections on screen and recomposes none of them. */
private fun Modifier.arriving(arrival: State<Float>, place: Int, rise: Float) = graphicsLayer {
    val t = Arrival.at(arrival.value, place)
    alpha = t
    translationY = rise * (1f - t)
}


/**
 * Which shelves the home page has, and in what order. Both live here rather than half here and half in
 * Settings: the rows are one thing, and hunting through a settings page for a switch that belongs to a
 * page you are looking at is the sort of thing this app is trying not to do.
 *
 * A row is picked up by holding it anywhere, not by a handle at its edge - there is nothing else to do
 * to a row here, so the whole row may as well be the grip. A long press is what starts it, because the
 * list itself scrolls and a plain drag could not mean both.
 */
@Composable
private fun RowOrder(settings: dev.nori.music.app.vm.SettingsViewModel, onDone: () -> Unit) {
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    val shown = prefs.homeRows
    // Which rows are left out, and where one switched on or off goes, are the core's (browse.rs).
    val hidden = remember(shown) { dev.nori.music.ffi.library.homeRowsHidden(HomeRow.entries.map { it.name }, shown.map { it.name }).map(HomeRow::valueOf) }
    fun toggled(row: HomeRow, on: Boolean) = settings.update { p ->
        p.copy(homeRows = dev.nori.music.ffi.library.homeRowsToggled(p.homeRows.map { it.name }, row.name, on).map(HomeRow::valueOf))
    }
    // Tracked by which shelf is being held, never by its position: the position changes the instant the
    // list reorders, and a gesture keyed on that is cancelled mid-drag - which is why a row could only
    // be moved one place per press. The offset keeps the held row under the finger while the rest slide
    // past it, so the gap follows the finger instead of the row snapping away from it.
    var held by remember { mutableStateOf<HomeRow?>(null) }
    var dragOffset by remember { mutableFloatStateOf(0f) }
    var rowHeight by remember { mutableFloatStateOf(0f) }
    val haptics = androidx.compose.ui.platform.LocalHapticFeedback.current

    Column(Modifier.fillMaxSize()) {
        LargeTitle(say.rows) {
            androidx.compose.material3.TextButton(onDone) { Text(say.done, style = MaterialTheme.typography.titleSmall) }
        }
        Caption(say.holdARowToMoveIt, Modifier.padding(start = Space.gutter, bottom = 8.dp))
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            items(shown, key = { it.name }) { row ->
                val dragged = held == row
                Row(
                    Modifier.fillMaxWidth()
                        .zIndex(if (dragged) 1f else 0f)
                        .graphicsLayer {
                            if (dragged) { translationY = dragOffset; shadowElevation = 14f; scaleX = 1.02f; scaleY = 1.02f }
                        }
                        .onGloballyPositioned { if (rowHeight == 0f) rowHeight = it.size.height.toFloat() }
                        // Keyed on the row, which never changes, so one press can carry it the whole way.
                        .pointerInput(row) {
                            detectDragGesturesAfterLongPress(
                                onDragStart = {
                                    held = row; dragOffset = 0f
                                    haptics.performHapticFeedback(androidx.compose.ui.hapticfeedback.HapticFeedbackType.LongPress)
                                },
                                onDragEnd = { held = null; dragOffset = 0f },
                                onDragCancel = { held = null; dragOffset = 0f },
                            ) { change, drag ->
                                change.consume()
                                dragOffset += drag.y
                                val h = rowHeight.takeIf { it > 0f } ?: return@detectDragGesturesAfterLongPress
                                var at = settings.prefs.value.homeRows.indexOf(row)
                                while (dragOffset >= h && at < settings.prefs.value.homeRows.lastIndex) {
                                    settings.moveHomeRow(at, at + 1); at++; dragOffset -= h
                                    haptics.performHapticFeedback(androidx.compose.ui.hapticfeedback.HapticFeedbackType.TextHandleMove)
                                }
                                while (dragOffset <= -h && at > 0) {
                                    settings.moveHomeRow(at, at - 1); at--; dragOffset += h
                                    haptics.performHapticFeedback(androidx.compose.ui.hapticfeedback.HapticFeedbackType.TextHandleMove)
                                }
                            }
                        }
                        .padding(horizontal = Space.gutter, vertical = 14.dp),
                    verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                ) {
                    Text(say.homeRow(row), Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                    // Turning a row off leaves it in the list below rather than taking it away, so it is
                    // clear where it went and how to have it back.
                    NoriSwitch(true, { _ -> toggled(row, false) })
                }
                Hairline()
            }
            if (hidden.isNotEmpty()) {
                item(key = "hidden") { SectionTitle(say.notShown) }
                items(hidden, key = { it.name }) { row ->
                    Row(
                        Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 14.dp),
                        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                    ) {
                        Text(say.homeRow(row), Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        // It comes back at the end of the page, where it can be seen, and can be carried
                        // up from there.
                        NoriSwitch(false, { _ -> toggled(row, true) })
                    }
                    Hairline()
                }
            }
        }
    }
}

