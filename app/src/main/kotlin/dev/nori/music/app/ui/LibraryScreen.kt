package dev.nori.music.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.size
import androidx.compose.ui.draw.clip
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.DropdownMenu
import androidx.compose.material.icons.filled.SwapVert
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.itemsIndexed
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Radio
import androidx.compose.material.icons.filled.FileDownload
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Downloading
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.PrimaryScrollableTabRow
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.AlbumsViewModel
import dev.nori.music.app.vm.ArtistsViewModel
import dev.nori.music.app.vm.DecadesViewModel
import dev.nori.music.app.vm.FoldersViewModel
import dev.nori.music.app.vm.SongSort
import dev.nori.music.app.vm.SongsViewModel
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.snapshotFlow
import kotlinx.coroutines.launch
import dev.nori.music.app.vm.GenresViewModel
import dev.nori.music.app.vm.PlaylistsViewModel
import dev.nori.music.app.vm.RadioViewModel
import dev.nori.music.app.vm.StarredViewModel

/**
 * One line that says how the list is ordered and opens the alternatives, instead of a second row of
 * chips under the first: two stacked chip strips made this screen read as a toolbar.
 */
@Composable
private fun <T> SortMenu(options: List<Pair<T, String>>, value: T, onChange: (T) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box(Modifier.padding(start = Space.gutter - 4.dp, top = 2.dp)) {
        Row(
            Modifier.clip(PillShape).clickable { open = true }.padding(horizontal = 6.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Filled.SwapVert, null, Modifier.size(17.dp), tint = MaterialTheme.colorScheme.primary)
            Text(
                options.firstOrNull { it.first == value }?.second ?: say.sort,
                Modifier.padding(start = 4.dp), style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        DropdownMenu(open, { open = false }) {
            options.forEach { (v, label) -> DropdownMenuItem({ Text(label) }, { onChange(v); open = false }) }
        }
    }
}

/** The library's sections, in the order their pills run (the core's `library_sections`). */
private val sections: List<dev.nori.music.ffi.library.LibrarySection> by lazy { dev.nori.music.ffi.library.librarySections() }

@Composable
fun LibraryScreen(actions: ActionsViewModel) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    Column {
        LargeTitle(say.library)
        // A scrolling row of pills, not a tab strip with an underline: twelve sections in a Material tab
        // row reads as a toolbar, and the library is a place to browse.
        LazyRow(Modifier.bleedsToEdges(), contentPadding = edgePadding(horizontal = Space.gutter, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            itemsIndexed(sections, key = { _, s -> s.ordinal }) { i, s -> Chip(say.librarySection(s), tab == i) { tab = i } }
        }
        // Only the visible section is composed, so only its view model loads anything.
        when (sections[tab]) {
            dev.nori.music.ffi.library.LibrarySection.ALBUMS -> Albums()
            dev.nori.music.ffi.library.LibrarySection.ARTISTS -> Artists()
            dev.nori.music.ffi.library.LibrarySection.SONGS -> SongsScreen(actions, null)
            dev.nori.music.ffi.library.LibrarySection.PLAYLISTS -> Playlists(actions)
            dev.nori.music.ffi.library.LibrarySection.SMART -> SmartList()
            dev.nori.music.ffi.library.LibrarySection.HISTORY -> HistoryList(actions)
            dev.nori.music.ffi.library.LibrarySection.FAVOURITES -> Favourites(actions)
            dev.nori.music.ffi.library.LibrarySection.GENRES -> Genres()
            dev.nori.music.ffi.library.LibrarySection.DECADES -> Decades()
            dev.nori.music.ffi.library.LibrarySection.FOLDERS -> Folders()
            dev.nori.music.ffi.library.LibrarySection.RADIO -> Radio()
            dev.nori.music.ffi.library.LibrarySection.DOWNLOADS -> Downloads(actions)
        }
    }
}

@Composable
private fun Albums(vm: AlbumsViewModel = viewModel()) {
    val albums by vm.albums.collectAsStateWithLifecycle()
    val sort by vm.sort.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    Column {
        SortMenu(remember { dev.nori.music.ffi.library.albumSorts().map { it to say.albumSort(it) } }, sort, vm::setSort)
        // Ask for the covers just past the fold while the ones on screen are still arriving. The first row
        // is read by a snapshot observer, not here: read in composition, it recomposed the whole grid on
        // every row scrolled.
        val list = rememberLazyGridState()
        val context = androidx.compose.ui.platform.LocalContext.current
        androidx.compose.runtime.LaunchedEffect(list, albums) {
            androidx.compose.runtime.snapshotFlow { list.firstVisibleItemIndex }.collect { first ->
                prefetchCovers(context, albums.drop(first + 6).take(12).map { vm.cover(it.coverArt, CoverSize.CARD) })
            }
        }
        // Two columns, counted rather than measured: an adaptive grid gave a wide phone a third column,
        // and a cover a third of the way across the screen is too small to recognise a sleeve by, which
        // is the only reason to show covers instead of a list of names.
        // On its side the same two would each be half a wide screen; there the cards keep about the size
        // they have upright and the row takes as many as fit.
        // On its side the cards keep about the size they have upright (a little larger than Home's shelves,
        // which is the point of a grid of covers) and the row takes as many as fit.
        val wide = LocalWide.current
        LazyVerticalGrid(if (wide) GridCells.Adaptive(170.dp) else GridCells.Fixed(2), state = list, contentPadding = PaddingValues(start = Space.gutter, end = Space.gutter, top = Space.gutter, bottom = Space.gutter + LocalChromeInset.current), horizontalArrangement = Arrangement.spacedBy(12.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            itemsIndexed(albums, key = { _, a -> a.id }, contentType = { _, _ -> "album" }) { i, a ->
                if (i >= albums.size - 12) vm.loadMore()
                AlbumCard(a, vm.cover(a.coverArt, CoverSize.CARD), 132.dp, { nav.album(a.id, a) }, Modifier.fillMaxWidth(), fill = true)
            }
        }
    }
}

@Composable
private fun Artists(vm: ArtistsViewModel = viewModel()) {
    val load by vm.artists.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    var filter by remember { mutableStateOf("") }
    val list = rememberLazyListState()
    val scope = rememberCoroutineScope()
    LoadBox(load) { all ->
        // What the filter keeps and the first row of each initial, for the index on the right edge
        // (nori-core's `TextIndex`): the names go over once per list, each keystroke sends the filter.
        val index = remember(all) { dev.nori.music.ffi.library.TextIndex(all.map { listOf(it.name) }) }
        val view = remember(index, filter) { index.view(filter) }
        val artists = remember(view) { view.rows.map { all[it.toInt()] } }
        val letters = view.letters
        Column {
            SearchField(filter, { filter = it }, say.filterArtists, Modifier.padding(horizontal = Space.gutter, vertical = 4.dp))
            Row {
                LazyColumn(Modifier.weight(1f), state = list, contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
                    items(artists, key = { it.id }, contentType = { "artist" }) { a ->
                        Column {
                            Row(Modifier.fillMaxWidth().clickable { nav.artist(a.id, a) }.padding(horizontal = Space.gutter, vertical = 7.dp), verticalAlignment = Alignment.CenterVertically) {
                                Cover(vm.cover(a.coverArt, CoverSize.ROW), 48.dp, radius = 24.dp)
                                Column(Modifier.padding(start = 12.dp)) {
                                    Text(a.name, style = MaterialTheme.typography.bodyLarge)
                                    Text(remember(a.albumCount) { say.albums(a.albumCount.toInt()) }, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                }
                            }
                            Hairline(startIndent = Space.gutter + 60.dp)
                        }
                    }
                }
                if (view.showLetters) Column(Modifier.padding(end = 2.dp).verticalScroll(rememberScrollState())) {
                    letters.forEach { l -> Text(l.letter, Modifier.clickable { scope.launch { list.scrollToItem(l.row.toInt()) } }.padding(horizontal = 8.dp, vertical = 1.dp), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary) }
                }
            }
        }
    }
}

/** Every indexed song, sorted; with [decade] set, only that decade. Reads the offline index, never the network. */
@Composable
fun SongsScreen(actions: ActionsViewModel, decade: Int?, vm: SongsViewModel = viewModel(key = "songs-$decade")) {
    LaunchedEffect(decade) { vm.setYears(decade?.let { dev.nori.music.ffi.library.decadeYears(it.toUInt()) }?.let { it.from.toInt()..it.to.toInt() }) }
    val songs by vm.songs.collectAsStateWithLifecycle()
    val sort by vm.sort.collectAsStateWithLifecycle()
    val starred by vm.starredOnly.collectAsStateWithLifecycle()
    val done = actions.downloads.collectAsState().value.doneIds
    val selection by actions.selection.collectAsStateWithLifecycle()
    val selected = remember(selection) { selection.mapTo(HashSet()) { it.id } }
    val player: dev.nori.music.app.vm.PlayerViewModel = viewModel()
    val playing by player.currentId.collectAsStateWithLifecycle()
    val menu = LocalSongMenu.current
    val list = rememberLazyListState()
    // Ask for the next page a screenful before the end, from a snapshot observer rather than from inside item composition.
    LaunchedEffect(list, songs.size) { snapshotFlow { (list.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0) >= songs.size - 40 }.collect { if (it) vm.loadMore() } }
    Column {
        if (decade != null) SectionTitle(remember(decade) { say.decade(decade) })
        LazyRow(Modifier.bleedsToEdges(), contentPadding = edgePadding(horizontal = Space.gutter), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            item { Chip(say.starredFavourites, starred) { vm.setStarredOnly(!starred) } }
            items(SongSort.entries) { s -> Chip(say.songSort(s.name), sort == s) { vm.setSort(s) } }
        }
        if (songs.isEmpty()) EmptyNote(Note.NO_INDEX)
        LazyColumn(state = list, contentPadding = PaddingValues(bottom = LocalChromeInset.current)) { songRows(songs, actions, playing, done, selected, menu, cover = { vm.cover(it.coverArt, CoverSize.ROW) }, from = LIBRARY_SONGS) }
    }
}

@Composable
private fun Decades(vm: DecadesViewModel = viewModel()) {
    val load by vm.decades.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    LoadBox(load) { decades ->
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            if (decades.isEmpty()) item { EmptyNote(Note.NO_INDEX) }
            items(decades, key = { it.start.toInt() }) { d ->
                NavRow(remember(d.start) { say.decade(d.start.toInt()) }, { nav.decade(d.start.toInt()) }, trailing = "${d.songCount}", chevron = true)
            }
        }
    }
}

@Composable
private fun Folders(vm: FoldersViewModel = viewModel()) {
    val load by vm.roots.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    LoadBox(load) { roots ->
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            items(roots, key = { it.id }) { f ->
                NavRow(
                    f.name, { nav.folder(f.id) }, chevron = true,
                    leading = { Icon(Icons.Filled.Folder, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.primary) },
                )
            }
        }
    }
}

@Composable
private fun Playlists(actions: ActionsViewModel, vm: PlaylistsViewModel = viewModel()) {
    val load by vm.playlists.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    var creating by remember { mutableStateOf(false) }
    var name by remember { mutableStateOf("") }
    val context = androidx.compose.ui.platform.LocalContext.current
    val pickM3u = androidx.activity.compose.rememberLauncherForActivityResult(androidx.activity.result.contract.ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) runCatching { context.contentResolver.openInputStream(uri)?.use { it.readBytes().decodeToString() } }.getOrNull()?.let { text ->
            actions.importM3u(dev.nori.music.ffi.library.m3uPlaylistName(uri.lastPathSegment), text)
        }
    }
    NoriDialog(creating, { creating = false }) {
        AlertCard(
            title = { Text(say.newPlaylist) },
            text = { FormField(name, { name = it }, label = { Text(say.name) }, singleLine = true) },
            confirmButton = { TextButton({ vm.create(name.trim()); creating = false }, enabled = name.isNotBlank()) { Text(say.create) } },
        )
    }
    LoadBox(load) { playlists ->
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            item { ActionRow(say.newPlaylist, Icons.Filled.Add, { name = ""; creating = true }) }
            item { ActionRow(say.importPlaylistFile, Icons.Filled.FileDownload, { pickM3u.launch(arrayOf("*/*")) }) }
            if (playlists.isEmpty()) item { EmptyNote(Note.NO_PLAYLISTS) }
            items(playlists, key = { it.id }) { p ->
                NavRow(
                    p.name, { nav.playlist(p.id, p) },
                    subtitle = remember(p.songCount, p.duration) { say.listCaption(p.songCount.toInt(), p.duration.toLong(), false) },
                    leading = { Cover(vm.cover(p.coverArt, CoverSize.ROW), 48.dp) },
                    action = { IconButton({ vm.delete(p.id) }, Modifier.size(40.dp)) { Icon(Icons.Filled.Delete, say.delete, Modifier.size(19.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant) } },
                )
            }
        }
    }
}

@Composable
private fun Favourites(actions: ActionsViewModel, vm: StarredViewModel = viewModel()) {
    val load by vm.starred.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    val menu = LocalSongMenu.current
    LoadBox(load) { s ->
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            // The songs as one list to play, the page the "Favourites" tile on Home opens too.
            item(key = "all") {
                NavRow(
                    say.favouriteSongs, { nav.mix(dev.nori.music.app.vm.FAVOURITES_MIX) }, chevron = true,
                    subtitle = remember(s.librarySongs) { say.songs(s.librarySongs.toInt()) },
                    leading = { Icon(Icons.Filled.Favorite, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.primary) },
                )
            }
            if (s.albums.isNotEmpty()) item(key = "albums") {
                LazyRow(Modifier.bleedsToEdges(), contentPadding = edgePadding(Space.gutter), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    items(s.albums, key = { it.id }) { a -> AlbumCard(a, vm.cover(a.coverArt, CoverSize.CARD), 120.dp, { nav.album(a.id, a) }) }
                }
            }
            items(s.artists, key = { "ar" + it.id }) { a ->
                NavRow(
                    a.name, { nav.artist(a.id, a) }, chevron = true,
                    leading = { Cover(vm.cover(a.coverArt, CoverSize.ROW), 44.dp, radius = 22.dp) },
                )
            }
            songRows(s.songs, actions, null, emptySet(), emptySet(), menu, cover = { vm.cover(it.coverArt, CoverSize.ROW) })
        }
    }
}

@Composable
private fun Genres(vm: GenresViewModel = viewModel()) {
    val load by vm.genres.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    LoadBox(load) { genres ->
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            items(genres, key = { it.name }) { g ->
                NavRow(g.name, { nav.genre(g.name) }, trailing = "${g.songCount}", chevron = true)
            }
        }
    }
}

@Composable
private fun Radio(vm: RadioViewModel = viewModel()) {
    val load by vm.stations.collectAsStateWithLifecycle()
    var adding by remember { mutableStateOf(false) }
    var name by remember { mutableStateOf("") }
    var url by remember { mutableStateOf("") }
    NoriDialog(adding, { adding = false }) {
        AlertCard(
            title = { Text(say.newStation) },
            text = { Column { FormField(name, { name = it }, label = { Text(say.name) }, singleLine = true); Spacer(Modifier.height(10.dp)); FormField(url, { url = it }, label = { Text(say.streamUrl) }, singleLine = true) } },
            confirmButton = { TextButton({ vm.add(name.trim(), url.trim()); adding = false }, enabled = dev.nori.music.ffi.library.radioCanAdd(name, url)) { Text(say.add) } },
        )
    }
    LoadBox(load) { stations ->
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            item { ActionRow(say.newStation, Icons.Filled.Add, { name = ""; url = ""; adding = true }) }
            if (stations.isEmpty()) item { EmptyNote(Note.NO_STATIONS) }
            items(stations, key = { it.id }) { s ->
                NavRow(
                    s.name, { vm.play(s) },
                    leading = { Icon(Icons.Filled.Radio, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.primary) },
                    action = { IconButton({ vm.delete(s.id) }, Modifier.size(40.dp)) { Icon(Icons.Filled.Delete, say.delete, Modifier.size(19.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant) } },
                )
            }
        }
    }
}

/** What a queue played from the library's list of songs carries. */
private val LIBRARY_SONGS = dev.nori.music.ffi.model.PageOrigin(dev.nori.music.ffi.model.OriginKind.SONGS, "")

/** What a queue played from the downloaded songs carries. */
internal val DOWNLOADED_SONGS = dev.nori.music.ffi.model.PageOrigin(dev.nori.music.ffi.model.OriginKind.DOWNLOADS, "")

@Composable
private fun Downloads(actions: ActionsViewModel) {
    val d by actions.downloads.collectAsState()
    val done by actions.downloadedSongs.collectAsStateWithLifecycle()
    val menu = LocalSongMenu.current
    val vm: StarredViewModel = viewModel()
    val sections by actions.downloadSections.collectAsStateWithLifecycle()
    val nav = LocalNav.current
    LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
        // The way to the queue, always there: what is on its way now, or where it went.
        item(key = "queue") {
            // Counted by the core, which splits the queue for the downloads screen: waiting is what is
            // downloading or queued, the failed are counted apart.
            val s = sections
            NavRow(
                say.downloadQueue, nav::downloads, chevron = true,
                subtitle = remember(s) { say.downloadQueue((s?.active?.size ?: 0) + (s?.queued?.size ?: 0), s?.failed?.size ?: 0) },
                leading = { Icon(Icons.Filled.Downloading, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.primary) },
            )
        }
        // Until the songs are read there is nothing to say, not "nothing downloaded" for a frame.
        val songs = done
        if (songs?.isEmpty() == true) item { EmptyNote(Note.NOTHING_DOWNLOADED) }
        songRows(songs.orEmpty(), actions, null, d.doneIds, emptySet(), menu, cover = { vm.cover(it.coverArt, CoverSize.ROW) }, from = DOWNLOADED_SONGS)
    }
}
