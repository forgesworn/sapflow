//! One bounded startup-media watermark per transport attempt. No payloads are retained.
//! Peer acknowledgement is network progress, never decoded or lossless delivery.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupProgressEvent {
    Sending,
    PeerMediaProgress,
}

#[derive(Debug)]
pub struct StartupWatermark {
    has_video: bool,
    has_audio: bool,
    video_complete: bool,
    last_end: u64,
    end: Option<u64>,
    acknowledged: bool,
    sending_pending: bool,
    peer_pending: bool,
    cancelled: bool,
}
impl StartupWatermark {
    pub fn new(has_video: bool, has_audio: bool) -> Option<Self> {
        (has_video || has_audio).then_some(Self {
            has_video,
            has_audio,
            video_complete: false,
            last_end: 0,
            end: None,
            acknowledged: false,
            sending_pending: false,
            peer_pending: false,
            cancelled: false,
        })
    }
    pub fn declares(&self, video: bool) -> bool {
        if video {
            self.has_video
        } else {
            self.has_audio
        }
    }
    pub fn tracks(&self) -> (bool, bool) {
        (self.has_video, self.has_audio)
    }
    /// Call only after the entire validated sample enters bounded transport
    /// ownership. `end` includes every wire/payload byte up to that sample end.
    /// Declared audio must follow the complete IDR in submission order; media
    /// timestamps are neither changed nor required to start at the same time.
    pub fn complete_sample(&mut self, video: bool, keyframe: bool, end: u64) {
        if self.cancelled || self.end.is_some() || !self.declares(video) {
            return;
        }
        if end <= self.last_end {
            self.cancel();
            return;
        }
        self.last_end = end;
        if video {
            if !keyframe {
                return;
            }
            self.video_complete = true;
            if self.has_audio {
                return;
            }
        } else if self.has_video && !self.video_complete {
            return;
        }
        self.end = Some(end);
        self.sending_pending = true;
    }
    /// Supply only validated cumulative peer acknowledgement of the SAME byte
    /// coordinate as `end`. The adapter must cancel this watermark on local
    /// abandonment, transport close or replacement, before polling events.
    pub fn acknowledge(&mut self, end: u64) {
        self.acknowledge_with_margin(end, 0);
    }
    /// Conservative proof where known peer counter conventions differ by at
    /// most `margin`. Checked addition refuses overflow instead of weakening it.
    pub fn acknowledge_with_margin(&mut self, end: u64, margin: u64) {
        if !self.cancelled
            && !self.acknowledged
            && self
                .end
                .and_then(|target| target.checked_add(margin))
                .is_some_and(|target| end >= target)
        {
            self.acknowledged = true;
            self.peer_pending = true;
        }
    }
    pub fn submitted(&self) -> bool {
        !self.cancelled && self.end.is_some()
    }
    pub fn peer_acknowledged(&self) -> bool {
        !self.cancelled && self.acknowledged
    }
    pub fn end_position(&self) -> Option<u64> {
        self.end
    }
    pub fn poll_event(&mut self) -> Option<StartupProgressEvent> {
        if std::mem::take(&mut self.sending_pending) {
            Some(StartupProgressEvent::Sending)
        } else if std::mem::take(&mut self.peer_pending) {
            Some(StartupProgressEvent::PeerMediaProgress)
        } else {
            None
        }
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.sending_pending = false;
        self.peer_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_idr_then_declared_audio_define_one_exact_boundary() {
        let mut m = StartupWatermark::new(true, true).expect("valid recovery fixture operation");
        m.complete_sample(false, false, 30); // early AAC cannot stand in for post-IDR audio
        m.complete_sample(true, false, 60);
        m.acknowledge(60);
        assert!(!m.submitted());
        m.complete_sample(true, true, 100);
        assert!(!m.submitted());
        m.complete_sample(false, false, 130);
        assert_eq!(m.end_position(), Some(130));
        m.acknowledge(129);
        assert_eq!(m.poll_event(), Some(StartupProgressEvent::Sending));
        assert_eq!(m.poll_event(), None);
        m.acknowledge(130);
        assert_eq!(
            m.poll_event(),
            Some(StartupProgressEvent::PeerMediaProgress)
        );
        m.complete_sample(true, true, 500);
        m.acknowledge(500);
        assert_eq!(m.end_position(), Some(130));
        assert_eq!(m.poll_event(), None);
    }
    #[test]
    fn conservative_ack_margin_refuses_inclusive_counter_false_positive_and_overflow() {
        let mut m = StartupWatermark::new(false, true).expect("audio");
        m.complete_sample(false, false, 4000);
        m.acknowledge_with_margin(0, 3073);
        m.acknowledge_with_margin(4000, 3073); // inclusive domain: only 927 post-handshake bytes
        m.acknowledge_with_margin(7072, 3073);
        assert!(!m.peer_acknowledged());
        m.acknowledge_with_margin(7073, 3073);
        assert!(m.peer_acknowledged());
        let mut overflow = StartupWatermark::new(false, true).expect("audio");
        overflow.complete_sample(false, false, u64::MAX - 1);
        overflow.acknowledge_with_margin(u64::MAX, 3073);
        assert!(!overflow.peer_acknowledged());
    }
    #[test]
    fn regressing_byte_coordinate_cannot_confirm_pre_idr_bytes() {
        let mut m = StartupWatermark::new(true, true).expect("declared tracks");
        m.complete_sample(true, true, 200);
        m.complete_sample(false, false, 100);
        m.acknowledge(150);
        assert!(!m.submitted());
        assert!(!m.peer_acknowledged());
        assert_eq!(m.poll_event(), None);
    }
    #[test]
    fn declared_single_track_and_cancel_have_no_extra_ownership() {
        assert!(StartupWatermark::new(false, false).is_none());
        for video in [false, true] {
            let mut m =
                StartupWatermark::new(video, !video).expect("valid recovery fixture operation");
            m.complete_sample(!video, true, 50);
            assert!(!m.submitted());
            m.complete_sample(video, true, 100);
            assert!(m.submitted());
            m.acknowledge(100);
            m.cancel();
            m.acknowledge(u64::MAX);
            assert!(!m.peer_acknowledged());
            assert_eq!(m.poll_event(), None);
        }
    }
}
