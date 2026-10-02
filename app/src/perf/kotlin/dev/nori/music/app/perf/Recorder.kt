package dev.nori.music.app.perf

import android.app.Activity
import android.app.Application
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.TrafficStats
import android.os.BatteryManager
import android.os.Build
import android.os.Bundle
import android.os.Debug
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.PowerManager
import android.os.Process
import android.os.SystemClock
import android.system.Os
import android.system.OsConstants
import android.view.FrameMetrics
import android.view.Window
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.core.content.ContextCompat
import dev.nori.music.Nori
import dev.nori.music.app.PerfHooks
import dev.nori.music.look.CoverPixels
import dev.nori.music.ffi.perf.PerfCounters
import dev.nori.music.ffi.perf.PerfDevice
import dev.nori.music.ffi.perf.PerfFrames
import dev.nori.music.ffi.perf.PerfLogs
import dev.nori.music.ffi.perf.PerfMemory
import dev.nori.music.ffi.perf.PerfNote
import dev.nori.music.ffi.perf.PerfOutput
import dev.nori.music.ffi.perf.PerfPage
import dev.nori.music.ffi.perf.PerfSong
import dev.nori.music.ffi.perf.PerfStretch
import dev.nori.music.ffi.perf.PerfThread
import dev.nori.music.playback.OpenedTrack
import dev.nori.music.playback.PlaybackObserver
import dev.nori.music.playback.PlaybackService
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.android.asCoroutineDispatcher
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import java.io.File

/**
 * The perf build's recorder: what the app costs, stretch by stretch, on the owner's own phone.
 *
 * A stretch is a span of one state (screen off and playing, the player open, charging, ...) with one set
 * of the settings that change the cost. Which state that is, what two readings of the counters make, how
 * the stretches add up and how the page and the report say it are the core's (perf_log.rs); this reads
 * Android's counters and draws the page. The counters are read when the state or those
 * settings change, which ends one stretch and starts the next, and when the Performance page opens
 * (the stretch so far). Nothing here ticks: every read is set off by a broadcast (screen on or off,
 * power connected, the service's play/pause), an activity starting or stopping, the player sheet or
 * a settings change. While the phone sleeps with music playing this runs only when a song changes,
 * which the service's broadcast already woke it for, so the recorder adds no wakeups of its own to the
 * numbers it records. Why a stretch cost what it did is read
 * at its ends as well: every thread's name, CPU time and wakeups, the AudioTrack the player opened, and
 * the bytes the app moved over the network.
 *
 * What happened during a stretch is its timeline (the core's `perf_note`): the player service tells this
 * each song, output, error and shallow buffer change as it happens ([PlaybackObserver]), and the settings flow
 * each change; the output's underrun count is read at a stretch's ends and at each song. The app's own
 * log is read from logcat only when the report is shared or the page's log is opened, and a crash is
 * kept in the app's database as the process dies, and the crash buffer at each start.
 *
 * Everything happens on one background thread, which sleeps in its looper between events. The frame
 * listener is there only while an activity is started, so it costs nothing with the screen off.
 */
internal class Recorder(private val app: Application) : PerfHooks.Recorder, PlaybackObserver {
    private val thread = HandlerThread("perf", Process.THREAD_PRIORITY_BACKGROUND).apply { start() }
    private val handler = Handler(thread.looper)
    private val main = Handler(Looper.getMainLooper())
    private val battery = app.getSystemService(BatteryManager::class.java)
    private val msPerTick = 1000.0 / Os.sysconf(OsConstants._SC_CLK_TCK)

    // What the app is doing now. Written and read on the perf thread only.
    private var screenOn = app.getSystemService(PowerManager::class.java).isInteractive
    private var playing = false
    private var foreground = false
    private var playerOpen = false
    private var charging = false
    private var settings = ""

    // The stretch under way: where it started, and the frames drawn since.
    private var start: PerfCounters? = null
    private var startKey = ""
    private var startCfg = ""
    private var frames = 0L
    private var janky = 0L
    private var worstNs = 0L
    private var frameBudgetNs = 16_666_667L

    /** What the page shows: the stretches kept and the one under way, read when it opens; none until then. */
    val shown = mutableStateOf<Shown?>(null)
    val callBench = mutableStateOf("")
    val coverBench = mutableStateOf("")
    /** The page's log section as the core laid it out, read when it is unfolded, and whether it is. */
    val log = mutableStateOf("")
    val logOpen = mutableStateOf(false)
    /** The page's fixed words. */
    val words = PerfWords

    /** The self test behind the page's button; made when the page first shows it. */
    val selfTest by lazy { SelfTest(app, this) }

    /** The song the player service last arrived on: the one heard, for the self test and the watch. */
    @Volatile var heardId: String? = null
        private set

    /** The service's arrivals (queue place, by itself or not, when), kept only while the self test runs. */
    class Arrival(val tMs: Long, val index: Int, val auto: Boolean)
    val arrivals = java.util.concurrent.CopyOnWriteArrayList<Arrival>()
    @Volatile var testing = false

    private val events = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.action) {
                Intent.ACTION_SCREEN_ON -> screenOn = true
                Intent.ACTION_SCREEN_OFF -> screenOn = false
                Intent.ACTION_POWER_CONNECTED -> charging = true
                Intent.ACTION_POWER_DISCONNECTED -> charging = false
                // The service says so on every song as well; only a change of playing counts.
                PlaybackService.ACTION_STATE -> playing = intent.getBooleanExtra(PlaybackService.EXTRA_PLAYING, playing)
            }
            changed()
        }
    }

    /**
     * Counted on the perf thread as the frames are drawn. A frame is janky when it took longer than
     * its deadline (Android 12 on) or than one refresh of the display (before).
     */
    private val frameListener = Window.OnFrameMetricsAvailableListener { _, m, dropped ->
        if (m.getMetric(FrameMetrics.FIRST_DRAW_FRAME) == 1L) return@OnFrameMetricsAvailableListener
        val total = m.getMetric(FrameMetrics.TOTAL_DURATION)
        val budget = if (Build.VERSION.SDK_INT >= 31) m.getMetric(FrameMetrics.DEADLINE) else frameBudgetNs
        // Frames the listener was too slow to be told about still happened; their times are unknown.
        frames += 1 + dropped
        if (total > budget) janky++
        if (total > worstNs) worstNs = total
    }

    fun install() {
        // A crash is kept before the process dies, then handed on to the platform's handler as before.
        val before = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { t, e ->
            runCatching { dev.nori.music.ffi.perf.perfCrashKeep("exception", System.currentTimeMillis(), "thread ${t.name}: ${e.stackTraceToString()}") }
            before?.uncaughtException(t, e) ?: Process.killProcess(Process.myPid())
        }
        PlaybackService.observer = this
        var started = 0
        app.registerActivityLifecycleCallbacks(object : Application.ActivityLifecycleCallbacks {
            override fun onActivityStarted(activity: Activity) {
                if (Build.VERSION.SDK_INT < 31) {
                    @Suppress("DEPRECATION")
                    val display = if (Build.VERSION.SDK_INT >= 30) activity.display else activity.windowManager.defaultDisplay
                    display?.refreshRate?.takeIf { it > 0f }?.let { frameBudgetNs = (1e9 / it).toLong() }
                }
                activity.window.addOnFrameMetricsAvailableListener(frameListener, handler)
                if (started++ == 0) handler.post { foreground = true; changed() }
            }

            override fun onActivityStopped(activity: Activity) {
                runCatching { activity.window.removeOnFrameMetricsAvailableListener(frameListener) }
                if (--started == 0) handler.post { foreground = false; changed() }
            }

            override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
            override fun onActivityResumed(activity: Activity) {}
            override fun onActivityPaused(activity: Activity) {}
            override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
            override fun onActivityDestroyed(activity: Activity) {}
        })
        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_SCREEN_ON)
            addAction(Intent.ACTION_SCREEN_OFF)
            addAction(Intent.ACTION_POWER_CONNECTED)
            addAction(Intent.ACTION_POWER_DISCONNECTED)
            addAction(PlaybackService.ACTION_STATE)
        }
        ContextCompat.registerReceiver(app, events, filter, null, handler, ContextCompat.RECEIVER_NOT_EXPORTED)
        // The invariant watch (the core's invariants.rs): from here on every event below is looked at too.
        dev.nori.music.ffi.perf.perfWatch(true)
        handler.post {
            charging = batteryIntent()?.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0)?.let { it != 0 } ?: false
            // A settings change that alters the cost ends the stretch as a change of state does. The line
            // is the core's, read from the settings it keeps, which have the change before this hears of it.
            val prefs = Nori.get(app).settings.prefs
            settings = cfg()
            val scope = CoroutineScope(handler.asCoroutineDispatcher())
            // Every change goes on the timeline, told first so that it lands in the stretch it ends; the
            // first value only tells the core where changes count from.
            scope.launch {
                var first = true
                prefs.collect {
                    dev.nori.music.ffi.perf.perfNoteSettings(System.currentTimeMillis())
                    // A change is in the engine a second later: looked at once then, never otherwise.
                    if (!first) { handler.removeCallbacks(settingsLook); handler.postDelayed(settingsLook, 1_000) }
                    first = false
                }
            }
            // The song on the screen against the one heard, and the queue's lengths under AutoMix: each
            // looked at as the page's state changes, which it does by itself on every song.
            val player = Nori.get(app).player
            scope.launch {
                player.state.map { it.current?.id }.distinctUntilChanged().collect { id ->
                    dev.nori.music.ffi.perf.perfWatchShown(System.currentTimeMillis(), id)
                }
            }
            scope.launch {
                player.state.map { it.queue }.distinctUntilChanged { a, b -> a === b }.collect { q ->
                    val missing = q.filter { it.duration == 0u && !it.id.startsWith("radio:") }.map { it.id }
                    dev.nori.music.ffi.perf.perfWatchQueue(System.currentTimeMillis(), missing, q.size.toUInt())
                }
            }
            scope.launch {
                prefs.map { cfg() }.distinctUntilChanged().collect { settings = it; changed() }
            }
            changed()
            // A crash buffer from an earlier run is kept, so the report has it after logcat has let it go.
            dev.nori.music.ffi.perf.perfCrashKeep("buffer", System.currentTimeMillis(), logcat("-b", "crash", "-d", "-v", "threadtime", "-t", "200", failed = ""))
        }
    }

    override fun playerOpen(open: Boolean) {
        handler.post { playerOpen = open; changed() }
    }

    // ---- the timeline: told by the player service, on its threads, and noted on this one ----

    override fun song(id: String) {
        val t = System.currentTimeMillis()
        heardId = id
        handler.post {
            dev.nori.music.ffi.perf.perfWatchHeard(t, id)
            underruns(t)
            val nori = Nori.get(app)
            // A song the queue does not know comes back with its id only.
            val s = dev.nori.music.ffi.queue.queueSongs(listOf(id)).firstOrNull()
            val copy = if (s != null) runCatching { nori.sources.streamCopy(id) }.getOrNull() else null
            val song = PerfSong(
                id = id, title = s?.title?.ifEmpty { null } ?: id, artist = s?.artist.orEmpty(), suffix = s?.suffix.orEmpty(),
                bitRate = s?.bitRate?.toInt() ?: 0, samplingRate = s?.samplingRate?.toInt() ?: 0, bitDepth = s?.bitDepth?.toInt() ?: 0,
                channels = s?.channelCount?.toInt() ?: 0, downloaded = s != null && nori.sources.isDownloaded(id),
                cacheKey = copy?.first.orEmpty(), cachedWhole = copy?.second == true,
            )
            note(t, PerfNote.Song(song))
        }
    }

    override fun engine(engine: String?) {
        val t = System.currentTimeMillis()
        handler.post { note(t, PerfNote.Engine(engine)) }
    }

    override fun track(opened: OpenedTrack?) {
        val t = System.currentTimeMillis()
        // Read on this thread a moment later: what the platform made of the track by then.
        handler.post { note(t, PerfNote.Output(opened?.let { System.identityHashCode(it.track).toLong() } ?: 0L, opened?.let(::output))) }
    }

    override fun error(message: String) {
        val t = System.currentTimeMillis()
        handler.post { note(t, PerfNote.Error(message)) }
    }

    override fun wakeLock(held: Boolean) {
        val t = System.currentTimeMillis()
        handler.post { note(t, PerfNote.WakeLock(held)) }
    }

    override fun shallow(on: Boolean) {
        val t = System.currentTimeMillis()
        handler.post { note(t, PerfNote.Shallow(on)) }
    }

    override fun skipped(index: Int) {
        val t = System.currentTimeMillis()
        handler.post { dev.nori.music.ffi.perf.perfWatchSkip(t, index.toLong()) }
    }

    override fun arrived(index: Int, auto: Boolean, shuffled: Boolean) {
        val t = System.currentTimeMillis()
        if (testing) arrivals += Arrival(SystemClock.elapsedRealtime(), index, auto)
        handler.post { dev.nori.music.ffi.perf.perfWatchArrived(t, index.toLong(), auto, shuffled) }
    }

    override fun coverFailed(url: String, status: Int, again: Boolean) {
        // The cover's id and size, not the address: that carries the login's token.
        val uri = android.net.Uri.parse(url)
        val which = listOfNotNull(uri.getQueryParameter("id"), uri.getQueryParameter("size")?.let { "at $it" }).joinToString(" ").ifEmpty { "a cover" }
        val why = when {
            status >= CoverPixels.HTTP -> "HTTP ${status - CoverPixels.HTTP}"
            status >= CoverPixels.NETWORK -> "network: " + (dev.nori.music.ffi.net.FailureKind.entries.getOrNull(status - CoverPixels.NETWORK)?.name ?: "failure ${status - CoverPixels.NETWORK}")
            status == CoverPixels.CLOSED -> "answered with nothing (nobody waited, or the loader closed)"
            status == CoverPixels.UNKNOWN -> "a format that is not decoded"
            status == CoverPixels.BROKEN -> "a broken file"
            status == CoverPixels.BAD_BITMAP -> "no Bitmap to draw into"
            else -> "unreadable ($status)"
        }
        dev.nori.music.NoriLog.i("cover: $which did not load: $why; " + if (again) "asking again in ${dev.nori.music.app.ui.CoverFetch.RETRY_MS} ms" else "the placeholder stays")
    }

    override fun lyricsShown(songId: String) {
        val t = System.currentTimeMillis()
        handler.post { dev.nori.music.ffi.perf.perfWatchLyrics(t, songId) }
    }

    /**
     * A second after the settings changed: what the engine shows against them (the core's call, which
     * judges only while playing through an open output, and not just after the service started).
     */
    private val settingsLook = Runnable {
        if (PlaybackService.engine == null) return@Runnable
        val nori = Nori.get(app)
        dev.nori.music.ffi.perf.perfWatchSettings(
            System.currentTimeMillis(), PlaybackService.offloadWanted, dev.nori.music.playback.Equalizer.inChain,
            PlaybackService.rustPlayer?.onCpu == true, nori.outputs.usb.value,
            playing, PlaybackService.track != null,
        )
    }

    private fun note(t: Long, note: PerfNote) = runCatching { dev.nori.music.ffi.perf.perfNote(t, note) }

    /** The output's underrun count, read now: the core notes it when it grew. A getter, no wakeup of its own. */
    private fun underruns(t: Long) {
        val opened = PlaybackService.track ?: return
        runCatching { opened.track.underrunCount }.getOrNull()?.let { note(t, PerfNote.Underruns(System.identityHashCode(opened.track).toLong(), it)) }
    }

    @Composable
    override fun Page() = PerfPage(this)

    /** The state a stretch is filed under (the core's `perf_state`). */
    private fun key(): String = dev.nori.music.ffi.perf.perfState(charging, screenOn, playing, foreground, playerOpen)

    /**
     * The settings that change what playing costs, in one line (the core's `perf_config`), the playback
     * path first: the one the service runs, which took the setting when it started; with no service yet,
     * the one it will start with.
     */
    private fun cfg(): String = dev.nori.music.ffi.perf.perfConfig(PlaybackService.engine)

    /** Something happened: when it moved the app into another state, one stretch ends and the next begins. */
    private fun changed() {
        // Only a screen that can be seen is held to the song heard (see the core's Watch::visible).
        dev.nori.music.ffi.perf.perfWatchVisible(System.currentTimeMillis(), foreground && screenOn)
        dev.nori.music.ffi.perf.perfWatchLook(System.currentTimeMillis())
        // The service may have started (or stopped) since, and with it the path that plays.
        settings = cfg()
        val key = key()
        if (start != null && key == startKey && settings == startCfg) return
        underruns(System.currentTimeMillis())
        val now = counters()
        start?.let { s -> stretch(s, now, live = false)?.let { dev.nori.music.ffi.perf.perfLogAdd(now.wallMs, it) } }
        begin(now, key)
    }

    private fun begin(now: PerfCounters, key: String) {
        start = now
        startKey = key
        startCfg = settings
        frames = 0; janky = 0; worstNs = 0
    }

    /** What the page shows, read again: the kept stretches and the one under way so far. */
    fun refresh() = handler.post {
        underruns(System.currentTimeMillis())
        val live = start?.let { stretch(it, counters(), live = true) }
        val page = dev.nori.music.ffi.perf.perfPage(live)
        main.post { shown.value = Shown(page, live) }
    }

    /**
     * The report, made on this thread (it reads every stretch kept) and handed to the system's share
     * sheet: the stretches as the page last read them, and the benchmarks' results.
     */
    fun share(context: Context, calls: String, covers: String) {
        val live = shown.value?.live
        handler.post {
            val device = PerfDevice(
                Build.MANUFACTURER, Build.MODEL, Build.DEVICE, Build.VERSION.RELEASE, Build.VERSION.SDK_INT,
                dev.nori.music.app.BuildConfig.VERSION_NAME, dev.nori.music.app.BuildConfig.GIT_SHA, dev.nori.music.app.BuildConfig.BUILD_TYPE,
            )
            val text = dev.nori.music.ffi.perf.perfReport(live, device, calls, covers, logs())
            main.post {
                context.startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text), null))
            }
        }
    }

    /** The page's log section, read now; folded again with [hideLog]. */
    fun showLog() = handler.post {
        val text = dev.nori.music.ffi.perf.perfLogText(logs())
        main.post { log.value = text; logOpen.value = true }
    }

    fun hideLog() { logOpen.value = false }

    /**
     * What logcat has for the app, read now: this process's last lines, and the crash buffer, which an app
     * reads for its own earlier processes too. A child process for each, only when asked for.
     */
    private fun logs() = PerfLogs(
        app = logcat("-d", "-v", "threadtime", "--pid=${Process.myPid()}", "-t", "400"),
        crash = logcat("-b", "crash", "-d", "-v", "threadtime", "-t", "200", failed = ""),
    )

    /**
     * logcat's answer to [args], or what went wrong ([failed] instead, when given). Its complaints are
     * not its answer: a crash buffer it would not give must not be kept as a crash.
     */
    private fun logcat(vararg args: String, failed: String? = null): String = runCatching {
        val p = ProcessBuilder("logcat", *args).redirectError(ProcessBuilder.Redirect.to(File("/dev/null"))).start()
        val out = p.inputStream.bufferedReader().use { it.readText() }
        check(p.waitFor() == 0) { "logcat exited with ${p.exitValue()}" }
        out
    }.getOrElse { failed ?: "logcat could not be read: $it" }

    /** "Start fresh": everything kept is forgotten, and the stretch under way starts again from now. */
    fun clear() = handler.post {
        dev.nori.music.ffi.perf.perfLogClear()
        begin(counters(), key())
        refresh()
    }

    fun runCalls() = bench(callBench) { dev.nori.music.app.Bench.calls() }

    fun runCovers() = bench(coverBench) { dev.nori.music.app.Bench.covers(app) }

    /** A benchmark takes seconds, so on a thread of its own; its cost lands in the stretch under way. */
    private fun bench(into: androidx.compose.runtime.MutableState<String>, body: () -> String) {
        into.value = words.running
        Thread({
            val result = runCatching(body).getOrElse { PerfWords.failed(it.toString()) }
            main.post { into.value = result }
        }, "bench").start()
    }

    /**
     * The difference between two readings, filed under the state that began at [a] (the core's
     * `perf_stretch`); none for a blink, unless it is the [live] one the page shows.
     */
    private fun stretch(a: PerfCounters, b: PerfCounters, live: Boolean): PerfStretch? =
        dev.nori.music.ffi.perf.perfStretch(startKey, startCfg, a, b, PerfFrames(frames, janky, worstNs), PlaybackService.offloadWanted, output(), live)

    /**
     * The AudioTrack the player last opened, as the platform describes it now, against what was asked of
     * it; none with no player. A track released since reads as what it last said, or not at all.
     */
    private fun output(): PerfOutput? = PlaybackService.track?.let(::output)

    private fun output(opened: OpenedTrack): PerfOutput? {
        val t = opened.track
        return runCatching {
            val device = t.routedDevice
            PerfOutput(
                engine = "rust", rate = t.sampleRate, channels = t.channelCount, encoding = t.audioFormat,
                askedBytes = opened.askedBytes.toLong(), sizeFrames = t.bufferSizeInFrames.toLong(),
                capacityFrames = t.bufferCapacityInFrames.toLong(), modeAsked = opened.askedMode, mode = t.performanceMode,
                offloaded = Build.VERSION.SDK_INT >= 29 && t.isOffloadedPlayback,
                deviceType = device?.type ?: 0, deviceName = device?.productName?.toString().orEmpty(),
                underruns = t.underrunCount, playState = t.playState,
                // Why it plays on the CPU, in the player's words.
                pcmWhy = PlaybackService.rustPlayer?.pcmWhy.orEmpty(),
            )
        }.getOrNull()
    }

    private fun batteryIntent(): Intent? = app.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))

    /**
     * Everything a stretch is measured by, read now. A few milliseconds, most of it the PSS and a file or
     * two per thread (its name and CPU time in `stat`, its wakeups in `status`).
     */
    private fun counters(): PerfCounters {
        val stat = runCatching { File("/proc/self/stat").readText().substringAfterLast(") ").split(' ') }.getOrNull()
        // utime and stime, fields 14 and 15 of the whole line: 11 and 12 once the name is cut off.
        val ticks = stat?.let { (it.getOrNull(11)?.toLongOrNull() ?: 0L) + (it.getOrNull(12)?.toLongOrNull() ?: 0L) } ?: 0L
        val threads = ArrayList<PerfThread>()
        File("/proc/self/task").listFiles()?.forEach { task ->
            val tid = task.name.toIntOrNull() ?: return@forEach
            runCatching {
                // The name is the one in brackets, which may itself hold spaces and brackets: up to the last one.
                val line = File(task, "stat").readText()
                val name = line.substringAfter('(').substringBeforeLast(')')
                val f = line.substringAfterLast(") ").split(' ')
                val cpu = ((f.getOrNull(11)?.toLongOrNull() ?: 0L) + (f.getOrNull(12)?.toLongOrNull() ?: 0L)) * msPerTick
                val switches = File(task, "status").useLines { lines ->
                    lines.firstOrNull { it.startsWith("voluntary_ctxt_switches") }?.substringAfter(':')?.trim()?.toLongOrNull()
                } ?: return@runCatching
                threads += PerfThread(tid, name, cpu.toLong(), switches)
            }
        }
        val memory = Debug.MemoryInfo().also { Debug.getMemoryInfo(it) }
        val b = batteryIntent()
        fun prop(id: Int) = battery.getIntProperty(id).takeIf { it != Int.MIN_VALUE && it != 0 }
        return PerfCounters(
            elapsedMs = SystemClock.elapsedRealtime(), wallMs = System.currentTimeMillis(),
            cpuMs = (ticks * msPerTick).toLong(), threads = threads,
            allocBytes = runtimeStat("art.gc.bytes-allocated"), gcs = runtimeStat("art.gc.gc-count"),
            pssKb = memory.totalPss.toLong(),
            chargeUah = prop(BatteryManager.BATTERY_PROPERTY_CHARGE_COUNTER)?.toLong(),
            capacityPct = battery.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY),
            gaugeUa = (prop(BatteryManager.BATTERY_PROPERTY_CURRENT_AVERAGE) ?: prop(BatteryManager.BATTERY_PROPERTY_CURRENT_NOW))?.toLong(),
            tempDeci = b?.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, 0) ?: 0,
            rxBytes = TrafficStats.getUidRxBytes(Process.myUid()).takeIf { it != TrafficStats.UNSUPPORTED.toLong() },
            txBytes = TrafficStats.getUidTxBytes(Process.myUid()).takeIf { it != TrafficStats.UNSUPPORTED.toLong() },
            memory = memory(memory),
        )
    }

    /**
     * Where the memory is (the core's `memory_line` says it): Android's app summary of the PSS just read,
     * the native heap's allocated bytes, and what of it is the app's own - the Rust side's (the core's
     * `perf_rust_memory`), the covers' Bitmaps and the moving cover's player.
     */
    private fun memory(m: Debug.MemoryInfo): PerfMemory {
        fun stat(name: String) = m.getMemoryStat("summary.$name")?.toLongOrNull() ?: 0L
        val covers = dev.nori.music.data.CoverLoader.get(app)
        return PerfMemory(
            javaKb = stat("java-heap"), nativeKb = stat("native-heap"), codeKb = stat("code"), stackKb = stat("stack"),
            graphicsKb = stat("graphics"), otherKb = stat("private-other"), systemKb = stat("system"),
            nativeAllocKb = Debug.getNativeHeapAllocatedSize() / 1024,
            coversKb = covers.keptBytes() / 1024, covers = covers.keptCount(),
            motion = dev.nori.music.playback.MotionPlayer.live,
            rust = runCatching { dev.nori.music.ffi.perf.perfRustMemory() }.getOrNull(),
        )
    }

    private fun runtimeStat(name: String) = Debug.getRuntimeStat(name)?.toLongOrNull() ?: 0L

}

/** Bytes of one frame of PCM [encoding] with [channels]; 0 for anything compressed. */
internal fun frameBytes(encoding: Int, channels: Int): Int = channels * when (encoding) {
    android.media.AudioFormat.ENCODING_PCM_16BIT -> 2
    android.media.AudioFormat.ENCODING_PCM_FLOAT -> 4
    android.media.AudioFormat.ENCODING_PCM_24BIT_PACKED -> 3
    android.media.AudioFormat.ENCODING_PCM_32BIT -> 4
    android.media.AudioFormat.ENCODING_PCM_8BIT -> 1
    else -> 0
}

/** The page as the core laid it out, and the stretch under way it was read with (for the report). */
internal class Shown(val page: PerfPage, val live: PerfStretch?)
