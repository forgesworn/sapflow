//! Strict, bounded control-plane messages. Never serialise a grant as a response.

use std::fmt;

use sapflow_core::StreamPath;
use sapflow_core::publish_session::{
    EpochMs, Fingerprint, GrantMetadata, JsPositive, Provision, Revoke, SessionId, SessionKey,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const BODY_LIMIT: usize = 16 * 1024;
pub const MAX_TTL_MS: u64 = 15 * 60 * 1000;
pub const MAX_QUEUE_BYTES: u64 = 4 * 1024 * 1024;

/// Deserialisation transfers the owned value straight into zeroising storage.
/// HTTP buffers and the caller's original copies have separate ownership.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn matches(&self, presented: &str) -> bool {
        let a = self.expose().as_bytes();
        let b = presented.as_bytes();
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .fold(0u8, |different, (a, b)| different | (a ^ b))
                == 0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}
impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose())
    }
}
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SrtEncryption {
    pub cipher: String,
    pub passphrase: Secret,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limits {
    pub max_publish_bps: u64,
    pub max_queue_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutSession {
    pub contract_version: u32,
    pub event_id: String,
    pub event_generation: u64,
    pub session_id: String,
    pub revision: u64,
    pub idempotency_key: String,
    pub expires_at_unix_ms: u64,
    pub reconnect_grace_ms: u32,
    pub transport: String,
    pub stream_path: String,
    pub publish_user: String,
    pub publish_password: Secret,
    pub srt: SrtEncryption,
    pub limits: Limits,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeSession {
    pub contract_version: u32,
    pub event_id: String,
    pub event_generation: u64,
    pub session_id: String,
    pub revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    Body,
    Identity,
    Version,
    Expiry,
    Transport,
    Credentials,
    Limits,
}

fn key(
    route_id: &str,
    version: u32,
    event_id: &str,
    generation: u64,
    session_id: &str,
    revision: u64,
    idempotency: &str,
) -> Result<(SessionKey, JsPositive), Invalid> {
    if version != 1 {
        return Err(Invalid::Version);
    }
    let path = StreamPath::parse(&format!("live/{event_id}")).ok_or(Invalid::Identity)?;
    let session_id = SessionId::parse(session_id).map_err(|_| Invalid::Identity)?;
    if session_id.as_str() != route_id || SessionId::parse(idempotency).is_err() {
        return Err(Invalid::Identity);
    }
    Ok((
        SessionKey {
            path,
            event_generation: JsPositive::new(generation).map_err(|_| Invalid::Identity)?,
            session_id,
        },
        JsPositive::new(revision).map_err(|_| Invalid::Identity)?,
    ))
}

fn token(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn fingerprint(kind: &str, value: &impl Serialize) -> Result<Fingerprint, Invalid> {
    let bytes = Zeroizing::new(serde_json::to_vec(value).map_err(|_| Invalid::Body)?);
    let mut hash = Sha256::new();
    hash.update(kind.as_bytes());
    hash.update([0]);
    hash.update(&*bytes);
    Ok(Fingerprint::new(hash.finalize().into()))
}

impl PutSession {
    pub fn parse(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > BODY_LIMIT {
            return Err(Invalid::Body);
        }
        serde_json::from_slice(bytes).map_err(|_| Invalid::Body)
    }

    pub fn validated(
        &self,
        route_id: &str,
        now_ms: u64,
        server_max_bps: u64,
    ) -> Result<Provision, Invalid> {
        let (key, revision) = key(
            route_id,
            self.contract_version,
            &self.event_id,
            self.event_generation,
            &self.session_id,
            self.revision,
            &self.idempotency_key,
        )?;
        if self.stream_path != key.path.as_path() {
            return Err(Invalid::Identity);
        }
        if self.transport != "srt" || self.srt.cipher != "aes256-ctr" {
            return Err(Invalid::Transport);
        }
        if self.publish_user != key.session_id.as_str()
            || !token(self.publish_password.expose())
            || !token(self.srt.passphrase.expose())
            || self.publish_password.matches(self.srt.passphrase.expose())
        {
            return Err(Invalid::Credentials);
        }
        if self.expires_at_unix_ms <= now_ms || self.expires_at_unix_ms - now_ms > MAX_TTL_MS {
            return Err(Invalid::Expiry);
        }
        let expires_at_ms = EpochMs::new(self.expires_at_unix_ms).map_err(|_| Invalid::Expiry)?;
        if self.reconnect_grace_ms > 3_600_000
            || self.limits.max_publish_bps == 0
            || self.limits.max_publish_bps > server_max_bps
            || !(1500..=MAX_QUEUE_BYTES).contains(&self.limits.max_queue_bytes)
        {
            return Err(Invalid::Limits);
        }
        #[derive(Serialize)]
        struct Immutable<'a> {
            version: u32,
            event_id: &'a str,
            generation: u64,
            session_id: &'a str,
            grace_ms: u32,
            transport: &'a str,
            path: &'a str,
            user: &'a str,
            password: &'a Secret,
            srt: &'a SrtEncryption,
            limits: Limits,
        }
        let immutable = Immutable {
            version: 1,
            event_id: &self.event_id,
            generation: self.event_generation,
            session_id: &self.session_id,
            grace_ms: self.reconnect_grace_ms,
            transport: &self.transport,
            path: &self.stream_path,
            user: &self.publish_user,
            password: &self.publish_password,
            srt: &self.srt,
            limits: self.limits,
        };
        Ok(Provision {
            key,
            revision,
            grant: GrantMetadata {
                expires_at_ms,
                reconnect_grace_ms: self.reconnect_grace_ms,
                immutable_fingerprint: fingerprint("immutable-v1", &immutable)?,
            },
            request_fingerprint: fingerprint("put-v1", self)?,
        })
    }
}

impl RevokeSession {
    pub fn parse(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() > BODY_LIMIT {
            return Err(Invalid::Body);
        }
        serde_json::from_slice(bytes).map_err(|_| Invalid::Body)
    }
    pub fn validated(&self, route_id: &str) -> Result<Revoke, Invalid> {
        let (key, revision) = key(
            route_id,
            self.contract_version,
            &self.event_id,
            self.event_generation,
            &self.session_id,
            self.revision,
            &self.idempotency_key,
        )?;
        Ok(Revoke {
            key,
            revision,
            request_fingerprint: fingerprint("revoke-v1", self)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SID: &str = "0123456789abcdef0123456789abcdef";
    fn body() -> serde_json::Value {
        serde_json::json!({"contractVersion":1,"eventId":"event-test","eventGeneration":1,"sessionId":SID,"revision":1,"idempotencyKey":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","expiresAtUnixMs":100001,"reconnectGraceMs":90000,"transport":"srt","streamPath":"live/event-test","publishUser":SID,"publishPassword":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","srt":{"cipher":"aes256-ctr","passphrase":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"limits":{"maxPublishBps":4000000,"maxQueueBytes":1048576}})
    }
    fn parsed(value: &serde_json::Value) -> PutSession {
        PutSession::parse(&serde_json::to_vec(value).expect("json")).expect("wire")
    }
    #[test]
    fn binds_identity_protocol_expiry_limits_and_separate_credentials() {
        assert!(parsed(&body()).validated(SID, 1, 40_000_000).is_ok());
        for (field, replacement, expected) in [
            ("contractVersion", serde_json::json!(2), Invalid::Version),
            (
                "eventGeneration",
                serde_json::json!(9_007_199_254_740_992u64),
                Invalid::Identity,
            ),
            (
                "publishUser",
                serde_json::json!("other"),
                Invalid::Credentials,
            ),
            ("transport", serde_json::json!("rtmp"), Invalid::Transport),
            (
                "streamPath",
                serde_json::json!("live/event-other"),
                Invalid::Identity,
            ),
            ("expiresAtUnixMs", serde_json::json!(1), Invalid::Expiry),
            (
                "expiresAtUnixMs",
                serde_json::json!(MAX_TTL_MS + 2),
                Invalid::Expiry,
            ),
        ] {
            let mut value = body();
            value[field] = replacement;
            assert_eq!(
                parsed(&value)
                    .validated(SID, 1, 40_000_000)
                    .expect_err("invalid wire must be rejected"),
                expected
            );
        }
        let mut value = body();
        value["srt"]["passphrase"] = value["publishPassword"].clone();
        assert_eq!(
            parsed(&value)
                .validated(SID, 1, 40_000_000)
                .expect_err("invalid wire must be rejected"),
            Invalid::Credentials
        );
        for replacement in [0, 40_000_001] {
            let mut value = body();
            value["limits"]["maxPublishBps"] = serde_json::json!(replacement);
            assert_eq!(
                parsed(&value)
                    .validated(SID, 1, 40_000_000)
                    .expect_err("invalid wire must be rejected"),
                Invalid::Limits
            );
        }
    }
    #[test]
    fn rejects_duplicate_unknown_and_oversize_json_and_redacts_secrets() {
        let value = body();
        let encoded = serde_json::to_vec(&value).expect("json");
        let mut duplicate = String::from_utf8(encoded.clone()).expect("utf8");
        duplicate.insert_str(1, "\"revision\":2,");
        assert_eq!(
            PutSession::parse(duplicate.as_bytes()).expect_err("invalid wire must be rejected"),
            Invalid::Body
        );
        let mut unknown = value.clone();
        unknown["callbackToken"] = serde_json::json!("must-not-enter-this-contract");
        assert_eq!(
            PutSession::parse(&serde_json::to_vec(&unknown).expect("json"))
                .expect_err("invalid wire must be rejected"),
            Invalid::Body
        );
        assert_eq!(
            PutSession::parse(&vec![b' '; BODY_LIMIT + 1])
                .expect_err("invalid wire must be rejected"),
            Invalid::Body
        );
        let debug = format!("{:?}", parsed(&value));
        assert!(!debug.contains(&"a".repeat(43)));
        assert!(!debug.contains(&"b".repeat(43)));
        assert!(
            parsed(&value)
                .validated("ffffffffffffffffffffffffffffffff", 1, 40_000_000)
                .is_err()
        );
    }
    #[test]
    fn refresh_can_extend_expiry_but_cannot_change_the_immutable_scope_silently() {
        let original = parsed(&body())
            .validated(SID, 1, 40_000_000)
            .expect("valid");
        let mut value = body();
        value["revision"] = serde_json::json!(2);
        value["expiresAtUnixMs"] = serde_json::json!(200001);
        value["idempotencyKey"] = serde_json::json!("cccccccccccccccccccccccccccccccc");
        let refresh = parsed(&value)
            .validated(SID, 1, 40_000_000)
            .expect("valid refresh");
        assert_eq!(
            original.grant.immutable_fingerprint,
            refresh.grant.immutable_fingerprint
        );
        assert_ne!(original.request_fingerprint, refresh.request_fingerprint);
        value["reconnectGraceMs"] = serde_json::json!(0);
        assert_ne!(
            original.grant.immutable_fingerprint,
            parsed(&value)
                .validated(SID, 1, 40_000_000)
                .expect("policy change")
                .grant
                .immutable_fingerprint
        );
    }
}
