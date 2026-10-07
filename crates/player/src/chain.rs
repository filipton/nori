//! The processors between the transition engine and the track, and the input they were given while
//! the track may still be asked to play what they made of it. With the processors' state kept every
//! [`MARK_FRAMES`], their output can be made again from any frame the track holds: a sound change
//! starts where the track can still change, continuing the old sound's state there.

use std::collections::VecDeque;

use crate::dsp::Equalizer;
use crate::pcm::Encoding;
use crate::silence::SilenceSkipper;
use crate::sing::Masker;
use crate::speed::SpeedPitch;

/// Input frames between kept states: the most that is run again to reach a splice.
pub const MARK_FRAMES: u64 = 8192;

/// The chain: Sing's vocal masker, then media3's order: equalizer, silence skipping, speed.
#[derive(Default)]
pub struct Processors {
    pub sing: Option<Masker>,
    pub eq: Option<Equalizer>,
    pub silence: Option<SilenceSkipper>,
    pub speed: Option<SpeedPitch>,
}

/// `clone_from` keeps every buffer's memory.
impl Clone for Processors {
    fn clone(&self) -> Self {
        Processors { sing: self.sing.clone(), eq: self.eq.clone(), silence: self.silence.clone(), speed: self.speed.clone() }
    }

    fn clone_from(&mut self, o: &Self) {
        self.sing.clone_from(&o.sing);
        self.eq.clone_from(&o.eq);
        self.silence.clone_from(&o.silence);
        self.speed.clone_from(&o.speed);
    }
}

impl Processors {
    /// `o`'s state to keep, in this one's memory: without Sing's scratch, which `clone_from` gives back.
    fn store_from(&mut self, o: &Self) {
        self.sing = o.sing.as_ref().map(|from| match self.sing.take() {
            Some(mut m) => {
                m.store_from(from);
                m
            }
            None => from.stored(),
        });
        self.eq.clone_from(&o.eq);
        self.silence.clone_from(&o.silence);
        self.speed.clone_from(&o.speed);
    }
}

/// Input as offered: its first frame, song frames per frame, timeline position (µs) of that frame, and
/// the gain the chain applies to it (1 when the samples are at their level).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    pub frame: u64,
    pub pace: f64,
    pub pts: i64,
    pub gain: f32,
}

impl Piece {
    /// Timeline position (µs) and pace of input frame `frame` of this piece.
    pub fn at(&self, frame: u64, rate: u32) -> (i64, f64) {
        (self.pts + ((frame - self.frame) as f64 * self.pace * 1e6 / rate as f64) as i64, self.pace)
    }
}

/// The processors' state before input frame `frame`, when they had made `out` frames of output from
/// `media` song frames of input.
pub struct Mark {
    pub frame: u64,
    pub out: u64,
    pub media: f64,
    pub chain: Processors,
}

/// Input kept since the flush, from the oldest mark the track may still need.
#[derive(Default)]
pub struct Kept {
    /// Frames from `first`, starting at `bytes[head]`.
    bytes: Vec<u8>,
    head: usize,
    first: u64,
    frame_bytes: usize,
    pieces: VecDeque<Piece>,
    marks: VecDeque<Mark>,
    /// Marks let go, reused so keeping one does not allocate.
    spare: Vec<Processors>,
}

impl Kept {
    /// Forgets everything; frames count from `first`, `frame_bytes` each. Makes room for `frames` of
    /// input and their marks at once, rather than growing on the buffer path.
    pub fn restart(&mut self, first: u64, frame_bytes: usize, frames: u64) {
        self.bytes.clear();
        self.head = 0;
        self.first = first;
        self.frame_bytes = frame_bytes.max(1);
        self.pieces.clear();
        while let Some(m) = self.marks.pop_front() {
            self.spare.push(m.chain);
        }
        self.bytes.reserve(frames as usize * self.frame_bytes);
        let marks = (frames / MARK_FRAMES) as usize + 2;
        self.marks.reserve(marks);
        self.spare.reserve(marks);
    }

    /// The frame after the last kept.
    pub fn end(&self) -> u64 {
        self.first + ((self.bytes.len() - self.head) / self.frame_bytes) as u64
    }

    /// Keeps `input` at the end.
    pub fn keep(&mut self, input: &[u8], pace: f64, pts: i64, gain: f32) {
        let frame = self.end();
        self.pieces.push_back(Piece { frame, pace, pts, gain });
        self.bytes.extend_from_slice(input);
    }

    /// Keeps `chain`'s state before `frame` if the last mark is [`MARK_FRAMES`] back (or there is none);
    /// returns whether it did.
    pub fn mark(&mut self, frame: u64, out: u64, media: f64, chain: &Processors) -> bool {
        if self.marks.back().is_some_and(|m| frame < m.frame + MARK_FRAMES) {
            return false;
        }
        self.push_mark(frame, out, media, chain);
        true
    }

    /// Keeps `chain`'s state before `frame` as it changed there, in place of any mark from `frame` on.
    pub fn mark_changed(&mut self, frame: u64, out: u64, media: f64, chain: &Processors) {
        while self.marks.back().is_some_and(|m| m.frame >= frame) {
            let m = self.marks.pop_back().expect("checked");
            self.spare.push(m.chain);
        }
        self.push_mark(frame, out, media, chain);
    }

    fn push_mark(&mut self, frame: u64, out: u64, media: f64, chain: &Processors) {
        let mut kept = self.spare.pop().unwrap_or_default();
        kept.store_from(chain);
        self.marks.push_back(Mark { frame, out, media, chain: kept });
    }

    /// Lets go of what comes before the last mark at or before output frame `out`.
    pub fn trim(&mut self, out: u64) {
        while self.marks.len() >= 2 && self.marks[1].out <= out {
            let m = self.marks.pop_front().expect("two marks");
            self.spare.push(m.chain);
        }
        let Some(from) = self.marks.front().map(|m| m.frame) else { return };
        if from > self.first {
            self.head += (from - self.first) as usize * self.frame_bytes;
            self.first = from;
        }
        while self.pieces.len() >= 2 && self.pieces[1].frame <= from {
            self.pieces.pop_front();
        }
        if self.head > self.bytes.len() / 2 {
            self.bytes.copy_within(self.head.., 0);
            self.bytes.truncate(self.bytes.len() - self.head);
            self.head = 0;
        }
    }

    pub fn has_marks(&self) -> bool {
        !self.marks.is_empty()
    }

    /// The last mark that `f` holds for.
    pub fn mark_where(&self, f: impl Fn(&Mark) -> bool) -> Option<usize> {
        self.marks.iter().rposition(f)
    }

    pub fn mark_at(&self, k: usize) -> &Mark {
        &self.marks[k]
    }

    /// Timeline position of the last input kept.
    pub fn last_pts(&self) -> Option<i64> {
        self.pieces.back().map(|p| p.pts)
    }

    /// Lets go of the marks after input frame `frame`.
    pub fn forget_after(&mut self, frame: u64) {
        while self.marks.back().is_some_and(|m| m.frame > frame) {
            let m = self.marks.pop_back().expect("checked");
            self.spare.push(m.chain);
        }
    }

    /// Lets go of the input from frame `frame` on.
    pub fn forget_from(&mut self, frame: u64) {
        self.forget_after(frame);
        let frame = frame.clamp(self.first, self.end());
        self.bytes.truncate(self.head + (frame - self.first) as usize * self.frame_bytes);
        while self.pieces.back().is_some_and(|p| p.frame >= frame) {
            self.pieces.pop_back();
        }
    }

    /// Song frames of the input from frame `frame` on.
    pub fn media_from(&self, frame: u64) -> f64 {
        let mut media = 0.0;
        for (k, p) in self.pieces.iter().enumerate() {
            let end = self.pieces.get(k + 1).map_or(self.end(), |n| n.frame);
            if end > frame {
                media += (end - p.frame.max(frame)) as f64 * p.pace;
            }
        }
        media
    }

    /// The input frame at timeline position `pts` (as a song's frames are stamped: whole µs, rounded
    /// down) within an unstretched piece, or the end of the input.
    pub fn frame_at(&self, pts: i64, rate: u32) -> Option<u64> {
        let k = self.pieces.partition_point(|p| p.pts <= pts).checked_sub(1)?;
        let p = self.pieces[k];
        let end = self.pieces.get(k + 1).map_or(self.end(), |n| n.frame);
        let d = ((pts - p.pts) as f64 * rate as f64 / 1_000_000.0).round() as u64;
        (p.pace == 1.0 && p.frame + d <= end).then_some(p.frame + d)
    }

    /// The piece holding input frame `frame`, and the frame after it.
    pub fn piece(&self, frame: u64) -> Option<(Piece, u64)> {
        let k = self.pieces.partition_point(|p| p.frame <= frame).checked_sub(1)?;
        let end = self.pieces.get(k + 1).map_or(self.end(), |p| p.frame);
        (frame < end).then_some((self.pieces[k], end))
    }

    /// Input frames `from..to`.
    pub fn frames(&self, from: u64, to: u64) -> &[u8] {
        let at = |f: u64| self.head + (f - self.first) as usize * self.frame_bytes;
        &self.bytes[at(from)..at(to)]
    }

    /// Scales the input from frame `from` whose timeline position is in `pts` by `ratio`: its gain, or
    /// the samples where they carry it.
    pub fn rescale(&mut self, from: u64, pts: std::ops::Range<i64>, ratio: f32, encoding: Encoding, rate: u32) {
        let mut k = 0;
        while k < self.pieces.len() {
            let p = self.pieces[k];
            let end = self.pieces.get(k + 1).map_or(self.end(), |n| n.frame);
            k += 1;
            if end <= from || !pts.contains(&p.pts) {
                continue;
            }
            if p.gain == 1.0 {
                let at = |f: u64| self.head + (f.max(self.first) - self.first) as usize * self.frame_bytes;
                crate::pcm::scale(&mut self.bytes[at(p.frame.max(from))..at(end)], encoding, ratio);
            } else if p.frame < from {
                // What comes before `from` is run again at the gain it was run at.
                self.pieces.insert(k, Piece { frame: from, pts: p.at(from, rate).0, gain: p.gain * ratio, ..p });
                k += 1;
            } else {
                self.pieces[k - 1].gain *= ratio;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: u64, from: u8) -> Vec<u8> {
        (0..n * 4).map(|i| from.wrapping_add(i as u8)).collect()
    }

    #[test]
    fn trimmed_to_mark_before_played() {
        let mut k = Kept::default();
        k.restart(0, 4, 0);
        let chain = Processors::default();
        for i in 0..4u64 {
            k.mark(i * MARK_FRAMES, i * MARK_FRAMES, 0.0, &chain);
            k.keep(&frames(MARK_FRAMES, i as u8), 1.0, i as i64 * 1000, 1.0);
        }
        k.trim(2 * MARK_FRAMES + 5);
        assert_eq!(k.mark_at(0).frame, 2 * MARK_FRAMES, "nothing kept before the mark still needed");
        assert_eq!(k.frames(2 * MARK_FRAMES, 2 * MARK_FRAMES + 1), &frames(1, 2)[..], "the input kept is the input given");
        assert_eq!(k.end(), 4 * MARK_FRAMES);
        assert_eq!(k.piece(3 * MARK_FRAMES + 1), Some((Piece { frame: 3 * MARK_FRAMES, pace: 1.0, pts: 3000, gain: 1.0 }, 4 * MARK_FRAMES)));
    }
}
