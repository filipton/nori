//! Display lines derived from a record's own fields (no translated words).

/// Album subtitle: "Artist · 2019", omitting empty parts.
pub fn album_subtitle(artist: &str, year: u32) -> String {
    match (artist.is_empty(), year > 0) {
        (false, true) => format!("{artist} · {year}"),
        (false, false) => artist.to_string(),
        (true, true) => year.to_string(),
        (true, false) => String::new(),
    }
}

/// Song row subtitle: explicit mark, then the artist unless it matches `page_artist` (case-insensitive).
pub fn song_line(explicit_status: &str, artist: &str, page_artist: Option<&str>) -> String {
    let show = page_artist.is_none_or(|p| !artist.to_lowercase().eq(&p.to_lowercase()));
    let mut out = String::new();
    if explicit_status == "explicit" {
        out.push_str("🅴 ");
    }
    if show {
        out.push_str(artist);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_subtitle_skips_empty_parts() {
        assert_eq!(album_subtitle("A Long Artist Name", 2019), "A Long Artist Name · 2019");
        assert_eq!((album_subtitle("", 2019), album_subtitle("A", 0), album_subtitle("", 0)), ("2019".into(), "A".into(), String::new()));
    }

    #[test]
    fn song_line_marks_explicit_and_hides_page_artist() {
        assert_eq!(song_line("explicit", "Muse", None), "🅴 Muse");
        assert_eq!(song_line("clean", "Muse", Some("muse")), "");
        assert_eq!(song_line("", "Muse", Some("Other")), "Muse");
    }
}
