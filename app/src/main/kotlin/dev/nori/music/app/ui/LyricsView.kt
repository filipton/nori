package dev.nori.music.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.gestures.animateScrollBy
import kotlinx.coroutines.withContext
import kotlinx.coroutines.launch
import kotlinx.coroutines.coroutineScope
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.animation.core.Animatable
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.ClipOp
import androidx.compose.ui.graphics.ShaderBrush
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.text.style.ResolvedTextDirection
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.runtime.mutableFloatStateOf
import dev.nori.music.ffi.model.LyricWord
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.StarBorder
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.Load
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.look.CoverLook
import androidx.compose.ui.graphics.ColorProducer
import dev.nori.music.look.LyricsClock
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive

private fun ease(t: Float): Float = t.coerceIn(0f, 1f).let { it * it * (3f - 2f * it) }

/**
 * How far a sung word or syllable stands above its line, 0..1: it rises as it is sung (over at least the
 * core's `lyricsRiseMinMs`, so a quick syllable does not jump) and settles back over `lyricsSettleMs` once
 * it is done, so a finished line is flat again and nothing drops when the next line takes over.
 */
private fun lift(w: LyricWord, ms: Long): Float {
    if (ms <= w.startMs) return 0f
    val up = ease((ms - w.startMs).toFloat() / maxOf(w.endMs - w.startMs, stage.lyricsRiseMinMs))
    val down = if (ms <= w.endMs) 0f else ease((ms - w.endMs).toFloat() / stage.lyricsSettleMs)
    return up * (1f - down)
}

/** How much a held note glows, 0..1: only one held for `lyricsHeldMs` or more, gathering while it is held and fading after. */
private fun glow(w: LyricWord, ms: Long): Float {
    val held = w.endMs - w.startMs
    if (held < stage.lyricsHeldMs || ms <= w.startMs) return 0f
    return if (ms < w.endMs) ease((ms - w.startMs).toFloat() / held) else 1f - ease((ms - w.endMs).toFloat() / stage.lyricsGlowFadeMs)
}

@Composable
fun LyricsView(vm: PlayerViewModel, actions: ActionsViewModel, playing: Boolean) {
    val loaded by vm.lyrics.collectAsStateWithLifecycle()
    val playerState by vm.state.collectAsStateWithLifecycle()
    // Only the words of the song on the page: an answer for any other song (the one before, handed over
    // as the panel opens, or still on its way out as the song changes) is the loader until the right
    // one comes.
    val load = loaded.of(playerState.current?.id, Load.Loading)
    val settings: SettingsViewModel = viewModel()
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    val shown = LocalPlayerShown.current
    // When the screen stays on is the core's (`lyrics_keep_screen_on`).
    if (prefs.lyricsKeepScreenOn && shown) {
        val view = LocalView.current
        DisposableEffect(playing) { view.keepScreenOn = dev.nori.music.ffi.lyricsKeepScreenOn(true, true, playing); onDispose { view.keepScreenOn = false } }
    }
    Column(Modifier.fillMaxSize()) {
        // On its side the title, the heart and the menu stay in the controls' half, where they are in every
        // panel; a second copy here pushed the words down and moved the controls up when the lyrics opened.
        if (!LocalWide.current) LyricsHeader(vm, actions, playerState.current)
        // Loading, nothing found, or the words - each fades into the next rather than replacing it, the
        // words included: they rise out of the loader instead of appearing in one frame. A new song goes
        // back through the loader, so the last song's lyrics never sit on screen under the new title.
        val found = (load as? Load.Ready)?.data?.takeIf { it.lyrics.lines.isNotEmpty() }
        // The perf build's watch hears which song's lyrics went up; no other build has a recorder.
        dev.nori.music.app.PerfHooks.recorder?.let { r -> LaunchedEffect(found) { if (found != null) loaded.songId?.let(r::lyricsShown) } }
        val phase: Any = found?.let { Words(loaded.songId, it) } ?: if (load is Load.Loading) LyricsPhase.LOADING else LyricsPhase.NONE
        // Where the words on screen are, so finer words for the same song take their place line for line.
        val anchor = remember { LyricsAnchor() }
        FrameCrossfade(
            phase, LYRICS_FADE, Modifier.fillMaxSize().weight(1f),
            // The lyrics race put finer words up for the same song (timed by the word where they were
            // timed by the line): the same page, so they cross-fade in place, starting where the words
            // shown were, rather than going out and rising in from below as a new song's do.
            timesFor = { from, to -> if (from is Words && to is Words && from.song == to.song) LYRICS_SWAP else null },
        ) { p ->
            when (p) {
                // The words on their way out (a new song's, or its loader, took over) stay as they were
                // left: the playhead is the next song's now, and following it jumped them back to the
                // top in one frame while they faded.
                is Words -> LyricsBody(vm, p.found, p.song, playing, following = p == phase, anchor)
                // In the middle, where "No lyrics" would be: it is waiting, not a line of words yet.
                // The dots only come for a wait long enough to see: lyrics already kept arrive well
                // inside it, and a song change then goes from one song's words straight to the next's
                // instead of flashing the loader between them. They fade in, never appear.
                LyricsPhase.LOADING -> Box(Modifier.fillMaxSize(), Alignment.Center) {
                    // Counted in frames like the fades around it, so a slow frame cannot skip it.
                    var seen by remember { mutableFloatStateOf(0f) }
                    LaunchedEffect(Unit) {
                        delay(LOADER_AFTER_MS)
                        var last = withFrameNanos { it }
                        while (seen < 1f) {
                            val now = withFrameNanos { it }
                            seen = if (AppMotion.reduce) 1f else (seen + ((now - last) / 1e6f).coerceIn(0f, FADE_MAX_STEP_MS) / LOADER_FADE_MS).coerceAtMost(1f)
                            last = now
                        }
                    }
                    LoadingDots(Modifier.graphicsLayer { alpha = fadeEase(seen) }, dot = 9.dp)
                }
                else -> Box(Modifier.fillMaxSize(), Alignment.Center) {
                    val look = LocalLook.current
                    LookText(noteText(Note.NO_LYRICS), { look.color(CoverLook.ON_VARIANT) }, style = androidx.compose.material3.LocalTextStyle.current)
                }
            }
        }
    }
}

private enum class LyricsPhase { LOADING, NONE }

/** Lyrics to show and the song they are for: another song's words are another page. */
private data class Words(val song: String?, val found: dev.nori.music.data.FoundLyrics)

/**
 * The words being followed on screen: their song, lines, list and lit line, kept by the body that follows
 * them. Finer words for the same song start from it, so the line on screen stays where it is.
 */
private class LyricsAnchor {
    var song: String? = null
    var lyrics: dev.nori.music.ffi.model.Lyrics? = null
    var list: androidx.compose.foundation.lazy.LazyListState? = null
    /**
     * The lit line's state itself, read when finer words come. A copy written by the body's SideEffect went
     * stale: the body does not recompose as the line changes (only its list and effects read it), so the
     * anchor kept the line lit when the words first went up, -1 for a song's first words.
     */
    var lit: androidx.compose.runtime.IntState? = null
    private val active: Int get() = lit?.intValue ?: -1

    /**
     * Where [lyrics] for [song] should start so that the line shown is where it was: the new line matching
     * the lit one (or the first on screen, when the lit one is scrolled away) and its place in pixels; null
     * when there is nothing to carry over (another song, nothing shown yet).
     */
    fun carry(song: String?, lyrics: dev.nori.music.ffi.model.Lyrics): Pair<Int, Int>? {
        val old = this.lyrics ?: return null
        val list = this.list ?: return null
        if (song == null || song != this.song || old === lyrics || lyrics.lines.isEmpty()) return null
        val seen = list.layoutInfo.visibleItemsInfo
        val ref = seen.firstOrNull { it.index == active } ?: seen.firstOrNull() ?: return null
        return matching(old, ref.index, lyrics) to ref.offset
    }

    /**
     * The line of [lyrics] for [song] standing for the one lit in the words followed, for their clock to
     * start on (`LyricsClock.land`); null when there is nothing to carry over (another song, nothing lit).
     */
    fun litLine(song: String?, lyrics: dev.nori.music.ffi.model.Lyrics): Int? {
        val old = this.lyrics ?: return null
        if (song == null || song != this.song || old === lyrics || lyrics.lines.isEmpty() || active !in old.lines.indices) return null
        return matching(old, active, lyrics)
    }

    /** Which new line stands for line [at] of [old] is the core's (nori_look::lyrics::matching_line). */
    private fun matching(old: dev.nori.music.ffi.model.Lyrics, at: Int, lyrics: dev.nori.music.ffi.model.Lyrics): Int {
        val starts = LongArray(old.lines.size) { old.lines[it].startMs }
        val next = LongArray(lyrics.lines.size) { lyrics.lines[it].startMs }
        return dev.nori.music.look.LyricsClock.matchingLine(starts, at, next, old.synced && lyrics.synced)
    }
}

/** A display frame as the core counts them for the sweep (a sixtieth of a second), and the leeway a frame's timing is given. */
private const val CORE_FRAME_NS = 16_666_667L
private const val FRAME_SLACK_NS = 2_000_000L

/** How long lyrics may take before the loader starts to show, and how long it then takes to fade in. */
private const val LOADER_AFTER_MS = 350L
private const val LOADER_FADE_MS = 300f

/** Loader, words and "No lyrics" into each other: the words leaving fade where they stand, the new ones rise in. */
private val LYRICS_FADE = FadeTimes(inMs = 380f, outMs = 200f, delayMs = 80f, riseMs = 420f)

/** Finer words for the same song over the ones shown: a plain cross-fade in place, the two together throughout. */
private val LYRICS_SWAP = FadeTimes(inMs = 320f, outMs = 320f)

/** The header's title and thumbnail, from one song to the next, as the player page's title changes. */
private val HEADER_FADE = FadeTimes(inMs = 220f, outMs = 160f)


/** The words of one song, in time with it. */
@Composable
private fun LyricsBody(vm: PlayerViewModel, found: dev.nori.music.data.FoundLyrics, song: String?, playing: Boolean, following: Boolean, anchor: LyricsAnchor) {
    val lyrics = found.lyrics
    val settings: SettingsViewModel = viewModel()
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    val shown = LocalPlayerShown.current
    // Which line is lit, how long its change takes, how far the singing is and when to look again are all
    // the core's (crates/look/src/lyrics.rs); this only asks with the playhead and draws the answer.
    // Finer words for the same song (or the same ones timed again) start on the line that was lit, not a
    // line back while the song catches up with their timing (see LyricsAnchor.litLine).
    val clock = remember(lyrics) { LyricsClock(lyrics, vm.positionMs).also { c -> anchor.litLine(song, lyrics)?.let(c::land) } }
    DisposableEffect(clock) { onDispose { clock.close() } }
    var nudgeMs by remember(clock) { mutableLongStateOf(0L) }
    var resumed by remember { mutableStateOf(false) }
    LifecycleResumeEffect(Unit) { resumed = true; onPauseOrDispose { resumed = false } }
    // Put away with the player is the same as paused: nothing to keep in step.
    val live = resumed && shown
    // The active line fills in word by word only when the listener wants it and the lyrics carry per-word times.
    val sweep = prefs.lyricsSweep && clock.sweeps
    // What is on screen, as the clock packed it: the line lit, its change and how far it is sung. Written
    // only when that changes; only the draw phase of the active line reads the sung part.
    var frame by remember(clock) { mutableLongStateOf(clock.shown()) }
    // The line lit and how long its change takes, written only when they change. Derived from [frame]
    // instead, every frame of a sweep made the composition check them again - a recomposition pass per
    // frame that found nothing to do, and on a phone the biggest part of the main thread's time.
    val lit = remember(clock) { mutableIntStateOf(LyricsClock.active(frame)) }
    val glide = remember(clock) { mutableIntStateOf(LyricsClock.glideMs(frame)) }
    val show: (Long) -> Unit = remember(clock) {
        { f ->
            frame = f
            LyricsClock.active(f).let { if (it != lit.intValue) lit.intValue = it }
            LyricsClock.glideMs(f).let { if (it != glide.intValue) glide.intValue = it }
        }
    }
    // Keyed on the clock, so on the next song this loop times the new lines, not the old. One call per
    // look: the clock says what to draw, whether it changed, and how long to sleep - a number of display
    // frames while sweeping, otherwise until the next line takes over; never, for words that are not timed.
    // The moment on screen and how far the lit line's backing vocals are sung, written with the frame:
    // the words' rise and glow are drawn for the moment, which moves while the fill stands still.
    var shownMs by remember(clock) { mutableLongStateOf(clock.shownMs()) }
    // The same moment outside the snapshot system: a line looks at it while drawing to see whether any of
    // its words still move, and only then reads [shownMs] and is redrawn as it changes.
    val lastMs = remember(clock) { longArrayOf(clock.shownMs()) }
    var backingSung by remember(clock) { mutableFloatStateOf(0f) }
    val hasBacking = remember(lyrics) { lyrics.lines.any { it.backing.isNotEmpty() } }
    // Words rise and glow as they are sung unless movement is reduced; then there is only the fill.
    val lively = !reduceMotion()
    LaunchedEffect(playing, live, sweep, lively, clock, following) {
        if (!following) return@LaunchedEffect
        // Only this song's playhead: the player moves on a frame before the page does, and the next
        // song's first second read here put these words back before their first line - scrolled to
        // the top in one frame as they began to fade out.
        var step = clock.at(vm.positionIn(song) ?: return@LaunchedEffect, sweep, lively, force = true)
        if (dev.nori.music.app.traceLyrics) android.util.Log.d("norilyrics", "${android.os.SystemClock.uptimeMillis()} start shown=${clock.shownMs()} lit=${LyricsClock.active(step)} words=${lyrics.wordTimed}")
        show(LyricsClock.frame(step))
        shownMs = clock.shownMs()
        lastMs[0] = shownMs
        if (hasBacking) backingSung = clock.backingSung()
        var drawnAt = 0L
        while (playing && live && isActive) {
            val wait = LyricsClock.wait(step)
            if (wait == 0) break
            // Display frames while the fill moves; asleep between words and after a line is sung, when
            // nothing would be drawn anyway.
            // Counted in time as well as in frames: a frame is a sixtieth of a second to the core, and on a
            // 120 Hz screen counting display frames drew the sweep twice as often as it asked for - every
            // frame of the whole screen, the page's blur included, for as long as a line was sung.
            if (sweep && !LyricsClock.still(step)) {
                var now = withFrameNanos { it }
                while (now - drawnAt < wait * CORE_FRAME_NS - FRAME_SLACK_NS) now = withFrameNanos { it }
                drawnAt = now
            } else { delay(wait.toLong()); drawnAt = 0L }
            val read = vm.positionIn(song) ?: break
            step = clock.at(read, sweep, lively, force = false)
            if (dev.nori.music.app.traceLyrics) android.util.Log.d("norilyrics", "${android.os.SystemClock.uptimeMillis()} read=$read shown=${clock.shownMs()} mixing=${vm.mixing.value} player=${vm.playerPositionMs} lit=${LyricsClock.active(step)} words=${lyrics.wordTimed}")
            if (LyricsClock.redraw(step)) {
                show(LyricsClock.frame(step))
                shownMs = clock.shownMs()
                lastMs[0] = shownMs
                if (hasBacking) backingSung = clock.backingSung()
            }
        }
    }
    val active by lit
    // How long the change into the current line takes; the scroll and every line's fade use the same.
    val glideMs by glide

    val style = when (prefs.lyricsSize) { 0 -> MaterialTheme.typography.titleMedium; 2 -> MaterialTheme.typography.headlineMedium; else -> MaterialTheme.typography.headlineSmall }
    // The page's colours, read while drawing: the player's page cross-fades under the words, and that
    // redraws them rather than recomposing the lyrics on every frame of it.
    val look = LocalLook.current
    val bright = remember(look) { ColorProducer { look.color(CoverLook.ON) } }
    val dim = remember(look) { ColorProducer { look.color(CoverLook.ON_35) } }
    val moment = remember(clock) { { shownMs } }
    val peek = remember(clock) { { lastMs[0] } }
    val translated = remember(look) { ColorProducer { look.color(CoverLook.ON_80) } }
    val accent = remember(look) { ColorProducer { look.color(CoverLook.ACCENT) } }
    // Finer words for the same song start with the line that was on screen where it was (see LyricsAnchor):
    // asked for before the list is first measured, so its first frame is already there.
    val list = remember {
        androidx.compose.foundation.lazy.LazyListState().also { l -> anchor.carry(song, lyrics)?.let { (i, y) -> l.requestScrollToItem(i, -y) } }
    }
    // The words being followed are the anchor for any finer ones that come for the song.
    if (following) androidx.compose.runtime.SideEffect { anchor.song = song; anchor.lyrics = lyrics; anchor.list = list; anchor.lit = lit }
    // Whether the song is a duet at all: only then does either side keep a lane clear.
    val duet = remember(lyrics) { lyrics.lines.any { it.voice.toInt() == 1 } }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val third = with(LocalDensity.current) { (maxHeight / 3).roundToPx() }
        // The line being sung rests a third of the way down, and the list glides there. It used to call
        // animateScrollToItem, whose default spring is so stiff that over one line's distance it is
        // done in two or three frames - on screen, a teleport. This is a measured ease instead, slow
        // enough to follow with the eye, the way Apple's lyrics move. A line that is not on screen at
        // all (a seek, the first line after opening) is still jumped to: gliding past a whole song's
        // worth of words is not a transition, it is a wait.
        val plain = reduceMotion()
        // Reading ahead, or back, is allowed: while a finger is on the words nothing scrolls them, and
        // for a few seconds after it lifts they stay where they were put. Then the lyrics come back to
        // the line being sung - gliding, not jumping, because this one is a return to the song rather
        // than a seek, and a jump here would look like the list had been snatched away.
        val dragged by list.interactionSource.collectIsDraggedAsState()
        var follow by remember(lyrics) { mutableStateOf(true) }
        var returning by remember(lyrics) { mutableStateOf(false) }
        LaunchedEffect(dragged) {
            if (dragged) { follow = false; returning = false } else if (!follow) {
                delay(stage.lyricsReadingMs)
                returning = true
                follow = true
            }
        }
        // Only the scroll lives here; each line's brightness is its own (below). A scroll cut short by the
        // next line is simply continued from wherever the list is, so it cannot jump either.
        LaunchedEffect(active, plain, lyrics, follow) {
            if (!follow) return@LaunchedEffect
            // Before the first line (a new song, an intro), or lyrics that are not timed: back to the top.
            // The list outlives a song, so without this the next song opened where the last one ended.
            if (active < 0) { if (list.firstVisibleItemIndex != 0 || list.firstVisibleItemScrollOffset != 0) list.scrollToItem(0); return@LaunchedEffect }
            // Once the last line is over the clock lights none (`active` is one past it): the page stays on
            // that line while it dims with the rest.
            val line = minOf(active, lyrics.lines.size - 1)
            // Not measured yet (the words have only just come): look after the first measure, which is
            // where finer words for the same song were put, rather than jumping from nowhere.
            if (list.layoutInfo.visibleItemsInfo.isEmpty()) withFrameNanos { }
            val here = list.layoutInfo.visibleItemsInfo.firstOrNull { it.index == line }
            if (returning) {
                returning = false
                if (plain) list.scrollToItem(line, -third) else list.animateScrollToItem(line, -third)
                return@LaunchedEffect
            }
            if (plain || here == null) { list.scrollToItem(line, -third); return@LaunchedEffect }
            // scrollToItem(active, -third) would leave the line at offset `third`; glide by the difference.
            val distance = (here.offset - third).toFloat()
            if (kotlin.math.abs(distance) < 1f) return@LaunchedEffect
            list.animateScrollBy(distance, androidx.compose.animation.core.tween(glideMs, easing = LyricEase))
        }
        // The words fade out towards both ends of the panel by becoming transparent, not by having a
        // colour painted over them. The page behind is the cover's blur and varies across the width;
        // any colour laid on top to hide the words ends on one flat value, and where that flat value
        // meets the blur below it there is a dead straight line - measured on a phone at the exact
        // bottom edge of this panel. A mask paints nothing, so there is nothing to meet anything.
        LazyColumn(
            state = list,
            contentPadding = PaddingValues(start = 20.dp, end = 20.dp, top = 8.dp, bottom = maxHeight / 2),
            modifier = Modifier
                .graphicsLayer { compositingStrategy = androidx.compose.ui.graphics.CompositingStrategy.Offscreen }
                .drawWithCache {
                  // Gone by the source label at the bottom, so the two never sit on each other. The
                  // stops are the core's (`nori_look::sleeve::LYRICS_MASK`); one brush per size, which a
                  // scroll does not change.
                  val mask = alphaGradient(stage.lyricsMask, Color.Black, 0f, size.height)
                  val at = androidx.compose.ui.geometry.Offset(-1f, -1f)
                  val area = androidx.compose.ui.geometry.Size(size.width + 2f, size.height + 2f)
                  onDrawWithContent {
                    drawContent()
                    // Drawn a pixel beyond the panel on every side, with the gradient still anchored to
                    // the panel's own height. The layer is clipped to whole pixels and the mask was not,
                    // so the last fractional row came through unmasked: a hairline of the tops of the
                    // letters below, between the source label and the seek bar, which is what was
                    // peeking out of the "1 px gap". Past the last stop the brush stays transparent, so
                    // the extra row erases rather than paints.
                    drawRect(mask, topLeft = at, size = area, blendMode = androidx.compose.ui.graphics.BlendMode.DstIn)
                  }
                },
        ) {
            itemsIndexed(lyrics.lines, key = { i, _ -> i }) { i, line ->
                // In a duet the other voice sings from the right, and each side leaves a lane clear on the
                // far side, so two singers read as a conversation rather than as one column.
                val right = line.voice.toInt() == 1
                val align = if (right) TextAlign.End else TextAlign.Start
                Column(
                    Modifier.fillMaxWidth().clickable(enabled = lyrics.synced) { vm.seekTo(clock.tap(i)); show(clock.shown()); shownMs = clock.shownMs() }
                        .padding(vertical = 8.dp)
                        .padding(start = if (duet && right) DUET_LANE else 0.dp, end = if (duet && !right) DUET_LANE else 0.dp),
                    horizontalAlignment = if (right) Alignment.End else Alignment.Start,
                ) {
                    val weight = if (line.background) FontWeight.Normal else FontWeight.SemiBold
                    // Every line owns its brightness and always moves it *from wherever it is now*
                    // towards what it should be. An earlier version drove all lines from one shared
                    // old-line/new-line blend, and when the next line arrived before a blend had
                    // finished it restarted from a line two changes back - one frame of the wrong
                    // line fully lit. Nothing here can jump: a change mid-way just turns it round.
                    // How lit each line is, by where the singing is, is the core's; asked over JNI when the
                    // line being sung changes, not per frame.
                    val target = remember(lyrics.synced, i, active) { LyricsClock.strength(lyrics.synced, i, active) }
                    val strength = remember { Animatable(target) }
                    LaunchedEffect(target, plain) {
                        if (plain) strength.snapTo(target)
                        else strength.animateTo(target, androidx.compose.animation.core.tween(glideMs, easing = LyricEase))
                    }
                    // Read in the draw phase: a fading line redraws, it does not recompose.
                    val level = remember(strength) { { strength.value } }
                    // How far the line and its backing vocals were last drawn sung, kept once it is no longer
                    // the line sung: a finished line dims from exactly how it was left. Filling the rest of it
                    // in first was the one-frame jump of a nearly sung line to fully lit.
                    val held = remember(lyrics) { FloatArray(2) }
                    val sungNow = sweep && i == active
                    // A line left before its last word was done (the next one takes over half a change
                    // early) fills the rest in over that change rather than staying part lit as it dims:
                    // a line that has been sung is sung to its end, and never in one frame.
                    val rest = remember(lyrics) { Array(2) { Animatable(-1f) } }
                    LaunchedEffect(sungNow, lyrics) {
                        if (sungNow) { rest.forEach { it.snapTo(-1f) }; return@LaunchedEffect }
                        if (!sweep) return@LaunchedEffect
                        coroutineScope {
                            for ((k, full) in listOf(line.text.length.toFloat(), line.backing.length.toFloat()).withIndex()) {
                                val from = held[k]
                                if (from <= 0f || from >= full) continue
                                launch {
                                    rest[k].snapTo(from)
                                    if (plain) rest[k].snapTo(full) else rest[k].animateTo(full, androidx.compose.animation.core.tween(glideMs, easing = LyricEase))
                                    held[k] = full
                                }
                            }
                        }
                    }
                    when {
                        !lyrics.synced -> LookText(line.text, bright, style = style, textAlign = align)
                        else -> SungText(
                            line.text, if (sweep) line.words else emptyList(), style.copy(fontWeight = weight), bright, align, sweep && lively, level,
                            when {
                                sungNow -> { { LyricsClock.sung(frame).also { held[0] = it } } }
                                sweep -> { { rest[0].value.takeIf { it >= 0f } ?: held[0] } }
                                // Timed by the line only: the line lit is lit whole.
                                else -> WHOLE
                            },
                            moment, peek,
                        )
                    }
                    // Backing vocals sung over the line: smaller, under it, lit as they are sung, the way Apple
                    // sets them. Kept inside the line, so the light never leaves the lead singer.
                    if (line.backing.isNotEmpty()) {
                        val backingStyle = style.copy(fontSize = style.fontSize * BACKING_SIZE, fontWeight = FontWeight.Normal)
                        if (lyrics.synced) {
                            SungText(
                                line.backing, if (sweep) line.backingWords else emptyList(), backingStyle, translated, align, sweep && lively, level,
                                when {
                                    sungNow -> { { backingSung.also { held[1] = it } } }
                                    sweep -> { { rest[1].value.takeIf { it >= 0f } ?: held[1] } }
                                    else -> WHOLE
                                },
                                moment, peek, Modifier.padding(top = 2.dp),
                            )
                        } else {
                            LookText(line.backing, translated, style = backingStyle, textAlign = align, modifier = Modifier.padding(top = 2.dp))
                        }
                    }
                    // The translation follows its line's fade rather than switching on the moment the line
                    // is reached - otherwise it lit up a frame ahead of the words above it.
                    if (prefs.lyricsTranslation) line.translation?.let {
                        LookText(it, translated, style = MaterialTheme.typography.bodyMedium, textAlign = align, modifier = Modifier.graphicsLayer { alpha = strength.value })
                    }
                }
            }
        }
        // Where the words came from, and how to nudge them. Nudging is for the handful of songs whose
        // timings are wrong, so it is not worth a permanent bar across the bottom of the lyrics: the
        // corner carries where they came from, and tapping it opens the two buttons. It closes again
        // on the next tap, and stays open while the offset is not zero so the number can be read.
        var tuning by remember(lyrics) { mutableStateOf(false) }
        // Whose words these are and whether they are timed, in the core's words; None: no corner at all.
        val credit = remember(found.source, lyrics.synced) { say.lyricsCredit(found.source, lyrics.synced) }
        val open = tuning || nudgeMs != 0L
        // With the corner turned off (Settings, Lyrics: "Source and timing") there is none at all, neither
        // where the words came from nor the nudges. A nudge already made keeps it, so it can still be put back.
        val nudges = prefs.lyricsTimingButton || nudgeMs != 0L
        val shown = if (nudges) credit else null
        if (shown != null) androidx.compose.material3.Surface(
            onClick = { if (lyrics.synced && nudges) tuning = !tuning },
            enabled = lyrics.synced && nudges,
            shape = PillShape,
            // The open pill's plate is drawn rather than composed, so the page changing colour under it
            // only redraws it.
            color = Color.Transparent,
            modifier = Modifier.align(if (open) Alignment.BottomCenter else Alignment.BottomStart).padding(start = 12.dp, bottom = 6.dp),
        ) {
            Row(
                Modifier.then(if (open) Modifier.drawBehind { drawRect(look.color(CoverLook.VEIL_10)) } else Modifier)
                    .padding(horizontal = 6.dp, vertical = 2.dp),
                Arrangement.spacedBy(2.dp), Alignment.CenterVertically,
            ) {
                // Closed, this is the one thing worth saying: whose words these are - and, when they
                // came without timings, that they did. Unsung words are all one brightness and a tap on
                // one goes nowhere, which looks broken unless the corner says why.
                LookText(
                    shown,
                    dim, Modifier.padding(horizontal = 8.dp), style = MaterialTheme.typography.labelSmall,
                )
                if (open) {
                    if (nudgeMs != 0L) LookText(remember(nudgeMs) { say.nudge(nudgeMs) }, bright, style = MaterialTheme.typography.labelSmall)
                    TextButton({ nudgeMs = clock.nudge(-1) }) { LookText(say.later, accent, style = MaterialTheme.typography.labelLarge) }
                    TextButton({ nudgeMs = clock.nudge(1) }) { LookText(say.sooner, accent, style = MaterialTheme.typography.labelLarge) }
                    if (nudgeMs != 0L) TextButton({ nudgeMs = clock.nudge(0) }) { LookText(say.reset, accent, style = MaterialTheme.typography.labelLarge) }
                }
            }
        }
    }
}

/**
 * Apple's lyrics header: a small rounded thumbnail with the title beside it, so the words get the
 * whole middle of the screen instead of fighting the title block for it.
 */
@Composable
private fun LyricsHeader(vm: PlayerViewModel, actions: ActionsViewModel, song: dev.nori.music.ffi.model.Song?) {
    val menu = LocalSongMenu.current
    val marks = LocalStarMarks.current
    Row(
        Modifier.fillMaxWidth().padding(start = 20.dp, end = 12.dp, top = 4.dp, bottom = 8.dp),
        Arrangement.spacedBy(12.dp), Alignment.CenterVertically,
    ) {
        // The player is put away from here as well as from the artwork, and the cover has to have
        // somewhere to fly from: this thumbnail is it. Measured only with the sheet fully open, so the
        // rectangle is in the sheet's own coordinates and does not move while the sheet does.
        val sheet = LocalPlayerSheet.current
        Box(
            Modifier.onGloballyPositioned { if (sheet.progress.value >= 0.999f) sheet.panelCover = it.boundsInRoot() }
                // While it is in flight it is the flying copy that is drawn, not this one - whether the
                // flight is the sheet's, out of the now playing bar, or the panel's, out of the sleeve.
                .graphicsLayer { alpha = if (!sheet.panelFlight && (sheet.progress.value >= 1f || sheet.miniCover == Rect.Zero)) 1f else 0f },
        ) {
            // Another album's cover cross-fades in: one already kept in memory was drawn at once, and one
            // still to come left the old picture for an empty plate in one frame.
            FrameCrossfade(song?.coverArt, HEADER_FADE) { art -> Cover(vm.cover(art, CoverSize.ROW), 64.dp, radius = 9.dp) }
        }
        androidx.compose.runtime.DisposableEffect(sheet) { onDispose { sheet.panelCover = androidx.compose.ui.geometry.Rect.Zero } }
        // The title and artist change with the song, cross-faded as the player page's are: swapped in
        // one frame, the new title sat over the last song's words for the frames before they faded.
        FrameCrossfade(song, HEADER_FADE, Modifier.weight(1f), key = { it?.id }) { s ->
            Column(Modifier.fillMaxWidth()) {
                val look = LocalLook.current
                LookText(
                    s?.title ?: "", { look.color(CoverLook.ON) }, Modifier.readable(), style = MaterialTheme.typography.titleMedium,
                    maxLines = 1, softWrap = false, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                )
                LookText(
                    s?.artist ?: "", { look.color(CoverLook.ON_60) }, style = MaterialTheme.typography.bodySmall,
                    maxLines = 1, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                )
            }
        }
        song?.let { s ->
            val starred = marks.effectiveStar(dev.nori.music.data.StarKind.SONG, s.id, s.starred)
            TitleCircle(if (starred) Icons.Filled.Favorite else Icons.Filled.FavoriteBorder, say.favourite, starred) { actions.star(s, !starred) }
            TitleCircle(Icons.Filled.MoreHoriz, say.more, false) { menu(s) }
        }
    }
}

/** A text laid out once, with the outline of each sung piece and its box, so drawing a frame makes nothing new. */
private class Laid(val layout: TextLayoutResult, val pieces: Array<Path>) {
    val boxes: Array<Rect> = Array(pieces.size) { pieces[it].getBounds() }
    private val n = layout.layoutInput.text.length
    /** Each character's row, and where the fill stands at its start and at its end, asked of the layout once, the first time the line is sung. */
    val row by lazy(LazyThreadSafetyMode.NONE) { IntArray(n) { layout.getLineForOffset(it) } }
    val from by lazy(LazyThreadSafetyMode.NONE) { FloatArray(n) { layout.getHorizontalPosition(it, true) } }
    val to by lazy(LazyThreadSafetyMode.NONE) { FloatArray(n) { i -> if (i + 1 < n && row[i + 1] == row[i]) layout.getHorizontalPosition(i + 1, true) else layout.getLineRight(row[i]) } }
    val rtl by lazy(LazyThreadSafetyMode.NONE) { BooleanArray(n) { layout.getParagraphDirection(it) == ResolvedTextDirection.Rtl } }
}

/**
 * The brushes a sung text is drawn with, made once and moved rather than made again: the fill's soft edge
 * (a gradient [FEATHER] wide, slid to where the singing is), a held note's light (a round gradient,
 * scaled to the word) and the glow under the sung words. Each is made again only when its colour changes,
 * which while a line is sung it does not.
 */
private class Brushes(private val feather: Float, private val blur: Float) {
    private val move = android.graphics.Matrix()
    private var edgeColour = 0
    private var edgeRtl = false
    private var edge: android.graphics.LinearGradient? = null
    private var edgeBrush: Brush? = null
    private var lightColour = 0
    private var light: android.graphics.RadialGradient? = null
    private var lightBrush: Brush? = null
    private var glowColour = 0
    private var glow: Shadow? = null
    private var awayColour = 0
    private var awayBy = 0f
    private var glowAway: Shadow? = null

    /** [glow] set [by] pixels to the right of its text: the glow alone, from text drawn that far to the left. */
    fun glowAway(colour: Color, by: Float): Shadow {
        val c = colour.toArgb()
        return glowAway?.takeIf { c == awayColour && by == awayBy }
            ?: Shadow(colour, androidx.compose.ui.geometry.Offset(by, 0f), blur).also { glowAway = it; awayColour = c; awayBy = by }
    }

    /** Bright before [x] and clear after it (the other way round for right-to-left), softly in between. */
    fun edge(colour: Color, x: Float, rtl: Boolean): Brush {
        val c = colour.toArgb()
        val s = edge?.takeIf { c == edgeColour && rtl == edgeRtl } ?: run {
            val clear = c and 0x00FFFFFF
            android.graphics.LinearGradient(-feather / 2f, 0f, feather / 2f, 0f, if (rtl) clear else c, if (rtl) c else clear, android.graphics.Shader.TileMode.CLAMP)
                .also { edge = it; edgeColour = c; edgeRtl = rtl; edgeBrush = ShaderBrush(it) }
        }
        move.setTranslate(x, 0f)
        s.setLocalMatrix(move)
        return edgeBrush ?: ShaderBrush(s)
    }

    /** A soft round light of [colour], [r] across from its middle at ([cx], [cy]). */
    fun light(colour: Color, cx: Float, cy: Float, r: Float): Brush {
        val c = colour.toArgb()
        val s = light?.takeIf { c == lightColour } ?: run {
            android.graphics.RadialGradient(0f, 0f, 1f, c, c and 0x00FFFFFF, android.graphics.Shader.TileMode.CLAMP)
                .also { light = it; lightColour = c; lightBrush = ShaderBrush(it) }
        }
        move.setScale(r, r)
        move.postTranslate(cx, cy)
        s.setLocalMatrix(move)
        return lightBrush ?: ShaderBrush(s)
    }

    /**
     * The sung words' glow: their own shape, blurred, in [colour]. A shadow colour that is not opaque keeps
     * its own alpha, and drawn with the fill's soft edge it follows that edge, so the glow gathers behind
     * the singing rather than running ahead of it.
     */
    fun glow(colour: Color): Shadow {
        val c = colour.toArgb()
        return glow?.takeIf { c == glowColour } ?: Shadow(colour, androidx.compose.ui.geometry.Offset.Zero, blur).also { glow = it; glowColour = c }
    }
}

/**
 * Timed text lit as it is sung - a line, or its backing vocals - the way Apple's lyrics are. [level] is how
 * lit the line is (the core's strength, moving between lines): sung words are that bright and glow softly,
 * unsung ones no brighter than the core's `lyricsUnsung`, so the line being sung is white where it has
 * been sung, a dimmer white ahead, and the other lines dimmer still. As a line stops being sung it keeps
 * its fill and dims as a whole, and the next one brightens the same way: nothing about either changes in
 * one frame. [sung] is how far the singing is (a UTF-16 offset with a fraction; past the end for a line lit
 * whole), with an edge that fades over [FEATHER] instead of cutting through the letter.
 *
 * When [lively], each word or syllable rises a little as it is sung and settles once it is done, and a
 * held note swells slightly and glows while it is held, all at the moment [ms]; [peek] is the same moment
 * read without being watched, so a line whose words are all still does not redraw as the moment moves.
 * Everything is read in the draw phase, so a frame redraws this one text and recomposes nothing. A piece
 * at rest is drawn with the rest of the text; only the ones moving are drawn on their own, clipped to
 * their letters, so a line costs a draw or two more a frame while it is sung and none once it is still.
 */
@Composable
private fun SungText(
    text: String, words: List<LyricWord>, style: TextStyle, bright: ColorProducer, align: TextAlign,
    lively: Boolean, level: () -> Float, sung: () -> Float, ms: () -> Long, peek: () -> Long, modifier: Modifier = Modifier,
) {
    var laid by remember(text, words) { mutableStateOf<Laid?>(null) }
    val density = LocalDensity.current
    val rise = with(density) { RISE.toPx() }
    val spread = with(density) { GLOW_SPREAD.toPx() }
    val blur = with(density) { SUNG_GLOW.toPx() }
    val brushes = remember(density) { Brushes(with(density) { FEATHER.toPx() }, blur) }
    // The pieces moving on this frame, gathered into one outline the rest of the text is drawn around.
    val moving = remember { Path() }
    LookText(
        text, bright, style = style, textAlign = align,
        onTextLayout = { l ->
            val n = text.length
            laid = Laid(l, Array(words.size) { k -> l.getPathForRange(words[k].start.toInt().coerceIn(0, n), words[k].end.toInt().coerceIn(0, n)) })
        },
        modifier = modifier.drawWithContent {
            // Laid out before it is drawn; nothing is drawn in the colour the plain text would have.
            val d = laid ?: return@drawWithContent
            val l = d.layout
            val colour = bright()
            val s = level()
            // Unsung words at the lower of the line's strength and the core's; sung ones drawn over them,
            // as much as it takes for the two together to come to the line's strength.
            val low = minOf(s, stage.lyricsUnsung)
            val over = if (s > low) (s - low) / (1f - low) else 0f
            val base = colour.copy(alpha = colour.alpha * low)
            val lit = colour.copy(alpha = colour.alpha * over)
            // Only a line lit above its unsung words asks how far it is sung.
            val at = if (over > 0f) sung() else 0f
            val shine = if (over > 0f) brushes.glow(colour.copy(alpha = colour.alpha * SUNG_GLOW_ALPHA * over)) else null
            if (!lively || words.isEmpty() || !stirring(words, peek())) {
                drawSung(d, at, base, lit, shine, spread, brushes)
                return@drawWithContent
            }
            val now = ms()
            moving.reset()
            var k = 0
            while (k < words.size) {
                if (lift(words[k], now) > 0f || glow(words[k], now) > 0f) moving.addPath(d.pieces[k])
                k++
            }
            // The glow first, for the whole line and unclipped: the pieces are boxes, not the letters'
            // outlines (getPathForRange gives boxes), and a glow clipped to them stopped in a straight
            // edge round every word that moved. A shadow colour that is not opaque keeps its own alpha,
            // so it is drawn on its own, from text set outside the layer with its shadow set back. Still
            // one blurred draw a frame.
            // The letters after it say Shadow.None, not null: null keeps the text paint's last shadow,
            // which is how the box-clipped copies of the glow came to be drawn at all.
            if (over > 0f) {
                val away = size.width + spread * 4f
                drawSung(d, at, Color.Transparent, lit, brushes.glowAway(colour.copy(alpha = colour.alpha * SUNG_GLOW_ALPHA * over), away), spread, brushes, away)
            }
            clipPath(moving, ClipOp.Difference) { drawSung(d, at, base, lit, Shadow.None, spread, brushes) }
            // Each moving piece on its own, moved as it is sung, clipped to the piece in its own moved
            // space, so it carries its letters and nothing of its neighbours'.
            k = 0
            while (k < words.size) {
                val w = words[k]
                val up = lift(w, now)
                val g = glow(w, now)
                if (up > 0f || g > 0f) {
                    val piece = d.pieces[k]
                    val box = d.boxes[k]
                    withTransform({
                        translate(0f, -up * rise)
                        if (g > 0f) scale(1f + SWELL * g, 1f + SWELL * g, box.center)
                    }) {
                        if (g > 0f) halo(box, g, lit, spread, brushes)
                        clipPath(piece) { drawSung(d, at, base, lit, Shadow.None, spread, brushes) }
                    }
                }
                k++
            }
        },
    )
}

/** Whether any of [words] rises, settles or glows at [ms]. */
private fun stirring(words: List<LyricWord>, ms: Long): Boolean {
    var k = 0
    while (k < words.size) {
        if (lift(words[k], ms) > 0f || glow(words[k], ms) > 0f) return true
        k++
    }
    return false
}

/**
 * [l] in [base], and in [lit] over it as far as [at] (a UTF-16 offset with a fraction), with [glow] under
 * the lit part. The rows above are lit whole; on the row being sung the light fades out over the feather
 * centred on where the singing is, which is what makes a fill look sung rather than wiped. A right-to-left
 * row fades the other way. The clips reach [spread] past the text where nothing else is, so the glow is
 * not cut off at the line's edges.
 */
private fun DrawScope.drawSung(d: Laid, at: Float, base: Color, lit: Color, glow: Shadow?, spread: Float, brushes: Brushes, away: Float = 0f) {
    // [away]: only the glow is wanted. The text is drawn that far to the left, out of the layer, and its
    // shadow is set back by as much (see Brushes.glowAway), so what lands here is the glow and no letters.
    if (away > 0f) {
        withTransform({ translate(-away, 0f) }) { drawSungAt(d, at, Color.Transparent, lit, glow, spread, brushes, away) }
        return
    }
    drawSungAt(d, at, base, lit, glow, spread, brushes, 0f)
}

/** [drawSung] with its clips moved [clipDx] to the right: where a glow set back from its text lands. */
private fun DrawScope.drawSungAt(d: Laid, at: Float, base: Color, lit: Color, glow: Shadow?, spread: Float, brushes: Brushes, clipDx: Float) {
    val l = d.layout
    if (base.alpha > 0f) drawText(l, color = base, shadow = Shadow.None)
    val n = l.layoutInput.text.length
    if (lit.alpha <= 0f || at <= 0f || n == 0) return
    if (at >= n) {
        drawText(l, color = lit, shadow = glow)
        return
    }
    val index = at.toInt().coerceIn(0, n - 1)
    val row = d.row[index]
    val from = d.from[index]
    val to = d.to[index]
    val x = from + (to - from) * (at - index)
    val top = l.getLineTop(row)
    if (row > 0) clipRect(clipDx - spread, -spread, clipDx + size.width + spread, top) { drawText(l, color = lit, shadow = glow) }
    val rtl = d.rtl[index]
    val bottom = if (row == l.lineCount - 1) size.height + spread else l.getLineBottom(row)
    // The edge is made opaque and faded by the draw's alpha, so a line dimming does not make it again.
    clipRect(clipDx - spread, if (row == 0) -spread else top, clipDx + size.width + spread, bottom) {
        drawText(l, brush = brushes.edge(lit.copy(alpha = 1f), x, rtl), alpha = lit.alpha, shadow = glow)
    }
}

/**
 * A held note's light: a soft oval of the text's own colour under the word, wider than it is tall, so the
 * word seems to glow without its letters being cut by the clip that draws them.
 */
private fun DrawScope.halo(box: Rect, g: Float, colour: Color, spread: Float, brushes: Brushes) {
    val r = box.height / 2f + spread / 2f
    val sx = ((box.width + spread * 2f) / (r * 2f)).coerceAtLeast(1f)
    withTransform({ scale(sx, 1f, box.center) }) {
        drawCircle(brushes.light(colour, box.center.x, box.center.y, r), radius = r, center = box.center, alpha = GLOW_ALPHA * g)
    }
}

/** The glow under sung words: how far it blurs, and how strong it is on a line fully lit. */
private val SUNG_GLOW = 7.dp
private const val SUNG_GLOW_ALPHA = 0.5f

/** How far a line timed by the line only is sung once it is lit: all of it. */
private val WHOLE: () -> Float = { Float.MAX_VALUE }

/** How wide the soft edge of the fill is. */
private val FEATHER = 24.dp

/** How far a word rises as it is sung. */
private val RISE = 2.dp

/** How much a held note swells at its brightest. */
private const val SWELL = 0.04f

/** How strong a held note's light is at its middle, and how far past the word it reaches. */
private const val GLOW_ALPHA = 0.22f
private val GLOW_SPREAD = 14.dp

/** Backing vocals, under their line: this much of its size. */
private const val BACKING_SIZE = 0.72f

/** In a duet, the lane each side leaves clear on the far side. */
private val DUET_LANE = 48.dp

/** Ease in and out, soft at both ends, the curve iOS uses for its own scrolling transitions. */
private val LyricEase = androidx.compose.animation.core.CubicBezierEasing(0.25f, 0.1f, 0.25f, 1f)

