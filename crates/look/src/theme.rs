//! Light or dark tones from one seed colour (a tiny Material-style generator).

use crate::color::{color_to_hsl, hsl_to_color, WHITE};

/// Tones in order: primary, on primary, primary container, on primary container, secondary, secondary
/// container, on secondary container, surface, background, surface variant, on surface variant.
pub fn seeded(seed: u32, dark: bool) -> [u32; 11] {
    let hsl = color_to_hsl(seed);
    let tone = |l: f32, s: f32| hsl_to_color([hsl[0], s.clamp(0.0, 1.0), l]);
    let s = hsl[1];
    if dark {
        [
            tone(0.80, s),
            tone(0.20, s),
            tone(0.30, s),
            tone(0.90, s),
            tone(0.78, s * 0.4),
            tone(0.28, s * 0.4),
            tone(0.90, s * 0.4),
            tone(0.07, s * 0.12),
            tone(0.07, s * 0.12),
            tone(0.22, s * 0.15),
            tone(0.80, s * 0.15),
        ]
    } else {
        [
            tone(0.40, s),
            WHITE,
            tone(0.90, s),
            tone(0.12, s),
            tone(0.40, s * 0.4),
            tone(0.90, s * 0.4),
            tone(0.12, s * 0.4),
            tone(0.98, s * 0.2),
            tone(0.98, s * 0.2),
            tone(0.90, s * 0.15),
            tone(0.30, s * 0.15),
        ]
    }
}

/// Accent choices when not using wallpaper colours, in display order; the first is the default.
pub const ACCENTS: [u32; 8] = [0xFF67_50A4, 0xFF1E_88E5, 0xFF00_897B, 0xFF43_A047, 0xFFF4_511E, 0xFFE5_3935, 0xFFD8_1B60, 0xFF8E_24AA];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::calculate_contrast;

    #[test]
    fn seeded_tones_have_readable_contrast() {
        for dark in [true, false] {
            let t = seeded(0xFF3F_51B5, dark);
            assert!(calculate_contrast(t[0], t[1]) >= 4.5, "primary / on primary, dark={dark}");
            assert!(calculate_contrast(t[10], t[9]) >= 3.0, "on surface variant, dark={dark}");
        }
    }
}
