//! Library records: the same structs deserialize Subsonic JSON, cross the FFI and are stored as JSON.

use serde::{Deserialize, Deserializer, Serialize};

// Player types, redeclared below for uniffi.
pub use nori_player::types::{AutoMixSettings, EqBand, EqKind, FadeCurve, NamedPreset, PresetKind, TrackAnalysis};

/// `starred` is a timestamp on the wire and a bool once stored.
fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum F {
        B(bool),
        S(String),
    }
    Ok(match Option::<F>::deserialize(d)? {
        Some(F::B(b)) => b,
        Some(F::S(s)) => !s.is_empty(),
        None => false,
    })
}

/// Some servers send numeric ids; ids are strings everywhere in the app.
fn opt_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum I {
        S(String),
        N(i64),
    }
    Ok(Option::<I>::deserialize(d)?.map(|i| match i {
        I::S(s) => s,
        I::N(n) => n.to_string(),
    }))
}

/// A string or numeric id as a string; empty when absent.
pub fn id_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(opt_id(d)?.unwrap_or_default())
}

/// Serde for a record with derived display fields (derived as `remote = "Self"`): every deserialized copy
/// gets them filled in by `dress`; they are never stored.
macro_rules! dressed {
    ($t:ty) => {
        impl Serialize for $t {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                <$t>::serialize(self, s)
            }
        }

        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let mut v = <$t>::deserialize(d)?;
                v.dress();
                Ok(v)
            }
        }
    };
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default, rename_all = "camelCase")]
pub struct ReplayGain {
    pub track_gain: Option<f32>,
    pub album_gain: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_peak: Option<f32>,
    /// OpenSubsonic: the gain in the file header (Opus output gain), dB; decoders apply it themselves.
    pub base_gain: Option<f32>,
    /// OpenSubsonic: the server's gain for a song lacking the requested tag, dB.
    pub fallback_gain: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct ArtistRef {
    #[serde(deserialize_with = "id_string")]
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(remote = "Self", default, rename_all = "camelCase")]
pub struct Song {
    #[serde(deserialize_with = "id_string")]
    pub id: String,
    pub title: String,
    pub album: String,
    pub artist: String,
    #[serde(deserialize_with = "opt_id")]
    pub album_id: Option<String>,
    #[serde(deserialize_with = "opt_id")]
    pub artist_id: Option<String>,
    #[serde(deserialize_with = "opt_id")]
    pub cover_art: Option<String>,
    pub duration: u32,
    pub track: u32,
    pub disc_number: u32,
    pub year: u32,
    pub genre: Option<String>,
    pub suffix: String,
    pub content_type: String,
    pub bit_rate: u32,
    pub size: u64,
    pub sampling_rate: u32,
    pub bit_depth: u32,
    pub user_rating: u8,
    #[serde(deserialize_with = "flag")]
    pub starred: bool,
    /// Set by octo-fiesta for provider items that are not in the library yet.
    pub is_external: bool,
    pub replay_gain: Option<ReplayGain>,
    /// Every credited artist (OpenSubsonic); `artist` stays the display string.
    pub artists: Vec<ArtistRef>,
    /// When the server first saw the file.
    pub created: Option<String>,
    /// Server-side play count and last play, across all clients.
    pub play_count: u32,
    pub played: Option<String>,
    pub path: Option<String>,
    /// "explicit", "clean" or empty.
    pub explicit_status: String,
    pub channel_count: u32,
    pub music_brainz_id: Option<String>,
    pub bpm: u32,
    pub comment: Option<String>,
    /// Row subtitle ([`crate::lines::song_line`]), filled when read.
    #[serde(skip)]
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub line: String,
}

/// Whether `id` is an octo-fiesta provider item's: it changes once downloaded, so it is never indexed.
pub fn is_provider_id(id: &str) -> bool {
    id.starts_with("ext-") || id.starts_with("pl-")
}

impl Song {
    /// A provider's song, not yet in the library.
    pub fn is_provider(&self) -> bool {
        self.is_external || is_provider_id(&self.id)
    }

    fn dress(&mut self) {
        self.line = crate::lines::song_line(&self.explicit_status, &self.artist, None);
    }

    /// A song with only its id (and derived fields) set.
    pub fn only_id(id: String) -> Self {
        let mut s = Song { id, ..Default::default() };
        s.dress();
        s
    }

    /// `self` with derived fields filled, as a read returns it.
    pub fn dressed(mut self) -> Self {
        self.dress();
        self
    }
}

dressed!(Song);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(remote = "Self", default, rename_all = "camelCase")]
pub struct Album {
    #[serde(deserialize_with = "id_string")]
    pub id: String,
    #[serde(alias = "title", alias = "album")]
    pub name: String,
    pub artist: String,
    #[serde(deserialize_with = "opt_id")]
    pub artist_id: Option<String>,
    #[serde(deserialize_with = "opt_id")]
    pub cover_art: Option<String>,
    pub song_count: u32,
    pub duration: u32,
    pub year: u32,
    pub genre: Option<String>,
    #[serde(deserialize_with = "flag")]
    pub starred: bool,
    /// Set by octo-fiesta for provider items that are not in the library yet.
    pub is_external: bool,
    /// OpenSubsonic: "Album", "EP", "Single", "Compilation", "Live", ... Empty on older servers.
    pub release_types: Vec<String>,
    pub is_compilation: bool,
    pub user_rating: u8,
    pub play_count: u32,
    pub created: Option<String>,
    pub explicit_status: String,
    pub music_brainz_id: Option<String>,
    /// Card subtitle ([`crate::lines::album_subtitle`]), filled when read.
    #[serde(skip)]
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub subtitle: String,
}

impl Album {
    /// A provider's album, not yet in the library.
    pub fn is_provider(&self) -> bool {
        self.is_external || is_provider_id(&self.id)
    }

    fn dress(&mut self) {
        self.subtitle = crate::lines::album_subtitle(&self.artist, self.year);
    }
}

dressed!(Album);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default, rename_all = "camelCase")]
pub struct Artist {
    #[serde(deserialize_with = "id_string")]
    pub id: String,
    pub name: String,
    #[serde(deserialize_with = "opt_id")]
    pub cover_art: Option<String>,
    pub artist_image_url: Option<String>,
    pub album_count: u32,
    #[serde(deserialize_with = "flag")]
    pub starred: bool,
    /// Set by octo-fiesta for provider items that are not in the library yet.
    pub is_external: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default, rename_all = "camelCase")]
pub struct Playlist {
    #[serde(deserialize_with = "id_string")]
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    pub owner: Option<String>,
    pub public: bool,
    pub song_count: u32,
    pub duration: u32,
    #[serde(deserialize_with = "opt_id")]
    pub cover_art: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default, rename_all = "camelCase")]
pub struct RadioStation {
    #[serde(deserialize_with = "id_string")]
    pub id: String,
    pub name: String,
    pub stream_url: String,
    pub home_page_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct Genre {
    #[serde(rename = "value")]
    pub name: String,
    #[serde(rename = "songCount")]
    pub song_count: u32,
    #[serde(rename = "albumCount")]
    pub album_count: u32,
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchResult {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub songs: Vec<Song>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct DiscTitle {
    pub disc: u32,
    pub title: String,
}

/// One level of the server's folder tree.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Directory {
    pub id: String,
    pub name: String,
    pub folders: Vec<Artist>,
    pub songs: Vec<Song>,
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ArtistInfo {
    pub last_fm_url: Option<String>,
    pub music_brainz_id: Option<String>,
    pub biography: Option<String>,
    pub image_url: Option<String>,
    pub similar: Vec<Artist>,
}

/// One word (or syllable) of a lyric line and when it is sung. `start`/`end` index the line's text in UTF-16 units.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct LyricWord {
    pub start_ms: i64,
    pub end_ms: i64,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct LyricLine {
    /// Milliseconds from track start; -1 when the lyrics are unsynced.
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// Empty for unsynced lyrics. See [Lyrics::word_timed] for whether these are real or estimated.
    pub words: Vec<LyricWord>,
    pub translation: Option<String>,
    /// A line of nothing but backing vocals, where the source says so.
    pub background: bool,
    /// Backing vocals sung over this line; empty for none.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub backing: String,
    /// The word timings inside [LyricLine::backing], as UTF-16 offsets into it.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub backing_words: Vec<LyricWord>,
    /// Duet voice: 0 first (or only), 1 second (drawn right), 2 together.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub voice: u8,
}

/// Cached as JSON (nori-lyrics `formats::to_cache`): only add fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct Lyrics {
    pub synced: bool,
    /// Word times come from the source; false when estimated from line times or absent.
    pub word_timed: bool,
    pub lines: Vec<LyricLine>,
    /// Offset (ms) the clock adds to the playhead, from the vocal sync check (nori-lyrics sync.rs); 0 if none.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    #[serde(skip)]
    pub offset_ms: i64,
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PlayQueue {
    pub songs: Vec<Song>,
    pub index: u32,
    pub position_ms: u64,
    /// The page the queue was started from; None if unknown.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub origin: Option<PageOrigin>,
}

/// A song's download state. `Queued` is shown for a waiting song with no stored mark; the last three are
/// post-download processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum DownloadPhase {
    Queued,
    Downloading,
    Failed,
    Done,
    FindingLyrics,
    Analysing,
    DetectingBeats,
}

/// The kind of page a queue was started from. Saved by name: only add variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum OriginKind {
    /// An album played whole.
    Album,
    /// All of an artist's songs.
    Artist,
    /// The top songs on an artist's page.
    ArtistTop,
    Playlist,
    Smart,
    /// A mix, by its key.
    Mix,
    Genre,
    Folder,
    /// The library's song list.
    Songs,
    /// Search results, by query.
    Search,
    /// A home shelf, by its key.
    Shelf,
    Downloads,
    /// "Shuffle songs": refills with random songs.
    ShuffleSongs,
    /// "Shuffle albums": refills with random whole albums.
    ShuffleAlbums,
}

/// The page a queue was started from. A page is "playing" only when this matches it (nori-queue `playlist_from`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PageOrigin {
    pub kind: OriginKind,
    pub id: String,
}

impl PageOrigin {
    pub fn new(kind: OriginKind, id: impl Into<String>) -> Self {
        PageOrigin { kind, id: id.into() }
    }
}

/// Ordinals are read from the flat band array by `dsp.rs`: only append.
#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
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

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct EqBand {
    pub kind: EqKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct EqPreset {
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum PresetKind {
    Flat,
    BassBoost,
    BassCut,
    TrebleBoost,
    TrebleCut,
    VocalBoost,
    Loudness,
    SmallSpeakers,
}

/// A built-in curve (`dsp::eq_presets`).
#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct NamedPreset {
    pub kind: PresetKind,
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ServerInfo {
    pub version: String,
    pub server_type: String,
    pub server_version: String,
    pub open_subsonic: bool,
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct IngestStats {
    pub artists: u32,
    pub albums: u32,
    pub songs: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct MusicFolder {
    #[serde(deserialize_with = "id_string")]
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct HistoryEntry {
    pub song: Song,
    pub started_ms: i64,
    pub heard_ms: i64,
    pub completed: bool,
    pub skipped: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TopSong {
    pub song: Song,
    pub plays: u32,
    pub listened_ms: i64,
}

/// An artist, album or genre in a top list; `id` is empty for genres and id-less artists.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TopEntry {
    pub id: String,
    pub name: String,
    /// Cover of the most played song of the entry.
    pub cover_art: Option<String>,
    pub plays: u32,
    pub listened_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ListeningStats {
    pub plays: u32,
    pub skips: u32,
    /// Everything heard, skipped plays included.
    pub listened_ms: i64,
    pub distinct_songs: u32,
    pub distinct_artists: u32,
    pub distinct_albums: u32,
    pub top_songs: Vec<TopSong>,
    pub top_artists: Vec<TopEntry>,
    pub top_albums: Vec<TopEntry>,
    pub top_genres: Vec<TopEntry>,
    /// 24 entries, local hour of day.
    pub plays_per_hour: Vec<u32>,
    /// 7 entries, Monday first.
    pub plays_per_weekday: Vec<u32>,
    pub active_days: u32,
    pub longest_streak_days: u32,
    pub first_play: Option<HistoryEntry>,
}

/// A built-in smart playlist; the client names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[repr(u8)]
pub enum SmartBuiltin {
    MostPlayed,
    RecentlyPlayed,
    RecentlyAdded,
    NeverPlayed,
    TopRated,
    ForgottenFavourites,
    LongTracks,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SmartPlaylist {
    pub id: String,
    /// User-given name; empty for a built-in.
    pub name: String,
    /// The definition; schema in `smart.rs`.
    pub json: String,
    /// Set only on built-in definitions (`smart_defaults`).
    pub builtin: Option<SmartBuiltin>,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct M3uEntry {
    /// None when the playlist does not say.
    pub duration_s: Option<u32>,
    pub artist: String,
    pub title: String,
    pub path: String,
}

/// One headphone measurement in the AutoEQ database.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AutoEqEntry {
    pub name: String,
    /// Measurer: oratory1990, crinacle, ...
    pub source: String,
    /// over-ear, in-ear, earbud, ...
    pub form: String,
    /// Target curve.
    pub target: String,
    /// Path in the AutoEQ results tree.
    pub path: String,
}

/// A saved sound setting: the whole chain under a name, optionally bound to output devices.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SoundProfile {
    pub name: String,
    /// Settings JSON, owned by the client.
    pub json: String,
    /// Output devices it applies to automatically.
    pub outputs: Vec<String>,
}

/// One `track_analysis` row (documented on `nori_player::types::TrackAnalysis`).
#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct TrackAnalysis {
    pub song_id: String,
    pub analysis_version: i32,
    pub duration_ms: i64,
    pub bpm: f64,
    pub bpm_confidence: f32,
    pub beat_offset_ms: f64,
    pub stability: f32,
    pub downbeat_phase: i32,
    pub downbeat_confidence: f32,
    pub beats_per_bar: i32,
    pub lufs: f32,
    pub key: i32,
    pub key_confidence: f32,
    pub silence_start_ms: i64,
    pub silence_end_ms: i64,
    pub mixramp_start_ms: i64,
    pub mixramp_end_ms: i64,
    pub intro_end_ms: i64,
    pub outro_start_ms: i64,
    pub outro_vocal: f32,
    pub intro_vocal: f32,
    pub outro_centroid: f32,
    pub intro_centroid: f32,
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
    pub drop_ms: i64,
    pub drop_runup_vocal: f32,
    pub drop_vocal: f32,
    pub exit_ms: i64,
    pub gap_ms: i64,
    pub gap_end_ms: i64,
    pub exit_vocal: f32,
    pub drop_runup_tonal_db: f32,
    pub intro_beats_per_bar: i32,
    pub outro_beats_per_bar: i32,
    pub intro_grid_source: i32,
    pub outro_grid_source: i32,
    pub analysed_ms: i64,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct AutoMixSettings {
    pub max_transition_s: f32,
    pub beat_match: bool,
    pub max_tempo_change_pct: f32,
    pub bass_swap: bool,
    pub filter_effects: bool,
    pub echo_out: bool,
    pub keep_pitch: bool,
    pub same_album_in_order: bool,
    pub match_loudness: bool,
    pub out_tag_bpm: f32,
    pub in_tag_bpm: f32,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum FadeCurve {
    EqualPower,
    Linear,
    SineSquared,
}

pub use nori_player::policy::{AudioPolicy, AudioPrefs, GainMode, GainTags, OutputState};

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct AudioPrefs {
    pub dsp: bool,
    pub skip_silence: bool,
    pub offload: bool,
    pub crossfade_s: i32,
    pub auto_mix: bool,
    pub speed: f32,
    pub pitch: f32,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct OutputState {
    pub hi_res: bool,
    pub bit_perfect: bool,
    pub usb: bool,
    pub offload_refused: bool,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct AudioPolicy {
    pub untouched: bool,
    pub float: bool,
    pub processing: bool,
    pub transitions_off: bool,
    pub lock_rate: bool,
    pub skip_silence: bool,
    pub offload: bool,
    pub processor_in_chain: bool,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum GainMode {
    Off,
    Track,
    Album,
    Auto,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct GainTags {
    pub track_gain: Option<f32>,
    pub album_gain: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_peak: Option<f32>,
}

pub use nori_player::device::{Arrival, ArrivalPlan, CurveStep};

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct Arrival {
    pub bound: bool,
    pub per_output: bool,
    pub speaker: bool,
    pub quiet: bool,
    pub auto_apply: bool,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum CurveStep {
    None,
    Offer,
    Apply,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct ArrivalPlan {
    pub load_bound: bool,
    pub restore: bool,
    pub curve: CurveStep,
}

pub use nori_player::transitions::{TransitionPrefs, WindowSong};

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct WindowSong {
    pub id: String,
    pub title: String,
    pub duration_ms: i64,
    pub album_id: Option<String>,
    pub disc: i32,
    pub track: i32,
    pub tag_bpm: f32,
    pub radio: bool,
    pub album_run: u32,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct TransitionPrefs {
    pub auto_mix: bool,
    pub crossfade_s: i32,
    pub auto_mix_max_s: i32,
    pub beat_match: bool,
    pub max_tempo_change_pct: f32,
    pub bass_swap: bool,
    pub filter_effects: bool,
    pub echo_out: bool,
    pub keep_pitch: bool,
    pub keep_albums: bool,
    pub replay_gain: bool,
    pub fade_curve: FadeCurve,
    pub fade_in_ms: i32,
    pub fade_out_ms: i32,
}

pub use nori_player::transport::{Dip, Switch};

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum Switch {
    Seek,
    ToSong,
    Skip,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct Dip {
    pub down_ms: i32,
    pub up_ms: i32,
}

pub use nori_player::dac::{DacBlock, DacMode};

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct DacMode {
    pub rate: u32,
    pub bits: u32,
    pub float: bool,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum DacBlock {
    NoModeAtRate { rate: u32 },
    NeedsExclusive { rate: u32, depths: u8 },
    PlatformTooOld,
    Refused,
}

pub use nori_player::queue::PlaybackError;

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum PlaybackError {
    Output,
    Network,
    Other,
}
