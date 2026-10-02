//! Stream targets and cache keys. Caches are keyed by song id and quality, never by URL.

/// A quality setting: `bit_rate` 0 and an empty `format` mean the original file.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StreamQuality {
    pub bit_rate: u32,
    pub format: String,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StreamTarget {
    pub url: String,
    /// The cache key: `<id>:<bit rate><format>` in the rolling stream cache, `dl:<id>` for downloads.
    pub key: String,
}

/// A song to fetch whole into the stream cache ahead of its turn.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Fetch {
    pub id: String,
    pub url: String,
    pub key: String,
}

/// A finished download's cache key.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn download_key(id: String) -> String {
    format!("dl:{id}")
}

/// Whether `key` is a streamed copy of `id` at any quality (the quality part never holds a colon).
pub fn is_copy(id: &str, key: &str) -> bool {
    key.rsplit_once(':').is_some_and(|(before, _)| before == id)
}
