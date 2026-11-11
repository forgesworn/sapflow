//! Session keys and the two packet transforms, applied in place.
//!
//! - Key derivation: RFC 3711 section 4.3 with a key derivation rate of 0,
//!   AES-128 counter mode as the PRF (section 4.3.3).
//! - `SRTP_AES128_CM_HMAC_SHA1_80`: AES-128 counter mode (section 4.1.1)
//!   and HMAC-SHA1 truncated to 80 bits (section 4.2.1).
//! - `SRTP_AEAD_AES_128_GCM`: RFC 7714 sections 8 and 9. The functions are
//!   generic over the AEAD so the RFC's AES-256 vectors can exercise the
//!   same IV, AAD and layout code; only AES-128 is offered as a profile.
//!
//! Each function checks lengths first and touches the buffer only after
//! that. Authentication is checked before decryption; on an error the
//! buffer's contents are unspecified and the packet should be dropped.

use super::SrtpError;
use aes::Aes128Enc;
use aes_gcm::aead::array::Array;
use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::{AeadInOut, Aes128Gcm, KeyInit};
use ctr::cipher::{InnerIvInit, StreamCipher};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// HMAC-SHA1-80 tag length.
pub(crate) const CM_TAG: usize = 10;
/// AES-GCM tag length (RFC 7714 section 10).
pub(crate) const GCM_TAG: usize = 16;
/// The E flag and SRTCP index word.
pub(crate) const ESRTCP: usize = 4;
/// Longest payload one counter-mode IV may cover: 2^16 blocks (RFC 3711
/// section 4.1.1).
const MAX_KEYSTREAM: usize = 1 << 20;

/// Labels for the session keys (RFC 3711 sections 4.3.1 and 4.3.2).
#[derive(Clone, Copy)]
pub(crate) struct Labels {
    encryption: u8,
    auth: u8,
    salt: u8,
}
pub(crate) const RTP_LABELS: Labels = Labels {
    encryption: 0,
    auth: 1,
    salt: 2,
};
pub(crate) const RTCP_LABELS: Labels = Labels {
    encryption: 3,
    auth: 4,
    salt: 5,
};

/// AES-128 counter-mode keystream XORed over `bytes`, starting at `iv`.
pub(crate) fn keystream(
    cipher: &Aes128Enc,
    iv: &[u8; 16],
    bytes: &mut [u8],
) -> Result<(), SrtpError> {
    if bytes.len() > MAX_KEYSTREAM {
        return Err(SrtpError::Malformed);
    }
    let core = ctr::CtrCore::<_, ctr::flavors::Ctr128BE>::inner_iv_init(cipher, &(*iv).into());
    ctr::Ctr128BE::<&Aes128Enc>::from_core(core)
        .try_apply_keystream(bytes)
        .map_err(|_| SrtpError::Malformed)
}

/// The PRF input block for `label` (RFC 3711 sections 4.3.1 and 4.3.3).
///
/// With a key derivation rate of 0, `r` is zero, so `key_id` is the label
/// followed by six zero octets, and `x = key_id XOR master_salt` with the
/// two right-aligned in 112 bits. The PRF input is `x * 2^16` in a 128-bit
/// block, so the salt starts at octet 0 and the label lands on octet 7.
///
/// A 96-bit master salt (the GCM profile) is first multiplied by 2^16 to
/// make the 112-bit salt the KDF expects: RFC 7714 section 11 as corrected
/// by erratum 4938 (verified 2023-11-08). So it occupies octets 0 to 11,
/// followed by zeros. No RFC test vector covers this end to end; the first
/// check against a real GCM peer is the DTLS stage (W2).
pub(crate) fn prf_input(master_salt: &[u8], label: u8) -> Result<[u8; 16], SrtpError> {
    let mut block = [0u8; 16];
    if master_salt.len() > 14 {
        return Err(SrtpError::Config);
    }
    block
        .get_mut(..master_salt.len())
        .ok_or(SrtpError::Config)?
        .copy_from_slice(master_salt);
    // The label is the seventh octet from the right of the 112-bit x.
    if let Some(b) = block.get_mut(7) {
        *b ^= label;
    }
    Ok(block)
}

/// Fills `out` with `PRF_n(k_master, x)` for `label`.
pub(crate) fn derive(
    master: &Aes128Enc,
    master_salt: &[u8],
    label: u8,
    out: &mut [u8],
) -> Result<(), SrtpError> {
    let iv = Zeroizing::new(prf_input(master_salt, label)?);
    out.fill(0);
    keystream(master, &iv, out)
}

/// Counter-mode and HMAC-SHA1 session keys for one direction of RTP or
/// RTCP.
pub(crate) struct CmKeys {
    pub(crate) cipher: Aes128Enc,
    salt: Zeroizing<[u8; 14]>,
    mac: Hmac<Sha1>,
}

impl CmKeys {
    pub(crate) fn derive(
        master: &Aes128Enc,
        master_salt: &[u8],
        labels: Labels,
    ) -> Result<Self, SrtpError> {
        let mut key = Zeroizing::new([0u8; 16]);
        let mut salt = Zeroizing::new([0u8; 14]);
        let mut auth = Zeroizing::new([0u8; 20]);
        derive(master, master_salt, labels.encryption, key.as_mut_slice())?;
        derive(master, master_salt, labels.salt, salt.as_mut_slice())?;
        derive(master, master_salt, labels.auth, auth.as_mut_slice())?;
        Self::from_session(&key, &salt, &auth)
    }

    pub(crate) fn from_session(
        key: &[u8; 16],
        salt: &[u8; 14],
        auth: &[u8; 20],
    ) -> Result<Self, SrtpError> {
        Ok(Self {
            cipher: Aes128Enc::new_from_slice(key).map_err(|_| SrtpError::Config)?,
            salt: Zeroizing::new(*salt),
            mac: <Hmac<Sha1> as KeyInit>::new_from_slice(auth).map_err(|_| SrtpError::Config)?,
        })
    }

    /// `IV = (k_s * 2^16) XOR (SSRC * 2^64) XOR (i * 2^16)`, RFC 3711
    /// section 4.1.1.
    pub(crate) fn iv(&self, ssrc: u32, index: u64) -> Zeroizing<[u8; 16]> {
        let mut iv = Zeroizing::new([0u8; 16]);
        let ssrc = ssrc.to_be_bytes();
        let index = index.to_be_bytes();
        for (n, b) in iv.iter_mut().enumerate() {
            let salt = self.salt.get(n).copied().unwrap_or(0);
            let s = n.checked_sub(4).and_then(|k| ssrc.get(k)).copied();
            // The 48-bit index lands in octets 8 to 13.
            let i = n.checked_sub(6).and_then(|k| index.get(k)).copied();
            let s = if n < 8 { s.unwrap_or(0) } else { 0 };
            let i = if (8..14).contains(&n) {
                i.unwrap_or(0)
            } else {
                0
            };
            *b = salt ^ s ^ i;
        }
        iv
    }

    /// HMAC-SHA1 over the parts, compared with `tag` in constant time.
    fn verify(&self, parts: &[&[u8]], tag: &[u8]) -> Result<(), SrtpError> {
        let computed = self.tag(parts);
        let ok = computed
            .get(..CM_TAG)
            .map(|c| bool::from(c.ct_eq(tag)))
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(SrtpError::Authentication)
        }
    }

    fn tag(&self, parts: &[&[u8]]) -> [u8; 20] {
        let mut mac = self.mac.clone();
        for part in parts {
            mac.update(part);
        }
        mac.finalize().into_bytes().into()
    }
}

/// AES-GCM session key and salt for one direction of RTP or RTCP.
pub(crate) struct GcmKeys<C = Aes128Gcm> {
    pub(crate) cipher: C,
    pub(crate) salt: Zeroizing<[u8; 12]>,
}

impl GcmKeys<Aes128Gcm> {
    pub(crate) fn derive(
        master: &Aes128Enc,
        master_salt: &[u8],
        labels: Labels,
    ) -> Result<Self, SrtpError> {
        let mut key = Zeroizing::new([0u8; 16]);
        let mut salt = Zeroizing::new([0u8; 12]);
        derive(master, master_salt, labels.encryption, key.as_mut_slice())?;
        derive(master, master_salt, labels.salt, salt.as_mut_slice())?;
        Self::from_session(key.as_slice(), &salt)
    }
}

impl<C: KeyInit> GcmKeys<C> {
    pub(crate) fn from_session(key: &[u8], salt: &[u8; 12]) -> Result<Self, SrtpError> {
        Ok(Self {
            cipher: C::new_from_slice(key).map_err(|_| SrtpError::Config)?,
            salt: Zeroizing::new(*salt),
        })
    }
}

/// RFC 7714 section 8.1: `00 00 || SSRC || ROC || SEQ`, XOR the salt.
pub(crate) fn gcm_rtp_iv(salt: &[u8; 12], ssrc: u32, index: u64) -> Array<u8, U12> {
    let mut iv = [0u8; 12];
    let ssrc = ssrc.to_be_bytes();
    // ROC || SEQ is the low 48 bits of the index.
    let index = index.to_be_bytes();
    for (n, b) in iv.iter_mut().enumerate() {
        let s = n.checked_sub(2).and_then(|k| ssrc.get(k)).copied();
        let i = n.checked_sub(4).and_then(|k| index.get(k)).copied();
        let v = if n < 6 {
            s.unwrap_or(0)
        } else {
            i.unwrap_or(0)
        };
        *b = v ^ salt.get(n).copied().unwrap_or(0);
    }
    Array::from(iv)
}

/// RFC 7714 section 9.1: `00 00 || SSRC || 00 00 || 0 || index`, XOR the
/// salt.
pub(crate) fn gcm_rtcp_iv(salt: &[u8; 12], ssrc: u32, index: u32) -> Array<u8, U12> {
    let mut iv = [0u8; 12];
    let ssrc = ssrc.to_be_bytes();
    let index = (index & 0x7fff_ffff).to_be_bytes();
    for (n, b) in iv.iter_mut().enumerate() {
        let s = n.checked_sub(2).and_then(|k| ssrc.get(k)).copied();
        let i = n.checked_sub(8).and_then(|k| index.get(k)).copied();
        let v = match n {
            2..=5 => s.unwrap_or(0),
            8..=11 => i.unwrap_or(0),
            _ => 0,
        };
        *b = v ^ salt.get(n).copied().unwrap_or(0);
    }
    Array::from(iv)
}

fn tag_from(bytes: &[u8]) -> Result<Array<u8, U16>, SrtpError> {
    let mut tag = [0u8; GCM_TAG];
    if bytes.len() != GCM_TAG {
        return Err(SrtpError::Malformed);
    }
    tag.copy_from_slice(bytes);
    Ok(Array::from(tag))
}

/// The SSRC (octets 4 to 7) of an RTCP or SRTCP packet.
pub(crate) fn rtcp_ssrc(buf: &[u8]) -> Result<u32, SrtpError> {
    crate::rtp::be32(buf, 4).ok_or(SrtpError::Malformed)
}

/// The E flag and 31-bit index of an SRTCP packet, with its trailer length
/// (`ESRTCP` plus `tag_len`). For counter mode the word comes before the
/// tag; for GCM, after it.
pub(crate) fn srtcp_word(
    buf: &[u8],
    tag_len: usize,
    word_last: bool,
) -> Result<(bool, u32), SrtpError> {
    let len = buf.len();
    if len < 8 + ESRTCP + tag_len {
        return Err(SrtpError::Malformed);
    }
    let at = if word_last {
        len - ESRTCP
    } else {
        len - ESRTCP - tag_len
    };
    let word = crate::rtp::be32(buf, at).ok_or(SrtpError::Malformed)?;
    Ok((word & 0x8000_0000 != 0, word & 0x7fff_ffff))
}

// ---- Counter mode and HMAC-SHA1 -------------------------------------------

/// Unprotects an SRTP packet: `header || encrypted payload || tag`.
/// Returns the RTP length.
pub(crate) fn cm_rtp_unprotect(
    keys: &CmKeys,
    buf: &mut [u8],
    header_len: usize,
    ssrc: u32,
    index: u64,
) -> Result<usize, SrtpError> {
    let body_end = buf.len().checked_sub(CM_TAG).ok_or(SrtpError::Malformed)?;
    if body_end < header_len {
        return Err(SrtpError::Malformed);
    }
    let (authenticated, tag) = buf.split_at_mut(body_end);
    let roc = ((index >> 16) as u32).to_be_bytes();
    keys.verify(&[authenticated, &roc], tag)?;
    let payload = authenticated
        .get_mut(header_len..)
        .ok_or(SrtpError::Malformed)?;
    keystream(&keys.cipher, &keys.iv(ssrc, index), payload)?;
    Ok(body_end)
}

/// Protects the RTP packet in `buf[..len]`, appending the tag. Returns the
/// SRTP length.
#[cfg(any(test, feature = "testing"))]
pub(crate) fn cm_rtp_protect(
    keys: &CmKeys,
    buf: &mut [u8],
    len: usize,
    header_len: usize,
    ssrc: u32,
    index: u64,
) -> Result<usize, SrtpError> {
    let end = len.checked_add(CM_TAG).ok_or(SrtpError::BufferTooSmall)?;
    if end > buf.len() {
        return Err(SrtpError::BufferTooSmall);
    }
    let packet = buf.get_mut(..end).ok_or(SrtpError::BufferTooSmall)?;
    let (rtp, tag) = packet.split_at_mut(len);
    let payload = rtp.get_mut(header_len..).ok_or(SrtpError::Malformed)?;
    keystream(&keys.cipher, &keys.iv(ssrc, index), payload)?;
    let roc = ((index >> 16) as u32).to_be_bytes();
    let computed = keys.tag(&[rtp, &roc]);
    tag.copy_from_slice(computed.get(..CM_TAG).ok_or(SrtpError::Malformed)?);
    Ok(end)
}

/// Unprotects an SRTCP packet: `RTCP || E+index || tag` (RFC 3711 section
/// 3.4). Returns the RTCP length.
pub(crate) fn cm_rtcp_unprotect(keys: &CmKeys, buf: &mut [u8]) -> Result<usize, SrtpError> {
    let (encrypted, index) = srtcp_word(buf, CM_TAG, false)?;
    let ssrc = rtcp_ssrc(buf)?;
    let auth_end = buf.len() - CM_TAG;
    let rtcp_end = auth_end - ESRTCP;
    let (authenticated, tag) = buf.split_at_mut(auth_end);
    keys.verify(&[authenticated], tag)?;
    if encrypted {
        let body = authenticated
            .get_mut(8..rtcp_end)
            .ok_or(SrtpError::Malformed)?;
        keystream(&keys.cipher, &keys.iv(ssrc, u64::from(index)), body)?;
    }
    Ok(rtcp_end)
}

/// Protects the RTCP packet in `buf[..len]` with SRTCP `index`, encrypting
/// when `encrypt` is set. Returns the SRTCP length.
pub(crate) fn cm_rtcp_protect(
    keys: &CmKeys,
    buf: &mut [u8],
    len: usize,
    index: u32,
    encrypt: bool,
) -> Result<usize, SrtpError> {
    let end = len
        .checked_add(ESRTCP + CM_TAG)
        .ok_or(SrtpError::BufferTooSmall)?;
    if len < 8 {
        return Err(SrtpError::Malformed);
    }
    if end > buf.len() {
        return Err(SrtpError::BufferTooSmall);
    }
    let ssrc = rtcp_ssrc(buf)?;
    let word = (u32::from(encrypt) << 31 | (index & 0x7fff_ffff)).to_be_bytes();
    let packet = buf.get_mut(..end).ok_or(SrtpError::BufferTooSmall)?;
    let (authenticated, tag) = packet.split_at_mut(len + ESRTCP);
    if encrypt {
        let body = authenticated.get_mut(8..len).ok_or(SrtpError::Malformed)?;
        keystream(&keys.cipher, &keys.iv(ssrc, u64::from(index)), body)?;
    }
    authenticated
        .get_mut(len..)
        .ok_or(SrtpError::Malformed)?
        .copy_from_slice(&word);
    let computed = keys.tag(&[authenticated]);
    tag.copy_from_slice(computed.get(..CM_TAG).ok_or(SrtpError::Malformed)?);
    Ok(end)
}

// ---- AES-GCM ----------------------------------------------------------------

/// Unprotects an AEAD SRTP packet: `header (AAD) || ciphertext || tag`
/// (RFC 7714 section 8.2). Returns the RTP length.
pub(crate) fn gcm_rtp_unprotect<C>(
    keys: &GcmKeys<C>,
    buf: &mut [u8],
    header_len: usize,
    ssrc: u32,
    index: u64,
) -> Result<usize, SrtpError>
where
    C: AeadInOut<NonceSize = U12, TagSize = U16>,
{
    let body_end = buf.len().checked_sub(GCM_TAG).ok_or(SrtpError::Malformed)?;
    if body_end < header_len {
        return Err(SrtpError::Malformed);
    }
    let (packet, tag) = buf.split_at_mut(body_end);
    let tag = tag_from(tag)?;
    if packet.len() < header_len {
        return Err(SrtpError::Malformed);
    }
    let (aad, ciphertext) = packet.split_at_mut(header_len);
    keys.cipher
        .decrypt_inout_detached(
            &gcm_rtp_iv(&keys.salt, ssrc, index),
            aad,
            ciphertext.into(),
            &tag,
        )
        .map_err(|_| SrtpError::Authentication)?;
    Ok(body_end)
}

/// Protects the RTP packet in `buf[..len]`, appending the AEAD tag.
#[cfg(any(test, feature = "testing"))]
pub(crate) fn gcm_rtp_protect<C>(
    keys: &GcmKeys<C>,
    buf: &mut [u8],
    len: usize,
    header_len: usize,
    ssrc: u32,
    index: u64,
) -> Result<usize, SrtpError>
where
    C: AeadInOut<NonceSize = U12, TagSize = U16>,
{
    let end = len.checked_add(GCM_TAG).ok_or(SrtpError::BufferTooSmall)?;
    if end > buf.len() {
        return Err(SrtpError::BufferTooSmall);
    }
    if header_len > len {
        return Err(SrtpError::Malformed);
    }
    let packet = buf.get_mut(..end).ok_or(SrtpError::BufferTooSmall)?;
    let (rtp, tag_out) = packet.split_at_mut(len);
    let (aad, plaintext) = rtp.split_at_mut(header_len);
    let tag = keys
        .cipher
        .encrypt_inout_detached(&gcm_rtp_iv(&keys.salt, ssrc, index), aad, plaintext.into())
        .map_err(|_| SrtpError::Malformed)?;
    tag_out.copy_from_slice(&tag);
    Ok(end)
}

/// Unprotects an AEAD SRTCP packet (RFC 7714 sections 9.2 and 9.3):
/// `header (8) || ciphertext || tag || E+index` when E is 1, and
/// `RTCP || tag || E+index` when E is 0. Returns the RTCP length.
pub(crate) fn gcm_rtcp_unprotect<C>(keys: &GcmKeys<C>, buf: &mut [u8]) -> Result<usize, SrtpError>
where
    C: AeadInOut<NonceSize = U12, TagSize = U16>,
{
    let (encrypted, index) = srtcp_word(buf, GCM_TAG, true)?;
    let ssrc = rtcp_ssrc(buf)?;
    let len = buf.len();
    let rtcp_end = len - ESRTCP - GCM_TAG;
    let nonce = gcm_rtcp_iv(&keys.salt, ssrc, index);
    let word: [u8; ESRTCP] = buf
        .get(len - ESRTCP..)
        .and_then(|w| w.try_into().ok())
        .ok_or(SrtpError::Malformed)?;
    let tag = tag_from(
        buf.get(rtcp_end..len - ESRTCP)
            .ok_or(SrtpError::Malformed)?,
    )?;
    if encrypted {
        // AAD: the first eight octets and the ESRTCP word.
        let mut aad = [0u8; 8 + ESRTCP];
        let (head, tail) = aad.split_at_mut(8);
        head.copy_from_slice(buf.get(..8).ok_or(SrtpError::Malformed)?);
        tail.copy_from_slice(&word);
        let ciphertext = buf.get_mut(8..rtcp_end).ok_or(SrtpError::Malformed)?;
        keys.cipher
            .decrypt_inout_detached(&nonce, &aad, ciphertext.into(), &tag)
            .map_err(|_| SrtpError::Authentication)?;
    } else {
        // AAD: the whole RTCP packet and the ESRTCP word. Slide the word
        // down over the tag (saved above) so the AAD is contiguous.
        buf.get_mut(rtcp_end..rtcp_end + ESRTCP)
            .ok_or(SrtpError::Malformed)?
            .copy_from_slice(&word);
        let aad = buf.get(..rtcp_end + ESRTCP).ok_or(SrtpError::Malformed)?;
        keys.cipher
            .decrypt_inout_detached(&nonce, aad, (&mut [0u8; 0][..]).into(), &tag)
            .map_err(|_| SrtpError::Authentication)?;
    }
    Ok(rtcp_end)
}

/// Protects the RTCP packet in `buf[..len]` with SRTCP `index`, encrypting
/// when `encrypt` is set. Returns the SRTCP length.
pub(crate) fn gcm_rtcp_protect<C>(
    keys: &GcmKeys<C>,
    buf: &mut [u8],
    len: usize,
    index: u32,
    encrypt: bool,
) -> Result<usize, SrtpError>
where
    C: AeadInOut<NonceSize = U12, TagSize = U16>,
{
    let end = len
        .checked_add(GCM_TAG + ESRTCP)
        .ok_or(SrtpError::BufferTooSmall)?;
    if len < 8 {
        return Err(SrtpError::Malformed);
    }
    if end > buf.len() {
        return Err(SrtpError::BufferTooSmall);
    }
    let ssrc = rtcp_ssrc(buf)?;
    let index = index & 0x7fff_ffff;
    let word = (u32::from(encrypt) << 31 | index).to_be_bytes();
    let nonce = gcm_rtcp_iv(&keys.salt, ssrc, index);
    let packet = buf.get_mut(..end).ok_or(SrtpError::BufferTooSmall)?;
    let tag = if encrypt {
        let mut aad = [0u8; 8 + ESRTCP];
        let (head, tail) = aad.split_at_mut(8);
        head.copy_from_slice(packet.get(..8).ok_or(SrtpError::Malformed)?);
        tail.copy_from_slice(&word);
        let plaintext = packet.get_mut(8..len).ok_or(SrtpError::Malformed)?;
        keys.cipher
            .encrypt_inout_detached(&nonce, &aad, plaintext.into())
            .map_err(|_| SrtpError::Malformed)?
    } else {
        // Place the word straight after the RTCP so the AAD is contiguous,
        // then move it after the tag.
        packet
            .get_mut(len..len + ESRTCP)
            .ok_or(SrtpError::Malformed)?
            .copy_from_slice(&word);
        let aad = packet.get(..len + ESRTCP).ok_or(SrtpError::Malformed)?;
        keys.cipher
            .encrypt_inout_detached(&nonce, aad, (&mut [0u8; 0][..]).into())
            .map_err(|_| SrtpError::Malformed)?
    };
    packet
        .get_mut(len..len + GCM_TAG)
        .ok_or(SrtpError::Malformed)?
        .copy_from_slice(&tag);
    packet
        .get_mut(len + GCM_TAG..)
        .ok_or(SrtpError::Malformed)?
        .copy_from_slice(&word);
    Ok(end)
}
