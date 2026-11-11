//! The TLS 1.2 PRF with SHA-256 (RFC 5246 section 5), the extended master
//! secret (RFC 7627 section 4), the key block (RFC 5246 section 6.3),
//! Finished (section 7.4.9), the exporter (RFC 5705 section 4) and the
//! random source.

use crate::Error;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

type HmacSha256 = Hmac<Sha256>;

/// The master secret.
pub(crate) type MasterSecret = Zeroizing<[u8; 48]>;

/// `PRF(secret, label, seed)` with `seed` given in parts, filling `out`:
/// `P_SHA256(secret, label || seed)`.
pub(crate) fn prf(
    secret: &[u8],
    label: &[u8],
    seed: &[&[u8]],
    out: &mut [u8],
) -> Result<(), Error> {
    let base = <HmacSha256 as KeyInit>::new_from_slice(secret).map_err(|_| Error::Internal)?;
    let hmac = |parts: &[&[u8]]| -> Zeroizing<[u8; 32]> {
        let mut mac = base.clone();
        for p in parts {
            mac.update(p);
        }
        Zeroizing::new(mac.finalize().into_bytes().into())
    };
    // A(1) = HMAC(secret, A(0)) with A(0) = label || seed.
    let mut a = {
        let mut mac = base.clone();
        mac.update(label);
        for s in seed {
            mac.update(s);
        }
        Zeroizing::new(<[u8; 32]>::from(mac.finalize().into_bytes()))
    };
    for chunk in out.chunks_mut(32) {
        let mut mac = base.clone();
        mac.update(a.as_slice());
        mac.update(label);
        for s in seed {
            mac.update(s);
        }
        let mut block = <[u8; 32]>::from(mac.finalize().into_bytes());
        let n = chunk.len();
        chunk.copy_from_slice(block.get(..n).ok_or(Error::Internal)?);
        block.zeroize();
        a = hmac(&[a.as_slice()]);
    }
    Ok(())
}

/// `PRF(pre_master_secret, "extended master secret", session_hash)[0..47]`.
pub(crate) fn extended_master_secret(
    pre_master: &[u8],
    session_hash: &[u8; 32],
) -> Result<MasterSecret, Error> {
    let mut m = Zeroizing::new([0u8; 48]);
    prf(
        pre_master,
        b"extended master secret",
        &[session_hash],
        m.as_mut_slice(),
    )?;
    Ok(m)
}

/// The epoch 1 keys for `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`: no MAC
/// keys, 16-byte write keys, 4-byte implicit IVs.
pub(crate) struct KeyBlock {
    bytes: Zeroizing<[u8; 40]>,
}

impl KeyBlock {
    /// `PRF(master_secret, "key expansion", server_random + client_random)`.
    pub(crate) fn derive(
        master: &MasterSecret,
        client_random: &[u8; 32],
        server_random: &[u8; 32],
    ) -> Result<Self, Error> {
        let mut bytes = Zeroizing::new([0u8; 40]);
        prf(
            master.as_slice(),
            b"key expansion",
            &[server_random, client_random],
            bytes.as_mut_slice(),
        )?;
        Ok(Self { bytes })
    }

    fn part(&self, from: usize, len: usize) -> Result<&[u8], Error> {
        self.bytes.get(from..from + len).ok_or(Error::Internal)
    }

    pub(crate) fn client_key(&self) -> Result<&[u8], Error> {
        self.part(0, 16)
    }
    pub(crate) fn server_key(&self) -> Result<&[u8], Error> {
        self.part(16, 16)
    }
    pub(crate) fn client_iv(&self) -> Result<&[u8], Error> {
        self.part(32, 4)
    }
    pub(crate) fn server_iv(&self) -> Result<&[u8], Error> {
        self.part(36, 4)
    }
}

/// `PRF(master_secret, finished_label, Hash(handshake_messages))[0..11]`.
pub(crate) fn finished(
    master: &MasterSecret,
    label: &[u8],
    transcript_hash: &[u8; 32],
) -> Result<[u8; 12], Error> {
    let mut v = [0u8; 12];
    prf(master.as_slice(), label, &[transcript_hash], &mut v)?;
    Ok(v)
}

/// The RFC 5705 exporter with no context (RFC 5764 section 4.2 and its
/// erratum 4788): `PRF(master_secret, label, client_random +
/// server_random)[0..length-1]`.
pub(crate) fn export(
    master: &MasterSecret,
    label: &[u8],
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    out: &mut [u8],
) -> Result<(), Error> {
    prf(
        master.as_slice(),
        label,
        &[client_random, server_random],
        out,
    )
}

/// SHA-256 of `data`.
pub(crate) fn sha256(data: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for d in data {
        h.update(d);
    }
    h.finalize().into()
}

/// HMAC-SHA256 of `data` under `key`.
pub(crate) fn hmac_sha256(key: &[u8], data: &[&[u8]]) -> Result<[u8; 32], Error> {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).map_err(|_| Error::Internal)?;
    for d in data {
        mac.update(d);
    }
    Ok(mac.finalize().into_bytes().into())
}

/// Where randomness comes from: the operating system, or (for tests and
/// fuzzing only) a SHA-256 counter stream from a seed, so that a run can be
/// replayed byte for byte.
pub(crate) enum Entropy {
    System,
    #[cfg(any(test, feature = "testing"))]
    Seeded {
        seed: Zeroizing<[u8; 32]>,
        counter: u64,
    },
}

impl Entropy {
    pub(crate) fn fill(&mut self, out: &mut [u8]) -> Result<(), Error> {
        match self {
            Self::System => getrandom::fill(out).map_err(|_| Error::Entropy),
            #[cfg(any(test, feature = "testing"))]
            Self::Seeded { seed, counter } => {
                for chunk in out.chunks_mut(32) {
                    let block = sha256(&[seed.as_slice(), &counter.to_be_bytes()]);
                    *counter += 1;
                    let n = chunk.len();
                    chunk.copy_from_slice(block.get(..n).ok_or(Error::Internal)?);
                }
                Ok(())
            }
        }
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<Zeroizing<[u8; N]>, Error> {
        let mut a = Zeroizing::new([0u8; N]);
        self.fill(a.as_mut_slice())?;
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prf_output_is_a_prefix_of_longer_output() {
        let mut short = [0u8; 20];
        let mut long = [0u8; 100];
        prf(b"secret", b"label", &[b"se", b"ed"], &mut short).expect("prf");
        prf(b"secret", b"label", &[b"seed"], &mut long).expect("prf");
        assert_eq!(short, long[..20]);
        assert_ne!(long[32..64], long[64..96]);
    }

    #[test]
    fn seeded_entropy_replays() {
        let mut a = Entropy::Seeded {
            seed: Zeroizing::new([1; 32]),
            counter: 0,
        };
        let mut b = Entropy::Seeded {
            seed: Zeroizing::new([1; 32]),
            counter: 0,
        };
        let x = a.array::<40>().expect("fill");
        let y = b.array::<40>().expect("fill");
        assert_eq!(*x, *y);
        assert_ne!(*a.array::<40>().expect("fill"), *x);
    }
}
