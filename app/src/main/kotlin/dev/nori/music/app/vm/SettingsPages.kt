package dev.nori.music.app.vm

import android.content.res.Resources
import dev.nori.music.app.R
import dev.nori.music.ffi.library.TextIndex
import dev.nori.music.ffi.model.MusicFolder
import dev.nori.music.ffi.settings.BeatModel
import dev.nori.music.ffi.settings.EqLevel
import dev.nori.music.ffi.settings.SettingKind
import dev.nori.music.ffi.settings.SettingsState
import dev.nori.music.ffi.settings.settingSpecs
import dev.nori.music.playback.DacState
import dev.nori.music.ffi.settings.StoredPrefs
import kotlin.math.roundToInt
import dev.nori.music.settings.label
import dev.nori.music.settings.server
import dev.nori.music.update.Updates
import dev.nori.music.net.said

/*
 * The settings screen: its groups, pages, sections and rows, what each says and which are shown. The
 * settings themselves - their names, options as values, ranges and defaults, and what the core's rules
 * make of them (`SettingsState`) - are the core's model (nori-settings, settings_model.rs); everything a person
 * reads here is in res/values/strings.xml.
 */

/** A settings group: its own page, reached from the root of Settings. The icon is the screen's, by [id]. */
data class SettingsGroup(val id: String, val title: String, val summary: String)

/** One search result: the page it opens, the row it points at, and the line under its title. */
data class SettingsHit(val group: String, val key: String, val title: String, val detail: String)

/** One switch of a [SettingRow.Picks]: the setting it turns on and off, and what it says. */
data class Pick(val name: String, val label: String, val on: Boolean)

/** One choice in a list of options: what it says, and the value the core takes for it. */
data class SettingOption(val label: String, val value: String)

/**
 * One row of a settings page. [key] is what search lands on; `name` is the setting it changes, sent to
 * the core with the value picked; `enabled = false` is a setting the app is going to ignore right now.
 */
sealed interface SettingRow {
    data class Toggle(val key: String, val name: String, val title: String, val detail: String, val on: Boolean, val enabled: Boolean) : SettingRow
    /** [shown] is the chosen option's label (or the value itself when no option is it). */
    data class Choice(val key: String, val name: String, val title: String, val options: List<SettingOption>, val shown: String, val enabled: Boolean) : SettingRow
    /** A line of explanation among the rows. */
    data class Note(val text: String) : SettingRow
    /** A row that opens something ([action]), with a status at its end; [dimmed]: it is not reaching the sound now. */
    data class Link(val key: String, val title: String, val status: String, val dimmed: Boolean, val action: String, val divider: Boolean) : SettingRow
    /** A line of text and a button at its end; [error]: the line is a failure. */
    data class Action(val key: String, val title: String, val detail: String, val button: String, val enabled: Boolean, val error: Boolean, val action: String) : SettingRow
    data class Info(val key: String, val title: String, val detail: String) : SettingRow
    /** [level]: dragged through the core's in-place level edit instead of by [name]. */
    data class Slider(val name: String, val label: String, val value: Float, val min: Float, val max: Float, val centred: Boolean, val level: EqLevel?) : SettingRow
    /** Colour swatches, ARGB; the chosen one is drawn larger. */
    data class Palette(val name: String, val colours: List<Long>, val chosen: Long) : SettingRow
    data class Server(val id: String, val label: String, val detail: String, val active: Boolean) : SettingRow
    data class Button(val title: String, val action: String) : SettingRow
    /** One of a ranked list (the lyrics services), switched where it stands and picked up to move. */
    data class Ranked(val key: String, val name: String, val id: String, val title: String, val detail: String, val on: Boolean) : SettingRow
    /** The compressor's gain reduction, read from the player only while the page is on screen. */
    data class CompressionMeter(val key: String) : SettingRow
    /** One setting made of several switches, each a place it applies to ([Pick]), shown side by side. */
    data class Picks(val key: String, val title: String, val detail: String, val picks: List<Pick>) : SettingRow
    /** One setting with a few values, all on view as chips, the one it has lit: [Picks] with one choice. */
    data class ChipChoice(val key: String, val name: String, val title: String, val detail: String, val options: List<SettingOption>, val chosen: String) : SettingRow
    /** Text typed in (a service's key), hidden when [secret]. */
    data class Text(val key: String, val name: String, val title: String, val detail: String, val value: String, val secret: Boolean) : SettingRow
}

data class SettingsSection(val title: String, val rows: List<SettingRow>)

/** Empty [sections] for a page the screen draws itself (About, Licences). */
data class SettingsPage(val title: String, val sections: List<SettingsSection>)

/** A question a settings action asks before it is done. */
data class ActionAsk(val title: String, val text: String, val confirm: String)

/** What a page depends on besides the settings, from this phone. */
data class SettingsFacts(
    val dac: DacState = DacState(),
    /** Wallpaper colours and the blurred sleeve both need Android 12. */
    val wallpaperColours: Boolean = false,
    val coverBlur: Boolean = false,
    /** Songs AutoMix has measured. */
    val analysed: Int = 0,
    val sync: SyncUi = SyncUi(),
    val storage: StorageUi = StorageUi(),
    /** The active server's music folders. */
    val folders: List<MusicFolder> = emptyList(),
    /** Where the app's own update stands, and whether this build installs one. */
    val update: Updates.State = Updates.State.Idle,
    val installsUpdates: Boolean = false,
)

/** A row's key: its title, lowercased, with every run of anything but a-z and 0-9 made one '-'. Search lands on a row by it. */
fun settingKey(title: String): String {
    val key = StringBuilder(title.length)
    for (c in title.lowercase()) {
        if (c in 'a'..'z' || c in '0'..'9') key.append(c) else if (key.isEmpty() || key.last() != '-') key.append('-')
    }
    return key.toString().trim('-')
}

/** Each setting's options as values, from the core's model; asked once. */
private val OPTIONS: Map<String, List<String>> by lazy { settingSpecs().associate { it.name to it.options } }
private val ACCENTS: List<Long> by lazy { settingSpecs().first { it.kind == SettingKind.COLOUR }.options.map { it.toLong() } }

/** "850 B", "38 MB", "2.1 GB". */
fun formatBytes(res: Resources, bytes: Long): String = when {
    bytes < 1024 -> res.getString(R.string.settings_bytes, bytes.toInt())
    bytes < 1_048_576 -> res.getString(R.string.settings_kilobytes_size, "%.0f".format(bytes / 1024.0))
    bytes < 10_485_760 -> res.getString(R.string.settings_megabytes_size, "%.1f".format(bytes / 1_048_576.0))
    bytes < 1_073_741_824 -> res.getString(R.string.settings_megabytes_size, "%.0f".format(bytes / 1_048_576.0))
    else -> res.getString(R.string.settings_gigabytes_size, "%.1f".format(bytes / 1_073_741_824.0))
}

/** A decibel figure with its sign, one decimal: "+3.5", "-1.0", and "+0.0" for nothing at all (never "-0.0"). */
fun signedDb(db: Float): String = "%+.1f".format(if (db == 0f) 0f else db)

/** The groups the root of Settings lists: connect first, then what plays, how it sounds, how it looks. */
fun settingsGroups(res: Resources): List<SettingsGroup> = GROUPS.map { (id, title, summary) -> SettingsGroup(id, res.getString(title), res.getString(summary)) }

private val GROUPS = listOf(
    Triple("servers", R.string.settings_group_servers, R.string.settings_group_servers_summary),
    Triple("playing", R.string.settings_group_playing, R.string.settings_group_playing_summary),
    Triple("sound", R.string.settings_group_sound, R.string.settings_group_sound_summary),
    Triple("look", R.string.settings_group_look, R.string.settings_group_look_summary),
    Triple("lyrics", R.string.settings_group_lyrics, R.string.settings_group_lyrics_summary),
    Triple("library", R.string.settings_group_library, R.string.settings_group_library_summary),
    Triple("data", R.string.settings_group_data, R.string.settings_group_data_summary),
    Triple("about", R.string.settings_group_about, R.string.settings_group_about_summary),
)

/** A page's title: a group's, or one of the pages reached from inside another (`page:<id>`). */
private fun pageTitle(id: String): Int? = GROUPS.firstOrNull { it.first == id }?.second ?: when (id) {
    "licences" -> R.string.settings_page_licences
    "lyrics-sources" -> R.string.settings_page_lyrics_sources
    else -> null
}

/** Each lyrics service's name and what it is good at, by the id the core stores it under. */
private val SERVICES: Map<String, Pair<Int, Int>> = mapOf(
    "BINILYRICS" to (R.string.lyrics_service_binilyrics to R.string.lyrics_service_binilyrics_about),
    "BETTER_LYRICS" to (R.string.lyrics_service_better_lyrics to R.string.lyrics_service_better_lyrics_about),
    "PAXSENIX" to (R.string.lyrics_service_paxsenix to R.string.lyrics_service_paxsenix_about),
    "LYRICS_PLUS" to (R.string.lyrics_service_lyrics_plus to R.string.lyrics_service_lyrics_plus_about),
    "PORTATO" to (R.string.lyrics_service_portato to R.string.lyrics_service_portato_about),
    "PAXSENIX_MUSIXMATCH" to (R.string.lyrics_service_paxsenix_musixmatch to R.string.lyrics_service_paxsenix_musixmatch_about),
    "SIMPMUSIC" to (R.string.lyrics_service_simpmusic to R.string.lyrics_service_simpmusic_about),
    "UNISON" to (R.string.lyrics_service_unison to R.string.lyrics_service_unison_about),
    "NETEASE" to (R.string.lyrics_service_netease to R.string.lyrics_service_netease_about),
    "KUGOU" to (R.string.lyrics_service_kugou to R.string.lyrics_service_kugou_about),
    "LRCLIB" to (R.string.lyrics_service_lrclib to R.string.lyrics_service_lrclib_about),
    "PAXSENIX_SPOTIFY" to (R.string.lyrics_service_paxsenix_spotify to R.string.lyrics_service_paxsenix_spotify_about),
    "YOUTUBE_CAPTIONS" to (R.string.lyrics_service_youtube_captions to R.string.lyrics_service_youtube_captions_about),
    "MEGALOBIZ" to (R.string.lyrics_service_megalobiz to R.string.lyrics_service_megalobiz_about),
    "YOUTUBE_MUSIC" to (R.string.lyrics_service_youtube_music to R.string.lyrics_service_youtube_music_about),
    "GENIUS" to (R.string.lyrics_service_genius to R.string.lyrics_service_genius_about),
)

// ---- search ----

/** Rows only a build with the beat model's runtime has. */
private val BEAT_MODEL_ROWS = setOf(R.string.settings_better_beats, R.string.settings_beats_mobile_data, R.string.settings_download_beats)

/**
 * What the search can find: which page a row lives on, its title, and the words under it. A row is
 * matched by its own title, so the entry here and the row on the page cannot drift apart in wording -
 * only in existence. The hints are the rows' own descriptions, plus the jargon someone might type.
 */
private val INDEX: List<Triple<String, Int, Int>> = listOf(
    Triple("playing", R.string.settings_crossfade, R.string.settings_hint_crossfade),
    Triple("playing", R.string.settings_crossfade_curve, 0),
    Triple("playing", R.string.settings_automix, R.string.settings_hint_automix),
    Triple("playing", R.string.settings_longest_mix, 0),
    Triple("playing", R.string.settings_match_beat, R.string.settings_hint_match_beat),
    Triple("playing", R.string.settings_biggest_speed_change, 0),
    Triple("playing", R.string.settings_keep_pitch, R.string.settings_hint_keep_pitch),
    Triple("playing", R.string.settings_swap_bass, R.string.settings_hint_swap_bass),
    Triple("playing", R.string.settings_muffle, R.string.settings_hint_muffle),
    Triple("playing", R.string.settings_echo_out, R.string.settings_hint_echo_out),
    Triple("playing", R.string.settings_better_beats, R.string.settings_hint_better_beats),
    Triple("playing", R.string.settings_beats_mobile_data, R.string.settings_hint_beats_mobile_data),
    Triple("playing", R.string.settings_measured, R.string.settings_hint_measured),
    Triple("playing", R.string.settings_keep_albums, R.string.settings_hint_keep_albums),
    Triple("playing", R.string.settings_fade, R.string.settings_hint_fade),
    Triple("playing", R.string.settings_speed, 0),
    Triple("playing", R.string.settings_pitch, 0),
    Triple("playing", R.string.settings_skip_silence, R.string.settings_hint_skip_silence),
    Triple("playing", R.string.settings_previous, R.string.settings_hint_previous),
    Triple("playing", R.string.settings_skip_explicit, R.string.settings_hint_skip_explicit),
    Triple("playing", R.string.settings_auto_fill, R.string.settings_hint_auto_fill),
    Triple("playing", R.string.settings_auto_fill_kind, R.string.settings_hint_auto_fill_kind),
    Triple("playing", R.string.settings_auto_fill_basis, R.string.settings_hint_auto_fill_basis),
    Triple("playing", R.string.settings_auto_fill_remote, R.string.settings_hint_auto_fill_remote),
    Triple("playing", R.string.settings_skip_errors, R.string.settings_hint_skip_errors),
    Triple("playing", R.string.settings_bridge_offline, R.string.settings_hint_bridge_offline),
    Triple("sound", R.string.settings_equalizer, 0),
    Triple("sound", R.string.settings_autoeq_auto, R.string.settings_hint_autoeq_auto),
    Triple("sound", R.string.settings_autoeq_list, R.string.settings_hint_autoeq_list),
    Triple("sound", R.string.settings_per_device, R.string.settings_hint_per_device),
    Triple("sound", R.string.settings_sound_bypass, R.string.settings_sound_bypass_detail),
    Triple("sound", R.string.settings_system_effects, 0),
    Triple("sound", R.string.settings_bass_boost_title, 0),
    Triple("sound", R.string.settings_virtualizer_title, R.string.settings_virtualizer_hint),
    Triple("sound", R.string.settings_volume_boost_title, 0),
    Triple("sound", R.string.settings_compressor, R.string.settings_compressor_detail),
    Triple("sound", R.string.settings_expander, R.string.settings_expander_detail),
    Triple("sound", R.string.settings_loudness, R.string.settings_loudness_detail),
    Triple("sound", R.string.settings_replay_gain, R.string.settings_hint_replay_gain),
    Triple("sound", R.string.settings_loudness_target, R.string.settings_hint_loudness_target),
    Triple("sound", R.string.settings_gain_boost, R.string.settings_hint_gain_boost),
    Triple("sound", R.string.settings_untagged_gain, 0),
    Triple("sound", R.string.settings_gain_measured, R.string.settings_hint_gain_measured),
    Triple("sound", R.string.settings_hi_res, R.string.settings_hint_hi_res),
    Triple("sound", R.string.settings_max_rate, R.string.settings_hint_max_rate),
    Triple("sound", R.string.settings_bit_perfect, R.string.settings_hint_bit_perfect),
    Triple("sound", R.string.settings_offload, R.string.settings_hint_offload),
    Triple("look", R.string.settings_theme, R.string.settings_hint_theme),
    Triple("look", R.string.settings_amoled, R.string.settings_hint_amoled),
    Triple("look", R.string.settings_keep_colours, R.string.settings_hint_keep_colours),
    Triple("look", R.string.settings_wallpaper, R.string.settings_hint_wallpaper),
    Triple("look", R.string.settings_cover_colours, R.string.settings_hint_cover_colours),
    Triple("look", R.string.settings_blur, R.string.settings_hint_blur),
    Triple("look", R.string.settings_moving_covers, R.string.settings_hint_moving_covers),
    Triple("look", R.string.settings_moving_covers_mobile, R.string.settings_hint_moving_covers_mobile),
    Triple("look", R.string.settings_confirm_favourites, R.string.settings_hint_confirm_favourites),
    Triple("look", R.string.settings_ui_scale, 0),
    Triple("look", R.string.settings_less_movement, R.string.settings_hint_less_movement),
    Triple("look", R.string.settings_hide_status_bar, R.string.settings_hint_hide_status_bar),
    Triple("look", R.string.settings_keep_awake, R.string.settings_hint_keep_awake),
    Triple("look", R.string.settings_animate_anyway, R.string.settings_hint_animate_anyway),
    Triple("lyrics", R.string.settings_lyrics_sweep, R.string.settings_hint_lyrics_sweep),
    Triple("lyrics", R.string.settings_lyrics_size, 0),
    Triple("lyrics", R.string.settings_lyrics_translation, R.string.settings_hint_lyrics_translation),
    Triple("lyrics", R.string.settings_lyrics_screen_on, R.string.settings_hint_lyrics_screen_on),
    Triple("lyrics", R.string.settings_lyrics_online, R.string.settings_hint_lyrics_online),
    Triple("lyrics", R.string.settings_lyrics_words, R.string.settings_hint_lyrics_words),
    Triple("lyrics", R.string.settings_lyrics_sources, R.string.settings_hint_lyrics_sources),
    Triple("lyrics-sources", R.string.lyrics_service_binilyrics, R.string.settings_hint_service_apple),
    Triple("lyrics-sources", R.string.lyrics_service_better_lyrics, R.string.settings_hint_service_apple),
    Triple("lyrics-sources", R.string.lyrics_service_paxsenix, R.string.settings_hint_service_apple),
    Triple("lyrics-sources", R.string.lyrics_service_lyrics_plus, R.string.settings_hint_service_lyrics_plus),
    Triple("lyrics-sources", R.string.lyrics_service_portato, R.string.settings_hint_service_portato),
    Triple("lyrics-sources", R.string.lyrics_service_paxsenix_musixmatch, R.string.settings_hint_service_musixmatch),
    Triple("lyrics-sources", R.string.lyrics_service_simpmusic, R.string.settings_hint_service_simpmusic),
    Triple("lyrics-sources", R.string.lyrics_service_unison, R.string.settings_hint_service_unison),
    Triple("lyrics-sources", R.string.lyrics_service_netease, R.string.settings_hint_service_netease),
    Triple("lyrics-sources", R.string.lyrics_service_kugou, R.string.settings_hint_service_kugou),
    Triple("lyrics-sources", R.string.lyrics_service_lrclib, R.string.settings_hint_service_lrclib),
    Triple("lyrics-sources", R.string.lyrics_service_paxsenix_spotify, R.string.settings_hint_service_spotify),
    Triple("lyrics-sources", R.string.lyrics_service_youtube_captions, R.string.settings_hint_service_lines),
    Triple("lyrics-sources", R.string.lyrics_service_megalobiz, R.string.settings_hint_service_lines),
    Triple("lyrics-sources", R.string.lyrics_service_youtube_music, R.string.settings_hint_service_untimed),
    Triple("lyrics-sources", R.string.lyrics_service_genius, R.string.settings_hint_service_untimed),
    Triple("lyrics-sources", R.string.settings_paxsenix_key, R.string.settings_hint_paxsenix_key),
    Triple("lyrics-sources", R.string.settings_betterlyrics_key, R.string.settings_hint_betterlyrics_key),
    Triple("library", R.string.settings_tap_action, R.string.settings_hint_tap_action),
    Triple("library", R.string.settings_swipe_right, 0),
    Triple("library", R.string.settings_swipe_left, 0),
    Triple("library", R.string.settings_playlist_descriptions, R.string.settings_hint_playlist_descriptions),
    Triple("library", R.string.settings_hide_import_notes, R.string.settings_hint_hide_import_notes),
    Triple("library", R.string.settings_offline_search, R.string.settings_hint_offline_search),
    Triple("library", R.string.settings_search_delay, R.string.settings_hint_search_delay),
    Triple("library", R.string.settings_taste_model, R.string.settings_hint_taste_model),
    Triple("library", R.string.settings_scrobble, R.string.settings_hint_scrobble),
    Triple("library", R.string.settings_scrobble_after, 0),
    Triple("library", R.string.settings_lookups, R.string.settings_hint_lookups),
    Triple("data", R.string.settings_quality_wifi, 0),
    Triple("data", R.string.settings_quality_mobile, 0),
    Triple("data", R.string.settings_quality_download, 0),
    Triple("data", R.string.settings_parallel_downloads, R.string.settings_hint_parallel_downloads),
    Triple("data", R.string.settings_download_library, 0),
    Triple("data", R.string.settings_download_beats, R.string.settings_hint_download_beats),
    Triple("data", R.string.settings_analyse_downloads, R.string.settings_hint_analyse_downloads),
    Triple("data", R.string.settings_ahead_wifi, R.string.settings_hint_ahead_wifi),
    Triple("data", R.string.settings_ahead_mobile, 0),
    Triple("data", R.string.settings_covers_ahead, 0),
    Triple("data", R.string.settings_cache_size, R.string.settings_hint_cache_size),
    Triple("data", R.string.settings_stored, R.string.settings_hint_stored),
    Triple("data", R.string.settings_streamed, R.string.settings_hint_streamed),
    Triple("data", R.string.settings_covers, R.string.settings_hint_covers),
    Triple("data", R.string.settings_lyrics_cache, R.string.settings_hint_lyrics_cache),
    Triple("about", R.string.settings_page_licences, R.string.settings_hint_licences),
    Triple("about", R.string.settings_update_check_now, R.string.settings_hint_updates),
    Triple("about", R.string.settings_update_auto, R.string.settings_update_auto_detail),
    Triple("servers", R.string.settings_music_folder, 0),
    Triple("servers", R.string.settings_alt_bitrate, 0),
)

/**
 * The settings search, over [INDEX] in this [res]'s language: titles first, then anything whose words
 * mention the query, each in the order of the pages (the core's `TextIndex`, one call per query).
 * [beatModel]: whether this build offers better beat detection.
 */
class SettingsSearch(private val res: Resources, private val beatModel: Boolean) {
    private val entries = INDEX.filter { beatModel || it.second !in BEAT_MODEL_ROWS }
    private val text: TextIndex by lazy { TextIndex(entries.map { (_, t, h) -> listOf(res.getString(t), if (h == 0) "" else res.getString(h)) }) }

    fun find(query: String): List<SettingsHit> = text.ranked(query.trim()).map { i ->
        val (group, t, h) = entries[i.toInt()]
        val title = res.getString(t)
        val page = pageTitle(group)?.let(res::getString).orEmpty()
        SettingsHit(group, settingKey(title), title, if (h == 0) page else res.getString(R.string.settings_hit_detail, page, res.getString(h)))
    }
}

// ---- the pages ----

/** One group's page for these settings, facts and the core's state; null for a group there is not. */
fun settingsPage(res: Resources, id: String, p: StoredPrefs, f: SettingsFacts, s: SettingsState): SettingsPage? {
    val title = pageTitle(id) ?: return null
    val b = PageBuilder(res, p, f, s)
    val sections = when (id) {
        "playing" -> b.playing()
        "sound" -> b.sound()
        "look" -> b.look()
        "lyrics" -> b.lyrics()
        "lyrics-sources" -> b.lyricsSources()
        "library" -> b.library()
        "data" -> b.data()
        "servers" -> b.servers()
        "about" -> b.about()
        else -> emptyList()
    }
    return SettingsPage(res.getString(title), sections)
}

/** One of a DAC's modes, or what is playing: "44.1 kHz / 24 bit" (the rate in the phone's number style). */
fun dacModeWords(res: Resources, rate: UInt, bits: UInt): String =
    res.getString(R.string.settings_dac_mode, dev.nori.music.text.Fmt.kiloHertz(rate.toInt()), bits.toInt())

/** What the AudioTrack was opened with: "96.0 kHz / 32 bit, offloaded to the audio chip". */
fun dacTrackWords(res: Resources, t: dev.nori.music.playback.DacTrack): String {
    val mode = dacModeWords(res, t.rate.toUInt(), t.bits.toUInt())
    return if (t.offloaded) res.getString(R.string.settings_dac_offloaded, mode) else mode
}

/** Why a DAC plays nothing bit-perfect, in words. */
fun dacBlockWords(res: Resources, b: dev.nori.music.ffi.model.DacBlock): String = when (b) {
    is dev.nori.music.ffi.model.DacBlock.NoModeAtRate -> res.getString(R.string.settings_dac_no_mode, dev.nori.music.text.Fmt.kiloHertz(b.rate.toInt()))
    is dev.nori.music.ffi.model.DacBlock.NeedsExclusive -> {
        val depths = listOf(1 to 16, 2 to 24, 4 to 32).filter { (bit, _) -> b.depths.toInt() and bit != 0 }
            .joinToString(res.getString(R.string.settings_dac_or)) { (_, bits) -> res.getString(R.string.settings_dac_bits, bits) }
        res.getString(R.string.settings_dac_needs_exclusive, depths, dev.nori.music.text.Fmt.kiloHertz(b.rate.toInt()))
    }
    dev.nori.music.ffi.model.DacBlock.PlatformTooOld -> res.getString(R.string.settings_dac_old_android)
    dev.nori.music.ffi.model.DacBlock.Refused -> res.getString(R.string.settings_dac_refused)
}

/** Whether clearing [action] asks first, and what it says; null for one that is done at once. */
fun settingsActionAsks(res: Resources, action: String, f: SettingsFacts): ActionAsk? = when (action) {
    // What is gone is fetched again from somebody else's services, song by song.
    "clear-lyrics" -> ActionAsk(
        res.getString(R.string.settings_clear_lyrics_title),
        res.getString(R.string.settings_clear_lyrics_text, formatBytes(res, f.storage.lyricsBytes)),
        res.getString(R.string.settings_clear),
    )
    else -> null
}

/**
 * What an update's state says, for About's row and the banner: the version found, the download's progress,
 * what is missing or what went wrong. [current] is this build's version.
 */
fun updateWords(res: Resources, s: Updates.State, current: String, installs: Boolean): String = when (s) {
    Updates.State.Idle -> res.getString(R.string.update_version, current)
    Updates.State.Checking -> res.getString(R.string.update_checking)
    is Updates.State.UpToDate -> res.getString(R.string.update_latest, current)
    is Updates.State.Available ->
        if (installs) res.getString(R.string.update_available, s.update.version, formatBytes(res, s.update.apkBytes.toLong()))
        else res.getString(R.string.update_available_elsewhere, s.update.version)
    is Updates.State.NoApk -> res.getString(R.string.update_no_apk, s.version)
    is Updates.State.CheckFailed -> res.getString(R.string.update_check_failed, s.error.said.orEmpty())
    is Updates.State.Downloading -> res.getString(R.string.update_downloading, formatBytes(res, s.done), formatBytes(res, s.total))
    is Updates.State.Installing -> res.getString(R.string.update_installing)
    is Updates.State.NeedsPermission -> res.getString(R.string.update_needs_permission)
    is Updates.State.Failed -> when (val why = s.why) {
        is Updates.Failure.Download -> res.getString(R.string.update_failed_download, why.error.said.orEmpty())
        is Updates.Failure.Size -> res.getString(R.string.update_failed_size, formatBytes(res, why.got), formatBytes(res, why.expected))
        Updates.Failure.NotThisApp -> res.getString(R.string.update_failed_not_nori)
        // Android's own message is for the log (it says INSTALL_FAILED_... in capitals): each kind is worded here.
        is Updates.Failure.Install -> res.getString(
            when (why.status) {
                android.content.pm.PackageInstaller.STATUS_FAILURE_CONFLICT -> R.string.update_failed_signature
                android.content.pm.PackageInstaller.STATUS_FAILURE_STORAGE -> R.string.update_failed_storage
                android.content.pm.PackageInstaller.STATUS_FAILURE_INCOMPATIBLE -> R.string.update_failed_incompatible
                android.content.pm.PackageInstaller.STATUS_FAILURE_BLOCKED -> R.string.update_failed_blocked
                else -> R.string.update_failed_install
            },
        )
    }
}

/** The word on the update's main button in state [s] (see SettingsViewModel.updateNow); null for none. */
fun updateButton(res: Resources, s: Updates.State, installs: Boolean): String? = when (s) {
    is Updates.State.Available -> res.getString(if (installs) R.string.update_now else R.string.update_view)
    is Updates.State.NoApk -> res.getString(R.string.update_view)
    // Another try cannot get past a copy signed with another key: the release's page can.
    is Updates.State.Failed -> res.getString(if ((s.why as? Updates.Failure.Install)?.conflict == true) R.string.update_view else R.string.update_try_again)
    is Updates.State.NeedsPermission -> res.getString(R.string.update_allow)
    is Updates.State.Downloading -> res.getString(R.string.update_cancel)
    Updates.State.Checking, is Updates.State.Installing -> null
    else -> res.getString(R.string.update_check)
}

private class PageBuilder(val res: Resources, val p: StoredPrefs, val f: SettingsFacts, val s: SettingsState) {
    fun str(id: Int) = res.getString(id)
    fun str(id: Int, vararg args: Any) = res.getString(id, *args)
    fun value(name: String) = s.values[name].orEmpty()
    fun on(name: String) = value(name) == "true"

    fun toggle(name: String, title: Int, detail: String, enabled: Boolean = true, on: Boolean = on(name)): SettingRow.Toggle {
        val t = str(title)
        return SettingRow.Toggle(settingKey(t), name, t, detail, on, enabled)
    }

    fun toggle(name: String, title: Int, detail: Int, enabled: Boolean = true) = toggle(name, title, str(detail), enabled)

    /** The core's options for [name] as chips, each worded by [labels] in the options' order. */
    fun chips(name: String, title: Int, detail: Int, vararg labels: Int): SettingRow.ChipChoice {
        val t = str(title)
        val options = OPTIONS[name].orEmpty().mapIndexed { i, v -> SettingOption(labels.getOrNull(i)?.let(::str) ?: v, v) }
        return SettingRow.ChipChoice(settingKey(t), name, t, str(detail), options, value(name))
    }

    fun picks(title: Int, detail: Int, vararg picks: Pair<String, Int>): SettingRow.Picks {
        val t = str(title)
        return SettingRow.Picks(settingKey(t), t, str(detail), picks.map { (name, label) -> Pick(name, str(label), on(name)) })
    }

    /** A list of the core's options for [name], each worded by [label]; [fallback] words a value no option is. */
    fun choice(name: String, title: Int, enabled: Boolean = true, fallback: (String) -> String = { it }, label: (String) -> String): SettingRow.Choice {
        val options = OPTIONS[name].orEmpty().map { SettingOption(label(it), it) }
        return choiceOf(name, title, options, enabled, fallback)
    }

    fun choiceOf(name: String, title: Int, options: List<SettingOption>, enabled: Boolean = true, fallback: (String) -> String = { it }): SettingRow.Choice {
        val v = value(name)
        val t = str(title)
        return SettingRow.Choice(settingKey(t), name, t, options, options.firstOrNull { it.value == v }?.label ?: fallback(v), enabled)
    }

    /** An enum setting: its options by name, worded by [labels] in the same order. */
    fun named(name: String, title: Int, vararg labels: Int): SettingRow.Choice {
        val names = OPTIONS[name].orEmpty()
        return choice(name, title) { v -> names.indexOf(v).takeIf { it in labels.indices }?.let { str(labels[it]) } ?: v }
    }

    fun action(title: Int, detail: String, button: String, enabled: Boolean, act: String): SettingRow.Action {
        val t = str(title)
        return SettingRow.Action(settingKey(t), t, detail, button, enabled, false, act)
    }

    fun link(title: Int, status: String, dimmed: Boolean, act: String, divider: Boolean): SettingRow.Link {
        val t = str(title)
        return SettingRow.Link(settingKey(t), t, status, dimmed, act, divider)
    }

    fun section(title: Int, rows: List<SettingRow>) = SettingsSection(str(title), rows)

    // How values read.
    fun offOr(v: String, words: (String) -> String) = if (v == "0") str(R.string.settings_off) else words(v)
    fun seconds(v: String) = str(R.string.settings_seconds, v)
    fun millis(v: String) = str(R.string.settings_millis, v)
    fun percent(v: String) = str(R.string.settings_percent, v)
    /** A float setting's value as Kotlin writes a float ("1.1"), for one no option is. */
    fun float(v: String) = v.toFloatOrNull()?.toString() ?: v
    /** −3 dB with a true minus sign. */
    fun minus(v: String) = v.replace('-', '−')

    fun playing(): List<SettingsSection> {
        val live = !s.untouched
        val between = mutableListOf<SettingRow>()
        if (s.untouched) between += SettingRow.Note(str(R.string.settings_held_by_dac))
        // AutoMix plans its own transitions, so the plain crossfade gives way to it.
        if (!p.autoMix) between += choice("crossfadeSec", R.string.settings_crossfade, live) { offOr(it, ::seconds) }
        // How a plain crossfade sounds: its curve, and how long each side takes within it.
        if (!p.autoMix && p.crossfadeSec > 0) {
            between += named("crossfadeCurve", R.string.settings_crossfade_curve, R.string.settings_crossfade_equal_power, R.string.settings_crossfade_linear, R.string.settings_crossfade_s_curve)
            fun part(v: String) = if (v == "0") str(R.string.settings_crossfade_whole) else seconds(v)
            between += choice("crossfadeInSec", R.string.settings_crossfade_in, live, label = ::part)
            between += choice("crossfadeOutSec", R.string.settings_crossfade_out, live, label = ::part)
        }
        between += toggle("autoMix", R.string.settings_automix, R.string.settings_automix_detail, live)
        if (p.autoMix) {
            between += choice("autoMixMaxS", R.string.settings_longest_mix, live, label = ::seconds)
            between += toggle("autoMixBeatMatch", R.string.settings_match_beat, R.string.settings_match_beat_detail, live)
            if (p.autoMixBeatMatch) {
                between += choice("autoMixMaxTempoPct", R.string.settings_biggest_speed_change, live, ::float, ::percent)
                between += toggle("autoMixKeepPitch", R.string.settings_keep_pitch, R.string.settings_keep_pitch_detail, live)
            }
            between += toggle("autoMixBassSwap", R.string.settings_swap_bass, R.string.settings_swap_bass_detail, live)
            between += toggle("autoMixFilters", R.string.settings_muffle, R.string.settings_muffle_detail, live)
            between += toggle("autoMixEchoOut", R.string.settings_echo_out, R.string.settings_echo_out_detail, live)
            // Only in a build that carries the beat model's runtime.
            val model = s.beatModel
            if (model !is BeatModel.Unavailable) {
                val mb = s.beatModelMb.toInt()
                val better = p.autoMixBetterBeats
                val detail = if (!better) str(R.string.settings_better_beats_off, mb) else when (model) {
                    is BeatModel.Failed -> str(R.string.settings_better_beats_failed, str(when (model.why) {
                        dev.nori.music.ffi.automix.BeatFailure.NETWORK -> R.string.settings_better_beats_network
                        dev.nori.music.ffi.automix.BeatFailure.WRONG_FILE -> R.string.settings_better_beats_wrong_file
                        dev.nori.music.ffi.automix.BeatFailure.STORAGE -> R.string.settings_better_beats_storage
                    }))
                    BeatModel.WaitingForWifi -> str(R.string.settings_better_beats_waiting, mb)
                    BeatModel.Downloading -> str(R.string.settings_better_beats_downloading, mb)
                    BeatModel.Ready -> str(R.string.settings_better_beats_ready)
                    else -> str(R.string.settings_better_beats_absent, mb)
                }
                between += toggle("autoMixBetterBeats", R.string.settings_better_beats, detail, live)
                if (better && model != BeatModel.Ready) {
                    between += toggle("autoMixBeatsMobileData", R.string.settings_beats_mobile_data, R.string.settings_beats_mobile_data_detail, live)
                }
            }
            between += action(
                R.string.settings_measured, res.getQuantityString(R.plurals.settings_measured_detail, f.analysed, f.analysed),
                str(R.string.settings_measure_again), f.analysed > 0, "measure-again",
            )
        }
        between += toggle("crossfadeKeepAlbums", R.string.settings_keep_albums, R.string.settings_keep_albums_detail, live)
        between += choice("fadeMs", R.string.settings_fade) { v -> offOr(v) { if (it.toInt() % 1000 == 0) seconds((it.toInt() / 1000).toString()) else millis(it) } }

        val controls = listOf(
            toggle("previousAlwaysSkips", R.string.settings_previous, R.string.settings_previous_detail),
            choice("speed", R.string.settings_speed, fallback = ::float) { if (it == "1") str(R.string.settings_normal) else str(R.string.settings_times, it) },
            choice("pitch", R.string.settings_pitch, fallback = ::float) { v ->
                val pct = ((v.toFloat() - 1f) * 100f).roundToInt()
                when {
                    pct == 0 -> str(R.string.settings_normal)
                    pct < 0 -> percent("−${-pct}")
                    else -> percent("+$pct")
                }
            },
            toggle("skipSilence", R.string.settings_skip_silence, str(if (s.untouched) R.string.settings_skip_silence_held else R.string.settings_skip_silence_detail), live),
        )

        val queue = mutableListOf<SettingRow>(
            toggle("skipExplicit", R.string.settings_skip_explicit, R.string.settings_skip_explicit_detail),
            toggle("autoFill", R.string.settings_auto_fill, R.string.settings_auto_fill_detail),
        )
        // What arrives and what it is chosen by are two separate questions, so they are two rows.
        if (p.autoFill) {
            queue += named("autoFillKind", R.string.settings_auto_fill_kind, R.string.settings_auto_fill_kind_songs, R.string.settings_auto_fill_kind_albums)
            queue += named(
                "autoFillBasis", R.string.settings_auto_fill_basis, R.string.settings_auto_fill_basis_similar, R.string.settings_auto_fill_basis_artist,
                R.string.settings_auto_fill_basis_genre, R.string.settings_auto_fill_basis_era,
            )
            queue += toggle("autoFillRemote", R.string.settings_auto_fill_remote, R.string.settings_auto_fill_remote_detail)
        }
        val wrong = listOf(
            toggle("skipOnError", R.string.settings_skip_errors, R.string.settings_skip_errors_detail),
            toggle("bridgeOffline", R.string.settings_bridge_offline, R.string.settings_bridge_offline_detail),
        )
        return listOf(
            section(R.string.settings_section_between, between), section(R.string.settings_section_controls, controls),
            section(R.string.settings_section_queue, queue), section(R.string.settings_section_wrong, wrong),
        )
    }

    fun sound(): List<SettingsSection> {
        // The chain is taken out of the path by the same rule the transitions are, so the row says so
        // rather than reading "On" over sound it is not touching.
        val bypassed = s.untouched
        val status = str(if (bypassed) R.string.settings_equalizer_off_now else if (s.soundChainOn) R.string.settings_on else R.string.settings_off)
        val eq = listOf(
            link(R.string.settings_equalizer, status, bypassed, "equalizer", divider = true),
            toggle("autoEqAuto", R.string.settings_autoeq_auto, R.string.settings_autoeq_auto_detail),
            // Under the switch for looking things up at all (Library), and turns it on with it.
            toggle("autoEqDownload", R.string.settings_autoeq_list, R.string.settings_autoeq_list_detail),
            toggle("profilePerOutput", R.string.settings_per_device, R.string.settings_per_device_detail),
            toggle("soundBypass", R.string.settings_sound_bypass, R.string.settings_sound_bypass_detail),
            link(R.string.settings_system_effects, "", false, "system-effects", divider = false),
        )
        val volume = mutableListOf<SettingRow>(
            named("replayGain", R.string.settings_replay_gain, R.string.settings_off, R.string.settings_replay_gain_track, R.string.settings_replay_gain_album, R.string.settings_replay_gain_auto),
        )
        if (p.replayGain != dev.nori.music.ffi.model.GainMode.OFF) {
            val r = dev.nori.music.settings.EQ.eqRanges.replayGainPreamp
            volume += SettingRow.Slider("preampDb", str(R.string.settings_overall_level, signedDb(p.preampDb)), p.preampDb, r.min, r.max, true, EqLevel.REPLAY_GAIN_PREAMP)
            volume += choice("loudnessTarget", R.string.settings_loudness_target, fallback = { str(R.string.settings_lufs, minus(float(it))) }) {
                val words = when (it) {
                    "-18" -> R.string.settings_lufs_replay_gain
                    "-14", "-16" -> R.string.settings_lufs_streaming
                    "-23" -> R.string.settings_lufs_broadcast
                    else -> R.string.settings_lufs
                }
                str(words, minus(it))
            }
            // Past 0 dB a song needs its samples turned up, with the limiter behind them: it keeps off the audio chip.
            volume += choice("gainBoostDb", R.string.settings_gain_boost, fallback = ::float) { offOr(it) { v -> str(R.string.settings_gain_boost_upto, "+$v") } }
            volume += choice("untaggedGainDb", R.string.settings_untagged_gain, fallback = ::float) { str(R.string.settings_db, minus(it)) }
            volume += toggle("gainMeasured", R.string.settings_gain_measured, R.string.settings_gain_measured_detail)
        }
        val d = f.dac
        val output = mutableListOf<SettingRow>(
            toggle("hiRes", R.string.settings_hi_res, R.string.settings_hi_res_detail),
            named("maxRate", R.string.settings_max_rate, R.string.settings_max_rate_auto, R.string.settings_max_rate_48, R.string.settings_max_rate_96, R.string.settings_max_rate_192),
            toggle("bitPerfect", R.string.settings_bit_perfect, bitPerfectWords(d)),
        )
        // What is actually going out, rather than what was asked for.
        dacDetail(d)?.let { output += SettingRow.Note(it) }
        val offload = str(if (d.device != null) R.string.settings_offload_usb else if (s.offloadPaused) R.string.settings_offload_paused else R.string.settings_offload_detail)
        output += toggle("offload", R.string.settings_offload, offload)
        return listOf(
            section(R.string.settings_section_equalizer, eq), section(R.string.settings_section_effects, effects()),
            section(R.string.settings_section_volume, volume), section(R.string.settings_section_output, output),
        )
    }

    /** Bass boost, virtualizer, volume boost and the compressor: sliders edited in place in the core. */
    fun effects(): List<SettingRow> {
        fun boost(db: Float) = if (db <= 0f) str(R.string.settings_off) else str(R.string.settings_db, signedDb(db))
        fun one(v: Float) = "%.1f".format(v)
        val rows = mutableListOf<SettingRow>(
            SettingRow.Slider("bassBoostDb", str(R.string.settings_bass_boost, boost(p.bassBoostDb)), p.bassBoostDb, 0f, 12f, false, EqLevel.BASS_BOOST),
            SettingRow.Slider(
                "virtualizer",
                str(R.string.settings_virtualizer, if (p.virtualizer <= 0f) str(R.string.settings_off) else percent((p.virtualizer * 100f).roundToInt().toString())),
                p.virtualizer, 0f, 1f, false, EqLevel.VIRTUALIZER,
            ),
            SettingRow.Slider("volumeBoostDb", str(R.string.settings_volume_boost, boost(p.volumeBoostDb)), p.volumeBoostDb, 0f, 12f, false, EqLevel.VOLUME_BOOST),
            SettingRow.Note(str(R.string.settings_boost_note)),
            toggle("compressor", R.string.settings_compressor, R.string.settings_compressor_detail),
        )
        if (p.compressor) {
            rows += SettingRow.CompressionMeter("compression-meter")
            rows += choice("compressorPreset", R.string.settings_compressor_preset, fallback = { str(R.string.settings_compressor_custom) }) {
                str(when (it) { "GENTLE" -> R.string.settings_compressor_gentle; "STRONG" -> R.string.settings_compressor_strong; else -> R.string.settings_compressor_balanced })
            }
            // The sliders span the useful part of each control (a ratio of 3 was a sliver at the start of
            // 1 to 20); the core takes the wider values a profile or the terminal may hold.
            rows += SettingRow.Slider("compThresholdDb", str(R.string.settings_comp_threshold, minus(one(p.compThresholdDb))), p.compThresholdDb, -60f, 0f, false, EqLevel.COMP_THRESHOLD)
            rows += SettingRow.Slider("compRatio", str(R.string.settings_comp_ratio, one(p.compRatio)), p.compRatio.coerceIn(1f, 10f), 1f, 10f, false, EqLevel.COMP_RATIO)
            rows += SettingRow.Slider("compAttackMs", str(R.string.settings_comp_attack, one(p.compAttackMs)), p.compAttackMs.coerceIn(0.1f, 100f), 0.1f, 100f, false, EqLevel.COMP_ATTACK)
            rows += SettingRow.Slider("compReleaseMs", str(R.string.settings_comp_release, p.compReleaseMs.roundToInt().toString()), p.compReleaseMs.coerceIn(10f, 1000f), 10f, 1000f, false, EqLevel.COMP_RELEASE)
            rows += SettingRow.Slider("compMakeupDb", str(R.string.settings_comp_makeup, signedDb(p.compMakeupDb)), p.compMakeupDb.coerceIn(0f, 12f), 0f, 12f, false, EqLevel.COMP_MAKEUP)
            rows += SettingRow.Slider("compKneeDb", str(R.string.settings_comp_knee, one(p.compKneeDb)), p.compKneeDb.coerceIn(0f, 12f), 0f, 12f, false, EqLevel.COMP_KNEE)
        }
        // Loudness compensation that follows the volume (ISO 226): off unless asked for.
        rows += toggle("loudness", R.string.settings_loudness, R.string.settings_loudness_detail)
        if (p.loudness) rows += choice("loudnessRefPhon", R.string.settings_loudness_reference) { str(R.string.settings_phon, it) }
        // The downward expander, a noise gate at a high ratio: off unless asked for.
        rows += toggle("expander", R.string.settings_expander, R.string.settings_expander_detail)
        if (p.expander) {
            rows += SettingRow.Slider("expThresholdDb", str(R.string.settings_exp_threshold, minus(one(p.expThresholdDb))), p.expThresholdDb, -90f, -10f, false, EqLevel.EXP_THRESHOLD)
            rows += SettingRow.Slider("expRatio", str(R.string.settings_exp_ratio, one(p.expRatio)), p.expRatio, 1f, 20f, false, EqLevel.EXP_RATIO)
            rows += SettingRow.Slider("expAttackMs", str(R.string.settings_exp_attack, one(p.expAttackMs)), p.expAttackMs.coerceIn(0.1f, 50f), 0.1f, 50f, false, EqLevel.EXP_ATTACK)
            rows += SettingRow.Slider("expReleaseMs", str(R.string.settings_exp_release, p.expReleaseMs.roundToInt().toString()), p.expReleaseMs.coerceIn(10f, 1000f), 10f, 1000f, false, EqLevel.EXP_RELEASE)
        }
        return rows
    }

    /** The bit-perfect switch's second line: what the DAC is doing, why it cannot, or what the switch is for. */
    fun bitPerfectWords(d: DacState): String = when {
        d.bitPerfect -> str(R.string.settings_bit_perfect_on, d.device ?: "null", (d.sampleRate / 1000.0).toString(), d.bits)
        d.blockedBy != null -> str(R.string.settings_bit_perfect_blocked, d.device ?: str(R.string.settings_bit_perfect_usb_dac), dacBlockWords(res, d.blockedBy!!))
        d.device != null && d.supported -> str(R.string.settings_bit_perfect_connected, d.device!!)
        d.device != null -> str(R.string.settings_bit_perfect_unsupported, d.device!!)
        else -> str(R.string.settings_bit_perfect_detail)
    }

    /** "Offers … · playing … · output …"; null when there is nothing to say. */
    fun dacDetail(d: DacState): String? {
        val parts = listOfNotNull(
            d.modes.takeIf { it.isNotEmpty() }?.let { m -> str(R.string.settings_dac_offers, m.joinToString(", ") { dacModeWords(res, it.rate, it.bits) }) },
            d.playing?.let { str(R.string.settings_dac_playing, dacModeWords(res, it.rate, it.bits)) },
            d.track?.let { str(R.string.settings_dac_output, dacTrackWords(res, it)) },
        )
        return if (parts.isEmpty()) null else parts.joinToString(str(R.string.settings_dac_separator))
    }

    fun look(): List<SettingsSection> {
        val theme = mutableListOf<SettingRow>(
            named("theme", R.string.settings_theme, R.string.settings_theme_system, R.string.settings_theme_light, R.string.settings_theme_dark),
            toggle("amoled", R.string.settings_amoled, R.string.settings_amoled_detail),
        )
        if (p.amoled) theme += picks(
            R.string.settings_keep_colours, R.string.settings_keep_colours_detail,
            "artistColours" to R.string.settings_keep_colours_artist, "albumColours" to R.string.settings_keep_colours_album,
            "playerColours" to R.string.settings_keep_colours_player,
        )
        if (f.wallpaperColours) theme += toggle("dynamicColor", R.string.settings_wallpaper, R.string.settings_wallpaper_detail)
        if (!p.dynamicColor || !f.wallpaperColours) theme += SettingRow.Palette("accent", ACCENTS, p.accent)
        val cover = mutableListOf<SettingRow>(toggle("coverColors", R.string.settings_cover_colours, R.string.settings_cover_colours_detail))
        if (f.coverBlur) cover += toggle("softSleeve", R.string.settings_blur, R.string.settings_blur_detail)
        // Under the switch for looking things up at all (Library), and turns it on with it.
        val moving = on("motionArtwork")
        cover += toggle("motionArtwork", R.string.settings_moving_covers, str(if (moving && p.reduceMotion) R.string.settings_moving_covers_still else R.string.settings_moving_covers_detail))
        if (moving) cover += toggle("motionArtworkMobile", R.string.settings_moving_covers_mobile, R.string.settings_moving_covers_mobile_detail)
        val messages = listOf(toggle("favouriteNotice", R.string.settings_confirm_favourites, R.string.settings_confirm_favourites_detail))
        val size = mutableListOf<SettingRow>(
            choice("uiScale", R.string.settings_ui_scale, fallback = ::float) { v ->
                when (v) {
                    "0" -> str(R.string.settings_ui_scale_auto)
                    "0.9" -> str(R.string.settings_ui_scale_smaller)
                    "1" -> str(R.string.settings_ui_scale_system)
                    "1.1" -> str(R.string.settings_ui_scale_larger)
                    else -> float(v)
                }
            },
            toggle("reduceMotion", R.string.settings_less_movement, R.string.settings_less_movement_detail),
        )
        if (!p.reduceMotion) size += toggle("ignoreSystemMotion", R.string.settings_animate_anyway, R.string.settings_animate_anyway_detail)
        val screen = listOf(
            chips(
                "hideStatusBar", R.string.settings_hide_status_bar, R.string.settings_hide_status_bar_detail,
                R.string.settings_hide_status_bar_never, R.string.settings_hide_status_bar_sideways,
                R.string.settings_hide_status_bar_upright, R.string.settings_hide_status_bar_always,
            ),
            chips(
                "keepAwake", R.string.settings_keep_awake, R.string.settings_keep_awake_detail,
                R.string.settings_keep_awake_never, R.string.settings_keep_awake_sideways, R.string.settings_keep_awake_charging,
                R.string.settings_keep_awake_sideways_charging, R.string.settings_keep_awake_always,
            ),
        )
        return listOf(
            section(R.string.settings_section_theme, theme), section(R.string.settings_section_cover, cover),
            section(R.string.settings_section_screen, screen),
            section(R.string.settings_section_messages, messages), section(R.string.settings_section_size, size),
        )
    }

    fun lyrics(): List<SettingsSection> {
        val display = listOf(
            toggle("lyricsSweep", R.string.settings_lyrics_sweep, R.string.settings_lyrics_sweep_detail),
            choice("lyricsSize", R.string.settings_lyrics_size) { v ->
                when (v) {
                    "0" -> str(R.string.settings_lyrics_size_small)
                    "1" -> str(R.string.settings_lyrics_size_medium)
                    "2" -> str(R.string.settings_lyrics_size_large)
                    else -> v
                }
            },
            toggle("lyricsTranslation", R.string.settings_lyrics_translation, R.string.settings_lyrics_translation_detail),
            toggle("lyricsKeepScreenOn", R.string.settings_lyrics_screen_on, R.string.settings_lyrics_screen_on_detail),
        )
        // The switch for looking things up at all lives in Library, but somebody looking for lyrics looks
        // here, so the lyrics half of it is offered here too and turns the other one on with it.
        val online = on("lyricsOnline")
        val sources = mutableListOf<SettingRow>(toggle("lyricsOnline", R.string.settings_lyrics_online, R.string.settings_lyrics_online_detail))
        if (online) {
            sources += toggle("lyricsPreferWords", R.string.settings_lyrics_words, R.string.settings_lyrics_words_detail)
            val n = s.lyricsSources.count { it.on }
            val status = if (n == 0) str(R.string.settings_lyrics_sources_none) else res.getQuantityString(R.plurals.settings_lyrics_sources_on, n, n)
            sources += link(R.string.settings_lyrics_sources, status, n == 0, "page:lyrics-sources", divider = false)
        }
        return listOf(section(R.string.settings_section_display, display), section(R.string.settings_section_sources, sources))
    }

    /**
     * Every lyrics service in one list, in the order they are asked: each switched on or off where it
     * stands, and picked up and moved. A switch never moves a service, so the list reads the same after it.
     */
    fun lyricsSources(): List<SettingsSection> {
        val ranked = s.lyricsSources.mapNotNull { src ->
            val (title, about) = SERVICES[src.id] ?: return@mapNotNull null
            val t = str(title)
            SettingRow.Ranked(settingKey(t), "lyricsService:${src.id}", src.id, t, str(about), src.on)
        }.toMutableList<SettingRow>()
        ranked += SettingRow.Note(str(if (on("lyricsOnline")) R.string.settings_lyrics_sources_note else R.string.settings_lyrics_sources_note_off))
        val keys = listOf(
            text("paxSenixKey", R.string.settings_paxsenix_key, R.string.settings_paxsenix_key_detail),
            text("betterLyricsKey", R.string.settings_betterlyrics_key, R.string.settings_betterlyrics_key_detail),
        )
        return listOf(section(R.string.settings_section_asked_order, ranked), section(R.string.settings_section_keys, keys))
    }

    fun text(name: String, title: Int, detail: Int): SettingRow.Text {
        val t = str(title)
        return SettingRow.Text(settingKey(t), name, t, str(detail), value(name), secret = true)
    }

    fun library(): List<SettingsSection> {
        val swipes = intArrayOf(R.string.settings_swipe_none, R.string.settings_swipe_queue, R.string.settings_swipe_play_next, R.string.settings_swipe_favourite, R.string.settings_swipe_download)
        val gestures = listOf(
            named("tapAction", R.string.settings_tap_action, R.string.settings_tap_play_list, R.string.settings_tap_play_one, R.string.settings_tap_queue, R.string.settings_tap_play_next),
            named("swipeRight", R.string.settings_swipe_right, *swipes),
            named("swipeLeft", R.string.settings_swipe_left, *swipes),
        )
        val sync = f.sync
        val indexed = sync.indexed
        val counts = str(
            R.string.settings_offline_search_detail,
            res.getQuantityString(R.plurals.settings_songs, indexed.songs.toInt(), indexed.songs.toInt()),
            res.getQuantityString(R.plurals.settings_albums, indexed.albums.toInt(), indexed.albums.toInt()),
            res.getQuantityString(R.plurals.settings_artists, indexed.artists.toInt(), indexed.artists.toInt()),
        )
        val t = str(R.string.settings_offline_search)
        val search = listOf(
            SettingRow.Action(settingKey(t), t, sync.error ?: counts, str(if (sync.running) R.string.settings_updating else R.string.settings_update), !sync.running, sync.error != null, "sync-library"),
            choice("liveSearchDelayMs", R.string.settings_search_delay, label = ::millis),
        )
        val playlists = mutableListOf<SettingRow>(toggle("playlistDescriptions", R.string.settings_playlist_descriptions, R.string.settings_playlist_descriptions_detail))
        if (p.playlistDescriptions) playlists += toggle("hideImportNotes", R.string.settings_hide_import_notes, R.string.settings_hide_import_notes_detail)
        val history = mutableListOf<SettingRow>(
            toggle("tasteModel", R.string.settings_taste_model, R.string.settings_taste_model_detail),
            toggle("scrobble", R.string.settings_scrobble, R.string.settings_scrobble_detail),
        )
        if (p.scrobble) history += choice("scrobblePercent", R.string.settings_scrobble_after) { if (it == "100") str(R.string.settings_scrobble_whole) else percent(it) }
        val online = listOf(toggle("thirdPartyLookups", R.string.settings_lookups, R.string.settings_lookups_detail))
        return listOf(
            section(R.string.settings_section_lists, gestures), section(R.string.settings_section_playlists, playlists),
            section(R.string.settings_section_search, search), section(R.string.settings_section_history, history), section(R.string.settings_section_online, online),
        )
    }

    /** A stream quality: "Original", "MP3 320", "Opus 192". */
    fun quality(name: String, title: Int) = choice(name, title, fallback = { it.replace(":", "") }) { v ->
        val (rate, format) = v.split(':', limit = 2).let { it[0] to it.getOrElse(1) { "" } }
        when (format) {
            "" -> str(R.string.settings_quality_original)
            "mp3" -> str(R.string.settings_quality_mp3, rate.toInt())
            "opus" -> str(R.string.settings_quality_opus, rate.toInt())
            else -> "$rate $format"
        }
    }

    fun songsAhead(v: String) = if (v == "1") str(R.string.settings_ahead_next) else res.getQuantityString(R.plurals.settings_ahead_songs, v.toInt(), v.toInt())

    fun data(): List<SettingsSection> {
        val streaming = listOf(quality("wifi", R.string.settings_quality_wifi), quality("mobile", R.string.settings_quality_mobile))
        val downloads = mutableListOf<SettingRow>(
            quality("download", R.string.settings_quality_download),
            choice("parallelDownloads", R.string.settings_parallel_downloads) { it },
            action(R.string.settings_download_library, str(R.string.settings_download_library_detail), str(R.string.settings_download), f.sync.indexed.songs > 0u, "download-library"),
        )
        // The beat model over what is downloaded: only while it is on (a build with it, AutoMix and Better beat detection on).
        if (s.beatModel !is BeatModel.Unavailable && p.autoMix && p.autoMixBetterBeats) {
            downloads += named("downloadBeats", R.string.settings_download_beats, R.string.settings_download_beats_ask, R.string.settings_download_beats_always, R.string.settings_download_beats_never)
        }
        downloads += action(
            R.string.settings_analyse_downloads, str(R.string.settings_analyse_downloads_detail), str(R.string.settings_analyse),
            f.storage.downloadSongs > 0, "analyse-downloads",
        )
        val ahead = listOf(
            choice("precacheWifi", R.string.settings_ahead_wifi, label = ::songsAhead),
            choice("precacheMobile", R.string.settings_ahead_mobile, label = ::songsAhead),
            choice("coversAhead", R.string.settings_covers_ahead) { offOr(it) { v -> v } },
        )
        // What lives on this device, and a way to throw the throwaway parts out. Downloads are the permanent
        // copy and are removed where they are listed; the streamed music and the covers rebuild themselves.
        val st = f.storage
        val bytes = { n: Long -> formatBytes(res, n) }
        val clearing = str(if (st.busy) R.string.settings_clearing else R.string.settings_clear)
        val stored = str(R.string.settings_stored)
        val storage = listOf(
            choice("cacheMb", R.string.settings_cache_size) { v ->
                val mb = v.toInt()
                if (mb >= 1024 && mb % 1024 == 0) str(R.string.settings_gigabytes, mb / 1024) else str(R.string.settings_megabytes, mb)
            },
            SettingRow.Info(
                settingKey(stored), stored,
                str(
                    R.string.settings_stored_detail, bytes(st.streamBytes), bytes(st.coverBytes), bytes(st.lyricsBytes),
                    res.getQuantityString(R.plurals.settings_stored_downloads, st.downloadSongs, st.downloadSongs, bytes(st.downloadBytes)), bytes(st.indexBytes),
                ),
            ),
            action(R.string.settings_streamed, str(R.string.settings_streamed_detail), clearing, !st.busy && st.streamBytes > 0, "clear-stream"),
            action(R.string.settings_covers, str(R.string.settings_covers_detail), clearing, !st.busy && st.coverBytes > 0, "clear-covers"),
            action(R.string.settings_lyrics_cache, str(R.string.settings_lyrics_cache_detail, bytes(st.lyricsBytes)), clearing, !st.busy && st.lyricsBytes > 0, "clear-lyrics"),
            action(R.string.settings_downloads, str(R.string.settings_downloads_detail), str(R.string.settings_show), true, "downloads"),
        )
        return listOf(
            section(R.string.settings_section_streaming, streaming), section(R.string.settings_section_downloads, downloads),
            section(R.string.settings_section_ahead, ahead), section(R.string.settings_section_storage, storage),
        )
    }

    /** The line under a saved server: who signs in, whether it is in use, and what is special about it. */
    fun serverDetail(user: String, active: Boolean, wifiOnly: Boolean, second: Boolean): String {
        val parts = mutableListOf(user.ifEmpty { str(R.string.settings_server_api_key) }, str(if (active) R.string.settings_server_in_use else R.string.settings_server_not_in_use))
        if (wifiOnly) parts += str(R.string.settings_server_wifi_only)
        if (second) parts += str(R.string.settings_server_second_address)
        return parts.joinToString(str(R.string.settings_server_separator))
    }

    /**
     * About's own rows under the build's facts (AboutContent draws those): the update check with where it
     * stands and its one button, and the daily check's switch.
     */
    fun about(): List<SettingsSection> {
        val u = f.update
        val button = updateButton(res, u, f.installsUpdates)
        val act = when (u) {
            is Updates.State.Downloading -> "update-cancel"
            is Updates.State.Available, is Updates.State.Failed, is Updates.State.NeedsPermission, is Updates.State.NoApk -> "update-go"
            else -> "update-check"
        }
        val t = str(R.string.settings_update_check_now)
        val failed = u is Updates.State.Failed || u is Updates.State.CheckFailed
        val check = SettingRow.Action(
            settingKey(t), t, updateWords(res, u, dev.nori.music.app.BuildConfig.VERSION_NAME, f.installsUpdates),
            button ?: str(R.string.update_check), button != null, failed, act,
        )
        return listOf(SettingsSection(str(R.string.settings_updates), listOf(check, toggle("updateCheck", R.string.settings_update_auto, R.string.settings_update_auto_detail))))
    }

    fun servers(): List<SettingsSection> {
        val accounts = p.servers.map { sv ->
            val active = sv.id == p.activeServerId
            SettingRow.Server(sv.id, sv.label, serverDetail(sv.user, active, sv.wifiOnly, sv.altUrl.isNotBlank()), active)
        }.toMutableList<SettingRow>()
        accounts += SettingRow.Button(str(R.string.settings_add_server), "add-server")
        val out = mutableListOf(section(R.string.settings_section_accounts, accounts))
        val server = p.server
        val folderRow = f.folders.size > 1
        val altRow = server != null && server.altUrl.isNotBlank()
        if (folderRow || altRow) {
            val rows = mutableListOf<SettingRow>()
            if (folderRow) {
                val options = listOf(SettingOption(str(R.string.settings_music_folder_all), "")) + f.folders.map { SettingOption(it.name, it.id) }
                rows += choiceOf("musicFolder", R.string.settings_music_folder, options)
            }
            if (altRow) rows += choice("altMaxBitRate", R.string.settings_alt_bitrate) { if (it == "0") str(R.string.settings_alt_bitrate_none) else str(R.string.settings_kbps, it.toInt()) }
            out += section(R.string.settings_section_this_server, rows)
        }
        return out
    }
}
