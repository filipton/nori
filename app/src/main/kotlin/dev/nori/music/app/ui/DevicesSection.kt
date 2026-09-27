package dev.nori.music.app.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandHorizontally
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkHorizontally
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.nori.music.app.vm.DeviceRow
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.playback.DeviceSound

/**
 * The equalizer's list of output devices, each with the sound it gets: the one playing now, the phone
 * speaker, and every pair of headphones or DAC seen before. Tapping one chooses its sound.
 */
@Composable
fun DevicesSection(vm: SettingsViewModel) {
    val rows by vm.deviceRows.collectAsStateWithLifecycle()
    val p by vm.prefs.collectAsStateWithLifecycle()
    var picking by remember { mutableStateOf<String?>(null) }
    NoriSheet(rows.firstOrNull { it.output == picking }, { picking = null }, skipPartiallyExpanded = false) { d -> DeviceSheet(vm, d) { picking = null } }

    SectionTitle(say.devices)
    AnimatedRows(rows, { it.output }) { d -> DeviceItem(d) { vm.clearAssignError(); picking = d.output } }
    Toggle(
        say.autoeqAuto,
        say.autoeqAutoDetail,
        p.autoEqAuto,
    ) { on -> vm.update { it.copy(autoEqAuto = on) } }
    AnimatedVisibility(!p.profilePerOutput, enter = fadeIn(tween(motion())) + expandVertically(tween(motion())), exit = fadeOut(tween(motion())) + shrinkVertically(tween(motion()))) {
        Text(
            say.devicesNote,
            Modifier.padding(horizontal = Space.gutter, vertical = 6.dp),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error,
        )
    }
}

@Composable
private fun motion(ms: Int = 220) = if (reduceMotion()) 0 else ms

@Composable
private fun DeviceItem(d: DeviceRow, onClick: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Column {
        Row(
            Modifier.fillMaxWidth().clickable(onClick = onClick).padding(start = Space.gutter, end = 10.dp, top = 11.dp, bottom = 11.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(d.name, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Row {
                    d.kind?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant) }
                    AnimatedVisibility(d.current, enter = fadeIn(tween(motion())) + expandHorizontally(tween(motion())), exit = fadeOut(tween(motion())) + shrinkHorizontally(tween(motion()))) {
                        val playing = remember(d.kind != null) { say.devicePlayingNow(d.kind != null) }
                        Text(playing, style = MaterialTheme.typography.bodySmall, color = scheme.primary)
                    }
                }
            }
            // The sound it gets cross-fades when it changes rather than being swapped in one frame.
            Crossfade(d.sound, Modifier.padding(start = 12.dp), animationSpec = tween(motion()), label = "sound") { sound ->
                Text(sound, style = MaterialTheme.typography.bodyMedium, color = scheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null, Modifier.padding(start = 6.dp).size(19.dp), tint = scheme.onSurfaceVariant.copy(alpha = 0.7f))
        }
        Hairline()
    }
}

private class Shown<T>(item: T, val key: String, val state: MutableTransitionState<Boolean>) {
    var item by mutableStateOf(item)
}

/**
 * A short list whose rows unfold in when they join and fold away when they leave. What is there when it
 * first draws is simply there; only later changes move.
 */
@Composable
internal fun <T> AnimatedRows(items: List<T>, key: (T) -> String, content: @Composable (T) -> Unit) {
    val holder = remember { arrayOfNulls<List<Shown<T>>>(1) }
    val rows = remember(items) {
        val old = holder[0]
        val byKey = old.orEmpty().associateBy { it.key }
        val next = items.map { item ->
            val k = key(item)
            byKey[k]?.also { it.item = item; it.state.targetState = true }
                ?: Shown(item, k, MutableTransitionState(old == null).apply { targetState = true })
        }.toMutableList()
        val keys = next.mapTo(HashSet()) { it.key }
        old.orEmpty().forEachIndexed { i, r ->
            if (r.key !in keys && (r.state.currentState || r.state.targetState)) {
                r.state.targetState = false
                next.add(minOf(i, next.size), r)
            }
        }
        holder[0] = next
        next
    }
    val ms = motion()
    rows.forEach { r ->
        key(r.key) {
            AnimatedVisibility(r.state, enter = fadeIn(tween(ms)) + expandVertically(tween(ms)), exit = fadeOut(tween(ms)) + shrinkVertically(tween(ms))) {
                content(r.item)
            }
        }
    }
}

/** What one device gets: nothing chosen, flat, left alone, a saved profile, or a curve from AutoEQ. The inside of a [NoriSheet]. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DeviceSheet(vm: SettingsViewModel, d: DeviceRow, onDone: () -> Unit) {
    val p by vm.prefs.collectAsStateWithLifecycle()
    val profiles by vm.profiles.collectAsStateWithLifecycle()
    val eq by vm.autoEq.collectAsStateWithLifecycle()
    val busy by vm.assigning.collectAsStateWithLifecycle()
    val error by vm.assignError.collectAsStateWithLifecycle()
    val suggested by produceState(emptyList<dev.nori.music.app.vm.AutoEqHit>(), d.output, eq.count) { value = vm.autoEqFor(d.output) }
    var query by remember { mutableStateOf("") }
    // Which profiles it offers and whether it can be forgotten are the core's (device_sheet); what it says, Say's.
    val sheet = remember(d.output, d.current, profiles) {
        dev.nori.music.ffi.devices.deviceSheet(d.output, d.current, profiles.map { it.name })
    }
    val intro = remember(d.kind) { say.deviceIntro(d.kind) }
    val automatic = remember(p.autoEqAuto) { say.deviceAutomatic(p.autoEqAuto) }
    val curves = if (!eq.tooShort && eq.query == query) eq.hits else suggested
    val pick = { c: DeviceSound.Choice -> vm.assignDevice(d.output, c, onDone) }
    val ms = motion()

    LazyColumn(Modifier.navigationBarsPadding()) {
        item("head") {
            Column(Modifier.padding(bottom = 4.dp)) {
                LargeTitle(d.name)
                Text(
                    intro,
                    Modifier.padding(horizontal = Space.gutter), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        item("auto") {
            Option(
                say.automatic,
                automatic,
                d.choice == DeviceSound.Choice.Automatic,
            ) { pick(DeviceSound.Choice.Automatic) }
        }
        item("flat") { Option(say.flat, say.flatDetail, d.choice == DeviceSound.Choice.Flat) { pick(DeviceSound.Choice.Flat) } }
        item("bypass") { Option(say.noProcessing, say.noProcessingDetail, d.choice == DeviceSound.Choice.Bypass) { pick(DeviceSound.Choice.Bypass) } }
        item("quiet") { Option(say.leaveAsIs, say.leaveAsIsDetail, d.choice == DeviceSound.Choice.Quiet) { pick(DeviceSound.Choice.Quiet) } }
        items(sheet.profiles, key = { "p:$it" }) { name ->
            Option(name, say.savedProfile, d.choice == DeviceSound.Choice.Profile(name), Modifier.animateItem()) { pick(DeviceSound.Choice.Profile(name)) }
        }
        item("curves") { SectionHeader(say.autoeqCurves) }
        if (eq.count == 0) item("download") {
            Column(Modifier.padding(horizontal = Space.gutter)) {
                Text(
                    say.autoeqDownloadHint,
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Row(Modifier.padding(vertical = 10.dp).heightIn(min = 42.dp), verticalAlignment = Alignment.CenterVertically) {
                    PillButton(say.downloadTheList, null, vm::downloadAutoEqIndex, prominent = true, enabled = !eq.busy)
                    AnimatedVisibility(eq.busy, enter = fadeIn(tween(ms)), exit = fadeOut(tween(ms))) { LoadingDots(Modifier.padding(start = 14.dp)) }
                }
            }
        } else item("search") {
            SearchField(query, { q -> query = q; vm.searchAutoEq(q) }, eq.searchWords, Modifier.padding(horizontal = Space.gutter, vertical = 6.dp))
        }
        items(curves, key = { "c:" + it.entry.path }) { e ->
            Option(e.entry.name, e.short, false, Modifier.animateItem()) { pick(DeviceSound.Choice.Curve(e.entry)) }
        }
        item("status") {
            Column {
                AnimatedVisibility(busy == d.output, enter = fadeIn(tween(ms)) + expandVertically(tween(ms)), exit = fadeOut(tween(ms)) + shrinkVertically(tween(ms))) {
                    Box(Modifier.fillMaxWidth().padding(12.dp), Alignment.Center) { LoadingDots() }
                }
                AnimatedVisibility(error != null || eq.error != null, enter = fadeIn(tween(ms)) + expandVertically(tween(ms)), exit = fadeOut(tween(ms)) + shrinkVertically(tween(ms))) {
                    Text(error ?: eq.error.orEmpty(), Modifier.padding(horizontal = Space.gutter, vertical = 8.dp), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                }
            }
        }
        if (sheet.canForget) item("forget") {
            Column(Modifier.padding(top = 12.dp)) {
                Hairline()
                ActionRow(say.forgetDevice, Icons.Outlined.Delete, { vm.forgetDevice(d.output); onDone() }, divider = false)
            }
        }
    }
}

@Composable
private fun Option(title: String, subtitle: String?, selected: Boolean, modifier: Modifier = Modifier, onClick: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    Column(modifier) {
        Row(Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = Space.gutter, vertical = 11.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                if (!subtitle.isNullOrEmpty()) Text(subtitle, style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            AnimatedVisibility(selected, enter = fadeIn(tween(motion())), exit = fadeOut(tween(motion()))) {
                Icon(Icons.Filled.Check, say.chosen, Modifier.size(20.dp), tint = scheme.primary)
            }
        }
        Hairline()
    }
}
