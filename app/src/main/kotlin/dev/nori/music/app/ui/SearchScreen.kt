package dev.nori.music.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.SearchViewModel
import androidx.compose.runtime.remember

@Composable
fun SearchScreen(actions: ActionsViewModel, vm: SearchViewModel = viewModel()) {
    val ui by vm.ui.collectAsStateWithLifecycle()
    val downloads by actions.downloads.collectAsState()
    val nav = LocalNav.current
    val menu = LocalSongMenu.current
    val scopes = remember { dev.nori.music.ffi.library.searchScopes().map { it to say.searchScope(it) } }
    Column {
        LargeTitle(say.search)
        SearchField(
            ui.query, vm::setQuery, say.searchHint,
            Modifier.padding(horizontal = Space.gutter, vertical = 8.dp), testTag = "search", autofocus = true,
            focusKey = searchFocusKey(),
        )
        if (ui.searching) LinearProgressIndicator(Modifier.fillMaxWidth())
        ui.error?.let { Text(it, Modifier.padding(horizontal = 16.dp), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }

        if (ui.scopesOffered) LazyRow(Modifier.bleedsToEdges(), contentPadding = edgePadding(horizontal = Space.gutter), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            items(scopes) { (scope, label) -> Chip(label, ui.scope == scope) { vm.setScope(scope) } }
        }
        val r = ui.shown
        if (r == null) {
            LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
                if (ui.history.isNotEmpty()) item {
                    Row(Modifier.fillMaxWidth().padding(start = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(say.recentSearches, Modifier.weight(1f).padding(start = 4.dp), style = MaterialTheme.typography.titleLarge)
                        TextButton(vm::clearHistory) { Text(say.clear) }
                    }
                }
                items(ui.history, key = { it }) { q -> Text(q, Modifier.fillMaxWidth().clickable { vm.setQuery(q) }.padding(horizontal = Space.gutter, vertical = 13.dp), style = MaterialTheme.typography.bodyLarge) }
            }
            return@Column
        }
        LazyColumn(contentPadding = PaddingValues(bottom = LocalChromeInset.current)) {
            if (r.artists.isNotEmpty()) item(key = "artists") {
                SectionTitle(say.artists)
                LazyRow(Modifier.bleedsToEdges(), contentPadding = edgePadding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    items(r.artists, key = { it.id }) { a ->
                        ArtistCard(a.name, "", vm.cover(a.coverArt, CoverSize.ROW), 96.dp, onClick = { vm.remember(); nav.artist(a.id, a) })
                    }
                }
            }
            if (r.albums.isNotEmpty()) item(key = "albums") {
                SectionTitle(say.albums)
                LazyRow(Modifier.bleedsToEdges(), contentPadding = edgePadding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    items(r.albums, key = { it.id }) { a -> AlbumCard(a, vm.cover(a.coverArt, CoverSize.CARD), 120.dp, { vm.remember(); nav.album(a.id, a) }) }
                }
            }
            if (r.songs.isNotEmpty()) item(key = "songs") { SectionTitle(say.songs) }
            itemsIndexed(r.songs, key = { _, s -> s.id }, contentType = { _, _ -> "song" }) { _, s ->
                // One tap plays one song: a search result list is not an album, and with octo-fiesta
                // queueing the rest would make the server download every provider track in it.
                // The same two swipes every other list of songs has: a result is a song like any other,
                // and reaching for the menu to queue one was the odd thing out here.
                val (onRight, onLeft) = actions.swipes
                SongRow(
                    s, vm.cover(s.coverArt, CoverSize.ROW), onClick = { vm.remember(); actions.play(listOf(s), from = dev.nori.music.ffi.model.PageOrigin(dev.nori.music.ffi.model.OriginKind.SEARCH, ui.query)) }, onMenu = { menu(s) },
                    downloaded = s.id in downloads.doneIds,
                    swipeRight = rowSwipe(onRight, s, actions), swipeLeft = rowSwipe(onLeft, s, actions),
                )
            }
            if (ui.nothingFound) item { EmptyNote(Note.NOTHING_FOUND) }
        }
    }
}
