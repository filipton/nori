package dev.nori.music.data

import dev.nori.music.ffi.model.Album
import dev.nori.music.ffi.library.AlbumDetail
import dev.nori.music.ffi.model.Artist
import dev.nori.music.ffi.library.ArtistDetail
import dev.nori.music.ffi.model.ArtistInfo
import dev.nori.music.ffi.Client
import dev.nori.music.ffi.Core
import dev.nori.music.ffi.model.Genre
import dev.nori.music.ffi.model.IngestStats
import dev.nori.music.ffi.lyrics.LyricsPick
import dev.nori.music.ffi.lyrics.LyricsShown
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.buffer
import kotlinx.coroutines.flow.channelFlow
import dev.nori.music.ffi.Page
import dev.nori.music.ffi.PageShown
import dev.nori.music.ffi.StarsShown
import dev.nori.music.ffi.model.Playlist
import dev.nori.music.ffi.library.PlaylistDetail
import dev.nori.music.ffi.model.RadioStation
import dev.nori.music.ffi.Read
import dev.nori.music.ffi.model.SearchResult
import dev.nori.music.ffi.model.ServerInfo
import dev.nori.music.ffi.model.Song
import dev.nori.music.ffi.net.Starrable
import dev.nori.music.ffi.library.Starred
import dev.nori.music.ffi.library.StarMarks
import dev.nori.music.ffi.library.AlbumSort
import dev.nori.music.ffi.library.albumSortApi
import dev.nori.music.ffi.library.librarySizes
import dev.nori.music.ffi.net.Write
import dev.nori.music.net.lifted
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.withContext

/** What can be starred. The marks are kept per kind by the core (`stars.rs`), keyed by id alone. */
enum class StarKind(internal val target: Starrable) {
    SONG(Starrable.SONG), ALBUM(Starrable.ALBUM), ARTIST(Starrable.ARTIST),
}

/** The mark of [id] of [kind], if this session changed it. */
fun StarMarks.of(kind: StarKind, id: String): Boolean? = when (kind) {
    StarKind.SONG -> songs[id]
    StarKind.ALBUM -> albums[id]
    StarKind.ARTIST -> artists[id]
}

/** How the app sizes, names and keeps artwork (the core's `cover_rules`), read once. */
object Covers {
    val rules: dev.nori.music.ffi.CoverRules by lazy { dev.nori.music.ffi.coverRules() }

    /**
     * A cover of an octo-fiesta provider item (external song, album, artist or playlist), which is never
     * kept: octo-fiesta draws a "not downloaded" badge on it and replaces the picture under the same id
     * once the item is in the library. The core's (`is_provider_cover`), through a `@FastNative` door:
     * 0.2 µs and nothing allocated, where the same test in Kotlin took 0.5 µs and 32 bytes.
     */
    fun isProvider(url: String): Boolean = dev.nori.music.look.CoverPixels.isProvider(url)
}

/**
 * Everything the app knows about the server. The core's [Client] does the work - which address, what is
 * stored and for how long, what is queued while offline, how LRCLIB is asked - and this turns its answers
 * into flows for the screens: the stored answer first, the network's only when it differs. The UI never
 * sees bytes or URLs.
 */
class Library(
    private val coreOf: () -> Core,
    private val clientOf: () -> Client,
) {
    // Resolved on every use, which is always on an IO thread: the core belongs to the active server profile
    // and building it costs ~100 ms the UI thread should not pay.
    private val core get() = coreOf()
    private val client get() = clientOf()
    /** How much each read asks for (the core's `library_sizes`). */
    private val sizes by lazy { librarySizes() }

    /**
     * Star changes made this session, one map per kind, as the core keeps them (`stars.rs`, which also
     * lays them over the lists it is asked to). Reads that paint once (a cached list, a queue snapshot)
     * would otherwise sit stale until the screen is reopened; the UI prefers these over the snapshot.
     * Bumped alongside [starsVersion].
     */
    private val _starMarks = MutableStateFlow(StarMarks(emptyMap(), emptyMap(), emptyMap()))
    val starMarks: StateFlow<StarMarks> = _starMarks.asStateFlow()
    /** Star state as it should be shown: this session's change wins over the [snapshot] a list or queue item was built with. */
    fun isStarred(kind: StarKind, id: String, snapshot: Boolean): Boolean = _starMarks.value.of(kind, id) ?: snapshot
    /** Bumped on every star change, so one-shot reads (the favourites list) can re-query. */
    private val _starsVersion = MutableStateFlow(0)
    val starsVersion: StateFlow<Int> = _starsVersion.asStateFlow()

    /** A read that always asks the server. */
    private suspend fun <T> call(read: Read, pick: (Page) -> T): T = withContext(Dispatchers.IO) { pick(lifted { client.readNow(read) }) }

    /**
     * The stored answer paints the screen at once; the server is asked unless that answer is fresh, and
     * its answer is emitted only when it differs. All of it, and when a failure is an error (offline with
     * something stored is not), is the core's (`Client::read_cached`); this only hands each page on.
     */
    private inline fun <T> cached(read: Read, crossinline pick: (Page) -> T): Flow<T> = channelFlow {
        val c = client
        val shown = object : PageShown {
            override fun show(page: Page) { trySend(page) }
        }
        lifted { c.readCached(read, shown) }
    }.buffer(Channel.UNLIMITED).flowOn(Dispatchers.IO).map { pick(it) }

    /**
     * Throws away the stored answers whose key starts with one of [prefixes], so the next read of them
     * has to ask the server (see the core's `drop_cached`).
     */
    suspend fun dropCached(vararg prefixes: String) = withContext(Dispatchers.IO) { lifted { client.dropCached(prefixes.toList()) } }

    // ---- session ----

    suspend fun musicFolders(): List<dev.nori.music.ffi.model.MusicFolder> = call(Read.MusicFolders) { (it as Page.Folders).v }


    // ---- search ----

    /**
     * Always asks the server, so provider results from octo-fiesta show up. One big
     * page: the proxy repeats its external results on every offset.
     */
    suspend fun search(query: String): SearchResult =
        call(Read.Search(query, sizes.searchSongs, sizes.searchAlbums, sizes.searchArtists)) { (it as Page.Found).v }

    /**
     * The server's answer to [query], taken into [session] by the core (`SearchSession::ask`) without
     * coming out here; null when the field has moved on.
     */
    suspend fun searchInto(session: dev.nori.music.ffi.SearchSession, query: String): dev.nori.music.ffi.library.SearchView? =
        withContext(Dispatchers.IO) { lifted { session.ask(client, query) } }

    suspend fun searchHistory(): List<String> = withContext(Dispatchers.IO) { core.searchHistory() }
    suspend fun forgetSearches() = withContext(Dispatchers.IO) { core.searchForget() }

    // ---- browse ----

    /** "By year" is this year's albums; "random" is never stored (the core decides both). */
    fun albums(sort: AlbumSort, size: Int, offset: Int = 0, genre: String? = null): Flow<List<Album>> =
        cached(Read.AlbumList(albumSortApi(sort), size, offset, genre)) { (it as Page.Albums).v }

    fun artists(): Flow<List<Artist>> = cached(Read.ArtistIndex) { (it as Page.Artists).v }
    fun album(id: String): Flow<AlbumDetail> = cached(Read.AlbumById(id)) { (it as Page.AlbumPage).v }

    /** The home page's favourite albums: the starred album list with this session's marks laid over it by the core. */
    fun favouriteAlbums(size: Int): Flow<List<Album>> = cached(Read.FavouriteAlbums(size)) { (it as Page.Albums).v }
    fun artist(id: String): Flow<ArtistDetail> = cached(Read.ArtistById(id)) { (it as Page.ArtistPage).v }
    fun artistInfo(id: String): Flow<ArtistInfo> = cached(Read.ArtistAbout(id)) { (it as Page.About).v }
    fun topSongs(artist: String): Flow<List<Song>> = cached(Read.TopSongs(artist)) { (it as Page.Songs).v }
    fun playlists(): Flow<List<Playlist>> = cached(Read.PlaylistList) { (it as Page.Playlists).v }
    fun playlist(id: String): Flow<PlaylistDetail> = cached(Read.PlaylistById(id)) { (it as Page.PlaylistPage).v }
    /** The favourites, with this session's marks laid over them by the core as they are read. */
    fun starred(): Flow<Starred> = cached(Read.StarredItems) { (it as Page.StarredPage).v }

    /**
     * The starred songs handed to the mixes (the core's `mix_favourites_stored` and `_refresh`): read and
     * handed inside the core, the stored answer first and the server's when it differs. Emits after each.
     */
    fun handFavourites(): Flow<Unit> = flow {
        val c = client
        val stored = lifted { c.mixFavouritesStored() }
        if (stored.handed) emit(Unit)
        if (stored.fresh) return@flow
        if (lifted { c.mixFavouritesRefresh(stored.digest) }) emit(Unit)
    }.flowOn(Dispatchers.IO)
    fun genres(): Flow<List<Genre>> = cached(Read.GenreList) { (it as Page.Genres).v }
    fun radio(): Flow<List<RadioStation>> = cached(Read.RadioList) { (it as Page.Stations).v }

    /**
     * The server's lyrics, and when it has no timed ones and the settings allow it, what the lyrics
     * services have: asked together, ranked and remembered by the core (`lyrics_lookup`), each better
     * answer emitted as it comes. Leaving the lyrics stops collecting this, which cancels the lookup and
     * every request in it.
     */
    fun lyricsFor(song: Song): Flow<FoundLyrics> = channelFlow {
        // The server's first, then the services', each only when it is new, in the core's order
        // (`Client::lyrics_for`).
        val shown = object : LyricsShown {
            override fun show(pick: LyricsPick) { trySend(FoundLyrics(pick.lyrics, pick.origin)) }
        }
        lifted { client.lyricsFor(song.id, shown) }
    }.buffer(Channel.UNLIMITED).flowOn(Dispatchers.IO)

    // ---- local only: history, mixes, smart playlists (all computed in the Rust core from the index) ----

    suspend fun clearHistory() = withContext(Dispatchers.IO) { core.historyClear() }

    suspend fun excludeFromMixes(songId: String, excluded: Boolean) = withContext(Dispatchers.IO) { core.mixExcludedSet(songId, excluded) }

    // ---- what to play: each list is made in the core (actions.rs), requests included ----

    /** A radio from one song: it, then what the server finds similar, or random songs of its genre. */
    suspend fun radio(song: Song): List<Song> = withContext(Dispatchers.IO) { lifted { client.radio(song.id) } }

    /** An instant mix from the index, or the radio when the index has nothing near the song. */
    suspend fun instantMix(song: Song): List<Song> = withContext(Dispatchers.IO) { lifted { client.instantMix(song.id) } }

    /** Every album of an artist (a provider's left out), in order, as one list of songs. */
    suspend fun artistSongs(artistId: String): List<Song> = withContext(Dispatchers.IO) { lifted { client.artistSongsOf(artistId) } }

    suspend fun shuffleAll(): List<Song> = withContext(Dispatchers.IO) { lifted { client.shuffleAll() } }
    suspend fun shuffleAlbums(): List<Song> = withContext(Dispatchers.IO) { lifted { client.shuffleAlbums() } }

    suspend fun smartPlaylists() = withContext(Dispatchers.IO) { core.smartList() }
    fun smartDefaults() = dev.nori.music.ffi.library.smartDefaults()
    suspend fun smartSave(id: String, name: String, json: String): String = withContext(Dispatchers.IO) { core.smartSave(id, name, json) }
    suspend fun smartDelete(id: String) = withContext(Dispatchers.IO) { core.smartDelete(id) }
    suspend fun smartPage(json: String): dev.nori.music.ffi.library.SmartPage =
        withContext(Dispatchers.IO) { core.smartPage(json, sizes.smartSongs) }

    fun m3uExport(name: String, songs: List<Song>): String = dev.nori.music.ffi.library.m3uExport(name, songs)

    /** The server's folder tree, for libraries organised by directory rather than by tags. */
    fun folders(): Flow<List<Artist>> = cached(Read.FolderIndex) { (it as Page.Artists).v }
    fun folder(id: String): Flow<dev.nori.music.ffi.model.Directory> = cached(Read.FolderById(id)) { (it as Page.DirectoryPage).v }

    /** Sorted pages of the offline index: the "all songs" and "by decade" lists. */
    suspend fun browseSongs(sort: String, descending: Boolean, starredOnly: Boolean, years: IntRange?, offset: Int, limit: Int): List<Song> = withContext(Dispatchers.IO) {
        core.browseSongs(sort, descending, starredOnly, (years?.first ?: 0).toUInt(), (years?.last ?: 0).toUInt(), offset.toUInt(), limit.toUInt())
    }

    suspend fun decades(): List<dev.nori.music.ffi.library.Decade> = withContext(Dispatchers.IO) { core.browseDecades() }


    suspend fun randomSongs(): List<Song> = call(Read.RandomSongs(sizes.randomSongs, null)) { (it as Page.Songs).v }

    suspend fun songsByGenre(genre: String): List<Song> = call(Read.SongsByGenre(genre, sizes.genreSongs)) { (it as Page.Songs).v }


    /** What carries the queue on past the song playing (see the core's autofill.rs); nothing on any failure. */
    suspend fun autofill(): dev.nori.music.ffi.Refill = withContext(Dispatchers.IO) { client.autofill() }

    /** Whether the fetched [count] songs go in; if so the core records what it picked. */
    suspend fun autofillArrived(fresh: dev.nori.music.ffi.Refill): Boolean = withContext(Dispatchers.IO) { client.autofillArrived(fresh) }

    /**
     * Draws mix [id] unless this period's draw is there already ([again]: a different one); the core
     * fetches the server's random songs when the index has nothing to draw from. True when it changed.
     */
    suspend fun mixEnsure(id: String, again: Boolean): Boolean = withContext(Dispatchers.IO) { client.mixEnsure(id, again) }

    /** Draws whichever mixes are missing or from the last period. True when any tile changed. */
    suspend fun mixWarm(): Boolean = withContext(Dispatchers.IO) { client.mixWarmAll() }

    /** What picking the server's saved queue back up plays (the core's `resume_from_server`). */
    suspend fun resumeFromServer(): dev.nori.music.ffi.library.ResumePlan = withContext(Dispatchers.IO) { lifted { client.resumeFromServer() } }

    suspend fun song(id: String): Song? = call(Read.SongById(id)) { (it as Page.OneSong).v }

    // ---- writes: the core sends them, keeps them while offline and drops the reads they make stale ----

    private suspend fun write(w: Write) = withContext(Dispatchers.IO) { lifted { client.write(w) } }

    /** Replays queued writes. Stops at the first network failure; a write the server rejects is dropped. */
    suspend fun flushPending() = withContext(Dispatchers.IO) { lifted { client.flushPending() } }

    suspend fun star(kind: StarKind, id: String, on: Boolean) {
        // The core puts the mark up before it asks the server, so the heart fills under the finger, and
        // puts the one from before back if the server refuses (offline is not refusing: the core keeps
        // those and replays them), handing the marks over each time (`Client::star`).
        val shown = object : StarsShown {
            override fun marks(marks: StarMarks) {
                _starMarks.value = marks
                _starsVersion.update { it + 1 }
            }
        }
        withContext(Dispatchers.IO) { lifted { client.star(kind.target, id, on, shown) } }
        // Now the server has it, so the lists that come from it can be asked again.
        _starsVersion.update { it + 1 }
    }

    suspend fun createPlaylist(name: String, songIds: List<String>) = write(Write.CreatePlaylist(name, songIds))

    suspend fun addToPlaylist(id: String, songIds: List<String>) = write(Write.AddToPlaylist(id, songIds))

    suspend fun deletePlaylist(id: String) = write(Write.DeletePlaylist(id))

    /** Not worth queueing: by the time it could be replayed it is no longer true. */
    suspend fun nowPlaying(id: String) { call(Read.NowPlaying(id)) { it } }

    suspend fun createRadio(name: String, streamUrl: String) = write(Write.CreateRadio(name, streamUrl))
    suspend fun deleteRadio(id: String) = write(Write.DeleteRadio(id))

    /** A public link to a song or album; the server must have sharing enabled. */
    suspend fun share(id: String): String = call(Read.ShareLink(id)) { (it as Page.ShareUrl).v }


    /** [submission] false marks "now playing"; true counts the play. */
    suspend fun scrobble(id: String, submission: Boolean, timeMs: Long? = null) = write(Write.Scrobble(id, submission, timeMs))

    // ---- queue hand-off between devices ----

    /** The core's queue handed to the server (`Client::playlist_push`); nothing when there is nothing to hand. */
    suspend fun pushQueue(current: String?, positionMs: Long) = withContext(Dispatchers.IO) { lifted { client.playlistPush(current, positionMs) } }


    // ---- offline index ----

    /**
     * Walks the whole library into the local index. The pages go from the socket
     * into SQLite inside Rust; only three counters come back per page.
     */
    fun sync(page: Int = sizes.syncPage.toInt()): Flow<IngestStats> = flow {
        var offset = 0u
        var total = IngestStats(0u, 0u, 0u)
        while (true) {
            val step = lifted { client.syncPage(offset, page.toUInt(), total) }
            total = step.total
            emit(total)
            offset = step.nextOffset ?: break
        }
    }.flowOn(Dispatchers.IO)

    suspend fun indexSize(): IngestStats = withContext(Dispatchers.IO) { core.indexSize() }

    @Volatile private var coverPrefix: String? = null

    /**
     * Called for every row a list draws, on the UI thread: plain string work, no FFI. The core signs the
     * prefix once (per server and address) and says how the rest is put together (`cover_rules`); a row
     * only appends its id and size, which is cheaper than any crossing into the core would be.
     */
    fun coverUrl(id: String?, size: Int): String? {
        if (id == null) return null
        val prefix = coverPrefix ?: (core.urlPrefix("getCoverArt") + Covers.rules.idParam).also { coverPrefix = it }
        return prefix + android.net.Uri.encode(id) + Covers.rules.sizeParam + size
    }

    /** The signed prefix changes with the server, the credentials or the address in use. */
    fun onServerChanged() { coverPrefix = null }

    /** Another profile is active: its core keeps its own star marks, and this one's are not its hearts. */
    fun onProfileChanged() {
        _starMarks.value = StarMarks(emptyMap(), emptyMap(), emptyMap())
    }
}
