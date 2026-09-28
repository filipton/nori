//! Lyrics from the lyrics services as the client's calls: asked through the client's transport, the
//! answers kept in the core's response cache. Which services to ask is the settings' (`lyrics_lookup`);
//! how each is asked, and whose answer is shown, are nori-lyrics' (services.rs, race.rs).

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

/// Hands each set of lyrics on to the platform with its timing kept, so the lyrics page starts its clock
/// on them by key (`look::keep`).
struct Keeping(Arc<dyn LyricsShown>);

impl LyricsShown for Keeping {
    fn show(&self, mut pick: LyricsPick) {
        crate::look::keep(&mut pick.lyrics);
        self.0.show(pick);
    }
}

/// Hands on only what differs from what it handed last ([`lyrics_replaces`]): the server's answer read
/// again, or the lookup's choice from before, are not new lyrics to show.
struct Screen {
    to: Arc<dyn LyricsShown>,
    last: parking_lot::Mutex<Option<LyricsPick>>,
}

impl Screen {
    fn show(&self, pick: LyricsPick) {
        let mut last = self.last.lock();
        if lyrics_replaces(last.as_ref(), &pick) {
            *last = Some(pick.clone());
            drop(last);
            self.to.show(pick);
        }
    }
}

impl LyricsShown for Screen {
    fn show(&self, pick: LyricsPick) {
        Screen::show(self, pick);
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Everything the lyrics page shows for song `id`, in order, each to `shown`: the server's own lyrics
    /// (what is stored, then the server's answer when it differs; an empty answer is not shown while a
    /// service may still have the song), then, when those are not timed, what the lyrics services find
    /// ([`Client::lyrics_lookup`]), and at the end an empty answer from the server when nobody had
    /// anything. Nothing is handed on twice. Returns when all is in; dropping the call cancels every
    /// request in it.
    pub async fn lyrics_for(&self, id: String, shown: Arc<dyn LyricsShown>) -> NetResult<()> {
        let asked = asked_now();
        self.lyrics_for_with(id, &asked, shown).await
    }
}

/// The lyrics services the settings ask now. Nothing kept yet is the defaults, which ask nobody.
fn asked_now() -> LyricsLookup {
    crate::settings_store::with_prefs(lyrics_lookup).unwrap_or_else(|| lyrics_lookup(&StoredPrefs::default()))
}

impl Client {
    /// [`Client::lyrics_for`] with the services to ask given.
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
        // A downloaded song's lyrics were looked up as it was downloaded: what is stored is shown and the
        // lookup served from it with no request, online or not. The server's answer is asked again only
        // once it is a week old, and then after the lyrics are on screen.
        let fresh = stored.as_ref().is_some_and(|s| s.fresh);
        let downloaded = server.is_some() && !fresh && self.core.download_song(&id).is_some();
        let refresh_after = downloaded && self.read_stored_within(read(), DOWNLOADED_SERVER_KEPT_MS).is_ok_and(|s| !s.fresh);
        if !fresh && !downloaded {
            // A failure to read the server's is no lyrics from it: the services are asked all the same.
            if let Ok(Some(Page::LyricsPage { v })) = self.read_refresh(read(), digest).await {
                show_server(&v);
                server = Some(v);
            }
        }
        let has_lines = server.as_ref().is_some_and(|l| !l.lines.is_empty());
        let synced = server.as_ref().is_some_and(|l| l.synced);
        let song = self.song_of(id.clone()).await?;
        self.lookup_with(&song, has_lines, synced, asked, &Keeping(screen.clone())).await;
        if refresh_after {
            // Timed lyrics from the server win over a service's, as on a first lookup; anything else it
            // says now is read from the store next time.
            if let Ok(Some(Page::LyricsPage { v })) = self.read_refresh(read(), digest).await {
                if v.synced && !synced {
                    show_server(&v);
                }
            }
        }
        Ok(())
    }
}

/// How long the server's own lyrics stored for a downloaded song stand before it is asked again (after
/// the lyrics are shown): a week, as a service's miss.
pub const DOWNLOADED_SERVER_KEPT_MS: i64 = MISS_KEPT_MS;

/// Asked only in Rust, so not exported to Kotlin.
impl Client {
    /// What to show once the server's own lyrics are in (`server_has_lines`, `server_synced` describe
    /// them; the platform shows them itself): the lyrics services the settings switch on, asked together,
    /// each better answer handed to `shown` as it comes. The server's synced lyrics win and nothing is
    /// asked. An empty server answer is not shown while a service may still have the song (the panel read
    /// "No lyrics" for a moment and then filled in); it comes to `shown` at the end when nothing better
    /// was found. Returns when the lookup is over; dropping the call cancels every request in it.
    pub async fn lyrics_lookup(&self, id: String, server_has_lines: bool, server_synced: bool, shown: Arc<dyn LyricsShown>) -> NetResult<()> {
        let asked = asked_now();
        // The song playing, as the queue keeps it: the platform names it by id.
        let song = self.song_of(id).await?;
        self.lookup_with(&song, server_has_lines, server_synced, &asked, &Keeping(shown)).await;
        Ok(())
    }
}

impl Client {
    async fn lookup_with(&self, song: &Song, server_has_lines: bool, server_synced: bool, asked: &LyricsLookup, shown: &dyn LyricsShown) {
        if let Some(line) = lookup(&*self.transport, &*self.core, song, server_has_lines, server_synced, asked, shown, &self.lyrics).await {
            nori_perf::perf_log::note_core("lyrics", line.trim_start_matches("lyrics: "));
        }
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Core {
    /// How much the lyrics looked up online take in the app's database, in bytes: every service's
    /// answers and the lyrics chosen for each song (Settings, Storage, "Lyrics").
    pub fn lyrics_cache_bytes(&self) -> i64 {
        let c = self.db.lock();
        c.query_row(
            "SELECT COALESCE(SUM(length(key) + length(body)), 0) FROM cache WHERE server=sid() AND key >= ?1 AND key < ?1 || x'ff'",
            [CACHE_PREFIX],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    /// Forgets every lyrics lookup: the next time a song's lyrics are opened, the services are asked
    /// again. The server's own lyrics are not touched, nor the lyrics chosen for a downloaded song, so it
    /// keeps them offline; once its download is deleted they go with the next clear.
    pub fn lyrics_cache_clear(&self) {
        let keep: Vec<(String, Vec<u8>)> =
            self.downloads(true).unwrap_or_default().iter().map(best_key).filter_map(|k| Some((k.clone(), self.cache_get(k).ok().flatten()?))).collect();
        let _ = self.cache_evict(CACHE_PREFIX.to_string());
        for (k, body) in keep {
            let _ = self.cache_put(k, body);
        }
    }
}

/// Lyrics looked up for a song nobody is looking at.
struct Unseen;

impl LyricsShown for Unseen {
    fn show(&self, _: LyricsPick) {}
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Looks up the lyrics of songs just downloaded, one after another, as the lyrics page does (the
    /// server's, then the services the settings switch on), so they are kept and shown when the song is
    /// played offline (see [`Core::lyrics_cache_clear`]). Provider songs are left alone. Found, failed or
    /// offline, each song stops waiting for its lyrics as its lookup ends (`transfers::work_done`).
    pub async fn lyrics_for_downloads(&self, ids: Vec<String>) {
        for id in ids.into_iter().filter(|id| !id.starts_with("ext-")) {
            // Timed from here, and given up if it hangs (`download_processing_expire`).
            crate::transfers::working(&id, crate::transfers::Work::Lyrics);
            let _ = self.lyrics_for(id.clone(), Arc::new(Unseen)).await;
            crate::transfers::work_done(&id, crate::transfers::Work::Lyrics);
        }
    }

    /// Returns once none of `ids` is processing any more: its analysis and lyrics over, or its time up
    /// (`download_processing_expire`).
    pub async fn downloads_processed(&self, ids: Vec<String>) {
        crate::transfers::processed(&ids).await
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
        block(c.lookup_with(s, has_lines, synced, asked, &screen));
        screen.0.into_inner()
    }

    #[derive(Default)]
    struct Page(Mutex<Vec<LyricsPick>>);

    impl LyricsShown for Page {
        fn show(&self, pick: LyricsPick) {
            self.0.lock().push(pick);
        }
    }

    const SYNCED: &str = r#"{"subsonic-response":{"status":"ok","lyricsList":{"structuredLyrics":[{"synced":true,"line":[{"start":1500,"value":"timed"}]}]}}}"#;
    const NONE: &str = r#"{"subsonic-response":{"status":"ok","lyricsList":{}}}"#;

    #[test]
    fn the_servers_lyrics_come_first_and_nothing_is_shown_twice() {
        let (c, fake) = setup();
        // Titles of their own: a service that fails for a song rests for it, whichever test asked.
        crate::queue::queue_register(vec![Song { id: "lf1".into(), title: "In Order One".into(), ..song() }, Song { id: "lf2".into(), title: "In Order Two".into(), ..song() }]);
        fake.answer(SYNCED);
        let page = Arc::new(Page::default());
        block(c.lyrics_for("lf1".into(), page.clone())).unwrap();
        let got = page.0.lock().clone();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].origin, got[0].lyrics.lines[0].text.as_str()), (LyricsOrigin::Server, "timed"));
        assert_eq!(fake.asked().len(), 1, "timed lyrics from the server: no service asked");
        // Stale, and the server answers the same: not handed on again.
        c.core.db.lock().execute("UPDATE cache SET ts = 0", []).unwrap();
        fake.answer(SYNCED);
        let again = Arc::new(Page::default());
        block(c.lyrics_for("lf1".into(), again.clone())).unwrap();
        assert_eq!(again.0.lock().len(), 1, "the stored answer only");
        // No lyrics at the server: said once, at the end, not while services may still have them.
        fake.answer(NONE);
        let none = Arc::new(Page::default());
        block(c.lyrics_for("lf2".into(), none.clone())).unwrap();
        let got = none.0.lock().clone();
        assert_eq!(got.len(), 1);
        assert!(got[0].lyrics.lines.is_empty() && got[0].origin == LyricsOrigin::Server);
    }

    #[test]
    fn a_hit_is_kept_in_the_response_cache_and_a_search_is_ranked_by_length() {
        let (c, fake) = setup();
        fake.answer(r#"{"statusCode":404,"message":"not found"}"#);
        fake.answer(r#"[{"duration":1000,"syncedLyrics":"[00:01.00]far"},{"duration":1026,"plainLyrics":"close plain"},{"duration":1022,"syncedLyrics":"[00:02.00]close synced\n[01:00.00]you gotta be crazy\n[02:00.00]you gotta have a real need\n[03:00.00]you gotta sleep on your toes"}]"#);
        let got = run(&c, &song(), true, false, &lrclib());
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].origin, got[0].lyrics.lines[0].text.as_str()), (LyricsOrigin::Lrclib, "close synced"));
        let asked = fake.asked();
        assert!(asked[0].contains("track_name=Dogs&"));
        assert!(asked[1].starts_with("https://lrclib.net/api/search?track_name=Dogs&artist_name=Pink+Floyd"));
        // Kept: no request the second time.
        let again = run(&c, &song(), false, false, &lrclib());
        assert_eq!(again[0].lyrics.lines[0].text, "close synced");
        assert_eq!(fake.asked().len(), 2);
    }

    #[test]
    fn plain_words_do_not_replace_the_servers_plain_ones_and_a_miss_is_kept() {
        let (c, fake) = setup();
        let s = Song { title: "Plain".into(), ..song() };
        fake.answer(r#"{"plainLyrics":"just words"}"#);
        fake.answer("[]");
        assert!(run(&c, &s, true, false, &lrclib()).is_empty(), "the server's own words stay");
        let (c, fake) = setup();
        fake.answer(r#"{"statusCode":404}"#);
        fake.answer("[]");
        let none = run(&c, &s, false, false, &lrclib());
        assert!(none[0].lyrics.lines.is_empty() && none[0].origin == LyricsOrigin::Server);
        run(&c, &s, false, false, &lrclib());
        assert_eq!(fake.asked().len(), 2, "the miss is kept, and fresh for a week: {:?}", fake.asked());
    }

    #[test]
    fn the_lyrics_cache_is_measured_and_cleared_alone() {
        let (c, fake) = setup();
        fake.answer(r#"{"statusCode":404}"#);
        fake.answer(r#"[{"duration":1022,"syncedLyrics":"[00:02.00]line one\n[01:00.00]line two here\n[02:00.00]a third line\n[03:00.00]and the fourth"}]"#);
        assert_eq!(c.core.lyrics_cache_bytes(), 0);
        c.core.cache_put("getAlbum|1".into(), b"{}".to_vec()).unwrap();
        run(&c, &song(), false, false, &lrclib());
        let bytes = c.core.lyrics_cache_bytes();
        assert!(bytes > 100, "the answer and the choice: {bytes}");
        c.core.lyrics_cache_clear();
        assert_eq!(c.core.lyrics_cache_bytes(), 0);
        assert!(c.core.cache_get("getAlbum|1".into()).unwrap().is_some(), "the rest of the cache stays");
    }

    /// A song measured before its lyrics are looked up: the lyrics come out with the offset their times are
    /// off by against its voice, for the page's clock to apply. The words are invented.
    #[test]
    fn a_measured_songs_lyrics_come_with_their_offset() {
        use nori_player::automix::analysis::Analyzer;
        use nori_player::automix::eval::{Song as Synthetic, Style, FULL, SUNG};
        let (c, fake) = setup();
        let synthetic = Synthetic { sections: vec![(4, FULL), (12, SUNG), (6, FULL), (12, SUNG), (4, FULL)], ..Synthetic::new("offset", Style::Backbeat, 112.0, 2, false) };
        let (x, truth) = synthetic.render();
        let mut a = Analyzer::new(synthetic.rate, 0);
        a.feed(&x);
        nori_automix::store::put_voice(&c.core.db.lock(), "sync1", &a.take_features().voice_curve()).unwrap();
        // A line at each sung phrase, every one 1.2 s late. Services start a line a little before its voice is
        // heard (its consonant, and time to read it): 0.2 s, what nori-lyrics' sync.rs allows for.
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
        assert_eq!(l.lines[0].start_ms, ((truth.voice[0].0 - 0.2 + 1.2) * 1000.0 / 10.0).round() as i64 * 10, "the times themselves are left as they came");
    }

    #[test]
    fn a_downloaded_songs_lyrics_outlive_clearing_the_cache_until_the_download_goes() {
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
        c.core.lyrics_cache_clear();
        // No network now (the fake has nothing left to answer).
        let asked = fake.asked().len();
        let offline = run(&c, &kept, false, false, &lrclib());
        assert_eq!((offline[0].origin, offline[0].lyrics.lines[0].text.as_str()), (LyricsOrigin::Lrclib, "a line made up"));
        assert_eq!(fake.asked().len(), asked, "the downloaded song's lyrics came from the cache");
        assert!(run(&c, &other, false, false, &lrclib())[0].lyrics.lines.is_empty(), "the streamed song's were cleared");
        c.core.download_remove("dl-lyr".into()).unwrap();
        c.core.lyrics_cache_clear();
        assert_eq!(c.core.lyrics_cache_bytes(), 0, "the download deleted, its lyrics go too");
    }

    /// What a page was handed, each with how many requests had gone out by then.
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

    /// A downloaded song's lyrics, chosen as it was downloaded, open from the store at once: no request,
    /// online or off, however long ago the server's own answer was asked (its hour long past), and the
    /// word-timing services a line-timed answer would widen to are not asked on an open either. A week
    /// on the server's answer is asked again, after the kept lyrics are on screen. The words are invented.
    #[test]
    fn a_downloaded_songs_kept_lyrics_open_with_no_request_online_or_off() {
        let (c, fake) = setup();
        let s = Song { id: "dl-open".into(), title: "Harbour Of Tin".into(), artist: "The Invented".into(), album: "Nowhere".into(), duration: 200, ..Default::default() };
        let asked = LyricsLookup { services: vec![LyricsService::Lrclib, LyricsService::LyricsPlus], ..lrclib() };
        crate::queue::queue_register(vec![s.clone()]);
        c.core.download_queue(vec![s.clone()]).unwrap();
        c.core.download_done(s.id.clone()).unwrap();
        // As it downloads: no lyrics at the server, LRCLIB's line-timed ones, and LyricsPlus (words timed,
        // asked because the best is only timed by line) out of reach.
        fake.answer(NONE);
        fake.answer(r#"{"statusCode":404}"#);
        fake.answer(r#"[{"duration":200,"syncedLyrics":"[00:02.00]tin boats in the harbour\n[01:00.00]a lantern made of paper\n[02:00.00]nobody sings this line\n[03:00.00]and the tide goes out"}]"#);
        let first = open(&c, &fake, &s.id, &asked);
        assert_eq!(first.last().unwrap().1.origin, LyricsOrigin::Lrclib);
        let kept = |picks: &[(usize, LyricsPick)]| picks.first().is_some_and(|(_, p)| p.origin == LyricsOrigin::Lrclib && p.lyrics.lines[0].text == "tin boats in the harbour");
        let age = |ms: i64| c.core.db.lock().execute("UPDATE cache SET ts = ts - ?1", [ms]).unwrap();
        // Two hours on, online: the server would answer, and is not asked.
        age(2 * 3_600_000);
        let before = fake.asked().len();
        fake.answer(NONE);
        let online = open(&c, &fake, &s.id, &asked);
        assert!(kept(&online), "{online:?}");
        assert_eq!(fake.asked().len(), before, "opened online: {:?}", &fake.asked()[before..]);
        assert_eq!(online.len(), 1, "shown once");
        // Offline: the same, and nothing waited on the network.
        fake.answers.lock().clear();
        let offline = open(&c, &fake, &s.id, &asked);
        assert!(kept(&offline), "{offline:?}");
        assert_eq!(fake.asked().len(), before, "opened offline: {:?}", &fake.asked()[before..]);
        // A week on: the kept lyrics first, before any request; then the server's answer is asked again
        // (offline here, so it stands), once.
        age(8 * DAY);
        let late = open(&c, &fake, &s.id, &asked);
        assert!(kept(&late) && late[0].0 == before, "shown before anything was asked: {late:?}");
        assert!(fake.asked().len() - before <= 2, "{:?}", &fake.asked()[before..]);
    }

    /// A downloaded song not in the queue (the lyrics asked for from elsewhere) is the one kept with its
    /// download, not asked of the server.
    #[test]
    fn a_downloaded_song_is_known_without_the_server() {
        let (c, fake) = setup();
        let s = Song { id: "dl-known".into(), title: "Kept Here".into(), artist: "The Invented".into(), duration: 180, ..Default::default() };
        c.core.download_queue(vec![s.clone()]).unwrap();
        c.core.download_done(s.id.clone()).unwrap();
        let got = block(c.song_of(s.id.clone())).unwrap();
        assert_eq!((got.title.as_str(), got.duration), ("Kept Here", 180));
        assert!(fake.asked().is_empty(), "{:?}", fake.asked());
    }

    #[test]
    fn downloads_are_looked_up_one_after_another_and_a_providers_song_never() {
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
    fn server_synced_lyrics_win_and_a_failure_is_not_kept() {
        let (c, fake) = setup();
        assert!(run(&c, &song(), true, true, &lrclib()).is_empty());
        assert!(fake.asked().is_empty());
        let s = Song { title: "Offline".into(), ..song() };
        fake.fail(FailureKind::UnknownHost);
        assert_eq!(run(&c, &s, false, false, &lrclib())[0].origin, LyricsOrigin::Server);
        // Not kept: nothing of it in the lyrics' cache (the service rests a while instead, and is asked
        // again after).
        assert_eq!(c.core.lyrics_cache_bytes(), 0, "the failure was kept as an answer");
    }
}
