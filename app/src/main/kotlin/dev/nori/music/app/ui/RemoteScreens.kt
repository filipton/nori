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
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.FastForward
import androidx.compose.material.icons.filled.FastRewind
import androidx.compose.material.icons.filled.Groups
import androidx.compose.material.icons.filled.IosShare
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.PhoneAndroid
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
import androidx.compose.runtime.mutableFloatStateOf
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
 * Where the music plays: this phone (its audio output, Android's own picker) and the account's other
 * devices with nori, each controllable from here, and the jam. Up only while remote control or jams are on.
 */
@Composable
fun DevicesSheet(open: Boolean, onDismiss: () -> Unit, onOutput: () -> Unit, jams: Boolean) {
    val vm: RemoteViewModel = viewModel()
    val nav = LocalNav.current
    NoriSheet(open, onDismiss) {
        DisposableEffect(Unit) { vm.watch(true); onDispose { vm.watch(false) } }
        val devices by vm.devices.collectAsStateWithLifecycle()
        val jam by vm.jam.collectAsStateWithLifecycle()
        var picked by remember { mutableStateOf<String?>(null) }
        Column(Modifier.verticalScroll(rememberScrollState()).navigationBarsPadding()) {
            SectionHeader(words(R.string.devices_title))
            NavRow(words(R.string.devices_this), onOutput, subtitle = words(R.string.devices_output), leading = { Icon(Icons.Filled.PhoneAndroid, null) })
            if (devices.isEmpty()) Text(
                words(R.string.devices_none), Modifier.padding(horizontal = Space.gutter, vertical = 12.dp),
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            devices.forEach { d ->
                val now = d.state?.current()
                NavRow(
                    d.name, { picked = if (picked == d.id) null else d.id },
                    subtitle = now?.let { "${it.title} · ${it.artist}" } ?: words(R.string.devices_idle),
                    trailing = if (d.nearby) words(R.string.devices_nearby) else null,
                    leading = { Icon(kindIcon(d.kind), null) },
                    divider = picked != d.id,
                )
                if (picked == d.id) DeviceControls(d, vm)
            }
            if (jams) ActionRow(words(if (jam?.hosting == true) R.string.jam_yours else R.string.jam_start), Icons.Filled.Groups, { onDismiss(); nav.jam() })
        }
    }
}

@Composable
private fun DeviceControls(d: RemoteDevice, vm: RemoteViewModel) {
    val st = d.state ?: return
    Column(Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 4.dp)) {
        d.refused?.let { Text(refusal(it), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium) }
        Row(Modifier.fillMaxWidth(), Arrangement.spacedBy(24.dp, Alignment.CenterHorizontally), Alignment.CenterVertically) {
            IconButton({ vm.send(d.id, Op.Previous) }) { Icon(Icons.Filled.FastRewind, say.previous, Modifier.size(34.dp)) }
            IconButton({ vm.send(d.id, if (st.playing) Op.Pause else Op.Play) }, Modifier.size(56.dp)) {
                PlayPauseGlyph(st.playing, false, 44.dp, 18.dp)
            }
            IconButton({ vm.send(d.id, Op.Next) }) { Icon(Icons.Filled.FastForward, say.next, Modifier.size(34.dp)) }
        }
        st.volume?.let { v ->
            var shown by remember(d.id) { mutableFloatStateOf(v.toFloat()) }
            var moved by remember(d.id) { mutableStateOf(false) }
            // Sent once the finger rests, not for every step of the drag.
            LaunchedEffect(shown, moved) {
                if (!moved) return@LaunchedEffect
                delay(250)
                vm.send(d.id, Op.Volume(shown.toInt().toUByte()))
            }
            NoriSlider(shown, 0f..100f, { shown = it; moved = true })
        }
        Row(Modifier.fillMaxWidth().padding(vertical = 8.dp), Arrangement.spacedBy(10.dp)) {
            PillButton(words(R.string.devices_play_here), null, { vm.playHere(d.id) }, Modifier.weight(1f), prominent = true)
            PillButton(words(R.string.devices_play_there), null, { vm.playThere(d.id) }, Modifier.weight(1f))
        }
        val next = st.upNext().take(8)
        if (next.isNotEmpty()) Caption(words(R.string.devices_up_next), Modifier.padding(top = 6.dp, bottom = 2.dp))
        next.forEach { e ->
            Row(Modifier.fillMaxWidth().clickable { vm.send(d.id, Op.Jump(e.index, st.rev)) }.padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(e.title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(e.artist, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                }
                IconButton({ vm.send(d.id, Op.Remove(e.index, st.rev)) }) { Icon(Icons.Filled.Close, say.remove, Modifier.size(18.dp)) }
            }
        }
        Hairline(startIndent = 0.dp)
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
    DisposableEffect(Unit) { vm.watch(true); onDispose { vm.watch(false) } }
    val jam by vm.jam.collectAsStateWithLifecycle()
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
                    Text(words(R.string.settings_jam_detail), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    PillButton(words(R.string.jam_start), Icons.Filled.Groups, vm::jamStart, Modifier.padding(top = 16.dp), prominent = true)
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
