package dev.nori.music.downloads

import android.app.Notification
import dalvik.annotation.optimization.FastNative
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.res.Resources
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.cache.CacheDataSource
import androidx.media3.exoplayer.offline.DefaultDownloadIndex
import androidx.media3.exoplayer.offline.DefaultDownloaderFactory
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadManager
import androidx.media3.exoplayer.offline.DownloadRequest
import androidx.media3.exoplayer.offline.DownloadService
import androidx.media3.exoplayer.offline.Downloader
import androidx.media3.exoplayer.offline.DownloaderFactory
import androidx.media3.exoplayer.scheduler.Scheduler
import dalvik.annotation.optimization.CriticalNative
import dev.nori.music.Nori
import dev.nori.music.core.R
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.Core
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import dev.nori.music.ffi.transfers.DownloadKnown
import dev.nori.music.ffi.transfers.DownloadQueued
import dev.nori.music.ffi.model.Song
import dev.nori.music.playback.MediaSources
import dev.nori.music.settings.Settings
import dev.nori.music.text.Fmt
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors

/**
 * The downloads table as the screens read it: how many songs are downloaded and how many are still to
 * come, and whether one song is either - asked of the core's memory of the table, not copied here, so a
 * song finishing costs the same with ten downloads as with ten thousand. A new value whenever the table
 * changes. The songs themselves are only read when something asks for them, off the main thread.
 */
class DownloadState internal constructor(
    val doneCount: Int = 0,
    val pendingCount: Int = 0,
    private val version: Long = 0,
    private val songs: (Boolean) -> List<Song> = { emptyList() },
    private val ids: (Boolean) -> List<String> = { emptyList() },
) {
    val doneIds: Set<String> = Membership(DownloadsJni.DONE, doneCount)
    val pendingIds: Set<String> = Membership(DownloadsJni.PENDING, pendingCount)

    /** Every downloaded song, newest first: read from the core the first time it is asked, never on the main thread. */
    val done: List<Song> by lazy { songs(true) }
    /** Every song still to download, newest first; read as [done] is. */
    val pending: List<Song> by lazy { songs(false) }

    override fun equals(other: Any?) = other is DownloadState && other.version == version
    override fun hashCode() = version.hashCode()

    /**
     * One side of the table as a set: a lookup is one question to the core. Equal only to itself, so
     * nothing compares two of them song by song; a change to the table is a new [DownloadState].
     */
    private inner class Membership(private val kind: Int, override val size: Int) : AbstractSet<String>() {
        override fun contains(element: String) = size > 0 && DownloadsJni.held(element) == kind
        override fun iterator() = ids(kind == DownloadsJni.DONE).iterator()
        override fun equals(other: Any?) = this === other
        override fun hashCode() = System.identityHashCode(this)
    }
}

/**
 * media3 moves and stores the bytes; the index in Rust remembers what each file is.
 *
 * media3 keeps its own queue in [DefaultDownloadIndex], which outlives the process: a force stop
 * mid-download leaves it saying "queued" for every song that had not finished. Everything this class
 * shows - the marks, the batch the notification counts - is rebuilt from that queue and from what the
 * manager reports, never kept only in memory, and the service is started again at launch whenever the
 * index says something is unfinished (see [resume]).
 */
@UnstableApi
class Downloads(private val context: Context, private val coreOf: () -> Core, private val clientOf: () -> Client, lazySources: Lazy<MediaSources>, private val settings: Settings) {
    private val core get() = coreOf()
    /** Songs just downloaded get their lyrics looked up, one batch after another (`Client::lyrics_for_downloads`). */
    private val lyrics = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val lyricsInTurn = Mutex()
    private val sources by lazySources
    /** The index's bookkeeping. Downloads never run here: see [TrackedDownloaders]. */
    private val io = Executors.newFixedThreadPool(2)
    private val main = Handler(Looper.getMainLooper())
    private val _state = MutableStateFlow(DownloadState())
    val state: StateFlow<DownloadState> = _state

    private val _marks = MutableStateFlow<Map<String, DownloadMark>>(emptyMap())
    /**
     * The songs this session's downloads are doing something with: downloading, failed, or finished
     * lately. A pending song with no mark is waiting its turn. It changes when a download changes phase,
     * never with its progress, which each mark carries in a flow of its own - so a list watching this
     * map is not redrawn per percent. The phases and the bookkeeping are the core's
     * (crates/transfers/src/transfers.rs); this mirrors them for the screens.
     */
    val marks: StateFlow<Map<String, DownloadMark>> = _marks

    /** Each marked song's progress, 0..1 or negative while its size is unknown. */
    private val progress = ConcurrentHashMap<String, MutableStateFlow<Float>>()

    private fun progressOf(id: String) = progress.getOrPut(id) { MutableStateFlow(DownloadsJni.startFraction(id)) }

    val manager: DownloadManager by lazy {
        // media3's own wiring (DefaultDownloadIndex, DefaultDownloaderFactory over the download cache),
        // with each downloader wrapped so its progress reaches [progress]. media3 itself only tells
        // whoever asks for the list of downloads, and asking would mean polling.
        //
        // Runnable::run: each download copies its bytes on the thread media3 already gave it. Handing
        // them to a pool instead caps the downloads that actually move at the pool's size, whatever
        // maxParallelDownloads says - a pool of two was why "5 at once" moved two songs and left three
        // rings standing at nought.
        // What comes from the network is analysed as it downloads (MeasuringSink): AutoMix, the lyrics' sync and
        // the loudness of an untagged song read it later, and a song so measured is not read back from the disk.
        val measured = androidx.media3.datasource.DataSource.Factory {
            androidx.media3.datasource.TeeDataSource(sources.network.createDataSource(), dev.nori.music.playback.MeasuringSink())
        }
        val upstream = CacheDataSource.Factory().setCache(sources.downloadCache).setUpstreamDataSourceFactory(measured)
        DownloadManager(context, DefaultDownloadIndex(sources.database), TrackedDownloaders(DefaultDownloaderFactory(upstream, Runnable::run))).apply {
            // The queue runs in the order songs were asked for, this many at a time; a failure or a
            // cancel frees its slot for the next in line. DownloadWorker keeps it in step with the setting.
            maxParallelDownloads = parallel()
            addListener(object : DownloadManager.Listener {
                // The queue as media3 restored it from its index: the downloads a force stop interrupted
                // start again from where their bytes end, and the batch counts them from here.
                override fun onInitialized(m: DownloadManager) {
                    for (d in m.currentDownloads) follow(d)
                }

                override fun onDownloadChanged(m: DownloadManager, d: Download, e: Exception?) {
                    follow(d)
                    if (d.state == Download.STATE_COMPLETED) settle(d.request.id, true)
                }

                override fun onDownloadRemoved(m: DownloadManager, d: Download) {
                    val flags = DownloadsJni.removed(d.request.id)
                    progress.remove(d.request.id)
                    if (flags and DownloadsJni.MARKS != 0) refreshMarks()
                    settle(d.request.id, false)
                    if (flags and DownloadsJni.DRAINED != 0) summarise()
                }

                // Nothing left to download: the service stops itself next (this listener is told first). Saved songs
                // still being processed keep it, and its notification says what they wait for.
                override fun onIdle(m: DownloadManager) {
                    if (processingNow() != null) hold()
                }
            })
        }
    }

    /** How many songs download at once, from the setting (its range is the core's). */
    fun parallel() = settings.value.parallelDownloads

    /** Whether the queue has been handed back to media3 in this process; see [resume]. */
    @Volatile private var resumed = false

    init {
        io.execute {
            // Saved songs are read back from the download cache (their analysis, the beat model), whether or not
            // the playback service runs.
            dev.nori.music.playback.MeasureBridge.sources = sources
            runCatching { dev.nori.music.playback.MeasureJni.processStart() }.onFailure { Log.w(TAG, "downloads will not be read back", it) }
            publish(); reconcile()
        }
    }

    /** The table's counts, from the core's memory of it: nothing is read or copied, however many songs it holds. */
    private fun publish() {
        val n = core.downloadCounts()
        _state.value = DownloadState(n.done.toInt(), n.pending.toInt(), n.version.toLong(), ::songs, ::ids)
    }

    private fun songs(done: Boolean): List<Song> = runCatching { core.downloads(done) }.getOrDefault(emptyList())

    /** Every downloaded song, newest first, read from the core now (never on the main thread). */
    fun doneSongs(): List<Song> = songs(true)
    private fun ids(done: Boolean): List<String> = runCatching { core.downloadIds(done) }.getOrDefault(emptyList())

    /** Downloads that finished (true) or left the queue (false) and are not written down yet, in order. */
    private val settledIds = ArrayList<String>()
    private val settledDone = ArrayList<Boolean>()

    /**
     * Writes down that a download finished or went. They are gathered and written together, in one call
     * and one transaction, followed by one [publish]: stopping a library's worth of downloads reports each
     * song on its own, and writing each on its own was a call, a statement and a count per song.
     */
    private fun settle(id: String, finished: Boolean) {
        val first = synchronized(settledIds) {
            settledIds.add(id); settledDone.add(finished)
            settledIds.size == 1
        }
        if (first) io.execute(::writeSettled)
    }

    private fun writeSettled() {
        val (ids, done) = synchronized(settledIds) {
            (ArrayList(settledIds) to ArrayList(settledDone)).also { settledIds.clear(); settledDone.clear() }
        }
        // A finished download is the permanent copy; the streamed one is the same bytes twice, so it
        // goes. (A song streamed before it was downloaded lives in both.)
        for (i in ids.indices) if (done[i]) sources.dropStreamCopies(ids[i])
        runCatching { core.downloadSettle(ids, done) }.onFailure { Log.w(TAG, "could not write ${ids.size} settled downloads", it) }
        publish()
        val got = ids.filterIndexed { i, _ -> done[i] }
        if (got.isEmpty()) return
        // What each needs besides its lyrics - its analysis read back from the disk, the beat model - is the
        // core's, decided and started here, one song at a time on a thread of its own.
        runCatching { dev.nori.music.playback.MeasureJni.processSaved(got.toTypedArray()) }.onFailure { Log.w(TAG, "could not read ${got.size} downloads back", it) }
        watchMarks()
        // Each shows as processing until its lyrics, analysis and beats are over, or a step runs past its time.
        main.post { main.removeCallbacks(expire); expire.run() }
        lyrics.launch {
            // One song at a time, each marked as its lookup ends: the rows and the notification count down.
            lyricsInTurn.withLock {
                for (id in got) {
                    runCatching { clientOf().lyricsForDownloads(listOf(id)) }
                    main.post(::refreshMarks)
                }
            }
        }
    }

    /**
     * "Analyse downloaded songs": the downloads with no analysis of the current version, and with [beats] those
     * the beat model has not read, are read back as a download's are once saved, under the download service and
     * its notification. [done] hears how many, on the main thread.
     */
    fun analyse(beats: Boolean, done: (Int) -> Unit) = io.execute {
        val ids = runCatching { core.downloadUnanalysed(beats) }.getOrElse { Log.w(TAG, "could not list the downloads to analyse", it); emptyList() }
        val n = if (ids.isEmpty()) 0 else runCatching { dev.nori.music.playback.MeasureJni.processAnalyse(ids.toTypedArray()) }.getOrDefault(0)
        if (n > 0) {
            watchMarks()
            main.post {
                // Asked from the app's own screen: the service may start in the foreground, and is held for the work.
                runCatching { DownloadService.startForeground(context, DownloadWorker::class.java) }.onFailure { Log.w(TAG, "could not start the download service", it) }
                hold()
                refreshMarks()
                main.removeCallbacks(expire); expire.run()
            }
        }
        main.post { done(n) }
    }

    /** The coroutine following the core's marks while saved songs are processing; see [watchMarks]. */
    private var watching: kotlinx.coroutines.Job? = null

    /**
     * While saved songs are processing, the rows and the notification follow the work the core does on its own
     * threads: woken each time a mark moves (`download_marks_moved`), never by asking every so often, and done
     * once nothing is processing.
     */
    private fun watchMarks() = synchronized(this) {
        if (watching?.isActive == true) return@synchronized
        watching = lyrics.launch {
            while (true) {
                runCatching { dev.nori.music.ffi.transfers.downloadMarksMoved() }
                // Applied before waiting again: the marks read are what the next wait starts from.
                kotlinx.coroutines.suspendCancellableCoroutine { c -> main.post { refreshMarks(); c.resumeWith(Result.success(Unit)) } }
                if (processingNow() == null) break
            }
        }
    }

    /** What the saved songs are still waiting for, or null when nothing is processing. */
    internal fun processingNow() = runCatching { dev.nori.music.ffi.downloadProcessing(SystemClock.elapsedRealtime()) }.getOrNull()

    /** The download service while it runs; it holds on for songs still processing ([hold]). */
    @Volatile internal var worker: DownloadWorker? = null

    /**
     * Keeps the download service up for the songs still processing once nothing is left to download: a start
     * of its own is the newest, so the stop media3 asks for when it goes idle does not take (see
     * [DownloadWorker.onStartCommand]). The service is in the foreground as this is asked, so it may be started.
     */
    /** The service is held: its notification says what the saved songs wait for from now on. Main thread. */
    internal fun held() {
        // media3's last word may be on its notification: built and shown again.
        lastWorking = null
        lastWorkingWords = ""
        summarise()
    }

    internal fun hold() {
        runCatching { context.startService(Intent(context, DownloadWorker::class.java).setAction(ACTION_HOLD)) }
            .onFailure { Log.w(TAG, "could not keep the download service for the songs still processing", it) }
    }

    /** Ends the processing that has run its time (`download_processing_expire`), and comes back for the next. */
    private val expire = object : Runnable {
        override fun run() {
            val next = runCatching { dev.nori.music.ffi.transfers.downloadProcessingExpire(SystemClock.elapsedRealtime()) }.getOrDefault(-1)
            refreshMarks()
            if (next >= 0) main.postDelayed(this, next)
        }
    }

    /**
     * Picks up what an earlier process left unfinished. Asked at launch (from the application, and
     * again once there is a screen, since a process started in the background may not start services).
     * Costs one query of media3's table when something is pending and nothing at all otherwise.
     */
    fun resume() {
        if (!resumed) io.execute { if (!resumed) reconcile() }
    }

    /**
     * Brings the Rust index and media3's queue back into agreement after the process died. What each
     * song left pending needs is the core's (`download_recover`); this reads media3's table for it and
     * does what it says: the finished ones' streamed copies go, the failed ones show how far they got,
     * the lost ones are asked for again, and when anything is queued or was interrupted the download
     * service is started, which starts the manager, which restores and resumes them.
     */
    private fun reconcile() {
        val pending = _state.value.pendingIds
        if (pending.isEmpty()) { resumed = true; return }
        val known = runCatching {
            DefaultDownloadIndex(sources.database).getDownloads().use { c ->
                buildList {
                    while (c.moveToNext()) c.download.let { if (it.request.id in pending) add(DownloadKnown(it.request.id, it.state, it.contentLength, it.bytesDownloaded)) }
                }
            }
        }.getOrDefault(emptyList())
        val r = runCatching { core.downloadRecover(known) }.getOrElse { Log.w(TAG, "could not recover the queue", it); return }
        for (id in r.finished) sources.dropStreamCopies(id)
        if (r.finished.isNotEmpty()) publish()
        for (f in r.failed) progress.getOrPut(f.id) { MutableStateFlow(f.progress) }
        if (r.failed.isNotEmpty()) main.post { refreshMarks() }
        if (!r.unfinished && r.lost.isEmpty()) { resumed = true; return }
        Log.i(TAG, "resuming: ${pending.size} pending, ${r.lost.size} asked for again, ${r.failed.size} failed")
        main.post {
            resumed = runCatching {
                // The first intent brings the service - and with it the manager and its restored queue - up.
                if (r.lost.isEmpty()) DownloadService.start(context, DownloadWorker::class.java)
                for (id in r.lost) DownloadService.sendAddDownload(context, DownloadWorker::class.java, request(id), false)
            }.onFailure { Log.w(TAG, "could not start the download service yet", it) }.isSuccess
        }
    }

    /**
     * Hands one download's state to the core, which keeps the batch and the phases, and follows what it
     * says: a new batch takes the last one's result away, the phases changed, or the batch is over and
     * says how it went. Runs on the main thread, where media3 reports.
     */
    private fun follow(d: Download) {
        val id = d.request.id
        val flags = DownloadsJni.followed(id, d.state, SystemClock.elapsedRealtime())
        if (flags and DownloadsJni.NEW_BATCH != 0) cancelResult()
        when (d.state) {
            Download.STATE_DOWNLOADING -> progressOf(id)
            Download.STATE_COMPLETED -> progress.getOrPut(id) { MutableStateFlow(1f) }.value = 1f
        }
        if (flags and DownloadsJni.MARKS != 0) refreshMarks()
        if (flags and DownloadsJni.DRAINED != 0) summarise()
    }

    /**
     * The core's phases as the screens read them, each with its song's progress flow. Only the marks that
     * moved since the last time come over; a song that lost its mark and is not waiting any more also
     * loses its progress. Runs on the main thread, the only one that applies them.
     */
    private fun refreshMarks() {
        val m = dev.nori.music.ffi.transfers.downloadMarksChanged()
        if (m.ids.isEmpty()) return
        val next = HashMap(_marks.value)
        for (i in m.ids.indices) {
            val id = m.ids[i]
            val phase = DownloadPhase.entries.getOrNull(m.phases[i]).takeIf { m.phases[i] > 0 }
            if (phase != null) next[id] = DownloadMark(phase, progressOf(id), m.at[i])
            else if (next.remove(id) != null && DownloadsJni.held(id) != DownloadsJni.PENDING) progress.remove(id)
        }
        _marks.value = next
        if (summaryWaits || next.values.any { it.phase.processing }) summarise()
    }

    private fun unmark(ids: Collection<String>) {
        var changed = false
        for (id in ids) { progress.remove(id); if (DownloadsJni.unmark(id) and DownloadsJni.MARKS != 0) changed = true }
        if (changed) main.post { refreshMarks() }
    }

    /**
     * Queues what is not downloaded yet. Which songs are new and which are asked for again is the core's
     * (`download_queue`); one asked again that media3 is still working on is left to it.
     */
    fun download(songs: List<Song>, beats: Boolean = false) = io.execute { queued(runCatching { core.downloadQueue(songs) }, beats) }

    /** Every song of the offline index, in one call to the core; sync the library first so the index is complete. */
    fun downloadLibrary() = io.execute {
        // No question for a whole library: the beat model reads it only when "ML beats for downloads" says always.
        val beats = runCatching { dev.nori.music.ffi.downloadBeatsOffer() }.getOrNull() == dev.nori.music.ffi.transfers.BeatsOffer.YES
        queued(runCatching { core.downloadQueueLibrary() }, beats)
    }

    /** [beats]: the beat model reads these songs once they are saved, written down before any of them can finish. */
    private fun queued(result: Result<DownloadQueued>, beats: Boolean) {
        val q = result.getOrElse { Log.w(TAG, "could not queue downloads", it); return }
        if (beats) runCatching { core.downloadWantBeats(q.fresh + q.again) }.onFailure { Log.w(TAG, "could not ask for the beat model", it) }
        if (q.fresh.isNotEmpty()) publish()
        val requests = q.fresh.map(::request)
        val retries = q.again.map(::request)
        main.post {
            for (r in requests) add(r)
            if (retries.isEmpty()) return@post
            // Listed once: media3 hands out a fresh copy of its list each time it is asked.
            val working = manager.currentDownloads.mapTo(HashSet()) { it.request.id }
            for (r in retries) if (r.id !in working) { unmark(listOf(r.id)); add(r) }
        }
    }

    /** Sends failed downloads round again; they rejoin the queue at its end. */
    fun retry(songs: List<Song>) {
        if (songs.isEmpty()) return
        unmark(songs.map { it.id })
        io.execute {
            val q = runCatching { core.downloadQueue(songs) }.getOrElse { Log.w(TAG, "could not retry downloads", it); return@execute }
            publish()
            val requests = (q.fresh + q.again).map(::request)
            main.post { requests.forEach(::add) }
        }
    }

    private fun add(r: DownloadRequest) {
        runCatching { DownloadService.sendAddDownload(context, DownloadWorker::class.java, r, false) }
            .onFailure { Log.w(TAG, "could not queue ${r.id}", it) }
    }

    /** What the song is and how much it should weigh the core knows from its own downloads table. */
    private fun request(id: String): DownloadRequest =
        DownloadRequest.Builder(id, Uri.parse(sources.downloadUrl(id))).setCustomCacheKey(sources.downloadKey(id)).build()

    fun remove(ids: List<String>) = ids.forEach { DownloadService.sendRemoveDownload(context, DownloadWorker::class.java, it, false) }

    /**
     * Stops downloads that have not finished and forgets them. The index entry goes at once as well as
     * through media3's callback: a song left pending by an earlier session may have no media3 download
     * to remove, and would otherwise sit in the queue for ever.
     */
    fun cancel(ids: List<String>) {
        if (ids.isEmpty()) return
        unmark(ids)
        remove(ids)
        for (id in ids) settle(id, false)
    }

    /**
     * Everything not yet downloaded, in one go: the core takes it out of the table and forgets its marks
     * in one call and says which songs they were, and they go straight to the manager rather than one
     * service intent per song, which for a whole library queued would be thousands. Finished downloads
     * stay, which media3's own "remove all" would not leave.
     */
    fun cancelAll() = io.execute {
        val ids = runCatching { core.downloadCancelAll() }.getOrElse { Log.w(TAG, "could not stop the downloads", it); return@execute }
        if (ids.isEmpty()) return@execute
        for (id in ids) progress.remove(id)
        publish()
        main.post {
            refreshMarks()
            for (id in ids) manager.removeDownload(id)
        }
    }

    /**
     * The download notification while a batch runs: where it is ("Downloading: 12 of 49"), the song
     * in flight, how fast the bytes arrive and how long they should take, and a bar over the whole
     * batch that moves with the bytes of the songs in flight. The service asks once a second;
     * nothing else redraws it. The batch's aggregate speed is published for the downloads screen.
     */
    internal fun progressNotification(context: Context, downloads: List<Download>, notMetRequirements: Int): Notification {
        // The facts and the bar are the core's, the words these; asked once a second, it says whether the
        // facts changed, and a notification whose words and bar did not is handed back as it was rather
        // than built again.
        when (DownloadsJni.notice(downloads.size, notMetRequirements, SystemClock.elapsedRealtime())) {
            0 -> lastProgress?.let { return it }
            // The bytes are in and saved songs are still processing: the service stays for them, and says so.
            2 -> processingNow()?.let { return workingNotification(it) } ?: return complete ?: NotificationCompat.Builder(context, CHANNEL)
                // Nothing left: the service is on its way out, and the batch's own summary (its own id,
                // so stopping the service does not take it) says how it went. No bar here.
                .setSmallIcon(android.R.drawable.stat_sys_download_done)
                .setContentTitle(context.getString(R.string.notice_complete))
                .setContentIntent(openDownloads(context))
                .setAutoCancel(true)
                .setOnlyAlertOnce(true)
                .setSilent(true)
                .setShowWhen(false)
                .build().also { complete = it }
        }
        val names = DownloadsJni.noticeFacts(noticeFacts) ?: "\n"
        val cut = names.indexOf('\n')
        val (title, text) = noticeWords(context.resources, names.substring(0, cut), names.substring(cut + 1), noticeFacts)
        val permille = noticeFacts[3].toInt()
        lastProgress?.let { if (title == lastTitle && text == lastText && permille == lastPermille) return it }
        lastTitle = title; lastText = text; lastPermille = permille
        return NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle(title)
            .setContentText(text.ifEmpty { null })
            .setProgress(1000, permille, false)
            .setContentIntent(openDownloads(context))
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, context.getString(R.string.notice_cancel), cancelIntent(context))
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setShowWhen(false)
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .build().also { lastProgress = it }
    }

    private var lastProgress: Notification? = null
    private var lastTitle = ""
    private var lastText = ""
    private var lastPermille = -1
    /** The notification's facts, `[kind, position, total, permille, speed_bps, eta_s]`. */
    private val noticeFacts = LongArray(6)
    private var complete: Notification? = null

    /**
     * The notification's title and text from the core's facts: which title applies ([facts] `[0]`: 0
     * waiting for a network, 1 one song named [current], 2 one song, 3 "12 of 49"), then the song in
     * flight, the batch's album, how fast and how long, each only when there is one.
     */
    private fun noticeWords(res: Resources, current: String, album: String, facts: LongArray): Pair<String, String> {
        val title = when (facts[0].toInt()) {
            0 -> res.getString(R.string.notice_waiting)
            1 -> res.getString(R.string.notice_downloading_named, current)
            2 -> res.getString(R.string.notice_downloading_one)
            else -> res.getString(R.string.notice_downloading_of, facts[1].toInt(), facts[2].toInt())
        }
        val text = StringBuilder()
        fun part(f: (StringBuilder) -> Unit) {
            val start = text.length
            if (start > 0) text.append(" · ")
            val mark = text.length
            f(text)
            if (text.length == mark) text.setLength(start)
        }
        if (current.isNotEmpty()) part { it.append(current) }
        if (album.isNotEmpty()) part { it.append(res.getString(R.string.notice_album, album)) }
        part { Fmt.appendSpeed(it, facts[4]) }
        part { appendEta(res, it, facts[5]) }
        return title to text.toString()
    }

    /**
     * What the downloads say once their bytes are in, asked again as the marks change. While saved songs are
     * still finding their lyrics, being analysed or having their beats read, the notification says which and
     * how many, counting down song by song: the held service's own ([DownloadWorker]), or one of its own if the
     * service could not be kept. Then the service goes, and how the batch went is said under its own id (the
     * service takes the progress one with it when it stops): all done, a quiet line that goes by itself;
     * something failed, it stays, and a tap shows which.
     */
    private fun summarise() {
        val nm = context.getSystemService(NotificationManager::class.java) ?: return
        val p = processingNow()
        if (p != null) {
            summaryWaits = true
            val w = worker
            // Still downloading: the progress notification is media3's, asked every second.
            if (w != null && !manager.isIdle) return
            val on = if (w?.holding == true) DOWNLOAD_NOTIFICATION else DOWNLOAD_RESULT_NOTIFICATION
            val shown = lastWorking
            val n = workingNotification(p)
            if (n === shown && on == workingOn) return
            if (workingOn != 0 && workingOn != on && workingOn == DOWNLOAD_RESULT_NOTIFICATION) nm.cancel(DOWNLOAD_RESULT_NOTIFICATION)
            workingOn = on
            runCatching { nm.notify(on, n) }
            return
        }
        val wasWorking = summaryWaits
        summaryWaits = false
        workingOn = 0
        // Nothing is processing: the service goes, and its notification with it.
        worker?.release()
        val facts = IntArray(4)
        val album = DownloadsJni.summary(facts)
        if (album == null) {
            if (wasWorking) nm.cancel(DOWNLOAD_RESULT_NOTIFICATION)
            return
        }
        val res = context.resources
        val (done, failedCount) = facts[2] to facts[3]
        val failed = failedCount > 0
        val title = when (facts[0]) {
            0 -> res.getQuantityString(R.plurals.summary_failed, failedCount, failedCount)
            1 -> res.getString(R.string.summary_album, album)
            else -> res.getQuantityString(R.plurals.summary_downloaded, done, done)
        }
        val text = when (facts[1]) {
            1 -> res.getString(R.string.summary_some_failed, done)
            2 -> res.getString(R.string.summary_try_again)
            else -> null
        }
        val b = NotificationCompat.Builder(context, CHANNEL)
            .setContentTitle(title)
            .setContentIntent(openDownloads(context))
            .setAutoCancel(true)
            .setSilent(true)
            .setOnlyAlertOnce(true)
            .setSmallIcon(if (failed) android.R.drawable.stat_notify_error else android.R.drawable.stat_sys_download_done)
            .setContentText(text)
        if (!failed) b.setTimeoutAfter(RESULT_TIMEOUT_MS)
        runCatching { nm.notify(DOWNLOAD_RESULT_NOTIFICATION, b.build()) }
    }

    /**
     * The notification while saved songs are processing: what most of them wait for as the title ("Finding lyrics
     * for 3 songs…", "Analysing 2 songs…", "Detecting beats for 5 songs…"), the rest and the time left below. The
     * same one is handed back while its words are.
     */
    private fun workingNotification(p: dev.nori.music.ffi.transfers.Processing): Notification {
        val res = context.resources
        val title = when {
            p.lyrics > 0 -> res.getQuantityString(R.plurals.summary_finding_lyrics, p.lyrics, p.lyrics)
            p.analysing > 0 -> res.getQuantityString(R.plurals.summary_analysing, p.analysing, p.analysing)
            else -> res.getQuantityString(R.plurals.summary_detecting_beats, p.beats, p.beats)
        }
        val text = StringBuilder()
        fun part(f: (StringBuilder) -> Unit) {
            val start = text.length
            if (start > 0) text.append(" · ")
            val mark = text.length
            f(text)
            if (text.length == mark) text.setLength(start)
        }
        if (p.lyrics > 0 && p.analysing > 0) part { it.append(res.getQuantityString(R.plurals.processing_analysing, p.analysing, p.analysing)) }
        if ((p.lyrics > 0 || p.analysing > 0) && p.beats > 0) part { it.append(res.getQuantityString(R.plurals.processing_beats, p.beats, p.beats)) }
        part { appendEta(res, it, p.etaS) }
        val words = title + "\n" + text
        lastWorking?.let { if (words == lastWorkingWords) return it }
        lastWorkingWords = words
        return NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle(title)
            .setContentText(text.ifEmpty { null })
            .setProgress(0, 0, true)
            .setContentIntent(openDownloads(context))
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setShowWhen(false)
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
            .build().also { lastWorking = it }
    }

    /** Saved songs are processing and the notification says so: [summarise] again as the marks change. Main thread. */
    private var summaryWaits = false
    /** The processing notification last built, its words, and the id it was last shown under (0 none). */
    private var lastWorking: Notification? = null
    private var lastWorkingWords = ""
    private var workingOn = 0

    /** A new batch starting takes the last one's result away: the progress notification replaces it. */
    private fun cancelResult() {
        summaryWaits = false
        context.getSystemService(NotificationManager::class.java)?.cancel(DOWNLOAD_RESULT_NOTIFICATION)
    }

    /**
     * Reports each downloader's chunks to the core against a slot of its own - three numbers a chunk,
     * nothing looked up or allocated - and passes on only the progress the core says is worth drawing.
     */
    private inner class TrackedDownloaders(private val inner: DownloaderFactory) : DownloaderFactory {
        override fun createDownloader(request: DownloadRequest): Downloader {
            val downloader = inner.createDownloader(request)
            val flow = progressOf(request.id)
            return object : Downloader {
                override fun download(listener: Downloader.ProgressListener?) {
                    val slot = DownloadsJni.open(request.id, SystemClock.elapsedRealtime())
                    downloader.download { length, bytes, percent ->
                        listener?.onProgress(length, bytes, percent)
                        val f = DownloadsJni.note(slot, length, bytes, SystemClock.elapsedRealtime())
                        if (!f.isNaN()) flow.value = f
                    }
                }
                override fun cancel() = downloader.cancel()
                override fun remove() = downloader.remove()
            }
        }
    }

    companion object {
        internal const val TAG = "noridl"
        /** How long a batch's result stays when nothing failed; one that failed stays until tapped. */
        private const val RESULT_TIMEOUT_MS = 8_000L
    }
}

/** The core's side of the downloads; see crates/transfers/src/transfers.rs. */
internal object DownloadsJni {
    init { System.loadLibrary("norimusic") }

    const val NEW_BATCH = 1
    const val DRAINED = 2
    const val MARKS = 4

    /** What [held] says of a song. */
    const val PENDING = 1
    const val DONE = 2

    /** Whether a song is in the downloads table: 0 no, [PENDING] queued or failed, [DONE] downloaded. */
    @JvmStatic @FastNative external fun held(id: String): Int
    @JvmStatic external fun followed(id: String, state: Int, now: Long): Int
    @JvmStatic external fun removed(id: String): Int
    @JvmStatic external fun unmark(id: String): Int
    @JvmStatic external fun startFraction(id: String): Float
    @JvmStatic external fun open(id: String, now: Long): Int
    /** Per chunk: the progress to show, or NaN when it has not moved enough to draw. */
    @JvmStatic @CriticalNative external fun note(slot: Int, length: Long, bytes: Long, now: Long): Float
    /** 0 unchanged, 1 changed, 2 the batch is over. */
    @JvmStatic @CriticalNative external fun notice(listed: Int, waiting: Int, now: Long): Int
    /**
     * The notification's facts, `[kind, position, total, permille, speed_bps, eta_s]` into [out], and the
     * song in flight's title and the batch's album as "title\nalbum": one crossing for all of it.
     */
    @JvmStatic @FastNative external fun noticeFacts(out: LongArray): String?
    /**
     * How the batch went, `[title, text, done, failed]` into [out] (title 0 failed, 1 an album, 2 downloaded;
     * text 0 none, 1 some failed, 2 try again), and the album; null when there is nothing to say.
     */
    @JvmStatic @FastNative external fun summary(out: IntArray): String?
}

/**
 * The downloads screen's lines, worded here from the core's facts (crates/transfers/src/transfers.rs). A
 * running row's line is asked whenever its ring moves and the summary once a second: one JNI call each,
 * numbers into an array kept for it, the words from string resources. Main thread only.
 */
object DownloadLines {
    private val facts = LongArray(4)
    private val out = StringBuilder(64)

    /** A song's second line: its artist, then, while it runs, "45% · 2.1 MB/s · 1:20 left". */
    fun row(res: Resources, id: String): String {
        val artist = DownloadFacts.row(id, facts) ?: return ""
        if (facts[0] == 0L) return artist
        out.setLength(0)
        out.append(artist)
        if (facts[1] >= 0) out.append(" · ").append(facts[1]).append('%')
        dotted { Fmt.appendSpeed(it, facts[2]) }
        dotted { appendEta(res, it, facts[3]) }
        return out.toString()
    }

    /** "2 downloading · 14 waiting · 1 failed · 3.2 MB/s · 12:34 left", or "Nothing downloading". */
    fun summary(res: Resources, active: Int, queued: Int, failed: Int): String {
        out.setLength(0)
        if (active > 0) out.append(res.getString(R.string.downloads_active, active))
        if (queued > 0) part { it.append(res.getString(R.string.downloads_waiting, queued)) }
        if (failed > 0) part { it.append(res.getString(R.string.downloads_failed, failed)) }
        if (active > 0) {
            DownloadFacts.speedEta(facts)
            part { Fmt.appendSpeed(it, facts[0]) }
            part { appendEta(res, it, facts[1]) }
        }
        if (out.isEmpty()) return res.getString(R.string.downloads_nothing)
        return out.toString()
    }

    /** Adds `" · "` and what [f] writes, or nothing when it writes nothing. */
    private inline fun dotted(f: (StringBuilder) -> Unit) {
        val start = out.length
        out.append(" · ")
        val mark = out.length
        f(out)
        if (out.length == mark) out.setLength(start)
    }

    /** Adds `" · "` and what [f] writes, or nothing when it writes nothing (the first part has no dot). */
    private inline fun part(f: (StringBuilder) -> Unit) {
        val start = out.length
        if (start > 0) out.append(" · ")
        val mark = out.length
        f(out)
        if (out.length == mark) out.setLength(start)
    }
}

/** "45 s left", "12:34 left", "2:05:00 left" onto [out]; nothing when it cannot be said (negative). */
internal fun appendEta(res: Resources, out: StringBuilder, sec: Long) {
    when {
        sec < 0 -> Unit
        sec < 60 -> out.append(res.getString(R.string.eta_seconds, sec.toInt()))
        else -> out.append(res.getString(R.string.eta_clock, StringBuilder(10).also { Fmt.appendClock(it, sec, false) }))
    }
}

/** The core's facts for the downloads screen (crates/android/src/transfers.rs). */
internal object DownloadFacts {
    init { System.loadLibrary("norimusic") }

    /** A song's artist (null when the id is null), and `[running, percent, speed_bps, eta_s]` into [out]. */
    @JvmStatic @FastNative external fun row(id: String, out: LongArray): String?
    /** The batch's `[speed_bps, eta_s]` into [out]. */
    @JvmStatic @FastNative external fun speedEta(out: LongArray)
}

/** Asks the app to open on its downloads screen. The activity answers it; the core only names it. */
const val ACTION_OPEN_DOWNLOADS = "dev.nori.music.OPEN_DOWNLOADS"

/** The notification's Cancel: everything not yet downloaded leaves the queue, finished songs stay. */
private const val ACTION_CANCEL_DOWNLOADS = "dev.nori.music.CANCEL_DOWNLOADS"

/** Keeps the download service for the saved songs still processing ([Downloads.hold]). */
private const val ACTION_HOLD = "dev.nori.music.HOLD_DOWNLOADS"

private const val CHANNEL = "downloads"

@Volatile private var openDownloadsIntent: PendingIntent? = null

/**
 * Where a tap on a download notification goes: the app's launcher activity, asked to show its
 * downloads. The action differs from the launcher's own, so a running app is handed the request
 * (onNewIntent) instead of only being brought to the front.
 */
fun openDownloads(context: Context): PendingIntent? = openDownloadsIntent ?: run {
    val launcher = context.packageManager.getLaunchIntentForPackage(context.packageName)?.component ?: return null
    val intent = Intent(ACTION_OPEN_DOWNLOADS).setComponent(launcher)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    PendingIntent.getActivity(context, 1, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        .also { openDownloadsIntent = it }
}

@UnstableApi
private fun cancelIntent(context: Context): PendingIntent =
    PendingIntent.getService(
        context, 2, Intent(context, DownloadWorker::class.java).setAction(ACTION_CANCEL_DOWNLOADS),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

/**
 * The progress of a download batch, for as long as the service runs. Not 1001: that is media3's
 * playback notification id, and sharing it replaced the now-playing notification while downloading.
 */
const val DOWNLOAD_NOTIFICATION = 2001

/** How the batch went, once it has: a separate id, so the service stopping does not take it away. */
const val DOWNLOAD_RESULT_NOTIFICATION = 2002

@UnstableApi
class DownloadWorker : DownloadService(DOWNLOAD_NOTIFICATION, 1000L, CHANNEL, androidx.media3.exoplayer.R.string.exo_download_notification_channel_name, 0) {
    override fun getDownloadManager(): DownloadManager = Nori.get(this).downloads.manager
    override fun getScheduler(): Scheduler? = null

    /**
     * The start that holds the service for the saved songs still processing, 0 when none does. media3 stops the
     * service once nothing is left to download with the id of the last start it was handed; a start it is never
     * handed is newer, and that stop does not take. [release] stops it with this one when the work is over.
     */
    private var holdId = 0

    internal val holding get() = holdId != 0

    /**
     * The CPU lock, held exactly while there is work: songs downloading (the manager is not idle, which it also is
     * while it waits for the network) or saved songs processing under the hold. A foreground service alone does
     * not keep the CPU up, and with the screen off the analysis stood still until the phone woke.
     */
    @Suppress("DEPRECATION")
    private val wakeLock by lazy { getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nori:downloads").apply { setReferenceCounted(false) } }

    private val working = object : DownloadManager.Listener {
        override fun onInitialized(m: DownloadManager) = awake()
        override fun onDownloadChanged(m: DownloadManager, d: Download, e: Exception?) = awake()
        override fun onIdle(m: DownloadManager) = awake()
        override fun onDownloadsPausedChanged(m: DownloadManager, paused: Boolean) = awake()
        override fun onWaitingForRequirementsChanged(m: DownloadManager, waiting: Boolean) = awake()
    }

    /** Takes or lets go of the CPU lock for what is going on now. Main thread. */
    private fun awake(done: Boolean = false) {
        val hold = !done && (holding || !getDownloadManager().isIdle)
        if (hold == wakeLock.isHeld) return
        if (hold) wakeLock.acquire() else wakeLock.release()
    }

    override fun onCreate() {
        super.onCreate()
        Nori.get(this).downloads.worker = this
        getDownloadManager().addListener(working)
        awake()
    }

    override fun onDestroy() {
        getDownloadManager().removeListener(working)
        awake(done = true)
        Nori.get(this).downloads.let { if (it.worker === this) it.worker = null }
        super.onDestroy()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val downloads = Nori.get(this).downloads
        if (intent?.action == ACTION_HOLD) {
            holdId = startId
            // Over already (a quick one): let go at once; otherwise its notification says what is left.
            if (downloads.processingNow() == null) release() else { awake(); downloads.held() }
            return START_NOT_STICKY
        }
        // Held: a start that finds nothing to download would stop the service under the songs still processing,
        // so a newer hold goes first.
        if (holdId != 0 && downloads.processingNow() != null) downloads.hold()
        if (intent?.action == ACTION_CANCEL_DOWNLOADS) {
            downloads.cancelAll()
            return super.onStartCommand(Intent(intent).setAction(DownloadService.ACTION_INIT), flags, startId)
        }
        return super.onStartCommand(intent, flags, startId)
    }

    /** The saved songs are processed: the hold goes, and the service with it unless downloads came meanwhile. */
    internal fun release() {
        val id = holdId
        if (id == 0) return
        holdId = 0
        awake()
        stopSelfResult(id)
    }

    /**
     * Asked once a second while anything downloads (media3 throttles it to the interval above), on the
     * main thread the manager lives on - which also makes it the place to follow a change to
     * "Downloads at once" without a listener of its own.
     */
    override fun getForegroundNotification(downloads: MutableList<Download>, notMetRequirements: Int): Notification {
        val all = Nori.get(this).downloads
        all.parallel().let { if (all.manager.maxParallelDownloads != it) all.manager.maxParallelDownloads = it }
        return all.progressNotification(this, downloads, notMetRequirements)
    }
}
