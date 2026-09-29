//! The equalizer page, as Android's EqualizerScreen: mode, bands and curve, presets, pre-amp, balance,
//! mono, limiter and crossfeed. All numbers come from the core.

use nori_core::dsp::{effective_preamp_db, eq_presets, graphic_bands, graphic_response};
use nori_core::settings::{crossfeed_preset_of, EqMode, StoredPrefs, EQ_RANGES};
use nori_core::{EqKind, PresetKind};
use slint::{ModelRc, SharedString, VecModel};

use crate::{AppWindow, EqBandRow};

/// "63", "1k", "2.5k", "31.5".
pub fn hz(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if (k - k.round()).abs() < 0.05 { format!("{:.0}k", k) } else { format!("{:.1}k", k) }
    } else if f.fract() != 0.0 {
        format!("{f:.1}")
    } else {
        format!("{f:.0}")
    }
}

/// "+1.5", "−3.0" (typographic minus).
pub fn signed(v: f32) -> String {
    if v > 0.0 { format!("+{v:.1}") } else { format!("{v:.1}").replace('-', "−") }
}

pub fn preset_name(kind: PresetKind) -> &'static str {
    match kind {
        PresetKind::Flat => "Flat",
        PresetKind::BassBoost => "Bass boost",
        PresetKind::BassCut => "Bass cut",
        PresetKind::TrebleBoost => "Treble boost",
        PresetKind::TrebleCut => "Treble cut",
        PresetKind::VocalBoost => "Vocal boost",
        PresetKind::Loudness => "Loudness",
        PresetKind::SmallSpeakers => "Small speakers",
    }
}

fn kind_name(k: EqKind) -> &'static str {
    match k {
        EqKind::Peaking => "Peak",
        EqKind::LowShelf | EqKind::LowShelfSlope => "Low shelf",
        EqKind::HighShelf | EqKind::HighShelfSlope => "High shelf",
        EqKind::LowPass => "Low-pass",
        EqKind::HighPass => "High-pass",
        EqKind::BandPass => "Band-pass",
        EqKind::Notch => "Notch",
        EqKind::AllPass => "All-pass",
    }
}

fn uses_gain(k: EqKind) -> bool {
    matches!(k, EqKind::Peaking | EqKind::LowShelf | EqKind::HighShelf | EqKind::LowShelfSlope | EqKind::HighShelfSlope)
}

/// The graphic EQ response as an SVG path in a `w` x `h` box (20 Hz..20 kHz, ±15 dB).
fn curve(sliders: &[f32], w: f32, h: f32) -> String {
    let freqs: Vec<f32> = (0..97).map(|i| 20.0 * 1000f32.powf(i as f32 / 96.0)).collect();
    let r = graphic_response(sliders.to_vec(), freqs);
    let mut out = String::new();
    for (i, db) in r.iter().enumerate() {
        let x = w * i as f32 / 96.0;
        let y = h / 2.0 - db.clamp(-15.0, 15.0) / 15.0 * (h / 2.0 - 3.0);
        out.push_str(&format!("{}{:.1} {:.1} ", if i == 0 { "M" } else { "L" }, x, y));
    }
    out
}

/// Updates only the curve, while a band is dragged.
pub fn curve_only(ui: &AppWindow, p: &StoredPrefs) {
    if p.eq_mode == EqMode::Graphic {
        ui.set_eq_curve(curve(&p.eq_graphic, ui.get_eq_curve_w(), ui.get_eq_curve_h()).into());
    }
}

pub fn fill(ui: &AppWindow, p: &StoredPrefs) {
    let graphic = p.eq_mode == EqMode::Graphic;
    ui.set_eq_on(p.eq_enabled);
    ui.set_eq_graphic(graphic);
    ui.set_eq_count(p.eq_graphic.len() as i32);
    let bands: Vec<EqBandRow> = if graphic {
        let centres = graphic_bands(p.eq_graphic.len() as u32);
        p.eq_graphic
            .iter()
            .enumerate()
            .map(|(i, g)| EqBandRow { label: centres.get(i).map_or(String::new(), |b| hz(b.label_hz)).into(), gain: *g, value: signed(*g).into(), uses_gain: true })
            .collect()
    } else {
        p.eq_bands
            .iter()
            .map(|b| {
                let side = match b.channel as i32 {
                    1 => " L",
                    2 => " R",
                    _ => "",
                };
                EqBandRow {
                    label: format!("{}{side}", hz(b.freq)).into(),
                    gain: b.gain_db,
                    value: if uses_gain(b.kind) { signed(b.gain_db).into() } else { kind_name(b.kind).into() },
                    uses_gain: uses_gain(b.kind),
                }
            })
            .collect()
    };
    ui.set_eq_bands(ModelRc::new(VecModel::from(bands)));
    ui.set_eq_curve(if graphic { curve(&p.eq_graphic, ui.get_eq_curve_w(), ui.get_eq_curve_h()).into() } else { SharedString::new() });
    let presets: Vec<SharedString> = eq_presets().iter().map(|x| preset_name(x.kind).into()).collect();
    ui.set_eq_presets(ModelRc::new(VecModel::from(presets)));
    let auto = p.eq_preamp_db.is_none();
    ui.set_eq_auto_preamp(auto);
    let effective = effective_preamp_db(p);
    ui.set_eq_preamp(p.eq_preamp_db.unwrap_or(effective));
    ui.set_eq_preamp_label(if auto { format!("Pre-amp {} dB (automatic)", signed(effective)) } else { format!("Pre-amp {} dB", signed(effective)) }.into());
    ui.set_eq_balance(p.balance);
    ui.set_eq_balance_label(
        match (p.balance * 100.0).round() as i32 {
            0 => "center".to_string(),
            n if n < 0 => format!("L {}%", -n),
            n => format!("R {n}%"),
        }
        .into(),
    );
    ui.set_eq_mono(p.mono);
    ui.set_eq_limiter(p.limiter);
    ui.set_eq_ceiling(p.limiter_threshold_db);
    ui.set_eq_ceiling_label(format!("Ceiling {} dB", signed(p.limiter_threshold_db)).into());
    let preset = crossfeed_preset_of(p.clone());
    ui.set_eq_crossfeed_preset(preset.as_str().into());
    ui.set_eq_crossfeed(p.crossfeed_db);
    ui.set_eq_crossfeed_label(
        if p.crossfeed_db <= 0.0 {
            "Off".to_string()
        } else if preset.is_empty() {
            format!("Custom, {} dB: each ear also hears a little of the other channel, like loudspeakers. For headphones.", signed(p.crossfeed_db))
        } else {
            format!("{} dB: each ear also hears a little of the other channel, like loudspeakers. For headphones.", signed(p.crossfeed_db))
        }
        .into(),
    );
    ui.set_eq_cut(p.crossfeed_hz);
    ui.set_eq_cut_label(format!("Cutoff {:.0} Hz: how high up the other ear hears", p.crossfeed_hz).into());
    ui.set_eq_bypass(if p.sound_bypass { "No processing on this output, so nothing here changes the sound. Turn it off in Settings, under Sound." } else { "" }.into());
    let r = EQ_RANGES;
    ui.set_eq_ranges(ModelRc::new(VecModel::from(vec![r.gain.min, r.gain.max, r.preamp.min, r.preamp.max, r.balance.min, r.balance.max, r.limiter.min, r.limiter.max, r.crossfeed.min, r.crossfeed.max, r.crossfeed_cut.min, r.crossfeed_cut.max])));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hz_labels() {
        assert_eq!((hz(31.5), hz(63.0), hz(1000.0), hz(2500.0), hz(16000.0)), ("31.5".into(), "63".into(), "1k".into(), "2.5k".into(), "16k".into()));
    }

    #[test]
    fn curve_spans_the_box_flat_at_zero() {
        let c = curve(&[0.0; 10], 960.0, 96.0);
        assert_eq!(c.matches('L').count(), 96);
        assert!(c.starts_with("M0.0 48.0"), "{c}");
        assert!(c.trim_end().ends_with("L960.0 48.0"), "{c}");
    }
}
