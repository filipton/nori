package dev.nori.music

import dalvik.annotation.optimization.FastNative
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Memory a per-frame door writes several values into and Kotlin reads with no call (crates/android/src/
 * view.rs): allocated and kept here, so it outlives every write; the door is given its [address].
 */
internal class NativeView(bytes: Int) {
    val buffer: ByteBuffer = ByteBuffer.allocateDirect(bytes).order(ByteOrder.nativeOrder())
    val address: Long = NativeViewJni.address(buffer)
}

internal object NativeViewJni {
    init { System.loadLibrary("norimusic") }
    @JvmStatic @FastNative external fun address(buffer: ByteBuffer): Long
}
