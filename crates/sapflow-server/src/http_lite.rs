//! Minimal HTTP/1.1 for the two paths that are not viewer delivery: the
//! loopback control listener (`publish_control.rs`) and the callback client
//! (`callback.rs`). Bytes in, values out, no I/O, so it is unit-tested here and
//! fuzzed by `fuzz/fuzz_targets/http_lite.rs`, which includes this file.
//!
//! Deliberately small. A control request is one head plus a body framed by a single
//! `content-length`. Chunked requests, repeated or non-numeric lengths, bare line
//! feeds, obsolete line folding and absolute-form targets are refused rather
//! than interpreted, so no second parser in front of or behind this one can
//! disagree about where a message ends. Every exchange is one request and one
//! response on a connection that then closes (`connection: close`). Callback
//! responses retain the previous client's framing checks but read only the head.

use std::fmt;

/// The longest request or response head accepted: start line and headers.
pub const MAX_HEAD: usize = 16 * 1024;
/// The longest request target accepted.
pub const MAX_TARGET: usize = 2 * 1024;
/// The most headers accepted in a request.
const MAX_REQUEST_HEADERS: usize = 32;
/// The most headers accepted in a response.
const MAX_RESPONSE_HEADERS: usize = 64;

/// The `authorization` header value as sent. Never printed, so a log line or
/// an assertion message cannot carry a credential.
#[derive(PartialEq, Eq)]
pub struct Credential(String);

impl Credential {
    /// The part after `Bearer `, if the header is a bearer credential. The
    /// scheme is case-sensitive, as the control API has always required.
    pub fn bearer(&self) -> Option<&str> {
        self.0.strip_prefix("Bearer ")
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential([REDACTED])")
    }
}

/// How the body of a request is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// `content-length`, or 0 when the request has none. Saturates at
    /// `u64::MAX` for a number too long to parse, which is over any limit.
    Length(u64),
    /// A `transfer-encoding`, or a `content-length` that is repeated or is not
    /// plain digits. The caller must refuse the request.
    Ambiguous,
}

/// One parsed request head.
#[derive(Debug, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    /// The path of an origin-form target, without its query.
    pub path: String,
    /// The single `authorization` header. `None` if absent, repeated or not
    /// UTF-8: a repeated header is ambiguous, so it grants nothing.
    pub authorization: Option<Credential>,
    pub framing: Framing,
    /// `expect: 100-continue`: the client waits for an interim answer before
    /// it sends the body.
    pub expects_continue: bool,
}

/// Why a head was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not a request this module understands.
    Malformed,
    /// More head than `MAX_HEAD`, or more than the header limit.
    HeadTooLarge,
    /// A request target longer than `MAX_TARGET`.
    TargetTooLong,
}

/// The result of looking for a request at the start of a buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum ParsedRequest {
    /// A request head, and how many bytes of the buffer it used.
    Complete(RequestHead, usize),
    /// Not a whole head yet.
    Partial,
    Refused(Refusal),
}

/// Parses the request head at the start of `buf`.
pub fn parse_request(buf: &[u8]) -> ParsedRequest {
    let mut headers = [httparse::EMPTY_HEADER; MAX_REQUEST_HEADERS];
    let mut req = httparse::Request::new(&mut headers);
    let used = match req.parse(buf) {
        Ok(httparse::Status::Complete(n)) if n <= MAX_HEAD => n,
        Ok(httparse::Status::Complete(_)) => return ParsedRequest::Refused(Refusal::HeadTooLarge),
        Ok(httparse::Status::Partial) if buf.len() >= MAX_HEAD => {
            return ParsedRequest::Refused(Refusal::HeadTooLarge);
        }
        Ok(httparse::Status::Partial) => return ParsedRequest::Partial,
        Err(httparse::Error::TooManyHeaders) => {
            return ParsedRequest::Refused(Refusal::HeadTooLarge);
        }
        Err(_) => return ParsedRequest::Refused(Refusal::Malformed),
    };
    // httparse accepts a bare line feed as a line end. Another parser in the
    // path may not, so every line must end in CRLF.
    let head = &buf[..used];
    if head
        .iter()
        .enumerate()
        .any(|(i, &b)| b == b'\n' && (i == 0 || head[i - 1] != b'\r'))
    {
        return ParsedRequest::Refused(Refusal::Malformed);
    }
    let (Some(method), Some(target)) = (req.method, req.path) else {
        return ParsedRequest::Refused(Refusal::Malformed);
    };
    if target.len() > MAX_TARGET {
        return ParsedRequest::Refused(Refusal::TargetTooLong);
    }
    if !target.starts_with('/') || target.contains('#') {
        return ParsedRequest::Refused(Refusal::Malformed);
    }
    let path = target.split('?').next().unwrap_or_default().to_owned();

    let mut authorization = None;
    let mut authorizations = 0usize;
    let mut length = None;
    let mut ambiguous = false;
    let mut expects_continue = false;
    for h in req.headers.iter() {
        if h.name.eq_ignore_ascii_case("authorization") {
            authorizations += 1;
            authorization = std::str::from_utf8(h.value)
                .ok()
                .map(|v| Credential(v.to_owned()));
        } else if h.name.eq_ignore_ascii_case("content-length") {
            match (length, content_length(h.value)) {
                (None, Some(n)) => length = Some(n),
                _ => ambiguous = true,
            }
        } else if h.name.eq_ignore_ascii_case("transfer-encoding") {
            ambiguous = true;
        } else if h.name.eq_ignore_ascii_case("expect")
            && h.value.trim_ascii().eq_ignore_ascii_case(b"100-continue")
        {
            expects_continue = true;
        }
    }
    ParsedRequest::Complete(
        RequestHead {
            method: method.to_owned(),
            path,
            authorization: if authorizations == 1 {
                authorization
            } else {
                None
            },
            framing: if ambiguous {
                Framing::Ambiguous
            } else {
                Framing::Length(length.unwrap_or(0))
            },
            expects_continue,
        },
        used,
    )
}

/// A `content-length` value: ASCII digits only, so no sign, list or space.
fn content_length(value: &[u8]) -> Option<u64> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let digits = std::str::from_utf8(value).ok()?;
    Some(digits.parse().unwrap_or(u64::MAX))
}

/// The reason phrase for the statuses these paths send.
fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}

/// A whole response: `headers` are extra lines, then the length and
/// `connection: close`, which every response here carries.
pub fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status)).into_bytes();
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "content-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);
    out
}

/// The result of looking for a response head at the start of a buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum ParsedResponse {
    /// The status, and how many bytes of the buffer the head used.
    Complete {
        status: u16,
        used: usize,
    },
    Partial,
    /// Not a response, invalid body framing, or a head over `MAX_HEAD`.
    Malformed,
}

/// Parses the response head at the start of `buf`. The body is never read:
/// callers want the status only.
pub fn parse_response(buf: &[u8]) -> ParsedResponse {
    let mut headers = [httparse::EMPTY_HEADER; MAX_RESPONSE_HEADERS];
    let mut res = httparse::Response::new(&mut headers);
    let used = match res.parse(buf) {
        Ok(httparse::Status::Complete(used)) if used <= MAX_HEAD => used,
        Ok(httparse::Status::Partial) if buf.len() < MAX_HEAD => return ParsedResponse::Partial,
        _ => return ParsedResponse::Malformed,
    };
    match res.code {
        Some(status)
            if (100..600).contains(&status)
                && valid_response_framing(res.version, status, res.headers) =>
        {
            ParsedResponse::Complete { status, used }
        }
        _ => ParsedResponse::Malformed,
    }
}

/// Preserve the previous client's response-head checks before acknowledging a
/// callback. No-body statuses ignore framing; otherwise Transfer-Encoding takes
/// precedence on HTTP/1.1 only. Without it, all Content-Length values (including
/// comma lists) must be valid and identical. Closing each connection does not
/// make a malformed successful response an acknowledgement.
fn valid_response_framing(
    version: Option<u8>,
    status: u16,
    headers: &[httparse::Header<'_>],
) -> bool {
    if status < 200 || matches!(status, 204 | 304) {
        return true;
    }
    if headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case("transfer-encoding"))
    {
        return version == Some(1);
    }
    let mut agreed = None;
    for header in headers {
        if !header.name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        for value in header.value.split(|byte| *byte == b',') {
            let value = value.trim_ascii();
            if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
                return false;
            }
            let length = std::str::from_utf8(value)
                .ok()
                .and_then(|value| value.parse::<u64>().ok());
            // The previous decoder reserved the two largest u64 values for
            // unknown lengths. Keep its accepted numeric range unchanged.
            let Some(length) = length.filter(|length| *length <= u64::MAX - 2) else {
                return false;
            };
            if agreed.is_some_and(|previous| previous != length) {
                return false;
            }
            agreed = Some(length);
        }
    }
    true
}

/// Where a callback goes, from an `http://` URL.
#[derive(PartialEq, Eq)]
pub struct Endpoint {
    /// The host to connect to, without brackets.
    pub host: String,
    pub port: u16,
    /// The `host` header: the host as written, with the port only if the URL
    /// gave one.
    host_header: String,
    /// Path and query, as sent on the request line. Callback URLs carry a
    /// secret in the query, so this is never printed.
    target: String,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("target", &"[REDACTED]")
            .finish()
    }
}

impl Endpoint {
    /// Parses an `http://` URL. Refuses anything that could not be sent as one
    /// request line: another scheme, a fragment, and any space, control or
    /// non-ASCII byte after the authority. User information is dropped, as the
    /// previous client did.
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url
            .get(..7)
            .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
            .and_then(|_| url.get(7..))?;
        let (authority, tail) = match rest.find(['/', '?']) {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };
        let target = match tail {
            "" => "/".to_owned(),
            t if t.starts_with('?') => format!("/{t}"),
            t => t.to_owned(),
        };
        if !target.bytes().all(|b| (0x21..=0x7e).contains(&b)) || target.contains('#') {
            return None;
        }
        let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port, bracketed) = match hostport.strip_prefix('[') {
            Some(inner) => {
                let (host, after) = inner.split_once(']')?;
                let port = match after {
                    "" => None,
                    s => Some(s.strip_prefix(':')?),
                };
                if host.is_empty()
                    || !host
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
                {
                    return None;
                }
                (host, port, true)
            }
            None => {
                let (host, port) = match hostport.rsplit_once(':') {
                    Some((h, p)) => (h, Some(p)),
                    None => (hostport, None),
                };
                if host.is_empty()
                    || !host
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
                {
                    return None;
                }
                (host, port, false)
            }
        };
        let number = match port {
            None => 80,
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                p.parse::<u16>().ok().filter(|n| *n != 0)?
            }
            Some(_) => return None,
        };
        let written = if bracketed {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        Some(Self {
            host: host.to_owned(),
            port: number,
            host_header: match port {
                Some(p) => format!("{written}:{p}"),
                None => written,
            },
            target,
        })
    }

    /// The path and query, for tests that check what was sent.
    #[cfg(test)]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// A whole `POST` request with a `content-length` body, ready to write.
    pub fn post(&self, content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "POST {} HTTP/1.1\r\nhost: {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            self.target,
            self.host_header,
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(raw: &str) -> (RequestHead, usize) {
        match parse_request(raw.as_bytes()) {
            ParsedRequest::Complete(head, used) => (head, used),
            other => panic!("expected a request, got {other:?}"),
        }
    }
    fn refused(raw: &str) -> Refusal {
        match parse_request(raw.as_bytes()) {
            ParsedRequest::Refused(why) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_request_head_gives_method_path_credential_and_length() {
        let raw = "PUT /v1/x?a=b HTTP/1.1\r\nHost: h\r\nAuthorization: Bearer t0k\r\nContent-Length: 7\r\n\r\nabcdefg";
        let (head, used) = head(raw);
        assert_eq!((head.method.as_str(), head.path.as_str()), ("PUT", "/v1/x"));
        assert_eq!(
            head.authorization.expect("credential").bearer(),
            Some("t0k")
        );
        assert_eq!(head.framing, Framing::Length(7));
        assert_eq!(&raw[used..], "abcdefg");
    }

    #[test]
    fn no_length_means_no_body_and_the_credential_is_never_printed() {
        let (head, _) = head("GET / HTTP/1.1\r\nauthorization: Bearer hunter2\r\n\r\n");
        assert_eq!(head.framing, Framing::Length(0));
        assert!(!format!("{head:?}").contains("hunter2"));
    }

    #[test]
    fn bearer_scheme_is_exact() {
        let bearer = |value: &str| {
            head(&format!("GET / HTTP/1.1\r\nauthorization: {value}\r\n\r\n"))
                .0
                .authorization
                .and_then(|c| c.bearer().map(str::to_owned))
        };
        assert_eq!(bearer("Bearer a").as_deref(), Some("a"));
        assert_eq!(bearer("bearer a"), None);
        assert_eq!(bearer("Basic YTpi"), None);
        assert_eq!(bearer("Bearer"), None);
    }

    #[test]
    fn a_repeated_authorization_header_grants_nothing() {
        let (head, _) = head(
            "GET / HTTP/1.1\r\nauthorization: Bearer good\r\nauthorization: Bearer good\r\n\r\n",
        );
        assert!(head.authorization.is_none());
    }

    #[test]
    fn ambiguous_framing_is_marked() {
        for raw in [
            "PUT / HTTP/1.1\r\ncontent-length: 1\r\ncontent-length: 1\r\n\r\n",
            "PUT / HTTP/1.1\r\ncontent-length: 1\r\ncontent-length: 2\r\n\r\n",
            "PUT / HTTP/1.1\r\ncontent-length: 1, 1\r\n\r\n",
            "PUT / HTTP/1.1\r\ncontent-length: +1\r\n\r\n",
            "PUT / HTTP/1.1\r\ncontent-length: -1\r\n\r\n",
            "PUT / HTTP/1.1\r\ncontent-length: 0x10\r\n\r\n",
            "PUT / HTTP/1.1\r\ncontent-length:\r\n\r\n",
            "PUT / HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n",
            "PUT / HTTP/1.1\r\nTransfer-Encoding: chunked\r\ncontent-length: 3\r\n\r\n",
            "PUT / HTTP/1.1\r\ncontent-length: 3\r\ntransfer-encoding: identity\r\n\r\n",
        ] {
            assert_eq!(head(raw).0.framing, Framing::Ambiguous, "{raw:?}");
        }
    }

    #[test]
    fn a_length_too_long_to_parse_saturates_over_every_limit() {
        let (head, _) = head("PUT / HTTP/1.1\r\ncontent-length: 99999999999999999999999\r\n\r\n");
        assert_eq!(head.framing, Framing::Length(u64::MAX));
    }

    #[test]
    fn expect_continue_is_noticed() {
        let (head, _) = head("PUT / HTTP/1.1\r\nExpect: 100-Continue\r\ncontent-length: 2\r\n\r\n");
        assert!(head.expects_continue);
        assert!(!self::head("PUT / HTTP/1.1\r\n\r\n").0.expects_continue);
    }

    #[test]
    fn a_partial_head_waits_and_an_endless_one_is_refused() {
        assert_eq!(
            parse_request(b"GET / HTTP/1.1\r\nhost: x\r\n"),
            ParsedRequest::Partial
        );
        assert_eq!(parse_request(b""), ParsedRequest::Partial);
        let mut endless = b"GET / HTTP/1.1\r\nx: ".to_vec();
        endless.resize(MAX_HEAD, b'a');
        assert_eq!(
            parse_request(&endless),
            ParsedRequest::Refused(Refusal::HeadTooLarge)
        );
        // A complete head just over the limit is refused as well.
        let mut big = b"GET / HTTP/1.1\r\nx: ".to_vec();
        big.resize(MAX_HEAD, b'a');
        big.extend_from_slice(b"\r\n\r\n");
        assert_eq!(
            parse_request(&big),
            ParsedRequest::Refused(Refusal::HeadTooLarge)
        );
        // One just inside it is accepted.
        let mut fits = b"GET / HTTP/1.1\r\nx: ".to_vec();
        fits.resize(MAX_HEAD - 4, b'a');
        fits.extend_from_slice(b"\r\n\r\n");
        assert!(matches!(parse_request(&fits), ParsedRequest::Complete(_, n) if n == MAX_HEAD));
    }

    #[test]
    fn too_many_headers_and_long_targets_are_refused() {
        let mut flood = String::from("GET / HTTP/1.1\r\n");
        for i in 0..=MAX_REQUEST_HEADERS {
            flood.push_str(&format!("h{i}: v\r\n"));
        }
        // Refused as soon as the extra header starts, before the head ends.
        assert_eq!(refused(&flood), Refusal::HeadTooLarge);
        flood.push_str("\r\n");
        assert_eq!(refused(&flood), Refusal::HeadTooLarge);
        let mut full = String::from("GET / HTTP/1.1\r\n");
        for i in 0..MAX_REQUEST_HEADERS {
            full.push_str(&format!("h{i}: v\r\n"));
        }
        full.push_str("\r\n");
        assert!(matches!(
            parse_request(full.as_bytes()),
            ParsedRequest::Complete(..)
        ));
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_TARGET));
        assert_eq!(refused(&long), Refusal::TargetTooLong);
        let fits = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_TARGET - 1));
        assert!(matches!(
            parse_request(fits.as_bytes()),
            ParsedRequest::Complete(..)
        ));
    }

    #[test]
    fn only_origin_form_with_crlf_lines_is_accepted() {
        for raw in [
            "GET http://h/x HTTP/1.1\r\n\r\n",
            "GET * HTTP/1.1\r\n\r\n",
            "GET x/y HTTP/1.1\r\n\r\n",
            "GET /x#frag HTTP/1.1\r\n\r\n",
            "GET /x HTTP/1.1\nhost: h\r\n\r\n",
            "GET /x HTTP/1.1\r\nhost: h\n\r\n",
            "GET /x HTTP/1.1\r\nhost: h\r\n\n",
            "GET /x HTTP/2.0\r\n\r\n",
            "GET /x HTTP/1.1\r\n folded: value\r\n\r\n",
            "GET /x HTTP/1.1\r\nbad name: v\r\n\r\n",
            "\x16\x03\x01\x02\x00\r\n\r\n",
        ] {
            assert_eq!(refused(raw), Refusal::Malformed, "{raw:?}");
        }
    }

    #[test]
    fn garbage_is_refused_without_waiting_for_a_head_end() {
        assert_eq!(
            parse_request(b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03"),
            ParsedRequest::Refused(Refusal::Malformed)
        );
    }

    #[test]
    fn pipelined_bytes_are_left_for_the_caller() {
        let raw = "GET /a HTTP/1.1\r\n\r\nGET /b HTTP/1.1\r\n\r\n";
        let (first, used) = head(raw);
        assert_eq!(first.path, "/a");
        assert_eq!(&raw[used..], "GET /b HTTP/1.1\r\n\r\n");
    }

    #[test]
    fn arbitrary_bytes_never_panic_and_progress_stays_in_bounds() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let alphabet = b"GETPUT /?#:%\r\n \t0123456789Bearer-ContentLength:";
        for _ in 0..2_000 {
            let len = (next() % 200) as usize;
            let buf: Vec<u8> = (0..len)
                .map(|_| {
                    let r = next();
                    if r % 5 == 0 {
                        (r >> 8) as u8
                    } else {
                        alphabet[(r >> 8) as usize % alphabet.len()]
                    }
                })
                .collect();
            if let ParsedRequest::Complete(_, used) = parse_request(&buf) {
                assert!(used > 0 && used <= buf.len() && used <= MAX_HEAD);
            }
            let _ = parse_response(&buf);
            let _ = Endpoint::parse(&String::from_utf8_lossy(&buf));
        }
    }

    #[test]
    fn responses_carry_length_close_and_the_extra_headers() {
        let bytes = response(401, &[("cache-control", "no-store")], b"{}");
        assert_eq!(
            bytes,
            b"HTTP/1.1 401 Unauthorized\r\ncache-control: no-store\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
        );
        assert!(response(204, &[], b"").starts_with(b"HTTP/1.1 204 Status\r\n"));
    }

    #[test]
    fn response_heads_give_the_status_and_are_bounded() {
        assert_eq!(
            parse_response(b"HTTP/1.1 204 No Content\r\nserver: x\r\n\r\n"),
            ParsedResponse::Complete {
                status: 204,
                used: 38
            }
        );
        assert_eq!(parse_response(b"HTTP/1.1 20"), ParsedResponse::Partial);
        assert_eq!(
            parse_response(b"HTTP/1.1 700 No\r\n\r\n"),
            ParsedResponse::Malformed
        );
        assert_eq!(parse_response(b"SSH-2.0-x\r\n"), ParsedResponse::Malformed);
        let mut endless = b"HTTP/1.1 200 OK\r\nx: ".to_vec();
        endless.resize(MAX_HEAD, b'a');
        assert_eq!(parse_response(&endless), ParsedResponse::Malformed);
        let mut many = String::from("HTTP/1.1 200 OK\r\n");
        for i in 0..=MAX_RESPONSE_HEADERS {
            many.push_str(&format!("h{i}: v\r\n"));
        }
        many.push_str("\r\n");
        assert_eq!(parse_response(many.as_bytes()), ParsedResponse::Malformed);
    }

    #[test]
    fn malformed_response_framing_cannot_acknowledge_a_callback() {
        for headers in [
            "Content-Length: 0\r\nContent-Length: 1\r\n",
            "Content-Length: 0, 1\r\n",
            "Content-Length: nope\r\n",
            "Content-Length: +1\r\n",
            "Content-Length: \r\n",
            "Content-Length: 18446744073709551616\r\n",
            "Content-Length: 18446744073709551615\r\n",
            "Content-Length: 18446744073709551614\r\n",
        ] {
            let raw = format!("HTTP/1.1 200 OK\r\n{headers}\r\n");
            assert_eq!(
                parse_response(raw.as_bytes()),
                ParsedResponse::Malformed,
                "{headers}"
            );
        }
        assert_eq!(
            parse_response(b"HTTP/1.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"),
            ParsedResponse::Malformed
        );
    }

    #[test]
    fn valid_response_framing_retains_status_only_acknowledgements() {
        for raw in [
            "HTTP/1.1 200 OK\r\n\r\n",
            "HTTP/1.0 200 OK\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 3, 03\r\nContent-Length: 3\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551613\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: nope\r\n\r\n",
            "HTTP/1.1 204 No Content\r\nContent-Length: nope\r\n\r\n",
            "HTTP/1.1 100 Continue\r\nContent-Length: nope\r\n\r\n",
        ] {
            assert!(
                matches!(parse_response(raw.as_bytes()), ParsedResponse::Complete { used, .. } if used == raw.len()),
                "{raw}"
            );
        }
    }

    fn endpoint(url: &str) -> Endpoint {
        Endpoint::parse(url).unwrap_or_else(|| panic!("{url} should parse"))
    }

    #[test]
    fn callback_urls_become_a_connection_and_a_request_line() {
        let e = endpoint("http://127.0.0.1:8080/hook/live%2Fa?x=1&token=s%20t");
        assert_eq!((e.host.as_str(), e.port), ("127.0.0.1", 8080));
        assert_eq!(e.target(), "/hook/live%2Fa?x=1&token=s%20t");
        let sent = e.post("application/json", b"{}");
        assert_eq!(
            String::from_utf8_lossy(&sent),
            "POST /hook/live%2Fa?x=1&token=s%20t HTTP/1.1\r\nhost: 127.0.0.1:8080\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
        );
        let e = endpoint("HTTP://v4v.internal");
        assert_eq!(
            (e.host.as_str(), e.port, e.target()),
            ("v4v.internal", 80, "/")
        );
        assert_eq!(endpoint("http://h?q=1").target(), "/?q=1");
        let e = endpoint("http://[::1]:9/x");
        assert_eq!((e.host.as_str(), e.port), ("::1", 9));
        assert!(String::from_utf8_lossy(&e.post("t", b"")).contains("host: [::1]:9\r\n"));
        assert_eq!(endpoint("http://[::1]/x").port, 80);
        // User information is not sent.
        let e = endpoint("http://user:pw@h:81/x");
        assert!(!String::from_utf8_lossy(&e.post("t", b"")).contains("pw"));
        assert_eq!((e.host.as_str(), e.port), ("h", 81));
    }

    #[test]
    fn callback_urls_that_cannot_be_one_request_line_are_refused() {
        for url in [
            "https://h/x",
            "ftp://h/x",
            "h/x",
            "http://",
            "http:///x",
            "http://h:0/x",
            "http://h:65536/x",
            "http://h:/x",
            "http://h:8a/x",
            "http://h/a b",
            "http://h/a\r\nx: y",
            "http://h/a\u{e9}",
            "http://h/a#frag",
            "http://h#frag/a",
            "http://[::1/x",
            "http://[zz]/x",
            "http://h h/x",
            "http://h\r\nx: y/",
        ] {
            assert!(Endpoint::parse(url).is_none(), "{url:?}");
        }
    }

    #[test]
    fn an_endpoint_never_prints_its_query() {
        let e = endpoint("http://h/x?token=hunter2");
        assert!(!format!("{e:?}").contains("hunter2"));
    }
}
