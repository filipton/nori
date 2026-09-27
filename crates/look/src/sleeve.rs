//! The pages' gradients: the stops of every gradient that dissolves a picture into its page. A
//! gradient's colours are the page's look (`dress`); these are where along it each colour sits and how
//! strongly, as shares of the picture they dissolve, whatever its box. All of it was tuned by eye on the
//! Android app against Apple's player (`w4` is the screenshot it was measured on) and is kept here with
//! the reasons, so another app draws the same page. The sleeve's box itself, how tall it is on a phone
//! held upright and how far it runs under the title, is the phone's layout (PlayerScreen.kt).

use crate::cover::MELT;

/// One stop of a gradient: where along it (0..1) and how opaque (0..1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub at: f32,
    pub alpha: f32,
}

const fn s(at: f32, alpha: f32) -> Stop {
    Stop { at, alpha }
}

/// The sleeve's soft bottom: how much of a record is rubbed out down the band (the last [`MELT`] of the
/// sleeve), in the melt's own easing - quick at first, then settling, and gone well before the record's
/// bottom edge. Rubbing out fades; it does not blur. A sharp line inside the band (Amnesiac's red book
/// ends on a thin black strip at 60 % of it) stays a line, only fainter, so the tail finishes by 70 % and
/// what lies below is the page's blur alone.
pub const RUB_OUT: [Stop; 7] = [s(0.0, 0.0), s(0.12, 0.31), s(0.25, 0.60), s(0.40, 0.86), s(0.55, 0.97), s(0.70, 1.0), s(1.0, 1.0)];

/// Where the blurred copy of the records shows over the sharp ones: nowhere above the band's upper reach,
/// all of it by the time the rub-out is under way; eased, so its own start is no line.
pub const SOFT: [Stop; 5] = [s(0.0, 0.0), s(0.3, 0.10), s(0.6, 0.50), s(0.85, 0.92), s(1.0, 1.0)];
/// The span of [`SOFT`], as shares of the sleeve's height from its top.
pub const SOFT_FROM: f32 = 1.0 - MELT * 2.2;
pub const SOFT_TO: f32 = 1.0 - MELT * 0.55;

/// Just enough shade under the status bar for white icons on a pale cover: black at this strength at the
/// top, gone at [`STATUS_SHADE_TO`] of the picture's height. The same on the album page and the sleeve.
pub const STATUS_SHADE: f32 = 0.30;
pub const STATUS_SHADE_TO: f32 = 0.16;

/// Where the album page's artwork dissolves: clear to [`HERO_STOPS`]`[0]`, then the look's `HERO_EDGE`,
/// `HERO_MID` and the page itself at the next three. The whole dissolve happens inside the artwork and
/// ends on the page colour, so there is nothing left to hand over to below it (the parallax slides the
/// picture over whatever is there, and a steep ramp squeezed by it reads as a line).
pub const HERO_STOPS: [f32; 4] = [0.60, 0.76, 0.88, 1.0];

/// The player page's calming floor below the sleeve: the look's `FLOOR_0`, `FLOOR_22`, `FLOOR_75` and the
/// page at these. The colours stay through the controls and settle into one only towards the bottom.
pub const FLOOR_STOPS: [f32; 4] = [0.0, 0.45, 0.80, 1.0];

/// How the lyrics fade out at both ends of their panel (a mask: the words go transparent, nothing is
/// painted over them), gone by the source label at the bottom so the two never sit on each other.
pub const LYRICS_MASK: [Stop; 4] = [s(0.0, 0.0), s(0.05, 1.0), s(0.66, 1.0), s(0.92, 0.0)];

/// Whether a page that can wear its cover (the player, an album's or a playlist's, an artist's) goes black: with AMOLED
/// black, unless that page is set to keep the cover's colours. The player keeps them out of the box - in
/// black, the page under the sleeve was pure black and the picture looked cut off, where Apple's carries
/// the record's colour down the whole screen; album, playlist and artist pages go black with the rest.
pub fn page_black(amoled: bool, cover_colours: bool) -> bool {
    amoled && !cover_colours
}

/// The colour matrix (4 x 5, row-major, as Android's `ColorMatrix` takes it) that gives the sleeve's
/// blurred band the page's tint on a white, cream or black page: the band's Rec. 709 luminance scaled per
/// channel by `k` (the page's colour over its brightest channel: grey for white, the same shading warmed
/// for cream), mixed in at `strength` (the look's `BAND_TINT`, which moves while a page cross-fades).
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

/// The key a [`band_matrix`] made for the look's band tint is kept under, so one is made per tint and not
/// per frame: `strength` and the three channel scales each kept to a 1/256th (finer than the blurred band
/// can show) and packed ten bits apiece. -1 when there is no tint (`strength` 0 or less): the plain blur,
/// with no matrix. Asked while the band is drawn, so it allocates nothing.
///
/// Twin of the key in `BandEffect.of` (app/.../ui/PlayerScreen.kt), which Android keeps: it is read in the
/// draw phase, where a crossing would cost more than the key.
pub fn band_key(strength: f32, kr: f32, kg: f32, kb: f32) -> i64 {
    if strength <= 0.0 {
        return -1;
    }
    let q = |x: f32| ((x * 256.0) as i64).clamp(0, 1023);
    (q(strength) << 30) | (q(kr) << 20) | (q(kg) << 10) | q(kb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_keeps_its_light_and_takes_the_pages_tint() {
        // No tint: the identity, the band's own colours.
        let id = band_matrix(0.0, 1.0, 1.0, 1.0);
        assert_eq!(id, [1., 0., 0., 0., 0., 0., 1., 0., 0., 0., 0., 0., 1., 0., 0., 0., 0., 0., 1., 0.]);
        // Full tint on white: every channel is the luminance, exactly as the Kotlin matrix had it.
        let grey = band_matrix(1.0, 1.0, 1.0, 1.0);
        assert_eq!(&grey[..5], &[0.2126, 0.7152, 0.0722, 0.0, 0.0]);
        assert_eq!(&grey[5..10], &[0.2126, 0.7152, 0.0722, 0.0, 0.0]);
        let cream = band_matrix(1.0, 1.0, 0.9, 0.5);
        assert_eq!(cream[12], 0.5 * 0.0722);
        let half = band_matrix(0.5, 1.0, 1.0, 1.0);
        assert_eq!(half[0], 0.5 + 0.5 * 0.2126);
        assert_eq!(half[18], 1.0);
    }

    #[test]
    fn the_stops_are_the_ones_the_app_drew() {
        assert_eq!(RUB_OUT[3], Stop { at: 0.40, alpha: 0.86 });
        assert!((SOFT_FROM - (1.0 - 0.19 * 2.2)).abs() < 1e-6);
        assert!((SOFT_TO - (1.0 - 0.19 * 0.55)).abs() < 1e-6);
        assert_eq!((HERO_STOPS[0], FLOOR_STOPS[1]), (0.60, 0.45));
        assert!(page_black(true, false) && !page_black(true, true) && !page_black(false, false));
    }
}
