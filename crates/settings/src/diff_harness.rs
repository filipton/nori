//! Differential harness: settings through every change by name, the sound profile's parts and JSON,
//! levels and the lyrics services, written to `$NORI_DIFF_OUT/settings.txt`.

use std::fmt::Write as _;

use crate::lyrics_sources::{self, LyricsService};
use crate::settings::*;
use crate::settings_model;

fn everything() -> StoredPrefs {
    let mut p = StoredPrefs::default();
    for s in settings_model::specs() {
        if let Some(v) = s.options.last() {
            if let Some(c) = set_by_name(&p, &s.name, v) {
                p = c.prefs;
            }
        }
    }
    p.servers = vec![SavedServer { id: "a".into(), name: "A".into(), url: "http://a".into(), user: "u".into(), alt_max_bit_rate: 128, ..Default::default() }];
    p.active_server_id = "a".into();
    p.eq_preamp_db = Some(-3.5);
    p.bass_boost_db = 4.0;
    p.compressor = true;
    p.expander = true;
    p.loudness = true;
    p
}

fn prefs_list() -> Vec<StoredPrefs> {
    let mut out = vec![StoredPrefs::default(), everything()];
    let mut rng = 0x1234_5678_9abc_def0u64;
    for n in 0..40 {
        let mut p = if n % 2 == 0 { StoredPrefs::default() } else { everything() };
        for _ in 0..8 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let specs = settings_model::specs();
            let s = &specs[rng as usize % specs.len()];
            let v = if s.options.is_empty() { format!("{}", (rng >> 20) % 50) } else { s.options[(rng >> 8) as usize % s.options.len()].clone() };
            if let Some(c) = set_by_name(&p, &s.name, &v) {
                p = c.prefs;
            }
        }
        out.push(p);
    }
    out
}

const ODD: [&str; 12] = ["", "garbage", "1", "0", "true", "false", "-5", "1e9", "NaN", " 3 ", "2.5", "LRCLIB:2"];

const SPECIAL: [&str; 14] =
    ["wifiQuality", "thirdPartyLookups", "lyricsSources", "lyricsPlace", "lyricsMove", "lyricsService:NETEASE", "lyricsService:nope", "compressorPreset", "crossfeedPreset", "eqLayout", "motionArtworkMobile", "musicFolder", "altMaxBitRate", "lyricsLrclib"];

const SOUND_JSON: [&str; 10] = [
    "{}",
    "[]",
    "not json",
    r#"{"eqPreampDb":"x"}"#,
    r#"{"eqPreampDb":null,"eqMode":1,"replayGain":9,"maxRate":7,"loudnessRefPhon":10}"#,
    r#"{"eqMode":0,"replayGain":-3,"loudnessRefPhon":100,"crossfeedHz":10,"bassBoostDb":99,"virtualizer":-1,"compRatio":0,"expRatio":50}"#,
    r#"{"eqEnabled":1,"mono":"yes","bypass":true,"eqBands":"garbage","eqGraphic":"1,2,3","eqGraphicTarget":"1,2"}"#,
    r#"{"crossfadeSec":6.7,"preampDb":"3","limiterThresholdDb":-2,"compressor":true,"compThresholdDb":-99}"#,
    r#"{"loudnessRefPhon":75.5,"hiRes":true,"bitPerfect":true,"maxRate":2}"#,
    r#"{"eqBands":"0:1000:3:1:0;1:60:-2:0.7:1","eqGraphic":"","eqPreampDb":2}"#,
];

#[test]
fn transcribe() {
    let Some(dir) = std::env::var_os("NORI_DIFF_OUT") else { return };
    let mut log = String::new();
    let list = prefs_list();
    let specs = settings_model::specs();
    for (i, p) in list.iter().enumerate() {
        let _ = writeln!(log, "prefs {i}: {p:?}");
        let sound = p.sound();
        let _ = writeln!(log, "  sound {sound:?}\n  effects {:?}", p.effects());
        let json = sound_json(&sound);
        let _ = writeln!(log, "  json {json}\n  back {:?}", sound_from(&json));
        let other = list[(i + 7) % list.len()].sound();
        let _ = writeln!(log, "  with {:?}", p.clone().with_sound(other));
        let mut saved: Vec<(String, PrefValue)> = save(p).into_iter().collect();
        saved.sort_by(|a, b| a.0.cmp(&b.0));
        let _ = writeln!(log, "  saved {saved:?}\n  loaded same {}", load(&saved.iter().cloned().collect()) == *p);
        let _ = writeln!(log, "  chain {} gain {:?} transition {:?}", p.sound_chain_on(), p.gain_prefs(), p.transition_prefs());
        for s in &specs {
            let _ = writeln!(log, "  value {} {:?}", s.name, settings_model::value_of(p, &s.name));
        }
        for l in EqLevel::ALL {
            let _ = write!(log, "  level {l:?} {}", l.of(&sound));
            for v in [-100.0, -1.0, 0.0, 0.01, 0.3, 1.0, 5.0, 12.0, 100.0, 2000.0, f32::NAN] {
                let _ = write!(log, " {:?}", set_level(sound.clone(), l, v));
            }
            let _ = writeln!(log);
        }
        let _ = writeln!(log, "  lookup {:?}", lyrics_sources::lyrics_lookup(p));
        for &s in LyricsService::ALL {
            let _ = writeln!(log, "  moved {s:?} {:?} {:?} placed {:?}", lyrics_sources::moved(p, s, -1), lyrics_sources::moved(p, s, 2), lyrics_sources::placed(p, s, 3));
        }
    }
    for (i, p) in list.iter().take(3).enumerate() {
        for s in &specs {
            let values: Vec<String> = s.options.iter().cloned().chain(ODD.iter().map(|v| v.to_string())).collect();
            for v in values {
                let _ = writeln!(log, "set {i} {} {v:?} {:?}", s.name, set_by_name(p, &s.name, &v));
            }
        }
        for name in SPECIAL {
            for v in ODD.iter().chain(&["opus:128", "raw", "default", "lrclib,unison", "NETEASE:3", "NETEASE:-1", "GENTLE", "chu_moy", "OFF", "31", "7"]) {
                let _ = writeln!(log, "special {i} {name} {v:?} {:?}", set_by_name(p, name, v));
            }
        }
    }
    for j in SOUND_JSON {
        let s = sound_from(j);
        let _ = writeln!(log, "json {j} {s:?} {:?}", s.as_ref().map(sound_json));
    }
    for &s in LyricsService::ALL {
        let _ = writeln!(log, "service {s:?} {} {} {:?} {} {} {} {:?}", s.name(), s.title(), s.best(), s.needs_key(), s.first_wave(), s.prior(), s.origin());
    }
    for n in ["lrclib", " GENIUS ", "Genius", "nope", ""] {
        let _ = writeln!(log, "named {n:?} {:?}", LyricsService::named(n));
    }
    let _ = writeln!(log, "parse {:?} default {:?}", lyrics_sources::parse("LRCLIB, UNISON,MUSIXMATCH,,LRCLIB"), lyrics_sources::default_order());
    for cut in [0.0, 650.0, 700.0, 701.0] {
        for level in [-1.0, 0.0, 4.5, 6.0, 9.5] {
            let _ = writeln!(log, "crossfeed {cut} {level} {:?}", crossfeed_preset(cut, level));
        }
    }
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(std::path::Path::new(&dir).join("settings.txt"), log).unwrap();
}
