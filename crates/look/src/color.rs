//! Bit-exact ports of AndroidX `ColorUtils` and Compose's `Color.luminance`, which the tuned thresholds
//! depend on.

pub fn alpha(c: u32) -> i32 {
    (c >> 24) as i32
}
pub fn red(c: u32) -> i32 {
    ((c >> 16) & 0xFF) as i32
}
pub fn green(c: u32) -> i32 {
    ((c >> 8) & 0xFF) as i32
}
pub fn blue(c: u32) -> i32 {
    (c & 0xFF) as i32
}

pub fn argb(a: i32, r: i32, g: i32, b: i32) -> u32 {
    ((a as u32 & 0xFF) << 24) | ((r as u32 & 0xFF) << 16) | ((g as u32 & 0xFF) << 8) | (b as u32 & 0xFF)
}

pub fn rgb(r: i32, g: i32, b: i32) -> u32 {
    argb(0xFF, r, g, b)
}

pub const WHITE: u32 = 0xFFFF_FFFF;
pub const BLACK: u32 = 0xFF00_0000;

/// Java's `Math.round(float)`: half up, computed exactly.
pub fn round(x: f32) -> i32 {
    (x as f64 + 0.5).floor() as i32
}

/// `ColorUtils.RGBToHSL`: hue in degrees, saturation and lightness 0..1.
pub fn rgb_to_hsl(r: i32, g: i32, b: i32) -> [f32; 3] {
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = rf.max(gf.max(bf));
    let min = rf.min(gf.min(bf));
    let delta = max - min;
    let l = (max + min) / 2.0;
    let (mut h, s);
    if max == min {
        h = 0.0;
        s = 0.0;
    } else {
        h = if max == rf {
            ((gf - bf) / delta) % 6.0
        } else if max == gf {
            ((bf - rf) / delta) + 2.0
        } else {
            ((rf - gf) / delta) + 4.0
        };
        s = delta / (1.0 - (2.0 * l - 1.0).abs());
    }
    h = (h * 60.0) % 360.0;
    if h < 0.0 {
        h += 360.0;
    }
    [h.clamp(0.0, 360.0), s.clamp(0.0, 1.0), l.clamp(0.0, 1.0)]
}

pub fn color_to_hsl(c: u32) -> [f32; 3] {
    rgb_to_hsl(red(c), green(c), blue(c))
}

/// `ColorUtils.HSLToColor`.
pub fn hsl_to_color(hsl: [f32; 3]) -> u32 {
    let [h, s, l] = hsl;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let m = l - 0.5 * c;
    let x = c * (1.0 - ((h / 60.0 % 2.0) - 1.0).abs());
    let segment = (h as i32) / 60;
    let (r, g, b) = match segment {
        0 => (round(255.0 * (c + m)), round(255.0 * (x + m)), round(255.0 * m)),
        1 => (round(255.0 * (x + m)), round(255.0 * (c + m)), round(255.0 * m)),
        2 => (round(255.0 * m), round(255.0 * (c + m)), round(255.0 * (x + m))),
        3 => (round(255.0 * m), round(255.0 * (x + m)), round(255.0 * (c + m))),
        4 => (round(255.0 * (x + m)), round(255.0 * m), round(255.0 * (c + m))),
        5 | 6 => (round(255.0 * (c + m)), round(255.0 * m), round(255.0 * (x + m))),
        _ => (0, 0, 0),
    };
    rgb(r.clamp(0, 255), g.clamp(0, 255), b.clamp(0, 255))
}

/// `ColorUtils.calculateLuminance`: relative luminance 0..1.
pub fn calculate_luminance(c: u32) -> f64 {
    fn lin(v: i32) -> f64 {
        let s = v as f64 / 255.0;
        if s < 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    let y = 100.0 * (lin(red(c)) * 0.2126 + lin(green(c)) * 0.7152 + lin(blue(c)) * 0.0722);
    y / 100.0
}

fn composite_alpha(fg: i32, bg: i32) -> i32 {
    0xFF - (((0xFF - bg) * (0xFF - fg)) / 0xFF)
}

fn composite_component(fg_c: i32, fg_a: i32, bg_c: i32, bg_a: i32, a: i32) -> i32 {
    if a == 0 {
        return 0;
    }
    ((0xFF * fg_c * fg_a) + (bg_c * bg_a * (0xFF - fg_a))) / (a * 0xFF)
}

/// `ColorUtils.compositeColors`: `fg` over `bg`.
pub fn composite_colors(fg: u32, bg: u32) -> u32 {
    let (bga, fga) = (alpha(bg), alpha(fg));
    let a = composite_alpha(fga, bga);
    argb(
        a,
        composite_component(red(fg), fga, red(bg), bga, a),
        composite_component(green(fg), fga, green(bg), bga, a),
        composite_component(blue(fg), fga, blue(bg), bga, a),
    )
}

/// `ColorUtils.calculateContrast`: the WCAG contrast ratio, 1..21. `bg` must be opaque.
pub fn calculate_contrast(fg: u32, bg: u32) -> f64 {
    let fg = if alpha(fg) < 255 { composite_colors(fg, bg) } else { fg };
    let l1 = calculate_luminance(fg) + 0.05;
    let l2 = calculate_luminance(bg) + 0.05;
    l1.max(l2) / l1.min(l2)
}

/// `ColorUtils.blendARGB`: `ratio` 0 is `a`, 1 is `b`.
pub fn blend_argb(a: u32, b: u32, ratio: f32) -> u32 {
    let inv = 1.0 - ratio;
    let al = alpha(a) as f32 * inv + alpha(b) as f32 * ratio;
    let r = red(a) as f32 * inv + red(b) as f32 * ratio;
    let g = green(a) as f32 * inv + green(b) as f32 * ratio;
    let bl = blue(a) as f32 * inv + blue(b) as f32 * ratio;
    argb(al as i32, r as i32, g as i32, bl as i32)
}

/// Compose's `Color.luminance()`: like [`calculate_luminance`] but with Compose's float rounding.
pub fn luminance(c: u32) -> f32 {
    fn eotf(x: f64) -> f64 {
        let (a, b, cc, d, g) = (1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045, 2.4);
        if x >= d {
            (a * x + b).powf(g)
        } else {
            cc * x
        }
    }
    let ch = |v: i32| eotf((v as f32 / 255.0) as f64);
    ((0.2126 * ch(red(c))) + (0.7152 * ch(green(c))) + (0.0722 * ch(blue(c)))).clamp(0.0, 1.0) as f32
}

/// Compose's opaque `Color(red, green, blue)`.
pub fn from_floats(r: f32, g: f32, b: f32) -> u32 {
    crate::compose::from_floats_a(r, g, b, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_round_trips() {
        for &c in &[0xFF12_3456u32, 0xFFFF_0000, 0xFF00_FF00, 0xFF00_00FF, 0xFFFF_FFFF, 0xFF00_0000, 0xFFC0_B090, 0xFF7F_7F80] {
            assert_eq!(hsl_to_color(color_to_hsl(c)), c, "{c:08x}");
        }
    }

    #[test]
    fn contrast_and_luminance_extremes() {
        assert!((calculate_contrast(WHITE, BLACK) - 21.0).abs() < 1e-9);
        assert!((calculate_contrast(0xFF77_7777, 0xFF77_7777) - 1.0).abs() < 1e-9);
        assert!((luminance(WHITE) - 1.0).abs() < 1e-6 && luminance(BLACK) == 0.0);
    }

    #[test]
    fn round_matches_java() {
        assert_eq!((round(0.5), round(1.5), round(-0.5), round(2.4999998)), (1, 2, 0, 2));
    }
}
