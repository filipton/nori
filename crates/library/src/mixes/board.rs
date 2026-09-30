//! The "For you" row: its mixes, each drawn once per day (or week) or on request, and the favourites.
//! Tiles, pages and playback read the same draw, held in memory per core; the same seed over the same
//! index draws the same mix after a restart, so nothing is stored.

use std::collections::{HashMap, HashSet};

use nori_model::Song;
use rusqlite::Connection;

use super::{discover, listen_again, quick_picks, top};

/// The id of the favourites tile and page; every other id names one of [MIXES].
pub const FAVOURITES_MIX: &str = "favourites";

/// How many songs one draw holds.
pub const DRAW: usize = 50;

/// What a mix draws from the index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    QuickPicks,
    Discover,
    ListenAgain,
    Top,
}

/// Which "For you" tile or page this is; the client names it ("Quick picks", "Your top songs").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[repr(u8)]
pub enum MixName {
    Favourites,
    QuickPicks,
    Discover,
    DiscoverWeekly,
    ListenAgain,
    Top,
}

/// One "For you" mix: `id` is the route, `kind` the draw, `name` the tile.
pub struct Spec {
    pub id: &'static str,
    pub kind: Kind,
    pub name: MixName,
    pub weekly: bool,
    /// The tile's colour, under its covers and name.
    colour: u32,
}

impl Spec {
    /// Top songs is a ranking, not a draw: asking for another would give the same list.
    pub fn refreshable(&self) -> bool {
        self.kind != Kind::Top
    }
}

/// The mixes "For you" offers, in the order it offers them.
pub const MIXES: [Spec; 5] = [
    Spec { id: "quick-picks", kind: Kind::QuickPicks, name: MixName::QuickPicks, weekly: false, colour: 0xFF8E_3BD6 },
    Spec { id: "discover", kind: Kind::Discover, name: MixName::Discover, weekly: false, colour: 0xFF1E_88E5 },
    Spec { id: "discover-weekly", kind: Kind::Discover, name: MixName::DiscoverWeekly, weekly: true, colour: 0xFF15_65C0 },
    Spec { id: "listen-again", kind: Kind::ListenAgain, name: MixName::ListenAgain, weekly: false, colour: 0xFF00_897B },
    Spec { id: "top", kind: Kind::Top, name: MixName::Top, weekly: false, colour: 0xFFE0_662B },
];

/// The mix `id` names; none for an id this build does not know.
pub fn spec_of(id: &str) -> Option<&'static Spec> {
    MIXES.iter().find(|s| s.id == id)
}

/// The favourites tile's colour, and the one a mix this build does not know wears.
const FAVOURITES_COLOUR: u32 = 0xFFE0_335A;
const OTHER_COLOUR: u32 = 0xFF5C_6BC0;

/// A mix tile's colours: its own, a deeper one (45 % to black), and the deeper at 0, 72 and 94 % alpha.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn mix_tile_colours(id: String) -> Vec<u32> {
    use nori_look::compose::{blend, with_alpha};
    let seed = if id == FAVOURITES_MIX { FAVOURITES_COLOUR } else { spec_of(&id).map_or(OTHER_COLOUR, |s| s.colour) };
    let deep = blend(seed, 0xFF00_0000, 0.45);
    vec![seed, deep, with_alpha(deep, 0.0), with_alpha(deep, 0.72), with_alpha(deep, 0.94)]
}

/// Provider songs are never queued unasked: a stream request makes octo-fiesta download them.
pub fn playable(s: &Song) -> bool {
    !s.is_provider()
}

/// Four different covers, for a tile's collage.
pub fn cover_ids(songs: &[Song]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(4);
    for art in songs.iter().filter_map(|s| s.cover_art.as_deref()) {
        if out.len() == 4 {
            break;
        }
        if !out.iter().any(|o| o == art) {
            out.push(art.to_string());
        }
    }
    out
}

/// Keeps the first of each id: lists are keyed by id.
pub fn distinct(songs: impl IntoIterator<Item = Song>) -> Vec<Song> {
    let mut seen = HashSet::new();
    songs.into_iter().filter(|s| seen.insert(s.id.clone())).collect()
}

// ---- what crosses to the app -----------------------------------------------

/// One entry of the catalogue, for a player that lists the mixes itself.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MixSpec {
    pub id: String,
    pub name: MixName,
    pub weekly: bool,
    pub refreshable: bool,
}

/// A "For you" tile: which it is and up to four cover ids of what is in it.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MixTile {
    pub id: String,
    pub name: MixName,
    pub covers: Vec<String>,
    pub favourites: bool,
}

/// A mix page: exactly the songs that play, in the order they play.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MixSheet {
    pub id: String,
    pub name: MixName,
    pub songs: Vec<Song>,
    /// Up to four cover ids.
    pub covers: Vec<String>,
    /// False for favourites (they follow the hearts) and for top songs (there is only one draw of those).
    pub refreshable: bool,
    pub favourites: bool,
    /// The songs' summed length in seconds, for the line under the title ("12 songs · 48:10").
    pub seconds: u64,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum MixLookup {
    /// No mix has this id (the client says so, with the id it asked for).
    Unknown,
    /// Not drawn yet (or, for favourites, not handed over yet): the page keeps its loader.
    NotDrawn,
    Ready { sheet: MixSheet },
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum MixDraw {
    Unknown,
    /// This period's draw was already there.
    Kept,
    Drawn,
    /// The index gave nothing: call again with the server's random songs, which stand in for the mix.
    NeedsFallback,
}

/// What warming the row did: whether any tile changed, and which mixes need the server's random songs.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MixWarm {
    pub changed: bool,
    pub needs_fallback: Vec<String>,
}

// ---- the boards ------------------------------------------------------------

/// One mix as drawn: its songs, the period it was drawn for and which draw of that period it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    pub songs: Vec<Song>,
    pub period: i64,
    pub generation: i64,
}

/// One core's "For you" row: each mix's draw and the favourites handed over.
#[derive(Default)]
pub struct Board {
    pub drawn: HashMap<&'static str, Drawn>,
    /// None until the app has handed the starred songs over once.
    pub favourites: Option<Vec<Song>>,
}

/// A draw that fails reads as an empty one, which then falls back to the server's random songs.
pub fn draw(c: &Connection, kind: Kind, seed: u64, now_ms: i64) -> Vec<Song> {
    match kind {
        Kind::QuickPicks => quick_picks(c, DRAW, seed, now_ms),
        Kind::Discover => discover(c, DRAW, seed, now_ms),
        Kind::ListenAgain => listen_again(c, DRAW, seed, now_ms),
        Kind::Top => top(c, DRAW, now_ms),
    }
    .unwrap_or_default()
}

/// The tiles before any draw: favourites, then the mixes when the taste model is on.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn mix_tiles(taste: bool) -> Vec<MixTile> {
    let mut out = vec![MixTile { id: FAVOURITES_MIX.into(), name: MixName::Favourites, covers: vec![], favourites: true }];
    if taste {
        out.extend(MIXES.iter().map(|s| MixTile { id: s.id.into(), name: s.name, covers: vec![], favourites: false }));
    }
    out
}

/// The mixes "For you" offers, in order.
pub fn mix_catalogue() -> Vec<MixSpec> {
    MIXES.iter().map(|s| MixSpec { id: s.id.into(), name: s.name, weekly: s.weekly, refreshable: s.refreshable() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::tests::song;

    #[test]
    fn tile_colours() {
        let c = mix_tile_colours("top".into());
        // Compose's blend(Color(0xFFE0662B), Color.Black, 0.45f), and the band's alphas in 8 bits.
        assert_eq!(c, [0xFFE0_662B, 0xFF7B_3818, 0x007B_3818, 0xB87B_3818, 0xF07B_3818]);
        assert_eq!(mix_tile_colours(FAVOURITES_MIX.into())[0], FAVOURITES_COLOUR);
        assert_eq!(mix_tile_colours("gone".into())[0], OTHER_COLOUR);
    }

    #[test]
    fn playable_leaves_out_provider_items() {
        let mut s = song("1", "a", "b", "c", "", 0);
        assert!(playable(&s));
        s.is_external = true;
        assert!(!playable(&s));
        assert!(!playable(&song("ext-deezer-1", "a", "b", "c", "", 0)));
        assert!(!playable(&song("pl-1", "a", "b", "c", "", 0)));
    }

    #[test]
    fn covers_are_four_distinct() {
        let mut l: Vec<Song> = (0..8).map(|i| song(&i.to_string(), "t", "a", "b", "", 0)).collect();
        l[1].cover_art = l[0].cover_art.clone();
        l[2].cover_art = None;
        assert_eq!(cover_ids(&l), ["cv-0", "cv-3", "cv-4", "cv-5"]);
    }

    #[test]
    fn catalogue_and_tiles() {
        assert!(mix_catalogue().iter().all(|m| m.refreshable == (m.id != "top")));
        let ids = |tiles: Vec<MixTile>| tiles.into_iter().map(|t| t.id).collect::<Vec<_>>();
        let every: Vec<String> = std::iter::once(FAVOURITES_MIX.to_string()).chain(mix_catalogue().into_iter().map(|m| m.id)).collect();
        assert_eq!(ids(mix_tiles(true)), every);
        assert_eq!(ids(mix_tiles(false)), [FAVOURITES_MIX], "no taste yet: only favourites");
    }
}
