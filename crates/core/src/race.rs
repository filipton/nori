//! Lyrics calls: the server's lyrics, then the enabled lyrics services, through the client's transport
//! and the response cache. Service logic and ranking are nori-lyrics'.

use std::sync::Arc;

use crate::client::{Client, NetResult};
use crate::settings::StoredPrefs;
use crate::{Core, Song};

pub use nori_lyrics::race::*;
use nori_settings::lyrics_sources::{lyrics_lookup, LyricsLookup};

impl LyricsCache for Core {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.cache_get(key.to_string()).ok().flatten()
    }

    fn fresh(&self, key: &str, max_age_ms: i64) -> bool {
        self.cache_fresh(key.to_string(), max_age_ms).unwrap_or(false)
    }

    fn put(&self, key: &str, body: Vec<u8>) {
        let _ = self.cache_put(key.to_string(), body);
    }

    fn voice(&self, song: &Song) -> Option<nori_player::automix::vocal::VocalCurve> {
        self.analysis_voice(&song.id).ok().flatten()
    }
}

/// Registers each pick's timing (`look::keep`) before passing it on.
struct Keeping(Arc<dyn LyricsShown>);

impl LyricsShown for Keeping {
    fn show(&self, mut pick: LyricsPick) {
        crate::look::keep(&mut pick.lyrics);
        self.0.show(pick);
    }
}

/// Passes on only picks that replace the last one shown ([`lyrics_replaces`]).
struct Screen {
    to: Arc<dyn LyricsShown>,
    last: parking_lot::Mutex<Option<LyricsPick>>,
}

impl LyricsShown for Screen {
    fn show(&self, pick: LyricsPick) {
        let mut last = self.last.lock();
        if lyrics_replaces(last.as_ref(), &pick) {
            *last = Some(pick.clone());
            drop(last);
            self.to.show(pick);
        }
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Streams song `id`'s lyrics to `shown`: the server's (cached, then changed), then the services'
    /// when those are not timed, and an empty server answer last if nobody had any. No pick is shown
    /// twice. Dropping the future cancels its requests.
    pub async fn lyrics_for(&self, id: String, shown: Arc<dyn LyricsShown>) -> NetResult<()> {
        let asked = self.settings().with_prefs(lyrics_lookup).unwrap_or_else(|| lyrics_lookup(&StoredPrefs::default()));
        self.lyrics_for_with(id, &asked, shown).await
    }
}

impl Client {
    /// [`Client::lyrics_for`] with the services given.
    async fn lyrics_for_with(&self, id: String, asked: &LyricsLookup, shown: Arc<dyn LyricsShown>) -> NetResult<()> {
        use crate::cache_policy::{Page, Read};
        let screen = Arc::new(Screen { to: shown, last: parking_lot::Mutex::new(None) });
        let read = || Read::LyricsBySong { song_id: id.clone() };
        let show_server = |v: &crate::Lyrics| {
            if !v.lines.is_empty() {
                screen.show(LyricsPick { lyrics: v.clone(), origin: nori_settings::lyrics_sources::LyricsOrigin::Server });
            }
        };
        let stored = self.read_stored(read()).ok();
        let digest = stored.as_ref().and_then(|s| s.digest);
        let mut server: Option<crate::Lyrics> = match stored.as_ref().and_then(|s| s.page.clone()) {
            Some(Page::LyricsPage { v }) => Some(v),
            _ => None,
        };
        if let Some(v) = &server {
            show_server(v);
        }
        // A downloaded song's lyrics were looked up at download: serve them from the cache without
        // requests; the server is re-asked only after a week, after showing them.
        let fresh = stored.as_ref().is_some_and(|s| s.fresh);
        let downloaded = server.is_some() && !fresh && self.core.download_song(&id).is_some();
        let refresh_after = downloaded && self.read_stored_within(read(), DOWNLOADED_SERVER_KEPT_MS).is_ok_and(|s| !s.fresh);
        if !fresh && !downloaded {
            // A server failure just means no server lyrics.
            if let Ok(Some(Page::LyricsPage { v })) = self.read_refresh(read(), digest).await {
                show_server(&v);
                server = Some(v);
            }
        }
        let has_lines = server.as_ref().is_some_and(|l| !l.lines.is_empty());
        let synced = server.as_ref().is_some_and(|l| l.synced);
        let song = self.song_of(id.clone()).await?;
        if let Some(line) = lookup(&*self.transport, &*self.core, &song, has_lines, synced, asked, &Keeping(screen.clone()), &self.lyrics).await {
            nori_perf::perf_log::note_core("lyrics", line.trim_start_matches("lyrics: "));
        }
        if refresh_after {
            // Newly timed server lyrics win; anything else is used next time.
            if let Ok(Some(Page::LyricsPage { v })) = self.read_refresh(read(), digest).await {
                if v.synced && !synced {
                    show_server(&v);
                }
            }
        }
        Ok(())
    }
}

/// Freshness of a downloaded song's cached server lyrics.
pub(crate) const DOWNLOADED_SERVER_KEPT_MS: i64 = MISS_KEPT_MS;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// Bytes of cached service lyrics and picks.
    pub fn lyrics_cache_bytes(&self) -> i64 {
        let c = self.db.lock();
        c.query_row(
            "SELECT COALESCE(SUM(length(key) + length(body)), 0) FROM cache WHERE server=sid() AND key >= ?1 AND key < ?1 || x'ff'",
            [CACHE_PREFIX],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    /// Clears cached service lyrics, keeping the picks of downloaded songs (and the server's lyrics).
    pub fn lyrics_cache_clear(&self) {
        let keep: Vec<String> = self.downloads(true).unwrap_or_default().iter().map(best_key).collect();
        let keep = serde_json::to_string(&keep).unwrap_or_default();
        let _ = self.db.lock().execute(
            "DELETE FROM cache WHERE server=sid() AND key >= ?1 AND key < ?1 || x'ff' AND key NOT IN (SELECT value FROM json_each(?2))",
            rusqlite::params![CACHE_PREFIX, keep],
        );
    }
}

/// Discards picks (background lookups).
struct Unseen;

impl LyricsShown for Unseen {
    fn show(&self, _: LyricsPick) {}
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Looks up lyrics for downloaded songs one by one so they work offline, marking each song's lyrics
    /// work done when its lookup ends. Provider songs are skipped.
    pub async fn lyrics_for_downloads(&self, ids: Vec<String>) {
        for id in ids.into_iter().filter(|id| !crate::is_provider_id(id)) {
            let downloads = &self.core.downloads;
            downloads.with(|t| t.working(&id, crate::transfers::Work::Lyrics));
            let _ = self.lyrics_for(id.clone(), Arc::new(Unseen)).await;
            downloads.with(|t| t.work_done(&id, crate::transfers::Work::Lyrics));
        }
    }

}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::tests::{block, client, Fake};
    use crate::client::NetProfile;
    use crate::transport::FailureKind;
    use nori_settings::lyrics_sources::LyricsService;
    use nori_settings::lyrics_sources::LyricsOrigin;
    use parking_lot::Mutex;

    fn song() -> Song {
        Song { id: "1".into(), title: "Dogs (Remastered)".into(), artist: "Pink Floyd".into(), album: "Animals".into(), duration: 1024, ..Default::default() }
    }

    fn setup() -> (Arc<Client>, Arc<Fake>) {
        client(NetProfile { url: "h".into(), ..Default::default() })
    }

    fn lrclib() -> LyricsLookup {
        LyricsLookup { services: vec![LyricsService::Lrclib], prefer_words: true, paxsenix_key: String::new(), better_lyrics_key: String::new() }
    }

    #[derive(Default)]
    struct Screen(Mutex<Vec<LyricsPick>>);

    impl LyricsShown for Screen {
        fn show(&self, pick: LyricsPick) {
            self.0.lock().push(pick);
        }
    }

    fn run(c: &Client, s: &Song, has_lines: bool, synced: bool, asked: &LyricsLookup) -> Vec<LyricsPick> {
        let screen = Screen::default();
        block(lookup(&*c.transport, &*c.core, s, has_lines, synced, asked, &screen, &c.lyrics));
        screen.0.into_inner()
    }

    const SYNCED: &str = r#"{"subsonic-response":{"status":"ok","lyricsList":{"structuredLyrics":[{"synced":true,"line":[{"start":1500,"value":"timed"}]}]}}}"#;
    const NONE: &str = r#"{"subsonic-response":{"status":"ok","lyricsList":{}}}"#;

    #[test]
    fn server_lyrics_first_and_never_twice() {
        let (c, fake) = setup();
        // Unique titles: a failing service rests per song across tests.
        c.core.session.register(vec![Song { id: "lf1".into(), title: "In Order One".into(), ..song() }, Song { id: "lf2".into(), title: "In Order Two".into(), ..song() }]);
        fake.answer(SYNCED);
        let page = Arc::new(Screen::default());
        block(c.lyrics_for("lf1".into(), page.clone())).unwrap();
        let got = page.0.lock().clone();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].origin, got[0].lyrics.lines[0].text.as_str()), (LyricsOrigin::Server, "timed"));
        assert_eq!(fake.asked().len(), 1, "timed server lyrics: no service asked");
        // Stale, unchanged answer: not shown again.
        c.core.db.lock().execute("UPDATE cache SET ts = 0", []).unwrap();
        fake.answer(SYNCED);
        let again = Arc::new(Screen::default());
        block(c.lyrics_for("lf1".into(), again.clone())).unwrap();
        assert_eq!(again.0.lock().len(), 1);
        // No server lyrics: shown once, at the end.
        fake.answer(NONE);
        let none = Arc::new(Screen::default());
        block(c.lyrics_for("lf2".into(), none.clone())).unwrap();
        let got = none.0.lock().clone();
        assert_eq!(got.len(), 1);
        assert!(got[0].lyrics.lines.is_empty() && got[0].origin == LyricsOrigin::Server);
    }

    #[test]
    fn service_hit_is_cached_and_ranked_by_duration() {
        let (c, fake) = setup();
        fake.answer(r#"{"statusCode":404,"message":"not found"}"#);
        fake.answer(r#"[{"duration":1000,"syncedLyrics":"[00:01.00]far"},{"duration":1026,"plainLyrics":"close plain"},{"duration":1022,"syncedLyrics":"[00:02.00]close synced\n[01:00.00]you gotta be crazy\n[02:00.00]you gotta have a real need\n[03:00.00]you gotta sleep on your toes"}]"#);
        let got = run(&c, &song(), true, false, &lrclib());
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].origin, got[0].lyrics.lines[0].text.as_str()), (LyricsOrigin::Lrclib, "close synced"));
        let asked = fake.asked();
        assert!(asked[0].contains("track_name=Dogs&"));
        assert!(asked[1].starts_with("https://lrclib.net/api/search?track_name=Dogs&artist_name=Pink+Floyd"));
        // Cached: no request the second time.
        let again = run(&c, &song(), false, false, &lrclib());
        assert_eq!(again[0].lyrics.lines[0].text, "close synced");
        assert_eq!(fake.asked().len(), 2);
    }

    #[test]
    fn plain_service_lyrics_do_not_replace_server_plain_and_miss_is_cached() {
        let (c, fake) = setup();
        let s = Song { title: "Plain".into(), ..song() };
        fake.answer(r#"{"plainLyrics":"just words"}"#);
        fake.answer("[]");
        assert!(run(&c, &s, true, false, &lrclib()).is_empty());
        let (c, fake) = setup();
        fake.answer(r#"{"statusCode":404}"#);
        fake.answer("[]");
        let none = run(&c, &s, false, false, &lrclib());
        assert!(none[0].lyrics.lines.is_empty() && none[0].origin == LyricsOrigin::Server);
        run(&c, &s, false, false, &lrclib());
        assert_eq!(fake.asked().len(), 2, "miss cached: {:?}", fake.asked());
    }

    #[test]
    fn lyrics_cache_size_and_clear() {
        let (c, fake) = setup();
        fake.answer(r#"{"statusCode":404}"#);
        fake.answer(r#"[{"duration":1022,"syncedLyrics":"[00:02.00]line one\n[01:00.00]line two here\n[02:00.00]a third line\n[03:00.00]and the fourth"}]"#);
        assert_eq!(c.core.lyrics_cache_bytes(), 0);
        c.core.cache_put("getAlbum|1".into(), b"{}".to_vec()).unwrap();
        run(&c, &song(), false, false, &lrclib());
        let bytes = c.core.lyrics_cache_bytes();
        assert!(bytes > 100, "{bytes}");
        c.core.lyrics_cache_clear();
        assert_eq!(c.core.lyrics_cache_bytes(), 0);
        assert!(c.core.cache_get("getAlbum|1".into()).unwrap().is_some(), "other cache entries stay");
    }

    /// Lyrics of an analysed song get the offset of their times against its vocal curve.
    #[test]
    fn analysed_song_lyrics_get_sync_offset() {
        use nori_player::automix::analysis::Analyzer;
        use nori_player::automix::eval::{Song as Synthetic, Style, FULL, SUNG};
        let (c, fake) = setup();
        let synthetic = Synthetic { sections: vec![(4, FULL), (12, SUNG), (6, FULL), (12, SUNG), (4, FULL)], ..Synthetic::new("offset", Style::Backbeat, 112.0, 2, false) };
        let (x, truth) = synthetic.render();
        let mut a = Analyzer::new(synthetic.rate, 0);
        a.feed(&x);
        nori_automix::store::put_voice(&c.core.db.lock(), "sync1", &a.take_features().voice_curve()).unwrap();
        // A line per sung phrase, 1.2 s late (plus the 0.2 s lead sync.rs expects).
        let mut lrc = String::new();
        let mut last = f64::MIN;
        for (i, (start, _)) in truth.voice.iter().enumerate() {
            if start - last > 0.3 {
                let t = start - 0.2 + 1.2;
                lrc.push_str(&format!("[{:02}:{:05.2}]lomira teshvan korupel sumidah {i} netori falquen\n", (t / 60.0) as u32, t % 60.0));
            }
            last = truth.voice[i].1;
        }
        let secs = (x.len() / synthetic.rate as usize) as u32;
        fake.answer(&serde_json::json!({"syncedLyrics": lrc, "duration": secs, "trackName": "Offset", "artistName": "Pink Floyd"}).to_string());
        let s = Song { id: "sync1".into(), title: "Offset".into(), duration: secs, ..song() };
        let got = run(&c, &s, false, false, &lrclib());
        let l = &got.last().expect("lyrics").lyrics;
        assert!((l.offset_ms - 1200).abs() < 150, "offset {}", l.offset_ms);
        assert_eq!(l.lines[0].start_ms, ((truth.voice[0].0 - 0.2 + 1.2) * 1000.0 / 10.0).round() as i64 * 10, "times unchanged");
    }

    #[test]
    fn downloaded_song_lyrics_survive_cache_clear() {
        let (c, fake) = setup();
        let kept = Song { id: "dl-lyr".into(), title: "Paper Lanterns".into(), artist: "The Invented".into(), album: "Nowhere".into(), duration: 200, ..Default::default() };
        let other = Song { id: "st-lyr".into(), title: "Streamed Only".into(), ..kept.clone() };
        c.core.download_queue(vec![kept.clone()]).unwrap();
        c.core.download_done("dl-lyr".into()).unwrap();
        for s in [&kept, &other] {
            fake.answer(r#"{"statusCode":404}"#);
            fake.answer(r#"[{"duration":200,"syncedLyrics":"[00:02.00]a line made up\n[01:00.00]another made up line\n[02:00.00]a third invented one\n[03:00.00]and the last of them"}]"#);
            assert_eq!(run(&c, s, false, false, &lrclib())[0].origin, LyricsOrigin::Lrclib);
        }
        let kept_at = |c: &Client| c.core.db.lock().query_row("SELECT ts FROM cache WHERE key = ?1", [best_key(&kept)], |r| r.get::<_, i64>(0)).unwrap();
        c.core.db.lock().execute("UPDATE cache SET ts = ts - 86400000", []).unwrap();
        let aged = kept_at(&c);
        c.core.lyrics_cache_clear();
        assert_eq!(kept_at(&c), aged, "a kept pick keeps its age, which times its lookup again");
        // Offline from here.
        let asked = fake.asked().len();
        let offline = run(&c, &kept, false, false, &lrclib());
        assert_eq!((offline[0].origin, offline[0].lyrics.lines[0].text.as_str()), (LyricsOrigin::Lrclib, "a line made up"));
        assert_eq!(fake.asked().len(), asked);
        assert!(run(&c, &other, false, false, &lrclib())[0].lyrics.lines.is_empty(), "streamed song's cleared");
        c.core.download_remove("dl-lyr".into()).unwrap();
        c.core.lyrics_cache_clear();
        assert_eq!(c.core.lyrics_cache_bytes(), 0, "download deleted");
    }

    /// Picks shown, each with the request count at that moment.
    struct Seen {
        fake: Arc<Fake>,
        picks: Mutex<Vec<(usize, LyricsPick)>>,
    }

    impl LyricsShown for Seen {
        fn show(&self, pick: LyricsPick) {
            self.picks.lock().push((self.fake.asked().len(), pick));
        }
    }

    fn open(c: &Client, fake: &Arc<Fake>, id: &str, asked: &LyricsLookup) -> Vec<(usize, LyricsPick)> {
        let seen = Arc::new(Seen { fake: fake.clone(), picks: Mutex::new(Vec::new()) });
        block(c.lyrics_for_with(id.into(), asked, seen.clone())).unwrap();
        let picks = seen.picks.lock().clone();
        picks
    }

    const DAY: i64 = 24 * 3_600_000;

    /// A downloaded song's lyrics open from the cache with no request, online or off; after a week the
    /// server is re-asked, after showing them.
    #[test]
    fn downloaded_song_lyrics_open_without_requests() {
        let (c, fake) = setup();
        let s = Song { id: "dl-open".into(), title: "Harbour Of Tin".into(), artist: "The Invented".into(), album: "Nowhere".into(), duration: 200, ..Default::default() };
        let asked = LyricsLookup { services: vec![LyricsService::Lrclib, LyricsService::LyricsPlus], ..lrclib() };
        c.core.session.register(vec![s.clone()]);
        c.core.download_queue(vec![s.clone()]).unwrap();
        c.core.download_done(s.id.clone()).unwrap();
        // At download: none on the server, line-timed LRCLIB, LyricsPlus unreachable.
        fake.answer(NONE);
        fake.answer(r#"{"statusCode":404}"#);
        fake.answer(r#"[{"duration":200,"syncedLyrics":"[00:02.00]tin boats in the harbour\n[01:00.00]a lantern made of paper\n[02:00.00]nobody sings this line\n[03:00.00]and the tide goes out"}]"#);
        let first = open(&c, &fake, &s.id, &asked);
        assert_eq!(first.last().unwrap().1.origin, LyricsOrigin::Lrclib);
        let kept = |picks: &[(usize, LyricsPick)]| picks.first().is_some_and(|(_, p)| p.origin == LyricsOrigin::Lrclib && p.lyrics.lines[0].text == "tin boats in the harbour");
        let age = |ms: i64| c.core.db.lock().execute("UPDATE cache SET ts = ts - ?1", [ms]).unwrap();
        // Two hours on, online: not asked.
        age(2 * 3_600_000);
        let before = fake.asked().len();
        fake.answer(NONE);
        let online = open(&c, &fake, &s.id, &asked);
        assert!(kept(&online), "{online:?}");
        assert_eq!(fake.asked().len(), before, "opened online: {:?}", &fake.asked()[before..]);
        assert_eq!(online.len(), 1);
        fake.answers.lock().clear();
        let offline = open(&c, &fake, &s.id, &asked);
        assert!(kept(&offline), "{offline:?}");
        assert_eq!(fake.asked().len(), before, "opened offline: {:?}", &fake.asked()[before..]);
        // A week on: shown first, then the server is re-asked once.
        age(8 * DAY);
        let late = open(&c, &fake, &s.id, &asked);
        assert!(kept(&late) && late[0].0 == before, "{late:?}");
        let asked_after = &fake.asked()[before..];
        assert!(matches!(asked_after, [u] if u.contains("getLyricsBySongId")), "{asked_after:?}");
    }

    #[test]
    fn song_of_reads_downloads_without_server() {
        let (c, fake) = setup();
        let s = Song { id: "dl-known".into(), title: "Kept Here".into(), artist: "The Invented".into(), duration: 180, ..Default::default() };
        c.core.download_queue(vec![s.clone()]).unwrap();
        c.core.download_done(s.id.clone()).unwrap();
        let got = block(c.song_of(s.id.clone())).unwrap();
        assert_eq!((got.title.as_str(), got.duration), ("Kept Here", 180));
        assert!(fake.asked().is_empty(), "{:?}", fake.asked());
    }

    #[test]
    fn download_lyrics_in_order_skipping_providers() {
        let (c, fake) = setup();
        fake.answer(SYNCED);
        fake.answer(SYNCED);
        block(c.lyrics_for_downloads(vec!["ext-deezer-7".into(), "dla-1".into(), "dla-2".into()]));
        let lyrics: Vec<String> = fake.asked().into_iter().filter(|u| u.contains("getLyricsBySongId")).collect();
        assert_eq!(lyrics.len(), 2, "{lyrics:?}");
        assert!(lyrics[0].contains("id=dla-1&") && lyrics[1].contains("id=dla-2&"));
        assert!(fake.asked().iter().all(|u| !u.contains("ext-")));
    }

    #[test]
    fn synced_server_lyrics_win_and_failures_are_not_cached() {
        let (c, fake) = setup();
        assert!(run(&c, &song(), true, true, &lrclib()).is_empty());
        assert!(fake.asked().is_empty());
        let s = Song { title: "Offline".into(), ..song() };
        fake.fail(FailureKind::UnknownHost);
        assert_eq!(run(&c, &s, false, false, &lrclib())[0].origin, LyricsOrigin::Server);
        assert_eq!(c.core.lyrics_cache_bytes(), 0);
    }
}
