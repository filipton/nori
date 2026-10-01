//! A simulated player for tests: the real `pipeline` (decoder, transition engine, sound chain) over a
//! simulated AudioTrack on a virtual clock, with simulated songs and planner. Output is captured as
//! samples; minutes of music run in milliseconds.

use std::collections::VecDeque;
use std::ops::{Deref, DerefMut, Range};
use std::sync::Arc;

use crate::automix::analysis::Analyzer;
use crate::automix::plan;
use crate::decode::{Codec, Decoder, MP3_DECODER_DELAY};
use crate::engine::{Host, Plan};
use crate::pcm::{Encoding, Format};
use crate::pipeline::{self, Reading as _, Sink, Songs};
use crate::playlist::Playlist;
use crate::transitions::{engine_plan, pick, whole_song, Skip, TransitionPrefs, WindowSong};
use crate::types::TrackAnalysis;

pub use crate::pipeline::{Sound, BASE_OFFSET_US, READ_AHEAD_US};

/// Virtual clock step per render turn.
pub const STEP_MS: i64 = 10;
/// Frames per buffer of a PCM track.
const PCM_BUFFER_FRAMES: u64 = 1024;

/// 16-bit samples as little-endian bytes (a plain copy on little-endian; per-sample is slow in debug).
pub fn bytes(samples: &[i16]) -> Vec<u8> {
    if cfg!(target_endian = "little") {
        // SAFETY: an i16 slice is valid as twice as many bytes.
        return unsafe { std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), samples.len() * 2) }.to_vec();
    }
    samples.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Little-endian bytes as 16-bit samples.
pub fn samples(bytes: &[u8]) -> Vec<i16> {
    let mut out = vec![0i16; bytes.len() / 2];
    if cfg!(target_endian = "little") {
        // SAFETY: copies `out.len() * 2` bytes, within both buffers.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr().cast::<u8>(), out.len() * 2) };
        return out;
    }
    for (o, c) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        *o = i16::from_le_bytes([c[0], c[1]]);
    }
    out
}

/// MPEG-1 layer III frames of a file, skipping ID3 and the Xing/Info frame.
pub fn mp3_frames(file: &[u8]) -> Vec<&[u8]> {
    let mut i = 0;
    if file.starts_with(b"ID3") {
        let size = file[6..10].iter().fold(0usize, |a, &b| (a << 7) | (b & 0x7F) as usize);
        i = 10 + size;
    }
    let mut frames = Vec::new();
    while i + 4 <= file.len() {
        let h = u32::from_be_bytes([file[i], file[i + 1], file[i + 2], file[i + 3]]);
        if h >> 21 != 0x7FF {
            i += 1;
            continue;
        }
        let bitrate = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320][((h >> 12) & 0xF) as usize] * 1000;
        let rate = [44_100, 48_000, 32_000][((h >> 10) & 3) as usize];
        let len = 144 * bitrate / rate + ((h >> 9) & 1) as usize;
        if len == 0 || i + len > file.len() {
            break;
        }
        let frame = &file[i..i + len];
        let info = frame.windows(4).take(64).any(|w| w == b"Xing" || w == b"Info");
        if !info {
            frames.push(frame);
        }
        i += len;
    }
    frames
}

/// Opus packets of an Ogg file and media3's setup (OpusHead, pre-skip ns, 80 ms pre-roll ns).
pub fn ogg_opus(file: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut packets = Vec::new();
    let mut cur = Vec::new();
    let mut i = 0;
    while i + 27 <= file.len() && &file[i..i + 4] == b"OggS" {
        let segs = file[i + 26] as usize;
        let table = &file[i + 27..i + 27 + segs];
        let mut at = i + 27 + segs;
        for &l in table {
            cur.extend_from_slice(&file[at..at + l as usize]);
            at += l as usize;
            if l < 255 {
                packets.push(std::mem::take(&mut cur));
            }
        }
        i = at;
    }
    let head = packets.remove(0);
    packets.remove(0); // OpusTags
    let pre_skip = u16::from_le_bytes([head[10], head[11]]) as i64;
    let mut setup = head.clone();
    setup.extend_from_slice(&(pre_skip * 1_000_000_000 / 48_000).to_ne_bytes());
    setup.extend_from_slice(&80_000_000i64.to_ne_bytes());
    (setup, packets)
}

/// 48 kHz samples in an Opus packet, from its TOC (RFC 6716 3.1).
fn opus_frames(packet: &[u8]) -> i64 {
    let Some(&toc) = packet.first() else { return 0 };
    let config = (toc >> 3) as i64;
    let per_frame = match config {
        0..=11 => [480, 960, 1920, 2880][(config % 4) as usize],
        12..=15 => [480, 960][(config % 2) as usize],
        _ => [120, 240, 480, 960][(config % 4) as usize],
    };
    let count = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => packet.get(1).map_or(0, |b| (b & 0x3F) as i64),
    };
    per_frame * count
}

/// A song's source audio.
#[derive(Clone)]
pub enum Audio {
    /// PCM `frames` long, repeating `cycle` (so long songs are cheap).
    Pcm { rate: u32, channels: usize, cycle: Arc<Vec<u8>>, frames: u64, float: bool },
    /// Compressed packets with setup, each packet's first sample position (before decoder drops), and
    /// the decoded length.
    Coded { codec: Codec, rate: u32, channels: usize, setup: Option<Arc<Vec<u8>>>, packets: Arc<Vec<Vec<u8>>>, starts: Arc<Vec<i64>>, frames: u64 },
}

impl Audio {
    pub fn pcm(rate: u32, channels: usize, samples: &[i16]) -> Audio {
        let frames = (samples.len() / channels) as u64;
        Audio::Pcm { rate, channels, cycle: Arc::new(bytes(samples)), frames, float: false }
    }

    /// `cycle` over and over for `frames` frames.
    pub fn looped(rate: u32, channels: usize, cycle: &[i16], frames: u64) -> Audio {
        Audio::Pcm { rate, channels, cycle: Arc::new(bytes(cycle)), frames, float: false }
    }

    /// Float PCM.
    pub fn pcm_float(rate: u32, channels: usize, samples: &[f32]) -> Audio {
        let frames = (samples.len() / channels) as u64;
        Audio::Pcm { rate, channels, cycle: Arc::new(samples.iter().flat_map(|v| v.to_le_bytes()).collect()), frames, float: true }
    }

    /// An MP3 file, decoded without a LAME header (the decoder drops its own delay).
    pub fn mp3(file: &[u8]) -> Audio {
        let packets: Vec<Vec<u8>> = mp3_frames(file).into_iter().map(<[u8]>::to_vec).collect();
        let starts = (0..packets.len()).map(|k| k as i64 * 1152 - MP3_DECODER_DELAY as i64).collect();
        Self::coded(Codec::Mp3, 44_100, 2, None, packets, starts)
    }

    /// An Ogg Opus file.
    pub fn opus(file: &[u8]) -> Audio {
        let (setup, packets) = ogg_opus(file);
        let channels = setup[9] as usize;
        let pre_skip = u16::from_le_bytes([setup[10], setup[11]]) as i64;
        let starts = packets
            .iter()
            .scan(-pre_skip, |at, p| {
                let s = *at;
                *at += opus_frames(p);
                Some(s)
            })
            .collect();
        Self::coded(Codec::Opus, 48_000, channels, Some(setup), packets, starts)
    }

    fn coded(codec: Codec, rate: u32, channels: usize, setup: Option<Vec<u8>>, packets: Vec<Vec<u8>>, starts: Vec<i64>) -> Audio {
        let mut d = Decoder::new(codec, rate, channels, setup.as_deref(), false).expect("the codec opens");
        let frames = packets.iter().map(|p| d.decode_lent(p).map_or(0, |l| l.samples.len() / channels) as u64).sum();
        Audio::Coded { codec, rate, channels, setup: setup.map(Arc::new), packets: Arc::new(packets), starts: Arc::new(starts), frames }
    }

    pub fn format(&self) -> Format {
        let (rate, channels) = match self {
            Audio::Pcm { rate, channels, .. } | Audio::Coded { rate, channels, .. } => (*rate, *channels),
        };
        let encoding = if matches!(self, Audio::Pcm { float: true, .. }) { Encoding::Float } else { Encoding::Pcm16 };
        Format { rate, channels, encoding }
    }

    pub fn frames(&self) -> u64 {
        match self {
            Audio::Pcm { frames, .. } | Audio::Coded { frames, .. } => *frames,
        }
    }

    pub fn duration_us(&self) -> i64 {
        (self.frames() as i128 * 1_000_000 / self.format().rate as i128) as i64
    }

    /// The whole song decoded.
    pub fn decode_all(&self) -> Vec<i16> {
        let mut r = Reading::new(self, 0);
        let mut out = Vec::new();
        while r.fill() {
            out.extend(samples(&r.buf));
        }
        out
    }
}

/// A song in the queue.
#[derive(Clone)]
pub struct Track {
    pub id: String,
    pub album: Option<String>,
    pub number: i32,
    pub audio: Audio,
    /// Server-listed length when it differs from the audio (what the planner uses).
    pub listed_ms: Option<i64>,
}

impl Track {
    pub fn new(id: &str, audio: Audio) -> Track {
        Track { id: id.to_string(), album: None, number: 0, audio, listed_ms: None }
    }

    /// The same, listed by the server as `ms` long.
    pub fn listed_as(mut self, ms: i64) -> Track {
        self.listed_ms = Some(ms);
        self
    }

    /// Track `number` of `album`.
    pub fn on_album(mut self, album: &str, number: i32) -> Track {
        self.album = Some(album.to_string());
        self.number = number;
        self
    }

    pub fn duration_ms(&self) -> i64 {
        self.audio.duration_us() / 1000
    }

    fn window_song(&self) -> WindowSong {
        WindowSong {
            id: self.id.clone(),
            title: self.id.clone(),
            duration_ms: self.listed_ms.unwrap_or_else(|| self.duration_ms()),
            album_id: self.album.clone(),
            disc: 1,
            track: self.number,
            tag_bpm: 0.0,
            radio: false,
            album_run: 0,
        }
    }
}

/// Reads one song buffer by buffer.
pub struct Reading {
    audio: Audio,
    /// Next output frame from the song's start.
    frame: i64,
    /// After a seek, buffers ending before this are dropped (media3's decode-only).
    skip_to: i64,
    next_packet: usize,
    decoder: Option<Decoder>,
    out: Vec<i16>,
    buf: Vec<u8>,
    at_us: i64,
}

impl Reading {
    /// Reads from `from_frame`; compressed audio starts at the packet a seek would land on.
    fn new(audio: &Audio, from_frame: i64) -> Reading {
        let mut r = Reading { audio: audio.clone(), frame: from_frame, skip_to: from_frame, next_packet: 0, decoder: None, out: Vec::new(), buf: Vec::new(), at_us: 0 };
        if let Audio::Coded { codec, rate, channels, setup, starts, .. } = audio {
            let mut d = Decoder::new(*codec, *rate, *channels, setup.as_deref().map(|s| s.as_slice()), false).expect("the codec opens");
            // Frames the decoder drops after a reset (MP3 delay, Opus 80 ms pre-roll).
            let dropped = match codec {
                Codec::Mp3 => MP3_DECODER_DELAY as i64,
                Codec::Opus => 3840,
                _ => 0,
            };
            // Start at the last packet whose output begins at or before `from_frame`.
            r.frame = 0;
            if let Some(k) = starts.iter().rposition(|&s| s + dropped <= from_frame).filter(|&k| k > 0) {
                d.reset(false);
                r.next_packet = k;
                r.frame = starts[k] + dropped;
            }
            r.out = vec![0i16; codec.max_frames() * channels];
            r.decoder = Some(d);
        }
        r
    }
}

impl pipeline::Reading for Reading {
    fn format(&self) -> Format {
        self.audio.format()
    }

    fn duration_us(&self) -> i64 {
        self.audio.duration_us()
    }

    fn fill(&mut self) -> bool {
        let f = self.audio.format();
        let fb = f.frame_bytes();
        self.buf.clear();
        match &self.audio {
            Audio::Pcm { cycle, frames, .. } => {
                let cycle_frames = (cycle.len() / fb) as u64;
                let at = self.frame as u64;
                if at >= *frames {
                    return false;
                }
                let in_cycle = at % cycle_frames;
                let n = PCM_BUFFER_FRAMES.min(cycle_frames - in_cycle).min(frames - at);
                self.buf.extend_from_slice(&cycle[in_cycle as usize * fb..(in_cycle + n) as usize * fb]);
                self.at_us = at as i64 * 1_000_000 / f.rate as i64;
                self.frame += n as i64;
                true
            }
            Audio::Coded { packets, channels, .. } => loop {
                let Some(p) = packets.get(self.next_packet) else { return false };
                self.next_packet += 1;
                let d = self.decoder.as_mut().expect("a coded track has a decoder");
                let Ok(n) = d.decode_i16(p, &mut self.out) else { continue };
                if n == 0 {
                    continue;
                }
                let at = self.frame;
                self.frame += n as i64;
                if self.frame <= self.skip_to && at < self.skip_to {
                    continue;
                }
                self.buf.extend_from_slice(&bytes(&self.out[..n * channels]));
                self.at_us = at * 1_000_000 / f.rate as i64;
                return true;
            },
        }
    }

    fn buffer(&self) -> &[u8] {
        &self.buf
    }

    fn at_us(&self) -> i64 {
        self.at_us
    }
}

/// The songs the simulated player can open.
#[derive(Clone, Default)]
pub struct Tracks {
    pub list: Vec<Track>,
    /// Ids that fail to open.
    pub broken: Vec<String>,
}

impl Tracks {
    fn get(&self, id: &str) -> &Track {
        self.list.iter().find(|t| t.id == id).expect("a queued song is a known track")
    }
}

impl Deref for Tracks {
    type Target = Vec<Track>;
    fn deref(&self) -> &Vec<Track> {
        &self.list
    }
}

impl DerefMut for Tracks {
    fn deref_mut(&mut self) -> &mut Vec<Track> {
        &mut self.list
    }
}

impl Songs for Tracks {
    type Reading = Reading;

    fn open(&mut self, id: &str, from_ms: i64) -> Result<Reading, String> {
        if self.broken.iter().any(|b| b == id) {
            return Err(format!("{id} is broken"));
        }
        let audio = &self.get(id).audio;
        Ok(Reading::new(audio, from_ms * audio.format().rate as i64 / 1000))
    }

    fn about(&self, id: &str) -> WindowSong {
        self.get(id).window_song()
    }
}

/// Appends the part of `data` (starting at heard frame `at`) inside `capture`.
fn keep(heard: &mut Vec<u8>, capture: &Range<u64>, at: u64, data: &[u8], fb: usize) {
    let n = (data.len() / fb) as u64;
    let from = capture.start.clamp(at, at + n);
    let to = capture.end.clamp(from, at + n);
    heard.extend_from_slice(&data[(from - at) as usize * fb..(to - at) as usize * fb]);
}

/// Written audio and the song frames it covers.
struct Piece {
    data: Vec<u8>,
    pos: usize,
    media: f64,
}

impl Piece {
    /// Takes `n` bytes off the front with their share of song time.
    fn take(&mut self, n: usize) -> (&[u8], f64) {
        let left = self.data.len() - self.pos;
        let media = if left == 0 { 0.0 } else { self.media * n as f64 / left as f64 };
        self.media -= media;
        let from = self.pos;
        self.pos += n;
        (&self.data[from..from + n], media)
    }
}

/// An AudioTrack on the virtual clock, recording what was played. Its play head moves only between
/// turns, so everything unplayed can be replaced.
pub struct AudioTrack {
    format: Option<Format>,
    pieces: VecDeque<Piece>,
    queued_bytes: usize,
    played_media: f64,
    /// Frames played since the flush.
    played: u64,
    /// What a cut dropped, blended into what is written next, and how much of it was blended.
    blend: Option<(Vec<u8>, usize)>,
    playing: bool,
    clock_us: i64,
    clock_frames: u64,
    started: bool,
    /// Every frame played, including silence where it ran dry (limited to `capture`).
    pub heard: Vec<u8>,
    /// Which played frames `heard` keeps.
    pub capture: Range<u64>,
    pub heard_frames: u64,
    /// Underruns mid-song: (frame, silent frames).
    pub gaps: Vec<(u64, u64)>,
}

impl AudioTrack {
    pub fn new() -> AudioTrack {
        AudioTrack {
            format: None,
            pieces: VecDeque::new(),
            queued_bytes: 0,
            played_media: 0.0,
            played: 0,
            blend: None,
            playing: false,
            clock_us: 0,
            clock_frames: 0,
            started: false,
            heard: Vec::new(),
            capture: 0..u64::MAX,
            heard_frames: 0,
            gaps: Vec::new(),
        }
    }

    /// `heard` as 16-bit samples.
    pub fn heard_samples(&self) -> Vec<i16> {
        samples(&self.heard)
    }

    /// `heard` as float samples.
    pub fn heard_floats(&self) -> Vec<f32> {
        self.heard.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
    }

    /// Plays `us` of the clock; running dry records a gap unless `quiet` (the music is over).
    fn advance(&mut self, us: i64, quiet: bool) {
        let Some(f) = self.format.filter(|_| self.playing) else { return };
        let fb = f.frame_bytes();
        self.clock_us += us;
        let due_total = (self.clock_us as i128 * f.rate as i128 / 1_000_000) as u64;
        let mut due = due_total - self.clock_frames;
        self.clock_frames = due_total;
        while due > 0 {
            let Some(p) = self.pieces.front_mut() else { break };
            let n = ((p.data.len() - p.pos) / fb).min(due as usize);
            let (data, media) = p.take(n * fb);
            keep(&mut self.heard, &self.capture, self.heard_frames, data, fb);
            let done = p.pos == p.data.len();
            self.played_media += media;
            self.played += n as u64;
            self.queued_bytes -= n * fb;
            self.heard_frames += n as u64;
            due -= n as u64;
            self.started = true;
            if done {
                self.pieces.pop_front();
            }
        }
        if due > 0 && self.started && !quiet {
            self.gaps.push((self.heard_frames, due));
            let from = self.capture.start.clamp(self.heard_frames, self.heard_frames + due);
            let to = self.capture.end.clamp(from, self.heard_frames + due);
            self.heard.resize(self.heard.len() + (to - from) as usize * fb, 0);
            self.heard_frames += due;
        }
    }
}

impl Default for AudioTrack {
    fn default() -> Self {
        AudioTrack::new()
    }
}

impl pipeline::Track for AudioTrack {
    fn open(&mut self, format: Format) {
        self.format = Some(format);
    }

    /// An AudioTrack has one format.
    fn must_reopen(&mut self, format: Format) -> bool {
        self.format.is_some_and(|f| f != format)
    }

    fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    fn write(&mut self, data: &[u8], media: f64) {
        self.queued_bytes += data.len();
        let mut data = data.to_vec();
        if let (Some((old, done)), Some(f)) = (self.blend.as_mut(), self.format) {
            let w = f.encoding.width();
            let total = old.len() / w;
            let n = (data.len() / w).min(total - *done);
            for (k, new) in data.chunks_exact_mut(w).take(n).enumerate() {
                let k = *done + k;
                let frame = k / f.channels;
                let frames = total / f.channels;
                let o = &old[k * w..k * w + w];
                if w == 2 {
                    let v = pipeline::blended(i16::from_le_bytes([o[0], o[1]]) as f32, i16::from_le_bytes([new[0], new[1]]) as f32, frame, frames);
                    new.copy_from_slice(&(v.round() as i16).to_le_bytes());
                } else {
                    let v = pipeline::blended(f32::from_le_bytes([o[0], o[1], o[2], o[3]]), f32::from_le_bytes([new[0], new[1], new[2], new[3]]), frame, frames);
                    new.copy_from_slice(&v.to_le_bytes());
                }
            }
            *done += n;
            if *done == total {
                self.blend = None;
            }
        }
        self.pieces.push_back(Piece { data, pos: 0, media });
    }

    fn played_media(&mut self) -> f64 {
        self.played_media
    }

    fn played(&mut self) -> u64 {
        self.played
    }

    fn freeze(&mut self) -> u64 {
        self.played
    }

    fn cut(&mut self, at: u64) -> f64 {
        let Some(f) = self.format else { return self.played_media };
        let fb = f.frame_bytes();
        let blend = (f.rate as i64 * pipeline::BLEND_US / 1_000_000) as usize * fb;
        let mut dropped = Vec::new();
        let (mut frame, mut media) = (self.played, self.played_media);
        for p in self.pieces.iter_mut() {
            let left = ((p.data.len() - p.pos) / fb) as u64;
            if frame + left <= at {
                frame += left;
                media += p.media;
                continue;
            }
            let keep = (at.max(frame) - frame) as usize * fb;
            let cut_from = p.pos + keep;
            if dropped.len() < blend {
                let n = (blend - dropped.len()).min(p.data.len() - cut_from);
                dropped.extend_from_slice(&p.data[cut_from..cut_from + n]);
            }
            let share = if left == 0 { 0.0 } else { p.media * (keep / fb) as f64 / left as f64 };
            self.queued_bytes -= p.data.len() - cut_from;
            p.data.truncate(cut_from);
            p.media = share;
            media += share;
            frame = frame.max(at);
        }
        self.pieces.retain(|p| p.pos < p.data.len());
        self.blend = (!dropped.is_empty()).then_some((dropped, 0));
        media
    }

    fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    fn flush(&mut self) {
        self.pieces.clear();
        self.queued_bytes = 0;
        self.played_media = 0.0;
        self.played = 0;
        self.blend = None;
        self.started = false;
    }

    fn play(&mut self) {
        self.playing = true;
    }

    fn pause(&mut self) {
        self.playing = false;
    }
}

impl Sink<AudioTrack> {
    /// The AudioTrack plays `us` of the clock.
    pub fn advance(&mut self, us: i64) {
        let quiet = self.source_ended && !self.pending();
        self.track.advance(us, quiet);
    }
}

/// Tests read the track's recording through the sink.
impl Deref for Sink<AudioTrack> {
    type Target = AudioTrack;
    fn deref(&self) -> &AudioTrack {
        &self.track
    }
}

impl DerefMut for Sink<AudioTrack> {
    fn deref_mut(&mut self) -> &mut AudioTrack {
        &mut self.track
    }
}

/// The simulated app: planner (as `crates/automix/src/planner.rs`), analysis store and log.
pub struct App {
    pub prefs: TransitionPrefs,
    pub transitions_off: bool,
    pub window: Vec<WindowSong>,
    pub shuffling: bool,
    pub analyses: std::collections::HashMap<String, TrackAnalysis>,
    pub log: Vec<String>,
    /// Every sound change, as [`pipeline::App::spliced`] said it.
    pub splices: Vec<pipeline::Splice>,
    pub now_ms: i64,
    /// The last "no transition" answer, logged once.
    none: Option<(String, Option<Skip>)>,
    /// Analyse the playing song (slow in debug builds).
    pub measure_playing: bool,
    /// Per-song ReplayGain; 1 if absent.
    pub gains: std::collections::HashMap<String, f32>,
    /// Output devices seen, and per-device sounds.
    pub outputs: Vec<String>,
    pub device_sounds: std::collections::HashMap<String, Sound>,
}

/// No crossfade, no AutoMix: gapless.
pub fn prefs_off() -> TransitionPrefs {
    TransitionPrefs {
        auto_mix: false,
        crossfade_s: 0,
        auto_mix_max_s: 16,
        beat_match: true,
        max_tempo_change_pct: 6.0,
        bass_swap: true,
        filter_effects: true,
        echo_out: true,
        keep_pitch: true,
        keep_albums: true,
        replay_gain: false,
        fade_curve: crate::types::FadeCurve::EqualPower,
        fade_in_ms: 0,
        fade_out_ms: 0,
    }
}

impl App {
    pub fn new() -> App {
        App {
            prefs: prefs_off(),
            transitions_off: false,
            window: Vec::new(),
            shuffling: false,
            analyses: Default::default(),
            log: Vec::new(),
            splices: Vec::new(),
            now_ms: 0,
            none: None,
            measure_playing: false,
            gains: Default::default(),
            outputs: Vec::new(),
            device_sounds: Default::default(),
        }
    }

    pub fn logged(&self, what: &str) -> bool {
        self.log.iter().any(|l| l.contains(what))
    }

    /// Stores a finished analysis if it covers the whole song.
    fn store(&mut self, id: &str, a: Analyzer, frames: u64, rate: u32, how: &str) {
        let expected = self.window.iter().find(|s| s.id == id).map_or(0, |s| s.duration_ms);
        let heard_ms = (frames * 1000 / rate.max(1) as u64) as i64;
        let mut a = a;
        if !whole_song(heard_ms, expected) || a.samples() < (a.rate() * 30.0) as u64 {
            self.log.push(format!("analysed {id}{how}: not stored: heard {heard_ms} ms of {expected} ms"));
            return;
        }
        let t = crate::automix::finish(id, &a.take_features()).track;
        self.log.push(format!("analysed {id}{how}: {:.2} bpm (conf {:.2}, stab {:.2})", t.bpm, t.bpm_confidence, t.stability));
        self.analyses.insert(id.to_string(), t);
    }
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

impl Host for App {
    fn plan_for(&mut self, outgoing_id: &str) -> Option<Plan> {
        let chosen = match pick(&self.prefs, self.transitions_off, &self.window, outgoing_id, self.shuffling) {
            Err(skip) => {
                let repeated = self.none.as_ref().is_some_and(|(id, s)| *s == Some(skip) && id == outgoing_id);
                if !repeated {
                    self.log.push(format!("planFor: {}", skip.describe(outgoing_id)));
                }
                self.none = Some((outgoing_id.to_string(), Some(skip)));
                return None;
            }
            Ok(p) => p,
        };
        let (o, n) = (&self.window[chosen.out], &self.window[chosen.next]);
        let (a, b) = if self.prefs.auto_mix { (self.analyses.get(&o.id), self.analyses.get(&n.id)) } else { (None, None) };
        let mut t = plan::plan(a, b, o.duration_ms, n.duration_ms, &chosen.settings);
        crate::transitions::shape_crossfade(&self.prefs, &mut t);
        let p = engine_plan(&t, &n.id);
        let line = match &p {
            None => format!("planFor: gapless ({})", t.reason),
            Some(_) => format!("transition {} -> {}: {:?} {} ms at {}, tempo x{:.3} ({})", o.title, n.title, t.kind, t.duration_ms, t.out_start_ms, t.tempo_ratio, t.reason),
        };
        let repeated = p.is_none() && self.none.as_ref().is_some_and(|(id, s)| s.is_none() && id == outgoing_id);
        if !repeated {
            self.log.push(line);
        }
        self.none = p.is_none().then(|| (outgoing_id.to_string(), None));
        p
    }

    fn wants_analysis(&mut self, song_id: &str) -> Option<u64> {
        if !self.prefs.auto_mix || !self.measure_playing || self.analyses.contains_key(song_id) {
            return None;
        }
        Some(self.window.iter().find(|s| s.id == song_id).map_or(0, |s| s.duration_ms.max(0) as u64))
    }

    fn analysed(&mut self, song_id: &str, analyzer: Analyzer, _channels: usize, frames: u64, rate: u32) {
        self.store(song_id, analyzer, frames, rate, "");
    }

    fn log(&mut self, message: &str) {
        self.log.push(message.to_string());
    }

    fn now_ms(&self) -> i64 {
        self.now_ms
    }
}

impl pipeline::App for App {
    fn clock(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
    }

    fn auto_mix(&self) -> bool {
        self.prefs.auto_mix
    }

    fn window(&mut self, window: Vec<WindowSong>, shuffling: bool) {
        self.window = window;
        self.shuffling = shuffling;
    }

    /// Analyses the unmeasured `ids` whole.
    fn measure_ahead<S: Songs>(&mut self, songs: &mut S, ids: &[String]) {
        let missing: Vec<&String> = ids.iter().filter(|id| !self.analyses.contains_key(*id)).collect();
        self.log.push(format!("measuring ahead: {} of {} unmeasured, 0 not on the device yet", missing.len(), ids.len()));
        for id in missing {
            let Ok(mut r) = songs.open(id, 0) else { continue };
            let f = r.format();
            let mut a = Analyzer::new(f.rate, (r.duration_us() / 1000) as u64);
            while r.fill() {
                let s = samples(r.buffer());
                a.feed_interleaved(&s, f.channels, |v| v as f32 / 32768.0);
            }
            let frames = a.samples();
            let rate = a.rate() as u32;
            self.store(id, a, frames, rate, " ahead");
        }
    }

    fn transitions_off(&mut self, off: bool) {
        self.transitions_off = off;
    }

    fn output_changed(&mut self, kind: crate::outputs::OutputKind, name: &str) -> Option<(String, Option<Sound>)> {
        let key = crate::outputs::key(kind, name);
        self.outputs.push(key.clone());
        let sound = self.device_sounds.get(&key).cloned();
        Some((key, sound))
    }

    fn gain(&mut self, _index: usize, id: &str) -> f32 {
        self.gains.get(id).copied().unwrap_or(1.0)
    }

    fn spliced(&mut self, what: &str, at: pipeline::Splice) {
        self.splices.push(at);
        self.log.push(format!("the {what} changes from output frame {} (input frame {})", at.output, at.input));
    }
}

/// The pipeline player over the simulated track.
pub type Player = pipeline::Player<Tracks, AudioTrack, App, Playlist>;

impl Player {
    /// A queue of `tracks`, idle.
    pub fn new(tracks: Vec<Track>) -> Player {
        let mut queue = Playlist::default();
        queue.set(tracks.iter().map(|t| t.id.clone()).collect(), Some(0), false, 0);
        Player::build(Tracks { list: tracks, broken: Vec::new() }, queue, App::new(), AudioTrack::new())
    }

    /// [`Player::new`] with `prefs`.
    pub fn with_prefs(tracks: Vec<Track>, prefs: TransitionPrefs) -> Player {
        let mut p = Player::new(tracks);
        p.app.prefs = prefs;
        p
    }

    /// Marks the whole queue as one album run (queued from the album page).
    pub fn as_album(mut self) -> Player {
        let n = self.queue.len();
        self.queue.as_album(0, n);
        self.queue_changed();
        self
    }

    /// Shuffled with `seed`, starting where that order starts.
    pub fn shuffled(tracks: Vec<Track>, seed: u64) -> Player {
        let mut p = Player::new(tracks);
        let ids = p.tracks.iter().map(|t| t.id.clone()).collect();
        p.queue.set(ids, None, true, seed);
        p.queue_changed();
        p
    }

    /// The track at list index `i`.
    pub fn track(&self, i: usize) -> &Track {
        self.tracks.get(&self.queue.ids()[i])
    }

    /// New transition settings: replan, and measure ahead with AutoMix on.
    pub fn set_prefs(&mut self, prefs: TransitionPrefs) {
        self.app.prefs = prefs;
        self.engine.replan();
        if prefs.auto_mix && self.measure_on_move {
            self.measure_ahead();
        }
    }

    /// New equalizer and effects, the rest of the chain as it is.
    pub fn set_sound(&mut self, sound: Sound) {
        let c = pipeline::ChainSettings { sound, ..self.sink.settings().clone() };
        self.set_chain(c);
    }

    pub fn set_speed(&mut self, speed: f32, pitch: f32) {
        let c = pipeline::ChainSettings { speed, pitch, ..self.sink.settings().clone() };
        self.set_chain(c);
    }

    pub fn set_skip_silence(&mut self, on: bool) {
        let c = pipeline::ChainSettings { skip_silence: on, ..self.sink.settings().clone() };
        self.set_chain(c);
    }

    /// Advances the clock one step and runs a render turn.
    pub fn step(&mut self) {
        let now = self.now_ms + STEP_MS;
        self.sink.advance(STEP_MS * 1000);
        self.turn(now);
    }

    /// Runs the clock `ms` on.
    pub fn run_for(&mut self, ms: i64) {
        let until = self.now_ms + ms;
        while self.now_ms < until {
            self.step();
        }
    }

    /// Runs until `done` or `max_ms`; returns whether `done` fired.
    pub fn run_until(&mut self, max_ms: i64, mut done: impl FnMut(&mut Player) -> bool) -> bool {
        let until = self.now_ms + max_ms;
        while self.now_ms < until {
            self.step();
            if done(self) {
                return true;
            }
        }
        false
    }

    /// Plays to the end of the queue (at most `max_ms`).
    pub fn run_to_end(&mut self, max_ms: i64) -> bool {
        self.run_until(max_ms, |p| p.ended())
    }
}
