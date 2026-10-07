//! Library client calls: cached screen reads, starring, favourites for mixes, mix draws and resuming
//! the server's queue. Decisions are nori-library's.

use crate::cache_policy::{Page, Read};
use crate::client::{Client, NetResult, Starrable, Write};
use crate::mixes::board::{MixDraw, MixLookup, FAVOURITES_MIX};
use crate::Song;
use std::sync::Arc;

pub use nori_library::library::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// A screen read: shows the cached page, then (unless fresh) the server's if it differs. Failure is an
    /// error only when nothing was cached.
    pub async fn read_cached(&self, read: Read, shown: Arc<dyn PageShown>) -> NetResult<()> {
        self.read_each(read, |p| shown.show(p)).await
    }

    /// Stars or unstars: the mark is shown at once via `marked`, then the write is sent (queued offline).
    /// A server refusal restores the previous mark and returns the error.
    pub async fn star(&self, kind: Starrable, id: String, on: bool, marked: Arc<dyn StarsShown>) -> NetResult<()> {
        let m = self.core.stars.lock().mark(kind, id.clone(), on);
        marked.marks(m.marks);
        let sent = self.write(Write::Star { kind, id: id.clone(), on }).await;
        if sent.is_err() {
            marked.marks(self.core.stars.lock().restore(kind, id, on, m.previous));
        }
        sent
    }

    /// Hands the cached starred songs (with session marks) to the mixes; the cached half of the read.
    pub fn mix_favourites_stored(&self) -> NetResult<FavouritesHanded> {
        let stored = self.read_stored(Read::StarredItems)?;
        let handed = match stored.page {
            Some(Page::StarredPage { v }) => {
                self.core.mix_favourites(v.songs);
                true
            }
            _ => false,
        };
        Ok(FavouritesHanded { handed, digest: stored.digest, fresh: stored.fresh })
    }

    /// Hands the server's starred songs to the mixes if they differ from `stored_digest`; true if so.
    pub async fn mix_favourites_refresh(&self, stored_digest: Option<u64>) -> NetResult<bool> {
        Ok(match self.read_refresh(Read::StarredItems, stored_digest).await? {
            Some(Page::StarredPage { v }) => {
                self.core.mix_favourites(v.songs);
                true
            }
            _ => false,
        })
    }

    /// All songs of the artist's albums ([`Client::artist_songs`]), from the cached artist page if any.
    pub async fn artist_songs_of(&self, artist_id: String) -> NetResult<Vec<Song>> {
        let Some(Page::ArtistPage { v }) = self.cached_or_fetched(Read::ArtistById { id: artist_id }).await? else { return Ok(Vec::new()) };
        Ok(self.artist_songs(v.albums).await)
    }

    /// Draws mix `id` unless this period's draw exists (`again`: redraw), falling back to random server
    /// songs without history. True when the draw changed.
    pub async fn mix_ensure(&self, id: String, again: bool) -> bool {
        let day = local_epoch_day();
        match self.core.mix_draw(id.clone(), day, again, None) {
            MixDraw::Drawn => true,
            MixDraw::NeedsFallback => self.mix_fallback(id, day, again).await,
            MixDraw::Kept | MixDraw::Unknown => false,
        }
    }

    /// Draws missing or outdated mixes; true when any changed.
    pub async fn mix_warm_all(&self) -> bool {
        let day = local_epoch_day();
        let warm = self.core.mix_warm(day);
        let mut changed = warm.changed;
        for id in warm.needs_fallback {
            changed |= self.mix_fallback(id, day, false).await;
        }
        changed
    }

    /// The play queue another device saved on the server.
    pub async fn resume_from_server(&self) -> NetResult<ResumePlan> {
        match self.read_now(Read::PullQueue).await? {
            Page::Queue { v } => Ok(resume_plan(v)),
            _ => Ok(ResumePlan::Nothing),
        }
    }
}

/// Receives each page of [`Client::read_cached`].
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait PageShown: Send + Sync {
    fn show(&self, page: Page);
}

/// Receives the session's star marks whenever [`Client::star`] changes them.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait StarsShown: Send + Sync {
    fn marks(&self, marks: crate::stars::StarMarks);
}

impl Client {
    /// [`Client::read_fetch`] where failing to reach the server is not an error when something was cached
    /// (`stored_digest`); the server's own refusal is.
    pub(crate) async fn read_refresh(&self, read: Read, stored_digest: Option<u64>) -> NetResult<Option<Page>> {
        match self.read_fetch(read, stored_digest).await {
            Err(e) if stored_digest.is_some() && !matches!(e, crate::transport::NetError::Api { .. }) => Ok(None),
            other => other,
        }
    }

    /// [`Client::read_cached`] with a closure.
    pub async fn read_each(&self, read: Read, mut each: impl FnMut(Page)) -> NetResult<()> {
        let stored = self.read_stored(read.clone())?;
        let digest = stored.digest;
        if let Some(p) = stored.page {
            each(p);
        }
        if stored.fresh {
            return Ok(());
        }
        if let Some(p) = self.read_refresh(read, digest).await? {
            each(p);
        }
        Ok(())
    }

    /// Hands the starred songs to the favourites mix: the stored ones at once, the server's when they
    /// are not fresh.
    pub async fn mix_hand_favourites(&self) -> NetResult<()> {
        let stored = self.mix_favourites_stored()?;
        if !stored.fresh {
            self.mix_favourites_refresh(stored.digest).await?;
        }
        Ok(())
    }

    /// Mix `id`'s songs, as its page lists them: the favourites handed first, a mix drawn if this
    /// period's draw is missing. Empty for an id no mix has.
    pub async fn mix_songs(&self, id: String) -> NetResult<Vec<Song>> {
        if id == FAVOURITES_MIX {
            self.mix_hand_favourites().await?;
        } else {
            self.mix_ensure(id.clone(), false).await;
        }
        Ok(match self.core.mix_page(id) {
            MixLookup::Ready { sheet } => sheet.songs,
            MixLookup::NotDrawn | MixLookup::Unknown => Vec::new(),
        })
    }

    /// Draws mix `id` from random server songs; false when they could not be read, so it is drawn again
    /// next time.
    async fn mix_fallback(&self, id: String, day: i64, again: bool) -> bool {
        let Ok(random) = self.songs(Read::RandomSongs { size: MIX_FALLBACK_SONGS, genre: None }).await else { return false };
        self.core.mix_draw(id, day, again, Some(random));
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::tests::{block, client};
    use crate::client::NetProfile;

    #[derive(Default)]
    struct Marks(parking_lot::Mutex<Vec<Option<bool>>>);

    impl StarsShown for Marks {
        fn marks(&self, m: crate::stars::StarMarks) {
            self.0.lock().push(m.songs.get("lib-refused").copied());
        }
    }

    #[test]
    fn a_mix_unread_offline_is_drawn_again() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.fail(crate::transport::FailureKind::Connect);
        assert!(!block(c.mix_ensure("top".into(), false)));
        fake.answer(r#"{"subsonic-response":{"status":"ok","randomSongs":{"song":[{"id":"x","title":"x","isDir":false}]}}}"#);
        assert!(block(c.mix_ensure("top".into(), false)));
        assert!(fake.asked()[1].contains("getRandomSongs"));
    }

    #[test]
    fn refused_star_restores_mark() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.answer(crate::client::tests::OK);
        let seen = Arc::new(Marks::default());
        assert!(block(c.star(Starrable::Song, "lib-refused".into(), true, seen.clone())).is_ok());
        fake.answer(r#"{"subsonic-response":{"status":"failed","error":{"code":50,"message":"no"}}}"#);
        assert!(block(c.star(Starrable::Song, "lib-refused".into(), false, seen.clone())).is_err());
        assert_eq!(*seen.0.lock(), [Some(true), Some(false), Some(true)]);
        assert_eq!(c.core.stars.lock().songs.get("lib-refused"), Some(&true));
    }

    const GENRES: &str = r#"{"subsonic-response":{"status":"ok","genres":{"genre":[{"value":"Rock","songCount":1,"albumCount":1}]}}}"#;
    const GENRES2: &str = r#"{"subsonic-response":{"status":"ok","genres":{"genre":[{"value":"Jazz","songCount":1,"albumCount":1}]}}}"#;

    fn each(c: &Client, read: Read) -> (NetResult<()>, usize) {
        let mut n = 0;
        let r = block(c.read_each(read, |_| n += 1));
        (r, n)
    }

    #[test]
    fn read_each_shows_cached_then_changed() {
        let (c, fake) = client(NetProfile { url: "h".into(), ..Default::default() });
        fake.fail(crate::transport::FailureKind::Connect);
        let (r, n) = each(&c, Read::GenreList);
        assert!(r.is_err() && n == 0, "nothing cached");
        fake.answer(GENRES);
        assert_eq!(each(&c, Read::GenreList).1, 1);
        assert_eq!(each(&c, Read::GenreList).1, 1, "fresh: not asked");
        assert_eq!(fake.asked().len(), 2);
        c.core.db.lock().execute("UPDATE cache SET ts = 0", []).unwrap();
        fake.answer(GENRES);
        assert_eq!(each(&c, Read::GenreList).1, 1, "stale, unchanged");
        c.core.db.lock().execute("UPDATE cache SET ts = 0", []).unwrap();
        fake.answer(GENRES2);
        assert_eq!(each(&c, Read::GenreList).1, 2, "stale, changed");
        c.core.db.lock().execute("UPDATE cache SET ts = 0", []).unwrap();
        fake.fail(crate::transport::FailureKind::Connect);
        let (r, n) = each(&c, Read::GenreList);
        assert!(r.is_ok() && n == 1, "offline with a cached page");
    }
}
