//! Sing: the vocals' level changes where the listener moves it, each song by its own mask.

use std::sync::Arc;

use nori_player::sim::Player;
use nori_player::sing::{bands, Separator, VocalMask, MODEL_BINS, MODEL_HOP, MODEL_RATE};

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

/// Every bin all vocals.
struct AllVocals;

impl Separator for AllVocals {
    fn separate(&self, _mags: Vec<f32>, frames: usize, row: &mut dyn FnMut(usize, &[f32])) -> Result<(), String> {
        let ones = vec![1.0; 2 * MODEL_BINS];
        (0..frames).for_each(|k| row(k, &ones));
        Ok(())
    }
}

/// A player of `secs` of tone whose masks are made as it is read, vocals at 0.
fn live(secs: f64) -> Player {
    let mut p = Player::new(vec![track("a", &sine(440.0, 0.3, secs))]);
    p.app.separator = Some(Box::new(AllVocals));
    p.set_sing(Some(0.0));
    p
}

/// On a device holding everything written, a mask made as the song is read takes the vocals down a second in:
/// only what went ahead of the ear before the first rows plays unmasked, and the music never waits.
#[test]
fn mask_made_as_read_is_heard_a_second_in() {
    let mut p = live(12.0);
    p.sink.track.deep = true;
    p.play_from(0);
    assert!(p.run_to_end(20_000));
    assert!(p.sink.gaps.is_empty(), "the audio kept flowing");
    let full = 0.3 / 2f64.sqrt();
    assert!((level(&p, 0.1, 0.7) / full - 1.0).abs() < 0.02, "the first moments went out before the mask");
    assert!(level(&p, 1.1, 11.5) < full * 0.01, "masked from a second in: {}", level(&p, 1.1, 11.5) / full);
    assert!(p.app.masks["a"].whole(), "the whole song's mask, once read to its end");
}

/// A model that falls behind lets the music play on as it is; once its rows come, what the output can still replace
/// is made again with them.
#[test]
fn music_plays_on_while_the_mask_is_late() {
    let mut p = live(10.0);
    p.app.rows_from_ms = p.now_ms + 4_000;
    p.play_from(0);
    assert!(p.run_to_end(20_000));
    assert!(p.sink.gaps.is_empty(), "the audio kept flowing");
    let full = 0.3 / 2f64.sqrt();
    assert!((level(&p, 0.5, 3.5) / full - 1.0).abs() < 0.02, "unmasked while no rows came");
    assert!(level(&p, 4.3, 9.5) < full * 0.01, "masked once they came");
}

/// A seek past what was masked masks from there within a second, the model given the music before it as context;
/// what was never read is never masked.
#[test]
fn seek_masks_from_there_with_history() {
    let mut p = live(30.0);
    p.sink.track.deep = true;
    p.play_from(0);
    p.run_for(2_000);
    p.seek(20_000);
    p.run_for(4_000);
    assert!(p.sink.gaps.is_empty());
    let full = 0.3 / 2f64.sqrt();
    assert!(level(&p, 3.1, 5.9) < full * 0.01, "masked a second after the seek");
    let mask = &p.app.masks["a"];
    let frame = |s: f64| (s * mask.fps as f64) as usize;
    assert!(mask.has(frame(18.7)) && mask.has(frame(20.0)), "rows from the history read before the seek point");
    assert!(!mask.has(frame(14.0)), "none where nothing was read");
}

/// Sing turned on mid-song reads the song again from the ear, and its vocals go down within a second, though the
/// output had seconds of it made.
#[test]
fn turned_on_mid_song_masks_within_a_second() {
    let mut p = Player::new(vec![track("a", &sine(440.0, 0.3, 10.0))]);
    p.app.separator = Some(Box::new(AllVocals));
    p.sink.track.deep = true;
    p.play_from(0);
    p.run_for(3_000);
    p.set_sing(Some(0.0));
    assert!(p.run_to_end(20_000));
    assert!(p.sink.gaps.is_empty());
    let full = 0.3 / 2f64.sqrt();
    assert!((level(&p, 0.5, 2.9) / full - 1.0).abs() < 0.02, "as recorded before");
    assert!(level(&p, 4.1, 9.5) < full * 0.01, "masked a second after: {}", level(&p, 4.1, 9.5) / full);
}
