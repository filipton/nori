package dev.nori.music.update

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.provider.Settings
import dev.nori.music.NoriLog
import dev.nori.music.ffi.AppUpdate
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.UpdateCheck
import dev.nori.music.ffi.updateSkip
import dev.nori.music.net.Http
import dev.nori.music.net.lifted
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Request
import okhttp3.Response
import java.io.File
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/**
 * The app updating itself from its GitHub releases. Whether there is a newer release, which APK is this
 * phone's and whether to say so are the core's (nori-core update.rs); this asks it, downloads the APK into
 * the cache over the app's one OkHttp pool, and hands it to Android's PackageInstaller. The words are the
 * app's: [State] only says where things stand.
 *
 * Asked when the app starts (the core does nothing unless a day has passed and the switch is on) and from
 * About's button; nothing polls. Only a release build installs ([installs]): a debug or perf build is signed
 * or named differently, so it says a newer version is out and offers the release's page instead.
 */
class Updates(private val context: Context, private val http: () -> Http, private val client: () -> Client) {
    sealed interface State {
        data object Idle : State
        data object Checking : State
        data class UpToDate(val latest: String) : State
        /** [skipped]: the user said "Later" to it; About still shows it, the banner does not. */
        data class Available(val update: AppUpdate, val skipped: Boolean) : State
        /** Newer, but with no APK this phone can run: only its page is offered. */
        data class NoApk(val version: String, val page: String) : State
        data class CheckFailed(val error: Throwable) : State
        /** [done] of [total] bytes. */
        data class Downloading(val update: AppUpdate, val done: Long, val total: Long) : State
        /** Handed to Android, which may be asking the user. */
        data class Installing(val update: AppUpdate) : State
        /** Android wants the user to allow this app to install apps first; the APK is kept for when they have. */
        data class NeedsPermission(val update: AppUpdate) : State
        data class Failed(val update: AppUpdate, val why: Failure) : State
    }

    /** Why an update did not go on. */
    sealed interface Failure {
        /** The download did not complete; [error] says how. */
        data class Download(val error: Throwable) : Failure
        /** The file that came is not the size GitHub stated. */
        data class Size(val got: Long, val expected: Long) : Failure
        /** The file is not a newer build of this app. */
        data object NotThisApp : Failure
        /**
         * Android refused it: [status] is PackageInstaller's (STATUS_FAILURE_CONFLICT for a copy signed with another
         * key, _STORAGE, _INCOMPATIBLE, _BLOCKED, ...; STATUS_FAILURE for one that never reached it), [message]
         * Android's own words, for the log.
         */
        data class Install(val status: Int, val message: String?) : Failure {
            /** Signed with another key than the release (a build of one's own): trying again cannot help. */
            val conflict get() = status == PackageInstaller.STATUS_FAILURE_CONFLICT
        }
    }

    private val _state = MutableStateFlow<State>(State.Idle)
    val state: StateFlow<State> = _state

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var job: Job? = null

    @Volatile private var version: String = ""
    /** Whether this build may install a release over itself: only a release build, set by the app. */
    @Volatile var installs: Boolean = false
        private set

    /** This build's version and whether it installs releases; the app says so once, when it starts. */
    fun configure(version: String, installs: Boolean) {
        this.version = version
        this.installs = installs
    }

    private val dir get() = File(context.cacheDir, DIR)

    /** The daily check, off the UI thread; the core says whether it is due. Called when the app starts. */
    fun checkIfDue() = check(asked = false)

    /** The button: asked now, whatever the day and the switch, and a version put off is shown again. */
    fun checkNow() = check(asked = true)

    private fun check(asked: Boolean) {
        if (version.isEmpty() || busy()) return
        job = scope.launch {
            if (asked) _state.value = State.Checking
            val abis = Build.SUPPORTED_ABIS.toList()
            val found = runCatching { lifted { client().updateCheck(asked, version, abis) } }
            _state.value = found.fold(
                onSuccess = { c ->
                    when (c) {
                        is UpdateCheck.NotDue -> _state.value
                        is UpdateCheck.UpToDate -> State.UpToDate(c.latest)
                        is UpdateCheck.Available -> State.Available(c.update, c.skipped)
                        is UpdateCheck.NoApk -> State.NoApk(c.version, c.page)
                    }
                },
                onFailure = { e ->
                    NoriLog.w("update check: ${e.message}")
                    if (asked) State.CheckFailed(e) else _state.value
                },
            )
            // An APK left from an update that has since gone on (or was given up) is only taking space.
            if (_state.value !is State.Available) dir.deleteRecursively()
        }
    }

    private fun busy() = job?.isActive == true || _state.value is State.Installing

    /** "Later": this version is not brought up by itself again; a newer one will be. */
    fun later() {
        job?.cancel()
        val s = _state.value
        val update = when (s) {
            is State.Available -> s.update
            is State.Downloading -> s.update
            is State.NeedsPermission -> s.update
            is State.Failed -> s.update
            else -> null
        } ?: return
        scope.launch { updateSkip(update.version) }
        _state.value = State.Available(update, skipped = true)
    }

    /** A download stopped half way: back to the offer, nothing put off. */
    fun cancel() {
        val s = _state.value as? State.Downloading ?: return
        job?.cancel()
        dir.deleteRecursively()
        _state.value = State.Available(s.update, skipped = false)
    }

    /**
     * "Update": the APK downloaded (unless it already is), checked, and handed to Android. Asks for the
     * permission to install apps first when this app does not have it yet.
     */
    fun update(update: AppUpdate) {
        if (!installs || busy()) return
        job = scope.launch {
            val apk = File(dir, update.apkName)
            if (!(apk.isFile && apk.length() == update.apkBytes.toLong())) {
                _state.value = State.Downloading(update, 0, update.apkBytes.toLong())
                try {
                    download(update, apk)
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Exception) {
                    NoriLog.w("update download: ${e.message}")
                    _state.value = State.Failed(update, if (e is WrongSize) Failure.Size(e.got, e.expected) else Failure.Download(e))
                    return@launch
                }
            }
            if (!ours(apk)) {
                apk.delete()
                _state.value = State.Failed(update, Failure.NotThisApp)
                return@launch
            }
            if (!canInstall()) {
                _state.value = State.NeedsPermission(update)
                allowInstalls()
                return@launch
            }
            install(update, apk)
        }
    }

    /** The app came back to the front: if it was waiting for the permission and now has it, the install goes on. */
    fun resumed() {
        val s = _state.value
        if (s is State.NeedsPermission && canInstall()) update(s.update)
    }

    /** Whether Android lets this app install apps (the "Install unknown apps" switch for it). */
    fun canInstall(): Boolean = context.packageManager.canRequestPackageInstalls()

    /** Android's "Install unknown apps" page for this app. */
    fun allowInstalls() {
        val intent = Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        runCatching { context.startActivity(intent) }.onFailure { NoriLog.w("install settings: ${it.message}") }
    }

    /** The release's page in the browser: what a debug build offers instead of installing. */
    fun openPage(page: String) {
        runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(page)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
    }

    private class WrongSize(val got: Long, val expected: Long) : IOException("$got bytes, expected $expected")

    /** The APK into the cache, through a part file, publishing whole percents; its size must be GitHub's. */
    private suspend fun download(update: AppUpdate, apk: File) {
        dir.deleteRecursively()
        dir.mkdirs()
        val part = File(dir, "${update.apkName}.part")
        val expected = update.apkBytes.toLong()
        val response = call(Request.Builder().url(update.apkUrl).build())
        response.use { r ->
            if (!r.isSuccessful) throw dev.nori.music.net.HttpStatusException(r.code)
            val body = r.body
            var done = 0L
            var shown = -1L
            val buf = ByteArray(64 * 1024)
            body.byteStream().use { input ->
                part.outputStream().use { out ->
                    while (true) {
                        kotlin.coroutines.coroutineContext.ensureActive()
                        val n = input.read(buf)
                        if (n < 0) break
                        out.write(buf, 0, n)
                        done += n
                        if (done > expected) throw WrongSize(done, expected)
                        val percent = done * 100 / expected
                        if (percent != shown) {
                            shown = percent
                            _state.value = State.Downloading(update, done, expected)
                        }
                    }
                }
            }
            if (done != expected) throw WrongSize(done, expected)
        }
        if (!part.renameTo(apk)) throw IOException("could not keep ${apk.name}")
    }

    /** One request on the app's own OkHttp client; cancelling the coroutine cancels it. */
    private suspend fun call(request: Request): Response = suspendCancellableCoroutine { cont ->
        val call = http().api.newCall(request)
        cont.invokeOnCancellation { call.cancel() }
        call.enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) { if (cont.isActive) cont.resumeWithException(e) }
            override fun onResponse(call: Call, response: Response) { if (cont.isActive) cont.resume(response) else response.close() }
        })
    }

    /** Whether [apk] is this app at a newer version code; Android checks the signature itself when it installs. */
    private fun ours(apk: File): Boolean {
        val pm = context.packageManager
        val info = pm.getPackageArchiveInfo(apk.path, 0) ?: return false
        val mine = pm.getPackageInfo(context.packageName, 0)
        @Suppress("DEPRECATION")
        val code = { p: android.content.pm.PackageInfo -> if (Build.VERSION.SDK_INT >= 28) p.longVersionCode else p.versionCode.toLong() }
        return info.packageName == context.packageName && code(info) > code(mine)
    }

    /** A PackageInstaller session writing [apk]; what Android answers comes to [UpdateStatusReceiver]. */
    private fun install(update: AppUpdate, apk: File) {
        _state.value = State.Installing(update)
        try {
            val installer = context.packageManager.packageInstaller
            val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
                setAppPackageName(context.packageName)
                setSize(apk.length())
                if (Build.VERSION.SDK_INT >= 31) setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
            }
            val id = installer.createSession(params)
            installer.openSession(id).use { session ->
                apk.inputStream().use { input ->
                    session.openWrite(update.apkName, 0, apk.length()).use { out ->
                        input.copyTo(out, 64 * 1024)
                        session.fsync(out)
                    }
                }
                val status = Intent(context, UpdateStatusReceiver::class.java).setAction(ACTION_STATUS).setPackage(context.packageName)
                val flags = PendingIntent.FLAG_UPDATE_CURRENT or (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
                session.commit(PendingIntent.getBroadcast(context, id, status, flags).intentSender)
            }
        } catch (e: Exception) {
            NoriLog.w("update install: ${e.message}")
            _state.value = State.Failed(update, Failure.Install(PackageInstaller.STATUS_FAILURE, e.message))
        }
    }

    /** What Android said about the session (see [UpdateStatusReceiver]). */
    internal fun onStatus(intent: Intent) {
        val update = (_state.value as? State.Installing)?.update
        val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
        val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
        NoriLog.i("update install: status $status ${message.orEmpty()}")
        when (status) {
            // Android's own confirmation, shown over the app: this app is on screen, as the user just
            // pressed Update, so it may start it.
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                @Suppress("DEPRECATION")
                val confirm = if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java) else intent.getParcelableExtra(Intent.EXTRA_INTENT)
                confirm?.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)?.let { runCatching { context.startActivity(it) }.onFailure { e -> NoriLog.w("update confirm: ${e.message}") } }
            }
            // Replaced: this process is ending. Nothing to keep.
            PackageInstaller.STATUS_SUCCESS -> dir.deleteRecursively()
            // The user said no on Android's page: back to where it was, the APK kept for another try.
            PackageInstaller.STATUS_FAILURE_ABORTED -> update?.let { _state.value = State.Available(it, skipped = false) }
            else -> update?.let { _state.value = State.Failed(it, Failure.Install(status, message)) }
        }
    }

    companion object {
        /** The cache directory the APK is downloaded into. */
        const val DIR = "update"
        const val ACTION_STATUS = "dev.nori.music.UPDATE_STATUS"
    }
}
