//! Line-for-line port of AndroidX Palette 1.0.0 (Apache-2.0, The Android Open Source Project). The
//! accent was tuned against its swatches, so its quirks are kept: unfiltered 112 px downscale, 5-bit
//! colours, and Java `PriorityQueue` array order deciding ties.

use crate::color::{blue, color_to_hsl, green, red, rgb, rgb_to_hsl, round};

const RESIZE_AREA: usize = 112 * 112;
const WORD: u32 = 5;
const WORD_MASK: i32 = (1 << WORD) - 1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Swatch {
    pub rgb: u32,
    pub population: i32,
}

impl Swatch {
    fn hsl(&self) -> [f32; 3] {
        rgb_to_hsl(red(self.rgb), green(self.rgb), blue(self.rgb))
    }
}

/// Swatches picked by Palette's default targets, in pick order.
#[derive(Debug, Clone, Default)]
pub struct Palette {
    pub dominant: Option<Swatch>,
    pub light_vibrant: Option<Swatch>,
    pub vibrant: Option<Swatch>,
    pub dark_vibrant: Option<Swatch>,
    pub light_muted: Option<Swatch>,
    pub muted: Option<Swatch>,
    pub dark_muted: Option<Swatch>,
}

/// `Palette.from(bitmap).maximumColorCount(max_colors).generate()` over `w` x `h` ARGB pixels.
pub fn generate(pixels: &[u32], w: usize, h: usize, max_colors: usize) -> Palette {
    let scaled = scale_down(pixels, w, h);
    let swatches = quantize(scaled.as_deref().unwrap_or(pixels), max_colors);
    score(swatches)
}

/// `Palette.Builder.scaleBitmapDown`: above 112² pixels, nearest-neighbour downscale to that area, as
/// `Bitmap.createScaledBitmap(..., filter = false)` samples (centre rounded down; verified on device).
/// None when no scaling is needed.
fn scale_down(pixels: &[u32], w: usize, h: usize) -> Option<Vec<u32>> {
    let area = w * h;
    if area <= RESIZE_AREA {
        return None;
    }
    let ratio = (RESIZE_AREA as f64 / area as f64).sqrt();
    let (dw, dh) = ((w as f64 * ratio).ceil() as usize, (h as f64 * ratio).ceil() as usize);
    let at = |d: usize, dst: usize, src: usize| (((2 * d + 1) * src).saturating_sub(1) / (2 * dst)).min(src - 1);
    let mut out = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        let sy = at(y, dh, h);
        for x in 0..dw {
            out.push(pixels[sy * w + at(x, dw, w)]);
        }
    }
    Some(out)
}

fn quantized_red(c: i32) -> i32 {
    (c >> (WORD + WORD)) & WORD_MASK
}
fn quantized_green(c: i32) -> i32 {
    (c >> WORD) & WORD_MASK
}
fn quantized_blue(c: i32) -> i32 {
    c & WORD_MASK
}

fn modify_word_width(value: i32, current: u32, target: u32) -> i32 {
    let v = if target > current { value << (target - current) } else { value >> (current - target) };
    v & ((1 << target) - 1)
}

fn quantize_from_rgb888(c: u32) -> i32 {
    let (r, g, b) = (modify_word_width(red(c), 8, WORD), modify_word_width(green(c), 8, WORD), modify_word_width(blue(c), 8, WORD));
    (r << (WORD + WORD)) | (g << WORD) | b
}

fn approximate_to_rgb888(r: i32, g: i32, b: i32) -> u32 {
    rgb(modify_word_width(r, WORD, 8), modify_word_width(g, WORD, 8), modify_word_width(b, WORD, 8))
}

fn approximate(c: i32) -> u32 {
    approximate_to_rgb888(quantized_red(c), quantized_green(c), quantized_blue(c))
}

/// Palette's default filter: rejects near white, near black and the skin-tone "red I line".
fn allowed(hsl: [f32; 3]) -> bool {
    let white = hsl[2] >= 0.95;
    let black = hsl[2] <= 0.05;
    let red_i_line = hsl[0] >= 10.0 && hsl[0] <= 37.0 && hsl[1] <= 0.82;
    !white && !black && !red_i_line
}

#[derive(Debug, Clone)]
struct Vbox {
    lower: usize,
    upper: usize,
    population: i32,
    min_r: i32,
    max_r: i32,
    min_g: i32,
    max_g: i32,
    min_b: i32,
    max_b: i32,
}

impl Vbox {
    fn volume(&self) -> i32 {
        (self.max_r - self.min_r + 1) * (self.max_g - self.min_g + 1) * (self.max_b - self.min_b + 1)
    }

    fn color_count(&self) -> usize {
        1 + self.upper - self.lower
    }
}

struct Quantizer {
    colors: Vec<i32>,
    hist: Vec<i32>,
    boxes: Vec<Vbox>,
}

const COMPONENT_RED: i32 = -3;
const COMPONENT_GREEN: i32 = -2;
const COMPONENT_BLUE: i32 = -1;

impl Quantizer {
    fn new_box(&mut self, lower: usize, upper: usize) -> usize {
        self.boxes.push(Vbox { lower, upper, population: 0, min_r: 0, max_r: 0, min_g: 0, max_g: 0, min_b: 0, max_b: 0 });
        let i = self.boxes.len() - 1;
        self.fit(i);
        i
    }

    fn fit(&mut self, i: usize) {
        let (mut min_r, mut min_g, mut min_b) = (i32::MAX, i32::MAX, i32::MAX);
        let (mut max_r, mut max_g, mut max_b) = (i32::MIN, i32::MIN, i32::MIN);
        let mut count = 0;
        let (lower, upper) = (self.boxes[i].lower, self.boxes[i].upper);
        for &c in &self.colors[lower..=upper] {
            count += self.hist[c as usize];
            let (r, g, b) = (quantized_red(c), quantized_green(c), quantized_blue(c));
            max_r = max_r.max(r);
            min_r = min_r.min(r);
            max_g = max_g.max(g);
            min_g = min_g.min(g);
            max_b = max_b.max(b);
            min_b = min_b.min(b);
        }
        let v = &mut self.boxes[i];
        (v.min_r, v.max_r, v.min_g, v.max_g, v.min_b, v.max_b, v.population) = (min_r, max_r, min_g, max_g, min_b, max_b, count);
    }

    fn longest_dimension(&self, i: usize) -> i32 {
        let v = &self.boxes[i];
        let (r, g, b) = (v.max_r - v.min_r, v.max_g - v.min_g, v.max_b - v.min_b);
        if r >= g && r >= b {
            COMPONENT_RED
        } else if g >= r && g >= b {
            COMPONENT_GREEN
        } else {
            COMPONENT_BLUE
        }
    }

    fn modify_significant_octet(&mut self, dimension: i32, lower: usize, upper: usize) {
        for c in &mut self.colors[lower..=upper] {
            let v = *c;
            *c = match dimension {
                COMPONENT_GREEN => (quantized_green(v) << (WORD + WORD)) | (quantized_red(v) << WORD) | quantized_blue(v),
                COMPONENT_BLUE => (quantized_blue(v) << (WORD + WORD)) | (quantized_green(v) << WORD) | quantized_red(v),
                _ => return,
            };
        }
    }

    fn split_point(&mut self, i: usize) -> usize {
        let dim = self.longest_dimension(i);
        let (lower, upper, population) = (self.boxes[i].lower, self.boxes[i].upper, self.boxes[i].population);
        // Sort by the longest channel by swapping it into the top bits and back (the swap is its own
        // inverse).
        self.modify_significant_octet(dim, lower, upper);
        self.colors[lower..=upper].sort_unstable();
        self.modify_significant_octet(dim, lower, upper);
        let mid = population / 2;
        let mut count = 0;
        for j in lower..=upper {
            count += self.hist[self.colors[j] as usize];
            if count >= mid {
                return (upper - 1).min(j);
            }
        }
        lower
    }

    fn split(&mut self, i: usize) -> usize {
        let at = self.split_point(i);
        let upper = self.boxes[i].upper;
        let new = self.new_box(at + 1, upper);
        self.boxes[i].upper = at;
        self.fit(i);
        new
    }

    fn average(&self, i: usize) -> Swatch {
        let v = &self.boxes[i];
        let (mut rs, mut gs, mut bs, mut pop) = (0i32, 0i32, 0i32, 0i32);
        for &c in &self.colors[v.lower..=v.upper] {
            let p = self.hist[c as usize];
            pop += p;
            rs += p * quantized_red(c);
            gs += p * quantized_green(c);
            bs += p * quantized_blue(c);
        }
        let mean = |s: i32| round(s as f32 / pop as f32);
        Swatch { rgb: approximate_to_rgb888(mean(rs), mean(gs), mean(bs)), population: pop }
    }
}

/// Java's `PriorityQueue` ordered by `rhs.volume - lhs.volume`, with identical sifts so iteration
/// order matches Java's.
struct Heap {
    items: Vec<usize>,
}

impl Heap {
    fn cmp(q: &Quantizer, a: usize, b: usize) -> i32 {
        q.boxes[b].volume() - q.boxes[a].volume()
    }

    fn offer(&mut self, q: &Quantizer, x: usize) {
        let mut k = self.items.len();
        self.items.push(x);
        while k > 0 {
            let parent = (k - 1) >> 1;
            let e = self.items[parent];
            if Self::cmp(q, x, e) >= 0 {
                break;
            }
            self.items[k] = e;
            k = parent;
        }
        self.items[k] = x;
    }

    fn poll(&mut self, q: &Quantizer) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let result = self.items[0];
        let x = self.items.pop().expect("not empty");
        let n = self.items.len();
        if n > 0 {
            let mut k = 0;
            let half = n >> 1;
            while k < half {
                let mut child = (k << 1) + 1;
                let mut c = self.items[child];
                let right = child + 1;
                if right < n && Self::cmp(q, c, self.items[right]) > 0 {
                    child = right;
                    c = self.items[child];
                }
                if Self::cmp(q, x, c) <= 0 {
                    break;
                }
                self.items[k] = c;
                k = child;
            }
            self.items[k] = x;
        }
        Some(result)
    }
}

/// `ColorCutQuantizer`.
fn quantize(pixels: &[u32], max_colors: usize) -> Vec<Swatch> {
    let mut hist = vec![0i32; 1 << (WORD * 3)];
    for &p in pixels {
        hist[quantize_from_rgb888(p) as usize] += 1;
    }
    for (c, n) in hist.iter_mut().enumerate() {
        if *n > 0 && !allowed(color_to_hsl(approximate(c as i32))) {
            *n = 0;
        }
    }
    let colors: Vec<i32> = (0..hist.len() as i32).filter(|&c| hist[c as usize] > 0).collect();
    if colors.len() <= max_colors {
        return colors.iter().map(|&c| Swatch { rgb: approximate(c), population: hist[c as usize] }).collect();
    }
    let mut q = Quantizer { colors, hist, boxes: Vec::new() };
    let first = q.new_box(0, q.colors.len() - 1);
    let mut heap = Heap { items: Vec::new() };
    heap.offer(&q, first);
    while heap.items.len() < max_colors {
        match heap.poll(&q) {
            Some(b) if q.boxes[b].color_count() > 1 => {
                let new = q.split(b);
                heap.offer(&q, new);
                heap.offer(&q, b);
            }
            _ => break,
        }
    }
    heap.items.iter().map(|&b| q.average(b)).filter(|s| allowed(s.hsl())).collect()
}

/// A Palette `Target`: [min, target, max] saturation and lightness, and scoring weights.
struct Target {
    sat: [f32; 3],
    light: [f32; 3],
    weights: [f32; 3],
}

const fn target(light: [f32; 3], sat: [f32; 3]) -> Target {
    Target { sat, light, weights: [0.24, 0.52, 0.24] }
}

const LIGHT: [f32; 3] = [0.55, 0.74, 1.0];
const NORMAL: [f32; 3] = [0.3, 0.5, 0.7];
const DARK: [f32; 3] = [0.0, 0.26, 0.45];
const VIBRANT: [f32; 3] = [0.35, 1.0, 1.0];
const MUTED: [f32; 3] = [0.0, 0.3, 0.4];

/// Scores each target in Palette's order; a picked swatch is excluded from later targets.
fn score(swatches: Vec<Swatch>) -> Palette {
    let dominant = swatches.iter().fold(None::<Swatch>, |best, s| match best {
        Some(b) if s.population <= b.population => Some(b),
        _ => Some(*s),
    });
    let max_population = dominant.map_or(1, |d| d.population);
    let mut used: Vec<u32> = Vec::new();
    let mut pick = |t: Target| {
        let sum: f32 = t.weights.iter().filter(|w| **w > 0.0).sum();
        let w = t.weights.map(|x| if sum != 0.0 && x > 0.0 { x / sum } else { x });
        let mut best: Option<(Swatch, f32)> = None;
        for s in &swatches {
            let hsl = s.hsl();
            let fits = hsl[1] >= t.sat[0] && hsl[1] <= t.sat[2] && hsl[2] >= t.light[0] && hsl[2] <= t.light[2] && !used.contains(&s.rgb);
            if !fits {
                continue;
            }
            let mut score = 0.0;
            if w[0] > 0.0 {
                score += w[0] * (1.0 - (hsl[1] - t.sat[1]).abs());
            }
            if w[1] > 0.0 {
                score += w[1] * (1.0 - (hsl[2] - t.light[1]).abs());
            }
            if w[2] > 0.0 {
                score += w[2] * (s.population as f32 / max_population as f32);
            }
            if best.is_none_or(|(_, b)| score > b) {
                best = Some((*s, score));
            }
        }
        let chosen = best.map(|(s, _)| s);
        if let Some(s) = chosen {
            used.push(s.rgb);
        }
        chosen
    };
    Palette {
        dominant,
        light_vibrant: pick(target(LIGHT, VIBRANT)),
        vibrant: pick(target(NORMAL, VIBRANT)),
        dark_vibrant: pick(target(DARK, VIBRANT)),
        light_muted: pick(target(LIGHT, MUTED)),
        muted: pick(target(NORMAL, MUTED)),
        dark_muted: pick(target(DARK, MUTED)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks() {
        let mut px = vec![0xFF20_40C0u32; 100 * 100];
        px[..3000].fill(0xFFE0_3030);
        let p = generate(&px, 100, 100, 16);
        assert_eq!(p.dominant.map(|s| s.population), Some(7000));
        let colours: Vec<u32> = [p.vibrant, p.dark_vibrant, p.light_vibrant, p.muted].iter().flatten().map(|s| s.rgb).collect();
        assert!(colours.contains(&approximate(quantize_from_rgb888(0xFF20_40C0))), "{colours:x?}");

        // Ignores near white and black.
        let mut px = vec![0xFFFF_FFFFu32; 50 * 50];
        px[..1250].fill(0xFF00_0000);
        let p = generate(&px, 50, 50, 16);
        assert!(p.dominant.is_none() && p.vibrant.is_none());

        // Quantizes to at most max colors.
        let px: Vec<u32> = (0..160 * 160).map(|i| rgb((i * 7 % 256) as i32, (i * 13 % 256) as i32, (i * 29 % 256) as i32)).collect();
        let scaled = scale_down(&px, 160, 160);
        let swatches = quantize(scaled.as_deref().unwrap_or(&px), 16);
        assert!((2..=16).contains(&swatches.len()), "{} swatches", swatches.len());
        let p = generate(&px, 160, 160, 16);
        assert!(p.dominant.is_some() && p.vibrant.is_some(), "{p:?}");
    }

    #[test]
    fn downscale_samples_like_android() {
        // Source columns Android's createScaledBitmap(filter = false) picked for 320 -> 112, measured on
        // device.
        let xs: Vec<u32> = (0..320).map(|x: i32| rgb(0, x / 256, x % 256)).collect();
        let row: Vec<u32> = (0..320 * 320).map(|i| xs[i % 320]).collect();
        let small = scale_down(&row, 320, 320).unwrap();
        let got: Vec<u32> = small[..112].iter().map(|&p| (p & 0xFF) + ((p >> 8) & 0xFF) * 256).collect();
        assert_eq!(&got[..12], &[1, 4, 7, 9, 12, 15, 18, 21, 24, 27, 29, 32]);
        assert_eq!(&got[108..], &[309, 312, 315, 318]);
    }

}
