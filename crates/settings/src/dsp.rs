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
