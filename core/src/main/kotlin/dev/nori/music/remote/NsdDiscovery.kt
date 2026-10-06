package dev.nori.music.remote

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import dev.nori.music.ffi.Announcement
import dev.nori.music.ffi.Discovery
import dev.nori.music.ffi.Param
import dev.nori.music.ffi.Remote

/**
 * The core's [Discovery] over Android's NsdManager: the system's mDNS daemon announces and looks, so the app
 * holds no multicast lock and wakes only for a door found or lost. Announcing runs while the device is
 * controllable, looking while a picker or jam screen is open (the core says when).
 */
class NsdDiscovery(context: Context, private val remote: () -> Remote?) : Discovery {
    private val nsd = context.getSystemService(NsdManager::class.java)
    private var registered: NsdManager.RegistrationListener? = null
    private var browsing: NsdManager.DiscoveryListener? = null
    /** Before Android 14 one resolve runs at a time; the others wait here. */
    private val resolving = ArrayDeque<NsdServiceInfo>()
    private var busy = false

    @Synchronized
    override fun announce(door: Announcement?) {
        registered?.let { runCatching { nsd.unregisterService(it) } }
        registered = null
        door ?: return
        val info = NsdServiceInfo().apply {
            serviceName = door.name
            serviceType = SERVICE
            port = door.port.toInt()
            door.txt.forEach { setAttribute(it.key, it.value) }
        }
        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(info: NsdServiceInfo) {}
            override fun onRegistrationFailed(info: NsdServiceInfo, error: Int) = dev.nori.music.NoriLog.w("nsd: not announced ($error)")
            override fun onServiceUnregistered(info: NsdServiceInfo) {}
            override fun onUnregistrationFailed(info: NsdServiceInfo, error: Int) {}
        }
        registered = listener
        runCatching { nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener) }
    }

    @Synchronized
    override fun browse(on: Boolean) {
        browsing?.let { runCatching { nsd.stopServiceDiscovery(it) } }
        browsing = null
        if (!on) return
        val listener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(type: String) {}
            override fun onDiscoveryStopped(type: String) {}
            override fun onStartDiscoveryFailed(type: String, error: Int) = dev.nori.music.NoriLog.w("nsd: no discovery ($error)")
            override fun onStopDiscoveryFailed(type: String, error: Int) {}
            override fun onServiceFound(info: NsdServiceInfo) = resolve(info)
            override fun onServiceLost(info: NsdServiceInfo) {
                remote()?.lanLost(info.serviceName)
            }
        }
        browsing = listener
        runCatching { nsd.discoverServices(SERVICE, NsdManager.PROTOCOL_DNS_SD, listener) }
    }

    @Synchronized
    private fun resolve(info: NsdServiceInfo) {
        if (busy) { resolving.addLast(info); return }
        busy = true
        @Suppress("DEPRECATION")
        nsd.resolveService(info, object : NsdManager.ResolveListener {
            override fun onResolveFailed(info: NsdServiceInfo, error: Int) = next()
            override fun onServiceResolved(info: NsdServiceInfo) {
                @Suppress("DEPRECATION")
                val host = info.host?.hostAddress
                if (host != null) {
                    val txt = info.attributes.map { (k, v) -> Param(k, v?.toString(Charsets.UTF_8).orEmpty()) }
                    remote()?.lanFound(info.serviceName, host, info.port.toUShort(), txt)
                }
                next()
            }
        })
    }

    @Synchronized
    private fun next() {
        busy = false
        resolving.removeFirstOrNull()?.let(::resolve)
    }

    private companion object {
        /** nori-remote's `lan::SERVICE`, as NsdManager writes a type. */
        const val SERVICE = "_nori._tcp"
    }
}
