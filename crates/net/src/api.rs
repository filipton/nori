//! Subsonic request signing and URL building.

use md5::{Digest, Md5};

pub const CLIENT: &str = "nori";
pub(crate) const API_VERSION: &str = "1.16.1";

#[derive(Debug, Clone, Default)]
pub struct Server {
    pub base: String,
    /// Pre-encoded `u=..&t=..&s=..&v=..&c=..&f=json`
    auth: String,
}

fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

/// Percent-encodes `v` into `out` (RFC 3986 unreserved characters kept).
fn encode(out: &mut String, v: &str) {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => {
                out.push('%');
                out.push(H[(b >> 4) as usize] as char);
                out.push(H[(b & 15) as usize] as char);
            }
        }
    }
}

/// How a request proves who it is from.
pub enum Auth<'a> {
    /// `t` + `s`: md5(password + salt).
    Token { user: &'a str, password: &'a str },
    /// `p=enc:<hex>`, for servers without token auth (error 41).
    Legacy { user: &'a str, password: &'a str },
    /// OpenSubsonic `apiKey`, which replaces the user name too.
    ApiKey(&'a str),
}

/// Trims the address, drops a trailing `/rest` and defaults the scheme to https.
fn normalize(base: &str) -> String {
    let mut base = base.trim().trim_end_matches('/').to_string();
    if let Some(b) = base.strip_suffix("/rest") {
        base = b.to_string();
    }
    if !base.is_empty() && !base.contains("://") {
        base = format!("https://{base}");
    }
    base
}

impl Server {
    #[cfg(test)]
    pub fn new(base: &str, user: &str, password: &str) -> Self {
        Self::with(base, Auth::Token { user, password })
    }

    /// The token salt is derived from the address and user, not random, so URLs stay stable across
    /// sessions and HTTP/media caches keep hitting.
    pub fn with(base: &str, auth: Auth) -> Self {
        let base = normalize(base);
        let mut q = String::new();
        match auth {
            Auth::Token { user, password } => {
                let salt = &hex(&Md5::digest(format!("nori:{base}:{user}").as_bytes()))[..12];
                let token = hex(&Md5::digest(format!("{password}{salt}").as_bytes()));
                q.push_str("u=");
                encode(&mut q, user);
                q.push_str(&format!("&t={token}&s={salt}"));
            }
            Auth::Legacy { user, password } => {
                q.push_str("u=");
                encode(&mut q, user);
                q.push_str("&p=enc:");
                q.push_str(&hex(password.as_bytes()));
            }
            Auth::ApiKey(key) => {
                q.push_str("apiKey=");
                encode(&mut q, key);
            }
        }
        q.push_str(&format!("&v={API_VERSION}&c={CLIENT}&f=json"));
        Server { base, auth: q }
    }

    /// Same credentials at another address (LAN / WAN switch).
    pub fn rebased(&self, base: &str) -> Self {
        Server { base: normalize(base), auth: self.auth.clone() }
    }

    pub fn url(&self, endpoint: &str, params: &[(String, String)]) -> String {
        let mut u = String::with_capacity(self.base.len() + self.auth.len() + 64);
        u.push_str(&self.base);
        u.push_str("/rest/");
        u.push_str(endpoint);
        u.push('?');
        u.push_str(&self.auth);
        for (k, v) in params {
            u.push('&');
            u.push_str(k);
            u.push('=');
            encode(&mut u, v);
        }
        u
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_signed_and_encoded() {
        let s = Server::new("example.com/rest/", "jo e", "sesame");
        let a = s.url("search3", &[("query".into(), "a b&c".into())]);
        assert_eq!(a, s.url("search3", &[("query".into(), "a b&c".into())]));
        assert!(a.starts_with("https://example.com/rest/search3?u=jo%20e&t="));
        assert!(a.ends_with("&f=json&query=a%20b%26c"));

        let legacy = Server::with("http://h", Auth::Legacy { user: "u", password: "ab" }).url("ping", &[]);
        assert!(legacy.contains("u=u&p=enc:6162&v="));
        let key = Server::with("http://h", Auth::ApiKey("k y")).url("ping", &[]);
        assert!(key.contains("/rest/ping?apiKey=k%20y&v=") && !key.contains("u="));
        assert_eq!(Server::new("h", "u", "p").rebased("http://lan:4533/").url("ping", &[]).split('?').next(), Some("http://lan:4533/rest/ping"));
    }
}
