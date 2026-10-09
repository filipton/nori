package dev.nori.music.remote

import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager
import android.media.MediaRoute2Info
import android.media.MediaRoute2ProviderService
import android.media.MediaRouter2
import android.media.RouteDiscoveryPreference
import android.media.RoutingSessionInfo
import android.os.Build
import android.os.Bundle
import androidx.annotation.RequiresApi

/**
 * The device this phone mirrors as Android's media router knows it: one route and one routing session
 * named after it, so the system's output switcher, the media controls and the volume panel say
 * "Filips-MacBook-Pro" rather than "Other device". The session's id is what the media session names as
 * its remote volume's controller (RemoteDevicePlayer's DeviceInfo). Nothing is registered while nothing
 * is mirrored. While something is, this app scans for its own route (the system binds the provider for
 * that, and lists the route as where this app's media goes) and the provider holds the session.
 *
 * The provider is enabled only while something is mirrored (manifest: disabled): the system keeps a
 * provider it bound, at a foreground service's priority, after its session is released until it next
 * looks at the routes; disabling the component makes it let go at once.
 */
object RemoteRoute {
    /** The device mirrored now, or null; the provider shows it whenever the system binds it. */
    private var shown: String? = null
    /** The provider while the system has it bound: it is the system's to make, so it is reached through here. */
    private var provider: Provider? = null
    private var scan: MediaRouter2.RouteCallback? = null

    private const val FEATURE = "dev.nori.music.REMOTE"
    private const val ROUTE = "mirrored"
    private const val SESSION = "mirrored"

    /** The routing controller id of the session, as the system knows it: the provider's id and the session's. */
    fun controllerId(context: Context): String? =
        if (Build.VERSION.SDK_INT >= 30) ComponentName(context, Provider::class.java).flattenToShortString() + ":" + SESSION else null

    /** Shows [device] (its name) as where this phone's media plays, or nothing (null). Main thread. */
    fun show(context: Context, device: String?) {
        if (Build.VERSION.SDK_INT < 30 || device == shown) return
        shown = device
        provider?.show(device)
        if (device != null) {
            enable(context, true)
            startScan(context)
        } else {
            stopScan(context)
            enable(context, false)
        }
    }

    private fun enable(context: Context, on: Boolean) = context.packageManager.setComponentEnabledSetting(
        ComponentName(context, Provider::class.java),
        if (on) PackageManager.COMPONENT_ENABLED_STATE_ENABLED else PackageManager.COMPONENT_ENABLED_STATE_DEFAULT,
        PackageManager.DONT_KILL_APP,
    )

    /** A scan for this app's own route only: a self-scan-only provider is bound for no other app's. */
    @RequiresApi(30)
    private fun startScan(context: Context) {
        if (scan != null) return
        val callback = object : MediaRouter2.RouteCallback() {}
        scan = callback
        MediaRouter2.getInstance(context).registerRouteCallback(context.mainExecutor, callback, RouteDiscoveryPreference.Builder(listOf(FEATURE), true).build())
    }

    @RequiresApi(30)
    private fun stopScan(context: Context) {
        val callback = scan ?: return
        scan = null
        MediaRouter2.getInstance(context).unregisterRouteCallback(callback)
    }

    /** The provider the system binds (manifest: self-scan only, so only this app's scan binds it). */
    @RequiresApi(30)
    class Provider : MediaRoute2ProviderService() {
        private var session: RoutingSessionInfo? = null

        override fun onCreate() {
            super.onCreate()
            provider = this
        }

        override fun onDestroy() {
            if (provider === this) provider = null
            super.onDestroy()
        }

        override fun onDiscoveryPreferenceChanged(preference: RouteDiscoveryPreference) {
            // Bound for the scan: what is mirrored goes up.
            show(shown)
        }

        fun show(device: String?) {
            if (device == null) {
                session?.let { notifySessionReleased(it.id) }
                session = null
                notifyRoutes(emptyList())
                return
            }
            val route = MediaRoute2Info.Builder(ROUTE, device).addFeature(FEATURE)
                .setType(MediaRoute2Info.TYPE_REMOTE_SPEAKER).setConnectionState(MediaRoute2Info.CONNECTION_STATE_CONNECTED).build()
            notifyRoutes(listOf(route))
            val made = RoutingSessionInfo.Builder(SESSION, packageName).addSelectedRoute(ROUTE).setName(device).build()
            if (session == null) notifySessionCreated(REQUEST_ID_NONE, made) else notifySessionUpdated(made)
            session = made
        }

        // Nothing is routed through here: the session only names the device the media session controls.
        override fun onCreateSession(requestId: Long, packageName: String, routeId: String, sessionHints: Bundle?) =
            notifyRequestFailed(requestId, REASON_ROUTE_NOT_AVAILABLE)
        override fun onReleaseSession(requestId: Long, sessionId: String) = notifyRequestFailed(requestId, REASON_INVALID_COMMAND)
        override fun onSelectRoute(requestId: Long, sessionId: String, routeId: String) = notifyRequestFailed(requestId, REASON_INVALID_COMMAND)
        override fun onDeselectRoute(requestId: Long, sessionId: String, routeId: String) = notifyRequestFailed(requestId, REASON_INVALID_COMMAND)
        override fun onTransferToRoute(requestId: Long, sessionId: String, routeId: String) = notifyRequestFailed(requestId, REASON_INVALID_COMMAND)
        override fun onSetRouteVolume(requestId: Long, routeId: String, volume: Int) = notifyRequestFailed(requestId, REASON_INVALID_COMMAND)
        override fun onSetSessionVolume(requestId: Long, sessionId: String, volume: Int) = notifyRequestFailed(requestId, REASON_INVALID_COMMAND)
    }
}
