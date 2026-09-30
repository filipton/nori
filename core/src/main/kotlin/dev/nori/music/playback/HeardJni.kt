package dev.nori.music.playback

import dalvik.annotation.optimization.CriticalNative

/** Which song the ear is on and where, for the seek bar; see crates/queue/src/heard.rs. */
internal object HeardJni {
    init { System.loadLibrary("norimusic") }

    /** The process's clock, never freed. */
    @JvmStatic @CriticalNative external fun create(): Long
    /** `(index + 1) << 44 | changed << 43 | ms`; index -1 means the player's own word stands. */
    @JvmStatic @CriticalNative external fun at(h: Long, nowMs: Long, playing: Boolean, positionMs: Long): Long
}

/** Making a seek stick; see crates/android/src/seek.rs. */
internal object SeekJni {
    init { System.loadLibrary("norimusic") }

    @JvmStatic @CriticalNative external fun create(): Long
    @JvmStatic @CriticalNative external fun ask(h: Long, target: Long, now: Long, ready: Boolean, pos: Long)
    @JvmStatic @CriticalNative external fun forget(h: Long)
    /** -1 keep watching, -2 done with it, otherwise the place to ask the player for again. */
    @JvmStatic @CriticalNative external fun look(h: Long, now: Long, sameSong: Boolean, ready: Boolean, pos: Long, playing: Boolean): Long
}
