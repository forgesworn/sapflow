//! The WHIP resource rules (RFC 9725, `docs/whip-ingest.md` Resources and
//! Admission), as pure functions for the signalling listener (W3c), which
//! owns HTTP.
//!
//! - [`Token::split`]: a bearer token is `<user>:<password>`, split at the
//!   first colon; with no colon the user is empty and the whole token is
//!   the password.
//! - [`SessionId`]: 128 random bits, written as 32 lower-case hex digits in
//!   `/whip/session/<id>`. It is not a credential: `DELETE` also needs the
//!   token.
//! - [`Refusal`]: every refusal and its status.
//! - [`PublicUrl`]: the absolute base URL `Location` is built from.

use crate::entropy::{Entropy, EntropyError};
use std::fmt;

/// A bearer token, split into the user and password the publish-auth
/// contract takes. `Debug` shows neither.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    /// Before the first colon, or empty.
    pub user: &'a str,
    /// After the first colon, or the whole token.
    pub password: &'a str,
}

impl fmt::Debug for Token<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl<'a> Token<'a> {
    /// Splits a token at its first colon.
    pub fn split(token: &'a str) -> Self {
        match token.split_once(':') {
            Some((user, password)) => Self { user, password },
            None => Self {
                user: "",
                password: token,
            },
        }
    }

    /// The token from an `Authorization` header value, if it is a bearer
    /// token (the scheme is case-insensitive, RFC 9110 section 11.1).
    pub fn from_authorization(header: &'a str) -> Option<Self> {
        let (scheme, token) = header.trim().split_once(' ')?;
        let token = token.trim();
        (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| Self::split(token))
    }
}

/// A WHIP session's ID: 128 random bits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId([u8; 16]);

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId({self})")
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl SessionId {
    /// A new random ID.
    pub fn generate() -> Result<Self, EntropyError> {
        Ok(Self(*Entropy::System.array::<16>()?))
    }

    /// As [`SessionId::generate`], from a seed. Not for production use.
    #[cfg(any(test, feature = "testing"))]
    pub fn generate_seeded(seed: [u8; 32]) -> Result<Self, EntropyError> {
        Ok(Self(*Entropy::seeded(seed).array::<16>()?))
    }

    /// The 16 bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Reads 32 hex digits, either case, and nothing else.
    pub fn parse(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 32 {
            return None;
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        };
        let mut out = [0u8; 16];
        for (o, pair) in out.iter_mut().zip(b.chunks_exact(2)) {
            let [hi, lo] = <[u8; 2]>::try_from(pair).ok()?;
            *o = (nibble(hi)? << 4) | nibble(lo)?;
        }
        Some(Self(out))
    }

    /// The resource path, `/whip/session/<id>`.
    pub fn path(&self) -> String {
        format!("{SESSION_PREFIX}{self}")
    }

    /// The ID in a resource path, if it is one.
    pub fn from_path(path: &str) -> Option<Self> {
        Self::parse(path.strip_prefix(SESSION_PREFIX)?)
    }
}

/// Where session resources live.
pub const SESSION_PREFIX: &str = "/whip/session/";
/// Where publishers `POST`: `/whip/<stream path>`, for example
/// `/whip/live/<eventId>`.
pub const PUBLISH_PREFIX: &str = "/whip/";

/// The stream path a `POST` names (`live/<eventId>` for
/// `/whip/live/<eventId>`), if the request path is under the publish
/// prefix and not a session resource. The server's own path rules apply
/// after this.
pub fn stream_path(request_path: &str) -> Option<&str> {
    if request_path.starts_with(SESSION_PREFIX) {
        return None;
    }
    request_path
        .strip_prefix(PUBLISH_PREFIX)
        .filter(|p| !p.is_empty())
}

/// The configured public base URL (`SAPFLOW_WHIP_PUBLIC_URL`): `http` or
/// `https`, with a host, and no query or fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicUrl(String);

impl PublicUrl {
    /// Checks and keeps a base URL, without a trailing slash.
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        let host = rest.split('/').next().unwrap_or_default();
        let bad = |c: char| c.is_whitespace() || c.is_control() || matches!(c, '?' | '#' | '@');
        if host.is_empty() || url.chars().any(bad) {
            return None;
        }
        Some(Self(url.trim_end_matches('/').to_owned()))
    }

    /// The absolute `Location` of a session.
    pub fn location(&self, id: &SessionId) -> String {
        format!("{}{}", self.0, id.path())
    }
}

/// Every refusal the signalling listener makes, and its status
/// (`docs/whip-ingest.md`, Resources).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// 415: the content type is missing or not `application/sdp`.
    UnsupportedMediaType,
    /// 400: the SDP does not parse.
    Malformed(crate::sdp::ParseError),
    /// 401: no token, or the app answered 401.
    Unauthorised,
    /// 403: the app answered 403.
    Forbidden,
    /// 409: the path is live with another publisher.
    Conflict,
    /// 422: a well-formed offer outside the profile.
    OutsideProfile(crate::sdp::ProfileError),
    /// 429: throttled.
    Throttled,
    /// 503: the app did not answer usably, or a resource cap is reached.
    Unavailable,
}

impl Refusal {
    /// The HTTP status.
    pub const fn status(self) -> u16 {
        match self {
            Self::UnsupportedMediaType => 415,
            Self::Malformed(_) => 400,
            Self::Unauthorised => 401,
            Self::Forbidden => 403,
            Self::Conflict => 409,
            Self::OutsideProfile(_) => 422,
            Self::Throttled => 429,
            Self::Unavailable => 503,
        }
    }

    /// The short plain-text reason for the body.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::UnsupportedMediaType => "the offer must be application/sdp",
            Self::Malformed(e) => crate::sdp::Refusal::Malformed(e).reason(),
            Self::Unauthorised => "a bearer token is required",
            Self::Forbidden => "publishing is not allowed",
            Self::Conflict => "the stream is live with another publisher",
            Self::OutsideProfile(e) => crate::sdp::Refusal::Profile(e).reason(),
            Self::Throttled => "too many attempts",
            Self::Unavailable => "unavailable, try again",
        }
    }
}

impl From<crate::sdp::Refusal> for Refusal {
    fn from(r: crate::sdp::Refusal) -> Self {
        match r {
            crate::sdp::Refusal::Malformed(e) => Self::Malformed(e),
            crate::sdp::Refusal::Profile(e) => Self::OutsideProfile(e),
        }
    }
}

/// Whether a `Content-Type` value is `application/sdp`, parameters
/// allowed (RFC 9725 section 4.2).
pub fn is_sdp_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/sdp"))
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.status(), self.reason())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdp::{ParseError, ProfileError};

    #[test]
    fn token_splits_at_the_first_colon() {
        assert_eq!(
            Token::split("user:pass:word"),
            Token {
                user: "user",
                password: "pass:word"
            }
        );
        assert_eq!(
            Token::split("opaque"),
            Token {
                user: "",
                password: "opaque"
            }
        );
        assert_eq!(
            Token::split(":p"),
            Token {
                user: "",
                password: "p"
            }
        );
        assert_eq!(
            Token::split("u:"),
            Token {
                user: "u",
                password: ""
            }
        );
        let t = Token::from_authorization("Bearer u:secret").expect("valid");
        assert_eq!((t.user, t.password), ("u", "secret"));
        assert_eq!(
            Token::from_authorization("bearer x"),
            Some(Token::split("x"))
        );
        assert_eq!(Token::from_authorization("Basic dTpw"), None);
        assert_eq!(Token::from_authorization("Bearer "), None);
        assert!(!format!("{t:?}").contains("secret"));
    }

    #[test]
    fn session_ids_are_random_and_round_trip() {
        let a = SessionId::generate().expect("valid");
        let b = SessionId::generate().expect("valid");
        assert_ne!(a, b);
        assert_eq!(a.to_string().len(), 32);
        assert_eq!(SessionId::parse(&a.to_string()), Some(a));
        assert_eq!(SessionId::parse(&a.to_string().to_uppercase()), Some(a));
        assert_eq!(SessionId::from_path(&a.path()), Some(a));
        assert_eq!(SessionId::parse("00"), None);
        assert_eq!(SessionId::parse(&"zz".repeat(16)), None);
        assert_eq!(SessionId::parse(&"é".repeat(16)), None);
        // Hex digits only: from_str_radix would take a sign.
        assert_eq!(SessionId::parse(&"+0".repeat(16)), None);
        assert_eq!(SessionId::parse(&"-0".repeat(16)), None);
        assert_eq!(SessionId::parse(&" 0".repeat(16)), None);
        let s = SessionId::generate_seeded([1; 32]).expect("valid");
        assert_eq!(s, SessionId::generate_seeded([1; 32]).expect("valid"));
    }

    #[test]
    fn location_is_absolute() {
        let id = SessionId::parse("00112233445566778899aabbccddeeff").expect("valid");
        let base = PublicUrl::parse("https://ingest.example/").expect("valid");
        assert_eq!(
            base.location(&id),
            "https://ingest.example/whip/session/00112233445566778899aabbccddeeff"
        );
        let prefixed = PublicUrl::parse("http://192.0.2.1:8080/media").expect("valid");
        assert!(
            prefixed
                .location(&id)
                .starts_with("http://192.0.2.1:8080/media/whip/session/")
        );
        for bad in [
            "/whip",
            "ftp://x",
            "https://",
            "https://a b",
            "https://x?q",
            "https://u@x",
        ] {
            assert_eq!(PublicUrl::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn statuses() {
        let all = [
            (Refusal::UnsupportedMediaType, 415),
            (Refusal::Malformed(ParseError::Line), 400),
            (Refusal::Unauthorised, 401),
            (Refusal::Forbidden, 403),
            (Refusal::Conflict, 409),
            (Refusal::OutsideProfile(ProfileError::NoH264), 422),
            (Refusal::Throttled, 429),
            (Refusal::Unavailable, 503),
        ];
        for (r, status) in all {
            assert_eq!(r.status(), status);
            assert!(!r.reason().is_empty());
        }
        let from: Refusal = crate::sdp::Refusal::Profile(ProfileError::NoOpus).into();
        assert_eq!(from.status(), 422);
        assert!(is_sdp_content_type("application/sdp"));
        assert!(is_sdp_content_type("Application/SDP; charset=utf-8"));
        assert!(!is_sdp_content_type("text/plain"));
        assert!(!is_sdp_content_type(""));
    }

    #[test]
    fn stream_paths() {
        assert_eq!(stream_path("/whip/live/abc"), Some("live/abc"));
        assert_eq!(stream_path("/whip/"), None);
        assert_eq!(stream_path("/whip/session/00"), None);
        assert_eq!(stream_path("/other"), None);
    }
}
