//! A separate loopback-only control listener. A private TLS proxy owns remote
//! access; neither the HLS listener nor CORS exposes these credentials.
//!
//! HTTP is served by `http_lite` on tokio. One request per connection, always
//! answered with `connection: close`; the private proxy opens a connection for
//! each call. A request is checked in this order, and a refusal at any step is
//! sent without going further:
//!
//! 1. The head: size, header count, target length, well-formed CRLF lines.
//! 2. The bearer token, compared in constant time, before the route is looked
//!    at and before one byte of body is read.
//! 3. The route and method.
//! 4. The body framing: one `content-length` no larger than `BODY_LIMIT`,
//!    checked against the declared length before anything is buffered.
//! 5. The body, then the handler, together within the handler timeout.
//!
//! Every response, error or not, carries `cache-control: no-store`.

use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::App;
use crate::http_lite::{self, Framing, ParsedRequest, Refusal};
use crate::publish_session_wire::{BODY_LIMIT, PutSession, RevokeSession};
use crate::publish_sessions::Error;

/// Connections served at once; more are closed as they arrive.
const CONNECTIONS: usize = 16;
/// After the answer, request bytes still arriving are read and dropped for up
/// to this much, so a refusal sent before the whole request came in is read by
/// the client rather than lost to a TCP reset. The proxy is on loopback.
const DRAIN_BYTES: usize = 64 * 1024;
const DRAIN_TIME: Duration = Duration::from_millis(250);

/// The time limits, separate so tests can shorten them.
#[derive(Debug, Clone, Copy)]
struct Timeouts {
    /// The whole request head, counted from accept. It is not restarted by
    /// each byte, so a client dripping bytes cannot hold a connection.
    head: Duration,
    /// Reading the body and running the handler, together.
    handler: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            head: Duration::from_secs(5),
            handler: Duration::from_secs(10),
        }
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    /// The `allow` header of a 405.
    allow: Option<&'static str>,
}

impl Reply {
    /// The whole response. Answering `HEAD` keeps the length and drops the body.
    fn bytes(&self, head_only: bool) -> Vec<u8> {
        let mut headers = vec![
            ("cache-control", "no-store"),
            ("content-type", "application/json"),
        ];
        if let Some(allow) = self.allow {
            headers.push(("allow", allow));
        }
        let mut bytes = http_lite::response(self.status, &headers, &self.body);
        if head_only {
            bytes.truncate(bytes.len() - self.body.len());
        }
        bytes
    }
}

fn failure(status: u16, code: &'static str) -> Reply {
    Reply {
        status,
        body: serde_json::to_vec(&serde_json::json!({"ok":false,"error":code})).unwrap_or_default(),
        allow: None,
    }
}
fn error(value: Error) -> Reply {
    let (status, code) = match value {
        Error::Invalid => (400, "invalid-request"),
        Error::Conflict => (409, "session-conflict"),
        Error::Denied => (403, "session-denied"),
        Error::Unknown => (404, "unknown-session"),
        Error::Busy => (429, "capacity"),
        Error::Unavailable => (503, "authority-unavailable"),
    };
    failure(status, code)
}
fn receipt<T: Serialize>(result: Result<T, Error>) -> Reply {
    match result {
        Ok(receipt) => match serde_json::to_vec(&receipt) {
            Ok(body) => Reply {
                status: 200,
                body,
                allow: None,
            },
            Err(_) => failure(500, "internal-error"),
        },
        Err(e) => error(e),
    }
}
fn refusal(why: Refusal) -> Reply {
    match why {
        Refusal::Malformed => failure(400, "invalid-request"),
        Refusal::HeadTooLarge => failure(431, "request-header-too-large"),
        Refusal::TargetTooLong => failure(414, "uri-too-long"),
    }
}

enum Route<'a> {
    Put(&'a str),
    Status(&'a str),
    Revoke(&'a str),
}

/// `PUT`, `GET` and `HEAD /v1/publish-sessions/{id}`, `POST .../{id}/revoke`. The id
/// is one non-empty segment, taken as sent: it is validated against the
/// session grammar by the handlers, so an encoded id is refused, not decoded.
fn route<'a>(method: &str, path: &'a str) -> Result<Route<'a>, Reply> {
    let not_found = || failure(404, "not-found");
    let rest = path
        .strip_prefix("/v1/publish-sessions/")
        .ok_or_else(not_found)?;
    let (id, revoke) = match rest.split_once('/') {
        None => (rest, false),
        Some((id, "revoke")) => (id, true),
        Some(_) => return Err(not_found()),
    };
    if id.is_empty() {
        return Err(not_found());
    }
    let wrong_method = |allow| Reply {
        allow: Some(allow),
        ..failure(405, "method-not-allowed")
    };
    match (revoke, method) {
        (false, "PUT") => Ok(Route::Put(id)),
        (false, "GET" | "HEAD") => Ok(Route::Status(id)),
        (false, _) => Err(wrong_method("GET, HEAD, PUT")),
        (true, "POST") => Ok(Route::Revoke(id)),
        (true, _) => Err(wrong_method("POST")),
    }
}

async fn handle(app: &App, route: Route<'_>, body: &[u8]) -> Reply {
    // A status call checks its id before it looks for the authority.
    if let Route::Status(id) = route
        && sapflow_core::publish_session::SessionId::parse(id).is_err()
    {
        return error(Error::Invalid);
    }
    let Some(sessions) = &app.publish_sessions else {
        return error(Error::Unavailable);
    };
    match route {
        Route::Put(id) => match PutSession::parse(body) {
            Ok(wire) => receipt(sessions.put(id, wire, app.config.max_publish_bps).await),
            Err(_) => error(Error::Invalid),
        },
        Route::Revoke(id) => match RevokeSession::parse(body) {
            Ok(wire) => receipt(sessions.revoke(id, wire).await),
            Err(_) => error(Error::Invalid),
        },
        Route::Status(id) => receipt(sessions.status(id).await),
    }
}

enum HeadError {
    Refused(Refusal),
    /// The client went away; there is nobody to answer.
    Closed,
}

/// Reads until a whole head is in `buf`. Never buffers past `MAX_HEAD`, so
/// the bytes after the head are at most the first part of the body.
async fn read_head(
    socket: &mut TcpStream,
    buf: &mut Vec<u8>,
) -> Result<(http_lite::RequestHead, usize), HeadError> {
    loop {
        match http_lite::parse_request(buf) {
            ParsedRequest::Complete(head, used) => return Ok((head, used)),
            ParsedRequest::Refused(why) => return Err(HeadError::Refused(why)),
            ParsedRequest::Partial => {}
        }
        let mut chunk = [0u8; 4096];
        let room = http_lite::MAX_HEAD
            .saturating_sub(buf.len())
            .min(chunk.len());
        match socket.read(&mut chunk[..room]).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return Err(HeadError::Closed),
        }
    }
}

/// The body: what came after the head, then the rest from the socket.
/// `length` is already within `BODY_LIMIT`.
async fn read_body(
    socket: &mut TcpStream,
    already: &[u8],
    length: usize,
) -> std::io::Result<Vec<u8>> {
    let have = already.len().min(length);
    let mut body = vec![0u8; length];
    body[..have].copy_from_slice(&already[..have]);
    socket.read_exact(&mut body[have..]).await?;
    Ok(body)
}

/// Reads the request and works out the answer. `None` means the client went
/// away and there is nobody to answer. `head_only` is set once the method is
/// known, so the answer to a `HEAD` carries no body.
async fn exchange(
    socket: &mut TcpStream,
    app: &App,
    timeouts: Timeouts,
    head_only: &mut bool,
) -> Option<Reply> {
    let mut buf = Vec::with_capacity(2048);
    let (head, used) = match tokio::time::timeout(timeouts.head, read_head(socket, &mut buf)).await
    {
        Err(_) => return Some(failure(408, "read-timeout")),
        Ok(Err(HeadError::Closed)) => return None,
        Ok(Err(HeadError::Refused(why))) => return Some(refusal(why)),
        Ok(Ok(parsed)) => parsed,
    };
    *head_only = head.method == "HEAD";
    let Some(config) = &app.config.control else {
        return Some(error(Error::Denied));
    };
    let allowed = head
        .authorization
        .as_ref()
        .and_then(|credential| credential.bearer())
        .is_some_and(|v| config.token.matches(v));
    if !allowed {
        return Some(failure(401, "unauthorised"));
    }
    let route = match route(&head.method, &head.path) {
        Ok(route) => route,
        Err(reply) => return Some(reply),
    };
    let length = match head.framing {
        Framing::Ambiguous => return Some(failure(400, "invalid-request")),
        Framing::Length(n) => match usize::try_from(n) {
            Ok(n) if n <= BODY_LIMIT => n,
            _ => return Some(failure(413, "body-too-large")),
        },
    };
    let already = &buf[used..];
    let work = async {
        if head.expects_continue && length > already.len() {
            socket
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .ok()?;
        }
        let body = read_body(socket, already, length).await.ok()?;
        Some(handle(app, route, &body).await)
    };
    match tokio::time::timeout(timeouts.handler, work).await {
        Ok(reply) => reply,
        Err(_) => Some(failure(504, "request-timeout")),
    }
}

async fn drain(socket: &mut TcpStream) {
    let mut scratch = [0u8; 4096];
    let mut total = 0;
    let _ = tokio::time::timeout(DRAIN_TIME, async {
        while total < DRAIN_BYTES {
            match socket.read(&mut scratch).await {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
    })
    .await;
}

async fn connection(mut socket: TcpStream, app: Arc<App>, timeouts: Timeouts) {
    let mut head_only = false;
    let Some(reply) = exchange(&mut socket, &app, timeouts, &mut head_only).await else {
        return;
    };
    let sent = async {
        socket.write_all(&reply.bytes(head_only)).await?;
        socket.shutdown().await
    };
    if matches!(tokio::time::timeout(timeouts.head, sent).await, Ok(Ok(()))) {
        drain(&mut socket).await;
    }
}

pub async fn serve(listener: TcpListener, app: Arc<App>) -> std::io::Result<()> {
    serve_with(listener, app, Timeouts::default()).await
}

async fn serve_with(
    listener: TcpListener,
    app: Arc<App>,
    timeouts: Timeouts,
) -> std::io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(std::io::Error::other("control listener must be loopback"));
    }
    let slots = Arc::new(tokio::sync::Semaphore::new(CONNECTIONS));
    loop {
        let (socket, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let app = app.clone();
        tokio::spawn(async move {
            let _slot = slot;
            connection(socket, app, timeouts).await;
        });
    }
}

#[cfg(test)]
#[path = "publish_control_tests.rs"]
mod tests;
