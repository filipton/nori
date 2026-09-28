//! Where the client streams a song from: the quality for the network the phone is on and the profile's
//! address, a download's permanent copy, and the songs fetched ahead with their addresses. The keys and
//! the network state are nori-net's (`nori_net::stream`).

use crate::client::Client;

pub use nori_net::stream::*;

/// The URL and cache key `id` opens from now, through the client the app streams through: a finished
/// download is the permanent copy, anything else streams at the quality for the network the platform
/// last said it is on. None before a client exists.
pub fn resolve_now(id: &str) -> Option<StreamTarget> {
    let client = crate::client::active_client()?;
    let kept = crate::transfers::held(id) == crate::transfers::HeldState::Done;
    Some(client.resolve(id.to_string(), kept, !kept && metered()))
}

/// Whether the length a stream URL is answered with is only the server's estimate: a transcode, asked
/// with `estimateContentLength` ([`crate::Core::stream_url`]) so that it has one at all. The server makes
/// it as it sends it, so a range from past its start is answered only once the whole song is transcoded.
pub fn length_estimated(url: &str) -> bool {
    url.split(['?', '&']).any(|p| p == "estimateContentLength=true")
}

/// What is fetched ahead now ([`Client::precache_targets`]) through the client the app streams through, on
/// the network the platform last said it is on; none before a client exists.
pub fn precache_now() -> Vec<Fetch> {
    crate::client::active_client().map(|c| c.precache_targets(metered())).unwrap_or_default()
}

impl Client {
    /// [`Self::precache_targets`] over `ids`, `held` saying whether each is downloaded or queued for it.
    fn fetches(&self, ids: Vec<String>, metered: bool, wifi: &StreamQuality, mobile: &StreamQuality, held: impl Fn(&str) -> crate::transfers::HeldState) -> Vec<Fetch> {
        crate::rules::precache_list(ids, |id| held(id) != crate::transfers::HeldState::Absent)
            .into_iter()
            .map(|id| {
                let t = self.stream_target(id.clone(), metered, wifi.clone(), mobile.clone());
                Fetch { id, url: t.url, key: t.key }
            })
            .collect()
    }

    /// The quality to stream at: the metered or the Wi-Fi setting, and through the profile's second
    /// (usually public) address an optional ceiling on top, as opus unless a format was chosen.
    fn quality(&self, metered: bool, wifi: StreamQuality, mobile: StreamQuality) -> StreamQuality {
        let q = if metered { mobile } else { wifi };
        let cap = self.profile.read().alt_max_bit_rate;
        if self.on_second_address() && cap > 0 && (q.bit_rate == 0 || q.bit_rate > cap) {
            return StreamQuality { bit_rate: cap, format: if q.format.is_empty() { "opus".into() } else { q.format } };
        }
        q
    }

    /// The quality a song that is not downloaded streams at on a network that is `metered` or not: the
    /// user's setting for that network (bit rate and format; 0 and none are the original file), capped
    /// on the profile's second address. What [`Self::resolve`] fetches at, for a client to show or log
    /// as the network changes.
    pub fn streaming_quality(&self, metered: bool) -> StreamQuality {
        let q = |s: &crate::settings::SavedQuality| StreamQuality { bit_rate: s.bit_rate.max(0) as u32, format: s.format.clone() };
        let (wifi, mobile) = crate::rules::prefs(|p| (q(&p.wifi), q(&p.mobile)));
        self.quality(metered, wifi, mobile)
    }
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Only the cache key [`Self::stream_target`] would give: for asking whether a song is already cached.
    pub fn stream_key(&self, id: String, metered: bool, wifi: StreamQuality, mobile: StreamQuality) -> String {
        let q = self.quality(metered, wifi, mobile);
        format!("{id}:{}{}", q.bit_rate, q.format)
    }

    /// The URL and cache key to open `id` from now: a finished download is the permanent copy (`downloaded`),
    /// fetched at the download quality; anything else streams at the quality for the network the phone is
    /// on (`metered`). The qualities are the user's settings.
    pub fn resolve(&self, id: String, downloaded: bool, metered: bool) -> StreamTarget {
        let q = |s: &crate::settings::SavedQuality| StreamQuality { bit_rate: s.bit_rate.max(0) as u32, format: s.format.clone() };
        let (wifi, mobile, download) = crate::rules::prefs(|p| (q(&p.wifi), q(&p.mobile), q(&p.download)));
        if downloaded {
            self.download_target(id, download)
        } else {
            self.stream_target(id, metered, wifi, mobile)
        }
    }

    /// The URL and cache key a download of `id` is fetched and kept under.
    pub fn download_target(&self, id: String, quality: StreamQuality) -> StreamTarget {
        let key = download_key(id.clone());
        StreamTarget { url: self.core.stream_url(id, quality.bit_rate, quality.format), key }
    }
}

/// Asked only in Rust, so not exported to Kotlin.
impl Client {
    /// The URL and cache key to stream `id` from now.
    pub fn stream_target(&self, id: String, metered: bool, wifi: StreamQuality, mobile: StreamQuality) -> StreamTarget {
        let q = self.quality(metered, wifi, mobile);
        let key = format!("{id}:{}{}", q.bit_rate, q.format);
        StreamTarget { url: self.core.stream_url(id, q.bit_rate, q.format), key }
    }

    /// What the precacher fetches now, in order, each with its address and key: [`crate::rules::queue_precache`]'s
    /// songs as [`crate::rules::precache_list`] filters them, and none that is downloaded or in the download
    /// queue (a download arrives for good; the rolling cache would keep it twice). One call per song
    /// start, where the list, its filtering and each song's download state and address were a call each.
    pub fn precache_targets(&self, metered: bool) -> Vec<Fetch> {
        let q = |s: &crate::settings::SavedQuality| StreamQuality { bit_rate: s.bit_rate.max(0) as u32, format: s.format.clone() };
        let (wifi, mobile) = crate::rules::prefs(|p| (q(&p.wifi), q(&p.mobile)));
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
    fn quality_follows_the_network_and_the_second_address_caps_it() {
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
        assert_eq!(c.stream_key("s1".into(), false, q(96, "mp3"), q(0, "")), "s1:96mp3", "already under the cap");
        assert_eq!(c.stream_key("s1".into(), false, q(320, "mp3"), q(0, "")), "s1:128mp3");
    }

    #[test]
    fn the_songs_ahead_come_with_where_they_are_fetched_from() {
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
    fn only_a_transcode_is_answered_with_an_estimated_length() {
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        assert!(!length_estimated(&c.resolve("s1".into(), false, false).url), "the original file on Wi-Fi");
        assert!(!length_estimated(&c.resolve("s1".into(), false, true).url), "the original file on a metered network too, by default");
        assert!(length_estimated(&c.stream_target("s1".into(), false, q(192, "opus"), q(0, "")).url), "192k opus is a transcode");
        assert!(length_estimated(&c.stream_target("s1".into(), false, q(128, ""), q(0, "")).url), "a bit rate alone transcodes too");
    }

    #[test]
    fn a_download_opens_as_itself_and_anything_else_streams() {
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        assert_eq!(c.resolve("s1".into(), true, true).key, "dl:s1", "the permanent copy, whatever the network");
        // The settings' defaults: the original file on Wi-Fi and on a metered network alike.
        assert_eq!(c.resolve("s1".into(), false, false).key, "s1:0");
        assert_eq!(c.resolve("s1".into(), false, true).key, "s1:0");
        let (wifi, metered) = (c.streaming_quality(false), c.streaming_quality(true));
        assert_eq!((wifi.bit_rate, wifi.format.as_str(), metered.bit_rate, metered.format.as_str()), (0, "", 0, ""), "what those keys name");
    }

    #[test]
    fn downloads_and_streamed_copies() {
        let (c, _) = client(NetProfile { url: "h".into(), ..Default::default() });
        let t = c.download_target("s:1".into(), q(0, ""));
        assert_eq!(t.key, "dl:s:1");
        assert!(t.url.ends_with("&id=s%3A1"));
        let keys = ["a:0", "a:192opus", "ab:0", "dl:a", "x:a:0"];
        assert_eq!(keys.into_iter().filter(|k| is_copy("a", k)).collect::<Vec<_>>(), ["a:0", "a:192opus"]);
    }
}
