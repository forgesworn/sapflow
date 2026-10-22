//! Types and rules shared by the sapflow media server and its native clients:
//! stream paths, publish requests and publisher credentials, the stream-health
//! callback events, publisher limits, recording policy and file names, and the
//! prepared publish-session authority.
//!
//! Everything here is pure parsing and bookkeeping. The crate has no
//! dependencies beyond `std`, no networking, no async runtime and no
//! filesystem access, and callers pass the time in, so it builds for iOS and
//! Android as well as the server.

#![warn(missing_docs)]

pub mod limits;
pub mod publish_session;
pub mod recording;

/// The prefix every stage stream lives under, matching the v4v contract.
pub const STREAM_PREFIX: &str = "live/";

/// A validated stream path such as `live/event-abc123`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamPath {
    event_id: String,
}

impl StreamPath {
    /// Parses `live/<eventId>`, where the event id is `event-` followed by
    /// lowercase letters, digits and hyphens (v4v's `^live/event-[a-z0-9-]+$`).
    /// The event id, `event-` included, is at most 128 bytes.
    pub fn parse(path: &str) -> Option<Self> {
        let event_id = path.strip_prefix(STREAM_PREFIX)?;
        let rest = event_id.strip_prefix("event-")?;
        if rest.is_empty() || event_id.len() > 128 {
            return None;
        }
        if !rest
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return None;
        }
        Some(Self {
            event_id: event_id.to_owned(),
        })
    }

    /// The event id including its `event-` prefix, such as `event-abc-123`.
    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    /// The full path, `live/<eventId>`.
    pub fn as_path(&self) -> String {
        format!("{STREAM_PREFIX}{}", self.event_id)
    }
}

/// Why a publish request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    /// Not a publish request, or not in a form we understand.
    BadRequest,
    /// The path is not a stage path (`live/event-...`).
    BadPath,
    /// Missing or wrong publisher credentials.
    Unauthorized,
}

/// Company-namespaced SRT access-control correlation attribute (Appendix B.2).
pub const SRT_ATTEMPT_ATTRIBUTE: &str = "forgesworn_attempt";

/// Public correlation metadata, not a credential or replay/authentication token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionAttemptId(String);
impl ConnectionAttemptId {
    /// Accepts exactly 32 lowercase hexadecimal characters.
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| Self(value.to_owned()))
    }
    /// The 32 lowercase hexadecimal characters.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A request to publish, as carried in an SRT stream id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRequest {
    /// The stream to publish to.
    pub path: StreamPath,
    /// The publisher user name from the request, if it carried one.
    pub user: Option<String>,
    /// The publisher password from the request, if it carried one.
    pub pass: Option<String>,
    /// The caller's correlation id, from the `forgesworn_attempt` attribute.
    /// Only the SRT access-control form can carry one; every other form leaves
    /// it `None`.
    pub connection_attempt_id: Option<ConnectionAttemptId>,
}

impl PublishRequest {
    /// Parses an SRT stream id in either form mediamtx accepts:
    ///
    /// - `publish:live/<eventId>` or `publish:live/<eventId>:<user>:<pass>`
    /// - the SRT access-control form `#!::m=publish,r=live/<eventId>,u=<user>,s=<pass>`
    ///
    /// Read requests are refused: sapflow does not serve SRT playback.
    pub fn parse_srt_stream_id(stream_id: &str) -> Result<Self, PublishError> {
        if stream_id.len() > 512 {
            return Err(PublishError::BadRequest);
        }
        if let Some(rest) = stream_id.strip_prefix("#!::") {
            return Self::parse_access_control(rest);
        }
        let rest = stream_id
            .strip_prefix("publish:")
            .ok_or(PublishError::BadRequest)?;
        let mut parts = rest.split(':');
        let path = parts.next().ok_or(PublishError::BadRequest)?;
        let user = parts.next().map(str::to_owned);
        let pass = parts.next().map(str::to_owned);
        if parts.next().is_some() || (user.is_some() != pass.is_some()) {
            return Err(PublishError::BadRequest);
        }
        let path = StreamPath::parse(path).ok_or(PublishError::BadPath)?;
        Ok(Self {
            path,
            user,
            pass,
            connection_attempt_id: None,
        })
    }

    /// Parses an RTMP publish: the app from the server URL (`live`) and the
    /// stream key as v4v prints it (`<eventId>?user=<user>&pass=<pass>`).
    pub fn parse_rtmp(app: &str, stream_key: &str) -> Result<Self, PublishError> {
        if app.len() + stream_key.len() > 512 {
            return Err(PublishError::BadRequest);
        }
        let app = app.trim_matches('/');
        let (key, query) = stream_key.split_once('?').unwrap_or((stream_key, ""));
        let path = StreamPath::parse(&format!("{app}/{key}")).ok_or(PublishError::BadPath)?;
        let (mut user, mut pass) = (None, None);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = percent_decode(v).ok_or(PublishError::BadRequest)?;
            match k {
                "user" => user = Some(v),
                "pass" => pass = Some(v),
                _ => {}
            }
        }
        Ok(Self {
            path,
            user,
            pass,
            connection_attempt_id: None,
        })
    }

    fn parse_access_control(rest: &str) -> Result<Self, PublishError> {
        let (mut mode, mut path, mut user, mut pass) = (None, None, None, None);
        let mut connection_attempt_id = None;
        for pair in rest.split(',') {
            let (k, v) = pair.split_once('=').ok_or(PublishError::BadRequest)?;
            match k {
                "m" => mode = Some(v),
                "r" => path = Some(v),
                "u" => user = Some(v.to_owned()),
                "s" => pass = Some(v.to_owned()),
                SRT_ATTEMPT_ATTRIBUTE => {
                    if connection_attempt_id.is_some() {
                        return Err(PublishError::BadRequest);
                    }
                    connection_attempt_id =
                        Some(ConnectionAttemptId::parse(v).ok_or(PublishError::BadRequest)?);
                }
                _ if k.trim().eq_ignore_ascii_case(SRT_ATTEMPT_ATTRIBUTE) => {
                    return Err(PublishError::BadRequest);
                }
                _ => {}
            }
        }
        if mode != Some("publish") {
            return Err(PublishError::BadRequest);
        }
        let path = path.ok_or(PublishError::BadRequest)?;
        let path = StreamPath::parse(path).ok_or(PublishError::BadPath)?;
        Ok(Self {
            path,
            user,
            pass,
            connection_attempt_id,
        })
    }

    /// Checks the request's credentials against the configured publisher.
    pub fn authorise(&self, publisher: &Publisher) -> Result<(), PublishError> {
        publisher.check(self.user.as_deref(), self.pass.as_deref())
    }
}

/// The one publisher allowed to push to stage paths.
#[derive(Clone)]
pub struct Publisher {
    user: String,
    pass: String,
}

impl std::fmt::Debug for Publisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Publisher")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl Publisher {
    /// Creates a publisher that accepts exactly this user name and password.
    pub fn new(user: impl Into<String>, pass: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            pass: pass.into(),
        }
    }

    /// Compares both fields in constant time for equal-length inputs.
    pub fn check(&self, user: Option<&str>, pass: Option<&str>) -> Result<(), PublishError> {
        let (Some(user), Some(pass)) = (user, pass) else {
            return Err(PublishError::Unauthorized);
        };
        let ok_user = ct_eq(user.as_bytes(), self.user.as_bytes());
        let ok_pass = ct_eq(pass.as_bytes(), self.pass.as_bytes());
        if ok_user & ok_pass {
            Ok(())
        } else {
            Err(PublishError::Unauthorized)
        }
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// What the server reports to v4v's stream-health callback.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// The encoder is connected and HLS is ready to fetch.
    Publish,
    /// The encoder went away.
    Unpublish,
    /// A recording finished; `segment_path` is as the media server sees it.
    RecordSegmentComplete {
        /// Where the finished recording file is, sent as `segmentPath`.
        segment_path: String,
        /// Length of the recording in seconds, sent as the string
        /// `segmentDuration`.
        duration_secs: f64,
    },
    /// Measured from the encoder's media, every so often while it publishes.
    /// v4v reads `bitrateKbps`; the rest is for Studio's encoder check.
    Health(Health),
}

/// What the stream-health callback reports about a live publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    /// Media received, audio and video, over the recent segments.
    pub bitrate_kbps: u64,
    /// The typical segment length, which is the encoder's keyframe interval
    /// once that is at least the segment target (1 s).
    pub keyframe_interval_ms: u64,
    /// Whether the stream carries audio.
    pub audio: bool,
    /// Breaks in the media (stalls, reconnects) since the publisher connected.
    pub breaks: u64,
}

impl StreamEvent {
    /// The `event` field v4v expects.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Unpublish => "unpublish",
            Self::RecordSegmentComplete { .. } => "recordSegmentComplete",
            Self::Health(_) => "health",
        }
    }

    /// The JSON body of the stream-health callback.
    pub fn callback_body(&self, path: &StreamPath) -> String {
        let mut s = format!(
            "{{\"event\":\"{}\",\"path\":\"{}\"",
            self.name(),
            json_escape(&path.as_path())
        );
        match self {
            Self::RecordSegmentComplete {
                segment_path,
                duration_secs,
            } => s.push_str(&format!(
                ",\"segmentPath\":\"{}\",\"segmentDuration\":\"{}\"",
                json_escape(segment_path),
                duration_secs
            )),
            Self::Health(h) => s.push_str(&format!(
                ",\"health\":{{\"bitrateKbps\":{},\"keyframeIntervalMs\":{},\"audio\":{},\"breaks\":{}}}",
                h.bitrate_kbps, h.keyframe_interval_ms, h.audio, h.breaks
            )),
            Self::Publish | Self::Unpublish => {}
        }
        s.push('}');
        s
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Fills a callback URL template: `{eventId}` and `{path}` are substituted;
/// the secret replaces `{token}`, or is appended as `token=` if the template
/// has no placeholder for it.
pub fn callback_url(template: &str, path: &StreamPath, secret: Option<&str>) -> String {
    let mut url = template
        .replace("{eventId}", path.event_id())
        .replace("{path}", &path.as_path());
    if let Some(secret) = secret {
        if url.contains("{token}") {
            url = url.replace("{token}", &percent_encode(secret));
        } else {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str("token=");
            url.push_str(&percent_encode(secret));
        }
    }
    url
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_v4v_event_paths() {
        let p = StreamPath::parse("live/event-abc-123").expect("valid");
        assert_eq!(p.event_id(), "event-abc-123");
        assert_eq!(p.as_path(), "live/event-abc-123");
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "",
            "live/",
            "live/event-",
            "event-abc",
            "live/event-ABC",
            "live/event-a/b",
            "live/event-a.b",
            "live/../x",
        ] {
            assert!(StreamPath::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn parses_mediamtx_publish_stream_ids() {
        let r = PublishRequest::parse_srt_stream_id("publish:live/event-a1").expect("valid");
        assert_eq!(r.path.event_id(), "event-a1");
        assert_eq!((r.user, r.pass), (None, None));

        let r =
            PublishRequest::parse_srt_stream_id("publish:live/event-a1:pub:secret").expect("valid");
        assert_eq!(r.user.as_deref(), Some("pub"));
        assert_eq!(r.pass.as_deref(), Some("secret"));

        let r = PublishRequest::parse_srt_stream_id("#!::r=live/event-a1,m=publish,u=pub,s=secret")
            .expect("valid");
        assert_eq!(r.path.event_id(), "event-a1");
        assert_eq!(r.pass.as_deref(), Some("secret"));
    }

    #[test]
    fn parses_rtmp_publishes_as_v4v_prints_them() {
        let r =
            PublishRequest::parse_rtmp("live", "event-a1?user=pub&pass=s%26cret").expect("valid");
        assert_eq!(r.path.as_path(), "live/event-a1");
        assert_eq!(r.user.as_deref(), Some("pub"));
        assert_eq!(r.pass.as_deref(), Some("s&cret"));
        let r = PublishRequest::parse_rtmp("/live/", "event-a1").expect("valid");
        assert_eq!(r.user, None);
        assert_eq!(
            PublishRequest::parse_rtmp("other", "event-a1"),
            Err(PublishError::BadPath)
        );
        assert_eq!(
            PublishRequest::parse_rtmp("live", "x/../y"),
            Err(PublishError::BadPath)
        );
        assert_eq!(
            PublishRequest::parse_rtmp("live", "event-a1?pass=%zz"),
            Err(PublishError::BadRequest)
        );
        assert_eq!(
            PublishRequest::parse_rtmp("live", "event-a1?pass=%e2%82"),
            Err(PublishError::BadRequest)
        );
    }

    #[test]
    fn refuses_reads_and_bad_stream_ids() {
        use PublishError::*;
        for (id, want) in [
            ("read:live/event-a1", BadRequest),
            ("live/event-a1", BadRequest),
            ("publish:live/event-a1:onlyuser", BadRequest),
            ("publish:live/event-a1:u:p:extra", BadRequest),
            ("publish:other/x", BadPath),
            ("publish:live/../etc", BadPath),
            ("#!::r=live/event-a1,m=request", BadRequest),
            ("#!::r=live/x,m=publish", BadPath),
        ] {
            assert_eq!(PublishRequest::parse_srt_stream_id(id), Err(want), "{id}");
        }
    }

    #[test]
    fn only_the_configured_publisher_is_authorised() {
        let publisher = Publisher::new("pub", "secret");
        let ok =
            PublishRequest::parse_srt_stream_id("publish:live/event-a:pub:secret").expect("valid");
        assert_eq!(ok.authorise(&publisher), Ok(()));
        for id in [
            "publish:live/event-a",
            "publish:live/event-a:pub:wrong",
            "publish:live/event-a:other:secret",
            "publish:live/event-a:pub:secret2",
        ] {
            let r = PublishRequest::parse_srt_stream_id(id).expect("parses");
            assert_eq!(
                r.authorise(&publisher),
                Err(PublishError::Unauthorized),
                "{id}"
            );
        }
        assert!(!format!("{publisher:?}").contains("secret"));
    }

    #[test]
    fn callback_bodies_match_the_contract() {
        let p = StreamPath::parse("live/event-a1").expect("valid");
        assert_eq!(
            StreamEvent::Publish.callback_body(&p),
            r#"{"event":"publish","path":"live/event-a1"}"#
        );
        assert_eq!(
            StreamEvent::Unpublish.callback_body(&p),
            r#"{"event":"unpublish","path":"live/event-a1"}"#
        );
        let rec = StreamEvent::RecordSegmentComplete {
            segment_path: "/recordings/live/event-a1/1792500000.mp4".into(),
            duration_secs: 3723.5,
        };
        assert_eq!(
            rec.callback_body(&p),
            r#"{"event":"recordSegmentComplete","path":"live/event-a1","segmentPath":"/recordings/live/event-a1/1792500000.mp4","segmentDuration":"3723.5"}"#
        );
        let odd = StreamEvent::RecordSegmentComplete {
            segment_path: "a\"b\\c\n".into(),
            duration_secs: 1.0,
        };
        assert!(
            odd.callback_body(&p)
                .contains(r#""segmentPath":"a\"b\\c\u000a""#)
        );
        let health = StreamEvent::Health(Health {
            bitrate_kbps: 2620,
            keyframe_interval_ms: 1000,
            audio: true,
            breaks: 2,
        });
        // v4v reads `health.bitrateKbps` and leaves the stage's status as it
        // is for an event name it does not know.
        assert_eq!(
            health.callback_body(&p),
            r#"{"event":"health","path":"live/event-a1","health":{"bitrateKbps":2620,"keyframeIntervalMs":1000,"audio":true,"breaks":2}}"#
        );
    }

    #[test]
    fn callback_urls_carry_the_event_and_secret() {
        let p = StreamPath::parse("live/event-a1").expect("valid");
        let t = "http://app:5181/api/public/live-events/{eventId}/stream-health";
        assert_eq!(
            callback_url(t, &p, Some("s3cret")),
            "http://app:5181/api/public/live-events/event-a1/stream-health?token=s3cret"
        );
        assert_eq!(
            callback_url("http://h/x?a=1&token={token}", &p, Some("a b&c")),
            "http://h/x?a=1&token=a%20b%26c"
        );
        assert_eq!(
            callback_url("http://h/{path}", &p, None),
            "http://h/live/event-a1"
        );
    }

    #[test]
    fn event_names_match_the_callback_contract() {
        assert_eq!(StreamEvent::Publish.name(), "publish");
        assert_eq!(StreamEvent::Unpublish.name(), "unpublish");
        let rec = StreamEvent::RecordSegmentComplete {
            segment_path: "/r/x.mp4".into(),
            duration_secs: 1.0,
        };
        assert_eq!(rec.name(), "recordSegmentComplete");
    }
}

#[cfg(test)]
mod publish_attempt_tests;
