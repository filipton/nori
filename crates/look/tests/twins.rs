//! Bit-exact checks against the Kotlin originals: testdata/twins/look_twins.tsv is JVM output of
//! LookTwins.kt (tools/twins.sh), floats as hex bits.

use nori_look::{cover, sleeve};

const TABLE: &str = include_str!("../testdata/twins/look_twins.tsv");

fn rows(case: &str) -> Vec<Vec<&'static str>> {
    let rows: Vec<Vec<&str>> =
        TABLE.lines().map(|l| l.split('\t').collect::<Vec<_>>()).filter(|f| f[0] == case).map(|f| f[1..].to_vec()).collect();
    assert!(!rows.is_empty(), "no vectors for {case}");
    rows
}

fn float(bits: &str) -> f32 {
    f32::from_bits(u32::from_str_radix(bits, 16).unwrap())
}

#[test]
fn palette_keys() {
    let mut out = String::new();
    for r in rows("palette_key") {
        let url = if r[0] == "_" { "" } else { r[0] };
        cover::palette_key(&mut out, url, r[1].parse().unwrap(), r[2].parse().unwrap());
        assert_eq!(out, r[3], "{r:?}");
    }
}

#[test]
fn band_keys() {
    for r in rows("band_key") {
        let want = if r[4] == "-" { -1 } else { r[4].parse().unwrap() };
        assert_eq!(sleeve::band_key(float(r[0]), float(r[1]), float(r[2]), float(r[3])), want, "{r:?}");
    }
}
