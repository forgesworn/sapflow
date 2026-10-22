//! HTTP/1.1 request framing for the viewer event loop (`own_http.rs`):
//! arbitrary bytes taken one request at a time, as a connection would. No I/O.
#![no_main]

use libfuzzer_sys::fuzz_target;

// Compile the production module, rather than a second fuzz-only parser.
#[path = "../../crates/sapflow-server/src/http1.rs"]
mod http1;

use http1::{MAX_HEAD, Parsed};

fuzz_target!(|data: &[u8]| {
    let mut rest = data;
    loop {
        match http1::parse(rest) {
            Parsed::Request(req, used) => {
                // Progress, within the buffer and the head limit.
                assert!(used > 0 && used <= rest.len() && used <= MAX_HEAD);
                assert!(req.target.starts_with('/') || req.target == "*");
                // The same bytes with more after them give the same request.
                let mut longer = rest.to_vec();
                longer.extend_from_slice(b"GET / HTTP/1.1\r\n");
                assert_eq!(http1::parse(&longer), Parsed::Request(req.clone(), used));
                let persistence = if req.close {
                    http1::Persist::Close
                } else if req.keep_alive_10 {
                    http1::Persist::KeepAlive
                } else {
                    http1::Persist::Default
                };
                let head = http1::head("200 OK", "", Some(used), persistence);
                assert!(head.ends_with("\r\n\r\n"));
                assert_eq!(head.contains("connection: close\r\n"), req.close);
                assert_eq!(
                    head.contains("connection: keep-alive\r\n"),
                    req.keep_alive_10
                );
                rest = rest.get(used..).unwrap_or_default();
            }
            // A partial head is never longer than the limit.
            Parsed::Partial => {
                assert!(rest.len() <= MAX_HEAD);
                break;
            }
            Parsed::Invalid => break,
        }
    }
});
