//! Lyrics in web pages (Genius, Megalobiz) and SimpMusic's HTML-escaped LRC, read with a small tag
//! scanner rather than an HTML parser.

use nori_model::Lyrics;

use crate::fit::norm;
use crate::formats::{decode_html, plain};

/// Elements that never have a closing tag.
const VOID: &[&str] = &["br", "img", "hr", "input", "meta", "link", "wbr", "source", "area", "col", "embed", "param", "track"];

/// One tag at the start of a piece of HTML.
struct Tag<'a> {
    /// Lower case; empty for comments, doctypes and the like.
    name: String,
    closing: bool,
    /// Closes itself (`<br/>`, or a void element).
    empty: bool,
    /// Everything between `<` and `>`.
    raw: &'a str,
    len: usize,
}

/// The tag `s` starts with (`s` begins with `<`), or None when it is not closed.
fn tag_at(s: &str) -> Option<Tag<'_>> {
    if s.starts_with("<!--") {
        let end = s.find("-->")? + 3;
        return Some(Tag { name: String::new(), closing: false, empty: true, raw: "", len: end });
    }
    let end = s.find('>')?;
    let raw = &s[1..end];
    let closing = raw.starts_with('/');
    let name: String = raw.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let empty = raw.ends_with('/') || raw.starts_with('!') || raw.starts_with('?') || VOID.contains(&name.as_str());
    Some(Tag { name, closing, empty, raw, len: end + 1 })
}

/// An attribute's value in a tag's raw text, quoted either way.
fn attr<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = raw[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        // A whole attribute name: `data-href=` is not `href=`.
        if at > 0 && !raw.as_bytes()[at - 1].is_ascii_whitespace() {
            continue;
        }
        let Some(rest) = raw[from..].trim_start().strip_prefix('=').map(str::trim_start) else { continue };
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
        let value = &rest[1..];
        return value.find(quote).map(|end| &value[..end]);
    }
    None
}

/// The text of the element whose start tag ends at `from`, `<br>` as a line break, without scripts,
/// styles and what `skip` names; and where it ends. Only `container` tags count toward its end, so
/// unclosed markup inside does not matter.
fn inner_text(html: &str, from: usize, container: &str, skip: impl Fn(&Tag) -> bool) -> (String, usize) {
    let mut out = String::new();
    let mut depth = 1usize;
    let mut skipping: Option<(String, usize)> = None;
    let mut i = from;
    while i < html.len() {
        let rest = &html[i..];
        let Some(lt) = rest.find('<') else {
            if skipping.is_none() {
                out.push_str(rest);
            }
            break;
        };
        if skipping.is_none() {
            out.push_str(&rest[..lt]);
        }
        let at = i + lt;
        let Some(t) = tag_at(&html[at..]) else {
            if skipping.is_none() {
                out.push('<');
            }
            i = at + 1;
            continue;
        };
        i = at + t.len;
        if t.name == container && !t.empty {
            if t.closing {
                depth -= 1;
                if depth == 0 {
                    return (out, i);
                }
            } else {
                depth += 1;
            }
        }
        match &mut skipping {
            Some((name, d)) => {
                if t.name == *name && !t.empty {
                    if t.closing {
                        *d -= 1;
                        if *d == 0 {
                            skipping = None;
                        }
                    } else {
                        *d += 1;
                    }
                }
            }
            None if t.closing => {}
            None if t.name == "br" => out.push('\n'),
            None if !t.empty && (t.name == "script" || t.name == "style" || skip(&t)) => skipping = Some((t.name.clone(), 1)),
            None => {}
        }
    }
    (out, html.len())
}

// ---- Genius -----------------------------------------------------------------------------------------

/// Whether a line of a Genius page is its own furniture rather than a sung line.
fn furniture(line: &str) -> bool {
    let l = line.trim();
    l.eq_ignore_ascii_case("you might also like") || (l.ends_with("Embed") && l.trim_end_matches("Embed").bytes().all(|b| b.is_ascii_digit()))
}

/// A Genius song page's words: every `data-lyrics-container` element without what the page excludes
/// from selection. Section headings ("[Chorus]") become gaps between verses.
pub fn from_genius(html: &str) -> Lyrics {
    const MARK: &str = "data-lyrics-container=\"true\"";
    let mut text = String::new();
    let mut pos = 0;
    while let Some(i) = html[pos..].find(MARK) {
        let at = pos + i;
        pos = at + MARK.len();
        let Some(start) = html[..at].rfind('<') else { continue };
        let Some(t) = tag_at(&html[start..]).filter(|t| !t.closing && !t.empty) else { continue };
        let (inner, end) = inner_text(html, start + t.len, &t.name, |t| t.raw.contains("data-exclude-from-selection=\"true\""));
        text.push_str(&inner);
        text.push('\n');
        pos = pos.max(end);
    }
    let mut lines: Vec<String> = Vec::new();
    for raw in decode_html(&text).lines() {
        let line = raw.trim();
        let heading = line.starts_with('[') && line.ends_with(']');
        if line.is_empty() || heading || furniture(line) {
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.push(line.to_string());
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        return Lyrics::default();
    }
    plain(&lines.join("\n"))
}

// ---- Megalobiz --------------------------------------------------------------------------------------

/// The `/lrc/maker/…` links of a Megalobiz search that name `title` as whole words, those also naming
/// `artist` first, otherwise in page order.
pub fn megalobiz_links(html: &str, title: &str, artist: &str) -> Vec<String> {
    let want = norm(title);
    let by = norm(artist);
    let mut out: Vec<(String, bool)> = Vec::new();
    if want.is_empty() {
        return Vec::new();
    }
    let mut pos = 0;
    while let Some(i) = html[pos..].find("<a") {
        let at = pos + i;
        pos = at + 2;
        let Some(t) = tag_at(&html[at..]).filter(|t| t.name == "a" && !t.closing) else { continue };
        let Some(href) = attr(t.raw, "href").filter(|h| h.starts_with("/lrc/maker/")) else { continue };
        let (inner, end) = inner_text(html, at + t.len, "a", |_| false);
        pos = end;
        let said: Vec<String> = [Some(inner.as_str()), attr(t.raw, "title"), attr(t.raw, "name")].into_iter().flatten().map(|s| format!(" {} ", norm(&decode_html(s)))).collect();
        let names = |w: &str| !w.is_empty() && said.iter().any(|s| s.contains(&format!(" {w} ")));
        let link = decode_html(href);
        if names(&want) && out.iter().all(|(l, _)| *l != link) {
            out.push((link, names(&by)));
        }
    }
    out.sort_by_key(|(_, artist)| !artist);
    out.into_iter().map(|(l, _)| l).collect()
}

/// The LRC in a Megalobiz page's `lrc_<number>_details` element; nothing without timed lines.
pub fn from_megalobiz(html: &str) -> Lyrics {
    let mut pos = 0;
    while let Some(i) = html[pos..].find("id=\"lrc_") {
        let at = pos + i;
        pos = at + 8;
        let Some(start) = html[..at].rfind('<') else { continue };
        let Some(t) = tag_at(&html[start..]).filter(|t| !t.closing && !t.empty) else { continue };
        if !attr(t.raw, "id").is_some_and(|id| id.starts_with("lrc_") && id.ends_with("_details")) {
            continue;
        }
        let (inner, _) = inner_text(html, start + t.len, &t.name, |_| false);
        let lyrics = crate::lyrics::from_lrc(&decode_html(&inner));
        if lyrics.synced && !lyrics.lines.is_empty() {
            return lyrics;
        }
    }
    Lyrics::default()
}

// ---- SimpMusic --------------------------------------------------------------------------------------

/// HTML-escaped LRC (SimpMusic's, word tags as `&lt;00:12.34&gt;` on some entries).
pub fn from_escaped_lrc(text: &str) -> Lyrics {
    crate::lyrics::from_lrc(&decode_html(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(l: &Lyrics) -> Vec<&str> {
        l.lines.iter().map(|x| x.text.as_str()).collect()
    }

    /// The structure of a Genius song page today, with invented words: two lyrics containers, a header
    /// inside the first that the page excludes, links and spans round lines, a script, and entities.
    const GENIUS: &str = include_str!("../testdata/genius.html");

    #[test]
    fn genius_page_to_plain_lines() {
        let l = from_genius(GENIUS);
        assert!(!l.synced && !l.word_timed);
        assert_eq!(
            texts(&l),
            ["Paper boats on a quiet river", "La la la line two, it's & more", "", "Counting lamps along the pier", "", "Salt on the window glass", "Hum\u{2026}"]
        );
        let instrumental = r#"<div data-lyrics-container="true">[Instrumental]</div>"#;
        assert!(from_genius(instrumental).lines.is_empty(), "nothing sung is no lyrics");
        assert!(from_genius("<html><body>no lyrics here</body></html>").lines.is_empty());
        assert!(from_genius(r#"<div data-lyrics-container="true">unclosed <b>line"#).lines[0].text.starts_with("unclosed"));
    }

    /// A Megalobiz search page's structure, invented: result links to LRC pages, one a cover by another
    /// band listed first, one for another song.
    const MEGALOBIZ_SEARCH: &str = include_str!("../testdata/megalobiz-search.html");

    #[test]
    fn megalobiz_links_name_the_song() {
        let links = megalobiz_links(MEGALOBIZ_SEARCH, "Glass Harbour", "The Lanterns");
        assert_eq!(
            links,
            ["/lrc/maker/Glass+Harbour.51234567", "/lrc/maker/Glass+Harbour.4000", "/lrc/maker/download/51234567/Glass+Harbour&x=1"],
            "the artist's own first, then the page's order; whole words, so 'Glass Harbours End' is another song"
        );
        assert!(megalobiz_links(MEGALOBIZ_SEARCH, "Paper Boats", "The Lanterns").is_empty());
        assert!(megalobiz_links(MEGALOBIZ_SEARCH, "", "").is_empty());
    }

    #[test]
    fn megalobiz_page_lrc() {
        let page = r#"<div class="lyrics_details entity_more_info"><span id="lrc_51234567_details">[ti:Glass Harbour]<br>[ar:The Lanterns]<br>[00:12.30]Paper boats on a quiet river<br>
[00:16.05]La la la line two<br>[00:20.00]It&#39;s the last line</span></div>"#;
        let l = from_megalobiz(page);
        assert!(l.synced && !l.word_timed);
        assert_eq!(texts(&l), ["Paper boats on a quiet river", "La la la line two", "It's the last line"]);
        assert_eq!(l.lines[1].start_ms, 16_050);
        assert!(from_megalobiz(r#"<span id="lrc_1_details">just words</span>"#).lines.is_empty(), "no timed lines");
        assert!(from_megalobiz(r#"<span id="lrc_1_other">[00:01.00]x</span>"#).lines.is_empty());
    }

    #[test]
    fn escaped_lrc_and_entities() {
        let rich = "[00:01.00]&lt;00:01.00&gt;We&#x27;re &lt;00:01.50&gt;here\n[00:03.00]&lt;00:03.00&gt;Again";
        let l = from_escaped_lrc(rich);
        assert!(l.word_timed);
        assert_eq!(l.lines[0].text, "We're here");
        assert_eq!(l.lines[0].words[1].start_ms, 1500);
        assert_eq!(decode_html("&amp;#39; &#x27; &#8217; a&nbsp;b &bogus; AT&T &lt;"), "&#39; ' \u{2019} a b &bogus; AT&T <");
        assert_eq!(attr(r#"a data-href="/x" href='/y'"#, "href"), Some("/y"));
    }
}
