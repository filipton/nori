//! How every client writes numbers and their unit symbols: times, sizes, speeds, decibels and
//! frequencies. The words around them are each client's. `decimal` is the separator a fraction is
//! written with ([`POINT`], or the phone's own); clock times and [`khz`] are always ASCII.

/// The decimal separator of the clients that do not localise numbers.
pub const POINT: &str = ".";

/// "3:07", or "1:02:03" from an hour; `minus` puts a "-" in front (the time left). Below zero is 0:00.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn clock(seconds: i64, minus: bool) -> String {
    let s = seconds.max(0);
    let sign = if minus { "-" } else { "" };
    if s >= 3600 { format!("{sign}{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{sign}{}:{:02}", s / 60, s % 60) }
}

/// `v` with `places` decimals, a "+" before a number that is not negative when `plus`. Halves round up
/// on the shortest decimal form of the number, as people (and Java's `%.nf`) do: 62.5 is "63", 0.15 is
/// "0.2".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn fixed(v: f64, places: u32, plus: bool, decimal: &str) -> String {
    localised(fixed_point(v, places as usize, plus), decimal)
}

/// A decibel figure with its sign, one decimal: "+3.5", "-1.0", and "+0.0" for nothing at all, negative
/// zero included.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn signed_db(db: f32, decimal: &str) -> String {
    fixed(if db == 0.0 { 0.0 } else { db as f64 }, 1, true, decimal)
}

/// How far the lyrics are nudged, in seconds with its sign: "+0.5", "-0.3".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn nudge(ms: i64, decimal: &str) -> String {
    fixed((ms as f32 / 1000.0) as f64, 1, true, decimal)
}

/// A band's frequency as its label says it: "63", "1k", "2.5k", "12.5k", "3.111k". Thousands keep four
/// significant digits, trailing zeros cut.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn hz(f: f32, decimal: &str) -> String {
    if f < 1000.0 {
        return fixed(f as f64, 0, false, decimal);
    }
    let k = (f / 1000.0) as f64;
    let whole_digits = (k.log10().floor() as usize + 1).min(4);
    let text = fixed_point(k, 4 - whole_digits, false);
    let trimmed = if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.') } else { &text };
    format!("{}k", localised(trimmed.to_string(), decimal))
}

/// A graphic equalizer band's nominal frequency: [`hz`], with the one decimal of a low ISO band ("31.5").
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn iso_band(hz_nominal: f32, decimal: &str) -> String {
    if hz_nominal < 100.0 && hz_nominal.fract() != 0.0 { fixed(hz_nominal as f64, 1, false, decimal) } else { hz(hz_nominal, decimal) }
}

/// A size: "850 B", "38 KB", "2.1 MB", "38 MB", "2.1 GB".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn bytes(bytes: i64, decimal: &str) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1_048_576 => format!("{} KB", fixed(b as f64 / 1024.0, 0, false, decimal)),
        b if b < 10_485_760 => format!("{} MB", fixed(b as f64 / 1_048_576.0, 1, false, decimal)),
        b if b < 1_073_741_824 => format!("{} MB", fixed(b as f64 / 1_048_576.0, 0, false, decimal)),
        b => format!("{} GB", fixed(b as f64 / 1_073_741_824.0, 1, false, decimal)),
    }
}

/// "12.4 MB".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn megabytes(bytes: i64, decimal: &str) -> String {
    format!("{} MB", fixed(bytes as f64 / 1_048_576.0, 1, false, decimal))
}

/// A transfer speed: "850 B/s", "850 KB/s", "3.2 MB/s", "38 MB/s"; empty when nothing is measurable.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn speed(bytes_per_s: i64, decimal: &str) -> String {
    match bytes_per_s {
        b if b <= 0 => String::new(),
        b if b < 1_000 => format!("{b} B/s"),
        b if b < 1_000_000 => format!("{} KB/s", fixed(b as f64 / 1_000.0, 0, false, decimal)),
        b if b < 10_000_000 => format!("{} MB/s", fixed(b as f64 / 1_000_000.0, 1, false, decimal)),
        b => format!("{} MB/s", fixed(b as f64 / 1_000_000.0, 0, false, decimal)),
    }
}

/// A sample rate as a DAC's mode is written: "44.1 kHz".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn kilohertz(rate: i32, decimal: &str) -> String {
    format!("{} kHz", fixed(rate as f64 / 1000.0, 1, false, decimal))
}

/// A sample rate in kHz as a record's format says it, never localised: "44.1", "96.0", "88.2".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn khz(rate: i32) -> String {
    format!("{:?}", rate as f64 / 1000.0)
}

fn localised(text: String, decimal: &str) -> String {
    if decimal == POINT { text } else { text.replacen('.', decimal, 1) }
}

/// [`fixed`] with a point.
fn fixed_point(v: f64, places: usize, plus: bool) -> String {
    let body = format!("{:.*}", places, half_up(v.abs(), places));
    let sign = if v.is_sign_negative() { "-" } else if plus { "+" } else { "" };
    format!("{sign}{body}")
}

/// `v` (not negative) rounded half up at `places` decimals, on its shortest decimal form.
fn half_up(v: f64, places: usize) -> f64 {
    let shortest = format!("{v}");
    let Some((whole, frac)) = shortest.split_once('.') else { return v };
    if frac.len() <= places {
        return v;
    }
    let kept: f64 = format!("{whole}.{}", &frac[..places]).parse().unwrap_or(v);
    if frac.as_bytes()[places] >= b'5' { kept + 10f64.powi(-(places as i32)) } else { kept }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Printed by Java's `String.format` (OpenJDK 21, root locale): value bits, places, plus, result.
    const JAVA: &[(u64, u32, bool, &str)] = &[
        (0x4016187300000000, 1, true, "+5.5"),
        (0x401e7d2900000000, 1, true, "+7.6"),
        (0xc0022c4f00000000, 0, true, "-2"),
        (0xbff72e8e00000000, 0, false, "-1"),
        (0xc01d7c2820000000, 0, false, "-7"),
        (0x40085eb800000000, 0, true, "+3"),
        (0xbfe16ac400000000, 1, false, "-0.5"),
        (0x40145c4400000000, 1, true, "+5.1"),
        (0xc017efbde0000000, 1, true, "-6.0"),
        (0xc0154d29e0000000, 1, true, "-5.3"),
        (0xc0205cf940000000, 2, false, "-8.18"),
        (0x40207ef3c0000000, 1, true, "+8.2"),
        (0x4019460700000000, 1, false, "6.3"),
        (0x3fef623c00000000, 3, true, "+0.981"),
        (0xc006892800000000, 1, false, "-2.8"),
        (0x401b092d00000000, 1, false, "6.8"),
        (0xc021068d40000000, 0, false, "-9"),
        (0xc02061f160000000, 2, false, "-8.19"),
        (0xc02282e780000000, 2, true, "-9.26"),
        (0xc02447d400000000, 2, false, "-10.14"),
        (0x40056e9400000000, 2, true, "+2.68"),
        (0x402181e000000000, 3, false, "8.754"),
        (0xc022b47560000000, 0, true, "-9"),
        (0xc00cc57400000000, 2, false, "-3.60"),
        (0xc0118c7580000000, 2, false, "-4.39"),
        (0xc01ff52d00000000, 3, false, "-7.989"),
        (0xc011aea1e0000000, 2, false, "-4.42"),
        (0xc016394f40000000, 0, false, "-6"),
        (0x4022ca9540000000, 1, false, "9.4"),
        (0xc002771480000000, 2, false, "-2.31"),
        (0xc0015fca00000000, 3, true, "-2.172"),
        (0x3ff495b400000000, 1, false, "1.3"),
        (0x4021a20b40000000, 0, false, "9"),
        (0x3ff9d20c00000000, 2, false, "1.61"),
        (0x40279fb400000000, 0, true, "+12"),
        (0x3f83580000000000, 2, true, "+0.01"),
        (0x3fbbfa5000000000, 0, true, "+0"),
        (0xc003965d80000000, 0, true, "-2"),
        (0x402426d640000000, 1, false, "10.1"),
        (0x3fdd039400000000, 0, false, "0"),
        (0x3fc7a4e000000000, 0, false, "0"),
        (0xc018258de0000000, 2, true, "-6.04"),
        (0xc014f72520000000, 1, true, "-5.2"),
        (0xc01d4843c0000000, 2, false, "-7.32"),
        (0xc02156bfc0000000, 3, true, "-8.669"),
        (0xc01b450cc0000000, 2, true, "-6.82"),
        (0x3fe0dd8c00000000, 1, true, "+0.5"),
        (0xc008d40f00000000, 0, true, "-3"),
        (0xc01a79ee80000000, 2, false, "-6.62"),
        (0xc025d73180000000, 0, true, "-11"),
    ];

    #[test]
    fn fractions_round_half_up_as_java_does() {
        let mut wrong = Vec::new();
        let mut check = |v: f64, places: u32, plus: bool, want: &str| {
            let got = fixed(v, places, plus, POINT);
            if got != want {
                wrong.push(format!("{v} to {places}: {got}, not {want}"));
            }
        };
        for &(bits, places, plus, want) in JAVA {
            check(f64::from_bits(bits), places, plus, want);
        }
        for (v, places, want) in [(0.15, 1, "0.2"), (62.5, 0, "63"), (-0.04, 1, "-0.0"), (9.96, 1, "10.0"), (0.0005, 3, "0.001"), (2.5, 0, "3")] {
            check(v, places, false, want);
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn numbers_and_units() {
        let comma = ",";
        let cases: &[(String, &str)] = &[
            (clock(0, false), "0:00"),
            (clock(187, false), "3:07"),
            (clock(3723, false), "1:02:03"),
            (clock(7200, false), "2:00:00"),
            (clock(187, true), "-3:07"),
            (clock(0, true), "-0:00"),
            (clock(-5, false), "0:00"),
            (hz(62.5, POINT), "63"),
            (hz(f32::from_bits(0x4479e000), POINT), "1000"),
            (hz(1_000.0, POINT), "1k"),
            (hz(1_600.0, POINT), "1.6k"),
            (hz(2_500.0, POINT), "2.5k"),
            (hz(f32::from_bits(0x45426b37), POINT), "3.111k"),
            (hz(10_000.0, POINT), "10k"),
            (hz(12_500.0, POINT), "12.5k"),
            (hz(16_000.0, POINT), "16k"),
            (hz(12_500.0, comma), "12,5k"),
            (hz(1_000.0, comma), "1k"),
            (iso_band(31.5, POINT), "31.5"),
            (iso_band(63.0, POINT), "63"),
            (iso_band(1_000.0, POINT), "1k"),
            (iso_band(12_500.0, POINT), "12.5k"),
            (signed_db(-0.0, POINT), "+0.0"),
            (signed_db(3.25, POINT), "+3.3"),
            (signed_db(-1.0, POINT), "-1.0"),
            (signed_db(3.0, comma), "+3,0"),
            (nudge(-250, POINT), "-0.3"),
            (nudge(500, POINT), "+0.5"),
            (bytes(850, POINT), "850 B"),
            (bytes(38 * 1024, POINT), "38 KB"),
            (bytes(2_202_009, POINT), "2.1 MB"),
            (bytes(38 * 1_048_576, POINT), "38 MB"),
            (bytes(2_254_857_830, POINT), "2.1 GB"),
            (megabytes(10 * 1_048_576 + 104_858, POINT), "10.1 MB"),
            (megabytes(12 * 1_048_576 + 419_431, comma), "12,4 MB"),
            (fixed(1234.5, 1, false, comma), "1234,5"),
            (speed(0, POINT), ""),
            (speed(850, POINT), "850 B/s"),
            (speed(850_000, POINT), "850 KB/s"),
            (speed(3_200_000, POINT), "3.2 MB/s"),
            (speed(38_000_000, POINT), "38 MB/s"),
            (kilohertz(44_100, POINT), "44.1 kHz"),
            (kilohertz(48_000, comma), "48,0 kHz"),
            (khz(44_100), "44.1"),
            (khz(96_000), "96.0"),
        ];
        let wrong: Vec<String> = cases.iter().filter(|(got, want)| got != want).map(|(got, want)| format!("{got:?}, not {want:?}")).collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
