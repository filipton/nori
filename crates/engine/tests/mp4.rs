//! An AAC song in MP4 keeps its exact length: encoder delay and padding from the edit list are cut as
//! media3 cuts them. Needs ffmpeg; passes trivially without it.

use std::path::{Path, PathBuf};
use std::process::Command;

use nori_engine::demux::Demuxed;
use nori_player::pcm::Encoding;
use nori_player::pipeline::Reading;

const RATE: usize = 44_100;
const FRAMES: usize = RATE * 3 + 317;

fn ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn run(args: &[&str]) {
    let ok = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).status().is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg {args:?}");
}

/// A tone of [`FRAMES`] encoded to AAC in MP4, and ffmpeg's decode of it (which honours the edit list).
fn made(dir: &Path) -> (PathBuf, Vec<i16>) {
    let m4a = dir.join("tone.m4a");
    let raw = dir.join("tone.raw");
    let len = format!("{FRAMES}");
    run(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100", "-af", &format!("atrim=end_sample={len}"), "-ac", "2", "-c:a", "aac", "-b:a", "160k", m4a.to_str().unwrap()]);
    run(&["-i", m4a.to_str().unwrap(), "-f", "s16le", "-acodec", "pcm_s16le", raw.to_str().unwrap()]);
    let bytes = std::fs::read(&raw).unwrap();
    (m4a, bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

fn decode(path: &Path, from_ms: i64) -> Vec<i16> {
    let file = std::fs::File::open(path).unwrap();
    let mut d = Demuxed::open(Box::new(file), Some("m4a"), from_ms, None, Encoding::Pcm16).unwrap();
    assert!(d.ready());
    let mut out = Vec::new();
    while d.fill() {
        out.extend(d.buffer().chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
    }
    out
}

#[test]
fn mp4_aac_keeps_exact_length() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: the MP4 gapless test has nothing to test with");
        return;
    }
    let dir = nori_testdir::TempDir::new("mp4");
    let (m4a, reference) = made(&dir);
    assert_eq!(reference.len(), FRAMES * 2, "ffmpeg cuts to the edit list");
    let ours = decode(&m4a, 0);
    assert_eq!(ours.len(), FRAMES * 2, "not a frame of priming or padding left");
    // Two decoders differ by rounding, not by a block of samples.
    let worst = ours.iter().zip(&reference).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(worst <= 4, "lined up with ffmpeg's own decode: {worst}");
    // A seek lands on the same samples as playing from the start.
    let from = decode(&m4a, 1_000);
    assert_eq!(from.len(), (FRAMES - RATE) * 2);
    let worst = from.iter().zip(&ours[RATE * 2..]).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(worst <= 4, "a seek into an MP4 lands where the song's time says: {worst}");
}
