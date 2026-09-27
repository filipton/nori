package dev.nori.music.app.vm

import android.app.Application
import androidx.lifecycle.viewModelScope
import dev.nori.music.ffi.model.IngestStats
import dev.nori.music.net.said
import dev.nori.music.playback.DacState
import dev.nori.music.playback.DeviceSound
import dev.nori.music.playback.Outputs
import dev.nori.music.ffi.settings.StoredPrefs
import dev.nori.music.ffi.settings.SavedServer
import dev.nori.music.net.describeConnectionError
import dev.nori.music.ffi.model.MusicFolder
import dev.nori.music.ffi.settings.SoundBand
import dev.nori.music.ffi.settings.HomeRow
import dev.nori.music.ffi.devices.ChoiceKind
import dev.nori.music.ffi.devices.deviceRows
import dev.nori.music.ffi.settings.eqPresets
import dev.nori.music.ffi.settings.SoundTool
import dev.nori.music.ffi.settings.EqLevel
import dev.nori.music.ffi.settings.SettingChange
import dev.nori.music.ffi.settings.serverNewId
import dev.nori.music.ffi.settings.settingSet
import dev.nori.music.ffi.settings.soundFromJson
import dev.nori.music.ffi.settings.storageIndexFiles
import dev.nori.music.ffi.model.NamedPreset
import dev.nori.music.ffi.model.AutoEqEntry
import dev.nori.music.ffi.settings.SoundException
import dev.nori.music.ffi.model.SoundProfile
import dev.nori.music.settings.sound
import dev.nori.music.settings.withSound
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.launch
import dev.nori.music.settings.server
import dev.nori.music.update.Updates

data class LoginUi(val busy: Boolean = false, val error: String? = null, val done: Boolean = false)
/**
 * The AutoEQ list: how many headphones it holds ([countWords] and [searchWords] say so), the query and
 * its hits, and whether the query is still too short to search (the core's `autoeq_too_short`).
 */
data class AutoEqUi(
    val count: Int = 0,
    val query: String = "",
    val hits: List<AutoEqHit> = emptyList(),
    val tooShort: Boolean = true,
    val busy: Boolean = false,
    val applied: String? = null,
    val error: String? = null,
    val countWords: String = "",
    val searchWords: String = "",
)

/**
 * One output device in the equalizer's device list: [name] is what it calls itself, [kind] where it is
 * plugged in, [sound] what it gets (a profile's name, "Flat", "Automatic", "Leave as is").
 */
data class DeviceRow(val output: String, val name: String, val kind: String?, val current: Boolean, val sound: String, val choice: DeviceSound.Choice)

/** One AutoEQ curve with the lines under it, in the app's words: [caption] in the browser, [short] in a device's sheet. */
data class AutoEqHit(val entry: dev.nori.music.ffi.model.AutoEqEntry, val caption: String, val short: String) {
    companion object {
        fun of(e: dev.nori.music.ffi.model.AutoEqEntry) = AutoEqHit(e, dev.nori.music.app.ui.say.autoeqCaption(e), dev.nori.music.app.ui.say.autoeqShort(e))
    }
}

/** A line for the snackbar about the device that just connected, with the one thing it offers to do. */
data class EqNotice(val message: String, val action: String, val source: DeviceSound.Notice)

data class SyncUi(val running: Boolean = false, val indexed: IngestStats = IngestStats(0u, 0u, 0u), val error: String? = null)

/** What lives on the phone: streamed music, covers, lyrics found online, finished downloads and the library index. */
data class StorageUi(
    val streamBytes: Long = 0L,
    val coverBytes: Long = 0L,
    val lyricsBytes: Long = 0L,
    val downloadBytes: Long = 0L,
    val downloadSongs: Int = 0,
    val indexBytes: Long = 0L,
    val busy: Boolean = false,
)

class SettingsViewModel(app: Application) : NoriViewModel(app) {
    val prefs: StateFlow<StoredPrefs> = nori.settings.prefs
    val dac: StateFlow<DacState> = nori.dac.state
    private val _login = MutableStateFlow(LoginUi())
    val login: StateFlow<LoginUi> = _login
    private val _sync = MutableStateFlow(SyncUi())
    val sync: StateFlow<SyncUi> = _sync
    private var syncJob: Job? = null

    init { viewModelScope.launch { runCatching { nori.library.indexSize() }.onSuccess { n -> _sync.update { it.copy(indexed = n) } } } }

    fun update(change: (StoredPrefs) -> StoredPrefs) = nori.settings.update(change)

    // ---- the settings screen (SettingsPages.kt), on the core's settings model (nori-settings, settings_model.rs) ----

    /** The groups the root of Settings lists, in [res]'s language. */
    fun settingsGroups(res: android.content.res.Resources): List<SettingsGroup> = dev.nori.music.app.vm.settingsGroups(res)

    private var search: Pair<android.content.res.Resources, SettingsSearch>? = null

    /** The rows whose title (first) or words (after) contain [query]; the index is built once per [res]. */
    fun searchSettings(query: String, res: android.content.res.Resources): List<SettingsHit> {
        val s = search?.takeIf { it.first === res }?.second
            ?: SettingsSearch(res, dev.nori.music.ffi.settings.settingsState(false, false).beatModel !is dev.nori.music.ffi.settings.BeatModel.Unavailable).also { search = res to it }
        return s.find(query)
    }

    /**
     * One group's page for settings [p] and [facts], in [res]'s language: one call into the core for what
     * its rules make of the settings, asked only when they or [facts] change.
     */
    fun settingsPage(id: String, p: StoredPrefs, facts: SettingsFacts, res: android.content.res.Resources): SettingsPage? =
        settingsPage(res, id, p, facts, dev.nori.music.ffi.settings.settingsState(facts.dac.bitPerfect, facts.dac.device != null))

    /** Whether the settings action [action] asks first, and what it says; null for one done at once. */
    fun actionAsks(action: String, res: android.content.res.Resources): ActionAsk? = settingsActionAsks(res, action, settingsFacts.value)

    /** What a settings page depends on besides the settings, from this platform. */
    val settingsFacts: StateFlow<SettingsFacts> by lazy {
        combine(combine(dac, _sync, _storage, _analysed, _folders, ::factsOf), update) { f, u -> f.copy(update = u, installsUpdates = nori.updates.installs) }
            .stateIn(
                viewModelScope, SharingStarted.WhileSubscribed(5_000),
                factsOf(dac.value, _sync.value, _storage.value, _analysed.value, _folders.value).copy(update = update.value, installsUpdates = nori.updates.installs),
            )
    }

    private fun factsOf(d: DacState, s: SyncUi, st: StorageUi, analysed: Int, folders: List<MusicFolder>) = SettingsFacts(
        dac = d,
        // Wallpaper colours and the blurred sleeve both need Android 12.
        wallpaperColours = android.os.Build.VERSION.SDK_INT >= 31,
        coverBlur = android.os.Build.VERSION.SDK_INT >= 31,
        analysed = analysed,
        sync = s,
        storage = st,
        folders = folders,
    )

    /** A row's setting changed: its name and the value picked, which the core reads and applies; false for a name or value it does not take. */
    fun set(name: String, value: String): Boolean {
        apply(settingSet(name, value) ?: return false)
        return true
    }

    /**
     * A ranked row (a lyrics service) dragged by its handle and dropped at place [to] of the one list;
     * whether the core took it. The ranking itself is the core's (`lyricsPlace`).
     */
    fun placeRanked(id: String, to: Int): Boolean {
        val change = settingSet("lyricsPlace", "$id:$to") ?: return false
        apply(change)
        return true
    }

    private fun apply(change: SettingChange) {
        // The active server's own settings go through the server's update, which connects it again.
        if (change.server) {
            change.prefs.server?.let(nori::updateServer)
            return
        }
        nori.settings.took(change)
        if (change.applyCacheLimit) applyCacheLimit()
    }

    /** A button on a settings row. */
    fun act(action: String) {
        when (action) {
            "measure-again" -> clearAnalyses()
            "sync-library" -> syncLibrary()
            "download-library" -> downloadLibrary()
            "clear-stream" -> clearStreamCache()
            "clear-covers" -> clearCovers()
            "clear-lyrics" -> clearLyrics()
            "update-check" -> checkForUpdates()
            "update-go" -> updateNow()
            "update-cancel" -> cancelUpdate()
        }
    }

    // ---- the app's own updates (core update/Updates.kt over nori-core update.rs) ----

    /** Where an update stands: About's row and the banner both draw it. */
    val update: StateFlow<Updates.State> = nori.updates.state

    /** Whether this build installs updates; a debug or perf build only points at the release. */
    val installsUpdates: Boolean get() = nori.updates.installs

    fun checkForUpdates() = nori.updates.checkNow()

    /**
     * The row's or the banner's main button, whatever it says now: download and install (a release build), the
     * release's page (any other build, or a release with no APK for this phone), Android's "install unknown
     * apps" page while that is what is missing, or another try after a failure.
     */
    fun updateNow() {
        val u = nori.updates
        when (val s = update.value) {
            is Updates.State.Available -> if (u.installs) u.update(s.update) else u.openPage(s.update.page)
            is Updates.State.Failed -> if ((s.why as? Updates.Failure.Install)?.conflict == true) u.openPage(s.update.page) else u.update(s.update)
            is Updates.State.NeedsPermission -> u.allowInstalls()
            is Updates.State.NoApk -> u.openPage(s.page)
            else -> u.checkNow()
        }
    }

    /** "Later": this version is not brought up again by itself. */
    fun updateLater() = nori.updates.later()

    fun cancelUpdate() = nori.updates.cancel()

    /** Applies "Space for streamed music" at once instead of at the next track. */
    fun applyCacheLimit() = viewModelScope.launch(Dispatchers.IO) { nori.applyCacheLimit() }

    // ---- storage ----

    private val _storage = MutableStateFlow(StorageUi())
    val storage: StateFlow<StorageUi> = _storage

    /** Measures what is on the phone; the caches answer from their index, the rest is weighed. */
    fun refreshStorage() = viewModelScope.launch(Dispatchers.IO) {
        val app = getApplication<Application>()
        // Which files are the index is the core's rule; weighing them is the file system's.
        val files = app.filesDir.listFiles().orEmpty()
        val index = storageIndexFiles(files.map { it.name })
        _storage.value = StorageUi(
            streamBytes = nori.sources.streamBytes(),
            coverBytes = dirBytes(java.io.File(app.cacheDir, dev.nori.music.data.CoverLoader.DIR)),
            lyricsBytes = runCatching { nori.core.lyricsCacheBytes() }.getOrDefault(0L),
            downloadBytes = nori.sources.downloadBytes(),
            downloadSongs = nori.downloads.state.value.done.size,
            indexBytes = index.sumOf { dirBytes(files[it.toInt()]) },
        )
    }

    /** Empties the streamed-music cache; downloads, covers and the index stay. */
    fun clearStreamCache() = viewModelScope.launch(Dispatchers.IO) {
        _storage.update { it.copy(busy = true) }
        nori.sources.clearStream()
        refreshStorage()
    }

    /** Empties the cover cache; pictures are fetched again as they are shown. */
    fun clearCovers() = viewModelScope.launch(Dispatchers.IO) {
        _storage.update { it.copy(busy = true) }
        dev.nori.music.data.CoverLoader.get(getApplication()).clearDisk()
        refreshStorage()
    }

    /** Forgets the lyrics found online (the core's `lyrics_cache_clear`); they are looked up again when opened. */
    fun clearLyrics() = viewModelScope.launch(Dispatchers.IO) {
        _storage.update { it.copy(busy = true) }
        runCatching { nori.core.lyricsCacheClear() }
        refreshStorage()
    }

    private fun dirBytes(f: java.io.File): Long {
        if (f.isFile) return f.length()
        return f.listFiles()?.sumOf(::dirBytes) ?: 0L
    }

    /** A blank profile for the "add server" form, with a fresh id from the core. */
    fun newProfile() = dev.nori.music.settings.newServer(serverNewId())

    fun login(profile: SavedServer) {
        if (_login.value.busy) return
        _login.value = LoginUi(busy = true)
        viewModelScope.launch {
            _login.value = try {
                nori.login(profile)
                LoginUi(done = true)
            } catch (e: Exception) {
                LoginUi(error = describeConnectionError(e))
            }
        }
    }

    fun clearLoginResult() { _login.value = LoginUi() }
    fun switchServer(profile: SavedServer) = nori.activate(profile)
    fun updateServer(profile: SavedServer) = nori.updateServer(profile)
    fun removeServer(id: String) = nori.removeServer(id)

    /** Copies a picked PKCS#12 file into the app and returns the profile that uses it. */
    fun importClientCert(profile: SavedServer, uri: android.net.Uri, password: String): SavedServer {
        val name = "${profile.id}.p12"
        val dir = java.io.File(getApplication<Application>().filesDir, "certs").apply { mkdirs() }
        getApplication<Application>().contentResolver.openInputStream(uri)?.use { input -> java.io.File(dir, name).outputStream().use { input.copyTo(it) } }
        return profile.copy(clientCert = name, clientCertPassword = password)
    }

    private val _folders = MutableStateFlow<List<MusicFolder>>(emptyList())
    val musicFolders: StateFlow<List<MusicFolder>> = _folders
    fun loadMusicFolders() = viewModelScope.launch { runCatching { nori.library.musicFolders() }.onSuccess { _folders.value = it } }

    fun logout() = nori.logout()

    // ---- equalizer ----

    /** Moves one home shelf up or down the page (the core's `home_rows_moved`). */
    fun moveHomeRow(from: Int, to: Int) = update { p ->
        p.copy(homeRows = dev.nori.music.ffi.library.homeRowsMoved(p.homeRows.map { it.name }, from.toUInt(), to.toUInt()).map { HomeRow.valueOf(it) })
    }

    /** The equalizer screen is open: the player answers a moved slider at once instead of seconds later. */
    fun setTuning(on: Boolean) = nori.player.setTuning(on)

    /** The built-in curves, straight from the core so the numbers live in one place. */
    val presets: List<NamedPreset> by lazy { eqPresets() }

    // The edits themselves (a preset's pre-amp of 0 meaning automatic, the new band's defaults, the
    // graphic bands coming back when the last one goes) are the core's (settings.rs), made where it keeps
    // the settings (settings_store::settings_sound_tool).
    fun applyPreset(p: NamedPreset) { nori.settings.soundTool(SoundTool.Preset(p)) }

    /** One band changed, held in the equalizer's ranges by the core; on every step of a drag, so edited in place there. */
    fun setBand(index: Int, band: SoundBand) = nori.settings.setBand(index, band)

    /** Pre-amp, balance, limiter ceiling or crossfeed moved; the core holds it in range and snaps it, in place like a band. */
    fun setLevel(level: EqLevel, value: Float) = nori.settings.setLevel(level, value)

    /** One graphic equalizer slider moved; in place in the core, like a band. */
    fun setGraphic(index: Int, value: Float) = nori.settings.setGraphic(index, value)

    /** The graphic or the parametric equalizer plays; each keeps its own settings. */
    fun setEqMode(mode: dev.nori.music.ffi.settings.EqMode) { set("eqMode", mode.name) }

    /** How many graphic bands (10, 15 or 31): the core draws the same curve on the new layout. */
    fun setEqLayout(count: Int) { set("eqLayout", count.toString()) }

    /** The automatic pre-amp on or off; off starts from the level it was at. */
    fun setAutoPreamp(automatic: Boolean) { nori.settings.soundTool(SoundTool.AutoPreamp(automatic)) }
    fun addBand() { nori.settings.soundTool(SoundTool.AddBand) }
    fun removeBand(index: Int) { nori.settings.soundTool(SoundTool.RemoveBand(index.toUInt())) }
    fun resetBands() { nori.settings.soundTool(SoundTool.ResetBands) }

    /** AutoEQ "ParametricEQ.txt" / Equalizer APO text. Returns how many filters were found. */
    fun importPreset(text: String): Int = try { import(text) } catch (e: SoundException) { 0 }

    /** Switches the preset in [text] on; throws, saying why, when it has no filters in it. */
    private fun import(text: String): Int = nori.settings.soundTool(SoundTool.Import(text))

    // ---- saved profiles and the AutoEQ database ----

    private val devices = nori.deviceSound
    val profiles: StateFlow<List<SoundProfile>> = devices.profiles
    val currentOutput: StateFlow<String> = nori.outputs.current

    init { refreshProfiles() }
    private fun refreshProfiles() = viewModelScope.launch { devices.refresh() }

    /** Every output seen, the one playing now first, each with the sound it gets. */
    val deviceRows: StateFlow<List<DeviceRow>> = combine(nori.outputs.known, currentOutput, profiles, devices.quiet, ::deviceRowsOf)
        // Filled from the start, so the list is there on the screen's first frame rather than popping in.
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), deviceRowsOf(nori.outputs.known.value, currentOutput.value, profiles.value, devices.quiet.value))

    /** The rows are the core's (nori_player::device::rows); their words are Say's, made once per change of the list. */
    private fun deviceRowsOf(known: List<String>, current: String, profiles: List<SoundProfile>, quiet: List<String>): List<DeviceRow> =
        deviceRows(known, current, profiles, quiet).map { r ->
            val choice = when (r.choice) {
                ChoiceKind.AUTOMATIC -> DeviceSound.Choice.Automatic
                ChoiceKind.QUIET -> DeviceSound.Choice.Quiet
                ChoiceKind.FLAT -> DeviceSound.Choice.Flat
                ChoiceKind.PROFILE -> DeviceSound.Choice.Profile(r.profile.orEmpty())
            }
            val say = dev.nori.music.app.ui.say
            val sound = when (r.choice) {
                ChoiceKind.AUTOMATIC -> say.automatic
                ChoiceKind.QUIET -> say.leaveAsIs
                ChoiceKind.FLAT -> say.flat
                ChoiceKind.PROFILE -> r.profile.orEmpty()
            }
            DeviceRow(r.output, say.outputName(r.port, r.name), say.outputKind(r.port), r.current, sound, choice)
        }

    private val _assigning = MutableStateFlow<String?>(null)
    /** The device whose sound is being fetched and saved right now (an AutoEQ curve is a download). */
    val assigning: StateFlow<String?> = _assigning
    private val _assignError = MutableStateFlow<String?>(null)
    val assignError: StateFlow<String?> = _assignError

    /** Gives [output] its own sound; [onDone] runs once it is saved (and loaded, if that device is playing). */
    fun assignDevice(output: String, choice: DeviceSound.Choice, onDone: () -> Unit = {}) = viewModelScope.launch {
        _assigning.value = output
        _assignError.value = null
        try {
            devices.assign(output, choice, currentOutput.value)
            onDone()
        } catch (e: Exception) {
            _assignError.value = describeConnectionError(e)
        } finally {
            _assigning.value = null
        }
    }

    fun clearAssignError() { _assignError.value = null }

    /** Takes a device out of the list, with whatever was chosen for it. */
    fun forgetDevice(output: String) = viewModelScope.launch {
        devices.forget(output)
        nori.outputs.forget(output)
    }

    /** What to say about the device that just connected; only while it is still the one playing. */
    val eqNotice: StateFlow<EqNotice?> = combine(devices.notice, currentOutput) { n, current ->
        // Only about the device still playing; the words are Say's.
        val text = when (n) {
            is DeviceSound.Offer -> dev.nori.music.app.ui.say.deviceNotice(true, n.entry.name)
            is DeviceSound.Applied -> dev.nori.music.app.ui.say.deviceNotice(false, n.curve)
            null -> null
        }
        if (n == null || text == null || n.output != current) null else EqNotice(text.first, text.second, n)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), null)

    /** The notice is on screen now, so it is not shown again. */
    fun eqNoticeShown(n: EqNotice) = devices.consume(n.source)

    /** "Apply" on an offer, "Undo" on a curve applied without asking. */
    fun eqNoticeAction(n: EqNotice) = viewModelScope.launch {
        when (val src = n.source) {
            is DeviceSound.Offer -> try {
                devices.accept(src)
            } catch (e: Exception) {
                _autoEq.update { it.copy(error = describeConnectionError(e)) }
            }
            is DeviceSound.Applied -> devices.undo(src)
        }
    }

    /** Saves the sound settings as they are now under [name]. */
    fun saveProfile(name: String) = viewModelScope.launch {
        val sound = prefs.value.sound()
        // Saving over a profile keeps the devices it is chosen for (the core's profile_save_sound).
        withContext(Dispatchers.IO) { runCatching { nori.core.profileSaveSound(name, sound) } }
        refreshProfiles()
    }

    fun applyProfile(p: SoundProfile) = soundFromJson(p.json)?.let { s -> update { it.withSound(s) } }
    fun deleteProfile(name: String) = viewModelScope.launch { runCatching { nori.core.profileDelete(name) }; refreshProfiles() }

    private val _autoEq = MutableStateFlow(AutoEqUi())
    val autoEq: StateFlow<AutoEqUi> = _autoEq

    init { viewModelScope.launch { val n = runCatching { nori.core.autoeqCount() }.getOrDefault(0u); _autoEq.update { it.counted(n) } } }

    private fun AutoEqUi.counted(n: UInt) = copy(count = n.toInt(), countWords = dev.nori.music.app.ui.say.autoeqCount(n.toInt()), searchWords = dev.nori.music.app.ui.say.autoeqSearch(n.toInt()))

    /**
     * Downloads the AutoEQ index (850 kB) now so searching is local afterwards; the core otherwise keeps
     * it by itself on Wi-Fi (Nori.keepAutoEqList).
     */
    fun downloadAutoEqIndex() = viewModelScope.launch {
        // Asked for by name, with a button: that is the consent, on any network. The lookups switch and
        // "Keep the AutoEQ list" are for what the app fetches on its own, not for a download the user started.
        _autoEq.update { it.copy(busy = true, error = null) }
        _autoEq.value = try {
            val n = withContext(Dispatchers.IO) { nori.client.autoeqUpdate(true, nori.http.metered) }
            AutoEqUi().counted(n ?: withContext(Dispatchers.IO) { nori.core.autoeqCount() })
        } catch (e: Exception) {
            AutoEqUi(error = describeConnectionError(e))
        }
    }

    fun searchAutoEq(query: String) = viewModelScope.launch {
        _autoEq.update { it.copy(query = query) }
        // Too short a query, the limit and the lines under each hit are the core's (autoeq_browse).
        val found = withContext(Dispatchers.IO) { runCatching { nori.core.autoeqBrowse(query) }.getOrNull() }
        _autoEq.update { if (it.query == query) it.copy(hits = found?.hits.orEmpty().map(AutoEqHit::of), tooShort = found?.tooShort ?: true) else it }
    }

    /**
     * What an output device might be in the AutoEQ database, best first: "Bluetooth: LE_WH-1000XM5" finds
     * "Sony WH-1000XM5". Empty when the index is not downloaded or the name says nothing (the speaker, a
     * generic "USB Audio").
     */
    suspend fun autoEqFor(output: String): List<AutoEqHit> = devices.curvesFor(output).map(AutoEqHit::of)

    /** Fetches one headphone's parametric preset and makes it the current curve. */
    fun applyAutoEq(entry: AutoEqEntry) = viewModelScope.launch {
        _autoEq.update { it.copy(busy = true, error = null) }
        try {
            // The parametric preset, or the graphic curve fitted with parametric filters when that is all
            // AutoEQ has (the core's autoeq_curve and parse_eq_preset); null when it has neither.
            val text = withContext(Dispatchers.IO) { nori.client.autoeqCurve(entry) }
            if (text == null) {
                // The core has taken it out of the list: the search and the count say so at once.
                val n = withContext(Dispatchers.IO) { runCatching { nori.core.autoeqCount() }.getOrDefault(0u) }
                _autoEq.update { it.counted(n).copy(busy = false, error = dev.nori.music.app.ui.say.autoeqNoCurve) }
                searchAutoEq(_autoEq.value.query)
                return@launch
            }
            withContext(Dispatchers.IO) { import(text) }
            _autoEq.update { it.copy(busy = false, applied = entry.name) }
        } catch (e: Exception) {
            _autoEq.update { it.copy(busy = false, error = describeConnectionError(e)) }
        }
    }

    private val _analysed = MutableStateFlow(0)
    /** How many tracks AutoMix has measured. */
    val analysed: StateFlow<Int> = _analysed

    fun refreshAnalysed() = viewModelScope.launch { _analysed.value = runCatching { nori.core.analysisCount() }.getOrDefault(0u).toInt() }

    fun clearAnalyses() = viewModelScope.launch { runCatching { nori.core.analysisClear() }; refreshAnalysed() }

    // ---- downloads ----

    /** Queues every song of the offline index for download; run [syncLibrary] first so the index is complete. */
    fun downloadLibrary() = nori.downloads.downloadLibrary()

    /** Fills the offline search index with the whole library. Optional: the app works without it. */
    fun syncLibrary() {
        if (_sync.value.running) return
        _sync.update { it.copy(running = true, error = null) }
        syncJob = viewModelScope.launch {
            try {
                nori.library.sync().collect { }
                _sync.value = SyncUi(indexed = nori.library.indexSize())
            } catch (e: Exception) {
                _sync.update { it.copy(running = false, error = e.said) }
            }
        }
    }
}
