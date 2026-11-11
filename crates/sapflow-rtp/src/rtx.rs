//! RTX retransmission payloads (RFC 4588 section 4).
//!
//! An RTX packet travels on its own SSRC and payload type, with its own
//! sequence numbers. Its payload starts with the original sequence number
//! (OSN) and then carries the original payload; the timestamp and marker
//! are the original's. Which RTX SSRC and payload type belong to which
//! media stream comes from the SDP (`a=ssrc-group:FID` and `apt=`), so the
//! caller supplies that mapping (the `receive` module's `RtxConfig`).

use std::fmt;

/// Why an RTX payload was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtxError {
    /// One byte: not even a whole OSN.
    Truncated,
}
impl fmt::Display for RtxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RTX: {self:?}")
    }
}
impl std::error::Error for RtxError {}

/// What an RTX payload holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rtx<'a> {
    /// No payload at all: a padding-only packet, which senders use to
    /// probe bandwidth. Nothing to retransmit.
    Padding,
    /// A retransmission of the packet with this original sequence number.
    Original {
        /// The original sequence number.
        sequence: u16,
        /// The original payload.
        payload: &'a [u8],
    },
}

/// Reads an RTX payload (RTP padding already removed).
pub fn unwrap(payload: &[u8]) -> Result<Rtx<'_>, RtxError> {
    match payload {
        [] => Ok(Rtx::Padding),
        [_] => Err(RtxError::Truncated),
        [hi, lo, rest @ ..] => Ok(Rtx::Original {
            sequence: u16::from_be_bytes([*hi, *lo]),
            payload: rest,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp::{RtpBuilder, RtpPacket};

    #[test]
    fn osn_and_payload() {
        let rtx = RtpBuilder {
            payload_type: 97,
            sequence: 4,
            timestamp: 9000,
            ssrc: 0xbeef,
            marker: true,
            payload: vec![0xff, 0xfe, 0x7c, 0x85, 1, 2],
            ..RtpBuilder::default()
        }
        .build();
        let p = RtpPacket::parse(&rtx).expect("parses");
        assert_eq!(
            unwrap(p.payload()),
            Ok(Rtx::Original {
                sequence: 0xfffe,
                payload: &[0x7c, 0x85, 1, 2]
            })
        );
        // Padding-only probes carry nothing.
        let probe = RtpBuilder {
            payload_type: 97,
            padding: 200,
            ..RtpBuilder::default()
        }
        .build();
        let p = RtpPacket::parse(&probe).expect("parses");
        assert_eq!(unwrap(p.payload()), Ok(Rtx::Padding));
        assert_eq!(unwrap(&[1]), Err(RtxError::Truncated));
        assert_eq!(
            unwrap(&[0, 7]),
            Ok(Rtx::Original {
                sequence: 7,
                payload: &[]
            })
        );
    }
}
