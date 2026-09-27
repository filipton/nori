//! Covers in the terminal, and the colours the interface takes from them. The picture is fetched and
//! decoded by nori-covers (the core's covers, as on Android) and handed to ratatui-image, which speaks
//! whichever graphics protocol the terminal answered to when asked: kitty's, sixel, iTerm2's, or half
//! blocks drawn in colour where none is spoken. The colours are nori-look's, worked out from the same
//! picture as the Android player's sleeve.

use std::sync::Arc;

use image::{DynamicImage, RgbaImage};
use nori_covers::memory::Image;
use nori_look::cover::CoverColours;
use ratatui::style::Color;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;

/// The graphics protocol in use, as a person would name it.
pub fn protocol_name(p: ProtocolType) -> &'static str {
    match p {
        ProtocolType::Kitty => "kitty graphics",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Iterm2 => "iTerm2 inline images",
        ProtocolType::Halfblocks => "half blocks",
    }
}

/// How many pixels a side covers are decoded at: sharp at a quarter of a large terminal, and small
/// enough that the colours are worked out in a few milliseconds.
pub const COVER_PX: u32 = 600;

/// The pictures on screen, each encoded once for the area it is drawn in.
pub struct Art {
    picker: Picker,
    /// A few covers by address, newest last: the one playing and the album page's, with the decoded
    /// picture each was made from (to be sent again, [`Art::resend`]).
    kept: Vec<(String, StatefulProtocol, Arc<Image>)>,
    /// The album cards' small covers (`thumb:<id>`), a screenful and more, least recently put first.
    thumbs: Vec<(String, StatefulProtocol, Arc<Image>)>,
}

/// How many small covers are kept: a few screens of cards.
const THUMBS: usize = 120;

/// How many pixels a side a card's cover is decoded at.
pub const THUMB_PX: u32 = 160;

impl Art {
    pub fn new(picker: Picker) -> Art {
        Art { picker, kept: Vec::new(), thumbs: Vec::new() }
    }

    /// A decoded cover, made ready for the terminal.
    pub fn put(&mut self, url: String, image: &Arc<Image>) {
        let Some(protocol) = self.protocol(image) else { return };
        let (list, cap) = if url.starts_with(crate::backend::THUMB) { (&mut self.thumbs, THUMBS) } else { (&mut self.kept, 3) };
        list.retain(|(u, _, _)| *u != url);
        if list.len() >= cap {
            list.remove(0);
        }
        list.push((url, protocol, image.clone()));
    }

    fn protocol(&self, image: &Image) -> Option<StatefulProtocol> {
        let rgba = RgbaImage::from_raw(image.width, image.height, image.pixels.to_vec())?;
        Some(self.picker.new_resize_protocol(DynamicImage::ImageRgba8(rgba)))
    }

    /// Every picture made again, so the next draw sends it to the terminal in full: a protocol state
    /// sends its picture once (kitty transmits it once, sixel only when its area changes), and a
    /// terminal that dropped it (tmux, while the pane was in another window) would never see it again.
    pub fn resend(&mut self) {
        if self.picker.protocol_type() == ProtocolType::Halfblocks {
            return;
        }
        for i in 0..self.kept.len() {
            if let Some(p) = self.protocol(&self.kept[i].2) {
                self.kept[i].1 = p;
            }
        }
        for i in 0..self.thumbs.len() {
            if let Some(p) = self.protocol(&self.thumbs[i].2) {
                self.thumbs[i].1 = p;
            }
        }
    }

    pub fn get(&mut self, url: &str) -> Option<&mut StatefulProtocol> {
        self.kept.iter_mut().chain(self.thumbs.iter_mut()).find(|(u, _, _)| u == url).map(|(_, p, _)| p)
    }

    pub fn has(&self, url: &str) -> bool {
        self.kept.iter().chain(self.thumbs.iter()).any(|(u, _, _)| u == url)
    }
}

/// The interface's colours: an accent, text on the page and the page itself (None: the terminal's own).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub accent: Color,
    pub text: Color,
    pub dim: Color,
    pub page: Option<Color>,
}

pub fn argb(c: u32) -> Color {
    Color::Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// `a` towards `b` by `t` (0 all `a`), for text lit only partly, as the lyrics' lines are.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)).round() as u8;
            Color::Rgb(m(ar, br), m(ag, bg), m(ab, bb))
        }
        _ if t >= 0.5 => b,
        _ => a,
    }
}

impl Theme {
    /// The app's own accent (the settings' swatch, ARGB) on the terminal's own colours, light or dark:
    /// its own text colour, and its dimmed one.
    pub fn plain(accent: i64) -> Theme {
        Theme { accent: argb(accent as u32), text: Color::Reset, dim: Color::DarkGray, page: None }
    }

    /// The page as a cover dresses it: its accent, its text, its page colour.
    pub fn from_cover(c: &CoverColours) -> Theme {
        let text = argb(c.on);
        let page = argb(c.background);
        Theme { accent: argb(c.accent), text, dim: blend(page, text, 0.62), page: Some(page) }
    }

    /// Text lit to `strength` (1 fully) on this page; on the terminal's own page, full or dimmed.
    pub fn lit(&self, strength: f32) -> Color {
        match self.page {
            Some(page) => blend(page, self.text, strength),
            None if strength >= 0.9 => self.text,
            None => self.dim,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_tints_the_interface_through_nori_look() {
        // A deep red record: the accent and the page come from it, the text stays readable on the page.
        let mut px = vec![0u8; 32 * 32 * 4];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&[180, 20, 30, 255]);
        }
        let image = Image { width: 32, height: 32, pixels: px.into_boxed_slice() };
        let c = crate::backend::derive(&image);
        let t = Theme::from_cover(&c);
        let Some(Color::Rgb(r, g, b)) = t.page else { panic!("a page colour") };
        assert!(r > g && r > b, "the page is the record's red, darkened: {r} {g} {b}");
        assert_ne!(t.accent, Theme::plain(0xff3478f6).accent);
    }

    #[test]
    fn a_decoded_cover_becomes_a_picture_in_every_protocol() {
        let image = Arc::new(Image { width: 8, height: 8, pixels: vec![200u8; 8 * 8 * 4].into_boxed_slice() });
        for p in [ProtocolType::Halfblocks, ProtocolType::Sixel, ProtocolType::Kitty, ProtocolType::Iterm2] {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(p);
            let mut art = Art::new(picker);
            art.put("u".into(), &image);
            assert!(art.has("u"));
            // Made again to be sent in full (a pane back from another tmux window): still there.
            art.resend();
            assert!(art.has("u"));
            let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 5));
            let widget = ratatui_image::StatefulImage::<StatefulProtocol>::default();
            ratatui::widgets::StatefulWidget::render(widget, buf.area, &mut buf, art.get("u").unwrap());
            assert!(buf.content.iter().any(|c| !c.symbol().trim().is_empty() || c.diff_option != ratatui::buffer::CellDiffOption::None), "{p:?} drew nothing");
        }
    }
}
