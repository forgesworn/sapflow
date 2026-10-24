//! SRT AES-CTR/KM profile, from draft-sharabayko-srt-01 §§3.2.2,6.
//!
//! AES-128/256 only; AES-192, GCM, nonzero KEKI and alternative salts are
//! explicitly unsupported. AES-CTR does not authenticate media or headers.
//! Sequence-window duplicate rejection is not cryptographic authentication.
//! Control packets (including stream IDs) remain clear. Use strong,
//! session-scoped secrets supplied over an authenticated channel.
//!
//! Primitives: RustCrypto aes 0.9.3, ctr 0.10.1, aes-kw 0.3.1,
//! pbkdf2 0.13.0, sha1 0.11.0 (MIT OR Apache-2.0); zeroize 1.9.1
//! (Apache-2.0 OR MIT), getrandom 0.4.3 (MIT OR Apache-2.0).
//! Primary algorithm references: RFC3394, RFC8018, NIST SP800-38A.
//! The KDF uses SHA-1/2048 iterations because this is SRT's wire profile.
//! Retained keys/passphrase, cached schedules and temporary key arrays are
//! zeroised on drop. Raw keys occupy at most two 32-byte buffers plus a
//! 32-byte KEK, with one boxed raw-key/encryption-schedule owner per occupied
//! slot. Empty slots retain only optional box metadata, never AES schedules.
//! Schedule storage depends on the cipher backend. KM messages are at most
//! 104 bytes; parsing never derives a key until shape validation.
//! Data encrypts/decrypts in place without extra per-payload allocations;
//! each packet borrows its slot's schedule with a fresh sequence-derived IV.
//! This does not promise locked memory, crash-dump exclusion or erasure of
//! copies held by the application or KDF's internal stack temporaries.

use crate::seq::Seq;
use aes_kw::{KeyInit, KwAes128, KwAes256};
use ctr::cipher::{InnerIvInit, StreamCipher};
use std::{fmt, sync::Arc};
use zeroize::Zeroizing;

const MAX_PERIOD: u32 = 1 << 25;
const KM_RETRY_US: i64 = 100_000;

/// AES key length: the two sizes this profile supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySize {
    /// AES-128 (16-byte keys).
    Aes128,
    /// AES-256 (32-byte keys).
    Aes256,
}
impl KeySize {
    pub(crate) fn len(self) -> usize {
        match self {
            Self::Aes128 => 16,
            Self::Aes256 => 32,
        }
    }
    pub(crate) fn field(self) -> u16 {
        (self.len() / 8) as u16
    }
}

/// Clones share one zeroising passphrase allocation; Debug is redacted.
#[derive(Clone)]
pub struct CryptoConfig {
    secret: Arc<Zeroizing<Vec<u8>>>,
    /// AES key length for the session.
    pub key_size: KeySize,
    /// Original packets per key, at most 2^25. Retransmissions do not count.
    pub refresh_packets: u32,
    /// Announce the next key this many packets before switching.
    /// Must be below half the refresh period. Sender fails closed if a
    /// replaced slot still has retained unacknowledged ciphertext.
    pub preannounce_packets: u32,
}
impl fmt::Debug for CryptoConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CryptoConfig")
            .field("secret", &"[REDACTED]")
            .field("key_size", &self.key_size)
            .field("refresh_packets", &self.refresh_packets)
            .field("preannounce_packets", &self.preannounce_packets)
            .finish()
    }
}
impl CryptoConfig {
    /// Takes ownership of 10..=79 non-NUL passphrase bytes. No plaintext
    /// fallback is permitted for a session configured with this value.
    pub fn new(passphrase: Vec<u8>, key_size: KeySize) -> Result<Self, CryptoError> {
        let secret = Zeroizing::new(passphrase);
        if !(10..=79).contains(&secret.len()) || secret.contains(&0) {
            return Err(CryptoError::Config);
        }
        Ok(Self {
            secret: Arc::new(secret),
            key_size,
            refresh_packets: MAX_PERIOD,
            preannounce_packets: 4000,
        })
    }
    pub(crate) fn valid(&self) -> bool {
        self.refresh_packets <= MAX_PERIOD
            && self.preannounce_packets > 0
            && self.preannounce_packets < self.refresh_packets / 2
    }
}

/// Why a key, key material or payload operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// The passphrase or key refresh settings are not valid.
    Config,
    /// The system's secure random source failed.
    Random,
    /// The key material or payload is the wrong shape or size.
    Malformed,
    /// Well formed but outside the supported profile (for example AES-192,
    /// GCM, a nonzero KEKI, or a key size or salt that differs from the
    /// session's).
    Unsupported,
    /// The key material would not unwrap with this passphrase.
    BadSecret,
    /// Encryption was required but not supplied. This crate does not
    /// currently return it.
    Required,
    /// No usable key: a key slot is empty, a key has run its period or cannot
    /// yet be replaced, or the next key was not confirmed in time.
    KeyExpired,
    /// A key refresh that repeats or contradicts the current keys, or a
    /// packet under a key that has already been replaced.
    Replay,
}
impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SRT crypto: {self:?}")
    }
}
impl std::error::Error for CryptoError {}

type Secret = Zeroizing<Vec<u8>>;
// Keep the schedule inside its single boxed key owner, rather than allocating
// the cipher variant separately from the raw-key/schedule pair.
#[allow(clippy::large_enum_variant)]
enum PacketCipher {
    Aes128(aes::Aes128Enc),
    Aes256(aes::Aes256Enc),
}
struct PacketKey {
    secret: Secret,
    cipher: PacketCipher,
}
impl PacketKey {
    fn new(size: KeySize, secret: Secret) -> Result<Box<Self>, CryptoError> {
        let cipher = match size {
            KeySize::Aes128 => PacketCipher::Aes128(
                aes::Aes128Enc::new_from_slice(&secret).map_err(|_| CryptoError::Config)?,
            ),
            KeySize::Aes256 => PacketCipher::Aes256(
                aes::Aes256Enc::new_from_slice(&secret).map_err(|_| CryptoError::Config)?,
            ),
        };
        Ok(Box::new(Self { secret, cipher }))
    }
    fn crypt(&self, iv: &[u8; 16], bytes: &mut [u8]) {
        match &self.cipher {
            PacketCipher::Aes128(cipher) => {
                let core =
                    ctr::CtrCore::<_, ctr::flavors::Ctr128BE>::inner_iv_init(cipher, &(*iv).into());
                ctr::Ctr128BE::<&aes::Aes128Enc>::from_core(core).apply_keystream(bytes);
            }
            PacketCipher::Aes256(cipher) => {
                let core =
                    ctr::CtrCore::<_, ctr::flavors::Ctr128BE>::inner_iv_init(cipher, &(*iv).into());
                ctr::Ctr128BE::<&aes::Aes256Enc>::from_core(core).apply_keystream(bytes);
            }
        }
    }
}
struct Material {
    size: KeySize,
    salt: [u8; 16],
    kek: Secret,
    keys: [Option<Box<PacketKey>>; 2],
}
fn index(kk: u8) -> Result<usize, CryptoError> {
    match kk {
        1 => Ok(0),
        2 => Ok(1),
        _ => Err(CryptoError::Malformed),
    }
}
fn derive(config: &CryptoConfig, salt: &[u8; 16]) -> Secret {
    let mut key = Zeroizing::new(vec![0; config.key_size.len()]);
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(
        config.secret.as_slice(),
        salt.get(8..).unwrap_or_default(),
        2048,
        &mut key,
    );
    key
}
fn random_key(size: KeySize) -> Result<Secret, CryptoError> {
    let mut key = Zeroizing::new(vec![0; size.len()]);
    getrandom::fill(&mut key).map_err(|_| CryptoError::Random)?;
    Ok(key)
}
fn wrap(size: KeySize, kek: &[u8], plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut out = vec![0; plain.len() + 8];
    match size {
        KeySize::Aes128 => KwAes128::new_from_slice(kek)
            .map_err(|_| CryptoError::Config)?
            .wrap_key(plain, &mut out)
            .map_err(|_| CryptoError::Malformed)?,
        KeySize::Aes256 => KwAes256::new_from_slice(kek)
            .map_err(|_| CryptoError::Config)?
            .wrap_key(plain, &mut out)
            .map_err(|_| CryptoError::Malformed)?,
    };
    Ok(out)
}
fn unwrap(size: KeySize, kek: &[u8], wrapped: &[u8]) -> Result<Secret, CryptoError> {
    let mut out = Zeroizing::new(vec![
        0;
        wrapped
            .len()
            .checked_sub(8)
            .ok_or(CryptoError::Malformed)?
    ]);
    match size {
        KeySize::Aes128 => KwAes128::new_from_slice(kek)
            .map_err(|_| CryptoError::Config)?
            .unwrap_key(wrapped, &mut out)
            .map_err(|_| CryptoError::BadSecret)?,
        KeySize::Aes256 => KwAes256::new_from_slice(kek)
            .map_err(|_| CryptoError::Config)?
            .unwrap_key(wrapped, &mut out)
            .map_err(|_| CryptoError::BadSecret)?,
    };
    Ok(out)
}
struct Km<'a> {
    kk: u8,
    size: KeySize,
    salt: [u8; 16],
    wrapped: &'a [u8],
}
impl<'a> Km<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, CryptoError> {
        let header: [u8; 16] = bytes
            .get(..16)
            .ok_or(CryptoError::Malformed)?
            .try_into()
            .map_err(|_| CryptoError::Malformed)?;
        let [
            v,
            sign1,
            sign2,
            kk,
            k0,
            k1,
            k2,
            k3,
            cipher,
            auth,
            se,
            r0,
            r1,
            r2,
            slen,
            klen,
        ] = header;
        if (v, sign1, sign2) != (0x12, 0x20, 0x29)
            || !(1..=3).contains(&kk)
            || [r0, r1, r2] != [0; 3]
        {
            return Err(CryptoError::Malformed);
        }
        if [k0, k1, k2, k3] != [0; 4] || (cipher, auth, se, slen) != (2, 0, 2, 4) {
            return Err(CryptoError::Unsupported);
        }
        let size = match klen {
            4 => KeySize::Aes128,
            8 => KeySize::Aes256,
            _ => return Err(CryptoError::Unsupported),
        };
        let count = if kk == 3 { 2 } else { 1 };
        if bytes.len() != 32 + 8 + count * size.len() {
            return Err(CryptoError::Malformed);
        }
        Ok(Self {
            kk,
            size,
            salt: bytes
                .get(16..32)
                .ok_or(CryptoError::Malformed)?
                .try_into()
                .map_err(|_| CryptoError::Malformed)?,
            wrapped: bytes.get(32..).ok_or(CryptoError::Malformed)?,
        })
    }
}
pub(crate) fn valid_km(bytes: &[u8], field: u16) -> bool {
    Km::parse(bytes).is_ok_and(|km| field == 0 || km.size.field() == field)
}
impl Material {
    fn message(&self) -> Result<Vec<u8>, CryptoError> {
        let mut kk = 0;
        let mut plain = Zeroizing::new(Vec::with_capacity(64));
        for (i, key) in self.keys.iter().enumerate() {
            if let Some(key) = key {
                kk |= 1 << i;
                plain.extend_from_slice(&key.secret);
            }
        }
        let mut out = vec![
            0x12,
            0x20,
            0x29,
            kk,
            0,
            0,
            0,
            0,
            2,
            0,
            2,
            0,
            0,
            0,
            4,
            (self.size.len() / 4) as u8,
        ];
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&wrap(self.size, &self.kek, &plain)?);
        Ok(out)
    }
    fn crypt(&self, kk: u8, seq: Seq, bytes: &mut [u8]) -> Result<(), CryptoError> {
        if bytes.len() > 1456 {
            return Err(CryptoError::Malformed);
        }
        let key = self
            .keys
            .get(index(kk)?)
            .and_then(Option::as_ref)
            .ok_or(CryptoError::KeyExpired)?;
        key.crypt(&iv(&self.salt, seq), bytes);
        Ok(())
    }
}
fn iv(salt: &[u8; 16], seq: Seq) -> [u8; 16] {
    let mut out = [0; 16];
    for (dst, src) in out.iter_mut().take(14).zip(salt) {
        *dst = *src;
    }
    for (dst, src) in out.iter_mut().skip(10).take(4).zip(seq.get().to_be_bytes()) {
        *dst ^= src;
    }
    out
}

pub(crate) struct Encryptor {
    material: Material,
    period: u32,
    preannounce: u32,
    count: u32,
    active: u8,
    pending: Option<Vec<u8>>,
    confirmed: bool,
    next_km: i64,
}
impl Encryptor {
    pub(crate) fn new(config: &CryptoConfig) -> Result<Self, CryptoError> {
        if !config.valid() {
            return Err(CryptoError::Config);
        }
        let mut salt = [0; 16];
        getrandom::fill(&mut salt).map_err(|_| CryptoError::Random)?;
        let material = Material {
            size: config.key_size,
            salt,
            kek: derive(config, &salt),
            keys: [
                Some(PacketKey::new(
                    config.key_size,
                    random_key(config.key_size)?,
                )?),
                None,
            ],
        };
        Ok(Self {
            material,
            period: config.refresh_packets,
            preannounce: config.preannounce_packets,
            count: 0,
            active: 1,
            pending: None,
            confirmed: false,
            next_km: 0,
        })
    }
    pub(crate) fn initial(&self) -> Result<Vec<u8>, CryptoError> {
        self.material.message()
    }
    pub(crate) fn confirm_initial(&self, bytes: &[u8]) -> bool {
        self.initial().is_ok_and(|b| b == bytes)
    }
    /// Prepare an alternate key only if its previous ciphertext has drained.
    /// Packet count is independent of wrapping 31-bit sequence numbers.
    pub(crate) fn encrypt(
        &mut self,
        seq: Seq,
        bytes: &mut [u8],
        retained_keys: u8,
    ) -> Result<u8, CryptoError> {
        if self.count == self.period {
            if !self.confirmed {
                return Err(CryptoError::KeyExpired);
            }
            self.active = 3 - self.active;
            self.count = 0;
            self.pending = None;
            self.confirmed = false;
        }
        if self.count == self.period - self.preannounce {
            let other = 3 - self.active;
            if retained_keys & other != 0 {
                return Err(CryptoError::KeyExpired);
            }
            let slot = self
                .material
                .keys
                .get_mut(index(other)?)
                .ok_or(CryptoError::Malformed)?;
            *slot = Some(PacketKey::new(
                self.material.size,
                random_key(self.material.size)?,
            )?);
            self.pending = Some(self.material.message()?);
            self.next_km = 0;
        }
        self.material.crypt(self.active, seq, bytes)?;
        self.count += 1;
        Ok(self.active)
    }
    pub(crate) fn poll_km(&mut self, now: i64) -> Option<Vec<u8>> {
        if self.confirmed || now < self.next_km {
            return None;
        }
        let bytes = self.pending.as_ref()?.clone();
        self.next_km = now.saturating_add(KM_RETRY_US);
        Some(bytes)
    }
    pub(crate) fn deadline(&self) -> Option<i64> {
        (!self.confirmed && self.pending.is_some()).then_some(self.next_km)
    }
    pub(crate) fn confirm(&mut self, bytes: &[u8]) -> bool {
        if self.pending.as_ref().is_some_and(|b| b == bytes) {
            self.confirmed = true;
            true
        } else {
            false
        }
    }
}

pub(crate) struct Decryptor {
    material: Material,
    active: u8,
    highest: Option<Seq>,
    transition: Option<Seq>,
    active_start: Option<Seq>,
    staged: bool,
    last_km: Vec<u8>,
}
impl Decryptor {
    pub(crate) fn new(config: &CryptoConfig, bytes: &[u8]) -> Result<Self, CryptoError> {
        if !config.valid() {
            return Err(CryptoError::Config);
        }
        let km = Km::parse(bytes)?;
        if km.size != config.key_size {
            return Err(CryptoError::Unsupported);
        }
        let kek = derive(config, &km.salt);
        let plain = unwrap(km.size, &kek, km.wrapped)?;
        let mut keys = [None, None];
        let mut chunks = plain.chunks_exact(km.size.len());
        for (i, slot) in keys.iter_mut().enumerate() {
            if km.kk & (1 << i) != 0 {
                *slot = Some(PacketKey::new(
                    km.size,
                    Zeroizing::new(chunks.next().ok_or(CryptoError::Malformed)?.to_vec()),
                )?);
            }
        }
        Ok(Self {
            material: Material {
                size: km.size,
                salt: km.salt,
                kek,
                keys,
            },
            active: if km.kk == 2 { 2 } else { 1 },
            highest: None,
            transition: None,
            active_start: None,
            staged: km.kk == 3,
            last_km: bytes.to_vec(),
        })
    }
    pub(crate) fn active_key(&self) -> u8 {
        self.active
    }
    pub(crate) fn matches_last_km(&self, bytes: &[u8]) -> bool {
        bytes == self.last_km
    }
    pub(crate) fn refresh(&mut self, bytes: &[u8]) -> Result<(), CryptoError> {
        if self.matches_last_km(bytes) {
            return Ok(());
        }
        let km = Km::parse(bytes)?;
        // Keep KDF work out of established packet callbacks. This profile
        // holds the session salt fixed and refuses alternative refresh salts.
        if km.size != self.material.size || km.salt != self.material.salt {
            return Err(CryptoError::Unsupported);
        }
        if self.staged {
            return Err(CryptoError::Replay);
        }
        let plain = unwrap(km.size, &self.material.kek, km.wrapped)?;
        let mut chunks = plain.chunks_exact(km.size.len());
        let mut replacement = None;
        for (i, slot) in self.material.keys.iter().enumerate() {
            if km.kk & (1 << i) == 0 {
                continue;
            }
            let key = chunks.next().ok_or(CryptoError::Malformed)?;
            if 1 << i == self.active {
                if slot.as_ref().is_none_or(|old| old.secret.as_slice() != key) {
                    return Err(CryptoError::Replay);
                }
            } else {
                if slot
                    .as_ref()
                    .is_some_and(|old| old.secret.as_slice() == key)
                {
                    return Err(CryptoError::Replay);
                }
                replacement = Some(Zeroizing::new(key.to_vec()));
            }
        }
        let key = PacketKey::new(km.size, replacement.ok_or(CryptoError::Replay)?)?;
        *self
            .material
            .keys
            .get_mut(index(3 - self.active)?)
            .ok_or(CryptoError::Malformed)? = Some(key);
        self.staged = true;
        self.last_km = bytes.to_vec();
        Ok(())
    }
    pub(crate) fn decrypt(
        &mut self,
        kk: u8,
        seq: Seq,
        bytes: &mut [u8],
    ) -> Result<(), CryptoError> {
        index(kk)?;
        let newer = self.highest.is_none_or(|old| old.distance_to(seq) > 0);
        if kk != self.active && newer {
            if !self.staged {
                return Err(CryptoError::KeyExpired);
            }
            self.active = kk;
            self.staged = false;
            self.transition = Some(seq);
            self.active_start = Some(seq);
        } else if kk != self.active && self.transition.is_some_and(|s| s.distance_to(seq) >= 0) {
            return Err(CryptoError::Replay);
        }
        if kk == self.active {
            let start = *self.active_start.get_or_insert(seq);
            if start.distance_to(seq) >= MAX_PERIOD as i32 {
                return Err(CryptoError::KeyExpired);
            }
        }
        self.material.crypt(kk, seq, bytes)?;
        if newer {
            self.highest = Some(seq);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctr::cipher::KeyIvInit;

    // Keep the old per-packet path independent of the cached cipher. The
    // unused KEK preserves the original Material layout for the benchmark.
    struct PerPacketMaterial {
        size: KeySize,
        salt: [u8; 16],
        _kek: Secret,
        keys: [Option<Secret>; 2],
    }
    impl From<&Material> for PerPacketMaterial {
        fn from(material: &Material) -> Self {
            Self {
                size: material.size,
                salt: material.salt,
                _kek: Zeroizing::new(material.kek.to_vec()),
                keys: std::array::from_fn(|i| {
                    material.keys[i]
                        .as_ref()
                        .map(|key| Zeroizing::new(key.secret.to_vec()))
                }),
            }
        }
    }
    impl PerPacketMaterial {
        fn crypt(&self, kk: u8, seq: Seq, bytes: &mut [u8]) -> Result<(), CryptoError> {
            if bytes.len() > 1456 {
                return Err(CryptoError::Malformed);
            }
            let key = self
                .keys
                .get(index(kk)?)
                .and_then(Option::as_ref)
                .ok_or(CryptoError::KeyExpired)?;
            let iv = iv(&self.salt, seq);
            match self.size {
                KeySize::Aes128 => ctr::Ctr128BE::<aes::Aes128>::new_from_slices(key, &iv)
                    .map_err(|_| CryptoError::Config)?
                    .apply_keystream(bytes),
                KeySize::Aes256 => ctr::Ctr128BE::<aes::Aes256>::new_from_slices(key, &iv)
                    .map_err(|_| CryptoError::Config)?
                    .apply_keystream(bytes),
            }
            Ok(())
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks_exact(2)
            .map(|b| {
                u8::from_str_radix(std::str::from_utf8(b).expect("valid crypto fixture"), 16)
                    .expect("valid crypto fixture")
            })
            .collect()
    }
    fn config() -> CryptoConfig {
        CryptoConfig::new(b"test passphrase!".to_vec(), KeySize::Aes128)
            .expect("valid crypto fixture")
    }
    fn fixture() -> Material {
        fixture_for(KeySize::Aes128)
    }
    fn fixture_for(size: KeySize) -> Material {
        let mut config = config();
        config.key_size = size;
        let salt = std::array::from_fn(|i| 16 + i as u8);
        Material {
            size,
            salt,
            kek: derive(&config, &salt),
            keys: [
                Some(
                    PacketKey::new(size, Zeroizing::new((0..size.len() as u8).collect()))
                        .expect("fixture key"),
                ),
                None,
            ],
        }
    }
    fn fixed_key(size: KeySize, byte: u8) -> Box<PacketKey> {
        PacketKey::new(size, Zeroizing::new(vec![byte; size.len()])).expect("fixture key")
    }
    fn key_probes(material: &Material) -> [[u8; 32]; 2] {
        let mut out = [[42; 32]; 2];
        for (i, bytes) in out.iter_mut().enumerate() {
            material
                .crypt(i as u8 + 1, Seq::new(17), bytes)
                .expect("installed key");
        }
        out
    }
    fn rejected_refresh_preserves_keys(rx: &mut Decryptor, km: &[u8], error: CryptoError) {
        let before = (
            key_probes(&rx.material),
            rx.material.message().expect("installed keys"),
            rx.last_km.clone(),
            rx.active,
            rx.staged,
            rx.highest,
            rx.transition,
            rx.active_start,
        );
        assert_eq!(rx.refresh(km), Err(error));
        assert_eq!(
            (
                key_probes(&rx.material),
                rx.material.message().expect("installed keys"),
                rx.last_km.clone(),
                rx.active,
                rx.staged,
                rx.highest,
                rx.transition,
                rx.active_start,
            ),
            before
        );
    }
    #[test]
    fn cached_schedules_match_per_packet_schedules_at_payload_and_sequence_boundaries() {
        for size in [KeySize::Aes128, KeySize::Aes256] {
            let mut material = fixture_for(size);
            material.keys[1] = Some(fixed_key(size, 64));
            let reference = PerPacketMaterial::from(&material);
            for kk in [1, 2] {
                for seq in [0, 1, 0x1234_5678, 0x7fff_fffe, 0x7fff_ffff, 0] {
                    for len in [0, 1, 15, 16, 17, 31, 32, 1316, 1456] {
                        let plain: Vec<u8> = (0..len).map(|n| n as u8).collect();
                        let mut expected = plain.clone();
                        let mut actual = plain.clone();
                        reference
                            .crypt(kk, Seq::new(seq), &mut expected)
                            .expect("reference");
                        material
                            .crypt(kk, Seq::new(seq), &mut actual)
                            .expect("cached");
                        assert_eq!(actual, expected, "{size:?} slot={kk} seq={seq} len={len}");
                        material
                            .crypt(kk, Seq::new(seq), &mut actual)
                            .expect("fresh IV");
                        assert_eq!(actual, plain);
                    }
                }
            }
            let mut oversized = [42; 1457];
            assert_eq!(
                material.crypt(1, Seq::new(0), &mut oversized),
                Err(CryptoError::Malformed)
            );
            assert_eq!(oversized, [42; 1457]);
            let mut payload = [42; 17];
            for kk in [0, 3] {
                assert_eq!(
                    material.crypt(kk, Seq::new(0), &mut payload),
                    Err(CryptoError::Malformed)
                );
                assert_eq!(payload, [42; 17]);
            }
            material.keys[1] = None;
            assert_eq!(
                material.crypt(2, Seq::new(0), &mut payload),
                Err(CryptoError::KeyExpired)
            );
            assert_eq!(payload, [42; 17]);
        }
    }
    #[test]
    fn cached_slot_replacement_is_atomic_and_preserves_old_key_reordering() {
        for size in [KeySize::Aes128, KeySize::Aes256] {
            let mut config = config();
            config.key_size = size;
            let mut tx = fixture_for(size);
            let mut rx =
                Decryptor::new(&config, &tx.message().expect("initial KM")).expect("receiver");
            let mut first = [1; 17];
            tx.crypt(1, Seq::new(0x7fff_fffd), &mut first)
                .expect("encrypt");
            rx.decrypt(1, Seq::new(0x7fff_fffd), &mut first)
                .expect("decrypt");
            assert_eq!(first, [1; 17]);
            let mut old = [2; 17];
            tx.crypt(1, Seq::new(0x7fff_fffe), &mut old)
                .expect("held old slot");
            tx.keys[1] = Some(fixed_key(size, 64));
            let km = tx.message().expect("refresh KM");
            rx.refresh(&km).expect("install both raw key and schedule");
            let installed = key_probes(&rx.material);
            for _ in 0..3 {
                rx.refresh(&km).expect("cached KM");
                assert_eq!(key_probes(&rx.material), installed);
            }
            let mut next = [3; 17];
            tx.crypt(2, Seq::new(0x7fff_ffff), &mut next)
                .expect("new slot");
            rx.decrypt(2, Seq::new(0x7fff_ffff), &mut next)
                .expect("switch slot");
            assert_eq!(next, [3; 17]);
            rx.decrypt(1, Seq::new(0x7fff_fffe), &mut old)
                .expect("reordered old slot");
            assert_eq!(old, [2; 17]);
            assert_eq!(rx.active_key(), 2);
            assert!(!rx.staged);

            // With slot 2 active, refresh sees the tentative slot 1 replacement
            // before it rejects the contradictory active key in slot 2.
            let mut invalid = fixture_for(size);
            invalid.keys[0] = Some(fixed_key(size, 128));
            invalid.keys[1] = Some(fixed_key(size, 96));
            rejected_refresh_preserves_keys(
                &mut rx,
                &invalid.message().expect("KM"),
                CryptoError::Replay,
            );
            let mut corrupt = km.clone();
            *corrupt.last_mut().expect("wrapped bytes") ^= 1;
            rejected_refresh_preserves_keys(&mut rx, &corrupt, CryptoError::BadSecret);

            tx.keys[0] = Some(fixed_key(size, 128));
            rx.refresh(&tx.message().expect("second refresh"))
                .expect("replace old slot");
            let replaced = key_probes(&rx.material);
            assert_ne!(replaced[0], installed[0]);
            assert_eq!(replaced[1], installed[1]);
            assert_eq!(replaced, key_probes(&tx));
            let mut wrapped = [4; 17];
            tx.crypt(1, Seq::new(0), &mut wrapped)
                .expect("wrapped sequence");
            rx.decrypt(1, Seq::new(0), &mut wrapped)
                .expect("new slot across wrap");
            assert_eq!(wrapped, [4; 17]);
            assert_eq!(rx.active_key(), 1);
            assert!(!rx.staged);
        }
    }
    #[test]
    fn cached_key_owners_keep_zeroising_drop_guarantees() {
        fn zeroises_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        zeroises_on_drop::<Secret>();
        zeroises_on_drop::<aes::Aes128Enc>();
        zeroises_on_drop::<aes::Aes256Enc>();
        for size in [KeySize::Aes128, KeySize::Aes256] {
            assert_eq!(
                PacketKey::new(size, Zeroizing::new(vec![0; size.len() - 1])).err(),
                Some(CryptoError::Config)
            );
        }
    }
    #[test]
    fn empty_packet_slots_do_not_embed_key_schedule_storage() {
        use std::mem::size_of;
        const _: () = assert!(size_of::<Option<Box<PacketKey>>>() == size_of::<usize>());
        const _: () = assert!(size_of::<Material>() <= size_of::<PerPacketMaterial>());
        let slots: [Option<Box<PacketKey>>; 2] = [None, None];
        assert_eq!(std::mem::size_of_val(&slots), 2 * size_of::<usize>());
        assert!(slots.iter().all(Option::is_none));
    }
    #[test]
    #[ignore = "bounded manual microbenchmark; run in release with --ignored --nocapture"]
    fn cached_aes_schedule_microbenchmark() {
        use std::{
            hint::black_box,
            mem::{align_of, size_of, size_of_val},
            time::{Duration, Instant},
        };
        const ITERATIONS: u32 = 4096;
        const ROUNDS: u32 = 4;

        fn setup_time(mut setup: impl FnMut()) -> Duration {
            let start = Instant::now();
            for _ in 0..ITERATIONS {
                setup();
            }
            start.elapsed()
        }
        fn packet_time(
            mut crypt: impl FnMut(u8, Seq, &mut [u8]) -> Result<(), CryptoError>,
            len: usize,
        ) -> (Duration, u64) {
            let input: [u8; 1456] = std::array::from_fn(|i| i as u8);
            let mut output = [0; 1456];
            let mut checksum = 0u64;
            let start = Instant::now();
            for n in 0..ITERATIONS {
                // Both paths reset the same preallocated input. No allocation
                // or key/KDF setup occurs in the cached packet loop.
                output[..len].copy_from_slice(black_box(&input[..len]));
                crypt(
                    black_box(1 + (n & 1) as u8),
                    black_box(Seq::new(0x7fff_f800).add(n)),
                    black_box(&mut output[..len]),
                )
                .expect("benchmark packet");
                let _ = black_box(&output[..len]);
                checksum = checksum
                    .wrapping_add(u64::from(output[0]))
                    .wrapping_add(u64::from(output[len - 1]));
            }
            (start.elapsed(), black_box(checksum))
        }

        eprintln!(
            "aes-cache sizes bytes: full128={} enc128={} full256={} enc256={} cipher={} key={} boxed_slot={} empty_slots={} raw_option={} legacy_material={} cached_material={} material_align={} key_align={} encryptor={} decryptor={} optional_encryptor={} optional_decryptor={} caller={} sender={}",
            size_of::<aes::Aes128>(),
            size_of::<aes::Aes128Enc>(),
            size_of::<aes::Aes256>(),
            size_of::<aes::Aes256Enc>(),
            size_of::<PacketCipher>(),
            size_of::<PacketKey>(),
            size_of::<Option<Box<PacketKey>>>(),
            size_of::<[Option<Box<PacketKey>>; 2]>(),
            size_of::<Option<Secret>>(),
            size_of::<PerPacketMaterial>(),
            size_of::<Material>(),
            align_of::<Material>(),
            align_of::<PacketKey>(),
            size_of::<Encryptor>(),
            size_of::<Decryptor>(),
            size_of::<Option<Encryptor>>(),
            size_of::<Option<Decryptor>>(),
            size_of::<crate::Caller>(),
            size_of::<crate::sender::Sender>()
        );
        eprintln!(
            "aes-cache scope: debug_assertions={} hardware={} iterations={} rounds={}; setup includes AES construction/drop; install separately includes raw Vec allocation/drop versus raw Vec plus boxed cached key construction/drop; packet includes identical buffer reset, IV, output black_box and checksum but no allocation; excludes KDF, refresh and network",
            cfg!(debug_assertions),
            aes::hardware_accelerated(),
            ITERATIONS,
            ROUNDS
        );
        for size in [KeySize::Aes128, KeySize::Aes256] {
            let mut material = fixture_for(size);
            material.keys[1] = Some(fixed_key(size, 64));
            let legacy = PerPacketMaterial::from(&material);
            let key = legacy.keys[0].as_ref().expect("benchmark key").as_slice();
            let full_setup = || match size {
                KeySize::Aes128 => {
                    drop(black_box(
                        aes::Aes128::new_from_slice(black_box(key)).expect("key"),
                    ));
                }
                KeySize::Aes256 => {
                    drop(black_box(
                        aes::Aes256::new_from_slice(black_box(key)).expect("key"),
                    ));
                }
            };
            let cached_setup = || match size {
                KeySize::Aes128 => {
                    drop(black_box(
                        aes::Aes128Enc::new_from_slice(black_box(key)).expect("key"),
                    ));
                }
                KeySize::Aes256 => {
                    drop(black_box(
                        aes::Aes256Enc::new_from_slice(black_box(key)).expect("key"),
                    ));
                }
            };
            let raw_install = || {
                drop(black_box(Zeroizing::new(black_box(key).to_vec())));
            };
            let boxed_install = || {
                drop(black_box(
                    PacketKey::new(size, Zeroizing::new(black_box(key).to_vec()))
                        .expect("boxed key install"),
                ));
            };
            let raw_heap_capacity = material.kek.capacity()
                + material
                    .keys
                    .iter()
                    .flatten()
                    .map(|key| key.secret.capacity())
                    .sum::<usize>();
            let occupied = material.keys.iter().flatten().count();
            let boxed_key_payload_bytes = material
                .keys
                .iter()
                .flatten()
                .map(|key| size_of_val(key.as_ref()))
                .sum::<usize>();
            eprintln!(
                "aes-cache size={size:?} occupied_slots={occupied} boxed_key_payload_bytes={boxed_key_payload_bytes} raw_vec_capacity={raw_heap_capacity}; box payload includes key Vec descriptors/ciphers, raw Vec bytes include SEK/KEK buffers without double counting; excludes allocator metadata and shared passphrase"
            );
            for len in [16, 1316, 1456] {
                let mut expected = [42; 1456];
                let mut actual = expected;
                legacy
                    .crypt(2, Seq::new(0x7fff_ffff), &mut expected[..len])
                    .expect("reference");
                material
                    .crypt(2, Seq::new(0x7fff_ffff), &mut actual[..len])
                    .expect("cached");
                assert_eq!(black_box(actual), black_box(expected));
            }
            for round in 0..ROUNDS {
                let (full, cached) = if round % 2 == 0 {
                    (setup_time(full_setup), setup_time(cached_setup))
                } else {
                    let cached = setup_time(cached_setup);
                    (setup_time(full_setup), cached)
                };
                eprintln!(
                    "aes-cache phase=setup size={size:?} round={round} full_ns={} cached_enc_ns={} count={ITERATIONS}",
                    full.as_nanos(),
                    cached.as_nanos()
                );
                for len in [16, 1316, 1456] {
                    let run_full =
                        || packet_time(|kk, seq, bytes| legacy.crypt(kk, seq, bytes), len);
                    let run_cached =
                        || packet_time(|kk, seq, bytes| material.crypt(kk, seq, bytes), len);
                    let ((full, full_sum), (cached, cached_sum)) = if round % 2 == 0 {
                        (run_full(), run_cached())
                    } else {
                        let cached = run_cached();
                        (run_full(), cached)
                    };
                    assert_eq!(full_sum, cached_sum, "{size:?} len={len} round={round}");
                    eprintln!(
                        "aes-cache phase=packet size={size:?} len={len} round={round} full_ns={} cached_ns={} count={ITERATIONS} checksum={full_sum}",
                        full.as_nanos(),
                        cached.as_nanos()
                    );
                }
            }
            // Run allocation/drop lifecycle work after the unchanged paired
            // AES-constructor and packet phases, outside their timed windows.
            for round in 0..ROUNDS {
                let (raw, boxed) = if round % 2 == 0 {
                    (setup_time(raw_install), setup_time(boxed_install))
                } else {
                    let boxed = setup_time(boxed_install);
                    (setup_time(raw_install), boxed)
                };
                eprintln!(
                    "aes-cache phase=install size={size:?} round={round} raw_secret_ns={} boxed_cached_key_ns={} count={ITERATIONS}",
                    raw.as_nanos(),
                    boxed.as_nanos()
                );
            }
        }
    }
    #[test]
    fn rfc3394_wrap_and_integrity_vector() {
        let kek = hex("000102030405060708090a0b0c0d0e0f");
        let key = hex("00112233445566778899aabbccddeeff");
        let wrapped = hex("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
        assert_eq!(
            wrap(KeySize::Aes128, &kek, &key).expect("valid crypto fixture"),
            wrapped
        );
        assert_eq!(
            *unwrap(KeySize::Aes128, &kek, &wrapped).expect("valid crypto fixture"),
            key
        );
        let mut bad = wrapped;
        bad[0] ^= 1;
        assert_eq!(
            unwrap(KeySize::Aes128, &kek, &bad).err(),
            Some(CryptoError::BadSecret)
        );
    }
    #[test]
    fn rfc6070_pbkdf2_vector() {
        let mut out = [0; 20];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(b"password", b"salt", 2, &mut out);
        assert_eq!(
            out.as_slice(),
            hex("ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957")
        );
    }
    #[test]
    fn ctr_iv_and_independent_openssl_vector() {
        let material = fixture();
        assert_eq!(
            iv(&material.salt, Seq::new(0x12345678)).as_slice(),
            hex("10111213141516171819082f4a650000")
        );
        let mut plain: Vec<u8> = (0..64).collect();
        material
            .crypt(1, Seq::new(0x12345678), &mut plain)
            .expect("valid crypto fixture");
        // Recorded from openssl enc -aes-128-ctr with the explicit IV above.
        assert_eq!(
            plain,
            hex(
                "8c6ed40552954bfd911b25200a07c0bc28573e7ef3521f5e26310ecb093bdf46e2cb08069963ed815a0d4da78f853892a07b722dea5da6e0d46054ea324790d6"
            )
        );
        material
            .crypt(1, Seq::new(0x12345678), &mut plain)
            .expect("valid crypto fixture");
        assert_eq!(plain, (0..64).collect::<Vec<_>>());
    }
    #[test]
    fn malformed_km_is_bounded_and_wrong_secret_fails() {
        let km = fixture().message().expect("valid crypto fixture");
        for n in 0..km.len() {
            assert!(Decryptor::new(&config(), &km[..n]).is_err());
        }
        for i in [0, 1, 2, 3, 4, 8, 9, 10, 11, 12, 13, 14, 15] {
            let mut bad = km.clone();
            bad[i] ^= 0x80;
            assert!(Decryptor::new(&config(), &bad).is_err());
        }
        let bad = CryptoConfig::new(b"different secret".to_vec(), KeySize::Aes128)
            .expect("valid crypto fixture");
        assert_eq!(
            Decryptor::new(&bad, &km).err(),
            Some(CryptoError::BadSecret)
        );
        assert!(!format!("{:?}", config()).contains("test passphrase"));
    }
    #[test]
    fn refresh_ack_loss_reorder_replay_and_sequence_wrap() {
        for size in [KeySize::Aes128, KeySize::Aes256] {
            let mut cfg = config();
            cfg.key_size = size;
            cfg.refresh_packets = 10;
            cfg.preannounce_packets = 3;
            let mut tx = Encryptor::new(&cfg).expect("valid crypto fixture");
            let initial = tx.initial().expect("valid crypto fixture");
            let mut rx = Decryptor::new(&cfg, &initial).expect("valid crypto fixture");
            let mut old = None;
            let mut prior_km = None;
            for n in 0..34 {
                let seq = Seq::new(0x7fff_fff8).add(n);
                let mut data = vec![n as u8; 23];
                let kk = tx.encrypt(seq, &mut data, 0).expect("valid crypto fixture");
                let mut expected = vec![n as u8; 23];
                PerPacketMaterial::from(&tx.material)
                    .crypt(kk, seq, &mut expected)
                    .expect("reference after key refresh");
                assert_eq!(data, expected);
                if n == 9 {
                    old = Some((kk, seq, data.clone()));
                }
                let mut decoded = data;
                rx.decrypt(kk, seq, &mut decoded)
                    .expect("valid crypto fixture");
                assert_eq!(decoded, vec![n as u8; 23]);
                if n == 11 {
                    let (kk, seq, mut data) = old.take().expect("valid crypto fixture");
                    rx.decrypt(kk, seq, &mut data)
                        .expect("valid crypto fixture");
                    assert_eq!(data, vec![9; 23]);
                }
                if let Some(km) = tx.poll_km(i64::from(n) * 100_000) {
                    rx.refresh(&km).expect("valid crypto fixture");
                    rx.refresh(&km).expect("valid crypto fixture");
                    if let Some(old) = prior_km.replace(km.clone()) {
                        assert!(rx.refresh(&old).is_err());
                    }
                    assert!(tx.confirm(&km));
                }
            }
        }
    }
    #[test]
    fn unconfirmed_or_retained_key_expiry_fails_closed() {
        for size in [KeySize::Aes128, KeySize::Aes256] {
            let mut cfg = config();
            cfg.key_size = size;
            cfg.refresh_packets = 6;
            cfg.preannounce_packets = 2;
            let mut tx = Encryptor::new(&cfg).expect("valid crypto fixture");
            for n in 0..6 {
                tx.encrypt(Seq::new(n), &mut [1], 0)
                    .expect("valid crypto fixture");
            }
            assert_eq!(
                tx.encrypt(Seq::new(6), &mut [1], 0),
                Err(CryptoError::KeyExpired)
            );
            let mut tx = Encryptor::new(&cfg).expect("valid crypto fixture");
            for n in 0..4 {
                tx.encrypt(Seq::new(n), &mut [1], 0)
                    .expect("valid crypto fixture");
            }
            let before = tx.material.message().expect("current key");
            assert_eq!(
                tx.encrypt(Seq::new(4), &mut [1], 2),
                Err(CryptoError::KeyExpired)
            );
            assert_eq!(tx.material.message().expect("same key"), before);
        }
    }
    #[test]
    fn lost_km_response_is_retried_without_burst_or_key_regeneration() {
        let mut cfg = config();
        cfg.refresh_packets = 12;
        cfg.preannounce_packets = 5;
        let mut tx = Encryptor::new(&cfg).expect("encryptor");
        let mut rx = Decryptor::new(&cfg, &tx.initial().expect("initial KM")).expect("decryptor");
        for n in 0..8 {
            tx.encrypt(Seq::new(n), &mut [1], 0).expect("encrypt");
        }
        let first = tx.poll_km(0).expect("preannounce");
        rx.refresh(&first).expect("install"); // Response is lost.
        assert!(tx.poll_km(0).is_none());
        assert!(tx.poll_km(99_999).is_none());
        let retry = tx.poll_km(100_000).expect("retry");
        assert_eq!(first, retry);
        rx.refresh(&retry).expect("duplicate is idempotent");
        assert!(tx.confirm(&retry));
        assert!(tx.poll_km(200_000).is_none());
        for n in 8..13 {
            tx.encrypt(Seq::new(n), &mut [1], 0)
                .expect("confirmed rotation");
        }
    }
    #[test]
    fn independent_kdf_km_header_and_receiver_key_use_limit() {
        let material = fixture();
        // Python hashlib.pbkdf2_hmac independent profile vector.
        assert_eq!(
            material.kek.as_slice(),
            hex("3090ae37914479967b10bfc92164ab47")
        );
        let km = material.message().expect("KM");
        assert_eq!(&km[..16], hex("12202901000000000200020000000404"));
        assert_eq!(km.len(), 56);
        let mut rx = Decryptor::new(&config(), &km).expect("decryptor");
        rx.decrypt(1, Seq::new(0), &mut [1]).expect("first");
        assert_eq!(
            rx.decrypt(1, Seq::new(MAX_PERIOD), &mut [1]),
            Err(CryptoError::KeyExpired)
        );
    }
}
