
use nori_player::automix::synth::Synth;

use super::*;
use crate::Core;

#[test]
fn analysis_is_stored_and_reported_missing() {
    let core = Core::new(String::new(), "t".into()).unwrap();
    assert_eq!(core.analysis_get("a".into()).unwrap(), None);
    assert_eq!(core.analysis_missing(vec!["a".into(), "b".into()]).unwrap(), vec!["a".to_string(), "b".to_string()]);
    let s = Synth::new(120.0);
    let bytes: Vec<u8> = s.render().iter().flat_map(|v| v.to_le_bytes()).collect();
    let t = core.analysis_run("a".into(), bytes, s.rate as i32, 1, PCM_FLOAT).unwrap();
    assert_eq!(core.analysis_get("a".into()).unwrap(), Some(t.clone()));
    assert_eq!(core.analysis_missing(vec!["a".into(), "b".into()]).unwrap(), vec!["b".to_string()]);
    core.analysis_store(TrackAnalysis { song_id: "b".into(), analysis_version: ANALYSIS_VERSION - 1, ..t.clone() }).unwrap();
    assert_eq!(core.analysis_missing(vec!["a".into(), "b".into()]).unwrap(), vec!["b".to_string()], "old version");
    core.analysis_store(TrackAnalysis { bpm: 99.5, ..t.clone() }).unwrap();
    assert_eq!(core.analysis_get("a".into()).unwrap().unwrap().bpm, 99.5, "store replaces");
}

#[test]
fn analysis_finish_stores_row_and_voice_curve() {
    let core = Core::new(String::new(), "t".into()).unwrap();
    let s = Synth::new(128.0);
    let x = s.render();
    let mut a = analysis::Analyzer::new(s.rate, 60_000);
    a.feed(&x[..s.rate as usize * 10]);
    assert_eq!(core.analysis_finish("x", a).unwrap(), None, "10 s is not a track");
    let mut a = analysis::Analyzer::new(s.rate, 60_000);
    a.feed(&x);
    let t = core.analysis_finish("x", a).unwrap().unwrap();
    assert!((t.bpm - 128.0).abs() < 0.05);
    assert_eq!(core.analysis_get("x".into()).unwrap(), Some(t));
    let voice = core.analysis_voice("x").unwrap().expect("a curve");
    let secs = x.len() as f64 / s.rate as f64;
    assert!((voice.seconds() - secs).abs() < 0.5, "{} s of curve for {secs} s", voice.seconds());
    assert_eq!(core.analysis_voice("none").unwrap(), None);
    core.analysis_clear().unwrap();
    assert_eq!(core.analysis_voice("x").unwrap(), None);
}

impl Core {
    fn analysis_store(&self, analysis: TrackAnalysis) -> Result<()> {
        Ok(put(&self.db.lock(), &analysis)?)
    }

    fn analysis_run(&self, song_id: String, pcm: Vec<u8>, sample_rate: i32, channels: i32, encoding: i32) -> Result<TrackAnalysis> {
        let a = analyse_bytes(&song_id, &pcm, sample_rate, channels, encoding).track;
        Ok(nori_automix::store::put_measured(&self.db.lock(), a)?)
    }
}

#[test]
fn beat_model_grid_survives_reanalysis() {
    use nori_player::automix::beats::{EndGrid, MixEnd, GRID_CHECKED, GRID_NEURAL};
    let core = Core::new(String::new(), "t".into()).unwrap();
    let row = TrackAnalysis {
        song_id: "s".into(),
        analysis_version: ANALYSIS_VERSION,
        duration_ms: 200_000,
        silence_start_ms: 1_500,
        silence_end_ms: 198_000,
        intro_bpm: 100.0,
        outro_bpm: 101.0,
        beats_per_bar: 4,
        ..Default::default()
    };
    core.analysis_store(row.clone()).unwrap();
    core.analysis_store(TrackAnalysis { song_id: "old".into(), analysis_version: 1, ..row.clone() }).unwrap();
    assert_eq!(core.analysis_neural_missing(vec!["s".into(), "old".into(), "none".into()]).unwrap(), vec!["s".to_string()]);
    let sure = EndGrid { bpm: 90.0, offset_ms: 10.0, confidence: 1.0, stability: 1.0, downbeat_phase: 0, beats_per_bar: 3, other_phase: None, anchor_ms: 0.0 };
    assert!(core.analysis_neural_store("s", MixEnd::Intro, Some(sure)).unwrap());
    assert!(!core.analysis_neural_store("s", MixEnd::Outro, None).unwrap());
    assert!(!core.analysis_neural_store("none", MixEnd::Outro, None).unwrap());
    let stored = core.analysis_get("s".into()).unwrap().unwrap();
    assert_eq!((stored.intro_bpm, stored.intro_beats_per_bar, stored.intro_grid_source, stored.outro_grid_source), (90.0, 3, GRID_NEURAL, GRID_CHECKED));
    assert!(core.analysis_neural_missing(vec!["s".into()]).unwrap().is_empty());
    // A classical re-analysis keeps the model's grid.
    let s = Synth { secs: 200.0, ..Synth::new(120.0) };
    let bytes: Vec<u8> = s.render().iter().flat_map(|v| v.to_le_bytes()).collect();
    let again = core.analysis_run("s".into(), bytes, s.rate as i32, 1, PCM_FLOAT).unwrap();
    assert_eq!((again.intro_grid_source, again.intro_bpm, again.outro_grid_source), (GRID_NEURAL, 90.0, GRID_CHECKED));
}
