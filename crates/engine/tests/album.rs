//! An album played in order with AutoMix (or a crossfade) on and "keep albums gapless" on is played as
//! though transitions were off: every song to its last sample and the next from its first, nothing cut at
//! either end, however the plan out of a song came about (measured before, measured while it plays, a seek
//! near the end, the setting switched while it plays). Where a mix is allowed (into another album, or
//! shuffled) it still mixes. The engine playing the core's queue over the core's planner, library, stream
//! cache and measurer on the test's clock, as Android and the terminal run it. The core keeps one queue,
//! planner and database per process, so the stories run one after the other, and never beside replan.rs's
//! (both hold `core_turn` in main.rs).

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_core::client::{Client, NetProfile};
use nori_core::settings_store::{edit_by_name, APPLY_AUDIO, REPLAN};
use nori_core::transport::{Exchange, Transport, TransportError, TransportResponse};
use nori_core::{Core, ServerConfig, Song};
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue, Measurer};
use nori_engine::{Body, ByteSource, Config, Engine, State, Store};

mod common;

struct NoApi;

#[async_trait::async_trait]
impl Transport for NoApi {
    async fn get(&self, _url: String, _timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 500, body: Vec::new() })
    }

    async fn send(&self, _request: Exchange) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse { status: 500, body: Vec::new() })
    }

    fn address_changed(&self) {}
}

const RATE: usize = 44_100;
const SECS: usize = 40;

/// A song's samples, stereo, both channels the same: a steady beat at 120 bpm over a tone of its own pitch,
/// so the measurer finds a grid and the planner a beat-matched mix, and a quiet noise that never repeats,
/// so that no stretch of a song is like another stretch of it or of another song.
fn samples(seed: u32) -> Vec<i16> {
    let frames = RATE * SECS;
    let mut s = Vec::with_capacity(frames * 2);
    let mut noise = 0x9e37_79b9u32 ^ seed.wrapping_mul(0x85eb_ca6b);
    for i in 0..frames {
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        let in_beat = i % (RATE / 2);
        let click = if in_beat < 2000 { (1.0 - in_beat as f64 / 2000.0) * 0.6 } else { 0.0 };
        let tone = (i as f64 * (220.0 + seed as f64 * 17.0) * std::f64::consts::TAU / RATE as f64).sin() * 0.1;
        let hiss = (noise as f64 / u32::MAX as f64 - 0.5) * 0.02;
        let v = ((click * ((i * 7919) % 97) as f64 / 97.0 + tone + hiss) * 32767.0) as i16;
        s.extend([v, v]);
    }
    s
}

fn wav(samples: &[i16]) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&(RATE as u32).to_le_bytes());
    w.extend_from_slice(&(RATE as u32 * 4).to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.extend(samples.iter().flat_map(|v| v.to_le_bytes()));
    w
}

struct Net(HashMap<String, Arc<Vec<u8>>>);

impl ByteSource for Net {
    fn open(&self, url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let id = url.split("&id=").nth(1).unwrap_or("").split('&').next().unwrap_or("");
        let file = self.0.get(id).cloned().ok_or("no such song")?;
        let len = file.len() as u64;
        let mut c = Cursor::new(file.as_ref().clone());
        c.set_position(from);
        Ok(Body { start: from, len: Some(len), reader: Box::new(c) })
    }
}

/// A song of the story: its id, album and disc; its track is the next on that disc.
#[derive(Clone, Copy)]
struct S(&'static str, &'static str, u32);

/// When the songs are measured.
#[derive(Clone, Copy, PartialEq)]
enum Measured {
    /// Before the play: every plan is made with both songs' analyses.
    Before,
    /// By the measurer, while the songs before them play: a plan made without them is made again.
    WhilePlaying,
}

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    card: Card,
    core: Arc<Core>,
    store: Arc<Store>,
    measurer: Option<Arc<Measurer>>,
    songs: Vec<(S, Vec<i16>)>,
    _dir: nori_testdir::TempDir,
}

impl Rig {
    /// `songs` queued in this order (shuffled if `shuffle`), each whole in the stream cache, AutoMix on (or
    /// a crossfade of `crossfade` s instead) and albums kept gapless as `keep` says.
    fn new(name: &str, songs: &[S], auto_mix: bool, crossfade: i32, keep: bool, measured: Measured, shuffle: bool) -> Rig {
        Rig::tagged(name, songs, auto_mix, crossfade, keep, measured, shuffle, &Tags::default())
    }

    /// [`Rig::new`], with the songs' tags as `tags` makes them.
    #[allow(clippy::too_many_arguments)]
    fn tagged(name: &str, songs: &[S], auto_mix: bool, crossfade: i32, keep: bool, measured: Measured, shuffle: bool, tags: &Tags) -> Rig {
        let dir = nori_testdir::TempDir::new(name);
        let core = Core::new(dir.join("nori.db").to_string_lossy().into_owned(), "test".into()).unwrap();
        core.configure(ServerConfig { url: "http://music.test".into(), user: "u".into(), password: "p".into(), api_key: None, legacy_auth: false }).unwrap();
        let client = Client::new(core.clone(), Arc::new(NoApi));
        client.set_profile(NetProfile { url: "http://music.test".into(), ..Default::default() });
        let mut prefs = nori_core::settings_store::settings_open(dir.join("app.db").to_string_lossy().into_owned()).unwrap();
        (prefs.auto_mix, prefs.crossfade_sec, prefs.crossfade_keep_albums, prefs.auto_mix_max_s) = (auto_mix, crossfade, keep, 8);
        nori_core::settings_store::settings_put(prefs.clone());
        let store = Store::open(dir.join("music"), 512 << 20, Box::new(CoreOrder)).unwrap();
        let mut tracks: HashMap<(&str, u32), u32> = HashMap::new();
        let made: Vec<(S, Vec<i16>)> = songs
            .iter()
            .enumerate()
            .map(|(k, s)| {
                let mut pcm = samples(k as u32);
                if tags.quiet_ends {
                    quiet_end(&mut pcm);
                }
                (*s, pcm)
            })
            .collect();
        let listed: Vec<Song> = made
            .iter()
            .map(|(s, _)| {
                let track = tracks.entry((s.1, s.2)).or_default();
                *track += 1;
                Song { id: s.0.into(), title: s.0.into(), album_id: Some(s.1.into()), track: *track, disc_number: s.2, duration: SECS as u32, suffix: "wav".into(), ..Default::default() }
            })
            .collect();
        let mut listed = listed;
        (tags.tag)(&mut listed);
        let mut files = HashMap::new();
        for (s, pcm) in &made {
            let bytes = wav(pcm);
            let mut w = store.writer(&format!("{}:0", s.0)).expect("a writer for the cache");
            assert!(w.write(0, &bytes));
            assert!(w.finish(bytes.len() as u64), "{} is whole in the cache", s.0);
            files.insert(s.0.to_string(), Arc::new(bytes));
            if measured == Measured::Before {
                let mut a = nori_player::automix::analysis::Analyzer::new(RATE as u32, SECS as u64 * 1000);
                a.feed_interleaved(pcm, 2, |v: i16| v as f32 / 32768.0);
                assert!(core.analysis_finish(s.0, a).unwrap().is_some(), "{} is measured", s.0);
            }
        }
        nori_core::queue::queue_register(listed.clone());
        if let Some(again) = tags.again {
            // Registered a second time, from another reading of the library that tags them otherwise.
            let mut twice = listed;
            again(&mut twice);
            nori_core::queue::queue_register(twice);
        }
        queue(&made.iter().map(|(s, _)| *s).collect::<Vec<_>>(), shuffle, tags.queued);
        let measurer = (measured == Measured::WhilePlaying).then(|| Measurer::new(core.clone(), client.clone(), store.clone()));
        let library = CoreLibrary { client, bytes: Arc::new(Net(files)), metered: false, store: Some(store.clone()) };
        let app = match &measurer {
            Some(m) => CoreApp::new().measuring(m.clone()),
            None => CoreApp::new(),
        };
        let card = Card::new();
        let clock = Virtual::default();
        let engine = Engine::start_on(library, app, CoreQueue, Box::new(card.clone()), None, Config { memory_mb: 256, settings: settings(&prefs, 0.0), ..Config::default() }, clock.clone(), |_| {});
        engine.queue_changed();
        Rig { engine, time: Stepper::new(clock, card.pull.clone()), card, core, store, measurer, songs: made, _dir: dir }
    }

    fn until(&self, secs: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        self.time.until(Duration::from_secs(secs), || {
            self.settle();
            done(self)
        })
    }

    /// Waits, in real time, until the measuring in the background is done: on a device a song lasts
    /// minutes and measuring the next takes seconds, so the analyses are there long before the boundary.
    fn settle(&self) {
        let Some(m) = &self.measurer else { return };
        let until = Instant::now() + Duration::from_secs(120);
        while self.store.fetching_ahead() || nori_engine::core::measuring_as_they_come() || m.busy() {
            assert!(Instant::now() < until, "the measuring ends");
            std::thread::park_timeout(Duration::from_millis(5));
        }
    }

    /// A setting by name, as the settings rows set it, and what the player is told of it.
    fn set(&self, name: &str, value: &str) {
        let effect = edit_by_name(name, value).unwrap_or_else(|| panic!("{name} is a setting")).effect;
        if effect & APPLY_AUDIO != 0 {
            self.engine.set_settings(settings(&nori_core::settings_store::settings_current().unwrap(), 0.0));
        }
        if effect & REPLAN != 0 {
            self.engine.replan();
        }
    }

    /// Plays on to the end of the queue: whether a mix was heard on the way, and the songs in the order
    /// they were heard.
    fn to_the_end(&self) -> (bool, Vec<String>) {
        let mut mixed = false;
        let mut order: Vec<String> = Vec::new();
        let ended = self.until(SECS as u64 * (self.songs.len() as u64 + 2), |r| {
            let s = r.engine.status();
            mixed |= s.mixing;
            if let Some(id) = s.id.filter(|id| order.last() != Some(id)) {
                order.push(id);
            }
            s.state == State::Ended
        });
        assert!(ended, "the queue plays to its end: {:?}", self.engine.status());
        assert_eq!(order.len(), self.songs.len(), "every song is heard: {order:?}");
        (mixed, order)
    }

    /// Checks what the card heard from `from_heard` on (a frame index) against the songs in play order from
    /// `first`, where `joins[k]` says whether song `first + k` goes into the next gaplessly (`true`) or is
    /// mixed (`false`). Found where the first is heard; each gapless join is the whole of the outgoing song
    /// to its last sample and then the incoming one from its first; after a mix the incoming song is found
    /// again and must play to its end from there. The music made again behind a dip (a setting changed
    /// while playing) is the song turned down and up again, not a cut: `dips` of them are let through, each
    /// shorter than [`DIP_MAX`] and going on within [`DIP_SLIP`] of where it was.
    fn heard_as(&self, order: &[String], from_heard: usize, first: usize, joins: &[bool], dips: usize) {
        let heard = self.card.heard.lock().clone();
        let order: Vec<&(S, Vec<i16>)> = order.iter().map(|id| self.songs.iter().find(|(s, _)| s.0 == *id).unwrap()).collect();
        let frames = heard.len() / 2;
        let at = |f: usize| heard.get(f * 2).copied().unwrap_or(0.0);
        let pcm = |k: usize, f: usize| order[k].1.get(f * 2).map_or(0.0, |v| *v as f32 / 32768.0);
        let len = RATE * SECS;
        const TOL: f32 = 2e-4;
        let same = |k: usize, hf: usize, sf: usize| (at(hf) - pcm(k, sf)).abs() < TOL;
        const W: usize = 256;
        let matches = |k: usize, hf: usize, sf: usize| hf + W <= frames && sf + W <= len && (0..W).all(|j| same(k, hf + j, sf + j));
        // Where in song `k` the heard frames from `hf` on are, if anywhere.
        let find = |k: usize, hf: usize| (0..len - W).find(|&sf| matches(k, hf, sf));
        // What is heard from `f` on: which song, and where in it.
        let what = |f: usize| {
            (0..order.len())
                .find_map(|o| find(o, f).map(|x| format!("{} at {} ms", order[o].0 .0, x * 1000 / RATE)))
                .unwrap_or_else(|| "none of the songs as they are".into())
        };
        let ms = |f: usize| f * 1000 / RATE;
        // (song, at ms, frames it went on from later than where it was)
        let mut dipped: Vec<(String, usize, i64)> = Vec::new();
        // Past the dip of a seek or a play: a second in.
        let mut hf = from_heard + RATE;
        let mut sf = find(first, hf).unwrap_or_else(|| panic!("{} is heard {} ms after {} ms: {}", order[first].0 .0, 1000, ms(from_heard), what(hf)));
        for k in first..order.len() {
            let id = order[k].0 .0;
            let gapless_out = joins.get(k - first).copied().unwrap_or(true);
            if gapless_out {
                // Every sample to the end, then the next song's first.
                while sf < len {
                    if hf >= frames {
                        panic!("{id} is heard to its end: it stops at {} ms of {} ms", ms(sf), ms(len));
                    }
                    if same(k, hf, sf) {
                        (hf, sf) = (hf + 1, sf + 1);
                        continue;
                    }
                    // Not the song as it is from here: the music made again behind a dip goes on from about here
                    // within a moment; anything else (another song, another place in this one) is a cut.
                    let cut = |hf: usize, sf: usize, how: &str| -> ! {
                        let then: Vec<String> = [0, RATE / 10, RATE / 2, RATE, 3 * RATE, 6 * RATE].iter().map(|d| format!("+{} ms: {}", ms(*d), what(hf + d))).collect();
                        panic!(
                            "{id} is {how} at {} ms of its {} ms (heard at {} ms); heard from there {then:?}; the plan out of it {:?}",
                            ms(sf),
                            ms(len),
                            ms(hf),
                            nori_core::automix::planner::transition_note(id)
                        )
                    };
                    let (back, expect) = (hf + DIP_MAX, sf + DIP_MAX);
                    if expect >= len {
                        cut(hf, sf, "faded out at its end");
                    }
                    let slip = (0..=DIP_SLIP).flat_map(|d| [expect + d, expect.saturating_sub(d)]).find(|&x| matches(k, back, x));
                    let Some(on) = slip else { cut(hf, sf, "cut") };
                    dipped.push((id.into(), ms(sf), on as i64 - expect as i64));
                    (hf, sf) = (back, on);
                }
                if k + 1 == order.len() {
                    // Nothing more but silence.
                    assert!((hf..frames).all(|f| at(f).abs() < 1e-6), "nothing after the last song");
                }
                sf = 0;
            } else {
                // Found again a few seconds past where the outgoing song ends, after the mix.
                let after = hf + (len - sf) + 4 * RATE;
                let next = order[k + 1].0 .0;
                let found = find(k + 1, after).unwrap_or_else(|| panic!("{next} is heard alone after the mix out of {id}: {}", what(after)));
                (hf, sf) = (after, found);
            }
        }
        assert!(dipped.len() <= dips, "{dips} dips at most, (song, at ms, frames later than it was): {dipped:?}");
    }
}

/// How far from where it was the music made again behind a dip may go on, frames: the song is opened again
/// ahead of the ear and taken up to 40 ms short of where it was opened (nori_player's `OPENED_EARLY_MS`), the
/// dip covering it.
const DIP_SLIP: usize = RATE * 40 / 1000;

/// The longest dip the music is made again behind, frames: the engine turns it down and up again over 30 ms
/// each way, and the two overlap a little longer.
const DIP_MAX: usize = RATE * 15 / 100;

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

/// How a story's songs are tagged beyond the defaults (each song's track the next on its disc, the album's
/// id and disc as given): as an imperfect library tags them. And how they were queued.
struct Tags {
    tag: fn(&mut Vec<Song>),
    queued: Queued,
    /// The songs registered a second time, tagged by this, after the first.
    again: Option<fn(&mut Vec<Song>)>,
    /// Every song's last four seconds fade to near silence.
    quiet_ends: bool,
}

impl Default for Tags {
    fn default() -> Self {
        Tags { tag: |_| {}, again: None, quiet_ends: false, queued: Queued::AsAlbums }
    }
}

/// How the songs came into the queue.
#[derive(Clone, Copy, PartialEq)]
enum Queued {
    /// Each album played as an album: the first from its page, each after it added to the queue whole.
    AsAlbums,
    /// The first song played on its own, then each of the others added to the queue by hand, one at a time.
    OneByOne,
    /// The first song played on its own, then the others brought by autofill.
    Autofill,
}

/// The queue of `songs` (shuffled if `shuffle`), as they were queued.
fn queue(songs: &[S], shuffle: bool, how: Queued) {
    use nori_core::playlist::{playlist_set, playlist_take, with, Hand};
    let ids = |s: &[S]| s.iter().map(|s| s.0.to_string()).collect::<Vec<_>>();
    let album = |s: &S| Some(nori_core::PageOrigin::new(nori_core::OriginKind::Album, s.1));
    let len = || with(|p| p.len()) as u32;
    match how {
        Queued::AsAlbums => {
            let mut runs: Vec<&[S]> = Vec::new();
            let mut from = 0;
            for k in 1..=songs.len() {
                if k == songs.len() || songs[k].1 != songs[from].1 {
                    runs.push(&songs[from..k]);
                    from = k;
                }
            }
            playlist_set(ids(runs[0]), 0, shuffle, album(&runs[0][0]));
            for r in &runs[1..] {
                playlist_take(len(), ids(r), vec![Hand::No; r.len()], album(&r[0]));
            }
        }
        Queued::OneByOne => {
            playlist_set(ids(&songs[..1]), 0, shuffle, None);
            for s in &songs[1..] {
                playlist_take(len(), ids(std::slice::from_ref(s)), vec![Hand::Last], None);
            }
        }
        Queued::Autofill => {
            playlist_set(ids(&songs[..1]), 0, shuffle, None);
            playlist_take(len(), ids(&songs[1..]), vec![Hand::No; songs.len() - 1], None);
        }
    }
    assert_eq!(with(|p| p.ids().to_vec()), ids(songs), "queued in order");
}

/// The last four seconds of `pcm` faded out to near silence, as many album tracks end.
fn quiet_end(pcm: &mut [i16]) {
    let fade = RATE * 4;
    let frames = pcm.len() / 2;
    for f in frames - fade..frames {
        let g = (frames - f) as f64 / fade as f64 * 0.01;
        for c in 0..2 {
            pcm[f * 2 + c] = (pcm[f * 2 + c] as f64 * g) as i16;
        }
    }
}

const ALBUM: [S; 3] = [S("a1", "al", 1), S("a2", "al", 1), S("a3", "al", 1)];

#[test]
fn an_album_kept_gapless_is_heard_whole_with_automix_or_a_crossfade_on() {
    let _turn = crate::core_turn();
    an_album_measured_before_plays_every_sample(true, 0);
    an_album_measured_before_plays_every_sample(false, 6);
    an_album_kept_gapless_as_an_older_change_is_told_last_plays_every_sample();
    an_album_measured_while_it_plays_plays_every_sample();
    a_seek_near_the_end_of_an_album_song_plays_on_into_the_next_whole();
    // From well before a1's mix into a2 is made (at 34 s, the output ten seconds ahead of the ear) to just
    // before it is heard: the mix made and held, or already made into the output.
    for at_ms in [26_000, 32_000, 33_500] {
        keeping_albums_switched_on_while_a_song_plays_joins_it_whole(at_ms);
    }
    a_double_album_is_heard_whole_across_its_discs();
    an_album_tagged_without_some_numbers_plays_every_sample();
    an_album_then_another_is_mixed_only_between_them();
    a_shuffled_album_is_mixed();
    songs_of_an_album_not_played_as_one_are_mixed(Queued::OneByOne);
    songs_of_an_album_not_played_as_one_are_mixed(Queued::Autofill);
}

fn an_album_measured_before_plays_every_sample(auto_mix: bool, crossfade: i32) {
    let rig = Rig::new(&format!("album-before-{auto_mix}"), &ALBUM, auto_mix, crossfade, true, Measured::Before, false);
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    rig.heard_as(&order, 0, 0, &[true, true], 0);
    assert!(!mixed, "nothing mixed");
}

/// "Keep albums gapless" switched on while another change is made on another thread (the equalizer's
/// device sound, say): the older change reached the planner last, which went on planning with albums
/// mixed, and an album played next was mixed song into song, cut where each mix began and each next song
/// taken up where it came in (a194db06).
fn an_album_kept_gapless_as_an_older_change_is_told_last_plays_every_sample() {
    let rig = Rig::new("album-older", &ALBUM, true, 0, false, Measured::Before, false);
    let older = nori_core::settings_store::settings_current().unwrap().transition_prefs();
    rig.set("crossfadeKeepAlbums", "true");
    nori_core::automix::planner::settings_changed(older);
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    rig.heard_as(&order, 0, 0, &[true, true], 0);
    assert!(!mixed, "nothing mixed");
}

fn an_album_measured_while_it_plays_plays_every_sample() {
    let rig = Rig::new("album-while", &ALBUM, true, 0, true, Measured::WhilePlaying, false);
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    for (s, _) in &rig.songs {
        assert!(rig.core.analysis_get(s.0.into()).unwrap().is_some(), "{} was measured", s.0);
    }
    rig.heard_as(&order, 0, 0, &[true, true], 0);
    assert!(!mixed, "nothing mixed");
}

fn a_seek_near_the_end_of_an_album_song_plays_on_into_the_next_whole() {
    let rig = Rig::new("album-seek", &ALBUM, true, 0, true, Measured::Before, false);
    rig.engine.play_at(0, 0);
    assert!(rig.until(20, |r| r.engine.status().position_ms > 2_000));
    // Inside where a mix out of a1 would have started.
    rig.engine.seek(SECS as i64 * 1000 - 6_000);
    assert!(rig.until(10, |r| !r.engine.status().switching && r.engine.status().position_ms >= SECS as i64 * 1000 - 6_000), "{:?}", rig.engine.status());
    let from = rig.card.heard.lock().len() / 2;
    let (_, order) = rig.to_the_end();
    // From a second after the seek was heard to the end: a1's last seconds, then a2 and a3 whole.
    rig.heard_as(&order, from, 0, &[true, true], 0);
}

fn keeping_albums_switched_on_while_a_song_plays_joins_it_whole(at_ms: i64) {
    let rig = Rig::new(&format!("album-switched-{at_ms}"), &ALBUM, true, 0, false, Measured::Before, false);
    rig.engine.play_at(0, 0);
    // a1's mix into a2 is planned, and made or not yet; then the album is kept gapless after all.
    assert!(rig.until(40, |r| r.engine.status().position_ms >= at_ms), "{:?}", rig.engine.status());
    assert!(!rig.engine.status().mixing, "switched at {} ms, before the mix is heard", rig.engine.status().position_ms);
    assert!(nori_core::automix::planner::transition_note("a1").is_some_and(|n| n.kind != "Gapless"), "a1 is planned as a mix first: {:?}", nori_core::automix::planner::transition_note("a1"));
    rig.set("crossfadeKeepAlbums", "true");
    let (mixed, order) = rig.to_the_end();
    rig.heard_as(&order, 0, 0, &[true, true], 1);
    assert!(!mixed, "nothing mixed");
}

/// A double album goes on from the last track of its first disc to the first of its second gaplessly too.
fn a_double_album_is_heard_whole_across_its_discs() {
    let songs = [S("d1", "dl", 1), S("d2", "dl", 1), S("d3", "dl", 2)];
    let rig = Rig::new("album-discs", &songs, true, 0, true, Measured::Before, false);
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    rig.heard_as(&order, 0, 0, &[true, true], 0);
    assert!(!mixed, "nothing mixed");
}

fn an_album_then_another_is_mixed_only_between_them() {
    let songs = [S("m1", "al", 1), S("m2", "al", 1), S("n1", "bl", 1), S("n2", "bl", 1)];
    let rig = Rig::new("album-two", &songs, true, 0, true, Measured::Before, false);
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    assert!(mixed, "the albums are mixed into each other");
    assert_ne!(nori_core::automix::planner::transition_note("m2").map(|n| n.kind), Some("Gapless".into()));
    rig.heard_as(&order, 0, 0, &[true, false, true], 0);
}

/// Two songs of one album, in order, that were not queued as the album (queued by hand one at a time, or
/// brought by autofill) mix like any others: Scar Tissue, then Californication, each queued on its own.
fn songs_of_an_album_not_played_as_one_are_mixed(how: Queued) {
    let rig = Rig::tagged("album-queued", &ALBUM, true, 0, true, Measured::Before, false, &Tags { queued: how, ..Tags::default() });
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    assert!(mixed, "mixed song into song");
    for id in &order[..order.len() - 1] {
        let note = nori_core::automix::planner::transition_note(id);
        assert!(note.as_ref().is_some_and(|n| n.kind != "Gapless"), "{id} mixes into the next: {note:?}");
    }
    rig.heard_as(&order, 0, 0, &[false, false], 0);
}

fn a_shuffled_album_is_mixed() {
    let rig = Rig::new("album-shuffled", &ALBUM, true, 0, true, Measured::Before, true);
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    assert!(mixed, "a shuffled album is mixed");
    for id in &order[..order.len() - 1] {
        let note = nori_core::automix::planner::transition_note(id);
        assert!(note.as_ref().is_some_and(|n| n.kind != "Gapless"), "{id} mixes into the next: {note:?}");
    }
}


/// An album whose numbers leave songs out or count otherwise than by one, as real libraries do, played in
/// the order the queue has it: with AutoMix on and albums kept gapless it is heard whole, each next song
/// from its first sample. Read as out of order, each song was mixed into the next, the next coming in
/// part-way where the mix entered it.
fn an_album_tagged_without_some_numbers_plays_every_sample() {
    fn no_tracks(v: &mut Vec<Song>) {
        for s in v.iter_mut() {
            s.track = 0;
        }
    }
    fn one_track_missing(v: &mut Vec<Song>) {
        v[1].track = 0;
    }
    fn one_disc_missing(v: &mut Vec<Song>) {
        v[1].disc_number = 0;
    }
    fn numbered_on(v: &mut Vec<Song>) {
        v[2].disc_number = 2;
    }
    fn gap(v: &mut Vec<Song>) {
        v[2].track = 4;
    }
    // In the order Navidrome lists an album whose track 2 has no disc number: that song first.
    fn listed_first(v: &mut Vec<Song>) {
        (v[0].track, v[0].disc_number, v[1].track, v[2].track) = (2, 0, 1, 3);
    }
    let stories: [(&str, Tags); 7] = [
        ("listed-first", Tags { tag: listed_first, ..Tags::default() }),
        // 1, 2, 4: a file the library does not have, or a track taken out of the queue.
        ("gap", Tags { tag: gap, ..Tags::default() }),
        ("no-tracks", Tags { tag: no_tracks, ..Tags::default() }),
        ("one-track", Tags { tag: one_track_missing, ..Tags::default() }),
        ("one-disc", Tags { tag: one_disc_missing, ..Tags::default() }),
        ("numbered-on", Tags { tag: numbered_on, ..Tags::default() }),
        // Songs ending in near silence: a mix into the next one is not heard as a mix at all, only as the
        // next song starting part-way, which is how it was reported.
        ("quiet-ends", Tags { tag: no_tracks, quiet_ends: true, ..Tags::default() }),
    ];
    for (name, tags) in stories {
        let rig = Rig::tagged(&format!("album-tags-{name}"), &ALBUM, true, 0, true, Measured::Before, false, &tags);
        rig.engine.play_at(0, 0);
        let (mixed, order) = rig.to_the_end();
        rig.heard_as(&order, 0, 0, &[true, true], 0);
        assert!(!mixed, "{name}: nothing mixed");
    }
    // Registered again by another reading of the library that has no track numbers: the store keeps the
    // last word, and the album is still in order.
    let rig = Rig::tagged("album-tags-again", &ALBUM, true, 0, true, Measured::Before, false, &Tags { again: Some(no_tracks), ..Tags::default() });
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    rig.heard_as(&order, 0, 0, &[true, true], 0);
    assert!(!mixed, "registered again: nothing mixed");
}
