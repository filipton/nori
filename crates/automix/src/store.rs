//! The `track_analysis` and `vocal_curve` tables, and the streaming analyser that feeds them.

use nori_model::TrackAnalysis;
use nori_player::automix::vocal::VocalCurve;
use rusqlite::{params, Connection, OptionalExtension};

use super::analysis::Analyzer;
use super::ANALYSIS_VERSION;

const COLUMNS: &str = "song_id, analysis_version, duration_ms, bpm, bpm_confidence, beat_offset_ms, stability, downbeat_phase, \
     downbeat_confidence, lufs, key, key_confidence, silence_start_ms, silence_end_ms, mixramp_start_ms, mixramp_end_ms, \
     intro_end_ms, outro_start_ms, outro_vocal, intro_vocal, outro_centroid, intro_centroid, analysed_ms, \
     outro_bpm, outro_bpm_confidence, outro_beat_offset_ms, outro_stability, outro_downbeat_phase, \
     intro_bpm, intro_bpm_confidence, intro_beat_offset_ms, intro_stability, intro_downbeat_phase, beats_per_bar, \
     drop_ms, drop_runup_vocal, drop_vocal, exit_ms, gap_ms, gap_end_ms, exit_vocal, drop_runup_tonal_db, intro_beats_per_bar, outro_beats_per_bar, intro_grid_source, outro_grid_source";

pub fn put(c: &Connection, a: &TrackAnalysis) -> rusqlite::Result<()> {
    c.prepare_cached(&format!(
        "INSERT OR REPLACE INTO track_analysis(server, {COLUMNS}) VALUES(sid(), ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46)"
    ))?
    .execute(params![
        a.song_id,
        a.analysis_version,
        a.duration_ms,
        a.bpm,
        a.bpm_confidence,
        a.beat_offset_ms,
        a.stability,
        a.downbeat_phase,
        a.downbeat_confidence,
        a.lufs,
        a.key,
        a.key_confidence,
        a.silence_start_ms,
        a.silence_end_ms,
        a.mixramp_start_ms,
        a.mixramp_end_ms,
        a.intro_end_ms,
        a.outro_start_ms,
        a.outro_vocal,
        a.intro_vocal,
        a.outro_centroid,
        a.intro_centroid,
        a.analysed_ms,
        a.outro_bpm,
        a.outro_bpm_confidence,
        a.outro_beat_offset_ms,
        a.outro_stability,
        a.outro_downbeat_phase,
        a.intro_bpm,
        a.intro_bpm_confidence,
        a.intro_beat_offset_ms,
        a.intro_stability,
        a.intro_downbeat_phase,
        a.beats_per_bar,
        a.drop_ms,
        a.drop_runup_vocal,
        a.drop_vocal,
        a.exit_ms,
        a.gap_ms,
        a.gap_end_ms,
        a.exit_vocal,
        a.drop_runup_tonal_db,
        a.intro_beats_per_bar,
        a.outro_beats_per_bar,
        a.intro_grid_source,
        a.outro_grid_source
    ])
    .map(|_| ())
}

pub fn get(c: &Connection, song_id: &str) -> rusqlite::Result<Option<TrackAnalysis>> {
    c.prepare_cached(&format!("SELECT {COLUMNS} FROM track_analysis WHERE server=sid() AND song_id=?1"))?
        .query_row([song_id], |r| {
            Ok(TrackAnalysis {
                song_id: r.get(0)?,
                analysis_version: r.get(1)?,
                duration_ms: r.get(2)?,
                bpm: r.get(3)?,
                bpm_confidence: r.get(4)?,
                beat_offset_ms: r.get(5)?,
                stability: r.get(6)?,
                downbeat_phase: r.get(7)?,
                downbeat_confidence: r.get(8)?,
                lufs: r.get(9)?,
                key: r.get(10)?,
                key_confidence: r.get(11)?,
                silence_start_ms: r.get(12)?,
                silence_end_ms: r.get(13)?,
                mixramp_start_ms: r.get(14)?,
                mixramp_end_ms: r.get(15)?,
                intro_end_ms: r.get(16)?,
                outro_start_ms: r.get(17)?,
                outro_vocal: r.get(18).unwrap_or(0.0),
                intro_vocal: r.get(19).unwrap_or(0.0),
                outro_centroid: r.get(20).unwrap_or(0.0),
                intro_centroid: r.get(21).unwrap_or(0.0),
                analysed_ms: r.get(22)?,
                outro_bpm: r.get(23).unwrap_or(0.0),
                outro_bpm_confidence: r.get(24).unwrap_or(0.0),
                outro_beat_offset_ms: r.get(25).unwrap_or(0.0),
                outro_stability: r.get(26).unwrap_or(0.0),
                outro_downbeat_phase: r.get(27).unwrap_or(0),
                intro_bpm: r.get(28).unwrap_or(0.0),
                intro_bpm_confidence: r.get(29).unwrap_or(0.0),
                intro_beat_offset_ms: r.get(30).unwrap_or(0.0),
                intro_stability: r.get(31).unwrap_or(0.0),
                intro_downbeat_phase: r.get(32).unwrap_or(0),
                beats_per_bar: r.get(33).unwrap_or(0),
                drop_ms: r.get(34).unwrap_or(0),
                drop_runup_vocal: r.get(35).unwrap_or(0.0),
                drop_vocal: r.get(36).unwrap_or(0.0),
                exit_ms: r.get(37).unwrap_or(0),
                gap_ms: r.get(38).unwrap_or(0),
                gap_end_ms: r.get(39).unwrap_or(0),
                exit_vocal: r.get(40).unwrap_or(0.0),
                drop_runup_tonal_db: r.get(41).unwrap_or(0.0),
                intro_beats_per_bar: r.get(42).unwrap_or(0),
                outro_beats_per_bar: r.get(43).unwrap_or(0),
                intro_grid_source: r.get(44).unwrap_or(0),
                outro_grid_source: r.get(45).unwrap_or(0),
            })
        })
        .optional()
}

/// Stores a fresh classical analysis, keeping the Beat This! grids and "model has looked" marks of the stored row
/// (`beats::carry`). Returns the row as stored.
pub fn put_measured(c: &Connection, mut a: TrackAnalysis) -> rusqlite::Result<TrackAnalysis> {
    if let Some(old) = get(c, &a.song_id)? {
        super::beats::carry(&old, &mut a);
    }
    put(c, &a)?;
    Ok(a)
}

/// Stores a whole song's measurement: its analysis (keeping the beat model's grids) and vocal curve.
pub fn put_finished(c: &Connection, song_id: &str, features: &crate::analysis::Features) -> rusqlite::Result<TrackAnalysis> {
    let stored = put_measured(c, crate::finish(song_id, features).track)?;
    put_voice(c, song_id, &features.voice_curve())?;
    Ok(stored)
}

/// Stores a song's vocal activity curve (`vocal.rs`), used to check synced lyrics against the audio.
pub fn put_voice(c: &Connection, song_id: &str, curve: &VocalCurve) -> rusqlite::Result<()> {
    c.prepare_cached("INSERT OR REPLACE INTO vocal_curve(server, song_id, curve) VALUES(sid(), ?1, ?2)")?.execute(params![song_id, curve.encode()]).map(|_| ())
}

/// A song's vocal activity curve; None when absent or stored in an old format.
pub fn get_voice(c: &Connection, song_id: &str) -> rusqlite::Result<Option<VocalCurve>> {
    let blob: Option<Vec<u8>> = c.prepare_cached("SELECT curve FROM vocal_curve WHERE server=sid() AND song_id=?1")?.query_row([song_id], |r| r.get(0)).optional()?;
    Ok(blob.and_then(|b| VocalCurve::decode(&b)))
}

/// The `ids` with a current analysis the beat model has not yet looked at, in order. Songs without a current
/// analysis are left out.
pub fn neural_missing(c: &Connection, ids: &[String]) -> rusqlite::Result<Vec<String>> {
    let mut out = Vec::new();
    for id in ids {
        if get(c, id)?.is_some_and(|r| r.analysis_version >= ANALYSIS_VERSION && super::beats::needs_model(&r)) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// The `ids` with no row or a row from an older analysis version, in order.
pub fn missing(c: &Connection, ids: &[String]) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare_cached("SELECT analysis_version FROM track_analysis WHERE server=sid() AND song_id=?1")?;
    let mut out = Vec::new();
    for id in ids {
        let v: Option<i32> = st.query_row([id], |r| r.get(0)).optional()?;
        if v.is_none_or(|v| v < ANALYSIS_VERSION) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// A song measured as it is decoded, fed buffer by buffer.
pub struct AnalysisStream {
    a: Analyzer,
    channels: usize,
}

impl AnalysisStream {
    /// `expected_ms` (0 if unknown) sizes the buffers so feeding never reallocates.
    pub fn new(rate: u32, channels: usize, expected_ms: u64) -> Self {
        AnalysisStream { a: Analyzer::new(rate.max(1), expected_ms), channels: channels.clamp(1, 8) }
    }

    /// Interleaved float samples.
    pub fn feed_f32(&mut self, x: &[f32]) {
        self.a.feed_interleaved(x, self.channels, |v| v);
    }

    pub fn into_analyzer(self) -> Analyzer {
        self.a
    }
}
