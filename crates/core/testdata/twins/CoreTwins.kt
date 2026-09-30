// The Kotlin originals of nori-core's twins, run on the host JVM by tools/twins.sh to write
// core_twins.tsv, which crates/core/tests/twins.rs holds the Rust to. Each function is the app's own code,
// copied as it is; where it leans on an Android or media3 class, the class's part in it is written out
// here (Uri.encode from AOSP's android/net/Uri.java, MutableTransitionState as its two booleans, media3's
// cache as plain numbers). Copy a function again when its original changes, and run tools/twins.sh.
//
// Strings go into the table as their UTF-8 bytes in hex ("_" empty, "-" none), so any character survives.

fun hx(s: String?): String = when {
    s == null -> "-"
    s.isEmpty() -> "_"
    else -> s.toByteArray(Charsets.UTF_8).joinToString("") { "%02x".format(it) }
}

fun list(xs: List<Any?>): String = if (xs.isEmpty()) "_" else xs.joinToString(",")

fun row(vararg f: Any?) = println(f.joinToString("\t"))

// ---- core/.../data/Library.kt: coverUrl, with the prefix the core signs handed in ----

fun coverUrl(urlPrefix: String, id: String?, size: Int): String? {
    if (id == null) return null
    val prefix = urlPrefix + "&id="
    return prefix + uriEncode(id) + "&size=" + size
}

// android.net.Uri.encode(String), AOSP frameworks/base/core/java/android/net/Uri.java, with allow = null.
val HEX_DIGITS = "0123456789ABCDEF".toCharArray()
fun isAllowed(c: Char): Boolean =
    (c in 'A'..'Z') || (c in 'a'..'z') || (c in '0'..'9') || "_-!.~'()*".indexOf(c) != -1
fun uriEncode(s: String): String {
    var encoded: StringBuilder? = null
    val oldLength = s.length
    var current = 0
    while (current < oldLength) {
        var nextToEncode = current
        while (nextToEncode < oldLength && isAllowed(s[nextToEncode])) nextToEncode++
        if (nextToEncode == oldLength) {
            return if (current == 0) s else { encoded!!.append(s, current, oldLength); encoded.toString() }
        }
        if (encoded == null) encoded = StringBuilder()
        if (nextToEncode > current) encoded.append(s, current, nextToEncode)
        current = nextToEncode
        var nextAllowed = current + 1
        while (nextAllowed < oldLength && !isAllowed(s[nextAllowed])) nextAllowed++
        val bytes = s.substring(current, nextAllowed).toByteArray(Charsets.UTF_8)
        for (b in bytes) {
            encoded.append('%')
            encoded.append(HEX_DIGITS[(b.toInt() and 0xf0) shr 4])
            encoded.append(HEX_DIGITS[b.toInt() and 0xf])
        }
        current = nextAllowed
    }
    return encoded?.toString() ?: s
}

// ---- app/.../vm/SearchViewModel.kt: the live search's debounce ----

fun liveDelay(query: String, liveSearchDelayMs: Int): Long = if (query.isBlank()) 0L else liveSearchDelayMs.toLong()

// ---- core/.../net/Http.kt get + decodeToString, as the AutoEQ fetches read a body ----

fun bodyText(body: ByteArray): String = body.decodeToString()

// ---- app/.../vm/PlayerViewModel.kt: setVolumeFraction and volumeFraction ----

fun volumeStep(f: Float, max: Int): Int? {
    if (max <= 0) return null
    return kotlin.math.round(f.coerceIn(0f, 1f) * max).toInt().coerceIn(0, max)
}
fun volumeFraction(step: Int, max: Int): Float {
    val m = max.takeIf { it > 0 } ?: return 0f
    return step / m.toFloat()
}

// ---- app/.../ui/DetailScreens.kt downloadEntry: the songs still to download ----

fun missing(songs: List<String>, done: Set<String>): List<Int> =
    songs.withIndex().filterNot { it.value in done }.map { it.index }

// ---- app/.../ui/DevicesSection.kt AnimatedRows, with MutableTransitionState as current/target ----

class State(var currentState: Boolean) { var targetState = currentState }
class Shown(val key: String, val state: State)
fun animatedRows(old: List<Shown>?, items: List<String>): List<Shown> {
    val byKey = old.orEmpty().associateBy { it.key }
    val next = items.map { k ->
        byKey[k]?.also { it.state.targetState = true } ?: Shown(k, State(old == null).apply { targetState = true })
    }.toMutableList()
    val keys = next.mapTo(HashSet()) { it.key }
    old.orEmpty().forEachIndexed { i, r ->
        if (r.key !in keys && (r.state.currentState || r.state.targetState)) {
            r.state.targetState = false
            next.add(minOf(i, next.size), r)
        }
    }
    return next
}

// ---- core/.../playback/MediaSources.kt ResizableEvictor.trimLocked, the core naming keys oldest first ----

fun trim(order: List<Pair<String, Long>>, max: Long): List<String> {
    val sizes = order.toMap()
    val next = ArrayDeque(order.map { it.first })
    var space = order.sumOf { it.second }
    val removed = ArrayList<String>()
    if (space <= max) return removed
    while (space > max) {
        val key = next.removeFirstOrNull() ?: return removed
        removed += key
        space -= sizes.getValue(key)
    }
    return removed
}

fun main() {
    val prefix = "https://m.example/rest/getCoverArt.view?u=admin&t=26719a1196d2a940705a59634eb18eab&s=c19b2d&f=json&v=1.16.1&c=nori"
    for (id in listOf("al-123", "a b/c?d&e=f", "!'()*~._-", "Ünïcödé", "日本語", "note 🎵 end", "", "%41", "+plus", "tab\there", "x\u007f\u0080")) {
        for (size in listOf(320, 800, 0, -1)) row("cover_url", hx(prefix), hx(id), size, hx(coverUrl(prefix, id, size)))
    }

    for (q in listOf("", " ", "\t\n\r", "\u000b\u000c", "\u001c\u001d\u001e\u001f", "\u0085", " ", " ", " ", "​", "　", "﻿", "    ", "a", " a ", "日本")) {
        row("live_delay", hx(q), liveDelay(q, 350))
    }
    // Every UTF-16 unit Kotlin counts as whitespace.
    for (c in 0..0xFFFF) if (c.toChar().isWhitespace()) row("whitespace", "%04x".format(c))

    val bodies = listOf(
        "4175746f4551",               // plain ASCII
        "c3a4e697a5f09f8eb5",         // two-, three- and four-byte characters
        "efbbbf2320",                 // a byte order mark, kept
        "ff", "c3", "e282", "f09f8e", // a stray byte, and characters cut short
        "c0af", "e080af", "f08080af", // overlong forms
        "eda080", "edbfbf",           // surrogates written out
        "f4908080", "f8888080",       // past U+10FFFF, and a five-byte lead
        "6fff6b", "c328", "e228a1", "f0289f8e", "80bf", "e2828241",
    )
    for (b in bodies) row("text", b, hx(bodyText(b.chunked(2).map { it.toInt(16).toByte() }.toByteArray())))
    // Every lead byte that starts a sequence, against second and third bytes either side of each range's
    // edges, whole and cut short, then a letter after: where the JVM's decoder and UTF-8's own rule for
    // bad bytes could part.
    val leads = listOf(0xc2, 0xdf, 0xe0, 0xe1, 0xed, 0xee, 0xef, 0xf0, 0xf1, 0xf4, 0xf5)
    val seconds = listOf(0x00, 0x41, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xff)
    for (b1 in leads) for (b2 in seconds) {
        for (tail in listOf(emptyList(), listOf(0x41), listOf(0x80), listOf(0xa0), listOf(0xbf), listOf(0xc0), listOf(0x80, 0x80), listOf(0xbf, 0x41))) {
            val bytes = (listOf(b1, b2) + tail).map { it.toByte() }.toByteArray()
            row("text", bytes.joinToString("") { "%02x".format(it) }, hx(bodyText(bytes)))
        }
    }

    for (max in listOf(-1, 0, 1, 7, 15, 25, 150)) {
        for (f in listOf(-0.5f, 0f, 0.01f, 0.1f, 1f / 3f, 0.5f, 0.5f / 15f * 3f, 1f / 14f, 3f / 14f, 0.99f, 1f, 1.5f, Float.NaN)) {
            row("volume_step", "%08x".format(f.toRawBits()), max, volumeStep(f, max) ?: "-")
        }
        for (step in listOf(0, 1, 3, 7, 15)) row("volume_fraction", step, max, "%08x".format(volumeFraction(step, max).toRawBits()))
    }

    for ((songs, done) in listOf(
        listOf("a", "b", "c") to setOf("b"),
        listOf("a", "b") to setOf("a", "b"),
        listOf("a", "b") to emptySet(),
        emptyList<String>() to setOf("a"),
    )) row("missing", list(songs), list(done.toList()), list(missing(songs, done)))

    // Scenarios of a device list changing, each step either the list as it now is ("set:a,b") or every row's
    // animation reaching its end ("settle").
    val scenarios = listOf(
        listOf("set:speaker,usb", "set:speaker,usb,bt", "settle", "set:speaker,bt", "set:speaker,bt", "settle", "set:speaker,bt"),
        listOf("set:a,b,c,d", "settle", "set:b,d", "set:d", "settle", "set:a,d", "set:d,a"),
        listOf("set:_", "set:a", "set:b", "set:a", "settle", "set:_", "settle", "set:_"),
    )
    for ((n, steps) in scenarios.withIndex()) {
        var rows: List<Shown>? = null
        for (step in steps) {
            if (step == "settle") rows?.forEach { it.state.currentState = it.state.targetState }
            else {
                val keys = step.removePrefix("set:").let { if (it == "_") emptyList() else it.split(",") }
                rows = animatedRows(rows, keys)
            }
            row("rows", n, step, list(rows.orEmpty().map { "${it.key}:${it.state.currentState}:${it.state.targetState}" }))
        }
    }

    for ((order, max) in listOf(
        listOf("a" to 400L, "b" to 300L, "c" to 500L) to 1_000L,
        listOf("a" to 400L, "b" to 300L, "c" to 500L) to 1_200L,
        listOf("a" to 400L, "b" to 300L, "c" to 500L) to 0L,
        listOf("a" to 400L, "b" to 300L, "c" to 500L) to 500L,
        emptyList<Pair<String, Long>>() to 0L,
    )) row("trim", list(order.map { "${it.first}:${it.second}" }), max, list(trim(order, max)))
}
