//! The private control API's request heads and the callback client's response
//! heads and URLs (`http_lite.rs`): arbitrary bytes in, no I/O.
#![no_main]

use libfuzzer_sys::fuzz_target;

// Compile the production module, rather than a second fuzz-only parser.
#[path = "../../crates/sapflow-server/src/http_lite.rs"]
mod http_lite;

use http_lite::{Endpoint, Framing, MAX_HEAD, MAX_TARGET, ParsedRequest, ParsedResponse};

fuzz_target!(|data: &[u8]| {
    match http_lite::parse_request(data) {
        ParsedRequest::Complete(head, used) => {
            // Progress, within the buffer and the head limit.
            assert!(used > 0 && used <= data.len() && used <= MAX_HEAD);
            assert!(head.path.starts_with('/') && head.path.len() <= MAX_TARGET);
            assert!(!head.path.contains(['?', '#']));
            // Every line of the head ends in CRLF, and the head ends with a blank one.
            let text = &data[..used];
            assert!(text.ends_with(b"\r\n\r\n"));
            assert!(
                text.iter()
                    .enumerate()
                    .all(|(i, &b)| b != b'\n' || (i > 0 && text[i - 1] == b'\r'))
            );
            // Framing is one plain length, or ambiguous: never a guess.
            if let Framing::Length(_) = head.framing {
                assert!(!String::from_utf8_lossy(text)
                    .to_ascii_lowercase()
                    .contains("\r\ntransfer-encoding:"));
            }
            // The same bytes with more after them give the same head.
            let mut longer = data[..used].to_vec();
            longer.extend_from_slice(b"X: y\r\n\r\nGET / HTTP/1.1\r\n\r\n");
            match http_lite::parse_request(&longer) {
                ParsedRequest::Complete(again, again_used) => {
                    assert_eq!(again_used, used);
                    assert_eq!(again, head);
                }
                other => panic!("a complete head changed with trailing bytes: {other:?}"),
            }
        }
        // A partial head is never as long as the limit.
        ParsedRequest::Partial => assert!(data.len() < MAX_HEAD),
        ParsedRequest::Refused(_) => {}
    }

    if let ParsedResponse::Complete { status, used } = http_lite::parse_response(data) {
        assert!((100..600).contains(&status));
        assert!(used > 0 && used <= data.len() && used <= MAX_HEAD);
    }

    // Whatever URL is accepted becomes one request head that the server side
    // reads back as the same POST, so no URL can add a header or a request.
    if let Ok(url) = std::str::from_utf8(data)
        && let Some(endpoint) = Endpoint::parse(url)
        && url.len() <= 1024
    {
        let sent = endpoint.post("application/json", b"{}");
        match http_lite::parse_request(&sent) {
            ParsedRequest::Complete(head, used) => {
                assert_eq!(head.method, "POST");
                assert_eq!(head.framing, Framing::Length(2));
                assert!(head.authorization.is_none());
                assert_eq!(&sent[used..], b"{}");
            }
            other => panic!("the client sent a request it cannot read back: {other:?}"),
        }
    }
});
