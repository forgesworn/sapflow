//! RFC 4571 framing for passive ICE-TCP (RFC 6544): each STUN, DTLS, RTP
//! or RTCP packet is preceded by its length as two bytes, big-endian.
//!
//! [`Decoder`] takes bytes as a TCP stream gives them and returns whole
//! frames, holding at most one partial frame plus what the caller pushed.
//! A frame longer than [`Framing::max_frame`] (1500 bytes, the same cap as
//! a UDP datagram) ends the stream: the framing cannot be trusted after it.
//! Empty frames carry nothing and are skipped. [`encode`] frames one packet
//! for sending. The TCP listener and PROXY protocol come in W3d.

use std::fmt;

/// Bounds for a framed stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framing {
    /// Longest frame accepted or sent, in bytes. 1 to 65535; default 1500.
    pub max_frame: usize,
    /// Most bytes held between [`Decoder::push`] and [`Decoder::pop`]. At
    /// least `max_frame + 2`; default 64 KiB.
    pub max_buffered: usize,
}

impl Default for Framing {
    fn default() -> Self {
        Self {
            max_frame: 1500,
            max_buffered: 64 * 1024,
        }
    }
}

impl Framing {
    /// Checks the ranges.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=65535).contains(&self.max_frame) {
            return Err("max_frame");
        }
        if self.max_buffered < self.max_frame + 2 || self.max_buffered > 1 << 20 {
            return Err("max_buffered");
        }
        Ok(())
    }
}

/// Why a stream's framing failed. Close the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramingError {
    /// A frame declared more than [`Framing::max_frame`] bytes.
    FrameTooLong(usize),
    /// More than [`Framing::max_buffered`] bytes would be held.
    Buffered,
    /// [`encode`] was given a packet longer than the cap.
    PacketTooLong,
}

impl fmt::Display for FramingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RFC 4571 framing: {self:?}")
    }
}

impl std::error::Error for FramingError {}

/// Splits a byte stream into frames.
#[derive(Debug, Clone)]
pub struct Decoder {
    framing: Framing,
    buf: Vec<u8>,
    /// Where the next unread byte is.
    at: usize,
    failed: Option<FramingError>,
}

impl Decoder {
    /// A decoder within `framing`'s bounds.
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            buf: Vec::new(),
            at: 0,
            failed: None,
        }
    }

    /// Bytes held, read or not yet.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.at
    }

    /// Adds bytes from the stream. Fails, for good, once more than
    /// [`Framing::max_buffered`] would be held: pop frames between pushes.
    pub fn push(&mut self, data: &[u8]) -> Result<(), FramingError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        if self.at > 0 {
            self.buf.drain(..self.at);
            self.at = 0;
        }
        if self.buf.len() + data.len() > self.framing.max_buffered {
            self.failed = Some(FramingError::Buffered);
            return Err(FramingError::Buffered);
        }
        self.buf.extend_from_slice(data);
        Ok(())
    }

    /// The next whole frame, or `None` until more bytes arrive. An error is
    /// final.
    pub fn pop(&mut self) -> Result<Option<Vec<u8>>, FramingError> {
        loop {
            if let Some(e) = self.failed {
                return Err(e);
            }
            let rest = self.buf.get(self.at..).unwrap_or_default();
            let Some(&[hi, lo]) = rest.first_chunk::<2>() else {
                return Ok(None);
            };
            let len = usize::from(u16::from_be_bytes([hi, lo]));
            if len > self.framing.max_frame {
                self.failed = Some(FramingError::FrameTooLong(len));
                return Err(FramingError::FrameTooLong(len));
            }
            let Some(frame) = rest.get(2..2 + len) else {
                return Ok(None);
            };
            let frame = frame.to_vec();
            self.at += 2 + len;
            if !frame.is_empty() {
                return Ok(Some(frame));
            }
        }
    }
}

/// Appends `packet` to `out` as one frame.
pub fn encode(framing: &Framing, packet: &[u8], out: &mut Vec<u8>) -> Result<(), FramingError> {
    let len = u16::try_from(packet.len()).map_err(|_| FramingError::PacketTooLong)?;
    if packet.len() > framing.max_frame {
        return Err(FramingError::PacketTooLong);
    }
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(packet);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framed(packets: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in packets {
            encode(&Framing::default(), p, &mut out).expect("frames");
        }
        out
    }

    #[test]
    fn frames_survive_any_split() {
        let packets: [&[u8]; 4] = [b"stun", &[0x16; 300], b"", &[0x80; 1500]];
        let stream = framed(&packets);
        for chunk in [1, 2, 3, 7, 100, stream.len()] {
            let mut d = Decoder::new(Framing::default());
            let mut got = Vec::new();
            for c in stream.chunks(chunk) {
                d.push(c).expect("frames");
                while let Some(f) = d.pop().expect("frames") {
                    got.push(f);
                }
            }
            let want: Vec<Vec<u8>> = packets
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.to_vec())
                .collect();
            assert_eq!(got, want, "chunk {chunk}");
            assert_eq!(d.buffered(), 0);
        }
    }

    #[test]
    fn too_long_frame_ends_the_stream() {
        let mut d = Decoder::new(Framing::default());
        d.push(&1501u16.to_be_bytes()).expect("frames");
        assert_eq!(d.pop(), Err(FramingError::FrameTooLong(1501)));
        assert_eq!(d.push(b"more"), Err(FramingError::FrameTooLong(1501)));
        assert_eq!(d.pop(), Err(FramingError::FrameTooLong(1501)));
    }

    #[test]
    fn buffering_is_bounded() {
        let framing = Framing {
            max_frame: 1500,
            max_buffered: 1502,
        };
        let mut d = Decoder::new(framing);
        d.push(&[0x05, 0xdc]).expect("frames");
        d.push(&[0; 1500]).expect("frames");
        assert_eq!(d.pop().expect("frames").map(|f| f.len()), Some(1500));
        // Popped bytes are released on the next push.
        d.push(&[0; 1502]).expect("frames");
        assert_eq!(d.push(&[0]), Err(FramingError::Buffered));
    }

    #[test]
    fn encode_refuses_long_packets() {
        let mut out = Vec::new();
        let f = Framing::default();
        assert_eq!(
            encode(&f, &[0; 1501], &mut out),
            Err(FramingError::PacketTooLong)
        );
        let wide = Framing {
            max_frame: 65535,
            max_buffered: 1 << 20,
        };
        assert_eq!(
            encode(&wide, &[0; 65536], &mut out),
            Err(FramingError::PacketTooLong)
        );
        assert!(out.is_empty());
        assert!(f.validate().is_ok() && wide.validate().is_ok());
        assert!(Framing { max_frame: 0, ..f }.validate().is_err());
        assert!(
            Framing {
                max_buffered: 100,
                ..f
            }
            .validate()
            .is_err()
        );
    }
}
