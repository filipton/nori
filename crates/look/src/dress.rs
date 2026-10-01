//! Everything a page is dressed in, worked out once per cover (or once per theme): the theme's roles,
//! the plates behind the buttons, the chrome's slab, the status bar's icons, the tint of the sleeve's
//! soft band and the stops of the page's gradients. The platform only looks these up - and, while two
//! pages cross-fade, mixes two of these tables - so nothing about a page's colour is decided per frame.
//!
//! One table of `u32`s, indexed by the constants below; a few entries are not colours but a flag or an
//! `f32` in its bits (said where). The Android side mirrors the indices in `CoverLook.kt`.
//!
//! Every rule here was in the Compose UI before and is ported exactly (see `compose`), so a page looks
//! the same to the pixel as it did when the app worked these out itself.

use crate::color::{calculate_contrast, color_to_hsl, luminance, BLACK, WHITE};
use crate::compose::{blend, lerp, veil, with_alpha};

// The page (the cover's colours as `cover::derive` gives them).
pub const EDGE: usize = 0;
pub const BACKGROUND: usize = 1;
pub const ON: usize = 2;
/// Text's softer variant: `ON` at 66 %.
pub const ON_VARIANT: usize = 3;
/// The accent, which is the theme's primary.
pub const ACCENT: usize = 4;
/// The colour the sleeve's soft bottom wears by itself.
pub const MELT: usize = 5;

// The theme's roles on a tinted page.
pub const ON_PRIMARY: usize = 6;
pub const SURFACE_VARIANT: usize = 7;
pub const SURFACE_CONTAINER: usize = 8;
pub const SURFACE_CONTAINER_HIGH: usize = 9;
pub const SECONDARY_CONTAINER: usize = 10;
pub const OUTLINE_VARIANT: usize = 11;

// The plates behind the buttons, which lean darker as the page gets paler.
/// `f32` bits: 0 on a dark page, 1 on paper - how far to lean into the high-contrast treatment.
pub const PAPER: usize = 12;
/// The prominent pill (Play) and its ink.
pub const PILL: usize = 13;
pub const PILL_INK: usize = 14;
/// A quiet pill's plate. Its ink is [`TINT_INK`].
pub const PILL_PLATE: usize = 15;
/// The accent as ink on a quiet plate: the accent on a dark page, the text colour on paper.
pub const TINT_INK: usize = 16;
/// A round button's plate when selected (a heart that is on), and plain.
pub const CIRCLE_SELECTED: usize = 17;
pub const CIRCLE_PLATE: usize = 18;
/// The player's title-row discs, which sit on the sleeve rather than the page, and a selected glyph on them.
pub const DISC: usize = 19;
pub const DISC_INK_SELECTED: usize = 20;
/// A search field or unselected chip (8 %), a form field or settings card (7 %), a switch that is off (16 %).
pub const FIELD: usize = 21;
pub const FORM: usize = 22;
pub const SWITCH_OFF: usize = 23;
/// Text at 13 % and 6 % over the page: the two tones of the plate under every cover; 6 % is also a
/// tile's plate (a statistic, a smart playlist rule).
pub const VEIL_13: usize = 24;
pub const VEIL_6: usize = 25;
/// Text at 10 % over the page: the lyrics' open timing pill.
pub const VEIL_10: usize = 26;

// Text at the strengths the player uses.
pub const ON_60: usize = 27;
pub const ON_45: usize = 28;
pub const ON_55: usize = 29;
pub const ON_22: usize = 30;
pub const ON_85: usize = 31;
pub const ON_35: usize = 32;
pub const ON_80: usize = 33;
pub const ON_VARIANT_70: usize = 34;

// The floating chrome (now playing bar and tabs) over this page.
pub const CHROME_SLAB: usize = 35;
pub const CHROME_CONTENT: usize = 36;
pub const CHROME_PAGE: usize = 37;
/// The hairline round the slab.
pub const CHROME_EDGE: usize = 38;
/// The chrome's second line (65 %) and a quiet heart on it (75 %).
pub const CHROME_CONTENT_65: usize = 39;
pub const CHROME_CONTENT_75: usize = 40;
/// Where the list fades out under the chrome: the page at 75 %.
pub const CHROME_FADE: usize = 41;

/// 1 when the status bar's icons should be dark (a light page), else 0.
pub const STATUS_LIGHT: usize = 42;
/// `f32` bits: 1 when the sleeve's blurred band takes the page's tint (a white, cream or black page), 0
/// when it keeps its own colours - a strength, so a page cross-fading between the two can mix them. The
/// three `f32`s after it scale the band's luminance per channel: grey for white, warmed for cream.
pub const BAND_TINT: usize = 43;
pub const BAND_KR: usize = 44;
pub const BAND_KG: usize = 45;
pub const BAND_KB: usize = 46;

/// The album page's artwork dissolve: the edge at 40 %, then half way to the page at 86 %.
pub const HERO_EDGE: usize = 47;
pub const HERO_MID: usize = 48;
/// The player page's calming floor: the page at 0, 22 and 75 %.
pub const FLOOR_0: usize = 49;
pub const FLOOR_22: usize = 50;
pub const FLOOR_75: usize = 51;

pub const LEN: usize = 52;

/// Entries that are not colours, and must not be mixed as colours.
pub const NOT_COLOURS: [usize; 6] = [PAPER, STATUS_LIGHT, BAND_TINT, BAND_KR, BAND_KG, BAND_KB];

/// Near-black of the prominent pill on paper, and the dark ink on a light accent.
const PILL_ON_PAPER: u32 = 0xFF1A_1A1A;
const INK_ON_LIGHT: u32 = 0xFF0D_0D0D;

/// The words on a fill of `fill`: white while it reads at 3:1 there (the large, bold words of a pill or a
/// button; WCAG's floor for them), else near-black. A pale accent - a grey or green taken from a quiet
/// cover - had white on it at about 2:1, which was asked for whenever its luminance was under a half.
pub fn ink_on(fill: u32) -> u32 {
    if calculate_contrast(WHITE, fill) >= 3.0 { WHITE } else { INK_ON_LIGHT }
}

/// A theme's own roles, for a page with no cover: what the platform's colour scheme says.
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

/// The look of a page tinted from its cover: [`crate::cover::CoverColours`]' colours in.
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

/// The look of a page in the theme's own colours (no cover): `edge` and `melt` are the background.
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

    // Continuous in the page's lightness, so a cross-fade onto white never flips a plate in one frame.
    let paper = ((luminance(bg) - 0.40) / 0.40).clamp(0.0, 1.0);
    out[PAPER] = paper.to_bits();
    out[PILL] = lerp(primary, PILL_ON_PAPER, paper);
    // Chosen against the pill as it is drawn, which leans to near-black on a light page: mixed from two
    // inks instead, it could land on a grey on a grey.
    out[PILL_INK] = ink_on(out[PILL]);
    out[PILL_PLATE] = veil(on, 0.12 + 0.05 * paper, bg);
    out[TINT_INK] = lerp(primary, on, paper);
    out[CIRCLE_SELECTED] = lerp(veil(primary, 0.28, bg), veil(on, 0.14, bg), paper);
    // Lighter on paper than on a dark page: at 16 % black the discs read as grey slabs on white.
    out[CIRCLE_PLATE] = veil(on, 0.12 - 0.04 * paper, bg);
    // The title-row discs sit on the sleeve, which can be any brightness: they bring their own contrast.
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

    // The chrome: neutral, lifted off the page by a little of the text colour - more on a dark page,
    // where a ninth left the slab indistinguishable from AMOLED black. A tinted page lends the slab its
    // cover's edge first, so it still belongs to the record.
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
    // A white, cream or black page has a wash in its own tint only, and the blurred band has to arrive
    // at the same thing: it keeps its light and dark but takes the page's tint.
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

/// What AMOLED black puts in place of a dark scheme's surfaces, in this order: background, surface,
/// surface dim, container lowest, low, container, high, highest. Those pixels are simply off.
pub const AMOLED: [u32; 8] = [BLACK, BLACK, BLACK, BLACK, 0xFF0A_0A0A, 0xFF11_1111, 0xFF18_1818, 0xFF20_2020];

/// A page part way from look `a` to look `b`, the way the player's page cross-fades between records:
/// colours mixed as Compose mixes them (in Oklab), the strengths and tints as plain numbers, and the
/// status bar's icons turning over half way. Into `out`, so a frame of a fade allocates nothing.
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
    fn a_mix_moves_colours_and_numbers_and_turns_the_icons_half_way() {
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

    /// Worked out by the Compose code this replaces (ui-graphics 1.12 on the JVM), for a few pages:
    /// edge, background, on, accent, then the table's colour entries.
    const PAGES: &[(u32, u32, u32, u32, [u32; 18])] = include!("dress_pages.in");

    #[test]
    fn the_tables_are_what_compose_worked_out() {
        for &(edge, bg, on, accent, want) in PAGES {
            let t = page(edge, bg, on, accent, edge);
            let got = [
                t[ON_VARIANT], t[ON_PRIMARY], t[SURFACE_VARIANT], t[SURFACE_CONTAINER], t[SURFACE_CONTAINER_HIGH], t[SECONDARY_CONTAINER],
                t[PILL], t[PILL_INK], t[PILL_PLATE], t[TINT_INK], t[CIRCLE_SELECTED], t[CIRCLE_PLATE], t[DISC], t[DISC_INK_SELECTED],
                t[CHROME_SLAB], t[CHROME_EDGE], t[HERO_MID], t[FIELD],
            ];
            assert_eq!(got, want, "page {bg:08x} on {on:08x}, accent {accent:08x}");
        }
    }

    #[test]
    fn the_words_on_a_pill_read_on_it_whatever_the_cover() {
        // Every accent and page lightness a cover can give: the pill's words at 3:1 at least, and white
        // on a saturated accent as before.
        for accent in [0xFFA0_A0A0u32, 0xFFA8_B8A0, 0xFF60_6060, 0xFFE0_4040, 0xFF30_40C0, 0xFFF0_E080, 0xFF2C_8885, 0xFFFF_FFFF] {
            for bg in [0xFF10_1010u32, 0xFF55_5555, 0xFF3A_4060, 0xFF90_9090, 0xFFB0_A080, WHITE] {
                let t = page(bg, bg, if luminance(bg) < 0.5 { WHITE } else { 0xFF11_1111 }, accent, bg);
                let c = calculate_contrast(t[PILL_INK], t[PILL]);
                assert!(c >= 3.0, "pill {:08x} with {:08x} reads at {c:.2} on page {bg:08x}", t[PILL], t[PILL_INK]);
            }
        }
        assert_eq!(ink_on(0xFFE0_4040), WHITE, "white on a red accent");
        assert_eq!(ink_on(0xFFA0_A0A0), INK_ON_LIGHT, "near-black on a pale grey one");
    }

    #[test]
    fn paper_leans_with_the_page() {
        let dark = page(0xFF20_1010, 0xFF20_1010, WHITE, 0xFFE0_4040, 0xFF20_1010);
        let white = page(WHITE, WHITE, 0xFF11_1111, 0xFFE0_4040, WHITE);
        assert_eq!(f32::from_bits(dark[PAPER]), 0.0);
        assert_eq!(f32::from_bits(white[PAPER]), 1.0);
        assert_eq!(white[PILL], PILL_ON_PAPER, "Play goes near-black on paper");
        assert_eq!(dark[PILL], 0xFFE0_4040, "and is the accent on a dark page");
        assert_eq!((dark[STATUS_LIGHT], white[STATUS_LIGHT]), (0, 1));
        assert_eq!((f32::from_bits(dark[BAND_TINT]), f32::from_bits(white[BAND_TINT])), (0.0, 1.0));
        assert_eq!(f32::from_bits(white[BAND_KG]), 1.0);
    }
}
