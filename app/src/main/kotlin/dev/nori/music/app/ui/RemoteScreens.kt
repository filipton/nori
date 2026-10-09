package dev.nori.music.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Groups
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.Speaker
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.R
import dev.nori.music.app.vm.RemoteViewModel
import dev.nori.music.ffi.remote.DeviceKind
import dev.nori.music.ffi.remote.DeviceState
import dev.nori.music.ffi.remote.Entry
import dev.nori.music.ffi.remote.Refusal
import dev.nori.music.ffi.remote.Role

private fun kindIcon(kind: DeviceKind): ImageVector = when (kind) {
    DeviceKind.PHONE -> Icons.Filled.PhoneAndroid
    DeviceKind.DESKTOP -> Icons.Filled.Computer
    DeviceKind.TERMINAL -> Icons.Filled.Terminal
    DeviceKind.GUEST -> Icons.Filled.Person
}

@Composable
internal fun words(id: Int, vararg args: Any): String {
    val r = LocalContext.current.resources
    return remember(id, *args) { r.getString(id, *args) }
}

@Composable
internal fun refusal(r: Refusal): String = words(
    when (r) {
        Refusal.STALE -> R.string.devices_stale
        Refusal.NOT_ALLOWED -> R.string.devices_not_allowed
        Refusal.UNKNOWN -> R.string.devices_unknown
        Refusal.TOO_MANY -> R.string.jam_too_many
    },
)

/** The song a state is on, if it lists it. */
private fun DeviceState.current(): Entry? = entries.firstOrNull { it.index == index }

/**
 * Where the music plays: this phone, or one of the account's other devices with nori (one tap moves the
 * playback there, and this phone then shows and controls it), the phone's own audio output (Android's
 * picker), and the jam: "Start a Jam", or "Your Jam" with End while one is on. Up only while remote control
 * or jams are on.
 */
@Composable
fun DevicesSheet(open: Boolean, onDismiss: () -> Unit, onOutput: () -> Unit, jams: Boolean) {
    val vm: RemoteViewModel = viewModel()
    val nav = LocalNav.current
    NoriSheet(open, onDismiss) {
        LifecycleResumeEffect(Unit) { vm.watch(true); onPauseOrDispose { vm.watch(false) } }
        val devices by vm.devices.collectAsStateWithLifecycle()
        val names by vm.names.collectAsStateWithLifecycle()
        val jam by vm.jam.collectAsStateWithLifecycle()
        val relay by vm.relay.collectAsStateWithLifecycle()
        val mirror by vm.mirror.collectAsStateWithLifecycle()
        val unsupported = relay == dev.nori.music.ffi.RelaySupport.UNSUPPORTED
        val pick = { device: String? -> vm.pick(device); onDismiss() }
        Column(Modifier.verticalScroll(rememberScrollState()).navigationBarsPadding()) {
            SectionHeader(words(R.string.devices_title))
            DeviceRow(words(R.string.devices_this), Icons.Filled.PhoneAndroid, null, mirror == null) { pick(null) }
            devices.forEachIndexed { k, d ->
                val now = d.state?.current()
                DeviceRow(
                    names.getOrNull(k) ?: d.name, kindIcon(d.kind),
                    now?.let { "${it.title} · ${it.artist}" } ?: words(R.string.devices_idle),
                    mirror?.id == d.id, if (d.nearby) words(R.string.devices_nearby) else null,
                ) { pick(d.id) }
            }
            mirror?.refused?.let { Text(refusal(it), Modifier.padding(horizontal = Space.gutter, vertical = 6.dp), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium) }
            if (devices.isEmpty() || unsupported) Text(
                words(if (unsupported) R.string.devices_nearby_only else R.string.devices_none), Modifier.padding(horizontal = Space.gutter, vertical = 12.dp),
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // The phone's own speaker, headphones or Bluetooth: Android's picker, a quiet row of its own.
            Row(
                Modifier.fillMaxWidth().clickable(onClick = onOutput).padding(horizontal = Space.gutter, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.Filled.Speaker, null, Modifier.size(18.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                Text(words(R.string.devices_output), Modifier.padding(start = 12.dp), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Hairline(startIndent = Space.gutter)
            val hosted = jam?.takeIf { it.hosting }
            when {
                !jams -> {}
                hosted != null -> YourJam(hosted.members.count { it.role != Role.HOST }, { onDismiss(); nav.player(Panel.QUEUE) }, vm::jamEnd)
                unsupported -> Text(
                    words(R.string.jam_unsupported), Modifier.padding(horizontal = Space.gutter, vertical = 12.dp),
                    style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                else -> ActionRow(words(R.string.jam_start), Icons.Filled.Groups, { onDismiss(); vm.jamStart(); nav.player(Panel.QUEUE) }, divider = false)
            }
        }
    }
}

/** The devices sheet, opened from the player's output button and the "Playing on" strips ([LocalDevices]). */
@Composable
fun DevicesHost(open: Boolean, onDismiss: () -> Unit) {
    val settings: dev.nori.music.app.vm.SettingsViewModel = viewModel()
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    val output by settings.currentOutput.collectAsStateWithLifecycle()
    val context = LocalContext.current
    if (prefs.remoteControl || prefs.jam) DevicesSheet(open, onDismiss, { onDismiss(); openOutputPicker(context, output) }, prefs.jam)
}

/** The jam this phone hosts, as the devices list it: a tap opens its queue, End ends it. */
@Composable
private fun YourJam(listening: Int, onOpen: () -> Unit, onEnd: () -> Unit) {
    NavRow(
        words(R.string.jam_yours), onOpen, subtitle = words(R.string.jam_strip, jamListening(listening)), divider = false,
        leading = {
            Box(Modifier.size(40.dp).clip(CircleShape).background(MaterialTheme.colorScheme.primary), contentAlignment = Alignment.Center) {
                Icon(Icons.Filled.Groups, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.onPrimary)
            }
        },
        action = {
            Text(
                words(R.string.jam_end_short), Modifier.clip(PillShape).clickable(onClick = onEnd).padding(horizontal = 14.dp, vertical = 8.dp),
                style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary,
            )
        },
    )
}

/** A place the music can play, ticked while it plays there. */
@Composable
private fun DeviceRow(name: String, icon: ImageVector, subtitle: String?, active: Boolean, trailing: String? = null, onClick: () -> Unit) {
    val accent = MaterialTheme.colorScheme.primary
    NavRow(
        name, onClick, subtitle = subtitle, trailing = trailing,
        leading = { Icon(icon, null, tint = if (active) accent else MaterialTheme.colorScheme.onSurface) },
        action = if (active) ({ Icon(Icons.Filled.Check, words(R.string.devices_playing_here), Modifier.padding(start = 8.dp).size(20.dp), tint = accent) }) else null,
    )
}

/**
 * "Playing on" another device, under the now playing bar and in the player: a tap opens the devices.
 * Nothing while this phone plays.
 */
@Composable
fun PlayingOnStrip(device: String?, color: Color, modifier: Modifier = Modifier) {
    device ?: return
    val open = LocalDevices.current
    Row(
        modifier.clickable(onClick = open).padding(horizontal = 12.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Filled.Speaker, null, Modifier.size(15.dp), tint = color)
        Text(
            words(R.string.devices_playing_on, device), Modifier.padding(start = 6.dp),
            style = MaterialTheme.typography.labelMedium, color = color, maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}
