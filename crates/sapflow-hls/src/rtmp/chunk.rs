//! The RTMP chunk stream: messages split into chunks, interleaved by chunk
//! stream id (csid), with headers compressed against the previous chunk on
//! the same csid.
//!
//! Reading is one chunk at a time from the front of a buffer. A chunk is
//! only consumed when all of it is present, so state never changes on a
//! partial read. Partial messages are kept per csid (chunks of different
//! messages may interleave) and grow only with bytes actually received;
//! their total, and the number of chunk streams, are capped.

use std::collections::HashMap;

/// Chunk size until the peer sets another.
pub const DEFAULT_CHUNK_SIZE: u32 = 128;
/// Largest chunk size accepted from a peer. A message cannot exceed 16 MiB
/// (24-bit length), so a larger chunk size buys nothing.
pub const MAX_CHUNK_SIZE: u32 = 1 << 24;
/// Most chunk streams one connection may use. Encoders use under ten.
pub const MAX_CHUNK_STREAMS: usize = 64;

/// The 24-bit timestamp value that says "extended timestamp follows".
const EXTENDED: u32 = 0x00ff_ffff;

/// Basic header (up to 3) + message header (11) + extended timestamp (4).
pub const MAX_HEADER: usize = 18;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub csid: u32,
    pub type_id: u8,
    pub stream_id: u32,
    /// Milliseconds; wraps at 2^32 like the protocol's.
    pub timestamp: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkError {
    /// A compressed header on a csid that never had a full one.
    NoPreviousHeader(u32),
    TooManyChunkStreams,
    /// Partial messages would hold more than the connection's budget.
    OverBudget,
    BadChunkSize(u32),
}

impl std::fmt::Display for ChunkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPreviousHeader(c) => write!(f, "chunk stream {c} has no previous header"),
            Self::TooManyChunkStreams => f.write_str("too many chunk streams"),
            Self::OverBudget => f.write_str("too much partial message data"),
            Self::BadChunkSize(s) => write!(f, "chunk size {s} refused"),
        }
    }
}

impl std::error::Error for ChunkError {}

#[derive(Debug, Default)]
struct StreamState {
    timestamp: u32,
    /// The last 24-bit timestamp field (absolute for type 0, else delta).
    /// A type 3 chunk starting a new message adds it again as a delta
    /// (after type 0 that is the absolute timestamp, as the specification
    /// says and ffmpeg and rml_rtmp do).
    field: u32,
    length: u32,
    type_id: u8,
    stream_id: u32,
    partial: Vec<u8>,
}

/// Reads chunks, one at a time.
pub struct ChunkReader {
    chunk_size: u32,
    streams: HashMap<u32, StreamState>,
    /// Sum of all partial message lengths.
    buffered: usize,
    /// Most bytes partial messages may hold together.
    budget: usize,
}

/// What one call to [`ChunkReader::read`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    /// Not a whole chunk yet; nothing consumed.
    NeedMore,
    /// One chunk consumed; it may have completed a message.
    Chunk {
        consumed: usize,
        message: Option<Message>,
    },
}

impl ChunkReader {
    pub fn new(budget: usize) -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            streams: HashMap::new(),
            buffered: 0,
            budget,
        }
    }

    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
    }

    pub fn buffered(&self) -> usize {
        self.buffered
    }

    pub fn chunk_size(&self) -> u32 {
        self.chunk_size
    }

    /// From the peer's Set Chunk Size. The top bit must be clear and the
    /// size at least 1 (both per the specification) and at most
    /// [`MAX_CHUNK_SIZE`].
    pub fn set_chunk_size(&mut self, size: u32) -> Result<(), ChunkError> {
        if size == 0 || size > MAX_CHUNK_SIZE {
            return Err(ChunkError::BadChunkSize(size));
        }
        self.chunk_size = size;
        Ok(())
    }

    /// From the peer's Abort: drops the partial message on `csid`.
    pub fn abort(&mut self, csid: u32) {
        if let Some(s) = self.streams.get_mut(&csid) {
            self.buffered = self.buffered.saturating_sub(s.partial.len());
            s.partial = Vec::new();
        }
    }

    /// Reads one chunk from the front of `buf`.
    pub fn read(&mut self, buf: &[u8]) -> Result<Read, ChunkError> {
        let mut r = Cursor { buf, pos: 0 };
        let Some(b0) = r.u8() else {
            return Ok(Read::NeedMore);
        };
        let fmt = b0 >> 6;
        let csid = match b0 & 0x3f {
            0 => match r.u8() {
                Some(b) => 64 + u32::from(b),
                None => return Ok(Read::NeedMore),
            },
            1 => match (r.u8(), r.u8()) {
                (Some(a), Some(b)) => 64 + u32::from(a) + 256 * u32::from(b),
                _ => return Ok(Read::NeedMore),
            },
            n => u32::from(n),
        };

        let prev = self.streams.get(&csid);
        if fmt != 0 && prev.is_none() {
            return Err(ChunkError::NoPreviousHeader(csid));
        }
        if prev.is_none() && self.streams.len() >= MAX_CHUNK_STREAMS {
            return Err(ChunkError::TooManyChunkStreams);
        }

        // Parse into locals; commit only once the whole chunk is present.
        let (mut field, mut length, mut type_id, mut stream_id) = match prev {
            Some(p) => (p.field, p.length, p.type_id, p.stream_id),
            None => (0, 0, 0, 0),
        };
        if fmt <= 2 {
            let Some(t) = r.u24() else {
                return Ok(Read::NeedMore);
            };
            field = t;
        }
        if fmt <= 1 {
            let (Some(l), Some(t)) = (r.u24(), r.u8()) else {
                return Ok(Read::NeedMore);
            };
            length = l;
            type_id = t;
        }
        if fmt == 0 {
            let Some(id) = r.u32_le() else {
                return Ok(Read::NeedMore);
            };
            stream_id = id;
        }
        // Repeated on every chunk, type 3 included, while the field says so.
        let extended = if field == EXTENDED {
            match r.u32_be() {
                Some(e) => Some(e),
                None => return Ok(Read::NeedMore),
            }
        } else {
            None
        };

        let (prev_ts, have) = match prev {
            Some(p) => (p.timestamp, p.partial.len()),
            None => (0, 0),
        };
        // A new header in the middle of a message starts a new message;
        // the partial one is dropped. A type 3 header continues it.
        let continuing = fmt == 3 && have > 0;
        let have = if continuing { have } else { 0 };
        let effective = extended.unwrap_or(field);
        let timestamp = if continuing {
            prev_ts
        } else if fmt == 0 {
            effective
        } else {
            prev_ts.wrapping_add(effective)
        };

        let total = usize::try_from(length).map_err(|_| ChunkError::OverBudget)?;
        let want = total
            .saturating_sub(have)
            .min(usize::try_from(self.chunk_size).unwrap_or(usize::MAX));
        let Some(body) = r.take(want) else {
            return Ok(Read::NeedMore);
        };

        // Commit.
        let dropped = if continuing {
            0
        } else {
            prev.map_or(0, |p| p.partial.len())
        };
        let buffered = self
            .buffered
            .saturating_sub(dropped)
            .checked_add(want)
            .ok_or(ChunkError::OverBudget)?;
        if buffered > self.budget {
            return Err(ChunkError::OverBudget);
        }
        self.buffered = buffered;
        let s = self.streams.entry(csid).or_default();
        if !continuing {
            s.partial = Vec::new();
        }
        s.field = field;
        s.timestamp = timestamp;
        s.length = length;
        s.type_id = type_id;
        s.stream_id = stream_id;
        s.partial.extend_from_slice(body);

        let message = if s.partial.len() >= total {
            self.buffered = self.buffered.saturating_sub(s.partial.len());
            Some(Message {
                csid,
                type_id,
                stream_id,
                timestamp,
                payload: std::mem::take(&mut s.partial),
            })
        } else {
            None
        };
        Ok(Read::Chunk {
            consumed: r.pos,
            message,
        })
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u24(&mut self) -> Option<u32> {
        let [a, b, c] = self.take(3)? else {
            return None;
        };
        Some(u32::from_be_bytes([0, *a, *b, *c]))
    }

    fn u32_be(&mut self) -> Option<u32> {
        let [a, b, c, d] = self.take(4)? else {
            return None;
        };
        Some(u32::from_be_bytes([*a, *b, *c, *d]))
    }

    fn u32_le(&mut self) -> Option<u32> {
        let [a, b, c, d] = self.take(4)? else {
            return None;
        };
        Some(u32::from_le_bytes([*a, *b, *c, *d]))
    }
}

/// Writes messages as chunks: a type 0 header, then type 3 continuations.
pub struct ChunkWriter {
    chunk_size: u32,
}

impl Default for ChunkWriter {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }
}

impl ChunkWriter {
    /// Call after writing the Set Chunk Size message that announces it.
    pub fn set_chunk_size(&mut self, size: u32) {
        self.chunk_size = size.clamp(1, MAX_CHUNK_SIZE);
    }

    /// `csid` must be 2..=63 (one-byte basic header); `payload` at most
    /// 16 MiB - 1 (the length field); longer payloads are cut.
    pub fn write(
        &self,
        out: &mut Vec<u8>,
        csid: u8,
        type_id: u8,
        stream_id: u32,
        timestamp: u32,
        payload: &[u8],
    ) {
        let csid = csid.clamp(2, 63);
        let payload = payload.get(..EXTENDED as usize).unwrap_or(payload);
        let ts_field = timestamp.min(EXTENDED);
        out.push(csid);
        out.extend_from_slice(&ts_field.to_be_bytes()[1..]);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
        out.push(type_id);
        out.extend_from_slice(&stream_id.to_le_bytes());
        if ts_field == EXTENDED {
            out.extend_from_slice(&timestamp.to_be_bytes());
        }
        let size = usize::try_from(self.chunk_size).unwrap_or(usize::MAX);
        for (i, part) in payload.chunks(size).enumerate() {
            if i > 0 {
                out.push(0xc0 | csid);
                if ts_field == EXTENDED {
                    out.extend_from_slice(&timestamp.to_be_bytes());
                }
            }
            out.extend_from_slice(part);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(r: &mut ChunkReader, mut buf: &[u8]) -> Vec<Message> {
        let mut out = Vec::new();
        loop {
            match r.read(buf).expect("valid chunks") {
                Read::NeedMore => return out,
                Read::Chunk { consumed, message } => {
                    buf = &buf[consumed..];
                    out.extend(message);
                }
            }
        }
    }

    #[test]
    fn writer_and_reader_agree_across_chunk_sizes() {
        let payload: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        for size in [1, 2, 127, 128, 129, 999, 1000, 4096] {
            for ts in [0, 1, EXTENDED - 1, EXTENDED, u32::MAX] {
                let mut w = ChunkWriter::default();
                w.set_chunk_size(size);
                let mut bytes = Vec::new();
                w.write(&mut bytes, 6, 9, 1, ts, &payload);
                let mut r = ChunkReader::new(1 << 20);
                r.set_chunk_size(size).expect("valid size");
                let msgs = read_all(&mut r, &bytes);
                assert_eq!(msgs.len(), 1, "size {size} ts {ts}");
                assert_eq!(msgs[0].payload, payload);
                assert_eq!(
                    (msgs[0].timestamp, msgs[0].type_id, msgs[0].stream_id),
                    (ts, 9, 1)
                );
                assert_eq!(r.buffered(), 0);
            }
        }
    }

    #[test]
    fn header_types_and_deltas() {
        let mut b = Vec::new();
        // Type 0 on csid 4: ts 1000, length 2, type 8, stream 1.
        b.extend_from_slice(&[0x04, 0, 0x03, 0xe8, 0, 0, 2, 8, 1, 0, 0, 0, 0xaa, 0xbb]);
        // Type 1: delta 20, length 1, type 9.
        b.extend_from_slice(&[0x44, 0, 0, 20, 0, 0, 1, 9, 0xcc]);
        // Type 2: delta 30.
        b.extend_from_slice(&[0x84, 0, 0, 30, 0xdd]);
        // Type 3 starting a new message: delta 30 again.
        b.extend_from_slice(&[0xc4, 0xee]);
        let mut r = ChunkReader::new(1 << 20);
        let m = read_all(&mut r, &b);
        let got: Vec<_> = m
            .iter()
            .map(|m| (m.timestamp, m.type_id, m.stream_id, m.payload.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (1000, 8, 1, vec![0xaa, 0xbb]),
                (1020, 9, 1, vec![0xcc]),
                (1050, 9, 1, vec![0xdd]),
                (1080, 9, 1, vec![0xee]),
            ]
        );
    }

    #[test]
    fn type_3_after_type_0_reuses_the_absolute_timestamp_as_delta() {
        let mut b = vec![0x05, 0, 0, 40, 0, 0, 1, 8, 1, 0, 0, 0, 1];
        b.extend_from_slice(&[0xc5, 2]);
        let mut r = ChunkReader::new(1 << 20);
        let ts: Vec<_> = read_all(&mut r, &b).iter().map(|m| m.timestamp).collect();
        assert_eq!(ts, vec![40, 80]);
    }

    #[test]
    fn extended_timestamps_repeat_on_continuations() {
        // Type 0 with extended timestamp 0x01000000, length 3, chunk size 2.
        let mut b = vec![
            0x06, 0xff, 0xff, 0xff, 0, 0, 3, 9, 1, 0, 0, 0, 1, 0, 0, 0, 1, 2,
        ];
        // Continuation, extended timestamp repeated.
        b.extend_from_slice(&[0xc6, 1, 0, 0, 0, 3]);
        // A new message by type 3: the extended field is the delta.
        b.extend_from_slice(&[0xc6, 0, 0, 0, 5, 7, 8]);
        b.extend_from_slice(&[0xc6, 0, 0, 0, 5, 9]);
        let mut r = ChunkReader::new(1 << 20);
        r.set_chunk_size(2).expect("valid");
        let m = read_all(&mut r, &b);
        assert_eq!(m.len(), 2);
        assert_eq!(
            (m[0].timestamp, m[0].payload.as_slice()),
            (0x0100_0000, &[1, 2, 3][..])
        );
        assert_eq!(
            (m[1].timestamp, m[1].payload.as_slice()),
            (0x0100_0005, &[7, 8, 9][..])
        );
    }

    #[test]
    fn interleaved_chunk_streams_and_long_csids() {
        let mut b = Vec::new();
        // csid 4 (video) first chunk of 4 bytes at chunk size 2.
        b.extend_from_slice(&[0x04, 0, 0, 0, 0, 0, 4, 9, 1, 0, 0, 0, 1, 2]);
        // csid 300 (three-byte form: 64 + 236 + 0 * 256), whole 1-byte message.
        b.extend_from_slice(&[0x01, 236, 0, 0, 0, 0, 0, 0, 1, 8, 1, 0, 0, 0, 0x55]);
        // csid 70 (two-byte form).
        b.extend_from_slice(&[0x00, 6, 0, 0, 0, 0, 0, 1, 8, 1, 0, 0, 0, 0x66]);
        b.extend_from_slice(&[0xc4, 3, 4]);
        let mut r = ChunkReader::new(1 << 20);
        r.set_chunk_size(2).expect("valid");
        let m = read_all(&mut r, &b);
        let got: Vec<_> = m.iter().map(|m| (m.csid, m.payload.clone())).collect();
        assert_eq!(
            got,
            vec![(300, vec![0x55]), (70, vec![0x66]), (4, vec![1, 2, 3, 4])]
        );
    }

    #[test]
    fn partial_chunks_consume_nothing() {
        let mut w = ChunkWriter::default();
        w.set_chunk_size(64);
        let mut bytes = Vec::new();
        w.write(&mut bytes, 3, 20, 0, EXTENDED + 1, &[7u8; 200]);
        for cut in 0..bytes.len() {
            let mut r = ChunkReader::new(1 << 20);
            r.set_chunk_size(64).expect("valid");
            // Feed the prefix; whatever is consumed, then the rest.
            let mut pending = bytes[..cut].to_vec();
            let mut msgs = Vec::new();
            for rest in [&bytes[cut..], &[][..]] {
                pending.extend_from_slice(rest);
                loop {
                    match r.read(&pending).expect("valid") {
                        Read::NeedMore => break,
                        Read::Chunk { consumed, message } => {
                            pending.drain(..consumed);
                            msgs.extend(message);
                        }
                    }
                }
            }
            assert_eq!(msgs.len(), 1, "cut at {cut}");
            assert_eq!(msgs[0].payload, vec![7u8; 200]);
        }
    }

    #[test]
    fn refusals() {
        let mut r = ChunkReader::new(1 << 20);
        assert_eq!(
            r.read(&[0x44, 0, 0, 0]),
            Err(ChunkError::NoPreviousHeader(4))
        );
        assert_eq!(r.read(&[0xc3]), Err(ChunkError::NoPreviousHeader(3)));
        assert_eq!(r.set_chunk_size(0), Err(ChunkError::BadChunkSize(0)));
        assert_eq!(
            r.set_chunk_size(0x8000_0000),
            Err(ChunkError::BadChunkSize(0x8000_0000))
        );
        assert_eq!(r.set_chunk_size(MAX_CHUNK_SIZE), Ok(()));

        // Budget: a 16 MiB message trickled in over a small budget.
        let mut r = ChunkReader::new(1000);
        r.set_chunk_size(600).expect("valid");
        let mut b = vec![0x04, 0, 0, 0, 0xff, 0xff, 0xff, 9, 1, 0, 0, 0];
        b.extend_from_slice(&[0u8; 600]);
        let Ok(Read::Chunk {
            consumed,
            message: None,
        }) = r.read(&b)
        else {
            panic!("first chunk fits");
        };
        assert_eq!(consumed, b.len());
        let mut c = vec![0xc4];
        c.extend_from_slice(&[0u8; 600]);
        assert_eq!(r.read(&c), Err(ChunkError::OverBudget));
        // Abort frees it.
        r.abort(4);
        assert_eq!(r.buffered(), 0);

        // Too many chunk streams.
        let mut r = ChunkReader::new(1 << 20);
        for csid in 0..MAX_CHUNK_STREAMS as u32 {
            let id = csid + 64;
            let mut b = vec![0x01, (id - 64) as u8, ((id - 64) >> 8) as u8];
            b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 8, 1, 0, 0, 0]);
            assert!(matches!(r.read(&b), Ok(Read::Chunk { .. })));
        }
        let b = [0x03, 0, 0, 0, 0, 0, 0, 8, 1, 0, 0, 0];
        assert_eq!(r.read(&b), Err(ChunkError::TooManyChunkStreams));
    }

    #[test]
    fn a_new_header_mid_message_drops_the_partial() {
        let mut r = ChunkReader::new(1 << 20);
        r.set_chunk_size(2).expect("valid");
        let mut b = vec![0x04, 0, 0, 0, 0, 0, 4, 9, 1, 0, 0, 0, 1, 2];
        b.extend_from_slice(&[0x04, 0, 0, 9, 0, 0, 1, 8, 1, 0, 0, 0, 5]);
        let m = read_all(&mut r, &b);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].timestamp, m[0].payload.as_slice()), (9, &[5][..]));
        assert_eq!(r.buffered(), 0);
    }
}
