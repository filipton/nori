package dev.nori.music.playback

import dalvik.annotation.optimization.CriticalNative

/** Which song the ear is on and where, for the seek bar; see crates/queue/src/heard.rs. */
internal object HeardJni {
    init { System.loadLibrary("norimusic") }

    /** The process's clock over the queue session [session] (`Nori.sessionHandle`), never freed. */
    @JvmStatic @CriticalNative external fun create(session: Long): Long
    /** `(index + 1) << 44 | changed << 43 | ms`; index -1 means the player's own word stands. */
    @JvmStatic @CriticalNative external fun at(h: Long, nowMs: Long, playing: Boolean, positionMs: Long): Long
}
