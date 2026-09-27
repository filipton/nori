//! What a record carries about itself beyond the server's fields, made when it is read (model.rs): a song
//! row's second line and an album card's subtitle. None of it is worded: they put the record's own names
//! together with a mark (🅴) and a " · ". Anything that says something in words ("12 songs") is each
//! client's own, and so is how a provider's item is marked: which service it comes from is not shown.

/// An album card's second line: "Artist · 2019", each part only if there is one. The album carries it
/// as its `subtitle`.
pub fn album_subtitle(artist: &str, year: u32) -> String {
    let mut out = String::new();
    let mut add = |s: &str| {
        if !out.is_empty() {
            out.push_str(" · ");
        }
        out.push_str(s);
    };
    if !artist.is_empty() {
        add(artist);
    }
    if year > 0 {
        add(&year.to_string());
    }
    out
}

/// A song row's second line: an explicit mark, then the artist unless the page is already about them.
/// The song carries it for no page as its `line`; an album's discs carry it for the album's artist.
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
    fn an_album_subtitle_never_names_the_provider() {
        assert_eq!(album_subtitle("A Long Artist Name", 2019), "A Long Artist Name · 2019");
        assert_eq!((album_subtitle("", 2019), album_subtitle("A", 0), album_subtitle("", 0)), ("2019".into(), "A".into(), String::new()));
    }
}
