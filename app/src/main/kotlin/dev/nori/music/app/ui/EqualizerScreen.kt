package dev.nori.music.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.material3.Surface
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.height
import kotlin.math.roundToInt
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.ffi.settings.EqLevel
import dev.nori.music.ffi.settings.EqMode
import dev.nori.music.ffi.settings.SoundBand
import dev.nori.music.ffi.settings.BandChannel
import dev.nori.music.ffi.model.EqKind
import dev.nori.music.settings.EqBands
import dev.nori.music.settings.EQ
import dev.nori.music.settings.effectivePreampDb
import dev.nori.music.settings.usesGain
import dev.nori.music.settings.slope

/** The limiter's gain reduction, sampled while this screen is resumed and dropped the moment it is not. */
@Composable
private fun limiterMeter(): Float {
    var value by remember { mutableStateOf(0f) }
    var resumed by remember { mutableStateOf(false) }
    androidx.lifecycle.compose.LifecycleResumeEffect(Unit) { resumed = true; onPauseOrDispose { resumed = false } }
    androidx.compose.runtime.LaunchedEffect(resumed) {
        while (resumed) {
            value = dev.nori.music.playback.Equalizer.meterDb
            kotlinx.coroutines.delay(stage.meterMs)
        }
    }
    return value
}

/** A band's label, its frequency and a mark for its channel or kind (nori-core's `settings::band_label`). */
private fun bandLabel(b: SoundBand): String = say.band(b.freq, dev.nori.music.ffi.settings.BandMark.entries[EqBands.mark(b.kind.ordinal, b.channel.ordinal)])

/** How far each control goes: the core's, the same ranges it holds every edit in. */
private val ranges get() = EQ.eqRanges

@Composable
fun EqualizerScreen(vm: SettingsViewModel) {
    val p by vm.prefs.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    var importing by remember { mutableStateOf(false) }
    var editing by remember { mutableStateOf(-1) }

    NoriDialog(importing, { importing = false }) { ImportDialog(vm) { importing = false } }
    NoriDialog(p.eqBands.getOrNull(editing), { editing = -1 }) { band -> BandDialog(band, { b -> vm.setBand(editing, b) }, { vm.removeBand(editing); editing = -1 }) { editing = -1 } }

    Column(Modifier.verticalScroll(rememberScrollState()).padding(bottom = LocalChromeInset.current)) {
        Row(Modifier.padding(start = 4.dp, end = Space.gutter), verticalAlignment = Alignment.CenterVertically) {
            IconButton(nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, say.back) }
            Text(say.equalizer, Modifier.weight(1f), style = MaterialTheme.typography.headlineSmall)
            NoriSwitch(p.eqEnabled, { on -> vm.update { it.copy(eqEnabled = on) } })
        }
        val graphic = p.eqMode == EqMode.GRAPHIC
        // Two equalizers, each with its own settings: the one picked here is the one that plays.
        Row(Modifier.padding(horizontal = Space.gutter, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Chip(say.eqGraphic, graphic) { if (!graphic) vm.setEqMode(EqMode.GRAPHIC) }
            Chip(say.eqParametric, !graphic) { if (graphic) vm.setEqMode(EqMode.PARAMETRIC) }
        }
        Text(
            if (graphic) say.eqGraphicHint else say.eqHint,
            Modifier.padding(horizontal = Space.gutter, vertical = 2.dp),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        // Two settings switch the whole sample chain off. Without this the screen looks broken: bands
        // move, the limiter says it is on, and nothing whatsoever happens to the sound.
        val dac by vm.dac.collectAsStateWithLifecycle()
        val bypass = remember(dac.bitPerfect, p.soundBypass) { dev.nori.music.ffi.settings.eqBypassReason(dac.bitPerfect, p.soundBypass)?.let(say::eqBypass) }
        if (bypass != null) Surface(
            shape = CardShape, color = MaterialTheme.colorScheme.errorContainer,
            modifier = Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 8.dp),
        ) {
            Text(bypass, Modifier.padding(12.dp), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onErrorContainer)
        }

        if (graphic) GraphicBands(vm, p.eqGraphic, p.eqGraphicTarget, p.eqEnabled)
        else p.eqBands.forEachIndexed { i, b ->
            Row(Modifier.padding(horizontal = Space.gutter), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                // Once per band shape, not on every frame of a gain drag.
                val label = remember(b.freq, b.channel, b.kind) { bandLabel(b) }
                Text(label, Modifier.width(56.dp).clickable { editing = i }, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
                if (b.kind.usesGain) {
                    NoriSlider(b.gainDb, ranges.gain.min..ranges.gain.max, { v -> vm.setBand(i, b.copy(gainDb = v)) }, Modifier.weight(1f), enabled = p.eqEnabled, centred = true)
                    Text(
                        remember(b.gainDb) { dev.nori.music.text.Fmt.signedDb(b.gainDb) }, Modifier.width(42.dp),
                        style = MaterialTheme.typography.labelMedium, textAlign = androidx.compose.ui.text.style.TextAlign.End,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else {
                    Text(say.bandKind(b.kind), Modifier.weight(1f).clickable { editing = i }, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        LazyRow(Modifier.bleedsToEdges(), contentPadding = edgePadding(horizontal = Space.gutter, vertical = 10.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (!graphic) item { Chip(say.addBand, false, onClick = vm::addBand) }
            // On the graphic equalizer a headphone correction is fitted to its sliders.
            item { Chip(say.pastePreset, false) { importing = true } }
            item { Chip(say.headphonePresets, false, onClick = nav::autoEq) }
            item { Chip(say.reset, false, onClick = vm::resetBands) }
        }
        SectionTitle(say.presets)
        LazyRow(Modifier.bleedsToEdges(), contentPadding = edgePadding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            items(vm.presets) { preset -> Chip(say.preset(preset.kind), false) { vm.applyPreset(preset) } }
        }

        Row(Modifier.padding(horizontal = Space.gutter), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(remember(p.effectivePreampDb, p.eqPreampDb == null) { say.preamp(p.effectivePreampDb, p.eqPreampDb == null) })
                Text(say.autoPreampHint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            NoriSwitch(p.eqPreampDb == null, vm::setAutoPreamp)
        }
        p.eqPreampDb?.let { v -> NoriSlider(v, ranges.preamp.min..ranges.preamp.max, { x -> vm.setLevel(EqLevel.PREAMP, x) }, Modifier.padding(horizontal = Space.gutter), enabled = p.eqEnabled) }

        SectionTitle(say.output)
        Row(Modifier.padding(horizontal = Space.gutter), verticalAlignment = Alignment.CenterVertically) {
            Text(say.balance, Modifier.width(80.dp))
            NoriSlider(p.balance, ranges.balance.min..ranges.balance.max, { v -> vm.setLevel(EqLevel.BALANCE, v) }, Modifier.weight(1f), centred = true)
            Text(remember(p.balance) { say.balance(p.balance) }, Modifier.width(72.dp), style = MaterialTheme.typography.labelMedium)
        }
        Toggle(say.mono, say.monoDetail, p.mono) { on -> vm.update { it.copy(mono = on) } }
        Toggle(say.limiter, say.limiterDetail, p.limiter) { on -> vm.update { it.copy(limiter = on) } }
        if (p.limiter) {
            Row(Modifier.padding(horizontal = Space.gutter), verticalAlignment = Alignment.CenterVertically) {
                Text(remember(p.limiterThresholdDb) { say.ceiling(p.limiterThresholdDb) }, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
                LimiterReduction()
            }
            NoriSlider(p.limiterThresholdDb, ranges.limiter.min..ranges.limiter.max, { v -> vm.setLevel(EqLevel.LIMITER, v) }, Modifier.padding(horizontal = Space.gutter))
        }

        DevicesSection(vm)

        SectionTitle(say.profiles)
        val profiles by vm.profiles.collectAsStateWithLifecycle()
        var naming by remember { mutableStateOf(false) }
        var newName by remember { mutableStateOf("") }
        NoriDialog(naming, { naming = false }) {
            AlertCard(
                title = { Text(say.saveTheseSettings) },
                text = { OutlinedTextField(newName, { newName = it }, singleLine = true, label = { Text(say.name) }) },
                confirmButton = { TextButton({ vm.saveProfile(newName); naming = false }, enabled = newName.isNotBlank()) { Text(say.save) } },
                dismissButton = { TextButton({ naming = false }) { Text(say.cancel) } },
            )
        }
        val rows by vm.deviceRows.collectAsStateWithLifecycle()
        AnimatedRows(profiles, { it.name }) { profile ->
            val used = remember(rows, profile) { say.profileUse(rows.filter { it.output in profile.outputs }.map { it.name }) }
            NavRow(
                profile.name, { vm.applyProfile(profile) },
                subtitle = used,
                action = { IconButton({ vm.deleteProfile(profile.name) }) { Icon(Icons.Outlined.Delete, remember(profile.name) { say.deleteNamed(profile.name) }, tint = MaterialTheme.colorScheme.onSurfaceVariant) } },
            )
        }
        ActionRow(say.saveAsProfile, Icons.Filled.Add, { newName = ""; naming = true }, divider = false)

        SectionTitle(say.crossfeed)
        // bs2b's presets, or the listener's own (none of the chips lit): which is the core's.
        val preset = remember(p.crossfeedDb, p.crossfeedHz) { dev.nori.music.ffi.settings.crossfeedPresetOf(p) }
        Row(Modifier.padding(horizontal = Space.gutter, vertical = 4.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            listOf("OFF" to say.crossfeedOff, "DEFAULT" to say.crossfeedDefault, "CHU_MOY" to say.crossfeedChuMoy, "JAN_MEIER" to say.crossfeedJanMeier).forEach { (v, label) ->
                Chip(label, v == preset) { if (v != preset) vm.set("crossfeedPreset", v) }
            }
        }
        Text(remember(p.crossfeedDb, preset) { say.crossfeed(p.crossfeedDb, preset.isEmpty()) }, Modifier.padding(horizontal = Space.gutter), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        NoriSlider(p.crossfeedDb, ranges.crossfeed.min..ranges.crossfeed.max, { v -> vm.setLevel(EqLevel.CROSSFEED, v) }, Modifier.padding(horizontal = Space.gutter))
        if (p.crossfeedDb > 0f) {
            Text(remember(p.crossfeedHz) { say.crossfeedCut(p.crossfeedHz) }, Modifier.padding(horizontal = Space.gutter), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            NoriSlider(p.crossfeedHz, ranges.crossfeedCut.min..ranges.crossfeedCut.max, { v -> vm.setLevel(EqLevel.CROSSFEED_CUT, v) }, Modifier.padding(horizontal = Space.gutter))
        }
    }
}

/** The graphic equalizer: the layout, the curve it plays, and a slider per band. */
@Composable
private fun GraphicBands(vm: SettingsViewModel, sliders: List<Float>, target: List<Float>, enabled: Boolean) {
    val count = sliders.size
    Row(Modifier.padding(horizontal = Space.gutter, vertical = 4.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        listOf(5, 10, 15, 31).forEach { n -> Chip(say.eqBandCount(n), n == count) { if (n != count) vm.setEqLayout(n) } }
    }
    ResponseCurve(sliders, target, enabled)
    // After a headphone correction: how closely the sliders follow it, asked of the core once per change.
    if (target.isNotEmpty()) {
        val follow = remember(sliders, target) { dev.nori.music.ffi.settings.graphicFollow(sliders, target) }
        follow?.let {
            Text(
                remember(it.maxDb, count) { say.eqFollows(it.maxDb, count) },
                Modifier.padding(horizontal = Space.gutter, vertical = 2.dp),
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    // The bands' labels and exact centres are the core's, asked once per layout.
    val bands = remember(count) { dev.nori.music.ffi.settings.graphicBands(count.toUInt()) }
    val gain = ranges.gain
    sliders.forEachIndexed { i, v ->
        Row(Modifier.padding(horizontal = Space.gutter), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            val label = remember(count, i) { bands.getOrNull(i)?.let { say.isoBand(it.labelHz) }.orEmpty() }
            Text(label, Modifier.width(56.dp), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
            NoriSlider(v, gain.min..gain.max, { x -> vm.setGraphic(i, (x * 2f).roundToInt() / 2f) }, Modifier.weight(1f), enabled = enabled, centred = true)
            Text(
                remember(v) { dev.nori.music.text.Fmt.signedDb(v) }, Modifier.width(42.dp),
                style = MaterialTheme.typography.labelMedium, textAlign = androidx.compose.ui.text.style.TextAlign.End,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** Where the curve is read, 20 Hz to 20 kHz on a log scale. */
private val CURVE_FREQS: List<Float> = List(97) { 20f * Math.pow(1000.0, it / 96.0).toFloat() }

/**
 * What the graphic equalizer plays for these sliders, drawn: the core's response of the filters it
 * designed (`graphic_response`), asked once per change and drawn as a line over ±15 dB.
 */
@Composable
private fun ResponseCurve(sliders: List<Float>, target: List<Float>, enabled: Boolean) {
    val response = remember(sliders) { dev.nori.music.ffi.settings.graphicResponse(sliders, CURVE_FREQS) }
    // A headphone correction's curve, drawn faintly at the level the sliders play it: both run 20 Hz to
    // 20 kHz on a log scale, so their means line them up.
    val aim = remember(target, response) {
        if (target.isEmpty()) emptyList() else { val shift = response.average().toFloat() - target.average().toFloat(); target.map { it + shift } }
    }
    val line = if (enabled) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant
    val faint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.45f)
    val grid = MaterialTheme.colorScheme.outlineVariant
    androidx.compose.foundation.Canvas(Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 8.dp).height(96.dp)) {
        val mid = size.height / 2f
        val perDb = size.height / 30f
        drawLine(grid, androidx.compose.ui.geometry.Offset(0f, mid), androidx.compose.ui.geometry.Offset(size.width, mid), strokeWidth = 1f)
        fun trace(values: List<Float>): androidx.compose.ui.graphics.Path {
            val path = androidx.compose.ui.graphics.Path()
            values.forEachIndexed { i, db ->
                val x = size.width * i / (values.size - 1).coerceAtLeast(1)
                val y = (mid - db.coerceIn(-15f, 15f) * perDb)
                if (i == 0) path.moveTo(x, y) else path.lineTo(x, y)
            }
            return path
        }
        if (aim.isNotEmpty()) drawPath(trace(aim), faint, style = androidx.compose.ui.graphics.drawscope.Stroke(width = 1.5.dp.toPx(), cap = androidx.compose.ui.graphics.StrokeCap.Round))
        drawPath(trace(response), line, style = androidx.compose.ui.graphics.drawscope.Stroke(width = 2.dp.toPx(), cap = androidx.compose.ui.graphics.StrokeCap.Round))
    }
}

/**
 * Proof that the limiter is working: what it is pulling back, right now. Polled only while this screen
 * is on top, so it costs nothing the rest of the time. Its own scope, so each reading redraws this one
 * line rather than recomposing the whole screen every 120 ms.
 */
@Composable
private fun LimiterReduction() {
    val reduction = limiterMeter()
    // The words change only with the tenth of a dB they show; the meter moves far more finely.
    Text(
        remember(kotlin.math.round(reduction * 10f)) { say.reduction(reduction) },
        style = MaterialTheme.typography.labelMedium,
        color = if (reduction > 0.05f) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun ImportDialog(vm: SettingsViewModel, onDone: () -> Unit) {
    var text by remember { mutableStateOf("") }
    var error by remember { mutableStateOf(false) }
    AlertCard(
        title = { Text(say.importPreset) },
        text = {
            Column {
                Text(say.importPresetHint, style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(text, { text = it; error = false }, Modifier.fillMaxWidth().padding(top = 8.dp), minLines = 5, maxLines = 10, isError = error, placeholder = { Text(say.importPresetExample) })
                if (error) Text(say.noFiltersFound, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
            }
        },
        confirmButton = { TextButton({ if (vm.importPreset(text) > 0) onDone() else error = true }) { Text(say.import) } },
        dismissButton = { TextButton(onDone) { Text(say.cancel) } },
    )
}

@Composable
private fun BandDialog(band: SoundBand, onChange: (SoundBand) -> Unit, onRemove: () -> Unit, onDone: () -> Unit) {
    // Asked again only when what they say changes, not on every recomposition a drag makes.
    val title = remember(band.freq) { say.hzTitle(band.freq) }
    val shape = remember(band.kind.slope, band.q) { say.shape(band.kind.slope, band.q) }
    AlertCard(
        title = { Text(title) },
        text = {
            Column {
                LazyRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    items(EqKind.entries) { k ->
                        TextButton({ onChange(band.copy(kind = k)) }) { Text(say.bandKind(k), color = if (band.kind == k) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant) }
                    }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    BandChannel.entries.forEach { c ->
                        TextButton({ onChange(band.copy(channel = c)) }) { Text(say.bandChannel(c), color = if (band.channel == c) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant) }
                    }
                }
                Text(say.frequency, style = MaterialTheme.typography.labelMedium)
                // Logarithmic: the slider position is the exponent, 20 Hz to 20 kHz.
                NoriSlider(EqBands.freqToSlider(band.freq), 0f..1f, { x -> onChange(band.copy(freq = EqBands.sliderToFreq(x))) })
                Text(shape, style = MaterialTheme.typography.labelMedium)
                NoriSlider(band.q, ranges.q.min..ranges.q.max, { q -> onChange(band.copy(q = q)) })
            }
        },
        confirmButton = { TextButton(onDone) { Text(say.done) } },
        dismissButton = { TextButton(onRemove) { Icon(Icons.Filled.Close, null); Text(say.removeBand) } },
    )
}
