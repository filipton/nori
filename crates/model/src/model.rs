//! Wire + FFI models. The same structs deserialize Subsonic JSON, cross the
//! FFI as uniffi records and are stored as JSON in the local index.

use serde::{Deserialize, Deserializer, Serialize};

// Defined in the player crate, where the audio code that uses them lives; described here again so
// uniffi can hand them to Kotlin unchanged.
pub use nori_player::types::{AutoMixSettings, EqBand, EqKind, FadeCurve, NamedPreset, PresetKind, TrackAnalysis, TransitionKind, TransitionPlan};

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

pub fn id_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    id(d)
}

fn id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(opt_id(d)?.unwrap_or_default())
}

/// A record that carries words made from its own fields (a row's second line, a card's subtitle) has its
/// serde derived as `remote = "Self"` and is read through here: every copy of it that is read - off the
/// wire, out of the index, out of a stored answer - comes out with its words, and none of them are stored.
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
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct ArtistRef {
    #[serde(deserialize_with = "id")]
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(remote = "Self", default, rename_all = "camelCase")]
pub struct Song {
    #[serde(deserialize_with = "id")]
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
    /// A row's second line: the explicit mark, then the artist ([`crate::lines::song_line`] on no artist's
    /// page). Worked out once when the song is read, so a list does not ask for it row by row.
    #[serde(skip)]
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub line: String,
}

impl Song {
    fn dress(&mut self) {
        self.line = crate::lines::song_line(&self.explicit_status, &self.artist, None);
    }

    /// A song known only by its id, with the words a read would have given it.
    pub fn only_id(id: String) -> Self {
        let mut s = Song { id, ..Default::default() };
        s.dress();
        s
    }

    /// The song as a read gives it back, for a test that builds one and compares it with what was read.
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
    #[serde(deserialize_with = "id")]
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
    /// A card's second line, "Artist · 2019" ([`crate::lines::album_subtitle`]), made when read.
    #[serde(skip)]
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub subtitle: String,
}

impl Album {
    fn dress(&mut self) {
        self.subtitle = crate::lines::album_subtitle(&self.artist, self.year);
    }
}

dressed!(Album);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default, rename_all = "camelCase")]
pub struct Artist {
    #[serde(deserialize_with = "id")]
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
    #[serde(deserialize_with = "id")]
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
    #[serde(deserialize_with = "id")]
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
    /// Backing vocals sung over this line, drawn smaller under it with their own timing; empty when
    /// there are none.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub backing: String,
    /// The word timings inside [LyricLine::backing], as UTF-16 offsets into it.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub backing_words: Vec<LyricWord>,
    /// Which voice sings the line in a duet: 0 the first (or the only one), 1 the other side, drawn on
    /// the right, 2 everyone together.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub voice: u8,
}

/// Kept as JSON in the response cache (nori-lyrics' `formats::to_cache`), so fields are only ever added.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct Lyrics {
    pub synced: bool,
    /// True when the word timing is real: the server's cues, inline LRC word tags, or a lyrics service's
    /// own word times. False when the core spread each line's time over its words, or there are none.
    pub word_timed: bool,
    pub lines: Vec<LyricLine>,
    /// What the core kept of these lyrics' timing when they were read, so a clock is started on them by
    /// this key (`LyricsJni.kept`) rather than with every line handed back. 0: nothing kept.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    #[serde(skip)]
    pub key: u64,
    /// How much later than the song these lyrics' times run, ms, as the sync check against the song's vocal
    /// activity found it with confidence (nori-lyrics sync.rs); 0 when it found no offset or has not looked.
    /// A clock adds it to the playhead, so the lines come when they are sung. Found again each time the
    /// lyrics are chosen, never kept with them.
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
    /// The page the queue was started from, kept with it so a queue put back still lights its page.
    /// None for one from the server or saved before origins were kept.
    #[cfg_attr(feature = "ffi", uniffi(default))]
    pub origin: Option<PageOrigin>,
}

/// What kind of place a queue was started from. Saved with the queue by name, so only ever add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum OriginKind {
    /// An album's songs (its page, or the album played whole from anywhere).
    Album,
    /// All of an artist's songs (the artist page's Play and Shuffle).
    Artist,
    /// The top songs listed on an artist's page: not the page's own Play, which is all the songs.
    ArtistTop,
    Playlist,
    /// A smart playlist.
    Smart,
    /// A made-for-you mix, by its key.
    Mix,
    Genre,
    Folder,
    /// The library's list of songs.
    Songs,
    /// A search's results, by the query.
    Search,
    /// A shelf of the home page, by its key.
    Shelf,
    /// The downloaded songs.
    Downloads,
}

/// The page a queue was started from: its kind and the id of what it shows. A page is "the one
/// playing" exactly when the queue's origin is its own (nori-queue `playlist_from`), not when the song
/// playing happens to be among its songs.
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

/// The order is the wire format: `dsp.rs` reads the ordinal out of the flat band array, so only append.
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
    /// Shelves whose `q` is the RBJ slope S (1 is the steepest slope without ripple) rather than a Q.
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

/// One of the built-in curves from `dsp::eq_presets`.
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
    #[serde(deserialize_with = "id")]
    pub id: String,
    pub name: String,
}

// ---- play history, listening stats, smart playlists, m3u ----

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
pub struct SongStat {
    pub song_id: String,
    /// Listens that were not skips.
    pub plays: u32,
    pub skips: u32,
    /// 0 when the song was only ever skipped.
    pub last_played_ms: i64,
    pub heard_ms_total: i64,
    /// The taste score as of now; see `history.rs`. Around 1 per recent full listen.
    pub taste: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TopSong {
    pub song: Song,
    pub plays: u32,
    pub listened_ms: i64,
}

/// An artist, album or genre in a top list. `id` is empty for genres and for artists the server gave no id.
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

/// Which ready-made smart playlist a built-in definition is; the client names it.
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
    /// The name the user gave it; empty for a ready-made one, which the client names by [`builtin`](Self::builtin).
    pub name: String,
    /// The definition; schema in `smart.rs`.
    pub json: String,
    /// Set on the ready-made definitions (`smart_defaults`), never on a stored one.
    pub builtin: Option<SmartBuiltin>,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct M3uEntry {
    /// -1 when the playlist does not say.
    pub duration_s: i32,
    pub artist: String,
    pub title: String,
    pub path: String,
}

/// One headphone measurement in the AutoEQ database.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AutoEqEntry {
    pub name: String,
    /// Who measured it: oratory1990, crinacle, ...
    pub source: String,
    /// over-ear, in-ear, earbud, ...
    pub form: String,
    /// The target curve it was equalised to.
    pub target: String,
    /// Path inside the AutoEQ results tree; the core turns it into a download url.
    pub path: String,
}

/// A saved sound setting: the whole chain under a name, optionally bound to output devices.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SoundProfile {
    pub name: String,
    /// The settings as JSON, written and read by the Kotlin side.
    pub json: String,
    /// Output devices this profile applies to automatically, one per line.
    pub outputs: Vec<String>,
}

/// What AutoMix knows about one track, from `automix::analysis`. One row in `track_analysis`.
/// Times are milliseconds from the start of the file. The beat grid is not stored beat by beat: beat `n` sits at
/// `beat_offset_ms + n * 60000 / bpm`, and beats with `n % beats_per_bar == downbeat_phase` start a bar.
#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
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
#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
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


#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
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

/// The order is the number the mixer's parameters carry (`automix::mixer::params`); only append.
#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum FadeCurve {
    /// cos/sin: constant power, for material that does not add coherently.
    EqualPower,
    Linear,
    /// sin²/cos²: constant amplitude, for beat-matched material that adds coherently.
    SineSquared,
}

/// How to get from one track to the next. Fields marked "relative" count from the moment the transition starts;
/// -1 means "not used".
#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
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
    /// Outro remix: hold captures this many ms and the mixer reads it with wrap for `duration_ms`.
    /// `-1` means capture the full duration with no loop (Apple iOS 27-style intro/outro extend).
    pub out_loop_ms: i64,
    /// High-pass sweep on the outgoing track (DJ "filter open"), `-1` when off.
    pub hp_start_ms: i64,
    pub hp_end_ms: i64,
    pub hp_from_hz: f32,
    pub hp_to_hz: f32,
    /// Relative. When both songs sing over the run-up, the incoming song's voice band (`vocal_duck_hz` at the
    /// centre) is held `vocal_duck_db` down until here, released over `vocal_duck_release_ms` before it; -1 when off.
    pub vocal_duck_until_ms: i64,
    pub vocal_duck_release_ms: i64,
    pub vocal_duck_db: f32,
    pub vocal_duck_hz: f32,
    /// Why this plan, for logs.
    pub reason: String,
}

// ---- the player's decisions (nori_player::policy), described again for uniffi ----

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
}

pub use nori_player::transport::{ChainChange, Dip, Rebuild, Switch};

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

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct ChainChange {
    pub offloaded: bool,
    pub offload: bool,
    pub offload_changed: bool,
    pub usb: bool,
    pub offload_refused: bool,
    pub tempo_changed: bool,
    pub processor_changed: bool,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum Rebuild {
    None,
    Now,
    AtBoundary,
}

pub use nori_player::dac::{DacBlock, DacChoice, DacMode};

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct DacMode {
    pub rate: u32,
    pub bits: u32,
    pub float: bool,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Record)]
pub struct DacChoice {
    pub use_index: i32,
    pub blocked_by: Option<DacBlock>,
}

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum DacBlock {
    NoModeAtRate { rate: u32 },
    NeedsExclusive { rate: u32, depths: u8 },
    PlatformTooOld,
    Refused,
}

/// What failed when a song would not play (`nori_player::queue`): the queue's rules count it, and the
/// words say it.
pub use nori_player::queue::PlaybackError;

#[cfg(feature = "ffi")]
#[uniffi::remote(Enum)]
pub enum PlaybackError {
    Output,
    Network,
    Other,
}
