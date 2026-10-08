package dev.nori.music.net

import android.annotation.SuppressLint
import android.content.Context
import android.net.ConnectivityManager
import dev.nori.music.ffi.model.CoreException
import dev.nori.music.ffi.net.FailureKind
import dev.nori.music.ffi.net.getFailed
import dev.nori.music.ffi.net.NetException
import dev.nori.music.ffi.net.Network
import dev.nori.music.ffi.net.Exchange
import dev.nori.music.ffi.net.RequestPolicy
import dev.nori.music.ffi.net.ServerHosts
import dev.nori.music.ffi.net.Transport
import dev.nori.music.ffi.net.TransportException
import dev.nori.music.ffi.net.TransportResponse
import dev.nori.music.ffi.net.netPolicy
import dev.nori.music.ffi.settings.SavedServer
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.suspendCancellableCoroutine
import okhttp3.Call
import okhttp3.Callback
import okhttp3.ConnectionPool
import okhttp3.Dispatcher
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.File
import java.io.IOException
import java.security.KeyStore
import java.security.cert.X509Certificate
import java.util.concurrent.TimeUnit
import javax.net.ssl.KeyManagerFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509TrustManager
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import dev.nori.music.settings.server

private val JSON = "application/json".toMediaType()

/** The server is only allowed on unmetered networks and this is not one. Its message is for the log; [said] words it. */
class MeteredNetworkException : IOException("metered network, server is Wi-Fi only")

/** The server answered with an error status and nothing a Subsonic client can read. Its message is for the log; [said] words it. */
class HttpStatusException(val status: Int) : IOException("HTTP $status")

/**
 * One connection pool for everything: API calls, cover art and audio all ride
 * the same HTTP/2 connection to the server, so the radio wakes once, not three times.
 * The clients are rebuilt when the server profile changes (headers, TLS); callers
 * go through [callFactory] / [streamFactory] so they always use the current ones.
 * How the clients are tuned (pool, request caps, timeouts) is the core's [netPolicy], with the reasons.
 */
class Http(private val context: Context) {
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private val policy = netPolicy()
    private val pool = ConnectionPool(policy.poolMaxIdle.toInt(), policy.poolKeepAliveMs.toLong(), TimeUnit.MILLISECONDS)
    private val dispatcher = Dispatcher().apply { maxRequestsPerHost = policy.maxRequestsPerHost.toInt(); maxRequests = policy.maxRequests.toInt() }
    private val streamDispatcher = Dispatcher().apply { maxRequestsPerHost = policy.streamMaxRequestsPerHost.toInt(); maxRequests = policy.streamMaxRequests.toInt() }
    @Volatile private var profile: SavedServer? = null

    /**
     * Which requests are this profile's server's, and its Wi-Fi-only rule: the core's (transport.rs
     * `ServerHosts`), with its answer per host kept, since every request (each cover, each range of a
     * stream) asks it and the answer only depends on the host and port. Replaced whole when the profile
     * changes, so an answer from the old one is never read.
     */
    private class Hosts(val server: ServerHosts) {
        val policies = java.util.concurrent.ConcurrentHashMap<String, Pair<Int, RequestPolicy>>()
    }
    @Volatile private var hosts = Hosts(ServerHosts(null, null, false))

    /** The audio's stall timeout (see [Stalls]), shared by every stream client. Starts no thread until a song streams. */
    private val stalls = Stalls(policy.streamReadTimeoutMs.toLong())

    @Volatile var api: OkHttpClient = build(null)
        private set

    @Volatile var stream: OkHttpClient = streamClient(api)
        private set

    val callFactory = Call.Factory { api.newCall(it) }
    val streamFactory = Call.Factory { stream.newCall(it) }

    /**
     * Asked once per track, when its quality is chosen. A registered network callback would be woken
     * for every signal-strength change for as long as the process lives; this costs one binder call.
     */
    val metered: Boolean get() = connectivity.isActiveNetworkMetered

    fun configure(next: SavedServer?) {
        val old = profile
        hosts = Hosts(ServerHosts(next?.url, next?.altUrl, next?.wifiOnly == true))
        profile = next
        // Only TLS settings need new clients; headers and the Wi-Fi rule are read per request.
        if (old?.allowSelfSigned != next?.allowSelfSigned || old?.clientCert != next?.clientCert || old?.clientCertPassword != next?.clientCertPassword) {
            api = build(next)
            stream = streamClient(api)
        }
    }

    /**
     * The audio's client: no read timeout, since OkHttp would arm its watchdog around every read of a song's
     * body, a network packet at a time; a stalled connection is found by [Stalls] instead.
     */
    private fun streamClient(api: OkHttpClient) =
        api.newBuilder().dispatcher(streamDispatcher).readTimeout(0, TimeUnit.MILLISECONDS).addInterceptor(stalls).build()

    private fun build(p: SavedServer?): OkHttpClient {
        val b = OkHttpClient.Builder()
            .dispatcher(dispatcher)
            .connectionPool(pool)
            .connectTimeout(policy.connectTimeoutMs.toLong(), TimeUnit.MILLISECONDS)
            .readTimeout(policy.readTimeoutMs.toLong(), TimeUnit.MILLISECONDS)
            .addInterceptor { chain ->
                val now = profile
                val request = chain.request()
                // Third parties (LRCLIB, AutoEQ) must not receive a reverse-proxy token, and are not subject to the
                // server's Wi-Fi-only setting: the core says which this is. The network is only looked at when
                // the answer depends on it.
                val rule = if (now != null) policyOf(request.url) else null
                if (rule?.unmeteredOnly == true && metered) throw MeteredNetworkException()
                chain.proceed(request.newBuilder().apply {
                    // Public services ask clients to identify themselves; some reject OkHttp's default outright.
                    header("User-Agent", policy.userAgent)
                    if (rule?.server == true) now?.headers?.forEach { (k, v) -> header(k, v) }
                }.build())
            }
        if (p != null && (p.allowSelfSigned || p.clientCert.isNotEmpty())) tls(b, p)
        return b.build()
    }

    /** The profile's policy for [url], asked of the core once per host and port. */
    private fun policyOf(url: okhttp3.HttpUrl): RequestPolicy {
        val h = hosts
        h.policies[url.host]?.let { (port, rule) -> if (port == url.port) return rule }
        return h.server.policy(url.toString()).also { h.policies[url.host] = url.port to it }
    }

    /** Self-signed servers and client certificates. Both are per profile and opt-in. */
    private fun tls(b: OkHttpClient.Builder, p: SavedServer) {
        val trust: X509TrustManager = if (p.allowSelfSigned) TrustAll else {
            TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm()).apply { init(null as KeyStore?) }.trustManagers.filterIsInstance<X509TrustManager>().first()
        }
        val keys = if (p.clientCert.isEmpty()) null else runCatching {
            val store = KeyStore.getInstance("PKCS12")
            File(context.filesDir, "certs/${p.clientCert}").inputStream().use { store.load(it, p.clientCertPassword.toCharArray()) }
            KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm()).apply { init(store, p.clientCertPassword.toCharArray()) }.keyManagers
        }.getOrNull()
        val ssl = SSLContext.getInstance("TLS").apply { init(keys, arrayOf(trust), null) }
        b.sslSocketFactory(ssl.socketFactory, trust)
        if (p.allowSelfSigned) b.hostnameVerifier { _, _ -> true }
    }

    /** The user ticked "accept any certificate" for this server: their own box with a self-signed cert. */
    @SuppressLint("CustomX509TrustManager", "TrustAllX509TrustManager")
    private object TrustAll : X509TrustManager {
        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {}
        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {}
        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }

    /** The status and the whole body. Cancelling the coroutine cancels the call, which is what makes live search cheap. */
    suspend fun exchange(url: String, timeoutMs: Long = 0): TransportResponse = exchange(Request.Builder().url(url).build(), timeoutMs)

    /**
     * [exchange] with a third party's own headers, and a JSON body POSTed when there is one: the core's
     * [Exchange], which says what to send; this only carries the bytes.
     */
    suspend fun exchange(e: Exchange): TransportResponse {
        val request = Request.Builder().url(e.url)
        for ((name, value) in e.headers) request.header(name, value)
        e.json?.let { request.post(it.toRequestBody(JSON)) }
        return exchange(request.build(), e.timeoutMs.toLong())
    }

    private suspend fun exchange(request: Request, timeoutMs: Long): TransportResponse = suspendCancellableCoroutine { cont ->
        // A request's own timeout is the whole wait, reading too: a held poll answers only when there is news.
        val client = if (timeoutMs > 0) api.newBuilder().callTimeout(timeoutMs, TimeUnit.MILLISECONDS).readTimeout(timeoutMs, TimeUnit.MILLISECONDS).build() else api
        val call = client.newCall(request)
        cont.invokeOnCancellation { call.cancel() }
        call.enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                if (cont.isActive) cont.resumeWithException(e)
            }

            override fun onResponse(call: Call, response: Response) {
                try {
                    val body = response.use { it.body.bytes() }
                    cont.resume(TransportResponse(response.code.toUShort(), body))
                } catch (e: IOException) {
                    if (cont.isActive) cont.resumeWithException(e)
                }
            }
        })
    }

    /**
     * A GET for callers outside the core's client (AutoEQ). octo-fiesta reports auth failures as 401 with a
     * normal Subsonic error body, so the body is read either way; only an empty error answer is a failure.
     */
    suspend fun get(url: String, timeoutMs: Long = 0): ByteArray {
        val r = exchange(url, timeoutMs)
        if (getFailed(r.status, r.body.isEmpty())) throw HttpStatusException(r.status.toInt())
        return r.body
    }

    /** The core's door to the network: one GET, with the platform's exceptions sorted into kinds. */
    fun transport(onAddressChanged: () -> Unit): Transport = object : Transport {
        override suspend fun get(url: String, timeoutMs: UInt): TransportResponse = try {
            exchange(url, timeoutMs.toLong())
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            throw TransportException.Failed(failureKind(e), e.message)
        }

        override suspend fun send(request: Exchange): TransportResponse = try {
            exchange(request)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            throw TransportException.Failed(failureKind(e), e.message)
        }

        override fun addressChanged() = onAddressChanged()

        /** Asked when the core chooses a quality or whether something may use mobile data, never per request. */
        override fun network() = if (metered) Network.METERED else Network.UNMETERED
    }
}

/** Which kind of failure the platform's exception is; the core decides what each kind means. */
fun failureKind(e: Throwable): FailureKind = when (e) {
    is MeteredNetworkException -> FailureKind.METERED
    is java.net.UnknownHostException -> FailureKind.UNKNOWN_HOST
    is java.net.ConnectException -> FailureKind.CONNECT
    is java.net.NoRouteToHostException -> FailureKind.NO_ROUTE
    is java.net.SocketTimeoutException -> FailureKind.TIMEOUT
    is java.io.InterruptedIOException -> FailureKind.INTERRUPTED
    is javax.net.ssl.SSLPeerUnverifiedException, is javax.net.ssl.SSLHandshakeException -> FailureKind.TLS
    is java.net.UnknownServiceException -> FailureKind.CLEARTEXT
    is IOException -> FailureKind.IO
    else -> FailureKind.OTHER
}

/**
 * A failure from the core's client as the exception the platform would have thrown itself, so callers keep
 * telling network trouble ([IOException]) from a refusal ([CoreException]) the way they always did.
 */
fun NetException.lift(): Exception = when (this) {
    is NetException.Transport -> when (kind) {
        FailureKind.METERED -> MeteredNetworkException()
        FailureKind.UNKNOWN_HOST -> java.net.UnknownHostException(detail)
        FailureKind.CONNECT -> java.net.ConnectException(detail)
        FailureKind.NO_ROUTE -> java.net.NoRouteToHostException(detail)
        FailureKind.TIMEOUT -> java.net.SocketTimeoutException(detail)
        FailureKind.INTERRUPTED -> java.io.InterruptedIOException(detail)
        FailureKind.TLS -> javax.net.ssl.SSLHandshakeException(detail.orEmpty())
        FailureKind.CLEARTEXT -> java.net.UnknownServiceException(detail)
        FailureKind.IO -> IOException(detail)
        FailureKind.OTHER -> IllegalStateException(detail)
    }
    is NetException.Http -> HttpStatusException(status.toInt())
    is NetException.Api -> CoreException.Api(code, reason)
    is NetException.Parse -> CoreException.Parse(reason)
    is NetException.Db -> CoreException.Db(reason)
}

/** Runs a call into the core's client, turning its failures into the platform's exceptions. */
inline fun <T> lifted(block: () -> T): T = try {
    block()
} catch (e: NetException) {
    throw e.lift()
}

/**
 * What a failure says. The core's exceptions carry no message of their own (uniffi's JNI bindings give
 * them none) and hand over a kind and its facts; they are worded here ([Failures]), as are the app's own
 * network exceptions. Anything else says its own message.
 */
val Throwable.said: String? get() = Failures.said(this)

/** What went wrong, in words a person can act on. */
fun describeConnectionError(e: Throwable): String = Failures.describe(e)
