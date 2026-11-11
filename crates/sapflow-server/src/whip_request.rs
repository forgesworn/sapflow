//! The WHIP listener's request rules: a parsed head in, a decision out, no
//! I/O, so `fuzz/fuzz_targets/whip_request.rs` includes this file with
//! `http_lite.rs` (docs/whip-ingest.md, Resources).
//!
//! - `POST /whip/<stream path>` publishes; `DELETE /whip/session/<id>`
//!   ends a session; `OPTIONS` on either is a CORS preflight; any other
//!   method on either is 405 with `Allow`, which answers `PATCH` (no
//!   trickle ICE or ICE restart in v1). Anything else is 404.
//! - The offer is `application/sdp` (415 otherwise) and at most
//!   [`MAX_OFFER`] bytes by its single `content-length` (413 over it, 400
//!   for chunked or repeated lengths), checked before a byte of it is read.
//! - CORS headers go only to an origin in `SAPFLOW_WHIP_ALLOW_ORIGINS`.
//! - The publisher's address is the peer's, or the last `X-Forwarded-For`
//!   entry when the peer is `SAPFLOW_WHIP_TRUSTED_PROXY` (400 if that
//!   header is missing, repeated or not an address).

use std::net::IpAddr;

use crate::http_lite::{Framing, RequestHead};
use sapflow_whip::resource::{self, SESSION_PREFIX};

/// The largest offer taken, as `sapflow_whip::sdp::Limits` allows.
pub const MAX_OFFER: usize = 16 * 1024;

/// What a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action<'a> {
    /// `POST /whip/<stream path>`: the stream path (`live/<eventId>`).
    Publish(&'a str),
    /// `DELETE /whip/session/<id>`: the ID as sent.
    Delete(&'a str),
    /// `OPTIONS` on a WHIP resource.
    Preflight,
}

/// A refusal: its status, a short plain-text reason, and `Allow` for 405.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub status: u16,
    pub reason: &'static str,
    pub allow: Option<&'static str>,
}

impl Refused {
    pub const fn new(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            allow: None,
        }
    }
}

impl From<resource::Refusal> for Refused {
    fn from(r: resource::Refusal) -> Self {
        Self::new(r.status(), r.reason())
    }
}

/// The route and method.
pub fn route<'a>(method: &str, path: &'a str) -> Result<Action<'a>, Refused> {
    let not_allowed = |allow| Refused {
        allow: Some(allow),
        ..Refused::new(405, "method not allowed")
    };
    if let Some(id) = path.strip_prefix(SESSION_PREFIX) {
        if id.is_empty() || id.contains('/') {
            return Err(Refused::new(404, "not found"));
        }
        return match method {
            "DELETE" => Ok(Action::Delete(id)),
            "OPTIONS" => Ok(Action::Preflight),
            _ => Err(not_allowed("DELETE, OPTIONS")),
        };
    }
    match resource::stream_path(path) {
        Some(stream) => match method {
            "POST" => Ok(Action::Publish(stream)),
            "OPTIONS" => Ok(Action::Preflight),
            _ => Err(not_allowed("POST, OPTIONS")),
        },
        None => Err(Refused::new(404, "not found")),
    }
}

/// The offer's length, checked against [`MAX_OFFER`] before it is read,
/// after its content type.
pub fn offer_length(head: &RequestHead) -> Result<usize, Refused> {
    if !head
        .content_type
        .as_deref()
        .is_some_and(resource::is_sdp_content_type)
    {
        return Err(resource::Refusal::UnsupportedMediaType.into());
    }
    match head.framing {
        Framing::Ambiguous => Err(Refused::new(400, "one content-length, no chunking")),
        Framing::Length(n) => match usize::try_from(n) {
            Ok(n) if n <= MAX_OFFER => Ok(n),
            _ => Err(Refused::new(413, "the offer is over 16 KiB")),
        },
    }
}

/// The origin to name in CORS headers, if the request's is allowed.
/// Origins compare without regard to ASCII case.
pub fn allowed_origin<'a>(origin: Option<&'a str>, allowed: &[String]) -> Option<&'a str> {
    let origin = origin?;
    allowed
        .iter()
        .any(|a| a.eq_ignore_ascii_case(origin))
        .then_some(origin)
}

/// CORS headers for an allowed origin: the preflight's, or an answer's.
pub fn cors_headers(origin: &str, preflight: bool) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("access-control-allow-origin", origin.to_owned()),
        ("vary", "origin".to_owned()),
    ];
    if preflight {
        h.push((
            "access-control-allow-methods",
            "POST, DELETE, OPTIONS".to_owned(),
        ));
        h.push((
            "access-control-allow-headers",
            "authorization, content-type".to_owned(),
        ));
        h.push(("access-control-max-age", "600".to_owned()));
    } else {
        h.push(("access-control-expose-headers", "location".to_owned()));
    }
    h
}

/// The publisher's address for admission and the throttle.
pub fn publisher_ip(
    peer: IpAddr,
    trusted: Option<IpAddr>,
    forwarded_for: Option<&str>,
) -> Result<IpAddr, Refused> {
    if trusted != Some(peer) {
        return Ok(peer);
    }
    forwarded_for
        .and_then(|v| v.rsplit(',').next())
        .and_then(|last| last.trim().parse::<IpAddr>().ok())
        .ok_or(Refused::new(
            400,
            "x-forwarded-for from the proxy is not an address",
        ))
}

/// The bearer token's user and password, from the `authorization` value.
pub fn token(authorization: Option<&str>) -> Result<resource::Token<'_>, Refused> {
    authorization
        .and_then(resource::Token::from_authorization)
        .ok_or_else(|| resource::Refusal::Unauthorised.into())
}

/// The bearer token as sent, by the same rule as [`token`]: what `DELETE`
/// must repeat.
pub fn bearer(authorization: Option<&str>) -> Option<&str> {
    let (scheme, token) = authorization?.trim().split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// The fuzz target's body, and the seed replay's: a head parsed from
/// arbitrary bytes through every rule here, checking each answer.
#[cfg(any(test, feature = "fuzzing"))]
pub fn check(data: &[u8]) {
    let crate::http_lite::ParsedRequest::Complete(head, used) =
        crate::http_lite::parse_request(data)
    else {
        return;
    };
    assert!(used <= data.len());
    match route(&head.method, &head.path) {
        Ok(Action::Publish(stream)) => {
            assert_eq!(head.method, "POST");
            assert_eq!(head.path, format!("{}{stream}", resource::PUBLISH_PREFIX));
            assert!(!head.path.starts_with(SESSION_PREFIX) && !stream.is_empty());
        }
        Ok(Action::Delete(id)) => {
            assert_eq!(head.method, "DELETE");
            assert!(!id.is_empty() && !id.contains('/'));
        }
        Ok(Action::Preflight) => assert_eq!(head.method, "OPTIONS"),
        Err(r) => {
            assert!(matches!(r.status, 404 | 405));
            assert_eq!(r.allow.is_some(), r.status == 405);
        }
    }
    match offer_length(&head) {
        Ok(n) => {
            assert!(n <= MAX_OFFER);
            assert_eq!(head.framing, Framing::Length(n as u64));
        }
        Err(r) => assert!(matches!(r.status, 400 | 413 | 415)),
    }
    let allowed = ["https://stage.example".to_owned()];
    if let Some(origin) = allowed_origin(head.origin.as_deref(), &allowed) {
        assert!(origin.eq_ignore_ascii_case(&allowed[0]));
        assert!(cors_headers(origin, true).len() > cors_headers(origin, false).len());
    }
    let proxy = IpAddr::from([10, 0, 0, 1]);
    let peer = IpAddr::from([192, 0, 2, 1]);
    assert_eq!(
        publisher_ip(peer, Some(proxy), head.forwarded_for.as_deref()),
        Ok(peer),
        "only the proxy is believed"
    );
    if let Ok(ip) = publisher_ip(proxy, Some(proxy), head.forwarded_for.as_deref()) {
        let last = head
            .forwarded_for
            .as_deref()
            .and_then(|v| v.rsplit(',').next())
            .map(|l| l.trim().parse::<IpAddr>());
        assert_eq!(last, Some(Ok(ip)), "the proxy's own entry");
    }
    let authorization = head.authorization.as_ref().map(|c| c.value());
    let split = token(authorization);
    let sent = bearer(authorization);
    assert_eq!(split.is_ok(), sent.is_some(), "one bearer rule");
    if let (Ok(t), Some(b)) = (split, sent) {
        if b.contains(':') {
            assert_eq!(format!("{}:{}", t.user, t.password), b);
        } else {
            assert_eq!((t.user, t.password), ("", b));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every committed seed runs through the fuzz target's checks.
    #[test]
    fn seeds_hold_the_fuzz_checks() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/whip_request");
        let mut n = 0;
        for entry in std::fs::read_dir(&dir).expect("seeds") {
            let bytes = std::fs::read(entry.expect("seed").path()).expect("seed");
            check(&bytes);
            n += 1;
        }
        assert!(n >= 6, "{n} seeds");
    }

    /// A fixed-seed mutation pass over the seeds: byte flips, cuts and
    /// insertions, each through the fuzz target's checks. Not coverage
    /// guided; it runs where cargo-fuzz does not.
    #[test]
    fn mutated_seeds_hold_the_fuzz_checks() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/whip_request");
        let mut state = 0x5eed_u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize
        };
        let mut runs = 0;
        for entry in std::fs::read_dir(&dir).expect("seeds") {
            let seed = std::fs::read(entry.expect("seed").path()).expect("seed");
            for _ in 0..2_000 {
                let mut b = seed.clone();
                for _ in 0..1 + next() % 4 {
                    let i = next() % b.len().max(1);
                    match next() % 4 {
                        0 if !b.is_empty() => b[i] ^= 1 << (next() % 8),
                        1 if !b.is_empty() => b.truncate(i),
                        2 => b.insert(i.min(b.len()), b"\r\n:, \0A"[next() % 7]),
                        _ if !b.is_empty() => b[i] = next() as u8,
                        _ => {}
                    }
                }
                check(&b);
                runs += 1;
            }
        }
        assert!(runs >= 12_000);
    }

    fn head(raw: &str) -> RequestHead {
        match crate::http_lite::parse_request(raw.as_bytes()) {
            crate::http_lite::ParsedRequest::Complete(h, _) => h,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn routes_and_methods() {
        assert_eq!(
            route("POST", "/whip/live/abc"),
            Ok(Action::Publish("live/abc"))
        );
        assert_eq!(route("OPTIONS", "/whip/live/abc"), Ok(Action::Preflight));
        assert_eq!(
            route("DELETE", "/whip/session/00ff"),
            Ok(Action::Delete("00ff"))
        );
        assert_eq!(
            route("OPTIONS", "/whip/session/00ff"),
            Ok(Action::Preflight)
        );
        let patch = route("PATCH", "/whip/session/00ff").expect_err("405");
        assert_eq!((patch.status, patch.allow), (405, Some("DELETE, OPTIONS")));
        let get = route("GET", "/whip/live/abc").expect_err("405");
        assert_eq!((get.status, get.allow), (405, Some("POST, OPTIONS")));
        for path in [
            "/",
            "/whip/",
            "/whip/session/",
            "/whip/session/a/b",
            "/live/abc",
        ] {
            assert_eq!(
                route("POST", path).map_err(|r| r.status),
                Err(404),
                "{path}"
            );
        }
    }

    #[test]
    fn the_offer_must_be_sdp_and_at_most_16_kib() {
        let ok = head(
            "POST /whip/live/a HTTP/1.1\r\ncontent-type: application/sdp\r\ncontent-length: 16384\r\n\r\n",
        );
        assert_eq!(offer_length(&ok), Ok(MAX_OFFER));
        let params = head(
            "POST /whip/live/a HTTP/1.1\r\ncontent-type: Application/SDP; charset=utf-8\r\ncontent-length: 5\r\n\r\n",
        );
        assert_eq!(offer_length(&params), Ok(5));
        let status = |raw: &str| offer_length(&head(raw)).map_err(|r| r.status);
        assert_eq!(
            status(
                "POST /whip/live/a HTTP/1.1\r\ncontent-type: application/sdp\r\ncontent-length: 16385\r\n\r\n"
            ),
            Err(413)
        );
        assert_eq!(
            status(
                "POST /whip/live/a HTTP/1.1\r\ncontent-type: text/plain\r\ncontent-length: 5\r\n\r\n"
            ),
            Err(415)
        );
        assert_eq!(
            status("POST /whip/live/a HTTP/1.1\r\ncontent-length: 5\r\n\r\n"),
            Err(415)
        );
        assert_eq!(
            status(
                "POST /whip/live/a HTTP/1.1\r\ncontent-type: application/sdp\r\ncontent-type: application/sdp\r\ncontent-length: 5\r\n\r\n"
            ),
            Err(415),
            "a repeated content type is ambiguous"
        );
        assert_eq!(
            status(
                "POST /whip/live/a HTTP/1.1\r\ncontent-type: application/sdp\r\ntransfer-encoding: chunked\r\n\r\n"
            ),
            Err(400)
        );
    }

    #[test]
    fn cors_only_for_configured_origins() {
        let allowed = vec!["https://stage.example".to_owned()];
        assert_eq!(
            allowed_origin(Some("https://stage.example"), &allowed),
            Some("https://stage.example")
        );
        assert_eq!(
            allowed_origin(Some("HTTPS://Stage.Example"), &allowed),
            Some("HTTPS://Stage.Example")
        );
        assert_eq!(
            allowed_origin(Some("https://other.example"), &allowed),
            None
        );
        assert_eq!(allowed_origin(None, &allowed), None);
        assert_eq!(allowed_origin(Some("null"), &allowed), None);
        let pre = cors_headers("https://stage.example", true);
        assert!(pre.contains(&(
            "access-control-allow-methods",
            "POST, DELETE, OPTIONS".into()
        )));
        assert!(pre.contains(&(
            "access-control-allow-headers",
            "authorization, content-type".into()
        )));
        let answer = cors_headers("https://stage.example", false);
        assert!(answer.contains(&("access-control-expose-headers", "location".into())));
    }

    #[test]
    fn forwarded_for_only_from_the_trusted_proxy() {
        let peer: IpAddr = "10.0.0.2".parse().expect("ip");
        let proxy: IpAddr = "10.0.0.1".parse().expect("ip");
        let client: IpAddr = "192.0.2.9".parse().expect("ip");
        assert_eq!(publisher_ip(peer, None, Some("192.0.2.9")), Ok(peer));
        assert_eq!(publisher_ip(peer, Some(proxy), Some("192.0.2.9")), Ok(peer));
        assert_eq!(
            publisher_ip(proxy, Some(proxy), Some("198.51.100.1, 192.0.2.9")),
            Ok(client),
            "the entry the proxy added"
        );
        for bad in [None, Some(""), Some("192.0.2.9:4000"), Some("unknown")] {
            assert_eq!(
                publisher_ip(proxy, Some(proxy), bad).map_err(|r| r.status),
                Err(400),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_bearer_token_is_required() {
        let t = token(Some("Bearer user:pass")).expect("bearer");
        assert_eq!((t.user, t.password), ("user", "pass"));
        let t = token(Some("bearer only-a-password")).expect("case-insensitive scheme");
        assert_eq!((t.user, t.password), ("", "only-a-password"));
        for bad in [
            None,
            Some("Basic dXNlcjpwYXNz"),
            Some("Bearer "),
            Some("Bearer"),
        ] {
            assert_eq!(
                token(bad).map(|_| ()).map_err(|r| r.status),
                Err(401),
                "{bad:?}"
            );
            assert_eq!(bearer(bad), None, "{bad:?}");
        }
        assert_eq!(bearer(Some(" BEARER  a:b ")), Some("a:b"));
    }
}
