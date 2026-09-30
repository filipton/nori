//! AAC songs keep their exact length: in MP4 the encoder delay and padding from the edit list are cut
//! as media3 cuts them, and HE-AAC through a platform decoder that holds packets back ends whole. Needs
//! ffmpeg; passes trivially without it.

use std::path::{Path, PathBuf};
use std::process::Command;

use nori_engine::demux::Demuxed;
use nori_player::decode::{lend_platform_aac, Codec, Decoder, Fault, PlatformDecoder};
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
    (m4a, bytes.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect())
}

fn decode(path: &Path, from_ms: i64) -> Vec<i16> {
    let file = std::fs::File::open(path).unwrap();
    let hint = path.extension().unwrap().to_str().unwrap();
    let mut d = Demuxed::open(Box::new(file), Some(hint), from_ms, None, Encoding::Pcm16).unwrap();
    assert!(d.ready());
    let mut out = Vec::new();
    while d.fill() {
        out.extend(d.buffer().as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)));
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

/// A platform HE-AAC decoder that hands each packet's samples out with the next packet, as some phones'
/// MediaCodec does (here the AAC core decodes).
struct Lagging {
    core: Decoder,
    held: Vec<f32>,
    shape: (usize, u32),
}

impl PlatformDecoder for Lagging {
    fn decode(&mut self, unit: &[u8], out: &mut Vec<f32>) -> Result<(usize, u32), Fault> {
        out.append(&mut self.held);
        let l = self.core.decode_lent(unit)?;
        self.shape = (l.channels, l.rate);
        self.held.extend_from_slice(l.samples);
        Ok(self.shape)
    }

    fn drain(&mut self, out: &mut Vec<f32>) -> Result<(usize, u32), Fault> {
        out.append(&mut self.held);
        Ok(self.shape)
    }

    fn reset(&mut self) {
        self.held.clear();
        self.core.reset(false);
    }
}

#[test]
fn he_aac_on_a_lagging_decoder_ends_whole() {
    if !ffmpeg() {
        eprintln!("ffmpeg is not installed: the HE-AAC test has nothing to test with");
        return;
    }
    // AAC at 22.05 kHz is taken for HE-AAC with implicit signalling: the platform decodes it.
    lend_platform_aac(|s| Some(Box::new(Lagging { core: Decoder::new(Codec::Aac, s.rate, s.channels, s.config, false).ok()?, held: Vec::new(), shape: (s.channels, s.rate) })));
    let dir = nori_testdir::TempDir::new("heaac");
    let (aac, raw) = (dir.join("tone.aac"), dir.join("tone.raw"));
    run(&["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=22050:duration=3", "-ac", "2", "-c:a", "aac", "-b:a", "64k", "-f", "adts", aac.to_str().unwrap()]);
    run(&["-i", aac.to_str().unwrap(), "-f", "s16le", "-acodec", "pcm_s16le", raw.to_str().unwrap()]);
    let reference: Vec<i16> = std::fs::read(&raw).unwrap().as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect();
    let ours = decode(&aac, 0);
    assert_eq!(ours.len(), reference.len(), "every packet heard, the last ones too");
    let worst = ours.iter().zip(&reference).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(worst <= 4, "the same samples: {worst}");
}
