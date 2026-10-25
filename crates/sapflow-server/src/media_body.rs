//! Immutable media chunks with independent HTTP body/write cursors.

use std::convert::Infallible;
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use hyper::body::{Body, Frame, SizeHint};
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

impl Body for MediaBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let body = self.get_mut();
        if let Data::Single(data) = &mut body.data {
            let mut data = std::mem::take(data);
            data.advance(body.offset);
            *body = Self::default();
            return if data.is_empty() {
                Poll::Ready(None)
            } else {
                Poll::Ready(Some(Ok(Frame::data(data))))
            };
        }
        while let Some(chunk) = body.data.chunks().get(body.chunk) {
            if body.offset == chunk.len() {
                body.chunk += 1;
                body.offset = 0;
                continue;
            }
            let data = chunk.slice(body.offset..);
            body.consume(data.len());
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        self.is_empty()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn single_frame_owns_the_remaining_payload_after_the_body_is_drained() {
        for offset in [0, 3] {
            let payload: std::sync::Arc<[u8]> = std::sync::Arc::from(&b"abcdefgh"[..]);
            let retained = std::sync::Arc::downgrade(&payload);
            let pointer = payload.as_ptr();
            let mut body = MediaBody::from(Bytes::from_owner(payload));
            body.advance(offset).expect("single-buffer cursor");

            let data = body
                .frame()
                .await
                .expect("remaining frame")
                .expect("body")
                .into_data()
                .expect("data frame");
            assert_eq!(data.as_ptr(), pointer.wrapping_add(offset));
            assert_eq!(data.as_ref(), &b"abcdefgh"[offset..]);
            assert!(body.is_end_stream());
            assert_eq!(body.size_hint().exact(), Some(0));
            assert_eq!((body.chunk, body.offset, body.remaining), (0, 0, 0));
            assert!(matches!(&body.data, Data::Single(bytes) if bytes.is_empty()));
            assert!(body.frame().await.is_none());
            drop(body);
            assert_eq!(retained.strong_count(), 1);
            drop(data);
            assert_eq!(retained.strong_count(), 0);
        }

        let mut empty = MediaBody::default();
        assert!(empty.frame().await.is_none());
        assert!(empty.is_end_stream());
        assert_eq!(empty.size_hint().exact(), Some(0));
    }

    #[tokio::test]
    async fn hyper_frames_share_payloads_and_keep_an_exact_remaining_length() {
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
        assert_eq!(body.size_hint().exact(), Some(8));
        let first = body.frame().await.expect("first frame").expect("body");
        let first = first.into_data().expect("data frame");
        assert_eq!(first.as_ptr(), segment.chunks()[1].as_ptr());
        assert_eq!(first.as_ref(), &[1; 3]);
        assert_eq!(body.size_hint().exact(), Some(5));
        let second = body.frame().await.expect("second frame").expect("body");
        let second = second.into_data().expect("data frame");
        assert_eq!(second.as_ptr(), segment.chunks()[3].as_ptr());
        assert_eq!(second.as_ref(), &[2; 5]);
        assert!(body.is_end_stream());
        assert_eq!(body.size_hint().exact(), Some(0));
        assert!(body.frame().await.is_none());
        assert_eq!(copy.size_hint().exact(), Some(8));
        assert!(matches!(body.data, Data::Single(ref bytes) if bytes.is_empty()));
    }

    #[tokio::test]
    async fn advanced_body_emits_only_remaining_bytes_and_rejects_excess_advance() {
        let segment = SegmentData::from_chunks(vec![
            Bytes::new(),
            Bytes::from_static(b"abc"),
            Bytes::new(),
            Bytes::from_static(b"defgh"),
        ])
        .expect("bounded fixture");
        let mut body = MediaBody::from(segment);
        body.advance(4).expect("partial vectored write");
        assert_eq!(body.size_hint().exact(), Some(4));
        assert_eq!(
            body.advance(5).expect_err("excess write").kind(),
            io::ErrorKind::InvalidData
        );
        let frame = body.frame().await.expect("remaining frame").expect("body");
        assert_eq!(frame.into_data().expect("data").as_ref(), b"efgh");
        assert!(body.frame().await.is_none());
    }

    #[tokio::test]
    async fn empty_chunks_have_no_frames_or_write_slices() {
        let segment =
            SegmentData::from_chunks(vec![Bytes::new(), Bytes::new()]).expect("empty fixture");
        let mut body = MediaBody::from(segment);
        let mut slices = [IoSlice::new(&[]); 4];
        assert_eq!(body.slices(&mut slices), 0);
        assert!(body.is_end_stream());
        assert_eq!(body.size_hint().exact(), Some(0));
        assert!(body.frame().await.is_none());
    }
}
