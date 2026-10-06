package dev.nori.music.app.ui

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.Crossfade
import androidx.compose.animation.SizeTransform
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyItemScope
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.DownloadDone
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.outlined.Downloading
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.State
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.layout
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.downloads.DownloadMark
import dev.nori.music.downloads.DownloadPhase
import dev.nori.music.downloads.processing
import dev.nori.music.downloads.DownloadLines
import androidx.compose.ui.platform.LocalContext
import dev.nori.music.downloads.DownloadState
import dev.nori.music.ffi.model.Song
import kotlinx.coroutines.flow.StateFlow
import kotlin.math.roundToInt

// ---- the download mark on song rows ----

/**
 * What every song row needs to draw its download mark, read once for the whole app. The states are
 * handed down unread, so a download changing phase recomposes the small mark on each visible row and
 * nothing else, and a percent moving recomposes one ring.
 */
@Stable
class DownloadMarks(val state: State<DownloadState>, val marks: State<Map<String, DownloadMark>>, val plain: State<Boolean>)

val LocalDownloadMarks = staticCompositionLocalOf<DownloadMarks?> { null }

@Composable
fun rememberDownloadMarks(actions: ActionsViewModel): DownloadMarks {
    val state = actions.downloads.collectAsStateWithLifecycle()
    val marks = actions.downloadMarks.collectAsStateWithLifecycle()
    val plain = rememberUpdatedState(reduceMotion())
    return remember(state, marks, plain) { DownloadMarks(state, marks, plain) }
}

/** The glyph a row's mark shows (the core's `download_glyph`). */
private typealias Glyph = dev.nori.music.ffi.library.DownloadGlyph

/** The core's answer for each phase (or none) and state, asked once each: a row asks on every composition. */
private object Glyphs {
    private val made = arrayOfNulls<Glyph>((DownloadPhase.entries.size + 1) * 4)
    fun of(phase: DownloadPhase?, downloaded: Boolean, pending: Boolean): Glyph {
        val i = ((phase?.ordinal ?: -1) + 1) * 4 + (if (downloaded) 2 else 0) + (if (pending) 1 else 0)
        return made[i] ?: dev.nori.music.ffi.library.downloadGlyph(phase, downloaded, pending).also { made[i] = it }
    }
}

/** The last glyph a row showed, kept so it can fade out rather than vanish. A plain field: never drawn from. */
private class LastGlyph(var value: Glyph, val bornEmpty: Boolean)

/** How big the mark is: the size of the downloaded icon it grew out of. */
private val MARK = 15.dp

/** The fixed width a song row gives its download mark, so the marks of every row line up. */
val MARK_SLOT = 26.dp

/**
 * Where a song row says how its download stands: a faint empty ring while it waits, a ring closing
 * clockwise while it arrives, the downloaded icon once it is here, a quiet warning if it failed. Rows
 * that have never had a mark cost one remembered object; a mark that arrives while the row is on
 * screen grows in, and one that goes shrinks away, so nothing snaps into place.
 */
@Composable
fun DownloadSlot(id: String, downloaded: Boolean, tint: Color) {
    val all = LocalDownloadMarks.current
    if (all == null) {
        if (downloaded) Icon(Icons.Filled.DownloadDone, say.downloaded, Modifier.size(MARK), tint)
        return
    }
    val mark = all.marks.value[id]
    val glyph = Glyphs.of(mark?.phase, downloaded, id in all.state.value.pendingIds)
    val last = remember { LastGlyph(glyph, glyph == Glyph.NONE) }
    if (glyph != Glyph.NONE) last.value = glyph
    if (last.value == Glyph.NONE) return
    MarkBody(glyph, last.value, mark, last.bornEmpty, all.plain.value, tint)
}

@Composable
private fun MarkBody(glyph: Glyph, drawn: Glyph, mark: DownloadMark?, growIn: Boolean, plain: Boolean, tint: Color) {
    val presence = remember { Animatable(if (growIn) 0f else 1f) }
    val target = if (glyph == Glyph.NONE) 0f else 1f
    LaunchedEffect(target) {
        if (presence.value != target) presence.animateTo(target, if (plain) snap() else tween(240, easing = FastOutSlowInEasing))
    }
    // The width is read in the layout phase and the fade in the draw phase: growing in re-lays out
    // this one row for a quarter of a second and recomposes nothing.
    val box = Modifier
        .layout { measurable, constraints ->
            val p = measurable.measure(constraints)
            val w = (p.width * presence.value).roundToInt()
            layout(w, p.height) { p.place(IntOffset((w - p.width) / 2, 0)) }
        }
        .graphicsLayer {
            alpha = presence.value
            val s = 0.6f + 0.4f * presence.value
            scaleX = s; scaleY = s
        }
    val accent = MaterialTheme.colorScheme.primary
    // A song that was already downloaded when the row appeared has nothing to change into but gone,
    // which the presence above handles: it is drawn as the plain icon it always was.
    if (drawn == Glyph.DONE && mark == null && !growIn) {
        Icon(Icons.Filled.DownloadDone, say.downloaded, box.size(MARK), tint)
        return
    }
    AnimatedContent(
        drawn, box,
        transitionSpec = {
            if (plain) fadeIn(snap()) togetherWith fadeOut(snap())
            // The finished icon rises out of the closing ring rather than replacing it.
            else (fadeIn(tween(220, delayMillis = 60)) + scaleIn(tween(260, easing = FastOutSlowInEasing), initialScale = 0.6f)) togetherWith
                (fadeOut(tween(180)) + scaleOut(tween(220), targetScale = 1.2f)) using SizeTransform(clip = false)
        },
        label = "download mark",
    ) { g ->
        when (g) {
            Glyph.RING -> DownloadRing(mark?.progress, MARK, 1.5.dp, tint.copy(alpha = 0.3f), accent, plain)
            Glyph.FAILED -> Icon(Icons.Outlined.ErrorOutline, say.downloadFailed, Modifier.size(MARK), tint.copy(alpha = 0.85f))
            else -> Icon(Icons.Filled.DownloadDone, say.downloaded, Modifier.size(MARK), tint)
        }
    }
}

/**
 * A thin ring that closes clockwise as a download arrives. [progress] null is a song still waiting (the
 * faint track alone); a negative value is a download whose size nobody knows, drawn as a short arc
 * turning slowly. Between the four updates a second the fill glides, and it is read in the draw phase:
 * an update recomposes this ring, a frame of the glide only redraws it.
 */
@Composable
fun DownloadRing(progress: StateFlow<Float>?, size: Dp, stroke: Dp, track: Color, fill: Color, plain: Boolean, modifier: Modifier = Modifier) {
    val value = progress?.collectAsStateWithLifecycle()?.value
    val known = value == null || value >= 0f
    val still = plain || LocalPageCovered.current
    Crossfade(known, modifier.size(size), animationSpec = if (plain) snap() else tween(200), label = "ring") { determinate ->
        if (determinate) {
            val shown = remember { Animatable(0f) }
            val to = (value ?: 0f).coerceAtLeast(0f)
            LaunchedEffect(to, still) { if (still) shown.snapTo(to) else shown.animateTo(to, tween(280, easing = LinearEasing)) }
            Canvas(Modifier.size(size)) {
                ring(track, stroke)
                val sweep = 360f * shown.value
                if (sweep > 0f) arc(fill, stroke, -90f, sweep)
            }
        } else if (still) {
            Canvas(Modifier.size(size)) { ring(track, stroke); arc(fill, stroke, -90f, 70f) }
        } else {
            val turn by rememberInfiniteTransition(label = "ring turn").animateFloat(
                0f, 360f, infiniteRepeatable(tween(1600, easing = LinearEasing), RepeatMode.Restart), label = "ring turn",
            )
            Canvas(Modifier.size(size).graphicsLayer { rotationZ = turn }) { ring(track, stroke); arc(fill, stroke, -90f, 70f) }
        }
    }
}

private fun DrawScope.ring(color: Color, stroke: Dp) {
    val w = stroke.toPx()
    drawCircle(color, radius = (size.minDimension - w) / 2f - 0.5f, style = Stroke(w))
}

private fun DrawScope.arc(color: Color, stroke: Dp, start: Float, sweep: Float) {
    val w = stroke.toPx()
    val inset = w / 2f + 0.5f
    drawArc(
        color, start, sweep, useCenter = false,
        topLeft = androidx.compose.ui.geometry.Offset(inset, inset),
        size = androidx.compose.ui.geometry.Size(size.width - inset * 2, size.height - inset * 2),
        style = Stroke(w, cap = StrokeCap.Round),
    )
}

// ---- the downloads screen ----

/**
 * What is downloading, waiting, failed and finished lately, with a way to stop or retry each. The
 * rows are keyed by song, so a song moving from waiting to downloading to finished slides between the
 * sections instead of disappearing from one and appearing in another.
 */
@Composable
fun DownloadsScreen(actions: ActionsViewModel) {
    val nav = LocalNav.current
    val sections by actions.downloadSections.collectAsStateWithLifecycle()
    // Speed and time left move every second while something downloads; the words are the core's,
    // asked again on this beat only while the screen is seen and something is running.
    val running = (sections?.active?.size ?: 0) > 0
    val seen = seen()
    val beat by androidx.compose.runtime.produceState(0, running, seen) {
        while (running && seen) { kotlinx.coroutines.delay(1_000); value++ }
    }
    val plain = reduceMotion()
    val cover = { id: String? -> actions.cover(id, CoverSize.ROW) }
    var confirm by remember { mutableStateOf(false) }
    val s = sections
    val unfinished = s?.let { it.active.size + it.queued.size + it.failed.size } ?: 0
    // Whether stopping everything asks first, and what it says, are the core's (`words_stop_all`).
    val stop = remember(unfinished) { StopAll(unfinished > 1, say.stopAllTitle, say.stopAllText(unfinished)) }
    NoriDialog(confirm, { confirm = false }) {
        AlertCard(
            title = { Text(stop.title) },
            text = { Text(stop.text) },
            confirmButton = { TextButton({ actions.cancelAllDownloads(); confirm = false }) { Text(say.stopAll) } },
            dismissButton = { TextButton({ confirm = false }) { Text(say.cancel) } },
        )
    }
    Column {
        Row(Modifier.fillMaxWidth().padding(start = 4.dp, end = Space.gutter - 8.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, say.back) }
            Spacer(Modifier.weight(1f))
            AnimatedVisibility(unfinished > 0, enter = fadeIn(tween(if (plain) 0 else 200)), exit = fadeOut(tween(if (plain) 0 else 200))) {
                Text(
                    say.stopAll,
                    Modifier.clip8().clickable { if (stop.asks) confirm = true else actions.cancelAllDownloads() }.padding(horizontal = 8.dp, vertical = 8.dp),
                    style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary,
                )
            }
        }
        LargeTitle(say.downloads)
        val res = LocalContext.current.resources
        val summary = s?.let { beat.let { _ -> DownloadLines.summary(res, it.active.size, it.queued.size, it.failed.size) } }.orEmpty()
        AnimatedContent(
            summary, Modifier.padding(start = Space.gutter, end = Space.gutter, bottom = 6.dp),
            transitionSpec = { fadeIn(tween(if (plain) 0 else 180)) togetherWith fadeOut(tween(if (plain) 0 else 120)) },
            label = "summary",
        ) { text ->
            Text(text, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
        }
        // Until the lists are worked out there is nothing to say, not "no downloads" for a frame.
        if (s == null) return@Column
        LazyColumn(contentPadding = PaddingValues(bottom = Space.section + LocalChromeInset.current)) {
            // Every section's rows come from this one call, so a row moving between sections is the same
            // row: its second line cross-fades rather than being drawn anew.
            fun LazyListScope.section(key: String, title: String, songs: List<Song>, section: DownloadPhase, action: (@Composable RowScope.() -> Unit)? = null) {
                if (songs.isEmpty()) return
                item(key = "h-$key", contentType = "header") { SectionHeader(title, moving(plain), action) }
                itemsIndexed(songs, key = { _, song -> song.id }, contentType = { _, _ -> "download" }) { i, song ->
                    // A saved song still being processed is listed with the downloading ones and says what it waits for.
                    val phase = LocalDownloadMarks.current?.marks?.value?.get(song.id)?.phase?.takeIf { section == DownloadPhase.DOWNLOADING && it.processing } ?: section
                    DownloadRow(
                        song, cover(song.coverArt), phase, i < songs.lastIndex, plain, moving(plain),
                        sub = if (phase == DownloadPhase.DOWNLOADING) activeSub(song) else null,
                        onClick = if (phase == DownloadPhase.DONE) ({ actions.play(songs, i, DOWNLOADED_SONGS) }) else null,
                    ) {
                        when (section) {
                            DownloadPhase.FAILED -> {
                                IconButton({ actions.retryDownloads(listOf(song)) }, Modifier.size(40.dp)) { Icon(Icons.Filled.Refresh, say.retry, Modifier.size(20.dp), MaterialTheme.colorScheme.primary) }
                                IconButton({ actions.cancelDownloads(listOf(song)) }, Modifier.size(40.dp)) { Icon(Icons.Filled.Close, say.remove, Modifier.size(19.dp), MaterialTheme.colorScheme.onSurfaceVariant) }
                            }
                            // The ring hands over to the downloaded icon as the song moves to the finished ones.
                            else -> Crossfade(section == DownloadPhase.DONE, animationSpec = if (plain) snap() else tween(200), label = "trailing") { done ->
                                if (done) Box(Modifier.size(40.dp), Alignment.Center) { Icon(Icons.Filled.DownloadDone, say.downloaded, Modifier.size(MARK), MaterialTheme.colorScheme.onSurfaceVariant) }
                                else StopControl(song, plain, stoppable = !phase.processing) { actions.cancelDownloads(listOf(song)) }
                            }
                        }
                    }
                }
            }
            // A row is keyed by its song alone (so it moves between sections), so a song may be shown once:
            // in the first section that lists it. A song still processing can be listed as downloading and as
            // finished for a moment, and a key used twice brings the screen down.
            val shown = HashSet<String>()
            val once = { list: List<Song> -> list.filter { shown.add(it.id) } }
            val active = once(s.active)
            val queued = once(s.queued)
            val failed = once(s.failed)
            val finished = once(s.finished)
            section("active", say.downloading, active, DownloadPhase.DOWNLOADING)
            section("queued", say.waiting, queued, DownloadPhase.QUEUED)
            section(
                "failed", say.failed, failed, DownloadPhase.FAILED,
                action = { Text(say.retryAll, Modifier.clip8().clickable { actions.retryDownloads(s.failed) }.padding(8.dp), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary) },
            )
            section("finished", say.finished, finished, DownloadPhase.DONE)
            if (s.active.isEmpty() && s.queued.isEmpty() && s.failed.isEmpty() && s.finished.isEmpty()) item(key = "empty", contentType = "empty") {
                Column(
                    Modifier.fillMaxWidth().then(moving(plain)).padding(horizontal = Space.gutter * 2, vertical = 96.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Icon(Icons.Outlined.Downloading, null, Modifier.size(44.dp), MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.5f))
                    Text(noteText(Note.NO_DOWNLOADS), Modifier.padding(top = 14.dp), style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold))
                    Text(
                        noteText(Note.NO_DOWNLOADS_HELP),
                        Modifier.padding(top = 6.dp), textAlign = TextAlign.Center,
                        style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

/**
 * An active row's second line: the artist, then where its song stands ("45% · 2.1 MB/s · 1:20 left"),
 * from the core's facts. It is asked again whenever the ring's progress moves, so the line and the ring
 * never disagree.
 */
@Composable
private fun activeSub(song: Song): String {
    val all = LocalDownloadMarks.current
    val progress = all?.marks?.value?.get(song.id)
        ?.takeIf { it.phase == DownloadPhase.DOWNLOADING }?.progress?.collectAsStateWithLifecycle()?.value
    return progress.let { _ -> DownloadLines.row(LocalContext.current.resources, song.id) }
}

/** Stopping every download: whether it asks first (more than one song would leave the queue), and how. */
private class StopAll(val asks: Boolean, val title: String, val text: String)

/** A song on the downloads screen: the same proportions as a row in any song list. */
@Composable
private fun DownloadRow(
    song: Song, coverUrl: String?, phase: DownloadPhase, divider: Boolean, plain: Boolean, modifier: Modifier = Modifier,
    sub: String? = null,
    onClick: (() -> Unit)? = null, trailing: @Composable RowScope.() -> Unit,
) {
    val scheme = MaterialTheme.colorScheme
    val couldNot = noteText(Note.DOWNLOAD_FAILED)
    Column(modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
                .padding(start = Space.gutter, end = 6.dp, top = 9.dp, bottom = 9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Cover(coverUrl, 46.dp, radius = 6.dp)
            Column(Modifier.weight(1f).padding(start = 12.dp, end = 8.dp)) {
                Text(song.title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyLarge, color = scheme.onSurface)
                // The second line says what went wrong when something did, what a saved song is still
                // waiting for, how an arrival is doing while it is, otherwise the artist.
                Crossfade(phase.takeIf { it == DownloadPhase.FAILED || it.processing }, animationSpec = if (plain) snap() else tween(200), label = "subtitle") { said ->
                    Text(
                        when (said) {
                            DownloadPhase.FAILED -> couldNot
                            DownloadPhase.FINDING_LYRICS -> say.findingLyrics
                            DownloadPhase.ANALYSING -> say.analysing
                            DownloadPhase.DETECTING_BEATS -> say.detectingBeats
                            else -> sub ?: song.artist
                        },
                        maxLines = 1, overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant,
                    )
                }
            }
            trailing()
        }
        if (divider) Hairline(startIndent = Space.gutter + 58.dp)
    }
}

/**
 * Apple's stop control: the download's own ring with a small square in it, which is the button. The
 * ring is the song's mark, so a song moving from waiting to downloading starts filling the ring it
 * already had.
 */
@Composable
private fun StopControl(song: Song, plain: Boolean, stoppable: Boolean, onStop: () -> Unit) {
    val all = LocalDownloadMarks.current
    val mark = all?.marks?.value?.get(song.id)
    val scheme = MaterialTheme.colorScheme
    // A saved song being processed has nothing to stop: its ring turns and the square fades away.
    val square by animateFloatAsState(if (stoppable) 1f else 0f, if (plain) snap() else tween(200), label = "stop")
    Box(
        Modifier.size(40.dp).clip8().clickable(stoppable, onClickLabel = say.stopDownload, onClick = onStop),
        Alignment.Center,
    ) {
        DownloadRing(if (stoppable) mark?.takeIf { it.phase == DownloadPhase.DOWNLOADING }?.progress else TURNING, 24.dp, 1.5.dp, scheme.onSurfaceVariant.copy(alpha = 0.3f), scheme.primary, plain)
        Box(Modifier.size(7.dp).graphicsLayer { alpha = square }.background(scheme.primary, RoundedCornerShape(1.5.dp)))
    }
}

/** A ring with no size to fill against: the short turning arc. */
private val TURNING = kotlinx.coroutines.flow.MutableStateFlow(-1f)

private fun Modifier.clip8() = clip(RoundedCornerShape(8.dp))

/** Rows slide to their new place and fade in and out; with reduced motion they simply are where they are. */
private fun LazyItemScope.moving(plain: Boolean): Modifier = if (plain) Modifier.animateItem(null, null, null) else Modifier.animateItem(
    fadeInSpec = tween(220), placementSpec = tween(260, easing = FastOutSlowInEasing), fadeOutSpec = tween(160),
)

/**
 * Download pressed with "Better beat detection" on and "ML beats for downloads" at Ask: whether the beat model
 * also reads these songs once saved, and whether to remember the answer (the setting becomes Always or Never).
 * Put away without an answer, nothing is downloaded.
 */
@Composable
fun BeatsQuestion(actions: ActionsViewModel) {
    val ask by actions.beatsAsk.collectAsStateWithLifecycle()
    var keep by remember { mutableStateOf(false) }
    NoriDialog(ask, { actions.dismissBeats(); keep = false }) {
        AlertCard(
            title = { Text(say.beatsAskTitle) },
            text = {
                Column {
                    Text(say.beatsAskText)
                    Row(
                        Modifier.padding(top = 12.dp).clip8().clickable { keep = !keep }.padding(end = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        androidx.compose.material3.Checkbox(keep, { keep = it })
                        Text(say.beatsAskRemember)
                    }
                }
            },
            confirmButton = { TextButton({ actions.answerBeats(true, keep); keep = false }) { Text(say.beatsAskYes) } },
            dismissButton = { TextButton({ actions.answerBeats(false, keep); keep = false }) { Text(say.beatsAskNo) } },
        )
    }
}
