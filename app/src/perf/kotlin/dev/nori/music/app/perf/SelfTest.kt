package dev.nori.music.app.perf

import android.app.Application
import android.content.Intent
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTimestamp
import android.os.Build
import android.os.Process
import android.os.SystemClock
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.ViewModelStore
import dev.nori.music.Nori
import dev.nori.music.app.vm.Load
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.data.CoverLoader
import dev.nori.music.ffi.model.PlayQueue
import dev.nori.music.ffi.model.Song
import dev.nori.music.playback.MediaSources
import dev.nori.music.playback.OffloadCalls
import dev.nori.music.playback.PlaybackService
import dev.nori.music.playback.Quiet
import dev.nori.music.playback.Repeat
import dev.nori.music.ffi.settings.StoredPrefs
import dev.nori.music.ffi.model.GainMode
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.coroutines.resume

/**
 * The Performance page's self test: the checks the owner used to make by hand on his phone, made by the
 * app on it, one after another, then everything put back as it was. What runs in which order, how the
 * readings are judged, what is put back and how the result reads are SelfTestLogic.kt's; this drives the
 * player the way the app does (its one PlayerConnection, the settings, the player service stopped and
 * started again) and reads what it needs: the page's state, the song the service arrived on, the
 * AudioTrack's play head and timestamp, and the player's own figures.
 *
 * It plays quietly unless asked to be heard: a player volume far under the music's ([Quiet]), never the
 * phone's. Only songs of the user's library, those on the phone first. Nothing of it exists until the
 * page's button is pressed, and nothing of it runs afterwards.
 */
internal class SelfTest(private val app: Application, private val recorder: Recorder) {
    /** Where a run is, for the page: running or not, the step, the outcomes so far, the report at the end. */
    class Progress(val running: Boolean, val step: Int, val total: Int, val current: String, val outcomes: List<Outcome>, val report: String?)

    var progress by mutableStateOf<Progress?>(null)
        private set
    /** Heard at the music's own volume instead of quietly. */
    var listen by mutableStateOf(false)
    /** Also check that a downloaded song opens. */
    var downloads by mutableStateOf(false)

    private var scope: CoroutineScope? = null
    private var job: Job? = null

    /** The activity on screen while a run is under way: its window keeps the screen on (a flag, no wake lock). */
    private var shown: java.lang.ref.WeakReference<android.app.Activity>? = null

    private fun screenOn(a: android.app.Activity?, on: Boolean) {
        val w = a?.window ?: return
        if (on) w.addFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        else w.clearFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }

    fun start(activity: android.app.Activity?) {
        if (job?.isActive == true) return
        shown = activity?.let { java.lang.ref.WeakReference(it) }
        screenOn(activity, true)
        val s = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate).also { scope = it }
        job = s.launch { Run(listen, downloads).go() }
    }

    fun cancel() {
        job?.cancel()
    }

    /** One run of the suite, with what it keeps between its steps. */
    private inner class Run(private val listen: Boolean, private val withDownloads: Boolean) {
        private val nori = Nori.get(app)
        private val player = nori.player
        private val outcomes = ArrayList<Outcome>()
        private val notes = ArrayList<String>()
        private var snapshot: Snapshot? = null
        private var songsBefore: List<Song> = emptyList()
        private var knobs: Knobs? = null
        private var queue: List<Song> = emptyList()
        private var mp3: List<Song> = emptyList()
        private var aborted = false
        private var started = false
        private var offloadWhyNot: String? = null
        private var offloadEntered = false

        suspend fun go() {
            val steps = plan(Options(downloads = withDownloads))
            val wall0 = System.currentTimeMillis()
            val t0 = SystemClock.elapsedRealtime()
            val breaks0 = dev.nori.music.ffi.perf.perfInvariantBreaks().size
            recorder.testing = true
            app.registerActivityLifecycleCallbacks(away)
            var cancelled = false
            try {
                for ((k, step) in steps.withIndex()) {
                    if (step.id == "restore") break
                    show(k, steps.size, step)
                    outcomes += if (aborted) Outcome(step.section, step.name, Verdict.SKIP, "not run: the setup failed") else run(step)
                }
            } catch (e: CancellationException) {
                cancelled = true
            } finally {
                withContext(NonCancellable + Dispatchers.Main) {
                    val restore = steps.last()
                    show(steps.size - 1, steps.size, restore)
                    outcomes += run(restore)
                    recorder.testing = false
                    recorder.arrivals.clear()
                    app.unregisterActivityLifecycleCallbacks(away)
                    screenOn(this@SelfTest.shown?.get(), false)
                    this@SelfTest.shown = null
                    away.note()?.let { notes += it }
                    val breaks = dev.nori.music.ffi.perf.perfInvariantBreaks().drop(breaks0)
                    val date = SimpleDateFormat("yyyy-MM-dd HH:mm", Locale.ROOT).format(Date(wall0))
                    val text = reportText(outcomes, date, SystemClock.elapsedRealtime() - t0, breaks, notes, cancelled)
                    withContext(Dispatchers.IO) { dev.nori.music.ffi.perf.perfSelftestKeep(System.currentTimeMillis(), text) }
                    progress = Progress(false, steps.size, steps.size, "", outcomes.toList(), text)
                    // For adb as well: `adb logcat -s noriselftest`.
                    text.lines().forEach { android.util.Log.i("noriselftest", it) }
                    scope?.cancel()
                }
            }
        }

        /**
         * The app leaving the screen while the test runs: the run goes on (it is not the page's), but a
         * check made then may read differently, so the report says when it happened.
         */
        private val away = object : android.app.Application.ActivityLifecycleCallbacks {
            private var shown = 1
            private var since = 0L
            private var times = 0
            private var ms = 0L
            override fun onActivityStarted(a: android.app.Activity) {
                if (shown++ == 0 && since > 0) ms += SystemClock.elapsedRealtime() - since
                // Back on screen (or a new activity after a rotation): it keeps the screen on too.
                this@SelfTest.shown = java.lang.ref.WeakReference(a)
                screenOn(a, true)
            }
            override fun onActivityStopped(a: android.app.Activity) { if (--shown == 0) { times++; since = SystemClock.elapsedRealtime() } }
            override fun onActivityCreated(a: android.app.Activity, b: android.os.Bundle?) {}
            override fun onActivityResumed(a: android.app.Activity) {}
            override fun onActivityPaused(a: android.app.Activity) {}
            override fun onActivitySaveInstanceState(a: android.app.Activity, b: android.os.Bundle) {}
            override fun onActivityDestroyed(a: android.app.Activity) {}
            fun note(): String? {
                if (times == 0) return null
                val still = if (shown <= 0) SystemClock.elapsedRealtime() - since else 0L
                return "INTERRUPTED: the app left the screen $times time${if (times == 1) "" else "s"} during the run (${took(ms + still)} in all); checks made then may read differently"
            }
        }

        private fun show(k: Int, total: Int, step: Step) {
            progress = Progress(true, k + 1, total, "${step.section}: ${step.name}", outcomes.toList(), null)
        }

        /** One step, in its time: a failure carries the timeline and the app's log from while it ran. */
        private suspend fun run(step: Step): Outcome {
            val wall = System.currentTimeMillis()
            val t = SystemClock.elapsedRealtime()
            val o = try {
                withTimeout(step.timeoutMs) { dispatch(step) }
            } catch (e: TimeoutCancellationException) {
                Outcome(step.section, step.name, Verdict.FAIL, "timed out after ${took(step.timeoutMs)}")
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                Outcome(step.section, step.name, Verdict.FAIL, "went wrong: $e", e.stackTrace.take(6).map { "at $it" })
            }
            val done = o.copy(tookMs = SystemClock.elapsedRealtime() - t)
            return if (done.verdict == Verdict.FAIL) done.copy(detail = done.detail + context(wall)) else done
        }

        private suspend fun dispatch(s: Step): Outcome {
            if (s.section == PLAYER && s.id != "start" && !started) return Outcome(s.section, s.name, Verdict.SKIP, "not run: the player did not start")
            return when (s.id) {
                "prepare" -> prepare(s)
                "start" -> start(s)
                "play" -> play(s)
                "pause" -> pause(s)
                "seek" -> seek(s)
                "nextprev" -> nextPrev(s)
                "rapid" -> rapid(s)
                "endskip" -> endSkip(s)
                "auto" -> auto(s)
                "eq" -> eq(s)
                "automix" -> autoMix(s)
                "crossfade" -> crossfade(s)
                "replaygain" -> replayGain(s)
                "offload" -> offload(s)
                "offloadeq" -> offloadEq(s)
                "lyrics" -> lyrics(s)
                "covers" -> covers(s)
                "downloads" -> downloads(s)
                "restore" -> restore(s)
                else -> Outcome(s.section, s.name, Verdict.SKIP, "unknown step")
            }
        }

        // ---- reading the player ----

        private fun reading(): Reading {
            val st = player.state.value
            val opened = PlaybackService.track
            val t = opened?.track
            var head: Long? = null
            var stamp: Long? = null
            var written: Long? = null
            if (t != null) runCatching {
                head = t.playbackHeadPosition.toLong() and 0xFFFF_FFFFL
                val ts = AudioTimestamp()
                if (t.getTimestamp(ts)) stamp = ts.framePosition
                val r = PlaybackService.rustPlayer
                if (r != null) frameBytes(t.audioFormat, t.channelCount).takeIf { it > 0 }?.let { written = r.bytesWritten / it }
            }
            val offloaded = PlaybackService.rustPlayer?.offloaded ?: false
            return Reading(
                SystemClock.elapsedRealtime(), player.positionMs, st.index, st.current?.id, recorder.heardId, st.playing,
                head, stamp, written, offloaded, t?.let(System::identityHashCode) ?: 0,
            )
        }

        private suspend fun sample(ms: Long, every: Long = 250, each: (Reading) -> Unit = {}): List<Reading> {
            val out = ArrayList<Reading>()
            val end = SystemClock.elapsedRealtime() + ms
            while (SystemClock.elapsedRealtime() < end) {
                out += reading().also(each)
                delay(every)
            }
            out += reading().also(each)
            return out
        }

        private suspend fun until(ms: Long, every: Long = 100, cond: () -> Boolean): Boolean {
            val end = SystemClock.elapsedRealtime() + ms
            while (SystemClock.elapsedRealtime() < end) {
                if (cond()) return true
                delay(every)
            }
            return cond()
        }

        private val st get() = player.state.value
        private fun durationMs(i: Int): Long = queue.getOrNull(i)?.duration?.toLong()?.times(1000) ?: 0L
        private fun mixing(): Boolean = PlaybackService.rustPlayer?.mixing == true

        /** Queue place [i] playing from [ms]. */
        private suspend fun at(i: Int, ms: Long): Boolean {
            if (st.index != i || !st.playing) player.skipTo(i)
            if (!until(15_000) { st.index == i && st.playing && player.positionMs >= 0 }) return false
            if (ms > 0) {
                player.seekTo(ms)
                return until(8_000) { kotlin.math.abs(player.positionMs - ms) < 2_000 && st.playing }
            }
            return true
        }

        private fun outcome(s: Step, j: Judged) = j.outcome(s.section, s.name, 0)
        private fun fail(s: Step, measured: String, detail: List<String> = emptyList()) = Outcome(s.section, s.name, Verdict.FAIL, measured, detail)
        private fun skip(s: Step, measured: String) = Outcome(s.section, s.name, Verdict.SKIP, measured)
        private fun pass(s: Step, measured: String) = Outcome(s.section, s.name, Verdict.PASS, measured)

        /** The timeline and the app's log since [wall]: what a failure is read by. */
        private suspend fun context(wall: Long): List<String> = withContext(Dispatchers.IO) {
            val events = runCatching { dev.nori.music.ffi.perf.perfEventsSince(wall) }.getOrDefault(emptyList()).takeLast(15)
            val log = logSince(wall).filter { NORI.containsMatchIn(it) }.takeLast(25)
            (if (events.isEmpty()) emptyList() else listOf("perf timeline:") + events.map { "  $it" }) +
                (if (log.isEmpty()) emptyList() else listOf("log:") + log.map { "  " + it.trim() })
        }

        // ---- settings ----

        private fun StoredPrefs.knobs() = Knobs(
            eqEnabled, crossfeedDb, balance, mono, limiter, speed, pitch, skipSilence, offload, crossfadeSec, autoMix,
            replayGain, scrobble, autoFill, skipExplicit, previousAlwaysSkips, fadeMs,
        )

        private fun StoredPrefs.with(k: Knobs) = copy(
            eqEnabled = k.eq, crossfeedDb = k.crossfeedDb, balance = k.balance, mono = k.mono, limiter = k.limiter,
            speed = k.speed, pitch = k.pitch, skipSilence = k.skipSilence, offload = k.offload, crossfadeSec = k.crossfadeSec, autoMix = k.autoMix,
            replayGain = k.replayGain, scrobble = k.scrobble, autoFill = k.autoFill, skipExplicit = k.skipExplicit,
            previousAlwaysSkips = k.previousAlwaysSkips, fadeMs = k.fadeMs,
        )

        private fun set(change: (Knobs) -> Knobs) {
            val k = change(nori.settings.value.knobs())
            nori.settings.update { it.with(k) }
        }

        private fun snap(): Snapshot {
            val s = st
            return Snapshot(
                nori.settings.value.knobs(), s.queue.map { it.id }, s.index, if (s.connected) player.positionMs else 0L, s.playing,
                s.shuffle, s.repeat.ordinal, PlaybackService.engine != null,
            )
        }

        // ---- the player service, as a start of the app has it ----

        private suspend fun stopPlayer(): Boolean {
            if (st.playing) { player.toggle(); until(4_000) { !st.playing } }
            player.disconnect()
            if (PlaybackService.engine == null) return true
            app.stopService(Intent(app, PlaybackService::class.java))
            return until(12_000) { PlaybackService.engine == null }
        }

        private suspend fun startPlayer(): Boolean {
            player.connect()
            return until(12_000) { PlaybackService.engine != null && st.connected }
        }

        // ---- the steps ----

        private suspend fun prepare(s: Step): Outcome {
            val before = snap()
            snapshot = before
            songsBefore = st.queue
            knobs = before.knobs
            Quiet.set(if (listen) 1f else QUIET)
            // Shuffle and repeat off while the service that has them runs; the settings the checks start from.
            if (st.connected && st.shuffle) player.setShuffle(false)
            for (k in 0 until 3) if (st.connected && st.repeat != Repeat.OFF) { player.cycleRepeat(); until(1_500) { st.repeat == Repeat.OFF } }
            set { testKnobs(it) }
            val cands = withContext(Dispatchers.IO) { candidates() }
            val picked = pickSongs(cands.map { it.first }, 6)
            val byId = cands.associate { it.first.id to it.second }
            queue = picked.mapNotNull { byId[it.id] }
            mp3 = pickSongs(cands.map { it.first }, 3, "mp3").mapNotNull { byId[it.id] }
            val local = picked.count { it.local }
            if (queue.size < 5) {
                aborted = true
                val all = cands.map { it.first }
                return fail(
                    s, "only ${queue.size} songs of the library found to play (5 needed)",
                    listOf(
                        "looked at ${all.size} songs: ${all.count { it.local }} on the phone, ${all.count { it.external || it.id.startsWith("ext-") }} from a provider, " +
                            "${all.count { it.durationS !in 45..1200 }} shorter than 45 s or longer than 20 min",
                    ),
                )
            }
            if (local < queue.size) notes += "${queue.size - local} of the ${queue.size} test songs are not on the phone and stream from the server"
            return pass(s, "${queue.size} songs (${local} on the phone), ${mp3.size} MP3s; ${if (listen) "heard" else "quiet"}; settings for the test: ${knobsDiffer(before.knobs, nori.settings.value.knobs()).joinToString().ifEmpty { "as they were" }}")
        }

        /**
         * Songs of the library to test with, each with whether it is on the phone (whole in the stream
         * cache at the quality it would stream at now, or downloaded). The offline index first, which
         * costs no request; the server only for what is still missing.
         */
        private suspend fun candidates(): List<Pair<Candidate, Song>> {
            val sources = nori.sources
            val downloaded = runCatching { nori.core.downloadIds(true).toSet() }.getOrDefault(emptySet())
            val cached = runCatching { sources.streamCache.keys.map { it.substringBefore(':') }.toSet() }.getOrDefault(emptySet())
            fun local(id: String) = id in downloaded || (id in cached && runCatching { MediaSources.isWhole(sources.streamCache, sources.streamKey(id)) }.getOrDefault(false))
            fun cand(song: Song) = Candidate(
                song.id, song.suffix, song.duration.toInt(), local(song.id), song.albumId ?: song.album, song.replayGain != null, song.isExternal,
            ) to song
            val out = LinkedHashMap<String, Pair<Candidate, Song>>()
            val wanted = cached + downloaded
            for (page in 0 until 6) {
                val songs = runCatching { nori.library.browseSongs("rowid", false, false, null, page * 500, 500) }.getOrDefault(emptyList())
                for (song in songs) {
                    if (song.id in wanted || (out.size < 300 && playable(cand(song).first))) out.getOrPut(song.id) { cand(song) }
                }
                if (songs.size < 500 || out.values.count { it.first.local } >= 12) break
            }
            // Not in the index (never synced): the songs on the phone asked for by id, a few.
            if (out.values.count { it.first.local } < 6) {
                for (id in wanted.filter { it !in out && !it.startsWith("ext-") }.take(12)) {
                    runCatching { nori.library.song(id) }.getOrNull()?.let { out[it.id] = cand(it) }
                }
            }
            if (out.values.count { playable(it.first) } < 6) {
                runCatching { nori.library.randomSongs() }.getOrDefault(emptyList()).forEach { out.getOrPut(it.id) { cand(it) } }
            }
            return out.values.toList()
        }

        private suspend fun start(s: Step): Outcome {
            started = false
            if (!startPlayer()) return fail(s, "the player service did not start")
            player.play(queue, 0)
            if (!until(20_000) { st.playing && st.index == 0 && player.positionMs > 300 }) return fail(s, "the queue did not start playing")
            started = true
            return pass(s, "playing \"${queue[0].title}\"")
        }

        private fun rate(): Int = PlaybackService.track?.track?.sampleRate ?: 48_000

        private suspend fun play(s: Step): Outcome {
            if (!at(0, 5_000)) return fail(s, "could not start the first song at 5 s")
            val r = sample(10_000)
            return outcome(s, judgeProgress(r, written = r.none { it.offloaded }))
        }

        private suspend fun pause(s: Step): Outcome {
            if (!st.playing && !at(0, 5_000)) return fail(s, "nothing playing to pause")
            player.toggle()
            if (!until(3_000) { !st.playing }) return fail(s, "still playing 3 s after pause")
            delay(400)
            val paused = judgePaused(sample(2_000), rate = rate())
            player.toggle()
            if (!until(5_000) { st.playing }) return fail(s, "did not play again within 5 s of resume", paused.problems)
            delay(500)
            val resumed = judgeProgress(sample(3_000), written = !(PlaybackService.rustPlayer?.offloaded ?: false))
            return outcome(s, Judged("${paused.measured}; resumed: ${resumed.measured}", paused.problems + resumed.problems.map { "after resuming: $it" }))
        }

        private suspend fun seek(s: Step): Outcome {
            if (!at(1, 3_000)) return fail(s, "could not play the second song")
            val target = minOf(30_000L, durationMs(1) / 2)
            player.seekTo(target)
            return outcome(s, judgeSeek(target, sample(3_000)))
        }

        private fun arrivalsSince(t: Long) = recorder.arrivals.filter { it.tMs >= t }

        private suspend fun nextPrev(s: Step): Outcome {
            if (!at(1, 0)) return fail(s, "could not play the second song")
            delay(500)
            var t = SystemClock.elapsedRealtime()
            player.next()
            until(4_000) { st.index == 2 && recorder.heardId == queue[2].id }
            delay(800)
            val next = judgeMove(1, 1, reading(), queue[2].id, arrivalsSince(t).map { it.index })
            t = SystemClock.elapsedRealtime()
            player.previous()
            until(4_000) { st.index == 1 && recorder.heardId == queue[1].id }
            delay(800)
            val back = judgeMove(2, -1, reading(), queue[1].id, arrivalsSince(t).map { it.index })
            // Well into a song, previous starts it again rather than going back.
            player.seekTo(10_000)
            until(4_000) { player.positionMs > 9_000 }
            player.previous()
            val restarted = until(4_000) { player.positionMs < 3_000 }
            val problems = next.problems.map { "next: $it" } + back.problems.map { "previous: $it" } +
                (if (!restarted || st.index != 1) listOf("previous 10 s into a song: at ${secs(player.positionMs)} of queue place ${st.index}, not the same song from the top") else emptyList())
            return outcome(s, Judged("next ${next.measured}; previous ${back.measured}; previous 10 s in restarted: $restarted", problems))
        }

        private suspend fun rapid(s: Step): Outcome {
            if (!at(0, 3_000)) return fail(s, "could not play the first song")
            val t = SystemClock.elapsedRealtime()
            repeat(3) { player.next(); delay(150) }
            delay(3_000)
            val end = reading()
            val engineIndex = dev.nori.music.ffi.perf.perfEngineSeen()?.index?.toInt()
            val j = judgeMove(0, 3, end, queue[3].id, arrivalsSince(t).map { it.index }, engineIndex)
            delay(2_500)
            val later = reading()
            val moved = if (later.index != end.index) listOf("moved on to queue place ${later.index} by itself 2.5 s later") else emptyList()
            return outcome(s, Judged(j.measured, j.problems + moved))
        }

        private suspend fun endSkip(s: Step): Outcome {
            if (!at(1, 0)) return fail(s, "could not play the second song")
            val d = durationMs(1).takeIf { it > 0 } ?: st.durationMs
            player.seekTo(d - 1_700)
            if (!until(5_000) { st.index != 1 || player.positionMs >= d - 1_650 }) return fail(s, "the seek to 1.7 s before the end did not land")
            val t = SystemClock.elapsedRealtime()
            val endedFirst = st.index != 1
            val left = d - player.positionMs
            player.next()
            delay(3_000)
            val arr = arrivalsSince(t - 2_000)
            // A song that ended just before the press moves on by itself first: then the press is the second song.
            val by = if (endedFirst || arr.any { it.auto && it.tMs < t }) 2 else 1
            val j = judgeMove(1, by, reading(), queue.getOrNull(1 + by)?.id, arrivalsSince(t).map { it.index })
            return outcome(s, Judged("pressed ${secs(left)} before the end; ${j.measured}", j.problems))
        }

        private suspend fun auto(s: Step): Outcome {
            if (!at(2, 0)) return fail(s, "could not play the third song")
            val d = durationMs(2).takeIf { it > 0 } ?: st.durationMs
            player.seekTo(d - 4_000)
            val t = SystemClock.elapsedRealtime()
            if (!until(12_000) { st.index == 3 }) return fail(s, "still on queue place ${st.index} at ${secs(player.positionMs)}, 12 s after seeking to 4 s before the end")
            delay(500)
            val arr = arrivalsSince(t)
            val r = sample(2_500)
            val j = judgeProgress(r, written = r.none { it.offloaded })
            val problems = j.problems.toMutableList()
            if (arr.none { it.auto && it.index == 3 }) problems += "the service did not say it moved on by itself: arrivals ${arr.map { "${it.index}${if (it.auto) " (by itself)" else ""}" }}"
            if (r.first().positionMs > 6_000) problems += "the next song started at ${secs(r.first().positionMs)}"
            if (r.last().heardId != queue[3].id || r.last().shownId != queue[3].id) problems += "shown ${r.last().shownId}, heard ${r.last().heardId}, expected ${queue[3].id}"
            return outcome(s, Judged("into the next song; ${j.measured}", problems))
        }

        private suspend fun eq(s: Step): Outcome {
            if (!at(0, 8_000)) return fail(s, "could not play the first song")
            val before = PlaybackService.rustPlayer?.chainIn == true
            val t = SystemClock.elapsedRealtime()
            set { it.copy(eq = true) }
            val inChain = until(1_500, 20) { PlaybackService.rustPlayer?.chainIn == true }
            val tookMs = SystemClock.elapsedRealtime() - t
            val on = judgeProgress(sample(2_500), written = true)
            set { it.copy(eq = false) }
            val off = judgeProgress(sample(2_500), written = true)
            val problems = (if (!inChain) listOf("the equalizer was not in the samples' path 1.5 s after it was switched on") else emptyList()) +
                on.problems.map { "equalizer on: $it" } + off.problems.map { "equalizer off: $it" }
            val chain = if (before) "the chain was in the path already (flat)" else "in the path after $tookMs ms"
            return outcome(s, Judged("$chain; on: ${on.measured}; off: ${off.measured}", problems))
        }

        /** A song and the one after it, from different albums (a run of one album keeps its own joins), both long enough. */
        private fun pair(from: Int = 0): Int? = (from until queue.size - 1).firstOrNull { i ->
            val (a, b) = queue[i] to queue[i + 1]
            a.duration >= 60u && (a.albumId ?: a.album) != (b.albumId ?: b.album)
        }

        private suspend fun measured(ids: List<String>): Map<String, Boolean> = withContext(Dispatchers.IO) {
            ids.associateWith { id -> runCatching { nori.core.analysisGet(id) != null }.getOrDefault(false) }
        }

        private suspend fun autoMix(s: Step): Outcome {
            val i = pair() ?: return skip(s, "no two songs of different albums, a minute long or more, follow each other in the test queue")
            if (!at(i, 15_000)) return fail(s, "could not play queue place $i")
            val wall = System.currentTimeMillis()
            set { it.copy(autoMix = true) }
            try {
                val ids = listOf(queue[i].id, queue[i + 1].id)
                // Measured ahead: the song playing and the next, as soon as they are whole on the phone.
                val end = SystemClock.elapsedRealtime() + 30_000
                var ahead = measured(ids)
                while (!ahead.values.all { it } && SystemClock.elapsedRealtime() < end) { delay(500); ahead = measured(ids) }
                player.seekTo(durationMs(i) - 30_000)
                var mixed = false
                val moved = until(45_000, 150) { if (mixing()) mixed = true; st.index != i }
                until(4_000, 150) { if (mixing()) mixed = true; false }
                val lines = withContext(Dispatchers.IO) { logSince(wall) }
                val j = judgeAutoMix(queue[i].title, lines, ahead, mixed)
                val problems = j.problems + if (moved) emptyList() else listOf("the song did not end within 45 s")
                return outcome(s, Judged(j.measured, problems))
            } finally {
                set { it.copy(autoMix = false) }
            }
        }

        private suspend fun crossfade(s: Step): Outcome {
            val j0 = pair(1) ?: pair() ?: return skip(s, "no two songs of different albums follow each other in the test queue")
            if (!at(j0, 0)) return fail(s, "could not play queue place $j0")
            player.seekTo(durationMs(j0) - 15_000)
            until(4_000) { player.positionMs > durationMs(j0) - 16_000 }
            val wall = System.currentTimeMillis()
            set { it.copy(crossfadeSec = 6) }
            try {
                var mixed = false
                val moved = until(25_000, 150) { if (mixing()) mixed = true; st.index != j0 }
                until(4_000, 150) { if (mixing()) mixed = true; false }
                val lines = withContext(Dispatchers.IO) { logSince(wall) }
                val j = judgeCrossfade(6, queue[j0].title, lines, mixed)
                return outcome(s, Judged(j.measured, j.problems + if (moved) emptyList() else listOf("the song did not end within 25 s")))
            } finally {
                set { it.copy(crossfadeSec = 0) }
            }
        }

        private suspend fun replayGain(s: Step): Outcome {
            val k = queue.indexOfFirst { it.replayGain != null }.takeIf { it >= 0 } ?: 0
            if (!at(k, 5_000)) return fail(s, "could not play queue place $k")
            val user = knobs?.replayGain ?: GainMode.OFF
            try {
                set { it.copy(replayGain = GainMode.OFF) }
                delay(1_200)
                val gOff = nori.session.playlistGain(false)
                set { it.copy(replayGain = GainMode.TRACK) }
                delay(1_200)
                val gTrack = nori.session.playlistGain(false)
                val r = judgeProgress(sample(2_000), written = !(PlaybackService.rustPlayer?.offloaded ?: false))
                val tags = if (queue[k].replayGain == null) " (the song has no ReplayGain tags)" else ""
                return outcome(s, Judged("level off ${fmt(gOff)}, by track ${fmt(gTrack)}$tags; the player puts it on the samples, which cannot be read from here; ${r.measured}", r.problems))
            } finally {
                set { it.copy(replayGain = user) }
            }
        }

        private fun fmt(f: Float) = String.format(Locale.ROOT, "%.3f", f)

        /** Why the chip cannot take an MP3 here (none: it can): the platform's own answer. */
        private fun chipWhyNot(): String? {
            if (Build.VERSION.SDK_INT < 29) return "Android ${Build.VERSION.RELEASE} has no audio offload"
            if (nori.outputs.usb.value) return "something USB is attached, which the audio chip cannot reach"
            val f = AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_MP3).setSampleRate(44_100).setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build()
            val attrs = android.media.AudioAttributes.Builder().setUsage(android.media.AudioAttributes.USAGE_MEDIA).setContentType(android.media.AudioAttributes.CONTENT_TYPE_MUSIC).build()
            return runCatching {
                if (Build.VERSION.SDK_INT >= 33) {
                    val v = AudioManager.getDirectPlaybackSupport(f, attrs)
                    if (v and (AudioManager.DIRECT_PLAYBACK_OFFLOAD_SUPPORTED or AudioManager.DIRECT_PLAYBACK_OFFLOAD_GAPLESS_SUPPORTED) == 0) "the platform says MP3 is not offloaded here (getDirectPlaybackSupport $v)" else null
                } else if (!AudioManager.isOffloadedPlaybackSupported(f, attrs)) "the platform says MP3 is not offloaded here" else null
            }.getOrElse { "the platform could not be asked: $it" }
        }

        private suspend fun offload(s: Step): Outcome {
            chipWhyNot()?.let { offloadWhyNot = it; return skip(s, "not available on this device: $it") }
            if (mp3.size < 2) { offloadWhyNot = "no MP3s"; return skip(s, "not run: the library has fewer than two MP3 songs to offload") }
            set { it.copy(offload = true) }
            val requests0 = OffloadCalls.dataRequests
            player.play(mp3, 0)
            if (!until(15_000) { PlaybackService.rustPlayer?.offloaded == true && st.playing }) {
                return fail(s, "offload was not entered: ${PlaybackService.rustPlayer?.pcmWhy ?: "no reason given"}")
            }
            offloadEntered = true
            val opened = PlaybackService.track
            val granted = runCatching { opened?.track?.bufferSizeInFrames ?: 0 }.getOrDefault(0)
            val asked = opened?.askedBytes ?: 0
            val t0 = SystemClock.elapsedRealtime()
            val jumps = ArrayList<Long>()
            var skipped = false
            var seeked = false
            val first = mp3[0].id
            val r = sample(21_000) { x ->
                val at = x.tMs - t0
                if (!skipped && at >= 7_000) { skipped = true; jumps += x.tMs; player.next() }
                if (!seeked && at >= 13_000) { seeked = true; jumps += x.tMs; player.seekTo(minOf(40_000L, (mp3[1].duration.toLong() * 1000).coerceAtLeast(60_000) / 2)) }
            }
            val requests = OffloadCalls.dataRequests - requests0
            val j = judgeOffload(r, jumps)
            val problems = j.problems.toMutableList()
            if (r.last().heardId == first) problems += "the skip at 7 s did not move to the next song"
            val heads = r.mapNotNull { it.head }
            val stamps = r.mapNotNull { it.stamp }
            val m = "granted ${granted / 1024} KB of ${asked / 1024} KB asked; play head ${heads.firstOrNull()} to ${heads.lastOrNull()}, timestamp ${stamps.firstOrNull()} to ${stamps.lastOrNull()}; " +
                "$requests data requests, ${OffloadCalls.tornDown} tear-downs so far; ${j.measured}"
            return outcome(s, Judged(m, problems))
        }

        private suspend fun offloadEq(s: Step): Outcome {
            offloadWhyNot?.let { return skip(s, "not available on this device: $it") }
            if (!offloadEntered) return skip(s, "not run: offload was not entered")
            val r = PlaybackService.rustPlayer ?: return fail(s, "the Rust player is gone")
            try {
                if (!r.offloaded && !until(5_000) { r.offloaded }) return fail(s, "not offloaded before the equalizer went on: ${r.pcmWhy}")
                var t = SystemClock.elapsedRealtime()
                set { it.copy(eq = true) }
                val left = until(3_000, 20) { !r.offloaded }
                val leftMs = SystemClock.elapsedRealtime() - t
                val on = judgeProgress(sample(2_500), written = false)
                t = SystemClock.elapsedRealtime()
                set { it.copy(eq = false) }
                val wanted = until(1_500, 20) { r.offloadWanted }
                var back = until(3_000) { r.offloaded }
                val why = r.pcmWhy
                if (!back) { player.next(); back = until(8_000) { r.offloaded } }
                val backMs = SystemClock.elapsedRealtime() - t
                val problems = ArrayList<String>()
                if (!left) problems += "still offloaded 3 s after the equalizer went on"
                problems += on.problems.map { "equalizer on: $it" }
                if (!wanted) problems += "offload not wanted again 1.5 s after the equalizer went off"
                if (!back) problems += "not offloaded again, even at the next song: $why"
                return outcome(s, Judged("left offload ${leftMs} ms after the equalizer went on (${on.measured}); back ${backMs} ms after it went off${if (why != null) " ($why)" else ""}", problems))
            } finally {
                set { it.copy(eq = false, offload = false) }
            }
        }

        private suspend fun lyrics(s: Step): Outcome {
            val store = ViewModelStore()
            val seen = java.util.concurrent.CopyOnWriteArrayList<LyricsSeen>()
            val vm = ViewModelProvider(store, ViewModelProvider.AndroidViewModelFactory.getInstance(app))[PlayerViewModel::class.java]
            val watching = scope!!.launch {
                vm.lyrics.collect { f ->
                    val v = f.value
                    val ready = (v as? Load.Ready)?.data
                    val lines = ready?.lyrics?.lines.orEmpty()
                    seen += LyricsSeen(
                        SystemClock.elapsedRealtime(), f.songId, st.current?.id, v is Load.Loading,
                        ready?.let { lyricsRank(lines.size, it.lyrics.synced, it.lyrics.wordTimed) } ?: 0, v is Load.Failed,
                        ready?.let { "${it.source}:${lines.size}:${lines.joinToString("\n") { l -> l.text }.hashCode()}" }.orEmpty(),
                    )
                }
            }
            try {
                for (i in 0 until 3) {
                    if (!at(i, 0)) return fail(s, "could not play queue place $i")
                    val id = queue[i].id
                    until(15_000, 200) { seen.any { it.songId == id && !it.loading } }
                    delay(1_500)
                }
                // Back to a song whose words were shown (the first, when none had any).
                val to = (0 until 3).firstOrNull { i -> seen.any { it.songId == queue[i].id && it.rank > 0 } } ?: 0
                if (!at(to, 0)) return fail(s, "could not go back to queue place $to")
                val back = SystemClock.elapsedRealtime() - 1_000
                delay(4_000)
                val j = judgeLyrics(seen.toList(), queue[to].id, back)
                val missing = (0 until 3).map { queue[it].id }.filter { id -> seen.none { it.songId == id && !it.loading } }
                return outcome(s, Judged(j.measured, j.problems + missing.map { "no answer for $it within 15 s" }))
            } finally {
                watching.cancel()
                store.clear()
            }
        }

        private suspend fun covers(s: Step): Outcome {
            val arts = queue.mapNotNull { it.coverArt }.distinct().take(3)
            if (arts.isEmpty()) return skip(s, "the test songs have no covers")
            val loader = CoverLoader.get(app)
            val problems = ArrayList<String>()
            val said = ArrayList<String>()
            for (art in arts) {
                val url = nori.library.coverUrl(art, 300) ?: continue
                val t = SystemClock.elapsedRealtime()
                val bitmap = withTimeout(10_000) { suspendCancellableCoroutine { c -> val r = loader.load(url, 300, 300) { c.resume(it) }; c.invokeOnCancellation { r.cancel() } } }
                val decodeMs = SystemClock.elapsedRealtime() - t
                val t2 = SystemClock.elapsedRealtime()
                val pages = withContext(Dispatchers.IO) { loader.colours(url, 300, true, true, true) }
                val colourMs = SystemClock.elapsedRealtime() - t2
                val look = pages?.plain?.look
                if (bitmap == null) problems += "cover $art did not decode"
                if (look == null || look.none { it != 0 }) problems += "cover $art gave no colours"
                if (pages?.black == null) problems += "cover $art gave no colours on black"
                said += "${bitmap?.let { "${it.width}x${it.height}" } ?: "none"} in $decodeMs ms, colours in $colourMs ms"
            }
            return outcome(s, Judged("${arts.size} covers: ${said.joinToString("; ")}", problems))
        }

        private suspend fun downloads(s: Step): Outcome = withContext(Dispatchers.IO) {
            val id = runCatching { nori.core.downloadIds(true) }.getOrDefault(emptyList()).firstOrNull { !it.startsWith("ext-") }
                ?: return@withContext skip(s, "nothing is downloaded")
            val sources = nori.sources
            val key = sources.downloadKey(id)
            if (!MediaSources.isWhole(sources.downloadCache, key)) return@withContext fail(s, "the download of $id is not whole on the phone")
            val t = SystemClock.elapsedRealtime()
            val (source, length) = sources.openResolved(sources.downloadUrl(id), key, 0)
            val buf = ByteArray(64 * 1024)
            var got = 0
            try {
                while (got < buf.size) { val n = source.read(buf, got, buf.size - got); if (n < 0) break; got += n }
            } finally { runCatching { source.close() } }
            if (got <= 0) fail(s, "the download of $id opened but gave no bytes")
            else pass(s, "$id: $got bytes of $length read from the phone in ${SystemClock.elapsedRealtime() - t} ms")
        }

        private suspend fun restore(s: Step): Outcome {
            val before = snapshot ?: return skip(s, "nothing was changed")
            val now = snap()
            val steps = restoreSteps(before, now)
            val problems = ArrayList<String>()
            if (!stopPlayer()) problems += "the player service would not stop"
            Quiet.set(1f)
            nori.settings.update { it.with(before.knobs) }
            // What the stopped service saved of the test's queue is written over with the user's, which the
            // service then opens as a start of the app does: at its place, unprepared, nothing fetched.
            suspend fun putQueue() = withContext(Dispatchers.IO) {
                delay(800)
                runCatching { nori.core.saveQueue(PlayQueue(songsBefore, before.index.coerceAtLeast(0).toUInt(), before.positionMs.coerceAtLeast(0).toULong())) }
            }
            if (RestoreStep.QUEUE in steps) putQueue()
            if (RestoreStep.START_PLAYER in steps) {
                if (!startPlayer()) problems += "the player service did not start again"
                if (RestoreStep.QUEUE in steps && !until(8_000) { st.queue.map { it.id } == before.ids }) {
                    // Written over once more by a save that came late: again, once.
                    stopPlayer(); putQueue(); startPlayer()
                    until(8_000) { st.queue.map { it.id } == before.ids }
                }
            }
            if (RestoreStep.SHUFFLE in steps && before.shuffle != st.shuffle) { player.setShuffle(before.shuffle); until(2_000) { st.shuffle == before.shuffle } }
            if (RestoreStep.REPEAT in steps) for (k in 0 until 3) if (st.repeat.ordinal != before.repeat) { player.cycleRepeat(); until(1_500) { st.repeat.ordinal == before.repeat } }
            if (RestoreStep.RESUME in steps) { player.toggle(); until(10_000) { st.playing } }
            delay(500)
            val after = if (before.serviceRunning || before.ids.isNotEmpty()) snap() else snap().copy(serviceRunning = before.serviceRunning)
            problems += restoreProblems(before, after)
            val m = "${before.ids.size} songs at queue place ${before.index}, ${secs(before.positionMs)}, ${if (before.playing) "playing" else "paused"}"
            return outcome(s, Judged(m, problems))
        }
    }

    companion object {
        /** The self test's volume unless it is to be heard: -66 dB, inaudible, and still a real output. */
        const val QUIET = 0.0005f

        /** A line of the app's own tag. */
        private val NORI = Regex(" [VDIWEF] (nori|noritest)\\s*:")

        /** The app's log since [wallMs], as logcat has it (this process). */
        fun logSince(wallMs: Long): List<String> = runCatching {
            val since = String.format(Locale.ROOT, "%d.%03d", wallMs / 1000, wallMs % 1000)
            val p = ProcessBuilder("logcat", "-d", "-v", "threadtime", "--pid=${Process.myPid()}", "-T", since)
                .redirectError(ProcessBuilder.Redirect.to(File("/dev/null"))).start()
            val lines = p.inputStream.bufferedReader().use { it.readLines() }
            p.waitFor()
            lines
        }.getOrDefault(emptyList())
    }
}
