package dev.nori.music.app.ui

import android.content.Intent
import android.media.audiofx.AudioEffect
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.CloudDownload
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.LibraryMusic
import androidx.compose.material.icons.outlined.Lyrics
import androidx.compose.material.icons.outlined.Palette
import androidx.compose.material.icons.outlined.PlayCircle
import androidx.compose.material.icons.outlined.Speed
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import kotlinx.coroutines.launch
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.zIndex
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInWindow
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.app.R
import dev.nori.music.app.vm.SettingRow
import dev.nori.music.app.vm.SettingsSection
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.stringResource
import dev.nori.music.ffi.settings.SavedServer

/**
 * Search lands on a row, not on a page: the group page is told which row to reveal, the row reports
 * where it is, and the page scrolls there and lets the highlight fade out. A row's key is its title
 * slugged (`settingKey`), the same key the search result carries.
 */
class SettingSpotlight(val key: String?, val onPlaced: (Int) -> Unit)

val LocalSpotlight = androidx.compose.runtime.compositionLocalOf { SettingSpotlight(null) {} }

/** The wash that says "this is the one you searched for", fading out once you have seen it. */
@Composable
private fun Modifier.spotlight(key: String?): Modifier {
    val spot = LocalSpotlight.current
    val on = key != null && spot.key == key
    if (!on) return this
    val fade = remember { androidx.compose.animation.core.Animatable(1f) }
    LaunchedEffect(Unit) {
        kotlinx.coroutines.delay(900)
        fade.animateTo(0f, androidx.compose.animation.core.tween(1400))
    }
    val colour = MaterialTheme.colorScheme.primary
    return this
        .onGloballyPositioned { spot.onPlaced(it.positionInWindow().y.toInt()) }
        .drawBehind { drawRect(colour.copy(alpha = 0.22f * fade.value)) }
}

@Composable
fun Toggle(title: String, detail: String, value: Boolean, enabled: Boolean = true, key: String? = null, onChange: (Boolean) -> Unit) {
    Column {
    Row(
        Modifier.fillMaxWidth().spotlight(key).clickable(enabled) { onChange(!value) }
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f).padding(end = 14.dp).alpha(if (enabled) 1f else DIMMED)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        NoriSwitch(value, onChange, enabled = enabled)
    }
    Hairline(startIndent = 16.dp)
    }
}

/**
 * One setting with a switch per place it applies to: its title and line like a [Toggle]'s, then the
 * places as chips, lit when on - the same chips search's scopes are, rather than a stack of near-identical
 * rows that read as separate settings.
 */
@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun Picks(row: SettingRow.Picks, onChange: (String, Boolean) -> Unit) {
    Column {
        Column(Modifier.fillMaxWidth().spotlight(row.key).padding(horizontal = 16.dp, vertical = 12.dp)) {
            Text(row.title, style = MaterialTheme.typography.bodyLarge)
            Text(row.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            androidx.compose.foundation.layout.FlowRow(
                Modifier.padding(top = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp),
            ) { row.picks.forEach { p -> Chip(p.label, p.on) { onChange(p.name, !p.on) } } }
        }
        Hairline(startIndent = 16.dp)
    }
}

/** One setting with a few values as chips, the one it has lit; a tap chooses another. See [Picks]. */
@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun ChipChoice(row: SettingRow.ChipChoice, onChange: (String) -> Unit) {
    Column {
        Column(Modifier.fillMaxWidth().spotlight(row.key).padding(horizontal = 16.dp, vertical = 12.dp)) {
            Text(row.title, style = MaterialTheme.typography.bodyLarge)
            Text(row.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            androidx.compose.foundation.layout.FlowRow(
                Modifier.padding(top = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp),
            ) { row.options.forEach { o -> Chip(o.label, o.value == row.chosen) { if (o.value != row.chosen) onChange(o.value) } } }
        }
        Hairline(startIndent = 16.dp)
    }
}

/** What a setting the app is going to ignore looks like: still there, still readable, plainly not live. */
private const val DIMMED = 0.38f

@Composable
private fun Choice(row: SettingRow.Choice, onChange: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    val dim = if (row.enabled) 1f else DIMMED
    Column {
    Row(Modifier.fillMaxWidth().spotlight(row.key).clickable(row.enabled) { open = true }.padding(horizontal = 16.dp, vertical = 15.dp)) {
        Text(row.title, Modifier.weight(1f).alpha(dim), style = MaterialTheme.typography.bodyLarge)
        Text(row.shown, Modifier.alpha(dim), color = MaterialTheme.colorScheme.primary)
        DropdownMenu(open, { open = false }) { row.options.forEach { o -> DropdownMenuItem({ Text(o.label) }, { onChange(o.value); open = false }) } }
    }
    Hairline(startIndent = 16.dp)
    }
}

/**
 * The rounded plate the rows of a group sit on. The content colour is spelled out: Material resolves
 * it to Unspecified for any colour it does not recognise as one of its own roles, and text inside then
 * renders almost black - which is why every setting's title was dimmer than its own description.
 */
@Composable
private fun SettingsCard(content: @Composable ColumnScope.() -> Unit) {
    Surface(
        shape = CardShape,
        color = LocalLook.current.color(dev.nori.music.look.CoverLook.FORM),
        contentColor = MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 6.dp),
    ) { Column(content = content) }
}

/** Each group's icon; the groups themselves, their order and their words are SettingsPages.kt's. */
private fun groupIcon(id: String): ImageVector = when (id) {
    "servers" -> Icons.Outlined.Dns
    "playing" -> Icons.Outlined.PlayCircle
    "sound" -> Icons.Outlined.GraphicEq
    "look" -> Icons.Outlined.Palette
    "lyrics" -> Icons.Outlined.Lyrics
    "library" -> Icons.Outlined.LibraryMusic
    "data" -> Icons.Outlined.CloudDownload
    "about" -> Icons.Outlined.Info
    else -> Icons.Outlined.AutoAwesome
}

/**
 * A titled block of rows on its own plate. A group page used to be one plate of twenty rows, which is
 * where things got lost; a few short, named sections are what make a page scannable.
 */
@Composable
private fun Section(title: String, content: @Composable ColumnScope.() -> Unit) {
    Text(
        title.uppercase(),
        Modifier.padding(start = 30.dp, end = 30.dp, top = 16.dp, bottom = 2.dp),
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    SettingsCard(content)
}

/** A row with a line of text and a button at its end: a count, an action. */
/**
 * What the compressor is taking off right now: a line and a bar. Read from the player only while this
 * page is resumed, the way the equalizer screen reads the limiter's meter, and dropped the moment it is
 * not: nothing ticks with the screen off. Its own scope, so a reading redraws only this row.
 */
@Composable
private fun CompressionMeter() {
    var db by remember { mutableFloatStateOf(0f) }
    var resumed by remember { mutableStateOf(false) }
    androidx.lifecycle.compose.LifecycleResumeEffect(Unit) { resumed = true; onPauseOrDispose { resumed = false } }
    LaunchedEffect(resumed) {
        while (resumed) {
            db = dev.nori.music.playback.Equalizer.compressionDb
            kotlinx.coroutines.delay(stage.meterMs)
        }
    }
    val active = db > 0.05f
    val scheme = MaterialTheme.colorScheme
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        // The words change only with the tenth of a dB they show; the bar moves far more finely.
        Text(
            remember(kotlin.math.round(db * 10f)) { say.compression(db) },
            style = MaterialTheme.typography.labelMedium,
            color = if (active) scheme.primary else scheme.onSurfaceVariant,
        )
        val track = scheme.surfaceVariant
        val fill = scheme.primary
        // Full width is 20 dB of reduction, more than any preset takes off music.
        Spacer(
            Modifier.padding(top = 6.dp).fillMaxWidth().height(4.dp).drawBehind {
                val r = androidx.compose.ui.geometry.CornerRadius(size.height / 2f)
                drawRoundRect(track, cornerRadius = r)
                val w = size.width * (db / 20f).coerceIn(0f, 1f)
                if (w > 0f) drawRoundRect(fill, size = size.copy(width = w), cornerRadius = r)
            },
        )
    }
}

@Composable
private fun ActionRowSetting(row: SettingRow.Action, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().spotlight(row.key).padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(row.title, style = MaterialTheme.typography.bodyLarge)
            Text(row.detail, style = MaterialTheme.typography.bodySmall, color = if (row.error) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
        }
        TextButton(onClick, enabled = row.enabled) { Text(row.button) }
    }
    Hairline(startIndent = 16.dp)
}

@Composable
fun SettingsScreen(vm: SettingsViewModel) {
    val nav = LocalNav.current
    var query by remember { mutableStateOf("") }
    val res = LocalContext.current.resources
    val config = LocalConfiguration.current
    // Titles first, then anything whose explanation mentions it: searching "oled" finds AMOLED black,
    // asked once per change of the query (or of the language).
    val hits = remember(query, config) { if (query.isBlank()) emptyList() else vm.searchSettings(query, res) }
    val groups = remember(config) { vm.settingsGroups(res) }
    Column {
        LargeTitle(say.settings)
        SearchField(query, { query = it }, stringResource(R.string.settings_search), Modifier.padding(horizontal = Space.gutter, vertical = 6.dp))
        if (query.isNotBlank()) {
            // A result is the setting itself: tapping opens its page and puts the finger on the row.
            LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
                items(hits, key = { it.group + it.title }) { e ->
                    Column(Modifier.clickable { nav.settingsGroup(e.group, e.key) }) {
                        Column(Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 11.dp)) {
                            Text(e.title, style = MaterialTheme.typography.bodyLarge)
                            Text(
                                e.detail,
                                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 2, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                            )
                        }
                        Hairline(startIndent = Space.gutter)
                    }
                }
                if (hits.isEmpty()) item { EmptyNote(stringResource(R.string.settings_nothing_matches, query)) }
            }
            return@Column
        }
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            items(groups, key = { it.id }) { g ->
                Column(Modifier.clickable { nav.settingsGroup(g.id) }) {
                    Row(Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 13.dp), verticalAlignment = Alignment.CenterVertically) {
                        Icon(groupIcon(g.id), null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.primary)
                        Column(Modifier.weight(1f).padding(start = 14.dp)) {
                            Text(g.title, style = MaterialTheme.typography.bodyLarge)
                            Text(g.summary, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    Hairline(startIndent = Space.gutter + 36.dp)
                }
            }
            // Only the perf build has a recorder, and with it this page (docs/perf-build.md).
            if (dev.nori.music.app.PerfHooks.recorder != null) item(key = "perf") {
                Column(Modifier.clickable { nav.go("perf") }) {
                    Row(Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 13.dp), verticalAlignment = Alignment.CenterVertically) {
                        Icon(Icons.Outlined.Speed, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.primary)
                        Column(Modifier.weight(1f).padding(start = 14.dp)) {
                            Text(say.performance, style = MaterialTheme.typography.bodyLarge)
                            Text(say.performanceDetail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    Hairline(startIndent = Space.gutter + 36.dp)
                }
            }
        }
    }
}

/** One group's page: its rows on their plates, scrolled to whichever row the search sent us to. */
@Composable
fun SettingsGroupScreen(vm: SettingsViewModel, id: String, highlight: String = "", actions: dev.nori.music.app.vm.ActionsViewModel? = null) {
    val nav = LocalNav.current
    val p by vm.prefs.collectAsStateWithLifecycle()
    val facts by vm.settingsFacts.collectAsStateWithLifecycle()
    // The page's facts, fetched while it is open: the songs AutoMix measured, what is stored on the
    // phone, the server's music folders.
    LaunchedEffect(id, p.autoMix) { if (id == "playing" && p.autoMix) vm.refreshAnalysed() }
    LaunchedEffect(id) { if (id == "data") vm.refreshStorage() }
    LaunchedEffect(id, p.activeServerId) { if (id == "servers") vm.loadMusicFolders() }
    // Pages that change the sound (effects, speed, pitch, skip silence) are heard at once while open.
    if (id == "sound" || id == "playing") TuningWhileOpen(vm)
    val res = LocalContext.current.resources
    val config = LocalConfiguration.current
    // Built once for the whole page, again only when the settings, its facts or the language change.
    val page = remember(id, p, facts, config) { vm.settingsPage(id, p, facts, res) } ?: return
    val scroll = rememberScrollState()
    var target by remember { mutableIntStateOf(-1) }
    LaunchedEffect(target) { if (target >= 0) scroll.animateScrollTo((scroll.value + target - 400).coerceAtLeast(0)) }
    CompositionLocalProvider(LocalSpotlight provides SettingSpotlight(highlight.ifEmpty { null }) { y -> if (target < 0) target = y }) {
        Column(Modifier.verticalScroll(scroll)) {
            Row(Modifier.padding(start = 4.dp, end = Space.gutter), verticalAlignment = Alignment.CenterVertically) {
                IconButton(nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, say.back) }
                Text(page.title, Modifier.weight(1f), style = MaterialTheme.typography.headlineSmall)
            }
            when (id) {
                // The build's facts, then the rows the page builder made for About (the update check).
                "about" -> AboutContent({ title, content -> Section(title, content) }, { nav.settingsGroup("licences") }, vm::reportProblem) {
                    page.sections.forEach { s -> SettingsSectionRows(vm, s, actions) }
                }
                "licences" -> LicencesContent { title, content -> Section(title, content) }
                else -> page.sections.forEach { s -> SettingsSectionRows(vm, s, actions) }
            }
            Spacer(Modifier.height(Space.section + LocalChromeInset.current))
        }
    }
}

/** Text for a setting typed in (a service's key); hidden as it is typed when the row says it is secret. */
@Composable
private fun TextSettingDialog(row: SettingRow.Text, onDismiss: () -> Unit, onSave: (String) -> Unit) {
    var text by remember(row.name) { mutableStateOf(row.value) }
    AlertCard(
        title = { Text(row.title) },
        text = {
            Column {
                Text(row.detail, Modifier.padding(bottom = 8.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                FormField(
                    text, { text = it }, Modifier.fillMaxWidth(), singleLine = true,
                    visualTransformation = if (row.secret) androidx.compose.ui.text.input.PasswordVisualTransformation() else androidx.compose.ui.text.input.VisualTransformation.None,
                )
            }
        },
        confirmButton = { TextButton({ onSave(text) }) { Text(say.save) } },
        dismissButton = { TextButton(onDismiss) { Text(say.cancel) } },
    )
}

/** One section of a page as SettingsPages.kt laid it out; this only draws the rows and hands back what was picked. */
@Composable
private fun SettingsSectionRows(vm: SettingsViewModel, section: SettingsSection, actions: dev.nori.music.app.vm.ActionsViewModel?) {
    val nav = LocalNav.current
    val context = LocalContext.current
    val p by vm.prefs.collectAsStateWithLifecycle()
    var editing by remember { mutableStateOf<SavedServer?>(null) }
    var asking by remember { mutableStateOf<Pair<String, dev.nori.music.app.vm.ActionAsk>?>(null) }
    NoriDialog(editing, { editing = null }, DialogStyle.Page) { e -> LoginScreen(vm, e) { editing = null } }
    val act: (String) -> Unit = { action ->
        when (action) {
            "equalizer" -> nav.equalizer()
            "system-effects" -> runCatching {
                context.startActivity(
                    Intent(AudioEffect.ACTION_DISPLAY_AUDIO_EFFECT_CONTROL_PANEL).putExtra(AudioEffect.EXTRA_PACKAGE_NAME, context.packageName)
                        .putExtra(AudioEffect.EXTRA_CONTENT_TYPE, AudioEffect.CONTENT_TYPE_MUSIC),
                )
            }
            "downloads" -> nav.downloads()
            // Asks first whether the beat model reads them too, as Download does.
            "analyse-downloads" -> actions?.analyseDownloads()
            "add-server" -> editing = vm.newProfile()
            // A page of its own inside this one (the lyrics sources), as the row names it.
            else -> when {
                action.startsWith("page:") -> nav.settingsGroup(action.removePrefix("page:"))
                // Whether it asks first, and what it says (`settingsActionAsks`).
                else -> vm.actionAsks(action, context.resources)?.let { asking = action to it } ?: vm.act(action)
            }
        }
    }
    NoriDialog(asking, { asking = null }) { (action, ask) ->
        AlertCard(
            title = { Text(ask.title) },
            text = { Text(ask.text) },
            confirmButton = { TextButton({ vm.act(action); asking = null }) { Text(ask.confirm) } },
            dismissButton = { TextButton({ asking = null }) { Text(say.cancel) } },
        )
    }
    // A ranked row held and dragged (the lyrics services, one list whether on or off): which one, in
    // what order the rows were when it was picked up and where it would go, how far the finger has gone,
    // and each row's height (they differ: a service's line runs to two or three), which places are
    // measured in. The ranking is only changed when it is let go,
    // in one step (`lyricsPlace`): moving a row's composition while a finger is on it cancels the gesture,
    // and the rest of the drag then scrolled the page instead.
    var drag by remember { mutableStateOf<RankDrag?>(null) }
    var fingerY by remember { mutableFloatStateOf(0f) }
    val heights = remember { androidx.compose.runtime.mutableStateMapOf<String, Float>() }
    /** Where row [id] starts, in pixels from the first, with the rows in [list]'s order. */
    fun top(list: List<String>, id: String): Float { var y = 0f; for (o in list) { if (o == id) break; y += heights[o] ?: 0f }; return y }
    val order = section.rows.mapNotNull { (it as? SettingRow.Ranked)?.id }
    // The order as it is now, for a gesture that outlives the composition it started in.
    val orderNow by androidx.compose.runtime.rememberUpdatedState(order)
    val shown = drag?.shown ?: order
    // Once the core's order is the one shown, the drag is over.
    drag?.let { d -> if (!d.active && d.shown == order) androidx.compose.runtime.SideEffect { drag = null } }
    var typing by remember { mutableStateOf<SettingRow.Text?>(null) }
    NoriDialog(typing, { typing = null }) { row -> TextSettingDialog(row, { typing = null }) { v -> vm.set(row.name, v); typing = null } }
    Section(section.title) {
        // Each row under a key of its own, so a ranked row moved in the core's order is the same row, its
        // glide and lift carried with it: keyed inside the `when` instead, a moved row began again from
        // nothing and was drawn at its new place in one frame.
        section.rows.forEachIndexed { at, row -> androidx.compose.runtime.key((row as? SettingRow.Ranked)?.id ?: "#$at") {
            when (row) {
                is SettingRow.Toggle -> Toggle(row.title, row.detail, row.on, enabled = row.enabled, key = row.key) { on -> vm.set(row.name, on.toString()) }
                is SettingRow.Choice -> Choice(row) { v -> vm.set(row.name, v) }
                is SettingRow.Picks -> Picks(row) { name, on -> vm.set(name, on.toString()) }
                is SettingRow.ChipChoice -> ChipChoice(row) { v -> vm.set(row.name, v) }
                is SettingRow.Note -> Text(row.text, Modifier.padding(horizontal = Space.gutter, vertical = 8.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                is SettingRow.Link -> {
                    val dim = if (row.dimmed) DIMMED else 1f
                    Row(Modifier.fillMaxWidth().spotlight(row.key).clickable { act(row.action) }.padding(horizontal = 16.dp, vertical = 15.dp)) {
                        Text(row.title, Modifier.weight(1f).alpha(dim), style = MaterialTheme.typography.bodyLarge)
                        if (row.status.isNotEmpty()) Text(row.status, Modifier.alpha(dim), color = MaterialTheme.colorScheme.primary)
                    }
                    if (row.divider) Hairline(startIndent = 16.dp)
                }
                is SettingRow.Action -> ActionRowSetting(row) { act(row.action) }
                is SettingRow.Info -> {
                    Column(Modifier.fillMaxWidth().spotlight(row.key).padding(horizontal = 16.dp, vertical = 12.dp)) {
                        Text(row.title, style = MaterialTheme.typography.bodyLarge)
                        Text(row.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    Hairline(startIndent = 16.dp)
                }
                is SettingRow.Slider -> {
                    Text(row.label, Modifier.padding(start = 16.dp, end = 16.dp, top = 10.dp), style = MaterialTheme.typography.bodySmall)
                    // A slider with a level is edited in place on every step, like the equalizer's.
                    NoriSlider(row.value, row.min..row.max, { v -> row.level?.let { vm.setLevel(it, v) } ?: vm.set(row.name, v.toString()) }, Modifier.padding(horizontal = 16.dp), centred = row.centred)
                }
                is SettingRow.CompressionMeter -> CompressionMeter()
                is SettingRow.Palette -> Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                    row.colours.forEach { c ->
                        androidx.compose.foundation.layout.Box(
                            Modifier.size(if (row.chosen == c) 36.dp else 30.dp).background(androidx.compose.ui.graphics.Color(c), androidx.compose.foundation.shape.CircleShape).clickable { vm.set(row.name, c.toString()) },
                        )
                    }
                }
                is SettingRow.Server -> {
                    Row(Modifier.fillMaxWidth().clickable(enabled = !row.active) { p.servers.firstOrNull { it.id == row.id }?.let(vm::switchServer) }.padding(start = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text(row.label, color = if (row.active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface)
                            Text(row.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        TextButton({ editing = p.servers.firstOrNull { it.id == row.id } }) { Text(say.edit) }
                        TextButton({ vm.removeServer(row.id) }) { Text(say.remove) }
                    }
                    Hairline(startIndent = 16.dp)
                }
                is SettingRow.Button -> TextButton({ act(row.action) }, Modifier.padding(horizontal = 8.dp)) { Text(row.title) }
                // Each row is drawn at its place in the order shown, gliding there, while it is laid out in the
                // core's: the neighbours make way as the held row passes them, and when it is let go it settles
                // into its place as the core's order catches up, with nothing jumping at either end.
                is SettingRow.Ranked -> {
                    val haptics = androidx.compose.ui.platform.LocalHapticFeedback.current
                    // The held row is lifted over its neighbours, so it needs the plate's own colour behind its words.
                    val plate = LocalLook.current.color(dev.nori.music.look.CoverLook.FORM)
                    val scope = androidx.compose.runtime.rememberCoroutineScope()
                    val mine = drag?.id == row.id
                    val following = mine && drag?.active == true
                    // Where it is drawn, from the top of the list: it glides to its place in the order shown when a
                    // drag moves it, and is simply there when anything else does (rows being measured, a page
                    // opening). Not restarted when the drag ends, so a row settling finishes its glide.
                    val shownY = top(shown, row.id)
                    val slot = remember { androidx.compose.animation.core.Animatable(shownY) }
                    // Whether a drag moved it is read here, in the composition that moved it: by the time the effect
                    // starts the core may already have taken the drop and ended the drag.
                    val glide = drag != null
                    LaunchedEffect(shownY, following) {
                        if (following) return@LaunchedEffect
                        if (!glide) slot.snapTo(shownY)
                        else slot.animateTo(shownY, androidx.compose.animation.core.tween(if (AppMotion.reduce) 0 else 200))
                    }
                    // Picked up and put down over a moment, not in a frame: its lift, shadow and plate.
                    val lift = androidx.compose.animation.core.animateFloatAsState(
                        if (following) 1f else 0f, androidx.compose.animation.core.tween(if (AppMotion.reduce) 0 else 160), label = "lift",
                    )
                    val above by remember { androidx.compose.runtime.derivedStateOf { lift.value > 0f } }
                    Column(
                        Modifier.fillMaxWidth()
                            .zIndex(if (mine || above) 1f else 0f)
                            .graphicsLayer {
                                val at = if (following) top(drag?.from ?: order, row.id) + fingerY else slot.value
                                translationY = at - top(order, row.id)
                                val l = lift.value
                                shadowElevation = 14f * l; scaleX = 1f + 0.02f * l; scaleY = 1f + 0.02f * l
                            }
                            .drawBehind { lift.value.takeIf { it > 0f }?.let { drawRect(plate.copy(alpha = plate.alpha * it)) } }
                            .onGloballyPositioned { val h = it.size.height.toFloat(); if (heights[row.id] != h) heights[row.id] = h },
                    ) {
                        // Held anywhere, like the home page's rows: a long press picks it up (a plain drag
                        // scrolls the page, and never moves a row), with a tick and the row lifting. The
                        // switch still turns the service on or off where it stands with a tap.
                        Row(
                            Modifier.fillMaxWidth().spotlight(row.key).pointerInput(row.id) {
                                detectDragGesturesAfterLongPress(
                                    onDragStart = {
                                        drag = RankDrag(row.id, orderNow, orderNow.indexOf(row.id), active = true); fingerY = 0f
                                        haptics.performHapticFeedback(androidx.compose.ui.hapticfeedback.HapticFeedbackType.LongPress)
                                    },
                                    onDragEnd = {
                                        val d = drag ?: return@detectDragGesturesAfterLongPress
                                        // From where the finger left it; the core is told once, where it was dropped.
                                        scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { slot.snapTo(top(d.from, row.id) + fingerY) }
                                        val placed = d.to == d.from.indexOf(row.id) || vm.placeRanked(row.id, d.to)
                                        drag = if (placed) d.copy(active = false) else d.copy(to = d.from.indexOf(row.id), active = false)
                                    },
                                    onDragCancel = {
                                        val d = drag ?: return@detectDragGesturesAfterLongPress
                                        scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { slot.snapTo(top(d.from, row.id) + fingerY) }
                                        drag = d.copy(to = d.from.indexOf(row.id), active = false)
                                    },
                                ) { change, moved ->
                                    change.consume()
                                    fingerY += moved.y
                                    val d = drag ?: return@detectDragGesturesAfterLongPress
                                    // Its place: after every other row whose middle its own middle has passed.
                                    val middle = top(d.from, row.id) + fingerY + (heights[row.id] ?: 0f) / 2f
                                    var y = 0f
                                    var to = 0
                                    for (o in d.from) {
                                        if (o == row.id) continue
                                        val h = heights[o] ?: 0f
                                        if (middle > y + h / 2f) to++
                                        y += h
                                    }
                                    if (to != d.to) {
                                        drag = d.copy(to = to)
                                        haptics.performHapticFeedback(androidx.compose.ui.hapticfeedback.HapticFeedbackType.TextHandleMove)
                                    }
                                }
                            }.padding(start = 16.dp, top = 12.dp, bottom = 12.dp, end = 16.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f).padding(end = 14.dp)) {
                                Text(row.title, style = MaterialTheme.typography.bodyLarge, color = if (row.on) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant)
                                Text(row.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                            NoriSwitch(row.on, { on -> vm.set(row.name, on.toString()) })
                        }
                        Hairline(startIndent = 16.dp)
                    }
                }
                is SettingRow.Text -> {
                    Column(Modifier.fillMaxWidth().spotlight(row.key).clickable { typing = row }.padding(horizontal = 16.dp, vertical = 12.dp)) {
                        Text(row.title, style = MaterialTheme.typography.bodyLarge)
                        Text(row.detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    Hairline(startIndent = 16.dp)
                }
            }
        } }
    }
}

/** A ranked row held: [id], the order when it was picked up ([from]), the place it would go ([to]), and whether the finger is still on it. */
private data class RankDrag(val id: String, val from: List<String>, val to: Int, val active: Boolean) {
    /** The order drawn: [from] with the held row moved to [to]. */
    val shown: List<String> = from.toMutableList().apply { remove(id); add(to, id) }
}
