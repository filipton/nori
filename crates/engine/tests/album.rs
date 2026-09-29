//! With AutoMix (or a crossfade) and "keep albums gapless" on, an album in order plays sample-exact
//! gapless however its plans came about; other joins (another album, shuffle) still mix. Runs over the
//! core, one case after another under `core_turn`.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_core::client::{Client, NetProfile};
use nori_core::settings_store::{edit_by_name, APPLY_AUDIO, REPLAN};
use nori_core::{Core, ServerConfig, Song};
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue, Measurer};
use nori_engine::{Body, ByteSource, Config, Engine, State, Store};

use crate::common;

use common::NoApi;

const RATE: usize = 44_100;
const SECS: usize = 40;

/// A song: a 120 bpm beat over its own tone (so mixes are beat-matched) plus non-repeating noise (so
/// any stretch identifies its song and place).
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
    common::wav(RATE as u32, samples)
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

/// A song: id, album and disc; the track is the next on that disc.
#[derive(Clone, Copy)]
struct S(&'static str, &'static str, u32);

/// When the songs are measured.
#[derive(Clone, Copy, PartialEq)]
enum Measured {
    /// Before playing.
    Before,
    /// By the measurer during playback: plans are remade.
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
    /// `songs` queued (shuffled if `shuffle`) and cached, AutoMix on (or a `crossfade`), albums kept
    /// gapless as `keep`.
    fn new(name: &str, songs: &[S], auto_mix: bool, crossfade: i32, keep: bool, measured: Measured, shuffle: bool) -> Rig {
        Rig::tagged(name, songs, auto_mix, crossfade, keep, measured, shuffle, &Tags::default())
    }

    /// [`Rig::new`] with `tags`.
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
            // Registered again with other tags.
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

    /// Blocks until background measuring is done (on a device it finishes long before the boundary).
    fn settle(&self) {
        let Some(m) = &self.measurer else { return };
        let until = Instant::now() + Duration::from_secs(120);
        while self.store.fetching_ahead() || nori_engine::core::measuring_as_they_come() || m.busy() {
            assert!(Instant::now() < until, "the measuring ends");
            std::thread::park_timeout(Duration::from_millis(5));
        }
    }

    /// Sets a setting by name and relays its effects.
    fn set(&self, name: &str, value: &str) {
        let effect = edit_by_name(name, value).unwrap_or_else(|| panic!("{name} is a setting")).effect;
        if effect & APPLY_AUDIO != 0 {
            self.engine.set_settings(settings(&nori_core::settings_store::settings_current().unwrap(), 0.0));
        }
        if effect & REPLAN != 0 {
            self.engine.replan();
        }
    }

    /// Plays to the end; returns whether a mix was heard and the songs in the order heard.
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

    /// Checks what was heard from frame `from_heard` against the songs from `first` in play order;
    /// `joins[k]`: song `first + k` joins the next gaplessly (every sample) or is mixed. Up to `dips`
    /// remake dips are allowed (shorter than [`DIP_MAX`], resuming within [`DIP_SLIP`]).
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
        // Where in song `k` the heard frames from `hf` are.
        let find = |k: usize, hf: usize| (0..len - W).find(|&sf| matches(k, hf, sf));
        // Which song, and where, is heard from `f`.
        let what = |f: usize| {
            (0..order.len())
                .find_map(|o| find(o, f).map(|x| format!("{} at {} ms", order[o].0 .0, x * 1000 / RATE)))
                .unwrap_or_else(|| "none of the songs as they are".into())
        };
        let ms = |f: usize| f * 1000 / RATE;
        // (song, at ms, frames it went on from later than where it was)
        let mut dipped: Vec<(String, usize, i64)> = Vec::new();
        // A second in, past any seek or play dip.
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
                    // A remake dip resumes near here; anything else is a cut.
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
                    // Only silence remains.
                    assert!((hf..frames).all(|f| at(f).abs() < 1e-6), "nothing after the last song");
                }
                sf = 0;
            } else {
                // After the mix, find the incoming song again.
                let after = hf + (len - sf) + 4 * RATE;
                let next = order[k + 1].0 .0;
                let found = find(k + 1, after).unwrap_or_else(|| panic!("{next} is heard alone after the mix out of {id}: {}", what(after)));
                (hf, sf) = (after, found);
            }
        }
        assert!(dipped.len() <= dips, "{dips} dips at most, (song, at ms, frames later than it was): {dipped:?}");
    }
}

/// How far a remake may resume from where it was, frames (nori_player's `OPENED_EARLY_MS`).
const DIP_SLIP: usize = RATE * 40 / 1000;

/// The longest remake dip, frames (30 ms down and up, plus overlap).
const DIP_MAX: usize = RATE * 15 / 100;

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

/// Tagging beyond the defaults (as an imperfect library does), and how songs were queued.
struct Tags {
    tag: fn(&mut [Song]),
    queued: Queued,
    /// Re-registers the songs with these tags.
    again: Option<fn(&mut [Song])>,
    /// Songs fade out over their last four seconds.
    quiet_ends: bool,
}

impl Default for Tags {
    fn default() -> Self {
        Tags { tag: |_| {}, again: None, quiet_ends: false, queued: Queued::AsAlbums }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Queued {
    /// Each album played from its page.
    AsAlbums,
    /// The first song played alone, the others added one by one.
    OneByOne,
    /// The first song played alone, the rest by autofill.
    Autofill,
}

/// Queues `songs` (shuffled if `shuffle`) as `how` says.
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
            playlist_set(ids(runs[0]), Some(0), shuffle, album(&runs[0][0]));
            for r in &runs[1..] {
                playlist_take(len(), ids(r), vec![Hand::No; r.len()], album(&r[0]));
            }
        }
        Queued::OneByOne => {
            playlist_set(ids(&songs[..1]), Some(0), shuffle, None);
            for s in &songs[1..] {
                playlist_take(len(), ids(std::slice::from_ref(s)), vec![Hand::Last], None);
            }
        }
        Queued::Autofill => {
            playlist_set(ids(&songs[..1]), Some(0), shuffle, None);
            playlist_take(len(), ids(&songs[1..]), vec![Hand::No; songs.len() - 1], None);
        }
    }
    assert_eq!(with(|p| p.ids().to_vec()), ids(songs), "queued in order");
}

/// Fades `pcm`'s last four seconds to near silence.
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
fn album_kept_gapless_with_transitions_on() {
    let _turn = crate::core_turn();
    an_album_measured_before_plays_every_sample(true, 0);
    an_album_measured_before_plays_every_sample(false, 6);
    an_album_measured_while_it_plays_plays_every_sample();
    a_seek_near_the_end_of_an_album_song_plays_on_into_the_next_whole();
    // Before a1's mix is made (34 s) up to just before it is heard.
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
    // Inside a1's would-be mix.
    rig.engine.seek(SECS as i64 * 1000 - 6_000);
    assert!(rig.until(10, |r| !r.engine.status().switching && r.engine.status().position_ms >= SECS as i64 * 1000 - 6_000), "{:?}", rig.engine.status());
    let from = rig.card.heard.lock().len() / 2;
    let (_, order) = rig.to_the_end();
    // a1's last seconds, then a2 and a3 whole.
    rig.heard_as(&order, from, 0, &[true, true], 0);
}

fn keeping_albums_switched_on_while_a_song_plays_joins_it_whole(at_ms: i64) {
    let rig = Rig::new(&format!("album-switched-{at_ms}"), &ALBUM, true, 0, false, Measured::Before, false);
    rig.engine.play_at(0, 0);
    // a1 is planned as a mix, then albums are kept gapless.
    assert!(rig.until(40, |r| r.engine.status().position_ms >= at_ms), "{:?}", rig.engine.status());
    assert!(!rig.engine.status().mixing, "switched at {} ms, before the mix is heard", rig.engine.status().position_ms);
    assert!(nori_core::automix::planner::transition_note("a1").is_some_and(|n| n.kind != "Gapless"), "a1 is planned as a mix first: {:?}", nori_core::automix::planner::transition_note("a1"));
    rig.set("crossfadeKeepAlbums", "true");
    let (mixed, order) = rig.to_the_end();
    rig.heard_as(&order, 0, 0, &[true, true], 1);
    assert!(!mixed, "nothing mixed");
}

/// Gapless across the discs of a double album.
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

/// Songs of one album not queued as the album (by hand, or autofill) mix like any others.
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

/// An album with gaps or odd numbering in its tags still plays gapless in queue order. Regression: read
/// as out of order, each song was mixed into the next.
fn an_album_tagged_without_some_numbers_plays_every_sample() {
    fn no_tracks(v: &mut [Song]) {
        for s in v.iter_mut() {
            s.track = 0;
        }
    }
    fn one_track_missing(v: &mut [Song]) {
        v[1].track = 0;
    }
    fn one_disc_missing(v: &mut [Song]) {
        v[1].disc_number = 0;
    }
    fn numbered_on(v: &mut [Song]) {
        v[2].disc_number = 2;
    }
    fn gap(v: &mut [Song]) {
        v[2].track = 4;
    }
    // Navidrome lists a track without a disc number first.
    fn listed_first(v: &mut [Song]) {
        (v[0].track, v[0].disc_number, v[1].track, v[2].track) = (2, 0, 1, 3);
    }
    let stories: [(&str, Tags); 7] = [
        ("listed-first", Tags { tag: listed_first, ..Tags::default() }),
        // 1, 2, 4: a missing track.
        ("gap", Tags { tag: gap, ..Tags::default() }),
        ("no-tracks", Tags { tag: no_tracks, ..Tags::default() }),
        ("one-track", Tags { tag: one_track_missing, ..Tags::default() }),
        ("one-disc", Tags { tag: one_disc_missing, ..Tags::default() }),
        ("numbered-on", Tags { tag: numbered_on, ..Tags::default() }),
        // Near-silent endings make a wrong mix sound like a late start, as reported.
        ("quiet-ends", Tags { tag: no_tracks, quiet_ends: true, ..Tags::default() }),
    ];
    for (name, tags) in stories {
        let rig = Rig::tagged(&format!("album-tags-{name}"), &ALBUM, true, 0, true, Measured::Before, false, &tags);
        rig.engine.play_at(0, 0);
        let (mixed, order) = rig.to_the_end();
        rig.heard_as(&order, 0, 0, &[true, true], 0);
        assert!(!mixed, "{name}: nothing mixed");
    }
    // Re-registered without track numbers: still in order.
    let rig = Rig::tagged("album-tags-again", &ALBUM, true, 0, true, Measured::Before, false, &Tags { again: Some(no_tracks), ..Tags::default() });
    rig.engine.play_at(0, 0);
    let (mixed, order) = rig.to_the_end();
    rig.heard_as(&order, 0, 0, &[true, true], 0);
    assert!(!mixed, "registered again: nothing mixed");
}
