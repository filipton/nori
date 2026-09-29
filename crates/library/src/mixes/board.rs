//! The "For you" row: which mixes it offers, today's (or this week's) draw of each, and the favourites.
//!
//! The Home tiles and the mix pages read the same draw, so the covers on a tile, the list on its page and
//! what plays are one list. A mix is drawn once per period (a day, or seven days for Discover Weekly) or
//! when the page asks for another, and never written to the server. The draws are held in memory, one
//! board per core (so per server profile): after a restart the same seed over the same index draws the
//! same mix again, so there is nothing worth storing.
//!
//! The app only says what day it is, which mix a screen wants, and what the server gave when a mix came
//! out empty (the one step here that needs the network).

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

/// One "For you" mix: `id` is the route, `kind` is what the index draws, `name` is which tile it is.
/// Discover Daily and Discover Weekly both call the same taste-based draw; only the seed period differs.
pub struct Spec {
    pub id: &'static str,
    pub kind: Kind,
    pub name: MixName,
    pub weekly: bool,
    /// The tile's own colour: what it wears before its covers arrive, and the band its name sits on.
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

/// A mix tile's colours: its own, the deeper one its gradient runs to (45 % of the way to black), and
/// that deeper colour at 0, 72 and 94 % for the band rising under the tile's name.
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
    /// The index gave nothing (no listening history yet): call again with what the server thinks is
    /// random (`getRandomSongs`, 50), which then stands in for the mix. Nothing was stored.
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

/// Every tile that can be named without the index: favourites first, then the mixes when the taste model
/// is on (switched off, it draws nothing and offers only favourites). No covers yet.
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
        assert_eq!(mix_catalogue().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["quick-picks", "discover", "discover-weekly", "listen-again", "top"]);
        assert!(mix_catalogue().iter().all(|m| m.refreshable == (m.id != "top")));
        assert_eq!(mix_tiles(false).len(), 1);
        assert_eq!(mix_tiles(true).len(), 6);
        assert!(mix_tiles(true)[0].favourites);
    }
}
