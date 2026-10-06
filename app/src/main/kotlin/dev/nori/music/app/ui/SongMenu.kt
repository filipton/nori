package dev.nori.music.app.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.PlaylistAdd
import androidx.compose.material.icons.automirrored.filled.PlaylistPlay
import androidx.compose.material.icons.automirrored.filled.QueueMusic
import androidx.compose.material.icons.filled.Album
import androidx.compose.material.icons.filled.AutoAwesome
import androidx.compose.material.icons.filled.Bedtime
import androidx.compose.material.icons.filled.Block
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.IosShare
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.LibraryAdd
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.Person
import androidx.compose.material.icons.filled.Radio
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.ffi.model.Artist
import dev.nori.music.ffi.model.Playlist
import dev.nori.music.ffi.model.Song

/**
 * One line of a sheet. The icon is what keeps a column of these from reading as a wall of text, so
 * every row of the song menu carries one; the sleep timer's own numbers are a list of values rather
 * than a list of actions, and they are left plain.
 */
@Composable
private fun Item(text: String, icon: ImageVector? = null, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = Space.gutter, vertical = 15.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        if (icon != null) Icon(icon, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(
            text, Modifier.weight(1f).padding(start = if (icon != null) 16.dp else 0.dp),
            style = MaterialTheme.typography.bodyLarge, maxLines = 1,
            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SongMenu(
    /** The song the menu is for; null closes it (it slides away showing the last one). */
    song: Song?,
    actions: ActionsViewModel,
    onDismiss: () -> Unit,
    /**
     * Set when the player's own ⋯ opened this. The sleep timer is a property of the evening, not of
     * the song, so it has no business on a row's menu in a list - but the player needs it somewhere
     * now that the bottom of that screen belongs to the output switcher, the way Apple's does.
     */
    player: dev.nori.music.app.vm.PlayerViewModel? = null,
    /** Set for a jam guest, whose menu only asks the host for the song. */
    request: ((Song) -> Unit)? = null,
) {
    val nav = LocalNav.current
    // Which of the menu's stages is up, fresh each time the menu opens. The sheet slides down as the
    // sleep choices, the details or the playlist picker come up over it, and each of those leaves with
    // the menu when it is done.
    val open = song != null
    var picking by remember(open) { mutableStateOf(false) }
    var details by remember(open) { mutableStateOf(false) }
    var sleeping by remember(open) { mutableStateOf(false) }
    NoriSheet(player?.takeIf { open && sleeping }, onDismiss) { p -> SleepChoices(p, onDismiss) }
    NoriDialog(song?.takeIf { details }, onDismiss) { s -> TrackInfo(s, onDismiss) }
    NoriDialog(song?.takeIf { picking }, onDismiss) { s -> PlaylistPicker(listOf(s), actions, onDismiss) }

    // A sheet with a half-open stage swallows the first back gesture to collapse itself, which reads
    // as the menu refusing to close. There is only ever one stage here, so back always dismisses.
    NoriSheet(song?.takeIf { !picking && !details && !sleeping }, onDismiss) { song ->
        val downloads by actions.downloads.collectAsState()
        var more by remember { mutableStateOf(false) }
        Column(Modifier.verticalScroll(rememberScrollState()).navigationBarsPadding()) {
            // The track leads the sheet, the way the row it came from looked.
            Row(Modifier.fillMaxWidth().padding(horizontal = Space.gutter, vertical = 4.dp), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Cover(actions.cover(song.coverArt, CoverSize.ROW), 52.dp, radius = 8.dp)
                Column(Modifier.weight(1f).padding(start = 12.dp)) {
                    Text(song.title, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis)
                    Text(remember(song) { say.songLine(song.artist, song.album) }, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis)
                    if (song.suffix.isNotEmpty()) Caption(remember(song) { say.songFormat(song) })
                }
            }
            Hairline(startIndent = Space.gutter)
            // What someone opened this menu to do: queue it, keep it, or leave for where it came
            // from. Everything one reaches for perhaps once a month waits under "More" below.
            // The heart comes first because it is the one thing here that is about the song rather than
            // about the queue, and it reads this session's marks rather than the snapshot the song was
            // handed over with: one starred a moment ago from the bar above still says so, without
            // waiting for the server's list to come round again. The marks are collected here rather
            // than taken from LocalStarMarks because the sheet is put up outside the provider, at the
            // top of the app, so that it outlives the row or the player that asked for it.
            val marks by actions.starMarks.collectAsState()
            val starred = marks.effectiveStar(dev.nori.music.data.StarKind.SONG, song.id, song.starred)
            val download = when (song.id) {
                in downloads.doneIds -> dev.nori.music.ffi.library.SongDownload.DONE
                in downloads.pendingIds -> dev.nori.music.ffi.library.SongDownload.PENDING
                else -> dev.nori.music.ffi.library.SongDownload.NONE
            }
            // What the menu offers and in what order is the core's (`menus::song_menu`); the words are this
            // app's (`Say.songAction`), made once when the menu opens. This draws each line with its icon.
            val items = remember(song, starred, download, player != null) { dev.nori.music.ffi.library.songMenu(song, starred, download, player != null, request != null) }
            val labels = remember(items) { items.map { say.songAction(it.action) } }
            @Composable fun line(i: dev.nori.music.ffi.library.SongMenuItem, label: String) = when (val a = i.action) {
                is dev.nori.music.ffi.library.SongAction.Favourite -> Item(label, if (a.on) Icons.Filled.FavoriteBorder else Icons.Filled.Favorite) { actions.star(song, a.on); onDismiss() }
                dev.nori.music.ffi.library.SongAction.PlayNext -> Item(label, Icons.AutoMirrored.Filled.PlaylistPlay) { actions.playNext(listOf(song)); onDismiss() }
                dev.nori.music.ffi.library.SongAction.AddToQueue -> Item(label, Icons.AutoMirrored.Filled.QueueMusic) { actions.enqueue(listOf(song)); onDismiss() }
                dev.nori.music.ffi.library.SongAction.AddToPlaylist -> Item(label, Icons.AutoMirrored.Filled.PlaylistAdd) { picking = true }
                dev.nori.music.ffi.library.SongAction.RemoveDownload -> Item(label, Icons.Filled.Delete) { actions.removeDownloads(listOf(song.id)); onDismiss() }
                dev.nori.music.ffi.library.SongAction.StopDownload -> Item(label, Icons.Filled.Close) { actions.cancelDownloads(listOf(song)); onDismiss() }
                dev.nori.music.ffi.library.SongAction.Download -> Item(label, Icons.Filled.Download) { actions.download(listOf(song)); onDismiss() }
                is dev.nori.music.ffi.library.SongAction.GoToAlbum -> Item(label, Icons.Filled.Album) { nav.album(a.id); onDismiss() }
                // The song's own cover stands in for a lone artist's until their page has one.
                is dev.nori.music.ffi.library.SongAction.GoToArtist -> Item(label, Icons.Filled.Person) {
                    nav.artist(a.id, Artist(a.id, a.name, song.coverArt.takeIf { song.artists.size <= 1 }, null, 0u, false, false)); onDismiss()
                }
                dev.nori.music.ffi.library.SongAction.AddToLibrary -> Item(label, Icons.Filled.LibraryAdd) { actions.addToLibrary(song.id, isAlbum = false); onDismiss() }
                dev.nori.music.ffi.library.SongAction.SleepTimer -> Item(label, Icons.Filled.Bedtime) { sleeping = true }
                dev.nori.music.ffi.library.SongAction.StartRadio -> Item(label, Icons.Filled.Radio) { actions.startRadio(song); onDismiss() }
                dev.nori.music.ffi.library.SongAction.InstantMix -> Item(label, Icons.Filled.AutoAwesome) { actions.instantMix(song); onDismiss() }
                dev.nori.music.ffi.library.SongAction.ExcludeFromMixes -> Item(label, Icons.Filled.Block) { actions.excludeFromMixes(song); onDismiss() }
                dev.nori.music.ffi.library.SongAction.Share -> Item(label, Icons.Filled.IosShare) { actions.share(song.id); onDismiss() }
                dev.nori.music.ffi.library.SongAction.Details -> Item(label, Icons.Filled.Info) { details = true }
                dev.nori.music.ffi.library.SongAction.Request -> Item(label, Icons.AutoMirrored.Filled.QueueMusic) { request?.invoke(song); onDismiss() }
            }
            items.forEachIndexed { n, it -> if (!it.more) line(it, labels[n]) }
            Hairline(startIndent = Space.gutter)
            val turn by animateFloatAsState(if (more) 180f else 0f, label = "more")
            Row(
                Modifier.fillMaxWidth().clickable { more = !more }.padding(horizontal = Space.gutter, vertical = 15.dp),
                verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
            ) {
                Icon(Icons.Filled.MoreHoriz, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                Text(say.more, Modifier.weight(1f).padding(start = 16.dp), style = MaterialTheme.typography.bodyLarge)
                // The chevron turns over rather than swapping for its opposite, so the row says which
                // way it is about to move before it moves.
                Icon(
                    Icons.Filled.KeyboardArrowDown, null,
                    Modifier.size(22.dp).graphicsLayer { rotationZ = turn }, tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            AnimatedVisibility(more, enter = fadeIn() + expandVertically(), exit = fadeOut() + shrinkVertically()) {
                Column { items.forEachIndexed { n, it -> if (it.more) line(it, labels[n]) } }
            }
        }
    }
}

/** The sleep choices on their own sheet, so the song's menu is not buried under eleven of them. */
@Composable
private fun SleepChoices(player: dev.nori.music.app.vm.PlayerViewModel, onDone: () -> Unit) {
    val state by player.state.collectAsState()
    Column(Modifier.verticalScroll(rememberScrollState()).navigationBarsPadding()) {
        SectionTitle(say.sleepTimer)
        val running = state.sleepAt > 0 || state.sleepAtEndOfTrack
        // The choices are the core's (`menus::sleep_choices`); "Off" is all zeros.
        val choices = remember(running) { dev.nori.music.ffi.library.sleepChoices(running) }
        val words = remember(choices) { choices.map(say::sleepChoice) }
        choices.forEachIndexed { n, c -> Item(words[n]) { player.sleep(c.minutes.toInt(), c.endOfTrack, c.songs.toInt()); onDone() } }
    }
}

@Composable
fun PlaylistPicker(songs: List<Song>, actions: ActionsViewModel, onDone: () -> Unit) {
    var playlists by remember { mutableStateOf<List<Playlist>?>(null) }
    var name by remember { mutableStateOf("") }
    LaunchedEffect(Unit) { playlists = actions.playlists() }
    AlertCard(
        title = { Text(say.addToPlaylist) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                OutlinedTextField(name, { name = it }, label = { Text(say.newPlaylist) }, singleLine = true, modifier = Modifier.fillMaxWidth())
                playlists?.forEach { p -> Text(p.name, Modifier.fillMaxWidth().clickable { actions.addToPlaylist(p, songs); onDone() }.padding(vertical = 12.dp)) }
            }
        },
        confirmButton = { TextButton({ actions.addToNewPlaylist(name.trim(), songs); onDone() }, enabled = name.isNotBlank()) { Text(say.create) } },
        dismissButton = { TextButton(onDone) { Text(say.cancel) } },
    )
}

/** Everything the server said about one file. */
@Composable
fun TrackInfo(song: Song, onDone: () -> Unit) {
    // Which rows there are, in what order and how each reads is nori-core's (`fmt::track_info`).
    val rows = remember(song) { say.trackInfo(song) }
    AlertCard(
        title = { Text(say.details) },
        text = { Column(Modifier.verticalScroll(rememberScrollState())) { rows.forEach { (label, value) -> Text(label, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant); Text(value, Modifier.padding(bottom = 8.dp)) } } },
        confirmButton = { TextButton(onDone) { Text(say.close) } },
    )
}

/** Shown instead of nothing while songs are selected: the batch actions. */
@Composable
fun SelectionBar(actions: ActionsViewModel) {
    val selection by actions.selection.collectAsState()
    // Above the early return, so the picker can leave after the selection it cleared is gone.
    var picking by remember { mutableStateOf(false) }
    NoriDialog(selection.takeIf { picking && it.isNotEmpty() }, { picking = false }) { songs ->
        PlaylistPicker(songs, actions) { picking = false; actions.clearSelection() }
    }
    LaunchedEffect(selection.isEmpty()) { if (selection.isEmpty()) picking = false }
    // Rises in and out on the app's clock (NoriBar), never on one frame; while it leaves it shows the
    // selection it had.
    NoriBar(selection.takeIf { it.isNotEmpty() }) { songs ->
        androidx.compose.material3.Surface(tonalElevation = 6.dp) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                SelectedCount(songs.size, Modifier.weight(1f).padding(start = 8.dp))
                BarAction(say.play) { actions.play(songs); actions.clearSelection() }
                BarAction(say.next) { actions.playNext(songs); actions.clearSelection() }
                BarAction(say.queue) { actions.enqueue(songs); actions.clearSelection() }
                BarAction(say.playlist) { picking = true }
                BarAction(say.get) { actions.download(songs); actions.clearSelection() }
                IconButton(actions::clearSelection) { Icon(androidx.compose.material.icons.Icons.Filled.Close, say.clearSelection) }
            }
        }
    }
}

/**
 * One of the selection bar's text buttons: Material's, with its padding and its 58 dp least width taken
 * in (a set least width stands in for the default), so the five and the ✕ leave the count room on a
 * phone 360 dp wide.
 */
@Composable
private fun BarAction(label: String, onClick: () -> Unit) =
    TextButton(onClick, Modifier.widthIn(min = 40.dp), contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 8.dp)) {
        Text(label, maxLines = 1, softWrap = false)
    }

/**
 * "3 selected" on one line, or the count alone where that does not fit: the bar's buttons come first,
 * and the words were squeezed into a column one letter wide.
 */
@Composable
private fun SelectedCount(n: Int, modifier: Modifier) {
    val full = remember(n) { say.selected(n) }
    val short = remember(n) { say.selectedShort(n) }
    val style = androidx.compose.material3.LocalTextStyle.current
    val measurer = androidx.compose.ui.text.rememberTextMeasurer()
    androidx.compose.foundation.layout.BoxWithConstraints(modifier) {
        val room = constraints.maxWidth
        val fits = remember(full, style, room) { measurer.measure(full, style, maxLines = 1, softWrap = false).size.width <= room }
        Text(if (fits) full else short, maxLines = 1, softWrap = false, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis)
    }
}
