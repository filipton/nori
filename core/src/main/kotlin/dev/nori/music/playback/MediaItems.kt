package dev.nori.music.playback

import android.net.Uri
import android.os.Bundle
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import dev.nori.music.ffi.queue.Hand
import dev.nori.music.ffi.model.OriginKind
import dev.nori.music.ffi.model.PageOrigin
import dev.nori.music.ffi.model.RadioStation
import dev.nori.music.ffi.model.Song

/**
 * The artwork size asked for on the lock screen and in the notification. The same number the
 * full-screen player uses, so the server renders and caches one large rendition per cover rather than
 * one for each place it is shown - each new size costs a slow first fetch on a real library.
 */
const val NOTIFICATION_ART = 800


/** Audio is addressed as nori://song/<id>; the real URL is decided when the bytes are needed. */
const val SONG_SCHEME = "nori"

fun songUri(id: String): Uri = Uri.Builder().scheme(SONG_SCHEME).authority("song").appendPath(id).build()

/**
 * What the player itself carries for a song: its id, and what the system's notification and lock screen
 * show. Everything else about it (ReplayGain, the transition planner's window, the queue as the app
 * lists it, the queue saved for next time) the core keeps by id - see [toMediaItems], which hands the
 * songs to it, and crates/queue/src/queue.rs.
 */
fun Song.toMediaItem(coverUrl: String?): MediaItem = MediaItem.Builder()
    .setMediaId(id)
    .setUri(songUri(id))
    .setMediaMetadata(
        MediaMetadata.Builder()
            .setTitle(title).setArtist(artist).setAlbumTitle(album)
            .setArtworkUri(coverUrl?.let(Uri::parse))
            .setDurationMs(duration.toLong() * 1000)
            .setTrackNumber(track.toInt()).setGenre(genre)
            .setMediaType(MediaMetadata.MEDIA_TYPE_MUSIC).setIsPlayable(true).setIsBrowsable(false)
            .build()
    )
    .build()

/** Songs about to be queued: handed to the queue [session] in one call, and made into the player's items. */
fun List<Song>.toMediaItems(session: dev.nori.music.ffi.queue.Session, coverUrl: (Song) -> String?): List<MediaItem> {
    if (isNotEmpty()) session.queueRegister(this)
    return heldMediaItems(coverUrl)
}

/**
 * Songs the core handed out for the queue itself and already keeps (the saved queue, autofill, the
 * offline bridge): made into the player's items without crossing back.
 */
fun List<Song>.heldMediaItems(coverUrl: (Song) -> String?): List<MediaItem> = map { it.toMediaItem(coverUrl(it)) }

const val RADIO_PREFIX = "radio:"

/** A station as media3 plays it; [artist] is what it is listed under where a song shows its artist ("Radio"). */
fun RadioStation.toMediaItem(artist: String): MediaItem = MediaItem.Builder()
    .setMediaId(RADIO_PREFIX + id)
    .setUri(streamUrl)
    .setRequestMetadata(MediaItem.RequestMetadata.Builder().setMediaUri(Uri.parse(streamUrl)).build())
    .setMediaMetadata(
        MediaMetadata.Builder().setTitle(name).setArtist(artist).setMediaType(MediaMetadata.MEDIA_TYPE_RADIO_STATION)
            .setIsPlayable(true).setIsBrowsable(false).build()
    )
    .build()

val MediaItem.isRadio get() = mediaId.startsWith(RADIO_PREFIX)

/**
 * Songs added by hand carry how they came across the controller: [Hand.NEXT] (Play next) or [Hand.LAST]
 * (Add to queue). Where that puts them is the core's (nori_player::playlist::Playlist::take); once in,
 * both count alike as hand-added.
 */
private const val QUEUED = "queued"
fun MediaItem.queuedAs(): Hand? = mediaMetadata.extras?.getString(QUEUED)?.let { runCatching { Hand.valueOf(it) }.getOrNull() }
fun MediaItem.queued(how: Hand): MediaItem = withExtra { putString(QUEUED, how.name) }

/**
 * A song taken out of the queue and put back by its undo: the core puts it where it was - its turn under
 * shuffle and its mark as added by hand included (nori-queue `playlist_restore`) - rather than where an
 * insert at its index would.
 */
private const val RESTORED = "restored"
fun MediaItem.isRestored(): Boolean = mediaMetadata.extras?.getBoolean(RESTORED) == true
fun MediaItem.restored(): MediaItem = withExtra { putBoolean(RESTORED, true) }

/**
 * A list already put in the order it plays (a weighted shuffle, which the player's own shuffle would
 * undo), marked on its first item so the core keeps shuffle shown while the player's is off.
 */
private const val KEPT = "kept"
/**
 * The song playing, in a new queue made around it (a tap on it in its album's list): the player takes
 * the new list and carries on with the song where it is instead of starting it again.
 */
fun MediaItem.isKept(): Boolean = mediaMetadata.extras?.getBoolean(KEPT) == true
fun MediaItem.kept(): MediaItem = withExtra { putBoolean(KEPT, true) }

/** A jam's song, [jam] ("Jam · Filip") its subtitle: the notification says it under the song. */
fun MediaItem.inJam(jam: String?): MediaItem =
    if (jam == null) this else buildUpon().setMediaMetadata(mediaMetadata.buildUpon().setSubtitle(jam).build()).build()

private const val ORDERED = "ordered"
fun MediaItem.inOrder(): Boolean = mediaMetadata.extras?.getBoolean(ORDERED) == true
fun MediaItem.ordered(): MediaItem = withExtra { putBoolean(ORDERED, true) }

/**
 * The page a new queue is started from (nori-queue `playlist_set`'s origin), marked on its first item so
 * the service hands it to the core with the songs: that page's Play then reads Pause while the queue
 * plays, and no other page's does. A list without it (radio, a car, a selection) is from no page.
 */
private const val ORIGIN_KIND = "originKind"
private const val ORIGIN_ID = "originId"
fun MediaItem.origin(): PageOrigin? {
    val extras = mediaMetadata.extras ?: return null
    val kind = extras.getString(ORIGIN_KIND)?.let { runCatching { OriginKind.valueOf(it) }.getOrNull() } ?: return null
    return PageOrigin(kind, extras.getString(ORIGIN_ID).orEmpty())
}
fun MediaItem.from(origin: PageOrigin): MediaItem = withExtra { putString(ORIGIN_KIND, origin.kind.name); putString(ORIGIN_ID, origin.id) }

/** [items] with [origin] marked on the first. */
fun startedFrom(items: List<MediaItem>, origin: PageOrigin?): List<MediaItem> =
    if (origin == null || items.isEmpty()) items else listOf(items.first().from(origin)) + items.drop(1)

private inline fun MediaItem.withExtra(put: Bundle.() -> Unit): MediaItem =
    buildUpon().setMediaMetadata(mediaMetadata.buildUpon().setExtras(Bundle(mediaMetadata.extras ?: Bundle.EMPTY).apply(put)).build()).build()

/** A controller's items arrive without their URI; put it back. */
fun MediaItem.playable(): MediaItem =
    buildUpon().setUri(requestMetadata.mediaUri ?: songUri(mediaId)).build()

