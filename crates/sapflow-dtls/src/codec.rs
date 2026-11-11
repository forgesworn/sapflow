//! Bounded big-endian reading and length-prefixed writing (RFC 5246
//! section 4). Every read checks what is left; nothing indexes the input.

use crate::Error;

/// A cursor over borrowed input. Every method returns `None` rather than
/// reading past the end.
#[derive(Debug, Clone)]
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.buf.len() {
            return None;
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Some(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, tail) = self.buf.split_first_chunk::<N>()?;
        self.buf = tail;
        Some(*head)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        let (&b, tail) = self.buf.split_first()?;
        self.buf = tail;
        Some(b)
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.array::<2>().map(u16::from_be_bytes)
    }

    pub(crate) fn u24(&mut self) -> Option<u32> {
        let [a, b, c] = self.array::<3>()?;
        Some(u32::from_be_bytes([0, a, b, c]))
    }

    pub(crate) fn u48(&mut self) -> Option<u64> {
        let [a, b, c, d, e, f] = self.array::<6>()?;
        Some(u64::from_be_bytes([0, 0, a, b, c, d, e, f]))
    }

    /// `opaque x<0..2^8-1>`.
    pub(crate) fn vec8(&mut self) -> Option<&'a [u8]> {
        let n = self.u8()?;
        self.bytes(usize::from(n))
    }

    /// `opaque x<0..2^16-1>`.
    pub(crate) fn vec16(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()?;
        self.bytes(usize::from(n))
    }

    /// `opaque x<0..2^24-1>`.
    pub(crate) fn vec24(&mut self) -> Option<&'a [u8]> {
        let n = self.u24()?;
        self.bytes(usize::try_from(n).ok()?)
    }
}

pub(crate) fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub(crate) fn put_u24(out: &mut Vec<u8>, v: u32) -> Result<(), Error> {
    if v > 0x00ff_ffff {
        return Err(Error::Internal);
    }
    let [_, a, b, c] = v.to_be_bytes();
    out.extend_from_slice(&[a, b, c]);
    Ok(())
}

pub(crate) fn put_u48(out: &mut Vec<u8>, v: u64) -> Result<(), Error> {
    if v > 0xffff_ffff_ffff {
        return Err(Error::Internal);
    }
    let [_, _, a, b, c, d, e, f] = v.to_be_bytes();
    out.extend_from_slice(&[a, b, c, d, e, f]);
    Ok(())
}

pub(crate) fn put_vec8(out: &mut Vec<u8>, body: &[u8]) -> Result<(), Error> {
    out.push(u8::try_from(body.len()).map_err(|_| Error::Internal)?);
    out.extend_from_slice(body);
    Ok(())
}

pub(crate) fn put_vec16(out: &mut Vec<u8>, body: &[u8]) -> Result<(), Error> {
    put_u16(out, u16::try_from(body.len()).map_err(|_| Error::Internal)?);
    out.extend_from_slice(body);
    Ok(())
}

pub(crate) fn put_vec24(out: &mut Vec<u8>, body: &[u8]) -> Result<(), Error> {
    put_u24(out, u32::try_from(body.len()).map_err(|_| Error::Internal)?)?;
    out.extend_from_slice(body);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stop_at_the_end() {
        let mut r = Reader::new(&[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(r.u8(), Some(1));
        assert_eq!(r.u24(), Some(0x020304));
        assert_eq!(r.u48(), None);
        assert_eq!(r.u16(), Some(0x0506));
        assert_eq!(r.vec8(), None, "length 7 but nothing left");
        assert!(r.is_empty());
        let mut r = Reader::new(&[0, 2, 9, 9, 1]);
        assert_eq!(r.vec16(), Some(&[9, 9][..]));
        assert_eq!(r.u8(), Some(1));
        assert_eq!(Reader::new(&[0, 0, 5, 1]).vec24(), None);
    }

    #[test]
    fn writes_refuse_overflow() {
        let mut out = Vec::new();
        assert_eq!(put_u24(&mut out, 0x0100_0000), Err(Error::Internal));
        assert_eq!(put_u48(&mut out, 1 << 48), Err(Error::Internal));
        assert_eq!(put_vec8(&mut out, &[0; 256]), Err(Error::Internal));
        put_vec16(&mut out, &[7]).expect("fits");
        put_u48(&mut out, 0x0102_0304_0506).expect("fits");
        assert_eq!(out, [0, 1, 7, 1, 2, 3, 4, 5, 6]);
    }
}
