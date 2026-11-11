//! The DTLS 1.2 record layer (RFC 6347 section 4.1): record framing within
//! a datagram, the epoch 1 AES-128-GCM protection (RFC 5288 section 3,
//! RFC 5246 section 6.2.3.3) and the anti-replay window (RFC 6347
//! section 4.1.2.6).

use crate::Error;
use crate::codec::{Reader, put_u16, put_u48};
use aes_gcm::aead::array::Array;
use aes_gcm::{AeadInOut, Aes128Gcm, KeyInit};
use zeroize::Zeroizing;

/// Record header: type, version, epoch, 48-bit sequence number, length.
pub const HEADER_LEN: usize = 13;
/// Explicit nonce and tag that epoch 1 adds to a record.
pub const GCM_OVERHEAD: usize = EXPLICIT_NONCE + TAG;
const EXPLICIT_NONCE: usize = 8;
const TAG: usize = 16;
/// Largest plaintext fragment (RFC 5246 section 6.2.1).
pub const MAX_PLAINTEXT: usize = 1 << 14;
/// Largest record body accepted: a full plaintext plus the GCM overhead.
pub const MAX_CIPHERTEXT: usize = MAX_PLAINTEXT + GCM_OVERHEAD;
/// Highest 48-bit sequence number.
pub const MAX_SEQUENCE: u64 = (1 << 48) - 1;

/// DTLS 1.2 on the wire.
pub const DTLS_1_2: u16 = 0xfefd;
/// DTLS 1.0 on the wire, which clients may use for the first flight.
pub const DTLS_1_0: u16 = 0xfeff;

/// `change_cipher_spec`.
pub const CHANGE_CIPHER_SPEC: u8 = 20;
/// `alert`.
pub const ALERT: u8 = 21;
/// `handshake`.
pub const HANDSHAKE: u8 = 22;
/// `application_data`.
pub const APPLICATION_DATA: u8 = 23;

/// One record from a datagram, borrowing its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<'a> {
    /// Content type.
    pub content_type: u8,
    /// Record version on the wire.
    pub version: u16,
    /// Epoch.
    pub epoch: u16,
    /// 48-bit sequence number.
    pub seq: u64,
    /// The record body (`fragment`).
    pub body: &'a [u8],
}

/// The records in a datagram, in order. Iteration stops at the first record
/// whose header is truncated or whose length runs past the datagram or
/// above [`MAX_CIPHERTEXT`]: records do not span datagrams (RFC 6347
/// section 4.1.1), so nothing after it can be framed.
pub fn records(datagram: &[u8]) -> impl Iterator<Item = Record<'_>> {
    let mut r = Reader::new(datagram);
    std::iter::from_fn(move || {
        let content_type = r.u8()?;
        let version = r.u16()?;
        let epoch = r.u16()?;
        let seq = r.u48()?;
        let len = usize::from(r.u16()?);
        if len > MAX_CIPHERTEXT {
            return None;
        }
        let body = r.bytes(len)?;
        Some(Record {
            content_type,
            version,
            epoch,
            seq,
            body,
        })
    })
}

/// Appends a record header and body to `out`.
pub(crate) fn put_record(
    out: &mut Vec<u8>,
    content_type: u8,
    epoch: u16,
    seq: u64,
    body: &[u8],
) -> Result<(), Error> {
    out.push(content_type);
    put_u16(out, DTLS_1_2);
    put_u16(out, epoch);
    put_u48(out, seq)?;
    put_u16(out, u16::try_from(body.len()).map_err(|_| Error::Internal)?);
    out.extend_from_slice(body);
    Ok(())
}

/// `seq_num + type + version + length` (RFC 5246 section 6.2.3.3), where
/// DTLS's `seq_num` is the epoch and sequence number as on the wire
/// (RFC 6347 section 4.1.2.1) and `length` is the plaintext length.
fn additional_data(content_type: u8, epoch: u16, seq: u64, len: usize) -> Result<[u8; 13], Error> {
    let mut aad = Vec::with_capacity(13);
    put_u16(&mut aad, epoch);
    put_u48(&mut aad, seq)?;
    aad.push(content_type);
    put_u16(&mut aad, DTLS_1_2);
    put_u16(&mut aad, u16::try_from(len).map_err(|_| Error::Internal)?);
    aad.try_into().map_err(|_| Error::Internal)
}

/// One direction's epoch 1 keys: AES-128-GCM with the 4-byte implicit
/// salt (`client_write_IV` or `server_write_IV`). The key schedule and salt
/// are zeroed on drop.
pub(crate) struct Protection {
    cipher: Aes128Gcm,
    salt: Zeroizing<[u8; 4]>,
}

impl Protection {
    pub(crate) fn new(key: &[u8], salt: &[u8]) -> Result<Self, Error> {
        let cipher = Aes128Gcm::new_from_slice(key).map_err(|_| Error::Internal)?;
        let salt = Zeroizing::new(<[u8; 4]>::try_from(salt).map_err(|_| Error::Internal)?);
        Ok(Self { cipher, salt })
    }

    fn nonce(&self, explicit: &[u8; EXPLICIT_NONCE]) -> Array<u8, aes_gcm::aead::consts::U12> {
        let mut n = [0u8; 12];
        let (head, tail) = n.split_at_mut(4);
        head.copy_from_slice(self.salt.as_slice());
        tail.copy_from_slice(explicit);
        Array::from(n)
    }

    /// Seals `plaintext` as one epoch `epoch` record with sequence number
    /// `seq`, appended to `out`. The explicit nonce is the epoch and
    /// sequence number (RFC 5288 section 3 allows it), so it is unique for
    /// as long as sequence numbers are, which [`MAX_SEQUENCE`] guards.
    pub(crate) fn seal(
        &self,
        out: &mut Vec<u8>,
        content_type: u8,
        epoch: u16,
        seq: u64,
        plaintext: &[u8],
    ) -> Result<(), Error> {
        if plaintext.len() > MAX_PLAINTEXT || seq > MAX_SEQUENCE {
            return Err(Error::Internal);
        }
        let explicit = ((u64::from(epoch) << 48) | seq).to_be_bytes();
        let aad = additional_data(content_type, epoch, seq, plaintext.len())?;
        let mut body = Vec::with_capacity(GCM_OVERHEAD + plaintext.len());
        body.extend_from_slice(&explicit);
        body.extend_from_slice(plaintext);
        let tag = self
            .cipher
            .encrypt_inout_detached(
                &self.nonce(&explicit),
                &aad,
                body.get_mut(EXPLICIT_NONCE..)
                    .ok_or(Error::Internal)?
                    .into(),
            )
            .map_err(|_| Error::Internal)?;
        body.extend_from_slice(&tag);
        put_record(out, content_type, epoch, seq, &body)
    }

    /// Opens an epoch 1 record body: `nonce_explicit || ciphertext || tag`.
    /// The explicit nonce is read from the record, not assumed. Returns the
    /// plaintext, or `None` if the record is too short or does not
    /// authenticate.
    pub(crate) fn open(&self, record: &Record<'_>) -> Option<Zeroizing<Vec<u8>>> {
        let (explicit, rest) = record.body.split_first_chunk::<EXPLICIT_NONCE>()?;
        let (ciphertext, tag) = rest.split_at_checked(rest.len().checked_sub(TAG)?)?;
        let tag: [u8; TAG] = tag.try_into().ok()?;
        let aad = additional_data(
            record.content_type,
            record.epoch,
            record.seq,
            ciphertext.len(),
        )
        .ok()?;
        let mut plain = Zeroizing::new(ciphertext.to_vec());
        self.cipher
            .decrypt_inout_detached(
                &self.nonce(explicit),
                &aad,
                plain.as_mut_slice().into(),
                &Array::from(tag),
            )
            .ok()?;
        Some(plain)
    }
}

/// The anti-replay window of RFC 6347 section 4.1.2.6, 64 records wide,
/// for one authenticated epoch. [`ReplayWindow::check`] comes before
/// decryption and [`ReplayWindow::accept`] only after the record
/// authenticated.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReplayWindow {
    /// Highest accepted sequence number, if any.
    top: Option<u64>,
    /// Bit `i` set: `top - i` was accepted.
    seen: u64,
}

impl ReplayWindow {
    /// Whether `seq` is new and not behind the window.
    pub(crate) fn check(&self, seq: u64) -> bool {
        let Some(top) = self.top else {
            return true;
        };
        if seq > top {
            return true;
        }
        let behind = top - seq;
        behind < 64 && self.seen & (1u64 << behind) == 0
    }

    /// Records `seq` as received. Call only after [`Self::check`] and
    /// authentication.
    pub(crate) fn accept(&mut self, seq: u64) {
        match self.top {
            None => {
                self.top = Some(seq);
                self.seen = 1;
            }
            Some(top) if seq > top => {
                let shift = seq - top;
                self.seen = if shift >= 64 { 0 } else { self.seen << shift };
                self.seen |= 1;
                self.top = Some(seq);
            }
            Some(top) => {
                let behind = top - seq;
                if behind < 64 {
                    self.seen |= 1u64 << behind;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_frame_and_stop_at_truncation() {
        let mut d = Vec::new();
        put_record(&mut d, HANDSHAKE, 0, 5, &[1, 2, 3]).expect("record");
        put_record(&mut d, ALERT, 1, MAX_SEQUENCE, &[]).expect("record");
        d.extend_from_slice(&[22, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 1]);
        let got: Vec<_> = records(&d).collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].seq, 5);
        assert_eq!(got[0].body, [1, 2, 3]);
        assert_eq!(got[1].epoch, 1);
        assert_eq!(got[1].seq, MAX_SEQUENCE);
        assert_eq!(records(&d[..12]).count(), 0);
        assert!(put_record(&mut d, ALERT, 0, MAX_SEQUENCE + 1, &[]).is_err());
    }

    #[test]
    fn replay_window_follows_the_rfc() {
        let mut w = ReplayWindow::default();
        assert!(w.check(10));
        w.accept(10);
        assert!(!w.check(10), "duplicate");
        assert!(w.check(9), "behind but unseen");
        w.accept(9);
        assert!(!w.check(9));
        assert!(w.check(100));
        w.accept(100);
        assert!(!w.check(36), "64 behind is outside the window");
        assert!(w.check(37), "63 behind is inside");
        w.accept(37);
        assert!(!w.check(37));
        assert!(!w.check(100));
        w.accept(1000);
        assert!(!w.check(100), "a jump past 64 forgets everything behind");
        assert!(w.check(999));
    }

    #[test]
    fn gcm_round_trip_and_tamper() {
        let p = Protection::new(&[7u8; 16], &[1, 2, 3, 4]).expect("keys");
        let mut out = Vec::new();
        p.seal(&mut out, HANDSHAKE, 1, 3, b"finished")
            .expect("seal");
        let rec = records(&out).next().expect("record");
        assert_eq!(rec.body.len(), 8 + GCM_OVERHEAD);
        assert_eq!(&rec.body[..8], &[0, 1, 0, 0, 0, 0, 0, 3]);
        assert_eq!(p.open(&rec).expect("open").as_slice(), b"finished");
        // The AAD covers type, epoch and sequence number.
        for altered in [
            Record {
                content_type: ALERT,
                ..rec
            },
            Record { seq: 4, ..rec },
            Record { epoch: 2, ..rec },
        ] {
            assert!(p.open(&altered).is_none());
        }
        let mut body = rec.body.to_vec();
        body[9] ^= 1;
        assert!(p.open(&Record { body: &body, ..rec }).is_none());
        assert!(
            p.open(&Record {
                body: &body[..23],
                ..rec
            })
            .is_none()
        );
    }
}
