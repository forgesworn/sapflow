//! Generated hostile payloads exercise the security-fixed dependency path.
//! No relay, account or user key is used.
use bitcoin_hashes::{hmac::HmacEngine, sha256, Hash, HashEngine};
use nostr::nips::nip44::v2::{decrypt_to_bytes, ConversationKey};

fn mac(key: &[u8], pieces: &[&[u8]]) -> [u8; 32] {
    let mut engine = HmacEngine::<sha256::HashEngine>::new(key);
    for piece in pieces {
        engine.input(piece);
    }
    engine.finalize().to_byte_array()
}

#[test]
fn authenticated_zero_and_one_byte_ciphertexts_reject_without_panic() {
    // Independent RFC5869 expand using public hash APIs, not the private NIP44
    // message-key helper. A legitimate peer can authenticate these short frames.
    let key = ConversationKey::new([0x19; 32]);
    let nonce = [0x42; 32];
    let mut expanded = Vec::new();
    let mut previous = Vec::new();
    for counter in 1..=3_u8 {
        previous = mac(key.as_bytes(), &[&previous, &nonce, &[counter]]).to_vec();
        expanded.extend_from_slice(&previous);
    }
    for count in [0, 1] {
        let ciphertext = vec![0_u8; count];
        let tag = mac(&expanded[44..76], &[&nonce, &ciphertext]);
        let mut payload = vec![2];
        payload.extend_from_slice(&nonce);
        payload.extend_from_slice(&ciphertext);
        payload.extend_from_slice(&tag);
        assert_eq!(payload.len(), 65 + count);
        assert!(decrypt_to_bytes(&key, &payload).is_err());
    }
}
