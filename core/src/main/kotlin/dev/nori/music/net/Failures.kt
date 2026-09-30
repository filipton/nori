package dev.nori.music.net

import android.content.res.Resources
import dev.nori.music.core.R
import dev.nori.music.ffi.model.CoreException
import dev.nori.music.ffi.net.FailureKind
import dev.nori.music.ffi.net.NetException
import dev.nori.music.ffi.settings.SoundException

/**
 * Failures in words, from core/res/values/strings.xml. The core hands over what kind of failure it was
 * (`NetError`, `CoreError`, `SoundError`: a kind and its facts, such as an HTTP status or the Subsonic
 * error's code); which words go with which kind is the app's. The resources are handed over once at start
 * and again when the locale changes, like the screens' `Say`. Only called when something failed.
 */
object Failures {
    @Volatile private var res: Resources? = null

    /** The app's resources, at start and whenever the locale changes. */
    fun use(r: Resources) { res = r }

    private fun str(id: Int, vararg args: Any): String? = res?.getString(id, *args)

    /** What a failure says: its kind's words, or else its own message. */
    fun said(e: Throwable): String? = when (e) {
        is NetException -> said(e.lift())
        is SoundException.NoFilters -> str(R.string.error_no_filters)
        is CoreException.Api -> e.reason
        is CoreException.Parse -> str(R.string.error_bad_response, e.reason)
        is CoreException.Db -> str(R.string.error_database, e.reason)
        is CoreException.Smart -> str(R.string.error_smart_playlist)
        is SoundException.Db -> e.message
        is MeteredNetworkException -> str(R.string.error_metered)
        is HttpStatusException -> str(R.string.error_http, e.status)
        else -> e.message
    } ?: e.message

    /** What went wrong, in words a person can act on: the kinds with words of their own say them, the rest what [said] says. */
    fun describe(e: Throwable): String {
        if (e is NetException) return describe(e.lift())
        val id = when (e) {
            is CoreException.Api -> when (e.code) {
                40 -> R.string.error_wrong_login
                41 -> R.string.error_token_auth
                50 -> R.string.error_not_allowed
                else -> 0
            }
            is CoreException.Parse -> R.string.error_not_subsonic
            is CoreException, is SoundException -> 0
            else -> when (failureKind(e)) {
                FailureKind.METERED -> R.string.error_metered
                FailureKind.UNKNOWN_HOST -> R.string.error_unknown_host
                FailureKind.CONNECT -> R.string.error_connect
                FailureKind.TIMEOUT -> R.string.error_timeout
                FailureKind.TLS -> R.string.error_tls
                FailureKind.CLEARTEXT -> R.string.error_cleartext
                else -> 0
            }
        }
        return (if (id != 0) str(id) else null) ?: said(e) ?: e.javaClass.simpleName
    }
}
