package dev.nori.music.remote

import android.content.Context
import android.os.Looper
import android.os.SystemClock
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.DeviceInfo
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.common.SimpleBasePlayer
import androidx.media3.common.util.UnstableApi
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import dev.nori.music.Nori
import dev.nori.music.ffi.Mirror
import dev.nori.music.ffi.queue.Hand
import dev.nori.music.ffi.remote.Op
import dev.nori.music.playback.CarArt
import dev.nori.music.playback.NOTIFICATION_ART
import dev.nori.music.playback.heldMediaItems
import dev.nori.music.playback.queuedAs

/**
 * The account's active device, as the media session shows it while it is another one (Remotes.mirror):
 * the notification, the lock screen, a headset and the car control it, and the volume keys set its volume
 * (a remote playback type, which media3 hands the system as a volume provider). Every command goes to the
 * device; what it shows comes from the core, which shows a command's outcome at once and the device's
 * next state after.
 */
@UnstableApi
class RemoteDevicePlayer(private val context: Context, private val nori: Nori) : SimpleBasePlayer(Looper.getMainLooper()) {
    private val remotes = nori.remotes
    private var mirror: Mirror? = null
    private var items: List<MediaItemData> = emptyList()
    private var itemsOf: List<dev.nori.music.ffi.MirrorRow>? = null

    /** The device's newest state. */
    fun show(m: Mirror) {
        mirror = m
        invalidateState()
    }

    private fun rows(m: Mirror): List<MediaItemData> {
        if (itemsOf === m.rows) return items
        val made = m.rows.map { it.song }.heldMediaItems { CarArt.cover(context, it.coverArt, NOTIFICATION_ART)?.toString() }
        items = m.rows.mapIndexed { k, row ->
            MediaItemData.Builder(row.index.toLong()).setMediaItem(made[k]).setDurationUs(row.song.duration.toLong() * 1_000_000).build()
        }
        itemsOf = m.rows
        return items
    }

    override fun getState(): State {
        val m = mirror
        val b = State.Builder().setAvailableCommands(COMMANDS).setAudioAttributes(AudioAttributes.DEFAULT)
            .setDeviceInfo(DeviceInfo.Builder(DeviceInfo.PLAYBACK_TYPE_REMOTE).setMaxVolume(if (m?.volume != null) STEPS else 0).build())
        if (m == null) return b.setPlaybackState(Player.STATE_IDLE).build()
        val list = rows(m)
        val at = m.at?.toInt()
        // The core's clock is elapsedRealtime's (Mirror.at_us).
        val elapsed = if (m.playing) (SystemClock.elapsedRealtimeNanos() / 1_000 - m.atUs) / 1_000 else 0
        return b.setPlaylist(list)
            .setCurrentMediaItemIndex(at ?: C.INDEX_UNSET)
            .setPlayWhenReady(m.playing, Player.PLAY_WHEN_READY_CHANGE_REASON_REMOTE)
            .setPlaybackState(
                when {
                    list.isEmpty() || at == null -> Player.STATE_ENDED
                    m.buffering && m.playing -> Player.STATE_BUFFERING
                    else -> Player.STATE_READY
                },
            )
            .setShuffleModeEnabled(m.shuffle)
            .setRepeatMode(m.repeat.toInt())
            .setContentPositionMs(PositionSupplier.getExtrapolating(m.positionMs + elapsed, if (m.playing) 1f else 0f))
            .setDeviceVolume(m.volume?.let { (it.toInt() * STEPS + 50) / 100 } ?: 0)
            .build()
    }

    private fun send(op: Op): ListenableFuture<*> = remotes.command(op)

    /** The list index the device names the row at [position] by. */
    private fun index(position: Int): UInt? = mirror?.rows?.getOrNull(position)?.index

    override fun handleSetPlayWhenReady(playWhenReady: Boolean): ListenableFuture<*> = send(if (playWhenReady) Op.Play else Op.Pause)

    override fun handlePrepare(): ListenableFuture<*> = Futures.immediateVoidFuture()

    override fun handleStop(): ListenableFuture<*> = send(Op.Pause)

    override fun handleRelease(): ListenableFuture<*> = Futures.immediateVoidFuture()

    override fun handleSeek(mediaItemIndex: Int, positionMs: Long, seekCommand: Int): ListenableFuture<*> {
        val m = mirror ?: return Futures.immediateVoidFuture()
        return when (seekCommand) {
            Player.COMMAND_SEEK_TO_NEXT, Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM -> send(Op.Next)
            Player.COMMAND_SEEK_TO_PREVIOUS, Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM -> send(Op.Previous)
            Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM -> send(Op.Seek(positionMs.coerceAtLeast(0)))
            else -> index(mediaItemIndex)?.let { send(Op.Jump(it, m.rev)) } ?: Futures.immediateVoidFuture()
        }
    }

    override fun handleSetShuffleModeEnabled(shuffleModeEnabled: Boolean): ListenableFuture<*> = send(Op.Shuffle(shuffleModeEnabled))

    override fun handleSetRepeatMode(repeatMode: Int): ListenableFuture<*> = send(Op.Repeat(repeatMode.toUByte()))

    override fun handleSetDeviceVolume(deviceVolume: Int, flags: Int): ListenableFuture<*> = volume(deviceVolume)

    override fun handleIncreaseDeviceVolume(flags: Int): ListenableFuture<*> = volume(deviceVolume + 1)

    override fun handleDecreaseDeviceVolume(flags: Int): ListenableFuture<*> = volume(deviceVolume - 1)

    private fun volume(steps: Int): ListenableFuture<*> = send(Op.Volume((steps.coerceIn(0, STEPS) * 100 / STEPS).toUByte()))

    /** A queue picked in the car or by a controller of this phone: it plays there. */
    override fun handleSetMediaItems(mediaItems: MutableList<MediaItem>, startIndex: Int, startPositionMs: Long): ListenableFuture<*> {
        if (mediaItems.isEmpty()) return Futures.immediateVoidFuture()
        val songs = nori.session.queueSongs(mediaItems.map { it.mediaId })
        val start = if (startIndex == C.INDEX_UNSET) 0 else startIndex
        val position = if (startPositionMs == C.TIME_UNSET) 0 else startPositionMs
        return send(Op.Replace(songs, start.toUInt(), position, true, null, mirror?.shuffle == true, mirror?.repeat ?: 0u))
    }

    override fun handleAddMediaItems(index: Int, mediaItems: MutableList<MediaItem>): ListenableFuture<*> =
        send(Op.Add(nori.session.queueSongs(mediaItems.map { it.mediaId }), mediaItems.firstOrNull()?.queuedAs() == Hand.NEXT))

    override fun handleRemoveMediaItems(fromIndex: Int, toIndex: Int): ListenableFuture<*> {
        val m = mirror ?: return Futures.immediateVoidFuture()
        return index(fromIndex)?.takeIf { toIndex == fromIndex + 1 }?.let { send(Op.Remove(it, m.rev)) } ?: Futures.immediateVoidFuture()
    }

    override fun handleMoveMediaItems(fromIndex: Int, toIndex: Int, newIndex: Int): ListenableFuture<*> {
        val m = mirror ?: return Futures.immediateVoidFuture()
        val from = index(fromIndex)
        val to = index(newIndex)
        return if (from != null && to != null && toIndex == fromIndex + 1) send(Op.Move(from, to, m.rev)) else Futures.immediateVoidFuture()
    }

    private companion object {
        /** The volume keys' steps over the device's 0 to 100. */
        const val STEPS = 20

        val COMMANDS: Player.Commands = Player.Commands.Builder().addAll(
            Player.COMMAND_PLAY_PAUSE, Player.COMMAND_PREPARE, Player.COMMAND_STOP, Player.COMMAND_RELEASE,
            Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM, Player.COMMAND_SEEK_TO_DEFAULT_POSITION, Player.COMMAND_SEEK_TO_MEDIA_ITEM,
            Player.COMMAND_SEEK_TO_NEXT, Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM, Player.COMMAND_SEEK_TO_PREVIOUS, Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM,
            Player.COMMAND_SET_SHUFFLE_MODE, Player.COMMAND_SET_REPEAT_MODE, Player.COMMAND_GET_CURRENT_MEDIA_ITEM, Player.COMMAND_GET_TIMELINE,
            Player.COMMAND_GET_METADATA, Player.COMMAND_SET_MEDIA_ITEM, Player.COMMAND_CHANGE_MEDIA_ITEMS, Player.COMMAND_GET_AUDIO_ATTRIBUTES,
            Player.COMMAND_GET_DEVICE_VOLUME, Player.COMMAND_SET_DEVICE_VOLUME_WITH_FLAGS, Player.COMMAND_ADJUST_DEVICE_VOLUME_WITH_FLAGS,
        ).build()
    }
}
