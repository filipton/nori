package dev.nori.music.playback

import android.content.Context
import android.net.Uri
import android.os.Bundle
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.session.CommandButton
import androidx.media3.session.MediaConstants
import androidx.media3.session.SessionCommand
import dev.nori.music.core.R
import dev.nori.music.ffi.library.BrowseFolder
import dev.nori.music.ffi.library.BrowsePage
import dev.nori.music.ffi.library.CarAction
import dev.nori.music.ffi.library.CarFolder
import dev.nori.music.ffi.library.CarGroup
import dev.nori.music.ffi.library.CarStyle
import dev.nori.music.ffi.library.MixName
import dev.nori.music.ffi.model.Song

/** Words the car's tree needs that are the app's, not this module's: a mix's name. The app sets it as it starts. */
object CarWords {
    @Volatile var mix: (MixName) -> String = { it.name }
}

/**
 * Where the car's pictures come from. A car draws a browse item's picture only from a `content://`
 * address, never from the server's signed one, so the app's provider (CarArtProvider) draws them - the
 * song playing's too, as the queue carries it: a
 * cover by its id, a mix's four covers as its tile on Home, and the glyphs of the Play and Shuffle rows
 * and of the long-press actions.
 */
object CarArt {
    private fun base(context: Context) = Uri.Builder().scheme("content").authority(context.packageName + AUTHORITY)
    fun cover(context: Context, id: String): Uri = base(context).appendPath("c").appendPath(id).build()

    /**
     * The cover [id] at [size] pixels a side, for a song in the queue: what the car's now playing, the
     * notification and the lock screen show. Null for a song without one.
     */
    fun cover(context: Context, id: String?, size: Int): Uri? =
        id?.let { base(context).appendPath("c").appendPath(it).appendQueryParameter("s", size.toString()).build() }

    /** The cover id and size of an address [cover] made; null for any other address. */
    fun coverOf(uri: Uri): Pair<String, Int>? {
        if (uri.scheme != "content" || uri.pathSegments.firstOrNull() != "c") return null
        val id = uri.pathSegments.getOrNull(1) ?: return null
        return id to (uri.getQueryParameter("s")?.toIntOrNull() ?: ART)
    }

    /** A car's cover, pixels a side, where no size is asked: the core's `car::ART`. */
    const val ART = 300
    fun mosaic(context: Context, mix: String, ids: List<String>): Uri =
        base(context).appendPath("m").appendPath(mix).apply { ids.forEach { appendQueryParameter("c", it) } }.build()
    fun glyph(context: Context, name: String): Uri = base(context).appendPath("g").appendPath(name).build()

    /** After the package name: the provider's authority (app/AndroidManifest.xml). */
    const val AUTHORITY = ".carart"
}

/**
 * The car's browse tree as media3 items: the core's folders and rows (crates/library/src/car.rs), named
 * and dressed for Android Auto - how a folder's contents are drawn, the headings rows sit under, the
 * downloaded and explicit marks, the long-press actions.
 */
internal class CarTree(private val context: Context) {
    /** Folder [parent]'s page as the car's rows; [made] is a song as the player carries it. */
    fun items(parent: String, page: BrowsePage, made: (Song) -> MediaItem): List<MediaItem> =
        page.actions.map { action(parent, it) } + page.folders.map(::folder) +
            page.songs.mapIndexed { i, s -> song(parent, s, made(s), page.downloaded.getOrElse(i) { false }, page.songsGroup) }

    /** A folder of the tree, named from the resources, or by its own name. */
    fun folder(f: BrowseFolder): MediaItem {
        val title = f.kind?.let { context.getString(folderName(it)) } ?: f.mix?.let { CarWords.mix(it) } ?: f.title
        val art = when {
            f.mix != null && f.art.isNotEmpty() -> CarArt.mosaic(context, f.id.substringAfter(':'), f.art)
            f.art.isNotEmpty() -> CarArt.cover(context, f.art.first())
            // The tree's own folders, a genre and an artist without a picture each have a glyph, not a blank plate.
            else -> glyphOf(f)?.let { CarArt.glyph(context, it) }
        }
        val extras = Bundle().apply {
            putInt(MediaConstants.EXTRAS_KEY_CONTENT_STYLE_BROWSABLE, if (f.style == CarStyle.GRID) MediaConstants.EXTRAS_VALUE_CONTENT_STYLE_GRID_ITEM else MediaConstants.EXTRAS_VALUE_CONTENT_STYLE_LIST_ITEM)
            putInt(MediaConstants.EXTRAS_KEY_CONTENT_STYLE_PLAYABLE, MediaConstants.EXTRAS_VALUE_CONTENT_STYLE_LIST_ITEM)
            f.group?.let { putString(MediaConstants.EXTRAS_KEY_CONTENT_STYLE_GROUP_TITLE, context.getString(groupName(it))) }
            // The root says the car may search it (MediaBrowserCompat's key, which Android Auto reads).
            if (f.kind == CarFolder.ROOT) putBoolean(SEARCH_SUPPORTED, true)
        }
        val type = when (f.id.substringBefore(':')) {
            "album" -> MediaMetadata.MEDIA_TYPE_ALBUM
            "artist" -> MediaMetadata.MEDIA_TYPE_ARTIST
            "playlist", "mix" -> MediaMetadata.MEDIA_TYPE_PLAYLIST
            "genre" -> MediaMetadata.MEDIA_TYPE_GENRE
            else -> MediaMetadata.MEDIA_TYPE_FOLDER_MIXED
        }
        val subtitle = f.subtitle ?: f.songs?.let { context.resources.getQuantityString(R.plurals.car_song_count, it.toInt(), it.toInt()) }
        return MediaItem.Builder().setMediaId(f.id).setMediaMetadata(
            MediaMetadata.Builder().setTitle(title).setArtist(subtitle).setArtworkUri(art)
                .setIsBrowsable(true).setIsPlayable(f.playable).setMediaType(type).setExtras(extras)
                .apply { if (f.playable) setSupportedCommands(FOLDER_COMMANDS) }.build(),
        ).build()
    }

    /** Play or Shuffle at the top of folder [parent]. */
    private fun action(parent: String, a: CarAction): MediaItem =
        MediaItem.Builder().setMediaId(dev.nori.music.ffi.library.carActionRow(parent, a)).setMediaMetadata(
            MediaMetadata.Builder()
                .setTitle(context.getString(if (a == CarAction.PLAY) R.string.car_play else R.string.car_shuffle))
                .setArtworkUri(CarArt.glyph(context, if (a == CarAction.PLAY) "play" else "shuffle"))
                .setIsBrowsable(false).setIsPlayable(true).setMediaType(MediaMetadata.MEDIA_TYPE_MUSIC).build(),
        ).build()

    /** Song [s] as a row of folder [parent]: picked, it plays the folder from it. */
    private fun song(parent: String, s: Song, made: MediaItem, downloaded: Boolean, group: CarGroup?): MediaItem {
        val extras = Bundle().apply {
            putLong(MediaConstants.EXTRAS_KEY_DOWNLOAD_STATUS, if (downloaded) MediaConstants.EXTRAS_VALUE_STATUS_DOWNLOADED else MediaConstants.EXTRAS_VALUE_STATUS_NOT_DOWNLOADED)
            if (s.explicitStatus == "explicit") putLong(MediaConstants.EXTRAS_KEY_IS_EXPLICIT, 1L)
            group?.let { putString(MediaConstants.EXTRAS_KEY_CONTENT_STYLE_GROUP_TITLE, context.getString(groupName(it))) }
        }
        return made.buildUpon().setMediaId(dev.nori.music.ffi.library.carSongRow(parent, s.id)).setMediaMetadata(
            made.mediaMetadata.buildUpon().setArtworkUri(s.coverArt?.let { CarArt.cover(context, it) })
                .setExtras(extras).setSupportedCommands(SONG_COMMANDS).build(),
        ).build()
    }

    /**
     * What a long press on a row offers in the car: queueing it, and its heart or its download. Each has
     * its picture's address as well as media3's icon: the car's own media app reads the address, and a
     * button without one stopped it before it listed anything.
     */
    fun itemButtons(): List<CommandButton> = listOf(
        button(CommandButton.ICON_QUEUE_NEXT, "next", R.string.car_play_next, CMD_ITEM_NEXT),
        button(CommandButton.ICON_QUEUE_ADD, "queue", R.string.car_add_to_queue, CMD_ITEM_QUEUE),
        button(CommandButton.ICON_HEART_UNFILLED, "heart", R.string.car_favourite, CMD_ITEM_FAVOURITE),
        button(CommandButton.ICON_UNDEFINED, "download", R.string.car_download, CMD_ITEM_DOWNLOAD),
    )

    private fun button(icon: Int, glyph: String, name: Int, command: String): CommandButton =
        CommandButton.Builder(icon).setIconUri(CarArt.glyph(context, glyph)).setDisplayName(context.getString(name))
            .setSessionCommand(SessionCommand(command, Bundle.EMPTY))
            .apply { if (icon == CommandButton.ICON_UNDEFINED) setCustomIconResId(R.drawable.car_download) }.build()

    private fun glyphOf(f: BrowseFolder): String? = when (f.kind) {
        // The tabs each have theirs, which the car draws over the tab's name.
        CarFolder.HOME -> "home"
        CarFolder.LIBRARY -> "library"
        CarFolder.RECENTLY_PLAYED -> "recent"
        CarFolder.RECENTLY_ADDED -> "new"
        CarFolder.MOST_PLAYED -> "most"
        CarFolder.ALBUMS -> "albums"
        CarFolder.ARTISTS -> "artists"
        CarFolder.PLAYLISTS -> "playlists"
        CarFolder.GENRES -> "genres"
        CarFolder.RANDOM -> "random"
        CarFolder.FAVOURITES -> "heart"
        CarFolder.DOWNLOADS -> "download"
        null -> when (f.id.substringBefore(':')) {
            "genre" -> "genres"
            "artist" -> "artists"
            else -> null
        }
        else -> null
    }

    private fun folderName(k: CarFolder): Int = when (k) {
        CarFolder.ROOT -> R.string.car_root
        CarFolder.RECENTLY_PLAYED -> R.string.car_recently_played
        CarFolder.RECENTLY_ADDED -> R.string.car_recently_added
        CarFolder.MOST_PLAYED -> R.string.car_most_played
        CarFolder.PLAYLISTS -> R.string.car_playlists
        CarFolder.FAVOURITES -> R.string.car_favourites
        CarFolder.RANDOM -> R.string.car_random
        CarFolder.DOWNLOADS -> R.string.car_downloads
        CarFolder.HOME -> R.string.car_home
        CarFolder.LIBRARY -> R.string.car_library
        CarFolder.ALBUMS -> R.string.car_albums
        CarFolder.ARTISTS -> R.string.car_artists
        CarFolder.GENRES -> R.string.car_genres
    }

    private fun groupName(g: CarGroup): Int = when (g) {
        CarGroup.MIXES -> R.string.car_for_you
        CarGroup.RECENTLY_PLAYED -> R.string.car_recently_played
        CarGroup.RECENTLY_ADDED -> R.string.car_recently_added
        CarGroup.ARTISTS -> R.string.car_artists
        CarGroup.ALBUMS -> R.string.car_albums
        CarGroup.PLAYLISTS -> R.string.car_playlists
        CarGroup.SONGS -> R.string.car_songs
    }

    companion object {
        /** A long press's actions; the row's id comes with them (MediaConstants.EXTRA_KEY_MEDIA_ID). */
        const val CMD_ITEM_NEXT = "nori.car.playNext"
        const val CMD_ITEM_QUEUE = "nori.car.addToQueue"
        const val CMD_ITEM_FAVOURITE = "nori.car.favourite"
        const val CMD_ITEM_DOWNLOAD = "nori.car.download"
        private val SONG_COMMANDS = listOf(CMD_ITEM_NEXT, CMD_ITEM_QUEUE, CMD_ITEM_FAVOURITE, CMD_ITEM_DOWNLOAD)
        private val FOLDER_COMMANDS = listOf(CMD_ITEM_NEXT, CMD_ITEM_QUEUE, CMD_ITEM_DOWNLOAD)
        private const val SEARCH_SUPPORTED = "android.media.browse.SEARCH_SUPPORTED"
    }
}
