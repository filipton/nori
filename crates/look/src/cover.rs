//! The colours a page takes from its cover. Ported line for line from the Android app, where every
//! rule below was tuned by eye against real records (named where it matters), so each one is kept
//! with the reason it exists.
//!
//! The seam is the whole trick. A page tinted with the cover's *dominant* colour still shows a line
//! where the picture ends, because the bottom of a picture is rarely its dominant colour. So the melt
//! starts from the average of the cover's bottom rows - the exact colour the last pixel row is - and
//! travels from there into a page colour deep or pale enough to carry text.

use crate::color::*;
use crate::palette;
use crate::random::JavaRandom;

/// How much of the sleeve's height melts into the page (the app's `MELT`).
pub const MELT: f32 = 0.19;

/// How many pixels a side the wash is worked out at. Sixteen held the colour but no shape at all, so
/// the page read as a plain field and the sleeve looked like it stopped dead; the record's forms are
/// what make the picture seem to carry on behind the words.
pub const WASH: usize = 32;

/// The size the wash is handed to the GPU at. Stretched straight from [`WASH`] over a whole screen,
/// each of its pixels became a visible step ("stairs"), and the melt, which reads it a row at a time,
/// stepped as well. Four times finer, filled in smoothly here once per cover, the steps are gone.
pub const WASH_OUT: usize = WASH * 4;

/// What a page wears. Colours are ARGB; the text's softer variant is `on` at 66 % opacity.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverColours {
    /// What the bottom of the cover is, so the picture can dissolve into the page without a seam.
    pub edge: u32,
    /// The page under it.
    pub background: u32,
    /// Text on the page.
    pub on: u32,
    pub accent: u32,
    /// The cover, blurred and pulled towards the page, [`WASH_OUT`] pixels a side; `None` on AMOLED
    /// black, which must not light pixels.
    pub wash: Option<Vec<u32>>,
    /// What the sleeve's soft bottom averages out to: the wash's last [`MELT`] of rows, which show
    /// through where the records are rubbed out. `edge` when there is no wash.
    pub wash_edge: u32,
}

/// The dark page for a record's dominant colour. It used to be that colour at a flat lightness of 0.20
/// whatever the record was, which turned a field of bright red into a dark maroon. What actually
/// limits it is the writing on it, and colours reach that limit at very different lightnesses - a blue
/// at 0.34 is dimmer to the eye than a yellow at 0.20. So the page keeps the record's own lightness and
/// is only taken down as far as the text needs: white has at least eight to one on it. A record darker
/// than that stays darker; nothing is brightened to meet a floor - the Black Album's page is black.
const PAGE_LIGHTEST: f32 = 0.34;
const PAGE_MAX_LUMA: f32 = 0.081;

fn dark_page(hsl: [f32; 3], max_luma: f32) -> u32 {
    let sat = (hsl[1] * 0.95).min(0.62);
    let mut lo = 0.04f32;
    let mut hi = hsl[2].clamp(0.04, PAGE_LIGHTEST);
    let at = |l: f32| hsl_to_color([hsl[0], sat, l]);
    if luminance(at(hi)) <= max_luma {
        return at(hi);
    }
    // Twelve halvings put it within a thousandth of the brightest this colour may be.
    for _ in 0..12 {
        let mid = (lo + hi) / 2.0;
        if luminance(at(mid)) <= max_luma {
            lo = mid
        } else {
            hi = mid
        }
    }
    at(lo)
}

/// How far a dark page is taken down towards a solid dark foot: a third of the way from the foot's own
/// luminance up to what the page would otherwise be. Enough that the melt out of the foot is a change
/// of colour rather than of light - which is what melts - while the page still wears the record's hue.
const FOOT_PULL: f32 = 0.35;

/// Contrast between the foot and the page past which the melt reads as a band and not as a fade.
const FOOT_JUMP: f64 = 1.8;

/// A light sleeve's page: its own colour at its own lightness, lifted only as far as dark text needs -
/// the mirror of [`dark_page`]. Pushed to 92-96 % lightness whatever the sleeve was, The Bravery's
/// yellow foot bleached to near white and Dire Straits' cream to a paler cream than the sleeve.
const PAGE_MIN_LUMA: f32 = 0.42;

fn paper_page(hsl: [f32; 3]) -> u32 {
    let sat = hsl[1].min(0.75);
    let at = |l: f32| hsl_to_color([hsl[0], sat, l]);
    let mut lo = hsl[2].clamp(0.5, 0.96);
    if luminance(at(lo)) >= PAGE_MIN_LUMA {
        return at(lo);
    }
    let mut hi = 0.96f32;
    // Twelve halvings: within a thousandth of the darkest this colour may be under dark text.
    for _ in 0..12 {
        let mid = (lo + hi) / 2.0;
        if luminance(at(mid)) >= PAGE_MIN_LUMA {
            hi = mid
        } else {
            lo = mid
        }
    }
    at(hi)
}

/// A black sleeve's page: its own black, deep enough for white text, keeping whatever hue it has.
fn ink_page(hsl: [f32; 3]) -> u32 {
    hsl_to_color([hsl[0], hsl[1].min(0.35), hsl[2].clamp(0.03, 0.06)])
}

/// Writes into `out` (cleared first) the key a cover's colours are kept under: [`derive`] answers
/// differently for the same picture in a dark or light theme and on AMOLED black, so all three are part of
/// it, and the picture is named by its address. `"<url>|<dark>|<amoled>"`, as the Android app keys them.
///
/// Twin of `paletteKey` (app/.../ui/CoverColors.kt), which Android keeps (string work where the cover is
/// composed).
pub fn palette_key(out: &mut String, url: &str, dark: bool, amoled: bool) {
    out.clear();
    out.push_str(url);
    out.push_str(if dark { "|true" } else { "|false" });
    out.push_str(if amoled { "|true" } else { "|false" });
}

/// The page for a cover of `w` x `h` ARGB pixels (as decoded for a list row), for a dark or light
/// theme, and on AMOLED black.
pub fn derive(pixels: &[u32], w: usize, h: usize, dark: bool, amoled: bool) -> CoverColours {
    let foot = bottom_average(pixels, w, h);
    let edge_raw = foot.colour;
    let p = palette::generate(pixels, w, h, 16);
    let body = dominant(pixels, w, h, edge_raw);
    let body_hsl = color_to_hsl(body);
    // Paper / ink: the sleeve is itself. Do not let Palette's "vibrant" JPEG fringe invent pink.
    let sleeve_paper = body_hsl[2] > 0.85 || (body_hsl[1] < 0.10 && body_hsl[2] > 0.72);
    // The colour there is most of is not always the page's colour. What the eye follows out of the
    // sleeve is its last rows, and when those are one solid dark strip - a black frame round a pale
    // record, like Demon Days - a light page under it melts black into white across the whole width.
    // So a solid near-black foot makes the page a dark one whatever the theme or the rest says.
    //
    // Not every strip is a foot, though. A thin one - under half the melt - with the sleeve's own
    // colour right above it is a frame: Amnesiac's black line under the red book. The melt rubs most of
    // it out and what goes soft is the red, so the page stays the red's. And it has to be a line: a
    // strip that stands out from what is over it. A Beautiful Lie ends on a few white rows under its red
    // lettering, and white on white is no frame.
    let above = color_to_hsl(foot.above);
    let frame = foot.strip < MELT / 2.0
        && calculate_contrast(foot.colour, foot.above) >= 1.6
        && calculate_contrast(foot.above, body) < 1.25
        && (body_hsl[1] < 0.15 || (((above[0] - body_hsl[0] + 540.0) % 360.0) - 180.0).abs() < 30.0);
    let solid = foot.solid && !frame;
    let edge_luma = luminance(edge_raw);
    let black_foot = solid && edge_luma < 0.03;
    // A light sleeve's page is its own light and a black sleeve's its own black - never one fixed white
    // or black for all of them (paper used to be #F7F7F7, so Dire Straits' and Rumours' cream came out
    // plain white). Only the lightness is set; hue and tint are the record's. And like a white sleeve
    // keeping a white page in dark mode, a black one keeps a black page in light mode.
    //
    // The mirror of a black foot: a solid light strip, a real part of the sleeve (8 % of its height or
    // more), under what would be a dark page. In Utero is cream from the middle down and Dreamland ends
    // in cloud; a dark page under them faded light into dark across the whole width.
    let body_ink = body_hsl[2] < 0.10 && body_hsl[1] < 0.18;
    let light_foot = solid && foot.strip >= 0.08 && edge_luma > 0.45 && !sleeve_paper && (dark || body_ink);
    // Paper keeps its tint only where there is one to see. A white sleeve averages out a few levels off
    // grey - JPEG noise, a scanner's cast, the grey of whatever is printed on it - and a whole screen of
    // page showed that as a tint the sleeve does not have. So the tint fades out below twelve levels and
    // is gone under four: Rumours' cream stays cream, and Dreamland's lavender cloud (ten) mostly stays.
    let paper_src = if light_foot { edge_raw } else { body };
    let mut paper_hsl = color_to_hsl(paper_src);
    paper_hsl[1] *= ((chroma(paper_src) - 4) as f32 / (GREY_CHROMA - 4) as f32).clamp(0.0, 1.0);
    let paper = (sleeve_paper && !black_foot) || light_foot;
    let ink = (body_ink && !light_foot) || (sleeve_paper && black_foot);
    let page_dark = (dark && !paper) || ink || black_foot;
    let ink_or_paper = paper || ink || body_hsl[1] < 0.12;
    let accent_seed = if ink_or_paper {
        body
    } else {
        p.vibrant.or(p.light_vibrant).or(p.light_muted).or(p.dominant).map_or(body, |s| s.rgb)
    };
    // A frame is rubbed out rather than melted from, so the fade starts from the colour just above it:
    // aimed at Amnesiac's black strip, the fade darkened the bottom of the red book into a dark band.
    let foot_colour = if frame { foot.above } else { edge_raw };
    let edge = if paper {
        blend_argb(foot_colour, paper_page(paper_hsl), 0.75)
    } else if ink {
        blend_argb(foot_colour, ink_page(body_hsl), 0.70)
    } else {
        foot_colour
    };
    let hsl = body_hsl;
    // A dark page over a solid foot much darker than it goes down towards the foot, keeping its own
    // hue: Elephant stays red and Demon Days slate, just deep enough that the strip fades into them
    // instead of stopping on a lighter colour. A busy foot, or one close to the page already, leaves
    // the page alone - that melt is the one that already looks right.
    let natural = (page_dark && !ink && !paper).then(|| dark_page(hsl, PAGE_MAX_LUMA));
    let foot_max = natural
        .filter(|&n| solid && calculate_contrast(n, edge_raw | 0xFF00_0000) >= FOOT_JUMP && edge_luma < luminance(n))
        .map(|n| edge_luma + (luminance(n) - edge_luma) * FOOT_PULL);
    let background = if dark && amoled {
        BLACK
    } else if paper {
        paper_page(paper_hsl)
    } else if ink {
        ink_page(if sleeve_paper && black_foot { color_to_hsl(edge_raw) } else { hsl })
    } else if page_dark {
        match foot_max {
            Some(m) => dark_page(hsl, m),
            None => natural.expect("a dark page that is neither ink nor paper"),
        }
    } else {
        hsl_to_color([hsl[0], (hsl[1] * 0.55).min(0.4), hsl[2].clamp(0.90, 0.96)])
    };
    let on = if luminance(background) < 0.4 { WHITE } else { 0xFF0D_0D0D };
    let accent = if paper {
        // Near-black ink on paper, in the paper's own hue: warm on cream, neutral on white.
        hsl_to_color([paper_hsl[0], paper_hsl[1].min(0.25), 0.17])
    } else {
        readable(accent_seed, background, on)
    };
    let wash = (!(amoled && dark)).then(|| wash_of(pixels, w, h, background, page_dark, paper, ink));
    CoverColours {
        edge,
        background,
        on,
        accent,
        wash_edge: wash.as_ref().map_or(edge, |w| w.1),
        wash: wash.map(|w| w.0),
    }
}

/// How far each pixel of the wash is pulled back towards the flat page colour. Two thirds of the way
/// left the page reading as one flat tint; the page should look like a blurred mirror of the record.
const MUTE: f32 = 0.38;

/// A player page is not one flat colour: the background is the artwork, enormously enlarged and
/// blurred, which is why it matches the cover so exactly. Done cheaply: the cover shrunk to [`WASH`]
/// pixels a side and smoothed once, then drawn stretched over the page, where the GPU's bilinear filter
/// does the enlarging for free. Every pixel is pulled to within a hair of the page colour's own
/// lightness and its saturation held back, so the hues vary but the contrast the text needs does not.
fn wash_of(pixels: &[u32], w: usize, h: usize, background: u32, dark: bool, paper: bool, ink: bool) -> (Vec<u32>, u32) {
    let ink_or_paper = paper || ink;
    let mut px = scale_bilinear(pixels, w, h, WASH, WASH);
    // Five passes: the cover has to become colour and light with no forms left in it at all; at three,
    // a strong shape near the middle of a record still arrived as a shape on the page.
    for _ in 0..5 {
        blur(&mut px);
    }
    let page_hsl = color_to_hsl(background);
    let mut mean_l = 0f32;
    for &p in &px {
        mean_l += color_to_hsl(p)[2];
    }
    mean_l /= px.len() as f32;
    // How far from the page colour a pixel may stray: wide enough for the record's own light and dark
    // to show through, narrow enough that white text never lands on a pale patch (at 0.11 the page's
    // own 0.20 reaches 0.31 at its brightest, where white still reads at about five to one). Paper keeps
    // the wash bright and grey - soft white variation, no chromatic bloom.
    let spread = if paper {
        0.04
    } else if dark {
        0.11
    } else {
        0.055
    };
    let pull = if dark && !paper { 1.0 } else { 0.85 };
    let max_sat = if ink_or_paper {
        0.04
    } else if dark {
        0.62
    } else {
        0.40
    };
    let mute = if ink_or_paper { 0.78 } else { MUTE };
    for p in px.iter_mut() {
        let mut hsl = color_to_hsl(*p);
        if ink_or_paper {
            // The wash wears the page's own tint - none on a white or black page, cream on a cream one -
            // so it varies in light only and never paints a hue the page has not got.
            hsl[0] = page_hsl[0];
            hsl[1] = page_hsl[1];
        } else {
            hsl[1] = (hsl[1] * pull).min(max_sat);
        }
        // On a black page the wash may lighten but never go under the page: a black sleeve's bottom rows
        // are darker than its average, and came out at pure black under a page of 0A - a darker line
        // where the sleeve melts, then the page again below it.
        let floor = if ink { page_hsl[2] } else { page_hsl[2] - spread };
        hsl[2] = (page_hsl[2] + (hsl[2] - mean_l) * spread * 2.5).max(floor).min(page_hsl[2] + spread).clamp(0.0, 1.0);
        // Then most of the way back to the flat page colour: on a record that is teal down one side and
        // warm down the other the page got teal and warm patches, and a patch reads as a fault where a
        // glow does not. Pulling back keeps the drift and takes the shouting out of it.
        *p = blend_argb(hsl_to_color(hsl), background, mute);
    }
    let first = ((WASH as f32 * (1.0 - MELT)) as usize).min(WASH - 1);
    let (mut r, mut g, mut b) = (0f32, 0f32, 0f32);
    for y in first..WASH {
        for x in 0..WASH {
            let p = px[y * WASH + x];
            r += red(p) as f32;
            g += green(p) as f32;
            b += blue(p) as f32;
        }
    }
    let n = ((WASH - first) * WASH * 255) as f32;
    (smooth(&px), from_floats(r / n, g / n, b / n))
}

/// `Bitmap.createScaledBitmap(..., filter = true)` as Android's Skia does it, measured against it
/// pixel for pixel: positions in 16.16 fixed point (the first of a row mapped from its centre, the
/// rest stepped by the truncated scale, so rounding builds up along the row as Skia's does), four bits
/// of sub-pixel weight, and the weighted sum truncated.
fn scale_bilinear(pixels: &[u32], w: usize, h: usize, dw: usize, dh: usize) -> Vec<u32> {
    let fixed = |v: f32| (v as f64 * 65536.0) as i32;
    let inv_x = 1.0f32 / (dw as f32 / w as f32);
    let inv_y = 1.0f32 / (dh as f32 / h as f32);
    let step = fixed(inv_x);
    let clamp = |i: i32, n: usize| i.clamp(0, n as i32 - 1) as usize;
    let mut out = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        let fy = fixed((y as f32 + 0.5) * inv_y) - 0x8000;
        let (y0, y1, ty) = (clamp(fy >> 16, h), clamp((fy >> 16) + 1, h), (fy >> 12) & 15);
        let mut fx = fixed(0.5 * inv_x) - 0x8000;
        for _ in 0..dw {
            let (x0, x1, tx) = (clamp(fx >> 16, w), clamp((fx >> 16) + 1, w), (fx >> 12) & 15);
            let (a, b, c, d) = (pixels[y0 * w + x0], pixels[y0 * w + x1], pixels[y1 * w + x0], pixels[y1 * w + x1]);
            let ch = |f: fn(u32) -> i32| (f(a) * (16 - tx) * (16 - ty) + f(b) * tx * (16 - ty) + f(c) * (16 - tx) * ty + f(d) * tx * ty) >> 8;
            out.push(rgb(ch(red), ch(green), ch(blue)));
            fx += step;
        }
    }
    out
}

/// [`WASH`] pixels a side up to [`WASH_OUT`]: bilinear, then two light box passes so the corners the
/// bilinear leaves between samples round off, then a dither of one level either way per channel. The
/// dither hides the banding eight bits give a slow dark gradient: without noise each level is a band.
fn smooth(px: &[u32]) -> Vec<u32> {
    let n = WASH_OUT;
    let (mut r, mut g, mut b) = (vec![0f32; n * n], vec![0f32; n * n], vec![0f32; n * n]);
    let scale = (WASH - 1) as f32 / (n - 1) as f32;
    for y in 0..n {
        let fy = y as f32 * scale;
        let y0 = (fy as usize).min(WASH - 2);
        let ty = fy - y0 as f32;
        for x in 0..n {
            let fx = x as f32 * scale;
            let x0 = (fx as usize).min(WASH - 2);
            let tx = fx - x0 as f32;
            let (a, bb, c, d) = (px[y0 * WASH + x0], px[y0 * WASH + x0 + 1], px[(y0 + 1) * WASH + x0], px[(y0 + 1) * WASH + x0 + 1]);
            let lerp = |f: fn(u32) -> i32| {
                (f(a) as f32 * (1.0 - tx) + f(bb) as f32 * tx) * (1.0 - ty) + (f(c) as f32 * (1.0 - tx) + f(d) as f32 * tx) * ty
            };
            let i = y * n + x;
            r[i] = lerp(red);
            g[i] = lerp(green);
            b[i] = lerp(blue);
        }
    }
    for _ in 0..2 {
        box_blur(&mut r, n);
        box_blur(&mut g, n);
        box_blur(&mut b, n);
    }
    // Fixed seed: the same cover gives the same texture every time, so reopening a page shows exactly
    // what it showed before.
    let mut noise = JavaRandom::new(0x5EED);
    let mut q = |v: f32| ((v + noise.next_float() - 0.5) as i32).clamp(0, 255);
    (0..n * n)
        .map(|i| {
            let (rr, gg) = (q(r[i]), q(g[i]));
            rgb(rr, gg, q(b[i]))
        })
        .collect()
}

/// A separable box blur of radius 2 over an n by n channel, in place.
fn box_blur(c: &mut [f32], n: usize) {
    let mut tmp = vec![0f32; c.len()];
    let at = |v: isize| v.clamp(0, n as isize - 1) as usize;
    for y in 0..n {
        for x in 0..n {
            let mut s = 0f32;
            for d in -2..=2isize {
                s += c[y * n + at(x as isize + d)];
            }
            tmp[y * n + x] = s / 5.0;
        }
    }
    for y in 0..n {
        for x in 0..n {
            let mut s = 0f32;
            for d in -2..=2isize {
                s += tmp[at(y as isize + d) * n + x];
            }
            c[y * n + x] = s / 5.0;
        }
    }
}

/// One separable 3-tap box pass over the tiny wash, so bilinear enlargement has no creases to show.
fn blur(px: &mut [u32]) {
    let mut out = vec![0u32; px.len()];
    let at = |v: isize| v.clamp(0, WASH as isize - 1) as usize;
    // Across `px` into `out`, then down `out` back into `px`.
    for (horizontal, from_px) in [(true, true), (false, false)] {
        for y in 0..WASH {
            for x in 0..WASH {
                let (mut r, mut g, mut b) = (0, 0, 0);
                for d in -1..=1isize {
                    let i = if horizontal { y * WASH + at(x as isize + d) } else { at(y as isize + d) * WASH + x };
                    let p = if from_px { px[i] } else { out[i] };
                    r += red(p);
                    g += green(p);
                    b += blue(p);
                }
                let v = rgb(r / 3, g / 3, b / 3);
                if from_px {
                    out[y * WASH + x] = v;
                } else {
                    px[y * WASH + x] = v;
                }
            }
        }
    }
}

/// Hue buckets the histogram counts into, then one for pale greys and whites, then one for black and
/// near-black. Black gets a bucket of its own at full weight: a sleeve that is mostly black is a black
/// record. White and pale grey stay discounted - a page that pale would take the controls with it.
const HUES: usize = 18;
const NEUTRAL: usize = HUES;
const DARK: usize = HUES + 1;

/// How far apart a pale pixel's channels must be (out of 255) before it counts as a colour and not as
/// paper. HSL saturation cannot say: it divides by how far the lightness is from white, so near white
/// it explodes - FEFDFD, one level off grey, is a third saturated. A sleeve's white paper, carrying a
/// few levels of JPEG chroma noise or a scanner's cast, was counted pixel by pixel as a vivid colour of
/// whatever hue the noise had, and the page took that hue: a white sleeve with a CD on it came out
/// blue, and Cage the Elephant's off-white paper joined the yellow of its splashes into an olive page.
/// Under twelve levels the eye sees white, whatever the hue works out to.
const GREY_CHROMA: i32 = 12;

fn chroma(c: u32) -> i32 {
    let (r, g, b) = (red(c), green(c), blue(c));
    r.max(g).max(b) - r.min(g).min(b)
}

/// The colour there is most of, which is not what Palette's dominant swatch answers: Palette drops
/// whole families on the way in (anything near white or black, the 10-37 degree band unless strongly
/// saturated), so on a field of pale dusty pink with dark hair down one side, the hair won. So count
/// pixels: each votes for its hue in twenty-degree buckets, the heaviest family wins, and the answer is
/// that family's weighted mean, so the page keeps the character of the field and not only its hue.
/// Nothing here rewards a colour for standing out - a small vivid mark loses to a large dull field.
fn dominant(pixels: &[u32], w: usize, h: usize, fallback: u32) -> u32 {
    if w == 0 || h == 0 {
        return fallback;
    }
    let row_step = (h / 64).max(1);
    let col_step = (w / 64).max(1);
    let mut weight = [0f32; HUES + 2];
    let (mut sum_r, mut sum_g, mut sum_b) = ([0f32; HUES + 2], [0f32; HUES + 2], [0f32; HUES + 2]);
    let (mut paper, mut ink, mut total) = (0f32, 0f32, 0f32);
    // Pixels of real colour per hue bucket, counted plainly, so a colour's share of the sleeve can be
    // set against black's or white's share on the same terms.
    let mut coloured = [0f32; HUES];
    // The same pixels as a grid, each cell holding its hue bucket (or -1): to tell a field from lines.
    let cols = w.div_ceil(col_step);
    let rows = h.div_ceil(row_step);
    let mut grid = vec![-1i32; cols * rows];
    let bucket_of = |hue: f32| ((hue / (360.0 / HUES as f32)) as i32).clamp(0, HUES as i32 - 1) as usize;
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let px = pixels[y * w + x];
            let hsl = color_to_hsl(px);
            total += 1.0;
            // Dark and colourless - or so dark that any hue it has is noise - is black.
            let black = hsl[2] < 0.06 || (hsl[2] < 0.18 && hsl[1] < 0.25);
            let white = hsl[2] > 0.88 && (hsl[1] < 0.18 || chroma(px) < GREY_CHROMA);
            if black {
                ink += 1.0;
            }
            if white {
                paper += 1.0;
            }
            if !black && !white && hsl[1] >= 0.25 {
                let hue = bucket_of(hsl[0]);
                coloured[hue] += 1.0;
                grid[(y / row_step) * cols + x / col_step] = hue as i32;
            }
            let bucket = if black {
                DARK
            } else if white || hsl[1] < 0.10 {
                NEUTRAL
            } else {
                bucket_of(hsl[0])
            };
            let wt = match bucket {
                // Paper and ink are the field on a white or black sleeve: full weight, so a speck of JPEG
                // pink cannot outvote the page.
                DARK => 1.0,
                NEUTRAL => {
                    if white {
                        1.0
                    } else {
                        0.35
                    }
                }
                _ => 0.25 + 0.75 * (hsl[1] / 0.25).min(1.0),
            };
            weight[bucket] += wt;
            sum_r[bucket] += wt * red(px) as f32;
            sum_g[bucket] += wt * green(px) as f32;
            sum_b[bucket] += wt * blue(px) as f32;
            x += col_step;
        }
        y += row_step;
    }
    // One colour to the eye is several buckets here: red from crimson to orange lands in three, and each
    // alone can lose to the black around it - how Amnesiac, half a field of red, got a black page. So a
    // hue is worth what its whole family covers. Black and the pale greys stand alone: lending them
    // neighbours would hand every dark record a black page. The Black Album (98 % black) still gets one.
    let mut score = [0f32; HUES + 2];
    for i in 0..HUES {
        score[i] = weight[i] + weight[(i + HUES - 1) % HUES] + weight[(i + 1) % HUES];
    }
    score[NEUTRAL] = weight[NEUTRAL];
    score[DARK] = weight[DARK];
    let mut best = 0;
    for i in 0..score.len() {
        if score[i] > score[best] {
            best = i;
        }
    }
    // Majority paper or ink wins outright - a white cover with a tiny coloured mark is still white - but
    // only over a mark. Amnesiac is 48 % black and 52 % one red book: a colour that covers a third of the
    // sleeve is the subject of the picture, and keeps its page unless black or white fill most of the rest.
    if total > 0.0 {
        let (mut family, mut family_share) = (0usize, 0f32);
        for i in 0..HUES {
            let share = (coloured[i] + coloured[(i + HUES - 1) % HUES] + coloured[(i + 1) % HUES]) / total;
            if share > family_share {
                family_share = share;
                family = i;
            }
        }
        // And a field, not lines: A Beautiful Lie is a third red too, but its red is lettering and rings
        // on white, and the eye takes the white for the sleeve. What counts is colour in solid blocks - a
        // cell whose eight neighbours are all the same colour.
        let in_family = |v: i32| v >= 0 && {
            let d = (v - family as i32).unsigned_abs() as usize;
            d.min(HUES - d) <= 1
        };
        let mut solid = 0;
        for gy in 1..rows.saturating_sub(1) {
            for gx in 1..cols.saturating_sub(1) {
                if !in_family(grid[gy * cols + gx]) {
                    continue;
                }
                let mut all = true;
                for dy in -1..=1isize {
                    for dx in -1..=1isize {
                        if !in_family(grid[(gy as isize + dy) as usize * cols + (gx as isize + dx) as usize]) {
                            all = false;
                        }
                    }
                }
                if all {
                    solid += 1;
                }
            }
        }
        let subject = family_share >= 0.30 && solid as f32 / total >= 0.18;
        // On paper the subject only keeps a page it won. A light sleeve's ground is what it is printed
        // on, and when the paper out-counts the colour as well, the eye takes the paper for the sleeve:
        // Villains is half off-white paper round a red devil, and handing the devil the page the paper
        // had won painted it red. A black field is different - Amnesiac's red book is the picture and
        // the black round it is not - so ink still gives way to a subject.
        if paper / total >= 0.45 {
            best = if subject && paper / total < 0.60 && best != NEUTRAL { family } else { NEUTRAL };
        }
        if ink / total >= 0.45 {
            best = if subject && ink / total < 0.60 { family } else { DARK };
        }
    }
    let run: &[usize] = if best < HUES { &[(best + HUES - 1) % HUES, best, (best + 1) % HUES] } else { std::slice::from_ref(&best) };
    let n = run.iter().map(|&i| weight[i] as f64).sum::<f64>() as f32;
    if n <= 0.0 {
        return fallback;
    }
    let mean = |s: &[f32; HUES + 2]| ((run.iter().map(|&i| s[i] as f64).sum::<f64>() / n as f64) as i32).clamp(0, 255);
    rgb(mean(&sum_r), mean(&sum_g), mean(&sum_b))
}

/// The colour of the cover's last rows, whether those rows are one solid strip (most pixels close to
/// that average) rather than a busy picture that merely averages out to something, how much of the
/// height the strip takes, and the average of the melt band's rows above it - what the eye sees going
/// soft once the strip is rubbed out.
struct Foot {
    colour: u32,
    solid: bool,
    strip: f32,
    above: u32,
}

fn bottom_average(pixels: &[u32], w: usize, h: usize) -> Foot {
    let rows = (h / 12).clamp(1, 12);
    let (mut r, mut g, mut b) = (0i64, 0i64, 0i64);
    for y in h - rows..h {
        for &px in &pixels[y * w..(y + 1) * w] {
            r += red(px) as i64;
            g += green(px) as i64;
            b += blue(px) as i64;
        }
    }
    let n = (w * rows).max(1) as i64;
    let (mr, mg, mb) = ((r / n) as i32, (g / n) as i32, (b / n) as i32);
    let mut close = 0;
    for y in h - rows..h {
        for &px in &pixels[y * w..(y + 1) * w] {
            let (dr, dg, db) = (red(px) - mr, green(px) - mg, blue(px) - mb);
            if dr * dr + dg * dg + db * db < 48 * 48 {
                close += 1;
            }
        }
    }
    let colour = rgb(mr, mg, mb);
    let near = |a: u32, b: u32| {
        let (dr, dg, db) = (red(a) - red(b), green(a) - green(b), blue(a) - blue(b));
        dr * dr + dg * dg + db * db < 48 * 48
    };
    let row_mean = |y: usize| {
        let (mut rr, mut gg, mut bb) = (0i64, 0i64, 0i64);
        for &px in &pixels[y * w..(y + 1) * w] {
            rr += red(px) as i64;
            gg += green(px) as i64;
            bb += blue(px) as i64;
        }
        rgb((rr / w as i64) as i32, (gg / w as i64) as i32, (bb / w as i64) as i32)
    };
    // Up from the bottom while the rows are still the strip.
    let mut top = h;
    while top > h / 2 && near(row_mean(top - 1), colour) {
        top -= 1;
    }
    let melt_top = (h as f32 * (1.0 - MELT)) as usize;
    let (mut ar, mut ag, mut ab, mut an) = (0i64, 0i64, 0i64, 0i64);
    for y in melt_top..top {
        let m = row_mean(y);
        ar += red(m) as i64;
        ag += green(m) as i64;
        ab += blue(m) as i64;
        an += 1;
    }
    let above = if an == 0 { colour } else { rgb((ar / an) as i32, (ag / an) as i32, (ab / an) as i32) };
    Foot { colour, solid: close as f32 >= n as f32 * 0.85, strip: (h - top) as f32 / h as f32, above }
}

/// Pushes a colour lighter or darker in its own hue until it has contrast against the page, or gives
/// `fallback` when no step of it does.
pub fn readable(color: u32, background: u32, fallback: u32) -> u32 {
    stepped(color, background, luminance(background) < 0.4).unwrap_or(fallback)
}

/// [`readable`] for a mark on a mid-tone surface (the tab bar's lifted slab), where the one way
/// [`readable`] goes runs out before it reads: tried the other way too before `fallback`. A pale accent
/// on a greyish bar can never be light enough, and comes out as a deeper shade of itself instead.
pub fn readable_either_way(color: u32, background: u32, fallback: u32) -> u32 {
    let light = luminance(background) < 0.4;
    stepped(color, background, light).or_else(|| stepped(color, background, !light)).unwrap_or(fallback)
}

fn stepped(color: u32, background: u32, towards_light: bool) -> Option<u32> {
    if calculate_contrast(color, background) >= 3.2 {
        return Some(color);
    }
    let mut hsl = color_to_hsl(color);
    for _ in 1..=8 {
        hsl[2] = if towards_light { (hsl[2] + 0.07).min(0.92) } else { (hsl[2] - 0.07).max(0.15) };
        hsl[1] = (hsl[1] * 1.05).min(1.0);
        let candidate = hsl_to_color(hsl);
        if calculate_contrast(candidate, background) >= 3.2 {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accent_is_moved_until_it_reads_on_another_background() {
        let (white, black, fallback) = (0xFFFF_FFFF, 0xFF12_1212, 0xFF67_50A4);
        assert_eq!(readable(0xFF1E_5AA0, white, fallback), 0xFF1E_5AA0, "already readable, kept");
        let on_white = readable(0xFFFF_E08A, white, fallback);
        let on_black = readable(0xFF20_1060, black, fallback);
        assert!(calculate_contrast(on_white, white) >= 3.2 && calculate_contrast(on_black, black) >= 3.2);
        assert_ne!((on_white, on_black), (fallback, fallback), "moved in its own hue, not given up");
    }

    #[test]
    fn a_pale_accent_on_a_greyish_bar_goes_deeper_instead_of_giving_up() {
        // Maroon 5's page: its jeans-blue accent on the bar lifted from its olive page.
        let (accent, bar, ink) = (0xFFA0_B0C8, 0xFF8A_8878, 0xFFFF_FFFF);
        assert_eq!(readable(accent, bar, ink), ink, "one way, it runs out");
        let got = readable_either_way(accent, bar, ink);
        assert!(got != ink && calculate_contrast(got, bar) >= 3.2 && luminance(got) < luminance(accent));
        let h = |c: u32| color_to_hsl(c)[0];
        assert!((h(got) - h(accent)).abs() < 12.0, "still its own blue");
        assert_eq!(readable_either_way(0xFF1E_5AA0, 0xFFFF_FFFF, ink), 0xFF1E_5AA0);
    }

    const S: usize = 160;

    fn solid(c: u32) -> Vec<u32> {
        vec![c; S * S]
    }

    /// `top` above, `bottom` for the last `rows` rows.
    fn footed(top: u32, bottom: u32, rows: usize) -> Vec<u32> {
        let mut v = solid(top);
        v[(S - rows) * S..].fill(bottom);
        v
    }

    #[test]
    fn a_black_record_keeps_a_black_page_in_either_theme() {
        for dark in [true, false] {
            let c = derive(&solid(0xFF08_0808), S, S, dark, false);
            assert!(luminance(c.background) < 0.01, "{:08x}", c.background);
            assert_eq!(c.on, WHITE);
        }
    }

    #[test]
    fn a_white_record_keeps_a_white_page_even_in_dark_mode() {
        let c = derive(&solid(0xFFF6_F6F4), S, S, true, false);
        assert!(luminance(c.background) > 0.8, "{:08x}", c.background);
        assert_eq!(c.on, 0xFF0D_0D0D);
    }

    #[test]
    fn a_cream_sleeve_stays_cream_and_a_yellow_one_yellow() {
        let cream = derive(&solid(0xFFEF_E4C8), S, S, false, false);
        let [h, s, _] = color_to_hsl(cream.background);
        assert!((35.0..55.0).contains(&h) && s > 0.2, "cream bleached to {:08x}", cream.background);
        // A coloured (not paper) sleeve in light mode gets a pale page in its own hue, not white.
        let yellow = derive(&solid(0xFFF2_D544), S, S, false, false);
        let [h, s, _] = color_to_hsl(yellow.background);
        assert!((40.0..60.0).contains(&h) && s > 0.3, "yellow bleached to {:08x}", yellow.background);
    }

    /// `base` with a few levels of JPEG-like noise per channel, leaning by `cast`.
    fn noisy(base: u32, cast: [i32; 3], noise: &mut JavaRandom) -> u32 {
        let mut ch = |v: i32, c: i32| (v + c + (noise.next_float() * 5.0) as i32 - 2).clamp(0, 255);
        rgb(ch(red(base), cast[0]), ch(green(base), cast[1]), ch(blue(base), cast[2]))
    }

    /// How far from grey a page is, in levels.
    fn tint(c: u32) -> i32 {
        red(c).max(green(c)).max(blue(c)) - red(c).min(green(c)).min(blue(c))
    }

    #[test]
    fn a_white_sleeve_with_a_disc_on_it_keeps_a_white_page() {
        // White paper with a faint cool cast and a silver CD in the middle with a bluish sheen. Each of
        // the paper's pixels is "a third saturated" to HSL, and they used to vote the page blue.
        let mut noise = JavaRandom::new(7);
        let c = S as f32 / 2.0;
        let v: Vec<u32> = (0..S * S)
            .map(|i| {
                let (x, y) = ((i % S) as f32 + 0.5 - c, (i / S) as f32 + 0.5 - c);
                let r = (x * x + y * y).sqrt() / S as f32;
                if r < 0.3 && r > 0.03 {
                    let sheen = (40.0 * (0.5 + 0.5 * (2.0 * (y.atan2(x) - 0.7)).cos())) as i32;
                    noisy(rgb(176 + sheen, 180 + sheen, 190 + sheen), [0, 0, 0], &mut noise)
                } else {
                    noisy(0xFFF6_F7FA, [-1, 0, 2], &mut noise)
                }
            })
            .collect();
        for dark in [true, false] {
            let p = derive(&v, S, S, dark, false);
            assert!(tint(p.background) <= 3 && luminance(p.background) > 0.7, "dark={dark}: {:08x}", p.background);
        }
    }

    #[test]
    fn off_white_paper_under_bright_splashes_is_paper_and_not_their_colour() {
        // Cage the Elephant: off-white paper, a few levels warm, with yellow and red splashes round it.
        // The paper and the yellow used to be averaged into one family and gave an olive page.
        let mut noise = JavaRandom::new(11);
        let v: Vec<u32> = (0..S * S)
            .map(|i| {
                let (x, y) = (i % S, i / S);
                let edge = x.min(y).min(S - 1 - x).min(S - 1 - y);
                match (edge < S / 12, (x / 8 + y / 8) % 3) {
                    (true, 0) => noisy(0xFFE8_D850, [0, 0, 0], &mut noise),
                    (true, 1) => noisy(0xFFE0_C890, [0, 0, 0], &mut noise),
                    (true, _) => noisy(0xFFD0_3048, [0, 0, 0], &mut noise),
                    _ => noisy(0xFFEE_EEE8, [0, 0, 0], &mut noise),
                }
            })
            .collect();
        for dark in [true, false] {
            let p = derive(&v, S, S, dark, false);
            assert!(tint(p.background) <= 3 && luminance(p.background) > 0.7, "dark={dark}: {:08x}", p.background);
        }
    }

    #[test]
    fn a_colour_on_paper_does_not_take_the_page_the_paper_won() {
        // Villains: off-white paper, a solid red devil over a third of it, a dark coat below.
        let mut noise = JavaRandom::new(3);
        let v: Vec<u32> = (0..S * S)
            .map(|i| {
                let (x, y) = (i % S, i / S);
                if x > S / 4 && y > S / 5 && x < S * 5 / 6 {
                    let c = if y > S * 4 / 5 { 0xFF1C_1D27 } else { 0xFFD2_4850 };
                    noisy(c, [0, 0, 0], &mut noise)
                } else {
                    noisy(0xFFF7_F8F1, [0, 0, 0], &mut noise)
                }
            })
            .collect();
        let p = derive(&v, S, S, true, false);
        assert!(luminance(p.background) > 0.7, "{:08x}", p.background);
    }

    #[test]
    fn a_red_field_gets_a_red_page_even_over_black() {
        // Amnesiac: half a red book, half black, with a thin black line at the foot.
        let mut v = solid(0xFF08_0808);
        for y in 0..S {
            for x in 0..S / 2 + 4 {
                v[y * S + x] = 0xFFB0_1818;
            }
        }
        v[(S - 3) * S..].fill(0xFF05_0505);
        let c = derive(&v, S, S, true, false);
        let [h, s, _] = color_to_hsl(c.background);
        assert!((h < 15.0 || h > 345.0) && s > 0.3, "not red: {:08x}", c.background);
    }

    #[test]
    fn a_solid_dark_foot_under_a_pale_sleeve_makes_a_dark_page() {
        // A black frame round a pale record (Demon Days): the page must not melt black into white.
        let c = derive(&footed(0xFFD8_D8D0, 0xFF06_0606, 40), S, S, false, false);
        assert!(luminance(c.background) < 0.1, "{:08x}", c.background);
    }

    #[test]
    fn the_edge_is_the_bottom_rows_colour() {
        let c = derive(&footed(0xFF30_60A0, 0xFF20_4070, 20), S, S, true, false);
        assert_eq!(c.edge, 0xFF20_4070);
    }

    #[test]
    fn text_always_reads_on_the_page() {
        for &c0 in &[0xFFE0_2020u32, 0xFF20_E020, 0xFF20_20E0, 0xFFE0_E020, 0xFF80_8080, 0xFF30_1060] {
            for dark in [true, false] {
                let c = derive(&solid(c0), S, S, dark, false);
                assert!(calculate_contrast(c.on, c.background) >= 4.5, "{c0:08x} dark={dark}: {:08x} on {:08x}", c.on, c.background);
            }
        }
    }

    #[test]
    fn amoled_dark_is_black_with_no_wash() {
        let c = derive(&solid(0xFF30_60A0), S, S, true, true);
        assert_eq!(c.background, BLACK);
        assert!(c.wash.is_none());
        assert_eq!(c.wash_edge, c.edge);
    }

    #[test]
    fn the_wash_is_the_size_the_gpu_gets_and_the_same_every_time() {
        let v: Vec<u32> = (0..S * S).map(|i| rgb((i % S) as i32, (i / S) as i32, 90)).collect();
        let a = derive(&v, S, S, true, false);
        let b = derive(&v, S, S, true, false);
        assert_eq!(a.wash.as_ref().map(Vec::len), Some(WASH_OUT * WASH_OUT));
        assert_eq!(a, b);
    }
}
