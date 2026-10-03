# uniffi's generated Kotlin. The native side finds package uniffi by class, method and JVM signature
# (the Scaffolding natives by their Java_ names; callbacks, lifted results and coroutine wake-ups by
# GetStaticMethodID), so nothing there, nor any type in those signatures, may be renamed or dropped.
-keep,includedescriptorclasses class uniffi.** { *; }
-keep class dev.nori.music.ffi.** { *; }

# The Rust player reaches these by name from native code (crates/android/src/player.rs): the bridge's
# methods, and a song body's buffer, length, read and close.
-keep class dev.nori.music.playback.RustBridge { *; }
-keep class dev.nori.music.playback.RustBody { *; }

# The measurer asks where a song is and says it measured one, by name (crates/android/src/measure.rs).
-keep class dev.nori.music.playback.MeasureBridge { *; }

# The cover loader calls each request back by name from its own threads (crates/android/src/covers.rs):
# the waiter interface's done, and the done of every class that implements it.
-keep interface dev.nori.music.look.CoverPixels$Waiter { *; }
-keepclassmembers class * implements dev.nori.music.look.CoverPixels$Waiter { void done(android.graphics.Bitmap, int); }

# The native library registers every JNI door by class and method name when it loads
# (crates/android/src/lib.rs), so neither may be renamed or dropped.
-keepclasseswithmembers class dev.nori.music.** { native <methods>; }
