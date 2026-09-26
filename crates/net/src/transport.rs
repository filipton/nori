//! The one door to the network. The platform keeps its HTTP stack (on Android OkHttp: the connection pool
//! shared with the player, TLS, proxies, the reverse-proxy headers) and implements [`Transport`], a single
//! GET. Everything above the socket is decided here: which address to ask, what is retried, what is
//! cached, what kind of failure it was (each client words it). A second player on another platform
//! implements one method and gets all of it.

use std::fmt;

/// Sent with every request, as public APIs like LRCLIB ask.
pub const USER_AGENT: &str = "nori-music/0.1 (+https://github.com/filipton/nori)";

/// What the platform says was wrong when a request did not come back. The platform only sorts its own
/// exceptions into these; each client words them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FailureKind {
    /// The server is set to Wi-Fi only and the phone is on a metered network.
    Metered,
    UnknownHost,
    Connect,
    NoRoute,
    /// The socket waited too long for a byte.
    Timeout,
    /// Any other interrupted exchange, a whole-call timeout among them.
    Interrupted,
    /// The certificate was refused or the handshake failed.
    Tls,
    /// Cleartext HTTP was refused by the platform.
    Cleartext,
    /// Any other I/O failure, an error status with nothing in the body among them.
    Io,
    /// Not an I/O failure at all (a URL the platform could not even build a request from). Never
    /// queued and never retried through the other address: it would fail the same way again.
    Other,
}

impl FailureKind {
    /// Whether the network (not the request) was the problem: those are queued and retried.
    pub fn is_io(self) -> bool {
        self != FailureKind::Other
    }
}

/// One link of what the platform says went wrong: its kind, and its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Failure {
    pub kind: FailureKind,
    pub detail: Option<String>,
}

/// Whether a song failed to play because the server could not be reached (not a bad file or a refused
/// output): what the offline bridge takes over. `status` is the player's own word that the server
/// answered with an error status, or that the connection failed or timed out; `causes` is the chain of
/// what the platform threw. A lookup, a connection or a wait that failed counts, and any I/O failure
/// whose words say "offline"; a refused certificate, a Wi-Fi-only refusal or anything else does not.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn failure_networkish(status: bool, causes: Vec<Failure>) -> bool {
    status
        || causes.iter().any(|c| match c.kind {
            FailureKind::UnknownHost | FailureKind::Connect | FailureKind::NoRoute | FailureKind::Timeout | FailureKind::Interrupted => true,
            FailureKind::Other => false,
            _ => c.detail.as_deref().is_some_and(|d| d.to_lowercase().contains("offline")),
        })
}

/// Whether a plain GET (for callers outside the client, AutoEQ) failed. octo-fiesta reports auth
/// failures as 401 with a normal Subsonic error body, so the body counts either way; only an empty error
/// answer is a failure.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn get_failed(status: u16, body_empty: bool) -> bool {
    body_empty && !(200..=299).contains(&status)
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TransportResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error))]
pub enum TransportError {
    /// `detail` is the platform's own message, data for the log and for a client that has no better
    /// words for the kind.
    Failed { kind: FailureKind, detail: Option<String> },
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let TransportError::Failed { detail, .. } = self;
        f.write_str(detail.as_deref().unwrap_or(""))
    }
}

impl std::error::Error for TransportError {}

#[cfg(feature = "ffi")]
impl From<uniffi::UnexpectedUniFFICallbackError> for TransportError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        TransportError::Failed { kind: FailureKind::Other, detail: Some(e.reason) }
    }
}

/// A request to a third party that wants more than a plain GET: headers of its own (a Referer, the
/// user's key for a service) or a body. Never the music server's: its requests are plain GETs, signed
/// in the address. The platform sends it as it is, with the app's own User-Agent, and hands back
/// whatever came, error statuses included.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Exchange {
    pub url: String,
    pub headers: std::collections::HashMap<String, String>,
    /// POSTed as `application/json` when there is one (YouTube Music's player API); a GET otherwise.
    pub json: Option<String>,
    /// As [`Transport::get`]'s: 0 is the platform's usual timeouts, otherwise the whole call's limit.
    pub timeout_ms: u32,
}

/// One GET, implemented once by the platform. Rust never holds a socket; it asks for bytes here.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// The status and the whole body. `timeout_ms` 0 means the platform's usual timeouts; otherwise
    /// the whole call gives up after that long. Dropping the future cancels the request.
    async fn get(&self, url: String, timeout_ms: u32) -> Result<TransportResponse, TransportError>;

    /// [`Transport::get`] with the headers and body [`Exchange`] says, for the third parties that ask
    /// for them (lyrics services, Apple's catalogue for moving covers).
    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError>;

    /// The address requests go to changed (the LAN / WAN switch), so anything the platform built from
    /// the old one - the signed cover-art prefix - has to be built again.
    fn address_changed(&self);
}

/// Everything a request from the client can end in, as a kind and its facts: each client words it (the
/// Display below is for logs). Kotlin's exception carries no message (uniffi's JNI bindings give none),
/// so its `toString` is that Display.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error), uniffi::export(Display))]
pub enum NetError {
    /// The request did not come back: what kind of failure the platform saw, and its own message.
    Transport { kind: FailureKind, detail: Option<String> },
    /// The server answered with an error `status` and no Subsonic answer in the body (a proxy's page,
    /// nothing at all). Counted as the network's failure: kept for later and tried at the other address.
    Http { status: u16 },
    /// The Subsonic error: its code, and the server's own message.
    Api { code: i32, reason: String },
    /// The address answered, but not with a Subsonic response; `reason` is for the log.
    Parse { reason: String },
    Db { reason: String },
}

impl NetError {
    /// The network failed, not the request: the write is kept for later and the other address is worth a try.
    pub fn is_io(&self) -> bool {
        matches!(self, NetError::Http { .. }) || matches!(self, NetError::Transport { kind, .. } if kind.is_io())
    }

    pub fn io(detail: String) -> Self {
        NetError::Transport { kind: FailureKind::Io, detail: Some(detail) }
    }
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Transport { detail, .. } => f.write_str(detail.as_deref().unwrap_or("")),
            NetError::Http { status } => write!(f, "HTTP {status}"),
            NetError::Api { reason, .. } => f.write_str(reason),
            NetError::Parse { reason } => write!(f, "bad response: {reason}"),
            NetError::Db { reason } => write!(f, "database: {reason}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<nori_model::CoreError> for NetError {
    fn from(e: nori_model::CoreError) -> Self {
        match e {
            nori_model::CoreError::Api { code, reason } => NetError::Api { code, reason },
            nori_model::CoreError::Parse { reason } => NetError::Parse { reason },
            nori_model::CoreError::Db { reason } => NetError::Db { reason },
        }
    }
}

impl From<rusqlite::Error> for NetError {
    fn from(e: rusqlite::Error) -> Self {
        NetError::Db { reason: e.to_string() }
    }
}

impl From<TransportError> for NetError {
    fn from(e: TransportError) -> Self {
        let TransportError::Failed { kind, detail } = e;
        NetError::Transport { kind, detail }
    }
}

/// One GET through the platform. octo-fiesta reports auth failures as 401 with a normal Subsonic error
/// body, so the body is read whatever the status; an error status with nothing in it, or with a page
/// that is no Subsonic answer at all (a proxy's "522: the origin is down"), is a failure, said by its status.
pub async fn get(transport: &dyn Transport, url: String, timeout_ms: u32) -> Result<Vec<u8>, NetError> {
    let r = transport.get(url, timeout_ms).await?;
    if !(200..300).contains(&r.status) && (r.body.is_empty() || !subsonic_body(&r.body)) {
        return Err(NetError::Http { status: r.status });
    }
    Ok(r.body)
}

/// Whether a body is a Subsonic answer (JSON, or the XML one), by its first bytes.
fn subsonic_body(body: &[u8]) -> bool {
    let start = body.iter().position(|b| !b.is_ascii_whitespace()).map_or(&[][..], |i| &body[i..]);
    start.starts_with(b"{") || (start.starts_with(b"<") && body.windows(17).take(512).any(|w| w == b"subsonic-response"))
}

// ---- how the platform's HTTP client is set up ----------------------------------------------------------

/// The platform's HTTP client settings. They are the app's network behaviour, so they live with the rest
/// of it; the platform builds its client from them once.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct NetPolicy {
    pub user_agent: String,
    /// Idle connections close after 20 s, while the radio is still up from the request that used them.
    /// The usual five minutes means every track fetch is followed, minutes later, by a lone FIN that
    /// wakes the modem again.
    pub pool_keep_alive_ms: u32,
    pub pool_max_idle: u32,
    /// OkHttp allows five requests per host by default and shares one dispatcher between every client
    /// built from the same one. A grid of covers then queues five at a time behind whatever else is
    /// running - including a provider stream that octo-fiesta can hold open for minutes - which is what
    /// made artwork crawl on a real server while it looked instant on a small local one. HTTP/2
    /// multiplexes them over the single connection anyway, so a higher cap costs no extra sockets.
    pub max_requests_per_host: u32,
    pub max_requests: u32,
    /// Long streams get their own dispatcher so they cannot occupy the slots the UI needs. Room for the
    /// most downloads the setting allows (10) plus the song playing and the one being fetched ahead:
    /// media3 queues on this dispatcher, so a lower cap would quietly override "Downloads at once".
    pub stream_max_requests_per_host: u32,
    pub stream_max_requests: u32,
    pub connect_timeout_ms: u32,
    pub read_timeout_ms: u32,
    /// octo-fiesta answers a stream request for a provider track only once the whole file is downloaded
    /// on its side, so the first byte can take minutes.
    pub stream_read_timeout_ms: u32,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn net_policy() -> NetPolicy {
    NetPolicy {
        user_agent: USER_AGENT.into(),
        pool_keep_alive_ms: 20_000,
        pool_max_idle: 4,
        max_requests_per_host: 24,
        max_requests: 48,
        stream_max_requests_per_host: 16,
        stream_max_requests: 24,
        connect_timeout_ms: 10_000,
        read_timeout_ms: 30_000,
        stream_read_timeout_ms: 240_000,
    }
}

// ---- which requests go to the music server --------------------------------------------------------------

/// A host and port as a request URL carries them, lower case, with the scheme's default port filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

/// The host and port of a server address written the way people type it (the scheme is optional and
/// means https). Only requests to these get the profile's headers and its Wi-Fi-only rule: third parties
/// (LRCLIB, AutoEQ) must not receive a reverse-proxy token and are not subject to the server's setting.
/// None when the address is blank or not an http(s) URL. Parsed once per profile, not per request.
pub fn server_host(address: String) -> Option<HostPort> {
    if address.chars().all(char::is_whitespace) {
        return None;
    }
    let full = if address.contains("://") { address } else { format!("https://{address}") };
    parse_host(&full)
}

/// The music server's addresses, and whether it may be reached on a metered network: set whenever the
/// server profile changes, read for every request.
static SERVER: parking_lot::RwLock<(Vec<HostPort>, bool)> = parking_lot::RwLock::new((Vec::new(), false));

/// The server profile changed (none: no server): its two addresses and its Wi-Fi-only setting.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn net_server(address: Option<String>, alt_address: Option<String>, wifi_only: bool) {
    let hosts = [address, alt_address].into_iter().flatten().filter_map(server_host).collect();
    *SERVER.write() = (hosts, wifi_only);
}

/// What a request is to the network policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RequestPolicy {
    /// It goes to the music server: it carries the profile's headers (a reverse proxy's token).
    pub server: bool,
    /// It may not go out over a metered network (the server is set to Wi-Fi only). The platform asks
    /// the network only for these, so an ordinary request costs no look at it.
    pub unmetered_only: bool,
}

/// The policy for a request to `url`. Third parties (LRCLIB, AutoEQ) get neither the server's headers nor
/// its Wi-Fi-only rule. Called once per request.
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn request_policy(url: String) -> RequestPolicy {
    let s = SERVER.read();
    let server = !s.0.is_empty() && parse_host(&url).is_some_and(|h| s.0.contains(&h));
    RequestPolicy { server, unmetered_only: server && s.1 }
}

/// The authority of an http(s) URL, read the way the platform's URL parser reads it: surrounding ASCII
/// whitespace ignored, any number of slashes after the scheme, user info dropped, IPv6 in brackets.
fn parse_host(url: &str) -> Option<HostPort> {
    let url = url.trim_matches(|c| matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' '));
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    let default_port = if scheme.eq_ignore_ascii_case("https") {
        443
    } else if scheme.eq_ignore_ascii_case("http") {
        80
    } else {
        return None;
    };
    let rest = url[colon + 1..].trim_start_matches(['/', '\\']);
    let authority = &rest[..rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len())];
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let close = v6.find(']')?;
        let after = &v6[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return None,
        };
        (&v6[..close], port)
    } else {
        match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    // A plain host cannot hold a colon (the first one starts the port); a bracketed IPv6 one is all colons.
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control() || "#%/?@[\\]<>^|".contains(c)) {
        return None;
    }
    let port = match port {
        None => default_port,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 && p.bytes().all(|b| b.is_ascii_digit()) => n,
            _ => return None,
        },
    };
    Some(HostPort { host: host.to_lowercase(), port })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(url: &str, address: &str) -> bool {
        let (Some(u), Some(a)) = (parse_host(url), server_host(address.into())) else { return false };
        u == a
    }

    #[test]
    fn server_headers_only_reach_the_server() {
        assert!(same("https://music.example.com/rest/ping", "music.example.com"));
        assert!(same("http://10.0.2.2:4533/rest/stream?id=1", "http://10.0.2.2:4533/"));
        assert!(!same("http://10.0.2.2:8080/", "http://10.0.2.2:4533"), "another port is another service");
        assert!(!same("https://lrclib.net/api/get", "music.example.com"));
        assert!(!same("https://raw.githubusercontent.com/x", "music.example.com"));
        assert!(!same("https://music.example.com.evil.net/", "music.example.com"));
        assert!(!same("https://music.example.com/", ""));
    }

    #[test]
    fn requests_to_the_server_carry_its_rules() {
        net_server(Some("http://10.0.2.2:4533".into()), Some("music.example.com".into()), true);
        assert_eq!(request_policy("http://10.0.2.2:4533/rest/stream?id=1".into()), RequestPolicy { server: true, unmetered_only: true });
        assert_eq!(request_policy("https://MUSIC.example.com/rest/ping".into()).server, true, "the second address");
        assert_eq!(request_policy("https://lrclib.net/api/get".into()), RequestPolicy { server: false, unmetered_only: false });
        assert!(!request_policy("http://10.0.2.2:8080/".into()).server, "another port is another service");
        net_server(Some("http://10.0.2.2:4533".into()), None, false);
        assert_eq!(request_policy("http://10.0.2.2:4533/x".into()), RequestPolicy { server: true, unmetered_only: false });
        net_server(None, None, true);
        assert!(!request_policy("http://10.0.2.2:4533/x".into()).server, "no server");
    }

    #[test]
    fn a_playback_failure_is_the_networks_when_it_could_not_reach_it() {
        let f = |kind, detail: Option<&str>| Failure { kind, detail: detail.map(str::to_string) };
        assert!(failure_networkish(true, vec![]), "the player said so");
        assert!(!failure_networkish(false, vec![]));
        for k in [FailureKind::UnknownHost, FailureKind::Connect, FailureKind::NoRoute, FailureKind::Timeout, FailureKind::Interrupted] {
            assert!(failure_networkish(false, vec![f(FailureKind::Io, None), f(k, None)]), "{k:?} anywhere in the chain");
        }
        assert!(failure_networkish(false, vec![f(FailureKind::Io, Some("Device is OFFLINE"))]));
        assert!(failure_networkish(false, vec![f(FailureKind::Tls, Some("offline"))]), "any I/O failure's words");
        assert!(!failure_networkish(false, vec![f(FailureKind::Other, Some("offline"))]), "not an I/O failure");
        assert!(!failure_networkish(false, vec![f(FailureKind::Metered, Some("This server is set to Wi-Fi only"))]));
        assert!(!failure_networkish(false, vec![f(FailureKind::Io, Some("bad file"))]));
        assert!(!failure_networkish(false, vec![f(FailureKind::Tls, None), f(FailureKind::Cleartext, None)]));
    }

    #[test]
    fn an_error_status_with_a_page_that_is_no_subsonic_answer_is_its_status() {
        assert!(subsonic_body(br#"  {"subsonic-response":{"status":"failed"}}"#));
        assert!(subsonic_body(br#"<?xml version="1.0"?><subsonic-response status="failed"/>"#));
        assert!(!subsonic_body(b"<html><body>error code: 522</body></html>"));
        assert!(!subsonic_body(b""));
    }

    #[test]
    fn a_get_fails_only_on_an_empty_error() {
        assert!(!get_failed(200, true));
        assert!(!get_failed(401, false), "an error with a body is read");
        assert!(get_failed(404, true));
        assert!(get_failed(301, true));
    }

    #[test]
    fn addresses_parse_like_urls() {
        assert_eq!(server_host("HTTP://Music.Example.com".into()), Some(HostPort { host: "music.example.com".into(), port: 80 }));
        assert_eq!(server_host("https://u:p@h:8443/x".into()), Some(HostPort { host: "h".into(), port: 8443 }));
        assert_eq!(server_host("http://[::1]:4533".into()), Some(HostPort { host: "::1".into(), port: 4533 }));
        assert_eq!(server_host("   ".into()), None);
        assert_eq!(server_host("ftp://h".into()), None);
        assert_eq!(server_host("h:0".into()), None);
        assert_eq!(server_host("h:99999".into()), None);
        assert_eq!(server_host("h b".into()), None);
    }
}
