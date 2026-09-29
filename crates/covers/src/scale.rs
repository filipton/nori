//! Center-crop resize of a decoded picture into an RGBA target: area filter when shrinking, bilinear when
//! growing, two separable fixed-point passes. No allocation once the scaler's buffers have grown.

/// Fixed-point 1.0: the sum of an output pixel's weights.
const ONE: u32 = 1 << 14;

/// Decoder output: `channels` bytes per pixel (grey, grey+alpha, RGB or RGBA), rows `stride` bytes apart.
pub struct Source<'a> {
    pub px: &'a mut [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub channels: usize,
}

/// RGBA destination, rows `stride` bytes apart (Bitmap rows may be padded).
pub struct Target<'a> {
    pub px: &'a mut [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

impl Target<'_> {
    /// Whether the dimensions are non-zero and every row fits in the buffer.
    pub fn fits(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.stride >= self.width * 4
            && (self.height - 1).checked_mul(self.stride).and_then(|n| n.checked_add(self.width * 4)).is_some_and(|n| n <= self.px.len())
    }

    /// Whether a decoder can write a tight `width` x `height` RGBA picture straight into it.
    pub fn takes(&self, width: usize, height: usize) -> bool {
        (width, height) == (self.width, self.height) && self.stride == width * 4
    }

    fn row(&mut self, y: usize) -> &mut [u8] {
        &mut self.px[y * self.stride..][..self.width * 4]
    }
}

/// Alpha encoding of the output (identical for opaque pictures).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alpha {
    /// Straight RGBA8, as most GUI toolkits take it.
    Straight,
    /// Premultiplied, as an Android Bitmap holds it.
    Premultiplied,
}

/// Resampling plan for one axis: output `i` reads `taps` pixels from `first[i]`, weighted by
/// `weights[i * taps..][..taps]`. Every output has the same tap count (unused taps weigh zero) so loops
/// have one length and never read past the line.
#[derive(Default)]
struct Axis {
    first: Vec<u32>,
    weights: Vec<u16>,
    taps: usize,
    /// Scratch: one output pixel's float weights.
    exact: Vec<f64>,
}

impl Axis {
    /// Plans `out` pixels from the span `from..from + len` of a line of `n` source pixels.
    fn plan(&mut self, from: f64, len: f64, n: usize, out: usize) {
        let s = len / out as f64;
        let shrink = s >= 1.0;
        let taps = if shrink { (s.ceil() as usize + 1).min(n) } else { 2.min(n) };
        self.taps = taps;
        self.first.clear();
        self.weights.clear();
        self.weights.resize(out * taps, 0);
        self.exact.resize(taps, 0.0);
        let w = &mut self.exact;
        for i in 0..out {
            let (first, count) = if shrink {
                // Area filter: each source pixel weighted by its covered fraction.
                let (a, b) = (from + i as f64 * s, from + (i + 1) as f64 * s);
                let first = (a.floor() as usize).min(n - 1);
                let last = ((b.ceil() as usize).max(first + 1) - 1).min(n - 1).min(first + taps - 1);
                for (k, j) in (first..=last).enumerate() {
                    w[k] = (b.min(j as f64 + 1.0) - a.max(j as f64)).max(0.0);
                }
                (first, last - first + 1)
            } else {
                // Bilinear between the two nearest source pixels.
                let c = (from + (i as f64 + 0.5) * s - 0.5).clamp(0.0, (n - 1) as f64);
                let first = c.floor() as usize;
                let f = c - first as f64;
                if first + 1 < n && f > 0.0 {
                    (w[0], w[1]) = (1.0 - f, f);
                    (first, 2)
                } else {
                    w[0] = 1.0;
                    (first, 1)
                }
            };
            // Near the line's end the taps start earlier, the extra leading taps weighing zero.
            let shift = (first + taps).saturating_sub(n);
            // To fixed point; the rounding error goes to the largest weight so they sum to ONE.
            let total: f64 = w[..count].iter().sum();
            let out = &mut self.weights[i * taps + shift..][..count];
            let mut sum = 0;
            let mut big = 0;
            for k in 0..count {
                out[k] = (w[k] / total * ONE as f64).round() as u16;
                sum += out[k] as u32;
                if out[k] > out[big] {
                    big = k;
                }
            }
            out[big] = (out[big] as i64 + ONE as i64 - sum as i64) as u16;
            self.first.push((first - shift) as u32);
        }
    }
}

/// Opaque alpha in the x64 intermediate format.
const OPAQUE: u16 = 255 * 64;

/// Plans and scratch rows, reused across pictures.
#[derive(Default)]
pub struct Scaler {
    h: Axis,
    v: Axis,
    sums: Vec<u32>,
    /// Intermediate pixels as RGBA x64: always four lanes so the second pass is one vector op per tap
    /// and stays within 32 bits.
    mid: Vec<[u16; 4]>,
    /// Source row held in each `mid` slot (rows-first pass).
    held: Vec<usize>,
}

impl Scaler {
    /// Center-crops and scales `src` into `t`. Pictures with alpha are filtered premultiplied (no colour
    /// bleed from transparent pixels; `src` is premultiplied in place) and written as `alpha` asks.
    pub fn fill(&mut self, src: Source, t: &mut Target, alpha: Alpha) {
        let has_alpha = src.channels.is_multiple_of(2);
        if (src.width, src.height) == (t.width, t.height) {
            for y in 0..t.height {
                let row = &src.px[y * src.stride..][..src.width * src.channels];
                let out = t.row(y);
                match src.channels {
                    1 => expand::<1>(row, out),
                    2 => expand::<2>(row, out),
                    3 => expand::<3>(row, out),
                    _ => expand::<4>(row, out),
                }
                if has_alpha && alpha == Alpha::Premultiplied {
                    premultiply(out, 4);
                }
            }
            return;
        }
        if has_alpha {
            for y in 0..src.height {
                premultiply(&mut src.px[y * src.stride..][..src.width * src.channels], src.channels);
            }
        }
        // Center crop to the target's aspect ratio.
        let (sw, sh, tw, th) = (src.width as f64, src.height as f64, t.width as f64, t.height as f64);
        let (mut x, mut y, mut w, mut h) = (0.0, 0.0, sw, sh);
        if sw * th > sh * tw {
            w = sh * tw / th;
            x = (sw - w) / 2.0;
        } else {
            h = sw * th / tw;
            y = (sh - h) / 2.0;
        }
        self.h.plan(x, w, src.width, t.width);
        self.v.plan(y, h, src.height, t.height);
        // Growing vertically: resample each source row once and reuse it. Shrinking: columns first
        // leaves fewer rows to resample.
        if h < th {
            self.rows_first(&src, t);
        } else {
            self.columns_first(&src, t);
        }
        if has_alpha && alpha == Alpha::Straight {
            for y in 0..t.height {
                unpremultiply(t.row(y));
            }
        }
    }

    fn columns_first(&mut self, src: &Source, t: &mut Target) {
        let c = src.channels;
        // Sum only the columns the horizontal pass reads.
        let c0 = self.h.first[0] as usize;
        let pixels = self.h.first[t.width - 1] as usize + self.h.taps - c0;
        let len = pixels * c;
        for f in &mut self.h.first {
            *f -= c0 as u32;
        }
        self.sums.resize(len, 0);
        self.mid.resize(pixels, [0; 4]);
        let vt = self.v.taps;
        for oy in 0..t.height {
            let first = self.v.first[oy] as usize;
            let sums = &mut self.sums[..len];
            let row = |k: usize| &src.px[(first + k) * src.stride + c0 * c..][..len];
            // The first non-zero tap initialises the sums, the rest accumulate.
            let mut taps = self.v.weights[oy * vt..][..vt].iter().enumerate().filter(|(_, &w)| w != 0);
            let (k, &w) = taps.next().expect("a pixel's weights add up to one");
            for (s, &p) in sums.iter_mut().zip(row(k)) {
                *s = w as u32 * p as u32;
            }
            for (k, &w) in taps {
                for (s, &p) in sums.iter_mut().zip(row(k)) {
                    *s += w as u32 * p as u32;
                }
            }
            let m = |s: u32| ((s + 128) >> 8) as u16;
            match c {
                4 => self.mid.iter_mut().zip(sums.as_chunks::<4>().0).for_each(|(o, s)| *o = s.map(m)),
                3 => self.mid.iter_mut().zip(sums.as_chunks::<3>().0).for_each(|(o, s)| *o = [m(s[0]), m(s[1]), m(s[2]), OPAQUE]),
                2 => self.mid.iter_mut().zip(sums.as_chunks::<2>().0).for_each(|(o, s)| *o = [m(s[0]), m(s[0]), m(s[0]), m(s[1])]),
                _ => self.mid.iter_mut().zip(sums.iter()).for_each(|(o, &s)| *o = [m(s), m(s), m(s), OPAQUE]),
            }
            across(&self.mid, &self.h, t.row(oy));
        }
    }

    fn rows_first(&mut self, src: &Source, t: &mut Target) {
        let (w, vt) = (t.width, self.v.taps);
        self.mid.resize(vt * w, [0; 4]);
        self.held.clear();
        self.held.resize(vt, usize::MAX);
        self.sums.resize(w * 4, 0);
        for oy in 0..t.height {
            let first = self.v.first[oy] as usize;
            // Rows advance monotonically: each is resampled once into ring slot `j % vt`.
            for j in first..first + vt {
                if self.held[j % vt] != j {
                    let row = &src.px[j * src.stride..][..src.width * src.channels];
                    let out = &mut self.mid[j % vt * w..][..w];
                    match src.channels {
                        1 => along::<1>(row, &self.h, out),
                        2 => along::<2>(row, &self.h, out),
                        3 => along::<3>(row, &self.h, out),
                        _ => along::<4>(row, &self.h, out),
                    }
                    self.held[j % vt] = j;
                }
            }
            let ws = &self.v.weights[oy * vt..][..vt];
            let row = |k: usize| self.mid[(first + k) % vt * w..][..w].as_flattened();
            let out = t.row(oy);
            if vt == 2 {
                let (w0, w1) = (ws[0] as u32, ws[1] as u32);
                for ((o, &a), &b) in out.iter_mut().zip(row(0)).zip(row(1)) {
                    *o = ((w0 * a as u32 + w1 * b as u32 + (1 << 19)) >> 20).min(255) as u8;
                }
                continue;
            }
            let sums = &mut self.sums[..w * 4];
            sums.fill(1 << 19);
            for (k, &wk) in ws.iter().enumerate() {
                for (s, &p) in sums.iter_mut().zip(row(k)) {
                    *s += wk as u32 * p as u32;
                }
            }
            for (o, &s) in out.iter_mut().zip(sums.iter()) {
                *o = (s >> 20).min(255) as u8;
            }
        }
    }
}

/// Horizontal pass over one source row of `C`-byte pixels, into RGBA x64.
fn along<const C: usize>(row: &[u8], h: &Axis, out: &mut [[u16; 4]]) {
    let taps = h.taps;
    for ((o, &first), ws) in out.iter_mut().zip(&h.first).zip(h.weights.chunks_exact(taps)) {
        let from = &row[first as usize * C..][..taps * C];
        let mut acc = [128u32; C];
        for (p, &w) in from.chunks_exact(C).zip(ws) {
            for ch in 0..C {
                acc[ch] += w as u32 * p[ch] as u32;
            }
        }
        let v = |ch: usize| (acc[ch] >> 8) as u16;
        *o = match C {
            1 => [v(0), v(0), v(0), OPAQUE],
            2 => [v(0), v(0), v(0), v(1)],
            3 => [v(0), v(1), v(2), OPAQUE],
            _ => [v(0), v(1), v(2), v(3)],
        };
    }
}

/// Horizontal pass from RGBA x64 (14 bits) with 14-bit weights, within 32 bits. Common tap counts get
/// an unrolled loop.
fn across(mid: &[[u16; 4]], h: &Axis, out: &mut [u8]) {
    match h.taps {
        1 => across_n::<1>(mid, h, out),
        2 => across_n::<2>(mid, h, out),
        3 => across_n::<3>(mid, h, out),
        4 => across_n::<4>(mid, h, out),
        _ => across_n::<0>(mid, h, out),
    }
}

/// [`across`] for `T` taps (0: the plan's count).
fn across_n<const T: usize>(mid: &[[u16; 4]], h: &Axis, out: &mut [u8]) {
    let taps = if T == 0 { h.taps } else { T };
    for ((px, &first), ws) in out.as_chunks_mut::<4>().0.iter_mut().zip(&h.first).zip(h.weights.chunks_exact(taps)) {
        let from = &mid[first as usize..][..taps];
        let mut acc = [1u32 << 19; 4];
        for (p, &w) in from.iter().zip(ws) {
            for ch in 0..4 {
                acc[ch] += w as u32 * p[ch] as u32;
            }
        }
        *px = acc.map(|a| (a >> 20).min(255) as u8);
    }
}

/// Converts a row of `C`-byte pixels to RGBA.
fn expand<const C: usize>(row: &[u8], out: &mut [u8]) {
    if C == 4 {
        out.copy_from_slice(row);
        return;
    }
    for (p, o) in row.chunks_exact(C).zip(out.chunks_exact_mut(4)) {
        o.copy_from_slice(&match C {
            1 => [p[0], p[0], p[0], 255],
            2 => [p[0], p[0], p[0], p[1]],
            _ => [p[0], p[1], p[2], 255],
        });
    }
}

/// Premultiplies in place, rounding like Android's `Bitmap.setPixels`. `c` is 2 (grey+alpha) or 4.
pub fn premultiply(px: &mut [u8], c: usize) {
    for p in px.chunks_exact_mut(c) {
        let a = p[c - 1] as u32;
        if a != 255 {
            for v in &mut p[..c - 1] {
                *v = ((*v as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

/// Converts premultiplied RGBA back to straight.
pub fn unpremultiply(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a != 255 {
            for v in &mut p[..3] {
                *v = if a == 0 { 0 } else { ((*v as u32 * 255 + a / 2) / a).min(255) as u8 };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Output `i`'s first source pixel and its non-zero weights.
    fn weights(a: &Axis, i: usize) -> (u32, Vec<u16>) {
        let w = &a.weights[i * a.taps..][..a.taps];
        let skip = w.iter().take_while(|&&w| w == 0).count();
        let end = w.iter().rposition(|&w| w != 0).map_or(skip, |e| e + 1);
        (a.first[i] + skip as u32, w[skip..end].to_vec())
    }

    #[test]
    fn shrink_weights_by_coverage() {
        let mut a = Axis::default();
        // 3 -> 2: each output covers 1.5 pixels.
        a.plan(0.0, 3.0, 3, 2);
        assert_eq!(weights(&a, 0), (0, vec![10923, 5461]));
        assert_eq!(weights(&a, 1), (1, vec![5461, 10923]));
        // Halving: even pairs.
        a.plan(0.0, 8.0, 8, 4);
        assert_eq!(weights(&a, 3), (6, vec![8192, 8192]));
    }

    #[test]
    fn grow_is_bilinear() {
        let mut a = Axis::default();
        a.plan(0.0, 2.0, 2, 4);
        assert_eq!(weights(&a, 0), (0, vec![16384]));
        assert_eq!(weights(&a, 1), (0, vec![12288, 4096]));
        assert_eq!(weights(&a, 2), (0, vec![4096, 12288]));
        assert_eq!(weights(&a, 3), (1, vec![16384]));
    }

    #[test]
    fn wide_picture_is_center_cropped() {
        // 4x2 with red edge columns and white middle, into 1x1: only the middle counts.
        let mut px = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                px.extend_from_slice(if x == 0 || x == 3 { &[255, 0, 0] } else { &[255, 255, 255] });
            }
        }
        let mut out = [0u8; 4];
        let src = Source { px: &mut px, width: 4, height: 2, stride: 12, channels: 3 };
        Scaler::default().fill(src, &mut Target { px: &mut out, width: 1, height: 1, stride: 4 }, Alpha::Straight);
        assert_eq!(out, [255, 255, 255, 255]);
    }

    #[test]
    fn alpha_filtered_premultiplied_and_written_as_asked() {
        // Opaque red next to transparent green: no green bleeds in.
        let mk = || vec![255u8, 0, 0, 255, 0, 255, 0, 0];
        let mut out = [0u8; 4];
        let mut px = mk();
        let src = Source { px: &mut px, width: 2, height: 1, stride: 8, channels: 4 };
        Scaler::default().fill(src, &mut Target { px: &mut out, width: 1, height: 1, stride: 4 }, Alpha::Premultiplied);
        assert_eq!(out, [128, 0, 0, 128]);
        let mut px = mk();
        let src = Source { px: &mut px, width: 2, height: 1, stride: 8, channels: 4 };
        Scaler::default().fill(src, &mut Target { px: &mut out, width: 1, height: 1, stride: 4 }, Alpha::Straight);
        assert_eq!(out, [255, 0, 0, 128]);
    }

    #[test]
    fn padded_target_leaves_padding_untouched() {
        let mut px = vec![10u8, 20, 30];
        let mut out = [7u8; 2 * 12];
        let src = Source { px: &mut px, width: 1, height: 1, stride: 3, channels: 3 };
        Scaler::default().fill(src, &mut Target { px: &mut out, width: 2, height: 2, stride: 12 }, Alpha::Straight);
        for row in out.chunks_exact(12) {
            assert_eq!(row, &[10, 20, 30, 255, 10, 20, 30, 255, 7, 7, 7, 7]);
        }
    }
}
