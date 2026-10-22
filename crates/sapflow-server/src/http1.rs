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
    /// The request target as sent: path and query, origin form.
    pub target: String,
    /// Close the connection after answering.
    pub close: bool,
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
    if !target.starts_with('/') {
        return Parsed::Invalid;
    }
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
    let method = match method {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "OPTIONS" => Method::Options,
        _ => Method::Other,
    };
    Parsed::Request(
        Request {
            method,
            target: target.to_owned(),
            close,
        },
        used,
    )
}

/// A response head. `headers` are extra header lines, each ending `\r\n`.
pub fn head(status: &str, headers: &str, content_length: usize, close: bool) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{headers}access-control-allow-origin: *\r\ncontent-length: {content_length}\r\n{}\r\n",
        if close { "connection: close\r\n" } else { "" }
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
        assert_eq!(parse(b"GET http://x/ HTTP/1.1\r\n\r\n"), Parsed::Invalid);
        let mut many = String::from("GET / HTTP/1.1\r\n");
        for i in 0..=MAX_HEADERS {
            many.push_str(&format!("h{i}: v\r\n"));
        }
        many.push_str("\r\n");
        assert_eq!(parse(many.as_bytes()), Parsed::Invalid);
    }

    #[test]
    fn other_methods_parse_so_they_can_be_answered_405() {
        assert_eq!(request("POST / HTTP/1.1\r\n\r\n").0.method, Method::Other);
    }

    #[test]
    fn response_heads_carry_cors_length_and_close() {
        let h = head("200 OK", "content-type: video/mp4\r\n", 3, false);
        assert_eq!(
            h,
            "HTTP/1.1 200 OK\r\ncontent-type: video/mp4\r\naccess-control-allow-origin: *\r\ncontent-length: 3\r\n\r\n"
        );
        assert!(head("400 Bad Request", "", 0, true).contains("connection: close\r\n"));
    }
}
