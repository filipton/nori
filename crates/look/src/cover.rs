//! Page colours derived from a cover, ported from the Android app; rules were tuned by eye against real
//! albums (named where relevant).
//!
//! To avoid a seam where the artwork ends, the fade starts from the average of the cover's bottom rows
//! (`edge`), not its dominant colour, and ends on a page colour with enough contrast for text.

use crate::color::*;
use crate::palette;
use crate::random::JavaRandom;

/// Fraction of the sleeve's height that fades into the page.
pub const MELT: f32 = 0.19;

/// Side of the wash computation grid (16 lost all shape).
pub const WASH: usize = 32;

/// Side of the wash handed to the GPU: upsampled and smoothed here, since stretching [`WASH`] directly
/// showed visible steps.
pub const WASH_OUT: usize = WASH * 4;

/// Colours derived from a cover (ARGB).
#[derive(Debug, Clone, PartialEq)]
pub struct CoverColours {
    /// Colour at the cover's bottom, where the fade starts.
    pub edge: u32,
    /// Page background.
    pub background: u32,
    /// Text on the page.
    pub on: u32,
    pub accent: u32,
    /// Blurred cover muted towards the page, [`WASH_OUT`]² pixels; `None` on AMOLED black.
    pub wash: Option<Vec<u32>>,
    /// Average of the wash's bottom [`MELT`] rows (visible where the sleeve fades); `edge` without a wash.
    pub wash_edge: u32,
}

/// Dark page: the colour at its own lightness (at most [`PAGE_LIGHTEST`]), darkened only until its
/// luminance allows white text at about 8:1. Darker colours are never brightened.
const PAGE_LIGHTEST: f32 = 0.34;
const PAGE_MAX_LUMA: f32 = 0.081;

fn dark_page(hsl: [f32; 3], max_luma: f32) -> u32 {
    let sat = (hsl[1] * 0.95).min(0.62);
    let hi = hsl[2].clamp(0.04, PAGE_LIGHTEST);
    let at = |l: f32| hsl_to_color([hsl[0], sat, l]);
    if luminance(at(hi)) <= max_luma {
        return at(hi);
    }
    // The lightest allowed lightness.
    at(bisect(0.04, hi, |l| luminance(at(l)) <= max_luma))
}

/// Over a solid dark foot, the page's max luminance is this far from the foot's up to its natural one,
/// so the fade out of the foot changes hue more than lightness.
const FOOT_PULL: f32 = 0.35;

/// Foot/page contrast above which the fade reads as a band.
const FOOT_JUMP: f64 = 1.8;

/// Light page: the mirror of [`dark_page`], lightened only as far as dark text needs (a fixed 92-96 %
/// lightness bleached creams and yellows).
const PAGE_MIN_LUMA: f32 = 0.42;

fn paper_page(hsl: [f32; 3]) -> u32 {
    let sat = hsl[1].min(0.75);
    let at = |l: f32| hsl_to_color([hsl[0], sat, l]);
    let lo = hsl[2].clamp(0.5, 0.96);
    if luminance(at(lo)) >= PAGE_MIN_LUMA {
        return at(lo);
    }
    // The darkest allowed lightness.
    at(bisect(0.96, lo, |l| luminance(at(l)) >= PAGE_MIN_LUMA))
}

/// Binary search between a lightness that is `ok` and one that is not (12 steps, ~0.001): the last `ok`.
fn bisect(mut good: f32, mut bad: f32, ok: impl Fn(f32) -> bool) -> f32 {
    for _ in 0..12 {
        let mid = (good + bad) / 2.0;
        if ok(mid) {
            good = mid
        } else {
            bad = mid
        }
    }
    good
}

/// Black page keeping the sleeve's hue.
fn ink_page(hsl: [f32; 3]) -> u32 {
    hsl_to_color([hsl[0], hsl[1].min(0.35), hsl[2].clamp(0.03, 0.06)])
}

/// Derives page colours from `w` x `h` ARGB pixels (at least 1 x 1: every decoder rejects empty images).
pub fn derive(pixels: &[u32], w: usize, h: usize, dark: bool, amoled: bool) -> CoverColours {
    let foot = bottom_average(pixels, w, h);
    let edge_raw = foot.colour;
    let p = palette::generate(pixels, w, h, 16);
    let body = dominant(pixels, w, h, edge_raw);
    let body_hsl = color_to_hsl(body);
    // A white or pale grey sleeve is "paper": its page is its own colour, not a Palette swatch.
    let sleeve_paper = body_hsl[2] > 0.85 || (body_hsl[1] < 0.10 && body_hsl[2] > 0.72);
    // A solid near-black foot forces a dark page (Demon Days' black frame would otherwise fade black
    // into white). Exception: a "frame", a thin contrasting strip (under half the melt) with the body
    // colour above it (Amnesiac's black line under the red book); the fade starts above it instead.
    let above = color_to_hsl(foot.above);
    let frame = foot.strip < MELT / 2.0
        && calculate_contrast(foot.colour, foot.above) >= 1.6
        && calculate_contrast(foot.above, body) < 1.25
        && (body_hsl[1] < 0.15 || (((above[0] - body_hsl[0] + 540.0) % 360.0) - 180.0).abs() < 30.0);
    let solid = foot.solid && !frame;
    let edge_luma = luminance(edge_raw);
    let black_foot = solid && edge_luma < 0.03;
    // Paper and ink sleeves keep their own light or black page (with their tint) in either theme.
    // Mirror of the black foot: a solid light foot (>= 8 % of the height) under a would-be dark page
    // makes a paper page (In Utero, Dreamland).
    let body_ink = body_hsl[2] < 0.10 && body_hsl[1] < 0.18;
    let light_foot = solid && foot.strip >= 0.08 && edge_luma > 0.45 && !sleeve_paper && (dark || body_ink);
    // Paper tint fades out below GREY_CHROMA levels of chroma and is gone under 4, so JPEG noise or a
    // scanner cast does not tint the page while real creams stay.
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
    // Fade from the colour above a frame, not the frame itself.
    let foot_colour = if frame { foot.above } else { edge_raw };
    let edge = if paper {
        blend_argb(foot_colour, paper_page(paper_hsl), 0.75)
    } else if ink {
        blend_argb(foot_colour, ink_page(body_hsl), 0.70)
    } else {
        foot_colour
    };
    let hsl = body_hsl;
    // Over a solid foot much darker than the page, darken the page towards it (keeping the hue) so the
    // strip fades in instead of ending on a lighter colour.
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
        // Near-black in the paper's hue.
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

/// Blend of each wash pixel towards the flat page colour (2/3 looked flat).
const MUTE: f32 = 0.38;

/// The page wash: the cover downscaled to [`WASH`], heavily blurred, with lightness held near the page's
/// and saturation capped so text contrast holds. Returns the upsampled wash and its bottom average.
fn wash_of(pixels: &[u32], w: usize, h: usize, background: u32, dark: bool, paper: bool, ink: bool) -> (Vec<u32>, u32) {
    let ink_or_paper = paper || ink;
    let mut px = scale_bilinear(pixels, w, h, WASH, WASH);
    // Five passes: at three, strong central shapes still showed.
    for _ in 0..5 {
        blur(&mut px);
    }
    let page_hsl = color_to_hsl(background);
    let mut mean_l = 0f32;
    for &p in &px {
        mean_l += color_to_hsl(p)[2];
    }
    mean_l /= px.len() as f32;
    // Max lightness deviation from the page (0.11 keeps white text at about 5:1 on dark pages).
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
            // Only lightness varies; hue and saturation are the page's.
            hsl[0] = page_hsl[0];
            hsl[1] = page_hsl[1];
        } else {
            hsl[1] = (hsl[1] * pull).min(max_sat);
        }
        // On ink pages never go darker than the page (it showed as a darker band under the sleeve).
        let floor = if ink { page_hsl[2] } else { page_hsl[2] - spread };
        hsl[2] = (page_hsl[2] + (hsl[2] - mean_l) * spread * 2.5).max(floor).min(page_hsl[2] + spread).clamp(0.0, 1.0);
        // Mostly back to the page colour so distinct hue regions read as a glow, not patches.
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

/// Skia's `Bitmap.createScaledBitmap(..., filter = true)`, pixel-exact: 16.16 fixed-point positions
/// stepped along each row (accumulating rounding like Skia), 4-bit weights, truncated sum.
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

/// Upsamples [`WASH`] to [`WASH_OUT`]: bilinear, two box passes, then ±0.5 level dither against banding.
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
    // Fixed seed: deterministic texture per cover.
    let mut noise = JavaRandom::new(0x5EED);
    let mut q = |v: f32| ((v + noise.next_float() - 0.5) as i32).clamp(0, 255);
    (0..n * n).map(|i| rgb(q(r[i]), q(g[i]), q(b[i]))).collect()
}

/// Separable radius-2 box blur of an n x n channel, in place.
fn box_blur(c: &mut [f32], n: usize) {
    let mut tmp = vec![0f32; c.len()];
    let pass = |src: &[f32], dst: &mut [f32], across: bool| {
        for y in 0..n {
            for x in 0..n {
                let at = |d: isize| {
                    let v = ((if across { x } else { y }) as isize + d).clamp(0, n as isize - 1) as usize;
                    src[if across { y * n + v } else { v * n + x }]
                };
                dst[y * n + x] = (-2..=2).map(at).sum::<f32>() / 5.0;
            }
        }
    };
    pass(c, &mut tmp, true);
    pass(&tmp, c, false);
}

/// Separable 3-tap box blur of the [`WASH`] grid: horizontally into a buffer, vertically back.
fn blur(px: &mut [u32]) {
    let mut out = vec![0u32; px.len()];
    let at = |v: isize| v.clamp(0, WASH as isize - 1) as usize;
    for horizontal in [true, false] {
        for y in 0..WASH {
            for x in 0..WASH {
                let (mut r, mut g, mut b) = (0, 0, 0);
                for d in -1..=1isize {
                    let i = if horizontal { y * WASH + at(x as isize + d) } else { at(y as isize + d) * WASH + x };
                    let p = if horizontal { px[i] } else { out[i] };
                    r += red(p);
                    g += green(p);
                    b += blue(p);
                }
                let v = rgb(r / 3, g / 3, b / 3);
                if horizontal {
                    out[y * WASH + x] = v;
                } else {
                    px[y * WASH + x] = v;
                }
            }
        }
    }
}

/// Histogram buckets: `HUES` hue buckets, then neutral (greys, whites), then dark.
const HUES: usize = 18;
const NEUTRAL: usize = HUES;
const DARK: usize = HUES + 1;

/// Minimum channel spread (of 255) for a pale pixel to count as coloured. HSL saturation is useless near
/// white (FEFDFD is 33 % saturated), so JPEG noise on white paper used to vote for a hue.
const GREY_CHROMA: i32 = 12;

fn chroma(c: u32) -> i32 {
    let (r, g, b) = (red(c), green(c), blue(c));
    r.max(g).max(b) - r.min(g).min(b)
}

/// The colour covering most of the cover (Palette's dominant filters out whole families). Sampled pixels
/// vote into 20° hue buckets (plus neutral and dark); the heaviest bucket with its neighbours wins, and
/// the result is that family's weighted mean. Large dull fields beat small vivid marks.
fn dominant(pixels: &[u32], w: usize, h: usize, fallback: u32) -> u32 {
    if w == 0 || h == 0 {
        return fallback;
    }
    let row_step = (h / 64).max(1);
    let col_step = (w / 64).max(1);
    let mut weight = [0f32; HUES + 2];
    let (mut sum_r, mut sum_g, mut sum_b) = ([0f32; HUES + 2], [0f32; HUES + 2], [0f32; HUES + 2]);
    let (mut paper, mut ink, mut total) = (0f32, 0f32, 0f32);
    // Unweighted coloured-pixel counts per hue, comparable with the paper and ink counts.
    let mut coloured = [0f32; HUES];
    // Hue bucket per sampled cell (-1: none), to tell solid fields from lines.
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
                // Full weight so JPEG specks cannot outvote a white or black field.
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
    // A hue scores with its two neighbours (one perceived colour spans several buckets); neutral and dark
    // score alone.
    let mut score = [0f32; HUES + 2];
    for i in 0..HUES {
        score[i] = weight[i] + weight[(i + HUES - 1) % HUES] + weight[(i + 1) % HUES];
    }
    score[NEUTRAL] = weight[NEUTRAL];
    score[DARK] = weight[DARK];
    let mut best = (0..score.len()).fold(0, |best, i| if score[i] > score[best] { i } else { best });
    // Majority paper or ink wins unless a colour family is the subject: >= 30 % of the cover and mostly
    // solid blocks (Amnesiac: 48 % black, 52 % red book).
    if total > 0.0 {
        let (mut family, mut family_share) = (0usize, 0f32);
        for i in 0..HUES {
            let share = (coloured[i] + coloured[(i + HUES - 1) % HUES] + coloured[(i + 1) % HUES]) / total;
            if share > family_share {
                family_share = share;
                family = i;
            }
        }
        // Solid cells: all eight neighbours in the family (lettering and rings do not count).
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
                let mut around = (gy - 1..=gy + 1).flat_map(|y| (gx - 1..=gx + 1).map(move |x| y * cols + x));
                if around.all(|at| in_family(grid[at])) {
                    solid += 1;
                }
            }
        }
        let subject = family_share >= 0.30 && solid as f32 / total >= 0.18;
        // Paper yields to a subject only if the subject already won the score (Villains stays paper);
        // ink always yields to a subject.
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

/// The cover's bottom rows.
struct Foot {
    /// Average colour of the bottom rows.
    colour: u32,
    /// Whether at least 85 % of those pixels are close to the average.
    solid: bool,
    /// Height of the uniform strip, as a fraction of the cover.
    strip: f32,
    /// Average of the melt band's rows above the strip.
    above: u32,
}

fn bottom_average(pixels: &[u32], w: usize, h: usize) -> Foot {
    let rows = (h / 12).clamp(1, 12);
    let foot = &pixels[(h - rows) * w..h * w];
    let colour = mean_rgb(foot.iter().copied()).expect("covers are at least 1 x 1");
    let near = |a: u32, b: u32| {
        let (dr, dg, db) = (red(a) - red(b), green(a) - green(b), blue(a) - blue(b));
        dr * dr + dg * dg + db * db < 48 * 48
    };
    let close = foot.iter().filter(|&&px| near(px, colour)).count();
    let row_mean = |y: usize| mean_rgb(pixels[y * w..(y + 1) * w].iter().copied()).expect("covers are at least 1 x 1");
    // Walk up while rows still match the strip.
    let mut top = h;
    while top > h / 2 && near(row_mean(top - 1), colour) {
        top -= 1;
    }
    let melt_top = (h as f32 * (1.0 - MELT)) as usize;
    let above = mean_rgb((melt_top..top).map(row_mean)).unwrap_or(colour);
    Foot { colour, solid: close as f32 >= foot.len() as f32 * 0.85, strip: (h - top) as f32 / h as f32, above }
}

/// The mean colour of `px`, each channel's sum divided down; None for no pixels.
fn mean_rgb(px: impl Iterator<Item = u32>) -> Option<u32> {
    let (mut r, mut g, mut b, mut n) = (0i64, 0i64, 0i64, 0i64);
    for p in px {
        (r, g, b, n) = (r + red(p) as i64, g + green(p) as i64, b + blue(p) as i64, n + 1);
    }
    (n > 0).then(|| rgb((r / n) as i32, (g / n) as i32, (b / n) as i32))
}

/// Steps `color`'s lightness (away from the background's) until it reaches 3.2:1 contrast, else
/// `fallback`.
pub fn readable(color: u32, background: u32, fallback: u32) -> u32 {
    stepped(color, background, luminance(background) < 0.4).unwrap_or(fallback)
}

/// [`readable`] that also tries the opposite direction before `fallback`, for mid-tone surfaces.
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
    fn readable_text() {
        let (white, black, fallback) = (0xFFFF_FFFF, 0xFF12_1212, 0xFF67_50A4);
        assert_eq!(readable(0xFF1E_5AA0, white, fallback), 0xFF1E_5AA0);
        let on_white = readable(0xFFFF_E08A, white, fallback);
        let on_black = readable(0xFF20_1060, black, fallback);
        assert!(calculate_contrast(on_white, white) >= 3.2 && calculate_contrast(on_black, black) >= 3.2);
        assert!(on_white != fallback && on_black != fallback);

        // Readable either way tries darker.
        // Pale blue accent on a mid-grey bar.
        let (accent, bar, ink) = (0xFFA0_B0C8, 0xFF8A_8878, 0xFFFF_FFFF);
        assert_eq!(readable(accent, bar, ink), ink);
        let got = readable_either_way(accent, bar, ink);
        assert!(got != ink && calculate_contrast(got, bar) >= 3.2 && luminance(got) < luminance(accent));
        let h = |c: u32| color_to_hsl(c)[0];
        assert!((h(got) - h(accent)).abs() < 12.0, "hue kept");
        assert_eq!(readable_either_way(0xFF1E_5AA0, 0xFFFF_FFFF, ink), 0xFF1E_5AA0);

        // Text contrast on page.
        for &c0 in &[0xFFE0_2020u32, 0xFF20_E020, 0xFF20_20E0, 0xFFE0_E020, 0xFF80_8080, 0xFF30_1060] {
            for dark in [true, false] {
                let c = derive(&solid(c0), S, S, dark, false);
                assert!(calculate_contrast(c.on, c.background) >= 4.5, "{c0:08x} dark={dark}: {:08x} on {:08x}", c.on, c.background);
            }
        }
    }

    const S: usize = 160;

    fn solid(c: u32) -> Vec<u32> {
        vec![c; S * S]
    }

    /// `top` colour with the last `rows` rows `bottom`.
    fn footed(top: u32, bottom: u32, rows: usize) -> Vec<u32> {
        let mut v = solid(top);
        v[(S - rows) * S..].fill(bottom);
        v
    }

    #[test]
    fn plain_covers() {
        for dark in [true, false] {
            let c = derive(&solid(0xFF08_0808), S, S, dark, false);
            assert!(luminance(c.background) < 0.01, "{:08x}", c.background);
            assert_eq!(c.on, WHITE);
        }

        // White cover gets white page in dark theme.
        let c = derive(&solid(0xFFF6_F6F4), S, S, true, false);
        assert!(luminance(c.background) > 0.8, "{:08x}", c.background);
        assert_eq!(c.on, 0xFF0D_0D0D);

        // Cream and yellow keep their hue.
        let cream = derive(&solid(0xFFEF_E4C8), S, S, false, false);
        let [h, s, _] = color_to_hsl(cream.background);
        assert!((35.0..55.0).contains(&h) && s > 0.2, "cream bleached to {:08x}", cream.background);
        let yellow = derive(&solid(0xFFF2_D544), S, S, false, false);
        let [h, s, _] = color_to_hsl(yellow.background);
        assert!((40.0..60.0).contains(&h) && s > 0.3, "yellow bleached to {:08x}", yellow.background);

        // Amoled dark is black with no wash.
        let c = derive(&solid(0xFF30_60A0), S, S, true, true);
        assert_eq!(c.background, BLACK);
        assert!(c.wash.is_none());
        assert_eq!(c.wash_edge, c.edge);
    }

    /// `base` plus ±2 levels of noise per channel, offset by `cast`.
    fn noisy(base: u32, cast: [i32; 3], noise: &mut JavaRandom) -> u32 {
        let mut ch = |v: i32, c: i32| (v + c + (noise.next_float() * 5.0) as i32 - 2).clamp(0, 255);
        rgb(ch(red(base), cast[0]), ch(green(base), cast[1]), ch(blue(base), cast[2]))
    }


    #[test]
    fn white_paper_covers() {
        // Regression: noisy white paper with a bluish CD voted the page blue.
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
            assert!(chroma(p.background) <= 3 && luminance(p.background) > 0.7, "dark={dark}: {:08x}", p.background);
        }

        // Off white paper with splashes stays white.
        // Regression (Cage the Elephant): off-white paper and yellow splashes averaged into olive.
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
            assert!(chroma(p.background) <= 3 && luminance(p.background) > 0.7, "dark={dark}: {:08x}", p.background);
        }

        // Subject on paper does not take page.
        // Villains: off-white paper, a red figure over a third of it, dark coat below.
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
    fn dark_fields() {
        // Amnesiac: half red, half black, thin black line at the foot.
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

        // Solid dark foot forces dark page.
        let c = derive(&footed(0xFFD8_D8D0, 0xFF06_0606, 40), S, S, false, false);
        assert!(luminance(c.background) < 0.1, "{:08x}", c.background);

        // Edge is bottom rows colour.
        let c = derive(&footed(0xFF30_60A0, 0xFF20_4070, 20), S, S, true, false);
        assert_eq!(c.edge, 0xFF20_4070);
    }

    #[test]
    fn wash_size_and_determinism() {
        let v: Vec<u32> = (0..S * S).map(|i| rgb((i % S) as i32, (i / S) as i32, 90)).collect();
        let a = derive(&v, S, S, true, false);
        let b = derive(&v, S, S, true, false);
        assert_eq!(a.wash.as_ref().map(Vec::len), Some(WASH_OUT * WASH_OUT));
        assert_eq!(a, b);
    }
}
