//! An MP4 audio track's encoder delay and padding, which symphonia does not read: from iTunes'
//! `iTunSMPB` comment, else the edit list (as media3 does). Only `moov` is read.

use std::io::{self, Read, Seek, SeekFrom};

/// Frames to cut, in the track's timescale (its sample rate): `delay` at the start, and everything past
/// `delay + frames` when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gapless {
    pub delay: u64,
    pub frames: Option<u64>,
    /// All decoder frames, delay and padding included (`mdhd`): offload derives the padding from it.
    pub total: u64,
}

/// A box: its type and body range.
struct Atom {
    kind: [u8; 4],
    body: u64,
    end: u64,
}

fn atom(r: &mut (impl Read + Seek), at: u64, limit: u64) -> io::Result<Option<Atom>> {
    if at + 8 > limit {
        return Ok(None);
    }
    r.seek(SeekFrom::Start(at))?;
    let mut h = [0u8; 8];
    r.read_exact(&mut h)?;
    let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
    let kind = [h[4], h[5], h[6], h[7]];
    let (body, end) = match size {
        0 => (at + 8, limit),
        1 => {
            let mut l = [0u8; 8];
            r.read_exact(&mut l)?;
            (at + 16, at + u64::from_be_bytes(l))
        }
        n => (at + 8, at + n),
    };
    if end < body || end > limit {
        return Ok(None);
    }
    Ok(Some(Atom { kind, body, end }))
}

/// The boxes directly inside `from..to`.
fn children(r: &mut (impl Read + Seek), from: u64, to: u64) -> io::Result<Vec<Atom>> {
    let mut out = Vec::new();
    let mut at = from;
    while let Some(a) = atom(r, at, to)? {
        at = a.end;
        out.push(a);
    }
    Ok(out)
}

fn find<'a>(atoms: &'a [Atom], kind: &[u8; 4]) -> Option<&'a Atom> {
    atoms.iter().find(|a| &a.kind == kind)
}

fn read_at(r: &mut (impl Read + Seek), at: u64, n: usize) -> io::Result<Vec<u8>> {
    r.seek(SeekFrom::Start(at))?;
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn be32(b: &[u8], at: usize) -> u64 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as u64
}

fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().expect("eight bytes"))
}

/// A `mvhd` or `mdhd` box's timescale and duration.
fn header_times(r: &mut (impl Read + Seek), a: &Atom) -> io::Result<(u64, u64)> {
    let b = read_at(r, a.body, (a.end - a.body).min(32) as usize)?;
    Ok(if b.first() == Some(&1) { (be32(&b, 20), be64(&b, 24)) } else { (be32(&b, 12), be32(&b, 16)) })
}

/// The single edit (segment duration in movie timescale, media time in track timescale); None unless
/// there is exactly one, as in media3.
fn edit(r: &mut (impl Read + Seek), elst: &Atom) -> io::Result<Option<(u64, i64)>> {
    let b = read_at(r, elst.body, (elst.end - elst.body).min(40) as usize)?;
    if b.len() < 8 || be32(&b, 4) != 1 {
        return Ok(None);
    }
    Ok(match b[0] {
        1 if b.len() >= 24 => Some((be64(&b, 8), be64(&b, 16) as i64)),
        0 if b.len() >= 16 => Some((be32(&b, 8), be32(&b, 12) as u32 as i32 as i64)),
        _ => None,
    })
}

/// Parses iTunes' ` 00000000 00000840 000001CA 0000000000059E36 ...` into delay and padding.
pub fn smpb(text: &str) -> Option<(u64, u64)> {
    let mut parts = text.split_whitespace();
    parts.next()?;
    let delay = u64::from_str_radix(parts.next()?, 16).ok()?;
    let padding = u64::from_str_radix(parts.next()?, 16).ok()?;
    Some((delay, padding))
}

/// The `iTunSMPB` comment in `moov/udta/meta/ilst`.
fn itunes(r: &mut (impl Read + Seek), moov: &[Atom]) -> io::Result<Option<(u64, u64)>> {
    let Some(udta) = find(moov, b"udta") else { return Ok(None) };
    let udta = children(r, udta.body, udta.end)?;
    let Some(meta) = find(&udta, b"meta") else { return Ok(None) };
    // A full box: skip version and flags.
    let meta = children(r, meta.body + 4, meta.end)?;
    let Some(ilst) = find(&meta, b"ilst") else { return Ok(None) };
    for item in children(r, ilst.body, ilst.end)?.iter().filter(|a| &a.kind == b"----") {
        let parts = children(r, item.body, item.end)?;
        let (Some(name), Some(data)) = (find(&parts, b"name"), find(&parts, b"data")) else { continue };
        let name = read_at(r, name.body + 4, (name.end - name.body).saturating_sub(4).min(64) as usize)?;
        if name != b"iTunSMPB" {
            continue;
        }
        let text = read_at(r, data.body + 8, (data.end - data.body).saturating_sub(8).min(256) as usize)?;
        return Ok(smpb(&String::from_utf8_lossy(&text)));
    }
    Ok(None)
}

/// The gapless numbers of `r` (None if not an MP4). Leaves `r`'s position anywhere.
pub fn gapless(r: &mut (impl Read + Seek)) -> io::Result<Option<Gapless>> {
    let len = r.seek(SeekFrom::End(0))?;
    if atom(r, 0, len)?.is_none_or(|a| &a.kind != b"ftyp") {
        return Ok(None);
    }
    let top = children(r, 0, len)?;
    let Some(moov) = find(&top, b"moov") else { return Ok(None) };
    let moov = children(r, moov.body, moov.end)?;
    if let Some((delay, padding)) = itunes(r, &moov)? {
        let total = audio_track(r, &moov)?.map_or(0, |t| t.duration);
        let frames = total.checked_sub(delay + padding);
        return Ok(Some(Gapless { delay, frames, total }));
    }
    let Some(track) = audio_track(r, &moov)? else { return Ok(None) };
    let Some((segment, media_time)) = track.edit.filter(|e| e.1 >= 0) else { return Ok(None) };
    let movie_scale = find(&moov, b"mvhd").map(|a| header_times(r, a)).transpose()?.map_or(1, |t| t.0.max(1));
    let scale = track.timescale.max(1);
    // As media3's AtomParsers: the edit's start is the delay, past its end the padding.
    let start = media_time as u64;
    let end = start + segment * scale / movie_scale;
    if end > track.duration || (start == 0 && end == track.duration) {
        return Ok(None);
    }
    Ok(Some(Gapless { delay: start, frames: Some(end - start), total: track.duration }))
}

struct Track {
    timescale: u64,
    duration: u64,
    edit: Option<(u64, i64)>,
}

/// The first sound track's timescale, length (`mdhd`) and single edit.
fn audio_track(r: &mut (impl Read + Seek), moov: &[Atom]) -> io::Result<Option<Track>> {
    for trak in moov.iter().filter(|a| &a.kind == b"trak") {
        let inner = children(r, trak.body, trak.end)?;
        let Some(mdia) = find(&inner, b"mdia") else { continue };
        let mdia = children(r, mdia.body, mdia.end)?;
        let (Some(hdlr), Some(mdhd)) = (find(&mdia, b"hdlr"), find(&mdia, b"mdhd")) else { continue };
        if read_at(r, hdlr.body + 8, 4)? != b"soun" {
            continue;
        }
        let (timescale, duration) = header_times(r, mdhd)?;
        let edit = match find(&inner, b"edts") {
            Some(edts) => match find(&children(r, edts.body, edts.end)?, b"elst") {
                Some(elst) => edit(r, elst)?,
                None => None,
            },
            None => None,
        };
        return Ok(Some(Track { timescale, duration, edit }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    fn file(edit: Option<(u32, i32)>, comment: Option<&str>) -> Vec<u8> {
        let mut mvhd = vec![0u8; 20];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        let mut mdhd = vec![0u8; 20];
        mdhd[12..16].copy_from_slice(&44_100u32.to_be_bytes());
        mdhd[16..20].copy_from_slice(&(1024u32 * 100).to_be_bytes());
        let mut hdlr = vec![0u8; 12];
        hdlr[8..12].copy_from_slice(b"soun");
        let mdia = [boxed(b"mdhd", &mdhd), boxed(b"hdlr", &hdlr)].concat();
        let mut trak = Vec::new();
        if let Some((d, t)) = edit {
            let elst = [&0u32.to_be_bytes()[..], &1u32.to_be_bytes(), &d.to_be_bytes(), &t.to_be_bytes(), &0x10000u32.to_be_bytes()].concat();
            trak.extend(boxed(b"edts", &boxed(b"elst", &elst)));
        }
        trak.extend(boxed(b"mdia", &mdia));
        let mut moov = [boxed(b"mvhd", &mvhd), boxed(b"trak", &trak)].concat();
        if let Some(c) = comment {
            let item = [boxed(b"mean", b"\0\0\0\0com.apple.iTunes"), boxed(b"name", b"\0\0\0\0iTunSMPB"), boxed(b"data", &[b"\0\0\0\x01\0\0\0\0".as_slice(), c.as_bytes()].concat())].concat();
            let meta = [&[0u8; 4][..], &boxed(b"ilst", &boxed(b"----", &item))].concat();
            moov.extend(boxed(b"udta", &boxed(b"meta", &meta)));
        }
        [boxed(b"ftyp", b"M4A \0\0\0\0"), boxed(b"mdat", &[0u8; 100]), boxed(b"moov", &moov)].concat()
    }

    #[test]
    fn gapless_info() {
        // 2.2 s (movie ms) from frame 1024.
        let g = gapless(&mut Cursor::new(file(Some((2200, 1024)), None))).unwrap();
        assert_eq!(g, Some(Gapless { delay: 1024, frames: Some(97_020), total: 102_400 }));
        assert_eq!(gapless(&mut Cursor::new(file(None, None))).unwrap(), None, "no edit, nothing to cut");
        assert_eq!(gapless(&mut Cursor::new(b"ID3 not an mp4 at all".to_vec())).unwrap(), None);

        // Itunes comment wins over edit list.
        let c = " 00000000 00000840 000001CA 00000000000186A0 00000000 00000000";
        let g = gapless(&mut Cursor::new(file(Some((2200, 1024)), Some(c)))).unwrap();
        assert_eq!(g, Some(Gapless { delay: 0x840, frames: Some(102_400 - 0x840 - 0x1CA), total: 102_400 }));
        assert_eq!(smpb(" 00000000 00000840 000001CA 0000000000059E36"), Some((2112, 458)));
    }

}
