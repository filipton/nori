//! Transition settings changed while playing apply at the next boundary, and a play right after a
//! change uses it: the engine over the core's queue, settings and planner. The core is per process, so
//! the cases run in one test and take `core_turn` (as album.rs does).
use crate::common;

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_core::client::{Client, NetProfile};
use nori_core::settings_store::{edit_by_name, APPLY_AUDIO, REPLAN};
use nori_core::{Core, ServerConfig, Song};
use nori_engine::core::{settings, CoreApp, CoreLibrary, CoreOrder, CoreQueue};
use nori_engine::{Body, ByteSource, Config, Engine, Store};

use common::NoApi;

/// `secs` of a quiet tone as a WAV file, a different pitch per seed.
fn tone_wav(secs: usize, seed: u32) -> Vec<u8> {
    common::wav(44_100, &common::sine(44_100, 220.0 + seed as f64 * 30.0, secs as f64, 0.2 * 32767.0))
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

const SECS: usize = 40;

struct Rig {
    engine: Engine,
    time: Stepper<Pull>,
    _dir: nori_testdir::TempDir,
}

impl Rig {
    /// An album of `ids` queued in order, AutoMix on, "keep albums gapless" as `keep`; not playing.
    fn new(name: &str, ids: &[&str], keep: bool) -> Rig {
        let dir = nori_testdir::TempDir::new(name);
        let core = Core::new(dir.join("nori.db").to_string_lossy().into_owned(), "test".into()).unwrap();
        core.configure(ServerConfig { url: "http://music.test".into(), user: "u".into(), password: "p".into(), api_key: None, legacy_auth: false }).unwrap();
        let client = Client::new(core.clone(), Arc::new(NoApi));
        client.set_profile(NetProfile { url: "http://music.test".into(), ..Default::default() });
        let mut prefs = nori_core::settings_store::settings_open(dir.join("app.db").to_string_lossy().into_owned()).unwrap();
        (prefs.auto_mix, prefs.crossfade_keep_albums, prefs.auto_mix_max_s) = (true, keep, 8);
        nori_core::settings_store::settings_put(prefs.clone());
        let store = Store::open(dir.join("music"), 256 << 20, Box::new(CoreOrder)).unwrap();
        let songs: Vec<Song> = ids
            .iter()
            .enumerate()
            .map(|(k, id)| Song {
                id: id.to_string(),
                title: id.to_string(),
                album_id: Some("al".into()),
                track: k as u32 + 1,
                disc_number: 1,
                duration: SECS as u32,
                suffix: "wav".into(),
                ..Default::default()
            })
            .collect();
        let net = Arc::new(Net(ids.iter().enumerate().map(|(k, id)| (id.to_string(), Arc::new(tone_wav(SECS, k as u32)))).collect()));
        core.session.register(songs);
        // Played from the album's page.
        core.session.set(ids.iter().map(|s| s.to_string()).collect(), Some(0), false, Some(nori_core::PageOrigin::new(nori_core::OriginKind::Album, "al")));
        let library = CoreLibrary { client, bytes: net, metered: false, store: Some(store) };
        let card = Card::new();
        let clock = Virtual::default();
        let engine = Engine::start_on(library, CoreApp::new(core.session.clone()), CoreQueue(core.session.clone()), Box::new(card.clone()), None, Config { memory_mb: 128, settings: settings(&prefs, 0.0), ..Config::default() }, clock.clone(), |_| {});
        engine.queue_changed();
        Rig { engine, time: Stepper::new(clock, card.pull.clone()), _dir: dir }
    }

    fn until(&self, secs: u64, mut done: impl FnMut(&Rig) -> bool) -> bool {
        self.time.until(Duration::from_secs(secs), || done(self))
    }

    /// Sets a setting by name and relays its effects to the engine, as the clients do.
    fn set(&self, name: &str, value: &str) {
        let effect = self.set_only(name, value);
        self.relay(effect);
    }

    /// Sets a setting by name without telling the engine; returns its effects.
    fn set_only(&self, name: &str, value: &str) -> u32 {
        let effect = edit_by_name(name, value).unwrap_or_else(|| panic!("{name} is a setting")).effect;
        assert_ne!(effect & REPLAN, 0, "{name} asks for the transition to be planned again");
        effect
    }

    /// Relays a change's effects to the engine, as PlaybackService does.
    fn relay(&self, effect: u32) {
        if effect & APPLY_AUDIO != 0 {
            self.engine.set_settings(settings(&nori_core::settings_store::settings_current().unwrap(), 0.0));
        }
        if effect & REPLAN != 0 {
            self.engine.replan();
        }
    }

    /// Whether a mix is heard before playback is 10 s into `index`.
    fn mixes_into(&self, index: usize) -> bool {
        let mut mixed = false;
        let came = self.until(SECS as u64 * 2, |r| {
            let s = r.engine.status();
            mixed |= s.mixing;
            s.index == Some(index) && s.position_ms > 10_000
        });
        assert!(came, "song {index} is heard: {:?}", self.engine.status());
        mixed
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

#[test]
fn transition_settings_replan_at_once() {
    let _turn = crate::core_turn();
    keeping_albums_gapless_switched_off_while_an_album_plays_mixes_its_next_boundary();
    switched_off_near_the_end_with_the_ending_made_gapless_it_still_mixes();
    every_transition_setting_changed_while_playing_is_planned_with();
    a_play_right_after_a_change_is_planned_with_it();
    automix_and_mixing_albums_switched_on_right_before_an_album_is_played_mix_it();
}

fn keeping_albums_gapless_switched_off_while_an_album_plays_mixes_its_next_boundary() {
    let rig = Rig::new("replan-keep", &["a1", "a2", "a3"], true);
    rig.engine.play_at(0, 0);
    assert!(!rig.mixes_into(1), "kept gapless while the setting says so");
    // Early in the second song, before its ending is made.
    rig.set("crossfadeKeepAlbums", "false");
    assert!(rig.mixes_into(2), "mixed into the third song once the album is no longer kept gapless");
    let note = nori_core::automix::planner::transition_note("a2").expect("a2's ending was planned");
    assert_ne!(note.kind, "Gapless", "{note:?}");
}

fn switched_off_near_the_end_with_the_ending_made_gapless_it_still_mixes() {
    let rig = Rig::new("replan-late", &["d1", "d2"], true);
    rig.engine.play_at(0, 0);
    assert!(rig.until(20, |r| r.engine.status().index == Some(0) && r.engine.status().position_ms > 1_000));
    rig.engine.seek(SECS as i64 * 1000 - 15_000);
    // The ending is already made gapless.
    // Looked at: the status is otherwise as old as the last burst.
    assert!(rig.until(20, |r| {
        r.engine.look();
        r.engine.status().position_ms > SECS as i64 * 1000 - 13_000
    }), "{:?}", rig.engine.status());
    rig.set("crossfadeKeepAlbums", "false");
    assert!(rig.mixes_into(1), "the ending is made again as a mix");
}

fn every_transition_setting_changed_while_playing_is_planned_with() {
    let rig = Rig::new("replan-each", &["b1", "b2", "b3", "b4", "b5"], false);
    rig.engine.play_at(0, 0);
    assert!(rig.until(20, |r| r.engine.status().index == Some(0) && r.engine.status().position_ms > 2_000));
    // Each early in a song: the plan out of it uses the new value.
    let cases: [(&str, &str, fn(&nori_core::automix::planner::TransitionNote) -> bool); 3] = [
        ("autoMixMaxS", "4", |n| n.duration_ms > 0 && n.duration_ms <= 4_000),
        ("crossfadeKeepAlbums", "true", |n| n.kind == "Gapless"),
        ("crossfadeKeepAlbums", "false", |n| n.kind != "Gapless"),
    ];
    for (k, (name, value, ok)) in cases.into_iter().enumerate() {
        rig.set(name, value);
        let id = format!("b{}", k + 1);
        let _ = rig.mixes_into(k + 1);
        let note = nori_core::automix::planner::transition_note(&id).unwrap_or_else(|| panic!("{id}'s ending was planned"));
        assert!(ok(&note), "{name}={value}: {note:?}");
    }
}

fn a_play_right_after_a_change_is_planned_with_it() {
    let rig = Rig::new("replan-play", &["c1", "c2"], true);
    rig.set("crossfadeKeepAlbums", "false");
    rig.engine.play_at(0, 0);
    assert!(rig.mixes_into(1), "the play right after the change mixes");
}

/// smoke.sh's AutoMix check: AutoMix and album mixing switched on right before an album is played; the
/// engine hears of the settings only after the play.
fn automix_and_mixing_albums_switched_on_right_before_an_album_is_played_mix_it() {
    let rig = Rig::new("replan-smoke", &["e1", "e2", "e3"], true);
    let mut prefs = nori_core::settings_store::settings_current().unwrap();
    prefs.auto_mix = false;
    nori_core::settings_store::settings_put(prefs.clone());
    rig.engine.set_settings(settings(&prefs, 0.0));
    rig.engine.play_at(2, 0);
    assert!(rig.until(20, |r| r.engine.status().index == Some(2) && r.engine.status().position_ms > 2_000));
    let a = rig.set_only("crossfadeKeepAlbums", "false");
    let b = rig.set_only("autoMix", "true");
    rig.engine.play_at(0, 0);
    rig.relay(a);
    rig.relay(b);
    assert!(rig.until(20, |r| r.engine.status().index == Some(0) && r.engine.status().position_ms > 1_000));
    rig.engine.seek(SECS as i64 * 1000 - 15_000);
    assert!(rig.mixes_into(1), "the album's first song mixes into its second");
}
