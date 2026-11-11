//! RTP packets (RFC 3550 section 5.1) with RFC 8285 header extensions.
//!
//! [`RtpPacket::parse`] checks the fixed header, the CSRC list, the
//! extension block and the padding against the buffer, and then gives a
//! borrowed view: nothing is copied. Every length comes from the packet and
//! is checked before use.
//!
//! Extension elements are read in the one-byte (`0xBEDE`, RFC 8285 section
//! 4.2) and two-byte (`0x100x`, section 4.3) forms. An extension block with
//! any other profile is kept as raw bytes and has no elements. Within a
//! block, parsing stops at the end, at a one-byte ID 15 or ID 0 with a
//! nonzero length (section 4.1.2), or at an element whose length runs past
//! the block; only the elements before that point are seen.

use std::fmt;

/// The RTP version this crate accepts.
pub const VERSION: u8 = 2;
/// Length of the fixed RTP header.
pub const FIXED_HEADER_LEN: usize = 12;
/// Profile value of the one-byte extension form (RFC 8285 section 4.2).
pub const ONE_BYTE_PROFILE: u16 = 0xBEDE;

/// Why an RTP packet was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpError {
    /// Shorter than the fixed header.
    TooShort,
    /// The version field is not 2.
    Version(u8),
    /// The CSRC list runs past the end of the packet.
    Csrc,
    /// The extension header or its body runs past the end of the packet.
    Extension,
    /// The padding count is zero or larger than the payload.
    Padding,
}
impl fmt::Display for RtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RTP: {self:?}")
    }
}
impl std::error::Error for RtpError {}

pub(crate) fn be16(buf: &[u8], at: usize) -> Option<u16> {
    let b = buf.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([*b.first()?, *b.get(1)?]))
}
pub(crate) fn be32(buf: &[u8], at: usize) -> Option<u32> {
    let b = buf.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([
        *b.first()?,
        *b.get(1)?,
        *b.get(2)?,
        *b.get(3)?,
    ]))
}

/// Where the header ends and, if present, where the extension block is.
struct Layout {
    header_len: usize,
    /// Extension profile, start and end of its body.
    extension: Option<(u16, usize, usize)>,
}

fn layout(buf: &[u8]) -> Result<Layout, RtpError> {
    let first = *buf.first().ok_or(RtpError::TooShort)?;
    if buf.len() < FIXED_HEADER_LEN {
        return Err(RtpError::TooShort);
    }
    let version = first >> 6;
    if version != VERSION {
        return Err(RtpError::Version(version));
    }
    let csrc_end = FIXED_HEADER_LEN + 4 * usize::from(first & 0x0f);
    if csrc_end > buf.len() {
        return Err(RtpError::Csrc);
    }
    if first & 0x10 == 0 {
        return Ok(Layout {
            header_len: csrc_end,
            extension: None,
        });
    }
    let profile = be16(buf, csrc_end).ok_or(RtpError::Extension)?;
    let words = be16(buf, csrc_end + 2).ok_or(RtpError::Extension)?;
    let start = csrc_end + 4;
    let end = start + 4 * usize::from(words);
    if end > buf.len() {
        return Err(RtpError::Extension);
    }
    Ok(Layout {
        header_len: end,
        extension: Some((profile, start, end)),
    })
}

/// Length of the RTP header (fixed part, CSRCs and extension) at the start
/// of `buf`. Used by SRTP, where the payload and padding are still
/// encrypted and so cannot be checked yet.
pub fn header_len(buf: &[u8]) -> Result<usize, RtpError> {
    layout(buf).map(|l| l.header_len)
}

/// A borrowed, checked view of one RTP packet.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    buf: &'a [u8],
    header_len: usize,
    payload_end: usize,
    extension: Option<Extension<'a>>,
}
impl fmt::Debug for RtpPacket<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtpPacket")
            .field("payload_type", &self.payload_type())
            .field("sequence", &self.sequence())
            .field("timestamp", &self.timestamp())
            .field("ssrc", &self.ssrc())
            .field("marker", &self.marker())
            .field("payload_len", &self.payload().len())
            .finish()
    }
}

impl<'a> RtpPacket<'a> {
    /// Checks `buf` as one RTP packet. The padding, when the P bit is set,
    /// is validated and left out of [`payload`](Self::payload).
    pub fn parse(buf: &'a [u8]) -> Result<Self, RtpError> {
        let layout = layout(buf)?;
        let padded = buf.first().is_some_and(|b| b & 0x20 != 0);
        let payload_end = if padded {
            let count = usize::from(*buf.last().ok_or(RtpError::Padding)?);
            if count == 0 || count > buf.len() - layout.header_len {
                return Err(RtpError::Padding);
            }
            buf.len() - count
        } else {
            buf.len()
        };
        let extension = match layout.extension {
            Some((profile, start, end)) => Some(Extension {
                profile,
                data: buf.get(start..end).ok_or(RtpError::Extension)?,
            }),
            None => None,
        };
        Ok(Self {
            buf,
            header_len: layout.header_len,
            payload_end,
            extension,
        })
    }

    fn byte(&self, at: usize) -> u8 {
        self.buf.get(at).copied().unwrap_or(0)
    }
    /// The padding (P) bit.
    pub fn has_padding(&self) -> bool {
        self.byte(0) & 0x20 != 0
    }
    /// Padding octets removed from the end, including the count octet.
    pub fn padding_len(&self) -> usize {
        self.buf.len() - self.payload_end
    }
    /// The extension (X) bit.
    pub fn has_extension(&self) -> bool {
        self.extension.is_some()
    }
    /// The marker bit.
    pub fn marker(&self) -> bool {
        self.byte(1) & 0x80 != 0
    }
    /// Payload type, 0 to 127.
    pub fn payload_type(&self) -> u8 {
        self.byte(1) & 0x7f
    }
    /// Sequence number.
    pub fn sequence(&self) -> u16 {
        be16(self.buf, 2).unwrap_or(0)
    }
    /// RTP timestamp.
    pub fn timestamp(&self) -> u32 {
        be32(self.buf, 4).unwrap_or(0)
    }
    /// Synchronisation source.
    pub fn ssrc(&self) -> u32 {
        be32(self.buf, 8).unwrap_or(0)
    }
    /// Number of CSRCs.
    pub fn csrc_count(&self) -> usize {
        usize::from(self.byte(0) & 0x0f)
    }
    /// The contributing sources, in order.
    pub fn csrcs(&self) -> impl Iterator<Item = u32> + 'a {
        let buf = self.buf;
        (0..self.csrc_count()).filter_map(move |n| be32(buf, FIXED_HEADER_LEN + 4 * n))
    }
    /// The header extension block, if the X bit is set.
    pub fn extension(&self) -> Option<Extension<'a>> {
        self.extension
    }
    /// The data of the first RFC 8285 element with this local ID, if any.
    pub fn extension_element(&self, id: u8) -> Option<&'a [u8]> {
        self.extension?.get(id)
    }
    /// Length of the header: fixed part, CSRCs and extension.
    pub fn header_len(&self) -> usize {
        self.header_len
    }
    /// The payload, without padding.
    pub fn payload(&self) -> &'a [u8] {
        self.buf
            .get(self.header_len..self.payload_end)
            .unwrap_or_default()
    }
    /// The whole packet as received.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.buf
    }
}

/// Which RFC 8285 form an extension block uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionForm {
    /// `0xBEDE`: one-byte element headers.
    OneByte,
    /// `0x100` and four application bits: two-byte element headers.
    TwoByte {
        /// The application-dependent low four bits of the profile.
        appbits: u8,
    },
    /// Any other profile value; its body is opaque.
    Other(u16),
}

/// The header extension block of an RTP packet (RFC 3550 section 5.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extension<'a> {
    profile: u16,
    data: &'a [u8],
}
impl<'a> Extension<'a> {
    /// The 16-bit "defined by profile" value.
    pub fn profile(&self) -> u16 {
        self.profile
    }
    /// The RFC 8285 form, from the profile value.
    pub fn form(&self) -> ExtensionForm {
        if self.profile == ONE_BYTE_PROFILE {
            ExtensionForm::OneByte
        } else if self.profile & 0xfff0 == 0x1000 {
            ExtensionForm::TwoByte {
                appbits: (self.profile & 0x0f) as u8,
            }
        } else {
            ExtensionForm::Other(self.profile)
        }
    }
    /// The block body, after its four-byte header.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    /// The RFC 8285 elements, in order. Empty for [`ExtensionForm::Other`].
    pub fn elements(&self) -> Elements<'a> {
        Elements {
            data: self.data,
            pos: 0,
            two_byte: match self.form() {
                ExtensionForm::OneByte => Some(false),
                ExtensionForm::TwoByte { .. } => Some(true),
                ExtensionForm::Other(_) => None,
            },
        }
    }
    /// The data of the first element with local ID `id`.
    pub fn get(&self, id: u8) -> Option<&'a [u8]> {
        self.elements().find(|e| e.id == id).map(|e| e.data)
    }
}

/// One RFC 8285 extension element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element<'a> {
    /// Local identifier: 1 to 14 in the one-byte form, 1 to 255 in the
    /// two-byte form.
    pub id: u8,
    /// Element data: 1 to 16 bytes (one-byte form) or 0 to 255 (two-byte).
    pub data: &'a [u8],
}

/// Iterator over the elements of an extension block.
#[derive(Debug, Clone)]
pub struct Elements<'a> {
    data: &'a [u8],
    pos: usize,
    /// `None` for an opaque profile.
    two_byte: Option<bool>,
}
impl<'a> Iterator for Elements<'a> {
    type Item = Element<'a>;
    fn next(&mut self) -> Option<Element<'a>> {
        let two_byte = self.two_byte?;
        loop {
            let Some(&head) = self.data.get(self.pos) else {
                self.two_byte = None;
                return None;
            };
            if head == 0 {
                // Padding byte, in either form.
                self.pos += 1;
                continue;
            }
            let (id, len, start) = if two_byte {
                let Some(&len) = self.data.get(self.pos + 1) else {
                    self.two_byte = None;
                    return None;
                };
                (head, usize::from(len), self.pos + 2)
            } else {
                let id = head >> 4;
                if id == 0 || id == 15 {
                    // ID 0 with a length, or the reserved ID 15: stop.
                    self.two_byte = None;
                    return None;
                }
                (id, usize::from(head & 0x0f) + 1, self.pos + 1)
            };
            let Some(data) = self.data.get(start..start + len) else {
                self.two_byte = None;
                return None;
            };
            self.pos = start + len;
            return Some(Element { id, data });
        }
    }
}

/// Builds RTP packets for tests and fuzz seeds.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone, Default)]
pub struct RtpBuilder {
    /// Marker bit.
    pub marker: bool,
    /// Payload type.
    pub payload_type: u8,
    /// Sequence number.
    pub sequence: u16,
    /// Timestamp.
    pub timestamp: u32,
    /// SSRC.
    pub ssrc: u32,
    /// CSRCs (at most 15 are written).
    pub csrcs: Vec<u32>,
    /// Extension profile and raw body (padded to a whole word).
    pub extension: Option<(u16, Vec<u8>)>,
    /// Payload.
    pub payload: Vec<u8>,
    /// Padding octets to add, including the count octet (0 for none).
    pub padding: u8,
}
#[cfg(any(test, feature = "testing"))]
impl RtpBuilder {
    /// Serialises the packet.
    pub fn build(&self) -> Vec<u8> {
        let csrcs = self.csrcs.len().min(15);
        let mut out = Vec::with_capacity(64 + self.payload.len());
        let mut first = VERSION << 6 | csrcs as u8;
        if self.padding > 0 {
            first |= 0x20;
        }
        if self.extension.is_some() {
            first |= 0x10;
        }
        out.push(first);
        out.push(u8::from(self.marker) << 7 | (self.payload_type & 0x7f));
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.ssrc.to_be_bytes());
        for csrc in self.csrcs.iter().take(15) {
            out.extend_from_slice(&csrc.to_be_bytes());
        }
        if let Some((profile, body)) = &self.extension {
            let words = body.len().div_ceil(4);
            out.extend_from_slice(&profile.to_be_bytes());
            out.extend_from_slice(&(words as u16).to_be_bytes());
            out.extend_from_slice(body);
            out.resize(out.len() + words * 4 - body.len(), 0);
        }
        out.extend_from_slice(&self.payload);
        if self.padding > 0 {
            out.resize(out.len() + usize::from(self.padding) - 1, 0);
            out.push(self.padding);
        }
        out
    }

    /// One-byte-form extension body from `(id, data)` pairs (data 1 to 16
    /// bytes each).
    pub fn one_byte_elements(elements: &[(u8, &[u8])]) -> (u16, Vec<u8>) {
        let mut body = Vec::new();
        for (id, data) in elements {
            body.push(id << 4 | (data.len().saturating_sub(1) as u8 & 0x0f));
            body.extend_from_slice(data);
        }
        (ONE_BYTE_PROFILE, body)
    }

    /// Two-byte-form extension body from `(id, data)` pairs.
    pub fn two_byte_elements(appbits: u8, elements: &[(u8, &[u8])]) -> (u16, Vec<u8>) {
        let mut body = Vec::new();
        for (id, data) in elements {
            body.push(*id);
            body.push(data.len() as u8);
            body.extend_from_slice(data);
        }
        (0x1000 | u16::from(appbits & 0x0f), body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic() -> RtpBuilder {
        RtpBuilder {
            marker: true,
            payload_type: 96,
            sequence: 0xf17b,
            timestamp: 0x8041_f8d3,
            ssrc: 0x5501_a0b2,
            payload: b"Gallia est omnis divisa in partes tres".to_vec(),
            ..RtpBuilder::default()
        }
    }

    #[test]
    fn fixed_header_fields() {
        let bytes = basic().build();
        let p = RtpPacket::parse(&bytes).expect("parses");
        assert!(p.marker());
        assert_eq!(p.payload_type(), 96);
        assert_eq!(p.sequence(), 0xf17b);
        assert_eq!(p.timestamp(), 0x8041_f8d3);
        assert_eq!(p.ssrc(), 0x5501_a0b2);
        assert_eq!(p.header_len(), 12);
        assert_eq!(p.payload(), b"Gallia est omnis divisa in partes tres");
        assert!(!p.has_extension() && !p.has_padding());
        assert_eq!(p.csrcs().count(), 0);
    }

    #[test]
    fn rfc7714_packet_header_reads() {
        // RFC 7714 section 16: header 8040f17b 8041f8d3 5501a0b2.
        let bytes = [
            0x80, 0x40, 0xf1, 0x7b, 0x80, 0x41, 0xf8, 0xd3, 0x55, 0x01, 0xa0, 0xb2, 0x47,
        ];
        let p = RtpPacket::parse(&bytes).expect("parses");
        assert_eq!(p.payload_type(), 64);
        assert!(!p.marker());
        assert_eq!(p.sequence(), 0xf17b);
        assert_eq!(p.payload(), &[0x47]);
    }

    #[test]
    fn csrcs_and_padding() {
        let mut b = basic();
        b.csrcs = vec![1, 2, 3];
        b.padding = 4;
        let bytes = b.build();
        let p = RtpPacket::parse(&bytes).expect("parses");
        assert_eq!(p.csrcs().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(p.header_len(), 24);
        assert_eq!(p.padding_len(), 4);
        assert_eq!(p.payload(), b"Gallia est omnis divisa in partes tres");
    }

    #[test]
    fn padding_that_eats_only_the_whole_payload_is_allowed() {
        let mut b = basic();
        b.payload.clear();
        b.padding = 8;
        let bytes = b.build();
        let p = RtpPacket::parse(&bytes).expect("parses");
        assert!(p.payload().is_empty());
    }

    #[test]
    fn bad_padding_refused() {
        let mut bytes = basic().build();
        bytes[0] |= 0x20;
        *bytes.last_mut().expect("payload") = 0;
        assert_eq!(RtpPacket::parse(&bytes), Err(RtpError::Padding));
        let len = bytes.len() - 12;
        *bytes.last_mut().expect("payload") = (len + 1) as u8;
        assert_eq!(RtpPacket::parse(&bytes), Err(RtpError::Padding));
        // Padding may not reach into the header.
        let mut header_only = basic().build();
        header_only.truncate(12);
        header_only[0] |= 0x20;
        assert_eq!(RtpPacket::parse(&header_only), Err(RtpError::Padding));
    }

    #[test]
    fn one_byte_extensions() {
        let mut b = basic();
        // RFC 8285 section 4.2 example shape: elements of 1 and 2 bytes,
        // two padding bytes, then one of 4.
        b.extension = Some((
            ONE_BYTE_PROFILE,
            vec![0x10, 0xaa, 0x21, 0xbb, 0xcc, 0, 0, 0x33, 1, 2, 3, 4],
        ));
        let bytes = b.build();
        let p = RtpPacket::parse(&bytes).expect("parses");
        let ext = p.extension().expect("extension");
        assert_eq!(ext.form(), ExtensionForm::OneByte);
        let all: Vec<_> = ext.elements().map(|e| (e.id, e.data.to_vec())).collect();
        assert_eq!(
            all,
            [
                (1, vec![0xaa]),
                (2, vec![0xbb, 0xcc]),
                (3, vec![1, 2, 3, 4])
            ]
        );
        assert_eq!(p.extension_element(2), Some(&[0xbb, 0xcc][..]));
        assert_eq!(p.extension_element(4), None);
        assert_eq!(p.header_len(), 12 + 4 + 12);
        assert_eq!(p.payload(), b"Gallia est omnis divisa in partes tres");
    }

    #[test]
    fn one_byte_id15_and_id0_with_length_stop_parsing() {
        let ext = Extension {
            profile: ONE_BYTE_PROFILE,
            data: &[0x10, 0xaa, 0xf3, 0x20, 0xbb, 0, 0, 0],
        };
        assert_eq!(ext.elements().map(|e| e.id).collect::<Vec<_>>(), [1]);
        let ext = Extension {
            profile: ONE_BYTE_PROFILE,
            data: &[0x10, 0xaa, 0x02, 0x20, 0xbb, 0, 0, 0],
        };
        assert_eq!(ext.elements().map(|e| e.id).collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn truncated_element_stops_parsing() {
        let ext = Extension {
            profile: ONE_BYTE_PROFILE,
            data: &[0x10, 0xaa, 0x2f, 0xbb],
        };
        assert_eq!(ext.elements().map(|e| e.id).collect::<Vec<_>>(), [1]);
        let ext = Extension {
            profile: 0x1000,
            data: &[0x05, 0x01, 0xaa, 0x07],
        };
        assert_eq!(ext.elements().map(|e| e.id).collect::<Vec<_>>(), [5]);
    }

    #[test]
    fn two_byte_extensions() {
        let mut b = basic();
        let (profile, body) =
            RtpBuilder::two_byte_elements(3, &[(1, &[]), (200, &[9]), (17, &[1, 2, 3, 4])]);
        b.extension = Some((profile, body));
        let bytes = b.build();
        let p = RtpPacket::parse(&bytes).expect("parses");
        let ext = p.extension().expect("extension");
        assert_eq!(ext.form(), ExtensionForm::TwoByte { appbits: 3 });
        let all: Vec<_> = ext.elements().map(|e| (e.id, e.data.len())).collect();
        assert_eq!(all, [(1, 0), (200, 1), (17, 4)]);
        assert_eq!(p.extension_element(200), Some(&[9][..]));
    }

    #[test]
    fn other_profiles_are_opaque() {
        let mut b = basic();
        b.extension = Some((0xabcd, vec![0x10, 0xaa, 0, 0]));
        let bytes = b.build();
        let p = RtpPacket::parse(&bytes).expect("parses");
        let ext = p.extension().expect("extension");
        assert_eq!(ext.form(), ExtensionForm::Other(0xabcd));
        assert_eq!(ext.elements().count(), 0);
        assert_eq!(ext.data(), &[0x10, 0xaa, 0, 0]);
    }

    #[test]
    fn malformed_and_truncated_refused() {
        assert_eq!(RtpPacket::parse(&[]), Err(RtpError::TooShort));
        let good = basic().build();
        for len in 0..12 {
            assert_eq!(
                RtpPacket::parse(&good[..len]),
                Err(RtpError::TooShort),
                "{len}"
            );
        }
        let mut v1 = good.clone();
        v1[0] = 0x40;
        assert_eq!(RtpPacket::parse(&v1), Err(RtpError::Version(1)));
        let mut csrc = good[..20].to_vec();
        csrc[0] |= 0x03;
        assert_eq!(RtpPacket::parse(&csrc), Err(RtpError::Csrc));

        let mut b = basic();
        b.extension = Some(RtpBuilder::one_byte_elements(&[(1, &[1, 2, 3])]));
        let with_ext = b.build();
        // Cut inside the extension header and inside its body.
        for len in 12..20 {
            assert_eq!(
                RtpPacket::parse(&with_ext[..len]),
                Err(RtpError::Extension),
                "{len}"
            );
        }
        // An extension length that claims more words than there are.
        let mut long = with_ext.clone();
        long[14] = 0xff;
        long[15] = 0xff;
        assert_eq!(RtpPacket::parse(&long), Err(RtpError::Extension));
        assert_eq!(header_len(&with_ext), Ok(20));
    }

    #[test]
    fn every_prefix_and_every_bit_flip_is_safe() {
        let mut b = basic();
        b.csrcs = vec![7];
        b.extension = Some(RtpBuilder::one_byte_elements(&[(1, &[1]), (2, &[2, 2])]));
        b.padding = 3;
        let bytes = b.build();
        for len in 0..=bytes.len() {
            if let Ok(p) = RtpPacket::parse(&bytes[..len]) {
                let _ = p.extension().map(|e| e.elements().count());
                assert!(p.header_len() + p.payload().len() + p.padding_len() == len);
            }
        }
        for at in 0..bytes.len() {
            for bit in 0..8 {
                let mut m = bytes.clone();
                m[at] ^= 1 << bit;
                if let Ok(p) = RtpPacket::parse(&m) {
                    let _ = p.extension().map(|e| e.elements().count());
                    let _ = p.csrcs().count();
                    assert!(p.header_len() + p.payload().len() + p.padding_len() == m.len());
                }
            }
        }
    }
}
