//! The sound settings' answers the platform asks for: the pre-amp in effect, the built-in curves and which
//! parts of the chain may run. The chain itself (`nori_player::dsp`) runs inside the player, which is
//! handed the settings as they change.

pub use nori_player::dsp::*;
/// The graphic equalizer's layouts and design, for a client that draws its bands.
pub use nori_player::graphic;

/// The pre-amp in effect: the one set, or the automatic one for the equalizer in use; none with the
/// equalizer off.
pub fn effective_preamp_db(s: &crate::settings::StoredPrefs) -> f32 {
    if !s.eq_enabled {
        return 0.0;
    }
    s.eq_preamp_db.unwrap_or_else(|| match s.eq_mode {
        crate::settings::EqMode::Graphic => nori_player::dsp::auto_preamp_db(s.eq_graphic.iter().map(|g| (nori_player::dsp::PEAKING, *g))),
        crate::settings::EqMode::Parametric => nori_player::dsp::auto_preamp_db(s.eq_bands.iter().map(|b| (b.kind as i32, b.gain_db))),
    })
}

/// The graphic equalizer's sliders when it is the one playing (on, and in graphic mode); empty otherwise.
pub fn graphic_sliders(s: &crate::settings::StoredPrefs) -> Vec<f64> {
    if s.eq_enabled && s.eq_mode == crate::settings::EqMode::Graphic {
        s.eq_graphic.iter().map(|g| *g as f64).collect()
    } else {
        Vec::new()
    }
}

/// The graphic equalizer's band centres for `count` sliders (10, 15 or 31), low to high, Hz: the exact
/// ones the filters sit on and the ISO labels they are named by. Empty for a count that is not a layout.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn graphic_bands(count: u32) -> Vec<GraphicBand> {
    nori_player::graphic::centres(count as usize).into_iter().map(|f| GraphicBand { freq: f as f32, label_hz: nori_player::graphic::nominal(f) as f32 }).collect()
}

/// How closely the graphic equalizer follows a headphone correction, dB: the root mean square and the
/// largest difference over 20 Hz to 20 kHz, the overall level taken out.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct GraphicFollow {
    pub rms_db: f32,
    pub max_db: f32,
}

/// How closely `sliders` follow a correction's `target` (`StoredPrefs::eq_graphic_target`); none without
/// a target. Asked once per change, for the equalizer screen's line.
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

/// A target's grid, Hz (`nori_player::graphic::target_grid`), for a screen that draws it.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn graphic_target_grid() -> Vec<f32> {
    nori_player::graphic::target_grid().into_iter().map(|f| f as f32).collect()
}

/// One band of the graphic equalizer.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct GraphicBand {
    pub freq: f32,
    /// The ISO 266 frequency it is labelled with (31.5, 63, 125 ... 16000).
    pub label_hz: f32,
}

/// The response in dB the graphic equalizer plays for these sliders at each of `freqs` (at 48 kHz), for
/// a screen that draws the curve; asked once per change, not per frame.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn graphic_response(sliders: Vec<f32>, freqs: Vec<f32>) -> Vec<f32> {
    let g: Vec<f64> = sliders.iter().map(|v| *v as f64).collect();
    let bands = nori_player::graphic::design(48_000.0, &g);
    freqs.iter().map(|f| nori_player::graphic::response_db(48_000.0, &bands, *f as f64) as f32).collect()
}

/// The built-in curves, as data, so the UI (and the settings store) never holds a frequency of its own.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn eq_presets() -> Vec<nori_model::NamedPreset> {
    nori_player::dsp::eq_presets()
}

/// Which parts of the chain may run, from the settings and the output; see `nori_player::policy`.
pub fn audio_policy(prefs: nori_model::AudioPrefs, output: nori_model::OutputState) -> nori_model::AudioPolicy {
    nori_player::policy::audio_policy(&prefs, &output)
}
