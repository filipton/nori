//! Fingerprints of what fixed input sounds like through the player, so a refactor that changes a
//! single sample shows up here. When one of these changes on purpose, the new value goes in with the
//! change that made it, and the statistics beside it say how big the change was.

use nori_player::dsp::{Band, HIGH_SHELF, LOW_SHELF, PEAKING};
use nori_player::sim::{Audio, Player, Sound, Track};

use crate::common::*;

/// Level and peak of what was heard, dB below full scale: what a changed fingerprint changed.
fn shape(s: &[i16]) -> (f64, f64) {
    let x = left(s);
    (db(rms(&x)), db(x.iter().fold(0.0, |m: f64, v| m.max(v.abs()))))
}

#[test]
fn an_evening_through_the_whole_chain_sounds_exactly_as_it_did() {
    // Three songs, six-second crossfades, an equalizer curve pushed into the limiter, played to the end.
    let songs: Vec<Track> = (0..3).map(|k| track(&format!("s{k}"), &music(20.0, 300 + k))).collect();
    let mut p = Player::with_prefs(songs, crossfade(6));
    let bands = vec![
        Band { kind: LOW_SHELF, freq: 120.0, gain_db: 5.0, q: 0.7, channel: 0 },
        Band { kind: PEAKING, freq: 2500.0, gain_db: -3.0, q: 1.2, channel: 0 },
        Band { kind: HIGH_SHELF, freq: 8000.0, gain_db: 4.0, q: 0.7, channel: 0 },
    ];
    p.set_sound(Sound { bands, preamp_db: 6.0, limiter: true, ..Sound::default() });
    p.play_from(0);
    assert!(p.run_to_end(80_000));
    let heard = p.sink.heard_samples();
    // The limiter delays everything by its look-ahead (5 ms, 220 frames), and at the end of the queue
    // silence pushed through it brings the last of the music out, so the whole of it is heard, later.
    assert_eq!(heard.len(), (frames(60.0 - 12.0) + 220) * 2, "three songs less two overlaps, and the look-ahead");
    let (level, peak) = shape(&heard);
    assert!((level + 9.63).abs() < 0.01 && (peak + 1.0).abs() < 0.01, "level {level:.2} dB, peak {peak:.2} dB");
    assert_eq!(fingerprint(&heard), 7_527_512_225_347_426_139, "level {level:.2} dB, peak {peak:.2} dB");
}

#[test]
fn the_decoders_give_the_samples_they_gave() {
    let mp3 = Audio::mp3(&testdata("tone440.mp3")).decode_all();
    let opus = Audio::opus(&testdata("tone440.opus")).decode_all();
    assert_eq!((mp3.len(), opus.len()), (91_102, 97_296));
    // Opus is decoded in floating point, and macOS's maths library rounds a few of its samples
    // differently from glibc's: each has its own fingerprint, held as exactly as the other.
    let opus_print = if cfg!(target_os = "macos") { 9_238_662_167_881_183_963 } else { 5_998_433_017_906_850_831 };
    assert_eq!((fingerprint(&mp3), fingerprint(&opus)), (11_834_309_614_667_511_277, opus_print));
}
