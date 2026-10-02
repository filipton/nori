//! Containers read with symphonia's format readers (MP3, FLAC, Ogg Vorbis/Opus, MP4 AAC/ALAC, WAV),
//! decoded by `nori_player::decode` (the decoder every platform uses). Encoder delay and padding are cut
//! so songs join sample-exactly; seeks are sample-exact.
//!
//! A song still downloading is opened on a thread of its own (opening reads its first, and for some
//! MP4s last, bytes); the engine is woken when it is open and never waits for the network. symphonia
//! allocates one buffer per packet read; decoding allocates nothing.
//!
//! [`Demuxed::load_packets`] reads undecoded packets for audio offload, with the encoder delay and
//! padding as that output takes them and each packet's frame count.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::Thread;

use nori_player::automix::resample::Resampler;
use nori_player::automix::PCM_FLOAT;
use nori_player::decode::{he_aac, Codec, Decoder, MP3_DECODER_DELAY};
use nori_player::pcm::{Encoding, Format};
use nori_player::pipeline::Reading;
use nori_player::queue::PlaybackError;
use parking_lot::Mutex;
use symphonia::core::codecs::audio::well_known::*;
use symphonia::core::codecs::audio::AudioCodecId;
use symphonia::core::errors::{Error as SymphoniaError, SeekErrorKind};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;
use symphonia::core::units::{Time, Timestamp};

use crate::mpeg::Frames;
use crate::panic_words;
use crate::source::Loader;

/// Opus pre-roll after a seek, dropped by the decoder (80 ms, RFC 7845).
const OPUS_PRE_ROLL: i64 = 3840;
/// Frames read before an MP4 seek target for the decoder to warm up: two AAC frames.
const AAC_WARM_UP: i64 = 2048;
/// Times a song is reopened after its length turned out shorter than promised (`Demuxed::start`).
const REOPENS: usize = 3;

/// A WAV file's sample format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pcm {
    U8,
    S16,
    S24,
    S32,
    F32,
}

impl Pcm {
    fn of(codec: AudioCodecId) -> Option<Pcm> {
        Some(match codec {
            CODEC_ID_PCM_U8 => Pcm::U8,
            CODEC_ID_PCM_S16LE => Pcm::S16,
            CODEC_ID_PCM_S24LE => Pcm::S24,
            CODEC_ID_PCM_S32LE => Pcm::S32,
            CODEC_ID_PCM_F32LE => Pcm::F32,
            _ => return None,
        })
    }

    fn width(self) -> usize {
        match self {
            Pcm::U8 => 1,
            Pcm::S16 => 2,
            Pcm::S24 => 3,
            Pcm::S32 | Pcm::F32 => 4,
        }
    }

    /// A stored sample as float.
    fn value(self, b: &[u8]) -> f32 {
        match self {
            Pcm::U8 => (b[0] as f32 - 128.0) / 128.0,
            Pcm::S16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            Pcm::S24 => i32::from_le_bytes([0, b[0], b[1], b[2]]) as f32 / 2_147_483_648.0,
            Pcm::S32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            Pcm::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        }
    }

    /// Appends stored samples to `out` in `to` (16-bit rounded as the decoder rounds, or float).
    fn put(self, stored: &[u8], to: Encoding, out: &mut Vec<u8>) {
        if (self, to) == (Pcm::S16, Encoding::Pcm16) {
            return out.extend_from_slice(stored);
        }
        let at = out.len();
        out.resize(at + stored.len() / self.width() * to.width(), 0);
        let made = out[at..].chunks_exact_mut(to.width());
        for (o, b) in made.zip(stored.chunks_exact(self.width())) {
            match (self, to) {
                (Pcm::U8, Encoding::Pcm16) => o.copy_from_slice(&(((b[0] as i16) - 128) << 8).to_le_bytes()),
                (_, Encoding::Pcm16) => o.copy_from_slice(&rounded(self.value(b)).to_le_bytes()),
                (_, Encoding::Float) => o.copy_from_slice(&self.value(b).to_le_bytes()),
            }
        }
    }
}

/// A float sample as 16-bit, rounded as `Decoder::take_i16` rounds.
fn rounded(v: f32) -> i16 {
    (v * 32768.0).round_ties_even().clamp(-32768.0, 32767.0) as i16
}

/// Appends `samples` to `out` in `to`, in one pass.
fn put(samples: &[f32], to: Encoding, out: &mut Vec<u8>) {
    let at = out.len();
    out.resize(at + samples.len() * to.width(), 0);
    match to {
        Encoding::Pcm16 => out[at..].as_chunks_mut::<2>().0.iter_mut().zip(samples).for_each(|(o, &v)| *o = rounded(v).to_le_bytes()),
        Encoding::Float => out[at..].as_chunks_mut::<4>().0.iter_mut().zip(samples).for_each(|(o, v)| *o = v.to_le_bytes()),
    }
}

enum Inner {
    Coded(Box<Decoder>),
    Pcm(Pcm),
    /// Read as packets, not decoded.
    Raw,
}

/// How a stream is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Play,
    /// Decoded for measuring: HE-AAC's core alone is enough.
    Measure,
    /// Undecoded packets for an output that decodes them.
    Packets,
}

/// How to open a stream.
#[derive(Clone, Copy)]
struct Spec<'a> {
    hint: Option<&'a str>,
    from_ms: i64,
    /// The tagged length, for a container that does not say.
    duration_ms: Option<i64>,
    encoding: Encoding,
    mode: Mode,
    /// Every byte is here: MP4 gapless boxes are read wherever they are.
    whole: bool,
    /// The source's length is the song's; otherwise it is read in order as of unknown length ([`Unsized`]).
    sized: bool,
}

impl<'a> Spec<'a> {
    fn new(hint: Option<&'a str>, from_ms: i64, duration_ms: Option<i64>, encoding: Encoding, mode: Mode) -> Self {
        Spec { hint, from_ms, duration_ms, encoding, mode, whole: true, sized: true }
    }
}

/// A compression an offload output may decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coding {
    Mp3,
    /// AAC Low Complexity.
    Aac,
    Opus,
}

impl Coding {
    pub fn name(self) -> &'static str {
        match self {
            Coding::Mp3 => "MP3",
            Coding::Aac => "AAC-LC",
            Coding::Opus => "Opus",
        }
    }
}

/// A compressed stream, as an offload output is asked about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coded {
    pub coding: Coding,
    pub rate: u32,
    pub channels: usize,
}

/// A song read as packets: their format, and the encoder gap the output should cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodedSong {
    pub coded: Coded,
    /// Bits per second from bytes and length (0 unknown), for sizing a track.
    pub bitrate: u32,
    /// Frames to cut at the start and end, as media3 passes them to an offload AudioTrack (LAME tag, MP4
    /// edit list). Opus pre-skip is in its header.
    pub delay: u32,
    pub padding: u32,
    /// The codec header (Opus's `OpusHead`).
    pub setup: Option<Box<[u8]>>,
    /// First packet's frame in the song (after a seek, the packet it landed in).
    pub from_frame: i64,
}

fn codec_of(id: AudioCodecId) -> Option<Codec> {
    Some(match id {
        CODEC_ID_MP3 => Codec::Mp3,
        CODEC_ID_FLAC => Codec::Flac,
        CODEC_ID_AAC => Codec::Aac,
        CODEC_ID_VORBIS => Codec::Vorbis,
        CODEC_ID_ALAC => Codec::Alac,
        CODEC_ID_OPUS => Codec::Opus,
        _ => return None,
    })
}

/// A song's packets: symphonia's container reader, or `mpeg.rs` for a live MP3 stream.
enum Packets {
    Container(Box<dyn FormatReader + 'static>),
    Mpeg(Frames),
}

impl Packets {
    /// The next packet; None at the end. `WouldBlock` means try again later.
    fn next_packet(&mut self) -> symphonia::core::errors::Result<Option<Packet>> {
        match self {
            Packets::Container(r) => r.next_packet(),
            Packets::Mpeg(f) => f.next_packet(),
        }
    }

    fn container(&mut self) -> Option<&mut Box<dyn FormatReader + 'static>> {
        match self {
            Packets::Container(r) => Some(r),
            Packets::Mpeg(_) => None,
        }
    }
}

/// A read to retry later, not a failure.
fn for_now(e: &SymphoniaError) -> bool {
    matches!(e, SymphoniaError::IoError(e) if e.kind() == io::ErrorKind::WouldBlock)
}

/// A seek past the song's end.
fn past_end(e: &SymphoniaError) -> bool {
    match e {
        SymphoniaError::SeekError(SeekErrorKind::OutOfRange) => true,
        SymphoniaError::IoError(e) => e.kind() == io::ErrorKind::UnexpectedEof,
        _ => false,
    }
}

/// Silent packets (broken, skipped, before a seek target) read in one call before returning empty, so
/// a stream of broken packets never holds the engine's thread.
const IDLE_PACKETS: u32 = 64;

/// One song being read: container, decoder, and the last decoded buffer.
struct Stream {
    reader: Packets,
    track: u32,
    inner: Inner,
    codec: Option<Codec>,
    /// The container states the encoder delay (an MP3's LAME header).
    delay_known: bool,
    format: Format,
    buf: Vec<u8>,
    /// The next frame out, from the song's start; None after a seek until a packet says.
    frame: Option<i64>,
    /// Frames before this are decoded and dropped (after a seek).
    skip_to: i64,
    at_us: i64,
    duration_us: i64,
    ended: bool,
    /// The first buffer was decoded while opening (to learn the true format) and not handed out.
    primed: bool,
    /// An MP4's encoder delay and music end, in decoder frames, from `iTunSMPB` or the edit list
    /// (symphonia reads neither).
    mp4: Option<(i64, i64)>,
    /// Stored bits per sample (0 unknown).
    bits: u32,
    /// The compression's name, for reports.
    compression: &'static str,
    /// Read as packets: their format, and the last packet's frames.
    coded: Option<CodedSong>,
    packet_frames: u64,
    first_packet: bool,
    /// The format is fixed (after the first packet): later audio in another shape is converted.
    settled: bool,
    /// Converter for a live stream whose rate or channels changed (a chained Ogg stream's next song).
    reshape: Option<Reshape>,
    /// Read whole, an Ogg stream without its last page: a copy cut short (by a server at its length
    /// estimate).
    cut_short: bool,
}

/// Converts float audio from one rate and channel count to another.
struct Reshape {
    from: (u32, usize),
    resampler: Resampler,
    input: Vec<u8>,
    output: Vec<u8>,
}

/// Converts `samples` (interleaved float at `from`) into `out` at `to`. Returns the frames made.
fn reshaped(reshape: &mut Option<Reshape>, from: (u32, usize), to: Format, samples: &[f32], out: &mut Vec<u8>) -> usize {
    if reshape.as_ref().is_none_or(|r| r.from != from) {
        let Some(resampler) = Resampler::new(from.0 as i32, from.1 as i32, to.rate as i32, to.channels as i32) else { return 0 };
        *reshape = Some(Reshape { from, resampler, input: Vec::new(), output: Vec::new() });
    }
    let r = reshape.as_mut().expect("made above");
    r.input.clear();
    put(samples, Encoding::Float, &mut r.input);
    let frames = samples.len() / from.1.max(1);
    r.output.resize((frames * to.rate as usize / from.0.max(1) as usize + 4) * to.channels * 4, 0);
    let Some((_, made)) = r.resampler.process(&r.input, PCM_FLOAT, &mut r.output, PCM_FLOAT) else { return 0 };
    let made_floats = r.output[..made].as_chunks::<4>().0;
    match to.encoding {
        Encoding::Float => out.extend_from_slice(&r.output[..made]),
        Encoding::Pcm16 => out.extend(made_floats.iter().flat_map(|b| rounded(f32::from_le_bytes(*b)).to_le_bytes())),
    }
    made / 4 / to.channels
}

impl Stream {
    /// Opens `source` as `spec` says. A panic in symphonia or the decoder fails the song, not the thread.
    fn open(source: Box<dyn MediaSource>, spec: Spec) -> Result<Stream, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Stream::open_unguarded(source, spec))).unwrap_or_else(|p| Err(format!("the song would not open: {}", panic_words(&*p))))
    }

    fn open_unguarded(mut source: Box<dyn MediaSource>, spec: Spec) -> Result<Stream, String> {
        let Spec { hint, from_ms, duration_ms, encoding, mode, whole, sized } = spec;
        let packets = mode == Mode::Packets;
        let gapless = if whole { crate::mp4::gapless(&mut source).ok().flatten() } else { None };
        source.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let source = past_id3(source).map_err(|e| e.to_string())?;
        // Asked once the first bytes are read: a song still arriving learns its length with them.
        let byte_len = source.byte_len();
        let source: Box<dyn MediaSource> = if sized { source } else { Box::new(Unsized(source)) };
        let mss = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
        let mut h = Hint::new();
        if let Some(x) = hint {
            if x.contains('/') {
                h.mime_type(x);
            } else {
                h.with_extension(x);
            }
        }
        let reader = symphonia::default::get_probe().probe(&h, mss, FormatOptions::default(), MetadataOptions::default()).map_err(|e| e.to_string())?;
        let track = reader.default_track(TrackType::Audio).ok_or("no audio in it")?;
        let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("no audio in it")?;
        let rate = params.sample_rate.ok_or("no sample rate")?;
        let channels = params.channels.as_ref().map_or(2, |c| c.count()).max(1);
        let codec = codec_of(params.codec);
        let delay_known = track.delay.is_some();
        let inner = match (codec, Pcm::of(params.codec)) {
            _ if packets => Inner::Raw,
            (Some(Codec::Aac), _) if mode == Mode::Play => Inner::Coded(Box::new(Decoder::whole_aac(rate, channels, params.extra_data.as_deref())?)),
            (Some(c), _) => Inner::Coded(Box::new(Decoder::new(c, rate, channels, params.extra_data.as_deref(), c == Codec::Mp3 && delay_known)?)),
            (None, Some(p)) => Inner::Pcm(p),
            _ => return Err(format!("{:?} is not decoded here", params.codec)),
        };
        let (track_delay, track_padding) = (track.delay, track.padding);
        let cut_short = sized && byte_len.is_some() && matches!(codec, Some(Codec::Opus | Codec::Vorbis)) && track.num_frames.is_none();
        let setup = params.extra_data.clone();
        let bits = params.bits_per_sample.or(params.bits_per_coded_sample).unwrap_or(0);
        // The gapless numbers are in the track's timescale: used when it counts frames.
        let per_frame = track.time_base.is_some_and(|t| t.numer.get() == 1 && t.denom.get() == rate);
        let mp4 = gapless.filter(|_| per_frame && codec.is_some()).map(|g| (g.delay as i64, g.frames.map_or(i64::MAX, |f| (g.delay + f) as i64)));
        let mp4_total = gapless.map_or(0, |g| g.total as i64);
        let frames = match mp4 {
            Some((delay, end)) if end < i64::MAX => Some(end - delay),
            _ => track.num_frames.map(|n| n as i64),
        };
        let duration_us = frames.map(|n| n * 1_000_000 / rate as i64).or(duration_ms.map(|d| d * 1000)).unwrap_or(0);
        let coded = packets.then(|| coding(codec, setup.as_deref(), rate)).flatten().map(|coding| {
            let bitrate = match byte_len {
                Some(b) if duration_us > 0 => (b as i128 * 8_000_000 / duration_us as i128).min(u32::MAX as i128) as u32,
                _ => 0,
            };
            let (delay, padding) = match (coding, mp4) {
                // An MP4's numbers are in decoder frames, as media3 passes them.
                (_, Some((delay, end))) => (delay as u32, if end < i64::MAX { (mp4_total - end).max(0) as u32 } else { 0 }),
                // symphonia moves the MP3 decoder's 529 frames from padding to delay; media3 passes
                // the LAME tag's numbers as they are.
                (Coding::Mp3, None) => match track_delay {
                    Some(d) => (d.saturating_sub(MP3_DECODER_DELAY as u32), track_padding.unwrap_or(0) + MP3_DECODER_DELAY as u32),
                    None => (0, 0),
                },
                // Pre-skip is in the header the output reads.
                (Coding::Opus, None) => (0, track_padding.unwrap_or(0)),
                (Coding::Aac, None) => (track_delay.unwrap_or(0), track_padding.unwrap_or(0)),
            };
            CodedSong { coded: Coded { coding, rate, channels }, bitrate, delay, padding, setup: setup.clone(), from_frame: 0 }
        });
        let compression = match (codec, Pcm::of(params.codec)) {
            (Some(Codec::Aac), _) if he_aac(setup.as_deref(), rate) => "HE-AAC",
            (Some(Codec::Aac), _) => "AAC-LC",
            (Some(Codec::Mp3), _) => "MP3",
            (Some(Codec::Flac), _) => "FLAC",
            (Some(Codec::Vorbis), _) => "Vorbis",
            (Some(Codec::Alac), _) => "ALAC",
            (Some(Codec::Opus), _) => "Opus",
            (None, Some(_)) => "PCM",
            (None, None) => "an unknown compression",
        };
        let max_frames = codec.map_or(8192, Codec::max_frames);
        let id = track.id;
        // A live MP3 station (no length or duration, from the start) is read frame by frame by `mpeg.rs`
        // so it plays through noise and format changes. A transcode without a length is not live.
        let live = byte_len.is_none() && duration_ms.is_none() && from_ms == 0;
        let reader = if live && !packets && params.codec == CODEC_ID_MP3 {
            Packets::Mpeg(Frames::new(reader.into_inner(), id))
        } else {
            Packets::Container(reader)
        };
        let width = encoding.width();
        let mut d = Stream {
            reader,
            track: id,
            inner,
            codec,
            delay_known,
            format: Format { rate, channels, encoding },
            buf: Vec::with_capacity(max_frames * channels * width),
            frame: Some(0),
            skip_to: 0,
            at_us: 0,
            duration_us,
            ended: false,
            primed: false,
            mp4,
            bits,
            compression,
            coded,
            packet_frames: 0,
            first_packet: true,
            settled: false,
            reshape: None,
            cut_short,
        };
        if from_ms > 0 {
            d.seek(from_ms)?;
        }
        if packets {
            // The first packet says where reading starts after a seek.
            d.primed = d.next_packet();
            return Ok(d);
        }
        // The first decoded packet gives the true format (an AAC stream's real rate). Retried through a
        // station's initial noise.
        d.primed = d.next();
        for _ in 0..64 {
            if !d.primed || !d.buf.is_empty() {
                break;
            }
            d.primed = d.next();
        }
        if let Inner::Coded(dec) = &d.inner {
            d.format.rate = dec.rate();
            d.format.channels = dec.channels();
        }
        d.settled = true;
        Ok(d)
    }

    fn seek(&mut self, ms: i64) -> Result<(), String> {
        let to = match self.mp4 {
            // Song time is track time minus the delay; start two AAC frames early to warm the decoder.
            Some((delay, _)) => SeekTo::Timestamp { ts: Timestamp::new((ms * self.format.rate as i64 / 1000 + delay - AAC_WARM_UP).max(0)), track_id: self.track },
            None => SeekTo::Time { time: Time::from_millis(ms), track_id: Some(self.track) },
        };
        let seeked = match self.reader.container().ok_or("a live stream is not seeked")?.seek(SeekMode::Accurate, to) {
            Ok(s) => s,
            // Past the end (the stated length was longer than the music): ended, as if played through.
            Err(e) if ms > 0 && past_end(&e) => {
                self.ended = true;
                self.frame = None;
                return Ok(());
            }
            Err(e) => return Err(e.to_string()),
        };
        self.skip_to = match self.mp4 {
            Some(_) => ms * self.format.rate as i64 / 1000,
            None => seeked.required_ts.get(),
        };
        self.frame = None;
        if let Inner::Coded(dec) = &mut self.inner {
            dec.reset(false);
        }
        Ok(())
    }

    /// Frames the decoder drops of the first packet after a reset (MP3 filterbank, Opus pre-roll).
    fn dropped_after_reset(&self) -> i64 {
        match self.codec {
            Some(Codec::Mp3) => MP3_DECODER_DELAY as i64,
            Some(Codec::Opus) => OPUS_PRE_ROLL,
            _ => 0,
        }
    }

    /// The song frame of a packet's first decoded sample. An MP3 without a LAME header is stamped from
    /// its first decoded sample, which the decoder drops.
    fn song_frame(&self, pts: i64) -> i64 {
        let origin = if self.codec == Some(Codec::Mp3) && !self.delay_known { MP3_DECODER_DELAY as i64 } else { 0 };
        pts - origin + self.dropped_after_reset()
    }

    /// Reads the next undecoded packet into `buf`; false at the end.
    fn next_packet(&mut self) -> bool {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) | Err(_) => {
                    self.ended = true;
                    return false;
                }
            };
            if packet.track_id != self.track || packet.data.is_empty() {
                continue;
            }
            let (pts, dur) = (packet.pts.get(), packet.dur.get() as i64);
            // The audible part: an MP4's stamps include delay and padding; elsewhere the packet's trims
            // say it (symphonia 0.6.1 counts them inside `dur`).
            let (start, frames) = match self.mp4 {
                Some((delay, end)) => {
                    let (from, to) = (pts.max(delay), (pts + dur).min(end));
                    (from - delay, (to - from).max(0))
                }
                None => {
                    let (trim_start, trim_end) = (packet.trim_start.get() as i64, packet.trim_end.get() as i64);
                    (pts + trim_start, (dur - trim_start - trim_end).max(0))
                }
            };
            // The packet's own memory, not a copy.
            self.buf = packet.data.into_vec();
            self.packet_frames = frames as u64;
            if std::mem::take(&mut self.first_packet) {
                if let Some(c) = self.coded.as_mut() {
                    c.from_frame = start.max(0);
                }
            }
            self.frame = Some(start + frames);
            self.at_us = start.max(0) * 1_000_000 / self.format.rate as i64;
            return true;
        }
    }

    /// Decodes the next audible packet into `buf` (empty: retry later); false at the end.
    fn next(&mut self) -> bool {
        let (ch, enc) = (self.format.channels, self.format.encoding);
        let mut idle = 0;
        loop {
            if idle == IDLE_PACKETS {
                self.buf.clear();
                return true;
            }
            idle += 1;
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                // No packet found yet (noise on a station).
                Err(e) if for_now(&e) => {
                    self.buf.clear();
                    return true;
                }
                // A chained Ogg stream's next song.
                Err(SymphoniaError::ResetRequired) if self.chain_on() => continue,
                Ok(None) | Err(_) => {
                    self.ended = true;
                    return self.drained();
                }
            };
            if packet.track_id != self.track {
                continue;
            }
            let mut at = match self.frame {
                Some(f) => f,
                None if matches!(self.inner, Inner::Pcm(_)) => packet.pts.get(),
                None => self.song_frame(packet.pts.get()),
            };
            let (samples, pcm, ch, rate): (&[f32], Option<(Pcm, usize)>, usize, u32) = match &mut self.inner {
                Inner::Coded(dec) => match dec.decode_lent(&packet.data) {
                    Ok(lent) => (lent.samples, None, lent.channels.max(1), lent.rate),
                    Err(nori_player::decode::Fault::Broken) => {
                        self.ended = true;
                        return false;
                    }
                    Err(_) => continue,
                },
                Inner::Pcm(p) => (&[], Some((*p, p.width())), ch, self.format.rate),
                Inner::Raw => return false,
            };
            let n = match pcm {
                Some((_, w)) => packet.data.len() / (w * ch),
                None => samples.len() / ch,
            };
            if n == 0 {
                continue;
            }
            // Encoder delay and padding as the container states them (Opus drops its own pre-skip).
            let (mut trim_start, mut trim_end) = if self.codec == Some(Codec::Opus) || self.frame.is_none() { (0, packet.trim_end.get() as usize) } else { (packet.trim_start.get() as usize, packet.trim_end.get() as usize) };
            if let Some((delay, end)) = self.mp4 {
                // An MP4 stamp counts decoder frames, delay included.
                let raw = packet.pts.get();
                trim_start = (delay - raw).clamp(0, n as i64) as usize;
                trim_end = (raw + n as i64 - end).clamp(0, n as i64) as usize;
                at = raw + trim_start as i64 - delay;
            }
            let (from, to) = (trim_start.min(n), n.saturating_sub(trim_end).max(trim_start.min(n)));
            let mut from = from;
            // After a seek, drop what comes before the target.
            let (mut first, end) = (at, at + (to - from) as i64);
            self.frame = Some(end);
            if end <= self.skip_to {
                continue;
            }
            if first < self.skip_to {
                from += (self.skip_to - first) as usize;
                first = self.skip_to;
            }
            if from >= to {
                continue;
            }
            self.buf.clear();
            match pcm {
                Some((p, w)) => p.put(&packet.data[from * ch * w..to * ch * w], enc, &mut self.buf),
                // A format change mid-stream (a station's next song): converted, or it would play at the
                // wrong speed.
                None if self.settled && (rate, ch) != (self.format.rate, self.format.channels) => {
                    let made = reshaped(&mut self.reshape, (rate, ch), self.format, &samples[from * ch..to * ch], &mut self.buf);
                    self.frame = Some(first + made as i64);
                    if made == 0 {
                        continue;
                    }
                }
                None => {
                    self.reshape = None;
                    put(&samples[from * ch..to * ch], enc, &mut self.buf);
                }
            }
            self.at_us = first * 1_000_000 / self.format.rate as i64;
            return true;
        }
    }
}

impl Stream {
    /// At the end of the packets: what the decoder still held back, into `buf` (its end trimmed as a
    /// packet's). False when nothing was.
    fn drained(&mut self) -> bool {
        let (Inner::Coded(dec), Some(at)) = (&mut self.inner, self.frame) else { return false };
        let Ok(lent) = dec.drain_lent() else { return false };
        let ch = lent.channels.max(1);
        let n = lent.samples.len() / ch;
        let to = match self.mp4 {
            Some((delay, end)) => (end - delay - at).clamp(0, n as i64) as usize,
            None => n,
        };
        let from = (self.skip_to - at).clamp(0, to as i64) as usize;
        if from >= to {
            return false;
        }
        self.buf.clear();
        put(&lent.samples[from * ch..to * ch], self.format.encoding, &mut self.buf);
        self.frame = Some(at + to as i64);
        self.at_us = (at + from as i64) * 1_000_000 / self.format.rate as i64;
        true
    }

    /// Follows a chained Ogg stream into its next logical stream (a station's next song) with a new
    /// decoder, keeping the output format. False when it cannot.
    fn chain_on(&mut self) -> bool {
        let Some(reader) = self.reader.container() else { return false };
        let Some(track) = reader.default_track(TrackType::Audio) else { return false };
        let Some(params) = track.codec_params.as_ref().and_then(|p| p.audio()) else { return false };
        let (Some(codec), Some(rate)) = (codec_of(params.codec), params.sample_rate) else { return false };
        let channels = params.channels.as_ref().map_or(2, |c| c.count()).max(1);
        let id = track.id;
        let Ok(dec) = Decoder::new(codec, rate, channels, params.extra_data.as_deref(), false) else { return false };
        if !matches!(self.inner, Inner::Coded(_)) {
            return false;
        }
        self.inner = Inner::Coded(Box::new(dec));
        self.codec = Some(codec);
        self.track = id;
        true
    }
}

impl Stream {
    fn duration_us(&self) -> i64 {
        match self.frame {
            // Read to its end: exactly what came out.
            Some(f) if self.ended => f * 1_000_000 / self.format.rate as i64,
            _ => self.duration_us,
        }
    }

    /// Drops everything before `ms`, trimming the primed first buffer. False once a buffer was handed
    /// out, or for packets.
    fn skip_ahead(&mut self, ms: i64) -> bool {
        if !self.primed || self.coded.is_some() {
            return false;
        }
        let rate = self.format.rate as i64;
        let to = ms * rate / 1000;
        self.skip_to = self.skip_to.max(to);
        let frame = (self.format.channels * self.format.encoding.width()).max(1);
        let n = (self.buf.len() / frame) as i64;
        let Some(end) = self.frame else { return true };
        let first = end - n;
        if end <= to {
            self.buf.clear();
            self.primed = false;
        } else if first < to {
            self.buf.drain(..(to - first) as usize * frame);
            self.at_us = to * 1_000_000 / rate;
        }
        true
    }

    fn fill(&mut self) -> bool {
        if std::mem::take(&mut self.primed) {
            return true;
        }
        if !self.ended && self.next() {
            return true;
        }
        self.buf.clear();
        false
    }

    fn buffer(&self) -> &[u8] {
        // The primed buffer is handed out by the first fill.
        if self.primed {
            &[]
        } else {
            &self.buf
        }
    }

    fn at_us(&self) -> i64 {
        self.at_us
    }
}

/// Decodes a song whole on disk (delay and padding cut) into `each` as float buffers with rate and
/// channels, for measuring. `each` returns whether to go on. Ok(false) when stopped or not read to the end.
#[cfg(feature = "core")]
pub(crate) fn decode_whole(source: Box<dyn MediaSource>, hint: Option<&str>, each: impl FnMut(u32, usize, &[f32]) -> bool) -> Result<bool, String> {
    decode_from_start(source, hint, true, each)
}

/// [`decode_whole`] over bytes still arriving, in order (`crate::arriving`); not for MP4.
pub(crate) fn decode_as_it_comes(source: Box<dyn MediaSource>, hint: Option<&str>, each: impl FnMut(u32, usize, &[f32]) -> bool) -> Result<bool, String> {
    decode_from_start(source, hint, false, each)
}

/// Whether a container can be decoded as it arrives (an MP4 may keep its index at the end).
#[cfg(feature = "core")]
pub(crate) fn decodes_as_it_comes(hint: Option<&str>) -> bool {
    !hint.is_some_and(mp4_like)
}

fn decode_from_start(source: Box<dyn MediaSource>, hint: Option<&str>, whole: bool, mut each: impl FnMut(u32, usize, &[f32]) -> bool) -> Result<bool, String> {
    let mut s = Stream::open(source, Spec { whole, ..Spec::new(hint, 0, None, Encoding::Float, Mode::Measure) })?;
    let mut floats: Vec<f32> = Vec::new();
    while s.fill() {
        if s.buffer().is_empty() {
            continue;
        }
        floats.clear();
        floats.extend(s.buffer().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        if !each(s.format.rate, s.format.channels, &floats) {
            return Ok(false);
        }
    }
    Ok(s.ended)
}

/// The source from after its ID3v2 tags. symphonia's ID3 reader is left out (it keeps the cover in
/// memory), and its probe would find false MPEG headers inside a large cover. Read through rather than
/// seeked over, so a song still arriving is fetched in one piece.
fn past_id3(mut source: Box<dyn MediaSource>) -> io::Result<Box<dyn MediaSource>> {
    let mut start = 0u64;
    loop {
        let mut header = [0u8; 10];
        let whole = read_up_to(&mut source, &mut header)? == header.len();
        let size = header[6..].iter().try_fold(0u64, |n, &b| (b < 0x80).then_some(n << 7 | b as u64));
        match size {
            Some(size) if whole && header.starts_with(b"ID3") && header[3] < 0xff && header[4] < 0xff => {
                // Plus a 10-byte footer if flagged.
                let length = size + if header[5] & 0x10 != 0 { 10 } else { 0 };
                if io::copy(&mut (&mut source).take(length), &mut io::sink())? < length {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the song ends inside its tag"));
                }
                start += 10 + length;
            }
            _ => break,
        }
    }
    source.seek(SeekFrom::Start(start))?;
    Ok(if start == 0 { source } else { Box::new(After { inner: source, start }) })
}

/// Reads until `buf` is full or the source ends; returns the bytes read.
fn read_up_to(source: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match source.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// A source offset to start at byte `start`.
struct After {
    inner: Box<dyn MediaSource>,
    start: u64,
}

impl Read for After {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for After {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let to = match to {
            SeekFrom::Start(p) => SeekFrom::Start(self.start + p),
            other => other,
        };
        let at = self.inner.seek(to)?;
        at.checked_sub(self.start).ok_or_else(|| io::Error::other("seek before the start"))
    }
}

impl MediaSource for After {
    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }

    fn byte_len(&self) -> Option<u64> {
        self.inner.byte_len().map(|l| l.saturating_sub(self.start))
    }
}

/// Hides the length of a transcode whose length is only an estimate. Shown a length, readers probe near
/// the end (Ogg's last page, ADTS, bisecting seeks), each a range the server answers only after
/// transcoding up to there: tens of seconds of silence. Unsized, the song is read in order.
struct Unsized(Box<dyn MediaSource>);

impl Read for Unsized {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Seek for Unsized {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.0.seek(to)
    }
}

impl MediaSource for Unsized {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// The offloadable compression of `codec`, if any. AAC only as LC (object type 2, what
/// `ENCODING_AAC_LC` promises) above 24 kHz: at or below, "AAC-LC" may be implicit-SBR HE-AAC
/// (`nori_settings::decoder::implicit_sbr`), which would play at half rate.
fn coding(codec: Option<Codec>, setup: Option<&[u8]>, rate: u32) -> Option<Coding> {
    match codec? {
        Codec::Mp3 => Some(Coding::Mp3),
        Codec::Aac if rate > 24_000 && setup.and_then(|s| s.first()).is_some_and(|b| b >> 3 == 2) => Some(Coding::Aac),
        Codec::Opus => Some(Coding::Opus),
        _ => None,
    }
}

/// Whether a song's hint names an MP4 container.
fn mp4_like(hint: &str) -> bool {
    matches!(hint.to_ascii_lowercase().as_str(), "m4a" | "m4b" | "mp4" | "aac" | "alac" | "audio/mp4" | "audio/x-m4a" | "audio/aac")
}

/// A song opened for the engine: open, opening on a thread of its own, or failed.
pub struct Demuxed {
    state: State,
    /// The loader of bytes still arriving, and the thread to wake.
    loader: Option<(Arc<Loader>, Thread)>,
}

enum State {
    Opening(Arc<Opening>),
    Open(Box<Stream>),
    Failed(PlaybackError, String),
}

/// A song opening on another thread.
#[derive(Default)]
struct Opening {
    done: Mutex<Done>,
    /// Nobody wants it any more: its reads fail.
    abandoned: Arc<AtomicBool>,
}

#[derive(Default)]
struct Done {
    opened: Option<Result<Stream, String>>,
    /// Woken when it is open.
    waiter: Option<Thread>,
}

impl Drop for Demuxed {
    fn drop(&mut self) {
        // Stop the opening thread's reads, which would fight another reader over the fetch position.
        if let (State::Opening(o), Some((l, _))) = (&self.state, &self.loader) {
            o.abandoned.store(true, Ordering::Release);
            l.nudge();
        }
    }
}

impl Demuxed {
    /// Opens bytes that are all here (a file) at `from_ms`, decoding to `encoding`. `hint` is an extension
    /// or MIME type; `duration_ms` the tagged length.
    pub fn open(source: Box<dyn MediaSource>, hint: Option<&str>, from_ms: i64, duration_ms: Option<i64>, encoding: Encoding) -> Result<Demuxed, String> {
        let s = Stream::open(source, Spec::new(hint, from_ms, duration_ms, encoding, Mode::Play))?;
        Ok(Demuxed { state: State::Open(Box::new(s)), loader: None })
    }

    /// [`Demuxed::open`] as undecoded packets ([`Demuxed::packet`]).
    pub fn open_packets(source: Box<dyn MediaSource>, hint: Option<&str>, from_ms: i64, duration_ms: Option<i64>) -> Result<Demuxed, String> {
        let s = Stream::open(source, Spec::new(hint, from_ms, duration_ms, Encoding::Pcm16, Mode::Packets))?;
        Ok(Demuxed { state: State::Open(Box::new(s)), loader: None })
    }

    /// [`Demuxed::load`] as undecoded packets ([`Demuxed::packet`]).
    pub fn load_packets(loader: Arc<Loader>, engine: Thread, hint: Option<&str>, from_ms: i64, duration_ms: Option<i64>, estimated: bool) -> Demuxed {
        Demuxed::start(loader, engine, hint, from_ms, duration_ms, estimated, Encoding::Pcm16, Mode::Packets)
    }

    /// The packets' format and encoder gap once open; None when decoded or not offloadable.
    pub fn coded(&self) -> Option<&CodedSong> {
        self.stream().and_then(|s| s.coded.as_ref())
    }

    /// The compression's name (MP3, FLAC, HE-AAC, ...), once open.
    pub fn compression(&self) -> Option<&'static str> {
        self.stream().map(|s| s.compression)
    }

    /// Reads the next packet into [`Reading::buffer`] ([`Demuxed::packet_frames`] its frames); false at
    /// the end. Only once ready.
    pub fn packet(&mut self) -> bool {
        match &mut self.state {
            State::Open(s) if s.primed => {
                s.primed = false;
                true
            }
            State::Open(s) if !s.ended => s.next_packet(),
            _ => false,
        }
    }

    /// The last packet's frames.
    pub fn packet_frames(&self) -> u64 {
        self.stream().map_or(0, |s| s.packet_frames)
    }

    /// Opens the song `loader` fetches at `from_ms`: on a thread of its own unless all of it is here;
    /// `engine` is woken when it is open and whenever it waited for bytes. `estimated`: the server's
    /// length is a transcode's estimate, hidden from the reader while the song arrives ([`Unsized`]).
    pub fn load(loader: Arc<Loader>, engine: Thread, hint: Option<&str>, from_ms: i64, duration_ms: Option<i64>, estimated: bool, encoding: Encoding) -> Demuxed {
        Demuxed::start(loader, engine, hint, from_ms, duration_ms, estimated, encoding, Mode::Play)
    }

    #[allow(clippy::too_many_arguments)]
    fn start(loader: Arc<Loader>, engine: Thread, hint: Option<&str>, from_ms: i64, duration_ms: Option<i64>, estimated: bool, encoding: Encoding, mode: Mode) -> Demuxed {
        if loader.complete() {
            let opened = Stream::open(Box::new(loader.reader()), Spec::new(hint, from_ms, duration_ms, encoding, mode));
            if !opened.as_ref().is_ok_and(|s| s.cut_short) || !loader.refetch() {
                let state = match opened {
                    Ok(s) => State::Open(Box::new(s)),
                    Err(why) => State::Failed(PlaybackError::Other, why),
                };
                return Demuxed { state, loader: Some((loader, engine)) };
            }
        }
        let opening = Arc::new(Opening::default());
        let (o, l, hint) = (opening.clone(), loader.clone(), hint.map(str::to_string));
        let spawned = std::thread::Builder::new().name("nori-open".into()).spawn(move || {
            // An MP4's gapless boxes may be at its end: wait for the whole song (one burst, mostly)
            // rather than fetching the end separately.
            let whole = hint.as_deref().is_some_and(mp4_like) && l.wait_whole();
            let spec = Spec { whole, sized: !estimated || whole, ..Spec::new(hint.as_deref(), from_ms, duration_ms, encoding, mode) };
            let mut seen = l.shortened();
            let reader = || Box::new(l.reader_until(o.abandoned.clone()));
            let mut opened = Stream::open(reader(), spec);
            // The reader probed the end where an estimated length put it, past the real end, and failed
            // or found no length: reopen now the real end is known.
            for _ in 0..REOPENS {
                let now = l.shortened();
                if now == seen || o.abandoned.load(Ordering::Acquire) {
                    break;
                }
                seen = now;
                opened = Stream::open(reader(), spec);
            }
            if opened.as_ref().is_ok_and(|s| s.cut_short) && l.refetch() {
                opened = Stream::open(reader(), spec);
            }
            let mut done = o.done.lock();
            done.opened = Some(opened);
            if let Some(t) = done.waiter.take() {
                t.unpark();
            }
        });
        let state = match spawned {
            Ok(_) => State::Opening(opening),
            Err(e) => State::Failed(PlaybackError::Other, e.to_string()),
        };
        Demuxed { state, loader: Some((loader, engine)) }
    }

    /// Open, and a copy cut short ([`Stream::cut_short`]).
    pub(crate) fn cut_short(&self) -> bool {
        self.stream().is_some_and(|s| s.cut_short)
    }

    fn stream(&self) -> Option<&Stream> {
        match &self.state {
            State::Open(s) => Some(s),
            _ => None,
        }
    }

    /// Why the song failed: the loader's failure if any (more telling than the reader's).
    fn failure(&self, why: String) -> (PlaybackError, String) {
        self.loader_failure().unwrap_or((PlaybackError::Other, why))
    }

    /// The loader's failure: the network's, unless the server answered with an error status.
    fn loader_failure(&self) -> Option<(PlaybackError, String)> {
        let (l, _) = self.loader.as_ref()?;
        let e = l.error()?;
        Some((if l.answered().is_some() { PlaybackError::Other } else { PlaybackError::Network }, e))
    }
}

impl Reading for Demuxed {
    fn format(&self) -> Format {
        self.stream().map_or(Format { rate: 44_100, channels: 2, encoding: Encoding::Pcm16 }, |s| s.format)
    }

    fn duration_us(&self) -> i64 {
        self.stream().map_or(0, Stream::duration_us)
    }

    fn ready(&mut self) -> bool {
        if let State::Opening(o) = &self.state {
            let mut done = o.done.lock();
            let Some(opened) = done.opened.take() else {
                done.waiter = self.loader.as_ref().map(|(_, engine)| engine.clone());
                return false;
            };
            drop(done);
            self.state = match opened {
                Ok(s) => State::Open(Box::new(s)),
                Err(why) => {
                    let (kind, why) = self.failure(why);
                    State::Failed(kind, why)
                }
            };
        }
        match &self.state {
            State::Open(s) => s.primed || s.ended || self.loader.as_ref().is_none_or(|(l, engine)| l.ready_or_wake(engine)),
            _ => true,
        }
    }

    fn error(&self) -> Option<(PlaybackError, String)> {
        match &self.state {
            State::Failed(kind, why) => Some((*kind, why.clone())),
            // Ended early because the bytes stopped.
            State::Open(s) if s.ended => self.loader_failure(),
            _ => None,
        }
    }

    /// A decoder panic fails the song, not the engine's thread.
    fn fill(&mut self) -> bool {
        let State::Open(s) = &mut self.state else { return false };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.fill())) {
            Ok(more) => more,
            Err(p) => {
                self.state = State::Failed(PlaybackError::Other, format!("the song could not be decoded: {}", panic_words(&*p)));
                false
            }
        }
    }

    fn buffer(&self) -> &[u8] {
        self.stream().map_or(&[], Stream::buffer)
    }

    fn at_us(&self) -> i64 {
        self.stream().map_or(0, Stream::at_us)
    }

    fn bits(&self) -> u32 {
        self.stream().map_or(0, |s| s.bits)
    }

    fn skip_to_ms(&mut self, ms: i64) -> bool {
        match &mut self.state {
            State::Open(s) => s.skip_ahead(ms),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Body, ByteSource, OpenError};
    use parking_lot::Condvar;

    const MP3: &[u8] = include_bytes!("../../player/testdata/tone440.mp3");

    /// Answers only once the gate opens.
    #[derive(Default)]
    struct Gated(Mutex<bool>, Condvar);

    impl ByteSource for Gated {
        fn open(&self, _: &str, from: u64) -> Result<Body, OpenError> {
            let mut open = self.0.lock();
            while !*open {
                self.1.wait(&mut open);
            }
            Ok(Body { start: from, len: Some(MP3.len() as u64), reader: Box::new(io::Cursor::new(&MP3[from as usize..])) })
        }
    }

    /// Waits for `d`'s opening, which wakes this thread.
    fn opened(d: &mut Demuxed) {
        while !d.ready() {
            std::thread::park();
        }
    }

    /// Everything `d` decodes.
    fn decoded(mut d: Demuxed) -> Vec<u8> {
        opened(&mut d);
        let mut out = Vec::new();
        while d.fill() {
            out.extend_from_slice(d.buffer());
        }
        out
    }

    #[test]
    fn opened_early_reads_as_file() {
        let gate = Arc::new(Gated::default());
        let loader = Loader::start(gate.clone(), "tone".into(), [1_000, 4_000, 0, 0, 1 << 30], None, None);
        let arriving = Demuxed::load(loader.clone(), std::thread::current(), Some("mp3"), 0, None, false, Encoding::Pcm16);
        let mut packets = Demuxed::load_packets(loader.clone(), std::thread::current(), Some("mp3"), 0, None, false);
        // The bytes come once both openings wait for them.
        loader.wait_blocked(2);
        *gate.0.lock() = true;
        gate.1.notify_all();
        let file = Demuxed::open(Box::new(io::Cursor::new(MP3)), Some("mp3"), 0, None, Encoding::Pcm16).unwrap();
        assert!(decoded(arriving) == decoded(file), "the same samples, its encoder delay and padding cut");
        opened(&mut packets);
        assert!(packets.coded().is_some_and(|c| c.bitrate > 0), "its bitrate sizes an offload track: {:?}", packets.coded());
    }
}
