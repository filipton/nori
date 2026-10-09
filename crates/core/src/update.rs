//! App update check: GitHub's latest release compared by semver, the APK picked for the device ABIs, and
//! a daily throttle. Downloading and installing is the platform's.

use serde::Deserialize;
use std::cmp::Ordering;

use crate::client::{Client, NetResult};
use crate::transport::{Exchange, NetError};

/// Latest published release (excludes drafts and prereleases).
pub(crate) const LATEST_URL: &str = "https://api.github.com/repos/norifm/nori/releases/latest";
/// The newest releases, prereleases among them, for the beta channel.
pub(crate) const RECENT_URL: &str = "https://api.github.com/repos/norifm/nori/releases?per_page=20";
/// Minimum interval between automatic checks.
pub(crate) const CHECK_EVERY_MS: i64 = 24 * 60 * 60 * 1000;
/// `app_kv` key: last check time (ms).
const CHECKED_KEY: &str = "update.checkedMs";
/// `app_kv` key: the version the user postponed.
const SKIPPED_KEY: &str = "update.skipped";
/// `app_kv` key: the version being installed and its notes, "version\nnotes".
const CHANGELOG_KEY: &str = "update.changelog";
/// Request timeout.
const TIMEOUT_MS: u32 = 20_000;

/// A semver version; build metadata is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl Version {
    /// Parses "0.4.1", "v0.4.1", "0.5.0-rc.1" or "1.2" (patch 0).
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

/// A release file.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Asset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub browser_download_url: String,
}

/// The fields read from GitHub's release JSON.
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

/// Android ABIs; an APK suffixed with one targets only it.
const ABIS: [&str; 4] = ["arm64-v8a", "armeabi-v7a", "x86_64", "x86"];

/// The non-empty APK for the first of `abis` (preference order) that has one, else the ABI-less APK.
pub(crate) fn pick_apk<'a>(assets: &'a [Asset], abis: &[String]) -> Option<&'a Asset> {
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

/// A newer release and its APK.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AppUpdate {
    /// The tag without its "v".
    pub version: String,
    /// Plain-text notes ([`plain_notes`]).
    pub notes: String,
    pub page: String,
    pub apk_name: String,
    pub apk_url: String,
    /// Expected APK size in bytes.
    pub apk_bytes: u64,
}

/// An update check's outcome.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum UpdateCheck {
    /// Disabled or checked within the last day.
    NotDue,
    /// This build is at least the latest release.
    UpToDate { latest: String },
    /// A newer release with a matching APK; `skipped` when the user postponed this version.
    Available { update: AppUpdate, skipped: bool },
    /// A newer release without an APK for this device.
    NoApk { version: String, page: String },
}

/// The newest of `releases` a channel offers: published ones, prereleases only on the beta channel.
pub(crate) fn newest(releases: Vec<Release>, beta: bool) -> Option<Release> {
    releases.into_iter().filter(|r| !r.draft && (beta || !r.prerelease)).filter_map(|r| Version::parse(&r.tag_name).map(|v| (v, r))).max_by(|a, b| a.0.cmp(&b.0)).map(|(_, r)| r)
}

/// The check outcome for `release` against build `current`; an error for drafts, prereleases off the
/// beta channel and unparseable tags.
pub fn decide(release: &Release, current: &str, abis: &[String], skipped: Option<&str>, beta: bool) -> Result<UpdateCheck, String> {
    if release.draft || (release.prerelease && !beta) {
        return Err(format!("{} is a draft or a prerelease", release.tag_name));
    }
    let latest = Version::parse(&release.tag_name).ok_or_else(|| format!("tag {:?} is no version", release.tag_name))?;
    let shown = release.tag_name.trim().trim_start_matches(['v', 'V']).to_string();
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

/// Whether the automatic check is due: enabled, and never checked, a day ago, or in the future (clock
/// set back).
pub fn due(on: bool, checked_ms: Option<i64>, now_ms: i64) -> bool {
    on && checked_ms.is_none_or(|t| t > now_ms || now_ms - t >= CHECK_EVERY_MS)
}

/// What becomes of the notes kept for an update being installed, once build `current` runs.
#[derive(Debug, PartialEq)]
pub(crate) enum KeptNotes<'a> {
    /// `current` is the version they were kept for: shown now, then forgotten.
    Show(&'a str),
    /// Kept for a newer version than `current`: the install has not happened yet.
    Wait,
    /// For an older version, unreadable, or empty: forgotten unseen.
    Forget,
}

/// The notes kept as `kept` ("version\nnotes") against the running build `current`.
pub(crate) fn kept_notes<'a>(kept: &'a str, current: &str) -> KeptNotes<'a> {
    let Some((version, notes)) = kept.split_once('\n') else { return KeptNotes::Forget };
    match (Version::parse(version), Version::parse(current)) {
        (Some(v), Some(c)) if v == c && !notes.is_empty() => KeptNotes::Show(notes),
        (Some(v), Some(c)) if v > c => KeptNotes::Wait,
        _ => KeptNotes::Forget,
    }
}

/// Markdown notes as plain text: headings unmarked, list items as bullets, wrapped paragraphs joined,
/// emphasis and code marks dropped, links as their text, single blank lines between blocks.
pub(crate) fn plain_notes(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    // Whether the next source line continues the last output line.
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

/// Strips inline Markdown: emphasis, code ticks, `[text](url)` to text.
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

#[cfg_attr(feature = "ffi", uniffi::export)]
impl Client {
    /// Postpones `version`: automatic checks mark it `skipped`.
    pub fn update_skip(&self, version: String) {
        self.settings().keep_app_value(SKIPPED_KEY, version);
    }

    /// `update` is about to be installed: its notes are kept for [`Client::update_changelog`].
    pub fn update_installing(&self, update: AppUpdate) {
        self.settings().keep_app_value(CHANGELOG_KEY, format!("{}\n{}", update.version, update.notes));
    }

    /// The notes of the update that installed build `version`, once: the first call after the update
    /// returns them, later ones None.
    pub fn update_changelog(&self, version: String) -> Option<String> {
        let kept = self.settings().app_value(CHANGELOG_KEY)?;
        let shown = match kept_notes(&kept, &version) {
            KeptNotes::Wait => return None,
            KeptNotes::Forget => None,
            KeptNotes::Show(notes) => Some(notes.to_string()),
        };
        self.settings().forget_app_value(CHANGELOG_KEY);
        shown
    }

    /// Checks GitHub's latest release against `version` for `abis`. Unless `asked`, only when [`due`]; the
    /// check time is stored before the request so failures are not retried the same day.
    pub async fn update_check(&self, asked: bool, version: String, abis: Vec<String>) -> NetResult<UpdateCheck> {
        let now = crate::db::now_ms();
        if !asked {
            let on = self.settings().prefs(|p| p.update_check);
            let checked = self.settings().app_value(CHECKED_KEY).and_then(|v| v.parse::<i64>().ok());
            if !due(on, checked, now) {
                return Ok(UpdateCheck::NotDue);
            }
        }
        self.settings().keep_app_value(CHECKED_KEY, now.to_string());
        let beta = self.settings().prefs(|p| p.update_beta);
        let request = Exchange {
            url: if beta { RECENT_URL } else { LATEST_URL }.to_string(),
            headers: [("Accept".to_string(), "application/vnd.github+json".to_string()), ("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string())].into(),
            json: None,
            timeout_ms: TIMEOUT_MS,
        };
        let r = self.transport.send(request).await?;
        if !(200..300).contains(&r.status) {
            return Err(NetError::Http { status: r.status });
        }
        let parse = |e: serde_json::Error| NetError::Parse { reason: format!("latest release: {e}") };
        let release: Release = if beta {
            let all: Vec<Release> = serde_json::from_slice(&r.body).map_err(parse)?;
            newest(all, true).ok_or_else(|| NetError::Parse { reason: "no release".into() })?
        } else {
            serde_json::from_slice(&r.body).map_err(parse)?
        };
        let skipped = if asked { None } else { self.settings().app_value(SKIPPED_KEY) };
        let found = decide(&release, &version, &abis, skipped.as_deref(), beta).map_err(|reason| NetError::Parse { reason })?;
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
    fn versions_and_assets() {
        assert_eq!(v("v0.4.1"), Version { major: 0, minor: 4, patch: 1, pre: vec![] });
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("0.4.0+abc123"), v("0.4.0"));
        for bad in ["", "v", "x1.0.0", "1..0", "1.0.0.0", "1.0.0-", "1.0.0-a..b", "1.a.0", "-1.0.0"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        // semver.org's example chain.
        let chain = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"];
        for w in chain.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert!(v("0.10.0") > v("0.9.9"), "numbers, not text");
        assert!(v("1.0.0") > v("0.99.99"));

        // Apk is picked by abi preference.
        let both = [asset("nori-music-0.5.0.apk"), asset("SHA256SUMS")];
        assert_eq!(pick_apk(&both, &abis(&["arm64-v8a", "armeabi-v7a"])).unwrap().name, "nori-music-0.5.0.apk");
        let split = [asset("SHA256SUMS"), asset("nori-music-0.5.0-x86_64.apk"), asset("nori-music-0.5.0-arm64-v8a.apk"), asset("nori-music-0.5.0-armeabi-v7a.apk")];
        assert_eq!(pick_apk(&split, &abis(&["arm64-v8a", "armeabi-v7a", "armeabi"])).unwrap().name, "nori-music-0.5.0-arm64-v8a.apk");
        assert_eq!(pick_apk(&split, &abis(&["x86_64", "arm64-v8a"])).unwrap().name, "nori-music-0.5.0-x86_64.apk", "device order wins");
        assert_eq!(pick_apk(&split, &abis(&["armeabi-v7a"])).unwrap().name, "nori-music-0.5.0-armeabi-v7a.apk");
        assert_eq!(pick_apk(&[asset("nori-music-0.5.0-x86_64.apk")], &abis(&["x86"])), None);
        // ABI-specific beats universal; another ABI's never.
        let mixed = [asset("nori-music-0.5.0.apk"), asset("nori-music-0.5.0-arm64-v8a.apk")];
        assert_eq!(pick_apk(&mixed, &abis(&["arm64-v8a"])).unwrap().name, "nori-music-0.5.0-arm64-v8a.apk");
        assert_eq!(pick_apk(&mixed, &abis(&["x86_64"])).unwrap().name, "nori-music-0.5.0.apk");
        let empty = Asset { size: 0, ..asset("nori-music-0.5.0.apk") };
        assert_eq!(pick_apk(&[empty], &abis(&["arm64-v8a"])), None, "an empty file");
        assert_eq!(pick_apk(&[asset("SHA256SUMS")], &abis(&["arm64-v8a"])), None);
    }

    fn asset(name: &str) -> Asset {
        Asset { name: name.into(), size: 1000, browser_download_url: format!("https://github.com/norifm/nori/releases/download/v1/{name}") }
    }

    fn abis(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// A trimmed GitHub answer, with extra fields.
    const LATEST: &str = r#"{"url":"https://api.github.com/repos/norifm/nori/releases/1","html_url":"https://github.com/norifm/nori/releases/tag/v0.5.0",
      "id":1,"author":{"login":"norifm"},"tag_name":"v0.5.0","name":"nori 0.5.0","draft":false,"prerelease":false,
      "assets":[{"name":"nori-music-0.5.0.apk","size":63901760,"content_type":"application/vnd.android.package-archive",
                 "browser_download_url":"https://github.com/norifm/nori/releases/download/v0.5.0/nori-music-0.5.0.apk"},
                {"name":"SHA256SUMS","size":87,"browser_download_url":"https://github.com/norifm/nori/releases/download/v0.5.0/SHA256SUMS"}],
      "body":"The **player** is faster.\n\n### Fixed\n\n- A song\n  that was cut off\n- [Undo](https://x.y/1) works"}"#;

    fn latest() -> Release {
        serde_json::from_str(LATEST).unwrap()
    }

    #[test]
    fn channel_takes_the_newest_it_offers() {
        let r = |tag: &str, prerelease: bool, draft: bool| Release { tag_name: tag.into(), prerelease, draft, ..latest() };
        let tag = |r: Option<Release>| r.map(|r| r.tag_name);
        let found = || vec![r("v0.5.1", false, false), r("v0.5.2-beta.2", true, false), r("v0.5.2-beta.3", true, true), r("v0.5.2-beta.1", true, false), r("nightly", true, false)];
        assert_eq!(tag(newest(found(), true)), Some("v0.5.2-beta.2".into()), "the newest beta, not the draft");
        assert_eq!(tag(newest(found(), false)), Some("v0.5.1".into()), "stable only off the beta channel");
        let released = vec![r("v0.5.2", false, false), r("v0.5.2-beta.2", true, false)];
        assert_eq!(tag(newest(released, true)), Some("v0.5.2".into()), "the release outranks its betas");
    }

    #[test]
    fn offers() {
        let phone = abis(&["arm64-v8a"]);
        let UpdateCheck::Available { update, skipped } = decide(&latest(), "0.4.0", &phone, None, false).unwrap() else { panic!("offered") };
        assert!(!skipped);
        assert_eq!(update.version, "0.5.0");
        assert_eq!(update.apk_name, "nori-music-0.5.0.apk");
        assert_eq!(update.apk_bytes, 63_901_760);
        assert_eq!(update.page, "https://github.com/norifm/nori/releases/tag/v0.5.0");
        assert_eq!(update.notes, "The player is faster.\n\nFixed\n\n• A song that was cut off\n• Undo works");
        // Postponed: still found, marked skipped.
        assert!(matches!(decide(&latest(), "0.4.0", &phone, Some("0.5.0"), false).unwrap(), UpdateCheck::Available { skipped: true, .. }));
        assert!(matches!(decide(&latest(), "0.4.0", &phone, Some("v0.5.0"), false).unwrap(), UpdateCheck::Available { skipped: true, .. }));
        assert!(matches!(decide(&latest(), "0.4.0", &phone, Some("0.4.9"), false).unwrap(), UpdateCheck::Available { skipped: false, .. }));
        assert_eq!(decide(&latest(), "0.5.0", &phone, None, false).unwrap(), UpdateCheck::UpToDate { latest: "0.5.0".into() });
        assert_eq!(decide(&latest(), "0.6.0-dev", &phone, None, false).unwrap(), UpdateCheck::UpToDate { latest: "0.5.0".into() });
        assert!(matches!(decide(&latest(), "0.5.0-rc.2", &phone, None, false).unwrap(), UpdateCheck::Available { .. }));
        let only_x86 = Release { assets: vec![asset("nori-music-0.5.0-x86_64.apk")], ..latest() };
        assert_eq!(
            decide(&only_x86, "0.4.0", &phone, None, false).unwrap(),
            UpdateCheck::NoApk { version: "0.5.0".into(), page: "https://github.com/norifm/nori/releases/tag/v0.5.0".into() }
        );
        assert!(decide(&Release { prerelease: true, ..latest() }, "0.4.0", &phone, None, false).is_err());
        assert!(decide(&Release { draft: true, ..latest() }, "0.4.0", &phone, None, true).is_err());
        assert!(decide(&Release { tag_name: "nightly".into(), ..latest() }, "0.4.0", &phone, None, false).is_err());
        // The beta channel takes a prerelease.
        let beta = Release { prerelease: true, tag_name: "v0.5.0-beta.2".into(), ..latest() };
        assert!(matches!(decide(&beta, "0.4.0", &phone, None, true).unwrap(), UpdateCheck::Available { .. }));
        assert!(matches!(decide(&beta, "0.5.0-beta.1", &phone, None, true).unwrap(), UpdateCheck::Available { .. }));

        // Due at most daily.
        let day = CHECK_EVERY_MS;
        let now = 1_800_000_000_000;
        assert!(due(true, None, now));
        assert!(!due(false, None, now));
        assert!(!due(true, Some(now - 1000), now));
        assert!(!due(true, Some(now - day + 1), now));
        assert!(due(true, Some(now - day), now));
        assert!(due(true, Some(now + 5 * day), now), "clock set back");

        // Plain notes strip markdown.
        assert_eq!(plain_notes(""), "");
        assert_eq!(plain_notes("\n\n## Added\n\n\n* `nori-cli` gains __bold__\n\n"), "Added\n\n• nori-cli gains bold");
        assert_eq!(plain_notes("one\ntwo\n\nthree"), "one two\n\nthree");
        assert_eq!(plain_notes("# A\n- x\n- y\n# B\ntext"), "A\n• x\n• y\n\nB\ntext");
        assert_eq!(plain_notes("see [the page](https://a/b) and [x] (y)"), "see the page and [x] (y)");
    }

    #[test]
    fn kept_notes_by_version() {
        for (kept, current, want) in [
            ("0.5.0\nFixed", "0.5.0", KeptNotes::Show("Fixed")),
            ("0.5.0\nFixed", "v0.5.0", KeptNotes::Show("Fixed")),
            ("0.5.0\nFixed", "0.4.0", KeptNotes::Wait),
            ("0.5.0\nFixed", "0.5.1", KeptNotes::Forget),
            ("0.5.0\n", "0.5.0", KeptNotes::Forget),
            ("0.5.0", "0.5.0", KeptNotes::Forget),
            ("nightly\nFixed", "0.5.0", KeptNotes::Forget),
        ] {
            assert_eq!(kept_notes(kept, current), want, "{kept:?} on {current}");
        }
    }

    #[test]
    fn changelog_shows_once_after_the_install() {
        let (c, _) = crate::client::tests::client(Default::default());
        let dir = nori_testdir::TempDir::new("update-changelog");
        c.session().settings.open(&dir.join("app.db").to_string_lossy()).unwrap();
        let UpdateCheck::Available { update, .. } = decide(&latest(), "0.4.0", &abis(&["arm64-v8a"]), None, false).unwrap() else { panic!("offered") };
        c.update_installing(update);
        nori_db::background::flush();
        assert_eq!(c.update_changelog("0.4.0".into()), None, "not installed yet");
        nori_db::background::flush();
        assert_eq!(c.update_changelog("0.5.0".into()).as_deref(), Some("The player is faster.\n\nFixed\n\n• A song that was cut off\n• Undo works"));
        nori_db::background::flush();
        assert_eq!(c.update_changelog("0.5.0".into()), None, "only once");
    }

}
