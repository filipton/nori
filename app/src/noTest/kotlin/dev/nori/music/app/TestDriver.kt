package dev.nori.music.app

import androidx.compose.runtime.Composable
import androidx.compose.runtime.NonRestartableComposable
import androidx.navigation.NavHostController
import dev.nori.music.app.ui.Nav
import dev.nori.music.app.ui.PlayerSheet
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.data.FoundLyrics
import dev.nori.music.ffi.model.Song
import kotlinx.coroutines.flow.Flow

/*
 * The release and perf builds' side of the test bridge: nothing. The debug build's twin of this file
 * (src/debug) drives the app over adb; these have the same names so main compiles against either, and
 * the minifier removes them whole.
 */

@Suppress("NOTHING_TO_INLINE", "UNUSED_PARAMETER")
inline fun testLyrics(song: Song): Flow<FoundLyrics>? = null

const val traceLyrics: Boolean = false

@Suppress("UNUSED_PARAMETER")
@Composable
@NonRestartableComposable
fun TestDriver(controller: NavHostController, nav: Nav, sheet: PlayerSheet, settings: SettingsViewModel, actions: ActionsViewModel, player: PlayerViewModel, devices: () -> Unit) {}
