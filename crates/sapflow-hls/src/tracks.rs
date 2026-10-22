//! Declared media topology, distinct from which codec header arrived first.

/// A supported non-empty topology explicitly declared by the publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSet {
    /// AAC audio without a video track; does not require a video keyframe.
    AudioOnly,
    /// H.264 video without an audio track.
    VideoOnly,
    /// H.264 video and AAC audio.
    AudioVideo,
}

impl TrackSet {
    /// Whether the declaration includes H.264 video.
    pub const fn has_video(self) -> bool {
        matches!(self, Self::VideoOnly | Self::AudioVideo)
    }

    /// Whether the declaration includes AAC audio.
    pub const fn has_audio(self) -> bool {
        matches!(self, Self::AudioOnly | Self::AudioVideo)
    }
}
