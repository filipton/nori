package dev.nori.music.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.util.Log

/**
 * Debug builds only: lets `adb shell am broadcast` drive the app directly.
 *
 *   adb shell am broadcast -a dev.nori.music.TEST --es cmd open --es arg settings/audio
 *   adb shell am broadcast -a dev.nori.music.TEST --es cmd state
 *   adb shell am broadcast -a dev.nori.music.TEST --es cmd set --es arg limiter --es value true
 *   adb shell am broadcast -a dev.nori.music.TEST --es cmd play --es arg "search:noise 1"
 *   adb shell am broadcast -a dev.nori.music.TEST --es cmd coverbench --es arg 40
 *
 * Answers go to logcat under the tag `noritest`, one line, so a script can read them back.
 */
class TestBridge : BroadcastReceiver() {
    /** What the player itself knows, with no Activity involved. */
    private fun serviceState(context: Context): String {
        val nori = dev.nori.music.Nori.get(context)
        val st = nori.player.state.value
        return """{"route":"background","playing":${st.playing},"title":"${st.current?.title.orEmpty().replace("\"", "'")}",""" +
            """"artist":"${st.current?.artist.orEmpty().replace("\"", "'")}","positionMs":${nori.player.positionMs},""" +
            """"durationMs":${st.durationMs},"queue":${st.queue.size},"index":${st.index},"error":"${st.error.orEmpty()}",""" +
            """"dspActive":${dev.nori.music.playback.Equalizer.inChain},""" +
            """"gainReductionDb":${dev.nori.music.playback.Equalizer.meterDb},"compressionDb":${dev.nori.music.playback.Equalizer.compressionDb},""" +
            """"offloadWanted":${dev.nori.music.playback.PlaybackService.offloadWanted},""" +
            """"offloaded":${dev.nori.music.playback.PlaybackService.rustPlayer?.offloaded ?: false},""" +
            """"sinkBytes":${dev.nori.music.playback.PlaybackService.rustPlayer?.bytesWritten ?: 0}}"""
    }

    private fun watchStates() {
        val counts = HashMap<String, Int>()
        val handle = androidx.compose.runtime.snapshots.Snapshot.registerApplyObserver { changed, _ ->
            for (o in changed) {
                val name = o.toString().take(120)
                counts[name] = (counts[name] ?: 0) + 1
            }
        }
        Handler(Looper.getMainLooper()).postDelayed({
            handle.dispose()
            Log.i("noritest", "states changed: ${counts.values.sum()} changes over ${counts.size} states")
            // By kind, with the value dropped, so many states of one kind count together.
            counts.entries.groupBy { it.key.substringBefore("(") + "@" + it.key.substringAfterLast("@").length }
                .mapValues { e -> e.value.sumOf { it.value } }.entries.sortedByDescending { it.value }.take(6)
                .forEach { Log.i("noritest", "kind ${it.value}x ${it.key}") }
            counts.entries.sortedByDescending { it.value }.take(12).forEach { Log.i("noritest", "state ${it.value}x ${it.key}") }
            Log.i("noritest", "states done")
        }, 1000)
    }

    override fun onReceive(context: Context, intent: Intent) {
        val cmd = intent.getStringExtra("cmd") ?: return
        val arg = intent.getStringExtra("arg").orEmpty()
        val value = intent.getStringExtra("value").orEmpty()
        // What the core's covers cost: seconds of work, so on a thread of its own, answering with one line
        // when done.
        if (cmd == "coverbench") {
            val app = context.applicationContext
            Thread({ Log.i("noritest", runCatching { Bench.covers(app, arg.toIntOrNull() ?: 40) }.getOrElse { "coverbench failed: $it" }) }, "coverbench").start()
            return
        }
        // Remote control and jams, as the device checks drive them: "watch on|off", "devices", "jam" (opens
        // one, answers its link), "view", "accept" (the first request), "join <link>". The core answers off
        // the main thread, so this does too.
        if (cmd == "remote") {
            val nori = dev.nori.music.Nori.get(context)
            Thread({ Log.i("noritest", runCatching { remoteCheck(nori, arg, value) }.getOrElse { "remote failed: $it" }) }, "remotecheck").start()
            return
        }
        Handler(Looper.getMainLooper()).post {
            val reply = when (cmd) {
                "open" -> TestHooks.open?.let { it(arg); "ok" } ?: "no ui"
                // Playback state must be answerable with the app in the background, because that is
                // where the interesting bugs are: resuming from a notification, a lock screen, a
                // headset button. The UI's richer answer is used when there is a UI.
                "state" -> TestHooks.state?.invoke() ?: serviceState(context)
                "set" -> TestHooks.set?.let { if (it(arg, value)) "ok" else "unknown setting $arg" } ?: "no ui"
                "play" -> TestHooks.play?.let { it(arg); "ok" } ?: "no ui"
                "login" -> TestHooks.login?.let { it(arg); "ok" } ?: "no ui"
                "do" -> TestHooks.act?.let { it(arg); "ok" } ?: "no ui"
                // An internet radio station by its address, as the server would list it:
                //   --es cmd radio --es arg https://ice1.somafm.com/groovesalad-32-aac
                "radio" -> {
                    dev.nori.music.Nori.get(context).player.playRadio(dev.nori.music.ffi.model.RadioStation("test:$arg", arg, arg, null))
                    "ok"
                }
                // Which Compose states change in the next second, and how often: a screen that redraws
                // when nothing on it moves has one of these changing every frame.
                "states" -> { watchStates(); "watching" }
                // How many collections the runtime has run so far, and how much it has allocated: read
                // before and after a stretch to count its GCs.
                "gc" -> "gc ${android.os.Debug.getRuntimeStat("art.gc.gc-count")} blocking ${android.os.Debug.getRuntimeStat("art.gc.blocking-gc-count")}"
                // What a crossing into the core costs, by kind, against the same work done in Kotlin.
                "bench" -> Bench.calls()
                else -> "unknown command $cmd"
            }
            Log.i("noritest", reply)
        }
    }
}

/** One line about the remote control or the jam; see the "remote" command. */
private fun remoteCheck(nori: dev.nori.music.Nori, arg: String, value: String): String {
    val r = { nori.remotes.peek() }
    return when (arg) {
        "watch" -> { nori.remotes.watch(value == "on"); "ok" }
        "devices" -> r()?.devices()?.joinToString("; ") { d ->
            val st = d.state?.let { s -> "${if (s.playing) "playing" else "paused"} ${s.entries.firstOrNull { it.index == s.index }?.title}" } ?: "no state"
            "${d.name}${if (d.nearby) " (nearby)" else ""}: $st${d.refused?.let { " refused $it" } ?: ""}"
        }?.ifEmpty { "none" } ?: "no remote"
        "jam" -> kotlinx.coroutines.runBlocking { nori.remotes.jamOpen() }
        "view" -> r()?.jamView()?.let { v ->
            "hosting=${v.hosting} members=${v.members.joinToString(",") { "${it.name}:${it.role}" }} pending=${v.pending.joinToString(",") { it.song.title }} next=${v.queue?.entries?.joinToString(",") { it.title }}"
        } ?: "no jam"
        "accept" -> r()?.jamView()?.pending?.firstOrNull()?.let { p -> r()?.jamAct(dev.nori.music.ffi.remote.Op.Decide(p.request, true)); "accepted ${p.song.title}" } ?: "nothing waiting"
        // "ask <query>": a guest asks for the first song found.
        "ask" -> kotlinx.coroutines.runBlocking { nori.library.search(value).songs.firstOrNull() }?.let { s ->
            r()?.jamAct(dev.nori.music.ffi.remote.Op.Request(s)); "asked for ${s.title}"
        } ?: "nothing found"
        "join" -> { kotlinx.coroutines.runBlocking { nori.joinJam(value) }; "joined" }
        // "found <host>|<port>|<k=v;k=v>": a door as mDNS would find it (the emulator sees no multicast
        // from the host); the TXT as `dns-sd -L` prints it.
        "found" -> {
            val (host, port, txt) = value.split("|", limit = 3)
            val params = txt.split(";").filter { "=" in it }.map { dev.nori.music.ffi.Param(it.substringBefore("="), it.substringAfter("=")) }
            r()?.lanFound("test-${params.firstOrNull { it.key == "id" }?.value}", host, port.toUShort(), params); "found"
        }
        // "pick <device name>|here": the devices sheet's tap.
        "pick" -> {
            val id = if (value == "here") null else r()?.devices()?.firstOrNull { it.name == value }?.id ?: return "no device $value"
            nori.remotes.pick(id); "picked ${id ?: "here"}"
        }
        // What the player mirrors: the device, its song, place and volume.
        "mirror" -> r()?.active()?.let { m ->
            "${m.name}: ${if (m.playing) "playing" else "paused"} ${m.at?.let { m.rows[it.toInt()].song.title }} at ${m.positionMs} volume=${m.volume} rows=${m.rows.size}/${m.len} shuffle=${m.shuffle} repeat=${m.repeat}"
        } ?: "none"
        else -> "unknown remote check $arg"
    }
}
