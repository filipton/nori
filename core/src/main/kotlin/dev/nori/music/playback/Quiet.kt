package dev.nori.music.playback

/**
 * The perf build's self test plays quietly: [level] is a player volume under every output's own (the
 * engine's fades and ReplayGain), never the phone's, so other apps and the volume keys are left alone. 1 everywhere else, where nothing ever changes it: one field read when an output's
 * volume is set.
 */
object Quiet {
    @Volatile var level = 1f
        private set

    /**
     * The outputs play at [level] of their own volume from now on, through the player (a player made later
     * starts at it); the track open now at once.
     */
    fun set(level: Float) {
        this.level = level.coerceIn(0f, 1f)
        PlaybackService.rustPlayer?.quiet(this.level)
        PlaybackService.track?.track?.let { runCatching { it.setVolume(this.level) } }
    }
}

/**
 * What an offloaded track's stream events said since the process started, counted as they come: for the
 * perf build's self test, which reports them beside the play head. Three counters, nothing else.
 */
object OffloadCalls {
    /** `onDataRequest`: the track has room and asks for more. */
    @Volatile @JvmField var dataRequests = 0L
    /** `onPresentationEnded`: it played everything up to the end of stream. */
    @Volatile @JvmField var presented = 0L
    /** `onTearDown`: the output went where the chip cannot follow. */
    @Volatile @JvmField var tornDown = 0L
}
