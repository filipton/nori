//! Sound-setting queries for the platform: the pre-amp in effect, graphic equalizer data and the
//! built-in curves.

pub use nori_player::dsp::*;
pub use nori_player::graphic;

/// The pre-amp in effect for the equalizer in use; 0 when it is off.
pub fn effective_preamp_db(s: &crate::settings::StoredPrefs) -> f32 {
    crate::settings::mode_preamp_db(s.eq_enabled, s.eq_preamp_db, s.eq_mode, &s.eq_graphic, &s.eq_bands)
}

/// The graphic sliders when the graphic equalizer is on; empty otherwise.
pub fn graphic_sliders(s: &crate::settings::StoredPrefs) -> Vec<f64> {
    if s.eq_enabled && s.eq_mode == crate::settings::EqMode::Graphic {
        s.eq_graphic.iter().map(|g| *g as f64).collect()
    } else {
        Vec::new()
    }
}

/// The graphic band centres for `count` sliders, low to high, with their ISO labels. Empty for an
/// invalid count.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn graphic_bands(count: u32) -> Vec<GraphicBand> {
    nori_player::graphic::centres(count as usize).into_iter().map(|f| GraphicBand { freq: f as f32, label_hz: nori_player::graphic::nominal(f) as f32 }).collect()
}

/// How closely the graphic sliders follow a correction, dB: RMS and maximum difference over 20 Hz-20 kHz,
/// overall level removed.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct GraphicFollow {
    pub rms_db: f32,
    pub max_db: f32,
}

/// [`GraphicFollow`] for `sliders` against `target` (`StoredPrefs::eq_graphic_target`); None without a
/// target.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn graphic_follow(sliders: Vec<f32>, target: Vec<f32>) -> Option<GraphicFollow> {
    if target.len() != nori_player::graphic::TARGET_POINTS {
        return None;
    }
    let s: Vec<f64> = sliders.iter().map(|v| *v as f64).collect();
    let t: Vec<f64> = target.iter().map(|v| *v as f64).collect();
    let (rms, max) = nori_player::graphic::follow(&s, &t);
    Some(GraphicFollow { rms_db: rms as f32, max_db: max as f32 })
}

/// The correction target's frequency grid, Hz.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn graphic_target_grid() -> Vec<f32> {
    nori_player::graphic::target_grid().into_iter().map(|f| f as f32).collect()
}

/// One band of the graphic equalizer.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct GraphicBand {
    pub freq: f32,
    /// Its ISO 266 nominal frequency (31.5, 63, 125 ... 16000).
    pub label_hz: f32,
}

/// The graphic equalizer's response in dB at each of `freqs` (48 kHz), for drawing the curve.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn graphic_response(sliders: Vec<f32>, freqs: Vec<f32>) -> Vec<f32> {
    let g: Vec<f64> = sliders.iter().map(|v| *v as f64).collect();
    let bands = nori_player::graphic::design(48_000.0, &g);
    freqs.iter().map(|f| nori_player::graphic::response_db(48_000.0, &bands, *f as f64) as f32).collect()
}

/// The built-in equalizer curves.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn eq_presets() -> Vec<nori_model::NamedPreset> {
    nori_player::dsp::eq_presets()
}
