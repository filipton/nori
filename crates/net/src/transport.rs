//! The platform's HTTP transport ([`Transport`]), its failure kinds, and the network policy the platform
//! builds its HTTP client from.

use std::fmt;

/// Sent with every request, as public APIs like LRCLIB ask.
pub const USER_AGENT: &str = "nori-music/0.1 (+https://github.com/filipton/nori)";

/// Why a request did not come back, as the platform classifies its exceptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FailureKind {
    /// The server is set to Wi-Fi only and the network is metered.
    Metered,
    UnknownHost,
    Connect,
    NoRoute,
    /// Socket read timeout.
    Timeout,
    /// Any other interrupted exchange, including a whole-call timeout.
    Interrupted,
    /// Certificate refused or handshake failed.
    Tls,
    /// Cleartext HTTP refused by the platform.
    Cleartext,
    /// Any other I/O failure, including an error status with an empty body.
    Io,
    /// Not an I/O failure (e.g. an unbuildable URL): never queued or retried at the other address.
    Other,
}

impl FailureKind {
    /// Whether the network, not the request, failed (such requests are queued and retried).
    pub fn is_io(self) -> bool {
        self != FailureKind::Other
    }
}

/// One link of the platform's exception chain.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Failure {
    pub kind: FailureKind,
    pub detail: Option<String>,
}

/// Whether a playback failure means the server was unreachable (the offline bridge takes over).
/// `status`: the player saw an error status or a failed/timed-out connection; `causes`: the platform's
/// exception chain. Lookup, connect and timeout failures count, as does any I/O failure mentioning
/// "offline".
#[cfg_attr(feature = "ffi", uniffi::export)]
pub fn failure_networkish(status: bool, causes: Vec<Failure>) -> bool {
    status
        || causes.iter().any(|c| match c.kind {
            FailureKind::UnknownHost | FailureKind::Connect | FailureKind::NoRoute | FailureKind::Timeout | FailureKind::Interrupted => true,
            FailureKind::Other => false,
            _ => c.detail.as_deref().is_some_and(|d| d.to_lowercase().contains("offline")),
        })
}

/// Whether a plain GET outside the client (AutoEQ) failed: only an error status with an empty body.
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
    /// `detail` is the platform's own message.
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

/// A third-party request with its own headers or a JSON body. Error statuses are returned, not raised.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Exchange {
    pub url: String,
    pub headers: std::collections::HashMap<String, String>,
    /// POSTed as `application/json` when set; a GET otherwise.
    pub json: Option<String>,
    /// As in [`Transport::get`].
    pub timeout_ms: u32,
}

/// The platform's HTTP client.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// The status and whole body. `timeout_ms` 0 uses the platform's default timeouts, otherwise it
    /// limits the whole call. Dropping the future cancels the request.
    async fn get(&self, url: String, timeout_ms: u32) -> Result<TransportResponse, TransportError>;

    /// [`Transport::get`] with the headers and body of an [`Exchange`].
    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError>;

    /// The server address changed (LAN / WAN switch); the platform rebuilds what it derived from it.
    fn address_changed(&self);

    /// The network requests go out on now.
    fn network(&self) -> Network;
}

/// What the network requests go out on costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Network {
    Unmetered,
    Metered,
}

/// Wakes the parked thread.
struct Unpark(std::thread::Thread);

impl std::task::Wake for Unpark {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs `f` to completion on this thread, parking while it waits (a platform transport answers from its
/// own threads; a blocking one is ready at the first poll).
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let waker = std::task::Waker::from(std::sync::Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

/// A failed client request. Display is for logs and is Kotlin's `toString` (uniffi gives no message).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error), uniffi::export(Display))]
pub enum NetError {
    /// The request did not come back.
    Transport { kind: FailureKind, detail: Option<String> },
    /// An error status without a Subsonic body (e.g. a proxy's page). Treated as a network failure.
    Http { status: u16 },
    /// A Subsonic error response.
    Api { code: i32, reason: String },
    /// The answer was not a valid Subsonic response.
    Parse { reason: String },
    Db { reason: String },
}

impl NetError {
    /// The network failed, not the request: writes are kept for later and the other address is tried.
    pub fn is_io(&self) -> bool {
        matches!(self, NetError::Http { .. }) || matches!(self, NetError::Transport { kind, .. } if kind.is_io())
    }

    /// The request never left: the next try cannot be a repeat.
    pub fn nothing_sent(&self) -> bool {
        use FailureKind::*;
        matches!(self, NetError::Transport { kind: UnknownHost | Connect | NoRoute | Metered | Cleartext | Tls, .. })
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
            e @ nori_model::CoreError::Smart { .. } => NetError::Parse { reason: e.to_string() },
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

/// One GET through the platform. An error status still returns its body when it is a Subsonic answer
/// (octo-fiesta sends auth failures as 401 with a Subsonic error body); otherwise it is [`NetError::Http`].
pub async fn get(transport: &dyn Transport, url: String, timeout_ms: u32) -> Result<Vec<u8>, NetError> {
    let r = transport.get(url, timeout_ms).await?;
    if !(200..300).contains(&r.status) && (r.body.is_empty() || !subsonic_body(&r.body)) {
        return Err(NetError::Http { status: r.status });
    }
    Ok(r.body)
}

/// Whether a body looks like a Subsonic answer (JSON, or XML naming `subsonic-response`).
fn subsonic_body(body: &[u8]) -> bool {
    let start = body.iter().position(|b| !b.is_ascii_whitespace()).map_or(&[][..], |i| &body[i..]);
    start.starts_with(b"{") || (start.starts_with(b"<") && body.windows(17).take(512).any(|w| w == b"subsonic-response"))
}

/// Settings the platform builds its HTTP client from.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct NetPolicy {
    pub user_agent: String,
    /// Short, so idle connections close while the radio is still up rather than minutes later with a
    /// lone FIN that wakes the modem again.
    pub pool_keep_alive_ms: u32,
    pub pool_max_idle: u32,
    /// Above OkHttp's default of 5, so covers do not queue behind long-held streams. HTTP/2 multiplexes
    /// them over one connection, so this costs no extra sockets.
    pub max_requests_per_host: u32,
    pub max_requests: u32,
    /// Streams use their own dispatcher so they cannot take the UI's slots. Must fit the most
    /// concurrent downloads the setting allows (10) plus the playing and prefetched songs.
    pub stream_max_requests_per_host: u32,
    pub stream_max_requests: u32,
    pub connect_timeout_ms: u32,
    pub read_timeout_ms: u32,
    /// Long: octo-fiesta sends a provider track's first byte only after downloading the whole file.
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

/// A URL's host (lower case) and port (the scheme's default when absent).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

/// The host and port of a server address as typed (scheme optional, https by default). None when blank
/// or not http(s).
fn server_host(address: String) -> Option<HostPort> {
    if address.chars().all(char::is_whitespace) {
        return None;
    }
    let full = if address.contains("://") { address } else { format!("https://{address}") };
    parse_host(&full)
}

/// The music server's hosts and its Wi-Fi-only setting, for one server profile (none: no server).
#[derive(Debug, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct ServerHosts {
    hosts: Vec<HostPort>,
    wifi_only: bool,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl ServerHosts {
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new(address: Option<String>, alt_address: Option<String>, wifi_only: bool) -> Self {
        ServerHosts { hosts: [address, alt_address].into_iter().flatten().filter_map(server_host).collect(), wifi_only }
    }

    /// The policy for a request to `url`. Third parties (LRCLIB, AutoEQ) get neither the server's headers
    /// nor its Wi-Fi-only rule.
    pub fn policy(&self, url: &str) -> RequestPolicy {
        let server = !self.hosts.is_empty() && parse_host(url).is_some_and(|h| self.hosts.contains(&h));
        RequestPolicy { server, unmetered_only: server && self.wifi_only }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RequestPolicy {
    /// Goes to the music server, so it carries the profile's headers (e.g. a reverse proxy's token).
    pub server: bool,
    /// Must not use a metered network. The platform checks the network only for these.
    pub unmetered_only: bool,
}

/// The authority of an http(s) URL, parsed as the platform's URL parser does: surrounding ASCII
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

    #[test]
    fn hosts() {
        let s = ServerHosts::new(Some("http://10.0.2.2:4533/".into()), Some("music.example.com".into()), true);
        let server = RequestPolicy { server: true, unmetered_only: true };
        let other = RequestPolicy { server: false, unmetered_only: false };
        assert_eq!(s.policy("http://10.0.2.2:4533/rest/stream?id=1"), server);
        assert_eq!(s.policy("https://MUSIC.example.com/rest/ping"), server, "the second address");
        assert_eq!(s.policy("https://lrclib.net/api/get"), other);
        assert_eq!(s.policy("http://10.0.2.2:8080/"), other, "another port is another service");
        assert_eq!(s.policy("https://music.example.com.evil.net/"), other);
        let s = ServerHosts::new(Some("http://10.0.2.2:4533".into()), None, false);
        assert_eq!(s.policy("http://10.0.2.2:4533/x"), RequestPolicy { server: true, unmetered_only: false });
        assert_eq!(ServerHosts::new(Some(" ".into()), None, true).policy("https://music.example.com/"), other);

        // Server host parses like urls.
        assert_eq!(server_host("HTTP://Music.Example.com".into()), Some(HostPort { host: "music.example.com".into(), port: 80 }));
        assert_eq!(server_host("https://u:p@h:8443/x".into()), Some(HostPort { host: "h".into(), port: 8443 }));
        assert_eq!(server_host("http://[::1]:4533".into()), Some(HostPort { host: "::1".into(), port: 4533 }));
        assert_eq!(server_host("music.example.com".into()), Some(HostPort { host: "music.example.com".into(), port: 443 }));
        assert_eq!(server_host("   ".into()), None);
        assert_eq!(server_host("ftp://h".into()), None);
        assert_eq!(server_host("h:0".into()), None);
        assert_eq!(server_host("h:99999".into()), None);
        assert_eq!(server_host("h b".into()), None);
    }

    #[test]
    fn failures() {
        let f = |kind, detail: Option<&str>| Failure { kind, detail: detail.map(str::to_string) };
        assert!(failure_networkish(true, vec![]));
        assert!(!failure_networkish(false, vec![]));
        for k in [FailureKind::UnknownHost, FailureKind::Connect, FailureKind::NoRoute, FailureKind::Timeout, FailureKind::Interrupted] {
            assert!(failure_networkish(false, vec![f(FailureKind::Io, None), f(k, None)]), "{k:?} anywhere in the chain");
        }
        assert!(failure_networkish(false, vec![f(FailureKind::Io, Some("Device is OFFLINE"))]));
        assert!(failure_networkish(false, vec![f(FailureKind::Tls, Some("offline"))]));
        assert!(!failure_networkish(false, vec![f(FailureKind::Other, Some("offline"))]));
        assert!(!failure_networkish(false, vec![f(FailureKind::Metered, Some("This server is set to Wi-Fi only"))]));
        assert!(!failure_networkish(false, vec![f(FailureKind::Io, Some("bad file"))]));
        assert!(!failure_networkish(false, vec![f(FailureKind::Tls, None), f(FailureKind::Cleartext, None)]));

        // Subsonic body detection.
        assert!(subsonic_body(br#"  {"subsonic-response":{"status":"failed"}}"#));
        assert!(subsonic_body(br#"<?xml version="1.0"?><subsonic-response status="failed"/>"#));
        assert!(!subsonic_body(b"<html><body>error code: 522</body></html>"));
        assert!(!subsonic_body(b""));

        // Get fails only on empty error.
        assert!(!get_failed(200, true));
        assert!(!get_failed(401, false));
        assert!(get_failed(404, true));
        assert!(get_failed(301, true));
    }

}
