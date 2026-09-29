//! The values the player works with, shared by every platform: equalizer bands, what AutoMix knows
//! about a track, the user's AutoMix settings, and the transition plans. Plain Rust types; the
//! Android library exposes them to Kotlin through uniffi's `remote` records.

/// The order is the wire format: `dsp.rs` reads the ordinal out of the flat band array, so only append.
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
    /// Shelves whose `q` is the RBJ slope S (1 is the steepest slope without ripple) rather than a Q.
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

/// Which built-in curve a preset is; the client names it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
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

/// One of the built-in curves from `dsp::eq_presets`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamedPreset {
    pub kind: PresetKind,
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
}

/// What AutoMix knows about one track, from `automix::analysis`. One row in `track_analysis`.
/// Times are milliseconds from the start of the file. The beat grid is not stored beat by beat: beat `n` sits at
/// `beat_offset_ms + n * 60000 / bpm`, and beats with `n % beats_per_bar == downbeat_phase` start a bar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackAnalysis {
    pub song_id: String,
    /// Rows older than `automix::ANALYSIS_VERSION` are reported by `analysis_missing` so they get redone.
    pub analysis_version: i32,
    /// Length of the audio that was analysed; a different file under the same id shows up as a different length.
    pub duration_ms: i64,
    /// 0 when no tempo was found.
    pub bpm: f64,
    /// 0..1. Below about 0.5 the grid should not be used for beat matching.
    pub bpm_confidence: f32,
    /// The first beat of the grid, 0 <= offset < one beat.
    pub beat_offset_ms: f64,
    /// 0..1: whether one constant grid can stand for the beats - the lower of how tightly they sit on it
    /// (median spread, 14 ms or less to pass) and how little the tempo moves between halves (1.2 %).
    pub stability: f32,
    /// 0..beats_per_bar-1: which grid beats start a bar.
    pub downbeat_phase: i32,
    pub downbeat_confidence: f32,
    /// 4, or 3 for a waltz; 0 (a row from before metres were measured) means 4. The same for the intro and
    /// outro grids.
    pub beats_per_bar: i32,
    /// Integrated loudness of the mono downmix, BS.1770 K-weighting and gating. -70 for silence.
    pub lufs: f32,
    /// Camelot code: 1..12 = 1A..12A (minor), 13..24 = 1B..12B (major), 0 = unknown.
    pub key: i32,
    pub key_confidence: f32,
    /// Where the audio first rises above, and last falls below, -55 dBFS.
    pub silence_start_ms: i64,
    pub silence_end_ms: i64,
    /// MixRamp points: where the start rises above, and the end falls below, 17 dB under the track's loudness.
    pub mixramp_start_ms: i64,
    pub mixramp_end_ms: i64,
    /// Phrase-aligned cues (multiples of 8 bars from the first downbeat, confirmed by an energy jump when there is
    /// one). `intro_end_ms == silence_start_ms` means the track starts at full energy.
    pub intro_end_ms: i64,
    pub outro_start_ms: i64,
    /// What the overlap windows sound like, for the pair gates: mean share of frame power in the voice
    /// band (0..1) and mean spectral centroid (Hz) over the outro (`outro_start_ms` to the music's end)
    /// and the intro (the music's start to `intro_end_ms`). 0 when unknown (silence, or a v1 row).
    pub outro_vocal: f32,
    pub intro_vocal: f32,
    pub outro_centroid: f32,
    pub intro_centroid: f32,
    /// The beat grid of the music's last and first `automix::GRID_WINDOW_S` seconds alone: tempo,
    /// confidence, first beat, stability and downbeat, as the whole-track fields but measured where the
    /// mix happens. Songs played by people drift a few per cent over four minutes - enough for one grid
    /// across the whole song to miss its last beats and score no stability at all - while any half
    /// minute of them is steady; and a song that changes tempo half way has two answers, of which only
    /// the one at the end matters for mixing out of it. 0 when unknown (a v2 row, or too little music).
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
    /// Where the arrangement arrives - the first four-bar line in the opening where the level, the low end and the
    /// chords all reach the body of the song - and the voice-band share over the eight bars before it (the run-up
    /// a mix lays under the outgoing song) and after it. 0 when the song starts full or has no usable grid.
    pub drop_ms: i64,
    pub drop_runup_vocal: f32,
    pub drop_vocal: f32,
    /// Where the ending stops being worth playing: the start of a closing breakdown (the level and the beat fall
    /// away for good in the last 24 s), or of the long silence before a hidden track whose music after it is short
    /// enough to leave. 0 when the song should play to its end.
    pub exit_ms: i64,
    /// The last silence of 6 s or more inside the music, start and end; 0 when there is none. Silence costs
    /// nothing against the skip cap, music after it does.
    pub gap_ms: i64,
    pub gap_end_ms: i64,
    /// Voice-band share over the eight bars before the exit (or the end of the music): what a mix's run-up lies
    /// under.
    pub exit_vocal: f32,
    /// Chord (tonal) energy of the run-up to the drop (its most chordal four bars) against the body of the song,
    /// dB: a drum intro reads far below (a key clash cannot happen over it), a pad or a sung intro near 0.
    pub drop_runup_tonal_db: f32,
    /// Beats in a bar of the intro and outro grids when that end was read on its own (Beat This! reads each end's
    /// metre); 0 means `beats_per_bar`.
    pub intro_beats_per_bar: i32,
    pub outro_beats_per_bar: i32,
    /// Where the intro and outro grids come from: `automix::beats::GRID_CLASSICAL` (the classical tracker, not yet
    /// seen by the model), `GRID_CHECKED` (the classical grid, kept because Beat This! was not sure enough to
    /// replace it) or `GRID_NEURAL` (Beat This!). A later run of the model picks up the ends below `GRID_CHECKED`.
    pub intro_grid_source: i32,
    pub outro_grid_source: i32,
    /// Wall-clock time of the analysis, ms since the epoch.
    pub analysed_ms: i64,
}

/// The user's AutoMix switches, as the planner sees them.
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
    /// Time-stretch (true) or varispeed, which also moves the pitch and is therefore held to 2 %.
    pub keep_pitch: bool,
    /// The two tracks are consecutive on one album played in order: no transition at all.
    pub same_album_in_order: bool,
    /// Trim the incoming track to the outgoing one's loudness. Leave off when ReplayGain already levels both.
    pub match_loudness: bool,
    /// Server/tag BPM for the outgoing track (0 unknown). Settles half/double errors against the analysis.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeCurve {
    /// cos/sin: constant power, for material that does not add coherently.
    EqualPower,
    Linear,
    /// sin²/cos²: constant amplitude, for beat-matched material that adds coherently.
    SineSquared,
}

/// A filter sweep on the outgoing deck over `[start_ms, end_ms]` of the transition, exponential in frequency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sweep {
    pub start_ms: i64,
    pub end_ms: i64,
    pub from_hz: f32,
    pub to_hz: f32,
}

/// The incoming lows are cut below `cut_hz` until `at_ms`; over `len_ms` they come in and the outgoing lows go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BassSwap {
    pub at_ms: i64,
    pub len_ms: i64,
    pub cut_hz: f32,
}

/// Beat-synced echo on the outgoing deck: `delay_ms` is one outgoing beat, `feedback` (0..1) what each repeat
/// keeps, `wet_db` the repeats' level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Echo {
    pub delay_ms: i64,
    pub feedback: f32,
    pub wet_db: f32,
}

/// The incoming voice band (centred on `hz`) is held `db` down until `until_ms`, released over the
/// `release_ms` before it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VocalDuck {
    pub until_ms: i64,
    pub release_ms: i64,
    pub db: f32,
    pub hz: f32,
}

/// How to get from one track to the next. Fields marked "relative" count from the start of the transition.
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
    /// After the overlap the incoming track ramps back to native speed over this many of its beats,
    pub tempo_ramp_beats: i32,
    /// which takes this long, wall-clock. 0 when there is no tempo change.
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
    /// Trim on the incoming deck; the mixer glides it back to 0 dB by the end of the overlap.
    pub in_gain_db: f32,
    /// Relative.
    pub bass_swap: Option<BassSwap>,
    /// Relative. Low-pass sweep on the outgoing track.
    pub low_pass: Option<Sweep>,
    /// Relative. High-pass sweep on the outgoing track (DJ "filter open").
    pub high_pass: Option<Sweep>,
    pub echo: Option<Echo>,
    /// Outro loop: the engine holds this many ms of the outgoing track and reads it round for `duration_ms`.
    pub out_loop_ms: Option<i64>,
    /// Relative.
    pub vocal_duck: Option<VocalDuck>,
    /// Why this plan, for logs.
    pub reason: String,
}
