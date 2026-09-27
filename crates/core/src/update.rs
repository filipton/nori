//! The app's own updates: GitHub's latest release of filipton/nori read, its tag compared with this build's
//! version (semver), the APK for this phone's ABIs picked from its assets, whether to say so, and the daily
//! throttle. Asked when the app starts (at most once a day, "Check for updates" on) and from its button; never
//! on a timer. Downloading and installing the APK is the platform's: this only says which file and how big.

use serde::Deserialize;
use std::cmp::Ordering;

use crate::client::{Client, NetResult};
use crate::transport::{Exchange, NetError};

/// The latest published release: GitHub leaves drafts and prereleases out of it.
pub const LATEST_URL: &str = "https://api.github.com/repos/filipton/nori/releases/latest";
/// Looked at no more often than this unless asked.
pub const CHECK_EVERY_MS: i64 = 24 * 60 * 60 * 1000;
/// When the last check was asked for (`app_kv`, milliseconds since the epoch).
const CHECKED_KEY: &str = "update.checkedMs";
/// The version the user said "Later" to: not offered again by itself (`app_kv`).
const SKIPPED_KEY: &str = "update.skipped";
/// The whole request's limit.
const TIMEOUT_MS: u32 = 20_000;

/// A version as semver orders it: the three numbers, then a prerelease (`-rc.1`) before the release itself.
/// Build metadata (`+abc`) is read and ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl Version {
    /// "0.4.1", "v0.4.1", "0.5.0-rc.1", "1.2" (the missing number is 0). None for anything else.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let s = s.split('+').next().unwrap_or("");
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = core.split('.').map(|n| if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) { n.parse::<u64>().ok() } else { None });
        let major = nums.next()??;
        let minor = nums.next().unwrap_or(Some(0))?;
        let patch = nums.next().unwrap_or(Some(0))?;
        if nums.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => {
                let ids: Vec<String> = p.split('.').map(str::to_string).collect();
                if ids.iter().any(|i| i.is_empty() || !i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')) {
                    return None;
                }
                ids
            }
        };
        Some(Version { major, minor, patch, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            // A prerelease comes before its release.
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&other.pre) {
                    let o = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        // Numbers come before words.
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => a.cmp(b),
                    };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Whether `latest` is a later version than `current`. False when either cannot be read: an odd tag is
/// never offered.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn version_newer(current: String, latest: String) -> bool {
    matches!((Version::parse(&current), Version::parse(&latest)), (Some(c), Some(l)) if l > c)
}

/// One file of a release, as GitHub lists it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Asset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub browser_download_url: String,
}

/// The parts of GitHub's release answer read here.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// Every ABI Android names; an APK named after one carries only it.
const ABIS: [&str; 4] = ["arm64-v8a", "armeabi-v7a", "x86_64", "x86"];

/// The APK for a phone whose ABIs are `abis`, most preferred first (Android's `SUPPORTED_ABIS`): one named
/// for the first of them that has one (`nori-music-<version>-<abi>.apk`), else one named for no ABI (the
/// release that carries both 64-bit ones, `nori-music-<version>.apk`). An empty file is never picked.
pub fn pick_apk<'a>(assets: &'a [Asset], abis: &[String]) -> Option<&'a Asset> {
    let apks: Vec<&Asset> = assets.iter().filter(|a| a.name.to_ascii_lowercase().ends_with(".apk") && a.size > 0 && !a.browser_download_url.is_empty()).collect();
    let abi_of = |a: &Asset| {
        let stem = a.name[..a.name.len() - 4].to_string();
        ABIS.iter().find(|abi| stem.ends_with(&format!("-{abi}"))).copied()
    };
    abis.iter()
        .find_map(|want| apks.iter().find(|a| abi_of(a) == Some(want.as_str())))
        .or_else(|| apks.iter().find(|a| abi_of(a).is_none()))
        .copied()
}

/// A release newer than this build, and the file to install it from.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AppUpdate {
    /// The version, as it reads without its tag's "v": "0.5.0".
    pub version: String,
    /// The release notes as plain text (see [`plain_notes`]).
    pub notes: String,
    /// The release's page on GitHub.
    pub page: String,
    pub apk_name: String,
    pub apk_url: String,
    /// The APK's size in bytes as GitHub states it: the download has to come to exactly this.
    pub apk_bytes: u64,
}

/// What a check found.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum UpdateCheck {
    /// Nothing was asked: the check is switched off, or the last one was less than a day ago.
    NotDue,
    /// This build is the latest release, or later.
    UpToDate { latest: String },
    /// A newer release with an APK for this phone. `skipped`: the user said "Later" to this version, so it
    /// is not brought up by itself (a check that was asked for still shows it).
    Available { update: AppUpdate, skipped: bool },
    /// A newer release, but none of its files is an APK this phone can run.
    NoApk { version: String, page: String },
}

/// What a release means for a build at `current` on a phone with `abis`; `skipped` is the version the user
/// put off. An error for a release that should not have been listed (a draft, a prerelease) or whose tag is
/// no version.
pub fn decide(release: &Release, current: &str, abis: &[String], skipped: Option<&str>) -> Result<UpdateCheck, String> {
    if release.draft || release.prerelease {
        return Err(format!("{} is a draft or a prerelease", release.tag_name));
    }
    let latest = Version::parse(&release.tag_name).ok_or_else(|| format!("tag {:?} is no version", release.tag_name))?;
    let shown = release.tag_name.trim().trim_start_matches(['v', 'V']).to_string();
    // A build whose own version cannot be read is offered nothing, as [`version_newer`] says.
    let newer = Version::parse(current).is_some_and(|c| latest > c);
    if !newer {
        return Ok(UpdateCheck::UpToDate { latest: shown });
    }
    let Some(apk) = pick_apk(&release.assets, abis) else {
        return Ok(UpdateCheck::NoApk { version: shown, page: release.html_url.clone() });
    };
    let skipped = skipped.and_then(Version::parse).is_some_and(|s| s == latest);
    Ok(UpdateCheck::Available {
        update: AppUpdate {
            version: shown,
            notes: plain_notes(release.body.as_deref().unwrap_or("")),
            page: release.html_url.clone(),
            apk_name: apk.name.clone(),
            apk_url: apk.browser_download_url.clone(),
            apk_bytes: apk.size,
        },
        skipped,
    })
}

/// Whether the daily check is due: switched on, and never done, a day ago or more, or stamped in the future
/// (the clock was set back).
pub fn due(on: bool, checked_ms: Option<i64>, now_ms: i64) -> bool {
    on && checked_ms.is_none_or(|t| t > now_ms || now_ms - t >= CHECK_EVERY_MS)
}

/// Release notes written in Markdown as plain text for a small card: headings without their marks, list
/// items as bullets, each paragraph or item one line (the notes are wrapped at 100 columns), `**`, `__` and
/// backticks dropped, and a link as its text. At most one blank line in a row, none at the ends.
pub fn plain_notes(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    // Whether the last line out may take the next source line on (a paragraph or a list item going on).
    let mut open = false;
    for raw in md.lines() {
        let line = raw.trim();
        if line.is_empty() {
            if out.last().is_some_and(|l| !l.is_empty()) {
                out.push(String::new());
            }
            open = false;
            continue;
        }
        if let Some(h) = line.strip_prefix('#') {
            let h = inline(h.trim_start_matches('#').trim());
            if out.last().is_some_and(|l| !l.is_empty()) {
                out.push(String::new());
            }
            out.push(h);
            open = false;
            continue;
        }
        let item = ["- ", "* ", "+ "].iter().find_map(|m| line.strip_prefix(m));
        match item {
            Some(rest) => {
                out.push(format!("• {}", inline(rest.trim())));
                open = true;
            }
            None if open => {
                let last = out.last_mut().expect("an open line");
                last.push(' ');
                last.push_str(&inline(line));
            }
            None => {
                out.push(inline(line));
                open = true;
            }
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// A line's inline Markdown taken out: emphasis marks, code ticks, `[text](url)` as its text.
fn inline(s: &str) -> String {
    let s = s.replace("**", "").replace("__", "").replace('`', "");
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| after[mid + 2..].find(')').map(|end| (mid, mid + 2 + end))) {
            Some((mid, end)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..mid]);
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The version the user said "Later" to is not brought up by itself again; a newer one is.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn update_skip(version: String) {
    crate::settings_store::keep_app_value(SKIPPED_KEY, version);
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Asks GitHub for the latest release and says what it means for this build (`version`, the app's own
    /// version name) on a phone with `abis` (most preferred first). Unless `asked` (the button), only when
    /// "Check for updates" is on and the last check was a day ago or more; the time is kept before the
    /// request goes, so a failed one is not tried again the same day. Called when the app starts, never on a
    /// timer.
    pub async fn update_check(&self, asked: bool, version: String, abis: Vec<String>) -> NetResult<UpdateCheck> {
        let now = crate::db::now_ms();
        if !asked {
            let on = crate::settings_store::with_prefs(|p| p.update_check).unwrap_or(false);
            let checked = crate::settings_store::app_value(CHECKED_KEY).and_then(|v| v.parse::<i64>().ok());
            if !due(on, checked, now) {
                return Ok(UpdateCheck::NotDue);
            }
        }
        crate::settings_store::keep_app_value(CHECKED_KEY, now.to_string());
        let request = Exchange {
            url: LATEST_URL.to_string(),
            headers: [("Accept".to_string(), "application/vnd.github+json".to_string()), ("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string())].into(),
            json: None,
            timeout_ms: TIMEOUT_MS,
        };
        let r = self.transport.send(request).await?;
        if !(200..300).contains(&r.status) {
            return Err(NetError::Http { status: r.status });
        }
        let release: Release = serde_json::from_slice(&r.body).map_err(|e| NetError::Parse { reason: format!("latest release: {e}") })?;
        let skipped = if asked { None } else { crate::settings_store::app_value(SKIPPED_KEY) };
        let found = decide(&release, &version, &abis, skipped.as_deref()).map_err(|reason| NetError::Parse { reason })?;
        crate::alog::info(&format!("update check: {version} here, {} latest", release.tag_name));
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} reads"))
    }

    #[test]
    fn versions_read_and_order_as_semver() {
        assert_eq!(v("v0.4.1"), Version { major: 0, minor: 4, patch: 1, pre: vec![] });
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("0.4.0+abc123"), v("0.4.0"));
        for bad in ["", "v", "x1.0.0", "1..0", "1.0.0.0", "1.0.0-", "1.0.0-a..b", "1.a.0", "-1.0.0"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        // semver.org's own chain.
        let chain = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert!(v("0.10.0") > v("0.9.9"), "numbers, not text");
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(version_newer("0.4.0".into(), "v0.4.1".into()));
        assert!(!version_newer("0.4.1".into(), "v0.4.1".into()));
        assert!(!version_newer("0.5.0".into(), "v0.4.1".into()));
        assert!(version_newer("0.5.0-rc.1".into(), "v0.5.0".into()));
        assert!(!version_newer("0.4.0".into(), "nightly".into()), "an odd tag is never offered");
    }

    fn asset(name: &str) -> Asset {
        Asset { name: name.into(), size: 1000, browser_download_url: format!("https://github.com/filipton/nori/releases/download/v1/{name}") }
    }

    fn abis(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_apk_for_this_phone_is_picked() {
        let both = [asset("nori-music-0.5.0.apk"), asset("SHA256SUMS")];
        assert_eq!(pick_apk(&both, &abis(&["arm64-v8a", "armeabi-v7a"])).unwrap().name, "nori-music-0.5.0.apk");
        let split = [asset("SHA256SUMS"), asset("nori-music-0.5.0-x86_64.apk"), asset("nori-music-0.5.0-arm64-v8a.apk"), asset("nori-music-0.5.0-armeabi-v7a.apk")];
        assert_eq!(pick_apk(&split, &abis(&["arm64-v8a", "armeabi-v7a", "armeabi"])).unwrap().name, "nori-music-0.5.0-arm64-v8a.apk");
        assert_eq!(pick_apk(&split, &abis(&["x86_64", "arm64-v8a"])).unwrap().name, "nori-music-0.5.0-x86_64.apk", "the phone's order wins");
        assert_eq!(pick_apk(&split, &abis(&["armeabi-v7a"])).unwrap().name, "nori-music-0.5.0-armeabi-v7a.apk");
        // "x86" is not "x86_64", and a phone with neither gets nothing.
        assert_eq!(pick_apk(&[asset("nori-music-0.5.0-x86_64.apk")], &abis(&["x86"])), None);
        // One for this phone beats the one for every phone; one for another phone is never picked.
        let mixed = [asset("nori-music-0.5.0.apk"), asset("nori-music-0.5.0-arm64-v8a.apk")];
        assert_eq!(pick_apk(&mixed, &abis(&["arm64-v8a"])).unwrap().name, "nori-music-0.5.0-arm64-v8a.apk");
        assert_eq!(pick_apk(&mixed, &abis(&["x86_64"])).unwrap().name, "nori-music-0.5.0.apk");
        let empty = Asset { size: 0, ..asset("nori-music-0.5.0.apk") };
        assert_eq!(pick_apk(&[empty], &abis(&["arm64-v8a"])), None, "an empty file");
        assert_eq!(pick_apk(&[asset("SHA256SUMS")], &abis(&["arm64-v8a"])), None);
    }

    /// GitHub's answer, cut down to what is read, with the rest of its fields left in.
    const LATEST: &str = r#"{"url":"https://api.github.com/repos/filipton/nori/releases/1","html_url":"https://github.com/filipton/nori/releases/tag/v0.5.0",
      "id":1,"author":{"login":"filipton"},"tag_name":"v0.5.0","name":"nori 0.5.0","draft":false,"prerelease":false,
      "assets":[{"name":"nori-music-0.5.0.apk","size":63901760,"content_type":"application/vnd.android.package-archive",
                 "browser_download_url":"https://github.com/filipton/nori/releases/download/v0.5.0/nori-music-0.5.0.apk"},
                {"name":"SHA256SUMS","size":87,"browser_download_url":"https://github.com/filipton/nori/releases/download/v0.5.0/SHA256SUMS"}],
      "body":"The **player** is faster.\n\n### Fixed\n\n- A song\n  that was cut off\n- [Undo](https://x.y/1) works"}"#;

    fn latest() -> Release {
        serde_json::from_str(LATEST).unwrap()
    }

    #[test]
    fn a_newer_release_is_offered_once_unless_asked() {
        let phone = abis(&["arm64-v8a"]);
        let UpdateCheck::Available { update, skipped } = decide(&latest(), "0.4.0", &phone, None).unwrap() else { panic!("offered") };
        assert!(!skipped);
        assert_eq!(update.version, "0.5.0");
        assert_eq!(update.apk_name, "nori-music-0.5.0.apk");
        assert_eq!(update.apk_bytes, 63_901_760);
        assert_eq!(update.page, "https://github.com/filipton/nori/releases/tag/v0.5.0");
        assert_eq!(update.notes, "The player is faster.\n\nFixed\n\n• A song that was cut off\n• Undo works");
        // Put off: still found, but marked so the app does not bring it up by itself.
        assert!(matches!(decide(&latest(), "0.4.0", &phone, Some("0.5.0")).unwrap(), UpdateCheck::Available { skipped: true, .. }));
        assert!(matches!(decide(&latest(), "0.4.0", &phone, Some("v0.5.0")).unwrap(), UpdateCheck::Available { skipped: true, .. }));
        // An older version put off does not hide a newer one.
        assert!(matches!(decide(&latest(), "0.4.0", &phone, Some("0.4.9")).unwrap(), UpdateCheck::Available { skipped: false, .. }));
        assert_eq!(decide(&latest(), "0.5.0", &phone, None).unwrap(), UpdateCheck::UpToDate { latest: "0.5.0".into() });
        assert_eq!(decide(&latest(), "0.6.0-dev", &phone, None).unwrap(), UpdateCheck::UpToDate { latest: "0.5.0".into() });
        assert!(matches!(decide(&latest(), "0.5.0-rc.2", &phone, None).unwrap(), UpdateCheck::Available { .. }));
        let only_x86 = Release { assets: vec![asset("nori-music-0.5.0-x86_64.apk")], ..latest() };
        assert_eq!(
            decide(&only_x86, "0.4.0", &phone, None).unwrap(),
            UpdateCheck::NoApk { version: "0.5.0".into(), page: "https://github.com/filipton/nori/releases/tag/v0.5.0".into() }
        );
        assert!(decide(&Release { prerelease: true, ..latest() }, "0.4.0", &phone, None).is_err());
        assert!(decide(&Release { draft: true, ..latest() }, "0.4.0", &phone, None).is_err());
        assert!(decide(&Release { tag_name: "nightly".into(), ..latest() }, "0.4.0", &phone, None).is_err());
    }

    #[test]
    fn checked_at_most_once_a_day() {
        let day = CHECK_EVERY_MS;
        let now = 1_800_000_000_000;
        assert!(due(true, None, now), "never checked");
        assert!(!due(false, None, now), "switched off");
        assert!(!due(true, Some(now - 1000), now));
        assert!(!due(true, Some(now - day + 1), now));
        assert!(due(true, Some(now - day), now));
        assert!(due(true, Some(now + 5 * day), now), "the clock was set back");
    }

    #[test]
    fn notes_read_as_plain_text() {
        assert_eq!(plain_notes(""), "");
        assert_eq!(plain_notes("\n\n## Added\n\n\n* `nori-cli` gains __bold__\n\n"), "Added\n\n• nori-cli gains bold");
        assert_eq!(plain_notes("one\ntwo\n\nthree"), "one two\n\nthree");
        assert_eq!(plain_notes("# A\n- x\n- y\n# B\ntext"), "A\n• x\n• y\n\nB\ntext");
        assert_eq!(plain_notes("see [the page](https://a/b) and [x] (y)"), "see the page and [x] (y)");
    }
}
