package dev.nori.music.app.vm

import android.app.Application
import androidx.lifecycle.viewModelScope
import dev.nori.music.ffi.library.AlbumSort
import dev.nori.music.ffi.library.albumSortKept
import dev.nori.music.ffi.library.albumSortSaved
import dev.nori.music.ffi.library.albumsExhausted
import dev.nori.music.ffi.library.songSortKept
import dev.nori.music.ffi.library.songSortSaved
import dev.nori.music.ffi.model.Album
import dev.nori.music.ffi.library.AlbumDetail
import dev.nori.music.ffi.model.Artist
import dev.nori.music.ffi.library.ArtistDetail
import dev.nori.music.ffi.model.ArtistInfo
import dev.nori.music.ffi.model.Genre
import dev.nori.music.ffi.model.Playlist
import dev.nori.music.ffi.library.PlaylistDetail
import dev.nori.music.ffi.model.RadioStation
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.library.Starred
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.distinctUntilChanged
import dev.nori.music.ffi.settings.HomeRow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.onStart
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import dev.nori.music.ffi.library.HomeShelf
import dev.nori.music.ffi.library.browsePaging
import dev.nori.music.ffi.library.homePinned
import dev.nori.music.ffi.library.homeRefreshDrops
import dev.nori.music.ffi.library.homeShelves
import dev.nori.music.ffi.library.homeRowsShown

/**
 * One row of the home page. Not every shelf is a shelf of albums: the playlists are playlists and the
 * songs someone plays most are songs, and both were asked for by name. Each kind knows whether it has
 * anything to show, so an empty shelf can be left out without the page having to ask what it holds.
 */
sealed interface Shelf {
    val row: HomeRow
    val isEmpty: Boolean

    data class Albums(override val row: HomeRow, val albums: List<Album>) : Shelf {
        override val isEmpty get() = albums.isEmpty()
    }

    data class Playlists(override val row: HomeRow, val playlists: List<Playlist>) : Shelf {
        override val isEmpty get() = playlists.isEmpty()
    }

    data class Songs(override val row: HomeRow, val songs: List<Song>) : Shelf {
        /** What a queue played from this shelf carries. */
        val origin = dev.nori.music.ffi.model.PageOrigin(dev.nori.music.ffi.model.OriginKind.SHELF, row.name)
        override val isEmpty get() = songs.isEmpty()
    }
}

data class HomeUi(val rows: List<Shelf> = emptyList(), val pinned: List<Playlist> = emptyList())

@OptIn(ExperimentalCoroutinesApi::class)
class HomeViewModel(app: Application) : NoriViewModel(app) {
    // The app was opened: a good moment to replay whatever was starred, rated or played while offline.
    init { viewModelScope.launch { runCatching { nori.library.flushPending() } } }

    /**
     * Bumped by [refresh]. Every shelf hangs off it rather than the page hanging off it as a whole, so
     * a re-query replaces each shelf's contents where they are instead of emptying the page first and
     * filling it again - the difference between a refresh and the page being built a second time.
     */
    private val refreshes = MutableStateFlow(0)
    private val _refreshing = MutableStateFlow(false)
    /** True while a manual refresh is running, so the page can show that it is and then stop. */
    val refreshing: StateFlow<Boolean> = _refreshing

    private val _closedToGuests = MutableStateFlow(false)
    /**
     * The jam host's server refused a shelf to this guest (an octo-fiesta from before it shared the
     * library with guests): the page says so instead of standing empty.
     */
    val closedToGuests: StateFlow<Boolean> = _closedToGuests

    private fun failed(e: Throwable) {
        val code = (e as? dev.nori.music.ffi.model.CoreException.Api)?.code ?: (e as? dev.nori.music.ffi.net.NetException.Api)?.code
        if (code == 50 && !nori.rules.account) _closedToGuests.value = true
    }

    /** What each shelf is and where it comes from is the core's (browse.rs); this only makes the requests. */
    private fun source(r: HomeRow, shelf: HomeShelf): kotlinx.coroutines.flow.Flow<Shelf> = when (shelf) {
        is HomeShelf.Albums -> {
            val sort = shelf.sort
            val size = shelf.size.toInt()
            // Re-read on every star change, and the core lays this session's marks over what it reads.
            if (shelf.followsStars) combine(refreshes, nori.library.starsVersion) { _, v -> v }.flatMapLatest { nori.library.favouriteAlbums(size) }
                .map { Shelf.Albums(r, it) }
                .catch { emit(Shelf.Albums(r, emptyList())) }.onStart { emit(Shelf.Albums(r, emptyList())) }
            else refreshes.flatMapLatest { nori.library.albums(sort, size = size) }
                .catch { failed(it); emit(emptyList()) }.onStart { emit(emptyList()) }.map { Shelf.Albums(r, it) }
        }
        is HomeShelf.Playlists -> refreshes.flatMapLatest { nori.library.playlists() }.map { Shelf.Playlists(r, it.take(shelf.take.toInt())) }
            .catch { emit(Shelf.Playlists(r, emptyList())) }.onStart { emit(Shelf.Playlists(r, emptyList())) }
        // Worth reading again after a refresh, which is the one thing that changes what the index holds.
        is HomeShelf.Songs -> refreshes.flatMapLatest {
            flow { emit(Shelf.Songs(r, runCatching { nori.library.browseSongs(shelf.sort, shelf.descending, false, null, 0, shelf.limit.toInt()) }.onFailure(::failed).getOrDefault(emptyList()))) }
        }.onStart { emit(Shelf.Songs(r, emptyList())) }
        HomeShelf.Pinned -> flowOf(Shelf.Playlists(r, emptyList()))
    }

    /** Only the rows the user kept are requested at all; a hidden shelf costs no request. */
    val ui: StateFlow<Load<HomeUi>> = nori.settings.prefs.map { Triple(it.homeRows, it.pinnedPlaylists, nori.rules.account) }.distinctUntilChanged().flatMapLatest { (rows, pins, account) ->
        val shown = homeRowsShown(rows, account)
        val shelves = homeShelves(shown)
        val pinned = if (HomeShelf.Pinned in shelves && pins.isNotEmpty()) refreshes.flatMapLatest { nori.library.playlists() }
            .map { all -> withContext(Dispatchers.Default) { homePinned(all, pins) } }.catch { emit(emptyList()) }.onStart { emit(emptyList()) } else flowOf(emptyList())
        combine(combine(shown.zip(shelves, ::source)) { it.toList() }.onStart { emit(emptyList()) }, pinned) { s, p -> HomeUi(s, p) }
    }.asLoad()

    /**
     * The pull at the top of the page. The stored answers behind the shelves go first, so asking again
     * really does reach the server rather than being told the two-minute-old copy is still fresh, and
     * then the offline index is walked the way the settings page walks it. A refresh that fails is
     * still over: the page has whatever it had before, and the spinner must not be left turning.
     */
    fun refresh() {
        if (_refreshing.value) return
        _refreshing.value = true
        viewModelScope.launch {
            try {
                runCatching { nori.library.dropCached(*homeRefreshDrops().toTypedArray()) }
                refreshes.update { it + 1 }
                if (nori.rules.indexed) runCatching { nori.library.sync() }
            } finally {
                _refreshing.value = false
            }
        }
    }
}

/** The album grid: one sort order at a time, pages appended as the list nears its end. */
@OptIn(ExperimentalCoroutinesApi::class)
class AlbumsViewModel(app: Application) : NoriViewModel(app) {
    private val pageSize by lazy { browsePaging().albums.toInt() }
    // The order it was left in, and how that is kept, are the core's (`album_sort_saved`, `album_sort_kept`).
    private val _sort = MutableStateFlow(albumSortSaved(nori.settings.value.listPrefs))
    val sort: StateFlow<AlbumSort> = _sort
    private val _albums = MutableStateFlow(Grown.empty<Album>())
    val albums: StateFlow<List<Album>> = _albums
    private var loading = false
    private var exhausted = false

    init { loadMore() }

    fun setSort(s: AlbumSort) {
        if (s == _sort.value) return
        _sort.value = s
        val kept = albumSortKept(s)
        nori.settings.update { it.copy(listPrefs = it.listPrefs + (kept.key to kept.value)) }
        _albums.value = Grown.empty()
        exhausted = false
        loading = false
        loadMore()
    }

    fun loadMore() {
        if (loading || exhausted) return
        loading = true
        val sort = _sort.value
        val offset = _albums.value.size
        viewModelScope.launch {
            nori.library.albums(sort, pageSize, offset).catch { }.collect { page ->
                if (sort != _sort.value) return@collect
                _albums.value = _albums.value.from(offset, page)
                exhausted = albumsExhausted(page.size.toUInt())
            }
            loading = false
        }
    }
}

class ArtistsViewModel(app: Application) : NoriViewModel(app) {
    val artists: StateFlow<Load<List<Artist>>> = nori.library.artists().asLoad()
}

class PlaylistsViewModel(app: Application) : NoriViewModel(app) {
    private val refresh = MutableStateFlow(0)
    @OptIn(ExperimentalCoroutinesApi::class)
    val playlists: StateFlow<Load<List<Playlist>>> = refresh.flatMapLatest { nori.library.playlists() }.asLoad()

    fun create(name: String) = viewModelScope.launch { runCatching { nori.library.createPlaylist(name, emptyList()) }; refresh.value++ }
    fun delete(id: String) = viewModelScope.launch { runCatching { nori.library.deletePlaylist(id) }; refresh.value++ }
}

class StarredViewModel(app: Application) : NoriViewModel(app) {
    // Re-queried on every star change: the one-shot read would otherwise keep a removed favourite
    // until the screen is reopened. The stored answer paints first, so there is no loading flash.
    // This session's marks are laid over it by the core as it is read, so an unstarred item leaves the
    // list at once rather than when the server's new answer arrives.
    @OptIn(ExperimentalCoroutinesApi::class)
    val starred: StateFlow<Load<Starred>> = nori.library.starsVersion.flatMapLatest { nori.library.starred() }.asLoad()
}

class GenresViewModel(app: Application) : NoriViewModel(app) {
    val genres: StateFlow<Load<List<Genre>>> = nori.library.genres().asLoad()
}

class RadioViewModel(app: Application) : NoriViewModel(app) {
    private val refresh = MutableStateFlow(0)
    @OptIn(ExperimentalCoroutinesApi::class)
    val stations: StateFlow<Load<List<RadioStation>>> = refresh.flatMapLatest { nori.library.radio() }.asLoad()
    fun play(s: RadioStation) = nori.player.playRadio(s)
    fun add(name: String, url: String) = viewModelScope.launch { runCatching { nori.library.createRadio(name, url) }; refresh.value++ }
    fun delete(id: String) = viewModelScope.launch { runCatching { nori.library.deleteRadio(id) }; refresh.value++ }
}

// ---- detail screens; ids arrive through [open] because the UI layer owns navigation ----

@OptIn(ExperimentalCoroutinesApi::class)
abstract class DetailViewModel<T>(app: Application) : NoriViewModel(app) {
    protected val id = MutableStateFlow<String?>(null)
    fun open(id: String) { this.id.value = id }
    protected abstract fun load(id: String): kotlinx.coroutines.flow.Flow<T>
    val ui: StateFlow<Load<T>> by lazy { id.flatMapLatest { if (it == null) flow { } else load(it) }.asLoad() }
}

class AlbumViewModel(app: Application) : DetailViewModel<AlbumDetail>(app) {
    /**
     * Read again when a provider's song (octo-fiesta) starts sounding and when it stops: playing it made
     * the server download it into the library, and its row's cloud goes only once the server is asked
     * again (the core never takes a stored page with a provider's song for fresh). Only while the page
     * is watched, and only on those moves, not on every player event.
     */
    @OptIn(ExperimentalCoroutinesApi::class)
    override fun load(id: String) = remoteSounding().flatMapLatest { nori.library.album(id) }

    private fun remoteSounding() = nori.player.state.map { s -> s.current?.takeIf { s.playing && it.isExternal }?.id }.distinctUntilChanged()
}

data class ArtistUi(val detail: ArtistDetail, val info: ArtistInfo?, val top: List<Song>)

class ArtistViewModel(app: Application) : DetailViewModel<ArtistUi>(app) {
    @OptIn(ExperimentalCoroutinesApi::class)
    override fun load(id: String) = nori.library.artist(id).flatMapLatest { d ->
        combine(
            nori.library.artistInfo(id).map<ArtistInfo, ArtistInfo?> { it }.catch { emit(null) }.onStart { emit(null) },
            nori.library.topSongs(d.artist.name).catch { emit(emptyList()) }.onStart { emit(emptyList()) },
        ) { info, top -> ArtistUi(d, info, top) }
    }
}

class PlaylistViewModel(app: Application) : DetailViewModel<PlaylistDetail>(app) {
    private val version = MutableStateFlow(0)
    @OptIn(ExperimentalCoroutinesApi::class)
    override fun load(id: String) = version.flatMapLatest { nori.library.playlist(id) }
}

class GenreViewModel(app: Application) : DetailViewModel<List<Song>>(app) {
    override fun load(id: String) = flow { emit(nori.library.songsByGenre(id)) }
}

/** The orders of the "all songs" list; what each sorts on is the core's (`song_sorts`), its name Say's. */
enum class SongSort { TITLE, ARTIST, ALBUM, YEAR, ADDED, PLAYS, LONGEST }

/** Every song of the offline index, a page at a time; the server's, where the profile keeps no index (the core's `songs_listed`). */
class SongsViewModel(app: Application) : NoriViewModel(app) {
    private val _sort = MutableStateFlow(SongSort.valueOf(songSortSaved(nori.settings.value.listPrefs)))
    val sort: StateFlow<SongSort> = _sort
    private val _starred = MutableStateFlow(false)
    val starredOnly: StateFlow<Boolean> = _starred
    private val _songs = MutableStateFlow(Grown.empty<Song>())
    val songs: StateFlow<List<Song>> = _songs
    private var years: IntRange? = null
    private var loading = false
    private var exhausted = false

    init { loadMore() }

    fun setYears(range: IntRange?) { if (range != years) { years = range; reset() } }
    fun setSort(s: SongSort) {
        _sort.value = s
        val kept = songSortKept(s.name)
        nori.settings.update { it.copy(listPrefs = it.listPrefs + (kept.key to kept.value)) }
        reset()
    }
    fun setStarredOnly(on: Boolean) { _starred.value = on; reset() }
    private fun reset() { _songs.value = Grown.empty(); exhausted = false; loading = false; loadMore() }

    fun loadMore() {
        if (loading || exhausted) return
        loading = true
        val s = _sort.value
        val st = _starred.value
        val y = years
        val offset = _songs.value.size
        viewModelScope.launch {
            // A page that could not be read ends the list, as an empty one would.
            val next = runCatching {
                nori.library.songsListed(s.name, st, (y?.first ?: 0).toUInt(), (y?.last ?: 0).toUInt(), offset.toUInt())
            }.getOrNull()
            if (s == _sort.value && st == _starred.value && y == years) { _songs.value = _songs.value.plus(next?.songs.orEmpty()); exhausted = next?.exhausted ?: true }
            loading = false
        }
    }
}

class DecadesViewModel(app: Application) : NoriViewModel(app) {
    val decades: StateFlow<Load<List<dev.nori.music.ffi.library.Decade>>> = flow { emit(nori.library.decades()) }.asLoad()
}

class FoldersViewModel(app: Application) : NoriViewModel(app) {
    val roots: StateFlow<Load<List<Artist>>> = nori.library.folders().asLoad()
}

class FolderViewModel(app: Application) : DetailViewModel<dev.nori.music.ffi.model.Directory>(app) {
    override fun load(id: String) = nori.library.folder(id)
}
