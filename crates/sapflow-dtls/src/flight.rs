//! Our outgoing flights (RFC 6347 section 4.2.4). A flight is kept as
//! plaintext handshake messages and turned into records afresh on every
//! send, so each retransmission has new record sequence numbers (and, at
//! epoch 1, a new nonce). Messages are fragmented to the MTU (section
//! 4.2.3) and records are packed into datagrams of at most the MTU.

use crate::message::{HEADER_LEN, header};
use crate::record::{self, CHANGE_CIPHER_SPEC, GCM_OVERHEAD, HANDSHAKE, MAX_SEQUENCE, Protection};
use crate::{Error, Limit};

/// One handshake message we send.
#[derive(Debug, Clone)]
pub(crate) struct OutMessage {
    pub(crate) msg_type: u8,
    pub(crate) seq: u16,
    pub(crate) body: Vec<u8>,
}

impl OutMessage {
    /// The message as one unfragmented handshake structure, as hashed.
    pub(crate) fn whole(&self) -> Result<Vec<u8>, Error> {
        let n = self.body.len();
        let mut m = Vec::with_capacity(HEADER_LEN + n);
        m.extend_from_slice(&header(self.msg_type, self.seq, 0, n, n)?);
        m.extend_from_slice(&self.body);
        Ok(m)
    }
}

/// A flight: epoch 0 handshake messages and, for the last flight,
/// ChangeCipherSpec and an epoch 1 Finished.
#[derive(Debug, Clone, Default)]
pub(crate) struct Flight {
    pub(crate) messages: Vec<OutMessage>,
    pub(crate) finished: Option<OutMessage>,
}

/// Next record sequence number per epoch.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Sequences {
    pub(crate) epoch0: u64,
    pub(crate) epoch1: u64,
}

impl Sequences {
    pub(crate) fn take(&mut self, epoch: u16) -> Result<u64, Error> {
        let slot = if epoch == 0 {
            &mut self.epoch0
        } else {
            &mut self.epoch1
        };
        let seq = *slot;
        if seq > MAX_SEQUENCE {
            return Err(Error::Limit(Limit::Sequence));
        }
        *slot = seq + 1;
        Ok(seq)
    }
}

/// Records for one flight, then packed into datagrams.
pub(crate) fn render(
    flight: &Flight,
    mtu: usize,
    seqs: &mut Sequences,
    write: Option<&Protection>,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut records: Vec<Vec<u8>> = Vec::new();
    let max_fragment = mtu
        .checked_sub(record::HEADER_LEN + HEADER_LEN)
        .filter(|&m| m > 0)
        .ok_or(Error::Internal)?;
    for m in &flight.messages {
        let n = m.body.len();
        let mut offset = 0;
        loop {
            let len = (n - offset).min(max_fragment);
            let mut body = Vec::with_capacity(HEADER_LEN + len);
            body.extend_from_slice(&header(m.msg_type, m.seq, offset, len, n)?);
            body.extend_from_slice(m.body.get(offset..offset + len).ok_or(Error::Internal)?);
            let mut rec = Vec::with_capacity(record::HEADER_LEN + body.len());
            record::put_record(&mut rec, HANDSHAKE, 0, seqs.take(0)?, &body)?;
            records.push(rec);
            offset += len;
            if offset >= n {
                break;
            }
        }
    }
    if let Some(fin) = &flight.finished {
        let mut ccs = Vec::with_capacity(record::HEADER_LEN + 1);
        record::put_record(&mut ccs, CHANGE_CIPHER_SPEC, 0, seqs.take(0)?, &[1])?;
        records.push(ccs);
        let plain = fin.whole()?;
        if record::HEADER_LEN + GCM_OVERHEAD + plain.len() > mtu {
            return Err(Error::Internal);
        }
        let write = write.ok_or(Error::Internal)?;
        let mut rec = Vec::new();
        write.seal(&mut rec, HANDSHAKE, 1, seqs.take(1)?, &plain)?;
        records.push(rec);
    }
    Ok(pack(records, mtu))
}

/// Packs records, in order, into as few datagrams of at most `mtu` bytes
/// as keeps the order.
pub(crate) fn pack(records: Vec<Vec<u8>>, mtu: usize) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for r in records {
        match out.last_mut() {
            Some(d) if d.len() + r.len() <= mtu => d.extend_from_slice(&r),
            _ => out.push(r),
        }
    }
    out
}

/// One alert record.
pub(crate) fn alert(
    level: u8,
    description: u8,
    seqs: &mut Sequences,
    write: Option<&Protection>,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(record::HEADER_LEN + GCM_OVERHEAD + 2);
    match write {
        Some(p) => p.seal(
            &mut out,
            record::ALERT,
            1,
            seqs.take(1)?,
            &[level, description],
        )?,
        None => record::put_record(
            &mut out,
            record::ALERT,
            0,
            seqs.take(0)?,
            &[level, description],
        )?,
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::fragments;
    use crate::record::records;

    #[test]
    fn fragments_to_the_mtu_and_packs() {
        let flight = Flight {
            messages: vec![
                OutMessage {
                    msg_type: 2,
                    seq: 0,
                    body: vec![1; 70],
                },
                OutMessage {
                    msg_type: 11,
                    seq: 1,
                    body: vec![2; 600],
                },
                OutMessage {
                    msg_type: 14,
                    seq: 2,
                    body: vec![],
                },
            ],
            finished: None,
        };
        let mut seqs = Sequences::default();
        let grams = render(&flight, 256, &mut seqs, None).expect("render");
        assert!(grams.iter().all(|d| d.len() <= 256));
        let mut cert = vec![0u8; 600];
        let mut covered = 0;
        let mut expected_seq = 0;
        for d in &grams {
            for r in records(d) {
                assert_eq!(r.seq, expected_seq);
                expected_seq += 1;
                for f in fragments(r.body) {
                    if f.msg_type == 11 {
                        assert_eq!(f.length, 600);
                        let o = f.offset as usize;
                        cert[o..o + f.data.len()].copy_from_slice(f.data);
                        covered += f.data.len();
                    }
                }
            }
        }
        assert_eq!(covered, 600);
        assert_eq!(cert, vec![2; 600]);
        assert_eq!(seqs.epoch0, expected_seq);
        // Sending again uses new sequence numbers.
        let again = render(&flight, 256, &mut seqs, None).expect("render");
        assert_eq!(records(&again[0]).next().expect("record").seq, expected_seq);
    }

    #[test]
    fn sequence_numbers_do_not_wrap() {
        let mut seqs = Sequences {
            epoch0: MAX_SEQUENCE,
            epoch1: 0,
        };
        assert_eq!(seqs.take(0), Ok(MAX_SEQUENCE));
        assert_eq!(seqs.take(0), Err(Error::Limit(Limit::Sequence)));
    }
}
