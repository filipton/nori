package dev.nori.music

import java.io.File
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A class that starts a thread while it is still being built must declare, before that point, every field
 * the thread reaches: Kotlin runs property initializers top to bottom, so a field further down (a `by lazy`
 * too, whose delegate is still null) is read as null from the other thread and start-up dies with an
 * NPE only on the runs where that thread wins. [Nori]'s downloads thread opening the core before the
 * beat model's lazy field existed was one.
 *
 * Read from the source, like a lint: from the members the thread starts at, every member whose name
 * their bodies mention is followed, and each field among them has to come before the line that starts
 * the thread. A new field the thread uses, added below it, fails here with its name.
 */
class InitOrderTest {
    private val src = File("src/main/kotlin/dev/nori/music")

    /** A member of the class body: its name, where it is declared, whether it is a field, its text. */
    private class Member(val name: String, val line: Int, val field: Boolean, val body: String)

    private fun members(lines: List<String>, from: Int): List<Member> {
        val decl = Regex("""^    (?:@\w+ )*(?:(?:private|internal|override|public) )*(val|var|fun) (?:\w+\.)?(\w+)""")
        val starts = lines.indices.mapNotNull { i -> decl.find(lines[i])?.let { i to it } }.filter { it.first >= from }
        return starts.mapIndexed { k, (i, m) ->
            val end = starts.getOrNull(k + 1)?.first ?: lines.size
            Member(m.groupValues[2], i + 1, m.groupValues[1] != "fun", code(lines.subList(i, end)))
        }
    }

    /**
     * What of a member's lines runs: comments left out, and blocks handed to the main thread
     * (`main.post { }`), which run once the object is built when it is built there, as the app builds it.
     */
    private fun code(lines: List<String>): String {
        val text = lines.filterNot { it.trim().startsWith("*") || it.trim().startsWith("/*") }
            .joinToString("\n") { it.substringBefore("//") }
        val out = StringBuilder()
        var i = 0
        while (i < text.length) {
            if (text.startsWith("main.post {", i)) {
                var depth = 0
                var j = text.indexOf('{', i)
                do {
                    if (text[j] == '{') depth++ else if (text[j] == '}') depth--
                    j++
                } while (depth > 0 && j < text.length)
                i = j
            } else {
                out.append(text[i++])
            }
        }
        return out.toString()
    }

    /** The fields reachable from [roots], each with the line it is declared on. */
    private fun reached(members: List<Member>, roots: List<String>): List<Member> {
        val byName = members.groupBy { it.name }
        val seen = LinkedHashSet<String>()
        val todo = ArrayDeque(roots)
        while (todo.isNotEmpty()) {
            val name = todo.removeFirst()
            if (!seen.add(name)) continue
            for (m in byName[name].orEmpty()) {
                val body = m.body.substringAfter(m.name)
                // A word after a dot is another object's member or a package's name, not this class's.
                Regex("""(?<![.\w])\w+\b""").findAll(body).map { it.value }.filter { it in byName && it !in seen }.forEach(todo::addLast)
            }
        }
        return seen.flatMap { byName[it].orEmpty() }.filter { it.field }
    }

    private fun check(path: String, klass: String, starter: Regex, roots: List<String>) {
        val all = File(src, path).readLines()
        val open = all.indexOfFirst { it.startsWith("class $klass") }
        assertTrue("$path: no `class $klass`", open >= 0)
        val close = (open + 1 until all.size).first { all[it] == "}" }
        val lines = all.subList(0, close)
        val at = (open until close).firstOrNull { starter.containsMatchIn(all[it]) }?.plus(1) ?: 0
        assertTrue("$path: no line of $klass matches ${starter.pattern}; the check has to follow where the thread starts now", at > 0)
        val late = reached(members(lines, open), roots).filter { it.line >= at }
        assertTrue(
            "$path: the thread started on line $at reaches fields declared after it, which it can read as null " +
                "while the object is still being built: " + late.joinToString { "${it.name} (line ${it.line})" } +
                ". Declare them above line $at.",
            late.isEmpty(),
        )
    }

    /** Nori hands [Downloads] `::core` and the sources, and its constructor starts a thread on them. */
    @Test fun whatTheDownloadsThreadReachesInNoriIsBuiltBeforeIt() =
        check("Nori.kt", "Nori", Regex("""^    val downloads = Downloads\("""), listOf("core", "lazySources", "client"))

    /** Downloads' own `init` runs [publish] and [reconcile] on its pool before the rest of it is built. */
    @Test fun whatDownloadsInitThreadReachesIsBuiltBeforeIt() =
        check("downloads/Downloads.kt", "Downloads", Regex("""^    init \{"""), listOf("publish", "reconcile"))
}
