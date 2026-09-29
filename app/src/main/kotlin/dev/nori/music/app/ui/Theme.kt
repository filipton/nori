package dev.nori.music.app.ui

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import dev.nori.music.ffi.settings.StoredPrefs
import dev.nori.music.ffi.settings.ThemeMode

/**
 * Material You: the wallpaper's colours on Android 12+, otherwise a scheme grown from one accent colour.
 * AMOLED replaces every dark surface with true black, so those pixels are simply off.
 */
@Composable
fun NoriTheme(prefs: StoredPrefs, content: @Composable () -> Unit) {
    val context = LocalContext.current
    val system = isSystemInDarkTheme()
    // Light, dark or the phone's: the core's rule (nori_look::theme::is_dark), the same for every screen.
    val dark = remember(prefs.theme, system) { dev.nori.music.ffi.settings.themeIsDark(prefs.theme, system) }
    val scheme = remember(dark, prefs.dynamicColor, prefs.accent, prefs.amoled) { schemeOf(context, prefs, dark) }
    val systemDensity = androidx.compose.ui.platform.LocalDensity.current
    // The shorter side, not the width: the same in both orientations, so text keeps its size on a turn.
    val widthDp = androidx.compose.ui.platform.LocalConfiguration.current.smallestScreenWidthDp
    val scale = remember(prefs.uiScale, widthDp) { uiScale(prefs.uiScale, widthDp) }
    // The whole app's density, scaled once here: dp and sp both follow it, so every size keeps its
    // proportion to the screen. The system's font scale is left as it is - that one is the reader's.
    val density = remember(systemDensity, scale) {
        if (scale == 1f) systemDensity else androidx.compose.ui.unit.Density(systemDensity.density * scale, systemDensity.fontScale)
    }
    // Everything dressed in the theme's own colours - the plates behind the buttons, the chrome, the
    // status bar - worked out once per scheme in Rust (nori_look::dress) and only looked up after.
    val look = remember(scheme) { FixedLook(plainLook(scheme)) }
    androidx.compose.runtime.CompositionLocalProvider(androidx.compose.ui.platform.LocalDensity provides density, LocalLook provides look) {
        MaterialTheme(colorScheme = scheme, typography = NoriTypography, content = content)
    }
}

/**
 * The scheme the app wears: the wallpaper's colours on Android 12+ when asked for, otherwise grown from the
 * accent, with AMOLED black over a dark one. Outside composition too, for the home-screen widgets.
 */
fun schemeOf(context: android.content.Context, prefs: StoredPrefs, dark: Boolean): ColorScheme {
    val base = when {
        prefs.dynamicColor && Build.VERSION.SDK_INT >= 31 && dark -> dynamicDarkColorScheme(context)
        prefs.dynamicColor && Build.VERSION.SDK_INT >= 31 -> dynamicLightColorScheme(context)
        else -> seeded(Color(prefs.accent), dark)
    }
    return if (dark && prefs.amoled) base.black() else base
}

/** Everything dressed in [scheme]'s own colours (nori_look::dress), as the core lays a look out. */
fun plainLook(scheme: ColorScheme): IntArray = dev.nori.music.look.CoverLook.plain(
    intArrayOf(
        scheme.background.toArgb(), scheme.onSurface.toArgb(), scheme.onSurfaceVariant.toArgb(), scheme.primary.toArgb(),
        scheme.onPrimary.toArgb(), scheme.surfaceVariant.toArgb(), scheme.surfaceContainer.toArgb(),
        scheme.surfaceContainerHigh.toArgb(), scheme.secondaryContainer.toArgb(), scheme.outlineVariant.toArgb(),
    ),
)

/** Every size in the app was measured as a share of the width of a phone this many dp wide. */
private const val REFERENCE_WIDTH_DP = 411f

/**
 * How big the interface is drawn. A setting above 0 is a fixed factor. 0 is automatic: laid out as if
 * the screen were at least [REFERENCE_WIDTH_DP] wide, so a phone set to a large display size (one
 * measured at 358 dp draws everything a seventh larger) keeps the proportions; it only ever shrinks,
 * never below three quarters, and never enlarges past what the system asked for. A phone's rule, about
 * Android's display size: a desktop window has no such setting to undo.
 */
fun uiScale(setting: Float, screenWidthDp: Int): Float = when {
    setting > 0f -> setting
    screenWidthDp <= 0 -> 1f
    else -> (screenWidthDp / REFERENCE_WIDTH_DP).coerceIn(0.75f, 1f)
}

/** A light or dark scheme from one colour: tones of the same hue, worked out in Rust (`nori_look::theme::seeded`). */
private fun seeded(seed: Color, dark: Boolean): ColorScheme {
    val tones = dev.nori.music.look.CoverLook.tones(seed.toArgb(), dark)
    val t = List(tones.size) { Color(tones[it]) }
    return if (dark) darkColorScheme(
        primary = t[0], onPrimary = t[1], primaryContainer = t[2], onPrimaryContainer = t[3],
        secondary = t[4], secondaryContainer = t[5], onSecondaryContainer = t[6],
        surface = t[7], background = t[8], surfaceVariant = t[9], onSurfaceVariant = t[10],
    ) else lightColorScheme(
        primary = t[0], onPrimary = t[1], primaryContainer = t[2], onPrimaryContainer = t[3],
        secondary = t[4], secondaryContainer = t[5], onSecondaryContainer = t[6],
        surface = t[7], background = t[8], surfaceVariant = t[9], onSurfaceVariant = t[10],
    )
}

/** AMOLED black: the dark surfaces nori_look puts in their place (`dress::AMOLED`), so those pixels are off. */
private fun ColorScheme.black(): ColorScheme {
    val a = dev.nori.music.look.CoverLook.amoled()
    return copy(
        background = Color(a[0]), surface = Color(a[1]), surfaceDim = Color(a[2]),
        surfaceContainerLowest = Color(a[3]), surfaceContainerLow = Color(a[4]), surfaceContainer = Color(a[5]),
        surfaceContainerHigh = Color(a[6]), surfaceContainerHighest = Color(a[7]),
    )
}
