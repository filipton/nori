//! Sing: the vocals' level changes where the listener moves it, each song by its own mask.

use std::sync::Arc;

use nori_player::sim::Player;
use nori_player::sing::{bands, VocalMask, MODEL_HOP, MODEL_RATE};

use crate::common::*;

/// A whole song's mask of all vocals.
fn all_vocals(secs: f64) -> Arc<VocalMask> {
    let fps = (MODEL_RATE / MODEL_HOP as f64) as f32;
    Arc::new(VocalMask::new(fps, vec![255; (secs * fps as f64) as usize * bands()]))
}

/// The heard level over `from..to` seconds of what played.
fn level(p: &Player, from: f64, to: f64) -> f64 {
    let heard = left(&p.sink.heard_samples());
    rms(&heard[frames(from)..frames(to).min(heard.len())])
}

#[test]
fn level_moved_mid_song_is_heard_there() {
    let tone = sine(440.0, 0.3, 8.0);
    let mut p = Player::new(vec![track("a", &tone)]);
    p.app.masks.insert("a".into(), all_vocals(8.0));
    p.set_sing(Some(1.0));
    p.play_from(0);
    p.run_for(2_000);
    p.set_sing(Some(0.0));
    p.run_for(2_000);
    p.set_sing(Some(0.5));
    assert!(p.run_to_end(10_000));
    assert!(p.sink.gaps.is_empty(), "the audio kept flowing");
    assert_eq!(p.sink.heard_samples().len(), tone.len(), "every frame played once");
    let full = 0.3 / 2f64.sqrt();
    assert!((level(&p, 0.5, 1.9) / full - 1.0).abs() < 0.02, "as recorded before the change");
    assert!(level(&p, 2.2, 3.9) < full * 0.01, "the vocals gone right after it, not a buffer later");
    assert!((level(&p, 4.3, 7.5) / full - 0.5).abs() < 0.02, "half as loud after the next");
}

#[test]
fn mask_made_mid_song_is_heard_and_each_song_keeps_its_own() {
    let tone = sine(440.0, 0.3, 5.0);
    let mut p = Player::new(vec![track("a", &tone), track("b", &tone), track("c", &tone)]);
    p.app.masks.insert("b".into(), all_vocals(5.0));
    p.set_sing(Some(0.0));
    p.play_from(0);
    p.run_for(2_000);
    // The first song's mask comes while it plays.
    p.app.masks.insert("a".into(), all_vocals(5.0));
    p.app.masks_made = true;
    assert!(p.run_to_end(30_000));
    assert!(p.sink.gaps.is_empty());
    let full = 0.3 / 2f64.sqrt();
    assert!((level(&p, 0.5, 1.9) / full - 1.0).abs() < 0.02, "no mask yet: as recorded");
    assert!(level(&p, 2.2, 4.8) < full * 0.01, "its mask, from where it came");
    assert!(level(&p, 5.2, 9.8) < full * 0.01, "the next song by its own mask");
    assert!((level(&p, 10.2, 14.8) / full - 1.0).abs() < 0.02, "a song without one plays unchanged");
}

#[test]
fn turned_off_mid_song_leaves_the_chain() {
    let tone = sine(440.0, 0.3, 6.0);
    let mut p = Player::new(vec![track("a", &tone)]);
    p.app.masks.insert("a".into(), all_vocals(6.0));
    p.set_sing(Some(0.0));
    p.play_from(0);
    p.run_for(2_000);
    p.set_sing(None);
    assert!(!p.sink.processing(), "the masker is out at once, not at the next seek");
    assert!(p.run_to_end(10_000));
    assert!(p.sink.gaps.is_empty(), "the audio kept flowing");
    assert_eq!(p.sink.heard_samples().len(), tone.len(), "every frame played once");
    let full = 0.3 / 2f64.sqrt();
    assert!(level(&p, 0.5, 1.9) < full * 0.01, "masked before");
    assert!((level(&p, 2.2, 5.5) / full - 1.0).abs() < 0.02, "as recorded after");
}
