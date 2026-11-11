//! Randomness: the system source, or a seed for tests and fuzzing so a run
//! replays byte for byte (the same pattern as `sapflow-dtls`).

#[cfg(any(test, feature = "testing"))]
use sha1::{Digest, Sha1};
use std::fmt;
use zeroize::Zeroizing;

/// The system random source failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyError;

impl fmt::Display for EntropyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the system random source failed")
    }
}

impl std::error::Error for EntropyError {}

pub(crate) enum Entropy {
    System,
    #[cfg(any(test, feature = "testing"))]
    Seeded {
        seed: Zeroizing<[u8; 32]>,
        counter: u64,
    },
}

impl Entropy {
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn seeded(seed: [u8; 32]) -> Self {
        Self::Seeded {
            seed: Zeroizing::new(seed),
            counter: 0,
        }
    }

    pub(crate) fn fill(&mut self, out: &mut [u8]) -> Result<(), EntropyError> {
        match self {
            Self::System => getrandom::fill(out).map_err(|_| EntropyError),
            #[cfg(any(test, feature = "testing"))]
            Self::Seeded { seed, counter } => {
                // Test randomness only: SHA-1 of the seed and a counter.
                for chunk in out.chunks_mut(20) {
                    let mut h = Sha1::new();
                    h.update(seed.as_slice());
                    h.update(counter.to_be_bytes());
                    let block = h.finalize();
                    *counter += 1;
                    for (d, s) in chunk.iter_mut().zip(block.iter()) {
                        *d = *s;
                    }
                }
                Ok(())
            }
        }
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<Zeroizing<[u8; N]>, EntropyError> {
        let mut a = Zeroizing::new([0u8; N]);
        self.fill(a.as_mut_slice())?;
        Ok(a)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, EntropyError> {
        Ok(u32::from_be_bytes(*self.array::<4>()?))
    }

    /// `n` characters drawn uniformly from `alphabet` (at most 256
    /// symbols), by rejection sampling.
    pub(crate) fn string(&mut self, alphabet: &[u8], n: usize) -> Result<String, EntropyError> {
        let size = alphabet.len().clamp(1, 256);
        let limit = 256 - 256 % size;
        let mut out = String::with_capacity(n);
        while out.len() < n {
            let block = self.array::<32>()?;
            for &b in block.iter() {
                if out.len() == n {
                    break;
                }
                if usize::from(b) < limit
                    && let Some(&c) = alphabet.get(usize::from(b) % size)
                {
                    out.push(char::from(c));
                }
            }
        }
        Ok(out)
    }
}

/// Letters and digits: valid `ice-char`s (RFC 8839) that need no escaping
/// anywhere.
pub(crate) const ALPHANUMERIC: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_is_repeatable_and_advances() {
        let mut a = Entropy::seeded([7; 32]);
        let mut b = Entropy::seeded([7; 32]);
        let x = a.array::<40>().expect("fill");
        assert_eq!(*x, *b.array::<40>().expect("fill"));
        assert_ne!(*a.array::<40>().expect("fill"), *x);
    }

    #[test]
    fn strings_use_only_the_alphabet() {
        let mut e = Entropy::seeded([1; 32]);
        let s = e.string(ALPHANUMERIC, 200).expect("fill");
        assert_eq!(s.len(), 200);
        assert!(s.bytes().all(|c| c.is_ascii_alphanumeric()));
    }
}
