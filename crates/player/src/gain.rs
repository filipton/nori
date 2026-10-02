//! Loudness normalisation from ReplayGain/R128 tags, the server's fallback gain, or measured loudness.
//!
//! ReplayGain 2.0 (and 1.0) tags target -18 LUFS, R128 tags -23 LUFS (Q7.8 dB, 5 dB less as
//! ReplayGain); a measured loudness needs `target - lufs`. The target shifts every gain by
//! `target - (-18)`.
//!
//! Gains up to 0 dB act as a volume, held under the peak. Gains above 0 dB are applied to float samples
//! with the limiter behind them, so the song cannot be offloaded ([`offload_allows`]); the user's cap
//! bounds them (0: attenuation only).

pub use crate::policy::{GainMode, GainTags};

/// ReplayGain 2.0 reference, LUFS.
pub const RG2_REFERENCE_LUFS: f32 = -18.0;
/// EBU R128 reference, LUFS.
pub const R128_REFERENCE_LUFS: f32 = -23.0;
/// Offered targets: streaming -14, Apple Music -16, ReplayGain -18 (default), broadcast -23.
pub const TARGETS_LUFS: [f32; 4] = [-14.0, -16.0, -18.0, R128_REFERENCE_LUFS];
/// Targets are clamped to within this of -18.
const TARGET_RANGE_DB: f32 = 12.0;
/// Largest boost any cap allows, dB.
pub const BOOST_MAX_DB: f32 = 12.0;

/// An R128 tag (Q7.8 dB relative to -23 LUFS) as ReplayGain 2.0 dB (relative to -18 LUFS).
pub fn r128_as_replay_gain_db(q78: i32) -> f32 {
    q78 as f32 / 256.0 + (RG2_REFERENCE_LUFS - R128_REFERENCE_LUFS)
}

/// ReplayGain settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainPrefs {
    pub mode: GainMode,
    /// Added to every tagged gain, dB.
    pub preamp_db: f32,
    /// Gain for songs with no tags and no measurement, ReplayGain dB.
    pub untagged_db: f32,
    pub target_lufs: f32,
    /// Maximum boost, dB; 0 only attenuates.
    pub boost_max_db: f32,
    /// Use measured loudness for untagged songs.
    pub measured: bool,
}

impl GainPrefs {
    /// -18 LUFS, attenuation only, no measurement.
    pub fn attenuating(mode: GainMode, preamp_db: f32, untagged_db: f32) -> GainPrefs {
        GainPrefs { mode, preamp_db, untagged_db, target_lufs: RG2_REFERENCE_LUFS, boost_max_db: 0.0, measured: false }
    }

    /// dB the target adds to a ReplayGain 2.0 gain.
    pub fn target_offset_db(&self) -> f32 {
        let t = if self.target_lufs.is_finite() { self.target_lufs } else { RG2_REFERENCE_LUFS };
        (t - RG2_REFERENCE_LUFS).clamp(-TARGET_RANGE_DB, TARGET_RANGE_DB)
    }

    /// Songs may be boosted (the limiter must then run).
    pub fn boosts(&self) -> bool {
        self.mode != GainMode::Off && self.boost_max_db > 0.0
    }
}

/// Loudness facts about one song.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SongLoudness {
    /// Tags in ReplayGain 2.0 terms (R128 converted); `None` when it has no tags.
    pub tags: Option<GainTags>,
    /// The server's OpenSubsonic `fallbackGain`, ReplayGain dB.
    pub fallback_db: Option<f32>,
    /// Measured integrated loudness (BS.1770), LUFS.
    pub measured_lufs: Option<f32>,
}

/// Linear gain for a song: tag (track or album; `in_album_run` for auto), else the server's fallback,
/// else measured loudness, else the untagged level, shifted to the target. Up to 1 it is held under the
/// peak; above 1 (only with a cap) it is held to the cap and the limiter guards the peaks.
pub fn song_gain(p: &GainPrefs, song: &SongLoudness, in_album_run: bool) -> f32 {
    if p.mode == GainMode::Off {
        return 1.0;
    }
    let album = match p.mode {
        GainMode::Album => true,
        GainMode::Auto => in_album_run,
        _ => false,
    };
    let offset = p.target_offset_db();
    let tagged = song.tags.as_ref().and_then(|g| if album { g.album_gain.or(g.track_gain) } else { g.track_gain.or(g.album_gain) });
    let peak = song.tags.as_ref().and_then(|g| if album { g.album_peak.or(g.track_peak) } else { g.track_peak.or(g.album_peak) }).unwrap_or(0.0);
    let measured = song.measured_lufs.filter(|l| p.measured && l.is_finite() && *l > -70.0);
    let db = match (tagged.or(song.fallback_db), measured) {
        (Some(g), _) => g + offset + p.preamp_db,
        (None, Some(lufs)) => p.target_lufs.clamp(RG2_REFERENCE_LUFS - TARGET_RANGE_DB, RG2_REFERENCE_LUFS + TARGET_RANGE_DB) - lufs + p.preamp_db,
        // Tags without a gain get the pre-amp; no tags at all do not.
        (None, None) if song.tags.is_some() => p.untagged_db + offset + p.preamp_db,
        (None, None) => p.untagged_db + offset,
    };
    let v = 10f32.powf(db / 20.0);
    if !v.is_finite() {
        return 1.0;
    }
    let cap = 10f32.powf(p.boost_max_db.clamp(0.0, BOOST_MAX_DB) / 20.0);
    if v > 1.0 && cap > 1.0 {
        return v.min(cap);
    }
    let v = if peak > 0.0 { v.min(1.0 / peak) } else { v };
    v.clamp(0.0, 1.0)
}

/// Whether a song at `gain` may be offloaded (only a volume is available there, so no boost).
pub fn offload_allows(gain: f32) -> bool {
    gain <= 1.0
}

/// Converts the analysis' mid-signal loudness (`TrackAnalysis::lufs`, of `(L + R) / 2`) to BS.1770
/// stereo loudness: +3 dB for stereo (`L² + R² = 2 (M² + S²)`, ignoring the unmeasured side).
pub fn stereo_loudness_of_mid(mid_lufs: f32, channels: u32) -> f32 {
    if channels == 1 {
        mid_lufs
    } else {
        mid_lufs + 10.0 * 2f32.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(v: f32) -> f32 {
        20.0 * v.log10()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn tagged(track: f32, album: f32) -> SongLoudness {
        SongLoudness { tags: Some(GainTags { track_gain: Some(track), album_gain: Some(album), ..Default::default() }), ..Default::default() }
    }

    fn prefs(mode: GainMode) -> GainPrefs {
        GainPrefs { mode, preamp_db: 0.0, untagged_db: -6.0, target_lufs: -18.0, boost_max_db: 6.0, measured: true }
    }

    #[test]
    fn gain_from_tags() {
        assert_eq!(r128_as_replay_gain_db(0), 5.0, "a song at -23 LUFS is 5 dB under ReplayGain's level");
        assert_eq!(r128_as_replay_gain_db(-1280), 0.0, "-5 dB to -23 LUFS: already at -18");
        assert_eq!(r128_as_replay_gain_db(-2432), -4.5, "Q7.8: -9.5 dB");
        assert!(close(r128_as_replay_gain_db(i16::MIN as i32), -123.0));
        // The same song either way: a song at -9 LUFS wants -9 dB of ReplayGain, -14 dB of R128.
        let rg = -9.0;
        let r128 = (-14.0 * 256.0) as i32;
        assert_eq!(r128_as_replay_gain_db(r128), rg);

        // Mode picks tag.
        let s = tagged(-6.0, -3.0);
        assert!(close(db(song_gain(&prefs(GainMode::Track), &s, true)), -6.0));
        assert!(close(db(song_gain(&prefs(GainMode::Album), &s, false)), -3.0));
        assert!(close(db(song_gain(&prefs(GainMode::Auto), &s, true)), -3.0), "in an album run: the album's");
        assert!(close(db(song_gain(&prefs(GainMode::Auto), &s, false)), -6.0), "elsewhere: the song's own");
        assert_eq!(song_gain(&prefs(GainMode::Off), &s, false), 1.0);
        // One tag missing: the other.
        let only_track = SongLoudness { tags: Some(GainTags { track_gain: Some(-4.0), ..Default::default() }), ..Default::default() };
        assert!(close(db(song_gain(&prefs(GainMode::Album), &only_track, false)), -4.0));

        // Target shifts gain.
        let s = tagged(-8.0, -8.0);
        for (target, want) in [(-18.0, -8.0), (-14.0, -4.0), (-16.0, -6.0), (-23.0, -13.0)] {
            let p = GainPrefs { target_lufs: target, ..prefs(GainMode::Track) };
            assert!(close(db(song_gain(&p, &s, false)), want), "{target} LUFS: {want} dB");
        }
        // A quiet song (ReplayGain +2 dB) at -14 LUFS wants +6 dB: turned up to the cap.
        let quiet = tagged(2.0, 2.0);
        let p = GainPrefs { target_lufs: -14.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &quiet, false)), 6.0));
        let p = GainPrefs { target_lufs: -14.0, boost_max_db: 3.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &quiet, false)), 3.0), "held to a +3 dB cap");
        // A stored target out of all reason is held within 12 dB of -18.
        let p = GainPrefs { target_lufs: 40.0, boost_max_db: 12.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &tagged(-20.0, -20.0), false)), -8.0));
        let p = GainPrefs { target_lufs: f32::NAN, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &s, false)), -8.0));

        // Untagged fallback order.
        let p = prefs(GainMode::Track);
        let nothing = SongLoudness::default();
        assert!(close(db(song_gain(&p, &nothing, false)), -6.0), "the untagged level");
        let measured = SongLoudness { measured_lufs: Some(-9.0), ..Default::default() };
        assert!(close(db(song_gain(&p, &measured, false)), -9.0), "-9 LUFS to -18: -9 dB");
        let quiet = SongLoudness { measured_lufs: Some(-21.0), ..Default::default() };
        assert!(close(db(song_gain(&p, &quiet, false)), 3.0), "-21 LUFS: +3 dB");
        let p14 = GainPrefs { target_lufs: -14.0, ..p };
        assert!(close(db(song_gain(&p14, &measured, false)), -5.0), "the target is the target");
        let unmeasured = GainPrefs { measured: false, ..p };
        assert!(close(db(song_gain(&unmeasured, &measured, false)), -6.0), "measuring off: the untagged level");
        let silent = SongLoudness { measured_lufs: Some(-70.0), ..Default::default() };
        assert!(close(db(song_gain(&p, &silent, false)), -6.0), "a silence measured is no loudness");
        let fallback = SongLoudness { fallback_db: Some(-4.0), measured_lufs: Some(-9.0), ..Default::default() };
        assert!(close(db(song_gain(&p, &fallback, false)), -4.0), "the server's fallback before a measure");
        // The untagged level moves with the target, as a guess at a tagged song's gain.
        assert!(close(db(song_gain(&GainPrefs { target_lufs: -23.0, ..p }, &nothing, false)), -11.0));
    }

    #[test]
    fn caps_and_cuts() {
        let quiet = SongLoudness { tags: Some(GainTags { track_gain: Some(4.0), track_peak: Some(0.9), ..Default::default() }), ..Default::default() };
        // Allowed: +4 dB, whatever the peak says (+4 dB on a 0.9 peak goes over full scale: the limiter's).
        assert!(close(db(song_gain(&prefs(GainMode::Track), &quiet, false)), 4.0));
        // Not allowed (cap 0): attenuation only, as ReplayGain always was here, peak guard included.
        let off = GainPrefs { boost_max_db: 0.0, ..prefs(GainMode::Track) };
        assert_eq!(song_gain(&off, &quiet, false), 1.0);
        let peaky = SongLoudness { tags: Some(GainTags { track_gain: Some(-1.0), track_peak: Some(1.25), ..Default::default() }), ..Default::default() };
        assert!(close(song_gain(&prefs(GainMode::Track), &peaky, false), 0.8), "turned down: the peak guard still holds the volume under full scale");
        // The pre-amp can turn a song up too.
        let s = tagged(-2.0, -2.0);
        let p = GainPrefs { preamp_db: 5.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &s, false)), 3.0));
        // A cap past the most there is is held to it.
        let p = GainPrefs { boost_max_db: 40.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &tagged(30.0, 30.0), false)), BOOST_MAX_DB));

        // Attenuating prefs.
        let p = GainPrefs::attenuating(GainMode::Track, -3.0, -6.0);
        assert!(close(song_gain(&p, &SongLoudness::default(), false), 10f32.powf(-6.0 / 20.0)), "no tags: no pre-amp");
        let empty = SongLoudness { tags: Some(GainTags::default()), ..Default::default() };
        assert!(close(song_gain(&p, &empty, false), 10f32.powf(-9.0 / 20.0)), "tags without gains: with the pre-amp");
        assert_eq!(song_gain(&p, &tagged(6.0, 6.0), false), 1.0);
        let peaky = SongLoudness { tags: Some(GainTags { track_gain: Some(-1.0), track_peak: Some(1.25), ..Default::default() }), ..Default::default() };
        assert!(close(song_gain(&GainPrefs::attenuating(GainMode::Track, 0.0, -6.0), &peaky, false), 0.8), "no clipping");
        let s = tagged(-6.0, -3.0);
        let auto = GainPrefs::attenuating(GainMode::Auto, 0.0, -6.0);
        assert!(close(db(song_gain(&auto, &s, true)), -3.0), "inside an album run: album gain");
        assert!(close(db(song_gain(&auto, &s, false)), -6.0), "elsewhere: track gain");
    }

    #[test]
    fn only_boost_blocks_offload() {
        assert!(offload_allows(1.0) && offload_allows(0.5) && offload_allows(0.0));
        assert!(!offload_allows(1.0001) && !offload_allows(2.0));
        let quiet = tagged(3.0, 3.0);
        assert!(!offload_allows(song_gain(&prefs(GainMode::Track), &quiet, false)), "turned up: on the CPU");
        assert!(offload_allows(song_gain(&GainPrefs { boost_max_db: 0.0, ..prefs(GainMode::Track) }, &quiet, false)), "no positive gain: the chip may");
        assert!(offload_allows(song_gain(&prefs(GainMode::Track), &tagged(-7.0, -7.0), false)), "turned down: a volume");
    }

    #[test]
    fn stereo_loudness_is_mid_plus_3_db() {
        assert!(close(stereo_loudness_of_mid(-17.0, 2), -13.99));
        assert!(close(stereo_loudness_of_mid(-17.0, 0), -13.99), "unknown: stereo");
        assert_eq!(stereo_loudness_of_mid(-17.0, 1), -17.0);
    }
}
