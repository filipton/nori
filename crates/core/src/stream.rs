//! Stream and download URLs and cache keys, and the prefetch list. Keys and network state are nori-net's.

use crate::client::Client;

pub use nori_net::stream::*;

/// Where `id` opens now via the active client: the download if finished, else a stream at the current
/// network's quality. None without a client.
pub fn resolve_now(id: &str) -> Option<StreamTarget> {
    let client = crate::client::active_client()?;
    let kept = crate::transfers::held(id) == crate::transfers::HeldState::Done;
    Some(client.resolve(id.to_string(), kept, !kept && metered()))
}

/// Whether the URL is a transcode with an estimated length ([`crate::Core::stream_url`]); ranges past the
/// start are only served once transcoding catches up.
pub fn length_estimated(url: &str) -> bool {
    url.split(['?', '&']).any(|p| p == "estimateContentLength=true")
}

/// [`Client::precache_targets`] via the active client on the current network.
pub fn precache_now() -> Vec<Fetch> {
    crate::client::active_client().map(|c| c.precache_targets(metered())).unwrap_or_default()
}

/// The user's (wifi, mobile, download) qualities.
fn saved_qualities() -> (StreamQuality, StreamQuality, StreamQuality) {
    let q = |s: &crate::settings::SavedQuality| StreamQuality { bit_rate: s.bit_rate.max(0) as u32, format: s.format.clone() };
    crate::rules::prefs(|p| (q(&p.wifi), q(&p.mobile), q(&p.download)))
}

fn stream_cache_key(id: &str, q: &StreamQuality) -> String {
    format!("{id}:{}{}", q.bit_rate, q.format)
}

impl Client {
    /// [`Self::precache_targets`] over `ids`; `held` gives each id's download state.
    fn fetches(&self, ids: Vec<String>, metered: bool, wifi: &StreamQuality, mobile: &StreamQuality, held: impl Fn(&str) -> crate::transfers::HeldState) -> Vec<Fetch> {
        crate::rules::precache_list(ids, |id| held(id) != crate::transfers::HeldState::Absent)
            .into_iter()
            .map(|id| {
                let t = self.stream_target(id.clone(), metered, wifi.clone(), mobile.clone());
                Fetch { id, url: t.url, key: t.key }
            })
            .collect()
    }

    /// The network's quality, capped on the second address (as opus unless a format is set).
    fn quality(&self, metered: bool, wifi: StreamQuality, mobile: StreamQuality) -> StreamQuality {
        let q = if metered { mobile } else { wifi };
        let cap = self.profile.read().alt_max_bit_rate;
        if self.on_second_address() && cap > 0 && (q.bit_rate == 0 || q.bit_rate > cap) {
            return StreamQuality { bit_rate: cap, format: if q.format.is_empty() { "opus".into() } else { q.format } };
        }
        q
    }

    /// The quality [`Self::resolve`] streams at on this network.
    pub fn streaming_quality(&self, metered: bool) -> StreamQuality {
        let (wifi, mobile, _) = saved_qualities();
        self.quality(metered, wifi, mobile)
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// The cache key [`Self::stream_target`] would give.
    pub fn stream_key(&self, id: String, metered: bool, wifi: StreamQuality, mobile: StreamQuality) -> String {
        stream_cache_key(&id, &self.quality(metered, wifi, mobile))
    }

    /// Where `id` opens: the download (`downloaded`) at download quality, else a stream for the network.
    pub fn resolve(&self, id: String, downloaded: bool, metered: bool) -> StreamTarget {
        let (wifi, mobile, download) = saved_qualities();
        if downloaded {
            self.download_target(id, download)
        } else {
            self.stream_target(id, metered, wifi, mobile)
        }
    }

    /// The URL and cache key of `id`'s download.
    pub fn download_target(&self, id: String, quality: StreamQuality) -> StreamTarget {
        let key = download_key(id.clone());
        StreamTarget { url: self.core.stream_url(id, quality.bit_rate, quality.format), key }
    }
}

impl Client {
    /// The URL and cache key to stream `id`.
    pub(crate) fn stream_target(&self, id: String, metered: bool, wifi: StreamQuality, mobile: StreamQuality) -> StreamTarget {
        let q = self.quality(metered, wifi, mobile);
        let key = stream_cache_key(&id, &q);
        StreamTarget { url: self.core.stream_url(id, q.bit_rate, q.format), key }
    }

    /// The songs to prefetch now ([`crate::rules::queue_precache`], minus downloaded or queued downloads),
    /// with URL and key.
    pub fn precache_targets(&self, metered: bool) -> Vec<Fetch> {
        let (wifi, mobile, _) = saved_qualities();
        self.fetches(crate::rules::queue_precache(metered), metered, &wifi, &mobile, crate::transfers::held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::{block, client, two_addresses};
    use crate::client::NetProfile;
    use crate::transport::FailureKind;

    fn q(bit_rate: u32, format: &str) -> StreamQuality {
        StreamQuality { bit_rate, format: format.into() }
    }

    #[test]
    fn quality_follows_network_and_second_address_cap() {
        let (c, fake) = client(NetProfile { alt_max_bit_rate: 128, ..two_addresses() });
        let t = c.stream_target("s1".into(), false, q(0, ""), q(192, "opus"));
        assert_eq!(t.key, "s1:0");
        assert!(t.url.contains("/rest/stream?") && t.url.ends_with("&id=s1"));
        assert_eq!(c.stream_key("s1".into(), true, q(0, ""), q(192, "opus")), "s1:192opus");

        fake.fail(FailureKind::Connect);
        assert!(block(c.choose_address()));
        let t = c.stream_target("s1".into(), false, q(0, ""), q(192, "opus"));
        assert_eq!(t.key, "s1:128opus");
        assert!(t.url.starts_with("https://wan.example/rest/stream?") && t.url.ends_with("&id=s1&maxBitRate=128&format=opus&estimateContentLength=true"));
        assert_eq!(c.stream_key("s1".into(), false, q(96, "mp3"), q(0, "")), "s1:96mp3", "under the cap");
        assert_eq!(c.stream_key("s1".into(), false, q(320, "mp3"), q(0, "")), "s1:128mp3");
    }

    #[test]
    fn prefetch_skips_radio_providers_and_downloads() {
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        let ids = ["a", "radio:1", "ext-2", "queued", "done", "b"].map(String::from).to_vec();
        let held = |id: &str| match id {
            "queued" => crate::transfers::HeldState::Pending,
            "done" => crate::transfers::HeldState::Done,
            _ => crate::transfers::HeldState::Absent,
        };
        let f = c.fetches(ids, true, &q(0, ""), &q(192, "opus"), held);
        assert_eq!(f.iter().map(|f| (f.id.as_str(), f.key.as_str())).collect::<Vec<_>>(), [("a", "a:192opus"), ("b", "b:192opus")]);
        assert!(f[0].url.ends_with("&id=a&maxBitRate=192&format=opus&estimateContentLength=true"), "{}", f[0].url);
    }

    #[test]
    fn only_transcodes_have_estimated_length() {
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        assert!(!length_estimated(&c.resolve("s1".into(), false, false).url));
        assert!(!length_estimated(&c.resolve("s1".into(), false, true).url));
        assert!(length_estimated(&c.stream_target("s1".into(), false, q(192, "opus"), q(0, "")).url));
        assert!(length_estimated(&c.stream_target("s1".into(), false, q(128, ""), q(0, "")).url), "bit rate alone");
    }

    #[test]
    fn resolve_prefers_download() {
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        assert_eq!(c.resolve("s1".into(), true, true).key, "dl:s1");
        assert_eq!(c.resolve("s1".into(), false, false).key, "s1:0", "not downloaded: streamed");
    }

    #[test]
    fn download_keys_and_stream_copies() {
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        let t = c.download_target("s:1".into(), q(0, ""));
        assert_eq!(t.key, "dl:s:1");
        assert!(t.url.ends_with("&id=s%3A1"));
        let keys = ["a:0", "a:192opus", "ab:0", "dl:a", "x:a:0"];
        assert_eq!(keys.into_iter().filter(|k| is_copy("a", k)).collect::<Vec<_>>(), ["a:0", "a:192opus"]);
    }
}
