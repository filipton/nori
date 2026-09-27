package dev.nori.music.app.ui

import android.os.Build
import androidx.compose.foundation.clickable
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import dev.nori.music.app.BuildConfig
import dev.nori.music.ffi.settings.Credit

/** A row that says something: a title, a line under it, and optionally something at its end or a tap. */
@Composable
internal fun InfoRow(title: String, detail: String, end: String? = null, onClick: (() -> Unit)? = null) {
    Row(
        Modifier.fillMaxWidth().then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f).padding(end = if (end != null) 12.dp else 0.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            if (detail.isNotEmpty()) Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (end != null) Text(end, style = MaterialTheme.typography.labelMedium, color = if (onClick != null) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
    }
    Hairline(startIndent = 16.dp)
}

/**
 * What this is and what it is built out of. Most of a bug report is answered here - which build, which
 * commit, which phone, which audio engine at which version - and a tap on the top row copies all of it.
 * The lines and the report are worded by [Say.about] from what only Android knows.
 */
@Composable
internal fun AboutContent(section: @Composable (String, @Composable ColumnScope.() -> Unit) -> Unit, openLicences: () -> Unit, updates: @Composable () -> Unit = {}) {
    val clipboard = LocalClipboardManager.current
    val facts = remember {
        say.about(
            BuildConfig.VERSION_NAME, BuildConfig.CORE_VERSIONS, BuildConfig.DEBUG, BuildConfig.GIT_SHA,
            Build.SUPPORTED_ABIS.firstOrNull(), Build.VERSION.RELEASE, Build.VERSION.SDK_INT,
        )
    }
    section("nori") {
        InfoRow(facts.title, facts.build, end = say.copyIt) { clipboard.setText(AnnotatedString(facts.report)) }
    }
    updates()
    section(say.underTheHood) {
        InfoRow(say.playback, facts.playback)
        InfoRow(say.libraryAndSearch, facts.library)
        InfoRow(say.automix, facts.automix)
        InfoRow(say.interfaceTitle, facts.ui)
    }
    section(say.openSource) {
        InfoRow(say.freeSoftware, say.licenceLine, end = "MIT")
        InfoRow(say.licences, say.licencesDetail, onClick = openLicences)
    }
}

/**
 * Everything the app is built from that is not ours, grouped: the core's crates, Android's libraries,
 * the typeface and the third parties' data. What each is, whose and under which terms is the core's
 * (`credits::core_credits`, `android_credits`, `data_credits`), so every app on it lists them
 * alike; each names the bundled text its terms are (`assets/licences/<file>.txt`; none for a service).
 */
private val CREDITS by lazy {
    listOf(
        say.rustCore to dev.nori.music.ffi.settings.coreCredits(),
        "Android" to dev.nori.music.ffi.settings.androidCredits(),
        say.fontsAndData to dev.nori.music.ffi.settings.dataCredits(),
    )
}

/**
 * The licences page: every credit, grouped, with its licence at the end of the row. A tap shows who it
 * belongs to and the licence's full text, as the licences themselves ask to be shipped with the app.
 */
@Composable
internal fun LicencesContent(section: @Composable (String, @Composable ColumnScope.() -> Unit) -> Unit) {
    var open by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf<Credit?>(null) }
    CREDITS.forEach { (heading, credits) ->
        section(heading) {
            credits.forEach { c -> InfoRow(c.name, c.what, end = c.licence) { open = c } }
        }
    }
    NoriDialog(open, { open = null }) { c -> LicenceText(c) { open = null } }
}

@Composable
private fun LicenceText(c: Credit, dismiss: () -> Unit) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val text = androidx.compose.runtime.remember(c) {
        c.file?.let { f -> runCatching { context.assets.open("licences/$f.txt").bufferedReader().use { it.readText() }.trim() }.getOrNull() }
    }
    AlertCard(
        confirmButton = { androidx.compose.material3.TextButton(dismiss) { Text(say.close) } },
        title = { Text(c.name) },
        text = {
            Column(Modifier.verticalScroll(androidx.compose.foundation.rememberScrollState())) {
                Text(c.copyright, style = MaterialTheme.typography.bodyMedium)
                Text(c.licence, Modifier.padding(top = 4.dp, bottom = 12.dp), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
                Text(
                    text ?: if (c.file == null) say.noLicenceText else say.licenceUnreadable,
                    style = MaterialTheme.typography.bodySmall.copy(fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
    )
}
