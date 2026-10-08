package dev.nori.music.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Groups
import androidx.compose.material.icons.filled.IosShare
import androidx.compose.material.icons.filled.PersonAdd
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorProducer
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.R
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.app.vm.RemoteViewModel
import dev.nori.music.ffi.JamView
import dev.nori.music.ffi.remote.JamMember
import dev.nori.music.ffi.remote.Pending
import dev.nori.music.ffi.remote.Role
import dev.nori.music.look.CoverLook

/*
 * The jam, inside the player the way Spotify's Jam and Apple's SharePlay sit in theirs: a strip under the
 * song while one is on, a header over the queue (who listens, Invite, End), the requests under it, and
 * who asked for each song on its row. Everything is the core's (nori-core remote.rs); this words and draws it.
 */

/** "Jam · 2 listening", or "Jam · no one yet". */
@Composable
internal fun jamListening(n: Int): String = if (n == 0) words(R.string.jam_no_one) else words(R.string.jam_listening, n)

/**
 * "Jam · 2 listening" under the song, in the mini player and the full one, while this phone hosts a jam, or
 * "Jam · Filip · 2 listening" in one it is a guest in ([jam] is null without one): a tap opens the queue
 * with the jam's header.
 */
@Composable
fun JamStrip(jam: PlayerViewModel.JamStripState?, color: Color, modifier: Modifier = Modifier) {
    jam ?: return
    val nav = LocalNav.current
    val listening = jamListening(jam.listening)
    val label = if (jam.host == null) words(R.string.jam_strip, listening) else words(R.string.jam_strip_guest, jam.host, listening)
    Row(
        modifier.clickable { nav.player(Panel.QUEUE) }.padding(horizontal = 12.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Filled.Groups, null, Modifier.size(15.dp), tint = color)
        Text(
            label, Modifier.padding(start = 6.dp),
            style = MaterialTheme.typography.labelMedium, color = color, maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}

/** A disc's colour for [name]: one of a few, the same for the same name everywhere. */
private fun avatarColour(name: String): Color = AVATARS[Math.floorMod(name.hashCode(), AVATARS.size)]

private val AVATARS = listOf(
    Color(0xFFF2994A), Color(0xFF27AE60), Color(0xFF2D9CDB), Color(0xFF9B51E0),
    Color(0xFFEB5757), Color(0xFF00A3A3), Color(0xFFD69E00), Color(0xFFE0569B),
)

/** A person as a disc with their initial. */
@Composable
internal fun Avatar(name: String, size: Dp, modifier: Modifier = Modifier) {
    val initial = remember(name) { name.trim().let { n -> if (n.isEmpty()) "?" else String(Character.toChars(n.codePointAt(0))).uppercase() } }
    Box(modifier.size(size).clip(CircleShape).background(avatarColour(name)), contentAlignment = Alignment.Center) {
        // Trimmed to the letter itself, so it sits in the middle of the smallest discs too.
        val type = (size.value * 0.44f).sp
        Text(
            initial, color = Color.White, maxLines = 1,
            style = androidx.compose.ui.text.TextStyle(
                fontSize = type, lineHeight = type, fontWeight = FontWeight.SemiBold,
                lineHeightStyle = androidx.compose.ui.text.style.LineHeightStyle(
                    androidx.compose.ui.text.style.LineHeightStyle.Alignment.Center, androidx.compose.ui.text.style.LineHeightStyle.Trim.Both,
                ),
            ),
        )
    }
}

/** The first few listeners' discs, overlapping, each ringed in [ring] so they read as separate. */
@Composable
private fun Avatars(names: List<String>, ring: ColorProducer) {
    Row(horizontalArrangement = Arrangement.spacedBy((-8).dp)) {
        names.take(AVATARS_SHOWN).forEach { n ->
            Box(
                Modifier.size(28.dp).drawWithCache { onDrawBehind { drawCircle(ring(), radius = size.minDimension / 2f) } },
                contentAlignment = Alignment.Center,
            ) { Avatar(n, 24.dp) }
        }
    }
}

/** How many listeners' discs the header shows before it lets the count speak. */
private const val AVATARS_SHOWN = 4

/** Who asked for a queue's song: their disc and name, a small chip before the artist. */
@Composable
internal fun AddedBy(name: String, ink: ColorProducer, plate: ColorProducer, modifier: Modifier = Modifier) {
    val said = words(R.string.jam_added_by, name)
    Row(
        modifier.padding(end = 6.dp).clip(RoundedCornerShape(50)).drawWithCache { onDrawBehind { drawRect(plate()) } }
            .padding(start = 2.dp, end = 7.dp, top = 1.dp, bottom = 1.dp)
            .semantics(mergeDescendants = true) { contentDescription = said },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Avatar(name, 14.dp)
        LookText(name, ink, Modifier.padding(start = 4.dp), style = MaterialTheme.typography.labelSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/**
 * The jam this phone hosts, over its queue: the listeners (a tap opens People), Invite and End, and what
 * guests asked for, to accept or refuse. Drawn in the player's own colours.
 */
@Composable
internal fun JamHeader(j: JamView, cover: (String?) -> String?) {
    val vm: RemoteViewModel = viewModel()
    val look = LocalLook.current
    val ink = ColorProducer { look.color(CoverLook.ON) }
    val quiet = ColorProducer { look.color(CoverLook.ON_VARIANT) }
    val accent = ColorProducer { look.color(CoverLook.ACCENT) }
    var inviting by remember { mutableStateOf(false) }
    var people by remember { mutableStateOf(false) }
    val listeners = remember(j.members) { j.members.filter { it.role != Role.HOST } }
    Column(Modifier.fillMaxWidth().padding(bottom = 6.dp)) {
        Row(Modifier.fillMaxWidth().padding(top = 2.dp), verticalAlignment = Alignment.CenterVertically) {
            LookIcon(Icons.Filled.Groups, null, Modifier.size(22.dp), accent)
            LookText(
                words(R.string.jam_title), ink, Modifier.weight(1f).padding(start = 8.dp),
                style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold), maxLines = 1,
            )
            PillButton(words(R.string.jam_invite_button), Icons.Filled.PersonAdd, { inviting = true }, prominent = true)
        }
        Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Row(
                Modifier.weight(1f).clip(RoundedCornerShape(50)).clickable { people = true }.padding(vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Avatars(remember(listeners) { listeners.map { it.name } }) { look.color(CoverLook.BACKGROUND) }
                LookText(
                    jamListening(listeners.size), quiet, Modifier.padding(start = if (listeners.isEmpty()) 0.dp else 8.dp),
                    style = MaterialTheme.typography.bodyMedium, maxLines = 1,
                )
                LookIcon(Icons.AutoMirrored.Filled.KeyboardArrowRight, words(R.string.jam_people), Modifier.size(18.dp), quiet)
            }
            LookText(
                words(R.string.jam_end), accent,
                Modifier.clip(RoundedCornerShape(50)).clickable(onClick = { vm.jamEnd() }).padding(horizontal = 8.dp, vertical = 6.dp),
                style = MaterialTheme.typography.labelLarge.copy(fontWeight = FontWeight.SemiBold), maxLines = 1,
            )
        }
        if (j.pending.isNotEmpty()) {
            Caption(words(R.string.jam_requests), Modifier.padding(top = 12.dp, bottom = 2.dp))
            // A long line of requests scrolls within its own room, and the queue keeps the rest.
            Column(Modifier.heightIn(max = 232.dp).verticalScroll(rememberScrollState())) {
                j.pending.forEach { p -> key(p.request) { Request(p, cover(p.song.coverArt), decides = true, ink, quiet, vm) } }
            }
        }
    }
    InviteSheet(j.link.takeIf { inviting }) { inviting = false }
    PeopleSheet(people, j, { people = false; inviting = true }) { people = false }
}

/**
 * The jam this phone is a guest in, over the host's queue: whose it is, who listens (a tap opens People),
 * Leave, and the songs asked for here until the host decides. Drawn in the player's own colours.
 */
@Composable
internal fun GuestJamHeader(j: JamView, cover: (String?) -> String?) {
    val vm: RemoteViewModel = viewModel()
    val look = LocalLook.current
    val ink = ColorProducer { look.color(CoverLook.ON) }
    val quiet = ColorProducer { look.color(CoverLook.ON_VARIANT) }
    val accent = ColorProducer { look.color(CoverLook.ACCENT) }
    var people by remember { mutableStateOf(false) }
    val host = remember(j.members) { j.members.firstOrNull { it.role == Role.HOST } }
    val listeners = remember(j.members) { j.members.filter { it.role != Role.HOST } }
    val mine = remember(j.pending, j.you) { j.pending.filter { it.from == j.you } }
    Column(Modifier.fillMaxWidth().padding(bottom = 6.dp)) {
        // The host's Invite stands at this row's end; a guest's "Listen here" will.
        Row(Modifier.fillMaxWidth().padding(top = 2.dp).heightIn(min = 40.dp), verticalAlignment = Alignment.CenterVertically) {
            LookIcon(Icons.Filled.Groups, null, Modifier.size(22.dp), accent)
            LookText(
                host?.let { words(R.string.jam_of, it.name) } ?: words(R.string.jam_title), ink, Modifier.weight(1f).padding(start = 8.dp),
                style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.SemiBold), maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        }
        Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Row(
                Modifier.weight(1f).clip(RoundedCornerShape(50)).clickable { people = true }.padding(vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Avatars(remember(j.members) { j.members.map { it.name } }) { look.color(CoverLook.BACKGROUND) }
                LookText(jamListening(listeners.size), quiet, Modifier.padding(start = 8.dp), style = MaterialTheme.typography.bodyMedium, maxLines = 1)
                LookIcon(Icons.AutoMirrored.Filled.KeyboardArrowRight, words(R.string.jam_people), Modifier.size(18.dp), quiet)
            }
            LookText(
                words(R.string.jam_leave), accent,
                Modifier.clip(RoundedCornerShape(50)).clickable(onClick = { vm.leave() }).padding(horizontal = 8.dp, vertical = 6.dp),
                style = MaterialTheme.typography.labelLarge.copy(fontWeight = FontWeight.SemiBold), maxLines = 1,
            )
        }
        j.refused?.let { r ->
            Text(refusal(r), Modifier.padding(top = 8.dp), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
        }
        if (mine.isNotEmpty()) {
            Caption(words(R.string.jam_yours_waiting), Modifier.padding(top = 12.dp, bottom = 2.dp))
            Column(Modifier.heightIn(max = 232.dp).verticalScroll(rememberScrollState())) {
                mine.forEach { p -> key(p.request) { Request(p, cover(p.song.coverArt), decides = false, ink, quiet, vm) } }
            }
        }
    }
    PeopleSheet(people, j, {}) { people = false }
}

/**
 * A song asked for: its cover, who asked and, for a provider's song, that accepting it downloads it to the
 * server. The host and admins ([decides]) refuse or accept it with the two discs.
 */
@Composable
private fun Request(p: Pending, coverUrl: String?, decides: Boolean, ink: ColorProducer, quiet: ColorProducer, vm: RemoteViewModel) {
    Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Cover(coverUrl, 44.dp, radius = 6.dp)
        Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
            LookText(p.song.title, ink, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            LookText(
                if (decides) words(R.string.jam_asked_by, p.fromName) else words(R.string.jam_waiting_for_host), quiet,
                style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            if (p.provider && decides) LookText(words(R.string.jam_will_download), quiet, style = MaterialTheme.typography.bodySmall, maxLines = 2)
        }
        if (decides) {
            CircleButton(Icons.Filled.Close, words(R.string.jam_decline), Modifier.size(36.dp)) { vm.accept(p.request, false) }
            Spacer(Modifier.width(10.dp))
            CircleButton(Icons.Filled.Check, words(R.string.jam_accept), Modifier.size(36.dp), lit = true) { vm.accept(p.request, true) }
        }
    }
}

/** A sheet in the colours of the theme it is shown in, wherever it was opened from (the player wears a cover's). */
@Composable
private fun SheetLook(content: @Composable () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    val look = remember(scheme) { FixedLook(plainLook(scheme)) }
    androidx.compose.runtime.CompositionLocalProvider(LocalLook provides look, content = content)
}

/** The invite: a large QR code, the link, Copy and Share. */
@Composable
private fun InviteSheet(link: String?, onDismiss: () -> Unit) {
    NoriSheet(link, onDismiss) { l ->
        SheetLook { InviteBody(l) }
    }
}

@Composable
private fun InviteBody(link: String) {
    val vm: RemoteViewModel = viewModel()
    val qr by produceState<dev.nori.music.ffi.remote.QrCode?>(null, link) { value = vm.qr(link) }
    val context = LocalContext.current
    val copied = words(R.string.jam_link_copied)
    Column(
        Modifier.fillMaxWidth().navigationBarsPadding().padding(start = Space.gutter, end = Space.gutter, bottom = 24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(words(R.string.jam_invite_title), style = MaterialTheme.typography.titleLarge.copy(fontWeight = FontWeight.SemiBold))
        // White under the code whatever the theme, as a scanner wants it; the room is kept while it is made.
        Box(Modifier.padding(top = 20.dp).clip(RoundedCornerShape(20.dp)).background(Color.White).padding(18.dp).size(232.dp)) {
            qr?.let { code ->
                Spacer(Modifier.fillMaxSize().drawWithCache {
                    val n = code.size.toInt()
                    val cell = size.width / n
                    val path = Path()
                    for (y in 0 until n) for (x in 0 until n) {
                        if (code.dark[y * n + x]) path.addRect(androidx.compose.ui.geometry.Rect(Offset(x * cell, y * cell), Size(cell + 0.5f, cell + 0.5f)))
                    }
                    onDrawBehind { drawPath(path, Color.Black) }
                })
            }
        }
        Text(
            words(R.string.jam_invite), Modifier.padding(top = 16.dp), textAlign = TextAlign.Center,
            style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        remember(vm) { vm.homeOnly() }?.let { address ->
            Text(
                words(R.string.jam_home_only, address), Modifier.padding(top = 8.dp), textAlign = TextAlign.Center,
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error,
            )
        }
        Text(
            link, Modifier.padding(top = 14.dp).fillMaxWidth().clip(CardShape).background(LocalLook.current.color(CoverLook.FIELD))
                .padding(horizontal = 14.dp, vertical = 10.dp),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1, overflow = TextOverflow.MiddleEllipsis,
        )
        Row(Modifier.fillMaxWidth().padding(top = 16.dp), Arrangement.spacedBy(10.dp)) {
            PillButton(words(R.string.jam_copy_link), Icons.Filled.ContentCopy, {
                val clip = context.getSystemService(android.content.ClipboardManager::class.java)
                clip?.setPrimaryClip(android.content.ClipData.newPlainText(link, link))
                // Android 13 and later show the copy themselves.
                if (android.os.Build.VERSION.SDK_INT < 33) android.widget.Toast.makeText(context, copied, android.widget.Toast.LENGTH_SHORT).show()
            }, Modifier.weight(1f), prominent = true)
            PillButton(words(R.string.jam_share), Icons.Filled.IosShare, {
                val send = android.content.Intent(android.content.Intent.ACTION_SEND).setType("text/plain").putExtra(android.content.Intent.EXTRA_TEXT, link)
                context.startActivity(android.content.Intent.createChooser(send, null))
            }, Modifier.weight(1f))
        }
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

/**
 * Who is in the jam: the host makes a guest an admin (or a guest again) and sends people out; a guest sees
 * who is in.
 */
@Composable
private fun PeopleSheet(open: Boolean, j: JamView, onInvite: () -> Unit, onDismiss: () -> Unit) {
    NoriSheet(open, onDismiss) {
        SheetLook {
            val vm: RemoteViewModel = viewModel()
            val guests = j.members.filter { it.role != Role.HOST }
            Column(Modifier.verticalScroll(rememberScrollState()).navigationBarsPadding().padding(bottom = 16.dp)) {
                SectionTitle(words(R.string.jam_people))
                j.members.firstOrNull { it.role == Role.HOST }?.let { h -> Person(h, if (j.hosting) words(R.string.jam_you_host) else roleName(Role.HOST)) {} }
                guests.forEach { m ->
                    key(m.id) {
                        val role = roleName(m.role)
                        Person(m, if (m.id == j.you) words(R.string.jam_you_role, role) else role) {
                            if (j.hosting) {
                                Chip(words(if (m.role == Role.ADMIN) R.string.jam_make_guest else R.string.jam_make_admin), false) { vm.promote(m.id, m.role != Role.ADMIN) }
                                IconButton({ vm.remove(m.id) }) {
                                    Icon(Icons.Filled.Close, words(R.string.jam_send_out), Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                                }
                            }
                        }
                    }
                }
                if (guests.isEmpty() && j.hosting) {
                    Text(
                        words(R.string.jam_people_none), Modifier.padding(horizontal = Space.gutter, vertical = 12.dp),
                        style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    PillButton(words(R.string.jam_invite_button), Icons.Filled.PersonAdd, onInvite, Modifier.padding(horizontal = Space.gutter), prominent = true)
                }
            }
        }
    }
}

/** One person of the jam, [line] under their name, [actions] at the end. */
@Composable
private fun Person(m: JamMember, line: String, actions: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit) {
    Row(Modifier.fillMaxWidth().padding(start = Space.gutter, end = 8.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Avatar(m.name, 40.dp)
        Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
            Text(m.name, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(line, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
        }
        actions()
    }
}

/**
 * "Start a Jam" for a page's ⋯ ([play] plays the page first), or null where none can start: jams off,
 * a server without the relay, or one already on.
 */
@Composable
internal fun jamStartEntry(play: () -> Unit): Pair<String, () -> Unit>? {
    val vm: RemoteViewModel = viewModel()
    val can by vm.canStartJam.collectAsStateWithLifecycle()
    if (!can) return null
    val nav = LocalNav.current
    return words(R.string.jam_start) to { play(); vm.jamStart(); nav.player(Panel.QUEUE) }
}

/** Says a jam that could not start, wherever one was started from. */
@Composable
internal fun JamFailures() {
    val vm: RemoteViewModel = viewModel()
    val context = LocalContext.current
    LaunchedEffect(vm) {
        vm.jamFailed.collect { android.widget.Toast.makeText(context, R.string.jam_failed, android.widget.Toast.LENGTH_LONG).show() }
    }
}
