package dev.nori.music.data

import dev.nori.music.ffi.model.Lyrics
import dev.nori.music.ffi.settings.LyricsOrigin

/**
 * Lyrics from outside the server (LRCLIB) are looked up, matched, ranked and cached by the core's client
 * (lrclib.rs); this is only the shape the screens show them in. Where they came from, and how the credit
 * line under them says it, are the core's (`LyricsOrigin`, `words_lyrics_credit`).
 */
data class FoundLyrics(val lyrics: Lyrics, val source: LyricsOrigin)

