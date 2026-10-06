package dev.nori.music.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.nori.music.app.BuildConfig
import dev.nori.music.app.R
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.app.vm.updateButton
import dev.nori.music.app.vm.updateWords
import dev.nori.music.update.Updates

/**
 * A newer release, said quietly at the top of the page: its version, the start of its notes (a tap shows
 * all of them) and "Later" / "Update". Once Update is pressed it stays to show the download, Android's
 * permission page or a failure. A version put off with Later is not shown again by itself; About's row
 * still has it. Nothing here moves unless the state does.
 */
@Composable
fun UpdateBanner(vm: SettingsViewModel, modifier: Modifier = Modifier) {
    val state by vm.update.collectAsStateWithLifecycle()
    val s = state
    val shown = when (s) {
        is Updates.State.Available -> !s.skipped
        is Updates.State.Downloading, is Updates.State.Installing, is Updates.State.NeedsPermission, is Updates.State.Failed -> true
        else -> false
    }
    // It comes and goes without animating: nothing moves that the user did not touch.
    if (shown) androidx.compose.foundation.layout.Box(modifier.statusBarsPadding().padding(top = 8.dp, start = 12.dp, end = 12.dp).widthIn(max = 560.dp)) { BannerCard(vm, s) }
}

@Composable
private fun BannerCard(vm: SettingsViewModel, s: Updates.State) {
    val res = LocalContext.current.resources
    val installs = vm.installsUpdates
    val update = when (s) {
        is Updates.State.Available -> s.update
        is Updates.State.Downloading -> s.update
        is Updates.State.Installing -> s.update
        is Updates.State.NeedsPermission -> s.update
        is Updates.State.Failed -> s.update
        else -> return
    }
    var open by remember(update.version) { mutableStateOf(false) }
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceContainerHigh,
        contentColor = MaterialTheme.colorScheme.onSurface,
        shadowElevation = 6.dp,
    ) {
        Column(Modifier.padding(start = 16.dp, end = 8.dp, top = 14.dp, bottom = 4.dp)) {
            Text(res.getString(R.string.update_banner_title, update.version), Modifier.padding(end = 8.dp), style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
            val line = when (s) {
                // The notes themselves while it is only on offer; what is happening once it is not.
                is Updates.State.Available -> null
                else -> updateWords(res, s, BuildConfig.VERSION_NAME, installs)
            }
            if (line != null) {
                Text(
                    line, Modifier.padding(top = 4.dp, end = 8.dp), style = MaterialTheme.typography.bodySmall,
                    color = if (s is Updates.State.Failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (update.notes.isNotEmpty()) {
                val notes = Modifier.padding(top = 4.dp, end = 8.dp).clickable { open = !open }
                if (open) {
                    Text(
                        update.notes, notes.heightIn(max = 280.dp).verticalScroll(rememberScrollState()),
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else {
                    Text(update.notes, notes, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 3, overflow = TextOverflow.Ellipsis)
                }
            }
            if (s is Updates.State.Downloading) {
                LinearProgressIndicator(
                    progress = { if (s.total > 0) (s.done.toFloat() / s.total).coerceIn(0f, 1f) else 0f },
                    Modifier.fillMaxWidth().padding(top = 10.dp, end = 8.dp),
                )
            }
            Row(Modifier.fillMaxWidth().padding(top = 4.dp), horizontalArrangement = Arrangement.End) {
                if (s is Updates.State.Available && !open && update.notes.isNotEmpty()) {
                    TextButton({ open = true }) { Text(res.getString(R.string.update_whats_new)) }
                    androidx.compose.foundation.layout.Spacer(Modifier.weight(1f))
                }
                if (s !is Updates.State.Downloading && s !is Updates.State.Installing) {
                    TextButton(vm::updateLater) { Text(res.getString(R.string.update_later)) }
                }
                updateButton(res, s, installs)?.let { word ->
                    val act = if (s is Updates.State.Downloading) vm::cancelUpdate else vm::updateNow
                    TextButton(act) { Text(word, fontWeight = FontWeight.SemiBold) }
                }
            }
        }
    }
}

/** What the update just installed brought: its release notes, once, the first time the app is opened after it. */
@Composable
fun ChangelogDialog(vm: SettingsViewModel) {
    val notes by vm.changelog.collectAsStateWithLifecycle()
    val res = LocalContext.current.resources
    NoriDialog(notes, vm::changelogSeen) { text ->
        AlertCard(
            title = { Text(res.getString(R.string.update_changelog_title, BuildConfig.VERSION_NAME)) },
            text = { Text(text, Modifier.verticalScroll(rememberScrollState())) },
            confirmButton = { TextButton(vm::changelogSeen) { Text(res.getString(R.string.update_changelog_done)) } },
        )
    }
}
