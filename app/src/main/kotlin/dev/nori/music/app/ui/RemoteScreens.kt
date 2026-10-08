package dev.nori.music.app.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Logout
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Groups
import androidx.compose.material.icons.filled.IosShare
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material.icons.filled.Speaker
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.R
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.RemoteViewModel
import dev.nori.music.ffi.JamView
import dev.nori.music.ffi.RemoteDevice
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.remote.DeviceKind
import dev.nori.music.ffi.remote.DeviceState
import dev.nori.music.ffi.remote.Entry
import dev.nori.music.ffi.remote.JamMember
import dev.nori.music.ffi.remote.Op
import dev.nori.music.ffi.remote.Pending
import dev.nori.music.ffi.remote.Refusal
import dev.nori.music.ffi.remote.Role
import kotlinx.coroutines.delay

private fun kindIcon(kind: DeviceKind): ImageVector = when (kind) {
    DeviceKind.PHONE -> Icons.Filled.PhoneAndroid
    DeviceKind.DESKTOP -> Icons.Filled.Computer
    DeviceKind.TERMINAL -> Icons.Filled.Terminal
    DeviceKind.GUEST -> Icons.Filled.Person
}

@Composable
private fun words(id: Int, vararg args: Any): String {
    val r = LocalContext.current.resources
    return remember(id, *args) { r.getString(id, *args) }
}

@Composable
private fun refusal(r: Refusal): String = words(
    when (r) {
        Refusal.STALE -> R.string.devices_stale
        Refusal.NOT_ALLOWED -> R.string.devices_not_allowed
        Refusal.UNKNOWN -> R.string.devices_unknown
        Refusal.TOO_MANY -> R.string.jam_too_many
    },
)

/** The song a state is on, if it lists it. */
private fun DeviceState.current(): Entry? = entries.firstOrNull { it.index == index }

/** The songs after the current one, in play order. */
private fun DeviceState.upNext(): List<Entry> = entries.dropWhile { it.index != index }.drop(1)

/**
 * Where the music plays: this phone, or one of the account's other devices with nori (one tap moves the
 * playback there, and this phone then shows and controls it), the phone's own audio output (Android's
 * picker), and the jam. Up only while remote control or jams are on.
 */
@Composable
fun DevicesSheet(open: Boolean, onDismiss: () -> Unit, onOutput: () -> Unit, jams: Boolean) {
    val vm: RemoteViewModel = viewModel()
    val nav = LocalNav.current
    NoriSheet(open, onDismiss) {
        LifecycleResumeEffect(Unit) { vm.watch(true); onPauseOrDispose { vm.watch(false) } }
        val devices by vm.devices.collectAsStateWithLifecycle()
        val jam by vm.jam.collectAsStateWithLifecycle()
        val relay by vm.relay.collectAsStateWithLifecycle()
        val mirror by vm.mirror.collectAsStateWithLifecycle()
        val unsupported = relay == dev.nori.music.ffi.RelaySupport.UNSUPPORTED
        val pick = { device: String? -> vm.pick(device); onDismiss() }
        Column(Modifier.verticalScroll(rememberScrollState()).navigationBarsPadding()) {
            SectionHeader(words(R.string.devices_title))
            DeviceRow(words(R.string.devices_this), Icons.Filled.PhoneAndroid, null, mirror == null) { pick(null) }
            devices.forEach { d ->
                val now = d.state?.current()
                DeviceRow(
                    d.name, kindIcon(d.kind),
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
            if (jams && unsupported) Text(
                words(R.string.jam_unsupported), Modifier.padding(horizontal = Space.gutter, vertical = 12.dp),
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            else if (jams) ActionRow(words(if (jam?.hosting == true) R.string.jam_yours else R.string.jam_start), Icons.Filled.Groups, { onDismiss(); nav.jam() })
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

/** The invite: a QR code (drawn from the core's modules) and the link to share. */
@Composable
private fun Invite(link: String, vm: RemoteViewModel) {
    val qr by produceState<dev.nori.music.ffi.remote.QrCode?>(null, link) { value = vm.qr(link) }
    val context = LocalContext.current
    Column(Modifier.fillMaxWidth().padding(Space.gutter), horizontalAlignment = Alignment.CenterHorizontally) {
        qr?.let { code ->
            Surface(color = Color.White, shape = RoundedCornerShape(16.dp)) {
                Canvas(Modifier.padding(16.dp).size(220.dp)) {
                    val n = code.size.toInt()
                    val cell = size.width / n
                    for (y in 0 until n) for (x in 0 until n) {
                        if (code.dark[y * n + x]) drawRect(Color.Black, Offset(x * cell, y * cell), Size(cell + 0.5f, cell + 0.5f))
                    }
                }
            }
        }
        Text(words(R.string.jam_invite), Modifier.padding(top = 12.dp), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        PillButton(words(R.string.jam_share_link), Icons.Filled.IosShare, {
            context.startActivity(android.content.Intent.createChooser(android.content.Intent(android.content.Intent.ACTION_SEND).setType("text/plain").putExtra(android.content.Intent.EXTRA_TEXT, link), null))
        }, Modifier.padding(top = 12.dp))
    }
}

@Composable
private fun roleName(role: Role): String = words(
    when (role) {
        Role.HOST -> R.string.jam_role_host
        Role.ADMIN -> R.string.jam_role_admin
        Role.GUEST -> R.string.jam_role_guest
    },
)

@Composable
private fun MemberRow(m: JamMember, hosting: Boolean, vm: RemoteViewModel) {
    var open by remember { mutableStateOf(false) }
    Box {
        NavRow(m.name, { if (hosting && m.role != Role.HOST) open = true }, trailing = roleName(m.role), leading = { Icon(Icons.Filled.Person, null) })
        androidx.compose.material3.DropdownMenu(open, { open = false }) {
            val admin = m.role == Role.ADMIN
            androidx.compose.material3.DropdownMenuItem({ Text(words(if (admin) R.string.jam_make_guest else R.string.jam_make_admin)) }, { vm.jamAct(Op.Promote(m.id, !admin)); open = false })
            androidx.compose.material3.DropdownMenuItem({ Text(words(R.string.jam_send_out)) }, { vm.jamAct(Op.Kick(m.id)); open = false })
        }
    }
}

@Composable
private fun PendingRow(p: Pending, decides: Boolean, vm: RemoteViewModel) {
    Row(Modifier.fillMaxWidth().padding(start = Space.gutter, end = 8.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Cover(vm.cover(p.song.coverArt, CoverSize.ROW), 46.dp, radius = 6.dp)
        Column(Modifier.weight(1f).padding(start = 12.dp)) {
            Text(p.song.title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(words(R.string.jam_asked_by, p.fromName), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
            if (p.provider && decides) Caption(words(R.string.jam_will_download), caps = false)
        }
        if (decides) {
            Chip(words(R.string.jam_decline), false) { vm.jamAct(Op.Decide(p.request, false)) }
            Spacer(Modifier.size(6.dp))
            Chip(words(R.string.jam_accept), true) { vm.jamAct(Op.Decide(p.request, true)) }
        }
    }
}

@Composable
private fun EntryRow(e: Entry, vm: RemoteViewModel) {
    Row(Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Cover(vm.cover(e.coverArt, CoverSize.ROW), 46.dp, radius = 6.dp)
        Column(Modifier.weight(1f).padding(start = 12.dp)) {
            Text(e.title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(e.by?.let { words(R.string.jam_added_by, it) } ?: e.artist, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
        }
    }
}

/** The jam's lists, shared by the host's screen and a guest's. */
private fun androidx.compose.foundation.lazy.LazyListScope.jamLists(j: JamView, vm: RemoteViewModel, requestsTitle: String, peopleTitle: String, nextTitle: String, nowTitle: String) {
    val you = j.members.firstOrNull { it.id == j.you }
    val decides = j.hosting || you?.role == Role.ADMIN
    j.queue?.current()?.let { now ->
        item { SectionHeader(nowTitle) }
        item { EntryRow(now, vm) }
    }
    val pending = if (decides) j.pending else j.pending.filter { it.from == j.you }
    if (pending.isNotEmpty()) {
        item { SectionHeader(requestsTitle) }
        items(pending, key = { "p${it.request}" }) { PendingRow(it, decides, vm) }
    }
    val next = j.queue?.upNext().orEmpty()
    if (next.isNotEmpty()) {
        item { SectionHeader(nextTitle) }
        items(next, key = { "q${it.index}" }) { EntryRow(it, vm) }
    }
    item { SectionHeader(peopleTitle) }
    items(j.members, key = { "m${it.id}" }) { MemberRow(it, j.hosting, vm) }
}

/** The host's jam: the invite, what is asked for, the queue, the people and their roles. */
@Composable
fun JamScreen() {
    val vm: RemoteViewModel = viewModel()
    LifecycleResumeEffect(Unit) { vm.watch(true); onPauseOrDispose { vm.watch(false) } }
    val jam by vm.jam.collectAsStateWithLifecycle()
    val relay by vm.relay.collectAsStateWithLifecycle()
    val context = LocalContext.current
    LaunchedEffect(vm) { vm.failures.collect { android.widget.Toast.makeText(context, R.string.jam_failed, android.widget.Toast.LENGTH_LONG).show() } }
    val requests = words(R.string.jam_requests)
    val people = words(R.string.jam_people)
    val next = words(R.string.devices_up_next)
    val now = words(R.string.jam_now_playing)
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = LocalChromeInset.current + 16.dp)) {
        item { LargeTitle(words(R.string.jam_title)) }
        val j = jam
        if (j == null || !j.hosting) {
            item {
                Column(Modifier.fillMaxWidth().padding(Space.gutter)) {
                    val unsupported = relay == dev.nori.music.ffi.RelaySupport.UNSUPPORTED
                    Text(words(if (unsupported) R.string.jam_unsupported else R.string.settings_jam_detail), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    if (!unsupported) PillButton(words(R.string.jam_start), Icons.Filled.Groups, vm::jamStart, Modifier.padding(top = 16.dp), prominent = true)
                }
            }
        } else {
            j.link?.let { link -> item { Invite(link, vm) } }
            jamLists(j, vm, requests, people, next, now)
            item { ActionRow(words(R.string.jam_end), Icons.Filled.Close, vm::jamEnd, divider = false) }
        }
    }
}

/**
 * The app as a jam guest sees it: the host's music, a search to ask for songs (the song menu's only
 * entry is asking for it), what one asked for, and leaving. Nothing plays here.
 */
@Composable
fun GuestApp(actions: ActionsViewModel) {
    val vm: RemoteViewModel = viewModel()
    DisposableEffect(Unit) { vm.watch(true); onDispose { vm.watch(false) } }
    val jam by vm.jam.collectAsStateWithLifecycle()
    val found by vm.found.collectAsStateWithLifecycle()
    var query by remember { mutableStateOf("") }
    var menu by remember { mutableStateOf<Song?>(null) }
    LaunchedEffect(query) { delay(350); vm.search(query) }
    val requests = words(R.string.jam_yours_waiting)
    val people = words(R.string.jam_people)
    val next = words(R.string.devices_up_next)
    val now = words(R.string.jam_now_playing)
    val context = LocalContext.current
    SongMenu(menu, actions, { menu = null }, request = { s ->
        vm.request(s)
        android.widget.Toast.makeText(context, context.getString(R.string.jam_asked, s.title), android.widget.Toast.LENGTH_SHORT).show()
    })
    Surface(color = MaterialTheme.colorScheme.background, contentColor = MaterialTheme.colorScheme.onBackground) {
        LazyColumn(Modifier.fillMaxSize().statusBarsPadding(), contentPadding = PaddingValues(bottom = 24.dp)) {
            item {
                LargeTitle(words(R.string.jam_title)) {
                    IconButton(vm::leave) { Icon(Icons.AutoMirrored.Filled.Logout, words(R.string.jam_leave)) }
                }
            }
            jam?.refused?.let { r -> item { Text(refusal(r), Modifier.padding(horizontal = Space.gutter), color = MaterialTheme.colorScheme.error) } }
            item { SearchField(query, { query = it }, words(R.string.jam_search), Modifier.padding(horizontal = Space.gutter, vertical = 8.dp)) }
            if (query.isNotBlank()) items(found, key = { "s${it.id}" }) { s ->
                SongRow(s, vm.cover(s.coverArt, CoverSize.ROW), { menu = s }, { menu = s })
            }
            val j = jam
            if (j == null || j.queue == null) item {
                Text(words(R.string.jam_waiting), Modifier.padding(Space.gutter), color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else jamLists(j, vm, requests, people, next, now)
        }
    }
}
