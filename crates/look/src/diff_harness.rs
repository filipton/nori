//! Differential harness: page colours, palettes and dresses derived from a set of generated covers,
//! written to `$NORI_DIFF_OUT/look.txt`.

use std::fmt::Write as _;

fn covers() -> Vec<(String, Vec<u32>, usize, usize)> {
    let mut out = Vec::new();
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for (w, h) in [(1, 1), (2, 3), (16, 16), (48, 48), (64, 40), (97, 131)] {
        out.push((format!("noise {w}x{h}"), (0..w * h).map(|_| 0xFF00_0000 | (next() as u32 & 0xFF_FFFF)).collect(), w, h));
        out.push((format!("solid {w}x{h}"), vec![0xFF20_4080; w * h], w, h));
        out.push((format!("white {w}x{h}"), vec![0xFFFF_FFFF; w * h], w, h));
        out.push((format!("black {w}x{h}"), vec![0xFF00_0000; w * h], w, h));
        let grad = (0..w * h).map(|i| {
            let (x, y) = ((i % w) as u32, (i / w) as u32);
            0xFF00_0000 | (x * 255 / w as u32) << 16 | (y * 255 / h as u32) << 8 | 0x40
        });
        out.push((format!("gradient {w}x{h}"), grad.collect(), w, h));
        let halves = (0..w * h).map(|i| if i / w < h / 2 { 0xFFD0_2020 } else { 0xFF10_1010 });
        out.push((format!("halves {w}x{h}"), halves.collect(), w, h));
        let foot = (0..w * h).map(|i| if i / w > h * 3 / 4 { 0xFFF0_E8D0 } else { 0xFF00_0000 | (next() as u32 & 0xFF_FFFF) });
        out.push((format!("foot {w}x{h}"), foot.collect(), w, h));
        let blocks = (0..w * h).map(|i| [0xFF10_60C0, 0xFF10_60C8, 0xFFE0_E0E0, 0xFF08_0808][(i % w * 4 / w.max(1) + i / w * 2 / h.max(1)) % 4]);
        out.push((format!("blocks {w}x{h}"), blocks.collect(), w, h));
    }
    out
}

#[test]
fn transcribe() {
    let Some(dir) = std::env::var_os("NORI_DIFF_OUT") else { return };
    let mut log = String::new();
    for (name, px, w, h) in covers() {
        for (dark, amoled) in [(true, false), (false, false), (true, true)] {
            let c = crate::cover::derive(&px, w, h, dark, amoled);
            let _ = writeln!(log, "{name} {dark} {amoled} {c:?}");
        }
        let p = crate::palette::generate(&px, w, h, 16);
        let _ = writeln!(log, "{name} palette {p:?}");
    }
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(std::path::Path::new(&dir).join("look.txt"), log).unwrap();
}
