//! Shared value types: equalizer bands, AutoMix track analysis, settings and transition plans
//! (exposed to Kotlin through uniffi `remote` records).

/// Ordinals are the wire format read by `dsp.rs`: only append.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EqKind {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
    AllPass,
    /// Shelves whose `q` is the RBJ slope S (1: steepest without ripple).
    LowShelfSlope,
    HighShelfSlope,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EqBand {
    pub kind: EqKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// A built-in equalizer curve.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PresetKind {
    #[default]
    Flat,
    BassBoost,
    BassCut,
    TrebleBoost,
    TrebleCut,
    VocalBoost,
    Loudness,
    SmallSpeakers,
}

/// A built-in curve from `dsp::eq_presets`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamedPreset {
    pub kind: PresetKind,
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
}

/// AutoMix analysis of one track (a `track_analysis` row). Times are ms from the file's start. Beat `n`
/// is at `beat_offset_ms + n * 60000 / bpm`; beats with `n % beats_per_bar == downbeat_phase` start a bar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackAnalysis {
    pub song_id: String,
    /// Rows older than `automix::ANALYSIS_VERSION` are redone.
    pub analysis_version: i32,
    /// Analysed length; a different length means a different file under the same id.
    pub duration_ms: i64,
    /// 0 when no tempo was found.
    pub bpm: f64,
    /// 0..1; below ~0.5 the grid is not used for beat matching.
    pub bpm_confidence: f32,
    /// The first beat of the grid, 0 <= offset < one beat.
    pub beat_offset_ms: f64,
    /// 0..1: how well one constant grid fits (the lower of beat spread, 14 ms to pass, and tempo drift
    /// between halves, 1.2 %).
    pub stability: f32,
    /// 0..beats_per_bar-1: which grid beats start a bar.
    pub downbeat_phase: i32,
    pub downbeat_confidence: f32,
    /// 4, or 3 for a waltz; 0 (old rows) means 4.
    pub beats_per_bar: i32,
    /// Integrated loudness of the mono downmix, BS.1770 K-weighting and gating. -70 for silence.
    pub lufs: f32,
    /// Camelot code: 1..12 = 1A..12A (minor), 13..24 = 1B..12B (major), 0 = unknown.
    pub key: i32,
    pub key_confidence: f32,
    /// First rise above and last fall below -55 dBFS.
    pub silence_start_ms: i64,
    pub silence_end_ms: i64,
    /// MixRamp points: first rise above and last fall below 17 dB under the track's loudness.
    pub mixramp_start_ms: i64,
    pub mixramp_end_ms: i64,
    /// Phrase-aligned cues (multiples of 8 bars from the first downbeat, preferring energy jumps).
    /// `intro_end_ms == silence_start_ms`: the track starts at full energy.
    pub intro_end_ms: i64,
    pub outro_start_ms: i64,
    /// Mean voice-band power share (0..1) and spectral centroid (Hz) over the outro and intro; 0 unknown.
    pub outro_vocal: f32,
    pub intro_vocal: f32,
    pub outro_centroid: f32,
    pub intro_centroid: f32,
    /// Beat grids of only the last and first `automix::GRID_WINDOW_S` seconds, where mixes happen (live
    /// tempo drifts over a whole song but not over half a minute). 0 unknown.
    pub outro_bpm: f64,
    pub outro_bpm_confidence: f32,
    pub outro_beat_offset_ms: f64,
    pub outro_stability: f32,
    pub outro_downbeat_phase: i32,
    pub intro_bpm: f64,
    pub intro_bpm_confidence: f32,
    pub intro_beat_offset_ms: f64,
    pub intro_stability: f32,
    pub intro_downbeat_phase: i32,
    /// The drop: first four-bar line where level, lows and chords reach the song's body; with the
    /// voice-band share over the eight bars before and after it. 0 when the song starts full or has no grid.
    pub drop_ms: i64,
    pub drop_runup_vocal: f32,
    pub drop_vocal: f32,
    /// Where the ending stops being worth playing (a closing breakdown in the last 24 s, or the silence
    /// before a short hidden track). 0: play to the end.
    pub exit_ms: i64,
    /// The last silence of 6 s or more inside the music; 0 when none.
    pub gap_ms: i64,
    pub gap_end_ms: i64,
    /// Voice-band share over the eight bars before the exit (or the music's end).
    pub exit_vocal: f32,
    /// Tonal energy of the drop's run-up (most chordal four bars) relative to the body, dB; a drum
    /// intro reads far below 0.
    pub drop_runup_tonal_db: f32,
    /// Per-end metre when measured separately; 0 means `beats_per_bar`.
    pub intro_beats_per_bar: i32,
    pub outro_beats_per_bar: i32,
    /// Grid source per end: `automix::beats::GRID_CLASSICAL`, `GRID_CHECKED` (classical, kept after the
    /// model was unsure) or `GRID_NEURAL`.
    pub intro_grid_source: i32,
    pub outro_grid_source: i32,
    /// Analysis time, ms since the epoch.
    pub analysed_ms: i64,
}

/// AutoMix settings as the planner sees them.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoMixSettings {
    /// Longest transition, seconds.
    pub max_transition_s: f32,
    pub beat_match: bool,
    /// Largest tempo change applied to the incoming track, percent.
    pub max_tempo_change_pct: f32,
    pub bass_swap: bool,
    pub filter_effects: bool,
    /// Beat-synced echo-out for clashing pairs (two vocals, far keys); a plain fade when off.
    pub echo_out: bool,
    /// Time-stretch (true) or varispeed (moves pitch, so held to 2 %).
    pub keep_pitch: bool,
    /// The tracks are consecutive on one album played in order: no transition.
    pub same_album_in_order: bool,
    /// Trim the incoming track to the outgoing one's loudness (off when ReplayGain levels them).
    pub match_loudness: bool,
    /// Tag BPM of the outgoing track (0 unknown), to settle half/double tempo errors.
    pub out_tag_bpm: f32,
    /// Server/tag BPM for the incoming track (0 unknown).
    pub in_tag_bpm: f32,
}

impl Default for AutoMixSettings {
    fn default() -> Self {
        AutoMixSettings {
            max_transition_s: 16.0,
            beat_match: true,
            max_tempo_change_pct: 6.0,
            bass_swap: true,
            filter_effects: true,
            echo_out: true,
            keep_pitch: true,
            same_album_in_order: false,
            match_loudness: false,
            out_tag_bpm: 0.0,
            in_tag_bpm: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    /// No overlap: the next track follows sample for sample.
    Gapless,
    /// Fixed cos/sin crossfade; nothing is known about either track.
    EqualPowerFade,
    /// Overlap chosen from loudness ramps and trimmed silence, optionally with a filter sweep.
    MixRampFade,
    /// Tempo-locked, bar-aligned mix, optionally with a bass swap.
    BeatMatched,
    /// The outgoing track exits into a beat-synced echo while the incoming track fades in over its tail.
    EchoOut,
}

/// Ordinals are carried in `automix::mixer::params`: only append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeCurve {
    /// cos/sin: constant power, for material that does not add coherently.
    EqualPower,
    Linear,
    /// sin²/cos²: constant amplitude, for beat-matched material that adds coherently.
    SineSquared,
}

/// A transition between two tracks. "Relative" fields count from the transition's start; -1 means unused.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionPlan {
    pub kind: TransitionKind,
    /// Position in the outgoing track where the transition starts. It stops at `out_start_ms + duration_ms`.
    pub out_start_ms: i64,
    /// Position in the incoming track that plays at the start of the transition.
    pub in_start_ms: i64,
    /// Length of the overlap, wall-clock.
    pub duration_ms: i64,
    /// Playback speed of the incoming track during the overlap (1 = native).
    pub tempo_ratio: f64,
    /// After the overlap the incoming track ramps back to native speed over this many of its beats...
    pub tempo_ramp_beats: i32,
    /// ...which takes this long, wall-clock. 0 when there is no tempo change.
    pub tempo_ramp_ms: i64,
    /// Time-stretch (true) or varispeed.
    pub keep_pitch: bool,
    pub fade_curve: FadeCurve,
    /// Relative. The outgoing gain goes 1 -> 0 between these.
    pub out_fade_start_ms: i64,
    pub out_fade_end_ms: i64,
    /// Relative. The incoming gain goes 0 -> 1 between these.
    pub in_fade_start_ms: i64,
    pub in_fade_end_ms: i64,
    /// Constant trim on the outgoing deck during the overlap.
    pub out_gain_db: f32,
    /// Trim on the incoming deck; the mixer glides it back to 0 dB over the last quarter of the overlap.
    pub in_gain_db: f32,
    /// Relative. Until here the incoming lows are cut; over `bass_swap_len_ms` they come in and the outgoing lows go.
    pub bass_swap_ms: i64,
    pub bass_swap_len_ms: i64,
    pub bass_cut_hz: f32,
    /// Relative. Low-pass sweep on the outgoing track, `filter_from_hz` -> `filter_to_hz`.
    pub filter_start_ms: i64,
    pub filter_end_ms: i64,
    pub filter_from_hz: f32,
    pub filter_to_hz: f32,
    /// Beat-synced echo on the outgoing deck, `-1` when off. `echo_delay_ms` is one outgoing beat;
    /// `echo_feedback` 0..1 is what each repeat keeps; `echo_wet_db` is the repeats' level.
    pub echo_delay_ms: i64,
    pub echo_feedback: f32,
    pub echo_wet_db: f32,
    /// Outro loop: capture this many ms and loop it for `duration_ms`; -1 captures the full duration.
    pub out_loop_ms: i64,
    /// High-pass sweep on the outgoing track, -1 when off.
    pub hp_start_ms: i64,
    pub hp_end_ms: i64,
    pub hp_from_hz: f32,
    pub hp_to_hz: f32,
    /// Relative. When both songs have vocals over the run-up, the incoming voice band (centred on
    /// `vocal_duck_hz`) is held `vocal_duck_db` down until here, released over `vocal_duck_release_ms`; -1 off.
    pub vocal_duck_until_ms: i64,
    pub vocal_duck_release_ms: i64,
    pub vocal_duck_db: f32,
    pub vocal_duck_hz: f32,
    /// Why this plan, for logs.
    pub reason: String,
}
