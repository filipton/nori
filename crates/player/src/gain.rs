//! Loudness normalisation: how loud each song plays so that songs of every mastering sound alike, from
//! its ReplayGain or R128 tags, the server's fallback, or its own measured loudness.
//!
//! The references the numbers are relative to:
//! - ReplayGain 2.0 tags (`REPLAYGAIN_TRACK_GAIN` and the OpenSubsonic `replayGain` a server sends) bring a
//!   song to -18 LUFS ([`RG2_REFERENCE_LUFS`]). ReplayGain 1.0's 89 dB SPL was set to the same level, so
//!   old tags read the same way.
//! - R128 tags (`R128_TRACK_GAIN`, `R128_ALBUM_GAIN`, in Opus files) are Q7.8 fixed point dB bringing a song
//!   to -23 LUFS, EBU R128's level ([`R128_REFERENCE_LUFS`]): as ReplayGain they are 5 dB more
//!   ([`r128_as_replay_gain_db`]). Navidrome converts them so itself when a file has no ReplayGain tags.
//! - A measured integrated loudness (LUFS) needs `target - lufs`.
//!
//! The target moves every gain by `target - (-18)`: -14 LUFS plays everything 4 dB louder than
//! ReplayGain's own level, -23 LUFS 5 dB quieter.
//!
//! A gain at or under 0 dB is a volume: applied as the player's (the output's own, under audio offload,
//! where the samples are never seen) it cannot clip, and the peak guard holds a song whose peak would
//! go over full scale. A gain over 0 dB cannot be a volume: it is put on the samples before the mix, as
//! floats, with the sound chain's look-ahead limiter behind it, and the song plays on the CPU
//! ([`offload_allows`]). How far it may go is the user's cap (0 dB: attenuation only, as before).

pub use crate::policy::{GainMode, GainTags};

/// The level ReplayGain 2.0 tags bring a song to, LUFS.
pub const RG2_REFERENCE_LUFS: f32 = -18.0;
/// The level R128 tags bring a song to (EBU R128), LUFS.
pub const R128_REFERENCE_LUFS: f32 = -23.0;
/// The loudness targets offered: streaming services' -14, Apple Music's -16, ReplayGain's own -18 (the
/// default) and broadcast's -23.
pub const TARGETS_LUFS: [f32; 4] = [-14.0, -16.0, -18.0, R128_REFERENCE_LUFS];
/// The furthest a target may be from -18, either way: a stored value outside is held to it.
const TARGET_RANGE_DB: f32 = 12.0;
/// The most positive gain any cap allows, dB.
pub const BOOST_MAX_DB: f32 = 12.0;

/// An R128 tag's value (Q7.8 fixed point: dB times 256, relative to -23 LUFS) as ReplayGain 2.0 dB
/// (relative to -18 LUFS). `R128_TRACK_GAIN=-1280` (-5 dB to -23 LUFS) is 0 dB of ReplayGain.
pub fn r128_as_replay_gain_db(q78: i32) -> f32 {
    q78 as f32 / 256.0 + (RG2_REFERENCE_LUFS - R128_REFERENCE_LUFS)
}

/// How the user wants songs levelled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainPrefs {
    pub mode: GainMode,
    /// Added to every tagged gain, dB.
    pub preamp_db: f32,
    /// The gain of a song with no tags and nothing measured, in ReplayGain terms (dB at -18 LUFS).
    pub untagged_db: f32,
    /// The loudness songs are brought to, LUFS.
    pub target_lufs: f32,
    /// The most a song is turned up, dB; 0 turns nothing up.
    pub boost_max_db: f32,
    /// A song without tags plays at its measured loudness, when it has been measured.
    pub measured: bool,
}

impl GainPrefs {
    /// ReplayGain as it was before targets and positive gain: -18 LUFS, attenuation only.
    pub fn attenuating(mode: GainMode, preamp_db: f32, untagged_db: f32) -> GainPrefs {
        GainPrefs { mode, preamp_db, untagged_db, target_lufs: RG2_REFERENCE_LUFS, boost_max_db: 0.0, measured: false }
    }

    /// What the target adds to a ReplayGain 2.0 gain, dB.
    pub fn target_offset_db(&self) -> f32 {
        let t = if self.target_lufs.is_finite() { self.target_lufs } else { RG2_REFERENCE_LUFS };
        (t - RG2_REFERENCE_LUFS).clamp(-TARGET_RANGE_DB, TARGET_RANGE_DB)
    }

    /// Whether a song may be turned up at all: the chain's limiter must then run behind it.
    pub fn boosts(&self) -> bool {
        self.mode != GainMode::Off && self.boost_max_db > 0.0
    }
}

/// What is known of one song's loudness.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SongLoudness {
    /// Its tags in ReplayGain 2.0 terms (R128 ones converted with [`r128_as_replay_gain_db`]); none when
    /// it has none at all.
    pub tags: Option<GainTags>,
    /// The server's `fallbackGain` (OpenSubsonic), dB in ReplayGain terms: for a song whose tags lack
    /// the gain asked for.
    pub fallback_db: Option<f32>,
    /// Its measured integrated loudness, LUFS (BS.1770), when it has been measured.
    pub measured_lufs: Option<f32>,
}

/// The gain a song plays at, linear: ReplayGain's (track or album, `in_album_run` for auto), then the
/// server's fallback, then the measured loudness, then the untagged level; moved to the target. At or
/// under 1 it is a volume, held under the peak guard; over 1 (only with a cap over 0 dB) it is for the
/// samples, held to the cap, the limiter doing what the peak guard did. Radio and a bit-perfect output
/// play at 1: the one has no song to level, the other must not be touched.
pub fn song_gain(p: &GainPrefs, song: &SongLoudness, in_album_run: bool, radio: bool, bit_perfect: bool) -> f32 {
    if p.mode == GainMode::Off || radio || bit_perfect {
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
        // Tags without a gain: the untagged level, with the pre-amp. No tags at all: the untagged level as
        // it is, the level a user picked for such songs.
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

/// Whether a song at `gain` may go to the output's own decoder (audio offload), where the only gain
/// is the output's volume: only while it is turned down or left as it is. A song turned up needs its
/// samples, and the limiter behind them, so it plays on the CPU.
pub fn offload_allows(gain: f32) -> bool {
    gain <= 1.0
}

/// The measure AutoMix's analysis stores (`TrackAnalysis::lufs`: BS.1770 integrated loudness of the
/// mid signal `(L + R) / 2`) as BS.1770's loudness of the song's own channels, LUFS. BS.1770 sums the
/// channels' powers, and `L² + R² = 2 (M² + S²)`, so a stereo song reads 3 dB more than its mid plus
/// what its side adds: nothing for a song centred in the middle, 0.4 dB for a side 10 dB under the mid
/// (usual for music), up to 3 dB for two unrelated channels. The side is not measured, so this reads a
/// wide song that much quiet, and levels it that much loud. A mono song (1 channel) reads as it is.
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
    fn r128_tags_read_as_replay_gain_five_db_up() {
        assert_eq!(r128_as_replay_gain_db(0), 5.0, "a song at -23 LUFS is 5 dB under ReplayGain's level");
        assert_eq!(r128_as_replay_gain_db(-1280), 0.0, "-5 dB to -23 LUFS: already at -18");
        assert_eq!(r128_as_replay_gain_db(-2432), -4.5, "Q7.8: -9.5 dB");
        assert!(close(r128_as_replay_gain_db(i16::MIN as i32), -123.0));
        // The same song either way: a song at -9 LUFS wants -9 dB of ReplayGain, -14 dB of R128.
        let rg = -9.0;
        let r128 = (-14.0 * 256.0) as i32;
        assert_eq!(r128_as_replay_gain_db(r128), rg);
    }

    #[test]
    fn each_mode_picks_its_tag() {
        let s = tagged(-6.0, -3.0);
        assert!(close(db(song_gain(&prefs(GainMode::Track), &s, true, false, false)), -6.0));
        assert!(close(db(song_gain(&prefs(GainMode::Album), &s, false, false, false)), -3.0));
        assert!(close(db(song_gain(&prefs(GainMode::Auto), &s, true, false, false)), -3.0), "in an album run: the album's");
        assert!(close(db(song_gain(&prefs(GainMode::Auto), &s, false, false, false)), -6.0), "elsewhere: the song's own");
        assert_eq!(song_gain(&prefs(GainMode::Off), &s, false, false, false), 1.0);
        assert_eq!(song_gain(&prefs(GainMode::Track), &s, false, true, false), 1.0, "radio");
        assert_eq!(song_gain(&prefs(GainMode::Track), &s, false, false, true), 1.0, "bit-perfect");
        // One tag missing: the other.
        let only_track = SongLoudness { tags: Some(GainTags { track_gain: Some(-4.0), ..Default::default() }), ..Default::default() };
        assert!(close(db(song_gain(&prefs(GainMode::Album), &only_track, false, false, false)), -4.0));
    }

    #[test]
    fn the_target_moves_every_gain_by_its_distance_from_replay_gain_s_level() {
        let s = tagged(-8.0, -8.0);
        for (target, want) in [(-18.0, -8.0), (-14.0, -4.0), (-16.0, -6.0), (-23.0, -13.0)] {
            let p = GainPrefs { target_lufs: target, ..prefs(GainMode::Track) };
            assert!(close(db(song_gain(&p, &s, false, false, false)), want), "{target} LUFS: {want} dB");
        }
        // A quiet song (ReplayGain +2 dB) at -14 LUFS wants +6 dB: turned up to the cap.
        let quiet = tagged(2.0, 2.0);
        let p = GainPrefs { target_lufs: -14.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &quiet, false, false, false)), 6.0));
        let p = GainPrefs { target_lufs: -14.0, boost_max_db: 3.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &quiet, false, false, false)), 3.0), "held to a +3 dB cap");
        // A stored target out of all reason is held within 12 dB of -18.
        let p = GainPrefs { target_lufs: 40.0, boost_max_db: 12.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &tagged(-20.0, -20.0), false, false, false)), -8.0));
        let p = GainPrefs { target_lufs: f32::NAN, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &s, false, false, false)), -8.0));
    }

    #[test]
    fn positive_gain_is_capped_and_the_limiter_not_the_peak_holds_it() {
        let quiet = SongLoudness { tags: Some(GainTags { track_gain: Some(4.0), track_peak: Some(0.9), ..Default::default() }), ..Default::default() };
        // Allowed: +4 dB, whatever the peak says (+4 dB on a 0.9 peak goes over full scale: the limiter's).
        assert!(close(db(song_gain(&prefs(GainMode::Track), &quiet, false, false, false)), 4.0));
        // Not allowed (cap 0): attenuation only, as ReplayGain always was here, peak guard included.
        let off = GainPrefs { boost_max_db: 0.0, ..prefs(GainMode::Track) };
        assert_eq!(song_gain(&off, &quiet, false, false, false), 1.0);
        let peaky = SongLoudness { tags: Some(GainTags { track_gain: Some(-1.0), track_peak: Some(1.25), ..Default::default() }), ..Default::default() };
        assert!(close(song_gain(&prefs(GainMode::Track), &peaky, false, false, false), 0.8), "turned down: the peak guard still holds the volume under full scale");
        // The pre-amp can turn a song up too.
        let s = tagged(-2.0, -2.0);
        let p = GainPrefs { preamp_db: 5.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &s, false, false, false)), 3.0));
        // A cap past the most there is is held to it.
        let p = GainPrefs { boost_max_db: 40.0, ..prefs(GainMode::Track) };
        assert!(close(db(song_gain(&p, &tagged(30.0, 30.0), false, false, false)), BOOST_MAX_DB));
    }

    #[test]
    fn a_song_without_tags_takes_the_server_s_fallback_then_its_measure_then_the_untagged_level() {
        let p = prefs(GainMode::Track);
        let nothing = SongLoudness::default();
        assert!(close(db(song_gain(&p, &nothing, false, false, false)), -6.0), "the untagged level");
        let measured = SongLoudness { measured_lufs: Some(-9.0), ..Default::default() };
        assert!(close(db(song_gain(&p, &measured, false, false, false)), -9.0), "-9 LUFS to -18: -9 dB");
        let quiet = SongLoudness { measured_lufs: Some(-21.0), ..Default::default() };
        assert!(close(db(song_gain(&p, &quiet, false, false, false)), 3.0), "-21 LUFS: +3 dB");
        let p14 = GainPrefs { target_lufs: -14.0, ..p };
        assert!(close(db(song_gain(&p14, &measured, false, false, false)), -5.0), "the target is the target");
        let unmeasured = GainPrefs { measured: false, ..p };
        assert!(close(db(song_gain(&unmeasured, &measured, false, false, false)), -6.0), "measuring off: the untagged level");
        let silent = SongLoudness { measured_lufs: Some(-70.0), ..Default::default() };
        assert!(close(db(song_gain(&p, &silent, false, false, false)), -6.0), "a silence measured is no loudness");
        let fallback = SongLoudness { fallback_db: Some(-4.0), measured_lufs: Some(-9.0), ..Default::default() };
        assert!(close(db(song_gain(&p, &fallback, false, false, false)), -4.0), "the server's fallback before a measure");
        // The untagged level moves with the target, as a guess at a tagged song's gain.
        assert!(close(db(song_gain(&GainPrefs { target_lufs: -23.0, ..p }, &nothing, false, false, false)), -11.0));
    }

    #[test]
    fn as_before_by_default() {
        // Targets and positive gain left out: ReplayGain as `policy::replay_gain` always did it.
        let p = GainPrefs::attenuating(GainMode::Track, -3.0, -6.0);
        assert!(close(song_gain(&p, &SongLoudness::default(), false, false, false), 10f32.powf(-6.0 / 20.0)), "no tags: no pre-amp");
        let empty = SongLoudness { tags: Some(GainTags::default()), ..Default::default() };
        assert!(close(song_gain(&p, &empty, false, false, false), 10f32.powf(-9.0 / 20.0)), "tags without gains: with the pre-amp");
        assert_eq!(song_gain(&p, &tagged(6.0, 6.0), false, false, false), 1.0);
    }

    #[test]
    fn only_a_song_turned_up_keeps_off_the_audio_chip() {
        assert!(offload_allows(1.0) && offload_allows(0.5) && offload_allows(0.0));
        assert!(!offload_allows(1.0001) && !offload_allows(2.0));
        let quiet = tagged(3.0, 3.0);
        assert!(!offload_allows(song_gain(&prefs(GainMode::Track), &quiet, false, false, false)), "turned up: on the CPU");
        assert!(offload_allows(song_gain(&GainPrefs { boost_max_db: 0.0, ..prefs(GainMode::Track) }, &quiet, false, false, false)), "no positive gain: the chip may");
        assert!(offload_allows(song_gain(&prefs(GainMode::Track), &tagged(-7.0, -7.0), false, false, false)), "turned down: a volume");
    }

    #[test]
    fn a_stereo_measure_of_the_mid_reads_3_db_up() {
        assert!(close(stereo_loudness_of_mid(-17.0, 2), -13.99));
        assert!(close(stereo_loudness_of_mid(-17.0, 0), -13.99), "unknown: stereo");
        assert_eq!(stereo_loudness_of_mid(-17.0, 1), -17.0);
    }
}
