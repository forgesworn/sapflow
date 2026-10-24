//! Bounded scheduling of opaque, already encrypted real-time frames.
//!
//! The caller owns ciphertext by nonzero frame ID and must release every ID
//! returned in `discarded`. This queue neither parses nor retains media bytes.
//! Metadata is scheduling input, not proof of codec validity or authorisation.
//! All clocks are caller-supplied monotonic microseconds. Call [`Queue::tick`]
//! while idle so stale frames do not survive a stopped capture source.

use std::collections::{BTreeMap, VecDeque};

/// The scheduling dependency of one opaque frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Independently decodable audio packet.
    Audio,
    /// Independently decodable video frame.
    VideoKey,
    /// Video frame requiring every preceding frame since the last keyframe.
    VideoDelta,
}
impl Kind {
    fn video(self) -> bool {
        self != Self::Audio
    }
}

/// Scalar metadata for ciphertext retained by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Unique nonzero ID among frames currently owned by this queue.
    pub id: u32,
    /// Nonzero stream ID; use separate IDs for separate audio/video tracks.
    pub stream: u32,
    /// Audio independence or video dependency category.
    pub kind: Kind,
    /// Strictly increasing sequence within this stream lifetime. Never wrap.
    pub sequence: u32,
    /// Exact ciphertext bytes retained under `id`, including its envelope.
    pub bytes: u32,
    /// Expiry in the same monotonic microsecond clock supplied to operations.
    pub deadline_us: u64,
}

/// Hard bounds on resident ciphertext and scheduling state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    max_bytes: u32,
    max_frames: u32,
    max_streams: u32,
    max_residence_us: u64,
}
impl Limits {
    /// Validates finite resource bounds. Maximums are 16 MiB, 4096 frames,
    /// 256 stream lifetimes and 60 seconds of residence.
    pub fn new(
        max_bytes: u32,
        max_frames: u32,
        max_streams: u32,
        max_residence_us: u64,
    ) -> Result<Self, Error> {
        if !(1..=16 * 1024 * 1024).contains(&max_bytes)
            || !(1..=4096).contains(&max_frames)
            || !(1..=256).contains(&max_streams)
            || !(1..=60_000_000).contains(&max_residence_us)
        {
            return Err(Error::Limits);
        }
        Ok(Self {
            max_bytes,
            max_frames,
            max_streams,
            max_residence_us,
        })
    }
}

/// Invalid operation; no frame is admitted when an error is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Invalid queue resource bounds.
    Limits,
    /// A frame has a zero ID, stream ID or length.
    Frame,
    /// Clock moved backwards. The host must use a monotonic clock.
    Clock,
    /// The ID is already resident in this queue.
    DuplicateId,
    /// The bounded stream-state table is full.
    Streams,
    /// An audio stream was used as video, or vice versa.
    StreamKind,
    /// Admission was closed permanently.
    Closed,
}

/// IDs whose ciphertext the host must immediately release.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Previously resident frames invalidated by age, pressure or dependency.
    pub discarded: Vec<u32>,
}

/// Result of offering one ciphertext frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Offered {
    /// Whether the host should retain the new ciphertext.
    pub accepted: bool,
    /// Previous ciphertext ownership to release, even if admission failed.
    pub changes: Changes,
    /// This stream currently needs an independently decodable video frame.
    pub waiting_for_keyframe: bool,
}

/// Result of selecting the next frame to send.
#[derive(Debug, PartialEq, Eq)]
pub struct Popped {
    /// Audio is selected before video; order within each class is preserved.
    pub frame: Option<Frame>,
    /// Expired ciphertext ownership to release before sending.
    pub changes: Changes,
}

#[derive(Debug, Clone, Copy)]
struct Stream {
    video: bool,
    last_sequence: Option<u32>,
    waiting: bool,
}
#[derive(Debug)]
struct Entry {
    frame: Frame,
    enqueued_us: u64,
}

/// Deadline-aware scheduler for opaque host-owned ciphertext.
#[derive(Debug)]
pub struct Queue {
    limits: Limits,
    entries: VecDeque<Entry>,
    streams: BTreeMap<u32, Stream>,
    bytes: u32,
    last_now: Option<u64>,
    closed: bool,
}
impl Queue {
    /// Starts an empty queue without a clock, files, sockets or media keys.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            entries: VecDeque::new(),
            streams: BTreeMap::new(),
            bytes: 0,
            last_now: None,
            closed: false,
        }
    }
    /// Resident ciphertext length claimed by the host.
    pub fn bytes(&self) -> u32 {
        self.bytes
    }
    /// Number of resident frames.
    pub fn frames(&self) -> usize {
        self.entries.len()
    }
    /// Number of remembered stream lifetimes, including drained streams.
    pub fn streams(&self) -> usize {
        self.streams.len()
    }
    /// Whether this stream must next supply a video keyframe.
    pub fn waiting_for_keyframe(&self, stream: u32) -> bool {
        self.streams.get(&stream).is_some_and(|s| s.waiting)
    }
    fn clock(&mut self, now_us: u64) -> Result<(), Error> {
        if self.last_now.is_some_and(|last| now_us < last) {
            return Err(Error::Clock);
        }
        self.last_now = Some(now_us);
        Ok(())
    }
    fn remove(&mut self, index: usize, changes: &mut Changes) {
        if let Some(entry) = self.entries.remove(index) {
            self.bytes -= entry.frame.bytes;
            changes.discarded.push(entry.frame.id);
        }
    }
    fn break_video(&mut self, stream: u32, changes: &mut Changes) {
        if let Some(state) = self.streams.get_mut(&stream) {
            state.waiting = true;
        }
        // This conservative policy drops the entire stream backlog. A new
        // keyframe is required even if one happened to be queued after loss.
        for index in (0..self.entries.len()).rev() {
            if self.entries[index].frame.stream == stream {
                self.remove(index, changes);
            }
        }
    }
    fn expire(&mut self, now_us: u64, changes: &mut Changes) {
        while let Some(index) = self.entries.iter().position(|entry| {
            now_us >= entry.frame.deadline_us
                || now_us - entry.enqueued_us >= self.limits.max_residence_us
        }) {
            let frame = self.entries[index].frame;
            if frame.kind.video() {
                self.break_video(frame.stream, changes);
            } else {
                self.remove(index, changes);
            }
        }
    }
    /// Expires stale frames even when no new capture arrives.
    pub fn tick(&mut self, now_us: u64) -> Result<Changes, Error> {
        self.clock(now_us)?;
        let mut changes = Changes::default();
        self.expire(now_us, &mut changes);
        Ok(changes)
    }
    /// Offers scheduling metadata for one already encrypted frame.
    ///
    /// The host retains ciphertext only when `accepted` is true. It releases
    /// all `discarded` IDs on both accepted and refused offers. Video pressure
    /// never evicts audio. Audio pressure first invalidates queued video and
    /// then replaces oldest audio, preserving fresh conversational audio.
    pub fn offer(&mut self, frame: Frame, now_us: u64) -> Result<Offered, Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        if frame.id == 0 || frame.stream == 0 || frame.bytes == 0 {
            return Err(Error::Frame);
        }
        if self.entries.iter().any(|e| e.frame.id == frame.id) {
            return Err(Error::DuplicateId);
        }
        if let Some(stream) = self.streams.get(&frame.stream) {
            if stream.video != frame.kind.video() {
                return Err(Error::StreamKind);
            }
        } else if self.streams.len() >= self.limits.max_streams as usize {
            return Err(Error::Streams);
        }
        self.clock(now_us)?;
        let mut changes = Changes::default();
        self.expire(now_us, &mut changes);
        let stream = self.streams.entry(frame.stream).or_insert(Stream {
            video: frame.kind.video(),
            last_sequence: None,
            waiting: frame.kind.video(),
        });
        let stale = stream
            .last_sequence
            .is_some_and(|seq| frame.sequence <= seq);
        let gap = stream
            .last_sequence
            .is_some_and(|seq| seq.checked_add(1) != Some(frame.sequence));
        if stale {
            return Ok(Offered {
                accepted: false,
                waiting_for_keyframe: stream.waiting,
                changes,
            });
        }
        stream.last_sequence = Some(frame.sequence);
        if frame.kind.video() && gap {
            self.break_video(frame.stream, &mut changes);
        }
        let invalid = frame.deadline_us <= now_us || frame.bytes > self.limits.max_bytes;
        if invalid {
            if frame.kind.video() {
                self.break_video(frame.stream, &mut changes);
            }
            return Ok(Offered {
                accepted: false,
                waiting_for_keyframe: self.waiting_for_keyframe(frame.stream),
                changes,
            });
        }
        if frame.kind == Kind::VideoDelta && self.waiting_for_keyframe(frame.stream) {
            return Ok(Offered {
                accepted: false,
                waiting_for_keyframe: true,
                changes,
            });
        }
        while self.bytes > self.limits.max_bytes - frame.bytes
            || self.entries.len() >= self.limits.max_frames as usize
        {
            if let Some(index) = self
                .entries
                .iter()
                .position(|entry| entry.frame.kind.video())
            {
                self.break_video(self.entries[index].frame.stream, &mut changes);
            } else if frame.kind == Kind::Audio && !self.entries.is_empty() {
                self.remove(0, &mut changes);
            } else {
                if frame.kind.video() {
                    self.break_video(frame.stream, &mut changes);
                }
                return Ok(Offered {
                    accepted: false,
                    waiting_for_keyframe: self.waiting_for_keyframe(frame.stream),
                    changes,
                });
            }
        }
        if frame.kind == Kind::VideoDelta && self.waiting_for_keyframe(frame.stream) {
            return Ok(Offered {
                accepted: false,
                waiting_for_keyframe: true,
                changes,
            });
        }
        if frame.kind == Kind::VideoKey
            && let Some(stream) = self.streams.get_mut(&frame.stream)
        {
            stream.waiting = false;
        }
        self.bytes += frame.bytes;
        self.entries.push_back(Entry {
            frame,
            enqueued_us: now_us,
        });
        Ok(Offered {
            accepted: true,
            waiting_for_keyframe: self.waiting_for_keyframe(frame.stream),
            changes,
        })
    }
    /// Takes fresh audio first, then video, releasing expired dependencies.
    pub fn pop(&mut self, now_us: u64) -> Result<Popped, Error> {
        self.clock(now_us)?;
        let mut changes = Changes::default();
        self.expire(now_us, &mut changes);
        let audio = self
            .entries
            .iter()
            .position(|entry| entry.frame.kind == Kind::Audio);
        let frame = if let Some(entry) = self.entries.remove(audio.unwrap_or(0)) {
            self.bytes -= entry.frame.bytes;
            Some(entry.frame)
        } else {
            None
        };
        Ok(Popped { frame, changes })
    }
    /// Permanently closes admission and releases all resident ciphertext IDs.
    pub fn close(&mut self) -> Changes {
        self.closed = true;
        self.streams.clear();
        self.bytes = 0;
        Changes {
            discarded: self.entries.drain(..).map(|entry| entry.frame.id).collect(),
        }
    }
}

#[cfg(test)]
#[path = "realtime_tests.rs"]
mod tests;
