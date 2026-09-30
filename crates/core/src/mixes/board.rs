//! The "For you" row's draws, held per core. Its contents are nori-library's.

use crate::{db, Core, Song};
use nori_library::pages::total_seconds;

pub use nori_library::mixes::board::*;

impl Core {
    fn board<R>(&self, f: impl FnOnce(&mut Board) -> R) -> R {
        f(&mut self.board.lock())
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Draws missing or outdated mixes, in row order.
    pub fn mix_warm(&self, today_epoch_day: i64) -> MixWarm {
        let mut out = MixWarm { changed: false, needs_fallback: vec![] };
        for spec in &MIXES {
            match self.mix_draw(spec.id.into(), today_epoch_day, false, None) {
                MixDraw::Drawn => out.changed = true,
                MixDraw::NeedsFallback => out.needs_fallback.push(spec.id.into()),
                MixDraw::Kept | MixDraw::Unknown => {}
            }
        }
        out
    }

    /// Sets the favourites mix from the starred songs, with session marks applied and provider songs
    /// dropped. True when it changed.
    pub fn mix_favourites(&self, starred_songs: Vec<Song>) -> bool {
        let marks = self.stars.lock().clone();
        let kept = distinct(starred_songs.into_iter().filter(|s| playable(s) && marks.kept(crate::client::Starrable::Song, &s.id)));
        self.board(|b| {
            let changed = b.favourites.as_ref() != Some(&kept);
            b.favourites = Some(kept);
            changed
        })
    }

    /// The row's tiles with drawn covers: favourites, then the mixes when `taste`.
    pub fn mix_cards(&self, taste: bool) -> Vec<MixTile> {
        let mut tiles = mix_tiles(taste);
        self.board(|b| {
            tiles[0].covers = cover_ids(b.favourites.as_deref().unwrap_or_default());
            for (tile, spec) in tiles.iter_mut().skip(1).zip(&MIXES) {
                tile.covers = b.drawn.get(spec.id).map(|d| cover_ids(&d.songs)).unwrap_or_default();
            }
        });
        tiles
    }

    /// One mix's page.
    pub fn mix_page(&self, id: String) -> MixLookup {
        if id == FAVOURITES_MIX {
            return self.board(|b| match &b.favourites {
                Some(songs) => MixLookup::Ready {
                    sheet: MixSheet { id: id.clone(), name: MixName::Favourites, covers: cover_ids(songs), seconds: total_seconds(songs), songs: songs.clone(), refreshable: false, favourites: true },
                },
                None => MixLookup::NotDrawn,
            });
        }
        let Some(spec) = spec_of(&id) else { return MixLookup::Unknown };
        self.board(|b| match b.drawn.get(spec.id) {
            Some(d) => MixLookup::Ready {
                sheet: MixSheet { id: spec.id.into(), name: spec.name, songs: d.songs.clone(), covers: cover_ids(&d.songs), refreshable: spec.refreshable(), favourites: false, seconds: total_seconds(&d.songs) },
            },
            None => MixLookup::NotDrawn,
        })
    }
}

impl Core {
    /// Draws mix `id` unless this period's draw exists (`again`: redraw). `today_epoch_day` is the local
    /// date. `fallback` carries random server songs after [MixDraw::NeedsFallback].
    pub(crate) fn mix_draw(&self, id: String, today_epoch_day: i64, again: bool, fallback: Option<Vec<Song>>) -> MixDraw {
        let Some(spec) = spec_of(&id) else { return MixDraw::Unknown };
        // Weekly mixes keep one seed for seven days.
        let period = if spec.weekly { today_epoch_day / 7 } else { today_epoch_day };
        let generation = match self.board(|b| b.drawn.get(spec.id).map(|d| (d.period, d.generation))) {
            Some((p, _)) if p == period && !again => return MixDraw::Kept,
            Some((p, g)) if p == period => g + 1,
            _ => 0,
        };
        // Offset so weekly seeds never equal a daily one.
        let seed = period * 1_000 + generation + if spec.weekly { 7_000_000 } else { 0 };
        let songs: Vec<Song> = match fallback {
            Some(random) => random.into_iter().filter(playable).collect(),
            None => {
                let songs: Vec<Song> = draw(&self.db.lock(), spec.kind, seed as u64, db::now_ms()).into_iter().filter(playable).collect();
                // Empty without history: fall back to random server songs.
                if songs.is_empty() {
                    return MixDraw::NeedsFallback;
                }
                songs
            }
        };
        let drawn = Drawn { songs: distinct(songs), period, generation };
        self.board(|b| b.drawn.insert(spec.id, drawn));
        MixDraw::Drawn
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db;
    use crate::history::tests::song;
    use crate::mixes::discover;

    fn ids(l: &[Song]) -> Vec<&str> {
        l.iter().map(|s| s.id.as_str()).collect()
    }

    fn sheet(core: &Core, id: &str) -> MixSheet {
        match core.mix_page(id.into()) {
            MixLookup::Ready { sheet } => sheet,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_index_needs_fallback_once_per_period() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        assert_eq!(core.mix_draw("nope".into(), 100, false, None), MixDraw::Unknown);
        assert!(matches!(core.mix_page("nope".into()), MixLookup::Unknown));
        assert_eq!(core.mix_page("discover".into()), MixLookup::NotDrawn);
        assert_eq!(core.mix_draw("discover".into(), 100, false, None), MixDraw::NeedsFallback);
        let random = vec![song("r1", "t", "a", "b", "", 0), song("ext-r2", "t", "a", "b", "", 0), song("r1", "t", "a", "b", "", 0)];
        assert_eq!(core.mix_draw("discover".into(), 100, false, Some(random)), MixDraw::Drawn);
        assert_eq!(ids(&sheet(&core, "discover").songs), ["r1"]);
        assert_eq!(core.mix_draw("discover".into(), 100, false, None), MixDraw::Kept);
        // An empty fallback still counts as this period's draw.
        assert_eq!(core.mix_draw("top".into(), 100, false, None), MixDraw::NeedsFallback);
        assert_eq!(core.mix_draw("top".into(), 100, false, Some(vec![])), MixDraw::Drawn);
        assert_eq!(core.mix_draw("top".into(), 100, false, None), MixDraw::Kept);
        assert!(!sheet(&core, "top").refreshable);
        let warm = core.mix_warm(100);
        assert_eq!(warm.needs_fallback, ["quick-picks", "discover-weekly", "listen-again"]);
        assert!(!warm.changed);
    }

    #[test]
    fn draws_follow_period_and_again_redraws() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let all: Vec<Song> = (0..60).map(|i| song(&format!("s{i}"), "t", &format!("Artist {}", i % 12), &format!("LP {i}"), "Rock", 1990)).collect();
        db::index(&mut core.db.lock(), &[], &[], &all).unwrap();
        let today = 20_000;
        // Without history Discover is a seeded walk, so its draw is predictable.
        let expected = |seed: i64| ids(&discover(&core.db.lock(), DRAW, seed as u64, db::now_ms()).unwrap()).into_iter().map(String::from).collect::<Vec<_>>();
        assert_eq!(core.mix_draw("discover".into(), today, false, None), MixDraw::Drawn);
        let first = sheet(&core, "discover").songs;
        assert!(!first.is_empty());
        assert_eq!(ids(&first), expected(today * 1000));
        assert_eq!(core.mix_draw("discover".into(), today, false, None), MixDraw::Kept);
        assert_eq!(core.mix_draw("discover".into(), today, true, None), MixDraw::Drawn);
        assert_eq!(ids(&sheet(&core, "discover").songs), expected(today * 1000 + 1));
        assert_eq!(core.mix_draw("discover".into(), today, true, None), MixDraw::Drawn);
        assert_eq!(ids(&sheet(&core, "discover").songs), expected(today * 1000 + 2));
        assert_eq!(core.mix_draw("discover".into(), today + 1, false, None), MixDraw::Drawn);
        assert_eq!(ids(&sheet(&core, "discover").songs), expected((today + 1) * 1000));
        assert_eq!(core.mix_draw("discover-weekly".into(), 7 * 3000, false, None), MixDraw::Drawn);
        assert_eq!(core.mix_draw("discover-weekly".into(), 7 * 3000 + 6, false, None), MixDraw::Kept);
        assert_eq!(ids(&sheet(&core, "discover-weekly").songs), expected(3000 * 1000 + 7_000_000));
        let cards = core.mix_cards(true);
        assert_eq!(cards.len(), 6);
        assert_eq!(cards[2].id, "discover");
        assert_eq!(cards[2].covers.len(), 4);
        assert!(cards[1].covers.is_empty());
    }

    #[test]
    fn favourites_follow_marks_per_core() {
        let core = Core::new(String::new(), "t".into()).unwrap();
        let other = Core::new(String::new(), "t".into()).unwrap();
        assert_eq!(core.mix_page(FAVOURITES_MIX.into()), MixLookup::NotDrawn);
        let starred = vec![song("1", "t", "a", "b", "", 0), song("2", "t", "a", "b", "", 0), song("ext-3", "t", "a", "b", "", 0), song("1", "t", "a", "b", "", 0)];
        assert!(core.mix_favourites(starred.clone()));
        assert!(!core.mix_favourites(starred.clone()));
        assert_eq!(ids(&sheet(&core, FAVOURITES_MIX).songs), ["1", "2"]);
        core.stars.lock().mark(crate::client::Starrable::Song, "1".into(), false);
        assert!(core.mix_favourites(starred.clone()));
        let fav = sheet(&core, FAVOURITES_MIX);
        assert_eq!(ids(&fav.songs), ["2"]);
        assert!(fav.favourites && !fav.refreshable);
        assert_eq!(core.mix_cards(false)[0].covers, ["cv-2"]);
        assert_eq!(other.mix_page(FAVOURITES_MIX.into()), MixLookup::NotDrawn);
        // Marks are per core too.
        other.mix_favourites(starred);
        assert_eq!(ids(&sheet(&other, FAVOURITES_MIX).songs), ["1", "2"]);
    }
}
