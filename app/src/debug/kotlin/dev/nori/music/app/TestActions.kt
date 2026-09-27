package dev.nori.music.app

import android.content.Context
import androidx.lifecycle.viewModelScope
import dev.nori.music.Nori
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.data.FoundLyrics
import dev.nori.music.ffi.devices.SpecKind
import dev.nori.music.ffi.devices.deviceSpec
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.queue.TestRef
import dev.nori.music.ffi.queue.testRef
import dev.nori.music.playback.DeviceSound
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.last
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Debug builds only: what the test bridge's `set`, `play` and `do` commands do, so a script never has to
 * find a button on screen. Over the same ViewModels the screens use; the few things no screen offers
 * (a mock DAC, made-up lyrics) straight at the core's objects.
 */
object TestActions {
    /** The last lyrics `do lyrics` asked for, so the state dump can report what arrived. */
    @Volatile var lastLyrics: FoundLyrics? = null
        private set

    /**
     * Flips one setting by name. Only the switches a check needs; anything else returns false so a typo
     * in a script fails loudly instead of silently doing nothing.
     */
    fun setByName(context: Context, settings: SettingsViewModel, name: String, value: String): Boolean {
        if (device(context, settings, name, value)) return true
        // Not a setting: the one button on that screen a check needs, so a run can start from a phone
        // that has measured nothing and see the measuring happen.
        if (name == "clearAnalyses") { settings.clearAnalyses(); return true }
        // The "Streamed music" button: a check that needs a song to be fetched cannot have it cached.
        if (name == "clearStreamCache") { settings.clearStreamCache(); return true }
        // The "Lyrics" button under Storage, without its question.
        if (name == "clearLyricsCache") { settings.clearLyrics(); return true }
        // Which names exist, how each value reads and the ranges are the core's (settings::set_by_name),
        // the same the settings screen's rows use and the settings are loaded with.
        return settings.set(name, value)
    }

    /**
     * The device-sound half: `set deviceSound "<output>=flat|auto|quiet|profile:<name>|curve:<search>"`,
     * `set saveProfile <name>`, `set deleteProfile <name>`, `set forgetDevice <output>`, `set autoEqIndex 1`,
     * `set eqNotice apply|undo` (presses the snackbar's button, whichever notice is up).
     */
    private fun device(context: Context, settings: SettingsViewModel, name: String, value: String): Boolean {
        val nori = Nori.get(context)
        val devices = nori.deviceSound
        when (name) {
            "deviceSound" -> settings.viewModelScope.launch {
                val spec = deviceSpec(value)
                val choice = when (spec.kind) {
                    SpecKind.FLAT -> DeviceSound.Choice.Flat
                    SpecKind.QUIET -> DeviceSound.Choice.Quiet
                    SpecKind.PROFILE -> DeviceSound.Choice.Profile(spec.arg)
                    SpecKind.CURVE -> withContext(Dispatchers.IO) { nori.core.autoeqSearch(spec.arg, 1u) }.firstOrNull()?.let { DeviceSound.Choice.Curve(it) } ?: return@launch
                    SpecKind.AUTOMATIC -> DeviceSound.Choice.Automatic
                }
                settings.assignDevice(spec.output, choice)
            }
            "saveProfile" -> settings.saveProfile(value)
            "deleteProfile" -> settings.deleteProfile(value)
            "forgetDevice" -> settings.forgetDevice(value)
            "autoEqIndex" -> settings.downloadAutoEqIndex()
            "eqNotice" -> devices.lastNotice?.let { n ->
                settings.viewModelScope.launch {
                    when (n) {
                        is DeviceSound.Offer -> if (value == "apply") devices.accept(n)
                        is DeviceSound.Applied -> if (value == "undo") devices.undo(n)
                    }
                    devices.consume(n)
                }
            }
            else -> return false
        }
        return true
    }

    /**
     * "song:<id>", "album:<id>", "search:<text>" (first song hit) or "downloaded:<n>". An album is played
     * as its page's Play plays it, so the page answers for it.
     */
    fun playByRef(context: Context, actions: ActionsViewModel, ref: String) = actions.attempt(null) {
        val nori = Nori.get(context)
        val r = testRef(ref)
        val songs = songsOf(nori, r)
        val from = (r as? TestRef.Album)?.let { dev.nori.music.ffi.model.PageOrigin(dev.nori.music.ffi.model.OriginKind.ALBUM, it.id) }
        if (songs.isNotEmpty()) nori.player.play(songs, 0, from = from)
    }

    /**
     * One-word actions a check needs to drive: "download <ref>", "star <ref>", "pause", "resume", "next",
     * "previous", and the rest below.
     */
    fun act(context: Context, actions: ActionsViewModel, what: String, player: PlayerViewModel) = actions.attempt(null) {
        val nori = Nori.get(context)
        val verb = what.substringBefore(' ')
        val ref = what.substringAfter(' ', "")
        val songs = songsOf(nori, testRef(ref))
        when (verb) {
            // Lyrics load only while the lyrics panel is watching them, which a headless check is not:
            // this asks for them the same way the panel does and parks the answer for the state dump.
            "lyrics" -> {
                val song = songs.firstOrNull() ?: nori.player.state.value.current ?: return@attempt
                // The flow emits the server's answer first and the LRCLIB fallback second; the last one
                // is the one the screen would end up showing.
                lastLyrics = nori.library.lyricsFor(song).last()
            }
            "seek" -> nori.player.seekTo(ref.toLongOrNull() ?: 0L)
            // "tracelyrics on|off": the lyrics panel logs each reading it takes and the moment it shows (tag norilyrics).
            "tracelyrics" -> { TestHooks.traceLyrics = ref == "on"; dev.nori.music.playback.tracePositions = ref == "on" }
            // "fakelyrics <ms>": every song's lyrics come timed by the line at once and then, <ms> later,
            // the same words timed word by word, as a slower and finer service replaces a fast one in the
            // lyrics race. "fakelyrics off" goes back to the real lookup.
            // "fakelyrics <ms>,<shift>" starts every line <shift> ms later, to put a word mid-fill at a given moment;
            // "fakelyrics <ms>,<shift>,<later>" times the finer words <later> ms later still, as a service whose
            // timing fits the song better replaces one that ran early.
            "fakelyrics" -> TestHooks.lyrics = if (ref == "off") null else ref.split(',').let { a ->
                val slow = a[0].toLongOrNull() ?: return@let null
                val shift = a.getOrNull(1)?.toLongOrNull() ?: 0L
                val later = a.getOrNull(2)?.toLongOrNull() ?: 0L
                { song -> fakeLyrics(song, slow, shift, later) }
            }
            // "dac <name>@44100/16,96000/24" pretends a USB DAC with those bit-perfect modes is attached;
            // "dac off" hands the app back to the real audio system. See DacSource.mock.
            "dac" -> {
                val off = ref.isEmpty() || ref == "off"
                nori.dac.testSource(if (off) null else dev.nori.music.playback.DacSource.mock(ref))
                nori.outputs.testUsb(if (off) null else ref.substringBefore('@').ifEmpty { "Mock DAC" })
            }
            "download" -> actions.download(songs)
            // "undownload <ref>": the song menu's "Remove download(s)", so a check can download the same album again.
            "undownload" -> actions.removeDownloads(songs.map { it.id })
            // Everything not yet downloaded is dropped; finished downloads stay.
            "canceldownloads" -> actions.cancelAllDownloads()
            "star" -> songs.firstOrNull()?.let { actions.star(it, !it.starred) }
            // "notification favourite" / "notification shuffle": the session command the notification's button sends.
            "notification" -> nori.player.pressSessionButton(if (ref == "shuffle") dev.nori.music.playback.PlaybackService.CMD_SHUFFLE else dev.nori.music.playback.PlaybackService.CMD_FAVOURITE)
            "pause" -> player.toggle()
            "resume" -> player.toggle()
            "next" -> player.next()
            "previous" -> player.previous()
            // What the equalizer screen sends while it is open: the shallow buffer for live
            // tweaking, then back. For the checks that the deep buffer returns afterwards.
            "tuning" -> nori.player.setTuning(ref == "on")
            "enqueue" -> actions.enqueue(songs)
            "playnext" -> actions.playNext(songs)
            "shuffle" -> player.toggleShuffle()
            // "newplaylist <name>|<ref>": the checks create one, look for it on the server, then delete it.
            "newplaylist" -> {
                val name = ref.substringBefore('|')
                val pick = ref.substringAfter('|', "")
                val tracks = (testRef(pick) as? TestRef.Search)?.let { songsOf(nori, it) }.orEmpty()
                nori.library.createPlaylist(name, tracks.map { it.id })
            }
        }
    }

    /**
     * What a test reference plays. Never a provider song: asking the server for an `ext-` item makes
     * octo-fiesta fetch it, so the test tools pass over them, even inside an album that mixes them in.
     */
    private suspend fun songsOf(nori: Nori, ref: TestRef): List<Song> = when (ref) {
        is TestRef.Album -> nori.library.album(ref.id).first().songs.filterNot { it.id.startsWith("ext-") }
        is TestRef.Song -> listOfNotNull(nori.library.song(ref.id)).filterNot { it.id.startsWith("ext-") }
        is TestRef.Search -> nori.library.search(ref.text).songs.filterNot { it.id.startsWith("ext-") }.take(1)
        // Straight from what is already on the device: the only way to start playback with the
        // network off, and therefore the only honest test of offline playback. Read from the core's table,
        // not the published state, which right after a start can still be the empty one it begins as.
        is TestRef.Downloaded -> withContext(Dispatchers.IO) { nori.downloads.doneSongs() }.drop(ref.index.toInt()).take(1)
        is TestRef.DownloadedSong -> withContext(Dispatchers.IO) { nori.downloads.doneSongs() }.filter { it.id == ref.id }
        TestRef.Nothing -> emptyList()
    }

    /** Made-up lyrics for [song] (see "fakelyrics"): a line every four seconds, by the line, then by the word after [slowMs]. */
    private fun fakeLyrics(song: Song, slowMs: Long, shiftMs: Long = 0, laterMs: Long = 0): kotlinx.coroutines.flow.Flow<FoundLyrics> = kotlinx.coroutines.flow.flow {
        val words = listOf("Somewhere", "the", "night", "is", "turning", "slowly", "over", "the", "water", "tonight", "and", "we")
        val total = (song.duration.toLong() * 1000).coerceAtLeast(60_000)
        fun lines(timed: Boolean) = (0 until (total / 4000).toInt()).map { i ->
            val start = 2000L + shiftMs + (if (timed) laterMs else 0L) + i * 4000L
            val n = 3 + i % 4
            val picked = List(n) { words[(i * 5 + it) % words.size] }
            val text = picked.joinToString(" ")
            var at = 0
            val timedWords = if (!timed) emptyList() else picked.mapIndexed { k, w ->
                val from = at; at += w.length + 1
                dev.nori.music.ffi.model.LyricWord(start + k * 3000L / n, start + (k + 1) * 3000L / n, from.toUInt(), (from + w.length).toUInt())
            }
            dev.nori.music.ffi.model.LyricLine(start, start + 3500, text, timedWords, null, false, "", emptyList(), 0u)
        }
        emit(FoundLyrics(dev.nori.music.ffi.model.Lyrics(true, false, lines(false), 0uL), dev.nori.music.ffi.settings.LyricsOrigin.LRCLIB))
        kotlinx.coroutines.delay(slowMs)
        emit(FoundLyrics(dev.nori.music.ffi.model.Lyrics(true, true, lines(true), 0uL), dev.nori.music.ffi.settings.LyricsOrigin.BETTER_LYRICS))
    }
}
