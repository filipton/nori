//! Sing over a core: a vocal mask that comes while its song plays turns the vocals down from there, with
//! the measurer driven apart from the engine, as on Android.
use crate::common;

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use common::card::{Card, Pull};
use common::{Stepper, Virtual};
use nori_core::Song;
use nori_engine::core::{settings, Analyses, CoreApp, CoreLibrary, CoreQueue, Measurer, Shelf, Whole};
use nori_engine::{Body, ByteSource, Config, Engine, Store};
use nori_player::sing::{bands, VocalMask, MODEL_HOP, MODEL_RATE};

const SECS: f64 = 12.0;
const PEAK: f64 = 0.3;

struct Net(Arc<Vec<u8>>);

impl ByteSource for Net {
    fn open(&self, _url: &str, from: u64) -> Result<Body, nori_engine::OpenError> {
        let mut c = Cursor::new(self.0.as_ref().clone());
        c.set_position(from);
        Ok(Body { start: from, len: Some(self.0.len() as u64), reader: Box::new(c) })
    }
}

/// Nothing whole: the masks come from the disk.
struct Nowhere;

impl Shelf for Nowhere {
    fn whole(&self, _id: &str) -> Option<Whole> {
        None
    }
}

/// The left channel's RMS over `from..to` seconds of what the card heard.
fn level(card: &Card, from: f64, to: f64) -> f64 {
    let heard = card.heard.lock();
    let left: Vec<f64> = heard.as_chunks::<2>().0.iter().map(|c| c[0] as f64).collect();
    let w = &left[(from * 44_100.0) as usize..((to * 44_100.0) as usize).min(left.len())];
    (w.iter().map(|v| v * v).sum::<f64>() / w.len() as f64).sqrt()
}

#[test]
fn mask_made_mid_song_turns_its_vocals_down() {
    let dir = nori_testdir::TempDir::new("sing-mid-song");
    let (core, client) = common::own_core(&dir, |p| (p.sing, p.sing_vocal_level) = (true, 0.0));
    let prefs = core.session.settings.current().unwrap();
    let song = common::wav(44_100, &common::sine(44_100, 440.0, SECS, PEAK * 32767.0));
    core.session.register(vec![Song { id: "s".into(), title: "s".into(), duration: SECS as u32, suffix: "wav".into(), ..Default::default() }]);
    core.session.set(vec!["s".into()], Some(0), false, None);
    let analyses = Analyses::of(client.clone());
    let measurer = Measurer::on_shelf(analyses.clone(), Box::new(Nowhere), None);
    let store = Store::open(dir.join("music"), 64 << 20).unwrap();
    let library = CoreLibrary { client, bytes: Arc::new(Net(Arc::new(song))), store: Some(store), analyses: analyses.clone() };
    let app = CoreApp::new(core.session.clone()).singing(analyses);
    let card = Card::new();
    let clock = Virtual::default();
    let time: Stepper<Pull> = Stepper::new(clock.clone(), card.pull.clone());
    let engine = Engine::start_on(library, app, CoreQueue(core.session.clone()), Box::new(card.clone()), None, Config { memory_mb: 64, settings: settings(&prefs, 0.0), ..Config::default() }, clock, |_| {});
    engine.queue_changed();
    engine.play_at(0, 0);
    time.run(Duration::from_secs(3));

    // The song's mask (all vocals) is made, the platform's measurer finds it and says so (Kotlin's replan).
    let fps = (MODEL_RATE / MODEL_HOP as f64) as f32;
    let mask = VocalMask::new(fps, vec![255; (SECS * fps as f64) as usize * bands()]);
    let name: String = "s".bytes().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(dir.join("sing").join("masks")).unwrap();
    std::fs::write(dir.join("sing").join("masks").join(name + ".mask"), mask.to_bytes()).unwrap();
    measurer.ask(core.session.measure());
    measurer.wait();
    engine.replan();
    time.run(Duration::from_secs(6));
    engine.stop();

    let full = PEAK / 2f64.sqrt();
    assert!((level(&card, 0.5, 2.5) / full - 1.0).abs() < 0.05, "no mask yet: as recorded");
    let after = level(&card, 5.0, 8.5);
    assert!(after < full * 0.1, "its vocals down once the mask came: {:.1} dB", 20.0 * (after / full).log10());
}
