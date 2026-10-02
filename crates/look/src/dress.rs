//! A page's full colour table, computed once per cover or theme; platforms only look entries up (and
//! [`mix`] two tables during a cross-fade).
//!
//! Indexed by the constants below; entries in [`NOT_COLOURS`] hold a flag or `f32` bits. Android mirrors
//! the indices in `CoverLook.kt`. Rules are exact ports of the former Compose code (see `compose`).

use crate::color::{calculate_contrast, color_to_hsl, luminance, BLACK, WHITE};
use crate::compose::{blend, lerp, veil, with_alpha};

// Page colours from `cover::derive`.
pub const EDGE: usize = 0;
pub const BACKGROUND: usize = 1;
pub const ON: usize = 2;
/// Text's softer variant: `ON` at 66 %.
pub const ON_VARIANT: usize = 3;
/// The accent, which is the theme's primary.
pub const ACCENT: usize = 4;
/// Colour of the sleeve's faded bottom.
pub const MELT: usize = 5;

// Theme roles.
pub const ON_PRIMARY: usize = 6;
pub const SURFACE_VARIANT: usize = 7;
pub const SURFACE_CONTAINER: usize = 8;
pub const SURFACE_CONTAINER_HIGH: usize = 9;
pub const SECONDARY_CONTAINER: usize = 10;
pub const OUTLINE_VARIANT: usize = 11;

// Button plates, darker as the page gets paler.
/// `f32` bits: 0 on a dark page, 1 on a pale ("paper") page.
pub const PAPER: usize = 12;
/// The prominent pill (Play) and its ink.
pub const PILL: usize = 13;
pub const PILL_INK: usize = 14;
/// Secondary pill plate; its ink is [`TINT_INK`].
pub const PILL_PLATE: usize = 15;
/// Ink on a secondary plate: accent on dark, text colour on paper.
pub const TINT_INK: usize = 16;
/// Round button plate, selected and plain.
pub const CIRCLE_SELECTED: usize = 17;
pub const CIRCLE_PLATE: usize = 18;
/// Player title-row discs (drawn over the artwork) and their selected glyph.
pub const DISC: usize = 19;
pub const DISC_INK_SELECTED: usize = 20;
/// Search field / chip (8 %), form field / card (7 %), switch off (16 %).
pub const FIELD: usize = 21;
pub const FORM: usize = 22;
pub const SWITCH_OFF: usize = 23;
/// Text at 13 % and 6 % over the page (cover placeholder tones, tile plates).
pub const VEIL_13: usize = 24;
pub const VEIL_6: usize = 25;
/// Text at 10 % over the page (lyrics timing pill).
pub const VEIL_10: usize = 26;

// Text alphas used by the player.
pub const ON_60: usize = 27;
pub const ON_45: usize = 28;
pub const ON_55: usize = 29;
pub const ON_22: usize = 30;
pub const ON_85: usize = 31;
pub const ON_35: usize = 32;
pub const ON_80: usize = 33;
pub const ON_VARIANT_70: usize = 34;

// Floating chrome (now-playing bar and tabs).
pub const CHROME_SLAB: usize = 35;
pub const CHROME_CONTENT: usize = 36;
pub const CHROME_PAGE: usize = 37;
/// Slab outline.
pub const CHROME_EDGE: usize = 38;
/// Chrome secondary text (65 %) and icon (75 %).
pub const CHROME_CONTENT_65: usize = 39;
pub const CHROME_CONTENT_75: usize = 40;
/// List fade under the chrome: page at 75 %.
pub const CHROME_FADE: usize = 41;

/// 1 for dark status bar icons (light page), else 0.
pub const STATUS_LIGHT: usize = 42;
/// `f32` bits: blurred band tint strength (1 on white, cream or black pages); a float so cross-fades
/// mix it. The next three are per-channel luminance scales for [`crate::sleeve::band_matrix`].
pub const BAND_TINT: usize = 43;
pub const BAND_KR: usize = 44;
pub const BAND_KG: usize = 45;
pub const BAND_KB: usize = 46;

/// Album artwork dissolve: edge at 40 %, then edge/page blend at 86 %.
pub const HERO_EDGE: usize = 47;
pub const HERO_MID: usize = 48;
/// Player floor gradient: page at 0, 22 and 75 %.
pub const FLOOR_0: usize = 49;
pub const FLOOR_22: usize = 50;
pub const FLOOR_75: usize = 51;

pub const LEN: usize = 52;

/// Entries that are not colours (mixed as numbers).
pub(crate) const NOT_COLOURS: [usize; 6] = [PAPER, STATUS_LIGHT, BAND_TINT, BAND_KR, BAND_KG, BAND_KB];

/// Play pill on paper, and ink on a light accent.
const PILL_ON_PAPER: u32 = 0xFF1A_1A1A;
const INK_ON_LIGHT: u32 = 0xFF0D_0D0D;

/// White ink on `fill` when it reads at 3:1 (WCAG's floor for large bold text), else near-black.
pub fn ink_on(fill: u32) -> u32 {
    if calculate_contrast(WHITE, fill) >= 3.0 { WHITE } else { INK_ON_LIGHT }
}

/// Theme roles for a page without a cover.
#[derive(Debug, Clone, Copy)]
pub struct Scheme {
    pub background: u32,
    pub on: u32,
    pub on_variant: u32,
    pub primary: u32,
    pub on_primary: u32,
    pub surface_variant: u32,
    pub surface_container: u32,
    pub surface_container_high: u32,
    pub secondary_container: u32,
    pub outline_variant: u32,
}

/// Table for a cover-tinted page, from [`crate::cover::CoverColours`].
pub fn page(edge: u32, background: u32, on: u32, accent: u32, melt: u32) -> [u32; LEN] {
    let mut out = [0u32; LEN];
    out[EDGE] = edge;
    out[MELT] = melt;
    let s = Scheme {
        background,
        on,
        on_variant: with_alpha(on, 0.66),
        primary: accent,
        on_primary: ink_on(accent),
        surface_variant: veil(on, 0.10, background),
        surface_container: veil(on, 0.07, background),
        surface_container_high: veil(on, 0.11, background),
        secondary_container: veil(on, 0.14, background),
        outline_variant: veil(on, 0.14, background),
    };
    dress(&s, Some(edge), &mut out);
    out
}

/// Table for a theme-coloured page; `EDGE` and `MELT` are the background.
pub fn plain(s: &Scheme) -> [u32; LEN] {
    let mut out = [0u32; LEN];
    out[EDGE] = s.background;
    out[MELT] = s.background;
    dress(s, None, &mut out);
    out
}

fn dress(s: &Scheme, edge: Option<u32>, out: &mut [u32; LEN]) {
    let (bg, on, primary) = (s.background, s.on, s.primary);
    out[BACKGROUND] = bg;
    out[ON] = on;
    out[ON_VARIANT] = s.on_variant;
    out[ACCENT] = primary;
    out[ON_PRIMARY] = s.on_primary;
    out[SURFACE_VARIANT] = s.surface_variant;
    out[SURFACE_CONTAINER] = s.surface_container;
    out[SURFACE_CONTAINER_HIGH] = s.surface_container_high;
    out[SECONDARY_CONTAINER] = s.secondary_container;
    out[OUTLINE_VARIANT] = s.outline_variant;

    // Continuous in lightness so a cross-fade never flips a plate in one frame.
    let paper = ((luminance(bg) - 0.40) / 0.40).clamp(0.0, 1.0);
    out[PAPER] = paper.to_bits();
    out[PILL] = lerp(primary, PILL_ON_PAPER, paper);
    // Picked against the drawn pill; a mix of two inks could land grey on grey.
    out[PILL_INK] = ink_on(out[PILL]);
    out[PILL_PLATE] = veil(on, 0.12 + 0.05 * paper, bg);
    out[TINT_INK] = lerp(primary, on, paper);
    out[CIRCLE_SELECTED] = lerp(veil(primary, 0.28, bg), veil(on, 0.14, bg), paper);
    // Lighter on paper: 16 % reads as grey slabs on white.
    out[CIRCLE_PLATE] = veil(on, 0.12 - 0.04 * paper, bg);
    // The discs sit on artwork of any brightness, so they carry their own contrast.
    out[DISC] = with_alpha(BLACK, 0.40 + 0.28 * paper);
    out[DISC_INK_SELECTED] = lerp(primary, WHITE, paper);
    out[FIELD] = veil(on, 0.08, bg);
    out[FORM] = veil(on, 0.07, bg);
    out[SWITCH_OFF] = veil(on, 0.16, bg);
    out[VEIL_13] = veil(on, 0.13, bg);
    out[VEIL_6] = veil(on, 0.06, bg);
    out[VEIL_10] = veil(on, 0.10, bg);

    for (i, a) in [(ON_60, 0.6), (ON_45, 0.45), (ON_55, 0.55), (ON_22, 0.22), (ON_85, 0.85), (ON_35, 0.35), (ON_80, 0.8)] {
        out[i] = with_alpha(on, a);
    }
    out[ON_VARIANT_70] = with_alpha(s.on_variant, 0.7);

    // Chrome slab: page lifted by the text colour (more on dark pages to stand out from AMOLED black),
    // with some of the cover's edge colour on tinted pages.
    let dark = luminance(bg) < 0.5;
    let lift = if dark { 0.20 } else { 0.11 };
    out[CHROME_SLAB] = veil(on, lift, edge.map_or(bg, |e| blend(bg, e, 0.30)));
    out[CHROME_CONTENT] = on;
    out[CHROME_PAGE] = bg;
    out[CHROME_EDGE] = with_alpha(on, if dark { 0.14 } else { 0.07 });
    out[CHROME_CONTENT_65] = with_alpha(on, 0.65);
    out[CHROME_CONTENT_75] = with_alpha(on, 0.75);
    out[CHROME_FADE] = with_alpha(bg, 0.75);

    out[STATUS_LIGHT] = (luminance(bg) > 0.5) as u32;
    // White, cream and black pages have a single-tint wash, so the blurred band takes that tint too.
    let l = color_to_hsl(bg)[2];
    let tinted = l > 0.85 || l < 0.08;
    out[BAND_TINT] = (if tinted { 1.0f32 } else { 0.0 }).to_bits();
    let (r, g, b) = (crate::color::red(bg) as f32 / 255.0, crate::color::green(bg) as f32 / 255.0, crate::color::blue(bg) as f32 / 255.0);
    let top = r.max(g).max(b).max(0.01);
    let k = if tinted { [r / top, g / top, b / top] } else { [1.0, 1.0, 1.0] };
    out[BAND_KR] = k[0].to_bits();
    out[BAND_KG] = k[1].to_bits();
    out[BAND_KB] = k[2].to_bits();

    let e = out[EDGE];
    out[HERO_EDGE] = with_alpha(e, 0.40);
    out[HERO_MID] = with_alpha(blend(e, bg, 0.55), 0.86);
    out[FLOOR_0] = with_alpha(bg, 0.0);
    out[FLOOR_22] = with_alpha(bg, 0.22);
    out[FLOOR_75] = with_alpha(bg, 0.75);
}

/// AMOLED replacements for dark scheme surfaces: background, surface, surface dim, container lowest,
/// low, container, high, highest.
pub const AMOLED: [u32; 8] = [BLACK, BLACK, BLACK, BLACK, 0xFF0A_0A0A, 0xFF11_1111, 0xFF18_1818, 0xFF20_2020];

/// Cross-fade from `a` to `b` into `out` (no allocation): colours via Compose `lerp`, numbers linearly,
/// status bar icons switching at half way.
pub fn mix(a: &[u32; LEN], b: &[u32; LEN], t: f32, out: &mut [u32; LEN]) {
    let t = t.clamp(0.0, 1.0);
    for i in 0..LEN {
        out[i] = if i == STATUS_LIGHT {
            if t < 0.5 { a[i] } else { b[i] }
        } else if NOT_COLOURS.contains(&i) {
            let x = f32::from_bits(a[i]);
            (x + (f32::from_bits(b[i]) - x) * t).to_bits()
        } else {
            lerp(a[i], b[i], t)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_flips_icons_half_way() {
        let dark = page(0xFF20_1010, 0xFF20_1010, WHITE, 0xFFE0_4040, 0xFF20_1010);
        let white = page(WHITE, WHITE, 0xFF11_1111, 0xFFE0_4040, WHITE);
        let mut out = [0u32; LEN];
        mix(&dark, &white, 0.0, &mut out);
        assert_eq!(out, dark);
        mix(&dark, &white, 1.0, &mut out);
        assert_eq!(out, white);
        mix(&dark, &white, 0.25, &mut out);
        assert_eq!(f32::from_bits(out[PAPER]), 0.25);
        assert_eq!(out[STATUS_LIGHT], 0);
        assert_eq!(out[BACKGROUND], lerp(dark[BACKGROUND], white[BACKGROUND], 0.25));
        mix(&dark, &white, 0.5, &mut out);
        assert_eq!(out[STATUS_LIGHT], 1);
    }

    /// Output of the former Compose code (ui-graphics 1.12, JVM): edge, background, on, accent, entries.
    const PAGES: &[(u32, u32, u32, u32, [u32; 18])] = include!("dress_pages.in");

    #[test]
    fn matches_compose() {
        for &(edge, bg, on, accent, want) in PAGES {
            let t = page(edge, bg, on, accent, edge);
            let got = [
                t[ON_VARIANT], t[ON_PRIMARY], t[SURFACE_VARIANT], t[SURFACE_CONTAINER], t[SURFACE_CONTAINER_HIGH], t[SECONDARY_CONTAINER],
                t[PILL], t[PILL_INK], t[PILL_PLATE], t[TINT_INK], t[CIRCLE_SELECTED], t[CIRCLE_PLATE], t[DISC], t[DISC_INK_SELECTED],
                t[CHROME_SLAB], t[CHROME_EDGE], t[HERO_MID], t[FIELD],
            ];
            assert_eq!(got, want, "page {bg:08x} on {on:08x}, accent {accent:08x}");
        }

        // Paper follows page lightness.
        let dark = page(0xFF20_1010, 0xFF20_1010, WHITE, 0xFFE0_4040, 0xFF20_1010);
        let white = page(WHITE, WHITE, 0xFF11_1111, 0xFFE0_4040, WHITE);
        assert_eq!(f32::from_bits(dark[PAPER]), 0.0);
        assert_eq!(f32::from_bits(white[PAPER]), 1.0);
        assert_eq!(white[PILL], PILL_ON_PAPER);
        assert_eq!(dark[PILL], 0xFFE0_4040);
        assert_eq!((dark[STATUS_LIGHT], white[STATUS_LIGHT]), (0, 1));
        assert_eq!((f32::from_bits(dark[BAND_TINT]), f32::from_bits(white[BAND_TINT])), (0.0, 1.0));
        assert_eq!(f32::from_bits(white[BAND_KG]), 1.0);
    }

    #[test]
    fn pill_ink_reads_on_any_cover() {
        for accent in [0xFFA0_A0A0u32, 0xFFA8_B8A0, 0xFF60_6060, 0xFFE0_4040, 0xFF30_40C0, 0xFFF0_E080, 0xFF2C_8885, 0xFFFF_FFFF] {
            for bg in [0xFF10_1010u32, 0xFF55_5555, 0xFF3A_4060, 0xFF90_9090, 0xFFB0_A080, WHITE] {
                let t = page(bg, bg, if luminance(bg) < 0.5 { WHITE } else { 0xFF11_1111 }, accent, bg);
                let c = calculate_contrast(t[PILL_INK], t[PILL]);
                assert!(c >= 3.0, "pill {:08x} with {:08x} reads at {c:.2} on page {bg:08x}", t[PILL], t[PILL_INK]);
            }
        }
        assert_eq!(ink_on(0xFFE0_4040), WHITE);
        assert_eq!(ink_on(0xFFA0_A0A0), INK_ON_LIGHT);
    }

}
