package dev.nori.music.app

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.platform.LocalContext
import androidx.navigation.NavHostController
import dev.nori.music.app.ui.Nav
import dev.nori.music.app.ui.PlayerSheet
import dev.nori.music.app.ui.effectiveStar
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.data.FoundLyrics
import dev.nori.music.ffi.model.Song
import kotlinx.coroutines.flow.Flow
import dev.nori.music.settings.server
import dev.nori.music.settings.loggedIn

/*
 * The debug build's side of the test bridge (TestBridge, TestHooks, TestActions): the handles a command
 * that came over adb reaches the app by. Release and perf builds have the empty twin of this file
 * (src/noTest), so none of it is in them. Main reads [TestDriver], [testLyrics] and [traceLyrics].
 */

/** Where the lyrics panel's words come from while a check has set them (TestHooks.lyrics); null: the real lookup. */
fun testLyrics(song: Song): Flow<FoundLyrics>? = TestHooks.lyrics?.invoke(song)

/** The lyrics panel logs every reading it takes (tag norilyrics) while a check has asked for it. */
val traceLyrics: Boolean get() = TestHooks.traceLyrics

/** The test bridge's handles, live for as long as the signed-in app is on screen. See TestHooks. */
@Composable
fun TestDriver(controller: NavHostController, nav: Nav, sheet: PlayerSheet, settings: SettingsViewModel, actions: ActionsViewModel, player: PlayerViewModel) {
    val context = LocalContext.current
    val player2 = player
    DisposableEffect(controller) {
        TestHooks.open = { route -> if (route == "player") sheet.open() else nav.go(route) }
        TestHooks.set = { name, value -> TestActions.setByName(context, settings, name, value) }
        TestHooks.play = { what -> TestActions.playByRef(context, actions, what) }
        TestHooks.login = { spec ->
            val (url, user, pass) = spec.split("|").let { Triple(it[0], it.getOrElse(1) { "" }, it.getOrElse(2) { "" }) }
            settings.login(settings.newProfile().copy(url = url, user = user, password = pass))
        }
        TestHooks.act = { what -> TestActions.act(context, actions, what, player2) }
        TestHooks.state = {
            val st = player2.state.value
            val p = settings.prefs.value
            """{"route":"${if (sheet.isOpen) "player" else controller.currentBackStackEntry?.destination?.route}",""" +
                """"playing":${st.playing},"title":"${st.current?.title.orEmpty().replace("\"", "'")}","artist":"${st.current?.artist.orEmpty().replace("\"", "'")}",""" +
                """"positionMs":${player2.positionMs},"durationMs":${st.durationMs},"queue":${st.queue.size},"index":${st.index},""" +
                // The next songs in the order they will play, and which of them were added by hand.
                st.order.drop(st.order.indexOf(st.index) + 1).take(8).let { up ->
                    """"upNext":"${up.joinToString(" ") { st.queue[it].id }}","upNextQueued":"${up.joinToString(" ") { if (it in st.queued) "1" else "0" }}","shuffle":${st.shuffle},"""
                } +
                """"error":"${st.error.orEmpty()}","bridging":${st.bridging},"parkedId":"${dev.nori.music.ffi.queue.playlistBridgeState().parked.orEmpty()}","songId":"${dev.nori.music.ffi.queue.playlistBridgeState().current.orEmpty()}","eq":${p.eqEnabled},"limiter":${p.limiter},"hiRes":${p.hiRes},""" +
                """"dspActive":${dev.nori.music.playback.Equalizer.inChain},"gainReductionDb":${dev.nori.music.playback.Equalizer.meterDb},"compressionDb":${dev.nori.music.playback.Equalizer.compressionDb},"soundBypass":${p.soundBypass},"loudness":${p.loudness},""" +
                """"output":"${settings.currentOutput.value}","offload":${p.offload},"offloadWanted":${dev.nori.music.playback.PlaybackService.offloadWanted},"autoMix":${p.autoMix},"amoled":${p.amoled},""" +
                // The player answers from its own engine and output.
                dev.nori.music.playback.PlaybackService.rustPlayer.let { r ->
                    """"engine":"rust","mixing":${r?.mixing ?: false},"offloaded":${r?.offloaded ?: false},"""
                } +
                """"downloaded":${actions.downloads.value.doneCount},"downloading":${actions.downloads.value.pendingCount},"dlActive":${actions.downloadMarks.value.values.count { it.phase == dev.nori.music.downloads.DownloadPhase.DOWNLOADING }},"dlProgress":"${actions.downloadMarks.value.values.filter { it.phase == dev.nori.music.downloads.DownloadPhase.DOWNLOADING }.joinToString(" ") { "%.2f".format(it.progress.value) }}","dlSpeed":${dev.nori.music.ffi.transfers.downloadSpeedEta()[0]},"dlEta":${dev.nori.music.ffi.transfers.downloadSpeedEta()[1]},""" +
                """"sinkBytes":${dev.nori.music.playback.PlaybackService.rustPlayer?.bytesWritten ?: 0},""" +
                // Moving covers: how many video players exist (nought whenever the switch is off) and
                // the video the open player found for this album, if any.
                """"motionPlayers":${dev.nori.music.playback.MotionPlayer.live},"motionVideo":"${player2.motionVideo.value.orEmpty()}",""" +
                // Everything the app's Java side has allocated since it started, for allocation checks.
                """"allocBytes":${android.os.Debug.getRuntimeStat("art.gc.bytes-allocated") ?: -1},""" +
                // What the app itself holds, KB (the perf report's memory line): the native heap allocated,
                // the Rust heap, the songs' bytes in memory, the ring, the beat model, the covers' Bitmaps.
                dev.nori.music.ffi.perf.perfRustMemory().let { r ->
                    val covers = dev.nori.music.data.CoverLoader.get(context)
                    """"memNative":${android.os.Debug.getNativeHeapAllocatedSize() / 1024},"memRust":${r.heapKb},"memSongs":${r.songsKb},"memSongLoaders":${r.songs},""" +
                        """"memSongsOnDisk":${r.songsOnDisk},"memRing":${r.ringKb},"memModel":${r.modelKb},"memCovers":${covers.keptBytes() / 1024},"memCoverCount":${covers.keptCount()},"""
                } +
                dev.nori.music.Nori.get(context).dac.state.value.let { d ->
                    """"dac":"${d.device.orEmpty()}","bitPerfect":${d.bitPerfect},"dacModes":${d.modes.size},""" +
                        """"dacBlocked":"${d.blockedBy?.let { dev.nori.music.app.vm.dacBlockWords(context.resources, it) }.orEmpty()}","dacTrack":"${d.track?.let { dev.nori.music.app.vm.dacTrackWords(context.resources, it) }.orEmpty()}","""
                } +
                (TestActions.lastLyrics ?: (player2.lyrics.value.value as? dev.nori.music.app.vm.Load.Ready)?.data)?.let { f ->
                    """"lyricLines":${f.lyrics.lines.size},"lyricsSynced":${f.lyrics.synced},""" +
                        """"lyricsWordTimed":${f.lyrics.wordTimed},"lyricsWordLines":${f.lyrics.lines.count { it.words.isNotEmpty() }},"lyricsSource":"${f.source}","""
                }.orEmpty() +
                // What the screen shows, mark included - not the snapshot the queue was painted with,
                // which is what a favourite toggled this session no longer agrees with.
                """"starred":${st.current?.let { actions.starMarks.value.effectiveStar(dev.nori.music.data.StarKind.SONG, it.id, it.starred) } ?: false},""" +
                """"notification":"${dev.nori.music.Nori.get(context).player.sessionButtons}",""" +
                // The app's own update: the state's kind ("Available", "UpToDate", ...) and its line in About.
                settings.update.value.let { u ->
                    """"update":"${u::class.simpleName}","updateLine":"${dev.nori.music.app.vm.updateWords(context.resources, u, BuildConfig.VERSION_NAME, settings.installsUpdates).replace("\"", "'")}","""
                } +
                """"loggedIn":${p.loggedIn},"server":"${p.server?.url.orEmpty()}","loginError":"${settings.login.value.error.orEmpty().replace("\"", "'")}"}"""
        }
        onDispose {
            TestHooks.open = null
            TestHooks.state = null
            TestHooks.set = null
            TestHooks.play = null
        }
    }
}
