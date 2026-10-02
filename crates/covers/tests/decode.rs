//! Decoders and scaler against Pillow's output (testdata/make.py).

use nori_covers::{header, Alpha, Decoder, Format, Target};

fn file(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn decode(name: &str, w: usize, h: usize, alpha: Alpha) -> Vec<u8> {
    Decoder::new().decode(&file(name), w, h, alpha).unwrap()
}

/// Mean and max absolute channel difference.
fn diff(a: &[u8], b: &[u8]) -> (f64, u8) {
    assert_eq!(a.len(), b.len());
    let d = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y));
    (d.clone().map(f64::from).sum::<f64>() / a.len() as f64, d.max().unwrap())
}

#[track_caller]
fn close(name: &str, got: &[u8], want: &[u8], mean: f64, max: u8) {
    let (m, x) = diff(got, want);
    assert!(m <= mean && x <= max, "{name}: mean {m:.2} (at most {mean}), largest {x} (at most {max})");
}

#[test]
fn decoders_match() {
    // The GIF is its first frame.
    for name in ["alpha.png", "alpha.webp", "palette.png", "photo.gif"] {
        close(name, &decode(name, 40, 30, Alpha::Straight), &file(&format!("{name}.rgba")), 0.0, 0);
    }
    let photo = decode("photo.png", 40, 30, Alpha::Straight);
    assert!(photo.chunks_exact(4).all(|p| p[3] == 255));

    // Lossy formats match reference decoders.
    // JPEGs differ from libjpeg-turbo only in chroma upsampling.
    for (name, mean, max) in [("photo.jpg", 1.0, 12), ("photo-progressive.jpg", 1.0, 12), ("grey.jpg", 0.5, 2), ("photo.webp", 1.5, 16)] {
        close(name, &decode(name, 40, 30, Alpha::Straight), &file(&format!("{name}.rgba")), mean, max);
    }

    // Reused decoder matches fresh one.
    let mut d = Decoder::new();
    for name in ["photo.jpg", "alpha.png", "photo.webp", "grey.jpg", "palette.png", "photo.gif", "turned-6.jpg", "photo.jpg"] {
        for side in [7, 12, 30, 64] {
            let px = d.decode(&file(name), side, side, Alpha::Premultiplied).unwrap();
            let fresh = Decoder::new().decode(&file(name), side, side, Alpha::Premultiplied).unwrap();
            assert!(px == fresh, "{name} at {side}x{side}");
        }
    }
}

#[test]
fn premultiplied_is_times_alpha() {
    let straight = decode("alpha.png", 40, 30, Alpha::Straight);
    let pre = decode("alpha.png", 40, 30, Alpha::Premultiplied);
    for (s, p) in straight.chunks_exact(4).zip(pre.chunks_exact(4)) {
        let a = s[3] as u32;
        assert_eq!(p, [0, 1, 2].map(|i| ((s[i] as u32 * a + 127) / 255) as u8).iter().chain([&s[3]]).copied().collect::<Vec<_>>());
    }
}

#[test]
fn scaling() {
    // 40x30 into 12x12 is the middle 30x30 at 2.5:1 (area average); 50x50 is Pillow's bilinear. The JPEG
    // is a half-size IDCT then an area average, against libjpeg-turbo's half-size decode averaged the
    // same way; loose, as this picture is small and busy (real covers differ by about one step).
    for (name, picture, side, reference, mean, max) in
        [("area", "photo.png", 12, "photo-12-area.rgba", 0.1, 1), ("bilinear", "photo.png", 50, "photo-50-bilinear.rgba", 0.1, 1), ("idct", "photo.jpg", 12, "photo.jpg-12-half.rgba", 4.5, 32)]
    {
        close(name, &decode(picture, side, side, Alpha::Straight), &file(reference), mean, max);
    }

    // Jpeg without idct scaling is exact average.
    let mut d = Decoder::new();
    d.set_idct_scaling(false);
    let d = d.decode(&file("photo.jpg"), 12, 12, Alpha::Straight).unwrap();
    close("whole", &d, &file("photo.jpg-12-area.rgba"), 0.5, 3);

    // Decodes into padded rows.
    let want = decode("photo.jpg", 40, 30, Alpha::Premultiplied);
    let stride = 40 * 4 + 16;
    let mut px = vec![0xAB; stride * 30];
    Decoder::new().decode_into(&file("photo.jpg"), Target { px: &mut px, width: 40, height: 30, stride }, Alpha::Premultiplied).unwrap();
    for y in 0..30 {
        close("row", &px[y * stride..][..160], &want[y * 160..][..160], 0.0, 0);
        assert!(px[y * stride + 160..][..16].iter().all(|&b| b == 0xAB));
    }
}

#[test]
fn broken_files_error_no_panic() {
    let mut d = Decoder::new();
    for name in ["photo.jpg", "photo.png", "photo.webp", "alpha.webp", "photo.gif", "turned-6.jpg", "turned-6.webp"] {
        let f = file(name);
        // Truncated in the headers: an error. Later truncation or corruption: anything but a panic.
        for cut in [4, 20] {
            assert!(d.decode(&f[..cut], 16, 16, Alpha::Straight).is_err(), "{name} cut at {cut} bytes decoded");
        }
        assert!(header(&f[..4]).is_err(), "{name} cut at 4 bytes has a header");
        for cut in [f.len() / 2, f.len() - 3] {
            let _ = d.decode(&f[..cut], 16, 16, Alpha::Straight);
            let _ = header(&f[..cut]);
        }
        let mut junk = f.clone();
        for b in junk.iter_mut().skip(30).step_by(7) {
            *b ^= 0x5A;
        }
        let _ = d.decode(&junk, 16, 16, Alpha::Straight);
        let _ = header(&junk);
    }
}

#[test]
fn gif_frames() {
    let px = decode("part.gif", 40, 30, Alpha::Premultiplied);
    for y in 0..30 {
        for x in 0..40 {
            let p = &px[(y * 40 + x) * 4..][..4];
            let inside = (8..28).contains(&x) && (6..16).contains(&y);
            assert_eq!(p, if inside { [250, 40, 60, 255] } else { [0, 0, 0, 0] }, "({x}, {y})");
        }
    }

    // Gif frame off screen or too large is refused.
    let one = Decoder::new().decode(&gif(1, 1, 0, 0, 1, 1), 1, 1, Alpha::Straight).unwrap();
    assert_eq!(one, [0, 0, 0, 255], "valid builder output");
    // 1x1 screen with a 65535x65535 frame (17 GB of RGBA).
    assert!(Decoder::new().decode(&gif(1, 1, 0, 0, 65535, 65535), 1, 1, Alpha::Straight).is_err());
    // Frames past the right or bottom edge, or starting beyond it.
    for (left, top, fw, fh) in [(200, 9, 1, 1), (9, 200, 1, 1), (5, 0, 8, 1), (0, 5, 1, 8), (65535, 65535, 1, 1)] {
        let r = Decoder::new().decode(&gif(10, 10, left, top, fw, fh), 10, 10, Alpha::Straight);
        assert!(r.is_err(), "a frame {fw}x{fh} at {left},{top} on a 10x10 screen");
    }
}

/// A GIF with a `sw` x `sh` screen and one `fw` x `fh` frame at `left`, `top` holding one black pixel
/// of LZW data.
fn gif(sw: u16, sh: u16, left: u16, top: u16, fw: u16, fh: u16) -> Vec<u8> {
    let mut g = b"GIF89a".to_vec();
    g.extend([sw, sh].iter().flat_map(|v| v.to_le_bytes()));
    // Global colour table: black, white.
    g.extend([0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
    g.push(0x2C);
    g.extend([left, top, fw, fh].iter().flat_map(|v| v.to_le_bytes()));
    // No local table; 2-bit LZW: clear, 0, end.
    g.extend([0, 2, 2, 0x44, 0x01, 0, 0x3B]);
    g
}

/// Reference EXIF orientation of `w` x `h` RGBA `px`.
fn turn(px: &[u8], w: usize, h: usize, turn: u8) -> Vec<u8> {
    let quarter = turn >= 5;
    let (ow, oh) = if quarter { (h, w) } else { (w, h) };
    let mut out = Vec::with_capacity(px.len());
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = match turn {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                8 => (w - 1 - y, x),
                _ => (x, y),
            };
            out.extend_from_slice(&px[(sy * w + sx) * 4..][..4]);
        }
    }
    out
}

#[test]
fn orientation() {
    // Same pixels as photo.jpg stored with each orientation.
    let plain = decode("photo.jpg", 40, 30, Alpha::Straight);
    for o in [2u8, 3, 5, 6, 7, 8] {
        let (w, h) = if o >= 5 { (30, 40) } else { (40, 30) };
        let name = format!("turned-{o}.jpg");
        let turned = decode(&name, w, h, Alpha::Straight);
        close(&name, &turned, &turn(&plain, 40, 30, o), 0.0, 0);
        // And Pillow's exif_transpose.
        close(&name, &turned, &file(&format!("{name}.rgba")), 1.0, 12);
    }
    // PNG eXIf and WebP EXIF chunks.
    close("png", &decode("turned-6.png", 30, 40, Alpha::Straight), &turn(&decode("photo.png", 40, 30, Alpha::Straight), 40, 30, 6), 0.0, 0);
    let webp = decode("photo.webp", 40, 30, Alpha::Straight);
    close("webp", &decode("turned-6.webp", 30, 40, Alpha::Straight), &turn(&webp, 40, 30, 6), 0.0, 0);

    // Oriented picture is cropped and scaled as displayed.
    let plain = decode("photo.jpg", 30, 30, Alpha::Straight);
    close("square", &decode("turned-8.jpg", 30, 30, Alpha::Straight), &turn(&plain, 30, 30, 8), 0.0, 0);
    let mut d = Decoder::new();
    let small = d.decode(&file("turned-6.jpg"), 12, 16, Alpha::Straight).unwrap();
    close("scaled", &small, &turn(&d.decode(&file("photo.jpg"), 16, 12, Alpha::Straight).unwrap(), 16, 12, 6), 0.0, 0);

    // Header reports displayed size.
    for (name, format) in [
        ("photo.jpg", Format::Jpeg),
        ("photo-progressive.jpg", Format::Jpeg),
        ("grey.jpg", Format::Jpeg),
        ("photo.png", Format::Png),
        ("palette.png", Format::Png),
        ("photo.webp", Format::WebP),
        ("alpha.webp", Format::WebP),
        ("photo.gif", Format::Gif),
        ("part.gif", Format::Gif),
        ("turned-3.jpg", Format::Jpeg),
        ("turned-2.jpg", Format::Jpeg),
    ] {
        let h = header(&file(name)).unwrap();
        assert_eq!((h.format, h.width, h.height), (format, 40, 30), "{name}");
    }
    // 90° orientations swap width and height.
    for name in ["turned-6.jpg", "turned-8.jpg", "turned-5.jpg", "turned-7.jpg", "turned-6.png", "turned-6.webp"] {
        let h = header(&file(name)).unwrap();
        assert_eq!((h.width, h.height), (30, 40), "{name}");
    }
    assert_eq!(header(&file("turned-7.jpg")).unwrap().orientation, 7);
    assert_eq!(header(&file("photo.jpg")).unwrap().orientation, 1);
}

