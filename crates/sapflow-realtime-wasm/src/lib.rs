//! Safe numeric browser boundary for Sapflow's opaque real-time scheduler.
//! The host retains ciphertext and must drain discarded IDs after every call.

use std::collections::VecDeque;

use sapflow_core::realtime::{Changes, Error, Frame, Kind, Limits, Queue};
use wasm_bindgen::prelude::*;

fn time_us(ms: f64) -> Result<u64, i32> {
    // JavaScript safe integer microseconds, with sub-millisecond browser clocks.
    if !ms.is_finite() || !(0.0..=9_007_199_254_740.0).contains(&ms) {
        return Err(-1);
    }
    Ok((ms * 1000.0).floor() as u64)
}
fn error(value: Error) -> i32 {
    match value {
        Error::Limits => -2,
        Error::Frame => -3,
        Error::Clock => -4,
        Error::DuplicateId => -5,
        Error::Streams => -6,
        Error::StreamKind => -7,
        Error::Closed => -8,
    }
}

/// One independently owned scheduler. It has no keys, bytes, logs or runtime.
#[wasm_bindgen]
pub struct RealtimeQueue {
    queue: Queue,
    discarded: VecDeque<u32>,
}

#[wasm_bindgen]
impl RealtimeQueue {
    /// Constructs one bounded scheduling lifetime. Invalid limits throw a
    /// generic numeric error; no user content is included.
    #[wasm_bindgen(constructor)]
    pub fn new(
        max_bytes: u32,
        max_frames: u32,
        max_streams: u32,
        max_residence_ms: f64,
    ) -> Result<RealtimeQueue, i32> {
        let limits = Limits::new(
            max_bytes,
            max_frames,
            max_streams,
            time_us(max_residence_ms)?,
        )
        .map_err(error)?;
        Ok(Self {
            queue: Queue::new(limits),
            discarded: VecDeque::new(),
        })
    }
    /// Offers metadata for one already encrypted frame. Returns 1 when
    /// admitted, 0 when dropped, or a negative generic error. Drain `takeDrop`
    /// before the next operation, even after a refused offer.
    #[allow(clippy::too_many_arguments)]
    pub fn offer(
        &mut self,
        id: u32,
        stream: u32,
        video: u32,
        key: u32,
        sequence: u32,
        bytes: u32,
        now_ms: f64,
        deadline_ms: f64,
    ) -> i32 {
        if !self.discarded.is_empty() {
            return -9;
        }
        let kind = match (video, key) {
            (0, 0) => Kind::Audio,
            (1, 0) => Kind::VideoDelta,
            (1, 1) => Kind::VideoKey,
            _ => return -3,
        };
        let (Ok(now_us), Ok(deadline_us)) = (time_us(now_ms), time_us(deadline_ms)) else {
            return -1;
        };
        match self.queue.offer(
            Frame {
                id,
                stream,
                kind,
                sequence,
                bytes,
                deadline_us,
            },
            now_us,
        ) {
            Ok(result) => {
                self.record(result.changes);
                i32::from(result.accepted)
            }
            Err(e) => error(e),
        }
    }
    /// Returns the next frame ID, 0 for empty, or a negative generic error.
    /// Discarded IDs must be drained before using the returned ciphertext.
    pub fn pop(&mut self, now_ms: f64) -> f64 {
        if !self.discarded.is_empty() {
            return -9.0;
        }
        let Ok(now_us) = time_us(now_ms) else {
            return -1.0;
        };
        match self.queue.pop(now_us) {
            Ok(result) => {
                self.record(result.changes);
                result.frame.map_or(0.0, |frame| f64::from(frame.id))
            }
            Err(e) => f64::from(error(e)),
        }
    }
    /// Expires stale ciphertext while capture or transport is idle.
    pub fn tick(&mut self, now_ms: f64) -> i32 {
        if !self.discarded.is_empty() {
            return -9;
        }
        let Ok(now_us) = time_us(now_ms) else {
            return -1;
        };
        match self.queue.tick(now_us) {
            Ok(changes) => {
                self.record(changes);
                0
            }
            Err(e) => error(e),
        }
    }
    /// Permanently closes admission and releases all currently retained IDs.
    pub fn close(&mut self) -> i32 {
        if !self.discarded.is_empty() {
            return -9;
        }
        let changes = self.queue.close();
        self.record(changes);
        0
    }
    /// Drains one ownership release; 0 means no remaining releases. Operations
    /// refuse until all releases are drained, bounding this list by maxFrames.
    #[wasm_bindgen(js_name = takeDrop)]
    pub fn take_drop(&mut self) -> u32 {
        self.discarded.pop_front().unwrap_or(0)
    }
    /// Resident ciphertext bytes, as claimed by the host.
    pub fn bytes(&self) -> u32 {
        self.queue.bytes()
    }
    /// Resident frame count.
    pub fn frames(&self) -> u32 {
        self.queue.frames() as u32
    }
    /// Whether the specified stream requires a new video keyframe.
    pub fn waiting(&self, stream: u32) -> bool {
        self.queue.waiting_for_keyframe(stream)
    }
}
impl RealtimeQueue {
    fn record(&mut self, changes: Changes) {
        self.discarded.extend(changes.discarded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_time_rejects_hostile_and_preserves_browser_fraction() {
        for ms in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.1,
            9_007_199_254_741.0,
        ] {
            assert_eq!(time_us(ms), Err(-1));
        }
        assert_eq!(time_us(1.2345), Ok(1234));
    }

    #[test]
    fn undrained_drops_refuse_new_work_and_remain_bounded() {
        let mut q = RealtimeQueue::new(20, 2, 2, 1000.0).expect("limits");
        assert_eq!(q.offer(1, 1, 0, 0, 0, 10, 0.0, 100.0), 1);
        assert_eq!(q.offer(2, 1, 0, 0, 1, 10, 0.0, 100.0), 1);
        assert_eq!(q.offer(3, 1, 0, 0, 2, 10, 0.0, 100.0), 1);
        for _ in 0..100 {
            assert_eq!(q.offer(4, 1, 0, 0, 3, 10, 0.0, 100.0), -9);
            assert_eq!(q.pop(0.0), -9.0);
            assert_eq!(q.tick(0.0), -9);
        }
        assert_eq!(q.take_drop(), 1);
        assert_eq!(q.take_drop(), 0);
        assert_eq!(q.pop(0.0), 2.0);
    }

    #[test]
    fn abi_scalar_boolean_and_clock_errors_are_generic() {
        let mut q = RealtimeQueue::new(20, 2, 2, 1000.0).expect("limits");
        assert_eq!(q.offer(1, 1, 2, 0, 0, 10, 0.0, 100.0), -3);
        assert_eq!(q.offer(1, 1, 0, 1, 0, 10, 0.0, 100.0), -3);
        assert_eq!(q.offer(1, 1, 0, 0, 0, 10, f64::NAN, 100.0), -1);
        assert_eq!(q.offer(1, 1, 0, 0, 0, 10, 2.0, 100.0), 1);
        assert_eq!(q.tick(1.0), -4);
        assert_eq!(q.close(), 0);
        assert_eq!(q.take_drop(), 1);
        assert_eq!(q.offer(2, 1, 0, 0, 1, 10, 3.0, 100.0), -8);
    }
}
