//! Immutable media chunks with independent write cursors.

use std::io::{self, IoSlice};

use bytes::Bytes;
use sapflow_hls::muxer::SegmentData;

#[derive(Clone)]
enum Data {
    Single(Bytes),
    Segment(SegmentData),
}

impl Data {
    fn chunks(&self) -> &[Bytes] {
        match self {
            Self::Single(bytes) => std::slice::from_ref(bytes),
            Self::Segment(data) => data.chunks(),
        }
    }
}

/// Cloning a segment body shares its immutable chunk metadata and payloads.
/// Only the cursor belongs to a viewer; no joined segment is materialised.
#[derive(Clone)]
pub(crate) struct MediaBody {
    data: Data,
    chunk: usize,
    offset: usize,
    remaining: usize,
}

impl Default for MediaBody {
    fn default() -> Self {
        Bytes::new().into()
    }
}

impl From<Bytes> for MediaBody {
    fn from(bytes: Bytes) -> Self {
        Self {
            remaining: bytes.len(),
            data: Data::Single(bytes),
            chunk: 0,
            offset: 0,
        }
    }
}

impl From<SegmentData> for MediaBody {
    fn from(data: SegmentData) -> Self {
        Self {
            remaining: data.len(),
            data: Data::Segment(data),
            chunk: 0,
            offset: 0,
        }
    }
}

impl MediaBody {
    pub(crate) fn len(&self) -> usize {
        self.remaining
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.remaining == 0
    }

    /// Fill a bounded caller-owned array, omitting empty chunks.
    pub(crate) fn slices<'a>(&'a self, into: &mut [IoSlice<'a>]) -> usize {
        let mut used = 0;
        for (index, chunk) in self.data.chunks()[self.chunk..].iter().enumerate() {
            if used == into.len() {
                break;
            }
            let offset = if index == 0 { self.offset } else { 0 };
            let bytes = &chunk[offset..];
            if !bytes.is_empty() {
                into[used] = IoSlice::new(bytes);
                used += 1;
            }
        }
        used
    }

    pub(crate) fn advance(&mut self, count: usize) -> io::Result<()> {
        if count > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "write exceeded remaining media body",
            ));
        }
        self.consume(count);
        Ok(())
    }

    fn consume(&mut self, mut count: usize) {
        self.remaining -= count;
        while count != 0 {
            let left = self.data.chunks()[self.chunk].len() - self.offset;
            if count < left {
                self.offset += count;
                break;
            }
            count -= left;
            self.chunk += 1;
            self.offset = 0;
        }
        if self.remaining == 0 {
            // A slow keep-alive viewer must not retain its completed segment.
            *self = Self::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gathered(body: &MediaBody) -> Vec<u8> {
        let mut slices = [IoSlice::new(&[]); 8];
        let used = body.slices(&mut slices);
        slices[..used]
            .iter()
            .flat_map(|s| s.iter().copied())
            .collect()
    }

    #[test]
    fn single_buffer_shares_its_payload_and_releases_it_when_drained() {
        for offset in [0, 3] {
            let payload: std::sync::Arc<[u8]> = std::sync::Arc::from(&b"abcdefgh"[..]);
            let retained = std::sync::Arc::downgrade(&payload);
            let pointer = payload.as_ptr();
            let mut body = MediaBody::from(Bytes::from_owner(payload));
            body.advance(offset).expect("single-buffer cursor");

            let mut slices = [IoSlice::new(&[]); 2];
            assert_eq!(body.slices(&mut slices), 1);
            assert_eq!(slices[0].as_ptr(), pointer.wrapping_add(offset));
            assert_eq!(&*slices[0], &b"abcdefgh"[offset..]);
            assert_eq!(body.len(), 8 - offset);
            assert!(!body.is_empty());
            assert_eq!(retained.strong_count(), 1);

            body.advance(8 - offset).expect("remaining bytes");
            assert!(body.is_empty());
            assert_eq!((body.chunk, body.offset, body.remaining), (0, 0, 0));
            assert!(matches!(&body.data, Data::Single(bytes) if bytes.is_empty()));
            assert_eq!(
                retained.strong_count(),
                0,
                "a drained body must not retain its payload"
            );
        }

        let empty = MediaBody::default();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(gathered(&empty).is_empty());
    }

    #[test]
    fn segment_clones_share_payloads_and_keep_independent_cursors() {
        let chunks = vec![
            Bytes::new(),
            Bytes::from(vec![1; 3]),
            Bytes::new(),
            Bytes::from(vec![2; 5]),
            Bytes::new(),
        ];
        let segment = SegmentData::from_chunks(chunks).expect("bounded fixture");
        let mut body = MediaBody::from(segment.clone());
        let copy = body.clone();
        let Data::Segment(shared) = &copy.data else {
            panic!("segment metadata must stay shared");
        };
        assert!(std::ptr::eq(shared.chunks(), segment.chunks()));
        assert_eq!(body.len(), 8);

        let mut slices = [IoSlice::new(&[]); 4];
        assert_eq!(body.slices(&mut slices), 2);
        assert_eq!(slices[0].as_ptr(), segment.chunks()[1].as_ptr());
        assert_eq!(&*slices[0], &[1; 3]);
        assert_eq!(slices[1].as_ptr(), segment.chunks()[3].as_ptr());
        assert_eq!(&*slices[1], &[2; 5]);

        body.advance(3).expect("first chunk");
        assert_eq!(body.len(), 5);
        let mut slices = [IoSlice::new(&[]); 4];
        assert_eq!(body.slices(&mut slices), 1);
        assert_eq!(slices[0].as_ptr(), segment.chunks()[3].as_ptr());

        body.advance(5).expect("second chunk");
        assert!(body.is_empty());
        assert!(matches!(body.data, Data::Single(ref bytes) if bytes.is_empty()));
        assert_eq!(copy.len(), 8, "the clone's cursor is untouched");
    }

    #[test]
    fn advanced_body_exposes_only_remaining_bytes_and_rejects_excess_advance() {
        let segment = SegmentData::from_chunks(vec![
            Bytes::new(),
            Bytes::from_static(b"abc"),
            Bytes::new(),
            Bytes::from_static(b"defgh"),
        ])
        .expect("bounded fixture");
        let mut body = MediaBody::from(segment);
        body.advance(4).expect("partial vectored write");
        assert_eq!(body.len(), 4);
        assert_eq!(
            body.advance(5).expect_err("excess write").kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(body.len(), 4, "a refused advance moves nothing");
        assert_eq!(gathered(&body), b"efgh");
        body.advance(4).expect("remaining bytes");
        assert!(body.is_empty());
        assert!(gathered(&body).is_empty());
    }

    #[test]
    fn empty_chunks_have_no_write_slices() {
        let segment =
            SegmentData::from_chunks(vec![Bytes::new(), Bytes::new()]).expect("empty fixture");
        let body = MediaBody::from(segment);
        let mut slices = [IoSlice::new(&[]); 4];
        assert_eq!(body.slices(&mut slices), 0);
        assert!(body.is_empty());
        assert_eq!(body.len(), 0);
    }

    #[test]
    fn partial_response_retains_consumed_prefix_until_the_final_body_drains() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        struct Payload(Vec<u8>, Arc<AtomicUsize>);
        impl AsRef<[u8]> for Payload {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }
        impl Drop for Payload {
            fn drop(&mut self) {
                self.1.fetch_add(1, Ordering::SeqCst);
            }
        }

        let released = Arc::new(AtomicUsize::new(0));
        let segment = SegmentData::from_chunks(vec![
            Bytes::from_owner(Payload(b"abc".to_vec(), released.clone())),
            Bytes::from_owner(Payload(b"defgh".to_vec(), released.clone())),
        ])
        .expect("owned generated payloads");
        let mut body = MediaBody::from(segment.clone());
        let alias = body.clone();
        drop(segment); // Models lookup/window eviction.
        body.advance(3).expect("consume whole prefix chunk");
        assert_eq!(gathered(&body), b"defgh");
        assert_eq!(
            released.load(Ordering::SeqCst),
            0,
            "consumed prefix remains in the shared chunk array"
        );
        drop(alias);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        body.advance(2).expect("partial suffix");
        assert_eq!(gathered(&body), b"fgh");
        body.advance(3).expect("final accepted bytes");
        assert_eq!(
            released.load(Ordering::SeqCst),
            2,
            "each backing released exactly once"
        );
        drop(body);
        assert_eq!(released.load(Ordering::SeqCst), 2);
    }
}
