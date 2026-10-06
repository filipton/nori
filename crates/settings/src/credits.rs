//! Third-party credits for the licences page.

/// One credit. `file` names the bundled licence text (`licences/<file>.txt`), None when there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Credit {
    pub name: String,
    pub what: String,
    pub copyright: String,
    pub licence: String,
    pub file: Option<String>,
}

/// The core's dependencies; add a line with each new dependency. Dual MIT/Apache crates show the MIT text.
const CORE_CREDITS: [(&str, &str, &str, &str, Option<&str>); 19] = [
    ("uniffi", "Generates the Kotlin bindings to the core and the JNI calls under them", "Mozilla Foundation", "MPL-2.0", Some("MPL-2.0")),
    ("rusqlite", "The library index, full-text search and caches", "Copyright (c) 2014 The rusqlite developers", "MIT", Some("MIT")),
    ("SQLite", "The database itself, bundled into the core", "D. Richard Hipp and the SQLite developers, dedicated to the public domain", "Public domain", None),
    ("RustFFT", "The spectrum analysis behind tempo, beats and key", "Copyright (c) 2015 The RustFFT Developers", "MIT or Apache-2.0", Some("MIT")),
    (
        "ebur128",
        "Measuring how loud each song is, for AutoMix's levels",
        "Copyright (c) 2011 Jan Kokemüller; Copyright (c) 2020 Sebastian Dröge",
        "MIT",
        Some("MIT"),
    ),
    (
        "Signalsmith Stretch",
        "Time-stretching for beat-matched mixes",
        "Copyright (c) 2022 Geraint Luff / Signalsmith Audio Ltd.; Rust binding Copyright 2024 Colin Marc",
        "MIT",
        Some("MIT"),
    ),
    ("serde and serde_json", "Reading the server's answers", "Copyright (c) David Tolnay and the Serde developers", "MIT or Apache-2.0", Some("MIT")),
    ("jni", "The core's direct calls from the audio path", "Copyright (c) 2016 Prevoty, Inc. and jni-rs contributors", "MIT or Apache-2.0", Some("MIT")),
    ("simd_cesu8", "Java's strings turned into the core's and back", "Copyright (c) Sean C. Roach", "MIT or Apache-2.0", Some("MIT")),
    ("md-5", "Signing requests the way the Subsonic API asks", "Copyright (c) RustCrypto Developers", "MIT or Apache-2.0", Some("MIT")),
    ("parking_lot", "Locks inside the core", "Copyright (c) 2016 The Rust Project Developers (Amanieu d'Antras)", "MIT or Apache-2.0", Some("MIT")),
    ("thiserror", "Errors inside the core", "Copyright (c) David Tolnay", "MIT or Apache-2.0", Some("MIT")),
    (
        "Media3 Sonic and silence skipping, ported",
        "Speed, pitch and shortened silences, ported line for line into the core",
        "Copyright The Android Open Source Project",
        "Apache-2.0",
        Some("Apache-2.0"),
    ),
    (
        "AndroidX Palette, ported",
        "The colour quantiser behind a page's accent, ported line for line into the core",
        "Copyright The Android Open Source Project",
        "Apache-2.0",
        Some("Apache-2.0"),
    ),
    (
        "zune-jpeg, jpeg-decoder, png, image-webp and gif",
        "Decoding cover art: JPEG, PNG, WebP and GIF",
        "Copyright (c) the zune-image, image-rs and jpeg-decoder developers",
        "MIT or Apache-2.0",
        Some("MIT"),
    ),
    ("yaml-rust2", "Reading LRCLIB's word-timed lyrics", "Copyright (c) 2015 Chen Yuheng; Copyright (c) 2023 Ethiraric", "MIT or Apache-2.0", Some("MIT")),
    ("roxmltree", "Reading word-timed lyrics written as TTML", "Copyright (c) 2018 Yevhenii Reizner", "MIT or Apache-2.0", Some("MIT")),
    (
        "miniz_oxide",
        "Unpacking KuGou's word-timed lyrics",
        "Copyright 2013-2014 RAD Game Tools and Valve Software; Copyright 2010-2014 Rich Geldreich and Tenacious Software LLC",
        "MIT, Zlib or Apache-2.0",
        Some("MIT"),
    ),
    ("futures-util", "Asking the lyrics services together", "Copyright (c) 2016 Alex Crichton; Copyright (c) 2017 The Tokio Authors", "MIT or Apache-2.0", Some("MIT")),
];

/// The Android app's libraries; add a line (and to NOTICE) with each new dependency.
const ANDROID_CREDITS: [(&str, &str, &str, &str, Option<&str>); 6] = [
    ("AndroidX Media3", "Playback, the media session and the notification", "Copyright The Android Open Source Project", "Apache-2.0", Some("Apache-2.0")),
    ("Jetpack Compose and Material 3", "The user interface toolkit", "Copyright The Android Open Source Project", "Apache-2.0", Some("Apache-2.0")),
    ("Material Icons", "The icons", "Copyright Google LLC", "Apache-2.0", Some("Apache-2.0")),
    ("AndroidX Navigation, Lifecycle, Activity, Core", "The app's plumbing", "Copyright The Android Open Source Project", "Apache-2.0", Some("Apache-2.0")),
    ("OkHttp", "Every network request", "Copyright Square, Inc.", "Apache-2.0", Some("Apache-2.0")),
    (
        "kotlinx.coroutines",
        "The concurrency the app is written in",
        "Copyright JetBrains s.r.o. and Kotlin Programming Language contributors",
        "Apache-2.0",
        Some("Apache-2.0"),
    ),
];

/// The typeface and the third-party data services.
const DATA_CREDITS: [(&str, &str, &str, &str, Option<&str>); 17] = [
    ("Inter", "The typeface", "Copyright (c) 2016 The Inter Project Authors (Rasmus Andersson)", "OFL-1.1", Some("OFL-1.1")),
    ("AutoEQ", "Headphone correction curves: the list kept on Wi-Fi, a curve fetched when it is chosen", "Copyright (c) 2018 Jaakko Pasanen", "MIT", Some("MIT")),
    ("LRCLIB", "Timed lyrics for songs your server has none for, asked only when switched on", "lrclib.net; lyrics belong to their authors and contributors", "Service", None),
    (
        "Unison",
        "Lyrics written and timed by listeners, asked only when switched on",
        "Lyrics from Unison (https://unison.boidu.dev), under the Open Database License (ODbL-1.0); lyrics belong to their authors",
        "ODbL-1.0",
        None,
    ),
    ("BiniLyrics", "Lyrics timed syllable by syllable, asked only when switched on", "binimum.org, a volunteer's copy of Apple Music's lyrics; lyrics belong to their authors", "Service", None),
    ("BetterLyrics", "Lyrics timed syllable by syllable, and QQ Music's word by word, asked only when switched on", "betterlyrics.org; lyrics belong to their authors", "Service", None),
    (
        "PaxSenix",
        "Apple Music's, Spotify's and Musixmatch's lyrics, asked only when switched on (the last two with your own key)",
        "paxsenix.org, with songs found through Apple's iTunes Search API; lyrics belong to their authors",
        "Service",
        None,
    ),
    ("LyricsPlus", "Lyrics timed syllable by syllable, asked only when switched on", "The YouLy+ project's volunteer servers; lyrics belong to their authors", "Service", None),
    ("NetEase Cloud Music", "Lyrics, often timed word by word, asked only when switched on", "music.163.com; lyrics belong to their authors", "Service", None),
    ("KuGou", "Lyrics, often timed word by word, asked only when switched on", "kugou.com; lyrics belong to their authors", "Service", None),
    ("SimpMusic", "Lyrics timed by listeners, asked only when switched on", "simpmusic.org; lyrics belong to their authors", "Service", None),
    ("YouTube Music", "Captions and lyrics of a song's YouTube upload, asked only when switched on", "Google LLC; lyrics belong to their authors", "Service", None),
    ("Megalobiz", "Lyrics timed line by line by its users, asked only when switched on", "megalobiz.com; lyrics belong to their authors", "Service", None),
    ("Genius", "Untimed lyrics, asked last and only when switched on", "genius.com; lyrics belong to their authors", "Service", None),
    ("iTunes Search API", "Finding a song's Apple Music id for PaxSenix, asked only when switched on", "Apple Inc.", "Service", None),
    ("Apple Music", "Moving album covers, asked only when switched on", "Apple Inc.; the artwork belongs to its artists and labels", "Service", None),
    (
        "Beat This!",
        "The neural beat tracker behind \"Better beat detection\": its network comes with the app, its weights from the authors' server when switched on",
        "Copyright (c) 2024 Institute of Computational Perception, JKU Linz, Austria (Foscarin, Schlüter and Widmer)",
        "MIT",
        Some("MIT"),
    ),
    (
        "Open-Unmix (UMX-HQ)",
        "The vocals model behind Sing: its network comes with the app, its weights from Zenodo when switched on",
        "Copyright (c) 2019 Inria (Fabian-Robert Stöter, Antoine Liutkus)",
        "MIT",
        Some("MIT"),
    ),
];

fn credits(list: &[(&str, &str, &str, &str, Option<&str>)]) -> Vec<Credit> {
    list.iter()
        .map(|(n, w, c, l, f)| Credit { name: n.to_string(), what: w.to_string(), copyright: c.to_string(), licence: l.to_string(), file: f.map(str::to_string) })
        .collect()
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn core_credits() -> Vec<Credit> {
    credits(&CORE_CREDITS)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn android_credits() -> Vec<Credit> {
    credits(&ANDROID_CREDITS)
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn data_credits() -> Vec<Credit> {
    credits(&DATA_CREDITS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_complete() {
        let (c, a, d) = (core_credits(), android_credits(), data_credits());
        // Every licence named is bundled for the licences page, and nothing is listed twice.
        let texts = concat!(env!("CARGO_MANIFEST_DIR"), "/../../app/src/main/assets/licences");
        let all: Vec<&Credit> = c.iter().chain(&a).chain(&d).collect();
        let missing: Vec<String> = all.iter().filter_map(|c| c.file.as_ref()).filter(|f| !std::path::Path::new(texts).join(format!("{f}.txt")).is_file()).cloned().collect();
        assert!(missing.is_empty(), "licence texts named but not in app/src/main/assets/licences: {missing:?}");
        let mut names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        let twice: Vec<&&str> = names.windows(2).filter(|w| w[0] == w[1]).map(|w| &w[0]).collect();
        assert!(twice.is_empty(), "credited twice: {twice:?}");
        assert!(all.iter().all(|c| !c.what.is_empty() && !c.copyright.is_empty() && !c.licence.is_empty()), "every credit says what, whose and under what terms");
    }
}
