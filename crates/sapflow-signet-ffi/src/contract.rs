//! Fixed creator-login proof contract. Remote signing is never account admission.
use crate::SignetError;
use nostr::prelude::*;
use serde::{Deserialize, Serialize};
use signet_nip46_client::PersistedSession;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

pub(crate) const MAX_BYTES: usize = 16_384;
const MAX_SAFE: u64 = 9_007_199_254_740_991;

pub(crate) fn hex(value: &str, length: usize) -> Result<(), SignetError> {
    if value.len() != length
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(SignetError::InvalidInput);
    }
    Ok(())
}

pub(crate) fn bunker_uri(value: &str, restoring: bool) -> Result<(), SignetError> {
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return Err(SignetError::InvalidInput);
    }
    match NostrConnectUri::parse(value).map_err(|_| SignetError::InvalidInput)? {
        NostrConnectUri::Bunker { relays, secret, .. } => {
            if relays.is_empty() || relays.len() > 4 || (restoring && secret.is_some()) {
                return Err(SignetError::InvalidInput);
            }
            for relay in relays {
                let uri = Url::parse(relay.as_str()).map_err(|_| SignetError::InvalidInput)?;
                if uri.scheme() != "wss"
                    || !uri.username().is_empty()
                    || uri.password().is_some()
                    || uri.fragment().is_some()
                    || uri.as_str().len() > 1024
                {
                    return Err(SignetError::InvalidInput);
                }
            }
            Ok(())
        }
        // The maintained pair() generates its own app key. A supplied client
        // URI cannot name that key and must never masquerade as paired.
        _ => Err(SignetError::InvalidInput),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialWire {
    version: u8,
    persona: String,
    app_secret_key: String,
    bunker_uri: String,
}

pub(crate) fn encode_credential(
    session: PersistedSession,
    persona: &str,
) -> Result<Zeroizing<Vec<u8>>, SignetError> {
    hex(persona, 64)?;
    hex(&session.app_secret_key, 64)?;
    bunker_uri(&session.bunker_uri, true)?;
    let mut wire = CredentialWire {
        version: 1,
        persona: persona.to_owned(),
        app_secret_key: session.app_secret_key,
        bunker_uri: session.bunker_uri,
    };
    let bytes = serde_json::to_vec(&wire).map_err(|_| SignetError::InvalidCredential);
    use zeroize::Zeroize;
    wire.app_secret_key.zeroize();
    wire.bunker_uri.zeroize();
    let bytes = Zeroizing::new(bytes?);
    if bytes.len() > MAX_BYTES {
        return Err(SignetError::InvalidCredential);
    }
    Ok(bytes)
}

pub(crate) fn decode_credential(bytes: &[u8]) -> Result<(PersistedSession, String), SignetError> {
    if bytes.len() > MAX_BYTES {
        return Err(SignetError::InvalidCredential);
    }
    let wire: CredentialWire =
        serde_json::from_slice(bytes).map_err(|_| SignetError::InvalidCredential)?;
    if wire.version != 1 {
        return Err(SignetError::InvalidCredential);
    }
    hex(&wire.persona, 64).map_err(|_| SignetError::InvalidCredential)?;
    hex(&wire.app_secret_key, 64).map_err(|_| SignetError::InvalidCredential)?;
    PublicKey::from_hex(&wire.persona).map_err(|_| SignetError::InvalidCredential)?;
    SecretKey::from_hex(&wire.app_secret_key).map_err(|_| SignetError::InvalidCredential)?;
    bunker_uri(&wire.bunker_uri, true).map_err(|_| SignetError::InvalidCredential)?;
    Ok((
        PersistedSession {
            app_secret_key: wire.app_secret_key,
            bunker_uri: wire.bunker_uri,
        },
        wire.persona,
    ))
}

pub(crate) struct LoginTemplate {
    pub unsigned: UnsignedEvent,
}
impl LoginTemplate {
    pub fn new(
        persona: PublicKey,
        challenge: &str,
        origin: &str,
        app: &str,
        created_at: u64,
    ) -> Result<Self, SignetError> {
        hex(challenge, 64)?;
        // Cheap scalar bounds precede maintained URL parsing. The FFI may be
        // called independently of the strict Swift profile parser.
        if origin.len() > 512
            || app.is_empty()
            || app.len() > 64
            || origin.chars().any(|c| c.is_control() || c.is_whitespace())
            || app.chars().any(char::is_control)
            || created_at == 0
            || created_at > MAX_SAFE
        {
            return Err(SignetError::InvalidInput);
        }
        let url = Url::parse(origin).map_err(|_| SignetError::InvalidInput)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !["", "/"].contains(&url.path())
        {
            return Err(SignetError::InvalidInput);
        }
        let tags = [["challenge", challenge], ["origin", origin], ["app", app]]
            .into_iter()
            .map(Tag::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SignetError::InvalidInput)?;
        Ok(Self {
            unsigned: EventBuilder::new(Kind::Custom(21236), "")
                .tags(tags)
                .custom_created_at(Timestamp::from(created_at))
                .finalize_unsigned(persona),
        })
    }
    pub fn fresh(&self) -> Result<(), SignetError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SignetError::Unavailable)?
            .as_secs();
        if self.unsigned.created_at.as_secs().abs_diff(now) > 300 {
            return Err(SignetError::InvalidInput);
        }
        Ok(())
    }
    pub fn verify(&self, signed: &Event) -> Result<Vec<u8>, SignetError> {
        if signed.pubkey != self.unsigned.pubkey
            || signed.kind != self.unsigned.kind
            || signed.content != self.unsigned.content
            || signed.tags != self.unsigned.tags
            || signed.created_at != self.unsigned.created_at
            || signed.verify().is_err()
        {
            return Err(SignetError::InvalidProof);
        }
        let bytes = signed.as_json().into_bytes();
        if bytes.len() > MAX_BYTES {
            return Err(SignetError::InvalidProof);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys() -> Keys {
        Keys::new(SecretKey::from_slice(&[1; 32]).expect("generated test key"))
    }
    fn template() -> LoginTemplate {
        LoginTemplate::new(
            keys().public_key(),
            &"a".repeat(64),
            "https://api.example.invalid",
            "Generated test",
            1_700_000_000,
        )
        .expect("template")
    }
    #[test]
    fn fixed_proof_verifies_id_signature_and_every_requested_field() {
        let t = template();
        let valid = t
            .unsigned
            .clone()
            .finalize(&keys())
            .expect("signed generated proof");
        assert!(t.verify(&valid).is_ok());
        let mut changed = valid.clone();
        changed.content = "changed".into();
        assert!(t.verify(&changed).is_err());
        let other = EventBuilder::new(Kind::Custom(21236), "")
            .custom_created_at(valid.created_at)
            .finalize(&keys())
            .expect("other");
        assert!(other.verify().is_ok());
        assert!(t.verify(&other).is_err());
        let different_persona = Keys::generate();
        let other = t.unsigned.clone().finalize(&different_persona);
        assert!(
            other.is_err()
                || t.verify(&other.expect("checked successful alternative"))
                    .is_err()
        );
    }
    #[test]
    fn credential_is_strict_and_strips_single_use_authority() {
        let uri = format!(
            "bunker://{}?relay=wss://relay.example.invalid",
            keys().public_key().to_hex()
        );
        let original = PersistedSession {
            app_secret_key: keys().secret_key().to_secret_hex(),
            bunker_uri: uri.clone(),
        };
        let bytes = encode_credential(original, &keys().public_key().to_hex()).expect("encoded");
        let (restored, persona) = decode_credential(&bytes).expect("decoded");
        assert_eq!(restored.bunker_uri, uri);
        assert_eq!(persona, keys().public_key().to_hex());
        for key in ["0".repeat(64), "f".repeat(64)] {
            let mut bad: serde_json::Value = serde_json::from_slice(&bytes).expect("fixture");
            bad["app_secret_key"] = key.into();
            assert!(decode_credential(&serde_json::to_vec(&bad).expect("fixture JSON")).is_err());
        }
        assert!(bunker_uri(&(uri.clone() + "&secret=one-use"), true).is_err());
        assert!(bunker_uri(&uri.replace("wss:", "ws:"), false).is_err());
        assert!(decode_credential(br#"{"version":1,"version":1}"#).is_err());
        assert!(decode_credential(&vec![0; MAX_BYTES + 1]).is_err());
    }
    #[test]
    fn oversized_origin_and_app_are_refused_at_scalar_admission() {
        let origin = format!("https://{}.invalid", "a".repeat(1_048_576));
        assert!(matches!(
            LoginTemplate::new(keys().public_key(), &"a".repeat(64), &origin, "Test", 1),
            Err(SignetError::InvalidInput)
        ));
        assert!(matches!(
            LoginTemplate::new(
                keys().public_key(),
                &"a".repeat(64),
                "https://api.example.invalid",
                &"a".repeat(65),
                1
            ),
            Err(SignetError::InvalidInput)
        ));
    }
    #[test]
    fn invalid_templates_and_stale_proofs_do_not_become_signing_requests() {
        assert!(
            LoginTemplate::new(
                keys().public_key(),
                &"a".repeat(64),
                "http://api.example.invalid",
                "Test",
                1
            )
            .is_err()
        );
        assert!(
            LoginTemplate::new(
                keys().public_key(),
                &"a".repeat(64),
                "https://api.example.invalid/path",
                "Test",
                1
            )
            .is_err()
        );
        assert!(
            LoginTemplate::new(
                keys().public_key(),
                &("a".repeat(64) + "\n"),
                "https://api.example.invalid",
                "Test",
                1
            )
            .is_err()
        );
        assert!(template().fresh().is_err());
    }
}
