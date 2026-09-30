//! Gradient stops that dissolve artwork into its page, as fractions of the artwork's box. Colours come
//! from `dress`; the stops were tuned by eye against Apple Music's player.

use crate::cover::MELT;

/// A gradient stop: position and alpha, both 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub at: f32,
    pub alpha: f32,
}

const fn s(at: f32, alpha: f32) -> Stop {
    Stop { at, alpha }
}

/// Fade-out of the sleeve over its bottom band (the last [`MELT`]): eased, complete by 70 % so sharp
/// lines in the artwork near the bottom do not show through the blur below.
pub const RUB_OUT: [Stop; 7] = [s(0.0, 0.0), s(0.12, 0.31), s(0.25, 0.60), s(0.40, 0.86), s(0.55, 0.97), s(0.70, 1.0), s(1.0, 1.0)];

/// Opacity of the blurred copy over the sharp artwork, spanning [`SOFT_FROM`]..[`SOFT_TO`].
pub const SOFT: [Stop; 5] = [s(0.0, 0.0), s(0.3, 0.10), s(0.6, 0.50), s(0.85, 0.92), s(1.0, 1.0)];
/// Span of [`SOFT`] as fractions of the sleeve height.
pub const SOFT_FROM: f32 = 1.0 - MELT * 2.2;
pub const SOFT_TO: f32 = 1.0 - MELT * 0.55;

/// Black shade under the status bar (alpha at the top), gone at [`STATUS_SHADE_TO`] of the height, so
/// white icons read on pale covers.
pub const STATUS_SHADE: f32 = 0.30;
pub const STATUS_SHADE_TO: f32 = 0.16;

/// Album artwork dissolve: clear until `[0]`, then `HERO_EDGE`, `HERO_MID` and the page colour. Ends
/// on the page colour inside the artwork so parallax never exposes a seam.
pub const HERO_STOPS: [f32; 4] = [0.60, 0.76, 0.88, 1.0];

/// Player floor gradient stops for `FLOOR_0`, `FLOOR_22`, `FLOOR_75` and the page colour.
pub const FLOOR_STOPS: [f32; 4] = [0.0, 0.45, 0.80, 1.0];

/// Alpha mask fading the lyrics panel at both ends (clear before the source label).
pub const LYRICS_MASK: [Stop; 4] = [s(0.0, 0.0), s(0.05, 1.0), s(0.66, 1.0), s(0.92, 0.0)];

/// Whether a cover-tinted page renders black: AMOLED on and the page not set to keep cover colours.
pub fn page_black(amoled: bool, cover_colours: bool) -> bool {
    amoled && !cover_colours
}

/// Android `ColorMatrix` (4x5, row-major) tinting the blurred band: Rec. 709 luminance scaled per channel
/// by `k`, mixed in at `strength` (`dress::BAND_TINT`).
pub fn band_matrix(strength: f32, kr: f32, kg: f32, kb: f32) -> [f32; 20] {
    let (t, i) = (strength, 1.0 - strength);
    let row = |k: f32, own: usize| {
        let mut r = [t * 0.2126 * k, t * 0.7152 * k, t * 0.0722 * k, 0.0, 0.0];
        r[own] += i;
        r
    };
    let (r, g, b) = (row(kr, 0), row(kg, 1), row(kb, 2));
    let mut m = [0.0; 20];
    m[..5].copy_from_slice(&r);
    m[5..10].copy_from_slice(&g);
    m[10..15].copy_from_slice(&b);
    m[18] = 1.0;
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_matrix_mixes_luminance_tint() {
        // No tint: identity.
        let id = band_matrix(0.0, 1.0, 1.0, 1.0);
        assert_eq!(id, [1., 0., 0., 0., 0., 0., 1., 0., 0., 0., 0., 0., 1., 0., 0., 0., 0., 0., 1., 0.]);
        // Full tint on white: every channel is the luminance.
        let grey = band_matrix(1.0, 1.0, 1.0, 1.0);
        assert_eq!(&grey[..5], &[0.2126, 0.7152, 0.0722, 0.0, 0.0]);
        assert_eq!(&grey[5..10], &[0.2126, 0.7152, 0.0722, 0.0, 0.0]);
        let cream = band_matrix(1.0, 1.0, 0.9, 0.5);
        assert_eq!(cream[12], 0.5 * 0.0722);
        let half = band_matrix(0.5, 1.0, 1.0, 1.0);
        assert_eq!(half[0], 0.5 + 0.5 * 0.2126);
        assert_eq!(half[18], 1.0);
    }
}
