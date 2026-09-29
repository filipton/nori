//! Word-by-word layout of the active lyric line, as Android's SungText draws it: a feathered fill, each
//! word lifting while sung, held notes swelling. Slint cannot vary colour or offset within one text, so
//! the line is measured with Skia here and each piece becomes its own text.

use nori_core::{LyricLine, LyricWord};
use nori_look::lyrics::{GLOW_FADE_MS, HELD_MS, RISE_MIN_MS, SETTLE_MS};
use skia_safe::font_arguments::variation_position::Coordinate;
use skia_safe::font_arguments::VariationPosition;
use skia_safe::{Font, FontArguments, FontMgr, FontStyle, FourByteTag, Typeface};

use crate::LyricPiece;

/// A laid-out piece: a timed word (`word` is its index) or the text between words.
struct Placed {
    text: String,
    /// Span in the line's text, UTF-16 units.
    start: u32,
    end: u32,
    x: f32,
    row: u32,
    w: f32,
    word: Option<usize>,
}

/// A line laid out at one size and width.
pub struct Laid {
    key: (usize, u32, u32),
    pieces: Vec<Placed>,
    row_h: f32,
}

/// The bold system font the window draws lyrics in (Slint's "System Font"), Inter or sans-serif elsewhere.
pub fn bold_face() -> Option<Typeface> {
    let m = FontMgr::new();
    ["System Font", ".AppleSystemUIFont", "Inter", "sans-serif"]
        .iter()
        .find_map(|name| m.match_family_style(name, FontStyle::bold()))
        .or_else(|| m.legacy_make_typeface(None, FontStyle::bold()))
}

/// `face` at `size`. SF is variable: its optical size follows the text size, so it is measured at that
/// optical size too.
fn font(face: &Typeface, size: f32) -> Font {
    let face = face.clone();
    let coords = [Coordinate { axis: FourByteTag::from_chars('o', 'p', 's', 'z'), value: size }, Coordinate { axis: FourByteTag::from_chars('w', 'g', 'h', 't'), value: 700.0 }];
    let args = FontArguments::new().set_variation_design_position(VariationPosition { coordinates: &coords });
    let face = if face.variation_design_parameters().is_some_and(|a| !a.is_empty()) { face.clone_with_arguments(&args).unwrap_or(face) } else { face };
    Font::from_typeface(face, size)
}

fn units(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// Byte offset of UTF-16 unit `u` in `s`.
fn byte_at(s: &str, u: u32) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= u {
            return i;
        }
        n += c.len_utf16() as u32;
    }
    s.len()
}

/// Lays out line `index` at `size` px within `width`, wrapping at spaces like the text does.
pub fn lay(face: &Typeface, index: usize, line: &LyricLine, size: f32, width: f32) -> Laid {
    let key = (index, size.to_bits(), width.to_bits());
    let font = font(face, size);
    let (spacing, _) = font.metrics();
    let measure = |s: &str| font.measure_str(s, None).0;
    let text = &line.text;
    let total = units(text);
    // Cut at word edges: one piece per timed word, plus the gaps.
    let mut cuts: Vec<(u32, u32, Option<usize>)> = Vec::new();
    let mut at = 0;
    let mut words: Vec<(usize, &LyricWord)> = line.words.iter().enumerate().filter(|(_, w)| w.start < w.end && w.end <= total).collect();
    words.sort_by_key(|(_, w)| w.start);
    for (k, w) in words {
        if w.start < at {
            continue;
        }
        if w.start > at {
            cuts.push((at, w.start, None));
        }
        cuts.push((w.start, w.end, Some(k)));
        at = w.end;
    }
    if at < total {
        cuts.push((at, total, None));
    }
    // Split at spaces so wrapping can break there; a space stays with the piece before it.
    let mut parts: Vec<(u32, u32, Option<usize>)> = Vec::new();
    for (s, e, k) in cuts {
        let piece = &text[byte_at(text, s)..byte_at(text, e)];
        let mut from = s;
        let mut u = s;
        let mut chars = piece.chars().peekable();
        while let Some(c) = chars.next() {
            u += c.len_utf16() as u32;
            if c.is_whitespace() && chars.peek().is_some_and(|n| !n.is_whitespace()) {
                parts.push((from, u, k));
                from = u;
            }
        }
        if from < e {
            parts.push((from, e, k));
        }
    }
    // Greedy wrap by runs between spaces.
    let mut pieces = Vec::new();
    let mut x = 0.0;
    let mut row = 0;
    let mut i = 0;
    while i < parts.len() {
        // A run ends with the part that ends in a space.
        let mut j = i;
        loop {
            let (s, e, _) = parts[j];
            let t = &text[byte_at(text, s)..byte_at(text, e)];
            if t.ends_with(char::is_whitespace) || j + 1 == parts.len() {
                break;
            }
            j += 1;
        }
        let run: Vec<(String, u32, u32, Option<usize>)> = parts[i..=j].iter().map(|&(s, e, k)| (text[byte_at(text, s)..byte_at(text, e)].to_string(), s, e, k)).collect();
        let inked: f32 = run.iter().map(|(t, ..)| measure(t)).sum::<f32>() - run.last().map_or(0.0, |(t, ..)| measure(t) - measure(t.trim_end()));
        if x > 0.0 && x + inked > width {
            x = 0.0;
            row += 1;
        }
        for (t, s, e, k) in run {
            let w = measure(&t);
            pieces.push(Placed { text: t, start: s, end: e, x, row, w, word: k });
            x += w;
        }
        i = j + 1;
    }
    Laid { key, pieces, row_h: spacing }
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Lift of a word, 0..1: rises while sung (over at least `RISE_MIN_MS`), settles over `SETTLE_MS` after.
fn lift(w: &LyricWord, ms: i64) -> f32 {
    if ms <= w.start_ms {
        return 0.0;
    }
    let up = ease((ms - w.start_ms) as f32 / (w.end_ms - w.start_ms).max(RISE_MIN_MS) as f32);
    let down = if ms <= w.end_ms { 0.0 } else { ease((ms - w.end_ms) as f32 / SETTLE_MS as f32) };
    up * (1.0 - down)
}

/// Swell of a note held at least `HELD_MS`, 0..1: grows while held, fades over `GLOW_FADE_MS` after.
fn held(w: &LyricWord, ms: i64) -> f32 {
    let long = w.end_ms - w.start_ms;
    if long < HELD_MS || ms <= w.start_ms {
        return 0.0;
    }
    if ms < w.end_ms {
        ease((ms - w.start_ms) as f32 / long as f32)
    } else {
        1.0 - ease((ms - w.end_ms) as f32 / GLOW_FADE_MS as f32)
    }
}

/// The pieces at `ms` with the fill `sung` UTF-16 units in: alpha `lit` before it, `dim` after, over a
/// `feather` px edge.
pub fn frame(laid: &Laid, line: &LyricLine, sung: f32, ms: i64, lit: f32, dim: f32, feather: f32) -> Vec<LyricPiece> {
    // Row and x of the fill edge.
    let (row_s, x_s) = laid
        .pieces
        .iter()
        .find(|p| sung < p.end as f32)
        .map(|p| (p.row, p.x + p.w * ((sung - p.start as f32) / (p.end - p.start).max(1) as f32).clamp(0.0, 1.0)))
        .unwrap_or((u32::MAX, f32::MAX));
    let alpha = |row: u32, x: f32| {
        if row < row_s {
            lit
        } else if row > row_s {
            dim
        } else {
            let t = ((x - (x_s - feather / 2.0)) / feather).clamp(0.0, 1.0);
            lit + (dim - lit) * t
        }
    };
    laid.pieces
        .iter()
        .map(|p| {
            let w = p.w.max(1.0);
            let a = ((x_s - feather / 2.0 - p.x) / w).clamp(0.0, 1.0);
            let b = ((x_s + feather / 2.0 - p.x) / w).clamp(0.0, 1.0);
            let word = p.word.and_then(|k| line.words.get(k));
            LyricPiece {
                text: p.text.as_str().into(),
                x: p.x,
                y: p.row as f32 * laid.row_h,
                w: p.w,
                a,
                b,
                a0: alpha(p.row, p.x),
                aa: alpha(p.row, p.x + a * w),
                ab: alpha(p.row, p.x + b * w),
                a1: alpha(p.row, p.x + w),
                lift: word.map_or(0.0, |w| lift(w, ms)),
                swell: word.map_or(0.0, |w| held(w, ms)),
            }
        })
        .collect()
}

impl Laid {
    pub fn is_for(&self, index: usize, size: f32, width: f32) -> bool {
        self.key == (index, size.to_bits(), width.to_bits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line() -> LyricLine {
        let w = |start_ms, end_ms, start, end| LyricWord { start_ms, end_ms, start, end };
        // "won" and "der" are one word sung in two syllables.
        LyricLine { start_ms: 0, end_ms: 3000, text: "I wonder why".into(), words: vec![w(0, 500, 0, 1), w(500, 900, 2, 5), w(900, 1400, 5, 8), w(1400, 3000, 9, 12)], ..Default::default() }
    }

    fn face() -> Typeface {
        bold_face().expect("a font")
    }

    #[test]
    fn cuts_at_words_and_spaces() {
        let l = lay(&face(), 0, &line(), 20.0, 1000.0);
        let texts: Vec<&str> = l.pieces.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["I", " ", "won", "der", " ", "why"]);
        assert_eq!(l.pieces.iter().map(|p| p.row).max(), Some(0));
        assert!(l.pieces.windows(2).all(|w| w[1].x > w[0].x));
    }

    #[test]
    fn narrow_line_wraps_at_spaces() {
        let l = lay(&face(), 0, &line(), 20.0, 40.0);
        assert!(l.pieces.iter().map(|p| p.row).max().unwrap_or(0) > 0);
        assert!(l.pieces.iter().filter(|p| p.x == 0.0).all(|p| !p.text.starts_with(' ')));
    }

    #[test]
    fn word_lifts_while_sung_and_settles() {
        let w = LyricWord { start_ms: 1000, end_ms: 1500, start: 0, end: 3 };
        assert_eq!(lift(&w, 900), 0.0);
        assert!(lift(&w, 1400) > 0.5);
        assert_eq!(lift(&w, 1500 + SETTLE_MS), 0.0);
        let long = LyricWord { start_ms: 0, end_ms: HELD_MS + 100, start: 0, end: 3 };
        assert!(held(&long, HELD_MS) > 0.5);
        assert_eq!(held(&w, 1200), 0.0);
    }

    #[test]
    fn fill_is_lit_behind_and_dim_ahead() {
        let line = line();
        let l = lay(&face(), 0, &line, 20.0, 1000.0);
        let f = frame(&l, &line, 6.0, 1000, 1.0, 0.3, 4.0);
        assert_eq!(f.first().map(|p| p.a0), Some(1.0));
        assert_eq!(f.last().map(|p| p.a1), Some(0.3));
    }
}
