//! HTTP/1.1 request framing for the viewer event loop (`own_http.rs`):
//! bytes in, one request out at a time. No I/O, so it is unit-tested here and
//! fuzzed by `fuzz/fuzz_targets/http1_request.rs`, which includes this file.
//!
//! Viewers only send `GET`, `HEAD` and `OPTIONS` without a body. A request
//! that declares a body is refused rather than skipped, so a body can never be
//! read as the next request.

/// The longest request head accepted: request line and headers.
pub const MAX_HEAD: usize = 16 * 1024;
/// The most headers accepted in one request.
const MAX_HEADERS: usize = 64;

/// The request methods the viewer path answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Options,
    Other,
}

/// One parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    /// The path and query. An absolute-form target (`http://host/path`)
    /// arrives here as its path; `OPTIONS *` as `*`.
    pub target: String,
    /// Close the connection after answering.
    pub close: bool,
    /// An HTTP/1.0 client asked to keep the connection: say yes in the reply.
    pub keep_alive_10: bool,
}

/// The result of looking for a request at the start of a buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// A request, and how many bytes of the buffer it used.
    Request(Request, usize),
    /// Not a whole request head yet.
    Partial,
    /// Malformed, too large, or with a body: answer 400 and close.
    Invalid,
}

/// Parses the request at the start of `buf`.
pub fn parse(buf: &[u8]) -> Parsed {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut headers);
    let used = match req.parse(buf) {
        Ok(httparse::Status::Complete(n)) if n <= MAX_HEAD => n,
        Ok(httparse::Status::Complete(_)) => return Parsed::Invalid,
        Ok(httparse::Status::Partial) if buf.len() > MAX_HEAD => return Parsed::Invalid,
        Ok(httparse::Status::Partial) => return Parsed::Partial,
        Err(_) => return Parsed::Invalid,
    };
    let (Some(method), Some(target), Some(version)) = (req.method, req.path, req.version) else {
        return Parsed::Invalid;
    };
    let Some(target) = origin_form(target) else {
        return Parsed::Invalid;
    };
    // HTTP/1.0 closes unless asked to keep alive; HTTP/1.1 stays open unless
    // asked to close.
    let mut close = version == 0;
    for h in req.headers.iter() {
        if h.name.eq_ignore_ascii_case("transfer-encoding") {
            return Parsed::Invalid;
        }
        if h.name.eq_ignore_ascii_case("content-length") {
            if h.value.trim_ascii() != b"0" {
                return Parsed::Invalid;
            }
        } else if h.name.eq_ignore_ascii_case("connection") {
            for token in h.value.split(|&b| b == b',') {
                let token = token.trim_ascii();
                if token.eq_ignore_ascii_case(b"close") {
                    close = true;
                } else if token.eq_ignore_ascii_case(b"keep-alive") && version == 0 {
                    close = false;
                }
            }
        }
    }
    let keep_alive_10 = version == 0 && !close;
    let method = match method {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "OPTIONS" => Method::Options,
        _ => Method::Other,
    };
    Parsed::Request(
        Request {
            method,
            target,
            close,
            keep_alive_10,
        },
        used,
    )
}

/// The target in origin form: as sent if it starts with `/`, the path of an
/// absolute-form `http://` or `https://` target (RFC 9112 3.2.2), `*` as is.
fn origin_form(target: &str) -> Option<String> {
    if target.starts_with('/') || target == "*" {
        return Some(target.to_owned());
    }
    let scheme_end = target.find("://")?;
    let scheme = target.get(..scheme_end)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let rest = target.get(scheme_end + 3..)?;
    match rest.find(['/', '?']) {
        Some(i) if rest.get(i..i + 1) == Some("/") => rest.get(i..).map(str::to_owned),
        Some(i) => rest.get(i..).map(|q| format!("/{q}")),
        None => Some("/".to_owned()),
    }
}

/// How the connection is left after a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persist {
    /// HTTP/1.1 default: say nothing.
    Default,
    /// Closing: `connection: close`.
    Close,
    /// An HTTP/1.0 client asked to keep alive: `connection: keep-alive`.
    KeepAlive,
}

/// A response head. `headers` are extra header lines, each ending `\r\n`.
/// `content_length` is `None` for a 204, which must not carry one.
pub fn head(
    status: &str,
    headers: &str,
    content_length: Option<usize>,
    persist: Persist,
) -> String {
    let length = content_length.map_or(String::new(), |n| format!("content-length: {n}\r\n"));
    let connection = match persist {
        Persist::Default => "",
        Persist::Close => "connection: close\r\n",
        Persist::KeepAlive => "connection: keep-alive\r\n",
    };
    format!(
        "HTTP/1.1 {status}\r\n{headers}access-control-allow-origin: *\r\n{length}{connection}\r\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(raw: &str) -> (Request, usize) {
        match parse(raw.as_bytes()) {
            Parsed::Request(r, n) => (r, n),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    #[test]
    fn a_viewer_request_stays_open() {
        let raw =
            "GET /live/event-a/stream.m3u8?_HLS_msn=4&_HLS_part=1 HTTP/1.1\r\nhost: x\r\n\r\n";
        let (r, n) = request(raw);
        assert_eq!(r.method, Method::Get);
        assert_eq!(r.target, "/live/event-a/stream.m3u8?_HLS_msn=4&_HLS_part=1");
        assert!(!r.close);
        assert_eq!(n, raw.len());
    }

    #[test]
    fn pipelined_requests_come_out_one_at_a_time() {
        let raw = "GET /a HTTP/1.1\r\n\r\nHEAD /b HTTP/1.1\r\n\r\n";
        let (first, n) = request(raw);
        assert_eq!(first.target, "/a");
        let (second, m) = request(&raw[n..]);
        assert_eq!(
            (second.method, second.target.as_str()),
            (Method::Head, "/b")
        );
        assert_eq!(n + m, raw.len());
    }

    #[test]
    fn a_partial_head_waits_and_an_endless_one_is_refused() {
        assert_eq!(parse(b"GET /a HTTP/1.1\r\nhost: x\r\n"), Parsed::Partial);
        let mut endless = b"GET /a HTTP/1.1\r\nx: ".to_vec();
        endless.resize(MAX_HEAD + 1, b'a');
        assert_eq!(parse(&endless), Parsed::Invalid);
    }

    #[test]
    fn connection_close_and_http_1_0_close() {
        assert!(
            request("GET / HTTP/1.1\r\nConnection: keep-alive, Close\r\n\r\n")
                .0
                .close
        );
        assert!(request("GET / HTTP/1.0\r\n\r\n").0.close);
        assert!(
            !request("GET / HTTP/1.0\r\nconnection: keep-alive\r\n\r\n")
                .0
                .close
        );
    }

    #[test]
    fn a_request_with_a_body_is_refused() {
        assert_eq!(
            parse(b"GET / HTTP/1.1\r\ncontent-length: 5\r\n\r\nGET /"),
            Parsed::Invalid
        );
        assert_eq!(
            parse(b"GET / HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n"),
            Parsed::Invalid
        );
        assert!(matches!(
            parse(b"GET / HTTP/1.1\r\ncontent-length: 0\r\n\r\n"),
            Parsed::Request(..)
        ));
    }

    #[test]
    fn garbage_absolute_targets_and_too_many_headers_are_refused() {
        assert_eq!(parse(b"\x16\x03\x01\x02\x00\r\n\r\n"), Parsed::Invalid);
        assert_eq!(parse(b"GET ftp://x/ HTTP/1.1\r\n\r\n"), Parsed::Invalid);
        assert_eq!(parse(b"GET x/y HTTP/1.1\r\n\r\n"), Parsed::Invalid);
        let mut many = String::from("GET / HTTP/1.1\r\n");
        for i in 0..=MAX_HEADERS {
            many.push_str(&format!("h{i}: v\r\n"));
        }
        many.push_str("\r\n");
        assert_eq!(parse(many.as_bytes()), Parsed::Invalid);
    }

    #[test]
    fn absolute_form_and_asterisk_targets_become_paths() {
        let t = |raw: &str| request(raw).0.target;
        assert_eq!(
            t("GET http://h:8888/live/e/f?q=1 HTTP/1.1\r\n\r\n"),
            "/live/e/f?q=1"
        );
        assert_eq!(t("GET HTTPS://h HTTP/1.1\r\n\r\n"), "/");
        assert_eq!(t("GET http://h?q HTTP/1.1\r\n\r\n"), "/?q");
        assert_eq!(t("OPTIONS * HTTP/1.1\r\n\r\n"), "*");
    }

    #[test]
    fn http_1_0_keep_alive_is_echoed() {
        assert!(
            request("GET / HTTP/1.0\r\nconnection: keep-alive\r\n\r\n")
                .0
                .keep_alive_10
        );
        assert!(!request("GET / HTTP/1.0\r\n\r\n").0.keep_alive_10);
        assert!(
            !request("GET / HTTP/1.1\r\nconnection: keep-alive\r\n\r\n")
                .0
                .keep_alive_10
        );
    }

    #[test]
    fn other_methods_parse_so_they_can_be_answered_405() {
        assert_eq!(request("POST / HTTP/1.1\r\n\r\n").0.method, Method::Other);
    }

    #[test]
    fn response_heads_carry_cors_length_and_close() {
        let h = head(
            "200 OK",
            "content-type: video/mp4\r\n",
            Some(3),
            Persist::Default,
        );
        assert_eq!(
            h,
            "HTTP/1.1 200 OK\r\ncontent-type: video/mp4\r\naccess-control-allow-origin: *\r\ncontent-length: 3\r\n\r\n"
        );
        assert!(
            head("400 Bad Request", "", Some(0), Persist::Close).contains("connection: close\r\n")
        );
        assert!(
            head("200 OK", "", Some(0), Persist::KeepAlive).contains("connection: keep-alive\r\n")
        );
        assert!(!head("204 No Content", "", None, Persist::Default).contains("content-length"));
    }
}
