//! Desktop HTTP: the core's [`Transport`] and the engine's [`ByteSource`] over one ureq agent (rustls),
//! so API calls, covers and audio share one connection pool.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use nori_core::transport::{Exchange, FailureKind, Transport, TransportError, TransportResponse, USER_AGENT};
use nori_engine::{Body, ByteSource, Cancel, OpenError};
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{Buffers, ConnectProxyConnector, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, RustlsConnector};
use ureq::{Agent, RequestBuilder};

/// Largest API response body read.
const MAX_ANSWER: u64 = 256 * 1024 * 1024;

pub struct Http {
    agent: Agent,
}

impl Http {
    pub fn new() -> Arc<Http> {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build();
        let connector = ().chain(ConnectProxyConnector::default()).chain(Sockets).chain(RustlsConnector::default()).chain(Cancellable);
        let agent = Agent::with_parts(config, connector, DefaultResolver::default());
        Arc::new(Http { agent })
    }

    /// `url`'s total length from a `bytes=0-0` request; None unless the server answers 206.
    fn total_length(&self, url: &str) -> Option<u64> {
        let r = self.agent.get(url).header("Range", "bytes=0-0").call().ok()?;
        if r.status().as_u16() != 206 {
            return None;
        }
        r.headers().get("content-range")?.to_str().ok().and_then(content_range)?.1
    }

    /// GETs `url` from byte `from` on.
    fn open_now(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        let mut req = self.agent.get(url);
        if from > 0 {
            req = req.header("Range", format!("bytes={from}-"));
        }
        let r = req.call().map_err(|e| e.to_string())?;
        let status = r.status().as_u16();
        let header = |name: &str| r.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        // Range past the end (the length was an estimate): report the real length, asking for it when
        // a proxy dropped Content-Range.
        if status == 416 && from > 0 {
            let len = header("content-range").as_deref().and_then(unsatisfied_range).or_else(|| self.total_length(url)).filter(|&l| l <= from);
            return Err(OpenError::PastEnd { len });
        }
        if !(200..300).contains(&status) {
            return Err(OpenError::Status(status));
        }
        let (start, len) = match header("content-range").as_deref().and_then(content_range) {
            Some(r) if status == 206 => r,
            _ => (0, header("content-length").and_then(|l| l.parse().ok())),
        };
        let reader: Box<dyn Read + Send> = Box::new(r.into_body().into_reader());
        Ok(Body { start, len, reader })
    }
}

fn failure(e: ureq::Error) -> TransportError {
    use std::io::ErrorKind;
    let kind = match &e {
        ureq::Error::HostNotFound => FailureKind::UnknownHost,
        ureq::Error::ConnectionFailed => FailureKind::Connect,
        ureq::Error::Timeout(_) => FailureKind::Timeout,
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => FailureKind::Tls,
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => FailureKind::Other,
        ureq::Error::Io(io) => match io.kind() {
            ErrorKind::ConnectionRefused => FailureKind::Connect,
            ErrorKind::TimedOut => FailureKind::Timeout,
            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::UnexpectedEof => FailureKind::Interrupted,
            // std's lookup failure has no kind of its own: a name that cannot be resolved (no network).
            _ if io.to_string().starts_with("failed to lookup address") => FailureKind::UnknownHost,
            _ => FailureKind::Io,
        },
        _ => FailureKind::Io,
    };
    TransportError::Failed { kind, detail: Some(e.to_string()) }
}

/// Parses `bytes 100-199/1000` into (start, total length).
fn content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let (range, total) = v.strip_prefix("bytes ")?.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

/// Parses the total length out of a 416's `bytes */1000`.
fn unsatisfied_range(v: &str) -> Option<u64> {
    v.strip_prefix("bytes ")?.trim().strip_prefix("*/")?.trim().parse().ok()
}

/// Adds `headers` and a whole-request timeout (0 keeps the agent's) to `req`. That timeout is also the
/// wait for the answer: a held poll answers only when there is news.
fn configured<B>(mut req: RequestBuilder<B>, headers: &HashMap<String, String>, timeout_ms: u32) -> RequestBuilder<B> {
    for (name, value) in headers {
        req = req.header(name, value);
    }
    if timeout_ms > 0 {
        let timeout = Some(Duration::from_millis(timeout_ms as u64));
        req = req.config().timeout_global(timeout).timeout_recv_response(timeout).build();
    }
    req
}

fn response(r: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<TransportResponse, TransportError> {
    let r = r.map_err(failure)?;
    let status = r.status().as_u16();
    let body = r.into_body().with_config().limit(MAX_ANSWER).read_to_vec().map_err(failure)?;
    Ok(TransportResponse { status, body })
}

/// Each request runs on a thread of its own ([`Call`]), so dropping its future ends it.
#[async_trait::async_trait]
impl Transport for Http {
    async fn get(&self, url: String, timeout_ms: u32) -> Result<TransportResponse, TransportError> {
        let agent = self.agent.clone();
        Call::run(move || response(configured(agent.get(&url), &HashMap::new(), timeout_ms).call())).await
    }

    async fn send(&self, request: Exchange) -> Result<TransportResponse, TransportError> {
        let agent = self.agent.clone();
        Call::run(move || {
            let (headers, timeout) = (&request.headers, request.timeout_ms);
            response(match &request.json {
                Some(json) => configured(agent.post(&request.url).header("Content-Type", "application/json"), headers, timeout).send(json.as_bytes()),
                None => configured(agent.get(&request.url), headers, timeout).call(),
            })
        })
        .await
    }

    fn address_changed(&self) {}

    fn network(&self) -> nori_core::transport::Network {
        nori_core::transport::Network::Unmetered
    }
}

type Answer = Result<TransportResponse, TransportError>;

/// A core request on a thread of its own. Its future waits without blocking the caller; dropped before
/// the answer, it shuts the request's socket, so a held poll ends at once rather than when it is answered.
#[derive(Default)]
struct Call(Mutex<CallState>);

#[derive(Default)]
struct CallState {
    answer: Option<Answer>,
    waker: Option<Waker>,
    /// The socket the request uses now.
    socket: Option<TcpStream>,
    given_up: bool,
}

impl Call {
    fn run(request: impl FnOnce() -> Answer + Send + 'static) -> Waiting {
        let call = Arc::new(Call::default());
        let on = call.clone();
        let spawned = std::thread::Builder::new().name("nori-http".into()).spawn(move || {
            CALL.with(|c| *c.borrow_mut() = Some(on.clone()));
            let answer = request();
            let mut s = on.state();
            s.socket = None;
            s.answer = Some(answer);
            if let Some(w) = s.waker.take() {
                w.wake();
            }
        });
        if let Err(e) = spawned {
            call.state().answer = Some(Err(TransportError::Failed { kind: FailureKind::Other, detail: Some(e.to_string()) }));
        }
        Waiting(call)
    }

    fn state(&self) -> MutexGuard<'_, CallState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The request goes on over `stream`; Err once it was given up.
    fn uses(&self, stream: &TcpStream) -> io::Result<()> {
        let mut s = self.state();
        if s.given_up {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "given up"));
        }
        s.socket = Some(stream.try_clone()?);
        Ok(())
    }
}

/// A [`Call`]'s answer to come.
struct Waiting(Arc<Call>);

impl Future for Waiting {
    type Output = Answer;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Answer> {
        let mut s = self.0.state();
        match s.answer.take() {
            Some(a) => Poll::Ready(a),
            None => {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut s = self.0.state();
        if s.answer.is_none() {
            s.given_up = true;
            if let Some(socket) = s.socket.take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }
}

thread_local! {
    /// The core request this thread runs: a [`Call`]'s thread.
    static CALL: RefCell<Option<Arc<Call>>> = const { RefCell::new(None) };

    /// The engine request this thread is currently blocked on. Thread-local because ureq pools
    /// connections across requests and has no per-call hook into the transport.
    static CURRENT_CANCEL: RefCell<Option<Cancel>> = const { RefCell::new(None) };
}

/// Runs `f` with `cancel` as this thread's current request.
fn with_cancel<R>(cancel: &Cancel, f: impl FnOnce() -> R) -> R {
    let before = CURRENT_CANCEL.with(|c| c.replace(Some(cancel.clone())));
    let r = f();
    CURRENT_CANCEL.with(|c| *c.borrow_mut() = before);
    r
}

/// Poll interval for cancellation while blocked on a socket (ureq cannot be cancelled from another thread).
const CANCEL_POLL: Duration = Duration::from_millis(250);

/// Opens TCP connections as [`Socket`]s (ureq's own TCP connector keeps its socket to itself); a CONNECT
/// proxy's connection, made before, goes on as it is.
#[derive(Debug)]
struct Sockets;

impl<In: ureq::unversioned::transport::Transport> Connector<In> for Sockets {
    type Out = Either<In, Socket>;

    fn connect(&self, details: &ConnectionDetails, chained: Option<In>) -> Result<Option<Self::Out>, ureq::Error> {
        if let Some(c) = chained {
            return Ok(Some(Either::A(c)));
        }
        let timeout = details.timeout.not_zero().map(|t| *t);
        let mut failed = None;
        for addr in &details.addrs {
            let connected = match timeout {
                Some(t) => TcpStream::connect_timeout(addr, t),
                None => TcpStream::connect(addr),
            };
            match connected {
                Ok(stream) => {
                    stream.set_nodelay(details.config.no_delay())?;
                    let buffers = LazyBuffers::new(details.config.input_buffer_size(), details.config.output_buffer_size());
                    return Ok(Some(Either::B(Socket { stream, buffers, call: Weak::new(), read_timeout: None, write_timeout: None })));
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => failed = Some(ureq::Error::Timeout(ureq::Timeout::Connect)),
                Err(e) => failed = Some(e.into()),
            }
        }
        Err(failed.unwrap_or(ureq::Error::ConnectionFailed))
    }
}

/// A TCP connection that gives the [`Call`] using it a handle to shut it down.
#[derive(Debug)]
struct Socket {
    stream: TcpStream,
    buffers: LazyBuffers,
    /// The call last given the handle.
    call: Weak<Call>,
    /// The socket's timeouts as last set, so they are set only when they change.
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

/// Sets `timeout` through `set` unless `now` already is it.
fn set_timeout(now: &mut Option<Duration>, timeout: NextTimeout, set: impl FnOnce(Option<Duration>) -> io::Result<()>) -> io::Result<()> {
    let wanted = timeout.not_zero().map(|t| *t);
    if *now != wanted {
        set(wanted)?;
        *now = wanted;
    }
    Ok(())
}

impl Socket {
    /// Gives this thread's call (if any) the handle, once; Err when that call was given up meanwhile.
    fn in_call(&mut self) -> io::Result<()> {
        CALL.with(|c| {
            let Some(call) = c.borrow().clone() else { return Ok(()) };
            if self.call.as_ptr() != Arc::as_ptr(&call) {
                call.uses(&self.stream)?;
                self.call = Arc::downgrade(&call);
            }
            Ok(())
        })
    }
}

fn timed_out(e: io::Error, timeout: NextTimeout) -> ureq::Error {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ureq::Error::Timeout(timeout.reason),
        _ => e.into(),
    }
}

impl ureq::unversioned::transport::Transport for Socket {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.in_call()?;
        set_timeout(&mut self.write_timeout, timeout, |t| self.stream.set_write_timeout(t))?;
        self.stream.write_all(&self.buffers.output()[..amount]).map_err(|e| timed_out(e, timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.in_call()?;
        set_timeout(&mut self.read_timeout, timeout, |t| self.stream.set_read_timeout(t))?;
        let read = self.stream.read(self.buffers.input_append_buf()).map_err(|e| timed_out(e, timeout))?;
        self.buffers.input_appended(read);
        Ok(read > 0)
    }

    /// A connection at rest has nothing to read: anything there, or its end, means it is done.
    fn is_open(&mut self) -> bool {
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let at_rest = matches!(self.stream.read(&mut [0]), Err(e) if e.kind() == io::ErrorKind::WouldBlock);
        at_rest && self.stream.set_nonblocking(false).is_ok()
    }
}

/// Connector wrapping every connection in [`CancellableTransport`].
#[derive(Debug)]
struct Cancellable;

impl<In: ureq::unversioned::transport::Transport> Connector<In> for Cancellable {
    type Out = CancellableTransport;

    fn connect(&self, _: &ConnectionDetails, chained: Option<In>) -> Result<Option<CancellableTransport>, ureq::Error> {
        Ok(chained.map(|t| CancellableTransport(Box::new(t))))
    }
}

/// Splits input waits into [`CANCEL_POLL`] slices and fails with `Interrupted` once the thread's
/// current request is cancelled.
#[derive(Debug)]
struct CancellableTransport(Box<dyn ureq::unversioned::transport::Transport>);

impl ureq::unversioned::transport::Transport for CancellableTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.0.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.0.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let Some(cancel) = CURRENT_CANCEL.with(|c| c.borrow().clone()) else { return self.0.await_input(timeout) };
        let until = (!timeout.after.is_not_happening()).then(|| Instant::now() + *timeout.after);
        loop {
            if cancel.cancelled() {
                return Err(ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")));
            }
            let left = until.map(|u| u.saturating_duration_since(Instant::now()));
            let step = left.map_or(CANCEL_POLL, |l| l.min(CANCEL_POLL));
            let piece = NextTimeout { after: ureq::unversioned::transport::time::Duration::Exact(step), reason: timeout.reason };
            match self.0.await_input(piece) {
                Err(ureq::Error::Timeout(_)) if left.is_none_or(|l| l > step) => continue,
                other => return other,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.0.is_open()
    }

    fn is_tls(&self) -> bool {
        self.0.is_tls()
    }
}

/// A response body whose reads run under its request's cancel.
struct CancellableBody {
    inner: Box<dyn Read + Send>,
    cancel: Cancel,
}

impl Read for CancellableBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let (inner, cancel) = (&mut self.inner, &self.cancel);
        with_cancel(cancel, || inner.read(buf))
    }
}

impl ByteSource for Http {
    fn open(&self, url: &str, from: u64) -> Result<Body, OpenError> {
        self.open_now(url, from)
    }

    fn open_cancellable(&self, url: &str, _key: Option<&str>, from: u64, cancel: &Cancel) -> Result<Body, OpenError> {
        let mut b = with_cancel(cancel, || self.open_now(url, from))?;
        b.reader = Box::new(CancellableBody { inner: b.reader, cancel: cancel.clone() });
        Ok(b)
    }

    /// Radio stream with ICY metadata requested; also returns `icy-metaint` (audio bytes between metadata blocks).
    fn open_live(&self, url: &str) -> Result<(Body, Option<usize>), String> {
        let r = self.agent.get(url).header("Icy-MetaData", "1").call().map_err(|e| e.to_string())?;
        let status = r.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}"));
        }
        let every = r.headers().get("icy-metaint").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        let reader: Box<dyn Read + Send> = Box::new(r.into_body().into_reader());
        Ok((Body { start: 0, len: None, reader }, every))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(content_range("bytes 100-199/1000"), Some((100, Some(1000))));
        assert_eq!(content_range("bytes 5-9/*"), Some((5, None)));
        assert_eq!(content_range("items 1-2/3"), None);
    }

    #[test]
    fn failures_are_told_apart() {
        use std::io::{Error, ErrorKind};
        let lookup = "failed to lookup address information: nodename nor servname provided, or not known";
        let cases = [
            (Error::other(lookup), FailureKind::UnknownHost),
            (Error::new(ErrorKind::ConnectionRefused, "refused"), FailureKind::Connect),
            (Error::new(ErrorKind::PermissionDenied, "denied"), FailureKind::Io),
        ];
        for (io, want) in cases {
            let TransportError::Failed { kind, .. } = failure(ureq::Error::Io(io)) else { panic!() };
            assert_eq!(kind, want);
        }
    }

    #[test]
    fn parses_unsatisfied_range() {
        assert_eq!(unsatisfied_range("bytes */6406842"), Some(6_406_842));
        assert_eq!(unsatisfied_range("bytes */*"), None);
        assert_eq!(unsatisfied_range("bytes 0-9/10"), None);
    }
}
