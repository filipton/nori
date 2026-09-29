//! Covers drawn through ratatui-image (kitty, sixel, iTerm2 or half blocks) from images decoded by
//! nori-covers, and the interface colours derived from them by nori-look.

use std::sync::Arc;

use image::{DynamicImage, RgbaImage};
use nori_covers::memory::Image;
use nori_look::cover::CoverColours;
use ratatui::style::Color;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;

/// Display name of a graphics protocol.
pub fn protocol_name(p: ProtocolType) -> &'static str {
    match p {
        ProtocolType::Kitty => "kitty graphics",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Iterm2 => "iTerm2 inline images",
        ProtocolType::Halfblocks => "half blocks",
    }
}

/// Decode size of large covers: sharp at a quarter of a big terminal, cheap to derive colours from.
pub const COVER_PX: u32 = 600;

/// Decode size of album card covers.
pub const THUMB_PX: u32 = 160;

/// Large covers kept: the playing one and the album page's.
const COVERS: usize = 3;

/// Card covers kept: a few screens of cards.
const THUMBS: usize = 120;

/// Encoded covers by key, each with its decoded image for [`Art::resend`]. Oldest first.
type Kept = Vec<(String, StatefulProtocol, Arc<Image>)>;

/// The covers on screen, each encoded once for the area it is drawn in.
pub struct Art {
    picker: Picker,
    covers: Kept,
    /// Keys prefixed with [`crate::backend::THUMB`].
    thumbs: Kept,
}

impl Art {
    pub fn new(picker: Picker) -> Art {
        Art { picker, covers: Vec::new(), thumbs: Vec::new() }
    }

    /// Adds a decoded cover under `key`, evicting the oldest past the cap.
    pub fn put(&mut self, key: String, image: &Arc<Image>) {
        let Some(protocol) = encode(&self.picker, image) else { return };
        let (list, cap) = if key.starts_with(crate::backend::THUMB) { (&mut self.thumbs, THUMBS) } else { (&mut self.covers, COVERS) };
        list.retain(|(k, _, _)| *k != key);
        if list.len() >= cap {
            list.remove(0);
        }
        list.push((key, protocol, image.clone()));
    }

    /// Re-encodes every cover so the next draw transmits it in full. A protocol state sends its picture
    /// once, so a terminal that dropped it (tmux, while the pane was hidden) would never get it back.
    pub fn resend(&mut self) {
        if self.picker.protocol_type() == ProtocolType::Halfblocks {
            return;
        }
        for (_, protocol, image) in self.covers.iter_mut().chain(self.thumbs.iter_mut()) {
            if let Some(p) = encode(&self.picker, image) {
                *protocol = p;
            }
        }
    }

    pub fn get(&mut self, key: &str) -> Option<&mut StatefulProtocol> {
        self.covers.iter_mut().chain(self.thumbs.iter_mut()).find(|(k, _, _)| k == key).map(|(_, p, _)| p)
    }

    pub fn has(&self, key: &str) -> bool {
        self.covers.iter().chain(self.thumbs.iter()).any(|(k, _, _)| k == key)
    }
}

fn encode(picker: &Picker, image: &Image) -> Option<StatefulProtocol> {
    let rgba = RgbaImage::from_raw(image.width, image.height, image.pixels.to_vec())?;
    Some(picker.new_resize_protocol(DynamicImage::ImageRgba8(rgba)))
}

/// Interface colours; `page` None means the terminal's own background.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Theme {
    pub accent: Color,
    pub text: Color,
    pub dim: Color,
    pub page: Option<Color>,
}

pub fn argb(c: u32) -> Color {
    Color::Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Linear blend from `a` (t = 0) to `b` (t = 1); non-RGB colours snap at 0.5.
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
    /// The terminal's own colours with the ARGB `accent`.
    pub fn plain(accent: i64) -> Theme {
        Theme { accent: argb(accent as u32), text: Color::Reset, dim: Color::DarkGray, page: None }
    }

    /// Colours derived from a cover.
    pub fn from_cover(c: &CoverColours) -> Theme {
        let text = argb(c.on);
        let page = argb(c.background);
        Theme { accent: argb(c.accent), text, dim: blend(page, text, 0.62), page: Some(page) }
    }

    /// Text colour at `strength` (1 = full); on the terminal's background only full or dim.
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
    fn cover_tints_theme() {
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
    fn cover_renders_in_every_protocol() {
        let image = Arc::new(Image { width: 8, height: 8, pixels: vec![200u8; 8 * 8 * 4].into_boxed_slice() });
        for p in [ProtocolType::Halfblocks, ProtocolType::Sixel, ProtocolType::Kitty, ProtocolType::Iterm2] {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(p);
            let mut art = Art::new(picker);
            art.put("u".into(), &image);
            assert!(art.has("u"));
            art.resend();
            assert!(art.has("u"));
            let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 10, 5));
            let widget = ratatui_image::StatefulImage::<StatefulProtocol>::default();
            ratatui::widgets::StatefulWidget::render(widget, buf.area, &mut buf, art.get("u").unwrap());
            assert!(buf.content.iter().any(|c| !c.symbol().trim().is_empty() || c.diff_option != ratatui::buffer::CellDiffOption::None), "{p:?} drew nothing");
        }
    }
}
